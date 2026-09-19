//! **Named sub-spans of ONE frame's draw**, printed on that frame's `FRAMEDROP` line as
//! `spans=<name>:<ms>,…` — the attribution the eight phase stamps cannot give.
//!
//! `FRAMEDROP … draw=61.5` says the draw phase ate the frame and nothing about which part of it:
//! the page, the host snapshot's full-screen copy, the underlay field's readback, a surface's
//! first paint. `plxnative-cpuprof` measures every `ui::profile::phase` but aggregates over sixty
//! frames, so the one slow frame of a modal's open — the frame this instrument exists for — is
//! averaged into fifty-nine quick ones. A span here is per FRAME, reset at every presented frame
//! and printed only beside the frame it belongs to.
//!
//! **Wall time on the render thread, no `glFinish`.** A span that contains a pipeline stall — a
//! `glReadPixels`, a framebuffer-0 command that waits on the previous frame — reports that wait as
//! its own time, and that is the point: the stall IS the frame's cost, and a span is where it was
//! paid. It is not GPU time; `plxnative-hwcnt` is.
//!
//! Armed with the rest of the frame instrument (`plxnative-framedrop`); unarmed, [`span`] is one
//! relaxed atomic load and a direct call.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

static ON: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// This frame's spans in first-seen order, summed by name.
    static SPANS: RefCell<Vec<(&'static str, u32, f64)>> = const { RefCell::new(Vec::new()) };
}

/// Arm the instrument — `app::boot`, beside `Instruments::new`, when `plxnative-framedrop` is set.
pub(crate) fn arm() {
    ON.store(true, Relaxed);
}

#[inline]
pub(crate) fn armed() -> bool {
    ON.load(Relaxed)
}

/// Run `f`, and when armed add its wall time to this frame's `name`.
#[inline]
pub(crate) fn span<R>(name: &'static str, f: impl FnOnce() -> R) -> R {
    if !armed() {
        return f();
    }
    let t0 = std::time::Instant::now();
    let r = f();
    note(name, t0.elapsed().as_secs_f64() * 1000.0);
    r
}

/// Add `ms` to this frame's `name` (the seam [`span`] and the tests share).
pub(crate) fn note(name: &'static str, ms: f64) {
    SPANS.with(|s| {
        let mut s = s.borrow_mut();
        match s.iter_mut().find(|e| e.0 == name) {
            Some(e) => {
                e.1 += 1;
                e.2 += ms;
            }
            None => s.push((name, 1, ms)),
        }
    });
}

/// This frame's spans as the `FRAMEDROP` field (`""` when none ran), and a reset for the next
/// frame. Called once per PRESENTED frame, whether or not it crossed the threshold, so a span
/// never carries into a later frame's line — the same rule `FrameCounters` learned in phase 11.
pub(crate) fn take() -> String {
    SPANS.with(|s| {
        let mut s = s.borrow_mut();
        let out = format(&s);
        s.clear();
        out
    })
}

fn format(spans: &[(&'static str, u32, f64)]) -> String {
    if spans.is_empty() {
        return String::new();
    }
    let mut out = String::from("spans=");
    for (i, (name, n, ms)) in spans.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if *n > 1 {
            out.push_str(&std::format!("{name}x{n}:{ms:.1}"));
        } else {
            out.push_str(&std::format!("{name}:{ms:.1}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_sum_by_name_in_first_seen_order_and_take_resets() {
        let _ = take();
        note("page", 3.0);
        note("field", 20.3);
        note("page", 1.0);
        assert_eq!(take(), "spans=pagex2:4.0,field:20.3");
        assert_eq!(take(), "", "taken: the next frame starts empty");
    }
}
