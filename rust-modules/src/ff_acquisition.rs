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
//! `arm_active_stall_guard` evaluation runs, from inputs it samples itself rather than inputs a
//! caller hands it.
use super::*;

/// The acquisition policy for one `hls_demux_segment` fetch. Its payload (`Policy`) is private to
/// this module, so nothing outside — including `ff` itself — can name `Policy::Active(..)` or
/// build an `ActiveAcquisition` by hand. The type name is `pub(super)` so `ff` can hold values of
/// it and pass them to `hls_demux_segment`; producing one is only ever possible through this
/// module's own constructors.
pub(super) struct SegmentAcquisition(Policy);

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
    Fixed,
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
    /// read ahead of the identical rung and must be classified identically. Takes the
    /// authoritative adaptive context itself — `hls_demux`'s own `adaptive` state, as
    /// `Some(&Controller)` for an ABR playback or `None` for non-adaptive playback — rather than a
    /// caller-derived `at_floor` bool, and samples the live reserve
    /// (`hls_buffer_snapshot(None).buffered_ms()`) and hold (`SHARED.hls_rebuffering`) itself
    /// inside this call, so a caller cannot pass stale or fabricated inputs. Non-adaptive playback
    /// is this same constructor given `None`; there is deliberately no separate public `fixed()`.
    ///
    /// **Out of scope, by design, not oversight:** this only ever arms against the terminal
    /// reserve boundary observed at the next AVIO callback. It does not wake a transport read that
    /// is currently blocked — the callback has to be re-entered to see a new hold — and it does
    /// not arm on the HTTP open/connect+headers leg or the `NotReady` wait inside
    /// `hls_demux_segment`, both of which precede any AVIO read and so precede any body existing.
    /// FFmpeg's probe reads DO go through this same guarded AVIO `read_cb`, so probing itself is
    /// covered; only those pre-body legs, and a read already blocked when a hold transition
    /// arrives, are not. Treating "no observation yet" as a completed zero-byte transfer is also
    /// out of scope. Those remain the demux loop's and `StallGuard`'s own concerns.
    pub(super) fn for_cursor(controller: Option<&crate::abr::Controller>) -> Self {
        match controller {
            Some(controller) => Self::active(
                hls_buffer_snapshot(None).buffered_ms(),
                controller.current().at_floor(),
                SHARED.hls_rebuffering.load(Ordering::Acquire),
            ),
            None => SegmentAcquisition(Policy::Fixed),
        }
    }

    /// Private: the only callers are [`SegmentAcquisition::for_cursor`] and, under `#[cfg(test)]`,
    /// [`SegmentAcquisition::for_test`]. Not reachable from `ff`.
    fn active(reserve_ms: Option<i64>, at_floor: bool, already_held: bool) -> Self {
        SegmentAcquisition(Policy::Active(ActiveAcquisition {
            reserve_deadline: ReserveDeadlineState::new(None, false),
            stall: arm_active_stall_guard(reserve_ms, at_floor, already_held),
        }))
    }

    pub(super) fn candidate(deadline: ReserveDeadlineState) -> Self {
        SegmentAcquisition(Policy::Candidate(deadline))
    }

    /// The armed guard, if any. Read-only: a caller can log `reserve_ms_at_start` on abort, or (in
    /// a test) assert whether an injected acquisition was armed, without being able to construct
    /// or replace the guard itself.
    pub(super) fn stall(&self) -> Option<StallGuard> {
        match &self.0 {
            Policy::Active(active) => active.stall,
            Policy::Candidate(_) | Policy::Fixed => None,
        }
    }

    pub(super) fn into_parts(self) -> (ReserveDeadlineState, Option<StallGuard>) {
        match self.0 {
            Policy::Active(active) => (active.reserve_deadline, active.stall),
            Policy::Candidate(deadline) => (deadline, None),
            Policy::Fixed => (ReserveDeadlineState::new(None, false), None),
        }
    }

    /// Test-only escape hatch: build an `Active` acquisition from explicit inputs rather than
    /// `for_cursor`'s own live sampling, so a test can hold the buffer/floor/hold state fixed
    /// without wiring up a real cursor, controller and `SHARED` state. Never reachable outside
    /// `#[cfg(test)]`; the shipping constructor is always `for_cursor`.
    #[cfg(test)]
    pub(super) fn for_test(reserve_ms: Option<i64>, at_floor: bool, already_held: bool) -> Self {
        Self::active(reserve_ms, at_floor, already_held)
    }
}
