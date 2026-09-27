//! Pure geometry for the owned Library's single vertical document.

use crate::ui::consts::{CARD_H, CARD_W, MARGIN_X, MARGIN_Y, SCR_H, SCR_W};
use crate::ui::card_row::RowStyle;
use crate::ui::theme;

pub(super) const COLS: usize = 6;
const EPISODE_COLS: usize = 4;
pub(super) const MAX_LIBRARY_PILLS: usize = 8;
pub(super) const MAX_SHELVES: usize = 12;
/// Only the focused and closing bands are represented, independent of catalog size.
/// Sixteen spans more row moves than a complete spring at normal remote repeat speed.
pub(super) const MAX_GRID_BANDS: usize = 16;
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct GridBand {
    pub row: usize,
    pub expansion: f32,
}
impl GridBand {
    pub const CLOSED: Self = Self { row: usize::MAX, expansion: 0.0 };
}
pub(super) const MAX_LETTERS: usize = 64;
pub(super) const CONTENT_TOP: f32 = crate::ui::consts::GRID_TOP_Y;
pub(super) const LIBRARY_ROW_H: f32 = crate::ui::widgets::StatusOverlay::CTRL_H + crate::ui::consts::CARD_DY + crate::ui::consts::TITLE_DY;
pub(super) const GRID_HEAD_H: f32 = crate::ui::consts::TITLE_DY
    + crate::ui::consts::CARD_DY
    + 52.0
    + crate::ui::consts::CARD_DY;
#[cfg(test)]
pub(super) const GRID_PITCH: f32 = CARD_H
    + crate::ui::card_row::LABEL_BAND_COLLAPSED
    + crate::ui::consts::UNDER_LABEL_AIR;
pub(super) const RAIL_TRACK_W: f32 = 44.0;
pub(super) const RAIL_PITCH: f32 = 34.0;
pub(super) const RAIL_CAP_PAD: f32 = 10.0;
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
    episodes: bool,
    pitches: [f32; MAX_SHELVES],
    grid_bands: [GridBand; MAX_GRID_BANDS],
}

impl Layout {
    pub(super) const SHAPE: &'static str = "LibraryLayout{libraries:bool,shelves:u32,rows:u32,grid_head:bool,status:bool,episodes:bool,pitches:[f32;12],grid_bands:[(row:u32,expansion:f32)]}";

    pub(super) fn write(&self, c: &mut crate::ui::machine::Canon) {
        let Self { libraries, shelves, rows, grid_head, status, episodes, pitches, grid_bands } = self;
        c.bool(*libraries).u32(*shelves as u32).u32(*rows as u32).bool(*grid_head).bool(*status).bool(*episodes);
        for pitch in pitches { c.f32(*pitch); }
        c.seq(grid_bands.iter().filter(|band| band.row != usize::MAX).count());
        for band in grid_bands.iter().filter(|band| band.row != usize::MAX) {
            c.u32(band.row as u32).f32(band.expansion);
        }
    }

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
            episodes: false,
            pitches,
            grid_bands: [GridBand::CLOSED; MAX_GRID_BANDS],
        }
    }

    pub(super) fn failed(libraries: bool, shelf_pitches: &[f32]) -> Self {
        Self { status: true, ..Self::new(libraries, shelf_pitches, 0, false) }
    }

    pub(super) fn with_episodes(self, episodes: bool) -> Self { Self { episodes, ..self } }

    pub(super) fn with_grid_bands(mut self, bands: [GridBand; MAX_GRID_BANDS]) -> Self {
        self.grid_bands = bands;
        self
    }

    pub(super) fn with_grid_focus(mut self, row: Option<usize>) -> Self {
        self.grid_bands = [GridBand::CLOSED; MAX_GRID_BANDS];
        if let Some(row) = row.filter(|&row| row < self.rows) {
            self.grid_bands[0] = GridBand { row, expansion: 1.0 };
        }
        self
    }

    pub(super) fn row_expansion(&self, row: usize) -> f32 {
        self.grid_bands.iter().find(|band| band.row == row).map_or(0.0, |band| band.expansion)
    }

    fn band_growth_before(&self, row: usize) -> f32 {
        self.grid_bands.iter().filter(|band| band.row < row.min(self.rows))
            .map(|band| crate::ui::card_row::under_band(band.expansion)
                - crate::ui::card_row::LABEL_BAND_COLLAPSED).sum()
    }

    fn row_top(&self, row: usize) -> f32 {
        self.grid_top() + row as f32 * self.grid_pitch() + self.band_growth_before(row)
    }

    pub(super) fn is_episodes(&self) -> bool { self.episodes }

    pub(super) fn cols(&self) -> usize { if self.episodes { EPISODE_COLS } else { COLS } }

    /// The grid keeps the shared episode treatment and gap, fitting four stills in the band
    /// before the alphabet rail. Its height follows the shared aspect, so the grid's hit boxes,
    /// scroll window and draws all describe the same cards.
    pub(super) fn style(&self) -> RowStyle {
        let style = if self.episodes {
            let base = RowStyle::EPISODE;
            let w = (GRID_RIGHT - MARGIN_X - (EPISODE_COLS - 1) as f32 * base.gap)
                / EPISODE_COLS as f32;
            RowStyle { w, h: w * base.h / base.w, ..base }
        } else { RowStyle { gap: GRID_GAP, ..RowStyle::HOME } };
        style.with_right_reserve(SCR_W - GRID_RIGHT)
    }

    pub(super) fn card_w(&self) -> f32 { self.style().w }

    pub(super) fn card_h(&self) -> f32 { self.style().h }

    pub(super) fn grid_pitch(&self) -> f32 {
        self.card_h() + crate::ui::card_row::LABEL_BAND_COLLAPSED + crate::ui::consts::UNDER_LABEL_AIR
    }

    pub(super) fn cell_x(&self, col: usize) -> f32 {
        let style = self.style();
        MARGIN_X + col as f32 * (style.w + style.gap)
    }

    pub(super) fn library_h(&self) -> f32 {
        if self.libraries { LIBRARY_ROW_H } else { 0.0 }
    }

    pub(super) fn shelf_pitch(&self, index: usize) -> f32 {
        self.pitches.get(index).copied().unwrap_or(crate::ui::consts::ROW_PITCH)
    }

    pub(super) fn shelf_origin(&self, index: usize) -> f32 {
        self.library_h() + self.pitches[..index.min(self.shelves)].iter().sum::<f32>()
    }

    pub(super) fn grid_block_top(&self) -> f32 {
        self.library_h() + self.pitches[..self.shelves].iter().sum::<f32>()
    }

    pub(super) fn grid_top(&self) -> f32 {
        self.grid_block_top() + if self.grid_head { GRID_HEAD_H } else { 0.0 }
    }

    pub(super) fn row_y(&self, row: usize, scroll: f32) -> f32 {
        CONTENT_TOP + self.row_top(row) - scroll
    }

    pub(super) fn shelf_y(&self, shelf: usize, scroll: f32) -> f32 {
        CONTENT_TOP + self.shelf_origin(shelf) - scroll
    }

    pub(super) fn doc_to_grid(&self, scroll: f32) -> f32 { scroll - self.grid_top() }

    pub(super) fn doc_h(&self) -> f32 { self.row_top(self.rows) }

    pub(super) fn max_scroll(&self) -> f32 {
        (self.doc_h() - (SCR_H - CONTENT_TOP) + MARGIN_Y).max(0.0)
    }

    pub(super) fn row_reveal(&self, row: usize) -> f32 {
        let top = if row == 0 { self.grid_block_top() } else { self.row_top(row) };
        top.clamp(0.0, self.max_scroll())
    }

    pub(super) fn shelf_reveal(&self, shelf: usize) -> f32 {
        (self.shelf_origin(shelf) - crate::ui::consts::TITLE_DY).clamp(0.0, self.max_scroll())
    }

    pub(super) fn first(&self) -> Option<Block> {
        if self.libraries { Some(Block::LibraryRow) }
        else if self.shelves > 0 { Some(Block::Shelf(0)) }
        else if self.status { Some(Block::Status) }
        else if self.grid_head { Some(Block::Toolbar) }
        else { None }
    }

    pub(super) fn seat_for_scroll(&self, scroll: f32, saved_row: usize) -> Option<Block> {
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

    pub(super) fn visible_rows(&self, scroll: f32) -> (usize, usize) {
        if self.rows == 0 { return (0, 0); }
        let local = self.doc_to_grid(scroll);
        let pitch = self.grid_pitch();
        // Prefix growth is nonnegative and at most the sparse bands' total. This conservative
        // inverse retains only nearby rows without scanning a catalog or binary-searching it.
        let growth = self.band_growth_before(self.rows);
        let lo = ((local - self.card_h() - growth) / pitch).floor().max(0.0) as usize;
        let hi = (((local + SCR_H - CONTENT_TOP) / pitch).ceil().max(0.0) as usize + 1).min(self.rows);
        (lo.min(hi), hi)
    }

}

/// The original fixed rail band: it never follows the scrolling first grid row.
pub(super) fn rail_geom(n: usize) -> (f32, f32, f32, f32) {
    const TOP: f32 = 232.0 + 8.0;
    let visible = ((SCR_H - MARGIN_Y - TOP - RAIL_CAP_PAD) / RAIL_PITCH).floor().max(1.0);
    let height = visible.min(n as f32) * RAIL_PITCH;
    (TOP, SCR_W - MARGIN_X - RAIL_TRACK_W * 0.5, height, (n as f32 * RAIL_PITCH - height).max(0.0))
}

pub(super) fn rail_scroll_target(scroll: f32, drive: usize, n: usize) -> f32 {
    let (_, _, height, max) = rail_geom(n);
    let drive = drive.min(n.saturating_sub(1)) as f32;
    crate::ui::card_row::reveal(scroll, (drive + 2.0) * RAIL_PITCH - height,
        (drive - 1.0) * RAIL_PITCH, max)
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
    let art = if landscape { crate::ui::card_row::RowStyle::EPISODE.h } else { CARD_H };
    crate::ui::consts::TITLE_DY + crate::ui::consts::CARD_DY + art
        + crate::ui::card_row::under_band(expanded)
        + crate::ui::consts::UNDER_LABEL_AIR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_rows_without_focus_use_the_shared_collapsed_band() {
        for episodes in [false, true] {
            let layout = Layout::new(false, &[], 10_000, true).with_episodes(episodes);
            let pitch = layout.card_h() + crate::ui::card_row::under_band(0.0)
                + crate::ui::consts::UNDER_LABEL_AIR;
            assert!((layout.row_y(1, 0.0) - layout.row_y(0, 0.0) - pitch).abs() < 0.001);
        }
    }

    #[test]
    fn only_the_focused_all_row_reserves_its_caption_and_the_last_caption_fits() {
        use crate::ui::card_row::{BAND_OPEN, UNDER_LABEL_H};
        for episodes in [false, true] {
            let compact = Layout::new(true, &[shelf_pitch(false, 0.0)], 40, true)
                .with_episodes(episodes);
            for focused in [0, 3, 39] {
                let layout = compact.with_grid_focus(Some(focused));
                assert!((layout.doc_h() - compact.doc_h() - BAND_OPEN).abs() < 0.002);
                for row in [0, 3, 39] {
                    let growth = if row > focused { BAND_OPEN } else { 0.0 };
                    assert!((layout.row_y(row, 0.0) - compact.row_y(row, 0.0) - growth).abs() < 0.002);
                    assert_eq!(layout.row_expansion(row), f32::from(row == focused));
                }
                let scroll = layout.row_reveal(focused);
                let (lo, hi) = layout.visible_rows(scroll);
                assert!((lo..hi).contains(&focused));
                assert!(hi - lo < 8, "paging remains bounded for both card shapes");
                assert!(layout.row_y(focused, scroll) + layout.card_h() + UNDER_LABEL_H
                    <= SCR_H - MARGIN_Y + 0.01);
            }
        }
    }

    fn posters(libraries: bool, shelves: usize, rows: usize, grid_head: bool) -> Layout {
        Layout::new(libraries, &vec![crate::ui::consts::ROW_PITCH; shelves], rows, grid_head)
    }

    #[test]
    fn the_document_stacks_chip_then_shelves_then_the_grid() {
        let bare = posters(false, 0, 40, true);
        assert_eq!(bare.grid_block_top(), 0.0, "no chip, no shelves: the grid heads the page");
        assert_eq!(bare.grid_top(), GRID_HEAD_H, "…under its own heading row");

        let chipped = posters(true, 0, 40, true);
        assert_eq!(chipped.grid_block_top(), LIBRARY_ROW_H, "the chip's block leads");

        let shelved = posters(true, 12, 40, true);
        assert_eq!(
            shelved.grid_block_top(),
            LIBRARY_ROW_H + 12.0 * crate::ui::consts::ROW_PITCH,
            "twelve shelves push the grid exactly twelve pitches down"
        );
        // …and a shelf's own origin is the one Home hangs a shelf from, so `card_row` draws here
        // unchanged: heading at origin − TITLE_DY, cards at origin + CARD_DY
        assert_eq!(shelved.shelf_origin(0), LIBRARY_ROW_H);
        assert_eq!(
            shelved.shelf_origin(3) - shelved.shelf_origin(2),
            crate::ui::consts::ROW_PITCH
        );

        // a grid with nothing in it draws no heading and no control row, so the block IS the grid
        let empty = posters(true, 2, 0, false);
        assert_eq!(empty.grid_top(), empty.grid_block_top());
    }

    /// **`doc_to_grid` is the fix for this screen's sharpest geometry bug.** `browse::want` derived
    /// its page window straight from `SCROLL`, i.e. assuming scroll 0 is grid row 0 — so under a
    /// document with a header and twelve shelves it asked for pages thirteen rows into the catalog
    /// while row 0's own slots sat unloaded.
    #[test]
    fn the_page_window_is_grid_local_whatever_is_above_the_grid() {
        let rows = 1667; // a 10k-item section
        let flat = posters(false, 0, rows, true);
        let deep = posters(true, 12, rows, true);

        // at each document's own head, both ask for row 0 — the bug was that only the flat one did
        assert_eq!(flat.visible_rows(0.0).0, 0);
        assert_eq!(deep.visible_rows(0.0).0, 0);
        assert_eq!(
            deep.visible_rows(deep.grid_top()).0,
            0,
            "scrolled exactly to the grid's top, the first wanted row is still 0"
        );
        // …and one pitch further down, both have moved by exactly one row
        assert_eq!(
            deep.visible_rows(deep.grid_top() + GRID_PITCH).0,
            flat.visible_rows(flat.grid_top() + GRID_PITCH).0
        );
        // the window never runs past the catalog
        assert!(deep.visible_rows(1.0e9).1 <= rows);
    }

    /// **`max_scroll` and `row_reveal` are ONE invariant**, and this is the test that holds them to
    /// it: if they disagree the last row's caption sits off the panel, which is the bug the old
    /// hand-written `max_y`/`lo` pair carried a paragraph about.
    #[test]
    fn the_last_row_can_always_reach_its_own_caption() {
        for shelves in [0usize, 1, 12] {
            for rows in [1usize, 2, 40, 1667] {
                let lay = posters(true, shelves, rows, true);
                let last = lay.row_reveal(rows - 1);
                assert!(
                    last <= lay.max_scroll() + 0.001,
                    "row_reveal must never ask past max_scroll ({shelves} shelves, {rows} rows)"
                );
                // the whole last row — card, label band and the air under it — is on the panel
                let bottom = CONTENT_TOP + lay.doc_h() - last;
                assert!(
                    bottom <= SCR_H + 0.001,
                    "the last row's caption is off the panel ({shelves} shelves, {rows} rows): {bottom}"
                );
            }
        }
    }

    /// Row snapping puts the focused row's OWN TOP EDGE at the viewport, clamped — not the minimal
    /// reveal `card_row::reveal` performs, which is right for a shelf inside a page and wrong for a
    /// wall of posters you walk down.
    #[test]
    fn row_snapping_targets_the_rows_own_top_edge() {
        let lay = posters(false, 0, 40, true);
        // **Row 0 is the canvas's own exception**: its target is the grid BLOCK's top, so the
        // heading and the two chips directly above it stay on screen. Snapping past them would
        // scroll away the count and the controls the moment focus entered the grid.
        assert_eq!(lay.row_reveal(0), lay.grid_block_top());
        assert_ne!(
            lay.row_reveal(0),
            lay.grid_top(),
            "…which is NOT the row's own top edge — every other row is"
        );
        assert_eq!(lay.row_reveal(3), lay.grid_top() + 3.0 * GRID_PITCH);
        // …and the clamp is the document's, so the last rows share one resting scroll
        assert_eq!(lay.row_reveal(39), lay.max_scroll());
    }

    // ---- the focus projection ----------------------------------------------------------------

    #[test]
    fn six_column_grid_fills_only_the_reserved_content_band() {
        let layout = posters(false, 0, 40, true);
        assert_eq!(layout.cell_x(0), MARGIN_X);
        let right = layout.cell_x(COLS - 1) + layout.card_w();
        assert!((right - GRID_RIGHT).abs() < 0.01);
        assert!(right + RAIL_BAND <= SCR_W - MARGIN_X + 0.01);
    }

    #[test]
    fn episode_grid_fits_four_stills_before_the_rail_and_reveals_the_last_caption() {
        let lay = posters(true, 2, 40, true).with_episodes(true);
        assert_eq!(lay.cols(), 4);
        assert_eq!(lay.cell_x(0), MARGIN_X);
        assert!((lay.cell_x(lay.cols() - 1) + lay.card_w() - GRID_RIGHT).abs() < 0.01);
        assert!((lay.card_w() / lay.card_h() - RowStyle::EPISODE.w / RowStyle::EPISODE.h).abs() < 0.001);
        assert!(lay.grid_pitch() < GRID_PITCH);
        assert_eq!(lay.row_reveal(8), lay.grid_top() + 8.0 * lay.grid_pitch());
        let last = lay.row_reveal(39);
        assert!(lay.row_y(39, last) + lay.grid_pitch() <= SCR_H - MARGIN_Y + 0.01);
        let (lo, hi) = lay.visible_rows(lay.row_reveal(8));
        assert!(lo <= 8 && hi > 8);
        assert!(hi - lo < 8, "the episode page window stays bounded: {lo}..{hi}");
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
        assert_eq!(lay.visible_rows(0.0), (0, 1), "the legacy window prefetches row zero above the grid");
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
