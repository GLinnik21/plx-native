//! Plex library fetch/parse into the private catalog (was src/pms.c), read by the UI
//! via movie()/hub_item()/hero_pool_item(), plus urlenc_str (shared by posters/route).
//! The fetch + JSON parse go through the typed `crate::plex` client (serde DTOs) — no
//! hand-built paths or `Value` scraping here.
use std::os::raw::c_int;
use std::panic::catch_unwind;

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
fn parse_item(it: &crate::plex::Metadata) -> PmsMovie {
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
    // UltraBlurColors -> ambient gradient. The typed model (de_ultrablur) accepts BOTH the
    // array and object shapes PMS returns; the old object-only read missed the array form
    // (D-1), so blur now populates where it was previously blank. Guard against a
    // malformed/empty envelope (corners defaulted to black) so we don't flag a pure-black
    // gradient as present — the old code keyed this on topLeft being a present string.
    if let Some(ub) = it.ultra_blur_colors {
        let blur = [ub.top_left.0, ub.top_right.0, ub.bottom_right.0, ub.bottom_left.0];
        if blur.iter().any(|c| *c != [0.0, 0.0, 0.0]) {
            m.blur = blur;
            m.has_blur = true;
        }
    }
    m
}

// The full-library browse path (fetch every section's items via the typed
// `client().sections()` + `.section_items()`) was removed as dead code — the home is
// hub-driven (`pms_fetch_hubs`). Those two client methods remain available for a future
// A-Z "Library" screen; re-add a ~30-line consumer here when one is built.

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
/// the catalog + the HUBS grouping. Skips music/photo/playlist hubs + empty ones.
/// Builds the new catalog/hubs/pool locally, then commits all three statics at once.
/// A failed fetch commits empty, blanking the home consistently (the old clear-first
/// code emptied the catalog/shelves but left a stale hero pool floating over them).
pub(crate) fn pms_fetch_hubs() -> c_int {
    let r = catch_unwind(|| {
        let mut new_cat: Vec<PmsMovie> = Vec::new();
        let mut new_hubs: Vec<HubRow> = Vec::new();
        let mut new_pool: Vec<usize> = Vec::new();
        if let Some(mc) = crate::plex::client().home_hubs(12) {
            const SKIP: [&str; 6] = ["album", "artist", "track", "photo", "clip", "playlist"];

            // Build the display shelves as ordered lists of item refs. Continue Watching
            // (home.continue) and On Deck (home.ondeck) are merged into one "Continue
            // Watching" shelf — the official-app behaviour: in-progress items unified with
            // next-up episodes, deduped by ratingKey (the same episode carries one rk in
            // both hubs; a next-up episode has its own). The merged shelf is then sorted by
            // lastViewedAt desc so "most recently played" leads, interleaving resume points
            // and next-up by recency rather than by which hub they came from.
            let mut shelves: Vec<(String, String, Vec<&crate::plex::Metadata>)> = Vec::new(); // (title, hubIdentifier, items)
            let mut cw_idx: Option<usize> = None; // shelves slot of the merged Continue Watching
            let mut cw_seen: Vec<&str> = Vec::new(); // ratingKeys already merged in
            for hub in &mc.hub {
                if SKIP.contains(&hub.kind.as_str()) || hub.metadata.is_empty() {
                    continue;
                }
                if hub.hub_identifier == "home.continue" || hub.hub_identifier == "home.ondeck" {
                    if cw_idx.is_none() {
                        // synthetic id: the merged shelf spans home.continue + home.ondeck; tag it with
                        // the former so the hero-pool eligibility can recognize it locale-independently.
                        shelves.push(("Continue Watching".to_string(), "home.continue".to_string(), Vec::new()));
                        cw_idx = Some(shelves.len() - 1);
                    }
                    let si = cw_idx.unwrap();
                    for item in &hub.metadata {
                        if SKIP.contains(&item.kind.as_str()) {
                            continue;
                        }
                        let rk = item.rating_key.as_str();
                        if !rk.is_empty() && cw_seen.contains(&rk) {
                            continue;
                        }
                        if !rk.is_empty() {
                            cw_seen.push(rk);
                        }
                        shelves[si].2.push(item);
                    }
                } else {
                    shelves.push((hub.title.clone(), hub.hub_identifier.clone(), hub.metadata.iter().collect()));
                }
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
        }
        let n = new_cat.len();
        unsafe {
            *std::ptr::addr_of_mut!(CATALOG) = new_cat;
            *std::ptr::addr_of_mut!(HUBS) = new_hubs;
            *std::ptr::addr_of_mut!(HERO_POOL) = new_pool;
        }
        n as c_int
    });
    r.unwrap_or(0)
}
