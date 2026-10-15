//! The watchlist intent ledger (plan §1.4): optimistic flip, same-id presses coalesce with the
//! newest intent winning, one ordered write worker, a failed write reverts, no toast. An intent
//! recorded here beats any list read started before the write's 200.
//!
//! CONTRACT FREEZE: names and signatures only; W2b owns the bodies.

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

#[derive(Debug, Default)]
pub struct IntentLedger {
    _private: (),
}

impl IntentLedger {
    pub fn new() -> IntentLedger {
        IntentLedger::default()
    }

    /// A press: flip optimistically and queue the write. A second press on the same id replaces
    /// the queued intent (newest wins). Returns the sequence number.
    pub fn press(&mut self, hex: [u8; 12], intent: Intent) -> u64 {
        let _ = (hex, intent);
        unimplemented!("W2b")
    }

    /// The next write for the single ordered worker, oldest first, or `None`.
    pub fn next_write(&mut self) -> Option<PendingWrite> {
        unimplemented!("W2b")
    }

    /// The worker's verdict on a write. `Failed` reverts the id; returns the membership to show.
    pub fn finished(&mut self, write: PendingWrite, outcome: WriteOutcome) -> Option<Intent> {
        let _ = (write, outcome);
        unimplemented!("W2b")
    }

    /// The membership this id must show despite a list read stamped `read_seq`, if an intent
    /// newer than the read exists; `None` leaves the read's answer standing.
    pub fn overrides(&self, hex: &[u8; 12], read_seq: u64) -> Option<Intent> {
        let _ = (hex, read_seq);
        unimplemented!("W2b")
    }

    /// The stamp a list read takes when it is issued.
    pub fn stamp(&self) -> u64 {
        unimplemented!("W2b")
    }
}
