//! The player's transport-pause contract and its app-switch lifecycle machine.
//!
//! `ForegroundLifecycle` is a tested `State + Event -> Effects` machine, and the transport helpers
//! (`paused`, `viewer_paused`, `set_transport_paused`, ...) are the one door every surface reaches
//! the player's pause through. All of it describes the PLAYER's transport and its resume after an
//! OS suspend, and the player machine ([`super::machine::Player`]), the detail page's trailer
//! transport ([`super::preview::transport`]) and the claim hold ([`super::claim_hold`]) all name
//! it, so it lives here rather than in `app::lifecycle` (a layer above the player). The loop's
//! adapter for it, `PlayerForegroundActuator`, needs `app` state and stays in `app::lifecycle`,
//! which re-exports everything below so app code spells the names it always did.
//!
//! Moved out of `app/lifecycle.rs` verbatim in module-layer step L12 (a pure move; the
//! `RESUME_REWIND_NS` constant came with it from `app/mod.rs`, and `resume_if_paused` from
//! `app/playback.rs`).

use std::sync::atomic::Ordering::Relaxed;

/// How far an OS-suspended, still-playing session rewinds when it is resumed (a paused one resumes
/// at exactly the saved offset).
const RESUME_REWIND_NS: i64 = 5_000_000_000;

// transport state — was the C playback globals; now crate::player (atomics)
#[inline]
pub fn paused() -> bool {
    crate::player::TX.paused.load(Relaxed)
}
#[inline]
pub fn set_paused(v: bool) {
    crate::player::TX.commit_paused(v)
}
/// PURE: the paused value a transport request settles on. `Some(true)` is the remote's PLAY key,
/// `Some(false)` its PAUSE, and `None` the single PLAYPAUSE toggle — which is the only one of the
/// three that needs the current state at all.
///
/// One function rather than a `match` per caller: the player's `PlayerReq::Transport` and the
/// detail page's full-trailer transport (`player::preview::transport`) answer the same key ladder,
/// and two hand-spelled copies of a three-arm match is exactly how a PLAY key ends up toggling on
/// one surface and resuming on the other.
pub fn transport_target(play: Option<bool>, paused: bool) -> bool {
    match play {
        Some(true) => false,
        Some(false) => true,
        None => !paused,
    }
}

/// The transport state the VIEWER sees and their next press toggles.
///
/// A claim's presentation hold (`player::claim_hold`) pauses the stream too, but that pause is the
/// hold's loan, not the viewer's: while it stands the viewer sees a spinner over a playing intent,
/// so the toggle means Pause (their intent, which the hold's restore then honours) and a seek's
/// resume leaves the stream held until the landing.
pub fn viewer_paused() -> bool {
    paused() && !crate::player::claim_hold::owns_pause()
}

/// The clock intent an OS suspend saves for the foreground restore. Reads the transport through
/// the same lens as the viewer's toggle ([`viewer_paused`]); the pause a claim's presentation hold
/// is keeping is the hold's loan and must not be saved as the viewer's, or foregrounding a session
/// that was playing under a spinner would restore it Paused.
pub fn clock_for_suspend_now(
    lifecycle: &ForegroundLifecycle,
) -> ForegroundClock {
    lifecycle.clock_for_suspend(viewer_paused())
}

/// Ask the synchronized player clock to commit a user Pause/Resume. The player publishes the feed
/// gate at the same accepted native boundary; keeping a second commit here used to leave a window
/// in which deadline accounting still treated an already-accepted Pause as active playback.
/// Resume during an internal HLS hold is accepted as a deferred transition: feeding reopens, while
/// measured re-prime owns both the eventual Starfish Play and the matching ACB Resume.
pub fn set_transport_paused(
    pa: &mut crate::player::adapter::PlayerAdapter,
    value: bool,
) -> bool {
    // PLAY while a hold owns the pause: the viewer sees a spinner over a playing intent, so Play is
    // already satisfied. Resuming here would run the OLD stream mid-flight and replay its seconds
    // after the landing, which still owes the play-back. Not a press that outranks the hold.
    if !value && crate::player::claim_hold::owns_pause() {
        return true;
    }
    // A viewer press outranks a claim's presentation hold (`player::claim_hold`), including the
    // press that finds the transport already in the state it asks for.
    crate::player::claim_hold::note_user_transport();
    if paused() == value {
        return true;
    }
    if value {
        crate::player::pause(pa)
    } else {
        crate::player::resume(pa)
    }
}

/// Resume if a seek landed while paused — the twin of `commit_seek`, which is the
/// stay-paused variant. Written out four separate times in `app::playback` before it had a name.
pub fn resume_if_paused(pa: &mut crate::player::adapter::PlayerAdapter) {
    if viewer_paused() {
        set_transport_paused(pa, false);
    }
}

/// The only two inputs which may claim an OS-suspended playback session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundInput {
    DidForeground,
    PlayKey,
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

#[derive(Debug)]
pub struct ForegroundLifecycle<Attempt = crate::route::RouteStartAttempt> {
    pub state: ForegroundState<Attempt>,
    pub next_id: u64,
}

impl<Attempt: Copy + PartialEq> ForegroundLifecycle<Attempt> {
    pub const IDLE: Self = Self {
        state: ForegroundState::Idle,
        next_id: 0,
    };

    pub fn suspend(&mut self, saved_ns: i64, clock: ForegroundClock) {
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == 0 {
            self.next_id = 1;
        }
        self.state = ForegroundState::Suspended {
            id: self.next_id,
            saved_ns,
            clock,
        };
    }

    /// The physical clock can remain Paused after a refused foreground Play. Preserve the
    /// viewer's newer Playing intent if the OS backgrounds that live session again.
    pub fn clock_for_suspend(&self, transport_paused: bool) -> ForegroundClock {
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

    /// `ClockPending` belongs to an already-loaded player. If a later real exit has removed that
    /// route, its retry must not intercept Play for the next item.
    pub fn discard_started_state(&mut self) {
        if matches!(
            self.state,
            ForegroundState::ClockPending { .. }
                | ForegroundState::Claimed {
                    claim: ForegroundClaim::PlayClock,
                    ..
                }
        ) {
            self.state = ForegroundState::Idle;
        }
    }

    /// Retire a parked foreground owner when a distinct playback request has already been
    /// accepted. `Prepared` remains untouched for the same session's Play-key retry; that path
    /// claims the lifecycle directly and never owns a fresh route resolve.
    pub fn replace_with_new_playback(&mut self) -> bool {
        if !self.awaiting_load() {
            return false;
        }
        self.state = ForegroundState::Idle;
        true
    }

    pub fn claim(&mut self, input: ForegroundInput) -> ForegroundClaimResult {
        let state = self.state;
        match state {
            ForegroundState::Idle => ForegroundClaimResult::Ordinary,
            ForegroundState::Suspended {
                id,
                saved_ns,
                clock: saved_clock,
            } => {
                let clock = if matches!(input, ForegroundInput::PlayKey) {
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
                ForegroundClaimResult::Effect(ForegroundEffect::Prepare { id, resume_ns })
            }
            ForegroundState::Prepared {
                id,
                resume_ns,
                clock,
            } => {
                let clock = if matches!(input, ForegroundInput::PlayKey) {
                    ForegroundClock::Playing
                } else {
                    clock
                };
                self.state = ForegroundState::Claimed {
                    id,
                    claim: ForegroundClaim::Load { resume_ns, clock },
                };
                ForegroundClaimResult::Effect(ForegroundEffect::Load {
                    id,
                    resume_ns,
                    clock,
                })
            }
            ForegroundState::ClockPending { id } if matches!(input, ForegroundInput::PlayKey) => {
                self.state = ForegroundState::Claimed {
                    id,
                    claim: ForegroundClaim::PlayClock,
                };
                ForegroundClaimResult::Effect(ForegroundEffect::PlayClock { id })
            }
            // A lifecycle edge or key which arrives while another claimant owns the effect is
            // consumed, not allowed to fall through to the ordinary start path.
            ForegroundState::Claimed { .. } | ForegroundState::ClockPending { .. } => {
                ForegroundClaimResult::Suppressed
            }
            ForegroundState::LoadPending { .. }
            | ForegroundState::PreparePending { .. }
            | ForegroundState::RecoveryPending { .. } => ForegroundClaimResult::Suppressed,
        }
    }

    pub fn finish_prepare(&mut self, id: u64, prepared: bool) -> Option<ForegroundEffect> {
        let ForegroundState::Claimed {
            id: owner,
            claim:
                ForegroundClaim::Prepare {
                    saved_ns,
                    saved_clock,
                    resume_ns,
                    clock,
                },
        } = self.state
        else {
            return None;
        };
        if owner != id {
            return None;
        }
        if !prepared {
            self.state = ForegroundState::Suspended {
                id,
                saved_ns,
                clock: saved_clock,
            };
            return None;
        }
        self.state = ForegroundState::Claimed {
            id,
            claim: ForegroundClaim::Load { resume_ns, clock },
        };
        Some(ForegroundEffect::Load {
            id,
            resume_ns,
            clock,
        })
    }

    /// The claimed `Prepare` is a flight now: park until its landing ([`Self::reclaim_prepare`]).
    pub fn finish_prepare_pending(&mut self, id: u64) -> bool {
        let ForegroundState::Claimed {
            id: owner,
            claim:
                ForegroundClaim::Prepare {
                    saved_ns,
                    saved_clock,
                    resume_ns,
                    clock,
                },
        } = self.state
        else {
            return false;
        };
        if owner != id {
            return false;
        }
        self.state = ForegroundState::PreparePending { id, saved_ns, saved_clock, resume_ns, clock };
        true
    }

    /// The claimed `Load`'s Original rollback is a flight now: park until its landing
    /// ([`Self::reclaim_recovery`]).
    pub fn finish_load_pending(&mut self, id: u64) -> bool {
        let ForegroundState::Claimed {
            id: owner,
            claim: ForegroundClaim::Load { resume_ns, clock },
        } = self.state
        else {
            return false;
        };
        if owner != id {
            return false;
        }
        self.state = ForegroundState::RecoveryPending { id, resume_ns, clock };
        true
    }

    /// The pending `Prepare`'s flight landed: take the claim back so [`Self::finish_prepare`]
    /// settles it with the outcome.
    pub fn reclaim_prepare(&mut self) -> Option<u64> {
        let ForegroundState::PreparePending { id, saved_ns, saved_clock, resume_ns, clock } = self.state else {
            return None;
        };
        self.state = ForegroundState::Claimed {
            id,
            claim: ForegroundClaim::Prepare { saved_ns, saved_clock, resume_ns, clock },
        };
        Some(id)
    }

    /// The pending rollback's flight landed: take the `Load` claim back to settle it with the
    /// reload's outcome.
    pub fn reclaim_recovery(&mut self) -> Option<(u64, ForegroundClock)> {
        let ForegroundState::RecoveryPending { id, resume_ns, clock } = self.state else {
            return None;
        };
        self.state = ForegroundState::Claimed { id, claim: ForegroundClaim::Load { resume_ns, clock } };
        Some((id, clock))
    }

    /// Whether the machine is waiting on a flight's landing ([`poll_foreground_flight`]).
    pub fn flight_pending(&self) -> bool {
        matches!(
            self.state,
            ForegroundState::PreparePending { .. } | ForegroundState::RecoveryPending { .. }
        )
    }

    /// The pending flight ended without landing here (a Back, a newer playback request, a suspend
    /// that already dropped it): whoever ended it owns what happens next, so the machine lets go.
    pub fn release_stale_flight(&mut self) {
        if self.flight_pending() {
            self.state = ForegroundState::Idle;
        }
    }

    /// An app-switch suspend arrived while a flight was pending: its landing is discarded, and the
    /// parked session goes back to waiting for the next foreground exactly where it was (a pending
    /// rollback has no snapshot but its resume position, which the next restore rebuilds from).
    pub fn park_pending_flight(&mut self) -> bool {
        self.state = match self.state {
            ForegroundState::PreparePending { id, saved_ns, saved_clock, .. } => {
                ForegroundState::Suspended { id, saved_ns, clock: saved_clock }
            }
            ForegroundState::RecoveryPending { id, resume_ns, clock } => {
                ForegroundState::Suspended { id, saved_ns: resume_ns, clock }
            }
            _ => return false,
        };
        true
    }

    pub fn finish_load_launch(&mut self, id: u64, attempt: Attempt) -> bool {
        let ForegroundState::Claimed {
            id: owner,
            claim: ForegroundClaim::Load { resume_ns, clock },
        } = self.state
        else {
            return false;
        };
        if owner != id {
            return false;
        }
        self.state = ForegroundState::LoadPending {
            id,
            attempt,
            resume_ns,
            clock,
        };
        true
    }

    pub fn finish_load_refusal(&mut self, id: u64) -> bool {
        let ForegroundState::Claimed {
            id: owner,
            claim: ForegroundClaim::Load { resume_ns, clock },
        } = self.state
        else {
            return false;
        };
        if owner != id {
            return false;
        }
        self.state = ForegroundState::Prepared {
            id,
            resume_ns,
            clock,
        };
        true
    }

    pub fn finish_load_terminal(&mut self, id: u64) -> bool {
        let ForegroundState::Claimed {
            id: owner,
            claim: ForegroundClaim::Load { .. },
        } = self.state
        else {
            return false;
        };
        if owner != id {
            return false;
        }
        self.state = ForegroundState::Idle;
        true
    }

    pub fn pending_load_attempt(&self) -> Option<Attempt> {
        match self.state {
            ForegroundState::LoadPending { attempt, .. } => Some(attempt),
            _ => None,
        }
    }

    pub fn settle_load(
        &mut self,
        attempt: Attempt,
        status: ForegroundLoadStatus<Attempt>,
    ) -> ForegroundLoadSettlement {
        let ForegroundState::LoadPending {
            id,
            attempt: owner,
            resume_ns,
            clock,
        } = self.state
        else {
            return ForegroundLoadSettlement::Inactive;
        };
        if owner != attempt {
            return ForegroundLoadSettlement::Inactive;
        }
        match status {
            ForegroundLoadStatus::Pending => ForegroundLoadSettlement::Pending,
            ForegroundLoadStatus::Superseded(replacement) => {
                self.state = ForegroundState::LoadPending {
                    id,
                    attempt: replacement,
                    resume_ns,
                    clock,
                };
                ForegroundLoadSettlement::Pending
            }
            ForegroundLoadStatus::Failed => {
                self.state = ForegroundState::Prepared {
                    id,
                    resume_ns,
                    clock,
                };
                ForegroundLoadSettlement::Finished {
                    clock,
                    started: false,
                    effect: None,
                }
            }
            ForegroundLoadStatus::Stale => {
                // Neither the observed Load nor a tokened replacement owns the route any more.
                // Release foreground ownership instead of retrying an unknown candidate or
                // destroying whichever Engine the main reducer may now own.
                self.state = ForegroundState::Idle;
                ForegroundLoadSettlement::Finished {
                    clock,
                    started: false,
                    effect: None,
                }
            }
            ForegroundLoadStatus::Started => {
                let effect = if matches!(clock, ForegroundClock::Paused) {
                    self.state = ForegroundState::Idle;
                    None
                } else {
                    self.state = ForegroundState::Claimed {
                        id,
                        claim: ForegroundClaim::PlayClock,
                    };
                    Some(ForegroundEffect::PlayClock { id })
                };
                ForegroundLoadSettlement::Finished {
                    clock,
                    started: true,
                    effect,
                }
            }
        }
    }

    pub fn finish_clock(&mut self, id: u64, accepted: bool) {
        if !matches!(
            self.state,
            ForegroundState::Claimed {
                id: owner,
                claim: ForegroundClaim::PlayClock,
            } if owner == id
        ) {
            return;
        }
        self.state = if accepted {
            ForegroundState::Idle
        } else {
            ForegroundState::ClockPending { id }
        };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundClaimResult {
    Ordinary,
    Suppressed,
    Effect(ForegroundEffect),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundEffect {
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
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundLoadStart<Attempt> {
    /// A rollback flight owns the way forward ([`ForegroundActuator::land_recovery`]).
    Pending,
    AlreadyRunning,
    Launched(Attempt),
    Failed,
    Terminal,
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
pub enum ForegroundLoadStatus<Attempt> {
    Pending,
    Started,
    Failed,
    Superseded(Attempt),
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundLoadSettlement {
    Inactive,
    Pending,
    Finished {
        clock: ForegroundClock,
        started: bool,
        effect: Option<ForegroundEffect>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundPollOutcome {
    Inactive,
    Pending,
    Started,
    Failed,
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
    let effect = match lifecycle.claim(input) {
        ForegroundClaimResult::Ordinary => return ForegroundActivation::Ordinary,
        ForegroundClaimResult::Suppressed => return ForegroundActivation::Handled,
        ForegroundClaimResult::Effect(effect) => Some(effect),
    };
    run_foreground_effects(lifecycle, ps, actuator, effect)
}

fn run_foreground_effects<A: ForegroundActuator>(
    lifecycle: &mut ForegroundLifecycle<A::Attempt>,
    ps: &mut crate::route::PlaybackSession,
    actuator: &mut A,
    mut effect: Option<ForegroundEffect>,
) -> ForegroundActivation {
    let mut launched = false;
    while let Some(next) = effect.take() {
        effect = match next {
            ForegroundEffect::Prepare { id, resume_ns } => match actuator.prepare_resume(ps, resume_ns) {
                crate::player::ResumeStart::Settled(outcome) => {
                    lifecycle.finish_prepare(id, matches!(outcome, crate::player::ResumeOutcome::Prepared))
                }
                crate::player::ResumeStart::Pending => {
                    let _ = lifecycle.finish_prepare_pending(id);
                    None
                }
            },
            ForegroundEffect::Load {
                id,
                resume_ns,
                clock,
            } => {
                actuator.before_load(resume_ns, clock);
                let start = actuator.start_load(ps);
                launched |= settle_load_start(lifecycle, ps, actuator, id, clock, start);
                None
            }
            ForegroundEffect::PlayClock { id } => {
                lifecycle.finish_clock(id, actuator.play_clock());
                None
            }
        };
    }
    if launched {
        ForegroundActivation::Launched
    } else {
        ForegroundActivation::Handled
    }
}

/// Settle the claimed `Load` with what starting it answered. True when an exact attempt launched.
fn settle_load_start<A: ForegroundActuator>(
    lifecycle: &mut ForegroundLifecycle<A::Attempt>,
    ps: &mut crate::route::PlaybackSession,
    actuator: &mut A,
    id: u64,
    clock: ForegroundClock,
    start: ForegroundLoadStart<A::Attempt>,
) -> bool {
    match start {
        ForegroundLoadStart::Launched(attempt) => lifecycle.finish_load_launch(id, attempt),
        ForegroundLoadStart::Pending => {
            let _ = lifecycle.finish_load_pending(id);
            false
        }
        ForegroundLoadStart::Failed => {
            actuator.after_load(ps, None, clock, false);
            let _ = lifecycle.finish_load_refusal(id);
            false
        }
        // No exact candidate can be followed. `AlreadyRunning` is a stable Engine outside this
        // suspended lifecycle; `Terminal` means Original rollback could not construct a truthful
        // route. Neither is a retryable Prepared edge.
        ForegroundLoadStart::AlreadyRunning | ForegroundLoadStart::Terminal => {
            actuator.after_load(ps, None, clock, false);
            let _ = lifecycle.finish_load_terminal(id);
            false
        }
    }
}

/// Once a frame while the machine waits on a flight ([`ForegroundLifecycle::flight_pending`]):
/// drain its landing and carry on from where the synchronous path would have been. A landed
/// resume continues into the Load; a landed rollback settles the Load with its reload's outcome;
/// a refused rebuild ends exactly as the synchronous `None` did (the parked session waits for the
/// next foreground), and a flight that ended without landing releases the machine. `Launched`
/// tells the caller to mount Player, as [`drive_foreground`]'s does.
pub fn poll_foreground_flight<A: ForegroundActuator>(
    lifecycle: &mut ForegroundLifecycle<A::Attempt>,
    ps: &mut crate::route::PlaybackSession,
    actuator: &mut A,
) -> ForegroundActivation {
    match lifecycle.state {
        ForegroundState::PreparePending { .. } => match actuator.land_prepare(ps) {
            FlightLanding::Waiting => ForegroundActivation::Handled,
            FlightLanding::Stale => {
                lifecycle.release_stale_flight();
                ForegroundActivation::Handled
            }
            FlightLanding::Landed(outcome) => {
                let Some(id) = lifecycle.reclaim_prepare() else {
                    return ForegroundActivation::Handled;
                };
                let effect = lifecycle
                    .finish_prepare(id, matches!(outcome, crate::player::ResumeOutcome::Prepared));
                run_foreground_effects(lifecycle, ps, actuator, effect)
            }
        },
        ForegroundState::RecoveryPending { .. } => match actuator.land_recovery(ps) {
            FlightLanding::Waiting => ForegroundActivation::Handled,
            FlightLanding::Stale => {
                lifecycle.release_stale_flight();
                ForegroundActivation::Handled
            }
            FlightLanding::Landed(start) => {
                let Some((id, clock)) = lifecycle.reclaim_recovery() else {
                    return ForegroundActivation::Handled;
                };
                if settle_load_start(lifecycle, ps, actuator, id, clock, start) {
                    ForegroundActivation::Launched
                } else {
                    ForegroundActivation::Handled
                }
            }
        },
        _ => ForegroundActivation::Handled,
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
    if !lifecycle.flight_pending() {
        return false;
    }
    crate::player::suspend_bufferfeed(ps, pa);
    lifecycle.park_pending_flight()
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
    let cleanup_attempt = matches!(status, ForegroundLoadStatus::Failed).then_some(attempt);
    match lifecycle.settle_load(attempt, status) {
        ForegroundLoadSettlement::Inactive => ForegroundPollOutcome::Inactive,
        ForegroundLoadSettlement::Pending => ForegroundPollOutcome::Pending,
        ForegroundLoadSettlement::Finished {
            clock,
            started,
            effect,
        } => {
            actuator.after_load(
                ps,
                if started {
                    Some(attempt)
                } else {
                    cleanup_attempt
                },
                clock,
                started,
            );
            if let Some(ForegroundEffect::PlayClock { id }) = effect {
                lifecycle.finish_clock(id, actuator.play_clock());
            }
            if started {
                ForegroundPollOutcome::Started
            } else {
                ForegroundPollOutcome::Failed
            }
        }
    }
}

#[cfg(test)]
mod transport_target_tests {
    /// The three keys, both ways round. A PLAY key on an already-playing transport is a no-op
    /// rather than a pause, and a PAUSE key on an already-paused one likewise — the property a
    /// per-caller `match` kept getting wrong, and the reason the player's transport and the detail
    /// page's trailer transport share this one function.
    #[test]
    fn only_the_toggle_depends_on_the_current_state() {
        for paused in [false, true] {
            assert!(!super::transport_target(Some(true), paused), "PLAY plays");
            assert!(super::transport_target(Some(false), paused), "PAUSE pauses");
        }
        assert!(super::transport_target(None, false), "PLAYPAUSE pauses a playing transport");
        assert!(!super::transport_target(None, true), "…and resumes a paused one");
    }
}

#[cfg(test)]
mod foreground_resume_tests {
    use super::*;

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

    #[test]
    fn a_claim_suppresses_did_and_play_until_its_effect_settles() {
        let mut lifecycle = ForegroundLifecycle::<u64>::IDLE;
        lifecycle.suspend(73_000_000_000, ForegroundClock::Paused);
        let first = lifecycle.claim(ForegroundInput::PlayKey);
        assert_eq!(
            first,
            ForegroundClaimResult::Effect(ForegroundEffect::Prepare {
                id: 1,
                resume_ns: 73_000_000_000,
            })
        );
        assert_eq!(
            lifecycle.claim(ForegroundInput::DidForeground),
            ForegroundClaimResult::Suppressed
        );
        assert_eq!(
            lifecycle.claim(ForegroundInput::PlayKey),
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
            lifecycle.suspend(91_000_000_000, ForegroundClock::Paused);
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
        lifecycle.suspend(73_000_000_000, ForegroundClock::Paused);
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
            lifecycle.claim(ForegroundInput::PlayKey),
            ForegroundClaimResult::Suppressed,
            "Play cannot claim a second Load while the first is pending"
        );
        assert_eq!(
            lifecycle.claim(ForegroundInput::DidForeground),
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

    /// **A resume whose rebuild is a flight parks the machine, and every landing settles it exactly
    /// as the synchronous answer would have.** `Waiting` changes nothing and consumes every claim;
    /// `Landed(Prepared)` carries on into the Load; a refused rebuild re-arms the suspended
    /// snapshot; a flight that ended without landing releases the machine instead of hanging it.
    #[test]
    fn a_pending_resume_parks_the_machine_until_its_landing_and_settles_like_the_synchronous_answer() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut lifecycle = ForegroundLifecycle::IDLE;
        lifecycle.suspend(63_000_000_000, ForegroundClock::Playing);
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
        lifecycle.suspend(63_000_000_000, ForegroundClock::Playing);
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
        lifecycle.suspend(63_000_000_000, ForegroundClock::Playing);
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
            lifecycle.suspend(63_000_000_000, ForegroundClock::Playing);
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
        lifecycle.suspend(63_000_000_000, ForegroundClock::Playing);
        let suspended = lifecycle.state;
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut actuator = FakeActuator { pending_prepares: 1, ..FakeActuator::default() };
        let _ = drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::DidForeground, &mut actuator);
        assert!(lifecycle.flight_pending());
        assert!(lifecycle.park_pending_flight());
        assert_eq!(lifecycle.state, suspended);
        assert!(!lifecycle.park_pending_flight(), "nothing pending any more");

        let mut actuator = FakeActuator {
            prepare: vec![crate::player::ResumeOutcome::Prepared],
            loads: vec![ForegroundLoadStart::Pending],
            ..FakeActuator::default()
        };
        let _ = drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::DidForeground, &mut actuator);
        assert!(matches!(lifecycle.state, ForegroundState::RecoveryPending { .. }));
        assert!(lifecycle.park_pending_flight());
        assert!(matches!(
            lifecycle.state,
            ForegroundState::Suspended { saved_ns: 58_000_000_000, clock: ForegroundClock::Playing, .. }
        ));
    }

    #[test]
    fn pending_load_failure_retains_prepared_route_and_retry_skips_resume_preparation() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut lifecycle = ForegroundLifecycle::IDLE;
        lifecycle.suspend(63_000_000_000, ForegroundClock::Playing);
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
        assert_eq!(
            lifecycle.settle_load(99, ForegroundLoadStatus::Failed),
            ForegroundLoadSettlement::Inactive,
            "another attempt cannot settle the foreground owner"
        );
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
        lifecycle.suspend(29_000_000_000, ForegroundClock::Playing);
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
        lifecycle.suspend(41_000_000_000, ForegroundClock::Playing);
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
            lifecycle.suspend(41_000_000_000, ForegroundClock::Playing);
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
        lifecycle.suspend(22_000_000_000, ForegroundClock::Paused);
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
        lifecycle.suspend(31_000_000_000, ForegroundClock::Playing);
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
        lifecycle.suspend(73_000_000_000, ForegroundClock::Paused);
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
            lifecycle.claim(ForegroundInput::DidForeground),
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
}

#[cfg(all(test, feature = "hostsim"))]
mod transport_pause_contract_tests {
    use super::{
        drive_foreground, paused, poll_foreground_load, set_transport_paused, FlightLanding,
        ForegroundActivation, ForegroundActuator, ForegroundClock, ForegroundInput, ForegroundLifecycle,
        ForegroundLoadStart, ForegroundLoadStatus, ForegroundPollOutcome, ForegroundState,
    };
    use std::sync::atomic::Ordering;

    #[test]
    fn a_refused_native_pause_or_play_cannot_diverge_the_feed_gate() {
        let _guard = plx_base::testlock::serial();
        let old_paused = crate::player::TX.paused.load(Ordering::Acquire);
        crate::player::TX.commit_paused(false);
        let old_rebuffering = crate::player::SHARED
            .hls_rebuffering
            .swap(false, Ordering::AcqRel);
        struct Restore {
            paused: bool,
            rebuffering: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::player::force_pause_result_for_test(None);
                crate::player::force_play_result_for_test(None);
                crate::player::TX.commit_paused(self.paused);
                crate::player::SHARED
                    .hls_rebuffering
                    .store(self.rebuffering, Ordering::Release);
            }
        }
        let _restore = Restore {
            paused: old_paused,
            rebuffering: old_rebuffering,
        };
        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });

        crate::player::force_pause_result_for_test(Some(0));
        assert!(!set_transport_paused(&mut pa, true));
        assert!(
            !paused(),
            "feed must continue when the native clock refused Pause"
        );

        crate::player::force_pause_result_for_test(Some(1));
        assert!(set_transport_paused(&mut pa, true));
        assert!(paused());

        crate::player::force_play_result_for_test(Some(0));
        assert!(!set_transport_paused(&mut pa, false));
        assert!(
            paused(),
            "feed must remain stopped when the native clock refused Play"
        );

        crate::player::force_play_result_for_test(Some(1));
        assert!(set_transport_paused(&mut pa, false));
        assert!(!paused());
    }

    #[test]
    fn refused_foreground_play_retries_the_native_clock_without_a_second_load() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _guard = plx_base::testlock::serial();
        let old_paused = crate::player::TX.paused.load(Ordering::Acquire);
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::player::force_pause_result_for_test(None);
                crate::player::force_play_result_for_test(None);
                crate::player::SHARED.reset_hls_clock_for_test();
                crate::player::TX.commit_paused(self.0);
            }
        }
        let _restore = Restore(old_paused);
        crate::player::SHARED.reset_hls_clock_for_test();
        crate::player::TX.commit_paused(false);
        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });
        crate::player::force_pause_result_for_test(Some(1));
        assert!(set_transport_paused(&mut pa, true));

        struct Actuator<'a> {
            pa: &'a mut crate::player::adapter::PlayerAdapter,
            prepares: usize,
            loads: usize,
        }
        impl ForegroundActuator for Actuator<'_> {
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
                self.loads += 1;
                ForegroundLoadStart::Launched(1)
            }

            fn load_status(
                &mut self,
                attempt: Self::Attempt,
            ) -> ForegroundLoadStatus<Self::Attempt> {
                assert_eq!(attempt, 1);
                ForegroundLoadStatus::Started
            }

            fn after_load(
                &mut self,
                _ps: &mut crate::route::PlaybackSession,
                _attempt: Option<Self::Attempt>,
                _clock: ForegroundClock,
                _started: bool,
            ) {
            }

            fn play_clock(&mut self) -> bool {
                set_transport_paused(self.pa, false)
            }
        }

        let mut lifecycle = ForegroundLifecycle::IDLE;
        lifecycle.suspend(42_000_000_000, ForegroundClock::Paused);
        let mut actuator = Actuator {
            pa: &mut pa,
            prepares: 0,
            loads: 0,
        };
        crate::player::force_play_result_for_test(Some(0));
        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::PlayKey, &mut actuator,),
            ForegroundActivation::Launched,
        );
        assert!(matches!(
            lifecycle.state,
            ForegroundState::LoadPending { .. }
        ));
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut ps, &mut actuator),
            ForegroundPollOutcome::Started,
        );
        assert!(matches!(
            lifecycle.state,
            ForegroundState::ClockPending { .. }
        ));
        assert_eq!((actuator.prepares, actuator.loads), (1, 1));
        assert!(paused(), "a refused native Play keeps the feed held");

        crate::player::force_play_result_for_test(Some(1));
        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::PlayKey, &mut actuator,),
            ForegroundActivation::Handled,
        );
        assert_eq!(
            (actuator.prepares, actuator.loads),
            (1, 1),
            "the retry reached preparation or Load instead of the native clock",
        );
        assert_eq!(lifecycle.state, ForegroundState::Idle);
        assert!(!paused());
    }
}
