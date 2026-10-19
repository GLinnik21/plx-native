//! Uniform portrait-grid geometry and caption motion shared by the grids of the application.
//!
//! [`GridBands`] is the collapsing caption band every grid row uses (lifted from the Library's
//! All grid, which still owns its own rail-aware layout and reads the bands through
//! [`GridBand`]): only the focused row reserves `card_row::UNDER_LABEL_H` under its posters, and
//! rows below it move down by what that row grows. The default-geometry free functions (test-only now that
//! `cards::Grid` computes its own) are the six-column grid
//! of a page without an alphabet rail (the Collection page) — row pitch, band-aware cell
//! placement, culling window and reveal arithmetic, so draw, focus placement, scrolling and
//! paging all describe the same cells. [`neighbour`], [`settled`] and [`growth_before`] are
//! column-count agnostic, and the Library's rail-aware layout uses them too.

use crate::card_row::{self, RowStyle, LABEL_BAND_COLLAPSED};
#[cfg(test)]
use crate::card_row::UNDER_LABEL_H;
#[cfg(test)]
use crate::consts::CARD_H;
use crate::consts::{CARD_W, MARGIN_X, MARGIN_Y, SCR_H, SCR_W, UNDER_LABEL_AIR};
use crate::{Rect, Spring};

pub const COLS: usize = 6;
pub const GAP: f32 = (SCR_W - 2.0 * MARGIN_X - COLS as f32 * CARD_W) / (COLS as f32 - 1.0);
pub const STYLE: RowStyle = RowStyle { gap: GAP, ..RowStyle::HOME };
/// A COLLAPSED row's pitch; a row whose band is open adds its share of `card_row::BAND_OPEN`.
#[cfg(test)]
pub const ROW_PITCH: f32 = CARD_H + LABEL_BAND_COLLAPSED + UNDER_LABEL_AIR;

/// Only the focused and closing bands are represented, independent of catalog size.
/// Sixteen spans more row moves than a complete spring at normal remote repeat speed.
pub const MAX_GRID_BANDS: usize = 16;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridBand {
    pub row: usize,
    pub expansion: f32,
}
impl GridBand {
    pub const CLOSED: Self = Self { row: usize::MAX, expansion: 0.0 };
}

/// Sparse caption motion: the focused row and rows still closing behind it. A catalog with
/// ten thousand rows costs exactly as much as a short one; no per-frame allocation or row walk.
pub struct GridBands {
    focus: Option<usize>,
    slots: [Slot; MAX_GRID_BANDS],
    /// What the focused caption band shows (CONTRACT FREEZE: P3 sizes the band from it). A pref
    /// input, not motion: it is not in [`write`](Self::write).
    caption_policy: crate::caption_policy::CaptionPolicy,
}

/// One band: the row it opens under, the card of that row whose caption it carries (known once the
/// row has held focus on a tick, so a band that closes behind a focus move still knows whose
/// caption fades with it) and the spring that opens and closes it.
#[derive(Clone, Copy)]
struct Slot {
    row: Option<usize>,
    card: Option<usize>,
    spring: Spring,
}

impl Slot {
    const EMPTY: Self = Self { row: None, card: None, spring: Spring::at(0.0) };
}

impl GridBands {
    pub fn new() -> Self {
        Self {
            focus: None,
            slots: [Slot::EMPTY; MAX_GRID_BANDS],
            caption_policy: crate::caption_policy::CaptionPolicy::DEFAULT,
        }
    }

    pub fn set_caption_policy(&mut self, policy: crate::caption_policy::CaptionPolicy) {
        self.caption_policy = policy;
    }

    pub fn caption_policy(&self) -> crate::caption_policy::CaptionPolicy {
        self.caption_policy
    }

    /// The focused band's height under this grid's caption policy (0 / 80 / 112), what
    /// [`growth_before_for`] and the other `_for` geometry take.
    pub fn band_h(&self) -> f32 {
        crate::caption_policy::band_h(&self.caption_policy)
    }

    /// The row whose band is open (or opening).
    pub fn focused(&self) -> Option<usize> {
        self.focus
    }

    pub fn focus(&mut self, row: Option<usize>, animate: bool) {
        if self.focus == row { return; }
        self.focus = row;
        if !animate {
            self.slots = [Slot::EMPTY; MAX_GRID_BANDS];
            if let Some(row) = row { self.slots[0] = Slot { row: Some(row), card: None, spring: Spring::at(1.0) }; }
            return;
        }
        let Some(row) = row else { return };
        if self.slots.iter().any(|s| s.row == Some(row)) { return; }
        // A stream faster than remote repeat can fill the bounded pool. Retire the smallest
        // closing band, never the focused row; normal input leaves several spare slots.
        let at = self.slots.iter().position(|s| s.row.is_none()).unwrap_or_else(|| {
            self.slots.iter().enumerate().min_by(|(_, a), (_, b)| a.spring.pos.total_cmp(&b.spring.pos))
                .map_or(0, |(at, _)| at)
        });
        self.slots[at] = Slot { row: Some(row), card: None, spring: Spring::at(0.0) };
    }

    /// Note which card of the focused row `row` the focus is on, so the band, once it closes behind
    /// a move, still knows whose caption it carries ([`closing`](Self::closing)). A card outside the
    /// focused row is not noted.
    pub fn note_card(&mut self, row: usize, card: usize) {
        if self.focus != Some(row) { return; }
        if let Some(slot) = self.slots.iter_mut().find(|s| s.row == Some(row)) { slot.card = Some(card); }
    }

    /// The bands closing behind a focus move, as `(row, card, expansion)`: each is the caption of
    /// `card` (the one the row held focus on) at its band's own progress. The focused row's band is
    /// not among them; a band with no card noted (a row that never held a tick) or fully shut is not.
    pub fn closing(&self) -> impl Iterator<Item = (usize, usize, f32)> + '_ {
        self.slots.iter().filter_map(|s| {
            let (row, card) = (s.row?, s.card?);
            (self.focus != Some(row) && s.spring.pos > 0.0).then_some((row, card, s.spring.pos))
        })
    }

    pub fn tick(&mut self, k: f32, dt: f32) {
        for slot in &mut self.slots {
            let Some(r) = slot.row else { continue };
            let target = f32::from(self.focus == Some(r));
            slot.spring.step(target, k, dt);
            if target == 0.0 && slot.spring.pos.abs() < 1.0e-5 && slot.spring.vel.abs() < 1.0e-4 { *slot = Slot::EMPTY; }
        }
    }

    pub fn geometry(&self) -> [GridBand; MAX_GRID_BANDS] {
        self.slots.map(|s| s.row.map_or(GridBand::CLOSED,
            |row| GridBand { row, expansion: s.spring.pos }))
    }

    pub fn write(&self, c: &mut plx_machine::machine::Canon) {
        c.option(self.focus, |c, row| { c.u32(row as u32); });
        c.seq(self.slots.iter().filter(|s| s.row.is_some()).count());
        for s in &self.slots {
            if let Some(row) = s.row {
                c.u32(row as u32).f32(s.spring.pos).f32(s.spring.vel);
                c.option(s.card, |c, card| { c.u32(card as u32); });
            }
        }
    }
}

/// **The focus pop of a poster grid** — the spring that carries the focused cell toward
/// `RowStyle::focus_scale` and the one that lets the cell that just lost focus back down, the
/// grid's counterpart of a shelf's `RowMotion` per-tile springs. The Library's All grid and the
/// Collection page both read their cells' scale from here, so neither draws `focus_scale` as a
/// step.
///
/// It only starts from REST when a deliberate MOVE arms it ([`arm`](Self::arm), from the
/// `FocusMoved` arm for `By::Dir` / `By::Pointer`); every other way a cell becomes focused — a
/// restore, a reconcile, a command seating the cursor — is adopted at full scale by
/// [`tick`](Self::tick), because a page coming back from a Detail push must land exactly as it
/// was left. Inputs and the `FocusMoved` they deliver run BEFORE the screen's `Tick`, so a D-pad
/// step has already armed the pop by the time `tick` steps it on from 1.0.
///
/// A hold menu changes nothing here: the menu is its own input scope, the page keeps its focus,
/// and the settled pop is what the opener redraw reads ([`scale`](Self::scale)).
pub struct GridPop {
    pop: (Option<usize>, Spring),
    /// The previously focused cell shrinking back to 1.0; its index clears once it has settled,
    /// so a settled grid pays for no shrinking tile.
    shrink: (Option<usize>, Spring),
}

impl GridPop {
    pub const fn new() -> Self {
        Self { pop: (None, Spring::at(1.0)), shrink: (None, Spring::at(1.0)) }
    }

    /// Arm the pop from REST: cell `index` starts at 1.0 and grows over the frames that follow,
    /// while whatever held the pop before is handed to the shrink spring.
    pub fn arm(&mut self, index: usize) {
        self.shrink = self.pop;
        self.pop = (Some(index), Spring::at(1.0));
    }

    /// The cell whose let-go is still running, if one is.
    pub(crate) fn shrinking(&self) -> Option<usize> {
        self.shrink.0
    }

    /// The shrinking tile's element is now at cell `to` (`None`: gone), so its let-go follows it.
    pub(crate) fn move_shrink(&mut self, to: Option<usize>) {
        self.shrink.0 = to;
    }

    /// The cell the pop currently belongs to.
    pub fn cell(&self) -> Option<usize> {
        self.pop.0
    }

    /// The focused element moved to cell `to` because the CONTENT changed (a landing): the pop
    /// spring, velocity and all, goes with it, and a let-go already running on `to` is dropped
    /// (that cell now holds the element that is popped).
    pub fn relocate(&mut self, to: usize) {
        if self.pop.0.is_some() {
            self.pop.0 = Some(to);
        }
        if self.shrink.0 == Some(to) {
            self.shrink.0 = None;
        }
    }

    /// The focused card at cell `to` took over a cell whose card left lifted (a landing's ghost
    /// carries that lift down while it fades): its pop starts from REST and grows with the
    /// landing, so the two never stand lifted side by side. Nothing lets go — the card that held
    /// the pop is the ghost, which eases itself.
    pub fn rise(&mut self, to: usize) {
        self.pop = (Some(to), Spring::at(1.0));
        if self.shrink.0 == Some(to) {
            self.shrink = (None, Spring::at(1.0));
        }
    }

    /// The focused card left as a ghost and focus has not been reconciled yet: the pop belongs to
    /// no card and nothing lets go (the card now in that cell was never lifted; the ghost eases
    /// itself).
    pub fn vacate(&mut self) {
        if let Some(c) = self.pop.0 {
            if self.shrink.0 == Some(c) {
                self.shrink = (None, Spring::at(1.0));
            }
        }
        self.pop = (None, Spring::at(1.0));
    }

    /// Cell `to` is focused with no deliberate move (a restore, a reconcile): FULL scale at once
    /// and nothing lets go, the cell it left is simply at rest.
    pub fn adopt(&mut self, to: usize, style: &RowStyle) {
        self.shrink = (None, Spring::at(1.0));
        self.pop = (Some(to), Spring::at(style.focus_scale));
    }

    /// Advance the pop one tick; `focused` is the focused cell's index, if focus is in the grid.
    pub fn tick(&mut self, focused: Option<usize>, style: &RowStyle, dt: f32) {
        if focused != self.pop.0 {
            self.shrink = self.pop;
            self.pop = (focused, Spring::at(if focused.is_some() { style.focus_scale } else { 1.0 }));
        }
        self.pop.1.step(if self.pop.0.is_some() { style.focus_scale } else { 1.0 }, style.k_scale, dt);
        self.shrink.1.step(1.0, style.k_scale, dt);
        if self.shrink.1.pos < 1.003 { self.shrink.0 = None; }
    }

    /// The focused cell's LIVE scale: the pop spring's position while the pop belongs to this
    /// cell, and FULL scale when it does not — a focus this grid has not been told about yet (a
    /// restore, a reconcile, a seat that landed after this frame's tick) lands finished rather
    /// than animating, so drawing it at rest would be a one-frame collapse.
    pub fn pop_scale(&self, index: usize, style: &RowStyle) -> f32 {
        if self.pop.0 == Some(index) { self.pop.1.pos } else { style.focus_scale }
    }

    /// **The one scale a cell draws at — for its RECT and for its TREATMENT, always together**
    /// (`card_row::draw_tile`/`draw_focused` derive the radius and shadow ramp from it): the live
    /// pop while `focused`, the live shrink for the cell that just lost focus, rest for every
    /// other. The press is the caller's to multiply in.
    pub fn scale(&self, index: usize, focused: bool, style: &RowStyle) -> f32 {
        if focused { self.pop_scale(index, style) }
        else if self.shrink.0 == Some(index) { self.shrink.1.pos }
        else { 1.0 }
    }

    /// Canonical animation state: a spring mid-flight decides the next frames even when two grids
    /// draw the same rects.
    pub fn write(&self, c: &mut plx_machine::machine::Canon) {
        for (index, sp) in [&self.pop, &self.shrink] {
            c.option(*index, |c, i| { c.u32(i as u32); });
            c.f32(sp.pos).f32(sp.vel);
        }
    }
}

impl Default for GridPop {
    fn default() -> Self { Self::new() }
}

/// The SETTLED bands of a grid whose focus is `row` — every reveal and scroll bound is computed
/// from where the bands are going, never from their live springs (`card_row::settled_top`'s
/// argument).
pub fn settled(row: Option<usize>) -> [GridBand; 1] {
    [row.map_or(GridBand::CLOSED, |row| GridBand { row, expansion: 1.0 })]
}

/// A grid's column geometry: the free functions below are this with [`Geom::DEFAULT`] (the six
/// columns of a page without an alphabet rail); a page with another column count, card size or
/// left edge (the Library's rail-aware grid) passes its own to the `_in` variants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geom {
    pub cols: usize,
    /// The x of column 0's card.
    pub left: f32,
    pub w: f32,
    pub h: f32,
    pub gap: f32,
}

impl Geom {
    #[cfg(test)]
    pub const DEFAULT: Self = Self { cols: COLS, left: MARGIN_X, w: CARD_W, h: CARD_H, gap: GAP };

    pub const fn of(cols: usize, left: f32, style: &RowStyle) -> Self {
        Self { cols, left, w: style.w, h: style.h, gap: style.gap }
    }

    /// A COLLAPSED row's pitch.
    pub fn pitch(&self) -> f32 { self.h + LABEL_BAND_COLLAPSED + UNDER_LABEL_AIR }
}

#[cfg(test)]
pub fn rows(len: usize) -> usize { len.div_ceil(COLS) }

/// How far every open band above row `row` pushes it down.
pub fn growth_before(row: usize, bands: &[GridBand]) -> f32 {
    growth_before_for(card_row::LEGACY_BAND_H, row, bands)
}

/// [`growth_before`] for bands of `band_h` ([`crate::caption_policy::band_h`]): 40 px per open
/// one-line band, 72 px per two-line one, nothing when the policy shows nothing.
pub fn growth_before_for(band_h: f32, row: usize, bands: &[GridBand]) -> f32 {
    bands.iter().filter(|band| band.row < row)
        .map(|band| card_row::under_band_for(band_h, band.expansion) - LABEL_BAND_COLLAPSED).sum()
}

/// Row `row`'s top in document space (before scroll), with every open band above it counted.
#[cfg(test)]
pub fn row_top(row: usize, top: f32, bands: &[GridBand]) -> f32 { row_top_in(&Geom::DEFAULT, row, top, bands) }

pub fn row_top_in(g: &Geom, row: usize, top: f32, bands: &[GridBand]) -> f32 {
    row_top_in_for(card_row::LEGACY_BAND_H, g, row, top, bands)
}

/// [`row_top_in`] for bands of `band_h`.
pub fn row_top_in_for(band_h: f32, g: &Geom, row: usize, top: f32, bands: &[GridBand]) -> f32 {
    top + row as f32 * g.pitch() + growth_before_for(band_h, row, bands)
}

#[cfg(test)]
pub fn cell(index: usize, top: f32, scroll: f32, bands: &[GridBand]) -> Rect {
    cell_in(&Geom::DEFAULT, index, top, scroll, bands)
}

pub fn cell_in(g: &Geom, index: usize, top: f32, scroll: f32, bands: &[GridBand]) -> Rect {
    cell_in_for(card_row::LEGACY_BAND_H, g, index, top, scroll, bands)
}

/// [`cell_in`] for bands of `band_h`.
pub fn cell_in_for(band_h: f32, g: &Geom, index: usize, top: f32, scroll: f32, bands: &[GridBand]) -> Rect {
    let col = index % g.cols;
    Rect::new(g.left + col as f32 * (g.w + g.gap),
        row_top_in_for(band_h, g, index / g.cols, top, bands) - scroll, g.w, g.h)
}

#[cfg(test)]
pub fn max_scroll(len: usize, top: f32, bands: &[GridBand]) -> f32 {
    max_scroll_in(&Geom::DEFAULT, len, top, bands)
}

#[cfg(test)]
pub fn max_scroll_in(g: &Geom, len: usize, top: f32, bands: &[GridBand]) -> f32 {
    (row_top_in(g, len.div_ceil(g.cols), top, bands) - (SCR_H - MARGIN_Y)).max(0.0)
}

/// The scroll that shows row `row` focused — its posters AND its open caption band above the
/// bottom margin — computed from the settled bands of that focus.
#[cfg(test)]
pub fn reveal_row(current: f32, row: usize, len: usize, top: f32) -> f32 {
    let g = Geom::DEFAULT;
    let bands = settled(Some(row));
    let row_top = row_top_in(&g, row, top, &bands);
    card_row::reveal(current,
        row_top + CARD_H + UNDER_LABEL_H - (SCR_H - MARGIN_Y),
        row_top - MARGIN_Y,
        max_scroll(len, top, &bands))
}

/// The scroll that shows row `row` focused on a page whose rows SNAP to a content edge (the
/// Collection page, `Collections.dc.html` C2): the minimal reveal of the row's posters and open
/// caption band, rounded UP to the scroll that puts some row's top exactly at `edge`, so the page
/// never rests with a row cut by the edge. Row 0 reveals the document's head (scroll 0), as the
/// Library's `row_reveal(0)` does; no rounding passes `row` itself, so the focused row always shows.
#[cfg(test)]
pub fn snap_row(current: f32, row: usize, len: usize, top: f32, edge: f32) -> f32 {
    snap_row_in(&Geom::DEFAULT, current, row, len, top, edge)
}

pub fn snap_row_in(g: &Geom, current: f32, row: usize, len: usize, top: f32, edge: f32) -> f32 {
    snap_row_in_for(card_row::LEGACY_BAND_H, g, current, row, len, top, edge)
}

/// [`snap_row_in`] for bands of `band_h`: the focused row shows whole with its open band.
pub fn snap_row_in_for(band_h: f32, g: &Geom, current: f32, row: usize, len: usize, top: f32, edge: f32) -> f32 {
    if row == 0 || len == 0 { return 0.0; }
    let bands = settled(Some(row));
    let focused_top = row_top_in_for(band_h, g, row, top, &bands);
    let min = card_row::reveal(current, focused_top + g.h + shown_band(band_h) - (SCR_H - MARGIN_Y),
        focused_top - edge, f32::INFINITY);
    if min <= 0.0 { return 0.0; }
    (0..=row).map(|k| row_top_in_for(band_h, g, k, top, &bands) - edge).find(|&at| at >= min - 0.5)
        .unwrap_or(focused_top - edge)
}

/// **The rest rule: a focused row rests where the first row does.** The scroll that puts row `row`'s
/// poster top at the first row's resting y: `row` pitches down, plus `growth_before`, the growth of
/// every open band above it (none for the row the focus itself opened). Row 0 rests at the head,
/// scroll 0. The document's end holds it at `max`: the last rows of a list cannot scroll past the
/// end, so they rest lower. A row never rests above the line.
///
/// The Library's `Layout::row_reveal` (its grid, and the Collections page that is the same layout)
/// and a [`Grid`](crate::cards::Grid) built with `GridSpec::resting` (the Watchlist) both land here,
/// so the line (244 on those pages) is one rule, not two.
pub fn rest_scroll(row: usize, pitch: f32, growth_before: f32, max: f32) -> f32 {
    if row == 0 { return 0.0; }
    (row as f32 * pitch + growth_before).min(max).max(0.0)
}

/// The label block a focused row shows under its posters for a band of `band_h`: the band less the
/// air [`UNDER_LABEL_AIR`] below it ([`UNDER_LABEL_H`] for the legacy band), never negative.
fn shown_band(band_h: f32) -> f32 {
    (card_row::open_gap(band_h) - UNDER_LABEL_AIR).max(0.0)
}

/// The D-pad neighbour of card `index` in a `len`-card grid of `cols` columns. Down from above a
/// short last row lands on its last card rather than stopping where no card sits directly below.
pub fn neighbour(index: usize, len: usize, cols: usize, dir: crate::screen::Dir) -> Option<usize> {
    use crate::screen::Dir;
    let (row, col) = (index / cols, index % cols);
    match dir {
        Dir::Left => col.checked_sub(1).map(|c| row * cols + c),
        Dir::Right => (col + 1 < cols).then_some(index + 1),
        Dir::Up => row.checked_sub(1).map(|r| r * cols + col),
        Dir::Down => ((row + 1) * cols < len).then(|| ((row + 1) * cols + col).min(len - 1)),
    }
    .filter(|&i| i < len)
}

/// The members that can touch the screen at `scroll` — one row wider on each side than the
/// collapsed pitch alone says, so an open band shifting rows down never culls a visible one.
#[cfg(test)]
pub fn visible(len: usize, top: f32, scroll: f32) -> std::ops::Range<usize> {
    visible_in(&Geom::DEFAULT, len, top, scroll)
}

pub fn visible_in(g: &Geom, len: usize, top: f32, scroll: f32) -> std::ops::Range<usize> {
    visible_in_for(card_row::LEGACY_BAND_H, g, len, top, scroll)
}

/// [`visible_in`] for bands of `band_h`.
pub fn visible_in_for(band_h: f32, g: &Geom, len: usize, top: f32, scroll: f32) -> std::ops::Range<usize> {
    if len == 0 { return 0..0; }
    let pitch = g.pitch();
    let first = ((scroll - top - g.h - shown_band(band_h)) / pitch).floor().max(0.0) as usize;
    let last = ((scroll + SCR_H - top) / pitch).ceil().max(0.0) as usize + 1;
    first.saturating_mul(g.cols)..last.saturating_mul(g.cols).min(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_portraits_keep_existing_card_size_and_share_one_pitch() {
        let a = cell(0, 400.0, 0.0, &[]);
        let b = cell(5, 400.0, 0.0, &[]);
        let next = cell(6, 400.0, 0.0, &[]);
        assert_eq!((a.w, a.h), (CARD_W, CARD_H));
        assert!(b.x + b.w <= SCR_W - MARGIN_X + 0.01);
        assert_eq!(next.x, a.x);
        assert!((next.y - a.y - ROW_PITCH).abs() < 0.01);
    }

    /// C2: a row focused below the first scrolls the page so a row's top rests exactly on the
    /// content edge — the row above the focused one, when both fit — and never cuts a row; row 0
    /// shows the head again.
    #[test]
    fn a_snapped_page_rests_a_row_on_the_content_edge() {
        let (top, edge, len) = (520.0, 96.0, 18);
        let at = snap_row(0.0, 1, len, top, edge);
        assert_eq!(cell(0, top, at, &settled(Some(1))).y, edge, "row 0 rests on the edge");
        let focused = cell(6, top, at, &settled(Some(1)));
        assert!(focused.y + CARD_H + UNDER_LABEL_H <= SCR_H - MARGIN_Y, "and the focused row shows whole");
        let at2 = snap_row(at, 2, len, top, edge);
        assert_eq!(cell(6, top, at2, &settled(Some(2))).y, edge, "down again: the next row takes the edge");
        assert_eq!(snap_row(at2, 1, len, top, edge), at2, "up to a row already on screen: no move");
        assert_eq!(snap_row(at2, 0, len, top, edge), 0.0, "row 0 reveals the head");
    }

    /// The focused row's caption band opens and pushes only the rows below it; a revealed
    /// row's posters AND caption clear the bottom margin, the last row's included.
    #[test]
    fn an_open_band_moves_only_the_rows_below_and_the_revealed_caption_fits() {
        let bands = settled(Some(1));
        assert_eq!(cell(6, 400.0, 0.0, &bands).y, cell(6, 400.0, 0.0, &[]).y, "the focused row stays");
        assert!((cell(12, 400.0, 0.0, &bands).y - cell(12, 400.0, 0.0, &[]).y - card_row::BAND_OPEN).abs() < 0.01);
        let len = 40;
        for row in [0, 3, rows(len) - 1] {
            let scroll = reveal_row(0.0, row, len, 400.0);
            let r = cell(row * COLS, 400.0, scroll, &settled(Some(row)));
            assert!(r.y + CARD_H + UNDER_LABEL_H <= SCR_H - MARGIN_Y + 0.01, "row {row} caption clipped");
            assert!(r.y >= MARGIN_Y - 0.01, "row {row} pushed off the top");
        }
    }

    #[test]
    fn all_caption_bands_open_with_shared_motion_and_stop_requesting_frames_at_rest() {
        let mut bands = GridBands::new();
        bands.focus(Some(0), false);
        bands.focus(Some(1), true);
        let k = RowStyle::HOME.k_scroll;
        let (_, moving) = plx_machine::idle::scoped_motion(|| bands.tick(k, 1.0 / 60.0));
        assert!(moving, "caption motion keeps the presenter awake");
        let geometry = bands.geometry();
        let opened = geometry.iter().find(|b| b.row == 1).unwrap().expansion;
        let closing = geometry.iter().find(|b| b.row == 0).unwrap().expansion;
        assert!(opened > 0.0 && opened < 1.0 && closing > 0.0 && closing < 1.0);
        assert!((opened + closing - 1.0).abs() < 0.0001);
        assert!(
            (card_row::band_fade(opened) + card_row::band_fade(closing) - 1.0).abs() < 0.0001,
            "the two captions of a move are one hand-over, out = 1 - in"
        );
        for _ in 0..120 { bands.tick(k, 1.0 / 60.0); }
        let (_, moving) = plx_machine::idle::scoped_motion(|| bands.tick(k, 1.0 / 60.0));
        assert!(!moving, "a settled grid lets idle suppression sleep");
        assert_eq!(bands.slots.iter().filter(|s| s.row.is_some()).count(), 1);
        assert!(card_row::band_fade(bands.geometry().iter().find(|b| b.row == 1).unwrap().expansion) > 0.999);
        bands.focus(None, true);
        for _ in 0..120 { bands.tick(k, 1.0 / 60.0); }
        assert!(bands.slots.iter().all(|s| s.row.is_none()));
    }

    /// The pop is armed from rest by a deliberate move, adopted whole when focus arrives
    /// unannounced, lets the old cell back down and sleeps once settled.
    #[test]
    fn the_focus_pop_grows_from_rest_adopts_a_seat_whole_and_settles() {
        let style = STYLE;
        let dt = 1.0 / 60.0;
        let mut pop = GridPop::new();
        assert_eq!(pop.scale(3, true, &style), style.focus_scale, "an unannounced focus draws whole");
        pop.tick(Some(3), &style, dt);
        assert_eq!(pop.scale(3, true, &style), style.focus_scale, "…and the tick adopts it unanimated");
        pop.arm(4);
        pop.tick(Some(4), &style, dt);
        let (new, old) = (pop.scale(4, true, &style), pop.scale(3, false, &style));
        assert!(new > 1.0 && new < style.focus_scale, "the new cell grows from rest: {new}");
        assert!(old > 1.0 && old < style.focus_scale, "the old cell lets go: {old}");
        for _ in 0..120 { pop.tick(Some(4), &style, dt); }
        assert!((pop.scale(4, true, &style) - style.focus_scale).abs() < 0.001);
        assert_eq!(pop.scale(3, false, &style), 1.0);
        let (_, moving) = plx_machine::idle::scoped_motion(|| pop.tick(Some(4), &style, dt));
        assert!(!moving, "a settled grid lets idle suppression sleep");
    }

    #[test]
    fn rapid_all_row_moves_keep_animation_bounded_and_preserve_the_focused_band() {
        let mut bands = GridBands::new();
        bands.focus(Some(0), false);
        for row in 1..200 {
            bands.focus(Some(row), true);
            bands.tick(RowStyle::EPISODE.k_scroll, 1.0 / 240.0);
            assert!(bands.slots.iter().any(|s| s.row == Some(row)));
            assert!(bands.slots.iter().filter(|s| s.row.is_some()).count() <= MAX_GRID_BANDS);
        }
        for _ in 0..120 { bands.tick(RowStyle::EPISODE.k_scroll, 1.0 / 60.0); }
        assert_eq!(bands.slots.iter().filter(|s| s.row.is_some()).count(), 1);
        assert!(bands.geometry().iter().find(|b| b.row == 199).unwrap().expansion > 0.999);
        let before = bands.geometry();
        bands.focus(Some(199), true);
        assert_eq!(bands.geometry(), before, "moving horizontally does not close the same row");
    }
}
