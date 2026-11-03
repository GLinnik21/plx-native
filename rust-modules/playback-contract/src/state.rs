//! Admission and publication for one playback backend. The native backend remains responsible
//! for its own registration, callback correlation, release deadlines, and quarantine policy.
use std::collections::BTreeMap;

use crate::{AttemptId, Capabilities, CommandId, ContentTime, DispatchResult, Failure, Observation,
    Observed, Operation, Playback, ReleaseStatus, SourceKey, TrackKind, TrackSelection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase { Vacant, Starting, Active, Releasing, Quarantined }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    Start { attempt: AttemptId, source: SourceKey, requirements: crate::Requirements },
    Control { attempt: AttemptId, operation: Operation },
    Stop { attempt: AttemptId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Intent(Intent),
    CommandAnswer { attempt: AttemptId, command: CommandId, result: DispatchResult },
    Observation { attempt: AttemptId, observation: Observation },
    Release { attempt: AttemptId, status: ReleaseStatus },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fx {
    Dispatch { attempt: AttemptId, command: CommandId, operation: Operation },
    Rejected { attempt: AttemptId, failure: Failure },
    /// Accepted evidence is delivered even when a later event in the same poll resets Snapshot.
    AcceptedObservation { attempt: AttemptId, observation: Observation },
    /// The concrete resource owner decided this status; a stale attempt publishes nothing.
    ReleaseOutcome { attempt: AttemptId, status: ReleaseStatus },
}

/// Only populated fields are observations. `last_queued` means the driver accepted a request
/// into its queue; it never updates playback, position, track or presentation evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub phase: Phase,
    pub attempt: Option<AttemptId>,
    pub playback: Option<Observed<Playback>>,
    pub position: Option<Observed<ContentTime>>,
    pub presented: bool,
    pub audio_track: Option<Observed<TrackSelection>>,
    pub subtitle_track: Option<Observed<TrackSelection>>,
    pub last_queued: Option<CommandId>,
    pub failure: Option<Failure>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self { phase: Phase::Vacant, attempt: None, playback: None, position: None,
            presented: false, audio_track: None, subtitle_track: None, last_queued: None,
            failure: None }
    }
}

/// Pure reducer. Its identities outlive the visible snapshot, so a released attempt's late
/// answer cannot alias the next attempt. A confirmed release clears all published evidence.
pub struct BackendState {
    capabilities: Capabilities,
    last_attempt: Option<AttemptId>,
    next_command: u64,
    snapshot: Snapshot,
    pending: BTreeMap<CommandId, Operation>,
}
impl BackendState {
    pub fn new(capabilities: Capabilities) -> Self {
        Self { capabilities, last_attempt: None, next_command: 0,
            snapshot: Snapshot::default(), pending: BTreeMap::new() }
    }

    pub fn snapshot(&self) -> &Snapshot { &self.snapshot }

    fn dispatch(&mut self, attempt: AttemptId, operation: Operation, effects: &mut Vec<Fx>) {
        self.next_command = self.next_command.checked_add(1).expect("playback command IDs exhausted");
        let command = CommandId(self.next_command);
        self.pending.insert(command, operation);
        effects.push(Fx::Dispatch { attempt, command, operation });
    }

    pub fn step(&mut self, event: Event, effects: &mut Vec<Fx>) {
        match event {
            Event::Intent(Intent::Start { attempt, source, requirements }) => {
                if self.snapshot.phase != Phase::Vacant {
                    effects.push(Fx::Rejected { attempt, failure: Failure::Busy });
                } else if self.last_attempt.is_some_and(|last| attempt <= last) {
                    effects.push(Fx::Rejected { attempt, failure: Failure::Stale });
                } else if !self.capabilities.supports(requirements) {
                    effects.push(Fx::Rejected { attempt, failure: Failure::Unsupported });
                } else {
                    self.last_attempt = Some(attempt);
                    self.snapshot.phase = Phase::Starting;
                    self.snapshot.attempt = Some(attempt);
                    self.dispatch(attempt, Operation::Start { source, requirements }, effects);
                }
            }
            Event::Intent(Intent::Control { attempt, operation }) => {
                if self.snapshot.attempt != Some(attempt) || self.snapshot.phase != Phase::Active {
                    effects.push(Fx::Rejected { attempt, failure: Failure::Stale });
                } else if !self.supports_operation(operation) {
                    effects.push(Fx::Rejected { attempt, failure: Failure::Unsupported });
                } else {
                    self.dispatch(attempt, operation, effects);
                }
            }
            Event::Intent(Intent::Stop { attempt }) => {
                if self.snapshot.attempt == Some(attempt)
                    && matches!(self.snapshot.phase, Phase::Starting | Phase::Active)
                {
                    self.snapshot.phase = Phase::Releasing;
                    self.pending.clear();
                    self.dispatch(attempt, Operation::Stop, effects);
                }
            }
            Event::CommandAnswer { attempt, command, result } => {
                if self.snapshot.attempt != Some(attempt) { return; }
                let Some(operation) = self.pending.remove(&command) else { return; };
                match result {
                    DispatchResult::Queued => self.snapshot.last_queued = Some(command),
                    DispatchResult::Refused(reason) => {
                        self.snapshot.failure = Some(reason);
                        if matches!(operation, Operation::Start { .. }) {
                            // The driver still owns the resource verdict, even when it refused
                            // the start. It must report a confirmed release before reuse.
                            self.snapshot.phase = Phase::Releasing;
                        } else if matches!(operation, Operation::Stop) {
                            self.snapshot.phase = Phase::Quarantined;
                            self.snapshot.failure = Some(Failure::ReleaseUncertain);
                        }
                    }
                }
            }
            Event::Observation { attempt, observation }
                if self.snapshot.attempt == Some(attempt)
                    && matches!(self.snapshot.phase, Phase::Starting | Phase::Active) =>
            {
                if observation == Observation::Presented && self.snapshot.presented { return; }
                match observation {
                    Observation::Playback(value) => {
                        self.snapshot.playback = Some(value);
                        self.snapshot.phase = Phase::Active;
                    }
                    Observation::Position(value) => self.snapshot.position = Some(value),
                    Observation::Presented => self.snapshot.presented = true,
                    Observation::TrackSelected(value) => match value.value.kind {
                        TrackKind::Audio => self.snapshot.audio_track = Some(value),
                        TrackKind::Subtitle => self.snapshot.subtitle_track = Some(value),
                    },
                    Observation::Failed(reason) => self.snapshot.failure = Some(reason),
                }
                effects.push(Fx::AcceptedObservation { attempt, observation });
            }
            Event::Observation { .. } => {}
            Event::Release { attempt, status } if self.snapshot.attempt == Some(attempt) => {
                match status {
                    ReleaseStatus::Closing => {
                        self.snapshot.phase = Phase::Releasing;
                        self.pending.clear();
                    }
                    ReleaseStatus::Quarantined => {
                        self.snapshot.phase = Phase::Quarantined;
                        self.snapshot.failure = Some(Failure::ReleaseUncertain);
                        self.pending.clear();
                    }
                    ReleaseStatus::Released => {
                        self.pending.clear();
                        self.snapshot = Snapshot::default();
                    }
                }
                effects.push(Fx::ReleaseOutcome { attempt, status });
            }
            Event::Release { .. } => {}
        }
    }

    fn supports_operation(&self, operation: Operation) -> bool {
        match operation {
            Operation::Play | Operation::Pause => self.capabilities.pause,
            Operation::Seek(_) => self.capabilities.seek,
            Operation::SelectTrack(track) => match track.kind {
                TrackKind::Audio => self.capabilities.audio_track,
                TrackKind::Subtitle => self.capabilities.subtitle_track,
            },
            Operation::Start { .. } | Operation::Stop => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EvidenceSource, Fidelity, Requirements, SourceKind};

    const CAPS: Capabilities = Capabilities {
        progressive_uri: true, hls_uri: false, packet_feed: false,
        exact_video: true, exact_audio: true, pause: true, seek: true,
        audio_track: true, subtitle_track: true,
    };
    const REQ: Requirements = Requirements {
        source: SourceKind::ProgressiveUri, video: Fidelity::Exact, audio: Fidelity::Exact,
        audio_track: None, subtitle_track: None,
    };
    fn step(state: &mut BackendState, event: Event) -> Vec<Fx> {
        let mut effects = Vec::new(); state.step(event, &mut effects); effects
    }
    fn start(state: &mut BackendState, attempt: AttemptId) -> CommandId {
        let effects = step(state, Event::Intent(Intent::Start { attempt, source: SourceKey::new(7), requirements: REQ }));
        let [Fx::Dispatch { attempt, command, operation: Operation::Start { .. } }] = effects.as_slice() else { panic!("start dispatch: {effects:?}") };
        assert_eq!(*attempt, state.snapshot().attempt.unwrap());
        *command
    }
    #[test]
    fn queued_is_not_playing_and_late_or_wrong_answers_cannot_publish() {
        let mut state = BackendState::new(CAPS);
        let attempt = AttemptId::new(7, 1);
        let command = start(&mut state, attempt);
        let wrong = AttemptId::new(7, 2);
        step(&mut state, Event::CommandAnswer { attempt: wrong, command, result: DispatchResult::Queued });
        step(&mut state, Event::CommandAnswer { attempt, command: CommandId(command.get() + 1), result: DispatchResult::Queued });
        assert_eq!(state.snapshot().last_queued, None);
        step(&mut state, Event::CommandAnswer { attempt, command, result: DispatchResult::Queued });
        assert_eq!(state.snapshot().last_queued, Some(command));
        assert_eq!(state.snapshot().playback, None);
        step(&mut state, Event::Observation { attempt: wrong, observation: Observation::Presented });
        assert!(!state.snapshot().presented);
        step(&mut state, Event::Observation { attempt, observation: Observation::Playback(Observed { value: Playback::Playing, from: EvidenceSource::BackendReport }) });
        assert_eq!(state.snapshot().playback.unwrap().value, Playback::Playing);
        assert!(!state.snapshot().presented, "a service report cannot prove a visible frame");
    }
    #[test]
    fn refusal_preserves_observed_state_and_stop_refusal_quarantines() {
        let mut state = BackendState::new(CAPS);
        let attempt = AttemptId::new(7, 1);
        start(&mut state, attempt);
        step(&mut state, Event::Observation { attempt, observation: Observation::Playback(Observed { value: Playback::Playing, from: EvidenceSource::BackendReport }) });
        let candidate = AttemptId::new(8, 1);
        assert_eq!(step(&mut state, Event::Intent(Intent::Start { attempt: candidate, source: SourceKey::new(8), requirements: REQ })),
            vec![Fx::Rejected { attempt: candidate, failure: Failure::Busy }]);
        let effects = step(&mut state, Event::Intent(Intent::Control { attempt, operation: Operation::Pause }));
        let [Fx::Dispatch { command, .. }] = effects.as_slice() else { panic!("pause") };
        step(&mut state, Event::CommandAnswer { attempt, command: *command, result: DispatchResult::Refused(Failure::Refused) });
        assert_eq!(state.snapshot().playback.unwrap().value, Playback::Playing);
        assert_eq!(state.snapshot().failure, Some(Failure::Refused));
        let effects = step(&mut state, Event::Intent(Intent::Stop { attempt }));
        let [Fx::Dispatch { command, .. }] = effects.as_slice() else { panic!("stop") };
        step(&mut state, Event::CommandAnswer { attempt, command: *command, result: DispatchResult::Refused(Failure::Refused) });
        assert_eq!(state.snapshot().phase, Phase::Quarantined);
        assert_eq!(step(&mut state, Event::Intent(Intent::Start { attempt: candidate, source: SourceKey::new(8), requirements: REQ })),
            vec![Fx::Rejected { attempt: candidate, failure: Failure::Busy }]);
    }
    #[test]
    fn confirmed_release_resets_sparse_snapshot_without_reusing_ids() {
        let mut state = BackendState::new(CAPS);
        let attempt = AttemptId::new(7, 1);
        let command = start(&mut state, attempt);
        step(&mut state, Event::Observation { attempt, observation: Observation::Position(Observed { value: ContentTime::from_millis(12_345), from: EvidenceSource::BackendReport }) });
        step(&mut state, Event::Release { attempt, status: ReleaseStatus::Quarantined });
        assert_eq!(state.snapshot().position.unwrap().value.as_millis(), 12_345);
        step(&mut state, Event::Release { attempt, status: ReleaseStatus::Released });
        assert_eq!(*state.snapshot(), Snapshot::default());
        let next = AttemptId::new(7, 2);
        let next_command = start(&mut state, next);
        assert!(next > attempt && next_command > command);
        step(&mut state, Event::Observation { attempt, observation: Observation::Presented });
        step(&mut state, Event::CommandAnswer { attempt, command, result: DispatchResult::Queued });
        assert_eq!(state.snapshot().attempt, Some(next));
        assert!(!state.snapshot().presented);
        assert_eq!(state.snapshot().last_queued, None);
    }
    #[test]
    fn unsupported_requirements_never_dispatch() {
        let mut state = BackendState::new(CAPS);
        let req = Requirements { source: SourceKind::HlsUri, ..REQ };
        assert_eq!(step(&mut state, Event::Intent(Intent::Start { attempt: AttemptId::new(7, 1), source: SourceKey::new(9), requirements: req })),
            vec![Fx::Rejected { attempt: AttemptId::new(7, 1), failure: Failure::Unsupported }]);
        assert_eq!(*state.snapshot(), Snapshot::default());
    }
    #[test]
    fn refused_start_needs_an_explicit_release_verdict() {
        let mut state = BackendState::new(CAPS);
        let attempt = AttemptId::new(7, 1);
        let command = start(&mut state, attempt);
        step(&mut state, Event::CommandAnswer { attempt, command, result: DispatchResult::Refused(Failure::Refused) });
        assert_eq!(state.snapshot().phase, Phase::Releasing);
        assert_eq!(step(&mut state, Event::Intent(Intent::Start { attempt: AttemptId::new(8, 1), source: SourceKey::new(8), requirements: REQ })),
            vec![Fx::Rejected { attempt: AttemptId::new(8, 1), failure: Failure::Busy }]);
        step(&mut state, Event::Release { attempt, status: ReleaseStatus::Released });
        assert_eq!(state.snapshot().phase, Phase::Vacant);
    }
}
