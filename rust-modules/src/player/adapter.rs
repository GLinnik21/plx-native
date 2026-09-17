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
//! DIFFERENT thing — the ACB/Starfish seam (`player::ffi`'s wrappers, whose bind order is a
//! sequence of calls with no locking behind it) — and that surface is not this module's.

use super::engine::Engine;
use crate::task::MainThread;

/// The `ENGINE` slot and its confinement, as one owned value (`App.adapters.player`).
pub(crate) struct PlayerAdapter {
    /// Proof, held rather than passed: see the module doc. It is a ZST, so this costs nothing and
    /// is what makes `&mut PlayerAdapter` mean "the main thread, exclusively".
    mt: MainThread,
    /// The live native session, or `None` between playbacks.
    engine: Option<Engine>,
    repair: Option<(u64, std::sync::mpsc::Receiver<Result<(), crate::webos::jail_repair::Failure>>)>,
    /// A timed-out native `Load` whose media thread had not returned when its Engine was torn
    /// down. Owned here, not by a static, for the same reason the Engine is: releasing it calls
    /// the Starfish seam, which only the main thread may do. See `engine::AbandonedLoad`.
    abandoned_load: Option<super::engine::AbandonedLoad>,
}

impl PlayerAdapter {
    /// Take the token. `app::boot` calls this once, with the token `plex_run` minted.
    pub(crate) fn new(mt: MainThread) -> Self {
        Self {
            mt,
            engine: None,
            repair: None,
            abandoned_load: None,
        }
    }

    /// UI-thread resource effect. The owner spends the attempt before the worker can run.
    pub(crate) fn repair_sandbox(&mut self, owner: &mut super::machine::RepairAttempt, supported: bool) {
        let Some(token) = owner.begin(supported) else { return; };
        let (tx, rx) = std::sync::mpsc::channel();
        if crate::task::spawn_small("jail repair", move || {
            let _ = tx.send(crate::webos::jail_repair::execute());
        }) {
            self.repair = Some((token, rx));
        } else {
            owner.complete(token, Err(crate::webos::jail_repair::Failure::StartFailed));
        }
    }

    /// Poll on every app frame, including while the player page is absent.
    pub(crate) fn poll_repair(&mut self, owner: &mut super::machine::RepairAttempt) -> bool {
        let Some((token, rx)) = self.repair.as_ref() else { return false; };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(crate::webos::jail_repair::Failure::StartFailed),
        };
        let changed = owner.complete(*token, result);
        self.repair = None;
        changed
    }

    /// The live session, borrowed mutably.
    #[inline]
    pub(crate) fn engine(&mut self) -> Option<&mut Engine> {
        self.engine.as_mut()
    }

    /// Is a session live? Distinct from [`engine`](Self::engine) because it answers without taking
    /// the exclusive borrow — `start_bufferfeed`'s double-start guard only needs to ask.
    #[inline]
    pub(crate) fn is_live(&self) -> bool {
        self.engine.is_some()
    }

    /// Install the freshly-built session. Overwriting a live slot would DROP an Engine whose
    /// workers hold raw pointers into the boxes it owns; `start_bufferfeed` guards on
    /// [`is_live`](Self::is_live) first.
    #[inline]
    pub(crate) fn install(&mut self, e: Engine) {
        self.engine = Some(e);
    }

    /// Take the session out of the slot; the caller then joins its workers and drops it.
    #[inline]
    pub(crate) fn take(&mut self) -> Option<Engine> {
        self.engine.take()
    }

    /// Park a still-in-flight Load for a later main-thread release. At most one can exist: the
    /// C seam owns one object at a time and every start is refused while this slot is full.
    pub(crate) fn park_abandoned_load(&mut self, load: super::engine::AbandonedLoad) {
        debug_assert!(self.abandoned_load.is_none(), "a second abandoned Load cannot exist");
        self.abandoned_load = Some(load);
    }

    /// Is a native object still parked behind a Load that has not been released?
    pub(crate) fn has_abandoned_load(&self) -> bool {
        self.abandoned_load.is_some()
    }

    /// The parked Load, borrowed mutably (its once-only log latch), or taken once it returned.
    pub(crate) fn abandoned_load_mut(&mut self) -> Option<&mut super::engine::AbandonedLoad> {
        self.abandoned_load.as_mut()
    }

    pub(crate) fn take_abandoned_load(&mut self) -> Option<super::engine::AbandonedLoad> {
        self.abandoned_load.take()
    }

    /// The token, for the ACB/Starfish seam. `player::ffi`'s wrappers still take one — see the
    /// module doc for why that surface keeps its own argument.
    #[inline]
    pub(crate) fn mt(&self) -> &MainThread {
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
    pub(crate) fn split(&mut self) -> (Option<&mut Engine>, &MainThread) {
        (self.engine.as_mut(), &self.mt)
    }
}

#[cfg(test)]
mod repair_receipt_tests {
    use super::*;
    use crate::webos::jail_repair::{Failure, State};
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
