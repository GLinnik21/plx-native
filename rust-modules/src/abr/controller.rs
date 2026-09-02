use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // retired reason codes remain in the diagnostics wire enum
pub(crate) enum HlsReason {
    SafeBudgetIncrease,
    UnsafeCurrentState,
    ProductionConstraint,
    BufferConstraint,
    /// **The terminal case: the downshift trigger fired and there is no rung below.** R12 — "a
    /// predicate with no action in the region that motivates it."
    ///
    /// It is not a new behaviour. `self.current.below()` returns `self.current` at the ladder
    /// floor, so the proposal was already skipped and `Stay` was already the answer; there is no
    /// other answer, because the escape this trigger exists to take does not exist at the bottom
    /// of the ladder. What was missing is that it said nothing. The line read `decision=stay
    /// reason=None`, identical to a healthy segment, and telling the two apart meant
    /// cross-referencing `current=` against the ladder floor by hand — on the one state where the
    /// controller has exhausted everything it can do and the picture is about to stop.
    LadderFloor,
    /// Retired HLS reason retained in the diagnostics wire vocabulary. HLS eviction now uses the
    /// exact finite-bag conservation/runway test; starvation horizons remain on the Original and
    /// utility telemetry paths.
    StarvationHorizon,
    /// Every feasible higher rung has a failure certificate that the current exact exploration
    /// surplus and live delivery distribution cannot release, or one transaction exhausted the
    /// common budget available to every quality excitation without producing an ordinal response
    /// endpoint. No wall-clock dwell exists; only strictly more measured surplus, confidence-
    /// separated service evidence, or a new controller can change the frontier.
    RejectBackoff,
    /// The finite bag at the current operating point is not sustainable, or the playable reserve
    /// contains no surplus above its exact rollback runway.  A smaller response can never veto a
    /// larger request by pretending to measure unused path capacity; this reason says only that
    /// there is currently no physically disposable reserve with which to perform that request.
    EvidenceWindow,
    /// A fetch was abandoned at an observed terminal reserve boundary or at its transaction's
    /// explicit deadline. Its prefix is censored: it says that this acquisition did not finish
    /// before that boundary, not that the prefix rate is the path capacity. Roll back to the last
    /// working actuator (or one rung when there is no transaction to undo) without feeding that
    /// prefix to either estimator.
    DeadlineRollback,
    /// **The reserve is not knowable on this sample**, so there is nothing to decide against —
    /// `buffered_ms()` is `None` for exactly one situation, an A/V session whose audio lane has
    /// produced no timestamp since the open or the seek.
    ///
    /// It is the one Stay that is the ABSENCE of a policy rather than the application of one, and
    /// it is worth a code because it is otherwise indistinguishable on the line from a healthy
    /// segment: the estimators have all taken the sample, so every other field looks normal.
    ReserveUnknown,
    /// No technically feasible rung exists above the current actuator.
    AtBestRung,
    /// The current actuator is already the largest request, but PMS returned a response whose
    /// decoded geometry is provably below that request/source.  It remains refreshable as the
    /// same actuator after strictly stronger completed-service evidence; the request ceiling is
    /// not promoted into a claim about delivered quality.
    ResponseLimited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecisionReason {
    Hls(HlsReason),
}

/// Classification of the next higher-rung HLS excitation at the *currently observable* disposable
/// budget.
///
/// This is deliberately a state, rather than a boolean assembled independently by the HLS and
/// Original controllers.  A failure without an ordinal response endpoint owns one common budget
/// frontier: while it is active, every otherwise-untested higher request is blocked by the same
/// physical experiment. Treating only the per-rung certificates as "exhausted" made the HLS
/// controller correctly hold while the Original controller incorrectly waited for another HLS
/// action which the common frontier forbade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsExplorationState {
    /// At least one higher HLS request has no active certificate at this budget.
    Open,
    /// One censored/response-unchanged transaction blocks every higher HLS request until measured
    /// disposable reserve grows strictly beyond the budget it consumed.
    CommonBudgetBlocked,
    /// No common block exists, but every feasible higher request is either absent or carries its
    /// own unreleased certificate.  This includes the ordinary actuator ceiling.
    PerRungExhausted,
}

/// One coherent controller snapshot for the event log. `window`, `pending` and `reason` carry the
/// live finite-bag decision. Delivery/production/risk remain alongside them for Original-mode
/// comparison, bootstrap carry-over and diagnostics; they are not passive HLS capacity ceilings.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ControllerTelemetry {
    pub(crate) current: Rung,
    pub(crate) safe_budget_kbps: u32,
    /// The EMERGENCY horizon — `T = B*R/(R - C)` on the MEASURED rate — as `edge=` on the steady
    /// line. `risk.starvation_seconds` beside it is the same formula on the conservative rate and
    /// is a planning quantity; this is the one the downshift trigger reads.
    pub(crate) emergency_horizon_secs: Option<u32>,
    /// Retained for the diagnostics ABI and always `None` in the exact controller. A demand-capped
    /// response cannot publish a passive optimum above itself; the next real candidate is the
    /// experiment that discovers another operating point.
    pub(crate) optimal: Option<HlsCandidate>,
    pub(crate) delivery: CapacityEstimate,
    pub(crate) production: ProductionEstimate,
    pub(crate) buffer: BufferEstimate,
    pub(crate) risk: CandidateRisk,
    pub(crate) pending: Option<Proposal>,
    pub(crate) reason: Option<DecisionReason>,
    /// Exact finite-bag sustainability/runway readout for the current rung.
    pub(crate) window: AdmissionReadout,
    /// Compatibility fields for the diagnostic wire: `dwell_ms` is always zero, `blocked_kbps`
    /// reports the active per-rung failure frontier, and the remaining values are observational.
    pub(crate) gates: GateCounters,
}

/// Guard state, for the log line only. **Nothing reads this to decide anything.**
///
/// `on_rung` and `draining` are observational state. `dwell_ms` stays zero for wire compatibility;
/// `blocked_kbps` names the highest failure certificate that the current finite-bag surplus has
/// not released.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GateCounters {
    /// Retained in the diagnostics wire format. Exploration has no wall-clock dwell, so this is
    /// always zero; the observable surplus above the larger of the replay runway and rollback
    /// media horizon is the only release clock.
    pub(crate) dwell_ms: u64,
    /// The rung the reject/backoff guard is currently refusing, in kbps (N11). `0` is nothing.
    pub(crate) blocked_kbps: u32,
    /// Samples taken since the current rung was committed. The first is a PMS cold start, which
    /// is the ONE sample count that survives anywhere in HLS policy — it feeds the production
    /// estimator's 1-vs-3 weight, and it is I3's cold-start predicate. It is not an adaptation
    /// gate any more (N9).
    pub(crate) on_rung: u8,
    /// Consecutive samples the reserve has been draining. **No longer a threshold** — N21 replaced
    /// `>= 8` on the production arm with `BufferEstimate::draining()`'s magnitude test — but still
    /// worth reading, because `starving()`'s second arm counts it.
    pub(crate) draining: u32,
}

/// **Why a candidate transaction was abandoned, in the only distinction the backoff guard needs.**
///
/// N11 asks the controller to record "the rejected rung and the reason". The reason matters for
/// exactly one decision: whether the failure says anything about the RUNG. Every reject site in
/// `ff.rs` already names itself to `tx.finish(...)`, so the vocabulary exists; this collapses it
/// to the one axis that changes behaviour, at the call site, where the knowledge is.
///
/// Getting this wrong in the permissive direction re-creates the livelock N11 exists to stop.
/// Getting it wrong in the strict direction blocks a perfectly good rung after a seek — which is
/// why `Circumstance` is not a courtesy: `reserve_unreadable` and `origin_changed` are statements
/// about the SESSION, and the transaction that follows them starts from different facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RejectCause {
    /// The transaction exhausted its absolute deadline before producing a complete candidate
    /// media quantum. This is a common right-censored observation, not an ordinal response-size
    /// endpoint: it proves only that the serial decision/start/playlist/body path did not finish
    /// inside the actually executed budget. No quality actuator may retry at that budget or less.
    /// After a larger budget releases that hard block, the failed actuator remains an operational
    /// search endpoint so an epsilon of refill cannot buy the identical maximum again; this is a
    /// scheduling fact, not a claim that a response size was observed.
    Censored,
    /// A complete candidate was observed but could not be funded from the post-transaction
    /// reserve. Its output is known, so this remains actuator-specific ordinal evidence; a lower
    /// response may still fit even when this one does not. The same actuator needs a strictly
    /// larger exploration budget.
    Candidate,
    /// A completed candidate acquired more slowly than it produced media (`A > D`). More reserve
    /// can postpone the loss but cannot make this completed operating point sustainable in the
    /// measured service regime. This is retained separately from a deadline certificate so a
    /// larger buffer cannot release it. A confidence-separated current-rung delivery regime can:
    /// that is new physical evidence about end-to-end service, not time or reserve pretending the
    /// old observation changed.
    CompletedUnsustainable,
    /// A typed fact about this exact actuator that more reserve cannot change for this controller
    /// scope, such as PMS refusing this ceiling. It remains excluded until a new controller is
    /// built for a new route/session.
    Structural,
    /// A fresh session returned no Pareto quality gain over the response already playing. This is
    /// not a structural or ordinal fact about the requested actuator: PMS answered with a
    /// different, demand-capped response. The completed transaction gives an exact common budget
    /// endpoint, so no quality request runs again until disposable reserve is strictly larger.
    /// Requiring the small response to prove a higher hidden service rate would make a demand-
    /// capped PMS response absorbing; trying lower request ceilings would merely churn encoders
    /// without any response-size evidence that orders those requests.
    ResponseUnchanged,
    /// The decoded raster exceeded this actuator's bounding box. Every smaller box is at least as
    /// restrictive, so this structural fact masks this rung and all rungs below it; a larger box
    /// remains eligible.
    StructuralAtOrBelow,
    /// The transaction was abandoned for a reason that says nothing about the rung: the origin
    /// moved, the reserve stopped being readable (a seek), or the session moved underneath the
    /// prime (`route::PrimeRefusal::Session` / `Control` — the encoder changed, the client is gone,
    /// or the control-plane call never answered). Does not arm the backoff.
    Circumstance,
}

/// Monotone failure memory for one actuator. Completed candidate evidence is ordered by response
/// size: a later, cheaper failure must not erase the larger budget already spent on a higher rung.
/// Transactions with no ordinal response endpoint have an additional common frontier on
/// [`Controller`]: either no response completed, or PMS completed a no-gain object unrelated to
/// the requested ceiling. The array is indexed by [`Rung::index`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FailureCertificate {
    /// Largest end-to-end exploration budget under which this rung failed or completed unfunded.
    /// Updates take `max`; the same rung is eligible again only at a strictly larger observable
    /// surplus.
    deadline_budget_ms: Option<i64>,
    /// This actuator supplied an upper scheduling endpoint even after a larger reserve releases
    /// its hard budget block.  A completed candidate does so directly; a deadline-censored
    /// candidate does so operationally because repeating that same largest transaction on every
    /// epsilon of refill is the observed livelock.  `ResponseUnchanged` deliberately does not:
    /// PMS returned an unrelated demand-capped response which cannot order request actuators.
    service_endpoint: bool,
    /// Recent current-rung delivery estimate when a completed `A > D` observation was made. The
    /// certificate blocks until the live distribution's conservative bound is strictly above
    /// this old-regime estimate. A larger reserve or elapsed wall time cannot release it.
    completed_unsustainable_hls_fast_kbps: Option<u32>,
    /// A typed session-scoped refusal that reserve cannot cure.
    structural: bool,
}

impl FailureCertificate {
    fn completed_unsustainable_blocks(self, delivery: &CapacityEstimate) -> bool {
        self.completed_unsustainable_hls_fast_kbps
            .is_some_and(|old_regime| delivery.conservative_kbps() <= old_regime)
    }

    fn blocks(self, exploration_budget_ms: Option<i64>, delivery: &CapacityEstimate) -> bool {
        self.structural
            || self.completed_unsustainable_blocks(delivery)
            || self
                .deadline_budget_ms
                .is_some_and(|failed| exploration_budget_ms.is_none_or(|budget| budget <= failed))
    }

    /// Whether this response-size experiment supplies an upper endpoint for the next ordinal
    /// search. Structural refusals deliberately do not: a codec/session refusal at one actuator
    /// says nothing about a larger bounding box, whereas a service failure tells the scheduler
    /// where splitting the still-unclassified response-size interval is useful.  Unlike
    /// [`Self::blocks`], a deadline endpoint remains useful after a larger reserve releases the
    /// hard retry block: the extra millisecond proves the experiment is affordable again, not
    /// that repeating the same largest response is the most informative next experiment.
    ///
    /// This is a scheduling relation, not a theorem that every larger future response must fail.
    /// Each rung remains independently eligible under [`Self::blocks`], and a strictly larger
    /// physical budget releases a censored endpoint exactly as before.
    fn service_blocks(
        self,
        _exploration_budget_ms: Option<i64>,
        delivery: &CapacityEstimate,
    ) -> bool {
        self.completed_unsustainable_blocks(delivery) || self.service_endpoint
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Proposal {
    pub(crate) rung: Rung,
    pub(crate) direction: Direction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Stay,
    Prime(Proposal),
}

/// What the candidate transaction is required to preserve while it runs.
///
/// This is controller intent, not a reconstruction from diagnostics. `TerminalFloor` is issued
/// only by the exact `B<R_o` / `!survivable` branch, together with `Down` to the feasible ladder
/// floor. It does not predict that the next acquisition must fail; it records that the finite bag
/// no longer supplies the rollback guarantee under which a reserve deadline is useful. The policy
/// belongs to one pending proposal and is cleared with that proposal on commit, reject or session
/// cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReservePolicy {
    Preserve,
    TerminalFloor,
}

/// Exact result of grading the candidate's own completed acquisition. Kept richer than a boolean
/// so the transaction can retain only the kind of evidence the observation actually supplies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CandidateVerdict {
    Ready,
    /// The first complete object of a fresh encoder contains the one-time decision/session/JIT
    /// setup leg.  It acquired slower than media time, but the post-object reserve still funds
    /// that exact acquisition.  This is neither acceptance nor an `A>D` steady-state refusal: it
    /// authorizes one ordinary object from the encoder that is now running.  Only the caller that
    /// structurally knows it is holding the session-boundary object can produce this verdict.
    SetupBearing,
    Incomplete,
    ReserveUnknown,
    /// `A > D`: this operating point loses playable media on every such acquisition.
    Unsustainable,
    /// `A <= D`, but the post-transaction reserve is shorter than `A`; more measured surplus can
    /// make the same experiment fundable.
    Unfunded,
}

/// Integer-only estimator and transaction state. Current-session samples decide whether to
/// propose. Candidate-session measurements decide whether that proposal may commit.
pub(crate) struct Controller {
    pub(super) current: Rung,
    pending: Option<Proposal>,
    pending_reserve_policy: Option<ReservePolicy>,
    delivery: CapacityEstimate,
    production: ProductionEstimate,
    buffer: BufferEstimate,
    catalog: HlsActuatorCatalog,
    policy: AbrPolicy,
    samples_on_rung: u8,
    /// **Wall clock, supplied by the caller on every `observe`** — the controller owns no clock of
    /// its own and must not: `SegmentSample::total_fetch_us` is per-REQUEST elapsed and is duty
    /// cycled, running slow exactly when the reserve is full and the byte cap is idling the demux
    /// worker, which is the substitution N13 identifies as a defect elsewhere.
    now_ms: u64,
    /// Wall-clock instant of the last readable reserve observation.  A reserve derivative is
    /// `delta B / delta wall`; media duration is the credit delivered by a completed segment, not
    /// an elapsed-time measurement.
    last_buffer_observation_ms: Option<u64>,
    /// Exact duration of the NEXT object on the still-live current cursor, whose completion must
    /// remain funded if an experiment rolls back. This is deliberately not the duration of the
    /// object which just completed: HLS permits variable EXTINF durations, so the previous `D`
    /// cannot certify the next response. `None` means the cursor has no next object (end of
    /// stream), which authorizes no new quality/source experiment.
    rollback_media_ms: Option<u32>,
    /// Per-rung, monotone failure frontier. A blocked top rung never masks an eligible lower one.
    failures: [FailureCertificate; LADDER.len()],
    /// Largest common refill frontier established by a quality transaction which did not produce
    /// an ordinal response endpoint: `E_f`, the exact disposable budget armed for the attempt.
    /// The transaction was either right-censored before
    /// complete media or PMS completed a demand-capped response with no Pareto gain. Until the
    /// live operating point exposes a strictly larger budget, *no other upshift actuator* is
    /// eligible; changing the requested rung is not new evidence in either case.
    ///
    /// This is separate from [`Self::failures`]. That array retains actuator-specific evidence
    /// for search once a larger common budget exists; this frontier prevents untouched rungs from
    /// laundering the same exhausted reserve into immediate PMS encoder churn.
    common_budget_frontier_ms: Option<i64>,
    /// Spendable reserve attached to the in-flight upshift. At rejection it becomes `E_f` in the
    /// common refill frontier; the worker replaces the proposal-time value with the exact budget
    /// it actually armed.
    pending_exploration_budget_ms: i64,
    /// The actuator displaced by the most recent upshift until the first ordinary live segment on
    /// the new actuator completes.  An abandoned first live fetch rolls the transaction back here;
    /// its censored prefix must not synthesize a fresh bitrate target.
    rollback_rung: Option<Rung>,
    /// Next actuator in an in-progress recovery descent. A failed rollback transaction is evidence
    /// that the previously known-good actuator no longer repairs this service episode; the only
    /// coefficient-free minimax target for time-to-picture is then the smallest feasible response.
    /// A completed active segment ends the recovery episode.
    recovery_target: Option<Rung>,
    last_reason: Option<DecisionReason>,
    last_safe_budget_kbps: u32,
    /// **The emergency deadline the last decision was taken against**, so the log carries the
    /// quantity that DECIDED rather than a neighbouring one that did not. `None` means the
    /// measured rate covers the current rung, which is the healthy state and not a missing
    /// reading.
    ///
    /// It is a second horizon beside `risk.starvation_seconds` on purpose and they can disagree by
    /// a factor of two: that one is computed on `conservative_kbps()` for the risk score and the
    /// mode comparison, this one on the measured rate for the eviction decision. Publishing only
    /// the planning horizon while deciding on the measured one is the exact shape of the trap
    /// `[[silent-instrument-trap]]` names -- a log full of plausible numbers, none of them the one
    /// that fired.
    last_emergency_horizon: Option<u32>,
    /// **Dev-only actuator pin — see [`Self::pinned_to`].** `None` in every production path.
    pin: Option<Rung>,
    /// The finite episode of completed acquisitions at the current operating point. Its live
    /// associative summary computes the replay boundaries and the surplus that may be spent on an
    /// actual excitation; its bounded ring is diagnostic only. Neither is scaled into a claim
    /// about the unused tail of a larger response.
    acquisitions: AcquisitionWindow,
    /// What the §4 rule WOULD have said about staying on the current rung, recomputed each sample.
    /// Read only by telemetry.
    last_window: AdmissionReadout,
    /// The server response currently feeding the picture, separate from [`Self::current`], which
    /// is only the actuator sent on the wire.  Conflating these two made a 22 Mbps request that
    /// returned 979 kbps / 720p terminal at `AtBestRung`.
    active_variant: Option<ObservedHlsVariant>,
    /// Completed-service rate observed when this response first became active. It authorizes the
    /// first fresh session at the same actuator only after the live conservative bound rises
    /// strictly above the old response's evidence. If that fresh session completes unchanged,
    /// the common disposable-budget frontier paces later retries; the demand-capped response is
    /// not asked to identify dormant path capacity.
    active_variant_evidence_kbps: Option<u32>,
}

impl Controller {
    /// Unknown links start at 480p/720 kbit/s. P240 remains available as the emergency floor.
    /// The active encoder already exists at `current` (including a runtime Original fallback
    /// chosen from its measured link), so the controller must begin from that exact wire state.
    ///
    /// `prior` is the bootstrap probe as an explicitly weak seed — the first segment then REFINES
    /// a measurement instead of starting from nothing, which is what stops the opening minute
    /// being spent re-deriving a number the app already paid for. `catalog` arrives with this
    /// playback's feasibility bounds already applied.
    pub(crate) fn starting_at(
        current: Rung,
        prior: Option<CapacityEstimate>,
        catalog: HlsActuatorCatalog,
    ) -> Self {
        Self {
            current,
            pending: None,
            pending_reserve_policy: None,
            delivery: prior.unwrap_or_default(),
            production: ProductionEstimate::default(),
            buffer: BufferEstimate::default(),
            catalog,
            policy: AbrPolicy::measured(),
            samples_on_rung: 0,
            now_ms: 0,
            last_buffer_observation_ms: None,
            rollback_media_ms: None,
            failures: [FailureCertificate::default(); LADDER.len()],
            common_budget_frontier_ms: None,
            pending_exploration_budget_ms: 0,
            rollback_rung: None,
            recovery_target: None,
            last_reason: None,
            last_safe_budget_kbps: 0,
            last_emergency_horizon: None,
            pin: None,
            acquisitions: AcquisitionWindow::default(),
            last_window: AdmissionReadout::default(),
            active_variant: None,
            active_variant_evidence_kbps: None,
        }
    }

    /// **Pin the controller to one actuator, for measurement only.** A builder rather than a
    /// parameter so no existing call site or test moves.
    ///
    /// Set from the `plxnative-abrpin` dev trigger, which is compiled out of a release build, so
    /// this is `None` in every shipped configuration and in every host test that does not ask for
    /// it. It exists for measurement step M4 (`docs/adaptive-playback-plan.md` §4), which has to
    /// hold one rung long enough to read a settled reserve at it — and which cannot use the
    /// playback-quality selector, because a non-Auto quality returns `None` from
    /// `route::hls_abr_control` before a controller is ever built, and because the quality ladder
    /// has no mid-1080p points.
    ///
    /// **What it does NOT do**: it changes no threshold, no budget and no risk term. Every
    /// estimator still updates on every segment, `telemetry()` still reports the model's real
    /// state, and the log lines still say what the model would have wanted. It short-circuits the
    /// DECISION only, and only after all measurement has been taken — so the numbers M4 reads are
    /// the numbers an unpinned run would have produced for that rung.
    pub(crate) fn pinned_to(mut self, pin: Option<Rung>) -> Self {
        self.pin = pin;
        self
    }

    #[cfg(test)]
    pub(crate) fn clear_pin(&mut self) {
        self.pin = None;
    }

    pub(crate) fn current(&self) -> Rung {
        self.current
    }

    #[cfg(test)]
    pub(crate) fn pending(&self) -> Option<Proposal> {
        self.pending
    }

    /// **Test-only: `observe` with the fixture wall clock advanced by one segment.**
    ///
    /// `observe` takes absolute monotonic milliseconds and the fixtures in `tests.rs` are written
    /// around one steady state: a stream that keeps up delivers `d` of content every `d` of wall
    /// clock. This advances the clock by exactly that and nothing else.
    ///
    /// **A test about a wall-clock guard must call `observe` directly.** Those are the tests where
    /// the cadence IS the subject, and expressing it through this fixture would assert the fixture.
    #[cfg(test)]
    pub(crate) fn observe_next(&mut self, sample: SegmentSample) -> Decision {
        let now = self
            .now_ms
            .saturating_add(u64::from(sample.media_duration_ms));
        self.observe(sample, now)
    }

    /// The controller's own wall clock, for a test that must CONTINUE it rather than invent a
    /// second origin. `observe_next` advances this by a segment per call; a test that then starts
    /// counting from zero would not be measuring the controller's timeline.
    #[cfg(test)]
    pub(crate) fn clock_ms(&self) -> u64 {
        self.now_ms
    }

    #[cfg(test)]
    pub(crate) fn window_len(&self) -> usize {
        self.acquisitions.episode_len()
    }

    pub(crate) fn catalog(&self) -> HlsActuatorCatalog {
        self.catalog
    }

    /// Attach the actual master/raster response to the request actuator that produced it. The
    /// evidence value is the completed segment's body-service observation, not the requested or
    /// declared bitrate. Repeated segments on the same response refine [`Self::delivery`] without
    /// moving the release snapshot; only a new response starts a new evidence regime.
    pub(crate) fn observe_active_variant(
        &mut self,
        variant: ObservedHlsVariant,
        evidence_kbps: u32,
    ) {
        if self.active_variant == Some(variant) {
            return;
        }
        self.active_variant = Some(variant);
        self.active_variant_evidence_kbps = Some(evidence_kbps);
    }

    fn active_variant_needs_refresh(&self) -> bool {
        self.active_variant.is_some_and(|variant| {
            variant.definitively_underfills(self.current, self.catalog.source_raster())
        })
    }

    fn active_variant_refresh_released(&self) -> bool {
        self.active_variant_evidence_kbps
            .is_some_and(|observed| self.delivery.conservative_kbps() > observed)
    }

    /// The worst fixed per-segment acquisition cost observed on this playback — see
    /// [`AcquisitionWindow::worst_overhead_us`]. Read by the transaction that sets a candidate
    /// warm-up deadline, which must cover it.
    pub(crate) fn worst_overhead_us(&self) -> u64 {
        self.acquisitions.worst_overhead_us()
    }

    pub(crate) fn delivery(&self) -> CapacityEstimate {
        self.delivery
    }

    pub(crate) fn buffer(&self) -> BufferEstimate {
        self.buffer
    }

    /// Classify whether HLS has an informative higher-rung excitation at the physical exploration
    /// budget available now.
    ///
    /// This is deliberately about the controller's measured failure frontier, not about whether
    /// `current` happens to equal the catalog's largest request. PMS is free to map a larger
    /// request to the same (or a worse) rendition. Such a completed transaction is correctly
    /// rejected and can therefore never become `current`; requiring it to do so turns a valid
    /// structural refusal into a permanent lock that also suppresses the independent direct-file
    /// experiment.
    pub(crate) fn hls_exploration_state(&self) -> HlsExplorationState {
        let exploration_budget = self.exploration_budget_ms(self.buffer.buffered_ms);
        if self.common_budget_blocks(exploration_budget) {
            return HlsExplorationState::CommonBudgetBlocked;
        }
        if self
            .catalog
            .feasible()
            .filter(|candidate| candidate.rung > self.current)
            .any(|candidate| {
                !self.failures[candidate.rung.index()].blocks(exploration_budget, &self.delivery)
            })
        {
            HlsExplorationState::Open
        } else {
            HlsExplorationState::PerRungExhausted
        }
    }

    /// Whether no informative higher-rung HLS excitation remains at the currently measured HLS
    /// budget, leaving the source request eligible to be the remaining quality excitation. Kept
    /// as the narrow predicate consumed by the independent Original utility gate; the higher-rung
    /// classification lives in [`Self::hls_exploration_state`].
    pub(crate) fn hls_frontier_exhausted(&self) -> bool {
        self.hls_exploration_state() != HlsExplorationState::Open
    }

    /// Exact worst-permutation replay boundary for starting a paused HLS clock on the active
    /// rendition. Unlike retired statistical admission this exists before `n` samples; it claims
    /// only what the completed observations would cost to replay, not what an unseen draw will cost.
    pub(crate) fn prime_runway_ms(&self) -> Option<i64> {
        self.acquisitions
            .observed_runway_us()
            .map(|us| us / 1_000 + i64::from(us % 1_000 != 0))
    }

    /// Smallest feasible response below the active one, or the active one at the terminal point.
    /// Recovery asks this by actuator order; it must never smuggle a demand-capped throughput
    /// reading back into target selection.
    fn recovery_floor(&self) -> Rung {
        self.catalog
            .feasible()
            .filter(|candidate| candidate.rung < self.current)
            .map(|candidate| candidate.rung)
            .min()
            .unwrap_or(self.current)
    }

    /// Highest lower actuator supported by the measurements this completed operating point made.
    ///
    /// The caller deliberately withholds this from both cases where another exploratory
    /// transaction cannot be funded: an abandoned fetch has no completed media quantum or point
    /// service observation, and a completed bag with `B` below its ordered runway cannot replay
    /// even the chronology it observed. Their minimax answer remains
    /// [`Self::recovery_floor`]. Here the current bag DID complete
    /// and remains survivable, so conservative delivery and reserve refill are measured inputs.
    /// Reusing their existing conjunction loses less picture without adding a threshold or
    /// pretending the demand-capped sample measured unused capacity. The selected actuator is
    /// still only a proposal; its own completed acquisition must validate before it can commit. If
    /// the model supports no lower point, the floor remains the only bounded exit.
    fn completed_recovery_target(&self, safe_budget_kbps: u32, buffered_ms: i64) -> Rung {
        self.catalog
            .feasible()
            .filter(|candidate| candidate.rung < self.current)
            .filter(|candidate| {
                self.catalog.modeled_sustainable(
                    *candidate,
                    safe_budget_kbps,
                    &self.policy,
                    buffered_ms,
                )
            })
            .max_by_key(|candidate| candidate.expected_wire_kbps)
            .map(|candidate| candidate.rung)
            .unwrap_or_else(|| self.recovery_floor())
    }

    /// Wall-clock budget an exploratory upshift may spend while the current picture keeps
    /// running. After a failed experiment, let `L` be the reserve left for the ordinary current-
    /// rung acquisition. Surviving any still-sustainable unseen response requires `L >= D`; after
    /// it completes, `B' = L - A + D_next >= L`, so restoring the finite-bag stress boundary
    /// requires `L >= R_s`. Both obligations are funded exactly by `L >= max(R_s,D_next)`, not by
    /// adding them: they occur on opposite sides of the same media credit. `D_next` comes from the
    /// rollback cursor's parsed playlist; no current-tier transfer rate predicts a larger response.
    pub(crate) fn exploration_budget_ms(&self, reserve_ms: i64) -> Option<i64> {
        self.exploration_budget_ms_for(reserve_ms, self.rollback_media_ms)
    }

    fn exploration_budget_ms_for(
        &self,
        reserve_ms: i64,
        rollback_media_ms: Option<u32>,
    ) -> Option<i64> {
        let observed = self.acquisitions.observed_admission(reserve_ms)?;
        if !observed.sustainable {
            return None;
        }
        let runway_ms = observed.runway_us / 1_000 + i64::from(observed.runway_us % 1_000 != 0);
        let rollback_media_ms = rollback_media_ms
            .map(i64::from)
            .filter(|duration| *duration > 0)?;
        let rollback_reserve_ms = runway_ms.max(rollback_media_ms);
        let budget = reserve_ms.saturating_sub(rollback_reserve_ms);
        (budget > 0).then_some(budget)
    }

    /// Whether a transaction without an ordinal response endpoint still owns this common refill
    /// frontier. Neither actuator order nor a different request ceiling can turn a budget which
    /// has not restored the failed transaction's starting surplus into fresh physical evidence.
    fn common_budget_blocks(&self, exploration_budget_ms: Option<i64>) -> bool {
        self.common_budget_frontier_ms
            .is_some_and(|failed| exploration_budget_ms.is_none_or(|budget| budget <= failed))
    }

    /// Replace the proposal-time surplus with the budget the worker actually armed after it
    /// re-read the reserve. A rejection certificate must price the experiment that ran, not an
    /// older, larger number observed before control-plane time elapsed.
    pub(crate) fn set_executed_exploration_budget(
        &mut self,
        proposal: Proposal,
        budget_ms: i64,
    ) -> bool {
        if self.pending != Some(proposal) || proposal.direction != Direction::Up || budget_ms <= 0 {
            return false;
        }
        // The sample that selected the proposal and the worker's transaction re-read are
        // separated by queue/feed and main-thread progress. A frontier released by the earlier
        // reserve may be blocked again by the budget that can actually be armed. Authorizing it
        // anyway buys the same failed rung with less reserve than it already exhausted — a
        // time-of-check/time-of-use hole in the physical guard.
        if self.common_budget_blocks(Some(budget_ms))
            || self.failures[proposal.rung.index()].blocks(Some(budget_ms), &self.delivery)
        {
            self.pending = None;
            self.pending_reserve_policy = None;
            self.pending_exploration_budget_ms = 0;
            self.last_reason = Some(DecisionReason::Hls(HlsReason::RejectBackoff));
            return false;
        }
        self.pending_exploration_budget_ms = budget_ms;
        true
    }

    /// A pause is wall-clock time with no measurement — the one gap where the estimate really has
    /// aged (backpressure is not: a full buffer stops the reader on purpose).
    pub(crate) fn on_resume(&mut self, paused_ms: u64) {
        self.delivery.age_ms(paused_ms, &self.policy);
        self.buffer = BufferEstimate::default();
        self.last_buffer_observation_ms = None;
        // `samples_on_rung` describes uninterrupted time on this rung, and a pause ends that.
        self.samples_on_rung = 0;
        // Keep candidate evidence. A pause opens an unmeasured era; it neither increases the
        // exploration budget nor changes the requested operating point, so clearing a common
        // failure here would make wall time alone retry the identical transaction.
    }

    /// Immutable reserve contract for this one in-flight proposal.
    ///
    /// `None` means the proposal is stale or belongs to another controller/session. The demux
    /// worker captures the answer before it performs any control-plane I/O, so a later diagnostic
    /// update, user pause or main-thread rebuffer transition cannot demote terminal recovery back
    /// into a rollback experiment.
    pub(crate) fn pending_reserve_policy(&self, proposal: Proposal) -> Option<ReservePolicy> {
        (self.pending == Some(proposal))
            .then_some(self.pending_reserve_policy)
            .flatten()
    }

    /// Everything one decision was made on, in one struct, for one event-log line. Assembled here
    /// rather than in `ff.rs` so the numbers logged are the numbers used.
    pub(crate) fn telemetry(&self) -> ControllerTelemetry {
        let current = self.catalog.candidate(self.current);
        ControllerTelemetry {
            current: self.current,
            safe_budget_kbps: self.last_safe_budget_kbps,
            emergency_horizon_secs: self.last_emergency_horizon,
            // There is no passive "optimal" above a demand-capped response. Publishing one here
            // was the diagnostic form of the same identification error that held playback at a
            // low tier: the panel said "best available" for a ceiling the request itself imposed.
            // The actual excitation target is published as `pending` and on the transaction line.
            optimal: None,
            delivery: self.delivery,
            production: self.production,
            buffer: self.buffer,
            gates: GateCounters {
                dwell_ms: 0,
                // The guard's EFFECT, not its storage: `0` means nothing is being refused right
                // now, which is the question a log line is asked. A block that has released is
                // indistinguishable from no block at all, and reporting it would read as a stuck
                // guard on exactly the segments where it had already got out of the way.
                blocked_kbps: {
                    let budget = self.exploration_budget_ms(self.buffer.buffered_ms);
                    self.catalog
                        .feasible()
                        .filter(|candidate| candidate.rung > self.current)
                        .filter(|candidate| {
                            self.common_budget_blocks(budget)
                                || self.failures[candidate.rung.index()]
                                    .blocks(budget, &self.delivery)
                        })
                        .map(|candidate| candidate.rung.kbps())
                        .max()
                        .unwrap_or(0)
                },
                on_rung: self.samples_on_rung,
                draining: self.buffer.draining_samples,
            },
            risk: candidate_risk(
                current,
                current,
                &self.delivery,
                &self.production,
                &self.buffer,
                &self.policy,
            ),
            pending: self.pending,
            reason: self.last_reason,
            window: self.last_window,
        }
    }

    /// **`now_ms` is ABSOLUTE monotonic wall clock, not a delta**, for the reason
    /// `OriginalRecovery::advance_to` already documents: segments do not arrive on a regular
    /// cadence, so a delta API would make every wall-clock guard depend on how often the caller
    /// happened to call — which is a property of the link, not of the guard. The production caller
    /// passes one `Instant`'s elapsed time for the whole playback (`ff.rs`).
    #[cfg(test)]
    pub(crate) fn observe(&mut self, sample: SegmentSample, now_ms: u64) -> Decision {
        self.observe_with_rollback(sample, Some(sample.media_duration_ms()), now_ms)
    }

    /// Observe a completed current object together with the exact media obligation which remains
    /// on the rollback cursor. The cursor owns this fact: it comes from the next parsed EXTINF,
    /// not from a bitrate estimate or from the duration of the preceding object.
    pub(crate) fn observe_with_rollback(
        &mut self,
        sample: SegmentSample,
        rollback_media_ms: Option<u32>,
        now_ms: u64,
    ) -> Decision {
        self.observe_inner(sample, rollback_media_ms, now_ms, false)
    }

    /// Observe the first completed media object of a newly opened encoder without treating its
    /// one-time encoder/session startup as a steady-state actuator failure. Delivery, production
    /// and buffer measurements are retained; only the repeatable acquisition bag and its decision
    /// are deferred until the next object. This is a structural boundary marker, not a sample
    /// count or duration threshold: every fresh HLS cursor has exactly one structural boundary
    /// object. Whether `A>D` later classifies a candidate as setup-bearing is a separate verdict.
    pub(crate) fn observe_session_boundary(
        &mut self,
        sample: SegmentSample,
        rollback_media_ms: Option<u32>,
        now_ms: u64,
    ) -> Decision {
        self.observe_inner(sample, rollback_media_ms, now_ms, true)
    }

    fn observe_inner(
        &mut self,
        sample: SegmentSample,
        rollback_media_ms: Option<u32>,
        now_ms: u64,
        session_boundary: bool,
    ) -> Decision {
        self.now_ms = now_ms;
        self.rollback_media_ms = rollback_media_ms.filter(|duration| *duration > 0);
        // **A reason describes THIS sample or there is none.** It was written on every path that
        // reached a conclusion and cleared on none, so any earlier return re-published the last
        // one that happened to be set. I6's dwell gate made that visible: it returns before a
        // target is selected, so the line read `dwell=1400ms reason=SafeBudgetIncrease` — the
        // reason belonging to the commit that ARMED the dwell, on a sample where no evaluation
        // took place. `HlsReason::RejectBackoff`'s doc already argues that the dwell needs no code
        // of its own ("a dwell that is holding returns before any target is selected, so there is
        // no rung to name"), and that argument only holds if the field is empty when it says so.
        self.last_reason = None;
        let ratio = sample.production_ratio_pm();
        let current_candidate = self.catalog.candidate(self.current);
        // A segment at a low rung on a fast link is too small to time; clamp what it may claim to
        // what it can actually support. See `CapacityObservation::clamped_to_evidence`.
        let observation = CapacityObservation {
            kbps: sample.network_kbps(),
            bytes: sample.bytes,
            active_us: sample.active_fetch_us,
            // **Not a hardcoded `true` any more.** `SegmentSample::abandoned` is how a caller says
            // the fetch was cut off, and `completed` was already wired to `MAX_UNCERTAINTY_PM` —
            // the field existed, its semantics were right, and this call site overrode them.
            completed: sample.completed(),
        }
        .clamped_to_evidence(current_candidate.expected_wire_kbps);
        let completed = sample.completed();
        // An abandoned prefix is right-censored by the deadline that stopped it.  It contains no
        // completed media quantum and therefore cannot be a point observation of capacity, a
        // regime-change trigger, or the target of a downshift.  Keep the last completed service
        // reading for the ordinary risk telemetry; the deadline branch below handles the event.
        let network = if completed {
            observation.kbps
        } else {
            self.delivery.fast_kbps
        };
        let regime_changed = completed && observation.is_collapse(&self.delivery);
        if completed {
            if regime_changed {
                self.delivery.collapse(network);
            }
            self.delivery.update(observation);
        }
        let cold_start = self.samples_on_rung == 0;
        if completed {
            self.production
                .observe(ratio, current_candidate.production_load_pm, cold_start);
            self.samples_on_rung = self.samples_on_rung.saturating_add(1);
            // The first ordinary completed segment after an upshift proves the new live cursor.
            self.rollback_rung = None;
            // Likewise, a completed active fetch ends any interrupted recovery descent. Its
            // failure memory exists only to stop an abandoned active fetch and a failed Down
            // candidate from replaying the same pair; fresh completed media is a new episode.
            self.recovery_target = None;
        }
        let wall_delta = self
            .last_buffer_observation_ms
            .map(|last| now_ms.saturating_sub(last))
            .and_then(|elapsed| i64::try_from(elapsed).ok())
            .unwrap_or(0);
        if sample.buffer.buffered_ms().is_some() {
            self.buffer.update(sample.buffer.buffered_ms(), wall_delta);
            self.last_buffer_observation_ms = Some(now_ms);
        }

        let segment = i64::from(sample.media_duration_ms);

        // **Computed HERE, above every early return, and only read below.** The budget is the
        // delivery estimate's conservative network rate, so its value is identical wherever
        // between here and the decision it is taken — nothing in between mutates that input.
        // Where it was computed mattered anyway, because three paths
        // return before reaching the decision: a transaction in flight, and both arms of the dev
        // pin. On a pinned run that is EVERY sample after the pin is reached, and the measured
        // consequence was that 397 of 527 `abr: steady` lines reported `safe=0kbps` — then the
        // central admission quantity, and still an input to bootstrap/Original comparison and a
        // useful diagnostic — on exactly the runs designed to characterise a rung.
        let safe_budget = hls_safe_budget(&self.delivery);
        self.last_safe_budget_kbps = safe_budget;
        // Observe the finite episode at the CURRENT operating point. Its current-query readout
        // remains useful telemetry, while the decision consumes only its actual
        // sustainability/runway; no larger candidate is projected through it.
        //
        // Placed above every early return for the same reason `safe_budget` is: a pinned run
        // returns before the decision on every sample after the pin lands, and a quantity that is
        // only computed on the path it does not take is unobservable on exactly the runs meant to
        // characterise it.
        //
        // Every observation keeps its actual `(A_i,D_i)`; bytes are logged but do not scale a
        // candidate query. This answers "is what we are already playing sustainable, and what
        // rollback runway did its finite bag require?" The candidate is measured separately.
        // The reserve here is the latest estimator value because this sample may have no readable
        // A/V minimum. The same readout is consumed by the decision whenever reserve is readable
        // and is independently graded from the wire line.
        // **Only a fetch that RAN TO COMPLETION**, for the reason the capacity estimator already
        // stops one: a prefix the stall guard cut short times the abort, not the link. This call
        // used to be unconditional, so the same prefix the estimator refused to trust still
        // entered the acquisition history as though it had delivered a media quantum.
        //
        // It poisons in both directions and that is the argument for excluding rather than for
        // reading its sign. `SegmentSample::completed`'s doc has the optimistic case (1 448 bytes
        // in 274 us, timing at 42 Mbit/s); the pessimistic one is device-measured 2026-08-30,
        // where `stall abort ... bytes=212992 ... at 6197kbps` followed a sample that had just
        // measured 56 660 kbps, and the ladder walked 22000 -> 4000 -> 2000 -> 720 behind it.
        //
        // The reserve and the production estimate above are deliberately still fed: those two
        // describe what HAPPENED to this playback, and an abort happened. This one describes what
        // the link can carry, and an abort is not evidence about that.
        if sample.completed() && !session_boundary {
            // `is_collapse` is the delivery estimator's explicit change-point declaration: its
            // old slow state is demoted before this response is incorporated. The acquisition bag
            // must use the same regime boundary. Letting pre-change fast segments subsidize this
            // new ordered queue both contradicts that posterior transition and double-counts
            // reserve they already filled before the collapse. Keep the completed response as
            // observation zero of the new regime; no second change threshold is introduced here.
            if regime_changed {
                self.acquisitions.reset();
            }
            self.acquisitions.observe(
                sample.bytes,
                sample.total_fetch_us(),
                sample.active_fetch_us(),
                i64::from(sample.media_duration_ms),
            );
        }
        self.last_window = self.acquisitions.observed_readout(self.buffer.buffered_ms);

        if session_boundary && sample.completed() {
            self.last_reason = Some(DecisionReason::Hls(HlsReason::EvidenceWindow));
            return Decision::Stay;
        }

        if self.pending.is_some() {
            return Decision::Stay;
        }
        // **An unknowable reserve decides nothing.** `buffered_ms` is `None` for exactly one
        // situation — an A/V session whose audio lane has produced no timestamp since the open or
        // the seek — and every branch below is keyed on the reserve: the emergency trigger reads it
        // as a level, the upshift gate as a depth, and §4's condition (2) as the excess it has to
        // cover. There is no honest value to substitute, and the one that WAS substituted (zero,
        // i.e. "empty") turned a full reserve into a downshift trigger.
        //
        // Staying is not a policy choice dressed as a guard: it is the absence of one. The
        // estimators above have already taken this segment, so the evidence is kept; only the
        // decision waits, and it waits at most until the audio lane produces a timestamp, which is
        // bounded by the first audio AU of the current segment.
        let Some(buffered) = sample.buffer.buffered_ms() else {
            self.last_reason = Some(DecisionReason::Hls(HlsReason::ReserveUnknown));
            return Decision::Stay;
        };
        if !completed {
            // An immediately displaced actuator is the only lower point known to have worked in
            // this playback, so prefer it. Without such a certificate, trying adjacent responses
            // minimizes quality loss but maximizes worst-case time-to-picture: every failed rung
            // spends another bounded transaction before reaching the one response that is no
            // larger than any other. The smallest feasible actuator is therefore the minimax
            // recovery action. This is order alone -- no guessed link rate or tuned threshold.
            let target = self
                .recovery_target
                .or(self.rollback_rung)
                .unwrap_or_else(|| self.recovery_floor());
            if target < self.current {
                let proposal = Proposal {
                    rung: target,
                    direction: Direction::Down,
                };
                self.pending = Some(proposal);
                self.pending_reserve_policy = Some(ReservePolicy::Preserve);
                self.last_reason = Some(DecisionReason::Hls(HlsReason::DeadlineRollback));
                return Decision::Prime(proposal);
            }
            self.last_reason = Some(DecisionReason::Hls(HlsReason::LadderFloor));
            return Decision::Stay;
        }
        // The dev pin (`pinned_to`) short-circuits the decision and NOTHING above it: every
        // estimator has already taken this segment. Reaching the pinned rung goes through the
        // ordinary prime/validate/commit transaction, so a pinned run exercises the real transport
        // path rather than a shortcut into it.
        if let Some(pin) = self.pin {
            if self.current == pin {
                return Decision::Stay;
            }
            let direction = if pin.kbps() > self.current.kbps() {
                Direction::Up
            } else {
                Direction::Down
            };
            // Wait for the direction-specific DEV-HARNESS reserve floor. Six segments keeps a
            // pinned upshift's inline initial transaction and possible staged repeatable phase
            // out of the measured re-proposal livelock. A downshift has no repeatable candidate
            // phase and its live media deadline is derived later from current reserve/transfer
            // evidence, so it uses the smaller tool precondition. Charging the upshift gate downward
            // is unsatisfiable at the top of the ladder — 12 000 ms against a `B_max(20000)` of
            // ~5 421 ms — which silently cost the M4 census five of its seven points. See
            // PIN_MIN_RESERVE_SEGMENTS and PIN_MIN_RESERVE_SEGMENTS_DOWN.
            let required = match direction {
                Direction::Up => PIN_MIN_RESERVE_SEGMENTS,
                Direction::Down => PIN_MIN_RESERVE_SEGMENTS_DOWN,
            };
            if buffered < segment.saturating_mul(required) {
                return Decision::Stay;
            }
            let proposal = Proposal {
                rung: pin,
                direction,
            };
            self.pending = Some(proposal);
            self.pending_reserve_policy = Some(ReservePolicy::Preserve);
            return Decision::Prime(proposal);
        }

        // The current operating point is judged by conservation of the media already observed,
        // and by nothing inferred from a demand-capped response. For the finite episode:
        //
        //   sustainable  <=>  sum A_i <= sum D_i
        //   survivable   <=>  B >= max_i(sum_{j<i}(A_j-D_j) + A_i)
        //
        // A failed first condition says this completed finite episode did not replenish itself.
        // While the second still holds, its conservative delivery and reserve evidence can order
        // the LOWER responses: choose the highest one their existing model supports,
        // then require that candidate's own exact acquisition before commit. A failed second
        // condition cannot replay the chronology actually observed, so it goes straight to the
        // smallest feasible time-to-picture response just like a censored prefix.
        // There is no new rate multiplier, dwell, margin, buffer heuristic or guessed
        // link-capacity target here.
        self.last_emergency_horizon = None;
        let current_physics = self
            .acquisitions
            .observed_ordered_admission(buffered)
            .expect("a completed sample seeded the current acquisition bag");
        if !current_physics.sustainable || !current_physics.survivable {
            // `B < runway` is already a time-to-picture emergency: replaying this completed bag
            // no longer guarantees its next media credit under the observed chronology, so
            // spending another transaction on a quality-preserving guess weakens the exact
            // guarantee. Only the sustainable-failure arm has enough runway to validate the
            // model-ordered lower candidate first.
            let target = if current_physics.survivable {
                self.completed_recovery_target(safe_budget, buffered)
            } else {
                self.recovery_floor()
            };
            if target == self.current {
                self.last_reason = Some(DecisionReason::Hls(HlsReason::LadderFloor));
                return Decision::Stay;
            }
            let proposal = Proposal {
                rung: target,
                direction: Direction::Down,
            };
            self.pending = Some(proposal);
            self.pending_reserve_policy = Some(if current_physics.survivable {
                ReservePolicy::Preserve
            } else {
                ReservePolicy::TerminalFloor
            });
            self.last_reason = Some(DecisionReason::Hls(if !current_physics.survivable {
                HlsReason::BufferConstraint
            } else {
                HlsReason::UnsafeCurrentState
            }));
            return Decision::Prime(proposal);
        }

        // A finite response at the current rung is a lower bound on service, never an upper bound
        // on what a larger response can obtain. The experiment is funded only from reserve above
        // the current finite-bag runway; that conservation budget replaces both the former dwell
        // timer and the scaled-current-response prefilter.
        if self.exploration_budget_ms(buffered).is_none() {
            self.last_reason = Some(DecisionReason::Hls(HlsReason::EvidenceWindow));
            return Decision::Stay;
        }
        // Scan the whole feasible ladder. With no failed response-size endpoint, the highest
        // unknown point has the same bounded loss as an adjacent one: both stop at the same reserve
        // floor. Once an endpoint has failed at this physical budget, linearly walking down from it
        // maximizes the number of encoder transactions. Split the ordinal interval instead. This
        // is minimax search over the finite actuator set, not a bitrate estimate; the candidate is
        // still accepted only from its own completed acquisition below.
        let exploration_budget = self
            .exploration_budget_ms(buffered)
            .expect("the surplus gate above admitted exploration");
        // The common frontier covers the two results which cannot order untouched REQUEST
        // actuators: a deadline-censored transaction with no complete media, and a completed PMS
        // underfill with no Pareto gain over the live response. Starting another encoder merely
        // by changing the request ceiling repeats the same failed experiment and, on a real PMS,
        // leaves enough overlapping resource state to make later decisions under-grant. Only a
        // physically observed surplus beyond the failed start budget releases quality
        // exploration. Its drawdown already reduced the live surplus and is replaced on the way
        // back to that start budget; adding it here would charge the same debt twice.
        if self.common_budget_blocks(Some(exploration_budget)) {
            self.last_reason = Some(DecisionReason::Hls(HlsReason::RejectBackoff));
            return Decision::Stay;
        }
        if !self
            .catalog
            .feasible()
            .any(|candidate| candidate.rung > self.current)
        {
            if self.active_variant_needs_refresh() {
                self.last_reason = Some(DecisionReason::Hls(HlsReason::ResponseLimited));
                if self.active_variant_refresh_released()
                    && !self.failures[self.current.index()]
                        .blocks(Some(exploration_budget), &self.delivery)
                {
                    // A fresh encoder at the SAME request is the missing excitation. Mapping the
                    // response back to a guessed lower rung would invent an inverse PMS does not
                    // have; treating `current == target` as terminal repeats the original bug.
                    let proposal = Proposal {
                        rung: self.current,
                        direction: Direction::Up,
                    };
                    self.pending_exploration_budget_ms = exploration_budget;
                    self.pending = Some(proposal);
                    self.pending_reserve_policy = Some(ReservePolicy::Preserve);
                    return Decision::Prime(proposal);
                }
                return Decision::Stay;
            }
            self.last_reason = Some(DecisionReason::Hls(HlsReason::AtBestRung));
            return Decision::Stay;
        }
        let service_ceiling = self
            .catalog
            .feasible()
            .filter(|candidate| candidate.rung > self.current)
            .filter(|candidate| {
                self.failures[candidate.rung.index()]
                    .service_blocks(Some(exploration_budget), &self.delivery)
            })
            .map(|candidate| candidate.rung)
            .min();
        let eligible = || {
            self.catalog
                .feasible()
                .filter(|candidate| candidate.rung > self.current)
                .filter(|candidate| {
                    !self.failures[candidate.rung.index()]
                        .blocks(Some(exploration_budget), &self.delivery)
                })
                .map(|candidate| candidate.rung)
        };
        let eligible_below_ceiling =
            || eligible().filter(|rung| service_ceiling.is_none_or(|ceiling| *rung < ceiling));
        // Once an actual response-size experiment establishes a service endpoint, prefer the
        // highest still-eligible actuator which the already-computed conservative delivery and
        // refill model supports. This is experiment ORDERING, not evidence
        // transferred from the demand-capped live response: the candidate below still has to
        // complete and pass its own exact `A <= D && B_post >= A` verdict. Before any endpoint
        // exists, retain the single maximum-information jump instead of manufacturing a staircase
        // of encoder reloads from a capped response.
        let modeled_target = service_ceiling.and_then(|_| {
            eligible()
                .filter(|rung| {
                    self.catalog.modeled_sustainable(
                        self.catalog.candidate(*rung),
                        safe_budget,
                        &self.policy,
                        buffered,
                    )
                })
                .max()
        });
        let ordinal_target = service_ceiling.and_then(|_| {
            let count = eligible_below_ceiling().count();
            (count > 0).then(|| {
                eligible_below_ceiling()
                    .nth((count - 1) / 2)
                    .expect("the counted ordinal midpoint exists")
            })
        });
        let target = if service_ceiling.is_some() {
            // The midpoint minimizes the worst-case number of remaining finite experiments. A
            // conservative modeled point may justify spending farther upward (including crossing
            // an old endpoint after fresh service evidence), but a demand-capped estimate must
            // not turn that minimax search into an adjacent-rung staircase.
            modeled_target.max(ordinal_target)
        } else {
            eligible().max()
        };
        let Some(target) = target else {
            self.last_reason = Some(DecisionReason::Hls(HlsReason::RejectBackoff));
            return Decision::Stay;
        };
        // **And there is no `stable_samples` here any more (N8).** Three consecutive samples on
        // which every conjunct above held was pure counting layered on a model that had already
        // passed every risk, budget and buffer condition, reset at seven separate
        // sites, and it was the dominant term in the opening seconds: counter spacing was exactly
        // five segments between successive upshifts and ten after a downshift.
        let proposal = Proposal {
            rung: target,
            direction: Direction::Up,
        };
        self.pending_exploration_budget_ms = exploration_budget;
        self.pending = Some(proposal);
        self.pending_reserve_policy = Some(ReservePolicy::Preserve);
        self.last_reason = Some(DecisionReason::Hls(HlsReason::SafeBudgetIncrease));
        Decision::Prime(proposal)
    }

    /// Candidate-session acceptance. The larger request is the excitation: only its own completed
    /// steady segment can identify that operating point. For one observation the conservation
    /// conditions reduce to `A <= D` and `B_post >= A`; neither contains a catalog-rate guess, a
    /// safety multiplier, or evidence transferred from a smaller demand-capped response.
    /// Downshifts remain recovery transactions and need only leave one decodable segment in hand.
    /// A losing intermediate rung must keep descending. The ladder floor is the terminal exception:
    /// rejecting it would retain an even more expensive current rung although no sustainable
    /// actuator exists, so accepting the floor means "best available", not "stable".
    pub(crate) fn candidate_verdict(
        &self,
        proposal: Proposal,
        sample: SegmentSample,
        _declared_bps: u64,
    ) -> CandidateVerdict {
        if self.pending != Some(proposal) || !sample.completed() {
            return CandidateVerdict::Incomplete;
        }
        // `TerminalFloor` is an explicit no-rollback contract: the current cursor's ordered
        // acquisition episode is no longer survivable and no cheaper actuator exists. Once the
        // floor returns a complete media object, rejecting it can only feed/abandon those bytes,
        // retain the more expensive old route, and request the same floor again. Completion is
        // therefore the terminal best-available certificate; transport/decoder failures remain
        // failures before this method is reached.
        if proposal.direction == Direction::Down
            && proposal.rung == self.recovery_floor()
            && self.pending_reserve_policy == Some(ReservePolicy::TerminalFloor)
        {
            return CandidateVerdict::Ready;
        }
        // An unreadable reserve refuses. It is the same answer an empty reserve gets, and that is
        // not a coincidence to paper over: this test asks whether the transaction can be paid for,
        // and a reserve that cannot be read cannot be shown to cover anything. It differs from the
        // old zero in what it does NOT do — the controller no longer proposes on an unknown
        // reserve at all, so reaching here with `None` means the lane fell silent mid-transaction.
        let Some(buffered) = sample.buffer.buffered_ms() else {
            return CandidateVerdict::ReserveUnknown;
        };
        let segment = i64::from(sample.media_duration_ms);
        let segment_obligation = i64::from(sample.media_obligation_ms());
        let acquisition_us = i64::try_from(sample.total_fetch_us()).unwrap_or(i64::MAX);
        let duration_us = segment.saturating_mul(1_000);
        match proposal.direction {
            Direction::Down
                if acquisition_us > duration_us && proposal.rung != self.recovery_floor() =>
            {
                CandidateVerdict::Unsustainable
            }
            Direction::Down if buffered >= segment_obligation => CandidateVerdict::Ready,
            Direction::Down => CandidateVerdict::Unfunded,
            Direction::Up => {
                // This is the operating point the transaction actually excited. Old, smaller HLS
                // responses are rollback evidence but cannot identify this request's unused tail,
                // so admission uses the candidate's completed acquisition directly. With one
                // sample the two conservation conditions reduce exactly to `T <= D` and
                // `B_post >= T`; neither contains a margin or a predicted capacity.
                if acquisition_us > duration_us {
                    CandidateVerdict::Unsustainable
                } else if buffered.saturating_mul(1_000) < acquisition_us {
                    CandidateVerdict::Unfunded
                } else {
                    CandidateVerdict::Ready
                }
            }
        }
    }

    /// Grade the structurally unique first object of a newly-created candidate encoder.
    ///
    /// A boundary object which already satisfies the ordinary conservation rule is immediately
    /// `Ready`.  If `A>D`, it cannot identify repeatable production because `A` contains the
    /// one-time PMS decision/session/JIT setup.  It may fund one observation from the now-running
    /// encoder only when the post-object reserve covers that exact measured `A`; otherwise the
    /// experiment is simply unfunded.  No count, dwell, tolerance or catalog-rate estimate enters
    /// this distinction.
    pub(crate) fn candidate_boundary_verdict(
        &self,
        proposal: Proposal,
        sample: SegmentSample,
        declared_bps: u64,
    ) -> CandidateVerdict {
        let verdict = self.candidate_verdict(proposal, sample, declared_bps);
        if verdict != CandidateVerdict::Unsustainable {
            return verdict;
        }
        // A down candidate may feed this completed media object, but A>D proves the intermediate
        // operating point still loses reserve. It must continue recovery rather than enter the
        // upshift-only setup-bearing steady-state phase.
        if proposal.direction == Direction::Down {
            return CandidateVerdict::Unsustainable;
        }
        let Some(buffered_ms) = sample.buffer.buffered_ms() else {
            return CandidateVerdict::ReserveUnknown;
        };
        let acquisition_us = i64::try_from(sample.total_fetch_us()).unwrap_or(i64::MAX);
        if buffered_ms.saturating_mul(1_000) >= acquisition_us {
            CandidateVerdict::SetupBearing
        } else {
            CandidateVerdict::Unfunded
        }
    }

    #[cfg(test)]
    pub(crate) fn candidate_ready(
        &self,
        proposal: Proposal,
        sample: SegmentSample,
        declared_bps: u64,
    ) -> bool {
        self.candidate_verdict(proposal, sample, declared_bps) == CandidateVerdict::Ready
    }

    /// Seed direct evidence from a committed candidate at the new operating point. [`Self::commit`]
    /// already retired the old bag; this method appends the completed candidate and is called for
    /// both directions. A rejected candidate never reaches either method, so the proven old bag is
    /// preserved for rollback.
    pub(crate) fn commit_candidate_evidence(&mut self, sample: SegmentSample) {
        if !sample.completed() {
            return;
        }
        self.acquisitions.observe(
            sample.bytes(),
            sample.total_fetch_us(),
            sample.active_fetch_us(),
            i64::from(sample.media_duration_ms()),
        );
        self.last_window = self
            .acquisitions
            .observed_readout(sample.buffer.buffered_ms().unwrap_or(0));
    }

    /// Validate, move the actuator and seed the new operating-point bag as one controller
    /// transition. Production uses this door so no observer can see the new rung with the old bag
    /// or with an empty bag between two calls.
    pub(crate) fn commit_candidate(
        &mut self,
        proposal: Proposal,
        sample: SegmentSample,
        variant: ObservedHlsVariant,
        now_ms: u64,
    ) -> bool {
        let response_improves = proposal.direction != Direction::Up
            || self
                .active_variant
                .is_some_and(|active| variant.strictly_dominates(active));
        if !response_improves
            || self.candidate_verdict(proposal, sample, variant.declared_bps)
                != CandidateVerdict::Ready
            || !self.commit(proposal, now_ms)
        {
            return false;
        }
        self.commit_candidate_evidence(sample);
        self.observe_active_variant(variant, sample.network_kbps());
        true
    }

    /// `now_ms` is the caller's clock at commit, after the control plane and candidate transfers.
    /// Failure release is reserve-based rather than time-based, but the controller still publishes
    /// one monotonic transaction timeline for diagnostics and mode-switch history.
    pub(crate) fn commit(&mut self, proposal: Proposal, now_ms: u64) -> bool {
        if self.pending != Some(proposal) {
            return false;
        }
        let previous = self.current;
        self.current = proposal.rung;
        if proposal.rung != previous {
            self.active_variant = None;
            self.active_variant_evidence_kbps = None;
        }
        self.pending = None;
        self.pending_reserve_policy = None;
        self.pending_exploration_budget_ms = 0;
        // A committed candidate is direct counter-evidence to the common no-endpoint frontier:
        // this serial path produced a Pareto-improving response. Future failures start a new
        // budget frontier in the new operating-point coordinates.
        self.common_budget_frontier_ms = None;
        self.samples_on_rung = 0;
        // Acquisition costs belong to one operating point. Retire the previous bag for BOTH
        // directions before any next observation can publish it as the new rung's runway; the
        // completed candidate is seeded immediately by the production caller below this commit.
        self.acquisitions.reset();
        self.last_window = self.acquisitions.observed_readout(0);
        // **The ceiling moved, so the reserve's units did.** `B_max` is inversely proportional to
        // the rung, so the next `buffered_ms` is measured against a different maximum and the
        // delta across this commit is a coordinate change rather than a flow. See
        // `BufferEstimate::rebase` for the device trace where differencing it withheld Original
        // recovery for an entire playback.
        self.buffer.rebase();
        self.last_buffer_observation_ms = None;
        self.rollback_rung = if proposal.direction == Direction::Up && proposal.rung > previous {
            Some(previous)
        } else {
            None
        };
        self.recovery_target = None;
        self.now_ms = self.now_ms.max(now_ms);
        // A completed commit is direct counter-evidence for this exact actuator. It says nothing
        // about failures retained for any other rung.
        self.failures[proposal.rung.index()] = FailureCertificate::default();
        true
    }

    /// **A reject now records what failed** (N11), where it recorded nothing and set a
    /// `cooldown = 1` that provably never blocked a segment.
    ///
    /// `cause` is the call site's own reading, and it decides whether a block is armed at all —
    /// see [`RejectCause`]. The block's two release conditions are computed HERE, from the state
    /// at the moment of failure, rather than re-derived later against numbers that have moved.
    pub(crate) fn reject(&mut self, proposal: Proposal, cause: RejectCause, now_ms: u64) -> bool {
        if self.pending != Some(proposal) {
            return false;
        }
        self.pending = None;
        self.pending_reserve_policy = None;
        self.now_ms = self.now_ms.max(now_ms);
        if cause == RejectCause::Circumstance {
            self.pending_exploration_budget_ms = 0;
            return true;
        }
        if proposal.direction == Direction::Down {
            // A recovery transaction that failed for a reason attached to the candidate must not
            // be proposed unchanged after the active stream aborts again. If it was the known-good
            // rollback point, the service episode has invalidated that knowledge; minimize the
            // remaining worst-case time-to-picture by trying the smallest feasible response.
            //
            // `StructuralAtOrBelow` is deliberately excluded. That refusal says every smaller
            // bounding box is at least as restrictive, so descending cannot repair it.
            if matches!(
                cause,
                RejectCause::Censored
                    | RejectCause::Candidate
                    | RejectCause::CompletedUnsustainable
                    | RejectCause::Structural
                    | RejectCause::ResponseUnchanged
            ) {
                let floor = self.recovery_floor();
                self.recovery_target = (floor < self.current).then_some(floor);
            }
            self.pending_exploration_budget_ms = 0;
            return true;
        }
        match cause {
            RejectCause::Censored | RejectCause::ResponseUnchanged => {
                self.common_budget_frontier_ms = Some(
                    self.common_budget_frontier_ms
                        .unwrap_or(0)
                        .max(self.pending_exploration_budget_ms),
                );
                let failure = &mut self.failures[proposal.rung.index()];
                failure.deadline_budget_ms = Some(
                    failure
                        .deadline_budget_ms
                        .unwrap_or(0)
                        .max(self.pending_exploration_budget_ms),
                );
                if cause == RejectCause::Censored {
                    failure.service_endpoint = true;
                }
            }
            RejectCause::Candidate => {
                let failure = &mut self.failures[proposal.rung.index()];
                failure.deadline_budget_ms = Some(
                    failure
                        .deadline_budget_ms
                        .unwrap_or(0)
                        .max(self.pending_exploration_budget_ms),
                );
                failure.service_endpoint = true;
            }
            RejectCause::CompletedUnsustainable => {
                let failure = &mut self.failures[proposal.rung.index()];
                failure.completed_unsustainable_hls_fast_kbps = Some(
                    failure
                        .completed_unsustainable_hls_fast_kbps
                        .unwrap_or(0)
                        .max(self.delivery.fast_kbps),
                );
            }
            RejectCause::Structural => {
                self.failures[proposal.rung.index()].structural = true;
            }
            RejectCause::StructuralAtOrBelow => {
                for rung in LADDER.iter().copied().filter(|rung| *rung <= proposal.rung) {
                    self.failures[rung.index()].structural = true;
                }
            }
            RejectCause::Circumstance => unreachable!("handled above"),
        }
        self.pending_exploration_budget_ms = 0;
        true
    }

    /// The reason attached to the LAST decision, for the invariant test that a `Stay` is never
    /// silent. `telemetry()` publishes the same value; this is the direct read, so a test does not
    /// have to build a whole telemetry snapshot to ask one question.
    #[cfg(test)]
    pub(crate) fn last_reason(&self) -> Option<DecisionReason> {
        self.last_reason
    }

    /// Is a transaction in flight. The steady line already reports this as `pending=<n>kbps`, so
    /// needs no reason code of its own; the invariant test skips it because the transaction has
    /// not reached a verdict yet.
    #[cfg(test)]
    pub(crate) fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
}
