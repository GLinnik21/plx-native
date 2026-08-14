//! search — the search screen's data layer (the store `ui/search/` draws).
//!
//! One query, fanned out across every registered source, merged into typed shelves and pumped once
//! a frame while the screen is up. The types down to [`shelves`] are what `ui/search/` draws;
//! everything below the *fetch plumbing* banner is the machine that fills them. [`terms`] is the
//! one predicate the screen shares with the store — see its doc before writing a second one.
//!
//! ## The endpoint, and what it actually returns
//!
//! `GET /hubs/search?query=…&limit=…` ([`crate::plex::Client::search`], written long before this
//! screen and dead until now). The spec says it "is intended to be very fast, and called as the
//! user types", which is the design this store is built for. Three things were measured against
//! PMS 1.43.3 rather than taken from the spec, each of which decides something here:
//!
//! - **A one-character query returns every hub empty.** So [`MIN_QUERY`] is 2, and the first
//!   keystroke of every search costs no round trip at all.
//! - **Hub ORDER moves per query** — `sta` ranks people first, `star` ranks films first. So the
//!   shelf order here is FIXED ([`KINDS`]) and ranking is honoured only *inside* a shelf.
//!   Reordering shelves per keystroke would move the row under the user's focus while they type.
//! - **Items arrive in two different containers** — see [`crate::plex::Hub::directory`]. That is
//!   why [`Item`] has two variants instead of being one struct.
//!
//! ## Multi-source
//!
//! Search is single-server: `/hubs/search` answers for the machine you asked, and
//! `docs/shared-servers.md` states that nothing aggregates server-side — the merge is the client's
//! job. So this fans out one query per [`crate::plex::server_ids`] and merges into the shelves
//! below, which is why every [`Item`] carries its own `ServerId`.
//!
//! The merge is **round robin** ([`merge`]), not source-by-source the way Home groups its shelves.
//! Home groups because a shelf there BELONGS to a source and adjacency is what says so; a search
//! shelf is genuinely mixed (`ui/search/results.rs`: the heading "cannot claim an owner — the
//! owner annotation follows FOCUS"), so concatenating would bury a friend's best match behind
//! twenty-three worse ones of ours. There is no cross-server relevance score to sort on — the only
//! ranking any server hands over is the order of its own list — so taking one from each in turn is
//! the most each server's ranking can be honoured at once.
//!
//! A source that fails contributes nothing and **fails nobody else**: each has its own in-flight
//! claim, its own retry backoff and its own mailbox, and [`State`] is `Failed` only when every one
//! of them is.
//!
//! ## The idiom to copy
//!
//! `person.rs`, not `browse.rs`: generation + a **monotone** mailbox write + [`supersede`] + a
//! per-fetch in-flight flag and retry backoff, pumped once a frame. As-you-type guarantees
//! overlapping workers, and a monotone mailbox is what stops a slow answer for `wal` repopulating
//! the results for `wallace`. Debounce is `ui/detail.rs`'s `season_settle` accumulator. Two rules
//! that are easy to miss and both wedge the screen forever if missed: release the in-flight flag
//! when `spawn_small` REFUSES, and call [`crate::ui::idle::invalidate`] on every landing including
//! the failure branch.
#![allow(dead_code)]

use crate::plex::ServerId;
use crate::pms::{parse_item, PmsMovie};
use std::panic::catch_unwind;
use std::ptr::{addr_of, addr_of_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

/// Below this many characters the server answers with nothing, so asking is pure latency.
/// Measured, not guessed — see the module doc.
pub(crate) const MIN_QUERY: usize = 2;

/// The typed shelves, in the order they are drawn — **fixed**, never the server's ranking.
/// `Search Screen.dc.html`: "Results are ranked inside a shelf, never across them."
pub(crate) const KINDS: [Kind; 5] = [Kind::Movie, Kind::Show, Kind::Episode, Kind::Person, Kind::Collection];

/// How many shelves there are — the index space every per-source projection is keyed by.
const NKIND: usize = KINDS.len();

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Movie,
    Show,
    Episode,
    /// Cast **and** crew: the server splits these into an `actor` hub and a `director` hub, and
    /// the design draws one shelf. Merging them here rather than in the UI keeps "what is a
    /// shelf" a data question.
    Person,
    Collection,
}

impl Kind {
    /// The shelf heading.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Kind::Movie => "Movies",
            Kind::Show => "TV Shows",
            Kind::Episode => "Episodes",
            Kind::Person => "Cast & Crew",
            Kind::Collection => "Collections",
        }
    }
    /// The count read-out beside it — how many RESULTS are on this shelf. People are counted as
    /// people; everything else as results. See [`items_word`] for the other count on this screen.
    pub(crate) fn count_word(self, n: usize) -> &'static str {
        match (self, n) {
            (Kind::Person, 1) => "person",
            (Kind::Person, _) => "people",
            (_, 1) => "result",
            (_, _) => "results",
        }
    }
    /// Which hub identifiers feed this shelf.
    pub(crate) fn hubs(self) -> &'static [&'static str] {
        match self {
            Kind::Movie => &["movie"],
            Kind::Show => &["show"],
            Kind::Episode => &["episode"],
            Kind::Person => &["actor", "director"],
            Kind::Collection => &["collection"],
        }
    }
}

/// The MEMBERSHIP read-out: how many things are inside ONE result ("12 items", a collection's
/// extent on the focused tile's second line). A sibling of [`Kind::count_word`] and deliberately
/// not an arm of it — that one answers "how many results are on this shelf", so a Collections
/// heading saying "3 results" and a collection tile saying "12 items" are both right and the two
/// numbers are never the same number. It lives here because the count vocabulary for this feature
/// is one thing: `ui/search/results.rs` spelled its own `if n == 1 {""} else {"s"}` beside a
/// `count_word` call on the very next screen, which is how one feature ends up with two plural
/// rules and then two ways to spell a zero.
///
/// Takes an `i64` because that is what the wire hands over ([`TagHit::count`]): a server that sends
/// a nonsense negative gets the plural word, rather than a cast at the call site that could wrap it
/// into "1 item".
pub(crate) fn items_word(n: i64) -> &'static str {
    match n {
        1 => "item",
        _ => "items",
    }
}

/// A person or collection result: the `Directory[]` shape, which carries no `ratingKey` and — for
/// a collection — no artwork either. Kept distinct from [`PmsMovie`] rather than flattened into
/// it, because the two open different screens and a struct with half its fields permanently empty
/// invites code that forgets which half it is holding.
///
/// It is a projection of [`crate::plex::Tag`] — the SAME record the detail page's cast row is
/// built from, which is what lets a search hit be handed to the person page unchanged (and lets
/// `Tag::is_person` match one against a credit).
#[derive(Clone, Default)]
pub(crate) struct TagHit {
    pub(crate) sid: ServerId,
    pub(crate) name: String,
    /// plex.tv's global person guid — the only portable identity here, and the only id
    /// `discover.provider.plex.tv` answers to. Empty on a collection.
    pub(crate) tag_key: String,
    /// The server-local numeric tag id, as a string. Dense from 1 and meaningless off this server.
    pub(crate) id: String,
    /// The artwork source, to be handed to the poster store **verbatim** — there is no second
    /// image route to build, and `posters::poster_key` is where the whole story is written down.
    ///
    /// **A person's is ABSOLUTE**: `https://metadata-static.plex.tv/…jpg`, a host `stream.rs` can
    /// never dial (no DNS, no TLS). It does not need to. The URL goes in as the `url=` value of
    /// `/photo/:/transcode`, percent-encoded whole, the request goes to our own PMS, and the
    /// server fetches it over TLS for us. Verified live against PMS 1.43.3 (2026-08-14): `200
    /// image/jpeg` at exactly the requested size.
    ///
    /// **A collection's is EMPTY**, and that is the server's answer, not a parse gap — a
    /// `/hubs/search` `collection` row carries no `thumb` and no `ratingKey`, only `key`, a tag
    /// `id` and a `collection://` guid. An empty source is refused by `poster_key` rather than
    /// turned into a request (the bare `url=` is a 404 here, and a shelf of them would spend the
    /// store on failures), so the tile draws the skeleton face the design specifies for art the
    /// server has not given us.
    ///
    /// Resolving one anyway is possible and was measured, in case a later unit wants it: `GET
    /// /library/sections/{librarySectionID}/collections` returns `Metadata[]` with real `thumb`
    /// paths, and the join is **`index` == this row's `id`** — the id here is NOT the collection's
    /// `ratingKey`, which is a different number entirely. (`guid` would join too, but this struct
    /// does not keep it: add a field before reaching for that, rather than re-parsing the hub.) Note the shape of the
    /// cost, which is better than it first looks: **one request per library SECTION**, not per
    /// collection, and cacheable for the session. Neither of the two obvious shortcuts works —
    /// following `key` returns the collection's MEMBERS under the section's own generic art, and
    /// the `/library/sections/{k}/collection` tag axis carries no thumb at all. It is still a
    /// second store with its own fetch, cache and invalidation for a shelf that is usually a row
    /// or two, which is why the skeleton stands for now.
    pub(crate) thumb: String,
    /// The listing this tag opens — `/library/sections/1/all?collection=6068`.
    pub(crate) key: String,
    /// How many items carry it, for the caption line.
    pub(crate) count: i64,
}

#[derive(Clone)]
pub(crate) enum Item {
    /// A movie, show or episode: the ordinary card DTO, so every existing tile path draws it
    /// unchanged — resume bar, watched mark, ambient blur and all.
    Media(PmsMovie),
    Tag(TagHit),
}

impl Item {
    pub(crate) fn title(&self) -> &str {
        match self {
            Item::Media(m) => &m.title,
            Item::Tag(t) => &t.name,
        }
    }
    pub(crate) fn sid(&self) -> ServerId {
        match self {
            Item::Media(m) => m.sid,
            Item::Tag(t) => t.sid,
        }
    }
}

pub(crate) struct Shelf {
    pub(crate) kind: Kind,
    pub(crate) items: Vec<Item>,
}

/// What the screen should be saying, which is not the same question as "are there items".
///
/// The distinction `browse.rs` had to learn the hard way: `Ready` with nothing in it is an ANSWER
/// (the server has no match) and reads as "No results"; `Failed` is a fault and reads as one. An
/// empty store alone cannot tell them apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum State {
    /// No query, or one below [`MIN_QUERY`]. Nothing has been asked, so nothing is pending.
    Idle,
    /// A query is settling or in flight and no answer for it has arrived yet.
    Searching,
    /// At least one source answered — possibly with nothing, which is still an answer and reads as
    /// "No results". See [`state_from`] for why one answer is enough and why an EMPTY one waits.
    Ready,
    /// Every source has had its say and none of them answered — which is also the verdict on a
    /// roster with nothing in it to ask.
    ///
    /// The shelves are empty here **by construction**, not by policy: a query change clears them
    /// ([`set_query`]) and [`merge`] draws only from a source whose status is [`Status::Answered`],
    /// so nothing a previous query fetched can still be on screen. That is exactly why this variant
    /// exists — the fault sentence is read off the STATE, because an empty store on its own cannot
    /// be told apart from a [`State::Ready`] one. (This line long said the opposite: "whatever is in
    /// the store is the PREVIOUS answer and is left alone", which is `browse.rs`'s rule, not this
    /// store's — search drops the old answer the moment the terms change.)
    Failed,
}

impl State {
    /// For the event log only — the screen never prints this.
    fn name(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Searching => "searching",
            State::Ready => "ready",
            State::Failed => "failed",
        }
    }
}

static mut QUERY: String = String::new();
static mut SHELVES: Vec<Shelf> = Vec::new();
static mut STATE: State = State::Idle;

/// The query as typed, verbatim — trailing space and all, because the FIELD draws this.
pub(crate) fn query() -> &'static str {
    unsafe { &*addr_of!(QUERY) }
}

/// **THE predicate for "is this a real query"**, and the terms a fetch would actually be addressed
/// to — `None` when the query is too short to be worth a round trip. Two halves, and each one is a
/// decision rather than a formality:
///
/// - **Trimmed**, because leading/trailing space is a fact about the FIELD and not about what is
///   being looked for — which is also what lets [`set_query`] tell "the user typed a space" apart
///   from "the user is looking for something else".
/// - **[`MIN_QUERY`] counted in CHARACTERS, not bytes**, because the floor is a measured fact about
///   what the SERVER answers (module doc) and the query arrives as UTF-8 off the television's own
///   keyboard: `len()` would put a one-letter Cyrillic or CJK query over a floor the server will
///   still answer nothing to, spending a round trip per keystroke on a guaranteed empty response.
///
/// `pub(crate)` so `ui/search/` asks instead of re-deriving it. "Is a search on screen", "may this
/// term be filed in the recents", and "is a fetch owed" are the SAME question, and a screen that
/// spells its own copy of the test can disagree with the store about whether a search is happening
/// at all — a results region drawn over a store parked on [`State::Idle`], which never asked
/// anything and so will never answer.
pub(crate) fn terms(q: &str) -> Option<&str> {
    let t = q.trim();
    (t.chars().count() >= MIN_QUERY).then_some(t)
}

/// Replace the query. Idempotent on an unchanged string, so a caller may hand it every frame.
pub(crate) fn set_query(q: &str) {
    // Two different changes, and only one of them is news for the SERVER: the field draws the raw
    // string (so a typed space must repaint), while the fetch is addressed to the trimmed one (so
    // that same space must not supersede an answer that is still correct). Collapsing the two
    // re-asks the whole roster for the identical terms every time the space bar is pressed.
    let restart = unsafe {
        let cur = &mut *addr_of_mut!(QUERY);
        if cur == q {
            return;
        }
        let restart = cur.trim() != q.trim();
        cur.clear();
        cur.push_str(q);
        restart
    };
    if restart {
        // A new query invalidates the old answer immediately. Leaving the previous shelves up
        // while the next lands would show results for a string that is no longer on screen.
        supersede();
        unsafe {
            (*addr_of_mut!(SHELVES)).clear();
            *addr_of_mut!(STATE) = if terms(q).is_some() { State::Searching } else { State::Idle };
            // …and the debounce restarts with it: the fetch is owed to the LAST keystroke, not to
            // the first one of the burst.
            *addr_of_mut!(SETTLE) = 0.0;
            *addr_of_mut!(ARMED) = terms(q).is_some();
        }
    }
    crate::ui::idle::invalidate();
}

pub(crate) fn state() -> State {
    unsafe { *addr_of!(STATE) }
}

/// The CONTENT epoch — bumped by every query change and every [`reset`]. Read by the screen to
/// notice that the shelves under it have been replaced, including by something the screen did not
/// do itself (a profile switch calling [`reset`]), which is what earns it a cross-fade rather than
/// a cut. `browse::query_gen` is the same accessor for the same reason.
pub(crate) fn query_gen() -> u32 {
    GEN.load(Ordering::SeqCst)
}

/// The shelves, already in [`KINDS`] order, with empty ones omitted — an empty type draws nothing
/// at all, so the UI never has to test for it.
pub(crate) fn shelves() -> &'static [Shelf] {
    unsafe { &*addr_of!(SHELVES) }
}

// ---- fetch plumbing (debounce + generation + single-flight + mailbox + retry backoff) ----------

/// How long the query must hold still before it is asked. `ui/detail.rs`'s `SEASON_SETTLE` is 0.2 s
/// for a D-pad step; typing on the television's own keyboard arrives faster than that and in
/// bursts, so this is a shade longer — a five-letter word costs ONE round trip rather than four.
/// It is the whole reason the "called as the user types" endpoint is affordable at all.
const SETTLE_S: f32 = 0.25;

/// Items asked for **per hub** — `plex-openapi.json`: "The number of items to return per hub. 3 if
/// not specified", which is why the parameter is always sent at all.
///
/// **Per HUB is the word that decides the number, and it is not [`SHELF_MAX`].** A search response
/// carries every hub type the server knows about — 17 on this set — so `limit` is multiplied by
/// however many of them the query happens to touch, not by the five this screen draws. Asking 24
/// buys a full row from a single source and pays for up to ~400 `Metadata` records per settled
/// keystroke, over `stream.rs`'s blocking socket, on an endpoint whose whole selling point is being
/// fast enough to call as the user types. Half a row from each of two sources still fills the
/// merged cap exactly, and a search that needs the 13th hit needs a better query instead.
const LIMIT: i64 = 12;

/// Per-shelf item cap, for the same reason `person.rs` carries one: a `CardRow` owns exactly
/// [`crate::ui::card_row::MAX_ROW_ITEMS`] focus-scale springs and `scale(i)` clamps past the end,
/// so an item beyond the cap would draw with the last cell's pop and never pop at all when focused.
const SHELF_MAX: usize = crate::ui::card_row::MAX_ROW_ITEMS;

/// Fetch-slot ceiling — the registry's own `MAX_SERVERS`, named rather than copied, so raising the
/// ceiling cannot leave this module quietly never asking the extra servers.
///
/// It was a separate `16` for a while, on the reasoning that `MAX_SERVERS` was `pub(super)` inside
/// `plex/` and could not be named here. That was already false when it was written — the constant
/// is `pub` and re-exported, and `person.rs` names it for exactly this purpose — so the alias is
/// kept only as a local name for what the arrays below are sized by.
///
/// Nothing here indexes by a raw server id it has not first clamped: [`nsrc`] is the one place
/// that happens, and every array is walked through it. So a registry that outgrew this would cost
/// an unsearched server, never a write off the end.
const NSRC: usize = crate::plex::MAX_SERVERS;

/// Bumped by every query change and every [`reset`]: a landing whose generation no longer matches
/// is discarded by [`pump`], so a slow answer for `wal` can never repopulate the results for
/// `wallace`.
static GEN: AtomicU32 = AtomicU32::new(0);

/// The claim that source `i`'s fetch is out. Released by the mailbox take — the only event that
/// knows the fetch is over — so anything that ends one by another route must release it itself:
/// [`supersede`] drops the mailbox the take would have come from, and a refused `spawn_small` never
/// produces one at all. Miss either and the source stays latched and never searches again.
///
/// This and [`SLOT`] are the two halves a WORKER touches, which is why they stay `Sync` statics
/// rather than fields on [`Source`] beside the main-thread-only status/backoff/answer.
///
/// Same latch `person.rs`/`browse.rs` document, and the same honest caveat: it bounds spawns per
/// *pump*, it is not a hard one-worker-at-a-time interlock. **Two ways past it, and they are
/// different sizes.**
///
/// *Within one generation* a stale landing releases the claim before the generation check, so it
/// can free a newer fetch's claim and buy one duplicate request. Bounded at one, and neither can
/// wedge nor corrupt: [`land`] is monotone and [`pump`] discards stale mail.
///
/// *Across generations there is no bound at all*, and that half is written down here because
/// nothing else says it. [`supersede`] releases the claim while the superseded worker is **still
/// running** — it drops the ANSWER, it cannot abort the fetch, since the worker is parked in a
/// blocking `stream.rs` GET with no cancellation to poll. So every keystroke that changes the terms
/// can leave one live worker per source behind and immediately free the slot for another, each
/// holding a `spawn_small` 256 KB stack until its request returns. [`SETTLE_S`]'s 0.25 s debounce is
/// what bounds this in the normal case (one settled query per source, and the previous one has
/// usually landed); a source that STALLS rather than failing — accepted, held open, never answered,
/// `tools/netcond.py`'s `stall` — is the case that genuinely accumulates, until `task.rs`'s thread
/// ceiling refuses a spawn, which [`maybe_spawn`] already treats as a retry rather than a fault. The
/// fix, if it is ever worth one, is a cancel token the worker polls between reads; a tighter claim
/// here cannot help, because the claim is not what is holding the thread.
static IN_FLIGHT: [AtomicBool; NSRC] = [const { AtomicBool::new(false) }; NSRC];

/// ~2 s at 60 fps — the same backoff `person.rs`/`browse.rs` use for a failed fetch. Counted down in
/// [`Source::retry_cd`].
const RETRY_FRAMES: u32 = 120;

/// Seconds the current query has held still, and whether it is still owed a fetch. Main thread
/// only, advanced by [`pump`] — the `season_settle` accumulator, one screen over.
static mut SETTLE: f32 = 0.0;
static mut ARMED: bool = false;

/// What one source's finished fetch delivers. `None` means the fetch FAILED (transport, parse, or
/// a panicking worker) and must be retried — kept distinguishable from a successful answer that
/// happens to be empty, which is the "one wifi hiccup blanked a populated grid" bug `browse.rs`
/// carries its `total < 0` sentinel for.
struct Mail {
    gen: u32,
    what: Option<Projection>,
}

/// One source's results, keyed by [`KINDS`] index. The worker builds this, so no wire DTO ever
/// crosses the mailbox.
type Projection = [Vec<Item>; NKIND];

/// One mailbox per source, indexed by [`ServerId::raw`].
static SLOT: [Mutex<Option<Mail>>; NSRC] = [const { Mutex::new(None) }; NSRC];

/// What the current generation's attempt at source `i` has come to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    /// never asked, or asked and still out
    Pending,
    /// answered — possibly with nothing, which is an ANSWER. A source in this state is not asked
    /// again for this generation, which is why it can never regress to [`Status::Failed`].
    Answered,
    /// the attempt failed; [`Source::retry_cd`] is counting down to the next one
    Failed,
}

/// One source's contribution to the merge, plus what its last attempt did. **Main thread only, in
/// full** — [`pump`], [`record`], [`supersede`] and [`maybe_spawn`] are the only code that touches
/// one, and all four run on the frame loop. The worker's two halves are [`IN_FLIGHT`] and [`SLOT`],
/// which is why those stay separate `Sync` statics.
struct Source {
    status: Status,
    /// Frames left before this source may be asked again after a FAILED attempt ([`pump`]
    /// decrements, [`record`] arms it at [`RETRY_FRAMES`]). PER SOURCE on purpose: a friend's server
    /// that is off must not hold our own library's results off for two seconds a go.
    ///
    /// A field rather than the parallel `RETRY_CD` array this used to be. It never moved
    /// independently of `status` — armed with it in [`record`], read with it in [`maybe_spawn`],
    /// cleared with it in [`supersede`] — so a second array was a second place to forget, and the
    /// per-slot reset is now the one assignment `Source::EMPTY` already meant.
    retry_cd: u32,
    /// The last successful answer for the CURRENT generation. Meaningful only while `status` is
    /// [`Status::Answered`], which is what [`merge`] filters on.
    items: Projection,
}

impl Source {
    const EMPTY: Source = Source { status: Status::Pending, retry_cd: 0, items: [const { Vec::new() }; NKIND] };
}

static mut SRC: [Source; NSRC] = [const { Source::EMPTY }; NSRC];

/// How many registry slots this store fans out over — **the one place a raw `ServerId` is clamped**
/// (see [`NSRC`]). The roster only ever grows, so a slot past this count has never been asked
/// anything and has nothing in its mailbox.
fn nsrc() -> usize {
    crate::plex::server_count().min(NSRC)
}

/// Post a finished fetch to its mailbox. MONOTONE: an older fetch landing late must never clobber a
/// newer result the pump has not consumed yet. Named (not inlined in the worker closure) because
/// the guard is the one piece of this machinery a test cannot reach through [`set_query`] —
/// reaching it needs two overlapping real fetches.
fn land(i: usize, gen: u32, what: Option<Projection>) {
    let mut slot = SLOT[i].lock().unwrap_or_else(|e| e.into_inner());
    let beats = match slot.as_ref() {
        None => true,
        // A newer generation always wins — the monotone rule this mailbox exists for.
        Some(m) if m.gen != gen => m.gen < gen,
        // …but at the SAME generation an ANSWER beats a failure. The in-flight claim bounds spawns
        // and is not a hard interlock (see `IN_FLIGHT`), so two workers can be out for one source
        // at one generation; with the loser's `None` arriving first, the real response was dropped
        // and the source then sat out a ~2 s backoff holding a good answer. `record` already
        // encodes this preference on the other side of the pump — "a late failure cannot unsay an
        // answer" — and `land` is what decides which mail survives to be read at all.
        Some(m) => m.what.is_none() && what.is_some(),
    };
    if beats {
        *slot = Some(Mail { gen, what });
    }
}

/// Invalidate everything in flight: bump the generation (a late landing is discarded), drop every
/// mailbox, release the single-flight claims with them, and put every source back to
/// [`Source::EMPTY`] — status, retry backoff and answer together, since they are one source's state
/// and "never asked" has exactly one spelling. The ONE place those move together, and what a
/// keystroke calls. It does NOT stop the workers; see [`IN_FLIGHT`] for what that costs.
fn supersede() {
    GEN.fetch_add(1, Ordering::SeqCst);
    for i in 0..NSRC {
        *SLOT[i].lock().unwrap_or_else(|e| e.into_inner()) = None;
        IN_FLIGHT[i].store(false, Ordering::SeqCst);
        unsafe { (*addr_of_mut!(SRC))[i] = Source::EMPTY };
    }
}

/// Advance the debounce and land whatever arrived. Called once a frame from the screen's update.
/// Returns whether anything changed, so the caller can re-clamp focus.
pub(crate) fn pump(dt: f32) -> bool {
    unsafe {
        if *addr_of!(ARMED) {
            let s = &mut *addr_of_mut!(SETTLE);
            *s += dt;
            if *s >= SETTLE_S {
                *addr_of_mut!(ARMED) = false;
                if let Some(q) = terms(query()) {
                    crate::log(&format!("search: q='{q}' settled, asking {} source(s)", nsrc()));
                }
            }
        }
    }
    let n = nsrc();
    let mut landed = false;
    for i in 0..n {
        unsafe {
            let cd = &mut (*addr_of_mut!(SRC))[i].retry_cd;
            if *cd > 0 {
                *cd -= 1;
            }
        }
        let taken = SLOT[i].lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(m) = taken {
            // the take ALWAYS releases the single-flight claim, whatever the landing turns out to
            // be — dropping a stale one without this is how the flag latches forever
            IN_FLIGHT[i].store(false, Ordering::SeqCst);
            if m.gen == GEN.load(Ordering::SeqCst) {
                record(i, m.what);
                landed = true;
            }
            // else superseded: this is news about a query that is no longer on screen. The FAILURE
            // arm is skipped with it on purpose — a stale failure that armed the backoff would
            // delay the current query's first answer by ~2 s for an error that was never about it.
        }
        maybe_spawn(i);
    }
    // The SHELVES are rebuilt only when something landed, but the STATE is recomputed every frame:
    // it is a scan of at most NSRC statuses, and making it conditional on a landing left the one
    // case that can never produce one — an EMPTY roster — parked on `Searching` forever, which is
    // precisely the endless spinner `state_from`'s empty arm exists to prevent. Reachable in
    // practice: `/tmp/plxnative-search=<q>` forces the route whether or not a server was installed.
    if landed {
        rebuild(n);
    }
    let state = state_from(live_sources(n), terms(query()).is_some());
    let moved = state != self::state();
    if moved {
        crate::log(&format!("search: q='{}' state={}", query().trim(), state.name()));
        unsafe { *addr_of_mut!(STATE) = state };
    }
    if !landed && !moved {
        return false;
    }
    // every landing repaints, the failure branch included: without this the screen sits on a
    // spinner that has already been answered until the next keypress happens to invalidate it
    crate::ui::idle::invalidate();
    true
}

/// Record ONE source's landing. The merge itself is [`rebuild`], run once after the whole sweep, so
/// two sources landing in the same frame cost one rebuild rather than two.
fn record(i: usize, what: Option<Projection>) {
    let q = query().trim();
    match what {
        // A failure for a source that has ALREADY answered this query is dropped on the floor. The
        // duplicate-spawn race [`IN_FLIGHT`] documents is what makes this reachable: two workers can
        // briefly be out for one source at one generation, and if the loser's `None` arrived after
        // the winner's answer, `Status::Answered` would regress to `Failed` — dropping that
        // source's already-drawn results out of the merge for a two-second backoff, over an error
        // about a request whose answer we are holding.
        None if unsafe { (*addr_of!(SRC))[i].status } == Status::Answered => {
            crate::log(&format!("search: q='{q}' sid={i} late failure ignored — already answered"));
        }
        None => {
            unsafe {
                let s = &mut (*addr_of_mut!(SRC))[i];
                s.status = Status::Failed;
                s.retry_cd = RETRY_FRAMES;
            }
            crate::log(&format!("search: q='{q}' sid={i} FAILED, retry in {RETRY_FRAMES}f"));
        }
        Some(items) => {
            let counts: Vec<String> =
                KINDS.iter().enumerate().map(|(k, kind)| format!("{}={}", kind.title(), items[k].len())).collect();
            crate::log(&format!("search: q='{q}' sid={i} hubs {}", counts.join(" ")));
            // The two fields an answer decides, and `retry_cd` is deliberately not one of them: a
            // source that has answered is refused by `maybe_spawn` on `status` alone, and the next
            // query resets the whole record through `Source::EMPTY`. (Assigning a whole `Source`
            // here would zero a backoff instead of leaving it — the same behaviour today, but only
            // by accident of nothing reading it.)
            unsafe {
                let s = &mut (*addr_of_mut!(SRC))[i];
                s.status = Status::Answered;
                s.items = items;
            }
        }
    }
}

/// The first `n` registry slots, as a slice — the one place a raw `static mut` array becomes a
/// borrow. The whole array is taken first and sliced after, because `&(*addr_of!(SRC))[..n]`
/// autorefs the raw pointer's target implicitly, which rustc denies outright
/// (`dangerous_implicit_autorefs`).
fn live_sources(n: usize) -> &'static [Source] {
    let all: &'static [Source; NSRC] = unsafe { &*addr_of!(SRC) };
    &all[..n]
}

/// Recompute the merged shelves from every source's last answer. The [`State`] is NOT set here —
/// [`pump`] recomputes that every frame, landing or no landing (see the note there).
fn rebuild(n: usize) {
    let shelves = merge(live_sources(n));
    let items: usize = shelves.iter().map(|s| s.items.len()).sum();
    crate::log(&format!("search: q='{}' shelves={} items={}", query().trim(), shelves.len(), items));
    unsafe { *addr_of_mut!(SHELVES) = shelves };
}

/// The merged result set: [`KINDS`] order, empty shelves omitted, sources taken **round robin** so
/// no source can bury another (module doc). Pure, so the ordering rule is graded on the host rather
/// than inferred from a screenshot with two servers plugged in.
fn merge(sources: &[Source]) -> Vec<Shelf> {
    let mut out = Vec::new();
    for (k, kind) in KINDS.iter().enumerate() {
        // only a source that ANSWERED contributes: one still pending has nothing to say yet, and
        // one that failed has nothing to say at all
        let live: Vec<&Vec<Item>> =
            sources.iter().filter(|s| s.status == Status::Answered).map(|s| &s.items[k]).collect();
        let deepest = live.iter().map(|v| v.len()).max().unwrap_or(0);
        let mut items: Vec<Item> = Vec::new();
        'fill: for d in 0..deepest {
            for v in &live {
                let Some(it) = v.get(d) else { continue };
                // **The same person on two servers is one person.** `project` folds per RESPONSE,
                // so it cannot see across sources — and this is the only place both are in hand.
                // `same_tag`'s doc already claimed the round-robin merge brought them together;
                // nothing here acted on it, so a shared actor drew twice with a split count.
                //
                // `tagKey` is what makes it safe across machines: `same_tag` compares the local id
                // only within one server, so two servers' id 921 stay two people.
                if let Item::Tag(t) = it {
                    if let Some(prev) = items.iter_mut().find_map(|e| match e {
                        Item::Tag(p) if same_tag(p, t) => Some(p),
                        _ => None,
                    }) {
                        prev.count += t.count;
                        if prev.thumb.is_empty() {
                            prev.thumb = t.thumb.clone();
                        }
                        continue;
                    }
                }
                items.push(it.clone());
                if items.len() >= SHELF_MAX {
                    break 'fill;
                }
            }
        }
        if !items.is_empty() {
            out.push(Shelf { kind: *kind, items });
        }
    }
    out
}

/// What the screen should be saying, from every source's last word. Pure.
///
/// **`Ready` is not "somebody replied", it is "there is nothing more to wait for" — with one
/// exception that has to be made.** `Ready` and no items is the *"No results for wallace"* screen,
/// and saying that while a source is still out is a sentence the next second contradicts: our own
/// server answers empty in 20 ms on the LAN, a friend's takes a second, and the user reads "no
/// results" and then watches a shelf appear under it. So a still-pending source holds `Searching`.
///
/// The exception is a source that HAS given us something to draw: those results go up at once
/// rather than waiting on the slowest server in the house, because a populated screen is never
/// contradicted by more arriving under it — it only grows.
///
/// `Failed` therefore means every source has had its say and none of them answered — which is also
/// the honest verdict on an EMPTY roster, since there is nothing left that could ever answer and a
/// spinner there would never end. A friend's server being off is not a reason to tell someone their
/// own library's search failed, which is why one answer beats any number of failures.
fn state_from(sources: &[Source], asking: bool) -> State {
    if !asking {
        return State::Idle;
    }
    let is_answered = |s: &Source| s.status == Status::Answered;
    if sources.iter().any(|s| s.status == Status::Pending) {
        // something is still out: only an answer with CONTENT is worth showing ahead of it
        let has_items =
            sources.iter().filter(|s| is_answered(s)).any(|s| s.items.iter().any(|v| !v.is_empty()));
        return if has_items { State::Ready } else { State::Searching };
    }
    if sources.iter().any(is_answered) {
        return State::Ready;
    }
    State::Failed
}

/// One fetch per source at a time, and only once the query has settled. Re-entered every frame by
/// [`pump`], which is what makes the failure path self-healing: a refused `spawn_small` (the
/// device's thread ceiling) or a transient network error simply retries after the backoff instead
/// of latching the screen on a spinner forever.
fn maybe_spawn(i: usize) {
    if unsafe { *addr_of!(ARMED) } {
        return; // still settling — the keystroke burst is not over
    }
    // ONE borrow of this source's record, since the backoff and the status are now one thing. The
    // whole array is taken first and indexed after, for `live_sources`' reason.
    let all: &'static [Source; NSRC] = unsafe { &*addr_of!(SRC) };
    let src = &all[i];
    if IN_FLIGHT[i].load(Ordering::SeqCst) || src.retry_cd > 0 {
        return;
    }
    if src.status == Status::Answered {
        return; // this source has had its say about this query
    }
    let Some(q) = terms(query()) else { return };
    let q = q.to_string();
    // `sid` is captured HERE, on the main thread, and resolved through `client_for` on the worker.
    // A worker that read `client()` would search whichever server the user had wandered off to by
    // the time it was scheduled, and file the answers under this slot's id.
    let sid = ServerId::from_raw(i as u16);
    let gen = GEN.load(Ordering::SeqCst);
    IN_FLIGHT[i].store(true, Ordering::SeqCst);
    crate::log(&format!("search: q='{q}' sid={i} asking limit={LIMIT}"));
    let spawned = crate::task::spawn_small("search", move || {
        // the mailbox is filled OUTSIDE the guard so a panicking fetch still lands — as a FAILURE
        // (None), not as an answer of "this server has nothing"
        let what = catch_unwind(|| {
            // sectionId 0 = every section, which `opt_int` sends by omitting it. The Search screen
            // is deliberately account-wide: `sectionId` only RANKS (measured — every other
            // section's rows still come back), so it could not scope this even if we wanted it to.
            let mc = crate::plex::client_for(sid)?.search(&q, LIMIT, 0)?;
            Some(project(&mc, sid))
        })
        .unwrap_or(None);
        land(i, gen, what);
    });
    if !spawned {
        // nothing will ever fill the mailbox, and the claim is cleared only by a take — release it
        // here or this source never searches again. `maybe_spawn` runs every frame, so this retries
        // by itself.
        IN_FLIGHT[i].store(false, Ordering::SeqCst);
    }
}

/// WORKER THREAD: one server's `/hubs/search` response, projected into [`KINDS`] order.
///
/// A search response carries EVERY hub type the server knows about — 17 of them on this set, most
/// empty — so anything that is not one of ours is dropped. `actor` and `director` both land on the
/// Person shelf, in hub order, which is the ONE place the "one shelf, two hubs" rule of
/// [`Kind::hubs`] is actually applied.
fn project(mc: &crate::plex::MediaContainer, sid: ServerId) -> Projection {
    let mut out: Projection = Default::default();
    for hub in &mc.hub {
        let Some(k) = kind_index(hub) else { continue };
        // Both containers, because a hub answers in one or the other and which one is per hub TYPE,
        // not per response (`Hub::directory`). Walking both is how this stops being a thing to
        // remember.
        for m in &hub.metadata {
            out[k].push(Item::Media(parse_item(m, sid)));
        }
        for t in &hub.directory {
            let hit = tag_hit(t, sid);
            // **A tag arrives once per LIBRARY SECTION**, measured: a person in both the Movies and
            // the TV Shows library comes back as two rows with the same `id` and `tagKey`, each
            // carrying that section's own `count`. Drawn raw that is the same face twice; keeping
            // only the first reports 5 credits for someone with 8.
            //
            // So fold on identity and SUM the counts. `tagKey` first because it is global — the
            // same person on two SERVERS is also one person, and the round-robin merge will bring
            // both here — with the local `id` as the fallback for a row that carries no guid, which
            // a collection never does.
            match out[k].iter_mut().find_map(|i| match i {
                Item::Tag(e) if same_tag(e, &hit) => Some(e),
                _ => None,
            }) {
                Some(e) => {
                    e.count += hit.count;
                    // whichever row happened to carry artwork wins; a section that has none
                    // must not blank a face the other section supplied
                    if e.thumb.is_empty() {
                        e.thumb = hit.thumb;
                    }
                }
                None => out[k].push(Item::Tag(hit)),
            }
        }
    }
    out
}

/// Are these two rows the same tag? `tagKey` is plex.tv's and global; `id` is server-local and
/// dense from 1, so it may only be compared **within one server** — two servers' id 921 are two
/// different people, and folding on it across sources would merge strangers.
fn same_tag(a: &TagHit, b: &TagHit) -> bool {
    if !a.tag_key.is_empty() && !b.tag_key.is_empty() {
        return a.tag_key == b.tag_key;
    }
    // The id fallback carries the NAME with it, because a local tag id is not unique within a
    // server: this module's own fixture has "Wallace Shawn" and "Dee Wallace" both at id 921, in
    // different sections. On the `tagKey`-less path that bare comparison folded two strangers into
    // one row — one of them losing their name and both their counts summed — which is worse than
    // the duplicate it was there to prevent.
    a.sid == b.sid && !a.id.is_empty() && a.id == b.id && a.name == b.name
}

/// Which shelf a hub feeds, or `None` for one this screen does not draw (`album`, `artist`,
/// `track`, `playlist`, `tag`, …).
///
/// Matched on `hubIdentifier` — the stable, locale-independent name — with `type` as a fallback:
/// on `/hubs/search` this server sets both to the same slug (as does `plex-openapi.json`'s own
/// worked example), so the fallback costs nothing and covers a server that prefixes one of them.
fn kind_index(hub: &crate::plex::Hub) -> Option<usize> {
    KINDS.iter().position(|k| {
        k.hubs().iter().any(|h| *h == hub.hub_identifier || *h == hub.kind)
    })
}

/// WORKER THREAD: one `Directory[]` entry as a search hit.
///
/// The numeric id is carried as a STRING and left empty when the server sent none, because 0 is
/// "absent" on the wire (`Tag::id`) and a literal `"0"` downstream would address a person that does
/// not exist — the same trap `Tag::is_person` documents from the other side.
fn tag_hit(t: &crate::plex::Tag, sid: ServerId) -> TagHit {
    TagHit {
        sid,
        name: t.tag.clone(),
        tag_key: t.tag_key.clone(),
        id: if t.id != 0 { t.id.to_string() } else { String::new() },
        thumb: t.thumb.clone(),
        key: t.key.clone(),
        count: t.count,
    }
}

/// Drop everything — the account changed, so both the query and the results belong to someone
/// else. Called beside `browse::reset()`.
pub(crate) fn reset() {
    supersede();
    unsafe {
        (*addr_of_mut!(QUERY)).clear();
        (*addr_of_mut!(SHELVES)).clear();
        *addr_of_mut!(STATE) = State::Idle;
        *addr_of_mut!(SETTLE) = 0.0;
        *addr_of_mut!(ARMED) = false;
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::{Hub, MediaContainer, Metadata, Tag};

    /// Take the crate-wide serialization lock, empty the server registry AND this store, so each
    /// test starts from a known table and leaves one behind. `route.rs`'s `fresh_registry`, plus
    /// the store's own globals — the two move together here because [`nsrc`] reads the registry.
    fn fresh() -> std::sync::MutexGuard<'static, ()> {
        let g = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        reset();
        g
    }

    /// Park every source's fetch, so no host test spawns a worker: one would dial a `Client` whose
    /// port belongs to nobody, and a stray background thread also perturbs the process-wide fd
    /// count `stream.rs`'s tests assert on. Call it before every `pump()`.
    fn hold_off() {
        unsafe {
            for s in &mut *addr_of_mut!(SRC) {
                s.retry_cd = RETRY_FRAMES;
            }
            *addr_of_mut!(ARMED) = false;
        }
    }

    /// A registered loopback slot. The port is never dialled — `hold_off` parks every spawn — so
    /// this exists only to give [`nsrc`] a roster to fan out over. `register_for_test`, not the
    /// public `register`: the latter mints and PERSISTS a device uuid.
    fn register(n: usize) {
        for i in 0..n {
            crate::plex::register_for_test(&format!("search-test-{i}"), "127.0.0.1", 1, "tok", "cid-search-test");
        }
        assert_eq!(nsrc(), n);
    }

    fn hub(id: &str, kind: &str) -> Hub {
        Hub { hub_identifier: id.to_string(), kind: kind.to_string(), ..Default::default() }
    }
    fn meta(kind: &str, rk: &str, title: &str) -> Metadata {
        Metadata { kind: kind.to_string(), rating_key: rk.to_string(), title: title.to_string(), ..Default::default() }
    }
    fn media(rk: &str) -> Item {
        Item::Media(PmsMovie { rk: rk.to_string(), title: rk.to_string(), ..Default::default() })
    }
    /// A source that answered, holding `items` on shelf `k`.
    fn answered(k: usize, items: Vec<Item>) -> Source {
        let mut s = Source { status: Status::Answered, ..Source::EMPTY };
        s.items[k] = items;
        s
    }
    /// A source whose attempt failed, with no backoff armed — [`hold_off`] is what parks spawns in
    /// these tests, so a fixture must not also decide the retry timing it is being graded on.
    fn failed() -> Source {
        Source { status: Status::Failed, ..Source::EMPTY }
    }
    fn titles(shelf: &Shelf) -> Vec<&str> {
        shelf.items.iter().map(|i| i.title()).collect()
    }

    /// The wire's hub order is NOT the drawn order: the server reranks its hubs per query (`sta`
    /// puts people first, `star` puts films first), and a shelf that moved under the user's focus
    /// mid-word is the bug `KINDS` exists to prevent. Both person hubs feed ONE shelf, and every
    /// hub this screen does not draw is dropped rather than mis-filed.
    #[test]
    fn hubs_map_onto_the_fixed_shelf_order_and_actor_plus_director_are_one_shelf() {
        let mut mc = MediaContainer::default();
        let mut director = hub("director", "director");
        director.directory = vec![Tag { tag: "Nick Park".into(), id: 7, ..Default::default() }];
        let mut actor = hub("actor", "actor");
        actor.directory = vec![Tag { tag: "Peter Sallis".into(), id: 6059, ..Default::default() }];
        let mut movie = hub("movie", "movie");
        movie.metadata = vec![meta("movie", "1971", "A Close Shave")];
        let mut album = hub("album", "album");
        album.metadata = vec![meta("album", "9", "Not A Shelf")];
        // wire order: people first, then a film, then a hub with no shelf at all
        mc.hub = vec![actor, director, movie, album];

        let p = project(&mc, ServerId::UNSET);
        assert_eq!(p[0].len(), 1, "Movies");
        assert!(p[1].is_empty() && p[2].is_empty(), "TV Shows / Episodes");
        assert_eq!(
            p[3].iter().map(|i| i.title()).collect::<Vec<_>>(),
            ["Peter Sallis", "Nick Park"],
            "actor and director are ONE shelf, in hub order"
        );
        assert!(p[4].is_empty(), "Collections");

        // …and the drawn order is KINDS, whatever the wire said
        let sh = merge(&[Source { status: Status::Answered, items: p, ..Source::EMPTY }]);
        assert_eq!(sh.iter().map(|s| s.kind).collect::<Vec<_>>(), [Kind::Movie, Kind::Person]);
    }

    /// The same person on two SERVERS is one person. `project` folds per response, so it
    /// cannot see across sources — [`merge`] is the only place both are in hand, and until it did
    /// this a shared actor drew twice with a split count while `same_tag`'s own doc claimed the
    /// round-robin brought them together.
    #[test]
    fn one_person_on_two_servers_is_one_row_after_the_merge() {
        let mk = |sid: u16, id: &str, thumb: &str, count: i64| {
            let mut p: Projection = Default::default();
            p[3].push(Item::Tag(TagHit {
                sid: ServerId::from_raw(sid),
                name: "Wallace Shawn".into(),
                tag_key: "gid-1".into(),
                id: id.into(),
                thumb: thumb.into(),
                count,
                ..Default::default()
            }));
            Source { status: Status::Answered, items: p, ..Source::EMPTY }
        };
        // Same guid, DIFFERENT local ids (they are server-local) and only one carries a face.
        let sh = merge(&[mk(0, "921", "", 5), mk(1, "4471", "/t.jpg", 3)]);
        let people = &sh.iter().find(|s| s.kind == Kind::Person).expect("a Person shelf").items;
        assert_eq!(people.len(), 1, "one person, not one per server");
        let Item::Tag(t) = &people[0] else { panic!("a person is a Tag row") };
        assert_eq!(t.count, 8, "their credits across both servers");
        assert_eq!(t.thumb, "/t.jpg", "the server with a face supplies it");
    }

    /// **A person in two libraries is one person.** The server answers per library SECTION, so the
    /// same actor comes back twice with the same identity and each section's own `count` — which
    /// drew the same face twice, and would have reported 5 credits for someone with 8 had the
    /// duplicate simply been dropped. Device-observed before it was fixed.
    #[test]
    fn a_tag_that_arrives_once_per_library_section_folds_into_one_row_with_the_counts_summed() {
        let mut mc = MediaContainer::default();
        let mut actor = hub("actor", "actor");
        actor.directory = vec![
            // the Movies section: no artwork on this row
            Tag { tag: "Wallace Shawn".into(), id: 921, tag_key: "gid-1".into(), count: 5, ..Default::default() },
            // …and the TV Shows section, same person, and this is the row with a headshot
            Tag {
                tag: "Wallace Shawn".into(),
                id: 4471,
                tag_key: "gid-1".into(),
                count: 3,
                thumb: "/t.jpg".into(),
                ..Default::default()
            },
            // a different person who happens to share the FIRST one's local id in another section
            Tag { tag: "Dee Wallace".into(), id: 921, tag_key: "gid-2".into(), count: 2, ..Default::default() },
        ];
        mc.hub = vec![actor];

        let p = project(&mc, ServerId::UNSET);
        assert_eq!(
            p[3].iter().map(|i| i.title()).collect::<Vec<_>>(),
            ["Wallace Shawn", "Dee Wallace"],
            "one row per PERSON, and a shared local id does not merge two of them"
        );
        let Item::Tag(first) = &p[3][0] else { panic!("a person is a Tag row") };
        assert_eq!(first.count, 8, "the section counts are summed, not taken from whichever came first");
        assert_eq!(first.thumb, "/t.jpg", "a section with no artwork must not blank a face another supplied");
    }

    /// A hub answers in `Metadata[]` OR `Directory[]` depending on its TYPE (`Hub::directory`), and
    /// the two become different `Item` variants. The numeric tag id is carried as a string and left
    /// EMPTY when the server sent none — a literal "0" would address a person that does not exist.
    #[test]
    fn a_directory_hub_becomes_a_tag_hit_and_a_metadata_hub_a_card() {
        let sid = ServerId::from_raw(3);
        let mut mc = MediaContainer::default();
        let mut people = hub("actor", "actor");
        people.directory = vec![
            Tag {
                tag: "Peter Sallis".into(),
                tag_key: "5d7768268718ba001e311be6".into(),
                id: 6059,
                thumb: "https://metadata-static.plex.tv/x.jpg".into(),
                count: 4,
                ..Default::default()
            },
            // a COLLECTION-shaped entry: no tagKey, no thumb, no numeric id — only a listing key
            Tag { tag: "Aardman".into(), key: "/library/sections/1/all?collection=6068".into(), ..Default::default() },
        ];
        let mut movie = hub("movie", "movie");
        movie.metadata = vec![meta("movie", "1971", "A Close Shave")];
        mc.hub = vec![people, movie];

        let p = project(&mc, sid);
        let Item::Media(m) = &p[0][0] else { panic!("a Metadata[] row must be a card") };
        assert_eq!((m.rk.as_str(), m.sid), ("1971", sid), "the row is stamped with the server that ANSWERED");
        let Item::Tag(t) = &p[3][0] else { panic!("a Directory[] row must be a tag hit") };
        assert_eq!((t.id.as_str(), t.tag_key.as_str(), t.count, t.sid), ("6059", "5d7768268718ba001e311be6", 4, sid));
        let Item::Tag(c) = &p[3][1] else { panic!("a Directory[] row must be a tag hit") };
        assert!(c.id.is_empty(), "id 0 is 'the server sent none', never the person numbered 0");
        assert!(c.tag_key.is_empty() && c.thumb.is_empty());
        assert_eq!(c.key, "/library/sections/1/all?collection=6068", "the only handle a collection gives you");
    }

    /// Sources are taken ROUND ROBIN, so a second server's best match sits second rather than
    /// twenty-fifth — and a source that has not answered (pending) or cannot (failed) contributes
    /// nothing at all instead of a gap.
    #[test]
    fn the_merge_round_robins_every_answered_source_and_skips_the_rest() {
        let sources = [
            answered(0, vec![media("ours-1"), media("ours-2"), media("ours-3")]),
            failed(),
            answered(0, vec![media("theirs-1"), media("theirs-2")]),
            Source::EMPTY, // still pending
        ];
        let sh = merge(&sources);
        assert_eq!(sh.len(), 1, "an empty type draws nothing at all");
        assert_eq!(titles(&sh[0]), ["ours-1", "theirs-1", "ours-2", "theirs-2", "ours-3"]);
    }

    /// Neither a source's own list nor the merged shelf may exceed a `CardRow`'s spring count: past
    /// it `scale(i)` clamps to the last cell, so an over-cap tile would wear its neighbour's pop and
    /// never animate its own.
    #[test]
    fn a_merged_shelf_is_capped_at_the_card_rows_spring_count() {
        let many = |p: &str| (0..SHELF_MAX).map(|i| media(&format!("{p}{i}"))).collect::<Vec<_>>();
        let sh = merge(&[answered(0, many("a")), answered(0, many("b"))]);
        assert_eq!(sh[0].items.len(), SHELF_MAX);
        // …and the cap falls on a ROUND boundary rather than on one source: both are represented
        assert_eq!(titles(&sh[0])[..4], ["a0", "b0", "a1", "b1"]);
    }

    /// `Ready` with nothing in it is an ANSWER and `Failed` is a fault — the distinction the screen
    /// draws two different sentences from. One answer is enough: a friend's server being off must
    /// not tell someone their own library's search failed.
    #[test]
    fn one_answer_is_ready_and_only_an_all_failed_roster_is_failed() {
        let ok = || answered(0, vec![media("x")]);
        let bad = failed;
        assert_eq!(state_from(&[ok(), bad()], true), State::Ready, "a dead source must not fail the others");
        assert_eq!(state_from(&[bad(), Source::EMPTY], true), State::Searching, "one is still out");
        assert_eq!(state_from(&[bad(), bad()], true), State::Failed);
        assert_eq!(state_from(&[], true), State::Failed, "nothing can ever answer — a spinner would never end");
        assert_eq!(state_from(&[Source::EMPTY], true), State::Searching);
        // a query below MIN_QUERY was never asked, so nothing about it is pending or failed
        assert_eq!(state_from(&[bad(), bad()], false), State::Idle);
        // an answer that is genuinely empty is still an answer, ONCE nobody else is still out —
        // this is the "No results for wallace" screen
        assert_eq!(state_from(&[Source { status: Status::Answered, ..Source::EMPTY }], true), State::Ready);
    }

    /// **"No results" must not be said over a source that has not answered yet.** Our own server
    /// replies empty in 20 ms on the LAN and a friend's takes a second: publishing `Ready` on the
    /// first would print "No results for wallace" and then grow a shelf underneath it. An answer
    /// with CONTENT is the exception — a populated screen is only ever added to, never contradicted,
    /// so those results go up without waiting on the slowest server in the house.
    #[test]
    fn an_empty_answer_waits_for_the_stragglers_but_a_populated_one_does_not() {
        let empty = || Source { status: Status::Answered, ..Source::EMPTY };
        assert_eq!(
            state_from(&[empty(), Source::EMPTY], true),
            State::Searching,
            "'No results' over a server that has not answered yet"
        );
        assert_eq!(
            state_from(&[answered(0, vec![media("A Close Shave")]), Source::EMPTY], true),
            State::Ready,
            "results already in hand must not wait on the slowest server"
        );
        // …and once the straggler has failed, the empty answer IS the whole truth
        assert_eq!(state_from(&[empty(), failed()], true), State::Ready);
    }

    /// [`terms`] is THE predicate — the store's own gate and the one `ui/search/` asks — so it is
    /// graded here rather than at each screen that used to re-spell it. Two properties, both of
    /// which a re-spelling has got wrong: the trim (the FIELD's spaces are not part of what is being
    /// looked for) and the floor counted in CHARACTERS, since a two-letter Cyrillic query is four
    /// bytes and a `len()` test would have let a ONE-letter one through to a server that answers
    /// every hub empty.
    #[test]
    fn terms_trims_and_counts_characters_not_bytes() {
        assert_eq!(terms("wa"), Some("wa"), "exactly MIN_QUERY is a real query");
        assert_eq!(terms("  wallace  "), Some("wallace"));
        assert_eq!(terms("w"), None, "one character costs a round trip and returns nothing");
        assert_eq!(terms(" w "), None, "…and padding it does not make it one");
        assert_eq!(terms("   "), None);
        assert_eq!(terms(""), None);
        assert_eq!(terms("к"), None, "one letter, two bytes — a byte count would have asked");
        assert_eq!(terms("ко"), Some("ко"));
    }

    /// The query state machine: below `MIN_QUERY` nothing is asked at all, at it the screen goes
    /// pending, and a new query drops the old answer at once rather than leaving results for a
    /// string that is no longer on screen.
    #[test]
    fn set_query_gates_on_min_query_and_drops_the_previous_answer() {
        let _g = fresh();
        set_query("w");
        assert_eq!(state(), State::Idle, "one character returns every hub empty — asking is pure latency");
        assert!(!settling(), "and nothing is owed a fetch");

        set_query("wa");
        assert_eq!(state(), State::Searching);
        assert!(settling());
        assert_eq!(query(), "wa");

        // seed an answer the honest way, then type on: it must not survive into the next query
        unsafe { (*addr_of_mut!(SRC))[0] = answered(0, vec![media("A Close Shave")]) };
        rebuild(1);
        assert_eq!(shelves().len(), 1);
        set_query("wal");
        assert!(shelves().is_empty(), "results for a string that is no longer on screen");
        assert_eq!(state(), State::Searching);
        assert_eq!(unsafe { (*addr_of!(SRC))[0].status }, Status::Pending, "every source is asked again");

        // …and back below the floor is Idle again, not a search that never answers
        set_query("w");
        assert_eq!(state(), State::Idle);
        assert!(!settling());
        reset();
    }

    /// The field draws the query VERBATIM, but the server is asked the trimmed one — so pressing
    /// the space bar must repaint without superseding an answer that is still correct. Getting this
    /// wrong re-asks the whole roster for identical terms on every trailing keystroke.
    #[test]
    fn a_trailing_space_repaints_but_does_not_re_ask() {
        let _g = fresh();
        set_query("wallace");
        unsafe { (*addr_of_mut!(SRC))[0] = answered(0, vec![media("A Close Shave")]) };
        rebuild(1);
        let gen = GEN.load(Ordering::SeqCst);

        set_query("wallace ");
        assert_eq!(query(), "wallace ", "the FIELD draws what was typed");
        assert_eq!(GEN.load(Ordering::SeqCst), gen, "the same terms are not a new search");
        assert_eq!(shelves().len(), 1, "the answer is still correct — it must not be dropped");

        set_query("wallace g");
        assert_ne!(GEN.load(Ordering::SeqCst), gen, "different terms ARE a new search");
        assert!(shelves().is_empty());
        reset();
    }

    /// Typing coalesces into ONE fetch: the accumulator restarts on every keystroke and only
    /// releases the fetch once the query has held still for `SETTLE_S`. Without it a five-letter
    /// word costs four round trips per source.
    #[test]
    fn the_debounce_coalesces_a_burst_of_keystrokes_into_one_fetch() {
        let _g = fresh();
        set_query("wa");
        assert!(settling());
        pump(SETTLE_S * 0.6);
        assert!(settling(), "still inside the settle window");
        set_query("wal"); // another keystroke restarts it
        pump(SETTLE_S * 0.6);
        assert!(settling(), "the fetch is owed to the LAST keystroke, not the first of the burst");
        pump(SETTLE_S * 0.6);
        assert!(!settling(), "the query held still — now it may be asked");
        // and it stays released: the accumulator must not re-arm itself frame after frame
        pump(0.016);
        assert!(!settling());
        reset();
    }

    /// A landing for the query you have already typed past must not repopulate the one on screen —
    /// and it must still release the single-flight claim while being dropped, or that source can
    /// never search again.
    #[test]
    fn a_landing_from_the_previous_query_is_discarded_but_still_releases_the_fetch() {
        let _g = fresh();
        register(1);
        set_query("wal");
        let stale = GEN.load(Ordering::SeqCst);
        set_query("wallace"); // supersedes: the fetch above is now about a string nobody typed

        IN_FLIGHT[0].store(true, Ordering::SeqCst);
        hold_off();
        land(0, stale, Some(answered(0, vec![media("Wallander")]).items));
        assert!(!pump(0.0), "a superseded landing must not publish");
        assert!(shelves().is_empty(), "the previous query's results leaked in");
        assert_eq!(state(), State::Searching, "a discarded landing must not settle the spinner");
        assert!(
            !IN_FLIGHT[0].load(Ordering::SeqCst),
            "the take must release the single-flight even for a landing it drops"
        );
        // …and it must not arm a backoff either, which would delay the CURRENT query's first answer
        assert_eq!(unsafe { (*addr_of!(SRC))[0].retry_cd }, RETRY_FRAMES - 1, "only the sentinel ticked");

        let fresh_gen = GEN.load(Ordering::SeqCst);
        land(0, fresh_gen, Some(answered(0, vec![media("A Close Shave")]).items));
        hold_off();
        assert!(pump(0.0), "the landing for the query ON SCREEN is a change the screen must see");
        assert_eq!(titles(&shelves()[0]), ["A Close Shave"]);
        assert_eq!(state(), State::Ready);
        crate::plex::reset_servers_for_test();
        reset();
    }

    /// A dead source arms its OWN backoff and contributes nothing; the source beside it still
    /// answers, and the screen reads `Ready` rather than failed. The other half of "release the
    /// claim on the take": a failure must not latch the source either.
    #[test]
    fn a_failed_source_backs_off_alone_and_the_others_still_answer() {
        let _g = fresh();
        register(2);
        set_query("wallace");
        let gen = GEN.load(Ordering::SeqCst);

        IN_FLIGHT[0].store(true, Ordering::SeqCst);
        IN_FLIGHT[1].store(true, Ordering::SeqCst);
        land(0, gen, Some(answered(0, vec![media("A Close Shave")]).items));
        land(1, gen, None); // the friend's server is off
        hold_off();
        assert!(pump(0.0));

        assert_eq!(titles(&shelves()[0]), ["A Close Shave"], "a dead source must not blank a live one");
        assert_eq!(state(), State::Ready, "one answer is enough");
        assert_eq!(unsafe { (*addr_of!(SRC))[1].status }, Status::Failed);
        assert_eq!(
            unsafe { (*addr_of!(SRC))[1].retry_cd },
            RETRY_FRAMES,
            "the failed source backs off before retrying"
        );
        assert!(!IN_FLIGHT[0].load(Ordering::SeqCst) && !IN_FLIGHT[1].load(Ordering::SeqCst));
        crate::plex::reset_servers_for_test();
        reset();
    }

    /// The duplicate-spawn race [`IN_FLIGHT`] admits to can leave two workers out for one source at
    /// one generation. If the loser's failure arrived after the winner's answer, a plain
    /// `Status::Failed` would drop that source's already-drawn results out of the merge and back off
    /// for two seconds — over an error about a request whose answer is already on screen.
    #[test]
    fn a_late_failure_cannot_unsay_an_answer_this_source_already_gave() {
        let _g = fresh();
        register(1);
        set_query("wallace");
        let gen = GEN.load(Ordering::SeqCst);
        land(0, gen, Some(answered(0, vec![media("A Close Shave")]).items));
        hold_off();
        assert!(pump(0.0));
        assert_eq!(state(), State::Ready);

        land(0, gen, None); // the duplicate worker, finishing second
        hold_off();
        pump(0.0);
        assert_eq!(titles(&shelves()[0]), ["A Close Shave"], "the results vanished for two seconds");
        assert_eq!(state(), State::Ready);
        assert_eq!(unsafe { (*addr_of!(SRC))[0].status }, Status::Answered);
        assert_eq!(unsafe { (*addr_of!(SRC))[0].retry_cd }, RETRY_FRAMES - 1, "no backoff — only the sentinel ticked");
        crate::plex::reset_servers_for_test();
        reset();
    }

    /// A screen that can never be answered must not spin forever. With no server registered nothing
    /// can land, so a `State` recomputed only on a landing would sit on `Searching` for good — and
    /// this is reachable: `/tmp/plxnative-search=<q>` forces the route whether or not a server was
    /// ever installed.
    #[test]
    fn an_empty_roster_settles_instead_of_spinning_forever() {
        let _g = fresh();
        set_query("wallace");
        assert_eq!(state(), State::Searching, "the query starts out pending, as typed");
        assert_eq!(nsrc(), 0, "no server can ever answer it");
        assert!(pump(SETTLE_S * 2.0), "the verdict changed, so the screen must repaint");
        assert_eq!(state(), State::Failed);
        assert!(!pump(0.016), "…and it is not news a second time");
        reset();
    }

    /// `reset` (an account switch) and `supersede` (a keystroke) both drop the mailboxes — and a
    /// single-flight claim is cleared ONLY by a take, so they must clear every one themselves or
    /// the next query never fetches. The `browse.rs` latch, once per source.
    #[test]
    fn reset_clears_every_claim_backoff_and_answer() {
        let _g = fresh();
        set_query("wallace");
        for i in 0..NSRC {
            IN_FLIGHT[i].store(true, Ordering::SeqCst);
            unsafe {
                let s = &mut (*addr_of_mut!(SRC))[i];
                *s = answered(0, vec![media("A Close Shave")]);
                s.retry_cd = RETRY_FRAMES;
            }
        }

        reset();

        for i in 0..NSRC {
            assert!(!IN_FLIGHT[i].load(Ordering::SeqCst), "source {i} stayed latched — the screen wedges");
            assert_eq!(unsafe { (*addr_of!(SRC))[i].retry_cd }, 0);
            assert_eq!(unsafe { (*addr_of!(SRC))[i].status }, Status::Pending, "source {i} kept the last account's answer");
        }
        assert!(query().is_empty() && shelves().is_empty());
        assert_eq!(state(), State::Idle);
        assert!(!settling());
    }

    /// The count read-out and the heading, which are the only strings this store hands the screen.
    /// The two counts are different questions and a Collections shelf answers both at once: three
    /// collections found ("3 results"), one of which holds twelve films ("12 items").
    #[test]
    fn a_person_shelf_counts_people_and_everything_else_counts_results() {
        assert_eq!((Kind::Person.title(), Kind::Person.count_word(1)), ("Cast & Crew", "person"));
        assert_eq!(Kind::Person.count_word(2), "people");
        assert_eq!(Kind::Movie.count_word(1), "result");
        assert_eq!(Kind::Collection.count_word(0), "results");

        assert_eq!((Kind::Collection.count_word(3), items_word(12)), ("results", "items"));
        assert_eq!(items_word(1), "item");
        // 0 is plural ("0 items"), and so is a nonsense negative the wire could still send
        assert_eq!((items_word(0), items_word(-1)), ("items", "items"));
    }
}

/// TEST ONLY: is the debounce still holding a keystroke back? The accumulator is otherwise
/// invisible — its only effect is that `maybe_spawn` declines — and a debounce that silently stops
/// releasing is a screen that never searches.
#[cfg(test)]
fn settling() -> bool {
    unsafe { *addr_of!(ARMED) }
}
