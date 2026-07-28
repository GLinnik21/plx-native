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

#[derive(Clone)]
pub(crate) struct Stream {
    pub(crate) id: i64, // Plex stream id (for &audioStreamID / &subtitleStreamID)
    pub(crate) index: i64, // PMS stream index (container order) — the ordinal mapping sorts by it
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
    /// external/sidecar stream (downloaded .srt etc. — NOT inside the container). The client
    /// renderer can't reach it on direct-play; only a server transcode can burn it.
    pub(crate) external: bool,
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

#[derive(Default)]
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
/// drop the loaded detail (on leaving the detail page). Also supersedes any in-flight async
/// fetch — otherwise a load requested on the way in lands after the page closed and silently
/// repopulates CURRENT (and NOW, via `sync_now_playing`) behind whatever screen is now mounted.
pub(crate) fn clear() {
    supersede_detail();
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

/// Convert a part's Stream[] into (audio, subs, video_fps) — the ONE plex::Stream → Stream
/// mapping (the detail parse and the playing-tracks store both use it).
fn convert_streams(streams: &[crate::plex::Stream]) -> (Vec<Stream>, Vec<Stream>, f64) {
    let (mut audio, mut subs, mut fps) = (Vec::new(), Vec::new(), 0.0);
    for s in streams {
        let st = Stream {
            id: s.id,
            index: s.index,
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
            // embedded container streams carry no delivery key; sidecars do
            external: s.stream_type == 3 && !s.key.is_empty(),
        };
        match s.stream_type {
            1 => fps = s.frame_rate, // e.g. 23.976 — for the Load esInfo
            2 => audio.push(st),
            3 => subs.push(st),
            _ => {}
        }
    }
    (audio, subs, fps)
}

/// parse an item's Media[0].Part[0].Stream[] into d.audio / d.subs (the About footer)
fn parse_streams(it: &crate::plex::Metadata, d: &mut Detail) {
    if let Some(p) = it.first_part() {
        let (audio, subs, fps) = convert_streams(&p.stream);
        d.audio = audio;
        d.subs = subs;
        if fps > 0.0 {
            d.video_fps = fps;
        }
    }
}

// ---- the PLAYING-item track store — the in-player source of truth ---------------------------
// Unlike `current()` (the detail page's item — it stays on the SHOW during an episode play, and
// can be a different item entirely when playing straight from Home), this always holds the
// played leaf's OWN streams. The track menu and the route's audio pick read it; feeding a menu
// built from episode 1's streams to a playback of episode 5 was a real track-identity bug.

#[derive(Clone)]
pub(crate) struct PlayingTracks {
    pub(crate) rk: String,
    pub(crate) audio: Vec<Stream>,
    pub(crate) subs: Vec<Stream>,
    pub(crate) video_fps: f64, // the played leaf's video fps (0 = unknown) — feeds the Load esInfo
}
static mut PLAYING: Option<PlayingTracks> = None;

/// the playing item's track lists (None until a catalog item starts playing). Main-thread only.
pub(crate) fn playing() -> Option<&'static PlayingTracks> {
    unsafe { (*addr_of!(PLAYING)).as_ref() }
}

/// Load the playing-item track store for `rk` at play time (route::build_stream). Reuses the
/// loaded detail's streams when it IS this item (no extra GET on the play path — the same
/// optimization the old `audio_tracks` fetch had); otherwise one metadata fetch. An empty `rk`
/// (local-sample / URL-override play) clears the store.
/// PURE: fetch the playing item's track lists. Safe on a worker — reads and writes no statics.
/// The cache-hit shortcut is `cached_playing` (main thread) and the install is `install_playing`,
/// because `playing()` hands out a `&'static` whose Vecs the track menu and info panel hold
/// slices into during playback.
/// MAIN THREAD: the cache-hit half of the old `load_playing` — reuse the loaded detail's streams
/// when it IS this item, so playing from a detail page costs no extra GET. Snapshotted into
/// `ResolveEnv` and handed to the worker; splitting the fetch out lost this and quietly added a
/// PMS round trip to every play from a detail page.
pub(crate) fn cached_playing(rk: &str) -> Option<PlayingTracks> {
    current().filter(|d| d.rk == rk && !d.audio.is_empty()).map(|d| PlayingTracks {
        rk: rk.to_string(),
        audio: d.audio.clone(),
        subs: d.subs.clone(),
        video_fps: d.video_fps,
    })
}

pub(crate) fn fetch_playing_tracks(rk: &str) -> Option<PlayingTracks> {
    if rk.is_empty() {
        return None;
    }
    let (audio, subs, video_fps) = crate::plex::client_opt()
        .and_then(|c| c.metadata(rk))
        .and_then(|it| it.first_part().map(|p| convert_streams(&p.stream)))
        .unwrap_or_default();
    Some(PlayingTracks { rk: rk.to_string(), audio, subs, video_fps })
}

/// MAIN THREAD: install a fetched track store.
pub(crate) fn install_playing(pt: Option<PlayingTracks>) {
    if let Some(pt) = &pt {
        crate::player::log(&format!(
            "playing tracks: rk={} audio={} subs={}", pt.rk, pt.audio.len(), pt.subs.len()));
    }
    unsafe { *addr_of_mut!(PLAYING) = pt };
}

// ---- list-position → demuxer-ordinal conversion --------------------------------------------
// The demuxer selects "the Nth stream of its type" in CONTAINER order; the menu/metadata lists
// are in PMS document order. These convert a list position to that container ordinal by sorting
// on PMS `Stream.index` (stable tie-break on list position, so an index-less response degrades
// to document order — the previous behavior).

/// Container-audio ordinal of `audio[i]` — what `player::set_audio_track`/`request_audio_track`
/// (→ ff's nth_audio_stream) consume.
pub(crate) fn audio_ordinal(audio: &[Stream], i: usize) -> i32 {
    if i >= audio.len() {
        return i as i32;
    }
    let me = (audio[i].index, i);
    audio.iter().enumerate().filter(|(j, s)| (s.index, *j) < me).count() as i32
}

/// Container ordinal of `subs[i]` among the EMBEDDED subtitle streams (all ff.rs enumerates —
/// sidecars are not in the container), or -1 when `subs[i]` is itself external (nothing to
/// client-render on direct-play; only a server transcode can burn it).
pub(crate) fn sub_render_ordinal(subs: &[Stream], i: usize) -> i32 {
    let s0 = match subs.get(i) {
        Some(s) if !s.external => s,
        _ => return -1,
    };
    let me = (s0.index, i);
    subs.iter()
        .enumerate()
        .filter(|(j, s)| !s.external && (s.index, *j) < me)
        .count() as i32
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

/// The full detail fetch for `rk` (movie or show): item metadata + cast + streams, plus — for
/// shows — seasons, the first season's episodes and its stream backfill, plus the related hub.
/// 2 PMS round-trips for a movie, 5 for a show.
///
/// PURE NETWORK + PARSING — it touches no `static mut`, which is exactly what lets it run either
/// on the main thread ([`load_detail_now`]) or on a worker ([`request_detail`]). Keep it that way:
/// installing the result is the caller's job, and on the async path that must happen on the main
/// thread (see the DETAIL_SLOT note).
fn fetch_full(rk: &str) -> Option<Detail> {
    // `ms=` is the whole chain's wall clock. It is the exact cost `request_detail` moves off the
    // SDL loop, so it is the number to read when judging whether a call site can afford to block
    // — note the framedrop breakdown CANNOT show it (fd_pc0 starts after event handling).
    let t0 = std::time::Instant::now();
    let mut d = fetch_detail(rk)?;
    if d.is_show {
        d.seasons = fetch_seasons(rk);
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
    d.related = fetch_related(rk);
    crate::player::log(&format!(
        "detail: rk={} '{}' show={} genres={} cast={} seasons={} eps={} related={} audio={} subs={} ms={}",
        d.rk, d.title, d.is_show, d.genres.len(), d.cast.len(), d.seasons.len(), d.episodes.len(),
        d.related.len(), d.audio.len(), d.subs.len(), t0.elapsed().as_millis()
    ));
    Some(d)
}

/// [`request_detail`] but BLOCKING — for the callers that read `current()` on the NEXT statement:
/// `open_rk_season` (whose chained `load_season_now` indexes `d.seasons`), home_activate's
/// play-a-show arm (which gates on `current().rk == expect`), and the headless `plxnative-play` /
/// `plxnative-detail` triggers (which derive the leaf part/codecs, or replay move_focus/on_ok, in
/// the same frame). Every remaining call of this is a deliberate freeze — hence the `_now` name.
pub(crate) fn load_detail_now(rk: &str) {
    // this synchronous load wins over anything in flight — both the detail worker (whose landing
    // would otherwise overwrite it a beat later) and the season fetch (same show re-opened: a
    // stale landing would overwrite the fresh first-season episode list)
    supersede_detail();
    supersede_season();
    let rk = rk.to_string();
    let _ = catch_unwind(move || {
        if let Some(d) = fetch_full(&rk) {
            unsafe { *addr_of_mut!(CURRENT) = Some(d) }
            // if this load is a playing leaf (episode/movie), refresh the Info card's descriptor
            sync_now_playing();
        }
    });
}

// ---- async detail load ---------------------------------------------------------------------
// Opening a detail page used to block the SDL loop on 2 (movie) to 5 (show) sequential PMS
// round-trips, straight off the key handler. `request_detail` spawns the fetch and `pump_detail`
// installs the result — the page mounts THIS frame on the catalog row's art/title/summary and
// fills in a beat later. Same shape as the season mailbox below and route.rs's play resolve.
//
// The worker MUST NOT write CURRENT. `current()` hands out a `&'static Detail` that ~25 draw
// sites read within a frame, so a background store would drop the old `Detail` under a live
// reference — a use-after-free, not a lint. Keeping the main thread the sole writer is precisely
// what makes that `&'static` sound, so the worker's only output is the mailbox.
static DETAIL_GEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static DETAIL_DONE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
struct DetailResult {
    gen: u32,
    d: Option<Detail>, // None = the fetch failed or panicked — the page keeps the previous item
}
static DETAIL_SLOT: std::sync::Mutex<Option<DetailResult>> = std::sync::Mutex::new(None);

/// Invalidate any in-flight/pending detail fetch and mark the mailbox settled: bump the
/// generation (so a late landing is discarded by `pump_detail`), catch DETAIL_DONE up to it
/// (`detail_loading()` → false), and clear the slot. Returns the fresh generation.
fn supersede_detail() -> u32 {
    use std::sync::atomic::Ordering;
    let gen = DETAIL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    DETAIL_DONE.store(gen, Ordering::SeqCst);
    *DETAIL_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    gen
}

/// Post a finished fetch to the mailbox. MONOTONE: an older fetch landing late must never clobber
/// a newer result the pump hasn't consumed yet. Called from the worker (and from the tests, which
/// is the point of it being a named function rather than inline in the closure — the guard is the
/// one piece of this machinery that a test can't reach through `request_detail`).
fn land_detail(gen: u32, d: Option<Detail>) {
    let mut slot = DETAIL_SLOT.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().map(|r| r.gen < gen).unwrap_or(true) {
        *slot = Some(DetailResult { gen, d });
    }
}

/// MAIN THREAD, NON-BLOCKING. Supersedes any in-flight load and spawns the fetch; the result
/// lands via [`pump_detail`]. The caller mounts the detail page this same frame.
pub(crate) fn request_detail(rk: &str) {
    use std::sync::atomic::Ordering;
    // drop any season fetch in flight for the OLD item — its landing would patch the new one
    supersede_season();
    // NOT supersede_detail(): the generation must move (a stale landing is discarded) but
    // DETAIL_DONE must stay behind so `detail_loading()` reports this fetch as in flight
    let gen = DETAIL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    *DETAIL_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let rk = rk.to_string();
    let spawned = crate::task::spawn_small("detail", move || {
        // the mailbox is filled OUTSIDE the guard so a panicking fetch still lands (as None) —
        // otherwise detail_loading() would report an in-flight fetch forever
        let d = catch_unwind(|| fetch_full(&rk)).unwrap_or(None);
        land_detail(gen, d);
    });
    if !spawned {
        // no worker means nothing will ever land: catch DONE up or detail_loading() latches true
        // forever behind a spinner that can never resolve
        DETAIL_DONE.store(gen, Ordering::SeqCst);
    }
}

/// MAIN THREAD, once a frame, ROUTE-UNCONDITIONAL (a landing must never depend on which screen is
/// mounted — the play paths request a detail from Home and flip straight to the player). Installs
/// a landed fetch into CURRENT and returns true when a fresh item was published. A stale landing —
/// superseded by a newer request, by a blocking load, or by `clear()` when the page closed — is
/// dropped.
pub(crate) fn pump_detail() -> bool {
    use std::sync::atomic::Ordering;
    let taken = DETAIL_SLOT.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(r) = taken else { return false };
    if r.gen != DETAIL_GEN.load(Ordering::SeqCst) {
        return false; // superseded while in flight
    }
    DETAIL_DONE.store(r.gen, Ordering::SeqCst);
    let Some(d) = r.d else { return false }; // fetch failed: keep the previously loaded item
    // The LANDING is what defines which show's episodes are current, so the season supersede has
    // to happen here as well as at request time: a tab hop issued while this load was in flight
    // spawned a fetch against the OLD item, and its landing would patch these fresh episodes.
    supersede_season();
    unsafe { *addr_of_mut!(CURRENT) = Some(d) }
    // if this load is a playing leaf (episode/movie), refresh the Info card's descriptor from it
    sync_now_playing();
    true
}

/// True while a detail fetch is in flight — drives the detail page's loading spinner.
pub(crate) fn detail_loading() -> bool {
    use std::sync::atomic::Ordering;
    let gen = DETAIL_GEN.load(Ordering::SeqCst);
    gen != 0 && gen != DETAIL_DONE.load(Ordering::SeqCst)
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
/// together — used by the blocking `load_season_now`, and by all three detail entry points (a new
/// item supersedes the old show's pending fetch): `load_detail_now`, `request_detail` (dropping the
/// OLD item's fetch) and `pump_detail` (dropping one issued WHILE the load was in flight).
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
    let spawned = crate::task::spawn_small("season", move || {
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
    if !spawned {
        // no worker means nothing will ever land: catch DONE up or the episode row keeps its
        // loading dim + spinner for the rest of the session. `cur_season` already moved, so the
        // tab highlight stays where the user put it and the old episodes stay listed.
        SEASON_DONE.store(gen, Ordering::SeqCst);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// post through the REAL mailbox write, so the monotone guard is under test rather than
    /// bypassed (an unconditional store here would make the "older lands late" case vacuous)
    fn landing(gen: u32, rk: &str) {
        land_detail(gen, Some(Detail { rk: rk.to_string(), ..Default::default() }));
    }
    fn slot_rk() -> Option<String> {
        DETAIL_SLOT.lock().unwrap().as_ref().and_then(|r| r.d.as_ref()).map(|d| d.rk.clone())
    }
    fn cur_rk() -> Option<String> {
        current().map(|d| d.rk.clone())
    }

    /// The whole detail mailbox in one serial test — the statics are global, so splitting this
    /// into parallel #[test]s would have them racing each other rather than the code.
    #[test]
    fn a_detail_landing_only_installs_while_it_is_still_the_one_being_awaited() {
        // idle: nothing requested, nothing loading, nothing to pump
        assert!(!detail_loading(), "a fresh process is not loading anything");
        assert!(!pump_detail(), "an empty mailbox pumps nothing");

        // a request is in flight until its landing is pumped
        let gen = DETAIL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(detail_loading(), "a bumped generation with DONE behind it reads as in flight");
        landing(gen, "movie-1");
        assert!(pump_detail(), "the awaited landing installs");
        assert_eq!(cur_rk().as_deref(), Some("movie-1"));
        assert!(!detail_loading(), "pumping the landing settles the spinner");

        // SUPERSEDED: a second request means the first one's landing is stale and must be dropped
        let old = DETAIL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        let new = DETAIL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        landing(old, "stale-show");
        assert!(!pump_detail(), "a landing from a superseded generation is discarded");
        assert_eq!(cur_rk().as_deref(), Some("movie-1"), "and it must not touch CURRENT");
        assert!(detail_loading(), "the NEWER request is still in flight");

        // MONOTONE mailbox: with the newer result already sitting unconsumed, the OLDER fetch
        // finally returns — it must not overwrite it. (This is the case that wedged the season
        // mailbox before its guard existed: losing the newest result stalled the spinner on.)
        landing(new, "fresh-show");
        landing(old, "stale-show");
        assert_eq!(slot_rk().as_deref(), Some("fresh-show"), "the late older landing is refused");
        assert!(pump_detail());
        assert_eq!(cur_rk().as_deref(), Some("fresh-show"));

        // a FAILED fetch (None) settles the spinner but keeps the previously loaded item
        let g = DETAIL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        *DETAIL_SLOT.lock().unwrap() = Some(DetailResult { gen: g, d: None });
        assert!(!pump_detail(), "a failed fetch reports no fresh item");
        assert_eq!(cur_rk().as_deref(), Some("fresh-show"), "and leaves the page as it was");
        assert!(!detail_loading(), "but it does settle the spinner");

        // CLOSING THE PAGE supersedes: a load requested on the way in must not repopulate
        // CURRENT behind whatever screen is mounted now.
        let inflight = DETAIL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        clear();
        assert!(!detail_loading(), "clear() settles the in-flight fetch");
        landing(inflight, "arrived-after-close");
        assert!(!pump_detail(), "a landing after close is dropped");
        assert_eq!(cur_rk(), None, "the page stays closed");
    }
}
