//! A registered Library menu surface. Navigation owns its lifetime, phase, and input scope.
use plx_data::browse::{LibraryType, GenreEntry, SecKind, SortEntry, SrcGroup, SrcRow};
use crate::registry::{AppFx, AppMsg, LibraryLike, LibraryMenuArg, LibraryMenuKind};
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
use plx_appkit::source_list::{self, Level, SrcTarget, Tail};
use plx_ui::popover::Popover;
use plx_ui::table::{Row, TableView};
use std::convert::Infallible;
#[cfg(test)]
use plx_ui::table::MENU_MAX_W;
use plx_ui::Rect;
use std::borrow::Cow;

// The compact Library menu's existing corner geometry.
const PANEL_RADIUS: f32 = 20.0;

pub const SHAPE: [&str; 2] = [
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
    folded: &[u32],
) -> MenuDraft {
    let kind = sections.get(current).map(|section| section.kind);
    // Only the libraries the header's fold hid: the visible names and the one on view are not here.
    let source_rows: Vec<SrcRow> = sections
        .iter()
        .filter(|section| Some(section.kind) == kind && section.row.pinned
            && u32::try_from(section.row.section).is_ok_and(|index| folded.contains(&index)))
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
    let mut stamp = Stamp::default();
    let mut selected = None;
    let section = sort_section(plx_platform::i18n::msg::browse_library_sort_by(), sorts, sort_index, sort_desc, &mut stamp, &mut selected);
    MenuDraft { stamp: stamp.finish(), form: MenuForm::new().section(section), selected }
}

/// The sort rows, under `title`, stamped into `stamp`; `selected` takes the active sort's identity.
fn sort_section(title: &str, sorts: &[SortEntry], sort_index: usize, sort_desc: bool,
    stamp: &mut Stamp, selected: &mut Option<String>) -> MenuSection
{
    let mut section = MenuSection::new(title);
    stamp.tag(7);
    stamp.u32(sort_index as u32);
    stamp.bool(sort_desc);
    for (i, sort) in sorts.iter().enumerate() {
        let active = i == sort_index;
        if active {
            *selected = Some(format!("sort:{}", sort.key));
        }
        let desc = if active {
            !sort_desc
        } else {
            sort.default_desc
        };
        // The name is the shared table's for a fact it knows (`plx_data::sortfacts`), the server's
        // otherwise; the checked row says which end comes first in a WORD, never a chevron.
        let mut row = Row::new(sort.label()).checked(active);
        if !sort.label_is_ours() { row = row.server_label(); }
        if active {
            if let Some(word) = sort.direction_word(sort_desc) { row = row.value(word); }
        }
        stamp.tag(8);
        stamp.str(&sort.key);
        stamp.str(sort.label());
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
    section
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
    let mut stamp = Stamp::default();
    let mut selected = None;
    let section = type_section(plx_platform::i18n::msg::browse_library_filter_by(), section_kind, current,
        LibraryType::offered(section_kind), &mut stamp, &mut selected);
    MenuDraft { stamp: stamp.finish(), form: MenuForm::new().section(section), selected }
}

/// One row per type in `offered`, the current one checked.
fn type_section(title: &str, section_kind: SecKind, current: LibraryType, offered: &[LibraryType],
    stamp: &mut Stamp, selected: &mut Option<String>) -> MenuSection
{
    let mut section = MenuSection::new(title);
    for &kind in offered {
        let identity = format!("type:{}", kind.code());
        if kind == current { *selected = Some(identity.clone()); }
        section = choice(
            section,
            identity,
            Action::Edit(QueryEdit::LibraryType(kind)),
            Row::new(kind.title(section_kind)).checked(kind == current),
        );
    }
    stamp.tag(13);
    stamp.tag(u8::from(section_kind == SecKind::Show));
    stamp.u32(current.code());
    section
}

fn filter_draft(unwatched: bool, genre: Option<&GenreEntry>, genres_supported: bool) -> MenuDraft {
    let mut stamp = Stamp::default();
    let section = filter_section(unwatched, genre, genres_supported, &mut stamp);
    MenuDraft { stamp: stamp.finish(), form: MenuForm::new().section(section), selected: None }
}

/// The Unwatched toggle and the Genre row, stamped into `stamp`.
fn filter_section(unwatched: bool, genre: Option<&GenreEntry>, genres_supported: bool, stamp: &mut Stamp) -> MenuSection {
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
    stamp.tag(9);
    stamp.bool(genres_supported);
    stamp.bool(unwatched);
    stamp.tag(u8::from(genre.is_some()));
    if let Some(genre) = genre {
        stamp.str(&genre.id);
        stamp.str(&genre.title);
    }
    section
}

/// **The one Sort & Filter menu** (redesign spec, "Header"): Show, Sort and Filter in one popover.
/// Show lists the types the library offers besides Collections (which has its own control) and is
/// absent when only one remains; Sort is the server's declared list; Filter is the genre and the
/// Unwatched toggle.
fn sort_filter_draft(
    section_kind: SecKind,
    current: LibraryType,
    sorts: &[SortEntry],
    sort_index: usize,
    sort_desc: bool,
    genre: Option<&GenreEntry>,
    unwatched: bool,
) -> MenuDraft {
    let mut stamp = Stamp::default();
    stamp.tag(14);
    let mut selected = None;
    let mut form = MenuForm::new();
    let shown: Vec<LibraryType> = LibraryType::offered(section_kind).iter().copied()
        .filter(|kind| *kind != LibraryType::Collections).collect();
    if shown.len() > 1 {
        form = form.section(type_section(plx_platform::i18n::msg::library_header_show(), section_kind,
            current, &shown, &mut stamp, &mut selected));
    }
    form = form.section(sort_section(plx_platform::i18n::msg::browse_library_sort(), sorts, sort_index,
        sort_desc, &mut stamp, &mut selected));
    let filter = filter_section(unwatched, genre, current == LibraryType::Primary, &mut stamp);
    MenuDraft { stamp: stamp.finish(), form: form.section(filter), selected }
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

pub struct LibraryMenu {
    entry: EntryId,
    arg: LibraryMenuArg,
    kind: LibraryMenuKind,
    /// The rows and their table, replaced together; a row's [`RowKey`] is its identity's slot in
    /// `identities`, so focus follows a row across a reorder.
    form: MenuTable,
    identities: Vec<String>,
    stamp: Vec<u8>,
    desired_unwatched: Option<bool>,
    /// The open/close motion: the panel MORPHS out of the control it opened from and back into it
    /// (`Popover::morphing` + `open_from`, the path the Watchlist's Sort & Filter takes: one opener
    /// rule for every header menu, design review S11). It owns the glass's arrival; the control
    /// itself is the host's opener lift (S12), so this draws no pill of its own.
    popover: Popover,
    #[cfg(test)] draft_rebuilds: usize,
}

impl LibraryMenu {
    pub fn new(entry: EntryId, arg: LibraryMenuArg) -> Self {
        Self {
            entry,
            kind: arg.kind,
            arg,
            form: MenuTable::new(crate::registry::BAND),
            identities: Vec::new(),
            stamp: Vec::new(),
            desired_unwatched: None,
            popover: Popover::new().morphing(),
            #[cfg(test)] draft_rebuilds: 0,
        }
    }
    /// The control's focused pill: where the panel grows from and shrinks back into, and what its
    /// width is measured against.
    fn pill(&self) -> Rect {
        let [x, y, w, h] = self.arg.anchor.map(f32::from_bits);
        plx_ui::widgets::focus_pill(Rect::new(x, y, w, h))
    }
    /// Hugs its rows: width is the shared menu rule ([`TableView::menu_panel_width_from`]: the
    /// rows, never narrower than the control's focused pill), hung off the opener's near edge
    /// ([`plx_ui::popover::hang_x`]) and held inside the safe frame. ONE placement for every kind: the
    /// "+N" list opens under its "+N" pill exactly as Sort & Filter does; it differs only in its
    /// ceiling, because its rows are library NAMES a user chose: as wide as the widest, up to the
    /// safe width, never cut with room to spare.
    fn frame(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        let [_, y, _, h] = self.arg.anchor.map(f32::from_bits);
        let height = self.form.table.measured_height().clamp(120.0, 740.0);
        let pill = self.pill();
        let width = if self.kind == LibraryMenuKind::Sources {
            self.form.table.menu_panel_width_to(measure, pill, plx_ui::page_header::TEXT_X1 - 96.0)
        } else {
            self.form.table.menu_panel_width_from(measure, pill)
        };
        Rect::new(
            // The shared rule: the panel's near edge is the opener's outer edge (NV1).
            plx_ui::popover::hang_x(pill, width, plx_ui::consts::SAFE),
            (y + h + 16.0).clamp(96.0, 984.0 - height),
            width,
            height,
        )
    }
    /// The panel as it reads: each section's heading and its rows' `(label, value)`, for a route
    /// test that compares the Library's words with the Watchlist's.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_panel(&self) -> Vec<(String, Vec<(String, Option<String>)>)> {
        self.form.table.sections.iter()
            .map(|section| (section.header.clone(), section.rows.iter().map(|row| (row.label.clone(), row.value.clone())).collect()))
            .collect()
    }
    /// The identity of the row focus is on (`"unwatched"`, `"sort:titleSort"`, a library's row), as
    /// a route test names what a key landed on; `None` off the menu.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_row(&self, focus: Option<FocusKey<u32>>) -> Option<&str> {
        focus.filter(|key| key.entry == self.entry)
            .and_then(|key| self.identities.get(key.elem as usize)).map(String::as_str)
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
            LibraryMenuKind::SortFilter => plx_platform::i18n::msg::library_header_sort_filter(),
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
            LibraryMenuKind::SortFilter => {
                return sort_filter_draft(listing_kind(listing, directory), listing.library_type(), listing.sorts(),
                    listing.sort_index(), listing.sort_desc(), listing.genre(),
                    self.desired_unwatched.unwrap_or(listing.unwatched()));
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
                        &self.arg.folded,
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
            ScreenEvent::App(AppMsg::Library(crate::registry::LibraryCmd::SwitchStep(step))) => {
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
            ScreenEvent::Mount => {
                self.popover.open_from(Some(self.pill()));
                self.refresh(cx)
            }
            // However the surface was dismissed (BACK, a pick, the host's own press), the panel
            // runs the morph back into its control.
            ScreenEvent::Closing => self.popover.dismiss(),
            ScreenEvent::StoreChanged(..) | ScreenEvent::Enter(_) => {
                self.refresh(cx)
            }
            ScreenEvent::Tick(tick) => {
                self.popover.update(tick.dt());
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
                        // Back from the genre list returns to the menu it was opened from.
                        self.kind = if self.arg.kind == LibraryMenuKind::SortFilter { LibraryMenuKind::SortFilter }
                            else { LibraryMenuKind::Filter };
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
    /// The header control the panel grew out of, for every kind of menu ("+N" included): the page
    /// leaves it out of its pass (`DrawFrame::lifted`) and the opener lift draws it focused above
    /// the scrim (`LibraryScreen::draw_header_opener`), so the control the panel is about stays
    /// lit while it is open (design review S12).
    fn opener(&self) -> Option<(EntryId, Option<FocusKey<u32>>)> {
        self.arg.opener.map(|(entry, control)| (entry, Some(FocusKey { entry, elem: control })))
    }
    fn name(&self) -> &'static str {
        "library_menu"
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> { Some(self) }
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
        // The glass and the rows ride the MORPH (`Popover::panel` / `morph_rows`), not the
        // container's appear: the panel begins and ends as the control it grows from.
        let field = f.underlay;
        let frame = self.frame(measure);
        let m = self.popover.content_painter_on(f.painter, 0.0);
        plx_ui::profile::phase("lb.menu", || {
            self.popover.panel(m, frame, PANEL_RADIUS, field, plx_ui::widgets::Ground::Menu);
            self.popover.morph_rows(m, frame, PANEL_RADIUS, |m| self.form.table.draw(m, frame, measure));
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
        let mut laid = MenuTable::new(crate::registry::BAND);
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
        type Memory = crate::registry::PageMemory;
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
    fn sort_refresh_updates_the_direction_word_and_the_next_action_direction() {
        let _g = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
        let sorts = vec![SortEntry {
            desc_key: String::new(),
            key: "titleSort".into(),
            title: "Title".into(),
            default_desc: false,
        }];
        let up = lay(sort_draft(&sorts, 0, false));
        assert_eq!(up.sections[0].rows[0].value.as_deref(), Some("A\u{2013}Z"));
        assert_eq!(up.sections[0].rows[0].ticon, None, "a word says the direction, not a chevron");
        assert!(matches!(
            up.rows[0].1,
            Action::Edit(QueryEdit::Sort { desc: true, .. })
        ));

        let down = lay(sort_draft(&sorts, 0, true));
        assert_eq!(down.sections[0].rows[0].value.as_deref(), Some("Z\u{2013}A"));
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
            kind: LibraryMenuKind::Sort, anchor: [0; 4], folded: Vec::new(), opener: None,
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
        let draft = lay(source_draft(11, 0, &groups, &sections, &(0..9).collect::<Vec<u32>>()));
        assert_eq!(draft.sections.len(), 2);
        assert_eq!(draft.sections[0].header, "Own NAS");
        assert_eq!(draft.sections[1].header, "Shared with you", "a shared server is a group, not a machine name");
        assert_eq!(draft.sections[1].accessory, "");
        assert_eq!(draft.sections[1].rows[0].value.as_deref(), Some("friend"), "its owner stands on the row, dimmed");
        assert!(draft.sections[0].rows[0].detail.is_empty(), "one-line rows: no count line");
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
                folded: (0..8).collect(),
                opener: None,
            },
        );
        menu.apply_draft(source_draft(11, 0, &groups, &sections, &(0..9).collect::<Vec<u32>>()));
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
        let before = lay(source_draft(11, 0, &groups, &sections, &(0..9).collect::<Vec<u32>>()));
        let mut changed_groups = groups.clone();
        changed_groups[1].state = SourceState::Unreachable;
        let mut changed_sections = sections.clone();
        changed_sections[1].row.count_line = "5 films".into();
        changed_sections[1].row.current = true;
        let after = lay(source_draft(11, 1, &changed_groups, &changed_sections, &(0..9).collect::<Vec<u32>>()));

        assert_ne!(before.stamp, after.stamp);
        assert!(after.sections[1].dim);
        assert_eq!(
            before.rows.iter().map(|r| &r.0).collect::<Vec<_>>(),
            after.rows.iter().map(|r| &r.0).collect::<Vec<_>>()
        );
        assert_eq!(after.selected, 1);
        assert!(after.sections[1].rows[0].detail.is_empty());
    }

    #[test]
    fn source_stamp_is_length_safe_for_colons_and_pipes_in_display_text() {
        let (mut groups, sections) = source_sections();
        groups[0].name = "A:B".into();
        groups[0].handle = "C".into();
        let first = lay(source_draft(11, 0, &groups, &sections, &(0..9).collect::<Vec<u32>>()));
        groups[0].name = "A".into();
        groups[0].handle = "B:C".into();
        let second = lay(source_draft(11, 0, &groups, &sections, &(0..9).collect::<Vec<u32>>()));
        assert_ne!(first.stamp, second.stamp);

        groups[0].name = "A|B".into();
        let third = lay(source_draft(11, 0, &groups, &sections, &(0..9).collect::<Vec<u32>>()));
        groups[0].name = "A".into();
        groups[0].handle = "B:C|A|B".into();
        let fourth = lay(source_draft(11, 0, &groups, &sections, &(0..9).collect::<Vec<u32>>()));
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
                folded: (0..8).collect(),
                opener: None,
            },
        );
        menu.apply_draft(source_draft(11, 0, &groups, &sections, &(0..9).collect::<Vec<u32>>()));
        let focused_key = menu.form.key_at(1).expect("second source row").0;
        menu.form.table.sel = 1;
        let quiet = source_draft(11, 0, &groups, &sections, &(0..9).collect::<Vec<u32>>());
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
        menu.apply_draft(source_draft(11, 0, &groups, &shifted, &(0..9).collect::<Vec<u32>>()));
        assert_eq!(
            menu.form.index_of_key(RowKey(focused_key)),
            Some(2)
        );
        assert_eq!(
            menu.form.table.sel, 2,
            "metadata/shape refresh preserves the focused source key"
        );
    }

    #[test]
    fn the_plus_n_list_shows_only_folded_libraries_one_line_each_then_the_recheck_row() {
        let _guard = plx_base::testlock::serial();
        let (groups, sections) = source_sections();
        let draft = lay(source_draft(11, 0, &groups, &sections, &[1]));
        assert_eq!(draft.sections.len(), 1, "the own server has nothing folded: no group");
        assert_eq!(draft.sections[0].header, "Shared with you");
        assert_eq!(draft.sections[0].rows[0].label, "Shared Movies");
        assert!(draft.sections[0].rows[0].detail.is_empty(), "no \"N films\" second line");
        assert!(!draft.sections[0].rows[0].checked);
        assert_eq!(draft.sections[0].rows[0].value.as_deref(), Some("friend"));
        assert!(matches!(draft.rows.last().unwrap().1, Action::Recheck), "the shares check stays, last");
        assert_eq!(draft.rows.len(), 2);
    }

    #[test]
    fn the_sort_filter_panel_is_flush_with_its_opener_and_lifts_its_control() {
        let _guard = plx_base::testlock::serial();
        let arg = |kind, opener| LibraryMenuArg {
            host: plx_machine::machine::InstanceId(8),
            target: SectionAddress { epoch: 11, sid: ServerId::from_raw(1), section: 7 },
            kind,
            anchor: [1640.0f32.to_bits(), 148.0f32.to_bits(), 150.0f32.to_bits(), 56.0f32.to_bits()],
            folded: Vec::new(),
            opener,
        };
        let mut menu = LibraryMenu::new(EntryId(7), arg(LibraryMenuKind::SortFilter, Some((EntryId(3), 5))));
        menu.apply_draft(filter_draft(false, None, true));
        let frame = menu.frame(&FixtureMeasure);
        // NV1: the near edge IS the opener's outer edge (the focused pill, 1792), not the header's
        // text edge 1771 that sits 21 px inside the control's plate.
        let pill_edge = 1640.0 + 150.0 + plx_ui::widgets::PILL_FOCUS_OUTSET;
        assert!((frame.x + frame.w - pill_edge).abs() < 0.01, "right edge {} is the opener's edge {pill_edge}", frame.x + frame.w);
        assert_eq!(
            <LibraryMenu as Screen<HostFixture>>::opener(&menu),
            Some((EntryId(3), Some(FocusKey { entry: EntryId(3), elem: 5 }))),
            "the page's control is lifted into the menu");
        let more = LibraryMenu::new(EntryId(7), arg(LibraryMenuKind::Sources, Some((EntryId(3), 6))));
        assert_eq!(
            <LibraryMenu as Screen<HostFixture>>::opener(&more),
            Some((EntryId(3), Some(FocusKey { entry: EntryId(3), elem: 6 }))),
            "the +N list lifts its +N too: it reads as the focused opener, not as the page's dimmed capsule");
    }

    /// **A header menu GROWS out of the control it opened from** (design review S11), as the
    /// Watchlist's Sort & Filter does: one opener rule, one morph path (`Popover::morphing` +
    /// `open_from`), whichever kind opens ("+N" included). Red while the panel was laid at its
    /// final rect on its first frame and only faded in: three frames in, the glass is still the
    /// control's size or a little over, and the settled panel is exactly `frame()`.
    #[test]
    fn the_panel_grows_out_of_its_control_and_settles_on_its_frame() {
        use plx_machine::machine::MachineId;
        let _guard = plx_base::testlock::serial();
        for kind in [LibraryMenuKind::SortFilter, LibraryMenuKind::Sources] {
            let anchor = Rect::new(1640.0, 148.0, 150.0, 56.0);
            let arg = LibraryMenuArg {
                host: plx_machine::machine::InstanceId(8),
                target: SectionAddress { epoch: 11, sid: ServerId::from_raw(1), section: 7 },
                kind,
                anchor: [anchor.x, anchor.y, anchor.w, anchor.h].map(f32::to_bits),
                folded: Vec::new(),
                opener: Some((EntryId(3), 5)),
            };
            let mut menu = LibraryMenu::new(EntryId(7), arg);
            let step = |menu: &mut LibraryMenu, ev: ScreenEvent<HostFixture>| {
                with_cx(|cx| {
                    let mut present = plx_machine::present::Present::new();
                    let mut out = Vec::new();
                    let mut fx = Effects::new(&mut out, MachineId::Instance(plx_machine::machine::InstanceId(10)), &mut present);
                    Machine::<HostFixture>::step(menu, &ev, cx, &mut fx);
                })
            };
            // The glass this frame: the panel ground (a `rect_rimmed`, tag 8, on a host with no
            // frozen field) the surface drew.
            let glass = |menu: &mut LibraryMenu| -> Rect {
                with_cx(|cx| {
                    plx_ui::draw_census::capture(|| {
                        let mut f = DrawFrame::new(cx, plx_ui::Painter::recording());
                        Screen::<HostFixture>::draw(menu, &mut f);
                    })
                })
                .into_iter()
                .find_map(|(tag, r)| (tag == 8).then_some(r))
                .unwrap_or_default()
            };
            step(&mut menu, ScreenEvent::Mount);
            let settled = menu.frame(&FixtureMeasure);
            let mut ms = 0;
            for _ in 0..3 {
                ms += 16;
                step(&mut menu, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
            }
            let early = glass(&mut menu);
            assert!(
                early.w < settled.w - 20.0 && early.h < settled.h - 20.0,
                "{kind:?}: three frames into the open the panel is still growing out of the control \
                 ({early:?}), not laid at its final {settled:?}"
            );
            for _ in 0..90 {
                ms += 16;
                step(&mut menu, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
            }
            let end = glass(&mut menu);
            assert!(
                (end.x - settled.x).abs() < 0.5 && (end.y - settled.y).abs() < 0.5
                    && (end.w - settled.w).abs() < 0.5 && (end.h - settled.h).abs() < 0.5,
                "{kind:?}: the settled panel is the frame {settled:?}, drew {end:?}"
            );
        }
    }

    /// **The panel is never narrower than the control's pill** (the Watchlist's menu hangs by the
    /// same rule, `TableView::menu_panel_width_from`): a wide control, here 400 px, gets a panel as
    /// wide as its focused pill, and the right edge still ends on that pill's outer edge.
    #[test]
    fn the_sort_filter_panel_is_never_narrower_than_its_controls_pill() {
        let _guard = plx_base::testlock::serial();
        let arg = LibraryMenuArg {
            host: plx_machine::machine::InstanceId(8),
            target: SectionAddress { epoch: 11, sid: ServerId::from_raw(1), section: 7 },
            kind: LibraryMenuKind::SortFilter,
            anchor: [1371.0f32.to_bits(), 148.0f32.to_bits(), 400.0f32.to_bits(), 56.0f32.to_bits()],
            folded: Vec::new(),
            opener: Some((EntryId(3), 5)),
        };
        let mut menu = LibraryMenu::new(EntryId(7), arg);
        menu.apply_draft(filter_draft(false, None, true));
        let frame = menu.frame(&FixtureMeasure);
        let pill = 400.0 + 2.0 * plx_ui::widgets::PILL_FOCUS_OUTSET;
        assert!(frame.w >= pill, "the panel {} is narrower than its control's pill {pill}", frame.w);
        let pill_edge = 1371.0 + 400.0 + plx_ui::widgets::PILL_FOCUS_OUTSET;
        assert!((frame.x + frame.w - pill_edge).abs() < 0.01, "right edge {} is the opener's edge {pill_edge}", frame.x + frame.w);
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

    /// **The one Sort & Filter popover's sections** (redesign spec, "Header"): Show appears only when
    /// the library offers more than one type besides Collections (TV: Shows / Seasons / Episodes),
    /// and never lists Collections itself; Sort and Filter always follow.
    #[test]
    fn sort_and_filter_menu_sections_follow_the_library_kind() {
        let sorts = vec![SortEntry { key: "titleSort".into(), desc_key: String::new(), title: "Title".into(), default_desc: false }];
        let titles = |draft: MenuDraft| -> Vec<String> { lay(draft).sections.iter().map(|s| s.header.clone()).collect() };
        let show = titles(sort_filter_draft(SecKind::Show, LibraryType::Primary, &sorts, 0, false, None, false));
        let movie = titles(sort_filter_draft(SecKind::Movie, LibraryType::Primary, &sorts, 0, false, None, false));
        assert_eq!(show.len(), 3, "TV libraries open Show, Sort and Filter");
        assert_eq!(show[0], plx_platform::i18n::msg::library_header_show());
        assert_eq!(movie.len(), 2, "a Movies library has one type to show, so no Show section");
        assert_eq!(movie[0], plx_platform::i18n::msg::browse_library_sort());
        let laid = lay(sort_filter_draft(SecKind::Show, LibraryType::Primary, &sorts, 0, false, None, false));
        assert!(!laid.rows.iter().any(|(id, ..)| id == &format!("type:{}", LibraryType::Collections.code())), "Collections has its own control");
        assert_ne!(sort_filter_draft(SecKind::Show, LibraryType::Primary, &sorts, 0, false, None, false).stamp,
            sort_filter_draft(SecKind::Show, LibraryType::Primary, &sorts, 0, false, None, true).stamp);
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
                ("sort & filter show", lay(sort_filter_draft(SecKind::Show, LibraryType::Primary, &sorts, 1, false, Some(&genres[0]), true))),
                ("sort & filter movie", lay(sort_filter_draft(SecKind::Movie, LibraryType::Primary, &sorts, 0, true, None, false))),
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
