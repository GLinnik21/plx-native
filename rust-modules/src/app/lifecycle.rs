//! The foreground/background lifecycle machine (`ForegroundLifecycle`, already a tested
//! `State + Event -> Effects` machine) and the transport-pause contract. Moved out of `app.rs`
//! verbatim in phase 1a (a pure move; `pub(super)` widening only).

use super::*;

// transport state — was the C playback globals; now crate::player (atomics)
#[inline]
pub(super) fn paused() -> bool {
    crate::player::TX.paused.load(Relaxed)
}
#[inline]
pub(super) fn set_paused(v: bool) {
    crate::player::TX.commit_paused(v)
}
/// Ask the synchronized player clock to commit a user Pause/Resume. The player publishes the feed
/// gate at the same accepted native boundary; keeping a second commit here used to leave a window
/// in which deadline accounting still treated an already-accepted Pause as active playback.
/// Resume during an internal HLS hold is accepted as a deferred transition: feeding reopens, while
/// measured re-prime owns both the eventual Starfish Play and the matching ACB Resume.
pub(super) fn set_transport_paused(mt: &crate::task::MainThread, value: bool) -> bool {
    if paused() == value {
        return true;
    }
    if value {
        crate::player::pause(mt)
    } else {
        crate::player::resume(mt)
    }
}

/// The only two inputs which may claim an OS-suspended playback session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForegroundInput {
    DidForeground,
    PlayKey,
}

/// Viewer clock intent carried independently of the native Engine's current clock state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForegroundClock {
    Paused,
    Playing,
}

impl ForegroundClock {
    pub(super) fn from_paused(paused: bool) -> Self {
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
pub(super) enum ForegroundClaim {
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

/// Complete app-switch resume state. In particular, `Prepared` means `resume_at` and route
/// preparation have completed, including a transcode URL rebuild when required: retrying its
/// failed native Load must not rebuild that route again.
/// The attempt type is opaque to the reducer. Production uses `RouteStartAttempt`, while pure
/// tests can prove exact-attempt handling with an ordinary integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForegroundState<Attempt = crate::route::RouteStartAttempt> {
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
    ClockPending {
        id: u64,
    },
}

#[derive(Debug)]
pub(super) struct ForegroundLifecycle<Attempt = crate::route::RouteStartAttempt> {
    pub(super) state: ForegroundState<Attempt>,
    pub(super) next_id: u64,
}

impl<Attempt: Copy + PartialEq> ForegroundLifecycle<Attempt> {
    pub(super) const IDLE: Self = Self {
        state: ForegroundState::Idle,
        next_id: 0,
    };

    pub(super) fn suspend(&mut self, saved_ns: i64, clock: ForegroundClock) {
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
    pub(super) fn clock_for_suspend(&self, transport_paused: bool) -> ForegroundClock {
        match self.state {
            ForegroundState::ClockPending { .. }
            | ForegroundState::Claimed {
                claim: ForegroundClaim::PlayClock,
                ..
            } => ForegroundClock::Playing,
            // A Play-key resume can still have a physically Paused clock until its exact Load
            // settles. If the OS backgrounds again in that window, preserve the viewer's intent
            // from the reducer rather than resnapshotting the deliberately stale native clock.
            ForegroundState::LoadPending { clock, .. } => clock,
            _ => ForegroundClock::from_paused(transport_paused),
        }
    }

    /// True while the preserved session is parked and awaiting a new native Load launch. The
    /// launched attempt is deliberately excluded: its Player route is live and a second OS
    /// background edge must suspend that Engine rather than ignore it.
    pub(super) fn awaiting_load(&self) -> bool {
        matches!(
            self.state,
            ForegroundState::Suspended { .. }
                | ForegroundState::Prepared { .. }
                | ForegroundState::Claimed {
                    claim: ForegroundClaim::Prepare { .. } | ForegroundClaim::Load { .. },
                    ..
                }
        )
    }

    /// `ClockPending` belongs to an already-loaded player. If a later real exit has removed that
    /// route, its retry must not intercept Play for the next item.
    pub(super) fn discard_started_state(&mut self) {
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

    pub(super) fn claim(&mut self, input: ForegroundInput) -> ForegroundClaimResult {
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
            ForegroundState::LoadPending { .. } => ForegroundClaimResult::Suppressed,
        }
    }

    pub(super) fn finish_prepare(&mut self, id: u64, prepared: bool) -> Option<ForegroundEffect> {
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

    pub(super) fn finish_load_launch(&mut self, id: u64, attempt: Attempt) -> bool {
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

    pub(super) fn finish_load_refusal(&mut self, id: u64) -> bool {
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

    pub(super) fn finish_load_terminal(&mut self, id: u64) -> bool {
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

    pub(super) fn pending_load_attempt(&self) -> Option<Attempt> {
        match self.state {
            ForegroundState::LoadPending { attempt, .. } => Some(attempt),
            _ => None,
        }
    }

    pub(super) fn settle_load(
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

    pub(super) fn finish_clock(&mut self, id: u64, accepted: bool) {
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
pub(super) enum ForegroundClaimResult {
    Ordinary,
    Suppressed,
    Effect(ForegroundEffect),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForegroundEffect {
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
pub(super) enum ForegroundActivation {
    Ordinary,
    Handled,
    Launched,
}

pub(super) trait ForegroundActuator {
    type Attempt: Copy + PartialEq;

    fn prepare_resume(&mut self, resume_ns: i64) -> crate::player::ResumeOutcome;
    fn before_load(&mut self, resume_ns: i64, clock: ForegroundClock);
    fn start_load(&mut self) -> ForegroundLoadStart<Self::Attempt>;
    fn load_status(&mut self, attempt: Self::Attempt) -> ForegroundLoadStatus<Self::Attempt>;
    fn after_load(&mut self, attempt: Option<Self::Attempt>, clock: ForegroundClock, started: bool);
    fn play_clock(&mut self) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForegroundLoadStart<Attempt> {
    AlreadyRunning,
    Launched(Attempt),
    Failed,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForegroundLoadStatus<Attempt> {
    Pending,
    Started,
    Failed,
    Superseded(Attempt),
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForegroundLoadSettlement {
    Inactive,
    Pending,
    Finished {
        clock: ForegroundClock,
        started: bool,
        effect: Option<ForegroundEffect>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForegroundPollOutcome {
    Inactive,
    Pending,
    Started,
    Failed,
}

/// Claim first, then execute the claimed synchronous effects in order. Launching the native Load
/// tells the caller to mount Player but deliberately does not issue Play: only the later exact
/// attempt result observed by `poll_foreground_load` may advance the clock.
pub(super) fn drive_foreground<A: ForegroundActuator>(
    lifecycle: &mut ForegroundLifecycle<A::Attempt>,
    input: ForegroundInput,
    actuator: &mut A,
) -> ForegroundActivation {
    let mut effect = match lifecycle.claim(input) {
        ForegroundClaimResult::Ordinary => return ForegroundActivation::Ordinary,
        ForegroundClaimResult::Suppressed => return ForegroundActivation::Handled,
        ForegroundClaimResult::Effect(effect) => Some(effect),
    };
    let mut launched = false;
    while let Some(next) = effect.take() {
        effect = match next {
            ForegroundEffect::Prepare { id, resume_ns } => {
                let prepared = matches!(
                    actuator.prepare_resume(resume_ns),
                    crate::player::ResumeOutcome::Prepared
                );
                lifecycle.finish_prepare(id, prepared)
            }
            ForegroundEffect::Load {
                id,
                resume_ns,
                clock,
            } => {
                actuator.before_load(resume_ns, clock);
                match actuator.start_load() {
                    ForegroundLoadStart::Launched(attempt) => {
                        launched |= lifecycle.finish_load_launch(id, attempt);
                    }
                    ForegroundLoadStart::Failed => {
                        actuator.after_load(None, clock, false);
                        let _ = lifecycle.finish_load_refusal(id);
                    }
                    // No exact candidate can be followed. `AlreadyRunning` is a stable Engine
                    // outside this suspended lifecycle; `Terminal` means Original rollback could
                    // not construct a truthful route. Neither is a retryable Prepared edge.
                    ForegroundLoadStart::AlreadyRunning | ForegroundLoadStart::Terminal => {
                        actuator.after_load(None, clock, false);
                        let _ = lifecycle.finish_load_terminal(id);
                    }
                }
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

/// Poll the exact native Load attempt after `player::pump` has drained its media-thread result.
/// Only a confirmed `Started` result may advance the viewer clock; failure retains the already
/// prepared route so a Play retry never repeats PMS preparation or `resume_at`.
pub(super) fn poll_foreground_load<A: ForegroundActuator>(
    lifecycle: &mut ForegroundLifecycle<A::Attempt>,
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

pub(super) struct PlayerForegroundActuator<'a> {
    pub(super) mt: &'a crate::task::MainThread,
    pub(super) repause_at: &'a mut i64,
}

impl ForegroundActuator for PlayerForegroundActuator<'_> {
    type Attempt = crate::route::RouteStartAttempt;

    fn prepare_resume(&mut self, resume_ns: i64) -> crate::player::ResumeOutcome {
        crate::player::resume_at(resume_ns)
    }

    fn before_load(&mut self, resume_ns: i64, clock: ForegroundClock) {
        if matches!(clock, ForegroundClock::Paused) {
            *self.repause_at = resume_ns;
            set_resume_pend(true);
            crate::player::TX.begin_paused_seek();
        }
    }

    fn start_load(&mut self) -> ForegroundLoadStart<Self::Attempt> {
        let first = crate::player::start_bufferfeed_tracked(self.mt);
        if matches!(first, crate::player::BufferfeedStartOutcome::Failed) {
            // A synchronous failure inside an Original trial has no Engine for player::pump to
            // recover. Take the same explicit rollback edge here and follow its exact HLS Load.
            return match crate::player::recover_failed_foreground_original(self.mt) {
                crate::player::ForegroundOriginalRecovery::NotOriginal
                | crate::player::ForegroundOriginalRecovery::RetryPrepared => {
                    ForegroundLoadStart::Failed
                }
                crate::player::ForegroundOriginalRecovery::Tracking(attempt) => {
                    ForegroundLoadStart::Launched(attempt)
                }
                crate::player::ForegroundOriginalRecovery::Terminal => {
                    ForegroundLoadStart::Terminal
                }
            };
        }
        match first {
            crate::player::BufferfeedStartOutcome::AlreadyRunning => {
                ForegroundLoadStart::AlreadyRunning
            }
            crate::player::BufferfeedStartOutcome::Launched(attempt) => {
                ForegroundLoadStart::Launched(attempt)
            }
            crate::player::BufferfeedStartOutcome::Failed => unreachable!("handled above"),
        }
    }

    fn load_status(&mut self, attempt: Self::Attempt) -> ForegroundLoadStatus<Self::Attempt> {
        match crate::route::route_start_status(attempt) {
            crate::route::RouteStartStatus::Pending => ForegroundLoadStatus::Pending,
            crate::route::RouteStartStatus::Started => ForegroundLoadStatus::Started,
            crate::route::RouteStartStatus::Failed => ForegroundLoadStatus::Failed,
            crate::route::RouteStartStatus::Superseded(replacement) => {
                ForegroundLoadStatus::Superseded(replacement)
            }
            crate::route::RouteStartStatus::Stale => ForegroundLoadStatus::Stale,
        }
    }

    fn after_load(
        &mut self,
        attempt: Option<Self::Attempt>,
        clock: ForegroundClock,
        started: bool,
    ) {
        if matches!(clock, ForegroundClock::Paused) {
            if started {
                set_resume_pend(true);
            } else {
                crate::player::TX.finish_seek_preroll();
            }
        }
        if !started && attempt.is_some() {
            // `sf_load == 0` publishes Error but intentionally leaves the Engine available for
            // diagnostics. Retire it only if it still owns the observed token: player::pump may
            // already have launched a healthy replacement before foreground polls this result.
            let _ = crate::player::suspend_bufferfeed_if_attempt(self.mt, attempt.unwrap());
        }
    }

    fn play_clock(&mut self) -> bool {
        // start_bufferfeed has installed the Initial native-clock hold from TX.paused. Publish
        // Play through the synchronized reducer so that hold and the feed gate reopen together.
        !paused() || set_transport_paused(self.mt, false)
    }
}

#[cfg(test)]
mod foreground_resume_tests {
    use super::*;

    #[derive(Default)]
    struct FakeActuator {
        prepare: Vec<crate::player::ResumeOutcome>,
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

        fn prepare_resume(&mut self, resume_ns: i64) -> crate::player::ResumeOutcome {
            self.prepare_calls.push(resume_ns);
            Self::answer(&mut self.prepare)
        }

        fn before_load(&mut self, resume_ns: i64, clock: ForegroundClock) {
            self.before_loads.push((resume_ns, clock));
        }

        fn start_load(&mut self) -> ForegroundLoadStart<Self::Attempt> {
            self.load_calls += 1;
            Self::answer(&mut self.loads)
        }

        fn load_status(&mut self, attempt: Self::Attempt) -> ForegroundLoadStatus<Self::Attempt> {
            self.status_calls.push(attempt);
            Self::answer(&mut self.statuses)
        }

        fn after_load(
            &mut self,
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
                drive_foreground(&mut lifecycle, ForegroundInput::PlayKey, &mut actuator),
                ForegroundActivation::Handled
            );
            assert_eq!(lifecycle.state, saved, "refusal {refused:?}");
            assert_eq!(actuator.load_calls, 0, "a rejected rebuild cannot Load");
        }
    }

    #[test]
    fn did_foreground_preserves_a_paused_snapshot_without_issuing_play() {
        let mut lifecycle = ForegroundLifecycle::IDLE;
        lifecycle.suspend(73_000_000_000, ForegroundClock::Paused);
        let mut actuator = FakeActuator {
            prepare: vec![crate::player::ResumeOutcome::Prepared],
            loads: vec![ForegroundLoadStart::Launched(11)],
            statuses: vec![ForegroundLoadStatus::Started],
            ..FakeActuator::default()
        };

        assert_eq!(
            drive_foreground(
                &mut lifecycle,
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
            poll_foreground_load(&mut lifecycle, &mut actuator),
            ForegroundPollOutcome::Started
        );
        assert_eq!(actuator.status_calls, vec![11]);
        assert_eq!(
            actuator.after_loads,
            vec![(Some(11), ForegroundClock::Paused, true)]
        );
        assert_eq!(lifecycle.state, ForegroundState::Idle);
    }

    #[test]
    fn pending_load_failure_retains_prepared_route_and_retry_skips_resume_preparation() {
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
            drive_foreground(
                &mut lifecycle,
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
            poll_foreground_load(&mut lifecycle, &mut actuator),
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
            poll_foreground_load(&mut lifecycle, &mut actuator),
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
            drive_foreground(&mut lifecycle, ForegroundInput::PlayKey, &mut actuator),
            ForegroundActivation::Launched
        );
        assert_eq!(actuator.prepare_calls.len(), 1, "prepared URL was rebuilt");
        assert_eq!(actuator.load_calls, 2);
        assert_eq!(
            actuator.clock_calls, 0,
            "retry launch was mistaken for Start"
        );
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut actuator),
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
            drive_foreground(
                &mut lifecycle,
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
        #[derive(Default)]
        struct EngineSpy {
            engine_live: bool,
            prepares: usize,
            loads: usize,
            teardowns: usize,
        }

        impl ForegroundActuator for EngineSpy {
            type Attempt = u64;

            fn prepare_resume(&mut self, _resume_ns: i64) -> crate::player::ResumeOutcome {
                self.prepares += 1;
                crate::player::ResumeOutcome::Prepared
            }

            fn before_load(&mut self, _resume_ns: i64, _clock: ForegroundClock) {}

            fn start_load(&mut self) -> ForegroundLoadStart<Self::Attempt> {
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
            drive_foreground(
                &mut lifecycle,
                ForegroundInput::DidForeground,
                &mut actuator
            ),
            ForegroundActivation::Launched
        );
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut actuator),
            ForegroundPollOutcome::Failed
        );
        assert_eq!(actuator.teardowns, 1);

        assert_eq!(
            drive_foreground(&mut lifecycle, ForegroundInput::PlayKey, &mut actuator),
            ForegroundActivation::Launched,
            "failed Engine survived and turned the retry into AlreadyRunning"
        );
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut actuator),
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
        let mut lifecycle = ForegroundLifecycle::IDLE;
        lifecycle.suspend(41_000_000_000, ForegroundClock::Playing);
        let mut actuator = FakeActuator {
            prepare: vec![crate::player::ResumeOutcome::Prepared],
            loads: vec![ForegroundLoadStart::Failed],
            ..FakeActuator::default()
        };

        assert_eq!(
            drive_foreground(
                &mut lifecycle,
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
                drive_foreground(
                    &mut lifecycle,
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
        let mut lifecycle = ForegroundLifecycle::IDLE;
        lifecycle.suspend(22_000_000_000, ForegroundClock::Paused);
        let mut actuator = FakeActuator {
            prepare: vec![crate::player::ResumeOutcome::Prepared],
            loads: vec![ForegroundLoadStart::Launched(7)],
            statuses: vec![ForegroundLoadStatus::Stale],
            ..FakeActuator::default()
        };

        assert_eq!(
            drive_foreground(
                &mut lifecycle,
                ForegroundInput::DidForeground,
                &mut actuator
            ),
            ForegroundActivation::Launched
        );
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut actuator),
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
            drive_foreground(
                &mut lifecycle,
                ForegroundInput::DidForeground,
                &mut actuator,
            ),
            ForegroundActivation::Launched,
        );
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut actuator),
            ForegroundPollOutcome::Pending,
        );
        assert!(matches!(
            lifecycle.state,
            ForegroundState::LoadPending { attempt: 8, .. }
        ));
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut actuator),
            ForegroundPollOutcome::Pending,
        );
        assert!(matches!(
            lifecycle.state,
            ForegroundState::LoadPending { attempt: 9, .. }
        ));
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut actuator),
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
            drive_foreground(&mut lifecycle, ForegroundInput::PlayKey, &mut actuator),
            ForegroundActivation::Launched,
            "spawning the Load only mounts the player while its result remains pending"
        );
        assert_eq!(actuator.clock_calls, 0);
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut actuator),
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
            drive_foreground(&mut lifecycle, ForegroundInput::PlayKey, &mut actuator),
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
        drive_foreground, paused, poll_foreground_load, set_transport_paused, ForegroundActivation,
        ForegroundActuator, ForegroundClock, ForegroundInput, ForegroundLifecycle,
        ForegroundLoadStart, ForegroundLoadStatus, ForegroundPollOutcome, ForegroundState,
    };
    use std::sync::atomic::Ordering;

    #[test]
    fn a_refused_native_pause_or_play_cannot_diverge_the_feed_gate() {
        let _guard = crate::testlock::serial();
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
        let mt = unsafe { crate::task::MainThread::assume() };

        crate::player::force_pause_result_for_test(Some(0));
        assert!(!set_transport_paused(&mt, true));
        assert!(
            !paused(),
            "feed must continue when the native clock refused Pause"
        );

        crate::player::force_pause_result_for_test(Some(1));
        assert!(set_transport_paused(&mt, true));
        assert!(paused());

        crate::player::force_play_result_for_test(Some(0));
        assert!(!set_transport_paused(&mt, false));
        assert!(
            paused(),
            "feed must remain stopped when the native clock refused Play"
        );

        crate::player::force_play_result_for_test(Some(1));
        assert!(set_transport_paused(&mt, false));
        assert!(!paused());
    }

    #[test]
    fn refused_foreground_play_retries_the_native_clock_without_a_second_load() {
        let _guard = crate::testlock::serial();
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
        let mt = unsafe { crate::task::MainThread::assume() };
        crate::player::force_pause_result_for_test(Some(1));
        assert!(set_transport_paused(&mt, true));

        struct Actuator<'a> {
            mt: &'a crate::task::MainThread,
            prepares: usize,
            loads: usize,
        }
        impl ForegroundActuator for Actuator<'_> {
            type Attempt = u64;

            fn prepare_resume(&mut self, _resume_ns: i64) -> crate::player::ResumeOutcome {
                self.prepares += 1;
                crate::player::ResumeOutcome::Prepared
            }

            fn before_load(&mut self, _resume_ns: i64, _clock: ForegroundClock) {}

            fn start_load(&mut self) -> ForegroundLoadStart<Self::Attempt> {
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
                _attempt: Option<Self::Attempt>,
                _clock: ForegroundClock,
                _started: bool,
            ) {
            }

            fn play_clock(&mut self) -> bool {
                set_transport_paused(self.mt, false)
            }
        }

        let mut lifecycle = ForegroundLifecycle::IDLE;
        lifecycle.suspend(42_000_000_000, ForegroundClock::Paused);
        let mut actuator = Actuator {
            mt: &mt,
            prepares: 0,
            loads: 0,
        };
        crate::player::force_play_result_for_test(Some(0));
        assert_eq!(
            drive_foreground(&mut lifecycle, ForegroundInput::PlayKey, &mut actuator,),
            ForegroundActivation::Launched,
        );
        assert!(matches!(
            lifecycle.state,
            ForegroundState::LoadPending { .. }
        ));
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut actuator),
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
            drive_foreground(&mut lifecycle, ForegroundInput::PlayKey, &mut actuator,),
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
