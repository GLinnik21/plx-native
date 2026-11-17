//! **Clock debt**: the pure value step that says a direct-play (progressive Original) stream's native
//! clock is no longer keeping up with the wall, so the Feed should hold it and show Buffering instead
//! of playing in slow motion and then catching up by dropping frames.
//!
//! The controlled quantity is the debt: the sum over wall time of `wall advance - position advance`
//! of the native position callback. A stream whose link delivers less than the file needs plays in
//! slow motion once its queues are empty, and that shows as a position that advances slower than the
//! wall. This is a Page CUSUM of that debt, `S <- max(0, S + (dwall - dpos) - k * dwall)`: the debt
//! has "fired" once `S >= h`. `k` forgives a clock a little slow (a healthy stream's callbacks are
//! not perfectly regular); `h` is how much owed time a viewer is shown before the hold begins, and
//! the stream catches up on release by dropping about 24 frames per second of debt, so `h` is also
//! the cost of a hold in dropped frames.
//!
//! It is a value step of the same kind as the abr estimators, not an Event/Effect machine: a sample
//! goes in, the next state comes out, and it reads no clock, no queue and no global (the caller
//! supplies the callback's wall ms, its position, the app video queue's bytes and whether the clock
//! may be judged at all, all as data). A step that is ineligible, whose gap since the previous sample
//! exceeds [`DEBT_STALE_GAP_MS`], or whose video queue holds more than the gate, resets `S` and the
//! debt since the reset to zero: a clock that went silent is the dead-link arm's job (the silence
//! observation, `play-feed`'s `native_clock_stopped`), and a queue that still holds media is not
//! starved, so the debt is only ever counted while the queue is empty.
//!
//! Integer microseconds throughout (`k` in per mille), so a step is exact and a table test can assert
//! it to the microsecond.

/// A gap between two samples longer than this is a clock that stopped being reported, not slow
/// motion: the step restarts from the new sample.
pub const DEBT_STALE_GAP_MS: u32 = 1_000;

/// A position at or under this is a stream still starting: nothing is judged.
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
    /// What a progressive hold ships with, measured on the television: k = 0.05, h = 100 ms and a
    /// 1 MiB gate gave no false hold on a healthy 1080p or 4K file and fired within a second of the
    /// queue running dry on a starved one, at about 3.6 frames dropped per resume.
    pub const PROGRESSIVE: DebtParams = DebtParams { k_permille: 50, h_us: 100_000, gate_bytes: 1 << 20 };
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
    /// `S` reached the threshold. Latched until the step resets (an ineligible sample or a queue
    /// over the gate): the runner begins its hold from this state.
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
                if dt_ms == 0 {
                    return self; // the same callback seen twice: nothing elapsed
                }
                if dt_ms < 0 || dt_ms as u32 > DEBT_STALE_GAP_MS {
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

    const P: DebtParams = DebtParams::PROGRESSIVE;
    const S10: i64 = 10_000_000_000;
    /// More than the gate: a queue that still holds media.
    const OVER_GATE: usize = (1 << 20) + 1;

    fn at(wall_ms: u32, pos_ns: i64) -> DebtSample {
        DebtSample { wall_ms, pos_ns, video_queue_bytes: 0, eligible: true }
    }

    /// A clock that plays at `rate_permille` of the wall for `steps` samples of 200 ms each after
    /// its first, from `start`.
    fn run(start: ClockDebt, rate_permille: i64, steps: u32) -> ClockDebt {
        let mut d = start;
        for i in 0..=steps {
            d = d.step(&P, at(1_000 + i * 200, S10 + i64::from(i) * 200_000_000 * rate_permille / 1000));
        }
        d
    }

    /// The constants are the owner's measured decision; this fails if one drifts.
    #[test]
    fn the_shipped_constants_are_k_five_percent_h_a_tenth_of_a_second_and_a_mebibyte_gate() {
        assert_eq!(P, DebtParams { k_permille: 50, h_us: 100_000, gate_bytes: 1_048_576 });
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
        // rate 0.5: each 200 ms step owes 100 ms and forgives 10 ms, so S grows 90 ms a step. The
        // first sample only starts the watch; the second leaves S at 90 ms (under h = 100 ms), the
        // third at 180 ms: fired, with 200 ms of debt owed.
        assert_eq!(run(ClockDebt::Ineligible, 500, 2), ClockDebt::Fired { s_us: 180_000, debt_since_reset_us: 200_000 });
        let short = run(ClockDebt::Ineligible, 500, 1);
        assert_eq!(short, ClockDebt::Watching { wall_ms: 1_200, pos_ns: S10 + 100_000_000, s_us: 90_000, debt_since_reset_us: 100_000 });
        assert!(!short.fired());
    }

    /// `S >= h` fires: exactly h does, a microsecond under does not.
    #[test]
    fn the_threshold_is_inclusive() {
        // 200 ms of wall with 90 ms of position owes 110 ms; 10 ms forgiven: S = 100 ms = h.
        let exactly = run(ClockDebt::Ineligible, 450, 1);
        assert_eq!(exactly, ClockDebt::Fired { s_us: 100_000, debt_since_reset_us: 110_000 });
        // 91 ms of position owes 109 ms: S = 99 ms.
        let under = run(ClockDebt::Ineligible, 455, 1);
        assert_eq!(under.s_us(), 99_000);
        assert!(!under.fired());
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
        let fired = run(ClockDebt::Ineligible, 500, 2);
        assert!(fired.fired());
        // more samples, slow or not, do not change it
        assert_eq!(fired.step(&P, at(1_600, S10 + 2_000_000_000)), fired);
        // an ineligible sample (the hold's prime) clears it
        let cleared = fired.step(&P, DebtSample { eligible: false, ..at(1_600, S10) });
        assert_eq!(cleared, ClockDebt::Ineligible);
        // and so does the queue refilling past the gate
        let refilled = fired.step(&P, DebtSample { video_queue_bytes: OVER_GATE, ..at(1_600, S10) });
        assert_eq!(refilled.s_us(), 0);
        assert!(!refilled.fired());
    }

    #[test]
    fn a_queue_over_the_gate_resets_s_and_the_debt_each_sample() {
        let mut d = run(ClockDebt::Ineligible, 500, 1);
        assert_eq!(d.s_us(), 90_000);
        d = d.step(&P, DebtSample { video_queue_bytes: OVER_GATE, ..at(1_400, S10 + 300_000_000) });
        assert_eq!((d.s_us(), d.debt_since_reset_us()), (0, 0));
        // the gate is "more than 1 MiB": exactly 1 MiB is still empty enough to be judged
        let edge = d.step(&P, DebtSample { video_queue_bytes: 1 << 20, ..at(1_600, S10 + 400_000_000) });
        assert_eq!(edge.s_us(), 90_000, "100 ms owed over 200 ms, 10 ms forgiven");
    }

    #[test]
    fn ineligible_samples_and_early_positions_judge_nothing() {
        let w = run(ClockDebt::Ineligible, 500, 1);
        assert_eq!(w.step(&P, DebtSample { eligible: false, ..at(1_400, S10) }), ClockDebt::Ineligible);
        assert_eq!(w.step(&P, at(1_400, DEBT_MIN_POS_NS)), ClockDebt::Ineligible, "a stream still starting");
        assert!(matches!(
            ClockDebt::Ineligible.step(&P, at(1_400, DEBT_MIN_POS_NS + 1)),
            ClockDebt::Watching { s_us: 0, .. }
        ));
    }

    #[test]
    fn a_gap_over_a_second_or_a_clock_going_backwards_restarts_the_watch() {
        let w = run(ClockDebt::Ineligible, 500, 1);
        let ClockDebt::Watching { wall_ms, .. } = w else { panic!("{w:?}") };
        let gap = w.step(&P, at(wall_ms + DEBT_STALE_GAP_MS + 1, S10 + 5_000_000_000));
        assert!(matches!(gap, ClockDebt::Watching { s_us: 0, debt_since_reset_us: 0, .. }));
        let backwards = w.step(&P, at(wall_ms - 1, S10 + 5_000_000_000));
        assert!(matches!(backwards, ClockDebt::Watching { s_us: 0, debt_since_reset_us: 0, .. }));
        assert_eq!(w.step(&P, at(wall_ms, S10 + 5_000_000_000)), w, "the same instant twice is a no-op");
        // exactly one second is still a step: 600 ms owed, 50 ms forgiven
        let edge = w.step(&P, at(wall_ms + DEBT_STALE_GAP_MS, S10 + 500_000_000));
        assert_eq!(edge.s_us(), 90_000 + 550_000, "{edge:?}");
    }

    #[test]
    fn the_wall_clock_wrapping_is_just_a_step() {
        let a = ClockDebt::Ineligible.step(&P, at(u32::MAX - 99, S10));
        let b = a.step(&P, at(100, S10 + 100_000_000));
        assert_eq!(b.s_us(), 90_000, "200 ms of wall across the wrap, 100 ms of position, 10 ms forgiven: {b:?}");
    }

    #[test]
    fn a_catch_up_pays_the_debt_back_but_s_never_goes_below_zero() {
        let slow = run(ClockDebt::Ineligible, 500, 1);
        let ClockDebt::Watching { wall_ms, pos_ns, s_us, .. } = slow else { panic!("{slow:?}") };
        // position jumps 2 s in one step: a big negative debt
        let caught = slow.step(&P, at(wall_ms + 200, pos_ns + 2_000_000_000));
        assert_eq!(caught.s_us(), 0, "S floors at zero (was {s_us})");
        assert!(caught.debt_since_reset_us() < 0);
    }
}
