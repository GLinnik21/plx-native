//! **The cold resume of a transcoded item, driven the way the run loop drives it.** A play that
//! resolves with a saved position on a transcode used to rebuild the encoder at that offset inline
//! on the frame that landed it (`main-thread block: PMS HTTP`). It is a flight now
//! (`engine::begin_resume` plans, a worker registers the replacement, `engine::land_resume` installs
//! it). These tests run both frame-thread halves inside a `FrameScope`, against the loopback server
//! of `route::flight_rig`, and grade what the frame thread did and what became of the replacement.

use super::*;
use crate::route::flight_rig::FlightRig;
use std::time::Duration;

const RESUME_NS: i64 = 1_800_000_000_000;

struct Rig {
    ps: crate::route::PlaybackSession,
    pa: adapter::PlayerAdapter,
    flight: FlightRig,
}

impl Rig {
    /// A resolved transcode `rig-1` whose plan has just landed: the start transaction is
    /// `Prepared`, no Engine exists, and the server's `/decision` takes `delay_ms`.
    fn new(delay_ms: u64) -> Rig {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let flight = FlightRig::start(&mut ps, Duration::from_millis(delay_ms));
        SHARED.reset_session();
        TX.reset();
        claim_hold::clear();
        let ticket = crate::route::begin_route_start().expect("a landing's start transaction");
        assert!(crate::route::prepare_route_start(ticket));
        let pa = adapter::PlayerAdapter::new(unsafe { MainThread::assume() });
        Rig { ps, pa, flight }
    }

    /// The resume frame, exactly as the run loop drives it: inside a `FrameScope`, so a PMS call
    /// without an `allow_blocking` panics.
    fn begin(&mut self) -> ResumeStart {
        self.begin_at(RESUME_NS)
    }

    fn begin_at(&mut self, resume_ns: i64) -> ResumeStart {
        let _frame = plx_base::task::FrameScope::enter();
        begin_resume(&mut self.ps, resume_ns)
    }

    /// Frames until the flight's landing is drained, each asking the question the loop asks.
    fn land(&mut self) -> ResumeOutcome {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            {
                let _frame = plx_base::task::FrameScope::enter();
                if let Some(outcome) = land_resume(&mut self.ps) {
                    return outcome;
                }
            }
            assert!(std::time::Instant::now() < deadline, "timed out waiting for the resume's landing");
            assert_eq!(state(&self.ps), PlaybackState::Resolving, "the spinner holds until the landing");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Frames of a flight that is over, until the late landing has been reaped and its replacement
    /// stopped: nothing is outstanding, so each frame's `land_resume` has nothing to land.
    fn frames_until_replacement_stopped(&mut self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while self.replacement_stops() == 0 {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for the replacement's stop");
            {
                let _frame = plx_base::task::FrameScope::enter();
                assert!(land_resume(&mut self.ps).is_none(), "a stale flight landed");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn replacement_stops(&self) -> usize {
        self.flight.stopped().iter().filter(|s| s.starts_with("rig-logical-abr-")).count()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        claim_hold::clear();
        SHARED.reset_session();
        TX.reset();
        crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
    }
}

/// **The cold resume's frame makes no PMS call; the worker lands the Load later.** When the resume
/// frame returns the server has been asked NOTHING, the play is still `Resolving`, and the landing
/// installs the replacement at the saved offset exactly as the inline rebuild did: the HUD's clock
/// is seeded, the old encoder retired, the start transaction `Prepared` for the Load to claim.
#[test]
fn a_cold_resume_frame_makes_no_pms_call_and_the_worker_lands_the_load_later() {
    let mut rig = Rig::new(120);
    assert_eq!(rig.begin(), ResumeStart::Pending);
    assert_eq!(
        rig.flight.decisions(),
        0,
        "the resume frame made a PMS round trip on the frame thread: {:?}",
        rig.flight.requests()
    );
    assert!(crate::route::resume_flight_outstanding(), "the rebuild must be a flight the loop waits on");
    assert_eq!(state(&rig.ps), PlaybackState::Resolving, "the viewer keeps the resolve spinner");
    assert!(crate::route::control_phase_label().contains("Preparing"), "{}", crate::route::control_phase_label());

    assert_eq!(rig.land(), ResumeOutcome::Prepared);
    assert_eq!(rig.flight.decisions(), 1, "exactly one /decision for the resume");
    assert!(!crate::route::resume_flight_outstanding());
    assert_ne!(crate::route::transcode_session(&rig.ps), "rig-1", "the replacement is the route's now");
    assert!(crate::route::url(&rig.ps).contains("offset=1800"), "{}", crate::route::url(&rig.ps));
    assert_eq!(SHARED.disp_base.load(Relaxed), RESUME_NS, "the HUD reads content time from the offset");
    assert_eq!(SHARED.playpos_ns.load(Relaxed), RESUME_NS);
    assert!(crate::route::control_phase_label().contains("Prepared"), "{}", crate::route::control_phase_label());
    let start = crate::route::pending_route_start().expect("the Load claims the landing's own transaction");
    assert!(crate::route::claim_route_start_attempt(start).is_some());
    assert_ne!(state(&rig.ps), PlaybackState::Resolving, "the flight is over");
    rig.flight.wait_for("the offset-0 encoder's retirement", |f| f.stopped().iter().any(|s| s == "rig-1"));
}

/// A refused rebuild ends exactly as the inline `None` did: the outcome the caller fails the start
/// on, the transaction `Failed` (nothing pending for the caller's own reject to touch), the route
/// untouched, and only the refused replacement stopped.
#[test]
fn a_refused_cold_resume_ends_as_the_inline_rejection_did() {
    let mut rig = Rig::new(40);
    rig.flight.refuse_decisions(1);
    assert_eq!(rig.begin(), ResumeStart::Pending);
    assert_eq!(rig.land(), ResumeOutcome::RebuildRejected);
    assert!(crate::route::control_phase_label().contains("Failed"), "{}", crate::route::control_phase_label());
    assert!(crate::route::pending_route_start().is_none());
    assert_eq!(crate::route::transcode_session(&rig.ps), "rig-1");
    assert_eq!(SHARED.disp_base.load(Relaxed), 0, "a refused resume seeds no clock");
    assert_ne!(state(&rig.ps), PlaybackState::Resolving);
    rig.flight.wait_for("the refused replacement's stop", |_| rig.replacement_stops() == 1);
    assert!(!rig.flight.stopped().iter().any(|s| s == "rig-1"), "{:?}", rig.flight.stopped());
}

/// BACK during the flight (`exit_player`'s `stop_bufferfeed`, with no Engine to stop): nothing
/// installs, the loop has no landing to start a Load on, and the replacement the worker registers
/// afterwards is stopped by the loop's next reap of the late landing (there is no pump to do it).
#[test]
fn back_during_a_cold_resume_discards_the_landing_and_stops_its_replacement() {
    let mut rig = Rig::new(150);
    assert_eq!(rig.begin(), ResumeStart::Pending);
    rig.flight.wait_for("the resume's request", |f| f.decisions() == 1);
    stop_bufferfeed(&mut rig.ps, &mut rig.pa);
    assert!(!crate::route::resume_flight_outstanding());
    assert!(land_resume(&mut rig.ps).is_none());
    rig.frames_until_replacement_stopped();
    assert_eq!(SHARED.disp_base.load(Relaxed), 0, "nothing was installed over the next playback");
    assert!(land_resume(&mut rig.ps).is_none());
    assert_eq!(rig.replacement_stops(), 1, "stopped exactly once: {:?}", rig.flight.stopped());
}

/// An app-switch suspend during the flight drops it like a live Engine's suspend does: the
/// reducer is `Stable`, the replacement is stopped, and the foreground restore's own resume (the
/// same flight, `begin_resume`/`land_resume`) still rebuilds and prepares the route.
#[test]
fn a_suspend_during_a_cold_resume_drops_the_flight_and_leaves_the_foreground_restore_its_path() {
    let mut rig = Rig::new(100);
    assert_eq!(rig.begin(), ResumeStart::Pending);
    rig.flight.wait_for("the resume's request", |f| f.decisions() == 1);
    // What the run loop snapshots for `lifecycle.suspend` (`app::intended_pos`), BEFORE the suspend
    // tears the flight down: the flight's own offset is where the viewer is, and no landing has
    // seeded the clock yet.
    let saved_ns = intended_pos_ns(&rig.ps);
    suspend_bufferfeed(&mut rig.ps, &mut rig.pa);
    assert!(!crate::route::resume_flight_outstanding());
    assert!(crate::route::control_phase_label().contains("Stable"), "{}", crate::route::control_phase_label());
    assert!(land_resume(&mut rig.ps).is_none());
    rig.frames_until_replacement_stopped();
    assert_eq!(crate::route::transcode_session(&rig.ps), "rig-1", "the suspended session keeps its route");

    assert_eq!(
        saved_ns, RESUME_NS,
        "a suspend mid resume flight saved {saved_ns} ns: the foreground restore would resume from there, \
         and the timeline reports would overwrite the server's viewOffset"
    );
    assert_eq!(rig.begin_at(saved_ns), ResumeStart::Pending, "the restore's rebuild is a flight too");
    assert_eq!(rig.land(), ResumeOutcome::Prepared);
    assert_ne!(crate::route::transcode_session(&rig.ps), "rig-1", "the restore's own rebuild installed its replacement");
    assert!(crate::route::url(&rig.ps).contains("offset=1800"), "{}", crate::route::url(&rig.ps));
}

/// A newer play request during the flight supersedes it: the reducer moves on to `Resolving`, the
/// landing the worker posts afterwards is discarded and only ITS replacement is stopped — the route
/// the flight was rebuilding is left to whoever retires it (`Failed`'s fallback), not stopped here.
#[test]
fn a_newer_play_request_supersedes_a_cold_resume_and_stops_only_its_replacement() {
    let mut rig = Rig::new(150);
    assert_eq!(rig.begin(), ResumeStart::Pending);
    rig.flight.wait_for("the resume's request", |f| f.decisions() == 1);
    assert!(FlightRig::begin_newer_play_request(), "a flight with no Engine must not refuse the request");
    assert!(crate::route::control_phase_label().contains("Resolving"), "{}", crate::route::control_phase_label());
    assert!(!crate::route::resume_flight_outstanding());
    rig.frames_until_replacement_stopped();
    assert!(!rig.flight.stopped().iter().any(|s| s == "rig-1"), "{:?}", rig.flight.stopped());
    assert_eq!(SHARED.disp_base.load(Relaxed), 0);
}
