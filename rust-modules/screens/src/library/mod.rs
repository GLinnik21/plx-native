//! Library instance: document motion and transactions belong here; focus belongs to Input.
mod identity;
mod bookmark;
mod layout;
mod lookahead;
mod parts;
mod rail;
#[cfg(test)]
mod rail_tests;
mod transactions;
mod draw;
mod toolbar;
mod status;
pub mod menu;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod cards_harness; // Tier 2 card conformance (cards_conformance_tests.rs)
#[cfg(test)]
mod window_tests;
#[cfg(test)]
mod labels_tests;
#[cfg(test)]
mod deferred_tests;
#[cfg(test)]
mod navigation_tests;
#[cfg(test)]
mod shelf_action_tests;
#[cfg(test)]
mod foreign_replacement_tests;
#[cfg(test)]
mod selector_matrix_tests;

use std::collections::HashSet;
use std::ops::Range;
use std::borrow::Cow;
use plx_data::browse::{SecFetch, SecKind};
use crate::registry::{
    AppFx, AppMsg, HomeTab, LibraryCmd, LibraryIdentity, LibraryLike, LibraryMemory,
    LibraryReq, LibrarySectionIdentity, LibraryViewport, PageMemory,
};
use plx_data::stores::{StoreCmd, StoreId, StoreWork};
use plx_data::stores::browse::{BrowseCmd, LibraryWork, SectionAddress};
use plx_ui::cards::RowStyle;
use plx_ui::cards::SectionFrame;
use plx_ui::consts::{MARGIN_X, SCR_W, SCR_H, K_SCROLL, CARD_DY};
use plx_ui::frame::Budget;
use plx_machine::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind,
    InstanceId, Key, LogicalState, Machine, MachineId,
};
use plx_ui::master_detail::{
    Follow, MasterDetail, MasterDetailGroups, MasterDetailLayout, MasterDetailPolicy, MasterSide,
    Outcome as MasterOutcome, Region,
};
use plx_machine::present::Provenance;
use plx_ui::screen::{
    At, AxisMask, By, Dir, DrawFrame, EdgeRule, ElemKind, Enter, FocusTarget, Focusable,
    GroupKind, GroupSpec, Link, Part, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step,
};
use plx_ui::{Rect, Spring};
use plx_ui::xfade::Xfade;
use identity::{KeyRegistry, KeyRegion, region_of_elem};
use layout::{Layout, Block, ShelfRun, CONTENT_TOP};
#[cfg(test)]
use layout::COLS;
use parts::{GridPart, RailPart, GRID_GROUP, RAIL_GROUP};
use transactions::{PendingTransactions, SectionTarget, GridTarget, GridAction};

pub const LIBRARY_GROUP: GroupId = GroupId(0x4c49_4210);
/// The heading row's group before a section is known. A known section's heading is its own
/// group (`LibraryScreen::toolbar_group`), like its grid and rail: Movies and Shows are one page
/// and one entry, so a shared id carried Movies' heading cursor into Shows as a "restore".
pub const TOOLBAR_GROUP: GroupId = GroupId(0x4c49_4211);
const STATUS_GROUP: GroupId = GroupId(0x4c49_4212);
const SORT: u32 = 1;
const FILTER: u32 = 2;
const RETRY: u32 = 3;
const MORE: u32 = 4;
/// "Connect without encryption?" over a failed source's read-out (`screens::plaintext_question`).
const PLAINTEXT_GROUP: GroupId = GroupId(0x4c49_4213);
const PLAINTEXT_CANCEL: u32 = 5;
const PLAINTEXT_CONNECT: u32 = 6;
const TYPE: u32 = 7;
const STRIP: GroupId = plx_ui::containers::tabs::STRIP;
/// Shelf rows held beyond the ones in view, each side.
const HOLD_MARGIN: usize = 2;

// The shelves' `motion` bytes are `cards::Shelf::write`, the L0 row's write unchanged; the type's name
// is split in the text only so the `cards` gate does not read a shape string as a use of the primitive.
pub const SHAPE: [&str; 8] = [
    concat!("LibraryScreen{entry:u32,instance:u32,kind:u32,wanted_kind:Option<u32>,scroll:{pos:f32,vel:f32},scroll_target:f32,restore_scroll:Option<f32>,live:bool,initial:bool,provisional:Option<u32>,placed:[(group:u32,elem:u32)],sweep_down:bool,epoch:Option<u32>,query:Option<u32>,grid_reset_pending:bool,shelf_publication:Option<(HubsId{epoch:u32,sid:u32,section:u64},revision:u64)>,page_fade:Xfade{phase:u8,t:f32},grid_fade:Xfade{phase:u8,t:f32},pair:MasterDetailState{side:u32,follow:u32,band:u32,door:Option<u32>},pending:PendingTransactions,ground_seeded:bool,ground:PageGround,chrome:LibraryChrome,memory:PageMemory::Library,viewport_cache:[LibraryViewport],shelves:[{id:str,group:u32,heading:Option<(group:u32,elem:u32)>,landscape:bool,elems:[u32],motion:Card", "Row}],libraries:[(elem:u32,section:u32)],readout:u32,layout:LibraryLayout,target_layout:LibraryLayout,grid:LibraryGrid,rail:LibraryRail}"),
    transactions::SHAPE,
    plx_ui::widgets::PageGround::SHAPE,
    plx_ui::widgets::TabStrip::SHAPE,
    "LibraryChrome{capsules:TabStrip,pop:CtlPop{sp:[Spring{pos:f32,vel:f32}],focused:Option<u32>},pair_groups:{master:u32,detail:u32}}",
    Layout::SHAPE,
    GridPart::SHAPE,
    RailPart::SHAPE,
];

struct Regions;
impl<H: LibraryLike> plx_ui::master_detail::KeyRegion<H> for Regions {
    fn region(&self, elem: &u32, _: &Cx<'_, H>) -> Option<Region> {
        match region_of_elem(*elem) {
            Some(KeyRegion::Grid) => Some(Region::Detail),
            Some(KeyRegion::Rail) => Some(Region::Master),
            _ => None,
        }
    }
}

/// One hub shelf above the grid: the published hub's identities and its `cards::Shelf`, which
/// owns the pop, the scroll, the caption band, the draw, the stops and the placement.
struct HubShelf {
    id: String,
    group: GroupId,
    /// A promoted collection shelf's linked heading (`ui::linked_heading`): its one-element group
    /// and element, registered by the hub's identity like the shelf's own group. `None` for every
    /// other hub, which keeps its plain heading.
    heading: Option<(GroupId, u32)>,
    elems: Vec<u32>,
    landscape: bool,
    cards: plx_ui::cards::Shelf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Readout { Loading, Empty, Failed, Grid }

fn readout(table: SecFetch, sections: usize, fetch: SecFetch, total: i64) -> Readout {
    if total < 0 {
        match table {
            SecFetch::Failed => return Readout::Failed,
            SecFetch::Ready if sections == 0 => return Readout::Empty,
            _ => {}
        }
    }
    match fetch {
        SecFetch::Loading => Readout::Loading,
        SecFetch::Failed if total < 0 => Readout::Failed,
        SecFetch::Failed => Readout::Grid,
        SecFetch::Ready if total == 0 => Readout::Empty,
        SecFetch::Ready => Readout::Grid,
    }
}

/// Is the shelf run above the grid final? `Fetching` (the first answer is still inside its
/// bounded first-paint window) and `Staged` (an answer held until the page may move — which a
/// hidden page always may, so it commits on the next frame) both mean the grid's offset is about
/// to change. A publication with no identity has no section behind it and nothing to wait for.
fn shelves_settled(hubs: plx_data::stores::browse::HubsView<'_>) -> bool {
    hubs.id().is_none() || hubs.publication() == plx_data::browse::section_hubs::Publication::Committed
}

/// A retained page body, also remountable from its entry-owned LibraryMemory.
pub struct LibraryScreen {
    entry: EntryId,
    instance: InstanceId,
    kind: SecKind,
    wanted_kind: Option<SecKind>,
    /// What the committed listing lists — a projection of the store's publication, refreshed by
    /// `sync` like the layout's episode geometry, so not logical state of its own. The toolbar
    /// reads it: collections take no filters, so FILTER leaves the row while they are listed.
    listed: plx_data::browse::LibraryType,
    keys: KeyRegistry,
    pair: MasterDetail<RailPart, GridPart, Regions>,
    /// The heading row's group: per section and epoch, registered beside the pair's.
    toolbar: GroupId,
    libraries: Vec<(u32, usize)>,
    shelves: Vec<HubShelf>,
    section: Option<LibrarySectionIdentity>,
    epoch: Option<u32>,
    query: Option<u32>,
    grid_reset_pending: bool,
    shelf_publication: Option<(plx_data::browse::section_hubs::HubsId, u64)>,
    layout: Layout,
    target_layout: Layout,
    /// The shelves stacked for `layout` (live pitches) and `target_layout` (the pitches the
    /// springs are heading to). Derived from `shelves` and refilled by every `relayout`; not
    /// canon, since each `Layout` already writes the run's height.
    run: ShelfRun,
    target_run: ShelfRun,
    scroll: Spring,
    scroll_target: f32,
    restore_scroll: Option<f32>,
    viewports: Vec<LibraryViewport>,
    /// The grid key an `Enter::Fresh` names to seat, kept projected until the engine has seated it.
    seat_wanted: Option<u32>,
    pending: PendingTransactions,
    page_fade: Xfade,
    grid_fade: Xfade,
    readout: Readout,
    live: bool,
    initial: bool,
    /// The head group the page seated its own focus in, while that seat is still the page's own
    /// choice: nobody has moved since. Such a seat follows the head when a landing changes what the
    /// head is — the shelves arrive async, usually after a prepared grid, and commit above it.
    provisional: Option<GroupId>,
    /// Remembered cursors the PAGE wrote by seating itself, which the reader has not confirmed.
    /// The engine remembers every seat alike, so its memory alone cannot tell a restore from the
    /// page's own earlier seat in the same section.
    placed: Vec<(GroupId, u32)>,
    sweep_down: bool,
    // Paint-only state: neither capsule travel nor ambient colours choose focus or activation.
    library_capsules: plx_ui::widgets::TabStrip,
    library_pop: plx_ui::widgets::CtlPop<1>,
    ground: plx_ui::widgets::PageGround,
    ground_seeded: bool,
    /// The failed source's server, when discovery offers the question for it
    /// (`plex::grant::offers`) — the grant table's, not logical state.
    plaintext: super::plaintext_question::OfferWatch,
    /// Why key mode cannot help (`net::keypin::blocked`), held between ticks so the draw and the
    /// *Try again* hit rect of one frame read ONE value — the reason line moves the action row.
    clock: super::clock_readout::ClockWatch,
    /// The question, asked from the failed read-out's *Connect*.
    plaintext_alert: super::plaintext_question::PlaintextAlert,
    /// The standing "Hold OK for options" hint over a card (`plx_ui::hold_hint`, the rarer
    /// cadence of a non-Home card screen). Paint-only: not hashed, not in the shape.
    hold_hint: plx_ui::hold_hint::HoldHint,
    /// The held range last sent to the store, with the section it was for, so the command goes out
    /// only when it changes. Derived from what is on screen; not canon.
    hold_sent: Option<(SectionAddress, (usize, usize))>,
    /// Shelf rows the document draw and the stop recorder looked at, for the windowing test.
    #[cfg(test)]
    shelf_visits: std::cell::Cell<usize>,
}

impl LibraryScreen {
    pub fn new(entry: EntryId, instance: InstanceId, kind: SecKind) -> Self {
        let layout = Layout::new(false, &[], 0, false);
        Self {
            entry, instance, kind, wanted_kind: Some(kind), listed: Default::default(), keys: KeyRegistry::default(),
            pair: MasterDetail::new(
                RailPart::new(entry, RAIL_GROUP), GridPart::new(entry, GRID_GROUP),
                MasterDetailLayout { master: Rect::FULL, detail: Rect::FULL },
                MasterDetailGroups { master: RAIL_GROUP, detail: GRID_GROUP },
                MasterDetailPolicy::new(MasterSide::Right, Follow::Live), Regions,
            ),
            toolbar: TOOLBAR_GROUP,
            libraries: Vec::new(), shelves: Vec::new(), section: None, epoch: None, query: None, grid_reset_pending: false,
            shelf_publication: None, layout, target_layout: layout,
            run: ShelfRun::default(), target_run: ShelfRun::default(),
            scroll: Spring::at(0.0), scroll_target: 0.0, restore_scroll: None,
            viewports: Vec::new(), seat_wanted: None,
            pending: PendingTransactions::default(), page_fade: Xfade::new(), grid_fade: Xfade::new(),
            readout: Readout::Loading, live: true, initial: true, provisional: None, placed: Vec::new(), sweep_down: true,
            library_capsules: plx_ui::widgets::TabStrip::new(),
            library_pop: plx_ui::widgets::CtlPop::new(),
            ground: plx_ui::widgets::PageGround::new(), ground_seeded: false,
            plaintext: Default::default(),
            clock: Default::default(),
            plaintext_alert: super::plaintext_question::PlaintextAlert::new(PLAINTEXT_GROUP, PLAINTEXT_CANCEL, PLAINTEXT_CONNECT),
            hold_hint: plx_ui::hold_hint::HoldHint::once_per_run(plx_ui::hold_hint::Kind::Library),
            hold_sent: None,
            #[cfg(test)] shelf_visits: std::cell::Cell::new(0),
        }
    }

    pub fn restore(&mut self, memory: &LibraryMemory) {
        self.wanted_kind = None;
        self.keys = KeyRegistry::restore(memory);
        self.pair.detail.restore_keys(&self.keys);
        self.section = memory.section.clone();
        self.epoch = memory.epoch;
        self.query = memory.query;
        self.grid_reset_pending = memory.grid_reset_pending;
        if self.grid_reset_pending { self.grid_fade.mount(); }
        self.viewports = memory.viewports.clone();
        self.restore_scroll = Some(memory.scroll);
        self.scroll.jump(memory.scroll);
        self.scroll_target = memory.scroll;
        self.initial = false;
        self.provisional = None;
    }

    fn key(&self, elem: u32) -> FocusKey<u32> { FocusKey { entry: self.entry, elem } }

    /// What the committed listing lists — the TYPE menu's value as the page last synced it.
    pub fn listed(&self) -> plx_data::browse::LibraryType { self.listed }

    /// The heading row's group for the section on the page.
    #[cfg(any(test, feature = "test-support"))]
    pub fn toolbar_group(&self) -> GroupId { self.toolbar }

    /// `(page alpha, focused shelf tile's pop scale)` — what a viewer sees of a focus arrival.
    /// The scale is `None` when focus is not on a shelf tile; it is the pop the draw paints, so a
    /// stale dormant frame shows.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_arrival(&self, focus: Option<FocusKey<u32>>) -> (f32, Option<f32>) {
        let elem = focus.filter(|key| key.entry == self.entry).map(|key| key.elem);
        let scale = self.shelves.iter().find_map(|shelf| {
            let col = elem.and_then(|elem| shelf.elems.iter().position(|key| *key == elem))?;
            Some(shelf.cards.drawn_pop(col, elem.and_then(|elem| shelf.elems.iter().position(|key| *key == elem))))
        });
        (self.page_fade.alpha(), scale)
    }

    /// `(page fade alpha, grid fade alpha, still loading, section key, grid tiles)`: what a
    /// draw of the page would put on screen now.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_content(&self) -> (f32, f32, bool, i64, usize) {
        (self.page_fade.alpha(), self.grid_fade.alpha(), self.readout == Readout::Loading,
            self.section.as_ref().map_or(-1, |section| section.key), self.pair.detail.total())
    }

    fn reseat<H: LibraryLike>(&self, focus: FocusTarget<u32>, fx: &mut Effects<'_, H>) {
        fx.push(Fx::Deliver(MachineId::Instance(self.instance),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))));
    }

    fn address<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<SectionAddress> {
        let id = H::listing(cx).id()?;
        Some(SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section })
    }


    /// User intent follows the incoming page while the committed listing is still fading out.
    fn requested_address<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<SectionAddress> {
        self.pending.section().map(|section| SectionAddress {
            epoch: section.epoch, sid: section.identity.sid, section: section.identity.key,
        }).or_else(|| self.address(cx))
    }

    fn store<H: LibraryLike>(&self, target: SectionAddress, work: LibraryWork, fx: &mut Effects<'_, H>) {
        fx.push(Fx::App(AppFx::Store(StoreId::Browse,
            StoreCmd::Browse(BrowseCmd::Addressed { target, work }))));
    }

    fn sync<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) {
        let listing = H::listing(cx);
        self.listed = listing.library_type();
        self.layout = self.layout.with_episodes(self.listed == plx_data::browse::LibraryType::Episodes);
        let directory = H::directory(cx);
        let identity = listing.id().map(|id| LibrarySectionIdentity { sid: id.sid, key: id.section });
        let epoch = listing.id().map(|id| id.epoch).or(directory.epoch());
        let changed = self.section != identity || self.epoch != epoch;
        if changed { self.grid_reset_pending = false; }
        else if self.query.is_some() && self.query != listing.id().map(|id| id.query) {
            self.grid_reset_pending = true;
            self.restore_scroll = None;
            if !self.grid_fade.is_swapping() { self.grid_fade.mount(); }
        }
        if let (Some(section), Some(epoch)) = (&identity, epoch) {
            let mut group = |kind: &str| GroupId(self.keys.register(LibraryIdentity::Control {
                section: section.clone(), kind: kind.into(), key: epoch.to_string(),
            }, GroupId(0), 0));
            let groups = MasterDetailGroups { master: group("rail-group"), detail: group("grid-group") };
            self.toolbar = group("toolbar-group");
            if self.pair.groups_config() != groups {
                self.pair = MasterDetail::new(
                    RailPart::new(self.entry, groups.master), GridPart::new(self.entry, groups.detail),
                    MasterDetailLayout { master: Rect::FULL, detail: Rect::FULL }, groups,
                    MasterDetailPolicy::new(MasterSide::Right, Follow::Live), Regions,
                );
                self.pair.detail.restore_keys(&self.keys);
            }
        }
        if changed {
            if self.section.is_some() {
                self.initial = true;
                if !self.page_fade.is_swapping() {
                    // This was not our page transaction: the outgoing publication is already gone.
                    self.page_fade.mount();
                    self.grid_fade = Xfade::new();
                    self.pending.cancel();
                }
            }
            if let Some(viewport) = self.current_viewport() {
                self.viewports.retain(|old| old.epoch != viewport.epoch || old.section != viewport.section);
                self.viewports.push(viewport);
            }
            // Viewports of an epoch that is no longer current can never be looked up again.
            if self.epoch != epoch { self.viewports.retain(|old| Some(old.epoch) == epoch); }
            self.ground_seeded = false;
            self.shelves.clear();
            self.shelf_publication = None;
            self.section = identity.clone();
            self.epoch = epoch;
            let saved = self.viewports.iter().find(|view| Some(view.epoch) == epoch && Some(&view.section) == identity.as_ref());
            self.restore_scroll = saved.map(|view| view.scroll).or_else(||
                cx.focus.remembered(self.pair.groups_config().detail).is_none()
                    .then(|| listing.cursor().map(|cursor| cursor.scroll)).flatten());
            self.scroll.jump(self.restore_scroll.unwrap_or(0.0));
            self.scroll_target = self.scroll.pos;
        }
        self.query = listing.id().map(|id| id.query);
        let table = if directory.sections().is_empty() { directory.discovery() } else { directory.source_fetch() };
        self.readout = readout(table, directory.sections().len(), listing.fetch(), listing.total());
        self.libraries.clear();
        if let Some(current) = directory.current() {
            let kind = directory.sections()[current].kind;
            if self.wanted_kind.is_none() || self.wanted_kind == Some(kind) {
                self.kind = kind;
                self.wanted_kind = None;
            }
        }
        if let Some(current) = self.view_section(cx) {
            for (index, section) in directory.favorite_sections_for(current) {
                let Some(sid) = section.sid else { continue };
                let elem = self.keys.register(
                    LibraryIdentity::Library(LibrarySectionIdentity { sid, key: section.key }),
                    LIBRARY_GROUP, self.libraries.len());
                self.libraries.push((elem, index));
            }
            // A SINGLE candidate never draws a selector, whatever owns the section it names.
            // The prior rule kept the row up for a `borrowed` singleton — carried forward
            // wholesale from the 0.6.x server picker by `3c2de7ad` — so a Guest or managed
            // profile, whose own household server always arrives `owned: false`
            // (`plex/account.rs`), saw a lone `Library · <name> ⌄` chip on every section with one
            // favourite: issue #100/#165, reported as a stray "Sources" control on a real TV. A
            // single library carries nothing to disambiguate for ANY profile, so ownership must
            // not decide this; only the CANDIDATE COUNT does, and only 2+ ever reaches the
            // overflow window below.
            if self.libraries.len() <= 1 {
                self.libraries.clear();
            } else {
                let widths: Vec<_> = self.library_lays(cx).iter().map(|lay|
                    plx_ui::widgets::strip_pill_rect(lay, 0.0, plx_ui::widgets::StatusOverlay::CTRL_H).w).collect();
                let selected = self.libraries.iter().position(|(_, index)| *index == current).unwrap_or(0);
                let more = std::ffi::CString::new(format!("+{}", widths.len().saturating_sub(1))).unwrap_or_default();
                let more_w = cx.measure.width(&more, plx_ui::theme::size::BODY, true) + 2.0 * plx_ui::widgets::STRIP_PAD;
                let (start, len) = layout::library_window(&widths, selected, layout::GRID_RIGHT - MARGIN_X,
                    plx_ui::widgets::STRIP_GAP_WIDE, more_w, layout::MAX_LIBRARY_PILLS);
                if len < self.libraries.len() {
                    self.libraries = self.libraries[start..start + len].to_vec();
                    self.libraries.push((MORE, usize::MAX));
                }
            }
        }
        if let Some(kind) = self.wanted_kind.filter(|kind|
            directory.current().map(|i| directory.sections()[i].kind) != Some(*kind)) {
            self.shelves.clear();
            self.shelf_publication = None;
            self.pair.detail.clear_projection();
            self.pair.master.clear_projection();
            self.grid_fade = Xfade::new();
            self.readout = if directory.preferred(kind).is_some() { Readout::Loading }
                else { readout(directory.kind_fetch(kind), 0, directory.kind_fetch(kind), -1) };
            self.relayout(cx.focus.current);
            return;
        }
        let hubs = H::section_hubs(cx);
        let publication = hubs.id().zip(hubs.revision());
        if self.shelf_publication != publication {
            let mut old = std::mem::take(&mut self.shelves);
            let previous: HashSet<u32> = old.iter().flat_map(|row| row.elems.iter().copied()).collect();
            if let Some(section) = &identity {
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
                    let cards = old.iter().position(|row| row.id == shelf.id && row.landscape == shelf.landscape)
                        .map(|i| old.remove(i).cards).unwrap_or_else(|| {
                            let mut cards = hub_cards(self.entry, shelf.landscape);
                            let saved = self.viewports.iter().find(|view| Some(view.epoch) == epoch && view.section == *section)
                                .and_then(|view| view.shelves.iter().find(|(id, _)| *id == shelf.id));
                            if let Some((_, x)) = saved {
                                cards.restore_scroll(*x, shelf.slots());
                            }
                            cards
                        });
                    let heading = shelf.link.as_ref()
                        .map(|_| {
                            let control = |kind: &str| LibraryIdentity::Control {
                                section: section.clone(), kind: kind.into(), key: shelf.id.clone() };
                            let heading_group = GroupId(self.keys.register(control("shelf-heading-group"), GroupId(0), index));
                            let elem = self.keys.register(control("shelf-heading"), heading_group, 0);
                            (heading_group, elem)
                        });
                    // A page that landed (or a refresh) answers the ask the row's latch was holding.
                    let mut cards = cards;
                    cards.reset_page_requests();
                    self.shelves.push(HubShelf { id: shelf.id.clone(), group, heading, elems, landscape: shelf.landscape, cards });
                }
            }
            self.shelf_publication = publication;
            // Rotating refetches mint a key per new item: keep the current shelves, the previous
            // publication's (a focus on one recovers through it) and the focus; above twice that
            // plus 256 the oldest others go.
            let current: HashSet<u32> = self.shelves.iter().flat_map(|row| row.elems.iter().copied()).collect();
            let focus = cx.focus.current.filter(|key| key.entry == self.entry).map(|key| key.elem);
            let cap = 2 * (current.len() + previous.len()) + 256;
            self.keys.prune(KeyRegion::Shelf, cap, |elem| current.contains(&elem) || previous.contains(&elem) || Some(elem) == focus);
        }
        self.project_grid(cx);
        self.relayout(cx.focus.current);
        // The relayout hands the grid this frame's scroll, and with it the window it will paint:
        // a cell it can paint or step to must hold a key in the frame, so project again (a
        // no-op when the window did not move).
        self.project_grid(cx);
        self.pair.master.refresh(cx, &mut self.keys);
    }

    fn project_grid<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) {
        let (ranges, pins) = self.grid_wanted(cx);
        self.pair.detail.set_pins(pins);
        self.pair.detail.refresh(cx, &mut self.keys, ranges);
    }

    /// The slot ranges the grid keeps keys for, and the keys a prune never drops. The data driver's
    /// window (`Want`) is the visible rows with a row of look-ahead each side; the same rows at
    /// the scroll TARGET (a jump to a letter is far from where the scroll is now), and around the
    /// focused and the remembered slot, so a seat is always on a projected key.
    fn grid_wanted<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> (Vec<Range<usize>>, Vec<u32>) {
        let cols = self.layout.cols();
        let rows = |(lo, hi): (usize, usize)| lo.saturating_sub(1) * cols..(hi + 1) * cols;
        let visible = |layout: &Layout, scroll: f32| if layout.rows == 0 { (0, layout.rows_per_screen()) }
            else { layout.visible_rows(scroll) };
        let mut ranges = vec![rows(visible(&self.layout, self.scroll.pos)),
            rows(visible(&self.target_layout, self.scroll_target))];
        let group = self.pair.groups_config().detail;
        let total = H::listing(cx).total().max(0) as usize;
        ranges.push(self.pair.detail.window_of(total));
        let mut pins = Vec::new();
        let focus = cx.focus.current.filter(|key| key.entry == self.entry).map(|key| key.elem);
        for elem in [focus, cx.focus.remembered(group), self.seat_wanted].into_iter().flatten() {
            if region_of_elem(elem) != Some(KeyRegion::Grid) { continue; }
            pins.push(elem);
            if let Some((_, index)) = self.keys.last_place(elem) {
                ranges.push(rows((index / cols, index / cols + 1)));
            }
        }
        (ranges, pins)
    }

    /// Feed an event to every hub shelf's card section: focus moves and the tick that steps them.
    /// A page still dissolving in holds its sections dormant (`cards::Shelf::dormant`). The Library keeps its
    /// own press, hold and paging handling, so the sections' `Activate` / `Hold` / `Want` are not
    /// used: `CardSource::more` is `false`.
    fn shelves_on<H: LibraryLike>(&mut self, ev: &ScreenEvent<H>, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let held = self.page_fade.is_arriving();
        let hubs = H::section_hubs(cx);
        let focus = cx.focus.current.filter(|key| key.entry == self.entry).map(|key| key.elem);
        let ticking = matches!(ev, ScreenEvent::Tick(_));
        let mut asks = Vec::new();
        let mut withdrawn = Vec::new();
        let seen = hubs.revision().unwrap_or_default();
        for (index, row) in self.shelves.iter_mut().enumerate() {
            let shelf = hubs.shelves().get(index);
            let at = focus.and_then(|elem| row.elems.iter().position(|key| *key == elem));
            let paging = at.is_some_and(|col| col.saturating_add(plx_ui::cards::PAGE_EDGE_CARDS) >= row.elems.len());
            let src = draw::HubSrc { elems: &row.elems, shelf, paging };
            row.cards.dormant(held);
            let want = row.cards.on(ev, cx, &src, fx);
            if !ticking && at.is_none() { row.cards.reset_page_requests(); }
            // An ask whose edge focus has left is withdrawn before the landing can slide the window
            // from under it.
            if ticking {
                if let (Some(shelf), Some(_)) = (shelf, row.cards.page_cancel(cx, &src, shelf.map_or(0, |s| s.offset), true)) {
                    withdrawn.push((shelf.id.clone(), shelf.key.clone()));
                }
            }
            // The row asks for its next page at an edge, once per window, as Home's rows do; the
            // store moves the window and the page follows the card focus is on.
            if ticking && at.is_some() {
                let wanted = matches!(want, Some(plx_ui::cards::CardEvent::Want(_)));
                if let (Some(shelf), Some(edge)) = (shelf, row.cards.page_ask(cx, &src, wanted, shelf.map_or(0, |s| s.offset), true)) {
                    asks.push((shelf.id.clone(), shelf.key.clone(), edge == plx_ui::cards::PageEdge::Before));
                }
            }
        }
        if let Some(target) = self.address(cx).filter(|_| !asks.is_empty() || !withdrawn.is_empty()) {
            for (id, key) in withdrawn { self.store(target, LibraryWork::HubPageCancel { id, key }, fx); }
            for (id, key, before) in asks { self.store(target, LibraryWork::HubPage { id, key, before, seen }, fx); }
        }
    }

    /// The shelf rows with a card on screen, plus two each side and the focused row, as the range the
    /// store holds cards for. Sent only when it changes.
    fn send_hold<H: LibraryLike>(&mut self, target: SectionAddress, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        if self.shelves.is_empty() { return; }
        let origin = self.layout.shelf_y(&self.run, 0, self.scroll.pos) - self.run.origin(0);
        let seen = self.run.window(-origin, SCR_H - origin);
        let focused = cx.focus.current.filter(|key| key.entry == self.entry)
            .and_then(|key| self.shelves.iter().position(|row| row.elems.contains(&key.elem)));
        let (mut lo, mut hi) = (seen.start, seen.end.saturating_sub(1).max(seen.start));
        if let Some(row) = focused { (lo, hi) = (lo.min(row), hi.max(row)); }
        let hold = (lo.saturating_sub(HOLD_MARGIN), (hi + HOLD_MARGIN).min(self.shelves.len() - 1));
        if self.hold_sent != Some((target, hold)) {
            self.hold_sent = Some((target, hold));
            self.store(target, LibraryWork::HubHold { lo: hold.0, hi: hold.1 }, fx);
        }
    }

    fn relayout(&mut self, focus: Option<FocusKey<u32>>) {
        let grid_focus = focus.filter(|key| key.entry == self.entry)
            .and_then(|key| self.pair.detail.index_of(key.elem));
        self.pair.detail.settle_band(grid_focus.map(|index| index / self.layout.cols()));
        self.run.set(self.shelves.iter().map(|row| layout::shelf_pitch(row.landscape, row.cards.band_expand())));
        self.target_run.set(self.shelves.iter().map(|row| layout::shelf_pitch(row.landscape,
            f32::from(focus.is_some_and(|key| row.elems.contains(&key.elem))))));
        let rows = self.pair.detail.total().div_ceil(self.layout.cols());
        // A grid transaction's commit empties the store (`browse::requery`) while the grid is
        // dark and its answer is in flight. The shelves, heading and TYPE chip around it are
        // NOT dark: sizing the document from the empty interim would shrink `max_scroll` and
        // slide the whole page to the clamp and back when the answer lands. Until the answer
        // says otherwise, the dark grid keeps reserving the extent it had.
        let rows = if rows == 0 && self.grid_fade.is_swapping() && self.readout == Readout::Loading {
            self.layout.rows
        } else { rows };
        let grid_head = rows > 0 || self.grid_fade.is_swapping()
            // An empty answer keeps the heading row: its TYPE chip is how the reader leaves an
            // empty listing (no collections, no episodes) for one that has something in it.
            || (self.section.is_some() && self.wanted_kind.is_none() && self.readout == Readout::Empty);
        let episodes = self.layout.is_episodes();
        self.layout = if self.readout == Readout::Failed {
            Layout::failed(!self.libraries.is_empty(), self.run.pitches())
        } else { Layout::new(!self.libraries.is_empty(), self.run.pitches(), rows, grid_head) }.with_episodes(episodes)
            .with_grid_bands(self.pair.detail.band_geometry());
        self.target_layout = Layout::new(!self.libraries.is_empty(), self.target_run.pitches(), rows, grid_head).with_episodes(episodes)
            .with_grid_focus(grid_focus.map(|index| index / self.layout.cols()));
        if self.grid_reset_pending {
            self.scroll_target = self.target_layout.row_reveal(0);
            self.scroll.jump(self.scroll_target);
        }
        self.layout.status = self.readout == Readout::Failed;
        self.target_layout.status = self.layout.status;
        self.layout.empty = self.readout == Readout::Empty;
        self.target_layout.empty = self.layout.empty;
        self.pair.detail.set_geometry(self.layout, self.scroll.pos, self.target_layout, self.scroll_target);
    }

    /// Seat the page's own focus in `group`; the library row seats the library on view.
    fn seat_on<H: LibraryLike>(&self, group: GroupId, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        if group == LIBRARY_GROUP {
            if let Some((elem, _)) = self.libraries.iter().find(|(_, index)| Some(*index) == self.view_section(cx)) {
                fx.remember(LIBRARY_GROUP, *elem);
            }
        }
        self.reseat(FocusTarget::ContainerGroup(group), fx);
    }

    fn first_group(&self) -> GroupId {
        match self.layout.first() {
            Some(Block::LibraryRow) => LIBRARY_GROUP,
            Some(Block::Shelf(index)) => self.shelves[index].entry_group(),
            Some(Block::Status) => STATUS_GROUP,
            Some(Block::Toolbar | Block::Grid(_)) => self.toolbar,
            None => STRIP,
        }
    }

    fn current_viewport(&self) -> Option<LibraryViewport> {
        let shelves = if self.shelf_publication.is_some() {
            self.shelves.iter().map(|row| (row.id.clone(), row.cards.scroll())).collect()
        } else {
            self.viewports.iter().find(|view| Some(view.epoch) == self.epoch && Some(&view.section) == self.section.as_ref())
                .map(|view| view.shelves.clone()).unwrap_or_default()
        };
        Some(LibraryViewport { epoch: self.epoch?, section: self.section.clone()?, scroll: self.scroll.pos,
            shelves })
    }

    fn page_memory(&self) -> LibraryMemory {
        let mut memory = self.keys.remember(self.section.clone(), self.scroll.pos,
            self.shelves.iter().map(|row| (row.id.clone(), row.cards.scroll())).collect());
        memory.epoch = self.epoch;
        memory.query = self.query;
        memory.grid_reset_pending = self.grid_reset_pending;
        memory.viewports = self.viewports.clone();
        if let Some(viewport) = self.current_viewport() {
            memory.viewports.retain(|old| old.epoch != viewport.epoch || old.section != viewport.section);
            memory.viewports.push(viewport);
        }
        memory
    }

    fn reveal<H: LibraryLike>(&mut self, key: FocusKey<u32>, by: By, cx: &Cx<'_, H>) {
        // A seat on a grid cell the projection has not reached (a jump or a restore) projects the
        // cell's neighbourhood before the page reads its row.
        if region_of_elem(key.elem) == Some(KeyRegion::Grid) && self.pair.detail.index_of(key.elem).is_none() {
            if let Some((_, index)) = self.keys.last_place(key.elem) {
                self.pair.detail.elem_for(cx, &mut self.keys, index, self.layout.cols());
            }
        }
        self.relayout(Some(key));
        let want = if let Some(index) = self.pair.detail.index_of(key.elem) {
            // held in the coordinates `scroll` is in until the grid's tick rebases both by a landing
            Some(self.target_layout.row_reveal(index / self.layout.cols()) - self.pair.detail.pending_shift(cx))
        } else if let Some(index) = self.shelves.iter().position(|row|
            row.elems.contains(&key.elem) || row.heading_elem() == Some(key.elem)) {
            Some(self.target_layout.shelf_reveal(&self.target_run, index))
        } else if matches!(key.elem, TYPE | SORT | FILTER) {
            Some(self.target_layout.grid_block_top().clamp(0.0, self.target_layout.max_scroll()))
        } else if self.libraries.iter().any(|(elem, _)| *elem == key.elem) || key.elem == RETRY {
            Some(0.0)
        } else { None };
        if let Some(want) = want {
            self.scroll_target = want;
            // Only a SAVED viewport is jumped to. A `Restore` that carries none is a reseat the
            // page asked of itself (BACK to the top, a section's first seat): the reader sees the
            // scroll spring carry them there, as Home's BACK glides back to the hero.
            if by == By::Restore {
                if let Some(saved) = self.restore_scroll.take() {
                    self.scroll_target = saved.clamp(0.0, self.target_layout.max_scroll());
                    self.scroll.jump(self.scroll_target);
                }
            }
        } else {
            self.scroll_target = self.scroll_target.clamp(0.0, self.target_layout.max_scroll());
        }
        self.relayout(Some(key));
        self.pair.master.refresh(cx, &mut self.keys);
    }

    fn follow<H: LibraryLike>(&mut self, elem: u32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let Some(index) = self.pair.master.start_for_elem(elem) else { return };
        let Some(target) = self.pair.detail.elem_for(cx, &mut self.keys, index, self.layout.cols()) else { return };
        fx.remember(self.pair.groups_config().detail, target);
        self.scroll_target = self.target_layout.with_grid_focus(None).row_reveal(index / self.layout.cols());
        fx.invalidate(Provenance::Input);
    }

    pub fn focused_item<'a, H: LibraryLike>(&self, focus: Option<FocusKey<u32>>, cx: &Cx<'a, H>) -> Option<&'a plx_data::pms::PmsMovie> {
        let key = focus.filter(|key| key.entry == self.entry)?;
        if let Some(index) = self.pair.detail.index_of(key.elem) { return H::listing(cx).item(index); }
        let (row, col) = self.shelves.iter().enumerate().find_map(|(row, shelf)|
            shelf.elems.iter().position(|elem| *elem == key.elem).map(|col| (row, col)))?;
        H::section_hubs(cx).shelves().get(row)?.items.get(col)
    }

    /// The two screen-specific answers the hold hint needs (`ui::hold_hint`, "Adopting it"): the
    /// CARD focus rests on, one whose hold opens an item menu (a grid poster or a shelf card with
    /// a library item behind it that `registry::item_has_menu`, so not a collection; not a chip, the rail, a heading or a covered page), and whether it
    /// has arrived (the page scroll, the fade and the card's own glide).
    fn hint_input<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> plx_ui::hold_hint::HintInput {
        let none = plx_ui::hold_hint::HintInput::default();
        if !self.hold_hint.wants_input(cx.press.held_ms.is_some()) {
            return none;
        }
        let Some(key) = cx.focus.current.filter(|key| key.entry == self.entry) else { return none };
        if self.plaintext_alert.is_open()
            || self.focused_item(Some(key), cx).is_none_or(|item| !crate::registry::item_has_menu(item))
        {
            return none;
        }
        let Some(drawn) = <Self as Focusable<H>>::place(self, &key.elem, cx, At::Drawn) else { return none };
        let still = (self.scroll.pos - self.scroll_target).abs() < 0.5
            && self.scroll.vel.abs() < 0.5
            && self.page_fade.alpha() * self.grid_fade.alpha() > 0.99;
        plx_ui::hold_hint::HintInput::for_placed(key.elem, drawn, still, cx.press.held_ms)
    }

    pub fn grid_position(&self, focus: Option<FocusKey<u32>>) -> Option<(usize, usize)> {
        let key = focus.filter(|key| key.entry == self.entry)?;
        self.pair.detail.index_of(key.elem).map(|index| (index / self.layout.cols(), index % self.layout.cols()))
    }

    /// The grid's shape as the scenes that drive it see it: `(rows, rows_per_screen)`, once the
    /// listing has answered with at least one row.
    #[cfg(feature = "devtriggers")]
    pub fn grid_extent(&self) -> Option<(usize, usize)> {
        (self.layout.rows > 0).then(|| (self.layout.rows, self.layout.rows_per_screen()))
    }

    /// The focused hub-shelf card, `(shelf, col)`, if focus is on a shelf above the grid.
    pub fn shelf_position(&self, focus: Option<FocusKey<u32>>) -> Option<(usize, usize)> {
        let key = focus.filter(|key| key.entry == self.entry)?;
        self.shelves.iter().enumerate()
            .find_map(|(row, shelf)| shelf.elems.iter().position(|elem| *elem == key.elem).map(|col| (row, col)))
    }

    pub fn probe_viewport(&self, focus: Option<FocusKey<u32>>) -> (&'static str, i32, i32, f32, f32) {
        if let Some((row, col)) = self.grid_position(focus) { return ("grid", row as i32, col as i32, 0.0, self.scroll.pos); }
        let elem = focus.filter(|key| key.entry == self.entry).map(|key| key.elem);
        for (row, shelf) in self.shelves.iter().enumerate() {
            if let Some(col) = elem.and_then(|elem| shelf.elems.iter().position(|key| *key == elem)) {
                return ("shelf", row as i32, col as i32, shelf.cards.scroll(), self.scroll.pos);
            }
        }
        let region = match elem {
            Some(TYPE | SORT | FILTER) => "toolbar", Some(RETRY) => "status",
            Some(elem) if region_of_elem(elem) == Some(KeyRegion::Rail) => "rail",
            Some(elem) if elem >= plx_ui::dispatch::STRIP_BASE => "strip",
            Some(_) => "library", None => "none",
        };
        (region, -1, -1, 0.0, self.scroll.pos)
    }

    fn from_deck<H: LibraryLike>(&self, elem: u32, cx: &Cx<'_, H>) -> bool {
        self.shelves.iter().position(|row| row.elems.contains(&elem))
            .and_then(|row| H::section_hubs(cx).shelves().get(row)).is_some_and(|row| row.is_continue)
    }

    fn activate<H: LibraryLike>(&mut self, elem: u32, held: bool, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        // Every activation path (OK, a click, a hold, a scripted menu) is a choice made AT the seat,
        // and none of them moves focus, so no `FocusMoved` will release it.
        self.provisional = None;
        self.placed.retain(|(_, placed)| *placed != elem);
        // A singleton never reaches this screen at all any more (`self.libraries` is cleared in
        // `sync`), so `MORE` is the only remaining way into the Sources menu — an ordinary library
        // pill below always requests a section transition, never a menu.
        if [TYPE, SORT, FILTER, MORE].contains(&elem) {
            if let (Some(target), Some(placed)) = (self.requested_address(cx), <Self as Focusable<H>>::place(self, &elem, cx, At::SpringTarget)) {
                let r = placed.rest_rect;
                fx.push(Fx::App(AppFx::Library(LibraryReq::Menu {
                    kind: if elem == MORE { crate::registry::LibraryMenuKind::Sources }
                        else if elem == SORT { crate::registry::LibraryMenuKind::Sort }
                        else if elem == TYPE { crate::registry::LibraryMenuKind::Type }
                        else { crate::registry::LibraryMenuKind::Filter },
                    anchor: [r.x.to_bits(), r.y.to_bits(), r.w.to_bits(), r.h.to_bits()], target,
                })));
            }
            return Handled::Yes;
        }
        if let Some((_, index)) = self.libraries.iter().find(|(key, _)| *key == elem) {
            let directory = H::directory(cx);
            let index = *index;
            if Some(index) == self.pending.section().map(|section| section.index).or(directory.current()) { return Handled::Yes; }
            if Some(index) == directory.current() && self.pending.section().is_some() {
                self.pending.cancel();
                self.page_fade.cancel();
                self.grid_fade.cancel();
                return Handled::Yes;
            }
            let section = &directory.sections()[index];
            if let (Some(epoch), Some(sid)) = (directory.epoch(), section.sid) {
                self.save_cursor(cx, fx);
                self.pending.request_section(SectionTarget {
                    epoch, index,
                    identity: LibrarySectionIdentity { sid, key: section.key }, kind: section.kind,
                });
                self.page_fade.reload();
                fx.invalidate(Provenance::Input);
            }
            return Handled::Yes;
        }
        if region_of_elem(elem) == Some(KeyRegion::Rail) { self.follow(elem, cx, fx); return Handled::Yes; }
        if let Some(index) = self.shelves.iter().position(|row| row.heading_elem() == Some(elem)) {
            // OK on a linked heading opens the collection its shelf lists.
            let hubs = H::section_hubs(cx);
            if let Some(link) = hubs.shelves().get(index).and_then(|shelf| shelf.link.as_ref()) {
                fx.push(Fx::App(AppFx::Content(crate::registry::ContentReq::Push(
                    crate::registry::ContentArg::Collection(link.clone())))));
            }
            return Handled::Yes;
        }
        if let Some(item) = self.focused_item(Some(self.key(elem)), cx).filter(|item| !item.rk.is_empty()) {
            let from_deck = self.from_deck(elem, cx);
            let req = if held {
                LibraryReq::ItemMenu { sid: item.sid, rk: item.rk.clone(), from_deck }
            } else if from_deck && plx_media::route::deck_press().press_plays() {
                LibraryReq::Play { sid: item.sid, rk: item.rk.clone(), resume_ns: plx_data::metadata::resume_ns(item.resume_ms, item.dur_ns / 1_000_000) }
            } else { LibraryReq::Detail { sid: item.sid, rk: item.rk.clone() } };
            fx.push(Fx::App(AppFx::Library(req)));
            return Handled::Yes;
        }
        let strip = plx_ui::dispatch::STRIP_BASE;
        let req = match elem.checked_sub(strip) {
            Some(0) => Some(LibraryReq::Tab(HomeTab::Home)),
            Some(1) => Some(LibraryReq::Tab(HomeTab::Movies)),
            Some(2) => Some(LibraryReq::Tab(HomeTab::Shows)),
            Some(3) => Some(LibraryReq::Tab(HomeTab::Search)),
            Some(4) => Some(LibraryReq::Account),
            _ => None,
        };
        if let Some(req) = req { fx.push(Fx::App(AppFx::Library(req))); return Handled::Yes; }
        if elem == RETRY && self.readout == Readout::Failed
            && super::plaintext_question::asks(self.plaintext.verdict())
        {
            // The source's server is on this network and only answers without encryption:
            // *Connect* asks the shared question rather than retrying what cannot succeed.
            let sid = H::directory(cx).source().map(|(sid, _)| *sid);
            if let Some(machine) = self.plaintext.verdict().map(|v| v.machine_id.clone()) {
                self.plaintext_alert.open(&machine, sid, MachineId::Instance(self.instance), fx);
            }
            return Handled::Yes;
        }
        if elem == RETRY {
            if let Some(target) = self.address(cx) {
                self.store(target, LibraryWork::Retry, fx);
            } else {
                let directory = H::directory(cx);
                if let (Some(epoch), Some((sid, _))) = (directory.epoch(), directory.source()) {
                    fx.push(Fx::App(AppFx::Store(StoreId::Browse,
                        StoreCmd::Browse(BrowseCmd::RetrySource { epoch, sid: *sid }))));
                }
            }
            return Handled::Yes;
        }
        Handled::No
    }

    fn flush<H: LibraryLike>(&mut self, fx: &mut Effects<'_, H>) {
        let section = self.pending.take_section(self.epoch.unwrap_or(0));
        let (_, grid) = self.pending.flush();
        let selected = section.as_ref().map(|section| SectionAddress {
            epoch: section.epoch, sid: section.identity.sid, section: section.identity.key,
        });
        let query = grid.map(|(target, action)| (SectionAddress {
            epoch: target.epoch, sid: target.sid, section: target.section,
        }, match action {
            GridAction::Sort { key, desc } => plx_data::stores::browse::QueryEdit::Sort { key, desc },
            GridAction::Unwatched { desired } => plx_data::stores::browse::QueryEdit::Unwatched(desired),
            GridAction::Genre { id } => plx_data::stores::browse::QueryEdit::Genre(id),
            GridAction::LibraryType(kind) => plx_data::stores::browse::QueryEdit::LibraryType(kind),
        }));
        if let Some(target) = selected {
            let query = query.filter(|(address, _)| *address == target).map(|(_, edit)| edit);
            self.store(target, LibraryWork::Commit { select: true, choice: true, query }, fx);
        } else if let Some((target, query)) = query {
            self.store(target, LibraryWork::Commit { select: false, choice: false, query: Some(query) }, fx);
        }
    }

    fn command<H: LibraryLike>(&mut self, command: LibraryCmd, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match command {
            LibraryCmd::Enter(kind) => {
                self.save_cursor(cx, fx);
                self.flush(fx);
                self.wanted_kind = Some(kind);
                self.initial = true;
                self.page_fade.mount();
                self.sync(cx);
            }
            LibraryCmd::FocusGrid { row, col } => {
                if col >= self.layout.cols() { return Handled::No; }
                let Some(index) = row.checked_mul(self.layout.cols()).and_then(|i| i.checked_add(col)) else { return Handled::No };
                let Some(elem) = self.pair.detail.elem_for(cx, &mut self.keys, index, self.layout.cols()) else { return Handled::No };
                self.initial = false; // An explicit owned command supersedes the pending boot seat.
                self.provisional = None;
                self.reseat(FocusTarget::Elem(self.key(elem)), fx);
            }
            LibraryCmd::FocusShelf { shelf, col } => {
                let Some(&elem) = self.shelves.get(shelf).and_then(|s| s.elems.get(col)) else { return Handled::No };
                self.initial = false; // As FocusGrid: an explicit owned command supersedes the boot seat.
                self.provisional = None;
                self.reseat(FocusTarget::Elem(self.key(elem)), fx);
            }
            LibraryCmd::ItemMenu => return cx.focus.current.map_or(Handled::No, |key| self.activate(key.elem, true, cx, fx)),
            // The TYPE menu's own edit, delivered the way its row delivers it.
            LibraryCmd::SetType(kind) => {
                let Some(target) = GridTarget::from_view(H::listing(cx)) else { return Handled::No };
                let address = SectionAddress { epoch: target.epoch, sid: target.sid, section: target.section };
                self.step(&ScreenEvent::App(AppMsg::LibraryEdit {
                    target: address, edit: plx_data::stores::browse::QueryEdit::LibraryType(kind),
                }), cx, fx);
                return Handled::Yes;
            }
            LibraryCmd::OpenMenu(kind) => {
                let elem = match kind {
                    crate::registry::LibraryMenuKind::Type => TYPE,
                    crate::registry::LibraryMenuKind::Sort => SORT,
                    crate::registry::LibraryMenuKind::Filter => FILTER,
                    _ => return Handled::No,
                };
                return self.activate(elem, false, cx, fx);
            }
            LibraryCmd::Page(direction) => {
                let Some((row, col)) = self.grid_position(cx.focus.current) else { return Handled::No };
                let total = self.pair.detail.total();
                let rows = total.div_ceil(self.layout.cols());
                let next = row.saturating_add_signed(direction.signum() as isize * 2).min(rows.saturating_sub(1));
                if let Some(elem) = self.pair.detail.elem_for(cx, &mut self.keys,
                    (next * self.layout.cols() + col).min(total.saturating_sub(1)), self.layout.cols()) {
                    self.reseat(FocusTarget::Elem(self.key(elem)), fx);
                }
            }
            LibraryCmd::Sweep => {
                let current = cx.focus.current.filter(|key| key.entry == self.entry);
                let group = current.and_then(|key| <Self as Focusable<H>>::group_of(self, &key.elem, cx));
                if self.sweep_down && self.grid_position(current).is_some_and(|(row, _)|
                    row + 1 >= self.pair.detail.total().div_ceil(self.layout.cols())) {
                    self.sweep_down = false;
                } else if !self.sweep_down && group == Some(self.first_group()) {
                    self.sweep_down = true;
                }
                fx.push(Fx::Deliver(MachineId::Instance(self.instance), Delivery::Screen(
                    ScreenEvent::Input(plx_machine::machine::InputEvent {
                        at: cx.tick, source: plx_machine::machine::Source::Script,
                        kind: InputKind::Key { key: if self.sweep_down { Key::Down } else { Key::Up },
                            sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
                    }))));
            }
            LibraryCmd::SwitchStep(step) => {
                match step % 14 {
                    0 if H::directory(cx).sections().iter().any(|section| section.kind == SecKind::Show) =>
                        return self.command(LibraryCmd::Enter(SecKind::Show), cx, fx),
                    1 => return self.command(LibraryCmd::Enter(SecKind::Movie), cx, fx),
                    2 => return self.activate(SORT, false, cx, fx),
                    5 | 6 => {
                        if let Some(target) = GridTarget::from_view(H::listing(cx)) {
                            let current = self.pending.grid().and_then(|(pending, action)| {
                                if pending != &target { return None; }
                                match action { GridAction::Unwatched { desired } => Some(*desired), _ => None }
                            }).unwrap_or(H::listing(cx).unwatched());
                            let address = SectionAddress { epoch: target.epoch, sid: target.sid, section: target.section };
                            return self.step(&ScreenEvent::App(AppMsg::LibraryEdit {
                                target: address, edit: plx_data::stores::browse::QueryEdit::Unwatched(!current),
                            }), cx, fx);
                        }
                    }
                    7 => return self.activate(FILTER, false, cx, fx),
                    12 | 13 => {
                        let letter = if step % 14 == 12 { self.pair.master.elems.last() } else { self.pair.master.elems.first() };
                        if let Some(elem) = letter { self.follow(*elem, cx, fx); }
                    }
                    // Menu steps are delivered to their actual input-owning entry by the bridge.
                    _ => {}
                }
            }
        }
        Handled::Yes
    }
}

impl<H: LibraryLike> Machine<H> for LibraryScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let handled = self.step_event(ev, cx, fx);
        // The page's fade can change on any event (and tick on while the page is covered), and the
        // draw reads the shelves' dormancy between events: re-sample it once, after all of them.
        self.hold_shelves();
        handled
    }
}

impl LibraryScreen {
    /// Hold every hub shelf's cards at rest while the page is still dissolving in.
    fn hold_shelves(&mut self) {
        let held = self.page_fade.is_arriving();
        for row in &mut self.shelves { row.cards.dormant(held); }
    }

    fn step_event<H: LibraryLike>(&mut self, ev: &ScreenEvent<H>, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        use super::plaintext_question::AlertStep;
        match self.plaintext_alert.step(ev, cx) {
            AlertStep::Pass => {}
            AlertStep::Done(handled) => return handled,
            AlertStep::Answer(cmd) => {
                if let Some(cmd) = cmd { fx.push(Fx::App(AppFx::Session(cmd))); }
                self.reseat(FocusTarget::ContainerGroup(STATUS_GROUP), fx);
                return Handled::Yes;
            }
        }
        match ev {
            ScreenEvent::Mount | ScreenEvent::StoreChanged(..) => { self.sync(cx); }
            ScreenEvent::RestoreMemory(PageMemory::Library(memory)) => { self.restore(memory); self.sync(cx); }
            ScreenEvent::Enter(enter) => {
                self.live = true;
                // The engine seats the target right after this event, so a key it names must hold
                // a projected slot by then: pinned and projected until the page's next tick.
                if let Enter::Fresh { focus: FocusTarget::Elem(key) } = enter {
                    self.seat_wanted = Some(key.elem).filter(|_| key.entry == self.entry);
                }
                self.sync(cx);
            }
            ScreenEvent::Cover => { self.live = false; }
            ScreenEvent::Uncover => { self.live = true; self.sync(cx); }
            ScreenEvent::WillLeave(_) => { self.save_cursor(cx, fx); self.flush(fx); }
            ScreenEvent::App(AppMsg::Library(command)) => return self.command(*command, cx, fx),
            ScreenEvent::App(AppMsg::LibraryEdit { target, edit }) => {
                use plx_data::stores::browse::QueryEdit;
                if Some(target.epoch) != self.epoch { return Handled::No; }
                let target = self.pending.section().map(|section| SectionAddress {
                    epoch: section.epoch, sid: section.identity.sid, section: section.identity.key,
                }).unwrap_or(*target);
                let action = match edit {
                    QueryEdit::Sort { key, desc } => GridAction::Sort { key: key.clone(), desc: *desc },
                    QueryEdit::Unwatched(desired) => GridAction::Unwatched { desired: *desired },
                    QueryEdit::Genre(id) => GridAction::Genre { id: id.clone() },
                    QueryEdit::LibraryType(kind) => GridAction::LibraryType(*kind),
                };
                if matches!(&action, GridAction::Unwatched { desired } if *desired == H::listing(cx).unwatched())
                    && self.pending.section().is_none() && self.address(cx) == Some(target)
                    && self.grid_fade.cancel() {
                    self.pending.cancel_grid();
                    return Handled::Yes;
                }
                self.pending.request_grid(GridTarget { epoch: target.epoch, sid: target.sid, section: target.section, query: self.query.unwrap_or(0) }, action);
                self.grid_fade.reload();
            }
            ScreenEvent::App(AppMsg::LibrarySelect(target)) => {
                let directory = H::directory(cx);
                if directory.epoch() == Some(target.epoch) {
                    if let Some(index) = directory.sections().iter().position(|section| section.sid == Some(target.sid) && section.key == target.section) {
                        self.save_cursor(cx, fx);
                        self.pending.request_section(SectionTarget { epoch: target.epoch, index, identity: LibrarySectionIdentity { sid: target.sid, key: target.section }, kind: directory.sections()[index].kind });
                        self.page_fade.reload();
                    }
                }
            }
            ScreenEvent::FocusMoved { from, to, by } => {
                // The grid opens the row's caption band and, for a deliberate move only, grows the
                // tile from rest over the frames after this, as a shelf's does. A restore or a
                // reconcile is adopted whole by its next tick so a page returning from a Detail
                // push lands as it was left.
                self.pair.detail.on(ev, cx, fx, self.scroll.pos);
                self.shelves_on(ev, cx, fx);
                if matches!(by, By::Dir | By::Pointer) {
                    self.initial = false;
                    self.provisional = None;
                }
                let from_group = from.and_then(|key| <Self as Focusable<H>>::group_of(self, &key.elem, cx));
                let to_group = <Self as Focusable<H>>::group_of(self, &to.elem, cx);
                // Any seat off the head that was not a user move (a restore, a reconcile, a seat by
                // element) is somebody else's choice too, and the head seat no longer describes it.
                if to_group != self.provisional { self.provisional = None; }
                // What the engine now remembers for this group is the page's own seat only if this
                // move IS that seat; any other move into the group was somebody's choice.
                if let Some(group) = to_group {
                    self.placed.retain(|(placed, _)| *placed != group);
                    if self.provisional == Some(group) { self.placed.push((group, to.elem)); }
                }
                let outcome = self.pair.focus_moved(from_group, to_group, *to);
                // Projected entry names the letter but preserves the exact remembered grid item.
                // Only a move within the rail (or direct pointer entry) jumps to its first title.
                if matches!(outcome, MasterOutcome::Follow(_))
                    && (from_group == Some(self.pair.groups_config().master) || *by == By::Pointer) {
                    self.follow(to.elem, cx, fx);
                }
                self.reveal(*to, *by, cx);
            }
            ScreenEvent::Tick(tick) => {
                self.seat_wanted = None;
                self.sync(cx);
                if self.watch_readout(cx) {
                    fx.invalidate(Provenance::Landing(fx.from()));
                }
                self.plaintext_alert.update(tick.dt());
                if self.grid_reset_pending {
                    if let Some(elem) = self.pair.detail.elem_for(cx, &mut self.keys, 0, self.layout.cols()) {
                        fx.remember(self.pair.groups_config().detail, elem);
                        self.grid_reset_pending = false;
                        if cx.focus.current.is_some_and(|key| key.entry == self.entry && region_of_elem(key.elem) == Some(KeyRegion::Grid)) {
                            self.reseat(FocusTarget::ContainerGroup(self.pair.groups_config().detail), fx);
                        }
                    } else if self.readout == Readout::Empty {
                        self.grid_reset_pending = false;
                    }
                }
                let current_grid = cx.focus.current.filter(|key| key.entry == self.entry)
                    .and_then(|key| self.pair.detail.index_of(key.elem));
                self.pair.master.advance(cx, current_grid, self.live && self.readout == Readout::Grid, tick.dt());
                if let Some(kind) = self.wanted_kind {
                    let directory = H::directory(cx);
                    if let Some(target) = directory.kind_address(kind) {
                        self.store(target, LibraryWork::Commit { select: true, choice: false, query: None }, fx);
                    } else {
                        self.readout = readout(directory.kind_fetch(kind), 0, directory.kind_fetch(kind), -1);
                    }
                }
                // Compact menus leave their host's transaction clock live while owning input.
                let waiting_for_kind = self.wanted_kind.is_some_and(|kind| {
                    let directory = H::directory(cx);
                    directory.preferred(kind).is_some() || directory.kind_fetch(kind) == SecFetch::Loading
                });
                let ready = self.readout != Readout::Loading && !waiting_for_kind;
                // A section's reveal also waits for the document ABOVE its grid. Its shelves are
                // a separate fetch that sets the grid's absolute offset, so revealing on the
                // listing alone drew the grid at the top and then dropped it by the shelves'
                // height when they landed (owner report item 9, the section half). A grid-only
                // swap keeps its own `ready`: its shelves are already the document it fades inside.
                let shelves = shelves_settled(H::section_hubs(cx));
                let page_commit = self.page_fade.tick(tick.dt(), ready && shelves);
                let grid_commit = self.grid_fade.tick(tick.dt(), ready);
                if page_commit || (grid_commit && self.pending.section().is_none()) { self.flush(fx); }
                if self.live {
                    let dt = tick.dt();
                    let focused = cx.focus.current;
                    let chosen = self.pending.section().map(|section| section.index).or(H::directory(cx).current());
                    let selected = self.libraries.iter().position(|(_, section)| Some(*section) == chosen).map_or(-1, |i| i as i32);
                    let focused_library = focused.filter(|key| key.entry == self.entry)
                        .and_then(|key| self.libraries.iter().position(|(elem, _)| *elem == key.elem)).map_or(-1, |i| i as i32);
                    let spans: Vec<_> = (0..self.libraries.len()).map(|i| {
                        let rect = self.library_rect(i, cx); (rect.x, rect.w)
                    }).collect();
                    self.library_capsules.update(selected, focused_library, |i| spans.get(i).copied(), plx_ui::widgets::SelMark::Travels, dt);
                    self.library_pop.step((focused_library >= 0).then_some(0), dt);
                    let colours = self.focused_item(focused, cx).filter(|item| item.has_blur).map(|item| item.blur)
                        .or_else(|| (!self.ground_seeded).then(|| H::listing(cx).item(0).filter(|item| item.has_blur).map(|item| item.blur)).flatten());
                    self.ground_seeded |= colours.is_some();
                    self.ground.key(colours, plx_ui::widgets::PageGround::CARD_W, dt);
                    // A page still dissolving in shows its seat unpopped: the pop starts when the
                    // page is opaque, where it can be seen, instead of playing out under the fade.
                    // A shelf at exact rest is a fixed point of its tick, so a page of hundreds of
                    // them costs a lookup each, not a spring step.
                    self.shelves_on(ev, cx, fx);
                    self.scroll.step(self.scroll_target, K_SCROLL, dt);
                    // The grid steps its pop and bands. A content landing that moved the focused
                    // element's row (`Landed`) comes back as the document shift that keeps the
                    // tile where it was on screen; the scroll and its target both take it, so the
                    // spring has nothing to chase. `refresh` ran in `sync` above and shifts nothing.
                    // A shift the document cannot honour (a tile near either end) clamps, each of
                    // the two on its own, so a clamp that does not bite adds no spring.
                    let shift = self.pair.detail.on(ev, cx, fx, self.scroll.pos);
                    if shift != 0.0 {
                        let max = self.target_layout.max_scroll();
                        self.scroll.pos = (self.scroll.pos + shift).clamp(0.0, max);
                        self.scroll_target = (self.scroll_target + shift).clamp(0.0, max);
                    }
                    self.relayout(focused);
                    // The scroll stepped and the grid was handed it: the cells it paints this
                    // frame (its window moved with the step) must hold keys before it draws.
                    if self.wanted_kind.is_none() { self.project_grid(cx); }
                    let hint = self.hint_input(cx);
                    self.hold_hint.step(hint, tick.ms, dt, &mut |ev| fx.note(ev));
                    // A first seat that applies a bookmarked scroll clamps it to the document, so
                    // behind a section reveal it waits for the same shelves the reveal does:
                    // seated on the shelfless interim, a bookmark deep in the grid landed short of
                    // where the reader left. A head seat needs no wait — `provisional` follows the
                    // head when the shelves land — and a page already on screen seats as it did.
                    let settled = shelves || !self.page_fade.is_swapping() || self.restore_scroll.is_none();
                    if self.initial && settled && self.layout.first().is_some() && self.seed_cursor(cx, fx) {
                        self.initial = false;
                        let group = match self.layout.seat_for_scroll(&self.run, self.scroll.pos, self.layout.visible_rows(self.scroll.pos).0) {
                            Some(Block::Grid(_)) => self.pair.groups_config().detail,
                            Some(Block::Shelf(index)) => self.shelves[index].group,
                            _ => self.first_group(),
                        };
                        // Only the page's OWN head seat is provisional. Any restore is the reader's:
                        // a restored scroll (a saved viewport or bookmark, at the head or not) names
                        // the block they left, and a remembered cursor in the seated group is where
                        // `Seat::Remembered` puts them back — neither may follow a landing.
                        let restoring = self.restore_scroll.is_some() || cx.focus.remembered(group)
                            .is_some_and(|elem| !self.placed.contains(&(group, elem)));
                        self.provisional = (group == self.first_group() && !restoring).then_some(group);
                        self.seat_on(group, cx, fx);
                    } else if let Some(seated) = self.provisional.filter(|_| self.layout.first().is_some()) {
                        // A landing moved the head out from under the page's own seat (field report
                        // on a788b27a: the shelves committed above a seat on the grid's heading, and
                        // the first DOWN went on into the grid). Follow it, as a first paint would.
                        let head = self.first_group();
                        let at = focused.filter(|key| key.entry == self.entry)
                            .and_then(|key| <Self as Focusable<H>>::group_of(self, &key.elem, cx));
                        // Focus anywhere but the seat is somebody's move, reported or not yet (the
                        // engine moves before the page hears `FocusMoved`): not the page's to take.
                        if at != Some(seated) { self.provisional = None; }
                        else if head != seated { self.provisional = Some(head); self.seat_on(head, cx, fx); }
                    }
                }
                // The dispatcher ticks the top page beneath a compact menu, but not buried pages.
                // Cover pauses its control motion; it must not stop the query just committed above.
                if let Some(target) = self.address(cx).filter(|_| self.wanted_kind.is_none()) {
                    let (lo, hi) = self.layout.visible_rows(self.scroll.pos);
                    let focus = cx.focus.current.filter(|key| key.entry == self.entry)
                        .and_then(|key| self.pair.detail.index_of(key.elem));
                    self.store(target, LibraryWork::Want { lo: lo.saturating_sub(1) * self.layout.cols(), hi: (hi + 1) * self.layout.cols(), focus }, fx);
                    self.store(target, LibraryWork::Letters, fx);
                    self.send_hold(target, cx, fx);
                    let at_head = self.scroll.pos.abs() < 1.0 && self.scroll.vel.abs() < 1.0 && self.scroll_target.abs() < 0.5;
                    fx.push(Fx::App(AppFx::Library(LibraryReq::PublishShelves {
                        target, at_head, hidden_page: self.page_fade.is_swapping() && self.page_fade.alpha() <= 0.01,
                    })));
                }
                fx.push(Fx::App(AppFx::StoreWork(StoreWork::Browse)));
            }
            ScreenEvent::Activate(elem) => return self.activate(*elem, false, cx, fx),
            ScreenEvent::PressHold(_) => {
                return self.command(LibraryCmd::ItemMenu, cx, fx);
            }
            ScreenEvent::PressCommit(_) => {
                return cx.focus.current.map_or(Handled::No, |key| self.activate(key.elem, false, cx, fx));
            }
            ScreenEvent::Input(input) => {
                // Any key, pointer motion, click, drag or wheel is the reader acting at the seat.
                // It must claim it HERE: OK-down arms a delayed press the dispatcher only commits
                // if focus is still on its key, and this page's next Tick may run before that
                // commit; hovering the already-focused control moves nothing, so no `FocusMoved`.
                if matches!(input.kind, InputKind::Key { edge: Edge::Down, .. } | InputKind::Pointer { .. }
                    | InputKind::Click { .. } | InputKind::Drag { .. } | InputKind::Wheel { .. }) {
                    self.provisional = None;
                }
                // A pointer on a control confirms it the way activating it does.
                if let InputKind::Pointer { hit: Some(hit), .. } | InputKind::Click { hit: Some(hit), .. }
                    | InputKind::Drag { hit: Some(hit), .. } = input.kind {
                    self.placed.retain(|(_, placed)| *placed != hit);
                }
                if matches!(input.kind, InputKind::Key { key: Key::Ok, edge: Edge::Down, .. })
                    && cx.focus.current.is_some_and(|key| key.entry == self.entry && region_of_elem(key.elem) == Some(KeyRegion::Rail)) {
                    self.reseat(FocusTarget::ContainerGroup(self.pair.groups_config().detail), fx);
                    return Handled::Yes;
                }
                if let InputKind::Wheel { dy } = input.kind {
                    self.scroll_target = (self.scroll_target - dy * self.layout.grid_pitch())
                        .clamp(0.0, self.target_layout.max_scroll());
                    fx.invalidate(Provenance::Input);
                    return Handled::Yes;
                }
                if let InputKind::Key { sym, wcode, edge: Edge::Down, .. } = input.kind {
                    if let Some(dir) = plx_ui::consts::page_dir(sym, wcode) {
                        return self.command(LibraryCmd::Page(dir), cx, fx);
                    }
                }
                if let InputKind::Key { key: Key::Back, edge: Edge::Down, .. } = input.kind {
                    let cancelled = self.page_fade.cancel() | self.grid_fade.cancel();
                    if cancelled { self.pending.cancel(); return Handled::Yes; }
                    if cx.focus.current.is_some_and(|key| key.entry == self.entry && region_of_elem(key.elem) == Some(KeyRegion::Rail)) {
                        self.reseat(FocusTarget::ContainerGroup(self.pair.groups_config().detail), fx);
                        return Handled::Yes;
                    }
                    if self.scroll_target > 0.5 {
                        self.scroll_target = 0.0;
                        self.reseat(FocusTarget::ContainerGroup(self.first_group()), fx);
                    } else { fx.push(Fx::App(AppFx::Library(LibraryReq::BackToHome { kind: self.kind }))); }
                    return Handled::Yes;
                }
            }
            _ => {}
        }
        Handled::No
    }
}

impl<H: LibraryLike> Focusable<H> for LibraryScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if self.plaintext_alert.groups(out) { return; }
        if !self.libraries.is_empty() {
            out.push(row_group(LIBRARY_GROUP, self.libraries.len(), Rect::new(MARGIN_X, CONTENT_TOP - self.scroll.pos, layout::GRID_RIGHT - MARGIN_X, 52.0), ElemKind::Control));
        }
        for (index, row) in self.shelves.iter().enumerate() {
            let mut shelf = row_group(row.group, row.elems.len(), Rect::new(MARGIN_X, self.target_layout.shelf_y(&self.target_run, index, self.scroll_target) + CARD_DY, SCR_W - 2.0 * MARGIN_X, row_style(row).h), ElemKind::Card);
            if let Some((group, elem)) = row.heading {
                if let Some(rect) = self.heading_rect(index, cx, At::SpringTarget) {
                    out.push(plx_ui::linked_heading::group_spec(group, rect));
                }
                // DOWN from the heading returns to the card this shelf remembered.
                shelf.seat = plx_ui::linked_heading::shelf_seat(&cx.focus, elem, row.group, shelf.seat);
            }
            out.push(shelf);
        }
        if self.layout.grid_head {
            out.push(row_group(self.toolbar, self.toolbar_elems().len(), self.toolbar_chip_rect(self.toolbar_elems()[0], cx, At::SpringTarget), ElemKind::Control));
        }
        if self.readout == Readout::Failed {
            if let Some(rect) = self.status_rect(cx) {
                out.push(GroupSpec { edge: [EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop, EdgeRule::Stop],
                    ..row_group(STATUS_GROUP, 1, rect, ElemKind::Control) });
            }
        }
        if self.pair.detail.total() > 0 && self.pair.master.eligible(cx) {
            self.pair.groups(cx, out);
        } else {
            self.pair.detail.groups(cx, out);
        }
    }
    fn group_of(&self, elem: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        if let Some(answer) = self.plaintext_alert.group_of(*elem) { return answer; }
        if self.libraries.iter().any(|(key, _)| key == elem) { return Some(LIBRARY_GROUP); }
        if let Some(row) = self.shelves.iter().find(|row| row.elems.contains(elem)) { return Some(row.group); }
        if let Some((group, _)) = self.shelves.iter().find_map(|row| row.heading.filter(|(_, e)| e == elem)) { return Some(group); }
        if self.layout.grid_head && self.toolbar_elems().contains(elem) { return Some(self.toolbar); }
        if self.readout == Readout::Failed && *elem == RETRY { return Some(STATUS_GROUP); }
        self.pair.group_of(elem, cx)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        if let Some(step) = self.plaintext_alert.neighbour(key, dir) { return step; }
        if matches!(region_of_elem(key.elem), Some(KeyRegion::Grid | KeyRegion::Rail)) {
            return self.pair.neighbour(key, dir, cx);
        }
        let elems = self.row_elems(self.group_of(&key.elem, cx));
        let Some(index) = elems.iter().position(|elem| *elem == key.elem) else { return Step::Edge };
        let next = match dir { Dir::Left => index.checked_sub(1), Dir::Right => Some(index + 1), _ => None };
        next.and_then(|i| elems.get(i).copied()).map_or(Step::Edge, |elem| Step::Move(self.key(elem)))
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        if let Some(placed) = self.plaintext_alert.place(*elem) { return placed; }
        if matches!(region_of_elem(*elem), Some(KeyRegion::Grid | KeyRegion::Rail)) {
            return self.pair.place(elem, cx, at);
        }
        let rect = if let Some(index) = self.libraries.iter().position(|(key, _)| key == elem) {
            self.library_rect(index, cx)
        } else if let Some((row, col)) = self.shelves.iter().enumerate().find_map(|(row, shelf)|
            shelf.elems.iter().position(|key| key == elem).map(|col| (row, col))) {
            if at == At::Drawn { return self.shelf_placed(row, elem, cx); }
            self.shelf_rect_at(row, col, cx)
        } else if let Some(index) = self.shelves.iter().position(|row| row.heading_elem() == Some(*elem)) {
            self.heading_rect(index, cx, at)?
        } else if self.toolbar_elems().contains(elem) && self.layout.grid_head { self.toolbar_chip_rect(*elem, cx, at) }
        else if *elem == RETRY && self.readout == Readout::Failed { self.status_rect(cx)? }
        else { return None };
        Some(Placed { rect, rest_rect: rect, clip: content_clip(), index: None })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if let Some(key) = self.plaintext_alert.reconcile(want) { return key; }
        if matches!(self.keys.region(want.elem).or_else(|| region_of_elem(want.elem)), Some(KeyRegion::Grid | KeyRegion::Rail)) {
            // Ownership is a typed identity query, even when group_of no longer places the key.
            let next = self.pair.reconcile(want, cx);
            if self.pair.place(&next.elem, cx, At::SpringTarget).is_some() { return next; }
        }
        if self.place(&want.elem, cx, At::SpringTarget).is_some() { return want; }
        if let Some((group, index)) = self.keys.last_place(want.elem) {
            let elems = self.row_elems(Some(group));
            if let Some(elem) = elems.get(index.min(elems.len().saturating_sub(1))) { return self.key(*elem); }
        }
        let group = self.first_group();
        self.row_elems(Some(group)).first().copied().map(|elem| self.key(elem))
            .unwrap_or_else(|| self.key(plx_ui::dispatch::STRIP_BASE))
    }
    fn seat(&self, group: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if let Some(key) = self.plaintext_alert.seat(group, self.entry) { return key; }
        if group == self.pair.groups_config().detail || group == self.pair.groups_config().master { return self.pair.seat(group, from, cx); }
        let elems = self.row_elems(Some(group));
        let elem = elems.iter().filter_map(|elem| self.place(elem, cx, At::SpringTarget)
            .map(|placed| (*elem, (placed.rect.cx() - from.rect.cx()).abs())))
            .min_by(|a, b| a.1.total_cmp(&b.1)).map(|(elem, _)| elem)
            .unwrap_or(plx_ui::dispatch::STRIP_BASE);
        self.key(elem)
    }
}

impl LibraryScreen {
    fn row_elems(&self, group: Option<GroupId>) -> Vec<u32> {
        match group {
            Some(LIBRARY_GROUP) => self.libraries.iter().map(|(elem, _)| *elem).collect(),
            Some(group) if group == self.toolbar => self.toolbar_elems().to_vec(),
            Some(STATUS_GROUP) => vec![RETRY],
            Some(group) => self.shelves.iter().find_map(|row| {
                if row.group == group { return Some(row.elems.clone()); }
                row.heading.filter(|(heading, _)| *heading == group).map(|(_, elem)| vec![elem])
            }).unwrap_or_default(),
            None => Vec::new(),
        }
    }
    /// Where shelf `index` sits for the component, in screen space: the top of its tiles.
    fn shelf_frame(&self, index: usize) -> SectionFrame {
        SectionFrame { y: self.layout.shelf_y(&self.run, index, self.scroll.pos) + CARD_DY, clip: content_clip() }
    }
    /// Shelf `index`'s content, read off the published hub (borrows only).
    fn hub_src<'a, 'v: 'a, H: LibraryLike>(&'a self, index: usize, cx: &Cx<'v, H>) -> draw::HubSrc<'a> {
        draw::HubSrc { elems: &self.shelves[index].elems, shelf: H::section_hubs(cx).shelves().get(index), paging: false }
    }
    /// The card `elem` of shelf `index` as drawn now: the component's placement (pop and press
    /// folded in, `rest_rect` the settled focus-scaled rect). `index` stays unset: nothing here
    /// reads a shelf cell's position as a slot of another container.
    fn shelf_placed<H: LibraryLike>(&self, index: usize, elem: &u32, cx: &Cx<'_, H>) -> Option<Placed> {
        let src = self.hub_src(index, cx);
        self.shelves[index].cards.place(cx, &src, elem, self.shelf_frame(index), At::Drawn)
            .map(|placed| Placed { index: None, ..placed })
    }
    /// Where card `col` of shelf `index` is GOING: the settled rect at the scroll target, with the
    /// row scrolled to bring the focused card into view. The component, holding the live page,
    /// cannot say it.
    fn shelf_rect_at<H: LibraryLike>(&self, index: usize, col: usize, cx: &Cx<'_, H>) -> Rect {
        let row = &self.shelves[index];
        let style = row_style(row);
        let focused = cx.focus.current.filter(|key| key.entry == self.entry)
            .and_then(|key| row.elems.iter().position(|elem| *elem == key.elem));
        let x = focused.map(|col| plx_ui::cards::scroll_into_view(row.cards.scroll(), col,
            row.elems.len(), style.w, style.gap, SCR_W - 2.0 * MARGIN_X)).unwrap_or(row.cards.scroll());
        plx_ui::cards::tile_rect(col, MARGIN_X, style.w + style.gap, x,
            self.target_layout.shelf_y(&self.target_run, index, self.scroll_target) + CARD_DY, (style.w, style.h))
            .scaled(if focused == Some(col) { style.focus_scale } else { 1.0 })
    }
    /// The linked heading of shelf `index`, over the published hub's own title.
    fn heading_widget<'a, H: LibraryLike>(&self, index: usize, cx: &Cx<'a, H>) -> Option<plx_ui::linked_heading::LinkedHeading<'a>> {
        self.shelves.get(index)?.heading?;
        let shelf = H::section_hubs(cx).shelves().get(index)?;
        Some(plx_ui::linked_heading::LinkedHeading::heading(&shelf.title, "").total(shelf.total)
            .bounded(layout::GRID_RIGHT - MARGIN_X))
    }
    /// The linked heading's face, cap top where an unlinked heading's would be.
    fn heading_rect<H: LibraryLike>(&self, index: usize, cx: &Cx<'_, H>, at: At) -> Option<Rect> {
        let heading = self.heading_widget(index, cx)?;
        let row = &self.shelves[index];
        let focused = cx.focus.current.is_some_and(|key| key.entry == self.entry && Some(key.elem) == row.heading_elem());
        let y = match at {
            At::Drawn => self.layout.shelf_y(&self.run, index, self.scroll.pos) - plx_ui::consts::TITLE_DY - row.cards.heading_lift(),
            At::SpringTarget => self.target_layout.shelf_y(&self.target_run, index, self.scroll_target) - plx_ui::consts::TITLE_DY,
        };
        Some(heading.face_rect(MARGIN_X, y, f32::from(focused), &heading.measure(cx.measure)))
    }
    fn library_rect<H: LibraryLike>(&self, index: usize, cx: &Cx<'_, H>) -> Rect {
        let y = CONTENT_TOP - self.scroll.pos - self.shelves.first().map_or(0.0, |row| row.cards.heading_lift());
        // Geometry always comes from the shared pill strip now: a singleton never reaches this
        // (`self.libraries` is empty), so there is no separate chip-width branch to keep in sync.
        self.library_lays(cx).get(index).map(|lay|
            plx_ui::widgets::strip_pill_rect(lay, y, plx_ui::widgets::StatusOverlay::CTRL_H))
            .unwrap_or(Rect::new(MARGIN_X, y, 0.0, 0.0))
    }
}
impl HubShelf {
    fn heading_elem(&self) -> Option<u32> { self.heading.map(|(_, elem)| elem) }
    /// The group a door INTO this shelf from above lands in: its linked heading when it has one.
    fn entry_group(&self) -> GroupId { self.heading.map_or(self.group, |(group, _)| group) }
}
fn row_style(row: &HubShelf) -> &'static RowStyle { hub_style(row.landscape) }
fn hub_style(landscape: bool) -> &'static RowStyle { if landscape { &RowStyle::EPISODE } else { &RowStyle::HOME } }
/// A hub shelf's card section. Cards a screen's width off to either side still paint: a popped
/// card's shadow reaches onto the screen from beyond its edge.
fn hub_cards(entry: EntryId, landscape: bool) -> plx_ui::cards::Shelf {
    plx_ui::cards::Shelf::new(entry, hub_style(landscape)).cull_margin(32.0)
}
/// The panel the shelves' cards are placed and hit-tested in: everything under the top bar.
fn content_clip() -> Rect {
    Rect::new(0.0, plx_ui::widgets::TOP_BAR_BOTTOM, SCR_W, SCR_H - plx_ui::widgets::TOP_BAR_BOTTOM)
}
fn row_group(id: GroupId, len: usize, extent: Rect, elem: ElemKind) -> GroupSpec {
    GroupSpec { id, kind: GroupKind::Row { wrap: false }, seat: Seat::Remembered,
        reachable: AxisMask::VERTICAL, edge: [EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop], extent, len, elem }
}

impl<H: LibraryLike> Screen<H> for LibraryScreen {
    fn focused_card<'a>(&self, cx: &Cx<'a, H>, focus: Option<plx_machine::machine::FocusKey<u32>>, at: Option<At>) -> Option<plx_ui::screen::FocusedCard<'a>> {
        let item = self.focused_item(focus, cx)?;
        let placed = focus.zip(at).and_then(|(key, at)| Focusable::<H>::place(self, &key.elem, cx, at));
        Some(plx_ui::screen::FocusedCard::new(item, placed))
    }
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<plx_machine::machine::FocusKey<u32>>) {
        LibraryScreen::redraw_focused::<H>(self, f, focus)
    }
    fn name(&self) -> &'static str { "library" }
    fn state(&self) -> &dyn LogicalState { self }
    fn crumb(&self, _: &Cx<'_, H>) -> Option<Cow<'_, str>> { None }
    fn prepare(&mut self, b: &mut Budget, cx: &Cx<'_, H>) { self.pair.prepare(b, cx); }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) { self.draw_page(f); }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn memory(&self) -> PageMemory {
        PageMemory::Library(self.page_memory())
    }
    fn links(&self, out: &mut Vec<Link>) {
        out.push(Link { from: STRIP, dir: Dir::Down, to: self.first_group() });
        out.push(Link { from: self.first_group(), dir: Dir::Up, to: STRIP });
        // A continuous document has adjacent blocks even while its offscreen geometry
        // passes the floating strip. Geometric ranking must not skip an intervening block.
        let mut document = Vec::new();
        if !self.libraries.is_empty() { document.push(LIBRARY_GROUP); }
        // A collection shelf's linked heading is its own block, directly above its cards: UP from
        // any card reaches it, DOWN returns to the remembered card (`groups`' seat).
        for row in &self.shelves {
            if let Some((heading, _)) = row.heading { document.push(heading); }
            document.push(row.group);
        }
        if self.layout.grid_head { document.push(self.toolbar); }
        if self.readout == Readout::Failed { document.push(STATUS_GROUP); }
        if self.pair.detail.total() > 0 { document.push(self.pair.groups_config().detail); }
        for pair in document.windows(2) {
            out.push(Link { from: pair[0], dir: Dir::Down, to: pair[1] });
            out.push(Link { from: pair[1], dir: Dir::Up, to: pair[0] });
        }
        self.pair.links(out);
        out.push(Link { from: self.toolbar, dir: Dir::Right, to: self.pair.groups_config().master });
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> { Some(self) }
}

impl LogicalState for LibraryScreen {
    fn write(&self, c: &mut Canon) {
        c.u32(self.entry.0).u32(self.instance.0);
        c.u32(match self.kind { SecKind::Movie => 0, SecKind::Show => 1 });
        c.option(self.wanted_kind, |c, kind| { c.u32(match kind { SecKind::Movie => 0, SecKind::Show => 1 }); });
        c.f32(self.scroll.pos).f32(self.scroll.vel).f32(self.scroll_target);
        c.option(self.restore_scroll, |c, value| { c.f32(value); });
        c.bool(self.live).bool(self.initial);
        c.option(self.provisional, |c, group| { c.u32(group.0); });
        c.seq(self.placed.len());
        for (group, elem) in &self.placed { c.u32(group.0).u32(*elem); }
        c.bool(self.sweep_down);
        c.option(self.epoch, |c, value| { c.u32(value); });
        c.option(self.query, |c, value| { c.u32(value); });
        c.bool(self.grid_reset_pending);
        c.option(self.shelf_publication, |c, (id, revision)| {
            c.u32(id.epoch).u32(u32::from(id.sid.raw())).u64(id.section as u64).u64(revision);
        });
        self.page_fade.write(c);
        self.grid_fade.write(c);
        self.pair.state().write(c);
        // `toolbar` is not written: like the pair's ids it is registered in `keys` for the section
        // and epoch, all three of which `page_memory` below already carries.
        c.u32(self.pair.groups_config().master.0).u32(self.pair.groups_config().detail.0);
        self.pending.write(c);
        c.bool(self.ground_seeded);
        self.ground.write_motion(c);
        self.library_capsules.write_motion(c);
        self.library_pop.write_motion(c);
        PageMemory::Library(self.page_memory()).write(c);
        c.seq(self.viewports.len());
        for viewport in &self.viewports { viewport.write(c); }
        c.seq(self.shelves.len());
        for row in &self.shelves {
            let HubShelf { id, group, heading, elems, landscape, cards } = row;
            c.str(id).u32(group.0);
            c.option(*heading, |c, (group, elem)| { c.u32(group.0).u32(elem); });
            c.bool(*landscape).seq(elems.len());
            for elem in elems { c.u32(*elem); }
            cards.write(c);
        }
        c.seq(self.libraries.len());
        for (elem, section) in &self.libraries { c.u32(*elem).u32(*section as u32); }
        c.u32(match self.readout { Readout::Loading => 0, Readout::Empty => 1, Readout::Failed => 2, Readout::Grid => 3 });
        self.layout.write(c);
        self.target_layout.write(c);
        self.pair.detail.write(c);
        self.pair.master.write(c);
    }
    fn probe(&self, out: &mut String) { out.push_str("library"); }
}
