//! `assperf:` — what libass costs on the television, one event-log line per second per worker
//! while a styled-subtitle source is rendering.
//!
//! ```text
//! assperf: worker w=0 tid=1234 nice=5
//! assperf: w=0 window_ms=1000 calls=3 changed=2 p95_us=1800 max_us=4100 job_p95_us=2100 job_max_us=9000 pub_bytes=123456 up_bytes=123456 jobs=2+1 ahead_hit=1 shown=0 late=0 stale=0 nwait_p95_us=0 nwait_max_us=0 lib_p95_us=900 comp_p95_us=800 lib_cpu_p95_us=870 comp_cpu_p95_us=790 images_p95=410 img_kpx_p95=620 scale=1.00
//! ```
//!
//! `w` is the worker: 0 the primary, 1 and 2 the helpers a moving cue's pipeline starts when one
//! worker cannot make thirty frames a second (issue #1328); each keeps its own window and writes its
//! own line, and `tools/tv-sched-trace.sh --tid <tid>` follows one. The counts the UI thread keeps
//! (`up_bytes`, `ahead_hit`, `shown`, `late`) belong to the primary's line alone: it reads and
//! restarts them, so a helper's line has zeros there and nothing is counted twice.
//!
//! `window_ms` is the span the counts cover: from the previous line (or the session's first job)
//! to this one. Jobs are rare while a cue is steady, so a line can follow a long idle stretch and
//! the counts are only comparable as rates over `window_ms`. `calls` is how many times the worker ran `plx_ass_render` (one per job: a frame is rendered
//! when the screen needs one, a moving cue's frames are rendered ahead of their playhead by the
//! pipeline, and the next boundary of a still cue is rendered ahead of its time, not once per UI
//! frame), `changed` how many of those produced a frame whose pixels differ from the one on screen
//! (published at once, or held ahead for the next boundary or playhead), `p95_us`/`max_us` the wall time
//! of the FFI call alone (nearest-rank 95th percentile over a fixed ring), `job_p95_us`/`job_max_us`
//! the same for the whole worker job (source load, event append, flush-and-reload, window slide
//! and the draw call: the cost a cue change or seek pays), `pub_bytes` the RGBA
//! bytes of the frames published or held ahead, and `up_bytes` the bytes the UI thread uploaded with
//! `glTexImage2D` in the same window ([`note_upload`]). `jobs=N+A` counts the worker's Normal and
//! Ahead jobs separately (a frame the pipeline planned is an Ahead one), `ahead_hit` how many
//! times the UI thread promoted the boundary frame of a still cue ([`note_ahead_hit`]), `shown`
//! how many times it promoted a frame of the pipeline ([`note_shown`]: the moving cue's updates a
//! second) and `late` how many of those were promoted more than a tick after their playhead (the
//! frame had not landed by then), `stale` how many frames this worker rendered that were thrown
//! away when they landed (the plan had moved on: a seek, an appended event it could not absorb, a
//! jump of the clock) and `nwait_*` how long a Normal job waited behind an Ahead job that was
//! already rendering ([`note_nwait`]; native rendering cannot be cancelled). The last six fields
//! split `p95_us`: the facade times its two phases itself (`clock_gettime` in `src/ass.c`,
//! `PlxAssStats::last_*`) and the worker reads them after every render. `lib_p95_us` is libass's
//! own `ass_render_frame`, `comp_p95_us` is everything the facade does with the image list after it
//! (bounds, regions, the composite into premultiplied RGBA, eight pixels at a time with NEON on the TV
//! and on arm64 hosts and scalar elsewhere; 0 for a call that found the frame
//! unchanged), both wall time (`CLOCK_MONOTONIC`) and nearest-rank over the same fixed ring as
//! `p95_us` (every call that returned a result, so a window of unchanged calls pulls them down).
//! `lib_cpu_p95_us` and `comp_cpu_p95_us` are the same two phases on the worker thread's CPU clock
//! (`CLOCK_THREAD_CPUTIME_ID`): a phase whose wall figure is far above its CPU one was waiting (the
//! thread descheduled behind the UI thread or another process, or blocked), not computing, which
//! is how a slow frame is told from a starved one. `images_p95` the `ASS_Image`s libass
//! produced and `img_kpx_p95` the sum of their width x height in thousands of pixels
//! (`last_image_pixels / 1000`), both over the composed frames only. They are 0 for a worker whose
//! library reports no stats. `scale` is the smallest size a job of the window was drawn at, as the
//! side of the frame over the side of the output (`1.00`, `0.67`, `0.50`): a moving stretch that
//! cannot make thirty frames a second at full size is drawn smaller (issue #1328, `docs/ass-subtitles.md`),
//! and the figures above are for the frames of that size, so a line is read at its own `scale`. Each
//! move of the scale is also logged once as `ass: render scale 1.00 -> 0.50 ...`. The upload's own time is the `assup`
//! span on `FRAMEDROP spans=` (spans print only on `FRAMEDROP` lines and only while the frame
//! instrument is armed, `/tmp/plxnative-framedrop`). A window closes on the first job at least a
//! second after it opened (an idle stretch emits no line, hence `window_ms`), and [`flush`] emits the partial window when the source is deselected.
//!
//! Always compiled; the cost is one clock pair per render in Rust, six clock reads in the facade
//! (three on each of the two clocks) and a fixed 256-slot ring per figure.

use super::ass::RenderScale;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const RING: usize = 256;
const WINDOW: Duration = Duration::from_secs(1);

/// What the UI thread counts between two lines. One owner reads and restarts it (the primary
/// worker, [`finish_call`]): were every worker to, a window closing on one would take the counts of
/// them all, and the rest would print zeros for a stretch that had them.
struct Counters {
    uploaded: AtomicU64,
    ahead_hits: AtomicU64,
    shown: AtomicU64,
    late: AtomicU64,
}

impl Counters {
    const fn new() -> Self {
        Self {
            uploaded: AtomicU64::new(0),
            ahead_hits: AtomicU64::new(0),
            shown: AtomicU64::new(0),
            late: AtomicU64::new(0),
        }
    }

    fn read(&self) -> Shared {
        Shared {
            up_bytes: self.uploaded.load(Ordering::Relaxed),
            ahead_hits: self.ahead_hits.load(Ordering::Relaxed),
            shown: self.shown.load(Ordering::Relaxed),
            late: self.late.load(Ordering::Relaxed),
        }
    }

    /// Restart them, after a window that [`Counters::read`] them. Events landing between the read
    /// and the restart are dropped (a diagnostic line, not an account).
    fn reset(&self) {
        for counter in [&self.uploaded, &self.ahead_hits, &self.shown, &self.late] {
            counter.store(0, Ordering::Relaxed);
        }
    }

    /// Read and restart in one step: the partial window of a deselected source.
    fn take(&self) -> Shared {
        Shared {
            up_bytes: self.uploaded.swap(0, Ordering::Relaxed),
            ahead_hits: self.ahead_hits.swap(0, Ordering::Relaxed),
            shown: self.shown.swap(0, Ordering::Relaxed),
            late: self.late.swap(0, Ordering::Relaxed),
        }
    }
}

static COUNTERS: Counters = Counters::new();

/// UI thread: RGBA bytes handed to `glTexImage2D` for a styled-subtitle region.
pub fn note_upload(bytes: usize) {
    COUNTERS.uploaded.fetch_add(bytes as u64, Ordering::Relaxed);
}

/// UI thread: the ahead frame of a still cue was promoted without a worker job.
pub fn note_ahead_hit() {
    COUNTERS.ahead_hits.fetch_add(1, Ordering::Relaxed);
}

/// UI thread: a frame of the moving cue's pipeline was promoted; `late` when it came more than a
/// tick after its playhead.
pub fn note_shown(late: bool) {
    COUNTERS.shown.fetch_add(1, Ordering::Relaxed);
    COUNTERS.late.fetch_add(u64::from(late), Ordering::Relaxed);
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

/// What the facade reports of the `plx_ass_render` call just made (`PlxAssStats::last_*`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct NativeCost {
    /// `ass_render_frame` alone.
    pub lib_us: u32,
    /// Everything after it until the frame was returned: 0 when the call found it unchanged.
    pub comp_us: u32,
    /// The worker thread's CPU time inside the same two phases (`CLOCK_THREAD_CPUTIME_ID`), the
    /// wall figures above minus the time the thread was not running.
    pub lib_cpu_us: u32,
    pub comp_cpu_us: u32,
    /// `ASS_Image`s in the frame; only meaningful when `composed`.
    pub images: u32,
    /// The sum of their width x height; only meaningful when `composed`.
    pub image_pixels: u64,
    /// The call returned a changed frame, so the two counts above are this frame's.
    pub composed: bool,
}

/// One finished worker cycle.
#[derive(Clone, Copy)]
pub(super) struct Cycle {
    /// The `plx_ass_render` call, if the cycle made one.
    pub render_us: Option<u32>,
    /// The facade's own account of that call, when it returned a result and could be asked.
    pub native: Option<NativeCost>,
    /// The whole `Engine::render` job.
    pub job_us: u32,
    pub changed: bool,
    pub pub_bytes: u64,
    /// An Ahead job (rendered early) rather than a Normal one.
    pub ahead: bool,
    /// The frame it rendered was thrown away when it landed (the plan had moved on).
    pub stale: bool,
    /// The size it drew at.
    pub scale: RenderScale,
}

/// Counters other threads keep, read when a window closes.
#[derive(Clone, Copy, Default)]
pub(super) struct Shared {
    pub up_bytes: u64,
    pub ahead_hits: u64,
    pub shown: u64,
    pub late: u64,
}

pub(super) struct Aggregator {
    /// Which worker this window belongs to; a window restart keeps it.
    slot: usize,
    started: Option<Instant>,
    render: Ring,
    job: Ring,
    nwait: Ring,
    lib: Ring,
    comp: Ring,
    lib_cpu: Ring,
    comp_cpu: Ring,
    images: Ring,
    image_kpx: Ring,
    normal_jobs: u32,
    ahead_jobs: u32,
    stale: u32,
    nwait_max_us: u32,
    calls: u32,
    changed: u32,
    max_us: u32,
    job_max_us: u32,
    pub_bytes: u64,
    /// The smallest size a job of the window was drawn at.
    scale: RenderScale,
    /// When the counters (these and the cross-thread ones) last restarted: the previous line, or
    /// the first job of a session. Survives a line so `window_ms` spans the idle gap too.
    counting_since: Option<Instant>,
}

impl Aggregator {
    pub(super) const fn new() -> Self {
        Self {
            slot: 0,
            started: None,
            render: Ring::new(),
            job: Ring::new(),
            nwait: Ring::new(),
            lib: Ring::new(),
            comp: Ring::new(),
            lib_cpu: Ring::new(),
            comp_cpu: Ring::new(),
            images: Ring::new(),
            image_kpx: Ring::new(),
            normal_jobs: 0,
            ahead_jobs: 0,
            stale: 0,
            nwait_max_us: 0,
            calls: 0,
            changed: 0,
            max_us: 0,
            job_max_us: 0,
            pub_bytes: 0,
            scale: RenderScale::Full,
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
        if let Some(cost) = cycle.native {
            self.lib.push(cost.lib_us);
            self.comp.push(cost.comp_us);
            self.lib_cpu.push(cost.lib_cpu_us);
            self.comp_cpu.push(cost.comp_cpu_us);
            if cost.composed {
                self.images.push(cost.images);
                self.image_kpx
                    .push((cost.image_pixels / 1000).min(u64::from(u32::MAX)) as u32);
            }
        }
        self.job.push(cycle.job_us);
        if cycle.ahead {
            self.ahead_jobs += 1;
        } else {
            self.normal_jobs += 1;
        }
        self.job_max_us = self.job_max_us.max(cycle.job_us);
        self.stale += u32::from(cycle.stale);
        self.changed += u32::from(cycle.changed);
        self.pub_bytes += cycle.pub_bytes;
        if cycle.scale.factor() < self.scale.factor() {
            self.scale = cycle.scale;
        }
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
            "assperf: w={} window_ms={} calls={} changed={} p95_us={} max_us={} job_p95_us={} job_max_us={} pub_bytes={} up_bytes={} jobs={}+{} ahead_hit={} shown={} late={} stale={} nwait_p95_us={} nwait_max_us={} lib_p95_us={} comp_p95_us={} lib_cpu_p95_us={} comp_cpu_p95_us={} images_p95={} img_kpx_p95={} scale={}",
            self.slot,
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
            shared.shown,
            shared.late,
            self.stale,
            self.nwait.p95(),
            self.nwait_max_us,
            self.lib.p95(),
            self.comp.p95(),
            self.lib_cpu.p95(),
            self.comp_cpu.p95(),
            self.images.p95(),
            self.image_kpx.p95(),
            self.scale.label()
        );
        let slot = self.slot;
        *self = Self::new();
        self.slot = slot;
        self.counting_since = Some(now);
        line
    }
}

thread_local! {
    static AGG: RefCell<Aggregator> = const { RefCell::new(Aggregator::new()) };
    static LAST_RENDER_US: Cell<Option<u32>> = const { Cell::new(None) };
    static LAST_NATIVE: Cell<Option<NativeCost>> = const { Cell::new(None) };
    static LAST_JOB_US: Cell<Option<u32>> = const { Cell::new(None) };
    static LAST_JOB_AHEAD: Cell<bool> = const { Cell::new(false) };
    static LAST_JOB_SCALE: Cell<RenderScale> = const { Cell::new(RenderScale::Full) };
    /// The worker this thread is (0 = the primary, which owns the shared counters).
    static SLOT: Cell<usize> = const { Cell::new(0) };
}

fn micros(elapsed: Duration) -> u32 {
    elapsed.as_micros().min(u32::MAX as u128) as u32
}

/// WORKER: a request was taken; drop any sample a cancelled cycle left behind.
pub(super) fn begin_call() {
    LAST_RENDER_US.set(None);
    LAST_NATIVE.set(None);
    LAST_JOB_US.set(None);
}

/// WORKER: remember the duration of the `plx_ass_render` call just made.
pub(super) fn record_render(elapsed: Duration) {
    LAST_RENDER_US.set(Some(micros(elapsed)));
}

/// WORKER: remember the facade's phase split of the `plx_ass_render` call just made.
pub(super) fn record_native(cost: NativeCost) {
    LAST_NATIVE.set(Some(cost));
}

/// WORKER: remember the duration of the whole `Engine::render` job just run, and the size it drew at.
pub(super) fn record_job(elapsed: Duration, ahead: bool, scale: RenderScale) {
    LAST_JOB_US.set(Some(micros(elapsed)));
    LAST_JOB_AHEAD.set(ahead);
    LAST_JOB_SCALE.set(scale);
}

/// WORKER: a Normal job started `waited` after it was queued behind an Ahead job in flight.
pub(super) fn note_nwait(waited: Duration) {
    AGG.with(|a| a.borrow_mut().note_nwait(micros(waited)));
}

/// Push one cycle and, when its window closes, restart the cross-thread counters: `counters` is
/// the owner's (the primary worker's); a helper passes `None` and its line reads zeros there.
fn close_cycle(agg: &mut Aggregator, now: Instant, cycle: Cycle, counters: Option<&Counters>) -> Option<String> {
    let shared = counters.map_or_else(Shared::default, Counters::read);
    let line = agg.push(now, cycle, shared)?;
    if let Some(counters) = counters {
        counters.reset();
    }
    Some(line)
}

/// The counters this thread owns: the primary worker's.
fn owned_counters() -> Option<&'static Counters> {
    (SLOT.get() == 0).then_some(&COUNTERS)
}

/// WORKER: close one cycle (if a job was timed) and log the window when due. `stale`: the frame
/// the job rendered was thrown away when it landed.
pub(super) fn finish_call(changed: bool, pub_bytes: u64, stale: bool) {
    let Some(job_us) = LAST_JOB_US.take() else {
        return;
    };
    let cycle = Cycle {
        render_us: LAST_RENDER_US.take(),
        native: LAST_NATIVE.take(),
        job_us,
        changed,
        pub_bytes,
        ahead: LAST_JOB_AHEAD.take(),
        stale,
        scale: LAST_JOB_SCALE.take(),
    };
    let line = AGG.with(|a| close_cycle(&mut a.borrow_mut(), Instant::now(), cycle, owned_counters()));
    if let Some(line) = line {
        plx_base::eventlog::log(&line);
    }
}

/// WORKER: the source was deselected; emit the partial window and start the next session clean.
pub(super) fn flush() {
    let line = AGG.with(|a| {
        let mut agg = a.borrow_mut();
        // Only a window with something in it takes the owner's counts: an idle helper's flush must
        // not leave the primary's partial window without them.
        let shared = if agg.job.pushed > 0 { owned_counters().map_or_else(Shared::default, Counters::take) } else { Shared::default() };
        agg.take_partial(Instant::now(), shared)
    });
    if let Some(line) = line {
        plx_base::eventlog::log(&line);
    }
}

/// WORKER: once, at thread start: which worker this thread is (`slot`, 0 for the primary), its
/// kernel thread id, which `tools/tv-sched-trace.sh --tid` follows, and the niceness it now runs at
/// (absent when the platform has none or the kernel refused it).
pub(super) fn log_worker_tid(slot: usize, nice: Option<i32>) {
    SLOT.set(slot);
    AGG.with(|a| a.borrow_mut().slot = slot);
    let tid = plx_base::task::thread_id().map_or_else(|| String::from("na"), |t| t.to_string());
    let nice = nice.map_or_else(String::new, |n| format!(" nice={n}"));
    plx_base::eventlog::log(&format!("assperf: worker w={slot} tid={tid}{nice}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A call whose time splits evenly between libass and the composite; a changed one is a
    /// composed frame of `render_us / 10` images and `render_us` thousand pixels.
    fn cycle(render_us: u32, changed: bool, pub_bytes: u64) -> Cycle {
        Cycle {
            render_us: Some(render_us),
            native: Some(NativeCost {
                lib_us: render_us / 2,
                comp_us: render_us / 2,
                lib_cpu_us: render_us / 4,
                comp_cpu_us: render_us / 4,
                images: render_us / 10,
                image_pixels: u64::from(render_us) * 1000,
                composed: changed,
            }),
            job_us: render_us * 2,
            changed,
            pub_bytes,
            ahead: false,
            stale: false,
            scale: RenderScale::Full,
        }
    }

    fn up(up_bytes: u64) -> Shared {
        Shared { up_bytes, ..Shared::default() }
    }

    #[test]
    fn window_closes_after_a_second_with_p95_and_max() {
        let t0 = Instant::now();
        let mut a = Aggregator::new();
        for i in 1..=200u32 {
            let mut call = cycle((i % 100 + 1) * 10, i % 10 == 0, 100);
            call.stale = i % 50 == 0;
            let line = a.push(
                t0 + Duration::from_micros((i as u64 - 1) * 5030),
                call,
                Shared { up_bytes: 777, ahead_hits: 3, shown: 140, late: 9 },
            );
            if i < 200 {
                assert!(line.is_none());
            } else {
                assert_eq!(
                    line.as_deref(),
                    Some("assperf: w=0 window_ms=1000 calls=200 changed=20 p95_us=950 max_us=1000 job_p95_us=1900 job_max_us=2000 pub_bytes=20000 up_bytes=777 jobs=200+0 ahead_hit=3 shown=140 late=9 stale=4 nwait_p95_us=0 nwait_max_us=0 lib_p95_us=475 comp_p95_us=475 lib_cpu_p95_us=237 comp_cpu_p95_us=237 images_p95=91 img_kpx_p95=910 scale=1.00")
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
        assert!(first.starts_with("assperf: w=0 window_ms=1000 calls=2 "), "{first}");
        // Ten idle seconds, then a job and, a second later, the job that closes the window.
        assert!(a.push(t0 + 11 * WINDOW, cycle(10, true, 1), up(0)).is_none());
        let second = a.push(t0 + 12 * WINDOW, cycle(10, true, 1), up(0)).unwrap();
        assert!(second.starts_with("assperf: w=0 window_ms=11000 calls=2 "), "{second}");
        // A partial window at deselection ends the session: the next one starts its own clock.
        assert!(a.push(t0 + 20 * WINDOW, cycle(10, true, 1), up(0)).is_none());
        let partial = a.take_partial(t0 + 20 * WINDOW + WINDOW / 2, up(0)).unwrap();
        assert!(partial.starts_with("assperf: w=0 window_ms=8500 "), "{partial}");
        assert!(a.push(t0 + 30 * WINDOW, cycle(10, true, 1), up(0)).is_none());
        let next = a.take_partial(t0 + 30 * WINDOW, up(0)).unwrap();
        assert!(next.starts_with("assperf: w=0 window_ms=0 "), "{next}");
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
            native: None,
            job_us: 50_000,
            changed: true,
            pub_bytes: 8,
            ahead: true,
            stale: true,
            scale: RenderScale::Half,
        };
        assert!(a.push(t0, load, up(0)).is_none());
        a.note_nwait(300);
        a.note_nwait(900);
        let line = a.take_partial(t0, up(0)).unwrap();
        assert_eq!(
            line,
            "assperf: w=0 window_ms=0 calls=0 changed=1 p95_us=0 max_us=0 job_p95_us=50000 job_max_us=50000 pub_bytes=8 up_bytes=0 jobs=0+1 ahead_hit=0 shown=0 late=0 stale=1 nwait_p95_us=900 nwait_max_us=900 lib_p95_us=0 comp_p95_us=0 lib_cpu_p95_us=0 comp_cpu_p95_us=0 images_p95=0 img_kpx_p95=0 scale=0.50"
        );
    }

    #[test]
    fn the_native_split_prints_its_own_percentiles() {
        let t0 = Instant::now();
        let mut a = Aggregator::new();
        // The thread's CPU time is half the wall time in both phases: an even split, so the CPU
        // percentiles are the wall ones halved and any mix-up of the four rings shows.
        let call = |lib_us, comp_us, images, image_pixels, composed| Cycle {
            render_us: Some(lib_us + comp_us),
            native: Some(NativeCost {
                lib_us,
                comp_us,
                lib_cpu_us: lib_us / 2,
                comp_cpu_us: comp_us / 2,
                images,
                image_pixels,
                composed,
            }),
            job_us: lib_us + comp_us,
            changed: composed,
            pub_bytes: 0,
            ahead: false,
            stale: false,
            scale: RenderScale::Full,
        };
        // Twenty composed frames: libass 100..=2000 us, the composite 5x that, 10..=200 images of
        // 1.5 Mpx..30 Mpx; then an unchanged call, which has no composite and whose stale image
        // counts must not enter the image rings.
        for i in 1..=20u32 {
            let cost = call(i * 100, i * 500, i * 10, u64::from(i) * 1_500_000, true);
            assert!(a.push(t0, cost, up(0)).is_none());
        }
        assert!(a.push(t0, call(40, 0, 9999, 999_999_999, false), up(0)).is_none());
        let line = a.take_partial(t0, up(0)).unwrap();
        // Nearest rank: ceil(21 * 0.95) = 20th of the 21 libass samples (40 sorts first), the 20th
        // of the 21 composite samples (the 0 sorts first); 19th of the 20 composed frames.
        assert!(
            line.ends_with(
                " lib_p95_us=1900 comp_p95_us=9500 lib_cpu_p95_us=950 comp_cpu_p95_us=4750 images_p95=190 img_kpx_p95=28500 scale=1.00"
            ),
            "{line}"
        );
    }

    #[test]
    fn a_window_reports_the_smallest_size_a_job_in_it_was_drawn_at_and_the_next_one_starts_over() {
        let t0 = Instant::now();
        let mut a = Aggregator::new();
        let at = |scale| Cycle { scale, ..cycle(10, true, 1) };
        // the first frame of a stretch at full size, then the pipeline's at two thirds and at half
        for scale in [RenderScale::Full, RenderScale::TwoThirds, RenderScale::Half, RenderScale::Full] {
            assert!(a.push(t0, at(scale), up(0)).is_none());
        }
        let line = a.take_partial(t0, up(0)).unwrap();
        assert!(line.ends_with(" scale=0.50"), "{line}");
        assert!(a.push(t0, at(RenderScale::TwoThirds), up(0)).is_none());
        let line = a.take_partial(t0, up(0)).unwrap();
        assert!(line.ends_with(" scale=0.67"), "the window starts over at full size: {line}");
        assert!(a.push(t0, at(RenderScale::Full), up(0)).is_none());
        assert!(a.take_partial(t0, up(0)).unwrap().ends_with(" scale=1.00"));
    }

    #[test]
    fn a_huge_pixel_count_saturates_the_kilopixel_figure() {
        let t0 = Instant::now();
        let mut a = Aggregator::new();
        let mut cost = cycle(10, true, 0);
        cost.native.as_mut().unwrap().image_pixels = u64::MAX;
        assert!(a.push(t0, cost, up(0)).is_none());
        let line = a.take_partial(t0, up(0)).unwrap();
        assert!(line.ends_with(" img_kpx_p95=4294967295 scale=1.00"), "{line}");
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
        let counters = Counters::new();
        let mut a = Aggregator::new();
        counters.uploaded.fetch_add(4096, Ordering::Relaxed);
        assert!(close_cycle(&mut a, t0, cycle(10, true, 1), Some(&counters)).is_none());
        assert_eq!(counters.read().up_bytes, 4096, "mid-window: kept");
        let line = close_cycle(&mut a, t0 + WINDOW, cycle(10, false, 0), Some(&counters)).unwrap();
        assert!(line.contains("up_bytes=4096 "), "{line}");
        assert_eq!(counters.read().up_bytes, 0, "next window starts at 0");
        counters.uploaded.fetch_add(7, Ordering::Relaxed);
        assert!(close_cycle(&mut a, t0 + 3 * WINDOW, cycle(10, false, 0), Some(&counters)).is_none());
        let line = close_cycle(&mut a, t0 + 5 * WINDOW, cycle(10, false, 0), Some(&counters)).unwrap();
        assert!(line.contains("up_bytes=7 "), "{line}");
    }

    #[test]
    fn closing_a_window_reports_and_restarts_the_ahead_hits_and_the_shown_frames() {
        let t0 = Instant::now();
        let counters = Counters::new();
        let mut a = Aggregator::new();
        counters.ahead_hits.fetch_add(2, Ordering::Relaxed);
        counters.shown.fetch_add(30, Ordering::Relaxed);
        counters.late.fetch_add(4, Ordering::Relaxed);
        let line = close_cycle(&mut a, t0, cycle(10, true, 1), Some(&counters));
        assert!(line.is_none() && counters.read().ahead_hits == 2, "mid-window: kept");
        let line = close_cycle(&mut a, t0 + WINDOW, cycle(10, false, 0), Some(&counters)).unwrap();
        assert!(line.contains("ahead_hit=2 shown=30 late=4 stale=0 "), "{line}");
        let after = counters.read();
        assert_eq!((after.ahead_hits, after.shown, after.late), (0, 0, 0));
    }

    #[test]
    fn a_helper_names_its_worker_and_neither_reads_nor_restarts_the_shared_counts() {
        let t0 = Instant::now();
        let counters = Counters::new();
        counters.uploaded.fetch_add(5, Ordering::Relaxed);
        note_shown_into(&counters, true);
        let mut helper = Aggregator::new();
        helper.slot = 2;
        let mut stale = cycle(10, true, 1);
        stale.stale = true;
        assert!(close_cycle(&mut helper, t0, stale, None).is_none());
        let line = close_cycle(&mut helper, t0 + WINDOW, cycle(10, false, 0), None).unwrap();
        assert!(line.starts_with("assperf: w=2 window_ms=1000 "), "{line}");
        assert!(line.contains(" up_bytes=0 jobs=2+0 ahead_hit=0 shown=0 late=0 stale=1 "), "{line}");
        let owner = counters.read();
        assert_eq!((owner.up_bytes, owner.shown, owner.late), (5, 1, 1), "left for the owner");
        // the next window of the helper keeps its number
        assert!(close_cycle(&mut helper, t0 + 2 * WINDOW, cycle(10, true, 1), None).is_none());
        let again = close_cycle(&mut helper, t0 + 3 * WINDOW, cycle(10, true, 1), None).unwrap();
        assert!(again.starts_with("assperf: w=2 "), "{again}");
        // the owner then takes them with its own line
        let mut primary = Aggregator::new();
        assert!(close_cycle(&mut primary, t0, cycle(10, true, 1), Some(&counters)).is_none());
        let line = close_cycle(&mut primary, t0 + WINDOW, cycle(10, true, 1), Some(&counters)).unwrap();
        assert!(line.starts_with("assperf: w=0 ") && line.contains(" up_bytes=5 jobs=2+0 ahead_hit=0 shown=1 late=1 "), "{line}");
    }

    /// `note_shown` against `counters` instead of the process's: the same two counts.
    fn note_shown_into(counters: &Counters, late: bool) {
        counters.shown.fetch_add(1, Ordering::Relaxed);
        counters.late.fetch_add(u64::from(late), Ordering::Relaxed);
    }

    #[test]
    fn finish_without_a_job_is_a_no_op() {
        // Idle (nothing ran this cycle): nothing is recorded, nothing logged.
        begin_call();
        finish_call(true, 99, false);
        AGG.with(|a| assert_eq!(a.borrow().job.pushed, 0));
    }

    #[test]
    fn render_then_finish_counts_one_call() {
        begin_call();
        record_render(Duration::from_micros(1234));
        record_native(NativeCost {
            lib_us: 400,
            comp_us: 800,
            lib_cpu_us: 390,
            comp_cpu_us: 700,
            images: 12,
            image_pixels: 345_678,
            composed: true,
        });
        record_job(Duration::from_micros(2000), false, RenderScale::Full);
        finish_call(true, 64, false);
        AGG.with(|a| {
            let a = a.borrow();
            assert_eq!(
                (a.calls, a.changed, a.max_us, a.job_max_us, a.pub_bytes),
                (1, 1, 1234, 2000, 64)
            );
            assert_eq!(
                (a.lib.p95(), a.comp.p95(), a.images.p95(), a.image_kpx.p95()),
                (400, 800, 12, 345)
            );
            assert_eq!(
                (a.lib_cpu.p95(), a.comp_cpu.p95()),
                (390, 700),
                "the CPU rings are fed from their own fields"
            );
        });
        // The slots were consumed: a second finish does not double count.
        finish_call(false, 0, false);
        AGG.with(|a| assert_eq!((a.borrow().calls, a.borrow().lib.pushed), (1, 1)));
    }

    #[test]
    fn a_cancelled_cycles_sample_does_not_leak_into_the_next() {
        AGG.with(|a| *a.borrow_mut() = Aggregator::new());
        // Cycle 1 timed a render, then the epoch changed: it never reached finish_call.
        begin_call();
        record_render(Duration::from_micros(9));
        record_native(NativeCost { lib_us: 4, comp_us: 5, ..NativeCost::default() });
        record_job(Duration::from_micros(9), false, RenderScale::Full);
        // Cycle 2 made no native call at all.
        begin_call();
        finish_call(false, 0, false);
        AGG.with(|a| {
            let a = a.borrow();
            assert_eq!((a.calls, a.job.pushed, a.lib.pushed, a.comp.pushed), (0, 0, 0, 0));
        });
    }
}
