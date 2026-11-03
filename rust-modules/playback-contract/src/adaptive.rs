//! Addressed adaptive trial phases. The backend keeps source handles and staged media;
//! this contract carries only identities, normalized measurements, and ordering.

use crate::AttemptId;

/// A worker-local trial serial under the route owner's existing playback attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TrialId { pub attempt: AttemptId, pub serial: u64 }

impl TrialId {
    pub const fn new(attempt: AttemptId, serial: u64) -> Self { Self { attempt, serial } }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrialProtocol { id: TrialId, phase: TrialPhase }

impl TrialProtocol {
    pub const fn new(id: TrialId) -> Self { Self { id, phase: TrialPhase::Granted } }
    pub const fn id(self) -> TrialId { self.id }
    pub const fn phase(self) -> TrialPhase { self.phase }

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
