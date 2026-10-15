//! `assperf:` — what libass costs on the television, one event-log line per second while a
//! styled-subtitle source is rendering.
//!
//! ```text
//! assperf: worker tid=1234
//! assperf: calls=60 changed=2 p95_us=1800 max_us=4100 pub_bytes=123456 up_bytes=123456
//! ```
//!
//! `calls` is how many times the worker ran `plx_ass_render` (one per UI frame while a source is
//! selected), `changed` how many of those published a new frame, `p95_us`/`max_us` the wall time
//! of the FFI call alone (nearest-rank 95th percentile over a fixed ring), `pub_bytes` the RGBA
//! bytes of the frames published, and `up_bytes` the bytes the UI thread uploaded with
//! `glTexImage2D` in the same second ([`note_upload`]). The upload's own time is the `assup`
//! span on `FRAMEDROP spans=`. `tools/tv-sched-trace.sh --tid <tid>` follows the worker.
//!
//! Always compiled; the cost is one clock pair per render and a fixed 256-slot ring.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const RING: usize = 256;
const WINDOW: Duration = Duration::from_secs(1);

static UPLOADED: AtomicU64 = AtomicU64::new(0);

/// UI thread: RGBA bytes handed to `glTexImage2D` for a styled-subtitle region.
pub fn note_upload(bytes: usize) {
    UPLOADED.fetch_add(bytes as u64, Ordering::Relaxed);
}

pub(super) struct Aggregator {
    started: Option<Instant>,
    samples: [u32; RING],
    stored: usize,
    calls: u32,
    changed: u32,
    max_us: u32,
    pub_bytes: u64,
}

impl Aggregator {
    pub(super) const fn new() -> Self {
        Self {
            started: None,
            samples: [0; RING],
            stored: 0,
            calls: 0,
            changed: 0,
            max_us: 0,
            pub_bytes: 0,
        }
    }

    /// Record one render; returns the finished window's line once a second has passed.
    pub(super) fn push(
        &mut self,
        now: Instant,
        render_us: u32,
        changed: bool,
        pub_bytes: u64,
        up_bytes: u64,
    ) -> Option<String> {
        let started = *self.started.get_or_insert(now);
        self.samples[self.calls as usize % RING] = render_us;
        self.stored = (self.stored + 1).min(RING);
        self.calls += 1;
        self.changed += u32::from(changed);
        self.max_us = self.max_us.max(render_us);
        self.pub_bytes += pub_bytes;
        if now.saturating_duration_since(started) < WINDOW {
            return None;
        }
        let mut sorted = [0u32; RING];
        sorted[..self.stored].copy_from_slice(&self.samples[..self.stored]);
        sorted[..self.stored].sort_unstable();
        let rank = (self.stored * 95).div_ceil(100).max(1);
        let line = format!(
            "assperf: calls={} changed={} p95_us={} max_us={} pub_bytes={} up_bytes={}",
            self.calls,
            self.changed,
            sorted[rank - 1],
            self.max_us,
            self.pub_bytes,
            up_bytes
        );
        *self = Self::new();
        Some(line)
    }
}

thread_local! {
    static AGG: RefCell<Aggregator> = const { RefCell::new(Aggregator::new()) };
    static LAST_RENDER_US: Cell<Option<u32>> = const { Cell::new(None) };
}

/// WORKER: remember the duration of the `plx_ass_render` call just made.
pub(super) fn record_render(elapsed: Duration) {
    LAST_RENDER_US.set(Some(elapsed.as_micros().min(u32::MAX as u128) as u32));
}

/// WORKER: close one render cycle (if a native call was made) and log the window when due.
pub(super) fn finish_call(changed: bool, pub_bytes: u64) {
    let Some(us) = LAST_RENDER_US.take() else {
        return;
    };
    let line = AGG.with(|a| {
        a.borrow_mut().push(
            Instant::now(),
            us,
            changed,
            pub_bytes,
            UPLOADED.load(Ordering::Relaxed),
        )
    });
    if let Some(line) = line {
        // Reset only on emission; uploads landing between the load and here are dropped (a
        // diagnostic line, not an account).
        UPLOADED.store(0, Ordering::Relaxed);
        plx_base::eventlog::log(&line);
    }
}

/// WORKER: once, at thread start.
pub(super) fn log_worker_tid() {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let tid = unsafe { libc::syscall(libc::SYS_gettid) }.to_string();
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let tid = String::from("na");
    plx_base::eventlog::log(&format!("assperf: worker tid={tid}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_closes_after_a_second_with_p95_and_max() {
        let t0 = Instant::now();
        let mut a = Aggregator::new();
        for i in 1..=200u32 {
            let line = a.push(
                t0 + Duration::from_micros((i as u64 - 1) * 5030),
                (i % 100 + 1) * 10,
                i % 10 == 0,
                100,
                777,
            );
            if i < 200 {
                assert!(line.is_none());
            } else {
                assert_eq!(
                    line.as_deref(),
                    Some("assperf: calls=200 changed=20 p95_us=950 max_us=1000 pub_bytes=20000 up_bytes=777")
                );
            }
        }
        assert_eq!(a.calls, 0, "the window resets");
    }

    #[test]
    fn ring_keeps_only_the_latest_samples() {
        let t0 = Instant::now();
        let mut a = Aggregator::new();
        for _ in 0..(RING + 44) {
            assert!(a.push(t0, 5000, false, 0, 0).is_none());
        }
        let line = a.push(t0 + WINDOW, 1, false, 0, 0).unwrap();
        assert!(
            line.contains("calls=301 ") && line.contains("p95_us=5000 max_us=5000"),
            "{line}"
        );
    }

    #[test]
    fn finish_without_a_render_is_a_no_op() {
        // Idle (no native call this cycle): nothing is recorded, nothing logged.
        finish_call(true, 99);
        AGG.with(|a| assert_eq!(a.borrow().calls, 0));
    }

    #[test]
    fn render_then_finish_counts_one_call() {
        record_render(Duration::from_micros(1234));
        finish_call(true, 64);
        AGG.with(|a| {
            let a = a.borrow();
            assert_eq!((a.calls, a.changed, a.max_us, a.pub_bytes), (1, 1, 1234, 64));
        });
        // The slot was consumed: a second finish does not double count.
        finish_call(false, 0);
        AGG.with(|a| assert_eq!(a.borrow().calls, 1));
    }

    #[test]
    fn uploads_accumulate() {
        let before = UPLOADED.load(Ordering::Relaxed);
        note_upload(10);
        note_upload(5);
        assert!(UPLOADED.load(Ordering::Relaxed) >= before + 15);
    }
}
