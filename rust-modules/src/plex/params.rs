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
///
/// **There is no bitrate field here, and a capped link is not answered by adding one.** The only
/// bitrate literal in the whole spec is `maxVideoBitrate`, on the RE-ENCODE branch of
/// `transcoder::transcode_query`; the [`remux`](Self::remux) branch deliberately sends no cap at
/// all, because a resolution or bitrate cap is precisely what makes PMS re-encode instead of copy.
/// And on the third path — direct play, this client's default and its whole point — a cap means
/// nothing, since no encoder is running to obey it. So a link with a ceiling (Plex's 2 Mbit/s
/// relay) is answered by choosing the FLAVOR — deny direct play, deny the remux, let the server
/// pick a rate on the branch that has one — never by asking for a number:
/// `transcoder::link_policy` is that decision, with the reasoning and the honesty note.
pub struct TranscodeSpec<'a> {
    pub rating_key: &'a str,
    /// The per-playback opaque session id (`route::sess()`).
    pub session: &'a str,
    /// true = container-only remux (the source codecs are direct-playable, the container
    /// isn't): copy video+audio into progressive MKV, no re-encode, keeps 4K/HDR.
    /// false = full re-encode to the profile's HEVC/AC3 target at up to 4K.
    pub remux: bool,
    /// **The server must not COPY the video track — an encoder has to run.** Only meaningful on
    /// the re-encode flavor (`remux == false`), where it swaps `directStream=1` for
    /// `directStream=0` + `directStreamAudio=1`: video re-encoded, audio still copied when it can
    /// be. Off for every ordinary transcode, where a copy is a free win.
    ///
    /// It exists because "we refused direct play" and "the server will re-encode" are NOT the same
    /// statement, and the gap between them shipped a fix that fixed nothing. `directStream=1`
    /// permits a stream copy, and PMS takes it whenever the source fits the caps the query carries
    /// — resolution, bitrate, and the profile's own limitation axes (width/height/bitDepth).
    /// **None of those axes can express Dolby Vision**, so a Profile 5 file refused by
    /// `route::video_direct_plays` came back `Part.decision=transcode` with the VIDEO stream's own
    /// decision `copy`: the identical IPT-PQ bitstream, one container down, the identical wrong
    /// colours (measured against the dev PMS 2026-08-21 — `docs/pms-api.md` §"What the server
    /// actually does with a Dolby Vision source").
    ///
    /// Setting it makes the ask honest, and the answer honest with it: a server that CAN convert
    /// the file does, and a server that cannot says so (this PMS answers
    /// `generalDecisionCode 2000` / *"File is unplayable. DoVi (Profile 5) color space is not
    /// supported."*), which `route::refusal` turns into the player's read-out quoting that
    /// sentence. Both beat a picture in the wrong colours with nothing on any surface to explain
    /// it.
    pub no_video_copy: bool,
    /// Source audio stream id to select (0 = server default).
    pub audio_stream_id: i64,
    /// Subtitle stream id to BURN (0 = none). Burn is Plex's decision for our profile —
    /// it advertises no soft-sub support (direct-play subs are client-rendered instead).
    pub subtitle_stream_id: i64,
    /// Restart the encode at this offset (seconds); < 0 = fresh start (no `&offset=`).
    pub offset_secs: i64,
}

/// One paged section-listing query (the Library browse grid). Built per fetch by the browse
/// store; consumed by `library.rs::section_items_query`. Filters are `(param, value)` pairs
/// appended verbatim (`("genre","150")`, `("unwatched","1")`, shows: `("unwatchedLeaves","1")`)
/// — tag-id values come from `section_directory` value lists. Paging: Start AND Size are
/// always sent together — PMS silently ignores a lone `X-Plex-Container-Size` query param
/// (verified live on PMS 1.43.2, 2026-07-19).
pub struct SectionQuery<'a> {
    pub section_key: i64,
    /// `sort=key[:desc]` value, e.g. "titleSort" / "addedAt:desc"; empty = server default.
    pub sort: &'a str,
    pub filters: &'a [(String, String)],
    pub start: i64,
    pub size: i64,
    /// `includeMeta=1` — the response's `Meta.Type[]` carries the section's full server-driven
    /// Sort/Filter menus; request it on the first page of a section, skip on later pages.
    pub include_meta: bool,
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
