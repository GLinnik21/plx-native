//! player::threads — the worker threads beside the demuxer (load / timeline) + the
//! SendPtr spawn seam. The demux thread body is `ff::demux` (libavformat over a custom
//! AVIO on stream.rs); its HttpStream box lives in the Engine (main owns it, closes it
//! to interrupt, and outlives the threads).
use super::SHARED;
use std::os::raw::c_char;
use std::sync::atomic::Ordering;

/// raw ptr we assert is Send for the spawn (the boxes/queue outlive the thread).
pub(crate) struct SendPtr<T>(pub *mut T);
unsafe impl<T> Send for SendPtr<T> {}

/// media/load thread: construct + Load (uid=NULL). The library owns its own
/// GMainContext + loop, so Load returns quickly and callbacks arrive on its thread.
pub(crate) fn load_thread(payload: SendPtr<c_char>) {
    super::log("SMP: calling Load (uid=NULL)");
    let ok = unsafe { super::ffi::sf_load(payload.0) };
    super::log(&format!("SMP: Load returned ok={ok}"));
}

/// How long the progress reporter waits between /:/timeline posts.
const REPORT_INTERVAL_S: u64 = 10;

/// Wait up to `secs`, returning `true` as soon as teardown asks us to stop.
///
/// This used to be ten 1-second `sleep`s with a flag check between them, which made
/// `engine::teardown`'s join of this thread cost a deterministic 0-1000 ms **on the main
/// thread** — paid on every stop, every reload-based seek and every audio switch.
/// `teardown` now latches `report_wake` and notifies before it joins, so the wait ends at
/// once. The loop re-checks the predicate because `wait_timeout` may wake spuriously; a
/// spurious wake must not shorten the interval into an early extra POST.
fn wait_or_stop(secs: u64) -> bool {
    let (m, cv) = &SHARED.report_wake;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if *g || SHARED.report_stop.load(Ordering::Acquire) {
            return true;
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return false;
        }
        g = cv
            .wait_timeout(g, deadline - now)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// progress-reporter thread: every ~10s, POST the current position to Plex's
/// /:/timeline (route::report_timeline → the typed client) so the server updates
/// viewOffset (the resume point) + watched state. `rk` is captured at spawn (fixed per
/// playback session, no static-mut race). Exits when SHARED.report_stop is set; the
/// final state=stopped report is sent by stop_bufferfeed (main thread) with the last
/// position.
pub(crate) fn timeline_thread(rk: String) {
    use crate::plex::TimelineState;
    loop {
        if wait_or_stop(REPORT_INTERVAL_S) {
            return;
        }
        let dur = SHARED.duration_ns.load(Ordering::Relaxed);
        if dur <= 0 || rk.is_empty() {
            continue;
        }
        let t = SHARED.playpos_ns.load(Ordering::Relaxed) / 1_000_000;
        let d = dur / 1_000_000;
        let state = if super::TX.paused.load(Ordering::Relaxed) {
            TimelineState::Paused
        } else {
            TimelineState::Playing
        };
        crate::route::report_timeline(&rk, state, t, d);
        super::log(&format!("timeline {} t={}s/{}s", state.as_str(), t / 1000, d / 1000));
    }
}

