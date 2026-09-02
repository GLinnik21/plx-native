//! player::shared — the engine's cross-thread transport, callback and clock state. Every field
//! replaces a `volatile` global from playback.c and is an atomic or a Mutex (never a bare value).
//! Route ownership and route-changing intents live under the separate synchronized authority in
//! `route::PLAYER_CONTROL`. One long-lived `static SHARED` in mod.rs outlives every start/stop
//! cycle and is *reset*, never freed. Native callbacks additionally carry the exact `Load` epoch:
//! retirement drains an event already inside that epoch and rejects every later event, so stable
//! storage is not mistaken for permission to mutate the next playback.
use crate::stream::HttpStream;
use std::ffi::CString;
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicI64, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering,
};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

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
        Self {
            audio: Vec::new(),
            subs: Vec::new(),
        }
    }
    /// The name of the `i`-th subtitle stream in file order, or `""` — `i` is what
    /// `metadata::sub_render_ordinal` answers, and its `-1` (an external sidecar, which is not in
    /// the container at all) can be passed straight in.
    pub fn sub(&self, i: i32) -> &str {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.subs.get(i))
            .map(String::as_str)
            .unwrap_or("")
    }
    /// The same for audio — `metadata::audio_ordinal`'s answer.
    pub fn audio(&self, i: i32) -> &str {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.audio.get(i))
            .map(String::as_str)
            .unwrap_or("")
    }
}

/// Exact conservation certificate accumulated while the native media clock is held.
///
/// For completed acquisitions `(A_i, D_i)`, `debt` is `Σ(A_i-D_i)` and `runway` is
/// `max_i(P_{i-1}+A_i)`, where `P` is that running debt.  A recovery epoch may resume only after
/// at least one complete segment, once the debt has closed and the playable reserve covers the
/// largest prefix cost.  Unlike the ABR acquisition bag, this state starts at each actual pause
/// and is discarded at Play, so an old slow segment can never raise the next pause's floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HlsRecoveryEpoch {
    pub(crate) active: bool,
    pub(crate) completed: u64,
    pub(crate) debt_us: i64,
    pub(crate) runway_us: i64,
}

impl HlsRecoveryEpoch {
    const fn idle() -> Self {
        Self {
            active: false,
            completed: 0,
            debt_us: 0,
            runway_us: 0,
        }
    }

    fn begin(&mut self) {
        *self = Self {
            active: true,
            ..Self::idle()
        };
    }

    fn observe(&mut self, acquisition_us: u64, media: std::time::Duration) {
        if !self.active {
            return;
        }
        let acquisition_us = i64::try_from(acquisition_us).unwrap_or(i64::MAX);
        let media_us = i64::try_from(media.as_micros()).unwrap_or(i64::MAX);
        self.runway_us = self
            .runway_us
            .max(self.debt_us.saturating_add(acquisition_us));
        self.debt_us = self
            .debt_us
            .saturating_add(acquisition_us)
            .saturating_sub(media_us);
        self.completed = self.completed.saturating_add(1);
    }

    pub(crate) fn ready(self, playable_ns: i64) -> bool {
        self.active
            && self.completed > 0
            && self.debt_us <= 0
            && playable_ns.max(0) / 1_000 >= self.runway_us
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClockHold {
    /// A freshly loaded pipeline has not been started yet.
    Initial,
    /// An in-place seek paused the old timeline and is priming the new one.
    Seek,
    /// ABR stopped the clock at a measured starvation boundary.
    Rebuffer,
    /// The viewer explicitly paused a running clock.
    User,
    /// The viewer resumed a queued stream; feeding is open, but the physical clock remains held
    /// until both decoder lanes and the measured runway are ready.
    ResumePrime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsPrimeKind {
    Fresh,
    Rebuffer,
    Resume,
}

impl ClockHold {
    fn prime_kind(self) -> Option<HlsPrimeKind> {
        match self {
            Self::Initial | Self::Seek => Some(HlsPrimeKind::Fresh),
            Self::Rebuffer => Some(HlsPrimeKind::Rebuffer),
            Self::ResumePrime => Some(HlsPrimeKind::Resume),
            Self::User => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlsClockPhase {
    Running,
    PauseIssuing {
        token: u64,
        hold: ClockHold,
    },
    Held {
        hold: ClockHold,
    },
    PlayIssuing {
        token: u64,
        hold: ClockHold,
        resume_acb: bool,
    },
    Stopping,
}

#[derive(Clone, Copy, Debug)]
struct HlsClockState {
    phase: HlsClockPhase,
    next_token: u64,
    recovery: HlsRecoveryEpoch,
    /// Accepted user Pause ownership. This is intentionally inside the actuator mutex: the
    /// transport atomic is a hot-path mirror, not a second authority over whether Play may issue.
    user_held: bool,
    /// Monotone identity of accepted user clock transitions.  `Running` is not sufficient state:
    /// a complete Pause -> Resume may occur while a worker is blocked and return to the same
    /// shape.  Every automatic lease names the sequence it observed and the mutex validates that
    /// exact boundary before granting ownership.
    user_sequence: u64,
    /// User Resume was accepted while an internal/candidate hold still owned the native clock.
    /// The eventual Play must also resume ACB; losing that second half produces video/audio drift.
    resume_acb_pending: bool,
    /// Exactly one automatic actuator may own the boundary. Network/native I/O runs outside the
    /// mutex, but its lease token stays here through commit/rollback so another actuator cannot
    /// pass the same precondition concurrently.
    automatic: Option<HlsAutomaticOwner>,
}

impl HlsClockState {
    const fn new() -> Self {
        Self {
            phase: HlsClockPhase::Running,
            next_token: 0,
            recovery: HlsRecoveryEpoch::idle(),
            user_held: false,
            user_sequence: 0,
            resume_acb_pending: false,
            automatic: None,
        }
    }

    fn token(&mut self) -> u64 {
        self.next_token = self
            .next_token
            .checked_add(1)
            .expect("HLS clock transition token space exhausted");
        self.next_token
    }

    fn advance_user_sequence(&mut self) {
        self.user_sequence = self
            .user_sequence
            .checked_add(1)
            .expect("HLS user clock sequence exhausted");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsClockFenceError {
    Stopping,
    Overlap,
    Exhausted,
    Superseded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsAutomaticTransition {
    QualityUp,
    QualityDown,
    Original,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlsAutomaticPhase {
    Working,
    CommitAuthorized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlsAutomaticOwner {
    token: u64,
    kind: HlsAutomaticTransition,
    phase: HlsAutomaticPhase,
    user_sequence: u64,
}

/// Ownership of the callback function installed in one native Starfish `Load`.
///
/// [`Shared`] survives reloads, while the library may deliver an event on its own thread after
/// teardown has started. Each Starfish object address is now session-unique, but Rust still needs
/// a generation check and drain before resetting this process-long state. A check without this mutex
/// still has a check/use race against [`Shared::reset_session`]: a callback could validate the
/// old generation, get descheduled, and then write into the freshly reset next session. Holding
/// this lock for the complete callback makes retirement a drain barrier as well as a token check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeSessionPhase {
    Idle,
    Active {
        epoch: u32,
        /// Type-0 presentation callbacks are admitted only for the current media timeline. An
        /// in-place seek closes this while old decoded frames are flushed, then the first real
        /// post-seek keyframe reopens it.
        presentation_gate: NativePresentationGate,
    },
    /// Firmware's synchronous UNLOADCOMPLETED callback was observed. This is independent lifecycle
    /// evidence, not a producer barrier: native callback admission is closed and drained in the C
    /// interposer before the main thread retires this phase and considers D1.
    Unloaded {
        epoch: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativePresentationGate {
    Armed,
    Disarmed,
    /// The first post-discontinuity AU is inside `sf_feed`. Firmware can publish the buffer to a
    /// GStreamer worker before Feed returns, so retain the newest position callback until the
    /// caller learns whether that exact AU was accepted.
    PendingArm {
        latched: Option<i64>,
    },
}

#[derive(Clone, Copy, Debug)]
struct NativeSessionState {
    next_epoch: u32,
    phase: NativeSessionPhase,
}

impl NativeSessionState {
    const fn new() -> Self {
        Self {
            next_epoch: 0,
            phase: NativeSessionPhase::Idle,
        }
    }

    fn next(&mut self) -> u32 {
        self.next_epoch = self.next_epoch.wrapping_add(1);
        if self.next_epoch == 0 {
            self.next_epoch = 1;
        }
        self.next_epoch
    }
}

/// What provenance/lifecycle effect one native callback carries. Keeping this typed here makes
/// `UNLOADCOMPLETED` a reducer transition rather than a magic event number interpreted by the
/// teardown code after the fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeEventClass {
    Presentation,
    UnloadCompleted,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsPauseCompletion {
    Accepted,
    Refused,
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsSeekPause {
    Issue(u64),
    AlreadyHeld,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsUserPause {
    Issue(u64),
    AlreadyHeld,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsUserResume {
    Issue(u64),
    Deferred,
    Prime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsPlayCompletion {
    Accepted { resume_acb: bool },
    Refused,
    Stale,
}

#[derive(Clone, Copy, Debug)]
struct AbrSeed {
    estimate: crate::abr::CapacityEstimate,
    observed_at: Instant,
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
    /// Synchronized lifecycle/provenance for callbacks from the current native `Load`.
    native_session: Mutex<NativeSessionState>,
    // library callback thread (K) -> main (M)
    pub playpos_ns: AtomicI64, // g_playpos_ns
    // the presented frame's fed PTS (0-based, raw `num` from the type=0 callback). The feed
    // loop throttles on max_fed_pts - pres_fed so it stays ~MAX_FEED_AHEAD ahead of the
    // decoder (feeding further overfills the 4K HEVC DPB/CPB and stalls the sink).
    pub pres_fed: AtomicI64,
    pub frames: AtomicI32, // bf_frames
    /// **Has this SESSION ever put a picture on the panel?** Set by the frame-presented callback
    /// beside `frames`, cleared ONLY by [`Shared::reset_session`] — so it survives a seek, which
    /// `frames` deliberately does not: `pump` zeroes `frames` as *part of applying* a seek, which
    /// makes "we have never shown a frame" and "we just seeked" indistinguishable through it. The
    /// HUD's one rule for which surface owns the "pipeline is working" read-out keys on this bit;
    /// see `ui::player_hud::busy_surface`. Monotone within a session (false→true only), which is
    /// what lets two readers in one frame sample it independently without disagreeing.
    pub seen_frame: AtomicBool,
    pub load_completed: AtomicBool,          // bf_loaded signal
    pub media_id: Mutex<Option<CString>>,    // bf_mediaId (captured once)
    pub source_info: Mutex<Option<Vec<u8>>>, // sourceInfoRaw, VERBATIM incl NUL

    // main/pump (M) -> library callback thread (K)
    pub pts_shift: AtomicI64, // g_pts_shift
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
    // NATIVE audio-track switch (direct-play, no transcode): the 0-based audio stream index
    // to feed. `desired` (-1 = av_find_best_stream) is read by the demuxer to pick the Nth audio
    // stream and PERSISTS across seeks/reloads (reset only on a new item, not in reset_session).
    // The synchronized route controller owns whether a reload is pending; keeping a second atomic
    // mailbox here used to let reset_session erase the request halfway through that reload.
    pub desired_audio_idx: AtomicI32,
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
    /// Sparse, typed playback transitions retained only for an opted-in handled-error report.
    /// Unlike the engine fields around it this spans reloads and seeks; `report::requested` owns
    /// the attempt boundary and clears it for a genuinely new Play.
    pub playback_trace: Mutex<super::report::PlaybackTrace>,

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
    pub file_size: AtomicI64, // g_file_size
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
    /// Coherent `{w,h}` publication for actuator consumers. The individual fields above remain
    /// diagnostic mirrors; reading them independently can manufacture a raster no stream owned.
    video_raster: AtomicU64,
    /// Decoder-reported source frame rate in thousandths of a frame per second. 0 means the
    /// sourceInfo callback has not supplied one; unlike `FRAMEREADY`, this is stream metadata and
    /// therefore does not mistake the firmware's ~5 Hz position tick for video cadence.
    pub video_fps_milli: AtomicI64,
    pub duration_ns: AtomicI64, // was g_mkv.duration_ns (published)
    /// Latest normalized timestamps successfully enqueued for each elementary stream. HLS writes
    /// its segment-normalized zero-based timeline; progressive Original writes absolute movie
    /// PTS. Their consumers apply the matching display-base rule. These are content-time facts
    /// (not queue byte counts); `-1` means the lane has not produced an AU in this session/seek.
    pub hls_video_tail_ns: AtomicI64,
    pub hls_audio_tail_ns: AtomicI64,
    /// Whether the active HLS segment declared an audio stream that this session feeds. The prime
    /// gate must not reinterpret a temporarily empty audio lane as a genuinely video-only file.
    pub hls_audio_expected: AtomicBool,
    /// Retrospective replay reserve used at a fresh HLS Load/seek boundary and when a viewer
    /// resumes a queue-backed stream. Automatic runtime rebuffer uses the pause-local
    /// `hls_recovery` certificate below; carrying this history into every controller-created hold
    /// was the source of repeated freeze/catch-up on an otherwise stable stream. Zero leaves the
    /// decoder's ordinary prime threshold in charge.
    pub hls_prime_runway_ms: AtomicI64,
    /// Demux -> main request to stop the internal media clock before a projected fetch overrun
    /// turns into freeze/catch-up. This is not the user's pause state; feeding continues.
    pub hls_rebuffer_requested: AtomicBool,
    /// Video-tail generation captured when `hls_rebuffer_requested` was raised. If a completed
    /// segment advances the tail before the main thread consumes the request, the condition it
    /// described has already cleared and the request is stale.
    pub hls_rebuffer_request_tail_ns: AtomicI64,
    /// Derived hot-path mirror of the synchronized clock state below: Starfish is internally
    /// paused and waiting for measured runway. Production transitions write it only while holding
    /// `hls_clock`; workers which merely need a deadline predicate can read the atomic cheaply.
    pub hls_rebuffering: AtomicBool,
    /// Monotone proof that an internal hold happened, even if Pause→Play completes entirely while
    /// a network operation is blocked and both of its boolean snapshots therefore read false.
    pub hls_internal_hold_epoch: AtomicU64,
    /// One synchronized authority for native Pause/Play issuance, the pause-local conservation
    /// certificate and candidate/trial exclusion. FFI is called outside this mutex through a
    /// reserve/complete token protocol; `Condvar` lets the demux worker linearize after a very
    /// short in-flight Play instead of treating that ordinary ordering as a fatal overlap.
    hls_clock: Mutex<HlsClockState>,
    hls_clock_changed: Condvar,
    /// Retrospective reserve retained by an exploratory candidate's own deadline. Its non-negative
    /// presence also prevents an already-held clock from restarting mid-transaction; it is not a
    /// continuously armed pause threshold. `-1` means no trial.
    pub hls_trial_reserve_ms: AtomicI64,
    /// Seqlock generation for candidate media/ownership publication. Even values are stable; a
    /// worker CASes even->odd before its first candidate AU, then either publishes new ownership or
    /// realigns the old cursor. It publishes the matching recovery state before advancing
    /// odd->next even. The main-thread prime gate accepts only an unchanged even value around its
    /// whole tails+epoch snapshot, preventing false->true->false ABA from mixing candidate media
    /// with the previous rung's certificate. This is not a timer or threshold.
    pub hls_candidate_generation: AtomicU64,
    // set once the pipeline has drained to true end-of-stream (EOS pushed AND the last fed frame
    // has been presented). app.rs polls player::ended() to tear the player down at the credits.
    pub ended: AtomicBool,

    // close-to-interrupt handle: raw ptr to the Engine-owned HttpStream box, so
    // the pump/teardown can close(fd) to unblock a blocked recv. The box outlives
    // the worker threads (Engine drops after join), so the ptr stays valid.
    pub hs_ptr: AtomicPtr<HttpStream>,

    // ---- diagnostics mirror (`ui::stats`) -------------------------------------------------
    // Values the render path (and the opted-in handled-error snapshot) need that live on the
    // Engine, republished from the pump.
    // The render path may not call `engine(&MainThread)` — that hands out a `&'static mut` to a
    // `static mut`, and a second live borrow is instant UB — so it reads these instead.
    //
    // STRICTLY ONE-WAY: written by the pump and seam, read only for observation. Nothing in the
    // playback state machine may ever branch on one, or a diagnostic becomes load-bearing.
    /// `Stage` as u8 — where the bind/play sequence has got to. Unlike the expensive queue
    /// mirrors below, this scalar is always current enough for a terminal error report even when
    /// Stats for Nerds is closed.
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
    /// The current HLS master declaration and the last completed segment's measured media rate.
    /// Both are observations of what PMS actually emitted; `dg_abr_kbps` is the requested ceiling
    /// and must never be presented as either of these.
    pub dg_abr_declared_kbps: AtomicI64,
    pub dg_abr_media_kbps: AtomicI64,
    /// Latest measured body throughput and normalized content reserve. `-1` means no complete
    /// measurement exists yet. Production ratio is total segment acquisition / media duration in
    /// per-mille; it is meaningful for HLS only.
    pub dg_abr_net_kbps: AtomicI64,
    pub dg_abr_buffer_ms: AtomicI64,
    pub dg_abr_ratio_pm: AtomicI64,
    /// Current controller action plus its candidate rung. A discrete transaction remains visible
    /// until the next completed segment publishes its steady decision; keeping it beyond that made
    /// `Action` describe an old commit beside `Reason` from the current model state.
    pub dg_abr_action: AtomicU8,
    pub dg_abr_target_kbps: AtomicI64,
    /// Last Original source failure, as `player::ABR_FAILURE_*` plus its HTTP status when one was
    /// observed. These are playback-scoped rather than engine-scoped: HLS→Original failure tears
    /// the Engine down while restoring HLS, and clearing them in `reset_session` would make the
    /// successful rollback erase its own cause before the diagnostics panel could photograph it.
    pub abr_failure_kind: AtomicU8,
    pub abr_failure_status: AtomicI32,
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
    /// **The live link estimate, carried ACROSS A SEEK** (I8) — and deliberately not a `dg_`
    /// field, because this coherent posterior decides something. An HLS seek tears the engine
    /// down and builds a fresh `Controller`, so before this the only survivor was
    /// `session().auto_prior_kbps`, whose
    /// writer on the fallback path is the rate measured at the moment Original FAILED. Every seek
    /// therefore re-seeded from the worst rate the playback had ever measured, at maximum
    /// uncertainty with one sample, and the ladder re-ramped for five to ten segments: ten to
    /// twenty seconds of visibly softer picture after every skip.
    ///
    /// One coherent estimate plus the wall instant it was last observed. Cleared by
    /// [`Shared::clear_abr_seed`] and **deliberately NOT by [`Shared::reset_session`]**. The time
    /// is load-bearing: a pause, app background or reload gap with no segment observations must
    /// age the carried evidence before a new controller may use it.
    abr_seed: Mutex<Option<AbrSeed>>,
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
            native_session: Mutex::new(NativeSessionState::new()),
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
            dg_abr_declared_kbps: AtomicI64::new(-1),
            dg_abr_media_kbps: AtomicI64::new(-1),
            dg_abr_net_kbps: AtomicI64::new(-1),
            dg_abr_buffer_ms: AtomicI64::new(-1),
            dg_abr_ratio_pm: AtomicI64::new(-1),
            dg_abr_action: AtomicU8::new(0),
            dg_abr_target_kbps: AtomicI64::new(0),
            abr_failure_kind: AtomicU8::new(0),
            abr_failure_status: AtomicI32::new(0),
            dg_abr_unsafe_deficit_ms: AtomicI64::new(0),
            abr_seed: Mutex::new(None),
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
            desired_audio_idx: AtomicI32::new(-1),
            pb_state: AtomicU8::new(PlaybackState::Idle as u8),
            demux_failed: AtomicBool::new(false),
            demux_io_failed: AtomicBool::new(false),
            demux_no_video: AtomicBool::new(false),
            load_failed: AtomicBool::new(false),
            playback_trace: Mutex::new(super::report::PlaybackTrace::new()),
            desired_sub_idx: AtomicI32::new(-1),
            track_names: Mutex::new(TrackNames::new()),
            sub_cues: Mutex::new(Vec::new()),
            sub_bitmaps: Mutex::new(Vec::new()),
            file_size: AtomicI64::new(0),
            video_w: AtomicI32::new(0),
            video_h: AtomicI32::new(0),
            video_raster: AtomicU64::new(0),
            video_fps_milli: AtomicI64::new(0),
            duration_ns: AtomicI64::new(0),
            hls_video_tail_ns: AtomicI64::new(-1),
            hls_audio_tail_ns: AtomicI64::new(-1),
            hls_audio_expected: AtomicBool::new(false),
            hls_prime_runway_ms: AtomicI64::new(0),
            hls_rebuffer_requested: AtomicBool::new(false),
            hls_rebuffer_request_tail_ns: AtomicI64::new(-1),
            hls_rebuffering: AtomicBool::new(false),
            hls_internal_hold_epoch: AtomicU64::new(0),
            hls_clock: Mutex::new(HlsClockState::new()),
            hls_clock_changed: Condvar::new(),
            hls_trial_reserve_ms: AtomicI64::new(-1),
            hls_candidate_generation: AtomicU64::new(0),
            ended: AtomicBool::new(false),
            hs_ptr: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    /// Enter the only state in which a Starfish `Load` may publish callbacks.
    ///
    /// `None` rejects an overlapping native session instead of silently retagging two live
    /// objects with one process-global owner.
    pub(crate) fn begin_native_session(&self) -> Option<u32> {
        let mut state = self
            .native_session
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if state.phase != NativeSessionPhase::Idle {
            return None;
        }
        let epoch = state.next();
        state.phase = NativeSessionPhase::Active {
            epoch,
            presentation_gate: NativePresentationGate::Armed,
        };
        Some(epoch)
    }

    /// Retire one exact native session and wait for every callback which already entered it.
    ///
    /// Ready-object teardown calls this after `Unload` has returned and the C interposer gate has
    /// closed and drained. Startup failure paths also retire the Rust epoch here: either no native
    /// object was constructed, or construction succeeded but arming the C gate failed, cleared
    /// dispatch readiness and retained the object in process-long quarantine. That latter path
    /// admitted no native callbacks and does not claim an Unload or native-gate drain. The Active
    /// arm also keeps this primitive independently testable without fabricating firmware callbacks.
    pub(crate) fn retire_native_session(&self, epoch: u32) -> bool {
        let mut state = self
            .native_session
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let owns = matches!(
            state.phase,
            NativeSessionPhase::Active { epoch: active, .. }
                | NativeSessionPhase::Unloaded { epoch: active }
                if epoch != 0 && active == epoch
        );
        if !owns {
            return false;
        }
        state.phase = NativeSessionPhase::Idle;
        true
    }

    /// Whether this exact object crossed firmware's synchronous unload-complete callback path.
    /// This is one mandatory lifecycle assertion, separate from the native interposer's admission
    /// and in-flight proof. A missing/renumbered callback quarantines the object rather than D1.
    pub(crate) fn native_unload_completed(&self, epoch: u32) -> bool {
        self.native_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase
            == NativeSessionPhase::Unloaded { epoch }
    }

    /// Fence pre-seek presentations while retaining non-frame Load/error callbacks. Locking is a
    /// drain barrier for a type-0 callback already inside the old timeline.
    pub(crate) fn begin_native_media_discontinuity(&self, epoch: u32) -> bool {
        let mut state = self
            .native_session
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match &mut state.phase {
            NativeSessionPhase::Active {
                epoch: active,
                presentation_gate,
            } if epoch != 0 && *active == epoch => {
                *presentation_gate = NativePresentationGate::Disarmed;
                true
            }
            _ => false,
        }
    }

    /// Admit presentation callbacks again after the new timeline's first keyframe is identified.
    #[cfg(test)]
    pub(crate) fn arm_native_presentations(&self, epoch: u32) -> bool {
        let mut state = self
            .native_session
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match &mut state.phase {
            NativeSessionPhase::Active {
                epoch: active,
                presentation_gate,
            } if epoch != 0 && *active == epoch => {
                *presentation_gate = NativePresentationGate::Armed;
                true
            }
            _ => false,
        }
    }

    /// Start a two-phase callback admission around the exact post-seek keyframe Feed. No lock is
    /// held across FFI: firmware may re-enter synchronously on another release even though the
    /// audited 4.10.2 path currently dispatches through a GStreamer worker.
    pub(crate) fn begin_native_presentation_probe(&self, epoch: u32) -> bool {
        let mut state = self
            .native_session
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match &mut state.phase {
            NativeSessionPhase::Active {
                epoch: active,
                presentation_gate,
            } if epoch != 0 && *active == epoch => {
                *presentation_gate = NativePresentationGate::PendingArm { latched: None };
                true
            }
            _ => false,
        }
    }

    /// Commit an accepted Feed and replay at most the newest presentation which raced its return.
    pub(crate) fn commit_native_presentation_probe(
        &self,
        epoch: u32,
        replay: impl FnOnce(i64),
    ) -> bool {
        let mut state = self
            .native_session
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let NativeSessionPhase::Active {
            epoch: active,
            presentation_gate,
        } = &mut state.phase
        else {
            return false;
        };
        if epoch == 0 || *active != epoch {
            return false;
        }
        let NativePresentationGate::PendingArm { latched } = *presentation_gate else {
            return false;
        };
        *presentation_gate = NativePresentationGate::Armed;
        if let Some(num) = latched {
            replay(num);
        }
        true
    }

    /// Discard callbacks observed while a BufferFull/error Feed was outstanding and keep the old
    /// timeline fenced. The retained AU will open a fresh probe on its next attempt.
    pub(crate) fn reject_native_presentation_probe(&self, epoch: u32) -> bool {
        let mut state = self
            .native_session
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match &mut state.phase {
            NativeSessionPhase::Active {
                epoch: active,
                presentation_gate,
            } if epoch != 0
                && *active == epoch
                && matches!(presentation_gate, NativePresentationGate::PendingArm { .. }) =>
            {
                *presentation_gate = NativePresentationGate::Disarmed;
                true
            }
            _ => false,
        }
    }

    /// Run an event only while its firmware-provided callback context owns this session.
    pub(crate) fn with_native_session<R>(
        &self,
        epoch: u32,
        class: NativeEventClass,
        num: i64,
        event: impl FnOnce() -> R,
    ) -> Option<R> {
        let mut state = self
            .native_session
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let NativeSessionPhase::Active {
            epoch: active,
            presentation_gate,
        } = &mut state.phase
        else {
            return None;
        };
        if epoch == 0 || *active != epoch {
            return None;
        }
        if class == NativeEventClass::Presentation {
            match presentation_gate {
                NativePresentationGate::Armed => {}
                NativePresentationGate::Disarmed => return None,
                NativePresentationGate::PendingArm { latched } => {
                    *latched = Some(num);
                    return None;
                }
            }
        }
        let result = event();
        if class == NativeEventClass::UnloadCompleted {
            state.phase = NativeSessionPhase::Unloaded { epoch };
        }
        Some(result)
    }
    /// reset per-file state on stop (mirrors the tail of stop_bufferfeed).
    /// **Drop the carried link estimate.** Called from `engine::teardown` on a real STOP only —
    /// `reset_session` runs on a reload too, and a reload is the same item on the same link at a
    /// new position, which is exactly the case I8 exists to preserve. Splitting it out rather than
    /// making `reset_session` caller-conditional keeps "clear everything about this session" a
    /// method with no exceptions in it.
    pub fn clear_abr_seed(&self) {
        *self.abr_seed.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub(crate) fn publish_abr_seed(&self, estimate: crate::abr::CapacityEstimate) {
        self.publish_abr_seed_at(estimate, Instant::now());
    }

    fn publish_abr_seed_at(&self, estimate: crate::abr::CapacityEstimate, observed_at: Instant) {
        let mut seed = self.abr_seed.lock().unwrap_or_else(|e| e.into_inner());
        *seed = (estimate.samples > 0).then_some(AbrSeed {
            estimate,
            observed_at,
        });
    }

    /// Delivery evidence carried across an engine reload, aged by every wall interval for which
    /// no segment could refresh it. This includes accepted Pause, app background and teardown —
    /// all three are the same epistemic fact: the old link observation was not repeated.
    pub(crate) fn abr_seed(&self) -> Option<crate::abr::CapacityEstimate> {
        let seed = (*self.abr_seed.lock().unwrap_or_else(|e| e.into_inner()))?;
        let mut estimate = seed.estimate;
        let elapsed_ms = u64::try_from(seed.observed_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        estimate.age_ms(elapsed_ms, &crate::abr::AbrPolicy::measured());
        Some(estimate)
    }

    /// Clear a playback's sticky Original failure. Kept separate from [`Shared::reset_session`]
    /// for the same lifetime reason as the ABR seed: an engine reload is not a new playback.
    pub fn clear_abr_failure(&self) {
        self.abr_failure_kind.store(0, Ordering::Relaxed);
        self.abr_failure_status.store(0, Ordering::Relaxed);
    }

    pub(crate) fn publish_video_raster(&self, width: i32, height: i32) {
        let packed = (u64::from(width as u32) << 32) | u64::from(height as u32);
        self.video_w.store(width, Ordering::Relaxed);
        self.video_h.store(height, Ordering::Relaxed);
        self.video_raster.store(packed, Ordering::Release);
    }

    pub(crate) fn video_raster(&self) -> (i32, i32) {
        let packed = self.video_raster.load(Ordering::Acquire);
        ((packed >> 32) as u32 as i32, packed as u32 as i32)
    }

    /// **NB: this does NOT clear the ABR seed OR Original failure** — see
    /// [`Shared::clear_abr_seed`] and [`Shared::clear_abr_failure`]. Everything else here describes
    /// one engine's session and must not outlive it.
    pub fn reset_session(&self) {
        // This lock is held through the whole reset. A callback that entered before retirement
        // finishes first; one arriving afterwards sees no owner. It can therefore never validate
        // session A and then publish into the zeroed/reused storage of session B.
        let mut native = self
            .native_session
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        native.phase = NativeSessionPhase::Idle;
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
        self.dg_abr_declared_kbps.store(-1, Ordering::Relaxed);
        self.dg_abr_media_kbps.store(-1, Ordering::Relaxed);
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
        // NB: desired_audio_idx is NOT reset here — it persists across seeks/reloads so a
        // native audio-track choice survives seeking. It is reset on a new item (route).
        self.pb_state
            .store(PlaybackState::Idle as u8, Ordering::Relaxed);
        self.demux_failed.store(false, Ordering::Relaxed);
        self.demux_io_failed.store(false, Ordering::Relaxed);
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
        self.video_w.store(0, Ordering::Relaxed);
        self.video_h.store(0, Ordering::Relaxed);
        self.video_raster.store(0, Ordering::Release);
        self.video_fps_milli.store(0, Ordering::Relaxed);
        self.duration_ns.store(0, Ordering::Relaxed);
        self.hls_video_tail_ns.store(-1, Ordering::Relaxed);
        self.hls_audio_tail_ns.store(-1, Ordering::Relaxed);
        self.hls_audio_expected.store(false, Ordering::Relaxed);
        self.hls_prime_runway_ms.store(0, Ordering::Relaxed);
        self.hls_rebuffer_requested.store(false, Ordering::Relaxed);
        self.hls_rebuffer_request_tail_ns
            .store(-1, Ordering::Relaxed);
        self.reset_hls_clock();
        self.ended.store(false, Ordering::Relaxed);
        self.hs_ptr.store(std::ptr::null_mut(), Ordering::Release);
    }

    /// Start (or restart after a rung commit) the proof for the media that will release this hold.
    #[cfg(all(test, feature = "hostsim"))]
    pub(crate) fn begin_hls_recovery(&self) {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        clock.recovery.begin();
        // Compatibility for the existing direct state-machine tests: production enters Held via
        // `complete_hls_rebuffer_pause`, while those tests construct the same boundary explicitly.
        if matches!(
            clock.phase,
            HlsClockPhase::Running
                | HlsClockPhase::Held {
                    hold: ClockHold::Initial,
                }
        ) {
            clock.phase = HlsClockPhase::Held {
                hold: ClockHold::Rebuffer,
            };
        }
    }

    /// Credit exactly one fully downloaded, validated and enqueued segment from the active rung.
    pub(crate) fn observe_hls_recovery(&self, acquisition_us: u64, media: std::time::Duration) {
        self.hls_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recovery
            .observe(acquisition_us, media);
    }

    pub(crate) fn hls_recovery(&self) -> HlsRecoveryEpoch {
        self.hls_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recovery
    }

    #[cfg(all(test, feature = "hostsim"))]
    pub(crate) fn finish_hls_recovery(&self) {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        clock.recovery = HlsRecoveryEpoch::idle();
        // The older focused tests construct an internal hold by publishing the mirrors directly.
        // Restore the synchronized half as well, otherwise a refused-Play case leaves `Held` behind
        // and whichever serial test happens to run next cannot issue its initial Play.
        if !self.hls_rebuffering.load(Ordering::Acquire)
            && !matches!(
                clock.phase,
                HlsClockPhase::PlayIssuing { .. } | HlsClockPhase::Stopping
            )
        {
            clock.phase = HlsClockPhase::Running;
            self.hls_clock_changed.notify_all();
        }
    }

    #[cfg(test)]
    pub(crate) fn reset_hls_clock_for_test(&self) {
        self.reset_hls_clock();
    }

    /// Put a newly constructed native pipeline into its real pre-Play state. This happens before
    /// either worker is spawned, so neither an ABR trial nor a user command can mistake "loaded but
    /// not started" for a running clock. `user_held` is the durable viewer intent carried across an
    /// engine reload; `seek_preroll` means that intent is temporarily allowed to decode one frame.
    pub(crate) fn arm_initial_clock_hold(&self, user_held: bool, seek_preroll: bool) -> bool {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase != HlsClockPhase::Running
            || self.hls_trial_reserve_ms.load(Ordering::Acquire) >= 0
            || self.hls_candidate_generation.load(Ordering::Acquire) & 1 != 0
        {
            return false;
        }
        clock.phase = HlsClockPhase::Held {
            hold: ClockHold::Initial,
        };
        clock.user_held = user_held && !seek_preroll;
        clock.resume_acb_pending = user_held && seek_preroll;
        true
    }

    /// A start that armed the initial hold but failed before installing an Engine owns no native
    /// clock. Invalidate every candidate/trial fence and return to the no-session state so retrying
    /// the same item cannot inherit `Held` forever.
    pub(crate) fn cancel_clock_session_start(&self) {
        self.reset_hls_clock();
    }

    #[cfg(all(test, feature = "hostsim"))]
    pub(crate) fn ensure_initial_clock_hold_for_test(&self) {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase == HlsClockPhase::Running {
            clock.phase = HlsClockPhase::Held {
                hold: ClockHold::Initial,
            };
        }
    }

    /// Reserve an internal Pause and start accepting recovery segments BEFORE the FFI call. A
    /// segment committed in the call window is retained if Pause succeeds and discarded if it is
    /// refused, so the first useful recovery object can no longer disappear between two flags.
    pub(crate) fn prepare_hls_rebuffer_pause(&self) -> Option<u64> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        // Trial and Pause spend the same reserve. Whichever reserves this mutex first owns it; the
        // loser retries after the transaction settles instead of creating two physical clocks.
        if clock.phase != HlsClockPhase::Running
            || self.hls_trial_reserve_ms.load(Ordering::Acquire) >= 0
        {
            return None;
        }
        let token = clock.token();
        clock.recovery.begin();
        clock.phase = HlsClockPhase::PauseIssuing {
            token,
            hold: ClockHold::Rebuffer,
        };
        Some(token)
    }

    pub(crate) fn complete_hls_rebuffer_pause(
        &self,
        token: u64,
        accepted: bool,
    ) -> HlsPauseCompletion {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase
            != (HlsClockPhase::PauseIssuing {
                token,
                hold: ClockHold::Rebuffer,
            })
        {
            return HlsPauseCompletion::Stale;
        }
        if accepted {
            clock.phase = HlsClockPhase::Held {
                hold: ClockHold::Rebuffer,
            };
            self.hls_rebuffering.store(true, Ordering::Release);
            self.hls_internal_hold_epoch.fetch_add(1, Ordering::AcqRel);
        } else {
            clock.phase = HlsClockPhase::Running;
            clock.recovery = HlsRecoveryEpoch::idle();
        }
        self.hls_clock_changed.notify_all();
        if accepted {
            HlsPauseCompletion::Accepted
        } else {
            HlsPauseCompletion::Refused
        }
    }

    /// Reserve a viewer Pause through the same actuator protocol as automatic rebuffering. If the
    /// internal controller already owns a physical hold, only the orthogonal user hold is added and
    /// no redundant native Pause is issued.
    pub(crate) fn prepare_hls_user_pause(&self) -> Option<HlsUserPause> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase == HlsClockPhase::Stopping {
            return None;
        }
        if clock.user_held {
            return Some(HlsUserPause::AlreadyHeld);
        }
        if matches!(clock.phase, HlsClockPhase::Held { .. }) {
            clock.user_held = true;
            clock.resume_acb_pending = false;
            clock.advance_user_sequence();
            return Some(HlsUserPause::AlreadyHeld);
        }
        if clock.phase != HlsClockPhase::Running {
            return None;
        }
        let token = clock.token();
        clock.phase = HlsClockPhase::PauseIssuing {
            token,
            hold: ClockHold::User,
        };
        Some(HlsUserPause::Issue(token))
    }

    pub(crate) fn complete_hls_user_pause(&self, token: u64, accepted: bool) -> HlsPauseCompletion {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase
            != (HlsClockPhase::PauseIssuing {
                token,
                hold: ClockHold::User,
            })
        {
            return HlsPauseCompletion::Stale;
        }
        if accepted {
            clock.phase = HlsClockPhase::Held {
                hold: ClockHold::User,
            };
            clock.user_held = true;
            clock.resume_acb_pending = false;
            clock.advance_user_sequence();
        } else {
            clock.phase = HlsClockPhase::Running;
        }
        self.hls_clock_changed.notify_all();
        if accepted {
            HlsPauseCompletion::Accepted
        } else {
            HlsPauseCompletion::Refused
        }
    }

    /// Reserve the native Pause that makes an in-place seek safe. A seek requested from an
    /// existing user hold reuses that physical Pause, but temporarily transfers ownership to the
    /// seek prime so exactly one landed frame may be decoded before the user hold is restored.
    pub(crate) fn prepare_seek_pause(&self) -> Option<HlsSeekPause> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        match clock.phase {
            HlsClockPhase::Running => {
                let token = clock.token();
                clock.phase = HlsClockPhase::PauseIssuing {
                    token,
                    hold: ClockHold::Seek,
                };
                Some(HlsSeekPause::Issue(token))
            }
            HlsClockPhase::Held {
                hold:
                    ClockHold::User | ClockHold::Initial | ClockHold::Seek | ClockHold::ResumePrime,
            } => {
                if clock.user_held {
                    clock.user_held = false;
                    clock.resume_acb_pending = true;
                }
                clock.phase = HlsClockPhase::Held {
                    hold: ClockHold::Seek,
                };
                Some(HlsSeekPause::AlreadyHeld)
            }
            HlsClockPhase::Held {
                hold: ClockHold::Rebuffer,
            }
            | HlsClockPhase::PauseIssuing { .. }
            | HlsClockPhase::PlayIssuing { .. }
            | HlsClockPhase::Stopping => None,
        }
    }

    pub(crate) fn complete_seek_pause(&self, token: u64, accepted: bool) -> HlsPauseCompletion {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase
            != (HlsClockPhase::PauseIssuing {
                token,
                hold: ClockHold::Seek,
            })
        {
            return HlsPauseCompletion::Stale;
        }
        clock.phase = if accepted {
            HlsClockPhase::Held {
                hold: ClockHold::Seek,
            }
        } else {
            HlsClockPhase::Running
        };
        self.hls_clock_changed.notify_all();
        if accepted {
            HlsPauseCompletion::Accepted
        } else {
            HlsPauseCompletion::Refused
        }
    }

    /// Accept the viewer's Resume intent. Queued streams transfer an ordinary user hold to an
    /// explicit resume-prime hold: the caller opens feeding, while `try_prime` remains the sole
    /// owner of the eventual Play + ACB Resume. A candidate publication, seek or internal recovery
    /// already holding the clock keeps its own certificate and only records the deferred ACB half.
    pub(crate) fn prepare_hls_user_resume(&self, queued_stream: bool) -> Option<HlsUserResume> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase == HlsClockPhase::Stopping || !clock.user_held {
            return None;
        }
        let HlsClockPhase::Held { hold } = clock.phase else {
            return None;
        };
        if hold != ClockHold::User {
            clock.user_held = false;
            clock.resume_acb_pending = true;
            clock.advance_user_sequence();
            return Some(HlsUserResume::Deferred);
        }
        if queued_stream {
            clock.phase = HlsClockPhase::Held {
                hold: ClockHold::ResumePrime,
            };
            clock.user_held = false;
            clock.resume_acb_pending = true;
            clock.advance_user_sequence();
            return Some(HlsUserResume::Prime);
        }
        let token = clock.token();
        clock.phase = HlsClockPhase::PlayIssuing {
            token,
            hold,
            resume_acb: true,
        };
        Some(HlsUserResume::Issue(token))
    }

    /// Snapshot which media certificate currently owns a held clock. The mutex phase is the
    /// authority; `hls_rebuffering` remains only a worker/diagnostics mirror and cannot classify a
    /// ResumePrime without racing a user transition.
    pub(crate) fn hls_prime_kind(&self) -> Option<HlsPrimeKind> {
        let clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        let HlsClockPhase::Held { hold } = clock.phase else {
            return None;
        };
        hold.prime_kind()
    }

    /// Final check-and-reserve for `sf_play`. Candidate/trial arm uses the same mutex, eliminating
    /// the old check/use gap between the last seqlock read and the native Play call.
    pub(crate) fn reserve_hls_prime_play(
        &self,
        expected_kind: HlsPrimeKind,
        expected_candidate: u64,
        expected_recovery: HlsRecoveryEpoch,
    ) -> Option<(u64, HlsRecoveryEpoch)> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if self.hls_trial_reserve_ms.load(Ordering::Acquire) >= 0
            || clock.automatic.is_some()
            || self.hls_candidate_generation.load(Ordering::Acquire) != expected_candidate
            || expected_candidate & 1 != 0
            || (expected_kind == HlsPrimeKind::Rebuffer && clock.recovery != expected_recovery)
            || clock.user_held
        {
            return None;
        }
        let HlsClockPhase::Held { hold } = clock.phase else {
            return None;
        };
        if hold.prime_kind() != Some(expected_kind) {
            return None;
        }
        let token = clock.token();
        let recovery = clock.recovery;
        let resume_acb = clock.resume_acb_pending;
        clock.phase = HlsClockPhase::PlayIssuing {
            token,
            hold,
            resume_acb,
        };
        Some((token, recovery))
    }

    pub(crate) fn complete_hls_prime_play(&self, token: u64, accepted: bool) -> HlsPlayCompletion {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        let HlsClockPhase::PlayIssuing {
            token: active,
            hold,
            resume_acb,
        } = clock.phase
        else {
            return HlsPlayCompletion::Stale;
        };
        if active != token {
            return HlsPlayCompletion::Stale;
        }
        if accepted {
            clock.phase = HlsClockPhase::Running;
            clock.recovery = HlsRecoveryEpoch::idle();
            clock.user_held = false;
            clock.resume_acb_pending = false;
            if hold == ClockHold::User {
                clock.advance_user_sequence();
            }
            if hold == ClockHold::Rebuffer {
                self.hls_rebuffering.store(false, Ordering::Release);
            }
        } else {
            clock.phase = HlsClockPhase::Held { hold };
            if hold == ClockHold::User {
                clock.user_held = true;
            };
        }
        self.hls_clock_changed.notify_all();
        if accepted {
            HlsPlayCompletion::Accepted { resume_acb }
        } else {
            HlsPlayCompletion::Refused
        }
    }

    pub(crate) fn begin_hls_candidate_transition(
        &self,
        expected_automatic: Option<u64>,
    ) -> Result<u64, HlsClockFenceError> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        while matches!(clock.phase, HlsClockPhase::PlayIssuing { .. }) {
            clock = self
                .hls_clock_changed
                .wait(clock)
                .unwrap_or_else(|e| e.into_inner());
        }
        if clock.phase == HlsClockPhase::Stopping {
            return Err(HlsClockFenceError::Stopping);
        }
        if let Some(expected) = expected_automatic {
            let Some(owner) = clock.automatic else {
                return Err(HlsClockFenceError::Superseded);
            };
            let phase_ok = match owner.kind {
                HlsAutomaticTransition::QualityUp | HlsAutomaticTransition::Original => {
                    clock.phase == HlsClockPhase::Running
                        && !self.hls_rebuffer_requested.load(Ordering::Acquire)
                }
                HlsAutomaticTransition::QualityDown => matches!(
                    clock.phase,
                    HlsClockPhase::Running
                        | HlsClockPhase::PauseIssuing {
                            hold: ClockHold::Rebuffer,
                            ..
                        }
                        | HlsClockPhase::Held {
                            hold: ClockHold::Rebuffer,
                        }
                ),
            };
            if owner.token != expected
                || owner.phase != HlsAutomaticPhase::Working
                || owner.user_sequence != clock.user_sequence
                || clock.user_held
                || !phase_ok
            {
                return Err(HlsClockFenceError::Superseded);
            }
            clock
                .automatic
                .as_mut()
                .expect("the validated automatic owner remains present")
                .phase = HlsAutomaticPhase::CommitAuthorized;
        }
        let stable = self.hls_candidate_generation.load(Ordering::Acquire);
        if stable & 1 != 0 {
            return Err(HlsClockFenceError::Overlap);
        }
        let generation = stable.checked_add(1).ok_or(HlsClockFenceError::Exhausted)?;
        self.hls_candidate_generation
            .compare_exchange(stable, generation, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| generation)
            .map_err(|_| HlsClockFenceError::Overlap)
    }

    pub(crate) fn settle_hls_candidate_transition(&self, generation: u64) -> bool {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        // The mutex phase is authoritative. During PauseIssuing the diagnostics mirror is still
        // false, but the recovery epoch already exists and a rung publication must rebase it.
        if matches!(
            clock.phase,
            HlsClockPhase::PauseIssuing {
                hold: ClockHold::Rebuffer,
                ..
            } | HlsClockPhase::Held {
                hold: ClockHold::Rebuffer,
            }
        ) {
            clock.recovery.begin();
        }
        let Some(settled) = generation.checked_add(1) else {
            // Remain odd/fenced forever rather than letting an ancient generation become current.
            return false;
        };
        let accepted = self
            .hls_candidate_generation
            .compare_exchange(generation, settled, Ordering::Release, Ordering::Acquire)
            .is_ok();
        if accepted {
            self.hls_clock_changed.notify_all();
        }
        accepted
    }

    pub(crate) fn hls_user_sequence(&self) -> u64 {
        self.hls_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .user_sequence
    }

    pub(crate) fn arm_hls_trial_at(
        &self,
        runway_ms: i64,
        expected_user_sequence: u64,
    ) -> Option<u64> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase != HlsClockPhase::Running
            || clock.user_held
            || clock.user_sequence != expected_user_sequence
            || clock.automatic.is_some()
            || self.hls_rebuffer_requested.load(Ordering::Acquire)
            || self.hls_candidate_generation.load(Ordering::Acquire) & 1 != 0
        {
            return None;
        }
        let token = clock.token();
        clock.automatic = Some(HlsAutomaticOwner {
            token,
            kind: HlsAutomaticTransition::QualityUp,
            phase: HlsAutomaticPhase::Working,
            user_sequence: clock.user_sequence,
        });
        self.hls_trial_reserve_ms
            .store(runway_ms.max(0), Ordering::Release);
        Some(token)
    }

    #[cfg(test)]
    pub(crate) fn arm_hls_trial(&self, runway_ms: i64) -> Option<u64> {
        let sequence = self.hls_user_sequence();
        self.arm_hls_trial_at(runway_ms, sequence)
    }

    pub(crate) fn finish_hls_trial(&self, token: u64) -> bool {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(
            clock.automatic,
            Some(HlsAutomaticOwner {
                token: active,
                kind: HlsAutomaticTransition::QualityUp,
                ..
            }) if active == token
        ) {
            return false;
        }
        clock.automatic = None;
        self.hls_trial_reserve_ms.store(-1, Ordering::Release);
        self.hls_clock_changed.notify_all();
        true
    }

    pub(crate) fn begin_hls_automatic_transition_at(
        &self,
        kind: HlsAutomaticTransition,
        expected_user_sequence: u64,
    ) -> Option<u64> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.automatic.is_some()
            || clock.user_held
            || clock.user_sequence != expected_user_sequence
            || self.hls_candidate_generation.load(Ordering::Acquire) & 1 != 0
            || self.hls_trial_reserve_ms.load(Ordering::Acquire) >= 0
        {
            return None;
        }
        let phase_ok = match kind {
            HlsAutomaticTransition::QualityUp | HlsAutomaticTransition::Original => {
                clock.phase == HlsClockPhase::Running
                    && !self.hls_rebuffer_requested.load(Ordering::Acquire)
            }
            HlsAutomaticTransition::QualityDown => matches!(
                clock.phase,
                HlsClockPhase::Running
                    | HlsClockPhase::PauseIssuing {
                        hold: ClockHold::Rebuffer,
                        ..
                    }
                    | HlsClockPhase::Held {
                        hold: ClockHold::Rebuffer,
                    }
            ),
        };
        if !phase_ok {
            return None;
        }
        let token = clock.token();
        clock.automatic = Some(HlsAutomaticOwner {
            token,
            kind,
            phase: HlsAutomaticPhase::Working,
            user_sequence: clock.user_sequence,
        });
        Some(token)
    }

    #[cfg(test)]
    pub(crate) fn begin_hls_automatic_transition(
        &self,
        kind: HlsAutomaticTransition,
    ) -> Option<u64> {
        let sequence = self.hls_user_sequence();
        self.begin_hls_automatic_transition_at(kind, sequence)
    }

    pub(crate) fn authorize_hls_automatic_transition_commit(&self, token: u64) -> bool {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.user_held || clock.phase != HlsClockPhase::Running {
            return false;
        }
        let user_sequence = clock.user_sequence;
        let Some(owner) = clock.automatic.as_mut() else {
            return false;
        };
        if owner.token != token
            || owner.phase != HlsAutomaticPhase::Working
            || owner.user_sequence != user_sequence
        {
            return false;
        }
        owner.phase = HlsAutomaticPhase::CommitAuthorized;
        true
    }

    pub(crate) fn finish_hls_automatic_transition(&self, token: u64) -> bool {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(clock.automatic, Some(HlsAutomaticOwner { token: active, .. }) if active == token)
        {
            return false;
        }
        clock.automatic = None;
        self.hls_clock_changed.notify_all();
        true
    }

    /// One synchronized authority check for every automatic actuator. Diagnostics atomics are
    /// mirrors; they cannot decide whether a user hold, native clock transition, candidate
    /// publication, or another private trial owns the boundary.
    pub(crate) fn hls_automatic_transition_permitted(&self, kind: HlsAutomaticTransition) -> bool {
        let clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.user_held
            || clock.automatic.is_some()
            || self.hls_candidate_generation.load(Ordering::Acquire) & 1 != 0
            || self.hls_trial_reserve_ms.load(Ordering::Acquire) >= 0
        {
            return false;
        }
        match kind {
            HlsAutomaticTransition::QualityUp | HlsAutomaticTransition::Original => {
                clock.phase == HlsClockPhase::Running
                    && !self.hls_rebuffer_requested.load(Ordering::Acquire)
            }
            HlsAutomaticTransition::QualityDown => matches!(
                clock.phase,
                HlsClockPhase::Running
                    | HlsClockPhase::PauseIssuing {
                        hold: ClockHold::Rebuffer,
                        ..
                    }
                    | HlsClockPhase::Held {
                        hold: ClockHold::Rebuffer,
                    }
            ),
        }
    }

    pub(crate) fn stop_hls_clock(&self) {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        clock.phase = HlsClockPhase::Stopping;
        clock.recovery = HlsRecoveryEpoch::idle();
        clock.user_held = false;
        clock.resume_acb_pending = false;
        clock.automatic = None;
        self.hls_rebuffering.store(false, Ordering::Release);
        self.hls_clock_changed.notify_all();
    }

    fn reset_hls_clock(&self) {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        // Session identity changes, token identity does not restart. A worker from the retired
        // session may still drop its RAII lease after reset; preserving this counter makes that
        // completion stale instead of letting token 1 clear the next session's token 1.
        let next_token = clock.next_token;
        let user_sequence = clock.user_sequence;
        *clock = HlsClockState::new();
        clock.next_token = next_token;
        clock.user_sequence = user_sequence;
        self.hls_rebuffering.store(false, Ordering::Relaxed);
        self.hls_internal_hold_epoch.fetch_add(1, Ordering::AcqRel);
        self.hls_trial_reserve_ms.store(-1, Ordering::Relaxed);
        // Session reset itself invalidates every prime snapshot. Preserve monotonicity rather than
        // writing zero: a reader spanning teardown must observe a different even generation.
        // The same read-modify-write loop `fetch_update` runs internally; see
        // `player::report::requested` for why it is spelled out rather than called.
        let mut current = self.hls_candidate_generation.load(Ordering::Acquire);
        loop {
            let next = if current & 1 == 0 {
                current.checked_add(2).unwrap_or(u64::MAX)
            } else {
                current.checked_add(1).unwrap_or(u64::MAX)
            };
            match self.hls_candidate_generation.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        self.hls_clock_changed.notify_all();
    }
}

/// UI-facing transport state. Main-thread-only in practice (plex_run + pump +
/// player_hud all run on M), but exposed as atomics so app.rs / player_hud.rs read
/// it with plain .load()/.store(). Replaces the #[no_mangle] transport globals.
pub(crate) struct Transport {
    pub started: AtomicBool, // bf_started
    pub paused: AtomicBool,  // pl_paused
    /// Monotone duration of native-accepted user Pause. The bool alone loses a Pause->Resume
    /// interval that begins and ends while the demuxer is blocked, so bounded recovery work uses
    /// this synchronized clock to subtract exactly those intervals from elapsed time.
    pause_clock: Mutex<UserPauseClock>,
    pub resume_pend: AtomicBool, // resumePausePending
    /// The viewer remains logically paused while a seek is allowed to feed, Play one landed frame,
    /// then re-pause. Keeping this separate prevents the UI/user intent from lying during preroll.
    seek_preroll: AtomicBool,
    pub hud_until: AtomicU32,  // pl_hud_until (SDL ticks)
    pub scrub_ns: AtomicI64,   // pl_scrub_ns (-1 = not scrubbing)
    pub seek_to_ns: AtomicI64, // g_seek_to_ns (UI seek request, -1 = none)
    // Seek requests received since the pump last APPLIED one. seek_to_ns only ever holds the
    // newest target, so without this counter a coalesced burst is indistinguishable from a
    // single tap after the fact — the pump reports `coalesced=` from it (see pump.rs).
    pub seek_reqs: AtomicU32,
}

#[derive(Clone, Copy)]
struct UserPauseClock {
    completed: Duration,
    active_since: Option<Instant>,
    sequence: u64,
}

impl UserPauseClock {
    const fn new() -> Self {
        Self {
            completed: Duration::ZERO,
            active_since: None,
            sequence: 0,
        }
    }

    fn elapsed_at(self, now: Instant) -> Duration {
        self.completed.saturating_add(
            self.active_since
                .map_or(Duration::ZERO, |since| now.saturating_duration_since(since)),
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct UserPauseSnapshot {
    pub(crate) now: Instant,
    pub(crate) elapsed: Duration,
    pub(crate) paused: bool,
    pub(crate) sequence: u64,
}

/// Per-worker cursor over accepted transport events. A complete Pause→Resume may happen while a
/// worker is blocked in native queue/network I/O; consuming the monotone sequence plus cumulative
/// duration makes that interval observable even when both surrounding boolean samples are false.
#[derive(Clone, Copy, Debug)]
pub(crate) struct UserPauseCursor {
    sequence: u64,
    elapsed: Duration,
}

impl UserPauseCursor {
    pub(crate) fn new(snapshot: UserPauseSnapshot) -> Self {
        Self {
            sequence: snapshot.sequence,
            elapsed: snapshot.elapsed,
        }
    }

    pub(crate) fn consume_completed(&mut self, snapshot: UserPauseSnapshot) -> Option<Duration> {
        if snapshot.paused || snapshot.sequence == self.sequence {
            return None;
        }
        let elapsed = snapshot.elapsed.saturating_sub(self.elapsed);
        self.sequence = snapshot.sequence;
        self.elapsed = snapshot.elapsed;
        (!elapsed.is_zero()).then_some(elapsed)
    }
}

impl Transport {
    pub const fn new() -> Self {
        Transport {
            started: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            pause_clock: Mutex::new(UserPauseClock::new()),
            resume_pend: AtomicBool::new(false),
            seek_preroll: AtomicBool::new(false),
            hud_until: AtomicU32::new(0),
            scrub_ns: AtomicI64::new(-1),
            seek_to_ns: AtomicI64::new(-1),
            seek_reqs: AtomicU32::new(0),
        }
    }

    /// Publish a native-accepted user transport transition to both the feed gate and the exact
    /// pause clock. Every production writer goes through this method; refused native transitions
    /// never enter either state.
    pub(crate) fn commit_paused(&self, paused: bool) {
        let mut clock = self.pause_clock.lock().unwrap_or_else(|e| e.into_inner());
        // The timestamp belongs to the mutex-order transition. Sampling it before `lock` lets a
        // concurrent clock snapshot linearize first while this transition carries an earlier
        // time, manufacturing paused duration before the transaction's own W0.
        let now = Instant::now();
        match (clock.active_since, paused) {
            (None, true) => {
                clock.active_since = Some(now);
                clock.sequence = clock
                    .sequence
                    .checked_add(1)
                    .expect("accepted Pause sequence exhausted");
            }
            (Some(since), false) => {
                clock.completed = clock
                    .completed
                    .saturating_add(now.saturating_duration_since(since));
                clock.active_since = None;
                clock.sequence = clock
                    .sequence
                    .checked_add(1)
                    .expect("accepted Pause sequence exhausted");
            }
            _ => {}
        }
        self.paused.store(paused, Ordering::Release);
    }

    /// One synchronized sample for an unpaused-elapsed transaction clock. `Instant` and the
    /// cumulative accepted-Pause duration are observed under the transition mutex, so a complete
    /// Pause->Resume inside a blocked network call can never disappear between two bool reads.
    pub(crate) fn pause_clock_sample(&self) -> (Instant, Duration) {
        let sample = self.pause_state_sample();
        (sample.now, sample.elapsed)
    }

    pub(crate) fn pause_state_sample(&self) -> UserPauseSnapshot {
        let clock = self.pause_clock.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        UserPauseSnapshot {
            now,
            elapsed: clock.elapsed_at(now),
            paused: clock.active_since.is_some(),
            sequence: clock.sequence,
        }
    }

    pub(crate) fn begin_paused_seek(&self) -> bool {
        if !self.paused.load(Ordering::Acquire) {
            return false;
        }
        self.seek_preroll.store(true, Ordering::Release);
        true
    }

    pub(crate) fn seek_preroll_active(&self) -> bool {
        self.seek_preroll.load(Ordering::Acquire)
    }

    /// Feeding is an actuator concern, not the user-visible pause intent. A paused seek is the one
    /// bounded exception: it must decode a landed frame while the transport still says Paused.
    pub(crate) fn feed_allowed(&self) -> bool {
        !self.paused.load(Ordering::Acquire) || self.seek_preroll_active()
    }

    pub(crate) fn finish_seek_preroll(&self) {
        self.seek_preroll.store(false, Ordering::Release);
        self.resume_pend.store(false, Ordering::Release);
    }

    /// An engine reload is not a new playback and must not erase viewer Pause or the paused-seek
    /// handoff. It only retires engine-local transport mailboxes.
    pub(crate) fn reset_for_reload(&self) {
        self.started.store(false, Ordering::Relaxed);
        self.scrub_ns.store(-1, Ordering::Relaxed);
        self.seek_to_ns.store(-1, Ordering::Relaxed);
        self.seek_reqs.store(0, Ordering::Relaxed);
    }

    /// reset on stop (mirrors the transport tail of stop_bufferfeed).
    pub fn reset(&self) {
        self.started.store(false, Ordering::Relaxed);
        *self.pause_clock.lock().unwrap_or_else(|e| e.into_inner()) = UserPauseClock::new();
        self.paused.store(false, Ordering::Release);
        self.resume_pend.store(false, Ordering::Relaxed);
        self.seek_preroll.store(false, Ordering::Relaxed);
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

    #[test]
    fn a_worker_consumes_a_complete_hidden_pause_cycle_as_one_event() {
        let transport = Transport::new();
        let mut cursor = UserPauseCursor::new(transport.pause_state_sample());
        transport.commit_paused(true);
        std::thread::sleep(Duration::from_millis(2));
        transport.commit_paused(false);

        let after = transport.pause_state_sample();
        assert!(
            !after.paused,
            "the surrounding boolean samples are both false"
        );
        assert_eq!(after.sequence, 2);
        assert!(
            cursor
                .consume_completed(after)
                .is_some_and(|elapsed| elapsed >= Duration::from_millis(1)),
            "the accepted interval must survive a blocked worker",
        );
        assert_eq!(
            cursor.consume_completed(after),
            None,
            "an event is consumed exactly once"
        );
    }

    #[test]
    fn user_hold_is_inside_the_automatic_transition_authority() {
        let shared = Shared::new();
        assert!(shared.hls_automatic_transition_permitted(HlsAutomaticTransition::Original));
        let HlsUserPause::Issue(token) = shared.prepare_hls_user_pause().unwrap() else {
            panic!("running clock must issue the user Pause");
        };
        assert_eq!(
            shared.complete_hls_user_pause(token, true),
            HlsPauseCompletion::Accepted,
        );
        assert!(!shared.hls_automatic_transition_permitted(HlsAutomaticTransition::Original));
        assert!(!shared.hls_automatic_transition_permitted(HlsAutomaticTransition::QualityDown));
    }

    #[test]
    fn exactly_one_automatic_actuator_owns_a_boundary() {
        let shared = Shared::new();
        let original = shared
            .begin_hls_automatic_transition(HlsAutomaticTransition::Original)
            .expect("running playback grants the first automatic lease");
        assert!(
            shared
                .begin_hls_automatic_transition(HlsAutomaticTransition::QualityDown)
                .is_none(),
            "a second actuator cannot pass the same boundary while Original owns it",
        );
        assert!(shared.authorize_hls_automatic_transition_commit(original));
        assert!(
            !shared.authorize_hls_automatic_transition_commit(original),
            "commit authorization is one explicit edge, not a repeatable predicate",
        );
        assert!(shared.finish_hls_automatic_transition(original));
        let down = shared
            .begin_hls_automatic_transition(HlsAutomaticTransition::QualityDown)
            .expect("dropping the former owner releases the next explicit transition");
        assert!(shared.finish_hls_automatic_transition(down));
    }

    #[test]
    fn a_complete_pause_resume_cycle_invalidates_an_earlier_automatic_boundary() {
        let shared = Shared::new();
        let observed_user_sequence = shared.hls_user_sequence();
        let HlsUserPause::Issue(pause) = shared.prepare_hls_user_pause().unwrap() else {
            panic!("running clock must issue Pause");
        };
        assert_eq!(
            shared.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted,
        );
        let HlsUserResume::Issue(play) = shared.prepare_hls_user_resume(false).unwrap() else {
            panic!("a held non-queued clock must issue Play");
        };
        assert!(matches!(
            shared.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { .. }
        ));

        assert!(
            shared
                .begin_hls_automatic_transition_at(
                    HlsAutomaticTransition::Original,
                    observed_user_sequence,
                )
                .is_none(),
            "the same Running/!held shape is a different boundary after Pause->Resume",
        );
    }

    #[test]
    fn a_retired_trial_cannot_release_the_next_sessions_lease() {
        let shared = Shared::new();
        let retired = shared
            .arm_hls_trial(1_000)
            .expect("first session grants its trial");
        shared.reset_hls_clock_for_test();
        let current = shared
            .arm_hls_trial(2_000)
            .expect("next session grants an independent trial");
        assert_ne!(retired, current);
        assert!(
            !shared.finish_hls_trial(retired),
            "the retired RAII drop has no authority in the new session",
        );
        assert_eq!(shared.hls_trial_reserve_ms.load(Ordering::Acquire), 2_000);
        assert!(shared.finish_hls_trial(current));
    }

    #[test]
    fn a_retired_actuator_cannot_publish_candidate_media_into_the_next_session() {
        let shared = Shared::new();
        let retired = shared
            .begin_hls_automatic_transition(HlsAutomaticTransition::QualityDown)
            .expect("first session grants its downshift");
        shared.reset_hls_clock_for_test();
        let current = shared
            .begin_hls_automatic_transition(HlsAutomaticTransition::QualityDown)
            .expect("next session grants its own downshift");
        assert_eq!(
            shared.begin_hls_candidate_transition(Some(retired)),
            Err(HlsClockFenceError::Superseded),
        );
        let candidate = shared
            .begin_hls_candidate_transition(Some(current))
            .expect("only the current actuator may enter publication");
        assert!(shared.settle_hls_candidate_transition(candidate));
        assert!(shared.finish_hls_automatic_transition(current));
    }

    #[test]
    fn internal_reprime_waits_for_the_recovery_actuator_transaction() {
        let shared = Shared::new();
        let down = shared
            .begin_hls_automatic_transition(HlsAutomaticTransition::QualityDown)
            .expect("running clock grants the recovery actuator");
        let pause = shared
            .prepare_hls_rebuffer_pause()
            .expect("the internal clock can enter its recovery hold");
        assert_eq!(
            shared.complete_hls_rebuffer_pause(pause, true),
            HlsPauseCompletion::Accepted,
        );
        shared.observe_hls_recovery(500_000, Duration::from_secs(2));
        let generation = shared.hls_candidate_generation.load(Ordering::Acquire);
        assert!(
            shared
                .reserve_hls_prime_play(HlsPrimeKind::Rebuffer, generation, shared.hls_recovery())
                .is_none(),
            "the old actuator may not restart while its replacement owns the boundary",
        );
        assert!(shared.finish_hls_automatic_transition(down));
        assert!(shared
            .reserve_hls_prime_play(HlsPrimeKind::Rebuffer, generation, shared.hls_recovery())
            .is_some());
    }

    #[test]
    fn a_user_pause_wins_over_an_inflight_original_completion() {
        let shared = Shared::new();
        let original = shared
            .begin_hls_automatic_transition(HlsAutomaticTransition::Original)
            .expect("running playback grants an Original lease");
        let HlsUserPause::Issue(pause) = shared.prepare_hls_user_pause().unwrap() else {
            panic!("the orthogonal user transition must remain available");
        };
        assert_eq!(
            shared.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted,
        );
        assert!(
            !shared.authorize_hls_automatic_transition_commit(original),
            "a late network result is evidence only after the viewer owns the clock",
        );
        assert!(shared.finish_hls_automatic_transition(original));
    }

    #[test]
    fn a_user_clock_transition_revokes_inflight_quality_publication() {
        let shared = Shared::new();
        let up = shared
            .begin_hls_automatic_transition(HlsAutomaticTransition::QualityUp)
            .expect("running playback grants the upshift lease");
        let HlsUserPause::Issue(pause) = shared.prepare_hls_user_pause().unwrap() else {
            panic!("the user Pause remains orthogonal while candidate I/O runs");
        };
        assert_eq!(
            shared.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted,
        );
        assert_eq!(
            shared.begin_hls_candidate_transition(Some(up)),
            Err(HlsClockFenceError::Superseded),
            "private candidate bytes may not publish after the viewer changed the clock",
        );
        assert!(shared.finish_hls_automatic_transition(up));
    }

    #[test]
    fn raster_readers_never_observe_a_pair_no_stream_published() {
        let shared = Shared::new();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..20_000 {
                    shared.publish_video_raster(1_280, 720);
                    shared.publish_video_raster(1_920, 1_080);
                }
            });
            for _ in 0..20_000 {
                assert!(matches!(
                    shared.video_raster(),
                    (0, 0) | (1_280, 720) | (1_920, 1_080)
                ));
            }
        });
    }

    #[test]
    fn recovery_closes_the_actual_pause_epoch_not_a_permutation_of_history() {
        let mut epoch = HlsRecoveryEpoch::idle();
        epoch.begin();
        epoch.observe(2_500_000, std::time::Duration::from_secs(2));
        assert_eq!(epoch.debt_us, 500_000);
        assert!(
            !epoch.ready(20_000_000_000),
            "reserve alone cannot erase an open acquisition debt"
        );

        epoch.observe(1_500_000, std::time::Duration::from_secs(2));
        assert_eq!(epoch.debt_us, 0);
        assert_eq!(epoch.runway_us, 2_500_000);
        assert!(
            !epoch.ready(2_499_999_999),
            "one nanosecond below the measured prefix is not enough"
        );
        assert!(epoch.ready(2_500_000_000));
    }

    #[test]
    fn a_new_hold_carries_no_debt_from_an_old_slow_segment() {
        let mut epoch = HlsRecoveryEpoch::idle();
        epoch.begin();
        epoch.observe(9_000_000, std::time::Duration::from_secs(2));
        assert_eq!(epoch.debt_us, 7_000_000);

        epoch.begin();
        epoch.observe(500_000, std::time::Duration::from_secs(2));
        assert_eq!(epoch.completed, 1);
        assert_eq!(epoch.debt_us, -1_500_000);
        assert_eq!(epoch.runway_us, 500_000);
        assert!(epoch.ready(500_000_000));
    }

    #[test]
    fn initial_prime_is_an_explicit_hold_that_rejects_trials_and_fences_user_resume() {
        let s = Shared::new();
        assert!(s.arm_initial_clock_hold(false, false));
        assert!(
            s.arm_hls_trial(1_000).is_none(),
            "an up-trial cannot spend reserve before the first native Play"
        );
        assert_eq!(s.prepare_hls_user_pause(), Some(HlsUserPause::AlreadyHeld));
        assert_eq!(
            s.prepare_hls_user_resume(true),
            Some(HlsUserResume::Deferred)
        );

        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        let (play, _) = s
            .reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, s.hls_recovery())
            .expect("the media-prime lane, not user Resume, owns the first Play");
        assert_eq!(
            s.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { resume_acb: true }
        );
        assert!(
            s.arm_hls_trial(1_000).is_some(),
            "trials are admitted once the clock is Running"
        );
    }

    #[test]
    fn foreground_play_releases_both_the_pause_mirror_and_initial_user_hold() {
        let s = Shared::new();
        assert!(s.arm_initial_clock_hold(true, false));
        assert_eq!(
            s.prepare_hls_user_resume(true),
            Some(HlsUserResume::Deferred),
        );
        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        assert!(
            s.reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, s.hls_recovery())
                .is_some(),
            "the foreground Play event must clear the clock reducer's user hold, not only TX",
        );
    }

    #[test]
    fn a_user_pause_before_progressive_load_completed_fences_the_initial_play() {
        let s = Shared::new();
        assert!(s.arm_initial_clock_hold(false, false));
        assert_eq!(s.prepare_hls_user_pause(), Some(HlsUserPause::AlreadyHeld));

        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        assert!(
            s.reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, s.hls_recovery())
                .is_none(),
            "the progressive load-complete fast path must not Play through viewer Pause",
        );

        assert_eq!(
            s.prepare_hls_user_resume(true),
            Some(HlsUserResume::Deferred)
        );
        assert!(
            s.reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, s.hls_recovery())
                .is_some(),
            "the same initial hold must become playable after the viewer resumes",
        );
    }

    #[test]
    fn a_stream_resume_does_not_issue_play_before_feed_reopens() {
        let s = Shared::new();
        let pause = match s.prepare_hls_user_pause() {
            Some(HlsUserPause::Issue(token)) => token,
            other => panic!("unexpected Pause reservation: {other:?}"),
        };
        assert_eq!(
            s.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted
        );

        assert_eq!(
            s.prepare_hls_user_resume(true),
            Some(HlsUserResume::Prime),
            "a queued stream must reopen feeding and earn a balanced prime before native Play",
        );
        assert_eq!(s.hls_prime_kind(), Some(HlsPrimeKind::Resume));
    }

    #[test]
    fn a_second_pause_during_resume_prime_fences_play_until_the_second_resume() {
        let s = Shared::new();
        let pause = match s.prepare_hls_user_pause() {
            Some(HlsUserPause::Issue(token)) => token,
            other => panic!("unexpected Pause reservation: {other:?}"),
        };
        assert_eq!(
            s.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted
        );
        assert_eq!(s.prepare_hls_user_resume(true), Some(HlsUserResume::Prime));
        assert_eq!(s.prepare_hls_user_pause(), Some(HlsUserPause::AlreadyHeld));

        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        assert!(
            s.reserve_hls_prime_play(HlsPrimeKind::Resume, generation, s.hls_recovery())
                .is_none(),
            "the second viewer Pause must fence a ready ResumePrime",
        );
        assert_eq!(
            s.prepare_hls_user_resume(true),
            Some(HlsUserResume::Deferred),
            "the existing ResumePrime certificate survives the second viewer Resume",
        );
        let (play, _) = s
            .reserve_hls_prime_play(HlsPrimeKind::Resume, generation, s.hls_recovery())
            .expect("the second Resume releases the same prime once its media is ready");
        assert_eq!(
            s.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { resume_acb: true }
        );
    }

    #[test]
    fn a_local_sample_without_stream_queues_keeps_immediate_resume() {
        let s = Shared::new();
        let pause = match s.prepare_hls_user_pause() {
            Some(HlsUserPause::Issue(token)) => token,
            other => panic!("unexpected Pause reservation: {other:?}"),
        };
        assert_eq!(
            s.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted
        );
        assert!(matches!(
            s.prepare_hls_user_resume(false),
            Some(HlsUserResume::Issue(_))
        ));
    }

    #[test]
    fn a_seek_transfers_resume_prime_to_the_fresh_timeline_certificate() {
        let s = Shared::new();
        let pause = match s.prepare_hls_user_pause() {
            Some(HlsUserPause::Issue(token)) => token,
            other => panic!("unexpected Pause reservation: {other:?}"),
        };
        assert_eq!(
            s.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted
        );
        assert_eq!(s.prepare_hls_user_resume(true), Some(HlsUserResume::Prime));
        assert_eq!(s.prepare_seek_pause(), Some(HlsSeekPause::AlreadyHeld));
        assert_eq!(s.hls_prime_kind(), Some(HlsPrimeKind::Fresh));

        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        assert!(
            s.reserve_hls_prime_play(HlsPrimeKind::Resume, generation, s.hls_recovery())
                .is_none(),
            "a stale ResumePrime snapshot cannot Play through a newer seek",
        );
        assert!(
            s.reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, s.hls_recovery())
                .is_some(),
            "the seek's fresh-timeline certificate now owns Play",
        );
    }

    #[test]
    fn a_paused_non_seek_reload_stays_held_but_a_seek_preroll_may_decode_its_still() {
        let ordinary_reload = Shared::new();
        assert!(ordinary_reload.arm_initial_clock_hold(true, false));
        let ordinary_generation = ordinary_reload
            .hls_candidate_generation
            .load(Ordering::Acquire);
        assert!(
            ordinary_reload
                .reserve_hls_prime_play(
                    HlsPrimeKind::Fresh,
                    ordinary_generation,
                    ordinary_reload.hls_recovery(),
                )
                .is_none(),
            "preserving Pause across a non-seek reload must not authorize native Play",
        );

        let seek_reload = Shared::new();
        assert!(seek_reload.arm_initial_clock_hold(true, true));
        let seek_generation = seek_reload.hls_candidate_generation.load(Ordering::Acquire);
        let (play, _) = seek_reload
            .reserve_hls_prime_play(
                HlsPrimeKind::Fresh,
                seek_generation,
                seek_reload.hls_recovery(),
            )
            .expect("the explicit one-frame seek preroll owns this Play");
        assert_eq!(
            seek_reload.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { resume_acb: true },
            "the preroll Play must carry the deferred ACB resume",
        );
    }

    #[test]
    fn trial_and_rebuffer_pause_linearize_under_one_mutex() {
        let trial_first = Shared::new();
        let trial = trial_first
            .arm_hls_trial(1_000)
            .expect("running clock grants the trial");
        assert!(
            trial_first.prepare_hls_rebuffer_pause().is_none(),
            "a reserved trial makes Pause retry"
        );
        assert!(trial_first.finish_hls_trial(trial));
        assert!(trial_first.prepare_hls_rebuffer_pause().is_some());

        let pause_first = Shared::new();
        let _pause = pause_first
            .prepare_hls_rebuffer_pause()
            .expect("Pause wins the reservation");
        assert!(
            pause_first.arm_hls_trial(1_000).is_none(),
            "a reserved Pause makes the up-trial reject"
        );
    }

    #[test]
    fn candidate_settlement_rebases_recovery_while_pause_is_still_issuing() {
        let s = Shared::new();
        let pause = s.prepare_hls_rebuffer_pause().expect("reserve Pause");
        s.observe_hls_recovery(500_000, Duration::from_secs(2));
        assert_eq!(s.hls_recovery().completed, 1);

        let candidate = s
            .begin_hls_candidate_transition(None)
            .expect("arm down candidate");
        assert!(s.settle_hls_candidate_transition(candidate));
        assert_eq!(
            s.hls_recovery().completed,
            0,
            "new-rung media needs a fresh recovery certificate even before Pause returns"
        );
        assert_eq!(
            s.complete_hls_rebuffer_pause(pause, true),
            HlsPauseCompletion::Accepted
        );
    }

    #[test]
    fn a_paused_seek_reuses_the_physical_hold_and_resumes_acb_only_at_prime() {
        let s = Shared::new();
        let pause = match s.prepare_hls_user_pause() {
            Some(HlsUserPause::Issue(token)) => token,
            other => panic!("unexpected Pause reservation: {other:?}"),
        };
        assert_eq!(
            s.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted
        );
        assert_eq!(s.prepare_seek_pause(), Some(HlsSeekPause::AlreadyHeld));

        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        let (play, _) = s
            .reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, s.hls_recovery())
            .expect("seek prime owns the reused hold");
        assert_eq!(
            s.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { resume_acb: true }
        );
    }

    /// The segment completion callback can win the tiny race after Starfish has accepted Pause but
    /// before the main thread has returned from the FFI call. Recovery must therefore be armed by
    /// the reservation, not by the completion half of that call.
    #[test]
    fn a_segment_completed_while_pause_is_issuing_belongs_to_that_hold() {
        let s = Shared::new();
        let token = s
            .prepare_hls_rebuffer_pause()
            .expect("the stable clock reserves Pause");

        s.observe_hls_recovery(500_000, Duration::from_secs(2));
        assert_eq!(
            s.complete_hls_rebuffer_pause(token, true),
            HlsPauseCompletion::Accepted
        );

        let recovery = s.hls_recovery();
        assert_eq!(recovery.completed, 1);
        assert!(recovery.ready(500_000_000));
    }

    #[test]
    fn a_segment_seen_during_a_refused_pause_is_discarded() {
        let s = Shared::new();
        let token = s
            .prepare_hls_rebuffer_pause()
            .expect("the stable clock reserves Pause");

        s.observe_hls_recovery(500_000, Duration::from_secs(2));
        assert_eq!(
            s.complete_hls_rebuffer_pause(token, false),
            HlsPauseCompletion::Refused
        );

        assert_eq!(s.hls_recovery(), HlsRecoveryEpoch::idle());
        assert!(!s.hls_rebuffering.load(Ordering::Acquire));
    }

    /// A bool snapshot loses a complete Pause -> Play cycle inside a blocked network read. The
    /// monotone epoch is the durable observation used by the deadline transaction instead.
    #[test]
    fn an_accepted_internal_hold_remains_observable_after_play() {
        let s = Shared::new();
        let before = s.hls_internal_hold_epoch.load(Ordering::Acquire);
        let pause = s.prepare_hls_rebuffer_pause().expect("reserve Pause");
        assert_eq!(
            s.complete_hls_rebuffer_pause(pause, true),
            HlsPauseCompletion::Accepted
        );
        let held = s.hls_internal_hold_epoch.load(Ordering::Acquire);
        assert_ne!(held, before);

        s.observe_hls_recovery(500_000, Duration::from_secs(2));
        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        let recovery = s.hls_recovery();
        let (play, _) = s
            .reserve_hls_prime_play(HlsPrimeKind::Rebuffer, generation, recovery)
            .expect("the ready held clock reserves Play");
        assert_eq!(
            s.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { resume_acb: false }
        );

        assert!(!s.hls_rebuffering.load(Ordering::Acquire));
        assert_eq!(s.hls_internal_hold_epoch.load(Ordering::Acquire), held);
    }

    #[test]
    fn user_pause_and_internal_rebuffer_are_orthogonal_holds() {
        let s = Shared::new();
        let pause = s
            .prepare_hls_rebuffer_pause()
            .expect("reserve internal Pause");
        assert_eq!(
            s.complete_hls_rebuffer_pause(pause, true),
            HlsPauseCompletion::Accepted
        );

        assert_eq!(s.prepare_hls_user_pause(), Some(HlsUserPause::AlreadyHeld));
        assert_eq!(
            s.prepare_hls_user_resume(true),
            Some(HlsUserResume::Deferred)
        );

        s.observe_hls_recovery(500_000, Duration::from_secs(2));
        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        let recovery = s.hls_recovery();
        let (play, _) = s
            .reserve_hls_prime_play(HlsPrimeKind::Rebuffer, generation, recovery)
            .expect("the internal certificate owns eventual Play");
        assert_eq!(
            s.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { resume_acb: true },
            "the eventual internal Play must also finish the viewer's deferred ACB Resume",
        );
    }

    #[test]
    fn candidate_publication_defers_user_resume_until_it_settles() {
        let s = Shared::new();
        let pause = match s.prepare_hls_user_pause() {
            Some(HlsUserPause::Issue(token)) => token,
            other => panic!("unexpected Pause reservation: {other:?}"),
        };
        assert_eq!(
            s.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted
        );
        let candidate = s
            .begin_hls_candidate_transition(None)
            .expect("arm candidate");

        assert_eq!(s.prepare_hls_user_resume(true), Some(HlsUserResume::Prime));
        let recovery = s.hls_recovery();
        assert!(
            s.reserve_hls_prime_play(HlsPrimeKind::Resume, candidate, recovery)
                .is_none(),
            "an active candidate generation must fence ResumePrime",
        );
        assert!(s.settle_hls_candidate_transition(candidate));

        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        let (play, _) = s
            .reserve_hls_prime_play(HlsPrimeKind::Resume, generation, recovery)
            .expect("settlement releases the pending user Play");
        assert_eq!(
            s.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { resume_acb: true }
        );
    }

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
        assert!(
            declared.len() >= 10,
            "found only {} dg_ fields — the parse broke",
            declared.len()
        );
        // rustfmt may wrap `SHARED.dg_field.store(...)` between either dot.  Whitespace carries no
        // source-level meaning here, so remove it before looking for the writer expression.
        let writers: Vec<String> = WRITERS
            .iter()
            .map(|src| src.chars().filter(|c| !c.is_whitespace()).collect())
            .collect();
        for f in declared {
            let written = writers.iter().any(|src| {
                src.contains(&format!("dg_{f}.store(")) || src.contains(&format!("dg_{f}.fetch_"))
            });
            assert!(written, "dg_{f} is declared and read but NOTHING writes it — the panel would print its sentinel as fact");
        }
    }

    /// **`reset_session` must NOT drop the ABR seed, and `clear_abr_seed` must** (I8).
    ///
    /// `engine::teardown` calls `reset_session` on both paths — a real stop AND a reload — and a
    /// reload is the same item on the same link at a new position: a seek, a quality pick, an
    /// app-switch resume. Re-measuring the link from nothing across one is what made every skip
    /// re-ramp the ladder for ten to twenty seconds. So the split is load-bearing and this is the
    /// assertion that keeps it: the two methods are one keystroke apart and merging them back would
    /// otherwise be silent.
    #[test]
    fn the_carried_link_estimate_survives_a_reload_and_not_a_stop() {
        let s = Shared::new();
        let estimate = crate::abr::CapacityEstimate::from_snapshot(40_000, 41_000, 200, 19)
            .expect("valid measured estimate");
        s.publish_abr_seed(estimate);

        s.reset_session();
        let carried = s
            .abr_seed()
            .expect("a reload tears the engine down and keeps the link; this is the seek path");
        assert_eq!(carried.samples, 19);
        assert_eq!(carried.slow_kbps, 40_000);

        s.clear_abr_seed();
        assert!(
            s.abr_seed().is_none(),
            "a real stop ends the playback, and the next one measures its own link",
        );
    }

    #[test]
    fn a_carried_link_estimate_ages_across_an_unobserved_reload_gap() {
        let s = Shared::new();
        let estimate = crate::abr::CapacityEstimate::from_snapshot(40_000, 42_000, 100, 19)
            .expect("valid measured estimate");
        let half_life = crate::abr::AbrPolicy::measured().stale_half_life_ms;
        let observed_at = Instant::now()
            .checked_sub(Duration::from_millis(u64::from(half_life) * 2))
            .expect("test instant");
        s.publish_abr_seed_at(estimate, observed_at);

        let aged = s.abr_seed().expect("a reload carries the estimate");
        assert!(
            aged.uncertainty_pm > estimate.uncertainty_pm,
            "pause/background/reload wall time must widen carried evidence",
        );
    }

    /// A failed HLS→Original transaction immediately tears down the failed Engine and reloads
    /// the retained HLS one. The HTTP cause must survive that reset, then die with the playback.
    #[test]
    fn original_failure_survives_rollback_reload_and_not_the_playback() {
        let s = Shared::new();
        s.abr_failure_kind
            .store(crate::player::ABR_FAILURE_ORIGINAL_HTTP, Ordering::Relaxed);
        s.abr_failure_status.store(503, Ordering::Relaxed);

        s.reset_session();
        assert_eq!(
            s.abr_failure_kind.load(Ordering::Relaxed),
            crate::player::ABR_FAILURE_ORIGINAL_HTTP
        );
        assert_eq!(s.abr_failure_status.load(Ordering::Relaxed), 503);

        s.clear_abr_failure();
        assert_eq!(s.abr_failure_kind.load(Ordering::Relaxed), 0);
        assert_eq!(s.abr_failure_status.load(Ordering::Relaxed), 0);
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
        assert!(
            !s.seen_frame.load(Ordering::Relaxed),
            "a fresh session has shown nothing"
        );
        s.seen_frame.store(true, Ordering::Relaxed);
        s.frames.store(9, Ordering::Relaxed);
        s.demux_io_failed.store(true, Ordering::Relaxed);
        s.playback_trace
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .seed_for_reset_test();
        s.dg_abr_mode.store(2, Ordering::Relaxed);
        s.dg_abr_kbps.store(2_000, Ordering::Relaxed);
        s.dg_abr_declared_kbps.store(1_459, Ordering::Relaxed);
        s.dg_abr_media_kbps.store(1_051, Ordering::Relaxed);
        s.dg_abr_net_kbps.store(4_000, Ordering::Relaxed);
        s.dg_abr_buffer_ms.store(2_800, Ordering::Relaxed);
        s.dg_abr_ratio_pm.store(500, Ordering::Relaxed);
        s.dg_abr_action.store(5, Ordering::Relaxed);
        s.dg_abr_target_kbps.store(4_000, Ordering::Relaxed);
        s.dg_abr_unsafe_deficit_ms.store(1_500, Ordering::Relaxed);
        s.reset_session();
        assert!(
            !s.seen_frame.load(Ordering::Relaxed),
            "a reload/stop blanks the plane — say so"
        );
        assert_eq!(
            s.frames.load(Ordering::Relaxed),
            0,
            "and the two must be reset together"
        );
        assert!(
            !s.demux_io_failed.load(Ordering::Relaxed),
            "a new session must not inherit an I/O failure"
        );
        assert_eq!(
            s.playback_trace
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .step_count_for_test(),
            1,
            "an engine reload must preserve the playback attempt's causal trace",
        );
        assert_eq!(s.dg_abr_mode.load(Ordering::Relaxed), 0);
        assert_eq!(s.dg_abr_kbps.load(Ordering::Relaxed), 0);
        assert_eq!(s.dg_abr_declared_kbps.load(Ordering::Relaxed), -1);
        assert_eq!(s.dg_abr_media_kbps.load(Ordering::Relaxed), -1);
        assert_eq!(s.dg_abr_net_kbps.load(Ordering::Relaxed), -1);
        assert_eq!(s.dg_abr_buffer_ms.load(Ordering::Relaxed), -1);
        assert_eq!(s.dg_abr_ratio_pm.load(Ordering::Relaxed), -1);
        assert_eq!(s.dg_abr_action.load(Ordering::Relaxed), 0);
        assert_eq!(s.dg_abr_target_kbps.load(Ordering::Relaxed), 0);
        assert_eq!(s.dg_abr_unsafe_deficit_ms.load(Ordering::Relaxed), 0);
    }
}
