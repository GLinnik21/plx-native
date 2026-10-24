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
//!   ([`crate::viewstate::ViewStateState::take_reread`]) once its burst is idle. No read is sent while the player is on top
//!   (`Stores::hold_network`) either: the items stay owed. On success the
//!   answer confirms the optimistic edit; on refusal it is the only thing that returns the tile to
//!   what the server holds, because the ledger entry of a refused write is retired. Only the rows
//!   it reads are put right: a refused Show- or Season-scope press re-reads the show or season row,
//!   and the episode rows the optimistic edit flipped in Browse listings stand until their next fetch.
//! * **One read in flight per item, plus a dirty flag.** Items are batched per server into one
//!   `metadata_many` (`/library/metadata/{k1,k2,…}`, already used by the hub windows and the person
//!   page), one batch per server at a time. An item asked for while its read is out is not read
//!   twice at once: it is marked dirty and re-queued ONCE when that read lands. There is no time cap.
//! * **No read is sent while a write is queued or out.** The stamp below is taken when the batch is
//!   sent, and items can wait in the queue (one flight per server, [`BATCH_KEYS`], the retry wait)
//!   past a later press; the send is gated on the same idleness `take_reread` is.
//! * **A result older than a press is dropped.** Each batch is stamped from the shared
//!   [`crate::viewledger::ViewLedger`] when it is requested. A row some edit published AFTER that
//!   stamp covers (a press made while the read was out) is discarded: the read describes the server
//!   from before that press, and the press's own write will queue its own read-back.
//! * **A leaf's season and show are read too.** What an episode's change moved is also the season's
//!   and the show's `viewedLeafCount`, and a Library grid, a Search shelf or a Home card holding
//!   those rows must say so. The keys are not on the row that was pressed, so they come from the
//!   leaf's own answer (`parentRatingKey`, `grandparentRatingKey`) and are read in ONE more batch
//!   per server, the same request shape as the first: an episode costs two reads, a movie one.
//!   The extra rows are published and followed no further. Their facts are the server's own word
//!   (`parse_item`: watched is `viewedLeafCount >= leafCount`), each addressed to its exact
//!   `(server, ratingKey)`, so another server's show of the same key is not touched. Read-backs of
//!   a burst share the extra batch: one extra batch per server per leaf batch (a burst of more than
//!   [`BATCH_KEYS`] leaves is several leaf batches, and each can name its own parents).
//! * **Results reach the stores as edits carrying facts**: `publish_view` of a
//!   [`crate::viewstate::ViewFact::Facts`] at `Scope::Leaf` (a show's facts describe the show row,
//!   never its episodes). Facts are the server's word, so they are not recorded in the ledger. They
//!   reach Home's hubs, Browse's listing, Search, Person, Collection and Detail's Related and
//!   collection-member tiles, each only where `ViewEdit::changes` says the row differs. Browse's
//!   section shelves (re-read whole after a burst) and Detail's own item, episodes and seasons
//!   take no `Facts` (a stop's `Offset` does reach the loaded item and its episode): they cannot be patched from a facts row (the season tabs' counts and the hero's
//!   on-deck episode are the server's numbers). Every row a read-back publishes is also listed for
//!   the application ([`ViewReconcileStore::take_landed`]), which re-reads each Detail page whose
//!   item, season or show it is.
//!   A stop the server REFUSED asks for no read-back (it would answer with the old resume point and
//!   undo the patches), but it too lists its item there ([`ViewReconcileStore::owe_page`]): the
//!   Detail pages were patched with the position the viewer reached (`Offset` reaches the loaded
//!   item and its episode), and their own re-read puts the server's word back.
//! * **A failed read is not re-queued**, and the optimistic patch stands (the same "a failure never
//!   blanks a populated screen" rule as every other read here). A read that was never STARTED is
//!   retried after [`RETRY_FRAMES`] (`Outcome::NotStarted`).
//!
//! ## Jobs, not threads
//!
//! The step starts no worker and reads no registry. [`ViewReconcileStore::pump_with_gate`] is handed
//! the registry's answer as an input ([`ClientKeys`]), and each batch it sends is a typed
//! [`StoreJob::ViewReconcile`] on an outbox ([`ViewReconcileStore::take_jobs`]) that the application's
//! runner starts in the same call (`Bridge::reconcile_pump`). The job names the client the step
//! decided on ([`ReconcileReq::server`]); the runner starts it only if the registry still holds that
//! client at the launch. The kind is not on the tape, so the runner records nothing for it.
//!
//! **Every job is answered once, a refusal included.** A read that is never started (the thread
//! ceiling, or a registry that has moved on between the step and the launch) answers
//! `Outcome::NotStarted` through the port's `refused`: the next pump takes it as the flight's
//! landing, puts the flight's items back and waits [`RETRY_FRAMES`]. Without the answer the flight
//! would stay owed for ever, and the landing's dump-mode wait with it.
//!
//! Reset (sign-out, profile switch) drops the queue and rotates the adapter, so an old worker can
//! only post into a mailbox nobody reads, and a job's port keeps reaching the adapter it was made
//! with (a job still on the outbox is kept for the drain, not dropped). The landing is taken
//! through `landgate` like every other store's, and in dump mode it is owed exactly while a batch
//! it has not yet received is out.

use super::StoreJob;
use crate::viewledger::{Stamp, ViewLedger};
use crate::viewstate::{Scope, ViewChange, ViewEdit, ViewFact};
use plx_machine::job::{Job, Port};
use plx_machine::machine::{Addr, MachineId, RequestId};
use plx_plex::plex::{Client, ClientKey, ClientKeys, ServerId};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// ~2 s at 60 fps, the wait every store here gives a job that was refused.
const RETRY_FRAMES: u32 = 120;

/// Keys per `metadata_many`: the URL carries them all, so a long burst is split.
const BATCH_KEYS: usize = 50;

type Key = (ServerId, String);

/// One read that is out: where, what, and where in the publish order it was requested.
struct Flight {
    /// Unique among the store's flights; also the job's [`RequestId`]. A `u32` so it fits there: a
    /// flight is minted only when a batch goes out, and four billion of them is not a lifetime.
    id: u32,
    sid: ServerId,
    /// Everything this read asked for: the items owed a read-back, then `parents`.
    keys: Vec<String>,
    /// The tail of `keys` asked only because a leaf's answer named them as its season or show. Their
    /// rows are published and never followed to a parent of their own.
    parents: Vec<String>,
    stamp: Stamp,
}

/// One answered row and the keys it names above itself: a season for an episode (`parent`) and the
/// show (`grandparent`); a season names its show as `parent`. Empty where the row has none.
struct ReadRow {
    movie: crate::pms::PmsMovie,
    parent: String,
    grandparent: String,
}

/// What a flight comes back with.
enum Outcome {
    /// The read ran. `None` when the server did not answer.
    Read(Option<Vec<ReadRow>>),
    /// The read was never started: the runner refused the job. Nothing was asked of the server, so
    /// nothing is published and no stop candidate is settled; the flight's items go back in the
    /// queue and wait out [`RETRY_FRAMES`].
    NotStarted,
}

/// What a flight brings back to the mailbox, whichever way it ended.
struct Landing {
    id: u32,
    outcome: Outcome,
}

/// The one read a [`StoreJob::ViewReconcile`] carries: everything the worker needs, captured by the
/// step. `key` is the client the step decided on; a runner starts the read only if the registry
/// still holds that client at the launch.
#[derive(Clone, Debug, PartialEq)]
pub struct ReconcileReq {
    sid: ServerId,
    key: ClientKey,
    /// The ratingKeys of the batch: the items owed a read-back, then the parents that ride with them.
    keys: Vec<String>,
}

impl ReconcileReq {
    /// The server to ask and the client the step decided to ask it through.
    pub fn server(&self) -> (ServerId, ClientKey) { (self.sid, self.key) }
}

/// A read's answer, opaque to the runner: the rows the pump publishes. The runner never names
/// `ReadRow`.
pub struct ReconcileAnswer(Option<Vec<ReadRow>>);

/// Where a read's ONE answer goes: the store's mailbox, captured with the job when it was emitted.
/// A store that has since been reset holds a new adapter, and this port still reaches the one the
/// read was made from, the retired one nobody pumps. Two ports are equal when they reach the same
/// adapter.
#[derive(Clone)]
pub struct ReconcilePort(Arc<ViewReconcileAdapter>);

impl ReconcilePort {
    fn of(adapter: &Arc<ViewReconcileAdapter>) -> Self { Self(Arc::clone(adapter)) }

    fn post(&self, id: u32, outcome: Outcome) {
        self.0.mail.lock().unwrap_or_else(|e| e.into_inner()).push(Landing { id, outcome });
    }
}

impl PartialEq for ReconcilePort {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.0, &other.0) }
}

impl std::fmt::Debug for ReconcilePort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ReconcilePort({:p})", Arc::as_ptr(&self.0))
    }
}

impl Port<ReconcileAnswer> for ReconcilePort {
    /// The flight's rows (`None`: the server did not answer), under the flight's id.
    fn done(&self, addr: Addr, out: ReconcileAnswer) {
        self.post(addr.req.0, Outcome::Read(out.0));
    }

    /// The read was never started, so nothing will land: say so under the flight's id, which is what
    /// takes the flight off the owed list and sends its items back to the queue.
    fn refused(&self, addr: Addr) {
        self.post(addr.req.0, Outcome::NotStarted);
    }
}

/// A flight's address: its store and its id. Flights are unique, so no two reads share an address.
fn reconcile_addr(id: u32) -> Addr {
    Addr { to: MachineId::Store(super::StoreId::ViewReconcile.ord()), req: RequestId(id) }
}

/// The read's IO body and its completion: ask `client` for the batch and post the rows to `port`.
/// The answer is posted OUTSIDE the guard, so a read that panics lands as "the server did not
/// answer" and never as a missing answer.
pub fn finish_reconcile_job(port: &ReconcilePort, addr: Addr, client: &'static Client, req: ReconcileReq) {
    let ReconcileReq { sid, keys, .. } = req;
    let rows = catch_unwind(AssertUnwindSafe(|| {
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        client.metadata_many(&refs).map(|mc| {
            mc.metadata.iter().map(|m| ReadRow {
                movie: crate::pms::parse_item(m, sid),
                parent: crate::pms::clean(&m.parent_rating_key),
                grandparent: crate::pms::clean(&m.grandparent_rating_key),
            }).collect::<Vec<_>>()
        })
    })).unwrap_or(None);
    port.done(addr, ReconcileAnswer(rows));
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
    /// Seasons and shows the answers named, owed one more read ([`Flight::parents`]).
    pending_parents: Vec<Key>,
    flights: Vec<Flight>,
    /// Items asked for again while their read was out: re-queued once when it lands.
    dirty: Vec<Key>,
    /// Stopped items that were NOT watched when their play began: when the read-back finds one
    /// watched, the stream reached its end and the title is owed a fan-out (`take_watched_ends`).
    /// Dropped when their read lands, fails or is overtaken, and on reset. Each carries the id the
    /// NEXT flight will be minted with when the stop was enqueued: only a flight with an id at or
    /// past it was sent after the stop, and only its body can settle the candidate. A read that
    /// was already out describes the server from before the stop.
    candidates: Vec<(Key, u32)>,
    watched_ends: Vec<Key>,
    /// Rows published since the last [`Self::take_landed`]: the application's cue to re-read the
    /// Detail pages that show them.
    landed: Vec<Key>,
    /// Queued or out for a quiet re-read ([`Self::enqueue_reread`]): when one lands it publishes
    /// its facts but owes no Detail page ([`Self::owe_page`]). The seasons and shows its answer
    /// names ride along quiet.
    quiet: Vec<Key>,
    retry_cd: u32,
    next_id: u32,
    /// The reads the step has sent and nobody has taken to a runner yet ([`Self::take_jobs`]).
    jobs: Vec<StoreJob>,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for ViewReconcileStore {
    fn default() -> Self {
        Self {
            ledger: Default::default(),
            adapter: Arc::new(Default::default()),
            pending: Vec::new(),
            pending_parents: Vec::new(),
            flights: Vec::new(),
            dirty: Vec::new(),
            candidates: Vec::new(),
            watched_ends: Vec::new(),
            landed: Vec::new(),
            quiet: Vec::new(),
            retry_cd: 0,
            next_id: 1,
            jobs: Vec::new(),
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
        !self.pending.is_empty() || !self.pending_parents.is_empty() || !self.flights.is_empty()
    }

    /// Is a read of `key` in flight?
    fn is_out(&self, key: &Key) -> bool {
        self.flights.iter()
            .any(|f| f.sid == key.0 && f.keys.iter().any(|k| plx_plex::plex::same_item((f.sid, k), (key.0, &key.1))))
    }

    /// Owe a read of each `(server, ratingKey)`. An item whose read is out is marked dirty instead.
    pub fn enqueue(&mut self, keys: impl IntoIterator<Item = Key>) {
        for key in keys {
            self.quiet.retain(|k| !plx_plex::plex::same_item((k.0, &k.1), (key.0, &key.1)));
            let list = if self.is_out(&key) { &mut self.dirty } else { &mut self.pending };
            if !list.iter().any(|k| plx_plex::plex::same_item((k.0, &k.1), (key.0, &key.1))) {
                list.push(key);
            }
        }
    }

    /// Owe a QUIET read of each key (a page re-read, L2-5): its answer is published as facts like
    /// any other, but owes no Detail page a re-read, so the Detail on screen is not re-fetched for
    /// the sake of the page under it. A key already queued or out is left to its own owner: a
    /// quiet ask never downgrades an owing one.
    pub fn enqueue_reread(&mut self, keys: impl IntoIterator<Item = Key>) {
        for key in keys {
            let fresh = !(self.pending.iter().chain(&self.pending_parents).chain(&self.dirty)
                .any(|k| plx_plex::plex::same_item((k.0, &k.1), (key.0, &key.1)))
                || self.is_out(&key));
            self.enqueue([key.clone()]);
            if fresh {
                self.quiet.push(key);
            }
        }
    }

    /// Owe a read of a stopped item. A first watch (`was_watched` false) is remembered, so that a
    /// read-back finding it watched reports an end of stream.
    pub fn enqueue_stop(&mut self, key: Key, was_watched: bool) {
        if !was_watched {
            // Re-stamped when the item stops again: the newest stop is the one a read must follow.
            let born = self.next_id;
            match self.candidates.iter_mut().find(|(k, _)| plx_plex::plex::same_item((k.0, &k.1), (key.0, &key.1))) {
                Some((_, at)) => *at = born,
                None => self.candidates.push((key.clone(), born)),
            }
        }
        self.enqueue([key]);
    }

    /// The stopped items whose read-back found them watched having not been before.
    pub fn take_watched_ends(&mut self) -> Vec<Key> {
        std::mem::take(&mut self.watched_ends)
    }

    /// Owe the Detail pages that show `key` a re-read of their own, once ([`Self::take_landed`]).
    /// A published row does it, and so does a stop the server refused: the page was patched with
    /// the position the viewer reached, no read-back is asked (it would answer with the old resume
    /// point and undo the tile patches), and the page's re-read is what puts the server's word back.
    pub fn owe_page(&mut self, key: Key) {
        if !self.landed.iter().any(|k| plx_plex::plex::same_item((k.0, &k.1), (key.0, &key.1))) {
            self.landed.push(key);
        }
    }

    /// The rows read-backs have published since the last call, each once.
    pub fn take_landed(&mut self) -> Vec<Key> {
        std::mem::take(&mut self.landed)
    }

    /// Drop everything: the identity changed. The adapter is replaced first, so a worker still out
    /// can only post to the retired one. A read the step sent and nobody has taken to a runner yet
    /// stays on the outbox, as a job does in the other stores: it answers into the adapter it was
    /// made for, and a drain that missed it is still seen by the backstop.
    pub fn reset(&mut self) {
        self.adapter = Arc::new(Default::default());
        self.pending.clear();
        self.pending_parents.clear();
        self.flights.clear();
        self.dirty.clear();
        self.candidates.clear();
        self.watched_ends.clear();
        self.landed.clear();
        self.quiet.clear();
        self.retry_cd = 0;
        self.bump();
    }

    /// Send one read per server that has queued items and none out, as a job each. MAIN THREAD.
    /// `clients` is the registry's answer as of this pump; a server it holds no client for has
    /// nowhere to read from, and its batch is dropped.
    fn kick(&mut self, clients: &ClientKeys) {
        if self.retry_cd > 0 {
            self.retry_cd -= 1;
            return;
        }
        let mut servers: Vec<ServerId> = Vec::new();
        for (sid, _) in self.pending.iter().chain(&self.pending_parents) {
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
            // The seasons and shows the last answers named ride in the same batch: one request per
            // server however many parents there are.
            let mut parents: Vec<String> = Vec::new();
            self.pending_parents.retain(|(s, rk)| {
                if *s != sid || keys.len() >= BATCH_KEYS {
                    return true;
                }
                if !keys.iter().any(|k| plx_plex::plex::same_item((sid, k), (sid, rk))) {
                    keys.push(rk.clone());
                    parents.push(rk.clone());
                }
                false
            });
            // The client is the registry's answer as of this pump; the job carries its key and the
            // runner starts the read only through that client. A slot that left the registry
            // while queued has nowhere to read from.
            let Some(key) = clients.key(sid) else {
                plx_base::eventlog::log(&format!(
                    "viewreconcile: {} item(s) DROPPED — server {} is not registered", keys.len(), sid.raw()));
                continue;
            };
            let id = self.next_id;
            self.next_id += 1;
            let stamp = self.ledger.stamp();
            plx_base::eventlog::log(&format!(
                "viewreconcile: {} item(s) → server {} (sent to the runner)", keys.len(), sid.raw()));
            self.jobs.push(StoreJob::ViewReconcile(
                Job { addr: reconcile_addr(id), req: ReconcileReq { sid, key, keys: keys.clone() } },
                ReconcilePort::of(&self.adapter),
            ));
            self.flights.push(Flight { id, sid, keys, parents, stamp });
        }
    }

    /// Take the reads the step has sent since the last take, in order. The caller OWES each one to
    /// a runner: a job dropped here is a flight that is never answered.
    #[must_use]
    pub fn take_jobs(&mut self) -> Vec<StoreJob> {
        std::mem::take(&mut self.jobs)
    }

    /// Does the outbox hold a job nobody has taken yet?
    pub fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// Take out of `dirty` every item asked for again while a read of `keys` on `sid` was out.
    fn take_asked_again(&mut self, sid: ServerId, keys: &[String]) -> Vec<Key> {
        let (again, rest) = std::mem::take(&mut self.dirty).into_iter().partition(|(s, rk)| {
            *s == sid && keys.iter().any(|k| plx_plex::plex::same_item((sid, k), (*s, rk)))
        });
        self.dirty = rest;
        again
    }

    /// A flight whose read was never started goes back where it came from: its leaves into
    /// `pending`, its trailing parents into `pending_parents`, and the retry wait starts. Nothing
    /// was asked of the server, so this publishes nothing and settles nothing: the stop candidates,
    /// the quiet marks and the pages owed are exactly as they were. An item asked for again while
    /// the flight was out is already coming back with it, so its dirty mark is dropped rather than
    /// queueing it a second time. A parent that was also asked for as an item goes back as the item
    /// it was asked for (a leaf), as a landing's re-queue of its dirty marks does, so its own season
    /// and show are read after it.
    fn requeue_unstarted(&mut self, flight: Flight) {
        let Flight { sid, keys, parents, .. } = flight;
        let asked_again = self.take_asked_again(sid, &keys);
        let queued = |list: &[Key], rk: &str| list.iter().any(|k| plx_plex::plex::same_item((k.0, &k.1), (sid, rk)));
        let leaves = keys.len() - parents.len();
        let mut keys = keys.into_iter();
        for rk in keys.by_ref().take(leaves) {
            if !queued(&self.pending, &rk) {
                self.pending.push((sid, rk));
            }
        }
        for rk in keys {
            if queued(&self.pending, &rk) || queued(&self.pending_parents, &rk) {
                continue;
            }
            if queued(&asked_again, &rk) {
                self.pending.push((sid, rk));
            } else {
                self.pending_parents.push((sid, rk));
            }
        }
        self.retry_cd = RETRY_FRAMES;
        plx_base::eventlog::log(&format!(
            "viewreconcile: server {} read NOT STARTED; its item(s) wait {RETRY_FRAMES} frames", sid.raw()));
    }

    /// Land what arrived, hand each surviving row to `publish` as a facts change, re-queue the
    /// items that went dirty, and send the next reads. MAIN THREAD, route-unconditional. True when
    /// a landing changed something a screen may be drawing.
    ///
    /// `may_send` is false while a view-state write is queued or out: a read's stamp is taken when
    /// it is sent, so a press whose write has not landed would sit before the stamp and the
    /// read's body (from before that write) would undo it. Queued items wait for the burst to be
    /// idle; landings are always taken. `clients` is the registry's answer as of this call; the
    /// reads this pump sends are on the outbox ([`Self::take_jobs`]) when it returns.
    pub fn pump_with_gate(
        &mut self,
        gate: &plx_machine::landgate::Gate,
        may_send: bool,
        clients: &ClientKeys,
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
            let rows = match landing.outcome {
                Outcome::Read(rows) => rows,
                Outcome::NotStarted => {
                    self.requeue_unstarted(flight);
                    continue;
                }
            };
            // Quiet only for this flight: a later ask for the same key is its own.
            let quiet: Vec<Key> = flight.keys.iter().map(|k| (flight.sid, k.clone()))
                .filter(|key| self.quiet.iter().any(|q| plx_plex::plex::same_item((q.0, &q.1), (key.0, &key.1))))
                .collect();
            self.quiet.retain(|q| !quiet.contains(q));
            // Everything the user pressed after this read was requested: it describes the server
            // from before those presses.
            let overtaking: Vec<_> = self.ledger.since(flight.stamp).into_iter()
                .filter(|(_, edit)| matches!(edit, ViewEdit::Watched(_)))
                .map(|(at, _)| at)
                .collect();
            match rows {
                None => plx_base::eventlog::log(&format!(
                    "viewreconcile: server {} did not answer; the patch stands", flight.sid.raw())),
                Some(rows) => {
                    let mut applied = 0;
                    for ReadRow { movie: m, parent, grandparent } in rows {
                        if !flight.keys.iter().any(|k| plx_plex::plex::same_item((flight.sid, k), (m.sid, &m.rk))) {
                            continue;
                        }
                        if overtaking.iter().any(|at| at.covers(&m)) {
                            continue;
                        }
                        let is_quiet = quiet.iter().any(|k| plx_plex::plex::same_item((k.0, &k.1), (m.sid, &m.rk)));
                        let is_parent = flight.parents.iter().any(|p| plx_plex::plex::same_item((flight.sid, p), (m.sid, &m.rk)));
                        if !is_parent {
                            // Its season and its show hold counts of their own, which this item's
                            // change moved: read them too, unless this very read already did.
                            for up in [parent, grandparent] {
                                if up.is_empty() || flight.keys.iter().any(|k| plx_plex::plex::same_item((flight.sid, k), (flight.sid, &up))) {
                                    continue;
                                }
                                let key = (flight.sid, up);
                                let queued = |list: &[Key]| list.iter().any(|k| plx_plex::plex::same_item((k.0, &k.1), (key.0, &key.1)));
                                if !queued(&self.pending) && !queued(&self.pending_parents) {
                                    if is_quiet {
                                        self.quiet.push(key.clone());
                                    }
                                    self.pending_parents.push(key);
                                }
                            }
                        }
                        if let Some(i) = self.candidates.iter()
                            .position(|(k, born)| *born <= flight.id && plx_plex::plex::same_item((k.0, &k.1), (m.sid, &m.rk)))
                        {
                            let (key, _) = self.candidates.remove(i);
                            if m.watched {
                                self.watched_ends.push(key);
                            }
                        }
                        if !is_quiet {
                            self.owe_page((m.sid, m.rk.clone()));
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
            // fan-out on a guess. One enqueued after the read was sent is not this read's to
            // drop: its item is dirty and the re-read sent after the stop decides it.
            self.candidates.retain(|(k, born)| *born > flight.id || !(k.0 == flight.sid && flight.keys.iter()
                .any(|f| plx_plex::plex::same_item((flight.sid, f), (k.0, &k.1)))));
            // Asked for again while this was out: once more, now.
            let again = self.take_asked_again(flight.sid, &flight.keys);
            self.pending.extend(again);
            changed = true;
        }
        if may_send {
            self.kick(clients);
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
    pub fn candidates_for_test(&self) -> Vec<Key> {
        self.candidates.iter().map(|(k, _)| k.clone()).collect()
    }

    /// Is `key` owed a QUIET read (no Detail page is owed a re-read when it lands)?
    #[cfg(any(test, feature = "test-support"))]
    pub fn is_quiet_for_test(&self, key: &Key) -> bool {
        self.quiet.iter().any(|k| plx_plex::plex::same_item((k.0, &k.1), (key.0, &key.1)))
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

#[cfg(test)]
#[path = "viewreconcile_parents_tests.rs"]
mod parents_tests;

#[cfg(test)]
#[path = "viewreconcile_rig.rs"]
mod rig;

#[cfg(test)]
#[path = "viewreconcile_job_tests.rs"]
mod job_tests;
