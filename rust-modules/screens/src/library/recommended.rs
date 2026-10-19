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
//! pitch of a row, the card source, the group spec), so nothing here reaches into code the
//! library page no longer needs.
//!
//! **Back** is the page's own pop (`ContentReq::Back`), whatever the focus is on: the return state
//! the Recommended press captured puts focus back on that control (the library page beneath keeps
//! its own focus engine memory). The page never changes the library.
use std::borrow::Cow;
use std::collections::HashSet;

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
    At, AxisMask, DrawFrame, EdgeRule, ElemKind, Enter, FocusTarget, Focusable, GroupKind,
    GroupSpec, Link, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step, Dir, By,
};
use plx_ui::widgets::Art;
use plx_ui::{on_axis, Env, Rect, Spring, View};

use super::identity::{KeyRegion, KeyRegistry};
use crate::registry::{
    tile_facts, AppFx, AppMsg, ContentArg, ContentReq, LibraryCmd, LibraryIdentity, LibraryLike,
    LibraryMemory, LibraryReq, LibrarySectionIdentity, LibraryViewport, PageMemory,
};

/// Where the page's title stands: the same top the library's header row has.
const CONTENT_TOP: f32 = plx_ui::page_header::TOP;
/// The title block under the header row (the library header row's own block height): the grid's
/// first content line sits this far under [`CONTENT_TOP`], y 244.
const TITLE_BLOCK_H: f32 = 96.0;
/// The run's start under [`CONTENT_TOP`]: the first shelf's HEADING (drawn [`TITLE_DY`] above its
/// row's origin) starts on the grid's content line, y 244, clear of the title row's 148..204 and the
/// air under it; the origin, and the cards under it, follow the heading.
const HEAD_H: f32 = TITLE_BLOCK_H + TITLE_DY;
/// Rows PageUp / PageDown move focus by.
const PAGE_ROWS: usize = 2;
/// The strip group the page's first row links up to (the shared top bar).
const STRIP: GroupId = plx_ui::containers::tabs::STRIP;

/// The crumb's root link ("Movies"): the page's one control above the rows, a fixed element and
/// group of its own (the same element the Collections page's crumb root has on the library page).
const CRUMB_ELEM: u32 = super::toolbar::CRUMB_ROOT;
const CRUMB_GROUP: GroupId = super::LIBRARY_GROUP;

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
    pub(super) cards: ui_cards::Shelf,
}

impl Row {
    fn heading_elem(&self) -> Option<u32> { self.heading.map(|(_, elem)| elem) }
    /// The group a door INTO this row from above lands in: its linked heading when it has one.
    fn entry_group(&self) -> GroupId { self.heading.map_or(self.group, |(group, _)| group) }
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
    sweep_down: bool,
    /// The standing "Hold OK for options" hint over a card. Paint-only.
    hold_hint: plx_ui::hold_hint::HoldHint,
    /// The held range last sent to the store, with the section it was for, so the command goes out
    /// only when it changes. Derived from what is on screen; not canon.
    hold_sent: Option<(SectionAddress, (usize, usize))>,
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

/// The height of a row of `landscape` cards whose caption band is `expanded` (0..1) open.
pub(super) fn shelf_pitch(landscape: bool, expanded: f32) -> f32 {
    let art = if landscape { RowStyle::EPISODE.h } else { CARD_H };
    TITLE_DY + CARD_DY + art + ui_cards::under_band(expanded) + UNDER_LABEL_AIR
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

fn row_group(id: GroupId, len: usize, extent: Rect, elem: ElemKind) -> GroupSpec {
    GroupSpec { id, kind: GroupKind::Row { wrap: false }, seat: Seat::Remembered,
        reachable: AxisMask::VERTICAL, edge: [EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop], extent, len, elem }
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
            ground: plx_ui::widgets::PageGround::new(), ground_seeded: false, sweep_down: true,
            hold_hint: plx_ui::hold_hint::HoldHint::once_per_run(plx_ui::hold_hint::Kind::Library),
            hold_sent: None,
            #[cfg(test)] shelf_visits: std::cell::Cell::new(0),
        }
    }

    /// The canon shape of the page (`LogicalState::write`).
    pub const SHAPE: &'static str = concat!(
        "RecommendedScreen{entry:u32,instance:u32,kind:u32,scroll:{pos:f32,vel:f32},scroll_target:f32,",
        "restore_scroll:Option<f32>,live:bool,initial:bool,sweep_down:bool,epoch:Option<u32>,query:Option<u32>,",
        "shelf_publication:Option<(HubsId{epoch:u32,sid:u32,section:u64},revision:u64)>,ground_seeded:bool,",
        "ground:PageGround,memory:PageMemory::Library,viewports:[LibraryViewport],",
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
        if self.shelf_publication != publication {
            self.rebuild(cx, identity.as_ref(), epoch);
            self.shelf_publication = publication;
        }
        self.readout = if self.section.is_none() || !settled(hubs) { Readout::Loading }
            else if self.shelves.is_empty() { Readout::Empty } else { Readout::Rows };
        self.relayout(cx.focus.current);
    }

    /// Build the rows from the store's publication: keys, groups, linked headings and card
    /// sections, keeping the card section (scroll, pop, ledgers) of a row that survives.
    fn rebuild<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, identity: Option<&LibrarySectionIdentity>, epoch: Option<u32>) {
        let hubs = H::section_hubs(cx);
        let mut old = std::mem::take(&mut self.shelves);
        let previous: HashSet<u32> = old.iter().flat_map(|row| row.elems.iter().copied()).collect();
        if let Some(section) = identity {
            for (index, shelf) in hubs.shelves().iter().enumerate() {
                let group = GroupId(self.keys.register(
                    LibraryIdentity::Control { section: section.clone(), kind: "shelf-group".into(), key: shelf.id.clone() },
                    GroupId(0), index));
                // A row the store gave its cards up to is its card count in placeholder slots, so it
                // keeps its length, its scroll and its focusability.
                let elems = (0..shelf.slots()).map(|col| {
                    let identity = match shelf.items.get(col).filter(|item| !item.rk.is_empty()) {
                        Some(item) => LibraryIdentity::Shelf { section: section.clone(), hub: shelf.id.clone(), sid: item.sid, rk: item.rk.clone() },
                        None => LibraryIdentity::ShelfSlot { section: section.clone(), hub: shelf.id.clone(), publication: self.query.unwrap_or(0), slot: col as u32 },
                    };
                    self.keys.register(identity, group, col)
                }).collect();
                let mut cards = old.iter().position(|row| row.id == shelf.id && row.landscape == shelf.landscape)
                    .map(|i| old.remove(i).cards).unwrap_or_else(|| {
                        let mut cards = hub_cards(self.entry, shelf.landscape);
                        let saved = self.viewports.iter().find(|view| Some(view.epoch) == epoch && view.section == *section)
                            .and_then(|view| view.shelves.iter().find(|(id, _)| *id == shelf.id));
                        if let Some((_, x)) = saved { cards.restore_scroll(*x, shelf.slots()); }
                        cards
                    });
                let heading = shelf.link.as_ref().map(|_| {
                    let control = |kind: &str| LibraryIdentity::Control {
                        section: section.clone(), kind: kind.into(), key: shelf.id.clone() };
                    let heading_group = GroupId(self.keys.register(control("shelf-heading-group"), GroupId(0), index));
                    let elem = self.keys.register(control("shelf-heading"), heading_group, 0);
                    (heading_group, elem)
                });
                // A page that landed (or a refresh) answers the ask the row's latch was holding.
                cards.reset_page_requests();
                self.shelves.push(Row { id: shelf.id.clone(), group, heading, elems, landscape: shelf.landscape, cards });
            }
        }
        // Rotating refetches mint a key per new item: keep the current rows, the previous
        // publication's (a focus on one recovers through it) and the focus; above twice that
        // plus 256 the oldest others go.
        let current: HashSet<u32> = self.shelves.iter().flat_map(|row| row.elems.iter().copied()).collect();
        let focus = cx.focus.current.filter(|key| key.entry == self.entry).map(|key| key.elem);
        let cap = crate::registry::key_cap(current.len() + previous.len());
        self.keys.prune(KeyRegion::Shelf, cap, |elem| current.contains(&elem) || previous.contains(&elem) || Some(elem) == focus);
    }

    fn relayout(&mut self, focus: Option<FocusKey<u32>>) {
        self.run.set(self.shelves.iter().map(|row| shelf_pitch(row.landscape, row.cards.band_expand())));
        self.target_run.set(self.shelves.iter().map(|row| shelf_pitch(row.landscape,
            f32::from(focus.is_some_and(|key| row.elems.contains(&key.elem))))));
        self.scroll_target = self.scroll_target.clamp(0.0, max_scroll(&self.target_run));
    }

    fn first_group(&self) -> GroupId {
        self.shelves.first().map_or(STRIP, Row::entry_group)
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

    /// The header row's group and its stops (the crumb root alone), as the library page's
    /// `toolbar_group` / `probe_header_stops` answer for theirs.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_header(&self) -> (GroupId, Vec<(&'static str, u32)>) { (CRUMB_GROUP, vec![("crumb", CRUMB_ELEM)]) }

    /// `(page alpha, focused card's pop scale)` — what a viewer sees of a focus arrival. The page
    /// is opaque from its first frame (the container's own transition carries the arrival).
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_arrival(&self, focus: Option<FocusKey<u32>>) -> (f32, Option<f32>) {
        let elem = focus.filter(|key| key.entry == self.entry).map(|key| key.elem);
        let scale = self.shelves.iter().find_map(|shelf| {
            let col = elem.and_then(|elem| shelf.elems.iter().position(|key| *key == elem))?;
            Some(shelf.cards.drawn_pop(col, Some(col)))
        });
        (1.0, scale)
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
    fn shelves_on<H: LibraryLike>(&mut self, ev: &ScreenEvent<H>, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let hubs = H::section_hubs(cx);
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
        let hold = ui_cards::hold_band((lo, hi), self.shelves.len());
        if self.hold_sent != Some((target, hold)) {
            self.hold_sent = Some((target, hold));
            self.store(target, LibraryWork::HubHold { lo: hold.0, hi: hold.1 }, fx);
        }
    }

    /// Bring a focused card (or heading) into view: the row's reveal scroll.
    fn reveal_focus(&mut self, key: FocusKey<u32>, by: By) {
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
        } else if key.elem == CRUMB_ELEM || key.elem >= plx_ui::dispatch::STRIP_BASE {
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
            ScreenEvent::FocusMoved { to, by, .. } => {
                self.shelves_on(ev, cx, fx);
                if matches!(by, By::Dir | By::Pointer) { self.initial = false; }
                self.reveal_focus(*to, *by);
            }
            ScreenEvent::Tick(tick) => {
                self.sync(cx);
                if self.live {
                    let dt = tick.dt();
                    let focused = cx.focus.current;
                    let colours = self.focused_item(focused, cx).filter(|item| item.has_blur).map(|item| item.blur)
                        .or_else(|| (!self.ground_seeded).then(|| H::section_hubs(cx).shelves().first()
                            .and_then(|shelf| shelf.items.first()).filter(|item| item.has_blur).map(|item| item.blur)).flatten());
                    self.ground_seeded |= colours.is_some();
                    self.ground.key(colours, plx_ui::widgets::PageGround::CARD_W, dt);
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
                        self.reseat(FocusTarget::ContainerGroup(self.first_group()), fx);
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
                if let InputKind::Wheel { dy } = input.kind {
                    self.scroll_target = (self.scroll_target - dy * shelf_pitch(false, 0.0))
                        .clamp(0.0, max_scroll(&self.target_run));
                    fx.invalidate(Provenance::Input);
                    return Handled::Yes;
                }
                if let InputKind::Key { sym, wcode, edge: Edge::Down, .. } = input.kind {
                    if let Some(dir) = plx_ui::consts::page_dir(sym, wcode) {
                        return self.page(dir, cx, fx);
                    }
                }
                // BACK is the page's pop: the return state captured at the Recommended press
                // seats focus on that control again.
                if let InputKind::Key { key: Key::Back, edge: Edge::Down, .. } = input.kind {
                    fx.push(Fx::App(AppFx::Content(ContentReq::Back)));
                    return Handled::Yes;
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

    /// Where row `index` sits for the component, in screen space: the top of its tiles.
    fn shelf_frame(&self, index: usize) -> SectionFrame {
        SectionFrame { y: shelf_y(&self.run, index, self.scroll.pos) + CARD_DY, clip: content_clip() }
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
        out.push(plx_ui::linked_heading::group_spec(CRUMB_GROUP, self.crumb_rect(cx, self.scroll_target)));
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
    }
    fn group_of(&self, elem: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        if *elem == CRUMB_ELEM { return Some(CRUMB_GROUP); }
        if let Some(row) = self.shelves.iter().find(|row| row.elems.contains(elem)) { return Some(row.group); }
        self.shelves.iter().find_map(|row| row.heading.filter(|(_, e)| e == elem)).map(|(group, _)| group)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        let elems = self.row_elems(self.group_of(&key.elem, cx));
        let Some(index) = elems.iter().position(|elem| *elem == key.elem) else { return Step::Edge };
        let next = match dir { Dir::Left => index.checked_sub(1), Dir::Right => Some(index + 1), _ => None };
        next.and_then(|i| elems.get(i).copied()).map_or(Step::Edge, |elem| Step::Move(self.key(elem)))
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let rect = if *elem == CRUMB_ELEM {
            self.crumb_rect(cx, if at == At::Drawn { self.scroll.pos } else { self.scroll_target })
        } else if let Some((row, col)) = self.cell_of(*elem) {
            if at == At::Drawn { return self.shelf_placed(row, elem, cx); }
            self.shelf_rect_at(row, col, cx)
        } else if let Some(index) = self.shelves.iter().position(|row| row.heading_elem() == Some(*elem)) {
            self.heading_rect(index, cx, at)?
        } else { return None };
        Some(Placed { rect, rest_rect: rect, clip: content_clip(), index: None })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.place(&want.elem, cx, At::SpringTarget).is_some() { return want; }
        if let Some((group, index)) = self.keys.last_place(want.elem) {
            let elems = self.row_elems(Some(group));
            if let Some(elem) = elems.get(index.min(elems.len().saturating_sub(1))) { return self.key(*elem); }
        }
        self.row_elems(Some(self.first_group())).first().copied().map(|elem| self.key(elem))
            .unwrap_or_else(|| self.key(plx_ui::dispatch::STRIP_BASE))
    }
    fn seat(&self, group: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<u32> {
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

    /// The painter the rows' cards are drawn through: the page's alpha.
    fn shelf_painter<H: LibraryLike>(&self, f: &DrawFrame<'_, '_, H>) -> plx_ui::Painter {
        f.painter.alpha(f.page_alpha)
    }

    /// The page's breadcrumb, "<Library> › Recommended": the library's name is the link back, the
    /// page's name sits on the selected plate. Placed on the header row like the Collections page's.
    fn crumb<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> (plx_ui::breadcrumb::Crumb, plx_ui::breadcrumb::CrumbLayout) {
        let crumb = plx_ui::breadcrumb::Crumb::new(
            super::collections_page::root_label(&Self::library_title(cx), self.kind),
            plx_platform::i18n::msg::library_header_recommended());
        let layout = crumb.layout(cx.measure);
        (crumb, layout)
    }

    /// The crumb's root plate at `scroll` (the row scrolls away with the page).
    fn crumb_rect<H: LibraryLike>(&self, cx: &Cx<'_, H>, scroll: f32) -> Rect {
        let mut plate = self.crumb(cx).1.root_plate;
        plate.y -= scroll;
        plate
    }

    /// The page's title row: "<Library> › Recommended", the same crumb the Collections page has.
    pub(super) fn draw_title<H: LibraryLike>(&self, f: &DrawFrame<'_, '_, H>) {
        let (crumb, mut layout) = self.crumb(f.cx);
        for rect in [&mut layout.root_text, &mut layout.root_plate, &mut layout.glyph, &mut layout.page_text, &mut layout.page_plate] {
            rect.y -= self.scroll.pos;
        }
        let focused = f.focus.current.is_some_and(|key| key.entry == self.entry && key.elem == CRUMB_ELEM);
        crumb.draw(f.painter.alpha(f.page_alpha), &layout, focused);
    }

    fn library_title<H: LibraryLike>(cx: &Cx<'_, H>) -> String {
        let directory = H::directory(cx);
        directory.current().and_then(|i| directory.sections().get(i)).map(|section| section.row.title.clone()).unwrap_or_default()
    }

    fn draw_page<H: LibraryLike>(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        plx_ui::profile::phase("rc.clear", || {
            plx_gfx::gfx::frame_clear(plx_ui::theme::CLEAR_RGB.0, plx_ui::theme::CLEAR_RGB.1, plx_ui::theme::CLEAR_RGB.2);
        });
        plx_ui::profile::phase("rc.ground", || self.ground.draw(f.painter.alpha(f.page_alpha), Rect::FULL));
        let p = f.painter.alpha(f.page_alpha);
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
                row.cards.paint(f, self.shelf_painter(f), &self.hub_src(index, f.cx), self.shelf_frame(index));
            }
        });
        match self.readout {
            Readout::Loading => {
                plx_ui::placeholder::note(f.painter, plx_ui::placeholder::Reason::LibrarySpinner, "");
                plx_ui::widgets::Spinner::new(SCR_W * 0.5, SCR_H * 0.52, 26.0).phase(f.cx.tick.ms).draw(&env, f.painter.alpha(f.page_alpha));
            }
            Readout::Empty => {
                let caption = std::ffi::CString::new(plx_platform::i18n::msg::recommended_page_empty()).unwrap_or_default();
                let frame = Rect::new(MARGIN_X, CONTENT_TOP + TITLE_BLOCK_H, SCR_W - 2.0 * MARGIN_X, SCR_H - (CONTENT_TOP + TITLE_BLOCK_H) - MARGIN_Y);
                plx_ui::widgets::StatusOverlay::new(frame, &caption, plx_ui::widgets::StatusKind::Empty)
                    .phase(f.cx.tick.ms).draw_measured(&env, f.painter.alpha(f.page_alpha), f.cx.measure);
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
        // Strip, then the crumb's link back, then the rows: one column down the page.
        out.push(Link { from: STRIP, dir: Dir::Down, to: CRUMB_GROUP });
        out.push(Link { from: CRUMB_GROUP, dir: Dir::Up, to: STRIP });
        if self.shelves.is_empty() { return; }
        out.push(Link { from: CRUMB_GROUP, dir: Dir::Down, to: self.first_group() });
        out.push(Link { from: self.first_group(), dir: Dir::Up, to: CRUMB_GROUP });
        // One continuous document: a linked heading is its own block directly above its cards, so
        // UP from a card reaches it and DOWN returns to the remembered card.
        let mut document = Vec::new();
        for row in &self.shelves {
            if let Some((heading, _)) = row.heading { document.push(heading); }
            document.push(row.group);
        }
        for pair in document.windows(2) {
            out.push(Link { from: pair[0], dir: Dir::Down, to: pair[1] });
            out.push(Link { from: pair[1], dir: Dir::Up, to: pair[0] });
        }
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
        self.ground.write_motion(c);
        PageMemory::Library(self.page_memory()).write(c);
        c.seq(self.viewports.len());
        for viewport in &self.viewports { viewport.write(c); }
        c.seq(self.shelves.len());
        for row in &self.shelves {
            let Row { id, group, heading, elems, landscape, cards } = row;
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
        self.shelf.map_or_else(|| ui_cards::TileLabel::title(""), |shelf| shelf_label(shelf, i))
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
    let Some(item) = shelf.items.get(col) else { return ui_cards::TileLabel::title("") };
    if shelf.landscape {
        let name = if item.title.is_empty() || item.title == item.show_title {
            plx_ui::fmt::episode_address(item.season_index as i64, item.ep_index as i64)
        } else { item.title.clone() };
        let fact = if shelf.is_continue && item.resume_frac().is_some() {
            plx_ui::fmt::time_left(item.dur_ns / 1_000_000 - item.resume_ms)
        } else if item.aired.is_empty() && item.year <= 0 { String::new() }
        else { plx_ui::fmt::pretty_date(&item.aired, item.year as i64) };
        return if fact.is_empty() { ui_cards::TileLabel::title(&name) }
        else { ui_cards::TileLabel::titled(&name, &fact) };
    }
    let mut label = if shelf.is_continue && plx_media::route::deck_press().press_plays() { ui_cards::TileLabel::played(&item.title) }
        else { ui_cards::TileLabel::title(&item.title) };
    label.caption = ui_cards::focused_caption(&tile_facts::of(item), shelf.is_continue);
    label
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
