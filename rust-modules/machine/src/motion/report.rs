//! Per-thread frame motion collector, not a present gate.
//! Rendering scopes report page/surface/decorative motion here; the runtime explicitly
//! reads the report and merges it into its owned `Present` before admission.
use std::cell::Cell;

pub const IDLE_POLL_MS: u32 = 16;

thread_local! {
    static MOVING: Cell<bool> = const { Cell::new(false) };
    static PAGE_MOVING: Cell<bool> = const { Cell::new(false) };
    static PAGE_LAYOUT_MOVING: Cell<bool> = const { Cell::new(false) };
    static JUMPED: Cell<bool> = const { Cell::new(false) };
    static PAGE_JUMPED: Cell<bool> = const { Cell::new(false) };

    static DEPTH: Cell<u32> = const { Cell::new(0) };
    static DECOR: Cell<u32> = const { Cell::new(0) };

    static DT: Cell<f32> = const { Cell::new(1.0 / 60.0) };
    static CLOCK_US: Cell<u64> = const { Cell::new(0) };
    #[cfg(any(test, feature = "test-support"))]
    static LOCAL_MOTION: Cell<u32> = const { Cell::new(0) };

    static FROZEN: Cell<bool> = const { Cell::new(false) };
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameReport {
    pub moving: bool,
    pub page_moving: bool,
    pub page_layout_moving: bool,
    /// A discontinuity already at rest; unlike running motion it owes no settle frame.
    pub jumped: bool,
    pub page_jumped: bool,

}
pub fn read() -> FrameReport {
    FrameReport { moving: present_moving(), page_moving: page_moving(),
        page_layout_moving: page_layout_moving(), jumped: JUMPED.get(), page_jumped: PAGE_JUMPED.get() }
}
/// Main-thread runner boundary for motion only. Content damage and its provenance
/// belong to the addressed Present, never to this frame collector.
pub fn collect_into(present: &mut crate::present::Present, tick_ms: u32) {
    present.collect(tick_ms, read());
}
pub fn frame_begin(dt: f32) { frame_begin_judging_rest_with(dt, dt); }
pub fn frame_begin_judging_rest_with(dt: f32, rest_dt: f32) {
    MOVING.set(false);
    PAGE_MOVING.set(false);
    PAGE_LAYOUT_MOVING.set(false);
    JUMPED.set(false);
    PAGE_JUMPED.set(false);

    DT.set(rest_dt);
    CLOCK_US.set(CLOCK_US.get().wrapping_add((dt * 1_000_000.0).round().max(0.0) as u64));
}
pub fn now_ms() -> u32 { (CLOCK_US.get() / 1000) as u32 }
pub fn settled(pos: f32, target: f32, vel: f32) -> bool {
    let dt = DT.get();
    // Invalid arithmetic must never make a visibly broken animation look quiet.
    pos.is_finite() && target.is_finite() && vel.is_finite() && dt.is_finite()
        && !super::moving(pos, target, vel, dt)
}
pub fn note_spring(pos: f32, target: f32, vel: f32) {
    if !settled(pos, target, vel) { merge_motion(); }
}
pub fn note_motion() {
    if page_frozen() { return; }
    #[cfg(any(test, feature = "test-support"))]
    LOCAL_MOTION.set(LOCAL_MOTION.get().wrapping_add(1));
    merge_motion();
}
/// A frame of a paused animation still needs its clock sampled, but no pixel is moving.
/// Unlike the worker door this request belongs only to this captured frame report.
pub fn note_wait() {
    // The animation's clock is running even while its pixels hold. This is a
    // frame-captured animation report, not a pending wake mailbox or content damage.
    if !page_frozen() { MOVING.set(true); }
}
fn merge_motion() {
    if page_frozen() { return; }
    MOVING.set(true);
    if DEPTH.get() == 0 {
        PAGE_MOVING.set(true);
        if DECOR.get() == 0 { PAGE_LAYOUT_MOVING.set(true); }
    }
}
pub fn present_moving() -> bool { MOVING.get() }
pub fn page_moving() -> bool { PAGE_MOVING.get() }
pub fn page_layout_moving() -> bool { PAGE_LAYOUT_MOVING.get() }
pub fn decorative<T>(f: impl FnOnce() -> T) -> T {
    struct Guard;
    impl Drop for Guard { fn drop(&mut self) { DECOR.set(DECOR.get().saturating_sub(1)); } }
    DECOR.set(DECOR.get() + 1);
    let _guard = Guard;
    f()
}
pub struct MotionScope { before: bool, open: bool }
impl MotionScope {
    pub fn open() -> Self {
        DEPTH.set(DEPTH.get() + 1);
        Self { before: MOVING.replace(false), open: true }
    }
    fn merge(&mut self) -> bool {
        if !self.open { return false; }
        self.open = false;
        DEPTH.set(DEPTH.get().saturating_sub(1));
        let moving = MOVING.get();
        MOVING.set(self.before || moving);
        moving
    }
    pub fn close(mut self) -> bool { self.merge() }
}
impl Drop for MotionScope { fn drop(&mut self) { self.merge(); } }
pub fn scoped_motion<T>(f: impl FnOnce() -> T) -> (T, bool) {
    let scope = MotionScope::open();
    let result = f();
    (result, scope.close())
}
pub fn set_page_frozen(on: bool) -> bool { FROZEN.replace(on) }
pub fn page_frozen() -> bool { FROZEN.get() }
/// A discontinuous spring move is motion too; this is not content damage.
pub fn note_jump(changed: bool) {
    if !changed || page_frozen() { return; }
    #[cfg(any(test, feature = "test-support"))]
    LOCAL_MOTION.set(LOCAL_MOTION.get().wrapping_add(1));
    JUMPED.set(true);
    if DEPTH.get() == 0 { PAGE_JUMPED.set(true); }
}

#[cfg(any(test, feature = "test-support"))]
pub fn take_local_motion() -> u32 { LOCAL_MOTION.replace(0) }
#[cfg(any(test, feature = "test-support"))]
pub fn reset_for_test() {
    frame_begin(1.0 / 60.0);
    FROZEN.set(false);
    LOCAL_MOTION.set(0);
}
