// The no-GL FrameCache stand-in `popover::host` is built against in every host test, this crate's
// and the dependents' (`test-support`): a host test constructs dispatchers without a GL context, so
// the real `plx_gfx::gfx::FrameCache` would copy from a framebuffer that does not exist. A copy
// replaces a CPU framebuffer (`PIXELS`), which popover_host_tests.rs draws into.
use std::cell::RefCell;

thread_local! {
    pub(super) static PIXELS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    /// How many times the snapshot was thrown away on this thread (`FrameCache::invalidate`), for
    /// a test that grades WHEN the dispatcher re-takes it.
    pub(super) static INVALIDATIONS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Snapshots taken on this thread (`capture` / `finish_render`): the page's and the ground's.
    pub(super) static CAPTURES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Copies into the retained second slot (`capture_prev`), releases of it, and the alpha of
    /// every quad drawn from it (`draw_prev`).
    pub(super) static PREV_COPIES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    pub(super) static PREV_RELEASES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    pub(super) static PREV_DRAWS: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
}

pub(super) struct FrameCache {
    pub(super) snapshot: Option<Vec<&'static str>>,
    pub(super) off: bool,
    /// The retained second slot: the look the crossfade fades away from.
    pub(super) prev: Option<Vec<&'static str>>,
}

impl FrameCache {
    pub(super) const fn new() -> Self {
        Self { snapshot: None, off: false, prev: None }
    }
    pub(super) fn invalidate(&mut self) {
        INVALIDATIONS.with(|c| c.set(c.get() + 1));
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
        CAPTURES.with(|c| c.set(c.get() + 1));
        true
    }
    /// Copy the framebuffer into the retained second slot (the real cache: its own texture).
    pub(super) fn capture_prev(&mut self) -> bool {
        if self.off || plx_gfx::gfx::blur_source_pass() {
            return false;
        }
        self.prev = Some(PIXELS.with(|p| p.borrow().clone()));
        PREV_COPIES.with(|c| c.set(c.get() + 1));
        true
    }
    /// Draw the retained slot over the frame at `alpha`.
    pub(super) fn draw_prev(&self, alpha: f32) -> bool {
        if self.prev.is_none() {
            return false;
        }
        PREV_DRAWS.with(|d| d.borrow_mut().push(alpha));
        true
    }
    /// Give the retained slot back.
    pub(super) fn release_prev(&mut self) {
        if self.prev.take().is_some() {
            PREV_RELEASES.with(|c| c.set(c.get() + 1));
        }
    }
    pub(super) fn has_prev(&self) -> bool {
        self.prev.is_some()
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
        CAPTURES.with(|c| c.set(c.get() + 1));
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
