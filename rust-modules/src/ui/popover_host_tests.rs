// Exercise the production host protocol with a CPU framebuffer standing in for GL copies.
// No host policy is mocked: begin_frame/page_pass/live/ground_drawn and gfx::culled are real.
// A copy replaces the framebuffer; a primitive appends ink unless the real freeze gate refuses it.
use super::*;
use std::cell::RefCell;

thread_local! {
    static PIXELS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

pub(super) struct FrameCache {
    snapshot: Option<Vec<&'static str>>,
    off: bool,
}

impl FrameCache {
    pub(super) const fn new() -> Self {
        Self { snapshot: None, off: false }
    }
    pub(super) fn invalidate(&mut self) {
        self.snapshot = None;
    }
    /// Host logic tests construct dispatchers without a GL context, so they always take the live
    /// fallback (`capture`/`draw`) rather than the FBO-render path `render_into` stands for — see
    /// `gfx::FrameCache::render_available`'s doc, which this mirrors.
    pub(super) fn render_available(&self) -> bool {
        false
    }
    pub(super) fn tex(&self) -> Option<std::ffi::c_uint> {
        self.snapshot.is_some().then_some(1)
    }
    pub(super) fn resident_bytes(&self) -> usize {
        self.snapshot.as_ref().map_or(0, |s| s.len())
    }
    pub(super) fn capture(&mut self) -> bool {
        if self.off || crate::gfx::blur_source_pass() {
            return false;
        }
        self.snapshot = Some(PIXELS.with(|p| p.borrow().clone()));
        true
    }
    /// No GL context in a host test: this always declines, exactly as `render_available` says, so
    /// every caller falls back to the `capture`/`draw` copy path exercised by this file's tests.
    pub(super) fn render_into(&mut self) -> Option<crate::surface::PageTarget> {
        None
    }
    pub(super) fn rendered(&mut self, target: crate::surface::PageTarget) {
        self.finish_render(target);
        self.draw();
    }
    pub(super) fn finish_render(&mut self, target: crate::surface::PageTarget) {
        drop(target);
        self.snapshot = Some(PIXELS.with(|p| p.borrow().clone()));
    }
    pub(super) fn draw(&self) -> bool {
        self.draw_alpha(1.0)
    }
    pub(super) fn draw_alpha(&self, _alpha: f32) -> bool {
        let Some(snapshot) = &self.snapshot else { return false };
        // FrameCache's quad bypasses PAGE_FROZEN. It replaces the whole viewport.
        PIXELS.with(|p| *p.borrow_mut() = snapshot.clone());
        true
    }
}

struct Reset {
    users: u32,
    frozen: bool,
}
impl Reset {
    fn new(cache_off: bool) -> Self {
        let users = HOST_USERS.swap(1, Relaxed);
        let frozen = crate::gfx::set_page_frozen(false);
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
        crate::gfx::set_page_frozen(self.frozen);
        CAPTURE_OWED.store(false, Relaxed);
        CAPTURE_POINTLESS.store(false, Relaxed);
        GROUND_DRAWN.store(false, Relaxed);
    }
}

fn ink(label: &'static str) {
    if !crate::gfx::culled(0.0, 0.0, 1920.0, 1080.0) {
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
    assert!(!crate::gfx::page_frozen(), "the page scope must restore its caller");
    PIXELS.with(|p| p.borrow().clone())
}

#[test]
fn captured_ground_cannot_overwrite_an_embedded_alerts_foreground() {
    let _serial = crate::testlock::serial();
    let _reset = Reset::new(false);
    let expected = vec!["page", "scrim", "glass", "title/body/buttons"];
    assert_eq!(embedded_alert_frame(false, true), expected, "opening frame");
    assert_eq!(embedded_alert_frame(true, true), expected, "first settled frame");
    assert_eq!(embedded_alert_frame(true, true), expected, "cached input frame");
    crate::ui::idle::invalidate();
    assert_eq!(embedded_alert_frame(true, true), expected, "host damage recapture");
}

#[test]
fn cache_off_and_no_later_scope_explain_the_old_green_paths() {
    let _serial = crate::testlock::serial();
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
    use crate::ui::decision_alert::{Answers, DecisionAlert};
    let _serial = crate::testlock::serial();
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
