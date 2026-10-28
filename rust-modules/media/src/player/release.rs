//! **The native object's release lifecycle** — a pure machine (`ci/check-deps.sh` `MACHINES`).
//!
//! A real stop hands the constructed native object to an off-frame `Unload` (`engine::PendingRelease`).
//! `StarfishMediaAPIs::Unload` takes 110-150 ms on the dev set, and on a set where it never returns
//! (PLX-NATIVE-1Q, webOS 4.9.6) the UI thread used to block in the `JoinHandle::join` of the next
//! start, the token accessors and the exit, without a bound. This machine owns what those callers
//! may do instead:
//!
//! ```text
//!   Idle ──UnloadHandedOff──▶ Releasing ──UnloadReturned──▶ (RunTail, then the queued start) ──▶ Idle
//!                                │  ▲ StartRequested: queued, answered Deferred
//!                                └──Tick past RELEASE_DEADLINE──▶ Overdue ──UnloadReturned──▶ (RunTail) ──▶ Idle
//! ```
//!
//! It holds no `JoinHandle`, reads no clock and starts nothing. The runner (`engine.rs`, the
//! adapter, `app::run`) joins a worker only once `is_finished()` says the join cannot block, feeds
//! the result back as one event, and executes the effects. `Instant`s arrive on events; the
//! machine only compares them. **`Overdue` never destroys the object**: the worker may still be
//! inside firmware, so the object, its payload and the Engine it belonged to are leaked until the
//! `Unload` returns (or until the process ends by `_exit` on a firmware exit), exactly as an `AbandonedLoad` is.

use std::time::{Duration, Instant};

/// How long an off-frame `Unload` may run before a start over it is refused with a typed failure.
/// The device `Unload` is 110-150 ms and `NATIVE_LOAD_BUDGET` for a Load is 20 s; this is the same
/// kind of bound for the other half. Unmeasured on a slow chassis: the 1Q set (o18, webOS 4.9.6)
/// is the reference.
pub const RELEASE_DEADLINE: Duration = Duration::from_secs(10);

/// A play that arrived while the previous object was still inside `Unload`: the prepared route
/// transaction the runner starts the moment the tail has run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueuedStart {
    pub route_start: crate::route::RouteStartTransaction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReleaseState {
    Idle,
    Releasing { epoch: u32, since: Instant, queued: Option<QueuedStart> },
    /// The deadline passed. The worker may be live; nothing is destroyed.
    Overdue { epoch: u32 },
}

/// One IO's result, delivered by the runner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseEvent {
    /// `release_off_frame` spawned the `Unload` worker at `at`.
    UnloadHandedOff { epoch: u32, at: Instant },
    /// A native start wants the object slot.
    StartRequested(QueuedStart),
    /// The per-frame poll saw the worker finished and joined it (that join cannot block).
    UnloadReturned { epoch: u32 },
    /// The per-frame poll's clock.
    Tick { now: Instant },
    /// The process is ending.
    Exit { now: Instant },
    /// A stop or suspend found a start queued behind the release: it must not run afterwards.
    CancelQueued,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseEffect {
    /// The main-thread tail of the release: gate retire, epoch retire, ACB unload, D1, window.
    RunTail { epoch: u32 },
    /// Start now (nothing is releasing) or after the tail (it was queued).
    Start(QueuedStart),
    /// Refuse the start with `FailureKind::ReleaseTimeout`; the epoch stays blocked.
    RefuseStart(QueuedStart),
    /// A queued start was replaced by a newer play; settle the old transaction as failed.
    AbortSuperseded(QueuedStart),
    ExitClean,
    /// The `Unload` is still running inside the deadline: keep polling.
    ExitWaiting,
    /// A worker is inside firmware (the `Unload` is overdue). SDL must not tear the display down.
    ExitWorkerInFirmware,
}

#[derive(Debug)]
pub struct NativeRelease {
    state: ReleaseState,
}

impl Default for NativeRelease {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeRelease {
    pub const fn new() -> Self {
        Self { state: ReleaseState::Idle }
    }

    /// An `Unload` is running (or overdue): the object slot is not free.
    pub fn is_busy(&self) -> bool {
        !matches!(self.state, ReleaseState::Idle)
    }

    /// A start is queued behind the running `Unload`.
    pub fn has_queued(&self) -> bool {
        matches!(self.state, ReleaseState::Releasing { queued: Some(_), .. })
    }

    /// The deadline passed and the worker has not returned.
    pub fn is_overdue(&self) -> bool {
        matches!(self.state, ReleaseState::Overdue { .. })
    }

    pub fn step(&mut self, event: &ReleaseEvent, fx: &mut Vec<ReleaseEffect>) {
        use ReleaseEffect as E;
        use ReleaseEvent as V;
        use ReleaseState as S;
        match (self.state, *event) {
            (S::Idle, V::UnloadHandedOff { epoch, at }) => {
                self.state = S::Releasing { epoch, since: at, queued: None };
            }
            // A second hand-off cannot exist (the slot holds one release); ignore rather than
            // lose the first.
            (_, V::UnloadHandedOff { .. }) => {}
            (S::Idle, V::StartRequested(q)) => fx.push(E::Start(q)),
            (S::Releasing { epoch, since, queued }, V::StartRequested(q)) => {
                if let Some(old) = queued {
                    // The same transaction asked twice is one start; a different one supersedes.
                    if old != q {
                        fx.push(E::AbortSuperseded(old));
                    }
                }
                self.state = S::Releasing { epoch, since, queued: Some(q) };
            }
            (S::Overdue { .. }, V::StartRequested(q)) => fx.push(E::RefuseStart(q)),
            (S::Releasing { epoch, queued, .. }, V::UnloadReturned { epoch: e }) if e == epoch => {
                fx.push(E::RunTail { epoch });
                if let Some(q) = queued {
                    fx.push(E::Start(q));
                }
                self.state = S::Idle;
            }
            (S::Overdue { epoch }, V::UnloadReturned { epoch: e }) if e == epoch => {
                fx.push(E::RunTail { epoch });
                self.state = S::Idle;
            }
            (_, V::UnloadReturned { .. }) => {}
            (S::Releasing { epoch, since, queued }, V::Tick { now }) => {
                if now.saturating_duration_since(since) >= RELEASE_DEADLINE {
                    if let Some(q) = queued {
                        fx.push(E::RefuseStart(q));
                    }
                    self.state = S::Overdue { epoch };
                }
            }
            (_, V::Tick { .. }) => {}
            (S::Releasing { epoch, since, queued: Some(q) }, V::CancelQueued) => {
                self.state = S::Releasing { epoch, since, queued: None };
                fx.push(E::AbortSuperseded(q));
            }
            (_, V::CancelQueued) => {}
            (S::Idle, V::Exit { .. }) => fx.push(E::ExitClean),
            (S::Releasing { epoch, since, queued }, V::Exit { now }) => {
                if now.saturating_duration_since(since) >= RELEASE_DEADLINE {
                    if let Some(q) = queued {
                        fx.push(E::RefuseStart(q));
                    }
                    self.state = S::Overdue { epoch };
                    fx.push(E::ExitWorkerInFirmware);
                } else {
                    fx.push(E::ExitWaiting);
                }
            }
            (S::Overdue { .. }, V::Exit { .. }) => fx.push(E::ExitWorkerInFirmware),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(serial_from: u64) -> QueuedStart {
        QueuedStart { route_start: crate::route::RouteStartTransaction::fixture(serial_from) }
    }

    fn run(m: &mut NativeRelease, ev: ReleaseEvent) -> Vec<ReleaseEffect> {
        let mut fx = Vec::new();
        m.step(&ev, &mut fx);
        fx
    }

    fn releasing(t0: Instant) -> NativeRelease {
        let mut m = NativeRelease::new();
        assert!(run(&mut m, ReleaseEvent::UnloadHandedOff { epoch: 7, at: t0 }).is_empty());
        m
    }

    #[test]
    fn an_idle_machine_starts_at_once_and_exits_clean() {
        let mut m = NativeRelease::new();
        assert_eq!(run(&mut m, ReleaseEvent::StartRequested(q(1))), vec![ReleaseEffect::Start(q(1))]);
        assert_eq!(run(&mut m, ReleaseEvent::Exit { now: Instant::now() }), vec![ReleaseEffect::ExitClean]);
        assert!(!m.is_busy());
    }

    #[test]
    fn a_start_over_a_running_unload_is_queued_and_runs_after_the_tail() {
        let t0 = Instant::now();
        let mut m = releasing(t0);
        assert!(run(&mut m, ReleaseEvent::StartRequested(q(1))).is_empty());
        assert!(m.is_busy());
        assert_eq!(
            run(&mut m, ReleaseEvent::UnloadReturned { epoch: 7 }),
            vec![ReleaseEffect::RunTail { epoch: 7 }, ReleaseEffect::Start(q(1))],
            "the tail must run before the queued start"
        );
        assert!(!m.is_busy());
    }

    #[test]
    fn the_newest_queued_start_wins_and_the_old_one_is_settled() {
        let mut m = releasing(Instant::now());
        assert!(run(&mut m, ReleaseEvent::StartRequested(q(1))).is_empty());
        assert!(run(&mut m, ReleaseEvent::StartRequested(q(1))).is_empty(), "the same transaction is one start");
        assert_eq!(run(&mut m, ReleaseEvent::StartRequested(q(2))), vec![ReleaseEffect::AbortSuperseded(q(1))]);
        assert_eq!(
            run(&mut m, ReleaseEvent::UnloadReturned { epoch: 7 }),
            vec![ReleaseEffect::RunTail { epoch: 7 }, ReleaseEffect::Start(q(2))]
        );
    }

    #[test]
    fn a_stop_cancels_the_queued_start_so_it_never_runs_after_the_tail() {
        let mut m = releasing(Instant::now());
        let _ = run(&mut m, ReleaseEvent::StartRequested(q(1)));
        assert_eq!(run(&mut m, ReleaseEvent::CancelQueued), vec![ReleaseEffect::AbortSuperseded(q(1))]);
        assert!(run(&mut m, ReleaseEvent::CancelQueued).is_empty());
        assert_eq!(
            run(&mut m, ReleaseEvent::UnloadReturned { epoch: 7 }),
            vec![ReleaseEffect::RunTail { epoch: 7 }],
            "a cancelled start must not follow the tail"
        );
    }

    #[test]
    fn a_wrong_epoch_return_changes_nothing() {
        let mut m = releasing(Instant::now());
        assert!(run(&mut m, ReleaseEvent::UnloadReturned { epoch: 8 }).is_empty());
        assert!(m.is_busy());
    }

    #[test]
    fn the_deadline_refuses_the_queued_start_and_never_runs_the_tail() {
        let t0 = Instant::now();
        let mut m = releasing(t0);
        let _ = run(&mut m, ReleaseEvent::StartRequested(q(1)));
        assert!(run(&mut m, ReleaseEvent::Tick { now: t0 + RELEASE_DEADLINE - Duration::from_millis(1) }).is_empty());
        assert!(!m.is_overdue());
        assert_eq!(
            run(&mut m, ReleaseEvent::Tick { now: t0 + RELEASE_DEADLINE }),
            vec![ReleaseEffect::RefuseStart(q(1))]
        );
        assert!(m.is_overdue() && m.is_busy(), "an overdue release still owns the slot");
        // Later starts are refused, never handed a second object.
        assert_eq!(run(&mut m, ReleaseEvent::StartRequested(q(2))), vec![ReleaseEffect::RefuseStart(q(2))]);
        // A late return still runs the tail and frees the slot.
        assert_eq!(run(&mut m, ReleaseEvent::UnloadReturned { epoch: 7 }), vec![ReleaseEffect::RunTail { epoch: 7 }]);
        assert!(!m.is_busy());
        assert_eq!(run(&mut m, ReleaseEvent::StartRequested(q(3))), vec![ReleaseEffect::Start(q(3))]);
    }

    #[test]
    fn exit_waits_inside_the_deadline_and_reports_the_worker_in_firmware_past_it() {
        let t0 = Instant::now();
        let mut m = releasing(t0);
        assert_eq!(
            run(&mut m, ReleaseEvent::Exit { now: t0 + Duration::from_secs(1) }),
            vec![ReleaseEffect::ExitWaiting]
        );
        assert_eq!(
            run(&mut m, ReleaseEvent::Exit { now: t0 + RELEASE_DEADLINE }),
            vec![ReleaseEffect::ExitWorkerInFirmware]
        );
        assert!(m.is_overdue());
        assert_eq!(
            run(&mut m, ReleaseEvent::Exit { now: t0 + RELEASE_DEADLINE }),
            vec![ReleaseEffect::ExitWorkerInFirmware]
        );
    }

    #[test]
    fn exit_after_the_unload_returned_is_clean() {
        let t0 = Instant::now();
        let mut m = releasing(t0);
        let _ = run(&mut m, ReleaseEvent::UnloadReturned { epoch: 7 });
        assert_eq!(run(&mut m, ReleaseEvent::Exit { now: t0 }), vec![ReleaseEffect::ExitClean]);
    }
}
