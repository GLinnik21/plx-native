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

/// DS7: the morph open style on a real `Popover`, beside the unchanged fade+slide. The pill and
/// the panel are one surface: on every frame of the open and the close the opener's plate and the
/// glass together cover it, and a row is only drawn once the fill is all glass.
#[test]
fn a_morphing_popover_is_one_surface_and_settles_both_ways() {
    let _g = plx_base::testlock::serial();
    let (pill, panel) = (crate::Rect::new(1500.0, 40.0, 196.0, 38.0), crate::Rect::new(1380.0, 40.0, 316.0, 400.0));
    let mut pop = crate::popover::Popover::new().morphing();
    assert_eq!(pop.open_style(), crate::popover::OpenStyle::Morph);
    pop.open_from(Some(pill));
    let one_surface = |f: crate::popover_morph::Frame| {
        assert!((f.pill_alpha + f.ground_alpha - 1.0).abs() < 1e-6, "plate + glass cover the surface: {f:?}");
        if f.content_alpha > 0.0 {
            assert_eq!(f.ground_alpha, 1.0, "a row only on a surface that is all glass: {f:?}");
        }
    };
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        one_surface(pop.morph_frame(Some(pill), panel, RAD).unwrap());
    }
    assert!(pop.appear_settled(), "the open settles, morph included");
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
        one_surface(f);
        assert!(f.rect.h <= last + 1e-3, "the close only shrinks");
        last = f.rect.h;
    }
    assert!(!pop.visible(), "the close lands and the panel is gone");
    let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
    assert_eq!((f.rect, f.pill_alpha), (pill, 1.0), "and what is left is the pill");
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

/// A morphing popover's `panel` draws the GROUND at the morphing rect: nothing while the fill is
/// all the opener's, then the glass at FULL strength (the opener's plate fades off it), and the
/// settled panel's own rect at the end. Graded on what `Popover::panel` really DECLARES (a
/// discovery pass, the glass command), against the command built from the morph frame's own rect
/// and radius. Red on the build that faded the glass in under rows that led it: the glass command
/// there carried a part-faded face over the rows (frame dump, n181: rows outside a faint glass).
#[test]
fn a_morphing_panel_grows_from_the_pill_and_its_glass_is_whole_once_it_is_down() {
    use crate::popover_glass::{face_at, tests::{declared_glass, expected}};
    let _g = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let (pill, panel) = (crate::Rect::new(1500.0, 40.0, 196.0, 38.0), crate::Rect::new(1380.0, 40.0, 316.0, 400.0));
    let mut pop = crate::popover::Popover::new().morphing();
    pop.open_from(Some(pill));
    let root = crate::Painter::root();
    // Frame 0: the opener only, no glass yet.
    assert!(declared_glass(|| pop.panel(root, panel, RAD, None, crate::widgets::Ground::Menu)).is_empty());
    let (mut grew, mut painted) = (false, 0);
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        let declared = declared_glass(|| pop.panel(root, panel, RAD, None, crate::widgets::Ground::Menu));
        if f.ground_alpha <= 0.0 {
            assert!(declared.is_empty(), "no glass before the fill begins to cross");
            continue;
        }
        assert_eq!(declared.len(), 1);
        painted += 1;
        let want = |r, rad| expected(root, r, rad, face_at(1.0));
        if declared[0] == want(panel, RAD)[0] {
            // The hand-over to the settled ground: no visible jump in rect OR radius.
            assert!((f.radius - RAD).abs() < 0.5 && (f.rect.h - panel.h).abs() < 1.0, "a snap at settle: {f:?}");
            continue;
        }
        assert!(declared[0] == want(f.rect, f.radius)[0], "mid-open: the morph rect and radius, the face whole: {f:?}");
        grew = true;
        assert!(f.rect.w <= panel.w + 1e-3 && f.rect.h <= panel.h + 1e-3 && f.rect.w >= pill.w - 1e-3);
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
        let Some(crate::popover::MorphGround { radius: rad, .. }) = pop.ground(panel, RAD) else { continue };
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
    let first_close = pop.ground(panel, RAD).expect("the close still has a ground").radius;
    assert_eq!(first_close, pop.morph_frame(Some(pill), panel, RAD).unwrap().radius, "one curve");
    pop.close();
}

/// Spec (c): the rows of an unsettled morph are clipped to the CURRENT surface rect on every frame
/// they draw, scaled about the panel anchor by the row spring, at the content alpha, and not drawn
/// at all before the glass has taken over (but still walked by the text recorder, so their text is
/// resident in time); settled (and the fade+slide) they draw through `p` untouched. Red on the
/// build whose rows led the glass: rows drew at progress 0.05, on a surface that was still pill.
#[test]
fn morph_rows_are_clipped_to_the_surface_and_wait_for_the_glass() {
    use crate::{popover_morph::rows_anchor, screen::ClipScope, Painter, Rect};
    let _g = plx_base::testlock::serial();
    let (pill, panel) = (Rect::new(1500.0, 40.0, 196.0, 38.0), Rect::new(1380.0, 40.0, 316.0, 400.0));
    let row = Rect::new(panel.x + 10.0, panel.y + 100.0, panel.w - 20.0, 60.0);
    let mut pop = crate::popover::Popover::new().morphing();
    pop.open_from(Some(pill));
    let root = Painter::root();
    let mut drew_held = false;
    pop.morph_rows(Painter::recording(), panel, RAD, |_| drew_held = true);
    assert!(drew_held, "the recorder walks the rows even while they are invisible");
    let (mut mid, mut waited) = (0, 0);
    let mut check = |pop: &crate::popover::Popover| {
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
            assert!(f.content_alpha > 0.0 && f.ground_alpha == 1.0, "rows drawn on a surface that is not yet glass: {f:?}");
            assert!((p.opacity() - f.content_alpha).abs() < 1e-6, "at the content alpha");
            let same = |a: Rect, b: Rect| (a.x - b.x).abs() + (a.y - b.y).abs() + (a.w - b.w).abs() + (a.h - b.h).abs() < 1e-2;
            assert!(same(p.clip_rect(), f.rect), "the cascade clip is the surface rect");
            assert!(same(ClipScope::current().unwrap(), f.rect), "the scissor is the surface rect");
            let placed = p.place(row);
            assert!((placed.w - row.w * f.rows_scale).abs() < 1e-3 && (placed.h - row.h * f.rows_scale).abs() < 1e-3, "scaled by the row spring");
            let (ax, ay) = rows_anchor(Some(pill), panel);
            let fixed = p.place(Rect::new(ax, ay, 1.0, 1.0));
            assert!((fixed.x - ax).abs() < 1e-3 && (fixed.y - ay).abs() < 1e-3, "the anchor does not move");
        });
        if !drew {
            assert_eq!(f.content_alpha, 0.0, "only invisible rows are skipped: {f:?}");
            waited += 1;
        }
        assert_eq!(ClipScope::current(), None, "the scissor is released");
    };
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        check(&pop);
    }
    pop.dismiss();
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        if !pop.visible() {
            break;
        }
        check(&pop);
    }
    assert!(mid > 5 && waited >= 4, "the morph ran through unsettled frames ({mid}) and frames without rows ({waited})");
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

/// Spec (a), (b), (d): the opener's FACE is the surface's other half. It starts as the focused
/// pill exactly (the opener's own fill at its rect and radius, the label at full strength), fades
/// off the glass as the fill crosses, with its label clipped to the surface, and a close lands
/// back on it: it never fades out and never pops. Red on the build whose pill was its own draw
/// faded by the CONTAINER's appear (frame dump n325–352: the landed pill faded out, seven empty
/// frames, then the page's pill popped in at full white).
#[test]
fn the_opener_face_starts_and_ends_as_the_pill_and_crosses_to_the_glass() {
    use crate::{Painter, Rect};
    let _g = plx_base::testlock::serial();
    let (pill, panel) = (Rect::new(1500.0, 40.0, 196.0, 38.0), Rect::new(1380.0, 40.0, 316.0, 400.0));
    const FILL: [f32; 4] = [0.9, 0.9, 0.9, 1.0];
    let plate = |pop: &crate::popover::Popover| {
        let mut label = None;
        let paints = crate::popover_glass::tests::declared_all(|| {
            pop.morph_face(Painter::root(), panel, RAD, FILL, |p| label = Some((p.opacity(), p.clip_rect())));
        });
        let plates: Vec<[f32; 4]> = paints.iter().filter(|p| p.kind() == "rrect").map(|p| p.placed()).collect();
        (plates, label)
    };
    let mut pop = crate::popover::Popover::new().morphing();
    pop.open_from(Some(pill));
    let (plates, label) = plate(&pop);
    assert_eq!(plates, vec![[pill.x, pill.y, pill.w, pill.h]], "frame 0: the opener's plate at the pill");
    assert_eq!(label, Some((1.0, pill)), "frame 0: the label whole, inside the pill");
    let mut crossed = 0;
    let mut check = |pop: &crate::popover::Popover| {
        let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        let (plates, label) = plate(pop);
        if f.pill_alpha <= 0.0 {
            assert!(plates.is_empty(), "no plate once the fill is glass: {f:?}");
        } else {
            assert_eq!(plates, vec![[f.rect.x, f.rect.y, f.rect.w, f.rect.h]], "the plate is the surface: {f:?}");
        }
        if f.label_alpha <= 0.0 {
            assert!(label.is_none(), "no label once it has dissolved: {f:?}");
            return;
        }
        let (a, clip) = label.expect("the label outlasts the plate");
        assert!((a - f.label_alpha).abs() < 1e-6, "the label at its own alpha");
        let near = (clip.x - f.rect.x).abs() + (clip.y - f.rect.y).abs() + (clip.w - f.rect.w).abs() + (clip.h - f.rect.h).abs();
        assert!(near < 1e-2, "the label is clipped to the surface: {clip:?} vs {:?}", f.rect);
        if f.label_alpha < 1.0 {
            crossed += 1;
        }
    };
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        check(&pop);
    }
    pop.dismiss();
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        check(&pop);
        if !pop.visible() {
            break;
        }
    }
    assert!(crossed >= 4, "the fill crosses over frames, both ways ({crossed})");
    assert!(!pop.visible());
    let (plates, label) = plate(&pop);
    assert_eq!(plates, vec![[pill.x, pill.y, pill.w, pill.h]], "the close lands on the opener's plate");
    assert_eq!(label, Some((1.0, pill)), "…with its label whole: nothing left to pop in");
    pop.close();
    let plain = crate::popover::Popover::new();
    assert!(plate(&plain).0.is_empty(), "a popover that does not morph has no face");
}
