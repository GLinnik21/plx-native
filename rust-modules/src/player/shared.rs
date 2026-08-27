//! player::shared — the ONLY cross-thread state. Every field replaces a `volatile`
//! global from playback.c and is an atomic or a Mutex (never a bare value). One
//! long-lived `static SHARED` in mod.rs: it outlives every start/stop cycle and is
//! *reset*, never freed — so a late library callback after teardown writes to a
//! live object, exactly as the C static globals behaved.
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicPtr, AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;
use crate::stream::HttpStream;

/// **The names the CONTAINER gives its audio and subtitle tracks**, each list in file order.
///
/// Published once by the demuxer when it opens a part ([`crate::ff`]), read by the in-player track
/// menu. Both `Vec`s are dense — a track the file does not name contributes an EMPTY string rather
/// than being skipped — because position is the whole join: the N-th entry is the N-th stream of
/// that type, which is the ordinal `metadata::sub_render_ordinal` resolves a menu row to. Skipping
/// unnamed tracks would silently shift every name after the first untagged one onto its neighbour,
/// which is worse than showing none: a wrong name is indistinguishable from a right one.
#[derive(Default)]
pub(crate) struct TrackNames {
    pub audio: Vec<String>,
    pub subs: Vec<String>,
}

impl TrackNames {
    /// `Default`, but callable from `Shared::new`, which is a `const fn`.
    pub const fn new() -> Self {
        Self { audio: Vec::new(), subs: Vec::new() }
    }
    /// The name of the `i`-th subtitle stream in file order, or `""` — `i` is what
    /// `metadata::sub_render_ordinal` answers, and its `-1` (an external sidecar, which is not in
    /// the container at all) can be passed straight in.
    pub fn sub(&self, i: i32) -> &str {
        usize::try_from(i).ok().and_then(|i| self.subs.get(i)).map(String::as_str).unwrap_or("")
    }
    /// The same for audio — `metadata::audio_ordinal`'s answer.
    pub fn audio(&self, i: i32) -> &str {
        usize::try_from(i).ok().and_then(|i| self.audio.get(i)).map(String::as_str).unwrap_or("")
    }
}

/// one client-rendered subtitle cue (content-time ns). `track` is the 0-based subtitle-stream
/// index it belongs to; the demuxer pushes cues for ALL text tracks and the render filters by
/// the selected `desired_sub_idx`, so switching tracks is instant (no re-demux of the buffered
/// region). Content-time ns matches the fed video PTS timeline (see active_subtitle).
pub(crate) struct SubCue {
    pub track: i32,
    pub start_ns: i64,
    pub end_ns: i64,
    pub text: String,
}

/// one rect of a decoded image-subtitle display set. `rgba` is a straight-alpha bitmap of
/// `w`×`h` at position (`x`,`y`) **in the subtitle stream's own authoring canvas** (see
/// [`SubBitmap::cw`]) — NOT in screen pixels; the renderer scales it into the video rect.
#[derive(Clone)]
pub(crate) struct SubRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub rgba: Vec<u8>,
}

/// one decoded image-subtitle display set (PGS/VobSub/DVB) — every rect of it, not just the
/// first: a two-line dialogue or a sign-plus-dialogue set is authored as several rects and they
/// belong to the same on-screen moment. `end_ns` is i64::MAX until a CLEAR display-set (or a
/// superseding set) truncates it. Unlike text cues we push ONLY the selected track (bitmaps are
/// heavier than text on this RAM-tight TV), keyed by `start_ns` for the renderer.
pub(crate) struct SubBitmap {
    pub track: i32,
    pub start_ns: i64,
    pub end_ns: i64,
    /// authoring-canvas width/height the rects' coords are expressed in — 1920×1080 for
    /// Blu-ray PGS, 720×480/576 for a DVD VobSub rip, 3840×2160 for some 4K PGS. `0` = the
    /// decoder never declared one, in which case the renderer falls back to 1:1.
    pub cw: i32,
    pub ch: i32,
    pub rects: Vec<SubRect>,
}
impl SubBitmap {
    /// total RGBA bytes held by this display set (what the store's byte budget counts)
    pub fn bytes(&self) -> usize {
        self.rects.iter().map(|r| r.rgba.len()).sum()
    }
}

/// What playback is actually doing, as one value the UI can render from.
///
/// This replaces the old single `seeking` boolean, which was only ever set by `request_seek` —
/// so `player::loading()` was **false for the whole initial load** and the `Spinner` that has sat
/// in `player_hud.rs` all along never fired on first play. The HUD drew a live-looking transport
/// at 0:00 / -0:00 instead, which is the half of the frozen-HUD report that is not blocking I/O.
///
/// Derived once per frame in `pump::set_state` from signals the workers already publish; no
/// new cross-thread plumbing. Ordered so `>= Playing` reads as "actually on screen".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum PlaybackState {
    /// no engine
    Idle = 0,
    /// the route/plan resolve is in flight — DERIVED in `player::state()` from `route::play_pending()`
    Resolving = 1,
    /// engine started, pipeline not yet loaded — the HTTP GET + Starfish `Load` window
    Connecting = 2,
    /// loaded and fed, but no frame has been presented yet
    Buffering = 3,
    /// a seek is resolving (reopen → prime → Play)
    Seeking = 4,
    /// frames on the panel
    Playing = 5,
    /// the producer died (a failed open / no video stream). Terminal until teardown.
    Error = 6,
}

impl PlaybackState {
    pub fn from_u8(v: u8) -> PlaybackState {
        match v {
            1 => PlaybackState::Resolving,
            2 => PlaybackState::Connecting,
            3 => PlaybackState::Buffering,
            4 => PlaybackState::Seeking,
            5 => PlaybackState::Playing,
            6 => PlaybackState::Error,
            _ => PlaybackState::Idle,
        }
    }
    /// "something is happening and the user should see a spinner, not a dead transport".
    pub fn is_busy(self) -> bool {
        matches!(
            self,
            PlaybackState::Resolving
                | PlaybackState::Connecting
                | PlaybackState::Buffering
                | PlaybackState::Seeking
        )
    }
    /// The one-line status the HUD shows beside the spinner. A `&'static CStr` (the house idiom
    /// for UI strings) so the draw path allocates nothing — this is read 60x/second for the whole
    /// load window, and indefinitely in `Error`.
    pub fn caption(self) -> &'static std::ffi::CStr {
        match self {
            PlaybackState::Resolving => c"Preparing…",
            PlaybackState::Connecting => c"Connecting…",
            PlaybackState::Buffering => c"Buffering…",
            PlaybackState::Seeking => c"Seeking…",
            PlaybackState::Error => c"Playback failed",
            _ => c"",
        }
    }
}

pub(crate) struct Shared {
    // library callback thread (K) -> main (M)
    pub playpos_ns: AtomicI64,               // g_playpos_ns
    // the presented frame's fed PTS (0-based, raw `num` from the type=0 callback). The feed
    // loop throttles on max_fed_pts - pres_fed so it stays ~MAX_FEED_AHEAD ahead of the
    // decoder (feeding further overfills the 4K HEVC DPB/CPB and stalls the sink).
    pub pres_fed: AtomicI64,
    pub frames: AtomicI32,                    // bf_frames
    /// **Has this SESSION ever put a picture on the panel?** Set by the frame-presented callback
    /// beside `frames`, cleared ONLY by [`Shared::reset_session`] — so it survives a seek, which
    /// `frames` deliberately does not: `pump` zeroes `frames` as *part of applying* a seek, which
    /// makes "we have never shown a frame" and "we just seeked" indistinguishable through it. The
    /// HUD's one rule for which surface owns the "pipeline is working" read-out keys on this bit;
    /// see `ui::player_hud::busy_surface`. Monotone within a session (false→true only), which is
    /// what lets two readers in one frame sample it independently without disagreeing.
    pub seen_frame: AtomicBool,
    pub load_completed: AtomicBool,           // bf_loaded signal
    pub media_id: Mutex<Option<CString>>,     // bf_mediaId (captured once)
    pub source_info: Mutex<Option<Vec<u8>>>,  // sourceInfoRaw, VERBATIM incl NUL

    // main/pump (M) -> library callback thread (K)
    pub pts_shift: AtomicI64,                 // g_pts_shift
    // content-time offset added to the displayed position: a transcode SEEK restarts the
    // stream 0-based (its PTS loses the seek offset), so playpos = fed - pts_shift + this.
    // 0 for direct-play (file PTS already IS content time) and for the initial transcode.
    pub disp_base: AtomicI64,

    // main/pump (M) -> demux (D)
    // The demux seek target in content-ns (-1 = none). The pump publishes it and the demux
    // thread consumes it with an av_seek_frame between two reads — it is the ENTIRE direct-play
    // seek mechanism, and also carries the resume offset armed before the first read. Nothing
    // interrupts the demuxer to make this happen; a `seek_byte` reopen trigger and a `next_url`
    // used to sit alongside it for that, and neither could ever fire (see ff.rs).
    pub seek_to_ns: AtomicI64,
    // the in-place seek's target content-ns, for the feed's rebase guard (drop stale drifted
    // keyframes). Distinct from seek_to_ns, which the demuxer consumes as it seeks. -1 = none.
    pub seek_target_ns: AtomicI64,
    // UI loading state: true from a seek request until playback resumes at the new position (prime→
    // Play). The HUD shows a spinner + freezes the playhead at `seek_display_ns` so it doesn't
    // wobble through the seek/rebase. -1 display = not loading.
    pub seeking: AtomicBool,
    pub seek_display_ns: AtomicI64,
    // pending audio-track switch: the Plex audioStreamID to switch to (-1 = none). The
    // pump forces a fresh transcode with that source audio at the current position.
    pub pending_audio_sid: AtomicI64,
    // NATIVE audio-track switch (direct-play, no transcode): the 0-based audio stream index
    // to feed. `pending` (-1 = none) is consumed by the pump to trigger a reload; `desired`
    // (-1 = av_find_best_stream) is read by the demuxer to pick the Nth audio stream and
    // PERSISTS across seeks/reloads (reset only on a new item, not in reset_session).
    pub pending_audio_idx: AtomicI32,
    pub desired_audio_idx: AtomicI32,
    // pending "re-transcode at the current position with the current audio + subtitle" —
    // set when a subtitle is (de)selected while transcoding, so Plex re-burns (or drops) it.
    pub pending_retranscode: AtomicBool,
    // the derived UI state (a `PlaybackState`), published by the pump once a frame and read by
    // the HUD. Written only on the main thread; atomic because it is read from the draw path.
    pub pb_state: AtomicU8,
    // demux (D) -> main (M): the producer died before publishing a duration, so the EOS path
    // (which needs `duration_ns > 0`) can never fire and the player would sit on a black screen
    // forever. The pump turns this into `PlaybackState::Error` so the HUD can say so.
    pub demux_failed: AtomicBool,
    /// The media transport failed after open, possibly after frames were already presented.
    /// Unlike `demux_failed`, this is not gated on a zero-frame start: a truncated live transfer
    /// is an error rather than a successful early EOF.
    pub demux_io_failed: AtomicBool,
    /// A sustained progressive Auto/Original starvation measurement (kbps), published by the
    /// demux worker and consumed once by the main-thread pump. `0` means no transition pending.
    /// One atomic carries both signal and evidence, so a reset cannot leave a stale companion
    /// value behind for the next playback.
    pub auto_fallback_kbps: AtomicI64,
    /// Sustained HLS evidence says the actual source is safe again. Consumed by the main-thread
    /// pump exactly like `auto_fallback_kbps`; zero means no recovery transaction is pending.
    pub auto_recover_kbps: AtomicI64,
    /// WHY the demuxer found nothing to feed, when the answer is "the stream itself": the server
    /// delivered audio streams and no video stream. Issue #22's whole shape — a transcode target
    /// the server cannot honour makes PMS drop the video track, and `ff: no video stream` alone
    /// sent the reviewer on a full server-side investigation the app could have named in one
    /// line. Set by the demux thread beside `demux_failed`; read on the main thread, which
    /// combines it with `route::is_transcoding()` to word the error (an audio-only TRANSCODE is
    /// the server's doing; an audio-only FILE is the file's).
    pub demux_no_video: AtomicBool,
    /// `sf_load` returned 0 — the pipeline REFUSED the Load payload.
    ///
    /// Without this the pump has no way to learn it: `load_thread` logged the result and dropped
    /// it, `loadCompleted` never arrives, and `Stage::Loading` is never left — so the player sits
    /// on a spinner forever with no error and no timeout. A rejected payload is the most likely
    /// shape of a webOS-5-specific failure (a key the newer pipeline will not accept), which makes
    /// this exactly the case that must be visible.
    pub load_failed: AtomicBool,

    // client-rendered subtitles: selected track index (-1 = off) + the demuxed cues.
    // demux (D) pushes cues; main (M) reads the active one for the current playpos.
    pub desired_sub_idx: AtomicI32,
    /// **The container's own per-track names, which PMS does not always send** (D -> M).
    ///
    /// `audio` and `subs`, each in FILE order, so position N is the N-th audio / subtitle stream of
    /// the part — the ordinal `metadata::audio_ordinal` and `metadata::sub_render_ordinal`
    /// already resolve a track-menu row to. Empty strings for a file that tags nothing, and an
    /// empty Vec until a demuxer has opened.
    ///
    /// It exists because for an **MP4** part PMS sends no `Stream.title` at all, though the file
    /// names every track (`"Полные Jaskier"`, `"Форс. iTunes"`). See `ff::stream_name`. So the
    /// picker's rows are the only place this reaches, and only on DIRECT PLAY: a transcode is a
    /// remux the server built, and its tags are whatever the server put there.
    pub track_names: Mutex<TrackNames>,
    pub sub_cues: Mutex<Vec<SubCue>>,
    pub sub_bitmaps: Mutex<Vec<SubBitmap>>, // image-sub cues (selected track only)

    // demux (D) -> main (M)
    pub file_size: AtomicI64,                 // g_file_size
    /// The DECODED FRAME SIZE, published by the demuxer once the video stream is known.
    ///
    /// Exists for the webOS 5+ exported window, whose `SDL_webOSSetExportedWindow(id, src, dst)`
    /// wants the frame you are FEEDING as `src` and the on-screen rect as `dst` — the pair is what
    /// expresses scaling. webOS 4's `AcbAPI_setDisplayWindow` takes only a destination, so nothing
    /// needed this before and the exported path was written passing the authoring canvas (1920x1080)
    /// for both. That is wrong for every 4K direct play, which is most of them.
    ///
    /// 0 until the demuxer has opened the stream; readers must treat 0 as "not known yet".
    pub video_w: AtomicI32,
    pub video_h: AtomicI32,
    pub duration_ns: AtomicI64,               // was g_mkv.duration_ns (published)
    /// Latest normalized timestamps successfully enqueued for each elementary stream. HLS writes
    /// its segment-normalized zero-based timeline; progressive Original writes absolute movie
    /// PTS. Their consumers apply the matching display-base rule. These are content-time facts
    /// (not queue byte counts); `-1` means the lane has not produced an AU in this session/seek.
    pub hls_video_tail_ns: AtomicI64,
    pub hls_audio_tail_ns: AtomicI64,
    // set once the pipeline has drained to true end-of-stream (EOS pushed AND the last fed frame
    // has been presented). app.rs polls player::ended() to tear the player down at the credits.
    pub ended: AtomicBool,

    // close-to-interrupt handle: raw ptr to the Engine-owned HttpStream box, so
    // the pump/teardown can close(fd) to unblock a blocked recv. The box outlives
    // the worker threads (Engine drops after join), so the ptr stays valid.
    pub hs_ptr: AtomicPtr<HttpStream>,

    // ---- diagnostics mirror (`ui::stats`) -------------------------------------------------
    // Values the RENDER path needs that live on the Engine, republished once per pump tick.
    // The render path may not call `engine(&MainThread)` — that hands out a `&'static mut` to a
    // `static mut`, and a second live borrow is instant UB — so it reads these instead.
    //
    // STRICTLY ONE-WAY: written by the pump and the seam, read only by the read-out. Nothing in
    // the playback state machine may ever branch on one, or a diagnostic becomes load-bearing.
    // One frame stale by construction, which no reader cares about at a 2 Hz sample.
    /// `Stage` as u8 — where the bind/play sequence has got to.
    pub dg_stage: AtomicU8,
    /// Starfish callbacks seen this session. A count of 0 with a completed Load is the pipeline
    /// never speaking to us — the sharpest single symptom there is. The TYPE of the last callback
    /// is deliberately not kept: the numbering shifts between webOS 4 and 5+ above 0x1c, so a
    /// displayed type would be a confident lie on the firmware we cannot test. `dg_cb_err` latches
    /// the one type that means the same on both.
    pub dg_cb_count: AtomicU32,
    /// Why the VIDEO feeder is where it is, taken at each of `feed_stream`'s exit points.
    /// 0 none yet · 1 accepting · 2 BufferFull · 3 refused · 4 waiting for a frame (feed-ahead
    /// throttle) · 5 queue empty.
    ///
    /// Supersedes a bare last-reply byte, which says strictly less: "BufferFull" and "the throttle
    /// held us back" and "there was nothing to send" are three different faults with three
    /// different fixes, and a reply byte cannot tell them apart because the last two never reach a
    /// `Feed()` call at all. `queue empty` vs `BufferFull` is what splits a dead PRODUCER from a
    /// dead SINK.
    pub dg_feed_state: AtomicU8,
    /// The first Starfish error callback (`ty == 18`) of this session, and the callback INDEX it
    /// arrived at. Sticky: a later healthy callback must not erase the error that explains the
    /// session, and the index says whether it was immediate or after a long healthy run.
    pub dg_cb_err: AtomicI32,
    pub dg_cb_err_at: AtomicU32,
    /// The media GET's HTTP status and the bytes actually delivered to the demuxer. Written by the
    /// DEMUX thread — the third writer of this block. 0/0 = no connection was ever made, which is
    /// a different failure from a connection that answered 401.
    pub dg_http_status: AtomicI32,
    pub dg_net_rx: AtomicI64,
    /// SDL ticks when `loadCompleted` landed, and when `frames` last CHANGED. A photograph has no
    /// time axis: "Load completed, 0 frames" is innocent at 2 s and damning at 4 minutes, and the
    /// panel cannot tell the difference without these. Stamped in the pump, never in `ui::stats` —
    /// a stats-local timer would start when the panel is OPENED, so a four-minute hang would
    /// photograph as twelve seconds.
    pub dg_load_at: AtomicU32,
    pub dg_frame_at: AtomicU32,
    /// **The video plane's own cadence** — the three counters behind the heartbeat's `vtick=`/`vgap=`.
    ///
    /// They exist because every other frame number in this app is about the GRAPHICS plane. `fps=`
    /// counts our GL swaps and `worstframe=` times our own draw, and both stay a flat 60/0.5 ms
    /// through playback that visibly stutters: the decoded picture is composited by the TV on a
    /// plane we never touch. The one place the pipeline tells us it put a frame on that plane is
    /// the `ty == 0` callback, so that is where these are stamped.
    ///
    /// Written on the LIBRARY thread (`sf_on_event`), drained on the main thread
    /// ([`crate::player::vplane_take`]) — hence a drained COUNT rather than a delta of
    /// [`frames`](Self::frames), which a seek resets underneath a reader.
    ///
    /// `dg_vpres_at` is the previous presentation's stamp and MUST be zeroed wherever `frames` is
    /// (session reset, and the post-seek re-count in the pump): the pipeline legitimately shows
    /// nothing across a seek, and a gap measured over that pause is the harness reporting the seek
    /// as a stutter.
    pub dg_vpres_ct: AtomicU32,
    pub dg_vpres_at: AtomicU32,
    pub dg_vpres_gap: AtomicU32,
    /// Bytes queued in each AU lane at the last tick, against `engine::aq_caps`.
    pub dg_aq_video: AtomicI64,
    pub dg_aq_audio: AtomicI64,
    /// High-water fed PTS per lane. Their DIFFERENCE is the sharpest instantaneous answer to
    /// "video plays but there is no sound": a snapshot of the fed COUNTS cannot see a lane that
    /// stopped advancing, because a large total stays large. A skew that grows without bound is
    /// the audio lane starving; a skew near zero says both lanes are keeping up and the fault is
    /// downstream of us.
    pub dg_fed_v_pts: AtomicI64,
    pub dg_fed_a_pts: AtomicI64,
    /// The codec strings this session actually put in the Starfish Load payload, as small codes
    /// (0 = none/not built; video 1 = H264, 2 = H265; audio 1 = AC3, 2 = AC3 PLUS, 3 = AAC).
    ///
    /// Codes rather than strings so they need no lock on the render path — and the PAYLOAD's view
    /// rather than a re-derivation, because the whole class of bug here is the payload disagreeing
    /// with the stream. `dg_load_a == 0` is `needAudio:false`, which is a complete answer to
    /// "there is no sound": the pipeline was never asked for any.
    pub dg_load_v: AtomicU8,
    pub dg_load_a: AtomicU8,
    /// Auto-quality facts written by the demux worker and read only by Stats for Nerds.
    /// Mode: 0 inactive, 1 progressive Original watchdog, 2 fixed-session HLS controller.
    pub dg_abr_mode: AtomicU8,
    /// Current Original source requirement or active HLS rung, in kbit/s.
    pub dg_abr_kbps: AtomicI64,
    /// Latest measured body throughput and normalized content reserve. `-1` means no complete
    /// measurement exists yet. Production ratio is total segment acquisition / media duration in
    /// per-mille; it is meaningful for HLS only.
    pub dg_abr_net_kbps: AtomicI64,
    pub dg_abr_buffer_ms: AtomicI64,
    pub dg_abr_ratio_pm: AtomicI64,
    /// Last controller action plus its candidate rung. The action is intentionally sticky so a
    /// photograph taken after a swap still explains why the current rendition changed.
    pub dg_abr_action: AtomicU8,
    pub dg_abr_target_kbps: AtomicI64,
    /// Consecutive Original windows whose starvation horizon sat inside the unsafe band.
    /// Wall milliseconds an unsafe Original deficit has held (N13). Was a COUNT of
    /// 750 ms active-read windows, on a clock that stops under backpressure.
    pub dg_abr_unsafe_deficit_ms: AtomicI64,
    /// **The controller's own model state**, published beside the measurements above so the
    /// read-out shows what the DECISION was made on rather than what a reader can infer. Every
    /// one is `-1` until the controller has produced it; `crate::abr::ControllerTelemetry` is the
    /// single struct they are all taken from, in one go, so the panel can never show a budget
    /// from one sample beside an uncertainty from the next.
    ///
    /// Safe budget (what selection may actually spend), the operating point the model would pick
    /// for this link ignoring hysteresis, the delivery estimate's own dispersion and sample count,
    /// the buffer's slope, its starvation horizon in seconds (`-1` = no deficit, so no horizon),
    /// the predicted production cost of the current candidate, and the risk score.
    pub dg_abr_safe_kbps: AtomicI64,
    pub dg_abr_optimal_kbps: AtomicI64,
    pub dg_abr_unc_pm: AtomicI64,
    pub dg_abr_samples: AtomicI64,
    pub dg_abr_slope_ms_per_s: AtomicI64,
    pub dg_abr_starve_secs: AtomicI64,
    pub dg_abr_pred_pm: AtomicI64,
    pub dg_abr_risk: AtomicI64,
    /// Why the last steady-state decision went the way it did — `crate::player::ABR_WHY_*`, `0`
    /// while nothing has decided anything. The plan's reason code, on the surface a user
    /// photographs rather than only in the event log.
    pub dg_abr_why: AtomicU8,
    /// `vp_place` return, and the size it was called with. `i32::MIN` = never called.
    pub dg_place_rv: AtomicI32,
    pub dg_placed_w: AtomicI32,
    pub dg_placed_h: AtomicI32,
}

impl Shared {
    pub const fn new() -> Self {
        Shared {
            dg_stage: AtomicU8::new(0),
            dg_cb_count: AtomicU32::new(0),
            dg_feed_state: AtomicU8::new(0),
            dg_cb_err: AtomicI32::new(0),
            dg_cb_err_at: AtomicU32::new(0),
            dg_http_status: AtomicI32::new(0),
            dg_net_rx: AtomicI64::new(0),
            dg_load_at: AtomicU32::new(0),
            dg_frame_at: AtomicU32::new(0),
            dg_vpres_ct: AtomicU32::new(0),
            dg_vpres_at: AtomicU32::new(0),
            dg_vpres_gap: AtomicU32::new(0),
            dg_aq_video: AtomicI64::new(0),
            dg_aq_audio: AtomicI64::new(0),
            dg_fed_v_pts: AtomicI64::new(0),
            dg_fed_a_pts: AtomicI64::new(0),
            dg_load_v: AtomicU8::new(0),
            dg_load_a: AtomicU8::new(0),
            dg_abr_mode: AtomicU8::new(0),
            dg_abr_kbps: AtomicI64::new(0),
            dg_abr_net_kbps: AtomicI64::new(-1),
            dg_abr_buffer_ms: AtomicI64::new(-1),
            dg_abr_ratio_pm: AtomicI64::new(-1),
            dg_abr_action: AtomicU8::new(0),
            dg_abr_target_kbps: AtomicI64::new(0),
            dg_abr_unsafe_deficit_ms: AtomicI64::new(0),
            dg_abr_safe_kbps: AtomicI64::new(-1),
            dg_abr_optimal_kbps: AtomicI64::new(-1),
            dg_abr_unc_pm: AtomicI64::new(-1),
            dg_abr_samples: AtomicI64::new(-1),
            dg_abr_slope_ms_per_s: AtomicI64::new(0),
            dg_abr_starve_secs: AtomicI64::new(-1),
            dg_abr_pred_pm: AtomicI64::new(-1),
            dg_abr_risk: AtomicI64::new(-1),
            dg_abr_why: AtomicU8::new(0),
            dg_place_rv: AtomicI32::new(i32::MIN),
            dg_placed_w: AtomicI32::new(0),
            dg_placed_h: AtomicI32::new(0),
            playpos_ns: AtomicI64::new(0),
            pres_fed: AtomicI64::new(0),
            frames: AtomicI32::new(0),
            seen_frame: AtomicBool::new(false),
            load_completed: AtomicBool::new(false),
            media_id: Mutex::new(None),
            source_info: Mutex::new(None),
            pts_shift: AtomicI64::new(0),
            disp_base: AtomicI64::new(0),
            seek_to_ns: AtomicI64::new(-1),
            seek_target_ns: AtomicI64::new(-1),
            seeking: AtomicBool::new(false),
            seek_display_ns: AtomicI64::new(-1),
            pending_audio_sid: AtomicI64::new(-1),
            pending_audio_idx: AtomicI32::new(-1),
            desired_audio_idx: AtomicI32::new(-1),
            pending_retranscode: AtomicBool::new(false),
            pb_state: AtomicU8::new(PlaybackState::Idle as u8),
            demux_failed: AtomicBool::new(false),
            demux_io_failed: AtomicBool::new(false),
            auto_fallback_kbps: AtomicI64::new(0),
            auto_recover_kbps: AtomicI64::new(0),
            demux_no_video: AtomicBool::new(false),
            load_failed: AtomicBool::new(false),
            desired_sub_idx: AtomicI32::new(-1),
            track_names: Mutex::new(TrackNames::new()),
            sub_cues: Mutex::new(Vec::new()),
            sub_bitmaps: Mutex::new(Vec::new()),
            file_size: AtomicI64::new(0),
            video_w: AtomicI32::new(0),
            video_h: AtomicI32::new(0),
            duration_ns: AtomicI64::new(0),
            hls_video_tail_ns: AtomicI64::new(-1),
            hls_audio_tail_ns: AtomicI64::new(-1),
            ended: AtomicBool::new(false),
            hs_ptr: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
    /// reset per-file state on stop (mirrors the tail of stop_bufferfeed).
    pub fn reset_session(&self) {
        // the diagnostics mirror is per-session too — a stale bind outcome from the last item is
        // exactly the misleading answer the read-out exists to avoid
        self.dg_stage.store(0, Ordering::Relaxed);
        self.dg_cb_count.store(0, Ordering::Relaxed);
        self.dg_feed_state.store(0, Ordering::Relaxed);
        // A latched error MUST be cleared here, or one bad session paints every later healthy
        // playback red for the life of the process.
        self.dg_cb_err.store(0, Ordering::Relaxed);
        self.dg_cb_err_at.store(0, Ordering::Relaxed);
        self.dg_http_status.store(0, Ordering::Relaxed);
        self.dg_net_rx.store(0, Ordering::Relaxed);
        self.dg_load_at.store(0, Ordering::Relaxed);
        self.dg_frame_at.store(0, Ordering::Relaxed);
        self.dg_vpres_ct.store(0, Ordering::Relaxed);
        self.dg_vpres_at.store(0, Ordering::Relaxed);
        self.dg_vpres_gap.store(0, Ordering::Relaxed);
        self.dg_aq_video.store(0, Ordering::Relaxed);
        self.dg_aq_audio.store(0, Ordering::Relaxed);
        self.dg_fed_v_pts.store(0, Ordering::Relaxed);
        self.dg_fed_a_pts.store(0, Ordering::Relaxed);
        self.dg_load_v.store(0, Ordering::Relaxed);
        self.dg_load_a.store(0, Ordering::Relaxed);
        self.dg_abr_mode.store(0, Ordering::Relaxed);
        self.dg_abr_kbps.store(0, Ordering::Relaxed);
        self.dg_abr_net_kbps.store(-1, Ordering::Relaxed);
        self.dg_abr_buffer_ms.store(-1, Ordering::Relaxed);
        self.dg_abr_ratio_pm.store(-1, Ordering::Relaxed);
        self.dg_abr_action.store(0, Ordering::Relaxed);
        self.dg_abr_target_kbps.store(0, Ordering::Relaxed);
        self.dg_abr_unsafe_deficit_ms.store(0, Ordering::Relaxed);
        self.dg_abr_safe_kbps.store(-1, Ordering::Relaxed);
        self.dg_abr_optimal_kbps.store(-1, Ordering::Relaxed);
        self.dg_abr_unc_pm.store(-1, Ordering::Relaxed);
        self.dg_abr_samples.store(-1, Ordering::Relaxed);
        self.dg_abr_slope_ms_per_s.store(0, Ordering::Relaxed);
        self.dg_abr_starve_secs.store(-1, Ordering::Relaxed);
        self.dg_abr_pred_pm.store(-1, Ordering::Relaxed);
        self.dg_abr_risk.store(-1, Ordering::Relaxed);
        self.dg_abr_why.store(0, Ordering::Relaxed);
        self.dg_place_rv.store(i32::MIN, Ordering::Relaxed);
        self.dg_placed_w.store(0, Ordering::Relaxed);
        self.dg_placed_h.store(0, Ordering::Relaxed);
        self.playpos_ns.store(0, Ordering::Relaxed);
        self.pres_fed.store(0, Ordering::Relaxed);
        self.frames.store(0, Ordering::Relaxed);
        // a fresh session has shown nothing yet — keep this adjacent to `frames` in all three
        // places (declaration, `new`, here): a reload that forgot the bit would silently suppress
        // the centred read-out for the rest of the app's life.
        self.seen_frame.store(false, Ordering::Relaxed);
        self.load_completed.store(false, Ordering::Relaxed);
        *self.media_id.lock().unwrap() = None;
        *self.source_info.lock().unwrap() = None;
        self.pts_shift.store(0, Ordering::Relaxed);
        self.disp_base.store(0, Ordering::Relaxed);
        self.seek_to_ns.store(-1, Ordering::Relaxed);
        self.seek_target_ns.store(-1, Ordering::Relaxed);
        self.seeking.store(false, Ordering::Relaxed);
        self.seek_display_ns.store(-1, Ordering::Relaxed);
        self.pending_audio_sid.store(-1, Ordering::Relaxed);
        self.pending_audio_idx.store(-1, Ordering::Relaxed);
        // NB: desired_audio_idx is NOT reset here — it persists across seeks/reloads so a
        // native audio-track choice survives seeking. It is reset on a new item (route).
        self.pending_retranscode.store(false, Ordering::Relaxed);
        self.pb_state.store(PlaybackState::Idle as u8, Ordering::Relaxed);
        self.demux_failed.store(false, Ordering::Relaxed);
        self.demux_io_failed.store(false, Ordering::Relaxed);
        self.auto_fallback_kbps.store(0, Ordering::Relaxed);
        self.auto_recover_kbps.store(0, Ordering::Relaxed);
        self.demux_no_video.store(false, Ordering::Relaxed);
        self.load_failed.store(false, Ordering::Relaxed);
        // NB: desired_sub_idx is NOT reset here — like desired_audio_idx it persists across
        // seeks/reloads so a reload-based seek keeps the chosen subtitle. It is reset on a new
        // item (player::reset_subtitle). The cue/bitmap STORES below are transient render state
        // and DO clear (the fresh demuxer re-populates them).
        self.sub_cues.lock().unwrap().clear();
        self.sub_bitmaps.lock().unwrap().clear();
        // Cleared with them, and for the same reason: they describe the FILE the last demuxer had
        // open. A reload-based seek re-opens the same part and re-publishes the same names, but a
        // session that ends with a transcode reload would otherwise leave the direct-play file's
        // names sitting under the server's own track list.
        *self.track_names.lock().unwrap() = TrackNames::new();
        self.file_size.store(0, Ordering::Relaxed);
        self.duration_ns.store(0, Ordering::Relaxed);
        self.hls_video_tail_ns.store(-1, Ordering::Relaxed);
        self.hls_audio_tail_ns.store(-1, Ordering::Relaxed);
        self.ended.store(false, Ordering::Relaxed);
        self.hs_ptr.store(std::ptr::null_mut(), Ordering::Release);
    }
}

/// UI-facing transport state. Main-thread-only in practice (plex_run + pump +
/// player_hud all run on M), but exposed as atomics so app.rs / player_hud.rs read
/// it with plain .load()/.store(). Replaces the #[no_mangle] transport globals.
pub(crate) struct Transport {
    pub started: AtomicBool,     // bf_started
    pub paused: AtomicBool,      // pl_paused
    pub resume_pend: AtomicBool, // resumePausePending
    pub hud_until: AtomicU32,    // pl_hud_until (SDL ticks)
    pub scrub_ns: AtomicI64,     // pl_scrub_ns (-1 = not scrubbing)
    pub seek_to_ns: AtomicI64,   // g_seek_to_ns (UI seek request, -1 = none)
    // Seek requests received since the pump last APPLIED one. seek_to_ns only ever holds the
    // newest target, so without this counter a coalesced burst is indistinguishable from a
    // single tap after the fact — the pump reports `coalesced=` from it (see pump.rs).
    pub seek_reqs: AtomicU32,
}
impl Transport {
    pub const fn new() -> Self {
        Transport {
            started: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            resume_pend: AtomicBool::new(false),
            hud_until: AtomicU32::new(0),
            scrub_ns: AtomicI64::new(-1),
            seek_to_ns: AtomicI64::new(-1),
            seek_reqs: AtomicU32::new(0),
        }
    }
    /// reset on stop (mirrors the transport tail of stop_bufferfeed).
    pub fn reset(&self) {
        self.started.store(false, Ordering::Relaxed);
        self.paused.store(false, Ordering::Relaxed);
        self.resume_pend.store(false, Ordering::Relaxed);
        self.scrub_ns.store(-1, Ordering::Relaxed);
        self.seek_to_ns.store(-1, Ordering::Relaxed);
        self.seek_reqs.store(0, Ordering::Relaxed);
    }
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
/// **`Bound` is the ACB bind, and the webOS 5 path SKIPS it** — `VP_EXPORTED` goes
/// `Playing -> Streaming` directly, because there is no bind sequence to be in the middle of.
/// So `stage >= Bound` means "bound or later" on webOS 4 and "is Streaming" on webOS 5; a new
/// test against it has to decide which of those it wants.
pub(crate) enum Stage {
    Loading = 0,
    Playing,
    Bound,
    Streaming,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Every `dg_*` mirror field must have a real writer.**
    ///
    /// The whole diagnostics mirror is write-here-read-there by construction, so a field that is
    /// declared, cleared in `reset_session`, and read by the panel compiles, runs, and is silently
    /// WRONG — there is no unused-field warning for it and no other test can see it. That is not
    /// hypothetical: `dg_place_rv`, `dg_placed_w`, `dg_placed_h` and `dg_splice` shipped exactly
    /// like that. `place_rv` stayed at its `i32::MIN` "never called" sentinel forever, so the
    /// read-out rendered `Placed: not placed` in DANGER bold on every webOS 5+ television — a
    /// fabricated fault, on the one firmware family we cannot test, aimed at the wrong layer.
    ///
    /// Source-level rather than behavioural because the writers live behind the ACB/Starfish seam,
    /// which does not exist on the host. `reset_session` is excluded deliberately: clearing a field
    /// is not writing it, and treating it as one is what let this through.
    #[test]
    fn every_diagnostics_mirror_field_has_a_writer() {
        const SHARED_SRC: &str = include_str!("shared.rs");
        // ff.rs is the THIRD writer of this block — the demux thread stamps the HTTP status and
        // the bytes it received. A writer file missing from this list reads exactly like a field
        // with no writer, so adding one here is part of adding one there.
        const WRITERS: [&str; 4] = [
            include_str!("pump.rs"),
            include_str!("engine.rs"),
            include_str!("mod.rs"),
            include_str!("../ff.rs"),
        ];

        let declared: Vec<&str> = SHARED_SRC
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pub dg_"))
            .filter_map(|l| l.split(':').next())
            .collect();
        assert!(declared.len() >= 10, "found only {} dg_ fields — the parse broke", declared.len());

        for f in declared {
            let written = WRITERS.iter().any(|src| {
                src.contains(&format!("dg_{f}.store(")) || src.contains(&format!("dg_{f}.fetch_"))
            });
            assert!(written, "dg_{f} is declared and read but NOTHING writes it — the panel would print its sentinel as fact");
        }
    }

    /// The one invariant that cannot be recovered from if it breaks, and breaks SILENTLY: a session
    /// boundary must forget this session's picture. `seen_frame` is set once, off-thread, and is
    /// never cleared anywhere else — so a `reset_session` that dropped it would leave it true for
    /// the rest of the process's life, and every subsequent cold start would show only the 12px
    /// transport mark instead of the centred read-out. Graded together with `frames`, because the
    /// two are only meaningful as a pair (see `Shared::seen_frame`).
    #[test]
    fn reset_session_forgets_this_sessions_picture() {
        let s = Shared::new();
        assert!(!s.seen_frame.load(Ordering::Relaxed), "a fresh session has shown nothing");
        s.seen_frame.store(true, Ordering::Relaxed);
        s.frames.store(9, Ordering::Relaxed);
        s.demux_io_failed.store(true, Ordering::Relaxed);
        s.auto_fallback_kbps.store(4_000, Ordering::Relaxed);
        s.auto_recover_kbps.store(60_000, Ordering::Relaxed);
        s.dg_abr_mode.store(2, Ordering::Relaxed);
        s.dg_abr_kbps.store(2_000, Ordering::Relaxed);
        s.dg_abr_net_kbps.store(4_000, Ordering::Relaxed);
        s.dg_abr_buffer_ms.store(2_800, Ordering::Relaxed);
        s.dg_abr_ratio_pm.store(500, Ordering::Relaxed);
        s.dg_abr_action.store(5, Ordering::Relaxed);
        s.dg_abr_target_kbps.store(4_000, Ordering::Relaxed);
        s.dg_abr_unsafe_deficit_ms.store(1_500, Ordering::Relaxed);
        s.reset_session();
        assert!(!s.seen_frame.load(Ordering::Relaxed), "a reload/stop blanks the plane — say so");
        assert_eq!(s.frames.load(Ordering::Relaxed), 0, "and the two must be reset together");
        assert!(!s.demux_io_failed.load(Ordering::Relaxed), "a new session must not inherit an I/O failure");
        assert_eq!(s.auto_fallback_kbps.load(Ordering::Relaxed), 0, "a new session must not inherit an Auto fallback");
        assert_eq!(s.auto_recover_kbps.load(Ordering::Relaxed), 0, "a new session must not inherit an Auto recovery");
        assert_eq!(s.dg_abr_mode.load(Ordering::Relaxed), 0);
        assert_eq!(s.dg_abr_kbps.load(Ordering::Relaxed), 0);
        assert_eq!(s.dg_abr_net_kbps.load(Ordering::Relaxed), -1);
        assert_eq!(s.dg_abr_buffer_ms.load(Ordering::Relaxed), -1);
        assert_eq!(s.dg_abr_ratio_pm.load(Ordering::Relaxed), -1);
        assert_eq!(s.dg_abr_action.load(Ordering::Relaxed), 0);
        assert_eq!(s.dg_abr_target_kbps.load(Ordering::Relaxed), 0);
        assert_eq!(s.dg_abr_unsafe_deficit_ms.load(Ordering::Relaxed), 0);
    }
}
