//! **The flight**: the ONE mechanism by which a route-changing PMS action leaves the frame thread
//! and lands back on it. A flight has three steps, and the type system and the control reducer
//! keep each on its own thread:
//!
//! 1. **Plan, on the frame thread.** [`super::decision::execute_retranscode_claim`] (a claimed
//!    track pick / enhancement toggle / quality change), [`super::decision::execute_recover_original_claim`]
//!    (the viewer's Original pick, and the Auto watchdog's HLS-to-Original handoff),
//!    [`super::decision::execute_auto_hls_claim`] (the Auto watchdog's Original-to-HLS fallback),
//!    [`super::decision::execute_adaptive_reload_claim`] (an adaptive refresh) and
//!    [`super::decision::dispatch_transcode_seek`] (a seek on a transcode),
//!    [`super::decision::dispatch_rollback_rebase`] (the way back from a failed Original trial),
//!    [`super::decision::dispatch_unopened_auto_hls`] (an Auto Original that never opened) and
//!    [`super::decision::dispatch_resume_rebase`] (the cold resume of a freshly resolved transcode,
//!    and the foreground restore's resume of a parked one),
//!    read the
//!    `PlaybackSession` and capture everything the server round trip needs as OWNED values ([`ClaimWork`], [`RetranscodeFallback`], the plan types behind them) — the worker
//!    never sees a `PlaybackSession`.
//! 2. **PMS, on a worker.** [`spawn_flight`] runs [`run_retranscode_claim_worker`] (handing it the
//!    [`OffFrame`](plx_base::task::OffFrame) token the frame thread cannot mint) under
//!    `catch_unwind` and posts the verdict ([`RetranscodeClaimResult`]) to a one-slot mailbox,
//!    [`RETRANSCODE_CLAIM_SLOT`]. A worker does PMS. A claim's and an Original recovery's worker
//!    commit their replacement as the route's encoder (so their discard stops it AND the one it
//!    replaced); a rebuild's worker and the automatic Original-to-HLS fallback's commit nothing, the
//!    commit being a mutex operation done at the install.
//! 3. **Install, on the frame thread.** The pump drains the mailbox once a frame
//!    ([`take_ready_flight`]) and applies the verdict to the session. A claim comes back as the
//!    tail `run_claim_tail` runs; a seek comes back as a [`SeekVerdict`] for the pump to reload on;
//!    a RECOVERY comes back as a [`RecoveryVerdict`] through its own drain
//!    ([`take_ready_recovery_flight`]), taken ahead of the pump's failure branches because the
//!    Engine it replaces has failed and the playing branch never runs for it. A COLD RESUME is a
//!    recovery by reducer phase (the `Prepared` start transaction its plan landed becomes
//!    `Preparing(serial)`) with NO Engine at all: the app loop drains it (`player::land_resume`),
//!    the HUD reads `Resolving` through [`engineless_flight_outstanding`], and the same drain reaps a
//!    landing a stale flight left ([`discard_stale_flight_landing`]) because no pump will. So is
//!    the FOREGROUND restore's resume (the foreground machine, `player::lifecycle`, waits on it the
//!    way it waits on a pending Load and drains it with the same `land_resume`) and the rollback of
//!    an Original trial whose Load failed before an Engine existed
//!    ([`dispatch_engineless_rollback_rebase`], drained by `player::land_engineless_rollback`).
//!    The foreground restore's resume may also find an Original TRIAL's transaction (the OS suspended
//!    mid-trial); see the invariant below on what the flight leaves of the trial.
//!
//! Invariants this module holds (each has a test in `decision_audio_enhancement_tests.rs`, and the
//! pump-level ones in `player::pump::flight_tests`):
//!
//! - **The reducer stays in the flight's phase from the dispatch until the install.** A claim holds
//!   `ControlPhase::Applying(serial)`, a seek holds `Preparing(serial)` (a start transaction it
//!   owns), and so does a recovery (the transaction the failed start already held, or a fresh one
//!   from `Stable`; `recovery_flight_outstanding` tells the pump to WAIT on it, its failure flags
//!   being up for the whole flight, and `fail_current_engine` ends it like any flight). Nothing else can claim, seek in place or reload meanwhile; a worker that cannot
//!   spawn settles at once and one that panics still lands (as a rejection), so the phase can never
//!   wait on a landing that will not arrive.
//! - **A stale landing stops what it created, and ONLY that.** A landing whose serial is no longer
//!   current ([`flight_is_current`]) or whose route ticket moved is discarded
//!   ([`RetranscodeClaimResult::discard`]). A claim's worker committed its replacement as the
//!   route's encoder, so its discard stops that AND the one it replaced; a rebuild's worker (and
//!   the automatic Original-to-HLS fallback's) committed nothing, so its discard stops the
//!   replacement alone and the encoder still on screen is untouched.
//! - **One flight at a time.** There is a single mailbox slot and a single flight phase;
//!   [`flight_outstanding`] is the question the pump's seek branches and the presentation hold
//!   ask. A second post into an occupied slot discards what it replaces.
//! - **A flight does not outlive its Engine.** A full teardown clears the slot. An app-switch suspend
//!   (`begin_engine_teardown(true)`) drops the outstanding flight of EVERY owner kind, because the
//!   foreground restore reserves its own transaction and a flight left behind would refuse it (and, an
//!   Engine gone, nothing would drain its landing): a seek's or a recovery's returns its start
//!   transaction to `Stable` (the trial's own `Prepared` inside an Original trial); a CLAIM's
//!   `Applying(serial)` settles to `Stable` the way `fail_current_engine` settles it, its hold is
//!   released, and the viewer's pick it was carrying (`claimed_user`) goes back to `pending_user` for
//!   the restored Engine to claim (an automatic claim owes nothing: its ticket died with the Engine). In
//!   every case the landing, posted or not yet, is discarded stopping what its owner kind registered,
//!   the late one by [`discard_stale_flight_landing`]. What the viewer is AT while a recovery flight is
//!   out is the flight's own offset ([`recovery_flight_offset_ns`]): nothing seeds the clock before the
//!   landing, so the suspend's snapshot (`player::intended_pos_ns`) reads it there.
//! - **A recovery flight over an Original trial leaves the trial's rollback alone.** A session
//!   suspended mid-trial is restored by a recovery flight (the foreground resume) that finds the
//!   TRIAL's start transaction (`OriginalTrial(Prepared)`, or `Failed`). Its phase is the trial's own
//!   `OriginalTrial(Preparing(serial))` rather than an ordinary `Preparing`, so the fence is the same
//!   (nothing else can claim, seek or reload; the three flight predicates answer for it) but the
//!   rollback snapshot (`pending_original`, a field no flight writes) is never erased: a landing hands
//!   the transaction back to the trial as `Prepared` (the candidate's projection and its remux follow
//!   the rebuilt stream, [`install_rebase`]), a refusal as `Failed`, a suspend as `Prepared` again,
//!   and a failed open of the rebuilt Original still rolls back to the retained route.
//!
//! [`FlightOwner`] names who owns a flight: `Claim` (a claimed route action, phase
//! `Applying(serial)`), `Start` (a seek on a transcode, the start transaction its plan reserved) and
//! `Recovery` (the replacement route a failed or suspended start is owed, phase `Preparing(serial)`
//! — or `OriginalTrial(Preparing(serial))` when the transaction is an Original trial's).

use super::decision::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// Who owns the flight that is outstanding.
pub(super) enum FlightOwner {
    /// A claimed route action (track pick, enhancement toggle, quality change, Original recovery —
    /// manual or the Auto watchdog's automatic handoff, its Original-to-HLS fallback (both a
    /// `ClaimedRouteAction` whose intent is `Automatic`) — adaptive refresh): the claim's
    /// `Applying(serial)` is the phase.
    Claim(ClaimedRouteAction),
    /// A seek on a transcode: the start transaction its plan reserved, `Preparing(serial)`.
    Start(RouteStartTransaction),
    /// A RECOVERY: the replacement route a failed start is owed (the rollback's rebase after a
    /// failed Original trial, the HLS fallback of a source that never opened) — and the cold
    /// resume's rebuild at the saved position, which is the same debt before the first Load: the
    /// start transaction a landed plan left `Prepared`. A failed Engine is still the pump's and
    /// waits for the landing; a cold resume has none. The transaction is `Preparing(serial)` like a
    /// seek's, and the landing is drained ahead of every failure branch
    /// ([`take_ready_recovery_flight`]), never by the playing branch's [`take_ready_flight`].
    Recovery(RouteStartTransaction),
}

impl FlightOwner {
    /// The serial `ControlPhase::Applying` carries while this flight is outstanding.
    pub(super) fn serial(&self) -> u64 {
        match self {
            Self::Claim(action) => action.serial(),
            Self::Start(ticket) | Self::Recovery(ticket) => ticket.serial,
        }
    }
}

/// Whether a flight is outstanding: a worker may be running a route-changing PMS action whose
/// landing the frame thread has not yet installed. The pump's own seek branches must skip while
/// this holds (see `flight_phase_open`, which answers it).
pub fn flight_outstanding() -> bool {
    flight_phase_open()
}

/// Whether the flight for exactly `serial` is still the current one — the check a landing must
/// pass before it may touch the `PlaybackSession`, and the presentation hold's test that its
/// flight is still flying. False after a teardown, a fresh playback request or the flight's own
/// settlement.
pub fn flight_is_current(serial: u64) -> bool {
    flight_phase_is(serial)
}

/// What a claimed `Retranscode` need to build if it fails and owes the displaced pick's own
/// reload, computed on the main thread (the worker never touches `PlaybackSession`) alongside the
/// primary attempt so the worker can run the whole "try, then maybe fall back" sequence without a
/// second main-thread round trip.
pub(super) enum RetranscodeFallback {
    RejectWith(&'static str),
    /// No PMS I/O — `run_legacy`'s own `LegacyAction::Native`. Staged on the main thread once the
    /// worker lands, since `stage_native_audio` writes `PlaybackSession`.
    Native { ordinal: i32, codec: String },
    Retranscode(plx_plex::plex::EncodeContract),
}

/// The worker's verdict, applied to `PlaybackSession` by [`take_ready_retranscode_claim`].
pub(super) enum RetranscodeClaimResult {
    Retranscode(AppliedRetranscode),
    /// An Original recovery's PMS half done ([`run_original_recovery`]) — an enhancement release,
    /// the viewer's Original pick or the Auto watchdog's handoff; the drain installs it
    /// ([`install_original_recovery`]) and the pump's `Original` tail reloads.
    OriginalRecovery(Box<OriginalRecoveryLanding>),
    /// A transcode rebuild's PMS half done ([`run_rebase`]); the drain installs it
    /// ([`install_rebase`]).
    Rebase(Box<RebaseLanding>),
    /// The automatic Original-to-HLS fallback's PMS half done ([`run_auto_hls`]); the drain
    /// installs it ([`install_auto_hls_outcome`]) and the pump reloads onto the HLS route.
    AutoHls(Box<AutoHlsLanding>),
    NativeAudio { ordinal: i32, codec: String },
    Rejected(&'static str),
}

pub(super) struct AutoHlsLanding {
    pub(super) plan: AutoHlsPlan,
    pub(super) outcome: AutoHlsOutcome,
}

pub(super) struct RebaseLanding {
    pub(super) plan: RebasePlan,
    pub(super) outcome: RebaseOutcome,
}

impl RebaseLanding {
    fn discard(self) {
        discard_rebase(self.plan, self.outcome);
    }
}

pub(super) struct OriginalRecoveryLanding {
    pub(super) plan: OriginalRecoveryPlan,
    pub(super) net: OriginalRecoveryNet,
}

impl RetranscodeClaimResult {
    /// A verdict that will never install: stop whatever encoder session its worker started, and
    /// the encoder it replaced — the worker committed the new one as the route's, so nothing else
    /// owns the old one any more and it would otherwise run on the server unowned.
    fn discard(self) {
        match self {
            Self::Retranscode(applied) => stop_discarded_landing(applied),
            Self::OriginalRecovery(landing) => {
                let OriginalRecoveryLanding { plan, net } = *landing;
                if let (OriginalRecoveryNet::Remux(prepared), Some(client)) = (net, plan.client) {
                    stop_encoder_session(client, prepared.replacement);
                    stop_encoder_session(client, plan.expected.encoder().to_owned());
                }
            }
            // The worker registered the replacement and committed NOTHING: the encoder on screen
            // is still the route's, so only the replacement is stopped.
            Self::Rebase(landing) => landing.discard(),
            // Likewise: the install commits, so only the replacement is stopped and the Original
            // on screen is untouched.
            Self::AutoHls(landing) => {
                let AutoHlsLanding { plan, outcome } = *landing;
                discard_auto_hls(plan, outcome);
            }
            Self::NativeAudio { .. } | Self::Rejected(_) => {}
        }
    }
}

/// What the claim worker tries first.
pub(super) enum ClaimWork {
    /// One `/decision` attempt for this contract ([`try_retranscode`]).
    Encode(plx_plex::plex::EncodeContract),
    /// A recovery back to the Original ([`run_original_recovery`]).
    RecoverOriginal(Box<OriginalRecoveryPlan>),
    /// A transcode rebuilt at an offset ([`run_rebase`]): an adaptive refresh, or a seek.
    Rebase(Box<RebasePlan>),
    /// The Auto watchdog's Original-to-HLS fallback ([`run_auto_hls`]).
    AutoHls(Box<AutoHlsPlan>),
}

/// One claimed `Retranscode`'s worker landing, gated for reuse the same way every other mailbox in
/// this module is: [`take_ready_retranscode_claim`] hands the pieces back to the pump exactly as
/// they were at claim time, and [`finish_route_action`]'s own `action.serial` check (inside
/// `run_claim_tail`) is what actually decides whether a stale result may still publish.
pub(super) struct RetranscodeClaimLanding {
    pub(super) owner: FlightOwner,
    pub(super) pending_seek: i64,
    pub(super) user_target: i64,
    pub(super) result: RetranscodeClaimResult,
}

pub(super) static RETRANSCODE_CLAIM_SLOT: std::sync::Mutex<Option<RetranscodeClaimLanding>> =
    std::sync::Mutex::new(None);
/// Whether [`RETRANSCODE_CLAIM_SLOT`] holds a landing. The pump drains the slot every frame of a
/// playing stream and almost always finds it empty, so that answer is this one load, not a lock.
/// Written only by [`post_claim_landing`] and [`take_claim_landing`], under the slot's guard.
static RETRANSCODE_CLAIM_LANDED: AtomicBool = AtomicBool::new(false);

/// Publish a landing for the frame thread's next drain.
pub(super) fn post_claim_landing(landing: RetranscodeClaimLanding) {
    let replaced = {
        let mut slot = RETRANSCODE_CLAIM_SLOT.lock().unwrap_or_else(|e| e.into_inner());
        let replaced = slot.replace(landing);
        RETRANSCODE_CLAIM_LANDED.store(true, Ordering::Release);
        replaced
    };
    // A landing nobody drained (its pump stopped, or the Engine went away) is stale by now; the
    // newest flight owns the slot, and what the old one registered must not run on unowned.
    if let Some(old) = replaced {
        old.result.discard();
    }
}

/// Take whatever landing is posted, or `None` — without touching the lock when nothing is. The one
/// way out of the slot: the drain, the teardown's discard and the tests' reset all come through
/// here, so the flag cannot drift from the slot.
pub(super) fn take_claim_landing() -> Option<RetranscodeClaimLanding> {
    if !RETRANSCODE_CLAIM_LANDED.load(Ordering::Acquire) {
        return None;
    }
    let mut slot = RETRANSCODE_CLAIM_SLOT.lock().unwrap_or_else(|e| e.into_inner());
    RETRANSCODE_CLAIM_LANDED.store(false, Ordering::Release);
    slot.take()
}

/// [`take_claim_landing`] for one family of owners: a recovery's landing is the pump's failure
/// branches' to drain, every other kind the playing branch's, and neither takes the other's.
fn take_claim_landing_for(recovery: bool) -> Option<RetranscodeClaimLanding> {
    if !RETRANSCODE_CLAIM_LANDED.load(Ordering::Acquire) {
        return None;
    }
    let mut slot = RETRANSCODE_CLAIM_SLOT.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_some_and(|l| matches!(l.owner, FlightOwner::Recovery(_)) != recovery) {
        return None;
    }
    RETRANSCODE_CLAIM_LANDED.store(false, Ordering::Release);
    slot.take()
}

/// The worker body: try the primary attempt, and on refusal run whichever fallback the claim owes
/// — all off the frame thread, all before anything reaches [`RETRANSCODE_CLAIM_SLOT`].
fn run_retranscode_claim_worker(
    off: &plx_base::task::OffFrame,
    inputs: Option<&RetranscodeClaimInputs>,
    work: ClaimWork,
    fallback: RetranscodeFallback,
) -> RetranscodeClaimResult {
    // A rebuild carries everything it needs in its plan; the other kinds are built only by
    // `execute_retranscode_claim`, which always captured inputs for them.
    if let ClaimWork::Rebase(plan) = work {
        let outcome = run_rebase(off, &plan);
        return match (outcome, fallback) {
            (RebaseOutcome::Refused, RetranscodeFallback::RejectWith(reason)) => RetranscodeClaimResult::Rejected(reason),
            (outcome, _) => RetranscodeClaimResult::Rebase(Box::new(RebaseLanding { plan: *plan, outcome })),
        };
    }
    // The automatic fallback carries its own inputs in its plan, like a rebuild.
    if let ClaimWork::AutoHls(plan) = work {
        return match run_auto_hls(off, &plan) {
            AutoHlsOutcome::Refused => RetranscodeClaimResult::Rejected(AUTO_HLS_REJECTED),
            outcome => RetranscodeClaimResult::AutoHls(Box::new(AutoHlsLanding { plan: *plan, outcome })),
        };
    }
    let inputs = inputs.expect("an encode or recovery flight is dispatched with its claim inputs");
    match work {
        ClaimWork::Rebase(_) | ClaimWork::AutoHls(_) => unreachable!("handled above"),
        ClaimWork::Encode(primary_contract) => {
            select_streams_for_encode(off, inputs);
            if let RetranscodeWorkerOutcome::Applied(applied) = try_retranscode(off, inputs, primary_contract) {
                return RetranscodeClaimResult::Retranscode(applied);
            }
        }
        ClaimWork::RecoverOriginal(plan) => {
            if let Some(net) = run_original_recovery(off, &plan) {
                return RetranscodeClaimResult::OriginalRecovery(Box::new(OriginalRecoveryLanding { plan: *plan, net }));
            }
            // The recovery's own selection PUT named the candidate's track; a rebuild owed to a
            // displaced pick names the session's, so it sends its own, once, like any other claim.
            if matches!(fallback, RetranscodeFallback::Retranscode(_)) {
                select_streams_for_encode(off, inputs);
            }
        }
    }
    match fallback {
        RetranscodeFallback::RejectWith(reason) => RetranscodeClaimResult::Rejected(reason),
        RetranscodeFallback::Native { ordinal, codec } => {
            RetranscodeClaimResult::NativeAudio { ordinal, codec }
        }
        RetranscodeFallback::Retranscode(contract) => match try_retranscode(off, inputs, contract) {
            RetranscodeWorkerOutcome::Applied(applied) => RetranscodeClaimResult::Retranscode(applied),
            RetranscodeWorkerOutcome::Refused => RetranscodeClaimResult::Rejected(LEGACY_REJECTED),
        },
    }
}

// A host test cannot politely exhaust the real thread limit (see `task`'s own module doc), and
// `spawn_small` fixes its stack size, so the `Some(usize::MAX / 2)` trick `task::tests` uses to
// force `spawn_with` to fail is not reachable from here. This is the same shape of seam
// `storage_worker::Writer::start_refused` uses for the analogous case: an explicit, test-only
// override, checked only in `cfg(test)` builds, so the shipping path is exactly `spawn_small`.
/// Test-only: a one-shot fault [`spawn_flight`] injects into itself, so a test can grade
/// its failure paths without exhausting the OS thread table or crashing inside `try_retranscode`.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    /// The next spawn reports refusal without spawning.
    SpawnRefusal,
    /// The next worker panics right before it would otherwise run.
    WorkerPanic,
}

/// The armed fault. Process-wide rather than thread-local: `WorkerPanic` is consumed on the
/// freshly spawned worker thread, not the test's own.
#[cfg(any(test, feature = "test-support"))]
static NEXT_FAULT: std::sync::Mutex<Option<Fault>> = std::sync::Mutex::new(None);

/// Test-only: arm `fault` for the next [`spawn_flight`].
#[cfg(test)]
pub(super) fn inject_next_fault(fault: Fault) {
    *NEXT_FAULT.lock().unwrap_or_else(|e| e.into_inner()) = Some(fault);
}

/// Test-only: consume the armed fault if it is `fault`, and answer whether it was.
#[cfg(any(test, feature = "test-support"))]
fn take_fault(fault: Fault) -> bool {
    let mut armed = NEXT_FAULT.lock().unwrap_or_else(|e| e.into_inner());
    if *armed == Some(fault) {
        *armed = None;
        true
    } else {
        false
    }
}

/// Test-only: disarm whatever fault a test left behind.
#[cfg(test)]
pub(super) fn clear_injected_fault() {
    *NEXT_FAULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub(super) fn spawn_flight(
    owner: FlightOwner,
    pending_seek: i64,
    user_target: i64,
    inputs: Option<RetranscodeClaimInputs>,
    work: ClaimWork,
    fallback: RetranscodeFallback,
) -> bool {
    #[cfg(any(test, feature = "test-support"))]
    if take_fault(Fault::SpawnRefusal) {
        return false;
    }
    plx_base::task::spawn_off_frame("retranscode-claim", move |off| {
        // catch_unwind OUTSIDE the mailbox write, like the resolve worker: a panicking attempt must
        // still land (as a `Rejected`) or `ControlPhase::Applying` waits forever for a mailbox
        // entry that will now never arrive.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(any(test, feature = "test-support"))]
            if take_fault(Fault::WorkerPanic) {
                panic!("forced retranscode worker panic (test)");
            }
            run_retranscode_claim_worker(off, inputs.as_ref(), work, fallback)
        }))
        .unwrap_or(RetranscodeClaimResult::Rejected(RETRANSCODE_WORKER_PANICKED));
        post_claim_landing(RetranscodeClaimLanding { owner, pending_seek, user_target, result });
    })
}

/// Discard whatever landing is sitting in the mailbox without applying it, stopping the encoder
/// session it started if it got that far. Called whenever the item this claim was for stops being
/// the one that's playing — a full teardown or a fresh playback request — since a landing that
/// arrives after that point belongs to a session nothing on screen refers to any more.
pub(super) fn discard_retranscode_claim_slot() {
    if let Some(landing) = take_claim_landing() {
        landing.result.discard();
    }
}

/// Discard the landing in the mailbox if its flight is no longer current, stopping what its worker
/// registered. The pump's drains do this for an Engine's flights; a flight with no Engine (a cold
/// resume, see [`resume_flight_outstanding`]) has no pump, so a Back, a suspend or a newer request
/// that ended it while the worker ran would leave the late landing parked — and its replacement
/// encoder running on the server — until some later flight replaced it. The app loop calls this
/// once a frame (`player::land_resume`); the check is one atomic load when the mailbox is empty.
pub fn discard_stale_flight_landing() {
    if !RETRANSCODE_CLAIM_LANDED.load(Ordering::Acquire) {
        return;
    }
    let serial = RETRANSCODE_CLAIM_SLOT.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|l| l.owner.serial());
    let Some(serial) = serial else { return };
    // Asked with the mailbox unlocked: the reducer's lock is never taken under it.
    if flight_is_current(serial) {
        return;
    }
    let stale = {
        let mut slot = RETRANSCODE_CLAIM_SLOT.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().is_some_and(|l| l.owner.serial() == serial) {
            RETRANSCODE_CLAIM_LANDED.store(false, Ordering::Release);
            slot.take()
        } else {
            None
        }
    };
    if let Some(landing) = stale {
        landing.result.discard();
    }
}

/// [`discard_retranscode_claim_slot`] under its other name, for an app-switch suspend that dropped
/// the outstanding flight of ANY owner (`begin_engine_teardown(true)`): the landing in the mailbox,
/// if the worker has posted it, is discarded now (stopping what its owner kind registered), and one
/// it has not posted yet is reaped by [`discard_stale_flight_landing`] (or the pump's drain), the
/// flight no longer being current.
pub(super) fn discard_flight_landing() {
    discard_retranscode_claim_slot();
}

/// Stop an encoder session without waiting on PMS: the frame thread reaches every caller of this
/// (a teardown, a fresh playback request, a stale drain, the reload's retirement), and
/// `transcode_stop` is a blocking round trip. It runs on a short-lived worker; if the OS refuses
/// the thread the stop still happens inline under the "(worker thread refused)" exception
/// ([`plx_base::task::spawn_small_or_inline`]), because a session left running is a server leak.
pub(super) fn stop_encoder_session(client: &'static plx_plex::plex::Client, session: String) {
    if session.is_empty() {
        return;
    }
    plx_base::task::spawn_small_or_inline(
        "retranscode-stop",
        const { &plx_base::task::BlockingLabel::new("encoder stop (worker thread refused)") },
        move || {
            let _ = client.transcode_stop(&session);
        },
    );
}

/// A landing that will never install: stop what the worker started AND the encoder it replaced —
/// the worker committed the new session as the route's encoder, so nothing else owns the old one
/// any more and it would otherwise run on the server unowned.
fn stop_discarded_landing(applied: AppliedRetranscode) {
    stop_encoder_session(applied.client, applied.qsess);
    stop_encoder_session(applied.client, applied.superseded);
}

/// The encoder an ACCEPTED landing replaced, waiting for the reload that moves the Engine off it.
/// Written by [`take_ready_retranscode_claim`] once the landing is installed; read by
/// [`retire_superseded_encoder`] after the pump has run the claim's tail.
static SUPERSEDED_ENCODER: std::sync::Mutex<Option<(&'static plx_plex::plex::Client, String)>> =
    std::sync::Mutex::new(None);

/// Stop the encoder an accepted claim replaced. The pump calls this AFTER the claim's reload, not
/// at the landing: until the reload the Engine (held at the claim offset) is still reading that
/// stream, and stopping it early kills the picture the viewer is looking at. A no-op when no
/// landing left one.
pub fn retire_superseded_encoder() {
    let pending = SUPERSEDED_ENCODER.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some((client, session)) = pending {
        stop_encoder_session(client, session);
    }
}

/// Move a claim's restore point onto the stream its landing installs. The reducer's restore point
/// must describe the stream the landing installs, not the one it replaces:
/// `claim_snapshot.projection` was captured BEFORE the worker ran, and publishing it as-is made a
/// later rejected claim reinstate the old route over an Engine playing the new one. Only the fields
/// `advance` names move; the snapshot keeps its revision, quality and every selection that was
/// current when the claim was built (a second edit queued mid-flight has already advanced `ps`,
/// and must not be recorded as applied). A claim settled in its own frame carries no snapshot.
fn advance_claim_snapshot(action: &mut ClaimedRouteAction, advance: impl FnOnce(&mut AppliedRouteProjection)) {
    if let Some(snapshot) = action.claim_snapshot.as_mut() {
        advance(&mut snapshot.projection);
    }
}

/// A landing the pump has to act on.
pub enum ReadyFlight {
    /// A claim's PMS half landed and was applied to the session; the pump runs the tail.
    Claim { action: ClaimedRouteAction, tail: ClaimTail, pending_seek: i64, user_target: i64 },
    /// A seek's rebuild landed. `serial` names the flight (its presentation hold), `target_ns` the
    /// seek it was dispatched for.
    Seek { serial: u64, target_ns: i64, verdict: SeekVerdict },
}

/// What the frame thread does with a seek's landing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeekVerdict {
    /// The replacement encoder is the route's now (`url` is the session's): cross the seek in the
    /// reducer and reload onto it.
    Install(String),
    /// A newer tap was waiting in the seek target, so this landing was discarded (its replacement
    /// stopped, the reducer back to `Stable`): the pump starts a fresh flight at the newest target.
    Superseded,
    /// PMS refused the rebuild (or the route moved under it): the reducer is `Stable` and the old
    /// stream carries on. The pump abandons the seek.
    Refused,
}

/// Drained once a frame by the pump, before it tries to claim a fresh action (a claim stays
/// `Applying` — see `claim_route_action` — and a seek stays `Preparing`, for as long as this
/// mailbox is empty, so nothing else can race either). `newer_seek_waiting` is the pump's one read
/// of `TX.seek_to_ns`: a tap that arrived during a seek's flight sits there untouched, and the
/// landing it outdates is discarded rather than installed (the viewer asked for somewhere else
/// since).
///
/// A claim's landing: applies the worker's session-projection fields, exactly where
/// `retranscode_as` used to write them inline, then hands back what `run_claim_tail` needs. A
/// seek's: installs the rebuild ([`install_rebase`]) and hands back the verdict.
pub fn take_ready_flight(ps: &mut PlaybackSession, newer_seek_waiting: bool) -> Option<ReadyFlight> {
    let landing = take_claim_landing_for(false)?;
    let serial = landing.owner.serial();
    // This landing's `ps`/route ownership is only valid if the exact flight it was dispatched for
    // is still the one the reducer is waiting on. `begin_engine_teardown(false)`/
    // `begin_playback_request` already clear the mailbox on their own transitions, but the worker
    // which built this landing could still have posted it in the gap between that clear and this
    // drain — `ps` may by then belong to an entirely different item, so nothing here may touch it.
    if !flight_is_current(serial) {
        landing.result.discard();
        return None;
    }
    end_flight(serial);
    let RetranscodeClaimLanding { owner, pending_seek, user_target, result } = landing;
    match owner {
        FlightOwner::Claim(action) => {
            let (action, tail) = install_claim_landing(ps, action, result);
            Some(ReadyFlight::Claim { action, tail, pending_seek, user_target })
        }
        FlightOwner::Start(ticket) => {
            let verdict = install_seek_landing(ps, ticket, result, newer_seek_waiting);
            Some(ReadyFlight::Seek { serial, target_ns: user_target, verdict })
        }
        FlightOwner::Recovery(_) => unreachable!("a recovery's landing is taken by take_ready_recovery_flight"),
    }
}

/// What the frame thread does with a recovery's landing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryVerdict {
    /// The replacement route is installed and its start transaction is `Prepared`: the pump
    /// reloads onto it at the landing's offset.
    Install,
    /// PMS refused (or the route moved under the worker): nothing was installed, the replacement
    /// the worker registered is stopped, and the pump raises the failure it was holding back.
    Refused,
}

/// A recovery's landing, drained.
pub struct RecoveryLanding {
    /// The recovery position the flight was dispatched for, in nanoseconds.
    pub offset_ns: i64,
    pub verdict: RecoveryVerdict,
}

/// Whether a recovery flight is outstanding — landed or not — so the pump's failure branches wait
/// instead of failing the Engine it will replace (`fail_current_engine` would end the flight).
pub fn recovery_flight_outstanding() -> bool {
    recovery_flight_open()
}

/// Whether the outstanding flight is a COLD RESUME's: a resolved transcode being rebuilt at the
/// saved position before its first Load, with no Engine for a pump to wait on. The app loop drains
/// it (`player::land_resume`) and the HUD reads `Resolving` through it; the pump's failure
/// branches never see one, because they only run for an Engine.
pub fn resume_flight_outstanding() -> bool {
    resume_flight_open()
}

/// Whether the outstanding flight is a recovery NO ENGINE waits on: a resume's rebuild (cold or
/// the foreground restore's) or the rollback of an Original trial whose Load failed while its
/// Engine was being built. The app loop drains it ([`take_ready_recovery_flight`] through
/// `player::land_resume` / `player::land_engineless_rollback`), the HUD reads `Resolving`, and an
/// app-switch suspend drops it. A pump never sees one.
pub fn engineless_flight_outstanding() -> bool {
    engineless_flight_open()
}

/// Where the outstanding RECOVERY flight rebuilds the route, in nanoseconds (`None` with no
/// recovery flight): the position the viewer is at while a cold or foreground resume, a rollback or
/// an unopened-source fallback is out. Nothing has seeded the clock before the landing, so a reader
/// that means "where is the viewer" asks this first (`player::intended_pos_ns`).
pub fn recovery_flight_offset_ns() -> Option<i64> {
    recovery_flight_offset()
}

/// Drained once a frame by the pump AHEAD of its failure branches. A landing whose flight is no
/// longer current (a teardown, a fresh playback request, a failure that ended it) is discarded
/// here — stopping what its worker registered — and answers `None`, like a landing that has not
/// arrived yet; the caller tells those apart with [`recovery_flight_outstanding`].
pub fn take_ready_recovery_flight(ps: &mut PlaybackSession) -> Option<RecoveryLanding> {
    let landing = take_claim_landing_for(true)?;
    let serial = landing.owner.serial();
    if !flight_is_current(serial) {
        landing.result.discard();
        return None;
    }
    let resume = resume_flight_is(serial);
    end_flight(serial);
    let RetranscodeClaimLanding { owner, user_target, result, .. } = landing;
    let FlightOwner::Recovery(ticket) = owner else {
        unreachable!("take_claim_landing_for(true) only takes a recovery's landing");
    };
    Some(RecoveryLanding { offset_ns: user_target, verdict: install_recovery_landing(ps, ticket, resume, result) })
}

/// A recovery's landing, applied. Every way out leaves the reducer out of `Preparing(serial)`:
/// `Prepared` when installed (the reload that follows claims the Load attempt), `Stable` when not
/// (the pump fails the Engine from there, as it did from the transaction the synchronous attempt
/// left).
fn install_recovery_landing(
    ps: &mut PlaybackSession,
    ticket: RouteStartTransaction,
    resume: bool,
    result: RetranscodeClaimResult,
) -> RecoveryVerdict {
    // The start transaction a landing that installs nothing gives back: a cold resume's was a
    // landing's `Prepared` one, which a refusal leaves `Failed` (what the inline attempt left).
    let owner = if resume { RebaseFor::Resume } else { RebaseFor::Rollback };
    match result {
        RetranscodeClaimResult::Rebase(landing) => {
            let RebaseLanding { plan, outcome } = *landing;
            // A cold resume is a recovery by reducer phase only: it is no rollback, so it reports
            // no delivery change and says nothing of one.
            let rollback = plan.owner == RebaseFor::Rollback;
            // `install_rebase` settles the start transaction itself on every refusal.
            match install_rebase(ps, plan, outcome) {
                Some(_) => {
                    if rollback {
                        crate::player::report::note_delivery_requested_for(
                            playback_trace_generation(),
                            crate::player::report::DeliveryClass::Hls,
                            crate::player::report::QualityClass::Unknown,
                            crate::player::report::DeliveryReason::OriginalOpenRollback,
                        );
                    }
                    RecoveryVerdict::Install
                }
                None => {
                    if rollback {
                        crate::player::log(ROLLBACK_REBASE_REFUSED);
                    }
                    RecoveryVerdict::Refused
                }
            }
        }
        RetranscodeClaimResult::AutoHls(landing) => {
            let AutoHlsLanding { plan, outcome } = *landing;
            let Some(installed) = install_auto_hls_outcome(ps, plan, outcome) else {
                release_rebase_start(owner, ticket);
                return RecoveryVerdict::Refused;
            };
            // Nothing is playing, so the Original it replaced has nothing left to feed: stop it
            // now, not after a reload that keeps it on screen.
            installed.retire_replaced();
            if !prepare_route_start(ticket) {
                crate::player::log("recovery: prepared PMS route lost its start transaction");
                return RecoveryVerdict::Refused;
            }
            RecoveryVerdict::Install
        }
        // A refused rebuild (the worker answered `Rejected`), a panicked worker, or a kind a
        // recovery never dispatches: nothing installs.
        other => {
            other.discard();
            release_rebase_start(owner, ticket);
            RecoveryVerdict::Refused
        }
    }
}

/// A seek's landing, applied. Every way out leaves the reducer out of `Preparing(serial)`.
fn install_seek_landing(
    ps: &mut PlaybackSession,
    ticket: RouteStartTransaction,
    result: RetranscodeClaimResult,
    newer_seek_waiting: bool,
) -> SeekVerdict {
    let settled = |newer: bool| if newer { SeekVerdict::Superseded } else { SeekVerdict::Refused };
    match result {
        RetranscodeClaimResult::Rebase(landing) => {
            if newer_seek_waiting {
                // The worker committed nothing, so the encoder on screen is untouched and the
                // reducer's `Preparing` is the whole of what is owed back.
                landing.discard();
                let _ = reject_route_start_preparation(ticket);
                return SeekVerdict::Superseded;
            }
            let RebaseLanding { plan, outcome } = *landing;
            // `install_rebase` settles the start transaction itself on every refusal.
            match install_rebase(ps, plan, outcome) {
                Some(installed) => SeekVerdict::Install(installed.url),
                None => SeekVerdict::Refused,
            }
        }
        // A panicked worker, or a kind a seek never dispatches: nothing installs.
        other => {
            other.discard();
            let _ = reject_route_start_preparation(ticket);
            settled(newer_seek_waiting)
        }
    }
}

/// A claim's landing, applied: the session-projection fields the worker's verdict changes, and
/// the tail the pump runs.
fn install_claim_landing(
    ps: &mut PlaybackSession,
    mut action: ClaimedRouteAction,
    result: RetranscodeClaimResult,
) -> (ClaimedRouteAction, ClaimTail) {
    let tail = match result {
        RetranscodeClaimResult::Retranscode(applied) => {
            // The worker's own `replace_active_encoder_for`/`replace_active_hls_for` check ran
            // before it committed; re-check here against the ticket it actually got back, since a
            // same-item route change (a concurrent ABR commit) can still move the route in the gap
            // between that commit and this drain (finding: "the mailbox is never... generation
            // checked"). A stale landing must not install a session projection nothing points at
            // any more, and must stop the encoder session it started instead of leaking it.
            if is_worker_ticket_current(&applied.ticket) {
                advance_claim_snapshot(&mut action, |p| apply_retranscode_outcome_to_projection(p, &applied));
                install_retranscode_outcome(ps, &applied);
                if !applied.superseded.is_empty() {
                    *SUPERSEDED_ENCODER.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some((applied.client, applied.superseded));
                }
                ClaimTail::Retranscode
            } else {
                stop_discarded_landing(applied);
                ClaimTail::Rejected(RETRANSCODE_REJECTED)
            }
        }
        RetranscodeClaimResult::OriginalRecovery(recovery) => {
            let OriginalRecoveryLanding { plan, net } = *recovery;
            // No `advance_claim_snapshot`: a recovery settles through its own `PendingOriginal`
            // (the phase leaves `Applying` inside the install), not `finish_route_action`. The
            // install refuses a landing the route moved past, and stops what it registered; the
            // rebuild a displaced pick would be owed is not tried against a route that has moved.
            match install_original_recovery(ps, plan, net) {
                Some(reload) => ClaimTail::Original(reload),
                None => ClaimTail::Rejected(ENHANCEMENT_REJECTED),
            }
        }
        RetranscodeClaimResult::Rebase(landing) => {
            let RebaseLanding { plan, outcome } = *landing;
            // The install commits the replacement (gated on the plan's ticket) and, being a claim's,
            // leaves publication to `finish_route_action`: the restore point moves onto the new
            // stream here, like every other landing's.
            match install_rebase(ps, plan, outcome) {
                Some(installed) => {
                    advance_claim_snapshot(&mut action, |p| installed.apply_to_projection(p));
                    ClaimTail::Adaptive
                }
                None => ClaimTail::Rejected(ADAPTIVE_REJECTED),
            }
        }
        RetranscodeClaimResult::AutoHls(landing) => {
            let AutoHlsLanding { plan, outcome } = *landing;
            // The install commits the replacement (gated on the plan's ticket) and, being a claim's,
            // leaves publication to `finish_route_action`: the restore point moves onto the new
            // stream here. The Original it replaced keeps feeding the Engine until the reload, so
            // it is retired after it ([`retire_superseded_encoder`]), like every claim's.
            match install_auto_hls_outcome(ps, plan, outcome) {
                Some(installed) => {
                    advance_claim_snapshot(&mut action, |p| installed.apply_to_projection(p));
                    if let Some(replaced) = installed.superseded() {
                        *SUPERSEDED_ENCODER.lock().unwrap_or_else(|e| e.into_inner()) = Some(replaced);
                    }
                    ClaimTail::Retranscode
                }
                None => ClaimTail::Rejected(AUTO_HLS_REJECTED),
            }
        }
        RetranscodeClaimResult::NativeAudio { ordinal, codec } => {
            // `stage_native_audio` moves `ps.stream_acodec`; it touches no other projection field
            // (the desired audio index and cues live outside it).
            advance_claim_snapshot(&mut action, |p| p.stream_acodec = codec.clone());
            crate::player::stage_native_audio(ps, ordinal, &codec);
            ClaimTail::NativeAudio
        }
        RetranscodeClaimResult::Rejected(reason) => ClaimTail::Rejected(reason),
    };
    (action, tail)
}
