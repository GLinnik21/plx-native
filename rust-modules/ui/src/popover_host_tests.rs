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
        cache(|c| *c = FrameCache { snapshot: None, off: cache_off, prev: None });
        HELD.set(Held::Nothing);
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
/// the panel are one surface: a row is only drawn once the fill is all glass, and a close lands
/// back on the pill.
#[test]
fn a_morphing_popover_is_one_surface_and_settles_both_ways() {
    let _g = plx_base::testlock::serial();
    let (pill, panel) = (crate::Rect::new(1500.0, 40.0, 196.0, 38.0), crate::Rect::new(1380.0, 40.0, 316.0, 400.0));
    let mut pop = crate::popover::Popover::new().morphing();
    assert_eq!(pop.open_style(), crate::popover::OpenStyle::Morph);
    pop.open_from(Some(pill));
    let one_surface = |f: crate::popover_morph::Frame| {
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
    assert_eq!(f.rect, pill, "and what is left is the pill");
    pop.close();
}

/// **N-b: the glass leaves its opener as a ramp, never as an opaque slab.** The first glass frame of
/// an open (and the last of a close) used to be laid at FULL strength while the springs had already
/// pushed its rect ~25 px past the lit opener, so a ~165x62 dark slab hung under the pill for one
/// frame (5-menus/libsf-zoom n=155 / 247). The rule, over every header opener: whenever the glass
/// stands outside the opener's rect, its alpha AND its shadow are the morph's one fill ramp, so it is
/// never opaque on the frame it first appears or last shows.
#[test]
fn the_glass_leaves_every_header_opener_as_a_ramp_on_open_and_on_close() {
    let _g = plx_base::testlock::serial();
    let openers = [
        ("Library Sort & Filter", crate::Rect::new(1589.0, 146.0, 204.0, 60.0)),
        ("Collections +N", crate::Rect::new(1180.0, 146.0, 118.0, 60.0)),
        ("the libraries menu", crate::Rect::new(94.0, 146.0, 230.0, 60.0)),
        ("the account chip", crate::widgets::chip_cap(1.0, 60.0)),
        ("the account chip at rest, a circle", crate::widgets::chip_cap(0.0, 0.0)),
    ];
    for (name, pill) in openers {
        let panel = crate::Rect::new(pill.x, pill.y + pill.h + 14.0, 316.0, 400.0);
        let outside = |r: crate::Rect| r.x < pill.x - 1.0 || r.y < pill.y - 1.0 || r.x + r.w > pill.x + pill.w + 1.0 || r.y + r.h > pill.y + pill.h + 1.0;
        let check = |pop: &crate::popover::Popover, what: &str, frame: usize, shown: &mut Vec<f32>| {
            let Some(g) = pop.ground(panel, RAD) else { return };
            let fill = crate::popover_morph::fill_at(pop.morph_frame(Some(pill), panel, RAD).unwrap().progress);
            shown.push(g.alpha);
            if outside(g.rect) {
                assert!(g.alpha <= fill + 1e-4 && g.shadow <= fill + 1e-4,
                    "{name} {what} frame {frame}: glass {} / shadow {} past the opener outruns the fill ramp {fill}", g.alpha, g.shadow);
            }
        };
        let mut pop = crate::popover::Popover::new().morphing();
        pop.open_from(Some(pill));
        // the morph begins ON the opener, in its own corner: the lit opener's rect and its round
        // surround (half its height), so the glass first stands exactly under the control
        let begin = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        assert_eq!((begin.rect, begin.radius), (pill, pill.h * 0.5), "{name}: the first frame is the opener's rect and corner");
        let mut opening = Vec::new();
        for n in 0..240 {
            pop.update(1.0 / 60.0);
            check(&pop, "open", n, &mut opening);
        }
        assert!(opening.first().is_some_and(|a| *a < 1.0), "{name}: the first glass frame is not opaque: {opening:?}");
        assert!(opening.windows(2).all(|w| w[1] >= w[0] - 1e-4), "{name}: the glass only strengthens while opening");
        pop.dismiss();
        let mut closing = Vec::new();
        for n in 0..240 {
            pop.update(1.0 / 60.0);
            if !pop.visible() {
                break;
            }
            check(&pop, "close", n, &mut closing);
        }
        assert!(closing.last().is_some_and(|a| *a < 1.0), "{name}: the last glass frame is not opaque: {closing:?}");
        let end = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        assert_eq!((end.rect, end.radius), (pill, pill.h * 0.5), "{name}: the close lands on the opener's rect and corner");
        assert!(closing.windows(2).all(|w| w[1] <= w[0] + 1e-4), "{name}: the glass only weakens while closing");
        pop.close();
    }
}

/// **NV1: a panel's near edge is its opener's outer edge**, one rule for every header menu: a control
/// in the right half hangs the panel from its right edge, one in the left half from its left, and
/// the safe frame only ever pulls a panel back, never off the opener's edge otherwise.
#[test]
fn a_panel_hangs_from_its_openers_outer_edge() {
    let keep = crate::consts::SAFE;
    let right = crate::Rect::new(1589.0, 146.0, 204.0, 60.0);
    let left = crate::Rect::new(120.0, 146.0, 230.0, 60.0);
    assert_eq!(crate::popover::hang_x(right, 316.0, keep) + 316.0, right.x + right.w, "right control: right edges flush");
    assert_eq!(crate::popover::hang_x(left, 316.0, keep), left.x, "left control: left edges flush");
    assert_eq!(crate::popover::hang_x(right, right.w, keep), right.x, "a panel as wide as its pill is the pill's own span");
    let at_edge = crate::Rect::new(keep.x - 2.0, 146.0, 100.0, 60.0);
    assert_eq!(crate::popover::hang_x(at_edge, 300.0, keep), keep.x, "held inside the safe frame");
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
/// at the first frame, then the glass at FULL strength (over the lit opener), and the
/// settled panel's own rect at the end. Graded on what `Popover::panel` really DECLARES (a
/// discovery pass, the glass command), against the command built from the morph frame's own rect
/// and radius. Red on the build that faded the glass in under rows that led it: the glass command
/// there carried a part-faded face over the rows (frame dump, n181: rows outside a faint glass).
#[test]
fn a_morphing_panel_grows_from_the_pill_and_its_glass_is_whole_once_it_is_down() {
    use crate::popover_glass::{face_at, tests::{declared_glass, expected, latched, unlatch}};
    let _g = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let tex = 9_751;
    let field = latched(tex);
    let sharp = page_tex().unwrap_or(0);
    let (pill, panel) = (crate::Rect::new(1500.0, 40.0, 196.0, 38.0), crate::Rect::new(1380.0, 40.0, 316.0, 400.0));
    let mut pop = crate::popover::Popover::new().morphing();
    pop.open_from(Some(pill));
    let root = crate::Painter::root();
    // Frame 0: the opener only, no glass yet.
    assert!(declared_glass(|| pop.panel(root, panel, RAD, Some(&field), crate::widgets::Ground::Menu)).is_empty());
    let (mut grew, mut painted) = (false, 0);
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        let declared = declared_glass(|| pop.panel(root, panel, RAD, Some(&field), crate::widgets::Ground::Menu));
        if f.ground_alpha <= 0.0 {
            assert!(declared.is_empty(), "no glass before the fill begins to cross");
            continue;
        }
        assert_eq!(declared.len(), 1);
        painted += 1;
        let want = |r, rad| expected(root, r, rad, tex, sharp, face_at(1.0));
        if declared[0] == want(panel, RAD)[0] {
            // The hand-over to the settled ground: no visible jump in rect OR radius.
            assert!((f.radius - RAD).abs() < 0.5 && (f.rect.h - panel.h).abs() < 1.0, "a snap at settle: {f:?}");
            continue;
        }
        // The glass rides the fill's one ramp, painter alpha and face together (N-b: no opaque slab
        // past the opener), and is whole once the fill is.
        let a = f.ground_alpha;
        let ramped = expected(root.alpha(a), f.rect, f.radius, tex, sharp, face_at(a));
        assert!(declared[0] == ramped[0], "mid-open: the morph rect and radius on the fill ramp {a}: {f:?}");
        grew = true;
        assert!(f.rect.w <= panel.w + 1e-3 && f.rect.h <= panel.h + 1e-3 && f.rect.w >= pill.w - 1e-3);
    }
    assert!(painted > 0 && grew, "mid-open the ground is the morphing rect, not the settled one");
    let settled = declared_glass(|| pop.panel(root, panel, RAD, Some(&field), crate::widgets::Ground::Menu));
    assert_eq!(settled.len(), 1, "settled: the glass stands");
    pop.close();
    unlatch(tex);
}

/// **A menu's glass stands on ONE capture of its host while it opens and stays, and one more for
/// its whole close.** Frames of a menu over a still page, through the real host protocol: the
/// snapshot is taken once ([`page_epoch`] moves once) for the open and the stay, however its own
/// springs move; the dismiss drops it (`Popover::release`) and the close's first frame takes it
/// again, after which the fade re-takes nothing. The glass's rim reads the snapshot's texture
/// (the CPU stand-in's).
#[test]
fn a_menu_glass_reads_one_host_capture_through_open_stay_and_close() {
    use crate::popover_glass::tests::{declared_glass, expected, latched, unlatch};
    let _g = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let tex = 9_752;
    let field = latched(tex);
    let panel = crate::Rect::new(1380.0, 40.0, 316.0, 400.0);
    let epoch = page_epoch();
    let mut pop = crate::popover::Popover::new().morphing();
    pop.open_from(Some(crate::Rect::new(1500.0, 40.0, 196.0, 38.0)));
    let (mut frames, mut drawn, mut closing, mut closed_at) = (0, 0, false, 0);
    while pop.visible() {
        pop.update(1.0 / 60.0);
        begin_frame(false);
        PIXELS.with(|p| p.borrow_mut().clear());
        {
            let _page = page_pass();
            ink("page");
            let _scrim = live();
            ink("scrim");
        }
        let sharp = page_tex().expect("the host holds its undimmed page");
        let glass = declared_glass(|| pop.panel(crate::Painter::root(), panel, RAD, Some(&field), crate::widgets::Ground::Menu));
        assert!(glass.len() <= 1);
        drawn += glass.len();
        frames += 1;
        // Each frame of the close after the first re-takes nothing (the first: below).
        if closing && frames > closed_at + 1 {
            assert_eq!(page_epoch().wrapping_sub(epoch), 2, "frame {frames} of the close re-took the host");
        }
        if !closing && pop.appear_settled() {
            assert_eq!(page_epoch().wrapping_sub(epoch), 1, "one host capture for the open and the stay");
            closed_at = frames;
            // The settled panel's glass, against one built with this capture's texture as its
            // rim's source and against one with none.
            let want = |sharp| expected(crate::Painter::root(), panel, RAD, tex, sharp, crate::popover_glass::FACE);
            assert!(sharp != 0 && glass[0] == want(sharp)[0], "the rim reads the host snapshot");
            assert!(glass[0] != want(0)[0]);
            // The close key, as `app.rs` delivers it: inside the panel's input scope, so its
            // invalidate is the panel's damage, not the page's.
            let _input = input_scope();
            pop.dismiss();
            closing = true;
        }
        assert!(frames < 600);
    }
    assert!(closing && drawn > 0, "the glass drew: {drawn}");
    // `Popover::release` drops the snapshot on the dismiss itself (a ground-stage snapshot holds
    // the panel), so the close's first frame takes the page once more; its fade takes nothing.
    assert_eq!(page_epoch().wrapping_sub(epoch), 2, "one capture for open and stay, one at the dismiss, over {frames} frames");
    unlatch(tex);
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

/// One frame of a surface over a page, as `embedded_alert_frame` draws it, with the page's ink
/// named so a test sees WHICH page was drawn. The page raises damage (it is moving).
fn reflow_frame(page: &'static str, moving: bool) -> Vec<&'static str> {
    if moving {
        plx_machine::idle::invalidate();
    }
    begin_frame(false);
    PIXELS.with(|p| p.borrow_mut().clear());
    {
        let _page = page_pass();
        ink(page);
        {
            let _scrim = live();
            ink("scrim");
        }
        {
            let _panel = live();
            ink("glass");
            ground_drawn(true);
            ink("foreground");
        }
    }
    PIXELS.with(|p| p.borrow().clone())
}

fn counts() -> (u32, u32, u32, u32, usize) {
    (
        page_epoch(),
        mock::CAPTURES.with(|c| c.get()),
        mock::PREV_COPIES.with(|c| c.get()),
        mock::PREV_RELEASES.with(|c| c.get()),
        mock::PREV_DRAWS.with(|d| d.borrow().len()),
    )
}

/// **A page that reflows under a glass menu is drawn live, and the host is retaken ONCE when it
/// stops.** Open and settle (one page capture, one ground capture), then ten frames of a moving
/// page: each shows its own page ink (live, not the held picture), `page_epoch` does not move and
/// nothing is captured per frame. The reflow's end costs the one old-look copy on its exit frame
/// (still drawn live), then on the next frame exactly one page capture (epoch + 1) and one
/// ground capture. What this counts: `page_epoch` (the page stage), `CAPTURES` (every snapshot
/// into the main slot, page or ground), `PREV_COPIES` (the old-side copy, counted apart),
/// `PREV_RELEASES` (the old copy given back).
#[test]
fn a_reflowing_page_is_live_under_the_glass_and_the_host_is_retaken_once() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    reset_reflow();
    mock::CAPTURES.with(|c| c.set(0));
    mock::PREV_COPIES.with(|c| c.set(0));
    mock::PREV_RELEASES.with(|c| c.set(0));
    mock::PREV_DRAWS.with(|d| d.borrow_mut().clear());
    let dt = 1.0 / 60.0;
    // open: the page stage, then the settled ground
    for _ in 0..3 {
        reflow_frame("page-open", false);
    }
    assert!(matches!(held(), Held::Ground(_)), "premise: the ground is held");
    let (epoch, caps, ..) = counts();
    assert_eq!(caps, 2, "premise: one page capture and one ground capture to open");

    set_reflow(true, dt);
    for k in 0..10 {
        let name: &'static str = ["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9"][k];
        assert_eq!(reflow_frame(name, true), vec![name, "scrim", "glass", "foreground"], "frame {k}: the live page, under the glass");
        set_reflow(true, dt);
        let (e, c, ..) = counts();
        assert_eq!((e, c), (epoch, caps), "frame {k}: nothing is captured while the page moves");
    }

    // it stops: the exit frame is still live and keeps the old look
    set_reflow(false, dt);
    assert_eq!(reflow_frame("r-final", false), vec!["r-final", "scrim", "glass", "foreground"], "the exit frame is live");
    let (e, c, copies, released, _) = counts();
    assert_eq!((e, c, copies, released), (epoch, caps, 1, 0), "the exit frame copies the old look, once, and captures nothing");

    // the next frame takes the host once: page (epoch + 1) and ground (one more snapshot)
    set_reflow(false, dt);
    reflow_frame("r-final", false);
    let (e, c, copies, released, draws) = counts();
    assert_eq!(e, epoch + 1, "page_epoch moved exactly once for the whole reflow");
    assert_eq!(c, caps + 2, "one page capture and one ground capture, nothing per frame");
    assert_eq!((copies, released, draws), (1, 0, 1), "the old look is kept and drawn over the new ground");
    assert_eq!(crossfade(), Some(0.0), "the blend starts at 0: all old look");

    // the blend runs to 1 over about 150 ms of ticks, drawing the old look at 1 - b
    let mut frames = 0;
    while crossfade().is_some() && frames < 30 {
        set_reflow(false, dt);
        reflow_frame("r-final", false);
        frames += 1;
    }
    assert!(crossfade().is_none(), "the blend finished");
    assert!((8..=11).contains(&frames), "about 150 ms at 60 fps, got {frames} frames");
    let drawn: Vec<f32> = mock::PREV_DRAWS.with(|d| d.borrow().clone());
    assert!(drawn.windows(2).all(|w| w[1] < w[0]), "the old look fades monotonically: {drawn:?}");
    assert_eq!(drawn[0], 1.0);
    let (e, c, copies, released, _) = counts();
    assert_eq!((e, c, copies, released), (epoch + 1, caps + 2, 1, 1), "still one retake in all; the old texture is released at b = 1");
    assert!(!cache(|c| c.has_prev()), "and the cache keeps no second slot");
    let after = counts().4;
    set_reflow(false, dt);
    reflow_frame("r-final", false);
    assert_eq!(counts().4, after, "no old-look quad once it is released");
}
