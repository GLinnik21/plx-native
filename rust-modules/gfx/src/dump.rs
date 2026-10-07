//! **Dump mode, the gfx half** (the deterministic frame dump; its landing half is
//! `plx_machine::landgate::Gate::arm_dump`).
//!
//! A dump writes frames from virtual time, so every wall-clock-dependent decision that reaches
//! logical state must be a constant. Two of them live here:
//!
//! * **Text prewarm.** [`crate::text::drain_prewarm`] spends a microsecond budget, so how many
//!   frames the queue takes to drain is CPU speed, and that hold decides the frame a modal turns
//!   `Open`. In dump mode both prewarm drains (`drain_prewarm` and the background drain) ignore
//!   the budget they are given and run to empty (the background drain still stops at its
//!   occupancy ceiling, which is logical, not timed), and
//!   [`crate::text::latch_surface_text_pending`] latches `false`, on the premise that the dump
//!   driver drains to empty before the readiness sample (the product loop's order is the reverse:
//!   it samples before the springs step and drains on the presenting side afterwards). **That is all this module guarantees, and it is per CALL.** Whether a
//!   call happens is still the callers' decision, and the ui callers make it on wall time:
//!   `ui/src/dispatch.rs`'s held-surface drain runs only while the frame's remaining microsecond
//!   budget (less what the first drain spent) is positive, and `ui/src/panel_motion.rs`'s
//!   background drain only if the time left is at least `BACKGROUND_MIN_US`. Until those gates
//!   are bypassed in dump mode, the dump driver must itself call the FULL drain (both) every
//!   iteration, before the readiness sample, and must not count on the ui callers having done
//!   so; whatever a skipped drain leaves in the queue is still readable through
//!   `prewarm_pending()` (page-image plan, `track_menu`'s `warm_other_tab`), which the latch does
//!   not cover.
//! * **Page-capture GPU readiness.** [`crate::gfx::snapshot_frame_begin`] runs `glFinish()` before
//!   it samples the capture fence and reports the capture as no longer pending, so GPU speed is
//!   never an input.
//!
//! The order within one iteration is: landing takes, then the busy/debt sample, then the draw.
//!
//! **Tests that arm it** hold `plx_base::testlock::serial()`: the switch is process-global, and
//! every gfx test that reaches a hooked function holds that lock, so nothing leaks between them.
//!
//! **A runtime switch, off by default.** [`arm`] is called by the dump driver only; nothing in a
//! shipping loop does. Unarmed, each hook costs one relaxed atomic load, and nothing else in the
//! crate changes behaviour.

use std::sync::atomic::{AtomicBool, Ordering};

static DUMP: AtomicBool = AtomicBool::new(false);

/// Arm dump mode for this process.
pub fn arm() {
    DUMP.store(true, Ordering::Relaxed);
}

/// Disarm: the ordinary behaviour, and what a host test restores.
pub fn disarm() {
    DUMP.store(false, Ordering::Relaxed);
}

/// Is dump mode armed?
pub fn armed() -> bool {
    DUMP.load(Ordering::Relaxed)
}

/// A host test's guard: dump mode is disarmed again even if an assertion panics.
#[cfg(any(test, feature = "test-support"))]
pub struct Armed;

#[cfg(any(test, feature = "test-support"))]
impl Armed {
    pub fn new() -> Self {
        arm();
        Armed
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Default for Armed {
    fn default() -> Self { Self::new() }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for Armed {
    fn drop(&mut self) {
        disarm();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text;

    fn queue(n: usize) {
        for i in 0..n {
            let s = std::ffi::CString::new(format!("dump-mode string {i}")).unwrap();
            text::queue_prewarm(s.as_ptr(), 20, 0);
        }
    }

    /// A clock that advances 1 ms per read, so any budget below the queue's length is exhausted.
    fn ticking() -> impl FnMut() -> u64 {
        let mut t = 0;
        move || {
            t += 1000;
            t
        }
    }

    #[test]
    fn unarmed_the_prewarm_drain_spends_its_budget_and_leaves_the_rest() {
        let _g = plx_base::testlock::serial();
        disarm();
        text::reset_prewarm_for_test();
        queue(8);
        let done = text::drain_prewarm(2500, ticking());
        assert!(done < 8, "a budget of 2.5 ms on a 1 ms clock cannot drain 8 strings (drained {done})");
        assert!(text::prewarm_pending());
        text::reset_prewarm_for_test();
    }

    #[test]
    fn armed_the_prewarm_drain_runs_to_empty_and_the_readiness_latch_reads_false() {
        let _g = plx_base::testlock::serial();
        text::reset_prewarm_for_test();
        let _armed = Armed::new();
        queue(8);
        assert_eq!(text::drain_prewarm(1, ticking()), 8, "no microsecond budget in dump mode");
        assert!(!text::prewarm_pending());
        // even a stale `true` sample is never an input in dump mode
        text::latch_surface_text_pending(true);
        assert!(!text::surface_text_pending());
        text::reset_prewarm_for_test();
    }

    #[test]
    fn armed_the_background_drain_has_no_time_budget_either() {
        let _g = plx_base::testlock::serial();
        text::reset_prewarm_for_test();
        let _armed = Armed::new();
        queue(6);
        text::park_prewarm_as_background();
        assert_eq!(text::drain_background_prewarm(1, ticking()), 6);
        assert!(!text::background_prewarm_pending());
        text::reset_prewarm_for_test();
    }

    #[test]
    fn the_capture_readiness_is_a_constant_in_dump_mode_and_unchanged_outside_it() {
        // fence unsignalled, nothing deferred yet: an ordinary frame defers, a dump frame never does
        assert!(crate::gfx::snapshot_defers_in(false, Some(false), 0));
        assert!(!crate::gfx::snapshot_defers_in(true, Some(false), 0));
        assert!(!crate::gfx::snapshot_defers_in(false, None, 0));
    }

    #[test]
    fn the_switch_is_off_by_default_and_the_guard_restores_it() {
        let _g = plx_base::testlock::serial();
        assert!(!armed());
        {
            let _armed = Armed::new();
            assert!(armed());
        }
        assert!(!armed());
    }
}
