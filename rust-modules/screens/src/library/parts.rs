//! The concrete master/detail pair: A–Z rail (master) and portrait or episode listing (detail).
//! The listing is a `plx_ui::cards::Grid` in external-scroll mode: the pop, caption bands, draw,
//! stops, placement and neighbours are the component's, over a `CardSource` read off the published
//! items. This part keeps the publication (element identities, indexes, the known-place memory),
//! the target-layout `SpringTarget` placement, `groups` and `seat`, which describe where the page
//! is going rather than where it is.

pub(super) use super::rail::RailPart;

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::registry::{tile_facts, LibraryIdentity, LibraryLike, LibrarySectionIdentity};
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

/// One label construction for the normal and modal-lifted focused grid card.
pub(super) fn grid_label(item: &plx_data::pms::PmsMovie) -> ui_cards::TileLabel {
    if item.kind == 3 {
        let name = if item.title.is_empty() || item.title == item.show_title {
            plx_ui::fmt::episode_address(item.season_index as i64, item.ep_index as i64)
        } else { item.title.clone() };
        // The shared still overlay already names the show and episode address on the artwork.
        // Focus reveals the episode title and release date, as it does on an episode shelf.
        return if item.aired.is_empty() && item.year <= 0 { ui_cards::TileLabel::title(&name) }
        else { ui_cards::TileLabel::titled(&name,
            &plx_ui::fmt::pretty_date(&item.aired, item.year as i64)) };
    }
    ui_cards::poster_label(&tile_facts::of(item))
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

/// The slot ranges to project, sorted and merged, clamped to `total`.
fn merged(mut ranges: Vec<Range<usize>>, total: usize) -> Vec<Range<usize>> {
    ranges.retain_mut(|r| { r.end = r.end.min(total); r.start < r.end });
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::new();
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
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
    /// the focused cell's pop and the previous one's let-go, the caption bands, drawing, stops,
    /// placement and neighbours. The page scroll is the Library's; the grid is handed it
    /// (`Grid::set_page`) before every call. A pop starts from REST only when a deliberate move
    /// arms it; every other way a cell becomes focused is adopted at full scale, because a page
    /// coming back from a Detail push must land exactly as it was left.
    grid: Grid,
    snapshot: Option<plx_data::stores::browse::ListingSnapshot>,
    indexes: GridIndexes,
    /// Render history for the at-rest poster lookahead (`lookahead.rs`); never logical state.
    rest: super::lookahead::Rest,
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

    pub(super) const SHAPE: &'static str = "LibraryGrid{group:u32,total:u32,windows:[(first:u32,elems:[u32])],known:[(elem:u32,index:u32)],identity:Option<(epoch:u32,sid:u32,section:u64,query:u32)>,layout:LibraryLayout,scroll:f32,target_layout:LibraryLayout,scroll_target:f32,pop:(index:Option<u32>,sp:Spring{pos:f32,vel:f32}),shrink:(index:Option<u32>,sp:Spring{pos:f32,vel:f32}),bands:{focus:Option<u32>,slots:[(row:u32,sp:Spring{pos:f32,vel:f32})]}}";

    pub(super) fn write(&self, c: &mut plx_machine::machine::Canon) {
        // The retained snapshot is a read-publication cache, not another cursor. Its placement
        // projection and identity are traversed below; its Arc address never enters logical state.
        let Self { entry: _, group, total, windows, applied: _, pins: _, known, identity, layout, scroll, target_layout,
            scroll_target, grid, snapshot: _, indexes: _, rest: _, #[cfg(test)] test_ops: _ } = self;
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
        let layout = Layout::new(false, &[], 0, false);
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
            #[cfg(test)] test_ops: PublicationOps::default(),
        }
    }

    /// The grid's one entry for the events that move it (`Tick`, `FocusMoved`; the Library keeps
    /// its own press, hold and paging handling, so the grid's `Activate` / `Hold` / `Want` are not
    /// used — [`GridSrc::more`](CardSource::more) is `false`, and the page requests its window
    /// itself). `scroll` is the Library's CURRENT page scroll. Returns the document shift
    /// (pixels, positive = down) a content landing on this tick needs so the focused tile stays
    /// where it was on screen; the caller applies it to its scroll and scroll target.
    pub(super) fn on<H: LibraryLike>(&mut self, ev: &ScreenEvent<H>, cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>, scroll: f32) -> f32 {
        self.grid.set_page(CONTENT_TOP + self.layout.grid_top(), scroll);
        let src = GridSrc { total: self.total, windows: &self.windows, indexes: &self.indexes, view: H::listing(cx) };
        let _ = self.grid.on(ev, cx, &src, fx);
        if matches!(ev, ScreenEvent::Tick(_)) { self.grid.landed_shift() } else { 0.0 }
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
        self.grid.set_page(CONTENT_TOP + layout.grid_top(), scroll);
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
        let ranges = merged(ranges, view.total().max(0) as usize);
        if self.snapshot.as_ref().is_some_and(|old| view.same_items(old.view())) && ranges == self.applied {
            return;
        }
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
        let row = index / cols.max(1);
        let mut ranges = self.applied.clone();
        ranges.push(row.saturating_sub(1) * cols..(row + 2) * cols);
        let ranges = merged(ranges, self.total);
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

    #[cfg(test)]
    pub(super) fn publication_ops(&self) -> (usize, usize) {
        (self.test_ops.slot_visits, self.test_ops.known_probes)
    }

    #[cfg(test)]
    pub(super) fn reset_publication_ops(&mut self) { self.test_ops = PublicationOps::default(); }

    /// The grid's content, read off the published items. O(1) to build: borrows, no copy.
    fn source<'a, 'v, H: LibraryLike>(&'a self, cx: &Cx<'v, H>) -> GridSrc<'a, 'v> {
        GridSrc { total: self.total, windows: &self.windows, indexes: &self.indexes, view: H::listing(cx) }
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
}

impl<H: Host<Elem = u32>> CardSource<H> for GridSrc<'_, '_> {
    fn len(&self) -> usize { self.total }
    fn elem(&self, i: usize) -> u32 { window_elem(self.windows, i).unwrap_or(0) }
    fn index_of(&self, e: &u32) -> Option<usize> { self.indexes.index_of(*e) }
    fn art(&self, i: usize) -> Art<'_> {
        self.view.item(i).map_or(Art::Poster(None), grid_art)
    }
    fn label(&self, i: usize) -> ui_cards::TileLabel {
        self.view.item(i).map(grid_label).unwrap_or_default()
    }
    fn progress(&self, i: usize) -> Option<f32> {
        self.view.item(i).filter(|item| item.kind != 3).and_then(|item| item.resume_frac())
    }
    fn overlay(&self, p: Painter, i: usize, tile: &Tile, measure: &dyn Measure) {
        #[cfg(test)] OVERLAID.with(|seen| seen.borrow_mut().push((i, tile.scale)));
        if let Some(item) = self.view.item(i).filter(|item| item.kind == 3) {
            plx_ui::widgets::still_overlay(p, &tile_facts::of(item), tile.rect, tile.radius, false, measure);
        }
    }
    /// Metadata still pages ahead via `LibraryWork::Want`; a slot whose page has not landed draws
    /// nothing (its stop still registers).
    fn loaded(&self, i: usize) -> bool { self.view.item(i).is_some() }
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
