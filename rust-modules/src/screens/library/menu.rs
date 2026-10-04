//! A registered Library menu surface. Navigation owns its lifetime, phase, and input scope.
use plx_data::browse::{LibraryType, GenreEntry, SecKind, SortEntry, SrcGroup, SrcRow};
use crate::screens::registry::{AppFx, AppMsg, LibraryLike, LibraryMenuArg, LibraryMenuKind};
use plx_data::stores::browse::{BrowseCmd, LibraryWork, QueryEdit, SectionAddress};
use plx_data::stores::{StoreCmd, StoreId};
use plx_ui::frame::Budget;
use plx_machine::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, Key,
    LogicalState, Machine, MachineId, NavOp,
};
use plx_ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec,
    Hover, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step, Stop,
};
use plx_ui::form::{Binding, Form, FormSection, FormTable, RowKey, RowKind};
use crate::appkit::source_list::{self, Level, SrcTarget, Tail};
use plx_ui::table::{Row, TableView};
use std::convert::Infallible;
#[cfg(test)]
use plx_ui::table::MENU_MAX_W;
use plx_ui::Rect;
use std::borrow::Cow;

// The compact Library menu's existing corner geometry.
const PANEL_RADIUS: f32 = 20.0;

pub(crate) const SHAPE: [&str; 2] = [
    "LibraryMenu{arg:LibraryMenuArg,kind:u32,desired_unwatched:Option<bool>,identities:[str],dependencies:[u8],rows:[{key:u32,table_index:i32}],table:TableViewMotion}",
    TableView::MOTION_SHAPE,
];

#[derive(Clone)]
enum Action {
    Edit(QueryEdit),
    Genre,
    Select(SectionAddress),
    Recheck,
}

/// A menu's rows: identity string per focusable row (the stable name focus follows across a
/// reorder), the semantic action, and no other parallel list.
type MenuForm = Form<String, Action, Infallible>;
type MenuSection = FormSection<String, Action, Infallible>;
type MenuTable = FormTable<String, Action, Infallible>;

/// What a menu wants to show now: the change-detection stamp, the declared rows (keys are assigned
/// when the draft is applied, from the menu's identity registry), and the identity to land on when
/// the focused row did not survive.
struct MenuDraft {
    stamp: Vec<u8>,
    form: MenuForm,
    selected: Option<String>,
}

/// A selectable row. The key is a placeholder until [`number`] gives it the identity's slot.
fn choice(section: MenuSection, identity: String, action: Action, row: Row) -> MenuSection {
    section.item_keyed(identity, RowKey(0), RowKind::Choice, action, row)
}

/// Give every focusable row the key its identity owns in `identities` (an identity keeps its key
/// for the menu's life, so focus follows a row across a reorder), interning new ones.
fn number(form: MenuForm, identities: &mut Vec<String>) -> MenuForm {
    form.map(|b| {
        let key = match identities.iter().position(|old| old == &b.id) {
            Some(i) => i as u32,
            None => {
                identities.push(b.id.clone());
                (identities.len() - 1) as u32
            }
        };
        Some(Binding { key: RowKey(key), ..b })
    })
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

fn stamp_kind(stamp: &mut Stamp, kind: Option<plx_data::browse::SecKind>) {
    stamp.tag(match kind {
        None => 0,
        Some(plx_data::browse::SecKind::Movie) => 1,
        Some(plx_data::browse::SecKind::Show) => 2,
    });
}

fn stamp_state(stamp: &mut Stamp, state: plx_data::browse::SourceState) {
    stamp.tag(match state {
        plx_data::browse::SourceState::NotProbed => 0,
        plx_data::browse::SourceState::Reachable => 1,
        plx_data::browse::SourceState::Unauthorized => 2,
        plx_data::browse::SourceState::Unreachable => 3,
        plx_data::browse::SourceState::InsecureOnly => 4,
    });
}

fn stamp_tier(stamp: &mut Stamp, tier: Option<plx_plex::plex::probe::Location>) {
    stamp.tag(match tier {
        None => 0,
        Some(plx_plex::plex::probe::Location::Local) => 1,
        Some(plx_plex::plex::probe::Location::Remote) => 2,
        Some(plx_plex::plex::probe::Location::Relay) => 3,
    });
}

fn source_draft(
    epoch: u32,
    current: usize,
    groups: &[SrcGroup],
    sections: &[plx_data::browse::view::SectionView],
) -> MenuDraft {
    let kind = sections.get(current).map(|section| section.kind);
    let source_rows: Vec<SrcRow> = sections
        .iter()
        .filter(|section| Some(section.kind) == kind && section.row.pinned)
        .map(|section| section.row.clone())
        .collect();
    let source_form =
        source_list::form(Level::Browse, groups, &source_rows, Tail::Recheck);
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
    let slots: Vec<Option<SrcTarget>> = source_form
        .slots()
        .into_iter()
        .map(|slot| slot.map(|b| b.action))
        .collect();
    let mut selected = None;
    let mut addresses = std::collections::HashMap::new();
    for slot in &slots {
        match slot {
            Some(SrcTarget::Library(index)) => {
                // A library the directory has no address for is drawn, never focusable.
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
                let identity = format!("section:{}:{}", sid.raw(), candidate.key);
                if candidate.row.current {
                    selected = Some(identity.clone());
                }
                addresses.insert(*index, (identity, target));
            }
            Some(SrcTarget::Recheck) => stamp.tag(5),
            None => stamp.tag(6),
        }
    }
    let form = source_form.map(|b| match b.action {
        SrcTarget::Library(index) => addresses.remove(&index).map(|(identity, target)| Binding {
            id: identity,
            key: RowKey(0),
            kind: b.kind,
            action: Action::Select(target),
            disabled: b.disabled,
        }),
        SrcTarget::Recheck => Some(Binding {
            id: "recheck".into(),
            key: RowKey(0),
            kind: b.kind,
            action: Action::Recheck,
            disabled: b.disabled,
        }),
    });
    MenuDraft {
        stamp: stamp.finish(),
        form,
        selected,
    }
}

fn sort_draft(sorts: &[SortEntry], sort_index: usize, sort_desc: bool) -> MenuDraft {
    let mut section = MenuSection::new(plx_platform::i18n::msg::browse_library_sort_by());
    let mut stamp = Stamp::default();
    stamp.tag(7);
    stamp.u32(sort_index as u32);
    stamp.bool(sort_desc);
    let mut selected = None;
    for (i, sort) in sorts.iter().enumerate() {
        let active = i == sort_index;
        if active {
            selected = Some(format!("sort:{}", sort.key));
        }
        let desc = if active {
            !sort_desc
        } else {
            sort.default_desc
        };
        let mut row = Row::new(&sort.title).checked(active);
        // The server advertises every sort's title; the client-side Plays entry is the app's own.
        if sort.key != plx_data::browse::PLAYS_SORT_KEY { row = row.server_label(); }
        if active {
            row = row.ticon(if sort_desc {
                plx_ui::icons::Icon::ChevronDown
            } else {
                plx_ui::icons::Icon::ChevronUp
            });
        }
        stamp.tag(8);
        stamp.str(&sort.key);
        stamp.str(&sort.title);
        stamp.bool(sort.default_desc);
        stamp.bool(desc);
        section = choice(
            section,
            format!("sort:{}", sort.key),
            Action::Edit(QueryEdit::Sort {
                key: sort.key.clone(),
                desc,
            }),
            row,
        );
    }
    MenuDraft {
        stamp: stamp.finish(),
        form: MenuForm::new().section(section),
        selected,
    }
}

/// The kind of the library `listing` lists — the section the directory names at the listing's own
/// address, so the TYPE menu offers what THAT library can list.
fn listing_kind(
    listing: plx_data::stores::browse::ListingView<'_>,
    directory: plx_data::stores::browse::DirectoryView<'_>,
) -> SecKind {
    listing.id().and_then(|id| directory.sections().iter()
        .find(|section| section.sid == Some(id.sid) && section.key == id.section))
        .map_or(SecKind::Movie, |section| section.kind)
}

/// The TYPE menu: every [`LibraryType`] the section's kind offers, the current one checked.
fn type_draft(section_kind: SecKind, current: LibraryType) -> MenuDraft {
    let mut section = MenuSection::new(plx_platform::i18n::msg::browse_library_filter_by());
    let mut selected = None;
    for &kind in LibraryType::offered(section_kind) {
        let identity = format!("type:{}", kind.code());
        if kind == current { selected = Some(identity.clone()); }
        section = choice(
            section,
            identity,
            Action::Edit(QueryEdit::LibraryType(kind)),
            Row::new(kind.title(section_kind)).checked(kind == current),
        );
    }
    let mut stamp = Stamp::default();
    stamp.tag(13);
    stamp.tag(u8::from(section_kind == SecKind::Show));
    stamp.u32(current.code());
    MenuDraft { stamp: stamp.finish(), form: MenuForm::new().section(section), selected }
}

fn filter_draft(unwatched: bool, genre: Option<&GenreEntry>, genres_supported: bool) -> MenuDraft {
    let mut section = MenuSection::new(plx_platform::i18n::msg::browse_library_filter()).item_keyed(
        "unwatched".into(),
        RowKey(0),
        RowKind::Toggle,
        Action::Edit(QueryEdit::Unwatched(!unwatched)),
        Row::new(plx_platform::i18n::msg::browse_library_unwatched_only()).toggle(unwatched),
    );
    if genres_supported {
        let mut row = Row::new(plx_platform::i18n::msg::browse_library_genre())
            .value(genre.map(|g| g.title.as_str()).unwrap_or(plx_platform::i18n::msg::browse_library_all()))
            .chevron(true);
        // A chosen genre is the server's tag title; "All" is the app's.
        if genre.is_some() { row = row.server_value(); }
        section = choice(section, "genre".into(), Action::Genre, row);
    }
    let mut stamp = Stamp::default();
    stamp.tag(9);
    stamp.bool(genres_supported);
    stamp.bool(unwatched);
    stamp.tag(u8::from(genre.is_some()));
    if let Some(genre) = genre {
        stamp.str(&genre.id);
        stamp.str(&genre.title);
    }
    MenuDraft {
        stamp: stamp.finish(),
        form: MenuForm::new().section(section),
        selected: None,
    }
}

fn genre_draft(genres: &[GenreEntry], current: Option<&GenreEntry>) -> MenuDraft {
    let mut section = choice(
        MenuSection::new(plx_platform::i18n::msg::browse_library_genre()),
        "genre:all".into(),
        Action::Edit(QueryEdit::Genre(None)),
        Row::new(plx_platform::i18n::msg::browse_library_all_genres()).checked(current.is_none()),
    );
    let mut stamp = Stamp::default();
    stamp.tag(10);
    stamp.tag(u8::from(current.is_some()));
    if let Some(current) = current {
        stamp.str(&current.id);
    }
    let mut selected = current.is_none().then(|| "genre:all".to_string());
    for genre in genres {
        let active = current.is_some_and(|selected| selected.id == genre.id);
        if active {
            selected = Some(format!("genre:{}", genre.id));
        }
        stamp.tag(11);
        stamp.str(&genre.id);
        stamp.str(&genre.title);
        stamp.bool(active);
        section = choice(
            section,
            format!("genre:{}", genre.id),
            Action::Edit(QueryEdit::Genre(Some(genre.id.clone()))),
            Row::new(&genre.title).checked(active).server_label(),
        );
    }
    MenuDraft {
        stamp: stamp.finish(),
        form: MenuForm::new().section(section),
        selected,
    }
}

pub(crate) struct LibraryMenu {
    entry: EntryId,
    arg: LibraryMenuArg,
    kind: LibraryMenuKind,
    /// The rows and their table, replaced together; a row's [`RowKey`] is its identity's slot in
    /// `identities`, so focus follows a row across a reorder.
    form: MenuTable,
    identities: Vec<String>,
    stamp: Vec<u8>,
    desired_unwatched: Option<bool>,
    #[cfg(test)] draft_rebuilds: usize,
}

impl LibraryMenu {
    pub(crate) fn new(entry: EntryId, arg: LibraryMenuArg) -> Self {
        Self {
            entry,
            kind: arg.kind,
            arg,
            form: MenuTable::new(crate::screens::registry::BAND),
            identities: Vec::new(),
            stamp: Vec::new(),
            desired_unwatched: None,
            #[cfg(test)] draft_rebuilds: 0,
        }
    }
    /// Hugs its rows: width is the shared menu rule ([`TableView::menu_panel_width`]), hung off the
    /// anchor's left edge and pulled back so the right edge stays inside the keep-out.
    fn frame(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        let [x, y, _, h] = self.arg.anchor.map(f32::from_bits);
        let height = self.form.table.measured_height().clamp(120.0, 740.0);
        let width = self.form.table.menu_panel_width(measure);
        Rect::new(
            x.clamp(96.0, (1920.0 - 96.0 - width).max(96.0)),
            (y + h + 16.0).clamp(96.0, 984.0 - height),
            width,
            height,
        )
    }
    fn refresh<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) {
        let listing = H::listing(cx);
        if self.desired_unwatched == Some(listing.unwatched()) {
            self.desired_unwatched = None;
        }
        let draft = self.draft(listing, H::directory(cx));
        self.apply_draft(draft);
    }

    fn apply_draft(&mut self, draft: MenuDraft) {
        if self.stamp == draft.stamp {
            return;
        }
        #[cfg(test)]
        {
            self.draft_rebuilds += 1;
        }

        let form = number(draft.form, &mut self.identities);
        // The focused row if it survives, else the draft's own pick.
        let keep = self
            .form
            .selected_id()
            .filter(|id| form.contains(id))
            .cloned()
            .or(draft.selected);
        self.stamp = draft.stamp;
        self.form.table.compact = true;
        self.form.set(form, keep.as_ref());
    }

    fn draft(
        &self,
        listing: plx_data::stores::browse::ListingView<'_>,
        directory: plx_data::stores::browse::DirectoryView<'_>,
    ) -> MenuDraft {
        let title = match self.kind {
            LibraryMenuKind::Type => plx_platform::i18n::msg::browse_library_filter_by(),
            LibraryMenuKind::Sort => plx_platform::i18n::msg::browse_library_sort_by(),
            LibraryMenuKind::Filter => plx_platform::i18n::msg::browse_library_filter(),
            LibraryMenuKind::Genre => plx_platform::i18n::msg::browse_library_genre(),
            LibraryMenuKind::Sources => plx_platform::i18n::msg::browse_library_libraries(),
        };
        let mut section = MenuSection::new(title);
        match self.kind {
            LibraryMenuKind::Type => return type_draft(listing_kind(listing, directory), listing.library_type()),
            LibraryMenuKind::Sort => {
                return sort_draft(listing.sorts(), listing.sort_index(), listing.sort_desc());
            }
            LibraryMenuKind::Filter => {
                return filter_draft(self.desired_unwatched.unwrap_or(listing.unwatched()), listing.genre(), listing.library_type() == LibraryType::Primary);
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
                section = choice(
                    section,
                    "recheck".into(),
                    Action::Recheck,
                    Row::new(plx_platform::i18n::msg::browse_library_check_shares()),
                );
            }
        }
        let mut stamp = Stamp::default();
        stamp.tag(12);
        stamp.u32(self.kind as u32);
        stamp.u32(listing.sort_index() as u32);
        stamp.bool(listing.unwatched());
        MenuDraft {
            stamp: stamp.finish(),
            form: MenuForm::new().section(section),
            selected: None,
        }
    }
    fn activate<H: LibraryLike>(&mut self, elem: u32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let Some(action) = self
            .form
            .index_of_key(RowKey(elem))
            .and_then(|i| self.form.binding_at(i))
            .map(|b| b.action.clone())
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
                if let QueryEdit::Unwatched(desired) = &edit {
                    self.desired_unwatched = Some(*desired);
                    self.refresh(cx);
                }
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
            ScreenEvent::App(AppMsg::Library(crate::screens::registry::LibraryCmd::SwitchStep(step))) => {
                let key = match step % 14 {
                    3 | 8 => Key::Down, 4 | 10 | 11 => Key::Back, 9 => Key::Ok,
                    _ => return Handled::No,
                };
                // Exercise normal menu input using this frame's clock and this instance's
                // engine owner. Never mutate the table selection as a script shortcut.
                for edge in [Edge::Down, Edge::Up] {
                    fx.push(Fx::Deliver(fx.from(), Delivery::Screen(ScreenEvent::Input(
                        plx_machine::machine::InputEvent { at: cx.tick, source: plx_machine::machine::Source::Script,
                            kind: InputKind::Key { key, sym: 0, wcode: 0, edge, at_edge: false } },
                    ))));
                }
                return Handled::Yes;
            }
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
                self.form.table.sel = cx
                    .focus
                    .current
                    .and_then(|focus| self.form.index_of_key(RowKey(focus.elem)))
                    .map_or(-1, |i| i as i32);
                self.form.table.update(tick.dt(), self.frame(cx.measure).h);
            }
            ScreenEvent::FocusMoved { to, .. } => {
                self.form.table.sel = self
                    .form
                    .index_of_key(RowKey(to.elem))
                    .map_or(-1, |i| i as i32);
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
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: GroupId(0),
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: self.frame(cx.measure),
            len: self.form.focusable_len(),
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, elem: &u32, _: &Cx<'_, H>) -> Option<GroupId> {
        self.form.index_of_key(RowKey(*elem)).map(|_| GroupId(0))
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _: &Cx<'_, H>) -> Step<u32> {
        let delta = match dir {
            Dir::Up => -1,
            Dir::Down => 1,
            _ => return Step::Edge,
        };
        self.form
            .step_key(RowKey(key.elem), delta)
            .map_or(Step::Edge, |next| {
                Step::Move(FocusKey {
                    entry: self.entry,
                    elem: next.0,
                })
            })
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, _: At) -> Option<Placed> {
        let index = self.form.index_of_key(RowKey(*elem))?;
        let rect = self.form.table.row_frame(self.frame(cx.measure), index as i32)?;
        Some(Placed {
            rect,
            rest_rect: rect,
            clip: self.frame(cx.measure),
            index: Some(index as u32),
        })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.group_of(&want.elem, cx).is_some() {
            want
        } else {
            FocusKey {
                entry: self.entry,
                elem: self.form.opening_key().map_or(0, |key| key.0),
            }
        }
    }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, H>) -> FocusKey<u32> {
        FocusKey {
            entry: self.entry,
            elem: self.form.selected_key().map_or(0, |key| key.0),
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
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, H>) {}
    /// The COMPACT role — the card menu's weight, because this is the same object one page over: a
    /// chip-shaped control on a live page opening a list beside it. The page recedes (inheriting
    /// its own light through the container's field) and stays readable.
    fn scrim(&self) -> plx_ui::screen::Scrim {
        plx_ui::screen::Scrim::dim(plx_ui::theme::underlay::DIM_COMPACT)
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f.painter.alpha(f.page_alpha);
        let measure = f.measure;
        // The panel's own share, named for `/tmp/plxnative-cpuprof` beside the page's `lb.*`
        // phases: the frosted ground plus its rows, so a slow frame while the Sort/Filter menu is
        // up can be read as the PANEL or as the host under it rather than as one `main.ui` total.
        let field = f.underlay;
        let frame = self.frame(measure);
        plx_ui::profile::phase("lb.menu", || {
            plx_ui::widgets::panel_ground(p, frame, PANEL_RADIUS, field);
            self.form.table.draw(p, frame, measure);
        });
        for index in 0..self.form.table.n_rows() as usize {
            let Some(key) = self.form.key_at(index) else { continue };
            if let Some(placed) = <Self as Focusable<H>>::place(self, &key.0, f.cx, At::Drawn) {
                f.stop(
                    p,
                    Stop {
                        key: FocusKey {
                            entry: self.entry,
                            elem: key.0,
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
        c.option(self.desired_unwatched.as_ref(), |c, desired| { c.bool(*desired); });
        c.seq(self.identities.len());
        for identity in &self.identities {
            c.str(identity);
        }
        // The length-safe dependency encoding includes every row's data and semantic action.
        // The identity registry alone cannot distinguish a reorder or changed action payload.
        c.seq(self.stamp.len());
        for byte in &self.stamp { c.u8(*byte); }
        // Each focusable row as (key, table index), in layout order.
        c.seq(self.form.focusable_len());
        for index in 0..self.form.table.n_rows() as usize {
            if let Some(key) = self.form.key_at(index) {
                c.u32(key.0).u32(index as u32);
            }
        }
        self.form.table.write_motion(c);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("library_menu");
    }
}

#[cfg(test)]
#[path = "review_actions_tests.rs"]
mod review_actions_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use plx_data::browse::{SecKind, SourceState};
    use plx_plex::plex::ServerId;
    use plx_ui::fixture::FixtureMeasure;
    use plx_machine::machine::{FocusRead, Host, InputOwner, PressRead, Tick};
    use plx_ui::screen::ScreenArg;

    /// A draft as a table lays it out — the drawn sections, and every focusable row as
    /// `(identity, action, table index)` — so a test reads WHAT the menu shows and binds without
    /// reaching into the form's internals.
    struct Laid {
        stamp: Vec<u8>,
        sections: Vec<plx_ui::table::Section>,
        rows: Vec<(String, Action, i32)>,
        /// The table index the menu would seat: the draft's own pick, else the first row.
        selected: i32,
    }
    fn lay(draft: MenuDraft) -> Laid {
        let mut identities = Vec::new();
        let mut laid = MenuTable::new(crate::screens::registry::BAND);
        laid.set(number(draft.form, &mut identities), None);
        let rows = (0..laid.table.n_rows() as usize)
            .filter_map(|i| {
                let b = laid.binding_at(i)?;
                Some((b.id.clone(), b.action.clone(), i as i32))
            })
            .collect();
        let selected = draft.selected.and_then(|id| laid.index_of(&id)).map_or(0, |i| i as i32);
        Laid { stamp: draft.stamp, sections: std::mem::take(&mut laid.table.sections), rows, selected }
    }

    #[derive(Clone)]
    struct Arg;
    impl LogicalState for Arg {
        fn write(&self, _: &mut Canon) {}
        fn probe(&self, _: &mut String) {}
    }
    impl ScreenArg for Arg {
        fn chrome(&self) -> plx_machine::machine::Chrome {
            plx_machine::machine::Chrome::None
        }
        fn id(&self) -> plx_machine::machine::ScreenId {
            plx_machine::machine::ScreenId(1)
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
        listing: plx_data::stores::browse::ListingView<'a>,
        directory: plx_data::stores::browse::DirectoryView<'a>,
        hubs: plx_data::stores::browse::HubsView<'a>,
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
        fn listing<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::ListingView<'a> {
            cx.views.listing
        }
        fn directory<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::DirectoryView<'a> {
            cx.views.directory
        }
        fn section_hubs<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::HubsView<'a> {
            cx.views.hubs
        }
    }

    fn with_cx<R>(test: impl FnOnce(&Cx<'_, HostFixture>) -> R) -> R {
        let listing = plx_data::browse::view::ListingSnapshot::fixture(
            ServerId::from_raw(1),
            Vec::new(),
            Vec::new(),
        );
        let directory = plx_data::stores::browse::DirectorySnapshot::default();
        let hubs = plx_data::stores::browse::HubsSnapshot::empty_for_test();
        let measure = FixtureMeasure;
        test(&Cx {
            views: Views {
                listing: listing.view(),
                directory: directory.view(),
                hubs: hubs.view(),
            },
            tick: Tick::default(),
            measure: &measure,
            focus: FocusRead { current: None , ..Default::default() },
            press: PressRead::default(),
            owner: InputOwner::Entry(EntryId(7)),
        })
    }

    #[test]
    fn sort_refresh_updates_icon_and_the_next_action_direction() {
        let sorts = vec![SortEntry {
            desc_key: String::new(),
            key: "titleSort".into(),
            title: "Title".into(),
            default_desc: false,
        }];
        let up = lay(sort_draft(&sorts, 0, false));
        assert_eq!(
            up.sections[0].rows[0].ticon,
            Some(plx_ui::icons::Icon::ChevronUp)
        );
        assert!(matches!(
            up.rows[0].1,
            Action::Edit(QueryEdit::Sort { desc: true, .. })
        ));

        let down = lay(sort_draft(&sorts, 0, true));
        assert_eq!(
            down.sections[0].rows[0].ticon,
            Some(plx_ui::icons::Icon::ChevronDown)
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
            host: plx_machine::machine::InstanceId(8),
            target: SectionAddress { epoch: 11, sid: ServerId::from_raw(1), section: 7 },
            kind: LibraryMenuKind::Sort, anchor: [0; 4],
        });
        let mut sorts = vec![
            SortEntry { key: "titleSort".into(), desc_key: String::new(), title: "Title".into(), default_desc: false },
            SortEntry { key: "addedAt".into(), desc_key: String::new(), title: "Added".into(), default_desc: true },
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
        let draft = lay(genre_draft(&genres, Some(&genres[1])));
        assert_eq!(draft.sections[0].rows[0].label, "All Genres");
        assert!(!draft.sections[0].rows[0].checked);
        assert_eq!(draft.sections[0].rows[2].label, "Comedy");
        assert!(draft.sections[0].rows[2].checked);
        assert_eq!(draft.selected, 2);

        let changed = lay(genre_draft(&genres, Some(&genres[0])));
        assert_ne!(draft.stamp, changed.stamp);
        assert!(changed.sections[0].rows[1].checked);
        assert!(!changed.sections[0].rows[2].checked);
    }

    #[test]
    fn tv_type_menu_checks_and_commits_each_granularity() {
        let types = [LibraryType::Primary, LibraryType::Seasons, LibraryType::Episodes, LibraryType::Collections];
        for (selected, current) in types.into_iter().enumerate() {
            let draft = lay(type_draft(SecKind::Show, current));
            assert_eq!(draft.selected, selected as i32);
            assert_eq!(draft.sections[0].rows.iter().map(|row| row.label.as_str()).collect::<Vec<_>>(),
                ["TV Shows", "Seasons", "Episodes", "Collections"]);
            for (index, row) in draft.sections[0].rows.iter().enumerate() {
                assert_eq!(row.checked, index == selected);
            }
            assert!(matches!(draft.rows[selected].1, Action::Edit(QueryEdit::LibraryType(kind)) if kind == current));
        }
        assert_ne!(type_draft(SecKind::Show, LibraryType::Primary).stamp,
            lay(type_draft(SecKind::Show, LibraryType::Episodes)).stamp);
    }

    #[test]
    fn movie_type_menu_offers_movies_and_collections() {
        for (selected, current) in [LibraryType::Primary, LibraryType::Collections].into_iter().enumerate() {
            let draft = lay(type_draft(SecKind::Movie, current));
            assert_eq!(draft.selected, selected as i32);
            assert_eq!(draft.sections[0].rows.iter().map(|row| row.label.as_str()).collect::<Vec<_>>(),
                ["Movies", "Collections"]);
            assert!(matches!(draft.rows[selected].1, Action::Edit(QueryEdit::LibraryType(kind)) if kind == current));
        }
        // The same checked row names a different menu in the other kind of library.
        assert_ne!(type_draft(SecKind::Movie, LibraryType::Collections).stamp,
            lay(type_draft(SecKind::Show, LibraryType::Collections)).stamp);
    }

    #[test]
    fn filter_refresh_updates_genre_value_and_unwatched_action() {
        let drama = GenreEntry {
            id: "7".into(),
            title: "Drama".into(),
        };
        let all = lay(filter_draft(false, None, true));
        assert_eq!(all.sections[0].rows[1].value.as_deref(), Some("All"));
        assert!(matches!(
            all.rows[0].1,
            Action::Edit(QueryEdit::Unwatched(true))
        ));

        let filtered = lay(filter_draft(true, Some(&drama), true));
        assert_eq!(filtered.sections[0].rows[1].value.as_deref(), Some("Drama"));
        assert!(filtered.sections[0].rows[0].toggle == Some(true));
        assert!(matches!(
            filtered.rows[0].1,
            Action::Edit(QueryEdit::Unwatched(false))
        ));
        assert_ne!(all.stamp, filtered.stamp);
    }

    fn source_sections() -> (Vec<SrcGroup>, Vec<plx_data::browse::view::SectionView>) {
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
            plx_data::browse::view::SectionView {
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
            plx_data::browse::view::SectionView {
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
        // with_cx retains the same explicit directory shape a Bridge captures from its owner.
        let _guard = plx_base::testlock::serial();
        let (groups, sections) = source_sections();
        let draft = lay(source_draft(11, 0, &groups, &sections));
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
                host: plx_machine::machine::InstanceId(8),
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
        let recheck = menu.form.key_at(3).expect("recheck is bound").0;
        let placed = with_cx(|cx|
            <LibraryMenu as Focusable<HostFixture>>::place(&menu, &recheck, cx, At::Drawn)
                .expect("recheck remains placed after the separator"));
        assert_eq!(placed.index, Some(3));
        let mut output = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut fx = Effects::new(
            &mut output,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(8)),
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
        let before = lay(source_draft(11, 0, &groups, &sections));
        let mut changed_groups = groups.clone();
        changed_groups[1].state = SourceState::Unreachable;
        let mut changed_sections = sections.clone();
        changed_sections[1].row.count_line = "5 films".into();
        changed_sections[1].row.current = true;
        let after = lay(source_draft(11, 1, &changed_groups, &changed_sections));

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
        let first = lay(source_draft(11, 0, &groups, &sections));
        groups[0].name = "A".into();
        groups[0].handle = "B:C".into();
        let second = lay(source_draft(11, 0, &groups, &sections));
        assert_ne!(first.stamp, second.stamp);

        groups[0].name = "A|B".into();
        let third = lay(source_draft(11, 0, &groups, &sections));
        groups[0].name = "A".into();
        groups[0].handle = "B:C|A|B".into();
        let fourth = lay(source_draft(11, 0, &groups, &sections));
        assert_ne!(third.stamp, fourth.stamp);
    }

    #[test]
    fn metadata_refresh_preserves_focus_by_stable_element_key() {
        let (groups, sections) = source_sections();
        let mut menu = LibraryMenu::new(
            EntryId(7),
            LibraryMenuArg {
                host: plx_machine::machine::InstanceId(8),
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
        let focused_key = menu.form.key_at(1).expect("second source row").0;
        menu.form.table.sel = 1;
        let quiet = source_draft(11, 0, &groups, &sections);
        menu.apply_draft(quiet);
        assert_eq!(
            menu.form.table.sel, 1,
            "identical refresh does not reset TableView selection"
        );

        let mut shifted = sections.clone();
        shifted.insert(
            1,
            plx_data::browse::view::SectionView {
                sid: Some(ServerId::from_raw(1)),
                key: 8,
                kind: SecKind::Movie,
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
            menu.form.index_of_key(RowKey(focused_key)),
            Some(2)
        );
        assert_eq!(
            menu.form.table.sel, 2,
            "metadata/shape refresh preserves the focused source key"
        );
    }

    include!("menu_contract_tests.rs");

    /// **Every TYPE row fits the popover, in every shipped language** — movie and TV sections
    /// alike, Collections included, with the section header. Measured with the device's
    /// whole-pixel advances at the shared menu cap (`MENU_MAX_W`) and at the width the hugged
    /// popover actually gets.
    #[test]
    fn every_type_row_fits_the_popover_in_every_language() {
        use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
        let measure = plx_base::fontcov::advances::ShippedMeasure;
        let mut out = Vec::new();
        for language in SHIPPED {
            let _guard = language_on_this_thread_for_test(language);
            for kind in [SecKind::Movie, SecKind::Show] {
                let draft = lay(type_draft(kind, LibraryType::Collections));
                let mut table = TableView::new();
                table.set_sections(draft.sections, draft.selected, false);
                out.extend(table.menu_cap_failure(&measure, &format!("{} {kind:?}", language.tag())));
                out.extend(table.app_fit_failures(MENU_MAX_W, &format!("{} {kind:?}", language.tag())));
                out.extend(table.app_fit_failures_hugged(&format!("{} {kind:?}", language.tag())));
            }
        }
        plx_ui::table::assert_no_fit_failures(&out);
    }

    /// **The Sort, Filter and Genre popovers' app text fits the popover, in every shipped language**
    /// — through the real drafts, with server sort/genre titles marked and exempt. Sort covers the
    /// client-side Plays entry (app text); Filter covers both the "All" value and a chosen genre.
    #[test]
    fn every_sort_filter_and_genre_row_fits_the_popover_in_every_language() {
        use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
        let sorts = vec![
            SortEntry { key: "titleSort".into(), desc_key: String::new(), title: "Title".into(), default_desc: false },
            SortEntry { key: plx_data::browse::PLAYS_SORT_KEY.into(), desc_key: String::new(),
                title: plx_platform::i18n::msg::browse_library_plays().into(), default_desc: true },
        ];
        let genres = vec![GenreEntry { id: "1".into(), title: "Drama".into() }];
        let measure = plx_base::fontcov::advances::ShippedMeasure;
        let mut out = Vec::new();
        for language in SHIPPED {
            let _guard = language_on_this_thread_for_test(language);
            let tag = language.tag();
            let drafts = [
                ("sort asc", lay(sort_draft(&sorts, 1, false))),
                ("sort desc", lay(sort_draft(&sorts, 0, true))),
                ("filter all", lay(filter_draft(false, None, true))),
                ("filter genre", lay(filter_draft(true, Some(&genres[0]), true))),
                ("filter no genres", lay(filter_draft(false, None, false))),
                ("genre all", lay(genre_draft(&genres, None))),
                ("genre one", lay(genre_draft(&genres, Some(&genres[0])))),
            ];
            for (name, draft) in drafts {
                let mut table = TableView::new();
                table.set_sections(draft.sections, draft.selected, false);
                out.extend(table.menu_cap_failure(&measure, &format!("{tag} {name}")));
                out.extend(table.app_fit_failures(MENU_MAX_W, &format!("{tag} {name}")));
                out.extend(table.app_fit_failures_hugged(&format!("{tag} {name}")));
            }
        }
        plx_ui::table::assert_no_fit_failures(&out);
    }
}
