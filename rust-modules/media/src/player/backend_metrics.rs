//! Local measurements derived from the common owner's accepted effects. The route owner supplies
//! the logical request ID, each physical Start origin, and monotonic ticks. No clock or recorder
//! is read here; results leave with the runner call that produced them.
//! A queued dispatch is not a native command ACK. Only an addressed ControlAccepted notice
//! contributes CommandAcknowledged; position and picture evidence remain separate.
use crate::player::attempt_metrics as m;
use plx_playback_contract::{AttemptId, BackendKind, Failure, Fx, Observation,
    Operation, Playback, ReleaseStatus, StopReason};

/// A wrapper around the route owner's logical request number, not another playback owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LogicalRequestId(u64);
impl LogicalRequestId {
    pub const fn new(value: u64) -> Self { Self(value) }
    pub const fn get(self) -> u64 { self.0 }
}

/// Local typed results. A preflight refusal has no physical release, while a native Start
/// refusal does. Only the caller may turn a terminal handled error into an existing report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetricNotice {
    Measured(m::Fx<LogicalRequestId, AttemptId>),
    PreflightRejected { request: LogicalRequestId, backend: BackendKind, attempt: AttemptId,
        failure: m::Failure },
    Release { request: LogicalRequestId, backend: BackendKind, attempt: AttemptId,
        status: ReleaseStatus },
}

/// Preserve the common error resolver's established cause while keeping local measurement
/// vocabulary coarse. A source-open failure names no specific network or parser fault.
pub fn handled_failure(kind: crate::player::FailureKind) -> m::Failure {
    use crate::player::FailureKind as K;
    use m::{Cause, Stage};
    match kind {
        K::DecisionRefused => m::Failure { stage: Stage::Planning, cause: Cause::Refused },
        K::PlaybackPolicy | K::NoVideoTranscodeTarget | K::PreviewNotDirect =>
            m::Failure { stage: Stage::Planning, cause: Cause::Unsupported },
        K::NoVideoTrack => m::Failure { stage: Stage::Open, cause: Cause::Unsupported },
        K::MediaSource => m::Failure { stage: Stage::Open, cause: Cause::Unknown },
        K::PlaybackInterrupted =>
            m::Failure { stage: Stage::Presentation, cause: Cause::Unknown },
        K::TvPipeline => m::Failure { stage: Stage::Decode, cause: Cause::Native },
        K::JailMissingRtkmem =>
            m::Failure { stage: Stage::Open, cause: Cause::Unsupported },
        K::LoadTimeout => m::Failure { stage: Stage::Open, cause: Cause::Timeout },
        K::ReleaseTimeout => m::Failure { stage: Stage::Release, cause: Cause::Timeout },
        K::ResolveFailed => m::Failure { stage: Stage::Planning, cause: Cause::Unknown },
        K::Unspecified => m::Failure { stage: Stage::Unknown, cause: Cause::Unknown },
    }
}

#[derive(Clone, Copy)]
struct Physical {
    backend: BackendKind,
    attempt: AttemptId,
    ready: bool,
    picture: bool,
    finished: bool,
    paused: bool,
    pre_pause: Option<bool>,
    seeking: bool,
    held: bool,
    last_position: Option<u64>,
    stop: Option<StopReason>,
}

pub(super) struct Metrics {
    model: m::AttemptMetrics<LogicalRequestId, AttemptId>,
    physical: Option<Physical>,
    foreground: bool,
}
impl Metrics {
    pub(super) fn new() -> Self { Self { model: m::AttemptMetrics::new(), physical: None,
        foreground: true } }

    pub(super) fn snapshot(&self) -> Option<&m::RequestSummary<LogicalRequestId, AttemptId>> {
        self.model.snapshot()
    }

    fn request(&self) -> Option<LogicalRequestId> {
        self.model.snapshot().filter(|r| r.outcome.is_none()).map(|r| r.id)
    }

    fn step(&mut self, event: m::Event<LogicalRequestId, AttemptId>, out: &mut Vec<MetricNotice>) {
        let mut effects = Vec::new();
        self.model.step(event, &mut effects);
        out.extend(effects.into_iter().map(MetricNotice::Measured));
    }

    pub(super) fn begin(&mut self, request: LogicalRequestId, origin: m::Origin,
        at: m::Tick, out: &mut Vec<MetricNotice>) {
        let previous = self.request();
        self.step(m::Event::RequestStarted { request, origin, at }, out);
        if previous != Some(request) && self.request() == Some(request) { self.physical = None; }
    }

    pub(super) fn finish(&mut self, request: LogicalRequestId, outcome: m::RequestOutcome,
        at: m::Tick, out: &mut Vec<MetricNotice>) {
        self.step(m::Event::RequestFinished { request, outcome, at }, out);
    }

    pub(super) fn handoff(&mut self, request: LogicalRequestId, from: AttemptId,
        to: AttemptId, at: m::Tick, out: &mut Vec<MetricNotice>) {
        self.step(m::Event::Handoff { request, from, to, at }, out);
    }

    pub(super) fn condition(&mut self, backend: BackendKind, attempt: AttemptId,
        condition: m::Condition, at: m::Tick, out: &mut Vec<MetricNotice>) {
        if self.physical.is_some_and(|p| (p.backend, p.attempt) == (backend, attempt)) {
            let effective = if let Some(p) = self.physical.as_mut() {
                match condition {
                    m::Condition::Paused => p.paused = true,
                    m::Condition::Seeking => { p.seeking = true; p.last_position = None; }
                    m::Condition::Held => p.held = true,
                    m::Condition::Playing => {
                        if self.foreground {
                            p.paused = false; p.seeking = false; p.held = false;
                        }
                    }
                    m::Condition::Rebuffering | m::Condition::Unknown => {}
                }
                if !self.foreground || p.held { m::Condition::Held }
                else if p.paused { m::Condition::Paused }
                else if p.seeking { m::Condition::Seeking }
                else { condition }
            } else { return };
            if let Some(request) = self.request() {
                self.step(m::Event::Condition { request, attempt, condition: effective, at }, out);
            }
        }
    }

    pub(super) fn foreground(&mut self, foreground: bool, at: m::Tick,
        out: &mut Vec<MetricNotice>) {
        self.foreground = foreground;
        let Some(mut physical) = self.physical else { return };
        physical.held = !foreground;
        self.physical = Some(physical);
        self.condition(physical.backend, physical.attempt,
            if foreground { m::Condition::Unknown } else { m::Condition::Held }, at, out);
    }

    pub(super) fn intent(&mut self, intent: plx_playback_contract::Intent, at: m::Tick,
        out: &mut Vec<MetricNotice>) {
        use plx_playback_contract::Intent;
        match intent {
            Intent::Control { backend, attempt, operation: Operation::Pause } => {
                if let Some(p) = self.physical.as_mut().filter(|p| (p.backend,p.attempt)==(backend,attempt)) {
                    p.pre_pause.get_or_insert(p.paused);
                    p.paused = true;
                }
                self.condition(backend, attempt, m::Condition::Paused, at, out);
            }
            Intent::Control { backend, attempt, operation: Operation::Seek(_) } => {
                if let Some(p) = self.physical.as_mut().filter(|p| (p.backend,p.attempt)==(backend,attempt)) {
                    p.seeking = true;
                    p.last_position = None;
                }
                self.condition(backend, attempt, m::Condition::Seeking, at, out);
            }
            Intent::Stop { backend, attempt, reason } => {
                if let Some(p) = self.physical.as_mut().filter(|p| (p.backend,p.attempt)==(backend,attempt)) {
                    p.stop = Some(reason);
                }
                if reason != StopReason::Replace {
                    self.condition(backend, attempt, m::Condition::Held, at, out);
                }
            }
            _ => {}
        }
    }

    pub(super) fn dispatch(&mut self, backend: BackendKind, attempt: AttemptId,
        operation: Operation, origin: Option<m::Origin>, paused: bool,
        at: m::Tick, out: &mut Vec<MetricNotice>) {
        let Some(request) = self.request() else { return };
        match operation {
            Operation::Start { .. } => {
                let Some(origin) = origin else { return };
                self.step(m::Event::AttemptStarted { request, attempt,
                    backend: map_backend(backend), origin, at }, out);
                if self.model.snapshot().and_then(|r| r.current_attempt).is_some_and(|a| a.id == attempt) {
                    self.physical = Some(Physical { backend, attempt, ready: false, picture: false,
                        finished: false, paused, pre_pause: None, seeking: false,
                        held: paused || !self.foreground,
                        last_position: None, stop: None });
                    if paused { self.condition(backend, attempt, m::Condition::Paused, at, out); }
                }
            }
            Operation::Seek(_) | Operation::Pause => {
                self.intent(plx_playback_contract::Intent::Control { backend, attempt, operation }, at, out);
            }
            Operation::Stop(reason) => {
                self.intent(plx_playback_contract::Intent::Stop { backend, attempt, reason }, at, out);
            }
            _ => {}
        }
    }

    pub(super) fn rejected_start(&mut self, backend: BackendKind, attempt: AttemptId,
        reason: Failure, out: &mut Vec<MetricNotice>) {
        if let Some(request) = self.request() {
            out.push(MetricNotice::PreflightRejected { request, backend, attempt,
                failure: m::Failure { stage: m::Stage::Open, cause: map_cause(reason) } });
        }
    }

    pub(super) fn notice(&mut self, notice: &Fx, mandatory_refusal: bool,
        at: m::Tick, out: &mut Vec<MetricNotice>) {
        let (backend, attempt) = match *notice {
            Fx::AcceptedObservation { backend, attempt, .. }
            | Fx::AcceptedControl { backend, attempt, .. }
            | Fx::CommandRefused { backend, attempt, .. }
            | Fx::AcceptedSeekLanded { backend, attempt, .. }
            | Fx::ReleaseOutcome { backend, attempt, .. } => (backend,attempt),
            _ => return,
        };
        let request = if matches!(notice, Fx::ReleaseOutcome { .. }) {
            self.model.snapshot().map(|summary| summary.id)
        } else { self.request() };
        let Some(request) = request else { return };
        let Some(mut physical) = self.physical.filter(|p| (p.backend,p.attempt)==(backend,attempt)) else { return };
        match *notice {
            Fx::AcceptedControl { native_acknowledged, .. } => {
                if native_acknowledged {
                    self.step(m::Event::Evidence { request, attempt,
                        evidence: m::Evidence::CommandAcknowledged, at }, out);
                }
            }
            Fx::AcceptedObservation { observation, .. } => match observation {
                Observation::Playback(value) => match value.value {
                    Playback::ReadyAwaitPlay => physical.ready = true,
                    Playback::Playing => {
                        physical.pre_pause = None;
                        physical.paused = false;
                        physical.seeking = false;
                        physical.held = !self.foreground;
                        self.step(m::Event::Evidence { request, attempt,
                            evidence: m::Evidence::PlaybackObserved, at }, out);
                        self.condition(backend, attempt,
                            if self.foreground { m::Condition::Playing } else { m::Condition::Held },
                            at, out);
                    }
                    Playback::Paused => {
                        physical.pre_pause = None;
                        physical.paused = true;
                        self.condition(backend, attempt, m::Condition::Paused, at, out);
                    }
                    Playback::Ended => {
                        self.finish_attempt(request, &mut physical, m::AttemptOutcome::Ended, at, out);
                    }
                },
                Observation::Position(value) => {
                    let ms = value.value.as_millis();
                    if !physical.paused && !physical.seeking && !physical.held
                        && physical.last_position.is_some_and(|old| ms > old) {
                        self.step(m::Event::Evidence { request, attempt,
                            evidence: m::Evidence::ClockAdvanced, at }, out);
                    }
                    physical.last_position = Some(ms);
                }
                Observation::Presented => {
                    physical.picture = true;
                    self.step(m::Event::Evidence { request, attempt,
                        evidence: m::Evidence::PicturePresented, at }, out);
                }
                Observation::Failed(reason) => {
                    let stage = if !physical.ready { m::Stage::Open }
                        else if physical.picture { m::Stage::Decode }
                        else { m::Stage::Presentation };
                    self.finish_attempt(request, &mut physical,
                        m::AttemptOutcome::Failed(m::Failure { stage, cause: map_cause(reason) }), at, out);
                }
                Observation::TrackSelected(_) | Observation::TrackOff(_) => {}
            },
            Fx::CommandRefused { operation, failure, .. } => {
                if operation == Operation::Pause {
                    if let Some(previous) = physical.pre_pause.take() {
                        physical.paused = previous;
                        let condition = if !self.foreground || physical.held { m::Condition::Held }
                            else if previous { m::Condition::Paused }
                            else { m::Condition::Unknown };
                        self.step(m::Event::Condition { request, attempt, condition, at }, out);
                    }
                } else if !mandatory_refusal && matches!(operation,
                    Operation::Play | Operation::Seek(_) | Operation::SelectTrack { .. }) {
                    // Interactive controls can fail without ending an already live attempt.
                    if matches!(operation, Operation::Seek(_)) {
                        physical.seeking = false;
                        physical.last_position = None;
                        self.condition(backend, attempt, m::Condition::Unknown, at, out);
                    }
                } else {
                let stage = match operation {
                    Operation::Start { .. } => m::Stage::Open,
                    Operation::Stop(_) => m::Stage::Release,
                    _ => m::Stage::Presentation,
                };
                self.finish_attempt(request, &mut physical,
                    m::AttemptOutcome::Failed(m::Failure { stage, cause: map_cause(failure) }), at, out);
                }
            }
            Fx::AcceptedSeekLanded { .. } => {
                physical.seeking = false;
                physical.last_position = None;
                self.condition(backend, attempt,
                    if physical.paused || physical.held { m::Condition::Held } else { m::Condition::Unknown }, at, out);
            }
            Fx::ReleaseOutcome { status, .. } => {
                out.push(MetricNotice::Release { request, backend, attempt, status });
                if status == ReleaseStatus::Quarantined {
                    self.finish_attempt(request, &mut physical,
                        m::AttemptOutcome::Failed(m::Failure { stage: m::Stage::Release,
                            cause: m::Cause::Native }), at, out);
                } else if status == ReleaseStatus::Released {
                    let outcome = match physical.stop {
                        Some(StopReason::Replace) => m::AttemptOutcome::Superseded,
                        Some(_) => m::AttemptOutcome::Cancelled,
                        None => m::AttemptOutcome::Unconfirmed,
                    };
                    self.finish_attempt(request, &mut physical, outcome, at, out);
                }
            }
            _ => {}
        }
        self.physical = Some(physical);
    }

    fn finish_attempt(&mut self, request: LogicalRequestId, physical: &mut Physical,
        outcome: m::AttemptOutcome, at: m::Tick, out: &mut Vec<MetricNotice>) {
        if physical.finished { return; }
        self.step(m::Event::AttemptFinished { request, attempt: physical.attempt, outcome, at }, out);
        physical.finished = true;
    }
}

pub(super) fn map_backend(backend: BackendKind) -> m::Backend {
    match backend { BackendKind::Uri => m::Backend::Uri, BackendKind::Feed => m::Backend::Feed }
}
fn map_cause(reason: Failure) -> m::Cause {
    match reason {
        Failure::Busy | Failure::Stale | Failure::Refused => m::Cause::Refused,
        Failure::Unavailable | Failure::Unsupported => m::Cause::Unsupported,
        Failure::TimedOut => m::Cause::Timeout,
        Failure::Backend | Failure::ReleaseUncertain => m::Cause::Native,
    }
}
