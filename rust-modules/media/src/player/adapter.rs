//! **The Player ADAPTER** — the native session object and the main-thread token that confines it
//! (restructure spec §2.2, phase 9).
//!
//! An adapter holds OS/FFI resources and never logical state; the decisions live one field away in
//! [`crate::player::machine::Player`]. What this one holds is the [`Engine`] — the Starfish/ACB
//! session, its HTTP stream box, its AU queues and the handles of the three worker threads that
//! read raw pointers into all three — together with the [`MainThread`] token.
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
//! live `&mut` to the engine no longer requires a convention: it does not compile.
//!
//! The rest of `player/engine.rs` keeps its `mt: &MainThread` parameters unchanged. They gate a
//! DIFFERENT thing — the ACB/Starfish seam (`tv::sink::VideoSink`'s methods, whose bind order is a
//! sequence of calls with no locking behind it) — and that surface is not this module's.

use super::engine::Engine;
use plx_base::task::MainThread;

/// The `ENGINE` slot and its confinement, as one owned value (`App.adapters.player`).
pub struct PlayerAdapter {
    /// Proof, held rather than passed: see the module doc. It is a ZST, so this costs nothing and
    /// is what makes `&mut PlayerAdapter` mean "the main thread, exclusively".
    mt: MainThread,
    /// The live native session, or `None` between playbacks.
    engine: Option<Engine>,
    repair: Option<(u64, std::sync::mpsc::Receiver<Result<(), plx_platform::tv::sandbox::Failure>>)>,
    /// A timed-out native `Load` whose media thread had not returned when its Engine was torn
    /// down. Owned here, not by a static, for the same reason the Engine is: releasing it calls
    /// the Starfish seam, which only the main thread may do. See `engine::AbandonedLoad`.
    abandoned_load: Option<super::engine::AbandonedLoad>,
    /// A real stop's native object whose `Unload` is still running on its worker
    /// (`engine::PendingRelease`): the worker's handle and the Engine it keeps alive. What may
    /// happen to it is decided by [`release`](Self::release), the pure machine this holds beside it.
    /// **The fence the off-frame release relies on is `player::sink()`:** while the object is
    /// `Releasing` the main thread's verbs reach the `RefusingSink` fence (`player::FENCE`), so [`mt`](Self::mt) and
    /// [`split`](Self::split) hand out the token without waiting for the worker (PLX-NATIVE-1Q: that
    /// wait had no bound). A `RefCell` because `take_finished_release` takes `&self`; the adapter is
    /// `!Send` (it holds the token), so the cell never crosses a thread.
    pending_release: std::cell::RefCell<Option<super::engine::PendingRelease>>,
    release: super::release::NativeRelease,
    /// What became of the start the FOREGROUND restore queued behind the release
    /// ([`watch_queued_start`](Self::watch_queued_start)); the lifecycle machine polls it.
    queued_watch: QueuedWatch,
    /// A hero PREVIEW's start was queued behind the release: whether its slot counts as admitted
    /// once the Load actually launches (`None`: no preview start is queued). Admission is taken
    /// when the queued start runs, never when it is merely queued.
    preview_admission: Option<bool>,
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
            engine: None,
            repair: None,
            abandoned_load: None,
            pending_release: std::cell::RefCell::new(None),
            release: super::release::NativeRelease::new(),
            queued_watch: QueuedWatch::Unwatched,
            preview_admission: None,
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
        {
            let mut slot = self.pending_release.borrow_mut();
            debug_assert!(slot.is_none(), "a second off-frame release cannot exist");
            *slot = Some(release);
        }
        self.release_step(&super::release::ReleaseEvent::UnloadHandedOff { epoch, at }, &mut Vec::new());
    }

    /// Is a stopped native object's `Unload` still owed its main-thread tail?
    pub fn has_pending_release(&self) -> bool {
        self.pending_release.borrow().is_some()
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
        let mut slot = self.pending_release.borrow_mut();
        if slot.as_ref().is_some_and(|p| p.is_finished()) {
            slot.take()
        } else {
            None
        }
    }

    /// The parked release whether or not its worker returned: only the exit, which leaks it
    /// (`engine::leak_parked_for_exit`), takes it that way.
    pub(super) fn take_pending_release_any(&self) -> Option<super::engine::PendingRelease> {
        self.pending_release.borrow_mut().take()
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
        self.engine.as_mut()
    }

    /// Is a session live? Distinct from [`engine`](Self::engine) because it answers without taking
    /// the exclusive borrow — `start_bufferfeed`'s double-start guard only needs to ask.
    #[inline]
    pub fn is_live(&self) -> bool {
        self.engine.is_some()
    }

    /// Install the freshly-built session. Overwriting a live slot would DROP an Engine whose
    /// workers hold raw pointers into the boxes it owns; `start_bufferfeed` guards on
    /// [`is_live`](Self::is_live) first.
    #[inline]
    pub fn install(&mut self, e: Engine) {
        self.engine = Some(e);
    }

    /// Take the session out of the slot; the caller then joins its workers and drops it.
    #[inline]
    pub fn take(&mut self) -> Option<Engine> {
        self.engine.take()
    }

    /// Park a still-in-flight Load for a later main-thread release. At most one can exist: the
    /// C seam owns one object at a time and every start is refused while this slot is full.
    pub fn park_abandoned_load(&mut self, load: super::engine::AbandonedLoad) {
        debug_assert!(self.abandoned_load.is_none(), "a second abandoned Load cannot exist");
        self.abandoned_load = Some(load);
    }

    /// Is a native object still parked behind a Load that has not been released?
    pub fn has_abandoned_load(&self) -> bool {
        self.abandoned_load.is_some()
    }

    /// The parked Load, borrowed mutably (its once-only log latch), or taken once it returned.
    pub fn abandoned_load_mut(&mut self) -> Option<&mut super::engine::AbandonedLoad> {
        self.abandoned_load.as_mut()
    }

    pub fn take_abandoned_load(&mut self) -> Option<super::engine::AbandonedLoad> {
        self.abandoned_load.take()
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
        (self.engine.as_mut(), &self.mt)
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
