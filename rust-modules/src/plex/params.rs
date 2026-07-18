//! Typed request params for the playback protocol ops — built per call by `route` from its
//! playback state (the client itself is stateless about the current item/selection).
//!
//! These were rebuilt FROM the live `route.rs` (not the original design sketch): the shipped
//! protocol carries a per-playback session id on every request, distinguishes a container-only
//! REMUX from a re-encode, and reports PlayQueue + stream-selection ids on the timeline.

/// One universal-transcoder request (decision registration + start.mkv). Mirrors
/// `route::universal_base`: the CURRENT audio/subtitle selection rides every transcode of the
/// item, and `session` is the per-playback id shared with the timeline
/// (`X-Plex-Session-Identifier == session=`, byte-for-byte, so /status/sessions correlates).
pub struct TranscodeSpec<'a> {
    pub rating_key: &'a str,
    /// The per-playback opaque session id (`route::sess()`).
    pub session: &'a str,
    /// true = container-only remux (the source codecs are direct-playable, the container
    /// isn't): copy video+audio into progressive MKV, no re-encode, keeps 4K/HDR.
    /// false = full re-encode to the profile's HEVC/AC3 target at up to 4K.
    pub remux: bool,
    /// Source audio stream id to select (0 = server default).
    pub audio_stream_id: i64,
    /// Subtitle stream id to BURN (0 = none). Burn is Plex's decision for our profile —
    /// it advertises no soft-sub support (direct-play subs are client-rendered instead).
    pub subtitle_stream_id: i64,
    /// Restart the encode at this offset (seconds); < 0 = fresh start (no `&offset=`).
    pub offset_secs: i64,
}

/// Server-side stream selection for a part (`PUT /library/parts/{id}`) — the transcoder
/// encodes the part's SELECTED audio and burns its SELECTED subtitle; only a PUT changes
/// them (query params on the stream URL do not).
pub struct StreamSelection {
    pub part_id: i64,
    /// 0 = keep the server default (the PUT omits audioStreamID).
    pub audio_stream_id: i64,
    /// Always sent: 0 = subtitles OFF (suppresses a default-selected burn), else burn this id.
    pub subtitle_stream_id: i64,
}

/// One `/:/timeline` progress report (POST — the spec verb). `play_queue_*` empty = omit;
/// `*_stream_id` 0 = omit. The session id must equal the transcode `session=` (see
/// [`TranscodeSpec`]) so the server correlates the report with the stream.
pub struct TimelineReport<'a> {
    pub rating_key: &'a str,
    pub state: TimelineState,
    pub time_ms: i64,
    pub duration_ms: i64,
    pub session: &'a str,
    pub play_queue_id: &'a str,
    pub play_queue_item_id: &'a str,
    pub audio_stream_id: i64,
    pub subtitle_stream_id: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TimelineState {
    Playing,
    Paused,
    Stopped,
}

impl TimelineState {
    pub fn as_str(self) -> &'static str {
        match self {
            TimelineState::Playing => "playing",
            TimelineState::Paused => "paused",
            TimelineState::Stopped => "stopped",
        }
    }
}
