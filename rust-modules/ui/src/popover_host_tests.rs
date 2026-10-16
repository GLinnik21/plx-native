// Exercise the production host protocol with a CPU framebuffer standing in for GL copies.
// No host policy is mocked: begin_frame/page_pass/live/ground_drawn and gfx::culled are real.
// A copy replaces the framebuffer; a primitive appends ink unless the real freeze gate refuses it.
use super::mock::{FrameCache, PIXELS};
use super::*;

struct Reset {
    users: u32,
    frozen: bool,
}
impl Reset {
    fn new(cache_off: bool) -> Self {
        let users = HOST_USERS.swap(1, Relaxed);
        let frozen = plx_gfx::gfx::set_page_frozen(false);
        unsafe {
            CACHE = FrameCache { snapshot: None, off: cache_off };
            HELD = Held::Nothing;
        }
        CAPTURE_OWED.store(false, Relaxed);
        CAPTURE_POINTLESS.store(false, Relaxed);
        GROUND_DRAWN.store(false, Relaxed);
        Self { users, frozen }
    }
}
impl Drop for Reset {
    fn drop(&mut self) {
        invalidate();
        HOST_USERS.store(self.users, Relaxed);
        plx_gfx::gfx::set_page_frozen(self.frozen);
        CAPTURE_OWED.store(false, Relaxed);
        CAPTURE_POINTLESS.store(false, Relaxed);
        GROUND_DRAWN.store(false, Relaxed);
    }
}

fn ink(label: &'static str) {
    if !plx_gfx::gfx::culled(0.0, 0.0, 1920.0, 1080.0) {
        PIXELS.with(|p| p.borrow_mut().push(label));
    }
}

fn embedded_alert_frame(settled: bool, later_scope: bool) -> Vec<&'static str> {
    begin_frame(false);
    // BUFFER_DESTROYED: every present starts with no usable prior framebuffer contents.
    PIXELS.with(|p| p.borrow_mut().clear());
    {
        let _page = page_pass();
        ink("page");
        {
            let _scrim = live();
            ink("scrim");
        }
        {
            let _alert = live();
            ink("glass");
            ground_drawn(settled);
            ink("title/body/buttons");
        }
        // Dispatcher::draw_with enters its modal-scrim scope AFTER the page has drawn the
        // embedded DecisionAlert, even when the modal stack has no surfaces.
        if later_scope {
            let _container_scrims = live();
        }
    }
    assert!(!plx_gfx::gfx::page_frozen(), "the page scope must restore its caller");
    PIXELS.with(|p| p.borrow().clone())
}

#[test]
fn captured_ground_cannot_overwrite_an_embedded_alerts_foreground() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let expected = vec!["page", "scrim", "glass", "title/body/buttons"];
    assert_eq!(embedded_alert_frame(false, true), expected, "opening frame");
    assert_eq!(embedded_alert_frame(true, true), expected, "first settled frame");
    assert_eq!(embedded_alert_frame(true, true), expected, "cached input frame");
    plx_machine::idle::invalidate();
    assert_eq!(embedded_alert_frame(true, true), expected, "host damage recapture");
}

#[test]
fn cache_off_and_no_later_scope_explain_the_old_green_paths() {
    let _serial = plx_base::testlock::serial();
    let expected = vec!["page", "scrim", "glass", "title/body/buttons"];
    {
        let _reset = Reset::new(true);
        assert_eq!(embedded_alert_frame(true, true), expected, "cache-off simulator");
    }
    {
        let _reset = Reset::new(false);
        assert_eq!(embedded_alert_frame(true, false), expected, "alert drawn last");
    }
}

#[test]
fn reconciling_card_content_invalidates_its_cached_ground_only_when_changed() {
    use crate::decision_alert::{Answers, DecisionAlert};
    let _serial = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let mut alert = DecisionAlert::new();
    alert.open_card(c"Details", vec!["support".into()], Answers::One);
    embedded_alert_frame(true, true);
    assert!(matches!(held(), Held::Ground(_)));
    assert!(!alert.reconcile_card(c"Details", vec!["support".into()], Answers::One));
    assert!(matches!(held(), Held::Ground(_)), "unchanged content keeps its snapshot");
    assert!(alert.reconcile_card(c"Details", vec!["receipt".into(), "support".into()], Answers::Two));
    assert!(held() == Held::Nothing, "the taller card cannot reuse the old panel outline");
    assert_eq!(embedded_alert_frame(true, true), ["page", "scrim", "glass", "title/body/buttons"]);
}

// Moved here from `gfx.rs`'s tests (module-layers step L5): it drives the popover host's
// `begin_frame`, which the `gfx` layer may not name, to prove `gfx::dither_for_field` has no motion
// term to be reached through.
#[test]
fn a_field_keeps_its_dither_through_every_motion() {
    use plx_machine::idle::{frame_begin, note_spring, page_moving, present_moving, MotionScope};
    use crate::popover::host::begin_frame;
    let _g = plx_base::testlock::serial();
    frame_begin(1.0 / 60.0);
    begin_frame(false);
    assert_eq!(plx_gfx::gfx::dither_for_field(700.0, 700.0), plx_gfx::gfx::DITHER_LSB, "at rest, the field pays");

    // A POPOVER's spring: 100 units from its target, stepped inside its own scope, the way
    // `Popover::update` steps every appear spring. The frame is in motion — and the page is not.
    frame_begin(1.0 / 60.0);
    let scope = MotionScope::open();
    note_spring(0.0, 100.0, 0.0);
    assert!(scope.close(), "the scope saw the spring");
    assert!(present_moving() && !page_moving());
    begin_frame(false);
    assert_eq!(
        plx_gfx::gfx::dither_for_field(700.0, 700.0),
        plx_gfx::gfx::DITHER_LSB,
        "a field still pays in motion — a focus spring on Settings must not strip its ground"
    );

    // The page's UNSCOPED springs (Detail updates outside `scoped_motion`) and the SCOPED
    // verdict app.rs threads in (Home, the Library, Search, the press dip). Both are real page
    // motion, and neither may reach this decision.
    frame_begin(1.0 / 60.0);
    note_spring(0.0, 100.0, 0.0);
    assert!(page_moving());
    begin_frame(true);
    assert_eq!(plx_gfx::gfx::dither_for_field(700.0, 700.0), plx_gfx::gfx::DITHER_LSB, "page motion is not a field's business");
}

/// A caller's own panel radius, deliberately not the board's 22: the morph must END on it.
const RAD: f32 = 26.0;

/// DS7: the morph open style on a real `Popover`, beside the unchanged fade+slide.
#[test]
fn a_morphing_popover_never_shows_glass_without_rows_and_settles_both_ways() {
    let _g = plx_base::testlock::serial();
    let (pill, panel) = (crate::Rect::new(1500.0, 40.0, 196.0, 38.0), crate::Rect::new(1380.0, 40.0, 316.0, 400.0));
    let mut pop = crate::popover::Popover::new().morphing();
    assert_eq!(pop.open_style(), crate::popover::OpenStyle::Morph);
    pop.open();
    let mut seen = 0;
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        assert!(f.content_alpha >= f.ground_alpha, "rows lead the glass");
        seen += 1;
    }
    assert!(seen > 0 && pop.appear_settled(), "the open settles, morph included");
    let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
    assert!((f.radius - RAD).abs() < 0.05 && (f.rect.h - panel.h).abs() < 0.5, "the morph ends on the caller's radius");
    pop.dismiss();
    let mut last = f.rect.h;
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        if !pop.visible() {
            break;
        }
        let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        assert!(f.content_alpha >= f.ground_alpha, "rows lead the glass on the way out too");
        assert!(f.rect.h <= last + 1e-3, "the close only shrinks");
        last = f.rect.h;
    }
    assert!(!pop.visible(), "the close lands and the panel is gone");
    pop.close();
}

/// DS7: every popover that does not opt in keeps the shared fade+slide, byte for byte.
#[test]
fn the_default_popover_keeps_the_shared_fade_and_slide() {
    let _g = plx_base::testlock::serial();
    let mut pop = crate::popover::Popover::new();
    assert_eq!(pop.open_style(), crate::popover::OpenStyle::Appear);
    assert!(pop.morph_frame(None, crate::Rect::FULL, RAD).is_none());
    pop.open();
    pop.update(1.0 / 60.0);
    let mut reference = crate::Spring::at(0.0);
    reference.step(1.0, crate::popover::K_APPEAR, 1.0 / 60.0);
    assert_eq!(pop.appear(), reference.pos, "the appear spring is the crate::popover::K_APPEAR one");
    pop.close();
}

/// A morphing popover's `panel` draws the GROUND at the morphing rect, never ahead of its rows.
/// Graded on what `Popover::panel` really DECLARES (a discovery pass, the glass command), against
/// the command built from the morph frame's own rect, radius and alpha ratio.
#[test]
fn a_morphing_panel_grows_from_the_pill_and_its_ground_never_leads_its_rows() {
    use crate::popover_glass::{face_at, tests::{declared_glass, expected}};
    let _g = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let (pill, panel) = (crate::Rect::new(1500.0, 40.0, 196.0, 38.0), crate::Rect::new(1380.0, 40.0, 316.0, 400.0));
    let mut pop = crate::popover::Popover::new().morphing();
    pop.open_from(Some(pill));
    let root = crate::Painter::root();
    // Frame 0: the ground is not down yet (pill only) -- no empty glass frame.
    assert!(declared_glass(|| pop.panel(root, panel, RAD, None, crate::widgets::Ground::Menu)).is_empty());
    let (mut grew, mut painted) = (false, 0);
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        let declared = declared_glass(|| pop.panel(root, panel, RAD, None, crate::widgets::Ground::Menu));
        if f.ground_alpha <= 0.0 || f.content_alpha <= 0.0 {
            assert!(declared.is_empty(), "no ground before there are rows");
            continue;
        }
        assert_eq!(declared.len(), 1);
        painted += 1;
        let mul = (f.ground_alpha / f.content_alpha).min(1.0);
        assert!(mul <= 1.0 && f.ground_alpha <= f.content_alpha + 1e-6, "ground alpha <= content alpha");
        let want = |r, rad, face| expected(root.alpha(mul), r, rad, face);
        if f.rect == panel {
            assert!(declared[0] == want(panel, RAD, face_at(mul))[0], "settled: the caller's own rect");
        } else {
            let morph = declared[0] == want(f.rect, f.radius, face_at(mul))[0];
            // The morph hands over to the caller's own rect once it has settled (a sub-pixel
            // tolerance in `popover_morph`), so the last frames may legitimately be the settled one.
            let snapped = declared[0] == want(panel, RAD, face_at(mul))[0];
            if snapped {
                // The hand-over to the settled ground: no visible jump in rect OR radius.
                assert!((f.radius - RAD).abs() < 0.5 && (f.rect.h - panel.h).abs() < 1.0, "a snap at settle: {f:?}");
            }
            assert!(morph || (snapped && mul == 1.0 && (panel.h - f.rect.h).abs() < 8.0), "mid-open: the morph rect, radius and faded face");
            if !morph {
                continue;
            }
            assert!(declared[0] != want(panel, RAD, face_at(mul))[0], "not the settled rect");
            grew = true;
            assert!(f.rect.w <= panel.w + 1e-3 && f.rect.h <= panel.h + 1e-3 && f.rect.w >= pill.w - 1e-3);
        }
    }
    assert!(painted > 0 && grew, "mid-open the ground is the morphing rect, not the settled one");
    let settled = declared_glass(|| pop.panel(root, panel, RAD, None, crate::widgets::Ground::Menu));
    assert_eq!(settled.len(), 1, "settled: the glass stands");
    pop.close();
}

/// The appear slide is carried on the content painter so a glass ground can use `backdrop_rising`.
#[test]
fn a_sliding_panel_carries_its_rest_lift_and_a_morph_does_not() {
    let _g = plx_base::testlock::serial();
    let mut pop = crate::popover::Popover::new();
    pop.open();
    pop.update(1.0 / 60.0);
    let a = pop.appear();
    let lift = pop.content_painter(crate::popover::Popover::RISE).rest_lift().expect("sliding");
    assert!((lift - crate::popover::Popover::RISE * (1.0 - a)).abs() < 1e-4);
    assert!(pop.content_painter(0.0).rest_lift().is_none());
    pop.close();
    let mut m = crate::popover::Popover::new().morphing();
    m.open();
    m.update(1.0 / 60.0);
    assert!(m.content_painter(crate::popover::Popover::RISE).rest_lift().is_none());
    m.close();
}

/// Finding 1: the ground's radius is the caller's through the whole life of the panel. The old
/// morph ended on 22 and `Popover::ground` snapped to the caller's 26 at settle, and back to 22
/// when the close began; the largest per-frame step across those two seams is graded here.
#[test]
fn the_ground_radius_does_not_snap_at_settle_or_at_the_start_of_a_close() {
    let _g = plx_base::testlock::serial();
    let (pill, panel) = (crate::Rect::new(1500.0, 40.0, 196.0, 38.0), crate::Rect::new(1380.0, 40.0, 316.0, 400.0));
    let mut pop = crate::popover::Popover::new().morphing();
    pop.open_from(Some(pill));
    let (mut prev, mut seam) = (None::<f32>, None::<f32>);
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        let Some((_, rad, _)) = pop.ground(panel, RAD) else { continue };
        if pop.appear_settled() && seam.is_none() {
            let before = prev.expect("frames before the settle");
            seam = Some((rad - before).abs());
            assert_eq!(rad, RAD, "settled: the caller's radius");
        }
        prev = Some(rad);
    }
    let seam = seam.expect("the open settled");
    assert!(seam < 1.0, "the radius jumps {seam} px at settle");
    // The close starts on the caller's radius from BOTH sources (the settled ground and the morph's
    // own curve at full progress), then follows the morph's one curve: the ground has no second
    // radius of its own to snap to.
    pop.dismiss();
    let at_rest = pop.morph_frame(Some(pill), panel, RAD).unwrap().radius;
    assert!((at_rest - RAD).abs() < 0.05, "the morph's curve starts the close on {at_rest}, not the caller's {RAD}");
    pop.update(1.0 / 60.0);
    let (_, first_close, _) = pop.ground(panel, RAD).expect("the close still has a ground");
    assert_eq!(first_close, pop.morph_frame(Some(pill), panel, RAD).unwrap().radius, "one curve");
    pop.close();
}

/// Finding 2: the rows of an unsettled morph are clipped to the CURRENT glass rect and scaled about
/// the panel anchor by the row spring; settled (and the fade+slide) they draw through `p` untouched.
#[test]
fn morph_rows_are_clipped_to_the_glass_rect_and_scaled_about_the_anchor() {
    use crate::{popover_morph::rows_anchor, screen::ClipScope, Painter, Rect};
    let _g = plx_base::testlock::serial();
    let (pill, panel) = (Rect::new(1500.0, 40.0, 196.0, 38.0), Rect::new(1380.0, 40.0, 316.0, 400.0));
    let row = Rect::new(panel.x + 10.0, panel.y + 100.0, panel.w - 20.0, 60.0);
    let mut pop = crate::popover::Popover::new().morphing();
    pop.open_from(Some(pill));
    let root = Painter::root();
    let mut mid = 0;
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        let mut drew = false;
        pop.morph_rows(root, panel, RAD, |p| {
            drew = true;
            if p.clip_rect() == Rect::FULL {
                // The morph has settled (a visual-arrival test, within 0.1% of rest): untouched rows.
                assert!((f.rect.w - panel.w).abs() < 1.0 && (f.rows_scale - 1.0).abs() < 0.01, "only a settled morph is unclipped: {f:?}");
                assert_eq!(ClipScope::current(), None);
                assert_eq!(p.place(row), row, "settled: rows at their final layout");
                return;
            }
            mid += 1;
            let same = |a: Rect, b: Rect| (a.x - b.x).abs() + (a.y - b.y).abs() + (a.w - b.w).abs() + (a.h - b.h).abs() < 1e-2;
            assert!(same(p.clip_rect(), f.rect), "the cascade clip is the glass rect");
            assert!(same(ClipScope::current().unwrap(), f.rect), "the scissor is the glass rect");
            let placed = p.place(row);
            assert!((placed.w - row.w * f.rows_scale).abs() < 1e-3 && (placed.h - row.h * f.rows_scale).abs() < 1e-3, "scaled by the row spring");
            let (ax, ay) = rows_anchor(Some(pill), panel);
            let fixed = p.place(Rect::new(ax, ay, 1.0, 1.0));
            assert!((fixed.x - ax).abs() < 1e-3 && (fixed.y - ay).abs() < 1e-3, "the anchor does not move");
        });
        assert!(drew, "the rows are always drawn");
        assert_eq!(ClipScope::current(), None, "the scissor is released");
    }
    assert!(mid > 5, "the morph ran through unsettled frames");
    // The fade+slide popover is never clipped or scaled.
    let mut plain = crate::popover::Popover::new();
    plain.open();
    plain.update(1.0 / 60.0);
    plain.morph_rows(root, panel, RAD, |p| {
        assert_eq!(p.clip_rect(), Rect::FULL);
        assert_eq!(p.place(row), row);
    });
    plain.close();
    pop.close();
}

/// Finding 2: a glass rect larger than the pill always has rows on it (open and close), and the
/// pill's label dissolves out while they come in and back in while they go.
#[test]
fn a_grown_glass_rect_always_has_rows_and_the_pill_cross_dissolves() {
    let _g = plx_base::testlock::serial();
    let (pill, panel) = (crate::Rect::new(1500.0, 40.0, 196.0, 38.0), crate::Rect::new(1380.0, 40.0, 316.0, 400.0));
    let root = crate::Painter::root();
    let mut pop = crate::popover::Popover::new().morphing();
    pop.open_from(Some(pill));
    assert_eq!(pop.pill_painter(root).opacity(), 1.0, "frame 0: the full pill label");
    let (mut last_pill, mut grown) = (1.0_f32, 0);
    let check = |pop: &crate::popover::Popover, opening: bool, last: &mut f32| {
        let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        let larger = f.rect.w > pill.w + 1.0 || f.rect.h > pill.h + 1.0;
        if larger && f.ground_alpha > 0.0 {
            assert!(pop.content_painter(0.0).opacity() > 0.0, "an empty glass frame: {f:?}");
        }
        let pa = pop.pill_painter(root).opacity();
        assert!((pa - (1.0 - f.ground_alpha)).abs() < 1e-6, "the pill is the ground's complement");
        // 1e-2: the underdamped row spring rebounds a hair past its start after the close lands
        // (ground alpha ~3e-4 there, invisible), which is not a label dissolving the wrong way.
        assert!(if opening { pa <= *last + 1e-2 } else { pa >= *last - 1e-2 }, "the label only dissolves one way: opening {opening}, {pa} after {last}");
        *last = pa;
        larger
    };
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        grown += check(&pop, true, &mut last_pill) as u32;
    }
    assert!(grown > 5 && last_pill == 0.0, "the label is gone once the rows are in");
    pop.dismiss();
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        if !pop.visible() {
            break;
        }
        check(&pop, false, &mut last_pill);
    }
    pop.close();
    let plain = crate::popover::Popover::new();
    assert_eq!(plain.pill_painter(root).opacity(), 1.0, "a non-morphing popover leaves the pill alone");
}
