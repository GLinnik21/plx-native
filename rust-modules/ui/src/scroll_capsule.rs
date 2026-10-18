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

use crate::{label::{HAlign, Label}, popover_glass, theme, underlay::UnderlayField, Painter, Rect};

/// Left edge, top edge and size of the capsule: the board's x 96, y 54, 320 x 64.
pub const X: f32 = 96.0;
pub const Y: f32 = 54.0;
pub const W: f32 = 320.0;
pub const H: f32 = 64.0;
/// A full capsule: the corner radius is half the height.
pub const RADIUS: f32 = H * 0.5;
/// The board's ground-colour tint over the capsule: 55%.
pub const GROUND_TINT: f32 = 0.55;
/// Horizontal room the label keeps from the capsule's ends.
pub const PAD_X: f32 = 24.0;
/// Height of the fade at the page's top edge, painted in the ambient ground colour, never black.
pub const FADE_H: f32 = 72.0;

/// The capsule's rectangle.
pub fn rect() -> Rect {
    Rect::new(X, Y, W, H)
}

/// The width the label may take.
pub fn label_budget() -> f32 {
    W - 2.0 * PAD_X
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

/// The capsule and its label at `alpha` (the tuck: 0 draws nothing). `field` is the frame's
/// latched underlay field when it has one (see the module doc); `ground` is the page's ambient
/// ground colour for the flat form.
pub fn draw(p: Painter, label: &CStr, ground: [f32; 3], field: Option<&UnderlayField>, alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let p = p.alpha(alpha);
    let r = rect();
    if !popover_glass::draw(p, r, RADIUS, field) {
        let fill = tint(ground);
        p.rect_rimmed(r, RADIUS, fill, fill, theme::GLASS_RIM, 0.0);
    }
    Label::new(label.as_ptr(), theme::size::BODY, theme::TEXT_PRIMARY)
        .bold()
        .h(HAlign::Center)
        .draw(p, r);
}

#[cfg(test)]
#[path = "scroll_capsule_tests.rs"]
mod tests;
