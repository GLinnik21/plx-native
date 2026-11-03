//! One worker's adaptive HLS owner. The feed supplies measured media and serialized
//! queue application; this module owns the controller and route transition.
#![deny(clippy::debug_assert_with_mut_call)]

use crate::{abr, route};
use plx_playback_contract::adaptive::PublicationGate;
use plx_playback_contract::adaptive::{
    Acquisition, ActiveAction, ActiveFeedPort, ActiveObservation, ActiveQuantum, AutomaticPort, AutomaticTransition, BufferFact, FeedAction, FeedFailure, FeedObservation, FeedPort, PauseFact, TrialClock,
    RoundId, TrialActuatorPort, TrialObservation, TrialPhase, TrialProtocol, TrialProtocolError,
};
use plx_playback_contract::SourceKey;

/// The route owner's captured inputs at a worker start. No second route identity is
/// minted here: the ticket remains the route's complete worker-generation fence.
pub struct Start {
    control: route::HlsAbrControl,
    ticket: route::WorkerTicket,
}

#[derive(Clone, Copy)]
pub(crate) struct StartInfo {
    pub(crate) initial: abr::Rung,
    pub(crate) prior: Option<abr::CapacityEstimate>,
    pub(crate) history: abr::TransitionHistory,
    pub(crate) original_candidate: bool,
    pub(crate) original_source_kbps: u32,
    pub(crate) original_refused: bool,
}

impl Start {
    pub(crate) fn info(&self) -> StartInfo {
        StartInfo {
            initial: self.control.initial_rung,
            prior: self.control.prior,
            history: self.control.history,
            original_candidate: self.control.has_original_candidate(),
            original_source_kbps: self.control.original_source_kbps(),
            original_refused: self.control.original_refused_on_this_route(),
        }
    }
}

impl From<(route::HlsAbrControl, route::WorkerTicket)> for Start {
    fn from((control, ticket): (route::HlsAbrControl, route::WorkerTicket)) -> Self {
        Self { control, ticket }
    }
}

/// Captured progressive Original policy and its exact worker ticket.
pub struct OriginalStart(route::AutoOriginalWatch);

impl From<route::AutoOriginalWatch> for OriginalStart {
    fn from(watch: route::AutoOriginalWatch) -> Self { Self(watch) }
}

impl OriginalStart {
    pub(crate) fn source_kbps(&self) -> u32 { self.0.source_kbps }

    pub(crate) fn start(self) -> OriginalCoordinator {
        let controller = abr::OriginalModeController::new(
            self.0.source_kbps, abr::AbrPolicy::measured(), self.0.catalog,
            self.0.history, self.0.features,
        );
        OriginalCoordinator { route: self.0, controller }
    }
}

/// Progressive Original policy. Seek changes the ticket and positional evidence
/// together; a superseded route can no longer publish a fallback.
pub(crate) struct OriginalCoordinator {
    route: route::AutoOriginalWatch,
    controller: Option<abr::OriginalModeController>,
}

pub(crate) enum OriginalFallbackResult { Published, Busy, RouteMoved }

pub(crate) enum ProbeResult {
    Measured(abr::CapacityObservation),
    Failed { outcome: &'static str, failure: ProbeFailure },
    Stale,
}

#[derive(Clone, Copy)]
pub(crate) enum ProbeFailure { HttpStatus(i32), Deadline, Transport, NoBody, Other }

impl From<route::OriginalProbeFailure> for ProbeFailure {
    fn from(failure: route::OriginalProbeFailure) -> Self {
        match failure {
            route::OriginalProbeFailure::HttpStatus(status) => Self::HttpStatus(status),
            route::OriginalProbeFailure::Deadline => Self::Deadline,
            route::OriginalProbeFailure::Transport => Self::Transport,
            route::OriginalProbeFailure::NoBody => Self::NoBody,
            route::OriginalProbeFailure::Other => Self::Other,
        }
    }
}

impl OriginalCoordinator {
    pub(crate) fn seek(&mut self, bytes: u64, active_us: u64) -> bool {
        if !self.route.refresh_ticket_after_seek() {
            self.controller = None;
            return false;
        }
        if let Some(controller) = self.controller.as_mut() {
            controller.on_seek(bytes, active_us);
        }
        true
    }

    pub(crate) fn on_resume(&mut self, elapsed_ms: u64) {
        if let Some(controller) = self.controller.as_mut() {
            controller.on_resume(elapsed_ms);
        }
    }

    pub(crate) fn observe(
        &mut self, bytes: u64, active_us: u64, buffered_ms: Option<i64>,
        remaining_ms: i64, now_ms: u64,
    ) -> Option<abr::OriginalObservation> {
        self.controller.as_mut()?.observe(
            bytes, active_us, buffered_ms, remaining_ms, now_ms,
        )
    }

    pub(crate) fn request_hls_fallback(
        &self, conservative_kbps: u32, position_ns: i64,
    ) -> OriginalFallbackResult {
        match self.route.request_hls_fallback(conservative_kbps, position_ns) {
            route::AutomaticIntentResult::Accepted => OriginalFallbackResult::Published,
            route::AutomaticIntentResult::Busy => OriginalFallbackResult::Busy,
            route::AutomaticIntentResult::Stale => OriginalFallbackResult::RouteMoved,
        }
    }
}

/// A typed refusal at the prime boundary. A server rung refusal is the only one
/// that certifies anything about the requested rung.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrimeFailure { Rung, Deadline, Control, Session }

pub(crate) fn classify_prime_failure(
    failure: PrimeFailure, deadline_final: bool,
) -> (abr::RejectCause, &'static str) {
    match failure {
        PrimeFailure::Rung => (abr::RejectCause::Structural, "prime_refused"),
        PrimeFailure::Deadline if !deadline_final =>
            (abr::RejectCause::Circumstance, "control_clock_moved"),
        PrimeFailure::Deadline => (abr::RejectCause::Censored, "control_deadline"),
        PrimeFailure::Control => (abr::RejectCause::Circumstance, "control_unresolved"),
        PrimeFailure::Session => (abr::RejectCause::Circumstance, "prime_session_moved"),
    }
}

/// One worker-local trial runner. It owns phase and address validation while the
/// feed holds the opaque cursor, timeline, and staged access units in its port.
pub(crate) struct TrialRunner<'a, P: FeedPort> {
    port: &'a mut P,
    protocol: &'a mut TrialProtocol,
}

impl<'a, P: FeedPort> TrialRunner<'a, P> {
    pub(crate) fn new(port: &'a mut P, protocol: &'a mut TrialProtocol) -> Self {
        Self { port, protocol }
    }

    pub(crate) fn physical(&mut self) -> &mut P { &mut *self.port }

    pub(crate) fn prime(
        &mut self, coordinator: &Coordinator, off: &plx_base::task::OffFrame,
        proposal: abr::Proposal, offset_micros: i64,
        deadline: Option<std::time::Instant>,
    ) -> Result<route::PrimedHls, PrimeFailure> {
        let primed = coordinator.prime(off, proposal, offset_micros, deadline)?;
        self.observe(TrialObservation::Prepared)
            .expect("a primed trial follows its grant");
        Ok(primed)
    }

    pub(crate) fn observe(
        &mut self, observation: TrialObservation,
    ) -> Result<TrialPhase, TrialProtocolError> {
        let trial = self.protocol.id();
        self.protocol.observe(trial, observation)
    }

    fn step(&mut self, action: FeedAction) -> FeedObservation {
        let observed = self.port.step(action);
        if observed.trial() == self.protocol.id() {
            observed
        } else {
            FeedObservation::Failed {
                trial: self.protocol.id(), failure: FeedFailure::Protocol,
            }
        }
    }

    pub(crate) fn aborted(&mut self) -> Result<bool, FeedFailure> {
        let trial = self.protocol.id();
        match self.step(FeedAction::CheckAbort { trial }) {
            FeedObservation::AbortStatus { aborted, .. } => Ok(aborted),
            _ => Err(FeedFailure::Protocol),
        }
    }

    pub(crate) fn expired(&mut self) -> Result<bool, FeedFailure> {
        let trial = self.protocol.id();
        match self.step(FeedAction::CheckExpired { trial }) {
            FeedObservation::ExpiryStatus { expired, .. } => Ok(expired),
            _ => Err(FeedFailure::Protocol),
        }
    }

    pub(crate) fn budget_snapshot(&mut self) -> Result<(Option<i64>, i64, Option<std::time::Duration>, bool), FeedFailure> {
        let trial = self.protocol.id();
        match self.step(FeedAction::BudgetSnapshot { trial }) {
            FeedObservation::BudgetSnapshot {
                buffered_ms, started_ns, remaining, rebuffering, ..
            } => Ok((buffered_ms, started_ns, remaining, rebuffering)),
            _ => Err(FeedFailure::Protocol),
        }
    }

    /// Bind the primed locator only in the physical source slot. The action
    /// itself carries an opaque key, never a URL or authentication material.
    pub(crate) fn open(&mut self, locator: P::Locator) -> FeedObservation {
        let trial = self.protocol.id();
        let source = SourceKey::new(trial.serial);
        self.port.bind_source(source, locator);
        self.step(FeedAction::Open { trial, source })
    }

    pub(crate) fn read_next(&mut self) -> FeedObservation {
        let trial = self.protocol.id();
        self.step(FeedAction::ReadNext { trial })
    }

    pub(crate) fn acquire_first(
        &mut self, budget: CandidateBudget, started_ns: i64,
    ) -> FeedObservation {
        let trial = self.protocol.id();
        self.step(FeedAction::AcquireFirst {
            trial, media_budget: budget.media_deadline,
            yields_to_rebuffer: budget.yields_to_rebuffer,
            started_ns, clock: budget.clock,
        })
    }

    pub(crate) fn acquire_followup(&mut self) -> FeedObservation {
        let trial = self.protocol.id();
        self.step(FeedAction::AcquireFollowup { trial })
    }

    pub(crate) fn stage_acquired(&mut self) -> FeedObservation {
        let trial = self.protocol.id();
        self.step(FeedAction::StageAcquired { trial })
    }

    pub(crate) fn first_action(
        &mut self, coordinator: &Coordinator, proposal: abr::Proposal,
        current: VariantFacts, candidate: VariantFacts,
        sample: Option<abr::SegmentSample>,
    ) -> FirstCandidateAction {
        let grade = coordinator.first_candidate_grade(proposal, current, candidate, sample);
        self.after_first_grade(grade)
    }

    /// The setup-bearing object is already staged. A second read is taken only
    /// after the common budget decision, and both physical objects stay private
    /// until the final grade authorizes queueing them together.
    pub(crate) fn grade_staged(
        &mut self, coordinator: &Coordinator, proposal: abr::Proposal,
        reserve_policy: abr::ReservePolicy, current: VariantFacts,
        first: Acquisition, first_buffer: BufferFact, opened_declared_bps: u64,
        mut on_followup: impl FnMut(Option<u64>),
        mut on_trace: impl FnMut(&mut P, TrialTraceMoment),
    ) -> StagedCandidate {
        let first_variant = VariantFacts {
            width: first.width, height: first.height, declared_bps: first.declared_bps,
        };
        let first_sample = candidate_sample(first, buffer_snapshot(first_buffer));
        let first_action = self.first_action(
            coordinator, proposal, current, first_variant, first_sample,
        );
        let mut verdict = match first_action {
            FirstCandidateAction::Finalize { verdict }
            | FirstCandidateAction::ReadFollowup { verdict, .. } => verdict,
            FirstCandidateAction::Unfunded => return StagedCandidate::Unfunded,
            FirstCandidateAction::Protocol => return StagedCandidate::Protocol(
                "adaptive feed follow-up reserve protocol violation",
            ),
        };
        let mut acquisitions = vec![first];
        let mut final_acquisition = first;
        let mut final_buffer = first_buffer;
        if let FirstCandidateAction::ReadFollowup { remaining_ms, .. } = first_action {
            let (acquisition, buffer) = match self.read_repeatable(
                remaining_ms, &mut on_followup, &mut on_trace,
            ) {
                Ok(evidence) => evidence,
                Err(outcome) => return outcome,
            };
            final_acquisition = acquisition;
            final_buffer = buffer;
            acquisitions.push(acquisition);
            verdict = self.followup_verdict(
                coordinator, proposal,
                candidate_sample(acquisition, buffer_snapshot(final_buffer)),
                opened_declared_bps,
            );
        }
        let candidate = VariantFacts {
            width: final_acquisition.width,
            height: final_acquisition.height,
            declared_bps: final_acquisition.declared_bps,
        };
        on_trace(self.physical(), TrialTraceMoment::FinalEvidence);
        let grade = self.final_action(
            coordinator, proposal, reserve_policy, current, candidate, &acquisitions, verdict,
        );
        StagedCandidate::Ready { final_acquisition, final_buffer, verdict, grade }
    }

    fn read_repeatable(
        &mut self, remaining_ms: Option<u64>,
        on_followup: &mut impl FnMut(Option<u64>),
        on_trace: &mut impl FnMut(&mut P, TrialTraceMoment),
    ) -> Result<(Acquisition, BufferFact), StagedCandidate> {
        on_followup(remaining_ms);
        self.observe(TrialObservation::FollowupRequested)
            .expect("a repeatable read follows its first staged object");
        let next = self.read_next();
        if !matches!(next, FeedObservation::SegmentReady { .. }) {
            return Err(StagedCandidate::Failed {
                step: TrialStep::FollowupRead,
                failure: match next {
                    FeedObservation::Failed { failure, .. } => failure,
                    _ => FeedFailure::Protocol,
                },
            });
        }
        let acquired = self.acquire_followup();
        let acquisition = match acquired {
            FeedObservation::Acquired { acquisition, .. } => acquisition,
            other => return Err(StagedCandidate::Failed {
                step: TrialStep::FollowupAcquire,
                failure: match other {
                    FeedObservation::Failed { failure, .. } => failure,
                    _ => FeedFailure::Protocol,
                },
            }),
        };
        on_trace(self.physical(), TrialTraceMoment::FollowupMedia);
        self.observe(TrialObservation::FollowupAcquired)
            .expect("the repeatable acquisition follows its request");
        match self.stage_acquired() {
            FeedObservation::Staged { buffer, count: 2, .. } => Ok((acquisition, buffer)),
            _ => Err(StagedCandidate::Protocol(
                "adaptive feed follow-up stage protocol violation",
            )),
        }
    }

    fn after_first_grade(&mut self, grade: FirstCandidateGrade) -> FirstCandidateAction {
        if !grade.needs_followup {
            return FirstCandidateAction::Finalize { verdict: grade.verdict };
        }
        let trial = self.protocol.id();
        match self.step(FeedAction::CheckFollowupReserve { trial }) {
            FeedObservation::FollowupReserve { funded: true, remaining_ms, .. } =>
                FirstCandidateAction::ReadFollowup {
                    verdict: grade.verdict, remaining_ms,
                },
            FeedObservation::FollowupReserve { funded: false, .. } =>
                FirstCandidateAction::Unfunded,
            _ => FirstCandidateAction::Protocol,
        }
    }

    pub(crate) fn followup_verdict(
        &self, coordinator: &Coordinator, proposal: abr::Proposal,
        sample: Option<abr::SegmentSample>, declared_bps: u64,
    ) -> abr::CandidateVerdict {
        sample.map_or(abr::CandidateVerdict::Incomplete, |sample| {
            coordinator.candidate_verdict(proposal, sample, declared_bps, false)
        })
    }

    pub(crate) fn final_action(
        &self, coordinator: &Coordinator, proposal: abr::Proposal,
        reserve_policy: abr::ReservePolicy, current: VariantFacts,
        candidate: VariantFacts, acquisitions: &[Acquisition],
        verdict: abr::CandidateVerdict,
    ) -> FinalCandidateGrade {
        coordinator.final_candidate_grade(
            proposal, reserve_policy, current, candidate, acquisitions, verdict,
        )
    }

    pub(crate) fn authorize_queue(&mut self) {
        self.observe(TrialObservation::QueueAuthorized)
            .expect("only graded candidate media may enter the queue");
    }

    pub(crate) fn queue_staged(&mut self) -> FeedObservation {
        self.observe(TrialObservation::QueueStarted)
            .expect("queueing follows candidate authorization");
        let trial = self.protocol.id();
        let observed = self.step(FeedAction::QueueStaged { trial });
        if observed == (FeedObservation::Queued { trial }) {
            self.observe(TrialObservation::QueueCompleted)
                .expect("all staged candidate media was queued");
        } else {
            let terminal = self.observe(TrialObservation::Ended);
            debug_assert_eq!(terminal, Err(TrialProtocolError::TerminalAfterQueue));
        }
        observed
    }
}

/// Project physical acquisition onto the controller's whole-millisecond reserve lattice.
pub(crate) fn candidate_sample(
    acquisition: Acquisition, buffer: abr::BufferSnapshot,
) -> Option<abr::SegmentSample> {
    let duration_ms = u32::try_from(acquisition.duration.as_millis()).ok()?;
    let obligation_ms = u32::try_from(
        acquisition.duration.as_nanos().checked_add(999_999)? / 1_000_000,
    ).ok()?;
    abr::SegmentSample::new_with_obligation(
        acquisition.bytes, acquisition.active_us, acquisition.total_us,
        duration_ms, obligation_ms, buffer,
    )
}

pub(crate) fn buffer_snapshot(fact: BufferFact) -> abr::BufferSnapshot {
    abr::BufferSnapshot {
        playback: abr::MediaTimeMs(fact.playback_ms),
        video_tail: abr::MediaTimeMs(fact.video_tail_ms),
        audio_tail: fact.audio_tail_ms.map(abr::MediaTimeMs),
        audio_expected: fact.audio_expected,
    }
}

pub(crate) fn active_sample(
    acquisition: Acquisition, buffer: BufferFact, abandoned: bool,
) -> Option<abr::SegmentSample> {
    let duration_ms = u32::try_from(acquisition.duration.as_millis()).ok()?;
    let obligation_ms = u32::try_from(
        acquisition.duration.as_nanos().checked_add(999_999)? / 1_000_000,
    ).ok()?;
    let buffer = buffer_snapshot(buffer);
    if abandoned {
        abr::SegmentSample::abandoned_acquisition(
            acquisition.bytes, acquisition.active_us, acquisition.total_us,
            duration_ms, obligation_ms, buffer,
        )
    } else {
        abr::SegmentSample::new_with_obligation(
            acquisition.bytes, acquisition.active_us, acquisition.total_us,
            duration_ms, obligation_ms, buffer,
        )
    }
}

/// The feed's existing abort mutex is the first serialization boundary. Its
/// implementation must run the callback while holding that mutex and do no IO.
fn inside_publication_gate<T>(
    gate: &mut dyn PublicationGate, transition: impl FnOnce() -> T,
) -> Option<T> {
    let mut transition = Some(transition);
    let mut result = None;
    let ran = gate.if_not_aborted(&mut || {
        result = Some(transition.take().expect("AQ publication callback runs once")());
    });
    if ran { result } else { None }
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    struct Gate { aborted: bool, calls: usize }
    impl PublicationGate for Gate {
        fn if_not_aborted(&mut self, transition: &mut dyn FnMut()) -> bool {
            if self.aborted { return false; }
            self.calls += 1;
            transition();
            true
        }
    }

    #[test]
    fn abort_wins_before_any_route_or_controller_publication() {
        let mut gate = Gate { aborted: true, calls: 0 };
        let mut active = 7;
        assert_eq!(inside_publication_gate(&mut gate, || { active = 8; active }), None);
        assert_eq!((active, gate.calls), (7, 0));
        gate.aborted = false;
        assert_eq!(inside_publication_gate(&mut gate, || { active = 8; active }), Some(8));
        assert_eq!((active, gate.calls), (8, 1));
    }
}


#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CandidatePublication {
    Aborted,
    SessionEnded,
    RouteMoved,
    ControllerRejected,
    Active { previous: String },
}

/// One AQ -> ACTIVE -> controller publication. A grant or a primed encoder is
/// not active: only this successful callback changes route/controller ownership.
pub(crate) fn publish_candidate(
    port: &mut dyn PublicationGate,
    control: &route::HlsAbrControl,
    controller: &mut abr::Controller,
    ticket: &mut route::WorkerTicket,
    candidate: &route::PrimedHls,
    proposal: abr::Proposal,
    sample: abr::SegmentSample,
    variant: abr::ObservedHlsVariant,
    now_ms: u64,
) -> CandidatePublication {
    let expected = ticket.clone();
    inside_publication_gate(port, || {
        match control.commit_transition(
            &expected, candidate, proposal, (variant, sample.network_kbps()),
            || controller.commit_candidate(proposal, sample, variant, now_ms)
                .then(|| expected.encoder().to_owned()),
        ) {
            Ok((previous, next)) => {
                *ticket = next;
                CandidatePublication::Active { previous }
            }
            Err(route::HlsCommitRefusal::Session) => CandidatePublication::SessionEnded,
            Err(route::HlsCommitRefusal::RouteMoved) => CandidatePublication::RouteMoved,
            Err(route::HlsCommitRefusal::TransitionRejected) => {
                CandidatePublication::ControllerRejected
            }
        }
    }).unwrap_or(CandidatePublication::Aborted)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OriginalPublication { Published, Busy, Aborted, RouteMoved, ClockMoved }

/// Publish an Original request at the same AQ -> ACTIVE boundary. The caller's
/// clock lease must have authorized this transition before entering the AQ lock.
pub(crate) fn publish_original(
    port: &mut dyn PublicationGate,
    authorized: bool,
    control: &route::HlsAbrControl,
    ticket: &route::WorkerTicket,
    recover_kbps: u32,
    position_ns: impl FnOnce() -> i64,
    on_published: impl FnOnce(),
) -> OriginalPublication {
    if !authorized { return OriginalPublication::ClockMoved; }
    inside_publication_gate(port, || match control.request_original_recovery(
        ticket, recover_kbps, position_ns(),
    ) {
        route::AutomaticIntentResult::Accepted => {
            on_published();
            OriginalPublication::Published
        }
        route::AutomaticIntentResult::Busy => OriginalPublication::Busy,
        route::AutomaticIntentResult::Stale => OriginalPublication::RouteMoved,
    }).unwrap_or(OriginalPublication::Aborted)
}

pub(crate) fn prime(
    control: &route::HlsAbrControl,
    ticket: &route::WorkerTicket,
    off: &plx_base::task::OffFrame,
    proposal: abr::Proposal,
    offset_micros: i64,
    deadline: Option<std::time::Instant>,
) -> Result<route::PrimedHls, PrimeFailure> {
    control.prime(off, ticket, proposal, offset_micros, deadline)
        .map_err(|failure| match failure {
            route::PrimeRefusal::Rung => PrimeFailure::Rung,
            route::PrimeRefusal::Deadline => PrimeFailure::Deadline,
            route::PrimeRefusal::Control => PrimeFailure::Control,
            route::PrimeRefusal::Session => PrimeFailure::Session,
        })
}

/// One controller and its physical route owner. Rebuilt on seek/reload from the
/// route's captured prior and history; pending reserve never crosses that boundary.
pub(crate) struct Coordinator {
    control: route::HlsAbrControl,
    ticket: route::WorkerTicket,
    controller: abr::Controller,
    recovery: Option<abr::OriginalRecovery>,
    recover_kbps: u32,
    next_trial_serial: u64,
    next_round_serial: u64,
}

pub(crate) struct RoundDecision {
    pub(crate) decision: abr::Decision,
    pub(crate) telemetry: abr::ControllerTelemetry,
    pub(crate) prime_runway_ms: Option<i64>,
}

pub(crate) struct ActiveAfterPrefetch {
    pub(crate) reconsidered: Option<Reconsideration>,
    pub(crate) steady: bool,
    pub(crate) source: LatchedOriginalRecovery,
}

pub(crate) trait ActiveDiagnostics {
    fn cleanup(&mut self, ready: bool);
    fn accepted_pause(&mut self, after_lookup: bool, sequence: u64, elapsed_ms: u64);
    fn sample(&mut self, sample: abr::SegmentSample);
    fn fresh_boundary(&mut self);
    fn observed(&mut self, round: &RoundDecision, sample: abr::SegmentSample);
}

pub(crate) enum ActiveRunError<E> { Physical(E), Protocol(&'static str) }

pub(crate) enum ActiveRun {
    Ended,
    InvalidSample,
    Observed(ActiveCycle),
}

pub(crate) struct ActiveCycle {
    pub(crate) sample: abr::SegmentSample,
    pub(crate) round: RoundDecision,
    pub(crate) rollback_media_ms: Option<u32>,
    pub(crate) remaining_ms: i64,
    pub(crate) paused: bool,
    pub(crate) expected_user_sequence: u64,
    pub(crate) cleanup_ready: bool,
    pub(crate) after_prefetch: ActiveAfterPrefetch,
}

fn active_step<P: ActiveFeedPort>(
    port: &mut P, round: RoundId, action: ActiveAction,
) -> Result<ActiveObservation, ActiveRunError<P::Error>> {
    let observed = port.step_active(action).map_err(ActiveRunError::Physical)?;
    if observed.round() != round {
        return Err(ActiveRunError::Protocol("adaptive active reply address mismatch"));
    }
    Ok(observed)
}

/// One active acquisition and the controller observation in their original
/// physical order. The port may block on a queue or playlist; no controller or
/// route lock spans a port call. The two buffer samples bracket the rollback
/// lookup, and both accepted-pause samples precede the policy decision.
pub(crate) fn run_active_round<P: ActiveFeedPort>(
    coordinator: &mut Coordinator, port: &mut P, round: RoundId,
    now_ms: impl Fn() -> u64, remaining_ms: impl Fn() -> i64,
    user_sequence: impl Fn() -> u64, session_boundary_pending: &mut bool,
    diagnostics: &mut impl ActiveDiagnostics,
) -> Result<ActiveRun, ActiveRunError<P::Error>> {
    let read = active_step(port, round, ActiveAction::Read {
        round, at_floor: Some(coordinator.at_floor()),
    })?;
    let (acquisition, delivered, abandoned) = match read {
        ActiveObservation::Ready { acquisition, delivered, abandoned, .. } =>
            (acquisition, delivered, abandoned),
        ActiveObservation::Ended { .. } => return Ok(ActiveRun::Ended),
        _ => return Err(ActiveRunError::Protocol("adaptive active read protocol violation")),
    };
    if active_step(port, round, ActiveAction::Feed { round })?
        != (ActiveObservation::Fed { round }) {
        return Err(ActiveRunError::Protocol("adaptive active feed protocol violation"));
    }
    let active_now_ms = now_ms();
    let first_buffer = match active_step(port, round, ActiveAction::Buffer { round })? {
        ActiveObservation::Buffer { fact, .. } => fact,
        _ => return Err(ActiveRunError::Protocol("adaptive active buffer protocol violation")),
    };
    let Some(active) = coordinator.begin_active_quantum(
        acquisition, || first_buffer, abandoned, delivered, active_now_ms,
    ) else { return Ok(ActiveRun::InvalidSample); };
    let cleanup_ready = active.cleanup_ready;
    diagnostics.cleanup(cleanup_ready);
    let before = match active_step(port, round, ActiveAction::Pause { round })? {
        ActiveObservation::Pause { paused, sequence, completed_ms, .. } =>
            (paused, sequence, completed_ms),
        _ => return Err(ActiveRunError::Protocol("adaptive active pause protocol violation")),
    };
    if let Some(elapsed_ms) = before.2 {
        coordinator.on_resume(elapsed_ms);
        diagnostics.accepted_pause(false, before.1, elapsed_ms);
    }
    let rollback_media_ms = if before.0 { None } else {
        match active_step(port, round, ActiveAction::RollbackDuration { round })? {
            ActiveObservation::RollbackDuration { duration_ms, .. } => duration_ms,
            _ => return Err(ActiveRunError::Protocol("adaptive rollback protocol violation")),
        }
    };
    let refreshed_buffer = match active_step(port, round, ActiveAction::Buffer { round })? {
        ActiveObservation::Buffer { fact, .. } => fact,
        _ => return Err(ActiveRunError::Protocol("adaptive refreshed buffer protocol violation")),
    };
    let active = active.refresh(refreshed_buffer);
    let after = match active_step(port, round, ActiveAction::Pause { round })? {
        ActiveObservation::Pause { paused, sequence, completed_ms, .. } =>
            (paused, sequence, completed_ms),
        _ => return Err(ActiveRunError::Protocol("adaptive refreshed pause protocol violation")),
    };
    if let Some(elapsed_ms) = after.2 {
        coordinator.on_resume(elapsed_ms);
        diagnostics.accepted_pause(true, after.1, elapsed_ms);
    }
    let expected_user_sequence = user_sequence();
    diagnostics.sample(active.sample());
    let remaining_ms = remaining_ms();
    let boundary = *session_boundary_pending && delivered && active.sample().completed();
    if boundary {
        *session_boundary_pending = false;
        diagnostics.fresh_boundary();
    }
    let (sample, round_decision, prefetch_allowed) =
        active.observe(coordinator, rollback_media_ms, now_ms(), boundary);
    diagnostics.observed(&round_decision, sample);
    if prefetch_allowed && active_step(port, round, ActiveAction::Prefetch {
        round, at_floor: Some(coordinator.at_floor()),
    })? != (ActiveObservation::Prefetched { round }) {
        return Err(ActiveRunError::Protocol("adaptive active prefetch protocol violation"));
    }
    let after_prefetch = coordinator.after_active_prefetch(
        &round_decision, sample, delivered, remaining_ms, after.0, cleanup_ready,
    );
    Ok(ActiveRun::Observed(ActiveCycle {
        sample, round: round_decision, rollback_media_ms,
        remaining_ms, paused: after.0, expected_user_sequence,
        cleanup_ready, after_prefetch,
    }))
}

pub(crate) struct PreparedOriginalSwitch {
    pub(crate) kbps: u32,
    pub(crate) cancelled: Option<(abr::Proposal, bool)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OriginalSwitchOutcome {
    NoSwitch,
    LeaseBusy,
    Published { kbps: u32 },
    Busy,
    ClockMoved,
    Aborted,
    RouteMoved,
}

pub(crate) struct ActiveQuantumResult {
    sample: abr::SegmentSample,
    pub(crate) cleanup_ready: bool,
    delivered: bool,
    abandoned: bool,
}

pub(crate) struct RefreshedQuantum {
    sample: abr::SegmentSample,
    delivered: bool,
    abandoned: bool,
}

impl ActiveQuantumResult {
    /// The rollback playlist may have blocked. It supplies a second buffer
    /// fact after that wait without changing this acquisition's transfer.
    pub(crate) fn refresh(self, buffer: BufferFact) -> RefreshedQuantum {
        RefreshedQuantum {
            sample: self.sample.with_buffer(buffer_snapshot(buffer)),
            delivered: self.delivered,
            abandoned: self.abandoned,
        }
    }
}

impl RefreshedQuantum {
    pub(crate) fn sample(&self) -> abr::SegmentSample { self.sample }

    pub(crate) fn observe(
        self, coordinator: &mut Coordinator, rollback_media_ms: Option<u32>,
        now_ms: u64, boundary: bool,
    ) -> (abr::SegmentSample, RoundDecision, bool) {
        let round = coordinator.observe(
            self.sample, rollback_media_ms, now_ms, boundary,
        );
        let prefetch = self.delivered && !self.abandoned
            && matches!(round.decision, abr::Decision::Stay);
        (self.sample, round, prefetch)
    }
}

pub(crate) struct Reconsideration {
    pub(crate) verdict: abr::RecoveryVerdict,
    pub(crate) measured_kbps: u32,
    pub(crate) comparison: Option<abr::ModeComparison>,
}

pub(crate) struct ProbeDecision {
    result: Result<abr::ProbePermit, abr::ProbeBlock>,
    current_wire_kbps: u32,
    conservative_kbps: u32,
}

pub(crate) struct ProbeBlockSummary {
    pub(crate) reason: abr::ProbeBlock,
    pub(crate) current_wire_kbps: u32,
    pub(crate) conservative_kbps: u32,
}

pub(crate) enum RoundAction {
    Stay,
    ProbeBlocked(ProbeBlockSummary),
    ProbeDeferredForCleanup,
    Probe(abr::ProbePermit),
    TrialDeferred {
        proposal: abr::Proposal,
        refreshing_same_request: bool,
        reason: TrialDeferral,
    },
    Trial { proposal: abr::Proposal, reserve_policy: abr::ReservePolicy },
}

pub(crate) enum TrialDeferral { EncoderCleanup, PlaybackClock }

pub(crate) struct ProbeGrade {
    pub(crate) verdict: abr::RecoveryVerdict,
    pub(crate) comparison: Option<abr::ModeComparison>,
    pub(crate) basis: (u32, u32, u32, u32, u32),
    pub(crate) probes: u8,
}

pub(crate) enum ProbeRecoveryAction {
    Steady,
    Held,
    Retained,
    Switch { kbps: u32, measured_kbps: u32 },
}

#[derive(Clone, Copy)]
pub(crate) enum ProbeTraceAction { Start, Failed, Steady, RecoverOriginal }

pub(crate) trait OriginalProbeDiagnostics {
    fn action(&mut self, action: ProbeTraceAction, target_kbps: u32);
    fn failure(&mut self, failure: ProbeFailure);
    fn log(&mut self, line: &str);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OriginalProbeOutcome { LeaseBusy, Stale, Continue, Published, Aborted }

pub(crate) fn mode_comparison_line(cmp: abr::ModeComparison) -> String {
    let loser = cmp.loser.unwrap_or_default();
    format!(
        "abr: mode chose={:?} why={:?} vs_hls={}kbps scale={}pm \
         win[q={} f={} r={} s={} t={} tot={}] \
         lose[q={} f={} r={} s={} t={} tot={}]",
        cmp.chosen, cmp.reason, cmp.hls_rung.kbps(), cmp.scale_pm,
        cmp.winner.quality, cmp.winner.features, cmp.winner.risk,
        cmp.winner.server, cmp.winner.transition, cmp.winner.total,
        loser.quality, loser.features, loser.risk, loser.server,
        loser.transition, loser.total,
    )
}

#[derive(Clone, Copy)]
pub(crate) struct VariantFacts {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) declared_bps: u64,
}

struct FirstCandidateGrade {
    verdict: abr::CandidateVerdict,
    needs_followup: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum FirstCandidateAction {
    Finalize { verdict: abr::CandidateVerdict },
    ReadFollowup { verdict: abr::CandidateVerdict, remaining_ms: Option<u64> },
    Unfunded,
    Protocol,
}

pub(crate) enum StagedCandidate {
    Ready {
        final_acquisition: Acquisition,
        final_buffer: BufferFact,
        verdict: abr::CandidateVerdict,
        grade: FinalCandidateGrade,
    },
    Unfunded,
    Failed { step: TrialStep, failure: FeedFailure },
    Protocol(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrialTraceMoment { FollowupMedia, FinalEvidence }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrialStep {
    Open, FirstRead, FirstAcquire, FollowupRead, FollowupAcquire,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrialRejectReason { SourceMoved, Ended, Deadline, Other }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrialFailureAction {
    Abort,
    Protocol,
    TimelineOverflow,
    Reject { cause: abr::RejectCause, reason: TrialRejectReason },
}

/// A physical reply does not decide whether a rung failed. This policy table
/// distinguishes terminal abort/protocol faults from circumstance or censored
/// evidence, including the stage where EOF and origin changes have meaning.
pub(crate) fn trial_failure_action(
    step: TrialStep, failure: FeedFailure,
) -> TrialFailureAction {
    use FeedFailure as F;
    use TrialFailureAction as A;
    use TrialRejectReason as R;
    use TrialStep as S;
    match (step, failure) {
        (_, F::Aborted) | (S::FirstRead | S::FollowupRead, F::SourceMoved) => A::Abort,
        (S::FirstAcquire | S::FollowupAcquire, F::TimelineOverflow) => A::TimelineOverflow,
        (_, F::Protocol) | (_, F::TimelineOverflow)
        | (S::FirstAcquire | S::FollowupAcquire, F::SourceMoved | F::Ended)
        | (S::Open, F::Ended) => A::Protocol,
        (S::Open, F::SourceMoved) => A::Reject {
            cause: abr::RejectCause::Circumstance, reason: R::SourceMoved,
        },
        (S::FirstRead | S::FollowupRead, F::Ended) => A::Reject {
            cause: abr::RejectCause::Circumstance, reason: R::Ended,
        },
        (_, F::Deadline) => A::Reject {
            cause: abr::RejectCause::Censored, reason: R::Deadline,
        },
        (_, F::NotReady | F::Parse) => A::Reject {
            cause: abr::RejectCause::Circumstance, reason: R::Other,
        },
    }
}

pub(crate) struct FinalCandidateGrade {
    pub(crate) raster_ready: bool,
    pub(crate) quality_improves: bool,
    pub(crate) disposition: HlsCandidateDisposition,
}

pub(crate) struct CandidateBudget {
    pub(crate) media_deadline: Option<std::time::Duration>,
    pub(crate) yields_to_rebuffer: bool,
    pub(crate) clock: TrialClock,
}

pub(crate) struct ExplorationGrant {
    pub(crate) buffered_ms: i64,
    pub(crate) runway_ms: i64,
    pub(crate) budget_ms: i64,
}

pub(crate) enum PreprimePlan {
    Recovery,
    Exploration { grant: ExplorationGrant, started_ns: i64 },
}

pub(crate) struct ExplorationClock {
    pub(crate) buffered_ms: i64,
    pub(crate) runway_ms: i64,
    pub(crate) budget_ms: i64,
    pub(crate) started_ns: i64,
}

pub(crate) struct CandidateStart<L, R> {
    pub(crate) automatic_lease: Option<L>,
    pub(crate) trial_reserve: Option<R>,
    pub(crate) token: u64,
    pub(crate) exploration: Option<ExplorationClock>,
}

pub(crate) enum CandidateStartRefusal {
    ReserveUnknown,
    NoRunway,
    NoSurplus,
    BudgetMoved { budget_ms: i64, target_kbps: u32 },
    LeaseMoved,
    TrialReserveMoved,
}

pub(crate) enum ExplorationFailure {
    ReserveUnknown { refreshing_same_request: bool },
    NoRunway { refreshing_same_request: bool },
    NoSurplus { refreshing_same_request: bool },
    BudgetMoved { budget_ms: i64 },
}

impl ExplorationFailure {
    pub(crate) fn refreshing_same_request(&self) -> Option<bool> {
        match self {
            Self::ReserveUnknown { refreshing_same_request }
            | Self::NoRunway { refreshing_same_request }
            | Self::NoSurplus { refreshing_same_request } => Some(*refreshing_same_request),
            Self::BudgetMoved { .. } => None,
        }
    }
}

impl Coordinator {
    pub(crate) fn start(start: Start, pin: Option<abr::Rung>) -> Self {
        let Start { control, ticket } = start;
        let recovery = control.can_recover_original().then(|| {
            abr::OriginalRecovery::new(
                control.original_source_kbps(),
                abr::AbrPolicy::measured(),
                control.original_features,
                control.history.advanced_by(0),
                control.catalog,
            )
        }).flatten().map(|gate| {
            if control.original_refused_on_this_route() {
                gate.after_route_refusal()
            } else {
                gate
            }
        });
        let mut controller = abr::Controller::starting_at(
            control.initial_rung, control.prior, control.catalog,
        ).pinned_to(pin);
        if let Some((variant, evidence_kbps)) = control.initial_observed {
            controller.observe_active_variant(variant, evidence_kbps);
        }
        Self {
            control, ticket, controller, recovery, recover_kbps: 0,
            next_trial_serial: 0, next_round_serial: 0,
        }
    }

    pub(crate) fn telemetry(&self) -> abr::ControllerTelemetry {
        self.controller.telemetry()
    }

    pub(crate) fn has_recovery(&self) -> bool { self.recovery.is_some() }

    pub(crate) fn at_floor(&self) -> bool { self.controller.current().at_floor() }

    pub(crate) fn current(&self) -> abr::Rung { self.controller.current() }
    pub(crate) fn delivery(&self) -> abr::CapacityEstimate { self.controller.delivery() }
    pub(crate) fn prime_runway_ms(&self) -> Option<i64> { self.controller.prime_runway_ms() }
    fn candidate_wire_kbps(&self, rung: abr::Rung) -> u32 {
        self.controller.catalog().candidate(rung).expected_wire_kbps
    }
    pub(crate) fn pending_reserve_policy(&self, proposal: abr::Proposal) -> abr::ReservePolicy {
        self.controller.pending_reserve_policy(proposal)
            .expect("a Prime decision owns one pending reserve policy")
    }
    pub(crate) fn candidate_budget_from_snapshot(
        &self, proposal: abr::Proposal, reserve_policy: abr::ReservePolicy,
        candidate_duration: std::time::Duration, buffered_ms: i64,
        remaining: Option<std::time::Duration>, rebuffering: bool,
    ) -> CandidateBudget {
        let reserve = remaining.unwrap_or_else(|| abr::reserve_as_budget(buffered_ms));
        self.candidate_budget(
            proposal, reserve_policy, candidate_duration, reserve, rebuffering,
        )
    }
    pub(crate) fn begin_candidate<P: TrialActuatorPort>(
        &mut self, proposal: abr::Proposal, port: &mut P,
        expected_user_sequence: u64,
        buffer: impl FnOnce() -> (Option<i64>, i64),
        now_ms: impl Fn() -> u64,
        mut on_rejected: impl FnMut(abr::Proposal, bool),
    ) -> Result<CandidateStart<P::Lease, P::TrialReserve>, CandidateStartRefusal> {
        let plan = match self.preprime_plan(proposal, buffer, || now_ms()) {
            Ok(plan) => plan,
            Err(failure) => {
                if let Some(refreshing) = failure.refreshing_same_request() {
                    on_rejected(proposal, refreshing);
                }
                return Err(match failure {
                    ExplorationFailure::ReserveUnknown { .. } => CandidateStartRefusal::ReserveUnknown,
                    ExplorationFailure::NoRunway { .. } => CandidateStartRefusal::NoRunway,
                    ExplorationFailure::NoSurplus { .. } => CandidateStartRefusal::NoSurplus,
                    ExplorationFailure::BudgetMoved { budget_ms } => CandidateStartRefusal::BudgetMoved {
                        budget_ms, target_kbps: proposal.rung.kbps(),
                    },
                });
            }
        };
        match plan {
            PreprimePlan::Recovery => {
                let Some(lease) = port.begin(
                    AutomaticTransition::QualityDown, expected_user_sequence,
                ) else {
                    let refreshing = self.reject(proposal, abr::RejectCause::Circumstance, now_ms());
                    on_rejected(proposal, refreshing);
                    return Err(CandidateStartRefusal::LeaseMoved);
                };
                let token = port.lease_token(&lease);
                Ok(CandidateStart {
                    automatic_lease: Some(lease), trial_reserve: None,
                    token, exploration: None,
                })
            }
            PreprimePlan::Exploration { grant, started_ns } => {
                let Some(reserve) = port.arm_trial_reserve(
                    grant.runway_ms, expected_user_sequence,
                ) else {
                    let refreshing = self.reject(proposal, abr::RejectCause::Circumstance, now_ms());
                    on_rejected(proposal, refreshing);
                    return Err(CandidateStartRefusal::TrialReserveMoved);
                };
                let token = port.trial_token(&reserve);
                Ok(CandidateStart {
                    automatic_lease: None, trial_reserve: Some(reserve), token,
                    exploration: Some(ExplorationClock {
                        buffered_ms: grant.buffered_ms,
                        runway_ms: grant.runway_ms,
                        budget_ms: grant.budget_ms,
                        started_ns,
                    }),
                })
            }
        }
    }
    pub(crate) fn next_round_id(&mut self) -> Option<RoundId> {
        self.next_round_serial = self.next_round_serial.checked_add(1)?;
        Some(RoundId::new(self.ticket.adaptive_attempt(), self.next_round_serial))
    }
    pub(crate) fn begin_trial_protocol(
        &mut self,
    ) -> Option<plx_playback_contract::adaptive::TrialProtocol> {
        self.next_trial_serial = self.next_trial_serial.checked_add(1)?;
        let id = plx_playback_contract::adaptive::TrialId::new(
            self.ticket.adaptive_attempt(), self.next_trial_serial,
        );
        Some(plx_playback_contract::adaptive::TrialProtocol::new(id))
    }
    fn grant_exploration(
        &mut self, proposal: abr::Proposal, buffered_ms: Option<i64>, now_ms: u64,
    ) -> Result<ExplorationGrant, ExplorationFailure> {
        let Some(buffered_ms) = buffered_ms else {
            let refreshing_same_request = self.reject(
                proposal, abr::RejectCause::Circumstance, now_ms,
            );
            return Err(ExplorationFailure::ReserveUnknown { refreshing_same_request });
        };
        let Some(runway_ms) = self.controller.prime_runway_ms() else {
            let refreshing_same_request = self.reject(
                proposal, abr::RejectCause::Circumstance, now_ms,
            );
            return Err(ExplorationFailure::NoRunway { refreshing_same_request });
        };
        let Some(budget_ms) = self.controller.exploration_budget_ms(buffered_ms) else {
            let refreshing_same_request = self.reject(
                proposal, abr::RejectCause::Circumstance, now_ms,
            );
            return Err(ExplorationFailure::NoSurplus { refreshing_same_request });
        };
        if !self.controller.set_executed_exploration_budget(proposal, budget_ms) {
            return Err(ExplorationFailure::BudgetMoved { budget_ms });
        }
        Ok(ExplorationGrant { buffered_ms, runway_ms, budget_ms })
    }
    pub(crate) fn preprime_plan(
        &mut self, proposal: abr::Proposal,
        buffer: impl FnOnce() -> (Option<i64>, i64),
        now_ms: impl FnOnce() -> u64,
    ) -> Result<PreprimePlan, ExplorationFailure> {
        match proposal.direction {
            abr::Direction::Down => Ok(PreprimePlan::Recovery),
            abr::Direction::Up => {
                let (buffered_ms, started_ns) = buffer();
                let grant = self.grant_exploration(proposal, buffered_ms, now_ms())?;
                Ok(PreprimePlan::Exploration { grant, started_ns })
            }
        }
    }
    pub(crate) fn reject(&mut self, proposal: abr::Proposal, cause: abr::RejectCause, now_ms: u64) -> bool {
        let refreshing = proposal.rung == self.controller.current();
        self.controller.reject(proposal, cause, now_ms);
        refreshing
    }
    fn candidate_verdict(
        &self, proposal: abr::Proposal, sample: abr::SegmentSample,
        declared_bps: u64, boundary: bool,
    ) -> abr::CandidateVerdict {
        if boundary {
            self.controller.candidate_boundary_verdict(proposal, sample, declared_bps)
        } else {
            self.controller.candidate_verdict(proposal, sample, declared_bps)
        }
    }
    fn first_candidate_grade(
        &self, proposal: abr::Proposal, current: VariantFacts,
        candidate: VariantFacts, sample: Option<abr::SegmentSample>,
    ) -> FirstCandidateGrade {
        let raster_ready = hls_raster_within(candidate.width, candidate.height, proposal.rung);
        let quality_improves = proposal.direction != abr::Direction::Up
            || hls_upshift_strictly_improves(
                current.width, current.height, current.declared_bps,
                candidate.width, candidate.height, candidate.declared_bps,
            );
        let verdict = sample.map_or(abr::CandidateVerdict::Incomplete, |sample| {
            self.candidate_verdict(
                proposal, sample, candidate.declared_bps,
                proposal.direction == abr::Direction::Up,
            )
        });
        FirstCandidateGrade {
            verdict,
            needs_followup: raster_ready && quality_improves
                && verdict == abr::CandidateVerdict::SetupBearing,
        }
    }
    fn final_candidate_grade(
        &self, proposal: abr::Proposal, reserve_policy: abr::ReservePolicy,
        current: VariantFacts, candidate: VariantFacts,
        acquisitions: &[Acquisition], verdict: abr::CandidateVerdict,
    ) -> FinalCandidateGrade {
        let outputs_inside_rung = acquisitions.iter().all(|output| {
            hls_raster_within(output.width, output.height, proposal.rung)
        });
        let raster_ready = !hls_candidate_requires_rung_box(proposal, reserve_policy)
            || outputs_inside_rung;
        let quality_improves = proposal.direction != abr::Direction::Up
            || hls_upshift_strictly_improves(
                current.width, current.height, current.declared_bps,
                candidate.width, candidate.height, candidate.declared_bps,
            );
        FinalCandidateGrade {
            raster_ready, quality_improves,
            disposition: hls_candidate_disposition(raster_ready, quality_improves, verdict),
        }
    }
    pub(crate) fn candidate_budget(
        &self, proposal: abr::Proposal, reserve_policy: abr::ReservePolicy,
        duration: std::time::Duration, reserve: std::time::Duration,
        internally_held: bool,
    ) -> CandidateBudget {
        let delivery = self.controller.delivery();
        let predicted = abr::predicted_transfer(
            self.candidate_wire_kbps(proposal.rung), duration,
            delivery.conservative_kbps(), delivery.uncertainty_pm,
        );
        let warmup = abr::candidate_warmup_budget(
            proposal, duration, reserve, predicted,
            std::time::Duration::from_micros(self.controller.worst_overhead_us()),
        );
        CandidateBudget {
            media_deadline: abr::candidate_media_reserve_deadline(
                proposal, reserve_policy, internally_held, warmup,
            ),
            yields_to_rebuffer: proposal.direction == abr::Direction::Down
                && proposal.rung.at_floor(),
            clock: match proposal.direction {
                abr::Direction::Up => TrialClock::Playhead,
                abr::Direction::Down => TrialClock::UnpausedElapsed,
            },
        }
    }
    /// A setup-bearing first object earns one ordinary continuation only while
    /// the original trial clock still funds it. An exhausted clock is censored
    /// budget evidence; the physical reader supplies that clock verdict.
    pub(crate) fn fund_followup(
        &mut self, proposal: abr::Proposal, funded: bool, now_ms: u64,
    ) -> Result<(), bool> {
        if funded { return Ok(()); }
        let refreshing_same_request = self.reject(
            proposal, abr::RejectCause::Censored, now_ms,
        );
        Err(refreshing_same_request)
    }
    pub(crate) fn abandon(&self, encoder: &str) { self.control.abandon(encoder); }
    pub(crate) fn retire(&self, encoder: String) { self.control.retire(encoder); }
    pub(crate) fn trace_generation(&self) -> u32 { self.control.trace_generation() }
    pub(crate) fn original_source_kbps(&self) -> u32 { self.control.original_source_kbps() }
    pub(crate) fn retain_original_recovery(&mut self, kbps: u32) { self.recover_kbps = kbps; }
    pub(crate) fn on_hls_commit(&mut self, direction: abr::Direction) {
        if let Some(gate) = self.recovery.as_mut() { gate.on_hls_commit(direction); }
        self.recover_kbps = 0;
    }
    fn reconsider_original(
        &mut self, telemetry: &abr::ControllerTelemetry, remaining_ms: i64,
    ) -> Option<Reconsideration> {
        let current = self.controller.catalog().candidate(self.controller.current());
        let gate = self.recovery.as_mut()?;
        let result = gate.reconsider_after_hls_commit(
            current, &telemetry.production, telemetry.buffer, &telemetry.delivery, remaining_ms,
        )?;
        self.recover_kbps = if result.verdict == abr::RecoveryVerdict::Recover {
            result.measured_kbps.max(1)
        } else { 0 };
        Some(Reconsideration {
            verdict: result.verdict, measured_kbps: result.measured_kbps,
            comparison: gate.comparison(),
        })
    }
    pub(crate) fn probe_due(
        &mut self, telemetry: &abr::ControllerTelemetry, sample: abr::SegmentSample,
        rollback_media_ms: Option<u32>, remaining_ms: i64, now_ms: u64,
    ) -> Option<ProbeDecision> {
        let current = self.controller.catalog().candidate(self.controller.current());
        let delivery = self.controller.delivery();
        let result = self.recovery.as_mut()?.probe_due_with_rollback(
            current, self.controller.hls_frontier_exhausted(), &telemetry.production,
            sample, rollback_media_ms, self.controller.prime_runway_ms(),
            self.controller.buffer(), &delivery, remaining_ms, now_ms,
        );
        Some(ProbeDecision {
            result, current_wire_kbps: current.expected_wire_kbps,
            conservative_kbps: delivery.conservative_kbps(),
        })
    }
    pub(crate) fn source_switch(
        &self, fresh_probe_kbps: Option<u32>, decision: abr::Decision,
        cleanup_ready: bool, buffered_ms: Option<i64>, media_duration_ms: u32,
    ) -> LatchedOriginalRecovery {
        latched_original_recovery(
            self.recover_kbps, fresh_probe_kbps, decision, cleanup_ready,
            buffered_ms, media_duration_ms,
        )
    }

    /// A latched source recovery wins over a pending rung proposal only after
    /// the physical automatic lease is acquired. Return its diagnostic receipt
    /// before the caller enters the AQ publication fence.
    pub(crate) fn prepare_original_switch(
        &mut self, source: LatchedOriginalRecovery, now_ms: u64,
    ) -> Option<PreparedOriginalSwitch> {
        let LatchedOriginalRecovery::Switch { kbps, cancel } = source else {
            return None;
        };
        let cancelled = cancel.map(|proposal| {
            let refreshing = self.reject(proposal, abr::RejectCause::Circumstance, now_ms);
            (proposal, refreshing)
        });
        Some(PreparedOriginalSwitch { kbps, cancelled })
    }

    /// A source latch wins a physical lease before cancelling a competing rung.
    /// Publication itself is short and runs under the feed's AQ abort fence;
    /// a Busy or moved clock retains source evidence for a later boundary.
    pub(crate) fn run_latched_original<P: AutomaticPort>(
        &mut self, source: LatchedOriginalRecovery, port: &mut P,
        expected_user_sequence: u64, now_ms: impl FnOnce() -> u64,
        position_ns: impl FnOnce() -> i64,
        on_published: impl FnOnce(),
        on_cancelled_rung: impl FnOnce(abr::Proposal, bool),
    ) -> OriginalSwitchOutcome {
        if !matches!(source, LatchedOriginalRecovery::Switch { .. }) {
            return OriginalSwitchOutcome::NoSwitch;
        }
        let Some(lease) = port.begin(AutomaticTransition::Original, expected_user_sequence) else {
            return OriginalSwitchOutcome::LeaseBusy;
        };
        let prepared = self.prepare_original_switch(source, now_ms())
            .expect("a source latch was checked before acquiring its lease");
        if let Some((proposal, refreshing)) = prepared.cancelled {
            on_cancelled_rung(proposal, refreshing);
        }
        let authorized = port.authorize(&lease);
        match self.publish_original(
            port.gate(), authorized, prepared.kbps, position_ns, on_published,
        ) {
            OriginalPublication::Published => OriginalSwitchOutcome::Published { kbps: prepared.kbps },
            OriginalPublication::Busy => OriginalSwitchOutcome::Busy,
            OriginalPublication::ClockMoved => OriginalSwitchOutcome::ClockMoved,
            OriginalPublication::Aborted => OriginalSwitchOutcome::Aborted,
            OriginalPublication::RouteMoved => OriginalSwitchOutcome::RouteMoved,
        }
    }


    /// A bounded Original probe keeps its actuator lease through physical GET,
    /// fresh pause settlement and the AQ-fenced route request. A stale resource
    /// produces no source evidence; Busy and clock movement retain a recovery.
    pub(crate) fn run_original_probe<P: AutomaticPort>(
        &mut self, permit: abr::ProbePermit, port: &mut P,
        expected_user_sequence: u64, telemetry: &abr::ControllerTelemetry,
        remaining_ms: i64, decision: abr::Decision, cleanup_ready: bool,
        buffered_ms: Option<i64>, media_duration_ms: u32,
        pause: impl FnOnce() -> PauseFact,
        position_ns: impl FnOnce() -> i64,
        diagnostics: &mut impl OriginalProbeDiagnostics,
    ) -> OriginalProbeOutcome {
        let Some(lease) = port.begin(AutomaticTransition::Original, expected_user_sequence) else {
            diagnostics.log("abr: Original probe deferred behind a playback-clock transition");
            return OriginalProbeOutcome::LeaseBusy;
        };
        diagnostics.action(ProbeTraceAction::Start, self.original_source_kbps());
        diagnostics.log("abr: checking finite actual Original range on the live HLS resource");
        let probe = match self.probe_original(permit.plan, || port.aborted()) {
            ProbeResult::Measured(sample) => sample,
            ProbeResult::Stale => {
                diagnostics.log("abr: discarded Original probe from a superseded HLS resource");
                return OriginalProbeOutcome::Stale;
            }
            ProbeResult::Failed { outcome, failure } => {
                diagnostics.failure(failure);
                let grade = self.probe_failed();
                diagnostics.action(ProbeTraceAction::Failed, 0);
                diagnostics.log(&format!(
                    "abr: Original probe #{} produced no body outcome={}; retaining best HLS \
                     request left={}s verdict={:?}",
                    grade.as_ref().map(|grade| grade.probes).unwrap_or(0),
                    outcome, remaining_ms / 1_000,
                    grade.as_ref().map(|grade| grade.verdict),
                ));
                return OriginalProbeOutcome::Continue;
            }
        };
        let grade = self.grade_probe(probe, telemetry, remaining_ms);
        if let Some(cmp) = grade.as_ref().and_then(|grade| grade.comparison) {
            diagnostics.log(&mode_comparison_line(cmp));
        }
        let (slow, unc, n, cons, need) = grade.as_ref()
            .map(|grade| grade.basis).unwrap_or_default();
        diagnostics.log(&format!(
            "abr: Original probe #{} measured={}kbps {}KiB/{}ms complete={} left={}s \
             slow={}kbps unc={}pm n={} cons={}kbps need={}kbps verdict={:?}",
            grade.as_ref().map(|grade| grade.probes).unwrap_or(0),
            probe.kbps, probe.bytes / 1024, probe.active_us / 1_000,
            probe.completed as i32, remaining_ms / 1_000,
            slow, unc, n, cons, need,
            grade.as_ref().map(|grade| grade.verdict),
        ));
        let recovery = self.settle_probe_recovery(
            grade.as_ref(), probe.kbps, pause, decision, cleanup_ready,
            buffered_ms, media_duration_ms,
        );
        match recovery {
            ProbeRecoveryAction::Steady => {
                diagnostics.action(ProbeTraceAction::Steady, 0);
                OriginalProbeOutcome::Continue
            }
            ProbeRecoveryAction::Held => {
                diagnostics.log(
                    "abr: Original recovery measured during a user hold; retained until Resume",
                );
                OriginalProbeOutcome::Continue
            }
            ProbeRecoveryAction::Retained => OriginalProbeOutcome::Continue,
            ProbeRecoveryAction::Switch { kbps, measured_kbps } => {
                let authorized = port.authorize(&lease);
                let publication = self.publish_probed_original(
                    port.gate(), authorized, kbps, measured_kbps, position_ns,
                    || diagnostics.action(ProbeTraceAction::RecoverOriginal, 0),
                );
                match publication {
                    OriginalPublication::Published => {
                        diagnostics.log(&format!(
                            "abr: source sustainable again at {}kbps; requesting Original on the probe boundary",
                            kbps,
                        ));
                        OriginalProbeOutcome::Published
                    }
                    OriginalPublication::Busy => {
                        diagnostics.log(
                            "abr: Original recovery retained behind another route transition",
                        );
                        OriginalProbeOutcome::Continue
                    }
                    OriginalPublication::ClockMoved => {
                        diagnostics.log(
                            "abr: Original recovery retained after playback clock moved",
                        );
                        OriginalProbeOutcome::Continue
                    }
                    OriginalPublication::Aborted | OriginalPublication::RouteMoved =>
                        OriginalProbeOutcome::Aborted,
                }
            }
        }
    }

    /// The active observation is complete and any same-encoder lookahead has
    /// finished. Reconsider a retained Original verdict before choosing the
    /// higher-priority source switch or the next quality action.
    pub(crate) fn after_active_prefetch(
        &mut self, round: &RoundDecision, sample: abr::SegmentSample,
        delivered: bool, remaining_ms: i64, paused: bool, cleanup_ready: bool,
    ) -> ActiveAfterPrefetch {
        let reconsidered = if delivered && sample.completed() {
            self.reconsider_original(&round.telemetry, remaining_ms)
        } else {
            None
        };
        let source = if paused {
            LatchedOriginalRecovery::Wait
        } else {
            self.source_switch(
                None, round.decision, cleanup_ready,
                sample.buffer.buffered_ms(), sample.media_duration_ms(),
            )
        };
        ActiveAfterPrefetch {
            reconsidered,
            steady: matches!(round.decision, abr::Decision::Stay),
            source,
        }
    }

    /// Called after the higher-priority latched source switch had its publication
    /// opportunity. A probe spends its permit only on a Stay boundary; a quality
    /// transaction receives its pending reserve policy from the same controller.
    pub(crate) fn next_round_action(
        &mut self, round: &RoundDecision, sample: abr::SegmentSample,
        rollback_media_ms: Option<u32>, remaining_ms: i64,
        now_ms: impl FnOnce() -> u64, paused: bool, cleanup_ready: bool,
        permission: impl FnOnce(AutomaticTransition) -> bool,
        rebuffer: impl FnOnce() -> (bool, bool),
    ) -> RoundAction {
        let automatic_permitted = match round.decision {
            abr::Decision::Stay => true,
            abr::Decision::Prime(proposal) => permission(match proposal.direction {
                abr::Direction::Up => AutomaticTransition::QualityUp,
                abr::Direction::Down => AutomaticTransition::QualityDown,
            }),
        };
        let now_ms = now_ms();
        let (rebuffer_requested, rebuffering) = rebuffer();
        self.next_action(
            round.decision, &round.telemetry, sample, rollback_media_ms,
            remaining_ms, now_ms, paused, cleanup_ready, automatic_permitted,
            rebuffer_requested, rebuffering,
        )
    }

    fn next_action(
        &mut self, decision: abr::Decision, telemetry: &abr::ControllerTelemetry,
        sample: abr::SegmentSample, rollback_media_ms: Option<u32>, remaining_ms: i64,
        now_ms: u64, paused: bool, cleanup_ready: bool,
        automatic_permitted: bool, rebuffer_requested: bool, rebuffering: bool,
    ) -> RoundAction {
        match decision {
            abr::Decision::Stay if !paused => {
                match self.probe_due(telemetry, sample, rollback_media_ms, remaining_ms, now_ms) {
                    Some(ProbeDecision {
                        result: Err(reason), current_wire_kbps, conservative_kbps,
                    }) => RoundAction::ProbeBlocked(ProbeBlockSummary {
                        reason, current_wire_kbps, conservative_kbps,
                    }),
                    Some(ProbeDecision { result: Ok(_), .. }) if !cleanup_ready => {
                        RoundAction::ProbeDeferredForCleanup
                    }
                    Some(ProbeDecision { result: Ok(permit), .. }) => RoundAction::Probe(permit),
                    None => RoundAction::Stay,
                }
            }
            abr::Decision::Stay => RoundAction::Stay,
            abr::Decision::Prime(proposal) => {
                // A higher-priority source latch can cancel this proposal while
                // its route request is Busy. The completed quantum's decision is
                // then stale and cannot open a new encoder.
                if self.controller.pending_reserve_policy(proposal).is_none() {
                    return RoundAction::Stay;
                }
                if proposal.direction == abr::Direction::Up && !cleanup_ready {
                    let refreshing_same_request = self.reject(
                        proposal, abr::RejectCause::Circumstance, now_ms,
                    );
                    return RoundAction::TrialDeferred {
                        proposal, refreshing_same_request,
                        reason: TrialDeferral::EncoderCleanup,
                    };
                }
                if !automatic_permitted || !hls_quality_trial_may_start(
                    proposal.direction, rebuffer_requested, rebuffering, paused,
                ) {
                    let refreshing_same_request = self.reject(
                        proposal, abr::RejectCause::Circumstance, now_ms,
                    );
                    return RoundAction::TrialDeferred {
                        proposal, refreshing_same_request,
                        reason: TrialDeferral::PlaybackClock,
                    };
                }
                RoundAction::Trial {
                    proposal, reserve_policy: self.pending_reserve_policy(proposal),
                }
            }
        }
    }
    pub(crate) fn probe_failed(&mut self) -> Option<ProbeGrade> {
        let delivery = self.controller.delivery();
        let gate = self.recovery.as_mut()?;
        let verdict = gate.observe_probe_failed(&delivery);
        Some(ProbeGrade {
            verdict, comparison: gate.comparison(), basis: gate.basis(), probes: gate.probes(),
        })
    }
    pub(crate) fn grade_probe(
        &mut self, probe: abr::CapacityObservation,
        telemetry: &abr::ControllerTelemetry, remaining_ms: i64,
    ) -> Option<ProbeGrade> {
        let current = self.controller.catalog().candidate(self.controller.current());
        let delivery = self.controller.delivery();
        let gate = self.recovery.as_mut()?;
        let verdict = gate.observe_probe(
            probe, current, &telemetry.production, self.controller.buffer(),
            &delivery, remaining_ms,
        );
        Some(ProbeGrade {
            verdict, comparison: gate.comparison(), basis: gate.basis(), probes: gate.probes(),
        })
    }
    /// A measured Original verdict gets a fresh pause snapshot only when it
    /// actually requests recovery. A user hold retains the result; a completed
    /// hidden pause cycle ages the controller before considering publication.
    pub(crate) fn settle_probe_recovery(
        &mut self, grade: Option<&ProbeGrade>, probe_kbps: u32,
        pause: impl FnOnce() -> PauseFact, decision: abr::Decision,
        cleanup_ready: bool, buffered_ms: Option<i64>, media_duration_ms: u32,
    ) -> ProbeRecoveryAction {
        if !grade.is_some_and(|grade| grade.verdict == abr::RecoveryVerdict::Recover) {
            return ProbeRecoveryAction::Steady;
        }
        let measured_kbps = probe_kbps.max(1);
        let pause = pause();
        if let Some(elapsed_ms) = pause.completed_ms {
            self.on_resume(elapsed_ms);
        }
        if pause.paused {
            self.retain_original_recovery(measured_kbps);
            return ProbeRecoveryAction::Held;
        }
        match self.source_switch(
            Some(measured_kbps), decision, cleanup_ready,
            buffered_ms, media_duration_ms,
        ) {
            LatchedOriginalRecovery::Switch { kbps, cancel } => {
                debug_assert!(cancel.is_none(), "a source probe is issued only from Stay");
                ProbeRecoveryAction::Switch { kbps, measured_kbps }
            }
            LatchedOriginalRecovery::Wait => {
                self.retain_original_recovery(measured_kbps);
                ProbeRecoveryAction::Retained
            }
        }
    }
    pub(crate) fn probe_original(
        &self, plan: abr::SourceProbePlan, cancelled: impl FnOnce() -> bool,
    ) -> ProbeResult {
        match self.control.probe_original_while_hls_cancellable(&self.ticket, plan, cancelled) {
            route::OriginalProbeResult::Measured(sample) => ProbeResult::Measured(sample),
            route::OriginalProbeResult::Failed { outcome, failure } => ProbeResult::Failed {
                outcome: outcome.code(), failure: failure.into(),
            },
            route::OriginalProbeResult::Stale => ProbeResult::Stale,
        }
    }
    pub(crate) fn publish_candidate(
        &mut self, port: &mut dyn PublicationGate, candidate: &route::PrimedHls,
        proposal: abr::Proposal, sample: abr::SegmentSample,
        variant: abr::ObservedHlsVariant, now_ms: u64,
    ) -> CandidatePublication {
        publish_candidate(port, &self.control, &mut self.controller, &mut self.ticket,
            candidate, proposal, sample, variant, now_ms)
    }
    /// Queue completion is the only entry into the AQ publication fence. A
    /// failed fence makes the trial terminal; neither its cursor nor its
    /// controller evidence can be reused for another publication attempt.
    pub(crate) fn publish_queued_candidate(
        &mut self, protocol: &mut TrialProtocol, port: &mut dyn PublicationGate,
        candidate: &route::PrimedHls, proposal: abr::Proposal,
        sample: abr::SegmentSample, variant: abr::ObservedHlsVariant, now_ms: u64,
    ) -> CandidatePublication {
        assert_eq!(protocol.phase(), TrialPhase::Queued, "publication requires queued media");
        let publication = self.publish_candidate(
            port, candidate, proposal, sample, variant, now_ms,
        );
        let observation = if matches!(&publication, CandidatePublication::Active { .. }) {
            TrialObservation::Published
        } else {
            TrialObservation::Ended
        };
        let trial = protocol.id();
        let result = protocol.observe(trial, observation);
        if matches!(&publication, CandidatePublication::Active { .. }) {
            assert_eq!(result, Ok(TrialPhase::Published));
        } else {
            assert_eq!(result, Err(TrialProtocolError::TerminalAfterQueue));
        }
        publication
    }
    pub(crate) fn publish_original(
        &self, port: &mut dyn PublicationGate, authorized: bool,
        kbps: u32, position_ns: impl FnOnce() -> i64,
        on_published: impl FnOnce(),
    ) -> OriginalPublication {
        publish_original(port, authorized, &self.control, &self.ticket,
            kbps, position_ns, on_published)
    }

    pub(crate) fn publish_probed_original(
        &mut self, port: &mut dyn PublicationGate, authorized: bool,
        kbps: u32, measured_kbps: u32,
        position_ns: impl FnOnce() -> i64, on_published: impl FnOnce(),
    ) -> OriginalPublication {
        let publication = self.publish_original(
            port, authorized, kbps, position_ns, on_published,
        );
        if matches!(publication, OriginalPublication::Busy | OriginalPublication::ClockMoved) {
            self.retain_original_recovery(measured_kbps);
        }
        publication
    }

    pub(crate) fn advance_to(&mut self, now_ms: u64) {
        if let Some(gate) = self.recovery.as_mut() { gate.advance_to(now_ms); }
    }

    pub(crate) fn observe_active_variant(
        &mut self, variant: abr::ObservedHlsVariant, evidence_kbps: u32,
    ) {
        self.controller.observe_active_variant(variant, evidence_kbps);
        self.control.observe_active_variant(&self.ticket, variant, evidence_kbps);
    }

    /// The route's cleanup runner is sampled once per completed active quantum.
    pub(crate) fn encoder_cleanup_ready(&self, completed_active: bool) -> bool {
        if completed_active {
            self.control.observe_encoder_cleanup()
        } else {
            self.control.encoder_cleanup_ready()
        }
    }

    /// The first active-boundary step runs before the rollback playlist lookup.
    /// That lookup may block, so cleanup and variant evidence use this earlier
    /// measurement instant exactly as they did in the feed loop.
    pub(crate) fn begin_active_quantum(
        &mut self, acquisition: Acquisition, buffer: impl FnOnce() -> BufferFact,
        abandoned: bool, delivered: bool, now_ms: u64,
    ) -> Option<ActiveQuantumResult> {
        self.advance_to(now_ms);
        let quantum = ActiveQuantum {
            acquisition, buffer: buffer(), abandoned, delivered,
        };
        let sample = active_sample(
            quantum.acquisition, quantum.buffer, quantum.abandoned,
        )?;
        if quantum.delivered {
            if let Some(variant) = abr::ObservedHlsVariant::new(
                quantum.acquisition.declared_bps,
                quantum.acquisition.width,
                quantum.acquisition.height,
            ) {
                self.observe_active_variant(variant, sample.network_kbps());
            }
        }
        let cleanup_ready = self.encoder_cleanup_ready(
            quantum.delivered && sample.completed(),
        );
        Some(ActiveQuantumResult {
            sample, cleanup_ready,
            delivered: quantum.delivered, abandoned: quantum.abandoned,
        })
    }


    pub(crate) fn on_resume(&mut self, elapsed_ms: u64) {
        self.controller.on_resume(elapsed_ms);
    }

    /// The pure controller step: the caller supplies the completed acquisition,
    /// current rollback obligation, and its monotonic time. No clock or registry
    /// is read here, and candidate media is never counted as active evidence.
    fn observe(
        &mut self, sample: abr::SegmentSample, rollback_media_ms: Option<u32>,
        now_ms: u64, session_boundary: bool,
    ) -> RoundDecision {
        let decision = if session_boundary {
            self.controller.observe_session_boundary(sample, rollback_media_ms, now_ms)
        } else {
            self.controller.observe_with_rollback(sample, rollback_media_ms, now_ms)
        };
        RoundDecision {
            decision,
            telemetry: self.controller.telemetry(),
            prime_runway_ms: self.controller.prime_runway_ms(),
        }
    }

    pub(crate) fn prime(
        &self,
        off: &plx_base::task::OffFrame,
        proposal: abr::Proposal,
        offset_micros: i64,
        deadline: Option<std::time::Instant>,
    ) -> Result<route::PrimedHls, PrimeFailure> {
        prime(&self.control, &self.ticket, off, proposal, offset_micros, deadline)
    }
}

/// A recovery downshift restores the picture; an upshift only explores quality.  Starting the
/// latter while the native clock is already held makes its private transaction latency part of
/// the visible rebuffer even though the recovery media is already in hand.  Order those two
/// operations by what they do: finish the active recovery edge first, then let ordinary measured
/// exploration run.  No duration or reserve threshold is involved.
pub(crate) fn hls_quality_trial_may_start(
    direction: abr::Direction,
    rebuffer_requested: bool,
    rebuffering: bool,
    user_paused: bool,
) -> bool {
    !user_paused
        && (direction == abr::Direction::Down || !(rebuffer_requested || rebuffering))
}

/// A source-mode recovery verdict is a higher-level actuator decision than an HLS rung proposal.
/// Once latched, it takes the next quiescent media boundary regardless of whether the rung
/// controller happened to answer `Stay` or `Prime` there. Returning the pending proposal makes
/// its cancellation explicit; otherwise it would survive into a source reload or, worse, let a
/// stale Original verdict cross a successful change of HLS operating point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LatchedOriginalRecovery {
    Wait,
    Switch {
        kbps: u32,
        cancel: Option<abr::Proposal>,
    },
}

pub(crate) fn latched_original_recovery(
    recover_kbps: u32,
    fresh_probe_kbps: Option<u32>,
    decision: abr::Decision,
    encoder_cleanup_ready: bool,
    buffered_ms: Option<i64>,
    media_duration_ms: u32,
) -> LatchedOriginalRecovery {
    // A fresh successful source observation is already sitting on a completed/fed HLS boundary.
    // It must be eligible on THIS call, not copied into a local latch which can only be consumed
    // after another HLS GET.  PMS may rebind the shared Streaming Resource to the raw Part as a
    // side effect of that probe; requiring one more old-session segment then makes the transition
    // depend on a resource the probe itself may have invalidated.
    let recover_kbps = fresh_probe_kbps.unwrap_or(recover_kbps);
    if recover_kbps == 0
        || !encoder_cleanup_ready
        || !buffered_ms.is_some_and(|ms| ms >= i64::from(media_duration_ms))
    {
        return LatchedOriginalRecovery::Wait;
    }
    LatchedOriginalRecovery::Switch {
        kbps: recover_kbps,
        cancel: match decision {
            abr::Decision::Stay => None,
            abr::Decision::Prime(proposal) => Some(proposal),
        },
    }
}


/// Classify a completed candidate from what its RESPONSE proved, never from the request label.
/// A no-gain response is PMS underfill: even when the request actuator differs, its smaller output
/// does not order that actuator against lower request ceilings. Treating it as structural walked
/// the whole ladder and allocated one physical encoder per rung on the remote PMS.
pub(crate) fn abr_reject_for_candidate_result(
    raster_ready: bool,
    quality_improves: bool,
    verdict: abr::CandidateVerdict,
) -> abr::RejectCause {
    if !raster_ready {
        return abr::RejectCause::StructuralAtOrBelow;
    }
    if !quality_improves {
        return abr::RejectCause::ResponseUnchanged;
    }
    match verdict {
        // A setup-bearing verdict is consumed by the serial continuation path before rejection.
        // If a future caller leaks it here, retain only an actuator-specific funding frontier;
        // it is explicitly not steady `A>D` evidence.
        abr::CandidateVerdict::SetupBearing => abr::RejectCause::Candidate,
        abr::CandidateVerdict::Unsustainable => {
            abr::RejectCause::CompletedUnsustainable
        }
        abr::CandidateVerdict::Unfunded => abr::RejectCause::Candidate,
        abr::CandidateVerdict::Incomplete | abr::CandidateVerdict::ReserveUnknown => {
            abr::RejectCause::Circumstance
        }
        abr::CandidateVerdict::Ready => {
            unreachable!("a ready candidate can only be rejected by response geometry")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsCandidateDisposition {
    Commit,
    Discard {
        cause: abr::RejectCause,
        outcome: &'static str,
    },
}

pub(crate) fn hls_candidate_requires_rung_box(
    proposal: abr::Proposal,
    reserve_policy: abr::ReservePolicy,
) -> bool {
    !(proposal.direction == abr::Direction::Down
        && proposal.rung.at_floor()
        && reserve_policy == abr::ReservePolicy::TerminalFloor)
}

pub(crate) fn hls_candidate_disposition(
    raster_ready: bool,
    quality_improves: bool,
    verdict: abr::CandidateVerdict,
) -> HlsCandidateDisposition {
    if raster_ready && quality_improves && verdict == abr::CandidateVerdict::Ready {
        HlsCandidateDisposition::Commit
    } else {
        let outcome = if !raster_ready {
            "raster_refused"
        } else if !quality_improves {
            "no_quality_gain"
        } else {
            "not_ready_discarded"
        };
        HlsCandidateDisposition::Discard {
            cause: abr_reject_for_candidate_result(raster_ready, quality_improves, verdict),
            outcome,
        }
    }
}


pub(crate) fn hls_raster_within(width: i32, height: i32, rung: abr::Rung) -> bool {
    let (max_width, max_height) = rung.raster();
    width > 0 && height > 0 && width <= i32::from(max_width) && height <= i32::from(max_height)
}

/// A larger request is a quality transition only when its OBSERVED output strictly dominates the
/// stream it would replace.  Raster and the master playlist's declared bandwidth are independent
/// axes: more bits at the same raster can improve an encode, and more pixels at a lower declared
/// rate is not orderable without inventing a subjective weight.  Pareto dominance is therefore
/// the strongest statement the measurements alone support — no resolution target, ratio or
/// tolerance is introduced.
pub(crate) fn hls_upshift_strictly_improves(
    current_width: i32,
    current_height: i32,
    current_declared_bps: u64,
    candidate_width: i32,
    candidate_height: i32,
    candidate_declared_bps: u64,
) -> bool {
    let Some(current) =
        abr::ObservedHlsVariant::new(current_declared_bps, current_width, current_height)
    else {
        return false;
    };
    abr::ObservedHlsVariant::new(candidate_declared_bps, candidate_width, candidate_height)
        .is_some_and(|candidate| candidate.strictly_dominates(current))
}

#[cfg(test)]
mod trial_runner_tests {
    use super::*;
    use plx_playback_contract::adaptive::TrialId;
    use plx_playback_contract::AttemptId;

    struct LateReplyPort { response: FeedObservation }

    impl FeedPort for LateReplyPort {
        type Locator = ();

        fn bind_source(&mut self, _source: SourceKey, _locator: ()) {}

        fn step(&mut self, _action: FeedAction) -> FeedObservation { self.response }
    }

    #[test]
    fn late_open_reply_cannot_advance_a_new_trial() {
        let current = TrialId::new(AttemptId::new(3, 8), 12);
        let previous = TrialId::new(AttemptId::new(3, 7), 12);
        let mut protocol = TrialProtocol::new(current);
        protocol.observe(current, TrialObservation::Prepared).unwrap();
        let mut port = LateReplyPort {
            response: FeedObservation::Opened { trial: previous, declared_bps: 2_000_000 },
        };
        let mut runner = TrialRunner::new(&mut port, &mut protocol);
        assert_eq!(runner.open(()), FeedObservation::Failed {
            trial: current, failure: FeedFailure::Protocol,
        });
        drop(runner);
        assert_eq!(protocol.phase(), TrialPhase::Prepared);
    }

    #[test]
    fn late_queue_reply_after_enqueue_stays_terminal() {
        let current = TrialId::new(AttemptId::new(4, 9), 2);
        let previous = TrialId::new(AttemptId::new(4, 8), 2);
        let mut protocol = TrialProtocol::new(current);
        for phase in [
            TrialObservation::Prepared, TrialObservation::FirstAcquired,
        ] {
            protocol.observe(current, phase).unwrap();
        }
        let mut port = LateReplyPort { response: FeedObservation::Queued { trial: previous } };
        let mut runner = TrialRunner::new(&mut port, &mut protocol);
        runner.authorize_queue();
        assert_eq!(runner.queue_staged(), FeedObservation::Failed {
            trial: current, failure: FeedFailure::Protocol,
        });
        drop(runner);
        assert_eq!(protocol.phase(), TrialPhase::Ended);
        assert_eq!(protocol.observe(current, TrialObservation::Published),
            Err(TrialProtocolError::OutOfOrder));
    }

    #[test]
    fn setup_bearing_followup_checks_the_original_reserve_once() {
        struct ReservePort { reply: FeedObservation, checks: usize }
        impl FeedPort for ReservePort {
            type Locator = ();
            fn bind_source(&mut self, _source: SourceKey, _locator: ()) {}
            fn step(&mut self, action: FeedAction) -> FeedObservation {
                assert!(matches!(action, FeedAction::CheckFollowupReserve { .. }));
                self.checks += 1;
                self.reply
            }
        }
        let trial = TrialId::new(AttemptId::new(5, 11), 3);
        let mut protocol = TrialProtocol::new(trial);
        protocol.observe(trial, TrialObservation::Prepared).unwrap();
        protocol.observe(trial, TrialObservation::FirstAcquired).unwrap();
        let mut port = ReservePort {
            reply: FeedObservation::FollowupReserve {
                trial, funded: false, remaining_ms: Some(0),
            },
            checks: 0,
        };
        let mut runner = TrialRunner::new(&mut port, &mut protocol);
        assert!(matches!(runner.after_first_grade(FirstCandidateGrade {
            verdict: abr::CandidateVerdict::Ready, needs_followup: false,
        }), FirstCandidateAction::Finalize { .. }));
        assert_eq!(runner.physical().checks, 0);
        assert!(matches!(runner.after_first_grade(FirstCandidateGrade {
            verdict: abr::CandidateVerdict::SetupBearing, needs_followup: true,
        }), FirstCandidateAction::Unfunded));
        assert_eq!(runner.physical().checks, 1);
        runner.physical().reply = FeedObservation::FollowupReserve {
            trial, funded: true, remaining_ms: Some(750),
        };
        assert!(matches!(runner.after_first_grade(FirstCandidateGrade {
            verdict: abr::CandidateVerdict::SetupBearing, needs_followup: true,
        }), FirstCandidateAction::ReadFollowup { remaining_ms: Some(750), .. }));
        assert_eq!(runner.physical().checks, 2);
    }

    #[test]
    fn repeatable_candidate_is_read_and_staged_before_it_can_be_graded() {
        use std::collections::VecDeque;
        struct Port {
            replies: VecDeque<FeedObservation>,
            actions: Vec<FeedAction>,
            media_trace: Vec<TrialTraceMoment>,
        }
        impl FeedPort for Port {
            type Locator = ();
            fn bind_source(&mut self, _source: SourceKey, _locator: ()) {}
            fn step(&mut self, action: FeedAction) -> FeedObservation {
                self.actions.push(action);
                self.replies.pop_front().expect("one reply per physical step")
            }
        }
        let trial = TrialId::new(AttemptId::new(8, 13), 4);
        let mut protocol = TrialProtocol::new(trial);
        protocol.observe(trial, TrialObservation::Prepared).unwrap();
        protocol.observe(trial, TrialObservation::FirstAcquired).unwrap();
        let acquisition = Acquisition {
            bytes: 200_000, active_us: 100_000, total_us: 125_000,
            duration: std::time::Duration::from_secs(2),
            width: 1280, height: 720, declared_bps: 2_000_000,
        };
        let buffer = BufferFact {
            playback_ms: 1_000, video_tail_ms: 5_000,
            audio_tail_ms: Some(5_000), audio_expected: true,
        };
        let mut port = Port {
            replies: VecDeque::from([
                FeedObservation::SegmentReady { trial, duration: acquisition.duration },
                FeedObservation::Acquired { trial, acquisition },
                FeedObservation::Staged { trial, buffer, count: 2 },
            ]),
            actions: Vec::new(), media_trace: Vec::new(),
        };
        let mut budgets = Vec::new();
        let mut runner = TrialRunner::new(&mut port, &mut protocol);
        let result = runner.read_repeatable(
            Some(700), &mut |budget| budgets.push(budget),
            &mut |port, moment| port.media_trace.push(moment),
        );
        assert!(matches!(result, Ok((got, fact)) if got == acquisition && fact == buffer));
        drop(runner);
        assert_eq!(budgets, [Some(700)]);
        assert_eq!(port.actions, [
            FeedAction::ReadNext { trial },
            FeedAction::AcquireFollowup { trial },
            FeedAction::StageAcquired { trial },
        ]);
        assert_eq!(port.media_trace, [TrialTraceMoment::FollowupMedia]);
        assert_eq!(protocol.phase(), TrialPhase::FollowupAcquired);
    }

    #[test]
    fn failure_course_depends_on_trial_stage_not_only_transport_code() {
        use TrialFailureAction as A;
        use TrialRejectReason as R;
        use TrialStep as S;
        assert_eq!(trial_failure_action(S::Open, FeedFailure::SourceMoved),
            A::Reject { cause: abr::RejectCause::Circumstance, reason: R::SourceMoved });
        assert_eq!(trial_failure_action(S::FirstRead, FeedFailure::SourceMoved), A::Abort);
        assert_eq!(trial_failure_action(S::FollowupRead, FeedFailure::Ended),
            A::Reject { cause: abr::RejectCause::Circumstance, reason: R::Ended });
        assert_eq!(trial_failure_action(S::FirstAcquire, FeedFailure::Deadline),
            A::Reject { cause: abr::RejectCause::Censored, reason: R::Deadline });
        assert_eq!(trial_failure_action(S::FollowupAcquire, FeedFailure::TimelineOverflow),
            A::TimelineOverflow);
        assert_eq!(trial_failure_action(S::Open, FeedFailure::Protocol), A::Protocol);
    }
}

