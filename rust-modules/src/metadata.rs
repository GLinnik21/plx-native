//! Item detail data layer for the detail page: full metadata (genres, cast, crew,
//! audio/subtitle streams), the TV season/episode hierarchy, and the related hub —
//! fetched on demand into a single CURRENT item. Idiomatic Rust (String/Vec), like the
//! browse catalog (pms.rs) — the fixed C buffers from the C port are gone.
use std::panic::catch_unwind;
use std::ptr::{addr_of, addr_of_mut};

/// Plex's resume rule, in ONE place (home Continue-Watching, the detail Play button, and the
/// plxnative-play harness all apply it): resume only past 10s and before 95% watched, else start
/// from the beginning. Both args are MILLISECONDS; the returned position is NANOSECONDS
/// (what `player::resume_at` takes).
pub(crate) fn resume_ns(resume_ms: i64, dur_ms: i64) -> i64 {
    if resume_ms > 10_000 && (dur_ms <= 0 || (resume_ms as f64) < 0.95 * dur_ms as f64) {
        resume_ms * 1_000_000
    } else {
        0
    }
}

/// Friendly display name for an audio/subtitle codec id — the ONE codec→name map (the track
/// menu's section accessory and the Info card's track line both read it, so the same track
/// can't be named two ways).
pub(crate) fn friendly_codec(codec: &str) -> String {
    match codec.to_lowercase().as_str() {
        "truehd" => "Dolby TrueHD".to_string(),
        "eac3" | "ec-3" => "Dolby Digital Plus".to_string(),
        "ac3" => "Dolby Digital".to_string(),
        "dts" | "dca" => "DTS".to_string(),
        "aac" => "AAC".to_string(),
        "flac" => "FLAC".to_string(),
        "opus" => "Opus".to_string(),
        "mp3" => "MP3".to_string(),
        other if other.is_empty() => String::new(),
        other => other.to_uppercase(),
    }
}

pub(crate) struct Cast {
    pub(crate) tag: String,   // actor name
    pub(crate) role: String,  // character
    pub(crate) thumb: String, // headshot (often an external metadata-static.plex.tv URL)
}

pub(crate) struct Stream {
    pub(crate) id: i64, // Plex stream id (for &audioStreamID / &subtitleStreamID)
    pub(crate) lang: String,      // display name ("English")
    pub(crate) lang_code: String, // ISO code ("eng") — the route's language preference matches this
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
}

pub(crate) struct Related {
    pub(crate) rk: String,
    pub(crate) title: String,
    pub(crate) thumb: String,
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
    pub(crate) kind: String,       // this item's own type: movie | episode | show | season
    pub(crate) show_title: String, // grandparentTitle — the show name, when this item is an episode
    pub(crate) show_rk: String,    // grandparentRatingKey — the show's rk (episode → its show)
    pub(crate) season: i64,        // parentIndex — season number, when an episode
    pub(crate) index: i64,         // index — episode number, when an episode
    pub(crate) title: String,
    pub(crate) year: i64,
    pub(crate) rating: String, // contentRating
    pub(crate) summary: String,
    pub(crate) aired: String,
    pub(crate) dur_ms: i64,
    pub(crate) resume_ms: i64, // viewOffset (0 = not partially watched) — the resume position
    pub(crate) watched: bool,  // movie: viewCount ≥ 1; show: viewedLeafCount ≥ leafCount
    pub(crate) part: String,   // Media[0].Part[0].key for a leaf (movie/episode); empty for a show
    pub(crate) vcodec: String, // Media[0].videoCodec (drives the direct-play/transcode decision)
    pub(crate) acodec: String, // Media[0].audioCodec
    pub(crate) video_fps: f64, // video Stream frameRate (0 = unknown); feeds the Load esInfo
    pub(crate) art: String,
    pub(crate) thumb: String,
    pub(crate) genres: Vec<String>,
    pub(crate) countries: Vec<String>,
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

/// A compact descriptor of the item currently *playing*, for the in-player Info card. Unlike
/// `current()` (which stays on the detail page's show/movie), this always describes the playing
/// **leaf**: an episode carries the show title + SxEy + episode name + its still; a movie carries the
/// movie title + landscape art. Set by the play paths — `sync_now_playing()` after a leaf load, or
/// explicitly by show-page episode play (where `current()` is still the show).
pub(crate) struct NowPlaying {
    pub(crate) is_episode: bool,
    pub(crate) title: String,     // big title: show title (episode) or movie title
    pub(crate) ep_title: String,  // episode name (episode only)
    pub(crate) season: i64,
    pub(crate) index: i64,
    pub(crate) summary: String,
    pub(crate) year: i64,
    pub(crate) dur_ms: i64,
    pub(crate) rating: String,
    pub(crate) thumb: String,     // 16:9 still (episode) / landscape art (movie)
    pub(crate) detail_rk: String, // "Go to Show"/"Go to Movie" target
}
static mut NOW: Option<NowPlaying> = None;
pub(crate) fn now_playing() -> Option<&'static NowPlaying> {
    unsafe { (*addr_of!(NOW)).as_ref() }
}
pub(crate) fn set_now_playing(np: Option<NowPlaying>) {
    unsafe { *addr_of_mut!(NOW) = np }
}
/// Refresh `now_playing` from `current()` — call after a leaf `load_detail` (Continue-Watching /
/// off-catalog play, where `current()` becomes the played leaf). A show/season load leaves it None.
pub(crate) fn sync_now_playing() {
    let np = current().and_then(|d| match d.kind.as_str() {
        "episode" => Some(NowPlaying {
            is_episode: true,
            title: d.show_title.clone(),
            ep_title: d.title.clone(),
            season: d.season,
            index: d.index,
            summary: d.summary.clone(),
            year: d.year,
            dur_ms: d.dur_ms,
            rating: d.rating.clone(),
            thumb: d.thumb.clone(),
            detail_rk: d.show_rk.clone(),
        }),
        "movie" => Some(NowPlaying {
            is_episode: false,
            title: d.title.clone(),
            ep_title: String::new(),
            season: 0,
            index: 0,
            summary: d.summary.clone(),
            year: d.year,
            dur_ms: d.dur_ms,
            rating: d.rating.clone(),
            thumb: if !d.art.is_empty() { d.art.clone() } else { d.thumb.clone() },
            detail_rk: d.rk.clone(),
        }),
        _ => None, // show / season → not a playing leaf
    });
    set_now_playing(np);
}

// ---- fetches (all via the typed crate::plex client; serde DTOs, no Value scraping) ----
fn fetch_detail(rk: &str) -> Option<Detail> {
    let it = crate::plex::client().metadata(rk)?;
    let media0 = it.media.first();
    let mut d = Detail {
        rk: rk.to_string(),
        is_show: it.kind == "show",
        kind: it.kind.clone(),
        show_title: it.grandparent_title.clone(),
        show_rk: it.grandparent_rating_key.clone(),
        season: it.parent_index,
        index: it.index,
        title: it.title.clone(),
        year: it.year,
        rating: it.content_rating.clone(),
        summary: it.summary.clone(),
        aired: it.originally_available_at.clone(),
        dur_ms: it.duration,
        resume_ms: it.view_offset,
        watched: if it.kind == "show" || it.kind == "season" {
            it.leaf_count > 0 && it.viewed_leaf_count >= it.leaf_count
        } else {
            it.view_count > 0
        },
        // empty for a show (no Media on the show container)
        part: it.first_part().map(|p| p.key.clone()).unwrap_or_default(),
        vcodec: media0.map(|m| m.video_codec.clone()).unwrap_or_default(),
        acodec: media0.map(|m| m.audio_codec.clone()).unwrap_or_default(),
        video_fps: 0.0, // set from the video Stream by parse_streams below
        art: it.art.clone(),
        thumb: it.thumb.clone(),
        genres: it.genre.iter().map(|t| t.tag.clone()).collect(),
        countries: it.country.iter().map(|t| t.tag.clone()).collect(),
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
            lang_code: s.language_code.to_lowercase(),
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
    // the detail page usually already holds this item's streams — reuse them instead of
    // re-downloading the full metadata on every Play press (a whole GET on the play path)
    if let Some(d) = current() {
        if d.rk == rk && !d.audio.is_empty() {
            return d.audio.iter().map(|s| (s.codec.to_lowercase(), s.lang_code.clone())).collect();
        }
    }
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
    // a fresh detail load supersedes any in-flight season fetch (same show re-opened: a stale
    // landing would overwrite the fresh first-season episode list)
    supersede_season();
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
        // if this load is a playing leaf (episode/movie), refresh the Info card's descriptor from it
        sync_now_playing();
    });
}

// ---- season switching ----------------------------------------------------------------------
// The tab UI's season switch is ASYNC: `load_season` flips `cur_season` optimistically (the tab
// highlight moves at once), fetches the episodes on a worker thread, and `pump_season` (called by
// the detail page once a frame) applies the landed list on the main thread. The blocking
// `/children` GET used to run on the main loop, freezing the UI for every rapid season hop.
// Generations guard against out-of-order landings; results for a different item are discarded.
static SEASON_GEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static SEASON_DONE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
struct SeasonResult {
    gen: u32,
    rk: String, // the show the fetch was for
    idx: usize,
    eps: Vec<Episode>,
}
static SEASON_RESULT: std::sync::Mutex<Option<SeasonResult>> = std::sync::Mutex::new(None);

/// Invalidate any in-flight/pending season fetch and mark the mailbox settled: bump the generation
/// (so a late async landing is discarded), catch SEASON_DONE up to it (season_loading() → false),
/// and clear the slot. Returns the fresh generation. The ONE place the three season atomics move
/// together — used by the blocking `load_season_now` and by `load_detail` (a new item supersedes
/// the old show's pending fetch).
fn supersede_season() -> u32 {
    use std::sync::atomic::Ordering;
    let gen = SEASON_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    SEASON_DONE.store(gen, Ordering::SeqCst);
    *SEASON_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    gen
}

/// Switch the loaded show to season `idx` (the season tabs): `cur_season` flips immediately, the
/// episodes arrive via [`pump_season`]. Main-thread only (touches CURRENT).
pub(crate) fn load_season(idx: usize) {
    use std::sync::atomic::Ordering;
    let (rk, season_rk) = match current().and_then(|d| d.seasons.get(idx).map(|s| (d.rk.clone(), s.rk.clone()))) {
        Some(t) => t,
        None => return,
    };
    unsafe {
        if let Some(d) = (*addr_of_mut!(CURRENT)).as_mut() {
            d.cur_season = idx;
        }
    }
    let gen = SEASON_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        // the mailbox is filled OUTSIDE the guard so a panicking fetch still lands (empty) —
        // otherwise season_loading() would report an in-flight fetch forever
        let eps = catch_unwind(|| fetch_episodes(&season_rk)).unwrap_or_default();
        let mut slot = SEASON_RESULT.lock().unwrap_or_else(|e| e.into_inner());
        // MONOTONE mailbox: an older fetch that lands late must never clobber a newer result the
        // pump hasn't consumed yet — that lost the newest season forever (and with it the
        // SEASON_DONE catch-up, wedging the loading spinner on).
        if slot.as_ref().map(|r| r.gen < gen).unwrap_or(true) {
            *slot = Some(SeasonResult { gen, rk, idx, eps });
        }
    });
}

/// [`load_season`] but BLOCKING — for the page-open paths (`open_rk_season`, and any caller that
/// plays `episodes[0]` right after) where the episode list must be right before the next line
/// runs. Invalidates any in-flight async fetch so a stale landing can't overwrite this one.
pub(crate) fn load_season_now(idx: usize) {
    let _ = catch_unwind(move || {
        let season_rk = match current().and_then(|d| d.seasons.get(idx)) {
            Some(s) => s.rk.clone(),
            None => return,
        };
        let eps = fetch_episodes(&season_rk);
        supersede_season(); // drop any async fetch in flight; this synchronous list wins
        unsafe {
            if let Some(d) = (*addr_of_mut!(CURRENT)).as_mut() {
                d.episodes = eps;
                d.cur_season = idx;
            }
        }
    });
}

/// True while a season fetch is in flight — drives the episode row's loading dim + spinner.
pub(crate) fn season_loading() -> bool {
    use std::sync::atomic::Ordering;
    let gen = SEASON_GEN.load(Ordering::SeqCst);
    gen != 0 && gen != SEASON_DONE.load(Ordering::SeqCst)
}

/// Main-thread pump: apply a landed season fetch to CURRENT, discarding stale generations (a newer
/// request is in flight) and results for a different item. Returns true when the episode list just
/// changed — the detail page resets its episode focus/scroll on it.
pub(crate) fn pump_season() -> bool {
    use std::sync::atomic::Ordering;
    let res = SEASON_RESULT.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(r) = res else { return false };
    if r.gen != SEASON_GEN.load(Ordering::SeqCst) {
        return false; // superseded — the newer fetch will land after this
    }
    SEASON_DONE.store(r.gen, Ordering::SeqCst);
    unsafe {
        if let Some(d) = (*addr_of_mut!(CURRENT)).as_mut() {
            if d.rk == r.rk {
                d.episodes = r.eps;
                d.cur_season = r.idx;
                return true;
            }
        }
    }
    false
}
