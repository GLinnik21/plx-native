//! Test double for the buffer-feed seam: [`FakeFeed`], a scriptable [`FeedControl`]. Compiled only
//! for this crate's own tests and for dependents that name the `test-support` feature from their
//! `[dev-dependencies]`.
//!
//! A test keeps the paired [`FakeFeedHandle`]: it scripts what each verb answers and reads the log
//! of the verbs the player forwarded. No handle method writes player readout state: a test that
//! needs the readout to change delivers an event through the real ingestion, never a setter
//! (D2 v2, 3.6).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI32, AtomicU32};
use std::sync::{Arc, Mutex};

use plx_base::task::MainThread;

use crate::demux::DemuxSession;
use crate::readout::AttemptFacts;
use crate::feed::{
    defer_media_join, AcceptedLoadOutcome, CtorGate, DvWithdrawal, FeedCommand, FeedControl, FeedFacts, FeedLoad, FeedOpen,
    FeedSeek, FeedStop, FeedTransportAction, OpenRequest, ReleaseHandoff, ResetScope, UnblockFacts,
    LoadTick, PausedSeekStep,
    PresentationTick, SeekMode, SeekPause, Stage, StuckSeekAction, UserPauseOutcome,
    UserResumeOutcome,
};
use crate::{AttemptId, Failure, SourceKey};

/// One verb the player forwarded to the fake feed, with the arguments it was given. The atomic
/// out-parameters of `inject_refuse601` and `publish_diag` are not logged: the fake does not
/// touch them, and a test that cares reads the counters it passed.
#[derive(Clone, Debug, PartialEq)]
pub enum FeedCall {
    PausedSeekStep { now: u32, route_flight: bool, claim_owns_pause: bool },
    StuckSeekDue { now: u32, feed_allowed: bool, route_flight: bool, budget_ms: u32 },
    RetryStuckSeek { now: u32, target_ns: i64, can_retry: bool },
    PendingSeekReady { seek_reloads: bool },
    PrepareSeekPause,
    BeginSeekDiscontinuity,
    ApplyInplaceSeekPre { target_ns: i64, mode: SeekMode, coalesced: u32 },
    ApplyInplaceSeekPost { target_ns: i64, now: u32 },
    Command(FeedCommand),
    TickLoad { now: u32, segmented_hls: bool, speed_deadline_ms: u32,
        no_audio: bool },
    TickPresentation { segmented_hls: bool, immersive: bool },
    CtorGate,
    TakeLoadOutcomes,
    NativeAudioStallFired { now: u32 },
    DvLandingDue { now: u32, route_flight: bool },
    NoteMissingEraCallback,
    InjectRefuse601 { generation: u32, preview: bool },
    PublishDiag { now: u32, diag_readout_on: bool },
    DeferPreviewJoin,
    Sample,
    UserPause,
    UserResume,
    TakePrerollVolume,
    TakeTransportAction,
    NoteTransportResult { action: FeedTransportAction, accepted: bool, native_acknowledged: bool },
    HasAbandonedLoad,
    ReapAbandonedLoad,
    OpenBegin,
    /// The request itself is not logged (it owns ports); its pause goal is the one scalar a test reads.
    OpenPrepare { start_paused: bool },
    OpenStage { attempt: AttemptId, key: SourceKey },
    OpenAbort,
    StopBegin { terminal: bool },
    StopSideReader { for_reload: bool },
    StopUnblock,
    StopReportDispatched,
    StopRelease { for_reload: bool },
    StopFinish { for_reload: bool },
    Reset { scope: ResetScope },
    TakeReturnedRelease,
    RunReleaseTail { epoch: u32 },
    LeakForExit,
}

/// What the fake feed answers. The default is an idle feed: nothing live, nothing loading, no
/// pending verdicts, and the answers a callee gives with no session behind it.
#[derive(Clone, Debug)]
pub struct FeedScript {
    pub paused_seek_step: PausedSeekStep,
    pub stuck_seek_due: bool,
    pub retry_stuck_seek: StuckSeekAction,
    pub pending_seek_ready: bool,
    pub seek_pause: SeekPause,
    pub begin_seek_discontinuity: bool,
    pub load_tick: LoadTick,
    pub presentation: PresentationTick,
    /// What `sample` answers while a session is installed (`facts.raw.load.stage` is `Some`): the
    /// scripted observation a test delivers through the real ingestion. `defer_preview_join` reads
    /// `facts.raw.load` too.
    pub facts: FeedFacts,
    pub ctor_gate: CtorGate,
    /// Delivered once, in order, by the next `take_load_outcomes`.
    pub load_outcomes: Vec<AcceptedLoadOutcome>,
    pub native_audio_stall_fired: bool,
    /// Delivered once by the next `dv_landing_due`.
    pub dv_landing: Option<DvWithdrawal>,
    pub user_pause: UserPauseOutcome,
    pub user_resume: UserResumeOutcome,
    /// Delivered once by the next `take_preroll_volume`.
    pub preroll_volume: Option<i32>,
    /// What `stop_unblock` reports.
    pub unblock: UnblockFacts,
    /// Delivered once by the next `stop_finish`.
    pub handoff: Option<ReleaseHandoff>,
    /// Delivered in order by `take_transport_action`.
    pub transport_actions: VecDeque<FeedTransportAction>,
    /// What `has_abandoned_load` answers.
    pub abandoned_load: bool,
    /// What `reap_abandoned_load` answers.
    pub reap_abandoned_load: bool,
    /// What `open_begin` answers.
    pub open_begin: bool,
    /// What `open_prepare` answers.
    pub open_prepare: Result<u32, Failure>,
    /// What `open_stage` answers.
    pub open_stage: Result<(), Failure>,
    /// Delivered once by the next `take_returned_release`.
    pub returned_release: Option<u32>,
    /// Whether `run_release_tail` finds a returned release to run.
    pub release_tail_runs: bool,
}

impl Default for FeedScript {
    fn default() -> Self {
        Self {
            paused_seek_step: PausedSeekStep::default(),
            stuck_seek_due: false,
            retry_stuck_seek: StuckSeekAction::Reload,
            pending_seek_ready: false,
            seek_pause: SeekPause::Ready,
            begin_seek_discontinuity: false,
            load_tick: LoadTick::default(),
            presentation: PresentationTick { stage: Stage::Loading, eos_pushed: false },
            facts: FeedFacts::default(),
            ctor_gate: CtorGate::Ready,
            load_outcomes: Vec::new(),
            native_audio_stall_fired: false,
            dv_landing: None,
            user_pause: UserPauseOutcome::Deferred,
            user_resume: UserResumeOutcome::NoHold,
            preroll_volume: None,
            unblock: UnblockFacts { attempt: AttemptFacts::default(), load_abandoned: false },
            handoff: None,
            transport_actions: VecDeque::new(),
            abandoned_load: false,
            reap_abandoned_load: false,
            open_begin: true,
            open_prepare: Ok(1),
            open_stage: Ok(()),
            returned_release: None,
            release_tail_runs: false,
        }
    }
}

#[derive(Default)]
struct Shared {
    script: FeedScript,
    calls: Vec<FeedCall>,
    /// Whether `defer_preview_join` marked the preview abandoned.
    preview_abandoned: bool,
}

/// The test's side of a [`FakeFeed`]: what it was asked, and what it should say next.
#[derive(Clone)]
pub struct FakeFeedHandle(Arc<Mutex<Shared>>);

impl FakeFeedHandle {
    /// Change what the feed answers from now on.
    pub fn script(&self, edit: impl FnOnce(&mut FeedScript)) {
        edit(&mut self.0.lock().unwrap().script);
    }
    /// Every verb the player forwarded to the feed, in order (queries included).
    pub fn calls(&self) -> Vec<FeedCall> { self.0.lock().unwrap().calls.clone() }
    /// Forget the log, so a test asserts only what happens after a point.
    pub fn clear_calls(&self) { self.0.lock().unwrap().calls.clear(); }
    /// Whether `defer_preview_join` deferred and marked the preview abandoned.
    pub fn preview_abandoned(&self) -> bool { self.0.lock().unwrap().preview_abandoned }
}

/// A scriptable [`FeedControl`]. Every verb is logged, and a verb answers what the script says.
pub struct FakeFeed {
    shared: Arc<Mutex<Shared>>,
}

impl FakeFeed {
    /// An idle feed, and the handle that scripts and inspects it.
    pub fn new() -> (Self, FakeFeedHandle) {
        let shared = Arc::new(Mutex::new(Shared::default()));
        (Self { shared: Arc::clone(&shared) }, FakeFeedHandle(shared))
    }

    /// Log `call` and answer `read(script)` under one lock.
    fn answer<T>(&self, call: FeedCall, read: impl FnOnce(&mut FeedScript) -> T) -> T {
        let mut shared = self.shared.lock().unwrap();
        shared.calls.push(call);
        read(&mut shared.script)
    }
}

impl FeedSeek for FakeFeed {
    fn paused_seek_step(&mut self, now: u32, route_flight: bool, claim_owns_pause: bool)
        -> PausedSeekStep {
        self.answer(FeedCall::PausedSeekStep { now, route_flight, claim_owns_pause },
            |s| s.paused_seek_step)
    }
    fn stuck_seek_due(&mut self, now: u32, feed_allowed: bool, route_flight: bool, budget_ms: u32)
        -> bool {
        self.answer(FeedCall::StuckSeekDue { now, feed_allowed, route_flight, budget_ms },
            |s| s.stuck_seek_due)
    }
    fn retry_stuck_seek(&mut self, now: u32, target_ns: i64, can_retry: bool) -> StuckSeekAction {
        self.answer(FeedCall::RetryStuckSeek { now, target_ns, can_retry }, |s| s.retry_stuck_seek)
    }
    fn pending_seek_ready(&self, seek_reloads: bool) -> bool {
        self.answer(FeedCall::PendingSeekReady { seek_reloads }, |s| s.pending_seek_ready)
    }
    fn prepare_seek_pause(&mut self, _mt: &MainThread) -> SeekPause {
        self.answer(FeedCall::PrepareSeekPause, |s| s.seek_pause)
    }
    fn begin_seek_discontinuity(&mut self) -> bool {
        self.answer(FeedCall::BeginSeekDiscontinuity, |s| s.begin_seek_discontinuity)
    }
    fn apply_inplace_seek_pre(&mut self, _mt: &MainThread, target_ns: i64, mode: SeekMode,
        coalesced: u32) {
        self.answer(FeedCall::ApplyInplaceSeekPre { target_ns, mode, coalesced }, |_| ());
    }
    fn apply_inplace_seek_post(&mut self, target_ns: i64, now: u32) {
        self.answer(FeedCall::ApplyInplaceSeekPost { target_ns, now }, |_| ());
    }
}

impl FeedLoad for FakeFeed {
    fn tick_load(&mut self, _mt: &MainThread, now: u32, segmented_hls: bool,
        speed_deadline_ms: u32, no_audio: bool) -> LoadTick {
        self.answer(FeedCall::TickLoad { now, segmented_hls, speed_deadline_ms, no_audio },
            |s| s.load_tick)
    }
    fn tick_presentation(&mut self, _mt: &MainThread, segmented_hls: bool, immersive: bool)
        -> PresentationTick {
        self.answer(FeedCall::TickPresentation { segmented_hls, immersive }, |s| s.presentation)
    }
    fn ctor_gate(&mut self, _mt: &MainThread) -> CtorGate {
        self.answer(FeedCall::CtorGate, |s| s.ctor_gate)
    }
    fn take_load_outcomes(&mut self) -> Vec<AcceptedLoadOutcome> {
        self.answer(FeedCall::TakeLoadOutcomes, |s| std::mem::take(&mut s.load_outcomes))
    }
    fn native_audio_stall_fired(&mut self, now: u32) -> bool {
        self.answer(FeedCall::NativeAudioStallFired { now }, |s| s.native_audio_stall_fired)
    }
    fn dv_landing_due(&mut self, now: u32, route_flight: bool) -> Option<DvWithdrawal> {
        self.answer(FeedCall::DvLandingDue { now, route_flight }, |s| s.dv_landing.take())
    }
    fn note_missing_era_callback(&mut self) {
        self.answer(FeedCall::NoteMissingEraCallback, |_| ());
    }
    fn inject_refuse601(&mut self, generation: u32, preview: bool, _refused_for: &AtomicU32) {
        self.answer(FeedCall::InjectRefuse601 { generation, preview }, |_| ());
    }
    fn publish_diag(&mut self, now: u32, _last_frames: &AtomicI32, diag_readout_on: bool) {
        self.answer(FeedCall::PublishDiag { now, diag_readout_on }, |_| ());
    }
    fn defer_preview_join(&mut self) -> bool {
        let mut shared = self.shared.lock().unwrap();
        shared.calls.push(FeedCall::DeferPreviewJoin);
        let load = shared.script.facts.raw.load;
        let defer = defer_media_join(load.loading(), load.load_thread_finished);
        shared.preview_abandoned |= defer;
        defer
    }

    fn sample(&self) -> Option<FeedFacts> {
        self.answer(FeedCall::Sample, |s| s.facts.raw.load.stage.is_some().then_some(s.facts))
    }
    fn has_abandoned_load(&self) -> bool {
        self.answer(FeedCall::HasAbandonedLoad, |s| s.abandoned_load)
    }
    fn reap_abandoned_load(&mut self, _mt: &MainThread) -> bool {
        self.answer(FeedCall::ReapAbandonedLoad, |s| s.reap_abandoned_load)
    }
}

impl FeedOpen for FakeFeed {
    fn open_begin(&mut self, _mt: &MainThread) -> bool {
        self.answer(FeedCall::OpenBegin, |s| s.open_begin)
    }
    fn open_prepare(&mut self, _mt: &MainThread, request: OpenRequest) -> Result<u32, Failure> {
        self.answer(FeedCall::OpenPrepare { start_paused: request.start_paused }, |s| s.open_prepare)
    }
    fn open_stage(&mut self, attempt: AttemptId, key: SourceKey, _session: Box<dyn DemuxSession>)
        -> Result<(), Failure> {
        self.answer(FeedCall::OpenStage { attempt, key }, |s| s.open_stage)
    }
    fn open_abort(&mut self) {
        self.answer(FeedCall::OpenAbort, |_| ());
    }
}

impl FeedStop for FakeFeed {
    fn stop_begin(&mut self, terminal: bool) -> Option<FeedFacts> {
        self.answer(FeedCall::StopBegin { terminal }, |s| s.facts.raw.load.stage.is_some().then_some(s.facts))
    }
    fn stop_side_reader(&mut self, for_reload: bool) {
        self.answer(FeedCall::StopSideReader { for_reload }, |_| ());
    }
    fn stop_unblock(&mut self) -> UnblockFacts {
        self.answer(FeedCall::StopUnblock, |s| s.unblock)
    }
    fn stop_report_dispatched(&mut self) {
        self.answer(FeedCall::StopReportDispatched, |_| ());
    }
    fn stop_release(&mut self, _mt: &MainThread, for_reload: bool) {
        self.answer(FeedCall::StopRelease { for_reload }, |_| ());
    }
    fn stop_finish(&mut self, for_reload: bool) -> Option<ReleaseHandoff> {
        self.answer(FeedCall::StopFinish { for_reload }, |s| s.handoff.take())
    }
    fn reset(&mut self, scope: ResetScope, common: &mut dyn FnMut()) {
        self.answer(FeedCall::Reset { scope }, |_| ());
        common();
    }
    fn take_returned_release(&mut self) -> Option<u32> {
        self.answer(FeedCall::TakeReturnedRelease, |s| s.returned_release.take())
    }
    fn run_release_tail(&mut self, _mt: &MainThread, epoch: u32) -> bool {
        self.answer(FeedCall::RunReleaseTail { epoch }, |s| s.release_tail_runs)
    }
    fn leak_for_exit(&mut self) {
        self.answer(FeedCall::LeakForExit, |_| ());
    }
}

impl FeedControl for FakeFeed {
    fn command(&mut self, command: FeedCommand) {
        self.answer(FeedCall::Command(command), |_| ());
    }
    fn user_pause(&mut self, _mt: &MainThread) -> UserPauseOutcome {
        self.answer(FeedCall::UserPause, |s| s.user_pause)
    }
    fn user_resume(&mut self, _mt: &MainThread) -> UserResumeOutcome {
        self.answer(FeedCall::UserResume, |s| s.user_resume)
    }
    fn take_preroll_volume(&mut self) -> Option<i32> {
        self.answer(FeedCall::TakePrerollVolume, |s| s.preroll_volume.take())
    }
    fn take_transport_action(&mut self) -> Option<FeedTransportAction> {
        self.answer(FeedCall::TakeTransportAction, |s| s.transport_actions.pop_front())
    }
    fn note_transport_result(&mut self, _mt: &MainThread, action: FeedTransportAction,
        accepted: bool, native_acknowledged: bool) {
        self.answer(FeedCall::NoteTransportResult { action, accepted, native_acknowledged }, |_| ());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mt() -> MainThread {
        // SAFETY: a host test; nothing here touches the thread-affine state the token guards.
        unsafe { MainThread::assume() }
    }

    /// The fake records each verb the pump forwarded, with the arguments it was given.
    #[test]
    fn a_fake_feed_logs_the_forwarded_verbs() {
        let (mut feed, handle) = FakeFeed::new();
        assert_eq!(feed.ctor_gate(&mt()), CtorGate::Ready);
        assert!(!feed.stuck_seek_due(10, true, false, 1200));
        feed.apply_inplace_seek_pre(&mt(), 5_000_000_000, SeekMode::Segment, 2);
        assert_eq!(handle.calls(), [
            FeedCall::CtorGate,
            FeedCall::StuckSeekDue { now: 10, feed_allowed: true, route_flight: false,
                budget_ms: 1200 },
            FeedCall::ApplyInplaceSeekPre { target_ns: 5_000_000_000, mode: SeekMode::Segment,
                coalesced: 2 },
        ]);
    }

    /// A test scripts what the feed answers; the answers replace whatever a native session would
    /// say, and a consumed answer (the Load verdicts) is consumed.
    #[test]
    fn scripted_answers_come_back_and_load_verdicts_are_consumed_once() {
        let (mut feed, handle) = FakeFeed::new();
        let outcome = AcceptedLoadOutcome { attempt: AttemptId::new(1, 3), ok: true,
            elapsed_ms: Some(40) };
        handle.script(|script| {
            script.facts.raw.load.stage = Some(Stage::Streaming);
            script.paused_seek_step = PausedSeekStep { finish: true, mid_seek_claimed: false, preroll_opened: false };
            script.retry_stuck_seek = StuckSeekAction::Retried(1);
            script.seek_pause = SeekPause::Busy;
            script.load_outcomes = vec![outcome];
        });
        assert_eq!(feed.sample().map(|facts| facts.raw.load.stage), Some(Some(Stage::Streaming)));
        assert_eq!(feed.paused_seek_step(1, false, false),
            PausedSeekStep { finish: true, mid_seek_claimed: false, preroll_opened: false });
        assert_eq!(feed.retry_stuck_seek(1, 0, true), StuckSeekAction::Retried(1));
        assert_eq!(feed.prepare_seek_pause(&mt()), SeekPause::Busy);
        assert_eq!(feed.take_load_outcomes(), [outcome]);
        assert!(feed.take_load_outcomes().is_empty(), "a verdict is delivered once");
        assert_eq!(handle.calls().len(), 6, "every query is logged too");
    }

    /// The unscripted fake is an idle feed: no session to sample, no pending verdicts, no join to
    /// defer.
    #[test]
    fn an_unscripted_fake_is_an_idle_feed() {
        let (mut feed, handle) = FakeFeed::new();
        assert!(feed.sample().is_none(), "a feed with no session is not an observation");
        assert!(!feed.defer_preview_join());
        assert!(!handle.preview_abandoned());
        assert!(feed.take_load_outcomes().is_empty());
    }

    /// A preview whose Load has not returned is deferred and marked abandoned; one whose Load has
    /// returned, one past its Load stage, and one with no session are joined as before.
    #[test]
    fn a_preview_join_defers_only_for_a_load_stage_with_an_unfinished_thread() {
        let load = |stage, load_thread_finished| crate::feed::LoadRaw {
            stage, load_thread_finished, route_actions_ready: false };
        for (raw, defers) in [
            (load(Some(Stage::Loading), false), true),
            (load(Some(Stage::Loading), true), false),
            (load(Some(Stage::Playing), false), false),
            (load(Some(Stage::Streaming), true), false),
            (crate::feed::LoadRaw::IDLE, false),
        ] {
            let (mut feed, handle) = FakeFeed::new();
            handle.script(|script| script.facts.raw.load = raw);
            assert_eq!(feed.defer_preview_join(), defers, "{raw:?}");
            assert_eq!(handle.preview_abandoned(), defers, "{raw:?}: the mark follows the answer");
            assert_eq!(handle.calls(), [FeedCall::DeferPreviewJoin]);
        }
    }

    /// The stop is driven phase by phase: the fake logs each phase with its arguments, the handoff
    /// and the returned release are delivered once, and `reset` runs the player's own stores
    /// exactly once, as the real feed does inside its barrier.
    #[test]
    fn the_stop_phases_are_logged_in_the_order_they_are_driven() {
        let (mut feed, handle) = FakeFeed::new();
        let handoff = ReleaseHandoff { epoch: 3, at: std::time::Instant::now() };
        handle.script(|script| {
            script.facts.raw.load.stage = Some(Stage::Streaming);
            script.handoff = Some(handoff);
            script.returned_release = Some(3);
            script.release_tail_runs = true;
            script.unblock.load_abandoned = true;
        });
        let mut common = 0;
        assert!(feed.stop_begin(false).is_some(), "a live session has a last sample");
        feed.stop_side_reader(true);
        assert!(feed.stop_unblock().load_abandoned);
        feed.stop_report_dispatched();
        feed.stop_release(&mt(), true);
        feed.reset(ResetScope::Reload, &mut || common += 1);
        assert_eq!(feed.stop_finish(true), Some(handoff));
        assert_eq!(feed.stop_finish(true), None, "a handoff is delivered once");
        assert_eq!(feed.take_returned_release(), Some(3));
        assert_eq!(feed.take_returned_release(), None, "a returned release is delivered once");
        assert!(feed.run_release_tail(&mt(), 3));
        feed.leak_for_exit();
        assert_eq!(common, 1, "reset runs the player's stores once");
        assert_eq!(handle.calls(), [
            FeedCall::StopBegin { terminal: false }, FeedCall::StopSideReader { for_reload: true }, FeedCall::StopUnblock,
            FeedCall::StopReportDispatched, FeedCall::StopRelease { for_reload: true },
            FeedCall::Reset { scope: ResetScope::Reload }, FeedCall::StopFinish { for_reload: true },
            FeedCall::StopFinish { for_reload: true },
            FeedCall::TakeReturnedRelease, FeedCall::TakeReturnedRelease,
            FeedCall::RunReleaseTail { epoch: 3 }, FeedCall::LeakForExit,
        ]);
    }
}
