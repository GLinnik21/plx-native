//! The foreground machine's drivers: the loop which feeds [`ForegroundLifecycle::step`] its events,
//! runs the effects each step asks for through a [`ForegroundActuator`] and feeds every answering
//! effect's answer back as the next event (`drive_foreground`, `poll_foreground_flight`,
//! `poll_foreground_load`, `park_pending_foreground_flight`). The machine itself, its events and
//! its effects are [`super::lifecycle`]; the actuator's adapter for the app, with the state the
//! effects touch, is `app::lifecycle::PlayerForegroundActuator`, and `app::lifecycle` re-exports
//! the names below, so app code spells them as it always did.
//!
//! Moved out of `lifecycle.rs` verbatim in the Sans-IO lifecycle PR B (a pure move). The only
//! code change to the machine's file is that three of its helpers which the loop calls
//! (`ForegroundEffect::awaits_answer`, `PrepareAnswer::of_outcome`, `PrepareAnswer::of_start`) became
//! `pub(super)`.

use super::lifecycle::{
    Flight, ForegroundClock, ForegroundEffect, ForegroundEvent, ForegroundInput, ForegroundLifecycle,
    ForegroundLoadStart, ForegroundLoadStatus, PrepareAnswer,
};
use plx_machine::machine::Handled;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundActivation {
    Ordinary,
    Handled,
    Launched,
}

pub trait ForegroundActuator {
    type Attempt: Copy + PartialEq;

    /// Prepare the resume at `resume_ns`: settled on the spot, or a flight the machine waits on
    /// ([`Self::land_prepare`]).
    fn prepare_resume(
        &mut self,
        ps: &mut crate::route::PlaybackSession,
        resume_ns: i64,
    ) -> crate::player::ResumeStart;
    /// Drain the pending resume's landing, once a frame.
    fn land_prepare(&mut self, ps: &mut crate::route::PlaybackSession) -> FlightLanding<crate::player::ResumeOutcome>;
    /// Drain the pending Original rollback's landing, once a frame.
    fn land_recovery(
        &mut self,
        ps: &mut crate::route::PlaybackSession,
    ) -> FlightLanding<ForegroundLoadStart<Self::Attempt>>;
    fn before_load(&mut self, resume_ns: i64, clock: ForegroundClock);
    fn start_load(
        &mut self,
        ps: &mut crate::route::PlaybackSession,
    ) -> ForegroundLoadStart<Self::Attempt>;
    fn load_status(&mut self, attempt: Self::Attempt) -> ForegroundLoadStatus<Self::Attempt>;
    fn after_load(
        &mut self,
        ps: &mut crate::route::PlaybackSession,
        attempt: Option<Self::Attempt>,
        clock: ForegroundClock,
        started: bool,
    );
    fn play_clock(&mut self) -> bool;
}

/// What draining a pending flight's landing found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlightLanding<T> {
    /// The worker has not landed.
    Waiting,
    /// The flight ended without landing here (a Back, a newer request): the machine lets go.
    Stale,
    Landed(T),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundPollOutcome {
    Inactive,
    Pending,
    Started,
    Failed,
}

/// What one run of the loop ([`run_foreground`]) reports to its driver.
#[derive(Clone, Copy, Debug)]
struct Run {
    /// Whether the machine consumed the FIRST event; the answers fed back after it are addressed
    /// by id to a state the machine itself just wrote.
    handled: Handled,
    /// A `Launched` effect ran: an exact attempt is in flight, so the caller mounts Player.
    launched: bool,
    /// `started` of the `AfterLoad` which ran, when a Load settled in this run.
    settled: Option<bool>,
}

/// Feed `first` to the machine, execute the effects it asks for in order, and feed each answer
/// back as the next event until nothing is owed. One slot, not a queue: `Prepare`, `Load` and
/// `PlayClock` each owe exactly one answer and a step emits at most one of them (asserted), so
/// there is never more than one event waiting. The effect buffer allocates only when a step emits
/// something, so a frame which only polls (a `Pending` status) allocates nothing.
fn run_foreground<A: ForegroundActuator>(
    lifecycle: &mut ForegroundLifecycle<A::Attempt>,
    ps: &mut crate::route::PlaybackSession,
    actuator: &mut A,
    first: ForegroundEvent<A::Attempt>,
) -> Run {
    let mut run = Run {
        handled: Handled::No,
        launched: false,
        settled: None,
    };
    let mut fx = Vec::new();
    let mut next = Some(first);
    let mut fed_back = false;
    while let Some(ev) = next.take() {
        let handled = lifecycle.step(&ev, &mut fx);
        if fed_back {
            debug_assert_eq!(handled, Handled::Yes, "an answer is addressed to the claim it settles");
        } else {
            run.handled = handled;
            fed_back = true;
        }
        debug_assert!(
            fx.iter().filter(|e| e.awaits_answer()).count() <= 1,
            "a step emits at most one effect which owes an answer"
        );
        for effect in fx.drain(..) {
            match effect {
                ForegroundEffect::Prepare { id, resume_ns } => {
                    // The restore reopens the engine at the parked position: that open is a resume.
                    crate::player::report::note_attempt(crate::player::report::AttemptOrigin::Resume);
                    next = Some(ForegroundEvent::PrepareAnswered {
                        id,
                        answer: PrepareAnswer::of_start(actuator.prepare_resume(ps, resume_ns)),
                    });
                }
                ForegroundEffect::Load {
                    id,
                    resume_ns,
                    clock,
                } => {
                    actuator.before_load(resume_ns, clock);
                    next = Some(ForegroundEvent::LoadAnswered {
                        id,
                        start: actuator.start_load(ps),
                    });
                }
                ForegroundEffect::PlayClock { id } => {
                    next = Some(ForegroundEvent::ClockAnswered {
                        id,
                        accepted: actuator.play_clock(),
                    });
                }
                ForegroundEffect::AfterLoad {
                    attempt,
                    clock,
                    started,
                } => {
                    actuator.after_load(ps, attempt, clock, started);
                    run.settled = Some(started);
                }
                ForegroundEffect::Retire { clock, .. } => {
                    actuator.after_load(ps, None, clock, false);
                }
                ForegroundEffect::Launched { .. } => run.launched = true,
                // `park_pending_foreground_flight`'s: it holds the adapter this loop does not.
                ForegroundEffect::DropFlight => {
                    debug_assert!(false, "only the park driver executes DropFlight")
                }
            }
        }
    }
    run
}

/// Claim first, then execute the claimed synchronous effects in order. Launching the native Load
/// tells the caller to mount Player but deliberately does not issue Play: only the later exact
/// attempt result observed by `poll_foreground_load` may advance the clock. A resume (or an
/// Original rollback) whose rebuild is a flight parks the machine instead; the landing is
/// [`poll_foreground_flight`]'s.
pub fn drive_foreground<A: ForegroundActuator>(
    lifecycle: &mut ForegroundLifecycle<A::Attempt>,
    ps: &mut crate::route::PlaybackSession,
    input: ForegroundInput,
    actuator: &mut A,
) -> ForegroundActivation {
    // The restoring Load's window create is a step of the foreground cycle (`window::CycleScope`).
    let _cycle = plx_telemetry::telemetry::window::CycleScope::enter();
    let run = run_foreground(lifecycle, ps, actuator, ForegroundEvent::Input(input));
    match run.handled {
        Handled::No => ForegroundActivation::Ordinary,
        Handled::Yes if run.launched => ForegroundActivation::Launched,
        Handled::Yes => ForegroundActivation::Handled,
    }
}

/// Once a frame while the machine waits on a flight ([`ForegroundLifecycle::flight_pending`]):
/// drain its landing and carry on. A landed
/// resume continues into the Load; a landed rollback settles the Load with its reload's outcome;
/// a refused rebuild leaves the parked session waiting for the
/// next foreground, and a flight that ended without landing releases the machine. `Launched`
/// tells the caller to mount Player, as [`drive_foreground`]'s does.
pub fn poll_foreground_flight<A: ForegroundActuator>(
    lifecycle: &mut ForegroundLifecycle<A::Attempt>,
    ps: &mut crate::route::PlaybackSession,
    actuator: &mut A,
) -> ForegroundActivation {
    // A restore whose Load waited for a flight starts it here, frames after `foreground_done`;
    // its exported-window create is still the foreground cycle's.
    let _cycle = plx_telemetry::telemetry::window::CycleScope::enter();
    let Some((flight, id)) = lifecycle.pending_flight() else {
        return ForegroundActivation::Handled;
    };
    let landing = match flight {
        Flight::Prepare => match actuator.land_prepare(ps) {
            FlightLanding::Waiting => return ForegroundActivation::Handled,
            FlightLanding::Stale => ForegroundEvent::FlightStale,
            FlightLanding::Landed(outcome) => ForegroundEvent::PrepareAnswered {
                id,
                answer: PrepareAnswer::of_outcome(outcome),
            },
        },
        Flight::Recovery => match actuator.land_recovery(ps) {
            FlightLanding::Waiting => return ForegroundActivation::Handled,
            FlightLanding::Stale => ForegroundEvent::FlightStale,
            FlightLanding::Landed(start) => ForegroundEvent::LoadAnswered { id, start },
        },
    };
    if run_foreground(lifecycle, ps, actuator, landing).launched {
        ForegroundActivation::Launched
    } else {
        ForegroundActivation::Handled
    }
}

/// The OS backgrounded the app again while the machine waits on a flight: drop the flight (its
/// landing is discarded and only the replacement it registered is stopped — the same
/// `suspend_bufferfeed` teardown an Engine's suspend takes) and park the session back where it was
/// suspended, so the next foreground restores it from the start. False when no flight was pending.
pub fn park_pending_foreground_flight<Attempt: Copy + PartialEq>(
    lifecycle: &mut ForegroundLifecycle<Attempt>,
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
) -> bool {
    let mut fx = Vec::new();
    if lifecycle.step(&ForegroundEvent::ParkFlight, &mut fx) == Handled::No {
        return false;
    }
    for effect in fx {
        match effect {
            ForegroundEffect::DropFlight => crate::player::suspend_bufferfeed(ps, pa),
            _ => debug_assert!(false, "ParkFlight emits only DropFlight"),
        }
    }
    true
}

/// Poll the exact native Load attempt after `player::pump` has drained its media-thread result.
/// Only a confirmed `Started` result may advance the viewer clock; failure retains the already
/// prepared route so a Play retry never repeats PMS preparation or `begin_resume`.
pub fn poll_foreground_load<A: ForegroundActuator>(
    lifecycle: &mut ForegroundLifecycle<A::Attempt>,
    ps: &mut crate::route::PlaybackSession,
    actuator: &mut A,
) -> ForegroundPollOutcome {
    let Some(attempt) = lifecycle.pending_load_attempt() else {
        return ForegroundPollOutcome::Inactive;
    };
    let status = actuator.load_status(attempt);
    let run = run_foreground(
        lifecycle,
        ps,
        actuator,
        ForegroundEvent::LoadStatus { attempt, status },
    );
    match (run.handled, run.settled) {
        (Handled::No, _) => ForegroundPollOutcome::Inactive,
        (Handled::Yes, None) => ForegroundPollOutcome::Pending,
        (Handled::Yes, Some(true)) => ForegroundPollOutcome::Started,
        (Handled::Yes, Some(false)) => ForegroundPollOutcome::Failed,
    }
}
