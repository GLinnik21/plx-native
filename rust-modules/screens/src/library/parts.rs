//! The concrete master/detail pair: A–Z rail (master) and portrait or episode listing (detail).
//! The listing is a `plx_ui::cards::Grid` in external-scroll mode: the pop, caption bands, draw,
//! stops, placement and neighbours are the component's, over a `CardSource` read off the published
//! items. This part keeps the publication (element identities, indexes, the known-place memory),
//! the target-layout `SpringTarget` placement, `groups` and `seat`, which describe where the page
//! is going rather than where it is.

pub(super) use super::rail::RailPart;

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::registry::{merged_ranges, row_neighbourhood, tile_facts, LibraryIdentity, LibraryLike, LibrarySectionIdentity};
use plx_ui::caption_policy::CaptionPolicy;
use plx_ui::cards as ui_cards;
use plx_ui::cards::{CardSource, Grid, GridSpec, Tile};
use plx_ui::consts::{MARGIN_X, SCR_H};
use plx_ui::frame::Budget;
use plx_machine::machine::{Cx, Effects, EntryId, FocusKey, GroupId, Host, Measure, ScreenEvent};
use plx_ui::screen::{
    At, AxisMask, Dir, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Part, Placed, Seat,
    Step,
};
use plx_ui::widgets::Art;
use plx_ui::{Painter, Rect};

use super::identity::KeyRegistry;
use super::layout::{GridBand, Layout, CONTENT_TOP, GRID_RIGHT, MAX_GRID_BANDS};

pub(super) const GRID_GROUP: GroupId = GroupId(0x4c49_4201);
pub(super) const RAIL_GROUP: GroupId = GroupId(0x4c49_4202);
const NO_HOLES: &[(usize, usize)] = &[];

pub(super) fn grid_art(item: &plx_data::pms::PmsMovie) -> Art<'_> {
    if item.kind == 3 { Art::Still(Some(tile_facts::of(item))) } else { Art::Poster(Some(tile_facts::of(item))) }
}

/// The standard focused caption policy; a grid sorted by a value draws its value line instead.
pub(super) fn caption_policy() -> CaptionPolicy { crate::registry::captions::policy() }

/// A grid card's label under `policy`: the focused caption band shows exactly what the Under
/// posters setting chose, for every POSTER (a season is named for its show by the one builder, like
/// a season on Home; a collection's details line is its size, and a name its face cuts rides it). An episode is a landscape still, not a poster, and keeps its own block in the
/// legacy band ([`captions::still_label`](crate::registry::captions::still_label)).
pub(super) fn grid_label(item: &plx_data::pms::PmsMovie, policy: &CaptionPolicy) -> ui_cards::TileLabel {
    if item.kind == 3 { return crate::registry::captions::still_label(item, false, ""); }
    crate::registry::captions::label(policy, &tile_facts::caption_facts(item), None, false)
}

#[derive(Default)]
struct GridIndexes {
    elems: HashMap<u32, ElemPositions>,
    known: HashMap<u32, usize>,
}

enum ElemPositions {
    One(usize),
    Many(Vec<usize>),
}

impl GridIndexes {
    fn add_elem(&mut self, elem: u32, index: usize) {
        use std::collections::hash_map::Entry;
        match self.elems.entry(elem) {
            Entry::Vacant(entry) => { entry.insert(ElemPositions::One(index)); }
            Entry::Occupied(mut entry) => match entry.get_mut() {
                ElemPositions::One(old) if *old != index => {
                    let mut positions = vec![*old, index];
                    positions.sort_unstable();
                    *entry.get_mut() = ElemPositions::Many(positions);
                }
                ElemPositions::Many(positions) => {
                    if let Err(at) = positions.binary_search(&index) { positions.insert(at, index); }
                }
                ElemPositions::One(_) => {}
            },
        }
    }

    fn index_of(&self, elem: u32) -> Option<usize> {
        match self.elems.get(&elem)? {
            ElemPositions::One(index) => Some(*index),
            ElemPositions::Many(indices) => indices.first().copied(),
        }
    }
}

#[cfg(test)]
#[derive(Default)]
struct PublicationOps {
    slot_visits: usize,
    known_probes: usize,
}

/// One run of consecutive slots with a key projected: slot `first + n` is `elems[n]`.
#[derive(Default)]
struct Window {
    first: usize,
    elems: Vec<u32>,
}

/// Render cache bounded by the projected windows. A data publication, sort or locale change
/// invalidates formatting; walking focus only changes the shared component's presentation.
#[derive(Default)]
struct CaptionCache {
    language: Option<plx_platform::i18n::Language>,
    format: String,
    sort: String,
    /// The calendar date the labels were worded for: "Today" and "Yesterday" go stale at midnight.
    today: Option<[i32; 3]>,
    labels: HashMap<usize, (usize, ui_cards::TileLabel)>,
}

pub(super) struct GridPart {
    entry: EntryId,
    group: GroupId,
    /// The listing's length: the engine group's length and the document's rows. Navigation is by
    /// index, so this is the TOTAL however few slots hold a key.
    total: usize,
    /// Keys exist only for the wanted ranges (visible rows, the look-ahead, the focus, the
    /// remembered seat): a few runs, never the whole listing.
    windows: Vec<Window>,
    /// The ranges `windows` was projected for, to skip a projection nothing asked to change.
    applied: Vec<Range<usize>>,
    /// The focused and remembered grid keys, which a prune never drops.
    pins: Vec<u32>,
    known: Vec<(u32, usize)>,
    identity: Option<(u32, plx_plex::plex::ServerId, i64, u32)>,
    layout: Layout,
    scroll: f32,
    target_layout: Layout,
    scroll_target: f32,
    /// The poster wall (`ui::cards::Grid`, [`ScrollMode::External`](plx_ui::cards::ScrollMode)):
    /// the focused cell's pop and the let-gos of the cells it left, the caption bands, drawing, stops,
    /// placement and neighbours. The page scroll is the Library's; the grid is handed it
    /// (`Grid::set_page`) before every call. A pop starts from REST when a deliberate move arms it
    /// and when the first awake tick finds a focus no event announced since the last dormant tick;
    /// a restore or reconcile announced after that is adopted at full scale (`ui::cards`, the pop
    /// rule). A page ENTERED under a route dip's held image is dormant, so its focused poster grows
    /// from rest when the image is let go; a page that RETURNS (`Enter(Restored)`, fed to the grid
    /// by `LibraryScreen`) is not put to sleep, so its poster comes back lifted, as it was left
    /// (`Grid::dormant`).
    grid: Grid,
    snapshot: Option<plx_data::stores::browse::ListingSnapshot>,
    indexes: GridIndexes,
    /// Render history for the at-rest poster lookahead (`lookahead.rs`); never logical state.
    rest: super::lookahead::Rest,
    captions: std::cell::RefCell<CaptionCache>,
    #[cfg(test)]
    test_ops: PublicationOps,
}

impl GridPart {
    pub(super) fn clear_projection(&mut self) {
        self.total = 0;
        self.windows.clear();
        self.applied.clear();
        self.indexes.elems.clear();
        self.identity = None;
        self.snapshot = None;
    }

    pub(super) const SHAPE: &'static str = "LibraryGrid{group:u32,total:u32,windows:[(first:u32,elems:[u32])],known:[(elem:u32,index:u32)],identity:Option<(epoch:u32,sid:u32,section:u64,query:u32)>,layout:LibraryLayout,scroll:f32,target_layout:LibraryLayout,scroll_target:f32,pop:(index:Option<u32>,sp:Spring{pos:f32,vel:f32}),let_gos:[(index:u32,sp:Spring{pos:f32,vel:f32})],bands:{focus:Option<u32>,slots:[(row:u32,sp:Spring{pos:f32,vel:f32},card:Option<u32>)]}}";

    pub(super) fn write(&self, c: &mut plx_machine::machine::Canon) {
        // The retained snapshot is a read-publication cache, not another cursor. Its placement
        // projection and identity are traversed below; its Arc address never enters logical state.
        let Self { entry: _, group, total, windows, applied: _, pins: _, known, identity, layout, scroll, target_layout,
            scroll_target, grid, snapshot: _, indexes: _, rest: _, captions: _, #[cfg(test)] test_ops: _ } = self;
        c.u32(group.0).u32(*total as u32).seq(windows.len());
        for w in windows {
            c.u32(w.first as u32).seq(w.elems.len());
            for elem in &w.elems { c.u32(*elem); }
        }
        c.seq(known.len());
        for (elem, index) in known { c.u32(*elem).u32(*index as u32); }
        c.option(*identity, |c, (epoch, sid, section, query)| {
            c.u32(epoch).u32(u32::from(sid.raw())).u64(section as u64).u32(query);
        });
        layout.write(c); c.f32(*scroll);
        target_layout.write(c); c.f32(*scroll_target);
        // canonical animation state, as `PageGround::write_motion`: a spring mid-flight decides
        // the next frames even when two grids draw the same rects
        grid.write_pop(c);
        grid.write_bands(c);
    }

    pub(super) fn new(entry: EntryId, group: GroupId) -> Self {
        let layout = Layout::new(false, 0);
        Self {
            entry, group,
            total: 0,
            windows: Vec::new(),
            applied: Vec::new(),
            pins: Vec::new(),
            known: Vec::new(),
            identity: None,
            layout,
            scroll: 0.0,
            target_layout: layout,
            scroll_target: 0.0,
            // `edge` 0: nothing the document draws is above the screen's top, so no row is culled
            // for being over a snapped scroll's edge (the Library reveals by its own rule)
            grid: Grid::new(entry, GridSpec::new(CONTENT_TOP + layout.grid_top(), 0.0)
                .columns(layout.cols(), layout.style(), MARGIN_X).external()),
            snapshot: None,
            indexes: GridIndexes::default(),
            rest: Default::default(),
            captions: Default::default(),
            #[cfg(test)] test_ops: PublicationOps::default(),
        }
    }

    /// The grid's one entry for the events that move it (`Tick`, `FocusMoved` and the `Enter(Restored)`
    /// that makes it a page returning; the Library keeps
    /// its own press, hold and paging handling, so the grid's `Activate` / `Hold` / `Want` are not
    /// used — [`GridSrc::more`](CardSource::more) is `false`, and the page requests its window
    /// itself). `scroll` is the Library's CURRENT page scroll. Returns the document shift
    /// (pixels, positive = down) a content landing on this tick needs so the focused tile stays
    /// where it was on screen; the caller applies it to its scroll and scroll target.
    pub(super) fn on<H: LibraryLike>(&mut self, ev: &ScreenEvent<H>, cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>, scroll: f32) -> f32 {
        self.grid.set_page(CONTENT_TOP + self.layout.grid_top(), scroll);
        let src = GridSrc { total: self.total, windows: &self.windows, indexes: &self.indexes, view: H::listing(cx), policy: caption_policy(), values: self.layout.value_line(), today: H::today(cx), captions: &self.captions };
        let _ = self.grid.on(ev, cx, &src, fx);
        if matches!(ev, ScreenEvent::Tick(_)) { self.grid.landed_shift() } else { 0.0 }
    }

    /// Hold the grid's cards at rest while the page is still dissolving in ([`Grid::dormant`]); the
    /// lift begins on the first frame after. A page that RETURNS is not held: the grid keeps its
    /// seat lifted through the fade or the image. Set before each [`on`](Self::on).
    pub(super) fn dormant(&mut self, held: bool) { self.grid.dormant(held); }

    /// The focus the page seats next is its arrival, not a restore ([`Grid::arrive`]).
    pub(super) fn arrive(&mut self) { self.grid.arrive(); }

    /// The scale the focused card is drawn at now (see [`Grid::pop_probe`]).
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn pop_probe(&self, index: Option<usize>) -> f32 { self.grid.pop_probe(index) }

    /// The document shift the grid's next tick will report for the cards as they stand now (see
    /// [`Grid::pending_shift`]): a scroll target set against a document that already landed is held
    /// in the coordinates the scroll is still in, so that tick's rebase does not apply it twice.
    pub(super) fn pending_shift<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> f32 {
        self.grid.pending_shift(cx, &self.source(cx))
    }

    /// Adopt the focused grid row's caption band settled: a layout built before the next tick (a
    /// restore, a landing) sizes the document from it. A no-op while that row is already focused.
    pub(super) fn settle_band(&mut self, row: Option<usize>) { self.grid.settle_band(row); }

    pub(super) fn band_geometry(&self) -> [GridBand; MAX_GRID_BANDS] { self.grid.band_geometry() }

    pub(super) fn set_geometry(&mut self, layout: Layout, scroll: f32, target_layout: Layout, scroll_target: f32) {
        self.layout = layout;
        self.scroll = scroll;
        self.target_layout = target_layout;
        self.scroll_target = scroll_target;
        self.grid.set_columns(layout.cols(), layout.style(), MARGIN_X);
        self.grid.set_value_line(layout.value_line());
        self.grid.set_caption_policy(Some(caption_policy()));
        self.grid.set_page(CONTENT_TOP + layout.grid_top(), scroll);
    }

    /// The policy the grid's focused band is sized by when the listing is not on the stored setting
    /// (`None`: the Episodes listing's legacy band). After [`set_geometry`](Self::set_geometry).
    pub(super) fn set_band_policy(&mut self, policy: Option<CaptionPolicy>) {
        self.grid.set_caption_policy(policy);
    }

    pub(super) fn restore_keys(&mut self, keys: &KeyRegistry) {
        self.grid.reset_bands();
        self.known = keys.keys().iter().filter(|key| matches!(key.identity,
            LibraryIdentity::Grid { .. } | LibraryIdentity::GridSlot { .. }))
            .map(|key| (key.elem, key.last_index as usize)).collect();
        self.indexes.known.clear();
        self.indexes.known.reserve(self.known.len());
        for (at, (elem, _)) in self.known.iter().enumerate() {
            self.indexes.known.entry(*elem).or_insert(at);
        }
        self.total = 0;
        self.windows.clear();
        self.applied.clear();
        self.indexes.elems.clear();
        self.identity = None;
        self.snapshot = None;
    }

    /// The listing's length.
    pub(super) fn total(&self) -> usize { self.total }

    /// The focused and remembered grid keys: the prune keeps them, and the screen widens the
    /// projection to their rows.
    pub(super) fn set_pins(&mut self, pins: Vec<u32>) { self.pins = pins; }

    /// Project keys for `ranges` (slot indices; clamped to the listing) and publish them. The
    /// work is the size of the ranges, never the listing's; a projection nothing changed is
    /// skipped.
    pub(super) fn refresh<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, keys: &mut KeyRegistry,
        ranges: Vec<Range<usize>>) {
        let view = H::listing(cx);
        let ranges = merged_ranges(ranges, view.total().max(0) as usize);
        if self.snapshot.as_ref().is_some_and(|old| view.same_items(old.view())) && ranges == self.applied {
            return;
        }
        self.captions.borrow_mut().labels.clear();
        self.snapshot = Some(view.retain());
        self.project(view, keys, ranges);
    }

    fn project(&mut self, view: plx_data::stores::browse::ListingView<'_>, keys: &mut KeyRegistry,
        ranges: Vec<Range<usize>>) {
        let Some(id) = view.id() else {
            if self.identity.is_some() { self.grid.forget_pop(); }
            self.total = 0;
            self.windows.clear();
            self.applied.clear();
            self.indexes.elems.clear();
            self.identity = None;
            return;
        };
        let section = LibrarySectionIdentity { sid: id.sid, key: id.section };
        let stamp = (id.epoch, id.sid, id.section, id.query);
        // A new stamp is a new content set; a covered page's grid never ticked across the swap,
        // so its pop would otherwise read the same element at a new index as a landing.
        if self.identity.is_some_and(|old| old != stamp) { self.grid.forget_pop(); }
        let before: HashSet<u32> = self.windows.iter().flat_map(|w| w.elems.iter().copied()).collect();
        self.total = view.total().max(0) as usize;
        self.windows.clear();
        self.indexes.elems.clear();
        for range in &ranges {
            let mut window = Window { first: range.start, elems: Vec::with_capacity(range.len()) };
            for index in range.clone() {
                let elem = self.publish_slot(view, &section, id.query, index, keys);
                window.elems.push(elem);
                self.indexes.add_elem(elem, index);
                self.remember(elem, index);
            }
            self.windows.push(window);
        }
        self.identity = Some(stamp);
        self.applied = ranges;
        self.prune(keys, &before);
    }

    /// Bound the key table. A key is needed while its card is projected, was projected a pass
    /// ago (a focus whose slot just changed identity recovers through it), or is pinned (the
    /// focus, the remembered seat); above twice the window plus 256 the oldest others go. The
    /// element numbers are minted in order and never reused.
    fn prune(&mut self, keys: &mut KeyRegistry, before: &HashSet<u32>) {
        let size: usize = self.windows.iter().map(|w| w.elems.len()).sum();
        let now = &self.indexes.elems;
        let pinned = &self.pins;
        let dropped = keys.prune(super::identity::KeyRegion::Grid, 2 * size + 256,
            |elem| now.contains_key(&elem) || before.contains(&elem) || pinned.contains(&elem));
        if dropped {
            self.known.retain(|(elem, _)| keys.key(*elem).is_some());
            self.indexes.known.clear();
            for (at, (elem, _)) in self.known.iter().enumerate() {
                self.indexes.known.entry(*elem).or_insert(at);
            }
        }
    }

    /// The key at `index`, projecting its neighbourhood first when it lies outside the windows:
    /// a far seat (jump to a letter, a restore) names the cell BEFORE the scroll reaches it.
    pub(super) fn elem_for<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, keys: &mut KeyRegistry,
        index: usize, cols: usize) -> Option<u32> {
        if let Some(elem) = self.elem_at(index) { return Some(elem); }
        if index >= self.total { return None; }
        let mut ranges = self.applied.clone();
        ranges.push(row_neighbourhood(index, cols));
        let ranges = merged_ranges(ranges, self.total);
        self.project(H::listing(cx), keys, ranges);
        self.elem_at(index)
    }

    fn publish_slot(&mut self, view: plx_data::stores::browse::ListingView<'_>,
        section: &LibrarySectionIdentity, query: u32, index: usize, keys: &mut KeyRegistry) -> u32 {
        #[cfg(test)] { self.test_ops.slot_visits += 1; }
        keys.register(item_identity(view, section, query, index), self.group, index)
    }

    fn remember(&mut self, elem: u32, index: usize) {
        #[cfg(test)] { self.test_ops.known_probes += 1; }
        if let Some(&at) = self.indexes.known.get(&elem) {
            self.known[at].1 = index;
        } else {
            let at = self.known.len();
            self.known.push((elem, index));
            self.indexes.known.insert(elem, at);
        }
    }

    pub(super) fn index_of(&self, elem: u32) -> Option<usize> {
        self.indexes.index_of(elem)
    }

    /// The at-rest poster lookahead (`lookahead.rs`), `ahead` rows deep; `0` is off and touches
    /// nothing. Called every prepared frame. Once the grid has rested and the poster source is
    /// quiet it warms ONE not-yet-held card of the rows just beyond the painted window, nearest
    /// first, through the key the card's own draw resolves, so a later draw is a hit.
    pub(super) fn prepare_ahead<H: LibraryLike>(&mut self, ahead: usize, cx: &Cx<'_, H>) {
        use plx_ui::tex::Warm;
        if ahead == 0 { return; }
        let resting = self.rest.observe(self.scroll, self.scroll_target);
        if !resting || self.total == 0 || !plx_ui::tex::source_idle() { return; }
        let view = H::listing(cx);
        let (len, cols) = (self.total, self.layout.cols());
        let painted = self.painted();
        let rows = len.div_ceil(cols).min(self.layout.rows);
        for index in super::lookahead::candidates(painted, cols, rows, len, ahead, self.rest.forward()) {
            let Some(item) = view.item(index) else { continue };
            let Some((srv, path, w, h)) = plx_ui::widgets::card_art_request(&grid_art(item)) else { continue };
            if plx_ui::tex::warm_ahead_on(srv, path, w, h, false) != Warm::Known { return; }
        }
    }

    /// The run of cards the grid paints now (`Grid::painted`): the rest of the window is buffered.
    pub(super) fn painted(&self) -> std::ops::Range<usize> { self.grid.painted(Painter::root(), self.total) }

    pub(super) fn elem_at(&self, index: usize) -> Option<u32> {
        window_elem(&self.windows, index)
    }

    pub(super) fn fallback_for(&self, elem: u32) -> Option<u32> {
        let index = self.known.get(*self.indexes.known.get(&elem)?)?.1;
        self.elem_at(index.min(self.total.saturating_sub(1)))
    }

    /// The first projected key: where a focus with no place falls back to.
    fn first_elem(&self) -> Option<u32> { self.windows.first().and_then(|w| w.elems.first().copied()) }

    /// How many slots hold a projected key.
    #[cfg(test)]
    pub(super) fn projected(&self) -> usize { self.windows.iter().map(|w| w.elems.len()).sum() }

    #[cfg(test)]
    pub(super) fn swap_for_test(&mut self, a: usize, b: usize) { self.windows[0].elems.swap(a, b); }

    /// Card `i`'s settled cell, the focused band folded in (see [`Grid::cell_probe`]).
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn probe_cell(&self, i: usize) -> plx_ui::Rect { self.grid.cell_probe(i) }

    #[cfg(test)]
    pub(super) fn publication_ops(&self) -> (usize, usize) {
        (self.test_ops.slot_visits, self.test_ops.known_probes)
    }

    #[cfg(test)]
    pub(super) fn reset_publication_ops(&mut self) { self.test_ops = PublicationOps::default(); }

    /// The grid's content, read off the published items. O(1) to build: borrows, no copy.
    fn source<'a, 'v, H: LibraryLike>(&'a self, cx: &Cx<'v, H>) -> GridSrc<'a, 'v> {
        GridSrc { total: self.total, windows: &self.windows, indexes: &self.indexes, view: H::listing(cx), policy: caption_policy(), values: self.layout.value_line(), today: H::today(cx), captions: &self.captions }
    }

    /// The painter the grid draws and registers stops through: the page's alpha, and the stops
    /// clipped to the content panel so a row scrolled up under the heading cannot be hit there.
    fn painter<H: LibraryLike>(f: &plx_ui::screen::DrawFrame<'_, '_, H>) -> Painter {
        f.painter.alpha(f.page_alpha).clipped(grid_clip())
    }

    #[cfg(test)]
    pub(super) fn record_stops<H: LibraryLike>(&self, f: &mut plx_ui::screen::DrawFrame<'_, '_, H>) {
        let p = Self::painter(f);
        let src = self.source(f.cx);
        self.grid.record_stops(f, p, &src);
    }

    /// The opener redraw: the focused card alone, popped and captioned exactly as in-page.
    pub(super) fn draw_focused<H: LibraryLike>(&self, f: &mut plx_ui::screen::DrawFrame<'_, '_, H>, focus: Option<FocusKey<u32>>) {
        let p = f.painter.alpha(f.page_alpha);
        let src = self.source(f.cx);
        self.grid.redraw_focused(f, p, &src, focus);
    }

    /// Card `index`'s drawn rect for the engine focus in `cx` (pop and press folded in).
    #[cfg(test)]
    pub(super) fn rect_at<H: LibraryLike>(&self, cx: &Cx<'_, H>, index: usize) -> Rect {
        let elem = self.elem_at(index).expect("a projected card");
        <Self as Focusable<H>>::place(self, &elem, cx, At::Drawn).expect("a published card places").rect
    }

    /// Card `elem`'s live pop (no press): the scale its rect and its treatment are both built from.
    #[cfg(test)]
    pub(super) fn scale_of<H: LibraryLike>(&self, cx: &Cx<'_, H>, elem: u32) -> Option<f32> {
        self.grid.scale_of(cx, &self.source(cx), &elem)
    }

    /// The cards the scroll can show out of `total`: what `draw` and `record_stops` touch, and so
    /// the cells that must hold a key in the frame. The grid's own function, not a copy of it.
    pub(super) fn window_of(&self, total: usize) -> std::ops::Range<usize> { self.grid.window(total) }

    #[cfg(test)]
    pub(super) fn window(&self) -> std::ops::Range<usize> { self.window_of(self.total) }
}

/// The content panel the grid's stops are clipped to.
fn grid_clip() -> Rect {
    Rect::new(MARGIN_X - 32.0, CONTENT_TOP, GRID_RIGHT - MARGIN_X + 32.0, SCR_H - CONTENT_TOP)
}

#[cfg(test)]
thread_local! {
    /// Every `(index, Tile.scale)` the grid handed `CardSource::overlay`, in paint order: what the
    /// card renderer is given, for tests that assert on it instead of recomputing it.
    pub(super) static OVERLAID: std::cell::RefCell<Vec<(usize, f32)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The grid's `CardSource`: the published items read through the part's element index. Built from
/// the part's FIELDS (borrows only), so a call can hold it while the grid is borrowed mutably.
struct GridSrc<'a, 'v> {
    total: usize,
    windows: &'a [Window],
    indexes: &'a GridIndexes,
    view: plx_data::stores::browse::ListingView<'v>,
    policy: CaptionPolicy,
    /// The grid draws a sort value line under every card rather than the Title/Details caption (an
    /// Episodes view's legacy two-line caption included).
    values: bool,
    /// The calendar date the application handed in (`LibraryLike::today`).
    today: Option<[i32; 3]>,
    captions: &'a std::cell::RefCell<CaptionCache>,
}

impl GridSrc<'_, '_> {
    /// The listing is the section's collections: the Collections page's tiles (caption "N films",
    /// smart mark) rather than the library's.
    fn collections(&self) -> bool { self.view.library_type() == plx_data::browse::LibraryType::Collections }

    /// The label the grid draws under tile `i`: the one [`CardSource::label`] answers.
    fn with_tile_label<R>(&self, i: usize, read: impl FnOnce(&ui_cards::TileLabel) -> R) -> R {
        let Some(item) = self.view.item(i) else { return read(&ui_cards::TileLabel::default()) };
        if self.values {
            let key = self.view.sorts().get(self.view.sort_index()).map_or("", |sort| sort.key.as_str());
            let locale = plx_platform::i18n::current();
            let mut cache = self.captions.borrow_mut();
            if cache.language != Some(locale.language()) || cache.format != locale.format_locale() || cache.sort != key
                || cache.today != self.today {
                cache.labels.clear(); cache.language = Some(locale.language());
                cache.format = locale.format_locale().to_owned(); cache.sort = key.to_owned();
                cache.today = self.today;
            }
            let owner = item as *const plx_data::pms::PmsMovie as usize;
            if let Some((_, label)) = cache.labels.get(&i).filter(|(old, _)| *old == owner) {
                return read(label);
            }
            let entry = cache.labels.entry(i).or_insert_with(|| (owner, crate::registry::sort_info::label(item, key, self.today)));
            if entry.0 != owner { *entry = (owner, crate::registry::sort_info::label(item, key, self.today)); }
            return read(&entry.1);
        }
        read(&grid_label(item, &self.policy))
    }
}

impl<H: Host<Elem = u32>> CardSource<H> for GridSrc<'_, '_> {
    fn len(&self) -> usize { self.total }
    fn elem(&self, i: usize) -> u32 { window_elem(self.windows, i).unwrap_or(0) }
    fn index_of(&self, e: &u32) -> Option<usize> { self.indexes.index_of(*e) }
    fn art(&self, i: usize) -> Art<'_> {
        self.view.item(i).map_or(Art::Poster(None), grid_art)
    }
    fn label(&self, i: usize) -> ui_cards::TileLabel { self.with_tile_label(i, Clone::clone) }
    fn with_label<R>(&self, i: usize, read: impl FnOnce(&ui_cards::TileLabel) -> R) -> R { self.with_tile_label(i, read) }
    fn progress(&self, i: usize) -> Option<f32> {
        self.view.item(i).filter(|item| item.kind != 3).and_then(|item| item.resume_frac())
    }
    fn overlay(&self, p: Painter, i: usize, tile: &Tile, measure: &dyn Measure) {
        #[cfg(test)] OVERLAID.with(|seen| seen.borrow_mut().push((i, tile.scale)));
        if let Some(item) = self.view.item(i).filter(|item| item.kind == 3) {
            plx_ui::widgets::still_overlay(p, &tile_facts::of(item), tile.rect, tile.radius, false, measure);
        }
        // A smart collection wears its mark, focused or not, on the popped tile's rect.
        if self.collections() && self.view.item(i).is_some_and(|item| item.smart) {
            plx_ui::collection_tile::draw_smart_mark(p, tile.rect);
        }
    }
    // Metadata still pages ahead via `LibraryWork::Want`; a slot whose page has not landed draws
    // the skeleton of a card whose poster has not loaded (`art` is `Poster(None)`), because the
    // listing's `total` says the item exists: a hole in the grid reads as "nothing there".
}

fn window_elem(windows: &[Window], index: usize) -> Option<u32> {
    windows.iter().find(|w| index >= w.first && index < w.first + w.elems.len())
        .map(|w| w.elems[index - w.first])
}

fn item_identity(
    view: plx_data::stores::browse::ListingView<'_>,
    section: &LibrarySectionIdentity,
    query: u32,
    index: usize,
) -> LibraryIdentity {
    match view.item(index) {
        Some(item) if !item.rk.is_empty() => LibraryIdentity::Grid {
            section: section.clone(),
            sid: item.sid,
            rk: item.rk.clone(),
        },
        _ => LibraryIdentity::GridSlot {
            section: section.clone(),
            query,
            slot: index as u32,
        },
    }
}

impl<H: LibraryLike> Focusable<H> for GridPart {
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if self.total == 0 { return; }
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Grid { cols: self.target_layout.cols(), holes: NO_HOLES },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(MARGIN_X, self.target_layout.row_y(0, self.scroll_target), GRID_RIGHT - MARGIN_X, self.target_layout.card_h()),
            len: self.total,
            elem: ElemKind::Card,
        });
    }

    fn group_of(&self, key: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        self.index_of(*key).map(|_| self.group)
    }

    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        self.grid.neighbour::<H, _>(&self.source(cx), key, dir)
    }

    /// `At::Drawn` is the grid's own placement (the rect `draw` paints and the stop registers);
    /// `At::SpringTarget` is where the document is GOING — the settled bands at the scroll target
    /// — which the grid, holding the live page, cannot say. The clip is the content panel's.
    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let index = self.index_of(*key)?;
        let clip = grid_clip();
        match at {
            At::Drawn => self.grid.place(cx, &self.source(cx), key, at).map(|placed| Placed { clip, ..placed }),
            At::SpringTarget => {
                let focused = cx.focus.current.is_some_and(|focus| focus.entry == self.entry && focus.elem == *key);
                let layout = self.target_layout;
                let rect = Rect::new(layout.cell_x(index % layout.cols()),
                    layout.row_y(index / layout.cols(), self.scroll_target), layout.card_w(), layout.card_h())
                    .scaled(if focused { layout.style().focus_scale } else { 1.0 });
                Some(Placed { rect, rest_rect: rect, clip, index: Some(index as u32) })
            }
        }
    }

    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.index_of(want.elem).is_some() { return want; }
        self.fallback_for(want.elem)
            .or_else(|| self.first_elem())
            .map(|elem| FocusKey { entry: want.entry, elem })
            .unwrap_or(want)
    }

    /// The cell a crossing lands on, projected from where the source stands (§7.3 step 4,
    /// `column_near_x`'s contract): the column whose centre is nearest the source's, in the row
    /// nearest it — the first row from the heading above. A source's own `index` names a slot
    /// in ITS container, never one of these cells.
    fn seat(&self, _group: GroupId, from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        let layout = self.target_layout;
        let cols = layout.cols();
        let rows = self.total.div_ceil(cols).max(1);
        let half_w = layout.card_w() / 2.0;
        let col = (0..cols)
            .min_by(|&a, &b| {
                let d = |c: usize| (layout.cell_x(c) + half_w - from.rect.cx()).abs();
                d(a).total_cmp(&d(b))
            })
            .unwrap_or(0);
        // Rows are one pitch apart but for the focused row's caption band, so the pitch estimate
        // is at most a row long; settle it on the nearest centre.
        let centre = |row: usize| layout.row_y(row, self.scroll_target) + layout.card_h() / 2.0;
        let estimate = ((from.rect.cy() - centre(0)) / layout.grid_pitch()).round().max(0.0) as usize;
        let row = [estimate.saturating_sub(1), estimate, estimate + 1]
            .into_iter()
            .map(|row| row.min(rows - 1))
            .min_by(|&a, &b| (centre(a) - from.rect.cy()).abs().total_cmp(&(centre(b) - from.rect.cy()).abs()))
            .unwrap_or(0);
        let index = (row * cols + col).min(self.total.saturating_sub(1));
        FocusKey { entry: self.entry, elem: self.elem_at(index).unwrap_or(0) }
    }
}

impl<H: LibraryLike> Part<H> for GridPart {
    fn prepare(&mut self, _budget: &mut Budget, cx: &Cx<'_, H>) { self.prepare_ahead(super::lookahead::rows(), cx); }

    fn draw(&mut self, f: &mut plx_ui::screen::DrawFrame<'_, '_, H>, _rect: Rect) {
        // Metadata still pages ahead via LibraryWork::Want. Hidden artwork waits until visible
        // (`Grid` paints only cards `paint_visible`): repeated warms recycle cold cache slots and
        // keep idle uploading. The one exception is the at-rest lookahead (`prepare`, on by
        // default), whose own slots the source protects. The grid also registers the stops of the
        // cards it draws.
        let p = Self::painter(f);
        let src = self.source(f.cx);
        self.grid.draw(f, p, &src);
    }
}

#[cfg(test)]
mod caption_cache_review_tests {
    use super::*;
    use plx_data::{browse::view::ListingSnapshot, pms::PmsMovie};
    use plx_platform::i18n::{language_on_this_thread_for_test, Preference};

    #[test]
    fn review_caption_cache_borrows_words_and_invalidates_stale_publications() {
        let _guard = plx_base::testlock::serial();
        let _locale = language_on_this_thread_for_test(Preference::En);
        let listing = ListingSnapshot::fixture(plx_plex::plex::ServerId::from_raw(0),
            vec![Some(PmsMovie { title: "Original".into(), year: 1997, ..Default::default() })], vec![]).with_sort_key("year");
        let cache = std::cell::RefCell::new(CaptionCache::default());
        let indexes = GridIndexes::default();
        let mut src = GridSrc { total: 1, windows: &[], indexes: &indexes, view: listing.view(),
            policy: CaptionPolicy::default(), values: true, today: Some([2026, 10, 30]), captions: &cache };
        let pointer = |label: &ui_cards::TileLabel| label.value.as_ref().unwrap().text.as_ptr() as usize;
        let first = src.with_tile_label(0, pointer);
        assert_eq!(first, pointer(&cache.borrow().labels[&0].1), "read the cached CString itself");
        assert_eq!(src.with_tile_label(0, pointer), first);
        assert_eq!(cache.borrow().labels.len(), 1);
        cache.borrow_mut().sort = "obsoleteSort".into();
        src.with_tile_label(0, |_| {});
        assert_eq!(cache.borrow().sort, "year");
        {
            let _ru = language_on_this_thread_for_test(Preference::Ru);
            src.with_tile_label(0, |_| {});
            assert_eq!(cache.borrow().language, Some(plx_platform::i18n::current().language()));
        }
        let changed = listing.clone().with_page(0, vec![PmsMovie { title: "Replaced".into(), ..Default::default() }]);
        src.view = changed.view();
        let owner = cache.borrow().labels[&0].0;
        src.with_tile_label(0, |_| {});
        assert_ne!(cache.borrow().labels[&0].0, owner, "a replaced item is worded again, keyed on the item it was worded for");
        assert_eq!(cache.borrow().labels.len(), 1);
        // The day turning over invalidates the labels: "Today" must not outlive midnight.
        cache.borrow_mut().labels.get_mut(&0).unwrap().1.value.as_mut().unwrap().text = "stale".into();
        assert_eq!(src.with_tile_label(0, |label| label.value.as_ref().unwrap().text.clone()), "stale",
            "the same date reads the cached label");
        src.today = Some([2026, 10, 31]);
        assert_ne!(src.with_tile_label(0, |label| label.value.as_ref().unwrap().text.clone()), "stale",
            "a new date rebuilds the label");
        assert_eq!(cache.borrow().today, Some([2026, 10, 31]));
    }
}
