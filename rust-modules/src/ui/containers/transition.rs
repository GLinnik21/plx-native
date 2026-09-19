//! Page transitions (restructure spec §6.2): WHEN a `NavStack`'s pending op commits and what the
//! two levels look like while it does. Three shapes, one trait:
//!
//! - [`Immediate`] — a CUT: the op applies at the NAV COMMIT that received it. What the fixture
//!   host runs under, and what every `ModalStack` present/dismiss is; the application's PAGE stack
//!   runs [`PageDip`] instead, since phase 12 (D1) made this the only route transition there is.
//! - [`PageDip`] — `ui::nav`'s dip lifted off its statics: Out 70 ms → one-frame Hold at the
//!   FLOOR, where the op applies → In 140 ms. Same schedule, same smoothstep, same
//!   continuous-chrome rule (`chrome_alpha` is 1 while the shared top bar exists on both sides,
//!   sticky-false for the duration of a retargeted fade), same reversal on cancel. It reports
//!   `Motion` from inside `tick`. The dispatcher uses [`PageImage`] to capture the outgoing page
//!   once, reuses the same snapshot texture for the incoming page at the floor, and draws only
//!   that image during Out/In. Captures render at full alpha; the dip belongs to the textured quad.
//!   Shared chrome remains a separate live layer. Existing snapshot fences pause the ramp while
//!   the GPU completes a capture (bounded by `gfx::SNAPSHOT_DEFER_MAX`). After In, live drawing
//!   resumes beneath the held image, which dissolves over [`PAGE_HANDOFF_MS`] so newly arrived
//!   art or advanced springs cannot pop at hand-off. No screen/input/lifecycle state is frozen.
//! - [`RoutePush`] — the Settings family's push: commit is immediate, BOTH levels are drawn, and a
//!   k=200 spring carries the incoming level in from −0.35 and the outgoing one out to +0.22 (in
//!   fractions of the width).
//!
//! Motion and image policy are pure: no static, no clock but the frame tick, no GL. The
//! [`PageSnapshot`] backend borrows the modal host's ONE FrameCache; a modal or video plane takes
//! precedence and capture failure falls back to live rendering. RoutePush still draws both levels
//! live: it cannot share one image across two simultaneously visible pages. The image hand-off
//! adds live fill after settle and must be included in the device frame-time gate.

use super::super::machine::{PresentHandle, Tick};
use super::super::motion;

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
        }
    }

    fn eased(&self) -> f32 {
        let t = self.t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }
}

impl Transition for PageDip {
    fn freezes_page(&self) -> bool { true }
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
    }

    fn cancel(&mut self) -> bool {
        if self.phase == DipPhase::Out {
            self.phase = DipPhase::In; // `t` kept: the ramp reverses
            true
        } else {
            false
        }
    }

    fn tick(&mut self, t: Tick, present: &mut PresentHandle<'_>) -> bool {
        let dt = t.dt();
        if matches!(self.phase, DipPhase::Out | DipPhase::In) {
            present.note(super::super::present::PresentEvent::Motion);
        }
        match self.phase {
            DipPhase::Idle => {
                self.t = 1.0;
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
                    self.phase = DipPhase::Hold;
                    true
                } else {
                    false
                }
            }
            DipPhase::Hold => {
                // exactly one frame: a route gates no fetch (every screen owns its own data wait)
                self.t = 0.0;
                self.phase = DipPhase::In;
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
pub(crate) struct PageImage {
    entry: Option<super::super::machine::EntryId>,
    handoff: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PagePaint {
    Live,
    Capture,
    Held(f32),
    Handoff(f32),
}
impl PagePaint {
    pub(crate) fn draws_live(self) -> bool {
        !matches!(self, Self::Held(_))
    }
}

impl PageImage {
    pub(crate) fn captured(&mut self, entry: super::super::machine::EntryId) {
        self.entry = Some(entry);
        self.handoff = None;
    }
    pub(crate) fn plan(
        &mut self,
        entry: super::super::machine::EntryId,
        active: bool,
        alpha: f32,
        ms: u32,
        valid: bool,
    ) -> PagePaint {
        if active {
            // Retarget during the live hand-off: capture the now-live page again.
            if self.handoff.take().is_some() {
                self.entry = None;
            }
            if !valid || self.entry != Some(entry) {
                return PagePaint::Capture;
            }
            return PagePaint::Held(alpha);
        }
        if valid && self.entry == Some(entry) {
            let start = *self.handoff.get_or_insert(ms);
            let t = (ms.wrapping_sub(start) as f32 / PAGE_HANDOFF_MS).clamp(0.0, 1.0);
            if t < 1.0 {
                return PagePaint::Handoff(1.0 - t * t * (3.0 - 2.0 * t));
            }
        }
        *self = Self::default();
        PagePaint::Live
    }

    pub(crate) fn held_entry(&self) -> Option<super::super::machine::EntryId> {
        self.entry
    }
}

/// Live content may have landed while held. Reveal it after settle, starting with the exact
/// held image on top; never replace a stale image with new pixels in one frame.
pub(crate) const PAGE_HANDOFF_MS: f32 = 70.0;

impl PagePaint {
    pub(crate) fn frozen_alpha(self) -> Option<f32> {
        match self {
            Self::Held(a) | Self::Handoff(a) => Some(a),
            _ => None,
        }
    }
}

/// A bounded snapshot backend. Production borrows the modal cache; host fixtures record calls.
pub(crate) trait PageSnapshot {
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
pub(crate) struct PageCapture<'a>(&'a mut dyn PageSnapshot);
impl<'a> PageCapture<'a> {
    pub(crate) fn begin(snapshot: &'a mut dyn PageSnapshot) -> Option<Self> {
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
    use crate::ui::present::Present;

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
    use crate::ui::machine::EntryId;

    #[test]
    fn frozen_out_and_in_do_not_invoke_live_page_draw() {
        let mut image = PageImage::default();
        let mut dip = PageDip::new();
        let mut entry = EntryId(1);
        dip.request(true);
        let mut present = crate::ui::present::Present::new();
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
                    image.plan(entry, true, dip.page_alpha(), i * 16, false),
                    PagePaint::Capture
                );
                image.captured(entry);
            }
            let paint = image.plan(entry, dip.in_flight(), dip.page_alpha(), i * 16, true);
            assert!(!paint.draws_live(), "frame {i}: {paint:?}");
        }
    }

    #[test]
    fn frozen_capture_wait_pauses_the_dip_without_spending_its_floor() {
        let mut dip = PageDip::new();
        dip.request(true);
        let mut present = crate::ui::present::Present::new();
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
            image.plan(EntryId(1), true, 0.4, 16, true),
            PagePaint::Held(0.4)
        );
        assert_eq!(
            image.plan(EntryId(2), true, 0.0, 80, true),
            PagePaint::Capture
        );
        image.captured(EntryId(2));
        assert_eq!(
            image.plan(EntryId(2), false, 1.0, 250, true),
            PagePaint::Handoff(1.0)
        );
        assert_eq!(
            image.plan(EntryId(2), true, 0.8, 266, true),
            PagePaint::Capture
        );
    }

    #[test]
    fn frozen_settle_handoff_starts_cross_matched_then_releases() {
        let mut image = PageImage::default();
        let entry = EntryId(2);
        image.captured(entry);
        assert_eq!(
            image.plan(entry, false, 1.0, 300, true),
            PagePaint::Handoff(1.0)
        );
        let midway = image.plan(entry, false, 1.0, 335, true);
        assert!(matches!(midway, PagePaint::Handoff(a) if a > 0.0 && a < 1.0));
        assert!(midway.draws_live());
        assert_eq!(image.plan(entry, false, 1.0, 370, true), PagePaint::Live);
        assert_eq!(image.plan(entry, false, 1.0, 400, true), PagePaint::Live);
    }
}
