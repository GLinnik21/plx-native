//! **Dump mode, the gfx half** (the deterministic frame dump; its landing half is
//! `plx_machine::landgate::Gate::arm_dump`).
//!
//! A dump writes frames from virtual time, so every wall-clock-dependent decision that reaches
//! logical state must be a constant. The ones that live here are listed below:
//!
//! * **Text prewarm.** [`crate::text::drain_prewarm`] spends a microsecond budget, so how many
//!   frames the queue takes to drain is CPU speed, and that hold decides the frame a modal turns
//!   `Open`. In dump mode both prewarm drains (`drain_prewarm` and the background drain) ignore
//!   the budget they are given and run to empty (the background drain still stops at its
//!   occupancy ceiling, which is logical, not timed), and
//!   [`crate::text::latch_surface_text_pending`] latches `false`, (the dump driver drains
//!   the full queue every iteration, after the takes and before the draw, so the queue is empty when
//!   the next iteration samples readiness; the product loop's order is the reverse: it samples
//!   before the springs step and drains on the presenting side afterwards). **That is all this module guarantees, and it is per CALL.** Whether a
//!   call happens is still the callers' decision, and the ui callers make it on wall time:
//!   `ui/src/dispatch.rs`'s held-surface drain runs only while the frame's remaining microsecond
//!   budget (less what the first drain spent) is positive, and `ui/src/panel_motion.rs`'s
//!   background drain only if the time left is at least `BACKGROUND_MIN_US`. Until those gates
//!   are bypassed in dump mode, the dump driver must itself call the FULL drain (both) every
//!   iteration, after the takes and before the draw, and must not count on the ui callers having done
//!   so; whatever a skipped drain leaves in the queue is still readable through
//!   `prewarm_pending()` (page-image plan, `track_menu`'s `warm_other_tab`), which the latch does
//!   not cover.
//! * **Page-capture GPU readiness.** [`crate::gfx::snapshot_frame_begin`] runs `glFinish()` before
//!   it samples the capture fence and reports the capture as no longer pending, so GPU speed is
//!   never an input.
//!
//! * **Ground probes.** `gfx::GroundProbe::step` (the top bar's `sample_ground` and the Hero row's
//!   `sample_control_ground` both go through it) reads on a cadence counted in PRESENTED loop
//!   iterations, held repeats included, so the virtual frame a probe re-reads on was the number of
//!   boot holds modulo 30 and the Resume pill's ground differed by 1/255 between runs. In dump mode
//!   it is SYNCHRONOUS and PER CALL (`gfx::probe_mode`): copy the tap boxes, `glReadPixels`, return;
//!   no cadence, no fence. The refusal paths (`may_read == false`, a blur-source pass, a frozen
//!   page) still answer with the last latched value.
//! * **Held repeats** ([`held_repeat`]). A page image's capture step and a page dip's
//!   floor frame advance once per virtual frame, not once per held iteration.
//!
//! The order within one iteration is: landing takes, then the busy/debt sample, then the draw.
//!
//! Which landing sites WAIT for their answer in dump mode, which still do not, and the counter the
//! driver asserts empty at the end of a run (`Gate::unconverted_takes`) are stated in
//! `plx_machine::landgate`'s module doc; this module owns only the gfx decisions listed above.
//!
//! **Tests that arm it** hold `plx_base::testlock::serial()`: the switch is process-global, and
//! every gfx test that reaches a hooked function holds that lock, so nothing leaks between them.
//! A test in another binary that arms it (`Armed::new()`) holds the lock too, and every OTHER test
//! of that binary that reaches a consumer below either holds it or calls the consumer's pure twin
//! (`evict_cooldown_due_in`, `declines_in`, `Budget::take_in`, `PageImage::step_filmed`,
//! `snapshot_defers_in`, `probe_mode`) with the switch passed as `false`. A test that reads the
//! global unguarded is not wrong today and flakes the day a neighbour arms it.
//!
//! # The admission rule
//!
//! A branch on [`armed`] or [`held_repeat`] may do exactly one of two things:
//!
//! 1. **replace a TIMING INPUT with a constant** (a wall-clock budget, a measured cadence, a GPU
//!    fence, a retry or eviction cooldown, a motion verdict computed from `dt`), or
//! 2. **make a per-iteration stepper idempotent on a held repeat** (so it advances once per written
//!    virtual frame, not once per loop iteration).
//!
//! It may **never skip a state the product passes through, or change what a settled frame shows.**
//! A frame the film writes must be a frame the app can draw. Anything that cannot be said in those
//! terms is a defect in the product's own ordering (the dump's landings are synchronous, so data is
//! present at mount, an order a television rarely has) and is answered there, not patched here one
//! branch at a time. `ci/check-dump-consumers.py` (in `make check`) holds the list of files that may
//! name the switch to `ci/allow/dump.txt`; a new file naming it fails the gate until this rule has
//! been read and the file's branch fits it (and is listed below).
//!
//! # Every consumer, and which kind it is
//!
//! The list is exact (the gate compares the files; this list says what each does). `T` is rule 1,
//! `S` is rule 2, `X` is an exception that changes the picture, kept on purpose or for want of a
//! product answer.
//!
//! * `gfx/src/text.rs` `drain_prewarm` (T): the microsecond budget becomes `u64::MAX`.
//! * `gfx/src/text.rs` `drain_background_prewarm_at` (T): the same; the occupancy ceiling stays.
//! * `gfx/src/text.rs` `latch_surface_text_pending` (T): the sampled readiness latches `false`.
//! * `gfx/src/gfx.rs` `snapshot_frame_begin` (T): `glFinish()` then the capture fence is read as
//!   signalled, so GPU speed is not an input.
//! * `gfx/src/gfx.rs` `GroundProbe::step` via `probe_mode` (**X**, a small one): the read is
//!   synchronous per call instead of on a cadence of presented iterations, so a probed tint
//!   (the top bar's and the Resume pill's ground) settles up to 30 frames earlier than the
//!   television's cadence would settle it. Kept because the cadence counts presented loop
//!   iterations, held repeats included, which is a function of how many holds a frame took and
//!   made two runs differ by 1/255; callers already refuse a read while the probe is unsettled.
//! * `ui/src/card_motion.rs` `declines_request` (T): the motion verdict, derived from `dt`, which
//!   is 0 on a repeat, becomes "admit".
//! * `ui/src/frame/budget.rs` `Budget::take` (T): the elapsed-microsecond term is dropped; the
//!   quotas stay.
//! * `ui/src/containers/transition.rs` `PageDip::tick` (S) and `PageImage::plan` (S): the dip's
//!   one-tick floor and the page image's capture step stand still on a repeat.
//! * `ui/src/containers/transition.rs` `PageImage::plan` under `armed()` (**X**, the one
//!   picture-changing exception besides the tint): **in a dump a pushed page is filmed live under
//!   the dip, never as a held image.** Every frame the dip is in flight re-captures the top page
//!   (`PageImage::step_filmed`: a live draw at full alpha into the snapshot, composed at the
//!   dip's alpha, exactly what the product's two capture frames do), and the frame the dip ends
//!   the page is live; there is no `Held` frame and no replacement capture. The product freezes
//!   the image captured at the floor through the whole In half and on past it while the
//!   destination's layout moves (`PAGE_QUIESCENCE_HOLD_MAX_MS`), so a page that starts springs
//!   at its mount (a card shelf's label band, the focus pop, the caption, an `Xfade` fade-in)
//!   runs them unseen under the image and the image is then replaced by the page mid-flight (or
//!   finished): a cut in the film. A dump has the whole page's data at mount and is not bound by
//!   frame cost, so it can film the layout as it is each frame. The product's own behaviour at
//!   that seam is unmeasured on the television.
//! * `src/app/adapters/poster.rs` `evict_cooldown_due` (T): a cooldown is always over, and a failed
//!   fetch is recorded (`note_dump_failure`, an observation for the driver, not a drawn change)
//!   instead of waiting out a retry on a clock that is held still.
//! * `src/dev/framedump.rs` (the driver): [`arm`], [`set_held_repeat`] and the read in `idle_dt`.
//!
//! Two other switches exist and are not this one: the landing gate's own
//! (`plx_machine::landgate::Gate::arm_dump`, which `machine` keeps because it may not name `gfx`)
//! and the placeholder counter's (`plx_ui::placeholder`, armed per thread).
//!
//! **A runtime switch, off by default.** [`arm`] is called by the dump driver only; nothing in a
//! shipping loop does. With `hostsim` (and in test builds) it is a runtime switch, and an unarmed
//! hook costs one relaxed atomic load; in every other build [`armed`] and [`held_repeat`] are
//! constant `false`, so the hooks fold away. Nothing else in the crate changes behaviour.

#[cfg(any(feature = "hostsim", feature = "test-support", test))]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(any(feature = "hostsim", feature = "test-support", test))]
static DUMP: AtomicBool = AtomicBool::new(false);

// Thread-local, unlike the switch: only the loop's own thread reads or writes it, and a host test
// that sets it must not leak a "this is a repeat" into a concurrent test's page dip.
#[cfg(any(feature = "hostsim", feature = "test-support", test))]
thread_local! {
    static REPEAT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Arm dump mode for this process. A no-op outside the simulator (`hostsim`) and host tests.
pub fn arm() {
    #[cfg(any(feature = "hostsim", feature = "test-support", test))]
    DUMP.store(true, Ordering::Relaxed);
}

/// Disarm: the ordinary behaviour, and what a host test restores.
pub fn disarm() {
    #[cfg(any(feature = "hostsim", feature = "test-support", test))]
    DUMP.store(false, Ordering::Relaxed);
}

/// Is dump mode armed? **A constant `false` outside the simulator and host tests**: without
/// `hostsim` (every television build) nothing can arm the dump, so this inlines to `false` and every
/// `if armed()` branch in a per-frame path, old or new, is folded away. With `hostsim` it is one
/// relaxed atomic load.
#[inline(always)]
pub fn armed() -> bool {
    #[cfg(any(feature = "hostsim", feature = "test-support", test))]
    {
        DUMP.load(Ordering::Relaxed)
    }
    #[cfg(not(any(feature = "hostsim", feature = "test-support", test)))]
    {
        false
    }
}

/// **The driver's "this iteration repeats a virtual frame" fact.** The dump driver sets it at the
/// top of every iteration that is a held repeat (`holds_run > 0`: the same virtual frame, the same
/// clock, `dt == 0`) and clears it on the first iteration of a virtual frame. Per-iteration
/// steppers that would otherwise advance once per ITERATION (a page image's capture
/// state, a page dip's one-tick floor) read it and stand still on a repeat, so how many holds a
/// frame took cannot decide which state a written frame shows. Constant `false` wherever
/// [`armed`] is.
#[inline(always)]
pub fn held_repeat() -> bool {
    #[cfg(any(feature = "hostsim", feature = "test-support", test))]
    {
        REPEAT.with(std::cell::Cell::get)
    }
    #[cfg(not(any(feature = "hostsim", feature = "test-support", test)))]
    {
        false
    }
}

/// Driver only (and host tests): publish whether this iteration is a held repeat. See [`held_repeat`].
pub fn set_held_repeat(on: bool) {
    #[cfg(any(feature = "hostsim", feature = "test-support", test))]
    REPEAT.with(|r| r.set(on));
    #[cfg(not(any(feature = "hostsim", feature = "test-support", test)))]
    let _ = on;
}

/// A host test's guard: this thread is a held repeat until it drops. It does not arm the dump.
#[cfg(any(test, feature = "test-support"))]
pub struct HeldRepeat;

#[cfg(any(test, feature = "test-support"))]
impl HeldRepeat {
    pub fn new() -> Self {
        set_held_repeat(true);
        HeldRepeat
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Default for HeldRepeat {
    fn default() -> Self { Self::new() }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for HeldRepeat {
    fn drop(&mut self) {
        set_held_repeat(false);
    }
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
    fn a_ground_probe_reads_synchronously_per_call_in_dump_mode_and_on_a_cadence_outside_it() {
        let _g = plx_base::testlock::serial();
        assert_eq!(crate::gfx::probe_mode(armed()), crate::gfx::ProbeMode::Cadenced);
        let _armed = Armed::new();
        assert_eq!(crate::gfx::probe_mode(armed()), crate::gfx::ProbeMode::Sync);
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
