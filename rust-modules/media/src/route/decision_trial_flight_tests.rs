//! **A recovery flight inside an Original trial.** A recovery flight (`FlightOwner::Recovery`: the
//! foreground restore's resume, the rollback's rebase) takes a start transaction a failed or
//! suspended start already holds. When the transaction is an Original trial's, the trial's rollback
//! snapshot (`PendingOriginal`) rides on it, so the flight has to fly WITHOUT erasing it: the retired
//! inline rebuild shared the trial's own transaction, and a flight that refused it left a session
//! suspended mid-trial unrestorable. Each test drives the reducer through one way out of the flight
//! and asks that the snapshot is still there, against the loopback server of [`FlightRig`].

use super::flight_rig::FlightRig;
use super::test_support::settle_pending_native_start;
use super::*;
use std::time::Duration;

const OFFSET_NS: i64 = 90_000_000_000;

fn pending_still_armed() -> bool {
    original_recovery_pending()
}

fn candidate_session() -> String {
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pending_original
        .as_ref()
        .map(|p| p.candidate_projection.tsession.clone())
        .expect("the trial's snapshot")
}

/// Drain a recovery's landing the way the app loop does, waiting (bounded) for the worker.
fn await_landing(ps: &mut PlaybackSession) -> RecoveryLanding {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(landing) = take_ready_recovery_flight(ps) {
            return landing;
        }
        assert!(std::time::Instant::now() < deadline, "timed out waiting for the recovery's landing");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn armed_trial(delay_ms: u64) -> (PlaybackSession, FlightRig) {
    let mut ps = PlaybackSession::IDLE;
    let rig = FlightRig::start(&mut ps, Duration::from_millis(delay_ms));
    FlightRig::arm_original_trial(&mut ps, 90);
    (ps, rig)
}

fn stops_of(rig: &FlightRig) -> usize {
    rig.stopped().iter().filter(|s| s.starts_with("rig-logical-abr-")).count()
}

/// The engine-less rollback's dispatch while an Original trial's phase owns the transaction (its
/// Load failed, the rollback not yet taken) is ACCEPTED: the flight flies on the trial's own
/// transaction (`OriginalTrial(Preparing)`), the snapshot stays armed throughout, and a landing
/// hands the transaction back to the trial as `Prepared`, the route and the snapshot's candidate
/// describing the rebuilt stream.
#[test]
fn an_engineless_rollback_dispatched_inside_a_failed_trial_flies_and_keeps_the_snapshot() {
    let (mut ps, rig) = armed_trial(30);
    FlightRig::fail_original_trial_load(&mut ps);
    assert!(control_phase_label().starts_with("OriginalTrial(Failed("), "{}", control_phase_label());
    let trial_encoder = transcode_session(&ps);

    let dispatched = {
        let _frame = plx_base::task::FrameScope::enter();
        dispatch_engineless_rollback_rebase(&mut ps, OFFSET_NS)
    };
    let RecoveryDispatch::Flying { serial } = dispatched else {
        panic!("a recovery flight was refused inside an Original trial ({})", control_phase_label());
    };
    assert_eq!(control_phase_label(), format!("OriginalTrial(Preparing({serial}))"));
    assert!(engineless_flight_outstanding() && flight_is_current(serial));
    assert!(pending_still_armed(), "dispatching must not erase the trial's snapshot");

    let landing = await_landing(&mut ps);
    assert_eq!(landing.verdict, RecoveryVerdict::Install);
    assert_eq!(control_phase_label(), format!("OriginalTrial(Prepared({serial}))"), "the transaction goes back to the trial");
    assert!(pending_still_armed());
    let rebuilt = transcode_session(&ps);
    assert_ne!(rebuilt, trial_encoder);
    assert_eq!(candidate_session(), rebuilt, "the trial's candidate describes the stream the flight installed");
    cleanup_trial(&mut ps, rig);
}

/// An app-switch suspend (`begin_engine_teardown(true)`) while the flight flies drops it, as it
/// drops every start-owned flight, but the transaction returns to the TRIAL's `Prepared`, not to
/// `Stable`: the snapshot and `PendingOriginal` are untouched, the foreground restore can reserve
/// the trial's transaction again, and the late landing is discarded, stopping only what its worker
/// registered.
#[test]
fn a_suspend_during_a_recovery_flight_in_a_trial_returns_the_trial_and_keeps_the_snapshot() {
    let (mut ps, rig) = armed_trial(60);
    FlightRig::suspend_in_original_trial(&mut ps);
    let before = control_phase_label();
    assert!(before.starts_with("OriginalTrial(Prepared("), "{before}");
    let stops = stops_of(&rig);

    let RecoveryDispatch::Flying { serial } = ({
        let _frame = plx_base::task::FrameScope::enter();
        dispatch_resume_rebase(&mut ps, OFFSET_NS)
    }) else {
        panic!("the foreground restore was refused inside an Original trial ({})", control_phase_label());
    };
    assert_eq!(before, control_phase_label().replace("Preparing", "Prepared"), "the flight kept the trial's own serial");
    begin_engine_teardown(true);
    assert!(!flight_outstanding() && !flight_is_current(serial));
    assert_eq!(control_phase_label(), format!("OriginalTrial(Prepared({serial}))"));
    assert!(pending_still_armed(), "a suspend must not erase the trial's snapshot");
    let again = begin_route_start().expect("the foreground restore must be able to reserve the trial's transaction");
    assert_eq!(again.serial, serial);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while stops_of(&rig) == stops {
        assert!(std::time::Instant::now() < deadline, "the discarded landing's replacement was never stopped");
        discard_stale_flight_landing();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(pending_still_armed());
    assert!(rollback_original_recovery(&mut ps).is_some(), "the trial still rolls back to the retained route");
    cleanup_trial(&mut ps, rig);
}

/// A newer play request during a recovery flight over an Original trial's transaction SUPERSEDES
/// the flight, as it does a plain engine-less flight's (it used to wait for the flight, refusing the
/// request): the landing is discarded stopping only its replacement, the reducer is `Resolving`, the
/// trial's snapshot is untouched, and a resolve that gives up hands the transaction back to the
/// trial as `Prepared` exactly as a suspend does.
#[test]
fn a_newer_play_request_supersedes_a_recovery_flight_in_a_trial_and_hands_the_trial_back() {
    let (mut ps, rig) = armed_trial(60);
    FlightRig::suspend_in_original_trial(&mut ps);
    let stops = stops_of(&rig);
    let RecoveryDispatch::Flying { serial } = ({
        let _frame = plx_base::task::FrameScope::enter();
        dispatch_resume_rebase(&mut ps, OFFSET_NS)
    }) else {
        panic!("the foreground restore was refused inside an Original trial ({})", control_phase_label());
    };
    assert_eq!(control_phase_label(), format!("OriginalTrial(Preparing({serial}))"));

    assert!(FlightRig::begin_newer_play_request(), "an Engine-less flight must not refuse the request");
    assert_eq!(control_phase_label(), "Resolving");
    assert!(!flight_outstanding() && !flight_is_current(serial));
    assert!(pending_still_armed(), "superseding must not erase the trial's snapshot");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while stops_of(&rig) == stops {
        assert!(std::time::Instant::now() < deadline, "the discarded landing's replacement was never stopped");
        discard_stale_flight_landing();
        std::thread::sleep(Duration::from_millis(2));
    }
    cancel_playback_request(&mut ps, false);
    assert_eq!(control_phase_label(), format!("OriginalTrial(Prepared({serial}))"), "the trial gets its transaction back");
    assert!(pending_still_armed());
    assert!(rollback_original_recovery(&mut ps).is_some(), "the trial still rolls back to the retained route");
    cleanup_trial(&mut ps, rig);
}

/// A refused rebuild leaves the trial as a failed open leaves it, `OriginalTrial(Failed)`, with the
/// snapshot armed: the rollback to the retained route is still the way out.
#[test]
fn a_refused_recovery_flight_in_a_trial_leaves_it_failed_with_the_snapshot() {
    let (mut ps, rig) = armed_trial(20);
    FlightRig::suspend_in_original_trial(&mut ps);
    rig.refuse_decisions(1);
    let RecoveryDispatch::Flying { serial } = ({
        let _frame = plx_base::task::FrameScope::enter();
        dispatch_resume_rebase(&mut ps, OFFSET_NS)
    }) else {
        panic!("the foreground restore was refused inside an Original trial ({})", control_phase_label());
    };
    let landing = await_landing(&mut ps);
    assert_eq!(landing.verdict, RecoveryVerdict::Refused);
    assert_eq!(control_phase_label(), format!("OriginalTrial(Failed({serial}))"));
    assert!(pending_still_armed());
    assert!(rollback_original_recovery(&mut ps).is_some(), "the refusal left the rollback available");
    assert_eq!(transcode_session(&ps), "rig-1");
    cleanup_trial(&mut ps, rig);
}

/// A flight is never started over a trial whose Engine is live and unproven: `AwaitingFrame` has
/// a Load attempt in flight, and nothing about it is recoverable by a rebuild.
#[test]
fn a_recovery_flight_is_refused_while_the_trial_awaits_its_first_frame() {
    let (mut ps, rig) = armed_trial(10);
    settle_pending_native_start(&mut ps, RouteStartResult::Started);
    assert!(control_phase_label().starts_with("OriginalTrial(AwaitingFrame("), "{}", control_phase_label());
    let _frame = plx_base::task::FrameScope::enter();
    assert_eq!(dispatch_resume_rebase(&mut ps, OFFSET_NS), RecoveryDispatch::Refused);
    assert!(control_phase_label().starts_with("OriginalTrial(AwaitingFrame("), "refusal must touch nothing");
    assert!(pending_still_armed());
    drop(_frame);
    cleanup_trial(&mut ps, rig);
}

/// Tear the rig down, then reset the process-global reducer, all under the rig's lock: the reset
/// advances the route generation, which a later test's flight must never see move under it.
fn cleanup_trial(ps: &mut PlaybackSession, rig: FlightRig) {
    let _serial = rig.close();
    drop(take_pending_original());
    drop(take_claim_landing());
    reset_player_control_for_test(idle_session_for_test());
    let _ = ps;
}
