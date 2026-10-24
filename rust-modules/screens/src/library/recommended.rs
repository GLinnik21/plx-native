//! **The Recommended page** — this library's server hubs (`GET /hubs/sections/{id}`) stacked down
//! one vertical document, in the order the server's admin arranged them, pushed over the library
//! grid by the header's Recommended control (`AppArg::LibraryRecommended`).
//!
//! The store drops an empty hub before it publishes (`browse::section_hubs`, §3a of
//! `docs/pms-api.md`), so a row here is always a hub with cards: the page's row `i` IS the store's
//! shelf `i`, and every `hubs.shelves().get(i)`, `HubHold` and `HubPage` below speaks of the same
//! index. A promoted collection hub keeps its linked heading, which opens that collection.
//!
//! The page owns what the library page used to keep for its hub shelves, moved here whole: the row
//! state ([`Row`]), the build from a publication, the per-row card sections and their paging,
//! the hold band sent to the store, the saved horizontal scrolls, and the heading-link press. It
//! copies, rather than borrows, the few helpers it shares by shape with the library page (the
//! pitch of a row, the card source, the content clip), so nothing here reaches into code the
//! library page no longer needs; the shelf group spec is the one exception, [`row_group`] is the
//! library page's own, so a change to it moves both pages.
//!
//! **Back** goes through the page's [`PageRegions`] like the other bar-wearing pages: a page
//! scrolled down first returns to its top, on the first shelf in the column focus was in; from the
//! top, from the crumb header or from the bar it is the page's own pop (`ContentReq::Back`), and
//! the return state the Recommended press captured puts focus back on the control that opened it
//! (the library page beneath keeps its own focus engine memory). The page never changes the
//! library.
use std::borrow::Cow;
use std::collections::HashSet;

use plx_data::browse::section_hubs::Shelf as HubShelf;
use plx_data::browse::SecKind;
use plx_data::stores::browse::{BrowseCmd, LibraryWork, SectionAddress};
use plx_data::stores::{StoreCmd, StoreId, StoreWork};
use plx_machine::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind,
    InstanceId, Key, LogicalState, Machine, MachineId,
};
use plx_machine::present::Provenance;
use plx_ui::cards::{self as ui_cards, CardSource, RowStyle, SectionFrame, Tile};
use plx_ui::consts::{CARD_DY, CARD_H, K_SCROLL, MARGIN_X, MARGIN_Y, SCR_H, SCR_W, TITLE_DY, UNDER_LABEL_AIR};
use plx_ui::frame::Budget;
use plx_ui::screen::{
    At, DrawFrame, ElemKind, Enter, FocusTarget, Focusable,
    GroupSpec, Link, Placed, RenderStrategy, Screen, ScreenEvent, Step, Dir, By,
};
use plx_ui::page_regions::{Arrival, BackStep, PageRegions};
use plx_ui::page_header::{HeaderFocus, HeaderRow, PageHeader, PageHeaderSpec, Slot};
use plx_ui::breadcrumb::Crumb;
use plx_ui::widgets::Art;
use plx_ui::xfade::Xfade;
use plx_ui::{on_axis, Env, Rect, Spring, View};

use super::identity::{KeyRegion, KeyRegistry};
use super::row_group;
use crate::registry::{
    key_cap, tile_facts, AppFx, AppMsg, ContentArg, ContentReq, LibraryCmd, LibraryIdentity, LibraryLike,
    LibraryMemory, LibraryReq, LibrarySectionIdentity, LibraryViewport, PageMemory, RowSeats,
    KEY_PRUNE_BATCH,
};

/// Where the page's title stands: the same top the library's header row has.
const CONTENT_TOP: f32 = plx_ui::page_header::TOP;
/// The run's start under [`CONTENT_TOP`]: the first shelf's HEADING (drawn [`TITLE_DY`] above its
/// row's origin) starts on the grid's content line, y 244 (`page_header::BLOCK_H` under the header
/// row's top), clear of the title row's 148..204 and the air under it; the origin, and the cards
/// under it, follow the heading.
const HEAD_H: f32 = plx_ui::page_header::BLOCK_H + TITLE_DY;
/// Rows PageUp / PageDown move focus by.
const PAGE_ROWS: usize = 2;
/// The strip group the page's first row links up to (the shared top bar).
const STRIP: GroupId = plx_ui::containers::tabs::STRIP;

/// The crumb's root link ("Movies"): the page's one control above the rows, a fixed element and
/// group of its own (the same element the Collections page's crumb root has on the library page).
const CRUMB_ELEM: u32 = super::toolbar::CRUMB_ROOT;
const CRUMB_GROUP: GroupId = super::LIBRARY_GROUP;
/// The shelves' place before any row has landed: declared empty (`PageRegions::declare_pending`), so
/// a page whose hubs are still loading can hold no seat (`PageRegions::unseat`).
const SHELVES_GROUP: GroupId = GroupId(0x4c49_4214);

/// One hub's row: the published hub's identities and its `cards::Shelf`, which owns the pop, the
/// scroll, the caption band, the draw, the stops and the placement.
pub(super) struct Row {
    pub(super) id: String,
    pub(super) group: GroupId,
    /// A promoted collection hub's linked heading (`ui::linked_heading`): its one-element group
    /// and element, registered by the hub's identity like the row's own group. `None` for every
    /// other hub, which keeps its plain heading.
    pub(super) heading: Option<(GroupId, u32)>,
    pub(super) elems: Vec<u32>,
    pub(super) landscape: bool,
    /// The Continue Watching deck of poster cards: its band follows
    /// `registry::captions::deck_policy` (its progress line is always shown). Derived from the
    /// publication; not canon.
    deck: bool,
    pub(super) cards: ui_cards::Shelf,
    /// What the row's keys were registered from, to tell a publication that changed the row from
    /// one that did not ([`Basis`]). Derived; not canon.
    basis: Basis,
}

impl Row {
    fn heading_elem(&self) -> Option<u32> { self.heading.map(|(_, elem)| elem) }

    /// Is this the row `shelf` publishes: the same hub, drawn the same way?
    fn is(&self, shelf: &HubShelf) -> bool { self.id == shelf.id && self.landscape == shelf.landscape }
}

/// **What a row's keys were registered from**, kept so a publication can be told from the last by
/// what the row holds, not by how many rows the page has. A row's keys are a function of its hub's
/// id, its length (`Shelf::slots`: the window, or the placeholders of a row the store gave up), the
/// identity of each card, whether it has a linked heading, and, for a placeholder, the query of the
/// listing it was minted under. A row whose basis is the publication's is the row the publication
/// would build, so it is kept as it stands: no key is registered, no identity cloned or hashed.
///
/// Compared whole, not by the store's `Shelf::epoch`: the epoch moves when a page LANDS, but a row
/// given up to placeholders (`settle_from`) keeps it, and a fresh parse of a list carries epoch 0
/// for a row nobody had walked, so neither tells a changed row from a kept one. What does is the
/// row itself, and only the rows the store holds cards for (the hold band, 16 at most) have cards
/// to compare.
#[derive(Clone, Debug, Default)]
struct Basis {
    landscape: bool,
    linked: bool,
    slots: usize,
    /// Any col keyed as a placeholder: the keys then carry `query`.
    slot_form: bool,
    query: u32,
    /// The window's cards, each as the identity its key was minted from (an empty `rk` is a
    /// placeholder). Empty for a row the store gave its cards up to.
    items: Vec<(plx_plex::plex::ServerId, String)>,
}

impl Basis {
    fn of(shelf: &HubShelf, query: u32) -> Self {
        Self {
            landscape: shelf.landscape, linked: shelf.link.is_some(), slots: shelf.slots(),
            slot_form: shelf.items.is_empty() || shelf.items.iter().any(|item| item.rk.is_empty()),
            query,
            items: shelf.items.iter().map(|item| (item.sid, item.rk.clone())).collect(),
        }
    }

    fn matches(&self, shelf: &HubShelf, query: u32) -> bool {
        self.landscape == shelf.landscape && self.linked == shelf.link.is_some() && self.slots == shelf.slots()
            && (!self.slot_form || self.query == query)
            && self.items.len() == shelf.items.len()
            && self.items.iter().zip(&shelf.items).all(|((sid, rk), item)| item.sid == *sid && item.rk == *rk)
    }
}

/// Does every hub of this publication appear once? A hub twice shares its keys between its rows
/// (the store does not dedupe `(hubIdentifier, key)`), and a build that keeps one of them and
/// registers the other would leave the shared group remembering the wrong row: that publication is
/// built whole.
fn ids_distinct(shelves: &[HubShelf]) -> bool {
    let mut seen: HashSet<&str> = HashSet::with_capacity(shelves.len());
    shelves.iter().all(|shelf| seen.insert(shelf.id.as_str()))
}

/// What the page is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Readout {
    /// No publication committed yet (or no section yet): the spinner.
    Loading,
    /// The library has no recommended hubs.
    Empty,
    /// One or more rows.
    Rows,
}

/// The Recommended page: a vertical stack of the library's hub rows.
pub struct RecommendedScreen {
    pub(super) entry: EntryId,
    instance: InstanceId,
    kind: SecKind,
    keys: KeyRegistry,
    pub(super) shelves: Vec<Row>,
    section: Option<LibrarySectionIdentity>,
    epoch: Option<u32>,
    query: Option<u32>,
    shelf_publication: Option<(plx_data::browse::section_hubs::HubsId, u64)>,
    /// The rows stacked for the live pitches and for the pitches the springs are heading to.
    /// Derived from `shelves` and refilled by every `relayout`; not canon.
    pub(super) run: ShelfRun,
    target_run: ShelfRun,
    pub(super) scroll: Spring,
    pub(super) scroll_target: f32,
    restore_scroll: Option<f32>,
    viewports: Vec<LibraryViewport>,
    /// The first seat the page chooses for itself, until it has made it (or been restored).
    pub(super) initial: bool,
    readout: Readout,
    live: bool,
    ground: plx_ui::widgets::PageGround,
    ground_seeded: bool,
    /// **The shelves' arrival** (`plx_ui::xfade`, the cross-fade the Library's `page_fade` and
    /// Search's results are on): a page opened before its first publication has nothing on screen
    /// to dissolve, so it is mounted dark ([`Xfade::mount`]) and the rows that land fade in. A page
    /// whose hubs were already published never mounts it: its arrival is the route dip's. Draws
    /// around the shelves, their headings and the Empty status caption (never the title row, the
    /// ground, the spinner or the hold hint), and holds the shelves dormant while it arrives
    /// ([`Self::shelves_on`]).
    fade: Xfade,
    sweep_down: bool,
    /// The crumb row's pills (the focus capsule, the selection plate under "Recommended") and its
    /// focus pop: the shared `PageHeader` motion, held by the page that wears the row.
    header_capsules: plx_ui::widgets::TabStrip,
    header_pop: plx_ui::widgets::CtlPop<1>,
    /// The crumb row itself, held by the one rule every header row is held by
    /// (`plx_ui::page_header::SharedRow`): built when the library's name, the kind or the locale
    /// changes, not per question. Derived from the store's directory; not canon.
    header_row: plx_ui::page_header::SharedRow<HeaderRow>,
    /// The standing "Hold OK for options" hint over a card. Paint-only.
    hold_hint: plx_ui::hold_hint::HoldHint,
    /// The page was remounted over a memory (`restore`): its keys are the memory's, so no row can
    /// be taken to hold keys the registry has. The next build registers every row again.
    rows_stale: bool,
    /// The held range sent to the store and the section it was for: the command goes out only when
    /// the rows in view outgrow the band last sent (`ui_cards::HoldBand`), or the section changes.
    /// Derived from what is on screen; not canon.
    hold_sent: Option<(SectionAddress, ui_cards::HoldBand)>,
    /// Rows the document draw and the stop recorder looked at, for the windowing test.
    #[cfg(test)]
    pub(super) shelf_visits: std::cell::Cell<usize>,
}

// ---- geometry -------------------------------------------------------------------------------

/// The shelves stacked down the document: one pitch each, however many the server
/// published. `tops` holds the prefix sums (`tops[i]` is shelf `i`'s offset from the run's start,
/// `tops[len]` the run's height), accumulated left to right in the order a straight
/// `pitches[..i].iter().sum()` adds them, so every origin is bit-identical to that sum. Origin
/// lookups are O(1) and the visible window is two binary searches, so a page of hundreds of
/// shelves costs what its on-screen shelves cost. The page keeps two (live and spring target) and
/// refills both into their existing capacity on every relayout.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ShelfRun {
    tops: Vec<f32>,
    pitches: Vec<f32>,
}

impl ShelfRun {
    #[cfg(test)]
    pub(super) fn of(pitches: &[f32]) -> Self {
        let mut run = Self::default();
        run.set(pitches.iter().copied());
        run
    }

    /// Replace the run with these pitches, reusing the allocated capacity.
    pub(super) fn set(&mut self, pitches: impl Iterator<Item = f32>) {
        self.tops.clear();
        self.pitches.clear();
        let mut top = 0.0f32;
        for pitch in pitches {
            self.tops.push(top);
            self.pitches.push(pitch);
            top += pitch;
        }
        self.tops.push(top);
    }

    pub(super) fn len(&self) -> usize { self.pitches.len() }

    /// Offset of shelf `index` from the run's start; past the end, the run's height.
    pub(super) fn origin(&self, index: usize) -> f32 {
        self.tops.get(index).copied().unwrap_or_else(|| self.height())
    }

    pub(super) fn pitch(&self, index: usize) -> f32 {
        self.pitches.get(index).copied().unwrap_or(plx_ui::consts::ROW_PITCH)
    }

    pub(super) fn height(&self) -> f32 { self.tops.last().copied().unwrap_or(0.0) }

    /// The shelves whose band `[origin, origin + pitch)` meets `top..bottom` (run coordinates),
    /// widened by one pixel each side so the caller's exact on-screen test owns the edge.
    pub(super) fn window(&self, top: f32, bottom: f32) -> std::ops::Range<usize> {
        let n = self.len();
        if n == 0 { return 0..0; }
        let first = self.tops[1..].partition_point(|&end| end <= top - 1.0);
        let end = self.tops[..n].partition_point(|&origin| origin < bottom + 1.0);
        first..end.max(first)
    }
}

/// The height of a row of `landscape` cards whose caption band, `band_h` tall when it is open, is
/// `expanded` (0..1) open.
pub(super) fn shelf_pitch(landscape: bool, band_h: f32, expanded: f32) -> f32 {
    let art = if landscape { RowStyle::EPISODE.h } else { CARD_H };
    TITLE_DY + CARD_DY + art + ui_cards::under_band_for(band_h, expanded) + UNDER_LABEL_AIR
}

fn hub_style(landscape: bool) -> &'static RowStyle { if landscape { &RowStyle::EPISODE } else { &RowStyle::HOME } }

fn row_style(row: &Row) -> &'static RowStyle { hub_style(row.landscape) }

/// A hub row's card section. Cards a screen's width off to either side still paint: a popped
/// card's shadow reaches onto the screen from beyond its edge.
fn hub_cards(entry: EntryId, landscape: bool) -> ui_cards::Shelf {
    ui_cards::Shelf::new(entry, hub_style(landscape)).cull_margin(32.0)
}

/// The panel the rows' cards are placed and hit-tested in: everything under the top bar.
fn content_clip() -> Rect {
    Rect::new(0.0, plx_ui::widgets::TOP_BAR_BOTTOM, SCR_W, SCR_H - plx_ui::widgets::TOP_BAR_BOTTOM)
}

fn row_on_screen(origin: f32, pitch: f32) -> bool {
    on_axis(origin - TITLE_DY, pitch, SCR_H, 0.0)
}

/// Screen y of the run's start at `scroll`.
fn run_top(scroll: f32) -> f32 { CONTENT_TOP + HEAD_H - scroll }

/// Screen y of row `index`'s origin (its card tops sit [`CARD_DY`] below it).
fn shelf_y(run: &ShelfRun, index: usize, scroll: f32) -> f32 { run_top(scroll) + run.origin(index) }

/// The furthest the document scrolls: its end sits at the bottom margin.
fn max_scroll(run: &ShelfRun) -> f32 {
    (HEAD_H + run.height() - (SCR_H - CONTENT_TOP) + MARGIN_Y).max(0.0)
}

/// The scroll that puts row `index` where the first row rests (heading under the title).
fn reveal(run: &ShelfRun, index: usize) -> f32 { run.origin(index).clamp(0.0, max_scroll(run)) }

/// The row focus stands on: the row that holds the focus key now, else the row the key was last
/// placed in (`last_place`, the key registry's record). The second half is what keeps a row focused
/// across the publication that re-keys it, slot keys to item keys or back (see `send_hold`): the
/// hold this page sends changes the very keys it would otherwise read, and a row that read as
/// unfocused for that frame would be released again at once.
pub(super) fn focused_row(rows: &[Row], elem: u32, last_place: Option<(GroupId, usize)>) -> Option<usize> {
    rows.iter().position(|row| row.elems.contains(&elem))
        .or_else(|| last_place.and_then(|(group, _)| rows.iter().position(|row| row.group == group)))
}

/// Is the row run final? `Fetching` (the first answer is still inside its bounded first-paint
/// window) and `Staged` (an answer held until the page may move) both mean the rows are about to
/// change. A publication with no identity has no section behind it and nothing to wait for.
fn settled(hubs: plx_data::stores::browse::HubsView<'_>) -> bool {
    hubs.id().is_none() || hubs.publication() == plx_data::browse::section_hubs::Publication::Committed
}

// ---- construction, sync -----------------------------------------------------------------------

impl RecommendedScreen {
    pub fn new(entry: EntryId, instance: InstanceId, kind: SecKind) -> Self {
        Self {
            entry, instance, kind, keys: KeyRegistry::default(), shelves: Vec::new(),
            section: None, epoch: None, query: None, shelf_publication: None,
            run: ShelfRun::default(), target_run: ShelfRun::default(),
            scroll: Spring::at(0.0), scroll_target: 0.0, restore_scroll: None, viewports: Vec::new(),
            initial: true, readout: Readout::Loading, live: true,
            ground: plx_ui::widgets::PageGround::new(), ground_seeded: false, fade: Xfade::new(), sweep_down: true,
            header_capsules: plx_ui::widgets::TabStrip::new(), header_pop: plx_ui::widgets::CtlPop::new(),
            header_row: plx_ui::page_header::SharedRow::new(),
            hold_hint: plx_ui::hold_hint::HoldHint::new(),
            hold_sent: None, rows_stale: false,
            #[cfg(test)] shelf_visits: std::cell::Cell::new(0),
        }
    }

    /// The canon shape of the page (`LogicalState::write`).
    pub const SHAPE: &'static str = concat!(
        "RecommendedScreen{entry:u32,instance:u32,kind:u32,scroll:{pos:f32,vel:f32},scroll_target:f32,",
        "restore_scroll:Option<f32>,live:bool,initial:bool,sweep_down:bool,epoch:Option<u32>,query:Option<u32>,",
        "shelf_publication:Option<(HubsId{epoch:u32,sid:u32,section:u64},revision:u64)>,ground_seeded:bool,",
        "fade:Xfade{phase:u8,t:f32},ground:PageGround,header_capsules:",
        "TabStrip{sel:Capsule{x:Spring{pos:f32,vel:f32},w:Spring{pos:f32,vel:f32},a:Spring{pos:f32,vel:f32},at:u32},foc:Capsule{x:Spring{pos:f32,vel:f32},w:Spring{pos:f32,vel:f32},a:Spring{pos:f32,vel:f32},at:u32}}",
        ",header_pop:{sp:[Spring{pos:f32,vel:f32};1],focused:Option<u32>},memory:PageMemory::Library,viewports:[LibraryViewport],",
        "shelves:[{id:str,group:u32,heading:Option<(group:u32,elem:u32)>,landscape:bool,elems:[u32],motion:ShelfMotion}],",
        "readout:u32,run:f32}");

    /// Remount from the memory a page pushed over this one (or this page's own earlier body) left.
    pub fn restore(&mut self, memory: &LibraryMemory) {
        self.keys = KeyRegistry::restore(memory);
        self.section = memory.section.clone();
        self.epoch = memory.epoch;
        self.query = memory.query;
        self.viewports = memory.viewports.clone();
        self.restore_scroll = Some(memory.scroll);
        self.scroll.jump(memory.scroll);
        self.scroll_target = memory.scroll;
        self.initial = false;
        self.rows_stale = true;
    }

    fn key(&self, elem: u32) -> FocusKey<u32> { FocusKey { entry: self.entry, elem } }

    fn reseat<H: LibraryLike>(&self, focus: FocusTarget<u32>, fx: &mut Effects<'_, H>) {
        fx.push(Fx::Deliver(MachineId::Instance(self.instance),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))));
    }

    fn address<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<SectionAddress> {
        let id = H::listing(cx).id()?;
        Some(SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section })
    }

    fn store<H: LibraryLike>(&self, target: SectionAddress, work: LibraryWork, fx: &mut Effects<'_, H>) {
        fx.push(Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed { target, work }))));
    }

    /// Follow the store: the section on view, and the publication of its hubs.
    pub(super) fn sync<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) {
        let listing = H::listing(cx);
        let directory = H::directory(cx);
        let identity = listing.id().map(|id| LibrarySectionIdentity { sid: id.sid, key: id.section });
        let epoch = listing.id().map(|id| id.epoch).or(directory.epoch());
        if self.section != identity || self.epoch != epoch {
            if let Some(viewport) = self.current_viewport() {
                self.viewports.retain(|old| old.epoch != viewport.epoch || old.section != viewport.section);
                self.viewports.push(viewport);
            }
            // Viewports of an epoch that is no longer current can never be looked up again.
            if self.epoch != epoch { self.viewports.retain(|old| Some(old.epoch) == epoch); }
            self.shelves.clear();
            self.shelf_publication = None;
            self.section = identity.clone();
            self.epoch = epoch;
            let saved = self.viewports.iter().find(|view| Some(view.epoch) == epoch && Some(&view.section) == identity.as_ref());
            self.restore_scroll = saved.map(|view| view.scroll);
            self.scroll.jump(self.restore_scroll.unwrap_or(0.0));
            self.scroll_target = self.scroll.pos;
        }
        self.query = listing.id().map(|id| id.query);
        if let Some(current) = directory.current() { self.kind = directory.sections()[current].kind; }
        let hubs = H::section_hubs(cx);
        let publication = hubs.id().zip(hubs.revision());
        if self.shelf_publication != publication || self.rows_stale {
            self.rebuild(cx, identity.as_ref(), epoch);
            self.shelf_publication = publication;
        }
        self.readout = if self.section.is_none() || !settled(hubs) { Readout::Loading }
            else if self.shelves.is_empty() { Readout::Empty } else { Readout::Rows };
        // A page with no row yet has nothing on screen to dissolve: park the shelves dark and let
        // them fade in once the first publication lands (`Xfade::tick` in the page's tick).
        if self.readout == Readout::Loading && self.shelves.is_empty() && !self.fade.is_swapping() { self.fade.mount(); }
        self.relayout(cx.focus.current);
    }

    /// Build the rows from the store's publication: keys, groups, linked headings and card
    /// sections, keeping the card section (scroll, pop, ledgers) of a row that survives.
    ///
    /// **The cost follows the change, not the page.** The store publishes once per landing, which on
    /// a long page walked quickly is one per key press, and a row whose [`Basis`] is the
    /// publication's is kept as it stands (the matching is the registry's [`RowSeats`], Home's
    /// too): a landing on one of 170 rows registers that row's slots and no other's. A row that
    /// moved (a hub inserted above it) is kept and only seated again, its group and heading, whose
    /// keys remember the row's index. The keys a changed row gave up are not swept one publication
    /// at a time (closing the gap of a key moves every key behind it, in the table and in both
    /// indexes) but when the region stands a batch over its bound.
    fn rebuild<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, identity: Option<&LibrarySectionIdentity>, epoch: Option<u32>) {
        let shelves = H::section_hubs(cx).shelves();
        let query = self.query.unwrap_or(0);
        let incremental = !std::mem::take(&mut self.rows_stale) && ids_distinct(shelves);
        let before = self.cards_held();
        let mut seats: RowSeats<Row, (String, bool)> = RowSeats::new(std::mem::take(&mut self.shelves), shelves.len());
        // What changed rows gave up: the previous publication's keys that this one may not hold.
        let mut departed: Vec<u32> = Vec::new();
        let mut rows: Vec<Row> = Vec::with_capacity(shelves.len());
        if let Some(section) = identity {
            for (index, shelf) in shelves.iter().enumerate() {
                let same_place = seats.at(index).is_some_and(|old| old.is(shelf));
                let looked = (!same_place).then(|| (shelf.id.clone(), shelf.landscape));
                let from = seats.locate(index, same_place, looked.as_ref(), |old| (old.id.clone(), old.landscape));
                if let Some(at) = from.filter(|&at| incremental && seats.at(at).is_some_and(|old| old.basis.matches(shelf, query))) {
                    let mut row = seats.keep(at);
                    if at != index { self.reseat_row(&mut row, section, shelf, index); }
                    rows.push(row);
                    continue;
                }
                seats.rebuilt(from);
                let cards = from.and_then(|at| seats.take(at)).map(|old| { departed.extend(old.elems); old.cards });
                rows.push(self.build_row(section, shelf, index, epoch, query, cards));
            }
        }
        for (_, old) in seats.finish().unclaimed { departed.extend(old.elems); }
        self.shelves = rows;
        // A publication answers an ask a row's latch may be holding, kept row or built: the store
        // refuses an ask computed from a window the publication replaced, and nothing else would
        // send it again before the ladder's next rung. A row's own landing is seen by its card
        // section (`observe_landing`); for a kept row this landing is another row's.
        for row in &mut self.shelves { row.cards.reset_page_requests(); }
        // Rotating refetches mint a key per new item: keep the current rows, the previous
        // publication's (a focus on one recovers through it) and the focus; above twice that
        // plus 256, and a batch more, the oldest others go.
        let live = self.cards_held();
        let cap = key_cap(live + before);
        if self.keys.count(KeyRegion::Shelf) > cap + KEY_PRUNE_BATCH {
            let mut keep: HashSet<u32> = HashSet::with_capacity(live + departed.len() + 1);
            keep.extend(self.shelves.iter().flat_map(|row| row.elems.iter().copied()));
            keep.extend(departed);
            keep.extend(cx.focus.current.filter(|key| key.entry == self.entry).map(|key| key.elem));
            self.keys.prune(KeyRegion::Shelf, cap, |elem| keep.contains(&elem));
        }
    }

    /// How many card keys the rows hold between them.
    fn cards_held(&self) -> usize { self.shelves.iter().map(|row| row.elems.len()).sum() }

    /// The row for `shelf` at `index`: its group, its keys and its linked heading registered, over
    /// the card section of the row it was (`cards`) or a new one at the scroll the page last left it.
    fn build_row(&mut self, section: &LibrarySectionIdentity, shelf: &HubShelf, index: usize, epoch: Option<u32>,
        query: u32, cards: Option<ui_cards::Shelf>) -> Row {
        let group = self.seat_group(section, shelf, index);
        // A row the store gave its cards up to is its card count in placeholder slots, so it
        // keeps its length, its scroll and its focusability.
        let elems = (0..shelf.slots()).map(|col| {
            let identity = match shelf.items.get(col).filter(|item| !item.rk.is_empty()) {
                Some(item) => LibraryIdentity::Shelf { section: section.clone(), hub: shelf.id.clone(), sid: item.sid, rk: item.rk.clone() },
                None => LibraryIdentity::ShelfSlot { section: section.clone(), hub: shelf.id.clone(), publication: query, slot: col as u32 },
            };
            self.keys.register(identity, group, col)
        }).collect();
        let cards = cards.unwrap_or_else(|| {
            let mut cards = hub_cards(self.entry, shelf.landscape);
            let saved = self.viewports.iter().find(|view| Some(view.epoch) == epoch && view.section == *section)
                .and_then(|view| view.shelves.iter().find(|(id, _)| *id == shelf.id));
            if let Some((_, x)) = saved { cards.restore_scroll(*x, shelf.slots()); }
            cards
        });
        let heading = self.seat_heading(section, shelf, index);
        Row { id: shelf.id.clone(), group, heading, elems, landscape: shelf.landscape, deck: shelf.is_continue && !shelf.landscape, cards, basis: Basis::of(shelf, query) }
    }

    /// A kept row that stands at another index now: its group and heading keys remember the index
    /// (`KeyRegistry::last_place`), and nothing else about it moved.
    fn reseat_row(&mut self, row: &mut Row, section: &LibrarySectionIdentity, shelf: &HubShelf, index: usize) {
        row.group = self.seat_group(section, shelf, index);
        row.heading = self.seat_heading(section, shelf, index);
    }

    fn seat_group(&mut self, section: &LibrarySectionIdentity, shelf: &HubShelf, index: usize) -> GroupId {
        GroupId(self.keys.register(
            LibraryIdentity::Control { section: section.clone(), kind: "shelf-group".into(), key: shelf.id.clone() },
            GroupId(0), index))
    }

    fn seat_heading(&mut self, section: &LibrarySectionIdentity, shelf: &HubShelf, index: usize) -> Option<(GroupId, u32)> {
        shelf.link.as_ref().map(|_| {
            let control = |kind: &str| LibraryIdentity::Control {
                section: section.clone(), kind: kind.into(), key: shelf.id.clone() };
            let group = GroupId(self.keys.register(control("shelf-heading-group"), GroupId(0), index));
            let elem = self.keys.register(control("shelf-heading"), group, 0);
            (group, elem)
        })
    }

    fn relayout(&mut self, focus: Option<FocusKey<u32>>) {
        // The poster rows follow the Under posters setting; a landscape row (an episode still) is
        // not a poster and keeps its own two-line block and band.
        let policy = crate::registry::captions::policy();
        for row in &mut self.shelves {
            let policy = if row.deck { crate::registry::captions::deck_policy(&policy) } else { policy };
            row.cards.set_caption_policy(Some(policy).filter(|_| !row.landscape));
        }
        self.run.set(self.shelves.iter().map(|row| shelf_pitch(row.landscape, row.cards.band_h(), row.cards.band_expand())));
        self.target_run.set(self.shelves.iter().map(|row| shelf_pitch(row.landscape, row.cards.band_h(),
            f32::from(focus.is_some_and(|key| row.elems.contains(&key.elem))))));
        self.scroll_target = self.scroll_target.clamp(0.0, max_scroll(&self.target_run));
    }

    /// **This page's column** (`plx_ui::page_regions`): the crumb's root as the header, then one
    /// continuous document, a linked heading being its own block directly above its cards, so Up
    /// from a card reaches the heading and Down returns to the remembered card. The page seats its
    /// first row on arrival and BACK pops to the Library; it does not walk the header.
    fn with_regions<R>(&self, then: impl FnOnce(&PageRegions<'_>) -> R) -> R {
        let mut content = Vec::with_capacity(2 * self.shelves.len());
        for row in &self.shelves {
            content.extend(row.heading.map(|(heading, _)| heading));
            content.push(row.group);
        }
        then(&PageRegions {
            header: Some(CRUMB_GROUP),
            content: &content,
            rail: None,
            back_walks_header: false,
            arrival: Arrival::Content,
            // No publication yet: the page seats nothing until its first row lands (or it learns
            // there are none), not the crumb whose OK pops the page.
            pending: (self.readout == Readout::Loading).then_some(SHELVES_GROUP),
        })
    }

    /// The group the page seats on its first arrival: the first row (its heading, when it has one);
    /// `None` while the hubs are still loading (`PageRegions::first_seat`).
    fn first_group(&self) -> Option<GroupId> {
        self.with_regions(|regions| regions.first_seat())
    }

    /// Seat `first_group`, or hold the page unseated while its hubs are loading.
    fn reseat_first<H: LibraryLike>(&self, fx: &mut Effects<'_, H>) {
        if let Some(target) = self.with_regions(|regions| regions.first_seat().map(FocusTarget::ContainerGroup).or_else(|| regions.unseat())) {
            self.reseat(target, fx);
        }
    }

    /// **BACK**, decided by [`PageRegions::back`] and executed here: a scrolled page first returns
    /// to its top, on the first row in the column focus was in; from there it pops to the Library
    /// page that opened it, and the return state captured at that press seats focus on the control
    /// again. This page walks neither the header nor the bar, so those steps are only the answers a
    /// page declaring `back_walks_header` would get.
    fn back<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let focus = cx.focus.current.filter(|key| key.entry == self.entry);
        let step = self.with_regions(|regions| {
            let at = focus.and_then(|key| regions.region_of(key.elem, <Self as Focusable<H>>::group_of(self, &key.elem, cx)));
            regions.back(at, self.scroll_target > 0.5)
        });
        match step {
            BackStep::Top => {
                self.scroll_target = 0.0;
                let column = focus.and_then(|key| self.shelves.iter().find_map(|row| row.elems.iter().position(|elem| *elem == key.elem)))
                    .unwrap_or(0);
                let seat = self.shelves.first().and_then(|row| row.elems.get(column.min(row.elems.len().saturating_sub(1))));
                match seat {
                    Some(&elem) => self.reseat(FocusTarget::Elem(self.key(elem)), fx),
                    None => self.reseat_first(fx),
                }
            }
            BackStep::Content => self.reseat_first(fx),
            BackStep::Header => self.reseat(FocusTarget::ContainerGroup(CRUMB_GROUP), fx),
            BackStep::Nav => self.reseat(FocusTarget::ContainerGroup(STRIP), fx),
            BackStep::Leave => fx.push(Fx::App(AppFx::Content(ContentReq::Back))),
        }
        Handled::Yes
    }

    fn current_viewport(&self) -> Option<LibraryViewport> {
        let shelves = if self.shelf_publication.is_some() {
            self.shelves.iter().map(|row| (row.id.clone(), row.cards.scroll())).collect()
        } else {
            self.viewports.iter().find(|view| Some(view.epoch) == self.epoch && Some(&view.section) == self.section.as_ref())
                .map(|view| view.shelves.clone()).unwrap_or_default()
        };
        Some(LibraryViewport { epoch: self.epoch?, section: self.section.clone()?, scroll: self.scroll.pos, shelves })
    }

    fn page_memory(&self) -> LibraryMemory {
        let mut memory = self.keys.remember(self.section.clone(), self.scroll.pos,
            self.shelves.iter().map(|row| (row.id.clone(), row.cards.scroll())).collect());
        memory.epoch = self.epoch;
        memory.query = self.query;
        memory.viewports = self.viewports.clone();
        if let Some(viewport) = self.current_viewport() {
            memory.viewports.retain(|old| old.epoch != viewport.epoch || old.section != viewport.section);
            memory.viewports.push(viewport);
        }
        memory
    }

    // ---- reads ------------------------------------------------------------------------------

    /// The card focus stands on, if it is a published item.
    pub fn focused_item<'a, H: LibraryLike>(&self, focus: Option<FocusKey<u32>>, cx: &Cx<'a, H>) -> Option<&'a plx_data::pms::PmsMovie> {
        let key = focus.filter(|key| key.entry == self.entry)?;
        let (row, col) = self.cell_of(key.elem)?;
        H::section_hubs(cx).shelves().get(row)?.items.get(col)
    }

    /// `(row, col)` of the card `elem` names.
    fn cell_of(&self, elem: u32) -> Option<(usize, usize)> {
        self.shelves.iter().enumerate()
            .find_map(|(row, shelf)| shelf.elems.iter().position(|e| *e == elem).map(|col| (row, col)))
    }

    /// The focused card, `(row, col)`, if focus is on a row.
    pub fn shelf_position(&self, focus: Option<FocusKey<u32>>) -> Option<(usize, usize)> {
        let key = focus.filter(|key| key.entry == self.entry)?;
        self.cell_of(key.elem)
    }

    /// `(region, row, col, viewport x, viewport y)` for the focus probe line. The grammar is the
    /// library page's (`region=shelf row= col=`), so a scene that reads it needs no change.
    pub fn probe_viewport(&self, focus: Option<FocusKey<u32>>) -> (&'static str, i32, i32, f32, f32) {
        let elem = focus.filter(|key| key.entry == self.entry).map(|key| key.elem);
        for (row, shelf) in self.shelves.iter().enumerate() {
            if let Some(col) = elem.and_then(|elem| shelf.elems.iter().position(|key| *key == elem)) {
                return ("shelf", row as i32, col as i32, shelf.cards.scroll(), self.scroll.pos);
            }
        }
        let region = match elem {
            Some(elem) if self.shelves.iter().any(|row| row.heading_elem() == Some(elem)) => "heading",
            Some(CRUMB_ELEM) => "crumb",
            Some(elem) if elem >= plx_ui::dispatch::STRIP_BASE => "strip",
            Some(_) => "library",
            None => "none",
        };
        (region, -1, -1, 0.0, self.scroll.pos)
    }

    /// The scroll the page is travelling to: 0 at its top.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_scroll_target(&self) -> f32 { self.scroll_target }

    /// How many header rows this page has built: the build counter of its `RowCache`/`SharedRow`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_header_builds(&self) -> u32 { self.header_row.builds() }

    /// The header row's group and its stops (the crumb root alone), as the library page's
    /// `toolbar_group` / `probe_header_stops` answer for theirs.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_header(&self) -> (GroupId, Vec<(&'static str, u32)>) { (CRUMB_GROUP, vec![("crumb", CRUMB_ELEM)]) }

    /// Every card's pop as the draw paints it, shelf by shelf: the page's answer to `HomeScreen::probe_card_pops`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_card_pops(&self, focus: Option<FocusKey<u32>>) -> Vec<Vec<f32>> {
        let at = self.shelf_position(focus);
        self.shelves.iter().enumerate().map(|(row, shelf)| {
            let col = at.filter(|&(r, _)| r == row).map(|(_, c)| c);
            (0..shelf.elems.len()).map(|i| shelf.cards.drawn_pop(i, col)).collect()
        }).collect()
    }

    /// `(shelves' alpha, focused card's pop scale)` — what a viewer sees of a focus arrival. The
    /// alpha is the shelves' dissolve (1 unless the page opened before its first publication; the
    /// route dip's own fade is the container's), the same answer the library page gives.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_arrival(&self, focus: Option<FocusKey<u32>>) -> (f32, Option<f32>) {
        let elem = focus.filter(|key| key.entry == self.entry).map(|key| key.elem);
        let scale = self.shelves.iter().find_map(|shelf| {
            let col = elem.and_then(|elem| shelf.elems.iter().position(|key| *key == elem))?;
            Some(shelf.cards.drawn_pop(col, Some(col)))
        });
        (self.fade.alpha(), scale)
    }

    fn from_deck<H: LibraryLike>(&self, elem: u32, cx: &Cx<'_, H>) -> bool {
        self.shelves.iter().position(|row| row.elems.contains(&elem))
            .and_then(|row| H::section_hubs(cx).shelves().get(row)).is_some_and(|row| row.is_continue)
    }

    /// The two screen-specific answers the hold hint needs (`ui::hold_hint`, "Adopting it"): the
    /// CARD focus rests on, one whose hold opens an item menu, and whether it has arrived.
    fn hint_input<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> plx_ui::hold_hint::HintInput {
        let none = plx_ui::hold_hint::HintInput::default();
        if !self.hold_hint.wants_input(cx.press.held_ms.is_some()) { return none; }
        let Some(key) = cx.focus.current.filter(|key| key.entry == self.entry) else { return none };
        if self.focused_item(Some(key), cx).is_none_or(|item| !crate::registry::item_has_menu(item)) { return none; }
        let Some(drawn) = <Self as Focusable<H>>::place(self, &key.elem, cx, At::Drawn) else { return none };
        let still = (self.scroll.pos - self.scroll_target).abs() < 0.5 && self.scroll.vel.abs() < 0.5;
        plx_ui::hold_hint::HintInput::for_placed(key.elem, drawn, still, cx.press.held_ms)
    }

    // ---- events -----------------------------------------------------------------------------

    /// Feed an event to every row's card section: focus moves and the tick that steps them. The
    /// page keeps its own press, hold and paging handling, so the sections' `Activate` / `Hold` /
    /// `Want` are not used: `CardSource::more` is `false`.
    ///
    /// **The shelves are held at rest while the page is not yet seen** (`Shelf::dormant`): under
    /// the route dip's image (`page_image_held`) or while the shelves dissolve in (the page's
    /// `fade`). Under the image a card that was lifted (a page returning from a Detail it froze
    /// mid-ease) is put at rest on the first tick, as the Library's grid does, because an ease
    /// stepped under the image would hold it past the dissolve; while the shelves dissolve in the
    /// page is in view and the let-go eases like an unfocused shelf's. The seat the page makes for
    /// itself then grows from rest the frame the page can be seen, on the standard lift spring, and
    /// the caption band it opens and the heading it lifts play live, instead of landing whole where
    /// an image hides them and keeps the dip's picture standing for half a second.
    fn shelves_on<H: LibraryLike>(&mut self, ev: &ScreenEvent<H>, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let hubs = H::section_hubs(cx);
        let unseen = self.fade.is_arriving() || plx_ui::containers::transition::page_image_held();
        let focus = cx.focus.current.filter(|key| key.entry == self.entry).map(|key| key.elem);
        let ticking = matches!(ev, ScreenEvent::Tick(_));
        let mut asks = Vec::new();
        let mut withdrawn = Vec::new();
        let seen = hubs.revision().unwrap_or_default();
        for (index, row) in self.shelves.iter_mut().enumerate() {
            let shelf = hubs.shelves().get(index);
            let at = focus.and_then(|elem| row.elems.iter().position(|key| *key == elem));
            let paging = at.is_some_and(|col| col.saturating_add(ui_cards::PAGE_EDGE_CARDS) >= row.elems.len());
            let src = HubSrc { elems: &row.elems, shelf, paging };
            row.cards.dormant(unseen);
            let want = row.cards.on(ev, cx, &src, fx);
            if !ticking && at.is_none() { row.cards.reset_page_requests(); }
            // An ask whose edge focus has left is withdrawn before the landing can slide the window
            // from under it.
            if ticking {
                if let (Some(shelf), Some(_)) = (shelf, row.cards.page_cancel(cx, &src, shelf.map_or(0, |s| s.offset), true)) {
                    withdrawn.push((shelf.id.clone(), shelf.key.clone()));
                }
            }
            // The row asks for its next page at an edge, as Home's rows do; the shelf repeats an
            // unanswered ask on its ladder (the store takes the same ask idempotently) and
            // `cancel_page` drops it when focus leaves. The store moves the window and the page
            // follows the card focus is on.
            if ticking && at.is_some() {
                let wanted = matches!(want, Some(ui_cards::CardEvent::Want(_)));
                if let (Some(shelf), Some(edge)) = (shelf, row.cards.page_ask(cx, &src, wanted, shelf.map_or(0, |s| s.offset), true)) {
                    asks.push((shelf.id.clone(), shelf.key.clone(), edge == ui_cards::PageEdge::Before));
                }
            }
        }
        if let Some(target) = self.address(cx).filter(|_| !asks.is_empty() || !withdrawn.is_empty()) {
            for (id, key) in withdrawn { self.store(target, LibraryWork::HubPageCancel { id, key }, fx); }
            for (id, key, before) in asks { self.store(target, LibraryWork::HubPage { id, key, before, seen }, fx); }
        }
    }

    /// The rows with a card on screen, plus two each side and the focused row, as the range the
    /// store holds cards for. Sent only when it changes.
    ///
    /// The focused row is found by the focus key's PLACE ([`focused_row`]), not only by looking
    /// for the key among the row's current ones: a row the store gave its cards up to is keyed by
    /// slot, and the same row holding its cards is keyed by item, so the hold this function sends
    /// changes the very keys it would otherwise read. BACK from deep in the page seats focus on a
    /// row far off screen whose cards are released; the row is held because it is focused, its
    /// cards come back under their item keys, the focus key (still the slot's, until the engine
    /// reconciles it) is then in no row, the row reads as unfocused and is released, and the engine
    /// reconciles focus onto a key that has just gone again. Measured on the frame dump: focus
    /// changed key on every loop iteration, and the film's frames depended on how many iterations
    /// a frame took.
    fn send_hold<H: LibraryLike>(&mut self, target: SectionAddress, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        if self.shelves.is_empty() { return; }
        let origin = run_top(self.scroll.pos);
        let seen = self.run.window(-origin, SCR_H - origin);
        let focused = cx.focus.current.filter(|key| key.entry == self.entry)
            .and_then(|key| focused_row(&self.shelves, key.elem, self.keys.last_place(key.elem)));
        let (mut lo, mut hi) = (seen.start, seen.end.saturating_sub(1).max(seen.start));
        if let Some(row) = focused { (lo, hi) = (lo.min(row), hi.max(row)); }
        let (sent_for, band) = self.hold_sent.get_or_insert((target, ui_cards::HoldBand::default()));
        if *sent_for != target {
            *sent_for = target;
            band.forget();
        }
        if let Some(hold) = band.next((lo, hi), self.shelves.len()) {
            self.store(target, LibraryWork::HubHold { lo: hold.0, hi: hold.1 }, fx);
        }
    }

    /// Bring a focused card (or heading) into view: the row's reveal scroll.
    fn reveal_focus<H: LibraryLike>(&mut self, key: FocusKey<u32>, by: By, cx: &Cx<'_, H>) {
        self.relayout(Some(key));
        let want = self.shelves.iter().position(|row| row.elems.contains(&key.elem) || row.heading_elem() == Some(key.elem))
            .map(|index| reveal(&self.target_run, index));
        if let Some(want) = want {
            self.scroll_target = want;
            // Only a SAVED viewport is jumped to. A `Restore` that carries none is a reseat the
            // page asked of itself.
            if by == By::Restore {
                if let Some(saved) = self.restore_scroll.take() {
                    self.scroll_target = saved.clamp(0.0, max_scroll(&self.target_run));
                    self.scroll.jump(self.scroll_target);
                }
            }
        } else if let Some(top) = self.header(cx).focus(self.entry, CRUMB_GROUP, (self.scroll.pos, self.scroll_target), content_clip()).reveal(key.elem) {
            self.scroll_target = top;
        } else if key.elem >= plx_ui::dispatch::STRIP_BASE {
            self.scroll_target = 0.0;
        }
        self.relayout(Some(key));
    }

    fn activate<H: LibraryLike>(&mut self, elem: u32, held: bool, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        if elem == CRUMB_ELEM {
            // The crumb's root link: back to the library page below.
            fx.push(Fx::App(AppFx::Content(ContentReq::Back)));
            return Handled::Yes;
        }
        if let Some(index) = self.shelves.iter().position(|row| row.heading_elem() == Some(elem)) {
            // OK on a linked heading opens the collection its row lists.
            if let Some(link) = H::section_hubs(cx).shelves().get(index).and_then(|shelf| shelf.link.as_ref()) {
                fx.push(Fx::App(AppFx::Content(ContentReq::Push(ContentArg::Collection(link.clone())))));
            }
            return Handled::Yes;
        }
        if let Some(item) = self.focused_item(Some(self.key(elem)), cx).filter(|item| !item.rk.is_empty()) {
            let from_deck = self.from_deck(elem, cx);
            let req = if held {
                LibraryReq::ItemMenu { sid: item.sid, rk: item.rk.clone(), from_deck }
            } else if from_deck && plx_media::route::deck_press().press_plays() {
                LibraryReq::Play { sid: item.sid, rk: item.rk.clone(), resume_ns: plx_data::metadata::resume_ns(item.resume_ms, item.dur_ns / 1_000_000) }
            } else { LibraryReq::Detail { sid: item.sid, rk: item.rk.clone(), from_deck } };
            fx.push(Fx::App(AppFx::Library(req)));
            return Handled::Yes;
        }
        use plx_ui::widgets::{TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_MOVIES, TAB_KEY_SEARCH, TAB_KEY_SHOWS, TAB_KEY_WATCHLIST};
        use crate::registry::HomeTab;
        let req = match elem {
            TAB_KEY_HOME => Some(LibraryReq::Tab(HomeTab::Home)),
            TAB_KEY_MOVIES => Some(LibraryReq::Tab(HomeTab::Movies)),
            TAB_KEY_SHOWS => Some(LibraryReq::Tab(HomeTab::Shows)),
            TAB_KEY_SEARCH => Some(LibraryReq::Tab(HomeTab::Search)),
            TAB_KEY_WATCHLIST => Some(LibraryReq::Tab(HomeTab::Watchlist)),
            plx_ui::widgets::TAB_KEY_LIVETV => Some(LibraryReq::Tab(HomeTab::LiveTv)),
            TAB_KEY_CHIP => Some(LibraryReq::Account),
            _ => None,
        };
        if let Some(req) = req { fx.push(Fx::App(AppFx::Library(req))); return Handled::Yes; }
        Handled::No
    }

    /// The page's own focus move by `rows` rows, keeping the column: PageUp / PageDown.
    fn page<H: LibraryLike>(&mut self, direction: i32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let Some((row, col)) = self.shelf_position(cx.focus.current) else { return Handled::No };
        let next = row.saturating_add_signed(direction.signum() as isize * PAGE_ROWS as isize).min(self.shelves.len().saturating_sub(1));
        let Some(elem) = self.shelves.get(next).and_then(|r| r.elems.get(col.min(r.elems.len().saturating_sub(1)))).copied() else { return Handled::No };
        self.reseat(FocusTarget::Elem(self.key(elem)), fx);
        Handled::Yes
    }

    fn command<H: LibraryLike>(&mut self, command: LibraryCmd, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match command {
            LibraryCmd::FocusShelf { shelf, col } => {
                let Some(&elem) = self.shelves.get(shelf).and_then(|s| s.elems.get(col)) else { return Handled::No };
                self.initial = false; // An explicit owned command supersedes the boot seat.
                self.reseat(FocusTarget::Elem(self.key(elem)), fx);
                Handled::Yes
            }
            LibraryCmd::ItemMenu => cx.focus.current.map_or(Handled::No, |key| self.activate(key.elem, true, cx, fx)),
            LibraryCmd::Page(direction) => self.page(direction, cx, fx),
            LibraryCmd::Sweep => {
                let current = cx.focus.current.filter(|key| key.entry == self.entry);
                let row = current.and_then(|key| focused_row(&self.shelves, key.elem, self.keys.last_place(key.elem)));
                if self.sweep_down && row.is_some_and(|row| row + 1 >= self.shelves.len()) {
                    self.sweep_down = false;
                } else if !self.sweep_down && row == Some(0) {
                    self.sweep_down = true;
                }
                fx.push(Fx::Deliver(MachineId::Instance(self.instance), Delivery::Screen(
                    ScreenEvent::Input(plx_machine::machine::InputEvent {
                        at: cx.tick, source: plx_machine::machine::Source::Script,
                        kind: InputKind::Key { key: if self.sweep_down { Key::Down } else { Key::Up },
                            sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
                    }))));
                Handled::Yes
            }
            // The library page's own commands (types, menus, library switches) mean nothing here.
            _ => Handled::No,
        }
    }

    fn step_event<H: LibraryLike>(&mut self, ev: &ScreenEvent<H>, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount | ScreenEvent::StoreChanged(..) => self.sync(cx),
            ScreenEvent::RestoreMemory(PageMemory::Library(memory)) => { self.restore(memory); self.sync(cx); }
            ScreenEvent::Enter(_) => { self.live = true; self.sync(cx); }
            ScreenEvent::Cover => self.live = false,
            ScreenEvent::Uncover => { self.live = true; self.sync(cx); }
            ScreenEvent::App(AppMsg::Library(command)) => return self.command(*command, cx, fx),
            ScreenEvent::FocusMoved { from, to, by } => {
                self.shelves_on(ev, cx, fx);
                // The engine seats a mounted page in the first group it can (the bar) before its hubs
                // have landed: a page still loading holds no seat instead (`PageRegions::unseat`).
                if self.initial && from.is_none() && to.entry == self.entry {
                    if let Some(target) = self.with_regions(|regions| regions.unseat()) { self.reseat(target, fx); }
                }
                if matches!(by, By::Dir | By::Pointer) { self.initial = false; }
                self.reveal_focus(*to, *by, cx);
            }
            ScreenEvent::Tick(tick) => {
                self.sync(cx);
                // The shelves dissolve in once the first publication is there (no swap is ever
                // scheduled on this fader, so there is never a commit frame to apply).
                self.fade.tick(tick.dt(), self.readout != Readout::Loading);
                if self.live {
                    let dt = tick.dt();
                    let focused = cx.focus.current;
                    let colours = self.focused_item(focused, cx).filter(|item| item.has_blur).map(|item| item.blur)
                        .or_else(|| (!self.ground_seeded).then(|| H::section_hubs(cx).shelves().first()
                            .and_then(|shelf| shelf.items.first()).filter(|item| item.has_blur).map(|item| item.blur)).flatten());
                    self.ground_seeded |= colours.is_some();
                    self.ground.key(colours, plx_ui::widgets::PageGround::CARD_W, dt);
                    self.header(cx).step(self.entry, focused, &mut self.header_capsules, &mut self.header_pop, dt);
                    // A row at exact rest is a fixed point of its tick, so a page of hundreds of
                    // them costs a lookup each, not a spring step.
                    self.shelves_on(ev, cx, fx);
                    self.scroll.step(self.scroll_target, K_SCROLL, dt);
                    self.relayout(focused);
                    let hint = self.hint_input(cx);
                    self.hold_hint.step(hint, tick.ms, dt, &mut |ev| fx.note(ev));
                    // The page's own first seat: the first row, once there is one.
                    if self.initial && self.readout == Readout::Rows {
                        self.initial = false;
                        self.reseat_first(fx);
                    }
                }
                if let Some(target) = self.address(cx) {
                    self.send_hold(target, cx, fx);
                    let at_head = self.scroll.pos.abs() < 1.0 && self.scroll.vel.abs() < 1.0 && self.scroll_target.abs() < 0.5;
                    fx.push(Fx::App(AppFx::Library(LibraryReq::PublishShelves { target, at_head, hidden_page: false })));
                }
                fx.push(Fx::App(AppFx::StoreWork(StoreWork::Browse)));
            }
            ScreenEvent::Activate(elem) => return self.activate(*elem, false, cx, fx),
            ScreenEvent::PressHold(_) => return self.command(LibraryCmd::ItemMenu, cx, fx),
            ScreenEvent::PressCommit(_) => {
                return cx.focus.current.map_or(Handled::No, |key| self.activate(key.elem, false, cx, fx));
            }
            ScreenEvent::Input(input) => {
                // An arrow or a pointer acting on a page that has seated nothing yet is the reader's
                // own move: the rows that land afterwards must not take the seat from it.
                if matches!(input.kind, InputKind::Key { key: Key::Up | Key::Down | Key::Left | Key::Right, edge: Edge::Down, .. }
                    | InputKind::Pointer { .. } | InputKind::Click { .. } | InputKind::Drag { .. }) {
                    self.initial = false;
                }
                if let InputKind::Wheel { dy } = input.kind {
                    self.scroll_target = (self.scroll_target - dy * shelf_pitch(false, ui_cards::LEGACY_BAND_H, 0.0))
                        .clamp(0.0, max_scroll(&self.target_run));
                    fx.invalidate(Provenance::Input);
                    return Handled::Yes;
                }
                if let InputKind::Key { sym, wcode, edge: Edge::Down, .. } = input.kind {
                    if let Some(dir) = plx_ui::consts::page_dir(sym, wcode) {
                        return self.page(dir, cx, fx);
                    }
                }
                if let InputKind::Key { key: Key::Back, edge: Edge::Down, .. } = input.kind {
                    return self.back(cx, fx);
                }
            }
            _ => {}
        }
        Handled::No
    }
}

impl<H: LibraryLike> Machine<H> for RecommendedScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        self.step_event(ev, cx, fx)
    }
}

// ---- focus ------------------------------------------------------------------------------------

impl RecommendedScreen {
    fn row_elems(&self, group: Option<GroupId>) -> Vec<u32> {
        let Some(group) = group else { return Vec::new() };
        if group == CRUMB_GROUP { return vec![CRUMB_ELEM]; }
        self.shelves.iter().find_map(|row| {
            if row.group == group { return Some(row.elems.clone()); }
            row.heading.filter(|(heading, _)| *heading == group).map(|(_, elem)| vec![elem])
        }).unwrap_or_default()
    }

    /// Where row `index` sits for the component, in screen space: the top of its tiles. What stands
    /// under its caption is the next row's heading, drawn lifted by that row's `heading_lift`.
    fn shelf_frame(&self, index: usize) -> SectionFrame {
        SectionFrame {
            below_rise: self.shelves.get(index + 1).map_or(0.0, |next| next.cards.heading_lift()),
            ..SectionFrame::new(shelf_y(&self.run, index, self.scroll.pos) + CARD_DY, content_clip())
        }
    }

    /// Row `index`'s content, read off the published hub (borrows only).
    pub(super) fn hub_src<'a, 'v: 'a, H: LibraryLike>(&'a self, index: usize, cx: &Cx<'v, H>) -> HubSrc<'a> {
        HubSrc { elems: &self.shelves[index].elems, shelf: H::section_hubs(cx).shelves().get(index), paging: false }
    }

    /// The card `elem` of row `index` as drawn now: the component's placement (pop and press
    /// folded in, `rest_rect` the settled focus-scaled rect).
    fn shelf_placed<H: LibraryLike>(&self, index: usize, elem: &u32, cx: &Cx<'_, H>) -> Option<Placed> {
        let src = self.hub_src(index, cx);
        self.shelves[index].cards.place(cx, &src, elem, self.shelf_frame(index), At::Drawn)
            .map(|placed| Placed { index: None, ..placed })
    }

    /// Where card `col` of row `index` is GOING: the settled rect at the scroll target, with the
    /// row scrolled to bring the focused card into view.
    fn shelf_rect_at<H: LibraryLike>(&self, index: usize, col: usize, cx: &Cx<'_, H>) -> Rect {
        let row = &self.shelves[index];
        let style = row_style(row);
        let focused = cx.focus.current.filter(|key| key.entry == self.entry)
            .and_then(|key| row.elems.iter().position(|elem| *elem == key.elem));
        let x = focused.map(|col| ui_cards::scroll_into_view(row.cards.scroll(), col,
            row.elems.len(), style.w, style.gap, SCR_W - 2.0 * MARGIN_X)).unwrap_or(row.cards.scroll());
        ui_cards::tile_rect(col, MARGIN_X, style.w + style.gap, x,
            shelf_y(&self.target_run, index, self.scroll_target) + CARD_DY, (style.w, style.h))
            .scaled(if focused == Some(col) { style.focus_scale } else { 1.0 })
    }

    /// The linked heading of row `index`, over the published hub's own title.
    fn heading_widget<'a, H: LibraryLike>(&self, index: usize, cx: &Cx<'a, H>) -> Option<plx_ui::linked_heading::LinkedHeading<'a>> {
        self.shelves.get(index)?.heading?;
        let shelf = H::section_hubs(cx).shelves().get(index)?;
        Some(plx_ui::linked_heading::LinkedHeading::heading(&shelf.title, "").total(shelf.total)
            .bounded(SCR_W - 2.0 * MARGIN_X))
    }

    /// The linked heading's face, cap top where an unlinked heading's would be.
    fn heading_rect<H: LibraryLike>(&self, index: usize, cx: &Cx<'_, H>, at: At) -> Option<Rect> {
        let heading = self.heading_widget(index, cx)?;
        let row = &self.shelves[index];
        let focused = cx.focus.current.is_some_and(|key| key.entry == self.entry && Some(key.elem) == row.heading_elem());
        let y = match at {
            At::Drawn => shelf_y(&self.run, index, self.scroll.pos) - TITLE_DY - row.cards.heading_lift(),
            At::SpringTarget => shelf_y(&self.target_run, index, self.scroll_target) - TITLE_DY,
        };
        Some(heading.face_rect(MARGIN_X, y, f32::from(focused), &heading.measure(cx.measure)))
    }
}

impl<H: LibraryLike> Focusable<H> for RecommendedScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.extend(self.header_focus(&self.header(cx)).group_spec());
        for (index, row) in self.shelves.iter().enumerate() {
            let mut shelf = row_group(row.group, row.elems.len(), Rect::new(MARGIN_X,
                shelf_y(&self.target_run, index, self.scroll_target) + CARD_DY, SCR_W - 2.0 * MARGIN_X, row_style(row).h), ElemKind::Card);
            if let Some((group, elem)) = row.heading {
                if let Some(rect) = self.heading_rect(index, cx, At::SpringTarget) {
                    out.push(plx_ui::linked_heading::group_spec(group, rect));
                }
                // DOWN from the heading returns to the card this row remembered.
                shelf.seat = plx_ui::linked_heading::shelf_seat(&cx.focus, elem, row.group, shelf.seat);
            }
            out.push(shelf);
        }
        self.with_regions(|regions| regions.seat_content_head(out));
        self.with_regions(|regions| regions.declare_pending(out));
    }
    fn group_of(&self, elem: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        if *elem == CRUMB_ELEM { return Some(CRUMB_GROUP); }
        if let Some(row) = self.shelves.iter().find(|row| row.elems.contains(elem)) { return Some(row.group); }
        self.shelves.iter().find_map(|row| row.heading.filter(|(_, e)| e == elem)).map(|(group, _)| group)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        if key.elem == CRUMB_ELEM { return self.header_focus(&self.header(cx)).neighbour(key, dir); }
        plx_ui::page_regions::row_neighbour(self.entry, &self.row_elems(self.group_of(&key.elem, cx)), key, dir)
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let rect = if *elem == CRUMB_ELEM {
            return self.header_focus(&self.header(cx)).place(*elem, at);
        } else if let Some((row, col)) = self.cell_of(*elem) {
            if at == At::Drawn { return self.shelf_placed(row, elem, cx); }
            self.shelf_rect_at(row, col, cx)
        } else if let Some(index) = self.shelves.iter().position(|row| row.heading_elem() == Some(*elem)) {
            self.heading_rect(index, cx, at)?
        } else { return None };
        Some(Placed { rect, rest_rect: rect, clip: content_clip(), index: None })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if let Some(key) = self.header_focus(&self.header(cx)).reconcile(want) { return key; }
        if self.place(&want.elem, cx, At::SpringTarget).is_some() { return want; }
        if let Some((group, index)) = self.keys.last_place(want.elem) {
            let elems = self.row_elems(Some(group));
            if let Some(elem) = elems.get(index.min(elems.len().saturating_sub(1))) { return self.key(*elem); }
        }
        self.first_group().and_then(|group| self.row_elems(Some(group)).first().copied()).map(|elem| self.key(elem))
            .unwrap_or_else(|| self.key(plx_ui::dispatch::STRIP_BASE))
    }
    fn seat(&self, group: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if group == CRUMB_GROUP { return self.header_focus(&self.header(cx)).seat(from); }
        let elems = self.row_elems(Some(group));
        let elem = elems.iter().filter_map(|elem| self.place(elem, cx, At::SpringTarget)
            .map(|placed| (*elem, (placed.rect.cx() - from.rect.cx()).abs())))
            .min_by(|a, b| a.1.total_cmp(&b.1)).map(|(elem, _)| elem)
            .unwrap_or(plx_ui::dispatch::STRIP_BASE);
        self.key(elem)
    }
}

// ---- draw -------------------------------------------------------------------------------------

impl RecommendedScreen {
    /// The rows whose band can meet the panel at the drawn scroll. Everything above and below is
    /// skipped without being looked at; `row_on_screen` stays the exact test for the edge rows the
    /// window keeps.
    pub(super) fn shelf_window(&self) -> std::ops::Range<usize> {
        let base = run_top(self.scroll.pos) - TITLE_DY;
        self.run.window(-base, SCR_H - base)
    }

    /// The painter the rows (their headings and their cards) and the Empty status caption are drawn
    /// through: the page's alpha under the shelves' own dissolve.
    fn shelf_painter<H: LibraryLike>(&self, f: &DrawFrame<'_, '_, H>) -> plx_ui::Painter {
        f.painter.alpha(f.page_alpha * self.fade.alpha())
    }

    /// The page's header row, "<Library> › Recommended": the shared `PageHeader` configured with a
    /// crumb (the library's name is the link back, the page's name sits on the selected plate), the
    /// same row the Collections page wears. Built from the library's name when that, the kind or
    /// the locale changes and held until then ([`Self::header_key`]); its one stop is the crumb's
    /// root.
    fn header<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> std::rc::Rc<HeaderRow> {
        self.header_row.get(self.header_key(cx), || {
            let crumb = Crumb::new(
                super::collections_page::root_label(Self::library_title(cx), self.kind),
                plx_platform::i18n::msg::library_header_recommended());
            let page = PageHeader::new(PageHeaderSpec { crumb: Some(crumb), ..Default::default() }, &[], cx.measure);
            let layout = page.layout(SCR_W, cx.measure);
            let root = layout.items.iter().position(|item| item.slot == Slot::CrumbRoot).unwrap_or(0);
            HeaderRow::new(page, layout, vec![(CRUMB_ELEM, root)])
        })
    }

    /// A fingerprint of everything [`Self::header`] builds the row from: the locale (the page's
    /// name), the library's name and the kind (the root word's fallback).
    fn header_key<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (std::ptr::from_ref(plx_platform::i18n::current()) as usize).hash(&mut h);
        Self::library_title(cx).hash(&mut h);
        matches!(self.kind, SecKind::Movie).hash(&mut h);
        h.finish()
    }

    /// `row` as this page's header focus group, at its scroll.
    fn header_focus<'a>(&self, row: &'a HeaderRow) -> HeaderFocus<'a> {
        row.focus(self.entry, CRUMB_GROUP, (self.scroll.pos, self.scroll_target), content_clip())
    }

    /// The page's title row: "<Library> › Recommended".
    pub(super) fn draw_title<H: LibraryLike>(&self, f: &DrawFrame<'_, '_, H>) {
        self.header(f.cx).draw(f.painter.alpha(f.page_alpha), self.scroll.pos, &self.header_capsules,
            self.header_pop.scale_with(0, f.press.scale), None);
    }

    fn library_title<'a, H: LibraryLike>(cx: &Cx<'a, H>) -> &'a str {
        let directory = H::directory(cx);
        directory.current().and_then(|i| directory.sections().get(i)).map_or("", |section| section.row.title.as_str())
    }

    fn draw_page<H: LibraryLike>(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        plx_ui::profile::phase("rc.clear", || {
            plx_gfx::gfx::frame_clear(plx_ui::theme::CLEAR_RGB.0, plx_ui::theme::CLEAR_RGB.1, plx_ui::theme::CLEAR_RGB.2);
        });
        plx_ui::profile::phase("rc.ground", || self.ground.draw(f.painter.alpha(f.page_alpha), Rect::FULL));
        let p = self.shelf_painter(f);
        let env = Env::inert();
        self.draw_title(f);
        plx_ui::profile::phase("rc.rows", || {
            for index in self.shelf_window() {
                #[cfg(test)] self.shelf_visits.set(self.shelf_visits.get() + 1);
                let Some(row) = self.shelves.get(index) else { continue };
                let Some(shelf) = H::section_hubs(f.cx).shelves().get(index) else { continue };
                let origin = shelf_y(&self.run, index, self.scroll.pos);
                if !row_on_screen(origin, self.run.pitch(index)) { continue; }
                let heading_y = origin - TITLE_DY - row.cards.heading_lift();
                if let Some(heading) = self.heading_widget(index, f.cx) {
                    let focused = f.focus.current.is_some_and(|key| key.entry == self.entry && Some(key.elem) == row.heading_elem());
                    heading.draw(p, MARGIN_X, heading_y, f32::from(focused), &heading.measure(f.measure), f.measure);
                } else {
                    ui_cards::draw_heading(p, &shelf.title, "", MARGIN_X, heading_y, SCR_W - 2.0 * MARGIN_X, f.measure);
                }
                row.cards.paint(f, p, &self.hub_src(index, f.cx), self.shelf_frame(index));
            }
        });
        match self.readout {
            Readout::Loading => {
                plx_ui::placeholder::note(f.painter, plx_ui::placeholder::Reason::LibrarySpinner, "");
                plx_ui::widgets::Spinner::new(SCR_W * 0.5, SCR_H * 0.52, 26.0).phase(f.cx.tick.ms).draw(&env, f.painter.alpha(f.page_alpha));
            }
            Readout::Empty => {
                let caption = std::ffi::CString::new(plx_platform::i18n::msg::recommended_page_empty()).unwrap_or_default();
                let frame = Rect::new(MARGIN_X, CONTENT_TOP + plx_ui::page_header::BLOCK_H, SCR_W - 2.0 * MARGIN_X, SCR_H - (CONTENT_TOP + plx_ui::page_header::BLOCK_H) - MARGIN_Y);
                plx_ui::widgets::StatusOverlay::new(frame, &caption, plx_ui::widgets::StatusKind::Empty)
                    .phase(f.cx.tick.ms).draw_measured(&env, p, f.cx.measure);
            }
            Readout::Rows => {}
        }
        // A standing note, never a target: over the page, records no stop and no hit rect.
        self.hold_hint.draw(f.painter.alpha(f.page_alpha), f.measure);
        self.record_stops(f);
    }

    /// The production stop producers without rasterization, for the no-SDL dispatcher fixture.
    pub(super) fn record_stops<H: LibraryLike>(&self, f: &mut DrawFrame<'_, '_, H>) {
        if !f.records_stops() { return; }
        for index in self.shelf_window() {
            #[cfg(test)] self.shelf_visits.set(self.shelf_visits.get() + 1);
            let Some(row) = self.shelves.get(index) else { continue };
            let origin = shelf_y(&self.run, index, self.scroll.pos);
            if !row_on_screen(origin, self.run.pitch(index)) { continue; }
            let cx = f.cx;
            row.cards.record_stops(f, f.painter, &self.hub_src(index, cx), self.shelf_frame(index));
            // After the row's cards, so the heading wins where a popped card's glow overlaps it.
            if let (Some(elem), Some(heading), Some(rect)) = (row.heading_elem(),
                self.heading_widget(index, f.cx), self.heading_rect(index, f.cx, At::Drawn)) {
                heading.stop(f, rect, self.key(elem));
            }
        }
    }

    fn redraw_focused<H: LibraryLike>(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<FocusKey<u32>>) {
        let Some(key) = focus.filter(|key| key.entry == self.entry) else { return };
        let Some(placed) = <Self as Focusable<H>>::place(self, &key.elem, f.cx, At::Drawn) else { return };
        let _clip = f.clip(f.painter, placed.clip);
        if let Some(index) = self.shelves.iter().position(|row| row.elems.contains(&key.elem)) {
            let p = self.shelf_painter(f);
            let cx = f.cx;
            self.shelves[index].cards.redraw_focused(f, p, &self.hub_src(index, cx), self.shelf_frame(index), Some(key));
        }
    }
}

impl<H: LibraryLike> Screen<H> for RecommendedScreen {
    fn focused_card<'a>(&self, cx: &Cx<'a, H>, focus: Option<FocusKey<u32>>, at: Option<At>) -> Option<plx_ui::screen::FocusedCard<'a>> {
        let item = self.focused_item(focus, cx)?;
        let placed = focus.zip(at).and_then(|(key, at)| Focusable::<H>::place(self, &key.elem, cx, at));
        Some(plx_ui::screen::FocusedCard::new(item, placed))
    }
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<FocusKey<u32>>) {
        RecommendedScreen::redraw_focused::<H>(self, f, focus)
    }
    // The heartbeat's `route=` word and the focus probe's screen both read this: it is the
    // library family's word, so a scene that greps `route=library` keeps matching.
    fn name(&self) -> &'static str { "library" }
    fn state(&self) -> &dyn LogicalState { self }
    fn crumb(&self, _: &Cx<'_, H>) -> Option<Cow<'_, str>> { None }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) { self.draw_page(f); }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn memory(&self) -> PageMemory { PageMemory::Library(self.page_memory()) }
    fn links(&self, out: &mut Vec<Link>) {
        // The column: bar, crumb, then the rows (a linked heading above its cards).
        self.with_regions(|regions| regions.links(out));
    }
    fn holds_arrival(&self, dir: plx_ui::screen::Dir) -> bool {
        self.with_regions(|regions| regions.holds(dir))
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> { Some(self) }
}

impl LogicalState for RecommendedScreen {
    fn write(&self, c: &mut Canon) {
        c.u32(self.entry.0).u32(self.instance.0);
        c.u32(match self.kind { SecKind::Movie => 0, SecKind::Show => 1 });
        c.f32(self.scroll.pos).f32(self.scroll.vel).f32(self.scroll_target);
        c.option(self.restore_scroll, |c, value| { c.f32(value); });
        c.bool(self.live).bool(self.initial).bool(self.sweep_down);
        c.option(self.epoch, |c, value| { c.u32(value); });
        c.option(self.query, |c, value| { c.u32(value); });
        c.option(self.shelf_publication, |c, (id, revision)| {
            c.u32(id.epoch).u32(u32::from(id.sid.raw())).u64(id.section as u64).u64(revision);
        });
        c.bool(self.ground_seeded);
        self.fade.write(c);
        self.ground.write_motion(c);
        self.header_capsules.write_motion(c);
        self.header_pop.write_motion(c);
        PageMemory::Library(self.page_memory()).write(c);
        c.seq(self.viewports.len());
        for viewport in &self.viewports { viewport.write(c); }
        c.seq(self.shelves.len());
        for row in &self.shelves {
            // `basis` is what the keys were registered from: derived from the publication, not canon.
            let Row { id, group, heading, elems, landscape, deck: _, cards, basis: _ } = row;
            c.str(id).u32(group.0);
            c.option(*heading, |c, (group, elem)| { c.u32(group.0).u32(elem); });
            c.bool(*landscape).seq(elems.len());
            for elem in elems { c.u32(*elem); }
            cards.write(c);
        }
        c.u32(match self.readout { Readout::Loading => 0, Readout::Empty => 1, Readout::Rows => 2 });
        c.f32(self.run.height());
    }
    fn probe(&self, out: &mut String) { out.push_str("library"); }
}

// ---- the card source ----------------------------------------------------------------------------

/// One hub row's cards for the shared section component: the published hub's items read through
/// the page's element ids. Built from borrows (no copy), so a call can hold it while the row's
/// own state is borrowed mutably.
pub(super) struct HubSrc<'a> {
    pub(super) elems: &'a [u32],
    /// `None` while the page still names a hub the store has not published (a frame between a
    /// store change and the next `sync`): its cards draw nothing.
    pub(super) shelf: Option<&'a plx_data::browse::section_hubs::Shelf>,
    /// Focus is near the row's trailing edge on a visible row: only then does the row ask for more.
    pub(super) paging: bool,
}

impl HubSrc<'_> {
    fn item(&self, i: usize) -> Option<&plx_data::pms::PmsMovie> { self.shelf?.items.get(i) }
}

impl<H: plx_machine::machine::Host<Elem = u32>> CardSource<H> for HubSrc<'_> {
    fn len(&self) -> usize { self.elems.len() }
    /// The card's place in the hub's listing (`positions` can skip what the window drops).
    fn global(&self, i: usize) -> usize {
        self.shelf.map_or(i, |shelf| shelf.positions.get(i).copied().unwrap_or(shelf.offset + i))
    }
    fn elem(&self, i: usize) -> u32 { self.elems.get(i).copied().unwrap_or(0) }
    fn index_of(&self, e: &u32) -> Option<usize> { self.elems.iter().position(|elem| elem == e) }
    /// A card whose item the hub has not published draws nothing (its stop still registers).
    fn loaded(&self, i: usize) -> bool { self.item(i).is_some() }
    fn more(&self) -> bool { self.paging && self.shelf.is_some_and(|shelf| shelf.more) }
    fn page_epoch(&self) -> u32 { self.shelf.map_or(0, |shelf| shelf.epoch) }
    fn more_after(&self) -> bool { self.shelf.is_some_and(|shelf| shelf.more) }
    fn more_before(&self) -> bool { self.shelf.is_some_and(|shelf| shelf.offset > 0) }
    fn art(&self, i: usize) -> Art<'_> {
        match (self.item(i), self.shelf) {
            (Some(item), Some(shelf)) if shelf.landscape => Art::Still(Some(tile_facts::of(item))),
            (Some(item), _) => Art::Poster(Some(tile_facts::of(item))),
            _ => Art::Poster(None),
        }
    }
    fn label(&self, i: usize) -> ui_cards::TileLabel {
        self.shelf.map_or_else(ui_cards::TileLabel::default, |shelf| shelf_label(shelf, i))
    }
    fn progress(&self, i: usize) -> Option<f32> {
        self.shelf.filter(|shelf| !shelf.landscape).and_then(|_| self.item(i)).and_then(|item| item.resume_frac())
    }
    /// A landscape row's episode still carries its show line, and the Continue Watching deck its
    /// play mark when a press plays.
    fn overlay(&self, p: plx_ui::Painter, i: usize, tile: &Tile, measure: &dyn plx_machine::machine::Measure) {
        let (Some(shelf), Some(item)) = (self.shelf, self.item(i)) else { return };
        if shelf.landscape {
            plx_ui::widgets::still_overlay(p, &tile_facts::of(item), tile.rect, tile.radius,
                shelf.is_continue && plx_media::route::deck_press().press_plays(), measure);
        }
    }
}

pub(super) fn shelf_label(shelf: &plx_data::browse::section_hubs::Shelf, col: usize) -> ui_cards::TileLabel {
    let Some(item) = shelf.items.get(col) else { return ui_cards::TileLabel::default() };
    if shelf.landscape { return crate::registry::captions::still_label(item, shelf.is_continue, ""); }
    crate::registry::captions::shelf_label(&crate::registry::captions::policy(), item, shelf.is_continue)
}

#[cfg(test)]
#[path = "recommended_tests.rs"]
pub(super) mod recommended_tests;
#[cfg(test)]
#[path = "shelf_action_tests.rs"]
mod shelf_action_tests;
#[cfg(test)]
#[path = "shelf_paging_tests.rs"]
mod shelf_paging_tests;
#[cfg(test)]
#[path = "recommended_sync_tests.rs"]
mod recommended_sync_tests;
