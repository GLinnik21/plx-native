//! **The Player ADAPTER** — the main-thread token and the owned backend resources it confines
//! (restructure spec §2.2, phase 9).
//!
//! An adapter holds OS/FFI resources and never logical state; the decisions live one field away in
//! [`crate::player::machine::Player`]. Buffer-feed ownership is grouped in [`FeedResources`]:
//! the [`Engine`] slot, native Load receipts, abandoned Load and parked release. The adapter
//! retains the [`MainThread`] token, periodic Plex reporter, release policy, and URI admission.
//!
//! # Why the slot moved here, and what replaced the token argument
//!
//! Until phase 9 the slot was `static mut ENGINE: Option<Engine>`, reachable only through four
//! accessors that each took a `&MainThread`. That argument was the enforcement: the accessor
//! handed out a `&'static mut` to a `static mut`, a second caller on another thread is instant UB,
//! and `static mut` carries no `Sync` bound to stop one (verified by compiling the counterexample
//! — see `docs/async-model-decision.md`).
//!
//! The reasoning is unchanged and the enforcement is stronger. **The token is now CONSUMED at
//! construction** ([`PlayerAdapter::new`] takes it by value, and `plex_run` is the only place that
//! can mint one), so holding a `&mut PlayerAdapter` IS the proof the old argument stood in for —
//! and it is a proof the borrow checker keeps rather than one a caller could satisfy twice. Two
//! live `&mut` to the engine no longer requires a convention: it does not compile. The native
//! feed owns that borrow; common callers receive addressed facts or coarse physical actions.
//!
//! The rest of `player/engine.rs` keeps its `mt: &MainThread` parameters unchanged. They gate a
//! DIFFERENT thing — the ACB/Starfish seam (`tv::sink::VideoSink`'s methods, whose bind order is a
//! sequence of calls with no locking behind it) — and that surface is not this module's.

#[cfg(test)]
use super::engine::Engine;
use super::{feed_resources::FeedResources, timeline};
#[cfg(test)]
use super::threads;
use plx_base::task::MainThread;

/// Common coordinator and main-thread proof, as one owned value (`App.adapters.player`).
pub struct PlayerAdapter {
    /// Proof, held rather than passed: see the module doc. It is a ZST, so this costs nothing and
    /// is what makes `&mut PlayerAdapter` mean "the main thread, exclusively".
    mt: MainThread,
    /// The native feed slot and its Load/release worker resources.
    pub(super) feed: FeedResources,
    /// Route identity and product trace for the installed physical attempt. The feed itself
    /// receives only the canonical contract ID and native epoch.
    feed_attempt: Option<(crate::route::RouteStartAttempt, u32)>,
    /// Backend-independent periodic reporting resource for the current physical attempt.
    timeline: Option<timeline::TimelineRunner>,
    repair: Option<(u64, std::sync::mpsc::Receiver<Result<(), plx_platform::tv::sandbox::Failure>>)>,
    release: super::release::NativeRelease,
    /// What became of the start the FOREGROUND restore queued behind the release
    /// ([`watch_queued_start`](Self::watch_queued_start)); the lifecycle machine polls it.
    queued_watch: QueuedWatch,
    /// A hero PREVIEW's start was queued behind the release: whether its slot counts as admitted
    /// once the Load actually launches (`None`: no preview start is queued). Admission is taken
    /// when the queued start runs, never when it is merely queued.
    preview_admission: Option<bool>,
    /// The URI-mode probe's session (issue #1064, dev trigger `plxnative-uriprobe`): the probe plays
    /// through `com.webos.media` instead of an [`Engine`], so it lives BESIDE the engine slot and
    /// never in it. [`is_live`](Self::is_live) stays Engine-only: the Engine paths (`pump`, the
    /// HUD, the stop ritual's `take`) must never see a probe. Owned here, not by a static, for the
    /// same reason the Engine is: ending it calls the window verbs of the sink, which only the
    /// main thread may. Absent from a release build.
    #[cfg(feature = "devtriggers")]
    uri: Option<super::uriprobe_drive::UriSession>,
}

/// The foreground restore's view of its own queued start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueuedWatch {
    /// Nobody follows the queued start (an ordinary play lands through `finish_landed_play`).
    Unwatched,
    /// The restore's start is queued; it has not run.
    Watching,
    /// The queued start ran: the Load was launched ...
    Launched(crate::route::RouteStartAttempt),
    /// ... or ended without one (refused at the deadline, or the start itself failed).
    Failed,
}

/// What polling the restore's queued start found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuedLanding {
    Waiting,
    /// Cancelled or superseded: nothing to follow.
    Stale,
    Launched(crate::route::RouteStartAttempt),
    Failed,
}

impl PlayerAdapter {
    /// Take the token. `app::boot` calls this once, with the token `plex_run` minted.
    pub fn new(mt: MainThread) -> Self {
        Self {
            mt,
            feed: FeedResources::new(),
            feed_attempt: None,
            timeline: None,
            repair: None,
            release: super::release::NativeRelease::new(),
            queued_watch: QueuedWatch::Unwatched,
            preview_admission: None,
            #[cfg(feature = "devtriggers")]
            uri: None,
        }
    }

    /// The foreground restore's start was just deferred: follow it.
    pub fn watch_queued_start(&mut self) {
        self.queued_watch = QueuedWatch::Watching;
    }

    /// Is the foreground restore following a queued start?
    pub fn watching_queued_start(&self) -> bool {
        self.queued_watch != QueuedWatch::Unwatched
    }

    /// The restore's queued start ran, and how.
    pub(super) fn queued_start_ran(&mut self, outcome: Option<crate::route::RouteStartAttempt>) {
        if self.queued_watch == QueuedWatch::Watching {
            self.queued_watch = outcome.map_or(QueuedWatch::Failed, QueuedWatch::Launched);
        }
    }

    /// A queued start was cancelled or replaced: whoever watched it has nothing to follow.
    pub(super) fn queued_start_dropped(&mut self) {
        self.queued_watch = QueuedWatch::Unwatched;
    }

    /// Poll the watched start; an answer other than `Waiting` ends the watch.
    pub fn take_queued_landing(&mut self) -> QueuedLanding {
        let landing = match self.queued_watch {
            QueuedWatch::Unwatched => QueuedLanding::Stale,
            QueuedWatch::Watching if self.release.has_queued() => return QueuedLanding::Waiting,
            QueuedWatch::Watching => QueuedLanding::Stale,
            QueuedWatch::Launched(a) => QueuedLanding::Launched(a),
            QueuedWatch::Failed => QueuedLanding::Failed,
        };
        self.queued_watch = QueuedWatch::Unwatched;
        landing
    }

    /// The preview's start was just deferred: it is admitted (`admit`) or refused when it runs.
    pub fn admit_preview_at_launch(&mut self, admit: bool) {
        self.preview_admission = Some(admit);
    }

    /// The queued start ended; settle the preview's admission with how it ended. `launched` false
    /// (refused at the deadline, failed) counts as a refused admission; a cancelled start
    /// (`settle = false`) leaves the preview machine to the stop that cancelled it.
    pub(super) fn settle_preview_admission(&mut self, launched: bool, settle: bool) {
        if let Some(admit) = self.preview_admission.take() {
            if settle {
                if launched && admit {
                    super::preview::note_admitted();
                } else {
                    super::preview::note_admission_refused();
                }
            }
        }
    }

    /// Is a start queued behind the running `Unload`?
    pub fn release_has_queued_start(&self) -> bool {
        self.release.has_queued()
    }

    /// Park a real stop's off-frame release. At most one exists: a start over it is deferred.
    pub fn park_release(&mut self, release: super::engine::PendingRelease) {
        let (epoch, at) = (release.epoch(), release.handed_off_at());
        self.feed.park_release(release);
        self.release_step(&super::release::ReleaseEvent::UnloadHandedOff { epoch, at }, &mut Vec::new());
    }

    /// Is a stopped native object's `Unload` still owed its main-thread tail?
    pub fn has_pending_release(&self) -> bool {
        self.feed.has_pending_release()
    }

    /// The release machine's verdict on one event; the effects are the caller's to run.
    pub(super) fn release_step(&mut self, event: &super::release::ReleaseEvent, fx: &mut Vec<super::release::ReleaseEffect>) {
        self.release.step(event, fx);
        // Publish the one fact `player::state` needs (no session copy of it can drift).
        super::SHARED
            .start_queued
            .store(self.release.has_queued(), std::sync::atomic::Ordering::Release);
    }

    /// An `Unload` is running or overdue: the object slot is not free.
    pub fn release_busy(&self) -> bool {
        self.release.is_busy()
    }

    /// The `Unload` outlived its deadline: it stays parked and every start is refused.
    pub fn release_overdue(&self) -> bool {
        self.release.is_overdue()
    }

    /// The parked release, taken only once its worker has returned (so joining it cannot block).
    pub(super) fn take_finished_release(&self) -> Option<super::engine::PendingRelease> {
        self.feed.take_finished_release()
    }

    /// The parked release whether or not its worker returned: only the exit, which leaks it
    /// (`engine::leak_parked_for_exit`), takes it that way.
    pub(super) fn take_pending_release_any(&self) -> Option<super::engine::PendingRelease> {
        self.feed.take_pending_release_any()
    }

    /// UI-thread resource effect. The owner spends the attempt before the worker can run.
    pub fn repair_sandbox(&mut self, owner: &mut super::machine::RepairAttempt, supported: bool) {
        let Some(token) = owner.begin(supported) else { return; };
        let (tx, rx) = std::sync::mpsc::channel();
        if plx_base::task::spawn_small("jail repair", move || {
            let _ = tx.send(plx_platform::tv::sandbox::repair());
        }) {
            self.repair = Some((token, rx));
        } else {
            owner.complete(token, Err(plx_platform::tv::sandbox::Failure::StartFailed));
        }
    }

    /// Poll on every app frame, including while the player page is absent.
    pub fn poll_repair(&mut self, owner: &mut super::machine::RepairAttempt) -> bool {
        let Some((token, rx)) = self.repair.as_ref() else { return false; };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(plx_platform::tv::sandbox::Failure::StartFailed),
        };
        let changed = owner.complete(*token, result);
        self.repair = None;
        changed
    }

    /// Test-only access to the physical session for the existing hostsim rigs.
    #[cfg(test)]
    #[inline]
    pub fn engine(&mut self) -> Option<&mut Engine> {
        self.feed.engine()
    }

    /// Route identity of the active physical feed, without lending Engine state to admission.
    pub(super) fn active_feed_attempt(&self) -> Option<crate::route::RouteStartAttempt> {
        let (attempt, _) = self.feed_attempt?;
        (self.feed.active_attempt() == Some(attempt.contract_id())).then_some(attempt)
    }

    pub(super) fn bind_feed_attempt(&mut self, attempt: crate::route::RouteStartAttempt,
        trace_generation: u32) {
        debug_assert_eq!(self.feed.active_attempt(), Some(attempt.contract_id()));
        self.feed_attempt = Some((attempt, trace_generation));
    }

    /// The physical executor consumes only the route owner's canonical address and its owned
    /// prepared source; common route identity is bound after installation succeeds.
    pub(super) fn launch_native_feed(&mut self, attempt: plx_playback_contract::AttemptId,
        prepared: super::engine::NativePrepared) -> Result<(), super::engine::NativeOpenError> {
        super::engine::launch_native_open(&mut self.feed, attempt, prepared)
    }

    pub(super) fn clear_feed_attempt(&mut self) {
        self.feed_attempt = None;
    }

    pub(super) fn take_native_stop(&mut self) -> super::engine::NativeStop {
        let stop = super::engine::NativeStop::take(&mut self.feed);
        self.clear_feed_attempt();
        stop
    }

    pub(super) fn release_native_stop(&mut self, stop: &mut super::engine::NativeStop,
        for_reload: bool) {
        stop.release_and_drain(&self.mt, &mut self.feed, for_reload);
    }

    pub(super) fn finish_native_stop(&mut self, stop: super::engine::NativeStop,
        for_reload: bool) {
        stop.finish(&mut self.feed, for_reload);
    }

    /// Address the physical callback snapshot to the current route-owned attempt.
    pub(super) fn publish_feed_attempt_view(&self, ps: &mut crate::route::PlaybackSession) {
        let Some(attempt) = self.active_feed_attempt() else { return; };
        let Some(facts) = super::feed_pump::attempt_facts(&self.feed) else { return; };
        let _ = ps.ingest_attempt_facts(attempt, facts);
    }

    pub(super) fn feed_uses_stream_queues(&self) -> bool { self.feed.uses_stream_queues() }
    pub(super) fn arm_feed_live_clock_prime(&mut self) -> bool { self.feed.arm_live_clock_prime() }
    pub(super) fn feed_stage(&self) -> Option<super::shared::Stage> { self.feed.stage() }
    pub(super) fn feed_load_thread_finished(&self) -> bool { self.feed.load_thread_finished() }

    /// Whether the current physical feed has yet to leave its Load stage.
    pub(super) fn feed_loading(&self) -> bool { self.feed.loading() }
    pub(super) fn preview_load_join_state(&self) -> Option<(bool, bool)> {
        self.feed.preview_load_join_state()
    }
    pub(super) fn abandon_preview_load(&mut self) { self.feed.abandon_preview(); }
    pub(super) fn foreground_follows_feed_load(&self) -> bool {
        self.feed.foreground_follows_load()
    }
    pub(super) fn mark_foreground_follows_feed_load(&mut self,
        attempt: crate::route::RouteStartAttempt) -> bool {
        if self.active_feed_attempt() != Some(attempt) { return false; }
        self.feed.mark_foreground_follows_load(attempt.contract_id())
    }
    /// Native readiness for a route/action claim; the route policy itself stays in the pump.
    pub(super) fn route_actions_ready(&self) -> bool { self.feed.route_actions_ready() }

    /// One physical seek-preroll tick. The common pump supplies route-flight facts and applies
    /// the returned finish request after this borrow of feed resources ends.
    pub(super) fn paused_seek_step(&mut self, now: u32, route_flight: bool,
        claim_owns_pause: bool) -> super::feed_pump::PausedSeekStep {
        super::feed_pump::paused_seek_step(&mut self.feed, now, route_flight, claim_owns_pause)
    }

    /// Publish native queue and frame diagnostics without exposing Engine to the common prelude.
    pub(super) fn publish_feed_diag(&mut self, now: u32,
        last_frames: &std::sync::atomic::AtomicI32) {
        super::feed_pump::publish_diag(&mut self.feed, now, last_frames);
    }

    /// Feed the dev firmware-refusal probe with route facts, before any route landing can replace
    /// this physical Engine in the same frame.
    pub(super) fn inject_refuse601(&mut self, generation: u32, preview: bool,
        refused_for: &std::sync::atomic::AtomicU32) {
        super::feed_pump::inject_refuse601(&mut self.feed, generation, preview, refused_for);
    }

    /// Probe the native audio feed's own stall evidence after common Load-refusal handling.
    pub(super) fn native_audio_stall_fired(&mut self, now: u32) -> bool {
        super::feed_pump::native_audio_stall_fired(&mut self.feed, now)
    }

    /// Gate the active physical tick on native constructor readiness.
    pub(super) fn feed_ctor_gate(&mut self) -> super::feed_pump::CtorGate {
        super::feed_pump::ctor_gate(&mut self.feed, &self.mt)
    }

    /// Emit the physical callback-cadence diagnostic after first-picture route settlement.
    pub(super) fn note_missing_era_callback(&mut self) {
        super::feed_pump::note_missing_era_callback(&mut self.feed);
    }

    pub(super) fn stuck_seek_due(&mut self, now: u32, feed_allowed: bool,
        route_flight: bool, budget_ms: u32) -> bool {
        super::feed_pump::stuck_seek_due(&mut self.feed, now, feed_allowed, route_flight, budget_ms)
    }

    pub(super) fn retry_stuck_seek(&mut self, now: u32, target_ns: i64,
        can_retry: bool) -> super::feed_pump::StuckSeekAction {
        super::feed_pump::retry_stuck_seek(&mut self.feed, now, target_ns, can_retry)
    }

    pub(super) fn pending_seek_ready(&self, seek_reloads: bool) -> bool {
        super::feed_pump::pending_seek_ready(&self.feed, seek_reloads)
    }
    pub(super) fn prepare_seek_pause(&self) -> super::feed_pump::SeekPause {
        super::feed_pump::prepare_seek_pause(&self.mt)
    }
    pub(super) fn begin_seek_discontinuity(&self) -> bool {
        super::feed_pump::begin_seek_discontinuity(&self.feed)
    }
    pub(super) fn apply_inplace_seek_pre(&mut self, target_ns: i64,
        mode: super::SeekMode, coalesced: u32) {
        super::feed_pump::apply_inplace_seek_pre(&mut self.feed, &self.mt, target_ns, mode, coalesced);
    }
    pub(super) fn apply_inplace_seek_post(&mut self, target_ns: i64, now: u32) {
        super::feed_pump::apply_inplace_seek_post(&mut self.feed, target_ns, now);
    }

    pub(super) fn tick_feed_load(&mut self, now: u32, segmented_hls: bool,
        budget: std::time::Duration, speed_deadline_ms: u32) -> super::feed_pump::LoadTick {
        super::feed_pump::tick_load(&mut self.feed, &self.mt, now,
            segmented_hls, budget, speed_deadline_ms)
    }

    pub(super) fn tick_feed_presentation(&mut self, segmented_hls: bool,
        immersive: bool) -> super::feed_pump::PresentationTick {
        super::feed_pump::tick_presentation(&mut self.feed, &self.mt, segmented_hls, immersive)
    }

    /// Is a session live? Distinct from [`engine`](Self::engine) because it answers without taking
    /// the exclusive borrow — `start_bufferfeed`'s double-start guard only needs to ask.
    #[inline]
    pub fn is_live(&self) -> bool {
        self.feed.is_live()
    }

    /// Address one Load verdict without handing route state to the native worker.
    #[cfg(test)]
    pub(super) fn load_receipt(&mut self, attempt: crate::route::RouteStartAttempt, native_epoch: u32,
        _trace_generation: u32) -> threads::LoadReceipt {
        self.feed.load_receipt(attempt.contract_id(), native_epoch)
    }

    /// A refused thread spawn cannot return a receipt; retire its mapping immediately.
    #[cfg(test)]
    pub(super) fn cancel_load_receipt(&mut self, token: u64) { self.feed.cancel_load_receipt(token); }

    /// The main-thread coordinator's only Load-result landing. Late outcomes from an abandoned
    /// or replaced Engine cannot settle a successor's route, even if a worker returns afterward.
    pub(super) fn drain_load_outcomes(&mut self, ps: &mut crate::route::PlaybackSession) {
        // Construction may still be installing the Engine when the synchronous Load returns.
        // Leave the inbox intact until its owner can validate the native epoch and attempt.
        if !self.feed.is_live() { return; }
        for outcome in self.feed.take_load_outcomes() {
            let Some((attempt, trace_generation)) = self.feed_attempt
                .filter(|(attempt, _)| attempt.contract_id() == outcome.attempt) else { continue; };
            if let Some(elapsed_ms) = outcome.elapsed_ms {
                super::report::note_load_gate_for(
                    trace_generation,
                    super::report::LoadElapsedClass::from_ms(elapsed_ms as i64),
                );
            }
            let result = if outcome.ok { crate::route::RouteStartResult::Started }
                else { crate::route::RouteStartResult::StartFailed };
            let _ = crate::route::settle_route_start(ps, attempt, result);
        }
    }

    /// A preview has no route lease, hence no periodic PMS writes. This follows the existing
    /// begin_timeline_reporting gate and keeps the reporter outside the Engine resource set.
    pub(super) fn start_timeline(&mut self, ps: &crate::route::PlaybackSession,
        attempt: crate::route::RouteStartAttempt) {
        debug_assert!(self.timeline.is_none(), "one reporter per player adapter");
        if self.timeline.is_some() { return; }
        if let Some(lease) = crate::route::begin_timeline_reporting(ps) {
            self.timeline = timeline::TimelineRunner::start(attempt, lease);
        }
    }

    pub(super) fn publish_timeline(&self, attempt: crate::route::RouteStartAttempt,
        snapshot: Option<timeline::TimelineSnapshot>) {
        if let Some(runner) = self.timeline.as_ref() { runner.publish(attempt, snapshot); }
    }

    /// Stop and detach without joining on the main thread. Real stop passes this handle to
    /// route::scrobble_stop; reload simply drops it after the stop signal became sticky.
    pub(super) fn stop_timeline(&mut self, attempt: crate::route::RouteStartAttempt)
        -> Option<std::thread::JoinHandle<()>> {
        let runner = self.timeline.take()?;
        debug_assert_eq!(runner.attempt(), attempt, "reporter must belong to the Engine being torn down");
        Some(runner.stop())
    }

    /// Install the freshly-built session. Overwriting a live slot would DROP an Engine whose
    /// workers hold raw pointers into the boxes it owns; `start_bufferfeed` guards on
    /// [`is_live`](Self::is_live) first.
    #[inline]
    #[cfg(test)]
    pub fn install(&mut self, e: Engine) {
        self.feed_attempt = Some((crate::route::RouteStartAttempt::fixture(), 0));
        self.feed.install(e);
    }

    /// Take the session out of the slot; the caller then joins its workers and drops it.
    #[inline]
    #[cfg(test)]
    pub fn take(&mut self) -> Option<Engine> {
        self.feed_attempt = None;
        self.feed.take()
    }

    /// Is a URI-mode probe session running? `start_bufferfeed`'s double-start guard asks this beside
    /// [`is_live`](Self::is_live): a probe holds the route-start attempt and the exported window, so
    /// a second Play cannot begin under it.
    #[cfg(feature = "devtriggers")]
    #[inline]
    pub fn uri_live(&self) -> bool {
        self.uri.is_some()
    }

    /// The service says the probe's video plays (`observe_video_plane`'s term for a session which
    /// has no Engine to read the ACB play state from).
    #[cfg(feature = "devtriggers")]
    #[inline]
    pub fn uri_plane_open(&self) -> bool {
        self.uri.as_ref().is_some_and(|session| session.plane_open())
    }

    #[cfg(feature = "devtriggers")]
    pub fn uri_closing(&self) -> bool {
        self.uri.as_ref().is_some_and(|session| session.closing())
    }

    #[cfg(feature = "devtriggers")]
    pub(super) fn uri_attempt(&self) -> Option<crate::route::RouteStartAttempt> {
        self.uri.as_ref().map(|session| session.route_start_attempt())
    }

    #[cfg(feature = "devtriggers")]
    pub(super) fn uri_acb_exit_quarantine(&self) -> bool { self.uri.as_ref().is_some_and(|session| session.acb_exit_quarantine()) }

    /// Install the probe's session. `uriprobe_drive::start` guards on [`uri_live`](Self::uri_live)
    /// first: overwriting a live slot would drop a session without ending it.
    #[cfg(feature = "devtriggers")]
    pub(super) fn install_uri(&mut self, session: super::uriprobe_drive::UriSession) {
        debug_assert!(self.uri.is_none(), "a second URI-mode session cannot start under a live one");
        self.uri = Some(session);
    }

    /// Take a physically released probe out of the slot, or leak unresolved ownership at exit.
    #[cfg(feature = "devtriggers")]
    #[inline]
    pub(super) fn take_uri(&mut self) -> Option<super::uriprobe_drive::UriSession> {
        self.uri.take()
    }

    /// The probe's session AND the token at once, borrowed from disjoint fields (the same split
    /// [`split`](Self::split) makes for the Engine).
    #[cfg(feature = "devtriggers")]
    #[inline]
    pub(super) fn uri_split(&mut self) -> (Option<&mut super::uriprobe_drive::UriSession>, &MainThread) {
        (self.uri.as_mut(), &self.mt)
    }

    /// Park a still-in-flight Load for a later main-thread release. At most one can exist: the
    /// C seam owns one object at a time and every start is refused while this slot is full.
    pub fn park_abandoned_load(&mut self, load: super::engine::AbandonedLoad) {
        self.feed.park_abandoned_load(load);
    }

    /// Is a native object still parked behind a Load that has not been released?
    pub fn has_abandoned_load(&self) -> bool {
        self.feed.has_abandoned_load()
    }

    /// The parked Load, borrowed mutably (its once-only log latch), or taken once it returned.
    pub fn abandoned_load_mut(&mut self) -> Option<&mut super::engine::AbandonedLoad> {
        self.feed.abandoned_load_mut()
    }

    pub fn take_abandoned_load(&mut self) -> Option<super::engine::AbandonedLoad> {
        self.feed.take_abandoned_load()
    }

    /// The token, for the ACB/Starfish seam. `tv::sink::VideoSink`'s methods still take one — see the
    /// module doc for why that surface keeps its own argument.
    ///
    /// Does not wait for a pending off-frame release: a caller holding the token may call any verb,
    /// and while the object is `Releasing` `player::sink()` answers a sink on which every verb is
    /// a no-op, so none of them reaches the object the worker is inside.
    #[inline]
    pub fn mt(&self) -> &MainThread {
        &self.mt
    }

    /// Hostsim fixture access to the session and main-thread token together. Production common
    /// code uses the feed's typed actions and does not borrow the Engine.
    #[inline]
    #[cfg(test)]
    pub fn split(&mut self) -> (Option<&mut Engine>, &MainThread) {
        (self.feed.engine(), &self.mt)
    }
}

#[cfg(test)]
mod repair_receipt_tests {
    use super::*;
    use plx_platform::tv::sandbox::{Failure, State};
    #[test]
    fn a_receipt_lands_without_a_player_screen_and_cannot_rearm_the_attempt() {
        let mut owner = super::super::machine::RepairAttempt::new();
        let token = owner.begin(true).unwrap();
        let mut adapter = PlayerAdapter::new(unsafe { MainThread::assume() });
        let (tx, rx) = std::sync::mpsc::channel();
        adapter.repair = Some((token, rx));
        assert!(!adapter.poll_repair(&mut owner));
        tx.send(Err(Failure::Timeout)).unwrap();
        assert!(adapter.poll_repair(&mut owner));
        assert_eq!(owner.state(), State::Failed(Failure::Timeout));
        assert!(!adapter.poll_repair(&mut owner));
        assert_eq!(owner.begin(true), None);
    }
}

#[cfg(all(test, feature = "hostsim"))]
mod load_outcome_tests {
    use super::*;

    #[test]
    fn load_outcome_waits_for_its_engine_and_refuses_late_epoch() {
        let _serial = plx_base::testlock::serial();
        let mut ps = crate::route::PlaybackSession::IDLE;
        crate::route::reset_player_control_for_test(&ps);
        let transaction = crate::route::begin_route_start().expect("start transaction");
        assert!(crate::route::prepare_route_start(transaction));
        let attempt = crate::route::claim_route_start_attempt(transaction).expect("physical attempt");
        let mut pa = PlayerAdapter::new(unsafe { MainThread::assume() });

        // A worker may answer before construction finishes, but its route cannot become Stable
        // without an installed Engine. This was the original Load-return race.
        let early = pa.load_receipt(attempt, 7, 0);
        early.deliver(7, true, Some(31));
        pa.drain_load_outcomes(&mut ps);
        assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Pending);

        let mut eng = crate::player::engine::streaming_engine_for_test(8);
        eng.attempt = attempt.contract_id();
        pa.install(eng);
        pa.bind_feed_attempt(attempt, 0);
        let late = pa.load_receipt(attempt, 7, 0);
        late.deliver(7, true, Some(32));
        pa.drain_load_outcomes(&mut ps);
        assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Pending,
            "an older native epoch cannot settle the same route attempt");

        let current = pa.load_receipt(attempt, 8, 0);
        current.deliver(8, true, Some(33));
        pa.drain_load_outcomes(&mut ps);
        assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Started);
        pa.drain_load_outcomes(&mut ps);
        assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Started,
            "one Load result settles at most once");
        drop(pa.take());
        crate::player::SHARED.reset_session();
        crate::player::TX.reset();
        crate::route::reset_player_control_for_test(&ps);
    }

    #[test]
    fn elapsed_fact_keeps_its_captured_trace_after_engine_installation() {
        let _serial = plx_base::testlock::serial();
        let attempt = crate::route::RouteStartAttempt::fixture();
        let mut pa = PlayerAdapter::new(unsafe { MainThread::assume() });
        let mut ps = crate::route::PlaybackSession::IDLE;

        // The worker can return before the Engine is installed. Polling cannot discard that
        // result or report it before the owner can validate the native epoch.
        let early = pa.load_receipt(attempt, 8, 41);
        early.deliver(8, true, Some(123));
        pa.drain_load_outcomes(&mut ps);

        let mut eng = crate::player::engine::streaming_engine_for_test(8);
        eng.attempt = attempt.contract_id();
        pa.install(eng);
        pa.bind_feed_attempt(attempt, 41);
        let old_epoch = pa.load_receipt(attempt, 7, 42);
        old_epoch.deliver(7, false, Some(456));
        let wrong_epoch = pa.load_receipt(attempt, 8, 43);
        wrong_epoch.deliver(9, false, Some(789));
        let wrong_attempt = plx_playback_contract::AttemptId::new(
            attempt.contract_id().owner(), attempt.contract_id().generation() + 1,
        );
        let wrong_owner = pa.feed.load_receipt(wrong_attempt, 8);
        wrong_owner.deliver(8, false, Some(790));
        let cancelled = pa.load_receipt(attempt, 8, 44);
        let cancelled_token = cancelled.token();
        cancelled.deliver(8, false, Some(900));
        pa.cancel_load_receipt(cancelled_token);

        let admitted = pa.feed.take_load_outcomes();
        assert_eq!(admitted.len(), 1, "only the installed Engine's exact attempt and epoch are admitted");
        assert_eq!(admitted[0].attempt, attempt.contract_id());
        assert_eq!(pa.feed_attempt, Some((attempt, 41)), "the common owner retains the captured trace");
        assert_eq!(admitted[0].elapsed_ms, Some(123));
        assert!(admitted[0].ok);
        assert!(pa.feed.take_load_outcomes().is_empty(), "each receipt is consumed once");
        drop(pa.take());
        crate::player::SHARED.reset_session();
        crate::player::TX.reset();
    }
}
