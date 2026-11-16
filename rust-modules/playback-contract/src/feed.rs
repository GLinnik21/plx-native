//! The buffer-feed seam between the player's common pump and the physical feed backend.
//!
//! Plain data only: the Load payload's facts, the typed verdicts one pump tick returns, and the
//! traits through which the player drives a feed backend. The backend's own state (its native
//! session, its worker threads, its callback mailboxes) never crosses this module; a verdict is
//! returned by value and the caller applies it.
//!
//! [`PhysicalSet`] is the one object the player adapter holds for every physical executor it can
//! drive. Its `feed` answers the buffer-feed half as a [`FeedControl`], or `None` when the
//! composition installed no feed (the default, [`UriOnlySet`]), so the player runs without naming
//! the crate that implements the feed.
//!
//! # Facts: the mapped readout and the raw one
//!
//! What the feed observed reaches the player as [`FeedFacts`]: the sparse [`AttemptFacts`] the
//! HUD and the route already read (`None` is "not known yet"), and a [`FeedRaw`] holding the values
//! whose `None` or collapsed form would change a decision. The audit of every member the player
//! reads (D2 v2, 1.3; counts are the production reads in `media/src`):
//!
//! | member (reads) | mapped as | decision |
//! |---|---|---|
//! | `playpos_ns` (10) | `position_ns = (reported_frame \|\| pos > 0).then_some(pos.max(0))`: a reset zeroes it, so before the first frame a non-zero value is a seeded target (`arm_open`, `rebase`) or a seek's | **raw**: the open-recovery resume offset (`pump.rs` `drive_open_recovery`) reads the store before any frame, whether or not this session's open published a seed |
//! | `frames` (6) | `reported_frame.then_some(frames)` | **raw**: `== 0` and `> 0` tests where "no callback yet" differs from "0 frames" whenever frames > 0 arrive without a frame callback (not proven impossible) |
//! | `load_failed`, `load_timed_out`, `demux_failed`, `demux_io_failed` (10) | one collapsed `runtime_failure` (`play-feed/src/readout.rs`) | **raw**: [`FailureFlags`]; readers test single flags and `live_runtime_failure` re-derives the collapse in media. The player's own notes of one are [`FeedCommand::NoteFailure`]: the flag is written inside the feed and returns through the next sample |
//! | `duration_ns` (5) | `(d > 0).then_some(d)` | **mapped**, `unwrap_or(0)`: every reader tests `> 0` (`timeline.rs` `from_content_ns`, `pump.rs` `stream_ended` and the `> 0` seek gate, `route_start.rs` `final_report`) or passes it to `report::ended`; the store is 0 or positive (`reset` stores 0), so `<= 0` never carries a meaning `None` loses |
//! | `dg_http_status` (3), `dg_cb_err_num` (1) | `> 0` and `!= 0` gates | **mapped** in `AttemptFacts`, and **raw** beside the flags ([`FeedRaw::http_status`], [`FeedRaw::cb_err_num`], 0 = none): the failure drains read a flag and its code from ONE sample, and a reload's session reset clears the raw one with the mirror it mirrors |
//! | `hls_rebuffering` (1), `seen_frame` (1) | plain bool | **mapped** |
//! | `native_audio_refused`, `decoder_revoked` | one-shot takes | **raw counters** ([`FeedRaw::native_audio_refusals`], [`FeedRaw::decoder_revocations`]): the feed counts each edge, the player's readout holds how many it has taken |
//! | `file_size`, `applied_rate_permille`, `speed_audio_verdict`, `dg_vpres_ct`, `dg_vpres_gap_sum`, `dg_vpres_gap_ct`, `pts_shift` | no `AttemptFacts` field | **raw**, in [`FeedRaw`]; the three presentation counters are diagnostics only (the heartbeat's liveness line differences two samples: the tick count, and the mean gap as sum over gap count) |
//! | `stage`, `load_thread_finished`, `route_actions_ready` (the Engine's own fields, not mailboxes) | none | **raw**, in [`LoadRaw`] (`FeedRaw::load`), which `FeedBackend::sample` fills beside the mailbox half: a `None` stage, a finished Load thread and "not ready" each mean something the idle default would lose, and the session's reset returns them to [`LoadRaw::IDLE`] |
//! | `native_load_abandoned` | the Load a teardown parked | **terminal fact**: `AttemptFacts::load_abandoned`, recorded by the teardown's own ingestion, because the live phase outlives the session reset and the readout does not |
//!
//! The seek family is here: `pipeline_eos`, the landed seek epoch, the feed's in-place target
//! (`seek_target_ns`) and the epoch'd `seek_requeue` are raw facts (`seeking` is not stored
//! anywhere: the player derives it from the epoch it requested and the landed one, and the pending
//! target and the HUD's display target are the player's own readout), and the requests themselves
//! are the intents [`FeedCommand::Seek`], [`FeedCommand::SeekTaken`], [`FeedCommand::SeekRequeued`],
//! [`FeedCommand::AbandonSeek`] and [`FeedCommand::ArmOpen`]. `disp_base` is raw too
//! ([`FeedRaw::disp_base`]): the feed zeroes it on its own seek and on every reset, so the player reads it
//! back from the sample, and a transcode restart's new base is the intent [`FeedCommand::Rebase`]. The
//! feed-written diagnostics are [`FeedRaw::diag`] ([`FeedDiag`]) and the demux's Auto-quality facts are
//! [`FeedRaw::abr`] ([`DemuxAbr`]); the adaptive worker's are not the feed's at all
//! ([`AbrReadout`](crate::adaptive::AbrReadout), sent by its [`AdaptiveEffects`](crate::adaptive::AdaptiveEffects)).
//! A value that is `-1` in the feed's mailbox is an `Option` here, as in [`AttemptFacts`].

use std::sync::atomic::{AtomicI32, AtomicU32};
use std::time::Duration;

use plx_base::task::MainThread;

use crate::adaptive::{AdaptiveSession, OriginalSession};
use crate::demux::DemuxSession;
use crate::readout::AttemptFacts;
use crate::{
    AttemptId, BackendEvent, BackendKind, Capabilities, CommandId, DispatchResult, DriverSet,
    ExitDisposition, Failure, Operation, SourceKey, UriDriver,
};

// ---------------------------------------------------------------------------------------------
// Load payload facts (resolved by the route, consumed by the native Load declaration).
// ---------------------------------------------------------------------------------------------

/// The PCM a Load declares for audio the app decodes itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcmFormat {
    pub sample_rate: u32,
    pub bits: u32,
    pub channels: u32,
    pub source_rate: u32,
}

/// The Dolby Vision node a Load declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DolbyHdrInfo {
    pub profile_id: i64,
    pub track_type: &'static str,
    pub encryption_type: &'static str,
}

/// Facts frozen by the common route adapter before native Load construction.
pub struct PayloadFacts {
    pub pcm: Option<Result<PcmFormat, String>>,
    pub max_raster: (u16, u16),
    pub fps: f64,
    pub dv: Option<DolbyHdrInfo>,
    pub immersive: bool,
    pub force_direct_play: bool,
}

#[derive(Clone, Copy)]
pub struct PayloadOptions {
    pub with_fps: bool,
    pub suppress_dv_node: bool,
}

/// Evidence about the unchanged source wire, separate from the native Load presentation.
#[derive(Clone, Copy)]
pub struct OriginalDvFacts {
    pub present: bool,
    pub profile: i64,
    pub bl_compat: i64,
    pub el_present: bool,
}

/// What the Dolby Vision landing guard (`plx_play_feed::dv_start`, #1166) does for one open, decided
/// once from the route.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DvLandingMode {
    /// Nothing: no node is sent, or the play is a trailer preview.
    #[default]
    Inert,
    /// The node is sent and no rung exists for the route (a Profile 5 direct play, or a remux that
    /// copies a declared video): the guard says so in the log and changes nothing.
    ReportOnly,
    /// Withdraw the declaration at a landing the firmware's splitter would skip, or after a silent one.
    Act,
}

/// The route's half of the Dolby Vision landing guard for one open: plain data, so the Feed builds
/// its guard from it at the Engine's construction without naming the route.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DvLandingPlan {
    pub mode: DvLandingMode,
    /// The attempt's firmware row says the splitter lets only an HEVC IDR be the first picture after
    /// a flush (`Caps::dv_first_picture`).
    pub idr_first: bool,
}

impl DvLandingPlan {
    /// No guard: the plan of an open whose Load sends no `DolbyHdrInfo`.
    pub const INERT: Self = Self { mode: DvLandingMode::Inert, idr_first: false };
}

/// A withdrawal [`FeedLoad::dv_landing_due`] hands the player: take the declaration out of the route,
/// then reload once without the node. The reload's target is the player's to choose (the newest tap
/// else the seek's own, else `armed_target`, else `kf_pts`), so the facts it chooses by are here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DvWithdrawal {
    /// The reason the route logs with the withdrawal itself.
    pub why: String,
    /// The landing keyframe's content pts (ns).
    pub kf_pts: i64,
    /// The seek target the guard was armed with: an Engine started with a seek armed, or the in-place
    /// seek's target (`None` for a plain start).
    pub armed_target: Option<i64>,
    /// The landing was an in-place seek of a live Engine.
    pub in_place: bool,
}

// ---------------------------------------------------------------------------------------------
// The native open: what the route freezes for one Load, handed to the feed in one request.
// ---------------------------------------------------------------------------------------------

/// A server's address as the feed dials it: the transport's scheme, the unbracketed host and the
/// port. Exactly the fields of the network layer's origin, without naming that crate.
#[derive(Clone, PartialEq, Eq)]
pub struct OpenOrigin {
    pub tls: bool,
    pub host: String,
    pub port: i32,
}

/// The resolved facts of one streamed open. Route selection and credential authorization finished
/// before this was built. URLs have no Debug surface; the demux session is bound later, by
/// [`FeedOpen::open_stage`], because the route attempt and the native epoch do not exist yet.
pub struct OpenStream {
    pub origin: OpenOrigin,
    pub path: String,
    pub video_codec: String,
    pub audio_codec: String,
    pub pcm_is_lossy: bool,
    pub audio_channels: i64,
    pub feed_payload: PayloadFacts,
    pub payload_options: PayloadOptions,
    pub original_dv: OriginalDvFacts,
    pub hls_abr: Option<Box<dyn AdaptiveSession>>,
    pub auto_original: Option<Box<dyn OriginalSession>>,
}

/// A local validation sample the feed loops over instead of a stream.
pub struct SampleBuf {
    pub data: Vec<u8>,
    pub au: Vec<usize>, // AU start offsets
    pub next: usize,
    pub loops: i64,
}

impl SampleBuf {
    /// The local validation sample, positioned at its first AU.
    pub fn new(data: Vec<u8>, au: Vec<usize>) -> Self {
        Self { data, au, next: 0, loops: 0 }
    }
}

/// split Annex-B into AUs on the 5-byte AUD prefix 00 00 00 01 <aud5>
/// (H264 AUD = 0x09; HEVC AUD is NAL type 35 → first header byte 0x46).
pub fn bf_split(data: &[u8], aud5: u8) -> Vec<usize> {
    let mut au = Vec::new();
    let mut i = 0usize;
    while i + 4 < data.len() {
        if data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
            && data[i + 4] == aud5
        {
            au.push(i);
            i += 4;
        }
        i += 1;
    }
    au
}

/// What the route decided about the side subtitle reader for one open: whether a reader is
/// admitted for it, whether it runs clock-only (a sidecar the app draws itself), and the
/// [`Spec`] it starts with (`None` when the target's server is gone). Plain data: the Feed
/// builds the reader's port from it ([`OpenRequest::side_reader`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SideReaderRequest {
    /// A reader is admitted for this open.
    pub selected: bool,
    /// The reader reads no subtitle and only measures the remux's clock
    /// ([`crate::subtitle::SIDE_READER_CLOCK_ONLY`]).
    pub clock_only: bool,
    pub spec: Option<crate::subtitle::Spec>,
}

/// Everything [`FeedOpen::open_prepare`] consumes. `stream` is `Some` for a stream and `sample`
/// for a local sample; never both.
pub struct OpenRequest {
    /// The goal pause the Load starts under.
    pub start_paused: bool,
    pub seek_preroll: bool,
    pub stream: Option<OpenStream>,
    pub sample: Option<Box<SampleBuf>>,
    pub is_h265: bool,
    pub session_rate: i32,
    /// The viewer's requested playback rate, in permille.
    pub requested_rate: i32,
    pub no_audio: bool,
    pub side_reader: SideReaderRequest,
    /// The Dolby Vision landing guard this open's Engine starts with ([`DvLandingPlan::INERT`] for a
    /// sample, which carries no node).
    pub dv_landing: DvLandingPlan,
}

/// A native transport action the feed's driver queued for the player's transport door.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedTransportAction {
    /// Release the captured viewer hold before the initial physical prime may Play.
    ResumeInitial { attempt: AttemptId, command: CommandId },
    Play { attempt: AttemptId, command: CommandId },
    Pause { attempt: AttemptId, command: CommandId },
}

// ---------------------------------------------------------------------------------------------
// Verdicts: what one physical tick or one native transition answers, applied by the caller.
// ---------------------------------------------------------------------------------------------

/// Whether native construction is callable yet. While a blocked Load's live non-preview session
/// still owes the Load budget check, the feed runs that check itself, on its own clock, and says
/// here when the budget ran out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtorGate {
    Ready,
    Awaiting { timeout: Option<LoadTimeout> },
}

/// What a stuck native seek does next: re-arm the demux target (the retry count so far) or fall
/// back to a full reload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StuckSeekAction { Retried(i32), Reload }

/// The typed answer of one native Pause transition before an in-place seek; the route owner
/// chooses any fallback reload from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekPause { Ready, Refused, Stale, Busy }

/// The pump's half of a paused seek, answered each tick: whether the preroll landed and the
/// re-pause is due, and whether a mid-seek claim holds the pause.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PausedSeekStep {
    pub finish: bool,
    pub mid_seek_claimed: bool,
    /// The feed opened (or retargeted) the paused-seek preroll in this very call. The feed is the
    /// writer of that open, so it says so: the player's readout steps `BeginPausedSeek` from this
    /// verdict BEFORE it acts on `finish`, which the same call may also report. A sampled level
    /// would read the pre-open value there.
    pub preroll_opened: bool,
}

/// A Load that did not return in time: after how long, or because its native epoch left the
/// active state while the result was still deferred.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadTimeout { Elapsed(Duration), LostEpoch }

/// What one Load tick found: a timeout to attribute to the route attempt, and a speed-path
/// fallback notice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadTick {
    pub timeout: Option<LoadTimeout>,
    pub speed_fallback: Option<&'static str>,
}

/// The physical presentation state after one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentationTick {
    pub stage: Stage,
    pub eos_pushed: bool,
}

/// One accepted Load verdict, addressed to the attempt that issued it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcceptedLoadOutcome {
    pub attempt: AttemptId,
    pub ok: bool,
    pub elapsed_ms: Option<u64>,
}

/// How a direct-play seek re-anchors the pipeline for one session: an in-place segment seek,
/// the platform's public native `Seek`, or a full reload.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeekMode {
    Segment,
    NativeSeek,
    Reload,
}

/// The outcome of an HLS pause the backend issued (the handshake completion).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsPauseCompletion {
    Accepted,
    Refused,
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsUserPause {
    Issue(u64),
    AlreadyHeld,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsUserResume {
    Issue(u64),
    Deferred,
    Prime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsPlayCompletion {
    Accepted { resume_acb: bool },
    Refused,
    Stale,
}

/// What the feed did with a viewer's Pause (the handshake the player ran itself before the feed
/// owned it). Every variant but `Deferred` and a refused or stale `Issued` has committed the
/// transport Paused: the feed gate and the pause clock moved at the same accepted boundary as the
/// native Pause, and the player mirrors the ACB playstate after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserPauseOutcome {
    /// An internal clock hold already pauses the pipeline: nothing native was issued.
    AlreadyHeld,
    /// A native Pause was issued and the clock transition completed as `completion` says.
    Issued(HlsPauseCompletion),
    /// An in-flight clock transition owns the clock: the Pause waits, nothing changed.
    Deferred,
}

/// What the feed did with a viewer's Resume. Every variant but the three that name a refusal
/// committed the transport Playing (and closed any paused-seek preroll that was open).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserResumeOutcome {
    /// Feeding resumes, but an initial, seek or recovery certificate still owns the physical
    /// clock: its eventual Play carries the pending ACB Resume.
    Fenced,
    /// Feeding resumes and the exact balanced prime owns the eventual Play and ACB Resume.
    Priming,
    /// A queued Resume found no engine to arm the prime on: nothing changed.
    PrimeLost,
    /// A native Play was issued and completed (`resume_acb`: the matching ACB Resume is owed).
    Played { resume_acb: bool },
    /// The pipeline refused the native Play: nothing changed.
    PlayRefused,
    /// The Play lost its clock token to a newer transition: nothing changed.
    PlayStale,
    /// The viewer cancelled "stay paused" after the seek handed the physical hold to its prime:
    /// no native command, the prime owns Play.
    PrerollCancelled,
    /// There is no accepted user hold to release: nothing changed.
    NoHold,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// **`Bound` is the ACB bind, and the webOS 5 path SKIPS it** — `VP_EXPORTED` goes
/// `Playing -> Streaming` directly, because there is no bind sequence to be in the middle of.
/// So `stage >= Bound` means "bound or later" on webOS 4 and "is Streaming" on webOS 5; a new
/// test against it has to decide which of those it wants.
pub enum Stage {
    Loading = 0,
    Playing,
    Bound,
    Streaming,
}

// ---------------------------------------------------------------------------------------------
// Facts: what the feed observed, by value.
// ---------------------------------------------------------------------------------------------

/// The four runtime failure flags as the feed holds them, before `runtime_failure` collapses them
/// by precedence. Readers test one flag, so the collapse is re-derived in the player.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FailureFlags {
    /// The pipeline refused the Load, or its Load never returned.
    pub load_failed: bool,
    /// Set beside `load_failed` only by the Load budget expiring: a hang, not a refusal.
    pub load_timed_out: bool,
    /// The producer died before publishing a duration.
    pub demux_failed: bool,
    /// The media transport failed after open, possibly after frames were presented.
    pub demux_io_failed: bool,
}

/// One of the failure flags the player can note ([`FeedCommand::NoteFailure`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureFlag {
    LoadFailed,
    LoadTimedOut,
    DemuxFailed,
    DemuxIoFailed,
}

impl FailureFlags {
    /// `flag` raised.
    pub fn set(&mut self, flag: FailureFlag) {
        match flag {
            FailureFlag::LoadFailed => self.load_failed = true,
            FailureFlag::LoadTimedOut => self.load_timed_out = true,
            FailureFlag::DemuxFailed => self.demux_failed = true,
            FailureFlag::DemuxIoFailed => self.demux_io_failed = true,
        }
    }
}

/// The feed's observations whose `AttemptFacts` form would lose what a decision reads (the
/// module doc audits each member). A fresh `Default` is a feed with no session: nothing seen,
/// every counter 0, 1x applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedRaw {
    /// The reported playhead in content time, before any frame has been shown.
    pub playpos_ns: i64,
    /// Frames the pipeline reported, 0 before any.
    pub frames: i32,
    pub flags: FailureFlags,
    /// How many times the pipeline took the video decoder back from an established session this
    /// session; the player holds how many of them it has taken (an edge, not a level).
    pub decoder_revocations: u32,
    /// How many times the pipeline refused natively fed audio this session (the refusal callback or
    /// the stall watchdog raised it); taken like [`Self::decoder_revocations`].
    pub native_audio_refusals: u32,
    /// The source's size in bytes; 0 while unknown.
    pub file_size: i64,
    /// The speed this session's Load plays at, 1000 = 1x.
    pub applied_rate_permille: i32,
    /// What the pipeline reported for a speed session's audio: `None` before any report,
    /// `Some(true)` for the declared 48 kHz stereo, `Some(false)` otherwise.
    pub speed_audio_ok: Option<bool>,
    /// Native presentation callbacks seen this session.
    pub vpres_count: u32,
    /// The shift between the container's timestamps and the pipeline's.
    pub pts_shift: i64,
    /// The pipeline reported its own end of stream after a picture; cleared by a seek and by
    /// the session's reset.
    pub pipeline_eos: bool,
    /// The newest seek epoch (the one a [`FeedCommand::Seek`] carried) whose playback has resumed
    /// at its new position; 0 before any, and again after the session's reset. A seek is in
    /// flight while the player's requested epoch is above this one.
    pub seek_landed_epoch: u32,
    /// Where the feed's last in-place seek was aimed (what the demuxer re-opens at); `None` before
    /// any seek this session.
    pub seek_target_ns: Option<i64>,
    /// The feed's own request to seek again: a refused native `Seek` leaves the pipeline flushed and
    /// the feed asks the player to redo that seek as a reload. See [`SeekRequeue`].
    pub seek_requeue: SeekRequeue,
    /// The firmware's error number of the first `type=18` callback, 0 = none. Loaded after the
    /// failure flags: its writer publishes it before the flag it explains.
    pub cb_err_num: i32,
    /// The HTTP status of the last failed transport request, 0 = none; loaded like
    /// [`Self::cb_err_num`].
    pub http_status: i32,
    /// The codec the file ACTUALLY carries, in the Load-declaration code space, recorded by the
    /// demuxer when it refuses a Load whose bytes disagree with its declared codec (a stale PMS
    /// track list, or a transcode the route described from the source's record); `0` = no
    /// disagreement on that Load. Published before the `demux_failed` flag it explains; the
    /// player's one-shot probe retry is its only consumer, and the session reset clears it.
    pub probe_actual_video: u8,
    pub probe_actual_audio: u8,
    /// What the Stats-for-nerds read-out shows of the feed's own mailboxes, from the same sample.
    pub diag: FeedDiag,
    /// The summed length (ms, wrapping) of the gaps between consecutive native presentation
    /// callbacks this session, and how many gaps that is. A gap is counted only between two
    /// callbacks with no stamp reset between them, so the count is NOT [`Self::vpres_count`].
    pub vpres_gap_sum_ms: u32,
    pub vpres_gap_count: u32,
    /// The offset the published position carries (`playpos = fed - pts_shift + disp_base`): a
    /// transcode restart's content offset, 0 once the feed rebases onto a landed keyframe and after
    /// the session's reset.
    pub disp_base: i64,
    /// What the live Engine's Load side reads, taken in the same sample ([`LoadRaw`]).
    pub load: LoadRaw,
    /// The Auto-quality facts the demux publishes ([`DemuxAbr`]).
    pub abr: DemuxAbr,
    /// The side subtitle reader's thread is running (the draw gate reads it for the dev trigger's run).
    pub side_reader_active: bool,
    /// The reader's give-ups this session that no stop has voided (`0` = none): a counter like
    /// [`Self::decoder_revocations`], so the player holds how many it has taken. A failure the stopped
    /// reader raised is voided by that stop, and reads `0` again.
    pub side_failures: u32,
    /// The clock delta the reader matched byte for byte for this engine run (Part time minus the
    /// playhead, ns); `None` while none has been measured.
    pub side_exact_delta_ns: Option<i64>,
    /// The audio the demuxer feeds the pipeline natively (the TV decodes the codec itself), once it
    /// has said so; `None` before that and for any track the TV decodes by name alone or the app
    /// decodes. The player reads it to tell a native audio refusal from any other.
    pub native_audio: Option<NativeAudio>,
    /// This session's direct-play seek mode (`Segment` before a Load selects one); the feed
    /// downgrades it to `Reload` when the pipeline refuses a native `Seek`.
    pub seek_mode: SeekMode,
}

/// The audio a session feeds the pipeline natively: the codec's FFmpeg name and the track's channel
/// count ([`FeedRaw::native_audio`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeAudio {
    pub codec: &'static str,
    pub channels: u32,
}

/// The Auto-quality facts the demux publishes about the open it runs: which controller started, the
/// rung it requested and the master playlist's declaration for it. The adaptive worker's own
/// measurements are the [`AbrReadout`](crate::adaptive::AbrReadout), which reaches the player on its
/// own path; the diagnostics combine the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DemuxAbr {
    /// `readout::ABR_MODE_*`: `0` inactive, 1 the progressive Original watchdog, 2 the fixed-session
    /// HLS controller.
    pub mode: u8,
    /// The Original source requirement or the active HLS rung, in kbit/s; `0` before a controller
    /// started.
    pub kbps: i64,
    /// The current HLS master declaration, in kbit/s; `-1` while none is known.
    pub declared_kbps: i64,
}

impl DemuxAbr {
    /// No controller: what a feed with no session publishes.
    pub const IDLE: Self = Self { mode: 0, kbps: 0, declared_kbps: -1 };
}

impl Default for DemuxAbr {
    fn default() -> Self { Self::IDLE }
}

/// The Load-side facts of the feed's session, which the Engine holds in its own fields rather than
/// in a mailbox: `FeedBackend::sample` fills them beside the mailbox half, so a reader takes them
/// from the readout like every other raw fact. `stage` is `None` exactly while no Engine is
/// installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadRaw {
    /// The session's stage; `None` with no Engine.
    pub stage: Option<Stage>,
    /// Whether the Load thread has returned (or never ran): `true` with no Engine or no thread. It
    /// only goes `false` to `true` within a session, so a stale sample says "still running", never
    /// the reverse.
    pub load_thread_finished: bool,
    /// Native readiness for a route or action claim: a stream source at `Playing` or later,
    /// unless an in-place seek's flush still waits for its rebase.
    pub route_actions_ready: bool,
}

impl LoadRaw {
    /// A feed with no session.
    pub const IDLE: Self = Self { stage: None, load_thread_finished: true,
        route_actions_ready: false };

    /// Whether the session has yet to leave its Load stage.
    pub fn loading(&self) -> bool { self.stage == Some(Stage::Loading) }
}

/// The feed-written values the Stats-for-nerds read-out and the lab snapshot show, taken in the
/// same sample as every other raw fact (`FeedRaw::diag`). They are diagnostics only: nothing in the
/// player decides on one. Each is the value of the feed mailbox of the same name, so while the
/// feed has a live session a reader gets what a live read of it would have given, one frame old at
/// most (the pump samples at its start and end), and [`Self::IDLE`] is what the mailboxes hold
/// after the session's reset. A feed with no live session is not sampled, so a start that failed
/// after its Load payload was built leaves the read-out at [`Self::IDLE`] while the mailboxes still
/// hold the refused Load's codecs. The ABR half of the read-out is not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedDiag {
    /// `setDisplayWindow`'s return code of the last placement; `i32::MIN` before any.
    pub place_rv: i32,
    /// The source rectangle the last placement used.
    pub placed_w: i32,
    pub placed_h: i32,
    /// The pipeline reported `loadCompleted`.
    pub load_completed: bool,
    /// Pipeline callbacks seen this session.
    pub cb_count: u32,
    /// The bytes queued in the video and audio access-unit lanes. The feed stores them (and
    /// `fed_v_pts`, `fed_a_pts` and `frame_at`) only while the read-out is on screen, so they hold
    /// the last value published while it was, and 0 if it never was this session.
    pub aq_video: i64,
    pub aq_audio: i64,
    /// The newest presentation timestamp fed to each lane (ns).
    pub fed_v_pts: i64,
    pub fed_a_pts: i64,
    /// The codec the Load payload declared, as a code (`0` = none).
    pub load_v: u8,
    pub load_a: u8,
    /// Why the video feeder is where it is, as a code (`0` = nothing fed yet).
    pub feed_state: u8,
    /// The type of the pipeline's first error callback (`18`, latched; 0 = none) and the callback
    /// count it arrived at. The firmware's own error number is [`FeedRaw::cb_err_num`], not this.
    pub cb_err: i32,
    pub cb_err_at: u32,
    /// Bytes the movie's transport has delivered this session.
    pub net_rx: i64,
    /// The millisecond stamps of the Load's completion and of the last frame-count movement.
    pub load_at: u32,
    pub frame_at: u32,
    /// The pipeline's reported frame rate in thousandths of a frame per second; 0 = not reported.
    pub video_fps_milli: i64,
    /// Video and audio access units the feed has handed the pipeline this session (accepted, not
    /// attempted: a refused push is not counted).
    pub fed_v: i64,
    pub fed_a: i64,
    /// The session's demuxer has handed over at least one video access unit. Tells "the demuxer
    /// produced nothing" from "it produced access units that never reached the decoder": two stalls
    /// that present identically as a spinner.
    pub pushed_any: bool,
}

impl FeedDiag {
    /// A feed with no session: the values its mailboxes are reset to.
    pub const IDLE: Self = Self {
        place_rv: i32::MIN, placed_w: 0, placed_h: 0, load_completed: false, cb_count: 0,
        aq_video: 0, aq_audio: 0, fed_v_pts: 0, fed_a_pts: 0, load_v: 0, load_a: 0, feed_state: 0,
        cb_err: 0, cb_err_at: 0, net_rx: 0, load_at: 0, frame_at: 0, video_fps_milli: 0,
        fed_v: 0, fed_a: 0, pushed_any: false,
    };
}

impl Default for FeedDiag {
    fn default() -> Self { Self::IDLE }
}

/// Whether to defer joining a preview's Load thread: while an in-flight Load has not returned,
/// joining it on the main thread is the stall abandoning a preview exists to avoid.
pub fn defer_media_join(loading: bool, thread_finished: bool) -> bool {
    loading && !thread_finished
}

/// A seek the feed asked the player to take up again. `epoch` counts the feed's requests this
/// session (0 = none); the player turns a sample whose epoch differs from the previous sample's
/// into ONE pending seek at `target_ns`, so a repeated sample of the same request adds nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeekRequeue {
    pub epoch: u32,
    pub target_ns: i64,
}

impl SeekRequeue {
    /// No request: a feed with no session.
    pub const NONE: Self = Self { epoch: 0, target_ns: 0 };
}

impl FeedRaw {
    /// A feed with no session; the const a holder initialises from.
    pub const IDLE: Self = Self {
        playpos_ns: 0, frames: 0,
        flags: FailureFlags {
            load_failed: false, load_timed_out: false, demux_failed: false, demux_io_failed: false,
        },
        decoder_revocations: 0, native_audio_refusals: 0, file_size: 0,
        applied_rate_permille: crate::RATE_NORMAL, speed_audio_ok: None, vpres_count: 0,
        pts_shift: 0, pipeline_eos: false, seek_landed_epoch: 0, seek_target_ns: None,
        seek_requeue: SeekRequeue::NONE, cb_err_num: 0, http_status: 0,
        probe_actual_video: 0, probe_actual_audio: 0,
        diag: FeedDiag::IDLE,
        vpres_gap_sum_ms: 0, vpres_gap_count: 0, disp_base: 0, load: LoadRaw::IDLE,
        abr: DemuxAbr::IDLE,
        side_reader_active: false, side_failures: 0, side_exact_delta_ns: None, native_audio: None,
        seek_mode: SeekMode::Segment,
    };
}

impl Default for FeedRaw {
    fn default() -> Self { Self::IDLE }
}

/// One sample of the feed: the mapped facts and the raw ones. `Copy`, addressed by the attempt
/// that is ingesting it. The demux ABR and the feed-written diagnostics are in `raw`
/// ([`FeedRaw::abr`], [`FeedRaw::diag`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FeedFacts {
    pub attempt: AttemptFacts,
    pub raw: FeedRaw,
}

// ---------------------------------------------------------------------------------------------
// Commands: what the player asks of the feed, by value.
// ---------------------------------------------------------------------------------------------

/// One intent the player gives the feed. Each is the effect of one readout event (the event states
/// what the viewer or the route meant, the command is what the feed does about it), so a request
/// that used to write several of the feed's members is one value here. The feed carries a command
/// out where it stands, in the order it was given. Seeking is not an [`Operation`]: it stays
/// outside the runner's door (R8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedCommand {
    /// A seek was requested. `epoch` is the player's requested-seek count after this request (a
    /// later request carries a larger one); the feed reports it back as [`FeedRaw::seek_landed_epoch`]
    /// once playback resumes at the new position. `display_ns` is the target the HUD freezes the
    /// playhead at while the seek resolves.
    Seek { epoch: u32, target_ns: i64, display_ns: i64 },
    /// The seek was given up on: the spinner and the frozen playhead end with it.
    AbandonSeek,
    /// The player took the pending seek (it is applying it, flying it or reloading at it): the
    /// feed's copy of the pending target, which gates feeding while a seek waits, is cleared with
    /// the player's own in the same call. `epoch` is the newest request the take belongs to.
    SeekTaken { epoch: u32 },
    /// The player put a taken seek back (the clock transition was busy, or a newer tap outlived a
    /// reload): only the feed's copy of the pending target is re-armed. This is not a [`Self::Seek`]:
    /// nothing was requested again, so the in-flight flag, the HUD's display target and the
    /// pipeline's end of stream stay as they were.
    SeekRequeued { target_ns: i64 },
    /// The next open starts at `target_ns` (a reload or a resume): the demuxer is armed to seek
    /// there, the HUD reads `playpos_ns` until frames land, and `disp_base` is the offset the
    /// published position carries. The caller supplies the clamped values.
    ArmOpen { target_ns: i64, disp_base: i64, playpos_ns: i64 },
    /// Publish a committed physical transport transition (or seed the viewer's saved goal as the
    /// initial hold of a new object) to the feed gate and the exact pause clock.
    CommitPaused { paused: bool },
    /// Open the paused-seek preroll for a restore that is about to Load; a feed that is not
    /// paused opens nothing.
    BeginPausedSeek,
    /// The pump dispatched a seek under the viewer's Pause: open the preroll if none is open and
    /// take ownership of closing it, or retarget the one the pump already owns.
    OwnPausedSeek { target_ns: i64 },
    /// Close the paused-seek preroll, and with it any ownership of it.
    FinishSeekPreroll,
    /// The player's own judgement ended the open, not a callback: the feed raises the flag in its
    /// own mailbox (the callbacks' ordering, `Release`), and it comes back through
    /// [`FeedRaw::flags`] with the next sample. The player does not keep a copy of it.
    NoteFailure(FailureFlag),
    /// A transcode restarted at a content offset (`reload_transcode`, a resume's rebuild): the
    /// encode is 0-based there, so the published position carries `disp_base` and the HUD reads
    /// `playpos_ns` until frames land. Unlike [`Self::ArmOpen`] nothing is armed to seek: the new
    /// stream starts at the offset by construction. `pts_shift` is not here: the feed's video lane
    /// alone writes it, when it rebases onto a landed keyframe.
    Rebase { playpos_ns: i64, disp_base: i64 },
    /// The audio stream the demuxer feeds, as the player's session holds it (`idx` is the 0-based
    /// audio stream index, `-1` the file's own default). The demuxer reads the feed's copy as it
    /// opens the container, so the player sends this before every start that spawns one.
    SelectAudio { idx: i32 },
    /// Make the running side subtitle reader match the route's target while the stream plays on:
    /// start one for a track picked on a live remux, run it clock-only for a sidecar, switch its
    /// track, or stop it (`None`). Never blocks: stopping a reader joins its thread, which the feed
    /// hands to a worker. The newest request wins.
    SyncSideReader(Option<std::sync::Arc<crate::subtitle::Spec>>),
    /// A fresh play (not a reload of the one on screen) begins: this playback gets to try the
    /// platform's native `Seek` again, whatever an earlier playback learned about its refusal.
    BeginPlayback,
    /// The route refused the withdrawal [`FeedLoad::dv_landing_due`] handed over (it no longer declares
    /// a node it can take out): the Dolby Vision guard never acts again for this Engine and the held
    /// landing feeds.
    DvStandDown,
}

// ---------------------------------------------------------------------------------------------
// The traits.
// ---------------------------------------------------------------------------------------------

/// The seek verbs of the feed backend, each the body of one existing call. A receiver matches
/// the borrow the wrapped function takes.
pub trait FeedSeek {
    /// The pump's per-tick paused-seek preroll step. Runs before the adapter is split because the
    /// re-pause takes the whole adapter.
    fn paused_seek_step(&mut self, now: u32, route_flight: bool, claim_owns_pause: bool)
        -> PausedSeekStep;
    /// Whether the native seek reopen watchdog fired.
    fn stuck_seek_due(&mut self, now: u32, feed_allowed: bool, route_flight: bool, budget_ms: u32)
        -> bool;
    /// Re-arm only the native demux target after the common player committed any newer seek.
    fn retry_stuck_seek(&mut self, now: u32, target_ns: i64, can_retry: bool) -> StuckSeekAction;
    /// Eligibility for applying a pending direct-play or transcode seek on this feed.
    fn pending_seek_ready(&self, seek_reloads: bool) -> bool;
    /// One native Pause transition before an in-place seek. An action, not a query: it moves the
    /// transport's clock hold and issues the sink's Pause.
    fn prepare_seek_pause(&mut self, mt: &MainThread) -> SeekPause;
    /// Open the native media discontinuity an in-place seek crosses; an action, so `&mut self`.
    fn begin_seek_discontinuity(&mut self) -> bool;
    fn apply_inplace_seek_pre(&mut self, mt: &MainThread, target_ns: i64, mode: SeekMode,
        coalesced: u32);
    fn apply_inplace_seek_post(&mut self, target_ns: i64, now: u32);
}

/// The Load-side verbs and pure queries of the feed backend.
pub trait FeedLoad {
    /// Physical Load and Play transition. `no_audio` is the common player's no-audio arming.
    fn tick_load(&mut self, mt: &MainThread, now: u32, segmented_hls: bool,
        speed_deadline_ms: u32, no_audio: bool) -> LoadTick;
    fn tick_presentation(&mut self, mt: &MainThread, segmented_hls: bool, immersive: bool)
        -> PresentationTick;
    /// Whether native construction is callable yet.
    fn ctor_gate(&mut self, mt: &MainThread) -> CtorGate;
    /// Load verdicts the live session still owns, in arrival order.
    fn take_load_outcomes(&mut self) -> Vec<AcceptedLoadOutcome>;
    fn native_audio_stall_fired(&mut self, now: u32) -> bool;
    /// The Dolby Vision landing guard's verdict for this frame (#1166): the withdrawal the feed raised
    /// at a landing it predicted a skip for, else the guard's watch on the pipeline's silence after a
    /// fed landing. `None` for a play the guard does not act on; a report-only guard's lines are the
    /// feed's own. A route flight owns the route's reload, so while `route_flight` a pending withdrawal
    /// waits and the watch's clock does not run. The player calls it BEFORE
    /// [`Self::native_audio_stall_fired`], which yields to a held landing, and answers a refused
    /// withdrawal with [`FeedCommand::DvStandDown`].
    fn dv_landing_due(&mut self, now: u32, route_flight: bool) -> Option<DvWithdrawal>;
    fn note_missing_era_callback(&mut self);
    /// Feed the dev firmware-refusal probe with route facts.
    fn inject_refuse601(&mut self, generation: u32, preview: bool, refused_for: &AtomicU32);
    /// Publish native queue and frame diagnostics; `diag_readout_on` is the panel's switch.
    fn publish_diag(&mut self, now: u32, last_frames: &AtomicI32, diag_readout_on: bool);
    /// Retiring a preview whose Load has not returned: when the session is still in its Load
    /// stage and the Load thread is unfinished ([`defer_media_join`]), mark the preview abandoned
    /// and answer `true`, and the caller leaves the Engine installed for the abandoned-Load poll.
    /// `false` (nothing marked) otherwise, and with no session. The Feed answers in the call that
    /// acts, because the thread's completion moves on another thread and no sample can be current.
    fn defer_preview_join(&mut self) -> bool;

    /// One sample of what the feed observed: the sparse facts the HUD and the route read, the raw
    /// ones a decision reads and the Engine's Load-side ones ([`FeedRaw::load`]). `None` with no
    /// session: an idle feed's mailboxes are not an observation of any attempt.
    fn sample(&self) -> Option<FeedFacts>;
    /// Is a native object still parked behind a Load that has not been released?
    fn has_abandoned_load(&self) -> bool;
    /// Release the abandoned Load if its worker has returned (the ordinary Unload, gate, retire
    /// and D1 release); `true` when one was released.
    fn reap_abandoned_load(&mut self, mt: &MainThread) -> bool;
}

/// The native open, in the phases the route's start drives. The prepared open stays inside the
/// feed between [`Self::open_prepare`] and [`Self::open_stage`]; the route reducer mints the
/// attempt between the two.
pub trait FeedOpen {
    /// Begin an open: reap an abandoned Load that has returned, refuse (`false`) while one has not
    /// (it still owns the one native object the C seam holds) and zero the per-session fed
    /// counters and the stall watchdog.
    fn open_begin(&mut self, mt: &MainThread) -> bool;
    /// Resolve the Load payload and arm the retractable clock and native epochs, parking the
    /// prepared open. Answers its native epoch. A refusal parks nothing.
    fn open_prepare(&mut self, mt: &MainThread, request: OpenRequest) -> Result<u32, Failure>;
    /// Bind the addressed demux session to the parked open and escrow it for the Start the runner
    /// will address to `attempt`. A refusal drops the parked open.
    fn open_stage(&mut self, attempt: AttemptId, key: SourceKey, session: Box<dyn DemuxSession>)
        -> Result<(), Failure>;
    /// Drop the parked open after a start that was lost between `open_prepare` and `open_stage`,
    /// releasing its clock and epoch guards. A no-op with nothing parked.
    fn open_abort(&mut self);
}

/// Which end of a Feed session a [`FeedStop::reset`] serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetScope {
    /// A reload of the same item: the file's duration survives (the demuxer re-publishes it a few
    /// hundred ms after it reopens, and a zero until then is a playbar drawn at position / 0).
    Reload,
    /// A real stop: everything of the session goes.
    Stop,
}

/// An off-frame release a stop handed off: the native epoch being unloaded and when. The release
/// itself (the Engine it keeps alive, its worker) stays in the feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleaseHandoff {
    pub epoch: u32,
    pub at: std::time::Instant,
}

/// What the stop learned once it had woken and joined the feed's workers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnblockFacts {
    /// The attempt's terminal sample (the player adds the members it owns).
    pub attempt: AttemptFacts,
    /// A Load past its budget was still inside the native call and stays parked: the release that
    /// follows hands it to the abandoned-Load release.
    pub load_abandoned: bool,
}

/// The stop of the feed backend, in phases the player's teardown drives in this order. Common
/// route and report work runs between the phases and never receives the Engine or a native
/// handle: the stop in progress is the backend's own state, keyed by nothing because at most one
/// exists. A phase called out of order is a programming error.
pub trait FeedStop {
    /// Begin a stop: stop the HLS clock, sample the outgoing attempt's last facts while it is still
    /// installed (`None` with no session) and take the Engine out. The sample is taken after the
    /// clock stops, as the player published it before this verb existed. `terminal` is the logical
    /// playback's end under a kept session (the server stopped it, and the read-out stays): the
    /// release is the off-frame kind of a real stop, though the stop is shaped like a reload.
    fn stop_begin(&mut self, terminal: bool) -> Option<FeedFacts>;
    /// The side reader stops before the paired demux reader is joined.
    fn stop_side_reader(&mut self, for_reload: bool);
    /// Wake both transports and queues, join the demux and Load workers (a Load past its budget, or
    /// one still in the native call when the stop is terminal, stays parked, unjoined) and close the
    /// socket once the demux reader no longer owns its fd.
    fn stop_unblock(&mut self) -> UnblockFacts;
    /// The common final report has been handed to its off-frame worker.
    fn stop_report_dispatched(&mut self);
    /// Release the native object after the common reporting has handed off its final write: an
    /// off-frame `Unload` for a real stop, a timed one in place for a reload. A Load found
    /// abandoned is parked instead, and its object stays until the Load returns.
    fn stop_release(&mut self, mt: &MainThread, for_reload: bool);
    /// End the stop. The Engine drops, or stays alive under an off-frame `Unload` whose handoff is
    /// returned; the driver is told how the release stands.
    fn stop_finish(&mut self, for_reload: bool) -> Option<ReleaseHandoff>;
    /// Clear the session's mirrors and run `common` (the player's own session stores), both inside
    /// the callback/reset barrier, so a native callback cannot validate one session and then write
    /// into the next one's reset storage.
    fn reset(&mut self, scope: ResetScope, common: &mut dyn FnMut());
    /// The epoch of a parked release whose `Unload` worker has returned. The worker is joined, so
    /// the epoch can be fed to the release machine, and the release waits for its main-thread tail.
    fn take_returned_release(&mut self) -> Option<u32>;
    /// Run the main-thread tail of the release [`Self::take_returned_release`] reported for
    /// `epoch`, and tell the driver how it ended. `false` when no returned release was waiting.
    fn run_release_tail(&mut self, mt: &MainThread, epoch: u32) -> bool;
    /// **A worker still in firmware reads memory its owner is about to drop.** At the exit's
    /// WorkerInFirmware outcome the installed Engine, the parked release and the parked
    /// abandoned Load are leaked for the life of the process instead of dropped under the call.
    fn leak_for_exit(&mut self);
}

/// One buffer-feed backend as the player drives it: the seek, Load, open and stop halves, and the
/// one verb every intent goes through.
pub trait FeedControl: FeedSeek + FeedLoad + FeedOpen + FeedStop {
    /// Carry out one player intent (see [`FeedCommand`]).
    fn command(&mut self, command: FeedCommand);
    /// The viewer's Pause: prepare the clock transition, issue the native Pause through the
    /// composition's sink, complete the transition and commit the transport (see
    /// [`UserPauseOutcome`]). A method rather than a command because the caller acts on the answer.
    fn user_pause(&mut self, mt: &MainThread) -> UserPauseOutcome;
    /// The viewer's Resume: the mirror of [`Self::user_pause`] (see [`UserResumeOutcome`]).
    fn user_resume(&mut self, mt: &MainThread) -> UserResumeOutcome;
    /// Take the sink volume a paused preroll's Play window muted, clearing the mute: `None` when
    /// nothing is muted. The caller sets the volume back, so the answer is a value, not an intent.
    fn take_preroll_volume(&mut self) -> Option<i32>;
    /// The next native transport action the driver queued for the player's transport door. The
    /// player runs it (the sink, the readout) and answers with [`Self::note_transport_result`]
    /// before taking another; nothing in the runner's event stream carries it, because the
    /// player's own state is read between the two.
    fn take_transport_action(&mut self) -> Option<FeedTransportAction>;
    /// What the transport door did with `action`, so the driver can answer the runner.
    fn note_transport_result(&mut self, mt: &MainThread, action: FeedTransportAction,
        accepted: bool, native_acknowledged: bool);
}

/// The physical drivers one composition supplies, behind one object. It is the runner's
/// [`DriverSet`] on the main thread, and `feed` is the buffer-feed half for the pump's direct
/// verbs; a set without a feed answers `None`. The other half is the URI driver's seat: the
/// composition installs one driver after the set is built, and the adapter reads, drives and
/// takes it back out through the four seat methods. A set with no URI seat (the defaults) holds
/// none, answers `None` and hands an offered driver back.
pub trait PhysicalSet: DriverSet<Context = MainThread> {
    fn feed(&mut self) -> Option<&mut dyn FeedControl>;

    /// The installed URI driver, if the composition built one.
    fn uri(&self) -> Option<&(dyn UriDriver<Context = MainThread> + 'static)> { None }

    fn uri_mut(&mut self) -> Option<&mut (dyn UriDriver<Context = MainThread> + 'static)> { None }

    /// Seat the URI driver. The caller has already refused a second one; a set with no URI seat
    /// hands the driver back untouched.
    fn install_uri(&mut self, driver: Box<dyn UriDriver<Context = MainThread>>)
        -> Result<(), Box<dyn UriDriver<Context = MainThread>>> {
        Err(driver)
    }

    /// Take the URI driver out, to be dropped or leaked by the caller.
    fn take_uri(&mut self) -> Option<Box<dyn UriDriver<Context = MainThread>>> { None }
}

/// The set a player starts with before a composition installs its own, and the whole set of a
/// build with no Feed: no feed, and the URI driver's seat, so a URI-only build installs nothing
/// but the one driver the composition may seat.
#[derive(Default)]
pub struct UriOnlySet {
    uri: Option<Box<dyn UriDriver<Context = MainThread>>>,
}

impl UriOnlySet {
    pub fn new() -> Self { Self::default() }
}

impl DriverSet for UriOnlySet {
    type Context = MainThread;

    fn capabilities(&self, backend: BackendKind) -> Option<Capabilities> {
        match backend {
            BackendKind::Feed => None,
            BackendKind::Uri => self.uri.as_ref().and_then(|d| d.capabilities(BackendKind::Uri)),
        }
    }

    fn dispatch(&mut self, backend: BackendKind, ctx: &MainThread, attempt: AttemptId,
        command: CommandId, operation: Operation) -> DispatchResult {
        match (backend, self.uri.as_mut()) {
            (BackendKind::Uri, Some(driver)) =>
                driver.dispatch(BackendKind::Uri, ctx, attempt, command, operation),
            _ => DispatchResult::Refused(Failure::Unavailable),
        }
    }

    fn poll(&mut self, ctx: &MainThread) -> Vec<BackendEvent> {
        self.uri.as_mut().map_or_else(Vec::new, |driver| driver.poll(ctx))
    }

    fn discard_staged(&mut self, backend: BackendKind, attempt: AttemptId, key: SourceKey) {
        if let (BackendKind::Uri, Some(driver)) = (backend, self.uri.as_mut()) {
            driver.discard_staged(BackendKind::Uri, attempt, key);
        }
    }

    fn exit_disposition(&self) -> ExitDisposition {
        self.uri.as_ref().map_or(ExitDisposition::Clean, |driver| driver.exit_disposition())
    }
}

impl PhysicalSet for UriOnlySet {
    fn feed(&mut self) -> Option<&mut dyn FeedControl> { None }

    fn uri(&self) -> Option<&(dyn UriDriver<Context = MainThread> + 'static)> { self.uri.as_deref() }

    fn uri_mut(&mut self) -> Option<&mut (dyn UriDriver<Context = MainThread> + 'static)> {
        self.uri.as_deref_mut()
    }

    fn install_uri(&mut self, driver: Box<dyn UriDriver<Context = MainThread>>)
        -> Result<(), Box<dyn UriDriver<Context = MainThread>>> {
        self.uri = Some(driver);
        Ok(())
    }

    fn take_uri(&mut self) -> Option<Box<dyn UriDriver<Context = MainThread>>> { self.uri.take() }
}

// ---------------------------------------------------------------------------------------------
// Vocabulary the player and the feed backend both name: plain data and constants, so the player
// reads them without naming the crate that runs the feed.
// ---------------------------------------------------------------------------------------------

/// "No presentation position yet" in the feed's `pres_fed` mailbox.
pub const PRES_NONE: i64 = i64::MIN;
/// Maximum video lead beyond the presented position in the native feed.
pub const MAX_FEED_AHEAD_NS: i64 = 1_600_000_000;
/// How much further ahead than [`MAX_FEED_AHEAD_NS`] the audio lane may ride: audio is cheap and
/// the master clock, so its bound is looser.
pub const AUDIO_SLACK_NS: i64 = 2_000_000_000;
/// Byte caps of the native video and audio access-unit queues.
pub const AQ_VIDEO_BYTES: std::os::raw::c_long = 10 * 1024 * 1024;
pub const AQ_AUDIO_BYTES: std::os::raw::c_long = 1024 * 1024;

/// The AU queues' two base byte caps, video then audio: the denominators the read-out shows its
/// `dg_aq_*` against.
pub const fn aq_caps() -> (i64, i64) {
    (AQ_VIDEO_BYTES as i64, AQ_AUDIO_BYTES as i64)
}

/// A native release transition the telemetry owner records at the exact moment it happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseStep {
    NativeUnload,
    AcbPlaneUnload,
    NativeQuarantined,
    NativeDestroyed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::FakeUriDriver;
    use crate::{DriverEvent, ReleaseStatus};

    /// The set that installs no physical driver: every backend is absent, every dispatch is refused
    /// as unavailable, and there is no feed. It overrides none of [`PhysicalSet`]'s defaults, so the
    /// tests below read what a set with no URI seat does.
    struct NoPhysical;

    impl DriverSet for NoPhysical {
        type Context = MainThread;
        fn capabilities(&self, _backend: BackendKind) -> Option<Capabilities> { None }
        fn dispatch(&mut self, _backend: BackendKind, _ctx: &MainThread, _attempt: AttemptId,
            _command: CommandId, _operation: Operation) -> DispatchResult {
            DispatchResult::Refused(Failure::Unavailable)
        }
        fn poll(&mut self, _ctx: &MainThread) -> Vec<BackendEvent> { Vec::new() }
        fn discard_staged(&mut self, _backend: BackendKind, _attempt: AttemptId, _key: SourceKey) {}
        fn exit_disposition(&self) -> ExitDisposition { ExitDisposition::Clean }
    }

    impl PhysicalSet for NoPhysical {
        fn feed(&mut self) -> Option<&mut dyn FeedControl> { None }
    }

    /// The URI driver answers through the seat alone: its capabilities, its polled events, its
    /// staged-source discards (never a feed discard) and its exit disposition count, and nothing
    /// is offered for the feed backend.
    #[test]
    fn the_uri_only_set_answers_for_its_seated_driver_alone() {
        let caps = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
            pause: true, seek: true, audio_track: false, subtitle_track: false, audio_off: false,
            subtitle_off: false };
        let mt = unsafe { MainThread::assume() };
        let (attempt, key) = (AttemptId::new(1, 1), SourceKey::new(3));
        let mut set = UriOnlySet::new();
        assert!(set.uri().is_none() && set.feed().is_none() && set.poll(&mt).is_empty());
        assert_eq!(crate::BackendSet::from_capabilities(&set), crate::BackendSet::NONE);
        let (driver, handle) = FakeUriDriver::new(caps);
        assert!(set.install_uri(Box::new(driver)).is_ok());
        assert_eq!(crate::BackendSet::from_capabilities(&set), crate::BackendSet::URI);
        assert!(set.capabilities(BackendKind::Feed).is_none());
        handle.queue([DriverEvent::Release { attempt, status: ReleaseStatus::Released }]);
        let events = set.poll(&mt);
        assert_eq!((events.len(), events[0].backend), (1, BackendKind::Uri));
        set.discard_staged(BackendKind::Feed, attempt, key);
        assert!(handle.discarded().is_empty(), "a feed discard never reaches the URI driver");
        set.discard_staged(BackendKind::Uri, attempt, key);
        assert_eq!(handle.discarded(), [(attempt, key)]);
        assert_eq!(set.exit_disposition(), ExitDisposition::Clean);
        handle.set_exit_preserved(true);
        assert_eq!(set.exit_disposition(), ExitDisposition::PreserveNativeOwnership);
        assert!(set.take_uri().is_some() && set.uri().is_none());
    }

    /// A feed with no session has seen nothing and runs at 1x, and a default sample is the empty
    /// readout beside it: the starting value every ingestion replaces.
    #[test]
    fn a_default_sample_is_a_feed_with_no_session() {
        let facts = FeedFacts::default();
        assert_eq!(facts.raw, FeedRaw::default());
        assert_eq!(facts.raw.applied_rate_permille, crate::RATE_NORMAL);
        assert_eq!(facts.raw.flags, FailureFlags::default());
        assert!(facts.attempt.position_ns.is_none() && facts.attempt.status.is_none());
        assert_eq!(facts.raw.speed_audio_ok, None, "no report is not a refusal");
    }

    /// A feed with no session has no controller: the demux's half of the Auto-quality facts is
    /// what the session's reset stored (`0`, `0`, no declaration).
    #[test]
    fn a_feed_with_no_session_has_no_controller() {
        assert_eq!(FeedRaw::IDLE.abr, DemuxAbr { mode: 0, kbps: 0, declared_kbps: -1 });
        assert_eq!(DemuxAbr::default(), DemuxAbr::IDLE);
    }

    /// A feed with no session has no stage, no Load to wait for and nothing ready; the IDLE value
    /// is what a session reset hands back.
    #[test]
    fn the_idle_load_half_has_no_stage_and_no_load_to_wait_for() {
        assert_eq!(FeedRaw::IDLE.load, LoadRaw::IDLE);
        assert_eq!(FeedRaw::IDLE.diag, FeedDiag::IDLE);
        assert_eq!(FeedDiag::default(), FeedDiag::IDLE);
        assert_eq!(FeedDiag::IDLE.place_rv, i32::MIN, "no placement yet is not a placement that returned 0");
        assert_eq!(LoadRaw::IDLE.stage, None);
        assert!(LoadRaw::IDLE.load_thread_finished, "no thread is a finished one");
        assert!(!LoadRaw::IDLE.route_actions_ready && !LoadRaw::IDLE.loading());
        let loading = LoadRaw { stage: Some(Stage::Loading), load_thread_finished: false,
            route_actions_ready: false };
        assert!(loading.loading());
        assert!(!LoadRaw { stage: Some(Stage::Playing), ..loading }.loading());
    }

    /// Abandoning a preview defers the join only while the Load thread has not returned.
    #[test]
    fn abandon_defers_the_join_until_the_load_thread_has_returned() {
        assert!(defer_media_join(true, false));
        assert!(!defer_media_join(true, true));
        assert!(!defer_media_join(false, false));
    }

    /// The set a composition hands the adapter is a boxed trait object, so the trait must be
    /// object-safe, and the default set answers like a composition with nothing installed.
    #[test]
    fn a_boxed_physical_set_is_object_safe_and_the_default_has_no_feed() {
        let mut set: Box<dyn PhysicalSet<Context = MainThread>> = Box::new(NoPhysical);
        assert!(set.feed().is_none(), "no physical set, no feed");
        assert_eq!(crate::BackendSet::from_capabilities(&*set), crate::BackendSet::NONE);
        assert_eq!(set.exit_disposition(), ExitDisposition::Clean);
        // SAFETY: a host test; nothing here touches the thread-affine state the token guards.
        let mt = unsafe { MainThread::assume() };
        assert!(set.poll(&mt).is_empty());
        assert!(matches!(
            set.dispatch(BackendKind::Feed, &mt, AttemptId::new(1, 1), CommandId(1),
                Operation::Pause),
            DispatchResult::Refused(Failure::Unavailable)));
    }

    /// A set with no URI seat holds no driver and gives an offered one straight back, so the
    /// adapter's refusal of an install reaches the composition instead of dropping the driver.
    #[test]
    fn a_set_without_a_uri_seat_hands_an_offered_driver_back() {
        let caps = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
            pause: true, seek: true, audio_track: false, subtitle_track: false, audio_off: false,
            subtitle_off: false };
        let (driver, _handle) = crate::test_support::FakeUriDriver::new(caps);
        let mut set: Box<dyn PhysicalSet<Context = MainThread>> = Box::new(NoPhysical);
        assert!(set.uri().is_none() && set.uri_mut().is_none() && set.take_uri().is_none());
        assert!(set.install_uri(Box::new(driver)).is_err(), "nowhere to seat it");
        assert!(set.uri().is_none());
    }

    /// A local validation sample is cut at each 5-byte access-unit delimiter of its codec: H.264's
    /// AUD (`09`) and HEVC's (`46`) are different delimiters, so one sample is never split by the
    /// other codec's, and bytes before the first delimiter belong to no unit.
    #[test]
    fn an_annex_b_sample_splits_at_its_codecs_access_unit_delimiter() {
        let h264 = [0xAA, 0, 0, 0, 1, 0x09, 1, 2, 0, 0, 0, 1, 0x09, 3, 0, 0, 0, 1, 0x46, 4];
        assert_eq!(bf_split(&h264, 0x09), vec![1, 8]);
        assert_eq!(bf_split(&h264, 0x46), vec![14]);
        assert!(bf_split(&[0, 0, 0, 1], 0x09).is_empty(), "a bare start code has no delimiter byte");
        assert!(bf_split(&[], 0x09).is_empty());
    }
}
