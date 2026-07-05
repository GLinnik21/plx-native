//! Plex library fetch/parse into the private catalog (was src/pms.c), read by the UI
//! via movie_ptr()/nmovies(), plus urlenc_str (shared by posters/route). The
//! hand-rolled JSON string-scrape is replaced with serde_json navigation.
#![allow(non_upper_case_globals)]
use serde_json::Value;
use std::os::raw::{c_char, c_int};
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
    pub(crate) kind: c_int,     // 0 = movie, 1 = show (a container: play episodes, no direct Part)
    pub(crate) resume_ms: i64,  // viewOffset — drives the Continue Watching resume bar
}
impl PmsMovie {
    const ZERO: PmsMovie = PmsMovie {
        title: [0; 128], year: 0, rating: [0; 12], dur_ns: 0, part: [0; 256],
        thumb: [0; 128], art: [0; 128], summary: [0; 600], rk: [0; 16],
        vcodec: [0; 12], acodec: [0; 12], blur: [[0.0; 3]; 4], has_blur: 0, kind: 0, resume_ms: 0,
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
/// catalog index whose ratingKey == `rk` (for the /tmp/poc-detail probe), or -1
pub(crate) fn index_of_rk(rk: &str) -> c_int {
    for i in 0..nmovies() {
        let m = unsafe { &*movie_ptr(i) };
        let n = m.rk.iter().position(|&x| x == 0).unwrap_or(m.rk.len());
        if std::str::from_utf8(&m.rk[..n]).ok() == Some(rk) {
            return i as c_int;
        }
    }
    -1
}

// ---- helpers ----
unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

fn jstr(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn hex3(hex: &str) -> [f32; 3] {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    [
        ((v >> 16) & 0xff) as f32 / 255.0,
        ((v >> 8) & 0xff) as f32 / 255.0,
        (v & 0xff) as f32 / 255.0,
    ]
}

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
fn parse_item(m: &mut PmsMovie, item: &Value) {
    *m = PmsMovie::ZERO;
    m.kind = if jstr(item.get("type")) == "show" { 1 } else { 0 };
    set_field(&mut m.title, &jstr(item.get("title")));
    m.year = item.get("year").and_then(|v| v.as_i64()).unwrap_or(0) as c_int;
    set_field(&mut m.rating, &jstr(item.get("contentRating")));
    let durms = item.get("duration").and_then(|v| v.as_i64()).unwrap_or(0);
    m.dur_ns = if durms > 0 { durms * 1_000_000 } else { 0 };
    m.resume_ms = item.get("viewOffset").and_then(|v| v.as_i64()).unwrap_or(0);
    // poster: prefer the show poster for episodes (grandparentThumb) so a landscape
    // episode still doesn't fill a portrait card
    let thumb = item
        .get("grandparentThumb")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| jstr(item.get("thumb")));
    set_field(&mut m.thumb, &thumb);
    set_field(&mut m.art, &jstr(item.get("art")));
    set_field(&mut m.summary, &jstr(item.get("summary")));
    set_field(&mut m.rk, &jstr(item.get("ratingKey")));
    // Media[0]: codecs + Part[0].key (movies/episodes; a show container has none)
    if let Some(md) = item.get("Media").and_then(|a| a.as_array()).and_then(|a| a.first()) {
        set_field(&mut m.vcodec, &jstr(md.get("videoCodec")));
        set_field(&mut m.acodec, &jstr(md.get("audioCodec")));
        if let Some(p0) = md.get("Part").and_then(|a| a.as_array()).and_then(|a| a.first()) {
            set_field(&mut m.part, &jstr(p0.get("key")));
        }
    }
    // UltraBlurColors -> ambient gradient
    if let Some(ub) = item.get("UltraBlurColors") {
        if let Some(tl) = ub.get("topLeft").and_then(|v| v.as_str()) {
            m.blur[0] = hex3(tl);
            m.blur[1] = hex3(ub.get("topRight").and_then(|v| v.as_str()).unwrap_or("000000"));
            m.blur[2] = hex3(ub.get("bottomRight").and_then(|v| v.as_str()).unwrap_or("000000"));
            m.blur[3] = hex3(ub.get("bottomLeft").and_then(|v| v.as_str()).unwrap_or("000000"));
            m.has_blur = 1;
        }
    }
}

/// Fetch one library section's items and APPEND them to the catalog starting at
/// index `start`. Returns the new total count.
unsafe fn fetch_section(host: &str, port: c_int, token: &str, sec: i64, start: usize) -> usize {
    let path = format!("/library/sections/{sec}/all?X-Plex-Token={token}");
    let body = match crate::stream::http_get(host, port, &path, Some("Accept: application/json\r\n")) {
        Some(b) => b,
        None => return start,
    };
    let json: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return start,
    };
    let meta = match json.get("MediaContainer").and_then(|m| m.get("Metadata")).and_then(|a| a.as_array()) {
        Some(m) => m,
        None => return start,
    };
    let movies = std::slice::from_raw_parts_mut(
        std::ptr::addr_of_mut!(pms_movies) as *mut PmsMovie,
        PMS_MAX_MOVIES,
    );
    let mut n = start;
    for item in meta {
        if n >= PMS_MAX_MOVIES {
            break;
        }
        let m = &mut movies[n];
        parse_item(m, item);
        // movies need a playable Part; shows are containers (episodes carry the parts)
        if m.title[0] != 0 && (m.part[0] != 0 || m.kind == 1) {
            n += 1;
        }
    }
    n
}

/// Fetch every movie + show library into the catalog (movies first, shows after),
/// discovering the section keys from /library/sections. Returns the total count.
pub(crate) fn pms_fetch_movies(host: *const c_char, port: c_int, token: *const c_char) -> c_int {
    let r = catch_unwind(|| unsafe {
        std::ptr::write(std::ptr::addr_of_mut!(pms_nmovies), 0);
        let host_s = cstr(host);
        let token_s = cstr(token);
        // discover the section keys once, then fetch movies first, shows after — so
        // movie rows keep their existing order and the hero (movie_at(0,0)) stays a
        // movie; shows append into the later grid rows.
        let secpath = format!("/library/sections?X-Plex-Token={token_s}");
        let mut sections: Vec<(i64, bool)> = Vec::new(); // (key, is_show)
        if let Some(body) = crate::stream::http_get(&host_s, port, &secpath, Some("Accept: application/json\r\n")) {
            if let Ok(json) = serde_json::from_slice::<Value>(&body) {
                if let Some(dirs) = json.get("MediaContainer").and_then(|m| m.get("Directory")).and_then(|a| a.as_array()) {
                    for is_show in [false, true] {
                        let want = if is_show { "show" } else { "movie" };
                        for d in dirs {
                            if jstr(d.get("type")) != want {
                                continue;
                            }
                            if let Some(key) = d.get("key").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()) {
                                sections.push((key, is_show));
                            }
                        }
                    }
                }
            }
        }
        let mut n = 0usize;
        for (key, _is_show) in sections {
            n = fetch_section(&host_s, port, &token_s, key, n);
        }
        std::ptr::write(std::ptr::addr_of_mut!(pms_nmovies), n as c_int);
        n as c_int
    });
    r.unwrap_or(0)
}

// ---- home hubs: each hub is a titled slice of the catalog (pms_movies) ----
struct HubRow {
    title: String,
    start: usize,
    len: usize,
}
static mut HUBS: Vec<HubRow> = Vec::new();

/// number of home hubs
pub(crate) fn hub_count() -> usize {
    unsafe { std::ptr::addr_of!(HUBS).as_ref().map(|v| v.len()).unwrap_or(0) }
}
/// title of hub `i` (e.g. "Continue Watching")
pub(crate) fn hub_title(i: usize) -> String {
    unsafe {
        std::ptr::addr_of!(HUBS).as_ref().and_then(|v| v.get(i)).map(|h| h.title.clone()).unwrap_or_default()
    }
}
/// item count in hub `i`
pub(crate) fn hub_len(i: usize) -> usize {
    unsafe { std::ptr::addr_of!(HUBS).as_ref().and_then(|v| v.get(i)).map(|h| h.len).unwrap_or(0) }
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

/// Fetch the home hubs (Continue Watching, On Deck, Recently Added, collections) into
/// the catalog + the HUBS grouping. Skips music/photo/playlist hubs + empty ones.
pub(crate) fn pms_fetch_hubs(host: *const c_char, port: c_int, token: *const c_char) -> c_int {
    let r = catch_unwind(|| unsafe {
        std::ptr::write(std::ptr::addr_of_mut!(pms_nmovies), 0);
        if let Some(v) = std::ptr::addr_of_mut!(HUBS).as_mut() {
            v.clear();
        }
        let host_s = cstr(host);
        let token_s = cstr(token);
        let path = format!("/hubs?count=12&excludeContinueWatching=0&X-Plex-Token={token_s}");
        let body = match crate::stream::http_get(&host_s, port, &path, Some("Accept: application/json\r\n")) {
            Some(b) => b,
            None => return 0,
        };
        let json: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return 0,
        };
        let hubs = match json.get("MediaContainer").and_then(|m| m.get("Hub")).and_then(|a| a.as_array()) {
            Some(h) => h,
            None => return 0,
        };
        let movies =
            std::slice::from_raw_parts_mut(std::ptr::addr_of_mut!(pms_movies) as *mut PmsMovie, PMS_MAX_MOVIES);
        const SKIP: [&str; 6] = ["album", "artist", "track", "photo", "clip", "playlist"];
        let mut n = 0usize;
        for hub in hubs {
            if SKIP.contains(&jstr(hub.get("type")).as_str()) {
                continue;
            }
            let items = match hub.get("Metadata").and_then(|a| a.as_array()) {
                Some(it) if !it.is_empty() => it,
                _ => continue,
            };
            let start = n;
            for item in items {
                if n >= PMS_MAX_MOVIES {
                    break;
                }
                if SKIP.contains(&jstr(item.get("type")).as_str()) {
                    continue;
                }
                let m = &mut movies[n];
                parse_item(m, item);
                if m.title[0] != 0 && m.thumb[0] != 0 {
                    n += 1; // need a poster to show it in a shelf
                }
            }
            if n > start {
                if let Some(v) = std::ptr::addr_of_mut!(HUBS).as_mut() {
                    v.push(HubRow { title: jstr(hub.get("title")), start, len: n - start });
                }
            }
        }
        std::ptr::write(std::ptr::addr_of_mut!(pms_nmovies), n as c_int);
        n as c_int
    });
    r.unwrap_or(0)
}
