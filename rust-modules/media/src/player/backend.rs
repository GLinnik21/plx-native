//! Backend-neutral runner at the player boundary. The route keeps the real source locator;
//! `SourceKey` names it to the driver without putting a URL in the pure reducer or diagnostics.
//! Native request correlation and release ownership remain in the concrete driver. This runner
//! only feeds dispatch results and polled observations back with their exact playback attempt.
//! No production driver is wired yet. A future caller supplies its canonical main-thread token
//! as the borrowed context; the runner does not mint or retain one.
use plx_playback_contract::{BackendDriver, BackendState, Fx, Event, Intent, Snapshot};

pub struct BackendRunner<D: BackendDriver> {
    state: BackendState,
    driver: D,
}

impl<D: BackendDriver> BackendRunner<D> {
    pub fn new(driver: D) -> Self {
        let state = BackendState::new(driver.capabilities());
        Self { state, driver }
    }

    pub fn snapshot(&self) -> &Snapshot { self.state.snapshot() }

    /// Runs one route/player intent and all synchronous dispatch answers. Returned notices
    /// belong to the caller; none are silently turned into a backend observation.
    pub fn send(&mut self, ctx: &D::Context, intent: Intent) -> Vec<Fx> {
        self.feed(ctx, Event::Intent(intent))
    }

    /// Called by the app's normal player poll. Accepted evidence and release outcomes are
    /// returned in driver order, including observations before a release that resets Snapshot.
    pub fn poll(&mut self, ctx: &D::Context) -> Vec<Fx> {
        let mut notices = Vec::new();
        for event in self.driver.poll(ctx) {
            notices.extend(self.feed(ctx, event.into()));
        }
        notices
    }

    fn feed(&mut self, ctx: &D::Context, event: Event) -> Vec<Fx> {
        let mut effects = Vec::new();
        self.state.step(event, &mut effects);
        let mut notices = Vec::new();
        for effect in effects {
            match effect {
                Fx::Dispatch { attempt, command, operation } => {
                    let result = self.driver.dispatch(ctx, attempt, command, operation);
                    let answer = Event::CommandAnswer { attempt, command, result };
                    notices.extend(self.feed(ctx, answer));
                }
                notice @ (Fx::Rejected { .. } | Fx::AcceptedObservation { .. } | Fx::ReleaseOutcome { .. }) => notices.push(notice),
            }
        }
        notices
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_playback_contract::{AttemptId, Capabilities, CommandId, ContentTime, DispatchResult, DriverEvent, Failure,
        EvidenceSource, Fidelity, Observation, Observed, Operation, Playback, ReleaseStatus,
        Requirements, SourceKey, SourceKind};

    struct FakeDriver {
        sent: Vec<(AttemptId, CommandId, Operation)>,
        events: Vec<DriverEvent>,
        refuse: bool,
    }
    impl BackendDriver for FakeDriver {
        type Context = ();
        fn capabilities(&self) -> Capabilities {
            Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
                exact_video: true, exact_audio: true, pause: true, seek: true,
                audio_track: true, subtitle_track: true }
        }
        fn dispatch(&mut self, _ctx: &Self::Context, attempt: AttemptId, command: CommandId, operation: Operation) -> DispatchResult {
            self.sent.push((attempt, command, operation));
            if self.refuse { DispatchResult::Refused(plx_playback_contract::Failure::Refused) }
            else { DispatchResult::Queued }
        }
        fn poll(&mut self, _ctx: &Self::Context) -> Vec<DriverEvent> { std::mem::take(&mut self.events) }
    }
    const REQ: Requirements = Requirements {
        source: SourceKind::ProgressiveUri, video: Fidelity::Exact, audio: Fidelity::Exact,
        audio_track: None, subtitle_track: None,
    };
    #[test]
    fn fake_driver_keeps_queue_ack_separate_from_observation_and_release() {
        let driver = FakeDriver { sent: Vec::new(), events: Vec::new(), refuse: false };
        let mut runner = BackendRunner::new(driver);
        assert!(runner.send(&(), Intent::Start { attempt: AttemptId::new(1, 1), source: SourceKey::new(1), requirements: REQ }).is_empty());
        let (attempt, command, _) = runner.driver.sent[0];
        assert_eq!(runner.snapshot().last_queued, Some(command));
        assert_eq!(runner.snapshot().playback, None);
        runner.driver.events.extend([
            DriverEvent::Observation { attempt, observation: Observation::Playback(Observed {
                value: Playback::Playing, from: EvidenceSource::BackendReport }) },
            DriverEvent::Observation { attempt, observation: Observation::Position(Observed {
                value: ContentTime::from_millis(4_000), from: EvidenceSource::BackendReport }) },
        ]);
        runner.poll(&());
        assert_eq!(runner.snapshot().position.unwrap().value.as_millis(), 4_000);
        assert!(!runner.snapshot().presented);
        runner.driver.events.push(DriverEvent::Release { attempt, status: ReleaseStatus::Released });
        runner.poll(&());
        assert_eq!(runner.snapshot().attempt, None);
        assert_eq!(runner.snapshot().position, None);
    }

    #[test]
    fn same_poll_preserves_terminal_evidence_before_snapshot_reset_and_filters_stale() {
        let driver = FakeDriver { sent: Vec::new(), events: Vec::new(), refuse: false };
        let mut runner = BackendRunner::new(driver);
        let attempt = AttemptId::new(2, 1);
        let stale = AttemptId::new(1, 1);
        runner.send(&(), Intent::Start { attempt, source: SourceKey::new(2), requirements: REQ });
        runner.driver.events.extend([
            DriverEvent::Observation { attempt: stale, observation: Observation::Presented },
            DriverEvent::Observation { attempt, observation: Observation::Failed(Failure::Backend) },
            DriverEvent::Observation { attempt, observation: Observation::Playback(Observed {
                value: Playback::Ended, from: EvidenceSource::BackendReport }) },
            DriverEvent::Observation { attempt, observation: Observation::Presented },
            DriverEvent::Observation { attempt, observation: Observation::Presented },
            DriverEvent::Release { attempt, status: ReleaseStatus::Released },
            DriverEvent::Observation { attempt, observation: Observation::Presented },
            DriverEvent::Release { attempt: stale, status: ReleaseStatus::Released },
        ]);
        let notices = runner.poll(&());
        assert_eq!(notices, vec![
            Fx::AcceptedObservation { attempt, observation: Observation::Failed(Failure::Backend) },
            Fx::AcceptedObservation { attempt, observation: Observation::Playback(Observed {
                value: Playback::Ended, from: EvidenceSource::BackendReport }) },
            Fx::AcceptedObservation { attempt, observation: Observation::Presented },
            Fx::ReleaseOutcome { attempt, status: ReleaseStatus::Released },
        ]);
        assert_eq!(*runner.snapshot(), Snapshot::default());
        assert!(runner.poll(&()).is_empty(), "the poll does not replay consumed notices");
    }

    /// A non-Send context stands in for the main-thread token without pulling `plx_base`
    /// into the lower contract crate.
    struct Token { _not_send_sync: std::rc::Rc<()> }

    struct ContextDriver {
        inner: FakeDriver,
        seen: Vec<usize>,
    }
    impl BackendDriver for ContextDriver {
        type Context = Token;
        fn capabilities(&self) -> Capabilities { self.inner.capabilities() }
        fn dispatch(&mut self, ctx: &Token, attempt: AttemptId, command: CommandId, operation: Operation) -> DispatchResult {
            self.seen.push(std::ptr::from_ref(ctx) as usize);
            self.inner.dispatch(&(), attempt, command, operation)
        }
        fn poll(&mut self, ctx: &Token) -> Vec<DriverEvent> {
            self.seen.push(std::ptr::from_ref(ctx) as usize);
            self.inner.poll(&())
        }
    }

    #[test]
    fn dispatch_and_poll_receive_the_caller_borrow_without_caching_it() {
        let first = Token { _not_send_sync: std::rc::Rc::new(()) };
        let second = Token { _not_send_sync: std::rc::Rc::new(()) };
        let attempt = AttemptId::new(3, 1);
        let mut runner = BackendRunner::new(ContextDriver {
            inner: FakeDriver { sent: Vec::new(), events: Vec::new(), refuse: false },
            seen: Vec::new(),
        });
        assert!(runner.send(&first, Intent::Start {
            attempt, source: SourceKey::new(3), requirements: REQ,
        }).is_empty());
        assert!(runner.snapshot().last_queued.is_some(), "the synchronous answer was fed back");
        runner.driver.inner.events.push(DriverEvent::Observation { attempt, observation: Observation::Presented });
        assert_eq!(runner.poll(&second), vec![Fx::AcceptedObservation { attempt, observation: Observation::Presented }]);
        assert_eq!(runner.driver.seen, [
            std::ptr::from_ref(&first) as usize,
            std::ptr::from_ref(&second) as usize,
        ]);
    }
}
