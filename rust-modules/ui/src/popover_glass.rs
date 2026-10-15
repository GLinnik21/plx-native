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
use plx_gfx::gfx::{GlassFace, GlassRim};

/// The face gradient's top stop: [`theme::MENU_GLASS_TOP`], the board's `rgba(58,58,64,.78)`.
pub const FACE_TOP: [f32; 4] = theme::MENU_GLASS_TOP;
/// The face gradient's bottom stop: [`theme::MENU_GLASS_BOT`], the board's `rgba(40,40,46,.86)`.
pub const FACE_BOT: [f32; 4] = theme::MENU_GLASS_BOT;

/// What the glass wears over its backdrop at full strength: the gradient face, the 1 px perimeter
/// and the lit top edge — the board's `inset 0 0 0 1px` and `inset 0 1px 0`, as the shader draws
/// them in the surface's own coverage.
pub const FACE: GlassFace = GlassFace {
    scrim_top: FACE_TOP,
    scrim_bot: FACE_BOT,
    rim: theme::GLASS_RIM,
    rim_lit: theme::GLASS_RIM_LIGHT,
    rim_w: theme::CARD_SHEEN_W,
};

/// The face at opacity `a`. Only the tint rides the painter's alpha cascade; a [`GlassFace`] does
/// not, so a panel that fades (appear, close, a route change's page dip) fades its face by hand
/// the way the hold hint does — otherwise a ~.8-opaque gradient and a full rim stand over rows
/// that are still near 0, which is the empty glass frame the morph forbids.
pub(crate) fn face_at(a: f32) -> GlassFace {
    crate::widgets::chip_face(FACE, a.clamp(0.0, 1.0))
}

/// Draw the glass panel at `r` with corner `radius`, at the opacity and appear-slide the painter
/// carries ([`Painter::opacity`], [`Painter::rest_lift`]). `false` = no live backdrop, nothing drawn.
#[must_use]
pub fn draw(p: Painter, r: Rect, radius: f32) -> bool {
    let a = p.opacity();
    let glass = crate::widgets::Glass::DYNAMIC_BACKDROP;
    let (tint, rim, face, mat) =
        ([1.0, 1.0, 1.0, 1.0], GlassRim::Menu, face_at(a), theme::PANEL_MATERIAL);
    // A sliding panel's region keeps the slide margin and its snapshot is grabbed around the REST
    // rect (`backdrop_rising`), so the open/close slide does not re-run the blur chain per frame.
    let drew = match p.rest_lift() {
        Some(dy) => glass.backdrop_rising(p, r, dy, radius, tint, rim, face, mat),
        None => glass.backdrop(p, r, 0.0, radius, tint, rim, face, mat),
    };
    if !drew {
        return false;
    }
    // After the glass, not before: `shadow_outside` cuts the panel's own rounded shape out of the
    // penumbra, so a translucent face has no ink under it, and the grab the rim refracts is taken
    // before the shadow is down. The shadow rides the painter's cascade (alpha and slide) itself.
    p.shadow_outside(r, radius, theme::MENU_SHADOW_BLUR, theme::MENU_SHADOW_DY, theme::MENU_SHADOW);
    true
}

#[cfg(test)]
pub(crate) mod tests {
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

    /// Run `f` inside a backdrop DISCOVERY pass and return the glass commands it declared, in the
    /// order declared. `None` if the pass could not run live (then the test would prove nothing).
    pub(crate) fn declared_glass(f: impl FnOnce()) -> Vec<std::rc::Rc<crate::frame::backdrop::Paint>> {
        use crate::frame::backdrop::{discover, Sources};
        use std::{cell::RefCell, rc::Rc};
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            f();
        }
        let glass = sources.borrow().paints.iter().filter(|p| p.glass.is_some()).cloned().collect();
        glass
    }

    /// The glass the board's panel should declare at `r`, built straight from the pieces (no
    /// `draw`): the shared `Glass` door, the menu rim, the face at opacity `a`.
    pub(crate) fn expected(p: Painter, r: Rect, rad: f32, face: GlassFace) -> Vec<std::rc::Rc<crate::frame::backdrop::Paint>> {
        declared_glass(|| {
            let g = crate::widgets::Glass::DYNAMIC_BACKDROP;
            let t = [1.0, 1.0, 1.0, 1.0];
            let _ = match p.rest_lift() {
                Some(dy) => g.backdrop_rising(p, r, dy, rad, t, GlassRim::Menu, face, theme::PANEL_MATERIAL),
                None => g.backdrop(p, r, 0.0, rad, t, GlassRim::Menu, face, theme::PANEL_MATERIAL),
            };
        })
    }

    /// The face fades with the panel, through `draw` itself: the declared glass command at opacity
    /// `a` equals the one built from `face_at(a)` and differs from the unfaded `FACE` one. Red if
    /// `draw` hands `FACE` as is (the finding: a ~.8-opaque face over rows still near 0).
    #[test]
    fn draw_declares_the_face_at_the_panels_opacity() {
        let _g = plx_base::testlock::serial();
        let r = Rect::new(100.0, 100.0, 300.0, 200.0);
        for a in [0.1_f32, 0.5] {
            let p = Painter::root().alpha(a);
            let drawn = declared_glass(|| assert!(draw(p, r, 24.0), "a live discovery pass takes the glass"));
            assert_eq!(drawn.len(), 1, "one glass command per panel");
            assert!(drawn[0] == expected(p, r, 24.0, face_at(a))[0], "the face rides the opacity {a}");
            assert!(drawn[0] != expected(p, r, 24.0, FACE)[0], "an unfaded face is a different command");
        }
        let p = Painter::root();
        let full = declared_glass(|| assert!(draw(p, r, 24.0)));
        assert!(full[0] == expected(p, r, 24.0, FACE)[0], "at full opacity the face is FACE");
    }

    /// The tint arithmetic itself: scrim and rim alphas scale by the opacity.
    #[test]
    fn the_face_scales_every_alpha() {
        let f = face_at(0.1);
        assert!((f.scrim_top[3] - 0.78 * 0.1).abs() < 1e-6);
        assert!((f.scrim_bot[3] - 0.86 * 0.1).abs() < 1e-6);
        assert!((f.rim[3] - theme::GLASS_RIM[3] * 0.1).abs() < 1e-6);
        assert!((f.rim_lit[3] - theme::GLASS_RIM_LIGHT[3] * 0.1).abs() < 1e-6);
        assert_eq!(face_at(1.0).scrim_top, FACE.scrim_top);
        assert_eq!(face_at(0.0).rim_lit[3], 0.0);
    }

    /// The compact-menu dim is the board's 30% scrim.
    #[test]
    fn the_menu_scrim_is_thirty_percent() {
        assert_eq!(theme::underlay::DIM_COMPACT, 0.30);
    }
}
