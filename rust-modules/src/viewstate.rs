//! viewstate — the server-side **view-state writes**, off the SDL thread.
//!
//! Three actions share one shape: Mark as Watched (`/:/scrobble`), Mark as Unwatched
//! (`/:/unscrobble`) and Remove from Continue Watching (`PUT /actions/removeFromContinueWatching`).
//! Each is one request to the item's own server, followed by a refresh of every surface that
//! describes that item — Home's shelves always, plus the detail page when the press came from it.
//!
//! ## Why this module exists
//!
//! All of it used to run INLINE on the frame loop, straight off the key handler: the scrobble, then
//! `detail::refresh_view_state` (a blocking 2-5 round-trip re-read), then
//! `pms::refetch_hubs_reconcile` (a blocking `/hubs` + `/hubs/continueWatching` pair for the owned
//! server). The justifying comment priced it at "~100 ms LAN and deliberately so", which was true of
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
//!   about the item BEFORE anything leaves the machine ([`crate::pms::edit_item`],
//!   [`crate::metadata::set_watched_local`]), so the tick, the veil and the resume bar all change on
//!   the frame of the press however far away the server is. The refresh that follows the write is
//!   the reconcile: it is the server's own answer, and it silently corrects an optimistic edit that
//!   turned out to be wrong. A server that never answers leaves the edit standing — the same "a
//!   failure never blanks a populated Home" rule `pms.rs` keeps for a fetch, applied to a write.
//! * **One write at a time, in order.** These are not idempotent reads: a watched/unwatched pair
//!   landing out of order leaves the server saying the opposite of what the user last asked for. So
//!   the queue is drained strictly one worker at a time, and a second press on the SAME item
//!   replaces the queued one rather than joining it ([`coalesce`]) — the newest intent for an item
//!   is the only one worth a round trip.
//! * **One refresh per burst.** The hub refetch (and the detail re-read) is deferred until the queue
//!   empties, so ticking four episodes watched in a row costs four writes and one refresh.
//! * **A refused spawn is a retry, never a panic and never a fallback to blocking.**
//!   `task::spawn_small` returning false means the OS is out of threads; the queue keeps its head
//!   and [`pump`] tries again after [`RETRY_FRAMES`], the same degradation `search.rs` and
//!   `browse.rs` apply to a refused fetch.
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
//!   [`crate::plex::Client::find_by_guid`] is the lookup, the same one "Also available" resolves
//!   its rows with.
//! * **No resume position is ever COPIED from one server to another — only the watched flag
//!   travels.** This is the subtle half. A resume offset is about a file you are streaming from one
//!   host, and `ui::alt_sources`' module doc reasons that out at length; that reasoning stands. So
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
//! [`request`]'s optimistic edit can only reach the `(sid, rk)` the user pressed, because no other
//! key is KNOWN yet — the resolve is what discovers them, and it is off-thread by construction.
//! The copies are flipped locally when the fan-out reports ([`pump`] applies [`edit_local`] to each
//! one the server took), and the hub refetch that already follows every write reconciles whatever
//! that missed. So the copy in hand changes at once and the others change a round trip later,
//! which is the same contract the press already has with its own server.
//!
//! Every static here is main-thread-only except [`MAIL`], which is the worker's single output —
//! the discipline `pms.rs` and `metadata.rs` state for their own mailboxes.

use crate::plex::ServerId;
use std::panic::catch_unwind;
use std::ptr::{addr_of, addr_of_mut};
use std::sync::Mutex;

/// What to ask the item's server for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Write {
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

    /// Perform it. WORKER THREAD — `c` was resolved on the main thread at the spawn site.
    fn perform(self, c: &crate::plex::Client, rk: &str) -> bool {
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

/// One queued write. Identity only — the `&'static Client` is resolved in [`kick`], on the main
/// thread, at the moment the worker is spawned. `pms::kick`'s rule ("capture at the spawn site")
/// says the worker must never ask which server is current; resolving a SLOT there is that rule, and
/// it also means a share whose registration went away while this sat in the queue is turned away
/// with a log line instead of being sent to whatever now occupies the slot.
struct Req {
    sid: ServerId,
    rk: String,
    w: Write,
    /// Re-read the mounted detail page when this lands, landing the filmstrip back on this episode.
    /// `Some("")` = the hero's own toggle (no filmstrip position to protect); `None` = the press
    /// came from Home, which has no detail page to refresh.
    detail: Option<String>,
    /// The item's PORTABLE identity (`plex://movie/…`) **as it is on `sid` for `rk`**, or empty when
    /// the press had none in hand — [`fan_out`] then resolves it from the pair, on the worker.
    ///
    /// It must be the guid OF `rk` and of nothing else. The detail page's hero toggle knows it
    /// (`metadata::Detail::guid`, the same pair the page is mounted on), while the card context
    /// menu's rk can be an EPISODE of the show whose guid that page happens to be holding — which
    /// is why `app.rs` passes nothing at all rather than something close: a wrong guid does not
    /// fail, it marks a different title watched on every other source.
    guid: String,
}

/// Writes waiting for a worker. Main-thread only; drained one at a time by [`kick`].
static mut QUEUE: Vec<Req> = Vec::new();
/// The write currently out, if any. Main-thread only — it is what [`pump`] turns into a refresh.
static mut SENT: Option<Req> = None;
/// Frames left before re-attempting a spawn the OS refused. Main-thread only.
static mut RETRY_CD: u32 = 0;
/// A hub refetch is owed once the queue empties. Main-thread only.
static mut WANT_HUBS: bool = false;
/// A detail re-read is owed once the queue empties, carrying the episode the filmstrip must land
/// back on (empty = none). Main-thread only.
static mut WANT_DETAIL: Option<String> = None;

/// The worker's ONLY output: whether the item's own server took the write it was given, plus the
/// OTHER sources' copies of the same title the fan-out also wrote — `(server, ratingKey)` each, and
/// only the ones that were TAKEN, since by the time this is filled the answer is known and there is
/// no reason to flip a copy the server refused. Empty for a write that does not propagate.
#[derive(Default)]
struct Done {
    ok: bool,
    also: Vec<(ServerId, String)>,
}

/// There is at most one worker at a time, and the main thread clears the slot as it takes it.
static MAIL: Mutex<Option<Done>> = Mutex::new(None);

/// ~2 s at 60 fps — the same wait `search.rs` gives a refused spawn. Without it, a process at the
/// thread ceiling would re-attempt (and log a refusal) every single frame.
const RETRY_FRAMES: u32 = 120;

fn queue() -> &'static mut Vec<Req> {
    unsafe { &mut *addr_of_mut!(QUEUE) }
}

/// Is a write queued or in flight? What gates the deferred refresh, and the predicate any future
/// "saving…" affordance would read.
pub(crate) fn is_busy() -> bool {
    unsafe { !(*addr_of!(QUEUE)).is_empty() || (*addr_of!(SENT)).is_some() }
}

/// Ask the item's server to change `(sid, rk)`'s view state, and update every surface that describes
/// it AT ONCE. **MAIN THREAD, NON-BLOCKING.** Returns false when the write never left — the one case
/// being a server slot that holds no client, which is a dropped PMS write and says so in the log
/// rather than being the one that goes unrecorded.
///
/// `detail` says what to re-read when the write lands: `None` for a press on a Home shelf,
/// `Some(episode_rk)` for the detail page's filmstrip menu, `Some("")` for the detail hero's own
/// toggle. The hub refetch happens either way — the Continue Watching shelf changes with any of
/// these three writes, whichever screen asked for it.
///
/// `guid` is the item's portable identity **if the caller already holds the one belonging to `rk`**,
/// and `""` otherwise — see [`Req::guid`], and note that "" is a perfectly ordinary answer rather
/// than a degraded one: the worker looks it up. It is what a watched write FANS OUT on, so that a
/// title held by more than one source ends up watched on all of them (module doc). It is ignored
/// for [`Write::RemoveFromDeck`], which stays on the server it was pressed on.
pub(crate) fn request(sid: ServerId, rk: &str, w: Write, detail: Option<String>, guid: &str) -> bool {
    // `client_for`, never `client()`: the item may live on a share, and a scrobble sent to the wrong
    // machine marks a DIFFERENT film watched there (both servers number their items from 1). None is
    // a slot that is not registered, where `client()` panics — a view-state write is exactly the
    // operation to skip and log rather than take to the wrong machine.
    if crate::plex::client_for(sid).is_none() {
        crate::log(&format!(
            "viewstate: rk={rk} {} DROPPED — server {} is not registered",
            w.name(),
            sid.raw()
        ));
        return false;
    }
    // OPTIMISTIC, before the request: the press must land on the panel now, not one WAN round trip
    // from now. Only THIS copy — the other sources' keys are not known until the fan-out resolves
    // them, which is what [`pump`] finishes the job with.
    edit_local(sid, rk, w);
    coalesce(sid, rk, w);
    queue().push(Req { sid, rk: rk.to_string(), w, detail, guid: guid.to_string() });
    kick();
    true
}

/// Flip every LOCAL surface that describes `(sid, rk)` to what `w` is about to make true, and ask
/// for a repaint. Both edits are no-ops where the item does not appear, so a Home press with no
/// detail page mounted costs one catalog walk and nothing else — and those two ARE every surface
/// that holds an editable copy: the Library grid, Search and the person page re-read rather than
/// cache a watch flag.
///
/// Called from two places, for one reason. [`request`] calls it optimistically for the copy the
/// user pressed, and [`pump`] calls it for each OTHER source's copy the fan-out reached — which
/// cannot be known any earlier than that, because discovering them is the round trip.
fn edit_local(sid: ServerId, rk: &str, w: Write) {
    match w {
        Write::Watched | Write::Unwatched => {
            let on = w == Write::Watched;
            // **Every store that can be DRAWING the item, not just the hub catalog.** The first two
            // lines were the whole edit while the context menu opened on Home and the detail page
            // alone; it opens on the Library grid, a Search result and a person's filmography now,
            // and each of those keeps its own rows (`pms::edit_item`'s own doc names the gap — "the
            // item is on no shelf (a Library-grid or Related item): nothing to redraw"). Left at
            // two, the press wrote correctly to the server and changed nothing on the screen the
            // user was looking at until a refetch, which reads as the row having done nothing.
            //
            // All five are no-ops where the item does not appear — a walk of an empty or unrelated
            // store — so a press from any one screen still costs about what it did.
            crate::pms::edit_item(sid, rk, crate::pms::LocalEdit::Watched(on));
            crate::metadata::set_watched_local(sid, rk, on);
            crate::browse::set_watched_local(sid, rk, on);
            crate::search::set_watched_local(sid, rk, on);
            crate::person::set_watched_local(sid, rk, on);
        }
        // The one edit that can be stated with certainty: the server hides the item from the deck
        // and keeps everything else about it (`plex::Client::remove_from_continue_watching`).
        Write::RemoveFromDeck => {
            crate::pms::edit_item(sid, rk, crate::pms::LocalEdit::LeftTheDeck);
        }
    }
    crate::ui::idle::invalidate(); // the tick/veil/bar just changed with no spring behind it
}

/// Drop any QUEUED write for the same item and toggle — see [`Write::family`]. The write already in
/// flight ([`SENT`]) is deliberately untouched: it is on the wire, the client cannot recall it, and
/// the one that supersedes it is queued behind it in the order pressed.
fn coalesce(sid: ServerId, rk: &str, w: Write) {
    let fam = w.family();
    queue().retain(|r| !(crate::plex::same_item((r.sid, &r.rk), (sid, rk)) && r.w.family() == fam));
}

/// Spend one frame of the refused-spawn ladder; true when the next attempt is due. Split out so the
/// ladder is gradeable without a registry or a socket — `pms::retry_due`'s twin.
fn retry_tick() -> bool {
    unsafe {
        let cd = *addr_of!(RETRY_CD);
        if cd > 0 {
            *addr_of_mut!(RETRY_CD) = cd - 1;
        }
        cd <= 1
    }
}

/// Start the next queued write if nothing is out. MAIN THREAD.
fn kick() {
    unsafe {
        if (*addr_of!(SENT)).is_some() || *addr_of!(RETRY_CD) > 0 {
            return;
        }
    }
    while !queue().is_empty() {
        let req = queue().remove(0);
        // Resolved HERE, at the spawn site, and never inside the worker. A slot that has since
        // stopped holding a client is a write with nowhere to go: dropping it silently would be the
        // one PMS write with no line at all, which is what the request-time check above exists to
        // prevent, so it says so here too.
        let Some(c) = crate::plex::client_for(req.sid) else {
            crate::log(&format!(
                "viewstate: rk={} {} DROPPED — server {} left the registry while queued",
                req.rk,
                req.w.name(),
                req.sid.raw()
            ));
            continue;
        };
        let (sid, rk, w, guid) = (req.sid, req.rk.clone(), req.w, req.guid.clone());
        let spawned = crate::task::spawn_small("viewstate", move || {
            // Filled OUTSIDE the guard, so a panicking write still lands (as a failure) rather than
            // latching the queue behind a worker that will never report.
            let done = catch_unwind(move || {
                let ok = w.perform(c, &rk);
                // The fan-out runs INSIDE this worker rather than beside it. The queue is drained
                // one worker at a time precisely so two writes for one item cannot land out of
                // order, and a second thread doing the other sources would put those copies outside
                // that ordering — a watched/unwatched pair could then settle differently per server,
                // which is the one thing this whole feature exists to stop.
                let also = if w.propagates() { fan_out(c, sid, &rk, &guid, w) } else { Vec::new() };
                Done { ok, also }
            })
            .unwrap_or_default();
            *MAIL.lock().unwrap_or_else(|e| e.into_inner()) = Some(done);
        });
        if !spawned {
            // Nothing will ever fill the mailbox. Put the write back at the HEAD — dropping it would
            // silently lose a press the user has already seen take effect on screen — and wait out
            // the ladder before trying again, exactly as `search.rs`/`browse.rs` do.
            queue().insert(0, req);
            unsafe { *addr_of_mut!(RETRY_CD) = RETRY_FRAMES };
            return;
        }
        crate::log(&format!(
            "viewstate: rk={} {} → server {} (off-thread)",
            req.rk,
            w.name(),
            req.sid.raw()
        ));
        unsafe { *addr_of_mut!(SENT) = Some(req) };
        return;
    }
}

/// MAIN THREAD, once a frame, ROUTE-UNCONDITIONAL — a landing must never depend on which screen is
/// mounted, because the user can walk off Home (or off the detail page) between the press and the
/// answer, and the refresh is owed either way.
pub(crate) fn pump() {
    let due = retry_tick();
    if let Some(done) = MAIL.lock().unwrap_or_else(|e| e.into_inner()).take() {
        if let Some(r) = unsafe { (*addr_of_mut!(SENT)).take() } {
            crate::log(&format!(
                "viewstate: rk={} {} ok={} others={}",
                r.rk,
                r.w.name(),
                done.ok as i32,
                done.also.len()
            ));
            // The other sources' copies could not be flipped at the press — their keys are exactly
            // what the resolve DISCOVERS — so they are flipped here, on the main thread, and only
            // the ones their server took. A press that reached no other source does nothing here,
            // which is every press on a one-server install.
            for (osid, ork) in &done.also {
                edit_local(*osid, ork, r.w);
            }
            // The refresh is owed whether or not the server took it: on success it is the reconcile,
            // and on failure it is what puts the optimistic edit back to whatever the server really
            // says. A server that can answer neither question leaves the shelves exactly as they
            // are — `pms.rs`'s per-source rule, unchanged.
            unsafe {
                *addr_of_mut!(WANT_HUBS) = true;
                if let Some(keep) = r.detail {
                    *addr_of_mut!(WANT_DETAIL) = Some(keep);
                }
            }
        }
    }
    if due {
        kick();
    }
    if is_busy() {
        return; // a burst still has writes to send — one refresh at the end of it, not per write
    }
    let (hubs, detail) = unsafe {
        (std::mem::take(&mut *addr_of_mut!(WANT_HUBS)), (*addr_of_mut!(WANT_DETAIL)).take())
    };
    if let Some(keep) = detail {
        // BEFORE the hubs: this re-reads the item the page is mounted on, and the hub refetch's own
        // reconcile (`detail::reselect`) then re-resolves the catalog row under it. If the user has
        // walked to a DIFFERENT page in the meantime it re-reads that one instead — one wasted
        // fetch, the same contract this call has always had ("re-read the mounted item"), and the
        // keep-focus latch simply does not resolve there (`take_kept_episode` starts the row over).
        crate::ui::detail::refresh_view_state(&keep);
    }
    if hubs {
        crate::pms::request_refetch_hubs();
        crate::ui::idle::invalidate();
    }
}

// ---- the fan-out: one title, every source that holds it ---------------------------------------

/// One source's answer to *"do you hold this guid, and under which key?"*. `None` is **did not
/// answer**, `Some(keys)` is what it holds — `Some(empty)` included, which is the source saying it
/// does not have the title. The two are deliberately not collapsed, for the same reason
/// [`crate::plex::Client::find_by_guid`] refuses to collapse them: a share that is merely asleep is
/// not a share that lacks the film, and only one of those is a fact about a library.
type Answer = (ServerId, Option<Vec<String>>);

/// WORKER. Repeat `w` on every OTHER copy of this title, and report the ones the server took so the
/// main thread can flip them locally. **Best-effort throughout**: nothing here can fail, delay or
/// retry the write that has already gone out, and a source that never answers costs one log line.
///
/// `c` is the item's OWN client — the one [`kick`] captured on the main thread and the write itself
/// went out on, handed down rather than re-resolved from `sid` here. That is `pms::kick`'s
/// capture-at-the-spawn-site rule (a worker never asks the registry about the server it was given),
/// and it also keeps [`resolved_guid`]'s failure honest: a slot re-pointed or revoked mid-write
/// cannot turn into a log line claiming the item has no portable id.
fn fan_out(
    c: &crate::plex::Client,
    sid: ServerId,
    rk: &str,
    guid: &str,
    w: Write,
) -> Vec<(ServerId, String)> {
    let sources: Vec<ServerId> = crate::plex::server_ids().collect();
    if sources.len() < 2 {
        // A one-server install pays NOTHING for this feature: no guid lookup, no query, no line in
        // the log. Checked before the lookup below for exactly that reason.
        return Vec::new();
    }
    let Some(guid) = resolved_guid(c, rk, guid) else {
        // Never a silent no-op. With two sources registered, a title that cannot be identified
        // portably is one whose watch state WILL disagree between them, and this line is the only
        // place that says so.
        crate::log(&format!("viewstate: fanout SKIPPED — rk={rk} on server {} has no guid", sid.raw()));
        return Vec::new();
    };
    let answers = ask_sources(&sources, &guid);
    let mut done = Vec::new();
    for (id, key) in fanout_targets((sid, rk), &answers) {
        // `dst`, not a second `c`: shadowing the item's own client inside the one loop that writes
        // to OTHER machines is how a key ends up posted to the wrong server, which is the exact
        // failure this whole function is arranged around.
        let Some(dst) = crate::plex::client_for(id) else { continue };
        let ok = w.perform(dst, &key);
        crate::log(&format!(
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
fn resolved_guid(c: &crate::plex::Client, rk: &str, given: &str) -> Option<String> {
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
fn ask_sources(sources: &[ServerId], guid: &str) -> Vec<Answer> {
    sources
        .iter()
        .map(|&id| {
            let keys = crate::plex::client_for(id)
                .and_then(|c| c.find_by_guid(guid))
                .map(|mc| mc.metadata.iter().map(|m| m.rating_key.clone()).collect::<Vec<_>>());
            let outcome = match &keys {
                None => "no answer".to_string(),
                Some(k) if k.is_empty() => "not held".to_string(),
                Some(k) => format!("holds {}", k.len()),
            };
            crate::log(&format!("viewstate: fanout {guid} server {}: {outcome}", id.raw()));
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
            let listed = out.iter().any(|(s, e)| crate::plex::same_item((*s, e), pair));
            if listed || crate::plex::same_item(pair, origin) {
                continue;
            }
            out.push((*id, k.clone()));
        }
    }
    out
}

/// Drop everything — the identity-change twin of `pms::reset`/`browse::reset`, called from the same
/// place. A queued write belongs to the account that asked for it; one already SENT is on the wire
/// and simply lands into a mailbox nobody is owed a refresh for.
pub(crate) fn reset() {
    queue().clear();
    *MAIL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    unsafe {
        *addr_of_mut!(SENT) = None;
        *addr_of_mut!(RETRY_CD) = 0;
        *addr_of_mut!(WANT_HUBS) = false;
        *addr_of_mut!(WANT_DETAIL) = None;
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    // Every test here that touches a STATIC holds the crate-wide serial lock: the queue, the
    // mailbox and the two refresh latches are process globals, and `request`/`pump` reach into `pms`
    // and `metadata` — whose own tests drive the same statics. `reset()` doubles as setup and
    // teardown. The fan-out block at the bottom is the exception and says so there: most of its
    // cases grade pure functions, which is why the identity rule was factored out of the worker in
    // the first place.
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

    fn req(rk: &str, w: Write, detail: Option<&str>) -> Req {
        Req {
            sid: ServerId::UNSET,
            rk: rk.into(),
            w,
            detail: detail.map(str::to_string),
            guid: String::new(),
        }
    }

    /// Queue a write without spawning anything — the half of [`request`] under test here.
    fn enqueue(rk: &str, w: Write, detail: Option<&str>) {
        coalesce(ServerId::UNSET, rk, w);
        queue().push(req(rk, w, detail));
    }

    /// One source's answer, as [`ask_sources`] would have built it: `Some` is an answer (empty =
    /// "I do not hold it"), `None` is a source that never replied.
    fn held(id: ServerId, keys: &[&str]) -> Answer {
        (id, Some(keys.iter().map(|k| k.to_string()).collect()))
    }

    fn queued() -> Vec<(String, Write)> {
        queue().iter().map(|r| (r.rk.clone(), r.w)).collect()
    }

    /// Two presses on one item are ONE write, and it is the latest one — a scrobble and an
    /// unscrobble that both went out would leave the server saying whichever landed last, which is
    /// not what the user asked for and is not even deterministic.
    #[test]
    fn a_second_press_on_one_item_replaces_the_queued_write_rather_than_joining_it() {
        let _g = crate::testlock::serial();
        reset();

        enqueue("7", Write::Watched, None);
        enqueue("9", Write::Watched, None);
        enqueue("7", Write::Unwatched, None); // the user changed their mind about 7

        assert_eq!(
            queued(),
            vec![("9".into(), Write::Watched), ("7".into(), Write::Unwatched)],
            "7's earlier write is gone and its latest one is queued behind 9's, in press order"
        );
        reset();
    }

    /// …but a deck removal is a DIFFERENT decision about the same item, so a watched toggle must not
    /// swallow it. Both are owed a round trip.
    #[test]
    fn a_deck_removal_and_a_watched_toggle_on_one_item_are_two_writes() {
        let _g = crate::testlock::serial();
        reset();

        enqueue("7", Write::RemoveFromDeck, None);
        enqueue("7", Write::Watched, None);

        assert_eq!(
            queued(),
            vec![("7".into(), Write::RemoveFromDeck), ("7".into(), Write::Watched)],
            "two families, two writes, in the order pressed"
        );
        reset();
    }

    /// A write already ON THE WIRE is not coalesced away: the client cannot recall it, so pretending
    /// otherwise would drop the press that supersedes it and leave the server on the older answer.
    #[test]
    fn a_write_already_in_flight_is_never_coalesced_away() {
        let _g = crate::testlock::serial();
        reset();
        unsafe { *addr_of_mut!(SENT) = Some(req("7", Write::Watched, None)) };

        enqueue("7", Write::Unwatched, None);

        assert!(unsafe { (*addr_of!(SENT)).is_some() }, "the one on the wire stays out");
        assert_eq!(queued(), vec![("7".into(), Write::Unwatched)], "and its successor is queued behind it");
        reset();
    }

    /// The refresh is owed ONCE, at the end of a burst — not per write. Ticking four episodes
    /// watched in a row must cost four writes and one `/hubs` pair, and the detail re-read must
    /// carry the episode the filmstrip has to land back on.
    #[test]
    fn the_refresh_waits_for_the_last_write_of_a_burst_and_carries_the_kept_episode() {
        let _g = crate::testlock::serial();
        reset();

        unsafe { *addr_of_mut!(SENT) = Some(req("1", Write::Watched, Some("1"))) };
        enqueue("2", Write::Watched, Some("2"));
        assert!(is_busy());

        // write 1 answers — the latches arm, but the queue is not empty, so nothing is refreshed
        let landed = unsafe { (*addr_of_mut!(SENT)).take() }.expect("a write was out");
        unsafe {
            *addr_of_mut!(WANT_HUBS) = true;
            *addr_of_mut!(WANT_DETAIL) = landed.detail;
        }
        assert!(is_busy(), "write 2 is still queued — the refresh is not owed yet");

        // …and write 2 answers, superseding which episode the filmstrip lands back on
        let two = queue().remove(0);
        unsafe {
            *addr_of_mut!(WANT_HUBS) = true;
            *addr_of_mut!(WANT_DETAIL) = two.detail;
        }
        assert!(!is_busy());
        let (hubs, detail) = unsafe {
            (std::mem::take(&mut *addr_of_mut!(WANT_HUBS)), (*addr_of_mut!(WANT_DETAIL)).take())
        };
        assert!(hubs, "exactly one hub refetch is owed for the burst");
        assert_eq!(detail.as_deref(), Some("2"), "…and the LAST write's episode is the one kept");
        reset();
    }

    /// A refused spawn must neither panic nor block nor silently lose the press: the write goes back
    /// to the HEAD of the queue and the ladder holds the next attempt off. The real refusal needs
    /// the OS out of threads, which no host test can arrange, so what is graded is the recovery.
    #[test]
    fn a_refused_spawn_keeps_the_write_at_the_head_of_the_queue_and_waits_out_the_ladder() {
        let _g = crate::testlock::serial();
        reset();

        // exactly what `kick`'s refusal branch does
        enqueue("5", Write::Watched, None);
        let head = queue().remove(0);
        queue().insert(0, head);
        unsafe { *addr_of_mut!(RETRY_CD) = RETRY_FRAMES };

        assert_eq!(queued(), vec![("5".into(), Write::Watched)], "the press survives the refusal");
        assert!(unsafe { (*addr_of!(SENT)).is_none() }, "and nothing is recorded as in flight");

        for i in 1..RETRY_FRAMES {
            assert!(!retry_tick(), "frame {i} of the wait is not the due one");
        }
        assert!(retry_tick(), "the ladder is spent after RETRY_FRAMES frames");
        assert_eq!(unsafe { *addr_of!(RETRY_CD) }, 0);
        assert!(retry_tick(), "…and stays due until a fresh refusal re-arms it");
        reset();
    }

    /// `reset` is what an identity change leans on: nothing of the previous account's may be left
    /// queued, in flight, or owed a refresh.
    #[test]
    fn reset_leaves_nothing_queued_in_flight_or_owed() {
        let _g = crate::testlock::serial();
        reset();
        enqueue("1", Write::Watched, Some(""));
        unsafe {
            *addr_of_mut!(SENT) = Some(req("2", Write::Unwatched, None));
            *addr_of_mut!(WANT_HUBS) = true;
            *addr_of_mut!(WANT_DETAIL) = Some("3".into());
            *addr_of_mut!(RETRY_CD) = 9;
        }
        *MAIL.lock().unwrap_or_else(|e| e.into_inner()) = Some(Done::default());

        reset();

        assert!(!is_busy());
        assert!(MAIL.lock().unwrap_or_else(|e| e.into_inner()).is_none());
        assert!(!unsafe { *addr_of!(WANT_HUBS) });
        assert!(unsafe { (*addr_of!(WANT_DETAIL)).is_none() });
        assert_eq!(unsafe { *addr_of!(RETRY_CD) }, 0);
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
        let answers = [held(SRV_A, &["4"]), held(SRV_B, &["4"]), held(SRV_C, &["5274"])];
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
        assert_eq!(from_a, vec![(SRV_B, "4".into())], "pressed on A, so only B is written");
        assert_eq!(from_b, vec![(SRV_A, "4".into())], "…and pressed on B, only A");
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
        assert!(Write::Watched.propagates(), "watched is a claim about the title");
        assert!(Write::Unwatched.propagates(), "…and so is taking that claim back");
        assert!(!Write::RemoveFromDeck.propagates(), "the deck is one server's own surface");
    }

    /// The EDIT the landing applies to a fanned-out copy: the same local flip the press made for
    /// the copy in hand, applied to a `(server, ratingKey)` that could not be known any earlier —
    /// and applied to that PAIR, so the identical key on our own server is left alone. Graded
    /// through the detail store, which is the surface that shows it. (That [`pump`] really calls
    /// this, for every copy the worker reported, is the next test — this one is about the edit.)
    #[test]
    fn the_landing_flips_another_sources_copy_the_press_could_not_reach() {
        let _g = crate::testlock::serial();
        reset();
        // the page is mounted on the SHARE's copy, which the press on our own copy never touched
        crate::metadata::install_for_test(Some(crate::metadata::Detail {
            sid: SRV_B,
            rk: "4".into(),
            resume_ms: 900_000,
            ..Default::default()
        }));

        edit_local(SRV_A, "4", Write::Watched); // our copy: same key, different film, no effect here
        assert!(!crate::metadata::current().unwrap().watched, "A's 4 is not B's 4");

        edit_local(SRV_B, "4", Write::Watched); // …and the fan-out's report, which is
        assert!(crate::metadata::current().unwrap().watched);
        assert_eq!(crate::metadata::current().unwrap().resume_ms, 0, "watched stops offering to resume");

        crate::metadata::install_for_test(None);
        reset();
    }

    /// **The landing itself, driven through [`pump`].** The test above grades what `edit_local`
    /// does; this one grades that `pump` actually CALLS it — for every copy the worker reported,
    /// with the write that was SENT rather than whatever was pressed since — and then clears the
    /// in-flight slot. Without this the fan-out could report copies nobody ever applied, and every
    /// pure test above would still be green.
    #[test]
    fn the_landing_applies_the_workers_whole_report_and_retires_the_write() {
        let _g = crate::testlock::serial();
        reset();
        // the mounted page is the SHARE's copy — the one the press could not reach
        crate::metadata::install_for_test(Some(crate::metadata::Detail {
            sid: SRV_B,
            rk: "4".into(),
            ..Default::default()
        }));

        // a write that went out on OUR copy, and the worker's answer naming the share's
        unsafe {
            *addr_of_mut!(SENT) = Some(Req {
                sid: SRV_A,
                rk: "4".into(),
                w: Write::Watched,
                detail: None, // no detail re-read: this press came from a Home card menu
                guid: "plex://movie/6856893830a4aaafd5c4291d".into(),
            })
        };
        *MAIL.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Done { ok: true, also: vec![(SRV_B, "4".into())] });

        pump();

        assert!(
            crate::metadata::current().unwrap().watched,
            "the page mounted on the share's copy is flipped by the landing, not by the press"
        );
        assert!(!is_busy(), "…and the write that reported is no longer in flight");
        assert!(MAIL.lock().unwrap_or_else(|e| e.into_inner()).is_none(), "the mailbox is drained");

        crate::metadata::install_for_test(None);
        reset();
    }
}
