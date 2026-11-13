//! The physically owned Watchlist model and its worker adapter (plan §1.2, §1.4, §1.5, §1.11).
//!
//! The store emits addressed List, Rematch and Delta streams plus Resolve and serial Write
//! jobs. The app runner executes them through boot-selected ports; the step only consumes
//! captured facts and addressed landings. Streams post in order, then owe one acknowledged
//! terminal. A profile switch rotates the epoch, cancels queued jobs and retires old mailboxes.
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
    Failure, Landed, Tag, What, WatchlistAuth, WatchlistInput,

};
use plx_machine::machine::{Cx, Effects, Handled, Host, Machine};
use plx_plex::plex::session::CurrentProfile;
use plx_plex::plex::{ListingFacts, ServerId};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use super::StoreEv;
#[cfg(any(test, feature = "test-support"))]
use crate::watchlist::WatchlistIo;

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


/// A single host-captured roster and date, shared by every decision in one pass.
use crate::watchlist::model::Reach;
use super::watchlist_job::{Snapshot, WatchlistStreamPort, ListReq, RematchReq, DeltaReq, AuthValue};
use super::watchlist_job::stream::{StreamMailbox, StreamLanding, StreamEvent, StreamEnd};
use plx_machine::machine::{Addr, MachineId, RequestId};
#[derive(Clone, Copy, PartialEq)]
enum ListRead { HoldingRows, AwaitingPage, Publishing }
#[derive(Clone, Copy, PartialEq)]
enum StreamKind { List(ListRead), Rematch, Delta }
impl StreamKind {
    fn is_list(self) -> bool { matches!(self, Self::List(_)) }
}
pub struct WatchlistFacts<'a> {
    pub keys: &'a plx_plex::plex::ClientKeys,
    pub today: Option<&'a str>,
}

impl crate::watchlist::model::Reach for WatchlistFacts<'_> {
    fn reachable(&self, server: ServerId) -> bool {
        matches!(self.keys.facts(server).and_then(|f| f.source),
            Some((Some(plx_plex::plex::probe::Outcome::Reachable), _)))
    }

    fn roster(&self) -> Vec<ServerId> {
        self.keys.slots().collect()
    }

    fn copy_rank(&self, server: ServerId) -> u8 {
        if server != ServerId::UNSET && self.keys.current() == server {
            0
        } else if self.keys.facts(server).and_then(|f| f.described).is_some_and(|f| f.owned) {
            1
        } else {
            2
        }
    }
}

/// A worker's mailbox. Landings queue in post order; the pump drains them all.
#[derive(Default)]
pub struct WatchlistAdapter {
    mail: Mutex<VecDeque<Landed>>,

    /// Card rows asked to be resolved by `(server, rk)` (`WatchlistStore::want_hex`), spawned by the
    /// next pump. Behind a lock because a menu asks from `&self`.
    wanted: Mutex<Vec<(ServerId, String)>>,
}

impl WatchlistAdapter {
    #[cfg(any(test, feature = "test-support"))]
    fn post(&self, landed: Landed) {
        self.mail.lock().unwrap_or_else(|e| e.into_inner()).push_back(landed);

    }

    fn take_all(&self) -> Vec<Landed> {
        self.mail.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
    }
}

#[cfg(any(test, feature = "test-support"))]
type Spawn = fn(&str, Box<dyn FnOnce() + Send + 'static>) -> bool;



/// Main-thread state.
struct WatchlistState {
    epoch: u32,
    req: u32,
    list_claim: Option<u32>,
    write_claim: Option<u32>,
    write_port: Option<super::watchlist_job::WritePort>,

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
    resolving: HashMap<(ServerId, String), (Tag, super::watchlist_job::ResolvePort)>,
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
            list_claim: None,
            write_claim: None,
            write_port: None,

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

            ledger: IntentLedger::new(),
            rows_stamp: 0,
            read_stamp: 0,
            sort,
            filter,
            members: HashSet::new(),
            resolved: HashMap::new(),
            resolving: HashMap::new(),
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
    resolve_profile: Option<u32>,
    resolve_mailbox: Arc<super::watchlist_job::ResolveMailbox>,
    write_mailbox: Arc<super::watchlist_job::WriteMailbox>,
    stream_mailbox: Arc<StreamMailbox<What>>,
    stream: Option<(StreamKind, WatchlistStreamPort)>,
    stream_keys: plx_plex::plex::ClientKeys,
    retired_streams: Vec<(WatchlistStreamPort, Arc<StreamMailbox<What>>)>,
    pending_delta: Option<(ServerId, Arc<Vec<(String, Kind)>>)>,
    rematch_wanted: bool,
    snapshot: Snapshot,
    jobs: Vec<super::StoreJob>,
    adapter: Arc<WatchlistAdapter>,
    #[cfg(any(test, feature = "test-support"))]
    io: Option<Arc<dyn WatchlistIo>>,
    #[cfg(any(test, feature = "test-support"))]
    spawn: Spawn,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for WatchlistStore {
fn default() -> Self {
        Self {
            state: WatchlistState::new(0, default_sort(), FilterSpec::default()),
            resolve_profile: None,
            resolve_mailbox: Arc::new(Default::default()),
            write_mailbox: Arc::new(Default::default()),
            stream_mailbox: Arc::new(Default::default()), stream: None, pending_delta: None, rematch_wanted: false,
            stream_keys: Default::default(), retired_streams: Vec::new(),
            snapshot: Snapshot::default(), jobs: Vec::new(), adapter: Arc::new(Default::default()),
            #[cfg(any(test, feature = "test-support"))]
            io: None,
            #[cfg(any(test, feature = "test-support"))]
            spawn: |_, _| false,
            notice_gen: AtomicU32::new(0), notice_dirty: AtomicBool::new(false),
        }
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
    /// Fixture transport; the pure store still emits the production job values.
#[cfg(any(test, feature = "test-support"))]
    pub fn with_io(io: Arc<dyn WatchlistIo>) -> Self {
        Self { snapshot: Snapshot::scripted(&*io), io: Some(io), ..Self::default() }
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
                Asked::Held(m, facts) if self.snapshot.reachable(*sid) => Some((self.snapshot.copy_rank(*sid), (m, facts))),
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
        for (_, port) in self.state.resolving.values() { port.cancel_queued(); }
        if let Some(port) = &self.state.write_port { port.cancel_queued(); }
        if let Some((_, port)) = &self.stream {
            port.cancel();
            self.retired_streams.push((port.clone(), Arc::clone(&self.stream_mailbox)));
        }
        for terminal in self.stream_mailbox.take() { terminal.ack_terminal(); }
        self.stream = None;
        self.pending_delta = None;
        self.rematch_wanted = false;
        self.stream_mailbox = Arc::new(Default::default());
        self.jobs.clear();
        self.write_mailbox = Arc::new(Default::default());
        self.resolve_mailbox = Arc::new(Default::default());
        self.resolve_profile = None;
        let epoch = self.state.epoch.wrapping_add(1);
        let req = self.state.req;
        let (sort, filter) = (self.state.sort, self.state.filter);
        self.adapter = Arc::new(WatchlistAdapter::default());
        self.state = WatchlistState::new(epoch, sort, filter);
        self.state.req = req;
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
        let io = &self.snapshot;
        // Released read failed and the row's date cannot say (none, or no clock): its release is
        // unknown, so a row every server answered absent stays Unknown (plan §1.3), not washed.
        let release_blind = s.released.is_none();
        let today = io.today.as_deref();
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

        self.refresh();
    }

    /// Settle a row's `released` against the released set; when that read failed, against today.
    fn settle_released(&self, rows: &mut [WatchRow]) {
        let today = self.snapshot.today.clone();
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
        let claim = match &landed.what {
            What::Written { .. } => self.state.write_claim,
            _ => self.state.list_claim,
        };
        if claim != Some(landed.tag.req) {
            return false;
        }
        match &landed.what {
            What::ListDone => {},
            What::Written { .. } => self.state.write_claim = None,
            _ => {}
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
                    if self.state.rows.is_empty() {
                        // First load: the page shows as soon as it lands.
                        self.state.truncated = false;
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
                } else if start > 0 && !had_rows {
                    // Only a first load changes the rows on a later-page failure. A failed
                    // reread retains the completeness of the rows already on screen.
                    self.state.truncated = true;
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
                    self.state.truncated = false;
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
    pub fn pump_with_gate(&mut self, gate: &plx_machine::landgate::Gate, profile: &CurrentProfile, keys: &plx_plex::plex::ClientKeys) -> bool {
        self.pump_with_gate_and_keys(gate, profile, keys)
    }

pub fn pump_with_gate_and_keys(&mut self, gate: &plx_machine::landgate::Gate, profile: &CurrentProfile, keys: &plx_plex::plex::ClientKeys) -> bool {
        self.pump_with_snapshot(gate, profile, Snapshot::new(keys, self.snapshot.today.clone()))
    }

    pub fn observe_snapshot(&mut self, input: Snapshot) {
        let changed = self.snapshot != input;
        self.snapshot = input;
        if changed && self.state.auth.is_some() { self.rederive(); self.bump(); }
    }
    pub fn pump_with_snapshot(&mut self, gate: &plx_machine::landgate::Gate, profile: &CurrentProfile, input: Snapshot) -> bool {
        let mut changed = false;
        if self.resolve_profile.is_some_and(|g| g != profile.generation)
            || self.state.auth.as_ref().is_some_and(|a| !a.is_current(profile)) {
            self.reset(); changed = true;
        }
        self.resolve_profile = Some(profile.generation);
        self.retired_streams.retain(|(port, mail)| {
            // A terminal phase guarantees its post is in the queue; sample it before draining.
            let running = port.is_running();
            for terminal in mail.take() { terminal.ack_terminal(); }
            running
        });
        let facts_changed = self.snapshot != input;
        self.snapshot = input;
        self.state.list_retry = self.state.list_retry.saturating_sub(1);
        self.state.write_retry = self.state.write_retry.saturating_sub(1);
        changed |= self.pump_streams(gate);
        for landed in self.adapter.take_all() { changed |= self.apply(landed); }
        if self.state.auth.is_some() && facts_changed { self.rederive(); changed = true; }
        changed |= self.pump_writes(gate, profile.generation);
        let snapshot = self.snapshot.clone();
        changed |= self.pump_resolves(gate, profile.generation, &snapshot.keys);
        self.maybe_spawn();
        if changed { self.bump(); }
        changed
    }

    fn pump_streams(&mut self, gate: &plx_machine::landgate::Gate) -> bool {
        let port = self.stream.as_ref().map(|(_, p)| p.clone());
        let mailbox = Arc::clone(&self.stream_mailbox);
        let batch = super::take_landings_owed(gate, super::StoreId::Watchlist,
            |_: &[StreamLanding<What>]| port.as_ref().is_some_and(|p| p.is_running()), || mailbox.take());
        let mut changed = false;
        for landed in batch {
            landed.ack_terminal();
            let expected = Addr { to: MachineId::Store(super::StoreId::Watchlist.ord()), req: RequestId(landed.tag.req) };
            if landed.addr != expected || self.state.list_claim != Some(landed.tag.req)
                || landed.tag.epoch != self.state.epoch
                || !self.state.auth.as_ref().is_some_and(|a| a.generation == landed.tag.profile_gen) { continue; }
            match landed.event {
                StreamEvent::Post(mut what) => {
                    if matches!(&what, What::Page { .. }) {
                        if let Some((StreamKind::List(progress), _)) = &mut self.stream {
                            if *progress == ListRead::AwaitingPage { *progress = ListRead::Publishing; }
                        }
                    }
                    let keys = &self.stream_keys;
                    let now = &self.snapshot.keys;
                    let retain = |answers: &mut Vec<crate::watchlist::Answer>| answers.retain(|(_, sid, _)| keys.key(*sid) == now.key(*sid));
                    match &mut what {
                        What::Page { answers, .. } | What::Delta { answers } => retain(answers),
                        What::Rematch { answers, .. } => {
                            if keys != now { continue; }
                            retain(answers);
                        }
                        _ => {}
                    }
                    changed |= self.apply(Landed { tag: landed.tag, what });
                }
                StreamEvent::End(end) => {
                    let kind = self.stream.as_ref().map(|(k, _)| *k);
                    match end {
                        StreamEnd::NotStarted => {
                            self.state.list_retry = RETRY_FRAMES;
                            if kind.is_some_and(StreamKind::is_list) { self.state.list_wanted = true; }
                        }
                        StreamEnd::Done if kind.is_some_and(StreamKind::is_list) => changed |= self.apply(Landed { tag: landed.tag, what: What::ListDone }),
                        StreamEnd::Panicked | StreamEnd::Cancelled => {
                            self.state.list_retry = RETRY_FRAMES;
                            if let Some(StreamKind::List(progress)) = kind {
                                let start = u32::from(progress == ListRead::Publishing);
                                changed |= self.apply(Landed { tag: landed.tag, what: What::PageFailed { start, why: Failure::Other } });
                                changed |= self.apply(Landed { tag: landed.tag, what: What::ListDone });
                            }
                        }
                        _ => {}
                    }
                    let stale_keys = self.stream_keys != self.snapshot.keys;
                    if kind == Some(StreamKind::Delta) && end == StreamEnd::Done && !stale_keys { self.pending_delta = None; }
                    if stale_keys || end != StreamEnd::Done {
                        self.state.list_retry = RETRY_FRAMES;
                        if kind == Some(StreamKind::Rematch) || (stale_keys && kind.is_some_and(StreamKind::is_list)) { self.rematch_wanted = true; }
                    }
                    self.state.list_claim = None;
                    self.stream = None;
                }
            }
        }
        changed
    }

    /// Settle launch refusals in the emitting drain without advancing retry clocks or emitting work.
    pub fn land_stream_results(&mut self, gate: &plx_machine::landgate::Gate, profile: &CurrentProfile) -> bool {
        if self.state.auth.as_ref().is_some_and(|a| !a.is_current(profile)) { self.reset(); self.bump(); return true; }
        // Admission refusals settle before ordinary landing gates, in the emitting drain.
        for landed in self.stream_mailbox.take_not_started() {
            landed.ack_terminal();
            let expected = Addr { to: MachineId::Store(super::StoreId::Watchlist.ord()), req: RequestId(landed.tag.req) };
            if landed.addr != expected || self.state.list_claim != Some(landed.tag.req)
                || landed.tag.epoch != self.state.epoch
                || !self.state.auth.as_ref().is_some_and(|a| a.generation == landed.tag.profile_gen) { continue; }
            match self.stream.as_ref().map(|(kind, _)| *kind) {
                Some(StreamKind::List(_)) => self.state.list_wanted = true,
                Some(StreamKind::Rematch) => self.rematch_wanted = true,
                _ => {}
            }
            self.state.list_retry = RETRY_FRAMES;
            self.state.list_claim = None;
            self.stream = None;
        }
        if !self.stream_mailbox.has_mail() { return false; }
        let changed = self.pump_streams(gate);
        if changed { self.bump(); }
        changed
    }

fn maybe_spawn(&mut self) {
        if self.state.auth.is_none() { return; }
        let write = if self.state.write_claim.is_none() && self.state.write_retry == 0 {
            self.state.ledger.next_write()
        } else { None };
        let stream_owed = self.state.list_claim.is_none() && self.state.list_retry == 0
            && (self.state.list_wanted || self.pending_delta.is_some()
                || (!self.state.rows.is_empty() && (self.rematch_wanted || self.snapshot.roster_gen != self.state.table.roster_gen)));
        if write.is_none() && !stream_owed { return; }
        let auth = self.state.auth.clone().expect("a pending Watchlist job has captured auth");
        if let Some(write) = write { self.spawn_write(&auth, write); }
        if !stream_owed { return; }
        if self.state.list_wanted { self.spawn_list(&auth); }
        else if let Some((sid, asks)) = self.pending_delta.clone() { self.emit_delta(&auth, sid, asks); }
        else if !self.state.rows.is_empty() && (self.rematch_wanted || self.snapshot.roster_gen != self.state.table.roster_gen) { self.spawn_rematch(&auth); }
    }

    fn spawn_write(&mut self, auth: &WatchlistAuth, write: PendingWrite) {
        use plx_machine::machine::{Addr, MachineId, RequestId};
        use super::watchlist_job::{WritePort, WriteReq};
        self.state.req = self.state.req.checked_add(1).expect("watchlist request ids exhausted");
        self.state.write_claim = Some(self.state.req);
        self.state.writes_seen = self.state.writes_seen.wrapping_add(1);
        self.state.list_etag = None;
        let tag = self.current_tag(auth);
        let addr = Addr { to: MachineId::Store(super::StoreId::Watchlist.ord()), req: RequestId(tag.req) };
        let req = WriteReq { tag, auth: auth.clone(), write };
        let port = WritePort::new(addr, &req, Arc::clone(&self.write_mailbox));
        self.state.write_port = Some(port.clone());
        self.jobs.push(super::StoreJob::WatchlistWrite(plx_machine::job::Job { addr, req }, port));
    }

    fn pump_writes(&mut self, gate: &plx_machine::landgate::Gate, profile_gen: u32) -> bool {
        use plx_machine::machine::{Addr, MachineId, RequestId};
        use super::watchlist_job::WriteEnd;
        let claim = self.state.write_claim;
        let expected = claim.map(|req| Addr { to: MachineId::Store(super::StoreId::Watchlist.ord()), req: RequestId(req) });
        let epoch = self.state.epoch;
        let answers = super::take_landings_owed(gate, super::StoreId::Watchlist, |got: &[super::watchlist_job::WriteLanding]| {
            expected.is_some() && !got.iter().any(|landing| Some(landing.addr) == expected
                && landing.tag.req == landing.addr.req.0 && landing.tag.epoch == epoch
                && landing.tag.profile_gen == profile_gen)
        }, || self.write_mailbox.take());
        let mut changed = false;
        for landing in answers {
            if Some(landing.addr) != expected || self.state.write_claim != Some(landing.tag.req)
                || landing.addr.req.0 != landing.tag.req || landing.tag.epoch != self.state.epoch
                || landing.tag.profile_gen != profile_gen { continue; }
            self.state.write_port = None;
            match landing.end {
                WriteEnd::NotStarted { write } => {
                    self.state.write_claim = None;
                    self.state.ledger.requeue(write);
                    self.state.write_retry = RETRY_FRAMES;
                }
                WriteEnd::Written { write, outcome } => {
                    changed |= self.apply(Landed { tag: landing.tag, what: What::Written { write, outcome } });
                }
            }
        }
        changed
    }

    /// Consume an immediate runner refusal without advancing the frame retry timer.
    pub fn land_write_results(&mut self, gate: &plx_machine::landgate::Gate, profile: &CurrentProfile) -> bool {
        for landed in self.write_mailbox.take_not_started() {
            let expected = Addr { to: MachineId::Store(super::StoreId::Watchlist.ord()), req: RequestId(landed.tag.req) };
            if landed.addr != expected || self.state.write_claim != Some(landed.tag.req)
                || landed.tag.epoch != self.state.epoch || landed.tag.profile_gen != profile.generation { continue; }
            let super::watchlist_job::WriteEnd::NotStarted { write } = landed.end else { unreachable!() };
            self.state.write_claim = None;
            self.state.write_port = None;
            self.state.ledger.requeue(write);
            self.state.write_retry = RETRY_FRAMES;
        }
        if !self.write_mailbox.has_mail() { return false; }
        let changed = self.pump_writes(gate, profile.generation);
        if changed { self.bump(); }
        changed
    }

    /// The guids the table already holds an answer for from every reachable server, so a reread
    /// matches only the new ones (the worker ignores this when the roster has moved since).
    fn matched_guids(&self) -> HashSet<String> {
        let asked: Vec<ServerId> = self.snapshot.roster().into_iter().filter(|s| self.snapshot.reachable(*s)).collect();
        self.state
            .table
            .by_guid
            .iter()
            .filter(|(_, answers)| asked.iter().all(|s| answers.iter().any(|(a, _)| a == s)))
            .map(|(g, _)| g.clone())
            .collect()
    }

fn spawn_list(&mut self, auth: &WatchlistAuth) {
        self.state.req = self.state.req.checked_add(1).expect("watchlist request ids exhausted");
        self.state.list_claim = Some(self.state.req);
        let tag = self.current_tag(auth);
        self.state.read_stamp = self.state.ledger.stamp();
        let quiet = !self.state.ledger.busy();
        let etag = self.state.list_etag.clone().filter(|_| quiet && !self.state.rows.is_empty() && !self.state.truncated);
        self.state.read_etag = None;
        self.state.read_writes = quiet.then_some(self.state.writes_seen);
        let req = ListReq { tag, input: self.snapshot.clone(), auth: AuthValue(auth.clone()),
            known: self.matched_guids(), known_gen: self.state.table.roster_gen, etag };
        let progress = if self.state.rows.is_empty() { ListRead::AwaitingPage } else { ListRead::HoldingRows };
        let (addr, port) = self.stream_port(tag, StreamKind::List(progress));
        self.state.list_wanted = false;
        self.jobs.push(super::StoreJob::WatchlistList(plx_machine::job::Job { addr, req }, port));
    }

fn spawn_rematch(&mut self, auth: &WatchlistAuth) {
        self.state.req = self.state.req.checked_add(1).expect("watchlist request ids exhausted");
        self.state.list_claim = Some(self.state.req);
        let tag = self.current_tag(auth);
        self.rematch_wanted = false;
        let req = RematchReq { tag, input: self.snapshot.clone(), rows: self.state.rows.iter().map(|r| (r.guid.clone(), r.kind)).collect() };
        let (addr, port) = self.stream_port(tag, StreamKind::Rematch);
        self.jobs.push(super::StoreJob::WatchlistRematch(plx_machine::job::Job { addr, req }, port));
    }

    fn stream_port(&mut self, tag: Tag, kind: StreamKind) -> (Addr, WatchlistStreamPort) {
        let addr = Addr { to: MachineId::Store(super::StoreId::Watchlist.ord()), req: RequestId(tag.req) };
        let port = WatchlistStreamPort::new(addr, tag, Arc::clone(&self.stream_mailbox));
        self.stream_keys = self.snapshot.keys.clone();
        self.stream = Some((kind, port.clone()));
        (addr, port)
    }
    fn emit_delta(&mut self, auth: &WatchlistAuth, sid: ServerId, rows: Arc<Vec<(String, Kind)>>) {
        self.state.req = self.state.req.checked_add(1).expect("watchlist request ids exhausted");
        self.state.list_claim = Some(self.state.req);
        let tag = self.current_tag(auth);
        let req = DeltaReq { tag, input: self.snapshot.clone(), sid, rows };
        let (addr, port) = self.stream_port(tag, StreamKind::Delta);
        self.jobs.push(super::StoreJob::WatchlistDelta(plx_machine::job::Job { addr, req }, port));
    }

    /// Has the list been read and shown? A library change is followed only once there are rows to
    /// follow: before that, the first read matches every title against the library as it is then.
    pub fn is_ready(&self) -> bool {
        self.state.auth.is_some() && self.state.status == Status::Ready
    }

    /// Is a list read out on a worker right now? Its matching may have asked a server before a
    /// library change that has just arrived, so a change must be kept for it; a read not yet
    /// started will ask after the change and needs no note of it.
pub fn read_in_flight(&self) -> bool { self.state.list_claim.is_some() }

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
        if !self.is_ready() { return false; }
        if asks.is_empty() || !self.snapshot.reachable(sid) { return true; }
        if self.state.list_wanted || self.state.list_claim.is_some() || self.state.list_retry > 0 || self.pending_delta.is_some() { return false; }
        let Some(auth) = self.state.auth.clone() else { return false };
        let asks = Arc::new(asks);
        self.pending_delta = Some((sid, Arc::clone(&asks)));
        self.emit_delta(&auth, sid, asks);
        true
    }

    /// A card row carried no catalog id: ask for it to be read off the item's own server record
    /// (`client.metadata(rk).guid`, on a worker, at the next pump). Idempotent per `(server, rk)`.
    /// The answer is [`WatchlistView::resolved_hex`]; a menu opened after it lands offers the row.
    pub fn want_hex(&self, sid: ServerId, rk: &str) {
        let key = (sid, rk.to_owned());
        if self.state.resolved.contains_key(&key) || self.state.resolving.contains_key(&key) {
            return;
        }
        let mut wanted = self.adapter.wanted.lock().unwrap_or_else(|e| e.into_inner());
        if !wanted.contains(&key) {
            wanted.push(key);
        }
    }

    /// Spawn a worker per asked-for record and land the answers. Needs no account: the read is
    /// the user's own server's.

    pub fn land_resolve_refusals(&mut self, profile: &CurrentProfile) {
        for landed in self.resolve_mailbox.take_not_started() {
            let expected = Addr { to: MachineId::Store(super::StoreId::Watchlist.ord()), req: RequestId(landed.tag.req) };
            let key = (landed.sid, landed.rk);
            if landed.addr == expected && landed.tag.epoch == self.state.epoch && landed.tag.profile_gen == profile.generation
                && self.state.resolving.get(&key).is_some_and(|(tag, _)| *tag == landed.tag) {
                self.state.resolving.remove(&key);
            }
        }
    }
    fn pump_resolves(&mut self, gate: &plx_machine::landgate::Gate, profile_gen: u32,
        keys: &plx_plex::plex::ClientKeys) -> bool {
        use plx_machine::machine::{Addr, MachineId, RequestId};
        use super::watchlist_job::{ResolveEnd, ResolvePort, ResolveReq};
        let claims = &self.state.resolving;
        let answers = super::take_landings_owed(gate, super::StoreId::Watchlist, |got: &[super::watchlist_job::ResolveLanding]| {
            got.iter().filter(|landing| claims.get(&(landing.sid, landing.rk.clone()))
                .is_some_and(|(tag, _)| *tag == landing.tag && landing.addr.req.0 == tag.req)).count() < claims.len()
        }, || self.resolve_mailbox.take());
        let mut changed = false;
        for landing in answers {
            let key = (landing.sid, landing.rk);
            if !self.state.resolving.get(&key).is_some_and(|(tag, _)| *tag == landing.tag
                && landing.addr.req.0 == tag.req && tag.epoch == self.state.epoch
                && tag.profile_gen == profile_gen) { continue; }
            self.state.resolving.remove(&key);
            if let ResolveEnd::Answer(Some(hex)) = landing.end {
                if landing.key != keys.key(key.0) { continue; }
                self.state.resolved.insert(key, hex);
                changed = true;
            }
        }
        let wanted: Vec<_> = self.adapter.wanted.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        for (sid, rk) in wanted {
            let key = (sid, rk.clone());
            if self.state.resolved.contains_key(&key) || self.state.resolving.contains_key(&key) { continue; }
            self.state.req = self.state.req.checked_add(1).expect("watchlist request ids exhausted");
            let tag = Tag { profile_gen, epoch: self.state.epoch, req: self.state.req };
            let addr = Addr { to: MachineId::Store(super::StoreId::Watchlist.ord()), req: RequestId(tag.req) };
            let req = ResolveReq { tag, keys: keys.clone(), sid, rk };
            let port = ResolvePort::new(addr, &req, Arc::clone(&self.resolve_mailbox));
            self.state.resolving.insert(key, (tag, port.clone()));
            self.jobs.push(super::StoreJob::WatchlistResolve(plx_machine::job::Job { addr, req }, port));
        }
        changed
    }

    #[must_use]
    pub fn take_jobs(&mut self) -> Vec<super::StoreJob> { std::mem::take(&mut self.jobs) }

    pub fn has_jobs(&self) -> bool { !self.jobs.is_empty() }

    /// [`Self::pump_with_gate`] for a test about something else: the fixture gate, and the seat
    /// [`WatchlistInput::for_test`] hands out at generation 1 (the one a store's tests open under).
#[cfg(any(test, feature = "test-support"))]
    pub fn pump(&mut self) -> bool {
        let gate = plx_machine::landgate::fixture_gate();
        let profile = WatchlistInput::profile_for_test(1);
        let input = self.io.as_ref().map_or_else(|| self.snapshot.clone(), |io| Snapshot::scripted(&**io));
        let changed = self.pump_with_snapshot(gate, &profile, input);
        let (streams, deferred): (Vec<_>, Vec<_>) = std::mem::take(&mut self.jobs).into_iter().partition(|j|
            matches!(j, super::StoreJob::WatchlistList(..) | super::StoreJob::WatchlistRematch(..) | super::StoreJob::WatchlistDelta(..)));
        self.jobs = streams;
        self.dispatch_jobs_for_test();
        self.jobs.extend(deferred);
        self.land_stream_results(gate, &profile) || changed
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
    pub fn pump_scripted_with_gate(&mut self, gate: &plx_machine::landgate::Gate, profile: &CurrentProfile) -> bool {
        let input = self.io.as_ref().map_or_else(|| self.snapshot.clone(), |io| Snapshot::scripted(&**io));
        let changed = self.pump_with_snapshot(gate, profile, input);
        self.drive_jobs_for_test(gate, profile) || changed
    }
#[cfg(any(test, feature = "test-support"))]
    fn drive_jobs_for_test(&mut self, gate: &plx_machine::landgate::Gate, profile: &CurrentProfile) -> bool {
        self.dispatch_jobs_for_test();
        let stream = self.land_stream_results(gate, profile);
        self.land_write_results(gate, profile) || stream
    }
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn dispatch_jobs_for_test(&mut self) {
        use plx_machine::job::Port;
        use super::watchlist_job::{finish_list_job, finish_rematch_job, finish_delta_job, finish_write_job};
        let Some(io) = self.io.clone() else { return };
        for job in std::mem::take(&mut self.jobs) {
            match job {
                super::StoreJob::WatchlistList(job, port) => {
                    let (addr, refusal, io) = (job.addr, port.clone(), io.clone());
                    if !(self.spawn)("wlist", Box::new(move || finish_list_job(&port, addr, job.req, &*io))) { refusal.refused(addr); }
                }
                super::StoreJob::WatchlistRematch(job, port) => {
                    let (addr, refusal, io) = (job.addr, port.clone(), io.clone());
                    if !(self.spawn)("wlist", Box::new(move || finish_rematch_job(&port, addr, job.req, &*io))) { refusal.refused(addr); }
                }
                super::StoreJob::WatchlistDelta(job, port) => {
                    let (addr, refusal, io) = (job.addr, port.clone(), io.clone());
                    if !(self.spawn)("wlist", Box::new(move || finish_delta_job(&port, addr, job.req, &*io))) { refusal.refused(addr); }
                }
                super::StoreJob::WatchlistWrite(job, port) => {
                    let (addr, refusal, io) = (job.addr, port.clone(), io.clone());
                    if !(self.spawn)("wlist-write", Box::new(move || {
                        if port.start() { finish_write_job(&port, addr, job.req, &*io); }
                    })) { refusal.refused(addr); }
                }
                other => self.jobs.push(other),
            }
        }
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
        let input = &self.store.snapshot;
        input.roster().into_iter().filter(|s| input.reachable(*s)).min_by_key(|s| input.copy_rank(*s))
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
        panic_writes: AtomicBool,
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
                panic_writes: AtomicBool::new(false),
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
            assert!(!self.panic_writes.load(Ordering::SeqCst), "scripted write panic");
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


    fn pump_scripted(s: &mut WatchlistStore) -> bool {
        let profile = crate::watchlist::WatchlistInput::profile_for_test(1);
        pump_handed(s, &profile)
    }

fn pump_handed(s: &mut WatchlistStore, profile: &CurrentProfile) -> bool {
        s.pump_scripted_with_gate(plx_machine::landgate::fixture_gate(), profile)
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
        let mut s = store(&mock);
        s.state.list_claim = Some(1);
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
        // Keep a genuine list claim pending; its own request id does not authorize a different profile.
        s.run(WatchlistCmd::Reread);
        s.maybe_spawn();
        let auth = s.state.auth.clone().unwrap();
        let stale = Tag { profile_gen: auth.generation + 1, epoch: s.state.epoch, req: s.state.list_claim.unwrap() };
        assert!(!s.apply(Landed { tag: stale, what: ghost_page() }), "profile is checked against the live claim");
        pump_scripted(&mut s);
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"], "a stale generation never lands");
        // A matching profile alone is insufficient: the request must also own a live claim.
    }

    #[test]
    fn an_unclaimed_request_in_the_current_epoch_cannot_land() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        let auth = s.state.auth.clone().unwrap();
        let unclaimed = Tag { profile_gen: auth.generation, epoch: s.state.epoch, req: 99 };
        s.adapter_for_test().post_for_test(Landed { tag: unclaimed, what: ghost_page() });
        pump_scripted(&mut s);
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"], "an unclaimed request must not mutate rows");
    }


    #[test]
    fn captured_key_only_roster_never_invents_reachability_or_preference() {
        let sid = ServerId::from_raw(7);
        let key = plx_plex::plex::ClientKey { instance: 1, token: 2, grant: 3 };
        let keys = plx_plex::plex::ClientKeys::from_keys([(sid, key)]);
        let facts = WatchlistFacts { keys: &keys, today: None };
        assert_eq!(facts.roster(), [sid]);
        assert!(!facts.reachable(sid));
        assert_eq!(facts.copy_rank(sid), 2);
        assert_eq!(facts.copy_rank(ServerId::UNSET), 2);
    }


    #[test]
    fn captured_roster_keeps_order_reachability_and_copy_preference() {
        use plx_plex::plex::{self, ClientKeys, ServerFacts};
        use plx_plex::plex::probe::Outcome;
        struct ResetRegistry;
        impl Drop for ResetRegistry {
            fn drop(&mut self) { plex::reset_servers_for_test(); }
        }
        let _lock = plx_base::testlock::serial();
        plex::reset_servers_for_test();
        let _reset = ResetRegistry;
        let current = plex::register_for_test("watchlist-current", "127.0.0.1", 9, "test-token", "test-client");
        let owned = plex::register_for_test("watchlist-owned", "127.0.0.1", 9, "test-token", "test-client");
        let shared = ServerId::from_raw(22);
        let a = plex::client_for(current).expect("registered current client");
        let b = plex::client_for(owned).expect("registered owned client");
        let description: &'static ServerFacts = Box::leak(Box::new(ServerFacts {
            name: String::new(), handle: String::new(), owned: true, home: false, owner_id: 0,
        }));
        let mut keys = ClientKeys::default();
        keys.refill_roster(current, 4, 5, [shared, owned, current],
            |sid| if sid == current { Some(a) } else if sid == owned { Some(b) } else { None },
            |sid| (sid == owned).then_some(description),
            |sid| (sid == current).then_some(Outcome::Reachable));
        let facts = WatchlistFacts { keys: &keys, today: Some("2026-10-15") };
        assert_eq!(facts.roster(), [shared, owned, current]);
        assert!(facts.reachable(current));
        assert!(!facts.reachable(owned));
        assert!(!facts.reachable(shared));
        assert_eq!((facts.copy_rank(current), facts.copy_rank(owned), facts.copy_rank(shared)), (0, 1, 2));
        assert_eq!((keys.roster_gen(), keys.facts_gen()), (4, 5));
    }


    #[test]
    fn a_retired_request_cannot_land_into_a_new_list_claim() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        s.run(WatchlistCmd::Reread);
        s.maybe_spawn();
        let old = Tag { profile_gen: s.state.auth.as_ref().unwrap().generation, epoch: s.state.epoch, req: s.state.list_claim.unwrap() };
        pump_scripted(&mut s);
        s.run(WatchlistCmd::Reread);
        s.maybe_spawn();
        assert_ne!(s.state.list_claim, Some(old.req));
        assert!(!s.apply(Landed { tag: old, what: ghost_page() }), "the retired claim cannot mutate its successor");
        pump_scripted(&mut s);
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"]);
    }


    #[test]
    fn paced_gate_cannot_release_a_claim_before_it_drains() {
        thread_local! { static HELD: std::cell::RefCell<Vec<Box<dyn FnOnce() + Send>>> = Default::default(); }
        fn hold(_: &str, f: Box<dyn FnOnce() + Send + 'static>) -> bool {
            HELD.with(|h| h.borrow_mut().push(f));
            true
        }
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        s.spawn = hold;
        s.run(WatchlistCmd::Reread);
        s.maybe_spawn();
        let claim = s.state.list_claim;
        let gate = plx_machine::landgate::Gate::default();
        gate.arm_dump_paced(1);
        gate.dump_frame(0);
        let profile = crate::watchlist::WatchlistInput::profile_for_test(1);
        s.pump_scripted_with_gate(&gate, &profile);
        assert_eq!(s.state.list_claim, claim, "a held gate has not consumed the answer");
        let jobs: Vec<_> = HELD.with(|h| h.borrow_mut().drain(..).collect());
        assert_eq!(jobs.len(), 1);
        for job in jobs { job(); }
        s.spawn = inline;
        gate.dump_frame(1);
        s.pump_scripted_with_gate(&gate, &profile);
        assert!(s.state.list_claim.is_none());
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"]);
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
        pump_scripted(&mut s);
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
        pump_scripted(&mut s);
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
        mock.items.lock().unwrap().push(item(9, "Ghost"));
        s.run(WatchlistCmd::Reread);
        s.maybe_spawn();
        let old = Tag { profile_gen: 1, epoch: s.state.epoch, req: s.state.list_claim.unwrap() };
        assert!(!s.apply(Landed { tag: old, what: ghost_page() }), "the old generation cannot use the new claim");
        pump_handed(&mut s, &crate::watchlist::WatchlistInput::profile_for_test(2));
        assert!(titles(&s).contains(&"Ghost".to_string()), "the new generation’s actual claimed read lands");
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
        pump_scripted(&mut s);
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
        for _ in 0..3 {
            pump_handed(&mut s, &crate::watchlist::WatchlistInput::profile_for_test(1));
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
            pump_scripted(&mut s);
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
            pump_scripted(&mut s);
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
            pump_scripted(&mut s);
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
            pump_scripted(&mut s);
        }
        assert_eq!(*mock.writes.lock().unwrap(), [(id(3), Intent::Remove), (id(1), Intent::Remove)]);
    }

    #[test]
    fn a_silent_server_is_not_memoized_as_no_portable_id() {
        use plx_machine::job::Port;
        use super::super::watchlist_job::ResolveEnd;
        let mock = three();
        let sid = ServerId::from_raw(0);
        let mut s = store(&mock);
        let answer = |s: &mut WatchlistStore, end| {
            pump_scripted(s);
            let mut jobs = s.take_jobs();
            assert_eq!(jobs.len(), 1);
            let super::super::StoreJob::WatchlistResolve(job, port) = jobs.pop().unwrap() else { panic!("resolve job") };
            assert!(port.start());
            port.done(job.addr, end);
            pump_scripted(s);
        };
        s.want_hex(sid, "7");
        answer(&mut s, ResolveEnd::Answer(None));
        assert_eq!(s.view().resolved_hex(sid, "7"), None);
        s.want_hex(sid, "7");
        answer(&mut s, ResolveEnd::Answer(Some(Some([0xab; 12]))));
        assert_eq!(s.view().resolved_hex(sid, "7"), Some(Some([0xab; 12])));
        s.want_hex(sid, "8");
        answer(&mut s, ResolveEnd::Answer(Some(None)));
        assert_eq!(s.view().resolved_hex(sid, "8"), Some(None));
    }

    #[test]
    fn resolve_accountless_reset_and_stale_request_cannot_settle_the_new_claim() {
        use plx_machine::job::Port;
        use super::super::{StoreJob, watchlist_job::ResolveEnd};
        let mut s = store(&three());
        let sid = ServerId::from_raw(0);
        s.want_hex(sid, "7");
        pump_scripted(&mut s);
        let StoreJob::WatchlistResolve(old, old_port) = s.take_jobs().pop().unwrap() else { panic!("resolve job") };
        assert_eq!(old.req.tag.profile_gen, 1);
        s.run(WatchlistCmd::Reset);
        assert!(!old_port.start(), "a queued retired claim cannot start");
        s.want_hex(sid, "7");
        pump_scripted(&mut s);
        let StoreJob::WatchlistResolve(new, new_port) = s.take_jobs().pop().unwrap() else { panic!("resolve job") };
        assert_ne!(old.addr.req, new.addr.req);
        old_port.done(old.addr, ResolveEnd::Answer(Some(Some([1; 12]))));
        pump_scripted(&mut s);
        assert_eq!(s.view().resolved_hex(sid, "7"), None);
        assert!(new_port.start());
        new_port.done(new.addr, ResolveEnd::Answer(Some(Some([2; 12]))));
        pump_scripted(&mut s);
        assert_eq!(s.view().resolved_hex(sid, "7"), Some(Some([2; 12])));
    }

    #[test]
    fn resolve_refusal_panic_and_profile_change_release_their_own_claim() {
        use plx_machine::job::Port;
        use super::super::{StoreJob, watchlist_job::ResolveEnd};
        let mut s = store(&three());
        let sid = ServerId::from_raw(0);
        for end in [ResolveEnd::NotStarted, ResolveEnd::Panicked] {
            s.want_hex(sid, "7");
            pump_scripted(&mut s);
            let StoreJob::WatchlistResolve(job, port) = s.take_jobs().pop().unwrap() else { panic!("resolve job") };
            port.done(job.addr, end);
            pump_scripted(&mut s);
            assert_eq!(s.view().resolved_hex(sid, "7"), None);
        }
        s.want_hex(sid, "7");
        pump_scripted(&mut s);
        let StoreJob::WatchlistResolve(_, port) = s.take_jobs().pop().unwrap() else { panic!("resolve job") };
        let profile = WatchlistInput::profile_for_test(2);
        s.pump_with_gate(plx_machine::landgate::fixture_gate(), &profile, &plx_plex::plex::ClientKeys::default());
        assert!(!port.start(), "profile change cancels a parked accountless job");
        assert_eq!(s.view().resolved_hex(sid, "7"), None);
    }

    #[test]
    fn a_refused_spawn_retries_after_n_frames_and_never_blocks() {
        let mock = three();
        let mut s = store(&mock).with_spawn_for_test(refuse);
        s.run_for_test(WatchlistCmd::Open);
        pump_scripted(&mut s);
        assert_eq!(mock.pages.load(Ordering::SeqCst), 0);
        assert!(s.state.list_claim.is_none(), "the claim is released");
        s = s.with_spawn_for_test(inline);
        for _ in 0..(RETRY_FRAMES - 2) {
            pump_scripted(&mut s);
        }
        assert_eq!(mock.pages.load(Ordering::SeqCst), 0, "not before N frames");
        for _ in 0..4 {
            pump_scripted(&mut s);
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
            pump_scripted(&mut s);
        }
        assert_eq!(mock.pages.load(Ordering::SeqCst), 1, "a refused read is not asked again");
        assert_eq!(s.view().status(), Status::Failed(Failure::Other));
    }

    #[test]
    fn write_retired_started_request_cannot_clear_new_profile_claim() {
        use plx_machine::job::Port;
        use super::super::watchlist_job::WriteEnd;
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        s.run(WatchlistCmd::Press { hex: hex_of(1), intent: Intent::Remove });
        s.maybe_spawn();
        let super::super::StoreJob::WatchlistWrite(old, old_port) = s.take_jobs().pop().unwrap() else { panic!("write job") };
        assert!(old_port.start());
        s.run(WatchlistCmd::Reset);
        load_as(&mut s, 2);
        s.run(WatchlistCmd::Press { hex: hex_of(1), intent: Intent::Remove });
        s.maybe_spawn();
        let claim = s.state.write_claim;
        assert_ne!(claim, Some(old.req.tag.req));
        old_port.done(old.addr, WriteEnd::Written { write: old.req.write, outcome: WriteOutcome::Landed });
        assert!(!s.pump_writes(plx_machine::landgate::fixture_gate(), 2));
        assert_eq!(s.state.write_claim, claim);
        assert!(mock.writes.lock().unwrap().is_empty());
    }


    #[test]
    fn write_full_tag_and_address_fence_rejects_stale_landings() {
        use plx_machine::{job::Port, machine::{Addr, MachineId, RequestId}};
        use super::super::watchlist_job::{WriteEnd, WritePort, WriteReq};
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        s.run(WatchlistCmd::Press { hex: hex_of(1), intent: Intent::Remove });
        s.maybe_spawn();
        let super::super::StoreJob::WatchlistWrite(job, current) = s.take_jobs().pop().unwrap() else { panic!("write") };
        let claim = s.state.write_claim;
        let tag = job.req.tag;
        let mut stale_epoch = tag; stale_epoch.epoch = stale_epoch.epoch.wrapping_add(1);
        let mut stale_profile = tag; stale_profile.profile_gen = stale_profile.profile_gen.wrapping_add(1);
        let mut stale_req = tag; stale_req.req = stale_req.req.wrapping_add(1);
        for stale in [stale_epoch, stale_profile, stale_req] {
            let addr = Addr { to: MachineId::Store(super::super::StoreId::Watchlist.ord()), req: RequestId(stale.req) };
            let req = WriteReq { tag: stale, auth: job.req.auth.clone(), write: job.req.write };
            let port = WritePort::new(addr, &req, Arc::clone(&s.write_mailbox));
            port.done(addr, WriteEnd::Written { write: req.write, outcome: WriteOutcome::Failed });
            assert!(!s.pump_writes(plx_machine::landgate::fixture_gate(), tag.profile_gen));
            assert_eq!(s.state.write_claim, claim);
            assert!(!s.view().is_member(&hex_of(1)), "stale failure cannot roll back the intent");
        }
        let wrong = Addr { to: job.addr.to, req: RequestId(job.addr.req.0 + 1) };
        current.done(wrong, WriteEnd::Written { write: job.req.write, outcome: WriteOutcome::Failed });
        assert!(current.start(), "wrong address must not retire the current port");
        current.done(job.addr, WriteEnd::Written { write: job.req.write, outcome: WriteOutcome::Landed });
        assert!(s.pump_writes(plx_machine::landgate::fixture_gate(), tag.profile_gen));
        assert_eq!(s.state.write_claim, None);
        assert!(!s.view().is_member(&hex_of(1)));
    }

    #[test]
    fn write_not_started_requeues_exactly_at_head_until_120th_pump() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        s.spawn = refuse;
        s.run(WatchlistCmd::Press { hex: hex_of(3), intent: Intent::Remove });
        s.run(WatchlistCmd::Press { hex: hex_of(1), intent: Intent::Remove });
        pump_scripted(&mut s);
        assert_eq!(s.state.write_retry, RETRY_FRAMES);
        let first = s.state.ledger.next_write().expect("refused write remains queued");
        assert_eq!(first.hex, hex_of(3));
        assert_eq!(first.intent, Intent::Remove);
        assert_eq!(first.seq, 1);
        s.state.ledger.requeue(first);
        s.spawn = inline;
        for _ in 0..(RETRY_FRAMES - 1) { pump_scripted(&mut s); }
        assert!(mock.writes.lock().unwrap().is_empty(), "no write before the boundary");
        pump_scripted(&mut s);
        assert_eq!(*mock.writes.lock().unwrap(), [(id(3), Intent::Remove)]);
        for _ in 0..3 { pump_scripted(&mut s); }
        assert_eq!(*mock.writes.lock().unwrap(), [(id(3), Intent::Remove), (id(1), Intent::Remove)]);
    }

    #[test]
    fn write_started_panic_is_failed_and_not_retried() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        mock.panic_writes.store(true, Ordering::SeqCst);
        s.run(WatchlistCmd::Press { hex: hex_of(1), intent: Intent::Remove });
        pump_scripted(&mut s);
        assert_eq!(s.state.write_retry, 0);
        assert!(!s.state.ledger.busy());
        assert!(s.view().is_member(&hex_of(1)));
        mock.panic_writes.store(false, Ordering::SeqCst);
        for _ in 0..(RETRY_FRAMES + 1) { pump_scripted(&mut s); }
        assert!(mock.writes.lock().unwrap().is_empty(), "started panic was not retried");
    }

    #[test]
    fn a_refused_write_spawn_keeps_the_write_for_later() {
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        s = s.with_spawn_for_test(refuse);
        s.run(WatchlistCmd::Press { hex: hex_of(1), intent: Intent::Remove });
        pump_scripted(&mut s);
        assert!(s.state.ledger.busy(), "still owed");
        s = s.with_spawn_for_test(inline);
        for _ in 0..(RETRY_FRAMES + 4) {
            pump_scripted(&mut s);
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
            pump_scripted(&mut s);
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
        pump_scripted(&mut s);
        assert_eq!(
            (s.view().state(0), s.view().state(1)),
            (Some(TileState::ServerOffline), Some(TileState::ServerOffline)),
            "one facts epoch moves them together"
        );
        assert!(s.held_keys().is_empty() || s.state.window.is_empty());
        mock.up.lock().unwrap().push(a);
        mock.facts_gen.fetch_add(1, Ordering::SeqCst);
        pump_scripted(&mut s);
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
        pump_scripted(&mut s);
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
        pump_scripted(&mut s);
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
            pump_scripted(&mut s);
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
            pump_scripted(&mut s);
        }
        assert_eq!(mock.finds.load(Ordering::SeqCst), first, "an unchanged list asks no server again");
        mock.items.lock().unwrap().push(item(4, "Delta"));
        s.run(WatchlistCmd::Reread);
        for _ in 0..4 {
            pump_scripted(&mut s);
        }
        assert_eq!(mock.finds.load(Ordering::SeqCst), first + 1, "one new movie guid: one chunk, one server");
        assert_eq!(titles(&s).len(), 4);
        // A roster move is a full re-match.
        mock.roster_gen.fetch_add(1, Ordering::SeqCst);
        s.run(WatchlistCmd::Reread);
        for _ in 0..4 {
            pump_scripted(&mut s);
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
            pump_scripted(&mut s);
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
        // A real claimed reread fails before page zero and retains the complete list.
        let _ = mock.etag_on.swap(false, Ordering::SeqCst);
        mock.fail_pages_from.store(u32::MIN, Ordering::SeqCst);
        s.run(WatchlistCmd::Reread);
        for _ in 0..2 { pump_scripted(&mut s); }
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
        pump_scripted(&mut s);
        assert_eq!(s.view().copy(1).map(|m| m.rating_key), Some(20), "owned beats a share");
        // The current server is last in the roster and wins over both.
        mock.ranks.lock().unwrap().insert(c, 0);
        pump_scripted(&mut s);
        assert_eq!(s.view().copy(1).map(|m| m.rating_key), Some(30), "current beats owned");
        // An unreachable preferred copy is never chosen.
        mock.up.lock().unwrap().retain(|x| *x != c);
        pump_scripted(&mut s);
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
        pump_scripted(&mut s);
        assert_eq!((s.view().copy(1).map(|m| m.rating_key), genre(&s)), (Some(20), Some("Comedy".into())), "the owned copy's facts");
        mock.up.lock().unwrap().clear();
        pump_scripted(&mut s);
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
            pump_scripted(&mut s);
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
    fn failed_rereads_preserve_the_truncated_status_of_held_rows() {
        for failed_from in [0, 100] {
            let mock = Mock::new((1..=150).map(|n| item(n, &format!("T{n:03}"))).collect());
            mock.fail_pages_from.store(100, Ordering::SeqCst);
            let mut s = store(&mock);
            load(&mut s);
            assert_eq!(s.view().len(), 100);
            assert!(s.view().truncated());

            mock.fail_pages_from.store(failed_from, Ordering::SeqCst);
            reread(&mut s);
            assert_eq!(s.view().len(), 100, "old partial rows must stay on failure at {failed_from}");
            assert!(s.view().truncated(), "held rows remain partial after failure at {failed_from}");
            assert_eq!(s.state.list_etag, None);

            mock.fail_pages_from.store(u32::MAX, Ordering::SeqCst);
            reread(&mut s);
            assert_eq!(s.view().len(), 150);
            assert!(!s.view().truncated());
        }
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
        pump_scripted(&mut s);
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
            pump_scripted(s);
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
            pump_scripted(&mut s);
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
            pump_scripted(&mut s);
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
        pump_scripted(&mut s);
        s.run(WatchlistCmd::Press { hex: hex_of(1), intent: Intent::Remove });
        pump_scripted(&mut s);
        s.spawn = inline;
        let jobs: Vec<_> = HELD.with(|h| h.borrow_mut().drain(..).collect());
        assert_eq!(jobs.len(), 2, "the list worker and the write worker");
        for job in jobs {
            job();
        }
        for _ in 0..4 {
            pump_scripted(&mut s);
        }
        assert_eq!(titles(&s), ["Bravo"], "the older list claim lands despite the newer write request");
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
        s.drive_jobs_for_test(plx_machine::landgate::fixture_gate(), &WatchlistInput::profile_for_test(1));
        assert_eq!(mock.found.lock().unwrap().len(), 1, "one chunk for three titles");
        pump_scripted(&mut s);
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
        assert!(ready.rematch_on(sid0(), vec![(guid(1), Kind::Movie)]), "an addressed job is emitted before admission");
        ready.drive_jobs_for_test(plx_machine::landgate::fixture_gate(), &WatchlistInput::profile_for_test(1));
        assert!(ready.pending_delta.is_some(), "refused delta stays owned for retry");
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
        s.drive_jobs_for_test(plx_machine::landgate::fixture_gate(), &WatchlistInput::profile_for_test(1));
        let sizes: Vec<usize> = mock.found.lock().unwrap().iter().map(Vec::len).collect();
        assert_eq!(sizes, [40, 40, 10]);
    }

    #[test]
    fn stream_jobs_list_rematch_delta_are_values_until_the_runner_enters() {
        use super::super::{StoreJob, watchlist_job::*};
        let mock = three();
        let mut s = store(&mock);
        s.run_for_test(WatchlistCmd::Open);
        let profile = WatchlistInput::profile_for_test(1);
        let gate = plx_machine::landgate::fixture_gate();
        s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
        assert_eq!(mock.pages.load(Ordering::SeqCst), 0, "the step does not do IO");
        let StoreJob::WatchlistList(job, port) = s.take_jobs().pop().unwrap() else { panic!("list job") };
        assert_eq!(job.addr.req.0, job.req.tag.req);
        assert_eq!(job.req.tag.profile_gen, 1);
        finish_list_job(&port, job.addr, job.req, &*mock);
        assert!(port.is_owed());
        s.land_stream_results(gate, &profile);
        assert!(!port.is_owed());
        assert_eq!(s.view().len(), 3);
        let finds = mock.finds.load(Ordering::SeqCst);
        mock.roster_gen.fetch_add(1, Ordering::SeqCst);
        s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
        let StoreJob::WatchlistRematch(job, port) = s.take_jobs().pop().unwrap() else { panic!("rematch job") };
        assert_eq!(mock.finds.load(Ordering::SeqCst), finds);
        finish_rematch_job(&port, job.addr, job.req, &*mock);
        s.land_stream_results(gate, &profile);
        assert!(!port.is_owed());
        let finds = mock.finds.load(Ordering::SeqCst);
        assert!(s.rematch_on(sid0(), s.rows_of(Kind::Movie)));
        let StoreJob::WatchlistDelta(job, port) = s.take_jobs().pop().unwrap() else { panic!("delta job") };
        assert_eq!(mock.finds.load(Ordering::SeqCst), finds);
        finish_delta_job(&port, job.addr, job.req, &*mock);
        s.land_stream_results(gate, &profile);
        assert!(!port.is_owed());
        assert!(!s.read_in_flight());
    }
    #[test]
    fn stream_jobs_refused_delta_retains_asks_and_retries_at_120_pumps() {
        use super::super::StoreJob;
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        let profile = WatchlistInput::profile_for_test(1);
        let gate = plx_machine::landgate::fixture_gate();
        assert!(s.rematch_on(sid0(), vec![(guid(1), Kind::Movie)]));
        let StoreJob::WatchlistDelta(job, port) = s.take_jobs().pop().unwrap() else { panic!("delta") };
        port.refused(job.addr);
        s.land_stream_results(gate, &profile);
        assert!(!port.is_owed());
        assert!(s.pending_delta.is_some());
        for _ in 0..119 { s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock)); assert!(!s.has_jobs()); }
        s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
        assert!(matches!(s.take_jobs().pop(), Some(StoreJob::WatchlistDelta(..))));
    }

    #[test]
    fn stream_jobs_delta_rows_keep_original_allocation_through_refused_retries() {
        use super::super::StoreJob;
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        let profile = WatchlistInput::profile_for_test(1);
        let gate = plx_machine::landgate::fixture_gate();
        let rows = vec![(guid(2), Kind::Movie), (guid(1), Kind::Movie)];
        let original_rows = rows.as_ptr();
        let original_guids: Vec<_> = rows.iter().map(|(guid, _)| guid.as_ptr()).collect();
        let expected = rows.clone();
        assert!(s.rematch_on(sid0(), rows));
        let StoreJob::WatchlistDelta(mut job, mut port) = s.take_jobs().pop().unwrap() else { panic!("delta") };
        assert_eq!(job.req.rows.as_ptr(), original_rows);
        assert_eq!(job.req.rows.as_ref(), &expected);
        let (sid, retained) = s.pending_delta.as_ref().unwrap();
        assert_eq!(*sid, sid0());
        assert!(Arc::ptr_eq(retained, &job.req.rows));
        let retained = Arc::clone(retained);
        let mut equal = job.req.clone();
        equal.rows = Arc::new(expected.clone());
        assert_eq!(job.req, equal, "request equality compares row content, not allocation identity");
        assert!(!Arc::ptr_eq(&job.req.rows, &equal.rows));
        for _ in 0..2 {
            let tag = job.req.tag;
            let input = job.req.input.clone();
            port.refused(job.addr);
            drop(job);
            s.land_stream_results(gate, &profile);
            assert!(!port.is_owed());
            assert!(Arc::ptr_eq(&s.pending_delta.as_ref().unwrap().1, &retained));
            for _ in 0..119 {
                s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
                assert!(!s.has_jobs());
            }
            s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
            let StoreJob::WatchlistDelta(retry, retry_port) = s.take_jobs().pop().unwrap() else { panic!("delta retry") };
            assert_eq!(retry.req.sid, sid0());
            assert_eq!(retry.req.tag.profile_gen, tag.profile_gen);
            assert_eq!(retry.req.tag.epoch, tag.epoch);
            assert!(retry.req.tag.req > tag.req);
            assert_eq!(retry.req.input, input);
            assert!(Arc::ptr_eq(&retry.req.rows, &retained));
            assert_eq!(retry.req.rows.as_ptr(), original_rows);
            assert_eq!(retry.req.rows.as_ref(), &expected);
            assert_eq!(retry.req.rows.iter().map(|(guid, _)| guid.as_ptr()).collect::<Vec<_>>(), original_guids);
            job = retry;
            port = retry_port;
        }
    }
    #[test]
    fn stream_jobs_captured_roster_is_used_even_when_transport_facts_move() {
        use super::super::{StoreJob, watchlist_job::finish_list_job};
        let mock = three();
        let mut s = store(&mock);
        s.run_for_test(WatchlistCmd::Open);
        let profile = WatchlistInput::profile_for_test(1);
        let gate = plx_machine::landgate::fixture_gate();
        s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
        let StoreJob::WatchlistList(job, port) = s.take_jobs().pop().unwrap() else { panic!("list") };
        mock.roster.lock().unwrap().clear();
        mock.up.lock().unwrap().clear();
        mock.roster_gen.store(99, Ordering::SeqCst);
        finish_list_job(&port, job.addr, job.req, &*mock);
        s.land_stream_results(gate, &profile);
        assert_eq!(mock.finds.load(Ordering::SeqCst), 1, "captured reachable server is still asked");
        assert_eq!(s.state.table.roster_gen, 1, "transport's current generation is not a step input");
    }
    #[test]
    fn stream_jobs_reset_cancels_queued_work_and_acks_retired_terminal() {
        use super::super::{StoreJob, watchlist_job::finish_list_job};
        let mock = three();
        let mut s = store(&mock);
        s.run_for_test(WatchlistCmd::Open);
        let profile = WatchlistInput::profile_for_test(1);
        let gate = plx_machine::landgate::fixture_gate();
        s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
        let StoreJob::WatchlistList(job, port) = s.take_jobs().pop().unwrap() else { panic!("list") };
        s.run(WatchlistCmd::Reset);
        finish_list_job(&port, job.addr, job.req, &*mock);
        assert_eq!(mock.pages.load(Ordering::SeqCst), 0);
        assert!(!port.is_owed());
        assert!(s.view().is_empty());
    }
    #[test]
    fn stream_jobs_started_reset_late_terminal_is_acked_without_publication() {
        use super::super::StoreJob;
        let mock = three();
        let mut s = store(&mock);
        s.run_for_test(WatchlistCmd::Open);
        let profile = WatchlistInput::profile_for_test(1);
        let gate = plx_machine::landgate::fixture_gate();
        s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
        let StoreJob::WatchlistList(_, port) = s.take_jobs().pop().unwrap() else { panic!("list") };
        let producer = port.start().unwrap();
        s.run(WatchlistCmd::Reset);
        producer.finish(StreamEnd::Cancelled);
        s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
        assert!(!port.is_owed());
        assert!(s.view().is_empty());
        assert!(s.retired_streams.is_empty());
    }
    #[test]
    fn stream_jobs_panicked_reread_preserves_held_completeness_before_and_after_page_zero() {
        use super::super::StoreJob;
        for truncated in [false, true] {
            for page_seen in [false, true] {
                let mock = Mock::new((1..=150).map(|n| item(n, &format!("T{n:03}"))).collect());
                if truncated { mock.fail_pages_from.store(100, Ordering::SeqCst); }
                let mut s = store(&mock);
                load(&mut s);
                let held = titles(&s);
                assert_eq!(s.view().truncated(), truncated);
                s.run(WatchlistCmd::Reread);
                let profile = WatchlistInput::profile_for_test(1);
                let gate = plx_machine::landgate::fixture_gate();
                s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
                let StoreJob::WatchlistList(job, port) = s.take_jobs().pop().unwrap() else { panic!("list") };
                let producer = port.start().unwrap();
                if page_seen {
                    producer.post(job.addr, What::Page { start: 0, total: Some(200), rows: Vec::new(),
                        answers: Vec::new(), roster_gen: 1, etag: Some("partial".into()) });
                }
                producer.finish(StreamEnd::Panicked);
                assert!(s.land_stream_results(gate, &profile));
                assert_eq!(titles(&s), held);
                assert_eq!(s.view().truncated(), truncated, "held completeness survives panic, page_seen={page_seen}");
                assert!(!s.read_in_flight());
                assert!(!port.is_owed());
                assert!(!s.land_stream_results(gate, &profile));
            }
        }
    }

    #[test]
    fn stream_jobs_first_load_panic_after_page_zero_keeps_partial_rows_truncated() {
        use super::super::StoreJob;
        let mock = three();
        let mut s = store(&mock);
        s.run_for_test(WatchlistCmd::Open);
        let profile = WatchlistInput::profile_for_test(1);
        let gate = plx_machine::landgate::fixture_gate();
        s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
        let StoreJob::WatchlistList(job, port) = s.take_jobs().pop().unwrap() else { panic!("list") };
        let producer = port.start().unwrap();
        producer.post(job.addr, What::Page { start: 0, total: Some(3),
            rows: vec![crate::watchlist::row_of(&item(1, "Alpha"), 0).unwrap()], answers: Vec::new(),
            roster_gen: 1, etag: Some("partial".into()) });
        producer.finish(StreamEnd::Panicked);
        assert!(s.land_stream_results(gate, &profile));
        assert_eq!(titles(&s), ["Alpha"]);
        assert!(s.view().truncated());
        assert_eq!(s.view().status(), Status::Ready);
        assert!(!s.read_in_flight());
        assert!(!port.is_owed());
        assert!(!s.land_stream_results(gate, &profile));
    }

    #[test]
    fn stream_jobs_panicked_reread_keeps_held_rows_and_has_one_terminal() {
        use super::super::StoreJob;
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        s.run(WatchlistCmd::Reread);
        let profile = WatchlistInput::profile_for_test(1);
        let gate = plx_machine::landgate::fixture_gate();
        s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
        let StoreJob::WatchlistList(job, port) = s.take_jobs().pop().unwrap() else { panic!("list") };
        let producer = port.start().unwrap();
        producer.post(job.addr, What::Page { start: 0, total: Some(5), rows: Vec::new(), answers: Vec::new(), roster_gen: 1, etag: Some("partial".into()) });
        producer.finish(StreamEnd::Panicked);
        s.land_stream_results(gate, &profile);
        assert_eq!(titles(&s), ["Alpha", "Bravo", "Charlie"]);
        assert!(!s.read_in_flight());
        assert!(!port.is_owed());
        assert!(s.state.list_etag.is_none() || s.state.list_etag == mock.tag());
        assert!(!s.land_stream_results(gate, &profile));
    }

    #[test]
    fn resolve_stale_full_client_key_is_not_cached() {
        use plx_machine::job::Port;
        use plx_plex::plex::{ClientKey, ClientKeys};
        use super::super::{StoreJob, watchlist_job::ResolveEnd};
        let old = ClientKey { instance: 1, token: 2, grant: 3 };
        for moved in [ClientKey { instance: 9, ..old }, ClientKey { token: 9, ..old }, ClientKey { grant: 9, ..old }] {
            let mut s = WatchlistStore::default();
            let sid = sid0();
            let profile = WatchlistInput::profile_for_test(1);
            let gate = plx_machine::landgate::fixture_gate();
            let keys = ClientKeys::from_keys([(sid, old)]);
            s.want_hex(sid, "7");
            s.pump_with_snapshot(gate, &profile, Snapshot::new(&keys, None));
            let StoreJob::WatchlistResolve(job, port) = s.take_jobs().pop().unwrap() else { panic!("resolve") };
            assert!(port.start());
            port.done(job.addr, ResolveEnd::Answer(Some(Some(hex_of(7)))));
            let keys = ClientKeys::from_keys([(sid, moved)]);
            s.pump_with_snapshot(gate, &profile, Snapshot::new(&keys, None));
            assert_eq!(s.view().resolved_hex(sid, "7"), None, "every full-key component fences a stale answer");
            assert!(s.state.resolving.is_empty());
            s.want_hex(sid, "7");
            s.pump_with_snapshot(gate, &profile, Snapshot::new(&keys, None));
            let StoreJob::WatchlistResolve(job, _) = s.take_jobs().pop().unwrap() else { panic!("replacement") };
            assert_eq!(job.req.keys.key(sid), Some(moved));
        }
    }
    #[test]
    fn stream_jobs_retired_terminal_race_is_always_acknowledged() {
        use super::super::StoreJob;
        for _ in 0..200 {
            let mock = three();
            let mut s = store(&mock);
            s.run_for_test(WatchlistCmd::Open);
            let profile = WatchlistInput::profile_for_test(1);
            let gate = plx_machine::landgate::fixture_gate();
            s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
            let StoreJob::WatchlistList(_, port) = s.take_jobs().pop().unwrap() else { panic!("list") };
            let producer = port.start().unwrap();
            s.run(WatchlistCmd::Reset);
            let worker = std::thread::spawn(move || producer.finish(StreamEnd::Cancelled));
            s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
            worker.join().unwrap();
            s.pump_with_snapshot(gate, &profile, Snapshot::scripted(&*mock));
            assert!(!port.is_owed());
        }
    }

    #[test]
    fn stream_jobs_all_admission_refusals_settle_before_paced_landing_gates() {
        use plx_machine::job::Port;
        use super::super::StoreJob;
        let mock = three();
        let mut s = store(&mock);
        load(&mut s);
        let profile = WatchlistInput::profile_for_test(1);
        let gate = plx_machine::landgate::Gate::default();
        gate.arm_dump_paced(120);
        gate.dump_frame(0);
        s.run(WatchlistCmd::Reread);
        s.run(WatchlistCmd::Press { hex: hex_of(1), intent: Intent::Remove });
        s.want_hex(sid0(), "7");
        s.pump_with_snapshot(&gate, &profile, Snapshot::scripted(&*mock));
        let mut streams = Vec::new();
        for job in s.take_jobs() {
            match job {
                StoreJob::WatchlistList(job, port) => { port.refused(job.addr); streams.push(port); }
                StoreJob::WatchlistWrite(job, port) => port.refused(job.addr),
                StoreJob::WatchlistResolve(job, port) => port.refused(job.addr),
                _ => panic!("unexpected job"),
            }
        }
        s.land_stream_results(&gate, &profile);
        s.land_write_results(&gate, &profile);
        s.land_resolve_refusals(&profile);
        assert!(s.state.list_claim.is_none());
        assert!(s.state.write_claim.is_none());
        assert!(s.state.resolving.is_empty());
        assert!(streams.iter().all(|p| !p.is_owed()));
        assert_eq!(s.state.list_retry, RETRY_FRAMES);
        assert_eq!(s.state.write_retry, RETRY_FRAMES);
        assert!(!s.has_jobs(), "a refusal drain never emits a replacement");
    }
}
