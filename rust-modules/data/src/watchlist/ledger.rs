//! The watchlist intent ledger (plan §1.4): optimistic flip, same-id presses coalesce with the
//! newest intent winning, one ordered write worker, a failed write reverts, no toast. An intent
//! recorded here beats any list read started before the write's 200.
//!
//! One entry per id: the newest intent shown (`shown`), the membership the service is last known
//! to hold (`settled`) and a sequence number. A press numbers the entry; a landed write renumbers
//! it higher, so a list read stamped before the landing is overridden and one stamped after it is
//! not (the same rule as `viewledger`).

/// What a press asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    Add,
    Remove,
}

/// A queued write the worker takes, in press order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingWrite {
    pub hex: [u8; 12],
    pub intent: Intent,
    /// Press sequence; a list read stamped below it is superseded for this id.
    pub seq: u64,
}

/// How a write ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The service took it (2xx): the intent now stands until a later list read agrees.
    Landed,
    /// Refused or unreachable: the flip reverts to the server's last known membership.
    Failed,
}

use std::collections::{HashMap, VecDeque};

#[derive(Clone, Copy, Debug)]
struct Entry {
    /// The newest intent pressed: what the row shows.
    shown: Intent,
    /// Membership the service is last known to hold (the inverse of the first press until a write lands).
    settled: Intent,
    /// Press sequence, renumbered higher when a write lands. A read stamped below it is superseded.
    seq: u64,
}

#[derive(Debug, Default)]
pub struct IntentLedger {
    seq: u64,
    entries: HashMap<[u8; 12], Entry>,
    /// Writes not yet handed to the worker, oldest first; at most one per id.
    queue: VecDeque<PendingWrite>,
    /// The write the single worker holds. The next one is not handed out until it finishes.
    in_flight: Option<PendingWrite>,
}

fn inverse(i: Intent) -> Intent {
    match i {
        Intent::Add => Intent::Remove,
        Intent::Remove => Intent::Add,
    }
}

impl IntentLedger {
    pub fn new() -> IntentLedger {
        IntentLedger::default()
    }

    /// A press: flip optimistically and queue the write. A second press on the same id replaces
    /// the queued intent (newest wins); a queued intent that equals what the service already holds
    /// is no write at all. Returns the sequence number.
    pub fn press(&mut self, hex: [u8; 12], intent: Intent) -> u64 {
        self.seq += 1;
        let seq = self.seq;
        let e = self.entries.entry(hex).or_insert(Entry { shown: intent, settled: inverse(intent), seq });
        e.shown = intent;
        e.seq = seq;
        let settled = e.settled;
        match self.queue.iter().position(|w| w.hex == hex) {
            Some(at) if intent == settled => {
                self.queue.remove(at);
            }
            Some(at) => self.queue[at] = PendingWrite { hex, intent, seq },
            None if intent == settled && self.in_flight.is_none_or(|w| w.hex != hex) => {}
            None => self.queue.push_back(PendingWrite { hex, intent, seq }),
        }
        seq
    }

    /// The next write for the single ordered worker, oldest first, or `None` (also `None` while
    /// the worker still holds one: the writes go out one at a time, in press order).
    pub fn next_write(&mut self) -> Option<PendingWrite> {
        if self.in_flight.is_some() {
            return None;
        }
        let w = self.queue.pop_front()?;
        self.in_flight = Some(w);
        Some(w)
    }

    /// A write handed out but never started (the thread was refused): back to the front of the queue.
    pub fn requeue(&mut self, write: PendingWrite) {
        if self.in_flight == Some(write) {
            self.in_flight = None;
            self.queue.push_front(write);
        }
    }

    /// The worker's verdict on a write. `Failed` reverts the id; returns the membership to show:
    /// the newest pressed intent if one is still pending, else (on failure) the service's last
    /// known membership. `None` for an id the ledger no longer tracks.
    pub fn finished(&mut self, write: PendingWrite, outcome: WriteOutcome) -> Option<Intent> {
        if self.in_flight == Some(write) {
            self.in_flight = None;
        }
        self.seq += 1;
        let landed_seq = self.seq;
        let e = self.entries.get_mut(&write.hex)?;
        match outcome {
            WriteOutcome::Landed => {
                e.settled = write.intent;
                e.seq = landed_seq;
            }
            WriteOutcome::Failed => {}
        }
        let (shown, settled) = (e.shown, e.settled);
        // A queued write that now asks for what the service holds is no write.
        if let Some(at) = self.queue.iter().position(|w| w.hex == write.hex && w.intent == settled) {
            self.queue.remove(at);
        }
        let pending = self.queue.iter().any(|w| w.hex == write.hex);
        if outcome == WriteOutcome::Failed && !pending {
            // Nothing newer to keep: the row shows what the service holds, and a read no longer
            // needs overriding for it.
            self.entries.remove(&write.hex);
            return Some(settled);
        }
        if !pending {
            if let Some(e) = self.entries.get_mut(&write.hex) {
                e.shown = settled;
            }
            return Some(settled);
        }
        Some(shown)
    }

    /// The membership this id must show despite a list read stamped `read_seq`, if an intent
    /// newer than the read exists; `None` leaves the read's answer standing.
    pub fn overrides(&self, hex: &[u8; 12], read_seq: u64) -> Option<Intent> {
        self.entries.get(hex).filter(|e| e.seq > read_seq).map(|e| e.shown)
    }

    /// Every id a read stamped `read_seq` must not decide, with the membership to show.
    pub fn overridden(&self, read_seq: u64) -> impl Iterator<Item = ([u8; 12], Intent)> + '_ {
        self.entries.iter().filter(move |(_, e)| e.seq > read_seq).map(|(h, e)| (*h, e.shown))
    }

    /// A list read stamped `read_seq` has landed whole: entries it already postdates and that have
    /// no write queued or in flight are the read's to decide now.
    pub fn forget_older(&mut self, read_seq: u64) {
        let busy: Vec<[u8; 12]> = self.queue.iter().chain(self.in_flight.iter()).map(|w| w.hex).collect();
        self.entries.retain(|h, e| e.seq > read_seq || busy.contains(h));
    }

    /// Is any write queued or in flight?
    pub fn busy(&self) -> bool {
        self.in_flight.is_some() || !self.queue.is_empty()
    }

    /// The stamp a list read takes when it is issued.
    pub fn stamp(&self) -> u64 {
        self.seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 12] = [1; 12];
    const B: [u8; 12] = [2; 12];

    #[test]
    fn a_press_flips_and_queues_one_write() {
        let mut l = IntentLedger::new();
        let stamp = l.stamp();
        let seq = l.press(A, Intent::Add);
        assert_eq!(l.overrides(&A, stamp), Some(Intent::Add), "a read issued before the press is overridden");
        assert_eq!(l.overrides(&A, seq), None, "a read issued after it is not");
        assert_eq!(l.next_write(), Some(PendingWrite { hex: A, intent: Intent::Add, seq }));
        assert_eq!(l.next_write(), None);
    }

    #[test]
    fn same_id_presses_coalesce_and_the_newest_wins() {
        let mut l = IntentLedger::new();
        l.press(A, Intent::Add);
        l.press(B, Intent::Add);
        // A: Remove cancels the queued Add (back where the service is), the last Add queues anew:
        // one write per id, the newest intent.
        l.press(A, Intent::Remove);
        let last = l.press(A, Intent::Add);
        assert_eq!(l.next_write().map(|w| w.hex), Some(B));
        let w = l.next_write();
        assert_eq!(l.finished(PendingWrite { hex: B, intent: Intent::Add, seq: 2 }, WriteOutcome::Landed), Some(Intent::Add));
        assert_eq!(w, None, "B is still out");
        let a = l.next_write().unwrap();
        assert_eq!((a.hex, a.intent, a.seq), (A, Intent::Add, last));
        assert_eq!(l.next_write(), None, "exactly one write for A");
    }

    #[test]
    fn a_queued_intent_is_replaced_in_place() {
        let mut l = IntentLedger::new();
        l.press(A, Intent::Add);
        l.press(B, Intent::Add);
        let a = l.next_write().unwrap();
        l.finished(a, WriteOutcome::Landed);
        // B queued; A, now a member, is pressed Remove then Add then Remove again.
        l.press(A, Intent::Remove);
        l.press(A, Intent::Add);
        let last = l.press(A, Intent::Remove);
        assert_eq!(l.next_write().unwrap().hex, B);
        let w = PendingWrite { hex: B, intent: Intent::Add, seq: 2 };
        l.finished(w, WriteOutcome::Landed);
        let a = l.next_write().unwrap();
        assert_eq!((a.intent, a.seq), (Intent::Remove, last));
        assert_eq!(l.next_write(), None);
    }

    #[test]
    fn a_net_nothing_burst_sends_no_write() {
        let mut l = IntentLedger::new();
        l.press(A, Intent::Add);
        l.press(A, Intent::Remove);
        assert_eq!(l.next_write(), None, "back where the service already is");
        assert_eq!(l.overrides(&A, 0), Some(Intent::Remove), "yet a read issued before still cannot undo it");
    }

    #[test]
    fn writes_go_out_one_at_a_time_in_press_order() {
        let mut l = IntentLedger::new();
        l.press(A, Intent::Add);
        l.press(B, Intent::Add);
        let a = l.next_write().unwrap();
        assert_eq!(l.next_write(), None, "the worker still holds A");
        l.finished(a, WriteOutcome::Landed);
        assert_eq!(l.next_write().unwrap().hex, B);
    }

    #[test]
    fn a_failed_write_reverts_to_the_last_known_membership() {
        let mut l = IntentLedger::new();
        l.press(A, Intent::Add);
        let w = l.next_write().unwrap();
        assert_eq!(l.finished(w, WriteOutcome::Failed), Some(Intent::Remove), "was not a member before");
        assert_eq!(l.overrides(&A, 0), None, "nothing left to override a read with");
        assert!(!l.busy());
    }

    #[test]
    fn a_failure_after_a_landed_write_reverts_to_what_landed() {
        let mut l = IntentLedger::new();
        l.press(A, Intent::Add);
        let add = l.next_write().unwrap();
        l.press(A, Intent::Remove);
        assert_eq!(l.finished(add, WriteOutcome::Landed), Some(Intent::Remove), "the newer press stands");
        let remove = l.next_write().unwrap();
        assert_eq!(l.finished(remove, WriteOutcome::Failed), Some(Intent::Add), "the add did land");
    }

    #[test]
    fn a_newer_press_survives_the_older_writes_failure() {
        let mut l = IntentLedger::new();
        l.press(A, Intent::Add);
        let add = l.next_write().unwrap();
        l.press(A, Intent::Remove);
        l.press(A, Intent::Add);
        assert_eq!(l.finished(add, WriteOutcome::Failed), Some(Intent::Add), "the pending press is the answer");
        assert_eq!(l.next_write().unwrap().intent, Intent::Add);
    }

    #[test]
    fn a_landed_write_overrides_reads_issued_before_its_landing_only() {
        let mut l = IntentLedger::new();
        l.press(A, Intent::Add);
        let w = l.next_write().unwrap();
        let before_landing = l.stamp();
        l.finished(w, WriteOutcome::Landed);
        assert_eq!(l.overrides(&A, before_landing), Some(Intent::Add), "the server may not have had it yet");
        let after = l.stamp();
        assert_eq!(l.overrides(&A, after), None);
        l.forget_older(after);
        assert_eq!(l.overridden(0).count(), 0, "settled and older than the read: forgotten");
    }

    #[test]
    fn forget_older_keeps_entries_with_a_write_outstanding() {
        let mut l = IntentLedger::new();
        l.press(A, Intent::Add);
        l.forget_older(l.stamp());
        assert_eq!(l.overridden(0).count(), 1);
    }

    #[test]
    fn a_requeued_write_goes_out_first_again() {
        let mut l = IntentLedger::new();
        l.press(A, Intent::Add);
        l.press(B, Intent::Add);
        let a = l.next_write().unwrap();
        l.requeue(a);
        assert_eq!(l.next_write(), Some(a));
    }
}
