//! Item detail data layer for the detail page: full metadata (genres, cast, crew,
//! audio/subtitle streams), the TV season/episode hierarchy, and the related hub —
//! fetched on demand into a single CURRENT item. Idiomatic Rust (String/Vec); only
//! the browse catalog (pms.rs) still uses fixed C buffers from the C port.
// TEMP: struct fields/accessors are consumed incrementally by the detail UI
// (increments 1-4); drop this once the About footer (last consumer) lands.
#![allow(dead_code)]
use serde_json::Value;
use std::os::raw::c_int;
use std::panic::catch_unwind;
use std::ptr::{addr_of, addr_of_mut};

pub(crate) struct Cast {
    pub(crate) tag: String,   // actor name
    pub(crate) role: String,  // character
    pub(crate) thumb: String, // headshot (often an external metadata-static.plex.tv URL)
}

pub(crate) struct Stream {
    pub(crate) id: i64, // Plex stream id (for &audioStreamID / &subtitleStreamID)
    pub(crate) lang: String,
    pub(crate) codec: String,
    pub(crate) channels: i64,
    pub(crate) layout: String, // audioChannelLayout, e.g. "5.1(side)"
    pub(crate) title: String,
    pub(crate) sdh: bool,
    pub(crate) ad: bool,
    pub(crate) forced: bool,
}

pub(crate) struct Episode {
    pub(crate) rk: String,
    pub(crate) index: i64,   // episode number
    pub(crate) season: i64,  // parentIndex
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) aired: String, // originallyAvailableAt
    pub(crate) dur_ms: i64,
    pub(crate) thumb: String,
    pub(crate) resume_ms: i64, // viewOffset (0 = not started)
    pub(crate) part: String,   // Media[0].Part[0].key (to play)
    pub(crate) rating: String,
    pub(crate) vcodec: String, // Media[0].videoCodec (for the direct-play/transcode decision)
    pub(crate) acodec: String, // Media[0].audioCodec
}

pub(crate) struct Season {
    pub(crate) rk: String,
    pub(crate) index: i64,
    pub(crate) title: String,
    pub(crate) leaf_count: i64,
}

pub(crate) struct Related {
    pub(crate) rk: String,
    pub(crate) title: String,
    pub(crate) thumb: String,
    pub(crate) year: i64,
    pub(crate) is_show: bool,
}

pub(crate) struct Detail {
    pub(crate) rk: String,
    pub(crate) is_show: bool,
    pub(crate) title: String,
    pub(crate) year: i64,
    pub(crate) rating: String, // contentRating
    pub(crate) summary: String,
    pub(crate) tagline: String,
    pub(crate) studio: String,
    pub(crate) aired: String,
    pub(crate) dur_ms: i64,
    pub(crate) resume_ms: i64, // viewOffset (0 = not partially watched) — the resume position
    pub(crate) art: String,
    pub(crate) thumb: String,
    pub(crate) genres: Vec<String>,
    pub(crate) countries: Vec<String>,
    pub(crate) directors: Vec<String>,
    pub(crate) writers: Vec<String>,
    pub(crate) cast: Vec<Cast>,
    pub(crate) audio: Vec<Stream>,
    pub(crate) subs: Vec<Stream>,
    pub(crate) seasons: Vec<Season>,   // shows only
    pub(crate) episodes: Vec<Episode>, // the currently-selected season
    pub(crate) cur_season: usize,
    pub(crate) related: Vec<Related>,
}

// The one loaded detail item (the detail page shows a single item at a time).
static mut CURRENT: Option<Detail> = None;

/// the currently-loaded detail item, or None
pub(crate) fn current() -> Option<&'static Detail> {
    unsafe { (*addr_of!(CURRENT)).as_ref() }
}
/// drop the loaded detail (on leaving the detail page)
pub(crate) fn clear() {
    unsafe { *addr_of_mut!(CURRENT) = None }
}

// ---- JSON helpers ----
fn jstr(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}
fn jint(v: Option<&Value>) -> i64 {
    v.and_then(|x| x.as_i64()).unwrap_or(0)
}
/// collect the `tag` string of every element of a `Foo[]` array (Genre/Country/Director/…)
fn tags(item: &Value, key: &str) -> Vec<String> {
    item.get(key)
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|t| t.get("tag").and_then(|s| s.as_str()).map(String::from)).collect())
        .unwrap_or_default()
}
fn get_json(host: &str, port: c_int, path: &str) -> Option<Value> {
    let body = crate::stream::http_get(host, port, path, Some("Accept: application/json\r\n"))?;
    serde_json::from_slice(&body).ok()
}
fn meta0(v: &Value) -> Option<&Value> {
    v.get("MediaContainer")?.get("Metadata")?.as_array()?.first()
}
fn metas(v: &Value) -> Vec<Value> {
    v.get("MediaContainer")
        .and_then(|m| m.get("Metadata"))
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default()
}
/// Media[0].Part[0].key of an item (empty if none)
fn first_part(item: &Value) -> String {
    item.get("Media")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|md| md.get("Part"))
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .map(|p| jstr(p.get("key")))
        .unwrap_or_default()
}

// ---- fetches ----
fn fetch_detail(host: &str, port: c_int, token: &str, rk: &str) -> Option<Detail> {
    let json = get_json(host, port, &format!("/library/metadata/{rk}?X-Plex-Token={token}"))?;
    let it = meta0(&json)?;
    let mut d = Detail {
        rk: rk.to_string(),
        is_show: jstr(it.get("type")) == "show",
        title: jstr(it.get("title")),
        year: jint(it.get("year")),
        rating: jstr(it.get("contentRating")),
        summary: jstr(it.get("summary")),
        tagline: jstr(it.get("tagline")),
        studio: jstr(it.get("studio")),
        aired: jstr(it.get("originallyAvailableAt")),
        dur_ms: jint(it.get("duration")),
        resume_ms: jint(it.get("viewOffset")),
        art: jstr(it.get("art")),
        thumb: jstr(it.get("thumb")),
        genres: tags(it, "Genre"),
        countries: tags(it, "Country"),
        directors: tags(it, "Director"),
        writers: tags(it, "Writer"),
        cast: it
            .get("Role")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .map(|r| Cast { tag: jstr(r.get("tag")), role: jstr(r.get("role")), thumb: jstr(r.get("thumb")) })
                    .collect()
            })
            .unwrap_or_default(),
        audio: Vec::new(),
        subs: Vec::new(),
        seasons: Vec::new(),
        episodes: Vec::new(),
        cur_season: 0,
        related: Vec::new(),
    };
    // audio/subtitle streams (movies carry Media/Part/Stream; a show does not — its
    // episodes do, so load_detail backfills a show's streams from its first episode).
    parse_streams(it, &mut d);
    Some(d)
}

/// parse an item's Media[0].Part[0].Stream[] into d.audio / d.subs (the About footer)
fn parse_streams(item: &Value, d: &mut Detail) {
    let part = item
        .get("Media")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|md| md.get("Part"))
        .and_then(|a| a.as_array())
        .and_then(|a| a.first());
    let streams = match part.and_then(|p| p.get("Stream")).and_then(|a| a.as_array()) {
        Some(s) => s,
        None => return,
    };
    for s in streams {
        let title = jstr(s.get("title"));
        let st = Stream {
            id: jint(s.get("id")),
            lang: jstr(s.get("language")),
            codec: jstr(s.get("codec")),
            channels: jint(s.get("channels")),
            layout: jstr(s.get("audioChannelLayout")),
            sdh: jint(s.get("hearingImpaired")) != 0,
            ad: jint(s.get("audioDescription")) != 0 || title.to_lowercase().contains("descri"),
            forced: jint(s.get("forced")) != 0,
            title,
        };
        match jint(s.get("streamType")) {
            2 => d.audio.push(st),
            3 => d.subs.push(st),
            _ => {}
        }
    }
}

/// fetch one item's full metadata and parse its streams into `d` — used to borrow a
/// show's first-episode audio/subtitle tracks (the show container carries none).
fn fetch_item_streams(host: &str, port: c_int, token: &str, rk: &str, d: &mut Detail) {
    if let Some(json) = get_json(host, port, &format!("/library/metadata/{rk}?X-Plex-Token={token}")) {
        if let Some(it) = meta0(&json) {
            parse_streams(it, d);
        }
    }
}

fn fetch_seasons(host: &str, port: c_int, token: &str, rk: &str) -> Vec<Season> {
    let json = match get_json(host, port, &format!("/library/metadata/{rk}/children?X-Plex-Token={token}")) {
        Some(j) => j,
        None => return Vec::new(),
    };
    metas(&json)
        .iter()
        .filter(|x| jstr(x.get("type")) == "season")
        .map(|x| Season {
            rk: jstr(x.get("ratingKey")),
            index: jint(x.get("index")),
            title: jstr(x.get("title")),
            leaf_count: jint(x.get("leafCount")),
        })
        .collect()
}

fn fetch_episodes(host: &str, port: c_int, token: &str, season_rk: &str) -> Vec<Episode> {
    let json = match get_json(host, port, &format!("/library/metadata/{season_rk}/children?X-Plex-Token={token}")) {
        Some(j) => j,
        None => return Vec::new(),
    };
    metas(&json)
        .iter()
        .map(|x| {
            let media0 = x.get("Media").and_then(|a| a.as_array()).and_then(|a| a.first());
            Episode {
                rk: jstr(x.get("ratingKey")),
                index: jint(x.get("index")),
                season: jint(x.get("parentIndex")),
                title: jstr(x.get("title")),
                summary: jstr(x.get("summary")),
                aired: jstr(x.get("originallyAvailableAt")),
                dur_ms: jint(x.get("duration")),
                thumb: jstr(x.get("thumb")),
                resume_ms: jint(x.get("viewOffset")),
                part: first_part(x),
                rating: jstr(x.get("contentRating")),
                vcodec: media0.map(|m| jstr(m.get("videoCodec"))).unwrap_or_default(),
                acodec: media0.map(|m| jstr(m.get("audioCodec"))).unwrap_or_default(),
            }
        })
        .collect()
}

fn fetch_related(host: &str, port: c_int, token: &str, rk: &str) -> Vec<Related> {
    let json = match get_json(host, port, &format!("/library/metadata/{rk}/related?X-Plex-Token={token}")) {
        Some(j) => j,
        None => return Vec::new(),
    };
    let hubs = json
        .get("MediaContainer")
        .and_then(|m| m.get("Hub"))
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for h in &hubs {
        let items = match h.get("Metadata").and_then(|a| a.as_array()) {
            Some(i) => i,
            None => continue,
        };
        for x in items {
            let rk = jstr(x.get("ratingKey"));
            if rk.is_empty() || !seen.insert(rk.clone()) {
                continue;
            }
            out.push(Related {
                rk,
                title: jstr(x.get("title")),
                thumb: jstr(x.get("thumb")),
                year: jint(x.get("year")),
                is_show: jstr(x.get("type")) == "show",
            });
            if out.len() >= 20 {
                return out;
            }
        }
    }
    out
}

/// Fetch the full detail for `rk` (movie or show) into CURRENT: item metadata + cast
/// + streams, plus — for shows — seasons and the first season's episodes, plus the
/// related hub. Blocks on several HTTP round-trips (like route::play_movie's
/// /decision handshake); called synchronously when opening the detail page.
pub(crate) fn load_detail(rk: &str) {
    let rk = rk.to_string();
    let _ = catch_unwind(move || {
        let (host, port, token) = match crate::route::config() {
            Some(c) => c,
            None => return,
        };
        let mut d = match fetch_detail(&host, port, &token, &rk) {
            Some(d) => d,
            None => return,
        };
        if d.is_show {
            d.seasons = fetch_seasons(&host, port, &token, &rk);
            if let Some(s0) = d.seasons.first() {
                d.episodes = fetch_episodes(&host, port, &token, &s0.rk);
            }
            // a show carries no streams itself — backfill the About footer's audio/
            // subtitle tracks from the first episode (one extra round-trip)
            let first_ep_rk = d.episodes.first().map(|e| e.rk.clone());
            if let Some(ep_rk) = first_ep_rk {
                fetch_item_streams(&host, port, &token, &ep_rk, &mut d);
            }
        }
        d.related = fetch_related(&host, port, &token, &rk);
        crate::player::log(&format!(
            "detail: rk={} '{}' show={} genres={} cast={} seasons={} eps={} related={} audio={} subs={}",
            d.rk, d.title, d.is_show, d.genres.len(), d.cast.len(), d.seasons.len(), d.episodes.len(),
            d.related.len(), d.audio.len(), d.subs.len()
        ));
        unsafe { *addr_of_mut!(CURRENT) = Some(d) }
    });
}

/// Switch the loaded show to season `idx`, fetching its episodes (used by the season tabs).
pub(crate) fn load_season(idx: usize) {
    let _ = catch_unwind(move || {
        let (host, port, token) = match crate::route::config() {
            Some(c) => c,
            None => return,
        };
        let season_rk = match current().and_then(|d| d.seasons.get(idx)) {
            Some(s) => s.rk.clone(),
            None => return,
        };
        let eps = fetch_episodes(&host, port, &token, &season_rk);
        unsafe {
            if let Some(d) = (*addr_of_mut!(CURRENT)).as_mut() {
                d.episodes = eps;
                d.cur_season = idx;
            }
        }
    });
}
