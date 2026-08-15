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

/// The worker's ONLY output: whether the server took the write it was given. There is at most one
/// worker at a time, and the main thread clears the slot as it takes it.
static MAIL: Mutex<Option<bool>> = Mutex::new(None);

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
pub(crate) fn request(sid: ServerId, rk: &str, w: Write, detail: Option<String>) -> bool {
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
    // from now. Both edits are no-ops where the item does not appear, so a Home press with no detail
    // page mounted costs one catalog walk and nothing else.
    match w {
        Write::Watched | Write::Unwatched => {
            let on = w == Write::Watched;
            crate::pms::edit_item(sid, rk, crate::pms::LocalEdit::Watched(on));
            crate::metadata::set_watched_local(sid, rk, on);
        }
        // The one edit that can be stated with certainty: the server hides the item from the deck
        // and keeps everything else about it (`plex::Client::remove_from_continue_watching`).
        Write::RemoveFromDeck => {
            crate::pms::edit_item(sid, rk, crate::pms::LocalEdit::LeftTheDeck);
        }
    }
    crate::ui::idle::invalidate(); // the tick/veil/bar just changed with no spring behind it
    coalesce(sid, rk, w);
    queue().push(Req { sid, rk: rk.to_string(), w, detail });
    kick();
    true
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
        let (rk, w) = (req.rk.clone(), req.w);
        let spawned = crate::task::spawn_small("viewstate", move || {
            // Filled OUTSIDE the guard, so a panicking write still lands (as a failure) rather than
            // latching the queue behind a worker that will never report.
            let ok = catch_unwind(move || w.perform(c, &rk)).unwrap_or(false);
            *MAIL.lock().unwrap_or_else(|e| e.into_inner()) = Some(ok);
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
    if let Some(ok) = MAIL.lock().unwrap_or_else(|e| e.into_inner()).take() {
        if let Some(r) = unsafe { (*addr_of_mut!(SENT)).take() } {
            crate::log(&format!("viewstate: rk={} {} ok={}", r.rk, r.w.name(), ok as i32));
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

    // Every test here holds the crate-wide serial lock: the queue, the mailbox and the two refresh
    // latches are process globals, and `request` reaches into `pms` and `metadata` — whose own tests
    // drive the same statics. `reset()` doubles as setup and teardown.
    //
    // None of these calls `kick`. A host test has no registry, so a kicked write resolves no client
    // and is dropped with a log line — which is correct behaviour and useless as a subject. What is
    // worth grading is the bookkeeping around it, which is what these drive directly.

    fn req(rk: &str, w: Write, detail: Option<&str>) -> Req {
        Req { sid: ServerId::UNSET, rk: rk.into(), w, detail: detail.map(str::to_string) }
    }

    /// Queue a write without spawning anything — the half of [`request`] under test here.
    fn enqueue(rk: &str, w: Write, detail: Option<&str>) {
        coalesce(ServerId::UNSET, rk, w);
        queue().push(req(rk, w, detail));
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
        *MAIL.lock().unwrap_or_else(|e| e.into_inner()) = Some(false);

        reset();

        assert!(!is_busy());
        assert!(MAIL.lock().unwrap_or_else(|e| e.into_inner()).is_none());
        assert!(!unsafe { *addr_of!(WANT_HUBS) });
        assert!(unsafe { (*addr_of!(WANT_DETAIL)).is_none() });
        assert_eq!(unsafe { *addr_of!(RETRY_CD) }, 0);
    }
}
