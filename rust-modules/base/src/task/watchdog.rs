//! Independent observation of loop progress, including blocks without an instrumented label.
//!
//! The existing heartbeat counts a resetting one-second window at the iteration's tail. This
//! counter advances at the entrance, even for iterations that never reach heartbeat or present.
//! Only the observer reads a clock: elapsed times have the coarse sampling interval's precision.
//! Timing is armed by the first completed present, excluding initial renderer warm-up. Later
//! freezes remain observable without another present; resumption logs their total sampled time.
//! Label pointers always name immutable static descriptors, never a guard on the stack.

use super::BlockingLabel;
use std::{
    cell::Cell,
    marker::PhantomData,
    rc::Rc,
    sync::{atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicUsize, Ordering}, OnceLock},
    time::{Duration, Instant},
};

/// A loop iteration over this long is a main-thread hang (also `heartbeat::health`'s stall edge).
pub const HANG_THRESHOLD_MS: u64 = 250;
const POLL_MS: u64 = 100;
/// A late observer cannot tell a stopped process, a system suspend, a debugger stop or its own
/// scheduling starvation from a main-thread stall, so an interval longer than this is discarded.
const MAX_POLL_GAP_MS: u64 = 4 * POLL_MS;

/// **A FREEZE** is the main loop making no progress for this long while the app is in the
/// foreground, the definition Android's ANR and Datadog's freeze use. It is a second, much higher
/// threshold on the same counter the 250 ms hang line watches, and the only one reported off the
/// device: a hang is a development signal, a freeze is a field one.
pub const FREEZE_THRESHOLD_MS: u64 = 5_000;
/// The most probes sent to the main thread in one process. Each can make one blocking call in
/// closed library code return early (`EINTR`), so the number is kept small; a loop that freezes
/// more often than this has been reported by its first few.
const MAX_FREEZE_PROBES: u32 = 4;

/// How long a freeze lasted, in the three buckets the report carries. **Never the raw
/// milliseconds**: a duration to the millisecond is a fingerprint of one television's stall.
/// `Unrecovered` is never written by the process; it is what a record with no recovery line means
/// to the importer (the process was killed, or died, while still frozen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeBucket {
    Short,
    Medium,
    Long,
    Unrecovered,
}

impl FreezeBucket {
    /// The bucket for a measured stall of `ms` (at least [`FREEZE_THRESHOLD_MS`] by construction).
    pub fn of_ms(ms: u64) -> Self {
        match ms {
            0..=9_999 => Self::Short,
            10_000..=29_999 => Self::Medium,
            _ => Self::Long,
        }
    }

    /// The word the crash log and the report carry. One table, read by both the writer here and
    /// the importer (`telemetry::crashreport`), so a bucket on the wire is always one of these four.
    pub fn word(self) -> &'static str {
        match self {
            Self::Short => "5-10s",
            Self::Medium => "10-30s",
            Self::Long => "30s+",
            Self::Unrecovered => "unrecovered",
        }
    }

    /// The inverse of [`word`](Self::word) for the three buckets a recovery line can name.
    pub fn from_word(word: &str) -> Option<Self> {
        [Self::Short, Self::Medium, Self::Long].into_iter().find(|b| b.word() == word)
    }
}

/// The crash-log line that closes a freeze: `freeze-end: seq=<n> bucket=<word>`. `seq` is the
/// number the tracer's `*** FREEZE seq=<n>` record carries (the n-th probe this process sent).
pub fn freeze_end_record(seq: u32, bucket: FreezeBucket) -> String {
    format!("freeze-end: seq={seq} bucket={}", bucket.word())
}

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Began { ms: u64, label: &'static str },
    Ended { ms: u64, label: &'static str },
}

impl Event {
    fn line(&self) -> String {
        match self {
            Self::Began { ms, label } => format!("main-thread hang: {ms}ms in {label}"),
            Self::Ended { ms, label } => format!("main-thread hang ended: {ms}ms in {label}"),
        }
    }
}

/// The observer owns time; the frame thread only increments a counter. None means no active loop.
#[derive(Default)]
struct Detector {
    advanced: Option<(usize, u64)>,
    announced: Option<&'static str>,
}

impl Detector {
    fn observe(&mut self, now: u64, progress: Option<usize>, label: &'static str) -> Option<Event> {
        let Some(counter) = progress else {
            let elapsed = self.advanced.take().map(|(_, at)| now.saturating_sub(at));
            return self.announced.take().map(|label| Event::Ended { ms: elapsed.unwrap_or(0), label });
        };
        let Some((previous, at)) = self.advanced else {
            self.advanced = Some((counter, now));
            return None;
        };
        let elapsed = now.saturating_sub(at);
        if counter != previous {
            self.advanced = Some((counter, now));
            return self.announced.take().map(|label| Event::Ended { ms: elapsed, label });
        }
        if elapsed > HANG_THRESHOLD_MS && self.announced.is_none() {
            self.announced = Some(label);
            return Some(Event::Began { ms: elapsed, label });
        }
        None
    }
}

/// Developer policy layered over the release detector; no clock, UI or signal side effects.
#[cfg(feature = "threadcheck")]
#[derive(Default)]
struct DevObserver {
    last_poll: Option<u64>,
    detector: Detector,
    kill: super::runtime_check::KillLatch,
    /// The last sample taken inside a GPU phase on a software renderer (see `poll`).
    gpu_sampled_at: Option<u64>,
}
#[cfg(feature = "threadcheck")]
struct Observation {
    event: Option<Event>,
    kill: bool,
    reset_warning: bool,
    elapsed: Option<u64>,
}
#[cfg(feature = "threadcheck")]
impl DevObserver {
    fn poll(&mut self, now: u64, progress: Option<usize>, label: &'static str, policy: super::runtime_check::Policy) -> Observation {
        // A late observer cannot distinguish a stopped process, system suspend, debugger stop
        // or its own scheduling starvation from a main-thread stall. Discard that interval.
        if self.last_poll.replace(now).is_some_and(|previous| now.saturating_sub(previous) > MAX_POLL_GAP_MS) {
            self.detector = Detector { advanced: progress.map(|counter| (counter, now)), announced: None };
            self.kill.poll(None, false, policy);
            self.gpu_sampled_at = None;
            return Observation { event: None, kill: false, reset_warning: true, elapsed: None };
        }
        let event = self.detector.observe(now, progress, label);
        let elapsed = self.detector.announced.and_then(|_| self.detector.advanced.map(|(_, at)| now.saturating_sub(at)));
        let gpu_phase = GPU_LABELS.contains(&label);
        // On a CPU rasterizer the fatal budget is the stall's time OUTSIDE GPU phases: it runs
        // from the later of the stall's start and the last GPU-phase sample, so a 2100 ms
        // software draw followed by ordinary post-draw work is not killed on its first
        // unlabelled sample. Hardware GL keeps the whole stall as the budget.
        if policy.software_gl && gpu_phase { self.gpu_sampled_at = Some(now); }
        let budget = elapsed.map(|ms| match (policy.software_gl, self.gpu_sampled_at) {
            (true, Some(at)) => ms.min(now.saturating_sub(at)),
            _ => ms,
        });
        Observation { event, kill: self.kill.poll(budget, gpu_phase, policy), reset_warning: false, elapsed }
    }
}

static UNLABELED: BlockingLabel = BlockingLabel::new("unlabeled");

struct Signals {
    active: AtomicBool,
    presented: AtomicBool,
    /// Whether the app is in the foreground. A freeze is only ever reported in the foreground: a
    /// backgrounded app is parked by design (its window is gone, its decoder released) and the loop
    /// may legitimately idle or block for as long as the system keeps it there.
    foreground: AtomicBool,
    progress: AtomicUsize,
    label: AtomicPtr<BlockingLabel>,
}

impl Signals {
    const fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            presented: AtomicBool::new(false),
            foreground: AtomicBool::new(true),
            progress: AtomicUsize::new(0),
            label: AtomicPtr::new(&UNLABELED as *const BlockingLabel as *mut BlockingLabel),
        }
    }

    fn progress(&self) -> Option<usize> {
        (self.active.load(Ordering::Acquire) && self.presented.load(Ordering::Acquire))
            .then(|| self.progress.load(Ordering::Acquire))
    }

    fn publish_label(&self, label: &'static BlockingLabel) {
        self.label.store(label as *const BlockingLabel as *mut BlockingLabel, Ordering::Release);
    }

    fn label(&self) -> &'static str {
        // Only new()/publish_label() write this pointer, and both require immutable static
        // storage. The descriptor (including the str's pointer and length) never changes or
        // disappears when a scope ends; the observer needs no lock or stack-pointer dereference.
        unsafe { &*self.label.load(Ordering::Acquire) }.text
    }
}

static SIGNALS: Signals = Signals::new();

/// The app's foreground state, written by the loop at its `LIFECYCLE` edges. Starts true.
pub fn set_foreground(foreground: bool) {
    SIGNALS.foreground.store(foreground, Ordering::Release);
}

thread_local! {
    /// Other workers may enter blocking scopes, but cannot publish the frame thread's label.
    static WATCHED: Cell<Option<&'static Signals>> = const { Cell::new(None) };
    static LABEL: Cell<&'static BlockingLabel> = const { Cell::new(&UNLABELED) };
}

pub struct LabelScope {
    previous: &'static BlockingLabel,
    _thread: PhantomData<Rc<()>>,
}

pub(super) fn enter_label(label: &'static BlockingLabel) -> LabelScope {
    let previous = LABEL.with(|current| current.replace(label));
    WATCHED.with(|watched| {
        if let Some(signals) = watched.get() { signals.publish_label(label); }
    });
    LabelScope { previous, _thread: PhantomData }
}

// Dedicated label-only entry points: neither asserts nor grants permission to block. The GPU
// texts are also how the fatal policy recognises a GPU phase (`runtime_check::Issue::Hang`):
// every stretch of the frame whose cost is the GL implementation's — drawing, a framebuffer
// readback, the swap — carries one of them, or a CPU rasterizer's time lands as a CPU label. The
// main loop's other phases (`LoopPhase`) name where a CPU hang landed and are NOT in
// `GPU_LABELS`, so the policy treats each exactly as it treats `unlabeled`.
#[cfg(feature = "threadcheck")]
const DRAW_LABEL: &str = "frame draw";
#[cfg(feature = "threadcheck")]
const READBACK_LABEL: &str = "gl readback";
#[cfg(feature = "threadcheck")]
const PRESENT_LABEL: &str = "gl present";
#[cfg(feature = "threadcheck")]
const GPU_LABELS: [&str; 3] = [DRAW_LABEL, READBACK_LABEL, PRESENT_LABEL];
#[cfg(feature = "threadcheck")]
pub fn draw_scope() -> LabelScope {
    enter_label(const { &BlockingLabel::new(DRAW_LABEL) })
}
/// A dev capture-stream grab or a simulator screenshot: `glReadPixels` drains the pipeline.
#[cfg(feature = "threadcheck")]
pub fn readback_scope() -> LabelScope {
    enter_label(const { &BlockingLabel::new(READBACK_LABEL) })
}
#[cfg(feature = "threadcheck")]
pub fn present_scope() -> LabelScope {
    enter_label(const { &BlockingLabel::new(PRESENT_LABEL) })
}

/// The CPU phases of one main-loop iteration, so a hang line names where it landed
/// (`main-thread hang: 303ms in land results`) instead of reading `unlabeled`. They are not a
/// third naming of the loop: each text is published by a mark or span the loop already has —
/// a heartbeat leg (`heartbeat::Instruments::mark` opens the leg that follows its `Phase`) or a
/// named FRAMEDROP span (`diag::spans::span`, via [`span_scope`]). Label-only, like `draw_scope`:
/// no assertion, no permission to block, and none is a GPU phase, so the fatal policy sees each
/// exactly as it sees `unlabeled`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopPhase {
    EventIngest,
    Results,
    DevScenarios,
    PlaybackPump,
    LandResults,
    Navcommit,
    DispatcherFrame,
    Update,
    PrepareWindow,
    PresentAndSwap,
    FrameReport,
}

impl LoopPhase {
    pub const ALL: [LoopPhase; 11] = [
        Self::EventIngest, Self::Results, Self::DevScenarios, Self::PlaybackPump, Self::LandResults,
        Self::Navcommit, Self::DispatcherFrame, Self::Update, Self::PrepareWindow,
        Self::PresentAndSwap, Self::FrameReport,
    ];

    #[cfg(feature = "threadcheck")]
    const fn descriptor(self) -> &'static BlockingLabel {
        match self {
            Self::EventIngest => const { &BlockingLabel::new("event ingest") },
            Self::Results => const { &BlockingLabel::new("results") },
            Self::DevScenarios => const { &BlockingLabel::new("dev scenarios") },
            Self::PlaybackPump => const { &BlockingLabel::new("playback pump") },
            Self::LandResults => const { &BlockingLabel::new("land results") },
            Self::Navcommit => const { &BlockingLabel::new("navcommit") },
            Self::DispatcherFrame => const { &BlockingLabel::new("dispatcher frame") },
            Self::Update => const { &BlockingLabel::new("update") },
            Self::PrepareWindow => const { &BlockingLabel::new("prepare window") },
            Self::PresentAndSwap => const { &BlockingLabel::new("present and swap") },
            Self::FrameReport => const { &BlockingLabel::new("frame report") },
        }
    }

    /// The phase a FRAMEDROP span of this name stands for, if the loop labels it.
    #[cfg(feature = "threadcheck")]
    fn of_span(name: &str) -> Option<Self> {
        Some(match name {
            "scen" => Self::DevScenarios,
            "pump" => Self::PlaybackPump,
            "land" => Self::LandResults,
            "dframe" => Self::DispatcherFrame,
            _ => return None,
        })
    }
}

/// `diag::spans::span`'s label: for the named spans the loop labels, the phase's text for the
/// duration of the guard; any other span name publishes nothing.
#[cfg(feature = "threadcheck")]
pub fn span_scope(name: &str) -> Option<LabelScope> {
    LoopPhase::of_span(name).map(|phase| enter_label(phase.descriptor()))
}

/// The heartbeat leg currently open. `enter` closes the previous leg and opens the next; the last
/// stays open until the next `enter`. Owned by `heartbeat::Instruments`, whose `mark` is the only
/// caller. Without `threadcheck` this is a zero-sized no-op.
#[derive(Default)]
pub struct LoopPhases {
    #[cfg(feature = "threadcheck")]
    open: Option<LabelScope>,
}

impl LoopPhases {
    #[inline]
    #[cfg_attr(not(feature = "threadcheck"), allow(unused_variables))]
    pub fn enter(&mut self, phase: LoopPhase) {
        #[cfg(feature = "threadcheck")]
        {
            self.open = None; // restores the label that was current before the first leg
            self.open = Some(enter_label(phase.descriptor()));
        }
    }
}

impl Drop for LabelScope {
    fn drop(&mut self) {
        LABEL.with(|current| current.set(self.previous));
        WATCHED.with(|watched| {
            if let Some(signals) = watched.get() { signals.publish_label(self.previous); }
        });
    }
}

/// The app loop owns this guard. Tests are disabled by default and may explicitly attach private
/// signals to exercise publication without creating a thread or waiting for real time.
pub struct LoopWatch {
    signals: Option<&'static Signals>,
    previous: Option<&'static Signals>,
    _thread: PhantomData<Rc<()>>,
}

impl LoopWatch {
    pub fn start() -> Self {
        if cfg!(any(test, feature = "test-support")) { return Self::disabled(); }
        Self::start_enabled()
    }

    fn disabled() -> Self {
        Self { signals: None, previous: None, _thread: PhantomData }
    }

    fn start_enabled() -> Self {
        static STARTED: OnceLock<bool> = OnceLock::new();
        let started = STARTED.get_or_init(|| {
            // task::spawn uses std::thread::Builder::spawn and logs EAGAIN as an error return.
            // Exactly one observer lives for this process; a failed start stays disabled.
            //
            // The loop owner's thread is captured HERE, on the loop owner, never inside the
            // observer: it is the target of the freeze probe (and of the developer build's fatal
            // abort). libc supplies the native pthread_t ABI (pointer on Darwin, unsigned long on
            // ARM Linux); fwcompat inventories export `pthread_self` and `pthread_kill` at
            // GLIBC_2.4 across all releases: libpthread on older firmware, libc after the glibc
            // pthread merge.
            let main_thread = unsafe { libc::pthread_self() } as usize;
            #[cfg(feature = "threadcheck")]
            let log_only = crate::devtrig::guard_log_only();
            #[cfg(feature = "threadcheck")]
            let started = super::spawn("main-thread watchdog", move || observe_loop(main_thread, log_only)).is_some();
            #[cfg(not(feature = "threadcheck"))]
            let started = super::spawn("main-thread watchdog", move || observe_loop(main_thread)).is_some();
            if !started { crate::eventlog::log("main-thread watchdog: unavailable; hang detection disabled"); }
            started
        });
        if !started { return Self::disabled(); }
        Self::attach(&SIGNALS)
    }

    fn attach(signals: &'static Signals) -> Self {
        if signals.active.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return Self::disabled();
        }
        signals.presented.store(false, Ordering::Release);
        signals.publish_label(LABEL.with(Cell::get));
        signals.progress.fetch_add(1, Ordering::Release);
        let previous = WATCHED.with(|watched| watched.replace(Some(signals)));
        Self { signals: Some(signals), previous, _thread: PhantomData }
    }

    /// Arm only after the first swap returns; shader/font warm-up may legitimately exceed 250 ms.
    pub fn presented(&self) {
        if let Some(signals) = self.signals {
            signals.presented.store(true, Ordering::Release);
        }
    }

    /// One native-word atomic increment, independent of presentation, replay time or heartbeat.
    #[inline]
    pub fn advance(&self) {
        if let Some(signals) = self.signals { signals.progress.fetch_add(1, Ordering::Release); }
    }
}

impl Drop for LoopWatch {
    fn drop(&mut self) {
        if let Some(signals) = self.signals {
            signals.active.store(false, Ordering::Release);
            signals.publish_label(&UNLABELED);
            WATCHED.with(|watched| watched.set(self.previous));
        }
    }
}

/// What the freeze tracker asks the observer to do.
#[derive(Debug, PartialEq, Eq)]
enum FreezeEvent {
    /// The loop has stood still for [`FREEZE_THRESHOLD_MS`]: capture the main thread now.
    Probe { seq: u32, ms: u64 },
    /// The loop made progress again after probe `seq`, `ms` after it last had.
    Recovered { seq: u32, ms: u64 },
}

/// The 5-second tracker, fed the same `(now, progress)` samples as the 250 ms [`Detector`] and
/// owning no clock. It adds the two rules a field report needs that a development hang line does
/// not: the app must be in the FOREGROUND, and a sample interval the observer itself was late for
/// (a stopped or suspended process) is discarded, which `DevObserver` already did and the release
/// path did not. A freeze is probed once, and at most [`MAX_FREEZE_PROBES`] times per process.
#[derive(Default)]
struct Freezes {
    /// The progress counter and when it was last seen to move.
    sample: Option<(usize, u64)>,
    last_poll: Option<u64>,
    probes: u32,
    /// The probe of the freeze in progress, until progress resumes.
    open: Option<u32>,
    /// Stalled time already counted before the last observer gap, for the freeze in progress.
    carried: u64,
}

impl Freezes {
    /// The probe just announced was not sent: it never reached the tracer, so it is not counted.
    /// The freeze stays open (it is not retried: nothing changed that could make a send work).
    fn unsent(&mut self) {
        self.probes = self.probes.saturating_sub(1);
    }

    fn observe(&mut self, now: u64, progress: Option<usize>, foreground: bool) -> Option<FreezeEvent> {
        let previous_poll = self.last_poll.replace(now);
        let late = previous_poll.is_some_and(|p| now.saturating_sub(p) > MAX_POLL_GAP_MS);
        let active = progress.filter(|_| foreground);
        if late {
            // The interval proves nothing about the loop, so it is never counted as freeze time and
            // never starts a second probe: a freeze in flight stays the SAME freeze while the
            // counter is unchanged, and its time so far is carried over the gap. If the counter
            // moved (or the app left the foreground) during the gap the freeze did end, and the
            // time before the gap is the lower bound of how long it lasted.
            let before = self.carried
                + match (self.sample, previous_poll) {
                    (Some((_, at)), Some(p)) => p.saturating_sub(at),
                    _ => 0,
                };
            let unchanged = matches!((active, self.sample), (Some(c), Some((s, _))) if c == s);
            self.sample = active.map(|counter| (counter, now));
            if unchanged {
                self.carried = if self.open.is_some() { before } else { 0 };
                return None;
            }
            self.carried = 0;
            return self.open.take().map(|seq| FreezeEvent::Recovered { seq, ms: before });
        }
        let Some(counter) = active else {
            // Backgrounded, or no loop at all: not a freeze. A freeze in flight ended with the
            // loop running far enough to flip the flag.
            let recovered = self.open.take().zip(self.sample).map(|(seq, (_, at))| FreezeEvent::Recovered {
                seq,
                ms: self.carried + now.saturating_sub(at),
            });
            self.sample = None;
            self.carried = 0;
            return recovered;
        };
        let Some((previous, at)) = self.sample else {
            self.sample = Some((counter, now));
            return None;
        };
        let elapsed = self.carried + now.saturating_sub(at);
        if counter != previous {
            self.sample = Some((counter, now));
            self.carried = 0;
            return self.open.take().map(|seq| FreezeEvent::Recovered { seq, ms: elapsed });
        }
        if elapsed >= FREEZE_THRESHOLD_MS && self.open.is_none() && self.probes < MAX_FREEZE_PROBES {
            self.probes += 1;
            self.open = Some(self.probes);
            return Some(FreezeEvent::Probe { seq: self.probes, ms: elapsed });
        }
        None
    }
}

/// The probe number the tracer's `freeze_handler` (`src/crashtrace.c`) records, read there as
/// `plx_freeze_seq`. [`probe_main_thread`] stores the number right before it signals; the handler
/// takes it back to 0 and writes it as the record's `seq=`. A `SIGUSR2` that finds 0 here (a stray
/// one from this process) is not a probe and writes nothing, so the record numbers and the
/// `freeze-end: seq=` lines this thread writes cannot drift apart.
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static plx_freeze_seq: AtomicU32 = AtomicU32::new(0);

/// How far the process has got, as the tracer's fault handler (`src/crashtrace.c`) reads it from
/// `plx_crash_phase` and writes it on the crash record's `phase:` line. Rust owns the storage (like
/// [`plx_freeze_seq`]) so the C file links without Rust and Rust without the C file; the values
/// are `PLX_PHASE_*` in `src/crashfmt.h`. A crash in the teardown or in the last flushes before
/// `_exit` is a different bug from one in the running app, and this is the only thing that tells
/// them apart.
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static plx_crash_phase: AtomicU32 = AtomicU32::new(0);

/// The values [`plx_crash_phase`] takes; keep in step with `PLX_PHASE_*` in `src/crashfmt.h`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashPhase {
    /// The main loop, or anything before it.
    Running = 0,
    /// The loop has ended and the teardown is running.
    Shutdown = 1,
    /// The teardown is over; the process is ending. Set by `process::exit_now`, which runs no
    /// `atexit` handler or static destructor.
    Exiting = 2,
}

/// Record the process phase for the crash tracer. One relaxed store; async-signal-safe to read.
pub fn set_crash_phase(phase: CrashPhase) {
    plx_crash_phase.store(phase as u32, Ordering::Relaxed);
}

/// Send the freeze probe `seq` to the main thread, if there is a handler to answer it.
///
/// The probe is `SIGUSR2` (`src/crashtrace.c` says why that number and what the handler does).
/// **It is sent only when the tracer's handler is installed**: a `SIGUSR2` with the default
/// disposition would terminate the process, which is exactly what a freeze report must never do,
/// and the simulators and the host test harness have no tracer installed. The disposition query
/// says which function is installed and [`handler_is_ours`] says whether it is in this binary,
/// which is the tracer's `freeze_handler` and nothing else, so a library's handler (ACB's, loaded
/// at playback) is never signalled. Returns whether the signal was sent.
fn probe_main_thread(main_thread: usize, seq: u32) -> bool {
    // SAFETY: `sigaction` with a null new action only reads the current disposition into `old`,
    // a zeroed plain-data struct; `pthread_kill` on a thread id captured from the loop owner,
    // which lives for the whole process, is the call the developer build's abort already makes.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGUSR2, std::ptr::null(), &mut old) != 0 {
            return false;
        }
        if old.sa_sigaction == libc::SIG_DFL || old.sa_sigaction == libc::SIG_IGN {
            return false;
        }
        if !handler_is_ours(old.sa_sigaction) {
            return false;
        }
        plx_freeze_seq.store(seq, Ordering::Release);
        if libc::pthread_kill(main_thread as libc::pthread_t, libc::SIGUSR2) == 0 {
            return true;
        }
        plx_freeze_seq.store(0, Ordering::Release);
        false
    }
}

/// Whether `handler` lives in the same loaded object as this code, i.e. is the crash tracer's own
/// `freeze_handler` and not a library's that took `SIGUSR2` after the tracer was installed
/// (ACB is `dlopen`ed at playback). A stranger's handler fired at a random moment is not a probe.
/// An unanswerable `dladdr` reads as "not ours": no probe is better than a wrong one.
fn handler_is_ours(handler: libc::sighandler_t) -> bool {
    // SAFETY: `dladdr` writes into zeroed plain-data `Dl_info` structs and only reads the
    // addresses it is given; neither is dereferenced.
    unsafe {
        let mut theirs: libc::Dl_info = std::mem::zeroed();
        let mut mine: libc::Dl_info = std::mem::zeroed();
        let here = handler_is_ours as *const () as *const libc::c_void;
        libc::dladdr(handler as *const libc::c_void, &mut theirs) != 0
            && libc::dladdr(here, &mut mine) != 0
            && theirs.dli_fbase == mine.dli_fbase
    }
}

fn observe_loop(main_thread: usize, #[cfg(feature = "threadcheck")] log_only: bool) {
    let origin = Instant::now();
    let mut freezes = Freezes::default();
    // The probe whose recovery this thread must write down: set only when the probe was really
    // sent, so a stall nothing could be recorded for never claims a record it does not have.
    let mut probed: Option<u32> = None;
    #[cfg(not(feature = "threadcheck"))]
    let mut detector = Detector::default();
    #[cfg(feature = "threadcheck")]
    let mut observer = DevObserver::default();
    loop {
        // A new iteration's label publication follows its progress increment. Read in this
        // order so that label cannot be paired with older progress, and timestamp afterwards
        // so a delayed observer does not backdate the progress it just saw.
        let label = SIGNALS.label();
        let progress = SIGNALS.progress();
        let now = origin.elapsed().as_millis().min(u64::MAX as u128) as u64;
        #[cfg(feature = "threadcheck")]
        let observation = observer.poll(now, progress, label, super::runtime_check::policy(log_only));
        #[cfg(feature = "threadcheck")]
        if observation.reset_warning { super::runtime_check::reset_warning(); }
        #[cfg(feature = "threadcheck")]
        let event = observation.event;
        #[cfg(not(feature = "threadcheck"))]
        let event = detector.observe(now, progress, label);
        match freezes.observe(now, progress, SIGNALS.foreground.load(Ordering::Acquire)) {
            Some(FreezeEvent::Probe { seq, ms }) => {
                let sent = probe_main_thread(main_thread, seq);
                if !sent {
                    freezes.unsent();
                }
                crate::eventlog::log(&format!("main-thread freeze: {ms}ms without loop progress (probe {seq}, recorded={sent})"));
                probed = sent.then_some(seq);
            }
            Some(FreezeEvent::Recovered { seq, ms }) => {
                crate::eventlog::log(&format!("main-thread freeze ended: {ms}ms (probe {seq})"));
                if probed.take() == Some(seq) {
                    let record = freeze_end_record(seq, FreezeBucket::of_ms(ms));
                    let _ = crate::eventlog::append_record(
                        &crate::paths::in_runtime_dir(crate::paths::runtime_file::CRASH),
                        &record,
                    );
                }
            }
            None => {}
        }
        if let Some(event) = event {
            crate::eventlog::log(&event.line());
            #[cfg(feature = "threadcheck")]
            match event {
                Event::Began { ms, label } => super::runtime_check::hang(ms, label),
                Event::Ended { ms, .. } => super::runtime_check::end(ms),
            }
        }
        #[cfg(feature = "threadcheck")]
        {
            if observation.kill {
                crate::eventlog::log(&format!("main-thread hang fatal: {}ms in {}; sending SIGABRT to main thread", observation.elapsed.unwrap(), label));
                // Recheck progress after logging: recovery while the observer was delayed must
                // not kill a healthy loop. The loop owner is the process-lifetime main thread.
                // Logging can itself be delayed by a process stop. Do not signal using a
                // stale sample; the next poll will rebase and discard its warning as well.
                let sample_age = origin.elapsed().as_millis().saturating_sub(now as u128);
                if sample_age <= MAX_POLL_GAP_MS as u128 && SIGNALS.progress() == progress {
                    let rc = unsafe { libc::pthread_kill(main_thread as libc::pthread_t, libc::SIGABRT) };
                    if rc != 0 { crate::eventlog::log(&format!("main-thread hang: pthread_kill failed: {rc}")); }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "threadcheck")]
    use super::super::runtime_check::Policy;
    #[cfg(feature = "threadcheck")]
    const HW: Policy = Policy { log_only: false, software_gl: false };

    fn signals() -> &'static Signals { Box::leak(Box::new(Signals::new())) }

    #[test]
    fn the_host_harness_does_not_start_or_advance_the_watchdog() {
        let before = SIGNALS.progress.load(Ordering::Relaxed);
        let watch = LoopWatch::start();
        assert!(watch.signals.is_none());
        watch.advance();
        assert_eq!(SIGNALS.progress.load(Ordering::Relaxed), before);
        assert_eq!(SIGNALS.progress(), None);
    }

    #[test]
    fn an_opted_in_loop_publishes_progress_and_disarms_on_exit() {
        let signals = signals();
        assert_eq!(signals.progress(), None);
        let watch = LoopWatch::attach(signals);
        watch.presented();
        assert_eq!(signals.progress(), Some(1));
        watch.advance();
        watch.advance();
        assert_eq!(signals.progress(), Some(3));
        drop(watch);
        assert_eq!(signals.progress(), None);
        assert_eq!(signals.label(), "unlabeled");
    }

    #[test]
    fn first_present_warmup_does_not_start_a_hang_timer() {
        let signals = signals();
        let watch = LoopWatch::attach(signals);
        let mut detector = Detector::default();
        assert_eq!(detector.observe(0, signals.progress(), signals.label()), None);
        assert_eq!(detector.observe(5_000, signals.progress(), signals.label()), None,
            "initial shader/font warm-up is outside the loop hang budget");
        watch.presented();
        assert_eq!(detector.observe(5_001, signals.progress(), signals.label()), None);
        assert!(matches!(detector.observe(5_252, signals.progress(), signals.label()), Some(Event::Began { .. })));
        watch.advance();
        assert!(matches!(detector.observe(6_000, signals.progress(), signals.label()), Some(Event::Ended { ms: 999, .. })));
    }

    #[test]
    fn nested_blocking_and_allowed_labels_restore_after_unwind() {
        let signals = signals();
        let _watch = LoopWatch::attach(signals);
        let outer = super::super::assert_may_block(const { &BlockingLabel::new("outer call") });
        assert_eq!(signals.label(), "outer call");
        let result = std::panic::catch_unwind(|| {
            let _allowed = super::super::allow_blocking(const { &BlockingLabel::new("explicit reason") });
            assert_eq!(signals.label(), "explicit reason");
            {
                let _inner = super::super::assert_may_block(const { &BlockingLabel::new("inner call") });
                assert_eq!(signals.label(), "inner call");
            }
            assert_eq!(signals.label(), "explicit reason");
            panic!("unwind the allowance");
        });
        assert!(result.is_err());
        assert_eq!(signals.label(), "outer call");
        drop(outer);
        assert_eq!(signals.label(), "unlabeled");
    }

    #[test]
    fn a_scope_already_open_at_loop_start_supplies_its_label() {
        let outer = super::super::allow_blocking(const { &BlockingLabel::new("outer allowance") });
        let signals = signals();
        let watch = LoopWatch::attach(signals);
        assert_eq!(signals.label(), "outer allowance");
        drop(outer);
        assert_eq!(signals.label(), "unlabeled");
        drop(watch);
    }

    #[test]
    fn worker_scopes_cannot_replace_the_watched_threads_label() {
        let signals = signals();
        let _watch = LoopWatch::attach(signals);
        let _outer = super::super::assert_may_block(const { &BlockingLabel::new("frame thread") });
        // This exercises TLS isolation only. The detector tests below use synthetic timestamps,
        // and no test starts the observer thread or sleeps to manufacture a stall.
        let observed = std::thread::Builder::new().spawn(move || {
            let _worker = super::super::assert_may_block(const { &BlockingLabel::new("background call") });
            signals.label()
        }).unwrap().join().unwrap();
        assert_eq!(observed, "frame thread");
        assert_eq!(signals.label(), "frame thread");
    }

    #[test]
    fn an_unlabeled_stall_starts_only_after_the_threshold() {
        let mut detector = Detector::default();
        assert_eq!(detector.observe(0, Some(1), "unlabeled"), None);
        assert_eq!(detector.observe(HANG_THRESHOLD_MS, Some(1), "unlabeled"), None);
        assert_eq!(detector.observe(HANG_THRESHOLD_MS + 1, Some(1), "unlabeled"),
            Some(Event::Began { ms: 251, label: "unlabeled" }));
    }

    #[test]
    fn a_stall_logs_once_and_ends_with_its_original_label_and_total_duration() {
        let mut detector = Detector::default();
        detector.observe(100, Some(8), "unlabeled");
        assert_eq!(detector.observe(351, Some(8), "storage helper transact"),
            Some(Event::Began { ms: 251, label: "storage helper transact" }));
        assert_eq!(detector.observe(500, Some(8), "inner scope"), None);
        assert_eq!(detector.observe(800, Some(8), "unlabeled"), None);
        assert_eq!(detector.observe(900, Some(9), "unlabeled"),
            Some(Event::Ended { ms: 800, label: "storage helper transact" }));
        assert_eq!(detector.observe(916, Some(10), "unlabeled"), None);
    }

    #[test]
    fn progress_rearms_the_next_stall() {
        let mut detector = Detector::default();
        detector.observe(0, Some(0), "first");
        assert_eq!(detector.observe(300, Some(0), "first"),
            Some(Event::Began { ms: 300, label: "first" }));
        assert_eq!(detector.observe(400, Some(1), "unlabeled"),
            Some(Event::Ended { ms: 400, label: "first" }));
        assert_eq!(detector.observe(700, Some(1), "second"),
            Some(Event::Began { ms: 300, label: "second" }));
        assert_eq!(detector.observe(900, Some(2), "unlabeled"),
            Some(Event::Ended { ms: 500, label: "second" }));
    }

    #[test]
    fn a_progressing_loop_and_a_delayed_observer_do_not_report_hangs() {
        let mut detector = Detector::default();
        for counter in 0..20 {
            assert_eq!(detector.observe(counter as u64 * POLL_MS, Some(counter), "unlabeled"), None);
        }
        assert_eq!(detector.observe(60_000, Some(1000), "unlabeled"), None);
    }

    #[test]
    fn stopping_disarms_and_finishes_an_announced_stall() {
        let mut detector = Detector::default();
        assert_eq!(detector.observe(10_000, None, "unlabeled"), None);
        assert_eq!(detector.observe(20_000, Some(1), "unlabeled"), None);
        assert_eq!(detector.observe(20_300, Some(1), "blocked"),
            Some(Event::Began { ms: 300, label: "blocked" }));
        assert_eq!(detector.observe(20_400, None, "unlabeled"),
            Some(Event::Ended { ms: 400, label: "blocked" }));
        assert_eq!(detector.observe(60_000, None, "unlabeled"), None);
        assert_eq!(detector.observe(70_000, Some(2), "unlabeled"), None);
    }

    #[test]
    fn counter_wrap_is_progress() {
        let mut detector = Detector::default();
        detector.observe(0, Some(usize::MAX), "unlabeled");
        assert_eq!(detector.observe(300, Some(usize::MAX), "blocked"),
            Some(Event::Began { ms: 300, label: "blocked" }));
        assert_eq!(detector.observe(400, Some(0), "unlabeled"),
            Some(Event::Ended { ms: 400, label: "blocked" }));
    }

    #[test]
    fn hang_messages_use_the_field_log_format() {
        assert_eq!(Event::Began { ms: 300, label: "unlabeled" }.line(),
            "main-thread hang: 300ms in unlabeled");
        assert_eq!(Event::Ended { ms: 1234, label: "LS2 round trip" }.line(),
            "main-thread hang ended: 1234ms in LS2 round trip");
    }
    #[cfg(feature = "threadcheck")]
    #[test]
    fn a_process_freeze_rebases_without_warning_or_kill() {
        let mut observer = DevObserver::default();
        observer.poll(0, Some(1), "gl present", HW);
        let paused = observer.poll(30_000, Some(1), "gl present", HW);
        assert!(paused.event.is_none(), "a stopped observer cannot establish a main-thread hang");
        assert!(!paused.kill);
        assert!(paused.reset_warning);
        for now in [30_100, 30_200, 30_250] {
            let sample = observer.poll(now, Some(1), "gl present", HW);
            assert!(sample.event.is_none());
            assert!(!sample.kill);
        }
        assert_eq!(observer.poll(30_300, Some(1), "gl present", HW).event,
            Some(Event::Began { ms: 300, label: "gl present" }));
    }

    #[cfg(feature = "threadcheck")]
    #[test]
    fn a_freeze_clears_existing_hang_and_rearms_budget() {
        let mut observer = DevObserver::default();
        for now in (0..=1900).step_by(100) { observer.poll(now, Some(1), "frame draw", HW); }
        let paused = observer.poll(31_900, Some(1), "frame draw", HW);
        assert!(paused.reset_warning);
        assert!(paused.event.is_none());
        assert!(!paused.kill);
        for now in (32_000..33_900).step_by(100) {
            assert!(!observer.poll(now, Some(1), "frame draw", HW).kill);
        }
        assert!(observer.poll(33_900, Some(1), "frame draw", HW).kill);
        assert!(!observer.poll(34_000, Some(1), "frame draw", HW).kill);
    }

    #[cfg(feature = "threadcheck")]
    #[test]
    fn the_poll_gap_boundary_is_strictly_over_four_polls() {
        for (gap, reset) in [(400, false), (401, true)] {
            let mut observer = DevObserver::default();
            observer.poll(0, Some(1), "gl present", HW);
            let sample = observer.poll(gap, Some(1), "gl present", HW);
            assert_eq!(sample.reset_warning, reset);
            assert_eq!(sample.event.is_some(), !reset);
            assert!(!sample.kill);
        }
    }

    #[cfg(feature = "threadcheck")]
    #[test]
    fn gpu_phase_labels_do_not_bypass_or_trip_the_blocking_guard() {
        let signals = signals();
        let _watch = LoopWatch::attach(signals);
        let _frame = super::super::FrameScope::enter();
        {
            let _draw = draw_scope();
            assert_eq!(signals.label(), "frame draw");
            {
                let _present = present_scope();
                assert_eq!(signals.label(), "gl present");
                assert!(std::panic::catch_unwind(|| {
                    super::super::assert_may_block(const { &BlockingLabel::new("still forbidden") })
                }).is_err(), "a phase label must not grant allow_blocking");
            }
            assert_eq!(signals.label(), "frame draw");
        }
        assert_eq!(signals.label(), "unlabeled");
    }

    #[cfg(feature = "threadcheck")]
    #[test]
    fn a_regular_gpu_stall_still_requests_one_kill() {
        for label in ["frame draw", "gl present"] {
            for log_only in [false, true] {
                let mut observer = DevObserver::default();
                let policy = Policy { log_only, software_gl: false };
                for now in (0..=3000).step_by(100) {
                    let sample = observer.poll(now, Some(1), label, policy);
                    assert!(!sample.reset_warning);
                    assert_eq!(sample.kill, now == 2000 && !log_only);
                    assert_eq!(sample.event, (now == 300).then_some(Event::Began { ms: 300, label }));
                }
            }
        }
    }

    /// The CI simulator's abort, end to end through the observer: on a software renderer a
    /// stall sampled in `frame draw` / `gl present` is reported (the `main-thread hang` line
    /// still fires at 300 ms) but never killed; any other label keeps the two-second kill.
    #[cfg(feature = "threadcheck")]
    #[test]
    fn a_software_renderer_gpu_stall_is_reported_but_not_killed() {
        let software = Policy { log_only: false, software_gl: true };
        for label in GPU_LABELS {
            let mut observer = DevObserver::default();
            for now in (0..=5000).step_by(100) {
                let sample = observer.poll(now, Some(1), label, software);
                assert!(!sample.kill, "{label} at {now}ms");
                assert_eq!(sample.event, (now == 300).then_some(Event::Began { ms: 300, label }));
            }
        }
        for label in ["unlabeled", "dev hang probe"] {
            let mut observer = DevObserver::default();
            for now in (0..=3000).step_by(100) {
                assert_eq!(observer.poll(now, Some(1), label, software).kill, now == 2000, "{label} at {now}ms");
            }
        }
    }

    /// Codex review round 1: the stall clock runs from the iteration's start, so a 2100 ms
    /// software draw followed by ordinary post-draw work must not be killed on its first
    /// unlabelled sample. The budget restarts at the last GPU-phase sample; two further seconds
    /// outside a GPU phase are still fatal, and hardware GL still kills at 2000 ms regardless.
    #[cfg(feature = "threadcheck")]
    #[test]
    fn time_in_a_software_gpu_phase_does_not_spend_the_budget() {
        let software = Policy { log_only: false, software_gl: true };
        let mut observer = DevObserver::default();
        for now in (0..=2100).step_by(100) { assert!(!observer.poll(now, Some(1), "frame draw", software).kill); }
        for now in (2200..4100).step_by(100) {
            assert!(!observer.poll(now, Some(1), "unlabeled", software).kill, "post-draw work at {now}ms");
        }
        assert!(observer.poll(4100, Some(1), "unlabeled", software).kill, "2000 ms outside a GPU phase");
        // Progress (the next frame) starts a fresh stall with a fresh budget.
        assert!(!observer.poll(4200, Some(2), "unlabeled", software).kill);
        for now in (4300..6200).step_by(100) { assert!(!observer.poll(now, Some(2), "unlabeled", software).kill); }
        assert!(observer.poll(6200, Some(2), "unlabeled", software).kill);

        let mut hardware = DevObserver::default();
        for now in (0..=1900).step_by(100) { assert!(!hardware.poll(now, Some(1), "frame draw", HW).kill); }
        assert!(hardware.poll(2000, Some(1), "unlabeled", HW).kill);
    }

    #[cfg(feature = "threadcheck")]
    #[test]
    fn a_hang_in_each_loop_phase_reports_that_phase() {
        for phase in LoopPhase::ALL {
            let signals = signals();
            let watch = LoopWatch::attach(signals);
            watch.presented();
            let mut phases = LoopPhases::default();
            phases.enter(phase);
            let label = signals.label();
            assert_eq!(label, phase.descriptor().text);
            let mut detector = Detector::default();
            detector.observe(0, signals.progress(), signals.label());
            let began = detector.observe(300, signals.progress(), signals.label());
            assert_eq!(began, Some(Event::Began { ms: 300, label }));
            assert_eq!(began.unwrap().line(), format!("main-thread hang: 300ms in {label}"));
            drop(phases);
            assert_eq!(signals.label(), "unlabeled", "{label}");
        }
        let labels: Vec<&str> = LoopPhase::ALL.iter().map(|p| p.descriptor().text).collect();
        for (i, a) in labels.iter().enumerate() { assert!(!labels[i + 1..].contains(a), "duplicate {a}"); }
        assert!(labels.contains(&"land results"));
    }

    #[cfg(feature = "threadcheck")]
    #[test]
    fn the_named_loop_spans_publish_their_label_and_restore_it() {
        let signals = signals();
        let _watch = LoopWatch::attach(signals);
        let mut phases = LoopPhases::default();
        phases.enter(LoopPhase::Results);
        for (name, label) in [("scen", "dev scenarios"), ("pump", "playback pump"), ("land", "land results"), ("dframe", "dispatcher frame")] {
            let scope = span_scope(name);
            assert_eq!(signals.label(), label);
            drop(scope);
            assert_eq!(signals.label(), "results");
        }
        assert!(span_scope("text").is_none());
        assert_eq!(signals.label(), "results");
    }

    #[cfg(feature = "threadcheck")]
    #[test]
    fn each_heartbeat_mark_opens_the_next_legs_label() {
        use crate::diag::heartbeat::{Instruments, Phase};
        let signals = signals();
        let watch = LoopWatch::attach(signals);
        let mut instr = Instruments::new(false, 22.0);
        for (mark, label) in [
            (Phase::Top, "event ingest"), (Phase::Ingest, "results"), (Phase::Results, "navcommit"),
            (Phase::NavCommit, "update"), (Phase::TickDrain, "prepare window"),
            (Phase::Prepare, "present and swap"), (Phase::Draw, "present and swap"),
            (Phase::Capture, "present and swap"), (Phase::Swap, "frame report"),
        ] {
            instr.mark(mark);
            assert_eq!(signals.label(), label, "after {mark:?}");
        }
        drop(instr);
        assert_eq!(signals.label(), "unlabeled");
        drop(watch);
    }

    #[cfg(feature = "threadcheck")]
    #[test]
    fn loop_phases_replace_each_other_nest_with_gpu_scopes_and_do_not_allow_blocking() {
        let signals = signals();
        let _watch = LoopWatch::attach(signals);
        let _frame = super::super::FrameScope::enter();
        let mut phases = LoopPhases::default();
        phases.enter(LoopPhase::Update);
        phases.enter(LoopPhase::PresentAndSwap);
        assert_eq!(signals.label(), "present and swap");
        {
            let _draw = draw_scope();
            assert_eq!(signals.label(), "frame draw");
        }
        assert_eq!(signals.label(), "present and swap");
        assert!(std::panic::catch_unwind(|| {
            super::super::assert_may_block(const { &BlockingLabel::new("still forbidden") })
        }).is_err(), "a phase label must not grant allow_blocking");
        drop(phases);
        assert_eq!(signals.label(), "unlabeled");
    }

    /// The fatal policy recognises a GPU phase only by `GPU_LABELS`; a loop phase is a CPU label
    /// and must be killed on the same schedule as `unlabeled`, on hardware and software GL alike.
    #[cfg(feature = "threadcheck")]
    #[test]
    fn a_loop_phase_is_treated_by_the_fatal_policy_exactly_as_unlabeled() {
        for phase in LoopPhase::ALL {
            let label = phase.descriptor().text;
            assert!(!GPU_LABELS.contains(&label), "{label} must not be a GPU label");
            for policy in [HW, Policy { log_only: false, software_gl: true }, Policy { log_only: true, software_gl: false }] {
                let mut phased = DevObserver::default();
                let mut plain = DevObserver::default();
                for now in (0..=5000).step_by(100) {
                    let a = phased.poll(now, Some(1), label, policy);
                    let b = plain.poll(now, Some(1), "unlabeled", policy);
                    assert_eq!((a.kill, a.reset_warning, a.elapsed), (b.kill, b.reset_warning, b.elapsed), "{label} at {now}ms");
                    assert_eq!(a.event.is_some(), b.event.is_some(), "{label} at {now}ms");
                }
            }
        }
    }

    /// Feed the tracker a stall sampled every `POLL_MS` from `from` to `to` inclusive on a frozen
    /// counter, collecting every event it raises.
    fn stall(f: &mut Freezes, from: u64, to: u64, counter: usize, fg: bool) -> Vec<(u64, FreezeEvent)> {
        let mut out = Vec::new();
        let mut now = from;
        while now <= to {
            if let Some(e) = f.observe(now, Some(counter), fg) { out.push((now, e)); }
            now += POLL_MS;
        }
        out
    }

    #[test]
    fn a_five_second_stall_is_probed_once_and_its_recovery_timed() {
        let mut f = Freezes::default();
        let events = stall(&mut f, 0, 8_000, 1, true);
        assert_eq!(events, vec![(5_000, FreezeEvent::Probe { seq: 1, ms: 5_000 })], "probed once, at the threshold");
        assert_eq!(f.observe(8_100, Some(2), true), Some(FreezeEvent::Recovered { seq: 1, ms: 8_100 }));
        assert_eq!(f.observe(8_200, Some(2), true), None, "recovery is reported once");
    }

    #[test]
    fn four_seconds_nine_hundred_is_not_a_freeze() {
        let mut f = Freezes::default();
        assert!(stall(&mut f, 0, 4_900, 1, true).is_empty());
        assert_eq!(f.observe(5_000, Some(2), true), None, "progress at the edge cancels it");
    }

    #[test]
    fn a_backgrounded_app_is_never_a_freeze() {
        let mut f = Freezes::default();
        assert!(stall(&mut f, 0, 60_000, 1, false).is_empty());
        // Foreground returns with the counter unchanged: the clock starts then, not at the stall's
        // old origin.
        assert!(stall(&mut f, 60_100, 65_000, 1, true).is_empty());
        assert_eq!(stall(&mut f, 65_100, 65_100, 1, true).len(), 1);
    }

    #[test]
    fn no_loop_or_no_first_present_is_not_a_freeze() {
        let mut f = Freezes::default();
        for now in (0..=20_000).step_by(100) {
            assert_eq!(f.observe(now, None, true), None);
        }
    }

    #[test]
    fn a_late_observer_discards_the_interval() {
        let mut f = Freezes::default();
        assert!(stall(&mut f, 0, 4_000, 1, true).is_empty());
        // A stopped or suspended process: the next sample is far later with the counter unmoved.
        assert_eq!(f.observe(40_000, Some(1), true), None, "a 36 s sampling gap is not a 36 s freeze");
        assert!(stall(&mut f, 40_100, 44_900, 1, true).is_empty(), "the clock restarts at the rebase");
        assert_eq!(stall(&mut f, 45_000, 45_000, 1, true).len(), 1);
    }

    #[test]
    fn a_late_observer_does_not_report_one_freeze_twice() {
        let mut f = Freezes::default();
        assert_eq!(stall(&mut f, 0, 5_000, 1, true).len(), 1, "the probe");
        // The observer is stopped for 25 s and the counter never moved: the freeze is still the
        // same one, so no second probe, however long the counter stays unchanged afterwards.
        assert_eq!(f.observe(30_000, Some(1), true), None);
        assert!(stall(&mut f, 30_100, 45_000, 1, true).is_empty(), "the open freeze is not probed again");
        assert_eq!(
            f.observe(45_100, Some(2), true),
            Some(FreezeEvent::Recovered { seq: 1, ms: 5_000 + 15_100 }),
            "the time before the gap still counts, the gap itself does not"
        );
    }

    #[test]
    fn a_recovery_inside_a_late_gap_is_still_reported() {
        let mut f = Freezes::default();
        assert_eq!(stall(&mut f, 0, 5_000, 1, true).len(), 1);
        // The counter moved while the observer was away: the pre-gap time is the lower bound.
        assert_eq!(f.observe(40_000, Some(2), true), Some(FreezeEvent::Recovered { seq: 1, ms: 5_000 }));
        assert_eq!(f.observe(40_100, Some(2), true), None, "recovery is reported once");
    }

    #[test]
    fn an_unsent_probe_is_not_counted() {
        let mut f = Freezes::default();
        assert_eq!(stall(&mut f, 0, 5_000, 1, true).len(), 1);
        f.unsent();
        assert!(stall(&mut f, 5_100, 20_000, 1, true).is_empty(), "the same freeze is not retried");
        assert_eq!(f.observe(20_100, Some(2), true), Some(FreezeEvent::Recovered { seq: 1, ms: 20_100 }));
        // The next freeze reuses the number the unsent one never spent.
        let events = stall(&mut f, 20_200, 25_200, 2, true);
        assert_eq!(events, vec![(25_100, FreezeEvent::Probe { seq: 1, ms: 5_000 })]);
    }

    #[test]
    fn a_freeze_that_ends_by_backgrounding_is_recovered() {
        let mut f = Freezes::default();
        assert_eq!(stall(&mut f, 0, 6_000, 1, true).len(), 1);
        assert_eq!(f.observe(6_100, Some(1), false), Some(FreezeEvent::Recovered { seq: 1, ms: 6_100 }));
    }

    #[test]
    fn probes_are_capped_per_process() {
        let mut f = Freezes::default();
        let mut probes = 0;
        let mut now = 0;
        let mut counter = 0;
        for _ in 0..(MAX_FREEZE_PROBES + 3) {
            counter += 1;
            for (_, e) in stall(&mut f, now, now + 6_000, counter, true) {
                if matches!(e, FreezeEvent::Probe { .. }) { probes += 1; }
            }
            now += 6_100;
        }
        assert_eq!(probes, MAX_FREEZE_PROBES);
    }

    #[test]
    fn buckets_are_exactly_three_and_round_trip_through_their_words() {
        assert_eq!(FreezeBucket::of_ms(5_000), FreezeBucket::Short);
        assert_eq!(FreezeBucket::of_ms(9_999), FreezeBucket::Short);
        assert_eq!(FreezeBucket::of_ms(10_000), FreezeBucket::Medium);
        assert_eq!(FreezeBucket::of_ms(29_999), FreezeBucket::Medium);
        assert_eq!(FreezeBucket::of_ms(30_000), FreezeBucket::Long);
        assert_eq!(FreezeBucket::of_ms(u64::MAX), FreezeBucket::Long);
        for b in [FreezeBucket::Short, FreezeBucket::Medium, FreezeBucket::Long] {
            assert_eq!(FreezeBucket::from_word(b.word()), Some(b));
        }
        assert_eq!(FreezeBucket::from_word("unrecovered"), None, "a recovery line never says unrecovered");
        assert_eq!(FreezeBucket::from_word("12345ms"), None, "raw durations are not a bucket");
        assert_eq!(freeze_end_record(2, FreezeBucket::Medium), "freeze-end: seq=2 bucket=10-30s");
    }

    #[test]
    fn the_probe_is_not_sent_where_no_handler_is_installed() {
        // The test harness installs no tracer, so SIGUSR2 has its default (fatal) disposition and
        // sending it would kill this test process. Sending nothing is the behaviour under test.
        let me = unsafe { libc::pthread_self() } as usize;
        assert!(!probe_main_thread(me, 1));
        assert_eq!(plx_freeze_seq.load(Ordering::Acquire), 0, "an unsent probe arms nothing");
    }

}
