//! Bounded addressed one-shot results and explicit progress streams (§5.2; see
//! docs/stores-as-machines.md §2 for the one-terminal-per-stream clarification).
//! Admission reserves one completion slot, bounded globally and per addressee. Capacity rejection
//! is synchronous; the requester settles it without spawning or queueing an unbounded refusal.
//! OS spawn refusal after admission is a reserved terminal, just like Data or Dropped.
//! One-shot data Full or stream progress overflow atomically queues Dropped. A stream's terminal
//! payload uses its reserved slot even when progress data is full. Progress never retires a slot.
//! Both lanes merge by arrival sequence. Reservations last until terminal consumption/discard.
//! Cancellation retains running reservations until the worker acknowledges completion; it does
//! not stop workers. Each admitted Addr must be unique for its operation (no completion tombstones).
//! Record/reservation bounds do not bound arbitrary V bytes: adapters must bound payload sizes.
//! Only drop counters are canonical here; complete queued-payload replay hashing remains work.
#![allow(dead_code)] // phase 4: the generic surface is wider than its first consumer uses

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use super::machine::{Addr, Canon, MachineId, RequestId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionError {
    Duplicate,
    Capacity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishError {
    /// The data was dropped and one Dropped terminal is already queued.
    Full,
    Unknown,
    AlreadyTerminal,
    NotStream,
    Cancelled,
}

#[derive(Clone, Copy)]
enum Reservation {
    Running { cancelled: bool, stream: bool },
    Terminal { cancelled: bool },
}

pub enum Lane<K, V> {
    Data(K, V),
    /// The request is over, nothing arrived (also queued automatically on Full).
    Dropped(RequestId),
    /// The OS refused a spawn AFTER admission; capacity refusals are synchronous.
    Refused(RequestId),
}

pub struct Landed<K, V> {
    pub seq: u64,
    pub addr: Addr,
    pub lane: Lane<K, V>,
    /// False only for stream progress. Every one-shot result and stream completion is terminal.
    pub terminal: bool,
}

/// Construct inside the running worker, borrowing its Arc-owned Landing. Early return/unwind
/// queues Dropped unless an explicit terminal already exists. The launcher, not this guard,
/// answers an OS refusal when the closure never starts. Addr identifies one unique operation.
pub struct CompletionGuard<'a, K, V> {
    landing: &'a Landing<K, V>,
    addr: Addr,
}

impl<K, V> Drop for CompletionGuard<'_, K, V> {
    fn drop(&mut self) { let _ = self.landing.dropped(self.addr); }
}

struct Inner<K, V> {
    data: VecDeque<Landed<K, V>>,
    control: VecDeque<Landed<K, V>>,
    seq: u64,
    dropped: u32,
    /// Running (including cancelled) and queued-terminal reservations, by exact address.
    inflight: BTreeMap<(MachineId, u32), Reservation>,
    /// Results dropped, per addressee — hashed state.
    dropped_for: BTreeMap<MachineId, u32>,
}

/// The default admission bound: how many requests one addressee may have out at once.
pub const INFLIGHT_CAP: u32 = 4;

pub struct Landing<K, V> {
    inner: Mutex<Inner<K, V>>,
    cap: usize,
    inflight_cap: u32,
    total_cap: u32,
}

impl<K, V> Landing<K, V> {
    pub const fn new(cap: usize) -> Self {
        Self::with_inflight(cap, INFLIGHT_CAP)
    }

    pub const fn with_inflight(cap: usize, inflight_cap: u32) -> Self {
        Self::with_limits(cap, inflight_cap, inflight_cap)
    }

    pub const fn with_limits(cap: usize, inflight_cap: u32, total_cap: u32) -> Self {
        Self {
            inner: Mutex::new(Inner {
                data: VecDeque::new(),
                control: VecDeque::new(),
                seq: 0,
                dropped: 0,
                inflight: BTreeMap::new(),
                dropped_for: BTreeMap::new(),
            }),
            cap,
            inflight_cap,
            total_cap,
        }
    }

    /// BEFORE a spawn. Rejection queues nothing: the requester handles this exact outcome.
    pub fn admit(&self, addr: Addr) -> Result<(), AdmissionError> {
        self.admit_mode(addr, false)
    }

    /// One reservation for ordered progress followed by exactly one terminal outcome.
    pub fn admit_stream(&self, addr: Addr) -> Result<(), AdmissionError> {
        self.admit_mode(addr, true)
    }

    fn admit_mode(&self, addr: Addr, stream: bool) -> Result<(), AdmissionError> {
        let mut g = self.lock();
        let key = (addr.to, addr.req.0);
        if g.inflight.contains_key(&key) {
            return Err(AdmissionError::Duplicate);
        }
        if g.inflight.len() >= self.total_cap as usize
            || g.inflight.keys().filter(|(to, _)| *to == addr.to).count() >= self.inflight_cap as usize {
            return Err(AdmissionError::Capacity);
        }
        g.inflight.insert(key, Reservation::Running { cancelled: false, stream });
        Ok(())
    }

    /// Requests out for `to`.
    pub fn inflight(&self, to: MachineId) -> u32 {
        self.lock().inflight.keys().filter(|(target, _)| *target == to).count() as u32
    }

    /// Results dropped for `to` (the newest at a full data lane, and records for a dead or
    /// unwanted key).
    pub fn dropped_for(&self, to: MachineId) -> u32 {
        self.lock().dropped_for.get(&to).copied().unwrap_or(0)
    }

    /// The hashed half (§5.2): the drop counters, per addressee, in a fixed order.
    pub fn write(&self, c: &mut Canon) {
        let g = self.lock();
        c.u32(g.dropped);
        c.u32(g.dropped_for.len() as u32);
        for (to, n) in &g.dropped_for {
            to.write_canon(c);
            c.u32(*n);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner<K, V>> {
        // A poisoned landing is a worker that panicked mid-put; the records it did write are
        // intact, and losing the whole mailbox to the poison flag would strand every request.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Worker completion. Full queues Dropped atomically. Unknown/duplicate sends queue nothing.
    pub fn put(&self, addr: Addr, key: K, value: V) -> Result<(), PublishError> {
        self.complete(addr, Lane::Data(key, value))
    }

    /// Nonterminal data for a live stream. Full closes it with one ordered Dropped terminal;
    /// the producer must stop. Later success cannot conceal a partially lost observation flow.
    pub fn progress(&self, addr: Addr, key: K, value: V) -> Result<(), PublishError> {
        let mut g = self.lock();
        match g.inflight.get(&(addr.to, addr.req.0)) {
            None => return Err(PublishError::Unknown),
            Some(Reservation::Terminal { .. }) => return Err(PublishError::AlreadyTerminal),
            Some(Reservation::Running { stream: false, .. }) => return Err(PublishError::NotStream),
            Some(Reservation::Running { cancelled: true, .. }) => return Err(PublishError::Cancelled),
            Some(Reservation::Running { .. }) => {}
        }
        if g.data.len() >= self.cap {
            Self::count_drop(&mut g, addr.to);
            self.complete_locked(&mut g, addr, Lane::Dropped(addr.req))?;
            return Err(PublishError::Full);
        }
        g.seq += 1;
        let seq = g.seq;
        g.data.push_back(Landed { seq, addr, lane: Lane::Data(key, value), terminal: false });
        Ok(())
    }

    pub fn completion_guard(&self, addr: Addr) -> Result<CompletionGuard<'_, K, V>, PublishError> {
        match self.lock().inflight.get(&(addr.to, addr.req.0)) {
            None => Err(PublishError::Unknown),
            Some(Reservation::Terminal { .. }) => Err(PublishError::AlreadyTerminal),
            Some(Reservation::Running { .. }) => Ok(CompletionGuard { landing: self, addr }),
        }
    }

    /// Worker abandonment after admission, using its reserved terminal slot.
    pub fn dropped(&self, addr: Addr) -> Result<(), PublishError> {
        self.complete(addr, Lane::Dropped(addr.req))
    }

    /// OS spawn refusal AFTER admission. Reservation remains until terminal consumption/discard.
    pub fn refused(&self, addr: Addr) -> Result<(), PublishError> {
        self.complete(addr, Lane::Refused(addr.req))
    }

    fn complete(&self, addr: Addr, lane: Lane<K, V>) -> Result<(), PublishError> {
        let mut g = self.lock();
        self.complete_locked(&mut g, addr, lane)
    }

    fn complete_locked(&self, g: &mut Inner<K, V>, addr: Addr, mut lane: Lane<K, V>) -> Result<(), PublishError> {
        let key = (addr.to, addr.req.0);
        let (cancelled, stream) = match g.inflight.get(&key) {
            None => return Err(PublishError::Unknown),
            Some(Reservation::Terminal { .. }) => return Err(PublishError::AlreadyTerminal),
            Some(Reservation::Running { cancelled, stream }) => (*cancelled, *stream),
        };
        let full = !cancelled && !stream && matches!(lane, Lane::Data(..)) && g.data.len() >= self.cap;
        if full {
            Self::count_drop(g, addr.to);
        }
        if full || cancelled {
            // Cancelled completions still arrive in sequence and retain their reserved slot.
            // Discard the payload here, but count/retire only on the main-thread drain.
            lane = Lane::Dropped(addr.req);
        }
        g.inflight.insert(key, Reservation::Terminal { cancelled });
        g.seq += 1;
        let seq = g.seq;
        let record = Landed { seq, addr, lane, terminal: true };
        if !stream && matches!(record.lane, Lane::Data(..)) { g.data.push_back(record); }
        else { g.control.push_back(record); }
        if full { Err(PublishError::Full) } else { Ok(()) }
    }

    fn count_drop(g: &mut Inner<K, V>, to: MachineId) {
        g.dropped = g.dropped.wrapping_add(1);
        let n = g.dropped_for.entry(to).or_insert(0);
        *n = n.wrapping_add(1);
    }

    /// Main thread. Moves every deliverable record whose key is wanted into `out`, both lanes
    /// merged by sequence. `Navigation` is the sole owner of the live index behind
    /// `is_deliverable`; a record for a dead addressee is dropped and counted.
    pub fn take_for(
        &self,
        is_deliverable: &dyn Fn(&Addr) -> bool,
        want: &dyn Fn(&K) -> bool,
        out: &mut Vec<Landed<K, V>>,
    ) {
        let mut g = self.lock();
        let mut merged: Vec<Landed<K, V>> = Vec::with_capacity(g.data.len() + g.control.len());
        while let Some(rec) = g.data.pop_front() {
            merged.push(rec);
        }
        while let Some(rec) = g.control.pop_front() {
            merged.push(rec);
        }
        merged.sort_by_key(|r| r.seq);
        for rec in merged {
            let key = (rec.addr.to, rec.addr.req.0);
            let cancelled = matches!(g.inflight.get(&key),
                Some(Reservation::Terminal { cancelled: true } | Reservation::Running { cancelled: true, .. }));
            if rec.terminal { g.inflight.remove(&key); }
            if cancelled || !is_deliverable(&rec.addr) {
                Self::count_drop(&mut g, rec.addr.to);
                continue;
            }
            let wanted = match &rec.lane {
                Lane::Data(k, _) => want(k),
                Lane::Dropped(_) | Lane::Refused(_) => true,
            };
            if wanted {
                out.push(rec);
            } else {
                Self::count_drop(&mut g, rec.addr.to);
            }
        }
    }

    /// Discard queued terminals and cancel running requests. Running reservations remain until
    /// the worker's completion acknowledgement: cancellation is not worker termination.
    pub fn clear(&self) {
        let mut g = self.lock();
        let addresses: Vec<_> = g.inflight.keys().map(|(to, req)| Addr { to: *to, req: RequestId(*req) }).collect();
        for addr in addresses { Self::cancel_locked(&mut g, addr); }
    }

    /// Main-thread cancellation of only this operation. Progress/queued terminal discard is
    /// counted here; a running worker retains its reservation until its terminal is consumed.
    pub fn cancel(&self, addr: Addr) -> bool {
        Self::cancel_locked(&mut self.lock(), addr)
    }

    fn cancel_locked(g: &mut Inner<K, V>, addr: Addr) -> bool {
        let key = (addr.to, addr.req.0);
        let Some(state) = g.inflight.get(&key).copied() else { return false };
        let mut discarded = 0;
        let mut keep = |rec: &Landed<K, V>| {
            if rec.addr == addr { discarded += 1; false } else { true }
        };
        g.data.retain(&mut keep);
        g.control.retain(&mut keep);
        for _ in 0..discarded { Self::count_drop(g, addr.to); }
        match state {
            Reservation::Terminal { .. } => { g.inflight.remove(&key); }
            Reservation::Running { stream, .. } => {
                g.inflight.insert(key, Reservation::Running { cancelled: true, stream });
            }
        }
        true
    }

    /// Per-landing drop counter — hashed state (§5.2).
    pub fn dropped_count(&self) -> u32 {
        self.lock().dropped
    }

    pub fn len(&self) -> usize {
        let g = self.lock();
        g.data.len() + g.control.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
#[path = "landing/stream_tests.rs"]
mod stream_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::machine::{InstanceId, MachineId};

    fn addr(req: u32) -> Addr {
        Addr {
            to: MachineId::Instance(InstanceId(1)),
            req: RequestId(req),
        }
    }

    #[test]
    fn a_dropped_record_never_overtakes_a_result_that_arrived_before_it() {
        let l: Landing<u32, &str> = Landing::new(1);
        l.admit(addr(1)).unwrap();
        l.admit(addr(2)).unwrap();
        assert_eq!(l.put(addr(1), 1, "first"), Ok(()));
        assert_eq!(l.put(addr(2), 2, "second"), Err(PublishError::Full));
        let mut out = Vec::new();
        l.take_for(&|_| true, &|_| true, &mut out);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0].lane, Lane::Data(1, "first")));
        assert!(matches!(out[1].lane, Lane::Dropped(RequestId(2))));
        assert_eq!(l.dropped_count(), 1);
        assert!(l.is_empty());
    }

    #[test]
    fn a_dead_addressee_is_dropped_and_counted() {
        let l: Landing<u32, u32> = Landing::new(4);
        l.admit(addr(1)).unwrap();
        l.put(addr(1), 1, 10).unwrap();
        let mut out = Vec::new();
        l.take_for(&|_| false, &|_| true, &mut out);
        assert!(out.is_empty());
        assert_eq!(l.dropped_count(), 1);
    }

    fn to() -> MachineId {
        MachineId::Instance(InstanceId(1))
    }

    #[test]
    fn cancelled_a_landing_cannot_retire_b() {
        let l: Landing<u32, u32> = Landing::with_inflight(4, 4);
        l.admit(addr(1)).unwrap();
        l.clear();
        l.admit(addr(2)).unwrap();
        let _ = l.put(addr(1), 1, 10);
        let mut out = Vec::new();
        l.take_for(&|_| true, &|_| true, &mut out);
        assert_eq!(l.inflight(to()), 1, "late A must not retire B");
        assert!(out.is_empty(), "cancelled A must not be delivered");
    }

    #[test]
    fn duplicate_a_completion_cannot_retire_b() {
        let l: Landing<u32, u32> = Landing::with_inflight(4, 4);
        l.admit(addr(1)).unwrap();
        l.admit(addr(2)).unwrap();
        l.put(addr(1), 1, 10).unwrap();
        let _ = l.put(addr(1), 1, 10);
        let mut out = Vec::new();
        l.take_for(&|_| true, &|_| true, &mut out);
        assert_eq!(l.inflight(to()), 1, "duplicate A must not retire B");
        assert_eq!(out.len(), 1, "one terminal answer for A");
    }

    #[test]
    fn duplicate_admission_does_not_consume_another_slot() {
        let l: Landing<u32, u32> = Landing::with_inflight(4, 4);
        l.admit(addr(1)).unwrap();
        let _ = l.admit(addr(1));
        assert_eq!(l.inflight(to()), 1, "admission is keyed by exact address");
    }

    /// Spec §15.1 / §5.2: the NEWEST result is what a full data lane drops (page 1 of a listing
    /// is not reconstructible), and the count is hashed state.
    #[test]
    fn the_landing_cap_drops_the_newest_and_hashes_the_count() {
        let l: Landing<u32, &str> = Landing::new(2);
        for n in 1..=3 { l.admit(addr(n)).unwrap(); }
        let mut before = Canon::new();
        l.write(&mut before);
        assert_eq!(l.put(addr(1), 1, "one"), Ok(()));
        assert_eq!(l.put(addr(2), 2, "two"), Ok(()));
        assert_eq!(l.put(addr(3), 3, "three"), Err(PublishError::Full), "the newest is refused");
        let mut out = Vec::new();
        l.take_for(&|_| true, &|_| true, &mut out);
        assert!(matches!(out[0].lane, Lane::Data(1, "one")) && matches!(out[1].lane, Lane::Data(2, "two")));
        assert_eq!(l.dropped_count(), 1);
        assert_eq!(l.dropped_for(to()), 1);
        let mut after = Canon::new();
        l.write(&mut after);
        assert_ne!(before.finish(), after.finish(), "the drop count is in the hash");
    }

    /// Spec §15.1: a full landing answers `Dropped`, the addressee hears exactly one event per
    /// request, and the in-flight slot is retired by the answer.
    #[test]
    fn a_full_landing_replies_dropped_and_retires_inflight() {
        let l: Landing<u32, &str> = Landing::with_inflight(1, 4);
        l.admit(addr(1)).unwrap();
        l.admit(addr(2)).unwrap();
        assert_eq!(l.inflight(to()), 2);
        assert_eq!(l.put(addr(1), 1, "first"), Ok(()));
        assert_eq!(l.put(addr(2), 2, "second"), Err(PublishError::Full));
        assert_eq!(l.dropped(addr(2)), Err(PublishError::AlreadyTerminal));
        let mut out = Vec::new();
        l.take_for(&|_| true, &|_| true, &mut out);
        assert_eq!(out.len(), 2, "one event per request");
        assert!(matches!(out[1].lane, Lane::Dropped(RequestId(2))));
        assert_eq!(l.inflight(to()), 0, "both slots retired");
    }

    /// Capacity refusal is synchronous; OS refusal after admission holds its reserved terminal
    /// until the consumer drains it.
    #[test]
    fn a_refused_spawn_lands_a_refusal_event() {
        let l: Landing<u32, &str> = Landing::with_inflight(4, 1);
        l.admit(addr(1)).unwrap();
        assert_eq!(l.admit(addr(2)), Err(AdmissionError::Capacity));
        assert_eq!(l.inflight(to()), 1, "a refusal holds no slot");
        l.refused(addr(1)).unwrap(); // spawn_small returned false for the admitted one
        assert_eq!(l.inflight(to()), 1, "undrained terminal retains reservation");
        let mut out = Vec::new();
        l.take_for(&|_| true, &|_| true, &mut out);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0].lane, Lane::Refused(RequestId(1))));
        assert_eq!(l.admit(addr(3)), Ok(()), "the slot is free again");
    }

    /// Spec §15.1 / §5.2: the key carries identity — `(server, ratingKey)` — so a landing for
    /// another server's item of the same number is skipped and counted, never delivered.
    #[test]
    fn a_same_rating_key_on_a_different_server_is_skipped() {
        let l: Landing<(u32, String), &str> = Landing::new(4);
        l.admit(addr(1)).unwrap();
        l.admit(addr(2)).unwrap();
        l.put(addr(1), (1, "7".into()), "ours").unwrap();
        l.put(addr(2), (2, "7".into()), "theirs").unwrap();
        let mut out = Vec::new();
        l.take_for(&|_| true, &|k| *k == (1, "7".to_string()), &mut out);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0].lane, Lane::Data(_, "ours")));
        assert_eq!(l.dropped_for(to()), 1);
    }

    #[test]
    fn unknown_and_duplicate_terminals_do_not_change_other_reservations() {
        let l: Landing<u32, u32> = Landing::new(2);
        l.admit(addr(1)).unwrap();
        l.admit(addr(2)).unwrap();
        assert_eq!(l.admit(addr(1)), Err(AdmissionError::Duplicate));
        assert_eq!(l.put(addr(9), 9, 9), Err(PublishError::Unknown));
        assert_eq!(l.dropped(addr(9)), Err(PublishError::Unknown));
        assert_eq!(l.refused(addr(9)), Err(PublishError::Unknown));
        l.refused(addr(1)).unwrap();
        assert_eq!(l.refused(addr(1)), Err(PublishError::AlreadyTerminal));
        assert_eq!(l.dropped(addr(1)), Err(PublishError::AlreadyTerminal));
        assert_eq!(l.put(addr(1), 1, 1), Err(PublishError::AlreadyTerminal));
        assert_eq!(l.inflight(to()), 2);
        assert_eq!(l.len(), 1);
        let mut out = Vec::new();
        l.take_for(&|_| true, &|_| true, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(l.inflight(to()), 1);
        assert_eq!(l.refused(addr(1)), Err(PublishError::Unknown));
        assert_eq!(l.inflight(to()), 1);
    }

    #[test]
    fn undrained_terminals_and_cancelled_workers_hold_bounded_reservations() {
        let l: Landing<u32, u32> = Landing::with_limits(1, 3, 3);
        for req in 1..=3 { l.admit(addr(req)).unwrap(); }
        l.put(addr(1), 1, 1).unwrap();
        assert_eq!(l.put(addr(2), 2, 2), Err(PublishError::Full));
        l.refused(addr(3)).unwrap();
        assert_eq!(l.inflight(to()), 3);
        for req in 4..1004 {
            assert_eq!(l.admit(addr(req)), Err(AdmissionError::Capacity));
            assert_eq!(l.admit(addr(4)), Err(AdmissionError::Capacity));
        }
        assert_eq!(l.len(), 3, "capacity rejection queues nothing");
        let mut out = Vec::new();
        l.take_for(&|_| true, &|_| true, &mut out);
        assert!(out.windows(2).all(|r| r[0].seq < r[1].seq));
        assert!(matches!(out[0].lane, Lane::Data(..)));
        assert!(matches!(out[1].lane, Lane::Dropped(_)));
        assert!(matches!(out[2].lane, Lane::Refused(_)));
        for req in 4..=6 { l.admit(addr(req)).unwrap(); }
        l.clear(); l.clear();
        assert_eq!(l.inflight(to()), 3, "cancel is not worker termination");
        assert_eq!(l.admit(addr(4)), Err(AdmissionError::Duplicate));
        assert_eq!(l.admit(addr(7)), Err(AdmissionError::Capacity));
        l.put(addr(4), 4, 4).unwrap();
        l.dropped(addr(5)).unwrap();
        l.refused(addr(6)).unwrap();
        assert_eq!(l.len(), 3, "cancelled acknowledgements await the ordered drain");
        assert_eq!(l.inflight(to()), 3);
        assert_eq!(l.admit(addr(7)), Err(AdmissionError::Capacity));
        let mut cancelled = Vec::new();
        l.take_for(&|_| true, &|_| true, &mut cancelled);
        assert!(cancelled.is_empty());
        assert_eq!(l.inflight(to()), 0);
        assert_eq!(l.put(addr(4), 4, 4), Err(PublishError::Unknown));
        l.admit(addr(7)).unwrap();
    }

    #[test]
    fn cancelled_completion_is_sequenced_and_counted_only_at_drain() {
        let l: Landing<u32, u32> = Landing::with_limits(2, 2, 2);
        l.admit(addr(1)).unwrap();
        l.clear();
        l.admit(addr(2)).unwrap();
        l.put(addr(1), 1, 1).unwrap();
        assert_eq!(l.inflight(to()), 2, "undrained A still reserves its completion");
        assert_eq!(l.admit(addr(3)), Err(AdmissionError::Capacity));
        assert_eq!(l.dropped_for(to()), 0, "worker cannot count the cancelled discard");
        let seq_a = l.lock().control.front().unwrap().seq;
        l.put(addr(2), 2, 2).unwrap();
        let seq_b = l.lock().data.front().unwrap().seq;
        assert!(seq_a < seq_b, "cancelled ack shares the data arrival sequence");
        let mut out = Vec::new();
        l.take_for(&|_| true, &|_| true, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].addr, addr(2));
        assert_eq!(l.dropped_for(to()), 1);
        assert_eq!(l.inflight(to()), 0);
        assert_eq!(l.dropped(addr(1)), Err(PublishError::Unknown));
        l.clear();
        assert_eq!(l.dropped_for(to()), 1, "one cancelled terminal discard");
    }

    #[test]
    fn total_and_per_target_admission_caps_are_independent() {
        let l: Landing<(), ()> = Landing::with_limits(4, 1, 2);
        let session = Addr { to: MachineId::Session, req: RequestId(1) };
        let cache = Addr { to: MachineId::Cache, req: RequestId(1) };
        l.admit(addr(1)).unwrap();
        assert_eq!(l.admit(addr(2)), Err(AdmissionError::Capacity));
        l.admit(session).unwrap();
        assert_eq!(l.admit(cache), Err(AdmissionError::Capacity));
        l.dropped(session).unwrap();
        assert_eq!(l.admit(cache), Err(AdmissionError::Capacity), "terminal still occupies slot");
        l.take_for(&|_| true, &|_| true, &mut Vec::new());
        l.admit(cache).unwrap();
        assert_eq!(l.inflight(to()), 1);
    }

    #[test]
    fn clear_and_filter_discard_count_canonically_by_addressee() {
        let l: Landing<(), ()> = Landing::with_limits(4, 4, 4);
        let session = Addr { to: MachineId::Session, req: RequestId(1) };
        l.admit(addr(1)).unwrap(); l.admit(session).unwrap();
        l.put(addr(1), (), ()).unwrap(); l.refused(session).unwrap();
        l.clear();
        assert_eq!(l.inflight(to()), 0);
        l.admit(addr(2)).unwrap(); l.put(addr(2), (), ()).unwrap();
        l.take_for(&|_| true, &|_| false, &mut Vec::new());
        assert_eq!(l.dropped_for(to()), 2);
        assert_eq!(l.dropped_for(MachineId::Session), 1);
        let mut expected = Canon::new();
        expected.u32(3).u32(2);
        MachineId::Session.write_canon(&mut expected); expected.u32(1);
        to().write_canon(&mut expected); expected.u32(2);
        let mut actual = Canon::new(); l.write(&mut actual);
        assert_eq!(actual.finish(), expected.finish());
    }

    #[test]
    fn concurrent_publications_claim_only_one_terminal() {
        let l: Landing<u32, u32> = Landing::new(1);
        l.admit(addr(1)).unwrap();
        std::thread::scope(|s| {
            let a = s.spawn(|| l.put(addr(1), 1, 1));
            let b = s.spawn(|| l.refused(addr(1)));
            let results = [a.join().unwrap(), b.join().unwrap()];
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
            assert_eq!(results.iter().filter(|r| **r == Err(PublishError::AlreadyTerminal)).count(), 1);
        });
        assert_eq!(l.inflight(to()), 1);
        assert_eq!(l.len(), 1);
    }
}
