//! Main-thread readout for one physical playback attempt. A backend publishes sparse facts
//! addressed to the attempt that produced them; an absent fact remains unknown across a reset.
//! This is a value owned by `PlaybackSession`, never a second process-wide mailbox.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend { Feed, Uri }

pub use plx_playback_contract::readout::{AttemptFacts, PlaybackState, RefusalPath, RuntimeFailure};

#[derive(Clone, Copy)]
pub struct AttemptView<A> {
    attempt: Option<A>,
    backend: Option<Backend>,
    facts: AttemptFacts,
}

impl<A: Copy + Eq> AttemptView<A> {
    pub const EMPTY: Self = Self { attempt: None, backend: None, facts: AttemptFacts::EMPTY };

    pub fn begin(&mut self, attempt: A, backend: Backend) {
        *self = Self { attempt: Some(attempt), backend: Some(backend), ..Self::EMPTY };
    }

    pub fn attempt(&self) -> Option<A> { self.attempt }
    pub fn backend(&self) -> Option<Backend> { self.backend }

    pub fn facts(&self) -> &AttemptFacts { &self.facts }

    /// Whether `attempt` on `backend` is the one this view belongs to: the address every event
    /// carrying one attempt's facts passes before it changes anything.
    pub fn addresses(&self, attempt: A, backend: Backend) -> bool {
        self.attempt == Some(attempt) && self.backend == Some(backend)
    }

    /// An old worker or an unrelated backend cannot change the current readout.
    pub fn ingest(&mut self, attempt: A, backend: Backend, incoming: AttemptFacts) -> bool {
        if !self.addresses(attempt, backend) { return false; }
        self.facts = incoming;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_attempt_resets_every_fact_and_refuses_late_publishers() {
        let (a, b) = (1_u64, 2_u64);
        let mut view = AttemptView::EMPTY;
        view.begin(a, Backend::Feed);
        assert!(view.ingest(a, Backend::Feed, AttemptFacts { position_ns: Some(42), reported_frame: Some(true), ..Default::default() }));
        view.begin(b, Backend::Uri);
        assert_eq!(view.facts().position_ns, None);
        assert_eq!(view.facts().reported_frame, None);
        assert!(view.backend() == Some(Backend::Uri));
        assert!(!view.ingest(a, Backend::Feed, AttemptFacts { position_ns: Some(99), ..Default::default() }));
        assert_eq!(view.facts().position_ns, None);
        assert!(view.ingest(b, Backend::Uri, AttemptFacts { duration_ns: Some(100), ..Default::default() }));
        assert_eq!(view.facts().duration_ns, Some(100));
        assert!(!view.ingest(b, Backend::Feed, AttemptFacts { position_ns: Some(999), ..Default::default() }));
        assert_eq!(view.facts().duration_ns, Some(100), "wrong backend cannot replace even a reused attempt identity");
        assert!(view.ingest(b, Backend::Uri, AttemptFacts::default()));
        assert_eq!(view.facts().duration_ns, None, "a sparse replacement cannot carry stale evidence");
    }
}
