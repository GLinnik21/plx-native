//! Pure geometry for the owned Library's single vertical document.

use plx_ui::consts::{CARD_W, MARGIN_X, MARGIN_Y, SCR_H, SCR_W};
use plx_ui::cards::RowStyle;
use plx_ui::theme;

pub(super) const COLS: usize = 6;
const EPISODE_COLS: usize = 4;
pub(super) const MAX_LIBRARY_PILLS: usize = 8;
pub(super) use plx_ui::cards::{GridBand, MAX_GRID_BANDS};
/// Where the page's header row stands (`PageHeader`'s top): the document starts here.
pub(super) const HEADER_Y: f32 = plx_ui::page_header::TOP;
/// A one-library page has no names, so its controls row sits this far lower than `HEADER_Y`: the
/// board's "row centred at y 158" is the row's top (spec, "Structure").
pub(super) const SINGLE_LIBRARY_DY: f32 = 10.0;
pub(super) const CONTENT_TOP: f32 = HEADER_Y;
/// The header block: the header row (56) and the air under it, so the grid's first tile top lands
/// on [`LIBRARY_GRID_TOP`] (244), 96 under the row's top.
pub(super) const HEADER_BLOCK_H: f32 = plx_ui::page_header::BLOCK_H;
pub(super) const LIBRARY_ROW_H: f32 = HEADER_BLOCK_H;
/// Screen y of the first grid row's tile top at rest.
#[cfg(test)]
#[cfg(test)]
pub(super) const LIBRARY_GRID_TOP: f32 = plx_ui::page_header::GRID_TOP;
#[cfg(test)]
pub(super) const GRID_PITCH: f32 = plx_ui::consts::CARD_H
    + plx_ui::cards::LABEL_BAND_COLLAPSED
    + plx_ui::consts::UNDER_LABEL_AIR;
pub(super) const RAIL_TRACK_W: f32 = 44.0;
/// The letter pitch of a short alphabet (the most a letter is given).
pub(super) const RAIL_PITCH: f32 = 32.0;
/// The least a letter is squeezed to: an alphabet that does not fit even at this pitch scrolls.
pub(super) const RAIL_MIN_PITCH: f32 = 26.0;
pub(super) const RAIL_CAP_PAD: f32 = 10.0;
pub(super) const RAIL_BAND: f32 = RAIL_TRACK_W + theme::space::XS;
pub(super) const GRID_RIGHT: f32 = SCR_W - MARGIN_X - RAIL_BAND;
pub(super) const GRID_GAP: f32 =
    (GRID_RIGHT - MARGIN_X - COLS as f32 * CARD_W) / (COLS as f32 - 1.0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Block {
    LibraryRow,
    Grid(usize),
    Status,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Layout {
    pub libraries: bool,
    pub rows: usize,
    pub status: bool,
    episodes: bool,
    grid_bands: [GridBand; MAX_GRID_BANDS],
    /// The focused grid row's band height under the Under posters setting (0 / 80 / 112; the
    /// legacy 114 until [`with_band_h`](Self::with_band_h)). A pref input, not motion: not in the canon.
    band_h: f32,
    /// The grid shows a sort value under every card (a poster or an episode still): rows keep [`plx_ui::cards::SORT_VALUE_ROW_GAP`]
    /// instead of 40 px, and the focused row opens [`plx_ui::cards::SORT_VALUE_BAND_H`]'s share. A pref
    /// and sort input, not motion: not in the canon.
    value_line: bool,
}

impl Layout {
    pub(super) const SHAPE: &'static str = "LibraryLayout{libraries:bool,rows:u32,status:bool,episodes:bool,grid_bands:[(row:u32,expansion:f32)]}";

    pub(super) fn write(&self, c: &mut plx_machine::machine::Canon) {
        let Self { libraries, rows, status, episodes, grid_bands, band_h: _, value_line: _ } = self;
        c.bool(*libraries).u32(*rows as u32).bool(*status).bool(*episodes);
        c.seq(grid_bands.iter().filter(|band| band.row != usize::MAX).count());
        for band in grid_bands.iter().filter(|band| band.row != usize::MAX) {
            c.u32(band.row as u32).f32(band.expansion);
        }
    }

    pub(super) fn new(libraries: bool, rows: usize) -> Self {
        Self {
            libraries,
            rows,
            status: false,
            episodes: false,
            grid_bands: [GridBand::CLOSED; MAX_GRID_BANDS],
            band_h: plx_ui::cards::LEGACY_BAND_H,
            value_line: false,
        }
    }

    pub(super) fn failed(libraries: bool) -> Self {
        Self { status: true, ..Self::new(libraries, 0) }
    }

    pub(super) fn with_episodes(self, episodes: bool) -> Self { Self { episodes, ..self } }

    pub(super) fn with_value_line(self, on: bool) -> Self { Self { value_line: on, ..self } }

    pub(super) fn value_line(&self) -> bool { self.value_line }

    pub(super) fn with_band_h(self, band_h: f32) -> Self { Self { band_h, ..self } }

    pub(super) fn with_grid_bands(mut self, bands: [GridBand; MAX_GRID_BANDS]) -> Self {
        self.grid_bands = bands;
        self
    }

    pub(super) fn with_grid_focus(mut self, row: Option<usize>) -> Self {
        self.grid_bands = [GridBand::CLOSED; MAX_GRID_BANDS];
        self.grid_bands[..1].copy_from_slice(&plx_ui::cards::grid_settled(row.filter(|&row| row < self.rows)));
        self
    }

    #[cfg(test)]
    pub(super) fn row_expansion(&self, row: usize) -> f32 {
        self.grid_bands.iter().find(|band| band.row == row).map_or(0.0, |band| band.expansion)
    }

    fn band_growth_before(&self, row: usize) -> f32 {
        plx_ui::cards::grid_growth_before_for(self.band_h, row.min(self.rows), &self.grid_bands)
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
        self.card_h() + if self.value_line { plx_ui::cards::SORT_VALUE_ROW_GAP }
            else { plx_ui::cards::LABEL_BAND_COLLAPSED + plx_ui::consts::UNDER_LABEL_AIR }
    }

    pub(super) fn cell_x(&self, col: usize) -> f32 {
        let style = self.style();
        MARGIN_X + col as f32 * (style.w + style.gap)
    }

    pub(super) fn library_h(&self) -> f32 {
        if self.libraries { LIBRARY_ROW_H } else { 0.0 }
    }

    pub(super) fn grid_top(&self) -> f32 {
        self.library_h()
    }

    pub(super) fn row_y(&self, row: usize, scroll: f32) -> f32 {
        CONTENT_TOP + self.row_top(row) - scroll
    }

    pub(super) fn doc_to_grid(&self, scroll: f32) -> f32 { scroll - self.grid_top() }

    pub(super) fn doc_h(&self) -> f32 { self.row_top(self.rows) }

    pub(super) fn max_scroll(&self) -> f32 {
        (self.doc_h() - (SCR_H - CONTENT_TOP) + MARGIN_Y).max(0.0)
    }

    /// The scroll that seats `row` with its poster top where the FIRST row's is at rest (the grid
    /// top, y 244), so focus reads on one line whether you are at the top of the library or deep in
    /// it. Never below 0 and never past the end of the document: the last rows of a list settle
    /// lower. The Library's grid and the Collections page's are this one layout.
    pub(super) fn row_reveal(&self, row: usize) -> f32 {
        plx_ui::cards::grid_rest_scroll(row, self.grid_pitch(), self.band_growth_before(row), self.max_scroll())
    }

    pub(super) fn first(&self) -> Option<Block> {
        if self.libraries { Some(Block::LibraryRow) }
        else if self.status { Some(Block::Status) }
        else { None }
    }

    pub(super) fn seat_for_scroll(&self, scroll: f32, saved_row: usize) -> Option<Block> {
        let first = self.first()?;
        if scroll <= 0.5 { return Some(first); }
        let mut best = (f32::INFINITY, first);
        if self.rows > 0 {
            for row in [saved_row.min(self.rows - 1), 0] {
                let d = (self.row_reveal(row) - scroll).abs();
                if d < best.0 { best = (d, Block::Grid(row)); }
            }
        }
        Some(best.1)
    }

    /// Grid rows one screenful spans below the content top: a row revealed at the top of the
    /// viewport leaves every row at least this far below it undrawn. The grid head band only
    /// pushes rows further down, so the document's head shows no more than this either.
    pub(super) fn rows_per_screen(&self) -> usize {
        ((SCR_H - CONTENT_TOP) / self.grid_pitch()).ceil() as usize
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

/// The rail's top when the whole alphabet is there to be fitted: the board starts the 27 letters
/// at y 152, above the grid's top, so they stand at the full 32 pitch before any is squeezed.
const RAIL_TOP_HIGH: f32 = 152.0;
/// The rail's top for a short alphabet: the grid's top (the first tile row).
const RAIL_TOP: f32 = HEADER_Y + HEADER_BLOCK_H;
/// The bottom the letters may reach (the bottom margin).
const RAIL_BOTTOM: f32 = SCR_H - MARGIN_Y - RAIL_CAP_PAD;
/// The height the letters may take when the rail starts as high as it goes.
const RAIL_AVAIL: f32 = RAIL_BOTTOM - RAIL_TOP_HIGH;
/// The board's letter column centre: the track box is x 1822, 44 wide.
pub(super) const RAIL_CX: f32 = 1844.0;

/// The pitch of `n` letters: [`RAIL_PITCH`], scaled down only as far as it takes (never below
/// [`RAIL_MIN_PITCH`]) for the whole alphabet (a Latin one is 27 letters with `#`) to stand
/// between [`RAIL_TOP_HIGH`] and the bottom margin without scrolling. The rail starts higher
/// first; the pitch shrinks only once even that is full.
pub(super) fn rail_pitch(n: usize) -> f32 {
    (RAIL_AVAIL / n.max(1) as f32).clamp(RAIL_MIN_PITCH, RAIL_PITCH)
}

/// The original fixed rail band: it never follows the scrolling first grid row.
/// `(top, centre x, window height, scroll range)`; the range is 0 when all `n` letters fit.
pub(super) fn rail_geom(n: usize) -> (f32, f32, f32, f32) {
    let pitch = rail_pitch(n);
    let all = n as f32 * pitch;
    if all <= RAIL_AVAIL + 0.01 {
        // bottom-anchored between the two tops: a short alphabet sits under the header at the
        // grid's top, a full one climbs as far as RAIL_TOP_HIGH.
        return ((RAIL_BOTTOM - all).clamp(RAIL_TOP_HIGH, RAIL_TOP), RAIL_CX, all, 0.0);
    }
    let height = (RAIL_AVAIL / pitch).floor().max(1.0) * pitch;
    (RAIL_TOP_HIGH, RAIL_CX, height, all - height)
}

pub(super) fn rail_scroll_target(scroll: f32, drive: usize, n: usize) -> f32 {
    let (_, _, height, max) = rail_geom(n);
    let pitch = rail_pitch(n);
    let drive = drive.min(n.saturating_sub(1)) as f32;
    plx_ui::cards::reveal(scroll, (drive + 2.0) * pitch - height,
        (drive - 1.0) * pitch, max)
}

/// The centre of a rail of BANDS (the sort's value ladder): ten px nearer the posters than the letters'
/// ([`RAIL_CX`]), the board's `x 1834`.
pub(super) const BAND_RAIL_CX: f32 = 1834.0;
/// The distance between two bands' tops: the board's 40. The letters' pitch is [`rail_pitch`]: 32 at
/// most, squeezed toward 26 for a long alphabet.
pub(super) const BAND_PITCH: f32 = 40.0;
/// The air a band row adds where its scale changes (a calendar rail's months give way to years, its
/// years to decades): the board's one spacing step. [`BandRows::new`] takes the rows it goes before
/// (`rail_bands::steps` names them from the bands' marks), and the fixed value ladders have none.
pub(super) const BAND_STEP_GAP: f32 = theme::space::XS;
/// The height of the focused band's white pill: the board's 38, which is [`BAND_PITCH`] less the two
/// px the row keeps above and below it.
pub(super) const BAND_PILL_H: f32 = 38.0;
/// The air each side of a band's label inside its pill: the board's 12 for every label that leaves
/// room for it ([`band_pill_pad`] gives some of it back to a wider one).
pub(super) const BAND_PILL_PAD: f32 = 12.0;
/// The least air each side of a label in its pill. A label too wide for [`BAND_PILL_PAD`] either side
/// within the track limit gives air back down to this and no further.
pub(super) const BAND_PILL_PAD_MIN: f32 = 4.0;
/// The widest the rail of bands may be, pill and all: its left edge stays clear of the posters,
/// which end at [`GRID_RIGHT`] (the grid does not move for it).
pub(super) const BAND_TRACK_LIMIT_W: f32 = 2.0 * (BAND_RAIL_CX - GRID_RIGHT);

/// The air each side of a label `label_w` wide inside its pill: [`BAND_PILL_PAD`], or what the track limit
/// leaves of it for a label too wide to have that much, never under [`BAND_PILL_PAD_MIN`]. So a label up to
/// [`BAND_TRACK_LIMIT_W`] less 24 keeps the board's pill, and one up to the limit less 8 (116 px) is the
/// widest any band's label may be.
pub(super) fn band_pill_pad(label_w: f32) -> f32 {
    ((BAND_TRACK_LIMIT_W - label_w) * 0.5).clamp(BAND_PILL_PAD_MIN, BAND_PILL_PAD)
}

/// The width of a rail of bands whose widest label is `widest` wide: the letters' [`RAIL_TRACK_W`] at
/// least, and wide enough for the focus pill around the widest label, never past [`BAND_TRACK_LIMIT_W`].
pub(super) fn band_track_w(widest: f32) -> f32 {
    (widest + 2.0 * band_pill_pad(widest)).clamp(RAIL_TRACK_W, BAND_TRACK_LIMIT_W.max(RAIL_TRACK_W))
}

/// Where the rows of a rail of bands stand: [`BAND_PITCH`] apart, with [`BAND_STEP_GAP`] more before
/// each row a scale change names. The letters' uniform column is the free functions above; the bands'
/// rows are not uniform, so their tops are an array and every question is asked of it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct BandRows {
    tops: Vec<f32>,
    total: f32,
    height: f32,
}

impl BandRows {
    /// The rows of `n` bands; `steps` are the indices of the rows that start a new scale (each adds
    /// [`BAND_STEP_GAP`] above it). The window is the grid's top to the bottom margin, in whole rows.
    pub(super) fn new(n: usize, steps: &[usize]) -> Self {
        let mut tops = Vec::with_capacity(n);
        let mut y = 0.0;
        for index in 0..n {
            if index > 0 && steps.contains(&index) { y += BAND_STEP_GAP; }
            tops.push(y);
            y += BAND_PITCH;
        }
        let avail = RAIL_BOTTOM - RAIL_TOP;
        let height = if y <= avail + 0.01 { y } else {
            tops.iter().map(|top| top + BAND_PITCH).take_while(|bottom| *bottom <= avail + 0.01).last()
                .unwrap_or(BAND_PITCH)
        };
        Self { tops, total: y, height }
    }

    /// The top of row `index` inside the column; one past either end continues at the pitch.
    pub(super) fn top(&self, index: isize) -> f32 {
        match usize::try_from(index) {
            Ok(i) if i < self.tops.len() => self.tops[i],
            Ok(i) => self.total + (i - self.tops.len()) as f32 * BAND_PITCH,
            Err(_) => index as f32 * BAND_PITCH,
        }
    }

    /// `(top, centre x, window height, scroll range)`, as [`rail_geom`] answers for letters: the
    /// column starts at the grid's top, and the range is 0 when every band fits.
    pub(super) fn geom(&self) -> (f32, f32, f32, f32) {
        (RAIL_TOP, BAND_RAIL_CX, self.height, (self.total - self.height).max(0.0))
    }

    /// [`rail_scroll_target`] for these rows: the driving band stays one row clear of both fades.
    pub(super) fn scroll_target(&self, scroll: f32, drive: usize) -> f32 {
        let (_, _, height, max) = self.geom();
        let drive = drive.min(self.tops.len().saturating_sub(1)) as isize;
        plx_ui::cards::reveal(scroll, self.top(drive + 1) + BAND_PITCH - height, self.top(drive - 1), max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_rows_without_focus_use_the_shared_collapsed_band() {
        for episodes in [false, true] {
            let layout = Layout::new(false, 10_000).with_episodes(episodes);
            let pitch = layout.card_h() + plx_ui::cards::under_band(0.0)
                + plx_ui::consts::UNDER_LABEL_AIR;
            assert!((layout.row_y(1, 0.0) - layout.row_y(0, 0.0) - pitch).abs() < 0.001);
        }
    }

    #[test]
    fn only_the_focused_all_row_reserves_its_caption_and_the_last_caption_fits() {
        use plx_ui::cards::{BAND_OPEN, UNDER_LABEL_H};
        for episodes in [false, true] {
            let compact = Layout::new(true, 40)
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

    fn posters(libraries: bool, rows: usize) -> Layout {
        Layout::new(libraries, rows)
    }

    #[test]
    fn the_document_stacks_chip_then_the_grid() {
        let bare = posters(false, 40);
        assert_eq!(bare.library_h(), 0.0, "no chip: the grid heads the page");
        assert_eq!(bare.grid_top(), 0.0);

        let chipped = posters(true, 40);
        assert_eq!(chipped.grid_top(), LIBRARY_ROW_H, "the chip's block leads");
    }

    /// **`doc_to_grid` is the fix for this screen's sharpest geometry bug.** `browse::want` derived
    /// its page window straight from `SCROLL`, i.e. assuming scroll 0 is grid row 0 — so under a
    /// document with a header above the grid it asked for pages rows into the catalog
    /// while row 0's own slots sat unloaded.
    #[test]
    fn the_page_window_is_grid_local_whatever_is_above_the_grid() {
        let rows = 1667; // a 10k-item section
        let flat = posters(false, rows);
        let deep = posters(true, rows);

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
        for libraries in [false, true] {
            for rows in [1usize, 2, 40, 1667] {
                let lay = posters(libraries, rows);
                let last = lay.row_reveal(rows - 1);
                assert!(
                    last <= lay.max_scroll() + 0.001,
                    "row_reveal must never ask past max_scroll (libraries: {libraries}, {rows} rows)"
                );
                // the whole last row — card, label band and the air under it — is on the panel
                let bottom = CONTENT_TOP + lay.doc_h() - last;
                assert!(
                    bottom <= SCR_H + 0.001,
                    "the last row's caption is off the panel (libraries: {libraries}, {rows} rows): {bottom}"
                );
            }
        }
    }

    /// Row snapping seats the focused row with its poster top where the FIRST row's is at rest
    /// (y 244), clamped to the document — not the minimal reveal `card_row::reveal` performs, which
    /// is right for a shelf inside a page and wrong for a wall of posters you walk down.
    #[test]
    fn row_snapping_seats_every_row_at_the_first_rows_resting_y() {
        let lay = posters(true, 40);
        // **Row 0 is the canvas's own exception**: its target is the top of the page, so the
        // header directly above it stays on screen. Snapping past it would scroll away the
        // controls the moment focus entered the grid.
        assert_eq!(lay.row_reveal(0), 0.0);
        // …and every other row is seated with its top where the first row's is at rest, so
        // the delta from row 0 is the rows between them.
        assert_eq!(lay.row_reveal(3), 3.0 * GRID_PITCH);
        assert_eq!(lay.row_y(3, lay.row_reveal(3)), lay.row_y(0, 0.0));
        // …and the clamp is the document's, so the last rows share one resting scroll
        assert_eq!(lay.row_reveal(39), lay.max_scroll());
    }

    // ---- the focus projection ----------------------------------------------------------------

    #[test]
    fn six_column_grid_fills_only_the_reserved_content_band() {
        let layout = posters(false, 40);
        assert_eq!(layout.cell_x(0), MARGIN_X);
        let right = layout.cell_x(COLS - 1) + layout.card_w();
        assert!((right - GRID_RIGHT).abs() < 0.01);
        assert!(right + RAIL_BAND <= SCR_W - MARGIN_X + 0.01);
    }

    #[test]
    fn episode_grid_fits_four_stills_before_the_rail_and_reveals_the_last_caption() {
        let lay = posters(true, 40).with_episodes(true);
        assert_eq!(lay.cols(), 4);
        assert_eq!(lay.cell_x(0), MARGIN_X);
        assert!((lay.cell_x(lay.cols() - 1) + lay.card_w() - GRID_RIGHT).abs() < 0.01);
        assert!((lay.card_w() / lay.card_h() - RowStyle::EPISODE.w / RowStyle::EPISODE.h).abs() < 0.001);
        assert!(lay.grid_pitch() < GRID_PITCH);
        assert_eq!(lay.row_reveal(8), 8.0 * lay.grid_pitch());
        let last = lay.row_reveal(39);
        assert!(lay.row_y(39, last) + lay.grid_pitch() <= SCR_H - MARGIN_Y + 0.01);
        let (lo, hi) = lay.visible_rows(lay.row_reveal(8));
        assert!(lo <= 8 && hi > 8);
        assert!(hi - lo < 8, "the episode page window stays bounded: {lo}..{hi}");
    }

    #[test]
    fn rows_per_screen_bounds_what_a_revealed_row_leaves_on_screen() {
        // The poster-gate scenes derive their targets from this: a row this far below the
        // revealed one (or below the document's head) must not be on screen at all.
        let lay = posters(true, 40);
        let per = lay.rows_per_screen();
        assert!(per >= 2, "{per}");
        for row in [0, 8, 20] {
            let scroll = lay.row_reveal(row);
            assert!(lay.row_y(row + per, scroll) >= SCR_H, "row {row}: +{per} is on screen");
            // Tight, not merely safe: below the head band a full screenful really is this many.
            assert!(row == 0 || lay.row_y(row + per - 1, scroll) < SCR_H, "row {row}: +{} is off screen", per - 1);
        }
    }

    #[test]
    fn last_row_caption_settles_inside_overscan() {
        let lay = Layout::new(false, 17);
        let scroll = lay.row_reveal(16);
        let bottom = lay.row_y(16, scroll) + GRID_PITCH;
        assert!(bottom <= SCR_H - MARGIN_Y + 0.01, "{bottom}");
    }

    #[test]
    fn restored_scroll_seats_the_block_it_displays() {
        let lay = Layout::new(true, 20);
        assert_eq!(lay.seat_for_scroll(0.0, 12), Some(Block::LibraryRow));
        assert_eq!(lay.seat_for_scroll(lay.row_reveal(12), 12), Some(Block::Grid(12)));
    }
}

#[cfg(test)]
mod sort_value_layout_tests {
    use super::*;
    use plx_ui::cards::{SORT_VALUE_BAND_H, SORT_VALUE_ROW_GAP};

    fn value_layout(rows: usize, on: bool) -> Layout {
        Layout::new(true, rows).with_value_line(on).with_band_h(if on { SORT_VALUE_BAND_H } else { 80.0 })
    }

    #[test]
    fn rows_are_sixty_px_apart_with_values_and_forty_without() {
        let (with, without) = (value_layout(17, true), value_layout(17, false));
        assert!(with.value_line() && !without.value_line());
        assert_eq!(with.grid_pitch() - with.card_h(), 60.0);
        assert_eq!(without.grid_pitch() - without.card_h(), 40.0);
        assert_eq!(SORT_VALUE_ROW_GAP, 60.0);
        // No row is focused: every row is one pitch from the next.
        assert!((with.row_y(5, 0.0) - with.row_y(4, 0.0) - (with.card_h() + 60.0)).abs() < 0.01);
        assert!((without.row_y(5, 0.0) - without.row_y(4, 0.0) - (without.card_h() + 40.0)).abs() < 0.01);
    }

    #[test]
    fn the_focused_row_opens_twenty_six_px_with_values() {
        let plain = value_layout(17, true);
        let open = value_layout(17, true).with_grid_focus(Some(3));
        assert!((open.row_y(3, 0.0) - plain.row_y(3, 0.0)).abs() < 0.01, "the focused row itself does not move");
        assert!((open.row_y(4, 0.0) - plain.row_y(4, 0.0) - 26.0).abs() < 0.01, "the row below opens 26 px");
        assert!((open.row_y(9, 0.0) - plain.row_y(9, 0.0) - 26.0).abs() < 0.01, "and so does every row after it");
        assert!((open.row_y(2, 0.0) - plain.row_y(2, 0.0)).abs() < 0.01, "rows above stay");
    }

    #[test]
    fn the_last_row_and_its_value_stay_inside_the_document() {
        let layout = value_layout(17, true).with_grid_focus(Some(16));
        assert!(layout.row_y(16, layout.max_scroll()) + layout.card_h() + plx_ui::cards::SORT_VALUE_REACH
            <= SCR_H - MARGIN_Y + 0.01);
    }
}
