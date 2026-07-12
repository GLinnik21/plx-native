//! Draw-time profiler — a diagnostic mode for the UI library.
//!
//! Enabled at boot by the `/tmp/poc-profile` dev-trigger. When on, [`phase`] brackets a named draw
//! phase with `glFinish` so the wall-clock it records is that phase's **actual GPU cost**: GLES draw
//! calls are queued and run async, so timing the CPU issue alone misses fill-rate — the `glFinish`
//! before/after forces the pipeline to drain, attributing the work to the phase. That serialization
//! means absolute FPS drops while profiling, but the **relative** per-phase split is accurate, which
//! is what you need to find an overdraw/fill-rate bottleneck. Averages over [`LOG_EVERY`] frames are
//! written to the event log by [`frame_end`]. Disabled, every hook is a single bool load + the inner
//! closure (zero overhead), so the instrumentation can stay in the draw path permanently.
//!
//! Wrap phases where you draw:  `profile::phase("backdrop", || draw_backdrop(p, m, scroll));`
//! Main-thread only (like the rest of the immediate-mode draw path).
use std::ptr::{addr_of, addr_of_mut};
use std::time::Instant;

const LOG_EVERY: u32 = 60; // frames per aggregate log line

static mut ON: bool = false;
static mut FRAMES: u32 = 0;
// (name, summed ns, sample count) — names are &'static, a handful per frame, so a Vec scan is fine.
static mut ACC: Vec<(&'static str, u128, u32)> = Vec::new();

pub(crate) fn set_enabled(on: bool) {
    unsafe { *addr_of_mut!(ON) = on }
}
#[inline]
pub(crate) fn enabled() -> bool {
    unsafe { *addr_of!(ON) }
}

/// Time one draw phase (GPU-synced) under the profiler; a passthrough when disabled.
#[inline]
pub(crate) fn phase<R>(name: &'static str, f: impl FnOnce() -> R) -> R {
    if !enabled() {
        return f();
    }
    crate::gfx::gl_finish();
    let t0 = Instant::now();
    let r = f();
    crate::gfx::gl_finish();
    accumulate(name, t0.elapsed().as_nanos());
    r
}

fn accumulate(name: &'static str, ns: u128) {
    let acc = unsafe { &mut *addr_of_mut!(ACC) };
    match acc.iter_mut().find(|e| e.0 == name) {
        Some(e) => {
            e.1 += ns;
            e.2 += 1;
        }
        None => acc.push((name, ns, 1)),
    }
}

/// Call once per frame (after the buffer swap). Every [`LOG_EVERY`] frames, log the per-phase mean
/// ms/frame and reset. No-op when disabled.
pub(crate) fn frame_end() {
    if !enabled() {
        return;
    }
    let frames = unsafe {
        let f = addr_of_mut!(FRAMES);
        *f += 1;
        *f
    };
    if frames < LOG_EVERY {
        return;
    }
    let acc = unsafe { &mut *addr_of_mut!(ACC) };
    let mut msg = String::from("PROFILE ms/frame:");
    let mut total = 0.0f64;
    for (name, ns, count) in acc.iter() {
        let ms = *ns as f64 / (*count).max(1) as f64 / 1_000_000.0;
        total += ms;
        msg.push_str(&format!(" {name}={ms:.2}"));
    }
    msg.push_str(&format!(" | sum={total:.2}"));
    log(&msg);
    acc.clear();
    unsafe { *addr_of_mut!(FRAMES) = 0 }
}

use crate::log;
