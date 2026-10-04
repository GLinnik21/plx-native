// The no-GL FrameCache stand-in `popover::host` is built against in every host test, this crate's
// and the dependents' (`test-support`): a host test constructs dispatchers without a GL context, so
// the real `plx_gfx::gfx::FrameCache` would copy from a framebuffer that does not exist. A copy
// replaces a CPU framebuffer (`PIXELS`), which popover_host_tests.rs draws into.
use std::cell::RefCell;

thread_local! {
    pub(super) static PIXELS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

pub(super) struct FrameCache {
    pub(super) snapshot: Option<Vec<&'static str>>,
    pub(super) off: bool,
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
        if self.off || plx_gfx::gfx::blur_source_pass() {
            return false;
        }
        self.snapshot = Some(PIXELS.with(|p| p.borrow().clone()));
        true
    }
    /// No GL context in a host test: this always declines, exactly as `render_available` says, so
    /// every caller falls back to the `capture`/`draw` copy path exercised by this file's tests.
    pub(super) fn render_into(&mut self) -> Option<plx_base::surface::PageTarget> {
        None
    }
    pub(super) fn rendered(&mut self, target: plx_base::surface::PageTarget) {
        self.finish_render(target);
        self.draw();
    }
    pub(super) fn finish_render(&mut self, target: plx_base::surface::PageTarget) {
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
