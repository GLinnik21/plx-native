//! The popover panel's GLASS — the look every menu shares (design boards `WatchlistOptions`,
//! `HoldMenu`, `WatchlistHold`): a gradient face over the captured host page, a 1 px rim, a top
//! highlight on the rim, and a soft shadow. The 30% scrim behind it is the compact-menu dim role
//! (`theme::underlay::DIM_COMPACT`), not a number stated here.
//!
//! **One entry point.** [`draw`] is called by [`widgets::panel_ground`](crate::widgets::panel_ground)
//! for a [`Ground::Menu`](crate::widgets::Ground) only (menus, not sheets or alerts), so no screen
//! restates any of this. It
//! reports `false` — and the caller draws the latched-underlay ground it always did — whenever no
//! live backdrop exists: a unit test with no frame, a surface over the hardware video plane, a
//! frozen page outside [`host::live`](crate::popover::host::live).
//!
//! **The refraction is the shader's, not this module's.** The panel asks for [`GlassRim::Menu`]:
//! `fs_glass.frag` already bends the captured page around the rim using the SAME rounded-rect SDF
//! it rounds the panel with, and mixes in the unblurred grab at `tokens::MENU_SHARP`. That sharp
//! source is NOT free: `GlassRim::Menu` turns on the sharp band copy, one per menu glass draw,
//! re-grabbed every frame (and with a new rect every frame while a morph moves). This module adds
//! no pass of its own; it only names the material.
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
    draw_shadowed(p, r, radius, 1.0)
}

/// [`draw`] with the drop shadow at `shadow` of the painter's opacity: a morph lays its glass at
/// full strength under the opener's fading plate, and its shadow arrives with the fill.
#[must_use]
pub fn draw_shadowed(p: Painter, r: Rect, radius: f32, shadow: f32) -> bool {
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
    if shadow > 0.0 {
        p.alpha(shadow).shadow_outside(r, radius, theme::MENU_SHADOW_BLUR, theme::MENU_SHADOW_DY, theme::MENU_SHADOW);
    }
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

    /// Every paint `f` declares inside a backdrop DISCOVERY pass, in order.
    pub(crate) fn declared_all(f: impl FnOnce()) -> Vec<std::rc::Rc<crate::frame::backdrop::Paint>> {
        use crate::frame::backdrop::{discover, Sources};
        use std::{cell::RefCell, rc::Rc};
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            f();
        }
        let paints = sources.borrow().paints.clone();
        paints
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

    /// A menu's glass is declared where the dispatcher draws a menu: INSIDE a surface's layer
    /// (`dispatch.rs`'s surfaces loop, `backdrop::layer(Z::surface(index), ..)`), above the page
    /// and the modal scrim. That is the DS6 design, and the glass resolves there: `surface_with`
    /// requests its entry before the declare, and the entry's prefix is the page and the scrim
    /// beneath it. `declared_glass` above walks at the default `Z::PAGE`, which is why it never
    /// saw this. Red on the build that asserted "a glass command was declared at/above the
    /// surfaces band … would never resolve" on every frame a menu was up (a debug build's frame
    /// guard caught the panic and the menu never drew).
    #[test]
    fn a_menu_glass_inside_a_surface_resolves_against_the_page_and_the_scrim() {
        use crate::frame::backdrop::{discover, layer, Sources, Z};
        use std::{cell::RefCell, rc::Rc};
        let _g = plx_base::testlock::serial();
        let r = Rect::new(1200.0, 220.0, 320.0, 600.0);
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let p = Painter::root();
            // The page under the menu, then the modal scrim in the DIM band, as `draw_with` orders them.
            p.rect(Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.2, 0.2, 0.2, 1.0], [0.2, 0.2, 0.2, 1.0], 0.0);
            {
                let _dim = layer(Z::DIM, false);
                p.rect(Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.0, 0.0, 0.0, 0.3], [0.0, 0.0, 0.0, 0.3], 0.0);
            }
            let _surface = layer(Z::surface(0), false);
            assert!(draw(p, r, 24.0), "a live discovery pass takes the menu's glass");
        }
        let mut s = sources.borrow_mut();
        s.resolve();
        let (&z, entry) = s
            .entries
            .iter()
            .find(|(&z, _)| z >= Z::surface(0))
            .expect("the menu glass requested a backdrop entry in its surface's band");
        assert!(z < Z::surface(1), "the entry belongs to the menu's own surface");
        assert_eq!(entry.underlay.len(), 2, "its source is the page and the scrim under it");
        assert!(s.jobs().iter().any(|&(jz, _)| jz == z), "the entry is captured: it resolves");
    }

    /// One frame of a menu over a page and its modal dim, the order `dispatch.rs::draw_with` walks
    /// them in: the page, the dim (`ModalStack::draw_scrims` → `UnderlayField::draw` as
    /// `Role::Dim`) and, when `lift` is set, an opener lift drawn ABOVE the dim under the menu's
    /// footprint, then the menu's own glass in its surface band. Returns whether the menu's source
    /// owes a capture this frame (a job), and marks it captured, as a successful GPU job does.
    fn menu_frame(
        sources: &std::rc::Rc<std::cell::RefCell<crate::frame::backdrop::Sources>>,
        field: &crate::underlay::UnderlayField,
        alpha: f32,
        lift: bool,
    ) -> bool {
        use crate::frame::backdrop::{commit, discover, layer, Z};
        let r = Rect::new(1200.0, 220.0, 320.0, 600.0);
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let p = Painter::root();
            p.rect(Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.2, 0.3, 0.4, 1.0], [0.2, 0.3, 0.4, 1.0], 0.0);
            {
                let _dim = layer(Z::DIM, false);
                field.draw(p, Rect::FULL, crate::underlay::Role::Dim { weight: theme::underlay::TINT }, alpha);
                if lift {
                    p.rect(Rect::new(1180.0, 200.0, 80.0, 80.0), 8.0, [0.9; 4], [0.9; 4], 0.0);
                }
            }
            let _surface = layer(Z::surface(0), false);
            assert!(draw(p, r, 24.0));
        }
        let owed = {
            let mut s = sources.borrow_mut();
            s.resolve();
            s.jobs().iter().any(|&(z, _)| z >= Z::surface(0))
        };
        commit(sources);
        owed
    }

    /// **A menu's dim ramping in front of a still page re-blurs nothing.** The dim is linear in its
    /// alpha (`mix(page, ink, a)`) and its ink is the page's own smooth light, so the menu's glass
    /// filters the page ONCE and applies this frame's dim at composite — the PageDip precedent
    /// (`Layer::composite_alpha`), with the dim's ink as the ground. Red on the build that keyed
    /// the source on the dim's alpha: every frame of an appear (and of a close) re-ran the whole
    /// direct source pass, `blur.scene` replaying the page plus both taps — the Sort & Filter and
    /// account menus' GLASSDIFF on the television named `field` arg 15 (the tint's alpha) on 18
    /// of SFopen's 24 frames.
    #[test]
    fn a_ramping_dim_under_a_menu_captures_its_source_once() {
        let _g = plx_base::testlock::serial();
        let sources = std::rc::Rc::new(std::cell::RefCell::new(crate::frame::backdrop::Sources::default()));
        let flat = crate::underlay::UnderlayField::new();
        let mut composite = Vec::new();
        let owed: Vec<bool> = (0..12)
            .map(|i| {
                let a = 0.025 * i as f32;
                let o = menu_frame(&sources, &flat, a, false);
                let s = sources.borrow();
                let z = *s.entries.keys().find(|&&z| z >= crate::frame::backdrop::Z::surface(0)).unwrap();
                composite.push((a, s.composite_dim(z).map(|d| d.tint[3])));
                o
            })
            .collect();
        assert_eq!(owed.iter().filter(|&&o| o).count(), 1, "one capture for the whole ramp: {owed:?}");
        assert!(owed[0], "and it is the first frame's");
        // …and what the composite applies is THIS frame's dim, the scrim ink at this alpha.
        for (a, applied) in composite {
            assert_eq!(applied, Some(a), "the composite takes the frame's own alpha");
        }
        // The band's direct replay skips the dim it applies; the visible walk always draws it.
        let z = *sources.borrow().entries.keys().find(|&&z| z >= crate::frame::backdrop::Z::surface(0)).unwrap();
        {
            let _job = crate::frame::backdrop::enter(sources.clone(), z);
            assert!(crate::frame::backdrop::dim_deferred(), "the job's replay leaves the dim out");
        }
        {
            let _visible = crate::frame::backdrop::enter(sources.clone(), crate::frame::backdrop::Z::ALL);
            assert!(!crate::frame::backdrop::dim_deferred(), "the visible frame draws it");
        }
    }

    /// **Nor does the dim's FIELD being re-latched** under a standing menu. Every host re-capture
    /// re-reads the page's light into the field (`ModalUnderlay::sync` on a new `page_epoch`),
    /// which re-specifies its texture; the composite reads the field live, so the source does not
    /// have to be re-taken for it. Red on the build that recorded the texture's revision in the
    /// dim's arguments: the tab pulse after "Add to Watchlist" re-captured the host on each of its
    /// frames, and each one re-ran the menu's source pass (GLASSDIFF `field` arg 10, HWCNT 19.4M a
    /// frame for the pulse's 0.55 s against 9.2M after it).
    #[test]
    fn a_relatched_dim_field_under_a_menu_captures_its_source_once() {
        let _g = plx_base::testlock::serial();
        let tex = 9_731;
        plx_gfx::gfx::tex_ledger::specified(tex, 60, 32);
        let field = crate::underlay::UnderlayField::latched_for_test(tex);
        let sources = std::rc::Rc::new(std::cell::RefCell::new(crate::frame::backdrop::Sources::default()));
        let owed: Vec<bool> = (0..8)
            .map(|_| {
                let o = menu_frame(&sources, &field, theme::underlay::DIM_COMPACT, false);
                plx_gfx::gfx::tex_ledger::specified(tex, 60, 32); // the next frame's re-latch
                o
            })
            .collect();
        plx_gfx::gfx::tex_ledger::deleted(tex);
        assert_eq!(owed.iter().filter(|&&o| o).count(), 1, "one capture: {owed:?}");
    }

    /// **The guard: anything drawn ABOVE the dim under the menu keeps the dim in the source.** A
    /// composite-time dim would darken that content too (an opener lift is drawn over the dim so
    /// that it does NOT recede), so such a menu keys on the dim's real arguments, as before, and
    /// re-captures when its alpha moves.
    #[test]
    fn content_above_the_dim_keeps_the_dim_in_the_menus_source() {
        let _g = plx_base::testlock::serial();
        let sources = std::rc::Rc::new(std::cell::RefCell::new(crate::frame::backdrop::Sources::default()));
        let flat = crate::underlay::UnderlayField::new();
        let owed: Vec<bool> = (0..6).map(|i| menu_frame(&sources, &flat, 0.05 * i as f32, true)).collect();
        assert_eq!(owed, vec![true; 6], "every alpha step is a new source");
        let s = sources.borrow();
        let z = *s.entries.keys().find(|&&z| z >= crate::frame::backdrop::Z::surface(0)).unwrap();
        assert_eq!(s.composite_dim(z), None, "and the composite applies no second dim");
    }

    /// **A menu morphing out of its pill captures its source once, not once per frame of growth.**
    /// The glass rect grows from the pill to the panel over the morph; a source sized by the rect
    /// it was first asked for has to be re-taken every frame the rect leaves it. The morph knows
    /// its whole envelope (the pill and the settled panel) from the first frame, so the glass asks
    /// for that. Red on the build that requested only the frame's own rect: Sort & Filter's
    /// GLASSDIFF on the television named `grow` on 8 consecutive frames of every open. The
    /// fade+scale fallback (no opener rect) grows inside the panel and is held to the same.
    #[test]
    fn a_morphing_menu_captures_its_source_once_for_the_whole_growth() {
        use crate::frame::backdrop::{commit, discover, layer, Sources, Z};
        let _g = plx_base::testlock::serial();
        let sources = std::rc::Rc::new(std::cell::RefCell::new(Sources::default()));
        let flat = crate::underlay::UnderlayField::new();
        let pill = Rect::new(1700.0, 150.0, 180.0, 56.0);
        let panel = Rect::new(1420.0, 220.0, 472.0, 600.0);
        for from in [Some(pill), None] {
        let mut pop = crate::popover::Popover::new().morphing();
        pop.open_from(from);
        let mut owed = Vec::new();
        for _ in 0..90 {
            pop.update(1.0 / 60.0);
            sources.borrow_mut().begin(vec![]);
            {
                let _walk = discover(sources.clone());
                let p = Painter::root();
                p.rect(Rect::FULL, 0.0, [0.2, 0.3, 0.4, 1.0], [0.2, 0.3, 0.4, 1.0], 0.0);
                {
                    let _dim = layer(Z::DIM, false);
                    flat.draw(p, Rect::FULL, crate::underlay::Role::Dim { weight: theme::underlay::TINT }, 0.3);
                }
                let _surface = layer(Z::surface(0), false);
                let p = pop.content_painter(0.0);
                pop.panel(p, panel, 24.0, None, crate::widgets::Ground::Menu);
                // The opener's face and the rows are drawn over the glass (`OptionsMenu::draw`), so
                // their changing alpha is in no source.
                pop.morph_face(p, panel, 24.0, [0.9, 0.9, 0.9, 1.0], |p| p.rrect(pill, 4.0, 4.0, [0.1, 0.1, 0.1, 1.0]));
                pop.morph_rows(p, panel, 24.0, |p| p.rrect(Rect::new(1440.0, 260.0, 400.0, 60.0), 8.0, 8.0, [1.0; 4]));
            }
            let o = {
                let mut s = sources.borrow_mut();
                s.resolve();
                s.jobs().iter().any(|&(z, _)| z >= Z::surface(0))
            };
            commit(&sources);
            if sources.borrow().entries.keys().any(|&z| z >= Z::surface(0)) {
                owed.push(o);
            }
            if pop.appear_settled() {
                break;
            }
        }
        pop.close();
        assert!(owed.len() > 8, "the morph ran over several frames with a glass: {owed:?}");
        assert_eq!(owed.iter().filter(|&&o| o).count(), 1, "one capture for the whole morph from {from:?}: {owed:?}");
        sources.borrow_mut().clear();
        }
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
