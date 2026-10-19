//! [`SampleRow`] — a still poster row for a page that illustrates one: a few tiles, one of them
//! lifted and captioned, drawn by the SAME tile and label painters the shelves and the Library grid
//! use. The Settings > Under posters preview is its caller, so what that page shows beneath a
//! poster is the card the shelves draw, not a screen-local imitation of it.
//!
//! It is geometry and paint only: no springs (a still picture has no motion), no focus (the engine's
//! focus is on the page's rows), no hit stops. The wrapper exists so a screen names no L0 tile
//! painter, as [`AvatarRow`](super::AvatarRow) does for the circular row.

use plx_machine::machine::Measure;

use crate::card_row::{self, RowStyle, TileLabel};
use crate::widgets::Art;
use crate::{Painter, Rect};

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

    /// Paint `tiles` as a row in `area`. Tile `focused` is lifted, drawn last (its z-order is the
    /// shelves') and wears `label`; every other tile is at rest and wears none.
    pub fn draw<'a>(
        p: Painter,
        area: Rect,
        sty: &RowStyle,
        tiles: impl ExactSizeIterator<Item = Art<'a>>,
        focused: usize,
        label: &TileLabel,
        measure: &dyn Measure,
    ) {
        let n = tiles.len();
        let mut lifted = None;
        for (i, art) in tiles.enumerate() {
            if i == focused {
                lifted = Some(art);
            } else {
                card_row::draw_tile(p, art, Self::tile(sty, n, i, area), 1.0, sty, None);
            }
        }
        if let Some(art) = lifted {
            card_row::draw_focused(p, art, Self::lifted(sty, n, focused, area), sty.focus_scale, sty, None, label, measure);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
