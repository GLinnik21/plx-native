//! One worker's adaptive HLS owner. The feed supplies measured media and serialized
//! queue application; this module owns the controller and route transition.
#![deny(clippy::debug_assert_with_mut_call)]

use crate::{abr, route};
use plx_playback_contract::adaptive::PublicationGate;
use plx_playback_contract::adaptive::{
    AbrReadout, ActionReadout, AdaptiveDirection, AdaptiveEffects, AdaptivePhysicalPort, AdaptiveProgress, AdaptiveSession, Acquisition, ActiveAction, ActiveFeedPort, ActiveObservation, ActiveQuantum, AdaptiveRoundPort, AutomaticTransition, BufferFact, CandidateFenceError, CandidateFencePort, CandidateMediaPort, CommitReadout, FailureEdit, FeedAction, FeedFailure, FeedObservation, FeedPort, PauseFact, TrialClock,
    OriginalProgress, OriginalQuantum, OriginalSession, RoundId, SeedReadout, TrialObservation, TrialPhase, TrialProtocol, TrialProtocolError,
};
use plx_playback_contract::SourceKey;
use std::cell::{Cell, RefCell};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::time::Instant;

/// How many shipments one open's channel holds before the worker's sends are refused. A shipment
/// is the worker's whole accumulated state, so the depth only sets how long the player may go
/// without pumping before the worker starts keeping its newest state to itself (see
/// [`PlayerAdaptiveEffects`]); the worker produces a handful per segment round and the pump drains
/// every frame, so a healthy open never gets near it.
const READOUT_QUEUE_DEPTH: usize = 16;

/// Everything the adaptive worker has told its sink so far, as the player takes it in: the
/// accumulated [`AbrReadout`] and the carried link estimate with the instant the worker observed it.
/// A shipment is cumulative, so the newest one stands for all of them.
///
/// The Original failure edits are not state but a sequence, and a sequence survives that rule
/// because every edit replaces the whole pair: `failure` is the worker's newest edit with its
/// number (this open's edits count from 1), so "noted, then cleared" between two drains reaches the
/// player as the clear, and a coalesced run ends where applying each would have. The number is what
/// lets the player apply an edit once ([`AbrInbox::newest`]) although every later shipment carries
/// it again, and a new open's shipments start with none, so they never touch a reason the readout
/// kept from the open before. The numbering is one sink's: the HLS worker's Original probe is the
/// only writer of edits, and an open carries one worker (an HLS worker or the Original watchdog,
/// which edits nothing), so the inbox's single counter cannot mix two sequences.
#[derive(Clone, Copy)]
pub(crate) struct AbrShipment {
    pub(crate) readout: AbrReadout,
    pub(crate) seed: Option<(SeedReadout, Instant)>,
    pub(crate) failure: Option<(u64, FailureEdit)>,
}

/// The player's end of one open's channel from its adaptive worker. It lives with the claimed open
/// on the main thread, beside the attempt the open was claimed under; the pump drains it into the
/// session's readout. Dropping it is the normal stop: the worker's next send finds no receiver and
/// carries on.
pub(crate) struct AbrInbox {
    receiver: Receiver<AbrShipment>,
    /// The number of the last failure edit handed out: the next call hands out only a later one.
    taken_failure: Cell<u64>,
}

impl AbrInbox {
    /// The newest shipment sent since the last call, if any. Never blocks. Its `failure` is the
    /// worker's newest edit when that is one this inbox has not handed out yet, else `None`.
    pub(crate) fn newest(&self) -> Option<AbrShipment> {
        let mut shipment = self.receiver.try_iter().last()?;
        shipment.failure = shipment.failure.filter(|&(number, _)| number > self.taken_failure.get());
        if let Some((number, _)) = shipment.failure { self.taken_failure.set(number); }
        Some(shipment)
    }
}

/// One open's channel: the worker's sender (clone it into each adaptive session the open carries)
/// and the player's inbox.
pub(crate) fn readout_channel() -> (SyncSender<AbrShipment>, AbrInbox) {
    let (tx, rx) = sync_channel(READOUT_QUEUE_DEPTH);
    (tx, AbrInbox { receiver: rx, taken_failure: Cell::new(0) })
}

/// The player sink for common adaptive effects. Only the common worker invokes
/// these callbacks; the physical feed does not choose when a seed or committed
/// route becomes visible.
///
/// The seed, the readout and the Original failure edits cross to the player on the open's channel,
/// by `try_send`: the worker never blocks on a main thread that may be joining it (the teardown
/// joins the worker while the receiver is undrained). `latest` is the whole state the worker has handed the sink, and every send
/// carries all of it. A full channel refuses the NEW shipment (`std`'s sender cannot evict the queued
/// ones) and the sink keeps `latest`, so the next call, whichever member it changes, carries
/// everything the refused one did. The player therefore ends on the state as of the worker's next
/// call after the channel has room again, and a disconnected receiver (the stop) is not an error.
pub struct PlayerAdaptiveEffects {
    tx: SyncSender<AbrShipment>,
    latest: AbrShipment,
}

impl PlayerAdaptiveEffects {
    pub(crate) fn new(tx: SyncSender<AbrShipment>) -> Self {
        Self { tx, latest: AbrShipment { readout: AbrReadout::IDLE, seed: None, failure: None } }
    }

    fn ship(&self) {
        match self.tx.try_send(self.latest) {
            Ok(()) | Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {}
        }
    }
}

impl AdaptiveEffects for PlayerAdaptiveEffects {
    fn seed(&mut self, readout: SeedReadout) {
        // An estimate without a sample is no estimate: it clears the carried one.
        self.latest.seed = (readout.samples > 0).then(|| (readout, Instant::now()));
        self.ship();
    }

    fn readout(&mut self, readout: AbrReadout) {
        self.latest.readout = readout;
        self.ship();
    }

    fn failure(&mut self, edit: FailureEdit) {
        let number = self.latest.failure.map_or(0, |(number, _)| number).saturating_add(1);
        self.latest.failure = Some((number, edit));
        self.ship();
    }

    fn committed(&mut self, readout: CommitReadout) {
        let rung = abr::LADDER.into_iter()
            .find(|rung| rung.kbps() == readout.target_kbps)
            .expect("a committed adaptive target belongs to the captured ladder");
        crate::player::report::note_hls_committed_for(
            readout.trace_generation,
            match readout.direction {
                AdaptiveDirection::Up => abr::Direction::Up,
                AdaptiveDirection::Down => abr::Direction::Down,
            },
            rung,
            readout.refreshing,
        );
    }
}

/// The worker's side of the readout: it accumulates the members the worker writes, one change at
/// a time, and hands the sink the whole value after each. Shared by reference among the
/// callbacks one round passes down (`Cell`/`RefCell`: one thread, never contended).
pub(crate) struct AbrPublisher {
    readout: Cell<AbrReadout>,
    effects: RefCell<Box<dyn AdaptiveEffects>>,
}

impl AbrPublisher {
    pub(crate) fn new(effects: Box<dyn AdaptiveEffects>) -> Self {
        Self { readout: Cell::new(AbrReadout::IDLE), effects: RefCell::new(effects) }
    }

    fn update(&self, change: impl FnOnce(&mut AbrReadout)) {
        let mut readout = self.readout.get();
        change(&mut readout);
        self.readout.set(readout);
        self.effects.borrow_mut().readout(readout);
    }

    pub(crate) fn seed(&self, seed: SeedReadout) { self.effects.borrow_mut().seed(seed); }

    /// The Original probe's typed failure, for the player's readout to keep (see [`FailureEdit`]).
    fn note_failure(&self, kind: u8, status: i32) {
        self.effects.borrow_mut().failure(FailureEdit::Noted { kind, status });
    }

    /// A new probe supersedes the reason an earlier one failed.
    fn clear_failure(&self) { self.effects.borrow_mut().failure(FailureEdit::Cleared); }

    pub(crate) fn committed(&self, readout: CommitReadout) {
        self.effects.borrow_mut().committed(readout);
    }

    fn set_action(&self, code: u8, target_kbps: Option<i64>) {
        self.update(|r| {
            r.action = code;
            if let Some(target) = target_kbps { r.target_kbps = target; }
        });
    }

    fn action(&self, action: ActionReadout) {
        use AdaptiveDirection::{Down, Up};
        let ActionReadout { direction, target_kbps, refreshing, committed } = action;
        let code = if refreshing {
            match committed {
                None => plx_playback_contract::readout::ABR_ACTION_PRIME_REFRESH,
                Some(true) => plx_playback_contract::readout::ABR_ACTION_COMMIT_REFRESH,
                Some(false) => plx_playback_contract::readout::ABR_ACTION_REJECT_REFRESH,
            }
        } else {
            match (direction, committed) {
                (Down, None) => plx_playback_contract::readout::ABR_ACTION_PRIME_DOWN,
                (Up, None) => plx_playback_contract::readout::ABR_ACTION_PRIME_UP,
                (Down, Some(true)) => plx_playback_contract::readout::ABR_ACTION_COMMIT_DOWN,
                (Up, Some(true)) => plx_playback_contract::readout::ABR_ACTION_COMMIT_UP,
                (Down, Some(false)) => plx_playback_contract::readout::ABR_ACTION_REJECT_DOWN,
                (Up, Some(false)) => plx_playback_contract::readout::ABR_ACTION_REJECT_UP,
            }
        };
        self.set_action(code, Some(i64::from(target_kbps)));
    }

    fn recover_original(&self) {
        self.set_action(plx_playback_contract::readout::ABR_ACTION_RECOVER_ORIGINAL, None);
    }

    fn steady(&self) {
        self.set_action(plx_playback_contract::readout::ABR_ACTION_STEADY, Some(0));
    }

    fn sample(&self, sample: SampleReadout) {
        self.update(|r| {
            r.net_kbps = sample.network_kbps;
            r.media_kbps = sample.media_kbps;
            r.buffer_ms = sample.buffer_ms;
            r.ratio_pm = sample.ratio_pm;
        });
    }

    fn model(&self, model: &ModelReadout) {
        self.update(|r| {
            r.safe_kbps = model.safe_kbps;
            r.optimal_kbps = model.optimal_kbps;
            r.unc_pm = model.uncertainty_pm;
            r.samples = model.samples;
            r.slope_ms_per_s = model.slope_ms_per_s;
            r.starve_secs = model.starvation_secs;
            r.pred_pm = model.production_pm;
            r.risk = model.risk;
            r.why = why_code(model.reason);
        });
    }

    /// The Original watchdog's refreshed ticket after a seek: the measurements restart.
    fn original_refreshed(&self) {
        self.update(|r| {
            r.net_kbps = -1;
            r.buffer_ms = -1;
            r.unsafe_deficit_ms = 0;
        });
    }

    fn original_observed(&self, observation: &abr::OriginalObservation) {
        self.update(|r| {
            r.net_kbps = i64::from(observation.measured_kbps);
            r.buffer_ms = observation.buffered_ms;
            r.unsafe_deficit_ms = observation.unsafe_deficit_ms;
            r.slope_ms_per_s = observation.slope_ms_per_s;
            r.starve_secs = observation.horizon_secs.map(i64::from).unwrap_or(-1);
            r.safe_kbps = i64::from(observation.conservative_kbps);
        });
    }
}

fn why_code(reason: Option<WhyReadout>) -> u8 {
    use WhyReadout as R;
    match reason {
        None => crate::player::ABR_WHY_NONE,
        Some(R::SafeBudget) => crate::player::ABR_WHY_SAFE_BUDGET,
        Some(R::UnsafeState) => crate::player::ABR_WHY_UNSAFE_STATE,
        Some(R::Production) => crate::player::ABR_WHY_PRODUCTION,
        Some(R::Buffer) => crate::player::ABR_WHY_BUFFER,
        Some(R::LadderFloor) => crate::player::ABR_WHY_LADDER_FLOOR,
        Some(R::RejectBackoff) => crate::player::ABR_WHY_REJECT_BACKOFF,
        Some(R::Starvation) => crate::player::ABR_WHY_STARVATION,
        Some(R::Evidence) => crate::player::ABR_WHY_EVIDENCE,
        Some(R::AtBest) => crate::player::ABR_WHY_AT_BEST,
        Some(R::ReserveUnknown) => crate::player::ABR_WHY_RESERVE_UNKNOWN,
        Some(R::DeadlineRollback) => crate::player::ABR_WHY_DEADLINE_ROLLBACK,
        Some(R::ResponseLimited) => crate::player::ABR_WHY_RESPONSE_LIMITED,
        Some(R::SwitchCost) => crate::player::ABR_WHY_SWITCH_COST,
    }
}

struct PlayerActiveDiagnostics<'a> {
    cleanup_wait_logged: &'a mut bool,
    publisher: &'a AbrPublisher,
}

impl ActiveDiagnostics for PlayerActiveDiagnostics<'_> {
    fn cleanup(&mut self, ready: bool) {
        if !ready && !*self.cleanup_wait_logged {
            *self.cleanup_wait_logged = true;
            crate::player::log(
                "abr: waiting for PMS to release the stopped encoder before another experiment",
            );
        } else if ready && *self.cleanup_wait_logged {
            *self.cleanup_wait_logged = false;
            crate::player::log("abr: PMS encoder release observed; exploration may resume");
        }
    }

    fn accepted_pause(&mut self, after_lookup: bool, sequence: u64, elapsed_ms: u64) {
        if after_lookup {
            crate::player::log(&format!(
                "abr: consumed accepted pause sequence={} elapsed={}ms after rollback lookup; delivery estimate aged",
                sequence, elapsed_ms,
            ));
        } else {
            crate::player::log(&format!(
                "abr: consumed accepted pause sequence={} elapsed={}ms; delivery estimate aged",
                sequence, elapsed_ms,
            ));
        }
    }

    fn sample(&mut self, sample: SampleReadout) { self.publisher.sample(sample); }

    fn fresh_boundary(&mut self) {
        crate::player::log(
            "abr: fresh-session boundary buffered; waiting for the first repeatable HLS acquisition",
        );
    }

    fn observed(&mut self, readout: ActiveReadout) {
        self.publisher.model(&readout.model);
        crate::player::log(&readout.sample_line);
        crate::player::log(&readout.window_line);
    }

    fn seed(&mut self, seed: SeedReadout) { self.publisher.seed(seed); }
}

struct PlayerOriginalProbeDiagnostics<'a> { publisher: &'a AbrPublisher }

impl OriginalProbeDiagnostics for PlayerOriginalProbeDiagnostics<'_> {
    fn action(&mut self, action: ProbeTraceAction, target_kbps: u32) {
        use plx_playback_contract::readout as code;
        match action {
            ProbeTraceAction::Start => {
                self.publisher.set_action(code::ABR_ACTION_PROBE_ORIGINAL, Some(i64::from(target_kbps)));
                self.publisher.clear_failure();
            }
            ProbeTraceAction::Failed =>
                self.publisher.set_action(code::ABR_ACTION_ORIGINAL_PROBE_FAILED, Some(0)),
            ProbeTraceAction::Steady => self.publisher.steady(),
            ProbeTraceAction::RecoverOriginal => self.publisher.recover_original(),
        }
    }

    fn failure(&mut self, failure: ProbeFailure) {
        use ProbeFailure as F;
        let (kind, status) = match failure {
            F::HttpStatus(status) => (crate::player::ABR_FAILURE_ORIGINAL_HTTP, status),
            F::Deadline => (crate::player::ABR_FAILURE_ORIGINAL_DEADLINE, 0),
            F::Transport => (crate::player::ABR_FAILURE_ORIGINAL_TRANSPORT, 0),
            F::NoBody => (crate::player::ABR_FAILURE_ORIGINAL_NO_BODY, 0),
            F::Other => (crate::player::ABR_FAILURE_ORIGINAL_OPEN, 0),
        };
        self.publisher.note_failure(kind, status);
    }

    fn record(&mut self, line: &str) { crate::player::log(line); }
}

/// The route owner's captured inputs at a worker start. No second route identity is
/// minted here: the ticket remains the route's complete worker-generation fence.
pub struct Start {
    control: route::HlsAbrControl,
    ticket: route::WorkerTicket,
}

#[derive(Clone, Copy)]
pub(crate) struct StartInfo {
    pub(crate) initial: abr::Rung,
    pub(crate) prior: Option<abr::CapacityEstimate>,
    pub(crate) history: abr::TransitionHistory,
    pub(crate) original_candidate: bool,
    pub(crate) original_source_kbps: u32,
    pub(crate) original_refused: bool,
}

impl Start {
    pub(crate) fn with_plant_geometry(mut self, geometry: Option<abr::PlantGeometry>) -> Self {
        self.control.catalog = self.control.catalog.with_plant_geometry(geometry);
        self
    }

    pub(crate) fn info(&self) -> StartInfo {
        StartInfo {
            initial: self.control.initial_rung,
            prior: self.control.prior,
            history: self.control.history,
            original_candidate: self.control.has_original_candidate(),
            original_source_kbps: self.control.original_source_kbps(),
            original_refused: self.control.original_refused_on_this_route(),
        }
    }
}

impl From<(route::HlsAbrControl, route::WorkerTicket)> for Start {
    fn from((control, ticket): (route::HlsAbrControl, route::WorkerTicket)) -> Self {
        Self { control, ticket }
    }
}

/// Captured progressive Original policy and its exact worker ticket.
pub struct OriginalStart(route::AutoOriginalWatch);

impl From<route::AutoOriginalWatch> for OriginalStart {
    fn from(watch: route::AutoOriginalWatch) -> Self { Self(watch) }
}

impl OriginalStart {
    pub(crate) fn source_kbps(&self) -> u32 { self.0.source_kbps }

    pub(crate) fn with_plant_geometry(mut self, geometry: Option<abr::PlantGeometry>) -> Self {
        self.0.catalog = self.0.catalog.with_plant_geometry(geometry);
        self
    }

    pub(crate) fn start(self) -> OriginalCoordinator {
        let controller = abr::OriginalModeController::new(
            self.0.source_kbps, abr::AbrPolicy::measured(), self.0.catalog,
            self.0.history, self.0.features,
        );
        OriginalCoordinator { route: self.0, controller }
    }
}

/// Progressive Original policy. Seek changes the ticket and positional evidence
/// together; a superseded route can no longer publish a fallback.
pub(crate) struct OriginalCoordinator {
    route: route::AutoOriginalWatch,
    controller: Option<abr::OriginalModeController>,
}

pub(crate) enum OriginalFallbackResult { Published, Busy, RouteMoved }

pub(crate) enum ProbeResult {
    Measured(abr::CapacityObservation),
    Failed { outcome: &'static str, failure: ProbeFailure },
    Stale,
}

#[derive(Clone, Copy)]
pub(crate) enum ProbeFailure { HttpStatus(i32), Deadline, Transport, NoBody, Other }

impl From<route::OriginalProbeFailure> for ProbeFailure {
    fn from(failure: route::OriginalProbeFailure) -> Self {
        match failure {
            route::OriginalProbeFailure::HttpStatus(status) => Self::HttpStatus(status),
            route::OriginalProbeFailure::Deadline => Self::Deadline,
            route::OriginalProbeFailure::Transport => Self::Transport,
            route::OriginalProbeFailure::NoBody => Self::NoBody,
            route::OriginalProbeFailure::Other => Self::Other,
        }
    }
}

impl OriginalCoordinator {
    pub(crate) fn seek(&mut self, bytes: u64, active_us: u64) -> bool {
        if !self.route.refresh_ticket_after_seek() {
            self.controller = None;
            return false;
        }
        if let Some(controller) = self.controller.as_mut() {
            controller.on_seek(bytes, active_us);
        }
        true
    }

    pub(crate) fn on_resume(&mut self, elapsed_ms: u64) {
        if let Some(controller) = self.controller.as_mut() {
            controller.on_resume(elapsed_ms);
        }
    }

    pub(crate) fn observe(
        &mut self, bytes: u64, active_us: u64, buffered_ms: Option<i64>,
        remaining_ms: i64, now_ms: u64,
    ) -> Option<abr::OriginalObservation> {
        self.controller.as_mut()?.observe(
            bytes, active_us, buffered_ms, remaining_ms, now_ms,
        )
    }

    pub(crate) fn request_hls_fallback(
        &self, conservative_kbps: u32, position_ns: i64,
    ) -> OriginalFallbackResult {
        match self.route.request_hls_fallback(conservative_kbps, position_ns) {
            route::AutomaticIntentResult::Accepted => OriginalFallbackResult::Published,
            route::AutomaticIntentResult::Busy => OriginalFallbackResult::Busy,
            route::AutomaticIntentResult::Stale => OriginalFallbackResult::RouteMoved,
        }
    }
}

struct ProgressiveOriginalSession {
    start: Option<OriginalStart>,
    coordinator: Option<OriginalCoordinator>,
    source_kbps: u32,
    since: Option<std::time::Instant>,
    publisher: AbrPublisher,
}

/// Capture the progressive route owner without reading time or player state.
/// The existing feed worker starts its watchdog clock after the first source
/// declaration and before opening the progressive URL.
pub fn boxed_original_session(
    start: OriginalStart, effects: Box<dyn AdaptiveEffects>,
) -> Box<dyn OriginalSession> {
    let source_kbps = start.source_kbps();
    Box::new(ProgressiveOriginalSession {
        start: Some(start), coordinator: None, source_kbps, since: None,
        publisher: AbrPublisher::new(effects),
    })
}

impl OriginalSession for ProgressiveOriginalSession {
    fn source_kbps(&self) -> u32 { self.source_kbps }

    fn begin_on_worker(&mut self) {
        if self.coordinator.is_some() { return; }
        self.coordinator = Some(self.start.take()
            .expect("an uninitialized Original owner retains its route").start());
        self.since = Some(std::time::Instant::now());
    }

    fn seek(&mut self, bytes: u64, active_us: u64) -> bool {
        self.begin_on_worker();
        let refreshed = self.coordinator.as_mut()
            .expect("Original worker owns its controller").seek(bytes, active_us);
        if refreshed {
            self.publisher.original_refreshed();
            self.since = Some(std::time::Instant::now());
        }
        refreshed
    }

    fn observe(
        &mut self, quantum: OriginalQuantum, position_ns: &dyn Fn() -> Option<i64>,
    ) -> OriginalProgress {
        self.begin_on_worker();
        let watch = self.coordinator.as_mut()
            .expect("Original worker owns its controller");
        if let Some(elapsed_ms) = quantum.completed_pause_ms {
            watch.on_resume(elapsed_ms);
        }
        if quantum.paused { return OriginalProgress::Continue; }
        let Some(remaining_ms) = quantum.remaining_ms else {
            return OriginalProgress::Continue;
        };
        let now_ms = self.since.as_ref()
            .expect("Original worker owns its clock").elapsed().as_millis() as u64;
        let Some(observation) = watch.observe(
            quantum.bytes, quantum.active_us, quantum.buffered_ms,
            remaining_ms, now_ms,
        ) else { return OriginalProgress::Continue; };
        self.publisher.original_observed(&observation);
        if let Some(reason) = observation.fallback {
            crate::player::log(&format!(
                "auto: Original -> HLS {reason:?} measured={}kbps safe={}kbps need={}kbps buf={}ms slope={}ms/s starve={} held={}ms target={}kbps",
                observation.measured_kbps,
                observation.conservative_kbps,
                observation.requirement_kbps,
                observation.buffered_ms,
                observation.slope_ms_per_s,
                observation.horizon_secs.map(|s| s.to_string())
                    .unwrap_or_else(|| "none".to_string()),
                observation.unsafe_deficit_ms,
                observation.target.map(|r| r.kbps()).unwrap_or(0),
            ));
            let Some(position_ns) = position_ns() else {
                return OriginalProgress::Retired;
            };
            match watch.request_hls_fallback(
                observation.conservative_kbps.max(1), position_ns,
            ) {
                OriginalFallbackResult::Published => return OriginalProgress::Published,
                OriginalFallbackResult::Busy => crate::player::log(
                    "auto: Original fallback retained behind another route transition",
                ),
                OriginalFallbackResult::RouteMoved => {
                    crate::player::log(
                        "auto: discarded Original fallback from a superseded worker intent",
                    );
                    return OriginalProgress::Retired;
                }
            }
        }
        OriginalProgress::Continue
    }
}

/// A typed refusal at the prime boundary. A server rung refusal is the only one
/// that certifies anything about the requested rung.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrimeFailure { Rung, Deadline, Control, Session }

pub(crate) fn classify_prime_failure(
    failure: PrimeFailure, deadline_final: bool,
) -> (abr::RejectCause, &'static str) {
    match failure {
        PrimeFailure::Rung => (abr::RejectCause::Structural, "prime_refused"),
        PrimeFailure::Deadline if !deadline_final =>
            (abr::RejectCause::Circumstance, "control_clock_moved"),
        PrimeFailure::Deadline => (abr::RejectCause::Censored, "control_deadline"),
        PrimeFailure::Control => (abr::RejectCause::Circumstance, "control_unresolved"),
        PrimeFailure::Session => (abr::RejectCause::Circumstance, "prime_session_moved"),
    }
}

/// One worker-local trial runner. It owns phase and address validation while the
/// feed holds the opaque cursor, timeline, and staged access units in its port.
pub(crate) struct TrialRunner<'a, P: FeedPort + ?Sized> {
    port: &'a mut P,
    protocol: &'a mut TrialProtocol,
}

impl<'a, P: FeedPort + ?Sized> TrialRunner<'a, P> {
    pub(crate) fn new(port: &'a mut P, protocol: &'a mut TrialProtocol) -> Self {
        Self { port, protocol }
    }

    pub(crate) fn prime(
        &mut self, coordinator: &Coordinator, off: &plx_base::task::OffFrame,
        proposal: abr::Proposal, offset_micros: i64,
        deadline: Option<std::time::Instant>,
    ) -> Result<route::PrimedHls, PrimeFailure> {
        let primed = coordinator.prime(off, proposal, offset_micros, deadline)?;
        self.observe(TrialObservation::Prepared)
            .expect("a primed trial follows its grant");
        Ok(primed)
    }

    fn trace(
        &mut self, on_trace: &mut impl FnMut(TrialTrace, &mut P), event: TrialTrace,
    ) {
        on_trace(event, &mut *self.port);
    }

    fn reject_first(
        &mut self, coordinator: &mut Coordinator, proposal: abr::Proposal,
        cause: abr::RejectCause, outcome: &'static str, line: Option<String>,
        now_ms: u64, on_trace: &mut impl FnMut(TrialTrace, &mut P),
    ) -> FirstPhase {
        self.trace(on_trace, TrialTrace::Decided);
        let refreshing = coordinator.reject(proposal, cause, now_ms);
        self.trace(on_trace, TrialTrace::Rejected { refreshing });
        self.trace(on_trace, TrialTrace::Finish(outcome));
        if let Some(line) = line { self.trace(on_trace, TrialTrace::Log(line)); }
        FirstPhase::Stay
    }

    fn after_primed_leg(
        &mut self, coordinator: &mut Coordinator, primed: &route::PrimedHls,
        proposal: abr::Proposal, deadline_outcome: &'static str,
        on_trace: &mut impl FnMut(TrialTrace, &mut P), now_ms: &impl Fn() -> u64,
    ) -> Result<(), FirstPhase> {
        match self.aborted() {
            Ok(true) => {
                coordinator.abandon(&primed.encoder_session);
                self.trace(on_trace, TrialTrace::Finish("aborted"));
                return Err(FirstPhase::Aborted);
            }
            Err(_) => {
                coordinator.abandon(&primed.encoder_session);
                self.trace(on_trace, TrialTrace::Finish("feed_protocol"));
                return Err(FirstPhase::Failed("adaptive feed abort status protocol violation"));
            }
            Ok(false) => {}
        }
        match self.expired() {
            Ok(true) => {
                coordinator.abandon(&primed.encoder_session);
                let line = match deadline_outcome {
                    "control_deadline" =>
                        "abr: candidate spent its exploration reserve in the control plane",
                    _ => "abr: candidate spent its exploration reserve before media",
                };
                return Err(self.reject_first(
                    coordinator, proposal, abr::RejectCause::Censored,
                    deadline_outcome, Some(line.to_string()), now_ms(), on_trace,
                ));
            }
            Err(_) => {
                coordinator.abandon(&primed.encoder_session);
                self.trace(on_trace, TrialTrace::Finish("feed_protocol"));
                return Err(FirstPhase::Failed("adaptive feed expiry status protocol violation"));
            }
            Ok(false) => {}
        }
        Ok(())
    }

    fn settle_first_failure(
        &mut self, coordinator: &mut Coordinator, primed: &route::PrimedHls,
        proposal: abr::Proposal, step: TrialStep, failure: FeedFailure,
        now_ms: &impl Fn() -> u64,
        on_trace: &mut impl FnMut(TrialTrace, &mut P),
    ) -> FirstPhase {
        use TrialFailureAction as A;
        use TrialRejectReason as R;
        use TrialStep as S;
        coordinator.abandon(&primed.encoder_session);
        match trial_failure_action(step, failure) {
            A::Abort => {
                self.trace(on_trace, TrialTrace::Finish("aborted"));
                FirstPhase::Aborted
            }
            A::Protocol => {
                self.trace(on_trace, TrialTrace::Finish("feed_protocol"));
                FirstPhase::Failed(match step {
                    S::Open => "adaptive feed open protocol violation",
                    S::FirstRead => "adaptive feed read protocol violation",
                    S::FirstAcquire => "adaptive feed acquisition protocol violation",
                    S::FollowupRead => "adaptive feed follow-up protocol violation",
                    S::FollowupAcquire => "adaptive feed follow-up acquisition protocol violation",
                })
            }
            A::TimelineOverflow => {
                let outcome = if step == S::FollowupAcquire {
                    "repeatable_timeline_overflow"
                } else { "timeline_overflow" };
                let _ = self.reject_first(
                    coordinator, proposal, abr::RejectCause::Circumstance,
                    outcome, None, now_ms(), on_trace,
                );
                FirstPhase::Failed("HLS candidate timeline overflow")
            }
            A::Reject { cause, reason } => {
                let deadline = reason == R::Deadline;
                let outcome = match (step, reason) {
                    (S::Open, R::SourceMoved) => "origin_changed",
                    (S::Open, _) if deadline => "master_deadline",
                    (S::Open, _) => "no_master_playlist",
                    (S::FirstRead, R::Ended) => "candidate_ended",
                    (S::FirstRead, _) if deadline => "playlist_deadline",
                    (S::FirstRead, _) => "no_media_playlist",
                    (S::FirstAcquire, _) if deadline => "warmup_deadline",
                    (S::FirstAcquire, _) => "warmup_failed",
                    (S::FollowupRead, R::Ended) => "candidate_ended_after_setup",
                    (S::FollowupRead, _) if deadline => "repeatable_playlist_deadline",
                    (S::FollowupRead, _) => "repeatable_playlist_failed",
                    (S::FollowupAcquire, _) if deadline => "repeatable_deadline",
                    (S::FollowupAcquire, _) => "repeatable_failed",
                };
                let line = match (step, reason) {
                    (S::Open, R::SourceMoved) => Some(
                        "abr: candidate changed origin; rejected".to_string(),
                    ),
                    (S::Open, _) => Some(
                        "abr: candidate master failed; staying on current rung".to_string(),
                    ),
                    (S::FirstRead, R::Ended) => Some(
                        "abr: candidate stream ended before media; staying".to_string(),
                    ),
                    (S::FirstRead, _) => Some(
                        "abr: candidate produced no segment; staying on current rung".to_string(),
                    ),
                    (S::FirstAcquire, R::Deadline) => Some(format!(
                        "abr: {:?} candidate warm-up exceeded deadline; staying on current rung",
                        proposal.direction,
                    )),
                    (S::FirstAcquire, _) => Some(
                        "abr: candidate segment failed; staying on current rung".to_string(),
                    ),
                    (S::FollowupAcquire, _) => Some(
                        "abr: candidate produced no complete repeatable object; staying current".to_string(),
                    ),
                    (S::FollowupRead, _) => None,
                };
                self.reject_first(
                    coordinator, proposal, cause, outcome, line, now_ms(), on_trace,
                )
            }
        }
    }

    /// Prime and acquire the first candidate without transferring cursor or AU
    /// ownership to policy. Every blocking completion is addressed, checked for
    /// queue abort and measured against the same physical reserve clock.
    pub(crate) fn run_first(
        &mut self, coordinator: &mut Coordinator,
        off: &plx_base::task::OffFrame, proposal: abr::Proposal,
        reserve_policy: abr::ReservePolicy, offset_micros: i64,
        now_ms: impl Fn() -> u64,
        mut on_trace: impl FnMut(TrialTrace, &mut P),
    ) -> FirstPhase
    where
        P: FeedPort<Locator = String>,
    {
        let prime_deadline = match self.prime_deadline() {
            Ok(deadline) => deadline,
            Err(_) => return FirstPhase::Failed("adaptive feed prime deadline protocol violation"),
        };
        let primed = match self.prime(coordinator, off, proposal, offset_micros, prime_deadline) {
            Ok(primed) => primed,
            Err(refusal) => {
                match self.aborted() {
                    Ok(true) => {
                        self.trace(&mut on_trace, TrialTrace::Finish("aborted"));
                        return FirstPhase::Aborted;
                    }
                    Err(_) => return FirstPhase::Failed(
                        "adaptive feed abort status protocol violation",
                    ),
                    Ok(false) => {}
                }
                let deadline_final = if matches!(refusal, PrimeFailure::Deadline) {
                    match self.deadline_final(prime_deadline) {
                        Ok(final_expiry) => final_expiry,
                        Err(_) => return FirstPhase::Failed(
                            "adaptive feed prime expiry protocol violation",
                        ),
                    }
                } else { true };
                let (cause, why) = classify_prime_failure(refusal, deadline_final);
                return self.reject_first(
                    coordinator, proposal, cause, why,
                    Some("abr: candidate registration rejected; staying on current rung".to_string()),
                    now_ms(), &mut on_trace,
                );
            }
        };
        self.trace(&mut on_trace, TrialTrace::Prime);
        if let Err(outcome) = self.after_primed_leg(
            coordinator, &primed, proposal, "control_deadline", &mut on_trace, &now_ms,
        ) { return outcome; }
        let opened = self.open(primed.url.clone());
        let declared_bps = match opened {
            FeedObservation::Opened { declared_bps, .. } => declared_bps,
            other => return self.settle_first_failure(
                coordinator, &primed, proposal, TrialStep::Open,
                feed_failure(other), &now_ms, &mut on_trace,
            ),
        };
        self.trace(&mut on_trace, TrialTrace::Master(declared_bps));
        if let Err(outcome) = self.after_primed_leg(
            coordinator, &primed, proposal, "master_deadline", &mut on_trace, &now_ms,
        ) { return outcome; }
        let next = self.read_next();
        let duration = match next {
            FeedObservation::SegmentReady { duration, .. } => duration,
            other => return self.settle_first_failure(
                coordinator, &primed, proposal, TrialStep::FirstRead,
                feed_failure(other), &now_ms, &mut on_trace,
            ),
        };
        self.trace(&mut on_trace, TrialTrace::ControlPlane);
        if let Err(outcome) = self.after_primed_leg(
            coordinator, &primed, proposal, "playlist_deadline", &mut on_trace, &now_ms,
        ) { return outcome; }
        let (buffered_ms, started_ns, remaining, rebuffering) = match self.budget_snapshot() {
            Ok(snapshot) => snapshot,
            Err(_) => {
                coordinator.abandon(&primed.encoder_session);
                self.trace(&mut on_trace, TrialTrace::Finish("feed_protocol"));
                return FirstPhase::Failed("adaptive feed budget snapshot protocol violation");
            }
        };
        let Some(buffered_ms) = buffered_ms else {
            coordinator.abandon(&primed.encoder_session);
            return self.reject_first(
                coordinator, proposal, abr::RejectCause::Circumstance,
                "reserve_unreadable", Some(
                    "abr: candidate reserve unreadable; cannot bound the transfer, staying on current rung".to_string(),
                ), now_ms(), &mut on_trace,
            );
        };
        let budget = coordinator.candidate_budget_from_snapshot(
            proposal, reserve_policy, duration, buffered_ms, remaining, rebuffering,
        );
        if let Some(media_budget) = budget.media_deadline {
            self.trace(&mut on_trace, TrialTrace::WarmupDeadline(media_budget));
        } else {
            self.trace(&mut on_trace, TrialTrace::Log(
                "abr: terminal floor recovery keeps the only candidate response alive".to_string(),
            ));
        }
        let acquired = self.acquire_first(budget, started_ns);
        let acquisition = match acquired {
            FeedObservation::Acquired { acquisition, .. } => acquisition,
            other => return self.settle_first_failure(
                coordinator, &primed, proposal, TrialStep::FirstAcquire,
                feed_failure(other), &now_ms, &mut on_trace,
            ),
        };
        self.trace(&mut on_trace, TrialTrace::FirstMedia(acquisition));
        self.observe(TrialObservation::FirstAcquired)
            .expect("the first candidate acquisition follows prime");
        let buffer = match self.stage_acquired() {
            FeedObservation::Staged { buffer, count: 1, .. } => buffer,
            _ => {
                coordinator.abandon(&primed.encoder_session);
                self.trace(&mut on_trace, TrialTrace::Finish("feed_protocol"));
                return FirstPhase::Failed("adaptive feed stage protocol violation");
            }
        };
        FirstPhase::Ready { primed, declared_bps, acquisition, buffer }
    }

    pub(crate) fn observe(
        &mut self, observation: TrialObservation,
    ) -> Result<TrialPhase, TrialProtocolError> {
        let trial = self.protocol.id();
        self.protocol.observe(trial, observation)
    }

    fn step(&mut self, action: FeedAction) -> FeedObservation {
        let observed = self.port.step(action);
        if observed.trial() == self.protocol.id() {
            observed
        } else {
            FeedObservation::Failed {
                trial: self.protocol.id(), failure: FeedFailure::Protocol,
            }
        }
    }

    pub(crate) fn aborted(&mut self) -> Result<bool, FeedFailure> {
        let trial = self.protocol.id();
        match self.step(FeedAction::CheckAbort { trial }) {
            FeedObservation::AbortStatus { aborted, .. } => Ok(aborted),
            _ => Err(FeedFailure::Protocol),
        }
    }

    pub(crate) fn expired(&mut self) -> Result<bool, FeedFailure> {
        let trial = self.protocol.id();
        match self.step(FeedAction::CheckExpired { trial }) {
            FeedObservation::ExpiryStatus { expired, .. } => Ok(expired),
            _ => Err(FeedFailure::Protocol),
        }
    }

    pub(crate) fn budget_snapshot(&mut self) -> Result<(Option<i64>, i64, Option<std::time::Duration>, bool), FeedFailure> {
        let trial = self.protocol.id();
        match self.step(FeedAction::BudgetSnapshot { trial }) {
            FeedObservation::BudgetSnapshot {
                buffered_ms, started_ns, remaining, rebuffering, ..
            } => Ok((buffered_ms, started_ns, remaining, rebuffering)),
            _ => Err(FeedFailure::Protocol),
        }
    }

    pub(crate) fn prime_deadline(&mut self) -> Result<Option<std::time::Instant>, FeedFailure> {
        let trial = self.protocol.id();
        match self.step(FeedAction::PrimeDeadline { trial }) {
            FeedObservation::PrimeDeadline { deadline, .. } => Ok(deadline),
            _ => Err(FeedFailure::Protocol),
        }
    }

    pub(crate) fn deadline_final(
        &mut self, snapshot: Option<std::time::Instant>,
    ) -> Result<bool, FeedFailure> {
        let trial = self.protocol.id();
        match self.step(FeedAction::DeadlineFinal { trial, snapshot }) {
            FeedObservation::DeadlineFinal { final_expiry, .. } => Ok(final_expiry),
            _ => Err(FeedFailure::Protocol),
        }
    }

    /// Bind the primed locator only in the physical source slot. The action
    /// itself carries an opaque key, never a URL or authentication material.
    pub(crate) fn open(&mut self, locator: P::Locator) -> FeedObservation {
        let trial = self.protocol.id();
        let source = SourceKey::new(trial.serial);
        self.port.bind_source(source, locator);
        self.step(FeedAction::Open { trial, source })
    }

    pub(crate) fn read_next(&mut self) -> FeedObservation {
        let trial = self.protocol.id();
        self.step(FeedAction::ReadNext { trial })
    }

    pub(crate) fn acquire_first(
        &mut self, budget: CandidateBudget, started_ns: i64,
    ) -> FeedObservation {
        let trial = self.protocol.id();
        self.step(FeedAction::AcquireFirst {
            trial, media_budget: budget.media_deadline,
            yields_to_rebuffer: budget.yields_to_rebuffer,
            started_ns, clock: budget.clock,
        })
    }

    pub(crate) fn acquire_followup(&mut self) -> FeedObservation {
        let trial = self.protocol.id();
        self.step(FeedAction::AcquireFollowup { trial })
    }

    pub(crate) fn stage_acquired(&mut self) -> FeedObservation {
        let trial = self.protocol.id();
        self.step(FeedAction::StageAcquired { trial })
    }

    pub(crate) fn first_action(
        &mut self, coordinator: &Coordinator, proposal: abr::Proposal,
        current: VariantFacts, candidate: VariantFacts,
        sample: Option<abr::SegmentSample>,
    ) -> FirstCandidateAction {
        let grade = coordinator.first_candidate_grade(proposal, current, candidate, sample);
        self.after_first_grade(grade)
    }

    /// The setup-bearing object is already staged. A second read is taken only
    /// after the common budget decision, and both physical objects stay private
    /// until the final grade authorizes queueing them together.
    pub(crate) fn grade_staged(
        &mut self, coordinator: &Coordinator, proposal: abr::Proposal,
        reserve_policy: abr::ReservePolicy, current: VariantFacts,
        first: Acquisition, first_buffer: BufferFact, opened_declared_bps: u64,
        mut on_followup: impl FnMut(Option<u64>),
        mut on_trace: impl FnMut(TrialTraceMoment),
    ) -> StagedCandidate {
        let first_variant = VariantFacts {
            width: first.width, height: first.height, declared_bps: first.declared_bps,
        };
        let first_sample = candidate_sample(first, buffer_snapshot(first_buffer));
        let first_action = self.first_action(
            coordinator, proposal, current, first_variant, first_sample,
        );
        let mut verdict = match first_action {
            FirstCandidateAction::Finalize { verdict }
            | FirstCandidateAction::ReadFollowup { verdict, .. } => verdict,
            FirstCandidateAction::Unfunded => return StagedCandidate::Unfunded,
            FirstCandidateAction::Protocol => return StagedCandidate::Protocol(
                "adaptive feed follow-up reserve protocol violation",
            ),
        };
        let mut acquisitions = vec![first];
        let mut final_acquisition = first;
        let mut final_buffer = first_buffer;
        if let FirstCandidateAction::ReadFollowup { remaining_ms, .. } = first_action {
            let (acquisition, buffer) = match self.read_repeatable(
                remaining_ms, &mut on_followup, &mut on_trace,
            ) {
                Ok(evidence) => evidence,
                Err(outcome) => return outcome,
            };
            final_acquisition = acquisition;
            final_buffer = buffer;
            acquisitions.push(acquisition);
            verdict = self.followup_verdict(
                coordinator, proposal,
                candidate_sample(acquisition, buffer_snapshot(final_buffer)),
                opened_declared_bps,
            );
        }
        let candidate = VariantFacts {
            width: final_acquisition.width,
            height: final_acquisition.height,
            declared_bps: final_acquisition.declared_bps,
        };
        on_trace(TrialTraceMoment::FinalEvidence(final_acquisition));
        let grade = self.final_action(
            coordinator, proposal, reserve_policy, current, candidate, &acquisitions, verdict,
        );
        StagedCandidate::Ready {
            final_acquisition, final_buffer, verdict, grade,
            staged_count: acquisitions.len(),
        }
    }

    /// Settle a staged trial that cannot reach publication. The physical feed
    /// reports only the failed operation; the owner chooses the rejection and
    /// retires the candidate before the active cursor can resume.
    pub(crate) fn settle_staged(
        &mut self, coordinator: &mut Coordinator, primed: &route::PrimedHls,
        proposal: abr::Proposal, staged: StagedCandidate, current: VariantFacts,
        opened_declared_bps: u64, now_ms: impl Fn() -> u64,
        mut on_trace: impl FnMut(TrialTrace, &mut P),
    ) -> CandidateJudgment {
        match staged {
            StagedCandidate::Unfunded => {
                self.trace(&mut on_trace, TrialTrace::Decided);
                let refreshing = coordinator.fund_followup(proposal, false, now_ms())
                    .expect_err("an unfunded follow-up must reject the proposal");
                self.observe(TrialObservation::Ended)
                    .expect("unfunded follow-up has not entered the queue");
                coordinator.abandon(&primed.encoder_session);
                self.trace(&mut on_trace, TrialTrace::Rejected { refreshing });
                self.trace(&mut on_trace, TrialTrace::Finish("repeatable_unfunded"));
                self.trace(&mut on_trace, TrialTrace::Log(
                    "abr: setup-bearing candidate exhausted its original exploration budget".to_string(),
                ));
                CandidateJudgment::Stay
            }
            StagedCandidate::Failed { step, failure } => match self.settle_first_failure(
                    coordinator, primed, proposal, step, failure, &now_ms, &mut on_trace,
                ) {
                    FirstPhase::Stay => CandidateJudgment::Stay,
                    FirstPhase::Aborted => CandidateJudgment::Aborted,
                    FirstPhase::Failed(message) => CandidateJudgment::Failed(message),
                    FirstPhase::Ready { .. } => unreachable!("failed acquisition cannot be ready"),
                },
            StagedCandidate::Protocol(message) => {
                coordinator.abandon(&primed.encoder_session);
                self.trace(&mut on_trace, TrialTrace::Finish("feed_protocol"));
                CandidateJudgment::Failed(message)
            }
            StagedCandidate::Ready {
                final_acquisition, final_buffer, verdict, grade, staged_count,
            } => {
                let FinalCandidateGrade { raster_ready, quality_improves, disposition } = grade;
                if let HlsCandidateDisposition::Discard { cause, outcome } = disposition {
                    self.observe(TrialObservation::Ended)
                        .expect("a rejected candidate has not entered the queue");
                    coordinator.abandon(&primed.encoder_session);
                    self.trace(&mut on_trace, TrialTrace::Decided);
                    let refreshing = coordinator.reject(proposal, cause, now_ms());
                    self.trace(&mut on_trace, TrialTrace::Rejected { refreshing });
                    let line = if !raster_ready {
                        format!(
                            "abr: candidate raster {}x{} exceeds {}x{}; staying on current rung",
                            final_acquisition.width, final_acquisition.height,
                            proposal.rung.raster().0, proposal.rung.raster().1,
                        )
                    } else if !quality_improves {
                        format!(
                            "abr: candidate request {}kbps produced {}x{} @ {}kbps versus current {}x{} @ {}kbps; no measured quality gain, staying on current rung",
                            proposal.rung.kbps(), final_acquisition.width,
                            final_acquisition.height, opened_declared_bps / 1_000,
                            current.width, current.height, current.declared_bps / 1_000,
                        )
                    } else {
                        format!(
                            "abr: candidate verdict={verdict:?}; discarded {staged_count} staged segment(s) without changing the active media owner",
                        )
                    };
                    self.trace(&mut on_trace, TrialTrace::Log(line));
                    self.trace(&mut on_trace, TrialTrace::Finish(outcome));
                    return CandidateJudgment::Stay;
                }
                let sample = candidate_sample(
                    final_acquisition, buffer_snapshot(final_buffer),
                ).expect("a ready candidate has a completed segment sample");
                let Some(variant) = abr::ObservedHlsVariant::new(
                    final_acquisition.declared_bps,
                    final_acquisition.width,
                    final_acquisition.height,
                ) else {
                    coordinator.abandon(&primed.encoder_session);
                    return CandidateJudgment::Failed(
                        "ABR candidate committed invalid output geometry",
                    );
                };
                CandidateJudgment::Queue { final_acquisition, sample, variant }
            }
        }
    }

    fn read_repeatable(
        &mut self, remaining_ms: Option<u64>,
        on_followup: &mut impl FnMut(Option<u64>),
        on_trace: &mut impl FnMut(TrialTraceMoment),
    ) -> Result<(Acquisition, BufferFact), StagedCandidate> {
        on_followup(remaining_ms);
        self.observe(TrialObservation::FollowupRequested)
            .expect("a repeatable read follows its first staged object");
        let next = self.read_next();
        if !matches!(next, FeedObservation::SegmentReady { .. }) {
            return Err(StagedCandidate::Failed {
                step: TrialStep::FollowupRead,
                failure: match next {
                    FeedObservation::Failed { failure, .. } => failure,
                    _ => FeedFailure::Protocol,
                },
            });
        }
        let acquired = self.acquire_followup();
        let acquisition = match acquired {
            FeedObservation::Acquired { acquisition, .. } => acquisition,
            other => return Err(StagedCandidate::Failed {
                step: TrialStep::FollowupAcquire,
                failure: match other {
                    FeedObservation::Failed { failure, .. } => failure,
                    _ => FeedFailure::Protocol,
                },
            }),
        };
        on_trace(TrialTraceMoment::FollowupMedia(acquisition));
        self.observe(TrialObservation::FollowupAcquired)
            .expect("the repeatable acquisition follows its request");
        match self.stage_acquired() {
            FeedObservation::Staged { buffer, count: 2, .. } => Ok((acquisition, buffer)),
            _ => Err(StagedCandidate::Protocol(
                "adaptive feed follow-up stage protocol violation",
            )),
        }
    }

    fn after_first_grade(&mut self, grade: FirstCandidateGrade) -> FirstCandidateAction {
        if !grade.needs_followup {
            return FirstCandidateAction::Finalize { verdict: grade.verdict };
        }
        let trial = self.protocol.id();
        match self.step(FeedAction::CheckFollowupReserve { trial }) {
            FeedObservation::FollowupReserve { funded: true, remaining_ms, .. } =>
                FirstCandidateAction::ReadFollowup {
                    verdict: grade.verdict, remaining_ms,
                },
            FeedObservation::FollowupReserve { funded: false, .. } =>
                FirstCandidateAction::Unfunded,
            _ => FirstCandidateAction::Protocol,
        }
    }

    pub(crate) fn followup_verdict(
        &self, coordinator: &Coordinator, proposal: abr::Proposal,
        sample: Option<abr::SegmentSample>, declared_bps: u64,
    ) -> abr::CandidateVerdict {
        sample.map_or(abr::CandidateVerdict::Incomplete, |sample| {
            coordinator.candidate_verdict(proposal, sample, declared_bps, false)
        })
    }

    pub(crate) fn final_action(
        &self, coordinator: &Coordinator, proposal: abr::Proposal,
        reserve_policy: abr::ReservePolicy, current: VariantFacts,
        candidate: VariantFacts, acquisitions: &[Acquisition],
        verdict: abr::CandidateVerdict,
    ) -> FinalCandidateGrade {
        coordinator.final_candidate_grade(
            proposal, reserve_policy, current, candidate, acquisitions, verdict,
        )
    }

    pub(crate) fn authorize_queue(&mut self) {
        self.observe(TrialObservation::QueueAuthorized)
            .expect("only graded candidate media may enter the queue");
    }

    pub(crate) fn queue_staged(&mut self) -> FeedObservation {
        self.observe(TrialObservation::QueueStarted)
            .expect("queueing follows candidate authorization");
        let trial = self.protocol.id();
        let observed = self.step(FeedAction::QueueStaged { trial });
        if observed == (FeedObservation::Queued { trial }) {
            self.observe(TrialObservation::QueueCompleted)
                .expect("all staged candidate media was queued");
        } else {
            let terminal = self.observe(TrialObservation::Ended);
            debug_assert_eq!(terminal, Err(TrialProtocolError::TerminalAfterQueue));
        }
        observed
    }

    /// Queue every staged AU before entering the AQ publication gate. The
    /// physical cursor/timeline transfer is synchronous and local, between the
    /// queue ACK and the short guarded route/controller publication. A failed
    /// queue or publication is terminal and owns candidate cleanup here.
    pub(crate) fn queue_publish(
        &mut self, coordinator: &mut Coordinator,
        automatic_token: u64,
        primed: &route::PrimedHls, proposal: abr::Proposal,
        sample: abr::SegmentSample, variant: abr::ObservedHlsVariant,
        now_ms: impl FnOnce() -> u64,
        mut on_trace: impl FnMut(QueueTrace),
    ) -> QueueAttempt<P::Fence>
    where P: CandidateMediaPort + CandidateFencePort,
    {
        on_trace(QueueTrace::Decided);
        let fence = match self.port.arm(automatic_token) {
            Ok(fence) => fence,
            Err(error) => {
                coordinator.abandon(&primed.encoder_session);
                if error == CandidateFenceError::Superseded {
                    coordinator.reject(proposal, abr::RejectCause::Circumstance, now_ms());
                    self.observe(TrialObservation::Ended)
                        .expect("a cancelled private candidate has not entered the queue");
                }
                on_trace(QueueTrace::Finish(match error {
                    CandidateFenceError::Superseded => "candidate_superseded",
                    CandidateFenceError::Aborted => "aborted",
                    CandidateFenceError::Overlap => "publication_overlap",
                    CandidateFenceError::Exhausted => "publication_generation_exhausted",
                }));
                return QueueAttempt::FenceFailed(error);
            }
        };
        self.authorize_queue();
        let started = std::time::Instant::now();
        let trial = self.protocol.id();
        let queued = self.queue_staged();
        if queued != (FeedObservation::Queued { trial }) {
            coordinator.abandon(&primed.encoder_session);
            let aborted = queued == (FeedObservation::Failed {
                trial, failure: FeedFailure::Aborted,
            });
            on_trace(QueueTrace::Finish(if aborted { "aborted" } else { "commit_feed_failed" }));
            return if aborted { QueueAttempt::Aborted } else {
                QueueAttempt::Failed("adaptive feed queue protocol violation")
            };
        }
        on_trace(QueueTrace::FeedCompleted(started));
        if !self.port.stage_queued_timeline() {
            coordinator.abandon(&primed.encoder_session);
            let terminal = self.observe(TrialObservation::Ended);
            debug_assert_eq!(terminal, Err(TrialProtocolError::TerminalAfterQueue));
            on_trace(QueueTrace::Finish("commit_feed_failed"));
            return QueueAttempt::Failed("adaptive staged timeline protocol violation");
        }
        let publication = coordinator.publish_queued_candidate(
            self.protocol, self.port.publication_gate(), primed, proposal, sample, variant, now_ms(),
        );
        match publication {
            CandidatePublication::Active { previous } => QueueAttempt::Published { previous, fence },
            CandidatePublication::Aborted => {
                coordinator.abandon(&primed.encoder_session);
                on_trace(QueueTrace::Finish("aborted"));
                QueueAttempt::Aborted
            }
            CandidatePublication::SessionEnded => {
                coordinator.abandon(&primed.encoder_session);
                on_trace(QueueTrace::Finish("commit_session_ended"));
                QueueAttempt::Aborted
            }
            CandidatePublication::RouteMoved => {
                coordinator.abandon(&primed.encoder_session);
                on_trace(QueueTrace::Finish("commit_route_moved"));
                QueueAttempt::Aborted
            }
            CandidatePublication::ControllerRejected => {
                coordinator.abandon(&primed.encoder_session);
                on_trace(QueueTrace::Finish("commit_controller_rejected"));
                QueueAttempt::Failed("ABR candidate commit lost its validated evidence")
            }
        }
    }
}

/// Project physical acquisition onto the controller's whole-millisecond reserve lattice.
pub(crate) fn candidate_sample(
    acquisition: Acquisition, buffer: abr::BufferSnapshot,
) -> Option<abr::SegmentSample> {
    let duration_ms = u32::try_from(acquisition.duration.as_millis()).ok()?;
    let obligation_ms = u32::try_from(
        acquisition.duration.as_nanos().checked_add(999_999)? / 1_000_000,
    ).ok()?;
    abr::SegmentSample::new_with_obligation(
        acquisition.bytes, acquisition.active_us, acquisition.total_us,
        duration_ms, obligation_ms, buffer,
    )
}

pub(crate) fn buffer_snapshot(fact: BufferFact) -> abr::BufferSnapshot {
    abr::BufferSnapshot {
        playback: abr::MediaTimeMs(fact.playback_ms),
        video_tail: abr::MediaTimeMs(fact.video_tail_ms),
        audio_tail: fact.audio_tail_ms.map(abr::MediaTimeMs),
        audio_expected: fact.audio_expected,
    }
}

pub(crate) fn active_sample(
    acquisition: Acquisition, buffer: BufferFact, abandoned: bool,
) -> Option<abr::SegmentSample> {
    let duration_ms = u32::try_from(acquisition.duration.as_millis()).ok()?;
    let obligation_ms = u32::try_from(
        acquisition.duration.as_nanos().checked_add(999_999)? / 1_000_000,
    ).ok()?;
    let buffer = buffer_snapshot(buffer);
    if abandoned {
        abr::SegmentSample::abandoned_acquisition(
            acquisition.bytes, acquisition.active_us, acquisition.total_us,
            duration_ms, obligation_ms, buffer,
        )
    } else {
        abr::SegmentSample::new_with_obligation(
            acquisition.bytes, acquisition.active_us, acquisition.total_us,
            duration_ms, obligation_ms, buffer,
        )
    }
}

/// The feed's existing abort mutex is the first serialization boundary. Its
/// implementation must run the callback while holding that mutex and do no IO.
fn inside_publication_gate<T>(
    gate: &mut dyn PublicationGate, transition: impl FnOnce() -> T,
) -> Option<T> {
    let mut transition = Some(transition);
    let mut result = None;
    let ran = gate.if_not_aborted(&mut || {
        result = Some(transition.take().expect("AQ publication callback runs once")());
    });
    if ran { result } else { None }
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    struct Gate { aborted: bool, calls: usize }
    impl PublicationGate for Gate {
        fn if_not_aborted(&mut self, transition: &mut dyn FnMut()) -> bool {
            if self.aborted { return false; }
            self.calls += 1;
            transition();
            true
        }
    }

    #[test]
    fn abort_wins_before_any_route_or_controller_publication() {
        let mut gate = Gate { aborted: true, calls: 0 };
        let mut active = 7;
        assert_eq!(inside_publication_gate(&mut gate, || { active = 8; active }), None);
        assert_eq!((active, gate.calls), (7, 0));
        gate.aborted = false;
        assert_eq!(inside_publication_gate(&mut gate, || { active = 8; active }), Some(8));
        assert_eq!((active, gate.calls), (8, 1));
    }
}


#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CandidatePublication {
    Aborted,
    SessionEnded,
    RouteMoved,
    ControllerRejected,
    Active { previous: String },
}

/// One AQ -> ACTIVE -> controller publication. A grant or a primed encoder is
/// not active: only this successful callback changes route/controller ownership.
pub(crate) fn publish_candidate(
    port: &mut dyn PublicationGate,
    control: &route::HlsAbrControl,
    controller: &mut abr::Controller,
    ticket: &mut route::WorkerTicket,
    candidate: &route::PrimedHls,
    proposal: abr::Proposal,
    sample: abr::SegmentSample,
    variant: abr::ObservedHlsVariant,
    now_ms: u64,
) -> CandidatePublication {
    let expected = ticket.clone();
    inside_publication_gate(port, || {
        match control.commit_transition(
            &expected, candidate, proposal, (variant, sample.network_kbps()),
            || controller.commit_candidate(proposal, sample, variant, now_ms)
                .then(|| expected.encoder().to_owned()),
        ) {
            Ok((previous, next)) => {
                *ticket = next;
                CandidatePublication::Active { previous }
            }
            Err(route::HlsCommitRefusal::Session) => CandidatePublication::SessionEnded,
            Err(route::HlsCommitRefusal::RouteMoved) => CandidatePublication::RouteMoved,
            Err(route::HlsCommitRefusal::TransitionRejected) => {
                CandidatePublication::ControllerRejected
            }
        }
    }).unwrap_or(CandidatePublication::Aborted)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OriginalPublication { Published, Busy, Aborted, RouteMoved, ClockMoved }

/// Publish an Original request at the same AQ -> ACTIVE boundary. The caller's
/// clock lease must have authorized this transition before entering the AQ lock.
pub(crate) fn publish_original(
    port: &mut dyn PublicationGate,
    authorized: bool,
    control: &route::HlsAbrControl,
    ticket: &route::WorkerTicket,
    recover_kbps: u32,
    position_ns: impl FnOnce() -> Option<i64>,
    on_published: impl FnOnce(),
) -> OriginalPublication {
    if !authorized { return OriginalPublication::ClockMoved; }
    inside_publication_gate(port, || {
        let Some(position_ns) = position_ns() else {
            return OriginalPublication::RouteMoved;
        };
        match control.request_original_recovery(ticket, recover_kbps, position_ns) {
            route::AutomaticIntentResult::Accepted => {
                on_published();
                OriginalPublication::Published
            }
            route::AutomaticIntentResult::Busy => OriginalPublication::Busy,
            route::AutomaticIntentResult::Stale => OriginalPublication::RouteMoved,
        }
    }).unwrap_or(OriginalPublication::Aborted)
}

pub(crate) fn prime(
    control: &route::HlsAbrControl,
    ticket: &route::WorkerTicket,
    off: &plx_base::task::OffFrame,
    proposal: abr::Proposal,
    offset_micros: i64,
    deadline: Option<std::time::Instant>,
) -> Result<route::PrimedHls, PrimeFailure> {
    control.prime(off, ticket, proposal, offset_micros, deadline)
        .map_err(|failure| match failure {
            route::PrimeRefusal::Rung => PrimeFailure::Rung,
            route::PrimeRefusal::Deadline => PrimeFailure::Deadline,
            route::PrimeRefusal::Control => PrimeFailure::Control,
            route::PrimeRefusal::Session => PrimeFailure::Session,
        })
}

/// One controller and its physical route owner. Rebuilt on seek/reload from the
/// route's captured prior and history; pending reserve never crosses that boundary.
pub(crate) struct Coordinator {
    control: route::HlsAbrControl,
    ticket: route::WorkerTicket,
    controller: abr::Controller,
    recovery: Option<abr::OriginalRecovery>,
    recover_kbps: u32,
    last_probe_block: Option<abr::ProbeBlock>,
    next_trial_serial: u64,
    next_round_serial: u64,
}

pub(crate) struct RoundDecision {
    pub(crate) decision: abr::Decision,
    pub(crate) telemetry: abr::ControllerTelemetry,
    pub(crate) prime_runway_ms: Option<i64>,
}

/// **One record per candidate transaction, emitted on EVERY exit path.** Increment I2's
/// instrumentation; it changes no decision.
///
/// A `Drop` guard rather than a log call at each `continue`, because the prime arm has twelve
/// distinct reject paths (`control.prime` refusing, an origin change, the master playlist, the
/// media playlist, two demux legs, the timeline, acceptance, the raster check) and a thirteenth
/// added later would silently stop being measured. Drop cannot be forgotten.
///
/// **What it was introduced for.** The claim that a transaction's budget can be derived from the
/// reserve (`T = B - A_i`) rests on the transaction's real cost. Before this trace and the current
/// end-to-end reserve clock, the only figure was a *derived* 4600 ms in the host plant: a sum of two
/// upshift deadlines describing a two-segment shape a downshift did not have. Those old deadlines
/// excluded `control.prime`, the master playlist and both `hls_cursor_next` calls. The live upshift
/// now carries one playhead-funded reserve through those control legs and the first candidate-media
/// phase, while this trace records their separate wall costs on every exit. A setup-bearing object
/// and its optional ordinary continuation remain staged under that same grant until one verdict.
///
/// **Why the drawdown is otherwise invisible.** The prime arm runs INLINE inside the loop that
/// emits one `abr: sample` per iteration, so no sample is emitted between proposal and commit. A
/// `min_buf_ms` computed from `abr: sample` cannot see the transaction's cost at all; that is a
/// property of where the samples are taken, not evidence that the cost is small.
pub(crate) struct TxTrace {
    buffered_ms: plx_playback_contract::demux::AddressedSampler<Option<i64>>,
    record: fn(&str),
    started: std::time::Instant,
    /// The control plane is THREE requests, not one near-zero-byte transfer: PMS session
    /// creation, the master playlist and the media playlist. Logged separately because they
    /// scale with different things — `prime` is server work and is anti-correlated with link
    /// speed, while the two playlist fetches are round trips. A single `control=` total cannot
    /// tell a slow encoder registration from a distant server, and the transaction's fixed
    /// overhead is estimated from exactly this number.
    prime_done_ms: Option<i64>,
    master_done_ms: Option<i64>,
    control_plane_ms: Option<i64>,
    /// Acquisition of the structurally unique first object in the candidate session.
    warmup_acq_ms: Option<i64>,
    /// Acquisition of the first ordinary object from that same already-running encoder. Present
    /// only when the boundary object was funded but did not itself satisfy the steady law.
    graded_acq_ms: Option<i64>,
    buf_start_ms: Option<i64>,
    /// **The deadline the warm-up leg was actually granted, ms.** Without it a captured log cannot
    /// say whether a transaction was bounded — only how long it took — so "the deadline held" is
    /// not a statement the corpus can make about itself. `None` on every exit path that never
    /// reached the media fetch.
    warmup_dl_ms: Option<i64>,
    /// Acquisition of the current stream's segment immediately before the transaction. Retained
    /// as transaction context; the live exploration budget itself comes from the full bag runway.
    cur_acq_before_ms: i64,
    net_kbps: u32,
    fast_kbps: u32,
    slow_kbps: u32,
    unc_pm: u32,
    direction: abr::Direction,
    from_kbps: u32,
    to_kbps: u32,
    outcome: &'static str,
    /// Elapsed at the moment the commit/reject was DECIDED. `total` below runs to scope end and so
    /// can also contain the single post-decision feed of every staged candidate object.
    decided_ms: Option<i64>,
    /// The reserve at the decision. This precedes every candidate feed.
    buf_decided_ms: Option<i64>,
    /// Sum of every candidate feed phase. Feeding can block on `aq_push` against a full queue;
    /// this is BACKPRESSURE, not acquisition cost: the media is already in hand and replenishes
    /// the reserve as it lands. It used to be folded into `decided_ms` — which is why the first
    /// published figure for an upshift was 9563 ms against a true 3065 ms.
    feed_ms: Option<i64>,
    /// The reserve after the feed, so the pre/post pair brackets what the staged segments added.
    buf_fed_ms: Option<i64>,
    /// The optional ordinary candidate acquisition after a setup-bearing boundary. It remains
    /// absent when the boundary object itself satisfies the steady conservation law.
    graded_bytes: Option<u64>,
    /// The completed acquisition on which the final candidate verdict rests: the boundary object
    /// when it is directly admissible, otherwise the staged ordinary object. The offline
    /// conservation grader needs all three facts because a reset otherwise hides that `(A_i,D_i)`
    /// observation.
    candidate_acq_ms: Option<i64>,
    candidate_bytes: Option<u64>,
    candidate_dur_ms: Option<i64>,
    /// The candidate rendition's DECLARED rate, kbit/s, from its own master playlist. `None`
    /// (logged `-1`) on every exit path that never got that far -- which is not zero and must not
    /// read as a rendition that declares nothing.
    declared_kbps: Option<u32>,
}

impl TxTrace {
    pub(crate) fn open(
        proposal: abr::Proposal,
        from: abr::Rung,
        sample: abr::SegmentSample,
        delivery: &abr::CapacityEstimate,
        buffered_ms: plx_playback_contract::demux::AddressedSampler<Option<i64>>, record: fn(&str),
    ) -> Self {
        Self {
            started: std::time::Instant::now(),
            buffered_ms: buffered_ms.clone(), record,
            prime_done_ms: None,
            master_done_ms: None,
            control_plane_ms: None,
            warmup_acq_ms: None,
            graded_acq_ms: None,
            warmup_dl_ms: None,
            // The injected physical sampler reads only what has actually been FED;
            // staged candidate tails cannot credit the reserve before publication.
            buf_start_ms: buffered_ms().flatten(),
            cur_acq_before_ms: i64::from(sample.production_ratio_pm())
                * i64::from(sample.media_duration_ms())
                / 1_000,
            net_kbps: sample.network_kbps(),
            fast_kbps: delivery.fast_kbps,
            slow_kbps: delivery.slow_kbps,
            unc_pm: delivery.uncertainty_pm,
            direction: proposal.direction,
            from_kbps: from.kbps(),
            to_kbps: proposal.rung.kbps(),
            // If this survives to the log, an exit path forgot `finish` — the record
            // says so rather than looking like a labelled outcome.
            outcome: "UNLABELLED_PATH",
            decided_ms: None,
            buf_decided_ms: None,
            feed_ms: None,
            buf_fed_ms: None,
            graded_bytes: None,
            candidate_acq_ms: None,
            candidate_bytes: None,
            candidate_dur_ms: None,
            declared_kbps: None,
        }
    }

    pub(crate) fn mark_prime(&mut self) {
        self.prime_done_ms = Some(self.elapsed_ms());
    }

    /// The candidate's master playlist is in, so its DECLARED rate is known -- for the only
    /// moment in a playback at which any per-rung rate is knowable at all. Recorded here so the
    /// `abr: tx` line carries it beside the catalog's guess and the two can be differenced on a
    /// captured trace with no extra instrumentation.
    pub(crate) fn mark_master(&mut self, declared_bps: u64) {
        self.master_done_ms = Some(self.elapsed_ms());
        self.declared_kbps = Some(u32::try_from(declared_bps / 1_000).unwrap_or(u32::MAX));
    }

    pub(crate) fn mark_control_plane(&mut self) {
        self.control_plane_ms = Some(self.elapsed_ms());
    }

    pub(crate) fn mark_warmup_deadline(&mut self, budget: std::time::Duration) {
        self.warmup_dl_ms = Some(i64::try_from(budget.as_millis()).unwrap_or(i64::MAX));
    }

    /// Add the post-decision feed phase, measured from its own start. All staged candidate objects
    /// cross into playback together under the committed media owner.
    pub(crate) fn mark_feed(&mut self, started: std::time::Instant) {
        let elapsed = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        self.feed_ms = Some(self.feed_ms.unwrap_or(0).saturating_add(elapsed));
        self.buf_fed_ms = (self.buffered_ms)().flatten();
    }

    /// Acquisition of one candidate media segment, measured the same way production measures the
    /// current stream: request to demux complete, backpressure excluded.
    pub(crate) fn mark_media(&mut self, bytes: u64, total_us: u64, graded: bool) {
        let acq = i64::try_from(total_us / 1_000).unwrap_or(i64::MAX);
        if graded {
            self.graded_acq_ms = Some(acq);
            self.graded_bytes = Some(bytes);
        } else {
            self.warmup_acq_ms = Some(acq);
        }
    }

    pub(crate) fn mark_candidate_evidence(
        &mut self,
        bytes: u64, total_us: u64, duration: std::time::Duration,
    ) {
        self.candidate_acq_ms =
            Some(i64::try_from(total_us / 1_000).unwrap_or(i64::MAX));
        self.candidate_bytes = Some(bytes);
        self.candidate_dur_ms = Some(i64::try_from(duration.as_millis()).unwrap_or(i64::MAX));
    }

    /// Freeze the user-visible decision cost before any post-decision queue backpressure.  This
    /// deliberately does NOT label the transaction committed: feeding the staged media and the
    /// controller's atomic reset+seed are still fallible after this point.
    pub(crate) fn mark_decided(&mut self) {
        if self.decided_ms.is_none() {
            self.decided_ms = Some(self.elapsed_ms());
            self.buf_decided_ms = (self.buffered_ms)().flatten();
        }
    }

    /// Label the path only when the outcome named here has really happened.  Rejection paths call
    /// this at their decision point; a commit first calls `mark_decided`, performs the fallible
    /// feed and atomic controller transition, and reaches this method only after the new finite
    /// bag has actually been seeded.
    pub(crate) fn finish(&mut self, outcome: &'static str) {
        self.mark_decided();
        self.outcome = outcome;
    }

    fn elapsed_ms(&self) -> i64 {
        i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX)
    }
}

impl Drop for TxTrace {
    fn drop(&mut self) {
        let opt = |v: Option<i64>| {
            v.map(|n| n.to_string())
                .unwrap_or_else(|| "none".to_string())
        };
        // The three control-plane legs as DURATIONS, each measured from the end of the one before
        // it, so they sum to `control=` and none of them silently contains another.
        let leg = |to: Option<i64>, from: Option<i64>| match (to, from) {
            (Some(to), Some(from)) => Some(to.saturating_sub(from)),
            _ => None,
        };
        (self.record)(&format!(
            "abr: tx {:?} {}->{}kbps outcome={} decided={}ms total={}ms control={}ms \
             prime={}ms master={}ms media={}ms warmup={}ms \
             graded={}ms warmup_dl={}ms buf_start={}ms buf_decided={}ms feed={}ms buf_fed={}ms \
             buf_end={}ms \
             cur_acq_before={}ms net={}kbps fast={}kbps slow={}kbps unc={}pm declared={}kbps \
             graded_bytes={} candidate_acq={}ms candidate_bytes={} candidate_dur={}ms",
            self.direction,
            self.from_kbps,
            self.to_kbps,
            self.outcome,
            opt(self.decided_ms),
            self.elapsed_ms(),
            opt(self.control_plane_ms),
            opt(self.prime_done_ms),
            opt(leg(self.master_done_ms, self.prime_done_ms)),
            opt(leg(self.control_plane_ms, self.master_done_ms)),
            opt(self.warmup_acq_ms),
            opt(self.graded_acq_ms),
            opt(self.warmup_dl_ms),
            opt(self.buf_start_ms),
            opt(self.buf_decided_ms),
            opt(self.feed_ms),
            opt(self.buf_fed_ms),
            opt((self.buffered_ms)().flatten()),
            self.cur_acq_before_ms,
            self.net_kbps,
            self.fast_kbps,
            self.slow_kbps,
            self.unc_pm,
            self.declared_kbps.map(i64::from).unwrap_or(-1),
            self.graded_bytes.map(|b| b as i64).unwrap_or(-1),
            opt(self.candidate_acq_ms),
            self.candidate_bytes.map(|b| b as i64).unwrap_or(-1),
            self.candidate_dur_ms.unwrap_or(-1),
        ));
    }
}


/// The exact per-round diagnostic lines are assembled alongside the policy
/// snapshot that decided them; the physical feed only emits the resulting text.
pub(crate) fn steady_line(t: &abr::ControllerTelemetry, remaining_ms: i64) -> String {
    format!(
        "abr: steady current={}kbps safe={}kbps pending={}kbps fast={}kbps slow={}kbps unc={}pm n={} \
         buf={}ms slope={}ms/s prod={}pm/{}pm risk={} starve={} edge={} left={}s \
         dwell={}ms block={}kbps onrung={} draining={} reason={:?}",
        t.current.kbps(),
        t.safe_budget_kbps,
        t.pending.map(|p| p.rung.kbps()).unwrap_or(0),
        t.delivery.fast_kbps,
        t.delivery.slow_kbps,
        t.delivery.uncertainty_pm,
        t.delivery.samples,
        t.buffer.buffered_ms,
        t.buffer.slope_ms_per_s,
        t.production.ratio_pm,
        t.risk.production_ratio_pm.unwrap_or(0),
        t.risk.score,
        t.risk
            .starvation_seconds
            .map(|s| s.to_string())
            .unwrap_or_else(|| "none".to_string()),
        // `edge=` beside `starve=`: the same formula on the MEASURED rate rather than the
        // conservative one, and the one the emergency downshift actually reads. They differ by up
        // to 2x on the first sample of every rung, where `uncertainty_pm` is at its 500 cap, so a
        // reader grading a downshift against `starve=` alone is grading a number that decided
        // nothing.
        t.emergency_horizon_secs
            .map(|s| s.to_string())
            .unwrap_or_else(|| "none".to_string()),
        remaining_ms / 1_000,
        // `dwell=` is retained in the diagnostics wire shape and is always zero: exploration has
        // no elapsed-time release. `block=` is the highest rung the per-rung failure frontier is
        // currently refusing at this exact measured surplus.
        t.gates.dwell_ms,
        t.gates.blocked_kbps,
        t.gates.on_rung,
        t.gates.draining,
        t.reason,
    )
}


fn sample_line(
    t: &abr::ControllerTelemetry,
    sample: abr::SegmentSample,
    decision: abr::Decision,
) -> String {
    use abr::{Decision::Prime, Direction};
    let (action, target) = match decision {
        Prime(p) if p.direction == Direction::Up && p.rung == t.current => {
            ("refresh", p.rung.kbps())
        }
        Prime(p) if p.direction == Direction::Up => ("prime_up", p.rung.kbps()),
        Prime(p) => ("prime_down", p.rung.kbps()),
        _ => ("stay", 0),
    };
    format!(
        "abr: sample current={}kbps media={}kbps net={}kbps buf={} vbuf={}ms abuf={} \
         dur={}ms prod={}pm n={} decision={} target={}kbps complete={} reason={:?}",
        t.current.kbps(),
        sample.media_kbps(),
        sample.network_kbps(),
        sample
            .buffer
            .buffered_ms()
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "none".to_string()),
        sample.buffer.video_buffered_ms(),
        sample
            .buffer
            .audio_buffered_ms()
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "none".to_string()),
        sample.media_duration_ms(),
        sample.production_ratio_pm(),
        t.delivery.samples,
        action,
        target,
        u8::from(sample.completed()),
        t.reason,
    )
}


fn window_line(t: &abr::ControllerTelemetry, sample: abr::SegmentSample) -> String {
    t.window.log_line(
        t.current.kbps(),
        sample.bytes(),
        sample.media_duration_ms(),
    )
}


/// The panel's completed-rate projection at the same observation boundary as
/// the controller sample. Censored prefixes are unknown in this compact wire.
pub(crate) struct SampleReadout {
    pub(crate) network_kbps: i64,
    pub(crate) media_kbps: i64,
    pub(crate) ratio_pm: i64,
    pub(crate) buffer_ms: i64,
}

#[derive(Clone, Copy)]
pub(crate) enum WhyReadout {
    SafeBudget, UnsafeState, Production, Buffer, LadderFloor, RejectBackoff,
    Starvation, Evidence, AtBest, ReserveUnknown, DeadlineRollback,
    ResponseLimited, SwitchCost,
}

pub(crate) fn action_readout(
    proposal: abr::Proposal, refreshing: bool, committed: Option<bool>,
) -> ActionReadout {
    ActionReadout {
        direction: match proposal.direction {
            abr::Direction::Up => AdaptiveDirection::Up,
            abr::Direction::Down => AdaptiveDirection::Down,
        },
        target_kbps: proposal.rung.kbps(), refreshing, committed,
    }
}

fn why_readout(reason: Option<abr::DecisionReason>) -> Option<WhyReadout> {
    use abr::{DecisionReason::Hls, HlsReason as R};
    Some(match reason? {
        Hls(R::SafeBudgetIncrease) => WhyReadout::SafeBudget,
        Hls(R::UnsafeCurrentState) => WhyReadout::UnsafeState,
        Hls(R::ProductionConstraint) => WhyReadout::Production,
        Hls(R::BufferConstraint) => WhyReadout::Buffer,
        Hls(R::LadderFloor) => WhyReadout::LadderFloor,
        Hls(R::RejectBackoff) => WhyReadout::RejectBackoff,
        Hls(R::StarvationHorizon) => WhyReadout::Starvation,
        Hls(R::EvidenceWindow) => WhyReadout::Evidence,
        Hls(R::AtBestRung) => WhyReadout::AtBest,
        Hls(R::ReserveUnknown) => WhyReadout::ReserveUnknown,
        Hls(R::DeadlineRollback) => WhyReadout::DeadlineRollback,
        Hls(R::ResponseLimited) => WhyReadout::ResponseLimited,
        Hls(R::SwitchCost) => WhyReadout::SwitchCost,
    })
}

pub(crate) struct ModelReadout {
    pub(crate) safe_kbps: i64,
    pub(crate) optimal_kbps: i64,
    pub(crate) uncertainty_pm: i64,
    pub(crate) samples: i64,
    pub(crate) slope_ms_per_s: i64,
    pub(crate) starvation_secs: i64,
    pub(crate) production_pm: i64,
    pub(crate) risk: i64,
    pub(crate) reason: Option<WhyReadout>,
    pub(crate) seed: SeedReadout,
}

pub(crate) struct ActiveReadout {
    pub(crate) prime_runway_ms: Option<i64>,
    pub(crate) model: ModelReadout,
    pub(crate) sample_line: String,
    pub(crate) window_line: String,
}

impl ActiveReadout {
    fn new(round: &RoundDecision, sample: abr::SegmentSample) -> Self {
        let t = &round.telemetry;
        Self {
            prime_runway_ms: round.prime_runway_ms,
            model: ModelReadout {
                safe_kbps: i64::from(t.safe_budget_kbps),
                optimal_kbps: t.optimal.map(|c| i64::from(c.rung.kbps())).unwrap_or(-1),
                uncertainty_pm: i64::from(t.delivery.uncertainty_pm),
                samples: i64::from(t.delivery.samples),
                slope_ms_per_s: t.buffer.slope_ms_per_s,
                starvation_secs: t.risk.starvation_seconds.map(i64::from).unwrap_or(-1),
                production_pm: t.risk.production_ratio_pm.map(i64::from).unwrap_or(-1),
                risk: i64::from(t.risk.score),
                reason: why_readout(t.reason),
                seed: SeedReadout {
                    fast_kbps: t.delivery.fast_kbps,
                    slow_kbps: t.delivery.slow_kbps,
                    uncertainty_pm: t.delivery.uncertainty_pm,
                    samples: t.delivery.samples,
                },
            },
            sample_line: sample_line(t, sample, round.decision),
            window_line: window_line(t, sample),
        }
    }
}

impl SampleReadout {
    pub(crate) fn from_sample(sample: abr::SegmentSample) -> Self {
        let (network_kbps, media_kbps, ratio_pm) = if sample.completed() {
            (
                i64::from(sample.network_kbps()),
                i64::from(sample.media_kbps()),
                i64::from(sample.production_ratio_pm()),
            )
        } else { (-1, -1, -1) };
        Self {
            network_kbps, media_kbps, ratio_pm,
            buffer_ms: sample.buffer.buffered_ms().unwrap_or(-1),
        }
    }
}

pub(crate) struct ActiveAfterPrefetch {
    pub(crate) reconsidered: Option<Reconsideration>,
    pub(crate) steady: bool,
    pub(crate) source: LatchedOriginalRecovery,
}

pub(crate) trait ActiveDiagnostics {
    fn cleanup(&mut self, ready: bool);
    fn accepted_pause(&mut self, after_lookup: bool, sequence: u64, elapsed_ms: u64);
    fn sample(&mut self, sample: SampleReadout);
    fn fresh_boundary(&mut self);
    fn observed(&mut self, readout: ActiveReadout);
    /// The controller's delivery estimate after this round, which the next controller starts from.
    fn seed(&mut self, seed: SeedReadout);
}

pub(crate) enum ActiveRunError<E> { Physical(E), Protocol(&'static str) }

pub(crate) enum ActiveRun {
    Ended,
    InvalidSample,
    Observed(ActiveCycle),
}

pub(crate) struct ActiveCycle {
    pub(crate) sample: abr::SegmentSample,
    pub(crate) round: RoundDecision,
    pub(crate) rollback_media_ms: Option<u32>,
    pub(crate) remaining_ms: i64,
    pub(crate) paused: bool,
    pub(crate) expected_user_sequence: u64,
    pub(crate) cleanup_ready: bool,
    pub(crate) after_prefetch: ActiveAfterPrefetch,
}

pub(crate) enum RoundDiagnostic {
    Line(String),
    Steady(String),
}

pub(crate) enum RoundCourse<L, R> {
    Stay,
    OriginalPublished,
    Aborted,
    Candidate {
        proposal: abr::Proposal,
        reserve_policy: abr::ReservePolicy,
        sample: abr::SegmentSample,
        start: CandidateStart<L, R>,
    },
}

fn active_step<P: ActiveFeedPort + ?Sized>(
    port: &mut P, round: RoundId, action: ActiveAction,
) -> Result<ActiveObservation, ActiveRunError<P::Error>> {
    let observed = port.step_active(action).map_err(ActiveRunError::Physical)?;
    if observed.round() != round {
        return Err(ActiveRunError::Protocol("adaptive active reply address mismatch"));
    }
    Ok(observed)
}

/// One active acquisition and the controller observation in their original
/// physical order. The port may block on a queue or playlist; no controller or
/// route lock spans a port call. The two buffer samples bracket the rollback
/// lookup, and both accepted-pause samples precede the policy decision.
pub(crate) fn run_active_round<P: ActiveFeedPort + ?Sized>(
    coordinator: &mut Coordinator, port: &mut P, round: RoundId,
    now_ms: impl Fn() -> u64, remaining_ms: impl Fn() -> Option<i64>,
    user_sequence: impl Fn() -> Option<u64>, session_boundary_pending: &mut bool,
    diagnostics: &mut impl ActiveDiagnostics,
) -> Result<ActiveRun, ActiveRunError<P::Error>> {
    let read = active_step(port, round, ActiveAction::Read {
        round, at_floor: Some(coordinator.at_floor()),
    })?;
    let (acquisition, delivered, abandoned) = match read {
        ActiveObservation::Ready { acquisition, delivered, abandoned, .. } =>
            (acquisition, delivered, abandoned),
        ActiveObservation::Ended { .. } => return Ok(ActiveRun::Ended),
        _ => return Err(ActiveRunError::Protocol("adaptive active read protocol violation")),
    };
    if active_step(port, round, ActiveAction::Feed { round })?
        != (ActiveObservation::Fed { round }) {
        return Err(ActiveRunError::Protocol("adaptive active feed protocol violation"));
    }
    let active_now_ms = now_ms();
    let first_buffer = match active_step(port, round, ActiveAction::Buffer { round })? {
        ActiveObservation::Buffer { fact, .. } => fact,
        _ => return Err(ActiveRunError::Protocol("adaptive active buffer protocol violation")),
    };
    let Some(active) = coordinator.begin_active_quantum(
        acquisition, || first_buffer, abandoned, delivered, active_now_ms,
    ) else { return Ok(ActiveRun::InvalidSample); };
    let cleanup_ready = active.cleanup_ready;
    diagnostics.cleanup(cleanup_ready);
    let before = match active_step(port, round, ActiveAction::Pause { round })? {
        ActiveObservation::Pause { paused, sequence, completed_ms, .. } =>
            (paused, sequence, completed_ms),
        _ => return Err(ActiveRunError::Protocol("adaptive active pause protocol violation")),
    };
    if let Some(elapsed_ms) = before.2 {
        coordinator.on_resume(elapsed_ms);
        diagnostics.accepted_pause(false, before.1, elapsed_ms);
    }
    let rollback_media_ms = if before.0 { None } else {
        match active_step(port, round, ActiveAction::RollbackDuration { round })? {
            ActiveObservation::RollbackDuration { duration_ms, .. } => duration_ms,
            _ => return Err(ActiveRunError::Protocol("adaptive rollback protocol violation")),
        }
    };
    let refreshed_buffer = match active_step(port, round, ActiveAction::Buffer { round })? {
        ActiveObservation::Buffer { fact, .. } => fact,
        _ => return Err(ActiveRunError::Protocol("adaptive refreshed buffer protocol violation")),
    };
    let active = active.refresh(refreshed_buffer);
    let after = match active_step(port, round, ActiveAction::Pause { round })? {
        ActiveObservation::Pause { paused, sequence, completed_ms, .. } =>
            (paused, sequence, completed_ms),
        _ => return Err(ActiveRunError::Protocol("adaptive refreshed pause protocol violation")),
    };
    if let Some(elapsed_ms) = after.2 {
        coordinator.on_resume(elapsed_ms);
        diagnostics.accepted_pause(true, after.1, elapsed_ms);
    }
    let Some(expected_user_sequence) = user_sequence() else {
        return Err(ActiveRunError::Protocol("adaptive active session retired"));
    };
    diagnostics.sample(SampleReadout::from_sample(active.sample()));
    let Some(remaining_ms) = remaining_ms() else {
        return Err(ActiveRunError::Protocol("adaptive active session retired"));
    };
    let boundary = *session_boundary_pending && delivered && active.sample().completed();
    if boundary {
        *session_boundary_pending = false;
        diagnostics.fresh_boundary();
    }
    let (sample, round_decision, prefetch_allowed) =
        active.observe(coordinator, rollback_media_ms, now_ms(), boundary);
    let readout = ActiveReadout::new(&round_decision, sample);
    let seed = readout.model.seed;
    if let Some(runway_ms) = readout.prime_runway_ms {
        port.note_prime_runway(runway_ms);
    }
    diagnostics.observed(readout);
    diagnostics.seed(seed);
    if prefetch_allowed && active_step(port, round, ActiveAction::Prefetch {
        round, at_floor: Some(coordinator.at_floor()),
    })? != (ActiveObservation::Prefetched { round }) {
        return Err(ActiveRunError::Protocol("adaptive active prefetch protocol violation"));
    }
    let after_prefetch = coordinator.after_active_prefetch(
        &round_decision, sample, delivered, remaining_ms, after.0, cleanup_ready,
    );
    Ok(ActiveRun::Observed(ActiveCycle {
        sample, round: round_decision, rollback_media_ms,
        remaining_ms, paused: after.0, expected_user_sequence,
        cleanup_ready, after_prefetch,
    }))
}

pub(crate) struct PreparedOriginalSwitch {
    pub(crate) kbps: u32,
    pub(crate) cancelled: Option<(abr::Proposal, bool)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OriginalSwitchOutcome {
    NoSwitch,
    LeaseBusy,
    Published { kbps: u32 },
    Busy,
    ClockMoved,
    Aborted,
    RouteMoved,
}

pub(crate) struct ActiveQuantumResult {
    sample: abr::SegmentSample,
    pub(crate) cleanup_ready: bool,
    delivered: bool,
    abandoned: bool,
}

pub(crate) struct RefreshedQuantum {
    sample: abr::SegmentSample,
    delivered: bool,
    abandoned: bool,
}

impl ActiveQuantumResult {
    /// The rollback playlist may have blocked. It supplies a second buffer
    /// fact after that wait without changing this acquisition's transfer.
    pub(crate) fn refresh(self, buffer: BufferFact) -> RefreshedQuantum {
        RefreshedQuantum {
            sample: self.sample.with_buffer(buffer_snapshot(buffer)),
            delivered: self.delivered,
            abandoned: self.abandoned,
        }
    }
}

impl RefreshedQuantum {
    pub(crate) fn sample(&self) -> abr::SegmentSample { self.sample }

    pub(crate) fn observe(
        self, coordinator: &mut Coordinator, rollback_media_ms: Option<u32>,
        now_ms: u64, boundary: bool,
    ) -> (abr::SegmentSample, RoundDecision, bool) {
        let round = coordinator.observe(
            self.sample, rollback_media_ms, now_ms, boundary,
        );
        let prefetch = self.delivered && !self.abandoned
            && matches!(round.decision, abr::Decision::Stay);
        (self.sample, round, prefetch)
    }
}

pub(crate) struct Reconsideration {
    pub(crate) verdict: abr::RecoveryVerdict,
    pub(crate) measured_kbps: u32,
    pub(crate) comparison: Option<abr::ModeComparison>,
}

pub(crate) struct ProbeDecision {
    result: Result<abr::ProbePermit, abr::ProbeBlock>,
    current_wire_kbps: u32,
    conservative_kbps: u32,
}

pub(crate) struct ProbeBlockSummary {
    pub(crate) reason: abr::ProbeBlock,
    pub(crate) current_wire_kbps: u32,
    pub(crate) conservative_kbps: u32,
}

pub(crate) enum RoundAction {
    Stay,
    ProbeBlocked(ProbeBlockSummary),
    ProbeDeferredForCleanup,
    Probe(abr::ProbePermit),
    TrialDeferred {
        proposal: abr::Proposal,
        refreshing_same_request: bool,
        reason: TrialDeferral,
    },
    Trial { proposal: abr::Proposal, reserve_policy: abr::ReservePolicy },
}

pub(crate) enum TrialDeferral { EncoderCleanup, PlaybackClock }

pub(crate) struct ProbeGrade {
    pub(crate) verdict: abr::RecoveryVerdict,
    pub(crate) comparison: Option<abr::ModeComparison>,
    pub(crate) basis: (u32, u32, u32, u32, u32),
    pub(crate) probes: u8,
}

pub(crate) enum ProbeRecoveryAction {
    Steady,
    Held,
    Retained,
    Switch { kbps: u32, measured_kbps: u32 },
}

#[derive(Clone, Copy)]
pub(crate) enum ProbeTraceAction { Start, Failed, Steady, RecoverOriginal }

pub(crate) trait OriginalProbeDiagnostics {
    fn action(&mut self, action: ProbeTraceAction, target_kbps: u32);
    fn failure(&mut self, failure: ProbeFailure);
    fn record(&mut self, line: &str);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OriginalProbeOutcome { LeaseBusy, Stale, Continue, Published, Aborted }

pub(crate) fn mode_comparison_line(cmp: abr::ModeComparison) -> String {
    let loser = cmp.loser.unwrap_or_default();
    format!(
        "abr: mode chose={:?} why={:?} vs_hls={}kbps scale={}pm \
         win[q={} f={} r={} s={} t={} tot={}] \
         lose[q={} f={} r={} s={} t={} tot={}]",
        cmp.chosen, cmp.reason, cmp.hls_rung.kbps(), cmp.scale_pm,
        cmp.winner.quality, cmp.winner.features, cmp.winner.risk,
        cmp.winner.server, cmp.winner.transition, cmp.winner.total,
        loser.quality, loser.features, loser.risk, loser.server,
        loser.transition, loser.total,
    )
}

#[derive(Clone, Copy)]
pub(crate) struct VariantFacts {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) declared_bps: u64,
}

struct FirstCandidateGrade {
    verdict: abr::CandidateVerdict,
    needs_followup: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum FirstCandidateAction {
    Finalize { verdict: abr::CandidateVerdict },
    ReadFollowup { verdict: abr::CandidateVerdict, remaining_ms: Option<u64> },
    Unfunded,
    Protocol,
}

pub(crate) enum StagedCandidate {
    Ready {
        final_acquisition: Acquisition,
        final_buffer: BufferFact,
        verdict: abr::CandidateVerdict,
        grade: FinalCandidateGrade,
        staged_count: usize,
    },
    Unfunded,
    Failed { step: TrialStep, failure: FeedFailure },
    Protocol(&'static str),
}

pub(crate) enum CandidateJudgment {
    Queue {
        final_acquisition: Acquisition,
        sample: abr::SegmentSample,
        variant: abr::ObservedHlsVariant,
    },
    Stay,
    Aborted,
    Failed(&'static str),
}

pub(crate) enum QueueTrace {
    Decided,
    FeedCompleted(std::time::Instant),
    Finish(&'static str),
}

pub(crate) enum QueueAttempt<F> {
    Published { previous: String, fence: F },
    Aborted,
    Failed(&'static str),
    FenceFailed(CandidateFenceError),
}

impl<F> QueueAttempt<F> {
    /// Only a refusal before queueing may keep the current demux round alive.
    /// After queueing starts, failures still retire the physical worker.
    pub(crate) fn into_publication(self) -> Result<(String, F), AdaptiveProgress> {
        match self {
            Self::Published { previous, fence } => Ok((previous, fence)),
            Self::Aborted => Err(AdaptiveProgress::Aborted),
            Self::Failed(message) => Err(AdaptiveProgress::Failed(message)),
            Self::FenceFailed(CandidateFenceError::Superseded) => Err(AdaptiveProgress::Continue),
            Self::FenceFailed(CandidateFenceError::Aborted) => Err(AdaptiveProgress::Aborted),
            Self::FenceFailed(CandidateFenceError::Overlap) =>
                Err(AdaptiveProgress::Failed("overlapping HLS candidate publication")),
            Self::FenceFailed(CandidateFenceError::Exhausted) =>
                Err(AdaptiveProgress::Failed("HLS candidate generation exhausted")),
        }
    }
}

pub(crate) enum FirstPhase {
    Ready {
        primed: route::PrimedHls,
        declared_bps: u64,
        acquisition: Acquisition,
        buffer: BufferFact,
    },
    Stay,
    Aborted,
    Failed(&'static str),
}

pub(crate) enum TrialTrace {
    Prime,
    Master(u64),
    ControlPlane,
    WarmupDeadline(std::time::Duration),
    FirstMedia(Acquisition),
    Decided,
    Rejected { refreshing: bool },
    Finish(&'static str),
    Log(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrialTraceMoment {
    FollowupMedia(Acquisition),
    FinalEvidence(Acquisition),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrialStep {
    Open, FirstRead, FirstAcquire, FollowupRead, FollowupAcquire,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrialRejectReason { SourceMoved, Ended, Deadline, Other }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrialFailureAction {
    Abort,
    Protocol,
    TimelineOverflow,
    Reject { cause: abr::RejectCause, reason: TrialRejectReason },
}

/// A physical reply does not decide whether a rung failed. This policy table
/// distinguishes terminal abort/protocol faults from circumstance or censored
/// evidence, including the stage where EOF and origin changes have meaning.
pub(crate) fn trial_failure_action(
    step: TrialStep, failure: FeedFailure,
) -> TrialFailureAction {
    use FeedFailure as F;
    use TrialFailureAction as A;
    use TrialRejectReason as R;
    use TrialStep as S;
    match (step, failure) {
        (_, F::Aborted) | (S::FirstRead | S::FollowupRead, F::SourceMoved) => A::Abort,
        (S::FirstAcquire | S::FollowupAcquire, F::TimelineOverflow) => A::TimelineOverflow,
        (_, F::Protocol) | (_, F::TimelineOverflow)
        | (S::FirstAcquire | S::FollowupAcquire, F::SourceMoved | F::Ended)
        | (S::Open, F::Ended) => A::Protocol,
        (S::Open, F::SourceMoved) => A::Reject {
            cause: abr::RejectCause::Circumstance, reason: R::SourceMoved,
        },
        (S::FirstRead | S::FollowupRead, F::Ended) => A::Reject {
            cause: abr::RejectCause::Circumstance, reason: R::Ended,
        },
        (_, F::Deadline) => A::Reject {
            cause: abr::RejectCause::Censored, reason: R::Deadline,
        },
        (_, F::NotReady | F::Parse) => A::Reject {
            cause: abr::RejectCause::Circumstance, reason: R::Other,
        },
    }
}

fn feed_failure(observation: FeedObservation) -> FeedFailure {
    match observation {
        FeedObservation::Failed { failure, .. } => failure,
        _ => FeedFailure::Protocol,
    }
}

pub(crate) struct FinalCandidateGrade {
    pub(crate) raster_ready: bool,
    pub(crate) quality_improves: bool,
    pub(crate) disposition: HlsCandidateDisposition,
}

pub(crate) struct CandidateBudget {
    pub(crate) media_deadline: Option<std::time::Duration>,
    pub(crate) yields_to_rebuffer: bool,
    pub(crate) clock: TrialClock,
}

pub(crate) struct ExplorationGrant {
    pub(crate) buffered_ms: i64,
    pub(crate) runway_ms: i64,
    pub(crate) budget_ms: i64,
}

pub(crate) enum PreprimePlan {
    Recovery,
    Exploration { grant: ExplorationGrant, started_ns: i64 },
}

pub(crate) struct ExplorationClock {
    pub(crate) buffered_ms: i64,
    pub(crate) runway_ms: i64,
    pub(crate) budget_ms: i64,
    pub(crate) started_ns: i64,
}

pub(crate) struct CandidateStart<L, R> {
    pub(crate) automatic_lease: Option<L>,
    pub(crate) trial_reserve: Option<R>,
    pub(crate) token: u64,
    pub(crate) exploration: Option<ExplorationClock>,
}

pub(crate) enum CandidateStartRefusal {
    Retired,
    ReserveUnknown,
    NoRunway,
    NoSurplus,
    BudgetMoved { budget_ms: i64, target_kbps: u32 },
    LeaseMoved,
    TrialReserveMoved,
}

pub(crate) enum ExplorationFailure {
    Retired,
    ReserveUnknown { refreshing_same_request: bool },
    NoRunway { refreshing_same_request: bool },
    NoSurplus { refreshing_same_request: bool },
    BudgetMoved { budget_ms: i64 },
}

impl ExplorationFailure {
    pub(crate) fn refreshing_same_request(&self) -> Option<bool> {
        match self {
            Self::ReserveUnknown { refreshing_same_request }
            | Self::NoRunway { refreshing_same_request }
            | Self::NoSurplus { refreshing_same_request } => Some(*refreshing_same_request),
            Self::BudgetMoved { .. } | Self::Retired => None,
        }
    }
}

impl Coordinator {
    pub(crate) fn start(
        start: Start, pin: Option<abr::Rung>,
    ) -> Self {
        let Start { control, ticket } = start;
        let recovery = control.can_recover_original().then(|| {
            abr::OriginalRecovery::new(
                control.original_source_kbps(),
                abr::AbrPolicy::measured(),
                control.original_features,
                control.history.advanced_by(0),
                control.catalog,
            )
        }).flatten().map(|gate| {
            if control.original_refused_on_this_route() {
                gate.after_route_refusal()
            } else {
                gate
            }
        });
        let mut controller = abr::Controller::starting_at(
            control.initial_rung, control.prior, control.catalog,
        ).pinned_to(pin);
        if let Some((variant, evidence_kbps)) = control.initial_observed {
            controller.observe_active_variant(variant, evidence_kbps);
        }
        Self {
            control, ticket, controller, recovery, recover_kbps: 0,
            last_probe_block: None,
            next_trial_serial: 0, next_round_serial: 0,
        }
    }

    pub(crate) fn telemetry(&self) -> abr::ControllerTelemetry {
        self.controller.telemetry()
    }

    pub(crate) fn has_recovery(&self) -> bool { self.recovery.is_some() }

    pub(crate) fn at_floor(&self) -> bool { self.controller.current().at_floor() }

    pub(crate) fn current(&self) -> abr::Rung { self.controller.current() }
    pub(crate) fn delivery(&self) -> abr::CapacityEstimate { self.controller.delivery() }
    pub(crate) fn prime_runway_ms(&self) -> Option<i64> { self.controller.prime_runway_ms() }
    fn candidate_wire_kbps(&self, rung: abr::Rung) -> u32 {
        self.controller.catalog().candidate(rung).expected_wire_kbps
    }
    pub(crate) fn pending_reserve_policy(&self, proposal: abr::Proposal) -> abr::ReservePolicy {
        self.controller.pending_reserve_policy(proposal)
            .expect("a Prime decision owns one pending reserve policy")
    }
    pub(crate) fn candidate_budget_from_snapshot(
        &self, proposal: abr::Proposal, reserve_policy: abr::ReservePolicy,
        candidate_duration: std::time::Duration, buffered_ms: i64,
        remaining: Option<std::time::Duration>, rebuffering: bool,
    ) -> CandidateBudget {
        let reserve = remaining.unwrap_or_else(|| abr::reserve_as_budget(buffered_ms));
        self.candidate_budget(
            proposal, reserve_policy, candidate_duration, reserve, rebuffering,
        )
    }
    pub(crate) fn begin_candidate<P: AdaptiveRoundPort + ?Sized>(
        &mut self, proposal: abr::Proposal, port: &mut P,
        expected_user_sequence: u64,
        now_ms: impl Fn() -> u64,
        mut on_rejected: impl FnMut(abr::Proposal, bool),
    ) -> Result<CandidateStart<P::Lease, P::TrialReserve>, CandidateStartRefusal> {
        let plan = match self.preprime_plan(proposal, || port.buffer_snapshot(), || now_ms()) {
            Ok(plan) => plan,
            Err(failure) => {
                if let Some(refreshing) = failure.refreshing_same_request() {
                    on_rejected(proposal, refreshing);
                }
                return Err(match failure {
                    ExplorationFailure::Retired => CandidateStartRefusal::Retired,
                    ExplorationFailure::ReserveUnknown { .. } => CandidateStartRefusal::ReserveUnknown,
                    ExplorationFailure::NoRunway { .. } => CandidateStartRefusal::NoRunway,
                    ExplorationFailure::NoSurplus { .. } => CandidateStartRefusal::NoSurplus,
                    ExplorationFailure::BudgetMoved { budget_ms } => CandidateStartRefusal::BudgetMoved {
                        budget_ms, target_kbps: proposal.rung.kbps(),
                    },
                });
            }
        };
        match plan {
            PreprimePlan::Recovery => {
                let Some(lease) = port.begin(
                    AutomaticTransition::QualityDown, expected_user_sequence,
                ) else {
                    let refreshing = self.reject(proposal, abr::RejectCause::Circumstance, now_ms());
                    on_rejected(proposal, refreshing);
                    return Err(CandidateStartRefusal::LeaseMoved);
                };
                let token = port.lease_token(&lease);
                Ok(CandidateStart {
                    automatic_lease: Some(lease), trial_reserve: None,
                    token, exploration: None,
                })
            }
            PreprimePlan::Exploration { grant, started_ns } => {
                let Some(reserve) = port.arm_trial_reserve(
                    grant.runway_ms, expected_user_sequence,
                ) else {
                    let refreshing = self.reject(proposal, abr::RejectCause::Circumstance, now_ms());
                    on_rejected(proposal, refreshing);
                    return Err(CandidateStartRefusal::TrialReserveMoved);
                };
                let token = port.trial_token(&reserve);
                Ok(CandidateStart {
                    automatic_lease: None, trial_reserve: Some(reserve), token,
                    exploration: Some(ExplorationClock {
                        buffered_ms: grant.buffered_ms,
                        runway_ms: grant.runway_ms,
                        budget_ms: grant.budget_ms,
                        started_ns,
                    }),
                })
            }
        }
    }
    pub(crate) fn next_round_id(&mut self) -> Option<RoundId> {
        self.next_round_serial = self.next_round_serial.checked_add(1)?;
        Some(RoundId::new(self.ticket.adaptive_attempt(), self.next_round_serial))
    }
    pub(crate) fn begin_trial_protocol(
        &mut self,
    ) -> Option<plx_playback_contract::adaptive::TrialProtocol> {
        self.next_trial_serial = self.next_trial_serial.checked_add(1)?;
        let id = plx_playback_contract::adaptive::TrialId::new(
            self.ticket.adaptive_attempt(), self.next_trial_serial,
        );
        Some(plx_playback_contract::adaptive::TrialProtocol::new(id))
    }
    fn grant_exploration(
        &mut self, proposal: abr::Proposal, buffered_ms: Option<i64>, now_ms: u64,
    ) -> Result<ExplorationGrant, ExplorationFailure> {
        let Some(buffered_ms) = buffered_ms else {
            let refreshing_same_request = self.reject(
                proposal, abr::RejectCause::Circumstance, now_ms,
            );
            return Err(ExplorationFailure::ReserveUnknown { refreshing_same_request });
        };
        let Some(runway_ms) = self.controller.prime_runway_ms() else {
            let refreshing_same_request = self.reject(
                proposal, abr::RejectCause::Circumstance, now_ms,
            );
            return Err(ExplorationFailure::NoRunway { refreshing_same_request });
        };
        let Some(budget_ms) = self.controller.exploration_budget_ms(buffered_ms) else {
            let refreshing_same_request = self.reject(
                proposal, abr::RejectCause::Circumstance, now_ms,
            );
            return Err(ExplorationFailure::NoSurplus { refreshing_same_request });
        };
        if !self.controller.set_executed_exploration_budget(proposal, budget_ms) {
            return Err(ExplorationFailure::BudgetMoved { budget_ms });
        }
        Ok(ExplorationGrant { buffered_ms, runway_ms, budget_ms })
    }
    pub(crate) fn preprime_plan(
        &mut self, proposal: abr::Proposal,
        buffer: impl FnOnce() -> Option<(Option<i64>, i64)>,
        now_ms: impl FnOnce() -> u64,
    ) -> Result<PreprimePlan, ExplorationFailure> {
        match proposal.direction {
            abr::Direction::Down => Ok(PreprimePlan::Recovery),
            abr::Direction::Up => {
                let (buffered_ms, started_ns) = buffer()
                    .ok_or(ExplorationFailure::Retired)?;
                let grant = self.grant_exploration(proposal, buffered_ms, now_ms())?;
                Ok(PreprimePlan::Exploration { grant, started_ns })
            }
        }
    }
    pub(crate) fn reject(&mut self, proposal: abr::Proposal, cause: abr::RejectCause, now_ms: u64) -> bool {
        let refreshing = proposal.rung == self.controller.current();
        self.controller.reject(proposal, cause, now_ms);
        refreshing
    }
    fn candidate_verdict(
        &self, proposal: abr::Proposal, sample: abr::SegmentSample,
        declared_bps: u64, boundary: bool,
    ) -> abr::CandidateVerdict {
        if boundary {
            self.controller.candidate_boundary_verdict(proposal, sample, declared_bps)
        } else {
            self.controller.candidate_verdict(proposal, sample, declared_bps)
        }
    }
    fn first_candidate_grade(
        &self, proposal: abr::Proposal, current: VariantFacts,
        candidate: VariantFacts, sample: Option<abr::SegmentSample>,
    ) -> FirstCandidateGrade {
        let raster_ready = hls_raster_within(candidate.width, candidate.height, proposal.rung);
        let quality_improves = proposal.direction != abr::Direction::Up
            || hls_upshift_strictly_improves(
                current.width, current.height, current.declared_bps,
                candidate.width, candidate.height, candidate.declared_bps,
            );
        let verdict = sample.map_or(abr::CandidateVerdict::Incomplete, |sample| {
            self.candidate_verdict(
                proposal, sample, candidate.declared_bps,
                proposal.direction == abr::Direction::Up,
            )
        });
        FirstCandidateGrade {
            verdict,
            needs_followup: raster_ready && quality_improves
                && verdict == abr::CandidateVerdict::SetupBearing,
        }
    }
    fn final_candidate_grade(
        &self, proposal: abr::Proposal, reserve_policy: abr::ReservePolicy,
        current: VariantFacts, candidate: VariantFacts,
        acquisitions: &[Acquisition], verdict: abr::CandidateVerdict,
    ) -> FinalCandidateGrade {
        let outputs_inside_rung = acquisitions.iter().all(|output| {
            hls_raster_within(output.width, output.height, proposal.rung)
        });
        let raster_ready = !hls_candidate_requires_rung_box(proposal, reserve_policy)
            || outputs_inside_rung;
        let quality_improves = proposal.direction != abr::Direction::Up
            || hls_upshift_strictly_improves(
                current.width, current.height, current.declared_bps,
                candidate.width, candidate.height, candidate.declared_bps,
            );
        FinalCandidateGrade {
            raster_ready, quality_improves,
            disposition: hls_candidate_disposition(raster_ready, quality_improves, verdict),
        }
    }
    pub(crate) fn candidate_budget(
        &self, proposal: abr::Proposal, reserve_policy: abr::ReservePolicy,
        duration: std::time::Duration, reserve: std::time::Duration,
        internally_held: bool,
    ) -> CandidateBudget {
        let delivery = self.controller.delivery();
        let predicted = abr::predicted_transfer(
            self.candidate_wire_kbps(proposal.rung), duration,
            delivery.conservative_kbps(), delivery.uncertainty_pm,
        );
        let warmup = abr::candidate_warmup_budget(
            proposal, duration, reserve, predicted,
            std::time::Duration::from_micros(self.controller.worst_overhead_us()),
        );
        CandidateBudget {
            media_deadline: abr::candidate_media_reserve_deadline(
                proposal, reserve_policy, internally_held, warmup,
            ),
            yields_to_rebuffer: proposal.direction == abr::Direction::Down
                && proposal.rung.at_floor(),
            clock: match proposal.direction {
                abr::Direction::Up => TrialClock::Playhead,
                abr::Direction::Down => TrialClock::UnpausedElapsed,
            },
        }
    }
    /// A setup-bearing first object earns one ordinary continuation only while
    /// the original trial clock still funds it. An exhausted clock is censored
    /// budget evidence; the physical reader supplies that clock verdict.
    pub(crate) fn fund_followup(
        &mut self, proposal: abr::Proposal, funded: bool, now_ms: u64,
    ) -> Result<(), bool> {
        if funded { return Ok(()); }
        let refreshing_same_request = self.reject(
            proposal, abr::RejectCause::Censored, now_ms,
        );
        Err(refreshing_same_request)
    }
    pub(crate) fn abandon(&self, encoder: &str) { self.control.abandon(encoder); }
    pub(crate) fn retire(&self, encoder: String) { self.control.retire(encoder); }
    pub(crate) fn commit_readout(
        &self, proposal: abr::Proposal, refreshing: bool, declared_bps: u64,
        observed: Acquisition,
    ) -> CommitReadout {
        CommitReadout {
            trace_generation: self.control.trace_generation(),
            direction: action_readout(proposal, refreshing, Some(true)).direction,
            target_kbps: proposal.rung.kbps(), refreshing, declared_bps,
            observed_width: observed.width, observed_height: observed.height,
        }
    }
    pub(crate) fn original_source_kbps(&self) -> u32 { self.control.original_source_kbps() }
    pub(crate) fn retain_original_recovery(&mut self, kbps: u32) { self.recover_kbps = kbps; }
    pub(crate) fn on_hls_commit(&mut self, direction: abr::Direction) {
        if let Some(gate) = self.recovery.as_mut() { gate.on_hls_commit(direction); }
        self.recover_kbps = 0;
        self.last_probe_block = None;
    }
    fn reconsider_original(
        &mut self, telemetry: &abr::ControllerTelemetry, remaining_ms: i64,
    ) -> Option<Reconsideration> {
        let current = self.controller.catalog().candidate(self.controller.current());
        let gate = self.recovery.as_mut()?;
        let result = gate.reconsider_after_hls_commit(
            current, &telemetry.production, telemetry.buffer, &telemetry.delivery, remaining_ms,
        )?;
        self.recover_kbps = if result.verdict == abr::RecoveryVerdict::Recover {
            result.measured_kbps.max(1)
        } else { 0 };
        Some(Reconsideration {
            verdict: result.verdict, measured_kbps: result.measured_kbps,
            comparison: gate.comparison(),
        })
    }
    pub(crate) fn probe_due(
        &mut self, telemetry: &abr::ControllerTelemetry, sample: abr::SegmentSample,
        rollback_media_ms: Option<u32>, remaining_ms: i64, now_ms: u64,
    ) -> Option<ProbeDecision> {
        let current = self.controller.catalog().candidate(self.controller.current());
        let delivery = self.controller.delivery();
        let result = self.recovery.as_mut()?.probe_due_with_rollback(
            current, self.controller.hls_frontier_exhausted(), &telemetry.production,
            sample, rollback_media_ms, self.controller.prime_runway_ms(),
            self.controller.buffer(), &delivery, remaining_ms, now_ms,
        );
        Some(ProbeDecision {
            result, current_wire_kbps: current.expected_wire_kbps,
            conservative_kbps: delivery.conservative_kbps(),
        })
    }
    pub(crate) fn source_switch(
        &self, fresh_probe_kbps: Option<u32>, decision: abr::Decision,
        cleanup_ready: bool, buffered_ms: Option<i64>, media_duration_ms: u32,
    ) -> LatchedOriginalRecovery {
        latched_original_recovery(
            self.recover_kbps, fresh_probe_kbps, decision, cleanup_ready,
            buffered_ms, media_duration_ms,
        )
    }

    /// A latched source recovery wins over a pending rung proposal only after
    /// the physical automatic lease is acquired. Return its diagnostic receipt
    /// before the caller enters the AQ publication fence.
    pub(crate) fn prepare_original_switch(
        &mut self, source: LatchedOriginalRecovery, now_ms: u64,
    ) -> Option<PreparedOriginalSwitch> {
        let LatchedOriginalRecovery::Switch { kbps, cancel } = source else {
            return None;
        };
        let cancelled = cancel.map(|proposal| {
            let refreshing = self.reject(proposal, abr::RejectCause::Circumstance, now_ms);
            (proposal, refreshing)
        });
        Some(PreparedOriginalSwitch { kbps, cancelled })
    }

    /// A source latch wins a physical lease before cancelling a competing rung.
    /// Publication itself is short and runs under the feed's AQ abort fence;
    /// a Busy or moved clock retains source evidence for a later boundary.
    pub(crate) fn run_latched_original<P: AdaptiveRoundPort + ?Sized>(
        &mut self, source: LatchedOriginalRecovery, port: &mut P,
        expected_user_sequence: u64, now_ms: impl FnOnce() -> u64,
        on_published: impl FnOnce(),
        on_cancelled_rung: impl FnOnce(abr::Proposal, bool),
    ) -> OriginalSwitchOutcome {
        if !matches!(source, LatchedOriginalRecovery::Switch { .. }) {
            return OriginalSwitchOutcome::NoSwitch;
        }
        let Some(lease) = port.begin(AutomaticTransition::Original, expected_user_sequence) else {
            return OriginalSwitchOutcome::LeaseBusy;
        };
        let prepared = self.prepare_original_switch(source, now_ms())
            .expect("a source latch was checked before acquiring its lease");
        if let Some((proposal, refreshing)) = prepared.cancelled {
            on_cancelled_rung(proposal, refreshing);
        }
        let authorized = port.authorize(&lease);
        let position_ns = port.position_sampler();
        match self.publish_original(
            port.gate(), authorized, prepared.kbps, move || position_ns(), on_published,
        ) {
            OriginalPublication::Published => OriginalSwitchOutcome::Published { kbps: prepared.kbps },
            OriginalPublication::Busy => OriginalSwitchOutcome::Busy,
            OriginalPublication::ClockMoved => OriginalSwitchOutcome::ClockMoved,
            OriginalPublication::Aborted => OriginalSwitchOutcome::Aborted,
            OriginalPublication::RouteMoved => OriginalSwitchOutcome::RouteMoved,
        }
    }


    /// A bounded Original probe keeps its actuator lease through physical GET,
    /// fresh pause settlement and the AQ-fenced route request. A stale resource
    /// produces no source evidence; Busy and clock movement retain a recovery.
    pub(crate) fn run_original_probe<P: AdaptiveRoundPort + ?Sized>(
        &mut self, permit: abr::ProbePermit, port: &mut P,
        expected_user_sequence: u64, telemetry: &abr::ControllerTelemetry,
        remaining_ms: i64, decision: abr::Decision, cleanup_ready: bool,
        buffered_ms: Option<i64>, media_duration_ms: u32,
        diagnostics: &mut impl OriginalProbeDiagnostics,
    ) -> OriginalProbeOutcome {
        let Some(lease) = port.begin(AutomaticTransition::Original, expected_user_sequence) else {
            diagnostics.record("abr: Original probe deferred behind a playback-clock transition");
            return OriginalProbeOutcome::LeaseBusy;
        };
        diagnostics.action(ProbeTraceAction::Start, self.original_source_kbps());
        diagnostics.record("abr: checking finite actual Original range on the live HLS resource");
        let probe = match self.probe_original(permit.plan, || port.aborted()) {
            ProbeResult::Measured(sample) => sample,
            ProbeResult::Stale => {
                diagnostics.record("abr: discarded Original probe from a superseded HLS resource");
                return OriginalProbeOutcome::Stale;
            }
            ProbeResult::Failed { outcome, failure } => {
                diagnostics.failure(failure);
                let grade = self.probe_failed();
                diagnostics.action(ProbeTraceAction::Failed, 0);
                diagnostics.record(&format!(
                    "abr: Original probe #{} produced no body outcome={}; retaining best HLS \
                     request left={}s verdict={:?}",
                    grade.as_ref().map(|grade| grade.probes).unwrap_or(0),
                    outcome, remaining_ms / 1_000,
                    grade.as_ref().map(|grade| grade.verdict),
                ));
                return OriginalProbeOutcome::Continue;
            }
        };
        let grade = self.grade_probe(probe, telemetry, remaining_ms);
        if let Some(cmp) = grade.as_ref().and_then(|grade| grade.comparison) {
            diagnostics.record(&mode_comparison_line(cmp));
        }
        let (slow, unc, n, cons, need) = grade.as_ref()
            .map(|grade| grade.basis).unwrap_or_default();
        diagnostics.record(&format!(
            "abr: Original probe #{} measured={}kbps {}KiB/{}ms complete={} left={}s \
             slow={}kbps unc={}pm n={} cons={}kbps need={}kbps verdict={:?}",
            grade.as_ref().map(|grade| grade.probes).unwrap_or(0),
            probe.kbps, probe.bytes / 1024, probe.active_us / 1_000,
            probe.completed as i32, remaining_ms / 1_000,
            slow, unc, n, cons, need,
            grade.as_ref().map(|grade| grade.verdict),
        ));
        let recovery = self.settle_probe_recovery(
            grade.as_ref(), probe.kbps, || port.pause_fact(), decision, cleanup_ready,
            buffered_ms, media_duration_ms,
        );
        match recovery {
            ProbeRecoveryAction::Steady => {
                diagnostics.action(ProbeTraceAction::Steady, 0);
                OriginalProbeOutcome::Continue
            }
            ProbeRecoveryAction::Held => {
                diagnostics.record(
                    "abr: Original recovery measured during a user hold; retained until Resume",
                );
                OriginalProbeOutcome::Continue
            }
            ProbeRecoveryAction::Retained => OriginalProbeOutcome::Continue,
            ProbeRecoveryAction::Switch { kbps, measured_kbps } => {
                let authorized = port.authorize(&lease);
                let position_ns = port.position_sampler();
                let publication = self.publish_probed_original(
                    port.gate(), authorized, kbps, measured_kbps, move || position_ns(),
                    || diagnostics.action(ProbeTraceAction::RecoverOriginal, 0),
                );
                match publication {
                    OriginalPublication::Published => {
                        diagnostics.record(&format!(
                            "abr: source sustainable again at {}kbps; requesting Original on the probe boundary",
                            kbps,
                        ));
                        OriginalProbeOutcome::Published
                    }
                    OriginalPublication::Busy => {
                        diagnostics.record(
                            "abr: Original recovery retained behind another route transition",
                        );
                        OriginalProbeOutcome::Continue
                    }
                    OriginalPublication::ClockMoved => {
                        diagnostics.record(
                            "abr: Original recovery retained after playback clock moved",
                        );
                        OriginalProbeOutcome::Continue
                    }
                    OriginalPublication::Aborted | OriginalPublication::RouteMoved =>
                        OriginalProbeOutcome::Aborted,
                }
            }
        }
    }

    /// The active observation is complete and any same-encoder lookahead has
    /// finished. Reconsider a retained Original verdict before choosing the
    /// higher-priority source switch or the next quality action.
    pub(crate) fn after_active_prefetch(
        &mut self, round: &RoundDecision, sample: abr::SegmentSample,
        delivered: bool, remaining_ms: i64, paused: bool, cleanup_ready: bool,
    ) -> ActiveAfterPrefetch {
        let reconsidered = if delivered && sample.completed() {
            self.reconsider_original(&round.telemetry, remaining_ms)
        } else {
            None
        };
        let source = if paused {
            LatchedOriginalRecovery::Wait
        } else {
            self.source_switch(
                None, round.decision, cleanup_ready,
                sample.buffer.buffered_ms(), sample.media_duration_ms(),
            )
        };
        ActiveAfterPrefetch {
            reconsidered,
            steady: matches!(round.decision, abr::Decision::Stay),
            source,
        }
    }

    /// Called after the higher-priority latched source switch had its publication
    /// opportunity. A probe spends its permit only on a Stay boundary; a quality
    /// transaction receives its pending reserve policy from the same controller.
    pub(crate) fn advance_round<P: AdaptiveRoundPort + ?Sized>(
        &mut self, cycle: ActiveCycle, port: &mut P,
        now_ms: impl Fn() -> u64,
        on_recover_original: impl FnOnce(),
        mut on_action: impl FnMut(ActionReadout),
        mut on_diagnostic: impl FnMut(RoundDiagnostic),
        probe_diagnostics: &mut impl OriginalProbeDiagnostics,
    ) -> RoundCourse<P::Lease, P::TrialReserve> {
        let ActiveCycle {
            sample, round, rollback_media_ms, remaining_ms, paused,
            expected_user_sequence, cleanup_ready, after_prefetch,
        } = cycle;
        if let Some(reconsidered) = after_prefetch.reconsidered {
            self.last_probe_block = None;
            if let Some(cmp) = reconsidered.comparison {
                on_diagnostic(RoundDiagnostic::Line(mode_comparison_line(cmp)));
            }
            on_diagnostic(RoundDiagnostic::Line(format!(
                "abr: retained Original probe reconsidered after HLS commit measured={}kbps verdict={:?}",
                reconsidered.measured_kbps, reconsidered.verdict,
            )));
        }
        if after_prefetch.steady {
            on_diagnostic(RoundDiagnostic::Steady(steady_line(&round.telemetry, remaining_ms)));
        }
        let source = self.run_latched_original(
            after_prefetch.source, port, expected_user_sequence, || now_ms(),
            on_recover_original,
            |proposal, refreshing| on_action(action_readout(
                proposal, refreshing, Some(false),
            )),
        );
        match source {
            OriginalSwitchOutcome::NoSwitch | OriginalSwitchOutcome::LeaseBusy => {}
            OriginalSwitchOutcome::Published { kbps } => {
                on_diagnostic(RoundDiagnostic::Line(format!(
                    "abr: source sustainable again at {}kbps; requesting Original", kbps,
                )));
                return RoundCourse::OriginalPublished;
            }
            OriginalSwitchOutcome::Busy => on_diagnostic(RoundDiagnostic::Line(
                "abr: Original recovery retained behind another route transition".to_string(),
            )),
            OriginalSwitchOutcome::ClockMoved => on_diagnostic(RoundDiagnostic::Line(
                "abr: Original recovery retained after playback clock moved".to_string(),
            )),
            OriginalSwitchOutcome::Aborted | OriginalSwitchOutcome::RouteMoved =>
                return RoundCourse::Aborted,
        }
        let action = self.next_round_action(
            &round, sample, rollback_media_ms, remaining_ms, || now_ms(),
            paused, cleanup_ready,
            |kind| port.transition_permitted(kind), || port.rebuffer_state(),
        );
        match action {
            RoundAction::Stay => RoundCourse::Stay,
            RoundAction::ProbeDeferredForCleanup => {
                self.last_probe_block = None;
                RoundCourse::Stay
            }
            RoundAction::ProbeBlocked(ProbeBlockSummary {
                reason, current_wire_kbps, conservative_kbps,
            }) => {
                if self.last_probe_block != Some(reason) {
                    self.last_probe_block = Some(reason);
                    on_diagnostic(RoundDiagnostic::Line(format!(
                        "abr: probe withheld reason={} rung={}kbps buf={}ms safe={}kbps",
                        reason.as_str(), current_wire_kbps,
                        sample.buffer.buffered_ms().unwrap_or(-1), conservative_kbps,
                    )));
                }
                RoundCourse::Stay
            }
            RoundAction::Probe(permit) => {
                self.last_probe_block = None;
                match self.run_original_probe(
                    permit, port, expected_user_sequence, &round.telemetry,
                    remaining_ms, round.decision, cleanup_ready,
                    sample.buffer.buffered_ms(), sample.media_duration_ms(),
                    probe_diagnostics,
                ) {
                    OriginalProbeOutcome::Published => RoundCourse::OriginalPublished,
                    OriginalProbeOutcome::Aborted => RoundCourse::Aborted,
                    OriginalProbeOutcome::LeaseBusy | OriginalProbeOutcome::Stale
                    | OriginalProbeOutcome::Continue => RoundCourse::Stay,
                }
            }
            RoundAction::TrialDeferred { proposal, refreshing_same_request, reason } => {
                on_action(action_readout(
                    proposal, refreshing_same_request, Some(false),
                ));
                on_diagnostic(RoundDiagnostic::Line(match reason {
                    TrialDeferral::EncoderCleanup =>
                        "abr: quality exploration deferred until PMS confirms prior encoder cleanup",
                    TrialDeferral::PlaybackClock =>
                        "abr: quality transaction deferred until playback clock is running and recovery completes",
                }.to_string()));
                RoundCourse::Stay
            }
            RoundAction::Trial { proposal, reserve_policy } => {
                let start = match self.begin_candidate(
                    proposal, port, expected_user_sequence,
                    || now_ms(),
                    |proposal, refreshing| on_action(action_readout(
                        proposal, refreshing, Some(false),
                    )),
                ) {
                    Ok(start) => start,
                    Err(refusal) => {
                        let line = match refusal {
                            CandidateStartRefusal::Retired => return RoundCourse::Aborted,
                            CandidateStartRefusal::ReserveUnknown =>
                                "abr: candidate reserve unreadable before exploration; staying on current rung".to_string(),
                            CandidateStartRefusal::NoRunway =>
                                "abr: candidate has no completed current acquisition to preserve; staying".to_string(),
                            CandidateStartRefusal::NoSurplus =>
                                "abr: candidate has no reserve above rollback boundary; staying on current rung".to_string(),
                            CandidateStartRefusal::BudgetMoved { budget_ms, target_kbps } => format!(
                                "abr: actual exploration budget {}ms no longer releases {}kbps; staying on current rung",
                                budget_ms, target_kbps,
                            ),
                            CandidateStartRefusal::LeaseMoved =>
                                "abr: recovery transaction superseded before its actuator lease could arm".to_string(),
                            CandidateStartRefusal::TrialReserveMoved =>
                                "abr: clock transition superseded exploration before its trial could arm".to_string(),
                        };
                        on_diagnostic(RoundDiagnostic::Line(line));
                        return RoundCourse::Stay;
                    }
                };
                RoundCourse::Candidate { proposal, reserve_policy, sample, start }
            }
        }
    }

    pub(crate) fn next_round_action(
        &mut self, round: &RoundDecision, sample: abr::SegmentSample,
        rollback_media_ms: Option<u32>, remaining_ms: i64,
        now_ms: impl FnOnce() -> u64, paused: bool, cleanup_ready: bool,
        permission: impl FnOnce(AutomaticTransition) -> bool,
        rebuffer: impl FnOnce() -> (bool, bool),
    ) -> RoundAction {
        let automatic_permitted = match round.decision {
            abr::Decision::Stay => true,
            abr::Decision::Prime(proposal) => permission(match proposal.direction {
                abr::Direction::Up => AutomaticTransition::QualityUp,
                abr::Direction::Down => AutomaticTransition::QualityDown,
            }),
        };
        let now_ms = now_ms();
        let (rebuffer_requested, rebuffering) = rebuffer();
        self.next_action(
            round.decision, &round.telemetry, sample, rollback_media_ms,
            remaining_ms, now_ms, paused, cleanup_ready, automatic_permitted,
            rebuffer_requested, rebuffering,
        )
    }

    fn next_action(
        &mut self, decision: abr::Decision, telemetry: &abr::ControllerTelemetry,
        sample: abr::SegmentSample, rollback_media_ms: Option<u32>, remaining_ms: i64,
        now_ms: u64, paused: bool, cleanup_ready: bool,
        automatic_permitted: bool, rebuffer_requested: bool, rebuffering: bool,
    ) -> RoundAction {
        match decision {
            abr::Decision::Stay if !paused => {
                match self.probe_due(telemetry, sample, rollback_media_ms, remaining_ms, now_ms) {
                    Some(ProbeDecision {
                        result: Err(reason), current_wire_kbps, conservative_kbps,
                    }) => RoundAction::ProbeBlocked(ProbeBlockSummary {
                        reason, current_wire_kbps, conservative_kbps,
                    }),
                    Some(ProbeDecision { result: Ok(_), .. }) if !cleanup_ready => {
                        RoundAction::ProbeDeferredForCleanup
                    }
                    Some(ProbeDecision { result: Ok(permit), .. }) => RoundAction::Probe(permit),
                    None => RoundAction::Stay,
                }
            }
            abr::Decision::Stay => RoundAction::Stay,
            abr::Decision::Prime(proposal) => {
                // A higher-priority source latch can cancel this proposal while
                // its route request is Busy. The completed quantum's decision is
                // then stale and cannot open a new encoder.
                if self.controller.pending_reserve_policy(proposal).is_none() {
                    return RoundAction::Stay;
                }
                if proposal.direction == abr::Direction::Up && !cleanup_ready {
                    let refreshing_same_request = self.reject(
                        proposal, abr::RejectCause::Circumstance, now_ms,
                    );
                    return RoundAction::TrialDeferred {
                        proposal, refreshing_same_request,
                        reason: TrialDeferral::EncoderCleanup,
                    };
                }
                if !automatic_permitted || !hls_quality_trial_may_start(
                    proposal.direction, rebuffer_requested, rebuffering, paused,
                ) {
                    let refreshing_same_request = self.reject(
                        proposal, abr::RejectCause::Circumstance, now_ms,
                    );
                    return RoundAction::TrialDeferred {
                        proposal, refreshing_same_request,
                        reason: TrialDeferral::PlaybackClock,
                    };
                }
                RoundAction::Trial {
                    proposal, reserve_policy: self.pending_reserve_policy(proposal),
                }
            }
        }
    }
    pub(crate) fn probe_failed(&mut self) -> Option<ProbeGrade> {
        let delivery = self.controller.delivery();
        let gate = self.recovery.as_mut()?;
        let verdict = gate.observe_probe_failed(&delivery);
        Some(ProbeGrade {
            verdict, comparison: gate.comparison(), basis: gate.basis(), probes: gate.probes(),
        })
    }
    pub(crate) fn grade_probe(
        &mut self, probe: abr::CapacityObservation,
        telemetry: &abr::ControllerTelemetry, remaining_ms: i64,
    ) -> Option<ProbeGrade> {
        let current = self.controller.catalog().candidate(self.controller.current());
        let delivery = self.controller.delivery();
        let gate = self.recovery.as_mut()?;
        let verdict = gate.observe_probe(
            probe, current, &telemetry.production, self.controller.buffer(),
            &delivery, remaining_ms,
        );
        Some(ProbeGrade {
            verdict, comparison: gate.comparison(), basis: gate.basis(), probes: gate.probes(),
        })
    }
    /// A measured Original verdict gets a fresh pause snapshot only when it
    /// actually requests recovery. A user hold retains the result; a completed
    /// hidden pause cycle ages the controller before considering publication.
    pub(crate) fn settle_probe_recovery(
        &mut self, grade: Option<&ProbeGrade>, probe_kbps: u32,
        pause: impl FnOnce() -> PauseFact, decision: abr::Decision,
        cleanup_ready: bool, buffered_ms: Option<i64>, media_duration_ms: u32,
    ) -> ProbeRecoveryAction {
        if !grade.is_some_and(|grade| grade.verdict == abr::RecoveryVerdict::Recover) {
            return ProbeRecoveryAction::Steady;
        }
        let measured_kbps = probe_kbps.max(1);
        let pause = pause();
        if let Some(elapsed_ms) = pause.completed_ms {
            self.on_resume(elapsed_ms);
        }
        if pause.paused {
            self.retain_original_recovery(measured_kbps);
            return ProbeRecoveryAction::Held;
        }
        match self.source_switch(
            Some(measured_kbps), decision, cleanup_ready,
            buffered_ms, media_duration_ms,
        ) {
            LatchedOriginalRecovery::Switch { kbps, cancel } => {
                debug_assert!(cancel.is_none(), "a source probe is issued only from Stay");
                ProbeRecoveryAction::Switch { kbps, measured_kbps }
            }
            LatchedOriginalRecovery::Wait => {
                self.retain_original_recovery(measured_kbps);
                ProbeRecoveryAction::Retained
            }
        }
    }
    pub(crate) fn probe_original(
        &self, plan: abr::SourceProbePlan, cancelled: impl FnOnce() -> bool,
    ) -> ProbeResult {
        match self.control.probe_original_while_hls_cancellable(&self.ticket, plan, cancelled) {
            route::OriginalProbeResult::Measured(sample) => ProbeResult::Measured(sample),
            route::OriginalProbeResult::Failed { outcome, failure } => ProbeResult::Failed {
                outcome: outcome.code(), failure: failure.into(),
            },
            route::OriginalProbeResult::Stale => ProbeResult::Stale,
        }
    }
    pub(crate) fn publish_candidate(
        &mut self, port: &mut dyn PublicationGate, candidate: &route::PrimedHls,
        proposal: abr::Proposal, sample: abr::SegmentSample,
        variant: abr::ObservedHlsVariant, now_ms: u64,
    ) -> CandidatePublication {
        publish_candidate(port, &self.control, &mut self.controller, &mut self.ticket,
            candidate, proposal, sample, variant, now_ms)
    }
    /// Queue completion is the only entry into the AQ publication fence. A
    /// failed fence makes the trial terminal; neither its cursor nor its
    /// controller evidence can be reused for another publication attempt.
    pub(crate) fn publish_queued_candidate(
        &mut self, protocol: &mut TrialProtocol, port: &mut dyn PublicationGate,
        candidate: &route::PrimedHls, proposal: abr::Proposal,
        sample: abr::SegmentSample, variant: abr::ObservedHlsVariant, now_ms: u64,
    ) -> CandidatePublication {
        assert_eq!(protocol.phase(), TrialPhase::Queued, "publication requires queued media");
        let publication = self.publish_candidate(
            port, candidate, proposal, sample, variant, now_ms,
        );
        let observation = if matches!(&publication, CandidatePublication::Active { .. }) {
            TrialObservation::Published
        } else {
            TrialObservation::Ended
        };
        let trial = protocol.id();
        let result = protocol.observe(trial, observation);
        if matches!(&publication, CandidatePublication::Active { .. }) {
            assert_eq!(result, Ok(TrialPhase::Published));
        } else {
            assert_eq!(result, Err(TrialProtocolError::TerminalAfterQueue));
        }
        publication
    }
    pub(crate) fn publish_original(
        &self, port: &mut dyn PublicationGate, authorized: bool,
        kbps: u32, position_ns: impl FnOnce() -> Option<i64>,
        on_published: impl FnOnce(),
    ) -> OriginalPublication {
        publish_original(port, authorized, &self.control, &self.ticket,
            kbps, position_ns, on_published)
    }

    pub(crate) fn publish_probed_original(
        &mut self, port: &mut dyn PublicationGate, authorized: bool,
        kbps: u32, measured_kbps: u32,
        position_ns: impl FnOnce() -> Option<i64>, on_published: impl FnOnce(),
    ) -> OriginalPublication {
        let publication = self.publish_original(
            port, authorized, kbps, position_ns, on_published,
        );
        if matches!(publication, OriginalPublication::Busy | OriginalPublication::ClockMoved) {
            self.retain_original_recovery(measured_kbps);
        }
        publication
    }

    pub(crate) fn advance_to(&mut self, now_ms: u64) {
        if let Some(gate) = self.recovery.as_mut() { gate.advance_to(now_ms); }
    }

    pub(crate) fn observe_active_variant(
        &mut self, variant: abr::ObservedHlsVariant, evidence_kbps: u32,
    ) {
        self.controller.observe_active_variant(variant, evidence_kbps);
        self.control.observe_active_variant(&self.ticket, variant, evidence_kbps);
    }

    /// The route's cleanup runner is sampled once per completed active quantum.
    pub(crate) fn encoder_cleanup_ready(&self, completed_active: bool) -> bool {
        if completed_active {
            self.control.observe_encoder_cleanup()
        } else {
            self.control.encoder_cleanup_ready()
        }
    }

    /// The first active-boundary step runs before the rollback playlist lookup.
    /// That lookup may block, so cleanup and variant evidence use this earlier
    /// measurement instant exactly as they did in the feed loop.
    pub(crate) fn begin_active_quantum(
        &mut self, acquisition: Acquisition, buffer: impl FnOnce() -> BufferFact,
        abandoned: bool, delivered: bool, now_ms: u64,
    ) -> Option<ActiveQuantumResult> {
        self.advance_to(now_ms);
        let quantum = ActiveQuantum {
            acquisition, buffer: buffer(), abandoned, delivered,
        };
        let sample = active_sample(
            quantum.acquisition, quantum.buffer, quantum.abandoned,
        )?;
        if quantum.delivered {
            if let Some(variant) = abr::ObservedHlsVariant::new(
                quantum.acquisition.declared_bps,
                quantum.acquisition.width,
                quantum.acquisition.height,
            ) {
                self.observe_active_variant(variant, sample.network_kbps());
            }
        }
        let cleanup_ready = self.encoder_cleanup_ready(
            quantum.delivered && sample.completed(),
        );
        Some(ActiveQuantumResult {
            sample, cleanup_ready,
            delivered: quantum.delivered, abandoned: quantum.abandoned,
        })
    }


    pub(crate) fn on_resume(&mut self, elapsed_ms: u64) {
        self.controller.on_resume(elapsed_ms);
    }

    /// The pure controller step: the caller supplies the completed acquisition,
    /// current rollback obligation, and its monotonic time. No clock or registry
    /// is read here, and candidate media is never counted as active evidence.
    fn observe(
        &mut self, sample: abr::SegmentSample, rollback_media_ms: Option<u32>,
        now_ms: u64, session_boundary: bool,
    ) -> RoundDecision {
        let decision = if session_boundary {
            self.controller.observe_session_boundary(sample, rollback_media_ms, now_ms)
        } else {
            self.controller.observe_with_rollback(sample, rollback_media_ms, now_ms)
        };
        RoundDecision {
            decision,
            telemetry: self.controller.telemetry(),
            prime_runway_ms: self.controller.prime_runway_ms(),
        }
    }

    pub(crate) fn prime(
        &self,
        off: &plx_base::task::OffFrame,
        proposal: abr::Proposal,
        offset_micros: i64,
        deadline: Option<std::time::Instant>,
    ) -> Result<route::PrimedHls, PrimeFailure> {
        prime(&self.control, &self.ticket, off, proposal, offset_micros, deadline)
    }
}

struct Session {
    start: Option<Start>,
    initial_kbps: u32,
    publisher: AbrPublisher,
    coordinator: Option<Coordinator>,
    /// Minted on the worker after the initial cursor opens, at the same
    /// boundary as the former in-loop controller construction.
    history_since: Option<std::time::Instant>,
    /// The first completed object carries encoder startup and does not certify
    /// repeatable acquisition; the next object opens that evidence window.
    session_boundary_pending: bool,
    /// PMS cleanup can finish after its stop ACK; log one wait/release pair.
    cleanup_wait_logged: bool,
}

/// Capture the one route owner for transfer into the existing feed worker.
/// Construction reads no clock, dev trigger or player state; those reads stay
/// at the first worker drive after the initial HLS cursor is open.
pub fn boxed_session(
    start: Start, effects: Box<dyn AdaptiveEffects>,
) -> Box<dyn AdaptiveSession> {
    let initial_kbps = start.info().initial.kbps();
    Box::new(Session {
        start: Some(start), initial_kbps, publisher: AbrPublisher::new(effects),
        coordinator: None, history_since: None,
        session_boundary_pending: true, cleanup_wait_logged: false,
    })
}

impl Session {
    fn initialize_on_worker(&mut self) {
        if self.coordinator.is_some() { return; }
        let history_since = std::time::Instant::now();
        let start = self.start.take().expect("an uninitialized session owns its start");
        let info = start.info();
        crate::player::log(&format!(
            "abr: history switches={} since_last={} advanced=0ms",
            info.history.visible_switches,
            info.history.since_last_ms.map(|ms| ms.to_string())
                .unwrap_or_else(|| "none".to_string()),
        ));
        if !info.original_candidate || info.original_source_kbps == 0 {
            crate::player::log(&format!(
                "abr: no Original recovery for this playback (candidate={} source={}kbps) — \
                 HLS is the ceiling until the next resolve",
                u8::from(info.original_candidate), info.original_source_kbps,
            ));
        }
        let pin = abr::abr_pin();
        let coordinator = Coordinator::start(start, pin);
        if info.original_refused && coordinator.has_recovery() {
            crate::player::log(
                "abr: Original was refused on this HLS route; not asked again until the route changes",
            );
        }
        let seeded = coordinator.telemetry();
        crate::player::log(&format!(
            "abr: seed rung={}kbps prior={} slow={}kbps fast={}kbps unc={}pm n={} pin={}",
            info.initial.kbps(),
            info.prior.map(|p| format!("{}kbps", p.slow_kbps))
                .unwrap_or_else(|| "none".to_string()),
            seeded.delivery.slow_kbps,
            seeded.delivery.fast_kbps,
            seeded.delivery.uncertainty_pm,
            seeded.delivery.samples,
            pin.map(|r| format!("{}kbps", r.kbps()))
                .unwrap_or_else(|| "none".to_string()),
        ));
        self.history_since = Some(history_since);
        self.coordinator = Some(coordinator);
    }
}

impl AdaptiveSession for Session {
    fn initial_kbps(&self) -> u32 { self.initial_kbps }

    fn drive_round(
        &mut self, off: &plx_base::task::OffFrame,
        physical: &mut dyn AdaptivePhysicalPort,
    ) -> AdaptiveProgress {
        self.initialize_on_worker();
        let history_since = self.history_since.as_ref()
            .expect("initialized session owns its clock");
        let coordinator = self.coordinator.as_mut()
            .expect("initialized session owns its controller");
        let now_ms = || history_since.elapsed().as_millis() as u64;
        let Some(round_id) = coordinator.next_round_id() else {
            return AdaptiveProgress::Failed("adaptive active round generation exhausted");
        };
        physical.begin_round(round_id);
        let publisher = &self.publisher;
        let mut diagnostics = PlayerActiveDiagnostics {
            cleanup_wait_logged: &mut self.cleanup_wait_logged,
            publisher,
        };
        let remaining_playback_ms = physical.remaining_playback_sampler();
        let user_sequence = physical.user_sequence_sampler();
        let cycle = match run_active_round(
            &mut *coordinator, physical, round_id, || now_ms(),
            || remaining_playback_ms(), || user_sequence(),
            &mut self.session_boundary_pending, &mut diagnostics,
        ) {
            Ok(ActiveRun::Ended) => return AdaptiveProgress::Ended,
            Ok(ActiveRun::InvalidSample) => {
                crate::player::log("abr: ignoring invalid segment timing sample");
                return AdaptiveProgress::Continue;
            }
            Ok(ActiveRun::Observed(cycle)) => cycle,
            Err(ActiveRunError::Physical(error)) => return AdaptiveProgress::PhysicalFailure(error),
            Err(ActiveRunError::Protocol(message)) => return AdaptiveProgress::Failed(message),
        };
        let Some(current) = physical.finish_active() else {
            return AdaptiveProgress::Failed("adaptive active round lost physical media");
        };
        let (proposal, reserve_policy, sample, start) = match coordinator.advance_round(
            cycle, physical, || now_ms(),
            || publisher.recover_original(),
            |action| publisher.action(action),
            |diagnostic| match diagnostic {
                RoundDiagnostic::Line(line) => crate::player::log(&line),
                RoundDiagnostic::Steady(line) => {
                    publisher.steady();
                    crate::player::log(&line);
                }
            },
            &mut PlayerOriginalProbeDiagnostics { publisher },
        ) {
            RoundCourse::Stay => return AdaptiveProgress::Continue,
            RoundCourse::OriginalPublished => return AdaptiveProgress::OriginalPublished,
            RoundCourse::Aborted => return AdaptiveProgress::Aborted,
            RoundCourse::Candidate { proposal, reserve_policy, sample, start } =>
                (proposal, reserve_policy, sample, start),
        };
        let mut trial_reserve = start.trial_reserve;
        let _automatic_lease = start.automatic_lease;
        let automatic_token = start.token;
        let refreshing = proposal.rung == coordinator.current();
        let Some(mut trial_protocol) = coordinator.begin_trial_protocol() else {
            return AdaptiveProgress::Failed("adaptive trial generation exhausted");
        };
        let trial_id = trial_protocol.id();
        physical.begin_trial(trial_id, start.exploration.map(|clock| {
            plx_playback_contract::adaptive::ExplorationFact {
                target_kbps: proposal.rung.kbps(),
                started_ns: clock.started_ns,
                buffered_ms: clock.buffered_ms,
                runway_ms: clock.runway_ms,
                budget_ms: clock.budget_ms,
            }
        }));
        let mut tx = TxTrace::open(
            proposal, coordinator.current(), sample, &coordinator.delivery(),
            physical.buffer_sampler(), crate::player::log,
        );
        publisher.action(action_readout(proposal, refreshing, None));
        let Some(offset_micros) = physical.candidate_offset_micros() else {
            return AdaptiveProgress::Failed("HLS candidate content offset overflow");
        };
        let mut runner = TrialRunner::new(physical, &mut trial_protocol);
        let first = runner.run_first(
            &mut *coordinator, off, proposal, reserve_policy, offset_micros, || now_ms(),
            |event, physical| match event {
                TrialTrace::Prime => tx.mark_prime(),
                TrialTrace::Master(declared_bps) => {
                    assert!(physical.candidate_declared_bps().is_some(),
                        "an opened trial owns its cursor");
                    tx.mark_master(declared_bps);
                }
                TrialTrace::ControlPlane => tx.mark_control_plane(),
                TrialTrace::WarmupDeadline(deadline) => tx.mark_warmup_deadline(deadline),
                TrialTrace::FirstMedia(acquisition) =>
                    tx.mark_media(acquisition.bytes, acquisition.total_us, false),
                TrialTrace::Decided => tx.mark_decided(),
                TrialTrace::Rejected { refreshing } =>
                    publisher.action(action_readout(proposal, refreshing, Some(false))),
                TrialTrace::Finish(outcome) => tx.finish(outcome),
                TrialTrace::Log(line) => crate::player::log(&line),
            },
        );
        let (primed, declared_bps, first_acquisition, first_buffer) = match first {
            FirstPhase::Ready { primed, declared_bps, acquisition, buffer } =>
                (primed, declared_bps, acquisition, buffer),
            FirstPhase::Stay => return AdaptiveProgress::Continue,
            FirstPhase::Aborted => return AdaptiveProgress::Aborted,
            FirstPhase::Failed(message) => return AdaptiveProgress::Failed(message),
        };
        let current_variant = VariantFacts {
            width: current.width, height: current.height, declared_bps: current.declared_bps,
        };
        let staged = runner.grade_staged(
            &mut *coordinator, proposal, reserve_policy, current_variant,
            first_acquisition, first_buffer, declared_bps,
            |remaining_ms| crate::player::log(&format!(
                "abr: setup-bearing candidate retains one staged observation budget={}ms",
                remaining_ms.unwrap_or(0),
            )),
            |moment| match moment {
                TrialTraceMoment::FollowupMedia(acquisition) =>
                    tx.mark_media(acquisition.bytes, acquisition.total_us, true),
                TrialTraceMoment::FinalEvidence(acquisition) => tx.mark_candidate_evidence(
                    acquisition.bytes, acquisition.total_us, acquisition.duration,
                ),
            },
        );
        let judgment = runner.settle_staged(
            &mut *coordinator, &primed, proposal, staged, current_variant,
            declared_bps, || now_ms(),
            |event, _| match event {
                TrialTrace::Decided => tx.mark_decided(),
                TrialTrace::Rejected { refreshing } =>
                    publisher.action(action_readout(proposal, refreshing, Some(false))),
                TrialTrace::Finish(outcome) => tx.finish(outcome),
                TrialTrace::Log(line) => crate::player::log(&line),
                _ => unreachable!("settling staged media cannot emit acquisition timing"),
            },
        );
        let (final_acquisition, sample, candidate_variant) = match judgment {
            CandidateJudgment::Queue { final_acquisition, sample, variant } =>
                (final_acquisition, sample, variant),
            CandidateJudgment::Stay => return AdaptiveProgress::Continue,
            CandidateJudgment::Aborted => return AdaptiveProgress::Aborted,
            CandidateJudgment::Failed(message) => return AdaptiveProgress::Failed(message),
        };
        let queued = runner.queue_publish(
            &mut *coordinator, automatic_token, &primed, proposal, sample,
            candidate_variant, || now_ms(),
            |event| match event {
                QueueTrace::Decided => tx.mark_decided(),
                QueueTrace::FeedCompleted(started) => tx.mark_feed(started),
                QueueTrace::Finish(outcome) => tx.finish(outcome),
            },
        );
        let (previous, fence) = match queued.into_publication() {
            Ok(published) => published,
            Err(progress) => return progress,
        };
        drop(runner);
        if let Some(runway_ms) = coordinator.prime_runway_ms() {
            physical.note_prime_runway(runway_ms);
        }
        tx.finish("committed");
        let candidate_declared_bps = physical.candidate_declared_bps()
            .expect("published trial retains its candidate cursor");
        let committed = coordinator.commit_readout(
            proposal, refreshing, candidate_declared_bps, final_acquisition,
        );
        publisher.action(ActionReadout {
            direction: committed.direction, target_kbps: committed.target_kbps,
            refreshing: committed.refreshing, committed: Some(true),
        });
        // Called after the AQ publication and native gauge projection. The sink
        // receives the generation captured with this route, never a global lookup.
        publisher.committed(committed);
        assert!(
            physical.promote_candidate(&committed),
            "published trial promotes its candidate cursor",
        );
        coordinator.on_hls_commit(proposal.direction);
        physical.settle(fence);
        drop(trial_reserve.take());
        coordinator.retire(previous);
        crate::player::log(&format!(
            "abr: committed {:?} to {}kbps {}x{} out={}x{}",
            proposal.direction, committed.target_kbps,
            proposal.rung.raster().0, proposal.rung.raster().1,
            committed.observed_width, committed.observed_height,
        ));
        AdaptiveProgress::Continue
    }
}

/// A recovery downshift restores the picture; an upshift only explores quality.  Starting the
/// latter while the native clock is already held makes its private transaction latency part of
/// the visible rebuffer even though the recovery media is already in hand.  Order those two
/// operations by what they do: finish the active recovery edge first, then let ordinary measured
/// exploration run.  No duration or reserve threshold is involved.
pub(crate) fn hls_quality_trial_may_start(
    direction: abr::Direction,
    rebuffer_requested: bool,
    rebuffering: bool,
    user_paused: bool,
) -> bool {
    !user_paused
        && (direction == abr::Direction::Down || !(rebuffer_requested || rebuffering))
}

/// A source-mode recovery verdict is a higher-level actuator decision than an HLS rung proposal.
/// Once latched, it takes the next quiescent media boundary regardless of whether the rung
/// controller happened to answer `Stay` or `Prime` there. Returning the pending proposal makes
/// its cancellation explicit; otherwise it would survive into a source reload or, worse, let a
/// stale Original verdict cross a successful change of HLS operating point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LatchedOriginalRecovery {
    Wait,
    Switch {
        kbps: u32,
        cancel: Option<abr::Proposal>,
    },
}

pub(crate) fn latched_original_recovery(
    recover_kbps: u32,
    fresh_probe_kbps: Option<u32>,
    decision: abr::Decision,
    encoder_cleanup_ready: bool,
    buffered_ms: Option<i64>,
    media_duration_ms: u32,
) -> LatchedOriginalRecovery {
    // A fresh successful source observation is already sitting on a completed/fed HLS boundary.
    // It must be eligible on THIS call, not copied into a local latch which can only be consumed
    // after another HLS GET.  PMS may rebind the shared Streaming Resource to the raw Part as a
    // side effect of that probe; requiring one more old-session segment then makes the transition
    // depend on a resource the probe itself may have invalidated.
    let recover_kbps = fresh_probe_kbps.unwrap_or(recover_kbps);
    if recover_kbps == 0
        || !encoder_cleanup_ready
        || !buffered_ms.is_some_and(|ms| ms >= i64::from(media_duration_ms))
    {
        return LatchedOriginalRecovery::Wait;
    }
    LatchedOriginalRecovery::Switch {
        kbps: recover_kbps,
        cancel: match decision {
            abr::Decision::Stay => None,
            abr::Decision::Prime(proposal) => Some(proposal),
        },
    }
}


/// Classify a completed candidate from what its RESPONSE proved, never from the request label.
/// A no-gain response is PMS underfill: even when the request actuator differs, its smaller output
/// does not order that actuator against lower request ceilings. Treating it as structural walked
/// the whole ladder and allocated one physical encoder per rung on the remote PMS.
pub(crate) fn abr_reject_for_candidate_result(
    raster_ready: bool,
    quality_improves: bool,
    verdict: abr::CandidateVerdict,
) -> abr::RejectCause {
    if !raster_ready {
        return abr::RejectCause::StructuralAtOrBelow;
    }
    if !quality_improves {
        return abr::RejectCause::ResponseUnchanged;
    }
    match verdict {
        // A setup-bearing verdict is consumed by the serial continuation path before rejection.
        // If a future caller leaks it here, retain only an actuator-specific funding frontier;
        // it is explicitly not steady `A>D` evidence.
        abr::CandidateVerdict::SetupBearing => abr::RejectCause::Candidate,
        abr::CandidateVerdict::Unsustainable => {
            abr::RejectCause::CompletedUnsustainable
        }
        abr::CandidateVerdict::Unfunded => abr::RejectCause::Candidate,
        abr::CandidateVerdict::Incomplete | abr::CandidateVerdict::ReserveUnknown => {
            abr::RejectCause::Circumstance
        }
        abr::CandidateVerdict::Ready => {
            unreachable!("a ready candidate can only be rejected by response geometry")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsCandidateDisposition {
    Commit,
    Discard {
        cause: abr::RejectCause,
        outcome: &'static str,
    },
}

pub(crate) fn hls_candidate_requires_rung_box(
    proposal: abr::Proposal,
    reserve_policy: abr::ReservePolicy,
) -> bool {
    !(proposal.direction == abr::Direction::Down
        && proposal.rung.at_floor()
        && reserve_policy == abr::ReservePolicy::TerminalFloor)
}

pub(crate) fn hls_candidate_disposition(
    raster_ready: bool,
    quality_improves: bool,
    verdict: abr::CandidateVerdict,
) -> HlsCandidateDisposition {
    if raster_ready && quality_improves && verdict == abr::CandidateVerdict::Ready {
        HlsCandidateDisposition::Commit
    } else {
        let outcome = if !raster_ready {
            "raster_refused"
        } else if !quality_improves {
            "no_quality_gain"
        } else {
            "not_ready_discarded"
        };
        HlsCandidateDisposition::Discard {
            cause: abr_reject_for_candidate_result(raster_ready, quality_improves, verdict),
            outcome,
        }
    }
}


pub(crate) fn hls_raster_within(width: i32, height: i32, rung: abr::Rung) -> bool {
    let (max_width, max_height) = rung.raster();
    width > 0 && height > 0 && width <= i32::from(max_width) && height <= i32::from(max_height)
}

/// A larger request is a quality transition only when its OBSERVED output strictly dominates the
/// stream it would replace.  Raster and the master playlist's declared bandwidth are independent
/// axes: more bits at the same raster can improve an encode, and more pixels at a lower declared
/// rate is not orderable without inventing a subjective weight.  Pareto dominance is therefore
/// the strongest statement the measurements alone support — no resolution target, ratio or
/// tolerance is introduced.
pub(crate) fn hls_upshift_strictly_improves(
    current_width: i32,
    current_height: i32,
    current_declared_bps: u64,
    candidate_width: i32,
    candidate_height: i32,
    candidate_declared_bps: u64,
) -> bool {
    let Some(current) =
        abr::ObservedHlsVariant::new(current_declared_bps, current_width, current_height)
    else {
        return false;
    };
    abr::ObservedHlsVariant::new(candidate_declared_bps, candidate_width, candidate_height)
        .is_some_and(|candidate| candidate.strictly_dominates(current))
}

#[cfg(test)]
mod trial_runner_tests {
    use super::*;
    use plx_playback_contract::adaptive::TrialId;
    use plx_playback_contract::AttemptId;

    struct LateReplyPort { response: FeedObservation }

    impl FeedPort for LateReplyPort {
        type Locator = ();

        fn bind_source(&mut self, _source: SourceKey, _locator: ()) {}

        fn step(&mut self, _action: FeedAction) -> FeedObservation { self.response }
    }

    #[test]
    fn late_open_reply_cannot_advance_a_new_trial() {
        let current = TrialId::new(AttemptId::new(3, 8), 12);
        let previous = TrialId::new(AttemptId::new(3, 7), 12);
        let mut protocol = TrialProtocol::new(current);
        protocol.observe(current, TrialObservation::Prepared).unwrap();
        let mut port = LateReplyPort {
            response: FeedObservation::Opened { trial: previous, declared_bps: 2_000_000 },
        };
        let mut runner = TrialRunner::new(&mut port, &mut protocol);
        assert_eq!(runner.open(()), FeedObservation::Failed {
            trial: current, failure: FeedFailure::Protocol,
        });
        drop(runner);
        assert_eq!(protocol.phase(), TrialPhase::Prepared);
    }

    #[test]
    fn late_queue_reply_after_enqueue_stays_terminal() {
        let current = TrialId::new(AttemptId::new(4, 9), 2);
        let previous = TrialId::new(AttemptId::new(4, 8), 2);
        let mut protocol = TrialProtocol::new(current);
        for phase in [
            TrialObservation::Prepared, TrialObservation::FirstAcquired,
        ] {
            protocol.observe(current, phase).unwrap();
        }
        let mut port = LateReplyPort { response: FeedObservation::Queued { trial: previous } };
        let mut runner = TrialRunner::new(&mut port, &mut protocol);
        runner.authorize_queue();
        assert_eq!(runner.queue_staged(), FeedObservation::Failed {
            trial: current, failure: FeedFailure::Protocol,
        });
        drop(runner);
        assert_eq!(protocol.phase(), TrialPhase::Ended);
        assert_eq!(protocol.observe(current, TrialObservation::Published),
            Err(TrialProtocolError::OutOfOrder));
    }

    #[test]
    fn setup_bearing_followup_checks_the_original_reserve_once() {
        struct ReservePort { reply: FeedObservation, checks: usize }
        impl FeedPort for ReservePort {
            type Locator = ();
            fn bind_source(&mut self, _source: SourceKey, _locator: ()) {}
            fn step(&mut self, action: FeedAction) -> FeedObservation {
                assert!(matches!(action, FeedAction::CheckFollowupReserve { .. }));
                self.checks += 1;
                self.reply
            }
        }
        let trial = TrialId::new(AttemptId::new(5, 11), 3);
        let mut protocol = TrialProtocol::new(trial);
        protocol.observe(trial, TrialObservation::Prepared).unwrap();
        protocol.observe(trial, TrialObservation::FirstAcquired).unwrap();
        let mut port = ReservePort {
            reply: FeedObservation::FollowupReserve {
                trial, funded: false, remaining_ms: Some(0),
            },
            checks: 0,
        };
        let mut runner = TrialRunner::new(&mut port, &mut protocol);
        assert!(matches!(runner.after_first_grade(FirstCandidateGrade {
            verdict: abr::CandidateVerdict::Ready, needs_followup: false,
        }), FirstCandidateAction::Finalize { .. }));
        assert_eq!(runner.port.checks, 0);
        assert!(matches!(runner.after_first_grade(FirstCandidateGrade {
            verdict: abr::CandidateVerdict::SetupBearing, needs_followup: true,
        }), FirstCandidateAction::Unfunded));
        assert_eq!(runner.port.checks, 1);
        runner.port.reply = FeedObservation::FollowupReserve {
            trial, funded: true, remaining_ms: Some(750),
        };
        assert!(matches!(runner.after_first_grade(FirstCandidateGrade {
            verdict: abr::CandidateVerdict::SetupBearing, needs_followup: true,
        }), FirstCandidateAction::ReadFollowup { remaining_ms: Some(750), .. }));
        assert_eq!(runner.port.checks, 2);
    }

    #[test]
    fn repeatable_candidate_is_read_and_staged_before_it_can_be_graded() {
        use std::collections::VecDeque;
        struct Port {
            replies: VecDeque<FeedObservation>,
            actions: Vec<FeedAction>,
        }
        impl FeedPort for Port {
            type Locator = ();
            fn bind_source(&mut self, _source: SourceKey, _locator: ()) {}
            fn step(&mut self, action: FeedAction) -> FeedObservation {
                self.actions.push(action);
                self.replies.pop_front().expect("one reply per physical step")
            }
        }
        let trial = TrialId::new(AttemptId::new(8, 13), 4);
        let mut protocol = TrialProtocol::new(trial);
        protocol.observe(trial, TrialObservation::Prepared).unwrap();
        protocol.observe(trial, TrialObservation::FirstAcquired).unwrap();
        let acquisition = Acquisition {
            bytes: 200_000, active_us: 100_000, total_us: 125_000,
            duration: std::time::Duration::from_secs(2),
            width: 1280, height: 720, declared_bps: 2_000_000,
        };
        let buffer = BufferFact {
            playback_ms: 1_000, video_tail_ms: 5_000,
            audio_tail_ms: Some(5_000), audio_expected: true,
        };
        let mut port = Port {
            replies: VecDeque::from([
                FeedObservation::SegmentReady { trial, duration: acquisition.duration },
                FeedObservation::Acquired { trial, acquisition },
                FeedObservation::Staged { trial, buffer, count: 2 },
            ]),
            actions: Vec::new(),
        };
        let mut budgets = Vec::new();
        let mut media_trace = Vec::new();
        let mut runner = TrialRunner::new(&mut port, &mut protocol);
        let result = runner.read_repeatable(
            Some(700), &mut |budget| budgets.push(budget),
            &mut |moment| media_trace.push(moment),
        );
        assert!(matches!(result, Ok((got, fact)) if got == acquisition && fact == buffer));
        drop(runner);
        assert_eq!(budgets, [Some(700)]);
        assert_eq!(port.actions, [
            FeedAction::ReadNext { trial },
            FeedAction::AcquireFollowup { trial },
            FeedAction::StageAcquired { trial },
        ]);
        assert_eq!(media_trace, [TrialTraceMoment::FollowupMedia(acquisition)]);
        assert_eq!(protocol.phase(), TrialPhase::FollowupAcquired);
    }

    #[test]
    fn failure_course_depends_on_trial_stage_not_only_transport_code() {
        use TrialFailureAction as A;
        use TrialRejectReason as R;
        use TrialStep as S;
        assert_eq!(trial_failure_action(S::Open, FeedFailure::SourceMoved),
            A::Reject { cause: abr::RejectCause::Circumstance, reason: R::SourceMoved });
        assert_eq!(trial_failure_action(S::FirstRead, FeedFailure::SourceMoved), A::Abort);
        assert_eq!(trial_failure_action(S::FollowupRead, FeedFailure::Ended),
            A::Reject { cause: abr::RejectCause::Circumstance, reason: R::Ended });
        assert_eq!(trial_failure_action(S::FirstAcquire, FeedFailure::Deadline),
            A::Reject { cause: abr::RejectCause::Censored, reason: R::Deadline });
        assert_eq!(trial_failure_action(S::FollowupAcquire, FeedFailure::TimelineOverflow),
            A::TimelineOverflow);
        assert_eq!(trial_failure_action(S::Open, FeedFailure::Protocol), A::Protocol);
    }
}


/// The worker's report to the player: what crosses the open's channel, what the worker's sink
/// accumulates, and what each publication writes. None of it needs the television.
#[cfg(test)]
mod abr_readout_tests {
    use super::*;
    use plx_playback_contract::readout as code;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn seed(samples: u32) -> SeedReadout {
        SeedReadout { fast_kbps: 42_000, slow_kbps: 40_000, uncertainty_pm: 100, samples }
    }

    fn net(kbps: i64) -> AbrReadout { AbrReadout { net_kbps: kbps, ..AbrReadout::IDLE } }

    /// The teardown joins the adaptive worker while the player's inbox is undrained (the pump is
    /// not running). A blocking `send` on a full bounded channel would park the worker there
    /// forever and the join with it, so the worker has to finish its whole run of publications
    /// with nobody receiving, far past the channel's depth, and the join has to return.
    #[test]
    fn a_worker_whose_inbox_is_never_drained_still_finishes() {
        let (tx, inbox) = readout_channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = plx_base::task::spawn("abr-readout-test", move || {
            let mut sink = PlayerAdaptiveEffects::new(tx);
            for i in 0..(READOUT_QUEUE_DEPTH as i64 * 40) {
                sink.readout(net(i));
                sink.seed(seed(1 + i as u32));
            }
            let _ = done_tx.send(());
        }).expect("a thread for the test worker");
        assert!(done_rx.recv_timeout(Duration::from_secs(20)).is_ok(),
            "the worker blocked on a full channel nobody was draining");
        worker.join().expect("the worker did not panic");
        drop(inbox);
    }

    /// The stop path drops the inbox with the worker still publishing: a send into a channel with
    /// no receiver is the normal end of the open, not an error and not a panic.
    #[test]
    fn a_worker_publishing_after_the_inbox_is_gone_carries_on() {
        let (tx, inbox) = readout_channel();
        drop(inbox);
        let mut sink = PlayerAdaptiveEffects::new(tx);
        sink.readout(net(5));
        sink.seed(seed(3));
        sink.seed(seed(0));
        sink.readout(AbrReadout::IDLE);
    }

    /// A full channel refuses the new shipment, never the worker. The documented bound: a player
    /// that has not drained since the channel filled reads the state as of the shipment that filled
    /// it, and the worker's next call after the drain carries everything the refused calls changed
    /// (the readout and the seed), whichever member that call is about.
    #[test]
    fn a_full_channel_costs_staleness_bounded_by_the_next_call_not_the_worker() {
        let (tx, inbox) = readout_channel();
        let mut sink = PlayerAdaptiveEffects::new(tx);
        for i in 1..=READOUT_QUEUE_DEPTH as i64 { sink.readout(net(i)); }
        // Past the depth: refused, and the worker is back at once.
        sink.readout(AbrReadout { action: code::ABR_ACTION_COMMIT_UP, ..net(100) });
        sink.seed(seed(19));
        let stale = inbox.newest().expect("the filled queue");
        assert_eq!(stale.readout.net_kbps, READOUT_QUEUE_DEPTH as i64,
            "the player ends on the shipment that filled the channel");
        assert_eq!(stale.seed.map(|(s, _)| s.samples), None);
        assert!(inbox.newest().is_none(), "a drain empties the queue");
        // Room again: the next call, about a different member, carries what the refused ones changed.
        sink.seed(seed(21));
        let fresh = inbox.newest().expect("room after the drain");
        assert_eq!(fresh.readout.net_kbps, 100);
        assert_eq!(fresh.readout.action, code::ABR_ACTION_COMMIT_UP);
        assert_eq!(fresh.seed.map(|(s, _)| s.samples), Some(21));
    }

    /// The pump keeps the last of what it drains, and an inbox with nothing new answers `None`.
    #[test]
    fn the_inbox_answers_the_newest_shipment_and_none_when_idle() {
        let (tx, inbox) = readout_channel();
        assert!(inbox.newest().is_none());
        let mut sink = PlayerAdaptiveEffects::new(tx);
        sink.readout(net(1));
        sink.readout(net(2));
        sink.readout(net(3));
        assert_eq!(inbox.newest().map(|s| s.readout.net_kbps), Some(3));
        assert!(inbox.newest().is_none());
    }

    /// The carried link estimate crosses with the instant the worker observed it; an estimate with
    /// no sample is no estimate and clears the carried one.
    #[test]
    fn the_seed_crosses_with_its_observed_instant_and_zero_samples_clears_it() {
        let (tx, inbox) = readout_channel();
        let mut sink = PlayerAdaptiveEffects::new(tx);
        let before = Instant::now();
        sink.seed(seed(19));
        let shipped = inbox.newest().expect("seed shipment");
        let (carried, at) = shipped.seed.expect("a seed with samples");
        assert_eq!((carried.slow_kbps, carried.fast_kbps, carried.samples), (40_000, 42_000, 19));
        assert!(at >= before && at.elapsed() < Duration::from_secs(10));
        sink.seed(seed(0));
        assert_eq!(inbox.newest().expect("clearing shipment").seed.map(|(s, _)| s.samples), None);
    }

    fn http(status: i32) -> FailureEdit {
        FailureEdit::Noted { kind: crate::player::ABR_FAILURE_ORIGINAL_HTTP, status }
    }

    /// An edit crosses once. Every shipment carries the worker's newest edit (the sink is
    /// cumulative), so a later shipment about another member must not hand it out again: the player
    /// may have cleared the reason since, and a replay would bring it back.
    #[test]
    fn a_failure_edit_crosses_once_though_every_later_shipment_carries_it() {
        let (tx, inbox) = readout_channel();
        let mut sink = PlayerAdaptiveEffects::new(tx);
        sink.failure(http(503));
        assert_eq!(inbox.newest().expect("the edit's shipment").failure, Some((1, http(503))));
        sink.readout(net(7));
        let later = inbox.newest().expect("a shipment about another member");
        assert_eq!(later.readout.net_kbps, 7, "the shipment itself is not lost");
        assert_eq!(later.failure, None, "the edit was handed out already");
        sink.seed(seed(5));
        assert_eq!(inbox.newest().expect("a seed shipment").failure, None);
        sink.failure(FailureEdit::Cleared);
        assert_eq!(inbox.newest().expect("the next edit").failure, Some((2, FailureEdit::Cleared)),
            "a new edit is a new number");
    }

    /// Latest wins within the worker's own sequence because every edit replaces the whole pair: a
    /// note and the clear after it, between two drains, end cleared, and a clear and the note after
    /// it end noted. Nothing is lost to the coalescing of the drain.
    #[test]
    fn a_note_and_a_clear_between_two_drains_end_as_the_later_one() {
        let (tx, inbox) = readout_channel();
        let mut sink = PlayerAdaptiveEffects::new(tx);
        sink.failure(http(503));
        sink.failure(FailureEdit::Cleared);
        assert_eq!(inbox.newest().expect("both edits").failure, Some((2, FailureEdit::Cleared)));
        sink.failure(FailureEdit::Cleared);
        sink.failure(http(404));
        sink.readout(net(1));
        assert_eq!(inbox.newest().expect("the three").failure, Some((4, http(404))),
            "the clear and the note after it end noted, the shipment about another member included");
    }

    /// A full channel refuses the new shipment; the sink keeps the edit, and the next call, whichever
    /// member it changes, carries it.
    #[test]
    fn a_refused_failure_edit_rides_the_next_shipment() {
        let (tx, inbox) = readout_channel();
        let mut sink = PlayerAdaptiveEffects::new(tx);
        for i in 1..=READOUT_QUEUE_DEPTH as i64 { sink.readout(net(i)); }
        sink.failure(http(500));
        let filled = inbox.newest().expect("the filled queue");
        assert_eq!(filled.failure, None, "the edit's own shipment was refused");
        sink.readout(net(99));
        let next = inbox.newest().expect("room after the drain");
        assert_eq!(next.readout.net_kbps, 99);
        assert_eq!(next.failure, Some((1, http(500))), "the refused edit rode the next shipment");
    }

    /// A new open's channel starts empty: its first shipments carry no edit, so applying them
    /// cannot touch a reason the readout kept from the open before (a reload makes a new open).
    #[test]
    fn a_new_opens_first_shipments_carry_no_failure_edit() {
        let (old_tx, old_inbox) = readout_channel();
        let mut old = PlayerAdaptiveEffects::new(old_tx);
        old.failure(http(503));
        assert!(old_inbox.newest().expect("the old open's edit").failure.is_some());
        let (tx, inbox) = readout_channel();
        let mut sink = PlayerAdaptiveEffects::new(tx);
        sink.readout(AbrReadout::IDLE);
        sink.seed(seed(0));
        assert_eq!(inbox.newest().expect("the idle shipment").failure, None);
        sink.failure(http(404));
        assert_eq!(inbox.newest().expect("the new open's own edit").failure, Some((1, http(404))),
            "its numbering is its own");
    }

    /// A recording sink: every failure edit the publisher hands its sink.
    struct EditRecorder(Arc<Mutex<Vec<FailureEdit>>>);
    impl AdaptiveEffects for EditRecorder {
        fn seed(&mut self, _: SeedReadout) {}
        fn committed(&mut self, _: CommitReadout) {}
        fn readout(&mut self, _: AbrReadout) {}
        fn failure(&mut self, edit: FailureEdit) { self.0.lock().expect("recorder").push(edit); }
    }

    /// The probe's two writes of the failure pair, as the worker makes them: a probe's start clears
    /// the reason an earlier one failed (and says it is probing), and each kind of failure notes its
    /// typed reason with the HTTP status only for a status.
    #[test]
    fn the_probe_clears_at_its_start_and_notes_each_kind_of_failure() {
        let edits = Arc::new(Mutex::new(Vec::new()));
        let publisher = AbrPublisher::new(Box::new(EditRecorder(Arc::clone(&edits))));
        let mut probe = PlayerOriginalProbeDiagnostics { publisher: &publisher };
        probe.action(ProbeTraceAction::Start, 8_000);
        probe.failure(ProbeFailure::HttpStatus(503));
        probe.failure(ProbeFailure::Deadline);
        probe.failure(ProbeFailure::Transport);
        probe.failure(ProbeFailure::NoBody);
        probe.failure(ProbeFailure::Other);
        probe.action(ProbeTraceAction::Failed, 0);
        probe.action(ProbeTraceAction::Steady, 0);
        use crate::player::{ABR_FAILURE_ORIGINAL_DEADLINE as DEADLINE, ABR_FAILURE_ORIGINAL_NO_BODY as NO_BODY,
            ABR_FAILURE_ORIGINAL_OPEN as OPEN, ABR_FAILURE_ORIGINAL_TRANSPORT as TRANSPORT};
        assert_eq!(*edits.lock().expect("recorder"), [
            FailureEdit::Cleared,
            http(503),
            FailureEdit::Noted { kind: DEADLINE, status: 0 },
            FailureEdit::Noted { kind: TRANSPORT, status: 0 },
            FailureEdit::Noted { kind: NO_BODY, status: 0 },
            FailureEdit::Noted { kind: OPEN, status: 0 },
        ], "only the start clears and only a failure notes");
    }

    /// A recording sink: every cumulative readout the publisher hands its sink.
    struct Recorder(Arc<Mutex<Vec<AbrReadout>>>);
    impl AdaptiveEffects for Recorder {
        fn seed(&mut self, _: SeedReadout) {}
        fn committed(&mut self, _: CommitReadout) {}
        fn readout(&mut self, readout: AbrReadout) {
            self.0.lock().expect("recorder").push(readout);
        }
        fn failure(&mut self, _: FailureEdit) {}
    }

    fn publisher() -> (AbrPublisher, Arc<Mutex<Vec<AbrReadout>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        (AbrPublisher::new(Box::new(Recorder(Arc::clone(&log)))), log)
    }

    fn last(log: &Arc<Mutex<Vec<AbrReadout>>>) -> AbrReadout {
        *log.lock().expect("recorder").last().expect("a publication")
    }

    /// The action family: one code per (direction, refresh, outcome), as the panel prints them, with
    /// the target beside it; the steady and the Original recovery actions follow the same rule.
    #[test]
    fn the_action_codes_are_the_panels_for_every_direction_and_outcome() {
        use AdaptiveDirection::{Down, Up};
        let (publisher, log) = publisher();
        let table = [
            (Down, false, None, code::ABR_ACTION_PRIME_DOWN),
            (Up, false, None, code::ABR_ACTION_PRIME_UP),
            (Down, false, Some(true), code::ABR_ACTION_COMMIT_DOWN),
            (Up, false, Some(true), code::ABR_ACTION_COMMIT_UP),
            (Down, false, Some(false), code::ABR_ACTION_REJECT_DOWN),
            (Up, false, Some(false), code::ABR_ACTION_REJECT_UP),
            (Down, true, None, code::ABR_ACTION_PRIME_REFRESH),
            (Up, true, Some(true), code::ABR_ACTION_COMMIT_REFRESH),
            (Down, true, Some(false), code::ABR_ACTION_REJECT_REFRESH),
        ];
        for (direction, refreshing, committed, expected) in table {
            publisher.action(ActionReadout { direction, target_kbps: 3_000, refreshing, committed });
            let r = last(&log);
            assert_eq!((r.action, r.target_kbps), (expected, 3_000));
        }
        publisher.recover_original();
        let r = last(&log);
        assert_eq!((r.action, r.target_kbps), (code::ABR_ACTION_RECOVER_ORIGINAL, 3_000), "the target stays");
        publisher.steady();
        let r = last(&log);
        assert_eq!((r.action, r.target_kbps), (code::ABR_ACTION_STEADY, 0), "steady clears the target");
    }

    /// The sample and model families write their own members, the `why` as the panel's code, and
    /// leave every other member as the earlier publications left it.
    #[test]
    fn the_sample_and_model_publications_write_their_members_and_keep_the_rest() {
        let (publisher, log) = publisher();
        publisher.sample(SampleReadout { network_kbps: 12_000, media_kbps: 6_000, ratio_pm: 2_000, buffer_ms: 31_000 });
        let r = last(&log);
        assert_eq!((r.net_kbps, r.media_kbps, r.ratio_pm, r.buffer_ms), (12_000, 6_000, 2_000, 31_000));
        assert_eq!((r.samples, r.why, r.action), (-1, 0, 0), "the model has not spoken");
        publisher.model(&ModelReadout {
            safe_kbps: 9_000, optimal_kbps: 8_000, uncertainty_pm: 150, samples: 12, slope_ms_per_s: -40,
            starvation_secs: 90, production_pm: 1_100, risk: 7, reason: Some(WhyReadout::ResponseLimited),
            seed: seed(12),
        });
        let r = last(&log);
        assert_eq!((r.safe_kbps, r.optimal_kbps, r.unc_pm, r.samples), (9_000, 8_000, 150, 12));
        assert_eq!((r.slope_ms_per_s, r.starve_secs, r.pred_pm, r.risk), (-40, 90, 1_100, 7));
        assert_eq!(r.why, crate::player::ABR_WHY_RESPONSE_LIMITED);
        assert_eq!((r.net_kbps, r.buffer_ms), (12_000, 31_000), "the sample stands");
        publisher.model(&ModelReadout {
            safe_kbps: 1, optimal_kbps: -1, uncertainty_pm: 1, samples: 1, slope_ms_per_s: 0,
            starvation_secs: -1, production_pm: -1, risk: 0, reason: None, seed: seed(1),
        });
        assert_eq!(last(&log).why, crate::player::ABR_WHY_NONE);
    }

    /// The Original watchdog's family: a refreshed ticket restarts the measurements; an observation
    /// writes the window's members, `-1` for an unbounded starvation horizon.
    #[test]
    fn the_original_watchdog_publishes_its_window_and_restarts_it_on_refresh() {
        let (publisher, log) = publisher();
        publisher.original_observed(&abr::OriginalObservation {
            measured_kbps: 20_000, conservative_kbps: 18_000, requirement_kbps: 9_000, buffered_ms: 12_000,
            slope_ms_per_s: -250, horizon_secs: Some(48), unsafe_deficit_ms: 1_500, fallback: None, target: None,
        });
        let r = last(&log);
        assert_eq!((r.net_kbps, r.buffer_ms, r.unsafe_deficit_ms), (20_000, 12_000, 1_500));
        assert_eq!((r.slope_ms_per_s, r.starve_secs, r.safe_kbps), (-250, 48, 18_000));
        publisher.original_observed(&abr::OriginalObservation {
            measured_kbps: 20_000, conservative_kbps: 18_000, requirement_kbps: 9_000, buffered_ms: 12_000,
            slope_ms_per_s: 0, horizon_secs: None, unsafe_deficit_ms: 0, fallback: None, target: None,
        });
        assert_eq!(last(&log).starve_secs, -1, "no horizon reads -1");
        publisher.original_refreshed();
        let r = last(&log);
        assert_eq!((r.net_kbps, r.buffer_ms, r.unsafe_deficit_ms), (-1, -1, 0));
        assert_eq!(r.safe_kbps, 18_000, "a refresh restarts the measurements, not the estimate");
    }
}

/// What the diagnostics line prints for the Auto-quality members, now that the demux's half rides
/// the sample and the worker's half is the readout's photograph: the same text and values the
/// shared mirror held. `diag()` reaches the host stand-ins for the TV's seam, so these run in the
/// `hostsim` pass.
#[cfg(all(test, feature = "hostsim"))]
mod abr_diag_tests {
    use super::*;
    use crate::player::attempt_view::Backend;
    use crate::player::readout::ReadoutEvent;
    use crate::route::{PlaybackSession, RouteStartAttempt};
    use plx_playback_contract::feed::{DemuxAbr, FeedFacts};
    use plx_playback_contract::readout as code;

    fn session(demux: DemuxAbr, photograph: AbrReadout) -> (PlaybackSession, RouteStartAttempt) {
        let attempt = RouteStartAttempt::fixture();
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(attempt, Backend::Feed);
        let mut facts = FeedFacts::default();
        facts.raw.abr = demux;
        assert!(ps.ingest_feed_facts(attempt, facts));
        assert!(ps.ingest_abr(attempt, photograph, None));
        (ps, attempt)
    }

    fn full() -> AbrReadout {
        AbrReadout {
            media_kbps: 6_000, net_kbps: 12_000, buffer_ms: 31_000, ratio_pm: 2_000, action: code::ABR_ACTION_COMMIT_DOWN,
            target_kbps: 3_000, unsafe_deficit_ms: 1_500, safe_kbps: 9_000, optimal_kbps: 8_000, unc_pm: 150,
            samples: 12, slope_ms_per_s: -40, starve_secs: 90, pred_pm: 1_100, risk: 7, why: crate::player::ABR_WHY_BUFFER,
        }
    }

    /// One golden per family: the demux's request and declaration, the sample, the decision, the
    /// model, and the Original watchdog's window all read through `diag()` as they were written.
    #[test]
    fn diag_reads_the_demuxs_half_from_the_sample_and_the_workers_from_the_photograph() {
        let _serial = plx_base::testlock::serial();
        let demux = DemuxAbr { mode: code::ABR_MODE_HLS, kbps: 3_000, declared_kbps: 2_900 };
        let (ps, _) = session(demux, full());
        let d = crate::player::test_physical::diag(&ps);
        assert_eq!((d.abr_mode, d.abr_kbps, d.abr_declared_kbps), (code::ABR_MODE_HLS, 3_000, 2_900));
        assert_eq!((d.abr_media_kbps, d.abr_net_kbps, d.abr_buffer_ms, d.abr_ratio_pm), (6_000, 12_000, 31_000, 2_000));
        assert_eq!((d.abr_action, d.abr_target_kbps, d.abr_why), (code::ABR_ACTION_COMMIT_DOWN, 3_000, crate::player::ABR_WHY_BUFFER));
        assert_eq!((d.abr_safe_kbps, d.abr_optimal_kbps, d.abr_unc_pm, d.abr_samples), (9_000, 8_000, 150, 12));
        assert_eq!((d.abr_slope_ms_per_s, d.abr_starve_secs, d.abr_pred_pm, d.abr_risk), (-40, 90, 1_100, 7));
        assert_eq!(d.abr_unsafe_deficit_ms, 1_500);
    }

    /// The demux's start used to store STEADY where the reset stores 0; the panel words them
    /// differently, so a started controller that has decided nothing reads STEADY, no controller
    /// reads 0, and a decision shows as itself.
    #[test]
    fn diag_words_a_started_controller_that_has_decided_nothing_as_steady() {
        let _serial = plx_base::testlock::serial();
        let started = DemuxAbr { mode: code::ABR_MODE_HLS, kbps: 3_000, declared_kbps: -1 };
        let (ps, _) = session(started, AbrReadout::IDLE);
        let d = crate::player::test_physical::diag(&ps);
        assert_eq!((d.abr_action, d.abr_target_kbps), (code::ABR_ACTION_STEADY, 0));
        assert_eq!((d.abr_media_kbps, d.abr_net_kbps, d.abr_samples, d.abr_why), (-1, -1, -1, 0));
        let (ps, _) = session(DemuxAbr::IDLE, AbrReadout::IDLE);
        assert_eq!(crate::player::test_physical::diag(&ps).abr_action, 0, "no controller at all");
        let original = DemuxAbr { mode: code::ABR_MODE_ORIGINAL, kbps: 8_000, declared_kbps: -1 };
        let decided = AbrReadout { action: code::ABR_ACTION_PRIME_UP, ..AbrReadout::IDLE };
        let (ps, _) = session(original, decided);
        assert_eq!(crate::player::test_physical::diag(&ps).abr_action, code::ABR_ACTION_PRIME_UP);
    }

    /// A line printed after the stop reads what the reset left: the photograph and the sample's
    /// demux half go back together, at the one `SessionReset` the stop runs.
    #[test]
    fn diag_after_a_session_reset_reads_the_idle_values() {
        let _serial = plx_base::testlock::serial();
        let demux = DemuxAbr { mode: code::ABR_MODE_HLS, kbps: 3_000, declared_kbps: 2_900 };
        let (mut ps, _) = session(demux, full());
        ps.step_readout(ReadoutEvent::SessionReset);
        let d = crate::player::test_physical::diag(&ps);
        assert_eq!((d.abr_mode, d.abr_kbps, d.abr_declared_kbps), (0, 0, -1));
        assert_eq!((d.abr_media_kbps, d.abr_net_kbps, d.abr_buffer_ms, d.abr_ratio_pm), (-1, -1, -1, -1));
        assert_eq!((d.abr_action, d.abr_target_kbps, d.abr_unsafe_deficit_ms, d.abr_why), (0, 0, 0, 0));
        assert_eq!((d.abr_safe_kbps, d.abr_optimal_kbps, d.abr_unc_pm, d.abr_samples), (-1, -1, -1, -1));
        assert_eq!((d.abr_slope_ms_per_s, d.abr_starve_secs, d.abr_pred_pm, d.abr_risk), (0, -1, -1, -1));
    }
}
