//! Typed request params. The `Client` fills in the identity/token/session/profile bits from
//! `self` — a caller only supplies the operation-specific fields here.

/// Server-side stream selection (PUT /library/parts/{id}). The transcoder encodes the part's
/// SELECTED audio and BURNS its SELECTED subtitle; only the PUT changes the selection.
pub struct StreamSelection {
    pub part_id: i64,
    pub audio_stream_id: i64,    // 0 = keep server default (omit from query)
    pub subtitle_stream_id: i64, // 0 = subs OFF (always sent; 0 disables burn)
    pub all_parts: bool,         // true → allParts=1
}

/// Everything `transcode_decision` + `transcode_start_url` + `transcode_subtitles_url` need.
/// The `Client` fills in token, client_id, product, version, platform, session, and the fixed
/// profile string.
pub struct TranscodeSpec<'a> {
    pub rating_key: &'a str,
    pub audio_stream_id: i64,      // 0 = server default
    pub subtitle_stream_id: i64,   // 0 = none (burn when >0)
    pub offset_secs: i64,          // 0 = from start; >0 = seek/re-transcode point
    pub max_video_bitrate: i64,    // 20000 (legacy maxVideoBitrate, honored by PMS)
    pub video_resolution: &'a str, // "1920x1080"
}

pub enum TimelineState {
    Playing,
    Paused,
    Stopped,
    Buffering,
}
impl TimelineState {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            TimelineState::Playing => "playing",
            TimelineState::Paused => "paused",
            TimelineState::Stopped => "stopped",
            TimelineState::Buffering => "buffering",
        }
    }
}

pub struct TimelineReport<'a> {
    pub rating_key: &'a str,
    pub state: TimelineState,
    pub time_ms: i64,
    pub duration_ms: i64,
}
