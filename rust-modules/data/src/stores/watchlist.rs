//! The physically owned Watchlist model and its worker adapter (plan §1.2, §1.4, §1.5, §1.11).
//!
//! Main-thread state plus an `Arc` mailbox, like the other owned stores. Three kinds of worker,
//! each `spawn_small`: the LIST worker (pages, released set, matching after each page), the
//! REMATCH worker (a full re-match after the roster moved) and the WRITE worker (one write at a
//! time, in press order). Every landing is tagged `{profile generation, store epoch, request id}`
//! and a landing for another profile or a rotated epoch is dropped. A profile switch rotates the
//! epoch: the rows, the match table and the intent ledger are cleared and the old workers land
//! into a retired mailbox.
//!
//! Membership is the default-order list from the service, with the intent ledger on top: a press
//! flips the row at once, a write that fails reverts it (no toast), and an intent beats any list
//! read issued before the write landed. Sort, type and Show filtering are client-side over the
//! stored rows; no request follows a Sort & Filter change.
//!
//! Watched marks and progress on matched tiles ride the existing view-state path: the store holds
//! `(held_keys)` for the rows in its hold window so the application owes their read-back, and a
//! `ViewChange` for a matched `(server, ratingKey)` patches the tile in place ([`WatchlistStore::apply_view`]).
//! `facts_gen` and the roster generation move a tile's state in one pass ([`WatchlistStore::pump_with_gate`]).

use crate::viewstate::{ViewChange, ViewEdit, ViewFact};
use crate::watchlist::ledger::{Intent, IntentLedger, PendingWrite, WriteOutcome};
use crate::watchlist::model::{tile_state, Asked, Kind, MatchTable, ServerMatch, TileState, WatchRow};
use crate::watchlist::order::{sorted_filtered, FilterSpec, SortSpec};
use crate::watchlist::{
    run_delta, run_list, run_rematch, run_write, Failure, Landed, LiveIo, Tag, What, WatchlistAuth, WatchlistInput,
    WatchlistIo,
};
use plx_machine::machine::{Cx, Effects, Handled, Host, Machine};
use plx_plex::plex::session::CurrentProfile;
use plx_plex::plex::{ListingFacts, ServerId};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use super::StoreEv;

/// Frames a refused spawn waits before the next try. A refused spawn never blocks a frame.
const RETRY_FRAMES: u32 = 120;

/// Every mutation of the watchlist store.
#[derive(Clone, Debug)]
pub enum WatchlistCmd {
    /// The page mounted (or the tab was entered): take the identity from the input if the epoch has none
    /// and read the list. It is stepped through [`WatchlistStore::run_with_input`]. A populated list is never blanked by it.
    Open,
    /// Read the list again behind the rows now held (resume, a write burst, a watched mark).
    Reread,
    /// A bookmark press (or the hold menu's Remove) on the item with this catalog id.
    Press { hex: [u8; 12], intent: Intent },
    Sort(SortSpec),
    Filter(FilterSpec),
    /// The indices (into the sorted, filtered order) the page holds cards for: visible plus margin.
    Window { wanted: Range<usize> },
    /// Sign-out or profile switch: a new epoch with nothing in it.
    Reset,
}

/// Where the list read stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Nothing asked yet.
    Idle,
    /// There is no account credential for this profile.
    NoAccount,
    Loading,
    Ready,
    /// The first read failed (a populated list is never replaced by this).
    Failed(Failure),
}

/// What a matched tile shows of its server copy's watch state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileFacts {
    pub watched: bool,
    pub resume_ms: i64,
    pub dur_ms: i64,
}

/// A worker's mailbox. Landings queue in post order; the pump drains them all.
#[derive(Default)]
pub struct WatchlistAdapter {
    mail: Mutex<VecDeque<Landed>>,
    list_busy: AtomicBool,
    write_busy: AtomicBool,
    /// Card rows asked to be resolved by `(server, rk)` (`WatchlistStore::want_hex`), spawned by the
    /// next pump. Behind a lock because a menu asks from `&self`.
    wanted: Mutex<Vec<(ServerId, String)>>,
    /// Resolved ids, landed by the resolve workers. The outer `None` is "the server did not
    /// answer": that attempt is forgotten, not memoized, so the next menu asks again.
    resolved: Mutex<Vec<(ServerId, String, Option<Option<[u8; 12]>>)>>,
}

impl WatchlistAdapter {
    fn post(&self, landed: Landed) {
        self.mail.lock().unwrap_or_else(|e| e.into_inner()).push_back(landed);
        plx_machine::idle::invalidate();
    }

    fn take_all(&self) -> Vec<Landed> {
        self.mail.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
    }
}

/// Clears a busy flag when the worker ends, however it ends (a panic included).
struct Release(Arc<WatchlistAdapter>, fn(&WatchlistAdapter) -> &AtomicBool);

impl Drop for Release {
    fn drop(&mut self) {
        (self.1)(&self.0).store(false, Ordering::SeqCst);
    }
}

type Spawn = fn(&str, Box<dyn FnOnce() + Send + 'static>) -> bool;

fn spawn_worker(what: &str, f: Box<dyn FnOnce() + Send + 'static>) -> bool {
    plx_base::task::spawn_small(what, f)
}

/// Main-thread state.
struct WatchlistState {
    epoch: u32,
    req: u32,
    auth: Option<WatchlistAuth>,
    status: Status,
    rows: Vec<WatchRow>,
    /// The next read's rows while it is still arriving behind a populated list.
    incoming: Option<Vec<WatchRow>>,
    /// The service's `totalSize` for the list, if it said.
    total: Option<u32>,
    /// A later page failed: the rows are the pages that arrived.
    truncated: bool,
    released: Option<HashSet<String>>,
    table: MatchTable,
    states: Vec<TileState>,
    facts: HashMap<(ServerId, i64), TileFacts>,
    facts_seen: u32,
    ledger: IntentLedger,
    /// The ledger stamp of the read whose rows are `rows`.
    rows_stamp: u64,
    /// The ledger stamp of the read now arriving.
    read_stamp: u64,
    sort: SortSpec,
    filter: FilterSpec,
    members: HashSet<[u8; 12]>,
    /// Catalog ids read off a card row's own server record, by `(server, rk)`; `None` is an
    /// answered "no portable id" (or no answer), not asked again this epoch.
    resolved: HashMap<(ServerId, String), Option<[u8; 12]>>,
    /// Asked and not yet answered.
    resolving: HashSet<(ServerId, String)>,
    index: Vec<usize>,
    window: Range<usize>,
    list_wanted: bool,
    reread_owed: bool,
    wrote: bool,
    list_retry: u32,
    write_retry: u32,
    /// Page 0's `ETag` from the last COMPLETE read of the list: sent as `If-None-Match` on the next
    /// one. `None` whenever the rows might not be the service's whole current list (nothing read,
    /// a write since, a cut-short read) and for the life of no epoch but this one.
    list_etag: Option<String>,
    /// Page 0's `ETag` of the read now arriving (or, on a 304, the validator it was answered for).
    read_etag: Option<String>,
    /// Writes spawned in this epoch. A read may store its validator only if no write was spawned
    /// while it was out ([`Self::read_writes`]).
    writes_seen: u32,
    /// `Some(writes_seen)` at the moment the read now arriving was spawned with no write queued or
    /// in flight, and still `Some` while no page of it has failed; `None` bars it from storing a
    /// validator.
    read_writes: Option<u32>,
}

impl WatchlistState {
    fn new(epoch: u32, sort: SortSpec, filter: FilterSpec) -> Self {
        Self {
            epoch,
            req: 0,
            auth: None,
            status: Status::Idle,
            rows: Vec::new(),
            incoming: None,
            total: None,
            truncated: false,
            released: None,
            table: MatchTable::default(),
            states: Vec::new(),
            facts: HashMap::new(),
            facts_seen: 0,
            ledger: IntentLedger::new(),
            rows_stamp: 0,
            read_stamp: 0,
            sort,
            filter,
            members: HashSet::new(),
            resolved: HashMap::new(),
            resolving: HashSet::new(),
            index: Vec::new(),
            window: 0..0,
            list_wanted: false,
            reread_owed: false,
            wrote: false,
            list_retry: 0,
            write_retry: 0,
            list_etag: None,
            read_etag: None,
            writes_seen: 0,
            read_writes: None,
        }
    }
}

pub struct WatchlistStore {
    state: WatchlistState,
    adapter: Arc<WatchlistAdapter>,
    io: Arc<dyn WatchlistIo>,
    spawn: Spawn,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for WatchlistStore {
    fn default() -> Self {
        Self::with_io(Arc::new(LiveIo))
    }
}

fn default_sort() -> SortSpec {
    SortSpec { key: crate::watchlist::order::SortKey::DateAdded, desc: true }
}

/// "On your servers": a copy is held on a Reachable server.
fn on_your_servers(state: TileState) -> bool {
    matches!(state, TileState::Available | TileState::ComingSoonMatched)
}

impl WatchlistStore {
    /// A store over a given I/O seam (tests script it; production is [`LiveIo`]).
    pub fn with_io(io: Arc<dyn WatchlistIo>) -> Self {
        Self {
            state: WatchlistState::new(0, default_sort(), FilterSpec::default()),
            adapter: Arc::new(WatchlistAdapter::default()),
            io,
            spawn: spawn_worker,
            notice_gen: AtomicU32::new(0),
            notice_dirty: AtomicBool::new(false),
        }
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

    /// What the page and the controls read.
    pub fn view(&self) -> WatchlistView<'_> {
        WatchlistView { store: self }
    }

    /// A re-read is owed (a write burst ended with a landed write): the application issues
    /// [`WatchlistCmd::Reread`] once, two seconds later. Taking clears it.
    pub fn take_reread_owed(&mut self) -> bool {
        std::mem::take(&mut self.state.reread_owed)
    }

    /// The `(server, ratingKey)` of every matched tile in the hold window (visible plus margin):
    /// what the application owes a read-back so a watched mark or progress made elsewhere reaches
    /// the tile through [`Self::apply_view`]. Only rows whose copy is on a Reachable server.
    pub fn held_keys(&self) -> super::HeldKeys {
        let mut keys = super::HeldKeys::new();
        let s = &self.state;
        let end = s.window.end.min(s.index.len());
        let start = s.window.start.min(end);
        for &ri in &s.index[start..end] {
            if on_your_servers(s.states[ri]) {
                if let Some(copy) = self.copy_of(ri) {
                    super::push_held(&mut keys, copy.server_id, &copy.rating_key.to_string());
                }
            }
        }
        keys
    }

    /// The copy a matched tile reads and opens (plan §1.5): a held copy on a Reachable server,
    /// the current server first, then an owned one, then a share; ties keep roster order.
    fn copy_of(&self, ri: usize) -> Option<ServerMatch> {
        self.held_copy(ri).map(|(m, _)| *m)
    }

    /// [`Self::copy_of`] with what its lookup row said of the title ([`ListingFacts`]): one choice
    /// of copy for the press that opens it and the caption that words it.
    fn held_copy(&self, ri: usize) -> Option<(&ServerMatch, &ListingFacts)> {
        let row = self.state.rows.get(ri)?;
        self.state
            .table
            .by_guid
            .get(&row.guid)?
            .iter()
            .filter_map(|(sid, asked)| match asked {
                Asked::Held(m, facts) if self.io.reachable(*sid) => Some((self.io.copy_rank(*sid), (m, facts))),
                _ => None,
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, held)| held)
    }

    /// The ViewChange patch: a watched/unwatched, `Facts` or `Offset` fact about a matched
    /// `(server, ratingKey)` updates that tile's watch state in place. Facts about other items are
    /// ignored. Returns whether a tile changed.
    pub fn apply_view(&mut self, change: &ViewChange) -> bool {
        let dur_ms = match change.fact {
            ViewFact::Offset { dur_ms, .. } => Some(dur_ms),
            _ => None,
        };
        self.apply_edit(change.sid, &change.rk, change.edit(), dur_ms)
    }

    /// [`Self::apply_view`] for a caller that holds the edit rather than the whole change (the
    /// optimistic press and the fan-out reach the stores as `ViewAt` + `ViewEdit`). `dur_ms` is the
    /// duration an Offset fact carried, when there is one.
    pub fn apply_edit(&mut self, sid: ServerId, rk: &str, edit: ViewEdit, dur_ms: Option<i64>) -> bool {
        let Ok(rk) = rk.parse::<i64>() else { return false };
        let matched = self.state.table.by_guid.values().flatten().any(|(s, a)| {
            matches!(a, Asked::Held(m, _) if *s == sid && m.rating_key == rk)
        });
        if !matched {
            return false;
        }
        let entry = self.state.facts.entry((sid, rk)).or_default();
        let before = *entry;
        match edit {
            ViewEdit::Watched(on) => {
                entry.watched = on;
                entry.resume_ms = 0;
            }
            ViewEdit::Facts { watched, resume_ms, .. } => {
                entry.watched = watched;
                entry.resume_ms = resume_ms;
            }
            ViewEdit::Offset { ms } => {
                entry.resume_ms = ms;
                if let Some(d) = dur_ms {
                    entry.dur_ms = d;
                }
            }
            ViewEdit::LeftDeck => {}
        }
        let changed = *entry != before;
        if changed {
            self.bump();
        }
        changed
    }

    /// Run one command for a caller that holds no [`WatchlistInput`]: every command but `Open`,
    /// which is the one that captures an identity or checks the seated profile. An `Open` is
    /// refused here, because the input it must read was never handed: it reaches
    /// [`WatchlistStore::run_with_input`].
    pub fn run(&mut self, cmd: WatchlistCmd) -> bool {
        if matches!(cmd, WatchlistCmd::Open) {
            debug_assert!(false, "an Open reached a step handed no input");
            plx_base::eventlog::log("watchlist: an Open reached a step handed no input - dropped");
            return false;
        }
        self.run_step(cmd, None)
    }

    /// Run one command; `input` is what the Bridge read for this call (`Bridge::watchlist_input`),
    /// which an `Open` captures from and checks the held identity against.
    pub fn run_with_input(&mut self, cmd: WatchlistCmd, input: &WatchlistInput) -> bool {
        self.run_step(cmd, Some(input))
    }

    fn run_step(&mut self, cmd: WatchlistCmd, input: Option<&WatchlistInput>) -> bool {
        let changed = match cmd {
            WatchlistCmd::Open => input.is_some_and(|input| self.open(input)),
            WatchlistCmd::Reread => {
                if self.state.auth.is_some() {
                    self.state.list_wanted = true;
                }
                false
            }
            WatchlistCmd::Press { hex, intent } => {
                self.state.ledger.press(hex, intent);
                self.refresh();
                true
            }
            WatchlistCmd::Sort(sort) => {
                let changed = self.state.sort != sort;
                self.state.sort = sort;
                self.refresh();
                changed
            }
            WatchlistCmd::Filter(filter) => {
                let changed = self.state.filter != filter;
                self.state.filter = filter;
                self.refresh();
                changed
            }
            WatchlistCmd::Window { wanted } => {
                self.state.window = wanted;
                false
            }
            WatchlistCmd::Reset => {
                self.reset();
                true
            }
        };
        if changed {
            self.bump();
        }
        changed
    }

    fn open(&mut self, input: &WatchlistInput) -> bool {
        let stale = self.state.auth.as_ref().is_none_or(|a| !a.is_current(input.profile()));
        if stale {
            if self.state.auth.is_some() {
                self.reset();
            }
            self.state.auth = input.auth().cloned();
        }
        let Some(auth) = &self.state.auth else {
            let was = self.state.status;
            self.state.status = Status::NoAccount;
            return was != Status::NoAccount;
        };
        let _ = auth;
        self.state.list_wanted = true;
        if self.state.rows.is_empty() && self.state.status != Status::Loading {
            self.state.status = Status::Loading;
            return true;
        }
        false
    }

    /// A new epoch with nothing in it. The old workers land into a retired mailbox; their writes
    /// that were still queued are dropped with the old ledger (never sent to another profile).
    fn reset(&mut self) {
        let epoch = self.state.epoch.wrapping_add(1);
        let (sort, filter) = (self.state.sort, self.state.filter);
        self.adapter = Arc::new(WatchlistAdapter::default());
        self.state = WatchlistState::new(epoch, sort, filter);
    }

    /// Recompute what is derived from rows, ledger and view: membership, then the sorted,
    /// filtered order. At commit and on a press, never per frame.
    fn refresh(&mut self) {
        let s = &mut self.state;
        let mut members: HashSet<[u8; 12]> = s.rows.iter().map(|r| r.hex).collect();
        for (hex, intent) in s.ledger.overridden(s.rows_stamp) {
            match intent {
                Intent::Add => members.insert(hex),
                Intent::Remove => members.remove(&hex),
            };
        }
        let states = &s.states;
        let mut index = sorted_filtered(&s.rows, &s.sort, &s.filter, &|i| {
            states.get(i).copied().is_some_and(on_your_servers)
        });
        index.retain(|&i| members.contains(&s.rows[i].hex));
        s.members = members;
        s.index = index;
    }

    /// Derive every row's tile state again (commit, `facts_gen`, a reachability flip).
    fn rederive(&mut self) {
        let s = &mut self.state;
        let io = &*self.io;
        // Released read failed and the row's date cannot say (none, or no clock): its release is
        // unknown, so a row every server answered absent stays Unknown (plan §1.3), not washed.
        let release_blind = s.released.is_none();
        let today = io.today();
        s.states = s
            .rows
            .iter()
            .map(|r| match tile_state(r, &s.table, io) {
                TileState::NotOnAnyServer if release_blind && (r.release.is_none() || today.is_none()) => {
                    TileState::Unknown
                }
                state => state,
            })
            .collect();
        s.facts_seen = io.facts_gen();
        self.refresh();
    }

    /// Settle a row's `released` against the released set; when that read failed, against today.
    fn settle_released(&self, rows: &mut [WatchRow]) {
        let today = self.io.today();
        for r in rows {
            r.released = match (&self.state.released, &today, &r.release) {
                (Some(set), ..) => set.contains(&r.guid),
                (None, Some(today), Some(date)) => date.as_str() <= today.as_str(),
                _ => true,
            };
        }
    }

    fn merge(&mut self, answers: Vec<crate::watchlist::Answer>, roster_gen: u32) {
        self.merge_answers(answers);
        self.state.table.roster_gen = roster_gen;
    }

    /// Lay answers over the table without saying anything about the roster they were asked under.
    fn merge_answers(&mut self, answers: Vec<crate::watchlist::Answer>) {
        for (guid, sid, asked) in answers {
            let entry = self.state.table.by_guid.entry(guid).or_default();
            match entry.iter_mut().find(|(s, _)| *s == sid) {
                Some(slot) => slot.1 = asked,
                None => entry.push((sid, asked)),
            }
        }
    }

    fn current_tag(&self, auth: &WatchlistAuth) -> Tag {
        Tag { profile_gen: auth.generation, epoch: self.state.epoch, req: self.state.req }
    }

    fn apply(&mut self, landed: Landed) -> bool {
        let Some(auth) = &self.state.auth else { return false };
        if landed.tag.profile_gen != auth.generation || landed.tag.epoch != self.state.epoch {
            return false;
        }
        match landed.what {
            What::Page { start, total, mut rows, answers, roster_gen, etag } => {
                self.state.total = total;
                if start == 0 {
                    // One line per list read, never the URL or a title: the only on-device
                    // evidence of whether the page-0 validator saved a read (see the 304 arm).
                    plx_base::eventlog::log(&format!(
                        "watchlist: list page 0 -> full read (etag={})",
                        if etag.is_some() { "yes" } else { "no" }
                    ));
                    self.state.read_etag = etag;
                }
                self.settle_released(&mut rows);
                self.merge(answers, roster_gen);
                if start == 0 {
                    self.state.truncated = false;
                    if self.state.rows.is_empty() {
                        // First load: the page shows as soon as it lands.
                        self.state.rows = rows;
                        self.state.rows_stamp = self.state.read_stamp;
                        self.state.status = Status::Ready;
                    } else {
                        self.state.incoming = Some(rows);
                    }
                } else if let Some(incoming) = self.state.incoming.as_mut() {
                    append_new(incoming, rows);
                } else {
                    append_new(&mut self.state.rows, rows);
                }
                self.rederive();
                true
            }
            What::NotModified => {
                // The held rows ARE the service's list as of this read: the same commit a whole
                // read makes at its end, with nothing to swap in.
                plx_base::eventlog::log("watchlist: list page 0 -> 304 not modified (held rows kept)");
                self.state.truncated = false;
                self.state.rows_stamp = self.state.read_stamp;
                self.state.status = Status::Ready;
                self.state.read_etag = self.state.list_etag.clone();
                self.rederive();
                true
            }
            What::PageFailed { start, why } => {
                self.state.read_writes = None;
                let had_rows = self.state.incoming.take().is_some();
                if start == 0 && self.state.rows.is_empty() {
                    self.state.status = Status::Failed(why);
                } else {
                    // A reread's later page failing keeps the old complete rows: only a list
                    // that is itself being built from pages is cut short.
                    self.state.truncated = start > 0 && !had_rows;
                }
                true
            }
            What::Released { guids } => {
                self.state.released = guids;
                let mut rows = std::mem::take(&mut self.state.rows);
                self.settle_released(&mut rows);
                self.state.rows = rows;
                if let Some(mut incoming) = self.state.incoming.take() {
                    self.settle_released(&mut incoming);
                    self.state.incoming = Some(incoming);
                }
                self.rederive();
                true
            }
            What::ListDone => {
                if self.state.read_writes.take() == Some(self.state.writes_seen) {
                    self.state.list_etag = self.state.read_etag.take();
                }
                self.state.read_etag = None;
                if let Some(mut rows) = self.state.incoming.take() {
                    self.settle_released(&mut rows);
                    self.state.rows = rows;
                    self.state.rows_stamp = self.state.read_stamp;
                    self.state.status = Status::Ready;
                }
                let stamp = self.state.rows_stamp;
                self.state.ledger.forget_older(stamp);
                self.rederive();
                true
            }
            What::Rematch { answers, roster_gen } => {
                self.state.table.by_guid.clear();
                self.merge(answers, roster_gen);
                self.rederive();
                true
            }
            What::Delta { answers } => {
                self.merge_answers(answers);
                self.rederive();
                true
            }
            What::Written { write, outcome } => {
                if outcome == WriteOutcome::Failed {
                    // No toast: the flip reverts. The log carries the verb only, never an id.
                    plx_base::eventlog::log(&format!("watchlist: {:?} write failed, reverted", write.intent));
                } else {
                    self.state.wrote = true;
                }
                self.state.ledger.finished(write, outcome);
                if !self.state.ledger.busy() && std::mem::take(&mut self.state.wrote) {
                    self.state.reread_owed = true;
                }
                self.refresh();
                true
            }
        }
    }

    /// Route-unconditional landing pass: land what arrived, notice a profile switch, follow
    /// `facts_gen`, then start whatever work is owed. MAIN THREAD, once a frame.
    /// `profile` is the one the Bridge read for this frame (`Bridge::seated_profile`): a pass never
    /// captures an identity (a reset leaves none, and the next `Open` captures), so it needs no more.
    pub fn pump_with_gate(&mut self, gate: &plx_machine::landgate::Gate, profile: &CurrentProfile) -> bool {
        let mut changed = false;
        if self.state.auth.as_ref().is_some_and(|a| !a.is_current(profile)) {
            self.reset();
            changed = true;
        }
        self.state.list_retry = self.state.list_retry.saturating_sub(1);
        self.state.write_retry = self.state.write_retry.saturating_sub(1);
        let adapter = Arc::clone(&self.adapter);
        // In the frame dump's landing gate this waits until no list or write worker is out, so a
        // landing is a function of the frame that spawned the work. The flags are read BEFORE the
        // drain: a worker posts, then clears its flag, so a flag read clear means its mail is in.
        let was_idle = std::cell::Cell::new(true);
        let landings = super::take_landings_owed(gate, super::StoreId::Watchlist, |_| !was_idle.get(), || {
            was_idle.set(!adapter.list_busy.load(Ordering::SeqCst) && !adapter.write_busy.load(Ordering::SeqCst));
            adapter.take_all()
        });
        for landed in landings {
            plx_machine::idle::invalidate();
            changed |= self.apply(landed);
        }
        if self.state.auth.is_some() && self.io.facts_gen() != self.state.facts_seen {
            self.rederive();
            changed = true;
        }
        changed |= self.pump_resolves(gate);
        self.maybe_spawn();
        if changed {
            self.bump();
        }
        changed
    }

    fn maybe_spawn(&mut self) {
        let Some(auth) = self.state.auth.clone() else { return };
        // The write worker: one write at a time, in press order.
        if !self.adapter.write_busy.load(Ordering::SeqCst) && self.state.write_retry == 0 {
            if let Some(write) = self.state.ledger.next_write() {
                self.spawn_write(&auth, write);
            }
        }
        if self.adapter.list_busy.load(Ordering::SeqCst) || self.state.list_retry > 0 {
            return;
        }
        if self.state.list_wanted {
            self.spawn_list(&auth);
        } else if !self.state.rows.is_empty()
            && self.io.roster_gen() != self.state.table.roster_gen
        {
            self.spawn_rematch(&auth);
        }
    }

    fn spawn_write(&mut self, auth: &WatchlistAuth, write: PendingWrite) {
        self.state.req += 1;
        // Whatever this write does, the service's list is no longer the one the validator names
        // (even a failed request may have reached it): the next read must be whole.
        self.state.writes_seen = self.state.writes_seen.wrapping_add(1);
        self.state.list_etag = None;
        let tag = self.current_tag(auth);
        let (adapter, io, auth) = (Arc::clone(&self.adapter), Arc::clone(&self.io), auth.clone());
        adapter.write_busy.store(true, Ordering::SeqCst);
        let guard = Release(Arc::clone(&adapter), |a| &a.write_busy);
        let spawned = (self.spawn)("wlist-write", Box::new(move || {
            let _guard = guard;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_write(&*io, &auth, tag, write, &mut |l| adapter.post(l));
            }));
            if outcome.is_err() {
                adapter.post(Landed { tag, what: What::Written { write, outcome: WriteOutcome::Failed } });
            }
        }));
        if !spawned {
            // Nothing will run: hand the write back and try again in a while.
            self.adapter.write_busy.store(false, Ordering::SeqCst);
            self.state.ledger.requeue(write);
            self.state.write_retry = RETRY_FRAMES;
        }
    }

    /// The guids the table already holds an answer for from every reachable server, so a reread
    /// matches only the new ones (the worker ignores this when the roster has moved since).
    fn matched_guids(&self) -> HashSet<String> {
        let asked: Vec<ServerId> = self.io.roster().into_iter().filter(|s| self.io.reachable(*s)).collect();
        self.state
            .table
            .by_guid
            .iter()
            .filter(|(_, answers)| asked.iter().all(|s| answers.iter().any(|(a, _)| a == s)))
            .map(|(g, _)| g.clone())
            .collect()
    }

    fn spawn_list(&mut self, auth: &WatchlistAuth) {
        self.state.req += 1;
        let tag = self.current_tag(auth);
        self.state.read_stamp = self.state.ledger.stamp();
        let known = self.matched_guids();
        let known_gen = self.state.table.roster_gen;
        // The validator only stands for a complete list with no write in the air.
        let quiet = !self.state.ledger.busy();
        let etag = self.state.list_etag.clone().filter(|_| quiet && !self.state.rows.is_empty() && !self.state.truncated);
        self.state.read_etag = None;
        self.state.read_writes = quiet.then_some(self.state.writes_seen);
        let (adapter, io, auth) = (Arc::clone(&self.adapter), Arc::clone(&self.io), auth.clone());
        adapter.list_busy.store(true, Ordering::SeqCst);
        let guard = Release(Arc::clone(&adapter), |a| &a.list_busy);
        let spawned = (self.spawn)("wlist", Box::new(move || {
            let _guard = guard;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_list(&*io, &auth, tag, (&known, known_gen), etag.as_deref(), &mut |l| adapter.post(l));
            }));
            if outcome.is_err() {
                adapter.post(Landed { tag, what: What::PageFailed { start: 0, why: Failure::Other } });
                adapter.post(Landed { tag, what: What::ListDone });
            }
        }));
        if spawned {
            self.state.list_wanted = false;
        } else {
            self.adapter.list_busy.store(false, Ordering::SeqCst);
            self.state.list_retry = RETRY_FRAMES;
        }
    }

    fn spawn_rematch(&mut self, auth: &WatchlistAuth) {
        self.state.req += 1;
        let tag = self.current_tag(auth);
        let keys: Vec<(String, Kind)> = self.state.rows.iter().map(|r| (r.guid.clone(), r.kind)).collect();
        let (adapter, io) = (Arc::clone(&self.adapter), Arc::clone(&self.io));
        adapter.list_busy.store(true, Ordering::SeqCst);
        let guard = Release(Arc::clone(&adapter), |a| &a.list_busy);
        let spawned = (self.spawn)("wlist", Box::new(move || {
            let _guard = guard;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_rematch(&*io, &keys, tag, &mut |l| adapter.post(l));
            }));
            if outcome.is_err() {
                // Keep the table as it is: no landing, the roster check asks again.
                plx_base::eventlog::log("watchlist: rematch worker panicked");
            }
        }));
        if !spawned {
            self.adapter.list_busy.store(false, Ordering::SeqCst);
            self.state.list_retry = RETRY_FRAMES;
        }
    }

    /// Has the list been read and shown? A library change is followed only once there are rows to
    /// follow: before that, the first read matches every title against the library as it is then.
    pub fn is_ready(&self) -> bool {
        self.state.auth.is_some() && self.state.status == Status::Ready
    }

    /// Is a list read out on a worker right now? Its matching may have asked a server before a
    /// library change that has just arrived, so a change must be kept for it; a read not yet
    /// started will ask after the change and needs no note of it.
    pub fn read_in_flight(&self) -> bool {
        self.adapter.list_busy.load(Ordering::SeqCst)
    }

    /// The rows `sid` holds, by that server's ratingKey: what a library item deleted there is
    /// looked up in (built once for a whole frame's worth of deletes).
    pub fn held_by_rk(&self, sid: ServerId) -> HashMap<i64, (String, Kind)> {
        let mut out = HashMap::new();
        for r in &self.state.rows {
            let Some(answers) = self.state.table.by_guid.get(&r.guid) else { continue };
            for (s, asked) in answers {
                if let (true, Asked::Held(m, _)) = (*s == sid, asked) {
                    out.insert(m.rating_key, (r.guid.clone(), r.kind));
                }
            }
        }
        out
    }

    /// Every row of `kind` that `sid` has not been seen to hold (absent, or never answered): what
    /// a title added to its library might now satisfy.
    pub fn unmatched_on(&self, sid: ServerId, kind: Kind) -> Vec<(String, Kind)> {
        let held = |guid: &str| {
            self.state.table.by_guid.get(guid).is_some_and(|answers| {
                answers.iter().any(|(s, asked)| *s == sid && matches!(asked, Asked::Held(..)))
            })
        };
        self.state.rows.iter().filter(|r| r.kind == kind && !held(&r.guid)).map(|r| (r.guid.clone(), r.kind)).collect()
    }

    /// Every row of `kind`, for a sweep when the exact change was not followed.
    pub fn rows_of(&self, kind: Kind) -> Vec<(String, Kind)> {
        self.state.rows.iter().filter(|r| r.kind == kind).map(|r| (r.guid.clone(), r.kind)).collect()
    }

    /// Ask `sid` again about `asks` on a worker; the answers land over the match table and the
    /// tiles re-derive at that commit. `false` = not started (no list yet, a read or re-match
    /// already out, or the worker was refused): the caller keeps the ask and tries a later frame.
    /// Nothing to ask, or a server that is not reachable, is `true`: there is nothing to start.
    pub fn rematch_on(&mut self, sid: ServerId, asks: Vec<(String, Kind)>) -> bool {
        if !self.is_ready() {
            return false;
        }
        if asks.is_empty() || !self.io.reachable(sid) {
            return true;
        }
        let Some(auth) = self.state.auth.clone() else { return false };
        if self.state.list_wanted
            || self.adapter.list_busy.load(Ordering::SeqCst)
            || self.state.list_retry > 0
        {
            return false;
        }
        self.state.req += 1;
        let tag = self.current_tag(&auth);
        let (adapter, io) = (Arc::clone(&self.adapter), Arc::clone(&self.io));
        adapter.list_busy.store(true, Ordering::SeqCst);
        let guard = Release(Arc::clone(&adapter), |a| &a.list_busy);
        let spawned = (self.spawn)("wlist", Box::new(move || {
            let _guard = guard;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_delta(&*io, sid, &asks, tag, &mut |l| adapter.post(l));
            }));
            if outcome.is_err() {
                plx_base::eventlog::log("watchlist: library re-match worker panicked");
            }
        }));
        if !spawned {
            self.adapter.list_busy.store(false, Ordering::SeqCst);
            self.state.list_retry = RETRY_FRAMES;
        }
        spawned
    }

    /// A card row carried no catalog id: ask for it to be read off the item's own server record
    /// (`client.metadata(rk).guid`, on a worker, at the next pump). Idempotent per `(server, rk)`.
    /// The answer is [`WatchlistView::resolved_hex`]; a menu opened after it lands offers the row.
    pub fn want_hex(&self, sid: ServerId, rk: &str) {
        let key = (sid, rk.to_owned());
        if self.state.resolved.contains_key(&key) || self.state.resolving.contains(&key) {
            return;
        }
        let mut wanted = self.adapter.wanted.lock().unwrap_or_else(|e| e.into_inner());
        if !wanted.contains(&key) {
            wanted.push(key);
        }
    }

    /// Spawn a worker per asked-for record and land the answers. Needs no account: the read is
    /// the user's own server's.
    fn pump_resolves(&mut self, gate: &plx_machine::landgate::Gate) -> bool {
        let mut changed = false;
        // Every claim in `resolving` owes one answer (a refused spawn removes its claim at once),
        // which the frame dump's landing gate waits for.
        let (adapter, claimed) = (&self.adapter, self.state.resolving.len());
        let answers = super::take_landings_owed(gate, super::StoreId::Watchlist, |got| got.len() < claimed, || {
            adapter.resolved.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
        });
        for (sid, rk, hex) in answers {
            self.state.resolving.remove(&(sid, rk.clone()));
            if let Some(hex) = hex {
                self.state.resolved.insert((sid, rk), hex);
                changed = true;
            }
        }
        let wanted: Vec<_> = self.adapter.wanted.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        for (sid, rk) in wanted {
            if self.state.resolved.contains_key(&(sid, rk.clone())) || !self.state.resolving.insert((sid, rk.clone())) {
                continue;
            }
            let (adapter, io) = (Arc::clone(&self.adapter), Arc::clone(&self.io));
            let key = rk.clone();
            let spawned = (self.spawn)("wlist-resolve", Box::new(move || {
                let hex = io.guid_of(sid, &key).map(|g| plx_plex::plex::resolve_catalog_hex(None, || Some(g)));
                adapter.resolved.lock().unwrap_or_else(|e| e.into_inner()).push((sid, key, hex));
                plx_machine::idle::invalidate();
            }));
            if !spawned {
                // Nothing will run: forget it so the next menu asks again.
                self.state.resolving.remove(&(sid, rk));
            }
        }
        changed
    }

    /// [`Self::pump_with_gate`] for a test about something else: the fixture gate, and the seat
    /// [`WatchlistInput::for_test`] hands out at generation 1 (the one a store's tests open under).
    #[cfg(any(test, feature = "test-support"))]
    pub fn pump(&mut self) -> bool {
        self.pump_with_gate(plx_machine::landgate::fixture_gate(), &WatchlistInput::profile_for_test(1))
    }

    /// [`Self::run_with_input`] for a test about something else: the same generation-1 seat.
    #[cfg(any(test, feature = "test-support"))]
    pub fn run_for_test(&mut self, cmd: WatchlistCmd) -> bool {
        self.run_with_input(cmd, &WatchlistInput::for_test(1))
    }

    /// Run the workers through `spawn` (an inline runner makes a landing deterministic).
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_spawn_for_test(mut self, spawn: fn(&str, Box<dyn FnOnce() + Send + 'static>) -> bool) -> Self {
        self.spawn = spawn;
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn adapter_for_test(&self) -> Arc<WatchlistAdapter> {
        Arc::clone(&self.adapter)
    }
}

/// Append the rows of a later page that the list does not hold yet.
fn append_new(into: &mut Vec<WatchRow>, rows: Vec<WatchRow>) {
    let seen: HashSet<[u8; 12]> = into.iter().map(|r| r.hex).collect();
    into.extend(rows.into_iter().filter(|r| !seen.contains(&r.hex)));
}

#[cfg(any(test, feature = "test-support"))]
impl WatchlistAdapter {
    pub fn post_for_test(&self, landed: Landed) {
        self.post(landed);
    }
}

/// What the page reads: the sorted, filtered order over the stored rows.
pub struct WatchlistView<'a> {
    store: &'a WatchlistStore,
}

impl WatchlistView<'_> {
    pub fn status(&self) -> Status {
        self.store.state.status
    }

    /// Rows shown, after the filter and the optimistic removes.
    pub fn len(&self) -> usize {
        self.store.state.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.state.index.is_empty()
    }

    /// The `i`-th row in the sorted, filtered order.
    pub fn row(&self, i: usize) -> Option<&WatchRow> {
        self.store.state.rows.get(*self.store.state.index.get(i)?)
    }

    pub fn state(&self, i: usize) -> Option<TileState> {
        self.store.state.states.get(*self.store.state.index.get(i)?).copied()
    }

    /// The matched copy's watch state, once a read-back or a ViewChange has told the store.
    pub fn facts(&self, i: usize) -> Option<TileFacts> {
        let copy = self.store.copy_of(*self.store.state.index.get(i)?)?;
        self.store.state.facts.get(&(copy.server_id, copy.rating_key)).copied()
    }

    /// The matched copy a tile opens, if any.
    pub fn copy(&self, i: usize) -> Option<ServerMatch> {
        self.store.copy_of(*self.store.state.index.get(i)?)
    }

    /// What the matched copy's lookup row says of the title (genre, video, ratings, length, counts):
    /// the facts the focused caption words, with the copy [`Self::copy`] opens.
    pub fn listing(&self, i: usize) -> Option<&ListingFacts> {
        self.store.held_copy(*self.store.state.index.get(i)?).map(|(_, facts)| facts)
    }

    /// The committed tile state of the title with catalog id `hex`, over EVERY row (the options
    /// narrowing the page do not hide it from the catalog page). `None` when the title is not a
    /// row of the list read.
    pub fn tile_state_of(&self, hex: &[u8; 12]) -> Option<TileState> {
        let s = &self.store.state;
        let i = s.rows.iter().position(|r| r.hex == *hex)?;
        s.states.get(i).copied()
    }

    /// The catalog id read off a card row's own server record: `None` not asked or not answered
    /// yet, `Some(None)` answered with no portable id. See [`WatchlistStore::want_hex`].
    pub fn resolved_hex(&self, sid: ServerId, rk: &str) -> Option<Option<[u8; 12]>> {
        self.store.state.resolved.get(&(sid, rk.to_owned())).copied()
    }

    /// Is this catalog id on the watchlist, intents in flight included?
    pub fn is_member(&self, hex: &[u8; 12]) -> bool {
        self.store.state.members.contains(hex)
    }

    /// Titles on the list, before the options narrow it and with the presses in flight counted:
    /// what separates "your watchlist is empty" from "nothing matches".
    pub fn list_len(&self) -> usize {
        self.store.state.members.len()
    }

    /// The server whose photo proxy fetches Discover art for titles no server holds (plan §0, §3):
    /// the reachable one `copy_rank` prefers, ties in roster order. `None` when no server is
    /// reachable; those titles then draw the skeleton (Q9).
    pub fn art_server(&self) -> Option<ServerId> {
        let io = &*self.store.io;
        io.roster().into_iter().filter(|s| io.reachable(*s)).min_by_key(|s| io.copy_rank(*s))
    }

    /// The service's list length, once it said, for the skeleton cells past the loaded tail.
    pub fn total(&self) -> Option<u32> {
        self.store.state.total
    }

    /// Titles the service counted that no page has delivered yet (0 once a later page failed:
    /// that tail is not coming), for the count beside the title and the skeleton tail.
    pub fn unloaded(&self) -> usize {
        let s = &self.store.state;
        if s.truncated {
            return 0;
        }
        s.total.map_or(0, |t| (t as usize).saturating_sub(s.rows.len()))
    }

    /// A later page failed: the rows are only the pages that arrived.
    pub fn truncated(&self) -> bool {
        self.store.state.truncated
    }

    pub fn sort(&self) -> SortSpec {
        self.store.state.sort
    }

    pub fn filter(&self) -> FilterSpec {
        self.store.state.filter
    }
}

impl<H: Host> Machine<H> for WatchlistStore {
    type Ev = StoreEv<WatchlistCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(cmd) => {
                self.run(cmd.clone());
            }
            StoreEv::Pump { .. } => {
                // Unreached: nothing sends `StoreEv::Pump`. A step reads no registry, and a pass needs
                // the seated profile (`Bridge::watchlist_pump` reads it and calls `pump_with_gate`):
                // a pump with none handed would read as a sign-out. A sender must pass the host's profile.
                debug_assert!(false, "a pump reached a step handed no profile");
                plx_base::eventlog::log("watchlist: a pump reached a step handed no profile - dropped");
            }
        }
        Handled::Yes
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::viewstate::Scope;
    use crate::watchlist::model::Reach;
    use crate::watchlist::order::{Show, SortKey};
    use plx_plex::plex::watchlist::{WatchItem, WatchKind, WatchPage, WatchlistError};
    use plx_plex::plex::{GuidKind, GuidLookupError, GuidMatch};
    use std::sync::atomic::AtomicUsize;

    fn hex_of(n: u8) -> [u8; 12] {
        [n; 12]
    }

    fn id(n: u8) -> String {
        crate::watchlist::encode_hex(&hex_of(n))
    }

    fn guid(n: u8) -> String {
        format!("plex://movie/{}", id(n))
    }

    fn item(n: u8, title: &str) -> WatchItem {
        WatchItem {
            hex: id(n),
            guid: guid(n),
            kind: Some(WatchKind::Movie),
            title: Some(title.into()),
            added_at: Some(1_000 - i64::from(n)),
            ..Default::default()
        }
    }

    /// A scripted world: the service's list, the servers and what they hold.
    struct Mock {
        items: Mutex<Vec<WatchItem>>,
        released: Mutex<Option<Vec<WatchItem>>>,
        roster: Mutex<Vec<ServerId>>,
        up: Mutex<Vec<ServerId>>,
        /// (server, guid) to ratingKey.
        held: Mutex<HashMap<(ServerId, String), String>>,
        writes: Mutex<Vec<(String, Intent)>>,
        fail_writes: AtomicBool,
        roster_gen: AtomicU32,
        facts_gen: AtomicU32,
        finds: AtomicUsize,
        /// The guids of every `find`, in call order.
        found: Mutex<Vec<Vec<String>>>,
        pages: AtomicUsize,
        /// Pages starting at or after this offset fail.
        fail_pages_from: AtomicU32,
        /// ...and fail as a READ the dev-token gate refused (`WatchlistError::Refused`) rather than
        /// as a transport error.
        fail_refused: AtomicBool,
        /// Copy preference per server (0 current, 1 owned, 2 share); absent = 2.
        ranks: Mutex<HashMap<ServerId, u8>>,
        /// What each server's lookup row says of a held title for a caption; absent = nothing.
        listings: Mutex<HashMap<ServerId, ListingFacts>>,
        /// Page starts requested, in order.
        starts: Mutex<Vec<u32>>,
        /// Entries per page the decoder "dropped": counted in `raw`, absent from `items`; also
        /// withholds `totalSize` while non-zero.
        dropped: AtomicU32,
        /// Scripted `guid_of` answers, front first; empty = no answer.
        guid_answers: Mutex<Vec<Option<String>>>,
        /// Does the service send an `ETag`? When it does, the validator names the list's content,
        /// so any change to `items` is a new one.
        etag_on: AtomicBool,
        /// Every page request as `(start, If-None-Match)`, 304s included.
        asked: Mutex<Vec<(u32, Option<String>)>>,
    }

    impl Mock {
        /// The list validator as of now: a hash of the ids in order, weak-tagged like the service's.
        fn tag(&self) -> Option<String> {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            self.items.lock().unwrap().iter().for_each(|i| i.hex.hash(&mut h));
            self.etag_on.load(Ordering::SeqCst).then(|| format!("W/\"{:x}\"", h.finish()))
        }

        fn new(items: Vec<WatchItem>) -> Arc<Mock> {
            let a = ServerId::from_raw(0);
            Arc::new(Mock {
                items: Mutex::new(items),
                released: Mutex::new(None),
                roster: Mutex::new(vec![a]),
                up: Mutex::new(vec![a]),
                held: Mutex::new(HashMap::new()),
                writes: Mutex::new(Vec::new()),
                fail_writes: AtomicBool::new(false),
                roster_gen: AtomicU32::new(1),
                facts_gen: AtomicU32::new(1),
                finds: AtomicUsize::new(0),
                found: Mutex::new(Vec::new()),
                pages: AtomicUsize::new(0),
                fail_pages_from: AtomicU32::new(u32::MAX),
                fail_refused: AtomicBool::new(false),
                ranks: Mutex::new(HashMap::new()),
                listings: Mutex::new(HashMap::new()),
                starts: Mutex::new(Vec::new()),
                dropped: AtomicU32::new(0),
                guid_answers: Mutex::new(Vec::new()),
                etag_on: AtomicBool::new(true),
                asked: Mutex::new(Vec::new()),
            })
        }
    }

    impl Reach for Mock {
        fn reachable(&self, s: ServerId) -> bool {
            self.up.lock().unwrap().contains(&s)
        }
        fn roster(&self) -> Vec<ServerId> {
            self.roster.lock().unwrap().clone()
        }
        fn copy_rank(&self, s: ServerId) -> u8 {
            self.ranks.lock().unwrap().get(&s).copied().unwrap_or(2)
        }
    }

    impl WatchlistIo for Mock {
        fn page(&self, _: &WatchlistAuth, start: u32, size: u32, if_none_match: Option<&str>)
            -> Result<WatchPage, WatchlistError>
        {
            self.pages.fetch_add(1, Ordering::SeqCst);
            self.asked.lock().unwrap().push((start, if_none_match.map(str::to_owned)));
            let etag = self.tag();
            if start == 0 && if_none_match.is_some() && if_none_match == etag.as_deref() {
                return Err(WatchlistError::NotModified);
            }
            if start >= self.fail_pages_from.load(Ordering::SeqCst) {
                return Err(if self.fail_refused.load(Ordering::SeqCst) {
                    WatchlistError::Refused
                } else {
                    WatchlistError::Transport
                });
            }
            let all = self.items.lock().unwrap();
            self.starts.lock().unwrap().push(start);
            let mut items: Vec<WatchItem> = all.iter().skip(start as usize).take(size as usize).cloned().collect();
            let raw = items.len() as u32;
            let dropped = self.dropped.load(Ordering::SeqCst) as usize;
            let total = (dropped == 0).then_some(all.len() as u32);
            items.truncate(items.len().saturating_sub(dropped));
            Ok(WatchPage { items, total, start, raw, etag: etag.filter(|_| start == 0) })
        }
        fn released(&self, _: &WatchlistAuth) -> Result<Vec<WatchItem>, WatchlistError> {
            self.released.lock().unwrap().clone().ok_or(WatchlistError::Transport)
        }
        fn write(&self, _: &WatchlistAuth, hex: &str, intent: Intent) -> Result<(), WatchlistError> {
            self.writes.lock().unwrap().push((hex.to_owned(), intent));
            if self.fail_writes.load(Ordering::SeqCst) { Err(WatchlistError::Transport) } else { Ok(()) }
        }
        fn find(&self, server: ServerId, _: GuidKind, guids: &[String]) -> Result<Vec<GuidMatch>, GuidLookupError> {
            self.finds.fetch_add(1, Ordering::SeqCst);
            self.found.lock().unwrap().push(guids.to_vec());
            let held = self.held.lock().unwrap();
            Ok(guids
                .iter()
                .filter_map(|g| {
                    held.get(&(server, g.clone()))
                        .map(|rk| GuidMatch { guid: g.clone(), rating_key: rk.clone(), library_section_id: None, blur: None,
                            facts: self.listings.lock().unwrap().get(&server).cloned().unwrap_or_default() })
                })
                .collect())
        }
        fn roster_gen(&self) -> u32 {
            self.roster_gen.load(Ordering::SeqCst)
        }
        fn facts_gen(&self) -> u32 {
            self.facts_gen.load(Ordering::SeqCst)
        }
        fn today(&self) -> Option<String> {
            Some("2026-10-15".into())
        }
        fn guid_of(&self, _: ServerId, _: &str) -> Option<String> {
            let mut q = self.guid_answers.lock().unwrap();
            if q.is_empty() { None } else { q.remove(0) }
        }
    }

    fn inline(_: &str, f: Box<dyn FnOnce() + Send + 'static>) -> bool {
        f();
        true
    }

    fn refuse(_: &str, _: Box<dyn FnOnce() + Send + 'static>) -> bool {
        false
    }

    fn store(mock: &Arc<Mock>) -> WatchlistStore {
        WatchlistStore::with_io(mock.clone()).with_spawn_for_test(inline)
    }

    /// `Open` handed the input the app read.
    fn open_handed(s: &mut WatchlistStore, input: &crate::watchlist::WatchlistInput) -> bool {
        s.run_with_input(WatchlistCmd::Open, input)
    }

    /// One frame pass handed the profile the app read.
    fn pump_handed(s: &mut WatchlistStore, profile: &plx_plex::plex::session::CurrentProfile) -> bool {
        s.pump_with_gate(plx_machine::landgate::fixture_gate(), profile)
    }

    /// Open, then pump until nothing is left to land.
    fn load(s: &mut WatchlistStore) {
        load_as(s, 1);
    }

    /// [`load`] with the seat at `generation`: the input it opens under and the profile it pumps with.
    fn load_as(s: &mut WatchlistStore, generation: u32) {
        open_handed(s, &crate::watchlist::WatchlistInput::for_test(generation));
        let profile = crate::watchlist::WatchlistInput::profile_for_test(generation);
        for _ in 0..4 {
            pump_handed(s, &profile);
        }
    }

    fn titles(s: &WatchlistStore) -> Vec<String> {
        let v = s.view();
        (0..v.len()).map(|i| v.row(i).unwrap().title.clone()).collect()
    }

    /// A store whose first list read is out on a worker and has not landed (the claim is held).
    pub(in crate::stores) fn reading_for_test(sid: ServerId, titles: u8) -> WatchlistStore {
        let mock = Mock::new((1..=titles).map(|n| item(n, "T")).collect());
        *mock.roster.lock().unwrap() = vec![sid];
        *mock.up.lock().unwrap() = vec![sid];
        let s = store(&mock);
        s.adapter.list_busy.store(true, Ordering::SeqCst);
        s
    }

    /// A loaded store whose one title is matched on reachable server `sid` as numeric ratingKey `rk` and
    /// whose hold window covers it: what `Stores::holds` enumerates for the Watchlist page.
    pub(in crate::stores) fn holding_for_test(sid: ServerId, rk: &str) -> WatchlistStore {
        let mock = Mock::new(vec![item(1, "Alpha")]);
        *mock.roster.lock().unwrap() = vec![sid];
        *mock.up.lock().unwrap() = vec![sid];
        mock.held.lock().unwrap().insert((sid, guid(1)), rk.into());
        let mut s = store(&mock);
        load(&mut s);
        s.run(WatchlistCmd::Window { wanted: 0..1 });
        s
    }

    /// A scripted world a test outside this file drives: the server `sid`, `titles` movies on the list
    /// (numbered from 1) and the `(title number, ratingKey)` pairs `sid` holds, with the list read.
    pub(in crate::stores) fn scripted_for_test(sid: ServerId, titles: u8, held: &[(u8, &str)]) -> (WatchlistStore, Script) {
        let mock = Mock::new((1..=titles).map(|n| item(n, "T")).collect());
        *mock.roster.lock().unwrap() = vec![sid];
        *mock.up.lock().unwrap() = vec![sid];
        for (n, rk) in held {
            mock.held.lock().unwrap().insert((sid, guid(*n)), (*rk).into());
        }
        let mut s = store(&mock);
        load(&mut s);
        (s, Script(mock))
    }

    /// The handle on [`scripted_for_test`]'s world: what the server holds now, and what it was asked.
    pub(in crate::stores) struct Script(Arc<Mock>);

    impl Script {
        pub(in crate::stores) fn hold(&self, sid: ServerId, n: u8, rk: &str) {
            self.0.held.lock().unwrap().insert((sid, guid(n)), rk.into());
        }
        pub(in crate::stores) fn drop_hold(&self, sid: ServerId, n: u8) {
            self.0.held.lock().unwrap().remove(&(sid, guid(n)));
        }
        pub(in crate::stores) fn guid(n: u8) -> String {
            guid(n)
        }
        /// The guids of every guid lookup since the list was read, per request.
        pub(in crate::stores) fn asked(&self) -> Vec<Vec<String>> {
            self.0.found.lock().unwrap().clone()
        }
        pub(in crate::stores) fn forget_asked(&self) {
            self.0.found.lock().unwrap().clear();
        }
    }

    fn three() -> Arc<Mock> {
        Mock::new(vec![item(1, "Alpha"), item(2, "Bravo"), item(3, "Charlie")])
    }

    #[test]
    fn the_list_lands_in_server_order_with_its_matches() {
        let mock = three();
        *mock.released.lock().unwrap() = Some(mock.items.lock().unwrap().clone());
        mock.held.lock().unwrap().insert((ServerId::from_raw(0), guid(2)), "42".into());
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(s.view().status(), Status::Ready);
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"]);
        assert_eq!(s.view().state(0), Some(TileState::NotOnAnyServer));
        assert_eq!(s.view().state(1), Some(TileState::Available));
        assert_eq!(s.view().copy(1).map(|c| c.rating_key), Some(42));
    }

    /// A one-row page that would append "Ghost" if it were applied.
    fn ghost_page() -> What {
        What::Page {
            start: 100,
            total: Some(4),
            rows: vec![crate::watchlist::row_of(&item(9, "Ghost"), 0).unwrap()],
            answers: vec![],
            roster_gen: 1,
            etag: None,
        }
    }

    #[test]
    fn a_landing_for_another_profile_generation_is_dropped() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"]);
        // Ready and nothing owed: no real list can land after this and supersede the ghost, so
        // an applied stale landing would show.
        let auth = s.state.auth.clone().unwrap();
        let stale = Tag { profile_gen: auth.generation + 1, epoch: s.state.epoch, req: 99 };
        s.adapter_for_test().post_for_test(Landed { tag: stale, what: ghost_page() });
        s.pump();
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"], "a stale generation never lands");
        let fresh = Tag { profile_gen: auth.generation, epoch: s.state.epoch, req: 99 };
        s.adapter_for_test().post_for_test(Landed { tag: fresh, what: ghost_page() });
        s.pump();
        assert!(titles(&s).contains(&"Ghost".to_string()), "the same landing with the live tag does");
    }

    #[test]
    fn a_profile_switch_rotates_the_epoch_and_clears_everything() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        let epoch = s.state.epoch;
        s.run(WatchlistCmd::Press { hex: hex_of(1), intent: Intent::Remove });
        pump_handed(&mut s, &crate::watchlist::WatchlistInput::profile_for_test(2));
        assert_ne!(s.state.epoch, epoch);
        assert!(s.view().is_empty());
        assert_eq!(s.view().status(), Status::Idle);
        assert!(!s.state.ledger.busy(), "the old profile's queued write is never sent to the new one");
        assert!(mock.writes.lock().unwrap().is_empty());
    }

    // ---- the session input (docs/sans-io.md S5): the step is handed the seated profile ----------

    /// The scripted seam says the profile is current and holds generation 1; the profile the app
    /// hands is generation 2, so only a step that reads what it is handed notices the switch.
    #[test]
    fn a_profile_switch_is_noticed_from_the_handed_profile() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        let epoch = s.state.epoch;
        let switched = crate::watchlist::WatchlistInput::profile_for_test(2);
        assert!(pump_handed(&mut s, &switched), "the pass that notices the switch reports a change");
        assert_ne!(s.state.epoch, epoch, "a new epoch");
        assert!(s.view().is_empty());
        assert_eq!(s.view().status(), Status::Idle);
    }

    /// The scripted seam would capture generation 1; the input carries generation 7.
    #[test]
    fn open_captures_the_handed_auth() {
        let mock = three();
        let mut s = store(&mock);
        open_handed(&mut s, &crate::watchlist::WatchlistInput::for_test(7));
        assert_eq!(s.state.auth.as_ref().map(|a| a.generation), Some(7));
    }

    /// The scripted seam would capture an identity; the input carries none (signed out, or no
    /// plex.tv credential): the page says so and reads nothing.
    #[test]
    fn an_input_with_no_auth_is_no_account() {
        let mock = three();
        let mut s = store(&mock);
        open_handed(&mut s, &crate::watchlist::WatchlistInput::signed_out_for_test(1));
        assert_eq!(s.view().status(), Status::NoAccount);
        s.pump();
        assert_eq!(mock.pages.load(Ordering::SeqCst), 0, "no identity, no read");
    }

    /// A sign-out is a pass handed a profile with nobody seated: the store resets in that pass, and the
    /// next `Open` (handed no credential) says there is no account.
    #[test]
    fn a_sign_out_resets_the_pass_and_the_next_open_is_no_account() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(s.view().status(), Status::Ready);
        let epoch = s.state.epoch;
        let signed_out = plx_plex::plex::session::CurrentProfile { user: None, generation: 2, stamp: String::new() };
        assert!(pump_handed(&mut s, &signed_out), "the pass that notices the sign-out reports a change");
        assert_ne!(s.state.epoch, epoch);
        assert!(s.view().is_empty());
        assert_eq!(s.view().status(), Status::Idle);
        let pages = mock.pages.load(Ordering::SeqCst);
        open_handed(&mut s, &crate::watchlist::WatchlistInput::new(Arc::new(signed_out), None, "test-client"));
        assert_eq!(s.view().status(), Status::NoAccount);
        s.pump();
        assert_eq!(mock.pages.load(Ordering::SeqCst), pages, "no account, no read");
    }

    /// A landing tagged with the generation a switch left behind never reaches the new epoch, whichever
    /// mailbox it arrives in; one tagged with the new generation does.
    #[test]
    fn a_landing_tagged_with_the_old_generation_is_dropped_after_a_switch() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        pump_handed(&mut s, &crate::watchlist::WatchlistInput::profile_for_test(2));
        load_as(&mut s, 2);
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"]);
        let epoch = s.state.epoch;
        let old = Tag { profile_gen: 1, epoch, req: 99 };
        s.adapter_for_test().post_for_test(Landed { tag: old, what: ghost_page() });
        pump_handed(&mut s, &crate::watchlist::WatchlistInput::profile_for_test(2));
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"], "the old generation never lands");
        let live = Tag { profile_gen: 2, epoch, req: 99 };
        s.adapter_for_test().post_for_test(Landed { tag: live, what: ghost_page() });
        pump_handed(&mut s, &crate::watchlist::WatchlistInput::profile_for_test(2));
        assert!(titles(&s).contains(&"Ghost".to_string()), "the new generation's does");
    }

    /// The same seat is not a switch: a pass handed the profile the store opened under changes nothing,
    /// and an `Open` under it keeps the identity (and the rows) it holds.
    #[test]
    fn the_same_profile_is_not_a_switch() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        let epoch = s.state.epoch;
        pump_handed(&mut s, &crate::watchlist::WatchlistInput::profile_for_test(1));
        open_handed(&mut s, &crate::watchlist::WatchlistInput::for_test(1));
        assert_eq!(s.state.epoch, epoch);
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"]);
    }

    /// `run` is the entry of a caller that holds no input, so an `Open` has no input to capture from.
    #[test]
    #[should_panic(expected = "an Open reached a step handed no input")]
    fn an_open_sent_through_the_inputless_entry_is_refused() {
        let mock = three();
        let mut s = store(&mock);
        s.run(WatchlistCmd::Open);
    }

    #[test]
    fn a_landing_of_a_retired_epoch_is_dropped() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        let old_tag = Tag { profile_gen: 1, epoch: s.state.epoch, req: 1 };
        let old_mailbox = s.adapter_for_test();
        s.run(WatchlistCmd::Reset);
        load(&mut s);
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"]);
        // A worker of the retired epoch lands late into the mailbox it captured.
        old_mailbox.post_for_test(Landed { tag: old_tag, what: ghost_page() });
        // And one that somehow reaches the new mailbox still carries the old epoch.
        s.adapter_for_test().post_for_test(Landed { tag: old_tag, what: ghost_page() });
        s.pump();
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"], "only the new epoch's own list");
    }

    #[test]
    fn a_press_during_a_read_survives_it() {
        let mock = three();
        let mut s = store(&mock);
        s.run_for_test(WatchlistCmd::Open);
        // The first pump issues the read (stamped now); the press follows before it is landed.
        s.state.list_wanted = true;
        s.maybe_spawn();
        s.run(WatchlistCmd::Press { hex: hex_of(2), intent: Intent::Remove });
        s.run(WatchlistCmd::Press { hex: hex_of(7), intent: Intent::Add });
        let gate = plx_machine::landgate::fixture_gate();
        for _ in 0..3 {
            s.pump_with_gate(gate, &crate::watchlist::WatchlistInput::profile_for_test(1));
        }
        assert_eq!(titles(&s), ["Alpha", "Charlie"], "the read, issued before the press, does not bring Bravo back");
        assert!(s.view().is_member(&hex_of(7)), "nor does it forget an add the list has not heard of");
    }

    #[test]
    fn a_failed_write_reverts_without_a_toast() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        mock.fail_writes.store(true, Ordering::SeqCst);
        s.run(WatchlistCmd::Press { hex: hex_of(2), intent: Intent::Remove });
        assert_eq!(titles(&s), ["Alpha", "Charlie"], "flipped at once");
        for _ in 0..3 {
            s.pump();
        }
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"], "reverted");
        assert!(s.view().is_member(&hex_of(2)));
        assert!(!s.take_reread_owed(), "a failed write owes no re-read");
    }

    #[test]
    fn a_landed_write_stays_and_owes_one_reread() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        s.run(WatchlistCmd::Press { hex: hex_of(2), intent: Intent::Remove });
        for _ in 0..3 {
            s.pump();
        }
        assert_eq!(*mock.writes.lock().unwrap(), [(id(2), Intent::Remove)]);
        assert_eq!(titles(&s), ["Alpha", "Charlie"]);
        assert!(s.take_reread_owed());
        assert!(!s.take_reread_owed(), "once per burst");
    }

    #[test]
    fn presses_on_one_id_coalesce_and_the_newest_intent_is_the_one_sent() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        s.run(WatchlistCmd::Press { hex: hex_of(2), intent: Intent::Remove });
        s.run(WatchlistCmd::Press { hex: hex_of(2), intent: Intent::Add });
        s.run(WatchlistCmd::Press { hex: hex_of(2), intent: Intent::Remove });
        for _ in 0..4 {
            s.pump();
        }
        assert_eq!(*mock.writes.lock().unwrap(), [(id(2), Intent::Remove)], "one write, the newest");
    }

    #[test]
    fn writes_for_different_ids_go_out_in_press_order() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        s.run(WatchlistCmd::Press { hex: hex_of(3), intent: Intent::Remove });
        s.run(WatchlistCmd::Press { hex: hex_of(1), intent: Intent::Remove });
        for _ in 0..6 {
            s.pump();
        }
        assert_eq!(*mock.writes.lock().unwrap(), [(id(3), Intent::Remove), (id(1), Intent::Remove)]);
    }

    #[test]
    fn a_silent_server_is_not_memoized_as_no_portable_id() {
        let mock = three();
        let sid = ServerId::from_raw(0);
        let mut s = store(&mock);
        // First read: no answer at all.
        s.want_hex(sid, "7");
        s.pump();
        s.pump();
        assert_eq!(s.view().resolved_hex(sid, "7"), None, "no answer is not an answer");
        // The next menu asks again and now gets a portable guid.
        mock.guid_answers.lock().unwrap().push(Some("plex://movie/abababababababababababab".into()));
        s.want_hex(sid, "7");
        s.pump();
        s.pump();
        assert_eq!(s.view().resolved_hex(sid, "7"), Some(Some([0xab; 12])));
        // A server that answers with a non-portable guid IS memoized.
        mock.guid_answers.lock().unwrap().push(Some("local://1".into()));
        s.want_hex(sid, "8");
        s.pump();
        s.pump();
        assert_eq!(s.view().resolved_hex(sid, "8"), Some(None));
    }

    #[test]
    fn a_refused_spawn_retries_after_n_frames_and_never_blocks() {
        let mock = three();
        let mut s = store(&mock).with_spawn_for_test(refuse);
        s.run_for_test(WatchlistCmd::Open);
        s.pump();
        assert_eq!(mock.pages.load(Ordering::SeqCst), 0);
        assert!(!s.adapter.list_busy.load(Ordering::SeqCst), "the claim is released");
        s = s.with_spawn_for_test(inline);
        for _ in 0..(RETRY_FRAMES - 2) {
            s.pump();
        }
        assert_eq!(mock.pages.load(Ordering::SeqCst), 0, "not before N frames");
        for _ in 0..4 {
            s.pump();
        }
        assert_eq!(s.view().status(), Status::Ready, "then it goes");
    }

    /// A READ the dev-token gate refused (`WatchlistError::Refused`, `plex::account::permit_read`) cannot
    /// succeed later in this process, and nothing re-asks it: `list_wanted` is cleared when the worker
    /// spawns and only a mount or a reread sets it again. It lands once as the unavailable read-out,
    /// never as an empty list, and ten simulated seconds of frames make no second request.
    #[test]
    fn a_refused_list_read_is_asked_once_and_is_not_retried_on_a_timer() {
        let mock = three();
        mock.fail_pages_from.store(0, Ordering::SeqCst);
        mock.fail_refused.store(true, Ordering::SeqCst);
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(s.view().status(), Status::Failed(Failure::Other), "unavailable, not an empty list");
        assert_eq!(s.view().len(), 0);
        for _ in 0..(10 * 60) {
            s.pump();
        }
        assert_eq!(mock.pages.load(Ordering::SeqCst), 1, "a refused read is not asked again");
        assert_eq!(s.view().status(), Status::Failed(Failure::Other));
    }

    #[test]
    fn a_refused_write_spawn_keeps_the_write_for_later() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        s = s.with_spawn_for_test(refuse);
        s.run(WatchlistCmd::Press { hex: hex_of(1), intent: Intent::Remove });
        s.pump();
        assert!(s.state.ledger.busy(), "still owed");
        s = s.with_spawn_for_test(inline);
        for _ in 0..(RETRY_FRAMES + 4) {
            s.pump();
        }
        assert_eq!(*mock.writes.lock().unwrap(), [(id(1), Intent::Remove)]);
    }

    #[test]
    fn a_roster_move_rematches_every_row() {
        let mock = three();
        *mock.released.lock().unwrap() = Some(mock.items.lock().unwrap().clone());
        let b = ServerId::from_raw(1);
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(s.view().state(0), Some(TileState::NotOnAnyServer));
        // A second server joins and holds the first title.
        mock.roster.lock().unwrap().push(b);
        mock.up.lock().unwrap().push(b);
        mock.held.lock().unwrap().insert((b, guid(1)), "7".into());
        mock.roster_gen.fetch_add(1, Ordering::SeqCst);
        mock.facts_gen.fetch_add(1, Ordering::SeqCst);
        for _ in 0..4 {
            s.pump();
        }
        assert_eq!(s.state.table.roster_gen, mock.roster_gen.load(Ordering::SeqCst));
        assert_eq!(s.view().state(0), Some(TileState::Available));
        assert_eq!(s.view().copy(0).map(|c| (c.server_id, c.rating_key)), Some((b, 7)));
        assert_eq!(s.view().state(1), Some(TileState::NotOnAnyServer), "both servers answered absent");
    }

    #[test]
    fn a_reachability_flip_rederives_every_tile_in_one_pump() {
        let mock = three();
        let a = ServerId::from_raw(0);
        mock.held.lock().unwrap().insert((a, guid(1)), "5".into());
        mock.held.lock().unwrap().insert((a, guid(2)), "6".into());
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!((s.view().state(0), s.view().state(1)), (Some(TileState::Available), Some(TileState::Available)));
        mock.up.lock().unwrap().clear();
        mock.facts_gen.fetch_add(1, Ordering::SeqCst);
        s.pump();
        assert_eq!(
            (s.view().state(0), s.view().state(1)),
            (Some(TileState::ServerOffline), Some(TileState::ServerOffline)),
            "one facts epoch moves them together"
        );
        assert!(s.held_keys().is_empty() || s.state.window.is_empty());
        mock.up.lock().unwrap().push(a);
        mock.facts_gen.fetch_add(1, Ordering::SeqCst);
        s.pump();
        assert_eq!(s.view().state(0), Some(TileState::Available));
    }

    #[test]
    fn held_keys_are_the_matched_reachable_tiles_in_the_hold_window() {
        let mock = three();
        let a = ServerId::from_raw(0);
        mock.held.lock().unwrap().insert((a, guid(1)), "5".into());
        mock.held.lock().unwrap().insert((a, guid(3)), "9".into());
        let mut s = store(&mock);
        load(&mut s);
        assert!(s.held_keys().is_empty(), "no window yet");
        s.run(WatchlistCmd::Window { wanted: 0..2 });
        assert_eq!(s.held_keys(), vec![(a, "5".to_string())], "Bravo is matched nowhere, Charlie is outside");
        s.run(WatchlistCmd::Window { wanted: 0..99 });
        assert_eq!(s.held_keys(), vec![(a, "5".to_string()), (a, "9".to_string())], "clamped to the list");
        mock.up.lock().unwrap().clear();
        mock.facts_gen.fetch_add(1, Ordering::SeqCst);
        s.pump();
        assert!(s.held_keys().is_empty(), "an unreachable server is not read back");
    }

    /// The user's OWN press reaches `publish_view` directly (optimistic edit), never the stop
    /// path or the read-back: the Stores wrapper must still patch the matched tile at the press.
    #[test]
    fn a_press_through_the_stores_patches_a_matched_tile_before_any_reconcile() {
        use crate::viewstate::Write;
        let _g = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("wl-press", "127.0.0.1", 9, "t", "wl-press-client");
        let mock = three();
        mock.held.lock().unwrap().insert((sid, guid(2)), "42".into());
        let mut stores = crate::stores::Stores::default();
        stores.watchlist = store(&mock);
        load(&mut stores.watchlist);
        assert_eq!(stores.watchlist.view().state(1), Some(TileState::Available));
        assert_eq!(stores.watchlist.view().facts(1), None);
        let directory = crate::stores::browse::DirectorySnapshot::default();
        assert!(stores.viewstate_run(crate::stores::viewstate::ViewStateCmd::Request {
            sid, rk: "42".into(), write: Write::Watched, scope: Scope::Leaf, guid: String::new(),
        }, directory.view(), &crate::pms::registry_keys_for_test(), 0).changed);
        assert_eq!(stores.watchlist.view().facts(1).map(|f| f.watched), Some(true),
            "patched at the press, before any pump or reconcile");
        drop(stores);
        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn a_view_change_patches_a_matched_tile_in_place() {
        let mock = three();
        let a = ServerId::from_raw(0);
        mock.held.lock().unwrap().insert((a, guid(2)), "42".into());
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(s.view().facts(1), None);
        let change = |rk: &str, fact| ViewChange { sid: a, rk: rk.into(), scope: Scope::Leaf, fact };
        assert!(s.apply_view(&change("42", ViewFact::Offset { ms: 30_000, dur_ms: 100_000 })));
        assert_eq!(s.view().facts(1), Some(TileFacts { watched: false, resume_ms: 30_000, dur_ms: 100_000 }));
        assert!(s.apply_view(&change("42", ViewFact::Watched(true))));
        assert_eq!(s.view().facts(1).map(|f| (f.watched, f.resume_ms)), Some((true, 0)));
        assert!(s.apply_view(&change("42", ViewFact::Facts { watched: false, unwatched: true, resume_ms: 5, sort: None })));
        assert_eq!(s.view().facts(1).map(|f| (f.watched, f.resume_ms)), Some((false, 5)));
        assert!(!s.apply_view(&change("999", ViewFact::Watched(true))), "an item no tile is matched to");
        assert!(!s.apply_view(&ViewChange { sid: ServerId::from_raw(3), ..change("42", ViewFact::Watched(true)) }),
            "the same key on another server is another item");
        assert!(s.take_notice().is_some(), "a patch raises a notice");
    }

    #[test]
    fn the_released_set_marks_coming_soon_and_a_failed_read_falls_back_to_the_date() {
        let mock = three();
        *mock.released.lock().unwrap() = Some(vec![item(1, "Alpha"), item(2, "Bravo")]);
        let mut s = store(&mock);
        load(&mut s);
        let released: Vec<bool> = (0..3).map(|i| s.view().row(i).unwrap().released).collect();
        assert_eq!(released, [true, true, false]);
        assert_eq!(s.view().state(2), Some(TileState::ComingSoon));

        let mock = three();
        mock.items.lock().unwrap()[2].originally_available = Some("2027-01-01".into());
        let mut s = store(&mock);
        load(&mut s);
        let released: Vec<bool> = (0..3).map(|i| s.view().row(i).unwrap().released).collect();
        assert_eq!(released, [true, true, false], "no released list: the release date against today");
    }

    #[test]
    fn sort_and_filter_are_client_side_and_make_no_request() {
        let mock = three();
        mock.held.lock().unwrap().insert((ServerId::from_raw(0), guid(3)), "1".into());
        let mut s = store(&mock);
        load(&mut s);
        let pages = mock.pages.load(Ordering::SeqCst);
        s.run(WatchlistCmd::Sort(SortSpec { key: SortKey::Title, desc: true }));
        assert_eq!(titles(&s), ["Charlie", "Bravo", "Alpha"]);
        s.run(WatchlistCmd::Filter(FilterSpec { kind: None, show: Show::OnYourServers }));
        assert_eq!(titles(&s), ["Charlie"]);
        s.pump();
        assert_eq!(mock.pages.load(Ordering::SeqCst), pages);
    }

    #[test]
    fn a_reread_keeps_the_rows_until_the_new_list_is_whole() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        mock.items.lock().unwrap().remove(0);
        s.run(WatchlistCmd::Reread);
        s.maybe_spawn();
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"], "nothing blanked while it is out");
        for _ in 0..3 {
            s.pump();
        }
        assert_eq!(titles(&s), ["Bravo", "Charlie"]);
    }

    #[test]
    fn a_reread_matches_only_the_guids_the_table_lacks() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        let first = mock.finds.load(Ordering::SeqCst);
        assert!(first > 0);
        s.run(WatchlistCmd::Reread);
        for _ in 0..4 {
            s.pump();
        }
        assert_eq!(mock.finds.load(Ordering::SeqCst), first, "an unchanged list asks no server again");
        mock.items.lock().unwrap().push(item(4, "Delta"));
        s.run(WatchlistCmd::Reread);
        for _ in 0..4 {
            s.pump();
        }
        assert_eq!(mock.finds.load(Ordering::SeqCst), first + 1, "one new movie guid: one chunk, one server");
        assert_eq!(titles(&s).len(), 4);
        // A roster move is a full re-match.
        mock.roster_gen.fetch_add(1, Ordering::SeqCst);
        s.run(WatchlistCmd::Reread);
        for _ in 0..4 {
            s.pump();
        }
        assert_eq!(mock.finds.load(Ordering::SeqCst), first + 2);
        assert_eq!(s.state.table.roster_gen, mock.roster_gen.load(Ordering::SeqCst));
    }

    #[test]
    fn paging_advances_by_the_wire_count_not_the_decoded_count() {
        // A full page of 100 entries whose decoder kept 99, and no totalSize.
        let items: Vec<WatchItem> = (1..=100u8).map(|n| item(n, "T")).collect();
        let mock = Mock::new(items);
        mock.dropped.store(1, Ordering::SeqCst);
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(*mock.starts.lock().unwrap(), [0, 100]);
    }

    #[test]
    fn paging_ends_at_total_size_with_no_page_count_cap() {
        // 2100 entries is 21 pages: past the old 20-page cap.
        let items: Vec<WatchItem> = (0..2100u32).map(|i| item((i % 250) as u8 + 1, "T")).collect();
        let mock = Mock::new(items);
        let mut s = store(&mock);
        s.run_for_test(WatchlistCmd::Open);
        for _ in 0..40 {
            s.pump();
        }
        let starts = mock.starts.lock().unwrap().clone();
        assert_eq!(starts.len(), 21);
        assert_eq!(starts.last(), Some(&2000));
    }

    #[test]
    fn paging_ends_on_a_short_page_when_the_server_gives_no_total() {
        let items: Vec<WatchItem> = (1..=150u8).map(|n| item(n, "T")).collect();
        let mock = Mock::new(items);
        mock.dropped.store(1, Ordering::SeqCst);
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(*mock.starts.lock().unwrap(), [0, 100]);
    }

    #[test]
    fn a_failed_first_read_is_a_failure_and_a_failed_reread_keeps_the_rows() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        // A later read that finds no answer keeps the list.
        let landed = |what| Landed { tag: Tag { profile_gen: 1, epoch: s.state.epoch, req: 0 }, what };
        let a = s.adapter_for_test();
        a.post_for_test(landed(What::PageFailed { start: 0, why: Failure::Offline }));
        s.pump();
        assert_eq!(s.view().status(), Status::Ready);
        assert_eq!(s.view().len(), 3);
    }

    #[test]
    fn items_of_other_kinds_or_without_an_id_never_become_rows() {
        let mut other = item(4, "Collection");
        other.kind = Some(WatchKind::Other);
        let mut bad = item(5, "Bad id");
        bad.hex = "nothex".into();
        let mock = Mock::new(vec![item(1, "Alpha"), other, bad]);
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(titles(&s), ["Alpha"]);
    }

    #[test]
    fn the_ordinal_of_the_watchlist_store_is_appended_after_every_other() {
        assert_eq!(super::super::StoreId::Watchlist.ord().0, 10);
        assert_eq!(super::super::StoreId::ViewStop.ord().0, 9);
    }

    #[test]
    fn the_copy_is_the_current_server_then_owned_then_a_share_not_roster_order() {
        let (a, b, c) = (ServerId::from_raw(0), ServerId::from_raw(1), ServerId::from_raw(2));
        let mock = three();
        *mock.roster.lock().unwrap() = vec![a, b, c];
        *mock.up.lock().unwrap() = vec![a, b, c];
        for (sid, rk) in [(a, "10"), (b, "20"), (c, "30")] {
            mock.held.lock().unwrap().insert((sid, guid(2)), rk.into());
        }
        let mut s = store(&mock);
        // All shares: roster order decides.
        load(&mut s);
        assert_eq!(s.view().copy(1).map(|m| m.rating_key), Some(10));
        // The owned server is second in the roster and wins over the first share.
        mock.ranks.lock().unwrap().insert(b, 1);
        assert_eq!(s.view().copy(1).map(|m| m.rating_key), Some(20), "owned beats a share");
        // The current server is last in the roster and wins over both.
        mock.ranks.lock().unwrap().insert(c, 0);
        assert_eq!(s.view().copy(1).map(|m| m.rating_key), Some(30), "current beats owned");
        // An unreachable preferred copy is never chosen.
        mock.up.lock().unwrap().retain(|x| *x != c);
        assert_eq!(s.view().copy(1).map(|m| m.rating_key), Some(20));
    }

    /// **The caption's facts are the OPENED copy's**: the listing row of the server whose copy a
    /// press opens, never another server's, and none for a copy that is not reachable.
    #[test]
    fn the_listing_facts_are_those_of_the_copy_a_tile_opens() {
        let (a, b) = (ServerId::from_raw(0), ServerId::from_raw(1));
        let mock = three();
        *mock.roster.lock().unwrap() = vec![a, b];
        *mock.up.lock().unwrap() = vec![a, b];
        for (sid, rk, genre) in [(a, "10", "Drama"), (b, "20", "Comedy")] {
            mock.held.lock().unwrap().insert((sid, guid(2)), rk.into());
            mock.listings.lock().unwrap().insert(sid, ListingFacts { genre: Some(genre.into()), ..Default::default() });
        }
        let mut s = store(&mock);
        load(&mut s);
        let genre = |s: &WatchlistStore| s.view().listing(1).and_then(|l| l.genre.clone());
        assert_eq!((s.view().copy(1).map(|m| m.rating_key), genre(&s)), (Some(10), Some("Drama".into())));
        mock.ranks.lock().unwrap().insert(b, 1);
        assert_eq!((s.view().copy(1).map(|m| m.rating_key), genre(&s)), (Some(20), Some("Comedy".into())), "the owned copy's facts");
        mock.up.lock().unwrap().clear();
        assert_eq!((s.view().copy(1), genre(&s)), (None, None), "no reachable copy, no facts");
        assert_eq!(s.view().listing(0), None, "a title no server holds has none");
    }

    #[test]
    fn a_failed_released_read_leaves_an_undated_absent_row_unknown_not_washed() {
        let mock = three();
        // The released read fails (the mock's default); every server answers absent.
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(s.view().state(0), Some(TileState::Unknown), "no date, no released set: not NotOnAnyServer");
    }

    #[test]
    fn a_reread_whose_later_page_fails_keeps_the_whole_old_list_untruncated() {
        let mock = Mock::new((1..=150).map(|n| item(n, &format!("T{n:03}"))).collect());
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(s.view().len(), 150);
        assert!(!s.view().truncated());
        mock.fail_pages_from.store(100, Ordering::SeqCst);
        s.run(WatchlistCmd::Reread);
        for _ in 0..3 {
            s.pump();
        }
        assert_eq!(s.view().len(), 150, "the old rows stay");
        assert!(!s.view().truncated(), "a complete old list is not reported as cut short");
    }

    #[test]
    fn a_first_read_whose_later_page_fails_is_truncated() {
        let mock = Mock::new((1..=150).map(|n| item(n, &format!("T{n:03}"))).collect());
        mock.fail_pages_from.store(100, Ordering::SeqCst);
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(s.view().len(), 100);
        assert!(s.view().truncated());
    }

    #[test]
    fn the_drawn_state_holds_until_the_pump_that_rederives_it() {
        let mock = three();
        mock.held.lock().unwrap().insert((ServerId::from_raw(0), guid(2)), "42".into());
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(s.view().state(1), Some(TileState::Available));
        // The server drops and the facts move, but no pump has run: a press in this tick routes
        // on what is drawn.
        mock.up.lock().unwrap().clear();
        mock.facts_gen.fetch_add(1, Ordering::SeqCst);
        assert_eq!(s.view().state(1), Some(TileState::Available), "the committed state, not the live one");
        s.pump();
        assert_eq!(s.view().state(1), Some(TileState::ServerOffline), "the pump re-derives it");
    }

    // ---- NET1: the page-0 ETag ------------------------------------------------------------

    /// 150 titles: two pages, so a 304 that skips the second page is visible in the request count.
    fn long_list() -> Arc<Mock> {
        Mock::new((1..=150u8).map(|n| item(n, "T")).collect())
    }

    fn asked(mock: &Mock) -> Vec<(u32, Option<String>)> {
        mock.asked.lock().unwrap().clone()
    }

    /// Run one owed re-read to the end.
    fn reread(s: &mut WatchlistStore) {
        s.run(WatchlistCmd::Reread);
        for _ in 0..6 {
            s.pump();
        }
    }

    #[test]
    fn the_first_read_sends_no_validator_and_a_complete_one_stores_the_page_zero_etag() {
        let mock = long_list();
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(asked(&mock), [(0, None), (100, None)]);
        assert_eq!(s.state.list_etag, mock.tag());
    }

    #[test]
    fn a_304_keeps_the_rows_and_skips_every_page() {
        let mock = long_list();
        let mut s = store(&mock);
        load(&mut s);
        let before = titles(&s);
        reread(&mut s);
        assert_eq!(asked(&mock), [(0, None), (100, None), (0, mock.tag())], "page 0 only, with the validator");
        assert!(mock.tag().is_some());
        assert_eq!(s.view().status(), Status::Ready);
        assert_eq!(titles(&s), before);
        assert_eq!(s.view().list_len(), 150);
        assert!(!s.view().truncated());
        assert_eq!(s.state.list_etag, mock.tag(), "the validator still stands");
    }

    #[test]
    fn a_304_still_refreshes_the_released_set() {
        let mock = three();
        *mock.released.lock().unwrap() = Some(vec![]);
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(s.state.released.as_ref().map(HashSet::len), Some(0));
        *mock.released.lock().unwrap() = Some(vec![item(1, "Alpha")]);
        reread(&mut s);
        assert_eq!(mock.asked.lock().unwrap().last().cloned(), Some((0, mock.tag())));
        assert_eq!(s.state.released.as_ref().map(HashSet::len), Some(1), "released moves without the list moving");
    }

    #[test]
    fn a_changed_etag_is_a_full_read_and_the_new_validator_is_kept() {
        let mock = long_list();
        let mut s = store(&mock);
        load(&mut s);
        let v1 = mock.tag();
        mock.items.lock().unwrap().remove(0);
        assert_ne!(mock.tag(), v1);
        reread(&mut s);
        assert_eq!(s.view().list_len(), 149);
        assert_eq!(asked(&mock)[2..], [(0, v1), (100, None)]);
        reread(&mut s);
        assert_eq!(asked(&mock).last(), Some(&(0, mock.tag())));
    }

    #[test]
    fn a_service_that_sends_no_etag_is_always_read_in_full() {
        let mock = three();
        mock.etag_on.store(false, Ordering::SeqCst);
        let mut s = store(&mock);
        load(&mut s);
        reread(&mut s);
        assert_eq!(asked(&mock), [(0, None), (0, None)]);
        assert_eq!(s.state.list_etag, None);
    }

    #[test]
    fn a_landed_write_invalidates_the_validator_so_the_re_read_is_whole() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        s.run(WatchlistCmd::Press { hex: hex_of(2), intent: Intent::Remove });
        for _ in 0..3 {
            s.pump();
        }
        assert_eq!(s.state.list_etag, None);
        assert!(s.take_reread_owed());
        // The service, not knowing better, would answer 304 to the stale validator: it must not be sent.
        reread(&mut s);
        assert_eq!(asked(&mock).last(), Some(&(0, None)));
    }

    #[test]
    fn a_failed_write_invalidates_it_too() {
        let mock = three();
        mock.fail_writes.store(true, Ordering::SeqCst);
        let mut s = store(&mock);
        load(&mut s);
        s.run(WatchlistCmd::Press { hex: hex_of(2), intent: Intent::Remove });
        for _ in 0..3 {
            s.pump();
        }
        assert_eq!(s.state.list_etag, None, "a request that may have reached the service changed what it holds");
    }

    #[test]
    fn a_read_that_a_write_overtook_does_not_store_its_validator() {
        thread_local! { static HELD: std::cell::RefCell<Vec<Box<dyn FnOnce() + Send>>> = Default::default(); }
        fn hold(_: &str, f: Box<dyn FnOnce() + Send + 'static>) -> bool {
            HELD.with(|h| h.borrow_mut().push(f));
            true
        }
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        // A re-read goes out and is held (the list changed, so it is a 200 with a new validator); a
        // write is spawned while it is out; then both finish.
        mock.items.lock().unwrap().remove(2);
        s.spawn = hold;
        s.run(WatchlistCmd::Reread);
        s.pump();
        s.run(WatchlistCmd::Press { hex: hex_of(1), intent: Intent::Remove });
        s.pump();
        s.spawn = inline;
        let jobs: Vec<_> = HELD.with(|h| h.borrow_mut().drain(..).collect());
        assert_eq!(jobs.len(), 2, "the list worker and the write worker");
        for job in jobs {
            job();
        }
        for _ in 0..4 {
            s.pump();
        }
        assert_eq!(s.state.list_etag, None, "page 0 was read around the write; its validator may describe the old list");
    }

    #[test]
    fn a_profile_switch_forgets_the_validator() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        assert!(s.state.list_etag.is_some());
        pump_handed(&mut s, &crate::watchlist::WatchlistInput::profile_for_test(2));
        assert_eq!(s.state.list_etag, None);
        load_as(&mut s, 2);
        assert_eq!(asked(&mock).last(), Some(&(0, None)), "the new profile's first read is whole");
        s.run(WatchlistCmd::Reset);
        assert_eq!(s.state.list_etag, None);
    }

    #[test]
    fn a_list_cut_short_stores_no_validator() {
        let mock = long_list();
        mock.fail_pages_from.store(100, Ordering::SeqCst);
        let mut s = store(&mock);
        load(&mut s);
        assert!(s.view().truncated());
        assert_eq!(s.state.list_etag, None, "the rows are not the whole list, so 304 must never keep them");
        mock.fail_pages_from.store(u32::MAX, Ordering::SeqCst);
        reread(&mut s);
        assert_eq!(asked(&mock).last(), Some(&(100, None)));
        assert_eq!(s.view().list_len(), 150);
        assert_eq!(s.state.list_etag, mock.tag());
    }

    #[test]
    fn a_failed_reread_keeps_the_old_validator_with_the_old_rows() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        mock.fail_pages_from.store(0, Ordering::SeqCst);
        reread(&mut s);
        assert_eq!(s.view().len(), 3);
        assert_eq!(s.state.list_etag, mock.tag());
    }

    // ---- a library change re-asks one server (W-notify) ----

    fn sid0() -> ServerId {
        ServerId::from_raw(0)
    }

    fn movie_state_of(s: &WatchlistStore, n: u8) -> Option<TileState> {
        let v = s.view();
        (0..v.len()).find(|&i| v.row(i).is_some_and(|r| r.guid == guid(n))).and_then(|i| v.state(i))
    }

    #[test]
    fn a_re_match_asks_one_server_and_flips_only_the_tile_it_changed() {
        let mock = three();
        *mock.released.lock().unwrap() = Some(mock.items.lock().unwrap().clone());
        let mut s = store(&mock);
        load(&mut s);
        assert_eq!(movie_state_of(&s, 1), Some(TileState::NotOnAnyServer));
        mock.found.lock().unwrap().clear();
        mock.held.lock().unwrap().insert((sid0(), guid(1)), "11".into());
        assert!(s.rematch_on(sid0(), s.unmatched_on(sid0(), Kind::Movie)));
        assert_eq!(mock.found.lock().unwrap().len(), 1, "one chunk for three titles");
        s.pump();
        assert_eq!(movie_state_of(&s, 1), Some(TileState::Available), "the tile flips at the landing");
        assert_eq!(movie_state_of(&s, 2), Some(TileState::NotOnAnyServer));
        assert_eq!(s.state.table.roster_gen, mock.roster_gen.load(Ordering::SeqCst), "the roster mark is not touched");
    }

    #[test]
    fn held_by_rk_and_unmatched_on_read_the_table() {
        let mock = three();
        mock.held.lock().unwrap().insert((sid0(), guid(2)), "12".into());
        let mut s = store(&mock);
        load(&mut s);
        let held = s.held_by_rk(sid0());
        assert_eq!(held.get(&12), Some(&(guid(2), Kind::Movie)));
        assert_eq!(held.len(), 1);
        assert!(s.held_by_rk(ServerId::from_raw(5)).is_empty());
        let mut open: Vec<String> = s.unmatched_on(sid0(), Kind::Movie).into_iter().map(|(g, _)| g).collect();
        open.sort();
        assert_eq!(open, [guid(1), guid(3)], "a held title is not asked again");
        assert!(s.unmatched_on(sid0(), Kind::Show).is_empty(), "no show rows");
        assert_eq!(s.rows_of(Kind::Movie).len(), 3);
    }

    #[test]
    fn a_re_match_does_not_start_before_the_list_is_read_or_while_a_worker_is_out() {
        let mock = three();
        let mut s = store(&mock);
        assert!(!s.rematch_on(sid0(), vec![(guid(1), Kind::Movie)]), "nothing read yet");
        load(&mut s);
        let mut ready = store(&mock);
        load(&mut ready);
        let mut ready = ready.with_spawn_for_test(refuse);
        assert!(!ready.rematch_on(sid0(), vec![(guid(1), Kind::Movie)]), "a refused worker is not started");
        assert!(!ready.rematch_on(sid0(), vec![(guid(1), Kind::Movie)]), "and waits out its retry");
        assert!(s.rematch_on(sid0(), Vec::new()), "nothing to ask is nothing to start");
        assert!(s.rematch_on(ServerId::from_raw(9), vec![(guid(1), Kind::Movie)]), "an unreachable server is not asked");
    }

    #[test]
    fn a_re_match_is_one_request_per_forty_guids() {
        let mock = Mock::new((1..=90u8).map(|n| item(n, "T")).collect());
        let mut s = store(&mock);
        load(&mut s);
        mock.found.lock().unwrap().clear();
        assert!(s.rematch_on(sid0(), s.unmatched_on(sid0(), Kind::Movie)));
        let sizes: Vec<usize> = mock.found.lock().unwrap().iter().map(Vec::len).collect();
        assert_eq!(sizes, [40, 40, 10]);
    }
}
