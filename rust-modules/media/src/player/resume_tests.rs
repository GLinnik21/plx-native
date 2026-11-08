//! **The cold resume of a transcoded item, driven the way the run loop drives it.** A play that
//! resolves with a saved position on a transcode used to rebuild the encoder at that offset inline
//! on the frame that landed it (`main-thread block: PMS HTTP`). It is a flight now
//! (`route_start::begin_resume` plans, a worker registers the replacement, `route_start::land_resume` installs
//! it). These tests run both frame-thread halves inside a `FrameScope`, against the loopback server
//! of `route::flight_rig`, and grade what the frame thread did and what became of the replacement.

use plx_play_feed::shared::{FEED_SHARED, FEED_TX};
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
        Rig::shaped(delay_ms, FlightRig::start)
    }

    /// [`Rig::new`] on the route `start` installs (one of `FlightRig`'s shapes).
    fn shaped(delay_ms: u64, start: fn(&mut crate::route::PlaybackSession, Duration) -> FlightRig) -> Rig {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let flight = start(&mut ps, Duration::from_millis(delay_ms));
        crate::player::reset_session_for_test();
        FEED_TX.reset();
        claim_hold::clear();
        let ticket = crate::route::begin_route_start().expect("a landing's start transaction");
        assert!(crate::route::prepare_route_start(ticket));
        let pa = adapter::PlayerAdapter::new_fed(unsafe { MainThread::assume() });
        Rig { ps, pa, flight }
    }

    /// The resume frame, exactly as the run loop drives it: inside a `FrameScope`, so a PMS call
    /// without an `allow_blocking` panics.
    fn begin(&mut self) -> ResumeStart {
        self.begin_at(RESUME_NS)
    }

    fn begin_at(&mut self, resume_ns: i64) -> ResumeStart {
        let _frame = plx_base::task::FrameScope::enter();
        begin_resume(&mut self.ps, &mut self.pa, resume_ns)
    }

    /// Frames until the flight's landing is drained, each asking the question the loop asks.
    fn land(&mut self) -> ResumeOutcome {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            {
                let _frame = plx_base::task::FrameScope::enter();
                if let Some(outcome) = land_resume(&mut self.ps, &mut self.pa) {
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
                assert!(land_resume(&mut self.ps, &mut self.pa).is_none(), "a stale flight landed");
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
        crate::player::reset_session_for_test();
        FEED_TX.reset();
        crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
    }
}

/// **The cold resume's frame makes no PMS call; the worker lands the Load later.** When the resume
/// frame returns no answer has been written (the rig holds them all), so the frame did not wait on
/// the server; the play is still `Resolving`, and the landing installs the replacement at the saved
/// offset exactly as the inline rebuild did: the HUD's clock is seeded, the old encoder retired, the
/// start transaction `Prepared` for the Load to claim. The request log is not graded at the frame's
/// return: the worker may already have sent its request by then.
#[test]
fn a_cold_resume_frame_makes_no_pms_call_and_the_worker_lands_the_load_later() {
    let mut rig = Rig::new(120);
    rig.flight.hold_answers();
    assert_eq!(rig.begin(), ResumeStart::Pending);
    assert_eq!(
        rig.flight.answered(),
        0,
        "the resume frame waited for a PMS answer on the frame thread: {:?}",
        rig.flight.requests()
    );
    assert!(crate::route::resume_flight_outstanding(), "the rebuild must be a flight the loop waits on");
    assert_eq!(state(&rig.ps), PlaybackState::Resolving, "the viewer keeps the resolve spinner");
    assert!(crate::route::control_phase_label().contains("Preparing"), "{}", crate::route::control_phase_label());
    rig.flight.release_answers();

    assert_eq!(rig.land(), ResumeOutcome::Prepared);
    assert_eq!(rig.flight.decisions(), 1, "exactly one /decision for the resume");
    assert!(!crate::route::resume_flight_outstanding());
    assert_ne!(crate::route::transcode_session(&rig.ps), "rig-1", "the replacement is the route's now");
    assert!(crate::route::url(&rig.ps).contains("offset=1800"), "{}", crate::route::url(&rig.ps));
    assert_eq!(FEED_SHARED.disp_base.load(Relaxed), RESUME_NS, "the HUD reads content time from the offset");
    assert_eq!(FEED_SHARED.playpos_ns.load(Relaxed), RESUME_NS);
    // No attempt has sampled the feed: the readout is idle, so a side reader captured by the Load that
    // follows reads the resume offset from the player's record of the Rebase, not 0 from `raw`.
    assert_eq!(rig.ps.readout().raw().disp_base, 0, "nothing sampled the Rebase yet");
    assert_eq!(super::subside::capture_offset_ns(&rig.ps), RESUME_NS, "the reader starts where the remux was resumed");
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
    assert_eq!(FEED_SHARED.disp_base.load(Relaxed), 0, "a refused resume seeds no clock");
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
    assert!(land_resume(&mut rig.ps, &mut rig.pa).is_none());
    rig.frames_until_replacement_stopped();
    assert_eq!(FEED_SHARED.disp_base.load(Relaxed), 0, "nothing was installed over the next playback");
    assert!(land_resume(&mut rig.ps, &mut rig.pa).is_none());
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
    // What the run loop snapshots for the lifecycle's `Park` (`app::intended_pos`), BEFORE the suspend
    // tears the flight down: the flight's own offset is where the viewer is, and no landing has
    // seeded the clock yet.
    let saved_ns = intended_pos_ns(&rig.ps);
    suspend_bufferfeed(&mut rig.ps, &mut rig.pa);
    assert!(!crate::route::resume_flight_outstanding());
    assert!(crate::route::control_phase_label().contains("Stable"), "{}", crate::route::control_phase_label());
    assert!(land_resume(&mut rig.ps, &mut rig.pa).is_none());
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
    assert_eq!(FEED_SHARED.disp_base.load(Relaxed), 0);
}

/// The field report's saved position (nightly 0.10.0-nightly-20261108, rk 2029): the reopened film
/// resumed at 343 s.
const FIELD_RESUME_NS: i64 = 343_000_000_000;

/// Land a cold resume at `resume_ns` and take the production start the run loop takes next
/// (`app::run`'s `finish_landed_play`: `start_playback_tracked`), grading the Feed start door
/// (`start_bufferfeed_prepared`'s `backend_planned_feed_allowed`) on the way. Stops what launched.
fn cold_resume_then_start(rig: &mut Rig, resume_ns: i64) {
    use crate::route::backend_policy::OfferId;
    assert_eq!(rig.ps.planned_feed_offer(), Some(OfferId::FEED_PMS_REMUX),
        "PRECONDITION: the plan qualified the PMS remux recipe");
    assert!(rig.ps.backend_planned_feed_allowed(),
        "PRECONDITION: the same route started at 0 (no resume) passes the Feed start door");
    assert_eq!(rig.begin_at(resume_ns), ResumeStart::Pending, "a transcode's resume is a flight");
    assert_eq!(rig.land(), ResumeOutcome::Prepared, "the rebuild at the saved offset landed");
    let offset = format!("offset={}", resume_ns / 1_000_000_000);
    assert!(crate::route::url(&rig.ps).contains(&offset), "{}", crate::route::url(&rig.ps));
    let planned = rig.ps.planned_feed_offer();
    let allowed = rig.ps.backend_planned_feed_allowed();
    let started = start_playback_tracked(&mut rig.ps, &mut rig.pa);
    let launched = !matches!(started, BufferfeedStartOutcome::Failed);
    if launched {
        stop_bufferfeed(&mut rig.ps, &mut rig.pa);
        super::route_start::finish_release_for_test(&mut rig.pa);
    }
    assert!(allowed,
        "the cold resume's landing left the route's recipe {planned:?} unqualified at the Feed start door \
         (start_bufferfeed: no qualified feed offer for this resolved route)");
    assert!(launched, "the cold resume's start was refused ({started:?})");
}

/// **The field bug: a cold resume of the enhanced remux Boost Dialog makes of a direct play is
/// refused at the Feed start door.** The first play of the film (offset 0) started; the reopen from
/// the detail page resumed at the saved 343 s, the rebuild landed (`resume(transcode): restart at
/// offset 343s`) and the start answered "no qualified feed offer for this resolved route", leaving
/// the player black at fps=0.
#[test]
fn an_enhanced_remux_cold_resume_is_admitted_at_the_feed_start_door() {
    let mut rig = Rig::shaped(20, FlightRig::start_enhanced_remux);
    cold_resume_then_start(&mut rig, FIELD_RESUME_NS);
}

/// The same door for any progressive transcode (a remux with no enhancement): nothing about the
/// refusal is Boost Dialog's.
#[test]
fn a_plain_remux_cold_resume_is_admitted_at_the_feed_start_door() {
    let mut rig = Rig::new(20);
    cold_resume_then_start(&mut rig, FIELD_RESUME_NS);
}

/// The field session's order: the film's first play (offset 0) started and was admitted, a seek to
/// 343 s was pending when BACK tore it down, and the viewer reopened the film from its detail page,
/// a NEW play request that cold-resumes at the saved 343 s. Nothing of the earlier play may make
/// that start's recipe unqualified (the request begins its own catalog), and the cold resume must
/// be admitted as it is with no earlier play.
#[test]
fn an_enhanced_remux_cold_resume_after_back_tore_down_an_earlier_play_mid_seek_is_admitted() {
    let mut rig = Rig::shaped(20, FlightRig::start_enhanced_remux);
    let first = start_playback_tracked(&mut rig.ps, &mut rig.pa);
    assert!(matches!(first, BufferfeedStartOutcome::Launched(_)), "PRECONDITION: the first play at 0 started ({first:?})");
    request_seek(&mut rig.ps, &mut rig.pa, FIELD_RESUME_NS);
    stop_bufferfeed(&mut rig.ps, &mut rig.pa);
    super::route_start::finish_release_for_test(&mut rig.pa);

    assert!(FlightRig::begin_newer_play_request(), "the reopen is a new play request");
    rig.flight.plan_again_as_new_request(&mut rig.ps);
    let ticket = crate::route::begin_route_start().expect("the reopen's landing has a start transaction");
    assert!(crate::route::prepare_route_start(ticket));
    cold_resume_then_start(&mut rig, FIELD_RESUME_NS);
}
