//! search — the search screen's data layer (the store the owned `screens::search::SearchScreen` draws).
//!
//! One query, fanned out across every registered source, merged into typed shelves and pumped once
//! a frame while the screen is up. The types down to [`shelves`] are what `screens::search::render`
//! draws;
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
//! shelf is genuinely mixed (the deleted legacy `ui/search/results.rs`'s own reasoning, carried
//! onto the owned `screens::search::mod.rs`'s `OWNER_FLOOR`: the owner annotation follows FOCUS,
//! not a fixed shelf owner), so concatenating would bury a friend's best match behind
//! twenty-three worse ones of ours. There is no cross-server relevance score to sort on — the only
//! ranking any server hands over is the order of its own list — so taking one from each in turn is
//! the most each server's ranking can be honoured at once.
//!
//! A source that fails contributes nothing and **fails nobody else**: each has its own in-flight
//! claim, its own retry backoff and its own mailbox, and [`State`] is `Failed` only when every one
//! of them is.
//!
//! ## Favourites RANK here; they never filter
//!
//! The **Favorite libraries** switch governs every other browsing surface — Home's shelves, which
//! type pills the top strip draws, the Library's Sources picker — and this screen is the deliberate
//! exception. It stays **grant-scoped**: every library the account may reach is searched, and the
//! favourites only decide the ORDER ([`merge`], favourites first, each pass stable so the server's
//! own ranking survives inside it).
//!
//! The reason is that a favourite is a browsing preference, not an authorization boundary. Removing
//! a non-favourite library's hits turns "I don't browse this often" into "this does not exist": a
//! user searching by name for a film they own and can play gets nothing, with no explanation on
//! screen and no control anywhere that obviously undoes it. Plex's own Favorite Libraries feature
//! makes the same split.
//!
//! **Counts stay grant-wide**, and that is a second decision rather than a consequence: "12 films"
//! must keep meaning every film the user can play, or the count becomes a quieter version of the
//! same false negative. Two edges make it harder than it sounds and both are pinned by tests — the
//! visible cap must not truncate the FOLD (the cap bounds what is DRAWN, not what is COUNTED), and
//! a tag straddling a favourite and a non-favourite library ranks favourite with its whole count.
//!
//! The favourite table is a SNAPSHOT taken at the spawn site and watched by a second generation
//! ([`FAV_GEN`]) beside the roster's — see that constant for why a change re-arms the query rather
//! than re-sorting what is on screen.
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
use std::sync::{Arc, Mutex};

pub(crate) mod view;
pub(crate) mod recents;
pub(crate) mod scope;

/// Below this many characters the server answers with nothing, so asking is pure latency.
/// Measured, not guessed — see the module doc.
pub(crate) const MIN_QUERY: usize = 2;

/// The typed shelves, in the order they are drawn — **fixed**, never the server's ranking.
/// `ui_kits/tv-app/SearchScreen.jsx`: "Results are ranked inside a shelf, never across them."
pub(crate) const KINDS: [Kind; 5] = [
    Kind::Movie,
    Kind::Show,
    Kind::Episode,
    Kind::Person,
    Kind::Collection,
];

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
    /// **Does any FAVOURITE library contribute to this tag?** Ranking only — it never removes a
    /// row (§6: a browsing preference is not an authorization boundary), and it never touches
    /// [`TagHit::count`], which stays grant-wide because "12 films" must keep meaning every film
    /// the user can play.
    ///
    /// It has to be carried rather than derived, and that is the whole reason this field exists:
    /// the wire's `Tag` has a `library_section_id` ([`crate::plex::Tag`]) and this projection drops
    /// it, because a tag arrives once per SECTION and the two folds below — per response, then
    /// across servers — sum those rows into one. After the fold there is no section left to ask
    /// about. So the bit is attached before the first fold and **OR'd** at both: a person in one
    /// favourite library and one non-favourite is favourite-ranked, which is the honest answer.
    pub(crate) fav: bool,
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

    /// **Is this hit from a favourite library?** — the ranking key, and the two variants answer it
    /// from different places for a reason worth knowing before "simplifying" it.
    ///
    /// A MEDIA hit keeps its own `librarySectionID` (`PmsMovie::sec`) all the way through, and
    /// nothing folds two of them together, so the bit is derivable at merge time from the same
    /// snapshot Home joins against. A TAG's section is destroyed by the fold, so its bit is
    /// attached upstream in [`tag_hit`] and carried — see [`TagHit::fav`].
    ///
    /// Unknown ranks as a FAVOURITE, in both directions (`sec == 0`, or a library the section table
    /// has not enumerated yet), which is [`crate::pms`]'s rule at the same join and for the same
    /// reason: demoting what we cannot classify would push a user's own results down the shelf on
    /// the frame the app boots.
    pub(crate) fn is_fav(&self, favs: &[(ServerId, i64, bool)]) -> bool {
        match self {
            Item::Tag(t) => t.fav,
            Item::Media(m) => section_is_fav(favs, m.sid, m.sec),
        }
    }
}

/// The one join: is `(sid, key)` a favourite library? `key == 0` is the server saying nothing about
/// the row's library and an absent row is a library nobody has enumerated yet — both rank as
/// favourite, for the reason [`Item::is_fav`] gives.
pub(crate) fn section_is_fav(favs: &[(ServerId, i64, bool)], sid: ServerId, key: i64) -> bool {
    if key == 0 {
        return true;
    }
    match favs.iter().find(|(s, k, _)| *s == sid && *k == key) {
        Some((_, _, fav)) => *fav,
        None => true,
    }
}

#[derive(Clone)]
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

// Published buffers are shared with retained frame views. Only the main-thread store writes
// them; replacing a query or landing cannot invalidate a frame that still owns the old Arc.
static mut QUERY: Option<Arc<str>> = None;
static mut SHELVES: Option<Arc<Vec<Shelf>>> = None;
static mut STATE: State = State::Idle;

/// The query as typed, verbatim — trailing space and all, because the FIELD draws this.
pub(crate) fn query() -> &'static str {
    unsafe { (&*addr_of!(QUERY)).as_deref().unwrap_or("") }
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
/// `pub(crate)` so the owned `screens::search::mod.rs` asks instead of re-deriving it. "Is a search on screen", "may this
/// term be filed in the recents", and "is a fetch owed" are the SAME question, and a screen that
/// spells its own copy of the test can disagree with the store about whether a search is happening
/// at all — a results region drawn over a store parked on [`State::Idle`], which never asked
/// anything and so will never answer.
pub(crate) fn terms(q: &str) -> Option<&str> {
    let t = q.trim();
    (t.chars().count() >= MIN_QUERY).then_some(t)
}

/// The query's own drawability invariant: no control byte reaches the store. **NUL is the one
/// that bites** — it survives every `String` operation on the way in from a boot trigger's file
/// and `CString::new` refuses it at the far end, so the field's run and the empty statement both
/// blank over a query the app still believes it is holding. Borrowed when there is nothing to
/// remove, which is every keystroke: the television's panel cannot commit one
/// (`textinput::decode_text_at` ends its string AT the NUL) and a remembered term is refused by
/// `recents::usable`, so the seed path is the only way one gets in — and that path now enters
/// through the STORE rather than through a screen's mount.
pub(crate) fn sanitize_query(q: &str) -> std::borrow::Cow<'_, str> {
    if q.chars().any(char::is_control) {
        std::borrow::Cow::Owned(q.chars().filter(|c| !c.is_control()).collect())
    } else {
        std::borrow::Cow::Borrowed(q)
    }
}

/// Replace the query. Idempotent on an unchanged string, so a caller may hand it every frame.
fn set_query(q: &str) {
    set_query_with_directory(q, None);
}

fn set_query_from_directory(q: &str, directory: crate::stores::browse::DirectoryView<'_>) {
    set_query_with_directory(q, Some(directory));
}

fn set_query_with_directory(
    q: &str,
    directory: Option<crate::stores::browse::DirectoryView<'_>>,
) {
    let q = &*sanitize_query(q);
    // Two different changes, and only one of them is news for the SERVER: the field draws the raw
    // string (so a typed space must repaint), while the fetch is addressed to the trimmed one (so
    // that same space must not supersede an answer that is still correct). Collapsing the two
    // re-asks the whole roster for the identical terms every time the space bar is pressed.
    // Finish reading the caller's string before replacing storage: even a caller deriving
    // a shorter query from the legacy read facade must not leave us reading a retired buffer.
    let real_query = terms(q).is_some();
    let restart = unsafe {
        let cur = &mut *addr_of_mut!(QUERY);
        let old = cur.as_deref().unwrap_or("");
        if old == q {
            return;
        }
        let restart = old.trim() != q.trim();
        *cur = Some(Arc::from(q));
        restart
    };
    if restart {
        // A new query invalidates the old answer immediately. Leaving the previous shelves up
        // while the next lands would show results for a string that is no longer on screen.
        match directory {
            Some(directory) => supersede_from_directory(directory),
            None => supersede(),
        }
        unsafe {
            *addr_of_mut!(SHELVES) = None;
            *addr_of_mut!(STATE) = if real_query {
                State::Searching
            } else {
                State::Idle
            };
            // …and the debounce restarts with it: the fetch is owed to the LAST keystroke, not to
            // the first one of the burst.
            *addr_of_mut!(SETTLE_US) = 0;
            *addr_of_mut!(ARMED) = real_query;
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

/// Publish a bounded catalog through the real retained-view boundary, without network work.
#[cfg(test)]
pub(crate) fn publish_shelves_for_test(shelves: Vec<Shelf>) {
    crate::testlock::assert_held("the search store (publish_shelves_for_test)");
    // A published catalog represents completed source answers, not merely painted rows over
    // still-pending requests. Keep it valid when a real owned-screen Tick pumps the store.
    VISIBLE.store(crate::plex::server_roster_gen(), Ordering::SeqCst);
    snapshot_favs();
    for i in slots() {
        let items = std::array::from_fn(|k| shelves.iter().filter(|s| s.kind == KINDS[k])
            .flat_map(|s| &s.items).filter(|item| item.sid().raw() as usize == i).cloned().collect());
        record(i, Some(items));
    }
    unsafe {
        *addr_of_mut!(ARMED) = false;
        *addr_of_mut!(SHELVES) = Some(Arc::new(shelves));
        *addr_of_mut!(STATE) = State::Ready;
    }
}

/// The shelves, already in [`KINDS`] order, with empty ones omitted — an empty type draws nothing
/// at all, so the UI never has to test for it.
pub(crate) fn shelves() -> &'static [Shelf] {
    unsafe { (&*addr_of!(SHELVES)).as_deref().map(Vec::as_slice).unwrap_or(&[]) }
}

/// Flip `(sid, rk)`'s watched state in the result set — the optimistic half of a view-state write,
/// for the Search screen's own tiles. `pms::edit_item`'s twin, for `browse::set_watched_local`'s
/// reason: that one reaches the HOME hubs alone, so a film marked watched from a search result's
/// context menu kept its old mark until a refetch.
///
/// **Both stores, and no `rebuild`.** A source owns its rows and [`SHELVES`] is a projection of
/// them ([`merge`]), so an edit to one alone would be undone by the next landing — but re-running
/// the merge here would re-clone every row and log a line for a press that changed one boolean.
/// Editing both by the same rule is the same result at the same cost as the walk itself.
///
/// Returns whether anything matched. **MAIN THREAD.**
fn set_watched_local(sid: ServerId, rk: &str, on: bool) -> bool {
    let mut hit = false;
    let mut flip = |it: &mut Item| {
        if let Item::Media(m) = it {
            if crate::plex::same_item((m.sid, &m.rk), (sid, rk)) {
                crate::pms::set_watched(m, on);
                hit = true;
            }
        }
    };
    for it in unsafe { (&mut *addr_of_mut!(SRC)).iter_mut() }
        .flat_map(|s| s.items.iter_mut())
        .flatten()
    {
        flip(it);
    }
    if let Some(shelves) = unsafe { &mut *addr_of_mut!(SHELVES) } {
        // A write outside the visible result cap must not clone a retained catalog it cannot
        // change. The catalog itself is bounded by KINDS × SHELF_MAX, never library-sized.
        if shelves.iter().flat_map(|s| &s.items).any(|it| matches!(it,
            Item::Media(m) if crate::plex::same_item((m.sid, &m.rk), (sid, rk)))) {
            for it in Arc::make_mut(shelves).iter_mut().flat_map(|s| &mut s.items) { flip(it); }
        }
    }
    hit
}

// ---- fetch plumbing (debounce + generation + single-flight + mailbox + retry backoff) ----------

/// How long the query must hold still before it is asked. `ui/detail.rs`'s `SEASON_SETTLE` is 0.2 s
/// for a D-pad step; typing on the television's own keyboard arrives faster than that and in
/// bursts, so this is a shade longer — a five-letter word costs ONE round trip rather than four.
/// It is the whole reason the "called as the user types" endpoint is affordable at all.
const SETTLE_S: f32 = 0.25;

/// [`SETTLE_S`] in whole microseconds — the unit [`SETTLE_US`] actually accumulates in, so the
/// debounce is an exact integer comparison rather than a summed `f32`.
const SETTLE_US_TARGET: u32 = (SETTLE_S * 1_000_000.0) as u32;

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

/// Registry identity generation last observed by the frame-loop store. This catches sparse
/// membership changes and same-slot re-points alike; both supersede cached answers and workers
/// aimed at the previous origin/profile.
static VISIBLE: AtomicU32 = AtomicU32::new(0);

/// **The favourite table this result set was projected under**, taken once per query at the spawn
/// site (`plex/CLAUDE.md` rule 5) and read by [`rebuild`]'s merge. It ranks; it never filters.
static FAVS: Mutex<Vec<(ServerId, i64, bool)>> = Mutex::new(Vec::new());

/// The retained Browse directory's section generation as of that snapshot — the cheap half of the
/// second identity this store watches beside [`VISIBLE`]. [`pump_with_optional_directory`] also
/// compares the exact favourite table: generations are owner-local and two Browse stores can both
/// be at zero while describing different libraries.
///
/// It is genuinely needed. Search watched the ROSTER generation alone, and the favourite answer
/// moves without the roster moving at all: discovery appends a library and `apply_pins` records an
/// edit, both bumping `SECTIONS_GEN` — and this screen deliberately runs discovery immediately
/// before its own pump, so the answer really can change under a landed result set.
///
/// A change **supersedes and re-arms the whole resident query** rather than re-sorting what is on
/// screen, and "re-sort" was never available: after the fold a [`TagHit`] no longer carries the
/// section its bit came from, so a stored bit cannot be re-derived locally. The alternative is
/// retaining full contributing-section provenance through both folds; re-arming is cheaper.
static FAV_GEN: AtomicU32 = AtomicU32::new(0);

/// Standalone fixture scope. Production always supplies the owning Bridge's retained directory;
/// tests that exercise Search in isolation have no Browse favourites by construction.
fn snapshot_favs() {
    FAV_GEN.store(0, Ordering::SeqCst);
    FAVS.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

fn snapshot_favs_from_directory(directory: crate::stores::browse::DirectoryView<'_>) {
    FAV_GEN.store(directory.sections_gen(), Ordering::SeqCst);
    *FAVS.lock().unwrap_or_else(|e| e.into_inner()) = directory.favorite_sections().to_vec();
}

/// The snapshot, for the merge and for a worker about to be spawned.
fn favs() -> Vec<(ServerId, i64, bool)> {
    FAVS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn favs_match_directory(directory: crate::stores::browse::DirectoryView<'_>) -> bool {
    FAVS.lock().unwrap_or_else(|e| e.into_inner()).as_slice()
        == directory.favorite_sections()
}

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

/// Microseconds the current query has held still, and whether it is still owed a fetch. Main
/// thread only, advanced by [`pump`] — the `season_settle` accumulator's cousin one screen over,
/// but WHOLE MICROSECONDS rather than a summed `f32`: [`pump`]'s `dt` argument already comes from
/// a real `Tick.dt_us` (`stores::search::pump`'s own caller reads it off `parts.tick`), and a
/// per-frame delta that size round-trips through `f32` exactly, so converting it back once and
/// accumulating the integer is what removes the drift `check-deps.sh`'s `dt` gate exists to catch
/// — without this debounce needing a `motion::Ramp` of its own (it is not logical state, not
/// hashed and not replayed; see the retired `ci/allow/dt.txt`'s header for why it was ever this
/// gate's lowest-stakes entry).
static mut SETTLE_US: u32 = 0;
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
    const EMPTY: Source = Source {
        status: Status::Pending,
        retry_cd: 0,
        items: [const { Vec::new() }; NKIND],
    };
}

static mut SRC: [Source; NSRC] = [const { Source::EMPTY }; NSRC];

/// The registry slots this store fans out over — **the one place a raw `ServerId` becomes an index
/// into [`SRC`]/[`SLOT`]/[`IN_FLIGHT`]** (see [`NSRC`]).
///
/// **Exact ids, not a prefix or a range.** Slot
/// numbers are permanent and a sign-out RETIRES the departing account's slots without renumbering
/// what registers after them (`plex::servers`' module doc), so after signing into a second account
/// the live roster is `2..3` and not `0..1`: a prefix would have asked the revoked slots (which
/// resolve to no client at all) and never asked the server the user is actually signed in to.
/// A profile switch can additionally deactivate only the middle slot, so even that post-sign-out
/// window is not necessarily contiguous. Collecting at most 16 indices is the honest shape.
fn slots() -> Vec<usize> {
    crate::plex::server_ids()
        .filter_map(|id| ((id.raw() as usize) < NSRC).then_some(id.raw() as usize))
        .collect()
}

/// How many sources this store fans out over — the width of [`slots`].
fn nsrc() -> usize {
    slots().len()
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
    supersede_with_directory(None);
}

fn supersede_from_directory(directory: crate::stores::browse::DirectoryView<'_>) {
    supersede_with_directory(Some(directory));
}

fn supersede_with_directory(directory: Option<crate::stores::browse::DirectoryView<'_>>) {
    GEN.fetch_add(1, Ordering::SeqCst);
    // A fresh favourite snapshot belongs to the fresh generation, and taking it HERE is what makes
    // the staleness rule need no second mailbox field: a landing projected under the old table
    // carries the old `gen`, and `pump` already discards those. One rejection rule, not two.
    match directory {
        Some(directory) => snapshot_favs_from_directory(directory),
        None => snapshot_favs(),
    }
    for i in 0..NSRC {
        *SLOT[i].lock().unwrap_or_else(|e| e.into_inner()) = None;
        IN_FLIGHT[i].store(false, Ordering::SeqCst);
        unsafe { (*addr_of_mut!(SRC))[i] = Source::EMPTY };
    }
}

/// Test-only compatibility pump for fixtures without a retained directory.
#[cfg(test)]
pub(crate) fn pump(dt: f32) -> bool {
    pump_with_optional_directory(dt, None)
}

/// Advance the debounce and land whatever arrived under this frame's retained directory policy.
/// Returns whether anything changed, so the caller can re-clamp focus.
pub(crate) fn pump_with_directory(
    dt: f32,
    directory: crate::stores::browse::DirectoryView<'_>,
) -> bool {
    pump_with_optional_directory(dt, Some(directory))
}

fn pump_with_optional_directory(
    dt: f32,
    directory: Option<crate::stores::browse::DirectoryView<'_>>,
) -> bool {
    let live = slots();
    let visible = crate::plex::server_roster_gen();
    let roster_changed = VISIBLE.swap(visible, Ordering::SeqCst) != visible;
    if roster_changed {
        // This is an identity boundary, not merely a changed source count. Clear every answer and
        // generation so a slot reactivated for another profile cannot surface rows fetched with
        // the credential it held before it was hidden.
        supersede();
        unsafe { *addr_of_mut!(SHELVES) = None };
    }
    // **The second Browse identity, and it moves without the first.** Discovery appending a library
    // and a favourites edit both bump `SECTIONS_GEN`; an independent owner can instead carry a
    // different exact table at the SAME local generation. This screen runs discovery immediately
    // before its own pump, so the ranking answer really can change under a landed result set.
    //
    // A resident query is SUPERSEDED AND RE-ARMED rather than re-sorted, because re-sorting is not
    // available: after the fold a `TagHit` no longer carries the section its bit came from, so the
    // bit cannot be re-derived locally (see [`FAV_GEN`]). With nothing resident there is nothing to
    // invalidate and the snapshot is simply brought up to date, so the next query does not open by
    // re-arming itself.
    let sections_gen = directory.map_or(0, |directory| directory.sections_gen());
    let favourites_changed = FAV_GEN.load(Ordering::SeqCst) != sections_gen
        || directory.is_some_and(|directory| !favs_match_directory(directory));
    if favourites_changed {
        if terms(query()).is_some() {
            match directory {
                Some(directory) => supersede_from_directory(directory),
                None => supersede(),
            }
            unsafe {
                *addr_of_mut!(SHELVES) = None;
                *addr_of_mut!(SETTLE_US) = 0;
                *addr_of_mut!(ARMED) = true;
            }
        } else {
            match directory {
                Some(directory) => snapshot_favs_from_directory(directory),
                None => snapshot_favs(),
            }
        }
    }
    unsafe {
        if *addr_of!(ARMED) {
            // Realistic per-frame deltas (tens of milliseconds) round-trip through `f32` exactly,
            // so converting once here and accumulating the whole-microsecond integer is exact —
            // see `SETTLE_US`'s doc for why that, and not a summed `f32`, is what this reads.
            let dt_us = (dt * 1_000_000.0).round() as u32;
            let s = &mut *addr_of_mut!(SETTLE_US);
            *s = s.saturating_add(dt_us);
            if *s >= SETTLE_US_TARGET {
                *addr_of_mut!(ARMED) = false;
                if let Some(q) = terms(query()) {
                    crate::log(&format!(
                        "search: q[{}ch] settled, asking {} source(s)",
                        q.chars().count(),
                        nsrc()
                    ));
                }
            }
        }
    }
    let mut landed = false;
    for i in live.iter().copied() {
        unsafe {
            let cd = &mut (*addr_of_mut!(SRC))[i].retry_cd;
            if *cd > 0 {
                *cd -= 1;
            }
        }
        // the landing GATE (§3.3 step 3, `ui::landgate`): under a replay a source's answer is
        // taken on the frame the recording took it on. The debounce above and `maybe_spawn` below
        // are outside it, so the query still goes out when it went out.
        let taken = crate::stores::take_landing(crate::stores::StoreId::Search, || {
            SLOT[i].lock().unwrap_or_else(|e| e.into_inner()).take()
        });
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
        rebuild();
    }
    let sources = live_sources(&live);
    let state = state_from_refs(&sources, terms(query()).is_some());
    let moved = state != self::state();
    if moved {
        crate::log(&format!(
            "search: q[{}ch] state={}",
            query().trim().chars().count(),
            state.name()
        ));
        unsafe { *addr_of_mut!(STATE) = state };
    }
    if !landed && !moved && !roster_changed {
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
            crate::log(&format!(
                "search: q[{}ch] sid={i} late failure ignored — already answered",
                q.chars().count()
            ));
        }
        None => {
            unsafe {
                let s = &mut (*addr_of_mut!(SRC))[i];
                s.status = Status::Failed;
                s.retry_cd = RETRY_FRAMES;
            }
            crate::log(&format!(
                "search: q[{}ch] sid={i} FAILED, retry in {RETRY_FRAMES}f",
                q.chars().count()
            ));
        }
        Some(items) => {
            let counts: Vec<String> = KINDS
                .iter()
                .enumerate()
                .map(|(k, kind)| format!("{}={}", kind.title(), items[k].len()))
                .collect();
            crate::log(&format!(
                "search: q[{}ch] sid={i} hubs {}",
                q.chars().count(),
                counts.join(" ")
            ));
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

/// The LIVE registry slots as exact references into the raw main-thread store. Taking the whole
/// array first avoids an implicit autoref of the raw pointer (`dangerous_implicit_autorefs`).
/// Collecting is necessary because profile visibility may contain holes.
fn live_sources(live: &[usize]) -> Vec<&'static Source> {
    let all: &'static [Source; NSRC] = unsafe { &*addr_of!(SRC) };
    live.iter().map(|&i| &all[i]).collect()
}

/// Recompute the merged shelves from every source's last answer. The [`State`] is NOT set here —
/// [`pump`] recomputes that every frame, landing or no landing (see the note there).
fn rebuild() {
    let live = slots();
    let sources = live_sources(&live);
    let shelves = merge_refs(&sources, &favs());
    let items: usize = shelves.iter().map(|s| s.items.len()).sum();
    crate::log(&format!(
        "search: q[{}ch] shelves={} items={}",
        query().trim().chars().count(),
        shelves.len(),
        items
    ));
    unsafe { *addr_of_mut!(SHELVES) = Some(Arc::new(shelves)) };
}

/// The merged result set: [`KINDS`] order, empty shelves omitted, sources taken **round robin** so
/// no source can bury another (module doc), then **favourite libraries first**. Pure, so both
/// ordering rules are graded on the host rather than inferred from a screenshot with two servers
/// plugged in.
///
/// **Favourites RANK; they never filter.** Removing a non-favourite library's hits would turn "I
/// don't browse this often" into "this does not exist" for a film the user owns and can play, with
/// no explanation on screen — so the whole granted roster is still here and only the order moves.
/// The pass is STABLE, so each server's own ranking survives inside it: the only ranking any server
/// hands over is the order of its own list, and re-sorting within a pass would throw that away.
///
/// **The fold runs to completion BEFORE the cap, and that ordering is load-bearing.** This used to
/// `break` out of the fill the moment the shelf was full, which meant a later duplicate of an
/// ALREADY DISPLAYED tag could no longer augment its count — so a person's "12 films" silently
/// became however many the servers happened to report before the cap was reached. The cap bounds
/// what is DRAWN; it must not bound what is COUNTED.
fn merge(sources: &[Source], favs: &[(ServerId, i64, bool)]) -> Vec<Shelf> {
    let sources: Vec<&Source> = sources.iter().collect();
    merge_refs(&sources, favs)
}

fn merge_refs(sources: &[&Source], favs: &[(ServerId, i64, bool)]) -> Vec<Shelf> {
    let mut out = Vec::new();
    for (k, kind) in KINDS.iter().enumerate() {
        // only a source that ANSWERED contributes: one still pending has nothing to say yet, and
        // one that failed has nothing to say at all
        let live: Vec<&Vec<Item>> = sources
            .iter()
            .filter(|s| s.status == Status::Answered)
            .map(|s| &s.items[k])
            .collect();
        let deepest = live.iter().map(|v| v.len()).max().unwrap_or(0);
        let mut items: Vec<Item> = Vec::new();
        for d in 0..deepest {
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
                        // the SECOND fold, and the bit is OR'd here exactly as in `project`
                        prev.fav |= t.fav;
                        if prev.thumb.is_empty() {
                            prev.thumb = t.thumb.clone();
                        }
                        continue;
                    }
                }
                items.push(it.clone());
            }
        }
        // Favourites first, and `sort_by_key` is STABLE, so this is the two round-robin passes the
        // design asks for expressed once rather than as two loops that would have to agree about
        // folding. Truncation comes last: the favourite pass fills the shelf, and every fold above
        // has already happened, so a tag that IS displayed carries its whole grant-wide count.
        items.sort_by_key(|it| !it.is_fav(favs));
        items.truncate(SHELF_MAX);
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
    let sources: Vec<&Source> = sources.iter().collect();
    state_from_refs(&sources, asking)
}

fn state_from_refs(sources: &[&Source], asking: bool) -> State {
    if !asking {
        return State::Idle;
    }
    let is_answered = |s: &Source| s.status == Status::Answered;
    if sources.iter().any(|s| s.status == Status::Pending) {
        // something is still out: only an answer with CONTENT is worth showing ahead of it
        let has_items = sources
            .iter()
            .filter(|s| is_answered(s))
            .any(|s| s.items.iter().any(|v| !v.is_empty()));
        return if has_items {
            State::Ready
        } else {
            State::Searching
        };
    }
    if sources.iter().any(|s| is_answered(s)) {
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
    // …and so is the favourite table, for the same reason and by the same rule: a worker that asked
    // `browse` what was current would answer with a table from a different moment than the query it
    // was given. `pump` rejects a landing taken under a snapshot that has since moved.
    let favs = favs();
    IN_FLIGHT[i].store(true, Ordering::SeqCst);
    crate::log(&format!(
        "search: q[{}ch] sid={i} asking limit={LIMIT}",
        q.chars().count()
    ));
    let spawned = crate::task::spawn_small("search", move || {
        // the mailbox is filled OUTSIDE the guard so a panicking fetch still lands — as a FAILURE
        // (None), not as an answer of "this server has nothing"
        let what = catch_unwind(|| {
            // sectionId 0 = every section, which `opt_int` sends by omitting it. The Search screen
            // is deliberately account-wide: `sectionId` only RANKS (measured — every other
            // section's rows still come back), so it could not scope this even if we wanted it to.
            let mc = crate::plex::client_for(sid)?.search(&q, LIMIT, 0)?;
            Some(project(&mc, sid, &favs))
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
fn project(
    mc: &crate::plex::MediaContainer,
    sid: ServerId,
    favs: &[(ServerId, i64, bool)],
) -> Projection {
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
            let hit = tag_hit(t, sid, favs);
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
                    // the FIRST of the two folds this bit has to survive — OR, never overwrite, so
                    // a person in one favourite section and one non-favourite is favourite-ranked
                    e.fav |= hit.fav;
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
        k.hubs()
            .iter()
            .any(|h| *h == hub.hub_identifier || *h == hub.kind)
    })
}

/// WORKER THREAD: one `Directory[]` entry as a search hit.
///
/// The numeric id is carried as a STRING and left empty when the server sent none, because 0 is
/// "absent" on the wire (`Tag::id`) and a literal `"0"` downstream would address a person that does
/// not exist — the same trap `Tag::is_person` documents from the other side.
fn tag_hit(t: &crate::plex::Tag, sid: ServerId, favs: &[(ServerId, i64, bool)]) -> TagHit {
    TagHit {
        sid,
        fav: section_is_fav(favs, sid, t.library_section_id),
        name: t.tag.clone(),
        tag_key: t.tag_key.clone(),
        id: if t.id != 0 {
            t.id.to_string()
        } else {
            String::new()
        },
        thumb: t.thumb.clone(),
        key: t.key.clone(),
        count: t.count,
    }
}

/// Drop everything — the account changed, so both the query and the results belong to someone
/// else. Called beside the Browse `BrowseCmd::Reset` command.
fn reset() {
    supersede();
    VISIBLE.store(crate::plex::server_roster_gen(), Ordering::SeqCst);
    unsafe {
        *addr_of_mut!(QUERY) = None;
        *addr_of_mut!(SHELVES) = None;
        *addr_of_mut!(STATE) = State::Idle;
        *addr_of_mut!(SETTLE_US) = 0;
        *addr_of_mut!(ARMED) = false;
    }
}

/// `stores::search`'s one door onto every [`SearchCmd`](crate::stores::search::SearchCmd) (D3):
/// the match used to live in `stores/search.rs::run`, calling `set_query`/`reset`/
/// `set_watched_local` across the module boundary. Relocating it here is what lets those three
/// go private; `RememberRecent`/`ClearRecents`/`SetQueryScoped` still address `search::recents`
/// and `search::scope`, which stay `pub(crate)` (out of this package's scope, per the census).
pub(crate) fn run(cmd: crate::stores::search::SearchCmd) -> bool {
    run_with_optional_directory(cmd, None)
}

pub(crate) fn run_with_directory(
    cmd: crate::stores::search::SearchCmd,
    directory: crate::stores::browse::DirectoryView<'_>,
) -> bool {
    scope::snapshot_with_directory(directory);
    run_with_optional_directory(cmd, Some(directory))
}

fn run_with_optional_directory(
    cmd: crate::stores::search::SearchCmd,
    directory: Option<crate::stores::browse::DirectoryView<'_>>,
) -> bool {
    use crate::stores::search::SearchCmd;
    match cmd {
        SearchCmd::SetQuery(q) => {
            match directory {
                Some(directory) => set_query_from_directory(&q, directory),
                None => set_query(&q),
            }
            true
        }
        SearchCmd::SetQueryScoped { profile_generation, query } => {
            if profile_generation != crate::plex::session::current_gen() { return false; }
            match directory {
                Some(directory) => set_query_from_directory(&query, directory),
                None => set_query(&query),
            }
            true
        }
        SearchCmd::RememberRecent { profile_generation, term } => {
            crate::search::recents::remember(profile_generation, &term)
        }
        SearchCmd::ClearRecents { profile_generation } => {
            crate::search::recents::clear(profile_generation)
        }
        SearchCmd::Reset => {
            reset();
            true
        }
        SearchCmd::SetWatchedLocal { sid, rk, on } => set_watched_local(sid, &rk, on),
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
#[path = "search_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "search_query_debounce_tests.rs"]
mod query_debounce_tests;

#[cfg(test)]
#[path = "search_account_scope_tests.rs"]
mod account_scope_tests;

#[cfg(test)]
#[path = "search_merge_ranking_tests.rs"]
mod merge_ranking_tests;

#[cfg(test)]
#[path = "search_publication_tests.rs"]
mod publication_tests;

/// TEST ONLY: is the debounce still holding a keystroke back? The accumulator is otherwise
/// invisible — its only effect is that `maybe_spawn` declines — and a debounce that silently stops
/// releasing is a screen that never searches.
#[cfg(test)]
pub(crate) fn settling() -> bool {
    unsafe { *addr_of!(ARMED) }
}

#[cfg(test)]
pub(crate) fn debounce_elapsed_for_test() -> f32 {
    crate::testlock::assert_held("the search store (debounce_elapsed_for_test)");
    unsafe { *addr_of!(SETTLE_US) as f32 / 1_000_000.0 }
}
