//! Addressed adaptive trial phases. The backend keeps source handles and staged media;
//! this contract carries only identities, normalized measurements, and ordering.

use crate::{AttemptId, SourceKey};
use crate::demux::AddressedSampler;
use std::time::Duration;

/// A worker-local trial serial under the route owner's existing playback attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TrialId { pub attempt: AttemptId, pub serial: u64 }

impl TrialId {
    pub const fn new(attempt: AttemptId, serial: u64) -> Self { Self { attempt, serial } }
}

/// One completed or abandoned acquisition under the route owner's attempt.
/// Replies from a superseded read cannot become evidence for a later round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RoundId { pub attempt: AttemptId, pub serial: u64 }

impl RoundId {
    pub const fn new(attempt: AttemptId, serial: u64) -> Self { Self { attempt, serial } }
}

/// Physical work on the active cursor. The feed owns its cursor, access units,
/// timeline, pause cursor, and prefetch slot; these instructions carry none of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveAction {
    Read { round: RoundId, at_floor: Option<bool> },
    Feed { round: RoundId },
    Buffer { round: RoundId },
    Pause { round: RoundId },
    RollbackDuration { round: RoundId },
    Prefetch { round: RoundId, at_floor: Option<bool> },
}

impl ActiveAction {
    pub const fn round(self) -> RoundId {
        match self {
            Self::Read { round, .. } | Self::Feed { round }
            | Self::Buffer { round } | Self::Pause { round }
            | Self::RollbackDuration { round } | Self::Prefetch { round, .. } => round,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveObservation {
    Ready { round: RoundId, acquisition: Acquisition, delivered: bool, abandoned: bool },
    Ended { round: RoundId },
    Fed { round: RoundId },
    Buffer { round: RoundId, fact: BufferFact },
    Pause { round: RoundId, paused: bool, sequence: u64, completed_ms: Option<u64> },
    RollbackDuration { round: RoundId, duration_ms: Option<u32> },
    Prefetched { round: RoundId },
}

impl ActiveObservation {
    pub const fn round(self) -> RoundId {
        match self {
            Self::Ready { round, .. } | Self::Ended { round }
            | Self::Fed { round } | Self::Buffer { round, .. }
            | Self::Pause { round, .. } | Self::RollbackDuration { round, .. }
            | Self::Prefetched { round } => round,
        }
    }
}

pub trait ActiveFeedPort {
    type Error;
    fn step_active(&mut self, action: ActiveAction) -> Result<ActiveObservation, Self::Error>;
    /// The controller's HLS prime runway after a completed round or a committed trial, in
    /// milliseconds: the physical feed keeps it, because the feed's own prime reads it back.
    fn note_prime_runway(&mut self, ms: i64);
}

/// Instructions for the existing feed worker. A source is an opaque local key;
/// its URL and authentication remain in that worker's private source binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedAction {
    CheckAbort { trial: TrialId },
    CheckExpired { trial: TrialId },
    BudgetSnapshot { trial: TrialId },
    PrimeDeadline { trial: TrialId },
    DeadlineFinal { trial: TrialId, snapshot: Option<std::time::Instant> },
    Open { trial: TrialId, source: SourceKey },
    ReadNext { trial: TrialId },
    AcquireFirst {
        trial: TrialId,
        media_budget: Option<Duration>,
        yields_to_rebuffer: bool,
        started_ns: i64,
        clock: TrialClock,
    },
    AcquireFollowup { trial: TrialId },
    StageAcquired { trial: TrialId },
    CheckFollowupReserve { trial: TrialId },
    QueueStaged { trial: TrialId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialClock { Playhead, UnpausedElapsed }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomaticTransition { Original, QualityUp, QualityDown }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdaptiveDirection { Up, Down }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionReadout {
    pub direction: AdaptiveDirection,
    pub target_kbps: u32,
    pub refreshing: bool,
    pub committed: Option<bool>,
}

/// Complete capacity estimate carried between media opens. The sink timestamps
/// this snapshot at receipt, and `samples == 0` means no usable prior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeedReadout {
    pub fast_kbps: u32,
    pub slow_kbps: u32,
    pub uncertainty_pm: u32,
    pub samples: u32,
}

/// A [`SeedReadout`] with the wall-clock milliseconds since it was last observed. The sink keeps the
/// estimate; the common adaptive policy ages it, so the physical transport neither chooses nor
/// copies that policy.
pub type CarriedSeed = (SeedReadout, u64);

/// Published only after queued media and the AQ-guarded route/controller
/// commit. All fields belong to the accepted generation, never a grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitReadout {
    pub trace_generation: u32,
    pub direction: AdaptiveDirection,
    pub target_kbps: u32,
    pub refreshing: bool,
    pub declared_bps: u64,
    pub observed_width: i32,
    pub observed_height: i32,
}

/// What the adaptive worker last measured and decided, as one `Copy` photograph: the Auto-quality
/// fields the diagnostics panel and the error context show. The worker is the only writer of every
/// member; it accumulates them and ships the whole value through [`AdaptiveEffects::readout`], and
/// the player's readout keeps the last one it took in. [`AbrReadout::IDLE`] is the value before any
/// publication (and after the session's reset): `-1` is "no measurement yet" and `0` is "nothing
/// decided", exactly as the diagnostics mirror these members came from held them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbrReadout {
    /// The last completed segment's measured media rate; `-1` before one.
    pub media_kbps: i64,
    /// The latest measured body throughput; `-1` before a complete measurement.
    pub net_kbps: i64,
    /// The normalized content reserve; `-1` while unknown.
    pub buffer_ms: i64,
    /// Total segment acquisition over media duration, per mille; `-1` before a sample (HLS only).
    pub ratio_pm: i64,
    /// A `readout::ABR_ACTION_*` code; `0` until the worker has decided anything.
    pub action: u8,
    /// The candidate rung of `action`, kbit/s; `0` when it names none.
    pub target_kbps: i64,
    /// Wall milliseconds an unsafe Original deficit has held.
    pub unsafe_deficit_ms: i64,
    /// The controller's own model state, each `-1` until it has produced it (`slope_ms_per_s` is `0`):
    /// the safe budget, the unconstrained operating point, the delivery estimate's dispersion and
    /// sample count, the buffer's slope, the starvation horizon in seconds (`-1` = no deficit), the
    /// candidate's predicted production cost and the risk score.
    pub safe_kbps: i64,
    pub optimal_kbps: i64,
    pub unc_pm: i64,
    pub samples: i64,
    pub slope_ms_per_s: i64,
    pub starve_secs: i64,
    pub pred_pm: i64,
    pub risk: i64,
    /// Why the last steady-state decision went the way it did (the common reason code); `0` while
    /// nothing has decided anything.
    pub why: u8,
}

impl AbrReadout {
    /// Before any publication, and after the session's reset.
    pub const IDLE: Self = Self {
        media_kbps: -1, net_kbps: -1, buffer_ms: -1, ratio_pm: -1, action: 0, target_kbps: 0,
        unsafe_deficit_ms: 0, safe_kbps: -1, optimal_kbps: -1, unc_pm: -1, samples: -1,
        slope_ms_per_s: 0, starve_secs: -1, pred_pm: -1, risk: -1, why: 0,
    };

    /// The action the panel shows, given what the demux published beside it. A started controller
    /// (`mode != 0`) that has decided nothing yet is holding its rung: the demux's start used to
    /// store `ABR_ACTION_STEADY` in the member, where the session's reset stores `0`, and the panel
    /// words the two differently ("hold current" and "starting").
    pub fn action_beside(&self, demux: crate::feed::DemuxAbr) -> u8 {
        if self.action == 0 && demux.mode != 0 {
            crate::readout::ABR_ACTION_STEADY
        } else {
            self.action
        }
    }
}

impl Default for AbrReadout {
    fn default() -> Self { Self::IDLE }
}

/// One edit of the typed reason the last Original source experiment or open failed, which the
/// player's readout owns. It is an EDIT, not a photograph: the readout keeps the reason across the
/// engine reload that restores HLS (the rollback must not erase its own cause) and drops it with the
/// playback, so a value that merely arrives with every shipment would either replay a reason the
/// player has since cleared or, on a new open's idle shipment, wipe one it should keep. Each edit
/// replaces the whole pair, so the newest of a run of them stands for all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureEdit {
    /// The Original failed: `kind` is a `player::ABR_FAILURE_ORIGINAL_*` code, `status` the HTTP
    /// status when one was observed (`0` when none was; a negative one reads as none).
    Noted { kind: u8, status: i32 },
    /// A new source attempt or a new probe supersedes the reason, or the playback is over.
    Cleared,
}

trait LeaseGuard: Send {}
impl<T: Send> LeaseGuard for T {}

/// An addressed physical actuator guard. The backend owns the guard's
/// implementation; dropping this move-only handle releases that exact lease.
/// No process registry or concrete backend type crosses the contract.
pub struct LeaseHandle {
    token: u64,
    _guard: Box<dyn LeaseGuard>,
}

impl LeaseHandle {
    pub fn new<T: Send + 'static>(token: u64, guard: T) -> Self {
        Self { token, _guard: Box::new(guard) }
    }

    pub const fn token(&self) -> u64 { self.token }
}

/// The physical implementation releases its publication fence only through
/// this consuming settlement. Dropping the handle leaves the fence armed for
/// teardown, including on a post-queue terminal error.
pub trait FenceSettlement: Send {
    fn settle(self: Box<Self>);
}

pub struct FenceHandle(Box<dyn FenceSettlement>);

impl FenceHandle {
    pub fn new<T: FenceSettlement + 'static>(fence: T) -> Self {
        Self(Box::new(fence))
    }

    pub fn settle(self) { self.0.settle(); }
}

/// Effects emitted by the adaptive worker after a completed observation or an
/// accepted publication. The session owns this sink; the feed supplies only
/// physical media and queue observations.
pub trait AdaptiveEffects: Send {
    fn seed(&mut self, readout: SeedReadout);
    fn committed(&mut self, readout: CommitReadout);
    /// The worker's accumulated [`AbrReadout`], after any member of it changed. The whole value
    /// crosses each time, so a sink that drops one loses nothing the next does not carry.
    fn readout(&mut self, readout: AbrReadout);
    /// The worker's Original probe noted or cleared the reason an Original attempt failed. Unlike
    /// [`Self::readout`] these are not a photograph to replace: the sink numbers each edit, and the
    /// player applies the newest one it has not applied yet, so a later shipment that only carries
    /// other members never replays it. A run of edits between two drains collapses to its last one,
    /// which loses nothing because every edit replaces the whole pair; a consumer that needs to see
    /// each note would be written against a promise this does not keep.
    fn failure(&mut self, edit: FailureEdit);
}

/// Physical automatic-actuator lease and the existing queue abort fence. The
/// policy owner holds the lease across blocking work and enters `gate` only for
/// the short route/controller publication closure.
pub trait AutomaticPort {
    type Lease;
    fn begin(&mut self, kind: AutomaticTransition, expected_user_sequence: u64)
        -> Option<Self::Lease>;
    fn authorize(&mut self, lease: &Self::Lease) -> bool;
    fn aborted(&self) -> bool;
    fn gate(&mut self) -> &mut dyn PublicationGate;
}

pub trait TrialActuatorPort: AutomaticPort {
    type TrialReserve;
    fn arm_trial_reserve(&mut self, runway_ms: i64, expected_user_sequence: u64)
        -> Option<Self::TrialReserve>;
    fn lease_token(&self, lease: &Self::Lease) -> u64;
    fn trial_token(&self, reserve: &Self::TrialReserve) -> u64;
}

/// Physical facts available to one adaptive decision on the existing feed
/// worker. The playhead sampler remains callable inside the AQ publication
/// closure without borrowing the actuator port through its gate.
pub trait AdaptiveRoundPort: TrialActuatorPort {
    fn position_sampler(&self) -> AddressedSampler<i64>;
    fn transition_permitted(&self, kind: AutomaticTransition) -> bool;
    fn rebuffer_state(&self) -> (bool, bool);
    fn buffer_snapshot(&self) -> Option<(Option<i64>, i64)>;
    fn pause_fact(&mut self) -> PauseFact;
}

/// Physical transfer and output geometry before a candidate policy verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Acquisition {
    pub bytes: u64,
    pub active_us: u64,
    pub total_us: u64,
    pub duration: Duration,
    pub width: i32,
    pub height: i32,
    pub declared_bps: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferFact {
    pub bytes: u64,
    pub active_us: u64,
    pub total_us: u64,
    pub audio_expected: bool,
}

/// A physical feed exit, preserving the current active-acquisition terminal
/// distinction without placing a transport or decoder type in policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicalFailure {
    Aborted,
    NotReady,
    PrimeExpired,
    StallAbort(TransferFact),
    Failed(&'static str),
}

/// Normalized content-time tails sampled by the physical feed. A missing audio
/// tail remains absent even when audio is expected; it is not an empty buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferFact {
    pub playback_ms: i64,
    pub video_tail_ms: i64,
    pub audio_tail_ms: Option<i64>,
    pub audio_expected: bool,
}

impl BufferFact {
    /// The playable tail is the shorter required lane. An expected but absent
    /// audio tail is unknown, never a fabricated zero-length reserve.
    pub fn buffered_ms(self) -> Option<i64> {
        let tail = match (self.audio_expected, self.audio_tail_ms) {
            (true, None) => return None,
            (_, Some(audio)) => audio.min(self.video_tail_ms),
            (false, None) => self.video_tail_ms,
        };
        Some(tail.saturating_sub(self.playback_ms).max(0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveQuantum {
    pub acquisition: Acquisition,
    pub buffer: BufferFact,
    pub abandoned: bool,
    pub delivered: bool,
}

/// Accepted user-pause evidence after a blocking probe or playlist read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PauseFact {
    pub paused: bool,
    pub completed_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedFailure {
    Aborted, Deadline, Ended, NotReady, Parse, SourceMoved, Protocol, TimelineOverflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedObservation {
    AbortStatus { trial: TrialId, aborted: bool },
    ExpiryStatus { trial: TrialId, expired: bool },
    BudgetSnapshot {
        trial: TrialId, buffered_ms: Option<i64>, started_ns: i64,
        remaining: Option<Duration>, rebuffering: bool,
    },
    PrimeDeadline { trial: TrialId, deadline: Option<std::time::Instant> },
    DeadlineFinal { trial: TrialId, final_expiry: bool },
    Opened { trial: TrialId, declared_bps: u64 },
    SegmentReady { trial: TrialId, duration: std::time::Duration },
    Acquired { trial: TrialId, acquisition: Acquisition },
    Staged { trial: TrialId, buffer: BufferFact, count: usize },
    FollowupReserve { trial: TrialId, funded: bool, remaining_ms: Option<u64> },
    Queued { trial: TrialId },
    Failed { trial: TrialId, failure: FeedFailure },
}

impl FeedObservation {
    pub const fn trial(self) -> TrialId {
        match self {
            Self::AbortStatus { trial, .. } | Self::ExpiryStatus { trial, .. }
            | Self::BudgetSnapshot { trial, .. }
            | Self::PrimeDeadline { trial, .. } | Self::DeadlineFinal { trial, .. }
            | Self::Opened { trial, .. }
            | Self::SegmentReady { trial, .. }
            | Self::Acquired { trial, .. }
            | Self::Staged { trial, .. }
            | Self::FollowupReserve { trial, .. }
            | Self::Queued { trial }
            | Self::Failed { trial, .. } => trial,
        }
    }
}

/// One worker-local IO port. Binding a locator is a runner operation; stepped
/// policy actions see only SourceKey. Physical cursor state stays in the port.
pub trait FeedPort {
    type Locator;
    fn bind_source(&mut self, source: SourceKey, locator: Self::Locator);
    fn step(&mut self, action: FeedAction) -> FeedObservation;
}

/// Physical candidate cursor and timeline remain in the feed port through the
/// guarded route publication. The runner may stage the completed timeline only
/// after all AUs are queued; promotion is available only after publication.
pub trait CandidateMediaPort: FeedPort<Locator = String> {
    fn candidate_declared_bps(&self) -> Option<u64>;
    fn stage_queued_timeline(&mut self) -> bool;
    /// Make the published candidate the active cursor. `committed` is the commit this promotes:
    /// the feed publishes its rung and declaration as the active HLS rung it is delivering.
    fn promote_candidate(&mut self, committed: &CommitReadout) -> bool;
    fn publication_gate(&mut self) -> &mut dyn PublicationGate;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VariantFact {
    pub width: i32,
    pub height: i32,
    pub declared_bps: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExplorationFact {
    pub target_kbps: u32,
    pub started_ns: i64,
    pub buffered_ms: i64,
    pub runway_ms: i64,
    pub budget_ms: i64,
}

/// One worker's physical HLS state. The common session issues every policy
/// action; the implementation retains cursor, network, AU and queue objects.
pub trait AdaptivePhysicalPort:
    ActiveFeedPort<Error = PhysicalFailure>
    + AdaptiveRoundPort<Lease = LeaseHandle, TrialReserve = LeaseHandle>
    + CandidateMediaPort
    + CandidateFencePort<Fence = FenceHandle>
{
    fn begin_round(&mut self, round: RoundId);
    fn finish_active(&mut self) -> Option<VariantFact>;
    fn begin_trial(&mut self, trial: TrialId, exploration: Option<ExplorationFact>);
    fn candidate_offset_micros(&self) -> Option<i64>;
    fn buffer_sampler(&self) -> AddressedSampler<Option<i64>>;
    fn remaining_playback_sampler(&self) -> AddressedSampler<i64>;
    fn user_sequence_sampler(&self) -> AddressedSampler<u64>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdaptiveProgress {
    Continue,
    Ended,
    OriginalPublished,
    Aborted,
    PhysicalFailure(PhysicalFailure),
    Failed(&'static str),
}

/// The route/controller owner moves into the existing feed worker. Every
/// round is driven synchronously on that worker through one addressed port.
pub trait AdaptiveSession: Send {
    fn initial_kbps(&self) -> u32;
    fn drive_round(
        &mut self, off: &plx_base::task::OffFrame,
        physical: &mut dyn AdaptivePhysicalPort,
    ) -> AdaptiveProgress;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OriginalQuantum {
    pub bytes: u64,
    pub active_us: u64,
    pub buffered_ms: Option<i64>,
    pub remaining_ms: Option<i64>,
    pub paused: bool,
    pub completed_pause_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginalProgress { Continue, Published, Retired }

/// Progressive Original's watchdog and route ticket. The physical feed only
/// reports completed byte and buffer facts, plus the late playhead sampler
/// used at the route publication boundary.
pub trait OriginalSession: Send {
    fn source_kbps(&self) -> u32;
    fn begin_on_worker(&mut self);
    fn seek(&mut self, bytes: u64, active_us: u64) -> bool;
    fn observe(&mut self, quantum: OriginalQuantum, position_ns: &dyn Fn() -> Option<i64>)
        -> OriginalProgress;
}

/// Refusal to arm a candidate before any of its media enters the active queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateFenceError {
    /// The private candidate lost its lease before any of its AUs were queued.
    Superseded,
    Aborted,
    Overlap,
    Exhausted,
}

/// The feed worker's physical clock fence. It is armed before candidate AUs
/// enter the queue and settled only after the queued route has been published
/// and its cursor promoted. Dropping an unsettled fence leaves teardown to
/// reset the generation.
pub trait CandidateFencePort {
    type Fence;
    fn arm(&mut self, automatic_token: u64) -> Result<Self::Fence, CandidateFenceError>;
    fn settle(&mut self, fence: Self::Fence);
}

/// Synchronous AQ abort fence. The closure is short and performs no IO. It is
/// invoked exactly once while the queue lock is held, or not invoked on abort.
pub trait PublicationGate {
    fn if_not_aborted(&mut self, transition: &mut dyn FnMut()) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialPhase {
    Granted,
    Prepared,
    FirstAcquired,
    FollowupRequested,
    FollowupAcquired,
    QueueAuthorized,
    QueueStarted,
    Queued,
    Published,
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialObservation {
    Prepared,
    FirstAcquired,
    FollowupRequested,
    FollowupAcquired,
    QueueAuthorized,
    QueueStarted,
    QueueCompleted,
    Published,
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialProtocolError { Stale, OutOfOrder, TerminalAfterQueue }

/// The synchronous owner of one trial's phase. IO outcomes are addressed to this ID;
/// a late result cannot settle the next trial, even if it names the same source.
#[derive(Debug, PartialEq, Eq)]
pub struct TrialProtocol { id: TrialId, phase: TrialPhase }

impl TrialProtocol {
    pub const fn new(id: TrialId) -> Self { Self { id, phase: TrialPhase::Granted } }
    pub const fn id(&self) -> TrialId { self.id }
    pub const fn phase(&self) -> TrialPhase { self.phase }

    pub fn observe(
        &mut self, id: TrialId, observation: TrialObservation,
    ) -> Result<TrialPhase, TrialProtocolError> {
        if id != self.id { return Err(TrialProtocolError::Stale); }
        use TrialObservation as O;
        use TrialPhase as P;
        self.phase = match (self.phase, observation) {
            (P::Granted, O::Prepared) => P::Prepared,
            (P::Prepared, O::FirstAcquired) => P::FirstAcquired,
            (P::FirstAcquired, O::FollowupRequested) => P::FollowupRequested,
            (P::FollowupRequested, O::FollowupAcquired) => P::FollowupAcquired,
            (P::FirstAcquired | P::FollowupAcquired, O::QueueAuthorized) => P::QueueAuthorized,
            (P::QueueAuthorized, O::QueueStarted) => P::QueueStarted,
            (P::QueueStarted, O::QueueCompleted) => P::Queued,
            (P::Queued, O::Published) => P::Published,
            (P::Granted | P::Prepared | P::FirstAcquired | P::FollowupRequested
                | P::FollowupAcquired | P::QueueAuthorized, O::Ended) => P::Ended,
            (P::QueueStarted | P::Queued, O::Ended) => {
                self.phase = P::Ended;
                return Err(TrialProtocolError::TerminalAfterQueue);
            }
            _ => return Err(TrialProtocolError::OutOfOrder),
        };
        Ok(self.phase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_bearing_trial_requires_both_staged_objects_before_queue_and_publication() {
        let id = TrialId::new(AttemptId::new(7, 9), 3);
        let mut trial = TrialProtocol::new(id);
        assert_eq!(trial.observe(id, TrialObservation::Published), Err(TrialProtocolError::OutOfOrder));
        for observation in [
            TrialObservation::Prepared, TrialObservation::FirstAcquired,
            TrialObservation::FollowupRequested, TrialObservation::FollowupAcquired,
            TrialObservation::QueueAuthorized, TrialObservation::QueueStarted,
            TrialObservation::QueueCompleted,
        ] {
            trial.observe(id, observation).unwrap();
        }
        assert_eq!(trial.observe(id, TrialObservation::FollowupRequested),
            Err(TrialProtocolError::OutOfOrder));
        assert_eq!(trial.observe(TrialId::new(id.attempt, 4), TrialObservation::Published),
            Err(TrialProtocolError::Stale));
        assert_eq!(trial.observe(id, TrialObservation::Published), Ok(TrialPhase::Published));

        let mut failed = TrialProtocol::new(id);
        for observation in [
            TrialObservation::Prepared, TrialObservation::FirstAcquired,
            TrialObservation::QueueAuthorized, TrialObservation::QueueStarted,
            TrialObservation::QueueCompleted,
        ] {
            failed.observe(id, observation).unwrap();
        }
        assert_eq!(failed.observe(id, TrialObservation::Ended),
            Err(TrialProtocolError::TerminalAfterQueue));
        assert_eq!(failed.phase(), TrialPhase::Ended);
        assert_eq!(failed.observe(id, TrialObservation::Published),
            Err(TrialProtocolError::OutOfOrder));
    }

    #[test]
    fn failed_prequeue_trial_ends_without_authorizing_publication() {
        let id = TrialId::new(AttemptId::new(1, 2), 1);
        let mut trial = TrialProtocol::new(id);
        trial.observe(id, TrialObservation::Prepared).unwrap();
        trial.observe(id, TrialObservation::FirstAcquired).unwrap();
        assert_eq!(trial.observe(id, TrialObservation::Ended), Ok(TrialPhase::Ended));
        assert_eq!(trial.observe(id, TrialObservation::Published), Err(TrialProtocolError::OutOfOrder));
    }

    /// The idle photograph is what the diagnostics mirror held before any publication and after a
    /// session reset: `-1` is "no measurement yet" and `0` "nothing decided", per member.
    #[test]
    fn the_idle_photograph_is_the_mirrors_reset_values() {
        let idle = AbrReadout::IDLE;
        assert_eq!(AbrReadout::default(), idle);
        assert_eq!((idle.media_kbps, idle.net_kbps, idle.buffer_ms, idle.ratio_pm), (-1, -1, -1, -1));
        assert_eq!((idle.action, idle.target_kbps, idle.unsafe_deficit_ms, idle.why), (0, 0, 0, 0));
        assert_eq!((idle.safe_kbps, idle.optimal_kbps, idle.unc_pm, idle.samples), (-1, -1, -1, -1));
        assert_eq!((idle.slope_ms_per_s, idle.starve_secs, idle.pred_pm, idle.risk), (0, -1, -1, -1));
    }

    /// The demux's start stored STEADY where the reset stores 0, and the panel words them
    /// differently ("hold current" and "starting"): a started controller that has decided nothing
    /// reads as holding its rung, one that has decided keeps its action, and no controller is 0.
    #[test]
    fn a_started_controller_that_has_decided_nothing_is_holding() {
        use crate::feed::DemuxAbr;
        use crate::readout::{ABR_ACTION_COMMIT_UP, ABR_ACTION_STEADY, ABR_MODE_HLS, ABR_MODE_ORIGINAL};
        let idle = AbrReadout::IDLE;
        assert_eq!(idle.action_beside(DemuxAbr::IDLE), 0, "no controller: nothing to hold");
        let hls = DemuxAbr { mode: ABR_MODE_HLS, kbps: 3_000, declared_kbps: -1 };
        let original = DemuxAbr { mode: ABR_MODE_ORIGINAL, kbps: 8_000, declared_kbps: -1 };
        assert_eq!(idle.action_beside(hls), ABR_ACTION_STEADY);
        assert_eq!(idle.action_beside(original), ABR_ACTION_STEADY);
        let committed = AbrReadout { action: ABR_ACTION_COMMIT_UP, ..idle };
        assert_eq!(committed.action_beside(hls), ABR_ACTION_COMMIT_UP);
        assert_eq!(committed.action_beside(DemuxAbr::IDLE), ABR_ACTION_COMMIT_UP);
    }
}
