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
use plx_ui::card_row;
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
pub(super) fn grid_label(item: &plx_data::pms::PmsMovie) -> card_row::TileLabel {
    if item.kind == 3 {
        let name = if item.title.is_empty() || item.title == item.show_title {
            plx_ui::fmt::episode_address(item.season_index as i64, item.ep_index as i64)
        } else { item.title.clone() };
        // The shared still overlay already names the show and episode address on the artwork.
        // Focus reveals the episode title and release date, as it does on an episode shelf.
        return if item.aired.is_empty() && item.year <= 0 { card_row::TileLabel::title(&name) }
        else { card_row::TileLabel::titled(&name,
            &plx_ui::fmt::pretty_date(&item.aired, item.year as i64)) };
    }
    card_row::poster_label(&tile_facts::of(item))
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

    fn remove_elem(&mut self, elem: u32, index: usize) {
        let mut remove = false;
        if let Some(positions) = self.elems.get_mut(&elem) {
            match positions {
                ElemPositions::One(at) => remove = *at == index,
                ElemPositions::Many(at) => {
                    if let Ok(found) = at.binary_search(&index) { at.remove(found); }
                    if at.len() == 1 { *positions = ElemPositions::One(at[0]); }
                }
            }
        }
        if remove { self.elems.remove(&elem); }
    }

    fn index_of(&self, elem: u32) -> Option<usize> {
        match self.elems.get(&elem)? {
            ElemPositions::One(index) => Some(*index),
            ElemPositions::Many(indices) => indices.first().copied(),
        }
    }

    fn last_index_of(&self, elem: u32) -> Option<usize> {
        match self.elems.get(&elem)? {
            ElemPositions::One(index) => Some(*index),
            ElemPositions::Many(indices) => indices.last().copied(),
        }
    }
}

#[cfg(test)]
#[derive(Default)]
struct PublicationOps {
    slot_visits: usize,
    known_probes: usize,
}

pub(super) struct GridPart {
    entry: EntryId,
    group: GroupId,
    pub(super) elems: Vec<u32>,
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
    #[cfg(test)]
    test_ops: PublicationOps,
}

impl GridPart {
    pub(super) fn clear_projection(&mut self) {
        self.elems.clear();
        self.indexes.elems.clear();
        self.identity = None;
        self.snapshot = None;
    }

    pub(super) const SHAPE: &'static str = "LibraryGrid{group:u32,elems:[u32],known:[(elem:u32,index:u32)],identity:Option<(epoch:u32,sid:u32,section:u64,query:u32)>,layout:LibraryLayout,scroll:f32,target_layout:LibraryLayout,scroll_target:f32,pop:(index:Option<u32>,sp:Spring{pos:f32,vel:f32}),shrink:(index:Option<u32>,sp:Spring{pos:f32,vel:f32}),bands:{focus:Option<u32>,slots:[(row:u32,sp:Spring{pos:f32,vel:f32})]}}";

    pub(super) fn write(&self, c: &mut plx_machine::machine::Canon) {
        // The retained snapshot is a read-publication cache, not another cursor. Its placement
        // projection and identity are traversed below; its Arc address never enters logical state.
        let Self { entry: _, group, elems, known, identity, layout, scroll, target_layout,
            scroll_target, grid, snapshot: _, indexes: _, #[cfg(test)] test_ops: _ } = self;
        c.u32(group.0).seq(elems.len());
        for elem in elems { c.u32(*elem); }
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
            elems: Vec::new(),
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
        let src = GridSrc { elems: &self.elems, indexes: &self.indexes, view: H::listing(cx) };
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
        self.elems.clear();
        self.indexes.elems.clear();
        self.identity = None;
        self.snapshot = None;
    }

    pub(super) fn refresh<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, keys: &mut KeyRegistry) {
        let view = H::listing(cx);
        if self.snapshot.as_ref().is_some_and(|old| view.same_items(old.view())) { return; }
        let changed = self.snapshot.as_ref().and_then(|old|
            view.changed_page_ranges(old.view()).map(|ranges| ranges.collect::<Vec<_>>()));
        self.snapshot = Some(view.retain());
        let Some(id) = view.id() else {
            if self.identity.is_some() { self.grid.forget_pop(); }
            self.elems.clear();
            self.indexes.elems.clear();
            self.identity = None;
            return;
        };
        let section = LibrarySectionIdentity { sid: id.sid, key: id.section };
        let total = view.total().max(0) as usize;
        let stamp = (id.epoch, id.sid, id.section, id.query);
        // A new stamp is a new content set; a covered page's grid never ticked across the swap,
        // so its pop would otherwise read the same element at a new index as a landing.
        if self.identity.is_some_and(|old| old != stamp) { self.grid.forget_pop(); }
        if self.identity != Some(stamp) || self.elems.len() != total || changed.is_none() {
            self.elems.clear();
            self.elems.reserve(total);
            self.indexes.elems.clear();
            self.indexes.elems.reserve(total);
            for index in 0..total {
                let elem = self.publish_slot(view, &section, id.query, index, keys);
                self.elems.push(elem);
                self.indexes.add_elem(elem, index);
                self.remember(elem, index);
            }
            self.identity = Some(stamp);
        } else {
            let mut affected = HashSet::new();
            for range in changed.unwrap_or_default() {
                affected.extend(self.elems[range.clone()].iter().copied());
                self.replace_range(view, &section, id.query, range.clone(), keys);
                affected.extend(self.elems[range].iter().copied());
            }
            // Full projection visits every occurrence in order: first is the active position,
            // last is recovery metadata. An unchanged page may contain that last occurrence.
            // Finalize only affected identities after ALL pages, including identities whose
            // highest occurrence was removed. Absent identities retain their prior tombstone.
            // These writes touch existing entries only; set iteration cannot alter vector order.
            for elem in affected {
                if let Some(index) = self.indexes.last_index_of(elem) {
                    if keys.last_place(elem) != Some((self.group, index)) {
                        keys.update_last_place(elem, self.group, index);
                        self.remember(elem, index);
                    }
                }
            }
        }
    }

    fn publish_slot(&mut self, view: plx_data::stores::browse::ListingView<'_>,
        section: &LibrarySectionIdentity, query: u32, index: usize, keys: &mut KeyRegistry) -> u32 {
        #[cfg(test)] { self.test_ops.slot_visits += 1; }
        keys.register(item_identity(view, section, query, index), self.group, index)
    }

    fn replace_range(&mut self, view: plx_data::stores::browse::ListingView<'_>,
        section: &LibrarySectionIdentity, query: u32, range: Range<usize>, keys: &mut KeyRegistry) {
        // Remove the whole old page first so a reorder within it cannot temporarily make a moved
        // identity resolve to the wrong occurrence.
        for index in range.clone() {
            self.indexes.remove_elem(self.elems[index], index);
        }
        for index in range {
            let elem = self.publish_slot(view, section, query, index, keys);
            self.elems[index] = elem;
            self.indexes.add_elem(elem, index);
            self.remember(elem, index);
        }
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

    pub(super) fn elem_at(&self, index: usize) -> Option<u32> {
        self.elems.get(index).copied()
    }

    pub(super) fn fallback_for(&self, elem: u32) -> Option<u32> {
        let index = self.known.get(*self.indexes.known.get(&elem)?)?.1;
        self.elems.get(index.min(self.elems.len().saturating_sub(1))).copied()
    }

    #[cfg(test)]
    pub(super) fn publication_ops(&self) -> (usize, usize) {
        (self.test_ops.slot_visits, self.test_ops.known_probes)
    }

    #[cfg(test)]
    pub(super) fn reset_publication_ops(&mut self) { self.test_ops = PublicationOps::default(); }

    /// The grid's content, read off the published items. O(1) to build: borrows, no copy.
    fn source<'a, 'v, H: LibraryLike>(&'a self, cx: &Cx<'v, H>) -> GridSrc<'a, 'v> {
        GridSrc { elems: &self.elems, indexes: &self.indexes, view: H::listing(cx) }
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
        let elem = self.elems[index];
        <Self as Focusable<H>>::place(self, &elem, cx, At::Drawn).expect("a published card places").rect
    }

    /// Card `elem`'s live pop (no press): the scale its rect and its treatment are both built from.
    #[cfg(test)]
    pub(super) fn scale_of<H: LibraryLike>(&self, cx: &Cx<'_, H>, elem: u32) -> Option<f32> {
        self.grid.scale_of(cx, &self.source(cx), &elem)
    }

    /// The cards the scroll can show: what `draw` and `record_stops` touch.
    #[cfg(test)]
    pub(super) fn window(&self) -> std::ops::Range<usize> { self.grid.window(self.elems.len()) }
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
    elems: &'a [u32],
    indexes: &'a GridIndexes,
    view: plx_data::stores::browse::ListingView<'v>,
}

impl<H: Host<Elem = u32>> CardSource<H> for GridSrc<'_, '_> {
    fn len(&self) -> usize { self.elems.len() }
    fn elem(&self, i: usize) -> u32 { self.elems.get(i).copied().unwrap_or(0) }
    fn index_of(&self, e: &u32) -> Option<usize> { self.indexes.index_of(*e) }
    fn art(&self, i: usize) -> Art<'_> {
        self.view.item(i).map_or(Art::Poster(None), grid_art)
    }
    fn label(&self, i: usize) -> card_row::TileLabel {
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
        if self.elems.is_empty() { return; }
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Grid { cols: self.target_layout.cols(), holes: NO_HOLES },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(MARGIN_X, self.target_layout.row_y(0, self.scroll_target), GRID_RIGHT - MARGIN_X, self.target_layout.card_h()),
            len: self.elems.len(),
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
            .or_else(|| self.elems.first().copied())
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
        let rows = self.elems.len().div_ceil(cols).max(1);
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
        let index = (row * cols + col).min(self.elems.len().saturating_sub(1));
        FocusKey { entry: self.entry, elem: self.elems.get(index).copied().unwrap_or(0) }
    }
}

impl<H: LibraryLike> Part<H> for GridPart {
    fn prepare(&mut self, _budget: &mut Budget, _cx: &Cx<'_, H>) {}

    fn draw(&mut self, f: &mut plx_ui::screen::DrawFrame<'_, '_, H>, _rect: Rect) {
        // Metadata still pages ahead via LibraryWork::Want. Hidden artwork waits until visible
        // (`Grid` paints only cards `paint_visible`): repeated warms recycle cold cache slots and
        // keep idle uploading. The grid also registers the stops of the cards it draws.
        let p = Self::painter(f);
        let src = self.source(f.cx);
        self.grid.draw(f, p, &src);
    }
}
