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
    stamp: String,
    sections: Vec<Section>,
    rows: Vec<(String, Action, i32)>,
    selected: i32,
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
    let mut stamp = format!("sources:{epoch}:{current}:{kind:?}");
    for group in groups {
        stamp.push_str(&format!(
            "|group:{}:{}:{:?}:{:?}",
            group.name, group.handle, group.state, group.tier
        ));
    }
    for row in &source_rows {
        stamp.push_str(&format!(
            "|row:{}:{}:{}:{}:{}:{}:{}",
            row.src,
            row.section,
            row.title,
            row.count_line,
            row.pinned,
            row.last_pinned,
            row.current
        ));
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
                stamp.push_str(&format!(
                    "|action:library:{}:{}:{}",
                    sid.raw(),
                    candidate.key,
                    target.epoch
                ));
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
                stamp.push_str("|action:recheck");
                rows.push(("recheck".into(), Action::Recheck, table_index as i32));
            }
            SrcAction::None => stamp.push_str("|action:none"),
        }
    }
    MenuDraft {
        stamp,
        sections: source_sections,
        rows,
        selected,
    }
}

fn sort_draft(sorts: &[SortEntry], sort_index: usize, sort_desc: bool) -> MenuDraft {
    let mut section = Section::new("Sort by");
    let mut rows = Vec::new();
    let mut stamp = format!("sort:{}:{}", sort_index, sort_desc);
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
        stamp.push_str(&format!(
            "|{}|{}|{}|{}",
            sort.key, sort.title, sort.default_desc, desc
        ));
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
        stamp,
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
    let genre_stamp = genre
        .map(|g| format!("{}:{}", g.id, g.title))
        .unwrap_or_else(|| "all".into());
    MenuDraft {
        stamp: format!("filter:{}:{}", unwatched, genre_stamp),
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
    let mut stamp = format!("genre:{}", current.map(|g| g.id.as_str()).unwrap_or("all"));
    let mut selected = 0;
    for (i, genre) in genres.iter().enumerate() {
        let active = current.is_some_and(|selected| selected.id == genre.id);
        if active {
            selected = (i + 1) as i32;
        }
        section = section.row(Row::new(&genre.title).checked(active));
        stamp.push_str(&format!("|{}|{}|{}", genre.id, genre.title, active));
        rows.push((
            format!("genre:{}", genre.id),
            Action::Edit(QueryEdit::Genre(Some(genre.id.clone()))),
            (i + 1) as i32,
        ));
    }
    MenuDraft {
        stamp,
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
    stamp: String,
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
            stamp: String::new(),
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
        let stamp = format!(
            "{:?}:{}:{}",
            self.kind,
            listing.sort_index(),
            listing.unwatched()
        );
        sections.push(section);
        MenuDraft {
            stamp,
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
    fn metadata_refresh_preserves_focus_by_stable_element_key() {
        let old = vec![
            MenuRow {
                key: 10,
                action: Action::Recheck,
                table_index: 0,
            },
            MenuRow {
                key: 11,
                action: Action::Recheck,
                table_index: 1,
            },
        ];
        let refreshed = vec![
            MenuRow {
                key: 10,
                action: Action::Recheck,
                table_index: 0,
            },
            MenuRow {
                key: 11,
                action: Action::Recheck,
                table_index: 1,
            },
        ];
        assert_eq!(preserved_selection(Some(11), &refreshed, 0), 1);
        assert_eq!(preserved_selection(None, &old, 1), 1);
    }
}
