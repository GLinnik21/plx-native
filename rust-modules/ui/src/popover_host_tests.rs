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
