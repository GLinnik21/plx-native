//! Plex library fetch/parse into the private catalog (was src/pms.c), read by the UI
//! via movie_ptr()/nmovies(), plus urlenc_str (shared by posters/route). The fetch +
//! JSON parse go through the typed `crate::plex` client (serde DTOs) — no hand-built
//! paths or `Value` scraping here.
#![allow(non_upper_case_globals)]
use std::os::raw::c_int;
use std::panic::catch_unwind;

const PMS_MAX_MOVIES: usize = 256;

// A catalog row. Fields pub(crate) so the UI / route / player read them; they carry
// NUL-terminated C strings in fixed buffers.
pub struct PmsMovie {
    pub(crate) title: [u8; 128],
    pub(crate) year: c_int,
    pub(crate) rating: [u8; 12],
    pub(crate) dur_ns: i64,
    pub(crate) part: [u8; 256],
    pub(crate) thumb: [u8; 128],
    pub(crate) art: [u8; 128],
    pub(crate) summary: [u8; 600],
    pub(crate) rk: [u8; 16],
    pub(crate) vcodec: [u8; 12],
    pub(crate) acodec: [u8; 12],
    pub(crate) blur: [[f32; 3]; 4],
    pub(crate) has_blur: c_int,
    pub(crate) kind: c_int,     // 0 = movie, 1 = show, 2 = season, 3 = episode
    pub(crate) resume_ms: i64,  // viewOffset — drives the Continue Watching resume bar
    pub(crate) show_rk: [u8; 16],   // parent show rk (episode: grandparent; season: parent)
    pub(crate) season_index: c_int, // season number (episode: parentIndex; season: index)
    pub(crate) show_title: [u8; 128], // episode only: grandparentTitle (the hero headlines the SHOW)
    pub(crate) ep_index: c_int,       // episode only: episode number within the season
}
impl PmsMovie {
    const ZERO: PmsMovie = PmsMovie {
        title: [0; 128], year: 0, rating: [0; 12], dur_ns: 0, part: [0; 256],
        thumb: [0; 128], art: [0; 128], summary: [0; 600], rk: [0; 16],
        vcodec: [0; 12], acodec: [0; 12], blur: [[0.0; 3]; 4], has_blur: 0, kind: 0, resume_ms: 0,
        show_rk: [0; 16], season_index: 0, show_title: [0; 128], ep_index: 0,
    };
}

// The catalog (private; the UI reads it through movie_ptr()/nmovies()).
static mut pms_movies: [PmsMovie; PMS_MAX_MOVIES] = [PmsMovie::ZERO; PMS_MAX_MOVIES];
static mut pms_nmovies: c_int = 0;

/// pointer to catalog row `i` (unchecked; caller ensures i < nmovies())
pub(crate) fn movie_ptr(i: usize) -> *mut PmsMovie {
    unsafe { (std::ptr::addr_of_mut!(pms_movies) as *mut PmsMovie).add(i) }
}
/// number of movies currently in the catalog
pub(crate) fn nmovies() -> usize {
    unsafe { std::ptr::addr_of!(pms_nmovies).read() as usize }
}
/// catalog index whose ratingKey == `rk` (for the /tmp/plxnative-detail probe), or -1
pub(crate) fn index_of_rk(rk: &str) -> c_int {
    for i in 0..nmovies() {
        let m = unsafe { &*movie_ptr(i) };
        if std::str::from_utf8(crate::cbuf::as_bytes(&m.rk)).ok() == Some(rk) {
            return i as c_int;
        }
    }
    -1
}

// ---- helpers ----
/// copy `s` into a fixed C char buffer (truncate, NUL-terminate, newlines->spaces)
fn set_field(dst: &mut [u8], s: &str) {
    if dst.is_empty() {
        return;
    }
    let cleaned: Vec<u8> = s.bytes().map(|b| if b == b'\n' || b == b'\r' { b' ' } else { b }).collect();
    let n = cleaned.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&cleaned[..n]);
    dst[n] = 0;
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
fn parse_item(m: &mut PmsMovie, it: &crate::plex::Metadata) {
    *m = PmsMovie::ZERO;
    m.kind = match it.kind.as_str() {
        "show" => 1,
        "season" => 2,
        "episode" => 3,
        _ => 0,
    };
    match m.kind {
        3 => {
            // episode: parent show = grandparent, season number = parentIndex
            set_field(&mut m.show_rk, &it.grandparent_rating_key);
            m.season_index = it.parent_index as c_int;
            set_field(&mut m.show_title, &it.grandparent_title);
            m.ep_index = it.index as c_int;
        }
        2 => {
            // season: parent show = parent, season number = index
            set_field(&mut m.show_rk, &it.parent_rating_key);
            m.season_index = it.index as c_int;
        }
        _ => {}
    }
    set_field(&mut m.title, &it.title);
    m.year = it.year as c_int;
    set_field(&mut m.rating, &it.content_rating);
    m.dur_ns = if it.duration > 0 { it.duration * 1_000_000 } else { 0 };
    m.resume_ms = it.view_offset;
    // poster: prefer the show poster for episodes (grandparentThumb) so a landscape
    // episode still doesn't fill a portrait card
    let thumb = if it.grandparent_thumb.is_empty() { &it.thumb } else { &it.grandparent_thumb };
    set_field(&mut m.thumb, thumb);
    set_field(&mut m.art, &it.art);
    set_field(&mut m.summary, &it.summary);
    set_field(&mut m.rk, &it.rating_key);
    // Media[0]: codecs + Part[0].key (movies/episodes; a show container has none)
    if let Some(md) = it.media.first() {
        set_field(&mut m.vcodec, &md.video_codec);
        set_field(&mut m.acodec, &md.audio_codec);
        if let Some(p0) = md.part.first() {
            set_field(&mut m.part, &p0.key);
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
            m.has_blur = 1;
        }
    }
}

// The full-library browse path (fetch every section's items via the typed
// `client().sections()` + `.section_items()`) was removed as dead code — the home is
// hub-driven (`pms_fetch_hubs`). Those two client methods remain available for a future
// A-Z "Library" screen; re-add a ~30-line consumer here when one is built.

// ---- home hubs: each hub is a titled slice of the catalog (pms_movies) ----
struct HubRow {
    title: String,
    hub_id: String, // locale-independent hubIdentifier ("home.continue", "home.movies.recent", …)
    start: usize,
    len: usize,
}
static mut HUBS: Vec<HubRow> = Vec::new();

// ---- rotating hero pool: curated catalog indices (Continue Watching then Recently Added) ----
const HERO_MAX: usize = 8;
static mut HERO_POOL: Vec<usize> = Vec::new();

/// number of items in the rotating hero pool
pub(crate) fn hero_pool_len() -> usize {
    unsafe { std::ptr::addr_of!(HERO_POOL).as_ref().map(|v| v.len()).unwrap_or(0) }
}
/// pointer to hero-pool item `i`, or null
pub(crate) fn hero_pool_ptr(i: usize) -> *mut PmsMovie {
    unsafe {
        if let Some(&idx) = std::ptr::addr_of!(HERO_POOL).as_ref().and_then(|v| v.get(i)) {
            return movie_ptr(idx);
        }
    }
    std::ptr::null_mut()
}

/// number of home hubs
pub(crate) fn hub_count() -> usize {
    unsafe { std::ptr::addr_of!(HUBS).as_ref().map(|v| v.len()).unwrap_or(0) }
}
/// title of hub `i` (e.g. "Continue Watching") — borrowed from the main-thread hub table (the
/// per-frame shelf-title draw shouldn't clone a String per row; HUBS only changes on a re-fetch).
pub(crate) fn hub_title(i: usize) -> &'static str {
    unsafe {
        std::ptr::addr_of!(HUBS).as_ref().and_then(|v| v.get(i)).map(|h| h.title.as_str()).unwrap_or("")
    }
}
/// item count in hub `i`
pub(crate) fn hub_len(i: usize) -> usize {
    unsafe { std::ptr::addr_of!(HUBS).as_ref().and_then(|v| v.get(i)).map(|h| h.len).unwrap_or(0) }
}
/// whether hub `i` is the merged Continue Watching shelf (its tiles play directly on OK, so the
/// home grid stamps the play-hint badge on them). Matched on the locale-independent hubIdentifier.
pub(crate) fn hub_is_continue(i: usize) -> bool {
    unsafe {
        std::ptr::addr_of!(HUBS)
            .as_ref()
            .and_then(|v| v.get(i))
            .map(|h| h.hub_id == "home.continue")
            .unwrap_or(false)
    }
}
/// pointer to item `col` of hub `hub`, or null
pub(crate) fn hub_item_ptr(hub: usize, col: usize) -> *mut PmsMovie {
    unsafe {
        if let Some(h) = std::ptr::addr_of!(HUBS).as_ref().and_then(|v| v.get(hub)) {
            if col < h.len {
                return movie_ptr(h.start + col);
            }
        }
    }
    std::ptr::null_mut()
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
pub(crate) fn pms_fetch_hubs() -> c_int {
    let r = catch_unwind(|| unsafe {
        std::ptr::write(std::ptr::addr_of_mut!(pms_nmovies), 0);
        if let Some(v) = std::ptr::addr_of_mut!(HUBS).as_mut() {
            v.clear();
        }
        let mc = match crate::plex::client().home_hubs(12) {
            Some(m) => m,
            None => return 0,
        };
        let movies =
            std::slice::from_raw_parts_mut(std::ptr::addr_of_mut!(pms_movies) as *mut PmsMovie, PMS_MAX_MOVIES);
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

        let mut n = 0usize;
        for (title, hub_id, items) in &shelves {
            let start = n;
            for item in items {
                if n >= PMS_MAX_MOVIES {
                    break;
                }
                let m = &mut movies[n];
                parse_item(m, item);
                if m.title[0] != 0 && m.thumb[0] != 0 {
                    n += 1; // need a poster to show it in a shelf
                }
            }
            if n > start {
                if let Some(v) = std::ptr::addr_of_mut!(HUBS).as_mut() {
                    v.push(HubRow { title: title.clone(), hub_id: hub_id.clone(), start, len: n - start });
                }
            }
        }
        std::ptr::write(std::ptr::addr_of_mut!(pms_nmovies), n as c_int);

        // Rebuild the rotating hero pool: Continue Watching items first, then Recently Added,
        // deduped by ratingKey. Require landscape `art` (the hero draws a full-bleed backdrop) and
        // skip seasons (a bare "Season 1" makes a poor billboard). Capped at HERO_MAX.
        if let Some(pool) = std::ptr::addr_of_mut!(HERO_POOL).as_mut() {
            pool.clear();
            if let Some(hubs) = std::ptr::addr_of!(HUBS).as_ref() {
                for hub in hubs {
                    // Match on the locale-independent hubIdentifier, not the localized display title:
                    // "home.continue" plus every Recently Added variant (home.movies.recent,
                    // home.television.recent, promoted <type>.recentlyadded.<id>) all carry "recent".
                    let eligible = hub.hub_id == "home.continue" || hub.hub_id.contains("recent");
                    if !eligible {
                        continue;
                    }
                    for idx in hub.start..hub.start + hub.len {
                        if pool.len() >= HERO_MAX {
                            break;
                        }
                        let m = &movies[idx];
                        if m.art[0] == 0 || m.kind == 2 {
                            continue; // need landscape art; skip seasons
                        }
                        if pool.iter().any(|&j| movies[j].rk == m.rk) {
                            continue; // dedup by ratingKey
                        }
                        pool.push(idx);
                    }
                }
            }
        }
        n as c_int
    });
    r.unwrap_or(0)
}
