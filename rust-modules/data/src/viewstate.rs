//! viewstate — the server-side **view-state writes**, off the SDL thread.
//!
//! Three actions share one shape: Mark as Watched (`/:/scrobble`), Mark as Unwatched
//! (`/:/unscrobble`) and Remove from Continue Watching (`PUT /actions/removeFromContinueWatching`).
//! Each is one request to the item's own server, followed by a refresh of every surface that
//! describes that item — Home's shelves always, plus every Detail page that shows it (re-read from the read-back's landing).
//!
//! ## Why this module exists
//!
//! All of it used to run INLINE on the frame loop, straight off the key handler: the scrobble, then
//! `detail::refresh_view_state` (a blocking 2-5 round-trip re-read), then
//! a blocking `/hubs` + `/hubs/continueWatching` pair for the owned server. The justifying comment
//! priced it at "~100 ms LAN and deliberately so", which was true of
//! the one-LAN-server world it was written in. With a share registered, the item's server is
//! routinely a WAN address or a machine that is simply asleep, and every one of those calls is
//! bounded by `stream::CONNECT_TIMEOUT_MS` (2 s) plus a 15 s `SO_RCVTIMEO` — so ONE Mark as Watched
//! press could park the whole UI for seconds and, on a stalled read, for half a minute. `pms.rs`
//! names that exact hazard for hub FETCHES and fixed it there with a worker; this is the same
//! discipline applied to the WRITES.
//!
//! ## The shape
//!
//! * **The press is optimistic.** [`request`] flips what the shelves and the loaded detail item say
//!   about the item BEFORE anything leaves the machine ([`publish_view`], which hands a [`ViewChange`]
//!   to every store that holds rows for it, a show's or season's EPISODE rows included), so the tick, the veil and the resume bar all change on
//!   the frame of the press however far away the server is. The refresh that follows the write is
//!   the reconcile: it is the server's own answer, and it silently corrects an optimistic edit that
//!   turned out to be wrong. The per-item answer is the read-back ([`crate::stores::viewreconcile`]),
//!   which this module triggers through [`ViewStateState::take_reread`] for every landed write, taken
//!   or refused, and which publishes its result as [`ViewFact::Facts`]. A press made while a read of some store was already out is not lost to
//!   that read's pre-press body either: every published change is also recorded in the shared
//!   [`crate::viewledger::ViewLedger`], which each store consults when such a read lands. A server that never answers leaves the edit standing — the same "a
//!   failure never blanks a populated Home" rule `pms.rs` keeps for a fetch, applied to a write.
//! * **One write at a time, in order.** These are not idempotent reads: a watched/unwatched pair
//!   landing out of order leaves the server saying the opposite of what the user last asked for. So
//!   the queue is drained strictly one write at a time (one job out, answered, before the next
//!   leaves), and a second press on the SAME item
//!   replaces the queued one rather than joining it ([`coalesce`]) — the newest intent for an item
//!   is the only one worth a round trip.
//! * **One refresh per burst.** The hub refetch is deferred until the queue
//!   empties, so ticking four episodes watched in a row costs four writes and one refresh.
//! * **A refused launch is a retry, never a panic and never a fallback to blocking.** The runner
//!   declines a write when the OS is out of threads or the origin's client no longer carries the
//!   job's key; that is the job's `refused` answer, and the pump judges it again
//!   ([`ViewStateState::restart_refused`]): a moved profile, a vanished server or a re-tokened client
//!   drops it, and a write that survives that keeps its head and [`pump`] tries again after
//!   [`RETRY_FRAMES`], the same degradation `search.rs` and `browse.rs` apply to a refused fetch.
//!
//! ## The job lane
//!
//! The step starts no worker and reads no `client_for`; the one registry read left in it is
//! `fan_out_origin_watched`'s `server_ids()` (the one-server short-circuit), as the Person and
//! Search steps still read theirs. [`ViewStateState::kick`] decides from its
//! inputs, the borrowed [`ClientKeys`] and the `profile` the Bridge read from the session, and
//! emits one [`StoreJob::ViewWrite`] on the owner's outbox ([`ViewStateState::take_jobs`]); the
//! app's `StoreRunner` starts it, through the origin's client only if the registry still holds the
//! very client (and credential) the job names, and the job's [`ViewWritePort`] takes its one
//! answer, [`finish_view_write_job`]'s write result or a refusal.
//!
//! * **The profile and the key are inputs.** [`Req`] captures both at the press. A profile switch
//!   re-tokens the registry's clients IN PLACE, so a queued write's slot still answers, under the
//!   next profile's credential; a write is one household member's statement, so at the kick and
//!   when a refusal lands ([`ViewStateState::admit`]) a write whose profile moved, or whose client
//!   carries another key than the press named, is dropped and its ledger entry retired. The key
//!   compare is what covers the frames in which a switch has swapped the tokens and the new
//!   profile's identity is not published yet: there the profile still reads as the press's, and a
//!   switch looks like a refresh. So a genuine same-profile token refresh drops a press too (the
//!   read-back restores what the server holds); sending it on would need an identity that moves
//!   with the token swap, which is a Session owner change.
//! * **The fan-out reaches other servers through the snapshot.** The job carries the whole
//!   [`ClientKeys`] the step decided under, and the worker builds [`KeyedClients`] from it: a source
//!   whose client no longer carries the key is neither asked for its copy nor written to.
//! * **A refusal is not a failure.** [`Outcome::NotStarted`] changes no ledger entry and owes no
//!   read-back or hub refresh ([`ViewStateState::restart_refused`]); a worker that panics answers a
//!   failed write instead, never `NotStarted`, because the origin write may already be on the
//!   server and a second scrobble adds a second viewCount.
//!
//! ## Watch state follows the TITLE, not the server
//!
//! View state in Plex is per-server: two copies of one film on two servers are two independent
//! items with independent `viewCount`, and the rest of this app is built on exactly that
//! (`docs/shared-servers.md` §1). Marking something watched is nonetheless a statement about the
//! TITLE — "I have seen this" is not a fact about a file on a particular host — so a
//! [`Write::Watched`]/[`Write::Unwatched`] **fans out**: the worker resolves the item's portable
//! `guid` across every registered source and repeats the write on each copy it finds.
//!
//! Five things about that, each a decision rather than an implementation detail:
//!
//! * **The identity is the `guid`, never the `ratingKey`.** Every item-shaped integer Plex issues
//!   is server-local and dense from 1, so both servers in this household have a `ratingKey` 4
//!   (`docs/shared-servers.md` §1) — a fan-out matched on the key would confidently mark a
//!   DIFFERENT film watched on the other machine, which is a bug this repo has already had once
//!   (`ui::detail`'s note on posting a borrowed item's key to our own server).
//!   [`plx_plex::plex::Client::find_by_guid`] is the lookup, the same one "Also available" resolves
//!   its rows with.
//! * **No resume position is ever COPIED from one server to another — only the watched flag
//!   travels.** This is the subtle half. A resume offset is about a file you are streaming from one
//!   host, and `screens::alt_sources`' module doc reasons that out at length; that reasoning stands. So
//!   `unscrobble` IS repeated on the other copies (it is the "unwatched" end of one control, and
//!   clearing the flag is the same statement about the title), but no `viewOffset` is read here and
//!   none is pushed anywhere.
//!   **One consequence has to be said plainly, because "the position does not travel" does not
//!   sound like it:** [`Write::Unwatched`] is `/:/unscrobble`, which clears `viewCount` **and**
//!   `viewOffset` — so marking a title unwatched DISCARDS the other copies' resume points too, the
//!   same thing the press does to the copy in hand. That is what "I have not seen this" means, and
//!   it is the reason [`Write::Watched`] and [`Write::Unwatched`] are not symmetric in cost: a
//!   scrobble adds a fact, an unscrobble throws two away, on every source that holds the title.
//! * **[`Write::RemoveFromDeck`] does NOT fan out** ([`Write::propagates`]). The deck is a
//!   per-server surface, and taking a friend's item off *your* Continue Watching is not what that
//!   row promises.
//! * **Resolved at WRITE time, on the worker.** Not from a detail page's earlier cross-source
//!   resolve: the press can come from a Home / Library / Search card menu where none ever ran, and
//!   a behaviour that only worked after visiting a page is a behaviour that is wrong most of the
//!   time. A press that carries no guid gets one looked up from its own `(server, ratingKey)`.
//! * **Best-effort per source, and unconditional.** A source that is asleep or refuses costs its
//!   own log line and nothing else — the write the user pressed has already reported, and one
//!   unreachable share must never fail or retry it. There is no setting: this app has no
//!   preferences screen by design.
//!
//! The cost is real and priced deliberately: a press on a two-source install is one write, one guid
//! query per source, and one write per other copy — every one of them on the worker and none on the
//! frame loop. The queue still drains ONE worker at a time, so a burst of four episodes pays it four
//! times in sequence rather than in parallel; that serialisation is the point rather than an
//! oversight (see [`kick`]), and the single deferred refresh at the end of a burst is unchanged.
//!
//! **What the other copies look like on the press frame: unchanged, and that is deliberate.**
//! [`request`]'s optimistic edit reaches only this server's copy of the item and its descendants
//! (`ViewAt::covers`), because no other server's key is KNOWN yet — the resolve is what discovers them, and it is off-thread by construction.
//! The copies are flipped locally when the fan-out reports ([`ViewStateState::pump`] publishes a
//! [`ViewChange`] for each
//! one the server took), and the hub refetch that already follows every write reconciles whatever
//! that missed. So the copy in hand changes at once and the others change a round trip later,
//! which is the same contract the press already has with its own server.
//!
//! Physical ownership now follows Browse's pattern: `stores::viewstate::ViewStateStore` owns this
//! module's main-thread `ViewStateState`, an `Arc<ViewStateAdapter>` mailbox captured by every
//! job's port, and its notice. Reset rotates the adapter, and request IDs fence malformed or stale
//! completions even within one adapter. There is no process-global ViewState transport or state.

use crate::stores::{StoreId, StoreJob};
use plx_machine::job::{Job, Port};
use plx_machine::machine::{Addr, MachineId, RequestId};
use plx_plex::plex::{Client, ClientKey, ClientKeys, KeyedClients, ServerId};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

/// How much of the library one view-state change reaches, read off the row the user acted on.
///
/// A leaf is one movie or episode. A show or season also changes every episode under it
/// (`/:/scrobble` on a container scrobbles each leaf), and any store that holds one of those
/// episode rows — a Search result, a Continue Watching card, a Related tile — must say so too,
/// not only the container's own row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// One movie or episode: exactly the row `(sid, rk)`.
    Leaf,
    /// A show: its own row, its season rows and every episode row whose `show_rk` is `rk`.
    Show,
    /// A season of `show_rk`: its own row and every episode row of that show and season number.
    Season { show_rk: String, index: i64 },
    /// The caller could not say (a fan-out copy on another server whose parentage was never read):
    /// matched as a leaf, so an unknown scope can only under-reach, never touch an unrelated row.
    Unknown,
}

impl Scope {
    /// The scope of the row the user is acting on (`PmsMovie::kind`: 1 show, 2 season).
    pub fn of_row(m: &crate::pms::PmsMovie) -> Scope {
        match m.kind {
            1 => Scope::Show,
            2 if !m.show_rk.is_empty() => {
                Scope::Season { show_rk: m.show_rk.clone(), index: i64::from(m.season_index) }
            }
            _ => Scope::Leaf,
        }
    }
}

/// What a finished playback tells the view state: the position its `stopped` timeline POST
/// carried, and whether the item was already watched when the play began. Filled by the media layer
/// when that POST is attempted or abandoned (`route::take_landed_stops`), consumed by
/// [`crate::stores::Stores::stops_landed`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopRecord {
    pub sid: ServerId,
    pub rk: String,
    pub pos_ms: i64,
    pub dur_ms: i64,
    /// The server took the POST. A refused or abandoned one still patches the tile (the viewer did
    /// watch up to there) but asks for no read-back, which would undo it.
    pub ok: bool,
    /// Watched when the play began (read off the play queue, fetched at play start). An end of
    /// stream fans out only when this is false and the read-back finds the item watched.
    pub was_watched: bool,
}

/// What became true about the item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewFact {
    /// The item is now watched (`true`) or unwatched (`false`).
    Watched(bool),
    /// The item was hidden from Continue Watching and nothing else about it changed.
    LeftDeck,
    /// What the server SAID about the item when the read-back asked ([`crate::stores::viewreconcile`]):
    /// the three fields of its watch state, as `pms::parse_item` reads them.
    Facts { watched: bool, unwatched: bool, resume_ms: i64 },
    /// A stopped playback reported this position (`ms` of `dur_ms`) for the item: the resume point
    /// the server was just told. Our own word, not the server's: a read-back corrects it when the server
    /// took the stop, and a refused or abandoned stop leaves it until the item is next read.
    Offset { ms: i64, dur_ms: i64 },
}

/// One change to what the app knows about an item's view state, published to every store that
/// shows it (`viewstate::publish_view`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewChange {
    pub sid: ServerId,
    pub rk: String,
    pub scope: Scope,
    pub fact: ViewFact,
}

/// The share of an item's duration at which PMS marks it watched and clears its `viewOffset`
/// (`docs/pms-api.md` §6: "~90%"). Not [`crate::metadata::resume_ns`]'s 95%, which is where the
/// client stops OFFERING a resume.
const WATCHED_PERCENT: i64 = 90;

/// The edit a store applies to the rows a [`ViewAt`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewEdit {
    /// `pms::set_watched`: the watched flag, its unwatched twin and the resume offset move together.
    Watched(bool),
    /// Leave Continue Watching; every other field of the row stands.
    LeftDeck,
    /// Set the three watch-state fields to what the server answered. Never recorded in the
    /// [`crate::viewledger::ViewLedger`]: it is the server's word, not a press.
    Facts { watched: bool, unwatched: bool, resume_ms: i64 },
    /// Set the resume point and nothing else: a stop below the server's watched threshold leaves
    /// `viewCount` alone (so `watched`/`unwatched` stand), and one past it is corrected by the
    /// read-back that follows every stop the server TOOK (`StopRecord::ok`; a refused or abandoned
    /// POST owes no read). Like `Facts`, never recorded in the ledger.
    Offset { ms: i64 },
}

/// WHICH rows a store edit reaches: the item and the scope it was published with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewAt {
    pub sid: ServerId,
    pub rk: String,
    pub scope: Scope,
}

impl ViewChange {
    pub fn at(&self) -> ViewAt {
        ViewAt { sid: self.sid, rk: self.rk.clone(), scope: self.scope.clone() }
    }

    pub fn edit(&self) -> ViewEdit {
        match self.fact {
            ViewFact::Watched(on) => ViewEdit::Watched(on),
            ViewFact::LeftDeck => ViewEdit::LeftDeck,
            ViewFact::Facts { watched, unwatched, resume_ms } => {
                ViewEdit::Facts { watched, unwatched, resume_ms }
            }
            // never past the end of what was playing; at the watched threshold PMS clears
            // `viewOffset` (docs/pms-api.md, "Client behavior expected by PMS"), so there is no point
            ViewFact::Offset { ms, dur_ms } => ViewEdit::Offset {
                ms: if dur_ms > 0 && ms.saturating_mul(100) >= dur_ms.saturating_mul(WATCHED_PERCENT) {
                    0
                } else {
                    ms.max(0)
                },
            },
        }
    }
}

impl ViewAt {
    /// Exactly the row `(sid, rk)`, nothing under it.
    pub fn leaf(sid: ServerId, rk: &str) -> ViewAt {
        ViewAt { sid, rk: rk.to_string(), scope: Scope::Leaf }
    }

    /// Is `(sid, rk)` the addressed item itself?
    pub fn is_item(&self, sid: ServerId, rk: &str) -> bool {
        plx_plex::plex::same_item((sid, rk), (self.sid, &self.rk))
    }

    /// Does a row with these identities fall under the change: the item itself, or a descendant
    /// the scope reaches? `kind` is `PmsMovie::kind` (2 season, 3 episode).
    pub fn covers_parts(&self, sid: ServerId, rk: &str, kind: i32, show_rk: &str, season_index: i64) -> bool {
        if self.is_item(sid, rk) {
            return true;
        }
        if sid != self.sid || show_rk.is_empty() {
            return false;
        }
        match &self.scope {
            Scope::Show => matches!(kind, 2 | 3) && show_rk == self.rk,
            Scope::Season { show_rk: want, index } => {
                kind == 3 && show_rk == want && season_index == *index
            }
            Scope::Leaf | Scope::Unknown => false,
        }
    }

    pub fn covers(&self, m: &crate::pms::PmsMovie) -> bool {
        self.covers_parts(m.sid, &m.rk, m.kind, &m.show_rk, i64::from(m.season_index))
    }
}

impl ViewEdit {
    /// Would [`ViewEdit::apply`] alter this row? A press always counts (the optimistic edit is the
    /// moment the screen answers, and the same press twice is still a press); the server's `Facts`
    /// count only when they differ from the row, so a read-back that agrees with the optimistic
    /// edit rebuilds nothing.
    pub fn changes(self, m: &crate::pms::PmsMovie) -> bool {
        match self {
            ViewEdit::Facts { watched, unwatched, resume_ms } => {
                m.watched != watched || m.unwatched != unwatched || m.resume_ms != resume_ms
            }
            ViewEdit::Offset { ms } => m.resume_ms != ms,
            ViewEdit::Watched(_) | ViewEdit::LeftDeck => true,
        }
    }
    /// Apply to one row already known to be covered. `LeftDeck` changes no field of a row — leaving
    /// the deck is a membership change the deck's owner makes.
    pub fn apply(self, m: &mut crate::pms::PmsMovie) {
        match self {
            ViewEdit::Watched(on) => crate::pms::set_watched(m, on),
            ViewEdit::Facts { watched, unwatched, resume_ms } => {
                m.watched = watched;
                m.unwatched = unwatched;
                m.resume_ms = resume_ms;
            }
            ViewEdit::Offset { ms } => m.resume_ms = ms,
            ViewEdit::LeftDeck => {}
        }
    }
}

impl Write {
    /// What this write, once requested, makes true.
    pub fn fact(self) -> ViewFact {
        match self {
            Write::Watched => ViewFact::Watched(true),
            Write::Unwatched => ViewFact::Watched(false),
            Write::RemoveFromDeck => ViewFact::LeftDeck,
        }
    }
}

/// What to ask the item's server for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Write {
    /// `/:/scrobble` — mark watched (on a show or season, every leaf).
    Watched,
    /// `/:/unscrobble` — mark unwatched; clears `viewCount` **and** `viewOffset`.
    Unwatched,
    /// `PUT /actions/removeFromContinueWatching` — HIDE from the deck, keeping the resume point.
    RemoveFromDeck,
}

impl Write {
    /// The toggle this write belongs to. [`coalesce`] replaces a queued write only within one
    /// family: Watched and Unwatched are two ends of ONE control, so a later press supersedes an
    /// earlier one, while a deck removal is a different decision about the same item and must not
    /// be swallowed by a watched toggle that happened to follow it.
    fn family(self) -> u8 {
        match self {
            Write::Watched | Write::Unwatched => 0,
            Write::RemoveFromDeck => 1,
        }
    }

    /// Does this write follow the TITLE across every source, or is it about ONE server?
    ///
    /// Watched/Unwatched are a claim about the title, so they fan out (see the module doc); a deck
    /// removal is a per-server surface and stays on the machine it was pressed on.
    fn propagates(self) -> bool {
        matches!(self, Write::Watched | Write::Unwatched)
    }

    /// Perform it. WORKER THREAD — `c` was resolved by the runner, through the key the step named.
    fn perform(self, c: &plx_plex::plex::Client, rk: &str) -> bool {
        match self {
            Write::Watched => c.scrobble(rk),
            Write::Unwatched => c.unscrobble(rk),
            Write::RemoveFromDeck => c.remove_from_continue_watching(rk),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Write::Watched => "watched",
            Write::Unwatched => "unwatched",
            Write::RemoveFromDeck => "deck-remove",
        }
    }
}

/// One queued write. Identity only — no client is held: [`ViewStateState::kick`] decides from the
/// step's inputs (the borrowed [`ClientKeys`] and the profile) and sends the runner a job that
/// names the client by key, and the runner starts it only through that very client.
/// `pms::kick_with`'s rule ("capture at the step": decide on the main thread) says the worker must
/// never ask which server is current; naming a SLOT's key there is that rule, and it also means a
/// share whose registration went away while this sat in the queue is turned away with a log line
/// instead of being sent to whatever now occupies the slot.
struct Req {
    /// Unique among this owner's writes and monotone; also the job's request id. A `u32` so it
    /// fits there: four billion writes is not a lifetime.
    id: u32,
    /// Where [`crate::viewledger::ViewLedger::record`] numbered this press. The landing renumbers
    /// (`landed`) or retires that exact entry; a newer press that replaced it is left alone.
    seq: crate::viewledger::Stamp,
    sid: ServerId,
    /// The client the step chose for `sid` when the write was queued, as the borrowed
    /// [`ClientKeys`] named it. [`ViewStateState::admit`] re-reads the slot from its own keys and
    /// drops the write when the client carries another key by then, whatever [`Req::profile`] says.
    key: ClientKey,
    /// The profile the press was made under (`Bridge::profile_scope`, the Session owner's scope
    /// counter), an input to the step like the keys. A write whose profile has moved by the time it
    /// is sent belongs to someone else's household: it is dropped and retired, never sent under the
    /// new profile's credential.
    profile: u32,
    rk: String,
    w: Write,
    /// How far down the library the press reaches (a show's episodes, a season's).
    scope: Scope,
    /// The item's PORTABLE identity (`plex://movie/…`) **as it is on `sid` for `rk`**, or empty when
    /// the press had none in hand — [`fan_out`] then resolves it from the pair, on the worker.
    ///
    /// It must be the guid OF `rk` and of nothing else. The detail page's hero toggle knows it
    /// (`metadata::Detail::guid`, the same pair the page is mounted on), while the card context
    /// menu's rk can be an EPISODE of the show whose guid that page happens to be holding — which
    /// is why `app.rs` passes nothing at all rather than something close: a wrong guid does not
    /// fail, it marks a different title watched on every other source.
    guid: String,
    /// The origin server already holds this write (an end of stream it scrobbled itself): skip
    /// `perform` there — a second scrobble would add a second viewCount — and run only the fan-out
    /// to the other copies of the title.
    origin_done: bool,
}

/// The worker's ONLY output: whether the item's own server took the write it was given, plus the
/// OTHER sources' copies of the same title the fan-out also wrote — `(server, ratingKey)` each, and
/// only the ones that were TAKEN, since by the time this is filled the answer is known and there is
/// no reason to flip a copy the server refused. Empty for a write that does not propagate.
#[derive(Default)]
pub struct Done {
    ok: bool,
    also: Vec<(ServerId, String)>,
}

/// How a write came back to the mailbox.
enum Outcome {
    /// The worker ran: the origin write, if it took one, and the fan-out. A failed write is
    /// `ok: false` here, and so is a worker that panicked (the origin write may already be on the
    /// server, and a second scrobble would add a second viewCount).
    Written(Done),
    /// The write was never started: the runner refused the job. Nothing left the device, so it is
    /// not a failure — it changes no ledger entry, owes no read-back or hub refresh, and goes back
    /// to be judged again ([`ViewStateState::restart_refused`]).
    NotStarted,
}

struct Completion {
    id: u32,
    outcome: Outcome,
}

/// The one write a [`StoreJob::ViewWrite`] carries: everything the worker needs, captured by the
/// step. `key` is the origin's client as the step decided it; the runner starts the write only if
/// the registry still holds that client at the launch. `keys` is the WHOLE snapshot the step decided
/// under: the fan-out reaches the other copies only through [`KeyedClients`] built from it.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewWriteReq {
    sid: ServerId,
    key: ClientKey,
    rk: String,
    w: Write,
    guid: String,
    origin_done: bool,
    keys: ClientKeys,
}

impl ViewWriteReq {
    /// The server to write to and the client the step decided to write it through.
    pub fn server(&self) -> (ServerId, ClientKey) { (self.sid, self.key) }
}

/// Where a write's ONE answer goes: the owner's mailbox, captured with the job when it was emitted.
/// A store that has since been reset holds a new adapter, and this port still reaches the one the
/// write was made from, the retired one nobody pumps. Two ports are equal when they reach the same
/// adapter.
#[derive(Clone)]
pub struct ViewWritePort(Arc<ViewStateAdapter>);

impl ViewWritePort {
    fn of(adapter: &Arc<ViewStateAdapter>) -> Self { Self(Arc::clone(adapter)) }

    fn post(&self, id: u32, outcome: Outcome) {
        *self.0.mail.lock().unwrap_or_else(|e| e.into_inner()) = Some(Completion { id, outcome });
    }
}

impl PartialEq for ViewWritePort {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.0, &other.0) }
}

impl std::fmt::Debug for ViewWritePort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ViewWritePort({:p})", Arc::as_ptr(&self.0))
    }
}

impl Port<Done> for ViewWritePort {
    /// The write's result, under its id.
    fn done(&self, addr: Addr, out: Done) {
        self.post(addr.req.0, Outcome::Written(out));
    }

    /// The write was never started, so nothing will land: say so under its id, which is what puts
    /// it back in the queue (or drops it, if the profile or the client moved).
    fn refused(&self, addr: Addr) {
        self.post(addr.req.0, Outcome::NotStarted);
    }
}

/// A write's address: this store and its id. Ids are unique, so no two writes share an address.
fn view_write_addr(id: u32) -> Addr {
    Addr { to: MachineId::Store(StoreId::ViewState.ord()), req: RequestId(id) }
}

/// The write's IO body and its completion: write to `origin` (unless it already holds the write),
/// fan the write out to the title's other copies through the key-checked `clients`, and post the
/// result to `port`. `lookup` is the registry read the host passes in; a copy whose client no
/// longer carries the key the step decided under is not written to. The answer is posted OUTSIDE
/// the guard, so a write that panics lands as a failure and never as a missing answer — and never
/// as "not started", because the origin write may already be on the server.
pub fn finish_view_write_job(
    port: &ViewWritePort,
    addr: Addr,
    origin: &'static Client,
    lookup: fn(ServerId) -> Option<&'static Client>,
    req: ViewWriteReq,
) {
    let done = catch_unwind(AssertUnwindSafe(|| {
        let clients = KeyedClients::new(&req.keys, lookup);
        let ok = req.origin_done || req.w.perform(origin, &req.rk);
        // The fan-out runs INSIDE this worker rather than beside it. The queue is drained one
        // worker at a time precisely so two writes for one item cannot land out of order, and a
        // second thread doing the other sources would put those copies outside that ordering — a
        // watched/unwatched pair could then settle differently per server, which is the one thing
        // this whole feature exists to stop.
        let also = if req.w.propagates() {
            fan_out(origin, clients, req.sid, &req.rk, &req.guid, req.w)
        } else {
            Vec::new()
        };
        Done { ok, also }
    }))
    .unwrap_or_else(|_| {
        plx_base::eventlog::log("viewstate: write worker panicked");
        Done::default()
    });
    port.done(addr, done);
}

/// Cross-thread transport for one physical owner. Every job's port captures the exact adapter that
/// emitted it; reset replaces the owner's `Arc`, fencing any completion still headed here.
#[derive(Default)]
pub struct ViewStateAdapter {
    mail: Mutex<Option<Completion>>,
}

/// Main-thread state for one ViewState owner. Nothing in this value selects another owner.
pub struct ViewStateState {
    /// Every change published is recorded here first, so a read already in flight is re-patched
    /// when it lands (`crate::viewledger`). `stores::Stores` shares it with the stores that read.
    pub(crate) ledger: crate::viewledger::ViewLedger,
    queue: Vec<Req>,
    sent: Option<Req>,
    retry_cd: u32,
    want_hubs: bool,
    /// Items whose write landed (taken or refused) and whose truth the read-back worker is owed,
    /// handed over by [`ViewStateState::take_reread`] once the burst is idle.
    want_reread: Vec<(ServerId, String)>,
    next_request_id: u32,
    /// The writes the step has sent and nobody has taken to a runner yet ([`Self::take_jobs`]).
    /// Not cleared by a reset: the job answers into the adapter it was emitted under, which the
    /// owning store rotated out, so the new state never sees its answer.
    jobs: Vec<StoreJob>,
    /// The player is on top: the terminal hub refetch stays owed (`want_hubs`) instead of running
    /// under the video. Writes and landings are untouched.
    pub(crate) hold_refresh: bool,
}

impl Default for ViewStateState {
    fn default() -> Self {
        Self {
            ledger: Default::default(),
            queue: Vec::new(),
            sent: None,
            retry_cd: 0,
            want_hubs: false,
            want_reread: Vec::new(),
            next_request_id: 1,
            jobs: Vec::new(),
            hold_refresh: false,
        }
    }
}

/// ~2 s at 60 fps — the same wait `search.rs` gives a refused spawn (here, a refused launch). Without it, a process at the
/// thread ceiling would re-attempt (and log a refusal) every single frame.
const RETRY_FRAMES: u32 = 120;

impl ViewStateState {
    fn mint_request_id(&mut self) -> u32 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.checked_add(1)
            .expect("viewstate request identity exhausted");
        id
    }

    /// Is a write queued or in flight? What gates the deferred refresh, and the predicate any
    /// future "saving…" affordance would read.
    pub fn is_busy(&self) -> bool {
        !self.queue.is_empty() || self.sent.is_some()
    }

/// Ask the item's server to change `(sid, rk)`'s view state, and update every surface that describes
/// it AT ONCE. **MAIN THREAD, NON-BLOCKING.** `changed` is false when the write never left — the one
/// case being a server slot that holds no client, which is a dropped PMS write and says so in the log
/// rather than being the one that goes unrecorded. The `endpoints` are the ones the optimistic edit's
/// Hubs step settled (see [`publish_view`]); the caller carries them.
///
/// A Detail page the write touched is not named here: the read-back of the item (and of its season
/// and show) lands the server's word, and every Detail page on the stack that shows one of those
/// rows is re-read from it (`Stores::take_landed_views`). The hub refetch happens whichever screen
/// asked — Continue Watching changes with any of these three writes.
///
/// `guid` is the item's portable identity **if the caller already holds the one belonging to `rk`**,
/// and `""` otherwise — see [`Req::guid`], and note that "" is a perfectly ordinary answer rather
/// than a degraded one: the worker looks it up. It is what a watched write FANS OUT on, so that a
/// title held by more than one source ends up watched on all of them (module doc). It is ignored
/// for [`Write::RemoveFromDeck`], which stays on the server it was pressed on.
    fn request(
    &mut self,
    adapter: &Arc<ViewStateAdapter>,
    sid: ServerId,
    rk: &str,
    w: Write,
    scope: Scope,
    guid: &str,
    keys: &ClientKeys,
    profile: u32,
    browse: &mut dyn FnMut(crate::stores::browse::BrowseCmd) -> bool,
    hubs: &mut dyn FnMut(crate::stores::hubs::HubsCmd) -> crate::stores::StoreOutcome,
    person: &mut dyn FnMut(crate::stores::person::PersonCmd) -> bool,
    collection: &mut dyn FnMut(crate::stores::collection::CollectionCmd) -> bool,
    search: &mut dyn FnMut(crate::stores::search::SearchCmd) -> bool,
    metadata: &mut dyn FnMut(crate::stores::metadata::MetadataCmd) -> bool,
) -> crate::stores::StoreOutcome {
    // The slot's own key, never "the current server": the item may live on a share, and a scrobble
    // sent to the wrong machine marks a DIFFERENT film watched there (both servers number their
    // items from 1). None is a slot the registry holds no client for — a view-state write is exactly
    // the operation to skip and log rather than take to the wrong machine.
    let Some(key) = keys.key(sid) else {
        plx_base::eventlog::log(&format!(
            "viewstate: rk={rk} {} DROPPED — server {} is not registered",
            w.name(),
            sid.raw()
        ));
        return crate::stores::StoreOutcome::default();
    };
    // OPTIMISTIC, before the request: the press must land on the panel now, not one WAN round trip
    // from now. Only THIS copy — the other sources' keys are not known until the fan-out resolves
    // them, which is what [`pump`] finishes the job with.
    let change = ViewChange { sid, rk: rk.to_string(), scope: scope.clone(), fact: w.fact() };
    let seq = self.ledger.record(&change.at(), change.edit());
    let endpoints = publish_view(change, browse, hubs, person, collection, search, metadata);
    self.coalesce(sid, rk, w);
    let id = self.mint_request_id();
    self.queue.push(Req {
        id,
        seq,
        sid,
        key,
        profile,
        rk: rk.to_string(),
        w,
        scope,
        guid: guid.to_string(),
        origin_done: false,
    });
    self.kick(adapter, keys, profile);
    crate::stores::StoreOutcome { changed: true, endpoints }
    }
}

/// Publish one view-state change to every LOCAL surface that holds rows for it, synchronously, and
/// ask for a repaint. Each store applies the [`ViewEdit`] to the rows the change [covers]
/// ([`ViewAt::covers`]) and raises its own notice; every store is a no-op where the item does not
/// appear, so a Home press with no detail page mounted costs one catalog walk and nothing else.
/// This is the one door: both callers hand it the per-store closures they were given.
///
/// Called from two places, for one reason. [`request`] calls it optimistically for the copy the
/// user pressed, and [`pump`] calls it for each OTHER source's copy the fan-out reached — which
/// cannot be known any earlier than that, because discovering them is the round trip.
///
/// **Every store that can be DRAWING the item, not just the hub catalog.** The context menu opens
/// on Home, the Library grid, a Search result, a person's filmography, a collection's members and
/// the detail page (whose Related shelf has a menu of its own), and each of those keeps its own
/// rows. A watched change reaches all six owners (Home's hubs, Browse's listing and section
/// shelves, Search, Person, Collection and the loaded Detail); a deck removal reaches the two decks
/// (Home's Continue Watching and the Library's own).
///
/// Through the store VOCABULARY (`crate::stores`) so every store a change touches raises its own
/// notice and a migrated screen hears the optimistic edit the same way it hears a landing.
pub fn publish_view(
    change: ViewChange,
    browse: &mut dyn FnMut(crate::stores::browse::BrowseCmd) -> bool,
    hubs: &mut dyn FnMut(crate::stores::hubs::HubsCmd) -> crate::stores::StoreOutcome,
    person: &mut dyn FnMut(crate::stores::person::PersonCmd) -> bool,
    collection: &mut dyn FnMut(crate::stores::collection::CollectionCmd) -> bool,
    search: &mut dyn FnMut(crate::stores::search::SearchCmd) -> bool,
    metadata: &mut dyn FnMut(crate::stores::metadata::MetadataCmd) -> bool,
) -> crate::stores::EndpointRefreshSet {
    let (at, edit) = (change.at(), change.edit());
    // A Hubs step settles the launches the runner refused since the last one, whichever command it
    // runs, so this edit's step can owe endpoint refreshes: they leave with the caller.
    let endpoints = hubs(crate::stores::hubs::HubsCmd::ApplyView { at: at.clone(), edit }).endpoints;
    browse(crate::stores::browse::BrowseCmd::ApplyView { at: at.clone(), edit });
    // The Library's own deck is a different shelf on a different screen, and the one place a
    // deck removal is reachable from besides Home; no other store has a deck. Watched, Facts and
    // Offset reach every store that can be drawing the item; Detail's own item, episodes and
    // seasons take no Facts (the application re-reads the page whole when the read-back lands,
    // `app::content::reconcile_details`), only the Offset, on the loaded item and its episode.
    if !matches!(edit, ViewEdit::LeftDeck) {
        metadata(crate::stores::metadata::MetadataCmd::ApplyView { at: at.clone(), edit });
        search(crate::stores::search::SearchCmd::ApplyView { at: at.clone(), edit });
        person(crate::stores::person::PersonCmd::ApplyView { at: at.clone(), edit });
        collection(crate::stores::collection::CollectionCmd::ApplyView { at, edit });
    }
    plx_machine::idle::invalidate(); // the tick/veil/bar just changed with no spring behind it
    endpoints
}

/// Drop any QUEUED write for the same item and toggle — see [`Write::family`]. The write already in
/// flight (`ViewStateState::sent`) is deliberately untouched: it is on the wire, the client cannot
/// recall it, and
/// the one that supersedes it is queued behind it in the order pressed.
impl ViewStateState {
    /// The origin server's own end of stream made `(sid, rk)` watched (a first watch, seen by the
    /// read-back): run only the fan-out of that watched write to the title's other copies, through
    /// this same serial queue so it stays ordered with the user's toggles. `false` when the item
    /// already has a write queued or out — a press newer than the stream's end wins, and its own
    /// fan-out (or its absence) decides.
    pub(crate) fn fan_out_origin_watched(&mut self, adapter: &Arc<ViewStateAdapter>, sid: ServerId, rk: &str,
        keys: &ClientKeys, profile: u32) -> bool {
        // One server has no other copy to reach: the request would only cost a second hub refetch.
        let Some(key) = keys.key(sid) else { return false };
        if plx_plex::plex::server_ids().nth(1).is_none()
            || self.queue.iter().chain(self.sent.iter()).any(|r| plx_plex::plex::same_item((r.sid, &r.rk), (sid, rk)))
        {
            return false;
        }
        let id = self.mint_request_id();
        self.queue.push(Req {
            id, seq: 0, sid, key, profile, rk: rk.to_string(), w: Write::Watched, scope: Scope::Leaf,
            guid: String::new(), origin_done: true,
        });
        self.kick(adapter, keys, profile);
        true
    }

    fn coalesce(&mut self, sid: ServerId, rk: &str, w: Write) {
    let fam = w.family();
    self.queue.retain(|r| !(plx_plex::plex::same_item((r.sid, &r.rk), (sid, rk)) && r.w.family() == fam));
    }

/// Spend one frame of the refused-launch ladder; true when the next attempt is due. Split out so the
/// ladder is gradeable without a registry or a socket — `pms::retry_due`'s twin.
    fn retry_tick(&mut self) -> bool {
        let cd = self.retry_cd;
        if cd > 0 {
            self.retry_cd = cd - 1;
        }
        cd <= 1
    }

/// Start the next queued write if nothing is out. MAIN THREAD.
    ///
    /// The decision is made from the step's inputs: `keys` is the registry's answer as of this
    /// call and `profile` the profile the Session owner has seated now ([`Self::admit`]). A write
    /// goes out as a [`StoreJob::ViewWrite`] the Bridge hands to the runner in the same call; the
    /// runner starts it only through the client its key names. This reads no registry and starts
    /// no thread.
    fn kick(&mut self, adapter: &Arc<ViewStateAdapter>, keys: &ClientKeys, profile: u32) {
        if self.sent.is_some() || self.retry_cd > 0 {
            return;
        }
        while !self.queue.is_empty() {
            let req = self.queue.remove(0);
            let Some(req) = self.admit(req, keys, profile) else { continue };
            // An end-of-stream request writes nothing on its origin: say what it does.
            let what = if req.origin_done { "fanout-only" } else { req.w.name() };
            plx_base::eventlog::log(&format!(
                "viewstate: rk={} {} → server {} (sent to the runner)",
                req.rk,
                what,
                req.sid.raw()
            ));
            self.jobs.push(StoreJob::ViewWrite(
                Job {
                    addr: view_write_addr(req.id),
                    req: ViewWriteReq {
                        sid: req.sid,
                        key: req.key,
                        rk: req.rk.clone(),
                        w: req.w,
                        guid: req.guid.clone(),
                        origin_done: req.origin_done,
                        keys: keys.clone(),
                    },
                },
                ViewWritePort::of(adapter),
            ));
            self.sent = Some(req);
            return;
        }
    }

    /// May `req` still go out, judged from the step's inputs? `None`: it was dropped, its ledger
    /// entry retired (it never left, so nothing will ever confirm it), and the log says why.
    ///
    /// - A profile other than the one it was pressed under: DROPPED. A profile switch re-tokens the
    ///   registry's clients in place, so the slot still answers, under the NEW profile's credential;
    ///   a write is a statement about one household member's viewing record and is never sent as
    ///   another's.
    /// - A server that holds no client now: DROPPED, with a line, so no PMS write goes unrecorded.
    /// - A client that carries another key than the press named (re-tokened since): DROPPED, with a
    ///   line, even when `profile` still reads as the press's. A switch swaps the tokens in the
    ///   frame the session adopts them and publishes the new profile's identity by a LATER commit,
    ///   so in between it is indistinguishable from a same-profile refresh, and sending then would
    ///   write one household member's press into the next one's record. The cost is a press dropped
    ///   on a genuine refresh; the read-back restores what the server holds.
    fn admit(&mut self, req: Req, keys: &ClientKeys, profile: u32) -> Option<Req> {
        if req.profile != profile {
            self.drop_queued(&req, "the profile changed while it was queued");
            return None;
        }
        let Some(key) = keys.key(req.sid) else {
            self.drop_queued(&req, &format!("server {} left the registry while queued", req.sid.raw()));
            return None;
        };
        if key != req.key {
            self.drop_queued(&req, &format!("server {} was re-tokened while it was queued", req.sid.raw()));
            return None;
        }
        Some(req)
    }

    /// Drop a write that will not go out: the log says why and its ledger entry is retired (it never
    /// left, so nothing will ever confirm it).
    fn drop_queued(&mut self, req: &Req, why: &str) {
        plx_base::eventlog::log(&format!("viewstate: rk={} {} DROPPED — {why}", req.rk, req.w.name()));
        self.ledger.retire(req.seq);
    }

    /// A write the runner never started ([`Outcome::NotStarted`]): nothing left the device. It goes
    /// back to the HEAD of the queue — dropping it would silently lose a press the user has already
    /// seen take effect on screen — after the same judgment [`Self::admit`] makes of any queued
    /// write: a moved profile, a vanished server or a re-tokened client drops it. A write that
    /// survives it met a client that did not move, so the refusal was the system's (the thread
    /// ceiling) and the retry waits out the ladder, exactly as `search.rs`/`browse.rs` do.
    fn restart_refused(&mut self, req: Req, keys: &ClientKeys, profile: u32) {
        plx_base::eventlog::log(&format!(
            "viewstate: rk={} {} not started by the runner",
            req.rk,
            req.w.name()
        ));
        let Some(req) = self.admit(req, keys, profile) else { return };
        self.queue.insert(0, req);
        self.retry_cd = RETRY_FRAMES;
    }

    /// Take the writes the step has sent since the last take, in order. The caller OWES each one to
    /// a runner: a job dropped here is a write that is never answered, and the queue behind it
    /// never starts.
    #[must_use]
    pub fn take_jobs(&mut self) -> Vec<StoreJob> {
        std::mem::take(&mut self.jobs)
    }

    /// Does the outbox hold a job nobody has taken yet?
    pub fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

/// MAIN THREAD, once a frame, ROUTE-UNCONDITIONAL — a landing must never depend on which screen is
/// mounted, because the user can walk off Home (or off the detail page) between the press and the
/// answer, and the refresh is owed either way.
pub fn pump_with_gate(
    &mut self,
    adapter: &Arc<ViewStateAdapter>,
    gate: &plx_machine::landgate::Gate,
    keys: &ClientKeys,
    profile: u32,
    browse: &mut dyn FnMut(crate::stores::browse::BrowseCmd) -> bool,
    hubs: &mut dyn FnMut(crate::stores::hubs::HubsCmd) -> crate::stores::StoreOutcome,
    person: &mut dyn FnMut(crate::stores::person::PersonCmd) -> bool,
    collection: &mut dyn FnMut(crate::stores::collection::CollectionCmd) -> bool,
    search: &mut dyn FnMut(crate::stores::search::SearchCmd) -> bool,
    metadata: &mut dyn FnMut(crate::stores::metadata::MetadataCmd) -> bool,
) -> crate::stores::EndpointRefreshSet {
    let mut endpoints = crate::stores::EndpointRefreshSet::default();
    let due = self.retry_tick();
    // the landing GATE (§3.3 step 3, `plx_machine::landgate`): under a replay the server's answer is taken
    // on the frame the recording took it on. The retry tick and `kick` below stay outside it.
    // DUMP MODE: owed while a write is on the wire (`sent`, set when its job is emitted and cleared by the
    // landing it answers). A completion that answers no in-flight request is dropped INSIDE the take
    // (and logged), so it neither ends the wait nor stands in for the one owed.
    let landed = crate::stores::take_landing_owed(gate, crate::stores::StoreId::ViewState,
        || self.sent.is_some(), || {
        let completion = adapter.mail.lock().unwrap_or_else(|e| e.into_inner()).take()?;
        if self.sent.as_ref().is_some_and(|request| request.id == completion.id) {
            return Some(completion);
        }
        plx_base::eventlog::log(&format!(
            "viewstate: completion {} ignored — in-flight identity is {}",
            completion.id,
            self.sent.as_ref().map(|request| request.id.to_string())
                .unwrap_or_else(|| "none".into()),
        ));
        None
    });
    let landed = match landed {
        Some(Completion { outcome: Outcome::Written(done), .. }) => Some(done),
        Some(Completion { outcome: Outcome::NotStarted, .. }) => {
            let r = self.sent.take().expect("the exact in-flight request remains present");
            self.restart_refused(r, keys, profile);
            None
        }
        None => None,
    };
    if let Some(done) = landed {
        let r = self.sent.take().expect("the exact in-flight request remains present");
        plx_base::eventlog::log(&format!(
            "viewstate: rk={} {} ok={} others={}",
            r.rk,
            r.w.name(),
            done.ok as i32,
            done.also.len()
        ));
        // The ledger learns the outcome of THIS press: a taken write is renumbered so a read that
        // was requested after the press but answered before the write arrived still replays it; a
        // refused one is retired so it stops replaying over the server's own answer.
        if done.ok {
            self.ledger.landed(r.seq);
        } else {
            self.ledger.retire(r.seq);
        }
        // The other sources' copies could not be flipped at the press — their keys are exactly
        // what the resolve DISCOVERS — so they are flipped here, on the main thread, and only
        // the ones their server took. A press that reached no other source does nothing here,
        // which is every press on a one-server install.
        for (osid, ork) in &done.also {
            // A copy on another server keeps the SHOW scope (its episodes hang off its own key) but
            // never a season's: the season's parent key is the origin server's, so the copy is
            // matched as the one row it is.
            let scope = match &r.scope { Scope::Show => Scope::Show, Scope::Leaf => Scope::Leaf,
                Scope::Season { .. } | Scope::Unknown => Scope::Unknown };
            let change = ViewChange { sid: *osid, rk: ork.clone(), scope, fact: r.w.fact() };
            // the server took this copy already, so its number is the landing's
            self.ledger.record(&change.at(), change.edit());
            endpoints.merge(publish_view(change, browse, hubs, person, collection, search, metadata));
        }
        // The refresh is owed whether or not the server took it: on success it is the reconcile,
        // and on failure it is what puts the optimistic edit back to whatever the server really
        // says. A server that can answer neither question leaves the shelves exactly as they
        // are — `pms.rs`'s per-source rule, unchanged.
        // (An end-of-stream fan-out that reached no copy changed nothing here: its origin's
        // read-back, which asked for it, already owes the refresh.)
        self.want_hubs |= !(r.origin_done && done.also.is_empty());
        // A watched/unwatched write is re-read whatever the server said, on the item and on every
        // copy the fan-out took: on success the answer confirms the optimistic edit, on refusal
        // it is the only thing that puts the tile back. A deck removal changes no field a read
        // reports, so it asks nothing (the hub refetch above reconciles the deck).
        if r.w.propagates() {
            // an end-of-stream fan-out wrote nothing on the origin, whose read-back is what
            // triggered it: only the copies it reached are owed one
            let origin = (!r.origin_done).then(|| (r.sid, r.rk.clone()));
            for key in origin.into_iter().chain(done.also.iter().cloned()) {
                if !self.want_reread.contains(&key) {
                    self.want_reread.push(key);
                }
            }
        }
    }
    if due {
        self.kick(adapter, keys, profile);
    }
    if self.is_busy() || self.hold_refresh {
        return endpoints; // a burst still has writes to send — one refresh at the end of it, not per write
    }
    let refresh_hubs = std::mem::take(&mut self.want_hubs);
    if refresh_hubs {
        endpoints.merge(hubs(crate::stores::hubs::HubsCmd::RefetchHubs).endpoints);
        // the same staleness, one screen over: a library's own shelves carry watch state and its
        // own Continue Watching row, so the burst that made Home's hubs stale made these stale too
        browse(crate::stores::browse::BrowseCmd::HubsInvalidateAll);
        plx_machine::idle::invalidate();
    }
    endpoints
}

/// [`Self::pump_with_gate`] on the fixture gate with no registry (empty keys) and profile 0: a write
/// that is kicked here finds no client, as a host test with nothing registered always has.
#[cfg(any(test, feature = "test-support"))]
pub fn pump(
    &mut self,
    adapter: &Arc<ViewStateAdapter>,
    browse: &mut dyn FnMut(crate::stores::browse::BrowseCmd) -> bool,
    hubs: &mut dyn FnMut(crate::stores::hubs::HubsCmd) -> crate::stores::StoreOutcome,
    person: &mut dyn FnMut(crate::stores::person::PersonCmd) -> bool,
    collection: &mut dyn FnMut(crate::stores::collection::CollectionCmd) -> bool,
    search: &mut dyn FnMut(crate::stores::search::SearchCmd) -> bool,
    metadata: &mut dyn FnMut(crate::stores::metadata::MetadataCmd) -> bool,
) -> crate::stores::EndpointRefreshSet {
    self.pump_with_gate(adapter, plx_machine::landgate::fixture_gate(), &ClientKeys::default(), 0, browse, hubs,
        person, collection, search, metadata)
}

#[cfg(any(test, feature = "test-support"))]
pub fn owe_hubs_refresh_for_test(&mut self) {
    self.want_hubs = true;
}

#[cfg(any(test, feature = "test-support"))]
pub fn hold_inflight_for_test(&mut self, sid: ServerId, rk: &str) {
        let id = self.mint_request_id();
        self.sent = Some(Req {
            id,
            seq: 0,
            sid, key: UNKEYED, profile: 0,
            rk: rk.into(),
            w: Write::Watched,
            scope: Scope::Leaf,
            guid: String::new(),
            origin_done: false,
        });
}

/// The items the read-back worker is owed, once this owner's burst is idle (four presses in a row cost
/// one batch).
pub fn take_reread(&mut self) -> Vec<(ServerId, String)> {
    if self.is_busy() { return Vec::new(); }
    std::mem::take(&mut self.want_reread)
}

}

/// The key a hand-built [`Req`] of a fixture carries: no registry stands behind it.
#[cfg(any(test, feature = "test-support"))]
const UNKEYED: ClientKey = ClientKey { instance: 0, token: 0 };

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, PartialEq, Eq)]
pub struct OwnershipFixture {
    pub queue: Vec<String>,
    pub sent: Option<String>,
    pub retry_cd: u32,
    pub want_hubs: bool,
    pub mail: bool,
}

#[cfg(any(test, feature = "test-support"))]
impl ViewStateState {
    fn fixture_req(&mut self, rk: &str) -> Req {
        let id = self.mint_request_id();
        Req {
            id,
            seq: 0,
            sid: ServerId::UNSET, key: UNKEYED, profile: 0,
            rk: rk.into(),
            w: Write::Watched,
            scope: Scope::Leaf,
            guid: String::new(),
            origin_done: false,
        }
    }

    pub fn seed_ownership_fixture(&mut self, adapter: &Arc<ViewStateAdapter>) {
        self.reset();
        let queued = self.fixture_req("queued");
        let flight = self.fixture_req("flight");
        let id = flight.id;
        self.queue.push(queued);
        self.sent = Some(flight);
        self.retry_cd = 9;
        self.want_hubs = true;
        *adapter.mail.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Completion { id, outcome: Outcome::Written(Done::default()) });
    }

    pub fn ownership_fixture(&self, adapter: &Arc<ViewStateAdapter>) -> OwnershipFixture {
        OwnershipFixture {
            queue: self.queue.iter().map(|r| r.rk.clone()).collect(),
            sent: self.sent.as_ref().map(|r| r.rk.clone()),
            retry_cd: self.retry_cd,
            want_hubs: self.want_hubs,
            mail: adapter.mail.lock().unwrap_or_else(|e| e.into_inner()).is_some(),
        }
    }

    pub fn late_completion(
        &self,
        adapter: &Arc<ViewStateAdapter>,
    ) -> Box<dyn FnOnce()> {
        let id = self.sent.as_ref().expect("an old request is held").id;
        let adapter = Arc::clone(adapter);
        Box::new(move || {
            *adapter.mail.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Completion { id, outcome: Outcome::Written(Done::default()) });
        })
    }

    pub fn seed_post_reset_flight(&mut self) {
        let req = self.fixture_req("post-reset");
        self.sent = Some(req);
    }

}

// ---- the fan-out: one title, every source that holds it ---------------------------------------

/// One source's answer to *"do you hold this guid, and under which key?"*. `None` is **did not
/// answer**, `Some(keys)` is what it holds — `Some(empty)` included, which is the source saying it
/// does not have the title. The two are deliberately not collapsed, for the same reason
/// [`plx_plex::plex::Client::find_by_guid`] refuses to collapse them: a share that is merely asleep is
/// not a share that lacks the film, and only one of those is a fact about a library.
type Answer = (ServerId, Option<Vec<String>>);

/// WORKER. Repeat `w` on every OTHER copy of this title, and report the ones the server took so the
/// main thread can flip them locally. **Best-effort throughout**: nothing here can fail, delay or
/// retry the write that has already gone out, and a source that never answers costs one log line.
///
/// `c` is the item's OWN client — the one the runner resolved through the key the step named and the
/// write itself went out on, handed down rather than re-resolved from `sid` here. That is
/// `pms::kick_with`'s capture-at-the-step rule (decide on the main thread; a worker never asks the
/// registry about the server it was given), and it also keeps [`resolved_guid`]'s failure honest: a
/// slot re-pointed or revoked mid-write cannot turn into a log line claiming the item has no
/// portable id.
///
/// `clients` reaches every OTHER source, and only while that source's client still carries the key
/// the step's snapshot holds. A profile switch re-tokens the registry's clients in place, so a
/// fan-out begun under one profile would otherwise send its lookups and its writes to the other
/// servers under the next profile's credential, into another person's viewing record. A source the
/// snapshot never held, or whose credential moved, is answered "no answer" and gets no request.
fn fan_out(
    c: &plx_plex::plex::Client,
    clients: KeyedClients<'_>,
    sid: ServerId,
    rk: &str,
    guid: &str,
    w: Write,
) -> Vec<(ServerId, String)> {
    let sources: Vec<ServerId> = plx_plex::plex::server_ids().collect();
    if sources.len() < 2 {
        // A one-server install pays NOTHING for this feature: no guid lookup, no query, no line in
        // the log. Checked before the lookup below for exactly that reason.
        return Vec::new();
    }
    let Some(guid) = resolved_guid(c, rk, guid) else {
        // Never a silent no-op. With two sources registered, a title that cannot be identified
        // portably is one whose watch state WILL disagree between them, and this line is the only
        // place that says so.
        plx_base::eventlog::log(&format!(
            "viewstate: fanout SKIPPED — rk={rk} on server {} has no guid",
            sid.raw()
        ));
        return Vec::new();
    };
    let answers = ask_sources(&sources, clients, &guid);
    let mut done = Vec::new();
    for (id, key) in fanout_targets((sid, rk), &answers) {
        // `dst`, not a second `c`: shadowing the item's own client inside the one loop that writes
        // to OTHER machines is how a key ends up posted to the wrong server, which is the exact
        // failure this whole function is arranged around.
        let Some(dst) = clients.client(id) else {
            continue;
        };
        let ok = w.perform(dst, &key);
        plx_base::eventlog::log(&format!(
            "viewstate: fanout {guid} {} → server {} rk={key} ok={}",
            w.name(),
            id.raw(),
            ok as i32
        ));
        if ok {
            done.push((id, key));
        }
    }
    done
}

/// WORKER. The guid to fan out on: the one the press carried, or — when it carried none, which is
/// every press from a Home / Library / Search card menu, since a catalog row (`pms::PmsMovie`) does
/// not hold one — the item's own record, read off its own server.
///
/// The lookup is what makes the "resolve at write time" rule hold: the fan-out must not require a
/// prior detail-page visit, because a card menu is a place no cross-source resolve has ever run. It
/// costs one extra GET, on this worker, and only where a second source is registered. `None` is a
/// server that did not answer or an item it has no portable id for — both leave the caller with a
/// log line rather than a quiet nothing.
fn resolved_guid(c: &plx_plex::plex::Client, rk: &str, given: &str) -> Option<String> {
    if !given.is_empty() {
        return Some(given.to_string()); // the press knew it; no round trip at all
    }
    let guid = c.metadata(rk).map(|m| m.guid).unwrap_or_default();
    (!guid.is_empty()).then_some(guid)
}

/// WORKER. Ask every source for its copy of `guid`, one line per source with its outcome.
///
/// A guid is a public metadata id and is safe to log; **a machine name, an address or a token is
/// not**, and none appears here — a source is named by its registry slot number, which means
/// nothing off this device. (This repo is public, and a friend's server details are theirs.)
fn ask_sources(sources: &[ServerId], clients: KeyedClients<'_>, guid: &str) -> Vec<Answer> {
    sources
        .iter()
        .map(|&id| {
            let keys = clients.client(id)
                .and_then(|c| c.find_by_guid(guid))
                .map(|mc| {
                    mc.metadata
                        .iter()
                        .map(|m| m.rating_key.clone())
                        .collect::<Vec<_>>()
                });
            let outcome = match &keys {
                None => "no answer".to_string(),
                Some(k) if k.is_empty() => "not held".to_string(),
                Some(k) => format!("holds {}", k.len()),
            };
            plx_base::eventlog::log(&format!(
                "viewstate: fanout {guid} server {}: {outcome}",
                id.raw()
            ));
            (id, keys)
        })
        .collect()
}

/// The copies a fan-out must write, given what each source answered — PURE, so the identity rule is
/// graded on the host rather than inferred from a device log.
///
/// The `origin` PAIR is dropped, and only that pair: the write the user pressed has already gone
/// out on it, so repeating it is a round trip that changes nothing. A source answering with a
/// SECOND key for the same guid is kept — the origin server included — because that is another item
/// in that library holding the same title, and the claim being propagated is about the title.
/// Duplicate pairs are collapsed, so a server that listed one key twice is written once.
fn fanout_targets(origin: (ServerId, &str), answers: &[Answer]) -> Vec<(ServerId, String)> {
    let mut out: Vec<(ServerId, String)> = Vec::new();
    for (id, keys) in answers {
        for k in keys.iter().flatten() {
            let pair = (*id, k.as_str());
            let listed = out
                .iter()
                .any(|(s, e)| plx_plex::plex::same_item((*s, e), pair));
            if listed || plx_plex::plex::same_item(pair, origin) {
                continue;
            }
            out.push((*id, k.clone()));
        }
    }
    out
}

impl ViewStateState {
/// Drop everything — the identity-change twin of `pms::reset`/`browse::reset`, called from the same
/// place. The request counter deliberately survives: identities remain monotone across reset, while
/// the owning store rotates the adapter so an old worker has no route into the replacement owner.
fn reset(&mut self) {
    self.queue.clear();
    self.sent = None;
    self.retry_cd = 0;
    self.want_hubs = false;
    self.want_reread.clear();
    // The edits describe the identity that just changed (sign-in, profile or server switch): they
    // must never be made again on a read of the next one's rows.
    self.ledger.reset();
}

pub fn run(
    &mut self,
    adapter: &Arc<ViewStateAdapter>,
    cmd: crate::stores::viewstate::ViewStateCmd,
    keys: &ClientKeys,
    profile: u32,
    browse: &mut dyn FnMut(crate::stores::browse::BrowseCmd) -> bool,
    hubs: &mut dyn FnMut(crate::stores::hubs::HubsCmd) -> crate::stores::StoreOutcome,
    person: &mut dyn FnMut(crate::stores::person::PersonCmd) -> bool,
    collection: &mut dyn FnMut(crate::stores::collection::CollectionCmd) -> bool,
    search: &mut dyn FnMut(crate::stores::search::SearchCmd) -> bool,
    metadata: &mut dyn FnMut(crate::stores::metadata::MetadataCmd) -> bool,
) -> crate::stores::StoreOutcome {
    use crate::stores::viewstate::ViewStateCmd;
    match cmd {
        ViewStateCmd::Request { sid, rk, write, scope, guid } => {
            self.request(adapter, sid, &rk, write, scope, &guid, keys, profile, browse, hubs, person, collection,
                search, metadata)
        }
        ViewStateCmd::Reset => {
            self.reset();
            crate::stores::StoreOutcome::changed(true)
        }
    }
}
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    #[test]
    fn owner_callback_receives_the_optimistic_browse_edit_before_run_returns() {
        let _g = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test(
            "viewstate-owner", "127.0.0.1", 9, "synthetic", "fixture");
        let mut store = crate::stores::viewstate::ViewStateStore::default();
        store.hold_inflight_for_test(sid, "held");
        let mut browse = Vec::new();
        let mut hubs = Vec::new();
        let mut person = Vec::new();
        let mut collection = Vec::new();
        let mut search = Vec::new();
        let mut metadata_store = crate::stores::metadata::MetadataStore::default();
        // the page is mounted on the pressed item: an edit that matches nothing raises no notice
        crate::metadata::install_for_test(metadata_store.state_mut(), Some(crate::metadata::Detail {
            sid, rk: "7".into(), ..Default::default()
        }));
        let _ = metadata_store.take_notice();

        assert!(store.run(
            crate::stores::viewstate::ViewStateCmd::Request {
                sid,
                rk: "7".into(),
                write: Write::Watched,
                scope: Scope::Leaf,
                    guid: String::new(),
            },
            &crate::pms::registry_keys_for_test(),
            0,
            &mut |cmd| { browse.push(cmd); true },
            &mut |cmd| {
                hubs.push(cmd);
                crate::stores::StoreOutcome::default()
            },
            &mut |cmd| { person.push(cmd); true },
            &mut |cmd| { collection.push(cmd); true },
            &mut |cmd| { search.push(cmd); true },
            &mut |cmd| metadata_store.run(cmd),
        ).changed);

        assert!(matches!(hubs.as_slice(), [crate::stores::hubs::HubsCmd::ApplyView {
            at, edit: ViewEdit::Watched(true)
        }] if at.sid == sid && at.rk == "7" && at.scope == Scope::Leaf));
        assert!(matches!(browse.as_slice(), [crate::stores::browse::BrowseCmd::ApplyView {
            at, edit: ViewEdit::Watched(true)
        }] if at.sid == sid && at.rk == "7" && at.scope == Scope::Leaf));
        assert!(matches!(person.as_slice(), [crate::stores::person::PersonCmd::ApplyView {
            at, edit: ViewEdit::Watched(true)
        }] if at.sid == sid && at.rk == "7" && at.scope == Scope::Leaf));
        assert!(matches!(collection.as_slice(), [crate::stores::collection::CollectionCmd::ApplyView {
            at, edit: ViewEdit::Watched(true)
        }] if at.sid == sid && at.rk == "7" && at.scope == Scope::Leaf), "a collection member's disc flips with the press");
        assert!(matches!(search.as_slice(), [crate::stores::search::SearchCmd::ApplyView {
            at, edit: ViewEdit::Watched(true)
        }] if at.sid == sid && at.rk == "7" && at.scope == Scope::Leaf));
        assert!(metadata_store.take_notice().is_some(),
            "the optimistic edit reaches {} before run returns", crate::stores::StoreId::Metadata.name());
        assert!(store.take_notice().is_some(), "the owning ViewState store notices its command");
        crate::metadata::install_for_test(metadata_store.state_mut(), None);
        plx_plex::plex::reset_servers_for_test();
    }

    /// Search holds the show S and three of its episodes (two in season 1, one in season 2) plus an
    /// unrelated episode of another show. Returns the Stores and the sid.
    fn search_with_show_and_episodes() -> (crate::stores::Stores, ServerId) {
        use crate::search::{Item, Kind, Shelf};
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test(
            "viewstate-scope", "127.0.0.1", 9, "synthetic", "fixture");
        let row = |rk: &str, kind: i32, show_rk: &str, season_index: i32| Item::Media(crate::pms::PmsMovie {
            sid, rk: rk.into(), kind, show_rk: show_rk.into(), season_index,
            unwatched: true, resume_ms: 5_000, ..Default::default()
        });
        let mut stores = crate::stores::Stores::default();
        stores.search.publish_shelves_for_test(vec![
            Shelf { epoch: 0, window: Default::default(), kind: Kind::Show,
                items: vec![row("S", 1, "", 0)] },
            Shelf { epoch: 0, window: Default::default(), kind: Kind::Episode,
                items: vec![row("e11", 3, "S", 1), row("e12", 3, "S", 1), row("e21", 3, "S", 2),
                    row("other", 3, "T", 1)] },
        ]);
        stores.viewstate.borrow_mut().hold_inflight_for_test(sid, "held");
        (stores, sid)
    }

    /// `(rk, watched, unwatched, resume_ms)` of every Search row, in shelf order.
    fn search_marks(stores: &crate::stores::Stores) -> Vec<(String, bool, bool, i64)> {
        let directory = crate::stores::browse::DirectorySnapshot::default();
        let snap = stores.search_snapshot(directory.view(), &[]);
        snap.view().shelves().iter().flat_map(|s| &s.items).filter_map(|it| match it {
            crate::search::Item::Media(m) => Some((m.rk.clone(), m.watched, m.unwatched, m.resume_ms)),
            _ => None,
        }).collect()
    }

    #[test]
    fn marking_a_show_watched_marks_every_episode_row_search_holds() {
        let _g = plx_base::testlock::serial();
        let (mut stores, sid) = search_with_show_and_episodes();
        let directory = crate::stores::browse::DirectorySnapshot::default();
        assert!(stores.viewstate_run(crate::stores::viewstate::ViewStateCmd::Request {
            sid, rk: "S".into(), write: Write::Watched, scope: Scope::Show, guid: String::new(),
        }, directory.view(), &crate::pms::registry_keys_for_test(), 0).changed);
        let marks = search_marks(&stores);
        let mark = |rk: &str| marks.iter().find(|m| m.0 == rk).unwrap().clone();
        assert_eq!(mark("S"), ("S".into(), true, false, 0), "the show row itself");
        for rk in ["e11", "e12", "e21"] {
            assert_eq!(mark(rk), (rk.into(), true, false, 0), "episode {rk} of the show reads watched");
        }
        assert_eq!(mark("other"), ("other".into(), false, true, 5_000), "another show's episode is untouched");
        drop(stores);
        plx_plex::plex::reset_servers_for_test();
    }

    /// The press is recorded in the publish order the stores share, once, at the scope it was
    /// pressed with — the half of the ledger a read landing later depends on.
    #[test]
    fn a_press_is_recorded_in_the_ledger_the_stores_share() {
        let _g = plx_base::testlock::serial();
        let (mut stores, sid) = search_with_show_and_episodes();
        let directory = crate::stores::browse::DirectorySnapshot::default();
        let before = stores.view_ledger.stamp();
        assert!(stores.viewstate_run(crate::stores::viewstate::ViewStateCmd::Request {
            sid, rk: "S".into(), write: Write::Watched, scope: Scope::Show, guid: String::new(),
        }, directory.view(), &crate::pms::registry_keys_for_test(), 0).changed);
        let seen = stores.view_ledger.since(before);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, ViewAt { sid, rk: "S".into(), scope: Scope::Show });
        assert_eq!(seen[0].1, ViewEdit::Watched(true));
        drop(stores);
        plx_plex::plex::reset_servers_for_test();
    }

    /// The server's word (`Facts`, `Offset`) patches Search's rows in place, only where it differs
    /// from what is drawn, and returns "nothing changed" otherwise so no notice is raised.
    #[test]
    fn the_servers_facts_and_offset_patch_the_search_rows_only_where_they_differ() {
        let _g = plx_base::testlock::serial();
        let (mut stores, sid) = search_with_show_and_episodes();
        let directory = crate::stores::browse::DirectorySnapshot::default();
        let run = |stores: &mut crate::stores::Stores, rk: &str, edit| {
            stores.search_run(crate::stores::search::SearchCmd::ApplyView { at: ViewAt::leaf(sid, rk), edit }, directory.view())
        };
        let facts = ViewEdit::Facts { watched: true, unwatched: false, resume_ms: 0 };
        assert!(run(&mut stores, "other", facts), "the server says watched: the row changes");
        assert!(!run(&mut stores, "other", facts), "the same facts again change nothing, so raise nothing");
        let marks = search_marks(&stores);
        let mark = |rk: &str| marks.iter().find(|m| m.0 == rk).unwrap().clone();
        assert_eq!(mark("other"), ("other".into(), true, false, 0));
        assert_eq!(mark("e11"), ("e11".into(), false, true, 5_000), "no other row moved");

        assert!(run(&mut stores, "e11", ViewEdit::Offset { ms: 9_000 }));
        assert!(!run(&mut stores, "e11", ViewEdit::Offset { ms: 9_000 }), "a stop at the offset already held");
        let marks = search_marks(&stores);
        assert_eq!(marks.iter().find(|m| m.0 == "e11").unwrap().3, 9_000);
        assert!(!marks.iter().find(|m| m.0 == "e11").unwrap().1, "an offset leaves the watched state alone");
        drop(stores);
        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn marking_a_show_unwatched_clears_every_episode_row_search_holds() {
        let _g = plx_base::testlock::serial();
        let (mut stores, sid) = search_with_show_and_episodes();
        let directory = crate::stores::browse::DirectorySnapshot::default();
        for rk in ["e11", "e12", "e21"] {
            stores.search_run(crate::stores::search::SearchCmd::ApplyView {
                at: ViewAt::leaf(sid, rk), edit: ViewEdit::Watched(true) }, directory.view());
        }
        assert!(stores.viewstate_run(crate::stores::viewstate::ViewStateCmd::Request {
            sid, rk: "S".into(), write: Write::Unwatched, scope: Scope::Show, guid: String::new(),
        }, directory.view(), &crate::pms::registry_keys_for_test(), 0).changed);
        for (rk, watched, unwatched, _) in search_marks(&stores).into_iter().filter(|m| m.0 != "other") {
            assert!(!watched && unwatched, "{rk} reads unwatched after its show was marked unwatched");
        }
        drop(stores);
        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn marking_a_season_watched_marks_only_that_seasons_episode_rows() {
        let _g = plx_base::testlock::serial();
        let (mut stores, sid) = search_with_show_and_episodes();
        let directory = crate::stores::browse::DirectorySnapshot::default();
        assert!(stores.viewstate_run(crate::stores::viewstate::ViewStateCmd::Request {
            sid, rk: "S1".into(), write: Write::Watched,
            scope: Scope::Season { show_rk: "S".into(), index: 1 }, guid: String::new(),
        }, directory.view(), &crate::pms::registry_keys_for_test(), 0).changed);
        let marks = search_marks(&stores);
        let watched = |rk: &str| marks.iter().find(|m| m.0 == rk).unwrap().1;
        assert!(watched("e11") && watched("e12"), "season 1's episodes");
        assert!(!watched("e21"), "season 2's episode is not in the season");
        assert!(!watched("S") && !watched("other"));
        drop(stores);
        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn stores_route_the_optimistic_edit_to_the_addressed_browse_owner() {
        let _g = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test(
            "viewstate-stores-owner", "127.0.0.1", 9, "synthetic", "fixture");
        let mut selected = crate::stores::Stores::default();
        selected.browse.borrow_mut().seed_registered_table_for_test([sid, sid]);
        selected.browse.borrow_mut().seed_items_for_test(1);
        let decoy = crate::stores::Stores::default();
        selected.viewstate.borrow_mut().hold_inflight_for_test(sid, "held");
        let directory = crate::stores::browse::DirectorySnapshot::default();

        assert!(selected.viewstate_run(crate::stores::viewstate::ViewStateCmd::Request {
            sid,
            rk: "1".into(),
            write: Write::Watched,
            scope: Scope::Leaf,
            guid: String::new(),
        }, directory.view(), &crate::pms::registry_keys_for_test(), 0).changed);

        assert!(!selected.browse.borrow_mut().listing_snapshot().view().item(0).unwrap().unwatched,
            "the selected owner changes before the command returns");
        assert!(decoy.browse.borrow_mut().listing_snapshot().view().item(0).is_none(),
            "the unaddressed decoy is not used by the owner-aware callback");
        drop((selected, decoy));
        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn owner_callback_receives_fanout_edits_and_the_terminal_hubs_invalidation() {
        let _g = plx_base::testlock::serial();
                let origin = ServerId::from_raw(0);
        let other = ServerId::from_raw(1);
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        let id = state.mint_request_id();
        state.sent = Some(Req { id, seq: 0, sid: origin, key: UNKEYED, profile: 0, rk: "7".into(), w: Write::Watched,
            scope: Scope::Leaf, guid: "plex://movie/7".into(), origin_done: false });
        *adapter.mail.lock().unwrap_or_else(|e| e.into_inner()) = Some(Completion {
            id,
            outcome: Outcome::Written(Done { ok: true, also: vec![(other, "70".into())] }),
        });
        let mut browse = Vec::new();
        let mut hubs = Vec::new();
        let mut person = Vec::new();

        let _ = state.pump(&adapter,
            &mut |cmd| { browse.push(cmd); true },
            &mut |cmd| {
                hubs.push(cmd);
                crate::stores::StoreOutcome::default()
            },
            &mut |cmd| { person.push(cmd); true },
            &mut |_| false,
            &mut |_| false,
            &mut |_| false,
        );

        assert!(matches!(&hubs[0], crate::stores::hubs::HubsCmd::ApplyView {
            at, edit: ViewEdit::Watched(true)
        } if at.sid == other && at.rk == "70"));
        assert!(matches!(hubs[1], crate::stores::hubs::HubsCmd::RefetchHubs),
            "the fan-out edit lands before the terminal reconcile request");
        assert!(matches!(&browse[0], crate::stores::browse::BrowseCmd::ApplyView {
            at, edit: ViewEdit::Watched(true)
        } if at.sid == other && at.rk == "70"));
        assert!(matches!(browse[1], crate::stores::browse::BrowseCmd::HubsInvalidateAll));
        assert_eq!(person.len(), 1, "the fan-out reaches the supplied Person owner callback");
            }

    #[test]
    fn endpoint_outcomes_survive_the_viewstate_refetch_pump() {
        let _g = plx_base::testlock::serial();
        let _session = plx_plex::plex::session::TempSession::new("endpoint-viewstate");
        plx_plex::plex::reset_servers_for_test();
                let sid = plx_plex::plex::register_for_test("endpoint-viewstate", "127.0.0.1", 9, "synthetic", "cid");
        let mut stores = crate::stores::Stores::default();
        stores.viewstate.borrow_mut().owe_hubs_refresh_for_test();
        let directory = crate::stores::browse::DirectorySnapshot::default();
        let endpoints = stores.viewstate_pump(directory.view(), &crate::pms::registry_keys_for_test(), 0);
        stores.hubs.refuse_jobs_for_test(); // the runner starts nothing
        assert_eq!(endpoints.iter().count(), 0, "a refused refetch is settled by the next Hubs step");
        let settled = stores.hubs.tick_with_directory(0.0, directory.view(), &crate::pms::registry_keys_for_test());
        assert_eq!(settled.endpoints.iter().map(|r| r.sid).collect::<Vec<_>>(), [sid]);
        assert_eq!(stores.viewstate_pump(directory.view(), &crate::pms::registry_keys_for_test(), 0).iter().count(), 0,
            "refetch is consumed once");
                plx_plex::plex::reset_servers_for_test();
    }

    /// A launch the runner refused is settled by the next Hubs step, and a view edit's `ApplyView` is
    /// such a step: the refreshes it owes leave with the call that ran it, not into `let _`.
    #[test]
    fn a_refusal_settled_by_a_view_edit_step_is_carried_out_of_the_press_and_the_stop_patch() {
        let _g = plx_base::testlock::serial();
        let _session = plx_plex::plex::session::TempSession::new("endpoint-view-edit");
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("endpoint-view-edit", "127.0.0.1", 9, "synthetic", "cid");
        let keys = crate::pms::registry_keys_for_test();
        let directory = crate::stores::browse::DirectorySnapshot::default();
        let refused_refetch = |stores: &mut crate::stores::Stores| {
            let out = stores.hubs.run_with_directory(crate::stores::hubs::HubsCmd::RefetchHubs, directory.view(), &keys);
            assert!(out.endpoints.is_empty(), "the refusal is the runner's answer, not the kick's");
            stores.hubs.refuse_jobs_for_test();
        };
        let sids = |endpoints: crate::stores::EndpointRefreshSet| endpoints.iter().map(|r| r.sid).collect::<Vec<_>>();

        let mut stores = crate::stores::Stores::default();
        refused_refetch(&mut stores);
        let press = stores.viewstate_run(crate::stores::viewstate::ViewStateCmd::Request {
            sid, rk: "7".into(), write: Write::Watched, scope: Scope::Leaf, guid: String::new(),
        }, directory.view(), &keys, 0);
        assert!(press.changed);
        assert_eq!(sids(press.endpoints), [sid], "the press's ApplyView step settled the refusal");

        let mut stores = crate::stores::Stores::default();
        refused_refetch(&mut stores);
        let stop = StopRecord { sid, rk: "7".into(), pos_ms: 30_000, dur_ms: 60_000, ok: false, was_watched: false };
        assert_eq!(sids(stores.stops_landed(vec![stop], directory.view(), &keys)), [sid],
            "the stop patch's ApplyView step settled the refusal");
        plx_plex::plex::reset_servers_for_test();
    }
    use super::*;

    // Tests that touch the remaining compatibility stores hold the crate-wide serial lock. The
    // ViewState queue, latches and adapter below are ordinary local values and need no lock by
    // themselves; fan-out's pure identity cases need none at all.
    //
    // None of these calls `kick`. A host test has no registry, so a kicked write resolves no client
    // and is dropped with a log line — which is correct behaviour and useless as a subject. What is
    // worth grading is the bookkeeping around it, which is what these drive directly.

    /// The two fixture slots the identity rules are graded against. They are `ServerId`s and NOT a
    /// registry: `from_raw` is `const` exactly so a test can name a server without a table, a
    /// client or a socket behind it (`plex::servers`). Both hold a `ratingKey` 4, which is the
    /// measured reality of this household and the whole reason the fan-out matches on a guid.
    const SRV_A: ServerId = ServerId::from_raw(0);
    const SRV_B: ServerId = ServerId::from_raw(1);
    const SRV_C: ServerId = ServerId::from_raw(2);

    fn req(state: &mut ViewStateState, rk: &str, w: Write) -> Req {
        let id = state.mint_request_id();
        Req {
            id,
            seq: 0,
            sid: ServerId::UNSET, key: UNKEYED, profile: 0,
            rk: rk.into(),
            w,
            scope: Scope::Leaf,
            guid: String::new(),
            origin_done: false,
        }
    }

    /// Queue a write without sending anything — the half of [`request`] under test here.
    fn enqueue(state: &mut ViewStateState, rk: &str, w: Write) {
        state.coalesce(ServerId::UNSET, rk, w);
        let req = req(state, rk, w);
        state.queue.push(req);
    }

    /// One source's answer, as [`ask_sources`] would have built it: `Some` is an answer (empty =
    /// "I do not hold it"), `None` is a source that never replied.
    fn held(id: ServerId, keys: &[&str]) -> Answer {
        (id, Some(keys.iter().map(|k| k.to_string()).collect()))
    }

    fn queued(state: &ViewStateState) -> Vec<(String, Write)> {
        state.queue.iter().map(|r| (r.rk.clone(), r.w)).collect()
    }

    /// Two presses on one item are ONE write, and it is the latest one — a scrobble and an
    /// unscrobble that both went out would leave the server saying whichever landed last, which is
    /// not what the user asked for and is not even deterministic.
    #[test]
    fn a_second_press_on_one_item_replaces_the_queued_write_rather_than_joining_it() {
        let mut state = ViewStateState::default();

        enqueue(&mut state, "7", Write::Watched);
        enqueue(&mut state, "9", Write::Watched);
        enqueue(&mut state, "7", Write::Unwatched); // the user changed their mind about 7

        assert_eq!(
            queued(&state),
            vec![("9".into(), Write::Watched), ("7".into(), Write::Unwatched)],
            "7's earlier write is gone and its latest one is queued behind 9's, in press order"
        );
    }

    /// …but a deck removal is a DIFFERENT decision about the same item, so a watched toggle must not
    /// swallow it. Both are owed a round trip.
    #[test]
    fn a_deck_removal_and_a_watched_toggle_on_one_item_are_two_writes() {
        let mut state = ViewStateState::default();

        enqueue(&mut state, "7", Write::RemoveFromDeck);
        enqueue(&mut state, "7", Write::Watched);

        assert_eq!(
            queued(&state),
            vec![
                ("7".into(), Write::RemoveFromDeck),
                ("7".into(), Write::Watched)
            ],
            "two families, two writes, in the order pressed"
        );
    }

    /// A write already ON THE WIRE is not coalesced away: the client cannot recall it, so pretending
    /// otherwise would drop the press that supersedes it and leave the server on the older answer.
    #[test]
    fn a_write_already_in_flight_is_never_coalesced_away() {
        let mut state = ViewStateState::default();
        state.sent = Some(req(&mut state, "7", Write::Watched));

        enqueue(&mut state, "7", Write::Unwatched);

        assert!(state.sent.is_some(), "the one on the wire stays out");
        assert_eq!(
            queued(&state),
            vec![("7".into(), Write::Unwatched)],
            "and its successor is queued behind it"
        );
    }

    /// The refresh is owed ONCE, at the end of a burst - not per write. Ticking four episodes
    /// watched in a row must cost four writes and one `/hubs` pair.
    #[test]
    fn the_refresh_waits_for_the_last_write_of_a_burst() {
        let mut state = ViewStateState::default();

        state.sent = Some(req(&mut state, "1", Write::Watched));
        enqueue(&mut state, "2", Write::Watched);
        assert!(state.is_busy());

        // write 1 answers - the latch arms, but the queue is not empty, so nothing is refreshed
        state.sent.take().expect("a write was out");
        state.want_hubs = true;
        assert!(
            state.is_busy(),
            "write 2 is still queued - the refresh is not owed yet"
        );

        // ...and write 2 answers: the burst is over
        state.queue.remove(0);
        state.want_hubs = true;
        assert!(!state.is_busy());
        assert!(std::mem::take(&mut state.want_hubs), "exactly one hub refetch is owed for the burst");
    }

    // ---- the job lane ----------------------------------------------------------------------
    //
    // A write leaves the step as a `StoreJob::ViewWrite` and comes back through its port. These
    // grade that edge with no registry, socket or thread: the step is driven with a hand-built
    // `ClientKeys` and a profile, and the answer is posted through the job's own port, as the
    // runner would.

    const PROFILE: u32 = 4;

    fn key(instance: u32, token: u32) -> ClientKey { ClientKey { instance, token } }

    fn keys_of(entries: &[(ServerId, ClientKey)]) -> ClientKeys {
        ClientKeys::from_keys(entries.iter().copied())
    }

    /// Queue a write on `sid` as `request` would have: the key and the profile the press saw, and a
    /// live ledger entry (`seq`) to retire or renumber.
    fn press(state: &mut ViewStateState, sid: ServerId, k: ClientKey, rk: &str, w: Write) {
        state.coalesce(sid, rk, w);
        let mut r = req(state, rk, w);
        let change = ViewChange { sid, rk: rk.into(), scope: Scope::Leaf, fact: w.fact() };
        r.seq = state.ledger.record(&change.at(), change.edit());
        r.sid = sid;
        r.key = k;
        r.profile = PROFILE;
        state.queue.push(r);
    }

    /// The jobs the step has sent, unwrapped.
    fn sent_jobs(state: &mut ViewStateState) -> Vec<(Addr, ViewWriteReq, ViewWritePort)> {
        state.take_jobs().into_iter().map(|job| match job {
            StoreJob::ViewWrite(job, port) => (job.addr, job.req, port),
            other => panic!("viewstate emitted {other:?}"),
        }).collect()
    }

    /// One frame of the pump on the fixture gate, with the inputs a Bridge call would hand it.
    /// Returns the hub commands the frame ran.
    fn frame(state: &mut ViewStateState, adapter: &Arc<ViewStateAdapter>, keys: &ClientKeys, profile: u32)
        -> Vec<crate::stores::hubs::HubsCmd> {
        let mut hubs = Vec::new();
        let _ = state.pump_with_gate(adapter, plx_machine::landgate::fixture_gate(), keys, profile,
            &mut |_| false,
            &mut |cmd| { hubs.push(cmd); crate::stores::StoreOutcome::default() },
            &mut |_| false, &mut |_| false, &mut |_| false, &mut |_| false);
        hubs
    }

    /// The step decides a write from its inputs and emits it: one job, the key it decided, the
    /// snapshot it decided under and the id the in-flight request carries. Only one write is out at
    /// a time, so the next one waits for the answer.
    #[test]
    fn a_kicked_write_leaves_as_one_job_naming_the_key_it_decided_and_the_next_waits() {
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        let keys = keys_of(&[(SRV_A, key(1, 1)), (SRV_B, key(2, 1))]);
        press(&mut state, SRV_A, key(1, 1), "5", Write::Watched);
        press(&mut state, SRV_B, key(2, 1), "9", Write::Unwatched);

        state.kick(&adapter, &keys, PROFILE);

        let jobs = sent_jobs(&mut state);
        assert_eq!(jobs.len(), 1, "one write is out at a time");
        let (addr, job, port) = &jobs[0];
        assert_eq!(job.server(), (SRV_A, key(1, 1)));
        assert_eq!((job.rk.as_str(), job.w, job.origin_done), ("5", Write::Watched, false));
        assert_eq!(job.keys, keys, "the worker reaches the other copies under the snapshot the step used");
        assert_eq!(addr.to, MachineId::Store(StoreId::ViewState.ord()));
        assert_eq!(Some(addr.req.0), state.sent.as_ref().map(|r| r.id), "the job's id is the in-flight request's");
        assert_eq!(queued(&state), vec![("9".into(), Write::Unwatched)], "the next write waits behind it");
        assert!(!state.has_jobs(), "taking the jobs empties the outbox");

        // its answer starts the next one
        port.done(*addr, Done { ok: true, also: Vec::new() });
        let _ = frame(&mut state, &adapter, &keys, PROFILE);
        let next = sent_jobs(&mut state);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].1.server(), (SRV_B, key(2, 1)));
        assert_eq!(next[0].1.rk, "9");
    }

    /// A job the runner refused changed nothing on any server: the write goes back to the HEAD of
    /// the queue and the ladder holds the next attempt off. It is not a failure, so it does not
    /// retire its ledger entry, owe a read-back or owe a hub refresh. The real refusal needs the OS
    /// out of threads, which no host test can arrange, so the refusal is posted through the port.
    #[test]
    fn a_refused_job_keeps_the_write_at_the_head_of_the_queue_and_waits_out_the_ladder() {
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        let keys = keys_of(&[(SRV_A, key(1, 1))]);
        press(&mut state, SRV_A, key(1, 1), "5", Write::Watched);
        state.kick(&adapter, &keys, PROFILE);
        let (addr, _, port) = sent_jobs(&mut state).remove(0);
        let first_id = addr.req.0;
        let entries = state.ledger.since(0);
        assert_eq!(entries.len(), 1, "the press is recorded");

        port.refused(addr);
        let hubs = frame(&mut state, &adapter, &keys, PROFILE);

        assert_eq!(queued(&state), vec![("5".into(), Write::Watched)], "the press survives the refusal");
        assert!(state.sent.is_none(), "and nothing is recorded as in flight");
        assert!(!state.has_jobs(), "the ladder holds the retry off");
        assert_eq!(state.retry_cd, RETRY_FRAMES);
        assert_eq!(state.ledger.since(0), entries, "nothing left the device, so the press stands");
        assert!(state.take_reread().is_empty(), "and no read-back is owed");
        assert!(hubs.is_empty() && !state.want_hubs, "and no hub refresh");

        for i in 1..RETRY_FRAMES {
            let _ = frame(&mut state, &adapter, &keys, PROFILE);
            assert!(!state.has_jobs(), "frame {i} of the wait is not the due one");
        }
        let _ = frame(&mut state, &adapter, &keys, PROFILE);
        let again = sent_jobs(&mut state);
        assert_eq!(again.len(), 1, "the ladder is spent after RETRY_FRAMES frames");
        assert_eq!(again[0].0.req.0, first_id, "the same write goes out again");
        assert_eq!(again[0].1.rk, "5");
    }

    /// A refusal is judged again like any queued write. A client re-tokened meanwhile is dropped
    /// and retired even though the profile still reads as the press's: a switch swaps the tokens
    /// before it publishes the new profile, so the two cannot be told apart. As the refusal was
    /// not the system's either, no ladder is armed.
    #[test]
    fn a_refused_job_whose_client_was_retokened_while_the_profile_read_the_same_is_dropped_and_retired() {
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        press(&mut state, SRV_A, key(1, 1), "5", Write::Watched);
        state.kick(&adapter, &keys_of(&[(SRV_A, key(1, 1))]), PROFILE);
        let (addr, _, port) = sent_jobs(&mut state).remove(0);

        port.refused(addr);
        let moved = keys_of(&[(SRV_A, key(1, 2))]);
        let _ = frame(&mut state, &adapter, &moved, PROFILE);

        assert!(!state.has_jobs() && !state.is_busy(), "the write is dropped, not sent under the new key");
        assert_eq!(state.retry_cd, 0, "and no ladder is armed for it");
        assert!(state.ledger.since(0).is_empty(), "and the optimistic edit is taken back");
    }

    /// A write is one household member's statement. A profile that moved drops the write and
    /// retires its ledger entry (queued, refused and about to be sent alike) on the profile alone:
    /// the keys here are the ones the press saw, so the key compare cannot be what drops it.
    #[test]
    fn a_write_whose_profile_moved_is_dropped_and_retired_wherever_it_is_judged() {
        let keys = keys_of(&[(SRV_A, key(1, 1))]);
        let next = PROFILE + 1;

        // queued, judged at the kick
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        press(&mut state, SRV_A, key(1, 1), "5", Write::Watched);
        state.kick(&adapter, &keys, next);
        assert!(!state.has_jobs() && state.sent.is_none() && !state.is_busy(), "nothing is sent");
        assert!(state.ledger.since(0).is_empty(), "and the optimistic edit is taken back");

        // refused, judged at the landing of the refusal
        let mut state = ViewStateState::default();
        press(&mut state, SRV_A, key(1, 1), "5", Write::Watched);
        state.kick(&adapter, &keys, PROFILE);
        let (addr, _, port) = sent_jobs(&mut state).remove(0);
        port.refused(addr);
        let _ = frame(&mut state, &adapter, &keys, next);
        assert!(!state.has_jobs() && !state.is_busy(), "the refused write is not queued again");
        assert_eq!(state.retry_cd, 0, "and no ladder is armed for it");
        assert!(state.ledger.since(0).is_empty());
    }

    /// A server that left the registry while the write waited has no client to send it through: it
    /// is dropped with a line, never sent to whatever now occupies the slot.
    #[test]
    fn a_write_whose_server_left_the_registry_is_dropped_and_retired() {
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        press(&mut state, SRV_A, key(1, 1), "5", Write::Watched);
        state.kick(&adapter, &keys_of(&[(SRV_B, key(2, 1))]), PROFILE);
        assert!(!state.has_jobs() && !state.is_busy());
        assert!(state.ledger.since(0).is_empty());
    }

    /// A client re-tokened while the write waited, the profile still reading as the press's: the
    /// frames in which a profile switch has swapped the tokens and not yet published the new
    /// profile look exactly like this, so the write is dropped and retired, never sent under the
    /// key the registry holds now.
    #[test]
    fn a_queued_write_whose_client_was_retokened_while_the_profile_read_the_same_is_dropped_and_retired() {
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        press(&mut state, SRV_A, key(1, 1), "5", Write::Watched);
        state.kick(&adapter, &keys_of(&[(SRV_A, key(1, 2))]), PROFILE);
        assert!(!state.has_jobs() && state.sent.is_none() && !state.is_busy(), "nothing is sent");
        assert!(state.ledger.since(0).is_empty(), "and the optimistic edit is taken back");
    }

    /// A reset drops the queue and what is in flight, not the outbox: a job already sent answers
    /// into the adapter it was emitted under, which the owning store has retired, and the new
    /// owner never sees that answer.
    #[test]
    fn a_job_sent_before_a_reset_answers_into_the_retired_adapter_only() {
        let mut state = ViewStateState::default();
        let old = Arc::new(ViewStateAdapter::default());
        let keys = keys_of(&[(SRV_A, key(1, 1))]);
        press(&mut state, SRV_A, key(1, 1), "5", Write::Watched);
        state.kick(&old, &keys, PROFILE);
        assert!(state.has_jobs());

        state.reset();
        let new = Arc::new(ViewStateAdapter::default());
        assert!(state.has_jobs(), "the outbox is not the reset's to empty");
        let (addr, _, port) = sent_jobs(&mut state).remove(0);
        port.done(addr, Done { ok: true, also: Vec::new() });

        let hubs = frame(&mut state, &new, &keys, PROFILE);
        assert!(hubs.is_empty() && !state.want_hubs && state.take_reread().is_empty(),
            "the new owner learns nothing of it");
        assert!(new.mail.lock().unwrap_or_else(|e| e.into_inner()).is_none());
        assert!(old.mail.lock().unwrap_or_else(|e| e.into_inner()).is_some(), "it went to the adapter it was made under");
    }

    /// A worker that panics answers with a failed write, NEVER with "not started": the origin
    /// write may already be on the server, and a second scrobble adds a second viewCount. The
    /// registry read is the panic here (the host passes it in), reached by the fan-out.
    #[test]
    fn a_worker_that_panics_answers_a_failed_write_and_never_not_started() {
        fn panicking_lookup(_: ServerId) -> Option<&'static Client> { panic!("registry read panicked") }
        let _g = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let a = plx_plex::plex::register_for_test("viewstate-panic-a", "127.0.0.1", 9, "token", "panic-a");
        let b = plx_plex::plex::register_for_test("viewstate-panic-b", "127.0.0.1", 9, "token", "panic-b");
        let keys = crate::pms::registry_keys_for_test();
        let origin = plx_plex::plex::client_for(a).expect("registered");
        let adapter = Arc::new(ViewStateAdapter::default());
        let job = ViewWriteReq {
            sid: a, key: keys.key(a).expect("keyed"), rk: "1".into(), w: Write::Watched,
            guid: "plex://movie/6856893830a4aaafd5c4291d".into(), origin_done: true, keys: keys.clone(),
        };
        assert!(keys.key(b).is_some());

        finish_view_write_job(&ViewWritePort::of(&adapter), view_write_addr(77), origin, panicking_lookup, job);

        let answer = adapter.mail.lock().unwrap_or_else(|e| e.into_inner()).take().expect("the job is answered");
        assert_eq!(answer.id, 77);
        assert!(matches!(&answer.outcome, Outcome::Written(done) if !done.ok && done.also.is_empty()),
            "a failed write with nothing fanned out, not a refusal");
        plx_plex::plex::reset_servers_for_test();
    }

    /// DUMP MODE at the ViewState site, through the real pump: a write on the wire when the pump
    /// runs lands on THAT pump however late its worker posts (25 ms here). A plain take (the
    /// conversion reverted) finds an empty mailbox, and the write is still `sent` afterwards.
    /// Serial like every test that arms a gate.
    #[test]
    fn dump_mode_a_write_on_the_wire_lands_on_the_pump_that_runs_whatever_the_worker() {
        let _serial = plx_base::testlock::serial();
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        let sent = req(&mut state, "7", Write::Watched);
        let id = sent.id;
        state.sent = Some(sent);
        let gate = plx_machine::landgate::Gate::default();
        gate.arm_dump(std::time::Duration::from_secs(20));
        let worker = Arc::clone(&adapter);
        let join = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(25));
            *worker.mail.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Completion { id, outcome: Outcome::Written(Done { ok: true, also: Vec::new() }) });
        });
        let mut hubs = Vec::new();
        let _ = state.pump_with_gate(&adapter, &gate, &ClientKeys::default(), 0, &mut |_| false,
            &mut |cmd| { hubs.push(cmd); crate::stores::StoreOutcome::default() },
            &mut |_| false, &mut |_| false, &mut |_| false, &mut |_| false);
        assert!(state.sent.is_none(), "the answer the pump owed must be taken by the pump that asked");
        assert!(matches!(hubs.first(), Some(crate::stores::hubs::HubsCmd::RefetchHubs)),
            "and the refresh it owes follows on the same pump");
        join.join().unwrap();
    }

    /// A completion that answers no in-flight request (a stale one) is dropped INSIDE the take: it
    /// neither ends the dump wait nor stands in for the write still on the wire.
    #[test]
    fn dump_mode_a_stale_completion_does_not_end_the_wait_for_the_write_on_the_wire() {
        let _serial = plx_base::testlock::serial();
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        let stale = req(&mut state, "6", Write::Watched).id;
        let sent = req(&mut state, "7", Write::Watched);
        let id = sent.id;
        state.sent = Some(sent);
        assert_ne!(stale, id);
        let gate = plx_machine::landgate::Gate::default();
        gate.arm_dump(std::time::Duration::from_secs(20));
        let worker = Arc::clone(&adapter);
        let join = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5));
            *worker.mail.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Completion { id: stale, outcome: Outcome::Written(Done::default()) });
            std::thread::sleep(std::time::Duration::from_millis(25));
            *worker.mail.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Completion { id, outcome: Outcome::Written(Done { ok: true, also: Vec::new() }) });
        });
        let _ = state.pump_with_gate(&adapter, &gate, &ClientKeys::default(), 0, &mut |_| false,
            &mut |_| crate::stores::StoreOutcome::default(),
            &mut |_| false, &mut |_| false, &mut |_| false, &mut |_| false);
        assert!(state.sent.is_none(), "the write on the wire was answered by ITS completion");
        join.join().unwrap();
    }

    /// The fan-out request an end of stream queues writes nothing on the origin, and when it reached
    /// no other copy it changed nothing the origin's own read-back (which asked for it) has not
    /// already brought: it must not owe the hub refetch a second time.
    fn land_origin_done(also: Vec<(ServerId, String)>) -> Vec<crate::stores::hubs::HubsCmd> {
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        let id = state.mint_request_id();
        state.sent = Some(Req { id, seq: 0, sid: ServerId::from_raw(0), key: UNKEYED, profile: 0, rk: "7".into(), w: Write::Watched,
            scope: Scope::Leaf, guid: String::new(), origin_done: true });
        *adapter.mail.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Completion { id, outcome: Outcome::Written(Done { ok: true, also }) });
        let mut hubs = Vec::new();
        let _ = state.pump(&adapter, &mut |_| true,
            &mut |cmd| { hubs.push(cmd); crate::stores::StoreOutcome::default() },
            &mut |_| false, &mut |_| false, &mut |_| false, &mut |_| false);
        hubs
    }

    #[test]
    fn an_origin_done_fanout_that_reached_no_copy_owes_no_hub_refetch() {
        let _g = plx_base::testlock::serial();
        let hubs = land_origin_done(Vec::new());
        assert!(!hubs.iter().any(|c| matches!(c, crate::stores::hubs::HubsCmd::RefetchHubs)),
            "nothing changed past what the origin's own read-back brings");
    }

    #[test]
    fn an_origin_done_fanout_that_reached_a_copy_owes_one_hub_refetch() {
        let _g = plx_base::testlock::serial();
        let hubs = land_origin_done(vec![(ServerId::from_raw(1), "70".into())]);
        assert_eq!(hubs.iter().filter(|c| matches!(c, crate::stores::hubs::HubsCmd::RefetchHubs)).count(), 1);
    }

    #[test]
    fn a_one_server_install_queues_no_origin_done_fanout() {
        let _g = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("viewstate-one-server", "127.0.0.1", 9, "synthetic", "fixture");
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        assert!(!state.fan_out_origin_watched(&adapter, sid, "7", &crate::pms::registry_keys_for_test(), 0));
        assert!(!state.is_busy(), "no request for a fan-out that has nowhere to go");
        plx_plex::plex::reset_servers_for_test();
    }

    /// A stop at or past the server's watched threshold has no resume point: PMS clears `viewOffset`
    /// there, so the immediate patch must read 0 and not the duration.
    #[test]
    fn a_stop_past_the_watched_threshold_patches_a_zero_resume_point() {
        let at = |ms, dur_ms| ViewChange { sid: ServerId::from_raw(0), rk: "7".into(), scope: Scope::Leaf,
            fact: ViewFact::Offset { ms, dur_ms } }.edit();
        assert_eq!(at(60_000, 100_000), ViewEdit::Offset { ms: 60_000 });
        assert_eq!(at(89_999, 100_000), ViewEdit::Offset { ms: 89_999 });
        assert_eq!(at(90_000, 100_000), ViewEdit::Offset { ms: 0 });
        assert_eq!(at(100_000, 100_000), ViewEdit::Offset { ms: 0 });
        assert_eq!(at(120_000, 100_000), ViewEdit::Offset { ms: 0 });
        assert_eq!(at(5_000, 0), ViewEdit::Offset { ms: 5_000 }, "an unknown duration clamps nothing");
    }

    /// `reset` is what an identity change leans on: nothing of the previous account's may be left
    /// queued, in flight, or owed a refresh.
    #[test]
    fn reset_leaves_nothing_queued_in_flight_or_owed() {
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        enqueue(&mut state, "1", Write::Watched);
        let sent = req(&mut state, "2", Write::Unwatched);
        let sent_id = sent.id;
        state.sent = Some(sent);
        state.want_hubs = true;
        state.retry_cd = 9;
        *adapter.mail.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Completion { id: sent_id, outcome: Outcome::Written(Done::default()) });

        state.reset();

        assert!(!state.is_busy());
        assert!(!state.want_hubs);
        assert_eq!(state.retry_cd, 0);
        assert!(adapter.mail.lock().unwrap_or_else(|e| e.into_inner()).is_some(),
            "state reset does not mutate an adapter; the owning store retires it wholesale");
    }

    // ---- the fan-out ---------------------------------------------------------------------
    //
    // The first FIVE grade PURE functions and take no lock: `fanout_targets` and `propagates` touch
    // no static, no registry and no socket, which is the whole reason the identity rule was factored
    // out of the worker. The last two move crate globals and hold `testlock::serial()`.

    /// **The claim this feature makes.** A title held by three sources ends up written on all of
    /// them: the press wrote its own copy, and the fan-out writes the rest — matched on the guid,
    /// so `4` on the share is understood to be a DIFFERENT item from `4` on ours and is written
    /// under its own key.
    #[test]
    fn a_watched_write_fans_out_to_every_other_sources_copy_of_the_title() {
        let answers = [
            held(SRV_A, &["4"]),
            held(SRV_B, &["4"]),
            held(SRV_C, &["5274"]),
        ];
        assert_eq!(
            fanout_targets((SRV_A, "4"), &answers),
            vec![(SRV_B, "4".into()), (SRV_C, "5274".into())],
            "both other copies, each under its own server's key"
        );
    }

    /// The copy the press ALREADY wrote is never written a second time — and it is excluded as the
    /// PAIR, not as a bare key: the share's `4` looks identical and is a different film.
    #[test]
    fn the_source_the_press_was_made_on_is_not_written_twice() {
        let answers = [held(SRV_A, &["4"]), held(SRV_B, &["4"])];
        let from_a = fanout_targets((SRV_A, "4"), &answers);
        let from_b = fanout_targets((SRV_B, "4"), &answers);
        assert_eq!(
            from_a,
            vec![(SRV_B, "4".into())],
            "pressed on A, so only B is written"
        );
        assert_eq!(
            from_b,
            vec![(SRV_A, "4".into())],
            "…and pressed on B, only A"
        );
    }

    /// The ordinary case, and it must cost nothing: only one source holds the title, so there is
    /// nothing to propagate to. A source that did not ANSWER is also nothing to write — it is not
    /// evidence either way, and guessing a key for it is how the wrong film gets marked.
    #[test]
    fn a_title_only_one_source_holds_produces_no_fan_out_at_all() {
        let answers = [held(SRV_A, &["4"]), held(SRV_B, &[]), (SRV_C, None)];
        assert!(
            fanout_targets((SRV_A, "4"), &answers).is_empty(),
            "B does not have it and C never said; neither is a copy to write"
        );
    }

    /// A source listing the same key twice is written once; a source listing a SECOND key for the
    /// guid is a second item holding that title, and the claim being propagated is about the title,
    /// so it is written too — the item's own server included.
    #[test]
    fn a_duplicate_key_is_collapsed_but_a_second_copy_is_still_a_copy() {
        let answers = [held(SRV_A, &["4", "88"]), held(SRV_B, &["4", "4"])];
        assert_eq!(
            fanout_targets((SRV_A, "4"), &answers),
            vec![(SRV_A, "88".into()), (SRV_B, "4".into())],
            "our own second copy of the film is written; B's repeated key is written once"
        );
    }

    /// **Remove from Continue Watching does NOT follow the title.** The deck is a per-server
    /// surface: taking an item off yours says nothing about anyone else's, and a fan-out here would
    /// reach into a friend's server to hide a row from a shelf that is not the one you pressed.
    #[test]
    fn a_deck_removal_stays_on_the_server_it_was_pressed_on() {
        assert!(
            Write::Watched.propagates(),
            "watched is a claim about the title"
        );
        assert!(
            Write::Unwatched.propagates(),
            "…and so is taking that claim back"
        );
        assert!(
            !Write::RemoveFromDeck.propagates(),
            "the deck is one server's own surface"
        );
    }

    /// The EDIT the landing applies to a fanned-out copy: the same local flip the press made for
    /// the copy in hand, applied to a `(server, ratingKey)` that could not be known any earlier —
    /// and applied to that PAIR, so the identical key on our own server is left alone. Graded
    /// through the detail store, which is the surface that shows it. (That [`pump`] really calls
    /// this, for every copy the worker reported, is the next test — this one is about the edit.)
    #[test]
    fn the_landing_flips_another_sources_copy_the_press_could_not_reach() {
        let _g = plx_base::testlock::serial();
        let mut metadata_store = crate::stores::metadata::MetadataStore::default();
        // the page is mounted on the SHARE's copy, which the press on our own copy never touched
        crate::metadata::install_for_test(metadata_store.state_mut(), Some(crate::metadata::Detail {
            sid: SRV_B,
            rk: "4".into(),
            resume_ms: 900_000,
            ..Default::default()
        }));

        let _ = publish_view(ViewChange { sid: SRV_A, rk: "4".into(), scope: Scope::Leaf, fact: ViewFact::Watched(true) },
            &mut |_| false,
            &mut |_cmd| crate::stores::StoreOutcome::default(), &mut |_| false, &mut |_| false, &mut |_| false,
            &mut |cmd| metadata_store.run(cmd));
        assert!(
            !metadata_store.view().current().unwrap().watched,
            "A's 4 is not B's 4"
        );

        let _ = publish_view(ViewChange { sid: SRV_B, rk: "4".into(), scope: Scope::Leaf, fact: ViewFact::Watched(true) },
            &mut |_| false,
            &mut |_cmd| crate::stores::StoreOutcome::default(), &mut |_| false, &mut |_| false, &mut |_| false,
            &mut |cmd| metadata_store.run(cmd));
        assert!(metadata_store.view().current().unwrap().watched);
        assert_eq!(
            metadata_store.view().current().unwrap().resume_ms,
            0,
            "watched stops offering to resume"
        );

        crate::metadata::install_for_test(metadata_store.state_mut(), None);
    }

    /// **Which stores a change reaches.** The server's word (`Facts`) and a stop's position
    /// (`Offset`) reach the person, collection, search and metadata owners, each with the edit the
    /// change names; leaving Continue Watching reaches none of the four (a result tile, a filmography
    /// row and a Detail tile have no deck).
    #[test]
    fn publish_view_hands_facts_and_offset_to_the_four_tile_stores_and_a_deck_removal_to_none() {
        use crate::stores::{collection::CollectionCmd, metadata::MetadataCmd, person::PersonCmd, search::SearchCmd};
        let reached = |fact| {
            let (mut p, mut c, mut s, mut m) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let _ = publish_view(ViewChange { sid: SRV_A, rk: "4".into(), scope: Scope::Leaf, fact },
                &mut |_| false,
                &mut |_cmd| crate::stores::StoreOutcome::default(),
                &mut |cmd| { if let PersonCmd::ApplyView { edit, .. } = cmd { p.push(edit) } false },
                &mut |cmd| { if let CollectionCmd::ApplyView { edit, .. } = cmd { c.push(edit) } false },
                &mut |cmd| { if let SearchCmd::ApplyView { edit, .. } = cmd { s.push(edit) } false },
                &mut |cmd| { if let MetadataCmd::ApplyView { edit, .. } = cmd { m.push(edit) } false });
            (p, c, s, m)
        };
        let facts = ViewFact::Facts { watched: true, unwatched: false, resume_ms: 0 };
        let edit = ViewEdit::Facts { watched: true, unwatched: false, resume_ms: 0 };
        assert_eq!(reached(facts), (vec![edit], vec![edit], vec![edit], vec![edit]), "Facts reach all four");
        let offset = ViewFact::Offset { ms: 42, dur_ms: 1_000_000 };
        let edit = ViewEdit::Offset { ms: 42 };
        assert_eq!(reached(offset), (vec![edit], vec![edit], vec![edit], vec![edit]), "Offset reaches all four");
        assert_eq!(
            reached(ViewFact::LeftDeck),
            (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
            "a deck removal reaches none of them"
        );
    }

    /// **The landing itself, driven through [`ViewStateState::pump`].** The test above grades what
    /// `publish_view` does; this one grades that the pump actually CALLS it — for every copy the worker reported,
    /// with the write that was SENT rather than whatever was pressed since — and then clears the
    /// in-flight slot. Without this the fan-out could report copies nobody ever applied, and every
    /// pure test above would still be green.
    #[test]
    fn the_landing_applies_the_workers_whole_report_and_retires_the_write() {
        let _g = plx_base::testlock::serial();
        let mut metadata_store = crate::stores::metadata::MetadataStore::default();
        // the mounted page is the SHARE's copy — the one the press could not reach
        crate::metadata::install_for_test(metadata_store.state_mut(), Some(crate::metadata::Detail {
            sid: SRV_B,
            rk: "4".into(),
            ..Default::default()
        }));

        // a write that went out on OUR copy, and the worker's answer naming the share's
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        let id = state.mint_request_id();
        state.sent = Some(Req {
            id,
            seq: 0,
            sid: SRV_A, key: UNKEYED, profile: 0,
            rk: "4".into(),
            w: Write::Watched,
            scope: Scope::Leaf,
            guid: "plex://movie/6856893830a4aaafd5c4291d".into(),
            origin_done: false,
        });
        *adapter.mail.lock().unwrap_or_else(|e| e.into_inner()) = Some(Completion {
            id,
            outcome: Outcome::Written(Done { ok: true, also: vec![(SRV_B, "4".into())] }),
        });

        let _outcome = state.pump(&adapter, &mut |_| false,
            &mut |_cmd| crate::stores::StoreOutcome::default(), &mut |_| false, &mut |_| false, &mut |_| false,
            &mut |cmd| metadata_store.run(cmd));

        assert!(
            metadata_store.view().current().unwrap().watched,
            "the page mounted on the share's copy is flipped by the landing, not by the press"
        );
        assert!(
            !state.is_busy(),
            "…and the write that reported is no longer in flight"
        );
        assert!(
            adapter.mail.lock().unwrap_or_else(|e| e.into_inner()).is_none(),
            "the mailbox is drained"
        );

        crate::metadata::install_for_test(metadata_store.state_mut(), None);
    }

    /// Land one write of `ok` through the pump, with its ledger entry numbered at the press, and
    /// hand back the ledger plus a stamp taken BETWEEN the press and the landing (a read requested
    /// after the press that the server answered before the write arrived).
    fn press_then_land(ok: bool) -> (crate::viewledger::ViewLedger, crate::viewledger::Stamp) {
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        let at = ViewAt::leaf(SRV_A, "4");
        let seq = state.ledger.record(&at, ViewEdit::Watched(true));
        let read_stamp = state.ledger.stamp();
        let id = state.mint_request_id();
        state.sent = Some(Req {
            id, seq, sid: SRV_A, key: UNKEYED, profile: 0, rk: "4".into(), w: Write::Watched, scope: Scope::Leaf,
            guid: String::new(),
            origin_done: false,
        });
        *adapter.mail.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Completion { id, outcome: Outcome::Written(Done { ok, also: Vec::new() }) });
        let _ = state.pump(&adapter, &mut |_| false, &mut |_cmd| crate::stores::StoreOutcome::default(),
            &mut |_| false, &mut |_| false, &mut |_| false, &mut |_| false);
        (state.ledger.clone(), read_stamp)
    }

    /// A press, then a read REQUESTED after it that the server answered before the write landed:
    /// it describes the server from before the press, and must not undo it.
    #[test]
    fn a_taken_write_is_renumbered_so_a_read_requested_before_its_landing_still_replays_the_press() {
        let (ledger, read_stamp) = press_then_land(true);
        let mut row = crate::pms::PmsMovie::default();
        row.sid = SRV_A;
        row.rk = "4".into();
        row.unwatched = true;
        ledger.patch(read_stamp, std::iter::once(&mut row));
        assert!(row.watched, "the press survives a read that was answered before its write landed");
        let after = ledger.stamp();
        let mut later = crate::pms::PmsMovie::default();
        later.sid = SRV_A;
        later.rk = "4".into();
        later.unwatched = true;
        ledger.patch(after, std::iter::once(&mut later));
        assert!(later.unwatched, "a read requested after the landing is the server's own word");
    }

    /// The server refused the write: later landings show the server's state, not the refused edit.
    #[test]
    fn a_refused_write_is_retired_so_later_landings_show_the_servers_state() {
        let (ledger, read_stamp) = press_then_land(false);
        assert!(ledger.since(0).is_empty(), "a refused edit is not kept");
        let mut row = crate::pms::PmsMovie::default();
        row.sid = SRV_A;
        row.rk = "4".into();
        row.unwatched = true;
        ledger.patch(read_stamp, std::iter::once(&mut row));
        assert!(row.unwatched && !row.watched, "the server's state stands");
    }

    /// A newer press replaced the entry while the older write was in flight: the older landing must
    /// not renumber the newer press's entry (its own write has not landed).
    #[test]
    fn an_older_landing_leaves_the_entry_of_a_newer_press_alone() {
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        let at = ViewAt::leaf(SRV_A, "4");
        let first = state.ledger.record(&at, ViewEdit::Watched(true));
        state.ledger.record(&at, ViewEdit::Watched(false));
        let stamp = state.ledger.stamp();
        let id = state.mint_request_id();
        state.sent = Some(Req {
            id, seq: first, sid: SRV_A, key: UNKEYED, profile: 0, rk: "4".into(), w: Write::Watched, scope: Scope::Leaf,
            guid: String::new(),
            origin_done: false,
        });
        *adapter.mail.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Completion { id, outcome: Outcome::Written(Done { ok: true, also: Vec::new() }) });
        let _ = state.pump(&adapter, &mut |_| false, &mut |_cmd| crate::stores::StoreOutcome::default(),
            &mut |_| false, &mut |_| false, &mut |_| false, &mut |_| false);
        assert!(state.ledger.since(stamp).is_empty());
    }

    /// Sign-in, profile and server switches reset the ViewState owner; the edits go with them.
    #[test]
    fn a_reset_clears_the_ledger() {
        let mut state = ViewStateState::default();
        state.ledger.record(&ViewAt::leaf(SRV_A, "4"), ViewEdit::Watched(true));
        let stamp = state.ledger.stamp();
        state.reset();
        assert!(state.ledger.since(0).is_empty());
        assert!(state.ledger.stamp() > stamp);
    }

    #[test]
    fn only_the_exact_monotone_request_identity_can_retire_a_flight() {
        let _guard = plx_base::testlock::serial();
        let mut state = ViewStateState::default();
        let adapter = Arc::new(ViewStateAdapter::default());
        let first = req(&mut state, "before-reset", Write::Watched);
        let first_id = first.id;
        state.sent = Some(first);
        state.reset();
        let second = req(&mut state, "after-reset", Write::Unwatched);
        let second_id = second.id;
        assert!(second_id > first_id, "request identity stays monotone across reset");
        state.sent = Some(second);
        *adapter.mail.lock().unwrap_or_else(|e| e.into_inner()) = Some(Completion {
            id: first_id,
            outcome: Outcome::Written(Done::default()),
        });

        let _ = state.pump(&adapter, &mut |_| false,
            &mut |_| crate::stores::StoreOutcome::default(), &mut |_| false, &mut |_| false, &mut |_| false,
            &mut |_| false);

        assert_eq!(state.sent.as_ref().map(|request| request.id), Some(second_id),
            "a stale or mismatched completion cannot satisfy the current in-flight identity");
        assert!(!state.want_hubs);
    }

    /// `ViewAt::covers_parts` as a table: what each scope reaches among rows of the same and other
    /// servers, parents, seasons, leaves and rows whose season is unknown.
    #[test]
    fn covers_parts_table() {
        use crate::pms::SEASON_UNKNOWN;
        let at = |scope| ViewAt { sid: SRV_A, rk: "pressed".into(), scope };
        let show = Scope::Show;
        let s1 = Scope::Season { show_rk: "show".into(), index: 1 };
        let s0 = Scope::Season { show_rk: "show".into(), index: 0 };
        let i = |n: i32| i64::from(n);
        // (case, scope, sid, rk, kind, show_rk, season_index, covered)
        let table: &[(&str, Scope, ServerId, &str, i32, &str, i64, bool)] = &[
            ("the item itself, any scope", Scope::Leaf, SRV_A, "pressed", 3, "", 0, true),
            ("same rk on another server is another item", Scope::Leaf, SRV_B, "pressed", 3, "", 0, false),
            ("a show reaches its episode", show.clone(), SRV_A, "e", 3, "pressed", 1, true),
            ("a show reaches its season row", show.clone(), SRV_A, "s", 2, "pressed", 1, true),
            ("same show_rk on a different sid", show.clone(), SRV_B, "e", 3, "pressed", 1, false),
            ("empty show_rk under Show", show.clone(), SRV_A, "e", 3, "", 1, false),
            ("another show's episode", show.clone(), SRV_A, "e", 3, "other", 1, false),
            ("a movie row with a stale show_rk", show.clone(), SRV_A, "m", 0, "pressed", 0, false),
            ("a season reaches its episodes", s1.clone(), SRV_A, "e", 3, "show", 1, true),
            ("a season misses another season", s1.clone(), SRV_A, "e", 3, "show", 2, false),
            ("a season row under Season", s1.clone(), SRV_A, "s", 2, "show", 1, false),
            ("Season on a different sid", s1.clone(), SRV_B, "e", 3, "show", 1, false),
            ("Season with empty show_rk", s1.clone(), SRV_A, "e", 3, "", 1, false),
            ("Specials are season 0", s0.clone(), SRV_A, "e", 3, "show", 0, true),
            ("index 0 does not reach an unknown season", s0, SRV_A, "e", 3, "show", i(SEASON_UNKNOWN), false),
            ("Unknown acts as a leaf: not descendants", Scope::Unknown, SRV_A, "e", 3, "pressed", 1, false),
            ("Unknown acts as a leaf: itself", Scope::Unknown, SRV_A, "pressed", 3, "", 0, true),
            ("Leaf reaches no descendant", Scope::Leaf, SRV_A, "e", 3, "pressed", 1, false),
        ];
        for (case, scope, sid, rk, kind, show_rk, idx, want) in table {
            assert_eq!(at(scope.clone()).covers_parts(*sid, rk, *kind, show_rk, *idx), *want, "{case}");
        }
    }
}
