//! **The scroll capsule and the top fade** — what a long page leaves on screen when its header and
//! the app's top chrome have tucked away: a small capsule that keeps your place ("Films 1,284"),
//! and a 72 px fade of the page's own ground colour at the top edge so rows leave under it instead
//! of being cut off.
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
//! Pure geometry and colour live here and are tested without a frame; the library's choice of WHEN
//! to show them (`screens::library::scroll_chrome`) stays with the screen.
use std::ffi::CStr;

use plx_machine::machine::Measure;

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

fn run_w(s: &CStr, size: i32, bold: bool, measure: &dyn Measure) -> f32 {
    measure.width(s, size, bold)
}

/// The capsule's width for `name` (BODY bold) and `count` (CAPTION regular): left padding, the two
/// runs and the gap between them, right padding.
pub fn width(name: &CStr, count: &CStr, measure: &dyn Measure) -> f32 {
    PAD_L + run_w(name, theme::size::BODY, true, measure) + NAME_COUNT_GAP
        + run_w(count, theme::size::CAPTION, false, measure) + PAD_R
}

/// The fade's two stops, top to bottom: the ground colour opaque at the very edge, clear at
/// [`FADE_H`] down. Shared by [`draw_top_fade`] and its test.
pub fn fade_stops(ground: [f32; 3]) -> ([f32; 4], [f32; 4]) {
    ([ground[0], ground[1], ground[2], 1.0], [ground[0], ground[1], ground[2], 0.0])
}

/// The top-edge fade over a `width` wide page, at `alpha` (the tuck: 0 draws nothing).
pub fn draw_top_fade(p: Painter, width: f32, ground: [f32; 3], alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let (top, bottom) = fade_stops(ground);
    p.alpha(alpha).rect(Rect::new(0.0, 0.0, width, FADE_H), 0.0, top, bottom, 0.0);
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
    let w = p.text(name.as_ptr(), x, name_y, theme::size::BODY, theme::TEXT_PRIMARY, 0, 1);
    let count_y = plx_gfx::text::text_vcenter_y(theme::size::CAPTION, 1, cy);
    p.text(count.as_ptr(), x + w + NAME_COUNT_GAP, count_y, theme::size::CAPTION, theme::TEXT_SECONDARY, 0, 0);
}

#[cfg(test)]
#[path = "scroll_capsule_tests.rs"]
mod tests;
