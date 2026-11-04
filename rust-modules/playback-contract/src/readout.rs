//! Sparse backend readouts. Missing values remain unknown; native reports and clock progress
//! never establish independent picture evidence. No renderer, Plex or native handle is stored.

/// Which runtime boundary ended playback after route resolution succeeded.
///
/// This is a small product vocabulary, not a copy of FFmpeg or Starfish return codes. Every
/// variant is backed by a distinct signal the worker already publishes, so the HUD never parses
/// log strings or guesses from elapsed time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RuntimeFailure {
    /// No worker supplied a cause. Say that honestly rather than leaving the reason slot blank.
    Unknown,
    /// The producer never opened a usable media stream or produced a video access unit.
    MediaSource,
    /// The media producer stopped after it had supplied at least one access unit. The worker's
    /// flag deliberately says nothing narrower about why it stopped.
    PlaybackInterrupted,
    /// The native pipeline refused its source declaration.
    TvPipeline,
    /// Issue #74 D.1.4's `NATIVE_LOAD_BUDGET` fired — a hang, not a firmware refusal. See
    /// the distinct Load timeout classification.
    LoadTimeout,
    /// The previous object's release is overdue.
    ReleaseTimeout,
    /// Planning produced neither a source nor a refusal.
    ResolveFailed,
}

/// How the pipeline refused a Load, for the `playback.failed` event: the Load call itself returned
/// 0 (`Sync`, `threads::load_thread` via `publish_native_load_failure`), or the call returned and
/// a `type=18` error callback arrived before any picture (`Async`, `sf_on_event`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum RefusalPath {
    None = 0,
    Sync = 1,
    Async = 2,
}

impl RefusalPath {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Sync,
            2 => Self::Async,
            _ => Self::None,
        }
    }

    /// The stable wire code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Sync => "sync",
            Self::Async => "async",
        }
    }
}
/// Common transport readout. Native state and clock reports do not establish independent picture evidence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum PlaybackState {
    /// no engine
    Idle = 0,
    /// the route/plan resolve is in flight — DERIVED in `player::state()` from `route::play_pending()`,
    /// and held through a cold resume's rebuild (`route::resume_flight_outstanding()`) after the plan landed
    Resolving = 1,
    /// engine started, pipeline not yet loaded — the HTTP GET + Starfish `Load` window
    Connecting = 2,
    /// loaded and fed, but no frame has been presented yet
    Buffering = 3,
    /// a seek is resolving (reopen → prime → Play)
    Seeking = 4,
    /// the backend reports active playback
    Playing = 5,
    /// the producer died (a failed open / no video stream). Terminal until teardown.
    Error = 6,
    /// the viewer is paused on a session that has no picture yet and the pump has no work in
    /// flight for one (`pump::paused_and_idle`): not `Buffering` (nothing is being waited on, so
    /// no spinner) and not `Playing` (no frame is on the panel, so no `is_playing`, no
    /// `PlaybackStarted`). The HUD draws its ordinary paused transport.
    Paused = 7,
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
            7 => PlaybackState::Paused,
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
}

#[derive(Clone, Copy, Default)]
pub struct AttemptFacts {
    pub status: Option<PlaybackState>,
    /// The exact physical transport start flag, independent of a route or busy status.
    pub started: Option<bool>,
    /// A backend reported Playing at least once for this attempt. This is service playback
    /// evidence, not a frame callback or independent panel presentation proof.
    pub playing_observed: Option<bool>,
    /// True only after the physical pipeline drained to end of stream.
    pub ended: Option<bool>,
    /// An in-flight physical seek target. Absence replaces the feed's private -1 sentinel.
    pub seek_pending_ns: Option<i64>,
    /// A runtime rebuffer, distinct from initial buffering and seek priming.
    pub rebuffering: Option<bool>,
    /// The target the viewer sees while a physical seek resolves.
    pub seek_display_ns: Option<i64>,
    /// Source display aspect ratio reported by the physical video plane.
    pub display_aspect: Option<(u32, u32)>,
    /// A native frame callback was seen; this is not independent panel presentation proof.
    pub seen_frame: Option<bool>,
    pub requested_kbps: Option<i64>,
    pub refusal_path: Option<RefusalPath>,
    pub runtime_failure: Option<RuntimeFailure>,
    pub no_video: Option<bool>,
    pub position_ns: Option<i64>,
    pub duration_ns: Option<i64>,
    pub playable_buffer_ms: Option<i64>,
    /// A callback reported a decoded frame. This does not independently prove panel presentation.
    pub reported_frame: Option<bool>,
    pub frames: Option<i32>,
    pub raster: Option<(i32, i32)>,
    pub pipeline_stage: Option<u8>,
    pub pipeline_error: Option<i32>,
    pub http_status: Option<i32>,
    pub demux_exit: Option<u8>,
    pub declared_kbps: Option<i64>,
    pub media_kbps: Option<i64>,
    pub abr_buffer_ms: Option<i64>,
    pub transport_rc: Option<i32>,
    pub transfer_age_ms: Option<i64>,
    pub transport_reopen_failed: Option<bool>,
}

impl AttemptFacts {
    pub const EMPTY: Self = Self {
        status: None, runtime_failure: None, no_video: None, requested_kbps: None, refusal_path: None,
        started: None, playing_observed: None, ended: None, seek_pending_ns: None, rebuffering: None,
        seek_display_ns: None, display_aspect: None, seen_frame: None,
        position_ns: None, duration_ns: None, playable_buffer_ms: None,
        reported_frame: None, frames: None, raster: None, pipeline_stage: None,
        pipeline_error: None, http_status: None, demux_exit: None,
        declared_kbps: None, media_kbps: None, abr_buffer_ms: None,
        transport_rc: None, transfer_age_ms: None, transport_reopen_failed: None,
    };
}
