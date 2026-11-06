//! One backend-neutral state runner for all physical playback drivers. The app composition owns
//! concrete source escrow and persistent drivers; this runner keeps only pure attempt state.
#[path = "backend_startup.rs"]
mod startup;
#[path = "backend_metrics.rs"]
mod metrics;

use plx_playback_contract::{BackendState, DriverSet, Event, ExitDisposition, Failure, Fx,
    Intent, Operation, Phase, Snapshot, AttemptId, BackendKind};
use super::attempt_metrics as measure;
pub use metrics::{handled_failure, terminal_failure_source, LogicalRequestId, MetricNotice,
    TerminalFailureSource};
use startup::Startup;

/// Route-owned lifecycle facts carried to the application runner. The route captures identity
/// at acceptance; the IO runner supplies a tick in its one monotonic clock domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestLifecycle {
    Begin { request: LogicalRequestId, origin: measure::Origin },
    Finish { request: LogicalRequestId, outcome: measure::RequestOutcome },
}

/// Immediate caller-owned results. Metric notices are local values; no report is sent here.
/// A command sent to a physical driver. Start's source key is deliberately excluded; this
/// receipt is for caller correlation of controls and never claims native acceptance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlDispatch {
    pub backend: BackendKind,
    pub attempt: AttemptId,
    pub command: plx_playback_contract::CommandId,
    pub operation: Operation,
}

#[derive(Default)]
pub struct RunResult {
    pub playback: Vec<Fx>,
    pub metrics: Vec<MetricNotice>,
    pub controls: Vec<ControlDispatch>,
}

pub struct BackendRunner { state: BackendState, startup: Startup, metrics: metrics::Metrics }

impl BackendRunner {
    pub fn new() -> Self { Self { state: BackendState::new(), startup: Startup::new(),
        metrics: metrics::Metrics::new() } }

    pub fn snapshot(&self) -> &Snapshot { self.state.snapshot() }

    pub fn startup_viewer_paused(&self, backend: BackendKind, attempt: AttemptId) -> Option<bool> {
        self.startup.viewer_paused(backend, attempt)
    }

    /// The route's viewer goal, including a pre-Ready startup change and a queued interactive
    /// command. Native Playback observations remain separate from this requested state.
    pub fn viewer_paused(&self, backend: BackendKind, attempt: AttemptId) -> Option<bool> {
        if self.state.snapshot().backend != Some(backend)
            || self.state.snapshot().attempt != Some(attempt)
            || !matches!(self.state.snapshot().phase, Phase::Starting | Phase::Active) {
            return None;
        }
        self.startup.viewer_paused(backend, attempt)
            .or(self.state.snapshot().viewer_paused)
    }

    pub fn metrics_snapshot(&self) -> Option<&measure::RequestSummary<LogicalRequestId, AttemptId>> {
        self.metrics.snapshot()
    }

    /// Begin one route-owned logical request. The supplied ID must increase in route order.
    pub fn begin_request_at(&mut self, request: LogicalRequestId, origin: measure::Origin,
        at: measure::Tick) -> RunResult {
        let mut result = RunResult::default();
        self.metrics.begin(request, origin, at, &mut result.metrics);
        result
    }

    /// The route owner settles the logical result after deciding whether another physical
    /// backend will be tried. A physical failure alone never emits a terminal handled error.
    pub fn finish_request_at(&mut self, request: LogicalRequestId,
        outcome: measure::RequestOutcome, at: measure::Tick) -> RunResult {
        let mut result = RunResult::default();
        self.metrics.finish(request, outcome, at, &mut result.metrics);
        result
    }

    pub fn handoff_at(&mut self, request: LogicalRequestId, from: AttemptId,
        to: AttemptId, at: measure::Tick) -> RunResult {
        let mut result = RunResult::default();
        self.metrics.handoff(request, from, to, at, &mut result.metrics);
        result
    }

    /// The common owner accepted recovery before its successor has a physical attempt ID.
    /// Address the gap to the failed attempt now; a later Start cannot count it twice.
    pub fn handoff_begun_at(&mut self, request: LogicalRequestId, from: AttemptId,
        at: measure::Tick) -> RunResult {
        let mut result = RunResult::default();
        self.metrics.handoff_begun(request, from, at, &mut result.metrics);
        result
    }

    /// Viewer/claim facts that the physical backend cannot infer. Held, paused, seeking and
    /// unknown intervals are excluded from involuntary stall time by the pure reducer.
    pub fn condition_at(&mut self, backend: BackendKind, attempt: AttemptId,
        condition: measure::Condition, at: measure::Tick) -> RunResult {
        let mut result = RunResult::default();
        self.metrics.condition(backend, attempt, condition, at, &mut result.metrics);
        result
    }

    /// Apply one route/player intent and its synchronous queue answer. Rejected Starts dispose
    /// only the exact staged source key, even while another backend owns the active attempt.
    pub fn send<C>(&mut self, drivers: &mut dyn DriverSet<Context = C>, ctx: &C, intent: Intent) -> Vec<Fx> {
        self.send_inner(drivers, ctx, intent, None, measure::Tick::Unknown).playback
    }

    /// Timed send. `attempt_origin` is supplied for each Start by the route owner; it is
    /// ignored for controls. No origin is inferred from backend order or the logical request.
    pub fn send_at<C>(&mut self, drivers: &mut dyn DriverSet<Context = C>, ctx: &C,
        intent: Intent, attempt_origin: measure::Origin, at: measure::Tick) -> RunResult {
        self.send_inner(drivers, ctx, intent, Some(attempt_origin), at)
    }

    fn send_inner<C>(&mut self, drivers: &mut dyn DriverSet<Context = C>, ctx: &C,
        intent: Intent, attempt_origin: Option<measure::Origin>, at: measure::Tick) -> RunResult {
        let mut result = RunResult::default();
        if let Intent::Start { backend, attempt, .. } = intent {
            if self.state.snapshot().phase == Phase::Vacant && matches!(attempt_origin,
                Some(measure::Origin::AutomaticHandoff | measure::Origin::Recovery)) {
                let handoff = self.metrics.snapshot().and_then(|request|
                    request.current_attempt
                        .filter(|current| current.backend != metrics::map_backend(backend)
                            && current.id != attempt)
                        .map(|current| (request.id, current.id)));
                if let Some((request, from)) = handoff {
                    self.metrics.handoff(request, from, attempt, at, &mut result.metrics);
                }
            }
        }
        match intent {
            Intent::Start { backend, attempt, requirements, initial, .. }
                if self.state.snapshot().phase == Phase::Vacant => {
                self.startup.begin(backend, attempt, requirements, initial);
            }
            Intent::Control { backend, attempt, operation }
                if self.startup.intercept_control(backend, attempt, operation) => {
                result.playback = if self.startup.is_halted(backend, attempt) {
                    vec![Fx::Rejected { backend, attempt, failure: Failure::Refused }]
                } else if matches!(operation, Operation::Play | Operation::Pause) {
                    self.metrics.intent(intent, at, &mut result.metrics);
                    self.feed(drivers, ctx, Event::ViewerGoal { backend, attempt,
                        paused: operation == Operation::Pause }, None, None, at, &mut result);
                    return result;
                } else {
                    vec![Fx::Rejected { backend, attempt, failure: Failure::Busy }]
                };
                return result;
            }
            Intent::Control { backend, attempt, operation: Operation::Play }
                if !self.startup.foreground() => {
                result.playback.push(Fx::Rejected { backend, attempt, failure: Failure::Busy });
                return result;
            }
            Intent::Stop { backend, attempt, .. } => self.startup.abort(backend, attempt),
            _ => {}
        }
        let capabilities = drivers.capabilities(intent.backend());
        self.feed(drivers, ctx, Event::Intent(intent), capabilities, attempt_origin, at,
            &mut result);
        result
    }

    /// Startup never plays in the background. Foreground restoration resumes only an unpaused
    /// startup goal whose exact track and seek confirmations have already arrived.
    pub fn set_foreground<C>(&mut self, drivers: &mut dyn DriverSet<Context = C>,
        ctx: &C, foreground: bool) -> Vec<Fx> {
        self.set_foreground_at(drivers, ctx, foreground, measure::Tick::Unknown).playback
    }

    pub fn set_foreground_at<C>(&mut self, drivers: &mut dyn DriverSet<Context = C>,
        ctx: &C, foreground: bool, at: measure::Tick) -> RunResult {
        let mut result = RunResult::default();
        self.metrics.foreground(foreground, at, &mut result.metrics);
        self.startup.set_foreground(foreground);
        self.advance(drivers, ctx, at, &mut result);
        result
    }

    /// Accepted observations, seek landings and release verdicts remain in driver poll order,
    /// including terminal evidence before a same-poll release resets the sparse snapshot.
    pub fn poll<C>(&mut self, drivers: &mut dyn DriverSet<Context = C>, ctx: &C) -> Vec<Fx> {
        self.poll_at(drivers, ctx, measure::Tick::Unknown).playback
    }

    pub fn poll_at<C>(&mut self, drivers: &mut dyn DriverSet<Context = C>,
        ctx: &C, at: measure::Tick) -> RunResult {
        let mut result = RunResult::default();
        for event in drivers.poll(ctx) {
            self.feed(drivers, ctx, event.into(), None, None, at, &mut result);
        }
        result
    }

    /// Both the common attempt and every physical driver must be released before exit is clean.
    pub fn exit_disposition<C>(&self, drivers: &dyn DriverSet<Context = C>) -> ExitDisposition {
        if self.state.exit_disposition() == ExitDisposition::Clean
            && drivers.exit_disposition() == ExitDisposition::Clean { ExitDisposition::Clean }
        else { ExitDisposition::PreserveNativeOwnership }
    }

    fn feed<C>(&mut self, drivers: &mut dyn DriverSet<Context = C>, ctx: &C,
        event: Event, capabilities: Option<plx_playback_contract::Capabilities>,
        attempt_origin: Option<measure::Origin>, at: measure::Tick, result: &mut RunResult) {
        let mut effects = Vec::new();
        self.state.step(event, capabilities, &mut effects);
        for effect in effects {
            match effect {
                Fx::Dispatch { backend, attempt, command, operation } => {
                    if !matches!(operation, Operation::Start { .. }) {
                        result.controls.push(ControlDispatch { backend, attempt, command, operation });
                    }
                    let paused = matches!(event, Event::Intent(Intent::Start { initial, .. }) if initial.paused);
                    self.metrics.dispatch(backend, attempt, operation, attempt_origin, paused,
                        at, &mut result.metrics);
                    self.startup.on_dispatch(backend, attempt, command, operation);
                    let answer = drivers.dispatch(backend, ctx, attempt, command, operation);
                    self.feed(drivers, ctx, Event::CommandAnswer {
                        backend, attempt, command, result: answer,
                    }, None, None, at, result);
                }
                Fx::DiscardStaged { backend, attempt, key } => {
                    drivers.discard_staged(backend, attempt, key);
                }
                notice @ (Fx::Rejected { .. } | Fx::CommandRefused { .. }
                    | Fx::AcceptedControl { .. } | Fx::StartupFailed { .. }
                    | Fx::AcceptedObservation { .. } | Fx::AcceptedTrackSettled { .. }
                    | Fx::AcceptedSeekLanded { .. } | Fx::ReleaseOutcome { .. }) => {
                    let mandatory_refusal = self.startup.is_mandatory_refusal(&notice);
                    self.startup.on_notice(&notice);
                    match (event, notice) {
                        (Event::Intent(Intent::Start { .. }), Fx::Rejected { backend, attempt, failure }) =>
                            self.metrics.rejected_start(backend, attempt, failure, at, &mut result.metrics),
                        (Event::Intent(Intent::Stop { .. }), Fx::Rejected { backend, attempt, failure }) =>
                            self.metrics.rejected_stop(backend, attempt, failure, at, &mut result.metrics),
                        _ => {}
                    }
                    self.metrics.notice(&notice, mandatory_refusal, at, &mut result.metrics);
                    result.playback.push(notice);
                    if mandatory_refusal {
                        if let Fx::CommandRefused { backend, attempt, command, failure, .. } = notice {
                            result.playback.push(Fx::StartupFailed { backend, attempt, command, failure });
                        }
                    }
                }
            }
        }
        self.advance(drivers, ctx, at, result);
    }

    fn advance<C>(&mut self, drivers: &mut dyn DriverSet<Context = C>, ctx: &C,
        at: measure::Tick, result: &mut RunResult) {
        let Some((backend, attempt, operation)) = self.startup.next() else { return };
        let capabilities = drivers.capabilities(backend);
        self.feed(drivers, ctx, Event::Intent(Intent::Control {
            backend, attempt, operation,
        }), capabilities, None, at, result)
    }
}
impl Default for BackendRunner { fn default() -> Self { Self::new() } }

#[cfg(test)]
mod tests {
    use super::*;
    use plx_playback_contract::{AttemptId, BackendEvent, BackendKind, Capabilities, CommandId,
        ContentTime, DispatchResult, DriverEvent, EvidenceSource, Failure, Fidelity, InitialPlayback,
        Observation, Observed, Operation, Playback, ReleaseStatus, Requirements, SourceKey,
        SourceKind, StopReason, TrackChoice};

    const URI: BackendKind = BackendKind::Uri;
    const FEED: BackendKind = BackendKind::Feed;
    const CAPS: Capabilities = Capabilities {
        progressive_uri: true, hls_uri: false, packet_feed: true, pause: true, seek: true,
        audio_track: true, subtitle_track: true, audio_off: false, subtitle_off: false,
    };
    const REQ: Requirements = Requirements {
        source: SourceKind::ProgressiveUri, video: Fidelity::Exact, audio: Fidelity::Exact,
        audio_track: TrackChoice::Unspecified, subtitle_track: TrackChoice::Unspecified,
    };
    const INITIAL: InitialPlayback = InitialPlayback { position: None, paused: false };
    const A: AttemptId = AttemptId::new(1, 1);

    struct FakeSet {
        uri: bool,
        feed: bool,
        caps: Capabilities,
        sent: Vec<(BackendKind, AttemptId, CommandId, Operation)>,
        events: Vec<BackendEvent>,
        discarded: Vec<(BackendKind, AttemptId, SourceKey)>,
        refuse_start: bool,
        refuse_stop: bool,
        exit: ExitDisposition,
    }
    impl FakeSet {
        fn new() -> Self {
            Self { uri: true, feed: true, caps: CAPS, sent: Vec::new(), events: Vec::new(),
                discarded: Vec::new(), refuse_start: false, refuse_stop: false,
                exit: ExitDisposition::Clean }
        }
    }
    impl DriverSet for FakeSet {
        type Context = ();
        fn capabilities(&self, backend: BackendKind) -> Option<Capabilities> {
            match backend {
                URI if self.uri => Some(self.caps), FEED if self.feed => Some(self.caps), _ => None,
            }
        }
        fn dispatch(&mut self, backend: BackendKind, _ctx: &(), attempt: AttemptId,
            command: CommandId, operation: Operation) -> DispatchResult {
            self.sent.push((backend, attempt, command, operation));
            if (self.refuse_start && matches!(operation, Operation::Start { .. }))
                || (self.refuse_stop && matches!(operation, Operation::Stop(_))) {
                DispatchResult::Refused(Failure::Refused)
            } else { DispatchResult::Queued }
        }
        fn poll(&mut self, _ctx: &()) -> Vec<BackendEvent> { std::mem::take(&mut self.events) }
        fn discard_staged(&mut self, backend: BackendKind, attempt: AttemptId, key: SourceKey) {
            self.discarded.push((backend, attempt, key));
        }
        fn exit_disposition(&self) -> ExitDisposition { self.exit }
    }
    fn start(runner: &mut BackendRunner, drivers: &mut FakeSet, backend: BackendKind,
        attempt: AttemptId, source: SourceKey) -> Vec<Fx> {
        runner.send(drivers, &(), Intent::Start { backend, attempt, source,
            requirements: REQ, initial: INITIAL })
    }

    #[test]
    fn one_runner_dispatches_both_backends_and_preserves_terminal_order() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        assert!(start(&mut runner, &mut drivers, URI, A, SourceKey::new(1)).is_empty());
        assert_eq!(drivers.sent[0].0, URI);
        assert_eq!(runner.snapshot().playback, None, "a queue ACK is not Ready or Playing");
        drivers.events.extend([
            BackendEvent { backend: FEED, event: DriverEvent::Observation {
                attempt: A, observation: Observation::Presented } },
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Playback(Observed { value: Playback::ReadyAwaitPlay,
                    from: EvidenceSource::BackendReport }) } },
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Position(Observed { value: ContentTime::from_millis(4_000),
                    from: EvidenceSource::BackendReport }) } },
        ]);
        let notices = runner.poll(&mut drivers, &());
        assert_eq!(notices.len(), 2);
        assert!(!runner.snapshot().presented, "Ready and position do not prove a picture");
        assert_eq!(runner.snapshot().position.unwrap().value.as_millis(), 4_000);
        drivers.events.extend([
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Failed(Failure::Backend) } },
            BackendEvent { backend: URI, event: DriverEvent::Release { attempt: A,
                status: ReleaseStatus::Released } },
            BackendEvent { backend: FEED, event: DriverEvent::Release { attempt: A,
                status: ReleaseStatus::Released } },
        ]);
        assert_eq!(runner.poll(&mut drivers, &()), vec![
            Fx::AcceptedObservation { backend: URI, attempt: A,
                observation: Observation::Failed(Failure::Backend) },
            Fx::ReleaseOutcome { backend: URI, attempt: A, status: ReleaseStatus::Released },
        ]);
        assert_eq!(runner.snapshot().attempt, None);
        assert!(start(&mut runner, &mut drivers, FEED, AttemptId::new(1, 2), SourceKey::new(2)).is_empty());
        assert_eq!(drivers.sent.last().unwrap().0, FEED);
    }

    #[test]
    fn missing_backend_and_busy_start_discard_only_submitted_sources() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        drivers.uri = false;
        assert_eq!(start(&mut runner, &mut drivers, URI, A, SourceKey::new(9)),
            [Fx::Rejected { backend: URI, attempt: A, failure: Failure::Unavailable }]);
        assert_eq!(drivers.discarded, [(URI, A, SourceKey::new(9))]);
        assert!(drivers.sent.is_empty());
        assert!(start(&mut runner, &mut drivers, FEED, A, SourceKey::new(10)).is_empty());
        let b = AttemptId::new(1, 2);
        assert_eq!(start(&mut runner, &mut drivers, URI, b, SourceKey::new(11)),
            [Fx::Rejected { backend: URI, attempt: b, failure: Failure::Busy }]);
        assert_eq!(drivers.discarded.last(), Some(&(URI, b, SourceKey::new(11))));
        assert_eq!(runner.snapshot().attempt, Some(A));
    }

    #[test]
    fn refused_start_still_owes_exact_release_and_exit_consults_all_drivers() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        drivers.refuse_start = true;
        let notices = start(&mut runner, &mut drivers, URI, A, SourceKey::new(1));
        assert_eq!(notices, [Fx::CommandRefused { backend: URI, attempt: A,
            command: drivers.sent[0].2, operation: drivers.sent[0].3, failure: Failure::Refused }]);
        assert_eq!(drivers.discarded, [(URI, A, SourceKey::new(1))]);
        assert_eq!(runner.snapshot().phase, plx_playback_contract::Phase::Releasing);
        assert_eq!(runner.exit_disposition(&drivers), ExitDisposition::PreserveNativeOwnership);
        drivers.events.push(BackendEvent { backend: FEED, event: DriverEvent::Release {
            attempt: A, status: ReleaseStatus::Released } });
        assert!(runner.poll(&mut drivers, &()).is_empty());
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::Release {
            attempt: A, status: ReleaseStatus::Released } });
        runner.poll(&mut drivers, &());
        drivers.exit = ExitDisposition::PreserveNativeOwnership;
        assert_eq!(runner.exit_disposition(&drivers), ExitDisposition::PreserveNativeOwnership,
            "a vacant common snapshot cannot clear another driver's quarantine");
        drivers.exit = ExitDisposition::Clean;
        assert_eq!(runner.exit_disposition(&drivers), ExitDisposition::Clean);
    }

    struct Token { _not_send_sync: std::rc::Rc<()> }
    struct ContextSet { seen: Vec<usize> }
    impl DriverSet for ContextSet {
        type Context = Token;
        fn capabilities(&self, _backend: BackendKind) -> Option<Capabilities> { Some(CAPS) }
        fn dispatch(&mut self, _backend: BackendKind, ctx: &Token, _attempt: AttemptId,
            _command: CommandId, _operation: Operation) -> DispatchResult {
            self.seen.push(std::ptr::from_ref(ctx) as usize);
            DispatchResult::Queued
        }
        fn poll(&mut self, ctx: &Token) -> Vec<BackendEvent> {
            self.seen.push(std::ptr::from_ref(ctx) as usize);
            Vec::new()
        }
        fn discard_staged(&mut self, _backend: BackendKind, _attempt: AttemptId, _key: SourceKey) {}
        fn exit_disposition(&self) -> ExitDisposition { ExitDisposition::Clean }
    }
    #[test]
    fn object_safe_set_borrows_the_callers_non_send_context_without_caching_it() {
        let first = Token { _not_send_sync: std::rc::Rc::new(()) };
        let second = Token { _not_send_sync: std::rc::Rc::new(()) };
        let mut drivers = ContextSet { seen: Vec::new() };
        let set: &mut dyn DriverSet<Context = Token> = &mut drivers;
        let mut runner = BackendRunner::new();
        assert!(runner.send(&mut *set, &first, Intent::Start { backend: URI, attempt: A,
            source: SourceKey::new(3), requirements: REQ, initial: INITIAL }).is_empty());
        assert!(runner.snapshot().last_queued.is_some());
        assert!(runner.poll(&mut *set, &second).is_empty());
        assert_eq!(drivers.seen, [std::ptr::from_ref(&first) as usize,
            std::ptr::from_ref(&second) as usize]);
    }

    #[test]
    fn stop_reason_is_dispatched_and_queued_stop_is_not_release() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        start(&mut runner, &mut drivers, URI, A, SourceKey::new(1));
        let effects = runner.send(&mut drivers, &(), Intent::Stop { backend: URI, attempt: A,
            reason: StopReason::Exit });
        assert!(effects.is_empty());
        assert!(matches!(drivers.sent.last().unwrap().3, Operation::Stop(StopReason::Exit)));
        assert_eq!(runner.exit_disposition(&drivers), ExitDisposition::PreserveNativeOwnership);
    }

    #[test]
    fn startup_waits_for_correlated_track_and_seek_completion_before_play() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        drivers.caps.subtitle_off = true;
        let requirements = Requirements { audio_track: TrackChoice::Selected(3),
            subtitle_track: TrackChoice::Off, ..REQ };
        let initial = InitialPlayback { position: Some(ContentTime::from_millis(9_000)), paused: false };
        runner.send(&mut drivers, &(), Intent::Start { backend: URI, attempt: A,
            source: SourceKey::new(8), requirements, initial });
        assert_eq!(drivers.sent.len(), 1);
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::Observation {
            attempt: A, observation: Observation::Playback(Observed { value: Playback::ReadyAwaitPlay,
                from: EvidenceSource::BackendReport }) } });
        runner.poll(&mut drivers, &());
        let audio_command = drivers.sent.last().unwrap().2;
        let start_command = drivers.sent[0].2;
        assert_eq!(drivers.sent.last().unwrap().3, Operation::SelectTrack {
            kind: plx_playback_contract::TrackKind::Audio, choice: TrackChoice::Selected(3) });
        drivers.events.extend([
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::TrackSelected(Observed { value:
                    plx_playback_contract::TrackSelection { kind: plx_playback_contract::TrackKind::Audio, id: 3 },
                    from: EvidenceSource::BackendReport }) } },
            BackendEvent { backend: URI, event: DriverEvent::TrackSettled { attempt: A,
                command: start_command, kind: plx_playback_contract::TrackKind::Audio,
                choice: TrackChoice::Selected(3) } },
        ]);
        runner.poll(&mut drivers, &());
        assert_eq!(drivers.sent.len(), 2, "track fact and stale command cannot advance startup");
        drivers.events.push(BackendEvent { backend: FEED, event: DriverEvent::TrackSettled {
            attempt: A, command: audio_command, kind: plx_playback_contract::TrackKind::Audio,
            choice: TrackChoice::Selected(3) } });
        runner.poll(&mut drivers, &());
        assert_eq!(drivers.sent.len(), 2, "wrong physical backend cannot settle the command");
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::TrackSettled {
            attempt: A, command: audio_command, kind: plx_playback_contract::TrackKind::Audio,
            choice: TrackChoice::Selected(3) } });
        runner.poll(&mut drivers, &());
        let subtitle_command = drivers.sent.last().unwrap().2;
        assert_eq!(drivers.sent.last().unwrap().3, Operation::SelectTrack {
            kind: plx_playback_contract::TrackKind::Subtitle, choice: TrackChoice::Off });
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::Observation {
            attempt: A, observation: Observation::TrackOff(Observed {
                value: plx_playback_contract::TrackKind::Subtitle,
                from: EvidenceSource::BackendReport }) } });
        runner.poll(&mut drivers, &());
        assert_eq!(drivers.sent.len(), 3, "track-off observation alone lacks command completion");
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::TrackSettled {
            attempt: A, command: subtitle_command, kind: plx_playback_contract::TrackKind::Subtitle,
            choice: TrackChoice::Off } });
        runner.poll(&mut drivers, &());
        let seek_command = drivers.sent.last().unwrap().2;
        assert_eq!(drivers.sent.last().unwrap().3, Operation::Seek(ContentTime::from_millis(9_000)));
        drivers.events.extend([
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Position(Observed { value: ContentTime::from_millis(9_000),
                    from: EvidenceSource::BackendReport }) } },
            BackendEvent { backend: URI, event: DriverEvent::SeekLanded { attempt: A,
                command: subtitle_command, position: None } },
        ]);
        runner.poll(&mut drivers, &());
        assert_eq!(drivers.sent.len(), 4, "clock and stale seek do not start playback");
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::SeekLanded {
            attempt: A, command: seek_command, position: None } });
        runner.poll(&mut drivers, &());
        assert_eq!(drivers.sent.last().unwrap().3, Operation::Play);
        assert_eq!(drivers.sent.len(), 5);
        assert!(!runner.snapshot().presented, "Play dispatch is not a presented picture");
    }

    #[test]
    fn paused_and_background_startup_hold_play_until_caller_allows_it() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        runner.set_foreground(&mut drivers, &(), false);
        runner.send(&mut drivers, &(), Intent::Start { backend: URI, attempt: A,
            source: SourceKey::new(1), requirements: REQ, initial: INITIAL });
        assert_eq!(runner.startup_viewer_paused(URI, A), Some(false));
        runner.send(&mut drivers, &(), Intent::Control { backend: URI, attempt: A,
            operation: Operation::Pause });
        assert_eq!(runner.startup_viewer_paused(URI, A), Some(true));
        assert_eq!(runner.viewer_paused(URI, A), Some(true));
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::Observation {
            attempt: A, observation: Observation::Playback(Observed { value: Playback::ReadyAwaitPlay,
                from: EvidenceSource::BackendReport }) } });
        runner.poll(&mut drivers, &());
        assert_eq!(drivers.sent.len(), 1);
        runner.set_foreground(&mut drivers, &(), true);
        assert_eq!(drivers.sent.len(), 1, "paused initial intent survives foreground restoration");
        runner.send(&mut drivers, &(), Intent::Control { backend: URI, attempt: A,
            operation: Operation::Play });
        assert_eq!(drivers.sent.last().unwrap().3, Operation::Play);
        assert_eq!(drivers.sent.len(), 2);
        assert_eq!(runner.startup_viewer_paused(URI, A), None,
            "an intentionally paused session's later interactive Play is ordinary control");
        assert_eq!(runner.viewer_paused(URI, A), Some(false));
    }

    #[test]
    fn mandatory_initial_play_refusal_fails_but_later_transport_refusals_do_not() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        let request = LogicalRequestId::new(30);
        runner.begin_request_at(request, measure::Origin::Fresh, tick(0));
        runner.send_at(&mut drivers, &(), Intent::Start { backend: FEED, attempt: A,
            source: SourceKey::new(1), requirements: REQ, initial: INITIAL },
            measure::Origin::Fresh, tick(1));
        drivers.events.push(BackendEvent { backend: FEED, event: DriverEvent::Observation {
            attempt: A, observation: Observation::Playback(Observed { value: Playback::ReadyAwaitPlay,
                from: EvidenceSource::BackendReport }) } });
        runner.poll_at(&mut drivers, &(), tick(2));
        let initial = drivers.sent.last().unwrap().2;
        assert_eq!(runner.startup_viewer_paused(FEED, A), Some(false),
            "pending native Play still leaves the viewer's unpaused goal visible");
        assert_eq!(drivers.sent.last().unwrap().3, Operation::Play);
        drivers.events.push(BackendEvent { backend: FEED, event: DriverEvent::ControlRefused {
            attempt: A, command: initial, operation: Operation::Play, failure: Failure::Refused } });
        let refused = runner.poll_at(&mut drivers, &(), tick(3));
        assert_eq!(refused.playback, [
            Fx::CommandRefused { backend: FEED, attempt: A,
                command: initial, operation: Operation::Play, failure: Failure::Refused },
            Fx::StartupFailed { backend: FEED, attempt: A,
                command: initial, failure: Failure::Refused },
        ]);
        assert_eq!(runner.metrics_snapshot().unwrap().current_attempt.unwrap().outcome,
            Some(measure::AttemptOutcome::Failed(measure::Failure {
                stage: measure::Stage::Presentation, cause: measure::Cause::Refused })));

        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        runner.begin_request_at(LogicalRequestId::new(31), measure::Origin::Fresh, tick(0));
        runner.send_at(&mut drivers, &(), Intent::Start { backend: FEED, attempt: A,
            source: SourceKey::new(2), requirements: REQ,
            initial: InitialPlayback { paused: true, position: None } },
            measure::Origin::Fresh, tick(1));
        drivers.events.push(BackendEvent { backend: FEED, event: DriverEvent::Observation {
            attempt: A, observation: Observation::Playback(Observed { value: Playback::ReadyAwaitPlay,
                from: EvidenceSource::BackendReport }) } });
        runner.poll_at(&mut drivers, &(), tick(2));
        assert_eq!(drivers.sent.len(), 1, "paused startup issues no mandatory Play");
        runner.send_at(&mut drivers, &(), Intent::Control { backend: FEED, attempt: A,
            operation: Operation::Play }, measure::Origin::Fresh, tick(3));
        let interactive = drivers.sent.last().unwrap().2;
        drivers.events.push(BackendEvent { backend: FEED, event: DriverEvent::ControlRefused {
            attempt: A, command: interactive, operation: Operation::Play,
            failure: Failure::Refused } });
        runner.poll_at(&mut drivers, &(), tick(4));
        assert_eq!(runner.metrics_snapshot().unwrap().current_attempt.unwrap().outcome, None);
        runner.send_at(&mut drivers, &(), Intent::Control { backend: FEED, attempt: A,
            operation: Operation::Pause }, measure::Origin::Fresh, tick(5));
        let pause = drivers.sent.last().unwrap().2;
        drivers.events.push(BackendEvent { backend: FEED, event: DriverEvent::ControlRefused {
            attempt: A, command: pause, operation: Operation::Pause,
            failure: Failure::Refused } });
        runner.poll_at(&mut drivers, &(), tick(6));
        assert_eq!(runner.metrics_snapshot().unwrap().current_attempt.unwrap().outcome, None);
        let before = drivers.sent.len();
        runner.send(&mut drivers, &(), Intent::Control { backend: FEED, attempt: A,
            operation: Operation::Play });
        assert_eq!(drivers.sent.len(), before + 1, "later refusal did not poison the startup plan");
    }

    #[test]
    fn mandatory_track_refusal_is_terminal_but_later_track_refusal_is_not() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        let requirements = Requirements { audio_track: TrackChoice::Selected(3), ..REQ };
        runner.begin_request_at(LogicalRequestId::new(41), measure::Origin::Fresh, tick(0));
        runner.send_at(&mut drivers, &(), Intent::Start { backend: URI, attempt: A,
            source: SourceKey::new(41), requirements, initial: INITIAL },
            measure::Origin::Fresh, tick(1));
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::Observation {
            attempt: A, observation: Observation::Playback(Observed {
                value: Playback::ReadyAwaitPlay, from: EvidenceSource::BackendReport }) } });
        runner.poll_at(&mut drivers, &(), tick(2));
        let (command, operation) = (drivers.sent.last().unwrap().2, drivers.sent.last().unwrap().3);
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::ControlRefused {
            attempt: A, command, operation, failure: Failure::Refused } });
        let result = runner.poll_at(&mut drivers, &(), tick(3));
        assert!(result.playback.contains(&Fx::StartupFailed { backend: URI, attempt: A,
            command, failure: Failure::Refused }));
        assert!(runner.metrics_snapshot().unwrap().current_attempt.unwrap().outcome.is_some());

        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        runner.begin_request_at(LogicalRequestId::new(42), measure::Origin::Fresh, tick(0));
        runner.send_at(&mut drivers, &(), Intent::Start { backend: URI, attempt: A,
            source: SourceKey::new(42), requirements: REQ, initial: INITIAL },
            measure::Origin::Fresh, tick(1));
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::Observation {
            attempt: A, observation: Observation::Playback(Observed {
                value: Playback::ReadyAwaitPlay, from: EvidenceSource::BackendReport }) } });
        runner.poll_at(&mut drivers, &(), tick(2));
        let initial_play = drivers.sent.last().unwrap().2;
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::ControlAccepted {
            attempt: A, command: initial_play, operation: Operation::Play,
            native_acknowledged: true } });
        runner.poll_at(&mut drivers, &(), tick(3));
        let operation = Operation::SelectTrack { kind: plx_playback_contract::TrackKind::Audio,
            choice: TrackChoice::Selected(3) };
        runner.send_at(&mut drivers, &(), Intent::Control { backend: URI, attempt: A,
            operation }, measure::Origin::Fresh, tick(4));
        let command = drivers.sent.last().unwrap().2;
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::ControlRefused {
            attempt: A, command, operation, failure: Failure::Refused } });
        let result = runner.poll_at(&mut drivers, &(), tick(5));
        assert!(!result.playback.iter().any(|notice| matches!(notice, Fx::StartupFailed { .. })));
        assert_eq!(runner.metrics_snapshot().unwrap().current_attempt.unwrap().outcome, None);
    }
    /// The URI driver no longer closes itself when a mandatory startup control never confirms
    /// (it reports `ControlRefused` with `Failure::TimedOut`); this verdict is what ends the attempt.
    #[test]
    fn mandatory_track_timeout_is_terminal_like_a_refusal() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        let requirements = Requirements { audio_track: TrackChoice::Selected(3), ..REQ };
        runner.begin_request_at(LogicalRequestId::new(43), measure::Origin::Fresh, tick(0));
        runner.send_at(&mut drivers, &(), Intent::Start { backend: URI, attempt: A,
            source: SourceKey::new(43), requirements, initial: INITIAL },
            measure::Origin::Fresh, tick(1));
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::Observation {
            attempt: A, observation: Observation::Playback(Observed {
                value: Playback::ReadyAwaitPlay, from: EvidenceSource::BackendReport }) } });
        runner.poll_at(&mut drivers, &(), tick(2));
        let (command, operation) = (drivers.sent.last().unwrap().2, drivers.sent.last().unwrap().3);
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::ControlRefused {
            attempt: A, command, operation, failure: Failure::TimedOut } });
        let result = runner.poll_at(&mut drivers, &(), tick(3));
        assert!(result.playback.contains(&Fx::StartupFailed { backend: URI, attempt: A,
            command, failure: Failure::TimedOut }));
    }
    fn tick(ms: u64) -> measure::Tick { measure::Tick::from_millis(ms) }

    #[test]
    fn feed_native_ack_and_position_progress_do_not_invent_playback_or_picture() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        runner.begin_request_at(LogicalRequestId::new(40), measure::Origin::Fresh, tick(0));
        runner.send_at(&mut drivers, &(), Intent::Start { backend: FEED, attempt: A,
            source: SourceKey::new(40), requirements: REQ, initial: INITIAL },
            measure::Origin::Fresh, tick(1));
        drivers.events.push(BackendEvent { backend: FEED, event: DriverEvent::Observation {
            attempt: A, observation: Observation::Playback(Observed {
                value: Playback::ReadyAwaitPlay, from: EvidenceSource::BackendReport }) } });
        runner.poll_at(&mut drivers, &(), tick(2));
        let play = drivers.sent.last().unwrap().2;
        assert_eq!(drivers.sent.last().unwrap().3, Operation::Play);
        drivers.events.extend([
            BackendEvent { backend: FEED, event: DriverEvent::ControlAccepted {
                attempt: A, command: play, operation: Operation::Play,
                native_acknowledged: true } },
            BackendEvent { backend: FEED, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Position(Observed { value: ContentTime::from_millis(100),
                    from: EvidenceSource::BackendReport }) } },
            BackendEvent { backend: FEED, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Position(Observed { value: ContentTime::from_millis(200),
                    from: EvidenceSource::BackendReport }) } },
        ]);
        let notices = runner.poll_at(&mut drivers, &(), tick(3));
        assert!(matches!(notices.playback.first(), Some(Fx::AcceptedControl {
            backend: FEED, attempt: A, command, operation: Operation::Play,
            native_acknowledged: true }) if *command == play));
        let evidence = runner.metrics_snapshot().unwrap().current_attempt.unwrap().evidence;
        assert!(evidence.command_acknowledged && evidence.clock_advanced);
        assert!(!evidence.playback_observed && !evidence.picture_presented);
        assert_eq!(runner.snapshot().playback.unwrap().value, Playback::ReadyAwaitPlay);
        assert!(!runner.snapshot().presented);
    }

    #[test]
    fn metrics_keep_picture_separate_and_terminal_failure_survives_same_poll_release() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        let request = LogicalRequestId::new(7);
        assert!(runner.begin_request_at(request, measure::Origin::Fresh, tick(0)).metrics.is_empty());
        let start = Intent::Start { backend: URI, attempt: A, source: SourceKey::new(19),
            requirements: REQ, initial: INITIAL };
        assert!(runner.send_at(&mut drivers, &(), start, measure::Origin::Fresh, tick(1)).metrics.is_empty());
        drivers.events.extend([
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Playback(Observed { value: Playback::ReadyAwaitPlay,
                    from: EvidenceSource::BackendReport }) } },
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Playback(Observed { value: Playback::Playing,
                    from: EvidenceSource::BackendReport }) } },
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Position(Observed { value: ContentTime::from_millis(100),
                    from: EvidenceSource::BackendReport }) } },
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Position(Observed { value: ContentTime::from_millis(200),
                    from: EvidenceSource::BackendReport }) } },
        ]);
        runner.poll_at(&mut drivers, &(), tick(5));
        let evidence = runner.metrics_snapshot().unwrap().current_attempt.unwrap().evidence;
        assert!(evidence.playback_observed && evidence.clock_advanced);
        assert!(!evidence.command_acknowledged, "queued dispatch is not a native ACK");
        assert!(!evidence.picture_presented);
        assert_eq!(runner.metrics_snapshot().unwrap().startup_ms, None);
        drivers.events.extend([
            BackendEvent { backend: FEED, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Presented } },
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Presented } },
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Failed(Failure::Backend) } },
            BackendEvent { backend: URI, event: DriverEvent::Release { attempt: A,
                status: ReleaseStatus::Released } },
        ]);
        let polled = runner.poll_at(&mut drivers, &(), tick(10));
        assert!(matches!(polled.metrics.as_slice(), [MetricNotice::Measured(measure::Fx::Incident(
            measure::Record { request: r, backend: measure::Backend::Uri, attempt: A,
                stage: measure::Stage::Decode,
                incident: measure::Incident::Failed(measure::Cause::Native), .. }))] if *r == request),
            "one failure record; a clean Released verdict records nothing");
        let summary = runner.metrics_snapshot().unwrap();
        assert_eq!(summary.startup_ms, Some(10));
        assert_eq!(summary.current_attempt.unwrap().outcome, Some(measure::AttemptOutcome::Failed(
            measure::Failure { stage: measure::Stage::Decode, cause: measure::Cause::Native })));
        let failure = measure::Failure { stage: measure::Stage::Decode, cause: measure::Cause::Native };
        let terminal = runner.finish_request_at(request, measure::RequestOutcome::Failed(failure), tick(11));
        assert!(matches!(terminal.metrics.as_slice(), [MetricNotice::Measured(measure::Fx::HandledError {
            attempt: Some(A), .. }), MetricNotice::Measured(measure::Fx::Finished(_))]));
        assert!(runner.finish_request_at(request, measure::RequestOutcome::Failed(failure), tick(12)).metrics.is_empty());
    }

    #[test]
    fn recovered_refusal_needs_second_attempt_picture_and_preflight_has_no_release() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        let request = LogicalRequestId::new(8);
        runner.begin_request_at(request, measure::Origin::Fresh, tick(0));
        drivers.uri = false;
        let preflight = runner.send_at(&mut drivers, &(), Intent::Start { backend: URI,
            attempt: A, source: SourceKey::new(9), requirements: REQ, initial: INITIAL },
            measure::Origin::Fresh, tick(1));
        assert!(matches!(preflight.metrics.as_slice(), [MetricNotice::Measured(measure::Fx::Incident(
            measure::Record { stage: measure::Stage::Open,
                incident: measure::Incident::Failed(measure::Cause::Unsupported), .. }))]));
        assert_eq!(runner.metrics_snapshot().unwrap().attempt_count, 0);
        assert!(drivers.sent.is_empty(), "no physical Start or release is owed");
        drivers.uri = true;
        drivers.refuse_start = true;
        runner.send_at(&mut drivers, &(), Intent::Start { backend: URI, attempt: A,
            source: SourceKey::new(10), requirements: REQ, initial: INITIAL },
            measure::Origin::Fresh, tick(2));
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Released } });
        runner.poll_at(&mut drivers, &(), tick(3));
        let successor = AttemptId::new(1, 2);
        let handoff = runner.handoff_at(request, A, successor, tick(4));
        assert!(matches!(handoff.metrics.as_slice(), [MetricNotice::Measured(measure::Fx::Incident(
            measure::Record { attempt: A, backend: measure::Backend::Uri,
                stage: measure::Stage::Open, incident: measure::Incident::Handoff, .. }))]),
            "the failed first attempt's stage labels the handoff");
        drivers.refuse_start = false;
        runner.send_at(&mut drivers, &(), Intent::Start { backend: FEED, attempt: successor,
            source: SourceKey::new(11), requirements: REQ, initial: INITIAL },
            measure::Origin::Recovery, tick(5));
        drivers.events.extend([
            BackendEvent { backend: FEED, event: DriverEvent::Observation { attempt: successor,
                observation: Observation::Playback(Observed { value: Playback::Playing,
                    from: EvidenceSource::BackendReport }) } },
            BackendEvent { backend: FEED, event: DriverEvent::Observation { attempt: successor,
                observation: Observation::Presented } },
        ]);
        let recovered = runner.poll_at(&mut drivers, &(), tick(6));
        assert!(matches!(recovered.metrics.as_slice(), [MetricNotice::Measured(
            measure::Fx::RecoveredRefusal { refused_attempt: A, recovered_attempt, .. })]
            if *recovered_attempt == successor));
        let summary = runner.metrics_snapshot().unwrap();
        assert_eq!(summary.attempt_count, 2);
        assert_eq!(summary.current_attempt.unwrap().origin, measure::Origin::Recovery);
        assert_eq!(summary.recent_attempts[0].outcome, Some(measure::AttemptOutcome::Failed(
            measure::Failure { stage: measure::Stage::Open, cause: measure::Cause::Refused })));
    }

    #[test]
    fn intentional_pause_seek_and_background_exclude_rebuffer_and_unknown_time_is_visible() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        let request = LogicalRequestId::new(9);
        runner.begin_request_at(request, measure::Origin::Fresh, tick(0));
        runner.send_at(&mut drivers, &(), Intent::Start { backend: URI, attempt: A,
            source: SourceKey::new(1), requirements: REQ, initial: INITIAL },
            measure::Origin::Fresh, tick(0));
        drivers.events.extend([
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Playback(Observed { value: Playback::ReadyAwaitPlay,
                    from: EvidenceSource::BackendReport }) } },
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Presented } },
        ]);
        runner.poll_at(&mut drivers, &(), tick(10));
        runner.condition_at(URI, A, measure::Condition::Rebuffering, tick(20));
        runner.send_at(&mut drivers, &(), Intent::Control { backend: URI, attempt: A,
            operation: Operation::Pause }, measure::Origin::Fresh, tick(30));
        runner.condition_at(URI, A, measure::Condition::Rebuffering, tick(40));
        runner.send_at(&mut drivers, &(), Intent::Control { backend: URI, attempt: A,
            operation: Operation::Seek(ContentTime::from_millis(2_000)) },
            measure::Origin::Fresh, tick(50));
        runner.set_foreground_at(&mut drivers, &(), false, tick(60));
        runner.condition_at(URI, A, measure::Condition::Rebuffering, tick(70));
        runner.condition_at(URI, A, measure::Condition::Unknown, measure::Tick::Unknown);
        runner.set_foreground_at(&mut drivers, &(), true, tick(80));
        let summary = runner.metrics_snapshot().unwrap();
        assert_eq!(summary.rebuffer_ms, 10);
        assert_eq!(summary.unknown_time_events, 1);
    }
    #[test]
    fn control_receipt_names_its_exact_dispatch_without_exposing_start_source() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        let initial = InitialPlayback { position: None, paused: true };
        let started = runner.send_at(&mut drivers, &(), Intent::Start { backend: URI,
            attempt: A, source: SourceKey::new(81), requirements: REQ, initial },
            measure::Origin::Fresh, tick(0));
        assert!(started.controls.is_empty(), "source escrow is not a control receipt");
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::Observation {
            attempt: A, observation: Observation::Playback(Observed {
                value: Playback::ReadyAwaitPlay, from: EvidenceSource::BackendReport }) } });
        let ready = runner.poll_at(&mut drivers, &(), tick(1));
        assert!(ready.controls.is_empty(), "a paused goal does not autoplay");
        let operation = Operation::SelectTrack { kind: plx_playback_contract::TrackKind::Audio,
            choice: TrackChoice::Selected(42) };
        let selected = runner.send_at(&mut drivers, &(), Intent::Control { backend: URI,
            attempt: A, operation }, measure::Origin::Fresh, tick(2));
        assert_eq!(selected.controls, [ControlDispatch { backend: URI, attempt: A,
            command: drivers.sent.last().unwrap().2, operation }]);
        assert!(selected.playback.is_empty(), "dispatch receipt is not native acceptance");
    }

    #[test]
    fn measured_handoff_and_rebuffer_share_their_overlapping_time() {
        let mut runner = BackendRunner::new();
        let mut drivers = FakeSet::new();
        let request = LogicalRequestId::new(10);
        runner.begin_request_at(request, measure::Origin::Fresh, tick(0));
        runner.send_at(&mut drivers, &(), Intent::Start { backend: URI, attempt: A,
            source: SourceKey::new(1), requirements: REQ, initial: INITIAL },
            measure::Origin::Fresh, tick(0));
        drivers.events.extend([
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Playback(Observed { value: Playback::ReadyAwaitPlay,
                    from: EvidenceSource::BackendReport }) } },
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Playback(Observed { value: Playback::Playing,
                    from: EvidenceSource::BackendReport }) } },
            BackendEvent { backend: URI, event: DriverEvent::Observation { attempt: A,
                observation: Observation::Presented } },
        ]);
        runner.poll_at(&mut drivers, &(), tick(10));
        runner.condition_at(URI, A, measure::Condition::Rebuffering, tick(20));
        let successor = AttemptId::new(1, 2);
        runner.handoff_at(request, A, successor, tick(30));
        runner.handoff_at(request, A, successor, tick(31));
        runner.send_at(&mut drivers, &(), Intent::Stop { backend: URI, attempt: A,
            reason: StopReason::Replace }, measure::Origin::AutomaticHandoff, tick(35));
        drivers.events.push(BackendEvent { backend: URI, event: DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Released } });
        runner.poll_at(&mut drivers, &(), tick(36));
        runner.send_at(&mut drivers, &(), Intent::Start { backend: FEED, attempt: successor,
            source: SourceKey::new(2), requirements: REQ, initial: INITIAL },
            measure::Origin::AutomaticHandoff, tick(40));
        drivers.events.extend([
            BackendEvent { backend: FEED, event: DriverEvent::Observation { attempt: successor,
                observation: Observation::Playback(Observed { value: Playback::Playing,
                    from: EvidenceSource::BackendReport }) } },
            BackendEvent { backend: FEED, event: DriverEvent::Observation { attempt: successor,
                observation: Observation::Presented } },
        ]);
        runner.poll_at(&mut drivers, &(), tick(50));
        let summary = runner.metrics_snapshot().unwrap();
        assert_eq!(summary.handoff_count, 1);
        assert_eq!(summary.handoff_gap_ms, 20);
        assert_eq!(summary.rebuffer_ms, 30);
        assert_eq!(summary.involuntary_stall_ms, 30, "overlap is counted once");
    }

    // --- W4: one typed record per failed start, rebuffer, handoff and release failure ---------
    // These run the shared runner against a scripted DriverSet for EACH BackendKind. They prove
    // the reducer, the wrapper and that both drivers' events take the same path; the real
    // drivers' own event vocabulary is covered beside them (play-uri backend rig, the Feed driver,
    // run_uri_boot_tests). The scripted sets inject `Observation::Presented` where the real
    // drivers emit it: the Feed driver's `note_picture` (the displayed-frame counter after the bind) and
    // `UriBackend::picture_step` (`play-uri/src/picture_gate.rs`: the service's `playing`, the plane
    // owner's window+PLAYING completions, then a clock advance; ACB binding only).
    type Rec = measure::Record<LogicalRequestId, AttemptId>;
    fn incidents(result: &RunResult) -> Vec<Rec> {
        result.metrics.iter().filter_map(|n| match n {
            MetricNotice::Measured(measure::Fx::Incident(rec)) => Some(*rec), _ => None }).collect()
    }
    fn mb(backend: BackendKind) -> measure::Backend { metrics::map_backend(backend) }
    fn observe(backend: BackendKind, attempt: AttemptId, observation: Observation) -> BackendEvent {
        BackendEvent { backend, event: DriverEvent::Observation { attempt, observation } }
    }
    fn playing(backend: BackendKind, attempt: AttemptId) -> Vec<BackendEvent> {
        let seen = |value| Observation::Playback(Observed { value, from: EvidenceSource::BackendReport });
        vec![observe(backend, attempt, seen(Playback::ReadyAwaitPlay)),
            observe(backend, attempt, seen(Playback::Playing)),
            observe(backend, attempt, Observation::Presented)]
    }
    fn begin(runner: &mut BackendRunner, id: u64) -> LogicalRequestId {
        let request = LogicalRequestId::new(id);
        runner.begin_request_at(request, measure::Origin::Fresh, tick(0));
        request
    }
    fn go(runner: &mut BackendRunner, drivers: &mut FakeSet, backend: BackendKind,
        attempt: AttemptId, origin: measure::Origin, at: u64) -> RunResult {
        runner.send_at(drivers, &(), Intent::Start { backend, attempt, source: SourceKey::new(at),
            requirements: REQ, initial: INITIAL }, origin, tick(at))
    }
    fn stop(runner: &mut BackendRunner, drivers: &mut FakeSet, backend: BackendKind,
        attempt: AttemptId, at: u64) -> RunResult {
        runner.send_at(drivers, &(), Intent::Stop { backend, attempt, reason: StopReason::Exit },
            measure::Origin::Fresh, tick(at))
    }

    fn failed_start_is_one_record(backend: BackendKind) {
        let (mut runner, mut drivers) = (BackendRunner::new(), FakeSet::new());
        let request = begin(&mut runner, 1);
        drivers.refuse_start = true;
        let refused = incidents(&go(&mut runner, &mut drivers, backend, A, measure::Origin::Fresh, 1));
        assert_eq!(refused, [Rec { request, attempt: A, backend: mb(backend), stage: measure::Stage::Open,
            incident: measure::Incident::Failed(measure::Cause::Refused), at: tick(1) }]);
        drivers.events.push(BackendEvent { backend, event: DriverEvent::Release {
            attempt: A, status: ReleaseStatus::Released } });
        assert!(incidents(&runner.poll_at(&mut drivers, &(), tick(2))).is_empty(),
            "the clean release of a failed start adds nothing");
        // A Start refused before it reaches the driver is the same record, with no release owed.
        drivers.refuse_start = false;
        match backend { URI => drivers.uri = false, FEED => drivers.feed = false }
        let b = AttemptId::new(1, 2);
        let rejected = incidents(&go(&mut runner, &mut drivers, backend, b, measure::Origin::Fresh, 3));
        assert_eq!(rejected, [Rec { request, attempt: b, backend: mb(backend), stage: measure::Stage::Open,
            incident: measure::Incident::Failed(measure::Cause::Unsupported), at: tick(3) }]);
        // An asynchronous failure before Ready is a failed start too (stage Open).
        match backend { URI => drivers.uri = true, FEED => drivers.feed = true }
        let c = AttemptId::new(1, 3);
        go(&mut runner, &mut drivers, backend, c, measure::Origin::Fresh, 4);
        drivers.events.push(observe(backend, c, Observation::Failed(Failure::Backend)));
        let late = incidents(&runner.poll_at(&mut drivers, &(), tick(5)));
        assert_eq!(late, [Rec { request, attempt: c, backend: mb(backend), stage: measure::Stage::Open,
            incident: measure::Incident::Failed(measure::Cause::Native), at: tick(5) }]);
    }
    #[test] fn uri_failed_start_is_exactly_one_typed_record() { failed_start_is_one_record(URI); }
    #[test] fn feed_failed_start_is_exactly_one_typed_record() { failed_start_is_one_record(FEED); }

    fn rebuffer_is_one_record_per_episode(backend: BackendKind) {
        let (mut runner, mut drivers) = (BackendRunner::new(), FakeSet::new());
        let request = begin(&mut runner, 2);
        go(&mut runner, &mut drivers, backend, A, measure::Origin::Fresh, 1);
        drivers.events.extend(playing(backend, A));
        runner.poll_at(&mut drivers, &(), tick(5));
        // The first `Presented` is what makes startup a number and a stall a rebuffer.
        let summary = runner.metrics_snapshot().unwrap();
        assert_eq!(summary.startup_ms, Some(5), "request began at tick 0");
        assert_eq!(summary.current_attempt.unwrap().startup_ms, Some(4), "attempt began at tick 1");
        let stall = |runner: &mut BackendRunner, at| incidents(
            &runner.condition_at(backend, A, measure::Condition::Rebuffering, tick(at)));
        assert_eq!(stall(&mut runner, 10), [Rec { request, attempt: A, backend: mb(backend),
            stage: measure::Stage::Decode, incident: measure::Incident::Rebuffer, at: tick(10) }]);
        assert!(stall(&mut runner, 20).is_empty(), "the same episode is not a second record");
        runner.condition_at(backend, A, measure::Condition::Playing, tick(30));
        assert_eq!(stall(&mut runner, 40).len(), 1, "a new episode is a new record");
        assert_eq!(runner.metrics_snapshot().unwrap().rebuffer_count, 2);
        // Unknown time or an unknown condition is never charged or recorded as a rebuffer.
        assert!(incidents(&runner.condition_at(backend, A, measure::Condition::Unknown,
            measure::Tick::Unknown)).is_empty());
    }
    #[test] fn uri_rebuffer_is_exactly_one_typed_record_per_episode() { rebuffer_is_one_record_per_episode(URI); }
    #[test] fn feed_rebuffer_is_exactly_one_typed_record_per_episode() { rebuffer_is_one_record_per_episode(FEED); }

    /// Production only hands URI to Feed; the runner itself is direction-neutral, so each
    /// driver is exercised as the failed source.
    fn handoff_is_one_record(from: BackendKind, to: BackendKind, begun_first: bool) {
        let (mut runner, mut drivers) = (BackendRunner::new(), FakeSet::new());
        let request = begin(&mut runner, 3);
        go(&mut runner, &mut drivers, from, A, measure::Origin::Fresh, 1);
        drivers.events.extend(playing(from, A));
        drivers.events.push(observe(from, A, Observation::Failed(Failure::Backend)));
        runner.poll_at(&mut drivers, &(), tick(5));
        let expected = Rec { request, attempt: A, backend: mb(from), stage: measure::Stage::Decode,
            incident: measure::Incident::Handoff, at: tick(6) };
        let b = AttemptId::new(1, 2);
        if begun_first {
            assert_eq!(incidents(&runner.handoff_begun_at(request, A, tick(6))), [expected]);
        }
        drivers.events.extend([BackendEvent { backend: from, event: DriverEvent::Release {
            attempt: A, status: ReleaseStatus::Released } }]);
        runner.poll_at(&mut drivers, &(), tick(7));
        let started = incidents(&go(&mut runner, &mut drivers, to, b, measure::Origin::Recovery, 6));
        // Starting the successor completes the same handoff; it must not count a second one.
        assert_eq!(started, if begun_first { vec![] } else { vec![expected] });
        assert_eq!(runner.metrics_snapshot().unwrap().handoff_count, 1);
        assert_eq!(runner.metrics_snapshot().unwrap().current_attempt.unwrap().backend, mb(to));
    }
    #[test] fn uri_handoff_is_exactly_one_typed_record_begun_first() { handoff_is_one_record(URI, FEED, true); }
    #[test] fn uri_handoff_is_exactly_one_typed_record_at_successor_start() { handoff_is_one_record(URI, FEED, false); }
    #[test] fn feed_handoff_is_exactly_one_typed_record_begun_first() { handoff_is_one_record(FEED, URI, true); }
    #[test] fn feed_handoff_is_exactly_one_typed_record_at_successor_start() { handoff_is_one_record(FEED, URI, false); }

    fn release_failure_is_one_record(backend: BackendKind) {
        let release = Rec { request: LogicalRequestId::new(4), attempt: A, backend: mb(backend),
            stage: measure::Stage::Release,
            incident: measure::Incident::Failed(measure::Cause::Native), at: tick(0) };
        let quarantine = BackendEvent { backend, event: DriverEvent::Release {
            attempt: A, status: ReleaseStatus::Quarantined } };
        // 1. Quarantined verdict while the request is current.
        let (mut runner, mut drivers) = (BackendRunner::new(), FakeSet::new());
        begin(&mut runner, 4);
        go(&mut runner, &mut drivers, backend, A, measure::Origin::Fresh, 1);
        drivers.events.extend(playing(backend, A));
        runner.poll_at(&mut drivers, &(), tick(2));
        stop(&mut runner, &mut drivers, backend, A, 3);
        drivers.events.push(quarantine);
        assert_eq!(incidents(&runner.poll_at(&mut drivers, &(), tick(4))),
            [Rec { at: tick(4), ..release }]);
        // 2. The same verdict after the logical request finished still names that request.
        let (mut runner, mut drivers) = (BackendRunner::new(), FakeSet::new());
        let request = begin(&mut runner, 4);
        go(&mut runner, &mut drivers, backend, A, measure::Origin::Fresh, 1);
        drivers.events.extend(playing(backend, A));
        runner.poll_at(&mut drivers, &(), tick(2));
        stop(&mut runner, &mut drivers, backend, A, 3);
        runner.finish_request_at(request, measure::RequestOutcome::Cancelled, tick(3));
        drivers.events.push(quarantine);
        assert_eq!(incidents(&runner.poll_at(&mut drivers, &(), tick(4))), [Rec { at: tick(4), ..release }]);
        // 3. ...and after a NEWER request began, the old request still owns the record.
        let (mut runner, mut drivers) = (BackendRunner::new(), FakeSet::new());
        begin(&mut runner, 4);
        go(&mut runner, &mut drivers, backend, A, measure::Origin::Fresh, 1);
        runner.poll_at(&mut drivers, &(), tick(2));
        stop(&mut runner, &mut drivers, backend, A, 3);
        runner.begin_request_at(LogicalRequestId::new(5), measure::Origin::Fresh, tick(3));
        drivers.events.push(quarantine);
        assert_eq!(incidents(&runner.poll_at(&mut drivers, &(), tick(4))), [Rec { at: tick(4), ..release }]);
        // 4. A Stop the driver refuses is the failure; the verdict that follows is not a second one.
        let (mut runner, mut drivers) = (BackendRunner::new(), FakeSet::new());
        begin(&mut runner, 4);
        go(&mut runner, &mut drivers, backend, A, measure::Origin::Fresh, 1);
        drivers.events.extend(playing(backend, A));
        runner.poll_at(&mut drivers, &(), tick(2));
        drivers.refuse_stop = true;
        assert_eq!(incidents(&stop(&mut runner, &mut drivers, backend, A, 3)),
            [Rec { at: tick(3), incident: measure::Incident::Failed(measure::Cause::Refused), ..release }]);
        drivers.events.push(quarantine);
        assert!(incidents(&runner.poll_at(&mut drivers, &(), tick(4))).is_empty());
        // 5. A Stop no driver can accept leaves the owner quarantined: also one release failure.
        let (mut runner, mut drivers) = (BackendRunner::new(), FakeSet::new());
        begin(&mut runner, 4);
        go(&mut runner, &mut drivers, backend, A, measure::Origin::Fresh, 1);
        match backend { URI => drivers.uri = false, FEED => drivers.feed = false }
        assert_eq!(incidents(&stop(&mut runner, &mut drivers, backend, A, 3)),
            [Rec { at: tick(3), incident: measure::Incident::Failed(measure::Cause::Unsupported), ..release }]);
        assert_eq!(runner.snapshot().phase, plx_playback_contract::Phase::Quarantined);
        // 6. A clean release records nothing.
        let (mut runner, mut drivers) = (BackendRunner::new(), FakeSet::new());
        begin(&mut runner, 4);
        go(&mut runner, &mut drivers, backend, A, measure::Origin::Fresh, 1);
        stop(&mut runner, &mut drivers, backend, A, 3);
        drivers.events.push(BackendEvent { backend, event: DriverEvent::Release {
            attempt: A, status: ReleaseStatus::Released } });
        assert!(incidents(&runner.poll_at(&mut drivers, &(), tick(4))).is_empty());
    }
    #[test] fn uri_release_failure_is_exactly_one_typed_record() { release_failure_is_one_record(URI); }
    #[test] fn feed_release_failure_is_exactly_one_typed_record() { release_failure_is_one_record(FEED); }
}
