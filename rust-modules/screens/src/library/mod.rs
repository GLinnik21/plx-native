//! Library instance: document motion and transactions belong here; focus belongs to Input.
mod identity;
mod bookmark;
mod layout;
mod recommended;
pub use recommended::RecommendedScreen;
mod lookahead;
mod parts;
mod rail;
#[cfg(test)] mod scroll_chrome_tests;
#[cfg(test)]
mod rail_tests;
mod transactions;
mod draw;
mod collections_page;
mod header;
mod toolbar;
mod status;
pub mod menu;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod cards_harness; // Tier 2 card conformance (cards_conformance_tests.rs)
#[cfg(test)]
#[cfg(test)]
mod labels_tests;
#[cfg(test)]
mod deferred_tests;
#[cfg(test)]
mod navigation_tests;
#[cfg(test)]
mod foreign_replacement_tests;
#[cfg(test)]
mod selector_matrix_tests;

use std::ops::Range;
use std::borrow::Cow;
use plx_data::browse::{SecFetch, SecKind};
use crate::registry::{
    AppFx, AppMsg, HomeTab, LibraryCmd, LibraryIdentity, LibraryLike, LibraryMemory,
    LibraryReq, LibrarySectionIdentity, LibraryViewport, PageMemory,
};
use plx_data::stores::{StoreCmd, StoreId, StoreWork};
use plx_data::stores::browse::{BrowseCmd, LibraryWork, SectionAddress};
use plx_ui::consts::{MARGIN_X, SCR_W, SCR_H, K_SCROLL};
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
use plx_ui::page_regions::{Arrival, BackStep, PageRegions};
use plx_ui::{Rect, Spring};
use plx_ui::xfade::Xfade;
use identity::{KeyRegistry, KeyRegion, region_of_elem};
use layout::{Layout, Block, CONTENT_TOP};
use toolbar::{CollectionsControl, COLLECTIONS, CRUMB_ROOT, RECOMMENDED, SORTFILTER};
#[cfg(test)]
use layout::COLS;
use parts::{GridPart, RailPart, GRID_GROUP, RAIL_GROUP};
use transactions::{PendingTransactions, SectionTarget, GridTarget, GridAction};

/// The header row's group: the library names, "+N" and the page's controls, one Left/Right path.
pub const LIBRARY_GROUP: GroupId = GroupId(0x4c49_4210);
const STATUS_GROUP: GroupId = GroupId(0x4c49_4212);
const RETRY: u32 = 3;
const MORE: u32 = 4;
/// "Connect without encryption?" over a failed source's read-out (`screens::plaintext_question`).
const PLAINTEXT_GROUP: GroupId = GroupId(0x4c49_4213);
const PLAINTEXT_CANCEL: u32 = 5;
const PLAINTEXT_CONNECT: u32 = 6;
const STRIP: GroupId = plx_ui::containers::tabs::STRIP;

pub const SHAPE: [&str; 10] = [
    "LibraryScreen{entry:u32,instance:u32,kind:u32,wanted_kind:Option<u32>,scroll:{pos:f32,vel:f32},scroll_target:f32,restore_scroll:Option<f32>,live:bool,initial:bool,provisional:Option<u32>,placed:[(group:u32,elem:u32)],sweep_down:bool,epoch:Option<u32>,query:Option<u32>,grid_reset_pending:bool,page_fade:Xfade{phase:u8,t:f32},grid_fade:Xfade{phase:u8,t:f32},handoff:Handoff{tucked:bool,chrome:f32,capsule:f32},pair:MasterDetailState{side:u32,follow:u32,band:u32,door:Option<u32>},pending:PendingTransactions,ground_seeded:bool,ground:PageGround,switched:bool,chrome:LibraryChrome,memory:PageMemory::Library,viewport_cache:[LibraryViewport],libraries:[(elem:u32,section:u32)],readout:u32,layout:LibraryLayout,target_layout:LibraryLayout,grid:LibraryGrid,rail:LibraryRail}",
    transactions::SHAPE,
    plx_ui::widgets::PageGround::SHAPE,
    "LibraryHeader{switched:bool}",
    "LibraryChrome{pair_groups:{master:u32,detail:u32}}",
    Layout::SHAPE,
    GridPart::SHAPE,
    RailPart::SHAPE,
    // The header row's pills, held by the screen (`header_capsules`, `header_pop`).
    plx_ui::widgets::TabStrip::SHAPE,
    "HeaderPop{sp:[Spring{pos:f32,vel:f32};1],focused:Option<u32>}",
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

/// **What this `LibraryScreen` lists** (CONTRACT FREEZE: the enum is final, P4 wires it).
/// `All` is the library's full grid, the page the header sits on; `Collections` (type 18) and
/// `Recommended` (the server's hubs) are the two pages the header's controls push over it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Listing {
    #[default]
    All,
    Collections,
    Recommended,
}

/// A retained page body, also remountable from its entry-owned LibraryMemory.
pub struct LibraryScreen {
    /// Which listing the page shows. Fixed at mount by the `AppArg` that mounted it, so it is not
    /// part of the canon: `LibraryCollections` / `LibraryRecommended` already say it.
    listing: Listing,
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
    /// The names on the header row (the library on view among them) and, last, `MORE` when some
    /// do not fit. Empty for a single library: the row then carries only the controls.
    libraries: Vec<(u32, usize)>,
    /// The directory sections the fold hid behind "+N" (what the Sources list shows).
    folded: Vec<u32>,
    /// The header row as `sync` last built it (`header.rs`): rebuilt only when [`Self::header_key`]
    /// moves (`RowCache`, the one rule the Recommended page and the Watchlist hold their row by).
    /// No row before the first sync.
    header: plx_ui::page_header::RowCache<plx_ui::page_header::HeaderRow>,
    /// The header row's pills: the focus capsule and the selection plate, and the focus pop. Held
    /// HERE and not in the row, so a library switch (which replaces the whole row) does not start
    /// them over: the capsule travels from the old stop to the new.
    header_capsules: plx_ui::widgets::TabStrip,
    header_pop: plx_ui::widgets::CtlPop<1>,
    /// The row's content swap: the Collections count cross-fading in place when a rebuilt row
    /// changes it, the control fading out when the same library's row loses it and in when its
    /// count lands (or dissolving between two x when the count's digits move it). A library
    /// SWITCH cuts instead: the control the old library had is not drawn on the new page. Held beside the capsules
    /// for the same reason (a library switch replaces the row), but unlike them it is NOT hashed
    /// into the canon: the capsules' position steers the next frame, while `collections` is no part
    /// of the screen's shape (a recording never probes), so the swap has nothing to be a function of.
    header_swap: plx_ui::page_header::HeaderSwap,
    /// The library (server, section key) the row in `header` was built for: a rebuild for another
    /// library is a switch (`HeaderSwap::begin_switch`), for the same one the page's own row
    /// changing (`HeaderSwap::begin`). A projection, not canon.
    header_of: Option<(Option<plx_plex::plex::ServerId>, i64)>,
    /// What the Collections control knows (`LibraryScreen::refresh_header` reads
    /// `LibraryLike::collections_count` for the library on view each `sync`): the same library's
    /// last count is held across a read of nothing, another library's never is.
    collections: CollectionsControl,
    /// The Collections page's header carries the Sort control (the server declared two or more
    /// sorts for the collections listing). A projection of `header`, read by `toolbar_elems`.
    collections_sort: bool,
    /// The Collections page asked the store for the collections listing of this epoch: asked once,
    /// so a store change before the answer lands does not ask again. Not canon.
    collections_asked: Option<u32>,
    /// The type the page below was listing when it pushed the Collections page (a TV library may
    /// be on Seasons or Episodes): where the section goes back to when the page is left.
    type_before_collections: Option<plx_data::browse::LibraryType>,
    /// The total of the library's unfiltered listing the page last saw, keyed by the listing it
    /// belongs to: the "312 of 1,284" read-out's denominator. A projection, not canon.
    unfiltered: Option<((LibrarySectionIdentity, u32, plx_data::browse::LibraryType), i64)>,
    /// A library was just chosen with OK: the page seats the name the reader pressed and shows the
    /// top of the page, where the header is, instead of restoring the new library's scroll.
    switched: bool,
    /// A library was chosen with OK and the reader has not entered its grid since: the grid group
    /// is published with [`Seat::RememberedFirst`], so the DOWN that enters it lands on the tile the
    /// reader left in that library (the engine's remembered element, seeded from the store's
    /// snapshot when the engine has none), else on its first row. Every other DOWN from the header
    /// keeps [`Seat::Remembered`]'s rule: the tile under the control it leaves. Not canon, like
    /// `unfiltered`: it is armed by a switch and disarmed by the focus move into the grid, both
    /// already in the recorded stream, and the focus it decides is in every frame's hash, so
    /// leaving it out moves no state shape (`SCREEN_SHAPES_PIN`, the three replay recordings).
    enter_remembered: bool,
    section: Option<LibrarySectionIdentity>,
    epoch: Option<u32>,
    query: Option<u32>,
    grid_reset_pending: bool,
    layout: Layout,
    target_layout: Layout,
    scroll: Spring,
    scroll_target: f32,
    /// The top chrome and the capsule, as drawn: decided from `scroll_target` and run on the swap
    /// clock, in sequence (`plx_ui::scroll_chrome`). Stepped beside the scroll spring.
    handoff: plx_ui::scroll_chrome::Handoff,
    restore_scroll: Option<f32>,
    viewports: Vec<LibraryViewport>,
    /// The grid key an `Enter::Fresh` names to seat, kept projected until the engine has seated it.
    seat_wanted: Option<u32>,
    pending: PendingTransactions,
    page_fade: Xfade,
    grid_fade: Xfade,
    readout: Readout,
    live: bool,
    /// A PAGE has been pushed over this one (`WillLeave(Deeper)`) and it has not come back: not
    /// ticked, not drawn, so the one state in which its listing may be re-fetched without moving
    /// ground under the reader. A modal (a compact menu) covers the page without setting it.
    /// Transient like `seat_wanted`: it is not part of the canon, a pop clears it.
    buried: bool,
    initial: bool,
    /// The head group the page seated its own focus in, while that seat is still the page's own
    /// choice: nobody has moved since. Such a seat follows the head when a landing changes what the
    /// head is (the header row, a status read-out).
    provisional: Option<GroupId>,
    /// Remembered cursors the PAGE wrote by seating itself, which the reader has not confirmed.
    /// The engine remembers every seat alike, so its memory alone cannot tell a restore from the
    /// page's own earlier seat in the same section.
    placed: Vec<(GroupId, u32)>,
    sweep_down: bool,
    // Paint-only state: ambient colours choose neither focus nor activation.
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
    /// The standing "Hold OK for options" hint over a card (`plx_ui::hold_hint`, once per
    /// install). Paint-only: not hashed, not in the shape.
    hold_hint: plx_ui::hold_hint::HoldHint,
}

impl LibraryScreen {
    /// This page listing `listing` instead of the full grid (see [`Listing`]).
    pub fn with_listing(mut self, listing: Listing) -> Self {
        self.listing = listing;
        self
    }

    pub fn listing(&self) -> Listing {
        self.listing
    }

    pub fn new(entry: EntryId, instance: InstanceId, kind: SecKind) -> Self {
        let layout = Layout::new(false, 0);
        Self {
            listing: Listing::All,
            entry, instance, kind, wanted_kind: Some(kind), listed: Default::default(), keys: KeyRegistry::default(),
            pair: MasterDetail::new(
                RailPart::new(entry, RAIL_GROUP), GridPart::new(entry, GRID_GROUP),
                MasterDetailLayout { master: Rect::FULL, detail: Rect::FULL },
                MasterDetailGroups { master: RAIL_GROUP, detail: GRID_GROUP },
                MasterDetailPolicy::new(MasterSide::Right, Follow::Live), Regions,
            ),
            libraries: Vec::new(), folded: Vec::new(), header: plx_ui::page_header::RowCache::new(), header_capsules: plx_ui::widgets::TabStrip::new(), header_pop: plx_ui::widgets::CtlPop::new(), header_swap: plx_ui::page_header::HeaderSwap::new(), header_of: None, collections: CollectionsControl::Unknown, collections_sort: false, collections_asked: None,
            type_before_collections: None, unfiltered: None,
            switched: false, enter_remembered: false, section: None, epoch: None, query: None, grid_reset_pending: false,
            layout, target_layout: layout,
            scroll: Spring::at(0.0), scroll_target: 0.0, handoff: plx_ui::scroll_chrome::Handoff::new(), restore_scroll: None,
            viewports: Vec::new(), seat_wanted: None,
            pending: PendingTransactions::default(), page_fade: Xfade::new(), grid_fade: Xfade::new(),
            readout: Readout::Loading, live: true, buried: false, initial: true, provisional: None, placed: Vec::new(), sweep_down: true,
            ground: plx_ui::widgets::PageGround::new(), ground_seeded: false,
            plaintext: Default::default(),
            clock: Default::default(),
            plaintext_alert: super::plaintext_question::PlaintextAlert::new(PLAINTEXT_GROUP, PLAINTEXT_CANCEL, PLAINTEXT_CONNECT),
            hold_hint: plx_ui::hold_hint::HoldHint::new(),
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

    /// The header row's group: the library names, "+N" and the controls.
    #[cfg(any(test, feature = "test-support"))]
    pub fn toolbar_group(&self) -> GroupId { LIBRARY_GROUP }

    /// The header row's stops in focus order, each named by what it is (`"library"`, `"collections"`,
    /// `"recommended"`, `"sort"`, `"crumb"`): the per-stop probe the app-tier route tables walk.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_header_stops(&self) -> Vec<(&'static str, u32)> {
        self.header_elems().into_iter().map(|elem| (match elem {
            toolbar::COLLECTIONS => "collections",
            toolbar::RECOMMENDED => "recommended",
            toolbar::SORTFILTER => "sort",
            toolbar::CRUMB_ROOT => "crumb",
            MORE => "more",
            _ => "library",
        }, elem)).collect()
    }

    /// The library names on the row as `(element, section index)`, "+N" not among them, and the
    /// section indexes the "+N" list holds: what the fold shows and what it hid.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_names(&self) -> (Vec<(u32, usize)>, Vec<u32>) {
        (self.libraries.iter().filter(|(elem, _)| *elem != MORE).copied().collect(), self.folded.clone())
    }

    /// The A-Z rail: every letter with whether it is empty, and the index of the focused one.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_rail(&self, focus: Option<FocusKey<u32>>) -> (Vec<(String, bool)>, Option<usize>) {
        self.pair.master.probe(focus)
    }

    /// The filter read-out on the header row ("312 of 1,284"), if it carries one.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_readout(&self) -> Option<String> {
        self.header.row().and_then(|row| row.readout()).map(str::to_owned)
    }

    /// The Sort & Filter pill's value on the header row (the text after the dot), if the row has one.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_sort_value(&self) -> Option<String> {
        self.header.row()?.page.spec().right.iter().find(|item| item.fold_glyph).and_then(|item| item.value.clone())
    }

    /// The scroll the page is travelling to: 0 at its top.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_scroll_target(&self) -> f32 { self.scroll_target }

    /// Card `i`'s settled cell with the focused caption band folded in, and the grid's column count:
    /// what a test reads the room the band opened under a row of cards from.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_cell(&self, i: usize) -> (plx_ui::Rect, usize) { (self.pair.detail.probe_cell(i), self.layout.cols()) }

    /// How many header rows this page has built: the build counter of its `RowCache`/`SharedRow`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_header_builds(&self) -> u32 { self.header.builds() }

    /// The library names on the header row as `(element, directory section index)`, the `+N`
    /// overflow stop last (its index is `usize::MAX`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_libraries(&self) -> Vec<(u32, usize)> { self.libraries.clone() }

    /// The directory section index of the library the header's selection plate stands under, read
    /// from the BUILT row (the same `selected_stop` the row's motion follows), `None` when the row
    /// has no plate on a library name (one library: no names).
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_plate_section(&self) -> Option<usize> {
        let stop = self.header.row()?.selected_stop()?;
        self.libraries.iter().find(|(elem, _)| *elem == stop).map(|(_, section)| *section)
    }

    /// `(the focused grid row's poster top on screen at the scroll the page is travelling to, the
    /// first row's poster top at scroll 0, the drawn scroll less the one travelled to)`: the settle
    /// rule says the first two are the same line wherever the row is, bar the last rows of a list,
    /// which rest lower; the third is 0 once the spring has arrived. `None` while focus is not on a
    /// grid card.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_rest_line(&self, focus: Option<FocusKey<u32>>) -> Option<(f32, f32, f32)> {
        let (row, _) = self.grid_position(focus)?;
        Some((self.target_layout.row_y(row, self.scroll_target), self.target_layout.row_y(0, 0.0), self.scroll.pos - self.scroll_target))
    }

    /// Does the page draw the scroll capsule once its chrome has tucked? The one predicate
    /// [`Self::draw`] asks (the header names two or more libraries to restate).
    pub(super) fn capsule_wanted(&self) -> bool { plx_ui::scroll_chrome::capsule_shown(self.libraries.len()) }

    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_capsule_wanted(&self) -> bool { self.capsule_wanted() }

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

    /// Re-fetch the listing when a read-back made its membership stale (the Unwatched switch,
    /// ordering by last viewed) or the server created or removed items: ONLY called while a page
    /// is pushed over this one (`buried`), so the reader's ground never moves under the cursor and
    /// the listing is fresh when the page is shown again. It is the same query refreshed, not a
    /// query edit (`ListingId::query` stands), so the page keeps its scroll and its focus. The
    /// Browse pump that lands the new first page is the application's (`Bridge::reconcile_pump`),
    /// because a buried page is not ticked to pump its own store. A modal over the page (a compact
    /// menu) never triggers it: the page is still visible and ticking.
    fn requery_stale<H: LibraryLike>(&self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        if !H::listing(cx).membership_stale() { return; }
        let Some(target) = self.address(cx) else { return };
        self.store(target, LibraryWork::Requery, fx);
    }

    /// The caption policy the grid reserves its band for: the Under posters setting for every
    /// listing of POSTERS (the Collections page included), and `None`, the legacy band, for the
    /// Episodes listing, whose landscape stills are not on the policy like every other landscape
    /// surface (`registry::captions::still_label` words their 92 px block in the 114 px legacy band,
    /// which the policy's 80 / 40 px gap would run under the next row).
    fn grid_policy(&self) -> Option<plx_ui::caption_policy::CaptionPolicy> {
        (self.listed != plx_data::browse::LibraryType::Episodes).then(parts::caption_policy)
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
            // A pushed Collections page is fixed to the section that opened it: the section moving
            // under it is not a foreign swap to dissolve through (`collections_page::syncs_section`).
            if self.section.is_some() && collections_page::syncs_section(self.listing) {
                self.initial = true;
                // A library chosen with OK fades the GRID (the header stays put), so the section
                // landing under that fade is the page's own transaction, not a foreign swap.
                if !self.page_fade.is_swapping() && !(self.switched && self.grid_fade.is_swapping()) {
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
            self.section = identity.clone();
            self.epoch = epoch;
            let saved = self.viewports.iter().find(|view| Some(view.epoch) == epoch && Some(&view.section) == identity.as_ref());
            self.restore_scroll = saved.map(|view| view.scroll).or_else(||
                cx.focus.remembered(self.pair.groups_config().detail).is_none()
                    .then(|| listing.cursor().map(|cursor| cursor.scroll)).flatten())
                // The reader pressed a name on the header: the page shows the header (the viewport
                // stays saved for when they come back to this library by another door).
                .filter(|_| !self.switched);
            self.scroll.jump(self.restore_scroll.unwrap_or(0.0));
            self.scroll_target = self.scroll.pos;
        }
        self.query = listing.id().map(|id| id.query);
        let table = if directory.sections().is_empty() { directory.discovery() } else { directory.source_fetch() };
        self.readout = readout(table, directory.sections().len(), listing.fetch(), listing.total());
        if let Some(current) = directory.current() {
            let kind = directory.sections()[current].kind;
            if self.wanted_kind.is_none() || self.wanted_kind == Some(kind) {
                self.kind = kind;
                self.wanted_kind = None;
            }
        }
        self.refresh_header(cx);
        if let Some(kind) = self.wanted_kind.filter(|kind|
            directory.current().map(|i| directory.sections()[i].kind) != Some(*kind)) {
            self.pair.detail.clear_projection();
            self.pair.master.clear_projection();
            self.grid_fade = Xfade::new();
            self.readout = if directory.preferred(kind).is_some() { Readout::Loading }
                else { readout(directory.kind_fetch(kind), 0, directory.kind_fetch(kind), -1) };
            self.relayout(cx.focus.current);
            return;
        }
        if collections_page::awaits_own_listing(self.listing, self.listed) {
            // The store still holds the page beneath this one. The grid is empty and parked dark
            // under the loading read-out (nothing to fade out, so the type request commits on the
            // next frame), and the collections fade in once they exist.
            self.pair.detail.clear_projection();
            self.pair.master.clear_projection();
            if !self.grid_fade.is_swapping() { self.grid_fade.mount(); }
            self.readout = Readout::Loading;
            self.relayout(cx.focus.current);
            return;
        }
        self.project_grid(cx);
        self.relayout(cx.focus.current);
        // The relayout hands the grid this frame's scroll, and with it the window it will paint:
        // a cell it can paint or step to must hold a key in the frame, so project again (a
        // no-op when the window did not move).
        self.project_grid(cx);
        self.pair.master.refresh(cx, &mut self.keys);
    }

    /// Is the listing in the store this page's to project and to page through? Not while it holds
    /// for another kind of library (`wanted_kind`), and not while the store still lists the page
    /// beneath it (`collections_page::awaits_own_listing`): asking the driver for windows of a
    /// listing about to be replaced only spends the round trips.
    fn owns_listing(&self) -> bool {
        self.wanted_kind.is_none() && !collections_page::awaits_own_listing(self.listing, self.listed)
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

    fn relayout(&mut self, focus: Option<FocusKey<u32>>) {
        let grid_focus = focus.filter(|key| key.entry == self.entry)
            .and_then(|key| self.pair.detail.index_of(key.elem));
        self.pair.detail.settle_band(grid_focus.map(|index| index / self.layout.cols()));
        let rows = self.pair.detail.total().div_ceil(self.layout.cols());
        // A grid transaction's commit empties the store (`browse::requery`) while the grid is
        // dark and its answer is in flight. The header around it is NOT dark: sizing the document
        // from the empty interim would shrink `max_scroll` and
        // slide the whole page to the clamp and back when the answer lands. Until the answer
        // says otherwise, the dark grid keeps reserving the extent it had.
        let rows = if rows == 0 && self.grid_fade.is_swapping() && self.readout == Readout::Loading {
            self.layout.rows
        } else { rows };
        let episodes = self.layout.is_episodes();
        let band_h = self.grid_policy().as_ref().map_or(plx_ui::cards::LEGACY_BAND_H, plx_ui::caption_policy::band_h);
        self.layout = if self.readout == Readout::Failed {
            Layout::failed(true)
        } else { Layout::new(true, rows) }.with_episodes(episodes).with_band_h(band_h)
            .with_grid_bands(self.pair.detail.band_geometry());
        self.target_layout = Layout::new(true, rows).with_episodes(episodes).with_band_h(band_h)
            .with_grid_focus(grid_focus.map(|index| index / self.layout.cols()));
        if self.grid_reset_pending {
            self.scroll_target = self.target_layout.row_reveal(0);
            self.scroll.jump(self.scroll_target);
        }
        self.layout.status = self.readout == Readout::Failed;
        self.target_layout.status = self.layout.status;
        self.pair.detail.set_geometry(self.layout, self.scroll.pos, self.target_layout, self.scroll_target);
        self.pair.detail.set_band_policy(self.grid_policy());
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

    /// Seat `group` as the page's arrival (`seat_on`). The grid's tile that takes the seat arrives, a
    /// restore's way ([`Grid::arrive`](plx_ui::cards::Grid::arrive)) never: it grows from rest on the
    /// standard lift spring whether the page seated it under the dissolve or when a listing that
    /// answered late landed its cells, and does not appear lifted in one frame.
    fn seat_arrival<H: LibraryLike>(&mut self, group: GroupId, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        if self.provisional == Some(group) && group == self.pair.groups_config().detail {
            self.pair.detail.arrive();
        }
        self.seat_on(group, cx, fx);
    }

    /// **This page's column** (`plx_ui::page_regions`): the header row, the status read-out when
    /// the listing failed and the grid when it has titles, with the A-Z rail beside the grid. The
    /// Library walks up it on BACK; the Collections page, pushed over the Library, does not (it
    /// pops to the page that opened it). One declaration: the links, BACK, the first seat and the
    /// region of a focused key all read it.
    fn with_regions<R>(&self, then: impl FnOnce(&PageRegions<'_>) -> R) -> R {
        let mut content = Vec::with_capacity(2);
        if self.readout == Readout::Failed { content.push(STATUS_GROUP); }
        if self.pair.detail.total() > 0 { content.push(self.pair.groups_config().detail); }
        then(&PageRegions {
            header: Some(LIBRARY_GROUP),
            content: &content,
            rail: Some(self.pair.groups_config().master),
            back_walks_header: self.listing != Listing::Collections,
            arrival: Arrival::Content,
            // The grid's cells are on their way: until they land (or the listing fails or comes back
            // empty) the page seats nothing, instead of a header control whose OK leaves the page.
            pending: (self.readout == Readout::Loading).then_some(self.pair.groups_config().detail),
        })
    }

    /// The group the page seats on its first arrival: its first poster once the grid has one, the
    /// header row when the listing is loaded and empty, the read-out's control when it failed, and
    /// `None` while it is still loading (`PageRegions::first_seat`): then the page holds no seat
    /// (`PageRegions::unseat`).
    fn first_group(&self) -> Option<GroupId> {
        self.with_regions(|regions| regions.first_seat())
    }

    /// The group the page seats once its listing has something to show: a library chosen with OK
    /// leaves focus on the header, where the name is; a grid with cells in view seats its poster;
    /// anything else is the page's first seat. `None` while the listing is still loading.
    fn arrival_group(&self) -> Option<GroupId> {
        match self.layout.seat_for_scroll(self.scroll.pos, self.layout.visible_rows(self.scroll.pos).0) {
            _ if self.switched => Some(LIBRARY_GROUP),
            Some(Block::Grid(_)) => Some(self.pair.groups_config().detail),
            _ => self.first_group(),
        }
    }

    /// Seat `first_group`, or hold the page unseated while its listing is loading: the one place a
    /// page's "arrive in the content" request is made.
    fn reseat_first<H: LibraryLike>(&self, fx: &mut Effects<'_, H>) {
        if let Some(target) = self.with_regions(|regions| regions.first_seat().map(FocusTarget::ContainerGroup).or_else(|| regions.unseat())) {
            self.reseat(target, fx);
        }
    }

    fn current_viewport(&self) -> Option<LibraryViewport> {
        Some(LibraryViewport { epoch: self.epoch?, section: self.section.clone()?, scroll: self.scroll.pos,
            shelves: Vec::new() })
    }

    fn page_memory(&self) -> LibraryMemory {
        let mut memory = self.keys.remember(self.section.clone(), self.scroll.pos, Vec::new());
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
        } else if let Some(top) = self.header_focus().and_then(|focus| focus.reveal(key.elem)) {
            Some(top)
        } else if key.elem == RETRY {
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
        None
    }

    /// The two screen-specific answers the hold hint needs (`ui::hold_hint`, "Adopting it"): the
    /// CARD focus rests on, one whose hold opens an item menu (a grid poster with
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

    /// `(page alpha, focused grid tile's drawn scale)` — what a viewer sees of a focus arrival: the
    /// scale is 1.0 (rest) while the focus is not on a tile, or the page is still dissolving in.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_arrival(&self, focus: Option<FocusKey<u32>>) -> (f32, f32) {
        let key = focus.filter(|key| key.entry == self.entry);
        (self.page_fade.alpha(), self.pair.detail.pop_probe(key.and_then(|key| self.pair.detail.index_of(key.elem))))
    }

    /// The top chrome's and the capsule's drawn alphas, for the armed `focusx` probe line
    /// (`app::bridge`): what a frame dump reads to hold the handoff to its contract.
    pub fn probe_handoff(&self) -> (f32, f32) { (self.handoff.chrome(), self.handoff.capsule()) }

    pub fn probe_viewport(&self, focus: Option<FocusKey<u32>>) -> (&'static str, i32, i32, f32, f32) {
        if let Some((row, col)) = self.grid_position(focus) { return ("grid", row as i32, col as i32, 0.0, self.scroll.pos); }
        let elem = focus.filter(|key| key.entry == self.entry).map(|key| key.elem);
        let region = match elem {
            Some(elem) if self.toolbar_elems().contains(&elem) => "toolbar", Some(RETRY) => "status",
            Some(elem) if region_of_elem(elem) == Some(KeyRegion::Rail) => "rail",
            Some(elem) if elem >= plx_ui::dispatch::STRIP_BASE => "strip",
            Some(_) => "library", None => "none",
        };
        (region, -1, -1, 0.0, self.scroll.pos)
    }

    /// Choose a library: ONLY the grid fades out and back in, under a header that does not move;
    /// the plate lands on the new name at once and focus stays on the name that was pressed.
    fn switch_library<H: LibraryLike>(&mut self, target: SectionTarget, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        self.save_cursor(cx, fx);
        self.pending.request_section(target);
        self.switched = true;
        self.enter_remembered = true;
        self.grid_fade.reload();
        self.refresh_header(cx);
        fx.invalidate(Provenance::Input);
    }

    /// Open the menu `kind`, hanging off the header item `anchor` (which says where it grows from).
    fn open_menu<H: LibraryLike>(&mut self, kind: crate::registry::LibraryMenuKind, anchor: u32,
        cx: &Cx<'_, H>, fx: &mut Effects<'_, H>)
    {
        if let (Some(target), Some(placed)) = (self.requested_address(cx), <Self as Focusable<H>>::place(self, &anchor, cx, At::SpringTarget)) {
            let r = placed.rest_rect;
            fx.push(Fx::App(AppFx::Library(LibraryReq::Menu {
                kind, anchor: [r.x.to_bits(), r.y.to_bits(), r.w.to_bits(), r.h.to_bits()], target,
                folded: if kind == crate::registry::LibraryMenuKind::Sources { self.folded.clone() } else { Vec::new() },
                control: anchor,
            })));
        }
    }

    /// Push this library's `listing` page over the full grid. The pages are one screen in another
    /// `Listing` (`AppArg::LibraryCollections` / `LibraryRecommended`), mounted by the application.
    /// Pressing the control of the page already shown does nothing.
    pub(super) fn open_listing<H: LibraryLike>(&mut self, listing: Listing, _cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        if listing == self.listing { return; }
        let Some(section) = self.section.as_ref().and_then(|section| u32::try_from(section.key).ok()) else { return };
        if listing == Listing::Collections && self.listed != plx_data::browse::LibraryType::Collections {
            self.type_before_collections = Some(self.listed);
        }
        fx.push(Fx::App(AppFx::Library(LibraryReq::OpenListing { listing, section })));
    }

    /// Stage `kind` as the section's listing type, the edit the Sort & Filter menu's Show rows
    /// deliver. False when the listing has no address yet.
    fn request_type<H: LibraryLike>(&mut self, kind: plx_data::browse::LibraryType, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> bool {
        let Some(target) = GridTarget::from_view(H::listing(cx)) else { return false };
        let address = SectionAddress { epoch: target.epoch, sid: target.sid, section: target.section };
        self.step(&ScreenEvent::App(AppMsg::LibraryEdit {
            target: address, edit: plx_data::stores::browse::QueryEdit::LibraryType(kind),
        }), cx, fx);
        true
    }

    /// **The Collections page and the page below it share one store listing** (the Browse store
    /// holds one query per section), so the type is a loan. The Collections page asks for the
    /// section's collections (`type=18`) once when it can address the section; the full grid, on
    /// becoming the top page again, puts the section back on the type it was listing when the
    /// page was pushed (its films when it has none noted). The same call heals a section left on
    /// collections by a run that ended on the Collections page, which the saved view restores.
    fn follow_listing_type<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        use plx_data::browse::LibraryType;
        match self.listing {
            Listing::Collections => {
                let Some(epoch) = self.epoch else { return };
                if self.listed == LibraryType::Collections || self.collections_asked == Some(epoch)
                    || self.pending.grid().is_some() { return; }
                if self.request_type(LibraryType::Collections, cx, fx) { self.collections_asked = Some(epoch); }
            }
            Listing::All => {
                if self.buried || !self.live { return; }
                if self.listed != LibraryType::Collections {
                    // Back on its own type: the loan is repaid.
                    self.type_before_collections = None;
                    return;
                }
                if self.pending.grid().is_some() { return; }
                let kind = self.kind;
                let back = self.type_before_collections
                    .filter(|back| LibraryType::offered(kind).contains(back) && *back != LibraryType::Collections)
                    .unwrap_or(LibraryType::Primary);
                self.request_type(back, cx, fx);
            }
            Listing::Recommended => {}
        }
    }

    fn activate<H: LibraryLike>(&mut self, elem: u32, held: bool, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        // Every activation path (OK, a click, a hold, a scripted menu) is a choice made AT the seat,
        // and none of them moves focus, so no `FocusMoved` will release it.
        self.provisional = None;
        self.placed.retain(|(_, placed)| *placed != elem);
        // A singleton selector never exists (`refresh_header` names 2+ libraries or none), so
        // `MORE` is the only way into the Sources menu: an ordinary library name below always
        // requests a section transition, never a menu.
        match elem {
            // The Collections page lists the library's collections (type 18).
            COLLECTIONS => { self.open_listing(Listing::Collections, cx, fx); return Handled::Yes; }
            // The Recommended page lists the server's hubs for the library.
            RECOMMENDED => { self.open_listing(Listing::Recommended, cx, fx); return Handled::Yes; }
            // The crumb's root link: back to the library page below.
            CRUMB_ROOT if self.listing == Listing::Collections => {
                fx.push(Fx::App(AppFx::Content(crate::registry::ContentReq::Back)));
                return Handled::Yes;
            }
            SORTFILTER => { self.open_menu(crate::registry::LibraryMenuKind::SortFilter, elem, cx, fx); return Handled::Yes; }
            MORE => { self.open_menu(crate::registry::LibraryMenuKind::Sources, elem, cx, fx); return Handled::Yes; }
            _ => {}
        }
        if let Some((_, index)) = self.libraries.iter().find(|(key, _)| *key == elem) {
            let directory = H::directory(cx);
            let index = *index;
            if Some(index) == self.pending.section().map(|section| section.index).or(directory.current()) { return Handled::Yes; }
            if Some(index) == directory.current() && self.pending.section().is_some() {
                self.pending.cancel();
                self.page_fade.cancel();
                self.grid_fade.cancel();
                self.switched = false;
                self.enter_remembered = false;
                self.refresh_header(cx);
                return Handled::Yes;
            }
            let section = &directory.sections()[index];
            if let (Some(epoch), Some(sid)) = (directory.epoch(), section.sid) {
                self.switch_library(SectionTarget {
                    epoch, index,
                    identity: LibrarySectionIdentity { sid, key: section.key }, kind: section.kind,
                }, cx, fx);
            }
            return Handled::Yes;
        }
        if region_of_elem(elem) == Some(KeyRegion::Rail) { self.follow(elem, cx, fx); return Handled::Yes; }
        if let Some(item) = self.focused_item(Some(self.key(elem)), cx).filter(|item| !item.rk.is_empty()) {
            // A grid poster is never a Continue Watching deck card.
            let req = if held {
                LibraryReq::ItemMenu { sid: item.sid, rk: item.rk.clone(), from_deck: false }
            } else { LibraryReq::Detail { sid: item.sid, rk: item.rk.clone(), from_deck: false } };
            fx.push(Fx::App(AppFx::Library(req)));
            return Handled::Yes;
        }
        use plx_ui::widgets::{TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_MOVIES, TAB_KEY_SEARCH, TAB_KEY_SHOWS, TAB_KEY_WATCHLIST};
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
                self.enter_remembered = false;
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
            // The hubs' cards live on the Recommended page.
            LibraryCmd::FocusShelf { .. } => return Handled::No,
            LibraryCmd::ItemMenu => return cx.focus.current.map_or(Handled::No, |key| self.activate(key.elem, true, cx, fx)),
            // The TYPE menu's own edit, delivered the way its row delivers it.
            LibraryCmd::SetType(kind) => {
                if !self.request_type(kind, cx, fx) { return Handled::No; }
                return Handled::Yes;
            }
            LibraryCmd::OpenListing(listing) => {
                self.open_listing(listing, cx, fx);
                return Handled::Yes;
            }
            LibraryCmd::OpenMenu(kind) => {
                use crate::registry::LibraryMenuKind as Kind;
                let anchor = match kind {
                    // The scripted single-purpose menus hang off the one control that replaced them.
                    Kind::Type | Kind::Sort | Kind::Filter | Kind::SortFilter => SORTFILTER,
                    Kind::Sources if self.libraries.iter().any(|(elem, _)| *elem == MORE) => MORE,
                    Kind::Sources | Kind::Genre => return Handled::No,
                };
                self.open_menu(kind, anchor, cx, fx);
                return Handled::Yes;
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
                } else if !self.sweep_down && group == Some(LIBRARY_GROUP) {
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
                    2 | 7 => {
                        self.open_menu(crate::registry::LibraryMenuKind::SortFilter, SORTFILTER, cx, fx);
                        return Handled::Yes;
                    }
                    // OK on the next library's name, as the reader would press it (menus take 3/8).
                    3 | 8 => {
                        let names: Vec<u32> = self.libraries.iter().filter(|(elem, _)| *elem != MORE).map(|(elem, _)| *elem).collect();
                        let chosen = self.pending.section().map(|section| section.index).or(H::directory(cx).current());
                        let at = names.iter().position(|elem| self.libraries.iter().any(|(key, index)| key == elem && Some(*index) == chosen));
                        if let Some(&elem) = at.and_then(|at| names.get((at + 1) % names.len())) {
                            return self.activate(elem, false, cx, fx);
                        }
                    }
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
        handled
    }
}

impl LibraryScreen {
    /// **BACK**, decided by [`PageRegions::back`] and executed here. It walks up the Library one
    /// region at a time and never changes the library: deep in the grid to the top of the page, the
    /// top of the page to the header, the header to the bar, the bar to Home. The Collections page,
    /// pushed over the Library, pops to it from the top of its grid, the header or the bar.
    fn back<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let cancelled = self.page_fade.cancel() | self.grid_fade.cancel();
        if cancelled {
            self.pending.cancel();
            self.switched = false;
            self.enter_remembered = false;
            self.refresh_header(cx);
            return Handled::Yes;
        }
        let focus = cx.focus.current.filter(|key| key.entry == self.entry);
        let step = self.with_regions(|regions| {
            let at = focus.and_then(|key| regions.region_of(key.elem, <Self as Focusable<H>>::group_of(self, &key.elem, cx)));
            regions.back(at, self.scroll_target > 0.5)
        });
        match step {
            BackStep::Content => self.reseat(FocusTarget::ContainerGroup(self.pair.groups_config().detail), fx),
            BackStep::Top => {
                // Deep in the page: back to its top, on the first row when focus is in the grid.
                self.scroll_target = 0.0;
                let first_row = self.grid_position(focus).and_then(|(_, col)|
                    self.pair.detail.elem_for(cx, &mut self.keys, col, self.layout.cols()));
                match first_row {
                    Some(elem) => self.reseat(FocusTarget::Elem(self.key(elem)), fx),
                    None => self.reseat_first(fx),
                }
            }
            BackStep::Header => self.reseat(FocusTarget::ContainerGroup(LIBRARY_GROUP), fx),
            BackStep::Nav => self.reseat(FocusTarget::ContainerGroup(STRIP), fx),
            BackStep::Leave => {
                // The Collections page was pushed over the library, so it pops to it; the library
                // itself goes Home.
                if self.listing == Listing::Collections { fx.push(Fx::App(AppFx::Content(crate::registry::ContentReq::Back))); }
                else { fx.push(Fx::App(AppFx::Library(LibraryReq::BackToHome { kind: self.kind }))); }
            }
        }
        Handled::Yes
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
            ScreenEvent::Mount | ScreenEvent::StoreChanged(..) => {
                self.sync(cx);
                self.follow_listing_type(cx, fx);
                // A read-back that lands while a page is pushed over this one is acted on at once.
                if self.buried { self.requery_stale(cx, fx); }
            }
            ScreenEvent::RestoreMemory(PageMemory::Library(memory)) => { self.restore(memory); self.sync(cx); }
            ScreenEvent::Enter(enter) => {
                self.live = true;
                self.buried = false;
                // The engine seats the target right after this event, so a key it names must hold
                // a projected slot by then: pinned and projected until the page's next tick.
                if let Enter::Fresh { focus: FocusTarget::Elem(key) } = enter {
                    self.seat_wanted = Some(key.elem).filter(|_| key.entry == self.entry);
                }
                self.sync(cx);
                self.follow_listing_type(cx, fx);
            }
            ScreenEvent::Cover => {
                self.live = false;
                if self.buried { self.requery_stale(cx, fx); }
            }
            ScreenEvent::Uncover => {
                self.live = true;
                self.buried = false;
                self.sync(cx);
                self.follow_listing_type(cx, fx);
            }
            ScreenEvent::WillLeave(leave) => {
                if matches!(leave, plx_machine::machine::Leave::Deeper) { self.buried = true; }
                self.save_cursor(cx, fx);
                self.flush(fx);
            }
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
                        self.switch_library(SectionTarget { epoch: target.epoch, index, identity: LibrarySectionIdentity { sid: target.sid, key: target.section }, kind: directory.sections()[index].kind }, cx, fx);
                    }
                }
            }
            ScreenEvent::FocusMoved { from, to, by } => {
                // The grid opens the row's caption band and, for a deliberate move only, grows the
                // tile from rest over the frames after this. A restore or a reconcile announced
                // here is adopted whole by its next tick, unless a dormant tick ran since (a page
                // returning from a Detail push under the held image, `Tick` below), in which case
                // the first awake tick regrows the tile from rest (`ui::cards`, the pop rule).
                self.pair.detail.on(ev, cx, fx, self.scroll.pos);
                // The engine seats a page it has just mounted in the first group it can (the bar), before a
                // page whose listing is still loading has anything to seat. Such a page holds no seat
                // instead (`PageRegions::unseat`); the reader's first key (`initial` is theirs to clear,
                // below) or the cells landing seat it.
                if self.initial && !self.switched && from.is_none() && to.entry == self.entry {
                    if let Some(target) = self.with_regions(|regions| regions.unseat()) { self.reseat(target, fx); }
                }
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
                if to_group == Some(self.pair.groups_config().detail) { self.enter_remembered = false; }
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
                let header_before = self.header.key();
                self.sync(cx);
                // The Collections count (or any other header input) landed this frame.
                if self.header.key() != header_before { fx.invalidate(Provenance::Landing(fx.from())); }
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
                self.pair.master.advance(cx, current_grid, rail::RailHint { scroll_vel: self.scroll.vel, right_edge: rail::on_last_column(current_grid, self.layout.cols()) }, self.live && self.readout == Readout::Grid, tick.dt());
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
                let page_commit = self.page_fade.tick(tick.dt(), ready);
                let grid_commit = self.grid_fade.tick(tick.dt(), ready);
                if page_commit || (grid_commit && (self.switched || self.pending.section().is_none())) { self.flush(fx); }
                if self.live {
                    let dt = tick.dt();
                    let focused = cx.focus.current;
                    let colours = self.focused_item(focused, cx).filter(|item| item.has_blur).map(|item| item.blur)
                        .or_else(|| (!self.ground_seeded).then(|| H::listing(cx).item(0).filter(|item| item.has_blur).map(|item| item.blur)).flatten());
                    self.ground_seeded |= colours.is_some();
                    self.ground.key(colours, plx_ui::widgets::PageGround::CARD_W, dt);
                    self.step_header(cx, dt);
                    self.scroll.step(self.scroll_target, K_SCROLL, dt);
                    // A page still dissolving in shows its seat unpopped: the lift starts on the
                    // dissolve's last frame, where it can be seen, instead of playing out under the
                    // fade or the held image of a route dip (the page arrives in its first poster).
                    self.pair.detail.dormant(self.page_fade.is_arriving() || plx_ui::containers::transition::page_image_held());
                    self.handoff.step(dt, self.scroll.pos, self.scroll_target, self.layout.grid_pitch(), self.capsule_wanted());
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
                    if self.owns_listing() { self.project_grid(cx); }
                    let hint = self.hint_input(cx);
                    self.hold_hint.step(hint, tick.ms, dt, &mut |ev| fx.note(ev));
                    let arrival = if self.initial && self.layout.first().is_some() { self.arrival_group() } else { None };
                    if let Some(group) = arrival.filter(|_| self.seed_cursor(cx, fx)) {
                        self.initial = false;
                        // Only the page's OWN head seat is provisional. Any restore is the reader's:
                        // a restored scroll (a saved viewport or bookmark, at the head or not) names
                        // the block they left, and a remembered cursor in the seated group is where
                        // `Seat::Remembered` puts them back — neither may follow a landing.
                        let restoring = self.restore_scroll.is_some() || cx.focus.remembered(group)
                            .is_some_and(|elem| !self.placed.contains(&(group, elem)));
                        self.provisional = (Some(group) == self.first_group() && !restoring && !self.switched).then_some(group);
                        self.seat_arrival(group, cx, fx);
                        self.switched = false;
                    } else if let Some(seated) = self.provisional.filter(|_| self.layout.first().is_some()) {
                        // A landing moved the head out from under the page's own seat. Follow it, as a
                        // first paint would.
                        let head = self.first_group();
                        let mine = focused.filter(|key| key.entry == self.entry);
                        let at = mine.and_then(|key| <Self as Focusable<H>>::group_of(self, &key.elem, cx));
                        // Focus anywhere but the seat is somebody's move, reported or not yet (the
                        // engine moves before the page hears `FocusMoved`): not the page's to take.
                        // A key of this page that nothing places any more is the seat itself gone
                        // (the status read-out clears as the grid lands), still the page's own.
                        if mine.is_none() || at.is_some_and(|group| group != seated) { self.provisional = None; }
                        else if let Some(head) = head.filter(|head| *head != seated) { self.provisional = Some(head); self.seat_arrival(head, cx, fx); }
                    }
                }
                // The dispatcher ticks the top page beneath a compact menu, but not buried pages.
                // Cover pauses its control motion; it must not stop the query just committed above.
                if let Some(target) = self.address(cx).filter(|_| self.owns_listing()) {
                    let (lo, hi) = self.layout.visible_rows(self.scroll.pos);
                    let focus = cx.focus.current.filter(|key| key.entry == self.entry)
                        .and_then(|key| self.pair.detail.index_of(key.elem));
                    self.store(target, LibraryWork::Want { lo: lo.saturating_sub(1) * self.layout.cols(), hi: (hi + 1) * self.layout.cols(), focus }, fx);
                    self.store(target, LibraryWork::Letters, fx);
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
                // An arrow or a pointer acting on a page that has seated nothing yet is the reader's own
                // move: the cells that land afterwards must not take the seat from it.
                if matches!(input.kind, InputKind::Key { key: Key::Up | Key::Down | Key::Left | Key::Right, edge: Edge::Down, .. }
                    | InputKind::Pointer { .. } | InputKind::Click { .. } | InputKind::Drag { .. }) {
                    self.initial = false;
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
                    return self.back(cx, fx);
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
        // The header row: the names (when there are two or more), "+N" and the controls, or the
        // crumb and Sort. One group, declared by the shared `HeaderFocus`.
        out.extend(self.header_focus().and_then(|focus| focus.group_spec()));
        if self.readout == Readout::Failed {
            if let Some(rect) = self.status_rect(cx) {
                out.push(GroupSpec { edge: [EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop, EdgeRule::Stop],
                    ..row_group(STATUS_GROUP, 1, rect, ElemKind::Control) });
            }
        }
        let content = out.len();
        if self.pair.detail.total() > 0 && self.pair.master.eligible(cx) {
            self.pair.groups(cx, out);
        } else {
            self.pair.detail.groups(cx, out);
        }
        self.with_regions(|regions| regions.seat_content_head(&mut out[content..]));
        // A grid with no cells declares no group; the pending one is declared empty so a fresh enter
        // naming it seats nothing (`PageRegions::unseat`) rather than falling back to the header.
        self.with_regions(|regions| regions.declare_pending(out));
        // After a library was chosen, the DOWN into its grid is the reader's way back to where they
        // were: the engine's remembered tile, across the vertical door that otherwise projects.
        if self.enter_remembered {
            let grid = self.pair.groups_config().detail;
            for spec in out[content..].iter_mut().filter(|spec| spec.id == grid) { spec.seat = Seat::RememberedFirst; }
        }
    }
    fn group_of(&self, elem: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        if let Some(answer) = self.plaintext_alert.group_of(*elem) { return answer; }
        if self.is_header_elem(*elem) { return Some(LIBRARY_GROUP); }
        if self.readout == Readout::Failed && *elem == RETRY { return Some(STATUS_GROUP); }
        self.pair.group_of(elem, cx)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        if let Some(step) = self.plaintext_alert.neighbour(key, dir) { return step; }
        if matches!(region_of_elem(key.elem), Some(KeyRegion::Grid | KeyRegion::Rail)) {
            return self.pair.neighbour(key, dir, cx);
        }
        if let Some(focus) = self.header_focus().filter(|focus| focus.contains(key.elem)) { return focus.neighbour(key, dir); }
        plx_ui::page_regions::row_neighbour(self.entry, &self.row_elems(self.group_of(&key.elem, cx)), key, dir)
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        if let Some(placed) = self.plaintext_alert.place(*elem) { return placed; }
        if matches!(region_of_elem(*elem), Some(KeyRegion::Grid | KeyRegion::Rail)) {
            return self.pair.place(elem, cx, at);
        }
        if let Some(placed) = self.header_focus().and_then(|focus| focus.place(*elem, at)) { return Some(placed); }
        if *elem != RETRY || self.readout != Readout::Failed { return None; }
        let rect = self.status_rect(cx)?;
        Some(Placed { rect, rest_rect: rect, clip: content_clip(), index: None })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if let Some(key) = self.plaintext_alert.reconcile(want) { return key; }
        if matches!(self.keys.region(want.elem).or_else(|| region_of_elem(want.elem)), Some(KeyRegion::Grid | KeyRegion::Rail)) {
            // Ownership is a typed identity query, even when group_of no longer places the key.
            let next = self.pair.reconcile(want, cx);
            if self.pair.place(&next.elem, cx, At::SpringTarget).is_some() { return next; }
        }
        if let Some(key) = self.header_focus().and_then(|focus| focus.reconcile(want)) { return key; }
        if self.place(&want.elem, cx, At::SpringTarget).is_some() { return want; }
        if let Some((group, index)) = self.keys.last_place(want.elem) {
            let elems = self.row_elems(Some(group));
            if let Some(elem) = elems.get(index.min(elems.len().saturating_sub(1))) { return self.key(*elem); }
        }
        // A key nothing places any more lands on the header row, which always has its stops (the
        // grid's keys are projected on demand, so the first seat's group may hold none to name).
        self.row_elems(Some(LIBRARY_GROUP)).first().copied().map(|elem| self.key(elem))
            .unwrap_or_else(|| self.key(plx_ui::dispatch::STRIP_BASE))
    }
    fn seat(&self, group: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if let Some(key) = self.plaintext_alert.seat(group, self.entry) { return key; }
        if group == self.pair.groups_config().detail || group == self.pair.groups_config().master { return self.pair.seat(group, from, cx); }
        // The first way into the header from below lands on the library on view (else the stop
        // nearest the key); once the reader has been on the row, the engine's remembered seat (the
        // last control used) wins first. That rule is the shared row's, not this page's.
        if group == LIBRARY_GROUP {
            return self.header_focus().map_or_else(|| self.key(plx_ui::dispatch::STRIP_BASE), |focus| focus.seat(from));
        }
        // The failed read-out's one control.
        self.key(self.row_elems(Some(group)).first().copied().unwrap_or(plx_ui::dispatch::STRIP_BASE))
    }
}

impl LibraryScreen {
    fn row_elems(&self, group: Option<GroupId>) -> Vec<u32> {
        match group {
            Some(LIBRARY_GROUP) => self.header_elems(),
            Some(STATUS_GROUP) => vec![RETRY],
            Some(_) | None => Vec::new(),
        }
    }
}
/// The panel the page's content is placed and hit-tested in: everything under the top bar.
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
    fn chrome_tuck(&self) -> f32 { self.handoff.tuck() }
    fn state(&self) -> &dyn LogicalState { self }
    fn crumb(&self, _: &Cx<'_, H>) -> Option<Cow<'_, str>> { None }
    fn prepare(&mut self, b: &mut Budget, cx: &Cx<'_, H>) { self.pair.prepare(b, cx); }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) { self.draw_page(f); }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn memory(&self) -> PageMemory {
        PageMemory::Library(self.page_memory())
    }
    fn links(&self, out: &mut Vec<Link>) {
        // The column: bar, header, status, grid. A continuous document has adjacent blocks even
        // while its offscreen geometry passes the floating strip, so geometric ranking must not
        // skip an intervening block; the rail's links are the pair's own.
        self.with_regions(|regions| regions.links(out));
        self.pair.links(out);
    }
    fn holds_arrival(&self, dir: plx_ui::screen::Dir) -> bool {
        self.with_regions(|regions| regions.holds(dir))
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
        self.page_fade.write(c);
        self.grid_fade.write(c);
        self.handoff.write(c);
        self.pair.state().write(c);
        // The pair's ids are registered in `keys` for the section and epoch, all three of which
        // `page_memory` below already carries.
        c.u32(self.pair.groups_config().master.0).u32(self.pair.groups_config().detail.0);
        self.pending.write(c);
        c.bool(self.ground_seeded);
        self.ground.write_motion(c);
        c.bool(self.switched);
        self.header_capsules.write_motion(c);
        self.header_pop.write_motion(c);
        PageMemory::Library(self.page_memory()).write(c);
        c.seq(self.viewports.len());
        for viewport in &self.viewports { viewport.write(c); }
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
