//! **The scroll capsule and the top fade** — what a long page leaves on screen when its header and
//! the app's top chrome have tucked away: a small capsule that keeps your place ("Films 1,284"),
//! and a fade of the page's own ground colour at the top edge so rows leave under it instead of
//! being cut off.
//!
//! **The fade is eased, and reaches through the capsule.** Its alpha falls along a smoothstep, not a
//! straight line: a linear ramp breaks slope where it ends, and over saturated poster art that
//! break reads as a bright band at y 72. It is [`FADE_H`] deep on its own; while the capsule shows
//! it reaches [`FADE_BELOW_CAPSULE`] past the capsule's own measured bottom edge, so the capsule's
//! lower half does not sit on full-strength art (the capsule's rect is the one source of that
//! extent, there is no second literal). The deeper profile arrives with the capsule's alpha, so the
//! strips stand still and only their alpha moves. The UI has no gradient primitive with an eased
//! ramp; [`Painter::rect`] is the two-stop gradient, so the ease is laid the way the hero scrim and
//! the skeleton do it: abutting [`FADE_STEP`] px strips whose stops are the curve at their edges
//! ([`fade_strips`]); the chord error is at most about half a percent of alpha.
//!
//! **Live liquid glass, the material the top navigation pill wears.** The capsule is a
//! `Glass::DYNAMIC_BACKDROP` surface: the same standing container (`GlassRim::Standing`,
//! `Material::UltraThin`, the track's own rim and lit top edge as its `GlassFace`) the tab track and
//! the hold hint wear, so it refracts the rows scrolling under it. The nav's own solved face
//! (`GlassPlan::tab_face`) is not reachable from a page's frame and goes stale while the bar is
//! tucked, which is exactly when this shows, so it takes the hold hint's fixed face
//! ([`hold_hint::glass_face`]). Where the renderer refuses the backdrop it is the flat track
//! material. (This replaces the frozen-ground form of 2026-10-17: the owner asked for the nav's
//! glass here explicitly.)
//!
//! **The whole name fits.** The capsule is as wide as the name and the count need, never shortened
//! with an ellipsis.
//!
//! Pure geometry and colour live here and are tested without a frame; the choice of WHEN to show
//! them (`scroll_chrome`) is owned by the Library (and its Collections page) and the Watchlist.
use std::ffi::CStr;

use plx_machine::machine::Measure;

use crate::hstack::{Align, HStack};
use crate::{theme, widgets, Painter, Rect};

/// Left edge, top edge and height of the capsule: the board's x 96, y 54, 64 tall. The width is the
/// text's ([`width`]): the whole name and the count, never cut.
pub const X: f32 = 96.0;
pub const Y: f32 = 54.0;
pub const H: f32 = 64.0;
/// A full capsule: the corner radius is half the height.
pub const RADIUS: f32 = H * 0.5;
/// Room the text keeps from the capsule's left end, and from its right end.
pub const PAD_L: f32 = 28.0;
pub const PAD_R: f32 = 28.0;
/// Space between the name and the count.
pub const NAME_COUNT_GAP: f32 = 10.0;
/// Height of the fade at the page's top edge, painted in the ambient ground colour, never black.
pub const FADE_H: f32 = 72.0;
/// How far the fade reaches below the capsule's bottom edge while the capsule shows: the capsule's
/// bottom (118) plus this is the fade's depth, about 130.
pub const FADE_BELOW_CAPSULE: f32 = 12.0;
/// Height of one strip of the fade: the knots the smoothstep is sampled at. 72 is twelve of them.
pub const FADE_STEP: f32 = 6.0;

fn run_w(s: &CStr, size: i32, bold: bool, measure: &dyn Measure) -> f32 {
    measure.width(s, size, bold)
}

/// The capsule's width for `name` (BODY bold) and `count` (CAPTION regular): left padding, the two
/// runs and the gap between them, right padding.
pub fn width(name: &CStr, count: &CStr, measure: &dyn Measure) -> f32 {
    PAD_L + HStack::widths(&text_widths(name, count, measure), NAME_COUNT_GAP, Align::Leading(0.0)).width() + PAD_R
}

/// The advances of the name (BODY bold) and the count (CAPTION regular): the two items of the
/// capsule's one text run, which an [`HStack`] spaces [`NAME_COUNT_GAP`] apart.
fn text_widths(name: &CStr, count: &CStr, measure: &dyn Measure) -> [f32; 2] {
    [run_w(name, theme::size::BODY, true, measure), run_w(count, theme::size::CAPTION, false, measure)]
}

/// The ground's alpha `y` px down the page's top edge: `1 - smoothstep(y / depth)`, opaque at the
/// very edge and clear at `depth`, with no slope at either end. `wide` (0..=1, the capsule's alpha)
/// moves the depth from [`FADE_H`] to `reach` by crossfading the two curves, so the strips stay put.
pub fn fade_alpha(y: f32, reach: f32, wide: f32) -> f32 {
    let edge = |depth: f32| {
        let t = (y / depth).clamp(0.0, 1.0);
        1.0 - t * t * (3.0 - 2.0 * t)
    };
    let near = edge(FADE_H);
    near + (edge(reach.max(FADE_H)) - near) * wide.clamp(0.0, 1.0)
}

/// The fade's depth while a capsule at `capsule` shows: [`FADE_BELOW_CAPSULE`] below its bottom edge,
/// never less than [`FADE_H`].
pub fn fade_reach(capsule: Rect) -> f32 {
    (capsule.y + capsule.h + FADE_BELOW_CAPSULE).max(FADE_H)
}

/// The fade as abutting strips, top to bottom: `(y, h, top stop, bottom stop)`, every stop the
/// ground colour at [`fade_alpha`] of the strip's edge. `capsule` is the capsule's measured rect and
/// its alpha while it shows (`None`: [`FADE_H`] deep). Strips that are clear at both edges are not
/// produced.
pub fn fade_strips(ground: [f32; 3], capsule: Option<(Rect, f32)>) -> impl Iterator<Item = (f32, f32, [f32; 4], [f32; 4])> {
    let (reach, wide) = capsule.map_or((FADE_H, 0.0), |(r, a)| (fade_reach(r), a));
    let depth = if wide > 0.0 { reach } else { FADE_H };
    let n = (depth / FADE_STEP).ceil() as usize;
    let colour = move |y: f32| [ground[0], ground[1], ground[2], fade_alpha(y, reach, wide)];
    (0..n).filter_map(move |k| {
        let (y0, y1) = (k as f32 * FADE_STEP, ((k + 1) as f32 * FADE_STEP).min(depth));
        let (top, bottom) = (colour(y0), colour(y1));
        (top[3] > 0.0 || bottom[3] > 0.0).then_some((y0, y1 - y0, top, bottom))
    })
}

/// The top-edge fade over a `width` wide page, at `alpha` (the tuck: 0 draws nothing). `capsule` is
/// the capsule's measured rect and its alpha while it is drawn, `None` for a page that wears none.
pub fn draw_top_fade(p: Painter, width: f32, ground: [f32; 3], alpha: f32, capsule: Option<(Rect, f32)>) {
    if alpha <= 0.0 {
        return;
    }
    let p = p.alpha(alpha);
    for (y, h, top, bottom) in fade_strips(ground, capsule) {
        p.rect(Rect::new(0.0, y, width, h), 0.0, top, bottom, 0.0);
    }
}

/// The capsule's rectangle for `name` and `count`.
pub fn rect(name: &CStr, count: &CStr, measure: &dyn Measure) -> Rect {
    Rect::new(X, Y, width(name, count, measure), H)
}

/// The capsule and its text at `alpha` (the tuck: 0 draws nothing): the name at BODY bold, then the
/// count at CAPTION regular in the secondary ink, the capsule exactly as wide as that and its
/// paddings, in the nav pill's live glass.
pub fn draw(p: Painter, name: &CStr, count: &CStr, alpha: f32, measure: &dyn Measure) {
    if alpha <= 0.0 {
        return;
    }
    let p = p.alpha(alpha);
    let r = rect(name, count, measure);
    let glass = widgets::Glass::DYNAMIC_BACKDROP.backdrop(
        p,
        r,
        0.0,
        RADIUS,
        [1.0, 1.0, 1.0, alpha],
        plx_gfx::gfx::GlassRim::Standing,
        widgets::chip_face(crate::hold_hint::glass_face(), alpha),
        theme::Material::UltraThin,
    );
    if !glass {
        let boost = theme::GLASS_RIM_LIGHT[3] - theme::GLASS_RIM[3];
        p.rect_rimmed(r, RADIUS, theme::TAB_TRACK_TOP, theme::TAB_TRACK_BOT, theme::GLASS_RIM, boost);
    }
    let cy = r.y + r.h * 0.5;
    let x = r.x + PAD_L;
    let name_y = plx_gfx::text::text_vcenter_y(theme::size::BODY, 1, cy);
    let widths = text_widths(name, count, measure);
    let run = HStack::widths(&widths, NAME_COUNT_GAP, Align::Leading(x));
    let (name_x, count_x) = (run.span(0).map_or(x, |s| s.0), run.span(1).map_or(x, |s| s.0));
    p.text(name.as_ptr(), name_x, name_y, theme::size::BODY, theme::TEXT_PRIMARY, 0, 1);
    let count_y = plx_gfx::text::text_vcenter_y(theme::size::CAPTION, 1, cy);
    p.text(count.as_ptr(), count_x, count_y, theme::size::CAPTION, theme::TEXT_SECONDARY, 0, 0);
}

#[cfg(test)]
#[path = "scroll_capsule_tests.rs"]
mod tests;
