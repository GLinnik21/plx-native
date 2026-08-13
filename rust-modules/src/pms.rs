//! Plex library fetch/parse into the private catalog (was src/pms.c), read by the UI
//! via movie()/hub_item()/hero_pool_item(), plus urlenc_str (shared by posters/route).
//! The fetch + JSON parse go through the typed `crate::plex` client (serde DTOs) — no
//! hand-built paths or `Value` scraping here.
use crate::plex::ServerId;
use std::os::raw::c_int;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

/// Catalog rows Home holds at most, across EVERY source. A hard ceiling on the store the whole
/// screen indexes into, not a per-server one — see [`allot`] for how the sources divide it.
const PMS_MAX_MOVIES: usize = 256;

/// Shelves Home holds at most, across every source — the number of rows the grid can actually
/// address (`ui::home::MAX_HUBS` is this constant, so the budget can never quietly stop matching
/// the array it is budgeting for).
///
/// It matters far more with two servers than it ever did with one. The truncation is in SHELF
/// ORDER, so a single greedy source — `/hubs` promotes several rows per library, and four
/// libraries already overrun this — used to be able to spend the entire allowance before the next
/// source got a row. That is starvation, and it is what [`allot`] exists to prevent.
pub(crate) const MAX_SHELVES: usize = 16;

/// Cards one shelf holds at most — the number the grid can address (`ui::home::MAX_ITEMS` is this
/// constant, for the same reason [`MAX_SHELVES`] is).
///
/// It was unreachable with one server, because `/hubs?count=12` bounds every shelf at 12. The
/// MERGED deck is what reaches it: three sources' Continue Watching is up to 36 cards, and
/// `ui::home`'s focus ring and its OK dispatch clamp differently past this number — the ring stops
/// at the last addressable card while the press opens whatever column the raw index names. Cap the
/// data and the two can never disagree.
pub(crate) const MAX_SHELF_ITEMS: usize = 24;

/// Items asked of each hub endpoint, per source. `/hubs?count=` is items-per-hub, so this bounds
/// a shelf, never the number of shelves.
const HUB_FETCH_COUNT: i64 = 12;

/// A catalog row — owned strings (the old C-ABI fixed `[u8; N]` buffers are gone; no C
/// consumer remains). Fields pub(crate) so the UI / route / player read them directly.
///
/// `Clone` because the catalog is now a MERGE: each source keeps the projection it last answered
/// with (so one dead share cannot blank the others), and every landing rebuilds the flat catalog
/// from those. The copy happens a handful of times a session, never per frame.
#[derive(Default, Clone)]
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
    /// Which SERVER this shelf's items came from, as the owner's handle ("friend1") — empty
    /// whenever the row came from a server of the signed-in user's OWN.
    ///
    /// Empty is the ABSENCE of an annotation, not an empty one: the home shelf heading draws no
    /// separator and no second run at all for it (`ui::home::heading_flow`), so the annotation costs
    /// a single-server library nothing — no gap, no dot, no draw call. (The heading's INK changed in
    /// the same pass, which is a separate, deliberate harmonization; `heading_flow`'s doc has it.)
    ///
    /// It is empty on the merged **Continue Watching** shelf too, and that is a design decision
    /// rather than a gap: that shelf is drawn from every source at once and sorted by when the
    /// owner last watched, so a borrowed item can legitimately hold first position — a shelf drawn
    /// from three servers cannot be named by one of them. Source identity for those tiles is stated
    /// where the ITEM is (the hero's meta line, the detail page), never on the heading and never on
    /// the artwork.
    source: String,
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
/// Handle of the server hub `i` came from ("friend1"), or **empty** for the signed-in user's own
/// server — see [`HubRow::source`]. Borrowed on the same terms as [`hub_title`]: main-thread only,
/// valid until the next hub commit.
pub(crate) fn hub_source(i: usize) -> &'static str {
    hubs().get(i).map(|h| h.source.as_str()).unwrap_or("")
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

/// Fetch Home — BLOCKING for the owned server, off-thread for every other source. The boot /
/// install path's fetch; returns the committed catalog's row count.
///
/// **Only the FIRST source is fetched inline.** Home cannot open without the signed-in user's own
/// server, and this call's return value is what `install_pms` logs as `nmovies=`, so that one stays
/// synchronous exactly as it always was. Every shared source is kicked onto a worker instead:
/// a share that has gone away costs a full connect timeout (8 s, measured — `docs/shared-servers.md`
/// §2), and serialising N of those on the SDL thread would freeze boot for as long as the friend's
/// router stays quiet. They land in [`pump`] whenever they land.
///
/// A failure does not commit empty. It used to — "blanking the home consistently" — which made a
/// dead server indistinguishable from a server with nothing on it, and left Home a bare dark screen
/// with no explanation and nothing that would ever try again. Now the verdict is PER SOURCE: the
/// failing one keeps whatever it last answered with, the others commit regardless, and [`pump`]
/// retries that source alone on its own backoff.
pub(crate) fn pms_fetch_hubs() -> c_int {
    HUB_GEN.fetch_add(1, Ordering::SeqCst); // supersede every retry already in flight
    sync_roster();
    let mut srcs = lock_srcs();
    // A superseded worker's landing is dropped on the generation above, so releasing the
    // single-flight latches here cannot double-apply anything — and without it a source whose
    // worker was in flight across this call would stay latched and never fetch again.
    for s in srcs.iter_mut() {
        s.fetching = false;
    }
    if let Some(own) = srcs.first_mut() {
        match crate::plex::client_for(own.sid).and_then(|c| catch_unwind(|| fetch_source(c)).ok().flatten()) {
            Some(b) => landed_ok(own, b),
            None => landed_fail(own),
        }
    }
    for s in srcs.iter_mut().skip(1) {
        kick(s);
    }
    let build = merge(&srcs);
    drop(srcs);
    commit(build)
}

/// The catalog/hubs/pool triple the merge produces, before it is committed.
type HubBuild = (Vec<PmsMovie>, Vec<HubRow>, Vec<usize>);

// ---- one source's contribution -----------------------------------------------------------------

/// A Continue Watching entry, carrying the sort key the MERGE needs. `lastViewedAt` used to be read
/// off the wire DTO and thrown away at parse time, because one server's hub arrived already in the
/// right order; across sources the order has to be re-established after the fact, so the key has to
/// survive the projection.
struct CwItem {
    last_viewed_at: i64,
    m: PmsMovie,
}

/// One shelf as a source projected it: rows already parsed and filtered, so the merge is pure
/// arithmetic over owned data and never touches a wire DTO.
struct Shelf {
    title: String,
    hub_id: String,
    items: Vec<PmsMovie>,
}

/// ONE source's whole contribution to Home — its Continue Watching items (merged with everyone
/// else's into a single shelf) and its own shelves (kept whole, annotated with its owner's handle).
///
/// Owned data only, so a worker can build it and hand it over through the mailbox, and a source can
/// KEEP the last one it answered with across a failure.
#[derive(Default)]
struct SourceBuild {
    cw: Vec<CwItem>,
    shelves: Vec<Shelf>,
}

/// GET one source's hubs and project them. `None` = the request failed (transport, HTTP, or
/// parse), which is NOT the same as a server with nothing on it (`Some` with an empty build) —
/// that distinction is the whole reason an empty library reads as Ready and not as an error.
///
/// The client is passed IN, captured at the spawn site: a worker must never ask which server is
/// current (`browse.rs` states the rule), and a `&'static Client` also pins the exact address this
/// fetch was aimed at even if the registry re-points that slot mid-request.
fn fetch_source(c: &crate::plex::Client) -> Option<SourceBuild> {
    let mc = c.home_hubs(HUB_FETCH_COUNT)?;
    // The Continue Watching shelf comes from the DEDICATED hub (see `project`). Its failure fails
    // THIS SOURCE (`?`) — nothing of it commits and it retries on its own backoff. Losing the most
    // important shelf to a transient error would be worse than briefly showing the previous one.
    let cw = c.continue_watching(HUB_FETCH_COUNT)?;
    Some(project(&mc, &cw))
}

/// Project one source's `/hubs` + `/hubs/continueWatching` responses into its [`SourceBuild`].
/// Pure — no statics, no I/O, no knowledge of any other source.
fn project(mc: &crate::plex::MediaContainer, cw: &crate::plex::MediaContainer) -> SourceBuild {
    const SKIP: [&str; 6] = ["album", "artist", "track", "photo", "clip", "playlist"];
    // need a poster to show it in a shelf
    let keep = |it: &crate::plex::Metadata| {
        let m = parse_item(it);
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
            .filter(|m| !SKIP.contains(&m.kind.as_str()))
            .filter_map(|it| keep(it).map(|m| CwItem { last_viewed_at: it.last_viewed_at, m }))
            .collect();
        if !out.cw.is_empty() {
            break; // the first hub that has anything in it IS the deck
        }
    }

    for hub in &mc.hub {
        if SKIP.contains(&hub.kind.as_str()) {
            continue;
        }
        if hub.hub_identifier == "home.continue" || hub.hub_identifier == "home.ondeck" {
            continue; // superseded by the dedicated hub above
        }
        let items: Vec<PmsMovie> = hub.metadata.iter().filter_map(&keep).collect();
        if items.is_empty() {
            continue;
        }
        out.shelves.push(Shelf { title: hub.title.clone(), hub_id: hub.hub_identifier.clone(), items });
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
/// server alone overruns [`MAX_SHELVES`] and a share behind it draws nothing at all.
///
/// It is water-filling, not a flat `budget / n`: the smallest demand is served first and what it
/// does not use is RE-DIVIDED among the rest, so a modest source costs nobody anything and two
/// greedy ones still split what is left evenly. A leftover pass in source order instead would hand
/// every unclaimed row to whoever came first, which is the starvation this exists to prevent
/// wearing a fairer name. Pure, so the rule is graded on the host rather than inferred from a
/// screenshot.
fn allot(budget: usize, want: &[usize]) -> Vec<usize> {
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
///    annotation (see [`HubRow::source`]).
/// 2. **Every other shelf**, source by source in roster order — the owned server's first, then each
///    shared server's, contiguously, because adjacency is the grouping device. Each keeps its own
///    heading annotation.
///
/// A source that has never answered contributes nothing at all: no heading, no empty shelf, no
/// placeholder row. One that answered and has since failed keeps the shelves it last had, which is
/// the other half of the same rule — a transient failure must not blank a populated Home, and a
/// source that is really gone leaves the ROSTER, which is what drops its shelves.
fn merge(srcs: &[Src]) -> HubBuild {
    let live: Vec<(&str, &SourceBuild)> =
        srcs.iter().filter_map(|s| s.last.as_ref().map(|b| (s.handle.as_str(), b))).collect();

    let mut new_cat: Vec<PmsMovie> = Vec::new();
    let mut new_hubs: Vec<HubRow> = Vec::new();
    // Parallel to `new_cat`: which source each row came from. The hero pool is the only reader, and
    // it needs both halves — whether the row is ours (own items open the rotation) and which server
    // it is from (ratingKeys are server-local integers starting at 1, so two servers collide and a
    // dedup on the key alone would silently drop a borrowed item against an owned one).
    //
    // It is LOCAL, and that is the open seam of this whole feature: `PmsMovie` carries no
    // `ServerId`, so the moment a row leaves here its server is forgotten and every consumer
    // (`posters`, the detail page, `scrobble`, `removeFromContinueWatching`) resolves its ratingKey
    // against whichever server is CURRENT. A borrowed card would draw our poster and mutate our
    // item. That is `docs/shared-servers.md` steps 2 and 3 — threading `ServerId` through the stored
    // structs and moving the call sites onto `client_for(sid)` — and THIS is the line that sets it
    // once it exists. Until then a second source only ever arrives through `/tmp/plxnative-servers`,
    // which is dev-only and compiled out of a release build.
    let mut row_src: Vec<usize> = Vec::new();

    // ---- 1. the merged deck ----
    let mut cw: Vec<(usize, &CwItem)> =
        live.iter().enumerate().flat_map(|(i, (_, b))| b.cw.iter().map(move |c| (i, c))).collect();
    // stable: equal timestamps keep source order, so the owned server wins a tie
    cw.sort_by(|a, b| b.1.last_viewed_at.cmp(&a.1.last_viewed_at));
    cw.truncate(MAX_SHELF_ITEMS);
    for (i, c) in &cw {
        new_cat.push(c.m.clone());
        row_src.push(*i);
    }
    if !new_cat.is_empty() {
        new_hubs.push(HubRow {
            // the hub id the rest of the module matches on (hero-pool eligibility,
            // `hub_is_continue`), rather than the dedicated hub's own "continueWatching"
            title: "Continue Watching".to_string(),
            hub_id: "home.continue".to_string(),
            source: String::new(),
            start: 0,
            len: new_cat.len(),
        });
    }

    // ---- 2. every other shelf, grouped by source ----
    let shelf_want: Vec<usize> = live.iter().map(|(_, b)| b.shelves.len()).collect();
    let row_want: Vec<usize> = live.iter().map(|(_, b)| b.shelves.iter().map(|s| s.items.len()).sum()).collect();
    let shelves_for = allot(MAX_SHELVES - new_hubs.len(), &shelf_want);
    let rows_for = allot(PMS_MAX_MOVIES - new_cat.len(), &row_want);

    for (i, (handle, b)) in live.iter().enumerate() {
        let mut rows_left = rows_for[i];
        for sh in b.shelves.iter().take(shelves_for[i]) {
            let start = new_cat.len();
            for m in sh.items.iter().take(rows_left.min(MAX_SHELF_ITEMS)) {
                new_cat.push(m.clone());
                row_src.push(i);
            }
            rows_left -= new_cat.len() - start;
            if new_cat.len() > start {
                new_hubs.push(HubRow {
                    title: sh.title.clone(),
                    hub_id: sh.hub_id.clone(),
                    source: (*handle).to_string(),
                    start,
                    len: new_cat.len() - start,
                });
            }
        }
    }

    // ---- 3. the rotating hero pool ----
    // Continue Watching items first, then Recently Added, deduped by (source, ratingKey). Require
    // landscape `art` (the hero draws a full-bleed backdrop) and skip seasons (a bare "Season 1"
    // makes a poor billboard). Capped at HERO_MAX.
    let mut new_pool: Vec<usize> = Vec::new();
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
            if new_pool.iter().any(|&j| row_src[j] == row_src[idx] && new_cat[j].rk == m.rk) {
                continue;
            }
            new_pool.push(idx);
        }
    }
    // Own items open the rotation — an ORDERING, not a filter. The pool stays merged and a
    // borrowed film still rotates in seconds later, attributed; what this rules out is the door
    // opening on a stranger's library. Filtering instead would leave an account that has only
    // borrowed sources with no hero at all.
    if let Some(k) = new_pool.iter().position(|&idx| live[row_src[idx]].0.is_empty()) {
        let own = new_pool.remove(k);
        new_pool.insert(0, own);
    }

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
pub(crate) enum HubState {
    /// a fetch is in flight, or the first one hasn't run yet — nothing to show is not an answer
    Loading,
    /// the server answered; the catalog is whatever it says it is (possibly legitimately empty)
    Ready,
    /// the fetch failed — whatever this source last answered with is untouched and [`pump`] is
    /// counting down to the next automatic attempt for it alone
    Failed,
}

/// One SOURCE Home is built from: a registered server, and how a shelf drawn from it names itself.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Source {
    /// The registry slot to fetch from. A slot, never a `&Client`, so a source survives the
    /// registry re-pointing that server at a new address.
    pub(crate) sid: ServerId,
    /// The owner's plex.tv handle ("friend1") for a BORROWED server; **empty** for one of our own.
    /// It is a label, not an identity — [`Source::sid`] is the identity.
    pub(crate) handle: String,
}

/// A source plus everything the fetch state machine knows about it. Main-thread only, behind
/// [`SRCS`].
struct Src {
    sid: ServerId,
    handle: String,
    state: HubState,
    /// Single flight, PER SOURCE. Cleared only where a landing is taken, where the spawn was
    /// refused, or where an authoritative fetch has invalidated everything in flight — drop one of
    /// those and this source never fetches again for the rest of the session.
    fetching: bool,
    /// Bumped by every [`kick`]. A landing whose seq is not the latest is a superseded worker's and
    /// is dropped: [`HUB_GEN`] says "a different account or server set", this says "an older attempt
    /// at the same one", and only the pair rules out both double-applies.
    seq: u32,
    retry_s: f32,
    retry_n: u32,
    /// The projection this source last ANSWERED with, kept across a failure. This is what makes a
    /// failure never blank a populated Home; `None` (never answered) is what makes a dead source
    /// contribute nothing at all — no heading, no empty shelf, no spinner row.
    last: Option<SourceBuild>,
}

impl Src {
    fn new(s: Source) -> Src {
        Src {
            sid: s.sid,
            handle: s.handle,
            state: HubState::Loading,
            fetching: false,
            seq: 0,
            retry_s: 0.0,
            retry_n: 0,
            last: None,
        }
    }
}

/// The source table, in display order: the owned server first, then each shared one. Rebuilt from
/// the roster by [`sync_roster`]; every other access is main-thread, so the lock is never contended
/// and exists to keep the borrow checker (and any future worker) honest rather than to arbitrate.
static SRCS: Mutex<Vec<Src>> = Mutex::new(Vec::new());
fn lock_srcs() -> std::sync::MutexGuard<'static, Vec<Src>> {
    SRCS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The roster the app was TOLD to build Home from ([`set_sources`]). Empty means "nobody has said",
/// and then [`roster`] falls back to the current server alone.
static ROSTER: Mutex<Vec<Source>> = Mutex::new(Vec::new());

/// Bumped by [`set_sources`] and [`reset`] — the half of the roster key a caller moves.
static ROSTER_GEN: AtomicU32 = AtomicU32::new(0);

/// What the source table was last built from: [`ROSTER_GEN`] in the high half, the CURRENT server
/// in the low half. Either moving rebuilds — which is how an `install` that retargets the current
/// server (a re-login resolving a different address) rebuilds without anyone calling anything.
///
/// It is deliberately NOT the registry's size. A slot exists for every address `install` has ever
/// been given, and it keys them by address, so a re-login at a new one for the SAME server leaves
/// two slots; deriving the roster from "every populated slot" would then draw every shelf twice.
/// Only the CURRENT server is implied — a second SOURCE is always something a caller declared.
static SEEN: AtomicU64 = AtomicU64::new(u64::MAX);

fn roster_key() -> u64 {
    ((ROSTER_GEN.load(Ordering::Relaxed) as u64) << 32) | crate::plex::current_server().raw() as u64
}

/// One source's finished (or failed) off-thread fetch. `build: None` deliberately carries no data,
/// so a failure can never be mistaken for "the server returned nothing".
struct Landing {
    gen: u32,
    seq: u32,
    sid: ServerId,
    build: Option<SourceBuild>,
}
static RESULTS: Mutex<Vec<Landing>> = Mutex::new(Vec::new());

/// Bumped by every authoritative fetch ([`reset`] on an identity change, and the blocking install
/// fetch): a worker spawned before it lands with a stale tag and is DROPPED. Without this, a retry
/// in flight across a profile switch could commit the previous account's hubs on top of the new
/// one's — the same late-landing hazard `browse.rs` keys its `GEN` on.
static HUB_GEN: AtomicU32 = AtomicU32::new(0);

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

/// Home's fetch state, folded from every source — what the loading / empty / error read-out reads.
///
/// **Any source answering makes Home answered**, and the total-failure read-out is reserved for the
/// case where every one of them failed. That is the whole point of the fold: "Can't reach your Plex
/// server", drawn because a friend's machine is asleep, is a lie about the library that is working.
/// No source at all (before the first install) is Loading — nothing to show is not an answer.
pub(crate) fn hub_state() -> HubState {
    let s = lock_srcs();
    if s.iter().any(|x| x.state == HubState::Ready) {
        HubState::Ready
    } else if !s.is_empty() && s.iter().all(|x| x.state == HubState::Failed) {
        HubState::Failed
    } else {
        HubState::Loading
    }
}

/// The sources Home is built from, in display order.
///
/// Explicit if the roster layer has said ([`set_sources`]); otherwise just the CURRENT server, with
/// no handle — which is exactly today's single-server Home (`client_opt()` is `current()`), and is
/// what keeps this module working with no caller at all.
fn roster() -> Vec<Source> {
    let explicit = ROSTER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if !explicit.is_empty() {
        return explicit;
    }
    let cur = crate::plex::current_server();
    cur.is_set().then(|| Source { sid: cur, handle: String::new() }).into_iter().collect()
}

/// Declare which servers feed Home, and whose they are. Replaces the roster wholesale, so this is
/// also how a source is un-pinned or a revoked share is dropped: what leaves the roster stops
/// contributing at the next merge, shelves and Continue Watching items alike.
///
/// Own sources are moved to the front (stably, so each group keeps the order it was given in):
/// [`merge`] appends shelves in this order and adjacency is the grouping device, so "own first,
/// then each shared server's, contiguously" is true by construction rather than by convention.
pub(crate) fn set_sources(mut v: Vec<Source>) {
    v.sort_by_key(|s| !s.handle.is_empty()); // stable; false (our own) sorts first
    // slots, never handles: a plex.tv username is the friend's, and the event log is what users send
    crate::log(&format!("hubs: Home is built from source slots {:?}", v.iter().map(|s| s.sid.raw()).collect::<Vec<_>>()));
    *ROSTER.lock().unwrap_or_else(|e| e.into_inner()) = v;
    ROSTER_GEN.fetch_add(1, Ordering::Relaxed);
    sync_roster();
}

/// Bring the source table in line with the roster: a surviving source keeps everything it has
/// (its state, its backoff, and the build it last answered with), a new one arrives Loading and is
/// picked up by the next [`pump`], and one that has left takes its shelves with it.
fn sync_roster() {
    let k = roster_key();
    if SEEN.swap(k, Ordering::Relaxed) == k {
        return;
    }
    let want = roster();
    let mut srcs = lock_srcs();
    let mut out: Vec<Src> = Vec::with_capacity(want.len());
    for w in want {
        // One slot, one source. A roster naming a slot twice is a caller's mistake and reachable
        // (`plex::register` answers with `current()` when the table is full, and an entry with no
        // machineIdentifier at the primary's address ADOPTS the primary slot), but a second `Src`
        // sharing a sid is worse than the mistake: every landing resolves to the first of them, so
        // the other never un-latches its single flight and silently stops fetching for good.
        if out.iter().any(|x| x.sid == w.sid) {
            continue;
        }
        match srcs.iter().position(|x| x.sid == w.sid) {
            Some(i) => {
                let mut keep = srcs.remove(i);
                keep.handle = w.handle;
                out.push(keep);
            }
            None => out.push(Src::new(w)),
        }
    }
    // Whatever is left in `srcs` has left the roster. A worker still out for one of them posts a
    // landing for a sid this table no longer holds, which `pump` drops.
    let dropped = srcs.iter().any(|x| x.last.is_some());
    *srcs = out;
    if dropped {
        let build = merge(&srcs);
        drop(srcs); // before calling out — `detail::reselect` walks the catalog this replaces
        commit(build);
        crate::ui::idle::invalidate();
        crate::ui::detail::reselect(); // the same post-mutation ritual every other commit performs
    }
}

/// Install a finished merge: all three statics move together (they always have — a half-applied
/// catalog once left a stale hero pool floating over emptied shelves).
fn commit(build: HubBuild) -> c_int {
    let (new_cat, new_hubs, new_pool) = build;
    let n = new_cat.len();
    unsafe {
        *std::ptr::addr_of_mut!(CATALOG) = new_cat;
        *std::ptr::addr_of_mut!(HUBS) = new_hubs;
        *std::ptr::addr_of_mut!(HERO_POOL) = new_pool;
    }
    n as c_int
}

/// Record one source's success: it answers with this build from now on, and the backoff retires so
/// its next failure starts at the bottom of the ladder instead of inheriting a 30 s wait.
fn landed_ok(s: &mut Src, b: SourceBuild) {
    s.last = Some(b);
    s.state = HubState::Ready;
    s.retry_n = 0;
    s.retry_s = 0.0;
}

/// Record one source's failure: keep whatever it last answered with and arm ITS next attempt.
fn landed_fail(s: &mut Src) {
    s.retry_n = s.retry_n.saturating_add(1);
    s.retry_s = backoff_secs(s.retry_n);
    s.state = HubState::Failed;
    // the ONE line that says a dead source is dead ON PURPOSE and is coming back — without it the
    // whole recovery is invisible in the event log. The SLOT, never the handle: a plex.tv username
    // is the friend's, and the event log is what users send us.
    crate::log(&format!(
        "hubs: source {} FAILED (attempt {}) — retrying in {:.0}s",
        s.sid.raw(),
        s.retry_n,
        s.retry_s
    ));
}

/// Step one source's retry countdown by `dt` seconds; true when its next attempt is due. Split out
/// so the ladder is testable without spawning a worker or touching a socket.
fn retry_due(s: &mut Src, dt: f32) -> bool {
    let left = s.retry_s - dt;
    s.retry_s = left.max(0.0);
    left <= 0.0
}

/// Spawn an off-thread fetch for ONE source (single flight); [`pump`] lands it. Only the owned
/// server's boot fetch is blocking, and that is exactly what lets Home show a live loading state at
/// all: a blocking fetch on the SDL loop draws no frames while it runs, so the spinner it is
/// supposed to be spinning would never reach the panel.
fn kick(s: &mut Src) {
    if s.fetching {
        return; // one in flight already — its spinner is the honest answer
    }
    // CAPTURE AT THE SPAWN SITE. The worker is handed this server's own `&'static Client`; it never
    // asks which server is current, and a slot re-pointed mid-request cannot redirect a fetch that
    // is already out (`plex::servers` leaks each client precisely so that reference stays live).
    let Some(c) = crate::plex::client_for(s.sid) else {
        landed_fail(s); // a source whose slot holds no client has nothing to contribute
        return;
    };
    s.fetching = true;
    s.state = HubState::Loading;
    s.retry_s = 0.0;
    s.seq = s.seq.wrapping_add(1);
    let (gen, seq, sid) = (HUB_GEN.load(Ordering::SeqCst), s.seq, s.sid);
    let spawned = crate::task::spawn_small("hubs", move || {
        let build = catch_unwind(|| fetch_source(c)).ok().flatten();
        // pushed OUTSIDE the guard so a panicking fetch still lands (as a failure) rather than
        // latching this source's single flight forever
        RESULTS.lock().unwrap_or_else(|e| e.into_inner()).push(Landing { gen, seq, sid, build });
    });
    if !spawned {
        // nothing will ever fill the mailbox (the thread limit refused us), so release the latch
        // here and back off — `pump` will try again on the ladder.
        s.fetching = false;
        landed_fail(s);
    } else {
        crate::log(&format!("hubs: source {} fetching (off-thread)", sid.raw()));
    }
}

/// Try this source again NOW, from the bottom of the ladder.
fn retry_now(s: &mut Src) {
    s.retry_n = 0;
    s.retry_s = 0.0;
    kick(s);
}

/// The Retry control's kick: try every source again NOW, from the bottom of the ladder — a person
/// who asks for it should never be made to sit out a 30-second automatic wait. A no-op for any
/// source whose fetch is already in flight.
pub(crate) fn request_retry() {
    for s in lock_srcs().iter_mut() {
        retry_now(s);
    }
}

/// Retry ONE source — the section read-out's "Try again", which names a single server and must not
/// disturb the others (a share failing is not the app failing).
#[allow(dead_code)] // the failed-section read-out that calls this is the next unit
pub(crate) fn request_retry_source(sid: ServerId) {
    if let Some(s) = lock_srcs().iter_mut().find(|s| s.sid == sid) {
        retry_now(s);
    }
}

/// Once-a-frame main-thread tick: land finished fetches, then count each source down to its next
/// automatic attempt. Driven by `ui::home::home_update`, so it runs exactly while Home is the
/// screen that cares (and never spawns a background fetch behind the player).
pub(crate) fn pump(dt: f32) {
    sync_roster();
    // taken into a `let` FIRST: an `if let`/`for` scrutinee holds its temporary guard for the whole
    // body under edition 2021, which would run the merge with the mailbox still locked — and the
    // worker that fills it must never wait on a main-thread frame
    let landed = std::mem::take(&mut *RESULTS.lock().unwrap_or_else(|e| e.into_inner()));
    let any_landed = !landed.is_empty();
    let cur = HUB_GEN.load(Ordering::SeqCst);
    let mut srcs = lock_srcs();
    let mut dirty = false;
    for l in landed {
        // A landing from before the last authoritative fetch describes a server (or an account) we
        // have since moved off, and one whose seq has been superseded describes an attempt this
        // source has already replaced. Either is dropped whole — neither committed nor blamed.
        let Some(s) = srcs.iter_mut().find(|s| s.sid == l.sid) else {
            continue; // its source left the roster while it was out
        };
        if l.gen != cur || l.seq != s.seq {
            continue;
        }
        s.fetching = false;
        match l.build {
            Some(b) => {
                landed_ok(s, b);
                dirty = true;
            }
            None => landed_fail(s),
        }
    }
    // Anything but Ready with nothing in flight is a state only a fetch can leave: Failed (with a
    // backoff owed) or a Loading whose worker landed stale and was dropped — the latter owes
    // nothing, so it re-kicks on the spot rather than wedging that source on a spinner forever.
    for s in srcs.iter_mut() {
        if s.state != HubState::Ready && !s.fetching && retry_due(s, dt) {
            kick(s);
        }
    }
    let build = dirty.then(|| merge(&srcs));
    drop(srcs); // before calling out: `detail::reselect` walks the catalog this is about to replace
    if any_landed {
        // A landing rewrites the shelves under a Home screen that may have gone idle (a failure
        // repaints too — the status caption changes with it).
        crate::ui::idle::invalidate();
    }
    if let Some(build) = build {
        let n = commit(build);
        crate::log(&format!("hubs: landed — {n} items, {} shelves", hub_count()));
        // the same post-mutation ritual `refetch_hubs_reconcile` performs: an open detail page
        // re-resolves its selected row against the rebuilt catalog
        crate::ui::detail::reselect();
    }
}

/// A source that has answered with `n` placeholder rows in one shelf (test fixture). Only the SHAPE
/// is real — `project` would have dropped these rows for having no title/poster; what the tests
/// using it assert is the landing/merge bookkeeping, which never looks inside a row.
#[cfg(test)]
fn build_test(n: usize) -> SourceBuild {
    SourceBuild {
        cw: Vec::new(),
        shelves: vec![Shelf {
            title: "Continue Watching".into(),
            hub_id: "home.continue".into(),
            items: (0..n).map(|_| PmsMovie::default()).collect(),
        }],
    }
}

/// Test hook: put the store in a known place — one source in `state`, having answered with `items`
/// rows in one shelf. Home's read-out is a pure projection of that pair, and the states a host test
/// cannot reach for real (a live server answering, or refusing) are exactly the ones worth pinning.
#[cfg(test)]
pub(crate) fn seed_for_test(items: usize, state: HubState) {
    reset();
    let mut s = Src::new(Source { sid: ServerId::UNSET, handle: String::new() });
    s.state = state;
    if items > 0 {
        s.last = Some(build_test(items));
    }
    let srcs = vec![s];
    let build = merge(&srcs);
    *lock_srcs() = srcs;
    // the registry is empty in a host test, so leave `sync_roster` believing it is up to date —
    // otherwise the next `pump` would replace this synthetic source with nothing
    SEEN.store(roster_key(), Ordering::Relaxed);
    commit(build);
}

/// Drop everything and re-arm the fetch — the identity-change twin of [`crate::browse::reset`],
/// called from the same place (`install_pms`). Now that a failed fetch KEEPS the previous build,
/// a profile switch whose fetch fails would otherwise leave the previous user's shelves on screen;
/// this is the one place that must still wipe them.
///
/// The declared roster goes too: which servers were shared with the account that just signed out is
/// not a fact about the one signing in. The next [`sync_roster`] derives a fresh one from the
/// registry, and the roster layer replaces it when it has looked plex.tv up again.
pub(crate) fn reset() {
    HUB_GEN.fetch_add(1, Ordering::SeqCst); // a worker still running belongs to the old identity
    *RESULTS.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
    *ROSTER.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
    *lock_srcs() = Vec::new();
    ROSTER_GEN.fetch_add(1, Ordering::Relaxed);
    SEEN.store(u64::MAX, Ordering::Relaxed);
    commit((Vec::new(), Vec::new(), Vec::new()));
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    // NB every test that touches the catalog statics holds the crate-wide serial lock: they are
    // read from other modules' tests too (`ui::home` walks `hub_len`), which a module-local mutex
    // cannot see. `reset()` doubles as the teardown.
    //
    // No test here may leave a source in a state `pump` would KICK (not Ready, not fetching, no
    // backoff owed): a kick reaches for `plex::client_for`, and a slot another module's test
    // registered would send a real worker at a real address.

    /// One source of the table, seeded directly. `handle` empty = a server of our own.
    fn src(slot: u16, handle: &str, state: HubState, last: Option<SourceBuild>) -> Src {
        let mut s = Src::new(Source { sid: ServerId::from_raw(slot), handle: handle.into() });
        s.state = state;
        s.last = last;
        s
    }

    /// Install a source table and the merge of it — the state a run of landings would have reached.
    /// Bypasses the roster, because a host test has no server registry to derive one from.
    fn seed(srcs: Vec<Src>) {
        let build = merge(&srcs);
        *lock_srcs() = srcs;
        SEEN.store(roster_key(), Ordering::Relaxed); // leave `sync_roster` idle
        commit(build);
    }

    /// A landing from the worker this source has out right now (current generation and seq) — what
    /// a real fetch would post. `None` is a failure.
    fn land(slot: u16, build: Option<SourceBuild>) {
        let sid = ServerId::from_raw(slot);
        let seq = lock_srcs().iter().find(|s| s.sid == sid).map(|s| s.seq).unwrap_or(0);
        RESULTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Landing { gen: HUB_GEN.load(Ordering::SeqCst), seq, sid, build });
    }

    /// A drawable catalog row. `thumb` is what `project` requires of a row, `art` what the hero
    /// pool requires of one.
    fn row(rk: &str) -> PmsMovie {
        PmsMovie { rk: rk.into(), title: rk.into(), thumb: "/t".into(), art: "/a".into(), ..Default::default() }
    }
    fn shelf(title: &str, hub_id: &str, rks: &[&str]) -> Shelf {
        Shelf { title: title.into(), hub_id: hub_id.into(), items: rks.iter().map(|r| row(r)).collect() }
    }
    /// A source's projection: `(lastViewedAt, rk)` deck entries plus whole shelves.
    fn built(cw: &[(i64, &str)], shelves: Vec<Shelf>) -> SourceBuild {
        SourceBuild {
            cw: cw.iter().map(|&(t, r)| CwItem { last_viewed_at: t, m: row(r) }).collect(),
            shelves,
        }
    }
    /// The ratingKeys of shelf `h`, in drawn order.
    fn rks(h: usize) -> Vec<String> {
        (0..hub_len(h)).filter_map(|c| hub_item(h, c)).map(|m| m.rk.clone()).collect()
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

    /// The bug the fetch state machine exists for: a failed fetch used to commit an EMPTY catalog,
    /// so one unreachable moment blanked a populated Home for good. A failure must leave every one
    /// of the three statics exactly as it found them.
    #[test]
    fn a_failed_landing_never_blanks_a_populated_home() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![src(0, "", HubState::Ready, Some(build_test(3)))]);
        assert_eq!(hub_count(), 1);
        assert_eq!(hub_len(0), 3);

        land(0, None);
        pump(0.0);

        assert_eq!(hub_state(), HubState::Failed, "the failure must be distinguishable");
        assert_eq!(hub_count(), 1, "the shelves survive a failed refetch");
        assert_eq!(hub_len(0), 3);
        assert!(lock_srcs()[0].retry_s > 0.0, "and the next attempt is armed");
        reset();
    }

    /// A landing that carries a build commits it, and a success retires that source's backoff so
    /// its next failure starts at the bottom of the ladder instead of inheriting a 30s wait.
    #[test]
    fn a_successful_landing_commits_and_retires_the_backoff() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![src(0, "", HubState::Loading, None)]);
        {
            let mut s = lock_srcs();
            landed_fail(&mut s[0]);
            landed_fail(&mut s[0]);
        }
        assert_eq!(hub_state(), HubState::Failed);

        land(0, Some(build_test(2)));
        pump(0.0);

        assert_eq!(hub_state(), HubState::Ready);
        assert_eq!(hub_len(0), 2);
        assert_eq!(lock_srcs()[0].retry_n, 0);
        assert_eq!(lock_srcs()[0].retry_s, 0.0);
        reset();
    }

    /// An answer of "nothing" is an ANSWER: it must land as Ready (Home's empty state), not as a
    /// failure, or the screen would apologise for a server that is simply empty — and retry it
    /// forever.
    #[test]
    fn a_server_with_no_hubs_is_ready_and_empty_not_failed() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![src(0, "", HubState::Loading, None)]);
        land(0, Some(SourceBuild::default()));
        pump(0.0);
        assert_eq!(hub_state(), HubState::Ready);
        assert_eq!(hub_count(), 0);
        reset();
    }

    /// The countdown is real time (seconds of `dt`), not frames like `browse.rs`'s — a device
    /// that drops to 30fps must still retry on the same wall clock. NB `retry_due` keeps
    /// reporting due once it is spent; what makes an attempt happen only once is `kick`
    /// re-latching that source's single flight, not this.
    #[test]
    fn the_retry_countdown_fires_when_the_backoff_elapses() {
        let mut s = src(0, "", HubState::Loading, None);
        landed_fail(&mut s); // arms RETRY_MIN_S
        for _ in 0..3 {
            assert!(!retry_due(&mut s, 0.5), "0.5s at a time must not fire before the 2s wait is spent");
        }
        assert!(retry_due(&mut s, 0.5), "the fourth half-second spends it");
    }

    /// A retry still in flight when the account changes must not commit the PREVIOUS identity's
    /// hubs over the new one's: its landing carries the old generation and is dropped whole —
    /// neither committed nor blamed on the current fetch. The same drop catches a SUPERSEDED
    /// attempt at the same source within one generation, which is what the per-source seq is for:
    /// an authoritative fetch releases every single-flight latch, so without it the worker that
    /// call abandoned could still land on top of the one that replaced it.
    #[test]
    fn a_stale_or_superseded_landing_is_dropped_whole() {
        let _g = crate::testlock::serial();
        reset();
        let stale = HUB_GEN.load(Ordering::SeqCst);
        reset(); // the identity change
        seed(vec![src(0, "", HubState::Ready, Some(build_test(2)))]); // …and the new identity's catalog
        let sid = ServerId::from_raw(0);
        let cur = HUB_GEN.load(Ordering::SeqCst);
        {
            let mut r = RESULTS.lock().unwrap_or_else(|e| e.into_inner());
            r.push(Landing { gen: stale, seq: 0, sid, build: Some(build_test(9)) });
            r.push(Landing { gen: cur, seq: 99, sid, build: Some(build_test(7)) });
        }
        pump(0.0);
        assert_eq!(hub_len(0), 2, "neither may replace the current catalog");
        assert_eq!(hub_state(), HubState::Ready, "nor be counted as a failure of the current one");
        reset();
    }

    /// `reset` is the profile-switch wipe: the previous user's shelves must not survive it, and
    /// the state machine must come back as a fresh boot's (Loading, no source, no backoff owed).
    #[test]
    fn reset_wipes_the_catalog_and_re_arms_the_fetch() {
        let _g = crate::testlock::serial();
        seed(vec![src(0, "", HubState::Ready, Some(build_test(4)))]);
        reset();
        assert_eq!(hub_count(), 0);
        assert_eq!(catalog().len(), 0);
        assert_eq!(hero_pool_len(), 0);
        assert_eq!(hub_state(), HubState::Loading);
        assert!(lock_srcs().is_empty());
        assert!(ROSTER.lock().unwrap().is_empty(), "the roster belongs to the account that left");
    }

    // ---- what a SECOND source changes -----------------------------------------------------------

    /// The failure this unit exists for. Home used to be one `?` chain over one server, so a single
    /// dead share aborted the whole build: nothing committed, and on a cold boot the user got a
    /// whole-screen "Can't reach your Plex server" about their own working library. A source's
    /// verdict is now its own — the one that answered commits, the one that failed contributes
    /// nothing and backs off alone.
    #[test]
    fn one_failing_source_still_commits_the_other() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![src(0, "", HubState::Loading, None), src(1, "friend1", HubState::Loading, None)]);

        land(0, Some(built(&[], vec![shelf("Recently Added", "home.movies.recent", &["a1", "a2"])])));
        land(1, None);
        pump(0.0);

        assert_eq!(hub_state(), HubState::Ready, "one server answering is an answered Home");
        assert_eq!(hub_count(), 1, "the dead share contributes NOTHING — no heading, no empty shelf");
        assert_eq!(hub_len(0), 2);
        assert_eq!(hub_source(0), "", "and the shelf that did arrive is our own, so it is unannotated");
        let s = lock_srcs();
        assert_eq!(s[0].state, HubState::Ready);
        assert_eq!(s[1].state, HubState::Failed);
        assert!(s[1].retry_s > 0.0, "the share retries on its own ladder…");
        assert_eq!(s[0].retry_s, 0.0, "…and the working server owes nothing");
        drop(s);
        reset();
    }

    /// …and the same rule once BOTH sources are populated, which is the state a user is actually
    /// sitting in front of when a friend's server goes to sleep. The failing source keeps the
    /// shelves it last answered with, the working source is not touched at all, and neither the
    /// catalog nor the deck loses a row. There is no screenshot of this: it is arithmetic over two
    /// projections, and a photograph of a Home screen that happened to look right proves nothing
    /// about which of the two the rows came from.
    #[test]
    fn a_failing_source_leaves_a_populated_home_completely_intact() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![
            src(0, "", HubState::Ready, Some(built(&[(9, "own-cw")], vec![shelf("Films", "a.recent", &["a1", "a2"])]))),
            src(1, "friend1", HubState::Ready, Some(built(&[(5, "their-cw")], vec![shelf("LDN Films", "b.recent", &["b1"])]))),
        ]);
        let before: Vec<(&str, &str, usize)> =
            (0..hub_count()).map(|i| (hub_title(i), hub_source(i), hub_len(i))).collect();
        assert_eq!(before.len(), 3, "deck + one shelf each");
        let rows = catalog().len();

        land(1, None); // the share stops answering
        pump(0.0);

        let after: Vec<(&str, &str, usize)> =
            (0..hub_count()).map(|i| (hub_title(i), hub_source(i), hub_len(i))).collect();
        assert_eq!(after, before, "not one shelf, heading or row moved");
        assert_eq!(catalog().len(), rows);
        assert_eq!(rks(0), ["own-cw", "their-cw"], "and the merged deck kept both servers' items");
        assert_eq!(hub_state(), HubState::Ready, "Home is not failed while a source is answering");
        assert_eq!(lock_srcs()[0].retry_s, 0.0, "the working source owes no backoff");
        reset();
    }

    /// A PARTIAL landing repaints. A settled Home stops presenting entirely (`ui::idle`), so a
    /// source arriving seconds after the owned server did — which is the normal shape of this
    /// feature, not an edge case — would otherwise draw its shelves invisibly until the next
    /// keypress. The failure half matters just as much: a retry that fails changes the status
    /// caption under an empty Home.
    #[test]
    fn a_source_landing_repaints_a_settled_home() {
        let _g = crate::testlock::serial();
        reset();
        crate::ui::idle::set_enabled(true);
        seed(vec![src(0, "", HubState::Ready, Some(build_test(1))), src(1, "friend1", HubState::Loading, None)]);

        for (what, build) in [("a share arriving", Some(build_test(2))), ("a share failing", None)] {
            crate::ui::idle::should_present(0); // takes-and-clears whatever was already pending
            assert!(!crate::ui::idle::should_present(0), "the panel is settled with nothing happening");
            land(1, build);
            pump(0.0);
            assert!(crate::ui::idle::should_present(0), "{what} must invalidate the frame");
        }
        reset();
    }

    /// The total-failure read-out is reserved for a total failure. Any source answering makes Home
    /// answered; a mix of failed and still-loading is still loading.
    #[test]
    fn only_every_source_failing_reads_as_a_failed_home() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![src(0, "", HubState::Failed, None), src(1, "friend1", HubState::Loading, None)]);
        assert_eq!(hub_state(), HubState::Loading, "one source still trying is not a dead Home");

        seed(vec![src(0, "", HubState::Failed, None), src(1, "friend1", HubState::Ready, None)]);
        assert_eq!(hub_state(), HubState::Ready, "a share being down says nothing about our own");

        seed(vec![src(0, "", HubState::Failed, None), src(1, "friend1", HubState::Failed, None)]);
        assert_eq!(hub_state(), HubState::Failed, "everything down IS the whole-screen case");
        reset();
    }

    /// Continue Watching is ONE shelf across every source, ordered by when the owner last watched —
    /// so a borrowed item legitimately holds first position, and the heading therefore cannot claim
    /// an owner. It carries no annotation at all.
    #[test]
    fn continue_watching_merges_across_sources_by_last_viewed() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![
            src(0, "", HubState::Ready, Some(built(&[(300, "own-old"), (100, "own-oldest")], vec![]))),
            src(1, "friend1", HubState::Ready, Some(built(&[(900, "their-new"), (200, "their-mid")], vec![]))),
        ]);

        assert_eq!(hub_count(), 1, "one deck, not one per server");
        assert!(hub_is_continue(0));
        assert_eq!(hub_source(0), "", "a shelf drawn from two servers cannot be named by one of them");
        // The timestamps INTERLEAVE on purpose: a per-source concatenation, which is what the
        // obvious implementation of "merge" does, would give own-old, own-oldest, their-new,
        // their-mid and pass any test written with one server's deck in front of the other's.
        assert_eq!(rks(0), ["their-new", "own-old", "their-mid", "own-oldest"]);
        reset();
    }

    /// Every OTHER shelf keeps its source: the owner's handle for a borrowed server, empty for our
    /// own. And the groups stay contiguous in roster order — adjacency is the grouping device, so a
    /// source's shelves may never be interleaved with another's.
    #[test]
    fn every_other_shelf_carries_its_source_and_the_groups_stay_contiguous() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![
            src(0, "", HubState::Ready, Some(built(&[(1, "cw")], vec![shelf("Films", "a.recent", &["a"])]))),
            src(1, "friend1", HubState::Ready, Some(built(&[], vec![shelf("LDN Films", "b.recent", &["b"]), shelf("LDN TV", "b.tv", &["b2"])]))),
            src(2, "friend2", HubState::Ready, Some(built(&[], vec![shelf("Docs", "c.recent", &["c"])]))),
        ]);

        let by_row: Vec<(&str, &str)> = (0..hub_count()).map(|i| (hub_title(i), hub_source(i))).collect();
        assert_eq!(
            by_row,
            [("Continue Watching", ""), ("Films", ""), ("LDN Films", "friend1"), ("LDN TV", "friend1"), ("Docs", "friend2")],
            "deck first, then our own, then each share whole"
        );
        reset();
    }

    /// A source that has never answered contributes nothing — no heading, no empty shelf, no
    /// spinner row (which would also hold the panel presenting for a source that isn't coming).
    /// One that HAS answered and has since failed keeps what it last had: a transient failure must
    /// not reflow the shelves under the focus ring.
    #[test]
    fn a_source_that_never_answered_draws_nothing_at_all() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![
            src(0, "", HubState::Ready, Some(built(&[], vec![shelf("Films", "a.recent", &["a"])]))),
            src(1, "friend1", HubState::Failed, None),
            src(2, "friend2", HubState::Failed, Some(built(&[], vec![shelf("Docs", "c.recent", &["c"])]))),
        ]);
        let by_row: Vec<(&str, &str)> = (0..hub_count()).map(|i| (hub_title(i), hub_source(i))).collect();
        assert_eq!(by_row, [("Films", ""), ("Docs", "friend2")]);
        reset();
    }

    /// A source that leaves the roster takes its shelves with it at the next read — the one thing
    /// that DOES remove a live source's rows, because "gone" is a fact about the grant, not about a
    /// fetch that happened to fail.
    #[test]
    fn a_source_that_leaves_the_roster_stops_contributing() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![
            src(0, "", HubState::Ready, Some(built(&[], vec![shelf("Films", "a.recent", &["a"])]))),
            src(1, "friend1", HubState::Ready, Some(built(&[], vec![shelf("LDN Films", "b.recent", &["b"])]))),
        ]);
        assert_eq!(hub_count(), 2);

        set_sources(vec![Source { sid: ServerId::from_raw(0), handle: String::new() }]);

        assert_eq!(hub_count(), 1, "the un-pinned source's shelf is gone");
        assert_eq!(hub_title(0), "Films");
        assert_eq!(lock_srcs().len(), 1);
        reset();
    }

    /// `set_sources` puts our own servers first whatever order it is handed, because the merge
    /// appends in roster order and the grouping is positional.
    #[test]
    fn set_sources_puts_our_own_servers_first() {
        let _g = crate::testlock::serial();
        reset();
        set_sources(vec![
            Source { sid: ServerId::from_raw(3), handle: "friend1".into() },
            Source { sid: ServerId::from_raw(1), handle: String::new() },
            Source { sid: ServerId::from_raw(2), handle: "friend2".into() },
        ]);
        let order: Vec<(u16, String)> = lock_srcs().iter().map(|s| (s.sid.raw(), s.handle.clone())).collect();
        assert_eq!(order, [(1, String::new()), (3, "friend1".into()), (2, "friend2".into())]);
        reset();
    }

    /// The budget is split, not raced for. Whoever is asked first used to spend it — and `/hubs`
    /// promotes several rows per library, so one four-library server already overruns
    /// [`MAX_SHELVES`] and the share behind it drew nothing.
    #[test]
    fn the_budget_is_shared_so_neither_source_starves_the_other() {
        assert_eq!(allot(10, &[4, 4]), [4, 4], "a budget nobody exhausts is not rationed");
        assert_eq!(allot(10, &[99, 99]), [5, 5], "two greedy sources split it evenly");
        assert_eq!(allot(10, &[2, 99]), [2, 8], "what one does not want is passed on, not wasted");
        assert_eq!(allot(10, &[99, 0, 99]), [5, 0, 5], "…and RE-DIVIDED, not given to whoever is first");
        assert_eq!(allot(1, &[9, 9, 9]), [1, 0, 0], "a budget below one each reaches as many as it can");
        assert_eq!(allot(10, &[]), Vec::<usize>::new());

        let _g = crate::testlock::serial();
        reset();
        let many = |tag: &str| {
            (0..30).map(|i| shelf(&format!("{tag}{i}"), "x", &["r"])).collect::<Vec<_>>()
        };
        seed(vec![
            src(0, "", HubState::Ready, Some(built(&[], many("a")))),
            src(1, "friend1", HubState::Ready, Some(built(&[], many("b")))),
        ]);
        assert_eq!(hub_count(), MAX_SHELVES, "the total cap still holds");
        let theirs = (0..hub_count()).filter(|&i| hub_source(i) == "friend1").count();
        assert_eq!(theirs, MAX_SHELVES / 2, "and the share gets its half rather than the leftovers");
        reset();
    }

    /// The same split over catalog ROWS, which is the cap the shelves' items come out of.
    #[test]
    fn the_row_budget_is_shared_too() {
        let _g = crate::testlock::serial();
        reset();
        // enough shelves, each already at the per-shelf ceiling, that the ROW cap is what binds
        let fat = |tag: &str| {
            (0..20)
                .map(|s| {
                    let rks: Vec<String> = (0..MAX_SHELF_ITEMS).map(|i| format!("{tag}-{s}-{i}")).collect();
                    shelf(&format!("{tag}{s}"), "x.recent", &rks.iter().map(|r| r.as_str()).collect::<Vec<_>>())
                })
                .collect::<Vec<_>>()
        };
        seed(vec![
            src(0, "", HubState::Ready, Some(built(&[], fat("a")))),
            src(1, "friend1", HubState::Ready, Some(built(&[], fat("b")))),
        ]);
        assert_eq!(catalog().len(), PMS_MAX_MOVIES, "the total cap still holds");
        let theirs: usize = (0..hub_count()).filter(|&i| hub_source(i) == "friend1").map(hub_len).sum();
        assert_eq!(theirs, PMS_MAX_MOVIES / 2, "and the share gets half the rows, not the leftovers");
        reset();
    }

    /// The merged deck is capped at what the grid can ADDRESS. Three sources' Continue Watching is
    /// up to 36 cards, and past `MAX_SHELF_ITEMS` `ui::home`'s focus ring and its OK dispatch clamp
    /// differently — the ring stops at the last addressable card while the press opens whatever
    /// column the raw index names. Unreachable with one server, which is why the cap lives here now.
    #[test]
    fn the_merged_deck_is_capped_at_what_the_grid_can_address() {
        let _g = crate::testlock::serial();
        reset();
        let deck = |tag: &str| {
            let v: Vec<(i64, String)> = (0..12).map(|i| (i, format!("{tag}{i}"))).collect();
            built(&v.iter().map(|(t, r)| (*t, r.as_str())).collect::<Vec<_>>(), vec![])
        };
        seed(vec![
            src(0, "", HubState::Ready, Some(deck("a"))),
            src(1, "b", HubState::Ready, Some(deck("b"))),
            src(2, "c", HubState::Ready, Some(deck("c"))),
        ]);
        assert_eq!(hub_count(), 1);
        assert_eq!(hub_len(0), MAX_SHELF_ITEMS, "36 cards merged, 24 drawable");
        reset();
    }

    /// Two things the hero pool can only get right by knowing which SOURCE a row came from: our own
    /// items open the rotation (an ordering, not a filter — the pool stays merged), and two
    /// servers' identical ratingKeys are two different films, so the dedup must not collapse them.
    /// Both are live faults, not theory: ratingKeys are server-local integers starting at 1.
    #[test]
    fn the_hero_pool_opens_on_our_own_item_and_never_dedups_across_servers() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![
            src(0, "", HubState::Ready, Some(built(&[(100, "1")], vec![]))),
            src(1, "friend1", HubState::Ready, Some(built(&[(900, "1")], vec![]))),
        ]);
        assert_eq!(rks(0), ["1", "1"], "the deck orders them by recency: theirs first");
        assert_eq!(hero_pool_len(), 2, "one ratingKey, two servers, two films");
        let hero = hero_pool_item(0).expect("a hero");
        assert!(
            std::ptr::eq(hero, movie(1).unwrap()),
            "catalog row 1 is ours (row 0 is theirs, watched later): the door opens on our own \
             library and the borrowed film rotates in behind it"
        );
        reset();
    }
}
