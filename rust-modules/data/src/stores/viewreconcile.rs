//! viewreconcile — the **read-back** of a view-state write: after Mark as Watched / Unwatched has
//! been answered (taken OR refused), ask the item's server what it now says and patch the stores
//! with the answer. Its own store (`StoreId::ViewReconcile`), appended after every earlier ordinal.
//!
//! ## Why a store of its own
//!
//! ViewState's queue is gated on `sent.is_some()` and drains one write at a time; a read-back
//! sharing that gate would wait behind the next write of a burst, and a write sharing the
//! read-back's would wait behind a slow read. They are different work with different ordering
//! rules, so they have different owners and different landing sites (`landgate` keys each by its
//! store ordinal).
//!
//! ## What it does
//!
//! * **Triggered by a landed write, whatever the outcome, and by a stopped playback the server
//!   took** (`enqueue_stop`, from `Stores::stops_landed`; it involves no write). `ViewStateState` hands over the
//!   `(server, ratingKey)` of the item and of every copy the fan-out took
//!   ([`crate::viewstate::ViewStateState::take_reread`]) once its burst is idle. No read is spawned while the player is on top
//!   (`Stores::hold_network`) either: the items stay owed. On success the
//!   answer confirms the optimistic edit; on refusal it is the only thing that returns the tile to
//!   what the server holds, because the ledger entry of a refused write is retired. Only the rows
//!   it reads are put right: a refused Show- or Season-scope press re-reads the show or season row,
//!   and the episode rows the optimistic edit flipped in Browse listings stand until their next fetch.
//! * **One read in flight per item, plus a dirty flag.** Items are batched per server into one
//!   `metadata_many` (`/library/metadata/{k1,k2,…}`, already used by the hub windows and the person
//!   page), one batch per server at a time. An item asked for while its read is out is not read
//!   twice at once: it is marked dirty and re-queued ONCE when that read lands. There is no time cap.
//! * **No read is spawned while a write is queued or out.** The stamp below is taken at spawn, and
//!   items can wait in the queue (one flight per server, [`BATCH_KEYS`], the retry wait) past a
//!   later press; the spawn is gated on the same idleness `take_reread` is.
//! * **A result older than a press is dropped.** Each batch is stamped from the shared
//!   [`crate::viewledger::ViewLedger`] when it is requested. A row some edit published AFTER that
//!   stamp covers (a press made while the read was out) is discarded: the read describes the server
//!   from before that press, and the press's own write will queue its own read-back.
//! * **Results reach the stores as edits carrying facts**: `publish_view` of a
//!   [`crate::viewstate::ViewFact::Facts`] at `Scope::Leaf` (a show's facts describe the show row,
//!   never its episodes). Facts are the server's word, so they are not recorded in the ledger. They
//!   reach Home's hubs and Browse's listing and shelves so far; the other stores follow.
//! * **A failed read is not re-queued**, and the optimistic patch stands (the same "a failure never
//!   blanks a populated screen" rule as every other read here). A refused spawn is retried after
//!   [`RETRY_FRAMES`].
//!
//! Reset (sign-out, profile switch) drops the queue and rotates the adapter, so an old worker can
//! only post into a mailbox nobody reads. The landing is taken through `landgate` like every other
//! store's, and in dump mode it is owed exactly while a batch it has not yet received is out.

use crate::viewledger::{Stamp, ViewLedger};
use crate::viewstate::{Scope, ViewChange, ViewEdit, ViewFact};
use plx_plex::plex::ServerId;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// ~2 s at 60 fps, the wait every store here gives a refused spawn.
const RETRY_FRAMES: u32 = 120;

/// Keys per `metadata_many`: the URL carries them all, so a long burst is split.
const BATCH_KEYS: usize = 50;

type Key = (ServerId, String);

/// One read that is out: where, what, and where in the publish order it was requested.
struct Flight {
    id: u64,
    sid: ServerId,
    keys: Vec<String>,
    stamp: Stamp,
}

/// What a worker brings back. `rows` is `None` when the server did not answer.
struct Landing {
    id: u64,
    rows: Option<Vec<crate::pms::PmsMovie>>,
}

/// Cross-thread transport for one owner; reset replaces it.
#[derive(Default)]
pub struct ViewReconcileAdapter {
    mail: Mutex<Vec<Landing>>,
}

/// The read-back worker's owner: its queue, its flights and its notice.
pub struct ViewReconcileStore {
    ledger: ViewLedger,
    adapter: Arc<ViewReconcileAdapter>,
    pending: Vec<Key>,
    flights: Vec<Flight>,
    /// Items asked for again while their read was out: re-queued once when it lands.
    dirty: Vec<Key>,
    /// Stopped items that were NOT watched when their play began: when the read-back finds one
    /// watched, the stream reached its end and the title is owed a fan-out (`take_watched_ends`).
    /// Dropped when their read lands, fails or is overtaken, and on reset.
    candidates: Vec<Key>,
    watched_ends: Vec<Key>,
    retry_cd: u32,
    next_id: u64,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for ViewReconcileStore {
    fn default() -> Self {
        Self {
            ledger: Default::default(),
            adapter: Arc::new(Default::default()),
            pending: Vec::new(),
            flights: Vec::new(),
            dirty: Vec::new(),
            candidates: Vec::new(),
            watched_ends: Vec::new(),
            retry_cd: 0,
            next_id: 1,
            notice_gen: AtomicU32::new(0),
            notice_dirty: AtomicBool::new(false),
        }
    }
}

impl ViewReconcileStore {
    /// Join the owner's view-state publish order (`Stores::view_ledger`).
    pub(crate) fn share_ledger(&mut self, ledger: &ViewLedger) {
        self.ledger = ledger.clone();
    }

    fn bump(&self) {
        self.notice_dirty.store(true, Ordering::Relaxed);
        self.notice_gen.fetch_add(1, Ordering::Relaxed);
    }

    pub fn gen(&self) -> u32 {
        self.notice_gen.load(Ordering::Relaxed)
    }

    pub fn take_notice(&self) -> Option<u32> {
        self.notice_dirty.swap(false, Ordering::Relaxed).then(|| self.gen())
    }

    /// Is anything queued or out?
    pub fn is_busy(&self) -> bool {
        !self.pending.is_empty() || !self.flights.is_empty()
    }

    /// Owe a read of each `(server, ratingKey)`. An item whose read is out is marked dirty instead.
    pub fn enqueue(&mut self, keys: impl IntoIterator<Item = Key>) {
        for key in keys {
            let out = self.flights.iter()
                .any(|f| f.sid == key.0 && f.keys.iter().any(|k| plx_plex::plex::same_item((f.sid, k), (key.0, &key.1))));
            let list = if out { &mut self.dirty } else { &mut self.pending };
            if !list.iter().any(|k| plx_plex::plex::same_item((k.0, &k.1), (key.0, &key.1))) {
                list.push(key);
            }
        }
    }

    /// Owe a read of a stopped item. A first watch (`was_watched` false) is remembered, so that a
    /// read-back finding it watched reports an end of stream.
    pub fn enqueue_stop(&mut self, key: Key, was_watched: bool) {
        if !was_watched && !self.candidates.iter().any(|k| plx_plex::plex::same_item((k.0, &k.1), (key.0, &key.1))) {
            self.candidates.push(key.clone());
        }
        self.enqueue([key]);
    }

    /// The stopped items whose read-back found them watched having not been before.
    pub fn take_watched_ends(&mut self) -> Vec<Key> {
        std::mem::take(&mut self.watched_ends)
    }

    /// Drop everything: the identity changed. The adapter is replaced first, so a worker still out
    /// can only post to the retired one.
    pub fn reset(&mut self) {
        self.adapter = Arc::new(Default::default());
        self.pending.clear();
        self.flights.clear();
        self.dirty.clear();
        self.candidates.clear();
        self.watched_ends.clear();
        self.retry_cd = 0;
        self.bump();
    }

    /// Spawn one read per server that has queued items and none out. MAIN THREAD.
    fn kick(&mut self) {
        if self.retry_cd > 0 {
            self.retry_cd -= 1;
            return;
        }
        let mut servers: Vec<ServerId> = Vec::new();
        for (sid, _) in &self.pending {
            if !servers.contains(sid) && !self.flights.iter().any(|f| f.sid == *sid) {
                servers.push(*sid);
            }
        }
        for sid in servers {
            let mut keys: Vec<String> = Vec::new();
            self.pending.retain(|(s, rk)| {
                if *s == sid && keys.len() < BATCH_KEYS {
                    keys.push(rk.clone());
                    false
                } else {
                    true
                }
            });
            // Resolved here, at the spawn site; the worker never asks which server is current. A
            // slot that left the registry while queued has nowhere to read from.
            let Some(client) = plx_plex::plex::client_for(sid) else {
                plx_base::eventlog::log(&format!(
                    "viewreconcile: {} item(s) DROPPED — server {} is not registered", keys.len(), sid.raw()));
                continue;
            };
            let id = self.next_id;
            self.next_id += 1;
            let stamp = self.ledger.stamp();
            let mail = Arc::clone(&self.adapter);
            let asked = keys.clone();
            let spawned = plx_base::task::spawn_small("viewreconcile", move || {
                let rows = catch_unwind(AssertUnwindSafe(|| {
                    let refs: Vec<&str> = asked.iter().map(String::as_str).collect();
                    client.metadata_many(&refs).map(|mc| {
                        mc.metadata.iter().map(|m| crate::pms::parse_item(m, sid)).collect::<Vec<_>>()
                    })
                })).unwrap_or(None);
                mail.mail.lock().unwrap_or_else(|e| e.into_inner()).push(Landing { id, rows });
            });
            if !spawned {
                // Nothing will ever land. Keep the items and wait out the ladder.
                self.pending.extend(keys.into_iter().map(|rk| (sid, rk)));
                self.retry_cd = RETRY_FRAMES;
                return;
            }
            plx_base::eventlog::log(&format!(
                "viewreconcile: {} item(s) → server {} (off-thread)", keys.len(), sid.raw()));
            self.flights.push(Flight { id, sid, keys, stamp });
        }
    }

    /// Land what arrived, hand each surviving row to `publish` as a facts change, re-queue the
    /// items that went dirty, and send the next reads. MAIN THREAD, route-unconditional. True when
    /// a landing changed something a screen may be drawing.
    ///
    /// `may_spawn` is false while a view-state write is queued or out: a read's stamp is taken when
    /// it is spawned, so a press whose write has not landed would sit before the stamp and the
    /// read's body (from before that write) would undo it. Queued items wait for the burst to be
    /// idle; landings are always taken.
    pub fn pump_with_gate(
        &mut self,
        gate: &plx_machine::landgate::Gate,
        may_spawn: bool,
        publish: &mut dyn FnMut(ViewChange),
    ) -> bool {
        let adapter = Arc::clone(&self.adapter);
        // DUMP MODE: owed while some batch this owner sent has not been received yet.
        let landings = super::take_landings_owed(gate, super::StoreId::ViewReconcile,
            |got: &[Landing]| self.flights.iter().any(|f| !got.iter().any(|l| l.id == f.id)),
            || std::mem::take(&mut *adapter.mail.lock().unwrap_or_else(|e| e.into_inner())));
        let mut changed = false;
        for landing in landings {
            let Some(at) = self.flights.iter().position(|f| f.id == landing.id) else {
                plx_base::eventlog::log(&format!("viewreconcile: landing {} ignored — no such flight", landing.id));
                continue;
            };
            let flight = self.flights.remove(at);
            // Everything the user pressed after this read was requested: it describes the server
            // from before those presses.
            let overtaking: Vec<_> = self.ledger.since(flight.stamp).into_iter()
                .filter(|(_, edit)| matches!(edit, ViewEdit::Watched(_)))
                .map(|(at, _)| at)
                .collect();
            match landing.rows {
                None => plx_base::eventlog::log(&format!(
                    "viewreconcile: server {} did not answer; the patch stands", flight.sid.raw())),
                Some(rows) => {
                    let mut applied = 0;
                    for m in rows {
                        if !flight.keys.iter().any(|k| plx_plex::plex::same_item((flight.sid, k), (m.sid, &m.rk))) {
                            continue;
                        }
                        if overtaking.iter().any(|at| at.covers(&m)) {
                            continue;
                        }
                        if let Some(i) = self.candidates.iter()
                            .position(|k| plx_plex::plex::same_item((k.0, &k.1), (m.sid, &m.rk)))
                        {
                            let key = self.candidates.remove(i);
                            if m.watched {
                                self.watched_ends.push(key);
                            }
                        }
                        publish(ViewChange {
                            sid: m.sid,
                            rk: m.rk.clone(),
                            scope: Scope::Leaf,
                            fact: ViewFact::Facts { watched: m.watched, unwatched: m.unwatched, resume_ms: m.resume_ms },
                        });
                        applied += 1;
                    }
                    plx_base::eventlog::log(&format!(
                        "viewreconcile: server {} answered, {applied} of {} row(s) applied",
                        flight.sid.raw(), flight.keys.len()));
                }
            }
            // a candidate this read did not settle (no answer, no row, overtaken) is dropped: no
            // fan-out on a guess
            self.candidates.retain(|k| !(k.0 == flight.sid && flight.keys.iter()
                .any(|f| plx_plex::plex::same_item((flight.sid, f), (k.0, &k.1)))));
            // Asked for again while this was out: once more, now.
            let again: Vec<Key> = self.dirty.iter()
                .filter(|(s, rk)| *s == flight.sid && flight.keys.iter().any(|k| plx_plex::plex::same_item((*s, k), (*s, rk))))
                .cloned().collect();
            self.dirty.retain(|k| !again.contains(k));
            self.pending.extend(again);
            changed = true;
        }
        if may_spawn {
            self.kick();
        }
        if changed {
            self.bump();
        }
        changed
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn pending_for_test(&self) -> Vec<Key> {
        self.pending.clone()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn flights_for_test(&self) -> usize {
        self.flights.len()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn dirty_for_test(&self) -> Vec<Key> {
        self.dirty.clone()
    }
}

#[cfg(test)]
#[path = "viewreconcile_tests.rs"]
mod tests;
