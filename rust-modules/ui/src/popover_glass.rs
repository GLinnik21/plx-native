//! The popover panel's GLASS — the look every menu shares (design boards `WatchlistOptions`,
//! `HoldMenu`, `WatchlistHold`): a gradient face over the captured host page, a 1 px rim, a top
//! highlight on the rim, and a soft shadow. The 30% scrim behind it is the compact-menu dim role
//! (`theme::underlay::DIM_COMPACT`), not a number stated here.
//!
//! **One entry point.** [`draw`] is called by [`widgets::panel_ground`](crate::widgets::panel_ground),
//! the one door every popover panel's ground goes through, so no screen restates any of this. It
//! reports `false` — and the caller draws the latched-underlay ground it always did — whenever no
//! live backdrop exists: a unit test with no frame, a surface over the hardware video plane, a
//! frozen page outside [`host::live`](crate::popover::host::live).
//!
//! **The refraction is the shader's, not this module's.** The panel asks for [`GlassRim::Menu`]:
//! `fs_glass.frag` already bends the captured page around the rim using the SAME rounded-rect SDF
//! it rounds the panel with, and mixes in the unblurred grab at `tokens::MENU_SHARP`. There is no
//! extra pass here; this module only names the material.
//!
//! **Board against shipped code** (shipped code wins, recorded): the board's panel radius is 26 and
//! the design system's `--radius-panel` is 24, while the shipped panels state their own
//! (`theme::ALERT_PANEL_RAD` 32 and the menus' `PANEL_RAD`) — the radius is the caller's, as it
//! always was. The board's top highlight is white .24; the shipped `theme::GLASS_RIM_LIGHT` is .28
//! and stays. The board's `saturate(1.6)` has no counterpart in the blur chain and is not drawn.
use crate::{theme, Painter, Rect};
use plx_gfx::gfx::tokens::rgb8;
use plx_gfx::gfx::{GlassFace, GlassRim};

/// The face gradient's top stop: the board's `rgba(58,58,64,.78)`. A neutral a touch cooler and
/// lighter than the opaque `PANEL_TOP`, because it sits over a live blur rather than being its own
/// ground.
pub const FACE_TOP: [f32; 4] = theme::with_a(rgb8(58, 58, 64), 0.78);
/// The face gradient's bottom stop: the board's `rgba(40,40,46,.86)`.
pub const FACE_BOT: [f32; 4] = theme::with_a(rgb8(40, 40, 46), 0.86);
/// The panel's drop shadow: the board's `0 24px 60px rgba(0,0,0,.45)` — offset, blur and weight.
pub const SHADOW_DY: f32 = 24.0;
/// See [`SHADOW_DY`].
pub const SHADOW_BLUR: f32 = 60.0;
/// See [`SHADOW_DY`]; the ink is the card shadow's black, only the weight differs.
pub const SHADOW: [f32; 4] = theme::with_a(theme::CARD_SHADOW, 0.45);

/// What the glass wears over its backdrop: the gradient face, the 1 px perimeter and the lit top
/// edge — the board's `inset 0 0 0 1px` and `inset 0 1px 0`, as the shader draws them in the
/// surface's own coverage.
pub const FACE: GlassFace = GlassFace {
    scrim_top: FACE_TOP,
    scrim_bot: FACE_BOT,
    rim: theme::GLASS_RIM,
    rim_lit: theme::GLASS_RIM_LIGHT,
    rim_w: theme::CARD_SHEEN_W,
};

/// Draw the glass panel at `r` with corner `radius`. `false` = no live backdrop, nothing drawn.
#[must_use]
pub fn draw(p: Painter, r: Rect, radius: f32) -> bool {
    if !crate::widgets::Glass::DYNAMIC_BACKDROP.backdrop(
        p,
        r,
        0.0,
        radius,
        [1.0, 1.0, 1.0, 1.0],
        GlassRim::Menu,
        FACE,
        theme::PANEL_MATERIAL,
    ) {
        return false;
    }
    // After the glass, not before: `shadow_outside` cuts the panel's own rounded shape out of the
    // penumbra, so a translucent face has no ink under it, and the grab the rim refracts is taken
    // before the shadow is down.
    p.shadow_outside(r, radius, SHADOW_BLUR, SHADOW_DY, SHADOW);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_face_is_the_boards_gradient() {
        assert_eq!(FACE_TOP[3], 0.78);
        assert_eq!(FACE_BOT[3], 0.86);
        assert!(FACE_TOP[0] > FACE_BOT[0], "lighter at the top, as the board's gradient runs");
    }

    #[test]
    fn the_rim_and_highlight_are_the_shipped_glass_tokens() {
        assert_eq!(FACE.rim, theme::GLASS_RIM);
        assert_eq!(FACE.rim_lit, theme::GLASS_RIM_LIGHT);
        assert_eq!(FACE.rim_w, 1.0, "the board's 1 px");
    }

    /// The compact-menu dim is the board's 30% scrim.
    #[test]
    fn the_menu_scrim_is_thirty_percent() {
        assert_eq!(theme::underlay::DIM_COMPACT, 0.30);
    }
}
