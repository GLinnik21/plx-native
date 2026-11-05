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

use std::sync::atomic::{AtomicI32, AtomicU32};
use std::time::Duration;

use plx_base::task::MainThread;

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
