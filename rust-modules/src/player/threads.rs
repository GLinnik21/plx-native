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

/// progress-reporter thread: every ~10s, POST the current position to Plex's
/// /:/timeline (route::report_timeline → the typed client) so the server updates
/// viewOffset (the resume point) + watched state. `rk` is captured at spawn (fixed per
/// playback session, no static-mut race). Exits when SHARED.report_stop is set; the
/// final state=stopped report is sent by stop_bufferfeed (main thread) with the last
/// position.
pub(crate) fn timeline_thread(rk: String) {
    use crate::plex::TimelineState;
    loop {
        // sleep ~10s in 1s steps so we exit promptly on teardown
        for _ in 0..10 {
            if SHARED.report_stop.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        if SHARED.report_stop.load(Ordering::Acquire) {
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

