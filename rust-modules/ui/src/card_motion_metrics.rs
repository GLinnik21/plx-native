//! Opt-in counters for the finite poster-gate device scenes. Counts actual source
//! admissions, actual resident draws and actual GL uploads; never infers art from FPS.
use std::cell::RefCell;

#[derive(Clone, Copy, Default)]
pub struct Stats {
    pub frames: u64,
    pub draws: u64,
    pub ready: u64,
    pub moving: u64,
    pub moving_frames: u64,
    pub moving_first_ms: u32,
    pub moving_last_ms: u32,
    frame_observed: bool,
    frame_moving: bool,
    pub unknown: u64,
    pub requested: u64,
    pub requested_moving: u64,
    pub refused_new: u64,
    pub refused_evicted: u64,
    pub refused_retry: u64,
    pub rearmed: u64,
    pub uploads: u64,
    pub lost: u64,
    pub last_draws: u64,
    pub last_ready: u64,
    /// Presented frames (a subset of `frames`) that drew at least one card with no texture.
    pub ph_frames: u64,
}
impl Stats {
    pub fn full(self) -> bool { !self.frame_observed && self.last_draws >= 6 && self.last_ready == self.last_draws }
}
thread_local! { static ACTIVE: RefCell<Option<Stats>> = const { RefCell::new(None) }; }
fn note(f: impl FnOnce(&mut Stats)) { ACTIVE.with(|s| { if let Some(s) = &mut *s.borrow_mut() { f(s); } }); }
pub fn arm() { ACTIVE.with(|s| *s.borrow_mut() = Some(Stats::default())); }
pub fn snapshot() -> Stats { ACTIVE.with(|s| s.borrow().unwrap_or_default()) }
pub fn frame() { note(|s| {
    s.frame_observed = true; s.frame_moving = false; s.last_draws = 0; s.last_ready = 0;
}); }
/// Called at the same post-swap seam as the heartbeat's present count. Offscreen
/// preparation and an unswapped frame cannot increase the measured FPS.
pub fn presented(now: u32) { note(|s| {
    if !s.frame_observed { return; }
    s.frame_observed = false;
    s.frames += 1;
    s.ph_frames += (s.last_draws > s.last_ready) as u64;
    if s.frame_moving {
        s.moving_frames += 1;
        if s.moving_frames == 1 { s.moving_first_ms = now; }
        s.moving_last_ms = now;
    }
}); }
pub fn draw(ready: bool) {
    let Some(verdict) = super::card_motion::verdict() else { return };
    note(|s| {
        s.draws += 1; s.last_draws += 1;
        s.ready += ready as u64; s.last_ready += ready as u64;
        let moving = verdict == super::card_motion::Verdict::Moving;
        s.moving += moving as u64;
        s.frame_moving |= moving;
        s.unknown += (verdict == super::card_motion::Verdict::Unknown) as u64;
    });
}
pub fn request() {
    let Some(verdict) = super::card_motion::verdict() else { return };
    note(|s| { s.requested += 1; s.requested_moving += (verdict == super::card_motion::Verdict::Moving) as u64; });
}
#[derive(Clone, Copy)]
pub enum Refused { New, Evicted, Retry }
pub fn refused(kind: Refused) {
    if super::card_motion::verdict() != Some(super::card_motion::Verdict::Moving) { return; }
    note(|s| match kind { Refused::New => s.refused_new += 1, Refused::Evicted => s.refused_evicted += 1, Refused::Retry => s.refused_retry += 1 });
}
pub fn upload() { note(|s| s.uploads += 1); }
pub fn evicted() { note(|s| s.lost += 1); }

pub fn rearmed() { note(|s| s.rearmed += 1); }

/// One `phcount:` event-log line for the interval between two snapshots: how many presented
/// frames drew cards (`frames`), how many of those showed at least one card with no texture
/// (`ph_frames`), and the card draws (`draws`) and the ones with no texture (`ph`, the
/// placeholder tile-frames). `tests/mock_fps.py` parses it by these field names.
pub fn interval_line(prev: Stats, cur: Stats) -> String {
    format!(
        "phcount: frames={} ph_frames={} draws={} ph={}",
        cur.frames.saturating_sub(prev.frames),
        cur.ph_frames.saturating_sub(prev.ph_frames),
        cur.draws.saturating_sub(prev.draws),
        cur.draws.saturating_sub(cur.ready).saturating_sub(prev.draws.saturating_sub(prev.ready)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_interval_line_counts_only_what_the_interval_drew() {
        arm();
        let _scope = crate::card_motion::Scope::moving_for_test();
        // frame 1: six cards, two without a texture
        frame();
        for ready in [true, true, true, true, false, false] { draw(ready); }
        presented(16);
        let first = snapshot();
        assert_eq!(interval_line(Stats::default(), first), "phcount: frames=1 ph_frames=1 draws=6 ph=2");
        // frame 2: all six ready; frame 3: one without
        frame();
        for _ in 0..6 { draw(true); }
        presented(32);
        frame();
        for ready in [true, true, true, true, true, false] { draw(ready); }
        presented(48);
        assert_eq!(interval_line(first, snapshot()), "phcount: frames=2 ph_frames=1 draws=12 ph=1");
        ACTIVE.with(|s| *s.borrow_mut() = None);
    }
    #[test]
    fn drawing_without_a_swap_is_not_frame_rate_evidence() {
        arm();
        frame();
        let _scope = crate::card_motion::Scope::moving_for_test();
        draw(false);
        assert_eq!(snapshot().moving_frames, 0);
        presented(16);
        assert_eq!(snapshot().moving_frames, 1);
        presented(32);
        assert_eq!(snapshot().moving_frames, 1);
        ACTIVE.with(|s| *s.borrow_mut() = None);
    }
    #[test]
    fn motion_pacing_counts_the_actual_frame_span_including_long_frames() {
        arm();
        for ms in [0, 50, 150] {
            crate::card_motion::begin_frame(ms);
            frame();
            let _scope = crate::card_motion::Scope::moving_for_test();
            draw(false);
            presented(ms);
        }
        let s = snapshot();
        assert_eq!(s.moving_frames, 3);
        assert_eq!(s.moving_last_ms - s.moving_first_ms, 150);
        assert_eq!(s.requested_moving, 0);
        ACTIVE.with(|s| *s.borrow_mut() = None);
    }
}
