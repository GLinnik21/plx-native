//! **The scroll capsule and the top fade** — what a long page leaves on screen when its header and
//! the app's top chrome have tucked away: a small capsule that keeps your place ("Films 1,284"),
//! and a 72 px fade of the page's own ground colour at the top edge so rows leave under it instead
//! of being cut off.
//!
//! **Frozen ground, never live blur** (owner policy, 2026-10-17; the build spec's "live blur 24 px"
//! is NOT built). The capsule wears the same glass every menu wears
//! ([`popover_glass::draw`]: the latched underlay field plus the host snapshot, no backdrop entry,
//! no blur pass) whenever it is handed a latched field. A page's own frame carries none
//! (`DrawFrame::underlay` is `None` outside a modal stack), so the Library hands the capsule its
//! OWN field, latched once per scrolled stay from the page ground
//! (`LibraryScreen::latch_capsule_field`). With no field at all the capsule is the
//! flat form of the same material: the page's ground colour at [`GROUND_TINT`] opacity with the
//! menu glass's rim. Either way it declares no live-glass surface and costs one quad.
//!
//! Pure geometry and colour live here and are tested without a frame; the library's choice of WHEN
//! to show them (`screens::library::scroll_chrome`) stays with the screen.
use std::ffi::CStr;

use plx_machine::machine::Measure;

use crate::{popover_glass, theme, underlay::UnderlayField, Painter, Rect};

/// Left edge, top edge and height of the capsule: the board's x 96, y 54, 64 tall. The width is the
/// text's ([`width`]), never more than [`W`].
pub const X: f32 = 96.0;
pub const Y: f32 = 54.0;
pub const H: f32 = 64.0;
/// The widest the capsule gets (the board's 320): a long library name is shortened to stay inside.
pub const W: f32 = 320.0;
/// A full capsule: the corner radius is half the height.
pub const RADIUS: f32 = H * 0.5;
/// The board's ground-colour tint over the capsule: 55%.
pub const GROUND_TINT: f32 = 0.55;
/// Room the text keeps from the capsule's left end, and from its right end.
pub const PAD_L: f32 = 28.0;
pub const PAD_R: f32 = 28.0;
/// Space between the name and the count.
pub const NAME_COUNT_GAP: f32 = 10.0;
/// Height of the fade at the page's top edge, painted in the ambient ground colour, never black.
pub const FADE_H: f32 = 72.0;

/// The widest capsule's rectangle.
pub fn rect() -> Rect {
    Rect::new(X, Y, W, H)
}

/// What the name and the count may take together between the paddings (the gap included).
pub fn label_budget() -> f32 {
    W - PAD_L - PAD_R
}

fn run_w(s: &CStr, size: i32, bold: bool, measure: &dyn Measure) -> f32 {
    measure.width(s, size, bold)
}

/// The capsule's width for `name` (BODY bold) and `count` (CAPTION regular): left padding, the two
/// runs and the gap between them, right padding.
pub fn width(name: &CStr, count: &CStr, measure: &dyn Measure) -> f32 {
    PAD_L + run_w(name, theme::size::BODY, true, measure) + NAME_COUNT_GAP
        + run_w(count, theme::size::CAPTION, false, measure) + PAD_R
}

/// The flat form's fill: the ground colour at [`GROUND_TINT`].
pub fn tint(ground: [f32; 3]) -> [f32; 4] {
    [ground[0], ground[1], ground[2], GROUND_TINT]
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

/// The capsule and its text at `alpha` (the tuck: 0 draws nothing): the name at BODY bold, then the
/// count at CAPTION regular in the secondary ink, the capsule exactly as wide as that and its
/// paddings. `field` is the frame's latched underlay field when it has one (see the module doc);
/// `ground` is the page's ambient ground colour for the flat form.
pub fn draw(p: Painter, name: &CStr, count: &CStr, ground: [f32; 3], field: Option<&UnderlayField>,
    alpha: f32, measure: &dyn Measure)
{
    if alpha <= 0.0 {
        return;
    }
    let p = p.alpha(alpha);
    let r = Rect::new(X, Y, width(name, count, measure), H);
    if !popover_glass::draw(p, r, RADIUS, field) {
        let fill = tint(ground);
        p.rect_rimmed(r, RADIUS, fill, fill, theme::GLASS_RIM, 0.0);
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
