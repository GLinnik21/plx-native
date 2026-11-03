//! One worker's adaptive HLS owner. The feed supplies measured media and serialized
//! queue application; this module owns the controller and route transition.
#![deny(clippy::debug_assert_with_mut_call)]

use crate::{abr, route};
use plx_playback_contract::adaptive::PublicationGate;
use plx_playback_contract::adaptive::{
    Acquisition, FeedAction, FeedFailure, FeedObservation, FeedPort, TrialClock,
    TrialObservation, TrialPhase, TrialProtocol, TrialProtocolError,
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

    pub(crate) fn first_action(
        &mut self, coordinator: &Coordinator, proposal: abr::Proposal,
        current: VariantFacts, candidate: VariantFacts,
        sample: Option<abr::SegmentSample>,
    ) -> FirstCandidateAction {
        let grade = coordinator.first_candidate_grade(proposal, current, candidate, sample);
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

/// A physical feed failure is a circumstance unless the candidate's reserve clock expired.
pub(crate) fn reject_cause_for_feed_failure(failure: FeedFailure) -> abr::RejectCause {
    if failure == FeedFailure::Deadline {
        abr::RejectCause::Censored
    } else {
        abr::RejectCause::Circumstance
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
}

pub(crate) struct RoundDecision {
    pub(crate) decision: abr::Decision,
    pub(crate) telemetry: abr::ControllerTelemetry,
    pub(crate) prime_runway_ms: Option<i64>,
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

#[derive(Clone, Copy)]
pub(crate) struct VariantFacts {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) declared_bps: u64,
}

pub(crate) struct FirstCandidateGrade {
    pub(crate) verdict: abr::CandidateVerdict,
    pub(crate) needs_followup: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum FirstCandidateAction {
    Finalize { verdict: abr::CandidateVerdict },
    ReadFollowup { verdict: abr::CandidateVerdict, remaining_ms: Option<u64> },
    Unfunded,
    Protocol,
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
        Self { control, ticket, controller, recovery, recover_kbps: 0, next_trial_serial: 0 }
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
    pub(crate) fn begin_trial_protocol(
        &mut self,
    ) -> Option<plx_playback_contract::adaptive::TrialProtocol> {
        self.next_trial_serial = self.next_trial_serial.checked_add(1)?;
        let id = plx_playback_contract::adaptive::TrialId::new(
            self.ticket.adaptive_attempt(), self.next_trial_serial,
        );
        Some(plx_playback_contract::adaptive::TrialProtocol::new(id))
    }
    pub(crate) fn grant_exploration(
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
    pub(crate) fn reject(&mut self, proposal: abr::Proposal, cause: abr::RejectCause, now_ms: u64) -> bool {
        let refreshing = proposal.rung == self.controller.current();
        self.controller.reject(proposal, cause, now_ms);
        refreshing
    }
    pub(crate) fn candidate_verdict(
        &self, proposal: abr::Proposal, sample: abr::SegmentSample,
        declared_bps: u64, boundary: bool,
    ) -> abr::CandidateVerdict {
        if boundary {
            self.controller.candidate_boundary_verdict(proposal, sample, declared_bps)
        } else {
            self.controller.candidate_verdict(proposal, sample, declared_bps)
        }
    }
    pub(crate) fn first_candidate_grade(
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
    pub(crate) fn final_candidate_grade(
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
    pub(crate) fn reconsider_original(
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

    /// Called after the higher-priority latched source switch had its publication
    /// opportunity. A probe spends its permit only on a Stay boundary; a quality
    /// transaction receives its pending reserve policy from the same controller.
    pub(crate) fn next_action(
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

    pub(crate) fn on_resume(&mut self, elapsed_ms: u64) {
        self.controller.on_resume(elapsed_ms);
    }

    /// The pure controller step: the caller supplies the completed acquisition,
    /// current rollback obligation, and its monotonic time. No clock or registry
    /// is read here, and candidate media is never counted as active evidence.
    pub(crate) fn observe(
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
}

