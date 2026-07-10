//! Item detail data layer for the detail page: full metadata (genres, cast, crew,
//! audio/subtitle streams), the TV season/episode hierarchy, and the related hub —
//! fetched on demand into a single CURRENT item. Idiomatic Rust (String/Vec); only
//! the browse catalog (pms.rs) still uses fixed C buffers from the C port.
// TEMP: struct fields/accessors are consumed incrementally by the detail UI
// (increments 1-4); drop this once the About footer (last consumer) lands.
#![allow(dead_code)]
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
    pub(crate) default: bool, // the file's default track (drives the "Original:" audio label)
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

pub(crate) struct Chapter {
    pub(crate) index: i64,    // 1-based chapter number
    pub(crate) start_ms: i64, // startTimeOffset — the seek target + timestamp label
    pub(crate) title: String, // Chapter.tag; empty → UI shows "Chapter {index}"
    pub(crate) thumb: String, // server image path → resolve_tex (empty if no chapter thumbs)
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
    pub(crate) part: String,   // Media[0].Part[0].key for a leaf (movie/episode); empty for a show
    pub(crate) vcodec: String, // Media[0].videoCodec (drives the direct-play/transcode decision)
    pub(crate) acodec: String, // Media[0].audioCodec
    pub(crate) video_fps: f64, // video Stream frameRate (0 = unknown); feeds the Load esInfo
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
    pub(crate) chapters: Vec<Chapter>,
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

// ---- fetches (all via the typed crate::plex client; serde DTOs, no Value scraping) ----
fn fetch_detail(rk: &str) -> Option<Detail> {
    let it = crate::plex::client().metadata(rk)?;
    let media0 = it.media.first();
    let mut d = Detail {
        rk: rk.to_string(),
        is_show: it.kind == "show",
        title: it.title.clone(),
        year: it.year,
        rating: it.content_rating.clone(),
        summary: it.summary.clone(),
        tagline: it.tagline.clone(),
        studio: it.studio.clone(),
        aired: it.originally_available_at.clone(),
        dur_ms: it.duration,
        resume_ms: it.view_offset,
        // empty for a show (no Media on the show container)
        part: it.first_part().map(|p| p.key.clone()).unwrap_or_default(),
        vcodec: media0.map(|m| m.video_codec.clone()).unwrap_or_default(),
        acodec: media0.map(|m| m.audio_codec.clone()).unwrap_or_default(),
        video_fps: 0.0, // set from the video Stream by parse_streams below
        art: it.art.clone(),
        thumb: it.thumb.clone(),
        genres: it.genre.iter().map(|t| t.tag.clone()).collect(),
        countries: it.country.iter().map(|t| t.tag.clone()).collect(),
        directors: it.director.iter().map(|t| t.tag.clone()).collect(),
        writers: it.writer.iter().map(|t| t.tag.clone()).collect(),
        cast: it
            .role
            .iter()
            .map(|r| Cast { tag: r.tag.clone(), role: r.role.clone(), thumb: r.thumb.clone() })
            .collect(),
        audio: Vec::new(),
        subs: Vec::new(),
        seasons: Vec::new(),
        episodes: Vec::new(),
        cur_season: 0,
        related: Vec::new(),
        chapters: it
            .chapter
            .iter()
            .map(|c| Chapter {
                index: c.index,
                start_ms: c.start_time_offset,
                title: c.tag.clone(),
                thumb: c.thumb.clone(),
            })
            .collect(),
    };
    // audio/subtitle streams (movies carry Media/Part/Stream; a show does not — its
    // episodes do, so load_detail backfills a show's streams from its first episode).
    parse_streams(&it, &mut d);
    Some(d)
}

/// parse an item's Media[0].Part[0].Stream[] into d.audio / d.subs (the About footer)
fn parse_streams(it: &crate::plex::Metadata, d: &mut Detail) {
    let streams = match it.first_part() {
        Some(p) => &p.stream,
        None => return,
    };
    for s in streams {
        let st = Stream {
            id: s.id,
            lang: s.language.clone(),
            codec: s.codec.clone(),
            channels: s.channels,
            layout: s.audio_channel_layout.clone(),
            sdh: s.hearing_impaired != 0,
            ad: s.audio_description != 0 || s.title.to_lowercase().contains("descri"),
            forced: s.forced != 0,
            default: s.is_default != 0,
            title: s.title.clone(),
        };
        match s.stream_type {
            1 => d.video_fps = s.frame_rate, // e.g. 23.976 — for the Load esInfo
            2 => d.audio.push(st),
            3 => d.subs.push(st),
            _ => {}
        }
    }
}

/// The audio tracks of `rk` in FILE order as (codec, languageCode) lowercase pairs, e.g.
/// [("ac3","rus"),("eac3","eng")]. Empty on any fetch/parse failure. Used by the route decision
/// to pick a direct-playable track — preferring a language (English) over the file's default —
/// and to fall back to a direct-playable sibling when the default codec isn't (TrueHD default).
pub(crate) fn audio_tracks(rk: &str) -> Vec<(String, String)> {
    let it = match crate::plex::client().metadata(rk) {
        Some(i) => i,
        None => return Vec::new(),
    };
    let streams = match it.first_part() {
        Some(p) => &p.stream,
        None => return Vec::new(),
    };
    streams
        .iter()
        .filter(|s| s.stream_type == 2)
        .map(|s| (s.codec.to_lowercase(), s.language_code.to_lowercase()))
        .collect()
}

/// fetch one item's full metadata and parse its streams into `d` — used to borrow a
/// show's first-episode audio/subtitle tracks (the show container carries none).
fn fetch_item_streams(rk: &str, d: &mut Detail) {
    if let Some(it) = crate::plex::client().metadata(rk) {
        parse_streams(&it, d);
    }
}

fn fetch_seasons(rk: &str) -> Vec<Season> {
    let mc = match crate::plex::client().children(rk) {
        Some(m) => m,
        None => return Vec::new(),
    };
    mc.metadata
        .iter()
        .filter(|x| x.kind == "season")
        .map(|x| Season {
            rk: x.rating_key.clone(),
            index: x.index,
            title: x.title.clone(),
            leaf_count: x.leaf_count,
        })
        .collect()
}

fn fetch_episodes(season_rk: &str) -> Vec<Episode> {
    let mc = match crate::plex::client().children(season_rk) {
        Some(m) => m,
        None => return Vec::new(),
    };
    mc.metadata
        .iter()
        .map(|x| {
            let media0 = x.media.first();
            Episode {
                rk: x.rating_key.clone(),
                index: x.index,
                season: x.parent_index,
                title: x.title.clone(),
                summary: x.summary.clone(),
                aired: x.originally_available_at.clone(),
                dur_ms: x.duration,
                thumb: x.thumb.clone(),
                resume_ms: x.view_offset,
                part: x.first_part().map(|p| p.key.clone()).unwrap_or_default(),
                rating: x.content_rating.clone(),
                vcodec: media0.map(|m| m.video_codec.clone()).unwrap_or_default(),
                acodec: media0.map(|m| m.audio_codec.clone()).unwrap_or_default(),
            }
        })
        .collect()
}

fn fetch_related(rk: &str) -> Vec<Related> {
    let mc = match crate::plex::client().related(rk) {
        Some(m) => m,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for h in &mc.hub {
        for x in &h.metadata {
            if x.rating_key.is_empty() || !seen.insert(x.rating_key.clone()) {
                continue;
            }
            out.push(Related {
                rk: x.rating_key.clone(),
                title: x.title.clone(),
                thumb: x.thumb.clone(),
                year: x.year,
                is_show: x.kind == "show",
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
        let mut d = match fetch_detail(&rk) {
            Some(d) => d,
            None => return,
        };
        if d.is_show {
            d.seasons = fetch_seasons(&rk);
            if let Some(s0) = d.seasons.first() {
                d.episodes = fetch_episodes(&s0.rk);
            }
            // a show carries no streams itself — backfill the About footer's audio/
            // subtitle tracks from the first episode (one extra round-trip)
            let first_ep_rk = d.episodes.first().map(|e| e.rk.clone());
            if let Some(ep_rk) = first_ep_rk {
                fetch_item_streams(&ep_rk, &mut d);
            }
        }
        d.related = fetch_related(&rk);
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
        let season_rk = match current().and_then(|d| d.seasons.get(idx)) {
            Some(s) => s.rk.clone(),
            None => return,
        };
        let eps = fetch_episodes(&season_rk);
        unsafe {
            if let Some(d) = (*addr_of_mut!(CURRENT)).as_mut() {
                d.episodes = eps;
                d.cur_season = idx;
            }
        }
    });
}
