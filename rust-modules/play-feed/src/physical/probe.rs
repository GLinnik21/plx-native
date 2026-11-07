//! Test support: a handle on the [`FeedSet`] an adapter boxes as its `dyn PhysicalSet`.
//!
//! The adapter holds its set as `Box<dyn PhysicalSet>`, so a test fixture that installs, takes or
//! inspects the Engine inside the Feed cannot reach it through the box, and the production traits
//! carry no downcast and no test-only verb. [`Probe::new`] builds the set the other way round:
//! it shares one [`FeedSet`] between a [`SharedFeedSet`], which is the box's content and
//! forwards every [`DriverSet`], [`PhysicalSet`] and Feed verb to it, and a [`Probe`], which the
//! adapter keeps beside the box (the player's `PlayerAdapter::new_probed`).
//! The fixtures read the Feed's resources through the probe; everything the adapter itself does
//! goes through the box, so the code under test is the production code over the same [`FeedSet`].
//!
//! The forwarder is the Feed the pump sees (a borrow of the shared value cannot outlive the call
//! that takes it), and it seats no URI driver: a probed adapter is for the Engine fixtures.
//! Every verb borrows the shared value for the length of the call, so a verb that re-entered the
//! adapter's fixtures would panic on the second borrow; none does.

use super::FeedSet;
use crate::feed_resources::FeedResources;
use crate::feed_state::Engine;
use plx_base::task::MainThread;
use plx_playback_contract::demux::DemuxSession;
use plx_playback_contract::feed::{
    AcceptedLoadOutcome, CtorGate, DvWithdrawal, FeedCommand, FeedControl, FeedFacts, FeedLoad, FeedOpen,
    FeedSeek, FeedStop, FeedTransportAction, LoadTick, OpenRequest, PausedSeekStep, PhysicalSet,
    PresentationTick, ReleaseHandoff, ResetScope, SeekMode, SeekPause, StuckSeekAction,
    UnblockFacts, UserPauseOutcome, UserResumeOutcome,
};
use plx_playback_contract::{
    AttemptId, BackendEvent, BackendKind, Capabilities, CommandId, DispatchResult, DriverSet,
    ExitDisposition, Failure, Operation, SourceKey,
};
use std::cell::{RefCell, RefMut};
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, AtomicU32};

/// The test fixtures' handle on the [`FeedSet`] behind a [`SharedFeedSet`].
pub struct Probe(Rc<RefCell<FeedSet>>);

impl Probe {
    /// `set`, as the forwarding set an adapter boxes and the handle it keeps.
    pub fn new(set: FeedSet) -> (Self, Box<dyn PhysicalSet>) {
        let shared = Rc::new(RefCell::new(set));
        (Self(Rc::clone(&shared)), Box::new(SharedFeedSet(shared)))
    }

    /// Run `fixture` over the Feed's resources: the Engine slot, the Load receipts, the release
    /// slot and the unload spawner.
    pub fn resources<R>(&self, fixture: impl FnOnce(&mut FeedResources) -> R) -> R {
        fixture(self.0.borrow_mut().feed.resources_mut())
    }

    /// The installed Engine, borrowed from the shared value until the guard drops.
    pub fn engine(&self) -> Option<RefMut<'_, Engine>> {
        let mut physical = self.0.borrow_mut();
        physical.feed.resources_mut().engine()?;
        Some(RefMut::map(physical, |physical| {
            physical.feed.resources_mut().engine().expect("the Engine was installed a line above")
        }))
    }
}

/// Forwards `&mut self` Feed verbs to the shared [`FeedSet`]'s Feed.
macro_rules! forward_mut {
    ($($name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)?;)*) => {$(
        fn $name(&mut self $(, $arg: $ty)*) $(-> $ret)? {
            self.with_feed(|feed| feed.$name($($arg),*))
        }
    )*};
}

/// Forwards `&self` Feed queries to the shared [`FeedSet`]'s Feed.
macro_rules! forward_ref {
    ($($name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)?;)*) => {$(
        fn $name(&self $(, $arg: $ty)*) $(-> $ret)? {
            self.with_feed(|feed| feed.$name($($arg),*))
        }
    )*};
}

/// The [`FeedSet`] shared with a [`Probe`], as the adapter's `dyn PhysicalSet`.
struct SharedFeedSet(Rc<RefCell<FeedSet>>);

impl SharedFeedSet {
    fn with_feed<R>(&self, verb: impl FnOnce(&mut dyn FeedControl) -> R) -> R {
        let mut physical = self.0.borrow_mut();
        verb(physical.feed().expect("a probed set is only built over a feed build"))
    }
}

impl DriverSet for SharedFeedSet {
    type Context = MainThread;

    fn capabilities(&self, backend: BackendKind) -> Option<Capabilities> {
        self.0.borrow().capabilities(backend)
    }

    fn dispatch(&mut self, backend: BackendKind, mt: &MainThread, attempt: AttemptId,
        command: CommandId, operation: Operation) -> DispatchResult {
        self.0.borrow_mut().dispatch(backend, mt, attempt, command, operation)
    }

    fn poll(&mut self, mt: &MainThread) -> Vec<BackendEvent> { self.0.borrow_mut().poll(mt) }

    fn discard_staged(&mut self, backend: BackendKind, attempt: AttemptId, key: SourceKey) {
        self.0.borrow_mut().discard_staged(backend, attempt, key);
    }

    fn exit_disposition(&self) -> ExitDisposition { self.0.borrow().exit_disposition() }
}

impl PhysicalSet for SharedFeedSet {
    fn feed(&mut self) -> Option<&mut dyn FeedControl> { Some(self) }
}

impl FeedSeek for SharedFeedSet {
    forward_mut! {
        paused_seek_step(now: u32, route_flight: bool, claim_owns_pause: bool) -> PausedSeekStep;
        stuck_seek_due(now: u32, feed_allowed: bool, route_flight: bool, budget_ms: u32) -> bool;
        retry_stuck_seek(now: u32, target_ns: i64, can_retry: bool) -> StuckSeekAction;
        prepare_seek_pause(mt: &MainThread) -> SeekPause;
        begin_seek_discontinuity() -> bool;
        apply_inplace_seek_pre(mt: &MainThread, target_ns: i64, mode: SeekMode, coalesced: u32);
        apply_inplace_seek_post(target_ns: i64, now: u32);
    }
    forward_ref! {
        pending_seek_ready(seek_reloads: bool) -> bool;
    }
}

impl FeedLoad for SharedFeedSet {
    forward_mut! {
        tick_load(mt: &MainThread, now: u32, segmented_hls: bool,
            speed_deadline_ms: u32, no_audio: bool) -> LoadTick;
        tick_presentation(mt: &MainThread, segmented_hls: bool, immersive: bool)
            -> PresentationTick;
        ctor_gate(mt: &MainThread) -> CtorGate;
        take_load_outcomes() -> Vec<AcceptedLoadOutcome>;
        native_audio_stall_fired(now: u32) -> bool;
        dv_landing_due(now: u32, route_flight: bool) -> Option<DvWithdrawal>;
        note_missing_era_callback();
        inject_refuse601(generation: u32, preview: bool, refused_for: &AtomicU32);
        publish_diag(now: u32, last_frames: &AtomicI32, diag_readout_on: bool);
        defer_preview_join() -> bool;
        reap_abandoned_load(mt: &MainThread) -> bool;
    }
    forward_ref! {
        sample() -> Option<FeedFacts>;
        has_abandoned_load() -> bool;
    }
}

impl FeedOpen for SharedFeedSet {
    forward_mut! {
        open_begin(mt: &MainThread) -> bool;
        open_prepare(mt: &MainThread, request: OpenRequest) -> Result<u32, Failure>;
        open_stage(attempt: AttemptId, key: SourceKey, session: Box<dyn DemuxSession>)
            -> Result<(), Failure>;
        open_abort();
    }
}

impl FeedStop for SharedFeedSet {
    forward_mut! {
        stop_begin(terminal: bool) -> Option<FeedFacts>;
        stop_side_reader(for_reload: bool);
        stop_unblock() -> UnblockFacts;
        stop_report_dispatched();
        stop_release(mt: &MainThread, for_reload: bool);
        stop_finish(for_reload: bool) -> Option<ReleaseHandoff>;
        reset(scope: ResetScope, common: &mut dyn FnMut());
        take_returned_release() -> Option<u32>;
        run_release_tail(mt: &MainThread, epoch: u32) -> bool;
        leak_for_exit();
    }
}

impl FeedControl for SharedFeedSet {
    forward_mut! {
        command(command: FeedCommand);
        user_pause(mt: &MainThread) -> UserPauseOutcome;
        user_resume(mt: &MainThread) -> UserResumeOutcome;
        take_preroll_volume() -> Option<i32>;
        take_transport_action() -> Option<FeedTransportAction>;
        note_transport_result(mt: &MainThread, action: FeedTransportAction, accepted: bool,
            native_acknowledged: bool);
    }
}
