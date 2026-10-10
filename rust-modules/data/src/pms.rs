//! Plex library fetch/parse into the private catalog (was src/pms.c), read by the UI
//! via the retained publication (`hubs_snapshot()` → `HubsView`) and movie()/hub_item().
//! The fetch + JSON parse go through the typed `plx_plex::plex` client (serde DTOs) — no
//! hand-built paths or `Value` scraping here.
//!
//! **This is the data module `stores::hubs` (`docs/stores-as-machines.md`) is a machine over** —
//! each production `Bridge` owns a `stores::hubs::HubsStore`, whose `run`/`run_with_directory`
//! forward each `HubsCmd` variant into `request_refetch_hubs`/`reset`/`edit_item`/`page`/
//! `cancel_page` here against its own `PmsState`/`Arc<PmsAdapter>`, the same relationship the owned
//! `BrowseStore::run` has to Browse commands against its explicit state. Controlled work captures
//! each immutable request before the adapter launches it. **The cross-module entry points
//! (`run_with_directory`, `land_with_directory`, `tick*`) cannot be scoped narrower than `pub`, and
//! that is a fact about Rust module topology, not an oversight**: `pms` and `stores` are both
//! top-level children of the crate root, so neither is an ancestor of the other, and `pub(in path)`
//! requires `path` to name an ancestor of the ITEM's own module. There is no visibility keyword
//! that means "visible to `stores::hubs` and nobody else" for an item defined here. What IS
//! enforceable, and is: (1) anything with no caller outside this file at all — `hub_state` — is
//! plain private, not `pub`; (2) every mutator `stores::hubs` (or a test) can reach —
//! `request_refetch_hubs`, `edit_item`, `tick`, `apply_landing`, `reset`, and the `_for_test`
//! seeds — asserts `plx_base::testlock::held()` under `#[cfg(test)]` before it touches the
//! crate-wide test-only state those seeds still share (see `lib.rs::testlock` and D5), which is the
//! runtime half of the same contract a compile-time visibility keyword cannot express across two
//! sibling modules. `ci/allow/mutators.txt`'s `# count: 0` already proves no PRODUCTION line
//! outside `pms`/`stores::hubs` spells the old direct-call form; this is the part that
//! keyword-level `pub(super)` genuinely cannot add on top of that count.
use crate::stores::{deck, paging};
use plx_plex::plex::ServerId;
use std::os::raw::c_int;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

pub mod record;
pub mod initial;

/// Rows of Home that hold cards at once: the rows in view plus a margin. Every other row keeps its
/// descriptor (identity, title, key, total, window offset, ledger, the count of cards it last
/// showed) and publishes that many placeholder slots, so every row stays reachable and nothing is
/// left off.
///
/// Bound on held cards: `HOLD_ROWS` x [`MAX_SHELF_ITEMS`] = 16 x 24 = 384, plus the merged
/// Continue Watching row (up to 24) and at most [`HERO_MAX`] retained heroes, however many rows
/// the servers offer. Descriptors are a few hundred bytes each; they are the directory.
pub(crate) const HOLD_ROWS: usize = 16;

/// The card bound Library section hubs still read until they take the same ring (a later step).
/// Home does not: it publishes every row.
pub(crate) const HOME_CARDS_MAX: usize = 2048;

/// Cards one shelf holds at most — the number the grid can address (the owned Home's `MAX_ITEMS`
/// is this constant).
///
/// The MERGED deck reaches it: three sources' Continue Watching is up to 36 cards, and a Recently
/// Added window reaches 24 from one server as the user scrolls (the window replaces its
/// overlapping pages). The home grid's focus ring and its OK dispatch clamp differently past this
/// number — the ring stops at the last addressable card while the press opens whatever column the
/// raw index names. Cap the data and the two can never disagree.
pub const MAX_SHELF_ITEMS: usize = 24;

pub const KIND_COLLECTION: c_int = 4;

pub fn listable(type_str: &str) -> bool {
    matches!(type_str, "movie" | "show" | "season" | "episode")
}

/// Items asked of each hub endpoint, per source. `/hubs?count=` is items-per-hub, so this bounds
/// a shelf, never the number of shelves.
pub(crate) const HUB_FETCH_COUNT: i64 = 12;

/// A catalog row — owned strings (the old C-ABI fixed `[u8; N]` buffers are gone; no C
/// consumer remains). Fields pub so the UI / route / player read them directly.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PmsMovie {
    /// WHICH SERVER this row came from. Every other identity on it — `rk`, `show_rk`, `part` — is a
    /// server-local key that a second server reuses from 1 (docs/shared-servers.md §2 measured the
    /// collision), so the row is only addressable as the PAIR `(sid, rk)`; see
    /// [`plx_plex::plex::same_item`]. Stamped by [`parse_item`] from a value the SPAWNING thread
    /// captured, never from `plex::current_server()` inside the worker — `parse_item` runs on the
    /// hub, page and person workers, and by the time one of them parses, "the current server" may
    /// already be a different machine than the one whose bytes it is holding.
    #[serde(with = "record::server_id")]
    pub sid: ServerId,
    /// The LIBRARY on `sid` this row came from (`librarySectionID`), 0 when the server sent none.
    /// The pin's grain: a whole-server `/hubs` answers with rows from every library, so this is the
    /// only thing that can keep an UNPINNED library's items off Home without a per-library fetch.
    pub sec: i64,
    pub title: String,
    pub year: c_int,
    pub rating: String,
    pub dur_ns: i64,
    pub part: String,
    pub thumb: String,
    /// The item's OWN thumb where [`PmsMovie::thumb`] holds a substitute — i.e. an episode's 16:9
    /// still, empty on everything else. A landscape tile draws this; a portrait card draws `thumb`.
    pub still: String,
    pub art: String,
    pub summary: String,
    pub rk: String,
    pub vcodec: String,
    pub acodec: String,
    #[serde(with = "record::blur_bits")]
    pub blur: [[f32; 3]; 4],
    pub has_blur: bool,
    pub kind: c_int, // 0 = movie, 1 = show, 2 = season, 3 = episode, 4 = collection
    pub resume_ms: i64,  // viewOffset — drives the Continue Watching resume bar
    pub show_rk: String, // parent show rk (episode: grandparent; season: parent)
    pub season_index: c_int, // season number (episode: parentIndex; season: index)
    pub show_title: String, // episode: grandparentTitle; season: parentTitle
    pub ep_index: c_int, // episode only: episode number within the season
    /// Fully unwatched (movie/episode: no viewCount; show/season: zero viewed leaves).
    pub unwatched: bool,
    /// Fully **watched** — and deliberately NOT `!unwatched`, which is the trap this field exists to
    /// close. For a movie or episode the two are the same thing, but for a SHOW or SEASON
    /// `!unwatched` only means "at least one episode has been played", so a series you are three
    /// episodes into satisfies it. The tile mark is a claim of DONE (`ui::widgets::poster_mark`), so
    /// it needs `viewedLeafCount >= leafCount` instead: partly-watched sits with never-started under
    /// "no mark", because the honest statement about a show mid-run is the resume state of its next
    /// episode, which a poster in a grid does not have. Caught by a device capture — a library
    /// filtered to `unwatchedLeaves=1` had five tiles wearing a watched disc.
    ///
    /// The comparison is the house rule, not a new one: it is `metadata::Season::watched`'s, and the
    /// same one `fetch_detail` applies to a show — including the load-bearing `leaf_count > 0` half,
    /// without which a container the server sent no counts for is `0 >= 0` and reads as watched.
    pub watched: bool,
    /// `originallyAvailableAt`, verbatim (`YYYY-MM-DD`) or empty — the RELEASE DATE an episode
    /// shelf trails under a focused tile (`Library Screens.dc.html` E: "focus adds the episode's
    /// name and its one trailing fact — time left on Continue Watching, release date on Recently
    /// Released"). Formatted by `plx_ui::fmt::pretty_date`, which already takes `year` as the
    /// fallback for an item the server dated only to a year.
    pub aired: String,
    /// A collection's member count (`childCount`) — its tile's caption, "12 items". 0 on every
    /// other kind, where the listing's count fields mean leaves rather than members.
    pub child_count: i64,
}

impl PmsMovie {
    /// Played fraction for the amber resume bar, or None when not in progress — THE one
    /// resume-bar rule, shared by the home shelves and the Library grid (it was copy-pasted
    /// into both screens before), and the definition of `PosterMark::InProgress`.
    ///
    /// **A resume point at or past the end is NOT in progress.** That is a finished item whose
    /// `viewOffset` the server never cleared, and counting it as in-progress drew a 100%-full bar
    /// that read as a rendering bug — and, once the poster's mark became the watched disc
    /// (2026-08-13), also suppressed the disc that item should be wearing, so a finished movie could
    /// end up with a full bar and no check. `ui::detail::ep_state` has always applied this rule to
    /// an episode still; now a poster and the filmstrip beside it cannot describe one item two ways.
    pub fn resume_frac(&self) -> Option<f32> {
        (self.resume_ms > 0 && self.dur_ns > 0
            && self.resume_ms * 1_000_000 < self.dur_ns)
            .then(|| (self.resume_ms as f32 * 1_000_000.0 / self.dur_ns as f32).clamp(0.0, 1.0))
    }
}

// Published as one immutable allocation, on the main thread. Owned readers retain the Arc,
// so a later store commit cannot invalidate their data. Legacy accessors below still have the
// old main-thread/until-next-commit lifetime and are retired with their screens.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HomeCatalog {
    items: Vec<Arc<PmsMovie>>,
    hubs: Vec<HubRow>,
    heroes: Vec<HeroSlot>,
}
static EMPTY_HOME: LazyLock<Arc<HomeCatalog>> = LazyLock::new(|| Arc::new(HomeCatalog::default()));

/// One Hubs owner's main-thread-only logical state (`docs/stores-as-machines.md`). Production
/// gains one through `stores::hubs::HubsStore`; the worker-touched half is [`PmsAdapter`].
pub struct PmsState {
    published: Option<Arc<HomeCatalog>>,
    /// The source table, in display order: our own servers first, then each shared one. Main
    /// thread only, so no lock is needed once this lives per-owner rather than behind a global.
    srcs: Vec<Src>,
    /// What the source table was last built from — the registry's exact roster generation and the
    /// pinned-library set. See the retired `SEEN` static's doc.
    seen: u64,
    /// …and what the roster SAID at the time. See the retired `SEEN_FACTS` static's doc.
    seen_facts: u32,
    /// Bumped by every authoritative fetch. See the retired `HUB_GEN` static's doc.
    hub_gen: u32,
    /// The retained Browse directory's semantic pin fingerprint as of the last merge. See the
    /// retired `LAST_SECTIONS_GEN` static's doc.
    last_sections_gen: u32,
    /// Moves every time the published catalog is replaced. See the retired `CATALOG_GEN` static's
    /// doc.
    pub catalog_gen: u32,
    /// Which sources may have a hub fetch out this tick ([`crate::stores::fanout`]).
    fanout: crate::stores::fanout::Fanout,
    /// A Continue Watching move waiting for the servers whose lanes it had to read.
    deck_ask: Option<DeckAsk>,
    /// The rows (catalog order, inclusive) the screen holds cards for; see [`HOLD_ROWS`].
    hold: (usize, usize),
}

/// The id of the merged Continue Watching row, which a page ask names instead of one server's hub.
const DECK_ID: &str = "home.continue";

/// One move of the Continue Watching deck that is waiting on server reads: it runs when every
/// server it asked has landed or is retrying, so one failing server never holds the others.
struct DeckAsk {
    before: bool,
    waiting: Vec<ServerId>,
}

impl Default for PmsState {
    fn default() -> Self {
        Self {
            published: None,
            srcs: Vec::new(),
            seen: u64::MAX,
            seen_facts: u32::MAX,
            hub_gen: 0,
            last_sections_gen: 0,
            catalog_gen: 0,
            fanout: Default::default(),
            deck_ask: None,
            hold: (0, HOLD_ROWS - 1),
        }
    }
}

/// The `Arc`'d worker half of one Hubs owner: the landing mailbox and the request-id minter. A
/// worker captures a clone of the owning `Bridge`'s `Arc<PmsAdapter>` before it spawns; rotating
/// the store's live `Arc` (on `HubsCmd::Reset`) orphans that clone harmlessly — the old worker can
/// still land, but only into a mailbox nothing reads any more.
pub struct PmsAdapter {
    results: Mutex<Vec<Landing>>,
    /// Request-id allocation, shared across sources so an addressed Hubs result has a unique
    /// request id even when two servers are both on their first fetch. Per-adapter (not
    /// process-wide) is enough: a still-running old worker captured the RETIRED adapter and can
    /// only ever mint (or land) into it, never into the one a reset rotated in.
    next_request: AtomicU32,
    /// Workers spawned through [`spawn_fetch`] whose landing [`take_landings`] has not yet taken:
    /// `+1` on the main thread before the worker exists, `-1` when the OS refuses the spawn, `-n`
    /// when a take moves `n` landings out. It is the claim dump mode's `take_all_owed` waits on
    /// ([`owed`]), and it has to live here rather than on [`Src::fetching`], which clears when a
    /// landing is APPLIED, after the take that delivered it. Every admitted worker answers exactly
    /// once (a panicking fetch posts a failure), so it reaches zero. A request handed to a
    /// launcher that does not call [`spawn_fetch`] (a replay) is never counted. It lives on the
    /// adapter, so a reset's rotation retires the old count with the old mailbox.
    owed: AtomicU32,
    /// Test only: how many times [`take_landings`] has finished, so a fake worker can post only
    /// AFTER a take has found its mailbox empty.
    #[cfg(any(test, feature = "test-support"))]
    takes: AtomicU32,
}

impl Default for PmsAdapter {
    fn default() -> Self {
        Self {
            results: Mutex::new(Vec::new()),
            next_request: AtomicU32::new(1),
            owed: AtomicU32::new(0),
            #[cfg(any(test, feature = "test-support"))]
            takes: AtomicU32::new(0),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn owed_count_for_test(adapter: &PmsAdapter) -> u32 { adapter.owed.load(Ordering::SeqCst) }

/// Is a worker spawned through [`spawn_fetch`] still owing this adapter a landing? Read by the
/// landing gate's dump mode, which waits for the answer a request it issued owes
/// (`plx_machine::landgate::Gate::take_all_owed`).
pub fn owed(adapter: &PmsAdapter) -> bool {
    adapter.owed.load(Ordering::SeqCst) > 0
}

fn published_home(state: &PmsState) -> &Arc<HomeCatalog> {
    state.published.as_ref().unwrap_or(&EMPTY_HOME)
}

#[cfg(any(test, feature = "test-support"))]
fn catalog(state: &PmsState) -> &Vec<Arc<PmsMovie>> {
    &published_home(state).items
}

/// catalog row `i`, or None. [`commit`] is the one mutation and re-resolves the open surfaces
/// itself.
#[cfg(any(test, feature = "test-support"))]
pub fn movie(state: &PmsState, i: usize) -> Option<&PmsMovie> {
    catalog(state).get(i).map(|m| &**m)
}
/// Catalog index of the row `(sid, rk)` names, or -1.
///
/// **Server-scoped, and that is the whole point.** This used to scan `m.rk == rk` over one flat
/// catalog, which is unambiguous only while every row comes from one machine. On a Continue
/// Watching shelf merged across servers it is not: a friend's episode and one of ours can carry the
/// same ratingKey, so a bare-key scan returns whichever row is EARLIER — and the caller that
/// exposed it was `detail::mount_rk` (which then mounts the wrong backdrop, blur envelope and
/// selection). -1 stays "not in the hub catalog", which every caller already handles as
/// "off-catalog".
///
/// The item menu's Play-from-Start was the other caller and is not one any more: it carries the row
/// it was opened on (`screens::registry::ItemMenuArg`'s row), because a Library, Search or person-page tile is in no
/// hub at all and this answered -1 for every one of them.
#[cfg(any(test, feature = "test-support"))]
pub fn index_of_rk(state: &PmsState, sid: ServerId, rk: &str) -> c_int {
    catalog(state)
        .iter()
        .position(|m| plx_plex::plex::same_item((m.sid, &m.rk), (sid, rk)))
        .map(|i| i as c_int)
        .unwrap_or(-1)
}

// ---- helpers ----
/// owned copy of a metadata string with newlines flattened to spaces (single-line UI fields)
pub(crate) fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

/// Parse one Plex `Metadata` item (from a section listing OR a hub) into a catalog row.
/// pub: the Library browse store (`browse.rs`) and the person page (`person.rs`) map their
/// listings with it too.
///
/// `sid` is the server the response came from and is **passed in, never looked up**: all three
/// callers run this on a worker thread, and the house rule (`browse.rs`'s spawn site states it
/// outright) is that a worker reads no statics. It is also the only correct answer — the current
/// server can change while a page fetch is in flight, and the rows in hand belong to the machine
/// that was asked, not to whichever one is current when they finish parsing.
pub fn parse_item(it: &plx_plex::plex::Metadata, sid: ServerId) -> PmsMovie {
    let mut m = PmsMovie {
        sid,
        sec: it.library_section_id,
        ..Default::default()
    };
    m.aired = clean(&it.originally_available_at);
    m.kind = match it.kind.as_str() {
        "show" => 1,
        "season" => 2,
        "episode" => 3,
        "collection" => KIND_COLLECTION,
        _ => 0,
    };
    match m.kind {
        3 => {
            // episode: parent show = grandparent, season number = parentIndex
            m.show_rk = clean(&it.grandparent_rating_key);
            m.season_index = it.parent_index as c_int;
            m.show_title = clean(&it.grandparent_title);
            m.ep_index = it.index as c_int;
        }
        2 => {
            // season: parent show = parent, season number = index
            m.show_rk = clean(&it.parent_rating_key);
            m.season_index = it.index as c_int;
            m.show_title = clean(&it.parent_title);
        }
        _ => {}
    }
    // A COLLECTION HAS NO WATCH OR RESUME STATE of its own, whatever counters the server sends
    // with it. This is the one place that says so: both flags are false and `resume_ms` is zero
    // below, and every reader (the poster mark, `resume_frac`, the item menu) trusts the row.
    //
    // shows/seasons count leaves (a show with any watched episode is no longer "unwatched");
    // movies/episodes key on viewCount absence (docs/pms-api.md §2)
    m.unwatched = match m.kind {
        1 | 2 => it.viewed_leaf_count == 0 && it.leaf_count > 0,
        KIND_COLLECTION => false,
        _ => it.view_count == 0,
    };
    // …and DONE is its own question, not the negation of that one: for a container it takes ALL the
    // leaves, so a show three episodes in is neither (see the `watched` field's doc).
    m.watched = match m.kind {
        1 | 2 => it.leaf_count > 0 && it.viewed_leaf_count >= it.leaf_count,
        KIND_COLLECTION => false,
        _ => it.view_count > 0,
    };
    if m.kind == KIND_COLLECTION {
        m.child_count = it.child_count.max(0);
    }
    m.title = clean(&it.title);
    m.year = it.year as c_int;
    m.rating = clean(&it.content_rating);
    m.dur_ns = if it.duration > 0 {
        it.duration * 1_000_000
    } else {
        0
    };
    m.resume_ms = if m.kind == KIND_COLLECTION { 0 } else { it.view_offset };
    // poster: prefer the show poster for episodes (grandparentThumb) so a landscape
    // episode still doesn't fill a portrait card
    let thumb = if it.grandparent_thumb.is_empty() {
        &it.thumb
    } else {
        &it.grandparent_thumb
    };
    m.thumb = clean(thumb);
    // …and the item's OWN thumb, unsubstituted. The line above is right for a POSTER shelf and
    // wrong for a landscape one, and both exist: an episode's own thumb is a 16:9 still, so Home's
    // portrait cards want the show poster, while a 420x236 tile wants the still — with the
    // substitution applied, a search for a show drew the same fanart on every episode in the row.
    //
    // Kept as a second field rather than resolved per caller because `parse_item` runs on a worker
    // and cannot know which shelf will draw the row. Empty on a movie, where `thumb` already IS
    // the item's own.
    // **Keyed on the item BEING an episode, not on the show poster existing.** It used to be
    // `grandparent_thumb.is_empty()`, which is a proxy that fails in the one direction that
    // matters: an episode whose show has no poster kept its own 16:9 still ONLY in `thumb`, and
    // `widgets::still_key` prefers `art` over `thumb` — so a landscape tile drew the show's shared
    // backdrop while the episode's own still sat right there unused, which is the opposite of the
    // documented still -> art -> poster chain.
    m.still = if m.kind == 3 {
        clean(&it.thumb)
    } else {
        String::new()
    };
    m.art = clean(&it.art);
    m.summary = clean(&it.summary);
    m.rk = clean(&it.rating_key);
    // Media[0]: codecs + Part[0].key (movies/episodes; a show container has none)
    if let Some(md) = it.media.first() {
        m.vcodec = clean(&md.video_codec);
        m.acodec = clean(&md.audio_codec);
        if let Some(p0) = md.part.first() {
            m.part = clean(&p0.key);
        }
    }
    // UltraBlurColors -> the ambient gradient. `UltraBlurColors::corners` owns the corner ORDER and
    // the all-black-envelope guard (shared with the detail store, which keys the same wash off the
    // LOADED item); `de_ultrablur` already accepted both the array and object shapes PMS returns
    // (D-1), so blur populates where the old object-only read left it blank.
    if let Some(blur) = it.ultra_blur_colors.and_then(|u| u.corners()) {
        m.blur = blur;
        m.has_blur = true;
    }
    m
}

// The full-library browse path lives in `crate::browse` (the Library screen's per-section
// PAGED catalog — sparse store + off-thread page fetches via `section_items_query`). This
// module stays hub-only; `browse` reuses `parse_item` above for its listings.

// ---- home hubs: each hub is a titled slice of the catalog ----
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HubRow {
    title: String,
    hub_id: String, // locale-independent hubIdentifier ("home.continue", "home.movies.recent", …)
    // Provider listing path. Part of an identified hub's identity as well as the fallback when
    // `hubIdentifier` is absent: PMS reuses one identifier for section-specific Home rows while
    // publishing distinct keys. Kept verbatim — observed keys carry content-defining query terms
    // (`type`, `sectionID`, filters/sort), not the parent `/hubs?count=…` request's page size.
    key: String,
    /// Which SERVER this shelf's items came from, as the owner's handle ("friend") — empty
    /// whenever the row came from the signed-in user's own server, which is every row today.
    /// Empty is the ABSENCE of an annotation, not an empty one: the home shelf heading draws no
    /// separator and no second run at all for it (`ui::cards::heading_flow`), so the annotation costs
    /// a single-server library nothing — no gap, no dot, no draw call. (The heading's INK changed in
    /// the same pass, which is a separate, deliberate harmonization; `heading_flow`'s doc has it.)
    /// Populated by the multi-server data layer when it lands.
    source: String,
    /// Every item the shelf's listing holds, which `len` caps — a linked collection heading's
    /// "· N" (`HubRef::total`). 0 when the server named no total.
    total: usize,
    start: usize,
    len: usize,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    more: bool,
}
fn hubs(state: &PmsState) -> &Vec<HubRow> {
    &published_home(state).hubs
}

// ---- rotating hero pool: curated catalog indices (Continue Watching then Recently Added) ----
const HERO_MAX: usize = 8;

/// One page of the rotating billboard: a catalog index, plus the handle of the SERVER the shelf it
/// was drawn from came from ("friend") — empty for the signed-in user's own, exactly as
/// [`HubRow::source`] means it.
///
/// The handle is carried on the SLOT rather than looked up from the item's shelf at draw time, and
/// that is the point of the type existing at all: the pool is the one place in the app that lifts
/// items OUT of their shelf order ([`own_items_first`] promotes an owned page to the front), so a
/// pool entry that only knew its catalog index would have to find its way back to a hub through a
/// range scan to answer "whose is this" — for a fact the build already had in its hand.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HeroSlot {
    idx: usize,
    source: String,
}
/// A retained publication, independent of subsequent store commits. Cloning copies one Arc,
/// never a movie or string. The status is captured alongside the catalog, including failed
/// fetches which keep the previous content and therefore do not move its generation.
#[derive(Clone)]
pub struct HubsSnapshot {
    data: Arc<HomeCatalog>,
    generation: u32,
    state: HubState,
}

pub fn hubs_snapshot(state: &PmsState) -> HubsSnapshot {
    HubsSnapshot {
        data: Arc::clone(published_home(state)),
        generation: state.catalog_gen,
        state: hub_state(state),
    }
}

impl HubsSnapshot {
    /// An explicit empty retained publication; fixture construction must not capture globals.
    #[cfg(any(test, feature = "test-support"))]
    pub fn empty_for_test() -> Self {
        Self { data: Arc::new(HomeCatalog::default()), generation: 0, state: HubState::Loading }
    }

    pub fn view(&self) -> HubsView<'_> {
        HubsView { data: &self.data, generation: self.generation, state: self.state }
    }
}

/// Frame-borrowed Home data. Every reference is tied to the retained publication, not a static
/// catalog that an effect could replace. The bridge owns the snapshot; screens only get this.
#[derive(Clone, Copy)]
pub struct HubsView<'a> {
    data: &'a HomeCatalog,
    pub generation: u32,
    pub state: HubState,
}

#[derive(Clone, Copy)]
pub struct HubRef<'a> {
    pub identity: Option<HubIdentity<'a>>,
    pub title: &'a str,
    pub source: &'a str,
    /// Every item the listing holds, or zero when unknown. `items` is the current window.
    pub total: usize,
    pub items: &'a [Arc<PmsMovie>],
    /// Server offset of the current window, used to request an earlier page.
    pub offset: usize,
    pub more: bool,
}

/// Provider identities are tagged: a listing key cannot collide with an identifier that
/// happens to contain the same bytes. An identifier is scoped by both server and its
/// provider-published listing key: PMS reuses `home.television.recent` for section-specific rows,
/// while a cross-section row keeps one key even when its leading item's library changes. Neither
/// display text, item content nor position participates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HubIdentity<'a> {
    ContinueWatching,
    Identifier { sid: ServerId, id: &'a str, key: &'a str },
    Key { sid: ServerId, key: &'a str },
}

fn stable_hub_identity<'a>(row: &'a HubRow, items: &[Arc<PmsMovie>]) -> Option<HubIdentity<'a>> {
    if row.len == 0 { return None; }
    let sid = items.get(row.start)?.sid;
    if row.hub_id == "home.continue" { Some(HubIdentity::ContinueWatching) }
    else if !row.hub_id.is_empty() {
        Some(HubIdentity::Identifier { sid, id: &row.hub_id, key: &row.key })
    }
    else if !row.key.is_empty() { Some(HubIdentity::Key { sid, key: &row.key }) }
    else { None }
}

#[derive(Clone, Copy)]
pub struct HeroRef<'a> {
    pub item: &'a PmsMovie,
    pub source: &'a str,
}

impl<'a> HubsView<'a> {
    pub fn hub_count(self) -> usize { self.data.hubs.len() }
    pub fn hero_count(self) -> usize { self.data.heroes.len() }
    pub fn hub(self, index: usize) -> Option<HubRef<'a>> {
        let row = self.data.hubs.get(index)?;
        let end = row.start.checked_add(row.len)?;
        Some(HubRef {
            identity: stable_hub_identity(row, &self.data.items),
            title: &row.title, source: &row.source, total: row.total,
            offset: row.offset, more: row.more,
            items: self.data.items.get(row.start..end)?,
        })
    }
    pub fn hero(self, index: usize) -> Option<HeroRef<'a>> {
        let slot = self.data.heroes.get(index)?;
        Some(HeroRef { item: &**self.data.items.get(slot.idx)?, source: &slot.source })
    }

    /// Server-scoped catalog lookup by `(sid, rk)`, over the retained publication rather than a
    /// global — see [`index_of_rk`]'s doc for why the scan must not compare `rk` alone.
    pub fn find(self, sid: ServerId, rk: &str) -> Option<&'a PmsMovie> {
        self.data.items.iter().find(|m| plx_plex::plex::same_item((m.sid, &m.rk), (sid, rk))).map(|m| &**m)
    }
}

/// **Own items first — an ORDERING, not a filter** (Shared Sources, deliverable C).
///
/// A borrowed item may not hold the FIRST rotation while the owner contributes at least one, so the
/// app's front door opens on your own library and a friend's film arrives one 8-second flip in,
/// attributed. Everything else about the pool is untouched: it stays merged, in the order the
/// shelves produced it, and the promoted page is lifted out and re-inserted rather than sorted, so
/// `[B1, B2, O1, O2, B3]` becomes `[O1, B1, B2, O2, B3]` — one page moves, nothing is dropped and
/// nothing else is reordered.
///
/// Filtering instead would leave a borrowed-only account with **no hero at all**, and would overrule
/// a pin the user made; that is why a pool with nothing of our own in it is left exactly as it is and
/// opens on a borrowed page. This is also why the rule needs no switch of its own: the pool is built
/// from included sources only, so a borrowed hero is always the consequence of a pin.
fn own_items_first(pool: &mut Vec<HeroSlot>) {
    if pool.first().map(|s| s.source.is_empty()).unwrap_or(true) {
        return; // nothing pooled, or one of ours already opens the door
    }
    if let Some(k) = pool.iter().position(|s| s.source.is_empty()) {
        let own = pool.remove(k);
        pool.insert(0, own);
    }
}

/// number of home hubs
pub fn hub_count(state: &PmsState) -> usize {
    hubs(state).len()
}
/// Item count in hub `i`, read straight off the published catalog. Test-only: production reads
/// the retained publication ([`HubsView::hub`]), and this is what other modules' store and
/// dispatcher tests assert a landing with.
#[cfg(any(test, feature = "test-support"))]
pub fn hub_len(state: &PmsState, i: usize) -> usize {
    hubs(state).get(i).map(|h| h.len).unwrap_or(0)
}

/// item `col` of hub `hub`, or None
#[cfg(any(test, feature = "test-support"))]
pub fn hub_item(state: &PmsState, hub: usize, col: usize) -> Option<&PmsMovie> {
    let h = hubs(state).get(hub)?;
    if col < h.len {
        movie(state, h.start + col)
    } else {
        None
    }
}

/// Refetch the home hubs OFF the main thread — every source on a worker, the owned one included,
/// landing through [`pump`] like any other fetch. **MAIN THREAD, NON-BLOCKING.**
///
/// No reconcile call here, and none is owed anywhere: [`commit`] performs the re-selection and the
/// repaint itself, at the only moment the catalog those surfaces index into actually moves.
fn request_refetch_hubs_with_scope(state: &mut PmsState, adapter: &Arc<PmsAdapter>, scope: &BrowseScope,
    launch: &mut dyn FnMut(HubRequest) -> bool) -> crate::stores::EndpointRefreshSet {
    // A test reaching this outside `plx_base::testlock::serial()` races some other module's test — see
    // `lib.rs::testlock`.
    #[cfg(any(test, feature = "test-support"))]
    plx_base::testlock::assert_held("the pms hub catalog (request_refetch_hubs)");
    state.hub_gen = state.hub_gen.wrapping_add(1); // supersede every retry already in flight
    sync_roster_with_scope(state, scope);
    let gen = state.hub_gen;
    let mut srcs = std::mem::take(&mut state.srcs);
    // A superseded worker's landing is dropped on the generation above, so releasing the
    // single-flight latches here cannot double-apply anything — and without it a source whose
    // worker was in flight across this call would stay latched and never fetch again. An
    // authoritative request supersedes any older flight, so release every latch here.
    let mut endpoints = crate::stores::EndpointRefreshSet::default();
    for s in srcs.iter_mut() {
        s.fetching = false;
        s.page = None;
    }
    let mut turn = fanout_turn(&mut state.fanout, &srcs, |_| true);
    for s in srcs.iter_mut() {
        if let Some(request) = retry_now_gated(&mut turn, gen, adapter, s, scope, launch) { endpoints.insert(request); }
    }
    state.srcs = srcs;
    endpoints
}

/// A local, **optimistic** edit to what the shelves say about one item — applied before the write
/// that justifies it has left the machine, so a press lands on the panel at once however far away
/// the item's server is. See [`edit_item_with_scope`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LocalEdit {
    /// The item is now watched (`true`) or unwatched (`false`), everywhere it appears.
    Watched(bool),
    /// The item has been hidden from Continue Watching (`removeFromContinueWatching`) — it leaves
    /// the deck and NOTHING else about it changes, which is exactly what that endpoint does.
    LeftTheDeck,
}

/// Apply `edit` to every shelf row naming `(sid, rk)` and re-commit Home under the retained Browse
/// scope. Returns whether anything
/// matched. **MAIN THREAD** — it rebuilds the catalog the UI holds `&'static` rows out of.
///
/// It edits each source's own last PROJECTION and re-runs the pure [`merge`], rather than splicing
/// the committed catalog: `HubRow` addresses its cards as a `start`/`len` window into one flat
/// `Vec`, and the hero pool holds indices into the same, so removing a row by hand means fixing up
/// every window behind it and every pool slot — three chances to leave the three statics disagreeing,
/// which is the exact class `commit`'s doc says they move together to avoid. Re-merging is arithmetic
/// the module already trusts, and it also lets a shelf that lost a card refill from the budget.
///
/// This is one half of a pair and is useless alone: it is what the user SEES, and the refetch the
/// write's landing kicks is what the server SAYS. Where they disagree the refetch wins, silently.
#[cfg(test)]
fn edit_item(state: &mut PmsState, sid: ServerId, rk: &str, edit: LocalEdit) -> bool {
    edit_item_with_scope(state, sid, rk, edit, &BrowseScope::standalone())
}

fn edit_item_with_scope(
    state: &mut PmsState,
    sid: ServerId,
    rk: &str,
    edit: LocalEdit,
    scope: &BrowseScope,
) -> bool {
    // Same test-only catalog guard as `request_refetch_hubs` — the catalog is `PmsState`, a
    // field of the per-`Bridge` `HubsStore`, not a crate-global; see `lib.rs::testlock` and D5.
    #[cfg(any(test, feature = "test-support"))]
    plx_base::testlock::assert_held("the pms hub catalog (edit_item)");
    let mut hit = false;
    for s in state.srcs.iter_mut() {
        if let Some(b) = s.last.as_mut() {
            hit |= apply_edit(b, sid, rk, edit);
        }
    }
    if !hit {
        return false; // the item is on no shelf (a Library-grid or Related item): nothing to redraw
    }
    let build = merge_with_scope(&state.srcs, scope);
    adopt_browse_scope(state, scope);
    commit(state, build);
    true
}

/// [`edit_item_with_scope`] on ONE source's projection. Pure — no statics, no I/O — so the rule is
/// graded on the host rather than inferred from a screenshot.
fn apply_edit(b: &mut SourceBuild, sid: ServerId, rk: &str, edit: LocalEdit) -> bool {
    let mine = |m: &PmsMovie| plx_plex::plex::same_item((m.sid, &m.rk), (sid, rk));
    match edit {
        LocalEdit::Watched(on) => {
            let mut hit = false;
            for c in b.cw.iter_mut().chain(b.lane.rows.iter_mut()) {
                if mine(&*c.m) {
                    set_watched(Arc::make_mut(&mut c.m), on);
                    hit = true;
                }
            }
            for m in b.shelves.iter_mut().flat_map(|s| s.items.iter_mut()) {
                // test the shared card first: `make_mut` clones only the card that matches
                if mine(&**m) {
                    set_watched(Arc::make_mut(m), on);
                    hit = true;
                }
            }
            hit
        }
        LocalEdit::LeftTheDeck => {
            let before = b.cw.len();
            b.cw.retain(|c| !mine(&*c.m));
            b.lane.remove(|row| mine(&*row.m)) || b.cw.len() != before
        }
    }
}

/// The three fields one row's watch state is spread over, moved together.
///
/// `resume_ms` goes with them, and it is the half that is easy to miss: [`PmsMovie::resume_frac`]
/// takes PRECEDENCE over the watched flag at the mark (`ui::widgets::poster_mark` — a re-watch in
/// flight outranks a finished item), so a row left holding its old `viewOffset` would wear the
/// progress bar it had before and show no tick at all — the press would read as having done
/// nothing. An unscrobble genuinely clears `viewOffset` server-side, and a scrobbled item leaves
/// the deck; where the server disagrees, its own refetch is a moment behind this and wins.
///
/// `pub` because the hub catalog stopped being the only store an optimistic edit reaches:
/// `browse`, `search` and `person` each hold their own rows and each flips them the same way, and
/// three copies of "which three fields" is three chances for one of them to leave the resume bar on.
pub fn set_watched(m: &mut PmsMovie, on: bool) {
    m.watched = on;
    m.unwatched = !on;
    m.resume_ms = 0;
}

/// The catalog/hubs/pool triple the merge produces, before it is committed.
type HubBuild = (Vec<Arc<PmsMovie>>, Vec<HubRow>, Vec<HeroSlot>);

// ---- one source's contribution -----------------------------------------------------------------

/// A Continue Watching entry, carrying the sort key the MERGE needs. `lastViewedAt` used to be read
/// off the wire DTO and thrown away at parse time, because one server's hub arrived already in the
/// right order; across sources the order has to be re-established after the fact, so the key has to
/// survive the projection.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CwItem {
    pub(crate) last_viewed_at: i64,
    pub(crate) m: Arc<PmsMovie>,
    /// Where the listing behind `/hubs/continueWatching/items` holds this card; 0 until the deck
    /// is placed, when a preview card takes its place in the preview.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) position: usize,
}

fn is_zero(n: &usize) -> bool { *n == 0 }

/// One shelf as a source projected it: rows already parsed, filtered and stamped with the server
/// they came from, so the merge is pure arithmetic over owned data and never touches a wire DTO.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Shelf {
    title: String,
    hub_id: String,
    key: String,
    items: Vec<Arc<PmsMovie>>,
    #[serde(default)]
    positions: Vec<usize>,
    /// Every item the hub's listing holds (`plex::Hub::total`) — a collection shelf's "· N".
    #[serde(default)]
    total: usize,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    end: usize,
    #[serde(default)]
    more: bool,
    /// Cards the row showed when it gave them up to the ring; 0 while it holds cards. A row with
    /// `shown > 0` and no items is a descriptor: Home publishes that many placeholder slots for it.
    #[serde(default, skip_serializing_if = "is_zero")]
    shown: usize,
    /// How the window relates to its listing, and the keys the row has shown. Absent for a row that
    /// has not paged, so an older recording decodes unchanged.
    #[serde(default, skip_serializing_if = "paging::RowState::is_default")]
    row: paging::RowState,
}

impl Shelf {
    fn is(&self, id: &str, key: &str) -> bool { self.hub_id == id && self.key == key }
    /// A descriptor: the row gave its cards up to the ring and keeps what is needed to ask for them.
    fn released(&self) -> bool { self.items.is_empty() && self.shown > 0 }
}

/// ONE source's whole contribution to Home — its Continue Watching items (merged with everyone
/// else's into a single shelf) and its own shelves (kept whole, annotated with its owner's handle).
///
/// Owned data only, so a worker can build it and hand it over through the mailbox, and a source can
/// KEEP the last one it answered with across a failure.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceBuild {
    cw: Vec<CwItem>,
    shelves: Vec<Shelf>,
    /// This server's part of the Continue Watching deck beyond its preview (`stores::deck`). Absent
    /// until the row has a listing to read, so an older recording decodes unchanged.
    #[serde(default, skip_serializing_if = "deck::Lane::is_default")]
    lane: deck::Lane,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    id: String,
    key: String,
    start: usize,
    #[serde(default)]
    before: bool,
    #[serde(default)]
    hidden: Vec<i64>,
    /// Not a move of the window: the row came back into the ring and is read again where it stood.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    reload: bool,
}

/// Whether a hub's listing is one the pager reads: every row whose key [`is_pageable_hub_key`]
/// admits pages, whatever it is called. A row whose key is not admitted keeps its preview.
fn pages(key: &str) -> bool {
    plx_plex::plex::is_pageable_hub_key(key)
}

/// How a fresh preview row starts: a row the pager will read waits for its first forward ask to
/// compare the preview with its listing; every other row is its preview, and Recently Added rows
/// are the head of their listing by definition.
fn preview_row(id: &str, key: &str) -> paging::RowState {
    if plx_plex::plex::is_pageable_hub_key(key) && !plx_plex::plex::hub_title::is_recently_added_hub(id) {
        paging::RowState { mode: paging::RowMode::Unprobed, ledger: None }
    } else {
        paging::RowState::default()
    }
}

/// The event-log line for a hub that holds more than its preview behind a key the pager does not
/// read, so those rows are findable in a log; `None` after the first time for the same hub and key.
fn unpaged_line(id: &str, key: &str) -> Option<String> {
    static SEEN: LazyLock<Mutex<std::collections::HashSet<String>>> = LazyLock::new(Default::default);
    let first = SEEN.lock().ok()?.insert(format!("{id} {key}"));
    first.then(|| format!("hubs: {id} holds more than its preview but its key {key} is not paged; the preview stays"))
}

fn find_shelf<'a>(shelves: &'a [Shelf], id: &str, key: &str) -> Option<&'a Shelf> {
    shelves.iter().find(|shelf| shelf.is(id, key))
}

fn request_page(state: &mut PmsState, adapter: &PmsAdapter, sid: ServerId,
    id: &str, key: &str, before: bool, scope: &BrowseScope, launch: &mut dyn FnMut(HubRequest) -> bool) -> Option<crate::stores::EndpointRefresh> {
    let Some(source) = state.srcs.iter_mut().find(|source| source.sid == sid) else { return None };
    if refresh_src_lifecycle(source) { return None; }
    if source.fetching || source.page.is_some() || source.state != HubState::Ready { return None; }
    let Some(shelf) = source.last.as_ref().and_then(|build| find_shelf(&build.shelves, id, key)) else { return None };
    if !pages(key) || (before && shelf.offset == 0) || (!before && !shelf.more) { return None; }
    let start = if before { shelf.offset } else { shelf.end.max(shelf.offset + shelf.items.len()) };
    source.retry_n = 0;
    source.page = Some(PageQuery { id: id.into(), key: key.into(), start, before,
        hidden: hidden_sections(scope, sid), reload: false });
    kick_with(state.hub_gen, adapter, source, scope, launch)
}

fn cancel_page(state: &mut PmsState, sid: ServerId, id: &str, key: &str) {
    let Some(source) = state.srcs.iter_mut().find(|source| source.sid == sid
        && source.page.as_ref().is_some_and(|page| page.id == id && page.key == key)) else { return };
    source.page = None;
    source.fetching = false;
    source.seq = source.seq.wrapping_add(1);
    source.retry_n = 0;
    source.retry_s = 0.0;
}

/// The shared body of `Page` and `CancelPage`, for both command runners.
fn run_page_cmd(state: &mut PmsState, adapter: &PmsAdapter, scope: &BrowseScope, cmd: crate::stores::hubs::HubsCmd,
    launch: &mut dyn FnMut(HubRequest) -> bool) -> crate::stores::EndpointRefreshSet {
    use crate::stores::hubs::HubsCmd;
    let mut endpoints = crate::stores::EndpointRefreshSet::default();
    match cmd {
        HubsCmd::CancelPage { id, .. } if id == DECK_ID => {
            state.deck_ask = None;
            let sids: Vec<ServerId> = state.srcs.iter().map(|s| s.sid).collect();
            for sid in sids { cancel_page(state, sid, DECK_ID, ""); }
        }
        HubsCmd::CancelPage { sid, id, key } => cancel_page(state, sid, &id, &key),
        HubsCmd::Page { id, before, .. } if id == DECK_ID => {
            sync_roster_with_scope(state, scope);
            endpoints = request_deck_page(state, adapter, before, scope, launch);
        }
        HubsCmd::Page { sid, id, key, before } => {
            sync_roster_with_scope(state, scope);
            if let Some(endpoint) = request_page(state, adapter, sid, &id, &key, before, scope, launch) { endpoints.insert(endpoint); }
        }
        _ => {}
    }
    endpoints
}

fn hidden_sections(scope: &BrowseScope, sid: ServerId) -> Vec<i64> {
    scope.pins.iter().filter(|(server, section, pinned)| *server == sid && *section != 0 && !pinned)
        .map(|(_, section, _)| *section).collect()
}

fn fetch_page(client: &plx_plex::plex::Client, sid: ServerId, page: &PageQuery, minimum_end: usize) -> Option<SourceBuild> {
    fetch_row(sid, page, None, minimum_end, |start, size| client.hub_items_paged(&page.key, start as i64, size as i64),
        |keys| many_items(client, keys))
}

/// The batch read the ledger re-materialises a window through: one row per key, in request order.
fn many_items(client: &plx_plex::plex::Client, keys: &[String]) -> Option<plx_plex::plex::MediaContainer> {
    client.metadata_many(&keys.iter().map(String::as_str).collect::<Vec<_>>())
}

/// The window a shelf holds, in the shape [`paging`] moves.
fn shelf_window(shelf: &Shelf) -> (Vec<paging::Row>, paging::PageInfo) {
    let rows = shelf.positions.iter().copied().zip(shelf.items.iter().map(Arc::clone)).collect();
    (rows, paging::PageInfo { offset: shelf.offset, end: shelf.end, total: shelf.total, more: shelf.more, unstable: false })
}

/// One ask against a row's window, on the sliding window of [`paging`]; `list(start, size)` is one
/// page of the row's listing and `many(keys)` a batch read of items by rating key.
///
/// A row not yet compared with its listing (`RowMode::Unprobed`) reads the listing from 0 on its
/// first forward ask. If the listing starts with the preview's rating keys, the row's positions are
/// listing offsets. If not (a `random` hub shows a sample), the row is the preview followed by the
/// listing minus the preview's keys, and a position past the preview is a listing offset plus the
/// preview's length.
///
/// A row whose listing does not hold between requests (the edge row is nowhere near where it was),
/// or whose server ignores paging (a read is answered from another offset), moves onto its ledger
/// of shown keys instead of stopping, so every item is still reached and none is shown twice.
fn fetch_row(sid: ServerId, page: &PageQuery, current: Option<&Shelf>, minimum_end: usize,
    mut list: impl FnMut(usize, usize) -> Option<plx_plex::plex::MediaContainer>,
    mut many: impl FnMut(&[String]) -> Option<plx_plex::plex::MediaContainer>) -> Option<SourceBuild> {
    let mut held = current.map(shelf_window);
    let mut state = current.map(|shelf| shelf.row.clone()).unwrap_or_default();
    let ignored = std::cell::Cell::new(false);
    let mut raw = |start: usize, size: usize| {
        let mc = list(start, size)?;
        if mc.offset != start as i64 { ignored.set(true); }
        Some(mc)
    };
    let mut start = page.start;
    let mut absorbed = false;
    if matches!(state.mode, paging::RowMode::Unprobed) && !page.before {
        if let Some((rows, info)) = held.as_mut() {
            let mc = raw(0, info.end.max(1))?;
            if ignored.get() {
                absorbed = true;
                state.mode = paging::RowMode::Head;
            } else if rows.iter().all(|(position, item)| mc.metadata.get(*position)
                .is_some_and(|listed| clean(&listed.rating_key) == item.rk)) {
                state.mode = paging::RowMode::Head;
            } else {
                let preview: Vec<String> = rows.iter().map(|(_, item)| item.rk.clone()).collect();
                for (i, row) in rows.iter_mut().enumerate() { row.0 = i; }
                (info.offset, info.end) = (0, preview.len());
                start = info.end;
                state.mode = paging::RowMode::Sample(preview);
            }
        }
    }
    let ask = paging::Ask { start, before: page.before, hidden: &page.hidden };
    let preview_len = match &state.mode { paging::RowMode::Sample(preview) => preview.len(), _ => 0 };
    let result;
    if let Some((rows, info)) = held.as_ref().filter(|_| state.ledger.as_ref().is_some_and(|ledger| ledger.active)) {
        let mut ledger = state.ledger.clone()?;
        result = Some(paging::ledger_window(sid, &ask, (rows, *info), &mut ledger, &mut raw, &mut many)?);
        state.ledger = Some(ledger);
    } else {
        let step = if absorbed { None } else {
            let held_ref = held.as_ref().map(|(rows, info)| (rows.as_slice(), *info));
            match &state.mode {
                paging::RowMode::Sample(preview) => {
                    let total = held_ref.map_or(0, |(_, info)| info.total);
                    let mut sample = sample_listing(preview, total, &ignored, &mut raw, &mut many);
                    paging::fetch_window(sid, &ask, held_ref, minimum_end, &mut sample)
                }
                _ => paging::fetch_window(sid, &ask, held_ref, minimum_end, &mut raw),
            }
        };
        // A window that came back holding a key the row showed before the rows it kept is a listing
        // that shifted by more than an edge check can tell: it stands in for an edge that is lost.
        let switch = absorbed || match &step {
            Some((rows, info)) => info.unstable
                || state.ledger.as_ref().is_some_and(|ledger| ledger.contradicts(rows)),
            None => ignored.get(),
        };
        if !switch {
            let (rows, mut info) = step?;
            if let Some((held_rows, _)) = &held {
                let ledger = state.ledger.get_or_insert_with(Default::default);
                ledger.note(held_rows);
                ledger.note(&rows);
            }
            if info.total > 0 { info.total = info.total.saturating_sub(preview_len); }
            result = Some((rows, info));
        } else if let Some((rows, info)) = &held {
            let mut ledger = state.ledger.take().unwrap_or_default();
            ledger.note(rows);
            ledger.active = true;
            ledger.next = info.end.saturating_sub(preview_len);
            ledger.done = false;
            ledger.idle = 0;
            ledger.added = 0;
            ledger.rescans = 0;
            ledger.filtered = 0;
            result = Some(paging::ledger_window(sid, &ask, (rows, *info), &mut ledger, &mut raw, &mut many)?);
            state.ledger = Some(ledger);
        } else {
            return None;
        }
    }
    let (rows, info) = result?;
    let (positions, items) = rows.into_iter().unzip();
    Some(SourceBuild { cw: Vec::new(), lane: Default::default(), shelves: vec![Shelf {
        title: String::new(), hub_id: page.id.clone(), key: page.key.clone(), items, positions,
        total: info.total, offset: info.offset, end: info.end, more: info.more, shown: 0, row: state,
    }] })
}

/// The row of a `random` hub as one listing: positions below the preview's length are its cards,
/// read by key; the rest are the listing from its first row, with a row the preview already shows
/// left blank so positions stay aligned and the card is not shown twice. `total` is the listing's
/// count when known; the container reports it with the preview's length added.
fn sample_listing<'a>(preview: &'a [String], total: usize, ignored: &'a std::cell::Cell<bool>,
    list: &'a mut dyn FnMut(usize, usize) -> Option<plx_plex::plex::MediaContainer>,
    many: &'a mut dyn FnMut(&[String]) -> Option<plx_plex::plex::MediaContainer>,
) -> impl FnMut(usize, usize) -> Option<plx_plex::plex::MediaContainer> + 'a {
    use plx_plex::plex::{MediaContainer, Metadata};
    let blank = |key: &str| Metadata { rating_key: key.into(), ..Metadata::default() };
    move |start, size| {
        let length = preview.len();
        let mut metadata = Vec::new();
        let mut listing_total = (total > 0).then_some(total);
        if start < length {
            let keys = &preview[start..(start + size).min(length)];
            let mut read = many(keys)?.metadata;
            for key in keys {
                let found = read.iter().position(|item| clean(&item.rating_key) == *key);
                metadata.push(found.map_or_else(|| blank(key), |i| read.swap_remove(i)));
            }
        }
        let from = start.max(length);
        if start + size > from {
            let want = start + size - from;
            let mc = list(from - length, want)?;
            if mc.offset != (from - length) as i64 {
                ignored.set(true);
                return Some(MediaContainer { offset: -1, ..MediaContainer::default() });
            }
            if mc.total_size > 0 { listing_total = Some(mc.total_size as usize); }
            metadata.extend(mc.metadata.into_iter().take(want).map(|item| {
                if preview.contains(&clean(&item.rating_key)) { blank("") } else { item }
            }));
        }
        Some(MediaContainer { offset: start as i64, total_size: listing_total.map_or(0, |n| (n + length) as i64),
            metadata, ..MediaContainer::default() })
    }
}

/// The window of a row on the plain sliding window alone: no ledger, no batch read. What the
/// characterisation tests of that window run against.
#[cfg(test)]
fn fetch_window(sid: ServerId, page: &PageQuery, current: Option<&Shelf>, minimum_end: usize,
    fetch: impl FnMut(usize, usize) -> Option<plx_plex::plex::MediaContainer>) -> Option<SourceBuild> {
    let ask = paging::Ask { start: page.start, before: page.before, hidden: &page.hidden };
    let current = current.map(shelf_window);
    let (rows, info) = paging::fetch_window(sid, &ask, current.as_ref().map(|(rows, info)| (rows.as_slice(), *info)),
        minimum_end, fetch)?;
    let (positions, items) = rows.into_iter().unzip();
    Some(SourceBuild { cw: Vec::new(), lane: Default::default(), shelves: vec![Shelf {
        title: String::new(), hub_id: page.id.clone(), key: page.key.clone(), items, positions,
        total: info.total, offset: info.offset, end: info.end, more: info.more, shown: 0, row: Default::default(),
    }] })
}

/// The live sources' lanes, moved out so the deck can work on them as one slice; `put_lanes` returns
/// them. The order is the one `live_sources` gives, which is the deck's roster order.
fn take_lanes(srcs: &mut [Src]) -> (Vec<usize>, Vec<deck::Lane>) {
    let idx: Vec<usize> = (0..srcs.len()).filter(|&i| srcs[i].last.is_some()).collect();
    let lanes = idx.iter().filter_map(|&i| srcs[i].last.as_mut().map(|b| std::mem::take(&mut b.lane))).collect();
    (idx, lanes)
}

fn put_lanes(srcs: &mut [Src], idx: &[usize], lanes: Vec<deck::Lane>) {
    for (&i, lane) in idx.iter().zip(lanes) {
        if let Some(build) = srcs[i].last.as_mut() { build.lane = lane; }
    }
}

/// Puts the deck on the lanes that have not got it (a first answer, a server that joined, a lane
/// whose reload failed) and brings a refreshed window back to its bound.
fn settle_deck(srcs: &mut [Src], refreshed: bool) {
    if srcs.iter().filter_map(|s| s.last.as_ref()).all(|b| b.lane.placed) && !refreshed { return; }
    let previews: Vec<Vec<CwItem>> = srcs.iter().filter_map(|s| s.last.as_ref()).map(|b| b.cw.clone()).collect();
    let (idx, mut lanes) = take_lanes(srcs);
    let refs: Vec<&[CwItem]> = previews.iter().map(Vec::as_slice).collect();
    deck::place(&mut lanes, &refs);
    if refreshed { deck::fit(&mut lanes); }
    put_lanes(srcs, &idx, lanes);
}

/// Moves the deck one page in a direction; a lane that could not be read stays where it is.
fn advance_deck(srcs: &mut [Src], before: bool) {
    let (idx, mut lanes) = take_lanes(srcs);
    deck::advance(&mut lanes, !before, deck::PAGE);
    put_lanes(srcs, &idx, lanes);
}

/// A move of the Continue Watching deck. Every lane that holds too few rows to be sure of the next
/// cards is read through the fan-out gate; when each of those has landed (or is retrying) the deck
/// moves. With nothing to read, it moves at once.
fn request_deck_page(state: &mut PmsState, adapter: &PmsAdapter, before: bool, scope: &BrowseScope,
    launch: &mut dyn FnMut(HubRequest) -> bool) -> crate::stores::EndpointRefreshSet {
    let mut endpoints = crate::stores::EndpointRefreshSet::default();
    if state.deck_ask.is_some() { return endpoints; }
    let gen = state.hub_gen;
    let mut srcs = std::mem::take(&mut state.srcs);
    let (idx, lanes) = take_lanes(&mut srcs);
    let short: Vec<usize> = deck::needs(&lanes, !before, deck::PAGE).into_iter().map(|k| idx[k]).collect();
    put_lanes(&mut srcs, &idx, lanes);
    if idx.is_empty() || srcs.iter().any(|s| s.last.as_ref().is_some_and(|b| !b.lane.placed)) {
        state.srcs = srcs;
        return endpoints;
    }
    let mut waiting = Vec::new();
    for &i in &short {
        let s = &mut srcs[i];
        if refresh_src_lifecycle(s) || s.fetching || s.page.is_some() || s.state != HubState::Ready { continue; }
        s.retry_n = 0;
        s.page = Some(PageQuery { id: DECK_ID.into(), key: String::new(), start: 0, before, hidden: Vec::new(), reload: false });
        waiting.push(i);
    }
    if waiting.is_empty() {
        advance_deck(&mut srcs, before);
        let mut build = merge_with_scope(&srcs, scope);
        state.srcs = srcs;
        preserve_heroes(state, &mut build);
        commit(state, build);
        return endpoints;
    }
    let mut turn = fanout_turn(&mut state.fanout, &srcs, source_waiting);
    for &i in &waiting {
        if let Some(request) = kick_gated(&mut turn, gen, adapter, &mut srcs[i], scope, &mut *launch) { endpoints.insert(request); }
    }
    state.deck_ask = Some(DeckAsk { before, waiting: waiting.iter().map(|&i| srcs[i].sid).collect() });
    state.srcs = srcs;
    endpoints
}

/// The deck's pending move has nothing left to wait for: every server it asked has landed, or
/// failed and is retrying (or gave the page up).
fn deck_ready(ask: &DeckAsk, srcs: &[Src]) -> bool {
    !ask.waiting.iter().any(|sid| srcs.iter().any(|s| s.sid == *sid && s.retry_n == 0
        && s.page.as_ref().is_some_and(|page| page.id == DECK_ID)))
}

fn land_lane(source: &mut Src, build: SourceBuild) -> bool {
    let Some(last) = source.last.as_mut() else { return false };
    last.lane = build.lane;
    true
}

fn land_page(source: &mut Src, page: &PageQuery, build: SourceBuild) -> bool {
    let Some(current) = source.last.as_mut().and_then(|build| build.shelves.iter_mut()
        .find(|shelf| shelf.is(&page.id, &page.key))) else { return false };
    let Some(mut next) = build.shelves.into_iter().find(|shelf| shelf.is(&page.id, &page.key)) else { return false };
    if next.items.is_empty() || (next.more && next.offset == current.offset && next.end <= current.end) {
        current.more = false;
        return true;
    }
    next.items.truncate(MAX_SHELF_ITEMS);
    current.items = next.items;
    current.positions = next.positions;
    current.offset = next.offset;
    current.end = next.end;
    current.more = next.more;
    current.total = next.total;
    current.row = next.row;
    true
}

/// A row back in the ring, read again where its window began. The cards replace the placeholders;
/// a read that came back empty leaves the row a descriptor and stalls it until the hold moves.
fn land_reload(source: &mut Src, page: &PageQuery, build: SourceBuild) -> bool {
    let Some(next) = build.shelves.into_iter().find(|shelf| shelf.is(&page.id, &page.key)) else { return false };
    if next.items.is_empty() {
        source.stalled.push((page.id.clone(), page.key.clone()));
        return false;
    }
    let Some(current) = source.last.as_mut().and_then(|build| build.shelves.iter_mut()
        .find(|shelf| shelf.is(&page.id, &page.key))) else { return false };
    if !current.released() { return false; }
    let mut next = next;
    next.items.truncate(MAX_SHELF_ITEMS);
    next.positions.truncate(MAX_SHELF_ITEMS);
    current.items = next.items;
    current.positions = next.positions;
    current.offset = next.offset;
    current.end = next.end;
    current.more = next.more;
    current.total = next.total;
    current.row = next.row;
    current.shown = 0;
    true
}

/// A refresh brings every row's first page. A row that had given its cards up keeps its descriptor
/// (window offset, ledger, the count it showed) and takes only the fresh total, so it returns where
/// it was.
fn carry_descriptors(old: &SourceBuild, new: &mut SourceBuild) {
    for shelf in &mut new.shelves {
        let Some(was) = find_shelf(&old.shelves, &shelf.hub_id, &shelf.key).filter(|was| was.released()) else { continue };
        shelf.items = Vec::new();
        shelf.positions = Vec::new();
        shelf.shown = was.shown;
        shelf.offset = was.offset;
        shelf.end = was.end;
        shelf.more = was.more;
        shelf.row = was.row.clone();
    }
}

/// The held range as the store keeps it: at most [`HOLD_ROWS`] rows, centred on what the screen
/// asked for when it asked for more.
fn clamp_hold((lo, hi): (usize, usize)) -> (usize, usize) {
    let hi = hi.max(lo);
    if hi - lo < HOLD_ROWS { return (lo, hi); }
    let lo = ((lo + hi) / 2).saturating_sub(HOLD_ROWS / 2);
    (lo, lo + HOLD_ROWS - 1)
}

/// The shelves that publish a row, in catalog order after the deck: `(source, shelf)` indices.
/// The single definition of that order, shared by the merge's output and the ring.
fn published_order(srcs: &[Src], pins: &[(ServerId, i64, bool)]) -> Vec<(usize, usize)> {
    let mut order = Vec::new();
    for (i, source) in srcs.iter().enumerate() {
        let Some(build) = &source.last else { continue };
        for (j, shelf) in build.shelves.iter().enumerate() {
            if shelf.released() || shelf.items.iter().any(|m| item_pinned(pins, m)) { order.push((i, j)); }
        }
    }
    order
}

/// Gives the cards of every row outside the held range up to its descriptor. `hubs` is the merge
/// that was just built from `srcs`; returns whether any row changed.
fn settle_ring(srcs: &mut [Src], pins: &[(ServerId, i64, bool)], hubs: &[HubRow], hold: (usize, usize)) -> bool {
    let order = published_order(srcs, pins);
    let deck = hubs.len().saturating_sub(order.len());
    let (lo, hi) = clamp_hold(hold);
    let mut changed = false;
    for (k, &(i, j)) in order.iter().enumerate() {
        let row = k + deck;
        if (lo..=hi).contains(&row) { continue; }
        let Some(shelf) = srcs[i].last.as_mut().and_then(|build| build.shelves.get_mut(j)) else { continue };
        // a row whose key cannot be read again keeps its cards: it has no way back
        if shelf.released() || !pages(&shelf.key) { continue; }
        shelf.shown = hubs[row].len;
        shelf.items = Vec::new();
        shelf.positions = Vec::new();
        changed = true;
    }
    changed
}

/// [`merge_with_scope`] with the ring applied: rows outside the held range give their cards up and
/// the catalog is built again from descriptors. The hero pool is the full merge's (it is built from
/// the rows a refresh brought, before they were released); its cards are kept in the catalog.
fn merge_held(srcs: &mut [Src], scope: &BrowseScope, hold: (usize, usize)) -> (HubBuild, bool) {
    let mut build = merge_with_scope(srcs, scope);
    let changed = settle_ring(srcs, &scope.pins, &build.1, hold);
    if changed {
        let full = std::mem::replace(&mut build, merge_with_scope(srcs, scope));
        keep_heroes(&full.0, &full.2, &mut build);
    }
    (build, changed)
}

/// The rows to read again: held, a descriptor, not stalled. Per source, in catalog order.
fn wanted_reloads(srcs: &[Src], pins: &[(ServerId, i64, bool)], deck: usize, hold: (usize, usize)) -> Vec<Vec<(String, String)>> {
    let (lo, hi) = clamp_hold(hold);
    let mut wanted = vec![Vec::new(); srcs.len()];
    for (k, (i, j)) in published_order(srcs, pins).into_iter().enumerate() {
        if !(lo..=hi).contains(&(k + deck)) { continue; }
        let Some(shelf) = srcs[i].last.as_ref().and_then(|build| build.shelves.get(j)) else { continue };
        if shelf.released() && !srcs[i].stalled.iter().any(|(id, key)| shelf.is(id, key)) {
            wanted[i].push((shelf.hub_id.clone(), shelf.key.clone()));
        }
    }
    wanted
}

/// Starts the reads for rows that came back into the ring. One row per source at a time, through
/// the fan-out gate, chosen on THIS tick from the rows held now: a row that left the ring before
/// its read started is dropped from the queue, so a key held through hundreds of rows asks for the
/// rows it stopped on and never for the ones it passed.
fn pump_reloads(state: &mut PmsState, adapter: &PmsAdapter, scope: &BrowseScope,
    launch: &mut dyn FnMut(HubRequest) -> bool) -> crate::stores::EndpointRefreshSet {
    let mut endpoints = crate::stores::EndpointRefreshSet::default();
    let deck = published_home(state).hubs.len().saturating_sub(published_order(&state.srcs, &scope.pins).len());
    let wanted = wanted_reloads(&state.srcs, &scope.pins, deck, state.hold);
    let mut srcs = std::mem::take(&mut state.srcs);
    let mut ask = false;
    for (source, want) in srcs.iter_mut().zip(&wanted) {
        if let Some(page) = source.page.as_ref().filter(|page| page.reload && !source.fetching) {
            if !want.iter().any(|(id, key)| *id == page.id && *key == page.key) {
                source.page = None;
                source.deferred = false;
            }
        }
        if source.page.is_some() || source.fetching || source.state != HubState::Ready { continue; }
        let Some((id, key)) = want.first() else { continue };
        let start = source.last.as_ref().and_then(|build| find_shelf(&build.shelves, id, key)).map_or(0, |shelf| shelf.offset);
        source.retry_n = 0;
        source.page = Some(PageQuery { id: id.clone(), key: key.clone(), start, before: false,
            hidden: hidden_sections(scope, source.sid), reload: true });
        ask = true;
    }
    if ask || srcs.iter().any(|source| source.deferred && source.page.as_ref().is_some_and(|page| page.reload)) {
        let gen = state.hub_gen;
        let mut turn = fanout_turn(&mut state.fanout, &srcs, source_waiting);
        for source in srcs.iter_mut().filter(|source| source.page.as_ref().is_some_and(|page| page.reload)) {
            if let Some(request) = kick_gated(&mut turn, gen, adapter, source, scope, &mut *launch) { endpoints.insert(request); }
        }
    }
    state.srcs = srcs;
    endpoints
}

/// `HubsCmd::Hold`: the screen holds rows `lo..=hi`. Rows that left the range give their cards up;
/// rows that entered it without cards are read at their windows.
fn run_hold(state: &mut PmsState, adapter: &PmsAdapter, scope: &BrowseScope, lo: usize, hi: usize,
    launch: &mut dyn FnMut(HubRequest) -> bool) -> crate::stores::EndpointRefreshSet {
    if state.hold == (lo, hi) { return Default::default(); }
    state.hold = (lo, hi);
    for source in &mut state.srcs { source.stalled.clear(); }
    let mut srcs = std::mem::take(&mut state.srcs);
    let (mut build, changed) = merge_held(&mut srcs, scope, state.hold);
    state.srcs = srcs;
    if changed {
        preserve_heroes(state, &mut build);
        commit(state, build);
    }
    pump_reloads(state, adapter, scope, launch)
}

// Scrolling an old Recently Added page must not change the banner's current selection.
fn preserve_heroes(state: &PmsState, build: &mut HubBuild) {
    let previous = published_home(state);
    keep_heroes(&previous.items, &previous.heroes, build);
}

/// Makes `heroes` (slots into `items`) the pool of `build`, adding to its catalog the cards it does
/// not hold.
fn keep_heroes(items: &[Arc<PmsMovie>], heroes: &[HeroSlot], build: &mut HubBuild) {
    build.2.clear();
    for hero in heroes {
        let item = &items[hero.idx];
        let idx = build.0.iter().position(|row| plx_plex::plex::same_item((row.sid, &row.rk), (item.sid, &item.rk)))
            .unwrap_or_else(|| { build.0.push(Arc::clone(item)); build.0.len() - 1 });
        build.2.push(HeroSlot { idx, source: hero.source.clone() });
    }
}

/// GET one source's hubs and project them. `None` = the request failed (transport, HTTP, or
/// parse), which is NOT the same as a server with nothing on it (`Some` with an empty build) —
/// that distinction is the whole reason an empty library reads as Ready and not as an error.
///
/// The client AND its `sid` are passed IN, captured at the spawn site: a worker must never ask
/// which server is current (`browse.rs` states the rule, and a server switch mid-fetch would
/// otherwise stamp these rows with the other machine's id — the one thing every `(sid, rk)`
/// comparison downstream then trusts). A `&'static Client` also pins the exact address this fetch
/// was aimed at even if the registry re-points that slot mid-request.
fn fetch_source(c: &plx_plex::plex::Client, sid: ServerId) -> Option<SourceBuild> {
    let mc = c.home_hubs(HUB_FETCH_COUNT)?;
    // The Continue Watching shelf comes from the DEDICATED hub (see `project`). Its failure fails
    // THIS SOURCE (`?`) — nothing of it commits and it retries on its own backoff. Losing the most
    // important shelf to a transient error would be worse than briefly showing the previous one.
    let cw = c.continue_watching(HUB_FETCH_COUNT)?;
    let mut build = project(&mc, &cw, sid);
    with_lookahead(&mut build, sid, |start, size| c.continue_watching_page(start as i64, size as i64));
    Some(build)
}

/// The first window is the merge of the servers' previews, which is the deck's order only if no
/// server holds a newer card past its preview: one more page per server makes it so. A failed read
/// leaves the preview, and the lane reads on when the user asks.
fn with_lookahead(build: &mut SourceBuild, sid: ServerId,
    list: impl FnMut(usize, usize) -> Option<plx_plex::plex::MediaContainer>) {
    if build.cw.is_empty() || build.lane.done { return; }
    build.lane.rows = build.cw.clone();
    let _ = deck::read_ahead(sid, &mut build.lane, MAX_SHELF_ITEMS, &[], list);
}

/// Project one source's `/hubs` + `/hubs/continueWatching` responses into its [`SourceBuild`].
/// Pure — no statics, no I/O, no knowledge of any other source; `sid` is the server the two
/// containers came from, stamped onto every row it builds.
fn project(
    mc: &plx_plex::plex::MediaContainer,
    cw: &plx_plex::plex::MediaContainer,
    sid: ServerId,
) -> SourceBuild {
    // need a poster to show it in a shelf
    let keep = |it: &plx_plex::plex::Metadata| {
        if !listable(&it.kind) { return None; }
        let m = parse_item(it, sid);
        (!m.title.is_empty() && !m.thumb.is_empty()).then_some(m)
    };
    let mut out = SourceBuild::default();

    // Continue Watching comes from the **dedicated** `/hubs/continueWatching` hub, and `/hubs`'s own
    // `home.continue` / `home.ondeck` pair is skipped entirely. It used to merge that pair by hand
    // (in-progress items unified with next-up episodes, deduped by ratingKey) to reproduce the
    // official-app row — the dedicated hub already IS that row, so the merge was reimplementing a
    // server-side answer.
    //
    // The reason it has to be the dedicated one, though, is `removeFromContinueWatching`: measured on
    // PMS 1.43.3, that action hides an item from `/hubs/continueWatching` and from `home.ondeck` but
    // **NOT** from `home.continue` (see `plex::Client::remove_from_continue_watching`). Built from the
    // pair, this shelf would keep drawing a card the server had been told to hide, and the context
    // menu's Remove row would look broken while the server had done exactly as asked.
    for hub in cw.hub.iter() {
        out.cw = hub
            .metadata
            .iter()
            .enumerate()
            .filter_map(|(position, it)| {
                keep(it).map(|m| CwItem {
                    last_viewed_at: it.last_viewed_at,
                    m: Arc::new(m),
                    position,
                })
            })
            .collect();
        if !out.cw.is_empty() {
            // The preview is the head of the listing behind `/hubs/continueWatching/items`; the
            // lane remembers where it ends so the deck can read on from there.
            let key = |it: Option<&plx_plex::plex::Metadata>| it.map_or_else(String::new, |it| clean(&it.rating_key));
            let more = hub.more || hub.total() > hub.metadata.len() || hub.metadata.len() >= HUB_FETCH_COUNT as usize;
            out.lane = deck::Lane::preview(&key(hub.metadata.first()), &key(hub.metadata.last()),
                hub.metadata.len(), hub.total(), !more);
            break; // the first hub that has anything in it IS the deck
        }
    }

    // Counted up front (not while looping) because `localized_hub_title`'s per-type "Recently
    // Added Movies" wording is only right for a `home.<type>.recent` hub that names exactly ONE
    // library — the moment PMS mints a second hub under the SAME identifier (one household with
    // two TV libraries: `home_keeps_recently_added_rows_for_two_same_type_libraries`), both need
    // the per-library "Recently Added in {library}" form to stay distinguishable, and neither
    // hub can tell that from itself alone.
    let mut hub_identifier_counts: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    for hub in &mc.hub {
        *hub_identifier_counts.entry(hub.hub_identifier.as_str()).or_insert(0) += 1;
    }

    for hub in &mc.hub {
        if hub.kind != "mixed" && !listable(&hub.kind) {
            continue;
        }
        if hub.hub_identifier == "home.continue" || hub.hub_identifier == "home.ondeck" {
            continue; // superseded by the dedicated hub above
        }
        let (positions, items): (Vec<usize>, Vec<Arc<PmsMovie>>) = hub.metadata.iter().enumerate()
            .filter_map(|(i, item)| keep(item).map(|item| (i, Arc::new(item)))).unzip();
        if items.is_empty() {
            continue;
        }
        let library = hub
            .metadata
            .iter()
            .find(|m| !m.library_section_title.is_empty())
            .map(|m| m.library_section_title.as_str())
            .unwrap_or("");
        let identifier_is_unique =
            hub_identifier_counts.get(hub.hub_identifier.as_str()).copied().unwrap_or(0) <= 1;
        if !plx_plex::plex::is_pageable_hub_key(&hub.key) && (hub.more || hub.total() > hub.metadata.len()) {
            if let Some(line) = unpaged_line(&hub.hub_identifier, &hub.key) { plx_base::eventlog::log(&line); }
        }
        out.shelves.push(Shelf {
            title: plx_plex::plex::hub_title::localized_hub_title(
                plx_plex::plex::hub_title::Scope::Home { library, identifier_is_unique },
                &hub.hub_identifier,
                &hub.title,
            ),
            hub_id: hub.hub_identifier.clone(),
            key: hub.key.clone(),
            items, positions,
            total: hub.total(),
            offset: 0,
            end: hub.metadata.len(),
            more: pages(&hub.key)
                && (hub.more || hub.total() > hub.metadata.len() || hub.metadata.len() >= HUB_FETCH_COUNT as usize),
            shown: 0,
            row: preview_row(&hub.hub_identifier, &hub.key),
        });
    }
    out
}

// ---- the merge: every source, one Home ----------------------------------------------------------

/// Divide `budget` between sources that each want `want[i]`, so that **no source can starve
/// another**.
///
/// This is the whole of the multi-server budget rule. Handing the budget out first-come (which is
/// what a single running total does, and what this module did while there was only ever one server)
/// means the first source spends it: `/hubs` promotes several rows per library, so a four-library
/// server alone can spend the whole card budget and a share behind it draws nothing at all.
///
/// It is water-filling, not a flat `budget / n`: the smallest demand is served first and what it
/// does not use is RE-DIVIDED among the rest, so a modest source costs nobody anything and two
/// greedy ones still split what is left evenly. A leftover pass in source order instead would hand
/// every unclaimed row to whoever came first, which is the starvation this exists to prevent
/// wearing a fairer name. Pure, so the rule is graded on the host rather than inferred from a
/// screenshot.
///
/// `pub` for `crate::person`, whose Movies/Shows shelves are the same merge one level down:
/// a prolific actor's films on the server you arrived through would otherwise fill the row and
/// leave the share behind it nothing — which is the bug that store exists to fix, re-created inside
/// it. One water-filling rule, not two.
pub fn allot(budget: usize, want: &[usize]) -> Vec<usize> {
    let n = want.len();
    let mut out = vec![0usize; n];
    let mut left = budget;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| want[i]); // stable: equal demands keep source order, our own first
    for (k, &i) in order.iter().enumerate() {
        // `.max(1)` so a budget smaller than the source count reaches as many sources as it can
        // rather than nobody; `.min(left)` keeps that honest once it runs out.
        let share = (left / (n - k)).max(1).min(left);
        out[i] = want[i].min(share);
        left -= out[i];
    }
    out
}

/// Merge every source's last good projection into the one catalog/hubs/pool triple the UI reads.
/// PURE — no statics, no I/O — which is what makes the ordering, the annotation and the budget
/// gradeable on the host.
///
/// The shape of Home, in order:
/// 1. **Continue Watching**, merged across every source and sorted by `lastViewedAt` descending, so
///    a borrowed item holds first position exactly when the owner watched it last. It carries NO
///    annotation (see [`HubRow::source`]). This is the official client's own shape: the owner's
///    screenshots show a friend's two films sitting BETWEEN their own three, in one row.
/// 2. **Every other shelf**, source by source in roster order — the owned server's first, then each
///    shared server's, contiguously, because adjacency is the grouping device.
///
/// A source that has never answered contributes nothing at all: no heading, no empty shelf, no
/// placeholder row. One that answered and has since failed keeps the shelves it last had, which is
/// the other half of the same rule — a transient failure must not blank a populated Home, and a
/// source that is really gone leaves the ROSTER, which is what drops its shelves.
#[cfg(any(test, feature = "test-support"))]
fn merge(srcs: &[Src]) -> HubBuild {
    merge_with_scope(srcs, &BrowseScope::standalone())
}

/// The sources that have answered, as `(handle, last projection)`, in roster order.
fn live_sources(srcs: &[Src]) -> Vec<(&str, &SourceBuild)> {
    srcs.iter()
        .filter_map(|s| s.last.as_ref().map(|b| (s.handle.as_str(), b)))
        .collect()
}

/// What each live source's shelves publish: every pinned card they hold, at most
/// [`MAX_SHELF_ITEMS`] per shelf. A shelf that gave its cards up to the ring holds none and
/// publishes placeholder slots instead (see [`Shelf::released`]).
fn publishable_shelves<'a>(
    live: &[(&str, &'a SourceBuild)],
    pins: &[(ServerId, i64, bool)],
) -> Vec<Vec<Vec<&'a Arc<PmsMovie>>>> {
    live.iter()
        .map(|(_, b)| {
            b.shelves
                .iter()
                .map(|sh| {
                    sh.items
                        .iter()
                        .filter(|m| item_pinned(pins, m))
                        .take(MAX_SHELF_ITEMS)
                        .collect()
                })
                .collect()
        })
        .collect()
}

/// Approximate resident bytes of a catalog: each card's struct plus the heap its strings own.
/// Counted per catalog entry, so a card two shelves share is counted twice — a ceiling on what the
/// catalog holds, which is the side a memory bound wants to err on.
fn catalog_bytes(cat: &[Arc<PmsMovie>]) -> usize {
    cat.iter()
        .map(|m| {
            std::mem::size_of::<PmsMovie>()
                + [
                    &m.title, &m.rating, &m.part, &m.thumb, &m.still, &m.art, &m.summary, &m.rk,
                    &m.vcodec, &m.acodec, &m.show_rk, &m.show_title, &m.aired,
                ]
                .iter()
                .map(|s| s.capacity())
                .sum::<usize>()
        })
        .sum()
}

fn merge_with_scope(srcs: &[Src], scope: &BrowseScope) -> HubBuild {
    let pins = &scope.pins;
    let live = live_sources(srcs);

    let mut new_cat: Vec<Arc<PmsMovie>> = Vec::new();
    let mut new_hubs: Vec<HubRow> = Vec::new();
    // Parallel to `new_cat`: the HANDLE of the source each row came from. The hero pool is the only
    // reader, and it needs the fact per ITEM rather than per shelf — the merged deck's own `source`
    // is empty by design, so a slot that took its handle from its shelf would attribute every
    // borrowed film in Continue Watching to nobody, and `own_items_first` would think our own
    // library already opened the door.
    let mut row_handle: Vec<&str> = Vec::new();

    // ---- 1. the merged deck ----
    // Once the deck is placed it is the merge of every server's lane window (`stores::deck`); before
    // that (a fixture that never settled) it is the previews, sorted.
    let lanes: Vec<&deck::Lane> = live.iter().map(|(_, b)| &b.lane).collect();
    let (mut cw, deck_offset, deck_more): (Vec<(&str, &CwItem)>, usize, bool) =
        if !lanes.is_empty() && lanes.iter().all(|lane| lane.placed) {
            let (cards, before, after) = deck::merge_deck(&lanes);
            let offset = if before { lanes.iter().map(|lane| lane.lo).sum::<usize>().max(1) } else { 0 };
            (cards.into_iter().map(|(i, c)| (live[i].0, c)).filter(|(_, c)| item_pinned(pins, &c.m)).collect(), offset, after)
        } else {
            let mut cw: Vec<(&str, &CwItem)> = live
                .iter()
                .flat_map(|(h, b)| b.cw.iter().map(move |c| (*h, c)))
                .filter(|(_, c)| item_pinned(pins, &c.m))
                .collect();
            // stable: equal timestamps keep source order, so the owned server wins a tie
            cw.sort_by(|a, b| b.1.last_viewed_at.cmp(&a.1.last_viewed_at));
            (cw, 0, false)
        };
    cw.truncate(MAX_SHELF_ITEMS);
    for (h, c) in &cw {
        new_cat.push(Arc::clone(&c.m));
        row_handle.push(h);
    }
    if !new_cat.is_empty() {
        new_hubs.push(HubRow {
            // the hub id the rest of the module matches on (hero-pool eligibility,
            // `hub_is_continue`), rather than the dedicated hub's own "continueWatching"
            title: plx_platform::i18n::msg::browse_home_continue_watching().to_string(),
            hub_id: "home.continue".to_string(),
            key: String::new(),
            source: String::new(),
            total: 0,
            start: 0,
            len: new_cat.len(),
            offset: deck_offset, more: deck_more,
        });
    }

    // ---- 2. every other shelf, grouped by source ----
    // Every shelf with something to show publishes a row; none is left off. A shelf that holds
    // cards publishes them (pin-filtered first, so an unpinned library's whole shelf disappears
    // rather than becoming an empty heading); a shelf that gave them up to the ring publishes as
    // many placeholder slots as it last showed, which carry its server so the row keeps its identity.
    let publishable = publishable_shelves(&live, pins);
    let sids: Vec<ServerId> = srcs.iter().filter(|s| s.last.is_some()).map(|s| s.sid).collect();

    for (i, (handle, b)) in live.iter().enumerate() {
        for (sh, items) in b.shelves.iter().zip(&publishable[i]) {
            let start = new_cat.len();
            if sh.released() {
                let slot = Arc::new(PmsMovie { sid: sids[i], ..PmsMovie::default() });
                for _ in 0..sh.shown {
                    new_cat.push(Arc::clone(&slot));
                    row_handle.push(handle);
                }
            } else {
                if items.is_empty() {
                    continue;
                }
                for m in items {
                    new_cat.push(Arc::clone(m));
                    row_handle.push(handle);
                }
            }
            new_hubs.push(HubRow {
                title: sh.title.clone(),
                hub_id: sh.hub_id.clone(),
                key: sh.key.clone(),
                source: (*handle).to_string(),
                total: sh.total,
                start,
                len: new_cat.len() - start,
                offset: sh.offset, more: sh.more,
            });
        }
    }

    // ---- 3. the rotating hero pool ----
    // Continue Watching items first, then Recently Added, deduped by the item's IDENTITY. Require
    // landscape `art` (the hero draws a full-bleed backdrop) and skip seasons (a bare "Season 1"
    // makes a poor billboard). Capped at HERO_MAX.
    let mut new_pool: Vec<HeroSlot> = Vec::new();
    for hub in &new_hubs {
        // Match on the locale-independent hubIdentifier, not the localized display title:
        // "home.continue" plus every Recently Added variant (home.movies.recent,
        // home.television.recent, promoted <type>.recentlyadded.<id>) all carry "recent".
        let eligible = hub.hub_id == "home.continue" || hub.hub_id.contains("recent");
        if !eligible {
            continue;
        }
        for idx in hub.start..hub.start + hub.len {
            if new_pool.len() >= HERO_MAX {
                break;
            }
            let m = &new_cat[idx];
            if m.art.is_empty() || m.kind == 2 {
                continue; // need landscape art; skip seasons
            }
            // dedup by the item's IDENTITY, not by its bare key: two shelves merged from two
            // servers can each contribute a different film numbered 1, and a bare-key dedup would
            // silently drop the second from the hero rotation.
            //
            // NB the pool holds `HeroSlot`s, so the index is `s.idx` — unit 12's hero-ordering work
            // and unit 3's identity work landed in this same expression from opposite directions.
            if new_pool.iter().any(|s| {
                plx_plex::plex::same_item((new_cat[s.idx].sid, &new_cat[s.idx].rk), (m.sid, &m.rk))
            }) {
                continue;
            }
            new_pool.push(HeroSlot {
                idx,
                source: row_handle[idx].to_string(),
            });
        }
    }
    // …and only now is the order decided: the pool is assembled shelf by shelf, so which server
    // opens the door is not knowable until every shelf has contributed.
    own_items_first(&mut new_pool);
    (new_cat, new_hubs, new_pool)
}

// ---- fetch state machine: PER SOURCE loading / ready / failed + the automatic-retry backoff ----
//
// Modelled on `browse.rs`'s page store, deliberately, because it already learned two of the three
// lessons this needed: a FAILED fetch must never overwrite a populated store (one wifi hiccup used
// to blank a whole grid permanently), and a fast-failing network must be held off by a countdown
// rather than re-spawning a worker every frame.
//
// The third only appears with a second server, and every piece of it was process-global before:
// **a verdict belongs to a SOURCE, not to Home.** One `?` chain meant a dead share aborted the whole
// build and nothing committed — on a cold boot, a whole-screen "Can't reach your Plex server" about
// a library that was answering perfectly well. One in-flight latch meant a share that takes eight
// seconds to time out held the owned server's retry behind it. One backoff meant the ladder a dead
// share had climbed to 30 s was the ladder every other source then waited on.

/// What a source's last fetch produced. Home's loading / empty / error read-out is a projection of
/// these ([`hub_state`] folds them): an empty catalog is only an empty *screen* when a fetch
/// actually succeeded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HubState {
    /// a fetch is in flight, or the first one hasn't run yet — nothing to show is not an answer
    Loading,
    /// the server answered; the catalog is whatever it says it is (possibly legitimately empty)
    Ready,
    /// the fetch failed — whatever this source last answered with is untouched and [`pump`] is
    /// counting down to the next automatic attempt for it alone
    Failed,
}

/// A source Home is built from, plus everything the fetch state machine knows about it.
/// Main-thread only, behind [`SRCS`].
struct Src {
    /// The registry slot every fetch for this source is issued through — and the id stamped onto
    /// every row it parses.
    sid: ServerId,
    /// Registry lifecycle currently represented by this slot. A slot id survives repoint, so the
    /// pointer and credential generation are part of the source identity too.
    client: Option<&'static plx_plex::plex::Client>,
    token_gen: u32,
    /// **The CREDIT** for this source — `plex::servers::owner_credit`'s answer, stamped onto every
    /// shelf and hero row this source contributes. `"friend"` for a borrowed server; **empty when
    /// there is nobody to credit**, which is our own server, the household's own server whichever
    /// Plex Home profile is watching, and a share plex.tv never named. Read from the registry at
    /// [`sync_roster`] time, never inside a worker.
    ///
    /// [`roster`] groups the uncredited ones first, so an empty string also orders Home. That is
    /// the right answer for the first two cases and the wrong one for the third; it was wrong for
    /// the third before this field held a credit too (see `docs/shared-servers.md` §13).
    handle: String,
    state: HubState,
    /// Single flight, PER SOURCE. Cleared only where a landing is taken, where the spawn was
    /// refused, or where an authoritative fetch has invalidated everything in flight — drop one of
    /// those and this source never fetches again for the rest of the session.
    fetching: bool,
    /// Assigned by every [`kick`] from the store-wide request sequence. A landing whose seq is not
    /// the latest is a superseded worker's and
    /// is dropped: [`HUB_GEN`] says "a different account or server set", this says "an older attempt
    /// at the same one", and only the pair rules out both double-applies.
    seq: u32,
    page: Option<PageQuery>,
    retry_s: f32,
    retry_n: u32,
    /// A fetch was wanted and the fan-out gate had no room for it. Stays set until the fetch starts,
    /// so the tick asks again; nothing is dropped.
    deferred: bool,
    /// Rows `(hub id, key)` whose reload failed for good while held; asked again only after the
    /// screen's hold moves.
    stalled: Vec<(String, String)>,
    /// The projection this source last ANSWERED with, kept across a failure. This is what makes a
    /// failure never blank a populated Home; `None` (never answered) is what makes a dead source
    /// contribute nothing at all — no heading, no empty shelf, no spinner row.
    last: Option<SourceBuild>,
}

impl Src {
    /// The request-state transition, independent of thread creation and network I/O. Mint only
    /// after admission to this source's single flight; the adapter receives the captured value.
    fn begin_request(
        &mut self,
        client: &'static plx_plex::plex::Client,
        generation: u32,
        mint: impl FnOnce() -> u32,
    ) -> Option<HubRequest> {
        if self.fetching { return None; }
        let request = HubRequest {
            gen: generation, seq: mint(), sid: self.sid,
            client: LandingClient::live(client), token_gen: client.token_gen(),
            page: self.page.clone(),
            lane: self.last.as_ref().filter(|build| build.lane.placed).map(|build| (build.lane.clone(), Vec::new())),
            window: self.page.as_ref().and_then(|page| self.last.as_ref().and_then(|build|
                find_shelf(&build.shelves, &page.id, &page.key)).cloned().map(|mut shelf| {
                    // A row returning to the ring reads forward from where its window began.
                    if page.reload { shelf.end = shelf.offset; }
                    shelf
                })),
            windows: self.last.as_ref().map_or_else(Vec::new, |build| build.shelves.iter()
                
                .filter(|shelf| pages(&shelf.key) && !shelf.released()
                    && (shelf.offset > 0 || shelf.end > HUB_FETCH_COUNT as usize))
                .map(|shelf| (PageQuery { id: shelf.hub_id.clone(), key: shelf.key.clone(), start: shelf.offset, before: false, hidden: Vec::new(), reload: false }, shelf.end, shelf.clone())).collect()),
        };
        self.client = Some(client);
        self.token_gen = request.token_gen;
        self.fetching = true;
        if self.page.is_none() { self.state = HubState::Loading; }
        self.retry_s = 0.0;
        self.seq = request.seq;
        Some(request)
    }

    fn new(sid: ServerId, handle: String) -> Src {
        let client = plx_plex::plex::client_for(sid);
        Src {
            sid,
            client,
            token_gen: client.map_or(0, |c| c.token_gen()),
            handle,
            state: HubState::Loading,
            fetching: false,
            seq: 0,
            page: None,
            retry_s: 0.0,
            retry_n: 0,
            deferred: false,
            stalled: Vec::new(),
            last: None,
        }
    }
}

fn refresh_src_lifecycle(s: &mut Src) -> bool {
    let client = plx_plex::plex::client_for(s.sid);
    let token_gen = client.map_or(0, |c| c.token_gen());
    let same = match (s.client, client) {
        (Some(a), Some(b)) => std::ptr::eq(a, b) && s.token_gen == token_gen,
        (None, None) => true,
        _ => false,
    };
    if same {
        return false;
    }
    s.client = client;
    s.token_gen = token_gen;
    s.fetching = false;
    s.page = None;
    // Retained cards may still paint during the new profile's fetch, but its request must
    // start at the preview rather than reuse the previous profile's listing positions.
    if let Some(build) = s.last.as_mut() {
        for shelf in build.shelves.iter_mut().filter(|shelf| pages(&shelf.key)) {
            shelf.items.truncate(HUB_FETCH_COUNT as usize);
            shelf.positions.truncate(HUB_FETCH_COUNT as usize);
            shelf.offset = 0;
            shelf.end = 0;
            shelf.more = false;
            shelf.row = preview_row(&shelf.hub_id, &shelf.key);
        }
    }
    s.state = HubState::Loading;
    s.retry_s = 0.0;
    s.retry_n = 0;
    s.seq = s.seq.wrapping_add(1);
    true
}

// The source table and the roster fingerprint (SRCS/SEEN/SEEN_FACTS) live as `PmsState` fields
// now — `state.srcs`/`state.seen`/`state.seen_facts` — rebuilt from the roster by
// `sync_roster_with_scope`; main-thread only, so no lock is needed.

/// One adapter resource and the logical identity under which this arrival knows it.
#[derive(Clone, Copy)]
struct LandingClient {
    /// Logical identity in the recording's process, preserved when replay binds another resource.
    instance: u32,
    resource: &'static plx_plex::plex::Client,
}

impl LandingClient {
    fn live(resource: &'static plx_plex::plex::Client) -> Self {
        Self { instance: resource.instance_gen(), resource }
    }
}

/// Everything a Home fetch needs from the main thread, captured before the adapter runs.
/// Completing it reads no current source, registry, generation or token state.
pub struct HubRequest {
    gen: u32,
    seq: u32,
    sid: ServerId,
    client: LandingClient,
    token_gen: u32,
    page: Option<PageQuery>,
    /// Each row that has moved off its preview: where to reload it, how far it had read, and the row
    /// itself (a sample or ledger row cannot be re-read at an offset, so the refresh keeps it).
    windows: Vec<(PageQuery, usize, Shelf)>,
    window: Option<Shelf>,
    /// The Continue Watching lane this source holds once the deck is placed on it, and the sections
    /// the user hid: a deck page reads from it, a refresh reloads it at its own offsets.
    lane: Option<(deck::Lane, Vec<i64>)>,
}

impl HubRequest {
    pub fn descriptor(&self) -> (u32, u32, u16, u32, u32) {
        (self.gen, self.seq, self.sid.raw(), self.client.instance, self.token_gen)
    }
    pub fn page_descriptor(&self) -> Option<serde_json::Value> {
        self.page.as_ref().map(|page| serde_json::json!({"id":page.id,"key":page.key,"start":page.start,"before":page.before,"hidden":page.hidden}))
    }
    fn fetch(&self) -> Option<SourceBuild> {
        let client = self.client.resource;
        match &self.page {
            Some(page) if page.id == DECK_ID => {
                let (mut lane, hidden) = self.lane.clone()?;
                let list = |start: usize, size: usize| client.continue_watching_page(start as i64, size as i64);
                let read = if page.before {
                    deck::read_behind(self.sid, &mut lane, deck::PAGE, &hidden, list, |keys| many_items(client, keys))
                } else {
                    deck::read_ahead(self.sid, &mut lane, deck::PAGE, &hidden, list)
                };
                read.ok()?;
                Some(SourceBuild { lane, ..SourceBuild::default() })
            }
            Some(page) => fetch_row(self.sid, page, self.window.as_ref(), 0,
                |start, size| self.client.resource.hub_items_paged(&page.key, start as i64, size as i64),
                |keys| many_items(self.client.resource, keys)),
            None => {
                let mut build = fetch_source(self.client.resource, self.sid)?;
                if let Some((old, hidden)) = &self.lane {
                    // A deck that has moved stays where it is: the lane is read again at its own
                    // offsets, which also brings the progress of its cards up to date.
                    let list = |start: usize, size: usize| client.continue_watching_page(start as i64, size as i64);
                    if let Ok(lane) = deck::reload(self.sid, old, hidden, list) { build.lane = lane; }
                }
                for (window, end, old) in &self.windows {
                    let Some(shelf) = build.shelves.iter_mut().find(|shelf| shelf.is(&window.id, &window.key)) else { continue };
                    if matches!(old.row.mode, paging::RowMode::Sample(_)) || old.row.ledger.as_ref().is_some_and(|ledger| ledger.active) {
                        // A random hub's sample is new on every read and a ledger's positions are not
                        // listing offsets, so neither can be reloaded where it stood. The row keeps
                        // its window, which is also what keeps the user's card where it is.
                        let title = shelf.title.clone();
                        *shelf = old.clone();
                        shelf.title = title;
                        continue;
                    }
                    // `/hubs` itself answered: a failed window reload keeps the first-page shelf.
                    let Some(mut refreshed) = fetch_page(self.client.resource, self.sid, window, *end)
                        .and_then(|page| page.shelves.into_iter().next()) else { continue };
                    if !refreshed.items.is_empty() {
                        refreshed.title = shelf.title.clone();
                        if let Some(mut ledger) = old.row.ledger.clone() {
                            let (rows, _) = shelf_window(&refreshed);
                            ledger.note(&rows);
                            refreshed.row.ledger = Some(ledger);
                        }
                        *shelf = refreshed;
                    }
                }
                Some(build)
            },
        }
    }
    fn complete(self, build: Option<SourceBuild>) -> Landing {
        Landing { gen: self.gen, seq: self.seq, sid: self.sid,
            client: Some(self.client), token_gen: self.token_gen, build }
    }
}

/// One source's finished (or failed) off-thread fetch. `build: None` deliberately carries no data,
/// so a failure can never be mistaken for "the server returned nothing".
#[derive(Clone)]
pub struct Landing {
    gen: u32,
    seq: u32,
    sid: ServerId,
    client: Option<LandingClient>,
    token_gen: u32,
    build: Option<SourceBuild>,
}
impl Landing {
    pub fn request_id(&self) -> u32 { self.seq }
}

// The retained Browse directory's semantic pin fingerprint as of the last merge. The field keeps
// its historical name because `pms::initial` records and restores it, but an owner-local section
// generation alone aliases independent Browse stores. Zero remains the empty standalone scope.
//
// The backoff ladder (`backoff_secs`: 2s, 4s, 8s, 16s, then 30s forever) and its ends live in
// `plex::retry` — the plaintext grant's upgrade retry steps the same ladder, and `plex` cannot name
// this module. Home's fetch and Browse's section hubs keep reading it through here.
pub use plx_plex::plex::retry::backoff_secs;
#[cfg(test)]
use plx_plex::plex::retry::RETRY_MIN_S;

/// Home's fetch state, folded from every source — what the loading / empty / error read-out reads.
///
/// **Any source answering makes Home answered**, and the total-failure read-out is reserved for the
/// case where every one of them failed. That is the whole point of the fold: "Can't reach your Plex
/// server", drawn because a friend's machine is asleep, is a lie about the library that is working.
/// No source at all (before the first install) is Loading — nothing to show is not an answer.
// Only `hubs_snapshot` and this module's own tests read the fold; nothing outside `pms.rs` names
// it, so it stays module-private — the D3 hardening this file's other mutators cannot get (a
// sibling of `stores`, not a child of it, so `pub` is the tightest visibility Rust allows
// them; see the doc above `edit_item`/`reset`/`tick` et al.).
fn hub_state(state: &PmsState) -> HubState {
    let s = &state.srcs;
    if s.iter().any(|x| x.state == HubState::Ready) {
        HubState::Ready
    } else if !s.is_empty() && s.iter().all(|x| x.state == HubState::Failed) {
        HubState::Failed
    } else {
        HubState::Loading
    }
}

/// Does this server feed **Home**? The design's one control, and the seam the pin store fills.
///
/// `pinned` is every pinned library's server, from `browse::pinned_libraries`. The rule is *not*
/// "is this server in that list": an EMPTY list means the pin store knows nothing yet, not that
/// nothing is pinned. `/library/sections` and `/hubs` land independently and asynchronously,
/// and the never-empty floor (the Home editor's draft refuses to unpin the last library —
/// `screens::onboard`'s `OnboardScreen::toggle_row` — and `plex::pins` applies the same floor to
/// a recorded selection)
/// forbids an empty pinned set — so "empty" can only mean "no
/// section has been discovered anywhere", and treating it as "nothing is pinned" would leave Home
/// with no sources at all on the frame it boots.
///
/// Pure, so the bootstrap rule is graded rather than observed on a television.
fn feeds_home(sid: ServerId, pinned: &[ServerId], known: &[ServerId]) -> bool {
    // **A server whose libraries we have not enumerated yet is UNDECIDED, not unpinned.**
    //
    // The pin is a decision about libraries; you cannot have decided against one nobody has
    // discovered. Section discovery runs on workers and may land after that source's shelves, so
    // on a fresh boot the share can be in the roster with no known sections yet. Testing
    // `pinned.contains` there excluded it from Home until you happened to visit the Library, which
    // is exactly how the owner found it: "it appeared on the home screen only after I watched the
    // library."
    //
    // **"Not enumerated" is no longer the same thing as "no answer", and that is what keeps this
    // rule honest now a friend's library defaults OFF.** While every granted library defaulted On,
    // undecided and pinned agreed and this cost nothing. They stopped agreeing when the first-run
    // route landed, and a Home hub fetch can beat that source's section worker — so the recorded
    // answer for a source with no rows in the section table is joined in from the retained
    // directory's favourite table and arrives here as an ordinary
    // `known`/`pinned` entry. What is left undecided is a library nobody has ever been ASKED
    // about, which is the case this rule was written for.
    //
    // The whole-set emptiness check below is the same rule one level up (nothing discovered
    // anywhere yet) and is kept for the boot frame before any source has answered.
    pinned.is_empty() || pinned.contains(&sid) || !known.contains(&sid)
}

/// The only Browse facts Home consumes. Controlled execution snapshots these from the Bridge's
/// retained directory. Standalone Home fixtures have no Browse owner and therefore no known pin
/// table; the normal unknown-library policy remains in force for them.
struct BrowseScope {
    sections_gen: u32,
    pins: Vec<(ServerId, i64, bool)>,
}

impl BrowseScope {
    fn standalone() -> Self {
        Self { sections_gen: 0, pins: Vec::new() }
    }

    fn retained(directory: crate::stores::browse::DirectoryView<'_>) -> Self {
        Self { sections_gen: directory.sections_gen(), pins: directory.favorite_sections().to_vec() }
    }

    /// Semantic pin-table identity. Browse generations are owner-local, so two independent stores
    /// may both report generation zero while naming different libraries. The replayed cache fields
    /// are fixed-width atomics; fold the actual `(server, section, pin)` input into that existing
    /// shape instead of adding a global owner selector.
    fn cache_key(&self) -> u32 {
        if self.pins.is_empty() {
            return 0;
        }
        let mut hash = 0x811c_9dc5u32;
        let mut fold = |bytes: &[u8]| {
            for byte in bytes {
                hash = (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193);
            }
        };
        fold(&self.sections_gen.to_le_bytes());
        fold(&(self.pins.len() as u64).to_le_bytes());
        for (sid, section, pinned) in &self.pins {
            fold(&sid.raw().to_le_bytes());
            fold(&section.to_le_bytes());
            fold(&[*pinned as u8]);
        }
        hash
    }
}

/// May this row appear on Home? **Per LIBRARY, which is the grain the switch offers.**
///
/// `/hubs` is a whole-SERVER request and answers with rows from every library on that server, so
/// without this the finest gate available was "does this server feed Home at all" — and unpinning
/// one library of a two-library server changed nothing at all on screen. Owner-reported.
///
/// Unknown is ALLOWED, in both directions: a row whose server sent no `librarySectionID`, and a
/// library the section table has not enumerated yet, both pass. The pin is a decision about
/// libraries we know about, and the alternative — hiding what we cannot classify — empties Home on
/// the frame it boots, which is the same mistake [`feeds_home`] documents one level up.
fn item_pinned(pins: &[(ServerId, i64, bool)], m: &PmsMovie) -> bool {
    if m.sec == 0 {
        return true; // the server said nothing about this row's library
    }
    match pins
        .iter()
        .find(|(sid, key, _)| *sid == m.sid && *key == m.sec)
    {
        Some((_, _, pinned)) => *pinned,
        None => true, // not enumerated yet
    }
}

/// The two server sets [`feeds_home`] takes, folded out of the retained directory's favourite
/// table in one pass: `(pinned, known)`. Separated so `feeds_home` stays pure and host-gradeable.
fn home_server_sets(pins: &[(ServerId, i64, bool)]) -> (Vec<ServerId>, Vec<ServerId>) {
    let (mut pinned, mut known) = (Vec::new(), Vec::new());
    for &(sid, _, is_pinned) in pins {
        if !known.contains(&sid) {
            known.push(sid);
        }
        if is_pinned && !pinned.contains(&sid) {
            pinned.push(sid);
        }
    }
    (pinned, known)
}

/// The sources Home is built from, in display order: our own servers first, then each share, each
/// group keeping registration order. The merge appends shelves in exactly this order and adjacency
/// is the grouping device, so "own first, then each shared server's, contiguously" is true by
/// construction rather than by convention.
///
/// The registry IS the granted roster — a server is in it only once plex.tv (or the
/// `plxnative-servers` dev trigger) handed us a token for it — and [`feeds_home`] is what narrows
/// the grant to a pin. The handle comes from the same place (`ServerFacts`), so nothing here has an
/// opinion about who a server belongs to that the Sources list does not share.
fn roster_with_scope(scope: &BrowseScope) -> Vec<(ServerId, String)> {
    let (pinned, known) = home_server_sets(&scope.pins);
    let mut own: Vec<(ServerId, String)> = Vec::new();
    let mut shared: Vec<(ServerId, String)> = Vec::new();
    for sid in plx_plex::plex::server_ids() {
        if plx_plex::plex::client_for(sid).is_none() || !feeds_home(sid, &pinned, &known) {
            continue;
        }
        let handle = plx_plex::plex::server_facts(sid)
            .map(|f| f.handle.clone())
            .unwrap_or_default();
        if handle.is_empty() {
            own.push((sid, handle));
        } else {
            shared.push((sid, handle));
        }
    }
    own.append(&mut shared);
    own
}

/// A cheap fingerprint of what [`roster`] would return, so [`sync_roster`] can skip the rebuild on
/// the frames — almost all of them — where nothing has changed.
///
/// **Two atomic loads and no allocation here.** The inputs are the registry's exact roster
/// generation and a semantic fingerprint of the retained pin table. Count was insufficient:
/// replacing active slot 1 with slot 2 leaves the same number and otherwise aliases the old roster
/// forever. A Browse generation was insufficient too: two independent owners legitimately start
/// at the same local generation while naming different libraries. `BrowseScope` already owns the
/// small pin snapshot this function folds, so the per-frame cache gate adds no table walk.
#[allow(dead_code)] // Standalone Home fixtures have no retained Browse directory.
fn roster_key() -> u64 {
    roster_key_with_scope(&BrowseScope::standalone())
}

fn roster_key_with_scope(scope: &BrowseScope) -> u64 {
    ((plx_plex::plex::server_roster_gen() as u64) << 32) | u64::from(scope.cache_key())
}

#[cfg(any(test, feature = "test-support"))]
fn remember_roster(state: &mut PmsState, scope: &BrowseScope) {
    state.seen = roster_key_with_scope(scope);
}

#[allow(dead_code)] // Retained for symmetry with `remember_roster`; no production caller today.
fn forget_roster(state: &mut PmsState) {
    state.seen = u64::MAX;
}

fn browse_scope_moved(state: &mut PmsState, scope: &BrowseScope) -> bool {
    let key = scope.cache_key();
    let moved = state.last_sections_gen != key;
    state.last_sections_gen = key;
    moved
}

fn adopt_browse_scope(state: &mut PmsState, scope: &BrowseScope) {
    state.last_sections_gen = scope.cache_key();
}

/// The other half of the fingerprint, kept as its OWN counter rather than folded into the 64 bits
/// above — three `u32`s do not fit in one `u64` without a truncation that would eventually alias
/// two states, and this whole mechanism exists to notice a change.
///
/// It is `plex::servers`' facts epoch: what the roster SAYS about a server, as opposed to which
/// servers there are. `Src::handle` is a copy of the "Shared by …" credit and `merge` stamps that
/// copy onto every shelf and hero row, so a credit re-graded by a roster refresh is exactly a
/// change this table must rebuild for — and the roster epoch alone cannot see one.
fn facts_key() -> u32 {
    plx_plex::plex::server_facts_gen()
}

/// Bring the source table in line with the roster: a surviving source keeps everything it has
/// (its state, its backoff, and the build it last answered with), a new one arrives Loading and is
/// picked up by the next [`pump`], and one that has left takes its shelves with it.
#[allow(dead_code)] // Standalone Home fixtures have no retained Browse directory.
fn sync_roster(state: &mut PmsState) {
    sync_roster_with_scope(state, &BrowseScope::standalone());
}

fn sync_roster_with_scope(state: &mut PmsState, scope: &BrowseScope) {
    let (k, fk) = (roster_key_with_scope(scope), facts_key());
    let scope_moved = browse_scope_moved(state, scope);
    // Both, and both swapped every time: a frame on which only one moved must still record the
    // other, or the next change to it reads as "unchanged" against a value from two epochs ago.
    let was_k = state.seen;
    let was_fk = state.seen_facts;
    state.seen = k;
    state.seen_facts = fk;
    if was_k == k && was_fk == fk && !scope_moved {
        return;
    }
    let want = roster_with_scope(scope);
    let mut srcs = std::mem::take(&mut state.srcs);
    let mut out: Vec<Src> = Vec::with_capacity(want.len());
    // A retained source whose CREDIT moved — `plex::servers::owner_credit`'s answer, which is what
    // the shelves and the hero pool were stamped with. Updating `Src::handle` alone left the built
    // rows saying the old thing until the next successful hub fetch, and an OFFLINE source never
    // has one: "keep the last good shelves" would then have preserved a wrong attribution for good.
    // The order can move with it (`roster` groups uncredited first), which the same re-merge fixes.
    let mut restamped = false;
    for (sid, handle) in want {
        // One slot, one source. The way a roster came to name a slot twice was `plex::register`
        // answering with `current()` when the table was full; that now answers `ServerId::UNSET`,
        // which resolves to nothing and never reaches this list. The guard stays because a second
        // `Src` sharing a sid is worse than the mistake that produced it: every landing resolves to
        // the first of them, so the other never un-latches its single flight and silently stops
        // fetching for good.
        if out.iter().any(|x| x.sid == sid) {
            continue;
        }
        match srcs.iter().position(|x| x.sid == sid) {
            Some(i) => {
                let mut keep = srcs.remove(i);
                restamped |= keep.handle != handle && keep.last.is_some();
                keep.handle = handle;
                // Preserve the last good shelves while the replacement lifecycle fetches, but
                // release and supersede the old single-flight so it cannot wedge this slot.
                refresh_src_lifecycle(&mut keep);
                out.push(keep);
            }
            None => out.push(Src::new(sid, handle)),
        }
    }
    // Whatever is left in `srcs` has left the roster — un-pinned, or a share plex.tv no longer
    // grants. A worker still out for one of them posts a landing for a sid this table no longer
    // holds, which `pump` drops.
    let dropped = srcs.iter().any(|x| x.last.is_some());
    state.srcs = out;
    if dropped || restamped || scope_moved {
        let (build, _) = merge_held(&mut state.srcs, scope, state.hold);
        commit(state, build);
    }
}

/// Install a finished merge — the whole post-mutation ritual, not just the stores.
///
/// Catalog, hub ranges and hero slots publish as one immutable snapshot (a half-applied catalog
/// once left a stale hero pool floating over emptied shelves). The generation moves on EVERY
/// publication, including optimistic edits and roster changes, so retained views can refresh.
/// Home's legacy focus self-clamps at its read accessors, and
/// `idle::invalidate` repaints a screen that may have settled — a shelf gaining or losing a card
/// has no spring behind it, so nothing else would report the change to the frame gate.
///
/// Those two used to be the CALLER's to remember, and the five commit sites did not agree: three
/// performed the pair, one legacy synchronous hub path reconciled only through a wrapper its other
/// caller bypassed, and [`reset`] did neither. What kept that last omission from being visible is that
/// `reset`'s one production caller routes away from the detail page first — not any property of
/// `reset`. A ritual every caller must repeat is the defect class, so it lives here, where a new
/// commit site cannot forget it and no site has to be checked against the others.
///
/// MAIN THREAD, and callers release the [`SRCS`] guard first: the re-selection re-enters this
/// module ([`index_of_rk`]) to walk the catalog just replaced.
fn commit(state: &mut PmsState, build: HubBuild) -> c_int {
    let (new_cat, new_hubs, new_pool) = build;
    let n = new_cat.len();
    state.published = Some(Arc::new(HomeCatalog { items: new_cat, hubs: new_hubs, heroes: new_pool }));
    state.catalog_gen = state.catalog_gen.wrapping_add(1);
    plx_machine::idle::invalidate();
    n as c_int
}

/// Record one source's success: it answers with this build from now on, and the backoff retires so
/// its next failure starts at the bottom of the ladder instead of inheriting a 30 s wait.
fn landed_ok(s: &mut Src, b: SourceBuild) {
    // The success twin of `landed_fail`'s line. Without it a source that fetched and answered was
    // indistinguishable in the log from one still in flight — "hubs: source 1 fetching" with
    // nothing after it says only that the worker started. The SLOT, never the handle (a plex.tv
    // username is the friend's, and the event log is what users send us).
    plx_base::eventlog::log(&format!(
        "hubs: source {} ok — {} shelves, {} in CW",
        s.sid.raw(),
        b.shelves.len(),
        b.cw.len()
    ));
    let mut b = b;
    if let Some(old) = s.last.as_ref() { carry_descriptors(old, &mut b); }
    s.last = Some(b);
    s.state = HubState::Ready;
    s.retry_n = 0;
    s.retry_s = 0.0;
}

/// Record one source's failure: keep whatever it last answered with and arm ITS next attempt.
fn landed_fail(s: &mut Src) -> crate::stores::EndpointRefresh {
    s.retry_n = s.retry_n.saturating_add(1);
    s.retry_s = backoff_secs(s.retry_n);
    if s.page.is_some() && s.retry_n >= 3 {
        if let Some(page) = s.page.take().filter(|page| page.reload) { s.stalled.push((page.id, page.key)); }
        s.page = None;
        s.retry_s = 0.0;
        plx_base::eventlog::log(&format!("hubs: source {} page failed after {} attempts", s.sid.raw(), s.retry_n));
        return crate::stores::EndpointRefresh { sid: s.sid };
    }
    if s.page.is_none() { s.state = HubState::Failed; }
    // the ONE line that says a dead source is dead ON PURPOSE and is coming back — without it the
    // whole recovery is invisible in the event log. The SLOT, never the handle: a plex.tv username
    // is the friend's, and the event log is what users send us.
    plx_base::eventlog::log(&format!(
        "hubs: source {} FAILED (attempt {}) — retrying in {:.0}s",
        s.sid.raw(),
        s.retry_n,
        s.retry_s
    ));
    // Retrying this Client can recover a transient outage, but not a network-topology change:
    // after Wi-Fi→LAN the same machine may need a different connection from its plex.tv Resource.
    // The application owns discovery. Return the request until source locks are released;
    // the caller propagates it alongside the unchanged store verdict.
    crate::stores::EndpointRefresh { sid: s.sid }
}

/// Step one source's retry countdown by `dt` seconds; true when its next attempt is due. Split out
/// so the ladder is testable without spawning a worker or touching a socket.
fn retry_due(s: &mut Src, dt: f32) -> bool {
    let left = s.retry_s - dt;
    s.retry_s = left.max(0.0);
    left <= 0.0
}

/// Spawn an off-thread fetch for ONE source (single flight); [`pump`] lands it. Every source takes
/// this path, including the primary during boot/profile activation: a blocking fetch on the SDL
/// loop would draw no frames while the loading spinner is supposed to be visible.
/// Keep request admission/state identical when another adapter holds the request instead of
/// launching a worker. The returned admission decision still controls latch release/backoff.
fn kick_with(gen: u32, adapter: &PmsAdapter, s: &mut Src, scope: &BrowseScope, launch: impl FnOnce(HubRequest) -> bool) -> Option<crate::stores::EndpointRefresh> {
    refresh_src_lifecycle(s);
    if s.fetching {
        return None; // one in flight already — its spinner is the honest answer
    }
    // CAPTURE AT THE SPAWN SITE. The worker is handed this server's own `&'static Client` and its
    // slot id; it never asks which server is current, and a slot re-pointed mid-request cannot
    // redirect a fetch that is already out (`plex::servers` leaks each client precisely so that
    // reference stays live).
    let Some(c) = plx_plex::plex::client_for(s.sid) else {
        return Some(landed_fail(s));
    };
    let Some(mut request) = s.begin_request(c, gen,
        || adapter.next_request.fetch_add(1, Ordering::Relaxed)) else { return None };
    for (window, _, _) in &mut request.windows { window.hidden = hidden_sections(scope, request.sid); }
    if let Some((_, hidden)) = &mut request.lane { *hidden = hidden_sections(scope, request.sid); }
    let sid = request.sid;
    let spawned = launch(request);
    if !spawned {
        // nothing will ever fill the mailbox (the thread limit refused us), so release the latch
        // here and back off — `pump` will try again on the ladder.
        s.fetching = false;
        Some(landed_fail(s))
    } else {
        plx_base::eventlog::log(&format!("hubs: source {} fetching (off-thread)", sid.raw()));
        None
    }
}

/// The live worker adapter. Replay must replace this operation, not skip `begin_request` and
/// thereby leave its recorded result with no matching in-flight state.
///
/// The claim ([`owed`]) is raised here, on the calling (main) thread, before the worker exists, and
/// dropped again if the OS refuses the spawn: a launcher that does not come through here never
/// raises it.
pub fn spawn_fetch(adapter: &Arc<PmsAdapter>, request: HubRequest) -> bool {
    // A test's refusal stands in for the OS refusing the thread, so it is taken AFTER the claim is
    // raised and must give it back, exactly as the real refusal does.
    #[cfg(any(test, feature = "test-support"))]
    let refused = REFUSE_FETCH_FOR_TEST.with(|flag| flag.get());
    #[cfg(not(any(test, feature = "test-support")))]
    let refused = false;
    #[cfg(any(test, feature = "test-support"))]
    let late = LATE_FETCH_FOR_TEST.with(|late| late.get())
        .map(|(extra, items)| (extra, items, adapter.takes.load(Ordering::SeqCst)));
    let worker_adapter = Arc::clone(adapter);
    adapter.owed.fetch_add(1, Ordering::SeqCst);
    let spawned = !refused && plx_base::task::spawn_small("hubs", move || {
        #[cfg(any(test, feature = "test-support"))]
        let build = match late {
            Some((extra, items, takes_at_spawn)) =>
                late_build_for_test(&worker_adapter, takes_at_spawn, extra, items),
            None => catch_unwind(std::panic::AssertUnwindSafe(|| request.fetch())).ok().flatten(),
        };
        #[cfg(not(any(test, feature = "test-support")))]
        let build = catch_unwind(std::panic::AssertUnwindSafe(|| request.fetch())).ok().flatten();
        // Outside the panic guard: every admitted worker answers, including a panicking fetch.
        worker_adapter.results.lock().unwrap_or_else(|e| e.into_inner()).push(request.complete(build));
    });
    if !spawned {
        adapter.owed.fetch_sub(1, Ordering::SeqCst);
    }
    spawned
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static LATE_FETCH_FOR_TEST: std::cell::Cell<Option<(u32, usize)>> = const { std::cell::Cell::new(None) };
}

/// Replace only the network fetch of every [`spawn_fetch`] this test thread makes inside `f` with
/// a worker that answers with `items` movies LATE: it posts only after the adapter's mailbox has
/// been taken at least once since the spawn (so a take that does not wait finds it empty, every
/// time), then yields `extra` more times so its post lands at a varying point of the poll loop of
/// a take that does. Both waits are counts, never clocks, and bounded.
#[cfg(any(test, feature = "test-support"))]
pub fn with_late_fetches_for_test<R>(extra: u32, items: usize, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<(u32, usize)>);
    impl Drop for Restore {
        fn drop(&mut self) { LATE_FETCH_FOR_TEST.with(|late| late.set(self.0)); }
    }
    let _restore = Restore(LATE_FETCH_FOR_TEST.with(|late| late.replace(Some((extra, items)))));
    f()
}

#[cfg(any(test, feature = "test-support"))]
fn late_build_for_test(adapter: &PmsAdapter, takes_at_spawn: u32, extra: u32, items: usize) -> Option<SourceBuild> {
    for _ in 0..50_000_000u64 {
        if adapter.takes.load(Ordering::SeqCst) > takes_at_spawn { break; }
        std::thread::yield_now();
    }
    for _ in 0..extra { std::thread::yield_now(); }
    Some(build_test(items))
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static REFUSE_FETCH_FOR_TEST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Replace only the OS spawn boundary on this test thread, including nested callers in stores.
#[cfg(any(test, feature = "test-support"))]
pub fn with_refused_fetches_for_test<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) { REFUSE_FETCH_FOR_TEST.with(|flag| flag.set(self.0)); }
    }
    let _restore = Restore(REFUSE_FETCH_FOR_TEST.with(|flag| flag.replace(true)));
    f()
}

/// Is this source waiting on a fetch the tick should start (its countdown aside)? Not Ready, a page
/// request pending, or a fetch the gate had no room for.
fn source_waiting(s: &Src) -> bool {
    (s.state != HubState::Ready || s.page.is_some() || s.deferred) && !s.fetching
}

/// Open a pump of the fan-out gate over `srcs`: the fetches out are the single-flight latches
/// held, and `wanting` picks the sources that want one now, in display order.
fn fanout_turn(fanout: &mut crate::stores::fanout::Fanout, srcs: &[Src],
    wanting: impl Fn(&Src) -> bool) -> crate::stores::fanout::FanoutTurn {
    let running = srcs.iter().filter(|s| s.fetching).count();
    let wanting: Vec<usize> = srcs.iter().filter(|s| !s.fetching && wanting(s))
        .map(|s| s.sid.raw() as usize).collect();
    fanout.begin(running, &wanting)
}

/// [`kick_with`] behind the fan-out gate. A source that is out already is not asking; one the gate
/// has no room for is marked [`Src::deferred`] and kicked by a later tick.
fn kick_gated(turn: &mut crate::stores::fanout::FanoutTurn, gen: u32, adapter: &PmsAdapter, s: &mut Src,
    scope: &BrowseScope, launch: &mut dyn FnMut(HubRequest) -> bool) -> Option<crate::stores::EndpointRefresh> {
    if s.fetching {
        return None;
    }
    if !turn.admit(s.sid.raw() as usize) {
        s.deferred = true;
        return None;
    }
    s.deferred = false;
    kick_with(gen, adapter, s, scope, launch)
}

/// The Retry control's kick: try every source again NOW, from the bottom of the ladder — a person
/// who asks for it should never be made to sit out a 30-second automatic wait. A no-op for any
/// source whose fetch is already in flight; a pending page request is re-sent by the same kick.
/// Behind the fan-out gate: the sources it has no room for are kicked by the following ticks.
fn retry_now_gated(turn: &mut crate::stores::fanout::FanoutTurn, gen: u32, adapter: &PmsAdapter, s: &mut Src,
    scope: &BrowseScope, launch: &mut dyn FnMut(HubRequest) -> bool) -> Option<crate::stores::EndpointRefresh> {
    s.retry_n = 0;
    s.retry_s = 0.0;
    kick_gated(turn, gen, adapter, s, scope, launch)
}

/// Move the worker mailbox into one owned batch. No source or catalog state changes here.
///
/// Releases the claim ([`owed`]) for every landing it moves out, in the same call: dump mode's
/// `take_all_owed` reads the claim after each take. Saturating, because a test can queue a
/// landing straight into the mailbox without a spawn.
pub fn take_landings(adapter: &PmsAdapter) -> Vec<Landing> {
    let taken = std::mem::take(&mut *adapter.results.lock().unwrap_or_else(|e| e.into_inner()));
    let n = u32::try_from(taken.len()).unwrap_or(u32::MAX);
    if n != 0 {
        let _ = adapter.owed.try_update(Ordering::SeqCst, Ordering::SeqCst, |owed| Some(owed.saturating_sub(n)));
    }
    #[cfg(any(test, feature = "test-support"))]
    adapter.takes.fetch_add(1, Ordering::SeqCst);
    taken
}

/// Apply one explicitly supplied batch through the live landing rules — land finished fetches,
/// then count each source down to its next automatic attempt. The supplier runs after roster
/// reconciliation, exactly where the live mailbox used to be drained. Keeping it separate lets an
/// adapter observe or substitute arrivals without a second state-application path. This is NOT an
/// offline replay mode: retry scheduling and worker spawning remain live.
#[cfg(any(test, feature = "test-support"))]
fn pump_with_landings(state: &mut PmsState, adapter: &Arc<PmsAdapter>, dt: f32,
    take: impl FnOnce() -> Vec<Landing>) -> crate::stores::EndpointRefreshSet {
    step_landings(state, adapter, Some(dt), take)
}

/// Test-only compatibility tick for fixtures without a retained directory.
#[cfg(any(test, feature = "test-support"))]
pub fn tick(state: &mut PmsState, adapter: &Arc<PmsAdapter>, dt: f32) -> crate::stores::EndpointRefreshSet {
    // Same test-only catalog guard as `request_refetch_hubs` — the catalog is `PmsState`, a
    // field of the per-`Bridge` `HubsStore`, not a crate-global; see `lib.rs::testlock` and D5.
    #[cfg(any(test, feature = "test-support"))]
    plx_base::testlock::assert_held("the pms hub catalog (tick)");
    pump_with_landings(state, adapter, dt, Vec::new)
}

/// The owned store's tick never consumes the worker mailbox. Arrivals are delivered separately
/// by the dispatcher; a worker finishing during its drain belongs to the next frame's ingest.
pub fn tick_with_directory(
    state: &mut PmsState,
    adapter: &Arc<PmsAdapter>,
    dt: f32,
    directory: crate::stores::browse::DirectoryView<'_>,
) -> crate::stores::EndpointRefreshSet {
    #[cfg(any(test, feature = "test-support"))]
    plx_base::testlock::assert_held("the pms hub catalog (owned tick)");
    let mut launch = |r| spawn_fetch(adapter, r);
    step_landings_with_scope(state, adapter, Some(dt), Vec::new, &BrowseScope::retained(directory),
        &mut launch)
}

/// An addressed arrival may update the catalog behind another page, but must not advance retry
/// timers or start a new hubs fetch there. The visible Home alone owes the store's tick.
#[cfg(any(test, feature = "test-support"))]
fn apply_landing(state: &mut PmsState, adapter: &Arc<PmsAdapter>, landing: &Landing) -> crate::stores::EndpointRefreshSet {
    #[cfg(any(test, feature = "test-support"))]
    plx_base::testlock::assert_held("the pms hub catalog (apply_landing)");
    step_landings(state, adapter, None, || vec![landing.clone()])
}

/// Test-only compatibility landing for fixtures without a retained directory.
#[cfg(any(test, feature = "test-support"))]
pub fn land(state: &mut PmsState, adapter: &Arc<PmsAdapter>, landing: &Landing) -> crate::stores::StoreOutcome {
    let before = state.catalog_gen;
    let endpoints = apply_landing(state, adapter, landing);
    crate::stores::StoreOutcome { changed: state.catalog_gen != before, endpoints }
}

/// Apply one addressed landing under the same retained directory policy the frame publishes.
pub fn land_with_directory(
    state: &mut PmsState,
    adapter: &Arc<PmsAdapter>,
    landing: &Landing,
    directory: crate::stores::browse::DirectoryView<'_>,
) -> crate::stores::StoreOutcome {
    let before = state.catalog_gen;
    let mut launch = |r| spawn_fetch(adapter, r);
    let endpoints = step_landings_with_scope(state, adapter, None, || vec![landing.clone()],
        &BrowseScope::retained(directory), &mut launch);
    crate::stores::StoreOutcome { changed: state.catalog_gen != before, endpoints }
}

/// `stores::hubs::HubsStore`'s one door onto every [`HubsCmd`](crate::stores::hubs::HubsCmd) (D3):
/// the match used to live in `stores/hubs.rs::run`, calling four `pub` mutators across the
/// module boundary. Relocating the match here is what lets those four go private.
#[cfg(any(test, feature = "test-support"))]
pub fn run(state: &mut PmsState, adapter: &Arc<PmsAdapter>, cmd: crate::stores::hubs::HubsCmd) -> crate::stores::StoreOutcome {
    run_with_scope(state, adapter, cmd, &BrowseScope::standalone())
}

pub fn run_with_directory(
    state: &mut PmsState,
    adapter: &Arc<PmsAdapter>,
    cmd: crate::stores::hubs::HubsCmd,
    directory: crate::stores::browse::DirectoryView<'_>,
) -> crate::stores::StoreOutcome {
    run_with_scope(state, adapter, cmd, &BrowseScope::retained(directory))
}

fn run_with_scope(
    state: &mut PmsState,
    adapter: &Arc<PmsAdapter>,
    cmd: crate::stores::hubs::HubsCmd,
    scope: &BrowseScope,
) -> crate::stores::StoreOutcome {
    use crate::stores::hubs::HubsCmd;
    match cmd {
        paging @ (HubsCmd::Page { .. } | HubsCmd::CancelPage { .. }) => {
            let mut launch = |r| spawn_fetch(adapter, r);
            let before = state.catalog_gen;
            let endpoints = run_page_cmd(state, adapter, scope, paging, &mut launch);
            crate::stores::StoreOutcome { changed: before != state.catalog_gen, endpoints }
        }
        HubsCmd::RefetchHubs => {
            let mut launch = |r| spawn_fetch(adapter, r);
            crate::stores::StoreOutcome {
                changed: true,
                endpoints: request_refetch_hubs_with_scope(state, adapter, scope, &mut launch),
            }
        }
        HubsCmd::Hold { lo, hi } => {
            let mut launch = |r| spawn_fetch(adapter, r);
            let before = state.catalog_gen;
            let endpoints = run_hold(state, adapter, scope, lo, hi, &mut launch);
            crate::stores::StoreOutcome { changed: before != state.catalog_gen, endpoints }
        }
        HubsCmd::Retry => {
            let mut launch = |request| spawn_fetch(adapter, request);
            controlled_work_with_scope(state, adapter, Some(cmd), 0.0, scope, &mut launch)
        },
        HubsCmd::EditItem { sid, rk, edit } => crate::stores::StoreOutcome::changed(
            edit_item_with_scope(state, sid, &rk, edit, scope)),
        HubsCmd::Reset => {
            reset_with_scope(state, adapter, scope);
            crate::stores::StoreOutcome::changed(true)
        }
    }
}

/// Test-only compatibility shape for bootstrap fixtures that do not retain a directory.
#[cfg(any(test, feature = "test-support"))]
pub fn controlled_work(state: &mut PmsState, adapter: &Arc<PmsAdapter>, cmd: Option<crate::stores::hubs::HubsCmd>, dt: f32,
    launch: &mut dyn FnMut(HubRequest) -> bool) -> crate::stores::StoreOutcome {
    controlled_work_with_scope(state, adapter, cmd, dt, &BrowseScope::standalone(), launch)
}

pub fn controlled_work_with_directory(state: &mut PmsState, adapter: &Arc<PmsAdapter>, cmd: Option<crate::stores::hubs::HubsCmd>, dt: f32,
    directory: crate::stores::browse::DirectoryView<'_>,
    launch: &mut dyn FnMut(HubRequest) -> bool) -> crate::stores::StoreOutcome {
    controlled_work_with_scope(state, adapter, cmd, dt, &BrowseScope::retained(directory), launch)
}

fn controlled_work_with_scope(state: &mut PmsState, adapter: &Arc<PmsAdapter>, cmd: Option<crate::stores::hubs::HubsCmd>, dt: f32,
    scope: &BrowseScope,
    launch: &mut dyn FnMut(HubRequest) -> bool) -> crate::stores::StoreOutcome {
    use crate::stores::hubs::HubsCmd;
    let before = state.catalog_gen;
    let command = cmd.as_ref().is_some_and(|cmd| !cmd.is_paging());
    let endpoints = match cmd {
        Some(paging) if paging.is_paging() => run_page_cmd(state, adapter, scope, paging, launch),
        Some(HubsCmd::RefetchHubs) => request_refetch_hubs_with_scope(state, adapter, scope, launch),
        Some(HubsCmd::Hold { lo, hi }) => run_hold(state, adapter, scope, lo, hi, launch),
        Some(HubsCmd::Retry) => {
            let gen = state.hub_gen;
            let mut srcs = std::mem::take(&mut state.srcs);
            let mut endpoints = crate::stores::EndpointRefreshSet::default();
            let mut turn = fanout_turn(&mut state.fanout, &srcs, |_| true);
            for source in srcs.iter_mut() {
                if let Some(request) = retry_now_gated(&mut turn, gen, adapter, source, scope, launch) { endpoints.insert(request); }
            }
            state.srcs = srcs;
            endpoints
        }
        Some(HubsCmd::Reset) => {
            reset_with_scope(state, adapter, scope);
            crate::stores::EndpointRefreshSet::default()
        }
        Some(other) => return run_with_scope(state, adapter, other, scope),
        None => step_landings_with_scope(state, adapter, Some(dt), Vec::new, scope, launch),
    };
    crate::stores::StoreOutcome { changed: command || before != state.catalog_gen, endpoints }
}

#[cfg(any(test, feature = "test-support"))]
fn step_landings(state: &mut PmsState, adapter: &Arc<PmsAdapter>, dt: Option<f32>, take: impl FnOnce() -> Vec<Landing>) -> crate::stores::EndpointRefreshSet {
    let mut launch = |r| spawn_fetch(adapter, r);
    step_landings_with(state, adapter, dt, take, &mut launch)
}

#[cfg(any(test, feature = "test-support"))]
fn step_landings_with(state: &mut PmsState, adapter: &Arc<PmsAdapter>, dt: Option<f32>, take: impl FnOnce() -> Vec<Landing>,
    launch: &mut dyn FnMut(HubRequest) -> bool) -> crate::stores::EndpointRefreshSet {
    step_landings_with_scope(state, adapter, dt, take, &BrowseScope::standalone(), launch)
}

fn step_landings_with_scope(state: &mut PmsState, adapter: &PmsAdapter, dt: Option<f32>, take: impl FnOnce() -> Vec<Landing>,
    scope: &BrowseScope,
    launch: &mut dyn FnMut(HubRequest) -> bool) -> crate::stores::EndpointRefreshSet {
    let mut endpoints = crate::stores::EndpointRefreshSet::default();
    sync_roster_with_scope(state, scope);
    let landed = take();
    let any_landed = !landed.is_empty();
    let cur = state.hub_gen;
    let mut srcs = std::mem::take(&mut state.srcs);
    let mut dirty = false;
    let mut paged = false;
    let mut refreshed = false;
    for l in landed {
        // A landing from before the last authoritative fetch describes a server (or an account) we
        // have since moved off, and one whose seq has been superseded describes an attempt this
        // source has already replaced. Either is dropped whole — neither committed nor blamed.
        let Some(s) = srcs.iter_mut().find(|s| s.sid == l.sid) else {
            continue; // its source left the roster while it was out
        };
        let lifecycle_matches = l.client.is_none_or(|client| {
            plx_plex::plex::client_for(l.sid)
                .is_some_and(|now| std::ptr::eq(now, client.resource) && now.token_gen() == l.token_gen)
        });
        if l.gen != cur || l.seq != s.seq || !lifecycle_matches {
            if l.gen == cur && l.seq == s.seq && !lifecycle_matches {
                s.fetching = false;
                s.page = None;
                s.state = HubState::Loading;
                s.retry_s = 0.0;
            }
            continue;
        }
        s.fetching = false;
        match l.build {
            Some(b) => {
                if let Some(page) = s.page.take() {
                    dirty |= if page.id == DECK_ID { land_lane(s, b) } else if page.reload { land_reload(s, &page, b) } else { land_page(s, &page, b) };
                    paged = true;
                    s.retry_n = 0;
                    s.retry_s = 0.0;
                } else {
                    landed_ok(s, b);
                    dirty = true;
                    refreshed = true;
                }
            }
            None => { endpoints.insert(landed_fail(s)); }
        }
    }
    if state.deck_ask.as_ref().is_some_and(|ask| deck_ready(ask, &srcs)) {
        if let Some(ask) = state.deck_ask.take() { advance_deck(&mut srcs, ask.before); }
        dirty = true;
        paged = true;
    }
    // Anything but Ready with nothing in flight is a state only a fetch can leave: Failed (with a
    // backoff owed) or a Loading whose worker landed stale and was dropped — the latter owes
    // nothing, so it re-kicks on the spot rather than wedging that source on a spinner forever.
    if let Some(dt) = dt {
        let mut turn = fanout_turn(&mut state.fanout, &srcs, |s| source_waiting(s) && s.retry_s - dt <= 0.0);
        for s in srcs.iter_mut() {
            if source_waiting(s) && retry_due(s, dt) {
                if let Some(request) = kick_gated(&mut turn, cur, adapter, s, scope, &mut *launch) { endpoints.insert(request); }
            }
        }
    }
    // …and re-merge when the SECTION TABLE moves, not only when a build lands. `feeds_home` reads
    // the pinned set, which is derived from that table — so both of the ways the set changes were
    // invisible to Home before this:
    //
    //   * a share's sections are discovered on a WORKER (`browse::maybe_discover`), strictly after
    //     the boot fetch. Until they land the share is in the roster but has no pinned library, so
    //     the merge that ran on its hub landing excluded it — and nothing re-ran.
    //   * a pin toggled by hand (now bumping the same generation).
    //
    // `merge` is pure over the builds each source already answered with: no request, no allocation
    // beyond the rebuilt catalog. Cheap enough to run on a generation change rather than to try to
    // predict which changes matter.
    let scope_moved = browse_scope_moved(state, scope);
    if dirty || scope_moved { settle_deck(&mut srcs, refreshed); }
    let build = (dirty || scope_moved).then(|| {
        let t0 = std::time::Instant::now();
        let (mut build, _) = merge_held(&mut srcs, scope, state.hold);
        if paged && !refreshed && !scope_moved { preserve_heroes(state, &mut build); }
        let took = t0.elapsed();
        (build, took)
    });
    state.srcs = srcs;
    if any_landed {
        // A landing that COMMITS repaints from inside `commit`; this is the one that does not —
        // a failure rewrites no shelf but does change the status caption, under a Home screen that
        // may have gone idle with nothing else on it to move.
        plx_machine::idle::invalidate();
    }
    if let Some((build, took)) = build {
        let bytes = catalog_bytes(&build.0);
        let n = commit(state, build);
        // The harness (`tests/mock_fps.py`) reads the first two numbers; new fields go AFTER them.
        plx_base::eventlog::log(&format!(
            "hubs: landed — {n} items, {} shelves (merge {} us, catalog ~{} KB)",
            hub_count(state),
            took.as_micros(),
            bytes / 1024
        ));
    }
    endpoints.merge(pump_reloads(state, adapter, scope, launch));
    endpoints
}

/// A source that has answered with `n` placeholder rows in one shelf (test fixture). Only the SHAPE
/// is real — `project` would have dropped these rows for having no title/poster; what the tests
/// using it assert is the landing/merge bookkeeping, which never looks inside a row.
///
/// Two fields ARE filled, and both because the hero pool reads them: `art`, since `merge` skips a
/// row with no landscape artwork (it would make a blank billboard), and a distinct `rk` per row,
/// since the pool dedups by item IDENTITY and n rows sharing the empty key are ONE film to it. A
/// fixture of bare defaults therefore committed shelves with an EMPTY pool — a Home that has
/// content but cannot page — and the Home pager test needs somewhere to page to. A fixture
/// that cannot express the app's ordinary state quietly limits what can be tested through it.
#[cfg(any(test, feature = "test-support"))]
fn build_test(n: usize) -> SourceBuild {
    SourceBuild {
        cw: Vec::new(),
        lane: Default::default(),
        shelves: vec![Shelf {
            title: "Continue Watching".into(),
            hub_id: "home.continue".into(),
            key: String::new(),
            items: (0..n)
                .map(|i| Arc::new(PmsMovie {
                    rk: (i + 1).to_string(),
                    // One backdrop per item, as a real catalog has: a test that asks WHICH
                    // backdrop was requested (Home's neighbour preload) needs them told apart.
                    art: format!("/art/{}", i + 1),
                    ..PmsMovie::default()
                }))
                .collect(),
            total: 0, offset: 0, end: 0, more: false, shown: 0, positions: (0..n).collect(), row: Default::default(),
        }],
    }
}

/// Test hook: put the store in a known place — one source in `state`, having answered with `items`
/// rows in one shelf. Home's read-out is a pure projection of that pair, and the states a host test
/// cannot reach for real (a live server answering, or refusing) are exactly the ones worth pinning.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_for_test(state: &mut PmsState, adapter: &Arc<PmsAdapter>, items: usize, hub_state: HubState) {
    plx_base::testlock::assert_held("the pms hub catalog (seed_for_test)");
    seed_with_scope_for_test(state, adapter, ServerId::UNSET, items, hub_state, &BrowseScope::standalone());
}

/// Seed a Hubs source that belongs to a real retained Browse directory. Full Bridge fixtures use
/// this instead of installing an `UNSET` source that owner-scoped roster reconciliation must drop.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_for_directory_test(
    state: &mut PmsState,
    adapter: &Arc<PmsAdapter>,
    sid: ServerId,
    items: usize,
    hub_state: HubState,
    directory: crate::stores::browse::DirectoryView<'_>,
) {
    plx_base::testlock::assert_held("the pms hub catalog (seed_for_directory_test)");
    assert!(directory.sections().iter().any(|section| section.sid == Some(sid)),
        "a directory-scoped Hubs fixture requires its server in the retained Browse directory");
    seed_with_scope_for_test(state, adapter, sid, items, hub_state, &BrowseScope::retained(directory));
}

/// Two-library Home fixture for the application-boundary watch-state regression. Both rows remain
/// in the source projection; the retained directory alone decides which one is published.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_two_library_home_for_test(
    state: &mut PmsState,
    sid: ServerId,
    directory: crate::stores::browse::DirectoryView<'_>,
) {
    plx_base::testlock::assert_held("the two-library pms home fixture");
    let sections = directory.sections();
    assert!(
        sections.len() >= 2 && sections[..2].iter().all(|section| section.sid == Some(sid)),
        "the two-library Home fixture requires two sections on its server"
    );
    let item = |section: &crate::stores::browse::SectionView, rk: &str| Arc::new(PmsMovie {
        sid,
        sec: section.key,
        rk: rk.into(),
        title: rk.into(),
        thumb: "/t.jpg".into(),
        art: "/a.jpg".into(),
        ..Default::default()
    });
    let mut source = Src::new(sid, String::new());
    source.state = HubState::Ready;
    source.last = Some(SourceBuild {
        cw: Vec::new(),
        lane: Default::default(),
        shelves: vec![Shelf {
            title: "Recent".into(),
            hub_id: "home.movies.recent".into(),
            key: String::new(),
            items: vec![item(&sections[0], "alpha"), item(&sections[1], "beta")],
            total: 0, offset: 0, end: 0, more: false, shown: 0, positions: Vec::new(), row: Default::default(),
        }],
    });
    let scope = BrowseScope::retained(directory);
    let sources = vec![source];
    let build = merge_with_scope(&sources, &scope);
    state.srcs = sources;
    remember_roster(state, &scope);
    state.seen_facts = facts_key();
    adopt_browse_scope(state, &scope);
    commit(state, build);
}

#[cfg(any(test, feature = "test-support"))]
fn seed_with_scope_for_test(state: &mut PmsState, adapter: &Arc<PmsAdapter>, sid: ServerId, items: usize, hub_state: HubState, scope: &BrowseScope) {
    reset_with_scope(state, adapter, scope);
    let handle = plx_plex::plex::server_facts(sid)
        .map(|facts| facts.handle.clone())
        .unwrap_or_default();
    let mut s = Src::new(sid, handle);
    s.state = hub_state;
    if items > 0 {
        let mut build = build_test(items);
        for shelf in &mut build.shelves {
            for item in &mut shelf.items {
                Arc::make_mut(item).sid = sid;
            }
        }
        s.last = Some(build);
    }
    let srcs = vec![s];
    let build = merge_with_scope(&srcs, scope);
    state.srcs = srcs;
    // Leave `sync_roster` believing this exact scope is up to date. For a standalone fixture that
    // preserves the synthetic source against an empty registry; for an owner-bound fixture it
    // preserves the source whose sid and retained directory were supplied together. BOTH halves
    // of the fingerprint matter, or the facts epoch alone reads as a change and rebuilds anyway.
    remember_roster(state, scope);
    state.seen_facts = facts_key();
    commit(state, build);
}

/// Drop everything and re-arm the fetch — the identity-change twin of `BrowseCmd::Reset`, called
/// from the same identity boundary. Now that a failed fetch KEEPS the previous build,
/// a profile switch whose fetch fails would otherwise leave the previous user's shelves on screen;
/// this is the one place that must still wipe them.
///
/// Private since D3's follow-up: `app/bridge.rs` and `app/recorder.rs` (nine `#[cfg(test)] mod
/// tests` call sites between them) were the last two direct callers, both now routed through
/// `HubsStore::run`/`run_with_directory` (`HubsCmd::Reset`) — `HubsCmd` already had the variant
/// and `pms::run` already matched it, so closing this was a caller-site swap alone, no new enum
/// surface.
#[cfg(test)]
fn reset(state: &mut PmsState, adapter: &Arc<PmsAdapter>) {
    reset_with_scope(state, adapter, &BrowseScope::standalone());
}

fn reset_with_scope(state: &mut PmsState, adapter: &Arc<PmsAdapter>, scope: &BrowseScope) {
    // Same test-only catalog guard as `request_refetch_hubs` — the catalog is `PmsState`, a
    // field of the per-`Bridge` `HubsStore`, not a crate-global; see `lib.rs::testlock` and D5.
    #[cfg(any(test, feature = "test-support"))]
    plx_base::testlock::assert_held("the pms hub catalog (reset)");
    let _ = adapter; // every HubsStore command path rotates before applying `HubsCmd::Reset`
    state.hub_gen = state.hub_gen.wrapping_add(1); // a worker still running belongs to the old identity
    state.srcs = Vec::new();
    state.deck_ask = None;
    forget_roster(state);
    state.seen_facts = u32::MAX;
    // Adopt the retained pin semantics with the empty commit below: a change from BEFORE this reset
    // is already reflected in "nothing", so it is not owed a re-merge. Left unadopted, the next
    // `pump` "caught up" on a scope some other era had moved and re-committed — freeing the HUBS
    // strings out from under a `hub_title` borrow held across that pump, which is how the test suite
    // read freed memory whenever another module's `browse::reset` ran in between.
    adopt_browse_scope(state, scope);
    commit(state, (Vec::new(), Vec::new(), Vec::new()));
}
// ---------------------------------------------------------------------------------------
#[cfg(any(test, feature = "test-support"))]
pub fn queue_test_landing(state: &PmsState, adapter: &PmsAdapter, items: Option<usize>) -> u32 {
    plx_base::testlock::assert_held("the pms hub catalog (queue_test_landing)");
    let source = &state.srcs[0];
    let seq = source.seq;
    let landing = Landing {
        gen: state.hub_gen, seq, sid: source.sid,
        client: None, token_gen: 0, build: items.map(build_test),
    };
    adapter.results.lock().unwrap_or_else(|e| e.into_inner()).push(landing);
    seq
}

#[cfg(any(test, feature = "test-support"))]
pub fn reverse_test_shelves(state: &mut PmsState) {
    plx_base::testlock::assert_held("the pms hub catalog (reverse_test_shelves)");
    for source in state.srcs.iter_mut() {
        if let Some(build) = source.last.as_mut() {
            for shelf in &mut build.shelves { shelf.items.reverse(); }
        }
    }
    let build = merge(&state.srcs);
    commit(state, build);
}

/// Test hook: every shelf's last card moves to its head, so each other card sits one place later
/// (an item landing above the focus, or a reorder that moves a card by one place).
#[cfg(any(test, feature = "test-support"))]
pub fn rotate_test_shelves_right(state: &mut PmsState) {
    plx_base::testlock::assert_held("the pms hub catalog (rotate_test_shelves_right)");
    for source in state.srcs.iter_mut() {
        if let Some(build) = source.last.as_mut() {
            for shelf in &mut build.shelves { shelf.items.rotate_right(1); }
        }
    }
    let build = merge(&state.srcs);
    commit(state, build);
}

#[cfg(any(test, feature = "test-support"))]
pub fn seed_grid_for_test(state: &mut PmsState, adapter: &Arc<PmsAdapter>, rows: usize, items: usize) {
    plx_base::testlock::assert_held("the pms hub catalog (seed_grid_for_test)");
    seed_for_test(state, adapter, items, HubState::Ready);
    let source = state.srcs[0].last.as_mut().unwrap();
    source.shelves = (0..rows).map(|row| {
        let mut shelf = build_test(items).shelves.remove(0);
        shelf.hub_id = format!("test.row.{row}");
        shelf
    }).collect();
    let build = merge(&state.srcs);
    commit(state, build);
}

/// [`seed_grid_for_test`] with each row's provider identity named: `(hubIdentifier, key, title)`.
/// A linked collection shelf is a `custom.collection.*` row, so its fixtures need both halves.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_named_hubs_for_test(
    state: &mut PmsState,
    adapter: &Arc<PmsAdapter>,
    items: usize,
    rows: &[(&str, &str, &str)],
) {
    plx_base::testlock::assert_held("the pms hub catalog (seed_named_hubs_for_test)");
    seed_for_test(state, adapter, items, HubState::Ready);
    let source = state.srcs[0].last.as_mut().unwrap();
    source.shelves = rows.iter().map(|(hub_id, key, title)| {
        let mut shelf = build_test(items).shelves.remove(0);
        shelf.hub_id = (*hub_id).into();
        shelf.key = (*key).into();
        shelf.title = (*title).into();
        shelf
    }).collect();
    let build = merge(&state.srcs);
    commit(state, build);
}

#[cfg(any(test, feature = "test-support"))]
pub fn seed_recent_window_for_test(state: &mut PmsState, adapter: &Arc<PmsAdapter>, offset: usize, items: usize, more: bool) {
    seed_named_hubs_for_test(state, adapter, items,
        &[("home.movies.recent", "/hubs/home/recentlyAdded?type=1", "Recent")]);
    let shelf = &mut state.srcs[0].last.as_mut().unwrap().shelves[0];
    shelf.offset = offset;
    shelf.end = offset + items;
    shelf.positions = (offset..offset + items).collect();
    shelf.more = more;
    for (i, item) in shelf.items.iter_mut().enumerate() { Arc::make_mut(item).rk = (offset + i + 1).to_string(); }
    let build = merge(&state.srcs);
    commit(state, build);
}

/// A Home whose only row is a Continue Watching deck of `items` cards with `more` behind them.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_continue_window_for_test(state: &mut PmsState, adapter: &Arc<PmsAdapter>, items: usize, more: bool) {
    let _ = adapter;
    let sid = ServerId::from_raw(0);
    let cw: Vec<CwItem> = (0..items).map(|i| CwItem {
        last_viewed_at: 10_000 - i as i64,
        m: Arc::new(PmsMovie { sid, rk: i.to_string(), title: i.to_string(), thumb: "/t".into(), art: "/a".into(),
            ..Default::default() }),
        position: i,
    }).collect();
    let mut source = Src::new(sid, String::new());
    source.state = HubState::Ready;
    let lane = deck::Lane::preview("0", &items.saturating_sub(1).to_string(), items, items + usize::from(more), !more);
    source.last = Some(SourceBuild { cw, lane, shelves: Vec::new() });
    let mut srcs = vec![source];
    settle_deck(&mut srcs, false);
    let build = merge(&srcs);
    state.srcs = srcs;
    remember_roster(state, &BrowseScope::standalone());
    state.seen_facts = facts_key();
    commit(state, build);
}

#[cfg(any(test, feature = "test-support"))]
pub fn reverse_test_hubs(state: &mut PmsState) {
    plx_base::testlock::assert_held("the pms hub catalog (reverse_test_hubs)");
    for source in state.srcs.iter_mut() {
        if let Some(build) = source.last.as_mut() { build.shelves.reverse(); }
    }
    let build = merge(&state.srcs);
    commit(state, build);
}

#[cfg(any(test, feature = "test-support"))]
pub fn remove_test_item(state: &mut PmsState, rk: &str) {
    plx_base::testlock::assert_held("the pms hub catalog (remove_test_item)");
    for source in state.srcs.iter_mut() {
        if let Some(build) = source.last.as_mut() {
            for shelf in &mut build.shelves {
                let mut index = 0;
                shelf.positions.retain(|_| {
                    let keep = shelf.items[index].rk != rk;
                    index += 1;
                    keep
                });
                shelf.items.retain(|item| item.rk != rk);
            }
        }
    }
    let build = merge(&state.srcs);
    commit(state, build);
}

/// Test hook: turn the shelf item `rk` into a collection, which the item menu has nothing to offer
/// (`has_actions`) — the one Home card whose hold the app DECLINES.
#[cfg(any(test, feature = "test-support"))]
pub fn retag_test_item_as_collection(state: &mut PmsState, rk: &str) {
    plx_base::testlock::assert_held("the pms hub catalog (retag_test_item_as_collection)");
    for source in state.srcs.iter_mut() {
        if let Some(build) = source.last.as_mut() {
            for shelf in &mut build.shelves {
                for item in shelf.items.iter_mut().filter(|item| item.rk == rk) {
                    Arc::make_mut(item).kind = KIND_COLLECTION;
                }
            }
        }
    }
    let build = merge(&state.srcs);
    commit(state, build);
}

#[cfg(test)]
#[path = "pms_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "pms_catalog_commit_tests.rs"]
mod catalog_commit_tests;

#[cfg(test)]
#[path = "pms_fetch_retry_tests.rs"]
mod fetch_retry_tests;

#[cfg(test)]
#[path = "pms_local_edit_tests.rs"]
mod local_edit_tests;

#[cfg(test)]
#[path = "pms_hero_pool_tests.rs"]
mod hero_pool_tests;

#[cfg(test)]
#[path = "pms_multi_source_merge_tests.rs"]
mod multi_source_merge_tests;

#[cfg(test)]
#[path = "pms_recent_paging_tests.rs"]
mod recent_paging_tests;

#[cfg(test)]
#[path = "pms_owed_tests.rs"]
mod owed_tests;

/// The library's tile abstraction (restructure spec §10) over a catalog row: the one place a
/// `PmsMovie` becomes a `Tile`, so a widget that draws a tile asks the trait and never this type.
impl plx_base::tile::Tile for PmsMovie {
    fn title(&self) -> &str {
        &self.title
    }
    fn poster(&self) -> Option<(u16, &str)> {
        (!self.thumb.is_empty()).then_some((self.sid.raw(), self.thumb.as_str()))
    }
    fn progress(&self) -> Option<f32> {
        self.resume_frac()
    }
    fn watched(&self) -> bool {
        self.watched
    }
    fn unwatched(&self) -> bool {
        self.unwatched
    }
}

#[cfg(test)]
#[path = "pms_hub_row_paging_tests.rs"]
mod hub_row_paging_tests;

#[cfg(test)]
#[path = "pms_deck_tests.rs"]
mod deck_tests;

#[cfg(test)]
#[path = "pms_home_row_ring_tests.rs"]
mod home_row_ring_tests;
