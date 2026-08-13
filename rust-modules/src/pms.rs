//! Plex library fetch/parse into the private catalog (was src/pms.c), read by the UI
//! via movie()/hub_item()/hero_pool_item(), plus urlenc_str (shared by posters/route).
//! The fetch + JSON parse go through the typed `crate::plex` client (serde DTOs) — no
//! hand-built paths or `Value` scraping here.
use std::os::raw::c_int;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

const PMS_MAX_MOVIES: usize = 256;

/// A catalog row — owned strings (the old C-ABI fixed `[u8; N]` buffers are gone; no C
/// consumer remains). Fields pub(crate) so the UI / route / player read them directly.
#[derive(Default)]
pub struct PmsMovie {
    pub(crate) title: String,
    pub(crate) year: c_int,
    pub(crate) rating: String,
    pub(crate) dur_ns: i64,
    pub(crate) part: String,
    pub(crate) thumb: String,
    pub(crate) art: String,
    pub(crate) summary: String,
    pub(crate) rk: String,
    pub(crate) vcodec: String,
    pub(crate) acodec: String,
    pub(crate) blur: [[f32; 3]; 4],
    pub(crate) has_blur: bool,
    pub(crate) kind: c_int,    // 0 = movie, 1 = show, 2 = season, 3 = episode
    pub(crate) resume_ms: i64, // viewOffset — drives the Continue Watching resume bar
    pub(crate) show_rk: String, // parent show rk (episode: grandparent; season: parent)
    pub(crate) season_index: c_int, // season number (episode: parentIndex; season: index)
    pub(crate) show_title: String, // episode only: grandparentTitle (the hero headlines the SHOW)
    pub(crate) ep_index: c_int,    // episode only: episode number within the season
    /// Fully unwatched (movie/episode: no viewCount; show/season: zero viewed leaves).
    pub(crate) unwatched: bool,
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
    pub(crate) watched: bool,
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
    pub(crate) fn resume_frac(&self) -> Option<f32> {
        (self.resume_ms > 0 && self.dur_ns > 0 && self.resume_ms * 1_000_000 < self.dur_ns)
            .then(|| (self.resume_ms as f32 * 1_000_000.0 / self.dur_ns as f32).clamp(0.0, 1.0))
    }
}

// The catalog (private; the UI reads it through movie()/hub_item()/hero_pool_item()).
// Main-thread only, like every UI static; rebuilt wholesale by pms_fetch_hubs.
static mut CATALOG: Vec<PmsMovie> = Vec::new();

fn catalog() -> &'static Vec<PmsMovie> {
    unsafe { &*std::ptr::addr_of!(CATALOG) }
}

/// catalog row `i`, or None. The reference stays valid until the next refetch (main-thread
/// only — the same lifetime discipline the old raw `movie_ptr` had, now bounds-checked;
/// `refetch_hubs_reconcile` is the one mutation ritual and re-resolves the open surfaces).
pub(crate) fn movie(i: usize) -> Option<&'static PmsMovie> {
    catalog().get(i)
}
/// catalog index whose ratingKey == `rk` (for the /tmp/plxnative-detail probe), or -1
pub(crate) fn index_of_rk(rk: &str) -> c_int {
    catalog().iter().position(|m| m.rk == rk).map(|i| i as c_int).unwrap_or(-1)
}

// ---- helpers ----
/// owned copy of a metadata string with newlines flattened to spaces (single-line UI fields)
fn clean(s: &str) -> String {
    s.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect()
}

/// percent-encode into a String (Rust callers, e.g. posters::poster_key)
pub(crate) fn urlenc_str(src: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(src.len());
    for &ch in src.as_bytes() {
        if ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_' | b'.' | b'~') {
            out.push(ch as char);
        } else {
            out.push('%');
            out.push(HEX[(ch >> 4) as usize] as char);
            out.push(HEX[(ch & 15) as usize] as char);
        }
    }
    out
}

/// Parse one Plex `Metadata` item (from a section listing OR a hub) into a catalog row.
/// pub(crate): the Library browse store (`browse.rs`) maps its paged listings with it too.
pub(crate) fn parse_item(it: &crate::plex::Metadata) -> PmsMovie {
    let mut m = PmsMovie::default();
    m.kind = match it.kind.as_str() {
        "show" => 1,
        "season" => 2,
        "episode" => 3,
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
        }
        _ => {}
    }
    // shows/seasons count leaves (a show with any watched episode is no longer "unwatched");
    // movies/episodes key on viewCount absence (docs/pms-api.md §2)
    m.unwatched = match m.kind {
        1 | 2 => it.viewed_leaf_count == 0 && it.leaf_count > 0,
        _ => it.view_count == 0,
    };
    // …and DONE is its own question, not the negation of that one: for a container it takes ALL the
    // leaves, so a show three episodes in is neither (see the `watched` field's doc).
    m.watched = match m.kind {
        1 | 2 => it.leaf_count > 0 && it.viewed_leaf_count >= it.leaf_count,
        _ => it.view_count > 0,
    };
    m.title = clean(&it.title);
    m.year = it.year as c_int;
    m.rating = clean(&it.content_rating);
    m.dur_ns = if it.duration > 0 { it.duration * 1_000_000 } else { 0 };
    m.resume_ms = it.view_offset;
    // poster: prefer the show poster for episodes (grandparentThumb) so a landscape
    // episode still doesn't fill a portrait card
    let thumb = if it.grandparent_thumb.is_empty() { &it.thumb } else { &it.grandparent_thumb };
    m.thumb = clean(thumb);
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
struct HubRow {
    title: String,
    hub_id: String, // locale-independent hubIdentifier ("home.continue", "home.movies.recent", …)
    start: usize,
    len: usize,
}
static mut HUBS: Vec<HubRow> = Vec::new();

fn hubs() -> &'static Vec<HubRow> {
    unsafe { &*std::ptr::addr_of!(HUBS) }
}

// ---- rotating hero pool: curated catalog indices (Continue Watching then Recently Added) ----
const HERO_MAX: usize = 8;
static mut HERO_POOL: Vec<usize> = Vec::new();

/// number of items in the rotating hero pool
pub(crate) fn hero_pool_len() -> usize {
    unsafe { std::ptr::addr_of!(HERO_POOL).as_ref().map(|v| v.len()).unwrap_or(0) }
}
/// hero-pool item `i`, or None
pub(crate) fn hero_pool_item(i: usize) -> Option<&'static PmsMovie> {
    let idx = unsafe { std::ptr::addr_of!(HERO_POOL).as_ref() }.and_then(|v| v.get(i)).copied()?;
    movie(idx)
}

/// number of home hubs
pub(crate) fn hub_count() -> usize {
    hubs().len()
}
/// title of hub `i` (e.g. "Continue Watching") — borrowed from the main-thread hub table (the
/// per-frame shelf-title draw shouldn't clone a String per row; HUBS only changes on a re-fetch).
pub(crate) fn hub_title(i: usize) -> &'static str {
    hubs().get(i).map(|h| h.title.as_str()).unwrap_or("")
}
/// item count in hub `i`
pub(crate) fn hub_len(i: usize) -> usize {
    hubs().get(i).map(|h| h.len).unwrap_or(0)
}
/// whether hub `i` is the merged Continue Watching shelf (its tiles play directly on OK, so the
/// home grid stamps the play-hint badge on them). Matched on the locale-independent hubIdentifier.
pub(crate) fn hub_is_continue(i: usize) -> bool {
    hubs().get(i).map(|h| h.hub_id == "home.continue").unwrap_or(false)
}
/// item `col` of hub `hub`, or None
pub(crate) fn hub_item(hub: usize, col: usize) -> Option<&'static PmsMovie> {
    let h = hubs().get(hub)?;
    if col < h.len {
        movie(h.start + col)
    } else {
        None
    }
}

/// Refetch the home hubs and reconcile the surfaces that index into the rebuilt catalog: an open
/// detail page re-resolves its selected row (home's focus self-clamps at its read accessors).
/// The ONE post-mutation refresh ritual — player exit and the watched toggle both call this.
pub(crate) fn refetch_hubs_reconcile() -> c_int {
    let n = pms_fetch_hubs();
    crate::ui::detail::reselect();
    n
}

/// Fetch the home hubs (Continue Watching, On Deck, Recently Added, collections) into
/// the catalog + the HUBS grouping — BLOCKING, and the boot/install path's fetch. Skips
/// music/photo/playlist hubs + empty ones. Builds the new catalog/hubs/pool locally, then
/// commits all three statics at once ([`commit`]).
///
/// A failure no longer commits empty. It used to — "blanking the home consistently" — which
/// made a dead server indistinguishable from a server with nothing on it, and left Home a bare
/// dark screen with no explanation and nothing that would ever try again. Now it lands in
/// [`fail`]: the previous (consistent) three statics stay exactly as they were, the state
/// machine goes [`HubState::Failed`], and [`pump`] retries on a backoff. The consistency the
/// old comment was defending is intact — all three statics still only ever move together.
pub(crate) fn pms_fetch_hubs() -> c_int {
    HUB_GEN.fetch_add(1, Ordering::SeqCst); // supersede any retry already in flight
    match catch_unwind(fetch_build) {
        Ok(Some(build)) => commit(build),
        _ => {
            fail();
            catalog().len() as c_int
        }
    }
}

/// The catalog/hubs/pool triple a fetch produces, before it is committed. Owned data only, so
/// the off-thread refetch can build it on a worker and hand it over through a mailbox.
type HubBuild = (Vec<PmsMovie>, Vec<HubRow>, Vec<usize>);

/// GET /hubs and project it — the whole fetch, minus the commit. `None` = the request failed
/// (transport, HTTP, or parse), which is NOT the same as a server with no hubs (`Some` with an
/// empty build). Shared verbatim by the blocking boot fetch and the off-thread retry.
fn fetch_build() -> Option<HubBuild> {
    let c = crate::plex::client_opt()?;
    let mc = c.home_hubs(12)?;
    // The Continue Watching shelf comes from the DEDICATED hub, not from `/hubs`'s
    // `home.continue` + `home.ondeck` pair — see `build_hubs`. Its failure fails the whole fetch
    // (`?`), which is the module's existing contract: nothing commits, the three statics keep their
    // last consistent values, and `pump` retries on a backoff. Losing the most important shelf to a
    // transient error would be worse than briefly showing the previous one.
    let cw = c.continue_watching(12)?;
    Some(build_hubs(&mc, &cw))
}

/// Project a /hubs response into the catalog/hubs/pool triple. Pure — no statics, no I/O.
fn build_hubs(mc: &crate::plex::MediaContainer, cw: &crate::plex::MediaContainer) -> HubBuild {
    let mut new_cat: Vec<PmsMovie> = Vec::new();
    let mut new_hubs: Vec<HubRow> = Vec::new();
    let mut new_pool: Vec<usize> = Vec::new();
    const SKIP: [&str; 6] = ["album", "artist", "track", "photo", "clip", "playlist"];

    // Build the display shelves as ordered lists of item refs.
    //
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
    let mut shelves: Vec<(String, String, Vec<&crate::plex::Metadata>)> = Vec::new(); // (title, hubIdentifier, items)
    let mut cw_idx: Option<usize> = None; // shelves slot of Continue Watching
    for hub in cw.hub.iter() {
        let items: Vec<&crate::plex::Metadata> =
            hub.metadata.iter().filter(|m| !SKIP.contains(&m.kind.as_str())).collect();
        if items.is_empty() {
            continue;
        }
        // keep the hub id the rest of the module already matches on (hero-pool eligibility,
        // `hub_plays_directly`), rather than the dedicated hub's own "continueWatching"
        shelves.push(("Continue Watching".to_string(), "home.continue".to_string(), items));
        cw_idx = Some(shelves.len() - 1);
        break;
    }
    for hub in &mc.hub {
        if SKIP.contains(&hub.kind.as_str()) || hub.metadata.is_empty() {
            continue;
        }
        if hub.hub_identifier == "home.continue" || hub.hub_identifier == "home.ondeck" {
            continue; // superseded by the dedicated hub above
        }
        shelves.push((hub.title.clone(), hub.hub_identifier.clone(), hub.metadata.iter().collect()));
    }
    if let Some(si) = cw_idx {
        // stable sort: most recently played first; equal timestamps keep hub order.
        shelves[si].2.sort_by(|a, b| b.last_viewed_at.cmp(&a.last_viewed_at));
    }

    for (title, hub_id, items) in &shelves {
        let start = new_cat.len();
        for item in items {
            if new_cat.len() >= PMS_MAX_MOVIES {
                break;
            }
            let m = parse_item(item);
            if !m.title.is_empty() && !m.thumb.is_empty() {
                new_cat.push(m); // need a poster to show it in a shelf
            }
        }
        if new_cat.len() > start {
            new_hubs.push(HubRow {
                title: title.clone(),
                hub_id: hub_id.clone(),
                start,
                len: new_cat.len() - start,
            });
        }
    }

    // Build the rotating hero pool: Continue Watching items first, then Recently Added,
    // deduped by ratingKey. Require landscape `art` (the hero draws a full-bleed backdrop)
    // and skip seasons (a bare "Season 1" makes a poor billboard). Capped at HERO_MAX.
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
            if new_pool.iter().any(|&j| new_cat[j].rk == m.rk) {
                continue; // dedup by ratingKey
            }
            new_pool.push(idx);
        }
    }
    (new_cat, new_hubs, new_pool)
}

// ---- fetch state machine: loading / ready / failed + the automatic-retry backoff -------------
//
// Modelled on `browse.rs`'s page store, deliberately, because it already learned both lessons
// this needed: a FAILED fetch must never overwrite a populated store (one wifi hiccup used to
// blank a whole grid permanently), and a fast-failing network must be held off by a countdown
// rather than re-spawning a worker every frame.

/// What the last hub fetch produced. Home's loading / empty / error read-out is a projection of
/// this: an empty catalog is only an empty *screen* when the fetch actually succeeded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HubState {
    /// a fetch is in flight, or the first one hasn't run yet — nothing to show is not an answer
    Loading,
    /// the server answered; the catalog is whatever it says it is (possibly legitimately empty)
    Ready,
    /// the fetch failed — the previous catalog (if any) is untouched and [`pump`] is counting
    /// down to the next automatic attempt
    Failed,
}
static mut HUB_STATE: HubState = HubState::Loading;

/// Off-thread refetch landing, tagged with the generation it was spawned in. `Built` commits;
/// `Failed` deliberately carries no data, so a failure cannot be mistaken for "the server
/// returned nothing".
enum HubLanding {
    Built(u32, HubBuild),
    Failed(u32),
}
static HUB_RESULT: Mutex<Option<HubLanding>> = Mutex::new(None);
/// Bumped by every authoritative fetch ([`reset`] on an identity change, and the blocking
/// install fetch): a worker spawned before it lands with a stale tag and is DROPPED. Without
/// this, a retry in flight across a profile switch could commit the previous account's hubs on
/// top of the new one's — the same late-landing hazard `browse.rs` keys its `GEN` on.
static HUB_GEN: AtomicU32 = AtomicU32::new(0);
/// Single flight, cleared ONLY where the mailbox is taken (or where the spawn is refused) — the
/// same latch discipline as `browse::IN_FLIGHT`: drop one without the other and the hubs never
/// fetch again for the rest of the session.
static HUBS_FETCHING: AtomicBool = AtomicBool::new(false);

/// Seconds until the next automatic attempt, and the consecutive-failure count that sizes it.
/// Main-thread only (stepped by [`pump`], which the Home update drives).
static mut RETRY_S: f32 = 0.0;
static mut RETRY_N: u32 = 0;
/// The backoff ladder's ends. A TV parked on a sleeping server must keep trying — that IS the
/// feature — without ever becoming a request loop, so the wait doubles from `MIN` to a `MAX`
/// that still recovers within half a minute of the server coming back.
const RETRY_MIN_S: f32 = 2.0;
const RETRY_MAX_S: f32 = 30.0;

/// Wait before attempt `fails + 1`: 2s, 4s, 8s, 16s, then 30s forever. Pure — host-tested.
fn backoff_secs(fails: u32) -> f32 {
    let steps = fails.saturating_sub(1).min(6); // 1<<6 already exceeds the cap; guards the shift
    (RETRY_MIN_S * (1u32 << steps) as f32).min(RETRY_MAX_S)
}

/// The hub-fetch state — Home's loading/empty/error projection reads it.
pub(crate) fn hub_state() -> HubState {
    unsafe { std::ptr::addr_of!(HUB_STATE).read() }
}

/// Install a finished build: all three statics move together (they always have — a half-applied
/// catalog once left a stale hero pool floating over emptied shelves), and a success retires the
/// backoff so the next failure starts at the bottom of the ladder again.
fn commit(build: HubBuild) -> c_int {
    let (new_cat, new_hubs, new_pool) = build;
    let n = new_cat.len();
    unsafe {
        *std::ptr::addr_of_mut!(CATALOG) = new_cat;
        *std::ptr::addr_of_mut!(HUBS) = new_hubs;
        *std::ptr::addr_of_mut!(HERO_POOL) = new_pool;
        std::ptr::addr_of_mut!(HUB_STATE).write(HubState::Ready);
        std::ptr::addr_of_mut!(RETRY_N).write(0);
        std::ptr::addr_of_mut!(RETRY_S).write(0.0);
    }
    n as c_int
}

/// Record a failed fetch: keep whatever catalog is already committed and arm the next attempt.
fn fail() {
    unsafe {
        let n = std::ptr::addr_of!(RETRY_N).read().saturating_add(1);
        std::ptr::addr_of_mut!(RETRY_N).write(n);
        std::ptr::addr_of_mut!(RETRY_S).write(backoff_secs(n));
        std::ptr::addr_of_mut!(HUB_STATE).write(HubState::Failed);
        // the ONE line that says a dead Home is dead ON PURPOSE and is coming back — without it
        // the whole recovery is invisible in the event log
        crate::log(&format!("hubs: fetch FAILED (attempt {n}) — retrying in {:.0}s", backoff_secs(n)));
    }
}

/// Step the automatic-retry countdown by `dt` seconds; true when the next attempt is due. Split
/// out from [`pump`] so the ladder is testable without spawning a worker or touching a socket.
fn retry_due(dt: f32) -> bool {
    unsafe {
        let left = std::ptr::addr_of!(RETRY_S).read() - dt;
        std::ptr::addr_of_mut!(RETRY_S).write(left.max(0.0));
        left <= 0.0
    }
}

/// Spawn an off-thread hub refetch (single flight); [`pump`] lands it. The boot/install fetch
/// stays blocking, but every RETRY goes through here — and that is exactly what lets Home show a
/// live loading state at all: a blocking fetch on the SDL loop draws no frames while it runs, so
/// the spinner it is supposed to be spinning would never reach the panel.
fn kick_refetch() {
    if HUBS_FETCHING.swap(true, Ordering::SeqCst) {
        return; // one in flight already — its spinner is the honest answer
    }
    unsafe {
        std::ptr::addr_of_mut!(HUB_STATE).write(HubState::Loading);
        std::ptr::addr_of_mut!(RETRY_S).write(0.0);
    }
    let gen = HUB_GEN.load(Ordering::SeqCst);
    let spawned = crate::task::spawn_small("hubs", move || {
        let built = catch_unwind(fetch_build).ok().flatten();
        // filled OUTSIDE the guard so a panicking fetch still lands (as a failure) rather than
        // latching the single flight forever
        *HUB_RESULT.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(built.map(|b| HubLanding::Built(gen, b)).unwrap_or(HubLanding::Failed(gen)));
    });
    if !spawned {
        // nothing will ever fill the mailbox (the thread limit refused us), so release the latch
        // here and back off — `pump` will try again on the ladder.
        HUBS_FETCHING.store(false, Ordering::SeqCst);
        fail();
    } else {
        crate::log("hubs: retrying (off-thread)");
    }
}

/// The Retry control's kick: try again NOW, from the bottom of the ladder — a person who asks
/// for it should never be made to sit out a 30-second automatic wait. A no-op while a fetch is
/// already in flight.
pub(crate) fn request_retry() {
    unsafe {
        std::ptr::addr_of_mut!(RETRY_N).write(0);
        std::ptr::addr_of_mut!(RETRY_S).write(0.0);
    }
    kick_refetch();
}

/// Once-a-frame main-thread tick: land a finished refetch, then count down to the next automatic
/// attempt. Driven by `ui::home::home_update`, so it runs exactly while Home is the screen that
/// cares (and never spawns a background fetch behind the player).
pub(crate) fn pump(dt: f32) {
    // taken into a `let` FIRST: an `if let` scrutinee holds its temporary guard for the whole
    // body under edition 2021, which would run the commit + `detail::reselect()` with the mailbox
    // still locked (and changes meaning again under 2024)
    let landed = HUB_RESULT.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(landing) = landed {
        // A refetch landing rewrites the shelves under a Home screen that may have gone idle
        // (the failure state repaints too — a retry that fails changes the status caption).
        crate::ui::idle::invalidate();
        HUBS_FETCHING.store(false, Ordering::SeqCst);
        let cur = HUB_GEN.load(Ordering::SeqCst);
        match landing {
            // a landing from before the last authoritative fetch describes a server (or an
            // account) we have since moved off — drop it, neither commit nor blame
            HubLanding::Built(gen, _) | HubLanding::Failed(gen) if gen != cur => {}
            HubLanding::Built(_, build) => {
                let n = commit(build);
                crate::log(&format!("hubs: retry landed — {n} items, {} shelves", hub_count()));
                // the same post-mutation ritual `refetch_hubs_reconcile` performs: an open
                // detail page re-resolves its selected row against the rebuilt catalog
                crate::ui::detail::reselect();
            }
            HubLanding::Failed(_) => fail(),
        }
    }
    // Anything but Ready with nothing in flight is a state only a fetch can leave: Failed (with a
    // backoff owed) or a Loading whose worker landed stale and was dropped — the latter owes
    // nothing, so it re-kicks on the spot rather than wedging Home on a spinner forever.
    if hub_state() != HubState::Ready && !HUBS_FETCHING.load(Ordering::SeqCst) && retry_due(dt) {
        kick_refetch();
    }
}

/// A committable build of `n` placeholder rows in one shelf (test fixture). Only the SHAPE is
/// real — `build_hubs` would have dropped these rows for having no title/poster; what the tests
/// here assert is the commit/landing bookkeeping, which never looks inside a row.
#[cfg(test)]
fn build_test(n: usize) -> HubBuild {
    let cat: Vec<PmsMovie> = (0..n).map(|_| PmsMovie::default()).collect();
    let hubs = vec![HubRow { title: "Continue Watching".into(), hub_id: "home.continue".into(), start: 0, len: n }];
    (cat, hubs, Vec::new())
}

/// Test hook: put the store in a known place — `items` rows in one shelf, plus a fetch state.
/// Home's read-out is a pure projection of this pair, and the states a host test cannot reach for
/// real (a live server answering, or refusing) are exactly the ones worth pinning.
#[cfg(test)]
pub(crate) fn seed_for_test(items: usize, state: HubState) {
    reset();
    if items > 0 {
        commit(build_test(items));
    }
    unsafe { std::ptr::addr_of_mut!(HUB_STATE).write(state) };
}

/// Drop the catalog and re-arm the fetch — the identity-change twin of [`crate::browse::reset`],
/// called from the same place (`install_pms`). Now that a failed fetch KEEPS the previous
/// catalog, a profile switch whose fetch fails would otherwise leave the previous user's shelves
/// on screen; this is the one place that must still wipe them.
pub(crate) fn reset() {
    HUB_GEN.fetch_add(1, Ordering::SeqCst); // a worker still running belongs to the old identity
    *HUB_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    HUBS_FETCHING.store(false, Ordering::SeqCst);
    unsafe {
        *std::ptr::addr_of_mut!(CATALOG) = Vec::new();
        *std::ptr::addr_of_mut!(HUBS) = Vec::new();
        *std::ptr::addr_of_mut!(HERO_POOL) = Vec::new();
        std::ptr::addr_of_mut!(HUB_STATE).write(HubState::Loading);
        std::ptr::addr_of_mut!(RETRY_N).write(0);
        std::ptr::addr_of_mut!(RETRY_S).write(0.0);
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    // NB every test that touches the catalog statics holds the crate-wide serial lock: they are
    // read from other modules' tests too (`ui::home` walks `hub_len`), which a module-local mutex
    // cannot see. `reset()` doubles as the teardown.

    /// A landing tagged with the CURRENT generation — what a worker spawned right now would post.
    fn land(l: impl FnOnce(u32) -> HubLanding) {
        *HUB_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some(l(HUB_GEN.load(Ordering::SeqCst)));
    }

    #[test]
    fn the_backoff_doubles_then_holds_at_the_ceiling() {
        assert_eq!(backoff_secs(1), RETRY_MIN_S, "the first retry is the shortest wait");
        assert_eq!(backoff_secs(2), 4.0);
        assert_eq!(backoff_secs(3), 8.0);
        assert_eq!(backoff_secs(4), 16.0);
        assert_eq!(backoff_secs(5), RETRY_MAX_S, "32s is past the ceiling");
        assert_eq!(backoff_secs(99), RETRY_MAX_S, "and it never grows past it (nor overflows)");
    }

    /// The bug this whole unit exists for: a failed fetch used to commit an EMPTY catalog, so one
    /// unreachable moment blanked a populated Home for good. A failure must leave every one of
    /// the three statics exactly as it found them.
    #[test]
    fn a_failed_landing_never_blanks_a_populated_home() {
        let _g = crate::testlock::serial();
        reset();
        commit(build_test(3));
        assert_eq!(hub_count(), 1);
        assert_eq!(hub_len(0), 3);

        land(HubLanding::Failed);
        pump(0.0);

        assert_eq!(hub_state(), HubState::Failed, "the failure must be distinguishable");
        assert_eq!(hub_count(), 1, "the shelves survive a failed refetch");
        assert_eq!(hub_len(0), 3);
        assert!(unsafe { std::ptr::addr_of!(RETRY_S).read() } > 0.0, "and the next attempt is armed");
        reset();
    }

    /// A landing that carries a build commits it, and a success retires the backoff so the next
    /// failure starts at the bottom of the ladder instead of inheriting a 30s wait.
    #[test]
    fn a_successful_landing_commits_and_retires_the_backoff() {
        let _g = crate::testlock::serial();
        reset();
        fail();
        fail();
        assert_eq!(hub_state(), HubState::Failed);

        land(|g| HubLanding::Built(g, build_test(2)));
        pump(0.0);

        assert_eq!(hub_state(), HubState::Ready);
        assert_eq!(hub_len(0), 2);
        assert_eq!(unsafe { std::ptr::addr_of!(RETRY_N).read() }, 0);
        assert_eq!(unsafe { std::ptr::addr_of!(RETRY_S).read() }, 0.0);
        reset();
    }

    /// An answer of "nothing" is an ANSWER: it must land as Ready (Home's empty state), not as a
    /// failure, or the screen would apologise for a server that is simply empty — and retry it
    /// forever.
    #[test]
    fn a_server_with_no_hubs_is_ready_and_empty_not_failed() {
        let _g = crate::testlock::serial();
        reset();
        land(|g| HubLanding::Built(g, (Vec::new(), Vec::new(), Vec::new())));
        pump(0.0);
        assert_eq!(hub_state(), HubState::Ready);
        assert_eq!(hub_count(), 0);
        reset();
    }

    /// The countdown is real time (seconds of `dt`), not frames like `browse.rs`'s — a device
    /// that drops to 30fps must still retry on the same wall clock. NB `retry_due` keeps
    /// reporting due once it is spent; what makes an attempt happen only once is `kick_refetch`
    /// re-latching the single flight, not this.
    #[test]
    fn the_retry_countdown_fires_when_the_backoff_elapses() {
        let _g = crate::testlock::serial();
        reset();
        fail(); // arms RETRY_MIN_S
        for _ in 0..3 {
            assert!(!retry_due(0.5), "0.5s at a time must not fire before the 2s wait is spent");
        }
        assert!(retry_due(0.5), "the fourth half-second spends it");
        reset();
    }

    /// A retry still in flight when the account changes must not commit the PREVIOUS identity's
    /// hubs over the new one's: its landing carries the old generation and is dropped whole —
    /// neither committed nor blamed on the current fetch.
    #[test]
    fn a_landing_from_before_a_reset_is_dropped_whole() {
        let _g = crate::testlock::serial();
        reset();
        let stale = HUB_GEN.load(Ordering::SeqCst);
        reset(); // the identity change
        commit(build_test(2)); // …and the new identity's catalog
        *HUB_RESULT.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(HubLanding::Built(stale, build_test(9)));
        pump(0.0);
        assert_eq!(hub_len(0), 2, "the stale build must not replace the current catalog");
        assert_eq!(hub_state(), HubState::Ready, "nor be counted as a failure of the current one");
        reset();
    }

    /// `reset` is the profile-switch wipe: the previous user's shelves must not survive it, and
    /// the state machine must come back as a fresh boot's (Loading, no backoff owed).
    #[test]
    fn reset_wipes_the_catalog_and_re_arms_the_fetch() {
        let _g = crate::testlock::serial();
        commit(build_test(4));
        fail();
        reset();
        assert_eq!(hub_count(), 0);
        assert_eq!(catalog().len(), 0);
        assert_eq!(hero_pool_len(), 0);
        assert_eq!(hub_state(), HubState::Loading);
        assert_eq!(unsafe { std::ptr::addr_of!(RETRY_N).read() }, 0);
    }
}
