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
//! live `&mut` to the engine no longer requires a convention: it does not compile. The current
//! `engine()`/`split()` accessors remain an intermediate bridge for the existing feed pump.
//!
//! The rest of `player/engine.rs` keeps its `mt: &MainThread` parameters unchanged. They gate a
//! DIFFERENT thing — the ACB/Starfish seam (`tv::sink::VideoSink`'s methods, whose bind order is a
//! sequence of calls with no locking behind it) — and that surface is not this module's.

use super::engine::Engine;
use super::{feed_resources::FeedResources, threads, timeline};
use plx_base::task::MainThread;

/// Common coordinator and main-thread proof, as one owned value (`App.adapters.player`).
pub struct PlayerAdapter {
    /// Proof, held rather than passed: see the module doc. It is a ZST, so this costs nothing and
    /// is what makes `&mut PlayerAdapter` mean "the main thread, exclusively".
    mt: MainThread,
    /// The native feed slot and its Load/release worker resources.
    feed: FeedResources,
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

    /// The live session, borrowed mutably.
    #[inline]
    pub fn engine(&mut self) -> Option<&mut Engine> {
        self.feed.engine()
    }

    /// Is a session live? Distinct from [`engine`](Self::engine) because it answers without taking
    /// the exclusive borrow — `start_bufferfeed`'s double-start guard only needs to ask.
    #[inline]
    pub fn is_live(&self) -> bool {
        self.feed.is_live()
    }

    /// Address one Load verdict without handing route state to the native worker.
    pub(super) fn load_receipt(&mut self, attempt: crate::route::RouteStartAttempt, native_epoch: u32,
        trace_generation: u32) -> threads::LoadReceipt {
        self.feed.load_receipt(attempt, native_epoch, trace_generation)
    }

    /// A refused thread spawn cannot return a receipt; retire its mapping immediately.
    pub(super) fn cancel_load_receipt(&mut self, token: u64) { self.feed.cancel_load_receipt(token); }

    /// Teardown retires the whole physical attempt before a still-blocked Load can return.
    pub(super) fn retire_load_attempt(&mut self, attempt: crate::route::RouteStartAttempt) {
        self.feed.retire_load_attempt(attempt);
    }

    /// The main-thread coordinator's only Load-result landing. Late outcomes from an abandoned
    /// or replaced Engine cannot settle a successor's route, even if a worker returns afterward.
    pub(super) fn drain_load_outcomes(&mut self, ps: &mut crate::route::PlaybackSession) {
        for (attempt, ok) in self.feed.take_load_outcomes() {
            let result = if ok { crate::route::RouteStartResult::Started }
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
    pub fn install(&mut self, e: Engine) {
        self.feed.install(e);
    }

    /// Take the session out of the slot; the caller then joins its workers and drops it.
    #[inline]
    pub fn take(&mut self) -> Option<Engine> {
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

    /// The session AND the token at once, borrowed from disjoint fields.
    ///
    /// `pump` needs both for the whole of its body — it reads the live `Engine` and issues
    /// `sf_pause`/`sf_flush` against the same session — and two separate accessor calls cannot
    /// give it that, `engine()` taking `&mut self` and [`mt`](Self::mt) `&self`. One `&mut self`
    /// split into two field borrows can, and the borrow checker still refuses to let either
    /// outlive a `reload_*` that replaces the slot: that is the `eng` dangles rule of `pump`,
    /// which was a COMMENT for as long as the slot handed out `&'static mut`.
    #[inline]
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
        early.deliver(7, true);
        pa.drain_load_outcomes(&mut ps);
        assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Pending);

        let mut eng = crate::player::engine::streaming_engine_for_test(8);
        eng.route_start = attempt;
        pa.install(eng);
        let late = pa.load_receipt(attempt, 7, 0);
        late.deliver(7, true);
        pa.drain_load_outcomes(&mut ps);
        assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Pending,
            "an older native epoch cannot settle the same route attempt");

        let current = pa.load_receipt(attempt, 8, 0);
        current.deliver(8, true);
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
}
