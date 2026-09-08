//! A registered Library menu surface. Navigation owns its lifetime, phase, and input scope.
use crate::browse::{GenreEntry, SortEntry, SrcGroup, SrcRow};
use crate::screens::registry::{AppFx, AppMsg, LibraryLike, LibraryMenuArg, LibraryMenuKind};
use crate::stores::browse::{BrowseCmd, LibraryWork, QueryEdit, SectionAddress};
use crate::stores::{StoreCmd, StoreId};
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, Key,
    LogicalState, Machine, MachineId, NavOp,
};
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec,
    Hover, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step, Stop,
};
use crate::ui::source_list::{self, Level, SrcAction, Tail};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::widgets::{Glass, GlassState};
use crate::ui::Rect;
use std::borrow::Cow;

// The compact Library menu's existing corner geometry.
const PANEL_RADIUS: f32 = 20.0;

pub(crate) const SHAPE: [&str; 2] = [
    "LibraryMenu{arg:LibraryMenuArg,kind:u32,identities:[str],dependencies:[u8],rows:[{key:u32,table_index:i32}],table:TableViewMotion}",
    TableView::MOTION_SHAPE,
];

#[derive(Clone)]
enum Action {
    Edit(QueryEdit),
    Genre,
    Select(SectionAddress),
    Recheck,
}

struct MenuRow {
    key: u32,
    action: Action,
    table_index: i32,
}

struct MenuDraft {
    stamp: Vec<u8>,
    sections: Vec<Section>,
    rows: Vec<(String, Action, i32)>,
    selected: i32,
}

#[derive(Default)]
struct Stamp(Vec<u8>);

impl Stamp {
    fn tag(&mut self, value: u8) {
        self.0.push(value);
    }
    fn bool(&mut self, value: bool) {
        self.0.push(u8::from(value));
    }
    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn i64(&mut self, value: i64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn str(&mut self, value: &str) {
        self.u32(value.len() as u32);
        self.0.extend_from_slice(value.as_bytes());
    }
    fn finish(self) -> Vec<u8> {
        self.0
    }
}

fn stamp_kind(stamp: &mut Stamp, kind: Option<crate::browse::SecKind>) {
    stamp.tag(match kind {
        None => 0,
        Some(crate::browse::SecKind::Movie) => 1,
        Some(crate::browse::SecKind::Show) => 2,
    });
}

fn stamp_state(stamp: &mut Stamp, state: crate::browse::SourceState) {
    stamp.tag(match state {
        crate::browse::SourceState::NotProbed => 0,
        crate::browse::SourceState::Reachable => 1,
        crate::browse::SourceState::Unauthorized => 2,
        crate::browse::SourceState::Unreachable => 3,
    });
}

fn stamp_tier(stamp: &mut Stamp, tier: Option<crate::plex::probe::Location>) {
    stamp.tag(match tier {
        None => 0,
        Some(crate::plex::probe::Location::Local) => 1,
        Some(crate::plex::probe::Location::Remote) => 2,
        Some(crate::plex::probe::Location::Relay) => 3,
    });
}

fn preserved_selection(old_key: Option<u32>, rows: &[MenuRow], fallback: i32) -> i32 {
    old_key
        .and_then(|key| {
            rows.iter()
                .find(|row| row.key == key)
                .map(|row| row.table_index)
        })
        .unwrap_or(fallback)
}

fn source_draft(
    epoch: u32,
    current: usize,
    groups: &[SrcGroup],
    sections: &[crate::browse::view::SectionView],
) -> MenuDraft {
    let kind = sections.get(current).map(|section| section.kind);
    let source_rows: Vec<SrcRow> = sections
        .iter()
        .filter(|section| Some(section.kind) == kind && section.row.pinned)
        .map(|section| section.row.clone())
        .collect();
    let (source_sections, actions) =
        source_list::sections(Level::Browse, groups, &source_rows, Tail::Recheck);
    let mut stamp = Stamp::default();
    stamp.tag(1);
    stamp.u32(epoch);
    stamp.u32(current as u32);
    stamp_kind(&mut stamp, kind);
    for group in groups {
        stamp.tag(2);
        stamp.str(&group.name);
        stamp.str(&group.handle);
        stamp_state(&mut stamp, group.state);
        stamp_tier(&mut stamp, group.tier);
    }
    for row in &source_rows {
        stamp.tag(3);
        stamp.u32(row.src as u32);
        stamp.u32(row.section as u32);
        stamp.str(&row.title);
        stamp.str(&row.count_line);
        stamp.bool(row.pinned);
        stamp.bool(row.last_pinned);
        stamp.bool(row.current);
    }
    let mut rows = Vec::new();
    let mut selected = 0i32;
    for (table_index, action) in actions.iter().enumerate() {
        match action {
            SrcAction::Library(index) => {
                let Some(candidate) = sections.get(*index) else {
                    continue;
                };
                let Some(sid) = candidate.sid else { continue };
                let target = SectionAddress {
                    epoch,
                    sid,
                    section: candidate.key,
                };
                stamp.tag(4);
                stamp.u32(u32::from(sid.raw()));
                stamp.i64(candidate.key);
                stamp.u32(target.epoch);
                if candidate.row.current {
                    selected = table_index as i32;
                }
                rows.push((
                    format!("section:{}:{}", sid.raw(), candidate.key),
                    Action::Select(target),
                    table_index as i32,
                ));
            }
            SrcAction::Recheck => {
                stamp.tag(5);
                rows.push(("recheck".into(), Action::Recheck, table_index as i32));
            }
            SrcAction::None => stamp.tag(6),
        }
    }
    MenuDraft {
        stamp: stamp.finish(),
        sections: source_sections,
        rows,
        selected,
    }
}

fn sort_draft(sorts: &[SortEntry], sort_index: usize, sort_desc: bool) -> MenuDraft {
    let mut section = Section::new("Sort by");
    let mut rows = Vec::new();
    let mut stamp = Stamp::default();
    stamp.tag(7);
    stamp.u32(sort_index as u32);
    stamp.bool(sort_desc);
    let mut selected = 0;
    for (i, sort) in sorts.iter().enumerate() {
        let active = i == sort_index;
        if active {
            selected = i as i32;
        }
        let desc = if active {
            !sort_desc
        } else {
            sort.default_desc
        };
        let mut row = Row::new(&sort.title).checked(active);
        if active {
            row = row.ticon(if sort_desc {
                crate::ui::icons::Icon::ChevronDown
            } else {
                crate::ui::icons::Icon::ChevronUp
            });
        }
        section = section.row(row);
        stamp.tag(8);
        stamp.str(&sort.key);
        stamp.str(&sort.title);
        stamp.bool(sort.default_desc);
        stamp.bool(desc);
        rows.push((
            format!("sort:{}", sort.key),
            Action::Edit(QueryEdit::Sort {
                key: sort.key.clone(),
                desc,
            }),
            i as i32,
        ));
    }
    MenuDraft {
        stamp: stamp.finish(),
        sections: vec![section],
        rows,
        selected,
    }
}

fn filter_draft(unwatched: bool, genre: Option<&GenreEntry>) -> MenuDraft {
    let section = Section::new("Filter")
        .row(Row::new("Unwatched only").toggle(unwatched))
        .row(
            Row::new("Genre")
                .value(genre.map(|g| g.title.as_str()).unwrap_or("All"))
                .chevron(true),
        );
    let mut stamp = Stamp::default();
    stamp.tag(9);
    stamp.bool(unwatched);
    stamp.tag(u8::from(genre.is_some()));
    if let Some(genre) = genre {
        stamp.str(&genre.id);
        stamp.str(&genre.title);
    }
    MenuDraft {
        stamp: stamp.finish(),
        sections: vec![section],
        rows: vec![
            (
                "unwatched".into(),
                Action::Edit(QueryEdit::Unwatched(!unwatched)),
                0,
            ),
            ("genre".into(), Action::Genre, 1),
        ],
        selected: 0,
    }
}

fn genre_draft(genres: &[GenreEntry], current: Option<&GenreEntry>) -> MenuDraft {
    let mut section = Section::new("Genre").row(Row::new("All Genres").checked(current.is_none()));
    let mut rows = vec![("genre:all".into(), Action::Edit(QueryEdit::Genre(None)), 0)];
    let mut stamp = Stamp::default();
    stamp.tag(10);
    stamp.tag(u8::from(current.is_some()));
    if let Some(current) = current {
        stamp.str(&current.id);
    }
    let mut selected = 0;
    for (i, genre) in genres.iter().enumerate() {
        let active = current.is_some_and(|selected| selected.id == genre.id);
        if active {
            selected = (i + 1) as i32;
        }
        section = section.row(Row::new(&genre.title).checked(active));
        stamp.tag(11);
        stamp.str(&genre.id);
        stamp.str(&genre.title);
        stamp.bool(active);
        rows.push((
            format!("genre:{}", genre.id),
            Action::Edit(QueryEdit::Genre(Some(genre.id.clone()))),
            (i + 1) as i32,
        ));
    }
    MenuDraft {
        stamp: stamp.finish(),
        sections: vec![section],
        rows,
        selected,
    }
}

pub(crate) struct LibraryMenu {
    entry: EntryId,
    arg: LibraryMenuArg,
    kind: LibraryMenuKind,
    rows: Vec<MenuRow>,
    identities: Vec<String>,
    table: TableView,
    stamp: Vec<u8>,
    glass: GlassState,
}

impl LibraryMenu {
    pub(crate) fn new(entry: EntryId, arg: LibraryMenuArg) -> Self {
        Self {
            entry,
            kind: arg.kind,
            arg,
            rows: Vec::new(),
            identities: Vec::new(),
            table: TableView::new(),
            stamp: Vec::new(),
            glass: GlassState::new(),
        }
    }
    fn frame(&self) -> Rect {
        let [x, y, _, h] = self.arg.anchor.map(f32::from_bits);
        let height = self.table.measured_height().clamp(120.0, 740.0);
        Rect::new(
            x.clamp(96.0, 1174.0),
            (y + h + 16.0).clamp(96.0, 984.0 - height),
            650.0,
            height,
        )
    }
    fn key_for(&mut self, identity: String) -> u32 {
        if let Some(i) = self.identities.iter().position(|old| old == &identity) {
            i as u32
        } else {
            self.identities.push(identity);
            (self.identities.len() - 1) as u32
        }
    }

    fn refresh<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) {
        let listing = H::listing(cx);
        let draft = self.draft(listing, H::directory(cx));
        self.apply_draft(draft);
    }

    fn apply_draft(&mut self, draft: MenuDraft) {
        if self.stamp == draft.stamp {
            return;
        }

        let old_key = self
            .rows
            .iter()
            .find(|row| row.table_index == self.table.sel)
            .map(|row| row.key);
        let mut rows = Vec::with_capacity(draft.rows.len());
        for (identity, action, table_index) in draft.rows {
            rows.push(MenuRow {
                key: self.key_for(identity),
                action,
                table_index,
            });
        }
        let selected = preserved_selection(old_key, &rows, draft.selected);
        self.stamp = draft.stamp;
        self.rows = rows;
        self.table.compact = true;
        self.table.set_sections(draft.sections, selected, false);
    }

    fn draft(
        &self,
        listing: crate::stores::browse::ListingView<'_>,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) -> MenuDraft {
        let mut rows = Vec::new();
        let mut sections = Vec::new();
        let selected = 0i32;
        let title = match self.kind {
            LibraryMenuKind::Sort => "Sort by",
            LibraryMenuKind::Filter => "Filter",
            LibraryMenuKind::Genre => "Genre",
            LibraryMenuKind::Sources => "Libraries",
        };
        let mut section = Section::new(title);
        match self.kind {
            LibraryMenuKind::Sort => {
                return sort_draft(listing.sorts(), listing.sort_index(), listing.sort_desc());
            }
            LibraryMenuKind::Filter => {
                return filter_draft(listing.unwatched(), listing.genre());
            }
            LibraryMenuKind::Genre => {
                return genre_draft(listing.genres(), listing.genre());
            }
            LibraryMenuKind::Sources => {
                if let Some(current) = directory.current() {
                    let groups: Vec<SrcGroup> = directory
                        .sources()
                        .iter()
                        .map(|(_, group)| group.clone())
                        .collect();
                    return source_draft(
                        directory.epoch().unwrap_or(0),
                        current,
                        &groups,
                        directory.sections(),
                    );
                }
                section = section.row(Row::new("Check for new shares"));
                rows.push(("recheck".into(), Action::Recheck, 0));
            }
        }
        let mut stamp = Stamp::default();
        stamp.tag(12);
        stamp.u32(self.kind as u32);
        stamp.u32(listing.sort_index() as u32);
        stamp.bool(listing.unwatched());
        sections.push(section);
        MenuDraft {
            stamp: stamp.finish(),
            sections,
            rows,
            selected,
        }
    }
    fn activate<H: LibraryLike>(&mut self, elem: u32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let Some(action) = self
            .rows
            .iter()
            .find(|row| row.key == elem)
            .map(|row| row.action.clone())
        else {
            return;
        };
        match action {
            Action::Genre => {
                self.kind = LibraryMenuKind::Genre;
                self.stamp.clear();
                self.refresh(cx);
            }
            Action::Edit(edit) => {
                let close = !matches!(edit, QueryEdit::Unwatched(_));
                fx.push(Fx::Deliver(
                    MachineId::Instance(self.arg.host),
                    Delivery::Screen(ScreenEvent::App(AppMsg::LibraryEdit {
                        target: self.arg.target,
                        edit,
                    })),
                ));
                if close {
                    fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
                }
            }
            Action::Select(target) => {
                fx.push(Fx::Deliver(
                    MachineId::Instance(self.arg.host),
                    Delivery::Screen(ScreenEvent::App(AppMsg::LibrarySelect(target))),
                ));
                fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
            }
            Action::Recheck => fx.push(Fx::App(AppFx::Store(
                StoreId::Browse,
                StoreCmd::Browse(BrowseCmd::RecheckShares),
            ))),
        }
    }
}
impl<H: LibraryLike> Machine<H> for LibraryMenu {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount | ScreenEvent::StoreChanged(..) | ScreenEvent::Enter(_) => {
                self.refresh(cx)
            }
            ScreenEvent::Tick(tick) => {
                self.refresh(cx);
                if self.kind == LibraryMenuKind::Genre {
                    fx.push(Fx::App(AppFx::Store(
                        StoreId::Browse,
                        StoreCmd::Browse(BrowseCmd::Addressed {
                            target: self.arg.target,
                            work: LibraryWork::Genres,
                        }),
                    )));
                }
                self.table.sel = cx
                    .focus
                    .current
                    .and_then(|focus| {
                        self.rows
                            .iter()
                            .find(|row| row.key == focus.elem)
                            .map(|row| row.table_index)
                    })
                    .unwrap_or(-1);
                self.table.update(tick.dt(), self.frame().h);
            }
            ScreenEvent::FocusMoved { to, .. } => {
                self.table.sel = self
                    .rows
                    .iter()
                    .find(|row| row.key == to.elem)
                    .map(|row| row.table_index)
                    .unwrap_or(-1);
            }
            ScreenEvent::Activate(elem) => self.activate(*elem, cx, fx),
            ScreenEvent::PressCommit(_) => {
                if let Some(key) = cx.focus.current {
                    self.activate(key.elem, cx, fx);
                }
            }
            ScreenEvent::Input(input) => {
                if let InputKind::Key {
                    key: Key::Back,
                    edge: Edge::Down,
                    ..
                } = input.kind
                {
                    if self.kind == LibraryMenuKind::Genre {
                        self.kind = LibraryMenuKind::Filter;
                        self.stamp.clear();
                        self.refresh(cx);
                    } else {
                        fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
                    }
                    return Handled::Yes;
                }
            }
            _ => {}
        }
        Handled::No
    }
}
impl<H: LibraryLike> Focusable<H> for LibraryMenu {
    fn groups(&self, _: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: GroupId(0),
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: self.frame(),
            len: self.rows.len(),
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, elem: &u32, _: &Cx<'_, H>) -> Option<GroupId> {
        self.rows
            .iter()
            .any(|row| row.key == *elem)
            .then_some(GroupId(0))
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _: &Cx<'_, H>) -> Step<u32> {
        let Some(index) = self.rows.iter().position(|row| row.key == key.elem) else {
            return Step::Edge;
        };
        let next = match dir {
            Dir::Up => index.checked_sub(1),
            Dir::Down => Some(index + 1),
            _ => None,
        };
        next.and_then(|i| self.rows.get(i))
            .map_or(Step::Edge, |row| {
                Step::Move(FocusKey {
                    entry: self.entry,
                    elem: row.key,
                })
            })
    }
    fn place(&self, elem: &u32, _: &Cx<'_, H>, _: At) -> Option<Placed> {
        let row = self.rows.iter().find(|row| row.key == *elem)?;
        let rect = self.table.row_frame(self.frame(), row.table_index)?;
        Some(Placed {
            rect,
            rest_rect: rect,
            clip: self.frame(),
            index: Some(row.table_index as u32),
        })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.group_of(&want.elem, cx).is_some() {
            want
        } else {
            FocusKey {
                entry: self.entry,
                elem: self.rows.first().map_or(0, |row| row.key),
            }
        }
    }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, H>) -> FocusKey<u32> {
        FocusKey {
            entry: self.entry,
            elem: self
                .rows
                .iter()
                .find(|row| row.table_index == self.table.sel)
                .map_or(0, |row| row.key),
        }
    }
}
impl<H: LibraryLike> Screen<H> for LibraryMenu {
    fn name(&self) -> &'static str {
        "library_menu"
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, H>) {
        Glass::CACHED.prepare(&mut self.glass, false);
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, H>) {
        let p = f.painter.alpha(f.page_alpha);
        Glass::CACHED.panel(p, self.frame(), 0.0, PANEL_RADIUS);
        self.table.draw(p, self.frame());
        for row in &self.rows {
            if let Some(placed) = <Self as Focusable<H>>::place(self, &row.key, f.cx, At::Drawn) {
                f.stop(
                    p,
                    Stop {
                        key: FocusKey {
                            entry: self.entry,
                            elem: row.key,
                        },
                        rect: placed.rect,
                        rest_rect: placed.rest_rect,
                        clip: placed.clip,
                        hover: Hover::Focus,
                        activate: Activate::Immediate,
                    },
                );
            }
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}
impl LogicalState for LibraryMenu {
    fn write(&self, c: &mut Canon) {
        self.arg.write(c);
        c.u32(self.kind as u32);
        c.seq(self.identities.len());
        for identity in &self.identities {
            c.str(identity);
        }
        // The length-safe dependency encoding includes every row's data and semantic action.
        // The identity registry alone cannot distinguish a reorder or changed action payload.
        c.seq(self.stamp.len());
        for byte in &self.stamp { c.u8(*byte); }
        c.seq(self.rows.len());
        for row in &self.rows { c.u32(row.key).u32(row.table_index as u32); }
        self.table.write_motion(c);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("library_menu");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::{SecKind, SourceState};
    use crate::plex::ServerId;
    use crate::ui::fixture::FixtureMeasure;
    use crate::ui::machine::{FocusRead, Host, InputOwner, PressRead, Tick};
    use crate::ui::screen::ScreenArg;

    #[derive(Clone)]
    struct Arg;
    impl LogicalState for Arg {
        fn write(&self, _: &mut Canon) {}
        fn probe(&self, _: &mut String) {}
    }
    impl ScreenArg for Arg {
        fn chrome(&self) -> crate::ui::machine::Chrome {
            crate::ui::machine::Chrome::None
        }
        fn id(&self) -> crate::ui::machine::ScreenId {
            crate::ui::machine::ScreenId(1)
        }
        fn title(&self) -> Option<&str> {
            None
        }
        fn same_instance(&self, _: &Self) -> bool {
            true
        }
    }
    struct HostFixture;
    #[derive(Clone, Copy)]
    struct Views<'a> {
        listing: crate::stores::browse::ListingView<'a>,
        directory: crate::stores::browse::DirectoryView<'a>,
        hubs: crate::stores::browse::HubsView<'a>,
    }
    impl Host for HostFixture {
        type Arg = Arg;
        type Fx = AppFx;
        type Msg = AppMsg;
        type Elem = u32;
        type Views<'a> = Views<'a>;
        type Init = Arg;
        type Memory = crate::screens::registry::PageMemory;
    }
    impl LibraryLike for HostFixture {
        fn listing<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::ListingView<'a> {
            cx.views.listing
        }
        fn directory<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::DirectoryView<'a> {
            cx.views.directory
        }
        fn section_hubs<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::HubsView<'a> {
            cx.views.hubs
        }
    }

    fn with_cx<R>(test: impl FnOnce(&Cx<'_, HostFixture>) -> R) -> R {
        let listing = crate::browse::view::ListingSnapshot::fixture(
            ServerId::from_raw(1),
            Vec::new(),
            Vec::new(),
        );
        let directory = crate::stores::browse::DirectorySnapshot::default();
        let hubs = crate::stores::browse::hubs_snapshot();
        let measure = FixtureMeasure;
        test(&Cx {
            views: Views {
                listing: listing.view(),
                directory: directory.view(),
                hubs: hubs.view(),
            },
            tick: Tick::default(),
            measure: &measure,
            focus: FocusRead { current: None },
            press: PressRead::default(),
            owner: InputOwner::Entry(EntryId(7)),
        })
    }

    #[test]
    fn sort_refresh_updates_icon_and_the_next_action_direction() {
        let sorts = vec![SortEntry {
            key: "titleSort".into(),
            title: "Title".into(),
            default_desc: false,
        }];
        let up = sort_draft(&sorts, 0, false);
        assert_eq!(
            up.sections[0].rows[0].ticon,
            Some(crate::ui::icons::Icon::ChevronUp)
        );
        assert!(matches!(
            up.rows[0].1,
            Action::Edit(QueryEdit::Sort { desc: true, .. })
        ));

        let down = sort_draft(&sorts, 0, true);
        assert_eq!(
            down.sections[0].rows[0].ticon,
            Some(crate::ui::icons::Icon::ChevronDown)
        );
        assert!(matches!(
            down.rows[0].1,
            Action::Edit(QueryEdit::Sort { desc: false, .. })
        ));
        assert_ne!(up.stamp, down.stamp);
    }

    fn state_hash(menu: &LibraryMenu) -> u64 {
        let mut c = Canon::new();
        menu.write(&mut c);
        c.finish()
    }

    #[test]
    fn canonical_menu_state_distinguishes_actions_and_row_order_with_the_same_identity_registry() {
        let mut menu = LibraryMenu::new(EntryId(7), LibraryMenuArg {
            host: crate::ui::machine::InstanceId(8),
            target: SectionAddress { epoch: 11, sid: ServerId::from_raw(1), section: 7 },
            kind: LibraryMenuKind::Sort, anchor: [0; 4],
        });
        let mut sorts = vec![
            SortEntry { key: "titleSort".into(), title: "Title".into(), default_desc: false },
            SortEntry { key: "addedAt".into(), title: "Added".into(), default_desc: true },
        ];
        menu.apply_draft(sort_draft(&sorts, 0, false));
        let identities = menu.identities.clone();
        let ascending = state_hash(&menu);
        menu.apply_draft(sort_draft(&sorts, 0, true));
        assert_eq!(menu.identities, identities);
        assert_ne!(state_hash(&menu), ascending, "the next sort action now requests the opposite direction");
        menu.apply_draft(sort_draft(&sorts, 0, false));
        assert_eq!(state_hash(&menu), ascending);
        sorts.reverse();
        menu.apply_draft(sort_draft(&sorts, 1, false));
        assert_eq!(menu.identities, identities);
        assert_ne!(state_hash(&menu), ascending, "directional navigation now sees a different row order");
    }

    #[test]
    fn genre_refresh_updates_labels_marks_and_selected_row() {
        let genres = vec![
            GenreEntry {
                id: "7".into(),
                title: "Drama".into(),
            },
            GenreEntry {
                id: "9".into(),
                title: "Comedy".into(),
            },
        ];
        let draft = genre_draft(&genres, Some(&genres[1]));
        assert_eq!(draft.sections[0].rows[0].label, "All Genres");
        assert!(!draft.sections[0].rows[0].checked);
        assert_eq!(draft.sections[0].rows[2].label, "Comedy");
        assert!(draft.sections[0].rows[2].checked);
        assert_eq!(draft.selected, 2);

        let changed = genre_draft(&genres, Some(&genres[0]));
        assert_ne!(draft.stamp, changed.stamp);
        assert!(changed.sections[0].rows[1].checked);
        assert!(!changed.sections[0].rows[2].checked);
    }

    #[test]
    fn filter_refresh_updates_genre_value_and_unwatched_action() {
        let drama = GenreEntry {
            id: "7".into(),
            title: "Drama".into(),
        };
        let all = filter_draft(false, None);
        assert_eq!(all.sections[0].rows[1].value.as_deref(), Some("All"));
        assert!(matches!(
            all.rows[0].1,
            Action::Edit(QueryEdit::Unwatched(true))
        ));

        let filtered = filter_draft(true, Some(&drama));
        assert_eq!(filtered.sections[0].rows[1].value.as_deref(), Some("Drama"));
        assert!(filtered.sections[0].rows[0].toggle == Some(true));
        assert!(matches!(
            filtered.rows[0].1,
            Action::Edit(QueryEdit::Unwatched(false))
        ));
        assert_ne!(all.stamp, filtered.stamp);
    }

    fn source_sections() -> (Vec<SrcGroup>, Vec<crate::browse::view::SectionView>) {
        let groups = vec![
            SrcGroup {
                name: "Own NAS".into(),
                handle: String::new(),
                state: SourceState::Reachable,
                tier: None,
            },
            SrcGroup {
                name: "Friend NAS".into(),
                handle: "friend".into(),
                state: SourceState::Reachable,
                tier: None,
            },
        ];
        let sections = vec![
            crate::browse::view::SectionView {
                sid: Some(ServerId::from_raw(1)),
                key: 7,
                kind: SecKind::Movie,
                borrowed: false,
                row: SrcRow {
                    src: 0,
                    section: 0,
                    title: "Movies".into(),
                    count_line: "26 films".into(),
                    pinned: true,
                    last_pinned: false,
                    current: true,
                },
            },
            crate::browse::view::SectionView {
                sid: Some(ServerId::from_raw(2)),
                key: 7,
                kind: SecKind::Movie,
                borrowed: true,
                row: SrcRow {
                    src: 1,
                    section: 1,
                    title: "Shared Movies".into(),
                    count_line: "4 films".into(),
                    pinned: true,
                    last_pinned: false,
                    current: false,
                },
            },
        ];
        (groups, sections)
    }

    #[test]
    fn sources_keep_server_identity_and_align_recheck_after_separator() {
        // with_cx retains a snapshot of the process-global browse store.
        let _guard = crate::testlock::serial();
        let (groups, sections) = source_sections();
        let draft = source_draft(11, 0, &groups, &sections);
        assert_eq!(draft.sections.len(), 2);
        assert_eq!(draft.sections[1].header, "Friend NAS");
        assert_eq!(draft.sections[1].accessory, "friend");
        assert!(draft.sections[1].rows[1].sep);
        assert_eq!(
            draft.rows[2].2, 3,
            "recheck follows the separator in TableView coordinates"
        );
        match &draft.rows[0].1 {
            Action::Select(target) => assert_eq!(target.sid, ServerId::from_raw(1)),
            _ => panic!("first source row is not selectable"),
        }
        match &draft.rows[1].1 {
            Action::Select(target) => assert_eq!(target.sid, ServerId::from_raw(2)),
            _ => panic!("second source row is not selectable"),
        }
        assert!(matches!(draft.rows[2].1, Action::Recheck));

        let mut menu = LibraryMenu::new(
            EntryId(7),
            LibraryMenuArg {
                host: crate::ui::machine::InstanceId(8),
                target: SectionAddress {
                    epoch: 11,
                    sid: ServerId::from_raw(1),
                    section: 7,
                },
                kind: LibraryMenuKind::Sources,
                anchor: [0; 4],
            },
        );
        menu.apply_draft(draft);
        let recheck = menu.rows[2].key;
        let placed = with_cx(|cx|
            <LibraryMenu as Focusable<HostFixture>>::place(&menu, &recheck, cx, At::Drawn)
                .expect("recheck remains placed after the separator"));
        assert_eq!(placed.index, Some(3));
        let mut output = Vec::new();
        let mut present = crate::ui::present::Present::new();
        let mut fx = Effects::new(
            &mut output,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(8)),
            &mut present,
        );
        with_cx(|cx| menu.activate(recheck, cx, &mut fx));
        assert!(output.iter().any(|effect| matches!(
            &effect.fx,
            Fx::App(AppFx::Store(
                StoreId::Browse,
                StoreCmd::Browse(BrowseCmd::RecheckShares)
            ))
        )));
    }

    #[test]
    fn sources_rebuild_on_metadata_without_changing_stable_row_identities() {
        let (groups, sections) = source_sections();
        let before = source_draft(11, 0, &groups, &sections);
        let mut changed_groups = groups.clone();
        changed_groups[1].state = SourceState::Unreachable;
        let mut changed_sections = sections.clone();
        changed_sections[1].row.count_line = "5 films".into();
        changed_sections[1].row.current = true;
        let after = source_draft(11, 1, &changed_groups, &changed_sections);

        assert_ne!(before.stamp, after.stamp);
        assert!(after.sections[1].dim);
        assert_eq!(
            before.rows.iter().map(|r| &r.0).collect::<Vec<_>>(),
            after.rows.iter().map(|r| &r.0).collect::<Vec<_>>()
        );
        assert_eq!(after.selected, 1);
        assert_eq!(after.sections[1].rows[0].detail, "5 films");
    }

    #[test]
    fn source_stamp_is_length_safe_for_colons_and_pipes_in_display_text() {
        let (mut groups, sections) = source_sections();
        groups[0].name = "A:B".into();
        groups[0].handle = "C".into();
        let first = source_draft(11, 0, &groups, &sections);
        groups[0].name = "A".into();
        groups[0].handle = "B:C".into();
        let second = source_draft(11, 0, &groups, &sections);
        assert_ne!(first.stamp, second.stamp);

        groups[0].name = "A|B".into();
        let third = source_draft(11, 0, &groups, &sections);
        groups[0].name = "A".into();
        groups[0].handle = "B:C|A|B".into();
        let fourth = source_draft(11, 0, &groups, &sections);
        assert_ne!(third.stamp, fourth.stamp);
    }

    #[test]
    fn metadata_refresh_preserves_focus_by_stable_element_key() {
        let (groups, sections) = source_sections();
        let mut menu = LibraryMenu::new(
            EntryId(7),
            LibraryMenuArg {
                host: crate::ui::machine::InstanceId(8),
                target: SectionAddress {
                    epoch: 11,
                    sid: ServerId::from_raw(1),
                    section: 7,
                },
                kind: LibraryMenuKind::Sources,
                anchor: [0; 4],
            },
        );
        menu.apply_draft(source_draft(11, 0, &groups, &sections));
        let focused_key = menu.rows[1].key;
        menu.table.sel = menu.rows[1].table_index;
        let quiet = source_draft(11, 0, &groups, &sections);
        menu.apply_draft(quiet);
        assert_eq!(
            menu.table.sel, 1,
            "identical refresh does not reset TableView selection"
        );

        let mut shifted = sections.clone();
        shifted.insert(
            1,
            crate::browse::view::SectionView {
                sid: Some(ServerId::from_raw(1)),
                key: 8,
                kind: SecKind::Movie,
                borrowed: false,
                row: SrcRow {
                    src: 0,
                    section: 1,
                    title: "More Movies".into(),
                    count_line: "1 film".into(),
                    pinned: true,
                    last_pinned: false,
                    current: false,
                },
            },
        );
        shifted[2].row.section = 2;
        menu.apply_draft(source_draft(11, 0, &groups, &shifted));
        assert_eq!(
            menu.rows
                .iter()
                .find(|row| row.key == focused_key)
                .map(|row| row.table_index),
            Some(2)
        );
        assert_eq!(
            menu.table.sel, 2,
            "metadata/shape refresh preserves the focused source key"
        );
    }
}
