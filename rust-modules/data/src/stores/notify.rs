//! **The PMS notification feed's owner** (layer-2 view state, L2-2): which servers hold an open
//! notification connection, when a dropped one is retried, and what a reconnect owes the screens.
//! The protocol is `plx_plex::plex::notify`; this is the policy around it.
//!
//! **It ships ON, for owned servers only.** [`NOTIFY_ENABLED`] is `true` and a connection opens
//! only to a server this account OWNS: a friend's share, a household member's server and a server
//! nothing has described yet all stay dark. The feed names other people's playback, so that rule is
//! the one that must not loosen. `docs/pms-api.md` §9 records what the mock-TV run verified (connect,
//! idle, backoff, a tile, a detail page) and what is still owed to the set (background/foreground,
//! a profile switch, a real network loss).
//!
//! **A notification here is a hint, never a playback event.** Nothing in this module calls
//! `enqueue_stop`, adds an end-of-stream candidate or publishes an Offset: a stranger's `stopped`
//! session must not scrobble our other servers (`fan_out_origin_watched`). The output
//! ([`NotifyOut`]) is read by `stores::notify_map`, which turns it into read-backs of held items.
//!
//! The store has an unrecorded job `StoreId` but no `StoreCmd`: no screen addresses it. It is pumped once
//! a frame by the application (`Bridge::notify_pump`) and told about the lifecycle
//! (`notify_suspend`, `notify_resume`, `notify_network_changed`) and a profile reset
//! (`notify_reset`). Every deadline is counted in pumped frames, never read from a clock.
//!
//! **One connection per owned server**, keyed by `(instance_gen, token_gen)` of that server's
//! client: a re-pointed client or a profile switch is a new key, which cancels the old connection
//! and discards whatever its worker still pushes. A feed that comes back after
//! being lost UNEXPECTEDLY while live bumps [`NotifyStore::resync_epoch`]: events were missed in
//! between. A deliberate drop (suspend, network change) is no gap, the foreground re-reads itself;
//! a flapping feed backs off instead of resetting on its first frame and resyncs once per
//! escalation, the rest waiting until it has stayed live for a while.

use plx_plex::plex::notify::{LibraryEntry, Notification, NotifyEnd, PlaySession};
use plx_plex::plex::ServerId;
use plx_plex::plex::{ClientKey, ClientKeys};
use plx_machine::machine::{Addr, MachineId, RequestId};
use plx_machine::job::Job;
use super::{StoreId, StoreJob};
use super::notify_job::{Link, NotifyStreamPort, NotifyStreamReq};

/// Ships `true`. `docs/pms-api.md` §9 lists what was checked on the mock TV and what is still owed. The owned-only rule is separate and
/// unconditional; `notify_tests::enabled_const_connects_to_an_owned_server` and
/// `non_owned_never_connects` pin both halves.
pub const NOTIFY_ENABLED: bool = true;

/// Pumps per second the backoff is written against (the frame loop's rate).
pub const FRAMES_PER_S: u64 = 60;
/// First retry delay and the ceiling the doubling stops at, in seconds.
const BACKOFF_BASE_S: u64 = 2;
const BACKOFF_MAX_S: u64 = 60;

/// A connection that stayed live this long (the protocol's idle window) is healthy: its backoff
/// starts over, and a resync it owed but skipped while flapping is delivered.
const STABLE_FRAMES: u64 = 25 * FRAMES_PER_S;

type Key = ClientKey;

/// Captured immutable input for a complete cursor pass.
pub struct NotifyInput<'a> { pub keys: &'a ClientKeys, pub armed: bool }

/// Owned transaction token: no borrow of Stores crosses runner IO.
#[must_use]
pub struct NotifyPass {
    owner: std::sync::Arc<()>, frame: u64, cursor: usize, count: usize,
    pending: bool, out: NotifyOut,
}
#[must_use]
pub struct NotifyTurn {
    owner: std::sync::Arc<()>, frame: u64, index: usize, sid: ServerId, key: Key,
    addr: Option<Addr>, launch: bool, settled: bool,
}

fn owned(keys: &ClientKeys, sid: ServerId) -> bool {
    keys.key(sid).is_some() && keys.facts(sid)
        .and_then(|f| f.described).is_some_and(|f| f.owned)
}
fn stand_down(conn: &mut Conn) {
    conn.drop_link();
    if conn.state == State::Live { conn.gap_owed = true; }
    if matches!(conn.state, State::Connecting | State::Live) { conn.state = State::Idle; }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Park {
    /// A 401: parked until the token changes (a new key).
    Token,
    /// A 4xx other than 401, or a device that cannot stream: parked until [`NotifyStore::reset`].
    Session,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// Nothing open and nothing owed: connects on the next eligible pump.
    Idle,
    /// A worker was started and has delivered nothing yet.
    Connecting,
    Live,
    Backoff { until: u64 },
    Parked(Park),
}

struct Conn {
    sid: ServerId,
    key: Key,
    link: Option<Link>,
    state: State,
    /// A live connection was lost unexpectedly: events were missed, and the next time the feed
    /// is live the screens are owed a re-read. A deliberate drop (suspend, network change) is
    /// not a gap: the foreground re-reads on its own.
    gap_owed: bool,
    /// A resync was delivered since this server's feed was last stable. While it is set a flapping
    /// connection does not bump again; the debt waits for stability.
    resynced_unstable: bool,
    /// The frame the connection went live on.
    live_since: u64,
    /// Consecutive short-lived connections; drives the backoff, zero once one stays live for
    /// [`STABLE_FRAMES`].
    attempts: u32,
}

/// Bump `sid`'s epoch in `resync` and record it in `out`.
fn bump_epoch(resync: &mut Vec<(ServerId, u32)>, sid: ServerId, out: &mut NotifyOut) {
    let epoch = match resync.iter_mut().find(|(s, _)| *s == sid) {
        Some((_, e)) => {
            *e += 1;
            *e
        }
        None => {
            resync.push((sid, 1));
            1
        }
    };
    out.resynced.push((sid, epoch));
    plx_base::eventlog::log(&format!("notify: resync sid={} epoch={epoch}", sid.raw()));
}

impl Conn {
    /// Cancel and forget the open connection, if any.
    fn drop_link(&mut self) {
        if let Some(link) = self.link.take() {
            link.cancel();
        }
    }

    fn back_off(&mut self, frame: u64) {
        let secs = (BACKOFF_BASE_S << self.attempts.min(8)).min(BACKOFF_MAX_S);
        let frames = secs * FRAMES_PER_S;
        // Up to a quarter more, picked by the frame count with the server id mixed in: servers that
        // dropped together, even in the same pump, do not all knock again on the same frame.
        let jitter = frame.wrapping_add(u64::from(self.sid.raw()).wrapping_mul(7919)) % (frames / 4).max(1);
        self.attempts = self.attempts.saturating_add(1);
        self.state = State::Backoff { until: frame + frames + jitter };
    }

    /// Take what the worker delivered since the last pump, then what ended it.
    fn drain(&mut self, frame: u64, resync: &mut Vec<(ServerId, u32)>, out: &mut NotifyOut) {
        let Some(link) = self.link.clone() else { return };
        let (events, end) = {
            let mut inbox = link.lock();
            (std::mem::take(&mut inbox.events), inbox.end.take())
        };
        let mut sessions = Vec::new();
        let mut library = Vec::new();
        for (key, n) in events {
            if key != self.key {
                continue;
            }
            if self.state == State::Connecting {
                self.state = State::Live;
                self.live_since = frame;
                if self.gap_owed && !self.resynced_unstable {
                    bump_epoch(resync, self.sid, out);
                    self.gap_owed = false;
                    self.resynced_unstable = true;
                }
                plx_base::eventlog::log(&format!("notify: live sid={}", self.sid.raw()));
            }
            match n {
                Notification::Play(mut s) => sessions.append(&mut s),
                Notification::Library(mut e) => library.append(&mut e),
                Notification::Ping | Notification::Ignored => {}
            }
        }
        if self.state == State::Live && frame.saturating_sub(self.live_since) >= STABLE_FRAMES {
            self.attempts = 0;
            self.resynced_unstable = false;
            if std::mem::take(&mut self.gap_owed) {
                bump_epoch(resync, self.sid, out);
                self.resynced_unstable = true;
            }
        }
        if !sessions.is_empty() {
            out.plays.push(NotifyPlays { sid: self.sid, sessions });
        }
        if !library.is_empty() {
            out.library.push(NotifyLibrary { sid: self.sid, entries: library });
        }
        if let Some(end) = end {
            self.link = None;
            if self.state == State::Live {
                // Lost while live: whatever the server said meanwhile was missed.
                self.gap_owed = true;
            }
            self.state = match end {
                NotifyEnd::Refused { status: 401 } => State::Parked(Park::Token),
                // Another 4xx or a device that cannot stream: retrying will not help. Status 0 (this
                // build's own gate refused the request) is NOT here: it is a state of the moment (a
                // plaintext grant a foreground cleared and discovery will mint again), so it backs off.
                NotifyEnd::Refused { status: 400..=499 } | NotifyEnd::Unsupported => {
                    State::Parked(Park::Session)
                }
                _ => {
                    self.back_off(frame);
                    self.state
                }
            };
            plx_base::eventlog::log(&format!(
                "notify: sid={} ended {:?} -> {:?}",
                self.sid.raw(),
                end,
                self.state
            ));
        }
    }

    fn due(&self, frame: u64) -> bool {
        match self.state {
            State::Idle => true,
            State::Backoff { until } => frame >= until,
            State::Connecting | State::Live | State::Parked(_) => false,
        }
    }
}

/// What one pump found: the sessions other clients reported and the library items a server
/// finished creating or deleted, per server, and the servers whose feed came back after a gap.
#[derive(Default, Debug, PartialEq)]
pub struct NotifyOut {
    pub plays: Vec<NotifyPlays>,
    pub library: Vec<NotifyLibrary>,
    /// `(server, epoch)` for every server whose [`NotifyStore::resync_epoch`] moved this pump.
    pub resynced: Vec<(ServerId, u32)>,
}

/// A server's finished movie and show creates and deletes (never empty).
#[derive(Debug, PartialEq)]
pub struct NotifyLibrary {
    pub sid: ServerId,
    pub entries: Vec<LibraryEntry>,
}

#[derive(Debug, PartialEq)]
pub struct NotifyPlays {
    pub sid: ServerId,
    pub sessions: Vec<PlaySession>,
}

pub struct NotifyStore {
    enabled: bool,
    jobs: Vec<StoreJob>,
    next_req: u32,
    active: Option<u64>,
    owner: std::sync::Arc<()>,
    conns: Vec<Conn>,
    resync: Vec<(ServerId, u32)>,
    /// `(roster_gen, facts_gen)` the connection table was last built against.
    seen: (u32, u32),
    frame: u64,
    suspended: bool,

}

impl Default for NotifyStore {
    fn default() -> Self {
        Self::new(NOTIFY_ENABLED)
    }
}

impl Drop for NotifyStore {
    fn drop(&mut self) {
        self.close_all();
    }
}

impl NotifyStore {
    fn new(enabled: bool) -> Self {
        Self { enabled, jobs: Vec::new(), next_req: 0, active: None,
            owner: std::sync::Arc::new(()), conns: Vec::new(), resync: Vec::new(),
            seen: (0, 0), frame: 0, suspended: false }
    }

    pub fn take_jobs(&mut self) -> Vec<StoreJob> { std::mem::take(&mut self.jobs) }
    pub fn has_jobs(&self) -> bool { !self.jobs.is_empty() }

    /// How many times `sid`'s feed came back after having been live.
    pub fn resync_epoch(&self, sid: ServerId) -> u32 {
        self.resync.iter().find(|(s, _)| *s == sid).map_or(0, |(_, e)| *e)
    }

    fn close_all(&mut self) {
        for conn in &mut self.conns {
            conn.drop_link();
        }
    }

    /// One frame: reconcile the connection table with the registry, connect what is due and
    /// allowed, collect what the workers delivered. `armed` is the landing gate: a recording, a
    /// replay or a dump owns the run and a live stream would break its determinism.
    pub fn begin(&mut self, input: &NotifyInput<'_>) -> NotifyPass {
        assert!(self.active.is_none(), "Notify pass in progress");
        assert!(self.jobs.is_empty(), "undrained Notify jobs");
        self.frame = self.frame.checked_add(1).expect("Notify frame exhausted");
        self.sync_roster(input.keys);
        let blocked = input.armed || !self.enabled || self.suspended;
        if blocked { for conn in &mut self.conns { stand_down(conn); } }
        self.active = Some(self.frame);
        NotifyPass { owner: self.owner.clone(), frame: self.frame, cursor: 0,
            count: if blocked { 0 } else { self.conns.len() }, pending: false,
            out: NotifyOut::default() }
    }

    fn check_pass(&self, pass: &NotifyPass) {
        assert!(std::sync::Arc::ptr_eq(&self.owner, &pass.owner));
        assert_eq!(self.active, Some(pass.frame));
    }

    pub fn prepare_next(&mut self, pass: &mut NotifyPass, input: &NotifyInput<'_>) -> Option<NotifyTurn> {
        self.check_pass(pass);
        assert!(!pass.pending, "finish current Notify turn first");
        if pass.cursor == pass.count { return None; }
        let conn = &mut self.conns[pass.cursor];
        let allowed = !input.armed && self.enabled && !self.suspended
            && owned(input.keys, conn.sid) && input.keys.key(conn.sid) == Some(conn.key)
            && input.keys.stream_allowed(conn.sid);
        let mut launch = false;
        if !allowed { stand_down(conn); }
        else if conn.link.is_none() && conn.due(pass.frame) {
            self.next_req = self.next_req.checked_add(1).expect("Notify request ids exhausted");
            let addr = Addr { to: MachineId::Store(StoreId::Notify.ord()), req: RequestId(self.next_req) };
            let link = Link::new(addr, conn.key, pass.frame);
            self.jobs.push(StoreJob::NotifyStream(Job { addr,
                req: NotifyStreamReq { sid: conn.sid, key: conn.key } }, NotifyStreamPort::new(link.clone())));
            conn.link = Some(link);
            launch = true;
        }
        pass.pending = true;
        Some(NotifyTurn { owner: self.owner.clone(), frame: pass.frame, index: pass.cursor, sid: conn.sid, key: conn.key,
            addr: conn.link.as_ref().map(|l| l.addr), launch, settled: false })
    }

    fn check_turn(&self, pass: &NotifyPass, turn: &NotifyTurn) {
        self.check_pass(pass);
        assert!(pass.pending);
        assert!(std::sync::Arc::ptr_eq(&self.owner, &turn.owner));
        assert_eq!((turn.frame, turn.index), (pass.frame, pass.cursor));
        let conn = &self.conns[turn.index];
        assert_eq!((turn.sid, turn.key), (conn.sid, conn.key));
        assert_eq!(turn.addr, conn.link.as_ref().map(|l| l.addr));
    }

    pub fn settle_admission(&mut self, pass: &mut NotifyPass, turn: &mut NotifyTurn) {
        self.check_turn(pass, turn);
        assert!(!turn.settled);
        let conn = &mut self.conns[turn.index];
        if turn.launch {
            let receipt = conn.link.as_ref().unwrap().take_admission().expect("runner must report Notify admission synchronously");
            assert_eq!(Some(receipt.addr), turn.addr);
            assert_eq!(receipt.origin_frame, pass.frame);
            if receipt.accepted { conn.state = State::Connecting; }
            else {
                conn.drop_link();
                conn.back_off(receipt.origin_frame);
                turn.addr = None;
            }
        }
        turn.settled = true;
    }

    pub fn finish_turn(&mut self, pass: &mut NotifyPass, turn: NotifyTurn, input: &NotifyInput<'_>) {
        self.check_turn(pass, &turn);
        assert!(turn.settled);
        let conn = &mut self.conns[turn.index];
        if input.armed || !self.enabled || self.suspended || !owned(input.keys, conn.sid)
            || input.keys.key(conn.sid) != Some(turn.key) || !input.keys.stream_allowed(conn.sid) {
            stand_down(conn);
        } else { conn.drain(pass.frame, &mut self.resync, &mut pass.out); }
        pass.pending = false;
        pass.cursor += 1;
    }

    pub fn end(&mut self, pass: NotifyPass) -> NotifyOut {
        self.check_pass(&pass);
        assert!(!pass.pending && pass.cursor == pass.count);
        assert!(self.jobs.is_empty(), "undrained Notify jobs");
        self.active = None;
        pass.out
    }

    /// Reconcile every captured fill, including eligibility changes without generation moves.
    /// Retain connection order; append newly described owned slots in supplied slot order.
    fn sync_roster(&mut self, keys: &ClientKeys) {
        self.seen = (keys.roster_gen(), keys.facts_gen());
        self.conns.retain_mut(|conn| {
            let keep = owned(keys, conn.sid);
            if !keep { conn.drop_link(); }
            keep
        });
        for conn in &mut self.conns {
            let key = keys.key(conn.sid).unwrap();
            if key != conn.key {
                conn.drop_link();
                conn.key = key;
                conn.attempts = 0;
                if conn.state == State::Live { conn.gap_owed = true; }
                if conn.state != State::Parked(Park::Session) { conn.state = State::Idle; }
            }
        }
        for sid in keys.slots() {
            if !owned(keys, sid) || self.conns.iter().any(|c| c.sid == sid) { continue; }
            self.conns.push(Conn { sid, key: keys.key(sid).unwrap(), link: None,
                state: State::Idle, gap_owed: false, resynced_unstable: false,
                live_since: 0, attempts: 0 });
        }
    }

    /// WILL background: every connection is cancelled now and none opens until [`Self::resume`].
    pub fn suspend(&mut self) {
        assert!(self.active.is_none(), "Notify pass in progress");
        plx_base::eventlog::log(&format!("notify: suspend conns={}", self.conns.len()));
        self.suspended = true;
        for conn in &mut self.conns {
            conn.drop_link();
            // Deliberate: the foreground re-reads every covered page itself.
            conn.gap_owed = false;
            if matches!(conn.state, State::Connecting | State::Live) {
                conn.state = State::Idle;
            }
        }
    }

    /// DID foreground: connections may open again, and a pending backoff is forgiven. A parked
    /// server stays parked.
    pub fn resume(&mut self) {
        assert!(self.active.is_none(), "Notify pass in progress");
        plx_base::eventlog::log(&format!("notify: resume conns={}", self.conns.len()));
        self.suspended = false;
        for conn in &mut self.conns {
            if matches!(conn.state, State::Backoff { .. }) {
                conn.state = State::Idle;
                conn.attempts = 0;
            }
        }
    }

    /// The network may have changed under every open connection: cancel them all and reconnect.
    /// Backoff is forgiven; a parked server stays parked (a refusal is not about the network).
    pub fn network_changed(&mut self) {
        assert!(self.active.is_none(), "Notify pass in progress");
        plx_base::eventlog::log(&format!("notify: network changed conns={}", self.conns.len()));
        for conn in &mut self.conns {
            conn.drop_link();
            conn.gap_owed = false;
            if !matches!(conn.state, State::Parked(_)) {
                conn.state = State::Idle;
                conn.attempts = 0;
            }
        }
    }

    /// A profile reset: every connection is closed and forgotten, parks included, and the table
    /// is rebuilt from the registry on the next pump.
    pub fn reset(&mut self) {
        assert!(self.active.is_none(), "Notify pass in progress");
        self.jobs.clear();
        self.close_all();
        self.conns.clear();
        self.resync.clear();
        self.seen = (0, 0);
    }
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod tests;
