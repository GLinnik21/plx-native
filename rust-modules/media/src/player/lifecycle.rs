//! The player's app-switch lifecycle machine.
//!
//! `ForegroundLifecycle` is a tested `State + Event -> Effects` machine. Its state is private and
//! its one writer is `step(&ForegroundEvent, &mut Vec<ForegroundEffect>) -> Handled`; callers read
//! it through `awaiting_load`, `claimable_clock`, `pending_flight` and `pending_load_attempt` (and
//! `state()` under `test` / `test-support`). It reads no global: the pause it parks with arrives as
//! a value on the event (`Park { viewer_paused, .. }`). The drivers which run it
//! ([`super::foreground`]: `drive_foreground`, `poll_foreground_flight`, `poll_foreground_load`)
//! hold one event in a single slot, run the effects each step asks for through a
//! `ForegroundActuator`, and feed every answering effect's answer back as the next event. The
//! transport helpers (`paused`, `viewer_paused`, `set_transport_paused`, ...) are the one door
//! every surface reaches the player's pause through; they are [`super::transport`], the IO side of
//! this contract, and nothing here names them.
//! All of it describes the PLAYER's resume after an OS suspend, and the player machine
//! ([`super::machine::Player`]) holds the `ForegroundLifecycle`, so it lives here rather than in
//! `app::lifecycle` (a layer above the player). The loop's adapter for it,
//! `PlayerForegroundActuator`, needs `app` state and stays in `app::lifecycle`, which re-exports
//! the names of this module, [`super::foreground`] and [`super::transport`] so app code spells
//! them as it always did.
//!
//! Moved out of `app/lifecycle.rs` verbatim in module-layer step L12 (a pure move; the
//! `RESUME_REWIND_NS` constant came with it from `app/mod.rs`). The drivers and the transport
//! helpers left for `foreground.rs` and `transport.rs` in the Sans-IO lifecycle PR B.

use plx_machine::machine::Handled;

/// How far an OS-suspended, still-playing session rewinds when it is resumed (a paused one resumes
/// at exactly the saved offset).
const RESUME_REWIND_NS: i64 = 5_000_000_000;

/// The inputs which move the foreground machine (carried by [`ForegroundEvent::Input`]).
/// `DidForeground` and `PlayKey` may claim a parked playback session — one an OS suspend parked, or
/// one the pipeline's decoder release did
/// (`app::run::park_on_decoder_release`). `DidForeground` restores the saved clock; `PlayKey` is a
/// press that asks to play, which restores it Playing at the exact saved offset: the remote's PLAY
/// key, or a toggle press (PLAYPAUSE, a click on no control) that wakes a decoder-release park
/// saved Paused. `Superseded` is the opposite edge: something else took the session over, and
/// whatever the machine parked is retired (the table is on [`ForegroundLifecycle::step`]).
/// `DidForegroundSuperseded` is the DID foreground that arrives together with something that is
/// about to end the session (a launch link leaving playback): it follows `DidForeground`'s table
/// except that it claims nothing, so a parked session stays parked, to be retired by the
/// `Superseded(Exit)` the ending sends. `PlayKeyOffPlayer` is `PlayKey` pressed with no player on
/// screen: it also drops a started session's pending clock, which belongs to a player a real exit
/// has removed (it is `PlayKey` everywhere else).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundInput {
    DidForeground,
    DidForegroundSuperseded,
    PlayKey,
    PlayKeyOffPlayer,
    Superseded(Successor),
}

/// Who took the session over. They differ in ONE state, `LoadPending`: a new playback leaves the
/// foreground Load to its own settlement (`LoadStatus`'s Stale arm), because the replacement's
/// resolve can still be refused after `PLAY_BUSY` is set; an exit ends the session now, so the
/// Load's armed paused-seek state is released with [`ForegroundEffect::Retire`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Successor {
    Playback,
    Exit,
}

/// Viewer clock intent carried independently of the native Engine's current clock state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundClock {
    Paused,
    Playing,
}

impl ForegroundClock {
    pub fn from_paused(paused: bool) -> Self {
        if paused {
            Self::Paused
        } else {
            Self::Playing
        }
    }
}

/// Work which has been claimed but whose synchronous external effect has not settled yet.
/// Publishing this state BEFORE each call is what makes a nested/duplicate DID or Play harmless.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundClaim {
    Prepare {
        saved_ns: i64,
        saved_clock: ForegroundClock,
        resume_ns: i64,
        clock: ForegroundClock,
    },
    Load {
        resume_ns: i64,
        clock: ForegroundClock,
    },
    PlayClock,
}

/// Complete app-switch resume state. In particular, `Prepared` means `begin_resume` (and its landing) and route
/// preparation have completed, including a transcode URL rebuild when required: retrying its
/// failed native Load must not rebuild that route again.
/// The attempt type is opaque to the reducer. Production uses `RouteStartAttempt`, while pure
/// tests can prove exact-attempt handling with an ordinary integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundState<Attempt = crate::route::RouteStartAttempt> {
    Idle,
    Suspended {
        id: u64,
        saved_ns: i64,
        clock: ForegroundClock,
    },
    Claimed {
        id: u64,
        claim: ForegroundClaim,
    },
    Prepared {
        id: u64,
        resume_ns: i64,
        clock: ForegroundClock,
    },
    LoadPending {
        id: u64,
        attempt: Attempt,
        resume_ns: i64,
        clock: ForegroundClock,
    },
    /// The resume's rebuild of a transcode at the saved position is a flight (`begin_resume`): the
    /// machine waits on its landing the way it waits on a pending Load, and settles the claim's
    /// `Prepare` when it lands.
    PreparePending {
        id: u64,
        saved_ns: i64,
        saved_clock: ForegroundClock,
        resume_ns: i64,
        clock: ForegroundClock,
    },
    /// The rollback of an Original trial whose Load failed before an Engine existed is a flight:
    /// the machine waits on its landing, then settles the claim's `Load` with the reload's outcome.
    RecoveryPending {
        id: u64,
        resume_ns: i64,
        clock: ForegroundClock,
    },
    ClockPending {
        id: u64,
    },
}

/// What the machine is told. Every way into [`ForegroundLifecycle`] is one of these, handed to
/// [`ForegroundLifecycle::step`]: the three that come from outside the loop (`Park`, `ParkFlight`,
/// `Input`) and the answers an effect's execution feeds back (`PrepareAnswered`, `LoadAnswered`,
/// `FlightStale`, `LoadStatus`, `ClockAnswered`). An answer is addressed by the id (or exact
/// attempt) of the claim it settles, and one that no longer owns the state is not handled.
///
/// | Event | Accepted in | Next state |
/// |---|---|---|
/// | `Park` | any | `Suspended` (a fresh id) |
/// | `ParkFlight` | `PreparePending`, `RecoveryPending` | `Suspended` (same id), effect `DropFlight` |
/// | `Input` | see [`ForegroundLifecycle::step`] | |
/// | `PrepareAnswered` | `Claimed{Prepare}` (the synchronous answer), `PreparePending` (the landing) | `Claimed{Load}` and effect `Load`; `Suspended` when refused; `PreparePending` when it is a flight |
/// | `LoadAnswered` | `Claimed{Load}` (the synchronous answer), `RecoveryPending` (the landing) | `LoadPending` and effect `Launched`; `RecoveryPending` when it is a flight; `Prepared` when refused; `Idle` when terminal |
/// | `FlightStale` | `PreparePending`, `RecoveryPending` | `Idle` |
/// | `LoadStatus` | `LoadPending` of that exact attempt | per [`ForegroundLoadStatus`] |
/// | `ClockAnswered` | `Claimed{PlayClock}` of that id | `Idle`, or `ClockPending` when refused |
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundEvent<Attempt = crate::route::RouteStartAttempt> {
    /// The OS (or the pipeline's decoder release) took the playing session away. `saved_ns` is the
    /// intended position; `viewer_paused` is the transport the VIEWER sees ([`super::transport::viewer_paused`]),
    /// read by the caller because the machine reads no global.
    Park { saved_ns: i64, viewer_paused: bool },
    /// An app-switch suspend arrived while a flight was pending: drop it and park the session back
    /// where it was suspended.
    ParkFlight,
    /// A DID foreground, a PLAY press or a supersession ([`ForegroundInput`]).
    Input(ForegroundInput),
    /// What preparing the resume answered: on the spot, or (as a landing) from its flight.
    PrepareAnswered { id: u64, answer: PrepareAnswer },
    /// What starting the claimed Load answered: on the spot, or (as a landing) the Original
    /// rollback's reload.
    LoadAnswered {
        id: u64,
        start: ForegroundLoadStart<Attempt>,
    },
    /// The pending flight ended without landing here (a Back, a newer request): the machine lets go.
    FlightStale,
    /// What the exact native Load attempt reported.
    LoadStatus {
        attempt: Attempt,
        status: ForegroundLoadStatus<Attempt>,
    },
    /// Whether the native clock accepted the foreground's Play.
    ClockAnswered { id: u64, accepted: bool },
}

/// What preparing a resume answered (`PrepareAnswered`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrepareAnswer {
    Prepared,
    /// No route to resume on, or the rebuild was rejected: the session stays suspended.
    Refused,
    /// A flight owns the rebuild; the landing answers.
    Pending,
}

impl PrepareAnswer {
    /// The answer a landed (or settled) resume gives.
    pub(super) fn of_outcome(outcome: crate::player::ResumeOutcome) -> Self {
        if matches!(outcome, crate::player::ResumeOutcome::Prepared) {
            Self::Prepared
        } else {
            Self::Refused
        }
    }

    /// The answer `begin_resume` gives.
    pub(super) fn of_start(start: crate::player::ResumeStart) -> Self {
        match start {
            crate::player::ResumeStart::Settled(outcome) => Self::of_outcome(outcome),
            crate::player::ResumeStart::Pending => Self::Pending,
        }
    }
}

/// Which flight the machine waits on ([`ForegroundLifecycle::pending_flight`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flight {
    /// The resume's transcode rebuild (`PreparePending`).
    Prepare,
    /// The Original trial's rollback (`RecoveryPending`).
    Recovery,
}

/// The machine's state is private and its one writer is [`step`](Self::step): there is no mutator
/// beside it for a caller to reach, so a new way in is a new [`ForegroundEvent`].
#[derive(Debug)]
pub struct ForegroundLifecycle<Attempt = crate::route::RouteStartAttempt> {
    state: ForegroundState<Attempt>,
    next_id: u64,
}

impl<Attempt: Copy + PartialEq> ForegroundLifecycle<Attempt> {
    pub const IDLE: Self = Self {
        state: ForegroundState::Idle,
        next_id: 0,
    };

    /// Apply one event, pushing what the caller must now execute onto `fx` in order, and say
    /// whether the machine consumed it. `Handled::No` covers an `Input` that arrives in `Idle` (the
    /// ordinary start path) and an answer whose id or attempt no longer owns the state; the state
    /// does not move for it, except for `PlayKeyOffPlayer` (below). A step leaves the state
    /// already settled for the effects it emits: an effect's answer is the NEXT event.
    ///
    /// `Input(Superseded(successor))` retires the parked owner, per this table:
    ///
    /// | State | `Playback` | `Exit` |
    /// |---|---|---|
    /// | `Idle` | Idle, `No` | Idle, `No` |
    /// | `Suspended`, `Prepared`, `PreparePending`, `RecoveryPending`, `Claimed{Prepare\|Load}` | Idle, `Yes` | Idle, `Yes` |
    /// | `Claimed{PlayClock}`, `ClockPending` | unchanged, `Yes` | Idle, `Yes` |
    /// | `LoadPending{clock}` | unchanged, `Yes` (its settlement owns it) | Idle, `Yes` and `Retire{clock}` |
    ///
    /// `DidForegroundSuperseded` claims nothing: `Suspended` and `Prepared` stay and answer `Yes`,
    /// `Idle` answers `No`, and every other state answers as it does for `DidForeground` (already
    /// `Yes`). The session stays awaiting its load, so playback stays off until the superseding
    /// `Exit` retires it.
    ///
    /// `PlayKeyOffPlayer` is `PlayKey` pressed with no player on screen: a started session's
    /// pending clock (`ClockPending`, `Claimed{PlayClock}`) belongs to a player which a real exit
    /// has since removed, so the press must not be intercepted by its retry. There it moves to
    /// `Idle` and answers `No` (the ordinary start path); in every other state it is `PlayKey`.
    ///
    /// `Prepared` is retired under both successors: its failed Load already released what
    /// `before_load` armed, and a Retry starts a fresh play, which supersedes it anyway.
    pub fn step(
        &mut self,
        ev: &ForegroundEvent<Attempt>,
        fx: &mut Vec<ForegroundEffect<Attempt>>,
    ) -> Handled {
        match *ev {
            ForegroundEvent::Park {
                saved_ns,
                viewer_paused,
            } => {
                let clock = self.clock_for_suspend(viewer_paused);
                self.next_id = self.next_id.wrapping_add(1);
                if self.next_id == 0 {
                    self.next_id = 1;
                }
                self.state = ForegroundState::Suspended {
                    id: self.next_id,
                    saved_ns,
                    clock,
                };
                Handled::Yes
            }
            ForegroundEvent::ParkFlight => self.park_flight(fx),
            ForegroundEvent::Input(input) => self.input(input, fx),
            ForegroundEvent::PrepareAnswered { id, answer } => self.prepare_answered(id, answer, fx),
            ForegroundEvent::LoadAnswered { id, start } => self.load_answered(id, start, fx),
            ForegroundEvent::FlightStale => {
                if self.pending_flight().is_none() {
                    return Handled::No;
                }
                self.state = ForegroundState::Idle;
                Handled::Yes
            }
            ForegroundEvent::LoadStatus { attempt, status } => self.load_status(attempt, status, fx),
            ForegroundEvent::ClockAnswered { id, accepted } => {
                if !matches!(
                    self.state,
                    ForegroundState::Claimed {
                        id: owner,
                        claim: ForegroundClaim::PlayClock,
                    } if owner == id
                ) {
                    return Handled::No;
                }
                self.state = if accepted {
                    ForegroundState::Idle
                } else {
                    ForegroundState::ClockPending { id }
                };
                Handled::Yes
            }
        }
    }

    /// The physical clock can remain Paused after a refused foreground Play. Preserve the
    /// viewer's newer Playing intent if the OS backgrounds that live session again.
    fn clock_for_suspend(&self, transport_paused: bool) -> ForegroundClock {
        match self.state {
            ForegroundState::ClockPending { .. }
            | ForegroundState::Claimed {
                claim: ForegroundClaim::PlayClock,
                ..
            } => ForegroundClock::Playing,
            // A Play-key resume can still have a physically Paused clock until its exact Load
            // settles. If the OS backgrounds again in that window, preserve the viewer's intent
            // from the reducer rather than resnapshotting the deliberately stale native clock.
            ForegroundState::LoadPending { clock, .. }
            | ForegroundState::PreparePending { saved_clock: clock, .. }
            | ForegroundState::RecoveryPending { clock, .. } => clock,
            _ => ForegroundClock::from_paused(transport_paused),
        }
    }

    /// True while the preserved session is parked and awaiting a new native Load launch. The
    /// launched attempt is deliberately excluded: its Player route is live and a second OS
    /// background edge must suspend that Engine rather than ignore it.
    pub fn awaiting_load(&self) -> bool {
        matches!(
            self.state,
            ForegroundState::Suspended { .. }
                | ForegroundState::Prepared { .. }
                | ForegroundState::PreparePending { .. }
                | ForegroundState::RecoveryPending { .. }
                | ForegroundState::Claimed {
                    claim: ForegroundClaim::Prepare { .. } | ForegroundClaim::Load { .. },
                    ..
                }
        )
    }

    /// The clock an `Input(DidForeground)` would restore with: the saved snapshot's while it is
    /// `Suspended`, or the prepared one's after a refused Load. `None` in every state
    /// whose claim restores no clock, so a press can ask what the parked session would come back
    /// as before choosing the input it claims with.
    pub fn claimable_clock(&self) -> Option<ForegroundClock> {
        match self.state {
            ForegroundState::Suspended { clock, .. } | ForegroundState::Prepared { clock, .. } => {
                Some(clock)
            }
            _ => None,
        }
    }

    /// Read-only view of the machine for tests in other crates; every write goes through
    /// [`step`](Self::step).
    #[cfg(any(test, feature = "test-support"))]
    pub fn state(&self) -> ForegroundState<Attempt> {
        self.state
    }

    /// Whether the machine is waiting on a flight's landing ([`super::foreground::poll_foreground_flight`]).
    pub fn flight_pending(&self) -> bool {
        self.pending_flight().is_some()
    }

    /// The flight the machine waits on and the id of the claim it settles: the landing's event
    /// carries that id, so a landing which no longer owns the state is not handled.
    pub fn pending_flight(&self) -> Option<(Flight, u64)> {
        match self.state {
            ForegroundState::PreparePending { id, .. } => Some((Flight::Prepare, id)),
            ForegroundState::RecoveryPending { id, .. } => Some((Flight::Recovery, id)),
            _ => None,
        }
    }

    pub fn pending_load_attempt(&self) -> Option<Attempt> {
        match self.state {
            ForegroundState::LoadPending { attempt, .. } => Some(attempt),
            _ => None,
        }
    }

    /// `Input`: a DID foreground, a PLAY press or a supersession.
    fn input(&mut self, input: ForegroundInput, fx: &mut Vec<ForegroundEffect<Attempt>>) -> Handled {
        if let ForegroundInput::Superseded(successor) = input {
            return self.supersede(successor, fx);
        }
        // `ClockPending` belongs to an already-loaded player. If a later real exit has removed
        // that route, its retry must not intercept Play for the next item.
        if matches!(input, ForegroundInput::PlayKeyOffPlayer)
            && matches!(
                self.state,
                ForegroundState::ClockPending { .. }
                    | ForegroundState::Claimed {
                        claim: ForegroundClaim::PlayClock,
                        ..
                    }
            )
        {
            self.state = ForegroundState::Idle;
            return Handled::No;
        }
        let play = matches!(
            input,
            ForegroundInput::PlayKey | ForegroundInput::PlayKeyOffPlayer
        );
        let state = self.state;
        match state {
            ForegroundState::Idle => Handled::No,
            // Nothing is claimed: the parked states stay as they are, exactly as the pending ones
            // do for every input.
            ForegroundState::Suspended { .. } | ForegroundState::Prepared { .. }
                if matches!(input, ForegroundInput::DidForegroundSuperseded) =>
            {
                Handled::Yes
            }
            ForegroundState::Suspended {
                id,
                saved_ns,
                clock: saved_clock,
            } => {
                let clock = if play {
                    ForegroundClock::Playing
                } else {
                    saved_clock
                };
                let resume_ns = if matches!(saved_clock, ForegroundClock::Paused) {
                    saved_ns
                } else {
                    saved_ns.saturating_sub(RESUME_REWIND_NS).max(0)
                };
                self.state = ForegroundState::Claimed {
                    id,
                    claim: ForegroundClaim::Prepare {
                        saved_ns,
                        saved_clock,
                        resume_ns,
                        clock,
                    },
                };
                fx.push(ForegroundEffect::Prepare { id, resume_ns });
                Handled::Yes
            }
            ForegroundState::Prepared {
                id,
                resume_ns,
                clock,
            } => {
                let clock = if play { ForegroundClock::Playing } else { clock };
                self.state = ForegroundState::Claimed {
                    id,
                    claim: ForegroundClaim::Load { resume_ns, clock },
                };
                fx.push(ForegroundEffect::Load {
                    id,
                    resume_ns,
                    clock,
                });
                Handled::Yes
            }
            ForegroundState::ClockPending { id } if play => {
                self.state = ForegroundState::Claimed {
                    id,
                    claim: ForegroundClaim::PlayClock,
                };
                fx.push(ForegroundEffect::PlayClock { id });
                Handled::Yes
            }
            // A lifecycle edge or key which arrives while another claimant owns the effect is
            // consumed, not allowed to fall through to the ordinary start path.
            ForegroundState::Claimed { .. }
            | ForegroundState::ClockPending { .. }
            | ForegroundState::LoadPending { .. }
            | ForegroundState::PreparePending { .. }
            | ForegroundState::RecoveryPending { .. } => Handled::Yes,
        }
    }

    fn supersede(
        &mut self,
        successor: Successor,
        fx: &mut Vec<ForegroundEffect<Attempt>>,
    ) -> Handled {
        let exit = matches!(successor, Successor::Exit);
        match self.state {
            ForegroundState::Idle => Handled::No,
            ForegroundState::LoadPending { clock, id, .. } if exit => {
                self.state = ForegroundState::Idle;
                fx.push(ForegroundEffect::Retire { id, clock });
                Handled::Yes
            }
            ForegroundState::LoadPending { .. } => Handled::Yes,
            ForegroundState::Claimed {
                claim: ForegroundClaim::PlayClock,
                ..
            }
            | ForegroundState::ClockPending { .. } => {
                if exit {
                    self.state = ForegroundState::Idle;
                }
                Handled::Yes
            }
            ForegroundState::Suspended { .. }
            | ForegroundState::Claimed { .. }
            | ForegroundState::Prepared { .. }
            | ForegroundState::PreparePending { .. }
            | ForegroundState::RecoveryPending { .. } => {
                self.state = ForegroundState::Idle;
                Handled::Yes
            }
        }
    }

    /// `ParkFlight`: the landing is discarded, and the parked session goes back to waiting for the
    /// next foreground exactly where it was (a pending rollback has no snapshot but its resume
    /// position, which the next restore rebuilds from).
    fn park_flight(&mut self, fx: &mut Vec<ForegroundEffect<Attempt>>) -> Handled {
        self.state = match self.state {
            ForegroundState::PreparePending {
                id,
                saved_ns,
                saved_clock,
                ..
            } => ForegroundState::Suspended {
                id,
                saved_ns,
                clock: saved_clock,
            },
            ForegroundState::RecoveryPending {
                id,
                resume_ns,
                clock,
            } => ForegroundState::Suspended {
                id,
                saved_ns: resume_ns,
                clock,
            },
            _ => return Handled::No,
        };
        fx.push(ForegroundEffect::DropFlight);
        Handled::Yes
    }

    /// `PrepareAnswered`: settles the claimed `Prepare`, whether it answered on the spot or its
    /// flight has landed.
    fn prepare_answered(
        &mut self,
        id: u64,
        answer: PrepareAnswer,
        fx: &mut Vec<ForegroundEffect<Attempt>>,
    ) -> Handled {
        let (owner, saved_ns, saved_clock, resume_ns, clock) = match self.state {
            ForegroundState::Claimed {
                id: owner,
                claim:
                    ForegroundClaim::Prepare {
                        saved_ns,
                        saved_clock,
                        resume_ns,
                        clock,
                    },
            }
            | ForegroundState::PreparePending {
                id: owner,
                saved_ns,
                saved_clock,
                resume_ns,
                clock,
            } => (owner, saved_ns, saved_clock, resume_ns, clock),
            _ => return Handled::No,
        };
        if owner != id {
            return Handled::No;
        }
        match answer {
            // The claimed `Prepare` is a flight: park until its landing.
            PrepareAnswer::Pending => {
                self.state = ForegroundState::PreparePending {
                    id,
                    saved_ns,
                    saved_clock,
                    resume_ns,
                    clock,
                };
            }
            PrepareAnswer::Refused => {
                self.state = ForegroundState::Suspended {
                    id,
                    saved_ns,
                    clock: saved_clock,
                };
            }
            PrepareAnswer::Prepared => {
                self.state = ForegroundState::Claimed {
                    id,
                    claim: ForegroundClaim::Load { resume_ns, clock },
                };
                fx.push(ForegroundEffect::Load {
                    id,
                    resume_ns,
                    clock,
                });
            }
        }
        Handled::Yes
    }

    /// `LoadAnswered`: settles the claimed `Load`, whether starting it answered on the spot or its
    /// Original rollback's flight has landed. A refusal or a terminal answer releases what
    /// `before_load` armed (`AfterLoad`) with no exact attempt to retire.
    fn load_answered(
        &mut self,
        id: u64,
        start: ForegroundLoadStart<Attempt>,
        fx: &mut Vec<ForegroundEffect<Attempt>>,
    ) -> Handled {
        let (owner, resume_ns, clock) = match self.state {
            ForegroundState::Claimed {
                id: owner,
                claim: ForegroundClaim::Load { resume_ns, clock },
            }
            | ForegroundState::RecoveryPending {
                id: owner,
                resume_ns,
                clock,
            } => (owner, resume_ns, clock),
            _ => return Handled::No,
        };
        if owner != id {
            return Handled::No;
        }
        match start {
            ForegroundLoadStart::Launched(attempt) => {
                self.state = ForegroundState::LoadPending {
                    id,
                    attempt,
                    resume_ns,
                    clock,
                };
                fx.push(ForegroundEffect::Launched { id });
            }
            // The claimed `Load`'s Original rollback is a flight: park until its landing.
            ForegroundLoadStart::Pending => {
                self.state = ForegroundState::RecoveryPending {
                    id,
                    resume_ns,
                    clock,
                };
            }
            ForegroundLoadStart::Failed => {
                self.state = ForegroundState::Prepared {
                    id,
                    resume_ns,
                    clock,
                };
                fx.push(ForegroundEffect::AfterLoad {
                    attempt: None,
                    clock,
                    started: false,
                });
            }
            // No exact candidate can be followed. `AlreadyRunning` is a stable Engine outside this
            // suspended lifecycle; `Terminal` means Original rollback could not construct a truthful
            // route. Neither is a retryable Prepared edge.
            ForegroundLoadStart::AlreadyRunning | ForegroundLoadStart::Terminal => {
                self.state = ForegroundState::Idle;
                fx.push(ForegroundEffect::AfterLoad {
                    attempt: None,
                    clock,
                    started: false,
                });
            }
        }
        Handled::Yes
    }

    /// `LoadStatus`: the exact native Load attempt reported. A finished one emits `AfterLoad`, and
    /// a Started one with the clock Playing then `PlayClock`, in that order.
    fn load_status(
        &mut self,
        attempt: Attempt,
        status: ForegroundLoadStatus<Attempt>,
        fx: &mut Vec<ForegroundEffect<Attempt>>,
    ) -> Handled {
        let ForegroundState::LoadPending {
            id,
            attempt: owner,
            resume_ns,
            clock,
        } = self.state
        else {
            return Handled::No;
        };
        if owner != attempt {
            return Handled::No;
        }
        match status {
            ForegroundLoadStatus::Pending => {}
            ForegroundLoadStatus::Superseded(replacement) => {
                self.state = ForegroundState::LoadPending {
                    id,
                    attempt: replacement,
                    resume_ns,
                    clock,
                };
            }
            ForegroundLoadStatus::Failed => {
                self.state = ForegroundState::Prepared {
                    id,
                    resume_ns,
                    clock,
                };
                fx.push(ForegroundEffect::AfterLoad {
                    attempt: Some(attempt),
                    clock,
                    started: false,
                });
            }
            ForegroundLoadStatus::Stale => {
                // Neither the observed Load nor a tokened replacement owns the route any more.
                // Release foreground ownership instead of retrying an unknown candidate or
                // destroying whichever Engine the main reducer may now own.
                self.state = ForegroundState::Idle;
                fx.push(ForegroundEffect::AfterLoad {
                    attempt: None,
                    clock,
                    started: false,
                });
            }
            ForegroundLoadStatus::Started => {
                fx.push(ForegroundEffect::AfterLoad {
                    attempt: Some(attempt),
                    clock,
                    started: true,
                });
                if matches!(clock, ForegroundClock::Paused) {
                    self.state = ForegroundState::Idle;
                } else {
                    self.state = ForegroundState::Claimed {
                        id,
                        claim: ForegroundClaim::PlayClock,
                    };
                    fx.push(ForegroundEffect::PlayClock { id });
                }
            }
        }
        Handled::Yes
    }
}

/// What a [`step`](ForegroundLifecycle::step) asks its caller to do, in order. `Prepare`, `Load`
/// and `PlayClock` each owe the machine ONE answer event (a step never emits two); the rest are
/// settled by the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundEffect<Attempt = crate::route::RouteStartAttempt> {
    Prepare {
        id: u64,
        resume_ns: i64,
    },
    Load {
        id: u64,
        resume_ns: i64,
        clock: ForegroundClock,
    },
    PlayClock {
        id: u64,
    },
    /// Release foreground ownership with `clock` unapplied: the Stale settlement's
    /// `after_load(ps, None, clock, false)`, performed by the actuator. No engine stop.
    Retire {
        id: u64,
        clock: ForegroundClock,
    },
    /// A Load settled (or its start was refused): the actuator's `after_load`. `attempt` is the
    /// exact attempt to retire when it failed, the started one when it started, and `None` when
    /// there is nothing to retire.
    AfterLoad {
        attempt: Option<Attempt>,
        clock: ForegroundClock,
        started: bool,
    },
    /// The Load launched the exact attempt, so the caller mounts Player
    /// ([`super::foreground::ForegroundActivation::Launched`]). The Load itself does not issue Play.
    Launched { id: u64 },
    /// The pending flight was dropped by `ParkFlight`: stop what it registered
    /// (`suspend_bufferfeed`, the teardown an Engine's suspend takes).
    DropFlight,
}

impl<Attempt> ForegroundEffect<Attempt> {
    /// Whether the effect owes the machine an answer event (see the type's doc).
    pub(super) fn awaits_answer(&self) -> bool {
        matches!(
            self,
            Self::Prepare { .. } | Self::Load { .. } | Self::PlayClock { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundLoadStart<Attempt> {
    /// A rollback flight owns the way forward ([`super::foreground::ForegroundActuator::land_recovery`]).
    Pending,
    AlreadyRunning,
    Launched(Attempt),
    Failed,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundLoadStatus<Attempt> {
    Pending,
    Started,
    Failed,
    Superseded(Attempt),
    Stale,
}

#[cfg(test)]
mod foreground_resume_tests {
    use super::*;
    use crate::player::foreground::*;

    /// The three-way answer an `Input` used to return (`Handled::No` was `Ordinary`; `Yes` with
    /// nothing to execute was `Suppressed`), kept so the tables below read as they always did.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum ForegroundClaimResult {
        Ordinary,
        Suppressed,
        Effect(ForegroundEffect<u64>),
    }

    fn claim(lifecycle: &mut ForegroundLifecycle<u64>, input: ForegroundInput) -> ForegroundClaimResult {
        let mut fx = Vec::new();
        let handled = lifecycle.step(&ForegroundEvent::Input(input), &mut fx);
        match (handled, fx.as_slice()) {
            (Handled::No, []) => ForegroundClaimResult::Ordinary,
            (Handled::Yes, []) => ForegroundClaimResult::Suppressed,
            (Handled::Yes, [effect]) => ForegroundClaimResult::Effect(*effect),
            other => panic!("an Input answers No, or Yes with at most one effect: {other:?}"),
        }
    }

    /// `Park` with the transport reading `clock` (the viewer's pause), which the machine saves
    /// unless its own state knows better: the saved clock is asserted to be the one asked for, so a
    /// test which parks over such a state says so.
    fn suspend(lifecycle: &mut ForegroundLifecycle<u64>, saved_ns: i64, clock: ForegroundClock) {
        let mut fx = Vec::new();
        let park = ForegroundEvent::Park {
            saved_ns,
            viewer_paused: matches!(clock, ForegroundClock::Paused),
        };
        assert_eq!(lifecycle.step(&park, &mut fx), Handled::Yes);
        assert!(fx.is_empty(), "a park has nothing to execute");
        assert!(
            matches!(lifecycle.state, ForegroundState::Suspended { clock: saved, .. } if saved == clock),
            "parked with {clock:?}: {:?}",
            lifecycle.state
        );
    }

    /// `ParkFlight`: whether a flight was pending, and a pending one is dropped by `DropFlight`.
    fn park_flight(lifecycle: &mut ForegroundLifecycle<u64>) -> bool {
        let mut fx = Vec::new();
        let handled = lifecycle.step(&ForegroundEvent::ParkFlight, &mut fx);
        match handled {
            Handled::Yes => assert_eq!(fx, vec![ForegroundEffect::DropFlight]),
            Handled::No => assert!(fx.is_empty()),
        }
        handled == Handled::Yes
    }

    #[derive(Default)]
    struct FakeActuator {
        prepare: Vec<crate::player::ResumeOutcome>,
        /// How many `prepare_resume` calls answer `Pending` (a flight) before `prepare` answers.
        pending_prepares: usize,
        land_prepares: Vec<FlightLanding<crate::player::ResumeOutcome>>,
        land_recoveries: Vec<FlightLanding<ForegroundLoadStart<u64>>>,
        loads: Vec<ForegroundLoadStart<u64>>,
        statuses: Vec<ForegroundLoadStatus<u64>>,
        clocks: Vec<bool>,
        prepare_calls: Vec<i64>,
        before_loads: Vec<(i64, ForegroundClock)>,
        status_calls: Vec<u64>,
        after_loads: Vec<(Option<u64>, ForegroundClock, bool)>,
        teardowns: usize,
        load_calls: usize,
        clock_calls: usize,
    }

    impl FakeActuator {
        fn answer<T>(answers: &mut Vec<T>) -> T {
            answers.remove(0)
        }
    }

    impl ForegroundActuator for FakeActuator {
        type Attempt = u64;

        fn prepare_resume(
            &mut self,
            _ps: &mut crate::route::PlaybackSession,
            resume_ns: i64,
        ) -> crate::player::ResumeStart {
            self.prepare_calls.push(resume_ns);
            if self.pending_prepares > 0 {
                self.pending_prepares -= 1;
                return crate::player::ResumeStart::Pending;
            }
            crate::player::ResumeStart::Settled(Self::answer(&mut self.prepare))
        }

        fn land_prepare(
            &mut self,
            _ps: &mut crate::route::PlaybackSession,
        ) -> FlightLanding<crate::player::ResumeOutcome> {
            Self::answer(&mut self.land_prepares)
        }

        fn land_recovery(
            &mut self,
            _ps: &mut crate::route::PlaybackSession,
        ) -> FlightLanding<ForegroundLoadStart<Self::Attempt>> {
            Self::answer(&mut self.land_recoveries)
        }

        fn before_load(&mut self, resume_ns: i64, clock: ForegroundClock) {
            self.before_loads.push((resume_ns, clock));
        }

        fn start_load(
            &mut self,
            _ps: &mut crate::route::PlaybackSession,
        ) -> ForegroundLoadStart<Self::Attempt> {
            self.load_calls += 1;
            Self::answer(&mut self.loads)
        }

        fn load_status(&mut self, attempt: Self::Attempt) -> ForegroundLoadStatus<Self::Attempt> {
            self.status_calls.push(attempt);
            Self::answer(&mut self.statuses)
        }

        fn after_load(
            &mut self,
            _ps: &mut crate::route::PlaybackSession,
            attempt: Option<Self::Attempt>,
            clock: ForegroundClock,
            started: bool,
        ) {
            self.after_loads.push((attempt, clock, started));
            if !started && attempt.is_some() {
                self.teardowns += 1;
            }
        }

        fn play_clock(&mut self) -> bool {
            self.clock_calls += 1;
            Self::answer(&mut self.clocks)
        }
    }

    /// **Only a snapshot a claim would restore has a claimable clock.** `Suspended` and `Prepared`
    /// carry the clock a `DidForeground` claim restores; every other state's claim restores none
    /// (it is idle, owned by another claimant, or past its Load).
    #[test]
    fn claimable_clock_reads_only_the_claimable_snapshot() {
        let mut lifecycle = ForegroundLifecycle::<u64>::IDLE;
        assert_eq!(lifecycle.claimable_clock(), None);
        for clock in [ForegroundClock::Paused, ForegroundClock::Playing] {
            suspend(&mut lifecycle, 73_000_000_000, clock);
            assert_eq!(lifecycle.claimable_clock(), Some(clock), "Suspended {clock:?}");
            lifecycle.state = ForegroundState::Prepared {
                id: 1,
                resume_ns: 73_000_000_000,
                clock,
            };
            assert_eq!(lifecycle.claimable_clock(), Some(clock), "Prepared {clock:?}");
        }
        lifecycle.state = ForegroundState::LoadPending {
            id: 1,
            attempt: 11,
            resume_ns: 73_000_000_000,
            clock: ForegroundClock::Paused,
        };
        assert_eq!(lifecycle.claimable_clock(), None, "a pending Load is past its claim");
        suspend(&mut lifecycle, 73_000_000_000, ForegroundClock::Paused);
        let _ = claim(&mut lifecycle, ForegroundInput::DidForeground);
        assert_eq!(lifecycle.claimable_clock(), None, "a Claimed snapshot is another claimant's");
    }

    #[test]
    fn a_claim_suppresses_did_and_play_until_its_effect_settles() {
        let mut lifecycle = ForegroundLifecycle::<u64>::IDLE;
        suspend(&mut lifecycle, 73_000_000_000, ForegroundClock::Paused);
        let first = claim(&mut lifecycle, ForegroundInput::PlayKey);
        assert_eq!(
            first,
            ForegroundClaimResult::Effect(ForegroundEffect::Prepare {
                id: 1,
                resume_ns: 73_000_000_000,
            })
        );
        assert_eq!(
            claim(&mut lifecycle, ForegroundInput::DidForeground),
            ForegroundClaimResult::Suppressed
        );
        assert_eq!(
            claim(&mut lifecycle, ForegroundInput::PlayKey),
            ForegroundClaimResult::Suppressed
        );
    }

    #[test]
    fn refused_resume_preparation_rearms_the_exact_suspended_snapshot() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        for refused in [
            crate::player::ResumeOutcome::NoRoute,
            crate::player::ResumeOutcome::RebuildRejected,
        ] {
            let mut lifecycle = ForegroundLifecycle::IDLE;
            suspend(&mut lifecycle, 91_000_000_000, ForegroundClock::Paused);
            let saved = lifecycle.state;
            let mut actuator = FakeActuator {
                prepare: vec![refused],
                ..FakeActuator::default()
            };

            assert_eq!(
                drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::PlayKey, &mut actuator),
                ForegroundActivation::Handled
            );
            assert_eq!(lifecycle.state, saved, "refusal {refused:?}");
            assert_eq!(actuator.load_calls, 0, "a rejected rebuild cannot Load");
        }
    }

    #[test]
    fn did_foreground_preserves_a_paused_snapshot_without_issuing_play() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut lifecycle = ForegroundLifecycle::IDLE;
        suspend(&mut lifecycle, 73_000_000_000, ForegroundClock::Paused);
        let mut actuator = FakeActuator {
            prepare: vec![crate::player::ResumeOutcome::Prepared],
            loads: vec![ForegroundLoadStart::Launched(11)],
            statuses: vec![ForegroundLoadStatus::Started],
            ..FakeActuator::default()
        };

        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps,
                ForegroundInput::DidForeground,
                &mut actuator
            ),
            ForegroundActivation::Launched
        );
        assert_eq!(
            lifecycle.state,
            ForegroundState::LoadPending {
                id: 1,
                attempt: 11,
                resume_ns: 73_000_000_000,
                clock: ForegroundClock::Paused,
            }
        );
        assert_eq!(
            claim(&mut lifecycle, ForegroundInput::PlayKey),
            ForegroundClaimResult::Suppressed,
            "Play cannot claim a second Load while the first is pending"
        );
        assert_eq!(
            claim(&mut lifecycle, ForegroundInput::DidForeground),
            ForegroundClaimResult::Suppressed,
            "duplicate DID cannot claim a second Load while the first is pending"
        );
        assert_eq!(
            actuator.before_loads,
            vec![(73_000_000_000, ForegroundClock::Paused)]
        );
        assert_eq!(actuator.clock_calls, 0);
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut ps, &mut actuator),
            ForegroundPollOutcome::Started
        );
        assert_eq!(actuator.status_calls, vec![11]);
        assert_eq!(
            actuator.after_loads,
            vec![(Some(11), ForegroundClock::Paused, true)]
        );
        assert_eq!(lifecycle.state, ForegroundState::Idle);
    }

    /// **PLAY chooses the clock, not the offset.** The press that rebuilds a parked session may be
    /// the remote's PLAY key; the claim then restores a paused snapshot Playing at the exact saved
    /// offset (no `RESUME_REWIND_NS`) and feeds the Load that clock, which is what keeps the
    /// one-frame paused-seek preroll closed.
    #[test]
    fn play_key_resumes_a_paused_snapshot_playing_at_the_saved_offset() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut lifecycle = ForegroundLifecycle::IDLE;
        suspend(&mut lifecycle, 73_000_000_000, ForegroundClock::Paused);
        let mut actuator = FakeActuator {
            prepare: vec![crate::player::ResumeOutcome::Prepared],
            loads: vec![ForegroundLoadStart::Launched(11)],
            statuses: vec![ForegroundLoadStatus::Started],
            clocks: vec![true],
            ..FakeActuator::default()
        };

        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::PlayKey, &mut actuator),
            ForegroundActivation::Launched
        );
        assert_eq!(
            actuator.prepare_calls,
            vec![73_000_000_000],
            "PLAY must not rewind a paused save"
        );
        assert_eq!(
            actuator.before_loads,
            vec![(73_000_000_000, ForegroundClock::Playing)],
            "the Load carries the PLAY intent, so no paused-seek preroll is armed"
        );
        assert_eq!(
            lifecycle.state,
            ForegroundState::LoadPending {
                id: 1,
                attempt: 11,
                resume_ns: 73_000_000_000,
                clock: ForegroundClock::Playing,
            }
        );
        assert_eq!(actuator.clock_calls, 0);
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut ps, &mut actuator),
            ForegroundPollOutcome::Started
        );
        assert_eq!(actuator.clock_calls, 1, "a started Playing Load issues PlayClock once");
        assert_eq!(
            actuator.after_loads,
            vec![(Some(11), ForegroundClock::Playing, true)]
        );
        assert_eq!(lifecycle.state, ForegroundState::Idle);
    }

    /// **A resume whose rebuild is a flight parks the machine, and every landing settles it exactly
    /// as the synchronous answer would have.** `Waiting` changes nothing and consumes every claim;
    /// `Landed(Prepared)` carries on into the Load; a refused rebuild re-arms the suspended
    /// snapshot; a flight that ended without landing releases the machine instead of hanging it.
    #[test]
    fn a_pending_resume_parks_the_machine_until_its_landing_and_settles_like_the_synchronous_answer() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut lifecycle = ForegroundLifecycle::IDLE;
        suspend(&mut lifecycle, 63_000_000_000, ForegroundClock::Playing);
        let suspended = lifecycle.state;
        let mut actuator = FakeActuator {
            pending_prepares: 1,
            land_prepares: vec![
                FlightLanding::Waiting,
                FlightLanding::Landed(crate::player::ResumeOutcome::Prepared),
            ],
            loads: vec![ForegroundLoadStart::Launched(3)],
            ..FakeActuator::default()
        };
        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::DidForeground, &mut actuator),
            ForegroundActivation::Handled
        );
        assert!(lifecycle.flight_pending() && lifecycle.awaiting_load());
        for input in [ForegroundInput::DidForeground, ForegroundInput::PlayKey] {
            assert_eq!(
                drive_foreground(&mut lifecycle, &mut ps, input, &mut actuator),
                ForegroundActivation::Handled,
                "a claim during the flight is consumed"
            );
        }
        assert_eq!(actuator.prepare_calls.len(), 1, "no second preparation");
        assert_eq!(actuator.load_calls, 0);
        assert_eq!(poll_foreground_flight(&mut lifecycle, &mut ps, &mut actuator), ForegroundActivation::Handled);
        assert!(lifecycle.flight_pending(), "Waiting changes nothing");
        assert_eq!(actuator.load_calls, 0);
        assert_eq!(poll_foreground_flight(&mut lifecycle, &mut ps, &mut actuator), ForegroundActivation::Launched);
        assert!(matches!(lifecycle.state, ForegroundState::LoadPending { attempt: 3, resume_ns: 58_000_000_000, .. }));
        assert_eq!(actuator.before_loads, vec![(58_000_000_000, ForegroundClock::Playing)]);

        // A refused rebuild: the exact suspended snapshot, nothing launched.
        let mut lifecycle = ForegroundLifecycle::IDLE;
        suspend(&mut lifecycle, 63_000_000_000, ForegroundClock::Playing);
        let mut refused = FakeActuator {
            pending_prepares: 1,
            land_prepares: vec![FlightLanding::Landed(crate::player::ResumeOutcome::RebuildRejected)],
            ..FakeActuator::default()
        };
        let _ = drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::DidForeground, &mut refused);
        let _ = poll_foreground_flight(&mut lifecycle, &mut ps, &mut refused);
        assert_eq!(lifecycle.state, suspended);
        assert_eq!(refused.load_calls, 0);

        // A flight that ended without landing (Back, a newer request) releases the machine.
        let mut lifecycle = ForegroundLifecycle::IDLE;
        suspend(&mut lifecycle, 63_000_000_000, ForegroundClock::Playing);
        let mut ended = FakeActuator {
            pending_prepares: 1,
            land_prepares: vec![FlightLanding::Stale],
            ..FakeActuator::default()
        };
        let _ = drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::DidForeground, &mut ended);
        let _ = poll_foreground_flight(&mut lifecycle, &mut ps, &mut ended);
        assert_eq!(lifecycle.state, ForegroundState::Idle);
        assert!(!lifecycle.awaiting_load());
    }

    /// An Original rollback that is a flight: the `Load` claim parks as `RecoveryPending`, and the
    /// landing settles it with the reload's exact attempt, a refusal (`Terminal`) or a reload that
    /// did not start (`Failed`: the prepared route stays retryable).
    #[test]
    fn a_pending_original_rollback_settles_the_load_with_the_reload_it_landed() {
        for (landing, expected) in [
            (
                FlightLanding::Landed(ForegroundLoadStart::Launched(9)),
                ForegroundActivation::Launched,
            ),
            (FlightLanding::Landed(ForegroundLoadStart::Terminal), ForegroundActivation::Handled),
            (FlightLanding::Landed(ForegroundLoadStart::Failed), ForegroundActivation::Handled),
            (FlightLanding::Stale, ForegroundActivation::Handled),
        ] {
            let mut ps = crate::route::PlaybackSession::IDLE;
            let mut lifecycle = ForegroundLifecycle::IDLE;
            suspend(&mut lifecycle, 63_000_000_000, ForegroundClock::Playing);
            let mut actuator = FakeActuator {
                prepare: vec![crate::player::ResumeOutcome::Prepared],
                loads: vec![ForegroundLoadStart::Pending],
                land_recoveries: vec![FlightLanding::Waiting, landing],
                ..FakeActuator::default()
            };
            assert_eq!(
                drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::DidForeground, &mut actuator),
                ForegroundActivation::Handled
            );
            assert!(matches!(lifecycle.state, ForegroundState::RecoveryPending { id: 1, .. }), "{:?}", lifecycle.state);
            assert!(lifecycle.awaiting_load(), "the parked rollback holds playback");
            assert_eq!(poll_foreground_flight(&mut lifecycle, &mut ps, &mut actuator), ForegroundActivation::Handled);
            assert!(lifecycle.flight_pending(), "Waiting changes nothing");
            assert_eq!(poll_foreground_flight(&mut lifecycle, &mut ps, &mut actuator), expected);
            assert!(!lifecycle.flight_pending(), "a landing always settles the machine");
            match landing {
                FlightLanding::Landed(ForegroundLoadStart::Launched(_)) => {
                    assert!(matches!(lifecycle.state, ForegroundState::LoadPending { attempt: 9, .. }));
                }
                FlightLanding::Landed(ForegroundLoadStart::Failed) => {
                    assert!(matches!(lifecycle.state, ForegroundState::Prepared { .. }), "{:?}", lifecycle.state);
                }
                FlightLanding::Landed(_) | FlightLanding::Stale => {
                    assert_eq!(lifecycle.state, ForegroundState::Idle);
                }
                FlightLanding::Waiting => unreachable!(),
            }
        }
    }

    /// A second OS background while a flight is pending parks the session back as it was suspended
    /// (a pending rollback has only its resume position), and the flight no longer awaits a landing.
    #[test]
    fn parking_a_pending_flight_restores_the_suspended_snapshot() {
        let mut lifecycle = ForegroundLifecycle::<u64>::IDLE;
        suspend(&mut lifecycle, 63_000_000_000, ForegroundClock::Playing);
        let suspended = lifecycle.state;
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut actuator = FakeActuator { pending_prepares: 1, ..FakeActuator::default() };
        let _ = drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::DidForeground, &mut actuator);
        assert!(lifecycle.flight_pending());
        assert!(park_flight(&mut lifecycle));
        assert_eq!(lifecycle.state, suspended);
        assert!(!park_flight(&mut lifecycle), "nothing pending any more");

        let mut actuator = FakeActuator {
            prepare: vec![crate::player::ResumeOutcome::Prepared],
            loads: vec![ForegroundLoadStart::Pending],
            ..FakeActuator::default()
        };
        let _ = drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::DidForeground, &mut actuator);
        assert!(matches!(lifecycle.state, ForegroundState::RecoveryPending { .. }));
        assert!(park_flight(&mut lifecycle));
        assert!(matches!(
            lifecycle.state,
            ForegroundState::Suspended { saved_ns: 58_000_000_000, clock: ForegroundClock::Playing, .. }
        ));
    }

    #[test]
    fn pending_load_failure_retains_prepared_route_and_retry_skips_resume_preparation() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut lifecycle = ForegroundLifecycle::IDLE;
        suspend(&mut lifecycle, 63_000_000_000, ForegroundClock::Playing);
        let mut actuator = FakeActuator {
            prepare: vec![crate::player::ResumeOutcome::Prepared],
            loads: vec![
                ForegroundLoadStart::Launched(17),
                ForegroundLoadStart::Launched(18),
            ],
            statuses: vec![
                ForegroundLoadStatus::Pending,
                ForegroundLoadStatus::Failed,
                ForegroundLoadStatus::Started,
            ],
            clocks: vec![true],
            ..FakeActuator::default()
        };

        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps,
                ForegroundInput::DidForeground,
                &mut actuator
            ),
            ForegroundActivation::Launched
        );
        assert_eq!(
            lifecycle.state,
            ForegroundState::LoadPending {
                id: 1,
                attempt: 17,
                resume_ns: 58_000_000_000,
                clock: ForegroundClock::Playing,
            }
        );
        assert_eq!(actuator.clock_calls, 0, "thread launch is not native Start");
        assert_eq!(
            lifecycle.clock_for_suspend(true),
            ForegroundClock::Playing,
            "a second background edge lost the Play-key intent to the held native clock"
        );
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut ps, &mut actuator),
            ForegroundPollOutcome::Pending
        );
        assert_eq!(actuator.clock_calls, 0, "pending Load issued Play");
        let mut fx = Vec::new();
        assert_eq!(
            lifecycle.step(
                &ForegroundEvent::LoadStatus { attempt: 99, status: ForegroundLoadStatus::Failed },
                &mut fx
            ),
            Handled::No,
            "another attempt cannot settle the foreground owner"
        );
        assert!(fx.is_empty());
        assert!(matches!(
            lifecycle.state,
            ForegroundState::LoadPending { attempt: 17, .. }
        ));
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut ps, &mut actuator),
            ForegroundPollOutcome::Failed
        );
        assert_eq!(
            lifecycle.state,
            ForegroundState::Prepared {
                id: 1,
                resume_ns: 58_000_000_000,
                clock: ForegroundClock::Playing,
            }
        );
        assert_eq!(actuator.prepare_calls, vec![58_000_000_000]);
        assert_eq!(actuator.load_calls, 1);
        assert_eq!(actuator.clock_calls, 0, "failed Load issued Play");
        assert_eq!(
            actuator.after_loads,
            vec![(Some(17), ForegroundClock::Playing, false)]
        );
        assert_eq!(actuator.teardowns, 1, "failed Engine was not retired once");

        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::PlayKey, &mut actuator),
            ForegroundActivation::Launched
        );
        assert_eq!(actuator.prepare_calls.len(), 1, "prepared URL was rebuilt");
        assert_eq!(actuator.load_calls, 2);
        assert_eq!(
            actuator.clock_calls, 0,
            "retry launch was mistaken for Start"
        );
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut ps, &mut actuator),
            ForegroundPollOutcome::Started
        );
        assert_eq!(actuator.status_calls, vec![17, 17, 18]);
        assert_eq!(
            actuator.teardowns, 1,
            "retry repeated failed-Engine teardown"
        );
        assert_eq!(actuator.clock_calls, 1);
        assert_eq!(lifecycle.state, ForegroundState::Idle);
        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps,
                ForegroundInput::DidForeground,
                &mut actuator
            ),
            ForegroundActivation::Ordinary,
            "DID after the successful Load must have no foreground effect"
        );
        assert_eq!(actuator.load_calls, 2, "DID issued a second Load");
    }

    #[test]
    fn async_failure_tears_down_once_so_retry_does_not_loop_on_already_running() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        #[derive(Default)]
        struct EngineSpy {
            engine_live: bool,
            prepares: usize,
            loads: usize,
            teardowns: usize,
        }

        impl ForegroundActuator for EngineSpy {
            type Attempt = u64;

            fn prepare_resume(
                &mut self,
                _ps: &mut crate::route::PlaybackSession,
                _resume_ns: i64,
            ) -> crate::player::ResumeStart {
                self.prepares += 1;
                crate::player::ResumeStart::Settled(crate::player::ResumeOutcome::Prepared)
            }

            fn land_prepare(
                &mut self,
                _ps: &mut crate::route::PlaybackSession,
            ) -> FlightLanding<crate::player::ResumeOutcome> {
                unreachable!("this actuator never goes pending")
            }

            fn land_recovery(
                &mut self,
                _ps: &mut crate::route::PlaybackSession,
            ) -> FlightLanding<ForegroundLoadStart<Self::Attempt>> {
                unreachable!("this actuator never goes pending")
            }

            fn before_load(&mut self, _resume_ns: i64, _clock: ForegroundClock) {}

            fn start_load(
            &mut self,
            _ps: &mut crate::route::PlaybackSession,
        ) -> ForegroundLoadStart<Self::Attempt> {
                if self.engine_live {
                    return ForegroundLoadStart::AlreadyRunning;
                }
                self.loads += 1;
                self.engine_live = true;
                ForegroundLoadStart::Launched(self.loads as u64)
            }

            fn load_status(
                &mut self,
                attempt: Self::Attempt,
            ) -> ForegroundLoadStatus<Self::Attempt> {
                if attempt == 1 {
                    ForegroundLoadStatus::Failed
                } else {
                    ForegroundLoadStatus::Started
                }
            }

            fn after_load(
                &mut self,
                _ps: &mut crate::route::PlaybackSession,
                attempt: Option<Self::Attempt>,
                _clock: ForegroundClock,
                started: bool,
            ) {
                if !started && attempt.is_some() {
                    self.teardowns += 1;
                    self.engine_live = false;
                }
            }

            fn play_clock(&mut self) -> bool {
                true
            }
        }

        let mut lifecycle = ForegroundLifecycle::IDLE;
        suspend(&mut lifecycle, 29_000_000_000, ForegroundClock::Playing);
        let mut actuator = EngineSpy::default();

        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps,
                ForegroundInput::DidForeground,
                &mut actuator
            ),
            ForegroundActivation::Launched
        );
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut ps, &mut actuator),
            ForegroundPollOutcome::Failed
        );
        assert_eq!(actuator.teardowns, 1);

        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::PlayKey, &mut actuator),
            ForegroundActivation::Launched,
            "failed Engine survived and turned the retry into AlreadyRunning"
        );
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut ps, &mut actuator),
            ForegroundPollOutcome::Started
        );
        assert_eq!(
            (actuator.prepares, actuator.loads, actuator.teardowns),
            (1, 2, 1)
        );
        assert_eq!(lifecycle.state, ForegroundState::Idle);
    }

    #[test]
    fn synchronous_load_refusal_retains_the_prepared_route() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut lifecycle = ForegroundLifecycle::IDLE;
        suspend(&mut lifecycle, 41_000_000_000, ForegroundClock::Playing);
        let mut actuator = FakeActuator {
            prepare: vec![crate::player::ResumeOutcome::Prepared],
            loads: vec![ForegroundLoadStart::Failed],
            ..FakeActuator::default()
        };

        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps,
                ForegroundInput::DidForeground,
                &mut actuator
            ),
            ForegroundActivation::Handled
        );
        assert_eq!(
            lifecycle.state,
            ForegroundState::Prepared {
                id: 1,
                resume_ns: 36_000_000_000,
                clock: ForegroundClock::Playing,
            }
        );
        assert_eq!(actuator.prepare_calls, vec![36_000_000_000]);
        assert_eq!(
            actuator.after_loads,
            vec![(None, ForegroundClock::Playing, false)]
        );
        assert_eq!(actuator.teardowns, 0);
        assert_eq!(actuator.clock_calls, 0);
    }

    #[test]
    fn unowned_or_terminal_load_releases_foreground_instead_of_retrying_forever() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        for refused in [
            ForegroundLoadStart::AlreadyRunning,
            ForegroundLoadStart::Terminal,
        ] {
            let mut lifecycle = ForegroundLifecycle::IDLE;
            suspend(&mut lifecycle, 41_000_000_000, ForegroundClock::Playing);
            let mut actuator = FakeActuator {
                prepare: vec![crate::player::ResumeOutcome::Prepared],
                loads: vec![refused],
                ..FakeActuator::default()
            };

            assert_eq!(
                drive_foreground(&mut lifecycle, &mut ps,
                    ForegroundInput::DidForeground,
                    &mut actuator,
                ),
                ForegroundActivation::Handled,
            );
            assert_eq!(lifecycle.state, ForegroundState::Idle);
            assert_eq!(actuator.teardowns, 0);
            assert_eq!(actuator.clock_calls, 0);
        }
    }

    #[test]
    fn stale_load_attempt_releases_foreground_without_touching_an_unknown_engine() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut lifecycle = ForegroundLifecycle::IDLE;
        suspend(&mut lifecycle, 22_000_000_000, ForegroundClock::Paused);
        let mut actuator = FakeActuator {
            prepare: vec![crate::player::ResumeOutcome::Prepared],
            loads: vec![ForegroundLoadStart::Launched(7)],
            statuses: vec![ForegroundLoadStatus::Stale],
            ..FakeActuator::default()
        };

        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps,
                ForegroundInput::DidForeground,
                &mut actuator
            ),
            ForegroundActivation::Launched
        );
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut ps, &mut actuator),
            ForegroundPollOutcome::Failed
        );
        assert_eq!(lifecycle.state, ForegroundState::Idle);
        assert_eq!(actuator.prepare_calls.len(), 1);
        assert_eq!(
            actuator.after_loads,
            vec![(None, ForegroundClock::Paused, false)]
        );
        assert_eq!(actuator.teardowns, 0);
        assert_eq!(actuator.clock_calls, 0);
    }

    #[test]
    fn foreground_follows_every_tokened_replacement_without_failure_cleanup() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut lifecycle = ForegroundLifecycle::IDLE;
        suspend(&mut lifecycle, 31_000_000_000, ForegroundClock::Playing);
        let mut actuator = FakeActuator {
            prepare: vec![crate::player::ResumeOutcome::Prepared],
            loads: vec![ForegroundLoadStart::Launched(7)],
            statuses: vec![
                ForegroundLoadStatus::Superseded(8),
                ForegroundLoadStatus::Superseded(9),
                ForegroundLoadStatus::Started,
            ],
            clocks: vec![true],
            ..FakeActuator::default()
        };

        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps,
                ForegroundInput::DidForeground,
                &mut actuator,
            ),
            ForegroundActivation::Launched,
        );
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut ps, &mut actuator),
            ForegroundPollOutcome::Pending,
        );
        assert!(matches!(
            lifecycle.state,
            ForegroundState::LoadPending { attempt: 8, .. }
        ));
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut ps, &mut actuator),
            ForegroundPollOutcome::Pending,
        );
        assert!(matches!(
            lifecycle.state,
            ForegroundState::LoadPending { attempt: 9, .. }
        ));
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut ps, &mut actuator),
            ForegroundPollOutcome::Started,
        );
        assert_eq!(actuator.status_calls, vec![7, 8, 9]);
        assert!(actuator.after_loads.iter().all(|(_, _, started)| *started));
        assert_eq!(actuator.teardowns, 0);
        assert_eq!(actuator.clock_calls, 1);
        assert_eq!(lifecycle.state, ForegroundState::Idle);
    }

    #[test]
    fn refused_play_after_load_retries_only_the_clock() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut lifecycle = ForegroundLifecycle::IDLE;
        suspend(&mut lifecycle, 73_000_000_000, ForegroundClock::Paused);
        let mut actuator = FakeActuator {
            prepare: vec![crate::player::ResumeOutcome::Prepared],
            loads: vec![ForegroundLoadStart::Launched(23)],
            statuses: vec![ForegroundLoadStatus::Started],
            clocks: vec![false, true],
            ..FakeActuator::default()
        };

        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::PlayKey, &mut actuator),
            ForegroundActivation::Launched,
            "spawning the Load only mounts the player while its result remains pending"
        );
        assert_eq!(actuator.clock_calls, 0);
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut ps, &mut actuator),
            ForegroundPollOutcome::Started,
            "the exact Load succeeded even though its following Play did not"
        );
        assert_eq!(lifecycle.state, ForegroundState::ClockPending { id: 1 });
        assert_eq!(
            claim(&mut lifecycle, ForegroundInput::DidForeground),
            ForegroundClaimResult::Suppressed,
            "DID after the Load must not claim another Load"
        );
        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::PlayKey, &mut actuator),
            ForegroundActivation::Handled
        );
        assert_eq!(actuator.prepare_calls.len(), 1);
        assert_eq!(actuator.load_calls, 1, "clock retry issued a second Load");
        assert_eq!(actuator.clock_calls, 2);
        assert_eq!(lifecycle.state, ForegroundState::Idle);
    }

    /// §3.2's table, every state under both successors: the exact state, the exact claim result,
    /// and the actuator calls (`after_load(None, clock, false)` once, for `LoadPending` x `Exit`).
    #[test]
    fn superseded_moves_every_state_as_the_table_says() {
        use ForegroundClaimResult::{Effect, Ordinary, Suppressed};
        let paused = ForegroundClock::Paused;
        let playing = ForegroundClock::Playing;
        let prepare = ForegroundClaim::Prepare {
            saved_ns: 5,
            saved_clock: paused,
            resume_ns: 5,
            clock: paused,
        };
        let load = ForegroundClaim::Load {
            resume_ns: 5,
            clock: paused,
        };
        let idle = ForegroundState::Idle;
        // (state, Playback: (after, result), Exit: (after, result), retired clock for Exit)
        let cases: Vec<(
            &str,
            ForegroundState<u64>,
            (ForegroundState<u64>, ForegroundClaimResult),
            (ForegroundState<u64>, ForegroundClaimResult),
        )> = vec![
            ("Idle", idle, (idle, Ordinary), (idle, Ordinary)),
            (
                "Suspended",
                ForegroundState::Suspended { id: 1, saved_ns: 5, clock: paused },
                (idle, Suppressed),
                (idle, Suppressed),
            ),
            (
                "Claimed Prepare",
                ForegroundState::Claimed { id: 1, claim: prepare },
                (idle, Suppressed),
                (idle, Suppressed),
            ),
            (
                "Claimed Load",
                ForegroundState::Claimed { id: 1, claim: load },
                (idle, Suppressed),
                (idle, Suppressed),
            ),
            (
                "Claimed PlayClock",
                ForegroundState::Claimed { id: 1, claim: ForegroundClaim::PlayClock },
                (ForegroundState::Claimed { id: 1, claim: ForegroundClaim::PlayClock }, Suppressed),
                (idle, Suppressed),
            ),
            (
                "Prepared",
                ForegroundState::Prepared { id: 1, resume_ns: 5, clock: paused },
                (idle, Suppressed),
                (idle, Suppressed),
            ),
            (
                "LoadPending",
                ForegroundState::LoadPending { id: 1, attempt: 7, resume_ns: 5, clock: playing },
                (
                    ForegroundState::LoadPending { id: 1, attempt: 7, resume_ns: 5, clock: playing },
                    Suppressed,
                ),
                (idle, Effect(ForegroundEffect::Retire { id: 1, clock: playing })),
            ),
            (
                "PreparePending",
                ForegroundState::PreparePending {
                    id: 1,
                    saved_ns: 5,
                    saved_clock: paused,
                    resume_ns: 5,
                    clock: paused,
                },
                (idle, Suppressed),
                (idle, Suppressed),
            ),
            (
                "RecoveryPending",
                ForegroundState::RecoveryPending { id: 1, resume_ns: 5, clock: paused },
                (idle, Suppressed),
                (idle, Suppressed),
            ),
            (
                "ClockPending",
                ForegroundState::ClockPending { id: 1 },
                (ForegroundState::ClockPending { id: 1 }, Suppressed),
                (idle, Suppressed),
            ),
        ];
        for (name, start, playback, exit) in cases {
            for (successor, (after, result)) in [(Successor::Playback, playback), (Successor::Exit, exit)] {
                let input = ForegroundInput::Superseded(successor);
                let mut lifecycle = ForegroundLifecycle::<u64> { state: start, next_id: 1 };
                assert_eq!(claim(&mut lifecycle, input), result, "{name} x {successor:?}: result");
                assert_eq!(lifecycle.state, after, "{name} x {successor:?}: state");

                let mut ps = crate::route::PlaybackSession::IDLE;
                let mut lifecycle = ForegroundLifecycle::<u64> { state: start, next_id: 1 };
                let mut actuator = FakeActuator::default();
                let activation = drive_foreground(&mut lifecycle, &mut ps, input, &mut actuator);
                assert_eq!(lifecycle.state, after, "{name} x {successor:?}: driven state");
                let expected_activation = if result == Ordinary {
                    ForegroundActivation::Ordinary
                } else {
                    ForegroundActivation::Handled
                };
                assert_eq!(activation, expected_activation, "{name} x {successor:?}: activation");
                let retired = match result {
                    Effect(ForegroundEffect::Retire { clock, .. }) => vec![(None, clock, false)],
                    _ => Vec::new(),
                };
                assert_eq!(actuator.after_loads, retired, "{name} x {successor:?}: after_load");
                assert_eq!(
                    (actuator.load_calls, actuator.clock_calls, actuator.prepare_calls.len()),
                    (0, 0, 0),
                    "{name} x {successor:?}: nothing else runs"
                );
            }
        }
    }

    /// The DID foreground that carries a launch link which ends playback: it claims nothing. A
    /// parked state stays parked (`Suppressed`, so no Prepare or Load runs and `playback_may_run`
    /// stays off until the link's `Superseded(Exit)` retires it); Idle is the ordinary start path;
    /// every other state is already suppressed, exactly as `DidForeground` leaves it.
    #[test]
    fn did_foreground_superseded_claims_nothing() {
        use ForegroundClaimResult::{Ordinary, Suppressed};
        let paused = ForegroundClock::Paused;
        let states: Vec<(&str, ForegroundState<u64>)> = vec![
            ("Idle", ForegroundState::Idle),
            ("Suspended", ForegroundState::Suspended { id: 1, saved_ns: 5, clock: paused }),
            ("Claimed Prepare", ForegroundState::Claimed {
                id: 1,
                claim: ForegroundClaim::Prepare { saved_ns: 5, saved_clock: paused, resume_ns: 5, clock: paused },
            }),
            ("Claimed Load", ForegroundState::Claimed {
                id: 1,
                claim: ForegroundClaim::Load { resume_ns: 5, clock: paused },
            }),
            ("Claimed PlayClock", ForegroundState::Claimed { id: 1, claim: ForegroundClaim::PlayClock }),
            ("Prepared", ForegroundState::Prepared { id: 1, resume_ns: 5, clock: paused }),
            ("LoadPending", ForegroundState::LoadPending { id: 1, attempt: 7, resume_ns: 5, clock: paused }),
            ("PreparePending", ForegroundState::PreparePending {
                id: 1, saved_ns: 5, saved_clock: paused, resume_ns: 5, clock: paused,
            }),
            ("RecoveryPending", ForegroundState::RecoveryPending { id: 1, resume_ns: 5, clock: paused }),
            ("ClockPending", ForegroundState::ClockPending { id: 1 }),
        ];
        for (name, start) in states {
            let want = if matches!(start, ForegroundState::Idle) { Ordinary } else { Suppressed };
            let mut lifecycle = ForegroundLifecycle::<u64> { state: start, next_id: 1 };
            assert_eq!(claim(&mut lifecycle, ForegroundInput::DidForegroundSuperseded), want, "{name}: result");
            assert_eq!(lifecycle.state, start, "{name}: no state moves");

            let mut ps = crate::route::PlaybackSession::IDLE;
            let mut lifecycle = ForegroundLifecycle::<u64> { state: start, next_id: 1 };
            let mut actuator = FakeActuator::default();
            let activation = drive_foreground(
                &mut lifecycle, &mut ps, ForegroundInput::DidForegroundSuperseded, &mut actuator);
            let expected = if want == Ordinary { ForegroundActivation::Ordinary } else { ForegroundActivation::Handled };
            assert_eq!(activation, expected, "{name}: activation");
            assert_eq!(lifecycle.state, start, "{name}: driven state");
            assert!(actuator.after_loads.is_empty(), "{name}: after_load");
            assert_eq!(
                (actuator.load_calls, actuator.clock_calls, actuator.prepare_calls.len()),
                (0, 0, 0),
                "{name}: nothing runs"
            );
        }
        // The one difference from `DidForeground`: it would claim a parked state.
        let mut parked = ForegroundLifecycle::<u64> {
            state: ForegroundState::Suspended { id: 1, saved_ns: 5, clock: paused }, next_id: 1 };
        assert!(matches!(claim(&mut parked, ForegroundInput::DidForeground), ForegroundClaimResult::Effect(_)));
    }

    /// **PLAY pressed with no player on screen is ONE event.** It used to be two calls (a discard
    /// of the started session's pending clock, then the PLAY claim), and the claim alone answered
    /// `Effect(PlayClock)` for the pending clock of a player a real exit had removed. Off the
    /// player a pending clock goes to `Idle` and the press answers `Ordinary`; every other state
    /// answers it exactly as it answers `PlayKey`.
    #[test]
    fn play_off_player_is_one_event() {
        use ForegroundClaimResult::{Effect, Ordinary, Suppressed};
        let mut ps = crate::route::PlaybackSession::IDLE;
        let pending_clocks: [(&str, ForegroundState<u64>); 2] = [
            ("ClockPending", ForegroundState::ClockPending { id: 1 }),
            (
                "Claimed PlayClock",
                ForegroundState::Claimed { id: 1, claim: ForegroundClaim::PlayClock },
            ),
        ];
        for (name, state) in pending_clocks {
            // On the player the press is the retry of the clock (or, mid-claim, consumed) ...
            let mut on = ForegroundLifecycle::<u64> { state, next_id: 1 };
            let want = if matches!(state, ForegroundState::ClockPending { .. }) {
                Effect(ForegroundEffect::PlayClock { id: 1 })
            } else {
                Suppressed
            };
            assert_eq!(claim(&mut on, ForegroundInput::PlayKey), want, "{name}: PlayKey");
            // ... off it, ONE event leaves the machine Idle and the press on the ordinary path.
            let mut off = ForegroundLifecycle::<u64> { state, next_id: 1 };
            assert_eq!(claim(&mut off, ForegroundInput::PlayKeyOffPlayer), Ordinary, "{name}");
            assert_eq!(off.state, ForegroundState::Idle, "{name}: the pending clock is dropped");
            // Driven, the actuator is not asked for the clock the exit removed.
            let mut driven = ForegroundLifecycle::<u64> { state, next_id: 1 };
            let mut actuator = FakeActuator::default();
            let activation =
                drive_foreground(&mut driven, &mut ps, ForegroundInput::PlayKeyOffPlayer, &mut actuator);
            assert_eq!(activation, ForegroundActivation::Ordinary, "{name}: activation");
            assert_eq!(driven.state, ForegroundState::Idle, "{name}: driven state");
            assert_eq!(actuator.clock_calls, 0, "{name}: no clock retry");
        }
        let paused = ForegroundClock::Paused;
        let others: [(&str, ForegroundState<u64>); 8] = [
            ("Idle", ForegroundState::Idle),
            ("Suspended", ForegroundState::Suspended { id: 1, saved_ns: 5, clock: paused }),
            (
                "Claimed Prepare",
                ForegroundState::Claimed {
                    id: 1,
                    claim: ForegroundClaim::Prepare {
                        saved_ns: 5,
                        saved_clock: paused,
                        resume_ns: 5,
                        clock: paused,
                    },
                },
            ),
            (
                "Claimed Load",
                ForegroundState::Claimed {
                    id: 1,
                    claim: ForegroundClaim::Load { resume_ns: 5, clock: paused },
                },
            ),
            ("Prepared", ForegroundState::Prepared { id: 1, resume_ns: 5, clock: paused }),
            (
                "LoadPending",
                ForegroundState::LoadPending { id: 1, attempt: 7, resume_ns: 5, clock: paused },
            ),
            (
                "PreparePending",
                ForegroundState::PreparePending {
                    id: 1,
                    saved_ns: 5,
                    saved_clock: paused,
                    resume_ns: 5,
                    clock: paused,
                },
            ),
            (
                "RecoveryPending",
                ForegroundState::RecoveryPending { id: 1, resume_ns: 5, clock: paused },
            ),
        ];
        for (name, state) in others {
            let mut on = ForegroundLifecycle::<u64> { state, next_id: 1 };
            let mut off = ForegroundLifecycle::<u64> { state, next_id: 1 };
            assert_eq!(
                claim(&mut off, ForegroundInput::PlayKeyOffPlayer),
                claim(&mut on, ForegroundInput::PlayKey),
                "{name}: the same answer as PlayKey"
            );
            assert_eq!(off.state, on.state, "{name}: the same state as PlayKey");
        }
    }

    /// **The machine's whole transition relation: every state x every event.** The next state, the
    /// effects in order and `Handled`, pinned. Each state lists only the cells which DO something;
    /// every other cell must leave the state alone, emit nothing and answer `No` (an `Input` a
    /// state consumes without acting on is `Yes`, so those are listed). `Park` is the same shape in
    /// every state, so a state names only what it saves for the viewer's pause and its own.
    #[test]
    fn every_state_meets_every_event_as_the_table_says() {
        type St = ForegroundState<u64>;
        type Fx = ForegroundEffect<u64>;
        type Ev = ForegroundEvent<u64>;
        type Cell = (Ev, St, Vec<Fx>, Handled);
        struct Row {
            name: &'static str,
            state: St,
            /// The clock a `Park` saves when the viewer's transport reads (paused, playing).
            park: (ForegroundClock, ForegroundClock),
            cells: Vec<Cell>,
        }
        fn yes(ev: Ev, next: St, fx: Vec<Fx>) -> Cell {
            (ev, next, fx, Handled::Yes)
        }
        fn no(ev: Ev, next: St, fx: Vec<Fx>) -> Cell {
            (ev, next, fx, Handled::No)
        }

        const NS: i64 = 50_000_000_000;
        const REWOUND: i64 = NS - RESUME_REWIND_NS;
        const R: i64 = 7_000_000_000;
        let paused = ForegroundClock::Paused;
        let playing = ForegroundClock::Playing;
        let idle = ForegroundState::Idle;

        let did = Ev::Input(ForegroundInput::DidForeground);
        let did_superseded = Ev::Input(ForegroundInput::DidForegroundSuperseded);
        let play = Ev::Input(ForegroundInput::PlayKey);
        let off_player = Ev::Input(ForegroundInput::PlayKeyOffPlayer);
        let superseded_by_playback = Ev::Input(ForegroundInput::Superseded(Successor::Playback));
        let superseded_by_exit = Ev::Input(ForegroundInput::Superseded(Successor::Exit));
        let prepared = |id| Ev::PrepareAnswered { id, answer: PrepareAnswer::Prepared };
        let refused = Ev::PrepareAnswered { id: 1, answer: PrepareAnswer::Refused };
        let prepare_pending = Ev::PrepareAnswered { id: 1, answer: PrepareAnswer::Pending };
        let load_answered = |id, start| Ev::LoadAnswered { id, start };
        let status = |attempt, status| Ev::LoadStatus { attempt, status };
        let clock_answered = |id, accepted| Ev::ClockAnswered { id, accepted };

        let events: Vec<Ev> = vec![
            Ev::Park { saved_ns: 777, viewer_paused: true },
            Ev::Park { saved_ns: 777, viewer_paused: false },
            Ev::ParkFlight,
            did,
            did_superseded,
            play,
            off_player,
            superseded_by_playback,
            superseded_by_exit,
            prepared(1),
            refused,
            prepare_pending,
            prepared(2),
            load_answered(1, ForegroundLoadStart::Launched(8)),
            load_answered(1, ForegroundLoadStart::Pending),
            load_answered(1, ForegroundLoadStart::Failed),
            load_answered(1, ForegroundLoadStart::AlreadyRunning),
            load_answered(1, ForegroundLoadStart::Terminal),
            load_answered(2, ForegroundLoadStart::Failed),
            Ev::FlightStale,
            status(7, ForegroundLoadStatus::Pending),
            status(7, ForegroundLoadStatus::Started),
            status(7, ForegroundLoadStatus::Failed),
            status(7, ForegroundLoadStatus::Superseded(9)),
            status(7, ForegroundLoadStatus::Stale),
            status(8, ForegroundLoadStatus::Failed),
            clock_answered(1, true),
            clock_answered(1, false),
            clock_answered(2, true),
        ];

        // The four inputs a state which is mid-claim, or waits on an answer, consumes unchanged.
        let consumed = |state: St| -> Vec<Cell> {
            [did, did_superseded, play, off_player]
                .into_iter()
                .map(|ev| yes(ev, state, vec![]))
                .collect()
        };
        let retired = || -> Vec<Cell> {
            vec![
                yes(superseded_by_playback, idle, vec![]),
                yes(superseded_by_exit, idle, vec![]),
            ]
        };
        let join = |parts: Vec<Vec<Cell>>| -> Vec<Cell> { parts.into_iter().flatten().collect() };

        let suspended_paused = ForegroundState::Suspended { id: 1, saved_ns: NS, clock: paused };
        let suspended_playing = ForegroundState::Suspended { id: 1, saved_ns: NS, clock: playing };
        let claimed_prepare = ForegroundState::Claimed {
            id: 1,
            claim: ForegroundClaim::Prepare {
                saved_ns: NS,
                saved_clock: paused,
                resume_ns: NS,
                clock: playing,
            },
        };
        let preparing = |state: St| -> Vec<Cell> {
            vec![
                yes(
                    prepared(1),
                    ForegroundState::Claimed {
                        id: 1,
                        claim: ForegroundClaim::Load { resume_ns: NS, clock: playing },
                    },
                    vec![Fx::Load { id: 1, resume_ns: NS, clock: playing }],
                ),
                yes(refused, suspended_paused, vec![]),
                yes(
                    prepare_pending,
                    ForegroundState::PreparePending {
                        id: 1,
                        saved_ns: NS,
                        saved_clock: paused,
                        resume_ns: NS,
                        clock: playing,
                    },
                    vec![],
                ),
            ]
            .into_iter()
            .map(|cell| {
                // A landing for a flight which is still pending leaves it pending.
                if matches!(state, ForegroundState::PreparePending { .. }) && cell.0 == prepare_pending {
                    (cell.0, state, cell.2, cell.3)
                } else {
                    cell
                }
            })
            .collect()
        };
        let preparing_flight = ForegroundState::PreparePending {
            id: 1,
            saved_ns: NS,
            saved_clock: paused,
            resume_ns: NS,
            clock: playing,
        };
        let claimed_load = ForegroundState::Claimed {
            id: 1,
            claim: ForegroundClaim::Load { resume_ns: R, clock: playing },
        };
        let recovering = ForegroundState::RecoveryPending { id: 1, resume_ns: R, clock: playing };
        let loading = |state: St| -> Vec<Cell> {
            vec![
                yes(
                    load_answered(1, ForegroundLoadStart::Launched(8)),
                    ForegroundState::LoadPending { id: 1, attempt: 8, resume_ns: R, clock: playing },
                    vec![Fx::Launched { id: 1 }],
                ),
                yes(
                    load_answered(1, ForegroundLoadStart::Pending),
                    recovering,
                    vec![],
                ),
                yes(
                    load_answered(1, ForegroundLoadStart::Failed),
                    ForegroundState::Prepared { id: 1, resume_ns: R, clock: playing },
                    vec![Fx::AfterLoad { attempt: None, clock: playing, started: false }],
                ),
                yes(
                    load_answered(1, ForegroundLoadStart::AlreadyRunning),
                    idle,
                    vec![Fx::AfterLoad { attempt: None, clock: playing, started: false }],
                ),
                yes(
                    load_answered(1, ForegroundLoadStart::Terminal),
                    idle,
                    vec![Fx::AfterLoad { attempt: None, clock: playing, started: false }],
                ),
            ]
            .into_iter()
            .map(|cell| {
                // A landing for a rollback which is still a flight leaves it pending.
                if state == recovering && cell.0 == load_answered(1, ForegroundLoadStart::Pending) {
                    (cell.0, state, cell.2, cell.3)
                } else {
                    cell
                }
            })
            .collect()
        };
        let prepared_state = ForegroundState::Prepared { id: 1, resume_ns: R, clock: paused };
        let load_pending = |clock: ForegroundClock| -> Row {
            let state = ForegroundState::LoadPending { id: 1, attempt: 7, resume_ns: R, clock };
            let started = if matches!(clock, ForegroundClock::Paused) {
                yes(
                    status(7, ForegroundLoadStatus::Started),
                    idle,
                    vec![Fx::AfterLoad { attempt: Some(7), clock, started: true }],
                )
            } else {
                yes(
                    status(7, ForegroundLoadStatus::Started),
                    ForegroundState::Claimed { id: 1, claim: ForegroundClaim::PlayClock },
                    vec![
                        Fx::AfterLoad { attempt: Some(7), clock, started: true },
                        Fx::PlayClock { id: 1 },
                    ],
                )
            };
            Row {
                name: if matches!(clock, ForegroundClock::Paused) {
                    "LoadPending Paused"
                } else {
                    "LoadPending Playing"
                },
                state,
                park: (clock, clock),
                cells: join(vec![
                    consumed(state),
                    vec![
                        yes(superseded_by_playback, state, vec![]),
                        yes(superseded_by_exit, idle, vec![Fx::Retire { id: 1, clock }]),
                        yes(status(7, ForegroundLoadStatus::Pending), state, vec![]),
                        yes(
                            status(7, ForegroundLoadStatus::Superseded(9)),
                            ForegroundState::LoadPending { id: 1, attempt: 9, resume_ns: R, clock },
                            vec![],
                        ),
                        yes(
                            status(7, ForegroundLoadStatus::Failed),
                            ForegroundState::Prepared { id: 1, resume_ns: R, clock },
                            vec![Fx::AfterLoad { attempt: Some(7), clock, started: false }],
                        ),
                        yes(
                            status(7, ForegroundLoadStatus::Stale),
                            idle,
                            vec![Fx::AfterLoad { attempt: None, clock, started: false }],
                        ),
                        started,
                    ],
                ]),
            }
        };
        let claimed_clock = ForegroundState::Claimed { id: 1, claim: ForegroundClaim::PlayClock };
        let clock_pending = ForegroundState::ClockPending { id: 1 };

        let rows: Vec<Row> = vec![
            Row { name: "Idle", state: idle, park: (paused, playing), cells: vec![] },
            Row {
                name: "Suspended Paused",
                state: suspended_paused,
                park: (paused, playing),
                cells: join(vec![
                    vec![
                        yes(
                            did,
                            ForegroundState::Claimed {
                                id: 1,
                                claim: ForegroundClaim::Prepare {
                                    saved_ns: NS,
                                    saved_clock: paused,
                                    resume_ns: NS,
                                    clock: paused,
                                },
                            },
                            vec![Fx::Prepare { id: 1, resume_ns: NS }],
                        ),
                        yes(did_superseded, suspended_paused, vec![]),
                        yes(play, claimed_prepare, vec![Fx::Prepare { id: 1, resume_ns: NS }]),
                        yes(off_player, claimed_prepare, vec![Fx::Prepare { id: 1, resume_ns: NS }]),
                    ],
                    retired(),
                ]),
            },
            Row {
                name: "Suspended Playing",
                state: suspended_playing,
                park: (paused, playing),
                cells: {
                    // A playing save rewinds; PLAY and DID restore it the same way.
                    let claimed = ForegroundState::Claimed {
                        id: 1,
                        claim: ForegroundClaim::Prepare {
                            saved_ns: NS,
                            saved_clock: playing,
                            resume_ns: REWOUND,
                            clock: playing,
                        },
                    };
                    let prepare = vec![Fx::Prepare { id: 1, resume_ns: REWOUND }];
                    join(vec![
                        vec![
                            yes(did, claimed, prepare.clone()),
                            yes(did_superseded, suspended_playing, vec![]),
                            yes(play, claimed, prepare.clone()),
                            yes(off_player, claimed, prepare),
                        ],
                        retired(),
                    ])
                },
            },
            Row {
                name: "Claimed Prepare",
                state: claimed_prepare,
                park: (paused, playing),
                cells: join(vec![consumed(claimed_prepare), retired(), preparing(claimed_prepare)]),
            },
            Row {
                name: "PreparePending",
                state: preparing_flight,
                park: (paused, paused),
                cells: join(vec![
                    consumed(preparing_flight),
                    retired(),
                    preparing(preparing_flight),
                    vec![
                        yes(Ev::ParkFlight, suspended_paused, vec![Fx::DropFlight]),
                        yes(Ev::FlightStale, idle, vec![]),
                    ],
                ]),
            },
            Row {
                name: "Claimed Load",
                state: claimed_load,
                park: (paused, playing),
                cells: join(vec![consumed(claimed_load), retired(), loading(claimed_load)]),
            },
            Row {
                name: "RecoveryPending",
                state: recovering,
                park: (playing, playing),
                cells: join(vec![
                    consumed(recovering),
                    retired(),
                    loading(recovering),
                    vec![
                        yes(
                            Ev::ParkFlight,
                            ForegroundState::Suspended { id: 1, saved_ns: R, clock: playing },
                            vec![Fx::DropFlight],
                        ),
                        yes(Ev::FlightStale, idle, vec![]),
                    ],
                ]),
            },
            Row {
                name: "Prepared",
                state: prepared_state,
                park: (paused, playing),
                cells: {
                    let load = |clock| ForegroundState::Claimed {
                        id: 1,
                        claim: ForegroundClaim::Load { resume_ns: R, clock },
                    };
                    join(vec![
                        vec![
                            yes(
                                did,
                                load(paused),
                                vec![Fx::Load { id: 1, resume_ns: R, clock: paused }],
                            ),
                            yes(did_superseded, prepared_state, vec![]),
                            yes(
                                play,
                                load(playing),
                                vec![Fx::Load { id: 1, resume_ns: R, clock: playing }],
                            ),
                            yes(
                                off_player,
                                load(playing),
                                vec![Fx::Load { id: 1, resume_ns: R, clock: playing }],
                            ),
                        ],
                        retired(),
                    ])
                },
            },
            load_pending(playing),
            load_pending(paused),
            Row {
                name: "Claimed PlayClock",
                state: claimed_clock,
                park: (playing, playing),
                cells: vec![
                    yes(did, claimed_clock, vec![]),
                    yes(did_superseded, claimed_clock, vec![]),
                    yes(play, claimed_clock, vec![]),
                    no(off_player, idle, vec![]),
                    yes(superseded_by_playback, claimed_clock, vec![]),
                    yes(superseded_by_exit, idle, vec![]),
                    yes(clock_answered(1, true), idle, vec![]),
                    yes(clock_answered(1, false), clock_pending, vec![]),
                ],
            },
            Row {
                name: "ClockPending",
                state: clock_pending,
                park: (playing, playing),
                cells: vec![
                    yes(did, clock_pending, vec![]),
                    yes(did_superseded, clock_pending, vec![]),
                    yes(play, claimed_clock, vec![Fx::PlayClock { id: 1 }]),
                    no(off_player, idle, vec![]),
                    yes(superseded_by_playback, clock_pending, vec![]),
                    yes(superseded_by_exit, idle, vec![]),
                ],
            },
        ];

        // The table covers every state and every event: a new variant fails these before it fails
        // silently.
        let state_kind = |s: &St| match s {
            ForegroundState::Idle => 0,
            ForegroundState::Suspended { .. } => 1,
            ForegroundState::Claimed { .. } => 2,
            ForegroundState::Prepared { .. } => 3,
            ForegroundState::LoadPending { .. } => 4,
            ForegroundState::PreparePending { .. } => 5,
            ForegroundState::RecoveryPending { .. } => 6,
            ForegroundState::ClockPending { .. } => 7,
        };
        let event_kind = |e: &Ev| match e {
            ForegroundEvent::Park { .. } => 0,
            ForegroundEvent::ParkFlight => 1,
            ForegroundEvent::Input(_) => 2,
            ForegroundEvent::PrepareAnswered { .. } => 3,
            ForegroundEvent::LoadAnswered { .. } => 4,
            ForegroundEvent::FlightStale => 5,
            ForegroundEvent::LoadStatus { .. } => 6,
            ForegroundEvent::ClockAnswered { .. } => 7,
        };
        let mut states: Vec<_> = rows.iter().map(|row| state_kind(&row.state)).collect();
        states.sort_unstable();
        states.dedup();
        assert_eq!(states.len(), 8, "a row for every ForegroundState");
        let mut kinds: Vec<_> = events.iter().map(event_kind).collect();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), 8, "an event for every ForegroundEvent");

        for row in &rows {
            for (i, cell) in row.cells.iter().enumerate() {
                assert!(events.contains(&cell.0), "{}: {:?} is not in the event list", row.name, cell.0);
                assert!(
                    row.cells[..i].iter().all(|earlier| earlier.0 != cell.0),
                    "{}: {:?} is listed twice",
                    row.name,
                    cell.0
                );
            }
            for ev in &events {
                let parks = matches!(ev, ForegroundEvent::Park { .. });
                let (next, fx, handled) = match *ev {
                    ForegroundEvent::Park { saved_ns, viewer_paused } => (
                        ForegroundState::Suspended {
                            id: 2,
                            saved_ns,
                            clock: if viewer_paused { row.park.0 } else { row.park.1 },
                        },
                        vec![],
                        Handled::Yes,
                    ),
                    _ => match row.cells.iter().find(|cell| cell.0 == *ev) {
                        Some((_, next, fx, handled)) => (*next, fx.clone(), *handled),
                        None => (row.state, vec![], Handled::No),
                    },
                };
                let label = format!("{} x {ev:?}", row.name);
                let mut lifecycle = ForegroundLifecycle::<u64> { state: row.state, next_id: 1 };
                let mut got = Vec::new();
                assert_eq!(lifecycle.step(ev, &mut got), handled, "{label}: handled");
                assert_eq!(lifecycle.state, next, "{label}: state");
                assert_eq!(got, fx, "{label}: effects");
                assert_eq!(lifecycle.next_id, if parks { 2 } else { 1 }, "{label}: only a park takes an id");
            }
        }
    }
}
