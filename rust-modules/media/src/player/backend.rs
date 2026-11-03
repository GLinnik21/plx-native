//! One backend-neutral state runner for all physical playback drivers. The app composition owns
//! concrete source escrow and persistent drivers; this runner keeps only pure attempt state.
use plx_playback_contract::{BackendState, DriverSet, Event, ExitDisposition, Fx, Intent, Snapshot};

pub struct BackendRunner { state: BackendState }

impl BackendRunner {
    pub fn new() -> Self { Self { state: BackendState::new() } }

    pub fn snapshot(&self) -> &Snapshot { self.state.snapshot() }

    /// Apply one route/player intent and its synchronous queue answer. Rejected Starts dispose
    /// only the exact staged source key, even while another backend owns the active attempt.
    pub fn send<C>(&mut self, drivers: &mut dyn DriverSet<Context = C>, ctx: &C, intent: Intent) -> Vec<Fx> {
        let capabilities = drivers.capabilities(intent.backend());
        self.feed(drivers, ctx, Event::Intent(intent), capabilities)
    }

    /// Accepted observations, seek landings and release verdicts remain in driver poll order,
    /// including terminal evidence before a same-poll release resets the sparse snapshot.
    pub fn poll<C>(&mut self, drivers: &mut dyn DriverSet<Context = C>, ctx: &C) -> Vec<Fx> {
        let mut notices = Vec::new();
        for event in drivers.poll(ctx) {
            notices.extend(self.feed(drivers, ctx, event.into(), None));
        }
        notices
    }

    /// Both the common attempt and every physical driver must be released before exit is clean.
    pub fn exit_disposition<C>(&self, drivers: &dyn DriverSet<Context = C>) -> ExitDisposition {
        if self.state.exit_disposition() == ExitDisposition::Clean
            && drivers.exit_disposition() == ExitDisposition::Clean { ExitDisposition::Clean }
        else { ExitDisposition::PreserveNativeOwnership }
    }

    fn feed<C>(&mut self, drivers: &mut dyn DriverSet<Context = C>, ctx: &C,
        event: Event, capabilities: Option<plx_playback_contract::Capabilities>) -> Vec<Fx> {
        let mut effects = Vec::new();
        self.state.step(event, capabilities, &mut effects);
        let mut notices = Vec::new();
        for effect in effects {
            match effect {
                Fx::Dispatch { backend, attempt, command, operation } => {
                    let result = drivers.dispatch(backend, ctx, attempt, command, operation);
                    notices.extend(self.feed(drivers, ctx, Event::CommandAnswer {
                        backend, attempt, command, result,
                    }, None));
                }
                Fx::DiscardStaged { backend, attempt, key } => {
                    drivers.discard_staged(backend, attempt, key);
                }
                notice @ (Fx::Rejected { .. } | Fx::CommandRefused { .. }
                    | Fx::AcceptedObservation { .. } | Fx::AcceptedSeekLanded { .. }
                    | Fx::ReleaseOutcome { .. }) => notices.push(notice),
            }
        }
        notices
    }
}
impl Default for BackendRunner { fn default() -> Self { Self::new() } }

#[cfg(test)]
mod tests {
    use super::*;
    use plx_playback_contract::{AttemptId, BackendEvent, BackendKind, Capabilities, CommandId,
        ContentTime, DispatchResult, DriverEvent, EvidenceSource, Failure, Fidelity,
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
    const A: AttemptId = AttemptId::new(1, 1);

    struct FakeSet {
        uri: bool,
        feed: bool,
        sent: Vec<(BackendKind, AttemptId, CommandId, Operation)>,
        events: Vec<BackendEvent>,
        discarded: Vec<(BackendKind, AttemptId, SourceKey)>,
        refuse_start: bool,
        exit: ExitDisposition,
    }
    impl FakeSet {
        fn new() -> Self {
            Self { uri: true, feed: true, sent: Vec::new(), events: Vec::new(),
                discarded: Vec::new(), refuse_start: false, exit: ExitDisposition::Clean }
        }
    }
    impl DriverSet for FakeSet {
        type Context = ();
        fn capabilities(&self, backend: BackendKind) -> Option<Capabilities> {
            match backend {
                URI if self.uri => Some(CAPS), FEED if self.feed => Some(CAPS), _ => None,
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
        runner.send(drivers, &(), Intent::Start { backend, attempt, source, requirements: REQ })
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
            source: SourceKey::new(3), requirements: REQ }).is_empty());
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
}
