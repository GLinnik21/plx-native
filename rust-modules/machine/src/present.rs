//! Dispatcher-owned present admission and actual-swap accounting.
//! Admission is observational: pending damage and worker wakes survive a skipped swap.
//! Acknowledgement retires only the generations captured before prepare/draw.


use std::sync::atomic::{AtomicU32, Ordering};

use super::machine::{MachineId, TimerId};

/// The ONE worker-side door (spec §4.4): a poster decode completing, the lab uploader — anything
/// off the main thread that needs the next frame to present. Its generation requests
/// scheduling, never content damage. Admission snapshots it; actual-swap acknowledgement
/// retires only that generation, so a wake racing prepare/draw survives for another frame.
static WAKE: AtomicU32 = AtomicU32::new(0);

/// Wake the gate from a worker thread. Safe to call from any thread, any number of times.
pub fn wake_from_worker() {
    WAKE.fetch_add(1, Ordering::Release);
}

/// Test seam: a wake left by a test must not leak into the next.
#[cfg(test)]
fn clear_worker_wake() {
    WAKE.store(0, Ordering::Release);
}

impl Present {
    /// The product's gate, on the GLOBAL door. `new()` under `cfg(test)` hands out a private door
    /// so parallel tests cannot wake each other's gates; the one test of the door itself asks for
    /// this.
    #[cfg(any(test, feature = "test-support"))]
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
    /// Owned main-thread scheduling demand: no content provenance or page motion.
    Wake,
    /// An INPUT to the gate from `Player.video_plane_bound`; the gate's state has no other writer.
    VideoPlane(bool),
    Fault(Fault),
}

/// The keepalive: a settled screen still presents at least this often (`plx_machine::idle`'s bound).
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
    /// Page motion not yet acknowledged by an actual swap.
    page_motion: bool,
    scope: Scope,
    video_plane: bool,
    fault: Option<Fault>,
    last_present_ms: u32,
    /// First unacknowledged damage provenance — what the recorder writes.
    why: Option<Provenance>,
    /// The worker door this gate reads (`WAKE`, or a private flag in a test).
    door: &'static AtomicU32,
    wake_seen: u32,
    damage_generation: u64,
    motion_generation: u64,
    page_motion_generation: u64,
    page_damage_generation: u64,
    page_damage_acknowledged: u64,
    admitted: Option<(u64, u64, u64, u64, u32)>,
    after_admission: Option<Provenance>,
    enabled: bool,
    was_moving: bool,
    presents: u32,
    last_change_ms: u32,
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
            wake_seen: 0,
            damage_generation: 0,
            motion_generation: 0,
            page_motion_generation: 0,
            page_damage_generation: 0,
            page_damage_acknowledged: 0,
            admitted: None,
            after_admission: None,
            enabled: true,
            was_moving: false,
            presents: 0,
            last_change_ms: 0,
            // a test's gate reads a PRIVATE door: tests run in parallel, and one wake must not
            // present a frame in another test's dispatcher (`global()` is the exception)
            // (`test-support` is how a dependent's tests, which build this crate without
            // `cfg(test)`, get the same private door their gates had before the crate split)
            #[cfg(not(any(test, feature = "test-support")))]
            door: &WAKE,
            #[cfg(any(test, feature = "test-support"))]
            door: Box::leak(Box::new(AtomicU32::new(0))),
        }
    }

    /// Apply one addressed effect without changing attribution of subsequent work.
    pub fn note_in_scope(&mut self, scope: Scope, ev: PresentEvent) {
        let previous = self.scope;
        self.scope = scope;
        self.note(ev);
        self.scope = previous;
    }

    pub fn note(&mut self, ev: PresentEvent) {
        match ev {
            PresentEvent::Damage(p) => self.damage(p, self.scope == Scope::Page),
            PresentEvent::Motion => {
                self.motion_generation = self.motion_generation.wrapping_add(1);
                self.motion = true;
                if self.scope == Scope::Page {
                    self.page_motion_generation = self.page_motion_generation.wrapping_add(1);
                    self.page_motion = true;
                }
            }
            PresentEvent::Wake => {
                // Pending scheduling uses the same admission-protected generation, but
                // never page generations, content provenance or motion continuation.
                self.motion_generation = self.motion_generation.wrapping_add(1);
                self.motion = true;
            }
            PresentEvent::VideoPlane(b) => {
                if self.video_plane != b {
                    self.video_plane = b;
                    self.note(PresentEvent::Damage(Provenance::Lifecycle));
                }
            }
            PresentEvent::Fault(f) => {
                self.fault = Some(f);
                self.note(PresentEvent::Damage(Provenance::Lifecycle));
            }
        }
    }

    fn damage(&mut self, p: Provenance, page: bool) {
        if self.admitted.is_some() && self.after_admission.is_none() {
            self.after_admission = Some(p);
        }
        self.damage_generation = self.damage_generation.wrapping_add(1);
        if page { self.page_damage_generation = self.page_damage_generation.wrapping_add(1); }
        self.dirty = true;
        if self.why.is_none() { self.why = Some(p); }
    }

    /// Typed page damage not yet represented by an acknowledged swap. Surface-only
    /// damage never changes this readout, even when it selects the same frame.
    pub fn page_damaged(&self) -> bool {
        self.page_damage_generation != self.page_damage_acknowledged
    }

    /// Read-only epochs for instrumentation; observations never consume pending damage.
    pub fn damage_epoch(&self) -> u64 { self.damage_generation }
    pub fn page_damage_epoch(&self) -> u64 { self.page_damage_generation }

    /// Side-effect-free: is there unacknowledged CHANGE — damage, motion or a worker's
    /// wake? `peek` without its two unconditional terms (the bound video plane and the keepalive),
    /// which present a frame without anything on it having moved.
    pub fn changed(&self) -> bool {
        self.dirty || self.motion || self.door.load(Ordering::Acquire) != self.wake_seen
    }

    /// Side-effect-free: what `take` would answer (the worker door included, un-consumed).
    pub fn peek(&self, tick_ms: u32) -> bool {
        !self.enabled
            || self.video_plane
            || self.dirty
            || self.motion
            || self.door.load(Ordering::Acquire) != self.wake_seen
            || tick_ms.wrapping_sub(self.last_present_ms) >= KEEPALIVE_MS
    }

    /// Admit before prepare/draw. Does not consume pending work or advance the clock.
    /// Call even for budget-only frames, so their eventual swap has a matching snapshot.
    pub fn take(&mut self, tick_ms: u32) -> bool {
        let will = self.peek(tick_ms);
        self.after_admission = None;
        self.admitted = Some((self.damage_generation, self.motion_generation,
            self.page_motion_generation, self.page_damage_generation, self.door.load(Ordering::Acquire)));
        will
    }

    /// Read-only post-draw validation. A pre-admission scheduling request (for example
    /// a screenshot's own Wake) is not new work; damage/motion/wakes raised by draw are.
    /// No admission snapshot is conservatively busy.
    pub fn changed_after_admission(&self) -> bool {
        self.admitted != Some((self.damage_generation, self.motion_generation,
            self.page_motion_generation, self.page_damage_generation, self.door.load(Ordering::Acquire)))
    }

    /// A real swap completed. Damage and wakes arriving after admission remain pending.
    pub fn acknowledge(&mut self, tick_ms: u32) {
        let Some((damage, motion, page, page_damage, wake)) = self.admitted.take() else { return };
        if damage == self.damage_generation {
            self.dirty = false;
            self.why = None;
        } else {
            self.why = self.after_admission.take();
        }
        if motion == self.motion_generation { self.motion = false; }
        if page == self.page_motion_generation { self.page_motion = false; }
        self.page_damage_acknowledged = page_damage;
        self.wake_seen = wake;
        self.last_present_ms = tick_ms;
        self.presents = self.presents.wrapping_add(1);
    }

    pub fn set_enabled(&mut self, enabled: bool) { self.enabled = enabled; }
    pub fn enabled(&self) -> bool { self.enabled }
    pub fn take_presents(&mut self) -> u32 { std::mem::take(&mut self.presents) }
    pub fn last_change_ms(&self) -> u32 { self.last_change_ms }

    /// Merge only frame-captured motion. Content damage must arrive through a typed
    /// owned handle at its input, effect or result-landing boundary.
    pub fn collect(&mut self, tick_ms: u32, frame: crate::motion::report::FrameReport) {
        if frame.jumped {
            self.note_in_scope(if frame.page_jumped { Scope::Page } else { Scope::Surface }, PresentEvent::Motion);
        }

        self.report_motion(frame.moving, frame.page_moving);
        if self.changed() { self.last_change_ms = tick_ms; }
    }

    /// Merge the explicit frame collector, including the final resting frame.
    pub fn report_motion(&mut self, moving: bool, page: bool) {
        if moving || self.was_moving {
            self.motion_generation = self.motion_generation.wrapping_add(1);
            self.motion = true;
            if page {
                self.page_motion_generation = self.page_motion_generation.wrapping_add(1);
                self.page_motion = true;
            }
        }
        self.was_moving = moving;
    }

    pub fn video_plane(&self) -> bool {
        self.video_plane
    }

    /// Open a motion scope: what `Motion` reported from here on is attributed to.
    pub fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
    }

    /// Unacknowledged PAGE motion (the host-snapshot question), independent of surface motion.
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
    fn swap(p: &mut super::Present, tick: u32) -> bool {
      let admitted = p.take(tick);
      if admitted { p.acknowledge(tick); }
      admitted
    }
    #[test]
    fn skipped_swap_keeps_damage_pending() {
        use super::{Present, PresentEvent, Provenance};

        let mut present = Present::new();
        assert!(swap(&mut present, 0));
        present.note(PresentEvent::Damage(Provenance::Input));
        assert!(present.take(1), "admission should request a frame");
        // A suspended surface or unavailable backbuffer can prevent the swap after admission.
        assert!(present.peek(2), "damage must survive an unpresented frame");
        assert_eq!(present.why(), Some(Provenance::Input));
    }

    #[test]
    fn skipped_swap_does_not_advance_keepalive_clock() {
        let mut present = super::Present::new();
        assert!(swap(&mut present, 0));
        present.note(super::PresentEvent::Damage(super::Provenance::Input));
        assert!(present.take(100), "admission should request a frame");
        assert!(present.peek(super::KEEPALIVE_MS), "a skipped swap must not advance the present clock");
    }

    #[test]
    fn skipped_swap_keeps_worker_wake_pending() {
        use std::sync::atomic::Ordering;

        let mut present = super::Present::new();
        assert!(swap(&mut present, 0), "the initial frame is presented");
        present.door.fetch_add(1, Ordering::Release);
        assert!(present.take(1), "the worker wake admits a frame");
        assert!(present.peek(2), "a skipped swap must not consume the wake");
    }

    /// Spec §15.1: a worker reaches the gate through `wake_from_worker` and nothing else; the
    /// main thread's `take` consumes the wake exactly once.
    #[test]
    fn a_worker_wakes_the_present_gate_through_the_one_door() {
        let _g = plx_base::testlock::serial();
        super::clear_worker_wake();
        let mut p = super::Present::global();
        assert!(swap(&mut p, 0), "the first frame always draws");
        assert!(!swap(&mut p, 1), "settled, inside the keepalive: nothing to present");
        std::thread::spawn(super::wake_from_worker).join().unwrap();
        assert!(p.peek(2), "the door is visible to peek");
        assert!(swap(&mut p, 2), "…and folded into the frame's verdict");
        assert!(!swap(&mut p, 3), "consumed exactly once");
        // a second wake is a second frame, not a lost one
        super::wake_from_worker();
        assert!(swap(&mut p, 4));
        assert!(!swap(&mut p, 5));
    }

    use super::*;

    #[test]
    fn owned_handles_keep_page_and_surface_damage_independent_through_swap() {
        use crate::machine::PresentHandle;
        let mut a = Present::new();
        let mut b = Present::new();
        assert!(swap(&mut a, 0));
        assert!(swap(&mut b, 0));
        a.set_scope(Scope::Surface);
        PresentHandle::of(&mut a).note(PresentEvent::Damage(Provenance::Input));
        assert!(a.changed());
        assert!(!a.page_damaged());
        assert!(!b.changed());
        assert!(a.take(16));
        a.set_scope(Scope::Page);
        PresentHandle::of(&mut a).note(PresentEvent::Damage(Provenance::Resource(ResourceKind::Texture)));
        a.acknowledge(16);
        assert!(a.page_damaged(), "draw-phase damage outlives acknowledgement of admission");
        assert_eq!(a.why(), Some(Provenance::Resource(ResourceKind::Texture)));
        assert!(!b.page_damaged());
        assert!(!b.changed());
        assert!(swap(&mut a, 32));
        assert!(!a.changed());
        assert!(!a.page_damaged());
    }

    #[test]
    fn an_owned_wake_is_isolated_and_survives_a_late_draw_without_ui_provenance() {
        let mut a = Present::new();
        let mut b = Present::new();
        assert!(swap(&mut a, 0));
        assert!(swap(&mut b, 0));
        a.note(PresentEvent::Wake);
        assert!(a.take(16));
        assert!(!a.changed_after_admission(), "the owner's pre-admission wake is not late draw work");
        assert!(!a.page_damaged());
        assert!(!a.page_moving());
        assert_eq!(a.why(), None);
        assert!(!b.changed());
        a.note(PresentEvent::Wake);
        assert!(a.changed_after_admission(), "late owned scheduling is observable before ack");
        a.acknowledge(16);
        assert!(a.changed(), "a scheduling request during draw survives actual swap");
        assert!(!a.page_moving());
        assert!(swap(&mut a, 32));
        assert!(!a.changed());
        assert!(!b.changed());
    }

    #[test]
    fn independent_gates_keep_damage_and_motion_separate() {
        let mut a = Present::new();
        let mut b = Present::new();
        assert!(swap(&mut a, 0));
        assert!(swap(&mut b, 0));
        a.note(PresentEvent::Damage(Provenance::Input));
        assert!(a.peek(1));
        assert!(!b.peek(1));
        a.set_scope(Scope::Surface);
        a.note(PresentEvent::Motion);
        assert!(!a.page_moving());
        a.set_scope(Scope::Page);
        a.note(PresentEvent::Motion);
        assert!(a.page_moving());
        assert!(!b.changed());
    }

    #[test]
    fn skipped_admission_preserves_damage_and_actual_present_clock() {
        let mut p = Present::new();
        assert!(swap(&mut p, 0));
        p.note(PresentEvent::Damage(Provenance::Input));
        assert!(p.peek(100), "admit a frame that cannot swap yet");
        assert!(p.changed(), "admission must not consume pending damage");
        assert_eq!(p.why(), Some(Provenance::Input));
        assert!(p.peek(200), "the skipped frame remains pending");
        assert!(swap(&mut p, 200), "the eventual swap consumes the damage");
        assert!(!p.peek(KEEPALIVE_MS), "keepalive starts at the actual swap");
        assert!(p.peek(200 + KEEPALIVE_MS));
    }

    #[test]
    fn acknowledgement_preserves_damage_and_wake_raised_after_admission() {
        let mut p = Present::new();
        assert!(swap(&mut p, 0));
        p.note(PresentEvent::Damage(Provenance::Input));
        assert!(p.take(100));
        p.note(PresentEvent::Damage(Provenance::Nav));
        p.door.fetch_add(1, Ordering::Release);
        p.acknowledge(100);
        assert!(p.changed());
        assert_eq!(p.why(), Some(Provenance::Nav));
        assert!(swap(&mut p, 101));
        assert!(!p.changed());
        assert!(!p.peek(KEEPALIVE_MS));
        assert!(p.peek(101 + KEEPALIVE_MS));
    }

    #[test]
    fn a_settled_gate_stops_presenting_and_the_keepalive_bounds_staleness() {
        let mut p = Present::new();
        assert!(swap(&mut p, 0), "the first frame draws");
        assert!(!swap(&mut p, 16), "nothing happened: no present");
        p.note(PresentEvent::Damage(Provenance::Input));
        assert_eq!(p.why(), Some(Provenance::Input));
        assert!(swap(&mut p, 32));
        assert!(!swap(&mut p, 48));
        assert!(swap(&mut p, 48 + KEEPALIVE_MS), "the keepalive");
        p.note(PresentEvent::VideoPlane(true));
        assert!(p.take(3000) && p.take(3016), "bound plane: every frame");
    }
}
