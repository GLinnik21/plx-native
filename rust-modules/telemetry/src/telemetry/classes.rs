//! **The closed vocabulary of a handled playback-error report.**
//!
//! Every value [`super::playback`] serialises is a member of one of these enums, and every
//! enum's `code` is the wire string, written out and never derived from a variant name, because a
//! rename is a refactor and must not silently re-partition a year of dashboards. The telemetry
//! layer owns the schema; `player::report` is the producer that classifies a live playback into
//! it, and `player::FailureKind::class` is the one conversion from the player's own failure enum.
//! Nothing here names the player, the route layer or the ABR controller: a constructor that needs
//! one of their types (a `QualityClass` from a `route::Quality` or an `abr::Rung`) is a function
//! of the producing layer.
//!
//! Pure data and pure bucketing, so every boundary is graded on the host. The producer's own tests
//! (`player::report`) pin the boundaries through its re-exports of these types.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceAge {
    Under1s,
    S1To3,
    S3To10,
    S10To30,
    S30To120,
    Over2m,
}

impl TraceAge {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Under1s => "<1s",
            Self::S1To3 => "1-3s",
            Self::S3To10 => "3-10s",
            Self::S10To30 => "10-30s",
            Self::S30To120 => "30-120s",
            Self::Over2m => "2m+",
        }
    }

    pub fn from_ms(ms: i64) -> Self {
        match ms.max(0) {
            0..=999 => Self::Under1s,
            1_000..=2_999 => Self::S1To3,
            3_000..=9_999 => Self::S3To10,
            10_000..=29_999 => Self::S10To30,
            30_000..=119_999 => Self::S30To120,
            _ => Self::Over2m,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryClass {
    /// No route was ever installed — the plan was refused (by the server at `/decision`, or by the
    /// Direct Play setting) or never resolved — so there is no delivery to name. Honest unknown, not
    /// a guess at which route the attempt would have taken.
    Unknown,
    Direct,
    Remux,
    Hls,
    Transcode,
}

impl DeliveryClass {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Direct => "original_direct",
            Self::Remux => "original_remux",
            Self::Hls => "hls",
            Self::Transcode => "progressive_transcode",
        }
    }
}

/// A `/decision` verdict number (`generalDecisionCode` / `transcodeDecisionCode`) as a CLOSED
/// domain. The number is a server protocol constant, not a quotation, so it is the one part of a
/// refusal a report may carry; the server's sentence beside it never is.
///
/// **Which codes exist is only partly documented, and this list claims no more than the evidence.**
/// The vendored OpenAPI spec (`docs/plex-openapi.json`, `generalDecisionCode`) documents the CLASSES
/// — "1xxx are playback can succeed, 2xxx are a general error (such as insufficient bandwidth),
/// 3xxx are errors in direct play, and 4xxx are errors in transcodes. Same codes are used in all"
/// — and no table of members. The exact numbers named here are the ones this repository has
/// observed a PMS send on a refusal: `2000` (general, "Neither direct play nor conversion is
/// available", the code `route::plan::refusal` fires on), `2003` (transcode lane, "File is
/// unplayable. DoVi (Profile 5) color space is not supported.", `docs/pms-api.md`) and `4007`
/// (transcode lane, "Cannot convert this item. Implementation for video encoder 'vp9' not found.",
/// PMS 1.43.3). Every other number falls into its documented class bucket — a new `4xxx` reads
/// `other_4xxx`, which still says "a transcode-lane error" without this list guessing its meaning —
/// and a number outside 1000-4999 is `other`. The wire code of a named member IS its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionCodeClass {
    /// The body carried no such code (never a defaulted 0).
    Absent,
    C2000,
    C2003,
    C4007,
    Other1xxx,
    Other2xxx,
    Other3xxx,
    Other4xxx,
    Other,
}

impl DecisionCodeClass {
    pub const ALL: [Self; 9] = [
        Self::Absent,
        Self::C2000,
        Self::C2003,
        Self::C4007,
        Self::Other1xxx,
        Self::Other2xxx,
        Self::Other3xxx,
        Self::Other4xxx,
        Self::Other,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::C2000 => "2000",
            Self::C2003 => "2003",
            Self::C4007 => "4007",
            Self::Other1xxx => "other_1xxx",
            Self::Other2xxx => "other_2xxx",
            Self::Other3xxx => "other_3xxx",
            Self::Other4xxx => "other_4xxx",
            Self::Other => "other",
        }
    }

    pub fn from_code(code: Option<i64>) -> Self {
        match code {
            None => Self::Absent,
            Some(2000) => Self::C2000,
            Some(2003) => Self::C2003,
            Some(4007) => Self::C4007,
            Some(1000..=1999) => Self::Other1xxx,
            Some(2000..=2999) => Self::Other2xxx,
            Some(3000..=3999) => Self::Other3xxx,
            Some(4000..=4999) => Self::Other4xxx,
            Some(_) => Self::Other,
        }
    }
}

/// What a `decision_refused` report adds to the common context — all closed domains, none of the
/// server's sentence. Present only for a refusal the SERVER made at `/decision`; the app's own
/// policy refusals (Direct Play off, Force Direct Play) carry no server codes and no attempted
/// transcode, so they get none of it rather than a half-filled block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefusalContext {
    /// `generalDecisionCode` of the refusing `/decision`.
    pub general: DecisionCodeClass,
    /// `transcodeDecisionCode` of the same body — the lane that names the cause.
    pub transcode: DecisionCodeClass,
    /// The route the refused plan ASKED for. A separate field from `delivery`, which stays
    /// `unknown` because no route was installed: the attempt is recorded, the delivery is not.
    pub attempted: DeliveryClass,
    /// The SOURCE file's codecs (not the transcode's output, which a refusal never produced).
    pub source_video: VideoCodecClass,
    pub source_audio: AudioCodecClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityClass {
    Unknown,
    Auto,
    Original,
    K320,
    K720,
    M2,
    M4,
    M6,
    M8,
    M10,
    M12,
    M14,
    M16,
    M18,
    M20,
    M22,
}

impl QualityClass {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Auto => "auto",
            Self::Original => "original",
            Self::K320 => "320k",
            Self::K720 => "720k",
            Self::M2 => "2m",
            Self::M4 => "4m",
            Self::M6 => "6m",
            Self::M8 => "8m",
            Self::M10 => "10m",
            Self::M12 => "12m",
            Self::M14 => "14m",
            Self::M16 => "16m",
            Self::M18 => "18m",
            Self::M20 => "20m",
            Self::M22 => "22m",
        }
    }

    pub fn from_kbps(kbps: i64) -> Self {
        match kbps {
            320 => Self::K320,
            720 => Self::K720,
            2_000 => Self::M2,
            4_000 => Self::M4,
            6_000 => Self::M6,
            8_000 => Self::M8,
            10_000 => Self::M10,
            12_000 => Self::M12,
            14_000 => Self::M14,
            16_000 => Self::M16,
            18_000 => Self::M18,
            20_000 => Self::M20,
            22_000 => Self::M22,
            _ => Self::Unknown,
        }
    }
}

/// Privacy-preserving buckets for rates PMS actually declared or emitted. These are observations,
/// not controller rungs: keeping the type separate prevents a 5.5 Mbit/s server response from being
/// mislabeled as the 22 Mbit/s actuator that requested it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateClass {
    Unknown,
    Under1m,
    M1To3,
    M3To6,
    M6To12,
    M12To20,
    Over20m,
}

impl RateClass {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Under1m => "<1m",
            Self::M1To3 => "1-3m",
            Self::M3To6 => "3-6m",
            Self::M6To12 => "6-12m",
            Self::M12To20 => "12-20m",
            Self::Over20m => "20m+",
        }
    }

    pub fn from_kbps(kbps: i64) -> Self {
        match kbps {
            k if k <= 0 => Self::Unknown,
            1..=999 => Self::Under1m,
            1_000..=2_999 => Self::M1To3,
            3_000..=5_999 => Self::M3To6,
            6_000..=11_999 => Self::M6To12,
            12_000..=19_999 => Self::M12To20,
            _ => Self::Over20m,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterClass {
    Unknown,
    Sd,
    Hd,
    Fhd,
    Uhd,
}

impl RasterClass {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Sd => "sd",
            Self::Hd => "hd",
            Self::Fhd => "fhd",
            Self::Uhd => "uhd",
        }
    }

    pub fn from_height(height: i32) -> Self {
        match height {
            h if h <= 0 => Self::Unknown,
            h if h <= 576 => Self::Sd,
            h if h <= 720 => Self::Hd,
            h if h <= 1080 => Self::Fhd,
            _ => Self::Uhd,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceDirection {
    Up,
    Down,
    Refresh,
}

impl TraceDirection {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Refresh => "refresh",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryReason {
    LinkFallback,
    OriginalRecovery,
    OriginalOpenRollback,
}

impl DeliveryReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::LinkFallback => "link_fallback",
            Self::OriginalRecovery => "original_recovery",
            Self::OriginalOpenRollback => "original_open_rollback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginalProbePhase {
    // Retained stable wire vocabulary for events produced by builds before 2026-08-31. Current
    // runtime emits SampleSource only; removing/reusing these strings would rewrite dashboards.
    RetireHls,
    SampleSource,
    CloseSource,
    RestoreHls,
    OpenHls,
    CommitHls,
}

impl OriginalProbePhase {
    pub const fn code(self) -> &'static str {
        match self {
            Self::RetireHls => "retire_hls",
            Self::SampleSource => "sample_source",
            Self::CloseSource => "close_source",
            Self::RestoreHls => "restore_hls",
            Self::OpenHls => "open_hls",
            Self::CommitHls => "commit_hls",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceOutcome {
    Started,
    Succeeded,
    NoBody,
    Deadline,
    Transport,
    /// The app observed failure but the available signal does not distinguish a moved local
    /// session, a missing client or another control-plane circumstance.
    Inconclusive,
    ServerState,
    Refused,
}

impl TraceOutcome {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Succeeded => "succeeded",
            Self::NoBody => "no_body",
            Self::Deadline => "deadline",
            Self::Transport => "transport",
            Self::Inconclusive => "inconclusive",
            Self::ServerState => "server_state",
            Self::Refused => "refused",
        }
    }
}

/// **How long a native Load spent in flight, as a bucket — never the millisecond count.**
///
/// Recorded once per attempt, either when `player::threads::load_thread`'s Load-returned gate
/// opens (the ordinary case) or when issue #74 D.1.4's `player::pump::NATIVE_LOAD_BUDGET` fires
/// first (the k5lp hang this bucket exists to make visible on a dashboard rather than only in a
/// device log). A duration is exactly the kind of measurement `PlaybackErrorContext`'s other
/// fields refuse to carry verbatim — see the module's bucket rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadElapsedClass {
    Under1s,
    S1To5,
    S5To20,
    Over20s,
}

impl LoadElapsedClass {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Under1s => "under_1s",
            Self::S1To5 => "1_to_5s",
            Self::S5To20 => "5_to_20s",
            Self::Over20s => "over_20s",
        }
    }

    pub fn from_ms(ms: i64) -> Self {
        match ms.max(0) {
            0..=999 => Self::Under1s,
            1_000..=4_999 => Self::S1To5,
            5_000..=19_999 => Self::S5To20,
            _ => Self::Over20s,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceEvent {
    Requested {
        selected: QualityClass,
    },
    Presented {
        delivery: DeliveryClass,
        requested: QualityClass,
        declared_rate: RateClass,
        raster: RasterClass,
    },
    SeekRequested,
    QualitySelected {
        selected: QualityClass,
    },
    DeliveryRequested {
        delivery: DeliveryClass,
        requested: QualityClass,
        reason: DeliveryReason,
    },
    HlsCommitted {
        direction: TraceDirection,
        requested: QualityClass,
    },
    OriginalProbe {
        phase: OriginalProbePhase,
        outcome: TraceOutcome,
    },
    /// The native Load-returned gate opened, or issue #74 D.1.4's budget fired first — see
    /// [`LoadElapsedClass`].
    LoadGateOpened {
        elapsed: LoadElapsedClass,
    },
    Failed {
        kind: FailureClass,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceStep {
    pub age: TraceAge,
    pub event: TraceEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineClass {
    Loading,
    Playing,
    Bound,
    Streaming,
}

impl PipelineClass {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Playing => "playing",
            Self::Bound => "bound",
            Self::Streaming => "streaming",
        }
    }

    pub fn from_stage(stage: u8) -> Self {
        match stage {
            1 => Self::Playing,
            2 => Self::Bound,
            3 => Self::Streaming,
            _ => Self::Loading,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpClass {
    None,
    Success,
    ClientError,
    ServerError,
    Other,
}

impl HttpClass {
    pub const fn code(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Success => "2xx",
            Self::ClientError => "4xx",
            Self::ServerError => "5xx",
            Self::Other => "other",
        }
    }

    pub fn from_status(status: i32) -> Self {
        match status {
            0 => Self::None,
            200..=299 => Self::Success,
            400..=499 => Self::ClientError,
            500..=599 => Self::ServerError,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferClass {
    Unknown,
    Empty,
    Under3s,
    S3To10,
    S10To30,
    Over30s,
}

impl BufferClass {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Empty => "empty",
            Self::Under3s => "<3s",
            Self::S3To10 => "3-10s",
            Self::S10To30 => "10-30s",
            Self::Over30s => "30s+",
        }
    }

    pub fn from_ms(ms: i64) -> Self {
        match ms {
            m if m < 0 => Self::Unknown,
            0 => Self::Empty,
            1..=2_999 => Self::Under3s,
            3_000..=9_999 => Self::S3To10,
            10_000..=29_999 => Self::S10To30,
            _ => Self::Over30s,
        }
    }
}

/// **Why the media transfer died, as a class — never the libcurl number.** Issue #504: a direct
/// play over https that stopped exactly a minute in reached Sentry as a bare `playback_interrupted`
/// and the event log said only that the demuxer's read failed; the `CURLcode` that explains it was
/// kept and never reported. The classes are the codes a mid-body death actually produces; a code
/// outside them is `other`, which cannot carry the number it replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportClass {
    /// No transport failure was recorded (the failure was not a curl transfer's).
    None,
    /// `CURLE_PARTIAL_FILE` (18): the connection closed before the whole body arrived.
    Partial,
    /// `CURLE_RECV_ERROR` (56): the connection was reset or dropped while receiving.
    RecvError,
    /// `CURLE_SEND_ERROR` (55).
    SendError,
    /// `CURLE_GOT_NOTHING` (52): the server closed without answering.
    GotNothing,
    /// `CURLE_OPERATION_TIMEDOUT` (28), the 30 s low-speed floor included.
    Timeout,
    /// `CURLE_HTTP2` (16) or `CURLE_HTTP2_STREAM` (92).
    Http2,
    /// The transfer died and the reopen that follows a failed read (libavformat's `seek_cb` heal)
    /// was refused too: the cause of the first death is in the event log, not here.
    ReopenFailed,
    /// Any other `CURLcode`.
    Other,
}

impl TransportClass {
    pub const ALL: [Self; 9] = [
        Self::None,
        Self::Partial,
        Self::RecvError,
        Self::SendError,
        Self::GotNothing,
        Self::Timeout,
        Self::Http2,
        Self::ReopenFailed,
        Self::Other,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Partial => "partial",
            Self::RecvError => "recv_error",
            Self::SendError => "send_error",
            Self::GotNothing => "got_nothing",
            Self::Timeout => "timeout",
            Self::Http2 => "http2",
            Self::ReopenFailed => "reopen_failed",
            Self::Other => "other",
        }
    }

    /// The class of a `CURLcode`; `0` is no failure.
    pub const fn from_curl_rc(rc: i32) -> Self {
        match rc {
            0 => Self::None,
            18 => Self::Partial,
            56 => Self::RecvError,
            55 => Self::SendError,
            52 => Self::GotNothing,
            28 => Self::Timeout,
            16 | 92 => Self::Http2,
            _ => Self::Other,
        }
    }
}

/// **How long the failed transfer had lived, as a bucket — never the millisecond count.** A proxy
/// or middlebox that cuts every transfer at a fixed time shows up as one bucket on every report
/// (30-90 s for a minute), which is the pattern that names it. `Unknown` is "no transport failure
/// was recorded", so the bucket never claims an age nobody measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferAgeClass {
    Unknown,
    Under5s,
    S5To30,
    S30To90,
    S90To300,
    Over300s,
}

impl TransferAgeClass {
    pub const ALL: [Self; 6] = [
        Self::Unknown,
        Self::Under5s,
        Self::S5To30,
        Self::S30To90,
        Self::S90To300,
        Self::Over300s,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Under5s => "<5s",
            Self::S5To30 => "5-30s",
            Self::S30To90 => "30-90s",
            Self::S90To300 => "90-300s",
            Self::Over300s => "300s+",
        }
    }

    pub const fn from_ms(ms: i64) -> Self {
        match ms {
            m if m < 0 => Self::Unknown,
            0..=4_999 => Self::Under5s,
            5_000..=29_999 => Self::S5To30,
            30_000..=89_999 => Self::S30To90,
            90_000..=299_999 => Self::S90To300,
            _ => Self::Over300s,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackErrorContext {
    pub delivery: DeliveryClass,
    pub selected: QualityClass,
    pub requested: QualityClass,
    pub declared_rate: RateClass,
    pub media_rate: RateClass,
    pub raster: RasterClass,
    pub pipeline: PipelineClass,
    pub http: HttpClass,
    pub buffer: BufferClass,
    /// Why the media transfer died, when a curl transfer's death was recorded.
    pub transport: TransportClass,
    /// How long that transfer had lived.
    pub transfer_age: TransferAgeClass,
    pub started: bool,
    /// Only for a refusal the server made at `/decision` — see [`RefusalContext`].
    pub refusal: Option<RefusalContext>,
}

/// The video codec, as a CLOSED domain.
///
/// `route::stream_vcodec` hands back a `String` off the wire, and `diag::schema` has no arm that
/// could carry one — deliberately, that being the property that makes "no runtime string reaches
/// the wire" a fact about the type. So the mapping is here: a name the table does not know becomes
/// `other`, which is a real answer (it means the server sent something this app did not expect) and
/// cannot become a leak. ONE table serves the usage funnel's `playback.started` and the handled
/// error report's source codecs, so the two cannot disagree about what "hevc" is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodecClass {
    Unknown,
    H264,
    Hevc,
    Av1,
    Vp9,
    Mpeg2,
    Other,
}

impl VideoCodecClass {
    pub const ALL: [Self; 7] = [
        Self::Unknown,
        Self::H264,
        Self::Hevc,
        Self::Av1,
        Self::Vp9,
        Self::Mpeg2,
        Self::Other,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
            Self::Vp9 => "vp9",
            Self::Mpeg2 => "mpeg2",
            Self::Other => "other",
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "h264" | "avc" | "avc1" => Self::H264,
            "hevc" | "h265" | "hvc1" => Self::Hevc,
            "av1" => Self::Av1,
            "vp9" => Self::Vp9,
            "mpeg2video" | "mpeg2" => Self::Mpeg2,
            "" => Self::Unknown,
            _ => Self::Other,
        }
    }
}

/// The audio codec, as a closed domain, for [`VideoCodecClass`]'s reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodecClass {
    Unknown,
    Aac,
    Ac3,
    Eac3,
    TrueHd,
    Dts,
    Flac,
    Mp3,
    Opus,
    Other,
}

impl AudioCodecClass {
    pub const ALL: [Self; 10] = [
        Self::Unknown,
        Self::Aac,
        Self::Ac3,
        Self::Eac3,
        Self::TrueHd,
        Self::Dts,
        Self::Flac,
        Self::Mp3,
        Self::Opus,
        Self::Other,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Aac => "aac",
            Self::Ac3 => "ac3",
            Self::Eac3 => "eac3",
            Self::TrueHd => "truehd",
            Self::Dts => "dts",
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
            Self::Opus => "opus",
            Self::Other => "other",
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "aac" => Self::Aac,
            "ac3" => Self::Ac3,
            "eac3" | "ac3 plus" | "ec-3" => Self::Eac3,
            "truehd" => Self::TrueHd,
            "dts" | "dca" => Self::Dts,
            "flac" => Self::Flac,
            "mp3" => Self::Mp3,
            "opus" => Self::Opus,
            "" => Self::Unknown,
            _ => Self::Other,
        }
    }
}

/// **Why a playback failed, as the wire's closed domain.** The player decides the cause
/// (`player::FailureKind`, which also drives the read-out's wording) and converts it here with
/// `FailureKind::class`; the code is the stable value `playback.failed`'s `kind` field and the
/// handled error's `playback.kind` tag carry.
///
/// One historical code, [`OriginalRollback`](Self::OriginalRollback), remains so old telemetry
/// fixtures and dashboards retain their meaning after the destructive probe transaction was
/// removed; no live path emits it now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// `/decision` refused the item outright.
    DecisionRefused,
    /// The explicit Direct Play policy cannot deliver the requested original stream.
    PlaybackPolicy,
    /// Transcoding, and the server produced no video stream.
    NoVideoTranscodeTarget,
    /// Direct playing, and the stream carries no video track.
    NoVideoTrack,
    /// The producer never opened a usable media stream or produced a video access unit.
    MediaSource,
    /// The media producer stopped after playback had already begun.
    PlaybackInterrupted,
    /// Starfish refused the Load declaration, so no decoder session could start.
    TvPipeline,
    /// Historical telemetry only: the retired exclusive Original experiment lost its HLS rollback.
    OriginalRollback,
    /// This device's jail is missing `/dev/rtkmem` on a SoC where that is a known cause of native
    /// A/V crashes — the Load was never attempted.
    JailMissingRtkmem,
    /// The native `Load` budget fired: the call never returned, or `loadCompleted` never arrived.
    LoadTimeout,
    /// Everything else. Honest rather than tidy.
    Unspecified,
}

impl FailureClass {
    /// Every class, so a test can hold the declared `playback.failed` domain to the enum it is
    /// built from.
    #[cfg(any(test, feature = "test-support"))]
    pub const ALL: [Self; 11] = [
        Self::DecisionRefused,
        Self::PlaybackPolicy,
        Self::NoVideoTranscodeTarget,
        Self::NoVideoTrack,
        Self::MediaSource,
        Self::PlaybackInterrupted,
        Self::TvPipeline,
        Self::OriginalRollback,
        Self::JailMissingRtkmem,
        Self::LoadTimeout,
        Self::Unspecified,
    ];

    /// The stable wire code. Written out rather than derived from the variant name, because a
    /// rename is a refactor and must not silently re-partition a year of dashboards.
    pub const fn code(self) -> &'static str {
        match self {
            Self::DecisionRefused => "decision_refused",
            Self::PlaybackPolicy => "playback_policy",
            Self::NoVideoTranscodeTarget => "no_video_transcode_target",
            Self::NoVideoTrack => "no_video_track",
            Self::MediaSource => "media_source",
            Self::PlaybackInterrupted => "playback_interrupted",
            Self::TvPipeline => "tv_pipeline",
            Self::OriginalRollback => "original_rollback",
            Self::JailMissingRtkmem => "jail_missing_rtkmem",
            Self::LoadTimeout => "load_timeout",
            Self::Unspecified => "unspecified",
        }
    }
}
