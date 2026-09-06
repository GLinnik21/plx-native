//! `Landing` — a bounded per-addressee QUEUE with backpressure (spec §5.2), the shape every
//! one-slot mailbox (`metadata::DETAIL`, `search::SLOT`, `route::PLAY_SLOT`) becomes in phase 4.
//!
//! Interior mutability because `put` is called from workers and `take_for` from the main thread.
//! Two lanes: DATA (capped; a full queue drops the NEWEST result, which is re-requestable) and
//! CONTROL (never full — at most one record per in-flight request, and `inflight_cap` bounds
//! those), merged on `take_for` by one per-landing sequence number so a `Dropped` can never
//! overtake a data result that arrived before it. The addressee always receives exactly one
//! event per request and nothing strands.
#![allow(dead_code)] // phase 2-i: no consumer until phase 4 (spec §13)

use std::collections::VecDeque;
use std::sync::Mutex;

use super::machine::{Addr, RequestId};

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
}

pub struct Landing<K, V> {
    inner: Mutex<Inner<K, V>>,
    cap: usize,
}

impl<K, V> Landing<K, V> {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                data: VecDeque::new(),
                control: VecDeque::new(),
                seq: 0,
                dropped: 0,
            }),
            cap,
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

    /// Main-thread side, the control lane: the admission refusal (§5.2).
    pub fn refused(&self, addr: Addr) {
        self.control(addr, Lane::Refused(addr.req));
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
            if !is_deliverable(&rec.addr) {
                g.dropped = g.dropped.wrapping_add(1);
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
            }
        }
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
}
