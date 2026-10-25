//! **Per-visit frame health: how the app FELT on one screen, as four coarse classes.**
//!
//! The usage event `route.left` (`telemetry::diag::schema`) says, once per screen visit, whether
//! the app was smooth there. Nothing in it is content: it counts frames, stalls and dead key
//! presses, and reports each as a bucket, never as the number.
//!
//! The definitions are Datadog Mobile Vitals', adapted to a television that swaps at a fixed
//! refresh:
//!
//! * a **slow frame** is a presented frame whose interval from the previous presented frame is over
//!   [`SLOW_FRAME_US`]. The vsync budget is 16.7 ms, but a swap that blocks on vsync measures
//!   16.7 ms plus clock noise, so a strict `> 16.67 ms` reads a healthy screen as mostly slow (the
//!   host stress run in `docs/measurements/global-image-cache-2026-09-26.json` has `gt16` at 63%
//!   of its intervals). The edge is the project's existing "late frame" (`UPLOAD_LATE_MS`, the
//!   next refresh slot after a 16.7 ms frame plus a 5 ms hitch): a frame that missed a vsync
//!   (33 ms at 60 Hz) is slow;
//! * a **frozen frame** is one over [`FROZEN_FRAME_US`] (700 ms). A frozen frame is also slow;
//! * a **hang** is a main-loop iteration that took over [`STALL_US`] (250 ms, the watchdog's own
//!   threshold), and the rate is the seconds of such stalls per hour of the visit.
//!
//! **The player route is measured like any other.** Its graphics plane is NOT paced at the video's
//! 24/30/50 Hz: while the video plane is bound the idle gate presents every iteration
//! (`plx_machine::idle::should_present`), the swap interval is 1 (`SDL_GL_SetSwapInterval`), and
//! the heartbeat's `fps=` reads 60 through playback (the decoded picture is composited by the
//! television on a surface this process never touches, `vtick=` is its own ~5 Hz liveness). So a
//! player frame over the slow edge is as late as any other and no route is exempt. If a device
//! trace ever shows the plane paced at the video rate, exclude `player` from the slow count
//! here (keeping frozen and hang), not at the call site.
//!
//! It runs in EVERY build, unarmed, because it needs one clock read per presented frame and one
//! per loop iteration ([`super::now_us`], the performance counter: `clock::now()` is whole
//! milliseconds, and a perfect 16.67 ms interval reads as 17 about two thirds of the time).
//!
//! The cadence rule is `Instruments`' own: an idle gap is not a sample ([`FrameHealth::idle`]),
//! a deferred present is (`Instruments::defer_present_phases` simply does not break the chain).
//! Nothing is counted before the first present (shader and font warm-up legitimately exceed every
//! threshold here, as in the watchdog), nor while the app is in the background, nor across a gap
//! of [`ASLEEP_US`] (the television slept; the app did not hang).

use crate::task::watchdog::HANG_THRESHOLD_MS;

/// A presented frame over this interval is slow. See the module doc for why it is not 16.7 ms.
pub const SLOW_FRAME_US: u64 = super::UPLOAD_LATE_MS * 1_000;
/// A presented frame over this interval is frozen.
pub const FROZEN_FRAME_US: u64 = 700_000;
/// A loop iteration over this long is a stall, and counts toward the hang rate.
pub const STALL_US: u64 = HANG_THRESHOLD_MS * 1_000;
/// A gap this long between two iterations is the television asleep, not the app unresponsive.
const ASLEEP_US: u64 = 30_000_000;
/// A visit shorter than this reports nothing: a per-hour rate and a share of frames mean nothing
/// over a second or two of a page transition, and the next screen's visit begins at once.
pub const MIN_VISIT_US: u64 = 5_000_000;
/// The slow share is taken over at least this many frames (one second at 60 Hz), so a settled
/// screen that presented a handful of frames cannot read `20%+` from one late one.
const SLOW_DENOMINATOR_FLOOR: u32 = 60;

/// What one visit has accumulated so far.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Visit {
    frames: u32,
    slow: u32,
    frozen: u32,
    stall_us: u64,
    active_us: u64,
}

/// The four classes of one finished visit, each a closed word of the `route.left` row of
/// `schema::EVENT_SPECS`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HealthClasses {
    pub slow_frames: &'static str,
    pub frozen_frames: &'static str,
    pub hang: &'static str,
    pub dead_presses: &'static str,
}

/// The accumulator. Pure: every method takes the clock as an argument, so a test injects times.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameHealth {
    visit: Visit,
    prev_present: Option<u64>,
    prev_iteration: Option<u64>,
    /// The frame `presented` counted in THIS iteration (slow, frozen), for [`Self::finish_at_transition`].
    this_iteration_frame: Option<(bool, bool)>,
    warm: bool,
    suspended: bool,
}

impl FrameHealth {
    /// The top of a loop iteration, `now_us` on the performance counter.
    pub fn iteration(&mut self, now_us: u64) {
        if self.suspended {
            return;
        }
        self.this_iteration_frame = None;
        let Some(prev) = self.prev_iteration.replace(now_us) else { return };
        let gap = now_us.saturating_sub(prev);
        if gap >= ASLEEP_US || !self.warm {
            return;
        }
        self.visit.active_us += gap;
        if gap > STALL_US {
            self.visit.stall_us += gap;
        }
    }

    /// A frame was presented (the swap returned) at `now_us`.
    pub fn presented(&mut self, now_us: u64) {
        if self.suspended {
            return;
        }
        let was_warm = std::mem::replace(&mut self.warm, true);
        // After an idle gap the chain has no previous present, and the first frame is the one
        // that ANSWERS the key press (a focus move, a nav commit, a cold open): the frame whose
        // lateness the viewer feels. It is measured from the top of this iteration, so the idle
        // gap before it is not in it. Not before the first present ever (warm-up).
        let prev = match self.prev_present.replace(now_us) {
            Some(prev) => prev,
            None if was_warm => match self.prev_iteration {
                Some(top) => top,
                None => return,
            },
            None => return,
        };
        let interval = now_us.saturating_sub(prev);
        let (slow, frozen) = (interval > SLOW_FRAME_US, interval > FROZEN_FRAME_US);
        self.visit.frames += 1;
        self.visit.slow += u32::from(slow);
        self.visit.frozen += u32::from(frozen);
        self.this_iteration_frame = Some((slow, frozen));
    }

    /// An iteration that skipped presenting: the gap until the next present is idle, not a frame.
    pub fn idle(&mut self) {
        self.prev_present = None;
    }

    /// End the visit at a ROUTE CHANGE seen after this iteration's present. That frame is the
    /// first of the NEW screen (the route was committed before it was drawn: the cost of opening
    /// it), and this iteration's stall is added at the next [`Self::iteration`], after the reset,
    /// to the new visit; so the frame moves with it and one stall is never split across two
    /// screens. Otherwise as [`Self::finish`].
    pub fn finish_at_transition(&mut self, dead_bursts: u32) -> Option<HealthClasses> {
        let moved = self.this_iteration_frame.take();
        if let Some((slow, frozen)) = moved {
            self.visit.frames -= 1;
            self.visit.slow -= u32::from(slow);
            self.visit.frozen -= u32::from(frozen);
        }
        let classes = self.finish(dead_bursts);
        if let Some((slow, frozen)) = moved {
            self.visit.frames = 1;
            self.visit.slow = u32::from(slow);
            self.visit.frozen = u32::from(frozen);
        }
        classes
    }

    /// The app left (`true`) or re-entered (`false`) the foreground. Neither edge leaves a chain
    /// to measure across, and nothing counts in between.
    pub fn suspend(&mut self, suspended: bool) {
        self.suspended = suspended;
        self.this_iteration_frame = None;
        self.idle();
        self.prev_iteration = None;
    }

    /// End the visit: the classes to report (`None` for one too short to mean anything), with
    /// `dead_bursts` the repeated-key bursts the dispatcher counted over the same visit. Either
    /// way the next visit starts from nothing.
    pub fn finish(&mut self, dead_bursts: u32) -> Option<HealthClasses> {
        let v = std::mem::take(&mut self.visit);
        (v.active_us >= MIN_VISIT_US).then(|| HealthClasses {
            slow_frames: slow_class(v.slow, v.frames),
            frozen_frames: frozen_class(v.frozen),
            hang: hang_class(v.stall_us, v.active_us),
            dead_presses: dead_class(dead_bursts),
        })
    }
}

/// The slow share of presented frames: `0` / `<1%` / `1-5%` / `5-20%` / `20%+`. Integer
/// cross-products, so an edge is exact (1% is `1-5%`, 5% is `5-20%`, 20% is `20%+`).
fn slow_class(slow: u32, frames: u32) -> &'static str {
    let (slow, frames) = (u64::from(slow) * 100, u64::from(frames.max(SLOW_DENOMINATOR_FLOOR)));
    match slow {
        0 => "0",
        s if s < frames => "<1%",
        s if s < 5 * frames => "1-5%",
        s if s < 20 * frames => "5-20%",
        _ => "20%+",
    }
}

fn frozen_class(n: u32) -> &'static str {
    match n {
        0 => "0",
        1 => "1",
        2..=3 => "2-3",
        _ => "4+",
    }
}

/// Seconds of stall per hour of the visit: `0` / `<1` / `1-10` / `10+`. The rate is
/// `stall_us * 3600 / active_us`; the comparisons are cross-multiplied to stay exact.
fn hang_class(stall_us: u64, active_us: u64) -> &'static str {
    let per_hour = u128::from(stall_us) * 3_600;
    let active = u128::from(active_us);
    match stall_us {
        0 => "0",
        _ if per_hour < active => "<1",
        _ if per_hour <= 10 * active => "1-10",
        _ => "10+",
    }
}

fn dead_class(n: u32) -> &'static str {
    match n {
        0 => "0",
        1 => "1",
        _ => "2+",
    }
}

/// Presses of one key that must land with nothing happening for the burst to count.
const DEAD_BURST_PRESSES: usize = 4;
/// …within this long (ms), first to last.
const DEAD_BURST_WINDOW_MS: u32 = 1_000;

/// **The repeated-key ("frustration") signal: the same key pressed four times within a second,
/// and none of the four did anything.** Datadog's rage click, for a remote.
///
/// The dispatcher feeds it from the one place every key a page can take goes through
/// (`ui::dispatch`'s delivery of a key to the input owner), not from a screen: a press that moved
/// focus, was handled by the screen, armed a press or went back is [`Self::acted`] and clears the
/// run; one nobody took is [`Self::dead`]. A different key ends the run too. Only the DOWN edge is
/// ever reported, so a held arrow (a stream of repeat edges) is not four presses.
///
/// A burst counts once: after the fourth dead press the run starts over, so eight dead presses in
/// a second are two bursts, and one long stretch of mashing is not a thousand. The player page
/// answers its own keys outside the dispatcher's focus engine and reports nothing here.
#[derive(Clone, Copy, Debug)]
pub struct DeadPresses<K> {
    key: Option<K>,
    at_ms: [u32; DEAD_BURST_PRESSES],
    len: usize,
    bursts: u32,
}

impl<K: Copy + PartialEq> Default for DeadPresses<K> {
    fn default() -> Self {
        Self { key: None, at_ms: [0; DEAD_BURST_PRESSES], len: 0, bursts: 0 }
    }
}

impl<K: Copy + PartialEq> DeadPresses<K> {
    /// `key` was pressed at `now_ms` and nothing took it.
    pub fn dead(&mut self, key: K, now_ms: u32) {
        if self.key != Some(key) {
            self.len = 0;
            self.key = Some(key);
        }
        // Presses more than a window older than this one cannot be in its burst.
        let keep = (0..self.len)
            .rev()
            .take_while(|&i| now_ms.wrapping_sub(self.at_ms[i]) <= DEAD_BURST_WINDOW_MS)
            .count();
        self.at_ms.copy_within(self.len - keep..self.len, 0);
        self.len = keep;
        self.at_ms[self.len] = now_ms;
        self.len += 1;
        if self.len == DEAD_BURST_PRESSES {
            self.bursts += 1;
            self.len = 0;
        }
    }

    /// A press did something (or the page ended it): whatever run was building is over.
    pub fn acted(&mut self) {
        self.len = 0;
        self.key = None;
    }

    /// The bursts counted since the last read; resets the count (the run in progress is kept).
    pub fn take(&mut self) -> u32 {
        std::mem::take(&mut self.bursts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One 60 Hz iteration, in microseconds.
    const STEP: u64 = 16_000;

    /// A warm accumulator with no frame chain and no pending iteration gap.
    fn warm() -> FrameHealth {
        let mut h = FrameHealth::default();
        h.presented(0);
        h.idle();
        h.iteration(0);
        h
    }

    /// Loop iterations at 60 Hz from `from` to `to`; returns the last stamp.
    fn walk(h: &mut FrameHealth, from: u64, to: u64) -> u64 {
        let mut t = from;
        while t + STEP <= to {
            t += STEP;
            h.iteration(t);
        }
        t
    }

    #[test]
    fn the_slow_edge_is_the_late_frame_not_the_vsync_budget() {
        for (interval, slow) in [(16_670, 0), (21_999, 0), (22_000, 0), (22_001, 1), (33_333, 1)] {
            let mut h = warm();
            h.prev_present = Some(1_000_000);
            h.presented(1_000_000 + interval);
            assert_eq!((h.visit.frames, h.visit.slow), (1, slow), "{interval} us");
        }
    }

    #[test]
    fn the_frozen_edge_is_700_ms_and_a_frozen_frame_is_also_slow() {
        for (interval, frozen) in [(699_999, 0), (700_000, 0), (700_001, 1)] {
            let mut h = warm();
            h.prev_present = Some(1_000_000);
            h.presented(1_000_000 + interval);
            assert_eq!(h.visit.frozen, frozen, "{interval} us");
            assert_eq!(h.visit.slow, 1, "{interval} us is slow either way");
        }
    }

    #[test]
    fn the_stall_edge_is_250_ms() {
        for (gap, stall) in [(249_999, 0), (250_000, 0), (250_001, 250_001)] {
            let mut h = warm();
            h.iteration(gap);
            assert_eq!(h.visit.stall_us, stall, "{gap} us");
            assert_eq!(h.visit.active_us, gap);
        }
    }

    #[test]
    fn an_idle_gap_is_not_a_frame_and_a_deferral_is() {
        let mut h = warm();
        h.idle();
        h.iteration(9_000_000 - 16_000);
        h.presented(9_000_000);
        assert_eq!((h.visit.frames, h.visit.slow), (1, 0), "the answering frame is counted from its iteration's top, not from the idle gap");
        h.presented(9_000_000 + 40_000);
        assert_eq!((h.visit.frames, h.visit.slow), (2, 1));
        // A deferred present never calls `idle`: the chain spans it.
        h.presented(9_000_000 + 40_000 + 100_000);
        assert_eq!((h.visit.frames, h.visit.slow), (3, 2));
    }

    #[test]
    fn the_frame_that_answers_a_key_after_idle_is_counted() {
        let mut h = warm();
        h.idle();
        // A settled screen, then a key: the iteration starts, and its commit frame takes 900 ms.
        h.iteration(20_000_000);
        h.presented(20_900_000);
        assert_eq!((h.visit.frames, h.visit.slow, h.visit.frozen), (1, 1, 1));
    }

    #[test]
    fn nothing_is_counted_for_the_very_first_present() {
        let mut h = FrameHealth::default();
        h.iteration(0);
        h.presented(900_000);
        assert_eq!(h.visit.frames, 0, "warm-up is never a frame");
    }

    #[test]
    fn a_transition_frame_and_its_stall_land_on_the_new_visit() {
        let mut h = warm();
        h.iteration(6_000_000);
        h.presented(6_016_000);
        // The next iteration's top: the committing iteration took 800 ms (frame and stall).
        h.iteration(6_800_000);
        h.presented(6_800_000 + 800_000); // the frame that opened the new screen: 800 ms late
        let _ = h.finish_at_transition(0); // route change seen in this iteration's report
        assert_eq!(
            (h.visit.frames, h.visit.slow, h.visit.frozen),
            (1, 1, 1),
            "the transition frame starts the new visit"
        );
        h.iteration(7_700_000);
        assert_eq!(h.visit.stall_us, 900_000, "and so does its stall");
    }

    #[test]
    fn nothing_counts_before_the_first_present_or_while_suspended_or_asleep() {
        let mut h = FrameHealth::default();
        h.iteration(0);
        h.iteration(5_000_000); // shader and font warm-up
        assert_eq!(h.visit, Visit::default());
        h.presented(5_000_000);
        h.iteration(5_100_000);
        assert_eq!(h.visit.active_us, 100_000);

        h.suspend(true);
        h.iteration(900_000_000);
        h.presented(900_000_000);
        h.presented(900_020_000);
        assert_eq!(h.visit.frames, 0, "no frame is counted in the background");
        h.suspend(false);
        h.iteration(950_000_000);
        assert_eq!(h.visit.active_us, 100_000, "the first iteration back has no previous to span");

        h.iteration(950_000_000 + ASLEEP_US);
        assert_eq!(h.visit.stall_us, 0, "a television asleep is not a hang");
        assert_eq!(h.visit.active_us, 100_000);
    }

    #[test]
    fn a_short_visit_reports_nothing_and_every_finish_starts_over() {
        let mut h = warm();
        let t = walk(&mut h, 0, MIN_VISIT_US - STEP);
        assert!(t < MIN_VISIT_US);
        assert_eq!(h.finish(0), None);
        let mut h = warm();
        h.iteration(MIN_VISIT_US);
        assert!(h.finish(0).is_some());
        assert_eq!(h.visit, Visit::default());
        assert_eq!(h.finish(0), None, "the next visit begins empty");
    }

    #[test]
    fn a_smooth_visit_reads_all_zero() {
        let mut h = warm();
        let mut t = 0;
        let mut present = 0;
        while t < 8_000_000 {
            t += 16_667;
            present += 16_667;
            h.iteration(t);
            h.presented(present);
        }
        let c = h.finish(0).unwrap();
        assert_eq!((c.slow_frames, c.frozen_frames, c.hang, c.dead_presses), ("0", "0", "0", "0"));
    }

    #[test]
    fn slow_share_buckets_have_exact_edges() {
        // 1000 frames: 9 is 0.9%, 10 is 1%, 49 is 4.9%, 50 is 5%, 199 is 19.9%, 200 is 20%.
        for (slow, class) in [(0, "0"), (1, "<1%"), (9, "<1%"), (10, "1-5%"), (49, "1-5%"),
            (50, "5-20%"), (199, "5-20%"), (200, "20%+"), (1000, "20%+")] {
            assert_eq!(slow_class(slow, 1000), class, "{slow}/1000");
        }
    }

    #[test]
    fn a_screen_that_barely_drew_cannot_read_twenty_percent_from_one_late_frame() {
        assert_eq!(slow_class(1, 3), "1-5%", "1 of 3 frames is read over a floor of 60");
        assert_eq!(slow_class(12, 3), "20%+");
    }

    #[test]
    fn frozen_and_dead_buckets() {
        for (n, c) in [(0, "0"), (1, "1"), (2, "2-3"), (3, "2-3"), (4, "4+"), (400, "4+")] {
            assert_eq!(frozen_class(n), c);
        }
        for (n, c) in [(0, "0"), (1, "1"), (2, "2+"), (9, "2+")] {
            assert_eq!(dead_class(n), c);
        }
    }

    #[test]
    fn hang_rate_buckets_have_exact_edges() {
        // A one-hour visit: the rate in s/h is the stall in seconds.
        let hour = 3_600_000_000;
        for (stall_us, class) in [(0, "0"), (1, "<1"), (999_999, "<1"), (1_000_000, "1-10"),
            (10_000_000, "1-10"), (10_000_001, "10+")] {
            assert_eq!(hang_class(stall_us, hour), class, "{stall_us} us per hour");
        }
        // The same stall over a one-minute visit is sixty times the rate.
        assert_eq!(hang_class(300_000, 60_000_000), "10+");
        assert_eq!(hang_class(60_000, 60_000_000), "1-10", "3.6 s/h");
        assert_eq!(hang_class(1_000, 60_000_000), "<1", "0.06 s/h");
    }

    #[test]
    fn three_dead_presses_are_not_a_burst_and_four_are() {
        let mut d = DeadPresses::default();
        for t in [0, 200, 400] {
            d.dead('r', t);
        }
        assert_eq!(d.take(), 0);
        d.dead('r', 600);
        assert_eq!(d.take(), 1);
    }

    #[test]
    fn four_in_a_second_is_inclusive_and_a_slower_four_is_not() {
        let mut d = DeadPresses::default();
        for t in [0, 300, 600, 1_000] {
            d.dead('r', t);
        }
        assert_eq!(d.take(), 1, "the fourth press exactly one second after the first");
        for t in [5_000, 5_400, 5_800, 6_001] {
            d.dead('r', t);
        }
        assert_eq!(d.take(), 0, "one millisecond over the window");
        // The run slides: the three newest plus one more inside a second do count.
        d.dead('r', 6_400);
        assert_eq!(d.take(), 1);
    }

    #[test]
    fn a_press_that_did_something_resets_the_run() {
        let mut d = DeadPresses::default();
        for t in [0, 100, 200] {
            d.dead('r', t);
        }
        d.acted();
        d.dead('r', 300);
        assert_eq!(d.take(), 0, "the move reset it: that is one dead press, not four");
        for t in [400, 500, 600] {
            d.dead('r', t);
        }
        assert_eq!(d.take(), 1);
    }

    #[test]
    fn a_different_key_ends_the_run() {
        let mut d = DeadPresses::default();
        for t in [0, 100, 200] {
            d.dead('r', t);
        }
        d.dead('l', 300);
        d.dead('r', 400);
        assert_eq!(d.take(), 0);
    }

    #[test]
    fn a_burst_counts_once_and_the_next_needs_four_more() {
        let mut d = DeadPresses::default();
        for t in 0..7 {
            d.dead('r', t * 10);
        }
        assert_eq!(d.take(), 1, "seven presses: one burst and three toward the next");
        d.dead('r', 70);
        assert_eq!(d.take(), 1);
        assert_eq!(d.take(), 0, "a read resets the count");
    }

    #[test]
    fn a_visit_with_a_freeze_and_a_stall_reports_both() {
        let mut h = warm();
        let t = walk(&mut h, 0, 6_000_000);
        h.presented(1_000_000);
        h.presented(1_800_000); // 800 ms between presents: frozen and slow
        h.iteration(t + 800_000); // and the loop was away for the same 800 ms
        let c = h.finish(2).unwrap();
        assert_eq!(c.frozen_frames, "1");
        assert_eq!(c.slow_frames, "1-5%", "one slow frame over the floor of 60");
        assert_eq!(c.hang, "10+");
        assert_eq!(c.dead_presses, "2+");
    }
}
