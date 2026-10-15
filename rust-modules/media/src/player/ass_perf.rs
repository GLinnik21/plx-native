//! `assperf:` — what libass costs on the television, one event-log line per second while a
//! styled-subtitle source is rendering.
//!
//! ```text
//! assperf: worker tid=1234
//! assperf: calls=60 changed=2 p95_us=1800 max_us=4100 job_p95_us=2100 job_max_us=9000 pub_bytes=123456 up_bytes=123456
//! ```
//!
//! `calls` is how many times the worker ran `plx_ass_render` (one per UI frame while a source is
//! selected), `changed` how many of those published a new frame, `p95_us`/`max_us` the wall time
//! of the FFI call alone (nearest-rank 95th percentile over a fixed ring), `job_p95_us`/`job_max_us`
//! the same for the whole worker job (source load, event append, flush-and-reload, window slide
//! and the draw call: the cost a cue change or seek pays), `pub_bytes` the RGBA
//! bytes of the frames published, and `up_bytes` the bytes the UI thread uploaded with
//! `glTexImage2D` in the same second ([`note_upload`]). The upload's own time is the `assup`
//! span on `FRAMEDROP spans=` (spans print only on `FRAMEDROP` lines and only while the frame
//! instrument is armed, `/tmp/plxnative-framedrop`). A window closes on the first render at least a
//! second after it opened, and [`flush`] emits the partial window when the source is deselected.
//! `tools/tv-sched-trace.sh --tid <tid>` follows the worker.
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

/// Fixed ring of the latest samples with a nearest-rank percentile.
struct Ring {
    samples: [u32; RING],
    stored: usize,
    pushed: usize,
}

impl Ring {
    const fn new() -> Self {
        Self {
            samples: [0; RING],
            stored: 0,
            pushed: 0,
        }
    }

    fn push(&mut self, us: u32) {
        self.samples[self.pushed % RING] = us;
        self.pushed += 1;
        self.stored = (self.stored + 1).min(RING);
    }

    fn p95(&self) -> u32 {
        if self.stored == 0 {
            return 0;
        }
        let mut sorted = [0u32; RING];
        sorted[..self.stored].copy_from_slice(&self.samples[..self.stored]);
        sorted[..self.stored].sort_unstable();
        sorted[(self.stored * 95).div_ceil(100).max(1) - 1]
    }
}

/// One finished worker cycle.
#[derive(Clone, Copy)]
pub(super) struct Cycle {
    /// The `plx_ass_render` call, if the cycle made one.
    pub render_us: Option<u32>,
    /// The whole `Engine::render` job.
    pub job_us: u32,
    pub changed: bool,
    pub pub_bytes: u64,
}

pub(super) struct Aggregator {
    started: Option<Instant>,
    render: Ring,
    job: Ring,
    calls: u32,
    changed: u32,
    max_us: u32,
    job_max_us: u32,
    pub_bytes: u64,
}

impl Aggregator {
    pub(super) const fn new() -> Self {
        Self {
            started: None,
            render: Ring::new(),
            job: Ring::new(),
            calls: 0,
            changed: 0,
            max_us: 0,
            job_max_us: 0,
            pub_bytes: 0,
        }
    }

    /// Record one cycle; returns the finished window's line once a second has passed.
    pub(super) fn push(&mut self, now: Instant, cycle: Cycle, up_bytes: u64) -> Option<String> {
        let started = *self.started.get_or_insert(now);
        if let Some(us) = cycle.render_us {
            self.render.push(us);
            self.calls += 1;
            self.max_us = self.max_us.max(us);
        }
        self.job.push(cycle.job_us);
        self.job_max_us = self.job_max_us.max(cycle.job_us);
        self.changed += u32::from(cycle.changed);
        self.pub_bytes += cycle.pub_bytes;
        if now.saturating_duration_since(started) < WINDOW {
            return None;
        }
        Some(self.take_line(up_bytes))
    }

    /// The open window as a line (and reset), or `None` if nothing was recorded.
    pub(super) fn take_partial(&mut self, up_bytes: u64) -> Option<String> {
        (self.job.pushed > 0).then(|| self.take_line(up_bytes))
    }

    fn take_line(&mut self, up_bytes: u64) -> String {
        let line = format!(
            "assperf: calls={} changed={} p95_us={} max_us={} job_p95_us={} job_max_us={} pub_bytes={} up_bytes={}",
            self.calls,
            self.changed,
            self.render.p95(),
            self.max_us,
            self.job.p95(),
            self.job_max_us,
            self.pub_bytes,
            up_bytes
        );
        *self = Self::new();
        line
    }
}

thread_local! {
    static AGG: RefCell<Aggregator> = const { RefCell::new(Aggregator::new()) };
    static LAST_RENDER_US: Cell<Option<u32>> = const { Cell::new(None) };
    static LAST_JOB_US: Cell<Option<u32>> = const { Cell::new(None) };
}

fn micros(elapsed: Duration) -> u32 {
    elapsed.as_micros().min(u32::MAX as u128) as u32
}

/// WORKER: a request was taken; drop any sample a cancelled cycle left behind.
pub(super) fn begin_call() {
    LAST_RENDER_US.set(None);
    LAST_JOB_US.set(None);
}

/// WORKER: remember the duration of the `plx_ass_render` call just made.
pub(super) fn record_render(elapsed: Duration) {
    LAST_RENDER_US.set(Some(micros(elapsed)));
}

/// WORKER: remember the duration of the whole `Engine::render` job just run.
pub(super) fn record_job(elapsed: Duration) {
    LAST_JOB_US.set(Some(micros(elapsed)));
}

/// Push one cycle and, when its window closes, restart the upload counter. Reset only on
/// emission; uploads landing between the load and the store are dropped (a diagnostic line, not
/// an account).
fn close_cycle(
    agg: &mut Aggregator,
    now: Instant,
    cycle: Cycle,
    uploaded: &AtomicU64,
) -> Option<String> {
    let line = agg.push(now, cycle, uploaded.load(Ordering::Relaxed))?;
    uploaded.store(0, Ordering::Relaxed);
    Some(line)
}

/// WORKER: close one cycle (if a job was timed) and log the window when due.
pub(super) fn finish_call(changed: bool, pub_bytes: u64) {
    let Some(job_us) = LAST_JOB_US.take() else {
        return;
    };
    let cycle = Cycle {
        render_us: LAST_RENDER_US.take(),
        job_us,
        changed,
        pub_bytes,
    };
    let line = AGG.with(|a| close_cycle(&mut a.borrow_mut(), Instant::now(), cycle, &UPLOADED));
    if let Some(line) = line {
        plx_base::eventlog::log(&line);
    }
}

/// WORKER: the source was deselected; emit the partial window and start the next session clean.
pub(super) fn flush() {
    let line = AGG.with(|a| {
        a.borrow_mut()
            .take_partial(UPLOADED.swap(0, Ordering::Relaxed))
    });
    if let Some(line) = line {
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

    fn cycle(render_us: u32, changed: bool, pub_bytes: u64) -> Cycle {
        Cycle {
            render_us: Some(render_us),
            job_us: render_us * 2,
            changed,
            pub_bytes,
        }
    }

    #[test]
    fn window_closes_after_a_second_with_p95_and_max() {
        let t0 = Instant::now();
        let mut a = Aggregator::new();
        for i in 1..=200u32 {
            let line = a.push(
                t0 + Duration::from_micros((i as u64 - 1) * 5030),
                cycle((i % 100 + 1) * 10, i % 10 == 0, 100),
                777,
            );
            if i < 200 {
                assert!(line.is_none());
            } else {
                assert_eq!(
                    line.as_deref(),
                    Some("assperf: calls=200 changed=20 p95_us=950 max_us=1000 job_p95_us=1900 job_max_us=2000 pub_bytes=20000 up_bytes=777")
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
            assert!(a.push(t0, cycle(5000, false, 0), 0).is_none());
        }
        let line = a.push(t0 + WINDOW, cycle(1, false, 0), 0).unwrap();
        assert!(
            line.contains("calls=301 ") && line.contains("p95_us=5000 max_us=5000"),
            "{line}"
        );
    }

    #[test]
    fn a_job_without_a_native_call_counts_in_the_job_ring_only() {
        let t0 = Instant::now();
        let mut a = Aggregator::new();
        let load = Cycle {
            render_us: None,
            job_us: 50_000,
            changed: true,
            pub_bytes: 8,
        };
        assert!(a.push(t0, load, 0).is_none());
        let line = a.take_partial(0).unwrap();
        assert_eq!(
            line,
            "assperf: calls=0 changed=1 p95_us=0 max_us=0 job_p95_us=50000 job_max_us=50000 pub_bytes=8 up_bytes=0"
        );
    }

    #[test]
    fn take_partial_emits_the_open_window_once_and_resets() {
        let t0 = Instant::now();
        let mut a = Aggregator::new();
        assert!(a.take_partial(5).is_none(), "nothing recorded");
        assert!(a.push(t0, cycle(100, true, 4), 0).is_none());
        let line = a.take_partial(9).unwrap();
        assert!(line.contains("calls=1 changed=1 ") && line.ends_with("up_bytes=9"), "{line}");
        assert!(a.take_partial(9).is_none(), "the window was reset");
    }

    #[test]
    fn closing_a_window_restarts_the_upload_counter() {
        let t0 = Instant::now();
        let uploaded = AtomicU64::new(0);
        let mut a = Aggregator::new();
        uploaded.fetch_add(4096, Ordering::Relaxed);
        assert!(close_cycle(&mut a, t0, cycle(10, true, 1), &uploaded).is_none());
        assert_eq!(uploaded.load(Ordering::Relaxed), 4096, "mid-window: kept");
        let line = close_cycle(&mut a, t0 + WINDOW, cycle(10, false, 0), &uploaded).unwrap();
        assert!(line.ends_with("up_bytes=4096"), "{line}");
        assert_eq!(uploaded.load(Ordering::Relaxed), 0, "next window starts at 0");
        uploaded.fetch_add(7, Ordering::Relaxed);
        assert!(close_cycle(&mut a, t0 + 3 * WINDOW, cycle(10, false, 0), &uploaded).is_none());
        let line = close_cycle(&mut a, t0 + 5 * WINDOW, cycle(10, false, 0), &uploaded).unwrap();
        assert!(line.ends_with("up_bytes=7"), "{line}");
    }

    #[test]
    fn finish_without_a_job_is_a_no_op() {
        // Idle (nothing ran this cycle): nothing is recorded, nothing logged.
        begin_call();
        finish_call(true, 99);
        AGG.with(|a| assert_eq!(a.borrow().job.pushed, 0));
    }

    #[test]
    fn render_then_finish_counts_one_call() {
        begin_call();
        record_render(Duration::from_micros(1234));
        record_job(Duration::from_micros(2000));
        finish_call(true, 64);
        AGG.with(|a| {
            let a = a.borrow();
            assert_eq!(
                (a.calls, a.changed, a.max_us, a.job_max_us, a.pub_bytes),
                (1, 1, 1234, 2000, 64)
            );
        });
        // The slots were consumed: a second finish does not double count.
        finish_call(false, 0);
        AGG.with(|a| assert_eq!(a.borrow().calls, 1));
    }

    #[test]
    fn a_cancelled_cycles_sample_does_not_leak_into_the_next() {
        AGG.with(|a| *a.borrow_mut() = Aggregator::new());
        // Cycle 1 timed a render, then the epoch changed: it never reached finish_call.
        begin_call();
        record_render(Duration::from_micros(9));
        record_job(Duration::from_micros(9));
        // Cycle 2 made no native call at all.
        begin_call();
        finish_call(false, 0);
        AGG.with(|a| assert_eq!((a.borrow().calls, a.borrow().job.pushed), (0, 0)));
    }
}
