//! Animation diagnostic — probe the critically-damped springs that drive the UI and, when enabled
//! (via /tmp/poc-anim, or the ANIM overlay toggle), (a) log each spring's settle metrics (frames,
//! ms, overshoot %) and (b) draw a live overlay with the approach curve. Instrument a spring by
//! calling `anim::probe(name, pos, vel, target, dt)` right after `Spring::step`. Zero cost when off.
#![allow(dead_code)]
use crate::ui::{Painter, Rect};
use std::ptr::{addr_of, addr_of_mut};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

static ENABLED: AtomicBool = AtomicBool::new(false);
pub(crate) fn set_enabled(v: bool) {
    ENABLED.store(v, Relaxed);
}
pub(crate) fn toggle() {
    ENABLED.store(!ENABLED.load(Relaxed), Relaxed);
}
pub(crate) fn enabled() -> bool {
    ENABLED.load(Relaxed)
}

const HN: usize = 96; // ring-buffer history depth

struct Entry {
    name: &'static str,
    hist: [f32; HN],
    hp: usize,
    n: usize,
    target: f32,
    vel: f32,
    moving: bool,
    frames: u32,
    ms: f32,
    start: f32,
    peak_over: f32,             // furthest past the target (in the travel direction)
    last: Option<(u32, f32, f32)>, // last settled episode: (frames, ms, overshoot %)
}
static mut ENTRIES: Vec<Entry> = Vec::new();

#[inline]
fn eps(target: f32) -> f32 {
    (target.abs() * 0.002).max(0.0015)
}

/// Record a spring sample. Detects animation episodes (settled→moving→settled) and logs the settle
/// metrics on completion. No-op unless the overlay is enabled.
pub(crate) fn probe(name: &'static str, pos: f32, vel: f32, target: f32, dt: f32) {
    if !enabled() {
        return;
    }
    let es = unsafe { &mut *addr_of_mut!(ENTRIES) };
    let i = match es.iter().position(|e| e.name == name) {
        Some(i) => i,
        None => {
            es.push(Entry {
                name,
                hist: [pos; HN],
                hp: 0,
                n: 0,
                target,
                vel,
                moving: false,
                frames: 0,
                ms: 0.0,
                start: pos,
                peak_over: 0.0,
                last: None,
            });
            es.len() - 1
        }
    };
    let e = &mut es[i];
    e.hist[e.hp] = pos;
    e.hp = (e.hp + 1) % HN;
    if e.n < HN {
        e.n += 1;
    }
    e.target = target;
    e.vel = vel;
    let moving = (pos - target).abs() > eps(target) || vel.abs() > eps(target);
    if moving && !e.moving {
        e.moving = true; // episode start
        e.frames = 0;
        e.ms = 0.0;
        e.start = pos;
        e.peak_over = 0.0;
    }
    if e.moving {
        e.frames += 1;
        e.ms += dt * 1000.0;
        let dir = (target - e.start).signum();
        let over = (pos - target) * dir; // > 0 = overshot past the target
        if over > e.peak_over {
            e.peak_over = over;
        }
    }
    if !moving && e.moving {
        e.moving = false; // settle
        let travel = (e.target - e.start).abs().max(1e-6);
        let over_pct = e.peak_over / travel * 100.0;
        e.last = Some((e.frames, e.ms, over_pct));
        log(&format!("anim {}: {}f {:.0}ms overshoot={:.1}%", e.name, e.frames, e.ms, over_pct));
    }
}

// Separate stream from the main event log — the per-settle (and, if extended, per-frame) trace can
// get large, and it should never drown the primary /tmp/poc-events.log debugging surface.
fn log(m: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/poc-anim.log") {
        let _ = writeln!(f, "{m}");
    }
}

impl Entry {
    fn cur(&self) -> f32 {
        self.hist[(self.hp + HN - 1) % HN]
    }
    /// value range spanned by the history + start/target, padded — for the sparkline y-scale
    fn range(&self) -> (f32, f32) {
        let mut lo = self.target.min(self.start);
        let mut hi = self.target.max(self.start);
        for k in 0..self.n {
            let v = self.hist[(self.hp + HN - self.n + k) % HN];
            lo = lo.min(v);
            hi = hi.max(v);
        }
        let pad = (hi - lo).max(1e-3) * 0.12;
        (lo - pad, hi + pad)
    }
}

/// Draw the debug overlay (top-left) when enabled: one panel per probed spring — a text line
/// (current pos → target, velocity, last settle metrics) and a sparkline of the approach with the
/// target marked. Call once per frame, after the rest of the UI.
pub(crate) fn draw_overlay() {
    if !enabled() {
        return;
    }
    let es = unsafe { &*addr_of!(ENTRIES) };
    if es.is_empty() {
        return;
    }
    let p = Painter::root();
    let x = 44.0f32;
    let row = 96.0f32;
    let w = 560.0f32;
    let top = 46.0f32;
    let bg = [0.0f32, 0.0, 0.0, 0.74];
    p.rect(Rect::new(x - 18.0, top - 6.0, w, es.len() as f32 * row + 58.0), 12.0, bg, bg, 0.0);
    if let Ok(t) = std::ffi::CString::new("ANIM DEBUG") {
        p.text(t.as_ptr(), x, top, 22, [0.55, 0.92, 0.6, 1.0], 0, 1);
    }
    let mut y = top + 44.0;
    for e in es.iter() {
        let col = if e.moving { [1.0f32, 0.86, 0.32, 1.0] } else { [0.72f32, 0.74, 0.80, 1.0] };
        let last = e
            .last
            .map(|(f, ms, o)| format!("   last {f}f {ms:.0}ms over {o:.1}%"))
            .unwrap_or_default();
        let line = format!("{}  {:.3} \u{2192} {:.3}  v={:.2}{}", e.name, e.cur(), e.target, e.vel, last);
        if let Ok(t) = std::ffi::CString::new(line) {
            p.text(t.as_ptr(), x, y, 18, col, 0, 0);
        }
        // sparkline of the recent samples, with a horizontal marker at the target
        let (gy, gh, gw) = (y + 28.0, 44.0f32, w - 62.0);
        let (lo, hi) = e.range();
        let span = (hi - lo).max(1e-6);
        let tny = gy + gh - (e.target - lo) / span * gh;
        let tc = [0.40f32, 0.62, 1.0, 0.55];
        p.rect(Rect::new(x, tny - 0.5, gw, 1.0), 0.0, tc, tc, 0.0);
        for k in 0..e.n {
            let v = e.hist[(e.hp + HN - e.n + k) % HN];
            let px = x + k as f32 / (e.n.max(1) - 1).max(1) as f32 * gw;
            let py = gy + gh - (v - lo) / span * gh;
            let dc = [0.58f32, 0.95, 0.62, 0.95];
            p.rect(Rect::new(px - 1.0, py - 1.0, 2.5, 2.5), 1.25, dc, dc, 0.0);
        }
        y += row;
    }
}
