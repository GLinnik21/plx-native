//! **The foreground restore's resume of a transcode, driven the way the run loop drives it.** The
//! foreground machine (`player::lifecycle`) used to rebuild the encoder at the saved position inline
//! on the DID-foreground frame (`resume_at` -> `route::transcode_seek`, since deleted: `main-thread block: PMS
//! HTTP`). The rebuild is a flight now (`begin_resume` plans, a worker registers the replacement,
//! `land_resume` installs it) and the machine WAITS on it the way it waits on a pending Load
//! (`ForegroundState::PreparePending`, drained by `poll_foreground_flight`). These tests run every
//! frame-thread half inside a `FrameScope`, against the loopback server of `route::flight_rig`.

use super::lifecycle::*;
use super::*;
use crate::route::flight_rig::FlightRig;
use std::time::Duration;

const RESUME_NS: i64 = 1_800_000_000_000;

/// What the run loop's `PlayerForegroundActuator` does for the resume half, with the Load recorded
/// instead of started (no native Engine is needed to grade what the Load would start on).
struct RigActuator {
    /// `(disp_base, url)` at each `start_load`: the offset the Load would start at.
    loads: Vec<(i64, String)>,
}

impl ForegroundActuator for RigActuator {
    type Attempt = u64;

    fn prepare_resume(&mut self, ps: &mut crate::route::PlaybackSession, resume_ns: i64) -> ResumeStart {
        begin_resume(ps, resume_ns)
    }

    fn land_prepare(&mut self, ps: &mut crate::route::PlaybackSession) -> FlightLanding<ResumeOutcome> {
        match land_resume(ps) {
            Some(outcome) => FlightLanding::Landed(outcome),
            None if crate::route::resume_flight_outstanding() => FlightLanding::Waiting,
            None => FlightLanding::Stale,
        }
    }

    fn land_recovery(&mut self, _ps: &mut crate::route::PlaybackSession) -> FlightLanding<ForegroundLoadStart<u64>> {
        unreachable!("no rollback in this rig")
    }

    fn before_load(&mut self, _resume_ns: i64, _clock: ForegroundClock) {}

    fn start_load(&mut self, ps: &mut crate::route::PlaybackSession) -> ForegroundLoadStart<u64> {
        self.loads.push((SHARED.disp_base.load(Relaxed), crate::route::url(ps)));
        ForegroundLoadStart::Launched(7)
    }

    fn load_status(&mut self, _attempt: u64) -> ForegroundLoadStatus<u64> {
        ForegroundLoadStatus::Pending
    }

    fn after_load(&mut self, _ps: &mut crate::route::PlaybackSession, _a: Option<u64>, _c: ForegroundClock, _s: bool) {}

    fn play_clock(&mut self) -> bool {
        true
    }
}

struct Rig {
    ps: crate::route::PlaybackSession,
    pa: adapter::PlayerAdapter,
    flight: FlightRig,
    lifecycle: ForegroundLifecycle<u64>,
    actuator: RigActuator,
}

impl Rig {
    /// A transcode `rig-1` the OS has just suspended at `RESUME_NS` (paused, so the resume position
    /// is the saved one exactly): no Engine, the start transaction `Stable`, the server's
    /// `/decision` taking `delay_ms`.
    fn new(delay_ms: u64) -> Rig {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let flight = FlightRig::start(&mut ps, Duration::from_millis(delay_ms));
        SHARED.reset_session();
        TX.reset();
        claim_hold::clear();
        let pa = adapter::PlayerAdapter::new(unsafe { MainThread::assume() });
        let mut lifecycle = ForegroundLifecycle::<u64>::IDLE;
        lifecycle.suspend(RESUME_NS, ForegroundClock::Paused);
        let actuator = RigActuator { loads: Vec::new() };
        Rig { ps, pa, flight, lifecycle, actuator }
    }

    /// The DID-foreground frame, exactly as the run loop drives it: inside a `FrameScope`, so a PMS
    /// call without an `allow_blocking` panics.
    fn did_foreground(&mut self) -> ForegroundActivation {
        let _frame = plx_base::task::FrameScope::enter();
        drive_foreground(&mut self.lifecycle, &mut self.ps, ForegroundInput::DidForeground, &mut self.actuator)
    }

    fn poll_frame(&mut self) -> ForegroundActivation {
        let _frame = plx_base::task::FrameScope::enter();
        poll_foreground_flight(&mut self.lifecycle, &mut self.ps, &mut self.actuator)
    }

    /// Frames until the machine stops waiting on its flight, each asking what the loop asks.
    fn frames_until_settled(&mut self) -> ForegroundActivation {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let activation = self.poll_frame();
            if !self.lifecycle.flight_pending() {
                return activation;
            }
            assert!(std::time::Instant::now() < deadline, "the foreground machine hung on its flight");
            assert_eq!(state(&self.ps), PlaybackState::Resolving, "the viewer keeps the spinner");
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

/// **(i)(ii)(v) The DID frame makes no PMS call; the machine waits; the worker lands the Load at the
/// resumed offset.** When the frame returns the server has been asked NOTHING, the machine is parked
/// (`PreparePending`: a second DID or Play is consumed, no Load starts), and the landing's frame
/// carries on into the Load exactly as the inline resume did — `disp_base` seeded, the URL at the
/// offset, one `/decision`.
#[test]
fn a_foreground_resume_frame_makes_no_pms_call_and_the_worker_lands_the_load_at_the_offset() {
    let mut rig = Rig::new(120);
    let activation = rig.did_foreground();
    assert_eq!(
        rig.flight.decisions(),
        0,
        "the foreground frame made a PMS round trip on the frame thread: {:?}",
        rig.flight.requests()
    );
    assert_eq!(activation, ForegroundActivation::Handled);
    assert!(rig.lifecycle.flight_pending());
    assert!(rig.lifecycle.awaiting_load(), "playback must not run on a parked restore");
    assert!(rig.actuator.loads.is_empty(), "no Load before the rebuild landed");
    assert_eq!(state(&rig.ps), PlaybackState::Resolving, "the viewer keeps a spinner");
    for input in [ForegroundInput::DidForeground, ForegroundInput::PlayKey] {
        let _frame = plx_base::task::FrameScope::enter();
        assert_eq!(
            drive_foreground(&mut rig.lifecycle, &mut rig.ps, input, &mut rig.actuator),
            ForegroundActivation::Handled,
            "a second claim must be consumed while the flight flies"
        );
    }
    assert_eq!(rig.flight.decisions(), 0);
    assert!(rig.actuator.loads.is_empty());

    assert_eq!(rig.frames_until_settled(), ForegroundActivation::Launched);
    assert_eq!(rig.flight.decisions(), 1, "exactly one /decision for the restore");
    assert!(matches!(rig.lifecycle.state, ForegroundState::LoadPending { attempt: 7, resume_ns: RESUME_NS, .. }));
    let [(disp_base, url)] = rig.actuator.loads.as_slice() else { panic!("{:?}", rig.actuator.loads) };
    assert_eq!(*disp_base, RESUME_NS, "the Load starts at the resumed offset");
    assert!(url.contains("offset=1800"), "{url}");
    assert_ne!(crate::route::transcode_session(&rig.ps), "rig-1", "the replacement is the route's now");
    rig.flight.wait_for("the offset-0 encoder's retirement", |f| f.stopped().iter().any(|s| s == "rig-1"));
}

/// **(iii) A refused rebuild ends exactly as the inline `None` did**: the machine re-arms the exact
/// suspended snapshot (`Suspended`, nothing launched, nothing hung), the route is untouched and only
/// the refused replacement is stopped.
#[test]
fn a_refused_foreground_rebuild_rearms_the_snapshot_and_hangs_nothing() {
    let mut rig = Rig::new(40);
    let suspended = rig.lifecycle.state;
    rig.flight.refuse_decisions(1);
    assert_eq!(rig.did_foreground(), ForegroundActivation::Handled);
    assert_eq!(rig.frames_until_settled(), ForegroundActivation::Handled);
    assert_eq!(rig.lifecycle.state, suspended, "the refusal re-arms the exact suspended snapshot");
    assert!(rig.actuator.loads.is_empty(), "a refused rebuild starts no Load");
    assert_eq!(crate::route::transcode_session(&rig.ps), "rig-1");
    assert_ne!(state(&rig.ps), PlaybackState::Resolving);
    rig.flight.wait_for("the refused replacement's stop", |_| rig.replacement_stops() == 1);
    assert!(!rig.flight.stopped().iter().any(|s| s == "rig-1"), "{:?}", rig.flight.stopped());
    // …and the next foreground tries again, from the top.
    assert_eq!(rig.did_foreground(), ForegroundActivation::Handled);
    assert_eq!(rig.frames_until_settled(), ForegroundActivation::Launched);
}

/// **(iv) A second background during the flight** drops it: the machine goes back to the snapshot
/// the first suspend left, the landing is discarded and ONLY the replacement the worker registered
/// is stopped (never the encoder the suspended session still names), and the next foreground
/// restores from the top.
#[test]
fn a_second_background_during_the_foreground_flight_parks_the_restore_and_stops_only_its_replacement() {
    let mut rig = Rig::new(150);
    let suspended = rig.lifecycle.state;
    assert_eq!(rig.did_foreground(), ForegroundActivation::Handled);
    rig.flight.wait_for("the restore's request", |f| f.decisions() == 1);

    assert!(park_pending_foreground_flight(&mut rig.lifecycle, &mut rig.ps, &mut rig.pa));
    assert_eq!(rig.lifecycle.state, suspended);
    assert!(!crate::route::engineless_flight_outstanding());
    assert!(crate::route::control_phase_label().contains("Stable"), "{}", crate::route::control_phase_label());
    // The worker lands after the park: the loop's per-frame reap finds it stale.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while rig.replacement_stops() == 0 {
        assert!(std::time::Instant::now() < deadline, "the discarded replacement was never stopped");
        crate::route::discard_stale_flight_landing();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!rig.flight.stopped().iter().any(|s| s == "rig-1"), "{:?}", rig.flight.stopped());
    assert_eq!(crate::route::transcode_session(&rig.ps), "rig-1", "nothing was installed");
    assert!(rig.actuator.loads.is_empty());
    assert!(!park_pending_foreground_flight(&mut rig.lifecycle, &mut rig.ps, &mut rig.pa), "nothing left to park");

    assert_eq!(rig.did_foreground(), ForegroundActivation::Handled);
    assert_eq!(rig.frames_until_settled(), ForegroundActivation::Launched);
    assert_eq!(rig.actuator.loads.len(), 1);
    assert_eq!(rig.replacement_stops(), 1, "the parked flight's replacement is stopped once: {:?}", rig.flight.stopped());
}

/// **(iv)(v) A teardown during the flight** (Back, `stop_bufferfeed`) ends it: the machine does not
/// hang on a landing that will never come — it lets go (`Idle`) once the stale flight is reaped —
/// no Load starts, and only the replacement the worker registered is stopped.
#[test]
fn a_teardown_during_the_foreground_flight_releases_the_machine_and_stops_only_its_replacement() {
    let mut rig = Rig::new(150);
    assert_eq!(rig.did_foreground(), ForegroundActivation::Handled);
    rig.flight.wait_for("the restore's request", |f| f.decisions() == 1);
    stop_bufferfeed(&mut rig.ps, &mut rig.pa);
    assert_eq!(rig.poll_frame(), ForegroundActivation::Handled);
    assert_eq!(rig.lifecycle.state, ForegroundState::Idle, "a flight that ended releases the machine");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while rig.replacement_stops() == 0 {
        assert!(std::time::Instant::now() < deadline, "the discarded replacement was never stopped");
        crate::route::discard_stale_flight_landing();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(rig.actuator.loads.is_empty());
    // (The Back's own stop retires the session's encoder, `rig-1`; the replacement is stopped once.)
    assert_eq!(rig.replacement_stops(), 1, "{:?}", rig.flight.stopped());
}

/// **The `ControlPhase::Idle` verdict (stage 4b's unverified assumption).** A resume from `Idle` is
/// refused by `begin_recovery_flight_start` rather than prepared as `begin_route_start` did (an Original
/// trial's transaction, by contrast, IS taken: see the test below this one), and it
/// is UNREACHABLE: the only way into `Idle` is a COMPLETED stop (`finish_engine_teardown`), which
/// clears the URL and the transcode session in the same teardown, so every resume entry
/// ([`begin_resume`]: the cold one and the foreground restore's) answers `NoRoute` before it can
/// reach the rebuild. An app-switch suspend is a `for_reload` teardown and never reaches `Idle`.
#[test]
fn a_resume_from_idle_never_reaches_the_rebuild_because_a_completed_stop_clears_the_route() {
    let mut rig = Rig::new(10);
    stop_bufferfeed(&mut rig.ps, &mut rig.pa);
    assert!(crate::route::control_phase_label().contains("Idle"), "{}", crate::route::control_phase_label());
    assert!(crate::route::url(&rig.ps).is_empty(), "a completed stop clears the URL");
    assert!(crate::route::transcode_session(&rig.ps).is_empty(), "and the transcode session");
    assert_eq!(begin_resume(&mut rig.ps, RESUME_NS), ResumeStart::Settled(ResumeOutcome::NoRoute));
    assert!(!crate::route::engineless_flight_outstanding());
    assert_eq!(rig.flight.decisions(), 0);
}

/// **An OS suspend INSIDE an Original remux trial is restored by a flight, and the trial keeps its
/// rollback.** The Auto session was trying Original (`PendingOriginal` armed, the HLS route held as
/// the way back) when the app was backgrounded. The foreground restore's rebuild at the saved
/// position must be accepted — the retired inline rebuild shared the trial's own start transaction,
/// and a flight that cannot own one would leave the machine `Suspended` for every Play retry — and
/// flying must not consume the trial's snapshot: a later failed open of the rebuilt Original still
/// rolls back to the retained HLS, and retires the rebuilt encoder the flight installed.
#[test]
fn a_foreground_restore_after_a_suspend_inside_an_original_trial_flies_and_keeps_the_rollback() {
    let mut rig = Rig::new(40);
    FlightRig::arm_original_trial(&mut rig.ps, 1800);
    FlightRig::suspend_in_original_trial(&mut rig.ps);
    let armed = rig.flight.decisions();
    let suspended = rig.lifecycle.state;

    assert_eq!(rig.did_foreground(), ForegroundActivation::Handled);
    assert!(
        rig.lifecycle.flight_pending(),
        "the restore of a session suspended mid-trial was refused ({}): state {:?} instead of {:?}",
        crate::route::control_phase_label(),
        rig.lifecycle.state,
        suspended,
    );
    assert_eq!(rig.flight.decisions(), armed, "the DID frame made a PMS round trip");
    assert!(crate::route::original_recovery_pending(), "flying must not consume the trial's rollback");

    assert_eq!(rig.frames_until_settled(), ForegroundActivation::Launched);
    assert_eq!(rig.flight.decisions(), armed + 1, "exactly one /decision for the restore");
    let [(disp_base, url)] = rig.actuator.loads.as_slice() else { panic!("{:?}", rig.actuator.loads) };
    assert_eq!(*disp_base, RESUME_NS, "the Load starts at the resumed offset");
    assert!(url.contains("offset=1800"), "{url}");
    assert!(crate::route::original_recovery_pending(), "the landing must leave the trial's rollback armed");
    let rebuilt = crate::route::transcode_session(&rig.ps);
    assert_ne!(rebuilt, "rig-1");

    // The rebuilt Original never opens: the way back is the retained HLS, exactly as before.
    let rollback = crate::route::rollback_original_recovery(&mut rig.ps).expect("the trial's snapshot survived the flight");
    assert_eq!(rollback.offset_ns, 1_800_000_000_000);
    assert_eq!(crate::route::transcode_session(&rig.ps), "rig-1", "the retained HLS route is restored");
    rig.flight.wait_for("the rebuilt Original's retirement", |f| f.stopped().iter().any(|s| *s == rebuilt));
}
