//! **Clock debt**: the pure value step that says a direct-play (progressive) stream's native clock is
//! no longer keeping up with the wall (scratch build `diag/progressive-v5-hold`, plan v5 D1/D2a).
//!
//! The controlled quantity is the debt, the sum over wall time of `wall advance - position advance`
//! of the native position callback. A stream whose link delivers less than the file needs plays in
//! slow motion once its queues are empty, and that shows as a position that advances slower than the
//! wall. This is a Page CUSUM of that debt: `S <- max(0, S + (dwall - dpos) - k * dwall)`; the debt
//! has "fired" once `S >= h`.
//!
//! It is a value step of the same kind as the abr estimators, not an Event/Effect machine: a sample
//! goes in, the next state comes out, and it reads no clock, no queue and no global. The caller
//! supplies the callback's wall ms, the callback's position, the app video queue's bytes (the
//! "gate") and whether the clock is eligible to be judged (not paused, not priming, no seek, not at a
//! non-unity rate, ...), all as data. A step that is ineligible, whose gap since the previous sample
//! exceeds [`DEBT_MAX_GAP_MS`], or whose video queue holds more than the gate, resets `S` and the debt
//! since the reset to zero: a clock that is silent is the dead-link arm's job, and a queue that still
//! holds media is not starved.
//!
//! Integer microseconds throughout (`k` in per mille), so a step is exact and a table test can assert
//! it to the microsecond.

/// A gap between two samples longer than this is a clock that stopped being reported, not slow
/// motion: the step restarts from the new sample.
pub const DEBT_MAX_GAP_MS: u32 = 1_000;

/// A position at or under this is a stream still starting (the replay's `pos > 2 s` rule).
pub const DEBT_MIN_POS_NS: i64 = 2_000_000_000;

/// The constants of the step: the drift `k`, the threshold `h` and the queue-empty gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebtParams {
    /// Drift, in per mille of wall time forgiven per step (`50` is k = 0.05).
    pub k_permille: i64,
    /// The threshold, microseconds of `S`.
    pub h_us: i64,
    /// Video queue bytes above which the stream is not starved: the step resets.
    pub gate_bytes: usize,
}

impl DebtParams {
    /// The replay's constants: k = 0.05, h = 0.4 s, gate = 1 MiB.
    pub const REPLAY: DebtParams = DebtParams { k_permille: 50, h_us: 400_000, gate_bytes: 1 << 20 };
}

/// What the caller observed at one native position callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebtSample {
    /// The callback's own time, in the caller's monotonic ms (wrapping `u32`).
    pub wall_ms: u32,
    /// The position the callback reported, film time ns.
    pub pos_ns: i64,
    /// Bytes the app's video queue holds at the callback.
    pub video_queue_bytes: usize,
    /// The clock may be judged: not paused, not priming, no seek, unity rate, a picture shown, ...
    pub eligible: bool,
}

/// The state between two samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ClockDebt {
    /// No usable previous sample: the next eligible one only starts the watch.
    #[default]
    Ineligible,
    /// Watching: the previous sample, the CUSUM `S` and the debt since the last reset, microseconds.
    Watching { wall_ms: u32, pos_ns: i64, s_us: i64, debt_since_reset_us: i64 },
    /// `S` reached the threshold. Latched until the step resets (an ineligible sample, a gap, or a
    /// queue over the gate): the runner begins its hold from this state.
    Fired { s_us: i64, debt_since_reset_us: i64 },
}

impl ClockDebt {
    /// The next state after one sample.
    pub fn step(self, p: &DebtParams, s: DebtSample) -> ClockDebt {
        if !s.eligible || s.pos_ns <= DEBT_MIN_POS_NS {
            return ClockDebt::Ineligible;
        }
        let start = ClockDebt::Watching { wall_ms: s.wall_ms, pos_ns: s.pos_ns, s_us: 0, debt_since_reset_us: 0 };
        if s.video_queue_bytes > p.gate_bytes {
            return start;
        }
        match self {
            ClockDebt::Ineligible => start,
            ClockDebt::Fired { .. } => self,
            ClockDebt::Watching { wall_ms, pos_ns, s_us, debt_since_reset_us } => {
                let dt_ms = s.wall_ms.wrapping_sub(wall_ms) as i32;
                if dt_ms <= 0 || dt_ms as u32 > DEBT_MAX_GAP_MS {
                    return start;
                }
                let dt_ms = i64::from(dt_ms);
                let debt_us = dt_ms * 1_000 - (s.pos_ns - pos_ns) / 1_000;
                let s_us = (s_us + debt_us - p.k_permille * dt_ms).max(0);
                let debt_since_reset_us = debt_since_reset_us + debt_us;
                if s_us >= p.h_us {
                    ClockDebt::Fired { s_us, debt_since_reset_us }
                } else {
                    ClockDebt::Watching { wall_ms: s.wall_ms, pos_ns: s.pos_ns, s_us, debt_since_reset_us }
                }
            }
        }
    }

    /// Has the debt crossed the threshold?
    pub const fn fired(self) -> bool {
        matches!(self, ClockDebt::Fired { .. })
    }

    /// `S` in microseconds (0 when not watching).
    pub const fn s_us(self) -> i64 {
        match self {
            ClockDebt::Ineligible => 0,
            ClockDebt::Watching { s_us, .. } | ClockDebt::Fired { s_us, .. } => s_us,
        }
    }

    /// The debt since the gate (or an ineligible sample) last reset, microseconds.
    pub const fn debt_since_reset_us(self) -> i64 {
        match self {
            ClockDebt::Ineligible => 0,
            ClockDebt::Watching { debt_since_reset_us, .. } | ClockDebt::Fired { debt_since_reset_us, .. } => {
                debt_since_reset_us
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: DebtParams = DebtParams::REPLAY;
    const S10: i64 = 10_000_000_000;

    fn at(wall_ms: u32, pos_ns: i64) -> DebtSample {
        DebtSample { wall_ms, pos_ns, video_queue_bytes: 0, eligible: true }
    }

    /// A clock that plays at `rate` of the wall for `steps` samples of 200 ms each, from `start`.
    fn run(start: ClockDebt, rate_permille: i64, steps: u32) -> ClockDebt {
        let mut d = start;
        for i in 0..=steps {
            d = d.step(&P, at(1_000 + i * 200, S10 + i64::from(i) * 200_000_000 * rate_permille / 1000));
        }
        d
    }

    #[test]
    fn a_clock_at_unity_never_accrues_and_never_fires() {
        let d = run(ClockDebt::Ineligible, 1000, 500);
        assert!(!d.fired());
        assert_eq!(d.s_us(), 0);
        assert_eq!(d.debt_since_reset_us(), 0);
    }

    #[test]
    fn a_slow_clock_fires_at_h_and_reports_s_and_the_debt() {
        // rate 0.5: each 200 ms step owes 100 ms, forgives 10 ms: S grows 90 ms a step. 0.4 s is the
        // fifth step (the first sample only starts the watch): 5 * 90 = 450 ms.
        let d = run(ClockDebt::Ineligible, 500, 5);
        assert_eq!(d, ClockDebt::Fired { s_us: 450_000, debt_since_reset_us: 500_000 });
        // one step short does not fire
        let short = run(ClockDebt::Ineligible, 500, 4);
        assert_eq!(short.s_us(), 360_000);
        assert!(!short.fired());
    }

    #[test]
    fn drift_forgives_a_clock_a_little_slow() {
        // rate 0.96: owes 8 ms a step, forgives 10 ms: S stays 0 while the debt accumulates.
        let d = run(ClockDebt::Ineligible, 960, 400);
        assert_eq!(d.s_us(), 0);
        assert!(!d.fired());
        assert_eq!(d.debt_since_reset_us(), 400 * 8_000);
    }

    #[test]
    fn a_fired_debt_stays_fired_until_something_resets_it() {
        let fired = run(ClockDebt::Ineligible, 500, 5);
        // more slow samples do not change it
        assert_eq!(fired.step(&P, at(2_200, S10 + 2_000_000_000)), fired);
        // an ineligible sample (the hold's prime) clears it
        let cleared = fired.step(&P, DebtSample { eligible: false, ..at(2_400, S10) });
        assert_eq!(cleared, ClockDebt::Ineligible);
        // and so does the queue refilling past the gate
        let refilled = fired.step(&P, DebtSample { video_queue_bytes: (1 << 20) + 1, ..at(2_400, S10) });
        assert_eq!(refilled.s_us(), 0);
        assert!(!refilled.fired());
    }

    #[test]
    fn a_full_queue_resets_s_and_the_debt_each_sample() {
        let mut d = run(ClockDebt::Ineligible, 500, 3);
        assert!(d.s_us() > 0);
        d = d.step(&P, DebtSample { video_queue_bytes: (1 << 20) + 1, ..at(1_800, S10 + 300_000_000) });
        assert_eq!((d.s_us(), d.debt_since_reset_us()), (0, 0));
        // the gate is "more than 1 MiB": exactly 1 MiB is still empty enough
        let edge = d.step(&P, DebtSample { video_queue_bytes: 1 << 20, ..at(2_000, S10 + 400_000_000) });
        assert_eq!(edge.s_us(), 90_000, "100 ms owed over 200 ms, 10 ms forgiven");
    }

    #[test]
    fn ineligible_samples_and_early_positions_judge_nothing() {
        let w = run(ClockDebt::Ineligible, 500, 2);
        assert_eq!(w.step(&P, DebtSample { eligible: false, ..at(1_600, S10) }), ClockDebt::Ineligible);
        assert_eq!(w.step(&P, at(1_600, DEBT_MIN_POS_NS)), ClockDebt::Ineligible, "a stream still starting");
        assert!(matches!(
            ClockDebt::Ineligible.step(&P, at(1_600, DEBT_MIN_POS_NS + 1)),
            ClockDebt::Watching { s_us: 0, .. }
        ));
    }

    #[test]
    fn a_gap_over_a_second_or_a_repeated_instant_restarts_the_watch() {
        let w = run(ClockDebt::Ineligible, 500, 2);
        let ClockDebt::Watching { wall_ms, .. } = w else { panic!("{w:?}") };
        let gap = w.step(&P, at(wall_ms + DEBT_MAX_GAP_MS + 1, S10 + 5_000_000_000));
        assert!(matches!(gap, ClockDebt::Watching { s_us: 0, debt_since_reset_us: 0, .. }));
        let same = w.step(&P, at(wall_ms, S10 + 5_000_000_000));
        assert!(matches!(same, ClockDebt::Watching { s_us: 0, debt_since_reset_us: 0, .. }));
        // exactly one second is still a step
        let edge = w.step(&P, at(wall_ms + DEBT_MAX_GAP_MS, S10 + 400_000_000));
        assert!(edge.s_us() > 0, "{edge:?}");
    }

    #[test]
    fn the_wall_clock_wrapping_is_just_a_step() {
        let a = ClockDebt::Ineligible.step(&P, at(u32::MAX - 99, S10));
        let b = a.step(&P, at(100, S10 + 100_000_000));
        assert_eq!(b.s_us(), 90_000, "200 ms of wall across the wrap, 100 ms of position, 10 ms forgiven: {b:?}");
    }

    #[test]
    fn a_catch_up_pays_the_debt_back_but_s_never_goes_below_zero() {
        let slow = run(ClockDebt::Ineligible, 500, 3);
        let ClockDebt::Watching { wall_ms, pos_ns, s_us, .. } = slow else { panic!("{slow:?}") };
        // position jumps 2 s in one step: a big negative debt
        let caught = slow.step(&P, at(wall_ms + 200, pos_ns + 2_000_000_000));
        assert_eq!(caught.s_us(), 0, "S floors at zero (was {s_us})");
        assert!(caught.debt_since_reset_us() < 0);
    }
}
