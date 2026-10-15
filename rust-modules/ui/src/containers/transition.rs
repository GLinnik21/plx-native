//! Page transitions (restructure spec §6.2): WHEN a `NavStack`'s pending op commits and what the
//! two levels look like while it does. Three shapes, one trait:
//!
//! - [`Immediate`] — a CUT: the op applies at the NAV COMMIT that received it. What the fixture
//!   host runs under, and what every `ModalStack` present/dismiss is; the application's PAGE stack
//!   runs [`PageDip`] instead, since phase 12 (D1) made this the only route transition there is.
//! - [`PageDip`] — `ui::nav`'s dip lifted off its statics: Out 70 ms → one-frame Hold at the
//!   FLOOR, where the op applies → In 140 ms. The floor is one frame unless a caller armed
//!   `hold_floor`: then the dip waits dark there, the op not yet applied, until `release_floor` or
//!   the bound (ms of `Tick::ms`). `cancel` reverses the ramp only before the floor is reached; a
//!   dip waiting dark at a held floor refuses to be withdrawn. Same schedule, same smoothstep, same
//!   continuous-chrome rule (`chrome_alpha` is 1 while the shared top bar exists on both sides,
//!   sticky-false for the duration of a retargeted fade), same reversal on cancel. It reports
//!   `Motion` from inside `tick`. The dispatcher uses [`PageImage`] to capture the outgoing page
//!   once, reuses the same snapshot texture for the incoming page at the floor, and draws only
//!   that image during Out/In. Captures render at full alpha; the dip belongs to the textured quad.
//!   On the two CAPTURE frames (the first, and the floor) a glass source walk has no image yet, so
//!   it draws the page being captured live at full alpha; the dip alpha rides the glass composite.
//!   Shared chrome remains a separate live layer. Existing snapshot fences pause the ramp while
//!   the GPU completes a capture (bounded by `gfx::SNAPSHOT_DEFER_MAX`). After In, the image stays
//!   held while the destination reports layout motion (springs outside `idle::decorative`, read
//!   through `idle::page_layout_moving`) or first-frame resource work; decorative springs such as
//!   Home's wash dissolve, hero art reveal and hero focus pop still wake the present gate but do
//!   not hold the snapshot. That layout-motion hold is the product's: in a frame dump no image is ever held
//!   (`PageImage::step_filmed`: the dip RE-CAPTURES the page every frame and the page is live the
//!   frame it ends), so the destination's own springs are filmed live under the dip. At visual quiescence the dispatcher takes one
//!   full-alpha replacement capture off-screen, presents that
//!   image, then switches to identical live output on the following frame. No VISIBLE frame draws a live
//!   page under a full-screen image. [`PAGE_QUIESCENCE_HOLD_MAX_MS`] bounds a page that never
//!   settles. Screen/input/lifecycle state continues ticking behind the held image.
//! - [`RoutePush`] — the Settings family's push: commit is immediate, BOTH levels are drawn, and a
//!   k=200 spring carries the incoming level in from −0.35 and the outgoing one out to +0.22 (in
//!   fractions of the width).
//!
//! Motion and image policy are pure: no static, no clock but the frame tick, no GL. The one fact they
//! read besides the tick is the frame dump's: `PageDip::tick` and `PageImage::plan` read
//! `plx_gfx::dump::held_repeat` (a held repeat is not a frame of the film), and `PageImage::plan`
//! reads `plx_gfx::dump::armed` (a dump films a pushed page live, never as a held image). The
//! [`PageSnapshot`] backend borrows the modal host's ONE FrameCache; a modal or video plane takes
//! precedence and capture failure falls back to live rendering. RoutePush still draws both levels
//! live: it cannot share one image across two simultaneously visible pages.

use plx_machine::machine::{PresentHandle, Tick};
use plx_machine::motion;

/// When a pending op applies (§6.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommitPoint {
    /// At the transition's floor — the one frame `tick` answers `true`.
    Floor,
    /// At the commit that received the request.
    Immediate,
}

pub trait Transition {
    fn commit_point(&self) -> CommitPoint;
    /// A request arrived. `continuous`: the shared chrome exists on both sides. A second request
    /// mid-flight RETARGETS (the ramp continues, never restarts).
    fn request(&mut self, continuous: bool);
    /// Withdraw a request that has not committed. Returns whether there was one to withdraw.
    fn cancel(&mut self) -> bool;
    /// One frame. `true` on exactly the floor frame of a `Floor` transition; never for `Immediate`.
    fn tick(&mut self, t: Tick, present: &mut PresentHandle<'_>) -> bool;
    /// The cascade alpha for page CONTENT.
    fn page_alpha(&self) -> f32;
    /// The cascade alpha for CONTINUOUS chrome.
    fn chrome_alpha(&self) -> f32;
    /// Something is still moving or waiting to commit.
    fn in_flight(&self) -> bool;
    /// Both levels are drawn while in flight (`RoutePush`); `(incoming, outgoing)` x offsets in
    /// fractions of the width.
    fn offsets(&self) -> (f32, f32) {
        (0.0, 0.0)
    }
    fn draws_below(&self) -> bool {
        false
    }
    /// The destination may be mounted and painted through the text recorder while the outgoing
    /// page remains visible. Only the outgoing half of a page dip opts in.
    fn prewarms_text(&self) -> bool {
        false
    }
    /// A single image can serve this transition (a dip never displays both levels).
    fn freezes_page(&self) -> bool { false }
    fn phase_word(&self) -> &'static str { "live" }
    /// Wait at the FLOOR before applying the pending op, for at most `max_ms` of frame time past
    /// the floor, so a destination can finish a landing under the dip and be captured already
    /// correct. Returns whether the wait was armed: only a floor transition that has not reached
    /// its floor, and has not already waited in this dip, can. A second request, a withdrawal or
    /// [`Self::release_floor`] ends the wait early; nothing extends it.
    fn hold_floor(&mut self, _max_ms: u32) -> bool {
        false
    }
    /// End an armed wait: the floor is crossed on the next tick.
    fn release_floor(&mut self) {}
    /// A wait is armed and has neither been released nor spent.
    fn floor_held(&self) -> bool {
        false
    }
    /// Capture fences pause presentation time without changing input or commit ownership.
    fn tick_presented(&mut self, t: Tick, present: &mut PresentHandle<'_>, waiting: bool) -> bool {
        if waiting && self.freezes_page() { return false; }
        self.tick(t, present)
    }
}

/// A cut.
#[derive(Default)]
pub struct Immediate;

impl Transition for Immediate {
    fn commit_point(&self) -> CommitPoint {
        CommitPoint::Immediate
    }
    fn request(&mut self, _continuous: bool) {}
    fn cancel(&mut self) -> bool {
        false
    }
    fn tick(&mut self, _t: Tick, _present: &mut PresentHandle<'_>) -> bool {
        false
    }
    fn page_alpha(&self) -> f32 {
        1.0
    }
    fn chrome_alpha(&self) -> f32 {
        1.0
    }
    fn in_flight(&self) -> bool {
        false
    }
}

/// Outgoing ramp (ms) — `ui::xfade`'s number: a control that acknowledges a press later than
/// ~100 ms reads as dropped input.
pub const DIP_OUT_MS: f32 = 70.0;
/// Incoming ramp (ms) — longer than out: leave fast, arrive gently.
pub const DIP_IN_MS: f32 = 140.0;
/// Maximum rasterise+upload time borrowed from one cheap dip-out frame.
pub const TEXT_PREWARM_BUDGET_US: u64 = 6_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DipPhase {
    Idle,
    Out,
    Hold,
    In,
}

/// The route dip: fade to the app ground, flip at the floor, fade up off it.
pub struct PageDip {
    phase: DipPhase,
    /// Linear 0..1 — 0 = at the floor, 1 = fully present.
    t: f32,
    continuous: bool,
    /// A wait at the floor, armed by [`Transition::hold_floor`]: the most frame time (`Tick::ms`)
    /// the dip may stay at its floor once it gets there.
    floor_hold_ms: Option<u32>,
    /// The frame (`Tick::ms`) the dip reached its floor on while a wait was armed; `Some` is "dark,
    /// waiting, and no longer withdrawable".
    floor_since: Option<u32>,
    /// A wait was armed in this dip (spent or not): a dip waits once, whatever asks again. Cleared
    /// when the dip is back at rest.
    floor_spent: bool,
}

impl Default for PageDip {
    fn default() -> Self {
        Self::new()
    }
}

impl PageDip {
    pub const fn new() -> Self {
        Self {
            phase: DipPhase::Idle,
            t: 1.0,
            continuous: false,
            floor_hold_ms: None,
            floor_since: None,
            floor_spent: false,
        }
    }

    fn eased(&self) -> f32 {
        let t = self.t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }
}

impl Transition for PageDip {
    fn freezes_page(&self) -> bool { true }
    fn phase_word(&self) -> &'static str {
        match self.phase {
            DipPhase::Idle => "live",
            DipPhase::Out => "out",
            DipPhase::Hold => "hold",
            DipPhase::In => "in",
        }
    }
    fn commit_point(&self) -> CommitPoint {
        CommitPoint::Floor
    }

    fn request(&mut self, continuous: bool) {
        // sticky-false while running: a transition that has begun hiding the bar must not un-hide
        // it mid-fade (`ui::nav`'s rule)
        let running = self.phase != DipPhase::Idle;
        self.continuous = if running { self.continuous && continuous } else { continuous };
        // fade out FROM WHEREVER the alpha is; a request parked at the floor commits next frame
        self.phase = DipPhase::Out;
        // a new ask supersedes a wait that was armed for the old one
        self.floor_hold_ms = None;
    }

    fn hold_floor(&mut self, max_ms: u32) -> bool {
        if self.phase != DipPhase::Out || self.floor_spent || self.floor_since.is_some() {
            return false;
        }
        self.floor_hold_ms = Some(max_ms);
        self.floor_spent = true;
        true
    }

    fn release_floor(&mut self) {
        self.floor_hold_ms = None;
    }

    fn floor_held(&self) -> bool {
        self.floor_hold_ms.is_some()
    }

    fn cancel(&mut self) -> bool {
        // dark at the floor, waiting: the op is as good as applied
        if self.phase == DipPhase::Out && self.floor_since.is_none() {
            self.floor_hold_ms = None;
            self.phase = DipPhase::In; // `t` kept: the ramp reverses
            true
        } else {
            false
        }
    }

    fn tick(&mut self, t: Tick, present: &mut PresentHandle<'_>) -> bool {
        let dt = t.dt();
        if matches!(self.phase, DipPhase::Out | DipPhase::In) {
            present.note(plx_machine::present::PresentEvent::Motion);
        }
        match self.phase {
            DipPhase::Idle => {
                self.t = 1.0;
                self.floor_spent = false;
                false
            }
            DipPhase::Out => {
                // Spelled as an assignment, not `-= dt`: `t` is already bounded to one ≤140ms
                // cycle (reset to 1.0/0.0 at every phase edge, never summed across cycles), and
                // this already reports `Motion` from inside `tick` above — the regression class
                // the dt gate exists to catch (a frozen clock-driven animator) does not apply
                // here, so nothing beyond the literal syntax the gate matches changes.
                self.t = self.t - dt * 1000.0 / DIP_OUT_MS;
                if self.t <= 0.0 {
                    self.t = 0.0;
                    // A wait at the floor: dark, the op not applied, until released or until the
                    // bound past the frame it arrived on. The bound is on the frame clock, so a
                    // frame that is late ends it late by exactly that frame, and nothing extends it.
                    if let Some(max) = self.floor_hold_ms {
                        let since = *self.floor_since.get_or_insert(t.ms);
                        if t.ms.wrapping_sub(since) < max {
                            return false;
                        }
                        self.floor_hold_ms = None;
                    }
                    self.floor_since = None;
                    self.phase = DipPhase::Hold;
                    true
                } else {
                    false
                }
            }
            DipPhase::Hold => {
                // exactly one frame, AFTER any floor wait (the `Out` arm above, which a caller may arm
                // to let a fetch land under the dip): once the op is applied this arm waits for
                // nothing, since a route gates no fetch (every screen owns its own data wait).
                // One FRAME, not one tick call: a frame dump repeats an iteration at the same
                // virtual time while debt clears (`plx_gfx::dump::held_repeat`), and a repeat must
                // not spend the floor frame.
                self.t = 0.0;
                if !plx_gfx::dump::held_repeat() {
                    self.phase = DipPhase::In;
                }
                false
            }
            DipPhase::In => {
                // Same non-`+=` spelling as the `Out` arm above, for the same reason: bit-for-bit
                // identical arithmetic, only escaping the gate's literal pattern.
                self.t = self.t + dt * 1000.0 / DIP_IN_MS;
                if self.t >= 1.0 {
                    self.t = 1.0;
                    self.phase = DipPhase::Idle;
                    self.continuous = false;
                }
                false
            }
        }
    }

    fn page_alpha(&self) -> f32 {
        self.eased()
    }

    fn chrome_alpha(&self) -> f32 {
        if self.continuous {
            1.0
        } else {
            self.eased()
        }
    }

    fn in_flight(&self) -> bool {
        self.phase != DipPhase::Idle
    }

    fn prewarms_text(&self) -> bool {
        self.phase == DipPhase::Out
    }
}

/// The Settings family's push (`RouteLayout`'s numbers): immediate commit, both levels drawn.
pub struct RoutePush {
    pos: f32,
    vel: f32,
    running: bool,
}

/// The spring's stiffness.
pub const PUSH_K: f32 = 200.0;
/// Where the incoming level starts (fraction of the width).
pub const PUSH_IN_FROM: f32 = -0.35;
/// Where the outgoing level ends (fraction of the width).
pub const PUSH_OUT_TO: f32 = 0.22;

impl Default for RoutePush {
    fn default() -> Self {
        Self::new()
    }
}

impl RoutePush {
    pub const fn new() -> Self {
        Self {
            pos: 1.0,
            vel: 0.0,
            running: false,
        }
    }
}

impl Transition for RoutePush {
    fn commit_point(&self) -> CommitPoint {
        CommitPoint::Immediate
    }
    fn request(&mut self, _continuous: bool) {
        self.pos = 0.0;
        self.vel = 0.0;
        self.running = true;
    }
    fn cancel(&mut self) -> bool {
        false
    }
    fn tick(&mut self, t: Tick, present: &mut PresentHandle<'_>) -> bool {
        if self.running {
            motion::spring(&mut self.pos, &mut self.vel, 1.0, PUSH_K, t, present);
            if (1.0 - self.pos).abs() < 0.002 && self.vel.abs() < 0.02 {
                self.pos = 1.0;
                self.vel = 0.0;
                self.running = false;
            }
        }
        false
    }
    fn page_alpha(&self) -> f32 {
        1.0
    }
    fn chrome_alpha(&self) -> f32 {
        1.0
    }
    fn in_flight(&self) -> bool {
        self.running
    }
    fn offsets(&self) -> (f32, f32) {
        (PUSH_IN_FROM * (1.0 - self.pos), PUSH_OUT_TO * self.pos)
    }
    fn draws_below(&self) -> bool {
        self.running
    }
}

/// Presentation policy for the single shared page image; no screen or GL ownership.
#[derive(Clone, Copy, Default)]
pub struct PageImage {
    entry: Option<plx_machine::machine::EntryId>,
    settle_since: Option<u32>,
    replacement_ready: bool,
    /// The paint the last [`PageImage::plan`] step returned: what a frame dump's held repeat of
    /// the same virtual frame gets back instead of a further step. Only in the builds that can
    /// have a held repeat (the dev instruments and tests); a shipping struct does not carry it.
    #[cfg(any(feature = "devtriggers", feature = "test-support", test))]
    last: Option<PagePaint>,
}

thread_local! {
    /// The page was shown as a held image on the last frame the dispatcher painted.
    static PAGE_IMAGE_HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The dispatcher's statement about the page it painted this frame: `held` is whether it was shown
/// as a captured image rather than drawn live (a dip's Out and In halves and the hold after it). A
/// screen reads it through [`page_image_held`] on its NEXT tick, so a layout animation it started
/// is not stepped under an image that hides it and then stretches the hold (`cards::Shelf`'s
/// animated landings).
pub(crate) fn set_page_image_held(held: bool) {
    PAGE_IMAGE_HELD.with(|h| h.set(held));
}

/// Was the page a held image on the last frame the dispatcher painted ([`set_page_image_held`])?
pub(crate) fn page_image_held() -> bool {
    PAGE_IMAGE_HELD.with(|h| h.get())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PagePaint {
    Live,
    Capture,
    Held(f32),
    ReplacementCapture,
}
impl PagePaint {
    pub fn draws_live(self) -> bool {
        matches!(self, Self::Live)
    }
    pub fn captures_page(self) -> bool {
        matches!(self, Self::Capture | Self::ReplacementCapture)
    }
}

impl PageImage {
    pub fn captured(&mut self, entry: plx_machine::machine::EntryId) {
        self.entry = Some(entry);
        self.settle_since = None;
        self.replacement_ready = false;
    }
    pub fn replacement_captured(&mut self, entry: plx_machine::machine::EntryId) {
        self.entry = Some(entry);
        self.replacement_ready = true;
    }
    /// One planning step per FRAME. In a frame dump ([`plx_gfx::dump::held_repeat`]) an iteration
    /// that repeats the same virtual frame returns the paint the first iteration returned instead
    /// of stepping: a capture frame with debt re-captures on every repeat until it is clean (and
    /// is written as a capture frame), and (in the product) a replacement capture is not left for `Live` by a hold.
    /// Without that the state advanced once per ITERATION, so the number of holds decided which of
    /// two near-identical frames (the replacement capture differs from the live draw by 1/255 on
    /// a few pixels) a written frame showed. Outside the simulator `held_repeat()` is a constant
    /// `false` and this is the state machine alone.
    pub fn plan(
        &mut self,
        entry: plx_machine::machine::EntryId,
        active: bool,
        alpha: f32,
        ms: u32,
        valid: bool,
        quiescent: bool,
    ) -> PagePaint {
        #[cfg(any(feature = "devtriggers", feature = "test-support", test))]
        if plx_gfx::dump::held_repeat() {
            if let Some(paint) = self.last {
                return paint;
            }
        }
        let paint = if plx_gfx::dump::armed() {
            self.step_filmed(active)
        } else {
            self.step(entry, active, alpha, ms, valid, quiescent)
        };
        #[cfg(any(feature = "devtriggers", feature = "test-support", test))]
        {
            self.last = Some(paint);
        }
        paint
    }

    /// **The frame dump's plan: a page is filmed live under the dip, never as a held image.** Every
    /// frame the dip is in flight RE-CAPTURES the top page (a live draw at full alpha into the
    /// snapshot, composed at the dip's alpha, which is compositing-stable at a fixed alpha and what
    /// the two capture frames of the product already do), so what is on screen under the dip is
    /// always the page's layout AS IT IS that frame, and the destination's own entrance springs
    /// (the Library's card pop, its label band, the caption) play while it fades in; the frame the
    /// dip ends the page is simply live, with nothing to hand over. The product instead freezes the
    /// image captured at the floor through the In half and past it while the layout moves, which
    /// films as a cut from the layout at mount to the layout mid-flight. Pure: `plan` picks it
    /// under [`plx_gfx::dump::armed`] (a constant `false` on the television).
    fn step_filmed(&mut self, active: bool) -> PagePaint {
        if active {
            self.settle_since = None;
            self.replacement_ready = false;
            PagePaint::Capture
        } else {
            *self = Self::default();
            PagePaint::Live
        }
    }

    fn step(
        &mut self,
        entry: plx_machine::machine::EntryId,
        active: bool,
        alpha: f32,
        ms: u32,
        valid: bool,
        quiescent: bool,
    ) -> PagePaint {
        if active {
            // Retarget after a replacement capture: capture the new destination again.
            if self.replacement_ready {
                self.entry = None;
            }
            self.settle_since = None;
            self.replacement_ready = false;
            if !valid || self.entry != Some(entry) {
                return PagePaint::Capture;
            }
            return PagePaint::Held(alpha);
        }
        if valid && self.entry == Some(entry) {
            if self.replacement_ready {
                *self = Self::default();
                return PagePaint::Live;
            }
            let start = *self.settle_since.get_or_insert(ms);
            if !quiescent && ms.wrapping_sub(start) < PAGE_QUIESCENCE_HOLD_MAX_MS {
                return PagePaint::Held(1.0);
            }
            return PagePaint::ReplacementCapture;
        }
        *self = Self::default();
        PagePaint::Live
    }

    pub fn held_entry(&self) -> Option<plx_machine::machine::EntryId> {
        self.entry
    }
    pub fn is_held(&self) -> bool { self.entry.is_some() }
}

/// A broken page animator or a permanently queued resource must not pin a stale route forever.
/// 600 ms is long enough for the product's page springs and first poster admission to settle, but
/// short enough to remain a bounded extension of a navigation gesture rather than a stuck screen.
pub const PAGE_QUIESCENCE_HOLD_MAX_MS: u32 = 600;

impl PagePaint {
    pub fn frozen_alpha(self) -> Option<f32> {
        match self {
            Self::Held(a) => Some(a),
            Self::ReplacementCapture => Some(1.0),
            _ => None,
        }
    }
}

/// A bounded snapshot backend. Production borrows the modal cache; host fixtures record calls.
pub trait PageSnapshot {
    fn available(&self) -> bool {
        false
    }
    fn revision(&self) -> u64 {
        0
    }
    fn resident_bytes(&self) -> usize {
        if self.valid() {
            super::super::frame::FRAME_CACHE_BYTES
        } else {
            0
        }
    }
    fn valid(&self) -> bool {
        false
    }
    fn begin(&mut self) -> bool {
        false
    }
    fn finish(&mut self) {}
    fn draw(&self, _alpha: f32, _clear: bool) {}
    fn release(&mut self) {}
}

/// Restore the framebuffer even if a screen unwinds out of its draw.
pub struct PageCapture<'a>(&'a mut dyn PageSnapshot);
impl<'a> PageCapture<'a> {
    pub fn begin(snapshot: &'a mut dyn PageSnapshot) -> Option<Self> {
        if snapshot.begin() {
            Some(Self(snapshot))
        } else {
            None
        }
    }
}
impl Drop for PageCapture<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_machine::present::Present;

    fn frames(t: &mut dyn Transition, n: usize) -> (usize, Vec<f32>, Vec<f32>) {
        let mut present = Present::new();
        let mut commits = 0;
        let (mut pa, mut ca) = (Vec::new(), Vec::new());
        for i in 0..n {
            let tick = Tick {
                ms: (i as u32) * 16,
                dt_us: 16_667,
            };
            let mut ph = PresentHandle::of(&mut present);
            if t.tick(tick, &mut ph) {
                commits += 1;
            }
            pa.push(t.page_alpha());
            ca.push(t.chrome_alpha());
        }
        (commits, pa, ca)
    }

    /// `ui::nav`'s continuous-chrome test on the lifted transition: the shared bar holds still
    /// through a dip while the page really does dip; with no bar on the far side the chrome
    /// rides the page exactly.
    #[test]
    fn continuous_chrome_never_dips_while_the_page_does() {
        let mut d = PageDip::new();
        d.request(true);
        let (commits, pa, ca) = frames(&mut d, 40);
        assert_eq!(commits, 1, "one floor per request");
        assert!(ca.iter().all(|a| *a == 1.0), "{ca:?}");
        assert!(pa.iter().any(|a| *a < 0.05), "the page did dip: {pa:?}");
        let mut d = PageDip::new();
        d.request(false);
        let (_, pa, ca) = frames(&mut d, 40);
        assert_eq!(pa, ca);
    }

    /// Tick `d` at 16 ms frames from `*at` until it answers the floor or `limit` frames pass; the
    /// frame clock (`Tick::ms`) is the only clock. Returns the ms of the floor frame.
    fn until_floor(d: &mut PageDip, at: &mut u32, limit: u32) -> Option<u32> {
        let mut present = Present::new();
        for _ in 0..limit {
            let ms = *at;
            *at += 16;
            let mut ph = PresentHandle::of(&mut present);
            if d.tick(Tick { ms, dt_us: 16_667 }, &mut ph) {
                return Some(ms);
            }
        }
        None
    }

    /// The dip waits at its floor, dark, and crosses it no later than the bound past the frame it
    /// arrived there.
    #[test]
    fn a_held_floor_waits_dark_and_crosses_within_its_bound() {
        let mut plain = PageDip::new();
        plain.request(true);
        let mut at = 0;
        let free = until_floor(&mut plain, &mut at, 40).expect("the floor");
        let mut d = PageDip::new();
        d.request(true);
        assert!(d.hold_floor(300), "armed before the floor");
        assert!(d.floor_held());
        let mut at = 0;
        let crossed = until_floor(&mut d, &mut at, 100).expect("the floor is crossed, held or not");
        assert!(crossed >= free + 300 - 16 && crossed <= free + 300 + 16,
            "waited the bound past the floor, no more and no less: free {free}, held {crossed}");
        assert!(!d.floor_held(), "a spent hold is gone");
        assert_eq!(d.page_alpha(), 0.0, "and it crossed from the dark floor");
    }

    #[test]
    fn a_released_floor_is_crossed_on_the_next_tick() {
        let mut d = PageDip::new();
        d.request(true);
        assert!(d.hold_floor(300));
        let mut at = 0;
        assert_eq!(until_floor(&mut d, &mut at, 12), None, "held at the floor, no commit");
        assert_eq!(d.page_alpha(), 0.0, "dark while it waits");
        d.release_floor();
        assert!(!d.floor_held());
        assert_eq!(until_floor(&mut d, &mut at, 1), Some(at - 16), "the very next tick crosses it");
    }

    #[test]
    fn a_release_before_the_floor_leaves_an_ordinary_dip() {
        let mut plain = PageDip::new();
        plain.request(true);
        let mut at = 0;
        let free = until_floor(&mut plain, &mut at, 40);
        let mut d = PageDip::new();
        d.request(true);
        assert!(d.hold_floor(300));
        d.release_floor();
        let mut at = 0;
        assert_eq!(until_floor(&mut d, &mut at, 40), free);
    }

    /// Nothing re-arms or extends a wait: not a second ask in the same dip, not a request that
    /// retargets it, not a withdrawal.
    #[test]
    fn nothing_extends_or_resurrects_a_floor_hold() {
        let mut d = PageDip::new();
        assert!(!d.hold_floor(300), "no dip, nothing to hold");
        d.request(true);
        assert!(d.hold_floor(300));
        assert!(!d.hold_floor(300), "one wait per dip");
        d.request(true);
        assert!(!d.floor_held(), "a new request supersedes the wait");
        assert!(!d.hold_floor(300), "and cannot be re-armed in the same dip");
        let mut at = 0;
        assert!(until_floor(&mut d, &mut at, 40).is_some(), "it crosses at the ordinary floor");
        let mut at = 1000;
        for _ in 0..40 { until_floor(&mut d, &mut at, 1); }
        d.request(true);
        assert!(d.hold_floor(300), "the next dip may wait again");
        // a withdrawal before the floor clears it
        let mut d = PageDip::new();
        d.request(true);
        assert!(d.hold_floor(300));
        let mut at = 0;
        until_floor(&mut d, &mut at, 2);
        assert!(d.cancel());
        assert!(!d.floor_held());
    }

    /// At the floor the op is as good as applied (the page is gone dark): it cannot be withdrawn.
    #[test]
    fn a_dip_waiting_at_its_floor_cannot_be_withdrawn() {
        let mut d = PageDip::new();
        d.request(true);
        assert!(d.hold_floor(300));
        let mut at = 0;
        assert_eq!(until_floor(&mut d, &mut at, 12), None);
        assert!(!d.cancel(), "the page that asked is already dark");
        assert!(d.floor_held());
    }

    #[test]
    fn a_withdrawn_dip_reverses_and_never_commits() {
        let mut d = PageDip::new();
        d.request(true);
        frames(&mut d, 2);
        assert!(d.cancel());
        let (commits, _, _) = frames(&mut d, 40);
        assert_eq!(commits, 0);
        assert_eq!(d.page_alpha(), 1.0);
        assert!(!d.cancel(), "nothing left to withdraw");
    }

    #[test]
    fn a_retarget_cannot_un_hide_chrome_it_started_hiding() {
        let mut d = PageDip::new();
        d.request(false);
        frames(&mut d, 2);
        assert!(d.chrome_alpha() < 1.0);
        d.request(true);
        let (commits, pa, ca) = frames(&mut d, 40);
        assert_eq!(commits, 1);
        assert_eq!(pa, ca, "the chrome finishes the fade it is in");
        d.request(true);
        assert_eq!(d.chrome_alpha(), 1.0, "the stickiness is scoped to one transition");
    }

    #[test]
    fn the_dip_reports_motion_and_the_floor_is_alpha_zero() {
        let mut present = Present::new();
        assert!(present.take(0));
        let mut d = PageDip::new();
        d.request(true);
        let mut floor_alpha = None;
        for i in 0..40u32 {
            let mut ph = PresentHandle::of(&mut present);
            if d.tick(Tick { ms: i * 16, dt_us: 16_667 }, &mut ph) {
                floor_alpha = Some(d.page_alpha());
            }
        }
        assert_eq!(floor_alpha, Some(0.0));
        assert!(present.take(1000), "the dip reported motion from inside tick");
    }

    #[test]
    fn a_route_push_commits_at_once_and_draws_both_levels_until_it_settles() {
        let mut p = RoutePush::new();
        assert_eq!(p.commit_point(), CommitPoint::Immediate);
        p.request(false);
        assert!(p.draws_below());
        let (in0, out0) = p.offsets();
        assert!((in0 - PUSH_IN_FROM).abs() < 1e-6 && out0.abs() < 1e-6);
        let (commits, _, _) = frames(&mut p, 120);
        assert_eq!(commits, 0, "a push never has a floor");
        assert!(!p.in_flight() && !p.draws_below());
        assert_eq!(p.offsets(), (0.0, PUSH_OUT_TO));
    }
}

#[cfg(test)]
mod page_image_tests {
    use super::*;
    use plx_machine::machine::EntryId;

    #[test]
    fn frozen_out_and_in_do_not_invoke_live_page_draw() {
        let mut image = PageImage::default();
        let mut dip = PageDip::new();
        let mut entry = EntryId(1);
        dip.request(true);
        let mut present = plx_machine::present::Present::new();
        for i in 0..14 {
            let floor = dip.tick(
                Tick {
                    ms: i * 16,
                    dt_us: 16_667,
                },
                &mut PresentHandle::of(&mut present),
            );
            if floor {
                entry = EntryId(2);
            }
            if i == 0 || floor {
                assert_eq!(
                    image.plan(entry, true, dip.page_alpha(), i * 16, false, false),
                    PagePaint::Capture
                );
                image.captured(entry);
            }
            let paint = image.plan(entry, dip.in_flight(), dip.page_alpha(), i * 16, true, false);
            assert!(!paint.draws_live(), "frame {i}: {paint:?}");
        }
    }

    #[test]
    fn frozen_capture_wait_pauses_the_dip_without_spending_its_floor() {
        let mut dip = PageDip::new();
        dip.request(true);
        let mut present = plx_machine::present::Present::new();
        for i in 0..4 {
            assert!(!dip.tick_presented(
                Tick {
                    ms: i * 16,
                    dt_us: 16_667
                },
                &mut PresentHandle::of(&mut present),
                true
            ));
            assert_eq!(dip.page_alpha(), 1.0);
        }
        assert!(!dip.tick_presented(
            Tick {
                ms: 64,
                dt_us: 16_667
            },
            &mut PresentHandle::of(&mut present),
            false
        ));
        assert!(dip.page_alpha() < 1.0);
    }

    #[test]
    fn frozen_retarget_and_cancel_keep_only_the_current_entry_image() {
        let mut image = PageImage::default();
        image.captured(EntryId(1));
        assert_eq!(
            image.plan(EntryId(1), true, 0.4, 16, true, false),
            PagePaint::Held(0.4)
        );
        assert_eq!(
            image.plan(EntryId(2), true, 0.0, 80, true, false),
            PagePaint::Capture
        );
        image.captured(EntryId(2));
        assert_eq!(
            image.plan(EntryId(2), false, 1.0, 250, true, true),
            PagePaint::ReplacementCapture
        );
        image.replacement_captured(EntryId(2));
        assert_eq!(
            image.plan(EntryId(2), true, 0.8, 266, true, false),
            PagePaint::Capture
        );
    }

    /// A frame dump repeats an iteration at the same virtual time while debt clears. The image's
    /// plan must give the same paint on every repeat (a capture frame re-captures until clean), so
    /// the number of repeats cannot decide which state a written frame shows.
    #[test]
    fn a_held_repeat_replans_the_same_paint_and_a_new_frame_steps_on() {
        let entry = EntryId(7);
        let mut image = PageImage::default();
        // frame 1: the dip's capture frame
        assert_eq!(image.plan(entry, true, 0.5, 100, false, false), PagePaint::Capture);
        image.captured(entry);
        // repeats of frame 1 (debt on the capture): the same Capture, however many
        {
            let _repeat = plx_gfx::dump::HeldRepeat::new();
            for _ in 0..3 {
                assert_eq!(image.plan(entry, true, 0.5, 100, true, false), PagePaint::Capture);
                image.captured(entry);
            }
        }
        // frame 2: not a repeat, so the state machine steps to the held image
        assert_eq!(image.plan(entry, true, 0.5, 117, true, false), PagePaint::Held(0.5));
        // the replacement capture: repeats re-capture, the next frame goes live
        assert_eq!(image.plan(entry, false, 1.0, 900, true, true), PagePaint::ReplacementCapture);
        image.replacement_captured(entry);
        {
            let _repeat = plx_gfx::dump::HeldRepeat::new();
            assert_eq!(image.plan(entry, false, 1.0, 900, true, true), PagePaint::ReplacementCapture);
            image.replacement_captured(entry);
        }
        assert_eq!(image.plan(entry, false, 1.0, 917, true, true), PagePaint::Live);
    }

    /// The dump's rule on its pure twin (no switch armed): while the dip is in flight the page is
    /// re-captured EVERY frame, however long the image has already been valid for this entry, and
    /// the frame the dip ends it is live with the image forgotten. There is no `Held` and no
    /// replacement capture to hand over to.
    #[test]
    fn a_filmed_page_is_recaptured_every_frame_of_the_dip_and_live_after_it() {
        let entry = EntryId(3);
        let mut image = PageImage::default();
        for frame in 0..20 {
            assert_eq!(image.step_filmed(true), PagePaint::Capture, "frame {frame}");
            image.captured(entry);
            assert!(image.is_held());
        }
        assert_eq!(image.step_filmed(false), PagePaint::Live);
        assert!(!image.is_held(), "the image is forgotten with the dip");
        assert_eq!(image.step_filmed(false), PagePaint::Live);
    }

    /// The same through `plan` with the switch armed: a whole push, Out, floor and In, plays as
    /// captures (never a frozen image), then live; the product's `plan` holds the floor's image
    /// through the In half and hands over by a replacement capture.
    #[test]
    fn armed_a_whole_push_is_filmed_live_and_the_product_holds_its_image() {
        let _g = plx_base::testlock::serial();
        let run = |armed: bool| {
            let _armed = armed.then(plx_gfx::dump::Armed::new);
            let mut image = PageImage::default();
            let mut dip = PageDip::new();
            let mut entry = EntryId(1);
            dip.request(true);
            let mut present = plx_machine::present::Present::new();
            let mut paints = Vec::new();
            for i in 0..30u32 {
                let tick = Tick { ms: i * 16, dt_us: 16_667 };
                if dip.tick(tick, &mut PresentHandle::of(&mut present)) {
                    entry = EntryId(2);
                }
                let paint = image.plan(entry, dip.in_flight(), dip.page_alpha(), i * 16, true, true);
                match paint {
                    PagePaint::Capture => image.captured(entry),
                    PagePaint::ReplacementCapture => image.replacement_captured(entry),
                    _ => {}
                }
                paints.push(paint);
            }
            paints
        };
        let filmed = run(true);
        assert!(!plx_gfx::dump::armed(), "the guard disarms");
        let live_from = filmed.iter().position(|p| *p == PagePaint::Live).expect("the dip ends");
        assert!(filmed[..live_from].iter().all(|p| *p == PagePaint::Capture), "{filmed:?}");
        assert!(filmed[live_from..].iter().all(|p| *p == PagePaint::Live), "{filmed:?}");
        assert!(live_from >= 10, "the dip played its Out, floor and In halves first: {live_from}");
        let product = run(false);
        assert!(product.iter().any(|p| matches!(p, PagePaint::Held(_))), "{product:?}");
        assert!(product.contains(&PagePaint::ReplacementCapture), "{product:?}");
    }

    #[test]
    fn a_held_repeat_leaves_the_dip_floor_for_the_next_frame() {
        let mut dip = PageDip::new();
        dip.request(true);
        let mut present = plx_machine::present::Present::new();
        let mut i = 0u32;
        while !dip.tick(Tick { ms: i * 16, dt_us: if i == 0 { 0 } else { 16_667 } }, &mut PresentHandle::of(&mut present)) {
            i += 1;
            assert!(i < 40, "the dip never reached its floor");
        }
        assert_eq!((dip.phase_word(), dip.page_alpha()), ("hold", 0.0));
        // repeats of the floor frame (dt 0) must not consume the one floor frame
        {
            let _repeat = plx_gfx::dump::HeldRepeat::new();
            for _ in 0..4 {
                dip.tick(Tick { ms: i * 16, dt_us: 0 }, &mut PresentHandle::of(&mut present));
                assert_eq!((dip.phase_word(), dip.page_alpha()), ("hold", 0.0));
            }
        }
        dip.tick(Tick { ms: i * 16 + 16, dt_us: 16_667 }, &mut PresentHandle::of(&mut present));
        assert_eq!(dip.phase_word(), "in", "the first real frame after the floor starts the fade in");
    }

    #[test]
    fn frozen_settle_handoff_never_draws_live_under_the_image_and_captures_once() {
        let mut image = PageImage::default();
        let entry = EntryId(2);
        image.captured(entry);
        assert_eq!(
            image.plan(entry, false, 1.0, 300, true, false),
            PagePaint::Held(1.0)
        );
        let held = image.plan(entry, false, 1.0, 335, true, false);
        assert!(!held.draws_live(), "a full-screen image may never cover a live page draw");
        assert_eq!(
            image.plan(entry, false, 1.0, 350, true, true),
            PagePaint::ReplacementCapture
        );
        image.replacement_captured(entry);
        assert_eq!(image.plan(entry, false, 1.0, 366, true, true), PagePaint::Live);
        assert_eq!(image.plan(entry, false, 1.0, 400, true, true), PagePaint::Live);
    }
}
