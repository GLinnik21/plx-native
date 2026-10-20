//! Carry what the "Hold [OK] for options" hint has taught from one run to the next.
//!
//! `plx_ui::hold_hint` keeps its latches per run on the frame thread and may not name a Plex type,
//! so it offers two doors and this module is the application end of both: [`seed`] hands it what the
//! install was already taught (boot, once, before the first frame), and [`HintTaught::poll`], called
//! every frame, drains its newly-taught signal and queues ONE session write for it.
//!
//! The store is the session preference record (`Preferences::hold_hint_taught`, an Install row:
//! the television's, kept across sign-out). The write goes through
//! `session::learn_hold_hint_taught`, which only queues work for the storage worker and so never
//! blocks the frame; it queues nothing for a lesson the cached session already holds. A write that
//! finds no stored session (not signed in) is lost and the hint teaches once more on the next run,
//! which is the right failure: it can only show again, never stay hidden by a failed save.

use plx_plex::plex::session::{self, HoldHintTaught};

/// Seed the hint's per-run latches from what the install was taught on an earlier run. Only the
/// "shown" latches come back: a taught install still shows the capsule and its fill on a physical
/// held OK, it just never offers itself again.
pub(crate) fn seed(taught: HoldHintTaught) {
    plx_ui::hold_hint::seed_taught(taught.card, taught.pill);
}

/// What the frame loop owns: nothing but the poll.
pub(crate) struct HintTaught;

impl HintTaught {
    pub(crate) fn new() -> Self {
        Self
    }

    /// One frame's look: a thread-local flag read unless the hint was just taught.
    ///
    /// `controlled` is a replay or a controlled recording (`App::boot_initial`): the run brings its
    /// own session and must not touch the install's stored one, so the lesson is drained and
    /// dropped (the signal is a one-shot edge; leaving it queued would only grow stale).
    pub(crate) fn poll(&mut self, controlled: bool) {
        self.poll_with(controlled, session::learn_hold_hint_taught);
    }

    /// The decision, with the write injected so a test can count it.
    fn poll_with(&mut self, controlled: bool, write: impl FnOnce(HoldHintTaught) -> bool) {
        if let Some(taught) = plx_ui::hold_hint::take_newly_taught() {
            if !controlled {
                write(HoldHintTaught { card: taught.card, pill: taught.pill });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// The hint's latches are thread-local and so is this test's whole world: reset on both sides.
    struct Fresh;
    impl Fresh {
        fn new() -> Self {
            plx_ui::hold_hint::reset_shown_for_test();
            plx_ui::hold_hint::reset_learned_for_test();
            let _ = plx_ui::hold_hint::take_newly_taught();
            Fresh
        }
    }
    impl Drop for Fresh {
        fn drop(&mut self) {
            plx_ui::hold_hint::reset_shown_for_test();
            plx_ui::hold_hint::reset_learned_for_test();
            let _ = plx_ui::hold_hint::take_newly_taught();
        }
    }

    #[test]
    fn a_taught_install_seeds_the_latches_without_issuing_a_write() {
        let _fresh = Fresh::new();
        seed(HoldHintTaught { card: true, pill: false });
        assert!(plx_ui::hold_hint::shown_this_run(), "the card latch is down");
        let writes = RefCell::new(Vec::new());
        HintTaught::new().poll_with(false, |t| { writes.borrow_mut().push(t); true });
        assert!(writes.borrow().is_empty(), "seeding is not news: {:?}", writes.borrow());
    }

    #[test]
    fn a_newly_taught_transition_issues_exactly_one_write() {
        let _fresh = Fresh::new();
        let mut poller = HintTaught::new();
        let writes = RefCell::new(Vec::new());
        poller.poll_with(false, |t| { writes.borrow_mut().push(t); true });
        assert!(writes.borrow().is_empty(), "nothing taught, nothing written");
        plx_ui::hold_hint::mark_learned();
        for _ in 0..3 {
            poller.poll_with(false, |t| { writes.borrow_mut().push(t); true });
        }
        assert_eq!(*writes.borrow(), vec![HoldHintTaught { card: true, pill: true }],
            "one write for the rising edge, none while it stays up");
    }

    #[test]
    fn a_controlled_run_drains_the_lesson_and_writes_nothing() {
        let _fresh = Fresh::new();
        let mut poller = HintTaught::new();
        let writes = RefCell::new(Vec::new());
        plx_ui::hold_hint::mark_learned();
        poller.poll_with(true, |t| { writes.borrow_mut().push(t); true });
        assert!(writes.borrow().is_empty(), "a replay never writes the real session: {:?}", writes.borrow());
        poller.poll_with(false, |t| { writes.borrow_mut().push(t); true });
        assert!(writes.borrow().is_empty(), "and the edge was consumed, not deferred");
    }
}
