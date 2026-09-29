//! **Presentation hold while a claimed retranscode's PMS half is in flight.**
//!
//! A track pick, an enhancement toggle or a quality change is reconciled on a worker
//! (`route::execute_retranscode_claim`), which takes 1-15 s for its `/decision` round trip while
//! the current stream keeps PLAYING. The landing then reloads at the offset the claim captured, and
//! PMS encoded from that offset, so the viewer watched the seconds of the flight a second time.
//!
//! The fix is to stop presentation at claim time, through the ONE pause the viewer's own Pause key
//! uses ([`super::pause`]), and give it back at landing whichever way the claim settled. The
//! spinner is the HUD's existing busy mark: [`super::state`] answers `Buffering` while a hold is
//! live, and the transport slot already draws its `Working` glyph for that.
//!
//! **A viewer's transport press wins.** The viewer owns the transport; the hold is a loan. Any
//! press that reaches `app::lifecycle::set_transport_paused` calls [`note_user_transport`], which
//! keeps the spinner (the claim is still flying) but forgets the restore, so a Pause the viewer
//! pressed during the flight stays paused after the landing and a Play they pressed keeps playing.
//! The hold's own pause and resume call [`super::pause`]/[`super::resume`] directly and so never
//! count as a viewer press.

use std::sync::Mutex;

/// One live hold. `serial` names the claim it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hold {
    serial: u64,
    /// The hold paused a playing stream, and no viewer press has touched the transport since.
    restore_play: bool,
}

static HOLD: Mutex<Option<Hold>> = Mutex::new(None);

fn slot() -> std::sync::MutexGuard<'static, Option<Hold>> {
    HOLD.lock().unwrap_or_else(|e| e.into_inner())
}

/// What [`take`] hands the landing: whether to give play back once the claim has settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Landing {
    restore_play: bool,
}

/// A retranscode claim was dispatched to its worker: hold presentation at the claim offset.
/// Already-paused streams stay as they are (nothing to give back); a refused native Pause is
/// logged by [`super::pause`] and leaves the stream playing, with no restore owed.
pub(crate) fn engage(pa: &mut super::adapter::PlayerAdapter, serial: u64) {
    let was_paused = super::TX.paused.load(std::sync::atomic::Ordering::Acquire);
    let held = !was_paused && super::pause(pa);
    if held {
        super::log("claim hold: paused at the claim offset while the server prepares the new stream");
    }
    *slot() = Some(Hold { serial, restore_play: held });
}

/// A viewer transport press (Play, Pause or the toggle): the viewer's choice outranks the hold, so
/// the landing must not overwrite it.
pub(crate) fn note_user_transport() {
    if let Some(hold) = slot().as_mut() {
        hold.restore_play = false;
    }
}

/// Whether a hold for a claim that is still in flight is up (the spinner's condition). A hold left
/// behind by a torn-down or superseded claim answers false through `route::claim_is_applying`.
pub(crate) fn active() -> bool {
    let serial = slot().map(|h| h.serial);
    serial.is_some_and(crate::route::claim_is_applying)
}

/// Take the hold belonging to `serial` at its landing. A hold for any other serial is left alone
/// (it is stale and the next [`engage`] overwrites it).
pub(crate) fn take(serial: u64) -> Option<Landing> {
    let mut guard = slot();
    match *guard {
        Some(hold) if hold.serial == serial => {
            *guard = None;
            Some(Landing { restore_play: hold.restore_play })
        }
        _ => None,
    }
}

/// Give play back after the claim settled and its tail ran (a rejection leaves the same Engine
/// running; an accepted claim has already reloaded, the reload having kept the pause).
pub(crate) fn release(pa: &mut super::adapter::PlayerAdapter, landing: Option<Landing>) {
    if landing.is_some_and(|l| l.restore_play) && !super::resume(pa) {
        super::log("claim hold: could not give play back after the claim settled");
    }
}

/// Drop any hold without touching the transport: the playback it belonged to is gone (teardown
/// resets the transport itself).
pub(crate) fn clear() {
    *slot() = None;
}

#[cfg(all(test, feature = "hostsim"))]
mod tests {
    use super::*;
    use crate::player::{ffi, SHARED, TX};
    use std::sync::atomic::Ordering::Acquire;

    const CLAIM_OFFSET_NS: i64 = 100_000_000_000;

    struct Rig {
        _serial: crate::testlock::Serial,
        pa: super::super::adapter::PlayerAdapter,
    }

    impl Rig {
        /// A playing stream at the claim offset on the host sink.
        fn playing() -> Rig {
            let serial = crate::testlock::serial();
            ffi::force_clocksink_for_test(true);
            TX.reset();
            SHARED.reset_hls_clock_for_test();
            clear();
            ffi::clock_run_for_test(CLAIM_OFFSET_NS);
            Rig {
                _serial: serial,
                pa: super::super::adapter::PlayerAdapter::new(unsafe { crate::task::MainThread::assume() }),
            }
        }
        fn paused() -> Rig {
            let mut rig = Rig::playing();
            assert!(crate::player::pause(&mut rig.pa));
            rig
        }
        fn position(&self) -> (i64, bool) {
            ffi::clock_state_for_test()
        }
        fn wait(&self) {
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            clear();
            TX.reset();
            SHARED.reset_hls_clock_for_test();
            ffi::force_clocksink_for_test(false);
        }
    }

    #[test]
    fn presentation_holds_at_the_claim_offset_for_the_whole_flight() {
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        let (held_at, running) = rig.position();
        rig.wait();
        let (later, still_running) = rig.position();
        assert!(!running && !still_running, "the sink clock must be stopped during the flight");
        assert_eq!(held_at, later, "the position advanced during the flight");
        assert_eq!(held_at, CLAIM_OFFSET_NS, "and it stopped AT the claim offset");
        assert!(TX.paused.load(Acquire), "the feed gate is the viewer's Pause gate");
    }

    #[test]
    fn an_accepted_claim_gives_play_back_after_the_reload() {
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        assert!(TX.paused.load(Acquire) && !rig.position().1, "the flight must have held presentation first");
        // What an accepted landing does to the transport: the reload resets the session but keeps
        // the viewer-visible pause (`TX::reset_for_reload`).
        TX.reset_for_reload();
        release(&mut rig.pa, take(7));
        assert!(!TX.paused.load(Acquire), "play state not restored after an accepted claim");
        assert!(rig.position().1, "the sink clock is running again");
    }

    #[test]
    fn a_rejected_claim_gives_play_back_to_the_stream_it_kept() {
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        assert!(TX.paused.load(Acquire) && !rig.position().1, "the flight must have held presentation first");
        let (held_at, _) = rig.position();
        release(&mut rig.pa, take(7));
        assert!(!TX.paused.load(Acquire), "play state not restored after a rejected claim");
        let (resumed_from, running) = rig.position();
        assert!(running);
        assert!(resumed_from >= held_at, "the rejected stream continues, it does not rewind");
    }

    #[test]
    fn a_stream_the_viewer_had_paused_stays_paused_through_the_claim() {
        let mut rig = Rig::paused();
        let plays = ffi::play_calls_for_test();
        engage(&mut rig.pa, 7);
        release(&mut rig.pa, take(7));
        assert!(TX.paused.load(Acquire), "the pre-claim pause must survive");
        assert_eq!(ffi::play_calls_for_test(), plays, "no Play was issued");
        assert!(!rig.position().1);
    }

    #[test]
    fn a_viewer_pause_during_the_flight_wins_over_the_restore() {
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        // Pause pressed while the hold's own pause is standing.
        assert!(crate::app::lifecycle::set_transport_paused(&mut rig.pa, true));
        let plays = ffi::play_calls_for_test();
        release(&mut rig.pa, take(7));
        assert!(TX.paused.load(Acquire), "the landing resumed a stream the viewer paused");
        assert_eq!(ffi::play_calls_for_test(), plays);
    }

    #[test]
    fn a_viewer_play_during_the_flight_wins_and_is_not_doubled_at_landing() {
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        assert!(crate::app::lifecycle::set_transport_paused(&mut rig.pa, false));
        assert!(!TX.paused.load(Acquire));
        // ...then paused again before the landing: that later press is the one that stands.
        assert!(crate::app::lifecycle::set_transport_paused(&mut rig.pa, true));
        release(&mut rig.pa, take(7));
        assert!(TX.paused.load(Acquire), "the viewer's last press stands");
    }

    #[test]
    fn a_stale_landing_leaves_the_transport_alone() {
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        assert_eq!(take(8), None, "another claim's landing does not own this hold");
        release(&mut rig.pa, take(8));
        assert!(TX.paused.load(Acquire));
        clear();
    }

    #[test]
    fn the_spinner_needs_a_claim_that_is_still_flying() {
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        assert!(!active(), "no claim is Applying(7), so the leftover hold must not draw a spinner");
    }
}
