#![deny(clippy::wildcard_enum_match_arm)]
//! Pure admission and publication for one logical playback attempt across physical backends.
//! Each concrete driver owns its native requests, release deadlines and quarantine evidence.
use std::collections::BTreeMap;

use crate::{AttemptId, BackendKind, Capabilities, CommandId, ContentTime, DispatchResult,
    ExitDisposition, Failure, InitialPlayback, Observation, Observed, Operation, Playback,
    ReleaseStatus, SourceKey, StopReason, TrackChoice, TrackKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase { Vacant, Starting, Active, Releasing, Quarantined }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    Start { backend: BackendKind, attempt: AttemptId, source: SourceKey,
        requirements: crate::Requirements, initial: InitialPlayback },
    Control { backend: BackendKind, attempt: AttemptId, operation: Operation },
    Stop { backend: BackendKind, attempt: AttemptId, reason: StopReason },
}
impl Intent {
    pub const fn backend(self) -> BackendKind {
        match self {
            Self::Start { backend, .. } | Self::Control { backend, .. } | Self::Stop { backend, .. } => backend,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Intent(Intent),
    CommandAnswer { backend: BackendKind, attempt: AttemptId, command: CommandId, result: DispatchResult },
    Observation { backend: BackendKind, attempt: AttemptId, observation: Observation },
    /// A viewer goal intercepted by startup before it can issue a physical command.
    ViewerGoal { backend: BackendKind, attempt: AttemptId, paused: bool },
    ControlAccepted { backend: BackendKind, attempt: AttemptId, command: CommandId,
        operation: Operation, native_acknowledged: bool },
    ControlRefused { backend: BackendKind, attempt: AttemptId, command: CommandId,
        operation: Operation, failure: Failure },
    TrackSettled { backend: BackendKind, attempt: AttemptId, command: CommandId,
        kind: TrackKind, choice: TrackChoice },
    SeekLanded { backend: BackendKind, attempt: AttemptId, command: CommandId,
        position: Option<Observed<ContentTime>> },
    Release { backend: BackendKind, attempt: AttemptId, status: ReleaseStatus },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fx {
    Dispatch { backend: BackendKind, attempt: AttemptId, command: CommandId, operation: Operation },
    /// Dispose only the rejected Start's staged source; never touch the occupant's source.
    DiscardStaged { backend: BackendKind, attempt: AttemptId, key: SourceKey },
    Rejected { backend: BackendKind, attempt: AttemptId, failure: Failure },
    /// A synchronous queue refusal still leaves native resource release to the driver.
    CommandRefused { backend: BackendKind, attempt: AttemptId, command: CommandId,
        operation: Operation, failure: Failure },
    AcceptedControl { backend: BackendKind, attempt: AttemptId, command: CommandId,
        operation: Operation, native_acknowledged: bool },
    /// The common startup plan identifies its mandatory initial Play by command ID.
    StartupFailed { backend: BackendKind, attempt: AttemptId, command: CommandId,
        failure: Failure },
    AcceptedObservation { backend: BackendKind, attempt: AttemptId, observation: Observation },
    AcceptedTrackSettled { backend: BackendKind, attempt: AttemptId, command: CommandId,
        kind: TrackKind, choice: TrackChoice },
    /// A generic position or queued ACK cannot produce this notice.
    AcceptedSeekLanded { backend: BackendKind, attempt: AttemptId, command: CommandId,
        position: Option<Observed<ContentTime>> },
    /// Only a concrete driver's addressed release verdict produces this notice.
    ReleaseOutcome { backend: BackendKind, attempt: AttemptId, status: ReleaseStatus },
}

/// A confirmed track state; `Unspecified` is a request, never an observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackFact { Off, Selected(u64) }

/// Sparse evidence for the active attempt. `last_queued` proves only that an operation entered
/// a driver queue. Ready and Playing reports never imply a presented picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub phase: Phase,
    pub backend: Option<BackendKind>,
    pub attempt: Option<AttemptId>,
    pub playback: Option<Observed<Playback>>,
    pub position: Option<Observed<ContentTime>>,
    /// Desired viewer transport, distinct from observed native playback.
    pub viewer_paused: Option<bool>,
    pub presented: bool,
    pub audio_track: Option<Observed<TrackFact>>,
    pub subtitle_track: Option<Observed<TrackFact>>,
    pub last_queued: Option<CommandId>,
    pub seek_landed: Option<CommandId>,
    pub failure: Option<Failure>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self { phase: Phase::Vacant, backend: None, attempt: None, playback: None,
            position: None, viewer_paused: None, presented: false,
            audio_track: None, subtitle_track: None,
            last_queued: None, seek_landed: None, failure: None }
    }
}

/// One state owner across URI and Feed. Attempts come from the route owner, command IDs from this
/// reducer; a confirmed release clears evidence but never rewinds either identity.
pub struct BackendState {
    last_attempt: Option<AttemptId>,
    next_command: u64,
    snapshot: Snapshot,
    pending: BTreeMap<CommandId, Operation>,
    latest_queued_seek: Option<CommandId>,
    latest_queued_transport: Option<(CommandId, Operation)>,
    settled_viewer_paused: Option<bool>,
    deferred_control_noticed: Option<CommandId>,
    latest_queued_track: [Option<(CommandId, TrackChoice)>; 2],
}
impl BackendState {
    pub fn new() -> Self {
        Self { last_attempt: None, next_command: 0, snapshot: Snapshot::default(),
            pending: BTreeMap::new(), latest_queued_seek: None,
            latest_queued_transport: None,
            settled_viewer_paused: None, deferred_control_noticed: None,
            latest_queued_track: [None; 2] }
    }

    pub fn snapshot(&self) -> &Snapshot { &self.snapshot }

    /// This is the common state's share of exit safety. The runner also asks every driver set;
    /// even a vacant snapshot cannot declare a quarantined physical resource clean.
    pub fn exit_disposition(&self) -> ExitDisposition {
        if self.snapshot.phase == Phase::Vacant { ExitDisposition::Clean }
        else { ExitDisposition::PreserveNativeOwnership }
    }

    fn owns(&self, backend: BackendKind, attempt: AttemptId) -> bool {
        let snapshot = &self.snapshot;
        snapshot.backend == Some(backend) && snapshot.attempt == Some(attempt)
    }



    fn reject_start(effects: &mut Vec<Fx>, backend: BackendKind, attempt: AttemptId,
        source: SourceKey, failure: Failure) {
        effects.push(Fx::DiscardStaged { backend, attempt, key: source });
        effects.push(Fx::Rejected { backend, attempt, failure });
    }

    /// `capabilities` is supplied by the injected driver set for this event's backend. It may be
    /// absent; no source or operation is dispatched to an unavailable driver.
    pub fn step(&mut self, event: Event, capabilities: Option<Capabilities>, effects: &mut Vec<Fx>) {
        self.transition(event, capabilities, effects);
    }
    fn transition(&mut self, event: Event, capabilities: Option<Capabilities>, effects: &mut Vec<Fx>) {
        match event {
            Event::Intent(Intent::Start { backend, attempt, source, requirements, initial }) => {
                let failure = if self.snapshot.phase != Phase::Vacant {
                    Some(Failure::Busy)
                } else if self.last_attempt.is_some_and(|last| attempt <= last) {
                    Some(Failure::Stale)
                } else if let Some(caps) = capabilities {
                    (!caps.supports(requirements)).then_some(Failure::Unsupported)
                } else {
                    Some(Failure::Unavailable)
                };
                if let Some(failure) = failure {
                    Self::reject_start(effects, backend, attempt, source, failure);
                } else {
                    self.last_attempt = Some(attempt);
                    self.snapshot.phase = Phase::Starting;
                    self.snapshot.backend = Some(backend);
                    self.snapshot.attempt = Some(attempt);
                    self.snapshot.viewer_paused = Some(initial.paused);
                    self.settled_viewer_paused = Some(initial.paused);
                    let operation = Operation::Start { source, requirements };
                    self.next_command = self.next_command.checked_add(1).expect("playback command IDs exhausted");
                    let command = CommandId(self.next_command);
                    self.pending.insert(command, operation);
                    effects.push(Fx::Dispatch { backend, attempt, command, operation });
                }
            }
            Event::Intent(Intent::Control { backend, attempt, operation }) => {
                let failure = if !self.owns(backend, attempt) || self.snapshot.phase != Phase::Active {
                    Some(Failure::Stale)
                } else if let Some(caps) = capabilities {
                    (!Self::supports_operation(caps, operation)).then_some(Failure::Unsupported)
                } else {
                    Some(Failure::Unavailable)
                };
                if let Some(failure) = failure {
                    effects.push(Fx::Rejected { backend, attempt, failure });
                } else {
                    self.next_command = self.next_command.checked_add(1).expect("playback command IDs exhausted");
                    let command = CommandId(self.next_command);
                    self.pending.insert(command, operation);
                    effects.push(Fx::Dispatch { backend, attempt, command, operation });
                }
            }
            Event::Intent(Intent::Stop { backend, attempt, reason }) => {
                if !self.owns(backend, attempt)
                    || !matches!(&self.snapshot.phase, Phase::Starting | Phase::Active) { return; }
                self.pending.clear();
                self.latest_queued_seek = None;
                self.latest_queued_transport = None;
                self.deferred_control_noticed = None;
                self.latest_queued_track = [None; 2];
                if capabilities.is_none() {
                    self.snapshot.phase = Phase::Quarantined;
                    self.snapshot.failure = Some(Failure::ReleaseUncertain);
                    effects.push(Fx::Rejected { backend, attempt, failure: Failure::Unavailable });
                } else {
                    self.snapshot.phase = Phase::Releasing;
                    let operation = Operation::Stop(reason);
                    self.next_command = self.next_command.checked_add(1).expect("playback command IDs exhausted");
                    let command = CommandId(self.next_command);
                    self.pending.insert(command, operation);
                    effects.push(Fx::Dispatch { backend, attempt, command, operation });
                }
            }
            Event::CommandAnswer { backend, attempt, command, result } => {
                if !self.owns(backend, attempt) { return; }
                let Some(operation) = self.pending.remove(&command) else { return; };
                match result {
                    DispatchResult::Queued => {
                        self.snapshot.last_queued = Some(command);
                        if matches!(operation, Operation::Seek(_)) {
                            self.latest_queued_seek = Some(command);
                            self.snapshot.seek_landed = None;
                        }
                        if matches!(operation, Operation::Play | Operation::Pause) {
                            self.latest_queued_transport = Some((command, operation));
                            self.deferred_control_noticed = None;
                            self.snapshot.viewer_paused = Some(operation == Operation::Pause);
                        }
                        if let Operation::SelectTrack { kind, choice } = operation {
                            self.latest_queued_track[Self::track_slot(kind)] = Some((command, choice));
                        }
                    }
                    DispatchResult::Refused(reason) => {
                        self.snapshot.failure = Some(reason);
                        if let Operation::Start { source, .. } = operation {
                            effects.push(Fx::DiscardStaged { backend, attempt, key: source });
                        }
                        effects.push(Fx::CommandRefused { backend, attempt, command, operation, failure: reason });
                        if matches!(operation, Operation::Start { .. }) {
                            // A refused Start can still own a native object. Release is explicit.
                            self.snapshot.phase = Phase::Releasing;
                        } else if matches!(operation, Operation::Stop(_)) {
                            self.snapshot.phase = Phase::Quarantined;
                            self.snapshot.failure = Some(Failure::ReleaseUncertain);
                        }
                    }
                }
            }
            Event::Observation { backend, attempt, observation }
                if self.owns(backend, attempt)
                    && matches!(&self.snapshot.phase, Phase::Starting | Phase::Active) =>
            {
                if observation == Observation::Presented && self.snapshot.presented { return; }
                match observation {
                    Observation::Playback(value) => {
                        self.snapshot.playback = Some(value);
                        self.snapshot.phase = Phase::Active;
                    }
                    Observation::Position(value) => self.snapshot.position = Some(value),
                    Observation::Presented => self.snapshot.presented = true,
                    Observation::TrackSelected(value) => {
                        let fact = Observed { value: TrackFact::Selected(value.value.id), from: value.from };
                        match value.value.kind {
                            TrackKind::Audio => self.snapshot.audio_track = Some(fact),
                            TrackKind::Subtitle => self.snapshot.subtitle_track = Some(fact),
                        }
                    }
                    Observation::TrackOff(value) => {
                        let fact = Observed { value: TrackFact::Off, from: value.from };
                        match value.value {
                            TrackKind::Audio => self.snapshot.audio_track = Some(fact),
                            TrackKind::Subtitle => self.snapshot.subtitle_track = Some(fact),
                        }
                    }
                    // Carried to the readout by the accepted effect; the metrics read it back
                    // from that readout (`observe_backend_condition`), and the sparse snapshot
                    // keeps no rebuffer flag a stale value could outlive.
                    Observation::Rebuffering(_) => {}
                    Observation::Failed(reason) => self.snapshot.failure = Some(reason),
                }
                effects.push(Fx::AcceptedObservation { backend, attempt, observation });
            }
            Event::Observation { .. } => {}
            Event::ViewerGoal { backend, attempt, paused }
                if self.owns(backend, attempt)
                    && matches!(&self.snapshot.phase, Phase::Starting | Phase::Active) => {
                self.snapshot.viewer_paused = Some(paused);
                self.settled_viewer_paused = Some(paused);
            }
            Event::ViewerGoal { .. } => {}
            Event::ControlRefused { backend, attempt, command, operation, failure }
                if self.owns(backend, attempt) && self.snapshot.phase == Phase::Active =>
            {
                let correlated = match operation {
                    Operation::Play | Operation::Pause => {
                        if self.latest_queued_transport == Some((command, operation)) {
                            self.latest_queued_transport = None;
                            self.deferred_control_noticed = None;
                            self.snapshot.viewer_paused = self.settled_viewer_paused;
                            true
                        } else { false }
                    }
                    Operation::Seek(_) => {
                        if self.latest_queued_seek == Some(command) {
                            self.latest_queued_seek = None;
                            true
                        } else { false }
                    }
                    Operation::SelectTrack { kind, choice } => {
                        let slot = Self::track_slot(kind);
                        if self.latest_queued_track[slot] == Some((command, choice)) {
                            self.latest_queued_track[slot] = None;
                            true
                        } else { false }
                    }
                    Operation::Start { .. } | Operation::Stop(_) => false,
                };
                if !correlated { return; }
                self.snapshot.failure = Some(failure);
                effects.push(Fx::CommandRefused { backend, attempt, command, operation, failure });
            }
            Event::ControlRefused { .. } => {}
            Event::ControlAccepted { backend, attempt, command, operation, native_acknowledged }
                if self.owns(backend, attempt) && self.snapshot.phase == Phase::Active
                    && self.latest_queued_transport == Some((command, operation)) =>
            {
                if native_acknowledged {
                    self.latest_queued_transport = None;
                    self.deferred_control_noticed = None;
                    self.settled_viewer_paused = self.snapshot.viewer_paused;
                } else if self.deferred_control_noticed.replace(command) == Some(command) {
                    return;
                }
                effects.push(Fx::AcceptedControl { backend, attempt, command, operation,
                    native_acknowledged });
            }
            Event::ControlAccepted { .. } => {}
            Event::TrackSettled { backend, attempt, command, kind, choice }
                if self.owns(backend, attempt) && self.snapshot.phase == Phase::Active =>
            {
                let slot = Self::track_slot(kind);
                if choice == TrackChoice::Unspecified
                    || self.latest_queued_track[slot] != Some((command, choice)) { return; }
                self.latest_queued_track[slot] = None;
                effects.push(Fx::AcceptedTrackSettled { backend, attempt, command, kind, choice });
            }
            Event::TrackSettled { .. } => {}
            Event::SeekLanded { backend, attempt, command, position }
                if self.owns(backend, attempt) && self.snapshot.phase == Phase::Active =>
            {
                if self.latest_queued_seek != Some(command) { return; }
                self.latest_queued_seek = None;
                self.snapshot.seek_landed = Some(command);
                if let Some(position) = position { self.snapshot.position = Some(position); }
                effects.push(Fx::AcceptedSeekLanded { backend, attempt, command, position });
            }
            Event::SeekLanded { .. } => {}
            Event::Release { backend, attempt, status } if self.owns(backend, attempt) => {
                match status {
                    ReleaseStatus::Closing => {
                        self.snapshot.phase = Phase::Releasing;
                        self.pending.clear();
                        self.latest_queued_seek = None;
                        self.latest_queued_transport = None;
                        self.deferred_control_noticed = None;
                        self.latest_queued_track = [None; 2];
                    }
                    ReleaseStatus::Quarantined => {
                        self.snapshot.phase = Phase::Quarantined;
                        self.snapshot.failure = Some(Failure::ReleaseUncertain);
                        self.pending.clear();
                        self.latest_queued_seek = None;
                        self.latest_queued_transport = None;
                        self.deferred_control_noticed = None;
                        self.latest_queued_track = [None; 2];
                    }
                    ReleaseStatus::Released => {
                        self.pending.clear();
                        self.latest_queued_seek = None;
                        self.latest_queued_transport = None;
                        self.deferred_control_noticed = None;
                        self.settled_viewer_paused = None;
                        self.latest_queued_track = [None; 2];
                        self.snapshot = Snapshot::default();
                    }
                }
                effects.push(Fx::ReleaseOutcome { backend, attempt, status });
            }
            Event::Release { .. } => {}
        }
    }

    fn supports_operation(caps: Capabilities, operation: Operation) -> bool {
        match operation {
            Operation::Play | Operation::Pause => caps.pause,
            Operation::Seek(_) => caps.seek,
            Operation::SelectTrack { kind, choice } => choice != TrackChoice::Unspecified
                && caps.supports_track(kind, choice),
            Operation::Start { .. } | Operation::Stop(_) => false,
        }
    }

    const fn track_slot(kind: TrackKind) -> usize {
        match kind { TrackKind::Audio => 0, TrackKind::Subtitle => 1 }
    }
}
impl Default for BackendState { fn default() -> Self { Self::new() } }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EvidenceSource, Fidelity, Requirements, SourceKind, TrackSelection};

    const URI: BackendKind = BackendKind::Uri;
    const FEED: BackendKind = BackendKind::Feed;
    const CAPS: Capabilities = Capabilities {
        progressive_uri: true, hls_uri: false, packet_feed: false, pause: true, seek: true,
        audio_track: true, subtitle_track: true, audio_off: false, subtitle_off: false,
    };
    const REQ: Requirements = Requirements {
        source: SourceKind::ProgressiveUri, video: Fidelity::Exact, audio: Fidelity::Exact,
        audio_track: TrackChoice::Unspecified, subtitle_track: TrackChoice::Unspecified,
    };
    const INITIAL: InitialPlayback = InitialPlayback { position: None, paused: false };
    fn step(state: &mut BackendState, event: Event, caps: Option<Capabilities>) -> Vec<Fx> {
        let mut effects = Vec::new(); state.step(event, caps, &mut effects); effects
    }
    fn start(state: &mut BackendState, backend: BackendKind, attempt: AttemptId) -> CommandId {
        let effects = step(state, Event::Intent(Intent::Start {
            backend, attempt, source: SourceKey::new(7), requirements: REQ, initial: INITIAL,
        }), Some(CAPS));
        let [Fx::Dispatch { backend: sent, attempt: addressed, command,
            operation: Operation::Start { .. } }] = effects.as_slice() else { panic!("start dispatch: {effects:?}") };
        assert_eq!((*sent, *addressed), (backend, attempt));
        *command
    }
    fn ready(state: &mut BackendState, backend: BackendKind, attempt: AttemptId) {
        step(state, Event::Observation { backend, attempt, observation: Observation::Playback(Observed {
            value: Playback::ReadyAwaitPlay, from: EvidenceSource::BackendReport,
        }) }, None);
    }

    #[test]
    fn one_owner_ignores_wrong_backend_even_with_matching_attempt_and_quarantines_stop() {
        let mut state = BackendState::new();
        let attempt = AttemptId::new(7, 1);
        let start_command = start(&mut state, URI, attempt);
        step(&mut state, Event::CommandAnswer { backend: FEED, attempt, command: start_command,
            result: DispatchResult::Queued }, None);
        assert_eq!(state.snapshot.last_queued, None);
        step(&mut state, Event::Observation { backend: FEED, attempt,
            observation: Observation::Presented }, None);
        step(&mut state, Event::Release { backend: FEED, attempt, status: ReleaseStatus::Released }, None);
        assert_eq!(state.snapshot.phase, Phase::Starting);
        ready(&mut state, URI, attempt);
        assert_eq!(state.snapshot.phase, Phase::Active);
        assert!(!state.snapshot.presented, "Ready is not picture proof");
        let stop = step(&mut state, Event::Intent(Intent::Stop { backend: URI, attempt,
            reason: StopReason::Replace }), Some(CAPS));
        let [Fx::Dispatch { operation: Operation::Stop(StopReason::Replace), command, .. }] = stop.as_slice()
            else { panic!("typed replace stop: {stop:?}") };
        step(&mut state, Event::CommandAnswer { backend: URI, attempt, command: *command,
            result: DispatchResult::Queued }, None);
        assert_eq!(state.exit_disposition(), ExitDisposition::PreserveNativeOwnership,
            "a queued Stop is not a release verdict");
        step(&mut state, Event::Release { backend: URI, attempt, status: ReleaseStatus::Quarantined }, None);
        let candidate = AttemptId::new(8, 1);
        let busy = step(&mut state, Event::Intent(Intent::Start { backend: FEED, attempt: candidate,
            source: SourceKey::new(8), requirements: REQ, initial: INITIAL }), Some(CAPS));
        assert_eq!(busy, vec![Fx::DiscardStaged { backend: FEED, attempt: candidate, key: SourceKey::new(8) },
            Fx::Rejected { backend: FEED, attempt: candidate, failure: Failure::Busy }]);
        assert!(step(&mut state, Event::Release { backend: FEED, attempt, status: ReleaseStatus::Released }, None).is_empty());
        assert_eq!(state.snapshot.phase, Phase::Quarantined);
        step(&mut state, Event::Release { backend: URI, attempt, status: ReleaseStatus::Released }, None);
        assert_eq!(*state.snapshot(), Snapshot::default());
        assert_eq!(state.exit_disposition(), ExitDisposition::Clean);
        start(&mut state, FEED, candidate);
        assert_eq!(state.snapshot.backend, Some(FEED));
    }

    #[test]
    fn unavailable_start_discards_only_its_staged_key_and_never_mints_an_attempt() {
        let mut state = BackendState::new();
        let attempt = AttemptId::new(2, 1);
        assert_eq!(step(&mut state, Event::Intent(Intent::Start { backend: URI, attempt,
            source: SourceKey::new(99), requirements: REQ, initial: INITIAL }), None), vec![
                Fx::DiscardStaged { backend: URI, attempt, key: SourceKey::new(99) },
                Fx::Rejected { backend: URI, attempt, failure: Failure::Unavailable },
            ]);
        assert_eq!(*state.snapshot(), Snapshot::default());
        start(&mut state, URI, attempt);
    }

    #[test]
    fn native_control_acceptance_is_exactly_correlated_and_is_not_playback() {
        let mut state = BackendState::new();
        let attempt = AttemptId::new(2, 2);
        start(&mut state, FEED, attempt);
        ready(&mut state, FEED, attempt);
        let effects = step(&mut state, Event::Intent(Intent::Control { backend: FEED,
            attempt, operation: Operation::Play }), Some(CAPS));
        let [Fx::Dispatch { command, .. }] = effects.as_slice() else { panic!("play dispatch") };
        let command = *command;
        step(&mut state, Event::CommandAnswer { backend: FEED, attempt, command,
            result: DispatchResult::Queued }, None);
        assert!(step(&mut state, Event::ControlAccepted { backend: URI, attempt, command,
            operation: Operation::Play, native_acknowledged: true }, None).is_empty());
        assert!(step(&mut state, Event::ControlAccepted { backend: FEED, attempt,
            command, operation: Operation::Pause, native_acknowledged: true }, None).is_empty());
        assert_eq!(step(&mut state, Event::ControlAccepted { backend: FEED, attempt, command,
            operation: Operation::Play, native_acknowledged: true }, None),
            [Fx::AcceptedControl { backend: FEED, attempt, command,
                operation: Operation::Play, native_acknowledged: true }]);
        assert!(step(&mut state, Event::ControlAccepted { backend: FEED, attempt, command,
            operation: Operation::Play, native_acknowledged: true }, None).is_empty());
        assert_eq!(state.snapshot.playback.unwrap().value, Playback::ReadyAwaitPlay);
        assert!(!state.snapshot.presented);
    }

    #[test]
    fn deferred_acceptance_keeps_refusal_correlation_and_latest_goal_rolls_back() {
        let mut state = BackendState::new();
        let attempt = AttemptId::new(2, 3);
        start(&mut state, FEED, attempt);
        ready(&mut state, FEED, attempt);
        let pause = step(&mut state, Event::Intent(Intent::Control { backend: FEED,
            attempt, operation: Operation::Pause }), Some(CAPS));
        let [Fx::Dispatch { command: first, .. }] = pause.as_slice() else { panic!("pause") };
        let first = *first;
        step(&mut state, Event::CommandAnswer { backend: FEED, attempt, command: first,
            result: DispatchResult::Queued }, None);
        assert_eq!(state.snapshot.viewer_paused, Some(true));
        assert_eq!(step(&mut state, Event::ControlAccepted { backend: FEED, attempt,
            command: first, operation: Operation::Pause, native_acknowledged: false }, None),
            [Fx::AcceptedControl { backend: FEED, attempt, command: first,
                operation: Operation::Pause, native_acknowledged: false }]);
        let refusal = Event::ControlRefused { backend: FEED, attempt, command: first,
            operation: Operation::Pause, failure: Failure::Refused };
        assert_eq!(step(&mut state, refusal, None), [Fx::CommandRefused { backend: FEED,
            attempt, command: first, operation: Operation::Pause, failure: Failure::Refused }]);
        assert_eq!(state.snapshot.viewer_paused, Some(false));

        let second = step(&mut state, Event::Intent(Intent::Control { backend: FEED,
            attempt, operation: Operation::Pause }), Some(CAPS));
        let [Fx::Dispatch { command: second, .. }] = second.as_slice() else { panic!("pause") };
        let second = *second;
        step(&mut state, Event::CommandAnswer { backend: FEED, attempt, command: second,
            result: DispatchResult::Queued }, None);
        assert!(step(&mut state, refusal, None).is_empty(), "old refusal is stale");
        assert_eq!(state.snapshot.viewer_paused, Some(true));
        step(&mut state, Event::ControlAccepted { backend: FEED, attempt, command: second,
            operation: Operation::Pause, native_acknowledged: false }, None);
        assert_eq!(step(&mut state, Event::ControlAccepted { backend: FEED, attempt,
            command: second, operation: Operation::Pause, native_acknowledged: true }, None),
            [Fx::AcceptedControl { backend: FEED, attempt, command: second,
                operation: Operation::Pause, native_acknowledged: true }]);
        let play = step(&mut state, Event::Intent(Intent::Control { backend: FEED,
            attempt, operation: Operation::Play }), Some(CAPS));
        let [Fx::Dispatch { command: play, .. }] = play.as_slice() else { panic!("play") };
        let play = *play;
        step(&mut state, Event::CommandAnswer { backend: FEED, attempt, command: play,
            result: DispatchResult::Queued }, None);
        assert_eq!(state.snapshot.viewer_paused, Some(false));
        step(&mut state, Event::ControlRefused { backend: FEED, attempt, command: play,
            operation: Operation::Play, failure: Failure::Refused }, None);
        assert_eq!(state.snapshot.viewer_paused, Some(true), "latest refusal restores settled goal");
    }

    #[test]
    fn synchronous_start_refusal_keeps_release_debt_and_terminal_notices_survive_reset() {
        let mut state = BackendState::new();
        let attempt = AttemptId::new(3, 1);
        let command = start(&mut state, FEED, attempt);
        let refused = step(&mut state, Event::CommandAnswer { backend: FEED, attempt, command,
            result: DispatchResult::Refused(Failure::Refused) }, None);
        assert_eq!(refused, [Fx::DiscardStaged { backend: FEED, attempt, key: SourceKey::new(7) },
            Fx::CommandRefused { backend: FEED, attempt, command,
            operation: Operation::Start { source: SourceKey::new(7), requirements: REQ },
            failure: Failure::Refused }]);
        assert_eq!(state.snapshot.phase, Phase::Releasing);
        assert_eq!(state.exit_disposition(), ExitDisposition::PreserveNativeOwnership);
        assert!(step(&mut state, Event::Release { backend: URI, attempt,
            status: ReleaseStatus::Released }, None).is_empty());
        assert_eq!(step(&mut state, Event::Release { backend: FEED, attempt,
            status: ReleaseStatus::Released }, None), [Fx::ReleaseOutcome { backend: FEED, attempt,
            status: ReleaseStatus::Released }]);
        assert_eq!(*state.snapshot(), Snapshot::default());
    }

    #[test]
    fn seek_lands_only_from_exact_queued_command_on_the_active_backend() {
        let mut state = BackendState::new();
        let attempt = AttemptId::new(4, 1);
        start(&mut state, URI, attempt); ready(&mut state, URI, attempt);
        let target = ContentTime::from_millis(5_000);
        let effects = step(&mut state, Event::Intent(Intent::Control { backend: URI, attempt,
            operation: Operation::Seek(target) }), Some(CAPS));
        let [Fx::Dispatch { command, .. }] = effects.as_slice() else { panic!("seek dispatch") };
        let command = *command;
        step(&mut state, Event::Observation { backend: URI, attempt, observation: Observation::Position(
            Observed { value: target, from: EvidenceSource::BackendReport }) }, None);
        assert_eq!(state.snapshot.seek_landed, None);
        for (backend, addressed, id) in [(FEED, attempt, command), (URI, AttemptId::new(4, 2), command),
            (URI, attempt, CommandId(command.get() + 1))] {
            assert!(step(&mut state, Event::SeekLanded { backend, attempt: addressed, command: id,
                position: None }, None).is_empty());
        }
        assert!(step(&mut state, Event::SeekLanded { backend: URI, attempt, command,
            position: None }, None).is_empty(), "a command not yet queued has not landed");
        step(&mut state, Event::CommandAnswer { backend: URI, attempt, command,
            result: DispatchResult::Queued }, None);
        let landed = step(&mut state, Event::SeekLanded { backend: URI, attempt, command,
            position: None }, None);
        assert_eq!(landed, [Fx::AcceptedSeekLanded { backend: URI, attempt, command, position: None }]);
        assert_eq!(state.snapshot.seek_landed, Some(command));
        assert!(step(&mut state, Event::SeekLanded { backend: URI, attempt, command,
            position: None }, None).is_empty(), "one seek lands once");

        let mut later = Vec::new();
        for ms in [6_000, 7_000] {
            let effects = step(&mut state, Event::Intent(Intent::Control {
                backend: URI, attempt, operation: Operation::Seek(ContentTime::from_millis(ms)),
            }), Some(CAPS));
            let [Fx::Dispatch { command, .. }] = effects.as_slice() else { panic!("later seek dispatch") };
            let command = *command;
            step(&mut state, Event::CommandAnswer { backend: URI, attempt, command,
                result: DispatchResult::Queued }, None);
            later.push(command);
        }
        assert!(step(&mut state, Event::SeekLanded { backend: URI, attempt, command: later[0],
            position: None }, None).is_empty(), "a superseded seek cannot land the latest intent");
        assert_eq!(step(&mut state, Event::SeekLanded { backend: URI, attempt, command: later[1],
            position: None }, None), [Fx::AcceptedSeekLanded { backend: URI, attempt,
                command: later[1], position: None }]);
    }

    #[test]
    fn track_completion_rejects_old_command_and_wrong_backend_after_supersession() {
        let mut state = BackendState::new();
        let attempt = AttemptId::new(5, 1);
        start(&mut state, URI, attempt);
        ready(&mut state, URI, attempt);
        let mut commands = Vec::new();
        for id in [3, 4] {
            let effects = step(&mut state, Event::Intent(Intent::Control { backend: URI,
                attempt, operation: Operation::SelectTrack { kind: TrackKind::Audio,
                    choice: TrackChoice::Selected(id) } }), Some(CAPS));
            let [Fx::Dispatch { command, .. }] = effects.as_slice() else { panic!("track dispatch") };
            let command = *command;
            step(&mut state, Event::CommandAnswer { backend: URI, attempt, command,
                result: DispatchResult::Queued }, None);
            commands.push(command);
        }
        for (backend, command) in [(URI, commands[0]), (FEED, commands[1])] {
            assert!(step(&mut state, Event::TrackSettled { backend, attempt, command,
                kind: TrackKind::Audio, choice: TrackChoice::Selected(4) }, None).is_empty());
        }
        assert_eq!(step(&mut state, Event::TrackSettled { backend: URI, attempt,
            command: commands[1], kind: TrackKind::Audio, choice: TrackChoice::Selected(4) }, None),
            [Fx::AcceptedTrackSettled { backend: URI, attempt, command: commands[1],
                kind: TrackKind::Audio, choice: TrackChoice::Selected(4) }]);
        assert!(step(&mut state, Event::TrackSettled { backend: URI, attempt,
            command: commands[1], kind: TrackKind::Audio,
            choice: TrackChoice::Selected(4) }, None).is_empty());
    }

    #[test]
    fn refused_second_track_preserves_first_queued_track_confirmation() {
        let mut state = BackendState::new();
        let attempt = AttemptId::new(6, 1);
        start(&mut state, URI, attempt); ready(&mut state, URI, attempt);
        let first_choice = TrackChoice::Selected(42);
        let first_operation = Operation::SelectTrack { kind: TrackKind::Audio,
            choice: first_choice };
        let first = step(&mut state, Event::Intent(Intent::Control { backend: URI,
            attempt, operation: first_operation }), Some(CAPS));
        let [Fx::Dispatch { command: first, .. }] = first.as_slice() else { panic!("first track") };
        let first = *first;
        step(&mut state, Event::CommandAnswer { backend: URI, attempt, command: first,
            result: DispatchResult::Queued }, None);
        let second = step(&mut state, Event::Intent(Intent::Control { backend: URI,
            attempt, operation: Operation::SelectTrack { kind: TrackKind::Audio,
                choice: TrackChoice::Selected(43) } }), Some(CAPS));
        let [Fx::Dispatch { command: second, .. }] = second.as_slice() else { panic!("second track") };
        step(&mut state, Event::CommandAnswer { backend: URI, attempt, command: *second,
            result: DispatchResult::Refused(Failure::Busy) }, None);
        assert_eq!(step(&mut state, Event::TrackSettled { backend: URI, attempt,
            command: first, kind: TrackKind::Audio, choice: first_choice }, None),
            [Fx::AcceptedTrackSettled { backend: URI, attempt, command: first,
                kind: TrackKind::Audio, choice: first_choice }]);
    }

    #[test]
    fn refused_second_seek_preserves_first_queued_seek_landing() {
        let mut state = BackendState::new();
        let attempt = AttemptId::new(7, 1);
        start(&mut state, URI, attempt); ready(&mut state, URI, attempt);
        let target = ContentTime::from_millis(5_000);
        let first = step(&mut state, Event::Intent(Intent::Control { backend: URI,
            attempt, operation: Operation::Seek(target) }), Some(CAPS));
        let [Fx::Dispatch { command: first, .. }] = first.as_slice() else { panic!("first seek") };
        let first = *first;
        step(&mut state, Event::CommandAnswer { backend: URI, attempt, command: first,
            result: DispatchResult::Queued }, None);
        let second = step(&mut state, Event::Intent(Intent::Control { backend: URI,
            attempt, operation: Operation::Seek(ContentTime::from_millis(6_000)) }), Some(CAPS));
        let [Fx::Dispatch { command: second, .. }] = second.as_slice() else { panic!("second seek") };
        step(&mut state, Event::CommandAnswer { backend: URI, attempt, command: *second,
            result: DispatchResult::Refused(Failure::Busy) }, None);
        assert_eq!(step(&mut state, Event::SeekLanded { backend: URI, attempt,
            command: first, position: None }, None),
            [Fx::AcceptedSeekLanded { backend: URI, attempt, command: first, position: None }]);
    }

    #[test]
    fn off_is_distinct_from_unspecified_and_positive_track_evidence() {
        let mut state = BackendState::new();
        assert!(CAPS.supports(REQ));
        assert!(!CAPS.supports(Requirements { subtitle_track: TrackChoice::Off, ..REQ }));
        let attempt = AttemptId::new(5, 1);
        start(&mut state, URI, attempt); ready(&mut state, URI, attempt);
        assert_eq!(step(&mut state, Event::Intent(Intent::Control { backend: URI, attempt,
            operation: Operation::SelectTrack { kind: TrackKind::Audio, choice: TrackChoice::Off } }), Some(CAPS)),
            [Fx::Rejected { backend: URI, attempt, failure: Failure::Unsupported }]);
        let observation = Observation::TrackSelected(Observed { value: TrackSelection {
            kind: TrackKind::Audio, id: 0 }, from: EvidenceSource::BackendReport });
        step(&mut state, Event::Observation { backend: URI, attempt, observation }, None);
        assert_eq!(state.snapshot.audio_track.unwrap().value, TrackFact::Selected(0),
            "zero is a valid selected identity, never a sentinel for Off");
        step(&mut state, Event::Observation { backend: URI, attempt,
            observation: Observation::TrackOff(Observed { value: TrackKind::Audio,
                from: EvidenceSource::BackendReport }) }, None);
        assert_eq!(state.snapshot.audio_track.unwrap().value, TrackFact::Off);
    }

    #[test]
    fn queued_play_refusal_is_correlated_and_does_not_claim_playback_failed() {
        let mut state = BackendState::new();
        let attempt = AttemptId::new(9, 1);
        let start_command = start(&mut state, URI, attempt);
        step(&mut state, Event::CommandAnswer { backend: URI, attempt,
            command: start_command, result: DispatchResult::Queued }, None);
        ready(&mut state, URI, attempt);
        let sent = step(&mut state, Event::Intent(Intent::Control { backend: URI, attempt,
            operation: Operation::Play }), Some(CAPS));
        let [Fx::Dispatch { command, .. }] = sent.as_slice() else { panic!("play dispatch") };
        let command = *command;
        step(&mut state, Event::CommandAnswer { backend: URI, attempt, command,
            result: DispatchResult::Queued }, None);
        for (backend, addressed, id) in [(FEED, attempt, command),
            (URI, AttemptId::new(9, 2), command), (URI, attempt, start_command)] {
            assert!(step(&mut state, Event::ControlRefused { backend, attempt: addressed,
                command: id, operation: Operation::Play, failure: Failure::Refused }, None).is_empty());
        }
        assert_eq!(step(&mut state, Event::ControlRefused { backend: URI, attempt,
            command, operation: Operation::Play, failure: Failure::Refused }, None),
            [Fx::CommandRefused { backend: URI, attempt, command,
                operation: Operation::Play, failure: Failure::Refused }]);
        assert_eq!(state.snapshot.playback.unwrap().value, Playback::ReadyAwaitPlay);
        assert_eq!(state.snapshot.failure, Some(Failure::Refused));
    }

    #[test]
    fn pending_cancellation_quarantine_and_release_never_rewind_command_identity() {
        let mut state = BackendState::new();
        let attempt = AttemptId::new(12, 1);
        let first = start(&mut state, URI, attempt);
        assert_eq!(state.pending.len(), 1);
        assert!(step(&mut state, Event::CommandAnswer { backend: FEED, attempt,
            command: first, result: DispatchResult::Queued }, None).is_empty());
        assert_eq!(state.pending.len(), 1, "wrong backend cannot consume an addressed command");
        let fx = step(&mut state, Event::Intent(Intent::Stop { backend: URI, attempt,
            reason: StopReason::Replace }), Some(CAPS));
        let [Fx::Dispatch { command: stop, operation: Operation::Stop(StopReason::Replace), .. }] = fx.as_slice()
            else { panic!("stop dispatch: {fx:?}") };
        let stop = *stop;
        assert!(stop > first);
        assert_eq!(state.pending.len(), 1);
        assert!(!state.pending.contains_key(&first));
        assert!(step(&mut state, Event::CommandAnswer { backend: URI, attempt,
            command: first, result: DispatchResult::Queued }, None).is_empty());
        step(&mut state, Event::Release { backend: URI, attempt, status: ReleaseStatus::Quarantined }, None);
        assert!(state.pending.is_empty());
        assert!(step(&mut state, Event::CommandAnswer { backend: URI, attempt,
            command: stop, result: DispatchResult::Queued }, None).is_empty());
        let next_attempt = AttemptId::new(12, 2);
        assert!(step(&mut state, Event::Release { backend: URI, attempt: next_attempt,
            status: ReleaseStatus::Released }, None).is_empty());
        assert_eq!(state.snapshot.phase, Phase::Quarantined);
        step(&mut state, Event::Release { backend: URI, attempt, status: ReleaseStatus::Released }, None);
        assert_eq!(*state.snapshot(), Snapshot::default());
        let next = start(&mut state, FEED, next_attempt);
        assert!(next > stop, "release does not recycle a command ID");
        assert!(step(&mut state, Event::CommandAnswer { backend: URI, attempt,
            command: first, result: DispatchResult::Queued }, None).is_empty());
        assert!(step(&mut state, Event::Release { backend: URI, attempt,
            status: ReleaseStatus::Released }, None).is_empty());
        assert_eq!(state.snapshot.attempt, Some(next_attempt));
        assert_eq!(state.pending.len(), 1);
        assert!(state.pending.contains_key(&next));
    }
}
