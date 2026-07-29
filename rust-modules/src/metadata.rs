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
    /// PMS `Stream.selected` — the server's CURRENT pick for this part, i.e. the track a user
    /// chose on ANY Plex client (phone, web, another TV) and the one `select_streams` writes.
    /// `route`'s selection ladder prefers it over its own defaults, which is what makes a pick
    /// made elsewhere survive here instead of being silently overwritten. NB for AUDIO the server
    /// marks a selected stream on essentially every part — for an untouched one that is just the
    /// container `default` echoed back — so `route::pick_dp_audio` only treats it as a choice when
    /// it names a DIFFERENT stream, and never as a reason to transcode. Read its doc before using
    /// this flag anywhere else.
    pub(crate) selected: bool,
}

#[derive(Default)]
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
    /// `viewCount ≥ 1` — played through at least once. Deliberately INDEPENDENT of `resume_ms`
    /// on the wire: PMS keeps both on an episode that was finished and then started again, so
    /// which of the two a tile shows is a presentation rule at the draw site (see
    /// `ui/detail.rs`'s filmstrip), not a mutual exclusion the data layer can assume.
    pub(crate) watched: bool,
    pub(crate) part: String,   // Media[0].Part[0].key (to play)
    pub(crate) rating: String,
    pub(crate) vcodec: String, // Media[0].videoCodec (for the direct-play/transcode decision)
    pub(crate) acodec: String, // Media[0].audioCodec
}

// Deliberately NOT `Default`: every construction site spells every field, so adding one to a
// season is a compile error at each of them rather than a silent zero (the counts below are
// exactly the kind of field that reads as a legitimate value when it defaults).
pub(crate) struct Season {
    pub(crate) rk: String,
    pub(crate) index: i64,
    pub(crate) title: String,
    /// episodes in this season (`leafCount`); 0 when the server sent no count
    pub(crate) leaf_count: i64,
    /// how many of those are watched (`viewedLeafCount`)
    pub(crate) viewed_leaf_count: i64,
}

impl Season {
    /// Every episode of this season is watched — the season-scope form of the container rule
    /// `fetch_detail` applies to a show (`viewed >= leaf && leaf > 0`). It lives here, on the data,
    /// so the season tab's tick and the coming "Mark Season Watched" row read ONE truth instead of
    /// each re-deriving the comparison at its own site. The `leaf_count > 0` half is load-bearing:
    /// a season the server sent no counts for is `0 >= 0`, which would otherwise read as watched.
    pub(crate) fn watched(&self) -> bool {
        self.leaf_count > 0 && self.viewed_leaf_count >= self.leaf_count
    }
}

pub(crate) struct Related {
    pub(crate) rk: String,
    pub(crate) title: String,
    pub(crate) thumb: String,
}

/// Clone because the playing-item store keeps the played leaf's OWN chapters (see [`PlayingItem`]) —
/// on the detail-page play path they are cloned from the already-loaded `Detail` rather than refetched.
#[derive(Clone)]
pub(crate) struct Chapter {
    pub(crate) index: i64,    // 1-based chapter number
    pub(crate) start_ms: i64, // startTimeOffset — the seek target + timestamp label
    pub(crate) title: String, // Chapter.tag; empty → UI shows "Chapter {index}"
    pub(crate) thumb: String, // server image path → resolve_tex (empty if no chapter thumbs)
}

/// Parse an item's `Chapter[]` into the app's model — the ONE `plex::Chapter` → [`Chapter`] mapping,
/// shared by the detail parse and the playing-item store (which must agree: the Chapters strip seeks
/// with these offsets, so two mappings is two chances to disagree about which item they describe).
fn convert_chapters(chapters: &[crate::plex::Chapter]) -> Vec<Chapter> {
    chapters
        .iter()
        .map(|c| Chapter {
            index: c.index,
            start_ms: c.start_time_offset,
            title: c.tag.clone(),
            thumb: c.thumb.clone(),
        })
        .collect()
}

/// Which timeline segment a [`Marker`] describes. Only the two the player acts on are modelled —
/// PMS also emits `commercial` on recorded content, which [`convert_markers`] drops, so an
/// unhandled kind can never be mistaken for one of these.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MarkerKind {
    Intro,
    Credits,
}

/// A server-detected intro / credits segment of the playing item (`?includeMarkers=1`). Drives
/// the in-player Skip prompt and — for an episode with something queued after it — the moment the
/// Up Next control takes over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Marker {
    pub(crate) kind: MarkerKind,
    pub(crate) start_ms: i64,
    pub(crate) end_ms: i64,
    /// this credits segment runs to the end of the item (PMS `final: true`)
    pub(crate) final_seg: bool,
}

/// Parse a leaf's `Marker[]` into the app's model, dropping kinds the player has no behaviour for
/// and any segment whose offsets are not a forward range (a zero-length or inverted marker would
/// otherwise produce a prompt that can never be satisfied by seeking to its end).
fn convert_markers(markers: &[crate::plex::Marker]) -> Vec<Marker> {
    markers
        .iter()
        .filter_map(|m| {
            let kind = match m.kind.as_str() {
                "intro" => MarkerKind::Intro,
                "credits" => MarkerKind::Credits,
                _ => return None,
            };
            (m.end_time_offset > m.start_time_offset && m.start_time_offset >= 0).then_some(Marker {
                kind,
                start_ms: m.start_time_offset,
                end_ms: m.end_time_offset,
                final_seg: m.is_final != 0,
            })
        })
        .collect()
}

/// Segments the user has already skipped in THIS playback, identified by kind + start (a leaf has
/// at most an intro and a credits, so this never grows past two).
static mut SKIPPED: Vec<(MarkerKind, i64)> = Vec::new();

/// Record that `m` has been skipped, so it is never offered again for this item.
///
/// This is what makes skipping terminal, and it is not belt-and-braces. `av_seek_frame` is called
/// with `AVSEEK_FLAG_BACKWARD` (`ff.rs`), so it lands on the keyframe **at or before** the target —
/// seeking to a marker's `end_ms` therefore resumes a few seconds INSIDE the segment, whose keyframe
/// spacing is the file's, not ours. Without this latch the button reappeared moments after the skip
/// and pressing it seeked to the same place again: press → jump back a little → press → forever.
/// Padding the seek target cannot fix that (keyframe intervals vary from 2 s to 10 s); refusing to
/// re-offer a segment the user has already dismissed can, and is what they meant by the press.
pub(crate) fn mark_skipped(m: Marker) {
    let key = (m.kind, m.start_ms);
    unsafe {
        let v = &mut *addr_of_mut!(SKIPPED);
        if !v.contains(&key) {
            v.push(key);
        }
    }
}

/// The segment the playhead is inside right now, or None — the ONE live "what am I in" read, so
/// no UI module has to re-derive it (and none has to ask another module a question about it).
///
/// Gated on `is_playing()`: through the whole pre-roll (Connecting/Buffering/Seeking) `playpos_ns`
/// is still 0 or frozen at a seek target, and an item whose intro starts at 0 would otherwise
/// report a segment during every load. Segments already skipped are filtered out — see
/// [`mark_skipped`].
pub(crate) fn active_marker() -> Option<Marker> {
    if !crate::player::is_playing() {
        return None;
    }
    let m = marker_at(playing_markers(), crate::player::playpos_ns() / 1_000_000)?;
    let skipped = unsafe { &*addr_of!(SKIPPED) };
    (!skipped.contains(&(m.kind, m.start_ms))).then_some(m)
}

/// The marker containing `pos_ms`, if any — the ONE "am I inside a skippable segment" rule, shared
/// by the skip prompt and the end-of-episode handoff so they can never disagree about where a segment
/// begins. The range is half-open (`start <= pos < end`) so the prompt clears itself the instant a
/// skip lands on `end_ms` rather than re-offering the segment it just left.
///
/// A `final` credits marker is treated as running to `i64::MAX` rather than its stated `end_ms`:
/// PMS sets that end to the container duration, but our playhead is the DECODER's, which routinely
/// stops a few hundred ms short of it — so the prompt would blink out over the last frames.
pub(crate) fn marker_at(markers: &[Marker], pos_ms: i64) -> Option<Marker> {
    markers
        .iter()
        .find(|m| {
            let end = if m.final_seg { i64::MAX } else { m.end_ms };
            pos_ms >= m.start_ms && pos_ms < end
        })
        .copied()
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
    pub(crate) markers: Vec<Marker>, // intro / credits segments (leaf items only)
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
        chapters: convert_chapters(&it.chapter),
        markers: convert_markers(&it.marker),
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
            // the server's current pick for this part (a track chosen on another client)
            selected: s.selected != 0,
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

// ---- the PLAYING-item store — the in-player source of truth ---------------------------------
// Unlike `current()` (the detail page's item — it stays on the SHOW during an episode play, and
// can be a different item entirely when playing straight from Home), this always holds the
// played leaf's OWN data. The track menu and the route's audio pick read its streams; feeding a
// menu built from episode 1's streams to a playback of episode 5 was a real track-identity bug.
// `markers` is here for exactly that reason and not on `Detail`: skipping episode 1's intro
// timing during episode 5 is the same bug wearing a different hat. `chapters` rides along for the
// third instance of it: the Chapters tab and strip used to read `current()`, so an episode played
// from a SHOW page found a show container (which carries no Chapter[]) and the tab silently
// vanished — while a `current()` holding some OTHER leaf would have seeked with its offsets.

#[derive(Clone)]
pub(crate) struct PlayingItem {
    pub(crate) rk: String,
    pub(crate) audio: Vec<Stream>,
    pub(crate) subs: Vec<Stream>,
    pub(crate) video_fps: f64, // the played leaf's video fps (0 = unknown) — feeds the Load esInfo
    pub(crate) markers: Vec<Marker>, // intro / credits segments — the in-player Skip prompt
    pub(crate) chapters: Vec<Chapter>, // chapter boundaries — the Chapters tab/strip + rail ticks
}
static mut PLAYING: Option<PlayingItem> = None;

/// the playing leaf's own streams + markers (None until a catalog item starts playing).
/// Main-thread only.
pub(crate) fn playing() -> Option<&'static PlayingItem> {
    unsafe { (*addr_of!(PLAYING)).as_ref() }
}

/// The playing leaf's markers, or an empty slice — the ONE accessor the in-player skip prompt
/// reads, so no call site has to know the store can be absent mid-resolve.
pub(crate) fn playing_markers() -> &'static [Marker] {
    playing().map(|p| p.markers.as_slice()).unwrap_or(&[])
}

/// The playing leaf's chapters, or an empty slice — the ONE accessor the Chapters strip and the
/// scrubber's chapter ticks read. Deliberately NOT `current()`: during a show-page episode play
/// `current()` is the SHOW (no `Chapter[]` at all), which is why the tab never appeared on that
/// path, and a `current()` holding a different leaf would seek with another item's offsets.
pub(crate) fn playing_chapters() -> &'static [Chapter] {
    playing().map(|p| p.chapters.as_slice()).unwrap_or(&[])
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
pub(crate) fn cached_playing(rk: &str) -> Option<PlayingItem> {
    current().filter(|d| d.rk == rk && !d.audio.is_empty()).map(|d| PlayingItem {
        rk: rk.to_string(),
        audio: d.audio.clone(),
        subs: d.subs.clone(),
        video_fps: d.video_fps,
        markers: d.markers.clone(),
        chapters: d.chapters.clone(),
    })
}

pub(crate) fn fetch_playing_item(rk: &str) -> Option<PlayingItem> {
    if rk.is_empty() {
        return None;
    }
    let it = crate::plex::client_opt().and_then(|c| c.metadata(rk));
    // Markers and chapters hang off the ITEM, streams off its first Part — so a part-less response
    // still yields both of those instead of discarding all three. `Client::metadata` already sends
    // `includeChapters=1` (plex/library.rs), so the Chapter[] is on the wire either way: taking it
    // here costs no request, and dropping it is what hid the Chapters tab on the episode path.
    let markers = it.as_ref().map(|it| convert_markers(&it.marker)).unwrap_or_default();
    let chapters = it.as_ref().map(|it| convert_chapters(&it.chapter)).unwrap_or_default();
    let (audio, subs, video_fps) = it
        .as_ref()
        .and_then(|it| it.first_part().map(|p| convert_streams(&p.stream)))
        .unwrap_or_default();
    Some(PlayingItem { rk: rk.to_string(), audio, subs, video_fps, markers, chapters })
}

/// Retire BOTH descriptions of the item that was playing, together.
///
/// They have to move as one. `NOW` feeds the HUD caption and Info card; `PLAYING` feeds the track
/// menu and — since markers landed here — the skip/Up Next controls. Clearing only `NOW` (which is
/// what each play path used to do by hand) leaves the FINISHED episode's markers live for the whole
/// resolve + pre-roll of the next one, and a `final` credits marker is deliberately open-ended to
/// `i64::MAX`, so a stale one matches any playhead: the new episode would offer to skip its own
/// credits seconds after starting. Nothing fires today, but only by incidental ordering — this
/// makes it a contract instead.
pub(crate) fn retire_playing() {
    set_now_playing(None);
    retire_playing_item();
}

/// Retire ONLY the track/marker/chapter store, leaving the `NowPlaying` caption alone — what a NEW
/// play REQUEST does at its start ([`crate::route::request_play`]), beside the same retirement of
/// `UP_NEXT`.
///
/// The caption cannot be retired there because `detail::play_episode_at` sets it just BEFORE
/// requesting the play. The store must be, though: it is the PREVIOUS leaf's for the whole resolve
/// window (0.5-3 s, longer through a `/decision` handshake) and the HUD is up for all of it. With
/// chapters in here that became user-reachable — the transport advertised a Chapters tab whose OK
/// seeked the NEW episode to some other item's offset.
pub(crate) fn retire_playing_item() {
    unsafe {
        *addr_of_mut!(PLAYING) = None;
        (*addr_of_mut!(SKIPPED)).clear();
    }
}

/// MAIN THREAD: install a fetched playing-item store.
pub(crate) fn install_playing(pt: Option<PlayingItem>) {
    unsafe { (*addr_of_mut!(SKIPPED)).clear() }; // a different leaf's markers, so a fresh slate
    if let Some(pt) = &pt {
        crate::player::log(&format!(
            "playing item: rk={} audio={} subs={} markers={} chapters={}",
            pt.rk, pt.audio.len(), pt.subs.len(), pt.markers.len(), pt.chapters.len()));
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
            leaf_count: x.leaf_count,
            viewed_leaf_count: x.viewed_leaf_count,
        })
        .collect()
}

/// A season's episode list, or **None when the `/children` GET failed**. The failure has to stay
/// distinguishable from a genuinely empty season all the way to the pump: returning an empty Vec
/// for both is what let one transient GET blank a populated episode row, with no spinner, no error
/// and no way to ask the tab again. Same rule browse.rs's page fetch carries with its `total < 0`
/// sentinel ("a wiped-to-empty store here was a review-confirmed bug").
///
/// NB its siblings `fetch_seasons`/`fetch_related` deliberately KEEP the degrade-to-empty: both are
/// only ever called from `fetch_full`, which builds a Detail from nothing — there is no previous
/// list there to protect, and neither is worth failing the whole page over.
fn fetch_episodes(season_rk: &str) -> Option<Vec<Episode>> {
    let mc = crate::plex::client().children(season_rk)?;
    Some(mc.metadata.iter().map(convert_episode).collect())
}

/// One `/children` row → an [`Episode`]. Split out of [`fetch_episodes`] so the wire → model
/// mapping is host-testable without a PMS — the watched flag in particular is DERIVED, and a
/// derivation nothing can exercise is how `viewCount` came to be parsed at
/// `plex/models.rs` and then dropped on the floor here for the whole life of the episode row.
fn convert_episode(x: &crate::plex::Metadata) -> Episode {
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
        // `viewCount` is ABSENT until the leaf has been watched once, so `> 0` is the whole test —
        // the same rule `fetch_detail` applies to a movie. (A show/season instead compares
        // `viewedLeafCount` to `leafCount`; an episode is a leaf and has neither.)
        watched: x.view_count > 0,
        part: x.first_part().map(|p| p.key.clone()).unwrap_or_default(),
        rating: x.content_rating.clone(),
        vcodec: media0.map(|m| m.video_codec.clone()).unwrap_or_default(),
        acodec: media0.map(|m| m.audio_codec.clone()).unwrap_or_default(),
    }
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
            // a first-season failure is not worth failing the whole page over — the hero, cast
            // and Related still load, and there is no previous list here to protect
            d.episodes = fetch_episodes(&s0.rk).unwrap_or_default();
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
    rk: String,  // the show the fetch was for
    idx: usize,  // the season it was for
    prev: usize, // the season `cur_season` held before the optimistic flip — restored on failure
    eps: Option<Vec<Episode>>, // None = the fetch failed or panicked — the row keeps its episodes
}
static SEASON_RESULT: std::sync::Mutex<Option<SeasonResult>> = std::sync::Mutex::new(None);

/// Post a finished season fetch to the mailbox. MONOTONE: an older fetch landing late must never
/// clobber a newer result the pump hasn't consumed yet — that lost the newest season forever, and
/// with it the SEASON_DONE catch-up, wedging the loading spinner on. Named rather than inlined in
/// the worker closure for the same reason as `land_detail`: the guard is the one piece of this
/// machinery a test cannot reach through `load_season`.
fn land_season(gen: u32, rk: String, idx: usize, prev: usize, eps: Option<Vec<Episode>>) {
    let mut slot = SEASON_RESULT.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().map(|r| r.gen < gen).unwrap_or(true) {
        *slot = Some(SeasonResult { gen, rk, idx, prev, eps });
    }
}

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
    // `prev` rides along so a FAILED fetch can put the tab back on the season whose episodes are
    // still listed (see `pump_season`) — the optimistic flip below is what has to be undone.
    let (rk, season_rk, prev) = match current()
        .and_then(|d| d.seasons.get(idx).map(|s| (d.rk.clone(), s.rk.clone(), d.cur_season)))
    {
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
        // the mailbox is filled OUTSIDE the guard so a panicking fetch still lands — as a
        // FAILURE (None), not as an empty season: a panic is not "this season has no episodes",
        // and otherwise season_loading() would report an in-flight fetch forever
        let eps = catch_unwind(|| fetch_episodes(&season_rk)).unwrap_or(None);
        land_season(gen, rk, idx, prev, eps);
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
        // `unwrap_or_default`, NOT propagation: this blocking twin still degrades to an empty
        // list on failure. Making it preserve the previous season would silently change what
        // `open_rk_season`'s chained play of `episodes[0]` launches — the WRONG season's first
        // episode under the requested season's name — and that path has no host coverage and needs
        // the full on-device suite. Deferred deliberately.
        let eps = fetch_episodes(&season_rk).unwrap_or_default();
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
    // SETTLE THE SPINNER FIRST — on failure as much as on success. `season_loading()` drives the
    // episode row's loading dim + spinner AND gates `play_episode_at`, so a failure that returned
    // before this store would spin that row and refuse every episode press for the rest of the
    // session.
    SEASON_DONE.store(r.gen, Ordering::SeqCst);
    unsafe {
        let Some(d) = (*addr_of_mut!(CURRENT)).as_mut() else { return false };
        if d.rk != r.rk {
            return false; // the page moved to another item — not ours to patch
        }
        match r.eps {
            Some(eps) => {
                d.episodes = eps;
                d.cur_season = r.idx;
                true
            }
            None => {
                // THE FETCH FAILED. Keep the episodes already on screen — one transient
                // `/children` failure used to blank a populated row, with no spinner and no error.
                // And put `cur_season` back on the season those episodes belong to: the tab
                // highlight and the row must agree (`play_episode_at` launches `episodes[i]` under
                // whichever tab reads selected), and it is what makes the tab RETRYABLE — both
                // load paths fetch only when the target `!= cur_season`, so a tab left marked
                // selected could never be asked for again.
                d.cur_season = r.prev;
                false
            }
        }
    }
}

#[cfg(test)]
mod marker_tests {
    use super::*;

    fn wire(kind: &str, start: i64, end: i64, is_final: bool) -> crate::plex::Marker {
        crate::plex::Marker {
            kind: kind.to_string(),
            start_time_offset: start,
            end_time_offset: end,
            is_final: is_final as i64,
        }
    }
    /// The Morning Show S2E2 as the live server actually returns it (2026-07-29): an intro and a
    /// `final` credits marker, in that wire order — credits FIRST, which is why nothing here may
    /// assume the array is sorted by time.
    fn morning_show() -> Vec<Marker> {
        convert_markers(&[
            wire("credits", 3_065_648, 3_130_720, true),
            wire("intro", 990, 99_625, false),
        ])
    }

    #[test]
    fn only_the_kinds_the_player_acts_on_survive_parsing() {
        let m = morning_show();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].kind, MarkerKind::Credits);
        assert!(m[0].final_seg);
        assert_eq!(m[1].kind, MarkerKind::Intro);
        assert!(!m[1].final_seg);
        // `commercial` (PMS emits it on recorded content) has no behaviour — it must be DROPPED,
        // not defaulted into one of the two, or the pill would offer to skip an ad break as an intro.
        assert!(convert_markers(&[wire("commercial", 10, 20, false)]).is_empty());
        assert!(convert_markers(&[wire("", 10, 20, false)]).is_empty());
    }

    #[test]
    fn a_degenerate_range_is_dropped_rather_than_offered() {
        // A zero-length or inverted marker would produce a prompt that seeking to `end_ms` can
        // never satisfy — the pill would sit there and the press would do nothing.
        assert!(convert_markers(&[wire("intro", 500, 500, false)]).is_empty());
        assert!(convert_markers(&[wire("intro", 900, 100, false)]).is_empty());
        assert!(convert_markers(&[wire("credits", -5, 100, true)]).is_empty());
    }

    #[test]
    fn the_playhead_selects_the_segment_it_is_inside() {
        let m = morning_show();
        assert!(marker_at(&m, 0).is_none(), "before the intro starts (it begins at 990ms)");
        assert_eq!(marker_at(&m, 990).unwrap().kind, MarkerKind::Intro, "inclusive at the start");
        assert_eq!(marker_at(&m, 50_000).unwrap().kind, MarkerKind::Intro);
        assert!(marker_at(&m, 99_625).is_none(), "EXCLUSIVE at the end: skipping to it clears the pill");
        assert!(marker_at(&m, 2_000_000).is_none(), "the long middle of the episode");
        assert_eq!(marker_at(&m, 3_065_648).unwrap().kind, MarkerKind::Credits);
        assert!(marker_at(&[], 1234).is_none());
    }

    #[test]
    fn a_final_credits_marker_holds_past_its_stated_end() {
        // PMS sets a `final` marker's end to the CONTAINER duration, but our playhead is the
        // decoder's and routinely stops short of it — an exclusive end there made the pill blink
        // out over the last frames, exactly when it is being reached for.
        let m = morning_show();
        assert!(marker_at(&m, 3_130_720).is_some(), "at the stated end");
        assert!(marker_at(&m, 3_130_720 + 5_000).is_some(), "and past it");

        // A NON-final credits marker (credits before a post-credits scene) must still end, or
        // playback past it would keep offering a skip for a segment already behind the playhead.
        let mid = convert_markers(&[wire("credits", 1000, 2000, false)]);
        assert!(marker_at(&mid, 1500).is_some());
        assert!(marker_at(&mid, 2000).is_none(), "a non-final segment ends where it says it does");
    }
}

#[cfg(test)]
mod episode_tests {
    use super::*;

    /// One `/library/metadata/{season}/children` row, shaped the way PMS actually sends one — the
    /// counters STRING-encoded, which is the form `models.rs`'s lenient `de_i64` exists for. Goes
    /// through serde on purpose rather than hand-building a `Metadata`: the DTO field and the
    /// mapping are the two halves of this gap, and a hand-built struct would only ever exercise
    /// the half that was already right.
    fn row(extra: &str) -> crate::plex::Metadata {
        let json = format!(
            r#"{{"type":"episode","ratingKey":"1804","index":"3","parentIndex":"2",
                 "title":"Ep","duration":"3000000"{extra}}}"#
        );
        serde_json::from_str(&json).expect("a /children row parses")
    }

    /// The gap this closes: `viewCount` was parsed at the DTO and then never copied onto
    /// [`Episode`], so a fully-watched episode and one never started carried identical values all
    /// the way to the filmstrip. No `testlock` here — `convert_episode` is pure and reads no
    /// crate global.
    #[test]
    fn view_count_on_the_wire_becomes_the_episode_watched_flag() {
        // ABSENT is the unwatched case: PMS omits the key entirely rather than sending 0, which is
        // why the flag can be a presence test and why a missing field must default to false.
        let e = convert_episode(&row(""));
        assert!(!e.watched, "an absent viewCount is unwatched");
        assert_eq!(e.resume_ms, 0, "and carries no resume point");
        assert_eq!((e.rk.as_str(), e.index, e.season), ("1804", 3, 2), "the rest still maps");

        // …and a literal 0 is unwatched too. PMS omits the key rather than sending this, but
        // `de_i64` would deliver a real 0, so the flag must be a THRESHOLD and not a presence test
        // on the JSON — the two only agree while the server keeps omitting.
        assert!(!convert_episode(&row(r#","viewCount":0"#)).watched, "an explicit 0 is unwatched");

        assert!(convert_episode(&row(r#","viewCount":"1""#)).watched, "watched once");
        assert!(convert_episode(&row(r#","viewCount":4"#)).watched, "and re-watched, sent numeric");

        // Watched AND resuming is a real server state — finished, then started again. Both must
        // survive the mapping: the mutual exclusion is a rule of the DRAW site (which shows the
        // resume bar over the check), so collapsing it here would silently lose the resume point.
        let both = convert_episode(&row(r#","viewCount":"1","viewOffset":"120000""#));
        assert!(both.watched, "a re-started episode is still watched");
        assert_eq!(both.resume_ms, 120_000, "and keeps the resume point the player needs");
    }
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
        let _serial = crate::testlock::serial();
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

    // ---- the season mailbox -----------------------------------------------------------------

    /// A two-season show with a populated episode row, as a landed detail fetch leaves it. Written
    /// straight into CURRENT rather than through `pump_detail` — that pump is the other test's
    /// subject, and routing through it would couple the two.
    fn install_show(rk: &str, cur: usize, eps: &[&str]) {
        unsafe {
            *addr_of_mut!(CURRENT) = Some(Detail {
                rk: rk.to_string(),
                is_show: true,
                seasons: vec![
                    Season { rk: "sk1".to_string(), index: 1, title: "Season 1".to_string(), leaf_count: 0, viewed_leaf_count: 0 },
                    Season { rk: "sk2".to_string(), index: 2, title: "Season 2".to_string(), leaf_count: 0, viewed_leaf_count: 0 },
                ],
                episodes: eps.iter().map(|e| episode(e)).collect(),
                cur_season: cur,
                ..Default::default()
            })
        };
    }
    fn episode(rk: &str) -> Episode {
        Episode { rk: rk.to_string(), ..Default::default() }
    }
    fn listed_eps() -> Vec<String> {
        current().map(|d| d.episodes.iter().map(|e| e.rk.clone()).collect()).unwrap_or_default()
    }
    /// which season tab reads *selected* — the tabs pill `d.cur_season`; the focus ring is a
    /// separate, view-local column
    fn selected_tab() -> usize {
        current().map(|d| d.cur_season).unwrap_or(usize::MAX)
    }
    /// arm a season switch exactly as `load_season` does — flip the tab optimistically, then take
    /// the generation. Hands back what the worker carries to `land_season`.
    fn begin_switch(to: usize) -> (u32, usize) {
        let prev = selected_tab();
        unsafe {
            if let Some(d) = (*addr_of_mut!(CURRENT)).as_mut() {
                d.cur_season = to;
            }
        }
        (SEASON_GEN.fetch_add(1, Ordering::SeqCst) + 1, prev)
    }

    /// The whole season mailbox in one serial test — same shape and same reason as the detail one
    /// above: the statics are global, so splitting this into parallel `#[test]`s would have them
    /// racing each other rather than the code.
    ///
    /// The FIRST block is the audit finding: `fetch_episodes` returned an empty Vec for BOTH "this
    /// season has no episodes" and "the `/children` GET failed", and `pump_season` installed it
    /// either way — so one transient PMS failure blanked a populated episode row, with no spinner
    /// and no error, onto a tab that could then never be asked again. The blocks after it cover the
    /// supersede / monotone / wrong-item guards this change rewrites; on their own they would pass
    /// before and after, which is why they live inside the failing test rather than beside it.
    #[test]
    fn a_season_landing_only_installs_while_it_is_still_the_one_being_awaited() {
        let _serial = crate::testlock::serial();

        // A FAILED /children GET. It must not be mistaken for a season with no episodes.
        install_show("show-1", 0, &["s1e1", "s1e2"]);
        let (gen, prev) = begin_switch(1);
        assert_eq!(selected_tab(), 1, "the tab flips optimistically while the fetch is in flight");
        assert!(season_loading(), "a bumped generation with DONE behind it reads as in flight");
        land_season(gen, "show-1".to_string(), 1, prev, None);
        assert!(!pump_season(), "a failed fetch is not a new episode list");
        assert_eq!(listed_eps(), ["s1e1", "s1e2"], "the populated row survives the failure");
        assert_eq!(selected_tab(), 0, "the failed tab is released, so focusing it again refetches");
        assert!(!season_loading(), "the episode row must still come out of its loading state");

        // A season that GENUINELY has no episodes is a SUCCESS: the row clears. This is why the
        // discriminant is an Option and not an `is_empty()` check — a "keep the old list whenever
        // the new one is empty" fix passes the block above and leaves THIS one showing the
        // previous season's episodes under the new season's tab.
        let (gen, prev) = begin_switch(1);
        land_season(gen, "show-1".to_string(), 1, prev, Some(Vec::new()));
        assert!(pump_season(), "an empty season is a successful fetch — the row did change");
        assert!(listed_eps().is_empty(), "and the previous season's episodes are gone");
        assert_eq!(selected_tab(), 1, "the tab stays on the season that answered");

        // the ordinary success path
        let (gen, prev) = begin_switch(0);
        land_season(gen, "show-1".to_string(), 0, prev, Some(vec![episode("s1e1")]));
        assert!(pump_season());
        assert_eq!(listed_eps(), ["s1e1"]);
        assert_eq!(selected_tab(), 0);

        // SUPERSEDED: a blocking `load_season_now`, or a new item's `request_detail`, bumps the
        // generation — the fetch that was in flight for the old tab is dropped, not applied.
        let (old, prev) = begin_switch(1);
        supersede_season();
        land_season(old, "show-1".to_string(), 1, prev, Some(vec![episode("s2e1")]));
        assert!(!pump_season(), "a landing from a superseded generation is discarded");
        assert_eq!(listed_eps(), ["s1e1"], "and it must not touch the episode row");

        // MONOTONE mailbox: with a newer result sitting unconsumed, an older fetch finally
        // returning must not overwrite it. Losing the newest season that way also lost its
        // SEASON_DONE catch-up, which wedged the loading spinner on.
        let (old, prev) = begin_switch(1);
        let (new, _) = begin_switch(1);
        land_season(new, "show-1".to_string(), 1, prev, Some(vec![episode("fresh")]));
        land_season(old, "show-1".to_string(), 1, prev, Some(vec![episode("stale")]));
        assert!(pump_season(), "the newest season lands");
        assert_eq!(listed_eps(), ["fresh"], "the late older landing was refused");

        // A LANDING FOR ANOTHER ITEM: the page can move (Related -> a new detail) while a season
        // fetch is in flight, and those episodes belong to nobody on screen. It must still settle
        // the spinner — nothing else is going to.
        let (gen, prev) = begin_switch(1);
        install_show("show-2", 0, &["other-e1"]);
        land_season(gen, "show-1".to_string(), 1, prev, Some(vec![episode("s2e1")]));
        assert!(!pump_season(), "a landing for a different item reports no change");
        assert_eq!(listed_eps(), ["other-e1"], "and leaves the item now on screen alone");
        assert!(!season_loading(), "but it still settles the spinner");

        clear();
    }

    /// The season-scope watched rule. Pure (no crate global, so no `testlock` here) and worth its
    /// own test because two very different call sites depend on it — the season tab draws a tick
    /// off it, and "Mark Season Watched" will decide which way to scrobble off it. The counts are
    /// the ones a live `/library/metadata/{show}/children` returned: `idx=1 leaves=10 viewed=10`
    /// and `idx=2 leaves=10 viewed=1`.
    #[test]
    fn a_season_is_watched_only_when_the_server_counted_episodes_and_all_of_them_are_seen() {
        let season = |leaf: i64, viewed: i64| Season {
            rk: String::new(),
            index: 0,
            title: String::new(),
            leaf_count: leaf,
            viewed_leaf_count: viewed,
        };
        assert!(season(10, 10).watched(), "every episode seen");
        assert!(!season(10, 1).watched(), "one episode in is not watched");
        assert!(!season(10, 0).watched(), "never started");
        // A season the server sent no counts for is 0 >= 0 — the `leaf_count > 0` half of the rule
        // is the only thing keeping "we don't know" from reporting as "fully watched".
        assert!(!season(0, 0).watched(), "no counts is not a watched season");
        // viewedLeafCount can lead leafCount right after a scrobble of a season being re-indexed;
        // more-watched-than-exists is still watched, never a negative remainder.
        assert!(season(10, 11).watched(), "an over-count is still watched");
    }
}
