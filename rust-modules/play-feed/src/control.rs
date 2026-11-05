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
    AcceptedLoadOutcome, CtorGate, FeedCommand, FeedControl, FeedFacts, FeedLoad, FeedSeek,
    LoadTick, PausedSeekStep, PresentationTick, SeekMode, SeekPause, Stage, StuckSeekAction,
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

    fn sample(&self) -> FeedFacts { feed_pump::native_feed_facts() }

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

impl FeedControl for FeedBackend {
    fn command(&mut self, command: FeedCommand) {
        match command {
            FeedCommand::Seek { epoch, target_ns, display_ns } => {
                feed_pump::seek_requested(epoch, target_ns, display_ns);
            }
            FeedCommand::AbandonSeek => feed_pump::seek_abandoned(),
            // The take addresses the newest request it belongs to; the feed clears its copy of the
            // pending target whatever that is (a reload zeroes the feed's own epoch, and a copy
            // left armed would hold the AU gate shut).
            FeedCommand::SeekTaken { epoch: _ } => feed_pump::seek_taken(),
            FeedCommand::SeekRequeued { target_ns } => feed_pump::seek_requeued(target_ns),
            FeedCommand::ArmOpen { target_ns, disp_base, playpos_ns } => {
                feed_pump::arm_open(target_ns, disp_base, playpos_ns);
            }
            FeedCommand::NoteFailure(flag) => feed_pump::note_failure(flag),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_executor::{
        FeedHooks, LaunchHooks, NativeHooks, PrepareHooks, ReleaseHooks,
    };
    use crate::pcm_lead::PcmLead;
    use crate::send_ptr::SendPtr;
    use crate::shared::{FEED_SHARED, FEED_TX};
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

    /// `command(Seek)` makes the stores the player's `request_seek` made itself, and the sample
    /// carries what the player derives its `seeking` from: the epoch the landing reports (only
    /// once playback resumed, so a request is in flight until then), and the pipeline's own end of
    /// stream, which the request clears. `AbandonSeek` is the give-up's two stores, and the
    /// session's reset zeroes the epochs with the rest of the seek family.
    #[test]
    fn the_seek_commands_leave_the_mailboxes_as_the_players_own_stores_did() {
        use std::sync::atomic::Ordering::Relaxed;
        let _serial = plx_base::testlock::serial();
        FEED_SHARED.reset_session();
        FEED_TX.reset();
        FEED_SHARED.pipeline_eos.store(true, Relaxed);
        let mut backend = backend();
        let control: &mut dyn FeedControl = &mut backend;

        control.command(FeedCommand::Seek { epoch: 1, target_ns: 40_000_000_000, display_ns: 41_000_000_000 });
        control.command(FeedCommand::Seek { epoch: 2, target_ns: 50_000_000_000, display_ns: 50_000_000_000 });
        assert!(FEED_SHARED.seeking.load(Relaxed), "the request arms the flag the feed reads");
        assert!(!FEED_SHARED.pipeline_eos.load(Relaxed), "a seek back from the end un-ends it");
        assert_eq!(FEED_SHARED.seek_display_ns.load(Relaxed), 50_000_000_000);
        assert_eq!(FEED_TX.seek_to_ns.load(Relaxed), 50_000_000_000, "the newest target wins");
        let raw = control.sample().raw;
        assert_eq!(raw.seek_landed_epoch, 0, "requested is not landed");
        assert!(!raw.pipeline_eos);

        control.command(FeedCommand::AbandonSeek);
        assert!(!FEED_SHARED.seeking.load(Relaxed));
        assert_eq!(FEED_SHARED.seek_display_ns.load(Relaxed), -1, "no stale frozen playhead");
        assert_eq!(FEED_TX.seek_to_ns.load(Relaxed), 50_000_000_000,
            "giving up leaves the pending target to the pump, as it always did");

        control.command(FeedCommand::Seek { epoch: 3, target_ns: 1, display_ns: 1 });
        FEED_SHARED.seek_landed_epoch
            .store(FEED_SHARED.seek_epoch.load(Relaxed), std::sync::atomic::Ordering::Release);
        assert_eq!(control.sample().raw.seek_landed_epoch, 3, "the landing reports the newest request");
        FEED_SHARED.reset_session();
        FEED_TX.reset();
        assert_eq!(control.sample().raw.seek_landed_epoch, 0, "a new session has landed nothing");
        assert_eq!(FEED_SHARED.seek_epoch.load(Relaxed), 0);
    }

    /// The feed's copy of the pending seek gates feeding, and only commands write it: a take clears
    /// it whatever epoch the feed holds (a reload zeroes the feed's own, and a copy left armed would
    /// hold every AU back), and a requeue re-arms it WITHOUT being a request: the in-flight flag,
    /// the display target and the pipeline's end of stream keep the values the reload left, exactly
    /// as the pump's own re-arm stores did.
    #[test]
    fn the_take_clears_the_feed_gate_and_the_requeue_rearms_only_it() {
        use std::sync::atomic::Ordering::Relaxed;
        let _serial = plx_base::testlock::serial();
        FEED_SHARED.reset_session();
        FEED_TX.reset();
        let mut backend = backend();
        let control: &mut dyn FeedControl = &mut backend;

        control.command(FeedCommand::Seek { epoch: 9, target_ns: 7_000_000_000, display_ns: 7_000_000_000 });
        assert_eq!(FEED_TX.seek_to_ns.load(Relaxed), 7_000_000_000);
        // The take names an epoch the feed does not hold (a reload zeroed it): it clears anyway.
        FEED_SHARED.seek_epoch.store(0, Relaxed);
        control.command(FeedCommand::SeekTaken { epoch: 9 });
        assert_eq!(FEED_TX.seek_to_ns.load(Relaxed), -1, "the feed gate reads no seek armed");
        control.command(FeedCommand::SeekTaken { epoch: 9 });
        assert_eq!(FEED_TX.seek_to_ns.load(Relaxed), -1, "a take with nothing armed changes nothing");

        // A reload reset the session; a newer tap is carried across it.
        FEED_SHARED.reset_session();
        FEED_TX.reset_for_reload();
        control.command(FeedCommand::SeekRequeued { target_ns: 8_000_000_000 });
        assert_eq!(FEED_TX.seek_to_ns.load(Relaxed), 8_000_000_000, "the carried target is armed again");
        assert!(!FEED_SHARED.seeking.load(Relaxed), "re-arming a target is not a request");
        assert_eq!(FEED_SHARED.seek_display_ns.load(Relaxed), -1);
        assert_eq!(FEED_SHARED.seek_epoch.load(Relaxed), 0);
        FEED_SHARED.reset_session();
        FEED_TX.reset();
    }

    /// `ArmOpen` stores what the player's old `arm_seek` stored: the demuxer's open target, the display base
    /// and the position the HUD reads until frames land, and nothing of the pending-seek mailbox.
    #[test]
    fn arm_open_leaves_the_mailboxes_as_the_players_own_arm_did() {
        use std::sync::atomic::Ordering::Relaxed;
        let _serial = plx_base::testlock::serial();
        FEED_SHARED.reset_session();
        FEED_TX.reset();
        FEED_SHARED.disp_base.store(123, Relaxed);
        FEED_SHARED.playpos_ns.store(456, Relaxed);
        let mut backend = backend();
        let control: &mut dyn FeedControl = &mut backend;

        control.command(FeedCommand::ArmOpen {
            target_ns: 90_000_000_000, disp_base: 0, playpos_ns: 90_000_000_000,
        });
        assert_eq!(FEED_SHARED.seek_to_ns.load(Relaxed), 90_000_000_000, "the demuxer's open target");
        assert_eq!(FEED_SHARED.disp_base.load(Relaxed), 0, "direct-play reads content time");
        assert_eq!(FEED_SHARED.playpos_ns.load(Relaxed), 90_000_000_000, "instant HUD feedback");
        assert_eq!(FEED_TX.seek_to_ns.load(Relaxed), -1, "the feed gate is not the open target");
        assert!(!FEED_SHARED.seeking.load(Relaxed), "arming an open is not a seek request");

        // The three values are stored as given, each in its own cell.
        control.command(FeedCommand::ArmOpen {
            target_ns: 11_000_000_000, disp_base: 7_000_000_000, playpos_ns: 8_000_000_000,
        });
        assert_eq!(FEED_SHARED.seek_to_ns.load(Relaxed), 11_000_000_000);
        assert_eq!(FEED_SHARED.disp_base.load(Relaxed), 7_000_000_000);
        assert_eq!(FEED_SHARED.playpos_ns.load(Relaxed), 8_000_000_000);

        FEED_SHARED.reset_session();
        FEED_TX.reset();
    }

    /// The feed's request to seek again reaches the player as a numbered raw fact, together with
    /// the in-place target it aimed at, and the session's reset forgets both.
    #[test]
    fn the_sample_carries_the_requeue_and_the_in_place_target() {
        use std::sync::atomic::Ordering::{Release, Relaxed};
        let _serial = plx_base::testlock::serial();
        FEED_SHARED.reset_session();
        FEED_TX.reset();
        let mut backend = backend();
        let control: &mut dyn FeedControl = &mut backend;
        let idle = control.sample().raw;
        assert_eq!(idle.seek_requeue, plx_playback_contract::feed::SeekRequeue::NONE);
        assert_eq!(idle.seek_target_ns, None);

        FEED_SHARED.seek_target_ns.store(61_000_000_000, Relaxed);
        FEED_SHARED.seek_requeue_target.store(61_000_000_000, Relaxed);
        FEED_SHARED.seek_requeue_epoch.fetch_add(1, Release);
        let raw = control.sample().raw;
        assert_eq!(raw.seek_target_ns, Some(61_000_000_000));
        assert_eq!(raw.seek_requeue.epoch, 1);
        assert_eq!(raw.seek_requeue.target_ns, 61_000_000_000);

        FEED_SHARED.reset_session();
        FEED_TX.reset();
        let raw = control.sample().raw;
        assert_eq!(raw.seek_requeue.epoch, 0, "a new session has requested nothing");
        assert_eq!(raw.seek_target_ns, None);
    }

    /// A failure the player notes is written inside the feed, in the callbacks' ordering, and
    /// comes back through the sample's flags (the readout reads nothing else), alongside the code
    /// that explains it: the flags are loaded first, so a code published before its flag is never
    /// read as 0. The session's reset takes the notes with the rest.
    #[test]
    fn a_noted_failure_returns_through_the_samples_flags() {
        use plx_playback_contract::feed::{FailureFlag, FailureFlags};
        use std::sync::atomic::Ordering::Relaxed;
        let _serial = plx_base::testlock::serial();
        FEED_SHARED.reset_session();
        FEED_TX.reset();
        let mut backend = backend();
        let control: &mut dyn FeedControl = &mut backend;
        assert_eq!(control.sample().raw.flags, FailureFlags::default());

        control.command(FeedCommand::NoteFailure(FailureFlag::LoadFailed));
        control.command(FeedCommand::NoteFailure(FailureFlag::LoadTimedOut));
        let raw = control.sample().raw;
        assert!(raw.flags.load_failed && raw.flags.load_timed_out, "the notes come back as flags");
        assert!(!raw.flags.demux_io_failed && !raw.flags.demux_failed, "and only those");
        assert_eq!(control.sample().attempt.runtime_failure,
            Some(plx_playback_contract::readout::RuntimeFailure::LoadTimeout),
            "the mapped half collapses the same flags");

        FEED_SHARED.dg_cb_err_num.store(601, Relaxed);
        FEED_SHARED.dg_http_status.store(404, Relaxed);
        control.command(FeedCommand::NoteFailure(FailureFlag::DemuxIoFailed));
        let raw = control.sample().raw;
        assert!(raw.flags.demux_io_failed);
        assert_eq!((raw.cb_err_num, raw.http_status), (601, 404), "the codes ride the same sample");

        FEED_SHARED.reset_session();
        assert_eq!(control.sample().raw.flags, FailureFlags::default(), "a reset retires the notes");
        assert_eq!(control.sample().raw.cb_err_num, 0);
    }

    /// The pipeline's decoder revocations and native-audio refusals are COUNTS the feed keeps for
    /// the session: each callback adds one, the sample carries the count, the session's reset zeroes it.
    #[test]
    fn the_sample_counts_the_one_shot_edges_and_a_reset_zeroes_them() {
        let _serial = plx_base::testlock::serial();
        FEED_SHARED.reset_session();
        FEED_TX.reset();
        let backend = backend();
        let control: &dyn FeedControl = &backend;
        assert_eq!((control.sample().raw.decoder_revocations, control.sample().raw.native_audio_refusals), (0, 0));
        FEED_SHARED.raise_decoder_revoked();
        FEED_SHARED.raise_decoder_revoked();
        FEED_SHARED.raise_native_audio_refused();
        let raw = control.sample().raw;
        assert_eq!((raw.decoder_revocations, raw.native_audio_refusals), (2, 1));
        FEED_SHARED.reset_session();
        let raw = control.sample().raw;
        assert_eq!((raw.decoder_revocations, raw.native_audio_refusals), (0, 0));
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
