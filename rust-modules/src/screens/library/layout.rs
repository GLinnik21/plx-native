//! Pure geometry for the owned Library's single vertical document.

use crate::ui::consts::{CARD_H, CARD_W, MARGIN_X, MARGIN_Y, SCR_H, SCR_W};
use crate::ui::theme;

pub(super) const COLS: usize = 6;
pub(super) const MAX_SHELVES: usize = 12;
pub(super) const MAX_LETTERS: usize = 64;
pub(super) const CONTENT_TOP: f32 = crate::ui::consts::GRID_TOP_Y;
pub(super) const LIBRARY_ROW_H: f32 = 52.0 + crate::ui::consts::CARD_DY + crate::ui::consts::TITLE_DY;
pub(super) const GRID_HEAD_H: f32 = crate::ui::consts::TITLE_DY
    + crate::ui::consts::CARD_DY
    + 52.0
    + crate::ui::consts::CARD_DY;
pub(super) const GRID_PITCH: f32 = CARD_H
    + crate::ui::card_row::UNDER_LABEL_H
    + crate::ui::consts::UNDER_LABEL_AIR;
pub(super) const RAIL_TRACK_W: f32 = 44.0;
pub(super) const RAIL_PITCH: f32 = 34.0;
pub(super) const RAIL_BAND: f32 = RAIL_TRACK_W + theme::space::XS;
pub(super) const GRID_RIGHT: f32 = SCR_W - MARGIN_X - RAIL_BAND;
pub(super) const GRID_GAP: f32 =
    (GRID_RIGHT - MARGIN_X - COLS as f32 * CARD_W) / (COLS as f32 - 1.0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Block {
    LibraryRow,
    Shelf(usize),
    Toolbar,
    Grid(usize),
    Status,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Layout {
    pub libraries: bool,
    pub shelves: usize,
    pub rows: usize,
    pub grid_head: bool,
    pub status: bool,
    pitches: [f32; MAX_SHELVES],
}

impl Layout {
    pub(super) fn new(libraries: bool, shelf_pitches: &[f32], rows: usize, grid_head: bool) -> Self {
        let mut pitches = [crate::ui::consts::ROW_PITCH; MAX_SHELVES];
        for (to, from) in pitches.iter_mut().zip(shelf_pitches.iter().take(MAX_SHELVES)) {
            *to = *from;
        }
        Self {
            libraries,
            shelves: shelf_pitches.len().min(MAX_SHELVES),
            rows,
            grid_head,
            status: false,
            pitches,
        }
    }

    pub(super) fn failed(libraries: bool, shelf_pitches: &[f32]) -> Self {
        Self { status: true, ..Self::new(libraries, shelf_pitches, 0, false) }
    }

    pub(super) fn library_h(self) -> f32 {
        if self.libraries { LIBRARY_ROW_H } else { 0.0 }
    }

    pub(super) fn shelf_pitch(self, index: usize) -> f32 {
        self.pitches.get(index).copied().unwrap_or(crate::ui::consts::ROW_PITCH)
    }

    pub(super) fn shelf_origin(self, index: usize) -> f32 {
        self.library_h() + self.pitches[..index.min(self.shelves)].iter().sum::<f32>()
    }

    pub(super) fn grid_block_top(self) -> f32 {
        self.library_h() + self.pitches[..self.shelves].iter().sum::<f32>()
    }

    pub(super) fn grid_top(self) -> f32 {
        self.grid_block_top() + if self.grid_head { GRID_HEAD_H } else { 0.0 }
    }

    pub(super) fn row_y(self, row: usize, scroll: f32) -> f32 {
        CONTENT_TOP + self.grid_top() + row as f32 * GRID_PITCH - scroll
    }

    pub(super) fn shelf_y(self, shelf: usize, scroll: f32) -> f32 {
        CONTENT_TOP + self.shelf_origin(shelf) - scroll
    }

    pub(super) fn doc_to_grid(self, scroll: f32) -> f32 { scroll - self.grid_top() }

    pub(super) fn doc_h(self) -> f32 { self.grid_top() + self.rows as f32 * GRID_PITCH }

    pub(super) fn max_scroll(self) -> f32 {
        (self.doc_h() - (SCR_H - CONTENT_TOP) + MARGIN_Y).max(0.0)
    }

    pub(super) fn row_reveal(self, row: usize) -> f32 {
        let top = if row == 0 { self.grid_block_top() } else { self.grid_top() + row as f32 * GRID_PITCH };
        top.clamp(0.0, self.max_scroll())
    }

    pub(super) fn shelf_reveal(self, shelf: usize) -> f32 {
        (self.shelf_origin(shelf) - crate::ui::consts::TITLE_DY).clamp(0.0, self.max_scroll())
    }

    pub(super) fn first(self) -> Option<Block> {
        if self.libraries { Some(Block::LibraryRow) }
        else if self.shelves > 0 { Some(Block::Shelf(0)) }
        else if self.status { Some(Block::Status) }
        else if self.grid_head { Some(Block::Toolbar) }
        else { None }
    }

    pub(super) fn seat_for_scroll(self, scroll: f32, saved_row: usize) -> Option<Block> {
        let first = self.first()?;
        if scroll <= 0.5 { return Some(first); }
        let mut best = (f32::INFINITY, first);
        for shelf in 0..self.shelves {
            let d = (self.shelf_reveal(shelf) - scroll).abs();
            if d < best.0 { best = (d, Block::Shelf(shelf)); }
        }
        if self.grid_head {
            for row in [saved_row.min(self.rows.saturating_sub(1)), 0] {
                let d = (self.row_reveal(row) - scroll).abs();
                if d < best.0 {
                    best = (d, if self.rows > 0 { Block::Grid(row) } else { Block::Toolbar });
                }
            }
        }
        Some(best.1)
    }

    pub(super) fn visible_rows(self, scroll: f32) -> (usize, usize) {
        if self.rows == 0 { return (0, 0); }
        let local = self.doc_to_grid(scroll);
        let lo = ((local - CARD_H) / GRID_PITCH).floor().max(0.0) as usize;
        let hi = (((local + SCR_H - CONTENT_TOP) / GRID_PITCH).ceil().max(0.0) as usize + 1).min(self.rows);
        (lo.min(hi), hi)
    }

    pub(super) fn grid_x(col: usize) -> f32 { MARGIN_X + col as f32 * (CARD_W + GRID_GAP) }

    pub(super) fn rail_rect(self, scroll: f32) -> crate::ui::Rect {
        let top = self.row_y(0, scroll).max(CONTENT_TOP);
        crate::ui::Rect::new(GRID_RIGHT + theme::space::XS, top, RAIL_TRACK_W, SCR_H - MARGIN_Y - top)
    }
}

/// Preserve the selected favourite while reserving a visible overflow control.
pub(super) fn library_window(widths: &[f32], selected: usize, available: f32, gap: f32, more: f32, cap: usize) -> (usize, usize) {
    if widths.is_empty() || cap == 0 { return (0, 0); }
    let total = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
    if widths.len() <= cap && total <= available { return (0, widths.len()); }
    let budget = available - more - gap;
    let room = cap.saturating_sub(1).max(1);
    let selected = selected.min(widths.len() - 1);
    for start in 0..=selected {
        let mut width = 0.0;
        let mut len = 0;
        for &pill in widths.iter().skip(start).take(room) {
            let step = pill + if len == 0 { 0.0 } else { gap };
            if len > 0 && width + step > budget { break; }
            width += step;
            len += 1;
        }
        if selected < start + len { return (start, len); }
    }
    (selected, 1)
}

pub(super) fn shelf_pitch(landscape: bool, expanded: f32) -> f32 {
    let art = if landscape { 236.0 } else { CARD_H };
    crate::ui::consts::TITLE_DY + crate::ui::consts::CARD_DY + art
        + crate::ui::card_row::under_band(expanded)
        + crate::ui::consts::UNDER_LABEL_AIR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_column_grid_fills_only_the_reserved_content_band() {
        assert_eq!(Layout::grid_x(0), MARGIN_X);
        let right = Layout::grid_x(COLS - 1) + CARD_W;
        assert!((right - GRID_RIGHT).abs() < 0.01);
        assert!(right + RAIL_BAND <= SCR_W - MARGIN_X + 0.01);
    }

    #[test]
    fn document_stacks_favourite_row_shelves_and_grid() {
        let portrait = shelf_pitch(false, 1.0);
        let landscape = shelf_pitch(true, 1.0);
        let lay = Layout::new(true, &[portrait, landscape], 10, true);
        assert_eq!(lay.shelf_origin(0), LIBRARY_ROW_H);
        assert_eq!(lay.shelf_origin(1), LIBRARY_ROW_H + portrait);
        assert_eq!(lay.grid_block_top(), LIBRARY_ROW_H + portrait + landscape);
        assert_eq!(lay.grid_top(), lay.grid_block_top() + GRID_HEAD_H);
        assert!(landscape < portrait);
    }

    #[test]
    fn page_window_is_grid_local_with_a_large_header() {
        let pitches = [shelf_pitch(false, 1.0); MAX_SHELVES];
        let lay = Layout::new(true, &pitches, 80, true);
        assert_eq!(lay.visible_rows(0.0), (0, 0));
        let at_grid = lay.row_reveal(8);
        let (lo, hi) = lay.visible_rows(at_grid);
        assert!(lo <= 8 && hi > 8, "{lo}..{hi}");
    }

    #[test]
    fn last_row_caption_settles_inside_overscan() {
        let lay = Layout::new(false, &[], 17, true);
        let scroll = lay.row_reveal(16);
        let bottom = lay.row_y(16, scroll) + GRID_PITCH;
        assert!(bottom <= SCR_H - MARGIN_Y + 0.01, "{bottom}");
    }

    #[test]
    fn restored_scroll_seats_the_block_it_displays() {
        let lay = Layout::new(true, &[500.0, 360.0], 20, true);
        assert_eq!(lay.seat_for_scroll(0.0, 12), Some(Block::LibraryRow));
        assert_eq!(lay.seat_for_scroll(lay.shelf_reveal(1), 12), Some(Block::Shelf(1)));
        assert_eq!(lay.seat_for_scroll(lay.row_reveal(12), 12), Some(Block::Grid(12)));
    }
}
