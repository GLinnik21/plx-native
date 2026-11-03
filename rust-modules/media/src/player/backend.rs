//! One backend-neutral state runner for all physical playback drivers. The app composition owns
//! concrete source escrow and persistent drivers; this runner keeps only pure attempt state.
#[path = "backend_startup.rs"]
mod startup;
#[path = "backend_metrics.rs"]
mod metrics;

use plx_playback_contract::{BackendState, DriverSet, Event, ExitDisposition, Failure, Fx,
    Intent, Operation, Phase, Snapshot, AttemptId, BackendKind};
use super::attempt_metrics as measure;
pub use metrics::{LogicalRequestId, MetricNotice};
use startup::Startup;

/// Immediate caller-owned results. Metric notices are local values; no report is sent here.
#[derive(Default)]
pub struct RunResult { pub playback: Vec<Fx>, pub metrics: Vec<MetricNotice> }

pub struct BackendRunner { state: BackendState, startup: Startup, metrics: metrics::Metrics }

impl BackendRunner {
    pub fn new() -> Self { Self { state: BackendState::new(), startup: Startup::new(),
        metrics: metrics::Metrics::new() } }

    pub fn snapshot(&self) -> &Snapshot { self.state.snapshot() }

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
                    self.advance(drivers, ctx, at, &mut result);
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
                    | Fx::AcceptedObservation { .. } | Fx::AcceptedTrackSettled { .. }
                    | Fx::AcceptedSeekLanded { .. } | Fx::ReleaseOutcome { .. }) => {
                    self.startup.on_notice(&notice);
                    if let (Event::Intent(Intent::Start { .. }), Fx::Rejected { backend, attempt, failure })
                        = (event, notice) {
                        self.metrics.rejected_start(backend, attempt, failure, &mut result.metrics);
                    }
                    self.metrics.notice(&notice, at, &mut result.metrics);
                    result.playback.push(notice);
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
        exit: ExitDisposition,
    }
    impl FakeSet {
        fn new() -> Self {
            Self { uri: true, feed: true, caps: CAPS, sent: Vec::new(), events: Vec::new(),
                discarded: Vec::new(), refuse_start: false, exit: ExitDisposition::Clean }
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
            if self.refuse_start && matches!(operation, Operation::Start { .. }) {
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
        runner.send(&mut drivers, &(), Intent::Control { backend: URI, attempt: A,
            operation: Operation::Pause });
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
    }
    fn tick(ms: u64) -> measure::Tick { measure::Tick::from_millis(ms) }

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
        assert!(matches!(polled.metrics.as_slice(), [MetricNotice::Release {
            request: r, backend: URI, attempt: A, status: ReleaseStatus::Released }] if *r == request));
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
        assert!(matches!(preflight.metrics.as_slice(), [MetricNotice::PreflightRejected {
            failure: measure::Failure { stage: measure::Stage::Open, cause: measure::Cause::Unsupported }, .. }]));
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
        assert!(handoff.metrics.is_empty());
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
}
