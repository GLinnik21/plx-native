//! **What became of a watched report, per event id.**
//!
//! A report the person can quote a Report ID for — an onboarding incident, standing or one-off —
//! is WATCHED here from the moment its lane takes it, and every place that decides its fate says
//! so against its event id: the flush (`super::process_records`) when a server answers or a record
//! is retired unsent, the spool (`super::spool`) when its cap or a withdrawal discards one, and the
//! one-off's bounded direct fallback (`super::oneoff`). The session adapter reads [`state`] once a
//! frame and hands each change to the incident owner, so "sent" on screen means a server accepted
//! it and nothing less.
//!
//! Only watched ids are tracked: [`settle`] updates an entry and never creates one, so the flush
//! can report every record it touches without the table growing past [`MAX_WATCHED`].
//!
//! [`forget`] ends the departing account's claim on all of it: it bumps the tenure counter — which
//! a one-off append and its fallback worker are fenced by — and clears the table, so a stale
//! worker cannot publish into the next account's states.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// How many reports are watched. A person presses Send a handful of times in one launch at most;
/// the bound exists so that a stuck screen cannot grow this without limit.
const MAX_WATCHED: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeliveryState {
    /// In the durable spool; no send has settled it yet.
    Queued,
    /// The one-off's direct fallback is on the network.
    Sending,
    /// A send was tried and did not get through for a reason that may pass — no response at all,
    /// or the server said not now (408, 429, 5xx). Still in the durable spool; a later flush
    /// tries again.
    Held,
    /// A server accepted it (2xx).
    Delivered,
    /// It will never be delivered: the server refused it (any other 4xx), it was discarded unsent
    /// (no endpoint in this build, a withdrawal, the spool's cap), or the one-off's direct
    /// fallback failed.
    Failed,
}

impl DeliveryState {
    fn settled(self) -> bool {
        matches!(self, DeliveryState::Delivered | DeliveryState::Failed)
    }
}

static TABLE: Mutex<Vec<(String, DeliveryState)>> = Mutex::new(Vec::new());
static TENURE: AtomicU64 = AtomicU64::new(0);

fn table() -> std::sync::MutexGuard<'static, Vec<(String, DeliveryState)>> {
    TABLE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The account tenure a caller is acting in, to be checked again with [`current`].
pub(crate) fn tenure() -> u64 {
    TENURE.load(Ordering::Acquire)
}

/// Is `tenure` still the account's? `false` once [`forget`] has run since it was read.
pub(crate) fn current(tenure: u64) -> bool {
    TENURE.load(Ordering::Acquire) == tenure
}

/// Start watching `event_id` in `state`, or move a watched one to it. Refused (`false`) when
/// `tenure` has ended, or when every watched report is on the network and none can make room.
pub(crate) fn watch(event_id: &str, state: DeliveryState, tenure: u64) -> bool {
    let mut t = table();
    if !current(tenure) {
        return false;
    }
    if let Some((_, existing)) = t.iter_mut().find(|(id, _)| id == event_id) {
        *existing = state;
        return true;
    }
    if t.len() >= MAX_WATCHED {
        // Evict the oldest that is not on the network.
        match t.iter().position(|(_, s)| *s != DeliveryState::Sending) {
            Some(i) => {
                t.remove(i);
            }
            None => return false,
        }
    }
    t.push((event_id.to_string(), state));
    true
}

/// What a sender learned about `event_id`. Only a watched report is updated, and a settled one
/// (delivered or failed) stays settled.
pub(crate) fn settle(event_id: &str, state: DeliveryState) {
    let mut t = table();
    if let Some((_, existing)) = t.iter_mut().find(|(id, _)| id == event_id) {
        if !existing.settled() {
            *existing = state;
        }
    }
}

/// [`settle`], from a worker that may have outlived the tenure it started in.
pub(crate) fn settle_if_current(event_id: &str, state: DeliveryState, tenure: u64) {
    let mut t = table();
    if !current(tenure) {
        return;
    }
    if let Some((_, existing)) = t.iter_mut().find(|(id, _)| id == event_id) {
        if !existing.settled() {
            *existing = state;
        }
    }
}

/// A build that can send nothing (no endpoint configured): every watched report still unsettled
/// will never be delivered.
pub(crate) fn fail_unsettled() {
    for (_, s) in table().iter_mut().filter(|(_, s)| !s.settled()) {
        *s = DeliveryState::Failed;
    }
}

/// What became of the watched report with this event id, or `None` once it is forgotten or
/// evicted.
pub(crate) fn state(event_id: &str) -> Option<DeliveryState> {
    table().iter().find(|(id, _)| id == event_id).map(|(_, s)| *s)
}

/// End the departing account's claim on every watched report. Called by sign-out and Delete all
/// local data (`app::adapters::consent`'s forget path) BEFORE the spool is erased, so a one-off
/// append racing it is refused rather than surviving the erasure.
pub(crate) fn forget() {
    TENURE.fetch_add(1, Ordering::AcqRel);
    table().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_watched_report_is_tracked_and_a_settled_one_stays_settled() {
        let _g = crate::testlock::serial();
        forget();
        settle("never-watched", DeliveryState::Delivered);
        assert_eq!(state("never-watched"), None, "the flush reports every record; only watched ones are kept");
        assert!(watch("r", DeliveryState::Queued, tenure()));
        settle("r", DeliveryState::Held);
        assert_eq!(state("r"), Some(DeliveryState::Held));
        settle("r", DeliveryState::Delivered);
        settle("r", DeliveryState::Failed);
        assert_eq!(state("r"), Some(DeliveryState::Delivered), "a delivered report stays delivered");
        forget();
    }

    #[test]
    fn a_build_that_can_send_nothing_fails_what_is_still_unsettled() {
        let _g = crate::testlock::serial();
        forget();
        let t = tenure();
        assert!(watch("done", DeliveryState::Queued, t));
        settle("done", DeliveryState::Delivered);
        assert!(watch("waiting", DeliveryState::Held, t));
        fail_unsettled();
        assert_eq!((state("done"), state("waiting")), (Some(DeliveryState::Delivered), Some(DeliveryState::Failed)));
        forget();
    }

    #[test]
    fn a_stale_tenure_cannot_watch_or_settle() {
        let _g = crate::testlock::serial();
        forget();
        let old = tenure();
        assert!(watch("r", DeliveryState::Sending, old));
        forget();
        assert_eq!(state("r"), None);
        assert!(!watch("r", DeliveryState::Queued, old));
        settle_if_current("r", DeliveryState::Delivered, old);
        assert_eq!(state("r"), None);
    }

    #[test]
    fn the_table_is_bounded_and_never_evicts_a_report_on_the_network() {
        let _g = crate::testlock::serial();
        forget();
        let t = tenure();
        for i in 0..MAX_WATCHED {
            assert!(watch(&format!("s{i}"), DeliveryState::Sending, t));
        }
        assert!(!watch("one-more", DeliveryState::Queued, t), "all on the network: no room");
        settle("s0", DeliveryState::Delivered);
        assert!(watch("one-more", DeliveryState::Queued, t));
        assert_eq!(state("s0"), None, "the oldest not on the network made room");
        forget();
    }
}
