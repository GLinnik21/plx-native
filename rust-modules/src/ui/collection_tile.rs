//! The **neutral collection tile** — what a collection with no artwork of its own draws in place of
//! a poster: the placeholder ground, the collection mark, and the collection's name under it.
//!
//! A collection whose server sent no `thumb` has nothing to resolve, so without this the tile is a
//! bare skeleton that reads as "still loading" forever and, beside its named neighbours, as a
//! broken image. The name is the one fact that tells two such tiles apart, so it is drawn ON the
//! tile rather than left to the focused caption, which only one tile at a time wears.
//!
//! **One widget for every surface that lists collections.** It is reached through the shared card
//! composite (`widgets::card`'s poster arm hands a thumb-less collection row here), so the Library
//! grid, a Home shelf and Search draw the same tile without asking. It names no application type:
//! the caller decides that a row is a thumb-less collection and passes the name.
//!
//! A server's automatic 2×2 composite IS artwork and draws as an ordinary poster; replacing it
//! with a client-drawn fan of the members is a separate piece of work.
//!
//! The name wraps to two lines through [`TextView`]'s live-font path, the same leaf-level measure
//! `widgets::LegacyMeasure` documents: `card` is drawn by callers that hold no `Measure`, and the
//! card composite skips recording passes entirely, so no recorded layout depends on this text.
use crate::ui::icons::{self, Icon};
use crate::ui::label::HAlign;
use crate::ui::text_view::TextView;
use crate::ui::{theme, Painter, Rect};

/// The mark's size as a fraction of the tile's width.
const GLYPH_RATIO: f32 = 0.3;
/// Air between the mark and the name's cap band, as a fraction of the tile's width.
const GAP_RATIO: f32 = 0.08;
/// Side inset of the name's column, as a fraction of the tile's width.
const INSET_RATIO: f32 = 0.1;
/// The name's size. `CAPTION` is the couch floor for content, and a collection name is content.
const NAME_SIZE: i32 = theme::size::CAPTION;
const NAME_LINES: usize = 2;

/// The mark's raster size for a tile `w` wide — quantized to 4px so a focus pop reuses a handful
/// of cached masks instead of rasterizing one per rounded pixel (the person glyph's discipline).
pub(crate) fn glyph_px(w: f32) -> f32 {
    ((w * GLYPH_RATIO) / 4.0).round() * 4.0
}

/// The name column's wrap width for a tile `w` wide — floored to 4px, the mark's discipline,
/// because the wrap is memoised per width: a focus pop that wrapped at every rounded pixel would
/// insert a fresh entry each frame and churn the process-wide wrap cache every other text block
/// shares. The column stays centred, so a resting 250px tile (a 200px column) is unchanged.
fn name_w(w: f32) -> f32 {
    ((w * (1.0 - 2.0 * INSET_RATIO)) / 4.0 + 1.0e-3).floor() * 4.0
}

/// Where the mark and the name's column sit in `r`: the pair is centred vertically as one block,
/// the mark above, sized from the tile so a scaled (popped) tile scales its layout with it.
pub(crate) fn layout(r: Rect, name_h: f32) -> (Rect, Rect) {
    let d = glyph_px(r.w);
    let gap = r.w * GAP_RATIO;
    let w = name_w(r.w);
    let top = r.y + (r.h - (d + gap + name_h)) * 0.5;
    let glyph = Rect::new(r.cx() - d * 0.5, top, d, d);
    let name = Rect::new(r.cx() - w * 0.5, top + d + gap, w, name_h);
    (glyph, name)
}

fn name_view(name: &str) -> TextView<'_> {
    TextView::new(name, NAME_SIZE, theme::TEXT_SECONDARY)
        .bold()
        .h(HAlign::Center)
        .max_lines(NAME_LINES)
}

/// Draw the neutral tile into `r` with corner radius `rad`. The caller has already decided the
/// row is a collection with no artwork (`thumb` empty) — an unresolved texture with a path behind
/// it is merely still loading and keeps the ordinary skeleton.
pub(crate) fn draw(p: Painter, r: Rect, rad: f32, name: &str) {
    p.rrect_sheened(r, rad, theme::CARD_PLACEHOLDER);
    let view = name_view(name);
    let name_h = if name.is_empty() { 0.0 } else { view.measure_h(name_w(r.w)) };
    let (glyph, column) = layout(r, name_h);
    icons::draw(p, Icon::Collection, glyph, theme::TEXT_TERTIARY);
    if !name.is_empty() {
        view.draw(p, column);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_and_name_are_one_block_centred_on_the_tile() {
        let r = Rect::new(100.0, 200.0, 250.0, 375.0);
        let (glyph, name) = layout(r, 64.0);
        assert_eq!(glyph.w, 76.0, "a grid poster's mark quantizes to 76px");
        assert!((glyph.cx() - r.cx()).abs() < 0.01);
        let top_air = glyph.y - r.y;
        let bottom_air = r.y + r.h - (name.y + name.h);
        assert!((top_air - bottom_air).abs() < 0.01, "the block is centred: {top_air} vs {bottom_air}");
        assert!(name.y > glyph.y + glyph.h, "the name sits under the mark");
        assert!(name.x > r.x && name.x + name.w < r.x + r.w, "the name keeps its side inset");
    }

    #[test]
    fn a_pop_wraps_the_name_at_a_handful_of_widths_and_rest_is_unchanged() {
        assert_eq!(name_w(250.0), 200.0, "a resting grid poster keeps its 200px column");
        let widths: std::collections::BTreeSet<i32> = (0..=20)
            .map(|step| name_w(250.0 * (1.0 + step as f32 * 0.004)) as i32)
            .collect();
        assert!(widths.len() <= 5, "a 1.08 pop spans a handful of wrap widths: {widths:?}");
        assert!(widths.iter().all(|w| w % 4 == 0));
    }

    #[test]
    fn a_pop_reuses_quantized_mark_sizes() {
        let sizes: std::collections::BTreeSet<i32> = (0..=20)
            .map(|step| glyph_px(250.0 * (1.0 + step as f32 * 0.004)) as i32)
            .collect();
        assert!(sizes.len() <= 2, "a 1.08 pop spans at most two cached masks: {sizes:?}");
        assert!(sizes.iter().all(|px| px % 4 == 0));
    }
}
