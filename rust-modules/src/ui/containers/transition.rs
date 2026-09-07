//! Page transitions (restructure spec §6.2): WHEN a `NavStack`'s pending op commits and what the
//! two levels look like while it does. Three shapes, one trait:
//!
//! - [`Immediate`] — a CUT: the op applies at the NAV COMMIT that received it. What the fixture
//!   host runs under, and what every one of today's ~25 `route =` assignments is.
//! - [`PageDip`] — `ui::nav`'s dip lifted off its statics: Out 70 ms → one-frame Hold at the
//!   FLOOR, where the op applies → In 140 ms. Same schedule, same smoothstep, same
//!   continuous-chrome rule (`chrome_alpha` is 1 while the shared top bar exists on both sides,
//!   sticky-false for the duration of a retargeted fade), same reversal on cancel. It reports
//!   `Motion` from inside `tick`, which is what `Xfade` lacked when it shipped frozen.
//! - [`RoutePush`] — the Settings family's push: commit is immediate, BOTH levels are drawn, and a
//!   k=200 spring carries the incoming level in from −0.35 and the outgoing one out to +0.22 (in
//!   fractions of the width).
//!
//! Pure: no static, no clock but the `Tick`, no GL. A transition never knows what it moves.

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
                self.t -= dt * 1000.0 / DIP_OUT_MS;
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
                self.t += dt * 1000.0 / DIP_IN_MS;
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
