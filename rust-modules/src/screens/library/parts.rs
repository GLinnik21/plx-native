//! The concrete master/detail pair: A–Z rail (master) and portrait or episode listing (detail).

pub(super) use super::rail::RailPart;

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::screens::registry::{LibraryIdentity, LibraryLike, LibrarySectionIdentity};
use crate::ui::card_row;
use crate::ui::consts::{MARGIN_X, SCR_H};
use crate::ui::frame::Budget;
use crate::ui::machine::{Cx, EntryId, FocusKey, GroupId};
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Hover, Part,
    Placed, Seat, Step, Stop,
};
use crate::ui::widgets::Art;
use crate::ui::{Rect, Spring};

use super::identity::KeyRegistry;
use super::layout::{
    GridBand, Layout, CONTENT_TOP, GRID_RIGHT, MAX_GRID_BANDS,
};

pub(super) const GRID_GROUP: GroupId = GroupId(0x4c49_4201);
pub(super) const RAIL_GROUP: GroupId = GroupId(0x4c49_4202);
const NO_HOLES: &[(usize, usize)] = &[];

pub(super) fn grid_art(item: &crate::pms::PmsMovie) -> Art<'_> {
    if item.kind == 3 { Art::Still(Some(item)) } else { Art::Poster(Some(item)) }
}

/// One label construction for the normal and modal-lifted focused grid card.
pub(super) fn grid_label(item: &crate::pms::PmsMovie) -> card_row::TileLabel {
    if item.kind == 3 {
        let name = if item.title.is_empty() || item.title == item.show_title {
            crate::ui::fmt::episode_address(item.season_index as i64, item.ep_index as i64)
        } else { item.title.clone() };
        // The shared still overlay already names the show and episode address on the artwork.
        // Focus reveals the episode title and release date, as it does on an episode shelf.
        return if item.aired.is_empty() && item.year <= 0 { card_row::TileLabel::title(&name) }
        else { card_row::TileLabel::titled(&name,
            &crate::ui::fmt::pretty_date(&item.aired, item.year as i64)) };
    }
    if item.kind == 2 && !item.show_title.is_empty() {
        return card_row::TileLabel::titled(&item.title, &item.show_title);
    }
    let mut label = card_row::TileLabel::title(&item.title);
    label.caption = card_row::focused_caption(item, false);
    label
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

/// Sparse caption motion: the focused row and rows still closing behind it. A catalog with
/// ten thousand rows costs exactly as much as a short one; no per-frame allocation or row walk.
struct GridBands {
    focus: Option<usize>,
    slots: [(Option<usize>, Spring); MAX_GRID_BANDS],
}

impl GridBands {
    fn new() -> Self {
        Self { focus: None, slots: [(None, Spring::at(0.0)); MAX_GRID_BANDS] }
    }

    fn focus(&mut self, row: Option<usize>, animate: bool) {
        if self.focus == row { return; }
        self.focus = row;
        if !animate {
            self.slots = [(None, Spring::at(0.0)); MAX_GRID_BANDS];
            if let Some(row) = row { self.slots[0] = (Some(row), Spring::at(1.0)); }
            return;
        }
        let Some(row) = row else { return };
        if self.slots.iter().any(|(r, _)| *r == Some(row)) { return; }
        // A stream faster than remote repeat can fill the bounded pool. Retire the smallest
        // closing band, never the focused row; normal input leaves several spare slots.
        let at = self.slots.iter().position(|(r, _)| r.is_none()).unwrap_or_else(|| {
            self.slots.iter().enumerate().min_by(|(_, a), (_, b)| a.1.pos.total_cmp(&b.1.pos))
                .map_or(0, |(at, _)| at)
        });
        self.slots[at] = (Some(row), Spring::at(0.0));
    }

    fn tick(&mut self, k: f32, dt: f32) {
        for (row, spring) in &mut self.slots {
            let Some(r) = *row else { continue };
            let target = f32::from(self.focus == Some(r));
            spring.step(target, k, dt);
            if target == 0.0 && spring.pos.abs() < 1.0e-5 && spring.vel.abs() < 1.0e-4 { *row = None; }
        }
    }

    fn geometry(&self) -> [GridBand; MAX_GRID_BANDS] {
        self.slots.map(|(row, spring)| row.map_or(GridBand::CLOSED,
            |row| GridBand { row, expansion: spring.pos }))
    }

    fn write(&self, c: &mut crate::ui::machine::Canon) {
        c.option(self.focus, |c, row| { c.u32(row as u32); });
        c.seq(self.slots.iter().filter(|(row, _)| row.is_some()).count());
        for (row, spring) in &self.slots {
            if let Some(row) = row { c.u32(*row as u32).f32(spring.pos).f32(spring.vel); }
        }
    }
}

pub(super) struct GridPart {
    entry: EntryId,
    group: GroupId,
    pub(super) elems: Vec<u32>,
    known: Vec<(u32, usize)>,
    identity: Option<(u32, crate::plex::ServerId, i64, u32)>,
    layout: Layout,
    scroll: f32,
    target_layout: Layout,
    scroll_target: f32,
    /// The focused cell's pop — the spring that carries a tile toward `RowStyle::HOME.focus_scale`,
    /// the legacy grid's `FOCUS_S` — and the cell it belongs to. It only starts from REST when a
    /// deliberate MOVE arms it ([`pop_from_rest`](Self::pop_from_rest), from the `FocusMoved` arm
    /// for `By::Dir` / `By::Pointer`); every other way a cell becomes focused — a restore, a
    /// reconcile, a command seating the cursor — is adopted at full scale by [`tick`](Self::tick),
    /// because a page coming back from a Detail push must land exactly as it was left.
    pop: (Option<usize>, Spring),
    /// The previously focused cell shrinking back to 1.0 (the legacy `PREV_S`); its index
    /// clears once it has settled, so a settled grid pays for no shrinking tile.
    shrink: (Option<usize>, Spring),
    bands: GridBands,
    snapshot: Option<crate::stores::browse::ListingSnapshot>,
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

    pub(super) fn write(&self, c: &mut crate::ui::machine::Canon) {
        // The retained snapshot is a read-publication cache, not another cursor. Its placement
        // projection and identity are traversed below; its Arc address never enters logical state.
        let Self { entry: _, group, elems, known, identity, layout, scroll, target_layout,
            scroll_target, pop, shrink, bands, snapshot: _, indexes: _, #[cfg(test)] test_ops: _ } = self;
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
        for (index, sp) in [pop, shrink] {
            c.option(*index, |c, i| { c.u32(i as u32); });
            c.f32(sp.pos).f32(sp.vel);
        }
        bands.write(c);
    }

    pub(super) fn new(entry: EntryId, group: GroupId) -> Self {
        Self {
            entry, group,
            elems: Vec::new(),
            known: Vec::new(),
            identity: None,
            layout: Layout::new(false, &[], 0, false),
            scroll: 0.0,
            target_layout: Layout::new(false, &[], 0, false),
            scroll_target: 0.0,
            pop: (None, Spring::at(1.0)),
            shrink: (None, Spring::at(1.0)),
            bands: GridBands::new(),
            snapshot: None,
            indexes: GridIndexes::default(),
            #[cfg(test)] test_ops: PublicationOps::default(),
        }
    }

    /// Arm the pop from REST: the cell at `index` starts at 1.0 and grows over the frames that
    /// follow, while whatever held the pop before is handed to the shrink spring — one tile
    /// growing as its neighbour lets go, the treatment every shelf gets from `RowMotion` and the
    /// one the poster wall had as the legacy grid's `FOCUS_S`/`PREV_S` pair before phase 8 applied
    /// the scale as a step. Called from the `FocusMoved` arm for `By::Dir` / `By::Pointer` and
    /// from nowhere else: a deliberate move is the only focus change the eye should see travel.
    pub(super) fn pop_from_rest(&mut self, index: usize) {
        self.shrink = self.pop;
        self.pop = (Some(index), Spring::at(1.0));
    }

    /// Advance the focus pop by one tick. `focused` is the focused cell's index, if focus is in
    /// the grid at all.
    ///
    /// **Order within a frame.** Inputs and the `FocusMoved` they deliver run BEFORE the screen's
    /// `Tick`, so a D-pad step has already called [`pop_from_rest`](Self::pop_from_rest) by the
    /// time this runs, and this only steps that spring on from 1.0. A cell that arrives here
    /// unannounced — a restore, a reconcile, a command that seated the cursor — is ADOPTED AT FULL
    /// SCALE with no animation, which is what makes a page returning from a Detail push draw the
    /// rect it was left at on its first frame back (`LibraryScreen::restore` jumps the scroll
    /// spring for the same reason).
    pub(super) fn tick(&mut self, focused: Option<usize>, dt: f32) {
        let style = self.layout.style();
        self.focus_row(focused.map(|index| index / self.layout.cols()), false);
        self.bands.tick(style.k_scroll, dt);
        if focused != self.pop.0 {
            self.shrink = self.pop;
            self.pop = (focused, Spring::at(if focused.is_some() { style.focus_scale } else { 1.0 }));
        }
        let k = style.k_scale;
        self.pop.1.step(if self.pop.0.is_some() { style.focus_scale } else { 1.0 }, k, dt);
        self.shrink.1.step(1.0, k, dt);
        if self.shrink.1.pos < 1.003 {
            self.shrink.0 = None;
        }
    }

    pub(super) fn focus_row(&mut self, row: Option<usize>, animate: bool) {
        self.bands.focus(row, animate);
    }

    pub(super) fn band_geometry(&self) -> [GridBand; MAX_GRID_BANDS] { self.bands.geometry() }

    pub(super) fn set_geometry(&mut self, layout: Layout, scroll: f32, target_layout: Layout, scroll_target: f32) {
        self.layout = layout;
        self.scroll = scroll;
        self.target_layout = target_layout;
        self.scroll_target = scroll_target;
    }

    pub(super) fn restore_keys(&mut self, keys: &KeyRegistry) {
        self.bands = GridBands::new();
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
            self.elems.clear();
            self.indexes.elems.clear();
            self.identity = None;
            return;
        };
        let section = LibrarySectionIdentity { sid: id.sid, key: id.section };
        let total = view.total().max(0) as usize;
        let stamp = (id.epoch, id.sid, id.section, id.query);
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

    fn publish_slot(&mut self, view: crate::stores::browse::ListingView<'_>,
        section: &LibrarySectionIdentity, query: u32, index: usize, keys: &mut KeyRegistry) -> u32 {
        #[cfg(test)] { self.test_ops.slot_visits += 1; }
        keys.register(item_identity(view, section, query, index), self.group, index)
    }

    fn replace_range(&mut self, view: crate::stores::browse::ListingView<'_>,
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

    /// The cell's drawn rect, scaled by [`tile_scale`](Self::tile_scale) — the live pop times the
    /// live press for the focused cell, the live shrink for the one that just lost focus, and rest
    /// for everybody else. This function and the unfocused branch of [`Part::draw`] MUST read that
    /// one function rather than each keep their own copy of the branch; see its doc for the defect
    /// that a second copy caused.
    pub(super) fn rect_at(&self, index: usize, focused: bool, press: f32) -> Rect {
        let row = index / self.layout.cols();
        let col = index % self.layout.cols();
        let scale = self.tile_scale(index, focused, press);
        Rect::new(self.layout.cell_x(col), self.layout.row_y(row, self.scroll),
            self.layout.card_w(), self.layout.card_h()).scaled(scale)
    }

    /// **The one scale a cell draws at — for its RECT and for its TREATMENT, always together.**
    /// Live pop×press while focused ([`treatment_scale`](Self::treatment_scale)), live shrink for
    /// the cell that just lost focus, rest for everybody else.
    ///
    /// This exists because `Part::draw`'s unfocused tile used to keep its own inline copy of this
    /// same branch for the RECT while handing [`card_row::draw_tile`]'s renderer a hardcoded `1.0`
    /// for the TREATMENT — exactly the disagreement [`treatment_scale`](Self::treatment_scale)'s
    /// own doc already warns about for the focused cell, just uncaught on the unfocused one.
    /// `draw_tile` derives the corner radius and the card-shadow ramp from its `s` argument
    /// (`ui/card_row.rs`'s `tile_radius`/the shadow `f`), so a tile mid-shrink was drawn with its
    /// radius and shadow already AT REST while its rectangle was still gliding down under the live
    /// shrink spring — the "abrupt" unfocus the owner reported in the All grid, unlike a Home
    /// shelf, whose own `draw_tile` call has always taken `RowMotion`'s live scale for both. One
    /// function, called from both the rect and the renderer, is what makes that impossible again.
    fn tile_scale(&self, index: usize, focused: bool, press: f32) -> f32 {
        if focused {
            self.treatment_scale(index, press)
        } else if self.shrink.0 == Some(index) {
            self.shrink.1.pos
        } else {
            1.0
        }
    }

    /// The focused cell's LIVE scale: the pop spring's position while the pop belongs to this
    /// cell, and FULL scale when it does not. That second case is a focus this part has not been
    /// told about yet — a restore, a reconcile, or a seat that landed after this frame's
    /// [`tick`](Self::tick) — and those land finished rather than animating, so drawing them at
    /// rest would be a one-frame collapse of the very card being returned to.
    fn pop_scale(&self, index: usize) -> f32 {
        if self.pop.0 == Some(index) { self.pop.1.pos } else { self.layout.style().focus_scale }
    }

    /// The scale the focused cell's TREATMENT is drawn at — what
    /// [`card_row::draw_focused`](crate::ui::card_row::draw_focused) is handed as its `s`, and
    /// therefore THE scale [`rect_at`](Self::rect_at) built that cell's rect from: the live pop
    /// times the live press. `draw_focused` divides by it twice (the shadow/sheen ramp and the
    /// label's anchor at the unscaled card bottom), so handing it anything else is not a
    /// refinement of the treatment but a rect and a treatment that disagree —
    /// `a_pressed_grid_tile_hands_the_card_renderer_the_scale_its_rect_was_built_from` is the
    /// account. [`tile_scale`](Self::tile_scale) is this same rule generalised past the focused
    /// cell: it calls here when `focused`, and answers the shrink/rest cases the same invariant
    /// covers for every other cell.
    pub(super) fn treatment_scale(&self, index: usize, press: f32) -> f32 {
        self.pop_scale(index) * if press > 0.0 { press } else { 1.0 }
    }

    pub(super) fn visible_window(&self) -> (usize, usize) {
        let (lo, hi) = self.layout.visible_rows(self.scroll);
        let cols = self.layout.cols();
        (lo.saturating_mul(cols), hi.saturating_mul(cols).min(self.elems.len()))
    }

    pub(super) fn record_stops<H: LibraryLike>(&self, f: &mut crate::ui::screen::DrawFrame<'_, '_, H>) {
        let (lo, hi) = self.visible_window();
        for index in lo..hi {
            let elem = self.elems[index];
            let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) else { continue };
            f.stop(f.painter, Stop { key: FocusKey { entry: self.entry, elem },
                rect: placed.rect, rest_rect: placed.rest_rect, clip: placed.clip,
                hover: Hover::Focus, activate: Activate::Press });
        }
    }

    pub(super) fn draw_focused<H: LibraryLike>(&self, f: &crate::ui::screen::DrawFrame<'_, '_, H>, focus: Option<FocusKey<u32>>) {
        let Some(index) = focus.filter(|key| key.entry == self.entry).and_then(|key| self.index_of(key.elem)) else { return };
        let Some(item) = H::listing(f.cx).item(index) else { return };
        let p = f.painter.alpha(f.page_alpha);
        let rect = self.rect_at(index, true, f.press.scale);
        let style = self.layout.style();
        let scale = self.treatment_scale(index, f.press.scale);
        if !card_row::paint_visible(p, rect, scale, true) {
            return;
        }
        let label = grid_label(item).revealed(card_row::band_reveal(
            self.layout.row_expansion(index / self.layout.cols())));
        let resume = if item.kind == 3 { None } else { item.resume_frac() };
        // ONE scale for the rect and the treatment: the shadow and sheen ramp in with the pop and
        // let go under the press, as a shelf's do (`RowMotion::scale` × `f.press.scale`)
        card_row::draw_focused(p, grid_art(item), rect, scale, &style, resume, &label, f.measure);
        if item.kind == 3 {
            crate::ui::widgets::still_overlay(p, item, rect, style.tile_radius(rect, scale), false, f.measure);
        }
    }
}

fn item_identity(
    view: crate::stores::browse::ListingView<'_>,
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

    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
        let Some(index) = self.index_of(key.elem) else { return Step::Edge };
        let cols = self.target_layout.cols();
        let row = index / cols;
        let col = index % cols;
        let next = match dir {
            Dir::Left => col.checked_sub(1).map(|c| row * cols + c),
            Dir::Right => (col + 1 < cols).then_some(index + 1),
            Dir::Up => row.checked_sub(1).map(|r| r * cols + col),
            Dir::Down => ((row + 1) * cols < self.elems.len())
                .then(|| ((row + 1) * cols + col).min(self.elems.len() - 1)),
        }
        .filter(|&i| i < self.elems.len());
        next.map_or(Step::Edge, |i| Step::Move(FocusKey { entry: key.entry, elem: self.elems[i] }))
    }

    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let index = self.index_of(*key)?;
        let focused = cx.focus.current.is_some_and(|focus| focus.entry == self.entry && focus.elem == *key);
        let (rect, rest_rect) = match at {
            At::Drawn => (self.rect_at(index, focused, cx.press.scale), self.rect_at(index, focused, 1.0)),
            At::SpringTarget => {
                let layout = self.target_layout;
                let rect = Rect::new(layout.cell_x(index % layout.cols()),
                    layout.row_y(index / layout.cols(), self.scroll_target), layout.card_w(), layout.card_h())
                    .scaled(if focused { layout.style().focus_scale } else { 1.0 });
                (rect, rect)
            }
        };
        Some(Placed {
            rect,
            rest_rect,
            clip: Rect::new(MARGIN_X - 32.0, CONTENT_TOP, GRID_RIGHT - MARGIN_X + 32.0, SCR_H - CONTENT_TOP),
            index: Some(index as u32),
        })
    }

    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.index_of(want.elem).is_some() { return want; }
        self.fallback_for(want.elem)
            .or_else(|| self.elems.first().copied())
            .map(|elem| FocusKey { entry: want.entry, elem })
            .unwrap_or(want)
    }

    fn seat(&self, _group: GroupId, from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        let index = from.index.unwrap_or(0) as usize;
        FocusKey {
            entry: self.entry,
            elem: self.elems.get(index.min(self.elems.len().saturating_sub(1))).copied().unwrap_or(0),
        }
    }
}

impl<H: LibraryLike> Part<H> for GridPart {
    fn prepare(&mut self, _budget: &mut Budget, _cx: &Cx<'_, H>) {}

    fn draw(&mut self, f: &mut crate::ui::screen::DrawFrame<'_, '_, H>, _rect: Rect) {
        let view = H::listing(f.cx);
        let focus = f.focus.current.filter(|key| key.entry == self.entry);
        let (lo, hi) = self.visible_window();
        let p = f.painter.alpha(f.page_alpha);
        let style = self.layout.style();
        for index in lo..hi {
            let Some(item) = view.item(index) else { continue };
            let selected = focus.is_some_and(|key| key.elem == self.elems[index]);
            if selected { continue; }
            let rect = self.rect_at(index, false, 1.0);
            let scale = self.tile_scale(index, false, 1.0);
            // Metadata still pages ahead via LibraryWork::Want. Hidden artwork must wait
            // until visible: repeated warms recycle cold cache slots and keep idle uploading.
            if !card_row::paint_visible(p, rect, scale, false) {
                continue;
            }
            let resume = if item.kind == 3 { None } else { item.resume_frac() };
            card_row::draw_tile(p, grid_art(item), rect, scale, &style, resume);
            if item.kind == 3 {
                crate::ui::widgets::still_overlay(p, item, rect, style.tile_radius(rect, scale), false, f.measure);
            }
        }
        self.draw_focused(f, focus);
        self.record_stops(f);
    }
}

#[cfg(test)]
mod pop_tests {
    use super::*;
    use crate::ui::card_row::RowStyle;
    use crate::ui::consts::{CARD_H, CARD_W};
    use crate::ui::machine::{EntryId, GroupId};

    #[test]
    fn all_caption_bands_open_with_shared_motion_and_stop_requesting_frames_at_rest() {
        let mut bands = GridBands::new();
        bands.focus(Some(0), false);
        bands.focus(Some(1), true);
        let k = RowStyle::HOME.k_scroll;
        let (_, moving) = crate::ui::idle::scoped_motion(|| bands.tick(k, 1.0 / 60.0));
        assert!(moving, "caption motion keeps the presenter awake");
        let geometry = bands.geometry();
        let opened = geometry.iter().find(|b| b.row == 1).unwrap().expansion;
        let closing = geometry.iter().find(|b| b.row == 0).unwrap().expansion;
        assert!(opened > 0.0 && opened < 1.0 && closing > 0.0 && closing < 1.0);
        assert!((opened + closing - 1.0).abs() < 0.0001);
        assert_eq!(card_row::band_reveal(opened), 0.0, "caption waits until its space is open");
        for _ in 0..120 { bands.tick(k, 1.0 / 60.0); }
        let (_, moving) = crate::ui::idle::scoped_motion(|| bands.tick(k, 1.0 / 60.0));
        assert!(!moving, "a settled grid lets idle suppression sleep");
        assert_eq!(bands.slots.iter().filter(|(r, _)| r.is_some()).count(), 1);
        assert!(card_row::band_reveal(bands.geometry().iter().find(|b| b.row == 1).unwrap().expansion) > 0.999);
        bands.focus(None, true);
        for _ in 0..120 { bands.tick(k, 1.0 / 60.0); }
        assert!(bands.slots.iter().all(|(r, _)| r.is_none()));
    }

    #[test]
    fn rapid_all_row_moves_keep_animation_bounded_and_preserve_the_focused_band() {
        let mut bands = GridBands::new();
        bands.focus(Some(0), false);
        for row in 1..200 {
            bands.focus(Some(row), true);
            bands.tick(RowStyle::EPISODE.k_scroll, 1.0 / 240.0);
            assert!(bands.slots.iter().any(|(r, _)| *r == Some(row)));
            assert!(bands.slots.iter().filter(|(r, _)| r.is_some()).count() <= MAX_GRID_BANDS);
        }
        for _ in 0..120 { bands.tick(RowStyle::EPISODE.k_scroll, 1.0 / 60.0); }
        assert_eq!(bands.slots.iter().filter(|(r, _)| r.is_some()).count(), 1);
        assert!(bands.geometry().iter().find(|b| b.row == 199).unwrap().expansion > 0.999);
        let before = bands.geometry();
        bands.focus(Some(199), true);
        assert_eq!(bands.geometry(), before, "moving horizontally does not close the same row");
    }

    #[test]
    fn episode_grid_geometry_and_page_window_share_four_column_rows() {
        let mut grid = GridPart::new(EntryId(7), GroupId(3));
        grid.elems = (1..=80).collect();
        let layout = Layout::new(false, &[], 20, true).with_episodes(true);
        grid.set_geometry(layout, 0.0, layout, 0.0);
        let first = grid.rect_at(0, false, 1.0);
        let next_row = grid.rect_at(4, false, 1.0);
        assert_eq!(next_row.x, first.x);
        assert!((next_row.y - first.y - layout.grid_pitch()).abs() < 0.001);
        assert_eq!((first.w, first.h), (layout.card_w(), layout.card_h()));
        let scroll = layout.row_reveal(12);
        grid.set_geometry(layout, scroll, layout, scroll);
        let (lo, hi) = grid.visible_window();
        assert!((lo..hi).contains(&48), "row 12's first episode must be in its page window");
        assert!(hi - lo < 32, "the window should request only nearby episode rows");
    }

    /// Owner report, 2026-09-09: "in the All section the poster just pops right away, not
    /// animated — nothing like that on Home". Home's shelves grow a focused tile over frames
    /// (`RowMotion`'s per-tile springs); the owned grid applied `focus_scale` as a step. Watched
    /// red against that: one frame after a focus move the tile was already at full scale.
    ///
    /// The D-pad path is `pop_from_rest` (the `FocusMoved` arm, `By::Dir`/`By::Pointer`) followed
    /// by the frame's ticks, which is the order the loop runs them in.
    #[test]
    fn a_newly_focused_grid_tile_grows_over_frames_and_the_old_one_lets_go() {
        let mut g = GridPart::new(EntryId(7), GroupId(3));
        g.elems = (1..=12).collect();
        let full = CARD_W * RowStyle::HOME.focus_scale;
        let dt = 1.0 / 60.0;
        g.pop_from_rest(3);
        g.tick(Some(3), dt);
        let first = g.rect_at(3, true, 1.0).w;
        assert!(
            first > CARD_W + 0.1 && first < full - 0.1,
            "one frame in, the tile is between rest and full scale: {first} (rest {CARD_W}, full {full})"
        );
        for _ in 0..120 { g.tick(Some(3), dt); }
        assert!((g.rect_at(3, true, 1.0).w - full).abs() < 0.5, "…and settles at full scale");
        g.pop_from_rest(4);
        g.tick(Some(4), dt);
        let old = g.rect_at(3, false, 1.0).w;
        let new = g.rect_at(4, true, 1.0).w;
        assert!(old > CARD_W + 0.1 && old < full - 0.1, "the old tile lets go rather than snapping: {old}");
        assert!(new > CARD_W + 0.1 && new < full - 0.1, "the new tile starts growing from rest: {new}");
        for _ in 0..120 { g.tick(Some(4), dt); }
        assert_eq!(g.rect_at(3, false, 1.0).w, CARD_W, "a settled neighbour costs nothing");
    }

    /// **The width-only check above cannot see this defect.** Owner report, real TV: in the All
    /// grid a tile GAINING focus lifts smoothly, but the tile LOSING it settles back down
    /// abruptly — unlike a Home shelf, which animates smoothly in both directions. The mechanism
    /// is that `Part::draw`'s unfocused branch built the outgoing tile's RECT from the live shrink
    /// spring (via `rect_at`/`tile_scale`) but handed [`card_row::draw_tile`]'s renderer a
    /// constant `1.0` for the TREATMENT — and `draw_tile` derives the corner radius and the card
    /// shadow's ramp from that `s` argument alone, so the tile's outline and shadow snapped to
    /// rest in one frame while its rectangle kept gliding down under the spring. The test above
    /// only reads `rect_at`'s own WIDTH, which never carried this bug — the rect was always built
    /// from the live spring — so it stays green with the defect fully shipped. This asserts on
    /// [`tile_scale`](GridPart::tile_scale) directly, the value now handed to BOTH call sites, and
    /// that `rect_at`'s width is exactly `CARD_W` times that same number.
    #[test]
    fn an_unfocusing_grid_tile_draws_its_shrinking_rect_and_treatment_at_the_same_scale() {
        let mut g = GridPart::new(EntryId(7), GroupId(3));
        g.elems = (1..=12).collect();
        let dt = 1.0 / 60.0;
        g.tick(Some(3), dt); // adopted at full scale: 3 is the resting focused tile
        for _ in 0..60 { g.tick(Some(3), dt); }
        g.pop_from_rest(4); // focus leaves 3 for 4: 3 now shrinks back toward rest
        g.tick(Some(4), dt);
        let scale = g.tile_scale(3, false, 1.0);
        assert!(scale > 1.0 && scale < RowStyle::HOME.focus_scale,
            "mid-shrink, the outgoing tile's own scale sits strictly between rest and full: {scale}");
        let rect = g.rect_at(3, false, 1.0);
        assert!((rect.w - CARD_W * scale).abs() < 0.001,
            "rect_at's width must be CARD_W times the exact same scale tile_scale answers: rect.w={} scale={scale}", rect.w);
        // The old call site's bug was indistinguishable from "already at rest": confirm this
        // frame's scale genuinely is not 1.0, so a renderer that received it instead of the old
        // hardcoded constant draws a visibly different (still-elevated) tile.
        assert!((1.0_f32 - scale).abs() > 0.01, "a fresh mid-shrink tile must not already read as rest");
        for _ in 0..120 { g.tick(Some(4), dt); }
        assert_eq!(g.tile_scale(3, false, 1.0), 1.0, "…and once settled, the shared scale agrees it is at rest");
    }

    /// The other half of the same design, and — unlike the test above — written AFTER it rather
    /// than watched red against the shipped bug: a cell that becomes focused without a
    /// `pop_from_rest` is a restore, a reconcile or a command seating the cursor, and it must draw
    /// at FULL scale on its first frame, before and after the tick that adopts it. The red that
    /// motivated it was real but was the bridge's, not this module's —
    /// `library_detail_return_restores_engine_card_and_viewport_after_stack_eviction` failed at
    /// 250 px against the 272.5 px it was left at, because a remounted grid re-popped from rest.
    #[test]
    fn a_cell_focused_without_a_move_is_adopted_at_full_scale() {
        let mut g = GridPart::new(EntryId(7), GroupId(3));
        g.elems = (1..=12).collect();
        let full = CARD_W * RowStyle::HOME.focus_scale;
        let dt = 1.0 / 60.0;
        assert_eq!(g.rect_at(5, true, 1.0).w, full, "the frame BEFORE the tick already draws it whole");
        g.tick(Some(5), dt);
        assert_eq!(g.rect_at(5, true, 1.0).w, full, "and the tick adopts it without a step of animation");
        for _ in 0..3 { g.tick(Some(5), dt); }
        assert_eq!(g.rect_at(5, true, 1.0).w, full, "…and it stays there");
    }

    /// Owner report, 2026-09-09 (TV session 4), beside the pop above: "the click-in animation on
    /// tiles was lost as well". The press DIP itself survived phase 8 — `rect_at` multiplies by
    /// `f.press.scale` and a simulator capture of the poster wall shows the art shrink — but the
    /// grid handed [`card_row::draw_focused`] a treatment scale with the press left OUT, and that
    /// argument is most of what the click LOOKS like:
    ///
    /// * `f = (s - 1) / ring_denom` is the focus drop-shadow and perimeter sheen. On Home a press
    ///   drives `s` from 1.09 to ~1.0006, so the tile visibly lets go of the page and presses IN;
    ///   with `s` pinned at the pop the grid's shadow stayed at full strength through the press.
    /// * `ty = … + (rect.h / s) * 0.5` anchors the label to the UNSCALED card bottom, so a press
    ///   "never moves it" — true only while `s` is the scale `rect` was built from. With the press
    ///   in the rect and not in `s`, the caption slid up and back on every click.
    ///
    /// Legacy did it right (`ui/library.rs::draw_focused_card`: `s = FOCUS_S.pos *
    /// press::scale()`, passed to BOTH), as do Home, the Library's own shelves, Search, Detail and
    /// Person. The grid was the one outlier.
    ///
    /// Watched red against the shipped grid: `treatment_scale` answered 1.09 while the rect it was
    /// drawn beside had already dipped to `1.09 * DIP`, and the derived label anchor was 11 px
    /// above the resting card bottom.
    #[test]
    fn a_pressed_grid_tile_hands_the_card_renderer_the_scale_its_rect_was_built_from() {
        let mut g = GridPart::new(EntryId(7), GroupId(3));
        g.elems = (1..=12).collect();
        let dt = 1.0 / 60.0;
        g.tick(Some(3), dt); // adopted at full scale: the resting focused tile
        // `ui::press::DIP` is private; this is a press mid-dip, which is all the draw sees.
        for press in [1.0_f32, 0.96, 0.918] {
            let rect = g.rect_at(3, true, press);
            let s = g.treatment_scale(3, press);
            assert!((rect.w - CARD_W * s).abs() < 0.001,
                "the treatment scale is the one the rect was built from: rect.w={} s={s}", rect.w);
            // the label block's anchor (`card_row::draw_focused`): the UNSCALED card bottom
            assert!((rect.h / s - CARD_H).abs() < 0.01,
                "a press never moves the label: rect.h/s={} (rest {CARD_H})", rect.h / s);
        }
        let ring = |press: f32| (g.treatment_scale(3, press) - 1.0) / (RowStyle::HOME.focus_scale - 1.0);
        assert!(ring(1.0) > 0.99, "a resting focused tile wears the whole shadow and sheen");
        assert!(ring(0.918) < 0.1,
            "…and lets go of them under a full press, as Home's does: {}", ring(0.918));
    }
}
