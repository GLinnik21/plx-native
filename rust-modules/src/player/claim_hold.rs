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
//! press that reaches `player::lifecycle::set_transport_paused` calls [`note_user_transport`], which
//! keeps the spinner (the claim is still flying) but forgets the restore, so a Pause the viewer
//! pressed during the flight stays paused after the landing and a Play they pressed keeps playing.
//! The hold's own pause and resume call [`super::pause`]/[`super::resume`] directly and so never
//! count as a viewer press.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;

/// One live hold. `serial` names the claim it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hold {
    serial: u64,
    /// The hold paused a playing stream, and no viewer press has touched the transport since.
    restore_play: bool,
}

static HOLD: Mutex<Option<Hold>> = Mutex::new(None);

/// The live hold's serial (`0` = none; claim serials are never 0, see `route::plan::next_generation`).
/// [`super::state`] asks [`active`] on every call and a frame makes many, so the idle answer is
/// this one load rather than a lock. Only [`store`] writes it, under `HOLD`'s guard.
static SERIAL: AtomicU64 = AtomicU64::new(0);

fn slot() -> std::sync::MutexGuard<'static, Option<Hold>> {
    HOLD.lock().unwrap_or_else(|e| e.into_inner())
}

/// Replace the hold and its mirrored serial together.
fn store(guard: &mut Option<Hold>, hold: Option<Hold>) {
    *guard = hold;
    SERIAL.store(hold.map_or(0, |h| h.serial), Relaxed);
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
    store(&mut slot(), Some(Hold { serial, restore_play: held }));
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
    let serial = SERIAL.load(Relaxed);
    serial != 0 && crate::route::claim_is_applying(serial)
}

/// Whether the transport is paused BY the hold (a live hold that owes the viewer a Play back),
/// not by the viewer. The viewer's own transport reads — the OK toggle, `Transport(None)`,
/// `resume_if_paused` — must not mistake this pause for theirs: the toggle would resume a stream
/// the hold is keeping still, and a seek-side resume would play the flight's seconds again.
pub(crate) fn owns_pause() -> bool {
    if SERIAL.load(Relaxed) == 0 {
        return false;
    }
    let hold = *slot();
    hold.is_some_and(|h| h.restore_play && crate::route::claim_is_applying(h.serial))
}

/// Take the hold belonging to `serial` at its landing, answering whether it owes the viewer a Play
/// back ([`release`]). A hold for any other serial is left alone (it is stale and the next
/// [`engage`] overwrites it).
pub(crate) fn take(serial: u64) -> bool {
    let mut guard = slot();
    match *guard {
        Some(hold) if hold.serial == serial => {
            store(&mut guard, None);
            hold.restore_play
        }
        _ => false,
    }
}

/// Give play back after the claim settled and its tail ran (a rejection leaves the same Engine
/// running; an accepted claim has already reloaded, the reload having kept the pause).
pub(crate) fn release(pa: &mut super::adapter::PlayerAdapter, restore_play: bool) {
    if restore_play && !super::resume(pa) {
        super::log("claim hold: could not give play back after the claim settled");
    }
}

/// Test-only: stand a hold up for `serial` without a live Engine.
#[cfg(test)]
pub(crate) fn hold_for_test(serial: u64, restore_play: bool) {
    store(&mut slot(), Some(Hold { serial, restore_play }));
}

/// Drop any hold without touching the transport: the playback it belonged to is gone (teardown
/// resets the transport itself).
pub(crate) fn clear() {
    store(&mut slot(), None);
}

#[cfg(all(test, feature = "hostsim"))]
mod tests {
    use super::*;
    use crate::player::shared::{HlsPlayCompletion, HlsPrimeKind};
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
            SHARED.seeking.store(false, std::sync::atomic::Ordering::Relaxed);
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
            crate::route::reset_player_control_for_test(&crate::route::PlaybackSession::IDLE);
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

    /// The accepted landing exactly as production runs it: the reload tears the clock down and
    /// `start_bufferfeed` re-arms it through `arm_initial_clock_hold(user_held = TX.paused)`, which
    /// leaves `Held{Initial}` with the viewer-hold flag set. The give-back is then the DEFERRED
    /// resume — `TX` reopens, the physical Play stays fenced — and the new stream's Initial prime
    /// is what finally issues Play. (`TX.reset_for_reload()` alone leaves the clock in `Held{User}`,
    /// which is the immediate-Play path and NOT what a landing produces.)
    #[test]
    fn an_accepted_claim_gives_play_back_after_the_reload() {
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        assert!(TX.paused.load(Acquire) && !rig.position().1, "the flight must have held presentation first");

        // The reload: teardown resets the session's clock, the transport keeps the pause, and the
        // new Engine arms its Initial hold from that pause.
        TX.reset_for_reload();
        SHARED.reset_hls_clock_for_test();
        assert!(SHARED.arm_initial_clock_hold(TX.paused.load(Acquire), false));
        assert_eq!(SHARED.hls_prime_kind(), Some(HlsPrimeKind::Fresh), "the new stream is held for its Initial prime");

        let plays = ffi::play_calls_for_test();
        release(&mut rig.pa, take(7));
        assert!(!TX.paused.load(Acquire), "play state not restored after an accepted claim");
        assert_eq!(ffi::play_calls_for_test(), plays, "the Deferred resume must leave the physical Play to the prime");
        assert!(!rig.position().1, "the sink clock must wait for the Initial prime");

        // The Initial prime's Play (`engine::try_prime`): reserve, issue, complete.
        let generation = SHARED.hls_candidate_generation.load(Acquire);
        let (token, _) = SHARED
            .reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, SHARED.hls_recovery())
            .expect("the Initial prime is owed the Play once the viewer hold is released");
        assert_ne!(unsafe { ffi::sf_play(rig.pa.mt()) }, 0);
        assert!(matches!(SHARED.complete_hls_prime_play(token, true), HlsPlayCompletion::Accepted { .. }));
        assert_eq!(ffi::play_calls_for_test(), plays + 1);
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
        assert!(crate::player::lifecycle::set_transport_paused(&mut rig.pa, true));
        let plays = ffi::play_calls_for_test();
        release(&mut rig.pa, take(7));
        assert!(TX.paused.load(Acquire), "the landing resumed a stream the viewer paused");
        assert_eq!(ffi::play_calls_for_test(), plays);
    }

    #[test]
    fn a_viewer_play_during_the_flight_wins_and_is_not_doubled_at_landing() {
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        assert!(crate::player::lifecycle::set_transport_paused(&mut rig.pa, false));
        assert!(!TX.paused.load(Acquire));
        // ...then paused again before the landing: that later press is the one that stands.
        assert!(crate::player::lifecycle::set_transport_paused(&mut rig.pa, true));
        release(&mut rig.pa, take(7));
        assert!(TX.paused.load(Acquire), "the viewer's last press stands");
    }

    #[test]
    fn a_stale_landing_leaves_the_transport_alone() {
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        assert!(!take(8), "another claim's landing does not own this hold");
        release(&mut rig.pa, take(8));
        assert!(TX.paused.load(Acquire));
        clear();
    }

    #[test]
    fn the_spinner_needs_a_claim_that_is_still_flying() {
        let ps = crate::route::PlaybackSession::IDLE;
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        assert!(!active(), "no claim is Applying(7), so the leftover hold must not draw a spinner");
        assert_ne!(crate::player::state(&ps), crate::player::PlaybackState::Buffering);

        // The positive case: the same hold, with claim 7 actually in flight.
        crate::route::force_applying_for_test(7);
        assert!(active(), "a hold whose claim is flying is the spinner's condition");
        assert_eq!(crate::player::state(&ps), crate::player::PlaybackState::Buffering);

        // And a DIFFERENT claim's flight is not this hold's.
        crate::route::force_applying_for_test(8);
        assert!(!active());
    }

    /// Finding: the OK toggle (`!paused()`), `PlayerReq::Transport(None)` and `resume_if_paused`
    /// (Skip Intro, SeekTo, From Beginning) all read the hold's OWN pause as the viewer's, so a
    /// press during the flight RESUMED the stream the hold had paused — playing the very seconds
    /// the hold exists to keep from being shown twice.
    #[test]
    fn the_ok_toggle_during_a_hold_pauses_instead_of_resuming_the_holds_own_pause() {
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        crate::route::force_applying_for_test(7);
        let target = crate::player::lifecycle::transport_target(None, crate::player::lifecycle::viewer_paused());
        assert!(target, "OK during the spinner read the hold's pause as the viewer's and asked for Play");

        // Applying that press is the viewer's Pause: it becomes THEIR intent, so the landing
        // must not give play back.
        assert!(crate::player::lifecycle::set_transport_paused(&mut rig.pa, target));
        let plays = ffi::play_calls_for_test();
        release(&mut rig.pa, take(7));
        assert!(TX.paused.load(Acquire), "the viewer's Pause was overridden by the hold's restore");
        assert_eq!(ffi::play_calls_for_test(), plays);
    }

    #[test]
    fn a_seek_side_resume_does_not_resume_the_holds_pause() {
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        crate::route::force_applying_for_test(7);
        let plays = ffi::play_calls_for_test();

        crate::player::lifecycle::resume_if_paused(&mut rig.pa);
        assert!(TX.paused.load(Acquire), "Skip Intro / SeekTo / From Beginning resumed the hold's pause");
        assert_eq!(ffi::play_calls_for_test(), plays);
        assert!(!rig.position().1);

        // The hold still owes its restore: the seek was carried past the reload, and Play lands after.
        release(&mut rig.pa, take(7));
        assert!(!TX.paused.load(Acquire), "the hold's restore was consumed by the seek's resume");
        assert!(rig.position().1);
    }

    /// A stream the VIEWER had paused, with a hold on top? Not possible (`engage` owes no restore),
    /// so a resume there is the viewer's and goes through.
    #[test]
    fn a_seek_side_resume_still_resumes_a_stream_the_viewer_paused() {
        let mut rig = Rig::paused();
        engage(&mut rig.pa, 7);
        crate::route::force_applying_for_test(7);
        crate::player::lifecycle::resume_if_paused(&mut rig.pa);
        assert!(!TX.paused.load(Acquire), "a viewer pause is the viewer's to resume");
    }
    /// Finding: `set_transport_paused` compared against the raw `paused()`, so an explicit PLAY
    /// during the flight dropped the restore and resumed the OLD stream mid-flight; the flight's
    /// seconds then replayed after the landing. The viewer sees a spinner over a playing intent,
    /// so Play there is already satisfied.
    #[test]
    fn an_explicit_play_during_a_hold_is_a_no_op_and_the_landing_still_gives_play_back() {
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        crate::route::force_applying_for_test(7);
        let plays = ffi::play_calls_for_test();
        assert!(crate::player::lifecycle::set_transport_paused(&mut rig.pa, false));
        assert!(TX.paused.load(Acquire), "PLAY during the hold resumed the old stream mid-flight");
        assert_eq!(ffi::play_calls_for_test(), plays);
        assert!(!rig.position().1);
        assert!(owns_pause(), "the hold's restore was dropped by the PLAY");
        release(&mut rig.pa, take(7));
        assert!(!TX.paused.load(Acquire), "the landing no longer gives play back");
        assert!(rig.position().1);
    }

    /// Finding: suspend saved `paused()`, i.e. the hold's own pause, as the viewer's.
    #[test]
    fn a_suspend_during_a_hold_saves_playing() {
        use crate::player::lifecycle::{clock_for_suspend_now, ForegroundClock, ForegroundLifecycle};
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        crate::route::force_applying_for_test(7);
        assert!(TX.paused.load(Acquire));
        let mut lifecycle = ForegroundLifecycle::IDLE;
        let clock = clock_for_suspend_now(&lifecycle);
        lifecycle.suspend(CLAIM_OFFSET_NS, clock);
        assert_eq!(clock, ForegroundClock::Playing, "the hold's pause was saved as the viewer's");
    }

    #[test]
    fn a_suspend_after_a_viewer_pause_during_a_hold_saves_paused() {
        use crate::player::lifecycle::{clock_for_suspend_now, ForegroundClock, ForegroundLifecycle};
        let mut rig = Rig::playing();
        engage(&mut rig.pa, 7);
        crate::route::force_applying_for_test(7);
        assert!(crate::player::lifecycle::set_transport_paused(&mut rig.pa, true));
        assert_eq!(clock_for_suspend_now(&ForegroundLifecycle::IDLE), ForegroundClock::Paused);
    }
}
