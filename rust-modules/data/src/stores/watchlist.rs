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
    run_list, run_rematch, run_write, Failure, Landed, LiveIo, Tag, What, WatchlistAuth, WatchlistIo,
};
use plx_machine::machine::{Cx, Effects, Handled, Host, Machine};
use plx_plex::plex::ServerId;
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
    /// The page mounted (or the tab was entered): capture the identity if the epoch has none and
    /// read the list. A populated list is never blanked by it.
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
    index: Vec<usize>,
    window: Range<usize>,
    list_wanted: bool,
    reread_owed: bool,
    wrote: bool,
    list_retry: u32,
    write_retry: u32,
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
            index: Vec::new(),
            window: 0..0,
            list_wanted: false,
            reread_owed: false,
            wrote: false,
            list_retry: 0,
            write_retry: 0,
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
        let row = self.state.rows.get(ri)?;
        self.state
            .table
            .by_guid
            .get(&row.guid)?
            .iter()
            .filter_map(|(sid, asked)| match asked {
                Asked::Held(m) if self.io.reachable(*sid) => Some((self.io.copy_rank(*sid), *m)),
                _ => None,
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, m)| m)
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
            matches!(a, Asked::Held(m) if *s == sid && m.rating_key == rk)
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

    pub fn run(&mut self, cmd: WatchlistCmd) -> bool {
        let changed = match cmd {
            WatchlistCmd::Open => self.open(),
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

    fn open(&mut self) -> bool {
        let stale = self.state.auth.as_ref().is_none_or(|a| !self.io.is_current(a));
        if stale {
            if self.state.auth.is_some() {
                self.reset();
            }
            self.state.auth = self.io.capture_auth();
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
        for (guid, sid, asked) in answers {
            let entry = self.state.table.by_guid.entry(guid).or_default();
            match entry.iter_mut().find(|(s, _)| *s == sid) {
                Some(slot) => slot.1 = asked,
                None => entry.push((sid, asked)),
            }
        }
        self.state.table.roster_gen = roster_gen;
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
            What::Page { start, total, mut rows, answers, roster_gen } => {
                self.state.total = total;
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
            What::PageFailed { start, why } => {
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
    pub fn pump_with_gate(&mut self, gate: &plx_machine::landgate::Gate) -> bool {
        let mut changed = false;
        if self.state.auth.as_ref().is_some_and(|a| !self.io.is_current(a)) {
            self.reset();
            changed = true;
        }
        self.state.list_retry = self.state.list_retry.saturating_sub(1);
        self.state.write_retry = self.state.write_retry.saturating_sub(1);
        let adapter = Arc::clone(&self.adapter);
        for landed in super::take_landings(gate, super::StoreId::Watchlist, || adapter.take_all()) {
            plx_machine::idle::invalidate();
            changed |= self.apply(landed);
        }
        if self.state.auth.is_some() && self.io.facts_gen() != self.state.facts_seen {
            self.rederive();
            changed = true;
        }
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
        let (adapter, io, auth) = (Arc::clone(&self.adapter), Arc::clone(&self.io), auth.clone());
        adapter.list_busy.store(true, Ordering::SeqCst);
        let guard = Release(Arc::clone(&adapter), |a| &a.list_busy);
        let spawned = (self.spawn)("wlist", Box::new(move || {
            let _guard = guard;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_list(&*io, &auth, tag, (&known, known_gen), &mut |l| adapter.post(l));
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

    #[cfg(any(test, feature = "test-support"))]
    pub fn pump(&mut self) -> bool {
        self.pump_with_gate(plx_machine::landgate::fixture_gate())
    }

    #[cfg(test)]
    fn with_spawn_for_test(mut self, spawn: Spawn) -> Self {
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

    /// Is this catalog id on the watchlist, intents in flight included?
    pub fn is_member(&self, hex: &[u8; 12]) -> bool {
        self.store.state.members.contains(hex)
    }

    /// The service's list length, once it said, for the skeleton cells past the loaded tail.
    pub fn total(&self) -> Option<u32> {
        self.store.state.total
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
                self.pump_with_gate(&plx_machine::landgate::Gate::default());
            }
        }
        Handled::Yes
    }
}

#[cfg(test)]
mod tests {
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
        current: AtomicBool,
        generation: AtomicU32,
        roster_gen: AtomicU32,
        facts_gen: AtomicU32,
        finds: AtomicUsize,
        pages: AtomicUsize,
        /// Pages starting at or after this offset fail.
        fail_pages_from: AtomicU32,
        /// Copy preference per server (0 current, 1 owned, 2 share); absent = 2.
        ranks: Mutex<HashMap<ServerId, u8>>,
        /// Page starts requested, in order.
        starts: Mutex<Vec<u32>>,
        /// Entries per page the decoder "dropped": counted in `raw`, absent from `items`; also
        /// withholds `totalSize` while non-zero.
        dropped: AtomicU32,
    }

    impl Mock {
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
                current: AtomicBool::new(true),
                generation: AtomicU32::new(1),
                roster_gen: AtomicU32::new(1),
                facts_gen: AtomicU32::new(1),
                finds: AtomicUsize::new(0),
                pages: AtomicUsize::new(0),
                fail_pages_from: AtomicU32::new(u32::MAX),
                ranks: Mutex::new(HashMap::new()),
                starts: Mutex::new(Vec::new()),
                dropped: AtomicU32::new(0),
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
        fn capture_auth(&self) -> Option<WatchlistAuth> {
            Some(WatchlistAuth {
                client_id: "test-client".into(),
                credential: "test-credential".into(),
                user: Default::default(),
                generation: self.generation.load(Ordering::SeqCst),
            })
        }
        fn is_current(&self, a: &WatchlistAuth) -> bool {
            self.current.load(Ordering::SeqCst) && a.generation == self.generation.load(Ordering::SeqCst)
        }
        fn page(&self, _: &WatchlistAuth, start: u32, size: u32) -> Result<WatchPage, WatchlistError> {
            self.pages.fetch_add(1, Ordering::SeqCst);
            if start >= self.fail_pages_from.load(Ordering::SeqCst) {
                return Err(WatchlistError::Transport);
            }
            let all = self.items.lock().unwrap();
            self.starts.lock().unwrap().push(start);
            let mut items: Vec<WatchItem> = all.iter().skip(start as usize).take(size as usize).cloned().collect();
            let raw = items.len() as u32;
            let dropped = self.dropped.load(Ordering::SeqCst) as usize;
            let total = (dropped == 0).then_some(all.len() as u32);
            items.truncate(items.len().saturating_sub(dropped));
            Ok(WatchPage { items, total, start, raw })
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
            let held = self.held.lock().unwrap();
            Ok(guids
                .iter()
                .filter_map(|g| {
                    held.get(&(server, g.clone()))
                        .map(|rk| GuidMatch { guid: g.clone(), rating_key: rk.clone(), library_section_id: None })
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

    /// Open, then pump until nothing is left to land.
    fn load(s: &mut WatchlistStore) {
        s.run(WatchlistCmd::Open);
        for _ in 0..4 {
            s.pump();
        }
    }

    fn titles(s: &WatchlistStore) -> Vec<String> {
        let v = s.view();
        (0..v.len()).map(|i| v.row(i).unwrap().title.clone()).collect()
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
        mock.current.store(false, Ordering::SeqCst);
        s.pump();
        assert_ne!(s.state.epoch, epoch);
        assert!(s.view().is_empty());
        assert_eq!(s.view().status(), Status::Idle);
        assert!(!s.state.ledger.busy(), "the old profile's queued write is never sent to the new one");
        assert!(mock.writes.lock().unwrap().is_empty());
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
        s.run(WatchlistCmd::Open);
        // The first pump issues the read (stamped now); the press follows before it is landed.
        s.state.list_wanted = true;
        s.maybe_spawn();
        s.run(WatchlistCmd::Press { hex: hex_of(2), intent: Intent::Remove });
        s.run(WatchlistCmd::Press { hex: hex_of(7), intent: Intent::Add });
        let gate = plx_machine::landgate::fixture_gate();
        for _ in 0..3 {
            s.pump_with_gate(gate);
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
    fn a_refused_spawn_retries_after_n_frames_and_never_blocks() {
        let mock = three();
        let mut s = store(&mock).with_spawn_for_test(refuse);
        s.run(WatchlistCmd::Open);
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
        }, directory.view()));
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
        assert!(s.apply_view(&change("42", ViewFact::Facts { watched: false, unwatched: true, resume_ms: 5 })));
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
        s.run(WatchlistCmd::Open);
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
    fn no_credential_means_no_list() {
        struct NoAuth(Arc<Mock>);
        impl Reach for NoAuth {
            fn reachable(&self, s: ServerId) -> bool { self.0.reachable(s) }
            fn roster(&self) -> Vec<ServerId> { self.0.roster() }
        }
        impl WatchlistIo for NoAuth {
            fn capture_auth(&self) -> Option<WatchlistAuth> { None }
            fn is_current(&self, a: &WatchlistAuth) -> bool { self.0.is_current(a) }
            fn page(&self, a: &WatchlistAuth, s: u32, n: u32) -> Result<WatchPage, WatchlistError> { self.0.page(a, s, n) }
            fn released(&self, a: &WatchlistAuth) -> Result<Vec<WatchItem>, WatchlistError> { self.0.released(a) }
            fn write(&self, a: &WatchlistAuth, h: &str, i: Intent) -> Result<(), WatchlistError> { self.0.write(a, h, i) }
            fn find(&self, s: ServerId, k: GuidKind, g: &[String]) -> Result<Vec<GuidMatch>, GuidLookupError> { self.0.find(s, k, g) }
            fn roster_gen(&self) -> u32 { 0 }
            fn facts_gen(&self) -> u32 { 0 }
            fn today(&self) -> Option<String> { None }
        }
        let mock = three();
        let mut s = WatchlistStore::with_io(Arc::new(NoAuth(mock.clone()))).with_spawn_for_test(inline);
        load(&mut s);
        assert_eq!(s.view().status(), Status::NoAccount);
        assert_eq!(mock.pages.load(Ordering::SeqCst), 0);
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
}
