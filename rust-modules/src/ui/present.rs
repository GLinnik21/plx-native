//! The present gate as a MACHINE with an owner (spec §4.4). `Present` is an `App` field with one
//! typed entrance on the main thread — `note(PresentEvent)`, reached through `PresentHandle` — and
//! one per-frame question, `take(tick)`, asked exactly once at §3.3 step 8. Workers get exactly
//! one documented atomic door (`wake_from_worker`, phase 2); nothing else touches the atomics.
//!
//! Phase 2-i: the logical half only. `ui/idle.rs` stays the product's gate until phase 2 swaps
//! this in under it; the two agree on the one behaviour a test can pin — a settled screen stops
//! presenting and the keepalive bounds staleness.
#![allow(dead_code)] // phase 2-i: no consumer until phase 2 (spec §13)

use super::machine::{MachineId, TimerId};

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

pub struct Present {
    dirty: bool,
    motion: bool,
    video_plane: bool,
    fault: Option<Fault>,
    last_present_ms: u32,
    /// The provenance of the first damage since the last take — what the recorder writes.
    why: Option<Provenance>,
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
            video_plane: false,
            fault: None,
            last_present_ms: 0,
            why: None,
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
            PresentEvent::Motion => self.motion = true,
            PresentEvent::VideoPlane(b) => self.video_plane = b,
            PresentEvent::Fault(f) => self.fault = Some(f),
        }
    }

    /// Side-effect-free: what `take` would answer.
    pub fn peek(&self, tick_ms: u32) -> bool {
        self.video_plane
            || self.dirty
            || self.motion
            || tick_ms.wrapping_sub(self.last_present_ms) >= KEEPALIVE_MS
    }

    /// The take-and-clear, once per frame (§3.3 step 8). Answers `true` unconditionally while the
    /// video plane is bound.
    pub fn take(&mut self, tick_ms: u32) -> bool {
        let will = self.peek(tick_ms);
        self.dirty = false;
        self.motion = false;
        self.why = None;
        if will {
            self.last_present_ms = tick_ms;
        }
        will
    }

    pub fn video_plane(&self) -> bool {
        self.video_plane
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
