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
//! A server's automatic 2×2 composite is replaced by the poster store with a baked FAN of the
//! first members (`app::adapters::poster::fan`). The fan leaves the name out of its pixels: the
//! card sets it LIVE over the bake through [`draw_fan_name`], so a fan tile is told apart from its
//! neighbours unfocused too, exactly as this neutral tile is.
//!
//! The name wraps to two lines through [`TextView`]'s live-font path, the same leaf-level measure
//! `widgets::LegacyMeasure` documents: `card` is drawn by callers that hold no `Measure`, and the
//! card composite skips recording passes entirely, so no recorded layout depends on this text.
use crate::ui::icons::{self, Icon};
use std::cell::RefCell;
use std::collections::HashMap;
use std::os::raw::c_int;
use std::rc::Rc;
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

// ── The name on a baked fan: `Collections.dc.html` G1's `.art b`, whose CSS is written for a
// 250×375 tile. Every length is scaled by the tile's RESTING width over that one.

/// The mock tile's width: the unit the lengths below are written in.
const FAN_MOCK_W: f32 = 250.0;
/// `left:18px; right:18px`.
const FAN_NAME_SIDE: f32 = 18.0;
/// `bottom:22px` — the bottom of the last line's box.
const FAN_NAME_BOTTOM: f32 = 22.0;
/// `font: 700 24px/1.08` — the caption rung, bold, on a 1.08 line pitch.
const FAN_NAME_SIZE: f32 = theme::size::CAPTION as f32;
const FAN_NAME_LEAD: f32 = 1.08;
/// `text-shadow: 0 2px …` — the drop's offset.
const FAN_NAME_SHADOW_DY: f32 = 2.0;
const FAN_NAME_LINES: usize = 2;
/// Where CSS puts the cap top inside a 1.08 line box, in em: the half-leading plus the font's
/// ascent-over-cap-height, about 0.15. [`TextView`] draws line 0's cap top AT its frame's `y`.
const FAN_NAME_CAP_DROP: f32 = 0.15;

/// Whether `key` is a server composite — the thumb the poster store bakes into our fan.
pub(crate) fn is_fan(key: &str) -> bool {
    crate::plex::collections::composite_parts(key).is_some()
}

/// A fan name's settled layout for one tile width: the upper-cased text, its size, and the
/// BALANCED wrap width (`text-wrap: balance` — the narrowest column that keeps the line count).
struct FanName {
    text: String,
    sz: c_int,
    wrap_w: f32,
}

thread_local! {
    /// Per (key, name, resting width): `None` when the key is not a fan. Settled once, so a frame
    /// pays one lookup per tile rather than an upper-casing, a classification and a search.
    static FAN_NAMES: RefCell<HashMap<u64, Option<Rc<FanName>>>> = RefCell::new(HashMap::new());
}
/// A bound on [`FAN_NAMES`]; past it the memo starts over (a library's collections are far fewer).
const FAN_NAMES_CAP: usize = 512;

fn fan_view(text: &str, sz: c_int, col: [f32; 4]) -> TextView<'_> {
    TextView::new(text, sz, col)
        .bold()
        .h(HAlign::Center)
        .max_lines(FAN_NAME_LINES)
        .leading(sz as f32 * FAN_NAME_LEAD)
}

fn fan_name(key: &str, name: &str, rest_w: f32) -> Option<Rc<FanName>> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (key, name, rest_w.to_bits()).hash(&mut h);
    let id = h.finish();
    if let Some(hit) = FAN_NAMES.with(|m| m.borrow().get(&id).cloned()) {
        return hit;
    }
    let settled = (is_fan(key) && !name.is_empty()).then(|| Rc::new(settle(name, rest_w)));
    FAN_NAMES.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() >= FAN_NAMES_CAP {
            m.clear();
        }
        m.insert(id, settled.clone());
    });
    settled
}

/// Upper-case, size and balance a fan name for a tile `rest_w` wide.
fn settle(name: &str, rest_w: f32) -> FanName {
    let k = rest_w / FAN_MOCK_W;
    let sz = (FAN_NAME_SIZE * k).round().max(1.0) as c_int;
    let text = name.to_uppercase();
    let column = (rest_w - 2.0 * FAN_NAME_SIDE * k).max(1.0).floor();
    let view = fan_view(&text, sz, theme::FAN_NAME_INK);
    let lh = sz as f32 * FAN_NAME_LEAD;
    let fits = |w: f32| !view.truncates(w) && view.measure_h(w) <= lh * FAN_NAME_LINES as f32;
    let mut wrap_w = column;
    if view.measure_h(column) > lh && fits(column) {
        // Two lines: the narrowest whole-pixel column that still sets it in two, never narrower
        // than its widest word (which would be elided rather than wrapped).
        let widest = text
            .split_whitespace()
            .map(|w| fan_view(w, sz, theme::FAN_NAME_INK).last_line_w(1.0e6))
            .fold(0.0f32, f32::max);
        let (mut lo, mut hi) = ((column * 0.5).max(widest.ceil()).floor(), column);
        while hi - lo > 1.0 {
            let mid = ((lo + hi) * 0.5).floor();
            if fits(mid) { hi = mid } else { lo = mid }
        }
        wrap_w = hi;
    }
    FanName { text, sz, wrap_w }
}

/// Set a collection's NAME over its baked fan, as G1 does: centred, upper-case, at most two
/// balanced lines, its last line's box `22px` above the tile's bottom, over a drop.
///
/// `rest` is the tile's RESTING rect and `r` the rect it is drawn at (popped when focused). The
/// size and the wrap come from `rest`, so a focus pop never re-wraps; the block rides `r`'s centre
/// and bottom so it moves with the art. Does nothing when `key` is not a fan.
pub(crate) fn draw_fan_name(p: Painter, rest: Rect, r: Rect, key: &str, name: &str) {
    let Some(f) = fan_name(key, name, rest.w) else { return };
    let k = rest.w / FAN_MOCK_W;
    let ink = fan_view(&f.text, f.sz, theme::FAN_NAME_INK);
    let h = ink.measure_h(f.wrap_w);
    let top = r.y + r.h - FAN_NAME_BOTTOM * k - h + FAN_NAME_CAP_DROP * f.sz as f32;
    let at = Rect::new(r.cx() - f.wrap_w * 0.5, top, f.wrap_w, h);
    let drop = Rect::new(at.x, at.y + FAN_NAME_SHADOW_DY * k, at.w, at.h);
    fan_view(&f.text, f.sz, theme::FAN_NAME_SHADOW).draw(p, drop);
    ink.draw(p, at);
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

    #[test]
    fn only_a_server_composite_is_a_fan() {
        assert!(is_fan("/library/collections/7/composite/1700000000?width=400"));
        assert!(!is_fan("/library/metadata/7/thumb/1700000000"));
        assert!(!is_fan(""));
    }

    /// G1's name: upper-case, 24px at the mock's 250-px tile and scaled with the RESTING width,
    /// and a two-line name set in the narrowest column that keeps it on two lines.
    #[test]
    fn a_fan_name_is_upper_cased_scaled_and_balanced() {
        let _serial = crate::testlock::serial();
        let short = settle("Cars", 250.0);
        assert_eq!(short.text, "CARS");
        assert_eq!(short.sz, 24);
        assert_eq!(short.wrap_w, 250.0 - 36.0, "one line keeps the full column");
        assert_eq!(settle("Cars", 200.0).sz, 19, "sized from the tile, not fixed");

        let long = settle("Harbor Lights Mysteries", 250.0);
        let view = fan_view(&long.text, long.sz, theme::FAN_NAME_INK);
        let lh = long.sz as f32 * FAN_NAME_LEAD;
        let column = 250.0 - 36.0;
        assert!(view.measure_h(column) > lh && !view.truncates(column), "the probe name sets in two lines");
        assert!(long.wrap_w < column, "balanced narrower than the column: {}", long.wrap_w);
        assert!(!view.truncates(long.wrap_w) && view.measure_h(long.wrap_w) <= 2.0 * lh);
        assert!(view.truncates(long.wrap_w - 1.0), "and no narrower column keeps two lines");
    }
}
