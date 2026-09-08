//! The present gate as a MACHINE with an owner (spec §4.4). `Present` is an `App` field with one
//! typed entrance on the main thread — `note(PresentEvent)`, reached through `PresentHandle` — and
//! one per-frame question, `take(tick)`, asked exactly once at §3.3 step 8. Workers get exactly
//! one documented atomic door (`wake_from_worker`, phase 2); nothing else touches the atomics.
//!
//! Phase 2-i: the logical half only. `ui/idle.rs` stays the product's gate until phase 2 swaps
//! this in under it; the two agree on the one behaviour a test can pin — a settled screen stops
//! presenting and the keepalive bounds staleness.
#![allow(dead_code)] // phase 2-i: no consumer until phase 2 (spec §13)

use std::sync::atomic::{AtomicBool, Ordering};

use super::machine::{MachineId, TimerId};

/// The ONE worker-side door (spec §4.4): a poster decode completing, the lab uploader — anything
/// off the main thread that needs the next frame to present. It is a flag, not an event: the
/// main thread's `take` folds it into the frame's verdict as `Damage` with no provenance of its
/// own, because a worker cannot say which machine it woke on behalf of. Nothing else touches
/// the atomic (`ci/check-deps.sh` gates `present::` atomics to this file).
static WAKE: AtomicBool = AtomicBool::new(false);

/// Wake the gate from a worker thread. Safe to call from any thread, any number of times.
pub fn wake_from_worker() {
    WAKE.store(true, Ordering::Release);
}

/// Test seam: a wake left by a test must not leak into the next.
#[cfg(test)]
fn clear_worker_wake() {
    WAKE.store(false, Ordering::Release);
}

impl Present {
    /// The product's gate, on the GLOBAL door. `new()` under `cfg(test)` hands out a private door
    /// so parallel tests cannot wake each other's gates; the one test of the door itself asks for
    /// this.
    #[cfg(test)]
    pub fn global() -> Self {
        let mut p = Self::new();
        p.door = &WAKE;
        p
    }
}

/// Why a frame presents — recorded, so a replay diff can say WHY (§4.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provenance {
    Landing(MachineId),
    Resource(ResourceKind),
    Timer(TimerId),
    Input,
    Lifecycle,
    Nav,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResourceKind {
    Texture,
    Text,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
    Measure,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PresentEvent {
    Damage(Provenance),
    Motion,
    /// An INPUT to the gate from `Player.video_plane_bound`; the gate's state has no other writer.
    VideoPlane(bool),
    Fault(Fault),
}

/// The keepalive: a settled screen still presents at least this often (`ui::idle`'s bound).
pub const KEEPALIVE_MS: u32 = 2000;

/// Whose springs are reporting (§4.4 `MotionScope`, structural): the dispatcher sets the scope
/// around each body's step, so a surface's foreground motion never counts as the host page
/// moving — the host snapshot is re-taken only for PAGE motion.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Scope {
    #[default]
    Page,
    Surface,
}

pub struct Present {
    dirty: bool,
    motion: bool,
    /// Motion reported under `Scope::Page` since the last take.
    page_motion: bool,
    scope: Scope,
    video_plane: bool,
    fault: Option<Fault>,
    last_present_ms: u32,
    /// The provenance of the first damage since the last take — what the recorder writes.
    why: Option<Provenance>,
    /// The worker door this gate reads (`WAKE`, or a private flag in a test).
    door: &'static AtomicBool,
}

impl Default for Present {
    fn default() -> Self {
        Self::new()
    }
}

impl Present {
    pub fn new() -> Self {
        Self {
            dirty: true, // the first frame always draws
            motion: false,
            page_motion: false,
            scope: Scope::Page,
            video_plane: false,
            fault: None,
            last_present_ms: 0,
            why: None,
            // a test's gate reads a PRIVATE door: tests run in parallel, and one wake must not
            // present a frame in another test's dispatcher (`global()` is the exception)
            #[cfg(not(test))]
            door: &WAKE,
            #[cfg(test)]
            door: Box::leak(Box::new(AtomicBool::new(false))),
        }
    }

    pub fn note(&mut self, ev: PresentEvent) {
        match ev {
            PresentEvent::Damage(p) => {
                self.dirty = true;
                if self.why.is_none() {
                    self.why = Some(p);
                }
            }
            PresentEvent::Motion => {
                self.motion = true;
                if self.scope == Scope::Page {
                    self.page_motion = true;
                }
            }
            PresentEvent::VideoPlane(b) => self.video_plane = b,
            PresentEvent::Fault(f) => self.fault = Some(f),
        }
    }

    /// Side-effect-free: what `take` would answer (the worker door included, un-consumed).
    pub fn peek(&self, tick_ms: u32) -> bool {
        self.video_plane
            || self.dirty
            || self.motion
            || self.door.load(Ordering::Acquire)
            || tick_ms.wrapping_sub(self.last_present_ms) >= KEEPALIVE_MS
    }

    /// The take-and-clear, once per frame (§3.3 step 8). Answers `true` unconditionally while the
    /// video plane is bound. Consumes the worker door.
    pub fn take(&mut self, tick_ms: u32) -> bool {
        let will = self.peek(tick_ms);
        self.door.swap(false, Ordering::AcqRel);
        self.dirty = false;
        self.motion = false;
        self.page_motion = false;
        self.why = None;
        if will {
            self.last_present_ms = tick_ms;
        }
        will
    }

    pub fn video_plane(&self) -> bool {
        self.video_plane
    }

    /// Open a motion scope: what `Motion` reported from here on is attributed to.
    pub fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
    }

    /// Did the PAGE move since the last take (the host-snapshot question)? Cleared by `take`.
    pub fn page_moving(&self) -> bool {
        self.page_motion
    }

    /// The fault the tail logs once, if any, and clears.
    pub fn take_fault(&mut self) -> Option<Fault> {
        self.fault.take()
    }

    pub fn why(&self) -> Option<Provenance> {
        self.why
    }
}

#[cfg(test)]
mod tests {
    /// Spec §15.1: a worker reaches the gate through `wake_from_worker` and nothing else; the
    /// main thread's `take` consumes the wake exactly once.
    #[test]
    fn a_worker_wakes_the_present_gate_through_the_one_door() {
        let _g = crate::testlock::serial();
        super::clear_worker_wake();
        let mut p = super::Present::global();
        assert!(p.take(0), "the first frame always draws");
        assert!(!p.take(1), "settled, inside the keepalive: nothing to present");
        std::thread::spawn(super::wake_from_worker).join().unwrap();
        assert!(p.peek(2), "the door is visible to peek");
        assert!(p.take(2), "…and folded into the frame's verdict");
        assert!(!p.take(3), "consumed exactly once");
        // a second wake is a second frame, not a lost one
        super::wake_from_worker();
        assert!(p.take(4));
        assert!(!p.take(5));
    }

    use super::*;

    #[test]
    fn a_settled_gate_stops_presenting_and_the_keepalive_bounds_staleness() {
        let mut p = Present::new();
        assert!(p.take(0), "the first frame draws");
        assert!(!p.take(16), "nothing happened: no present");
        p.note(PresentEvent::Damage(Provenance::Input));
        assert_eq!(p.why(), Some(Provenance::Input));
        assert!(p.take(32));
        assert!(!p.take(48));
        assert!(p.take(48 + KEEPALIVE_MS), "the keepalive");
        p.note(PresentEvent::VideoPlane(true));
        assert!(p.take(3000) && p.take(3016), "bound plane: every frame");
    }
}
