//! Uniform portrait-grid geometry and caption motion shared by the grids of the application.
//!
//! [`GridBands`] is the collapsing caption band every grid row uses (lifted from the Library's
//! All grid, which still owns its own rail-aware layout and reads the bands through
//! [`GridBand`]): only the focused row reserves `card_row::UNDER_LABEL_H` under its posters, and
//! rows below it move down by what that row grows. The free functions are the six-column grid
//! of a page without an alphabet rail (the Collection page) — row pitch, band-aware cell
//! placement, culling window and reveal arithmetic, so draw, focus placement, scrolling and
//! paging all describe the same cells. [`neighbour`], [`settled`] and [`growth_before`] are
//! column-count agnostic, and the Library's rail-aware layout uses them too.

use crate::card_row::{self, RowStyle, LABEL_BAND_COLLAPSED, UNDER_LABEL_H};
use crate::consts::{CARD_H, CARD_W, MARGIN_X, MARGIN_Y, SCR_H, SCR_W, UNDER_LABEL_AIR};
use crate::{Rect, Spring};

pub const COLS: usize = 6;
pub const GAP: f32 = (SCR_W - 2.0 * MARGIN_X - COLS as f32 * CARD_W) / (COLS as f32 - 1.0);
pub const STYLE: RowStyle = RowStyle { gap: GAP, ..RowStyle::HOME };
/// A COLLAPSED row's pitch; a row whose band is open adds its share of `card_row::BAND_OPEN`.
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
    slots: [(Option<usize>, Spring); MAX_GRID_BANDS],
}

impl GridBands {
    pub fn new() -> Self {
        Self { focus: None, slots: [(None, Spring::at(0.0)); MAX_GRID_BANDS] }
    }

    pub fn focus(&mut self, row: Option<usize>, animate: bool) {
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

    pub fn tick(&mut self, k: f32, dt: f32) {
        for (row, spring) in &mut self.slots {
            let Some(r) = *row else { continue };
            let target = f32::from(self.focus == Some(r));
            spring.step(target, k, dt);
            if target == 0.0 && spring.pos.abs() < 1.0e-5 && spring.vel.abs() < 1.0e-4 { *row = None; }
        }
    }

    pub fn geometry(&self) -> [GridBand; MAX_GRID_BANDS] {
        self.slots.map(|(row, spring)| row.map_or(GridBand::CLOSED,
            |row| GridBand { row, expansion: spring.pos }))
    }

    pub fn write(&self, c: &mut plx_machine::machine::Canon) {
        c.option(self.focus, |c, row| { c.u32(row as u32); });
        c.seq(self.slots.iter().filter(|(row, _)| row.is_some()).count());
        for (row, spring) in &self.slots {
            if let Some(row) = row { c.u32(*row as u32).f32(spring.pos).f32(spring.vel); }
        }
    }
}

/// The SETTLED bands of a grid whose focus is `row` — every reveal and scroll bound is computed
/// from where the bands are going, never from their live springs (`card_row::settled_top`'s
/// argument).
pub fn settled(row: Option<usize>) -> [GridBand; 1] {
    [row.map_or(GridBand::CLOSED, |row| GridBand { row, expansion: 1.0 })]
}

pub fn rows(len: usize) -> usize { len.div_ceil(COLS) }

/// How far every open band above row `row` pushes it down.
pub fn growth_before(row: usize, bands: &[GridBand]) -> f32 {
    bands.iter().filter(|band| band.row < row)
        .map(|band| card_row::under_band(band.expansion) - LABEL_BAND_COLLAPSED).sum()
}

/// Row `row`'s top in document space (before scroll), with every open band above it counted.
pub fn row_top(row: usize, top: f32, bands: &[GridBand]) -> f32 {
    top + row as f32 * ROW_PITCH + growth_before(row, bands)
}

pub fn cell(index: usize, top: f32, scroll: f32, bands: &[GridBand]) -> Rect {
    let col = index % COLS;
    Rect::new(MARGIN_X + col as f32 * (CARD_W + GAP), row_top(index / COLS, top, bands) - scroll,
        CARD_W, CARD_H)
}

pub fn max_scroll(len: usize, top: f32, bands: &[GridBand]) -> f32 {
    let rows = rows(len);
    (row_top(rows, top, bands) - (SCR_H - MARGIN_Y)).max(0.0)
}

/// The scroll that shows row `row` focused — its posters AND its open caption band above the
/// bottom margin — computed from the settled bands of that focus.
pub fn reveal_row(current: f32, row: usize, len: usize, top: f32) -> f32 {
    let bands = settled(Some(row));
    let row_top = row_top(row, top, &bands);
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
pub fn snap_row(current: f32, row: usize, len: usize, top: f32, edge: f32) -> f32 {
    if row == 0 || len == 0 { return 0.0; }
    let bands = settled(Some(row));
    let focused_top = row_top(row, top, &bands);
    let min = card_row::reveal(current, focused_top + CARD_H + UNDER_LABEL_H - (SCR_H - MARGIN_Y),
        focused_top - edge, f32::INFINITY);
    if min <= 0.0 { return 0.0; }
    (0..=row).map(|k| row_top(k, top, &bands) - edge).find(|&at| at >= min - 0.5)
        .unwrap_or(focused_top - edge)
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
pub fn visible(len: usize, top: f32, scroll: f32) -> std::ops::Range<usize> {
    if len == 0 { return 0..0; }
    let first = ((scroll - top - CARD_H - UNDER_LABEL_H) / ROW_PITCH).floor().max(0.0) as usize;
    let last = ((scroll + SCR_H - top) / ROW_PITCH).ceil().max(0.0) as usize + 1;
    first.saturating_mul(COLS)..last.saturating_mul(COLS).min(len)
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
        assert_eq!(card_row::band_reveal(opened), 0.0, "caption waits until its space is open");
        for _ in 0..120 { bands.tick(k, 1.0 / 60.0); }
        let (_, moving) = plx_machine::idle::scoped_motion(|| bands.tick(k, 1.0 / 60.0));
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
}
