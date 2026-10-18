//! The catalog store (plan §2.4 W6): owns `GET discover /library/metadata/<hex>` for the catalog
//! page, captures [`WatchlistAuth`] when it opens, and reads membership from `WatchlistStore`
//! (never from a copy of its own).
//!
//! Main-thread state plus an `Arc` mailbox. `open(hex)` starts a request (one `spawn_small`
//! worker); the worker posts one landing tagged with the request id, and a landing for an older
//! request, another title, or another profile is dropped. The page never touches the metadata
//! store: a catalog title has no server item.

use super::watchlist::{Status, WatchlistStore};
use crate::catalog::{record_of, CatalogIo, LiveCatalogIo, Load, Membership};
use crate::watchlist::{encode_hex, WatchlistAuth};
use plx_plex::plex::watchlist::WatchlistError;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Frames a refused spawn waits before the next try. A refused spawn never blocks a frame.
const RETRY_FRAMES: u32 = 120;

type Spawn = fn(&str, Box<dyn FnOnce() + Send + 'static>) -> bool;

fn spawn_worker(what: &str, f: Box<dyn FnOnce() + Send + 'static>) -> bool {
    plx_base::task::spawn_small(what, f)
}

/// One worker's answer, tagged with the request it answers.
struct Landing {
    req: u32,
    load: Load,
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

pub struct CatalogStore {
    /// The title the page is about, set by `open`.
    pub hex: Option<[u8; 12]>,
    pub load: Load,
    io: Arc<dyn CatalogIo>,
    spawn: Spawn,
    auth: Option<WatchlistAuth>,
    req: u32,
    /// The request is made but its worker has not started (a refused spawn).
    owed: bool,
    retry: u32,
    mailbox: Arc<Mailbox>,
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
            spawn: spawn_worker,
            auth: None,
            req: 0,
            owed: false,
            retry: 0,
            mailbox: Arc::new(Mailbox::default()),
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
    /// request, so an older one still in flight lands into nothing.
    pub fn open(&mut self, hex: [u8; 12]) {
        if self.auth.as_ref().is_none_or(|a| !self.io.is_current(a)) {
            self.auth = self.io.capture_auth();
        }
        self.hex = Some(hex);
        self.req = self.req.wrapping_add(1);
        self.retry = 0;
        self.load = Load::Loading;
        // A fresh mailbox: a retired request's landing never reaches the new one.
        self.mailbox = Arc::new(Mailbox::default());
        if self.auth.is_some() {
            self.owed = true;
            self.start();
        } else {
            // No account credential: nothing can be asked.
            self.owed = false;
            self.load = Load::Unreachable;
        }
        self.bump();
    }

    /// Sign-out or profile switch: forget the title, the identity and any request in flight.
    pub fn reset(&mut self) {
        self.hex = None;
        self.auth = None;
        self.req = self.req.wrapping_add(1);
        self.owed = false;
        self.load = Load::Loading;
        self.mailbox = Arc::new(Mailbox::default());
        self.bump();
    }

    fn start(&mut self) {
        let (Some(hex), Some(auth)) = (self.hex, self.auth.clone()) else {
            self.owed = false;
            return;
        };
        let (io, mailbox, req) = (Arc::clone(&self.io), Arc::clone(&self.mailbox), self.req);
        let job = move || {
            let load = match io.fetch(&auth, &encode_hex(&hex)) {
                Ok(item) => Load::Loaded(Box::new(record_of(&item, io.today().as_deref()))),
                Err(WatchlistError::NotFound) => Load::NotFound,
                // Offline, refused credential, unreadable body, any other status: nothing is stored; the page shows its unreachable state.
                Err(_) => Load::Unreachable,
            };
            mailbox.post(Landing { req, load });
        };
        if (self.spawn)("catalog-fetch", Box::new(job)) {
            self.owed = false;
        }
    }

    /// Frame pass: retry a refused spawn, then take the worker's landing. Returns whether `load`
    /// changed. A landing for a profile that is no longer seated is dropped and the page asks
    /// again under the new identity.
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
            if self.auth.as_ref().is_some_and(|a| !self.io.is_current(a)) {
                self.auth = None;
                if let Some(hex) = self.hex {
                    self.open(hex);
                }
                return true;
            }
            self.load = landing.load;
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

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_spawn_for_test(mut self, spawn: Spawn) -> Self {
        self.spawn = spawn;
        self
    }
}
