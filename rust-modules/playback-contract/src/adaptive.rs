//! Addressed adaptive trial phases. The backend keeps source handles and staged media;
//! this contract carries only identities, normalized measurements, and ordering.

use crate::{AttemptId, SourceKey};
use std::time::Duration;

/// A worker-local trial serial under the route owner's existing playback attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TrialId { pub attempt: AttemptId, pub serial: u64 }

impl TrialId {
    pub const fn new(attempt: AttemptId, serial: u64) -> Self { Self { attempt, serial } }
}

/// One completed or abandoned acquisition under the route owner's attempt.
/// Replies from a superseded read cannot become evidence for a later round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RoundId { pub attempt: AttemptId, pub serial: u64 }

impl RoundId {
    pub const fn new(attempt: AttemptId, serial: u64) -> Self { Self { attempt, serial } }
}

/// Physical work on the active cursor. The feed owns its cursor, access units,
/// timeline, pause cursor, and prefetch slot; these instructions carry none of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveAction {
    Read { round: RoundId, at_floor: Option<bool> },
    Feed { round: RoundId },
    Buffer { round: RoundId },
    Pause { round: RoundId },
    RollbackDuration { round: RoundId },
    Prefetch { round: RoundId, at_floor: Option<bool> },
}

impl ActiveAction {
    pub const fn round(self) -> RoundId {
        match self {
            Self::Read { round, .. } | Self::Feed { round }
            | Self::Buffer { round } | Self::Pause { round }
            | Self::RollbackDuration { round } | Self::Prefetch { round, .. } => round,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveObservation {
    Ready { round: RoundId, acquisition: Acquisition, delivered: bool, abandoned: bool },
    Ended { round: RoundId },
    Fed { round: RoundId },
    Buffer { round: RoundId, fact: BufferFact },
    Pause { round: RoundId, paused: bool, sequence: u64, completed_ms: Option<u64> },
    RollbackDuration { round: RoundId, duration_ms: Option<u32> },
    Prefetched { round: RoundId },
}

impl ActiveObservation {
    pub const fn round(self) -> RoundId {
        match self {
            Self::Ready { round, .. } | Self::Ended { round }
            | Self::Fed { round } | Self::Buffer { round, .. }
            | Self::Pause { round, .. } | Self::RollbackDuration { round, .. }
            | Self::Prefetched { round } => round,
        }
    }
}

pub trait ActiveFeedPort {
    type Error;
    fn step_active(&mut self, action: ActiveAction) -> Result<ActiveObservation, Self::Error>;
}

/// Instructions for the existing feed worker. A source is an opaque local key;
/// its URL and authentication remain in that worker's private source binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedAction {
    CheckAbort { trial: TrialId },
    CheckExpired { trial: TrialId },
    BudgetSnapshot { trial: TrialId },
    PrimeDeadline { trial: TrialId },
    DeadlineFinal { trial: TrialId, snapshot: Option<std::time::Instant> },
    Open { trial: TrialId, source: SourceKey },
    ReadNext { trial: TrialId },
    AcquireFirst {
        trial: TrialId,
        media_budget: Option<Duration>,
        yields_to_rebuffer: bool,
        started_ns: i64,
        clock: TrialClock,
    },
    AcquireFollowup { trial: TrialId },
    StageAcquired { trial: TrialId },
    CheckFollowupReserve { trial: TrialId },
    QueueStaged { trial: TrialId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialClock { Playhead, UnpausedElapsed }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomaticTransition { Original, QualityUp, QualityDown }

/// Physical automatic-actuator lease and the existing queue abort fence. The
/// policy owner holds the lease across blocking work and enters `gate` only for
/// the short route/controller publication closure.
pub trait AutomaticPort {
    type Lease;
    fn begin(&mut self, kind: AutomaticTransition, expected_user_sequence: u64)
        -> Option<Self::Lease>;
    fn authorize(&mut self, lease: &Self::Lease) -> bool;
    fn aborted(&self) -> bool;
    fn gate(&mut self) -> &mut dyn PublicationGate;
}

pub trait TrialActuatorPort: AutomaticPort {
    type TrialReserve;
    fn arm_trial_reserve(&mut self, runway_ms: i64, expected_user_sequence: u64)
        -> Option<Self::TrialReserve>;
    fn lease_token(&self, lease: &Self::Lease) -> u64;
    fn trial_token(&self, reserve: &Self::TrialReserve) -> u64;
}

/// Physical facts available to one adaptive decision on the existing feed
/// worker. Associated samplers keep the playhead read callable inside the AQ
/// publication closure without borrowing the actuator port through its gate.
pub trait AdaptiveRoundPort: TrialActuatorPort {
    fn position_ns() -> i64;
    fn transition_permitted(kind: AutomaticTransition) -> bool;
    fn rebuffer_state() -> (bool, bool);
    fn buffer_snapshot() -> (Option<i64>, i64);
}

/// Physical transfer and output geometry before a candidate policy verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Acquisition {
    pub bytes: u64,
    pub active_us: u64,
    pub total_us: u64,
    pub duration: Duration,
    pub width: i32,
    pub height: i32,
    pub declared_bps: u64,
}

/// Normalized content-time tails sampled by the physical feed. A missing audio
/// tail remains absent even when audio is expected; it is not an empty buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferFact {
    pub playback_ms: i64,
    pub video_tail_ms: i64,
    pub audio_tail_ms: Option<i64>,
    pub audio_expected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveQuantum {
    pub acquisition: Acquisition,
    pub buffer: BufferFact,
    pub abandoned: bool,
    pub delivered: bool,
}

/// Accepted user-pause evidence after a blocking probe or playlist read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PauseFact {
    pub paused: bool,
    pub completed_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedFailure {
    Aborted, Deadline, Ended, NotReady, Parse, SourceMoved, Protocol, TimelineOverflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedObservation {
    AbortStatus { trial: TrialId, aborted: bool },
    ExpiryStatus { trial: TrialId, expired: bool },
    BudgetSnapshot {
        trial: TrialId, buffered_ms: Option<i64>, started_ns: i64,
        remaining: Option<Duration>, rebuffering: bool,
    },
    PrimeDeadline { trial: TrialId, deadline: Option<std::time::Instant> },
    DeadlineFinal { trial: TrialId, final_expiry: bool },
    Opened { trial: TrialId, declared_bps: u64 },
    SegmentReady { trial: TrialId, duration: std::time::Duration },
    Acquired { trial: TrialId, acquisition: Acquisition },
    Staged { trial: TrialId, buffer: BufferFact, count: usize },
    FollowupReserve { trial: TrialId, funded: bool, remaining_ms: Option<u64> },
    Queued { trial: TrialId },
    Failed { trial: TrialId, failure: FeedFailure },
}

impl FeedObservation {
    pub const fn trial(self) -> TrialId {
        match self {
            Self::AbortStatus { trial, .. } | Self::ExpiryStatus { trial, .. }
            | Self::BudgetSnapshot { trial, .. }
            | Self::PrimeDeadline { trial, .. } | Self::DeadlineFinal { trial, .. }
            | Self::Opened { trial, .. }
            | Self::SegmentReady { trial, .. }
            | Self::Acquired { trial, .. }
            | Self::Staged { trial, .. }
            | Self::FollowupReserve { trial, .. }
            | Self::Queued { trial }
            | Self::Failed { trial, .. } => trial,
        }
    }
}

/// One worker-local IO port. Binding a locator is a runner operation; stepped
/// policy actions see only SourceKey. Physical cursor state stays in the port.
pub trait FeedPort {
    type Locator;
    fn bind_source(&mut self, source: SourceKey, locator: Self::Locator);
    fn step(&mut self, action: FeedAction) -> FeedObservation;
}

/// The feed worker's physical clock fence. It is armed before candidate AUs
/// enter the queue and settled only after the queued route has been published
/// and its cursor promoted. Dropping an unsettled fence leaves teardown to
/// reset the generation.
pub trait CandidateFencePort {
    type Fence;
    type Error;
    fn arm(&mut self, automatic_token: u64) -> Result<Self::Fence, Self::Error>;
    fn settle(&mut self, fence: Self::Fence);
}

/// Synchronous AQ abort fence. The closure is short and performs no IO. It is
/// invoked exactly once while the queue lock is held, or not invoked on abort.
pub trait PublicationGate {
    fn if_not_aborted(&mut self, transition: &mut dyn FnMut()) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialPhase {
    Granted,
    Prepared,
    FirstAcquired,
    FollowupRequested,
    FollowupAcquired,
    QueueAuthorized,
    QueueStarted,
    Queued,
    Published,
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialObservation {
    Prepared,
    FirstAcquired,
    FollowupRequested,
    FollowupAcquired,
    QueueAuthorized,
    QueueStarted,
    QueueCompleted,
    Published,
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialProtocolError { Stale, OutOfOrder, TerminalAfterQueue }

/// The synchronous owner of one trial's phase. IO outcomes are addressed to this ID;
/// a late result cannot settle the next trial, even if it names the same source.
#[derive(Debug, PartialEq, Eq)]
pub struct TrialProtocol { id: TrialId, phase: TrialPhase }

impl TrialProtocol {
    pub const fn new(id: TrialId) -> Self { Self { id, phase: TrialPhase::Granted } }
    pub const fn id(&self) -> TrialId { self.id }
    pub const fn phase(&self) -> TrialPhase { self.phase }

    pub fn observe(
        &mut self, id: TrialId, observation: TrialObservation,
    ) -> Result<TrialPhase, TrialProtocolError> {
        if id != self.id { return Err(TrialProtocolError::Stale); }
        use TrialObservation as O;
        use TrialPhase as P;
        self.phase = match (self.phase, observation) {
            (P::Granted, O::Prepared) => P::Prepared,
            (P::Prepared, O::FirstAcquired) => P::FirstAcquired,
            (P::FirstAcquired, O::FollowupRequested) => P::FollowupRequested,
            (P::FollowupRequested, O::FollowupAcquired) => P::FollowupAcquired,
            (P::FirstAcquired | P::FollowupAcquired, O::QueueAuthorized) => P::QueueAuthorized,
            (P::QueueAuthorized, O::QueueStarted) => P::QueueStarted,
            (P::QueueStarted, O::QueueCompleted) => P::Queued,
            (P::Queued, O::Published) => P::Published,
            (P::Granted | P::Prepared | P::FirstAcquired | P::FollowupRequested
                | P::FollowupAcquired | P::QueueAuthorized, O::Ended) => P::Ended,
            (P::QueueStarted | P::Queued, O::Ended) => {
                self.phase = P::Ended;
                return Err(TrialProtocolError::TerminalAfterQueue);
            }
            _ => return Err(TrialProtocolError::OutOfOrder),
        };
        Ok(self.phase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_bearing_trial_requires_both_staged_objects_before_queue_and_publication() {
        let id = TrialId::new(AttemptId::new(7, 9), 3);
        let mut trial = TrialProtocol::new(id);
        assert_eq!(trial.observe(id, TrialObservation::Published), Err(TrialProtocolError::OutOfOrder));
        for observation in [
            TrialObservation::Prepared, TrialObservation::FirstAcquired,
            TrialObservation::FollowupRequested, TrialObservation::FollowupAcquired,
            TrialObservation::QueueAuthorized, TrialObservation::QueueStarted,
            TrialObservation::QueueCompleted,
        ] {
            trial.observe(id, observation).unwrap();
        }
        assert_eq!(trial.observe(id, TrialObservation::FollowupRequested),
            Err(TrialProtocolError::OutOfOrder));
        assert_eq!(trial.observe(TrialId::new(id.attempt, 4), TrialObservation::Published),
            Err(TrialProtocolError::Stale));
        assert_eq!(trial.observe(id, TrialObservation::Published), Ok(TrialPhase::Published));

        let mut failed = TrialProtocol::new(id);
        for observation in [
            TrialObservation::Prepared, TrialObservation::FirstAcquired,
            TrialObservation::QueueAuthorized, TrialObservation::QueueStarted,
            TrialObservation::QueueCompleted,
        ] {
            failed.observe(id, observation).unwrap();
        }
        assert_eq!(failed.observe(id, TrialObservation::Ended),
            Err(TrialProtocolError::TerminalAfterQueue));
        assert_eq!(failed.phase(), TrialPhase::Ended);
        assert_eq!(failed.observe(id, TrialObservation::Published),
            Err(TrialProtocolError::OutOfOrder));
    }

    #[test]
    fn failed_prequeue_trial_ends_without_authorizing_publication() {
        let id = TrialId::new(AttemptId::new(1, 2), 1);
        let mut trial = TrialProtocol::new(id);
        trial.observe(id, TrialObservation::Prepared).unwrap();
        trial.observe(id, TrialObservation::FirstAcquired).unwrap();
        assert_eq!(trial.observe(id, TrialObservation::Ended), Ok(TrialPhase::Ended));
        assert_eq!(trial.observe(id, TrialObservation::Published), Err(TrialProtocolError::OutOfOrder));
    }
}
