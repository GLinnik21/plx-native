//! Physical buffer-feed tick operations. The common pump owns route/recovery decisions and
//! maps these native results to its attempt trace.

use super::engine::Engine;
use super::SHARED;
use plx_base::task::MainThread;
use std::sync::atomic::Ordering::{Relaxed, Release};
use std::time::Duration;

pub(super) struct LoadGate {
    pub native_epoch: u32,
    pub verdict: LoadGateVerdict,
}

pub(super) enum LoadGateVerdict {
    Returned,
    Deferred,
    TimedOut(Duration),
    LostEpoch,
}

/// The native Load's issued→returned gate. The worker's epoch, not a later route choice,
/// authorizes the result. A timeout publishes the existing callback failure latches and lets
/// the common pump attach the appropriate trace class to its current route attempt.
pub(super) fn load_gate(epoch: u32, budget: Duration) -> LoadGate {
    if SHARED.native_load_returned(epoch) {
        return LoadGate { native_epoch: epoch, verdict: LoadGateVerdict::Returned };
    }
    if SHARED.native_load_deferred_logged_epoch.swap(epoch, Relaxed) != epoch {
        super::log("native: loadCompleted while Load in flight — deferring");
    }
    let verdict = match SHARED.native_load_elapsed(epoch) {
        Some(elapsed) if elapsed >= budget => {
            super::log(&format!(
                "native: Load did not return within {}s — publishing load_failed",
                budget.as_secs()
            ));
            SHARED.load_failed.store(true, Release);
            SHARED.load_timed_out.store(true, Release);
            LoadGateVerdict::TimedOut(elapsed)
        }
        Some(_) => LoadGateVerdict::Deferred,
        // A synchronous UnloadCompleted can retire the native epoch while Load is deferred.
        // The gate can never open then, so surface the same failure instead of waiting forever.
        None => {
            super::log(
                "native: Load epoch left Active while loadCompleted was still deferred — \
                 publishing load_failed",
            );
            SHARED.load_failed.store(true, Release);
            SHARED.load_timed_out.store(true, Release);
            LoadGateVerdict::LostEpoch
        }
    };
    LoadGate { native_epoch: epoch, verdict }
}

/// Place the webOS 5+ exported window at the native Load's completion, before decoded frames.
/// `src` is the coded size, not the 1080p canvas. The coded size becomes known later; the caller
/// re-places once the demuxer publishes it. Record the place result for diagnostics: before this
/// mirror, the panel falsely said "not placed" throughout a successful exported-window session.
pub(super) fn place_exported(mt: &MainThread, eng: &mut Engine) {
    if super::sink().window_mode() != super::VP_EXPORTED { return; }
    let (w, h) = SHARED.video_raster();
    let src = if w > 0 && h > 0 { (w, h) } else { (1920, 1080) };
    let rv = unsafe { super::sink().place_window(mt, src.0, src.1, 0, 0, 1920, 1080) };
    eng.placed_src = src;
    SHARED.dg_place_rv.store(rv, Relaxed);
    SHARED.dg_placed_w.store(src.0, Relaxed);
    SHARED.dg_placed_h.store(src.1, Relaxed);
    super::log(&format!(
        "vplane: exported window placed src={}x{} rv={rv}",
        src.0, src.1
    ));
}
