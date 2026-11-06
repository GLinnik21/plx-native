//! Test doubles for the buffer-feed seam: [`FakeFeed`], a scriptable [`FeedControl`], and
//! [`FakePhysical`], the [`PhysicalSet`] that lends it. Compiled only for this crate's own tests and
//! for dependents that name the `test-support` feature from their `[dev-dependencies]`.
//!
//! The adapter boxes the set, so a test keeps the paired [`FakeFeedHandle`]: it scripts what each
//! verb answers, reads the log of the verbs the player forwarded, queues the events the runner's
//! next poll delivers, and inspects what the runner dispatched. No handle method writes player
//! readout state: a test that needs the readout to change delivers an event through the real
//! ingestion, never a setter (D2 v2, 3.6).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI32, AtomicU32};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use plx_base::task::MainThread;

use crate::feed::{
    defer_media_join, AcceptedLoadOutcome, CtorGate, FeedCommand, FeedControl, FeedFacts, FeedLoad, FeedSeek,
    LoadTick, PausedSeekStep,
    PhysicalSet, PresentationTick, SeekMode, SeekPause, Stage, StuckSeekAction, UserPauseOutcome,
    UserResumeOutcome,
};
use crate::{
    AttemptId, BackendEvent, BackendKind, Capabilities, CommandId, DispatchResult, DriverSet,
    ExitDisposition, Failure, Operation, SourceKey,
};

/// What the packet-feed backend advertises: pause, and nothing the common player drives through
/// `DriverSet` for seek or tracks (those reach the feed through its own verbs).
const FEED_CAPABILITIES: Capabilities = Capabilities {
    progressive_uri: false, hls_uri: false, packet_feed: true,
    pause: true, seek: false, audio_track: false, subtitle_track: false,
    audio_off: false, subtitle_off: false,
};

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
    TickLoad { now: u32, segmented_hls: bool, budget: Duration, speed_deadline_ms: u32,
        no_audio: bool },
    TickPresentation { segmented_hls: bool, immersive: bool },
    CtorGate,
    TakeLoadOutcomes,
    NativeAudioStallFired { now: u32 },
    NoteMissingEraCallback,
    InjectRefuse601 { generation: u32, preview: bool },
    PublishDiag { now: u32, diag_readout_on: bool },
    DeferPreviewJoin,
    Sample,
    UserPause,
    UserResume,
    TakePrerollVolume,
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
    pub user_pause: UserPauseOutcome,
    pub user_resume: UserResumeOutcome,
    /// Delivered once by the next `take_preroll_volume`.
    pub preroll_volume: Option<i32>,
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
            user_pause: UserPauseOutcome::Deferred,
            user_resume: UserResumeOutcome::NoHold,
            preroll_volume: None,
        }
    }
}

#[derive(Default)]
struct Shared {
    script: FeedScript,
    calls: Vec<FeedCall>,
    dispatched: Vec<(AttemptId, CommandId, Operation)>,
    events: VecDeque<BackendEvent>,
    exit_preserved: bool,
    /// Whether `defer_preview_join` marked the preview abandoned.
    preview_abandoned: bool,
}

/// The test's side of a [`FakeFeed`] or [`FakePhysical`]: what it was asked, and what it should
/// say next.
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
    /// Every command the runner dispatched to the feed backend.
    pub fn dispatched(&self) -> Vec<(AttemptId, CommandId, Operation)> {
        self.0.lock().unwrap().dispatched.clone()
    }
    /// Events the next poll returns, in order.
    pub fn queue(&self, events: impl IntoIterator<Item = BackendEvent>) {
        self.0.lock().unwrap().events.extend(events);
    }
    /// Whether `defer_preview_join` deferred and marked the preview abandoned.
    pub fn preview_abandoned(&self) -> bool { self.0.lock().unwrap().preview_abandoned }
    /// Whether native ownership is still held at exit.
    pub fn set_exit_preserved(&self, preserved: bool) {
        self.0.lock().unwrap().exit_preserved = preserved;
    }
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
    fn tick_load(&mut self, _mt: &MainThread, now: u32, segmented_hls: bool, budget: Duration,
        speed_deadline_ms: u32, no_audio: bool) -> LoadTick {
        self.answer(FeedCall::TickLoad { now, segmented_hls, budget, speed_deadline_ms, no_audio },
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
}

/// A scriptable [`PhysicalSet`]: the runner's [`DriverSet`] for the packet-feed backend it holds,
/// and the lender of its [`FakeFeed`]. A set built without a feed is a composition with no feed
/// installed: it advertises nothing, refuses every dispatch and lends `None`.
pub struct FakePhysical {
    feed: Option<FakeFeed>,
    shared: Arc<Mutex<Shared>>,
}

impl FakePhysical {
    /// A set holding a feed, and the handle that scripts and inspects it.
    pub fn new() -> (Self, FakeFeedHandle) {
        let (feed, handle) = FakeFeed::new();
        let shared = Arc::clone(&handle.0);
        (Self { feed: Some(feed), shared }, handle)
    }

    /// A set with no feed; the handle still reads the (empty) log and queues nothing deliverable.
    pub fn without_feed() -> (Self, FakeFeedHandle) {
        let shared = Arc::new(Mutex::new(Shared::default()));
        (Self { feed: None, shared: Arc::clone(&shared) }, FakeFeedHandle(shared))
    }
}

impl DriverSet for FakePhysical {
    type Context = MainThread;

    fn capabilities(&self, backend: BackendKind) -> Option<Capabilities> {
        (backend == BackendKind::Feed && self.feed.is_some()).then_some(FEED_CAPABILITIES)
    }

    fn dispatch(&mut self, backend: BackendKind, _ctx: &MainThread, attempt: AttemptId,
        command: CommandId, operation: Operation) -> DispatchResult {
        if backend != BackendKind::Feed || self.feed.is_none() {
            return DispatchResult::Refused(Failure::Unavailable);
        }
        self.shared.lock().unwrap().dispatched.push((attempt, command, operation));
        DispatchResult::Queued
    }

    fn poll(&mut self, _ctx: &MainThread) -> Vec<BackendEvent> {
        self.shared.lock().unwrap().events.drain(..).collect()
    }

    fn discard_staged(&mut self, _backend: BackendKind, _attempt: AttemptId, _key: SourceKey) {}

    fn exit_disposition(&self) -> ExitDisposition {
        if self.shared.lock().unwrap().exit_preserved {
            ExitDisposition::PreserveNativeOwnership
        } else {
            ExitDisposition::Clean
        }
    }
}

impl PhysicalSet for FakePhysical {
    fn feed(&mut self) -> Option<&mut dyn FeedControl> {
        self.feed.as_mut().map(|feed| feed as &mut dyn FeedControl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mt() -> MainThread {
        // SAFETY: a host test; nothing here touches the thread-affine state the token guards.
        unsafe { MainThread::assume() }
    }

    /// A composition hands the adapter a boxed `PhysicalSet`; a fake drives that exact object, and
    /// the feed it lends records each verb the pump forwarded, with the arguments it was given.
    #[test]
    fn a_fake_physical_set_lends_a_feed_that_logs_the_forwarded_verbs() {
        let (fake, handle) = FakePhysical::new();
        let mut set: Box<dyn PhysicalSet<Context = MainThread>> = Box::new(fake);
        let feed = set.feed().expect("the fake installs a feed");
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
        let (fake, handle) = FakePhysical::new();
        let outcome = AcceptedLoadOutcome { attempt: AttemptId::new(1, 3), ok: true,
            elapsed_ms: Some(40) };
        handle.script(|script| {
            script.facts.raw.load.stage = Some(Stage::Streaming);
            script.paused_seek_step = PausedSeekStep { finish: true, mid_seek_claimed: false, preroll_opened: false };
            script.retry_stuck_seek = StuckSeekAction::Retried(1);
            script.seek_pause = SeekPause::Busy;
            script.load_outcomes = vec![outcome];
        });
        let mut set: Box<dyn PhysicalSet<Context = MainThread>> = Box::new(fake);
        let feed = set.feed().unwrap();
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
        let (fake, handle) = FakePhysical::new();
        let mut set = fake;
        let feed = PhysicalSet::feed(&mut set).unwrap();
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
            let (fake, handle) = FakePhysical::new();
            handle.script(|script| script.facts.raw.load = raw);
            let mut set: Box<dyn PhysicalSet<Context = MainThread>> = Box::new(fake);
            let feed = set.feed().unwrap();
            assert_eq!(feed.defer_preview_join(), defers, "{raw:?}");
            assert_eq!(handle.preview_abandoned(), defers, "{raw:?}: the mark follows the answer");
            assert_eq!(handle.calls(), [FeedCall::DeferPreviewJoin]);
        }
    }

    /// A composition with no feed answers `None`, as the default set does.
    #[test]
    fn a_fake_set_without_a_feed_lends_none() {
        let (fake, handle) = FakePhysical::without_feed();
        let mut set: Box<dyn PhysicalSet<Context = MainThread>> = Box::new(fake);
        assert!(set.feed().is_none());
        assert_eq!(crate::BackendSet::from_capabilities(&*set), crate::BackendSet::NONE);
        assert!(handle.calls().is_empty());
    }

    /// The fake is also the runner's `DriverSet`: it answers for the feed it holds, records what
    /// was dispatched, and delivers the events a test queued once.
    #[test]
    fn the_fake_set_is_a_driver_set_that_records_dispatch_and_delivers_queued_events() {
        let (mut fake, handle) = FakePhysical::new();
        assert_eq!(crate::BackendSet::from_capabilities(&fake), crate::BackendSet::FEED);
        assert!(matches!(
            fake.dispatch(BackendKind::Uri, &mt(), AttemptId::new(1, 1), CommandId(1),
                Operation::Pause),
            DispatchResult::Refused(Failure::Unavailable)));
        assert_eq!(fake.dispatch(BackendKind::Feed, &mt(), AttemptId::new(1, 1), CommandId(2),
            Operation::Pause), DispatchResult::Queued);
        assert_eq!(handle.dispatched(), [(AttemptId::new(1, 1), CommandId(2), Operation::Pause)]);
        let event = crate::BackendEvent { backend: BackendKind::Feed,
            event: crate::DriverEvent::Release { attempt: AttemptId::new(1, 1),
                status: crate::ReleaseStatus::Released } };
        handle.queue([event]);
        assert_eq!(fake.poll(&mt()), [event]);
        assert!(fake.poll(&mt()).is_empty(), "events are delivered once");
        assert_eq!(fake.exit_disposition(), ExitDisposition::Clean);
        handle.set_exit_preserved(true);
        assert_eq!(fake.exit_disposition(), ExitDisposition::PreserveNativeOwnership);
    }
}
