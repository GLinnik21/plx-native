//! Epoch-scoped native Load, release and presentation callback gate for the buffer-feed driver.
//! The one mutex remains held through callback writes and common-state reset via typed closures;
//! neither a guard nor the private phase escapes to the media coordinator.
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// issue #74 D.1: whether the epoch's Starfish `Load` call has returned yet. Mirrors, on the Rust
/// side, the `g_load_returned` flag `starfish.c`'s `sf_ready_object()` now requires alongside
/// `SMP_READY()` — every session begins `InFlight`, and `native_load::load_thread` flips it to
/// `Returned` right after the real, synchronous `sf_load` call comes back, before
/// `publish_route_start_result` runs. `pump.rs`'s `loadCompleted` arm requires
/// `NativeGate::native_load_returned` before it may even poll `sf_is_load_completed` — that poll is
/// itself a concurrent Starfish call and used to be exactly what raced `Load`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoadCall {
    InFlight,
    /// `at` is when the flip happened (`mark_native_load_returned`'s own `Instant::now()`) — the
    /// zero point for the OTHER half of issue #74 D.1's budget: `pump.rs` bounds "Load never
    /// returns" from `load_issued_at`, and separately bounds "Load returned but loadCompleted
    /// never arrives" from this timestamp, on a fresh `NATIVE_LOAD_BUDGET` window rather than
    /// whatever was left of the first one.
    Returned {
        at: Instant,
        /// The audio lead the Load thread resolved from the sound output before it called `Load`
        /// (`engine::PcmLead::FromOutput`, on a stack whose lead follows the output); `None` for a Load whose lead the frame thread settled.
        /// Published WITH the return, so no reader can see the Load returned without it.
        pcm_lead_ns: Option<i64>,
    },
}

/// Ownership of the callback function installed in one native Starfish `Load`.
///
/// [`NativeGate`] survives reloads, while the library may deliver an event on its own thread after
/// teardown has started. Each Starfish object address is now session-unique, but Rust still needs
/// a generation check and drain before resetting this process-long state. A check without this mutex
/// still has a check/use race against [`NativeGate::reset_session_with`]: a callback could validate the
/// old generation, get descheduled, and then write into the freshly reset next session. Holding
/// this lock for the complete callback makes retirement a drain barrier as well as a token check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeSessionPhase {
    Idle,
    Active {
        epoch: u32,
        /// Type-0 presentation callbacks are admitted only for the current media timeline. An
        /// in-place seek closes this while old decoded frames are flushed, then the first real
        /// post-seek keyframe reopens it.
        presentation_gate: NativePresentationGate,
        /// See [`LoadCall`]. Epoch-scoped exactly like `presentation_gate`: a late `load_thread`
        /// from a SUPERSEDED session can never flip this for a newer one, because
        /// `mark_native_load_returned`/`native_load_returned` both require `epoch == active`.
        load_call: LoadCall,
        /// When this epoch's `Load` was issued — for the "native: Load returned after Nms" log.
        load_issued_at: Instant,
    },
    /// Firmware's synchronous UNLOADCOMPLETED callback was observed. This is independent lifecycle
    /// evidence, not a producer barrier: native callback admission is closed and drained in the C
    /// interposer before the main thread retires this phase and considers D1.
    Unloaded {
        epoch: u32,
    },
    /// Teardown gave up on this epoch's `Load` while it was still in flight (the D.1.4 budget
    /// had fired and the media thread had not returned). The native object belongs to no engine
    /// any more: it is held by `PlayerAdapter`'s abandoned-Load slot until `sf_load` returns and
    /// the main thread releases it. Only the firmware's UNLOADCOMPLETED is admitted (that is the
    /// release's own lifecycle evidence); every other callback is dropped, and no new native
    /// session may begin, because the C seam still owns exactly this one object.
    Abandoned {
        epoch: u32,
    },
    /// A real stop handed this epoch's ready object to the OFF-FRAME release (`engine`'s
    /// `PendingRelease`): its `Unload` is running on a worker while the main thread has already
    /// reset the session for whatever comes next. The admission rule is `Abandoned`'s — only the
    /// firmware's UNLOADCOMPLETED is admitted, recorded in `unload_completed` rather than as a move
    /// to `Unloaded` so that [`NativeGate::reset_session_with`] (which preserves this phase) cannot erase the
    /// evidence the release then reads — and no new native session may begin until the release has
    /// retired it.
    Releasing {
        epoch: u32,
        unload_completed: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativePresentationGate {
    Armed,
    Disarmed,
    /// The first post-discontinuity AU is inside `sf_feed`. Firmware can publish the buffer to a
    /// GStreamer worker before Feed returns, so retain the newest position callback until the
    /// caller learns whether that exact AU was accepted.
    PendingArm {
        latched: Option<i64>,
    },
}

#[derive(Clone, Copy, Debug)]
struct NativeSessionState {
    next_epoch: u32,
    phase: NativeSessionPhase,
}

impl NativeSessionState {
    const fn new() -> Self {
        Self {
            next_epoch: 0,
            phase: NativeSessionPhase::Idle,
        }
    }

    fn next(&mut self) -> Option<u32> {
        self.next_epoch = self.next_epoch.checked_add(1)?;
        Some(self.next_epoch)
    }
}

/// What provenance/lifecycle effect one native callback carries. Keeping this typed here makes
/// `UNLOADCOMPLETED` a reducer transition rather than a magic event number interpreted by the
/// teardown code after the fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeEventClass {
    Presentation,
    UnloadCompleted,
    Other,
}

/// Whose state a native callback may write; see [`NativeGate::with_native_session_scoped`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventScope {
    Live,
    Release,
}

/// Physical session owner. This owns the clock and callback retirement barrier; the media
/// coordinator supplies only the addressed writes that must occur inside that barrier.
pub struct NativeGate {
    state: Mutex<NativeSessionState>,
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn native_epoch_exhaustion_never_reuses_an_earlier_callback_address() {
        let gate = NativeGate {
            state: Mutex::new(NativeSessionState {
                next_epoch: u32::MAX - 1,
                phase: NativeSessionPhase::Idle,
            }),
        };
        assert_eq!(gate.begin_native_session(), Some(u32::MAX));
        assert!(gate.retire_native_session(u32::MAX));
        assert_eq!(gate.begin_native_session(), None, "an old callback must never alias a wrapped epoch");
        assert_eq!(gate.begin_native_session(), None, "exhaustion must remain closed");
    }
}

impl NativeGate {
    pub const fn new() -> Self {
        Self { state: Mutex::new(NativeSessionState::new()) }
    }

    /// Preserve abandoned and releasing objects while clearing every common callback mirror
    /// under the same mutex. A callback cannot validate A then write into B's reset storage.
    pub fn reset_session_with(&self, reset: impl FnOnce()) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(state.phase, NativeSessionPhase::Abandoned { .. } | NativeSessionPhase::Releasing { .. }) {
            state.phase = NativeSessionPhase::Idle;
        }
        reset();
    }

    /// Enter the only state in which a Starfish `Load` may publish callbacks.
    ///
    /// `None` rejects an overlapping native session or exhausted identity domain instead of
    /// retagging a live object or reusing an old callback address.
    pub fn begin_native_session(&self) -> Option<u32> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if state.phase != NativeSessionPhase::Idle {
            return None;
        }
        let epoch = state.next()?;
        state.phase = NativeSessionPhase::Active {
            epoch,
            presentation_gate: NativePresentationGate::Armed,
            load_call: LoadCall::InFlight,
            load_issued_at: Instant::now(),
        };
        Some(epoch)
    }

    /// Mark `epoch`'s Starfish `Load` call as returned (issue #74 D.1) — called by
    /// `native_load::load_thread` immediately after the real, blocking `sf_load` call comes back,
    /// BEFORE `route::publish_route_start_result` runs (that mechanism stays separate: a route
    /// ticket can be rejected as stale independent of whether Load has returned, and this gate
    /// must never depend on the reducer's verdict). Returns the elapsed time since the session's
    /// Load was issued, for the "native: Load returned after Nms" log — `None` when `epoch` no
    /// longer owns the `Active` phase (a superseded/already-retired session; nothing to mark).
    pub fn mark_native_load_returned(&self, epoch: u32, pcm_lead_ns: Option<i64>) -> Option<Duration> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match &mut state.phase {
            NativeSessionPhase::Active {
                epoch: active,
                load_call,
                load_issued_at,
                ..
            } if epoch != 0 && *active == epoch => {
                *load_call = LoadCall::Returned {
                    at: Instant::now(),
                    pcm_lead_ns,
                };
                Some(load_issued_at.elapsed())
            }
            _ => None,
        }
    }

    /// Admit a synchronous Load refusal and run its common-state write under the same
    /// retirement barrier as callbacks and session reset.
    pub fn with_active_epoch(&self, epoch: u32, write: impl FnOnce()) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(state.phase, NativeSessionPhase::Active { epoch: active, .. } if epoch != 0 && active == epoch) {
            return false;
        }
        write();
        true
    }

    /// Whether `epoch`'s Starfish `Load` call has returned yet. Required by `pump.rs`'s
    /// `loadCompleted` arm before it may even poll `sf_is_load_completed` (issue #74: that poll is
    /// itself a concurrent Starfish call, and on v0.6.0 it was the FIRST thing that arm did while
    /// `Load` was still executing on the load thread). Epoch-scoped exactly like
    /// `mark_native_load_returned`: a late thread from a SUPERSEDED session can never open this
    /// gate for a newer one, because only `epoch == active` can ever read `Returned` here.
    pub fn native_load_returned(&self, epoch: u32) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        matches!(
            state.phase,
            NativeSessionPhase::Active {
                epoch: active,
                load_call: LoadCall::Returned { .. },
                ..
            } if epoch != 0 && active == epoch
        )
    }

    /// The audio lead `epoch`'s Load thread resolved from the sound output, once its Load has
    /// returned: `None` while it is in flight, for another epoch, and for a Load that resolved none.
    pub fn native_load_pcm_lead(&self, epoch: u32) -> Option<i64> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match state.phase {
            NativeSessionPhase::Active {
                epoch: active,
                load_call: LoadCall::Returned { pcm_lead_ns, .. },
                ..
            } if epoch != 0 && active == epoch => pcm_lead_ns,
            _ => None,
        }
    }

    /// How long ago `epoch`'s Starfish `Load` call returned — `None` when `epoch` no longer owns
    /// the `Active` phase, OR when its Load has not returned yet (still `LoadCall::InFlight`,
    /// which the OTHER budget window — [`native_load_elapsed`] — covers).
    ///
    /// This is the second half of issue #74 D.1's budget (`load-returned-log-is-conditional-on-
    /// loadcompleted-and-unbounded-after`): `native_load_elapsed` is measured from
    /// `load_issued_at` and never resets, so by the time Load actually returns it may already
    /// read close to `NATIVE_LOAD_BUDGET` — reusing it to bound the WAIT FOR `loadCompleted`
    /// would leave that second wait almost no budget of its own on a Load that took a while to
    /// return. This gives that wait a fresh full window, timed from the moment Load returned.
    pub fn native_load_returned_elapsed(&self, epoch: u32) -> Option<Duration> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match state.phase {
            NativeSessionPhase::Active {
                epoch: active,
                load_call: LoadCall::Returned { at, .. },
                ..
            } if epoch != 0 && active == epoch => Some(at.elapsed()),
            _ => None,
        }
    }

    /// How long `epoch`'s Load has been in flight (or took, once returned), for `pump.rs`'s
    /// D.1.4 timeout — `None` once `epoch` no longer owns the `Active` phase (retired/superseded;
    /// there is nothing left to time out).
    pub fn native_load_elapsed(&self, epoch: u32) -> Option<Duration> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match state.phase {
            NativeSessionPhase::Active {
                epoch: active,
                load_issued_at,
                ..
            } if epoch != 0 && active == epoch => Some(load_issued_at.elapsed()),
            _ => None,
        }
    }

    /// Test-only injection point for `pump.rs`'s D.1.4 budget arm (`load-budget-expiry-path-
    /// itself-has-no-test`, a fix-wave finding): rewinds `epoch`'s recorded `load_issued_at` by
    /// `by`, so `native_load_elapsed` reads at least `by` older without a real 20s wait. Returns
    /// `false` when `epoch` no longer owns the `Active` phase (nothing to backdate) — callers
    /// must treat that as a precondition failure, not a silent no-op, per this project's
    /// silent-instrument rule (AGENTS.md).
    #[cfg(feature = "test-support")]
    pub fn test_backdate_native_load_issued(&self, epoch: u32, by: Duration) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match &mut state.phase {
            NativeSessionPhase::Active {
                epoch: active,
                load_issued_at,
                ..
            } if epoch != 0 && *active == epoch => {
                *load_issued_at = load_issued_at.checked_sub(by).unwrap_or(*load_issued_at);
                true
            }
            _ => false,
        }
    }

    /// Test-only injection point for `pump.rs`'s D.1.4 budget arm, second half
    /// (`load-returned-log-is-conditional-on-loadcompleted-and-unbounded-after`): rewinds
    /// `epoch`'s recorded `LoadCall::Returned` timestamp by `by`, so `native_load_returned_elapsed`
    /// reads at least `by` older without a real wait. Requires `epoch` to already have returned
    /// (`LoadCall::Returned`) — `false` otherwise, including when it is still `InFlight` or the
    /// phase is no longer `Active`, per this project's silent-instrument rule (AGENTS.md).
    #[cfg(feature = "test-support")]
    pub fn test_backdate_native_load_returned(&self, epoch: u32, by: Duration) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match &mut state.phase {
            NativeSessionPhase::Active {
                epoch: active,
                load_call: LoadCall::Returned { at, .. },
                ..
            } if epoch != 0 && *active == epoch => {
                *at = at.checked_sub(by).unwrap_or(*at);
                true
            }
            _ => false,
        }
    }

    /// Retire one exact native session and wait for every callback which already entered it.
    ///
    /// Ready-object teardown calls this after `Unload` has returned and the C interposer gate has
    /// closed and drained. Startup failure paths also retire the Rust epoch here: either no native
    /// object was constructed, or construction succeeded but arming the C gate failed, cleared
    /// dispatch readiness and retained the object in process-long quarantine. That latter path
    /// admitted no native callbacks and does not claim an Unload or native-gate drain. The Active
    /// arm also keeps this primitive independently testable without fabricating firmware callbacks.
    pub fn retire_native_session(&self, epoch: u32) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let owns = matches!(
            state.phase,
            NativeSessionPhase::Active { epoch: active, .. }
                | NativeSessionPhase::Unloaded { epoch: active }
                | NativeSessionPhase::Abandoned { epoch: active }
                | NativeSessionPhase::Releasing { epoch: active, .. }
                if epoch != 0 && active == epoch
        );
        if !owns {
            return false;
        }
        state.phase = NativeSessionPhase::Idle;
        true
    }

    /// Hand `epoch`'s ready object to the off-frame release (see
    /// [`NativeSessionPhase::Releasing`]). Taking the lock is the drain barrier for a callback
    /// already inside the session; from here only UNLOADCOMPLETED is admitted. `false` when `epoch`
    /// does not own the `Active` phase (the caller then releases synchronously, as before).
    pub fn begin_native_release(&self, epoch: u32) -> bool {
        self.leave_active(epoch, NativeSessionPhase::Releasing { epoch, unload_completed: false })
    }

    /// Hand `epoch`'s still-in-flight Load to the abandoned-Load release (see
    /// [`NativeSessionPhase::Abandoned`]). `false` when `epoch` does not own the `Active` phase.
    pub fn abandon_native_session(&self, epoch: u32) -> bool {
        self.leave_active(epoch, NativeSessionPhase::Abandoned { epoch })
    }

    /// Move `epoch`'s `Active` phase to `to`; `false` (nothing changed) when `epoch` does not own it.
    fn leave_active(&self, epoch: u32, to: NativeSessionPhase) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !matches!(state.phase, NativeSessionPhase::Active { epoch: active, .. } if epoch != 0 && active == epoch)
        {
            return false;
        }
        state.phase = to;
        true
    }

    /// Whether an off-frame `Unload` owns the native object ([`NativeSessionPhase::Releasing`]):
    /// [`crate::sink_fence::SinkFence`] answers the fenced sink for the main thread while this holds.
    pub fn native_releasing(&self) -> bool {
        matches!(
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .phase,
            NativeSessionPhase::Releasing { .. }
        )
    }

    /// Whether a timed-out Load is parked ([`NativeSessionPhase::Abandoned`]): the native object
    /// is still owned by a `Load` that has not returned, so every native start is refused.
    pub fn native_load_abandoned(&self) -> bool {
        matches!(
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .phase,
            NativeSessionPhase::Abandoned { .. }
        )
    }

    /// Test-only: drop an abandoned phase that a failing test left behind, which
    /// [`reset_session`](Self::reset_session) deliberately preserves.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_force_native_idle(&self) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase = NativeSessionPhase::Idle;
    }

    /// Whether this exact object crossed firmware's synchronous unload-complete callback path.
    /// This is one mandatory lifecycle assertion, separate from the native interposer's admission
    /// and in-flight proof. A missing/renumbered callback quarantines the object rather than D1.
    pub fn native_unload_completed(&self, epoch: u32) -> bool {
        matches!(
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .phase,
            NativeSessionPhase::Unloaded { epoch: e }
                | NativeSessionPhase::Releasing { epoch: e, unload_completed: true }
                if e == epoch
        )
    }

    /// Fence pre-seek presentations while retaining non-frame Load/error callbacks. Locking is a
    /// drain barrier for a type-0 callback already inside the old timeline.
    pub fn begin_native_media_discontinuity(&self, epoch: u32) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match &mut state.phase {
            NativeSessionPhase::Active {
                epoch: active,
                presentation_gate,
                ..
            } if epoch != 0 && *active == epoch => {
                *presentation_gate = NativePresentationGate::Disarmed;
                true
            }
            _ => false,
        }
    }

    /// Admit presentation callbacks again after the new timeline's first keyframe is identified.
    #[cfg(feature = "test-support")]
    pub fn arm_native_presentations(&self, epoch: u32) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match &mut state.phase {
            NativeSessionPhase::Active {
                epoch: active,
                presentation_gate,
                ..
            } if epoch != 0 && *active == epoch => {
                *presentation_gate = NativePresentationGate::Armed;
                true
            }
            _ => false,
        }
    }

    /// Start a two-phase callback admission around the exact post-seek keyframe Feed. No lock is
    /// held across FFI: firmware may re-enter synchronously on another release even though the
    /// audited 4.10.2 path currently dispatches through a GStreamer worker.
    pub fn begin_native_presentation_probe(&self, epoch: u32) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match &mut state.phase {
            NativeSessionPhase::Active {
                epoch: active,
                presentation_gate,
                ..
            } if epoch != 0 && *active == epoch => {
                *presentation_gate = NativePresentationGate::PendingArm { latched: None };
                true
            }
            _ => false,
        }
    }

    /// Commit an accepted Feed and replay at most the newest presentation which raced its return.
    pub fn commit_native_presentation_probe(
        &self,
        epoch: u32,
        replay: impl FnOnce(i64),
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let NativeSessionPhase::Active {
            epoch: active,
            presentation_gate,
            ..
        } = &mut state.phase
        else {
            return false;
        };
        if epoch == 0 || *active != epoch {
            return false;
        }
        let NativePresentationGate::PendingArm { latched } = *presentation_gate else {
            return false;
        };
        *presentation_gate = NativePresentationGate::Armed;
        if let Some(num) = latched {
            replay(num);
        }
        true
    }

    /// Discard callbacks observed while a BufferFull/error Feed was outstanding and keep the old
    /// timeline fenced. The retained AU will open a fresh probe on its next attempt.
    pub fn reject_native_presentation_probe(&self, epoch: u32) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match &mut state.phase {
            NativeSessionPhase::Active {
                epoch: active,
                presentation_gate,
                ..
            } if epoch != 0
                && *active == epoch
                && matches!(presentation_gate, NativePresentationGate::PendingArm { .. }) =>
            {
                *presentation_gate = NativePresentationGate::Disarmed;
                true
            }
            _ => false,
        }
    }

    /// Run an event only while its firmware-provided callback context owns this session.
    pub fn with_native_session<R>(
        &self,
        epoch: u32,
        class: NativeEventClass,
        num: i64,
        event: impl FnOnce() -> R,
    ) -> Option<R> {
        self.with_native_session_scoped(epoch, class, num, |_| event())
    }

    /// [`with_native_session`](Self::with_native_session), telling the event whose state it may
    /// write: [`EventScope::Live`] for the session that owns the process-long diagnostics,
    /// [`EventScope::Release`] for the lifecycle evidence of an object already handed to a release
    /// (the main thread reset that state for the NEXT session before this callback arrived).
    pub fn with_native_session_scoped<R>(
        &self,
        epoch: u32,
        class: NativeEventClass,
        num: i64,
        event: impl FnOnce(EventScope) -> R,
    ) -> Option<R> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let NativeSessionPhase::Abandoned { epoch: abandoned } = state.phase {
            // The release of an abandoned object needs its own UNLOADCOMPLETED evidence; nothing
            // else from that object may reach the process-long state a later session will own.
            if epoch == 0 || abandoned != epoch || class != NativeEventClass::UnloadCompleted {
                return None;
            }
            let result = event(EventScope::Release);
            state.phase = NativeSessionPhase::Unloaded { epoch };
            return Some(result);
        }
        if let NativeSessionPhase::Releasing { epoch: releasing, unload_completed } = &mut state.phase {
            // The same rule for an object a real stop handed to the off-frame release: the main
            // thread has already reset the state this callback would write into for the NEXT
            // session, so only the release's own lifecycle evidence gets through.
            if epoch == 0 || *releasing != epoch || class != NativeEventClass::UnloadCompleted {
                return None;
            }
            let result = event(EventScope::Release);
            *unload_completed = true;
            return Some(result);
        }
        let NativeSessionPhase::Active {
            epoch: active,
            presentation_gate,
            ..
        } = &mut state.phase
        else {
            return None;
        };
        if epoch == 0 || *active != epoch {
            return None;
        }
        if class == NativeEventClass::Presentation {
            match presentation_gate {
                NativePresentationGate::Armed => {}
                NativePresentationGate::Disarmed => return None,
                NativePresentationGate::PendingArm { latched } => {
                    *latched = Some(num);
                    return None;
                }
            }
        }
        let result = event(EventScope::Live);
        if class == NativeEventClass::UnloadCompleted {
            state.phase = NativeSessionPhase::Unloaded { epoch };
        }
        Some(result)
    }
}
