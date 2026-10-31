//! [`SampleRow`] — a still poster row for a page that illustrates one: a few tiles, one of them
//! lifted and captioned, drawn by the SAME tile and label painters the shelves and the Library grid
//! use. The Settings > Under posters page AND its Details picker are its callers, so what those
//! pages show beneath a poster is the card the shelves draw, not a screen-local imitation of it.
//!
//! The row stands in a [`SampleFrame`]: the panel (a faint fill and a hairline rim), the scissor
//! that clips the outer tiles to it, the dimming of every tile but the lifted one (a column: its
//! value line dims with it) and an optional caption at the panel's foot.
//!
//! It is geometry and paint only: no springs (a still picture has no motion), no focus (the engine's
//! focus is on the page's rows), no hit stops. The wrapper exists so a screen names no L0 tile
//! painter, as [`AvatarRow`](super::AvatarRow) does for the circular row.

use plx_machine::machine::Measure;

use crate::card_row::{self, RowStyle, SortValue, TileLabel};
use crate::screen::ClipScope;
use crate::widgets::Art;
use crate::{theme, Painter, Rect};

/// The panel's corner radius.
const FRAME_RAD: f32 = 24.0;
/// The panel's fill: black at 20%, over the page.
const FRAME_FILL_A: f32 = 0.20;
/// The panel's rim, inside its edge.
const FRAME_RIM_W: f32 = 1.0;
/// The air between the panel's bottom edge and its foot caption's line box.
const FOOT_BOTTOM: f32 = 32.0;
/// The foot caption's line box: the MICRO rung and its leading.
const FOOT_LINE_H: f32 = 28.0;

/// **The panel a [`SampleRow`] stands in.** `rect` is the panel (the row's tiles are clipped to it,
/// so a row wider than the panel shows its outer tiles cut by the edges), `row` is where the row
/// of tiles stands (see [`SampleRow::tile`]), `rest_alpha` is how much of a tile that is not the
/// lifted one shows, and `foot` is an optional caption centred at the panel's foot, in the
/// tertiary ink at the MICRO rung.
#[derive(Clone, Copy)]
pub struct SampleFrame<'a> {
    pub rect: Rect,
    pub row: Rect,
    pub rest_alpha: f32,
    pub foot: Option<&'a str>,
}

impl SampleFrame<'_> {
    /// The panel under the row: a faint fill and a hairline rim, painted before any tile.
    fn panel(&self, p: Painter) {
        p.rect(self.rect, FRAME_RAD, theme::scrim_black(FRAME_FILL_A), theme::scrim_black(FRAME_FILL_A), 0.0);
        p.rring(self.rect, FRAME_RAD, FRAME_RIM_W, theme::HAIRLINE);
    }

    /// The caption at the panel's foot, centred, never cut: it is judged against the panel less its sides.
    fn draw_foot(&self, p: Painter, measure: &dyn Measure) {
        let Some(text) = self.foot else { return };
        let size = theme::size::MICRO;
        let (run, _) = measure.fit_line_measured(text, self.rect.w - 2.0 * theme::space::MD, size, false);
        let cy = self.rect.y + self.rect.h - FOOT_BOTTOM - FOOT_LINE_H * 0.5;
        p.text(run.as_ptr(), self.rect.cx(), plx_gfx::text::text_vcenter_y(size, 0, cy), size, theme::TEXT_TERTIARY, 1, 0);
    }
}

/// A still row of tiles drawn with the shelves' own painters.
pub struct SampleRow;

impl SampleRow {
    /// The settled rect of tile `i` of `n` (tiles of `sty`'s size, `sty.gap` apart): the row is
    /// centred in `area` and rests on its top edge.
    pub fn tile(sty: &RowStyle, n: usize, i: usize, area: Rect) -> Rect {
        let width = n as f32 * sty.w + n.saturating_sub(1) as f32 * sty.gap;
        let x = area.x + (area.w - width) * 0.5 + i as f32 * (sty.w + sty.gap);
        Rect::new(x, area.y, sty.w, sty.h)
    }

    /// The rect tile `i` is drawn at when it is the lifted one: its settled rect at the style's
    /// focus scale.
    pub fn lifted(sty: &RowStyle, n: usize, i: usize, area: Rect) -> Rect {
        Self::tile(sty, n, i, area).scaled(sty.focus_scale)
    }

    /// Paint `tiles` as a row in `frame`. Tile `focused` is lifted, drawn last (its z-order is the
    /// shelves') and wears `label`; every other tile is at rest, dimmed to `frame.rest_alpha`, and
    /// wears none.
    pub fn draw<'a>(
        p: Painter,
        frame: &SampleFrame<'_>,
        sty: &RowStyle,
        tiles: impl ExactSizeIterator<Item = Art<'a>>,
        focused: usize,
        label: &TileLabel,
        measure: &dyn Measure,
    ) {
        Self::framed(p, frame, measure, || Self::tiles(p, frame, sty, tiles, focused, label, measure));
    }

    /// Paint `tiles` as a library grid shows them while values are on: every tile wears its sort
    /// value line (`value(i)`) at rest, and the lifted one wears it bold, through the same painter
    /// a real grid draws with ([`card_row::draw_sort_value`]). A tile at rest is a dimmed column:
    /// its value line dims with it.
    pub fn draw_values<'a>(p: Painter, frame: &SampleFrame<'_>, sty: &RowStyle,
        tiles: impl ExactSizeIterator<Item = Art<'a>>, focused: usize,
        mut value: impl FnMut(usize) -> SortValue, measure: &dyn Measure) {
        Self::framed(p, frame, measure, || {
            let n = tiles.len();
            Self::tiles(p, frame, sty, tiles, focused, &TileLabel::default(), measure);
            // Over every tile, as the grid draws them: the lifted card's shadow reaches the value line.
            for i in 0..n {
                let (rect, s, layer) = if i == focused {
                    (Self::lifted(sty, n, i, frame.row), sty.focus_scale, p)
                } else {
                    (Self::tile(sty, n, i, frame.row), 1.0, p.alpha(frame.rest_alpha))
                };
                card_row::draw_sort_value(layer, rect, s, sty, &value(i), measure, i == focused);
            }
        });
    }

    /// The frame's panel, then `body` clipped to the frame, then its foot caption.
    fn framed(p: Painter, frame: &SampleFrame<'_>, measure: &dyn Measure, body: impl FnOnce()) {
        frame.panel(p);
        {
            let _clip = ClipScope::open_in(p, frame.rect);
            body();
        }
        frame.draw_foot(p, measure);
    }

    /// The tiles of a row, the lifted one last and wearing `label`, the others dimmed.
    fn tiles<'a>(p: Painter, frame: &SampleFrame<'_>, sty: &RowStyle,
        tiles: impl ExactSizeIterator<Item = Art<'a>>, focused: usize, label: &TileLabel, measure: &dyn Measure) {
        let n = tiles.len();
        let rest = p.alpha(frame.rest_alpha);
        let mut lifted = None;
        for (i, art) in tiles.enumerate() {
            if i == focused {
                lifted = Some(art);
            } else {
                card_row::draw_tile(rest, art, Self::tile(sty, n, i, frame.row), 1.0, sty, None);
            }
        }
        if let Some(art) = lifted {
            card_row::draw_focused(p, art, Self::lifted(sty, n, focused, frame.row), sty.focus_scale, sty, None, label, measure);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw_census::capture_runs;
    use crate::fixture::FixtureMeasure;

    #[test]
    fn the_row_is_centred_and_the_lifted_tile_grows_about_its_own_centre() {
        let sty = RowStyle { w: 100.0, h: 150.0, gap: 20.0, ..RowStyle::HOME };
        let area = Rect::new(10.0, 40.0, 500.0, 300.0);
        let tiles: Vec<Rect> = (0..3).map(|i| SampleRow::tile(&sty, 3, i, area)).collect();
        let left = tiles[0].x - area.x;
        let right = area.x + area.w - (tiles[2].x + tiles[2].w);
        assert!((left - right).abs() < 1e-3, "centred: {left} vs {right}");
        assert!((tiles[1].x - (tiles[0].x + 120.0)).abs() < 1e-3, "one gap between tiles");
        assert!(tiles.iter().all(|t| t.y == area.y && t.h == 150.0));
        let up = SampleRow::lifted(&sty, 3, 1, area);
        let (c0, c1) = ((tiles[1].x + tiles[1].w * 0.5), (up.x + up.w * 0.5));
        assert!((c0 - c1).abs() < 1e-3 && up.w > tiles[1].w);
    }

    /// The board's frame: Library cards 35 px apart in a 660 px panel stand at -80 / 205 / 490, the
    /// outer two cut by the panel's edges and only the middle one wholly inside.
    #[test]
    fn a_row_wider_than_its_frame_stands_at_the_boards_offsets_with_the_outer_tiles_cut() {
        let sty = RowStyle { gap: 35.0, ..RowStyle::HOME };
        let frame = Rect::new(96.0, 470.0, 660.0, 556.0);
        let row = Rect::new(frame.x, frame.y + 40.0, frame.w, sty.h);
        let xs: Vec<f32> = (0..3).map(|i| SampleRow::tile(&sty, 3, i, row).x - frame.x).collect();
        for (got, want) in xs.iter().zip([-80.0, 205.0, 490.0]) {
            assert!((got - want).abs() < 1e-3, "{xs:?}");
        }
        assert_eq!((sty.w, sty.h), (250.0, 375.0), "the Library's own card");
        let inside = |r: Rect| r.x >= frame.x && r.x + r.w <= frame.x + frame.w;
        assert!(!inside(SampleRow::tile(&sty, 3, 0, row)) && !inside(SampleRow::tile(&sty, 3, 2, row)));
        assert!(inside(SampleRow::lifted(&sty, 3, 1, row)), "the lifted tile is whole");
    }

    #[test]
    fn the_foot_caption_is_centred_micro_tertiary_32_px_above_the_frames_bottom_and_absent_without_one() {
        let sty = RowStyle { gap: 35.0, ..RowStyle::HOME };
        let rect = Rect::new(96.0, 470.0, 660.0, 556.0);
        let frame = |foot| SampleFrame { rect, row: Rect::new(rect.x, rect.y + 40.0, rect.w, sty.h), rest_alpha: 0.35, foot };
        let draw = |foot| capture_runs(|| {
            SampleRow::draw(Painter::recording(), &frame(foot), &sty, (0..3).map(|_| Art::Poster(None)), 1, &TileLabel::default(),
                &FixtureMeasure);
        });
        let runs = draw(Some("Sorted by Year"));
        let foot = runs.iter().find(|r| r.text == "Sorted by Year").expect("the foot caption is drawn");
        assert_eq!((foot.size, foot.bold, foot.ink), (theme::size::MICRO, false, theme::TEXT_TERTIARY));
        assert!((foot.at.x - rect.cx()).abs() < 1e-3 && foot.align == 1, "centred on the frame: {:?}", foot.at);
        let bottom = rect.y + rect.h;
        assert!(foot.at.y < bottom - FOOT_BOTTOM && foot.at.y > bottom - FOOT_BOTTOM - FOOT_LINE_H - 8.0, "its line box ends 32 px above the foot: {:?}", foot.at);
        assert!(draw(None).iter().all(|r| r.text != "Sorted by Year"), "no foot, no caption");
    }

    #[test]
    fn a_tile_at_rest_is_dimmed_with_its_value_line_and_the_lifted_column_is_not() {
        let sty = RowStyle { gap: 35.0, ..RowStyle::HOME };
        let rect = Rect::new(96.0, 470.0, 660.0, 556.0);
        let frame = SampleFrame { rect, row: Rect::new(rect.x, rect.y + 40.0, rect.w, sty.h), rest_alpha: 0.35, foot: None };
        let value = |i: usize| SortValue { text: ["2026", "2025", "2024"][i].to_string(), ..Default::default() };
        let runs = capture_runs(|| {
            SampleRow::draw_values(Painter::recording(), &frame, &sty, (0..3).map(|_| Art::Poster(None)), 1, value, &FixtureMeasure);
        });
        let ink = |text: &str| runs.iter().filter(|r| r.text == text).map(|r| r.ink[3]).fold(0.0f32, f32::max);
        assert!(ink("2026") <= 0.35 + 1e-3 && ink("2026") > 0.0, "the left value dims with its tile: {}", ink("2026"));
        assert!(ink("2024") <= 0.35 + 1e-3 && ink("2024") > 0.0, "and the right: {}", ink("2024"));
        assert!(ink("2025") > 0.9, "the lifted column's value is whole: {}", ink("2025"));
    }
}
