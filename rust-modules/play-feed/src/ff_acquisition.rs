//! **One acquisition policy, not two independently-optional parameters, and sealed against the
//! bypass that caused it.** `hls_demux_segment` used to take `(ReserveDeadlineState,
//! Option<StallGuard>)`, and every combination of the two compiled — including "no reserve
//! deadline AND no stall guard", which is exactly what `hls_prefetch_same_encoder`'s same-encoder
//! lookahead built (`ReserveDeadlineState::new(None, false), None`, unconditionally). That is how
//! a rung-20000 link collapse to 500 kbps (`pipe_abr_down_collapse`, segment 11, 5.4 MB) ran
//! 86.8 s and froze the picture for ~84s: the lookahead fetched that very segment and never
//! evaluated a terminal-reserve abort at all, because nothing forced it to.
//!
//! Replacing the pair with one required, role-typed argument was the first fix; it was not
//! enough on its own, because `ff` is one module and `SegmentAcquisition::Active { stall: None,
//! .. }` was just as constructible by hand inside `ff` as the two-parameter call it replaced.
//! This module is the second half: `SegmentAcquisition`'s payload lives in a private inner enum
//! (`Policy`) that `ff` cannot name, so the only way `ff` can produce an `Active` acquisition at
//! all is [`SegmentAcquisition::for_cursor`] — which is also the only place the
//! `arm_active_stall_guard` evaluation runs. The adaptive owner supplies the current floor
//! decision; this module samples the live reserve and hold at the acquisition boundary.
use super::*;
use plx_base::checkpoint::{Checkpoint, Flow};

/// The acquisition policy for one `hls_demux_segment` fetch. Its payload (`Policy`) is private to
/// this module, so nothing outside — including `ff` itself — can name `Policy::Active(..)` or
/// build an `ActiveAcquisition` by hand. The type name is `pub(super)` so `ff` can hold values of
/// it and pass them to `hls_demux_segment`; producing one is only ever possible through this
/// module's own constructors.
pub struct SegmentAcquisition(Policy);

enum Policy {
    /// The playback session's ACTIVE cursor — an ordinary fetch or the same-encoder lookahead
    /// that reads ahead of it. Both are the same kind of request against the same rung and must
    /// arm the identical [`StallGuard`]. Only [`SegmentAcquisition::for_cursor`] can build one.
    Active(ActiveAcquisition),
    /// An ABR exploration fetch (a candidate's warm-up or its repeatable follow-up), racing its
    /// own [`ReserveDeadlineState`]. Never carries a `StallGuard`: PMS may pause a sized response
    /// while its JIT encoder catches up, and a candidate's prefix rate is not proof that its
    /// remainder will miss that deadline — only the deadline itself decides.
    Candidate(ReserveDeadlineState),
    /// Non-adaptive playback: there is no ladder to abandon a rung on, so there is no reserve
    /// deadline and no stall guard to arm.
    Fixed(Arc<dyn DemuxObserver>),
}

/// Payload of [`Policy::Active`]. Private, in a module whose only `Active`-producing path is
/// [`SegmentAcquisition::for_cursor`] — so every active-cursor fetch, ordinary or lookahead, is
/// structurally forced through the same `arm_active_stall_guard` evaluation the ordinary branch
/// always ran. A legitimately unarmed outcome (unknown/zero reserve at the start of a fetch, or
/// an already-held clock — see `StallGuard::arm` / `arm_active_stall_guard`) still results, but
/// only as this evaluation's own answer, never a call site's shortcut.
struct ActiveAcquisition {
    reserve_deadline: ReserveDeadlineState,
    stall: Option<StallGuard>,
}

impl SegmentAcquisition {
    /// **The one constructor for a fetch against the session's active cursor** — the ordinary
    /// fetch in `hls_demux`'s segment loop and the same-encoder lookahead in
    /// `hls_prefetch_same_encoder` both call this, with the SAME adaptive context, because they
    /// read ahead of the identical rung and must be classified identically. The adaptive owner
    /// supplies `Some(at_floor)` for ABR playback, sampled at this boundary; `None` denotes
    /// non-adaptive playback. This module samples live reserve
    /// (`hls_buffer_fact(observer, None)`) and the captured hold observer itself.
    /// Non-adaptive playback
    /// is this same constructor given `None`; there is deliberately no separate public `fixed()`.
    ///
    /// The guard this evaluation arms is consulted on EVERY blocking leg of the fetch, not only at
    /// the next AVIO callback: [`SegmentAcquisition::begin`] turns the acquisition into one
    /// [`AcquisitionRuntime`] that `hls_demux_segment` hands as the transports' [`Checkpoint`]
    /// through the HTTP open (connect, request, headers), the `NotReady` retry wait, and then —
    /// moved into the AVIO, never re-armed — FFmpeg's probe and body reads. A wait already blocked
    /// when the boundary arrives re-asks at least every [`CHECK_SLICE`] while armed.
    ///
    /// **Still out of scope:** the synchronous `getaddrinfo` inside a plaintext open's connect
    /// cannot be interrupted by any checkpoint; the guard is consulted as soon as it returns.
    pub fn for_cursor(
        at_floor: Option<bool>, observer: Arc<dyn DemuxObserver>,
    ) -> Option<Self> {
        observer.hold_epoch()?;
        Some(match at_floor {
            Some(at_floor) => Self::active(
                hls_buffer_fact(observer.as_ref(), None)?.buffered_ms(),
                at_floor,
                observer.rebuffering().unwrap_or(true),
                observer,
            ),
            None => SegmentAcquisition(Policy::Fixed(observer)),
        })
    }

    /// Private: the only callers are [`SegmentAcquisition::for_cursor`] and, under
    /// the test-support gate, [`SegmentAcquisition::for_test`]. Not reachable from `ff`.
    fn active(
        reserve_ms: Option<i64>, at_floor: bool, already_held: bool,
        observer: Arc<dyn DemuxObserver>,
    ) -> Self {
        SegmentAcquisition(Policy::Active(ActiveAcquisition {
            reserve_deadline: ReserveDeadlineState::new(None, false, observer.clone()),
            stall: arm_active_stall_guard(reserve_ms, at_floor, already_held, observer),
        }))
    }

    pub fn candidate(deadline: ReserveDeadlineState) -> Self {
        SegmentAcquisition(Policy::Candidate(deadline))
    }

    /// The armed guard, if any. Read-only: a caller can log `reserve_ms_at_start` on abort, or (in
    /// a test) assert whether an injected acquisition was armed, without being able to construct
    /// or replace the guard itself.
    pub fn stall(&self) -> Option<StallGuard> {
        match &self.0 {
            Policy::Active(active) => active.stall.clone(),
            Policy::Candidate(_) | Policy::Fixed(_) => None,
        }
    }

    /// **Start the acquisition.** Consumes the role and returns its reserve deadline (which the
    /// open loop and then the AVIO compose with transport liveness, as before) and the ONE runtime
    /// that owns the armed guard for the rest of the fetch. The acquisition clock starts here.
    /// Nothing re-arms or resets it afterwards: the runtime moves, whole, from the open loop into
    /// `AvioState`.
    pub fn begin(self, aq: *mut AuQueue) -> (ReserveDeadlineState, AcquisitionRuntime) {
        let (reserve_deadline, stall) = match self.0 {
            Policy::Active(active) => (active.reserve_deadline, active.stall),
            Policy::Candidate(deadline) => (deadline, None),
            Policy::Fixed(observer) =>
                (ReserveDeadlineState::new(None, false, observer), None),
        };
        (
            reserve_deadline,
            AcquisitionRuntime {
                aq,
                stall,
                request_started: std::time::Instant::now(),
                phase: Phase::Opening,
                stop: None,
            },
        )
    }

    /// Test-only escape hatch: build an `Active` acquisition from explicit inputs rather than
    /// `for_cursor`'s own live sampling, so a test can hold the buffer/floor/hold state fixed
    /// without wiring up a real cursor, controller and session observer. Available only
    /// to tests and the test-support feature; the shipping constructor is always `for_cursor`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(
        reserve_ms: Option<i64>, at_floor: bool, already_held: bool,
        observer: Arc<dyn DemuxObserver>,
    ) -> Self {
        Self::active(reserve_ms, at_floor, already_held, observer)
    }

    /// [`Self::for_test`] with an explicit reserve deadline, so a test can make the open's own
    /// deadline expire sooner than [`CHECK_SLICE`] — the one way a transport ends a wait without
    /// asking its checkpoint again.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test_with_deadline(
        reserve_ms: Option<i64>,
        at_floor: bool,
        already_held: bool,
        reserve_deadline: ReserveDeadlineState,
        observer: Arc<dyn DemuxObserver>,
    ) -> Self {
        SegmentAcquisition(Policy::Active(ActiveAcquisition {
            reserve_deadline,
            stall: arm_active_stall_guard(reserve_ms, at_floor, already_held, observer),
        }))
    }
}

/// How often an ARMED acquisition re-asks while a transport or retry wait is blocked, at most.
/// The projected playhead boundary may come sooner; an accepted hold has no projection and is
/// seen within one slice. Unarmed runtimes never ask again at all (`next_check: None`).
pub const CHECK_SLICE: std::time::Duration = std::time::Duration::from_millis(100);

/// Where the fetch is. Only `BodyComplete` changes a decision: a response whose every declared
/// byte arrived (or whose unsized body reached a confirmed end) is credited, never abandoned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Opening,
    RetryWaiting,
    Body,
    BodyComplete,
}

/// Why the runtime answered [`Flow::Stop`]. Latched: every later check stops too, so a
/// subsequent AVIO callback cannot resume a fetch this runtime has already abandoned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopReason {
    Teardown,
    StallAbort,
}

/// **One active fetch's guard, across every leg that can block.** Built only by
/// [`SegmentAcquisition::begin`], so it inherits the role's sealed arming: a candidate or a
/// non-adaptive fetch carries no guard (and therefore never stops except for teardown), an
/// already-held clock or an unknowable/zero starting reserve arms nothing.
///
/// As the transports' [`Checkpoint`] its answer is, in priority order: teardown (the AU lane
/// aborted) stops; a completed body continues unconditionally; the guard's boundary — an
/// accepted hold, a hold epoch that moved since arming, or a playhead that has spent the
/// starting reserve — stops above the floor after asking the main thread to hold the clock, and
/// at the floor asks once, disarms and continues the SAME operation; otherwise continue, asking
/// again by the projected boundary or [`CHECK_SLICE`], whichever is sooner. Reserve-deadline and
/// transport-liveness classification stay with the caller, which composes them into the
/// transport's own deadline exactly as before.
pub struct AcquisitionRuntime {
    aq: *mut AuQueue,
    stall: Option<StallGuard>,
    request_started: std::time::Instant,
    phase: Phase,
    stop: Option<StopReason>,
}

impl AcquisitionRuntime {
    pub fn request_started(&self) -> std::time::Instant {
        self.request_started
    }

    /// Move to `phase`. `BodyComplete` is terminal: nothing moves a completed body back.
    pub fn enter(&mut self, phase: Phase) {
        if self.phase != Phase::BodyComplete {
            self.phase = phase;
        }
    }

    /// Record body progress. `received` is the end offset of what the transport has RECEIVED
    /// (`AvioState::note_received`), not what FFmpeg has read. A known length is complete once
    /// every declared byte arrived; an unknown length (`size < 0`) is complete only at a
    /// confirmed end (`eof`).
    pub fn note_body(&mut self, size: i64, received: i64, eof: bool) {
        if eof || (size >= 0 && received >= size) {
            self.enter(Phase::BodyComplete);
        }
    }

    /// The abort latched for the enclosing FFmpeg operation (`classify_hls_avio_operation`).
    pub fn stall_aborted(&self) -> bool {
        self.stop == Some(StopReason::StallAbort)
    }

    /// The exit a stopped leg BEFORE the AVIO existed returns, or `None` when this runtime never
    /// stopped. Teardown outranks a latched abort however they interleaved. A pre-body abort
    /// reports what was really acquired: no body byte and no body read, over the elapsed request.
    pub fn stopped_exit(&self, audio_expected: bool) -> Option<HlsExit> {
        if unsafe { crate::aq::aq_is_aborted(self.aq) } || self.stop == Some(StopReason::Teardown) {
            return Some(HlsExit::Aborted);
        }
        self.stall_aborted().then(|| {
            HlsExit::StallAbort(SegmentTransfer {
                bytes: 0,
                active_us: 0,
                total_us: self.request_started.elapsed().as_micros().max(1) as u64,
                audio_expected,
            })
        })
    }

    /// **Settle a leg that ended without success** — a deadline, a transport failure, a
    /// `NotReady`, a stop. The transport's last answer may be stale: a deadline shorter than
    /// [`CHECK_SLICE`] ends a wait without asking again. So this asks AFRESH, in `check()`'s own
    /// priority (teardown, completed body, guard boundary — at the floor that requests the hold
    /// and continues), and only then lets the leg's own classification stand. Returns whether the
    /// runtime has stopped.
    pub fn settle_stopped(&mut self) -> bool {
        Checkpoint::check(self) == Flow::Stop
    }

    /// [`Self::settle_stopped`] for a leg before the AVIO exists: the latched exit if the runtime
    /// stopped, otherwise the leg's own `outcome`.
    pub fn settle(&mut self, outcome: HlsExit, audio_expected: bool) -> HlsExit {
        self.settle_stopped();
        self.stopped_exit(audio_expected).unwrap_or(outcome)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn armed(&self) -> bool {
        self.stall.is_some()
    }
}

impl Checkpoint for AcquisitionRuntime {
    fn check(&mut self) -> Flow {
        let flow = self.evaluate();
        #[cfg(any(test, feature = "test-support"))]
        observe::report(self.phase, flow);
        flow
    }
}

impl AcquisitionRuntime {
    fn evaluate(&mut self) -> Flow {
        if self.stop.is_some() {
            return Flow::Stop;
        }
        if unsafe { crate::aq::aq_is_aborted(self.aq) } {
            self.stop = Some(StopReason::Teardown);
            return Flow::Stop;
        }
        if self.phase == Phase::BodyComplete {
            return Flow::Continue { next_check: None };
        }
        let Some(guard) = self.stall.as_ref() else {
            return Flow::Continue { next_check: None };
        };
        if guard.should_abort(false, guard.observer.rebuffering().unwrap_or(true)) {
            guard.observer.publish(DemuxCallbackEffect::RequestTerminalHold);
            if guard.aborts_fetch() {
                self.stop = Some(StopReason::StallAbort);
                return Flow::Stop;
            }
            self.stall = None;
            return Flow::Continue { next_check: None };
        }
        let slice = std::time::Instant::now() + CHECK_SLICE;
        Flow::Continue {
            next_check: Some(guard.projected_boundary().min(slice)),
        }
    }
}

/// A host-suite seam: a test registers ONE thread and receives every answer an acquisition
/// runtime on that thread gives, with the phase it gave it in — so it can change the world
/// strictly AFTER a given `Continue` instead of sleeping and hoping. A change made by ANOTHER thread
/// that received the answer races the watched thread's next ask; `on_nth_continue` runs the change
/// on the watched thread itself, between that answer and its next ask.
#[cfg(any(test, feature = "test-support"))]
pub mod observe {
    use super::{Flow, Phase};
    use std::sync::mpsc::SyncSender;
    use std::sync::Mutex;
    use std::thread::ThreadId;

    /// One registered observer: the thread it watches, where that thread's answers go, and an
    /// optional action to run ON that thread (see [`on_nth_continue`]).
    struct Watched {
        thread: ThreadId,
        tx: SyncSender<(Phase, Flow)>,
        hook: Option<Hook>,
    }

    struct Hook {
        phase: Phase,
        /// `Continue`s in `phase` still to be answered before the action runs.
        remaining: usize,
        action: Box<dyn FnOnce() + Send>,
    }

    static WATCHED: Mutex<Option<Watched>> = Mutex::new(None);

    pub fn watch_this_thread(tx: SyncSender<(Phase, Flow)>) {
        *WATCHED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Watched {
            thread: std::thread::current().id(),
            tx,
            hook: None,
        });
    }

    /// Run `action` on the WATCHED thread itself, synchronously, once the runtime has answered
    /// its `nth` `Continue` in `phase` (counted from this call): before that answer reaches the
    /// channel and before that thread can ask again. The channel [`watch_this_thread`] feeds cannot give that ordering: another
    /// thread that receives the answer and then changes the world races the watched thread's
    /// next ask, so a stalled receiver let a recheck slip in ahead of the change.
    pub fn on_nth_continue(
        phase: Phase,
        nth: usize,
        action: impl FnOnce() + Send + 'static,
    ) {
        assert!(nth > 0, "nth counts from 1");
        let mut watched = WATCHED.lock().unwrap_or_else(|e| e.into_inner());
        let watched = watched
            .as_mut()
            .expect("watch_this_thread must run before on_nth_continue");
        watched.hook = Some(Hook {
            phase,
            remaining: nth,
            action: Box::new(action),
        });
    }

    pub fn unwatch() {
        *WATCHED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn report(phase: Phase, flow: Flow) {
        let (tx, action) = {
            let mut watched = WATCHED.lock().unwrap_or_else(|e| e.into_inner());
            let Some(watched) = watched.as_mut() else {
                return;
            };
            if watched.thread != std::thread::current().id() {
                return;
            }
            let action = match watched.hook.as_mut() {
                Some(hook) if hook.phase == phase && matches!(flow, Flow::Continue { .. }) => {
                    hook.remaining -= 1;
                    if hook.remaining == 0 {
                        watched.hook.take().map(|hook| hook.action)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            (watched.tx.clone(), action)
        };
        // Outside the lock (the action is the test's own code), and BEFORE the answer is
        // delivered: a receiver that wakes on this answer finds the change already made.
        if let Some(action) = action {
            action();
        }
        let _ = tx.try_send((phase, flow));
    }
}
