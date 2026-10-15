//! `assperf:` — what libass costs on the television, one event-log line per second while a
//! styled-subtitle source is rendering.
//!
//! ```text
//! assperf: worker tid=1234
//! assperf: window_ms=1000 calls=3 changed=2 p95_us=1800 max_us=4100 job_p95_us=2100 job_max_us=9000 pub_bytes=123456 up_bytes=123456 jobs=2+1 ahead_hit=1 nwait_p95_us=0 nwait_max_us=0
//! ```
//!
//! `window_ms` is the span the counts cover: from the previous line (or the session's first job)
//! to this one. Jobs are rare while a cue is steady, so a line can follow a long idle stretch and
//! the counts are only comparable as rates over `window_ms`. `calls` is how many times the worker ran `plx_ass_render` (one per job: a frame is rendered
//! when the screen needs one, and the next boundary is rendered ahead of its time, not once per UI
//! frame), `changed` how many of those produced a frame whose pixels differ from the one on screen
//! (published at once, or held ahead for the next boundary), `p95_us`/`max_us` the wall time
//! of the FFI call alone (nearest-rank 95th percentile over a fixed ring), `job_p95_us`/`job_max_us`
//! the same for the whole worker job (source load, event append, flush-and-reload, window slide
//! and the draw call: the cost a cue change or seek pays), `pub_bytes` the RGBA
//! bytes of the frames published or held ahead, and `up_bytes` the bytes the UI thread uploaded with
//! `glTexImage2D` in the same window ([`note_upload`]). `jobs=N+A` counts the worker's Normal and
//! Ahead jobs separately, `ahead_hit` how many times the UI thread promoted an ahead frame
//! ([`note_ahead_hit`]) and `nwait_*` how long a Normal job waited behind an Ahead job that was
//! already rendering ([`note_nwait`]; native rendering cannot be cancelled). The upload's own time is the `assup`
//! span on `FRAMEDROP spans=` (spans print only on `FRAMEDROP` lines and only while the frame
//! instrument is armed, `/tmp/plxnative-framedrop`). A window closes on the first job at least a
//! second after it opened (an idle stretch emits no line, hence `window_ms`), and [`flush`] emits the partial window when the source is deselected.
//! `tools/tv-sched-trace.sh --tid <tid>` follows the worker.
//!
//! Always compiled; the cost is one clock pair per render and a fixed 256-slot ring.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const RING: usize = 256;
const WINDOW: Duration = Duration::from_secs(1);

static UPLOADED: AtomicU64 = AtomicU64::new(0);
static AHEAD_HITS: AtomicU64 = AtomicU64::new(0);

/// UI thread: RGBA bytes handed to `glTexImage2D` for a styled-subtitle region.
pub fn note_upload(bytes: usize) {
    UPLOADED.fetch_add(bytes as u64, Ordering::Relaxed);
}

/// UI thread: the ahead frame was promoted without a worker job.
pub fn note_ahead_hit() {
    AHEAD_HITS.fetch_add(1, Ordering::Relaxed);
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
    /// An Ahead job (rendered early) rather than a Normal one.
    pub ahead: bool,
}

/// Counters other threads keep, read when a window closes.
#[derive(Clone, Copy, Default)]
pub(super) struct Shared {
    pub up_bytes: u64,
    pub ahead_hits: u64,
}

pub(super) struct Aggregator {
    started: Option<Instant>,
    render: Ring,
    job: Ring,
    nwait: Ring,
    normal_jobs: u32,
    ahead_jobs: u32,
    nwait_max_us: u32,
    calls: u32,
    changed: u32,
    max_us: u32,
    job_max_us: u32,
    pub_bytes: u64,
    /// When the counters (these and the cross-thread ones) last restarted: the previous line, or
    /// the first job of a session. Survives a line so `window_ms` spans the idle gap too.
    counting_since: Option<Instant>,
}

impl Aggregator {
    pub(super) const fn new() -> Self {
        Self {
            started: None,
            render: Ring::new(),
            job: Ring::new(),
            nwait: Ring::new(),
            normal_jobs: 0,
            ahead_jobs: 0,
            nwait_max_us: 0,
            calls: 0,
            changed: 0,
            max_us: 0,
            job_max_us: 0,
            pub_bytes: 0,
            counting_since: None,
        }
    }

    /// Record one cycle; returns the finished window's line once a second has passed.
    pub(super) fn push(&mut self, now: Instant, cycle: Cycle, shared: Shared) -> Option<String> {
        let started = *self.started.get_or_insert(now);
        self.counting_since.get_or_insert(now);
        if let Some(us) = cycle.render_us {
            self.render.push(us);
            self.calls += 1;
            self.max_us = self.max_us.max(us);
        }
        self.job.push(cycle.job_us);
        if cycle.ahead {
            self.ahead_jobs += 1;
        } else {
            self.normal_jobs += 1;
        }
        self.job_max_us = self.job_max_us.max(cycle.job_us);
        self.changed += u32::from(cycle.changed);
        self.pub_bytes += cycle.pub_bytes;
        if now.saturating_duration_since(started) < WINDOW {
            return None;
        }
        Some(self.take_line(now, shared))
    }

    /// A Normal job waited `us` behind an Ahead job that was already rendering.
    pub(super) fn note_nwait(&mut self, us: u32) {
        self.nwait.push(us);
        self.nwait_max_us = self.nwait_max_us.max(us);
    }

    /// The open window as a line (and reset), or `None` if nothing was recorded.
    pub(super) fn take_partial(&mut self, now: Instant, shared: Shared) -> Option<String> {
        (self.job.pushed > 0).then(|| {
            let line = self.take_line(now, shared);
            self.counting_since = None;
            line
        })
    }

    fn take_line(&mut self, now: Instant, shared: Shared) -> String {
        let window_ms = self
            .counting_since
            .map_or(0, |since| now.saturating_duration_since(since).as_millis());
        let line = format!(
            "assperf: window_ms={} calls={} changed={} p95_us={} max_us={} job_p95_us={} job_max_us={} pub_bytes={} up_bytes={} jobs={}+{} ahead_hit={} nwait_p95_us={} nwait_max_us={}",
            window_ms,
            self.calls,
            self.changed,
            self.render.p95(),
            self.max_us,
            self.job.p95(),
            self.job_max_us,
            self.pub_bytes,
            shared.up_bytes,
            self.normal_jobs,
            self.ahead_jobs,
            shared.ahead_hits,
            self.nwait.p95(),
            self.nwait_max_us
        );
        *self = Self::new();
        self.counting_since = Some(now);
        line
    }
}

thread_local! {
    static AGG: RefCell<Aggregator> = const { RefCell::new(Aggregator::new()) };
    static LAST_RENDER_US: Cell<Option<u32>> = const { Cell::new(None) };
    static LAST_JOB_US: Cell<Option<u32>> = const { Cell::new(None) };
    static LAST_JOB_AHEAD: Cell<bool> = const { Cell::new(false) };
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
pub(super) fn record_job(elapsed: Duration, ahead: bool) {
    LAST_JOB_US.set(Some(micros(elapsed)));
    LAST_JOB_AHEAD.set(ahead);
}

/// WORKER: a Normal job started `waited` after it was queued behind an Ahead job in flight.
pub(super) fn note_nwait(waited: Duration) {
    AGG.with(|a| a.borrow_mut().note_nwait(micros(waited)));
}

/// Push one cycle and, when its window closes, restart the cross-thread counters. Reset only on
/// emission; events landing between the load and the store are dropped (a diagnostic line, not
/// an account).
fn close_cycle(
    agg: &mut Aggregator,
    now: Instant,
    cycle: Cycle,
    uploaded: &AtomicU64,
    ahead_hits: &AtomicU64,
) -> Option<String> {
    let shared = Shared {
        up_bytes: uploaded.load(Ordering::Relaxed),
        ahead_hits: ahead_hits.load(Ordering::Relaxed),
    };
    let line = agg.push(now, cycle, shared)?;
    uploaded.store(0, Ordering::Relaxed);
    ahead_hits.store(0, Ordering::Relaxed);
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
        ahead: LAST_JOB_AHEAD.take(),
    };
    let line = AGG.with(|a| {
        close_cycle(&mut a.borrow_mut(), Instant::now(), cycle, &UPLOADED, &AHEAD_HITS)
    });
    if let Some(line) = line {
        plx_base::eventlog::log(&line);
    }
}

/// WORKER: the source was deselected; emit the partial window and start the next session clean.
pub(super) fn flush() {
    let line = AGG.with(|a| {
        a.borrow_mut().take_partial(Instant::now(), Shared {
            up_bytes: UPLOADED.swap(0, Ordering::Relaxed),
            ahead_hits: AHEAD_HITS.swap(0, Ordering::Relaxed),
        })
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
            ahead: false,
        }
    }

    fn up(up_bytes: u64) -> Shared {
        Shared { up_bytes, ahead_hits: 0 }
    }

    #[test]
    fn window_closes_after_a_second_with_p95_and_max() {
        let t0 = Instant::now();
        let mut a = Aggregator::new();
        for i in 1..=200u32 {
            let line = a.push(
                t0 + Duration::from_micros((i as u64 - 1) * 5030),
                cycle((i % 100 + 1) * 10, i % 10 == 0, 100),
                Shared { up_bytes: 777, ahead_hits: 3 },
            );
            if i < 200 {
                assert!(line.is_none());
            } else {
                assert_eq!(
                    line.as_deref(),
                    Some("assperf: window_ms=1000 calls=200 changed=20 p95_us=950 max_us=1000 job_p95_us=1900 job_max_us=2000 pub_bytes=20000 up_bytes=777 jobs=200+0 ahead_hit=3 nwait_p95_us=0 nwait_max_us=0")
                );
            }
        }
        assert_eq!(a.calls, 0, "the window resets");
    }

    #[test]
    fn a_line_after_an_idle_stretch_reports_the_span_it_covers() {
        let t0 = Instant::now();
        let mut a = Aggregator::new();
        assert!(a.push(t0, cycle(10, true, 1), up(0)).is_none());
        let first = a.push(t0 + WINDOW, cycle(10, true, 1), up(0)).unwrap();
        assert!(first.starts_with("assperf: window_ms=1000 calls=2 "), "{first}");
        // Ten idle seconds, then a job and, a second later, the job that closes the window.
        assert!(a.push(t0 + 11 * WINDOW, cycle(10, true, 1), up(0)).is_none());
        let second = a.push(t0 + 12 * WINDOW, cycle(10, true, 1), up(0)).unwrap();
        assert!(second.starts_with("assperf: window_ms=11000 calls=2 "), "{second}");
        // A partial window at deselection ends the session: the next one starts its own clock.
        assert!(a.push(t0 + 20 * WINDOW, cycle(10, true, 1), up(0)).is_none());
        let partial = a.take_partial(t0 + 20 * WINDOW + WINDOW / 2, up(0)).unwrap();
        assert!(partial.starts_with("assperf: window_ms=8500 "), "{partial}");
        assert!(a.push(t0 + 30 * WINDOW, cycle(10, true, 1), up(0)).is_none());
        let next = a.take_partial(t0 + 30 * WINDOW, up(0)).unwrap();
        assert!(next.starts_with("assperf: window_ms=0 "), "{next}");
    }

    #[test]
    fn ring_keeps_only_the_latest_samples() {
        let t0 = Instant::now();
        let mut a = Aggregator::new();
        for _ in 0..(RING + 44) {
            assert!(a.push(t0, cycle(5000, false, 0), up(0)).is_none());
        }
        let line = a.push(t0 + WINDOW, cycle(1, false, 0), up(0)).unwrap();
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
            ahead: true,
        };
        assert!(a.push(t0, load, up(0)).is_none());
        a.note_nwait(300);
        a.note_nwait(900);
        let line = a.take_partial(t0, up(0)).unwrap();
        assert_eq!(
            line,
            "assperf: window_ms=0 calls=0 changed=1 p95_us=0 max_us=0 job_p95_us=50000 job_max_us=50000 pub_bytes=8 up_bytes=0 jobs=0+1 ahead_hit=0 nwait_p95_us=900 nwait_max_us=900"
        );
    }

    #[test]
    fn take_partial_emits_the_open_window_once_and_resets() {
        let t0 = Instant::now();
        let mut a = Aggregator::new();
        assert!(a.take_partial(t0, up(5)).is_none(), "nothing recorded");
        assert!(a.push(t0, cycle(100, true, 4), up(0)).is_none());
        let line = a.take_partial(t0, up(9)).unwrap();
        assert!(line.contains("calls=1 changed=1 ") && line.contains("up_bytes=9 "), "{line}");
        assert!(a.take_partial(t0, up(9)).is_none(), "the window was reset");
    }

    #[test]
    fn closing_a_window_restarts_the_upload_counter() {
        let t0 = Instant::now();
        let uploaded = AtomicU64::new(0);
        let hits = AtomicU64::new(0);
        let mut a = Aggregator::new();
        uploaded.fetch_add(4096, Ordering::Relaxed);
        assert!(close_cycle(&mut a, t0, cycle(10, true, 1), &uploaded, &hits).is_none());
        assert_eq!(uploaded.load(Ordering::Relaxed), 4096, "mid-window: kept");
        let line = close_cycle(&mut a, t0 + WINDOW, cycle(10, false, 0), &uploaded, &hits).unwrap();
        assert!(line.contains("up_bytes=4096 "), "{line}");
        assert_eq!(uploaded.load(Ordering::Relaxed), 0, "next window starts at 0");
        uploaded.fetch_add(7, Ordering::Relaxed);
        assert!(close_cycle(&mut a, t0 + 3 * WINDOW, cycle(10, false, 0), &uploaded, &hits).is_none());
        let line = close_cycle(&mut a, t0 + 5 * WINDOW, cycle(10, false, 0), &uploaded, &hits).unwrap();
        assert!(line.contains("up_bytes=7 "), "{line}");
    }

    #[test]
    fn closing_a_window_reports_and_restarts_the_ahead_hits() {
        let t0 = Instant::now();
        let (uploaded, hits) = (AtomicU64::new(0), AtomicU64::new(0));
        let mut a = Aggregator::new();
        hits.fetch_add(2, Ordering::Relaxed);
        let line = close_cycle(&mut a, t0, cycle(10, true, 1), &uploaded, &hits);
        assert!(line.is_none() && hits.load(Ordering::Relaxed) == 2, "mid-window: kept");
        let line = close_cycle(&mut a, t0 + WINDOW, cycle(10, false, 0), &uploaded, &hits).unwrap();
        assert!(line.contains("ahead_hit=2 "), "{line}");
        assert_eq!(hits.load(Ordering::Relaxed), 0);
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
        record_job(Duration::from_micros(2000), false);
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
        record_job(Duration::from_micros(9), false);
        // Cycle 2 made no native call at all.
        begin_call();
        finish_call(false, 0);
        AGG.with(|a| assert_eq!((a.borrow().calls, a.borrow().job.pushed), (0, 0)));
    }
}
