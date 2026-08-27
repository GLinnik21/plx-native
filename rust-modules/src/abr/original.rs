use super::*;

/// A runtime Original failure is not an unknown-link bootstrap: the direct transfer has just
/// measured the link. Start the replacement at the best actuator that measurement sustains, so a
/// 4 Mbit/s cap enters at 2 Mbit/s/720p rather than needlessly flashing the 240p emergency floor.
///
/// `catalog` carries this playback's feasibility bounds, so the answer can never be a raster the
/// device or the source cannot supply.
pub(crate) fn original_fallback_rung(
    measured_kbps: u32,
    catalog: &HlsActuatorCatalog,
    policy: &AbrPolicy,
) -> Rung {
    let budget = u32::try_from(
        u64::from(measured_kbps).saturating_mul(1_000)
            / u64::from(policy.vbr_allowance_pm.max(1)),
    )
    .unwrap_or(u32::MAX);
    catalog
        .best_for_budget(budget)
        .or_else(|| catalog.feasible().next())
        .map(|candidate| candidate.rung)
        .unwrap_or(Rung::P240)
}

/// **Cold-start Original admission, and only that.** The measured source prefix must complete and
/// carry the whole-file average with the policy's confidence margin. It is deliberately a fixed
/// margin rather than an uncertainty discount: at this moment there is exactly one sample, so
/// there is no dispersion to discount and the margin has to stand in for the confidence a history
/// would have given. Everything after the first segment goes through
/// [`OriginalModeController`] instead, which has one.
pub(crate) fn original_sustainable(
    source_kbps: u32,
    measured_kbps: u32,
    complete: bool,
    policy: &AbrPolicy,
) -> bool {
    source_kbps > 0
        && complete
        && u64::from(measured_kbps).saturating_mul(1_000)
            >= u64::from(source_kbps).saturating_mul(u64::from(policy.bootstrap_confidence_pm))
}

/// Segments of healthy HLS between two source probes. A probe reads real media bytes over the same
/// link the segments need, so it is not free: this is the "bounded expensive-probe frequency"
/// policy, expressed in the only clock the demux worker has.
pub(crate) const ORIGINAL_PROBE_SPACING: u8 = 3;

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
}

/// **Explicit HLS→Original gate, on evidence about the SOURCE.**
///
/// Two things it deliberately does not require, both of which it used to. It does not require the
/// top rung: PMS producing 20 Mbit/s of H.264 says the SERVER can encode, and says nothing about
/// whether the link can carry a 60 Mbit/s remux — a set that struggles to transcode may be an
/// ideal direct-play target, so gating recovery on transcode success measured the wrong resource.
/// And it does not count successful probes: the probes go into a [`CapacityEstimate`], and what
/// has to clear the requirement is its UNCERTAINTY-DISCOUNTED value. One probe at twice the
/// requirement therefore recovers immediately, while a marginal one waits for a second that
/// agrees — which is the behaviour "two probes" was reaching for, without the number.
pub(crate) struct OriginalRecovery {
    source_kbps: u32,
    policy: AbrPolicy,
    /// Evidence about the source request specifically. Never seeded from HLS segments; see
    /// [`CapacityEstimate::demote_to_prior`].
    probe: CapacityEstimate,
    features: bool,
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
    healthy_samples: u8,
    probes: u8,
}

impl OriginalRecovery {
    pub(crate) fn new(
        source_kbps: u32,
        policy: AbrPolicy,
        features: bool,
        history: TransitionHistory,
    ) -> Option<Self> {
        (source_kbps > 0).then_some(Self {
            source_kbps,
            policy,
            probe: CapacityEstimate::default(),
            features,
            history,
            elapsed_ms: 0,
            healthy_samples: 0,
            probes: 0,
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

    /// Would a SUCCESSFUL probe change anything? Asked before spending one, because a probe reads
    /// real media bytes over the link the segments need. Answered with the utility comparison
    /// under an assumed-good outcome, so "twenty seconds left" and "already switched three times"
    /// stop the measurement rather than being discovered after paying for it.
    fn worth_probing(
        &self,
        current: HlsCandidate,
        buffer: BufferEstimate,
        hls_delivery: &CapacityEstimate,
        remaining_ms: i64,
    ) -> bool {
        let requirement = source_requirement_kbps(self.source_kbps, &self.policy);
        let assumed = CapacityEstimate::from_prior(requirement.saturating_mul(2));
        let inputs = self.inputs(assumed, buffer, *hls_delivery, remaining_ms);
        let (mode, _, _, _) = choose_mode(&inputs, current, current, &self.policy);
        mode == ModeKind::Original
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
        buffer: BufferEstimate,
        hls_delivery: CapacityEstimate,
        remaining_ms: i64,
    ) -> ModeInputs {
        ModeInputs {
            current: ModeKind::Hls,
            source_kbps: self.source_kbps,
            source_delivery,
            hls_delivery,
            production: ProductionEstimate::default(),
            buffer,
            remaining_ms,
            history: self.history.advanced_by(self.elapsed_ms),
            original_feasible: true,
            original_features: self.features,
            // Recovery asks about a source that is NOT currently being read, so there is no live
            // deficit to have persisted. The probe estimate carries all the doubt there is.
            persistent_deficit_windows: 0,
        }
    }

    /// Is this the moment to spend a probe? Four independent gates, none of them a rung: a reserve
    /// deep enough that the probe cannot cause the starvation it is looking for, a reserve that is
    /// not draining, measurable spare capacity in the HLS evidence (a lower bound on the link —
    /// the only thing segments can honestly prove about it), and the spacing above.
    pub(crate) fn probe_due(
        &mut self,
        current: HlsCandidate,
        sample: SegmentSample,
        buffer: BufferEstimate,
        hls_delivery: &CapacityEstimate,
        remaining_ms: i64,
    ) -> bool {
        let segment = i64::from(sample.media_duration_ms);
        // An unreadable reserve is not a deep one. A probe spends the reserve it cannot
        // see, which is the one thing this gate exists to prevent.
        let deep_reserve = sample
            .buffer
            .buffered_ms()
            .is_some_and(|ms| ms >= segment.saturating_mul(3));
        let refilling = !buffer.draining();
        let spare_capacity =
            hls_delivery.conservative_kbps() > current.expected_wire_kbps;
        if !(deep_reserve && refilling && spare_capacity) {
            self.healthy_samples = 0;
            return false;
        }
        self.healthy_samples = self.healthy_samples.saturating_add(1);
        if self.healthy_samples < ORIGINAL_PROBE_SPACING {
            return false;
        }
        if !self.worth_probing(current, buffer, hls_delivery, remaining_ms) {
            return false;
        }
        self.healthy_samples = 0;
        true
    }

    pub(crate) fn observe_probe(
        &mut self,
        observation: CapacityObservation,
        buffer: BufferEstimate,
        hls_delivery: &CapacityEstimate,
        remaining_ms: i64,
    ) -> RecoveryVerdict {
        self.probes = self.probes.saturating_add(1);
        if !observation.completed {
            // A truncated probe is not a slow link — it is an absent measurement, and folding it
            // into the estimate as a low rate would poison the next decision with a number no
            // transfer ever sustained.
            return RecoveryVerdict::Insufficient;
        }
        self.probe.update(observation);
        let requirement = source_requirement_kbps(self.source_kbps, &self.policy);
        if self.probe.conservative_kbps() < requirement {
            return RecoveryVerdict::Insufficient;
        }
        let inputs = self.inputs(self.probe, buffer, *hls_delivery, remaining_ms);
        let hls = HlsActuatorCatalog::measured().candidate(Rung::P1080High);
        let (mode, _, _, _) = choose_mode(&inputs, hls, hls, &self.policy);
        if mode == ModeKind::Original {
            RecoveryVerdict::Recover
        } else {
            RecoveryVerdict::NotWorthIt
        }
    }
}

/// One measurement window of the live progressive transfer. 750 ms of ACTIVE body-read time, not
/// wall clock: a reader parked on backpressure with a full buffer is the healthy case and must not
/// be measured as a slow link.
pub(crate) const ORIGINAL_WINDOW_US: u64 = 750_000;
/// Windows spent with the buffer's starvation horizon inside the unsafe band before a sustained
/// deficit is called. Six windows is about four and a half seconds of real transfer — long enough
/// that one shaped burst is not a mode switch, short enough to act while the reserve still exists.
pub(crate) const ORIGINAL_DEFICIT_WINDOWS: u8 = 6;

/// Why Original was abandoned. Three distinct causes, logged by name, because the operator
/// question after a visible switch is always which of them fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OriginalExit {
    /// The reserve will not outlast the deficit: [`starvation_horizon`] is inside the policy's
    /// fallback band. Acted on WITHOUT consulting utility — a stall is worse than any switch.
    ImminentStarvation,
    /// A deficit that has persisted for [`ORIGINAL_DEFICIT_WINDOWS`] and that the utility
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
    pub(crate) bad_windows: u8,
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
    features: bool,
    last_bytes: u64,
    last_active_us: u64,
    pub(super) deficit_windows: u8,
}

impl OriginalModeController {
    pub(crate) fn new(
        source_kbps: u32,
        policy: AbrPolicy,
        catalog: HlsActuatorCatalog,
        history: TransitionHistory,
        features: bool,
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
            deficit_windows: 0,
        })
    }

    /// A seek keeps the DELIVERY estimate and discards everything positional. The link did not
    /// change because the viewer jumped; the buffer, the deficit history and the byte counters all
    /// describe a position that no longer exists.
    pub(crate) fn on_seek(&mut self, bytes: u64, active_us: u64) {
        self.last_bytes = bytes;
        self.last_active_us = active_us;
        self.deficit_windows = 0;
        self.buffer = BufferEstimate::default();
    }

    /// A pause is the one gap where wall-clock time passes with no measurement, so it is the one
    /// place staleness is real rather than backpressure. See [`CapacityEstimate::age_ms`].
    pub(crate) fn on_resume(&mut self, paused_ms: u64) {
        self.delivery.age_ms(paused_ms, &self.policy);
        self.buffer = BufferEstimate::default();
        self.deficit_windows = 0;
    }

    fn fallback_target(&self) -> Option<Rung> {
        self.catalog
            .best_for_budget(self.delivery.conservative_kbps())
            .or_else(|| self.catalog.feasible().next())
            .map(|candidate| candidate.rung)
    }

    pub(crate) fn observe(
        &mut self,
        bytes: u64,
        active_us: u64,
        buffered_ms: Option<i64>,
        remaining_ms: i64,
    ) -> Option<OriginalObservation> {
        if bytes < self.last_bytes || active_us < self.last_active_us {
            self.on_seek(bytes, active_us);
            return None;
        }
        let active_delta = active_us - self.last_active_us;
        if active_delta < ORIGINAL_WINDOW_US {
            return None;
        }
        let byte_delta = bytes - self.last_bytes;
        self.last_bytes = bytes;
        self.last_active_us = active_us;
        let Some(buffered_ms) = buffered_ms else {
            // No timestamp on one lane yet. That IS the starvation this metric watches for, but it
            // is not yet evidence of it — an A/V session that has not produced both tails cannot
            // be told apart from one that never will.
            self.deficit_windows = 0;
            return None;
        };
        if byte_delta == 0 {
            self.deficit_windows = 0;
            return None;
        }
        let measured_kbps = (byte_delta.saturating_mul(8_000) / active_delta)
            .min(u64::from(u32::MAX)) as u32;
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
        self.buffer
            .update(Some(buffered_ms), i64::try_from(active_delta / 1_000).unwrap_or(1).max(1));

        let requirement = source_requirement_kbps(self.source_kbps, &self.policy);
        let conservative = self.delivery.conservative_kbps();
        let horizon = starvation_horizon(buffered_ms, requirement, conservative);
        let unsafe_horizon = horizon
            .seconds
            .is_some_and(|secs| secs < self.policy.starvation_safe_secs);
        if unsafe_horizon {
            self.deficit_windows = self.deficit_windows.saturating_add(1);
        } else {
            self.deficit_windows = 0;
        }
        let target = self.fallback_target();
        let fallback = self.verdict(buffered_ms, remaining_ms, horizon, target);
        Some(OriginalObservation {
            measured_kbps,
            conservative_kbps: conservative,
            requirement_kbps: requirement,
            buffered_ms,
            slope_ms_per_s: self.buffer.slope_ms_per_s,
            horizon_secs: horizon.seconds,
            bad_windows: self.deficit_windows,
            fallback,
            target,
        })
    }

    fn verdict(
        &self,
        buffered_ms: i64,
        remaining_ms: i64,
        horizon: StarvationHorizon,
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
        if buffered_ms <= self.policy.emergency_buffer_ms && self.buffer.last_delta_ms < 0 {
            return Some(OriginalExit::EmergencyLowBuffer);
        }
        let imminent = horizon
            .seconds
            .is_some_and(|secs| secs <= self.policy.starvation_fallback_secs);
        if imminent {
            return Some(OriginalExit::ImminentStarvation);
        }
        if self.deficit_windows < ORIGINAL_DEFICIT_WINDOWS {
            return None;
        }
        // Sustained but not imminent: the only branch where a visible switch is a JUDGEMENT, so
        // it is the only one that consults utility — and therefore the only one the anti-flapping
        // penalty can veto.
        let candidate = self.catalog.candidate(target?);
        let inputs = ModeInputs {
            current: ModeKind::Original,
            source_kbps: self.source_kbps,
            source_delivery: self.delivery,
            hls_delivery: self.delivery,
            production: ProductionEstimate::default(),
            buffer: self.buffer,
            remaining_ms,
            history: self.history,
            original_feasible: true,
            original_features: self.features,
            persistent_deficit_windows: self.deficit_windows,
        };
        let (mode, _, _, _) = choose_mode(&inputs, candidate, candidate, &self.policy);
        (mode == ModeKind::Hls).then_some(OriginalExit::SustainedDeficit)
    }
}

