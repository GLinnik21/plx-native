//! The buffer-feed seam between the player's common pump and the physical feed backend.
//!
//! Plain data only: the Load payload's facts, the typed verdicts one pump tick returns, and the
//! traits through which the player drives a feed backend. The backend's own state (its native
//! session, its worker threads, its callback mailboxes) never crosses this module; a verdict is
//! returned by value and the caller applies it.
//!
//! [`PhysicalSet`] is the one object the player adapter holds for every physical executor it can
//! drive. Its `feed` answers the buffer-feed half as a [`FeedControl`], or `None` when the
//! composition installed no feed (the default, [`NoPhysical`]), so the player runs without naming
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
//! | `playpos_ns` (10) | `position_ns = reported_frame.then_some(pos.max(0))` | **raw**: every resume seeds it before any frame, and the open-recovery resume offset (`pump.rs` `drive_open_recovery`) reads it before one; through `position_ns` a refused resume would reconvert at 0 |
//! | `frames` (6) | `reported_frame.then_some(frames)` | **raw**: `== 0` and `> 0` tests where "no callback yet" differs from "0 frames" whenever frames > 0 arrive without a frame callback (not proven impossible) |
//! | `load_failed`, `load_timed_out`, `demux_failed`, `demux_io_failed` (10) | one collapsed `runtime_failure` (`play-feed/src/readout.rs`) | **raw**: [`FailureFlags`]; readers test single flags and `live_runtime_failure` re-derives the collapse in media |
//! | `duration_ns` (5) | `(d > 0).then_some(d)` | **mapped**, `unwrap_or(0)`: every reader tests `> 0` (`timeline.rs` `from_content_ns`, `pump.rs` `stream_ended` and the `> 0` seek gate, `route_start.rs` `final_report`) or passes it to `report::ended`; the store is 0 or positive (`reset` stores 0), so `<= 0` never carries a meaning `None` loses |
//! | `dg_http_status` (3), `dg_cb_err_num` (1) | `> 0` and `!= 0` gates | **mapped**, `unwrap_or(0)`: every site compares a code against a constant, and 0 is "none" |
//! | `hls_rebuffering` (1), `seen_frame` (1) | plain bool | **mapped** |
//! | `native_audio_refused`, `decoder_revoked`, `file_size`, `applied_rate_permille`, `speed_audio_verdict`, `native_load_abandoned`, `dg_vpres_ct`, `dg_vpres_gap`, `pts_shift` (13) | no `AttemptFacts` field | **raw**, in [`FeedRaw`] |
//!
//! Not here yet: the seek family (`seeking`, `seek_target_ns`, `disp_base`, `pipeline_eos`, and the
//! epoch'd `seek_requeue`/`seek_landed_epoch`; they have media writers and join with L3b's intents),
//! `abr_seed` (a worker event, L3c), and the `DemuxAbr` and diagnostics halves of [`FeedFacts`] (L3c).
//! A value that is `-1` in the feed's mailbox is an `Option` here, as in [`AttemptFacts`].

use std::sync::atomic::{AtomicI32, AtomicU32};
use std::time::Duration;

use plx_base::task::MainThread;

use crate::readout::AttemptFacts;
use crate::{
    AttemptId, BackendEvent, BackendKind, Capabilities, CommandId, DispatchResult, DriverSet,
    ExitDisposition, Failure, Operation, SourceKey,
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

// ---------------------------------------------------------------------------------------------
// Verdicts: what one physical tick or one native transition answers, applied by the caller.
// ---------------------------------------------------------------------------------------------

/// Whether native construction is callable yet. A blocked Load exposes its exact epoch only
/// while the live non-preview session still owes the Load budget check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtorGate {
    Ready,
    Awaiting { budget_epoch: Option<u32> },
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
pub enum HlsSeekPause {
    Issue(u64),
    AlreadyHeld,
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
    /// The pipeline took the video decoder back from an established session.
    pub decoder_revoked: bool,
    /// The pipeline refused the PCM audio of a speed session.
    pub native_audio_refused: bool,
    /// A timed-out Load is still inside the native call.
    pub load_abandoned: bool,
    /// The source's size in bytes; 0 while unknown.
    pub file_size: i64,
    /// The speed this session's Load plays at, 1000 = 1x.
    pub applied_rate_permille: i32,
    /// What the pipeline reported for a speed session's audio: `None` before any report,
    /// `Some(true)` for the declared 48 kHz stereo, `Some(false)` otherwise.
    pub speed_audio_ok: Option<bool>,
    /// Native presentation callbacks seen, and the largest gap between two of them.
    pub vpres_count: u32,
    pub vpres_gap: u32,
    /// The shift between the container's timestamps and the pipeline's.
    pub pts_shift: i64,
}

impl FeedRaw {
    /// A feed with no session; the const a holder initialises from.
    pub const IDLE: Self = Self {
        playpos_ns: 0, frames: 0,
        flags: FailureFlags {
            load_failed: false, load_timed_out: false, demux_failed: false, demux_io_failed: false,
        },
        decoder_revoked: false, native_audio_refused: false, load_abandoned: false, file_size: 0,
        applied_rate_permille: crate::RATE_NORMAL, speed_audio_ok: None, vpres_count: 0,
        vpres_gap: 0, pts_shift: 0,
    };
}

impl Default for FeedRaw {
    fn default() -> Self { Self::IDLE }
}

/// One sample of the feed: the mapped facts and the raw ones. `Copy`, addressed by the attempt
/// that is ingesting it. L3c adds the demux ABR and the diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FeedFacts {
    pub attempt: AttemptFacts,
    pub raw: FeedRaw,
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
    /// One native Pause transition before an in-place seek.
    fn prepare_seek_pause(&self, mt: &MainThread) -> SeekPause;
    fn begin_seek_discontinuity(&self) -> bool;
    fn apply_inplace_seek_pre(&mut self, mt: &MainThread, target_ns: i64, mode: SeekMode,
        coalesced: u32);
    fn apply_inplace_seek_post(&mut self, target_ns: i64, now: u32);
}

/// The Load-side verbs and pure queries of the feed backend.
pub trait FeedLoad {
    /// Physical Load and Play transition. `no_audio` is the common player's no-audio arming.
    fn tick_load(&mut self, mt: &MainThread, now: u32, segmented_hls: bool, budget: Duration,
        speed_deadline_ms: u32, no_audio: bool) -> LoadTick;
    fn tick_presentation(&mut self, mt: &MainThread, segmented_hls: bool, immersive: bool)
        -> PresentationTick;
    /// Whether native construction is callable yet.
    fn ctor_gate(&mut self, mt: &MainThread) -> CtorGate;
    /// Load verdicts the live session still owns, in arrival order.
    fn take_load_outcomes(&mut self) -> Vec<AcceptedLoadOutcome>;
    /// Prime the live clock for a feed that has presented nothing yet; false with no session.
    fn arm_live_clock_prime(&mut self) -> bool;
    fn native_audio_stall_fired(&mut self, now: u32) -> bool;
    fn note_missing_era_callback(&mut self);
    /// Feed the dev firmware-refusal probe with route facts.
    fn inject_refuse601(&mut self, generation: u32, preview: bool, refused_for: &AtomicU32);
    /// Publish native queue and frame diagnostics; `diag_readout_on` is the panel's switch.
    fn publish_diag(&mut self, now: u32, last_frames: &AtomicI32, diag_readout_on: bool);
    fn abandon_preview(&mut self);
    /// Mark the foreground as following this Load, for `attempt` only; false for any other.
    fn mark_foreground_follows_load(&mut self, attempt: AttemptId) -> bool;

    /// One sample of what the feed observed: the sparse facts the HUD and the route read and the
    /// raw ones a decision reads. Ungated: the caller asks [`Self::is_live`] first, because a feed
    /// with no session answers its idle mailboxes, which is not an observation of any attempt.
    fn sample(&self) -> FeedFacts;

    fn stage(&self) -> Option<Stage>;
    /// Whether the live session has yet to leave its Load stage.
    fn loading(&self) -> bool;
    fn load_thread_finished(&self) -> bool;
    /// `(still loading, load thread finished)` of the live session.
    fn preview_load_join_state(&self) -> Option<(bool, bool)>;
    fn uses_stream_queues(&self) -> bool;
    fn foreground_follows_load(&self) -> bool;
    /// Native readiness for a route or action claim.
    fn route_actions_ready(&self) -> bool;
    /// Whether a session is live.
    fn is_live(&self) -> bool;
    fn active_attempt(&self) -> Option<AttemptId>;
}

/// One buffer-feed backend as the player drives it. Later steps of the seam add the command,
/// readout, open and stop verbs here; today it is the seek and Load halves.
pub trait FeedControl: FeedSeek + FeedLoad {}

/// The physical drivers one composition supplies, behind one object. It is the runner's
/// [`DriverSet`] on the main thread, and `feed` is the buffer-feed half for the pump's direct
/// verbs; a set without a feed answers `None`.
pub trait PhysicalSet: DriverSet<Context = MainThread> {
    fn feed(&mut self) -> Option<&mut dyn FeedControl>;
}

/// The set that installs no physical driver: every backend is absent, every dispatch is refused
/// as unavailable, and there is no feed.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPhysical;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
