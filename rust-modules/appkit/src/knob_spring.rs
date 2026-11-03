//! **The scrub's way back**: the knob's spring to the playhead when a scrub is cancelled.
//!
//! PURE — a value of time in, a number out — so the choreography is tested without a GL context,
//! like [`crate::scrub_preview::Motion`] (whose [`Ramp`] and [`Ease`] it reuses rather than growing
//! a second easing). `HudMotion` owns one; the HUD draws what it answers.
//!
//! A cancelled scrub ends with the published preview position gone and the playhead where it
//! always was, so the knob would jump. [`KnobSpring`] remembers where the preview last was and
//! eases from there to wherever the bar then shows. A COMMITTED scrub ends the same way and needs
//! no case of its own: the bar then shows the seek target, which is where the preview was, so the
//! ease has nothing to travel.

use crate::scrub_preview::{Ease, Ramp};

/// The knob's return to the playhead after a cancelled scrub.
pub const SPRING_MS: u32 = 280;

/// The knob's return to the playhead after a scrub ends (see the module doc).
#[derive(Clone, Copy, Debug)]
pub struct KnobSpring {
    /// the preview position as of the last frame a scrub was live; `-1` = no scrub is live
    live_ns: i64,
    /// where the spring started from
    from_ns: i64,
    ramp: Ramp,
}

impl Default for KnobSpring {
    fn default() -> Self {
        Self::new()
    }
}

impl KnobSpring {
    pub const fn new() -> Self {
        Self { live_ns: -1, from_ns: 0, ramp: Ramp::rest(1.0) }
    }

    /// Once per frame, with the PUBLISHED scrub position (`-1` = none). The frame the scrub ends
    /// starts the spring from where the preview last was.
    pub fn step(&mut self, scrub_ns: i64, now: u32) {
        if scrub_ns >= 0 {
            self.live_ns = scrub_ns;
            self.ramp = Ramp::rest(1.0);
        } else if self.live_ns >= 0 {
            self.from_ns = self.live_ns;
            self.live_ns = -1;
            self.ramp = Ramp::new(0.0, 1.0, now, SPRING_MS, Ease::Out);
        }
    }

    /// The position the bar draws: `shown_ns` (what it would show with no spring) once the spring is
    /// done, and a point between where the preview was and `shown_ns` until then. It eases toward
    /// the CURRENT `shown_ns`, so a playhead that keeps moving is followed, not passed.
    pub fn apply(&self, shown_ns: i64, now: u32) -> i64 {
        let t = self.ramp.at(now);
        if t >= 1.0 {
            return shown_ns;
        }
        self.from_ns + ((shown_ns - self.from_ns) as f64 * f64::from(t)) as i64
    }

    /// Does the spring still move at `now`?
    pub fn busy(&self, now: u32) -> bool {
        self.ramp.busy(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_knob_springs_from_the_last_preview_to_the_playhead_in_about_280_ms() {
        let mut s = KnobSpring::new();
        s.step(60_000_000_000, 1_000);
        s.step(80_000_000_000, 1_016);
        assert_eq!(s.apply(80_000_000_000, 1_016), 80_000_000_000, "while live the bar shows the preview");
        // the scrub is cancelled: the published position is gone, the playhead is at 10 s
        s.step(-1, 1_032);
        let head = 10_000_000_000;
        assert_eq!(s.apply(head, 1_032), 80_000_000_000, "the first frame is still where the knob was");
        let mid = s.apply(head, 1_032 + 140);
        assert!(mid < 80_000_000_000 && mid > head, "half-way: {mid}");
        assert_eq!(s.apply(head, 1_032 + 280), head, "home after 280 ms");
        assert!(s.busy(1_032 + 100) && !s.busy(1_032 + 280));
        let path: Vec<i64> = (0..=280).step_by(20).map(|d| s.apply(head, 1_032 + d)).collect();
        assert!(path.windows(2).all(|w| w[0] >= w[1]), "monotone toward the playhead: {path:?}");
    }

    #[test]
    fn a_committed_scrub_has_nothing_to_spring() {
        let mut s = KnobSpring::new();
        s.step(80_000_000_000, 1_000);
        s.step(-1, 1_016);
        // the bar then shows the seek target, which is where the preview was
        for d in [0, 50, 140, 280, 400] {
            assert_eq!(s.apply(80_000_000_000, 1_016 + d), 80_000_000_000, "+{d} ms");
        }
    }

    #[test]
    fn a_new_scrub_cancels_a_spring_in_flight() {
        let mut s = KnobSpring::new();
        s.step(80_000_000_000, 1_000);
        s.step(-1, 1_016);
        s.step(30_000_000_000, 1_100);
        assert_eq!(s.apply(30_000_000_000, 1_100), 30_000_000_000);
        assert!(!s.busy(1_100));
    }

    #[test]
    fn an_idle_spring_never_moves_the_bar() {
        let s = KnobSpring::new();
        assert_eq!(s.apply(42, 0), 42);
        assert!(!s.busy(0));
    }
}
