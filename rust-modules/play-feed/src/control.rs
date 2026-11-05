//! The buffer-feed backend behind the contract's [`FeedControl`] and [`DriverSet`]: the player's
//! seek and Load verbs reach the physical feed through this one owner of its native session and
//! its driver, and every method is the body of the function the player's adapter called directly
//! before the seam existed. A forwarding layer only; the verbs, their borrows and their answers
//! are unchanged.

use crate::feed_driver::{FeedDriver, FeedTransportAction};
use crate::feed_pump;
use crate::feed_resources::FeedResources;
use crate::native_executor::NativePrepared;
use plx_base::task::MainThread;
use plx_playback_contract::feed::{
    AcceptedLoadOutcome, CtorGate, FeedControl, FeedLoad, FeedSeek, LoadTick, PausedSeekStep,
    PresentationTick, SeekMode, SeekPause, Stage, StuckSeekAction,
};
use plx_playback_contract::{
    AttemptId, BackendEvent, BackendKind, Capabilities, CommandId, DispatchResult, DriverSet,
    ExitDisposition, Failure, Operation, ReleaseStatus, SourceKey, StopReason,
};
use std::sync::atomic::{AtomicI32, AtomicU32};
use std::time::Duration;

/// The physical buffer-feed backend: the native session slot with its Load and release
/// resources, and the driver that answers the runner for it.
pub struct FeedBackend {
    resources: FeedResources,
    driver: FeedDriver,
}

impl FeedBackend {
    pub fn new(resources: FeedResources) -> Self { Self { resources, driver: FeedDriver::new() } }

    /// The session resources, for the open, stop and release paths that do not yet go through
    /// the trait.
    pub fn resources(&self) -> &FeedResources { &self.resources }

    pub fn resources_mut(&mut self) -> &mut FeedResources { &mut self.resources }

    /// Escrow the prepared native open for the Start the runner will address to `attempt`.
    pub fn stage_prepared(&mut self, attempt: AttemptId, key: SourceKey, prepared: NativePrepared)
        -> Result<(), Failure> {
        self.driver.stage(attempt, key, prepared)
    }

    /// The stop the driver asked the player for, once.
    pub fn take_stop_request(&mut self) -> Option<(AttemptId, StopReason)> {
        self.driver.take_stop_request()
    }

    /// The next native transport action the driver queued for the player's transport door.
    pub fn take_transport_action(&mut self) -> Option<FeedTransportAction> {
        self.driver.take_transport_action()
    }

    /// Report what the transport door did with `action`, so the driver can answer the runner.
    pub fn note_transport_result(&mut self, mt: &MainThread, action: FeedTransportAction,
        accepted: bool, native_acknowledged: bool) {
        self.driver.note_transport_result(&mut self.resources, mt, action, accepted,
            native_acknowledged);
    }

    /// The native release of `attempt` progressed to `status`.
    pub fn note_release(&mut self, attempt: AttemptId, status: ReleaseStatus) {
        self.driver.note_release(attempt, status);
    }

    /// The Load for `attempt` settled.
    pub fn note_load_outcome(&mut self, attempt: AttemptId, ok: bool) {
        self.driver.note_load_outcome(attempt, ok);
    }
}

/// The feed alone composes with the runner directly, as a URI-only set does: it answers only for
/// [`BackendKind::Feed`], and a larger composition wraps it beside the URI driver.
impl DriverSet for FeedBackend {
    type Context = MainThread;

    fn capabilities(&self, backend: BackendKind) -> Option<Capabilities> {
        (backend == BackendKind::Feed).then(|| self.driver.capabilities())
    }

    fn dispatch(&mut self, backend: BackendKind, mt: &MainThread, attempt: AttemptId,
        command: CommandId, operation: Operation) -> DispatchResult {
        if backend != BackendKind::Feed { return DispatchResult::Refused(Failure::Unavailable); }
        self.driver.dispatch(&mut self.resources, mt, attempt, command, operation)
    }

    fn poll(&mut self, _mt: &MainThread) -> Vec<BackendEvent> {
        self.driver.poll(&mut self.resources)
    }

    fn discard_staged(&mut self, backend: BackendKind, attempt: AttemptId, key: SourceKey) {
        if backend == BackendKind::Feed { self.driver.discard_staged(attempt, key); }
    }

    fn exit_disposition(&self) -> ExitDisposition {
        self.driver.exit_disposition(&self.resources)
    }
}

impl FeedSeek for FeedBackend {
    fn paused_seek_step(&mut self, now: u32, route_flight: bool, claim_owns_pause: bool)
        -> PausedSeekStep {
        feed_pump::paused_seek_step(&mut self.resources, now, route_flight, claim_owns_pause)
    }

    fn stuck_seek_due(&mut self, now: u32, feed_allowed: bool, route_flight: bool, budget_ms: u32)
        -> bool {
        feed_pump::stuck_seek_due(&mut self.resources, now, feed_allowed, route_flight, budget_ms)
    }

    fn retry_stuck_seek(&mut self, now: u32, target_ns: i64, can_retry: bool) -> StuckSeekAction {
        feed_pump::retry_stuck_seek(&mut self.resources, now, target_ns, can_retry)
    }

    fn pending_seek_ready(&self, seek_reloads: bool) -> bool {
        feed_pump::pending_seek_ready(&self.resources, seek_reloads)
    }

    fn prepare_seek_pause(&self, mt: &MainThread) -> SeekPause {
        feed_pump::prepare_seek_pause(&self.resources, mt)
    }

    fn begin_seek_discontinuity(&self) -> bool {
        feed_pump::begin_seek_discontinuity(&self.resources)
    }

    fn apply_inplace_seek_pre(&mut self, mt: &MainThread, target_ns: i64, mode: SeekMode,
        coalesced: u32) {
        feed_pump::apply_inplace_seek_pre(&mut self.resources, mt, target_ns, mode, coalesced);
    }

    fn apply_inplace_seek_post(&mut self, target_ns: i64, now: u32) {
        feed_pump::apply_inplace_seek_post(&mut self.resources, target_ns, now);
    }
}

impl FeedLoad for FeedBackend {
    fn tick_load(&mut self, mt: &MainThread, now: u32, segmented_hls: bool, budget: Duration,
        speed_deadline_ms: u32, no_audio: bool) -> LoadTick {
        feed_pump::tick_load(&mut self.resources, mt, now, segmented_hls, budget,
            speed_deadline_ms, no_audio)
    }

    fn tick_presentation(&mut self, mt: &MainThread, segmented_hls: bool, immersive: bool)
        -> PresentationTick {
        feed_pump::tick_presentation(&mut self.resources, mt, segmented_hls, immersive)
    }

    fn ctor_gate(&mut self, mt: &MainThread) -> CtorGate {
        feed_pump::ctor_gate(&mut self.resources, mt)
    }

    fn take_load_outcomes(&mut self) -> Vec<AcceptedLoadOutcome> {
        self.resources.take_load_outcomes()
    }

    fn arm_live_clock_prime(&mut self) -> bool {
        let Some(engine) = self.resources.engine() else { return false; };
        crate::native_executor::arm_live_clock_prime(engine);
        true
    }

    fn native_audio_stall_fired(&mut self, now: u32) -> bool {
        feed_pump::native_audio_stall_fired(&mut self.resources, now)
    }

    fn note_missing_era_callback(&mut self) {
        feed_pump::note_missing_era_callback(&mut self.resources);
    }

    fn inject_refuse601(&mut self, generation: u32, preview: bool, refused_for: &AtomicU32) {
        feed_pump::inject_refuse601(&mut self.resources, generation, preview, refused_for);
    }

    fn publish_diag(&mut self, now: u32, last_frames: &AtomicI32, diag_readout_on: bool) {
        feed_pump::publish_diag(&mut self.resources, now, last_frames, diag_readout_on);
    }

    fn abandon_preview(&mut self) { self.resources.abandon_preview(); }

    fn mark_foreground_follows_load(&mut self, attempt: AttemptId) -> bool {
        self.resources.mark_foreground_follows_load(attempt)
    }

    fn stage(&self) -> Option<Stage> { self.resources.stage() }
    fn loading(&self) -> bool { self.resources.loading() }
    fn load_thread_finished(&self) -> bool { self.resources.load_thread_finished() }
    fn preview_load_join_state(&self) -> Option<(bool, bool)> {
        self.resources.preview_load_join_state()
    }
    fn uses_stream_queues(&self) -> bool { self.resources.uses_stream_queues() }
    fn foreground_follows_load(&self) -> bool { self.resources.foreground_follows_load() }
    fn route_actions_ready(&self) -> bool { self.resources.route_actions_ready() }
    fn is_live(&self) -> bool { self.resources.is_live() }
    fn active_attempt(&self) -> Option<AttemptId> { self.resources.active_attempt() }
}

impl FeedControl for FeedBackend {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_executor::{
        FeedHooks, LaunchHooks, NativeHooks, PrepareHooks, ReleaseHooks,
    };
    use crate::pcm_lead::PcmLead;
    use crate::send_ptr::SendPtr;
    use crate::shared::FEED_SHARED;
    use plx_platform::tv::sink::{NoSink, VideoSink};
    use std::os::raw::c_char;

    fn sink() -> &'static dyn VideoSink { &NoSink }

    /// Hooks the tests never call: with no session installed no verb reaches a native seam.
    const HOOKS: NativeHooks = NativeHooks {
        prepare: PrepareHooks {
            sink, on_exported_window_create: || {}, load_refusal_latch: 0,
        },
        launch: LaunchHooks {
            demux_transport: || unreachable!("no session, no demux"),
            load_thread: |_: SendPtr<c_char>, _, _, _: PcmLead| {},
            sink,
        },
        feed: FeedHooks {
            sink, mirror_playstate: |_, _, _| {}, mute_for_preroll: |_| {}, feed_clock_us: || 0,
        },
        release: ReleaseHooks {
            raw_sink: sink,
            plane: plx_platform::tv::plane::installed(),
            on_release_step: |_| {},
            on_exported_window_destroy: || {},
            spawn_unload: |_| None,
        },
    };

    fn backend() -> FeedBackend {
        FeedBackend::new(FeedResources::new(&FEED_SHARED, &HOOKS))
    }

    /// A forwarded verb gives the answer of the function it wraps: with no session installed the
    /// constructor gate is still awaiting, with no Load budget epoch to check.
    #[test]
    fn a_forwarded_verb_answers_like_the_function_it_wraps() {
        // SAFETY: a host test; nothing here touches the thread-affine state the token guards.
        let mt = unsafe { MainThread::assume() };
        let mut backend = backend();
        let wrapped = feed_pump::ctor_gate(backend.resources_mut(), &mt);
        assert_eq!(wrapped, CtorGate::Awaiting { budget_epoch: None });
        let control: &mut dyn FeedControl = &mut backend;
        assert_eq!(control.ctor_gate(&mt), wrapped);
    }

    /// The pure queries read the same session slot as the resources they forward to.
    #[test]
    fn the_queries_read_the_same_slot_as_the_resources() {
        let backend = backend();
        let control: &dyn FeedControl = &backend;
        assert!(!control.is_live());
        assert_eq!(control.stage(), backend.resources().stage());
        assert_eq!(control.active_attempt(), backend.resources().active_attempt());
        assert_eq!(control.loading(), backend.resources().loading());
        assert_eq!(control.route_actions_ready(), backend.resources().route_actions_ready());
        assert!(!control.pending_seek_ready(false));
        assert!(!control.begin_seek_discontinuity());
    }

    /// The backend is the runner's single-backend driver set: it answers for the feed alone, and
    /// with no session installed it has nothing to report and nothing to preserve at exit.
    #[test]
    fn the_backend_is_a_feed_only_driver_set_that_is_idle_without_a_session() {
        // SAFETY: a host test; nothing here touches the thread-affine state the token guards.
        let mt = unsafe { MainThread::assume() };
        let mut backend = backend();
        assert!(backend.capabilities(BackendKind::Feed).is_some_and(|caps| caps.packet_feed));
        assert_eq!(backend.capabilities(BackendKind::Uri), None);
        assert!(backend.poll(&mt).is_empty());
        assert_eq!(backend.exit_disposition(), ExitDisposition::Clean);
        backend.discard_staged(BackendKind::Feed, AttemptId::new(1, 1), SourceKey::new(1));
        assert_eq!(backend.take_stop_request(), None);
        assert_eq!(backend.take_transport_action(), None);
    }
}
