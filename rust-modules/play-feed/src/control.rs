//! The buffer-feed backend behind the contract's [`FeedControl`] and [`DriverSet`]: the player's
//! seek, Load and stop verbs reach the physical feed through this one owner of its native session
//! and its driver, and every method is the body of the function the player's adapter called
//! directly before the seam existed. A forwarding layer, except `sample`, which answers only for a
//! live session and adds the Engine's Load-side half (`FeedRaw::load`) to the mailbox sample, the
//! open, which parks the prepared open between its phases, and the stop, which keeps the
//! [`NativeStop`] in progress between its phases; the verbs, their
//! borrows and their answers are otherwise unchanged.

use crate::feed_driver::FeedDriver;
use crate::feed_pump;
use crate::feed_resources::FeedResources;
use crate::feed_state::PendingRelease;
use crate::native_executor::{
    begin_native_open, prepare_native_open, reap_abandoned_load_resource, NativePrepared,
    NativeStop, PrepareRefusal,
};
use crate::open::PhysicalOpen;
use crate::shared::FEED_TX;
use crate::side_reader::SideReaderOpen;
use plx_base::task::MainThread;
use plx_playback_contract::feed::{
    AcceptedLoadOutcome, CtorGate, DvWithdrawal, FeedCommand, FeedControl, FeedFacts, FeedLoad, FeedSeek,
    FeedOpen, FeedStop, FeedTransportAction, LoadTick, OpenRequest, PausedSeekStep,
    PresentationTick, ReleaseHandoff, ResetScope, SeekMode, SeekPause, StuckSeekAction,
    UnblockFacts, UserPauseOutcome, UserResumeOutcome,
};
use plx_playback_contract::demux::DemuxSession;
use plx_playback_contract::{
    AttemptId, BackendEvent, BackendKind, Capabilities, CommandId, DispatchResult, DriverSet,
    ExitDisposition, Failure, Operation, SourceKey,
};
use std::sync::atomic::{AtomicI32, AtomicU32};

/// The physical buffer-feed backend: the native session slot with its Load and release
/// resources, and the driver that answers the runner for it.
pub struct FeedBackend {
    resources: FeedResources,
    driver: FeedDriver,
    /// The stop in progress ([`FeedStop`]): the Engine taken out of the resources, owned through
    /// the phases. At most one exists.
    stop: Option<NativeStop>,
    /// A parked release whose `Unload` worker returned and was joined, waiting for its
    /// main-thread tail ([`FeedStop::run_release_tail`]).
    returned_release: Option<PendingRelease>,
    /// The prepared open between [`FeedOpen::open_prepare`] and [`FeedOpen::open_stage`] (or
    /// [`FeedOpen::open_abort`]): it owns the clock and native-epoch guards, a payload and a side
    /// reader, so it stays here instead of crossing the seam. At most one exists.
    prepared: Option<NativePrepared>,
}

impl FeedBackend {
    pub fn new(resources: FeedResources) -> Self {
        Self { resources, driver: FeedDriver::new(), stop: None, returned_release: None, prepared: None }
    }

    /// The session resources, for the test fixtures that install, take and inspect an Engine.
    pub fn resources(&self) -> &FeedResources { &self.resources }

    pub fn resources_mut(&mut self) -> &mut FeedResources { &mut self.resources }
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

    fn prepare_seek_pause(&mut self, mt: &MainThread) -> SeekPause {
        feed_pump::prepare_seek_pause(&mut self.resources, mt)
    }

    fn begin_seek_discontinuity(&mut self) -> bool {
        feed_pump::begin_seek_discontinuity(&mut self.resources)
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
    fn tick_load(&mut self, mt: &MainThread, now: u32, segmented_hls: bool,
        speed_deadline_ms: u32, no_audio: bool) -> LoadTick {
        feed_pump::tick_load(&mut self.resources, mt, now, segmented_hls, speed_deadline_ms, no_audio)
    }

    fn tick_presentation(&mut self, mt: &MainThread, segmented_hls: bool, immersive: bool)
        -> PresentationTick {
        feed_pump::tick_presentation(&mut self.resources, mt, segmented_hls, immersive)
    }

    fn ctor_gate(&mut self, mt: &MainThread) -> CtorGate {
        feed_pump::ctor_gate(&mut self.resources, mt)
    }

    /// The driver learns each verdict as the player takes it, so a Load that failed for the active
    /// attempt becomes that attempt's `Failed` observation on the runner's next poll.
    fn take_load_outcomes(&mut self) -> Vec<AcceptedLoadOutcome> {
        let outcomes = self.resources.take_load_outcomes();
        for outcome in &outcomes {
            self.driver.note_load_outcome(outcome.attempt, outcome.ok);
        }
        outcomes
    }

    fn native_audio_stall_fired(&mut self, now: u32) -> bool {
        feed_pump::native_audio_stall_fired(&mut self.resources, now)
    }

    fn dv_landing_due(&mut self, now: u32, route_flight: bool) -> Option<DvWithdrawal> {
        feed_pump::dv_landing_due(&mut self.resources, now, route_flight)
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

    fn defer_preview_join(&mut self) -> bool { self.resources.defer_preview_join() }

    fn sample(&self) -> Option<FeedFacts> {
        self.resources.is_live().then(|| {
            let mut facts = feed_pump::native_feed_facts();
            facts.raw.load = self.resources.load_raw();
            facts
        })
    }

    fn has_abandoned_load(&self) -> bool { self.resources.has_abandoned_load() }

    fn reap_abandoned_load(&mut self, mt: &MainThread) -> bool {
        let hooks = self.resources.hooks();
        reap_abandoned_load_resource(&mut self.resources, mt, &hooks.release)
    }
}

impl FeedOpen for FeedBackend {
    fn open_begin(&mut self, mt: &MainThread) -> bool {
        begin_native_open(&mut self.resources, mt)
    }

    fn open_prepare(&mut self, mt: &MainThread, request: OpenRequest) -> Result<u32, Failure> {
        debug_assert!(self.prepared.is_none(), "an open is staged or aborted before the next one");
        let hooks = &self.resources.hooks().prepare;
        let side_reader = Box::new(SideReaderOpen::new(request.side_reader, hooks.side_reader_job));
        let prepared = prepare_native_open(
            mt, self.resources.shared(), request.start_paused, request.seek_preroll,
            request.stream.map(PhysicalOpen::from), request.sample, request.is_h265,
            request.session_rate, side_reader, request.no_audio,
            request.dv_landing, hooks,
        ).map_err(|refusal| match refusal {
            PrepareRefusal::ClockBusy | PrepareRefusal::NativeBusy => Failure::Busy,
            PrepareRefusal::Refused => Failure::Refused,
        })?;
        let epoch = prepared.native_epoch();
        self.prepared = Some(prepared);
        Ok(epoch)
    }

    fn open_stage(&mut self, attempt: AttemptId, key: SourceKey, session: Box<dyn DemuxSession>)
        -> Result<(), Failure> {
        let Some(mut prepared) = self.prepared.take() else { return Err(Failure::Stale); };
        prepared.bind_demux_session(session);
        self.driver.stage(attempt, key, prepared)
    }

    fn open_abort(&mut self) { self.prepared = None; }
}

impl FeedBackend {
    fn stop_mut(&mut self) -> &mut NativeStop {
        self.stop.as_mut().expect("stop_begin ran before this phase of the stop")
    }
}

impl FeedStop for FeedBackend {
    fn stop_begin(&mut self, terminal: bool) -> Option<FeedFacts> {
        self.resources.shared().stop_hls_clock();
        let last = self.sample();
        self.stop = Some(NativeStop::take(&mut self.resources, terminal));
        last
    }

    fn stop_side_reader(&mut self, for_reload: bool) {
        self.stop_mut().stop_side_reader(for_reload);
    }

    fn stop_unblock(&mut self) -> UnblockFacts {
        let stop = self.stop_mut();
        stop.unblock_join_close();
        let load_abandoned = stop.load_abandoned();
        UnblockFacts { attempt: feed_pump::native_attempt_facts(), load_abandoned }
    }

    fn stop_report_dispatched(&mut self) { self.stop_mut().report_dispatched(); }

    fn stop_release(&mut self, mt: &MainThread, for_reload: bool) {
        let hooks = self.resources.hooks();
        let stop = self.stop.as_mut().expect("stop_begin ran before this phase of the stop");
        stop.release_and_drain(mt, &mut self.resources, for_reload, &hooks.release);
    }

    fn stop_finish(&mut self, for_reload: bool) -> Option<ReleaseHandoff> {
        let stop = self.stop.take().expect("stop_begin ran before this phase of the stop");
        let (attempt, status) = (stop.attempt(), stop.release_status());
        let handoff = stop.finish(for_reload).map(|release| {
            let handoff = ReleaseHandoff { epoch: release.epoch(), at: release.handed_off_at() };
            self.resources.park_release(release);
            handoff
        });
        self.driver.note_release(attempt, status);
        handoff
    }

    fn reset(&mut self, scope: ResetScope, common: &mut dyn FnMut()) {
        self.resources.shared().reset_scoped(scope, common);
        // The transport's half of the same teardown, after the common stores (as the player ran it
        // when it reached for the transport itself): a real stop clears the viewer's Pause, the
        // preroll and the owed re-pause; a reload keeps them for the new engine.
        match scope {
            ResetScope::Stop => FEED_TX.reset(),
            ResetScope::Reload => FEED_TX.reset_for_reload(),
        }
    }

    fn take_returned_release(&mut self) -> Option<u32> {
        let mut release = self.resources.take_finished_release()?;
        release.join_returned_worker();
        let epoch = release.epoch();
        // A release the machine did not ask the tail of is dropped here, with its Engine, by the
        // next one or by the exit's leak.
        self.returned_release = Some(release);
        Some(epoch)
    }

    fn run_release_tail(&mut self, mt: &MainThread, epoch: u32) -> bool {
        let Some(release) = self.returned_release.take() else { return false; };
        debug_assert_eq!(release.epoch(), epoch);
        let attempt = release.attempt();
        let status = crate::native_executor::run_release_tail(
            mt, release, self.resources.shared(), &self.resources.hooks().release);
        self.driver.note_release(attempt, status);
        true
    }

    fn leak_for_exit(&mut self) {
        if let Some(engine) = self.resources.take() { std::mem::forget(engine); }
        if let Some(release) = self.returned_release.take() { std::mem::forget(release); }
        if let Some(release) = self.resources.take_pending_release_any() { std::mem::forget(release); }
        if let Some(load) = self.resources.take_abandoned_load() { std::mem::forget(load); }
    }
}

impl FeedControl for FeedBackend {
    fn command(&mut self, command: FeedCommand) {
        match command {
            FeedCommand::Seek { epoch, target_ns, display_ns } => {
                feed_pump::seek_requested(epoch, target_ns, display_ns);
            }
            FeedCommand::AbandonSeek => {
                if let Some(engine) = self.resources.engine() {
                    engine.segment_landing = None;
                }
                feed_pump::seek_abandoned();
            }
            // The take addresses the newest request it belongs to; the feed clears its copy of the
            // pending target whatever that is (a reload zeroes the feed's own epoch, and a copy
            // left armed would hold the AU gate shut).
            FeedCommand::SeekTaken { epoch: _ } => feed_pump::seek_taken(),
            FeedCommand::SeekRequeued { target_ns } => feed_pump::seek_requeued(target_ns),
            FeedCommand::ArmOpen { target_ns, disp_base, playpos_ns } => {
                feed_pump::arm_open(target_ns, disp_base, playpos_ns);
            }
            FeedCommand::CommitPaused { paused } => FEED_TX.commit_paused(paused),
            FeedCommand::BeginPausedSeek => { FEED_TX.begin_paused_seek(); }
            FeedCommand::OwnPausedSeek { target_ns } => { FEED_TX.own_paused_seek(target_ns); }
            FeedCommand::FinishSeekPreroll => FEED_TX.finish_seek_preroll(),
            FeedCommand::NoteFailure(flag) => feed_pump::note_failure(flag),
            FeedCommand::Rebase { playpos_ns, disp_base } => feed_pump::rebase(playpos_ns, disp_base),
            FeedCommand::SelectAudio { idx } => feed_pump::select_audio(idx),
            FeedCommand::BeginPlayback => crate::seek::set_native_seek_refused(false),
            FeedCommand::DvStandDown => feed_pump::dv_stand_down(&mut self.resources),
            FeedCommand::SyncSideReader(want) => crate::side_reader::sync(
                want.map(|spec| (*spec).clone()), self.resources.hooks().prepare.side_reader_job),
        }
    }

    fn user_pause(&mut self, mt: &MainThread) -> UserPauseOutcome {
        feed_pump::user_pause(&mut self.resources, mt)
    }

    fn user_resume(&mut self, mt: &MainThread) -> UserResumeOutcome {
        feed_pump::user_resume(&mut self.resources, mt)
    }

    fn take_transport_action(&mut self) -> Option<FeedTransportAction> {
        self.driver.take_transport_action()
    }

    fn note_transport_result(&mut self, mt: &MainThread, action: FeedTransportAction,
        accepted: bool, native_acknowledged: bool) {
        self.driver.note_transport_result(&mut self.resources, mt, action, accepted,
            native_acknowledged);
    }

    fn take_preroll_volume(&mut self) -> Option<i32> { FEED_TX.take_preroll_volume() }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::native_executor::{
        FeedHooks, LaunchHooks, NativeHooks, PrepareHooks, ReleaseHooks,
    };
    use crate::pcm_lead::PcmLead;
    use crate::send_ptr::SendPtr;
    use crate::shared::HlsUserPause;
    use crate::shared::{FEED_SHARED, FEED_TX};
    use plx_platform::tv::sink::{NoSink, VideoSink};
    use std::os::raw::c_char;

    fn sink() -> &'static dyn VideoSink { &NoSink }

    /// The mailbox half of a sample, which needs no installed session: these tests grade what a
    /// command stores, not the session gate of [`FeedLoad::sample`].
    fn mailboxes() -> FeedFacts { feed_pump::native_feed_facts() }

    /// Hooks the tests never call: with no session installed no verb reaches a native seam.
    const HOOKS: NativeHooks = NativeHooks {
        prepare: PrepareHooks {
            sink, on_exported_window_create: || {}, load_refusal_latch: 0,
            side_reader_job: &NoSideJob,
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

    pub(crate) fn backend() -> FeedBackend {
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
        assert_eq!(wrapped, CtorGate::Awaiting { timeout: None });
        let control: &mut dyn FeedControl = &mut backend;
        assert_eq!(control.ctor_gate(&mt), wrapped);
    }

    /// With no session the sample is `None`, the Load half is the idle value and nothing defers
    /// (the sample's own gate is the session slot, which nothing here installs).
    #[test]
    fn a_feed_with_no_session_has_no_sample_and_nothing_to_defer() {
        use plx_playback_contract::feed::LoadRaw;
        let mut backend = backend();
        assert_eq!(backend.resources().load_raw(), LoadRaw::IDLE);
        let control: &mut dyn FeedControl = &mut backend;
        assert!(control.sample().is_none());
        assert!(!control.defer_preview_join());
        assert!(!control.pending_seek_ready(false));
        assert!(!control.begin_seek_discontinuity());
    }

    /// **The reset is one barrier around the Feed's mirrors and the player's own stores.** The
    /// mirrors are clear when `common` runs, and `common` runs once; a reload keeps the duration
    /// the demuxer published, a stop does not.
    #[test]
    fn reset_runs_the_common_stores_after_the_mirrors_and_a_reload_keeps_the_duration() {
        use std::sync::atomic::Ordering::Relaxed;
        let _serial = plx_base::testlock::serial();
        let mut backend = backend();
        let control: &mut dyn FeedControl = &mut backend;
        for (scope, duration_after) in [(ResetScope::Reload, 90_000_000_000), (ResetScope::Stop, 0)] {
            FEED_SHARED.duration_ns.store(90_000_000_000, Relaxed);
            FEED_SHARED.seeking.store(true, Relaxed);
            let mut ran = 0;
            control.reset(scope, &mut || {
                ran += 1;
                assert!(!FEED_SHARED.seeking.load(Relaxed), "the mirrors are clear when common runs");
            });
            assert_eq!(ran, 1, "{scope:?}: common runs once");
            assert_eq!(FEED_SHARED.duration_ns.load(Relaxed), duration_after, "{scope:?}");
        }
        FEED_SHARED.reset_session();
    }

    /// The transport's half of a teardown is the feed's own `reset`, by scope: a real stop clears the
    /// viewer's Pause, a reload keeps it for the new engine (the player no longer reaches for the
    /// transport itself).
    #[test]
    fn reset_clears_the_transports_pause_for_a_stop_and_keeps_it_for_a_reload() {
        use std::sync::atomic::Ordering::Acquire;
        let _serial = plx_base::testlock::serial();
        let mut backend = backend();
        let control: &mut dyn FeedControl = &mut backend;
        FEED_TX.commit_paused(true);
        control.reset(ResetScope::Reload, &mut || {});
        assert!(FEED_TX.paused.load(Acquire), "a reload keeps the viewer's Pause");
        control.reset(ResetScope::Stop, &mut || {});
        assert!(!FEED_TX.paused.load(Acquire), "a real stop clears it");
        FEED_SHARED.reset_session();
    }

    /// A fresh play gets to try the native `Seek` again (`BeginPlayback`), and the session's seek
    /// mode reaches the player in the sample instead of a call.
    #[test]
    fn a_new_playback_clears_the_native_seek_latch_and_the_sample_carries_the_seek_mode() {
        use plx_playback_contract::feed::SeekMode;
        let _serial = plx_base::testlock::serial();
        let mut backend = backend();
        crate::seek::set_native_seek_refused(true);
        backend.command(FeedCommand::BeginPlayback);
        assert!(!crate::seek::native_seek_refused());
        crate::seek::set_seek_mode(SeekMode::NativeSeek);
        assert_eq!(mailboxes().raw.seek_mode, SeekMode::NativeSeek);
        crate::seek::set_seek_mode(SeekMode::Segment);
        assert_eq!(mailboxes().raw.seek_mode, SeekMode::Segment);
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
        let raw = mailboxes().raw;
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
        assert_eq!(mailboxes().raw.seek_landed_epoch, 3, "the landing reports the newest request");
        FEED_SHARED.reset_session();
        FEED_TX.reset();
        assert_eq!(mailboxes().raw.seek_landed_epoch, 0, "a new session has landed nothing");
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

    /// A transcode restart's `Rebase` stores the display base and the HUD position, arms no seek, and
    /// both come back in the NEXT sample (`FeedRaw::disp_base` / `playpos_ns`), which is how the
    /// player reads them now. The feed's own reset zeroes the base again, and the sample says so.
    #[test]
    fn rebase_sets_the_display_base_and_the_position_and_the_next_sample_reads_them() {
        use std::sync::atomic::Ordering::Relaxed;
        let _serial = plx_base::testlock::serial();
        FEED_SHARED.reset_session();
        FEED_TX.reset();
        let mut backend = backend();
        let control: &mut dyn FeedControl = &mut backend;
        assert_eq!(mailboxes().raw.disp_base, 0);

        control.command(FeedCommand::Rebase { playpos_ns: 600_000_000_000, disp_base: 600_000_000_000 });
        assert_eq!(FEED_SHARED.disp_base.load(Relaxed), 600_000_000_000);
        assert_eq!(FEED_SHARED.playpos_ns.load(Relaxed), 600_000_000_000);
        assert_eq!(FEED_SHARED.seek_to_ns.load(Relaxed), -1, "a rebase arms no demuxer seek");
        let raw = mailboxes().raw;
        assert_eq!((raw.disp_base, raw.playpos_ns), (600_000_000_000, 600_000_000_000));

        FEED_SHARED.reset_session();
        assert_eq!(mailboxes().raw.disp_base, 0, "the session reset is the feed's own");
        FEED_TX.reset();
    }

    /// `SelectAudio` writes the copy the demuxer reads as it opens the container, and the feed's own
    /// session reset leaves it alone: the player's choice survives a reload and is sent again.
    #[test]
    fn select_audio_sets_the_stream_the_demuxer_feeds_and_a_session_reset_keeps_it() {
        use std::sync::atomic::Ordering::Relaxed;
        let _serial = plx_base::testlock::serial();
        FEED_SHARED.reset_session();
        let mut backend = backend();
        let control: &mut dyn FeedControl = &mut backend;
        control.command(FeedCommand::SelectAudio { idx: 3 });
        assert_eq!(FEED_SHARED.desired_audio_idx.load(Relaxed), 3);
        FEED_SHARED.reset_session();
        assert_eq!(FEED_SHARED.desired_audio_idx.load(Relaxed), 3, "a reload keeps the choice");
        control.command(FeedCommand::SelectAudio { idx: -1 });
        assert_eq!(FEED_SHARED.desired_audio_idx.load(Relaxed), -1);
    }

    /// The transport commands make the stores the player's own calls made: `CommitPaused` publishes
    /// the feed gate and the exact pause clock together, a preroll opens only under a Pause
    /// (`BeginPausedSeek`), the pump's seek under a Pause takes ownership of it and a later request
    /// retargets it (`OwnPausedSeek`, never below 0), and `FinishSeekPreroll` closes the preroll
    /// and the ownership with it.
    #[test]
    fn the_transport_commands_leave_the_mailboxes_as_the_players_own_stores_did() {
        use std::sync::atomic::Ordering::Acquire;
        let _serial = plx_base::testlock::serial();
        FEED_SHARED.reset_session();
        FEED_TX.reset();
        let mut backend = backend();
        let control: &mut dyn FeedControl = &mut backend;

        control.command(FeedCommand::BeginPausedSeek);
        assert!(!FEED_TX.seek_preroll_active(), "a feed that is not paused opens no preroll");
        control.command(FeedCommand::OwnPausedSeek { target_ns: 5 });
        assert!(FEED_TX.owned_paused_seek().is_none(), "nothing to hold: nothing owned");

        control.command(FeedCommand::CommitPaused { paused: true });
        assert!(FEED_TX.paused.load(Acquire));
        assert!(FEED_TX.pause_state_sample().paused, "the pause clock moved with the gate");
        assert!(!FEED_TX.feed_allowed(), "a paused feed feeds nothing");

        control.command(FeedCommand::OwnPausedSeek { target_ns: -7 });
        assert!(FEED_TX.seek_preroll_active(), "the pump's seek under a Pause opens the preroll");
        assert_eq!(FEED_TX.owned_paused_seek(), Some(0), "an owned target is never negative");
        assert!(FEED_TX.feed_allowed(), "the preroll lets the keyframe through");
        control.command(FeedCommand::OwnPausedSeek { target_ns: 9_000_000_000 });
        assert_eq!(FEED_TX.owned_paused_seek(), Some(9_000_000_000), "a coalesced tap retargets it");

        control.command(FeedCommand::FinishSeekPreroll);
        assert!(!FEED_TX.seek_preroll_active() && FEED_TX.owned_paused_seek().is_none());
        assert!(!FEED_TX.feed_allowed(), "the gate is shut again behind the closed preroll");

        control.command(FeedCommand::BeginPausedSeek);
        assert!(FEED_TX.seek_preroll_active(), "a caller's own preroll opens under a Pause");
        assert!(FEED_TX.owned_paused_seek().is_none(), "and stays the caller's");

        control.command(FeedCommand::CommitPaused { paused: false });
        assert!(!FEED_TX.paused.load(Acquire));
        assert!(!FEED_TX.pause_state_sample().paused);
        FEED_SHARED.reset_session();
        FEED_TX.reset();
    }

    /// The viewer's Pause and Resume, run by the feed. With the sink refusing (the stand-in sink
    /// here), a native Pause or Play changes nothing: no transport commit, no preroll closed, and
    /// the clock transition is completed as refused so the next press can try again. A Pause the
    /// clock already holds commits without issuing anything native, and a Resume with no hold to
    /// release says so.
    #[test]
    fn a_refused_native_transition_commits_nothing_and_a_held_clock_pauses_without_one() {
        use std::sync::atomic::Ordering::Acquire;
        use plx_playback_contract::feed::{HlsPauseCompletion, UserPauseOutcome, UserResumeOutcome};
        let _serial = plx_base::testlock::serial();
        // SAFETY: a host test; nothing here touches the thread-affine state the token guards.
        let mt = unsafe { MainThread::assume() };
        FEED_SHARED.reset_session();
        FEED_TX.reset();
        let mut backend = backend();
        let control: &mut dyn FeedControl = &mut backend;

        assert_eq!(control.user_resume(&mt), UserResumeOutcome::NoHold, "nothing is paused");
        assert_eq!(control.user_pause(&mt), UserPauseOutcome::Issued(HlsPauseCompletion::Refused));
        assert!(!FEED_TX.paused.load(Acquire), "a refused Pause never enters the transport");
        assert_eq!(control.user_pause(&mt), UserPauseOutcome::Issued(HlsPauseCompletion::Refused),
            "and leaves the clock free for the next press");

        // The viewer's Pause is accepted elsewhere (a sink that took it); the next one finds it held.
        let Some(HlsUserPause::Issue(token)) = FEED_SHARED.prepare_hls_user_pause() else {
            panic!("a running clock issues the user Pause");
        };
        assert_eq!(FEED_SHARED.complete_hls_user_pause(token, true), HlsPauseCompletion::Accepted);
        assert_eq!(control.user_pause(&mt), UserPauseOutcome::AlreadyHeld);
        assert!(FEED_TX.paused.load(Acquire), "the held clock commits the transport Paused");

        assert_eq!(control.user_resume(&mt), UserResumeOutcome::PlayRefused, "the sink refuses Play");
        assert!(FEED_TX.paused.load(Acquire), "a refused Play leaves the viewer paused");
        FEED_SHARED.reset_session();
        FEED_TX.reset();
    }

    /// The mute a paused preroll saved is the feed's to hand back, once.
    #[test]
    fn the_saved_preroll_volume_is_handed_back_once() {
        let _serial = plx_base::testlock::serial();
        FEED_TX.reset();
        let mut backend = backend();
        let control: &mut dyn FeedControl = &mut backend;
        assert_eq!(control.take_preroll_volume(), None, "nothing is muted");
        FEED_TX.note_preroll_muted(30);
        assert_eq!(control.take_preroll_volume(), Some(30));
        assert_eq!(control.take_preroll_volume(), None, "the mute is cleared by the take");
    }

    /// The feed's request to seek again reaches the player as a numbered raw fact, together with
    /// the in-place target it aimed at, and the session's reset forgets both.
    #[test]
    fn the_sample_carries_the_requeue_and_the_in_place_target() {
        use std::sync::atomic::Ordering::{Release, Relaxed};
        let _serial = plx_base::testlock::serial();
        FEED_SHARED.reset_session();
        FEED_TX.reset();
        let idle = mailboxes().raw;
        assert_eq!(idle.seek_requeue, plx_playback_contract::feed::SeekRequeue::NONE);
        assert_eq!(idle.seek_target_ns, None);

        FEED_SHARED.seek_target_ns.store(61_000_000_000, Relaxed);
        FEED_SHARED.seek_requeue_target.store(61_000_000_000, Relaxed);
        FEED_SHARED.seek_requeue_epoch.fetch_add(1, Release);
        let raw = mailboxes().raw;
        assert_eq!(raw.seek_target_ns, Some(61_000_000_000));
        assert_eq!(raw.seek_requeue.epoch, 1);
        assert_eq!(raw.seek_requeue.target_ns, 61_000_000_000);

        FEED_SHARED.reset_session();
        FEED_TX.reset();
        let raw = mailboxes().raw;
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
        assert_eq!(mailboxes().raw.flags, FailureFlags::default());

        control.command(FeedCommand::NoteFailure(FailureFlag::LoadFailed));
        control.command(FeedCommand::NoteFailure(FailureFlag::LoadTimedOut));
        let raw = mailboxes().raw;
        assert!(raw.flags.load_failed && raw.flags.load_timed_out, "the notes come back as flags");
        assert!(!raw.flags.demux_io_failed && !raw.flags.demux_failed, "and only those");
        assert_eq!(mailboxes().attempt.runtime_failure,
            Some(plx_playback_contract::readout::RuntimeFailure::LoadTimeout),
            "the mapped half collapses the same flags");

        FEED_SHARED.dg_cb_err_num.store(601, Relaxed);
        FEED_SHARED.dg_http_status.store(404, Relaxed);
        control.command(FeedCommand::NoteFailure(FailureFlag::DemuxIoFailed));
        let raw = mailboxes().raw;
        assert!(raw.flags.demux_io_failed);
        assert_eq!((raw.cb_err_num, raw.http_status), (601, 404), "the codes ride the same sample");

        FEED_SHARED.reset_session();
        assert_eq!(mailboxes().raw.flags, FailureFlags::default(), "a reset retires the notes");
        assert_eq!(mailboxes().raw.cb_err_num, 0);
    }

    /// The pipeline's decoder revocations and native-audio refusals are COUNTS the feed keeps for
    /// the session: each callback adds one, the sample carries the count, the session's reset zeroes it.
    #[test]
    fn the_sample_counts_the_one_shot_edges_and_a_reset_zeroes_them() {
        let _serial = plx_base::testlock::serial();
        FEED_SHARED.reset_session();
        FEED_TX.reset();
        assert_eq!((mailboxes().raw.decoder_revocations, mailboxes().raw.native_audio_refusals), (0, 0));
        FEED_SHARED.raise_decoder_revoked();
        FEED_SHARED.raise_decoder_revoked();
        FEED_SHARED.raise_native_audio_refused();
        let raw = mailboxes().raw;
        assert_eq!((raw.decoder_revocations, raw.native_audio_refusals), (2, 1));
        FEED_SHARED.reset_session();
        let raw = mailboxes().raw;
        assert_eq!((raw.decoder_revocations, raw.native_audio_refusals), (0, 0));
    }

    /// The read-out's feed-written values ride the sample, each from the mailbox of its own name
    /// (every one given a different value, so a swapped pair fails), and the session's reset returns
    /// the whole set to `FeedDiag::IDLE`: the value the read-out showed from the mailboxes after a
    /// stop or a reload, `place_rv` included (`i32::MIN`, not 0).
    #[test]
    fn the_sample_carries_the_readouts_mailboxes_and_a_reset_returns_them_to_idle() {
        use plx_playback_contract::feed::FeedDiag;
        use std::sync::atomic::Ordering::Relaxed;
        let _serial = plx_base::testlock::serial();
        FEED_SHARED.reset_session();
        crate::native_executor::reset_fed_counters();
        crate::ff::store_pushed_any(false);
        assert_eq!(mailboxes().raw.diag, FeedDiag::IDLE, "a fresh session reads the idle set");

        FEED_SHARED.dg_place_rv.store(-3, Relaxed);
        FEED_SHARED.dg_placed_w.store(3840, Relaxed);
        FEED_SHARED.dg_placed_h.store(2160, Relaxed);
        FEED_SHARED.load_completed.store(true, Relaxed);
        FEED_SHARED.dg_cb_count.store(41, Relaxed);
        FEED_SHARED.dg_aq_video.store(9_000_001, Relaxed);
        FEED_SHARED.dg_aq_audio.store(9_000_002, Relaxed);
        FEED_SHARED.dg_fed_v_pts.store(10_400_000_000, Relaxed);
        FEED_SHARED.dg_fed_a_pts.store(10_300_000_000, Relaxed);
        FEED_SHARED.dg_load_v.store(2, Relaxed);
        FEED_SHARED.dg_load_a.store(5, Relaxed);
        FEED_SHARED.dg_feed_state.store(4, Relaxed);
        FEED_SHARED.dg_cb_err.store(18, Relaxed);
        FEED_SHARED.dg_cb_err_at.store(7, Relaxed);
        FEED_SHARED.dg_net_rx.store(13_000_000, Relaxed);
        FEED_SHARED.dg_load_at.store(4_200, Relaxed);
        FEED_SHARED.dg_frame_at.store(5_300, Relaxed);
        FEED_SHARED.video_fps_milli.store(23_976, Relaxed);
        crate::native_executor::store_fed_totals(5_000, 4_800);
        crate::ff::store_pushed_any(true);
        assert_eq!(mailboxes().raw.diag, FeedDiag {
            place_rv: -3, placed_w: 3840, placed_h: 2160, load_completed: true, cb_count: 41,
            aq_video: 9_000_001, aq_audio: 9_000_002, fed_v_pts: 10_400_000_000,
            fed_a_pts: 10_300_000_000, load_v: 2, load_a: 5, feed_state: 4, cb_err: 18, cb_err_at: 7,
            net_rx: 13_000_000, load_at: 4_200, frame_at: 5_300, video_fps_milli: 23_976,
            fed_v: 5_000, fed_a: 4_800, pushed_any: true,
        });

        FEED_SHARED.reset_session();
        crate::native_executor::reset_fed_counters();
        crate::ff::store_pushed_any(false);
        assert_eq!(mailboxes().raw.diag, FeedDiag::IDLE, "a stop or a reload returns the whole set");
    }

    /// The audio the demuxer feeds natively rides the sample (the player reads it from there, not
    /// from the demuxer's cell), and a new session forgets it.
    #[test]
    fn the_sample_carries_the_audio_the_demuxer_feeds_natively() {
        use plx_playback_contract::feed::NativeAudio;
        let _serial = plx_base::testlock::serial();
        crate::native_audio::reset();
        assert_eq!(mailboxes().raw.native_audio, None, "nothing fed natively yet");
        crate::native_audio::publish_fed(crate::native_audio::Kind::Opus, 6);
        assert_eq!(mailboxes().raw.native_audio, Some(NativeAudio { codec: "opus", channels: 6 }));
        crate::native_audio::reset();
        assert_eq!(mailboxes().raw.native_audio, None, "a new session forgets it");
    }

    /// The side reader's caption runner for tests that never start a reader.
    struct NoSideJob;

    impl plx_playback_contract::subtitle::SideReaderJobPort for NoSideJob {
        fn read_once(
            &self, _: &plx_playback_contract::subtitle::ReadCfg, _: &std::sync::Arc<crate::side_reader::SideStop>,
            _: &plx_playback_contract::subtitle::Anchor, _: &mut Option<i64>, _: &dyn Fn(i64) -> i64,
            _: &dyn Fn() -> i64, _: &dyn Fn() -> (i64, i64),
            _: &dyn Fn() -> plx_playback_contract::subtitle::CaptionRetention, _: &dyn Fn(i64),
        ) -> crate::side_reader::RunEnd { crate::side_reader::RunEnd::Stopped }
        fn desired_sub_idx(&self) -> i32 { -1 }
        fn request_subtitle(&self, _: i32) {}
        fn clear_bitmaps(&self) {}
    }

    /// A local-sample open: it needs no stream, no demux session and no device.
    fn sample_request() -> OpenRequest {
        OpenRequest {
            start_paused: false, seek_preroll: false, stream: None,
            sample: Some(Box::new(crate::feed_state::SampleBuf::new(Vec::new(), Vec::new()))),
            is_h265: false, session_rate: crate::shared::RATE_NORMAL, requested_rate: 1000,
            no_audio: true, side_reader: Default::default(),
            dv_landing: plx_playback_contract::feed::DvLandingPlan::INERT,
        }
    }

    /// A start lost between `open_prepare` and `open_stage` (the route reducer refused the attempt
    /// claim) aborts its parked open: the prepared open owns the clock and native-epoch guards, so
    /// without the abort the next start would be refused as an overlapping one, and with it the
    /// next start prepares.
    #[test]
    fn a_start_lost_after_open_prepare_aborts_and_the_next_open_prepares() {
        let _serial = plx_base::testlock::serial();
        // SAFETY: a host test; nothing here touches the thread-affine state the token guards.
        let mt = unsafe { MainThread::assume() };
        let mut backend = backend();
        assert!(backend.open_begin(&mt));
        backend.open_prepare(&mt, sample_request()).expect("the first open prepares");
        backend.open_abort();
        assert!(backend.open_begin(&mt));
        let epoch = backend.open_prepare(&mt, sample_request())
            .expect("the guards of the lost start were released, so the next start prepares");
        assert!(epoch > 0);
        backend.open_abort();
    }

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
        assert_eq!(backend.take_transport_action(), None);
    }
}
