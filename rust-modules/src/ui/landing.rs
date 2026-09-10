//! `Landing` — a bounded per-addressee QUEUE with backpressure (spec §5.2), the shape every
//! one-slot mailbox (`metadata::DETAIL`, `search::SLOT`, `route::decision::PLAY_SLOT`) becomes in
//! phase 4.
//!
//! Interior mutability because `put` is called from workers and `take_for` from the main thread.
//! Two lanes: DATA (capped; a full queue drops the NEWEST result, which is re-requestable) and
//! CONTROL (never full — at most one record per in-flight request, and `inflight_cap` bounds
//! those), merged on `take_for` by one per-landing sequence number so a `Dropped` can never
//! overtake a data result that arrived before it. The addressee always receives exactly one
//! event per request and nothing strands.
//!
//! ADMISSION (§5.2, phase 4): a store asks `admit(addr)` before it spawns; beyond `inflight_cap`
//! in-flight requests per addressee the answer is `false` and a `Refused` record is already on
//! the control lane, so the addressee hears it on the next frame's step 3 like any other answer.
//! A spawn the OS refused after admission is `refused(addr)`, which releases the slot it held.
//! Per-addressee drop counters are hashed state (`write`). First consumer: `metadata`'s detail
//! landing (`docs/stores-as-machines.md` §2.5).
#![allow(dead_code)] // phase 4: the generic surface is wider than its first consumer uses

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use super::machine::{Addr, Canon, MachineId, RequestId};

/// What `put` answers when the data lane is at its cap.
#[derive(Debug, PartialEq, Eq)]
pub struct Full;

pub enum Lane<K, V> {
    Data(K, V),
    /// The worker-side adapter's answer to `Full`: the request is over, nothing arrived.
    Dropped(RequestId),
    /// A store refused to spawn beyond `inflight_cap` (§5.2 admission control).
    Refused(RequestId),
}

pub struct Landed<K, V> {
    pub seq: u64,
    pub addr: Addr,
    pub lane: Lane<K, V>,
}

struct Inner<K, V> {
    data: VecDeque<Landed<K, V>>,
    control: VecDeque<Landed<K, V>>,
    seq: u64,
    dropped: u32,
    /// Requests admitted and not yet answered, per addressee (§5.2 admission control).
    inflight: BTreeMap<MachineId, u32>,
    /// Results dropped, per addressee — hashed state.
    dropped_for: BTreeMap<MachineId, u32>,
}

/// The default admission bound: how many requests one addressee may have out at once.
pub const INFLIGHT_CAP: u32 = 4;

pub struct Landing<K, V> {
    inner: Mutex<Inner<K, V>>,
    cap: usize,
    inflight_cap: u32,
}

impl<K, V> Landing<K, V> {
    pub const fn new(cap: usize) -> Self {
        Self::with_inflight(cap, INFLIGHT_CAP)
    }

    pub const fn with_inflight(cap: usize, inflight_cap: u32) -> Self {
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
        }
    }

    /// Main-thread side, BEFORE a spawn: may `addr` go out? Beyond `inflight_cap` for its
    /// addressee the answer is `false` and a `Refused` is already on the control lane (§5.2:
    /// "a store refuses to spawn beyond `inflight_cap` per addressee and answers `Refused`
    /// immediately"), so the addressee still hears exactly one event for the request.
    pub fn admit(&self, addr: Addr) -> bool {
        let mut g = self.lock();
        let n = g.inflight.entry(addr.to).or_insert(0);
        if *n >= self.inflight_cap {
            drop(g);
            self.control(addr, Lane::Refused(addr.req));
            return false;
        }
        *n += 1;
        true
    }

    /// Requests out for `to`.
    pub fn inflight(&self, to: MachineId) -> u32 {
        self.lock().inflight.get(&to).copied().unwrap_or(0)
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
            c.str(&format!("{to:?}")).u32(*n);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner<K, V>> {
        // A poisoned landing is a worker that panicked mid-put; the records it did write are
        // intact, and losing the whole mailbox to the poison flag would strand every request.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Worker side. `Err(Full)` means the NEWEST result was dropped; the caller answers with
    /// `dropped(addr)` so the addressee still hears exactly once.
    pub fn put(&self, addr: Addr, key: K, value: V) -> Result<(), Full> {
        let mut g = self.lock();
        if g.data.len() >= self.cap {
            g.dropped = g.dropped.wrapping_add(1);
            *g.dropped_for.entry(addr.to).or_insert(0) += 1;
            return Err(Full);
        }
        g.seq += 1;
        let seq = g.seq;
        g.data.push_back(Landed {
            seq,
            addr,
            lane: Lane::Data(key, value),
        });
        Ok(())
    }

    /// Worker side, the control lane: cannot be full.
    pub fn dropped(&self, addr: Addr) {
        self.control(addr, Lane::Dropped(addr.req));
    }

    /// Main-thread side, the control lane: a spawn the OS refused AFTER admission — the slot it
    /// held is released here, and the addressee hears `Refused` (§5.2).
    pub fn refused(&self, addr: Addr) {
        self.release(addr.to);
        self.control(addr, Lane::Refused(addr.req));
    }

    fn release(&self, to: MachineId) {
        let mut g = self.lock();
        if let Some(n) = g.inflight.get_mut(&to) {
            *n = n.saturating_sub(1);
        }
    }

    fn control(&self, addr: Addr, lane: Lane<K, V>) {
        let mut g = self.lock();
        g.seq += 1;
        let seq = g.seq;
        g.control.push_back(Landed { seq, addr, lane });
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
            // a Data or Dropped record ANSWERS an admitted request: its slot is released whether
            // or not the record is deliverable (a Refused never held one)
            if !matches!(rec.lane, Lane::Refused(_)) {
                if let Some(n) = g.inflight.get_mut(&rec.addr.to) {
                    *n = n.saturating_sub(1);
                }
            }
            if !is_deliverable(&rec.addr) {
                g.dropped = g.dropped.wrapping_add(1);
                *g.dropped_for.entry(rec.addr.to).or_insert(0) += 1;
                continue;
            }
            let wanted = match &rec.lane {
                Lane::Data(k, _) => want(k),
                Lane::Dropped(_) | Lane::Refused(_) => true,
            };
            if wanted {
                out.push(rec);
            } else {
                g.dropped = g.dropped.wrapping_add(1);
                *g.dropped_for.entry(rec.addr.to).or_insert(0) += 1;
            }
        }
    }

    /// Drop every queued record and release every slot — a supersede that starts over (the
    /// detail page closing). Counted as drops, since each was a request somebody made.
    pub fn clear(&self) {
        let mut g = self.lock();
        let n = (g.data.len() + g.control.len()) as u32;
        g.dropped = g.dropped.wrapping_add(n);
        g.data.clear();
        g.control.clear();
        g.inflight.clear();
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
        assert_eq!(l.put(addr(1), 1, "first"), Ok(()));
        assert_eq!(l.put(addr(2), 2, "second"), Err(Full));
        l.dropped(addr(2));
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
        l.put(addr(1), 1, 10).unwrap();
        let mut out = Vec::new();
        l.take_for(&|_| false, &|_| true, &mut out);
        assert!(out.is_empty());
        assert_eq!(l.dropped_count(), 1);
    }

    fn to() -> MachineId {
        MachineId::Instance(InstanceId(1))
    }

    /// Spec §15.1 / §5.2: the NEWEST result is what a full data lane drops (page 1 of a listing
    /// is not reconstructible), and the count is hashed state.
    #[test]
    fn the_landing_cap_drops_the_newest_and_hashes_the_count() {
        let l: Landing<u32, &str> = Landing::new(2);
        let mut before = Canon::new();
        l.write(&mut before);
        assert_eq!(l.put(addr(1), 1, "one"), Ok(()));
        assert_eq!(l.put(addr(2), 2, "two"), Ok(()));
        assert_eq!(l.put(addr(3), 3, "three"), Err(Full), "the newest is refused");
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
        assert!(l.admit(addr(1)) && l.admit(addr(2)));
        assert_eq!(l.inflight(to()), 2);
        assert_eq!(l.put(addr(1), 1, "first"), Ok(()));
        assert_eq!(l.put(addr(2), 2, "second"), Err(Full));
        l.dropped(addr(2)); // the worker-side adapter's answer to Full
        let mut out = Vec::new();
        l.take_for(&|_| true, &|_| true, &mut out);
        assert_eq!(out.len(), 2, "one event per request");
        assert!(matches!(out[1].lane, Lane::Dropped(RequestId(2))));
        assert_eq!(l.inflight(to()), 0, "both slots retired");
    }

    /// Spec §15.1: admission beyond the cap lands a `Refused` at once and takes no slot; a spawn
    /// the OS refused after admission releases the slot it held and lands one too.
    #[test]
    fn a_refused_spawn_lands_a_refusal_event() {
        let l: Landing<u32, &str> = Landing::with_inflight(4, 1);
        assert!(l.admit(addr(1)));
        assert!(!l.admit(addr(2)), "beyond inflight_cap");
        assert_eq!(l.inflight(to()), 1, "a refusal holds no slot");
        l.refused(addr(1)); // spawn_small returned false for the admitted one
        assert_eq!(l.inflight(to()), 0);
        let mut out = Vec::new();
        l.take_for(&|_| true, &|_| true, &mut out);
        assert!(matches!(out[0].lane, Lane::Refused(RequestId(2))));
        assert!(matches!(out[1].lane, Lane::Refused(RequestId(1))));
        assert!(l.admit(addr(3)), "the slot is free again");
    }

    /// Spec §15.1 / §5.2: the key carries identity — `(server, ratingKey)` — so a landing for
    /// another server's item of the same number is skipped and counted, never delivered.
    #[test]
    fn a_same_rating_key_on_a_different_server_is_skipped() {
        let l: Landing<(u32, String), &str> = Landing::new(4);
        l.put(addr(1), (1, "7".into()), "ours").unwrap();
        l.put(addr(2), (2, "7".into()), "theirs").unwrap();
        let mut out = Vec::new();
        l.take_for(&|_| true, &|k| *k == (1, "7".to_string()), &mut out);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0].lane, Lane::Data(_, "ours")));
        assert_eq!(l.dropped_for(to()), 1);
    }
}
