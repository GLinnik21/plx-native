use super::*;

pub(crate) const SOURCE_PROBE_MIN_BYTES: usize = 512 * 1024;
pub(crate) const SOURCE_PROBE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// The finite source object a probe requests and the longest useful BODY time to wait for it.
///
/// The unclamped object is exactly one second of source media. Its sustainability question has a
/// coefficient-free deadline: if those bytes have not arrived within the amount of media they
/// represent, the response cannot establish `A <= D`. Connection setup is a distinct bounded
/// phase and is never subtracted from this interval. The byte clamps bound sampling noise and
/// memory, so their represented duration is recomputed rather than still called one second.
/// `max_budget_ms` is only the operational ceiling for a tiny source whose minimum sample spans
/// several seconds; waiting past it was already forbidden by [`AbrPolicy::probe_budget_ms`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SourceProbePlan {
    pub(crate) target_bytes: usize,
    pub(crate) budget_ms: u64,
}

pub(crate) fn source_probe_plan(source_kbps: u32, max_budget_ms: u64) -> Option<SourceProbePlan> {
    let rate = u64::from(source_kbps);
    if rate == 0 || max_budget_ms == 0 {
        return None;
    }
    let target_bytes = usize::try_from(rate)
        .unwrap_or(usize::MAX)
        .saturating_mul(125)
        .clamp(SOURCE_PROBE_MIN_BYTES, SOURCE_PROBE_MAX_BYTES);
    // kbps is bits/ms, so bits/kbps is already milliseconds. Round upward: truncating this
    // deadline would refuse a response at the exact conservation boundary.
    let represented_ms = u64::try_from(target_bytes)
        .unwrap_or(u64::MAX)
        .saturating_mul(8)
        .saturating_add(rate - 1)
        / rate;
    Some(SourceProbePlan {
        target_bytes,
        budget_ms: represented_ms.max(1).min(max_budget_ms),
    })
}

/// A runtime Original failure is not an unknown-link bootstrap: the direct transfer has just
/// measured the link. Start the replacement at the best actuator that measurement sustains, so a
/// 4 Mbit/s cap enters at 2 Mbit/s/720p rather than needlessly flashing the 240p emergency floor.
///
/// `catalog` carries this playback's feasibility bounds, so the answer can never be a raster the
/// device or the source cannot supply.
pub(crate) fn original_fallback_rung(
    measured_kbps: u32,
    catalog: &HlsActuatorCatalog,
    _policy: &AbrPolicy,
) -> Rung {
    catalog
        .best_for_budget(measured_kbps)
        .or_else(|| catalog.feasible().next())
        .map(|candidate| candidate.rung)
        .unwrap_or(Rung::P240)
}

/// The HLS entry point when an admitted Original request is refused before it produces a body.
///
/// This is deliberately not [`original_fallback_rung`] with a fabricated zero measurement.  No
/// transfer took place, so the refusal says nothing about link capacity.  Reuse the exact rung
/// [`bootstrap`] already computed while it still had the right evidence: Remote's completed source
/// probe, or Local's explicit unknown-link fallback.  In particular, the source bitrate is demand,
/// not capacity; turning a 28 Mbps file into a 28 Mbps connection claim would repeat the modelling
/// error this seam exists to remove.
///
/// The carried rung is still checked against the live catalog.  A stale/impossible value falls
/// back to the ordinary unknown-link bootstrap selection instead of bypassing device/source bounds.
pub(crate) fn original_open_fallback_rung(
    bootstrap_rung: Option<Rung>,
    catalog: &HlsActuatorCatalog,
    policy: &AbrPolicy,
) -> Rung {
    let fallback = catalog
        .best_for_budget(policy_startup_floor_kbps(policy))
        .or_else(|| catalog.feasible().next())
        .map(|candidate| candidate.rung)
        .unwrap_or(Rung::P480);
    bootstrap_rung
        .and_then(|rung| catalog.feasible().find(|candidate| candidate.rung == rung))
        .map(|candidate| candidate.rung)
        .unwrap_or(fallback)
}

/// **Cold-start Original admission, and only that.** The measured source prefix must complete and
/// arrive no slower than the file's average consumption rate.  That is a physical conservation
/// test: the prefix contributes media at least as quickly as playback removes it.  A finite prefix
/// is still only evidence about that prefix, so everything after admission remains an observed
/// trial under [`OriginalModeController`]; no invented multiplier turns it into a capacity claim.
pub(crate) fn original_sustainable(
    source_kbps: u32,
    measured_kbps: u32,
    complete: bool,
    _policy: &AbrPolicy,
) -> bool {
    source_kbps > 0 && complete && measured_kbps >= source_kbps
}

/// What a completed source probe settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryVerdict {
    /// The source request is sustainable with enough confidence AND Original is worth the visible
    /// switch for the playback that remains.
    Recover,
    /// Sustainable on this evidence, but the switch does not pay for itself — usually a film
    /// nearly over, or one visible switch too many already spent.
    NotWorthIt,
    /// The evidence does not clear the requirement yet. More probes may; that is the point of
    /// keeping the estimate rather than a success counter.
    Insufficient,
    /// The source experiment never produced a body. A PMS 5xx, DNS/connect failure or local
    /// transport refusal makes this attempt inconclusive; it does NOT prove that the same Part is
    /// unavailable to a later real playback open. The client keeps HLS selected without
    /// contaminating the source-capacity estimate with a fabricated zero-rate sample; continuity
    /// of PMS's cursor is established only by the next actual HLS response.
    ProbeFailed,
}

/// A completed source experiment re-scored after an upward HLS commit.
///
/// The rate travels with the verdict so the caller cannot accidentally latch a decision from one
/// probe beside the number from another. Reconsideration performs no I/O and does not increment
/// the probe count: it applies the same utility comparison to retained exact source evidence and
/// the first ordinary observation from the newly committed HLS stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RetainedProbeDecision {
    pub(crate) verdict: RecoveryVerdict,
    pub(crate) measured_kbps: u32,
}

/// **Explicit HLS→Original gate, on evidence about the SOURCE.**
///
/// HLS traffic can only establish a lower bound on service while its request is finite.  Recovery
/// therefore lets useful HLS traffic exercise the full feasible ladder, then performs the one
/// missing experiment: a bounded request for actual source bytes. PMS cannot account that raw Part
/// as a fresh AdHoc resource without re-running server admission, so the request exact-reuses the
/// live HLS identity while the media worker is between HLS acquisitions. Because PMS may rebind
/// that shared resource, a successful result leaves on the same media boundary. An insufficient
/// result is repeated only after HLS establishes a strictly stronger confidence-separated link
/// bound, so it neither competes with playback traffic nor loops on an arbitrary spacing counter.
/// **Which of `probe_due`'s conditions said no.** Not a policy input — a name for the log.
///
/// It is that function's ERROR TYPE rather than a field it publishes, and the difference is not
/// cosmetic. It was both at once: `probe_due` returned `bool` AND wrote the reason to a
/// `last_block` field the caller read back, so `last_block.is_none()` and "it returned true" were
/// two channels carrying one answer. They went out of step exactly when a probe SUCCEEDED — the
/// gate cleared its copy, `ff.rs`'s change-detecting latch kept the old reason, and the next
/// refusal for that same reason printed nothing at all. `Result<(), ProbeBlock>` makes the two
/// inseparable, and an early return that forgets to publish the reason stops compiling.
///
/// The gate is now a conjunction of three observable conditions and contains no spacing timer:
/// enough reserve to pay both bounded source-transfer phases while preserving the next ordinary
/// HLS acquisition, a reserve that is not draining, and no larger useful HLS request left to try.
/// From outside those failures would otherwise all look identical, so the typed result keeps
/// every refusal legible in the event log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProbeBlock {
    /// Reserve shorter than source setup + source body + the current HLS continuity boundary, or
    /// unreadable. A probe spends a reserve it cannot see, and one that cannot outlast those
    /// serial obligations causes the starvation it looks for.
    ShallowReserve,
    /// The reserve is draining, so the link is not currently paying for what is playing.
    Draining,
    /// HLS has not yet exercised the highest feasible rendition.  Let useful playback traffic do
    /// that first; a source probe is the one experiment HLS cannot perform for free.
    BelowHlsCeiling,
    /// Healthy, but a successful probe would not change the decision.
    NotWorthIt,
    /// Re-reading the same source prefix would add no fact. Either an insufficient experiment has
    /// not yet been released by a confidence-separated stronger HLS regime, or retained completed
    /// source evidence is awaiting/already exhausted its comparison at this HLS operating point.
    NoNewLinkEvidence,
}

impl ProbeBlock {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ProbeBlock::ShallowReserve => "shallow_reserve",
            ProbeBlock::Draining => "draining",
            ProbeBlock::BelowHlsCeiling => "below_hls_ceiling",
            ProbeBlock::NotWorthIt => "not_worth_it",
            ProbeBlock::NoNewLinkEvidence => "no_new_link_evidence",
        }
    }
}

/// Physical authorization returned by [`OriginalRecovery::probe_due`].  Carry the exact finite
/// object and phase deadline the gate funded so the transfer cannot silently re-derive a
/// different experiment at the call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProbePermit {
    pub(crate) plan: SourceProbePlan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceProbeState {
    /// No source experiment has been made in this HLS worker.
    Fresh,
    /// A previous experiment was insufficient. Retry only when the HLS conservative bound rises
    /// above both the source result and HLS's own recent estimate at that experiment.
    AwaitHlsAbove(u32),
    /// The source was selected or was not worth a reload. Neither conclusion becomes more
    /// informative by repeating the same request at this HLS operating point.
    Terminal,
    /// An upward HLS commit invalidated only the counterfactual side of the last mode comparison.
    /// Keep the completed source evidence, wait for one ordinary observation from the new live
    /// stream, then score the same source result against that operating point without another
    /// request. The `Direction::Down` branch instead retires terminal evidence and returns the gate
    /// to `Fresh`.
    ReconsiderAfterHlsCommit,
}

pub(crate) struct OriginalRecovery {
    source_kbps: u32,
    policy: AbrPolicy,
    /// Evidence about the source request specifically. Never seeded from HLS segments; see
    /// [`CapacityEstimate::demote_to_prior`].
    probe: CapacityEstimate,
    features: SourceFeatures,
    /// The switch history AS CAPTURED on the main thread when this worker started. It is the
    /// BASE, never mutated: [`OriginalRecovery::advance_to`] carries the worker's own clock and
    /// the two are combined at the point of use, so advancing cannot double-count however often
    /// the caller ticks it.
    history: TransitionHistory,
    /// Wall time since that capture. The visible-switch penalty HALVES every
    /// `visible_switch_decay_ms`, which is the whole of this controller's hysteresis — and until
    /// this field existed the caller passed a literal 0, so `since_last_ms` never advanced, the
    /// penalty never decayed, and Original stayed unreachable for the rest of a playback after
    /// two mode switches. The decay RATE is policy and is unchanged; this is only the clock that
    /// drives it, which was stopped.
    elapsed_ms: u64,
    probes: u8,
    source_probe_state: SourceProbeState,
    /// The last comparison this gate made, whole, for the event log — see [`ModeComparison`].
    /// Written only by a decision over completed source evidence: [`Self::observe_probe`] or
    /// [`Self::reconsider_after_hls_commit`]. `worth_probing` asks a hypothetical ("would a GOOD
    /// probe change anything") and publishing that beside a real decision is how a log acquires
    /// two numbers where one of them decided nothing.
    last_comparison: Option<ModeComparison>,
    /// **This playback's actuator set** — carried so both halves of the comparison can be scored
    /// on real alternatives (N14) and so Original's own quality can be scored against the SOURCE
    /// raster, which `HlsActuatorCatalog::source_raster` already holds. The catalog is `Copy` and
    /// the worker already owns one; taking it here beats adding a raster parameter to three
    /// methods and beats a second copy of a bound that can disagree with the ladder it bounds.
    catalog: HlsActuatorCatalog,
}

impl OriginalRecovery {
    pub(crate) fn new(
        source_kbps: u32,
        policy: AbrPolicy,
        features: SourceFeatures,
        history: TransitionHistory,
        catalog: HlsActuatorCatalog,
    ) -> Option<Self> {
        (source_kbps > 0).then_some(Self {
            source_kbps,
            policy,
            probe: CapacityEstimate::default(),
            features,
            history,
            elapsed_ms: 0,
            probes: 0,
            source_probe_state: SourceProbeState::Fresh,
            last_comparison: None,
            catalog,
        })
    }

    /// Advance the switch-penalty clock to `elapsed_ms` since construction. ABSOLUTE, not a
    /// delta, so calling it every segment and calling it once are the same thing — the caller
    /// owns the clock (this module is integer-only and deterministic by contract) and cannot
    /// corrupt the decay by ticking at an irregular rate.
    pub(crate) fn advance_to(&mut self, elapsed_ms: u64) {
        self.elapsed_ms = elapsed_ms;
    }

    pub(crate) fn probes(&self) -> u8 {
        self.probes
    }

    /// **The comparison the verdict was actually taken on**, for the log — the estimate's central
    /// value, its uncertainty, how many probes are behind it, the discounted rate that is compared,
    /// and the requirement it is compared against.
    ///
    /// Without it the line reads `measured=42012kbps ... verdict=Insufficient` against a 25 Mbit/s
    /// file, which is not merely terse but actively misleading: it invites the reading that 42
    /// Mbit/s was judged too slow for a 25 Mbit/s source, when what happened is that
    /// `conservative_kbps` was 29 689 against a requirement of 34 106. The operative numbers were
    /// unreconstructable from the event log, and this gate is precisely where a run gets stuck —
    /// seven consecutive refusals on a healthy link, with nothing on the line to say which
    /// quantity was short. `[[silent-instrument-trap]]`: the instrument has to be able to show the
    /// thing before its silence means anything.
    pub(crate) fn basis(&self) -> (u32, u32, u32, u32, u32) {
        (
            self.probe.slow_kbps,
            self.probe.uncertainty_pm,
            self.probe.samples,
            self.probe.conservative_kbps(),
            source_requirement_kbps(self.source_kbps, &self.policy),
        )
    }

    /// The basis of the decision THIS probe reached, for one log line. `None` whenever the last
    /// probe did not reach one — before the first, and after either of `observe_probe`'s two early
    /// exits (`!observation.completed`, and a conservative rate under the requirement), which is
    /// why that function clears this on entry rather than only writing it on success. Saying "no
    /// comparison" beats publishing a stale one, and the difference is invisible from the log:
    /// `ff.rs` emits `abr: mode` on every probe result, so a value left behind reads as a decision
    /// that had just been taken.
    pub(crate) fn comparison(&self) -> Option<ModeComparison> {
        self.last_comparison
    }

    /// Invalidate a terminal mode comparison when a candidate HLS stream commits.
    ///
    /// An upward commit changes only the HLS counterfactual, so the completed source lower bound
    /// remains evidence and the next ordinary HLS object re-scores it. A downward commit is itself
    /// evidence that the previous service regime did not sustain the old operating point. The old
    /// source rate cannot be projected across that boundary, so it is retired and the ordinary
    /// fully-funded source gate may authorize a fresh bounded probe. Neither branch invents a
    /// timeout, margin or capacity estimate.
    pub(crate) fn on_hls_commit(&mut self, direction: Direction) {
        if self.source_probe_state != SourceProbeState::Terminal {
            return;
        }
        self.last_comparison = None;
        match direction {
            Direction::Up => {
                self.source_probe_state = SourceProbeState::ReconsiderAfterHlsCommit;
            }
            Direction::Down => {
                self.probe = CapacityEstimate::default();
                self.source_probe_state = SourceProbeState::Fresh;
            }
        }
    }

    /// Re-score retained completed source evidence against the newly observed HLS operating point.
    /// No timer, margin or network request is involved.
    pub(crate) fn reconsider_after_hls_commit(
        &mut self,
        current: HlsCandidate,
        production: &ProductionEstimate,
        buffer: BufferEstimate,
        hls_delivery: &CapacityEstimate,
        remaining_ms: i64,
    ) -> Option<RetainedProbeDecision> {
        if self.source_probe_state != SourceProbeState::ReconsiderAfterHlsCommit {
            return None;
        }
        let measured_kbps = self.probe.slow_kbps;
        debug_assert!(
            self.probe.samples > 0 && measured_kbps > 0,
            "only a completed source decision can await HLS reconsideration"
        );
        let verdict = self.decide_from_completed_probe(
            current,
            production,
            buffer,
            hls_delivery,
            remaining_ms,
        );
        Some(RetainedProbeDecision {
            verdict,
            measured_kbps,
        })
    }

    /// Would a SUCCESSFUL probe change anything? Asked before spending one, because a probe reads
    /// real media bytes over the link the segments need. Answered with the utility comparison
    /// under an assumed-good outcome, so "twenty seconds left" and "already switched three times"
    /// stop the measurement rather than being discovered after paying for it.
    fn worth_probing(
        &self,
        current: HlsCandidate,
        production: &ProductionEstimate,
        buffer: BufferEstimate,
        hls_delivery: &CapacityEstimate,
        remaining_ms: i64,
    ) -> bool {
        let requirement = source_requirement_kbps(self.source_kbps, &self.policy);
        // Ask the value-of-information question at the weakest result that could actually recover
        // Original: one completed finite response at exactly the source requirement.  `2 * R` was
        // an arbitrary optimistic point and could buy a probe whose minimally successful outcome
        // would still lose the utility comparison.  This is the same evidence shape
        // `observe_probe` constructs after a completed request, with no invented headroom.
        let assumed = CapacityEstimate {
            fast_kbps: requirement,
            slow_kbps: requirement,
            uncertainty_pm: 0,
            samples: 1,
        };
        let inputs = self.inputs(assumed, *production, buffer, *hls_delivery, remaining_ms);
        // **The value-of-information gate has to score the decision it gates** (N14 site 2). Both
        // arguments were `current`, so it asked "is Original better than STAYING HERE" while the
        // decision it guards asks "is Original better than the BEST rung this link supports" —
        // and the app spent real source probes, over the link the segments need, on questions the
        // decision had already settled the other way.
        let best = self.best_hls(current, buffer, hls_delivery);
        let (mode, _, _, _) = choose_mode(&inputs, current, best, &self.policy);
        mode == ModeKind::Original
    }

    /// **The HLS alternative the mode comparison is actually against**: the best counterfactual
    /// rung the measured delivery and reserve model currently support. The live
    /// HLS actuator does not use this prediction as an upshift ceiling; it spends measured surplus
    /// on a real candidate and grades that candidate directly. Here a prediction is unavoidable
    /// because the question is whether a visible source reload is worth replacing the HLS mode.
    ///
    /// Falls back to `current` when nothing is sustainable, which is the honest answer: the thing
    /// Original would be replacing is the thing that is playing.
    fn best_hls(
        &self,
        current: HlsCandidate,
        buffer: BufferEstimate,
        hls_delivery: &CapacityEstimate,
    ) -> HlsCandidate {
        self.catalog
            .best_sustainable(
                hls_safe_budget(hls_delivery),
                &self.policy,
                buffer.buffered_ms,
            )
            .unwrap_or(current)
    }

    /// **Both sides of the comparison get real evidence.** `hls_delivery` was an empty estimate
    /// here for one afternoon, which made the HLS candidate look starved (an empty estimate is zero
    /// capacity, so every candidate has a finite starvation horizon) and quietly biased every
    /// recovery decision toward Original. The bias happened to point the way the tests wanted,
    /// which is exactly why it survived a green suite: a comparison is only meaningful if both
    /// sides are scored on what was actually measured.
    fn inputs(
        &self,
        source_delivery: CapacityEstimate,
        production: ProductionEstimate,
        buffer: BufferEstimate,
        hls_delivery: CapacityEstimate,
        remaining_ms: i64,
    ) -> ModeInputs {
        ModeInputs {
            current: ModeKind::Hls,
            source_kbps: self.source_kbps,
            // The catalog was bounded by this playback's own source frame on the main thread; see
            // `HlsActuatorCatalog::source_raster`.
            source_raster: self.catalog.source_raster(),
            source_delivery,
            hls_delivery,
            // Preserve the real end-to-end acquisition telemetry rather than fabricating an idle
            // default. The ratio spans PMS wait, pacing and path transfer; mode utility does not
            // treat it as an independent server-load or feasibility gate.
            production,
            buffer,
            remaining_ms,
            history: self.history.advanced_by(self.elapsed_ms),
            original_feasible: true,
            source_dv: self.features.dv,
            source_atmos: self.features.atmos,
            // Recovery asks about a source that is NOT currently being read, so there is no live
            // deficit to have persisted. The probe estimate carries all the doubt there is.
            unsafe_deficit_ms: 0,
        }
    }

    /// Is this the moment to spend a source experiment?  It needs a reserve deep enough to
    /// outlast its own deadline, a reserve that is not draining, and the HLS experiment frontier
    /// exhausted. The latter is supplied by [`Controller`], which owns the per-actuator failure
    /// certificates. It is not equivalent to playing the largest requested rung: PMS may answer a
    /// larger request with the same smaller encode, making that rung structurally uncommittable
    /// while also leaving no useful HLS request to try.
    #[cfg(test)]
    pub(crate) fn probe_due(
        &mut self,
        current: HlsCandidate,
        hls_frontier_exhausted: bool,
        production: &ProductionEstimate,
        sample: SegmentSample,
        current_runway_ms: Option<i64>,
        buffer: BufferEstimate,
        hls_delivery: &CapacityEstimate,
        remaining_ms: i64,
        now_ms: u64,
    ) -> Result<ProbePermit, ProbeBlock> {
        self.probe_due_with_rollback(
            current,
            hls_frontier_exhausted,
            production,
            sample,
            Some(sample.media_duration_ms()),
            current_runway_ms,
            buffer,
            hls_delivery,
            remaining_ms,
            now_ms,
        )
    }

    pub(crate) fn probe_due_with_rollback(
        &mut self,
        current: HlsCandidate,
        hls_frontier_exhausted: bool,
        production: &ProductionEstimate,
        sample: SegmentSample,
        rollback_media_ms: Option<u32>,
        current_runway_ms: Option<i64>,
        buffer: BufferEstimate,
        hls_delivery: &CapacityEstimate,
        remaining_ms: i64,
        _now_ms: u64,
    ) -> Result<ProbePermit, ProbeBlock> {
        // An unreadable reserve is not a deep one. A probe spends the reserve it cannot
        // see, which is the one thing this gate exists to prevent.
        //
        // Three serial obligations are funded before a source probe which does not switch modes:
        //
        //   source setup + source body + max(current stress boundary R_s,
        //                                    exact next HLS horizon D_next).
        //
        // The source request exact-reuses the live HLS Streaming Resource, so the client issues no
        // HLS stop/close/restart transaction. That does not prove PMS preserves the prior HLS
        // cursor: a successful Recover is therefore published on this same completed media
        // boundary and performs no later HLS GET. For any result which retains HLS, `D_next` funds the
        // next response if PMS continues the resource. The two `P`s are not a safety multiplier:
        // setup and body have two separately enforced deadlines from one [`SourceProbePlan`]. Once
        // that response completes, `D_next-A >= 0`, so the same balance restores `R_s`. Adding
        // `R_s` and `D_next` would charge opposite sides of one media credit twice.
        let funding = source_probe_plan(self.source_kbps, self.policy.probe_budget_ms)
            .zip(current_runway_ms)
            .zip(rollback_media_ms)
            .zip(sample.buffer.buffered_ms())
            .map(|(((plan, runway_ms), rollback_media_ms), buffered_ms)| {
                let continuity_ms = runway_ms.max(0).max(i64::from(rollback_media_ms));
                let required_ms = i64::try_from(plan.budget_ms)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(2)
                    .saturating_add(continuity_ms);
                (plan, required_ms, buffered_ms)
            });
        let deep_reserve =
            funding.is_some_and(|(_, required_ms, buffered_ms)| buffered_ms >= required_ms);
        let refilling = !buffer.draining();
        if !(deep_reserve && refilling && hls_frontier_exhausted) {
            return Err(if !deep_reserve {
                ProbeBlock::ShallowReserve
            } else if !refilling {
                ProbeBlock::Draining
            } else {
                ProbeBlock::BelowHlsCeiling
            });
        }
        match self.source_probe_state {
            SourceProbeState::Fresh => {}
            SourceProbeState::AwaitHlsAbove(floor_kbps)
                if hls_delivery.conservative_kbps() > floor_kbps => {}
            SourceProbeState::AwaitHlsAbove(_)
            | SourceProbeState::Terminal
            | SourceProbeState::ReconsiderAfterHlsCommit => {
                return Err(ProbeBlock::NoNewLinkEvidence);
            }
        }
        if !self.worth_probing(current, production, buffer, hls_delivery, remaining_ms) {
            return Err(ProbeBlock::NotWorthIt);
        }
        let plan = funding
            .map(|(plan, _, _)| plan)
            .ok_or(ProbeBlock::ShallowReserve)?;
        Ok(ProbePermit { plan })
    }

    pub(crate) fn observe_probe(
        &mut self,
        observation: CapacityObservation,
        current: HlsCandidate,
        production: &ProductionEstimate,
        buffer: BufferEstimate,
        hls_delivery: &CapacityEstimate,
        remaining_ms: i64,
    ) -> RecoveryVerdict {
        self.begin_probe_attempt();
        // **Retire the previous probe's comparison before this one can fail.** Both exits below
        // return without reaching `choose_mode`, and `ff.rs` logs `abr: mode` off `comparison()`
        // on EVERY probe result — so leaving the old value in place printed a decision that was
        // made two probes ago immediately above this probe's `verdict=Insufficient`, with nothing
        // marking it stale, and `RE_ABR_MODE` parsed it as a decision that had just been taken.
        // The doc on `comparison()` already promised this ("saying so beats publishing a stale
        // one"); it was true only because the test that pinned it truncated the FIRST probe, when
        // there was nothing stale to publish yet.
        if !observation.completed {
            // A truncated probe is not a slow link — it is an absent measurement, and folding it
            // into the estimate as a low rate would poison the next decision with a number no
            // transfer ever sustained.
            self.await_stronger_hls(0, hls_delivery);
            return RecoveryVerdict::Insufficient;
        }
        // A completed finite response is a LOWER bound on available service.  A lower bound above
        // the source requirement is sufficient evidence; discounting it as though it were an
        // uncertain point estimate is what made 36-45 Mbit/s probes fail a 34 Mbit/s requirement
        // forever.  Keep the exact lower bound for the comparison and do not average it with a
        // request of another size or era.
        self.probe = CapacityEstimate {
            fast_kbps: observation.kbps,
            slow_kbps: observation.kbps,
            uncertainty_pm: 0,
            samples: 1,
        };
        let requirement = source_requirement_kbps(self.source_kbps, &self.policy);
        if observation.kbps < requirement {
            self.await_stronger_hls(observation.kbps, hls_delivery);
            return RecoveryVerdict::Insufficient;
        }
        self.decide_from_completed_probe(current, production, buffer, hls_delivery, remaining_ms)
    }

    /// Apply the one mode comparison shared by a fresh completed probe and its post-commit
    /// reconsideration. Keeping this as one transition prevents the two paths from acquiring
    /// different utility rules while claiming to reuse the same evidence.
    fn decide_from_completed_probe(
        &mut self,
        current: HlsCandidate,
        production: &ProductionEstimate,
        buffer: BufferEstimate,
        hls_delivery: &CapacityEstimate,
        remaining_ms: i64,
    ) -> RecoveryVerdict {
        let inputs = self.inputs(self.probe, *production, buffer, *hls_delivery, remaining_ms);
        // **The whole HLS side of the argmax was fabricated** (N14 site 1): both arguments were
        // `candidate(P1080High)`, a rung this playback may not be on, may not be able to reach, and
        // may not even have in its catalog — so the decision to tear down an encoder and reload was
        // taken against an alternative that did not exist. Every input needed to score the real one
        // is on the demux worker's stack at the call site.
        let best = self.best_hls(current, buffer, hls_delivery);
        let (mode, reason, winner, loser) = choose_mode(&inputs, current, best, &self.policy);
        self.last_comparison = Some(ModeComparison {
            chosen: mode,
            reason,
            winner,
            loser,
            hls_rung: best.rung,
            scale_pm: benefit_scale_pm(remaining_ms, &self.policy),
        });
        self.source_probe_state = SourceProbeState::Terminal;
        if mode == ModeKind::Original {
            RecoveryVerdict::Recover
        } else {
            RecoveryVerdict::NotWorthIt
        }
    }

    /// Record a source experiment which failed before any response body existed. It contributes no
    /// delivery evidence: HTTP 5xx and transport setup failures describe this request attempt, not
    /// the Part's future availability and not zero throughput.
    pub(crate) fn observe_probe_failed(
        &mut self,
        hls_delivery: &CapacityEstimate,
    ) -> RecoveryVerdict {
        self.begin_probe_attempt();
        self.await_stronger_hls(0, hls_delivery);
        RecoveryVerdict::ProbeFailed
    }

    fn await_stronger_hls(&mut self, source_kbps: u32, hls_delivery: &CapacityEstimate) {
        // This is a confidence-separation test, not a margin. `fast_kbps` is HLS's recent central
        // estimate; requiring a later conservative bound to exceed it means the two regimes no
        // longer overlap on the side relevant to source sustainability. The source lower bound is
        // included too, so a high-but-insufficient source result cannot be retried on weaker HLS.
        self.source_probe_state =
            SourceProbeState::AwaitHlsAbove(source_kbps.max(hls_delivery.fast_kbps));
    }

    fn begin_probe_attempt(&mut self) {
        self.probes = self.probes.saturating_add(1);
        self.last_comparison = None;
    }
}

/// One measurement window of the live progressive transfer. 750 ms of ACTIVE body-read time, not
/// wall clock: a reader parked on backpressure with a full buffer is the healthy case and must not
/// be measured as a slow link.
pub(crate) const ORIGINAL_WINDOW_US: u64 = 750_000;
/// **Retired by N13.** Kept as `#[cfg(test)]` because two tests derive their expectations from it,
/// which is the point: the new duration must be *the same rule on the right clock*, and a test that
/// says so has to be able to name the old one.
///
/// Six windows of [`ORIGINAL_WINDOW_US`] ACTIVE BODY-READ time was the rule, and its own two doc
/// comments disagreed about the unit — "about four and a half seconds of real transfer" in one
/// place and "about nine seconds" of wall clock in another, for the same counter. Under
/// backpressure, the healthy full-buffer case, a 750 ms active window spans unbounded wall time, so
/// the second reading was not merely imprecise: the rule named no duration at all.
///
/// `AbrPolicy::sustained_unsafe_deficit_ms` carries the same 4 500 ms onto the WALL clock. The
/// conversion is deliberately 1:1 in the number and **is not 1:1 in the world** — under
/// backpressure the wall interval is longer, so the new rule is at least as patient as the old one
/// and usually more so. Which is the safe direction (it delays a visible switch, never hastens
/// one), and the observed ratio is an M2 measurement this project has not taken.
#[cfg(test)]
pub(crate) const ORIGINAL_DEFICIT_WINDOWS: u8 = 6;

/// Why Original was abandoned. Three distinct causes, logged by name, because the operator
/// question after a visible switch is always which of them fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OriginalExit {
    /// The reserve will not outlast the deficit: [`starvation_horizon`] is inside the policy's
    /// fallback band. Acted on WITHOUT consulting utility once the drain is confirmed, or at once
    /// when waiting for confirmation would spend the runway down to the emergency reserve — a
    /// stall is worse than any switch.
    ImminentStarvation,
    /// A deficit that has persisted for `AbrPolicy::sustained_unsafe_deficit_ms` and that the utility
    /// comparison agrees is worth a visible switch. This is the one hysteresis applies to.
    SustainedDeficit,
    /// The **labelled emergency guard**: reserve under the emergency floor and falling, whatever
    /// the estimates say. It exists because an estimator can be wrong in a way arithmetic cannot,
    /// it should be unreachable when the model works, and its firing in a log is a finding about
    /// this module rather than about the network.
    EmergencyLowBuffer,
}

/// One completed runtime-Original measurement window, with the whole basis of its verdict attached
/// so the event log can print the reasoning and not just the outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OriginalObservation {
    pub(crate) measured_kbps: u32,
    pub(crate) conservative_kbps: u32,
    pub(crate) requirement_kbps: u32,
    pub(crate) buffered_ms: i64,
    pub(crate) slope_ms_per_s: i64,
    pub(crate) horizon_secs: Option<u32>,
    /// Consecutive windows inside the unsafe horizon band. Published to the diagnostics panel,
    /// where it replaced the old two-window counter and means the same thing to a reader: how long
    /// this has been going on.
    /// Wall milliseconds the unsafe condition has held, uninterrupted (N13). Was `bad_windows`, a
    /// count on a clock that stops under backpressure.
    pub(crate) unsafe_deficit_ms: i64,
    pub(crate) fallback: Option<OriginalExit>,
    /// The HLS state a fallback should land on — the best candidate the CURRENT estimate sustains,
    /// never the bottom of the ladder. `None` when nothing is feasible, which is also the only
    /// case where a fallback is refused outright: there is nowhere to go.
    pub(crate) target: Option<Rung>,
}

/// **Auto's Original state, governed by starvation risk rather than by slow windows.**
///
/// The transfer counters contain successful body-read time only, while `buffered_ms` is content
/// time on the one normalized movie timeline. Either signal alone lies: a shaped socket can look
/// slow while a large reserve makes it harmless, and a temporarily small queue can occur while a
/// fast reader is refilling it. What this controller does with them is turn the pair into
/// SECONDS — how long the reserve survives the measured deficit — which is the number that makes
/// "59 Mbit/s on a 60 Mbit/s file" (half an hour of headroom, stay) and "5 Mbit/s on the same
/// file" (ten seconds, go) different answers instead of the same "rate below requirement".
///
/// The rule it replaced was two 750 ms windows below 1.1x the average with under 3.5 s buffered.
/// That fired on a temporary dip with a minute of reserve in hand, and it read the whole-file
/// average as if it were the instantaneous demand.
pub(crate) struct OriginalModeController {
    source_kbps: u32,
    policy: AbrPolicy,
    catalog: HlsActuatorCatalog,
    pub(super) delivery: CapacityEstimate,
    pub(super) buffer: BufferEstimate,
    history: TransitionHistory,
    features: SourceFeatures,
    last_bytes: u64,
    last_active_us: u64,
    /// **Wall milliseconds the unsafe condition has held, uninterrupted** (N13). It was
    /// `deficit_windows: u8`, a count of 750 us-of-ACTIVE-BODY-READ windows — and under
    /// backpressure, which is the healthy full-buffer case, one such window spans unbounded wall
    /// clock. So "six windows" named no duration at all, and the module said so twice in two
    /// different units: one doc read it as "four and a half seconds of real transfer" (right for
    /// the active clock) and another as "about nine seconds" (a wall-clock reading of the same
    /// counter). The 750 us window survives as the SAMPLING rate; the policy is now a duration.
    pub(super) unsafe_deficit_ms: i64,
    /// True only after an unsafe endpoint has started an interval. The next unsafe endpoint may
    /// charge that known-unsafe wall span; the first may not relabel an unobserved gap retroactively.
    unsafe_deficit_active: bool,
    /// The wall clock at the previous window, so the accumulation above is a real elapsed
    /// difference rather than a count of windows wearing a millisecond suffix.
    last_now_ms: u64,
}

impl OriginalModeController {
    pub(crate) fn new(
        source_kbps: u32,
        policy: AbrPolicy,
        catalog: HlsActuatorCatalog,
        history: TransitionHistory,
        features: SourceFeatures,
    ) -> Option<Self> {
        (source_kbps > 0).then_some(Self {
            source_kbps,
            policy,
            catalog,
            delivery: CapacityEstimate::default(),
            buffer: BufferEstimate::default(),
            history,
            features,
            last_bytes: 0,
            last_active_us: 0,
            unsafe_deficit_ms: 0,
            unsafe_deficit_active: false,
            last_now_ms: 0,
        })
    }

    /// A seek keeps the DELIVERY estimate and discards everything positional. The link did not
    /// change because the viewer jumped; the buffer, the deficit history and the byte counters all
    /// describe a position that no longer exists.
    pub(crate) fn on_seek(&mut self, bytes: u64, active_us: u64) {
        self.last_bytes = bytes;
        self.last_active_us = active_us;
        self.unsafe_deficit_ms = 0;
        self.unsafe_deficit_active = false;
        self.buffer = BufferEstimate::default();
    }

    /// A pause is the one gap where wall-clock time passes with no measurement, so it is the one
    /// place staleness is real rather than backpressure. See [`CapacityEstimate::age_ms`].
    pub(crate) fn on_resume(&mut self, paused_ms: u64) {
        self.delivery.age_ms(paused_ms, &self.policy);
        self.buffer = BufferEstimate::default();
        self.unsafe_deficit_ms = 0;
        self.unsafe_deficit_active = false;
    }

    /// **Test-only: `observe` with the wall clock pinned to the ACTIVE-read clock.**
    ///
    /// The two coincide exactly when the reader is saturated — every millisecond of wall time is a
    /// millisecond of body read — and diverge only under backpressure, which is the healthy
    /// full-buffer case. Every fixture in `tests.rs` models a link that is SHORT, so the reader is
    /// never parked and this is the physically correct clock for them; it is also what lets N13's
    /// change preserve their expectations rather than re-fit them, because at wall == active the
    /// old six-window rule and the new 4 500 ms rule are the same rule.
    ///
    /// **A test about the divergence must call `observe` directly**, and one does — that is the
    /// whole of N13, and expressing it through this helper would assert the helper.
    #[cfg(test)]
    pub(crate) fn observe_saturated(
        &mut self,
        bytes: u64,
        active_us: u64,
        buffered_ms: Option<i64>,
        remaining_ms: i64,
    ) -> Option<OriginalObservation> {
        self.observe(
            bytes,
            active_us,
            buffered_ms,
            remaining_ms,
            active_us / 1_000,
        )
    }

    fn fallback_target(&self) -> Option<Rung> {
        self.catalog
            .best_for_budget(self.delivery.conservative_kbps())
            .or_else(|| self.catalog.feasible().next())
            .map(|candidate| candidate.rung)
    }

    /// `now_ms` is ABSOLUTE monotonic wall clock, for the reason [`Self::advance_history`]'s
    /// sibling `advance_to` documents: windows do not arrive on a regular cadence — that is the
    /// whole defect N13 names — so a delta API would make the policy depend on how often the caller
    /// happened to call.
    pub(crate) fn observe(
        &mut self,
        bytes: u64,
        active_us: u64,
        buffered_ms: Option<i64>,
        remaining_ms: i64,
        now_ms: u64,
    ) -> Option<OriginalObservation> {
        if bytes < self.last_bytes || active_us < self.last_active_us {
            self.on_seek(bytes, active_us);
            self.last_now_ms = now_ms;
            return None;
        }
        let active_delta = active_us - self.last_active_us;
        if active_delta < ORIGINAL_WINDOW_US {
            return None;
        }
        // Elapsed since the previous WINDOW, which is what the deficit accumulates in. Taken before
        // any early return below so a window that decides nothing still advances the clock.
        let wall_delta = i64::try_from(now_ms.saturating_sub(self.last_now_ms)).unwrap_or(0);
        self.last_now_ms = now_ms;
        let byte_delta = bytes - self.last_bytes;
        self.last_bytes = bytes;
        self.last_active_us = active_us;
        // The first window of a playback — and the first after a seek or a resume, both of which
        // reset `buffer` — does not count toward the SUSTAINED-deficit tally. `unsafe_horizon`
        // there is manufactured rather than observed: `uncertainty_pm` sits at its 500 pm floor on
        // the first capacity sample, so `conservative_kbps` is EXACTLY half the measurement, and
        // `buffered_ms` is whatever the prime left, which is ~0 by design. Both hard guards below
        // are already unable to fire on it (they require a measured drain, and there is no
        // derivative yet), so this is the one place the cold start still has to be said.
        let cold_start = self.buffer.samples == 0;
        let Some(buffered_ms) = buffered_ms else {
            // No timestamp on one lane yet. That IS the starvation this metric watches for, but it
            // is not yet evidence of it — an A/V session that has not produced both tails cannot
            // be told apart from one that never will.
            self.unsafe_deficit_ms = 0;
            self.unsafe_deficit_active = false;
            return None;
        };
        if byte_delta == 0 {
            self.unsafe_deficit_ms = 0;
            self.unsafe_deficit_active = false;
            return None;
        }
        let measured_kbps = kbps_from(byte_delta, active_delta).min(u64::from(u32::MAX)) as u32;
        let observation = CapacityObservation {
            kbps: measured_kbps,
            bytes: byte_delta,
            active_us: active_delta,
            completed: true,
        };
        if observation.is_collapse(&self.delivery) {
            self.delivery.collapse(measured_kbps);
        }
        self.delivery.update(observation);
        // **WALL, not active-read.** `slope_ms_per_s` is a rate of change of the reserve, and the
        // reserve is spent by the PLAYHEAD — one millisecond of media per millisecond of wall
        // clock. `active_delta` is body-read time, which is the right denominator for a CAPACITY
        // (a reader parked on backpressure must not measure as a slow link — that is
        // `ORIGINAL_WINDOW_US`'s whole point) and the wrong one for a reserve derivative: parking
        // is exactly what a healthy link does, so `t_active < t_wall` precisely when the reserve
        // is filling, and the slope came out inflated by `t_wall / t_active`.
        //
        // Device-measured 2026-08-29 on a 4K DV film: the line printed `slope=1020ms/s` while the
        // reserve grew 749 -> 4814 ms over 8 s of playback, i.e. +508 ms per wall second. A factor
        // of two, in the direction that reads as healthier than the truth — and `slope_ms_per_s`
        // is then compared against `DRAIN_EPS_MS_PER_S`, a threshold stated in wall seconds, and
        // sits beside `starvation_horizon`, which is wall seconds throughout.
        //
        // `observe_saturated` passes `now_ms = active_us / 1_000`, so every existing host test is
        // byte-identical across this change: there, wall and active ARE the same clock.
        self.buffer.update(Some(buffered_ms), wall_delta.max(1));

        let requirement = source_requirement_kbps(self.source_kbps, &self.policy);
        let conservative = self.delivery.conservative_kbps();
        // **The EVICTION horizon is computed on the MEASURED rate, not on `conservative_kbps` —
        // which is the rule `controller.rs` already states, follows and calls load-bearing, and
        // which this side was violating.** Its words, at the HLS emergency horizon:
        //
        // > Conservatism belongs to ADMISSION — a rung you have not tried might be dearer than you
        // > think, so plan against a lower bound. It does not belong to EVICTION, where the claim
        // > is that the link in front of you cannot carry what is already playing, and the
        // > evidence for that has to be observed rather than discounted into existence.
        //
        // `immediate` is the same construction that side uses (`immediate_network`): this window's
        // rate, floored by the fast estimate so one lucky burst cannot excuse a link that has
        // stopped delivering.
        //
        // **What the discount was doing here, in the device's numbers.** 25 264 kbps source,
        // R = 34 106. The live link measured 31 037 and the discount published 23 932, so the
        // model saw a 10 174 kbps deficit where the true one was 3 069 — 92 % of it composition.
        // Worse, it was PERMANENT: `T = B·R/(R−C)` is increasing in `B` and `B` is bounded by the
        // plant ceiling `B_max = lead + queue_bytes·8/R` ≈ 5.0 s for a source this size, so
        // `T_max = 5.0 × 34 106/10 174 = 16.8 s`, under `starvation_fallback_secs`. The imminent
        // branch's horizon half was satisfied on EVERY window the playback could ever produce,
        // saturated or not — the reserve's own physical ceiling could not buy its way out. On the
        // measured rate the same ceiling gives `T_max = 5.0 × 34 106/3 069 = 55 s`, and the branch
        // becomes reachable only when the link genuinely stops covering the file.
        //
        // That is why the derivative guards below are necessary and were not sufficient: they
        // close the channel through which the permanently-armed condition fired, and the condition
        // stays armed for the next one. `conservative` is still published as `safe=` and still
        // chooses the fallback RUNG, which is an admission decision and is where it belongs.
        let immediate = self.delivery.fast_kbps.min(measured_kbps);
        let horizon = starvation_horizon(buffered_ms, requirement, immediate);
        let unsafe_horizon = horizon
            .seconds
            .is_some_and(|secs| secs < self.policy.starvation_safe_secs);
        // **The same premise test the imminent branch carries, one level up — and it was missing
        // here, which is why fixing that branch alone only DELAYED the abandon by two seconds.**
        //
        // `unsafe_horizon` is `T < starvation_safe_secs` and `T = B·R/(R−C)` is a forecast under
        // one premise: that the reserve is being consumed at `(R−C)/R`. `R` is the whole-file
        // average and `C` is the measured transfer rate; neither describes instantaneous VBR
        // demand. The reserve derivative beside them therefore decides whether this arithmetic is
        // a live forecast or only an average-rate comparison.
        //
        // So the tally requires the drain to be OBSERVED, exactly as the imminent branch requires
        // it. Without this the accumulator ran for six consecutive windows with the reserve rising
        // at +783 ms/s, reached `sustained_unsafe_deficit_ms` on the seventh, and handed the
        // decision to `choose_mode` — a second, slower route to the same wrong reload.
        //
        // **`draining()` and not "not filling"**, because the reserve saturates. `B_max` is the
        // queue caps plus the pump's feed-ahead lead, and a healthy stream sits AT that ceiling
        // with a slope of ~0; "not filling" would resume the tally on a FULL buffer, which is the
        // one state that most conclusively refutes starvation. A link that is genuinely marginal
        // still gets counted the moment a VBR peak starts eating the reserve — that is what
        // `draining()` is, and it costs the tally only the delay of becoming observable.
        let unsafe_now = unsafe_horizon && !cold_start && self.buffer.draining();
        if unsafe_now {
            if self.unsafe_deficit_active {
                self.unsafe_deficit_ms = self.unsafe_deficit_ms.saturating_add(wall_delta);
            } else {
                self.unsafe_deficit_ms = 0;
            }
            self.unsafe_deficit_active = true;
        } else {
            self.unsafe_deficit_ms = 0;
            self.unsafe_deficit_active = false;
        }
        let target = self.fallback_target();
        let fallback = self.verdict(buffered_ms, remaining_ms, target);
        Some(OriginalObservation {
            measured_kbps,
            conservative_kbps: conservative,
            requirement_kbps: requirement,
            buffered_ms,
            slope_ms_per_s: self.buffer.slope_ms_per_s,
            horizon_secs: horizon.seconds,
            unsafe_deficit_ms: self.unsafe_deficit_ms,
            fallback,
            target,
        })
    }

    fn verdict(
        &self,
        buffered_ms: i64,
        remaining_ms: i64,
        target: Option<Rung>,
    ) -> Option<OriginalExit> {
        // Nowhere to go: every HLS candidate is infeasible, so a switch cannot help.
        target?;
        // **The rest of the film is already in hand.** No deficit can starve a reserve that
        // outlasts the content, which is also why the closing minutes need no special case: the
        // buffer overtakes what is left and the question stops being asked.
        if remaining_ms > 0 && buffered_ms >= remaining_ms {
            return None;
        }
        // RAW delta, not the smoothed slope: this branch exists for the case where the estimates
        // are wrong, so it must not consult a trend that lags the drop it is watching for.
        //
        // **`&& !filling()`, because a small reserve has two meanings and the LEVEL cannot tell
        // them apart.** Nearly gone and still falling is the emergency this guard is for. Nearly
        // gone because a fresh `Load` just consumed the prime is a stream WARMING UP, and every
        // mode entry starts there by construction — `primed: v=749ms a=908ms` is a normal join.
        //
        // Host-simulator reproduction, 2026-08-29, five seconds after a CORRECT recovery into 4K
        // Dolby Vision direct play:
        //
        // ```text
        // reload_at: fresh Load at 129s
        // auto: Original -> HLS EmergencyLowBuffer measured=25911kbps safe=21168kbps
        //       need=21104kbps buf=1181ms slope=1113ms/s starve=none held=0ms
        // reload_transcode: fresh Load at offset 126s
        // ```
        //
        // `starve=none` is the whole argument: the horizon is INFINITE because `conservative_kbps`
        // had already cleared the requirement — the model's own capacity test said the link was
        // sufficient — while the reserve refilled at better than one millisecond of media per
        // millisecond of wall clock. Two reloads in ten seconds, both visible.
        //
        // The raw delta stays the trigger, so a genuine cliff is still acted on the window it
        // happens. `filling()` only disqualifies a reserve measurably going the other way, and it
        // is not `!draining()`: the flat-within-noise band between them is left to the guard.
        if buffered_ms <= self.policy.emergency_buffer_ms
            && self.buffer.last_delta_ms < 0
            && !self.buffer.filling()
        {
            return Some(OriginalExit::EmergencyLowBuffer);
        }
        // **The horizon's own premise has to be observed, not assumed.** `starvation_horizon` is
        // `T = B·R/(R−C)`, and that formula is a prediction ONLY under the premise that the
        // reserve is being consumed at `(R−C)/R`. When the measured reserve is flat or growing,
        // the premise is contradicted by the measurement sitting next to it, and `T` is arithmetic
        // on a discounted rate rather than a forecast of anything.
        //
        // This is what `docs/measurements/orig-first-window-fallback.md` concluded in its own
        // words — *"any test of sustainability that looked at the buffer's DIRECTION rather than
        // at a discounted rate against an inflated requirement would have stayed"* — and it is the
        // same conjunct `EmergencyLowBuffer` above already carries, read the same way: the RAW
        // delta, because a branch that exists for the case where the estimates are wrong must not
        // consult a trend that lags.
        //
        // It also makes window 1 structurally unable to fire, which is the other half of that
        // finding: `last_delta_ms` is 0 on the first sample by construction, there being nothing
        // to difference against, while `conservative_kbps` is pinned to half the measurement by
        // the 500 pm uncertainty floor. A 42 365 kbps link carrying a 25 264 kbps file abandoned
        // 4K Dolby Vision + Atmos on that window, permanently, with the reserve rising at
        // +113 ms/s.
        //
        // **NECESSARY, not sufficient.** The deficit term is still `(R − C)` with `C` discounted,
        // so a reserve that is falling for any reason still gets a horizon computed from a rate
        // rather than from the observed drain. Replacing that is the plant-model work the plan of
        // record defers; this removes the case the device actually produced and claims no more.
        // **`draining()` here, `last_delta_ms` in the emergency guard above, and the split is the
        // argument rather than an inconsistency.** Both branches want "the reserve is actually
        // falling"; they differ in what a wrong answer costs, so they read the derivative at
        // different noise tolerances.
        //
        // `EmergencyLowBuffer` fires under `emergency_buffer_ms` (2 000 ms), where the next window
        // may be the stall. There the RAW delta is right: acting on noise costs one downshift,
        // waiting for a trend costs the picture.
        //
        // This branch fires with a reserve that can be arbitrarily large — 4 814 ms in the device
        // case below — and it costs a full pipeline reload and a visible blink. A single raw
        // sample cannot support that, because `buffered_ms` is
        // `min(video_tail, audio_tail) - playpos` (`ff.rs::progressive_buffered_ms`) and `playpos`
        // comes off a **5 Hz** position callback, so B carries ~200 ms of quantisation. One window
        // is 750 ms of read; against a reserve filling at ~500 ms/s the expected travel is ~380 ms
        // and the quantisation is ~200 ms, so the SIGN of a single delta is very nearly a coin
        // flip on a healthy link. Over the eight windows a joined stream actually runs, at least
        // one negative sample is close to certain.
        //
        // Device-measured 2026-08-29, and this is the whole of the bug: a 25 264 kbps 4K Dolby
        // Vision + Atmos source, joined at 943 s, playing at full speed (`play=985..1031pm`) with
        // the reserve climbing 749 -> 4 814 ms, abandoned after 8 seconds on
        // `starve=16 ... slope=1020ms/s` — a starvation horizon and a FILLING reserve on the same
        // line. The switch back cost a second reload eight seconds after the first, and the two
        // Loads are the two blinks the viewer saw.
        //
        // **It is not sluggish against a real collapse**, which is the objection to reading a
        // smoothed slope at all. The EWMA is 3:1, so one window is a quarter of the step: a
        // reserve falling 3 000 ms in a 750 ms window contributes a sample slope of -4 000 ms/s
        // and pulls a slope sitting at +1 000 straight to -250, past `DRAIN_EPS_MS_PER_S`, on that
        // same window. What it cannot do is cross on the ±200 ms of quantisation noise, which is
        // exactly the discrimination this branch was missing.
        //
        // **And the derivative is read as a HORIZON, not as a boolean, because the band is a
        // TIME.** `starvation_fallback_secs` says "the reserve runs out within twenty seconds";
        // `draining()` alone says only "it is going down", which is true of a reserve five seconds
        // from empty and equally true of one thirty-five seconds from empty. Between the two of
        // them the branch was asserting a time it had measured from `T` alone — and `T`'s `R` is
        // the file's whole-file average, a claim about the FILE rather than about the reserve in
        // front of the decoder.
        //
        // `observed_starvation_secs` is the same quantity differenced out of the reserve itself,
        // and its module doc has the argument for why that is the measurement and this is the
        // arithmetic. Device, 2026-08-29 — the second half of the same film's bug, after the
        // basis fix above had already moved the first: a 25 264 kbps source, link measuring
        // ~18 000 kbps, reserve at 5 083 ms falling 146 ms/s. The model reads a 47 % deficit and
        // forecasts 11 s. The reserve loses 146 ms of media per second of wall clock — a 15 %
        // deficit — and is 35 s from empty. The link recovered well inside that; the abandon threw
        // the recovery away and cost a reload and a blink.
        //
        // It is not sluggish against a collapse either, which is the standing objection to every
        // guard added here: a reserve genuinely falling off a cliff has a SHORT observed horizon
        // by definition, so the conjunct it adds is satisfied by exactly the case the branch
        // exists for. `a_genuine_collapse_still_exits_at_once` is that case — 6 000 ms draining at
        // 2 666 ms/s, two seconds of runway, and it fires on the first window with a derivative.
        //
        // This SUBSUMES `draining()`, which was the previous conjunct: a horizon only exists when
        // the reserve is measurably draining, at the same `DRAIN_EPS_MS_PER_S` magnitude test and
        // for the same anti-noise reason.
        let observed_horizon = self.buffer.observed_starvation_secs();
        let observed_imminent = observed_horizon
            .is_some_and(|secs| secs <= i64::from(self.policy.starvation_fallback_secs));

        // **The fallback band says a switch is WORTH IT; it does not turn one derivative into a
        // confirmed trend.** Device, 2026-08-30, after an in-place seek: the film played another
        // fifty seconds at ~1.0x with the video AU queue near its 10 MiB cap, then this branch
        // abandoned 4K Original on only `held=2406ms`, with a modest `slope=-198ms/s` and fifteen
        // seconds of OBSERVED runway. The modelled horizon was ten seconds, so both halves above
        // agreed — on a short trend whose own runway could afford more evidence.
        //
        // Use the duration the sustained branch already names as that evidence. This does NOT make
        // an urgent collapse wait: `confirmation_remaining_ms` is compared with the observed
        // runway, and the branch fires immediately when waiting would leave no more than
        // `emergency_buffer_ms`. That floor already has exactly this product meaning — below it
        // "wait and see" is no longer a policy — and it exceeds the measured downshift reload
        // cost. So a two-second cliff still exits on its first derivative, while the device's
        // fifteen-second runway pays the remaining two seconds of confirmation before a visible
        // reload is allowed.
        let confirmation_remaining_ms = self
            .policy
            .sustained_unsafe_deficit_ms
            .saturating_sub(self.unsafe_deficit_ms)
            .max(0);
        let confirmation_due = confirmation_remaining_ms == 0;
        let cannot_afford_confirmation = observed_horizon.is_some_and(|secs| {
            secs.saturating_mul(1_000)
                <= confirmation_remaining_ms.saturating_add(self.policy.emergency_buffer_ms.max(0))
        });
        // The reserve derivative is the physical signal.  A whole-file average cannot describe a
        // VBR peak, decoder backpressure or any other instantaneous demand, so requiring its
        // modelled horizon to agree can suppress a real, measured countdown.  Confirmation keeps
        // quantisation noise from firing this branch; once confirmed, the observed runway is
        // sufficient on its own.
        let imminent = observed_imminent && (confirmation_due || cannot_afford_confirmation);
        if imminent {
            return Some(OriginalExit::ImminentStarvation);
        }
        if self.unsafe_deficit_ms < self.policy.sustained_unsafe_deficit_ms {
            return None;
        }
        // Sustained but not imminent: the only branch where a visible switch is a JUDGEMENT, so
        // it is the only one that consults utility — and therefore the only one the anti-flapping
        // penalty can veto.
        let candidate = self.catalog.candidate(target?);
        let inputs = ModeInputs {
            current: ModeKind::Original,
            source_kbps: self.source_kbps,
            source_raster: self.catalog.source_raster(),
            source_delivery: self.delivery,
            hls_delivery: self.delivery,
            // No HLS session is running, so there IS no measured server cadence to consult — the
            // default is the honest value here rather than a fabrication. (Contrast the recovery
            // gate, where an HLS session is live and its estimator was being thrown away.)
            production: ProductionEstimate::default(),
            buffer: self.buffer,
            remaining_ms,
            history: self.history,
            original_feasible: true,
            source_dv: self.features.dv,
            source_atmos: self.features.atmos,
            unsafe_deficit_ms: self.unsafe_deficit_ms,
        };
        let (mode, _, _, _) = choose_mode(&inputs, candidate, candidate, &self.policy);
        (mode == ModeKind::Hls).then_some(OriginalExit::SustainedDeficit)
    }
}
