//! The catalog store (plan §2.4 W6): owns `GET discover /library/metadata/<hex>` for the catalog
//! page, captures [`WatchlistAuth`] when it opens, and reads membership from `WatchlistStore`
//! (never from a copy of its own).
//!
//! Main-thread state plus an `Arc` mailbox. The store starts no worker: `open(hex)` puts one
//! [`StoreJob::Catalog`] on its outbox ([`CatalogStore::take_jobs`]) that the application's runner
//! starts in the same call (`Bridge::catalog_open`, `Bridge::catalog_pump`). The job's port posts
//! one landing tagged with the request id, and a landing for an older request, another title, or
//! another profile is dropped. The page never touches the metadata store: a catalog title has no
//! server item.
//!
//! **Every job is answered once, a refusal included.** A job the runner never started (the thread
//! ceiling) answers [`Outcome::NotStarted`] through the port's `refused`: the next pump takes it,
//! marks the request owed and asks again after [`RETRY_FRAMES`]. A read that panics lands as the
//! failure an unanswered one does ([`Load::Unreachable`]), so the page never waits on it.

use super::watchlist::{Status, WatchlistStore};
use super::{StoreId, StoreJob};
use crate::catalog::{record_of, CatalogIo, LiveCatalogIo, Load, Membership};
use crate::watchlist::{encode_hex, WatchlistAuth};
use plx_machine::job::{Job, Port};
use plx_machine::machine::{Addr, MachineId, RequestId};
use plx_plex::plex::watchlist::WatchlistError;
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Frames a refused job waits before the next try. A refused job never blocks a frame.
const RETRY_FRAMES: u32 = 120;

/// What a job brings back to the mailbox, whichever way it ended.
enum Outcome {
    /// The read ran (or panicked, which lands as unreachable).
    Read(Load),
    /// The runner never started the job. Nothing was asked of plex.tv and nothing is shown: the
    /// request is owed again and waits out [`RETRY_FRAMES`].
    NotStarted,
}

/// One job's answer, tagged with the request it answers.
struct Landing {
    req: u32,
    outcome: Outcome,
}

#[derive(Default)]
struct Mailbox {
    mail: Mutex<VecDeque<Landing>>,
}

impl Mailbox {
    fn post(&self, landing: Landing) {
        self.mail.lock().unwrap_or_else(|e| e.into_inner()).push_back(landing);
        plx_machine::idle::invalidate();
    }

    fn take_all(&self) -> Vec<Landing> {
        self.mail.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
    }
}

/// The one read a [`StoreJob::Catalog`] carries: who to ask as and which title, captured by the
/// step. The request id is the job's address. The seam it reads through rides on the job's port
/// ([`CatalogPort`]), so the request itself is a plain value.
#[derive(Clone, Debug)]
pub struct CatalogReq {
    auth: WatchlistAuth,
    hex: [u8; 12],
}

impl PartialEq for CatalogReq {
    /// [`WatchlistAuth`] carries a credential and no `PartialEq` of its own: two requests are equal
    /// when they ask the same title as the same identity.
    fn eq(&self, other: &Self) -> bool {
        self.hex == other.hex
            && self.auth.client_id == other.auth.client_id
            && self.auth.credential == other.auth.credential
            && self.auth.generation == other.auth.generation
    }
}

#[cfg(test)]
impl CatalogReq {
    pub(crate) fn for_test(auth: WatchlistAuth, hex: [u8; 12]) -> Self {
        Self { auth, hex }
    }
}

/// A read's answer, opaque to the runner: the page state it lands. The runner never names
/// [`Outcome`].
pub struct CatalogAnswer(Load);

/// Where a catalog job's ONE answer goes: the store's mailbox as it was when the job was emitted,
/// and the I/O seam the read goes through (production is [`LiveCatalogIo`]; the seam rides here, not
/// in the request, so the request stays a comparable value). A store that has since opened another
/// title, or been reset, holds a new mailbox, and this port still reaches the retired one nobody
/// pumps. Two ports are equal when they reach the same mailbox.
#[derive(Clone)]
pub struct CatalogPort {
    mailbox: Arc<Mailbox>,
    io: Arc<dyn CatalogIo>,
}

impl PartialEq for CatalogPort {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.mailbox, &other.mailbox)
    }
}

impl std::fmt::Debug for CatalogPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CatalogPort({:p})", Arc::as_ptr(&self.mailbox))
    }
}

impl Port<CatalogAnswer> for CatalogPort {
    /// The read's answer, under the request's id.
    fn done(&self, addr: Addr, out: CatalogAnswer) {
        self.mailbox.post(Landing { req: addr.req.0, outcome: Outcome::Read(out.0) });
    }

    /// The job was never started, so nothing will land: say so under the request's id, which is
    /// what makes the next pump owe the request again.
    fn refused(&self, addr: Addr) {
        self.mailbox.post(Landing { req: addr.req.0, outcome: Outcome::NotStarted });
    }
}

/// A catalog job's address: its store and the request id. One read is out at a time and a new
/// `open` or `reset` takes a new id, so no two jobs in flight share an address.
fn catalog_addr(req: u32) -> Addr {
    Addr { to: MachineId::Store(StoreId::Catalog.ord()), req: RequestId(req) }
}

/// The job's IO body and its completion: read the title through the port's seam and post the
/// answer to `port`. This is the worker body the store used to start itself, unchanged
/// (`LiveCatalogIo::fetch` keeps the dev-build read refusal inside `watchlist_transport`), except
/// that a read which unwinds still answers. The answer is posted OUTSIDE the guard, so a panic
/// lands as [`Load::Unreachable`] and never as a missing answer.
pub fn finish_catalog_job(port: &CatalogPort, addr: Addr, req: CatalogReq) {
    let CatalogReq { auth, hex } = req;
    let io = &port.io;
    let load = catch_unwind(AssertUnwindSafe(|| match io.fetch(&auth, &encode_hex(&hex)) {
        Ok(item) => Load::Loaded(Box::new(record_of(&item, io.today().as_deref()))),
        Err(WatchlistError::NotFound) => Load::NotFound,
        // Offline, refused credential, unreadable body, any other status: nothing is stored; the page shows its unreachable state.
        Err(_) => Load::Unreachable,
    }))
    .unwrap_or_else(|_| {
        plx_base::eventlog::log("catalog: the read panicked; the page reads unreachable");
        Load::Unreachable
    });
    port.done(addr, CatalogAnswer(load));
}

pub struct CatalogStore {
    /// The title the page is about, set by `open`.
    pub hex: Option<[u8; 12]>,
    pub load: Load,
    io: Arc<dyn CatalogIo>,
    auth: Option<WatchlistAuth>,
    req: u32,
    /// The request is made but no job is out for it: the runner refused the last one. The pump
    /// emits a new job after [`RETRY_FRAMES`].
    owed: bool,
    retry: u32,
    mailbox: Arc<Mailbox>,
    /// The jobs the step has emitted and nobody has taken to a runner yet ([`Self::take_jobs`]).
    jobs: Vec<StoreJob>,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for CatalogStore {
    fn default() -> Self {
        Self::with_io(Arc::new(LiveCatalogIo))
    }
}

impl CatalogStore {
    /// A store over a given I/O seam (tests script it; production is [`LiveCatalogIo`]).
    pub fn with_io(io: Arc<dyn CatalogIo>) -> Self {
        Self {
            hex: None,
            load: Load::Loading,
            io,
            auth: None,
            req: 0,
            owed: false,
            retry: 0,
            mailbox: Arc::new(Mailbox::default()),
            jobs: Vec::new(),
            notice_gen: AtomicU32::new(0),
            notice_dirty: AtomicBool::new(false),
        }
    }

    pub fn gen(&self) -> u32 {
        self.notice_gen.load(Ordering::Relaxed)
    }

    pub fn take_notice(&self) -> Option<u32> {
        self.notice_dirty.swap(false, Ordering::Relaxed).then(|| self.gen())
    }

    /// Raise a notice (a landing changed `load`).
    pub fn bump(&self) {
        self.notice_dirty.store(true, Ordering::Relaxed);
        self.notice_gen.fetch_add(1, Ordering::Relaxed);
    }

    /// The page mounted (or was read again: `Bridge::catalog_reread`) for the title `hex`: capture the identity if
    /// the held one is not the seated profile's, and read the record. Every call starts a fresh
    /// request, so an older one still in flight lands into nothing. It does NOT start the read it
    /// emitted: the caller takes the job with [`CatalogStore::take_jobs`] and hands it to a runner
    /// in the same turn (the production caller is `Bridge::catalog_open`).
    pub fn open(&mut self, hex: [u8; 12]) {
        if self.auth.as_ref().is_none_or(|a| !self.io.is_current(a)) {
            self.auth = self.io.capture_auth();
        }
        self.hex = Some(hex);
        self.req = self.req.wrapping_add(1);
        self.retry = 0;
        self.load = Load::Loading;
        // A fresh mailbox: a retired request's landing never reaches the new one. A job not yet
        // taken to a runner belonged to the retired request and goes with it.
        self.mailbox = Arc::new(Mailbox::default());
        self.jobs.clear();
        if self.auth.is_some() {
            self.start();
        } else {
            // No account credential: nothing can be asked.
            self.owed = false;
            self.load = Load::Unreachable;
        }
        self.bump();
    }

    /// Sign-out or profile switch: forget the title, the identity and any request in flight. A job
    /// not yet taken to a runner is dropped with it, credential and all.
    pub fn reset(&mut self) {
        self.hex = None;
        self.auth = None;
        self.req = self.req.wrapping_add(1);
        self.owed = false;
        self.load = Load::Loading;
        self.mailbox = Arc::new(Mailbox::default());
        self.jobs.clear();
        self.bump();
    }

    /// Emit the job that reads the open title as the captured identity.
    fn start(&mut self) {
        let (Some(hex), Some(auth)) = (self.hex, self.auth.clone()) else {
            self.owed = false;
            return;
        };
        self.jobs.push(StoreJob::Catalog(
            Job { addr: catalog_addr(self.req), req: CatalogReq { auth, hex } },
            self.catalog_port(),
        ));
        self.owed = false;
    }

    /// Take the jobs the step has emitted since the last take, in order. The caller OWES each one
    /// to a runner: a job dropped here is a read whose answer never comes.
    #[must_use]
    pub fn take_jobs(&mut self) -> Vec<StoreJob> {
        std::mem::take(&mut self.jobs)
    }

    /// Does the outbox hold a job nobody has taken yet?
    pub fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// The port that lands an answer into THIS store's current mailbox: what a job emitted now
    /// carries (`assert_fx!` compares it by mailbox identity).
    pub fn catalog_port(&self) -> CatalogPort {
        CatalogPort { mailbox: Arc::clone(&self.mailbox), io: Arc::clone(&self.io) }
    }

    /// Frame pass: ask again for a request the runner refused, then take the job's landing.
    /// Returns whether `load` changed. A landing for a profile that is no longer seated is dropped
    /// and the page asks again under the new identity. Like [`CatalogStore::open`], it does NOT
    /// start the job it emitted (a retry, or the new identity's read): the production caller is
    /// `Bridge::catalog_pump`, which drains in the same call.
    pub fn pump(&mut self) -> bool {
        if self.owed {
            self.retry += 1;
            if self.retry >= RETRY_FRAMES {
                self.retry = 0;
                self.start();
            }
        }
        let mut changed = false;
        for landing in self.mailbox.take_all() {
            if landing.req != self.req {
                continue;
            }
            let load = match landing.outcome {
                Outcome::Read(load) => load,
                Outcome::NotStarted => {
                    // Nothing was asked and nothing is shown: owe the request, wait out the ladder.
                    self.owed = true;
                    self.retry = 0;
                    continue;
                }
            };
            if self.auth.as_ref().is_some_and(|a| !self.io.is_current(a)) {
                self.auth = None;
                if let Some(hex) = self.hex {
                    self.open(hex);
                }
                return true;
            }
            self.load = load;
            changed = true;
        }
        if changed {
            self.bump();
        }
        changed
    }

    /// The status line the open title wears: what its tile on the Watchlist says (a match landing
    /// while the page is open moves it), else `None` and the record's own date rule stands.
    pub fn tile_status(&self, watchlist: &WatchlistStore) -> Option<crate::catalog::CatalogStatus> {
        let hex = self.hex?;
        watchlist.view().tile_state_of(&hex).map(crate::catalog::CatalogStatus::of_tile)
    }

    /// Is the open title on the signed-in profile's Watchlist? Read from the watchlist store,
    /// intents in flight counted (a press flips at once, a failed write reverts). `Unknown` while
    /// the list has not been read (or plex.tv could not be reached): the page then wears the Add
    /// face, since the PUT is idempotent.
    pub fn membership(&self, watchlist: &WatchlistStore) -> Membership {
        let Some(hex) = self.hex else { return Membership::Unknown };
        let view = watchlist.view();
        if view.is_member(&hex) {
            Membership::On
        } else if view.status() == Status::Ready {
            Membership::Off
        } else {
            Membership::Unknown
        }
    }
}
