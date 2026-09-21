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
    sync::{atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering}, OnceLock},
    time::{Duration, Instant},
};

const HANG_THRESHOLD_MS: u64 = 250;
const POLL_MS: u64 = 100;

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

static UNLABELED: BlockingLabel = BlockingLabel::new("unlabeled");

struct Signals {
    active: AtomicBool,
    presented: AtomicBool,
    progress: AtomicUsize,
    label: AtomicPtr<BlockingLabel>,
}

impl Signals {
    const fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            presented: AtomicBool::new(false),
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

thread_local! {
    /// Other workers may enter blocking scopes, but cannot publish the frame thread's label.
    static WATCHED: Cell<Option<&'static Signals>> = const { Cell::new(None) };
    static LABEL: Cell<&'static BlockingLabel> = const { Cell::new(&UNLABELED) };
}

pub(super) struct LabelScope {
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
pub(crate) struct LoopWatch {
    signals: Option<&'static Signals>,
    previous: Option<&'static Signals>,
    _thread: PhantomData<Rc<()>>,
}

impl LoopWatch {
    pub(crate) fn start() -> Self {
        if cfg!(test) { return Self::disabled(); }
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
            let started = super::spawn("main-thread watchdog", observe_loop).is_some();
            if !started { crate::log("main-thread watchdog: unavailable; hang detection disabled"); }
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
    pub(crate) fn presented(&self) {
        if let Some(signals) = self.signals {
            signals.presented.store(true, Ordering::Release);
        }
    }

    /// One native-word atomic increment, independent of presentation, replay time or heartbeat.
    #[inline]
    pub(crate) fn advance(&self) {
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

fn observe_loop() {
    let origin = Instant::now();
    let mut detector = Detector::default();
    loop {
        // A new iteration's label publication follows its progress increment. Read in this
        // order so that label cannot be paired with older progress, and timestamp afterwards
        // so a delayed observer does not backdate the progress it just saw.
        let label = SIGNALS.label();
        let progress = SIGNALS.progress();
        let now = origin.elapsed().as_millis().min(u64::MAX as u128) as u64;
        if let Some(event) = detector.observe(now, progress, label) {
            crate::log(&event.line());
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
