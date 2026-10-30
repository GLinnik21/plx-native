//! The Seerr store (docs/seerr.md): the connection to the household's Seerr server and every answer
//! a request surface reads — Settings' connection line, the Search row, the Requests tab, a title's
//! page and the Detail page's missing seasons.
//!
//! Main-thread state plus an `Arc` mailbox, on the catalog store's shape (`stores::catalog`): the
//! store starts no worker. Every ask puts one [`StoreJob::Seerr`] on its outbox
//! ([`SeerrStore::take_jobs`]) that the application's runner starts in the same call. Answers are
//! kept per [`Slot`] — one surface's question — and only a slot's LATEST ask is answered: a landing
//! for an older ask of the slot, for another address or for another profile is dropped.
//!
//! **A refresh keeps what is shown.** [`SeerrCmd::Refresh`] (the Seerr tab's request list, read
//! again while downloads move) asks like [`SeerrCmd::Ask`] but leaves a slot's `Ready` answer drawn
//! until the new one lands, and a refresh that fails keeps it too: a list that was right a moment
//! ago is better than a failure read-out for one missed poll.
//!
//! **Every job is answered once, a refusal included.** A job the runner never started (the thread
//! ceiling) comes back as `NotStarted` and its slot is asked again after [`RETRY_FRAMES`]. A job
//! that panics lands as [`SeerrError::Transport`], so no slot waits for ever.
//!
//! The session is the store's alone: a job is handed the one held and says what became of it
//! ([`SessionChange`]). It is never persisted, and it goes with the profile it was signed in for.

use super::{StoreId, StoreJob};
use crate::seerr::{run_op, ArrOptions, ArrServer, Availability, DiscoverRow, LiveSeerrIo, MediaKind, SeerrData, SeerrError,
    SeerrIo, SeerrOp, SeerrOutcome, SeerrSession, SeerrUser, SessionChange, TitleDetail};
use crate::watchlist::{WatchlistAuth, WatchlistInput};
use plx_machine::job::{Job, Port};
use plx_machine::machine::{Addr, MachineId, RequestId};
use plx_plex::plex::session::CurrentProfile;
use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Frames a refused job waits before the next try. A refused job never blocks a frame.
const RETRY_FRAMES: u32 = 120;

/// Frames a typed-in ask ([`SeerrStore::want`], Search's query) must stay the same before it is
/// asked: one call per settled term, not one per key.
const SETTLE_FRAMES: u32 = 18;

/// What a screen asks of the Seerr store (`StoreCmd::Seerr`): the Bridge runs it as the seated
/// profile ([`SeerrStore::ask`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeerrCmd {
    Ask { slot: Slot, op: SeerrOp },
    /// Ask again, keeping the slot's answer shown meanwhile (module doc).
    Refresh { slot: Slot, op: SeerrOp },
}

/// One surface's question. A new ask of a slot replaces the old one's answer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    /// Settings' Connect, and the sign-in a new address or profile owes.
    Connect,
    /// The Search page's row (one query at a time).
    Search,
    /// One page of one discover row.
    Discover(DiscoverRow, u32),
    /// The request list's first page.
    Requests,
    Detail(MediaKind, i64),
    /// A request write for one title.
    Request(MediaKind, i64),
    /// The request menu's list of the Radarr (a movie) or Sonarr (a show) servers.
    Servers(MediaKind),
    /// The request menu's profiles and folders of one of those servers.
    ServerOptions(MediaKind, i64),
    /// An approval or a decline of one request, by its id.
    Decide(i64),
}

/// The connection, as Settings states it.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Connection {
    /// No address configured: every request surface is hidden.
    #[default]
    Off,
    /// An address, and nothing asked of it yet, or nobody seated with a plex.tv account.
    Idle,
    Connecting,
    /// Signed in. The name is Seerr's for the user, shown in Settings only.
    Connected { name: String },
    /// The address is not a URL this client can use.
    BadAddress,
    /// The server answered, and does not let this Plex account in.
    NotAllowed,
    /// No answer, or an answer that is not Seerr's.
    Unreachable,
}

impl Connection {
    /// What a sign-in that failed with `e` says about the connection.
    fn of_sign_in_error(e: &SeerrError) -> Self {
        match e {
            SeerrError::BadAddress => Connection::BadAddress,
            SeerrError::Forbidden | SeerrError::Unauthorized => Connection::NotAllowed,
            _ => Connection::Unreachable,
        }
    }
}

/// A slot's state.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Loading,
    Ready(SeerrData),
    Failed(SeerrError),
}

/// What a request surface offers for one title: the Seerr title page's pill and the Detail page's
/// request disc read this one decision, from the title's detail and its request slot's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offer {
    /// Ask for it: a movie (no seasons) or these seasons of a show.
    Request(Vec<i32>),
    /// A request is on its way to the server.
    Sending,
    /// Nothing to ask for: the server holds or was asked for all of it, its owner blocked it, or
    /// this title's request went through.
    Nothing,
}

/// What can be asked of `detail` now, given its request slot's answer. A request that failed
/// offers itself again; one that went through offers nothing more.
pub fn offer(detail: &TitleDetail, request: Option<&Answer>) -> Offer {
    match request {
        Some(Answer::Loading) => return Offer::Sending,
        Some(Answer::Ready(SeerrData::Requested)) => return Offer::Nothing,
        _ => {}
    }
    match detail.title.kind {
        Some(MediaKind::Movie) if detail.title.status == Availability::Unknown => Offer::Request(Vec::new()),
        Some(MediaKind::Tv) => {
            let seasons = detail.requestable_seasons();
            if seasons.is_empty() { Offer::Nothing } else { Offer::Request(seasons) }
        }
        _ => Offer::Nothing,
    }
}

enum Outcome {
    Ran(SeerrOutcome),
    NotStarted,
}

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

/// What a [`StoreJob::Seerr`] carries: where, as whom, with which session, and what to ask.
#[derive(Clone)]
pub struct SeerrReq {
    address: String,
    auth: WatchlistAuth,
    held: Option<(SeerrSession, SeerrUser)>,
    op: SeerrOp,
}

impl PartialEq for SeerrReq {
    /// [`WatchlistAuth`] has no `PartialEq` of its own: equal when they ask the same of the same
    /// server as the same identity with the same session.
    fn eq(&self, other: &Self) -> bool {
        self.address == other.address && self.op == other.op && self.held == other.held
            && self.auth.credential == other.auth.credential && self.auth.generation == other.auth.generation
    }
}

impl std::fmt::Debug for SeerrReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeerrReq").field("op", &self.op).finish_non_exhaustive()
    }
}

/// A job's answer, opaque to the runner.
pub struct SeerrAnswer(SeerrOutcome);

/// Where a Seerr job's ONE answer goes: the store's mailbox as it was when the job was emitted,
/// and the I/O seam. A store that has since changed address or profile holds a new mailbox, and this
/// port still reaches the retired one nobody pumps.
#[derive(Clone)]
pub struct SeerrPort {
    mailbox: Arc<Mailbox>,
    io: Arc<dyn SeerrIo>,
}

impl PartialEq for SeerrPort {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.mailbox, &other.mailbox)
    }
}

impl std::fmt::Debug for SeerrPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SeerrPort({:p})", Arc::as_ptr(&self.mailbox))
    }
}

impl Port<SeerrAnswer> for SeerrPort {
    fn done(&self, addr: Addr, out: SeerrAnswer) {
        self.mailbox.post(Landing { req: addr.req.0, outcome: Outcome::Ran(out.0) });
    }

    /// The job was never started: its request is owed again (the store knows its slot and op).
    fn refused(&self, addr: Addr) {
        self.mailbox.post(Landing { req: addr.req.0, outcome: Outcome::NotStarted });
    }
}

fn seerr_addr(req: u32) -> Addr {
    Addr { to: MachineId::Store(StoreId::Seerr.ord()), req: RequestId(req) }
}

/// The job's IO body: run the op through the port's seam and post the one answer. A body that
/// unwinds lands as [`SeerrError::Transport`] with the session kept.
pub fn finish_seerr_job(port: &SeerrPort, addr: Addr, req: SeerrReq) {
    let SeerrReq { address, auth, held, op } = req;
    let io = &port.io;
    let outcome = catch_unwind(AssertUnwindSafe(|| run_op(io.as_ref(), &address, &auth.credential, held.as_ref(), &op)))
        .unwrap_or_else(|_| {
            plx_base::eventlog::log("seerr: a call panicked; it lands as no answer");
            SeerrOutcome { session: SessionChange::Kept, result: Err(SeerrError::Transport) }
        });
    port.done(addr, SeerrAnswer(outcome));
}

pub struct SeerrStore {
    /// The owner's address, as stored (`Preferences::seerr_address`); empty is off.
    address: String,
    auth: Option<WatchlistAuth>,
    session: Option<(SeerrSession, SeerrUser)>,
    connection: Connection,
    answers: HashMap<Slot, Answer>,
    /// Each slot's latest ask.
    asks: HashMap<Slot, u32>,
    /// What each ask in flight asked: a landing names only its request id.
    flights: HashMap<u32, (Slot, SeerrOp)>,
    /// Asks whose job the runner refused, with the frames waited so far.
    owed: HashMap<u32, u32>,
    /// The profile generation the last Connect was asked for, so the frame pass signs a profile in
    /// once and does not ask again every frame for one that cannot be.
    tried: Option<u32>,
    /// Each slot's latest ask, so a surface can tell whose answer it reads.
    asked: HashMap<Slot, SeerrOp>,
    /// The asks in flight that are refreshes: their slot keeps its `Ready` answer (module doc).
    refreshes: HashSet<u32>,
    /// A typed-in ask waiting for its term to settle, and the frames it has stood.
    wanted: Option<(Slot, SeerrOp, u32)>,
    req: u32,
    io: Arc<dyn SeerrIo>,
    mailbox: Arc<Mailbox>,
    jobs: Vec<StoreJob>,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for SeerrStore {
    fn default() -> Self {
        Self::with_io(Arc::new(LiveSeerrIo))
    }
}

impl SeerrStore {
    /// A store over a given I/O seam (tests script it; production is [`LiveSeerrIo`]).
    pub fn with_io(io: Arc<dyn SeerrIo>) -> Self {
        Self {
            address: String::new(),
            auth: None,
            session: None,
            connection: Connection::Off,
            answers: HashMap::new(),
            asks: HashMap::new(),
            flights: HashMap::new(),
            owed: HashMap::new(),
            tried: None,
            asked: HashMap::new(),
            refreshes: HashSet::new(),
            wanted: None,
            req: 0,
            io,
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

    fn bump(&self) {
        self.notice_dirty.store(true, Ordering::Relaxed);
        self.notice_gen.fetch_add(1, Ordering::Relaxed);
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// An address is set: the request surfaces are drawn (whether it answers is theirs to show).
    pub fn configured(&self) -> bool {
        !self.address.is_empty()
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn answer(&self, slot: &Slot) -> Option<&Answer> {
        self.answers.get(slot)
    }

    /// The detail of one title, once its [`Slot::Detail`] ask has landed.
    pub fn title_detail(&self, kind: MediaKind, tmdb: i64) -> Option<&TitleDetail> {
        match self.answer(&Slot::Detail(kind, tmdb))? {
            Answer::Ready(SeerrData::Detail(detail)) => Some(detail),
            _ => None,
        }
    }

    /// The servers a request of `kind` can go to, once their [`Slot::Servers`] ask has landed.
    pub fn servers(&self, kind: MediaKind) -> Option<&[ArrServer]> {
        match self.answer(&Slot::Servers(kind))? {
            Answer::Ready(SeerrData::Servers(servers)) => Some(servers),
            _ => None,
        }
    }

    /// One server's profiles and folders, once their [`Slot::ServerOptions`] ask has landed.
    pub fn server_options(&self, kind: MediaKind, server: i64) -> Option<&ArrOptions> {
        match self.answer(&Slot::ServerOptions(kind, server))? {
            Answer::Ready(SeerrData::ServerOptions(options)) => Some(options),
            _ => None,
        }
    }

    /// May the signed-in user choose where a request goes (`SeerrUser::may_choose_options`)? A
    /// request surface opens the request menu only then; `false` while nobody is signed in.
    pub fn may_choose_options(&self) -> bool {
        self.session.as_ref().is_some_and(|(_, user)| user.may_choose_options())
    }

    /// May the signed-in user approve and decline requests (`SeerrUser::may_manage_requests`)?
    pub fn may_manage_requests(&self) -> bool {
        self.session.as_ref().is_some_and(|(_, user)| user.may_manage_requests())
    }

    /// Does the request list hold everybody's requests for the signed-in user
    /// (`SeerrUser::sees_all_requests`), or only their own?
    pub fn sees_all_requests(&self) -> bool {
        self.session.as_ref().is_some_and(|(_, user)| user.sees_all_requests())
    }

    /// What `slot` was last asked: a surface reads [`Self::answer`] only when this is its question.
    pub fn asked(&self, slot: &Slot) -> Option<&SeerrOp> {
        self.asked.get(slot)
    }

    /// A typed-in ask: `op` for `slot` once it has stood unchanged for [`SETTLE_FRAMES`] frames
    /// ([`Self::take_due`]). The same op as the slot's last ask, or as the one already waiting, is
    /// nothing new.
    pub fn want(&mut self, slot: Slot, op: SeerrOp) {
        if self.asked.get(&slot) == Some(&op) || self.wanted.as_ref().is_some_and(|(s, o, _)| *s == slot && *o == op) {
            return;
        }
        self.wanted = Some((slot, op, 0));
    }

    /// Nothing is wanted of `slot` any more (Search's field holds no real query): forget the waiting
    /// ask, the slot's answer and its last ask.
    pub fn unwant(&mut self, slot: &Slot) {
        if self.wanted.as_ref().is_some_and(|(s, ..)| s == slot) {
            self.wanted = None;
        }
        if self.asked.remove(slot).is_some() | self.answers.remove(slot).is_some() {
            if let Some(req) = self.asks.remove(slot) {
                self.flights.remove(&req);
                self.owed.remove(&req);
                self.refreshes.remove(&req);
            }
            self.bump();
        }
    }

    /// One frame of the settle wait: the wanted ask, once it has stood long enough. The caller asks
    /// it ([`Self::ask`]) with the input it reads then.
    pub fn take_due(&mut self) -> Option<(Slot, SeerrOp)> {
        let (_, _, frames) = self.wanted.as_mut()?;
        *frames += 1;
        if *frames < SETTLE_FRAMES {
            return None;
        }
        self.wanted.take().map(|(slot, op, _)| (slot, op))
    }

    /// Forget the session, every answer and every ask in flight. A job not yet taken to a runner
    /// goes with them, credential and all.
    fn forget(&mut self) {
        self.session = None;
        self.answers.clear();
        self.asks.clear();
        self.flights.clear();
        self.owed.clear();
        self.asked.clear();
        self.refreshes.clear();
        self.wanted = None;
        self.mailbox = Arc::new(Mailbox::default());
        self.jobs.clear();
    }

    /// The stored address, read by the Bridge from the session. A different one forgets everything
    /// the old one answered; the next ask signs in there.
    pub fn set_address(&mut self, address: &str) {
        if self.address == address {
            return;
        }
        self.address = address.to_string();
        self.forget();
        self.tried = None;
        self.connection = if self.address.is_empty() { Connection::Off } else { Connection::Idle };
        self.bump();
    }

    /// Sign in afresh as the seated profile (Settings' Connect, a boot with an address set).
    pub fn connect(&mut self, input: &WatchlistInput) {
        self.tried = Some(input.profile().generation);
        self.ask(Slot::Connect, SeerrOp::Connect, input);
    }

    /// Does the frame pass owe the seated profile a sign-in? An address is set, nothing is signed
    /// in, and no Connect was asked yet for this profile (a failed one is retried by Settings, not
    /// every frame).
    pub fn needs_connect(&self, profile: &CurrentProfile) -> bool {
        self.connection == Connection::Idle && profile.user.is_some() && self.tried != Some(profile.generation)
    }

    /// Ask `op` for `slot` as the seated profile. The slot reads `Loading` until the answer lands;
    /// an older ask of the slot still in flight lands into nothing. With no address the slot fails
    /// at once; with nobody to sign in as, the connection stays idle and the slot fails.
    pub fn ask(&mut self, slot: Slot, op: SeerrOp, input: &WatchlistInput) {
        self.ask_keeping(slot, op, input, false);
    }

    /// [`Self::ask`], leaving a `Ready` answer shown until the new one lands, and in place if the new
    /// one fails (module doc). A slot with no `Ready` answer is asked as by [`Self::ask`].
    pub fn refresh(&mut self, slot: Slot, op: SeerrOp, input: &WatchlistInput) {
        self.ask_keeping(slot, op, input, true);
    }

    /// A screen's command, as the Bridge hands it on.
    pub fn run(&mut self, cmd: SeerrCmd, input: &WatchlistInput) {
        match cmd {
            SeerrCmd::Ask { slot, op } => self.ask(slot, op, input),
            SeerrCmd::Refresh { slot, op } => self.refresh(slot, op, input),
        }
    }

    fn ask_keeping(&mut self, slot: Slot, op: SeerrOp, input: &WatchlistInput, keep: bool) {
        if self.auth.as_ref().is_none_or(|a| !a.is_current(input.profile())) {
            if self.auth.take().is_some() {
                self.forget();
                if self.configured() {
                    self.connection = Connection::Idle;
                }
            }
            self.auth = input.auth().cloned();
        }
        if !self.configured() || self.auth.is_none() {
            let why = if self.configured() { SeerrError::Unauthorized } else { SeerrError::BadAddress };
            self.answers.insert(slot, Answer::Failed(why));
            self.bump();
            return;
        }
        if slot == Slot::Connect || self.session.is_none() {
            self.connection = Connection::Connecting;
        }
        let keep = keep && matches!(self.answers.get(&slot), Some(Answer::Ready(_)));
        self.asked.insert(slot.clone(), op.clone());
        if !keep {
            self.answers.insert(slot.clone(), Answer::Loading);
        }
        self.start(slot, op);
        if keep {
            self.refreshes.insert(self.req);
        } else {
            self.refreshes.remove(&self.req);
        }
        self.bump();
    }

    fn start(&mut self, slot: Slot, op: SeerrOp) {
        let Some(auth) = self.auth.clone() else { return };
        self.req = self.req.wrapping_add(1);
        if let Some(old) = self.asks.insert(slot.clone(), self.req) {
            self.flights.remove(&old);
            self.owed.remove(&old);
            if self.refreshes.remove(&old) {
                self.refreshes.insert(self.req);
            }
        }
        self.flights.insert(self.req, (slot, op.clone()));
        let req = SeerrReq { address: self.address.clone(), auth, held: self.session.clone(), op };
        self.jobs.push(StoreJob::Seerr(Job { addr: seerr_addr(self.req), req }, self.seerr_port()));
    }

    /// Take the jobs emitted since the last take, in order. The caller OWES each one to a runner.
    #[must_use]
    pub fn take_jobs(&mut self) -> Vec<StoreJob> {
        std::mem::take(&mut self.jobs)
    }

    pub fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// The port that lands an answer into THIS store's current mailbox.
    pub fn seerr_port(&self) -> SeerrPort {
        SeerrPort { mailbox: Arc::clone(&self.mailbox), io: Arc::clone(&self.io) }
    }

    /// Frame pass: ask again for what the runner refused, then take the landings. Returns whether
    /// anything a surface reads changed. A landing for a profile that is no longer seated forgets
    /// the session and everything it answered (the Bridge connects again for the new one). Like
    /// `ask`, it does NOT start the jobs it emitted.
    pub fn pump(&mut self, profile: &CurrentProfile) -> bool {
        let due: Vec<u32> = self.owed.iter_mut()
            .filter_map(|(req, wait)| { *wait += 1; (*wait >= RETRY_FRAMES).then_some(*req) })
            .collect();
        for req in due {
            self.owed.remove(&req);
            if let Some((slot, op)) = self.flights.remove(&req) {
                self.start(slot, op);
            }
        }
        let mut changed = false;
        for landing in self.mailbox.take_all() {
            let Some((slot, _)) = self.flights.get(&landing.req).cloned() else { continue };
            let outcome = match landing.outcome {
                Outcome::NotStarted => {
                    self.owed.insert(landing.req, 0);
                    continue;
                }
                Outcome::Ran(outcome) => outcome,
            };
            if self.auth.as_ref().is_some_and(|a| !a.is_current(profile)) {
                self.forget();
                self.auth = None;
                self.connection = if self.configured() { Connection::Idle } else { Connection::Off };
                self.bump();
                return true;
            }
            self.flights.remove(&landing.req);
            let refresh = self.refreshes.remove(&landing.req);
            self.land(slot, outcome, refresh);
            changed = true;
        }
        if changed {
            self.bump();
        }
        changed
    }

    fn land(&mut self, slot: Slot, outcome: SeerrOutcome, refresh: bool) {
        match outcome.session {
            SessionChange::Fresh(session, user) => {
                self.connection = Connection::Connected { name: user.name.clone() };
                self.session = Some((session, user));
            }
            SessionChange::Lost => {
                self.session = None;
                if let Err(e) = &outcome.result {
                    self.connection = Connection::of_sign_in_error(e);
                }
            }
            SessionChange::Kept => {}
        }
        let answer = match outcome.result {
            Ok(data) => Answer::Ready(data),
            Err(_) if refresh && matches!(self.answers.get(&slot), Some(Answer::Ready(_))) => return,
            Err(e) => Answer::Failed(e),
        };
        self.answers.insert(slot, answer);
    }
}

#[cfg(test)]
#[path = "seerr_tests.rs"]
mod tests;
