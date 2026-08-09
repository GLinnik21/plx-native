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
    if ok == 0 {
        // Publish it. This used to be logged and discarded, so a refused payload was
        // indistinguishable from a slow one and the pump waited on a `loadCompleted` that could
        // never come.
        super::SHARED.load_failed.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// How long the progress reporter waits between /:/timeline posts.
const REPORT_INTERVAL_S: u64 = 10;

/// One reporter's stop signal — **owned by that reporter and its Engine**, not shared.
///
/// It used to be `SHARED.report_stop` + `SHARED.report_wake`, which `reset_session` clears at the
/// END of teardown. So a reporter still alive at that moment — parked in its POST — came back to a
/// cleared flag and looped forever, which is precisely why teardown had to JOIN it before letting
/// the session reset. That join is on the MAIN thread, and `stream`'s one-shot wrappers box their
/// socket privately, so nothing could interrupt the POST: against a server that accepts and then
/// goes quiet the frame loop parked for the rest of `SO_RCVTIMEO`. **Measured at 6974 ms** with
/// `tools/netcond.py` in `stall@/:/timeline` mode.
///
/// Per-session ownership makes the stop unambiguous: a detached reporter always sees ITS OWN flag
/// set and exits, no matter what the next session does to `SHARED`. That is what lets the join
/// move off the main thread.
pub(crate) struct ReportStop {
    flag: std::sync::Mutex<bool>,
    cv: std::sync::Condvar,
}

impl ReportStop {
    pub(crate) fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(ReportStop { flag: std::sync::Mutex::new(false), cv: std::sync::Condvar::new() })
    }

    /// Tell this reporter to exit, and wake it so it notices now rather than up to 10 s from now.
    pub(crate) fn stop(&self) {
        *self.flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.cv.notify_all();
    }

    /// Wait up to `secs`, returning `true` as soon as [`stop`](Self::stop) has been called.
    ///
    /// The loop re-checks the predicate because `wait_timeout` may wake spuriously, and a spurious
    /// wake must not shorten the interval into an early extra POST.
    fn wait_or_stop(&self, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        let mut g = self.flag.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if *g {
                return true;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            g = self.cv.wait_timeout(g, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}

pub(crate) fn timeline_thread(rk: String, stop: std::sync::Arc<ReportStop>) {
    use crate::plex::TimelineState;
    loop {
        if stop.wait_or_stop(REPORT_INTERVAL_S) {
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

