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

/// media/load thread: construct + Load (uid=NULL). The library owns its own GMainContext + loop,
/// and callbacks arrive on its thread — but Load itself is SYNCHRONOUS here and on some chassis
/// blocks for a long time inside video-output init (issue #74, k5lp/k3lp), which is why the C seam
/// refuses every other verb until it returns (`g_load_returned`) and the pump bounds the wait
/// with `NATIVE_LOAD_BUDGET`.
pub(crate) fn load_thread(
    payload: SendPtr<c_char>,
    native_epoch: u32,
    route_start: Option<crate::route::RouteStartAttempt>,
) {
    super::log("SMP: calling Load (uid=NULL)");
    let ok = unsafe { super::ffi::sf_load(payload.0, native_epoch) };
    super::log(&format!("SMP: Load returned ok={ok}"));
    // `/tmp/plxnative-holdload[=ms]`: on demand, hold the flip below so issue #74 D.1's budget
    // (the pump's "deferring" line, then, past `NATIVE_LOAD_BUDGET`, the failure read-out) is
    // observable on a real television without a set that hangs here for real. Read AFTER
    // `sf_load` returns and BEFORE `mark_native_load_returned`, matching where this needs to bite.
    if let Some(ms) = crate::dev::holdload_delay_ms() {
        super::log(&format!(
            "holdload: armed — holding the Load-returned flag for {ms}ms"
        ));
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    // issue #74 D.1: flip the epoch-scoped gate BEFORE the route ticket is published and BEFORE
    // `load_failed` is set below — `pump.rs`'s loadCompleted arm (and every other Starfish/ACB
    // verb) must never observe "Load has returned" any later than this. Route tickets are a
    // SEPARATE mechanism (a ticket can be rejected as stale independent of this gate) and are
    // published after, not before, on purpose.
    if let Some(elapsed) = SHARED.mark_native_load_returned(native_epoch) {
        // Captured HERE, at the moment the flag actually flips, so the "native: Load returned
        // after Nms" log measures the real in-flight duration rather than however long it took
        // the main-thread pump to next run and notice. Logged unconditionally, right at the gate
        // transition, rather than from `pump.rs`'s `loadCompleted` arm — that arm only runs once
        // `loadCompleted` also arrives, so on a session where Load returns but `loadCompleted`
        // never does, the old placement left this line unlogged forever, with no way to tell
        // "Load is still on the stack" from "Load returned and the pipeline went quiet". See
        // finding `load-returned-log-is-conditional-on-loadcompleted-and-unbounded-after`.
        let elapsed_ms = elapsed.as_millis() as u64;
        SHARED
            .native_load_elapsed_ms
            .store(elapsed_ms, Ordering::Relaxed);
        super::log(&format!("native: Load returned after {elapsed_ms}ms"));
        // issue #74 item 3: the Load-returned gate opening, bucketed — never the millisecond
        // count. This is the ordinary path; `pump.rs`'s two D.1.4 budget arms record the other
        // half, for a Load that never returns at all.
        super::report::note_load_gate_for(
            crate::route::playback_trace_generation(),
            super::report::LoadElapsedClass::from_ms(elapsed_ms as i64),
        );
    }
    if let Some(ticket) = route_start {
        crate::route::publish_route_start_result(
            ticket,
            if ok != 0 {
                crate::route::RouteStartResult::Started
            } else {
                crate::route::RouteStartResult::StartFailed
            },
        );
    }
    if ok == 0 {
        // Publish it. This used to be logged and discarded, so a refused payload was
        // indistinguishable from a slow one and the pump waited on a `loadCompleted` that could
        // never come.
        super::SHARED
            .load_failed
            .store(true, std::sync::atomic::Ordering::Release);
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
/// That proxy's SCOPE was broken until 2026-08-23 (`relay` discarded it), so the run actually
/// stalled every connection rather than only the reporter's. The number survives: it was recorded
/// as `THREADJOIN timeline 6974ms`, one NAMED line per join, and `timeline` is the only join that
/// could have parked whatever else was stalled — `engine::teardown` wakes the demux socket and
/// both AU lanes before joining them, while this POST had no such wake, which is the whole reason
/// this type exists.
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
        std::sync::Arc::new(ReportStop {
            flag: std::sync::Mutex::new(false),
            cv: std::sync::Condvar::new(),
        })
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
            g = self
                .cv
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// The ~10 s `/:/timeline` progress reporter.
///
/// The lease names one exact Engine. Route identity, server, PlayQueue and track projection are
/// sampled together under `PlayerControl`; no field of the main-thread `Session` is touched here.
pub(crate) fn timeline_thread(
    lease: crate::route::TimelineLease,
    stop: std::sync::Arc<ReportStop>,
) {
    use crate::plex::TimelineState;
    loop {
        if stop.wait_or_stop(REPORT_INTERVAL_S) {
            return;
        }
        let dur = SHARED.duration_ns.load(Ordering::Relaxed);
        if dur <= 0 {
            continue;
        }
        let t = SHARED.playpos_ns.load(Ordering::Relaxed) / 1_000_000;
        let d = dur / 1_000_000;
        let state = if super::TX.paused.load(Ordering::Relaxed) {
            TimelineState::Paused
        } else {
            TimelineState::Playing
        };
        if !crate::route::report_timeline(&lease, state, t, d) {
            return;
        }
        super::log(&format!(
            "timeline {} t={}s/{}s",
            state.as_str(),
            t / 1000,
            d / 1000
        ));
    }
}
