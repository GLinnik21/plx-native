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
}

impl PlayerAdapter {
    /// Take the token. `app::boot` calls this once, with the token `plex_run` minted.
    pub(crate) fn new(mt: MainThread) -> Self {
        Self {
            mt,
            engine: None,
        }
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
