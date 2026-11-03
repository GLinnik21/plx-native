//! One worker's adaptive HLS owner. The feed supplies measured media and serialized
//! queue application; this module owns the controller and route transition.

use crate::{abr, route};

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

/// The feed's existing abort mutex is the first serialization boundary. Its
/// implementation must run the callback while holding that mutex and do no IO.
pub(crate) trait PublicationPort {
    fn if_not_aborted<T>(&mut self, transition: impl FnOnce() -> T) -> Option<T>;
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
    port: &mut impl PublicationPort,
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
    port.if_not_aborted(|| {
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
    port: &mut impl PublicationPort,
    authorized: bool,
    control: &route::HlsAbrControl,
    ticket: &route::WorkerTicket,
    recover_kbps: u32,
    position_ns: impl FnOnce() -> i64,
    on_published: impl FnOnce(),
) -> OriginalPublication {
    if !authorized { return OriginalPublication::ClockMoved; }
    port.if_not_aborted(|| match control.request_original_recovery(
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

pub(crate) struct FinalCandidateGrade {
    pub(crate) raster_ready: bool,
    pub(crate) quality_improves: bool,
    pub(crate) disposition: HlsCandidateDisposition,
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
    pub(crate) fn worst_overhead_us(&self) -> u64 { self.controller.worst_overhead_us() }
    pub(crate) fn candidate_wire_kbps(&self, rung: abr::Rung) -> u32 {
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
    pub(crate) fn exploration_budget_ms(&self, buffered_ms: i64) -> Option<i64> {
        self.controller.exploration_budget_ms(buffered_ms)
    }
    pub(crate) fn set_executed_exploration_budget(
        &mut self, proposal: abr::Proposal, budget_ms: i64,
    ) -> bool {
        self.controller.set_executed_exploration_budget(proposal, budget_ms)
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
        outputs_inside_rung: bool, verdict: abr::CandidateVerdict,
    ) -> FinalCandidateGrade {
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
        &mut self, port: &mut impl PublicationPort, candidate: &route::PrimedHls,
        proposal: abr::Proposal, sample: abr::SegmentSample,
        variant: abr::ObservedHlsVariant, now_ms: u64,
    ) -> CandidatePublication {
        publish_candidate(port, &self.control, &mut self.controller, &mut self.ticket,
            candidate, proposal, sample, variant, now_ms)
    }
    pub(crate) fn publish_original(
        &self, port: &mut impl PublicationPort, authorized: bool,
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

