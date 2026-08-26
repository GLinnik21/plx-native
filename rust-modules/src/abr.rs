//! Client-managed adaptive-quality policy for Plex's measured fixed-rendition HLS sessions.
//!
//! The PMS probe proved that one HLS encoder session has one fixed rendition. A quality move is
//! therefore a transaction: propose a rung, prime a separately named encoder, then commit only
//! after that candidate has delivered a decodable segment with enough headroom. A rejected prime
//! leaves this controller's current rung untouched.
//!
//! All presentation values are normalized [`MediaTimeMs`] values. Raw FFmpeg PTS, stream time
//! bases, segment-local offsets and discontinuity counters never cross this boundary.
//!
//! # The decision pipeline
//!
//! Every Auto decision — the first one at startup and every one during playback — runs the same
//! ordered stages, and the ordering is the design:
//!
//! ```text
//! feasibility  -> which playback states are technically possible at all
//! estimation   -> delivery capacity, PMS production, buffer, each with UNCERTAINTY
//! risk         -> per-candidate starvation horizon + production + buffer stress
//! utility      -> compare feasible states: quality + features - risk - server - transition
//! selection    -> argmax utility
//! validation   -> prime the winner off-screen and grade the actual media
//! commit       -> or keep the current state, untouched
//! ```
//!
//! Three consequences are worth stating, because each replaced an earlier rule that looked
//! reasonable and was wrong:
//!
//! * **Feasibility is not a utility term.** A candidate the decoder cannot decode, or a raster
//!   the device's own codec table refuses, is removed before anything is scored. No weight can be
//!   large enough to make an impossible state the argmax, so no weight is asked to.
//! * **Measurements feed [`CandidateRisk`], not the utility formula.** Variance, VBR headroom,
//!   buffer slope and PMS cadence all reach the decision through one risk number per candidate.
//!   The alternative — one term per telemetry field — is how a utility function becomes
//!   untunable, since every new measurement silently reweights every old one.
//! * **A deficit is not an emergency.** `C < R` says the buffer drains, not that playback stops:
//!   [`starvation_horizon`] turns the pair into seconds, and 60 s of reserve against a 3% deficit
//!   is half an hour away from trouble. Auto used to abandon Original on two slow windows.
//!
//! # What this module deliberately does not model
//!
//! * **Decoder/render health.** This television publishes no trustworthy dropped-frame or
//!   decoder-starvation counter — the heartbeat's `vtick=`/`vgap=` pair counts a 5 Hz position
//!   callback and reads flat straight through a visible stutter (see the CLAUDE.md instrument
//!   note). A proxy invented here would be an unfalsifiable input to every decision below, so
//!   candidate feasibility asks the device's codec table (a fact) and nothing asks the decoder
//!   how it feels.
//! * **Thermal state.** Both a throttling SoC and a throttling server arrive as what they
//!   actually are here: production ratio drift, delivery drift, buffer slope.
//! * **Anything learned.** Every number below is a measurement or a policy constant with a
//!   product meaning in [`AbrPolicy`].

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct MediaTimeMs(pub(crate) i64);

impl MediaTimeMs {
    pub(crate) fn saturating_since(self, earlier: Self) -> i64 {
        self.0.saturating_sub(earlier.0).max(0)
    }
}

/// Auto's request ladder — **the ACTUATOR set, not a settings menu**.
///
/// Six of these are byte-for-byte aligned with `route::Quality`'s canonical fixed rungs, because a
/// user who picks "1080p · 8 Mbps" by hand and Auto arriving at the same operating point must send
/// the same request. The rest exist only for Auto: `P240` is an emergency floor, and the six
/// 1080p rungs between 6 and 18 Mbps are the resolution this controller needs to spend a measured
/// link instead of rounding it down to the next power of two — a 17.5 Mbit/s link that had to
/// choose between 8 and 20 Mbps spent 12 Mbit/s of it on nothing.
///
/// `Uhd` is the one entry whose REQUEST is not its output. See [`HlsActuatorCatalog`]: PMS holds
/// 1920x1080 up to a 21,750 kbps ask and switches to 3840x2160 at 22,000, so the request is the
/// actuator and the raster is the measured consequence. It is also the one rung
/// [`HlsActuatorCatalog::feasible`] can remove: no device decodes every raster.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Rung {
    P240,
    P480,
    P720Low,
    P720,
    P1080M6,
    P1080,
    P1080M10,
    P1080M12,
    P1080M14,
    P1080M16,
    P1080M18,
    P1080High,
    Uhd,
}

pub(crate) const LADDER: [Rung; 13] = [
    Rung::P240,
    Rung::P480,
    Rung::P720Low,
    Rung::P720,
    Rung::P1080M6,
    Rung::P1080,
    Rung::P1080M10,
    Rung::P1080M12,
    Rung::P1080M14,
    Rung::P1080M16,
    Rung::P1080M18,
    Rung::P1080High,
    Rung::Uhd,
];

/// **How much reserve the dev rung pin waits for before it transacts.** Six segments.
///
/// A TOOL constant, not ABR policy: nothing outside [`Controller::pinned_to`] reads it and it
/// takes no part in any decision an unpinned build makes. It exists because a candidate
/// transaction runs inline on the demux worker and costs roughly 2.3 segments of unrefilled
/// playback (`candidate_warmup_budget` + `candidate_prime_budget`), while `candidate_ready`
/// requires two segments of reserve to still be there afterwards. Propose with less and the
/// transaction drains the reserve below its own acceptance test, the candidate is rejected, and
/// the pin re-proposes forever without ever landing — which is not a hypothetical: the closed-loop
/// plant reproduced exactly that livelock the first time this was written without a gate.
///
/// Consequence worth knowing before using the pin: at the top of the ladder the reachable reserve
/// is under three segments (`abr/sim.rs`), so a pin cannot be satisfied there from a standing
/// start. Pin UPWARD from the bootstrap rung, which is how measurement step M4 is written.
const PIN_MIN_RESERVE_SEGMENTS: i64 = 6;

impl Rung {
    pub(crate) const fn kbps(self) -> u32 {
        match self {
            Rung::P240 => 320,
            Rung::P480 => 720,
            Rung::P720Low => 2_000,
            Rung::P720 => 4_000,
            Rung::P1080M6 => 6_000,
            Rung::P1080 => 8_000,
            Rung::P1080M10 => 10_000,
            Rung::P1080M12 => 12_000,
            Rung::P1080M14 => 14_000,
            Rung::P1080M16 => 16_000,
            Rung::P1080M18 => 18_000,
            Rung::P1080High => 20_000,
            Rung::Uhd => 22_000,
        }
    }

    pub(crate) const fn raster(self) -> (u16, u16) {
        match self {
            Rung::P240 => (426, 240),
            Rung::P480 => (854, 480),
            Rung::P720Low | Rung::P720 => (1280, 720),
            Rung::P1080M6
            | Rung::P1080
            | Rung::P1080M10
            | Rung::P1080M12
            | Rung::P1080M14
            | Rung::P1080M16
            | Rung::P1080M18
            | Rung::P1080High => (1920, 1080),
            Rung::Uhd => (3840, 2160),
        }
    }

    /// The ladder entry whose REQUEST is exactly `kbps`, or `None`. Actuator identity by the
    /// number that goes on the wire as the ceiling, which is stable across catalog re-measurement
    /// — unlike `planning`/`expected_wire_kbps`, which moves when somebody probes the server, and
    /// unlike the UI quality enum, which has no mid-1080p points at all
    /// (`plex::session::PlaybackQuality`). Used only by the dev rung pin (I0-D).
    pub(crate) fn from_request_kbps(kbps: u32) -> Option<Rung> {
        LADDER.into_iter().find(|r| r.kbps() == kbps)
    }

    pub(crate) fn ceiling(self) -> crate::plex::Ceiling {
        let (width, height) = self.raster();
        crate::plex::Ceiling {
            max_kbps: i64::from(self.kbps()),
            max_w: i64::from(width),
            max_h: i64::from(height),
        }
    }

    fn index(self) -> usize {
        LADDER.iter().position(|r| *r == self).unwrap_or(0)
    }

    fn below(self) -> Self {
        LADDER[self.index().saturating_sub(1)]
    }

    /// Recover the controller's starting rung from the exact ceiling stored in the playback
    /// route. Auto owns only these canonical values; an arbitrary/manual ceiling is not an ABR
    /// state and therefore has no answer here.
    pub(crate) fn from_ceiling(ceiling: crate::plex::Ceiling) -> Option<Self> {
        LADDER.iter().copied().find(|rung| rung.ceiling() == ceiling)
    }
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
const ORIGINAL_PROBE_SPACING: u8 = 3;

/// End-to-end delivery observations. The source and HLS estimators are deliberately separate:
/// a source probe and an HLS segment exercise different PMS work, but each can seed the other
/// only as an explicitly labelled weak prior, never as an interchangeable measurement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CapacityEstimate {
    pub(crate) fast_kbps: u32,
    pub(crate) slow_kbps: u32,
    pub(crate) uncertainty_pm: u32,
    pub(crate) samples: u32,
}

impl CapacityEstimate {
    pub(crate) fn update(&mut self, observation: CapacityObservation) {
        // **A measurement a factor of four away from the history, in EITHER direction, is not the
        // same link.** Averaging across a regime change describes a link that never existed, and
        // the failure is not symmetric in cost: measured on the television 2026-08-25, an Original
        // recovery probe taken at 3,952 kbps while the shaped leg was still in force pinned the
        // estimate so hard that the next probe's 28,116 kbps blended to 9,993 — below the 10,800
        // requirement — and Auto never returned to Original at all. Two probes, seven times apart,
        // one verdict, and it was the wrong one. So a jump that large restarts the estimate at the
        // new value with a single sample's confidence, which is exactly what it is.
        if self.samples > 0 && observation.is_regime_change(self) {
            *self = Self::from_prior(observation.kbps);
            self.samples = 1;
            return;
        }
        let old_slow = self.slow_kbps;
        let old_fast = self.fast_kbps;
        let weight = observation.weight();
        self.slow_kbps = weighted_mean(old_slow, observation.kbps, weight, 8);
        self.fast_kbps =
            weighted_mean(old_fast, observation.kbps, observation.weight().min(2), 4);
        if self.samples == 0 {
            self.slow_kbps = observation.kbps;
            self.fast_kbps = observation.kbps;
            // **One measurement is one measurement.** A first sample starts at the maximum
            // discount and earns confidence as later samples AGREE with it — which is the whole of
            // "two successful probes" as a property of the estimate rather than a counter: a probe
            // at twice the requirement clears it alone, a marginal one has to be confirmed.
            self.uncertainty_pm = MAX_UNCERTAINTY_PM;
        } else {
            let spread = observation.kbps.abs_diff(self.slow_kbps);
            let relative = if self.slow_kbps == 0 {
                1_000
            } else {
                (u64::from(spread) * 1_000 / u64::from(self.slow_kbps)).min(1_000) as u32
            };
            // The floor falls as agreeing samples accumulate, and never to zero: a link that has
            // behaved for ten segments can still change in the eleventh.
            let sample_uncertainty = match (observation.completed, self.samples) {
                (false, _) => MAX_UNCERTAINTY_PM,
                (true, 1) => 300,
                (true, _) => 200,
            };
            self.uncertainty_pm = relative.max(sample_uncertainty);
            if observation.kbps < self.slow_kbps {
                let downside = self.slow_kbps - observation.kbps;
                self.uncertainty_pm = self
                    .uncertainty_pm
                    .max((downside.saturating_mul(2_000) / self.slow_kbps.max(1)).min(800));
            }
        }
        self.samples = self.samples.saturating_add(1);
    }

    /// Conservative budget used by both bootstrap and steady-state selection. Uncertainty is a
    /// discount, not a bonus: high dispersion means the lower part of the history, not its mean,
    /// is what a new encoder must survive.
    pub(crate) fn conservative_kbps(&self) -> u32 {
        let uncertainty = u64::from(self.uncertainty_pm.min(500));
        let discount = 1_000_u64.saturating_sub(uncertainty);
        (u64::from(self.slow_kbps) * discount / 1_000).min(u64::from(u32::MAX)) as u32
    }

    /// A sudden low fast estimate invalidates a high slow estimate. The slow value remains as a
    /// weak prior so recovery is possible, but a new candidate must fit the observed regime.
    pub(crate) fn collapse(&mut self, measured_kbps: u32) {
        if self.fast_kbps > 0 && measured_kbps * 4 < self.fast_kbps {
            self.slow_kbps = measured_kbps.max(self.slow_kbps / 4);
            self.uncertainty_pm = 400;
        }
        self.fast_kbps = measured_kbps;
    }

    /// **Keep the number, throw away the confidence.** The estimate's value survives as a starting
    /// guess while its uncertainty goes to the maximum discount and its sample count collapses to
    /// one, so the very next real measurement dominates it.
    ///
    /// Three callers, three different reasons the history stopped describing the present, and they
    /// are listed here because "why is confidence being thrown away" is the question a reader of a
    /// surprising decision asks first:
    ///
    /// * **A bootstrap source probe seeding steady-state HLS.** Different request, different PMS
    ///   work, same link — evidence, but not an interchangeable measurement (see
    ///   [`bootstrap`]).
    /// * **A path change.** Local to Remote, Remote to Relay, a different server address: the
    ///   measurements were honest about a route that is no longer the one in use.
    /// * **A long pause.** A rate measured before a ten-minute pause describes a network that has
    ///   had ten minutes to change.
    pub(crate) fn demote_to_prior(&mut self) {
        if self.samples == 0 {
            return;
        }
        self.samples = 1;
        self.fast_kbps = self.slow_kbps;
        self.uncertainty_pm = self.uncertainty_pm.max(MAX_UNCERTAINTY_PM);
    }

    /// Age the estimate over a wall-clock gap in which nothing was measured. Below one half-life
    /// this is a graded widening of uncertainty; past four it is [`Self::demote_to_prior`],
    /// because at that point the estimate is a memory rather than a measurement.
    pub(crate) fn age_ms(&mut self, elapsed_ms: u64, policy: &AbrPolicy) {
        let half_life = u64::from(policy.stale_half_life_ms.max(1));
        if self.samples == 0 || elapsed_ms < half_life {
            return;
        }
        if elapsed_ms >= half_life.saturating_mul(4) {
            self.demote_to_prior();
            return;
        }
        // Each half-life closes half the remaining distance to the maximum discount: one gives
        // 250, two 375, three 437 — and the fourth is the demotion above.
        let halvings = u32::try_from(elapsed_ms / half_life).unwrap_or(u32::MAX).min(16);
        let widened = MAX_UNCERTAINTY_PM - (MAX_UNCERTAINTY_PM >> halvings);
        self.uncertainty_pm = self.uncertainty_pm.max(widened);
    }

    /// One measurement standing in for a history — a bootstrap probe, or a rate carried across a
    /// mode switch. Deliberately born at maximum uncertainty: it is a place to start, not a fact
    /// about the next ten minutes.
    pub(crate) fn from_prior(kbps: u32) -> Self {
        Self {
            fast_kbps: kbps,
            slow_kbps: kbps,
            uncertainty_pm: MAX_UNCERTAINTY_PM,
            samples: 1,
        }
    }
}

/// How much a single observation is allowed to move the estimate. The distinction is the plan's
/// "weight observations by quality", and it exists because throughput is a RATE: a 40 KiB read
/// that finished in 3 ms honestly reports 100 Mbit/s and proves nothing about the next second,
/// while two megabytes over 400 ms has actually held the link open long enough to be a claim about
/// sustained capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationQuality {
    /// Truncated, tiny, or over too short an interval to have left TCP's opening burst.
    Weak,
    /// A complete transfer of real size.
    Normal,
    /// Complete, megabyte-scale, and long enough to be a sustained rate.
    Strong,
}

/// One bounded delivery observation. Transfer duration, bytes and completion all matter: a tiny
/// partial read can honestly report a high instantaneous rate while proving nothing about
/// sustained capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CapacityObservation {
    pub(crate) kbps: u32,
    pub(crate) bytes: u64,
    pub(crate) active_us: u64,
    pub(crate) completed: bool,
}

impl CapacityObservation {
    pub(crate) fn quality(self) -> ObservationQuality {
        if !self.completed {
            return ObservationQuality::Weak;
        }
        if self.bytes >= STRONG_OBSERVATION_BYTES && self.active_us >= STRONG_OBSERVATION_US {
            ObservationQuality::Strong
        } else if self.bytes >= NORMAL_OBSERVATION_BYTES {
            ObservationQuality::Normal
        } else {
            ObservationQuality::Weak
        }
    }

    pub(crate) fn weight(self) -> u32 {
        match self.quality() {
            ObservationQuality::Weak => 1,
            ObservationQuality::Normal => 2,
            ObservationQuality::Strong => 3,
        }
    }

    pub(crate) fn is_collapse(self, prior: &CapacityEstimate) -> bool {
        prior.fast_kbps > 0 && self.kbps.saturating_mul(4) < prior.fast_kbps
    }

    /// **A transfer too short to measure reports latency, not capacity** — and reporting it as
    /// capacity is not a rounding error. Measured on the television against a real server: a 2 s
    /// segment at the 320 kbps floor is 80 KB, a LAN delivers it in under a millisecond, and the
    /// arithmetic that follows is honest and absurd — the delivery estimate read **865 Gbit/s**,
    /// and every budget downstream was computed from it.
    ///
    /// What such a transfer really says is "comfortably more than the rate we are asking for", so
    /// that is what it is allowed to say: a [`ObservationQuality::Weak`] sample is clamped to a
    /// small multiple of the rung it was measured on. The ladder then climbs geometrically —
    /// 320 kbps proves 2.5 Mbps, whose segments are large enough to measure properly — which is
    /// how it should have ramped in the first place, and cannot invent a gigabit link on the way.
    pub(crate) fn clamped_to_evidence(self, wire_kbps: u32) -> Self {
        if self.quality() != ObservationQuality::Weak || wire_kbps == 0 {
            return self;
        }
        let ceiling = wire_kbps.saturating_mul(WEAK_SAMPLE_HEADROOM);
        Self { kbps: self.kbps.min(ceiling), ..self }
    }

    /// A factor-of-four gap from the SLOW estimate in either direction — the test
    /// [`CapacityEstimate::update`] restarts on. The downward half overlaps
    /// [`Self::is_collapse`] deliberately: that one pins the FAST estimate the moment a collapse is
    /// seen (the controller's fast-down path reads it), while this one decides whether the history
    /// is still describing the present at all.
    pub(crate) fn is_regime_change(self, prior: &CapacityEstimate) -> bool {
        if prior.slow_kbps == 0 || self.kbps == 0 {
            return false;
        }
        self.kbps.saturating_mul(REGIME_FACTOR) < prior.slow_kbps
            || prior.slow_kbps.saturating_mul(REGIME_FACTOR) < self.kbps
    }
}

/// How far a measurement has to be from the history before the history is treated as describing a
/// different link. Four is deliberately coarse: ordinary variance on a healthy link is well inside
/// it, so this fires on a shaped leg starting or ending, not on jitter.
const REGIME_FACTOR: u32 = 4;

/// The largest discount [`CapacityEstimate::conservative_kbps`] will apply, and therefore the
/// value a demoted prior carries: half the estimate. A cap is needed at all because the discount
/// multiplies — an uncapped one would drive a volatile link's budget to zero and park Auto on the
/// emergency floor for the rest of the film.
const MAX_UNCERTAINTY_PM: u32 = 500;
/// How much more than the current rung a transfer too small to measure is allowed to claim. Eight
/// is one or two ladder steps: enough to climb out of a low rung promptly, small enough that the
/// climb is re-measured at every step instead of being asserted once.
const WEAK_SAMPLE_HEADROOM: u32 = 8;
const STRONG_OBSERVATION_BYTES: u64 = 1_048_576;
const STRONG_OBSERVATION_US: u64 = 250_000;
const NORMAL_OBSERVATION_BYTES: u64 = 256 * 1024;

fn weighted_mean(old: u32, new: u32, weight: u32, denominator: u64) -> u32 {
    if old == 0 {
        return new;
    }
    let new_weight = u64::from(weight.min(u32::try_from(denominator).unwrap_or(u32::MAX)));
    ((u64::from(old) * (denominator - new_weight) + u64::from(new) * new_weight) / denominator)
        .min(u64::from(u32::MAX)) as u32
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
    history: TransitionHistory,
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
            healthy_samples: 0,
            probes: 0,
        })
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
        let (mode, _, _, _) = choose_mode(
            &inputs,
            current,
            Some(current),
            &self.policy,
        );
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
            history: self.history,
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
        let deep_reserve = sample.buffer.buffered_ms() >= segment.saturating_mul(3);
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
        let (mode, _, _, _) = choose_mode(&inputs, hls, Some(hls), &self.policy);
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
const ORIGINAL_WINDOW_US: u64 = 750_000;
/// Windows spent with the buffer's starvation horizon inside the unsafe band before a sustained
/// deficit is called. Six windows is about four and a half seconds of real transfer — long enough
/// that one shaped burst is not a mode switch, short enough to act while the reserve still exists.
const ORIGINAL_DEFICIT_WINDOWS: u8 = 6;

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
    delivery: CapacityEstimate,
    buffer: BufferEstimate,
    history: TransitionHistory,
    features: bool,
    last_bytes: u64,
    last_active_us: u64,
    deficit_windows: u8,
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
            .update(buffered_ms, i64::try_from(active_delta / 1_000).unwrap_or(1).max(1));

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
        let (mode, _, _, _) = choose_mode(&inputs, candidate, Some(candidate), &self.policy);
        (mode == ModeKind::Hls).then_some(OriginalExit::SustainedDeficit)
    }
}

/// Tail timestamps from the demuxer after normalization. `audio_expected` distinguishes genuinely
/// silent media from an A/V session whose audio lane has not produced a timestamp yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BufferSnapshot {
    pub(crate) playback: MediaTimeMs,
    pub(crate) video_tail: MediaTimeMs,
    pub(crate) audio_tail: Option<MediaTimeMs>,
    pub(crate) audio_expected: bool,
}

impl BufferSnapshot {
    pub(crate) fn buffered_ms(self) -> i64 {
        let tail = match (self.audio_expected, self.audio_tail) {
            (true, None) => return 0,
            (_, Some(audio)) => audio.min(self.video_tail),
            (false, None) => self.video_tail,
        };
        tail.saturating_since(self.playback)
    }

    /// The VIDEO lane's own buffered duration, ignoring audio. Diagnostic only: nothing in the
    /// controller reads it, and nothing may — [`Self::buffered_ms`] is the quantity every decision
    /// is made on. It exists because the playable reserve is `min(video, audio)` and the two lanes
    /// have DIFFERENT ceilings (the video queue is 8 MiB against a multi-Mbit stream, the audio
    /// queue 1 MiB against ~192 kbps), so which one binds changes with the rung — and a `buf=`
    /// alone cannot say which. See `docs/adaptive-playback-plan.md` §0.1.
    pub(crate) fn video_buffered_ms(self) -> i64 {
        self.video_tail.saturating_since(self.playback)
    }

    /// The AUDIO lane's own buffered duration, or `None` when this stream has no audio or the lane
    /// has not yet produced a timestamp. Diagnostic only, as [`Self::video_buffered_ms`].
    pub(crate) fn audio_buffered_ms(self) -> Option<i64> {
        self.audio_tail.map(|a| a.saturating_since(self.playback))
    }
}

/// **Can the SERVER keep up** — the resource constraint that is not the network.
///
/// `ratio_pm` is per-mille of segment acquisition time over content duration, so 1000 is exactly
/// real time: below it PMS is running ahead of playback, above it the encoder is losing ground
/// whatever the link does. The two constraints have to be separate because they move
/// independently — the measured 4K point costs 4% more bits and 110% more server work — and
/// because only one of them can be fixed by asking for less picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProductionEstimate {
    pub(crate) ratio_pm: u32,
    pub(crate) uncertainty_pm: u32,
    pub(crate) samples: u32,
    /// The measured ratio divided by the load of the candidate that produced it, i.e. how fast
    /// this server is per unit of transcoding work. This is the part that transfers between
    /// candidates; the ratio itself does not.
    server_pm: u32,
}

impl ProductionEstimate {
    /// One steady-state segment. `cold_start` is a NEW ENCODER's first segment, which carries
    /// decoder and encoder start-up and is not the cadence the replacement will sustain — it is
    /// admitted at low weight rather than discarded, because a cold start bad enough to matter is
    /// still evidence about the server.
    pub(crate) fn observe(&mut self, ratio_pm: u32, load_pm: u32, cold_start: bool) {
        let weight = if cold_start { 1 } else { 3 };
        let normalized = u32::try_from(
            u64::from(ratio_pm).saturating_mul(1_000) / u64::from(load_pm.max(1)),
        )
        .unwrap_or(u32::MAX);
        let (ratio, server) = if self.samples == 0 {
            (ratio_pm, normalized)
        } else {
            (
                weighted_mean(self.ratio_pm, ratio_pm, weight, 8),
                weighted_mean(self.server_pm, normalized, weight, 8),
            )
        };
        self.ratio_pm = ratio;
        self.server_pm = server;
        self.samples = self.samples.saturating_add(1);
        self.uncertainty_pm = if self.samples < 3 {
            250
        } else if ratio_pm.abs_diff(self.ratio_pm) > 200 {
            500
        } else {
            250
        };
    }

    /// What this server would probably spend on `candidate`, given what it is spending on
    /// `current`. `None` until there is a measurement to scale — absence of evidence is not a
    /// prediction of success, and the callers treat it that way.
    ///
    /// **Only part of the measurement scales, and getting that wrong makes this unusable.** The
    /// ratio is total ACQUISITION time over content duration, so it contains a fixed per-segment
    /// cost — connection, request, time to first byte, playlist latency — that does not care how
    /// hard the encode was. Extrapolating the whole number by the load ratio therefore reads a
    /// LAN's 300 ms of round trips on a 480p segment as a struggling server and vetoes every
    /// upshift out of the opening rung (measured on this suite: 480p at 0.4 predicted 1080p at
    /// 1.0, and Auto never left 480p on a 7 Mbit/s link). Split the measurement at
    /// [`AbrPolicy::production_floor_pm`] and scale only the part above it.
    pub(crate) fn predicted_ratio_pm(
        &self,
        candidate: HlsCandidate,
        current: HlsCandidate,
        policy: &AbrPolicy,
    ) -> Option<u32> {
        if self.samples == 0 {
            return None;
        }
        // Same operating point: the measurement IS the prediction. Going through the load model
        // for that case would substitute an interpolated constant for a real number.
        if candidate.rung == current.rung {
            return Some(self.ratio_pm);
        }
        let overhead = self.ratio_pm.min(policy.production_floor_pm);
        let work = u64::from(self.ratio_pm - overhead);
        let scaled = work.saturating_mul(u64::from(candidate.production_load_pm))
            / u64::from(current.production_load_pm.max(1));
        Some(
            u32::try_from(u64::from(overhead).saturating_add(scaled)).unwrap_or(u32::MAX),
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BufferEstimate {
    pub(crate) buffered_ms: i64,
    pub(crate) slope_ms_per_s: i64,
    /// The LAST raw change, unsmoothed. Kept beside the smoothed slope because the two answer
    /// different questions and one of them cannot wait: `slope_ms_per_s` is a 3:1 EWMA, so a single
    /// sharp drop after a healthy stretch still reads POSITIVE — correct for "is this a trend",
    /// useless for "did the reserve just fall off a cliff". The emergency guard reads this one.
    pub(crate) last_delta_ms: i64,
    samples: u32,
    draining_samples: u32,
}

impl BufferEstimate {
    pub(crate) fn update(&mut self, buffered_ms: i64, media_duration_ms: i64) {
        if media_duration_ms <= 0 {
            return;
        }
        let delta = buffered_ms - self.buffered_ms;
        self.last_delta_ms = if self.samples == 0 { 0 } else { delta };
        let sample_slope = (delta * 1_000) / media_duration_ms;
        self.slope_ms_per_s = if self.samples == 0 {
            sample_slope
        } else {
            (self.slope_ms_per_s * 3 + sample_slope) / 4
        };
        if self.draining() {
            self.draining_samples = self.draining_samples.saturating_add(1);
        } else {
            self.draining_samples = 0;
        }
        self.buffered_ms = buffered_ms;
        self.samples = self.samples.saturating_add(1);
    }

    /// **Is the reserve actually shrinking** — a magnitude test, not a sign test, and that is a
    /// device finding rather than a refinement. `slope_ms_per_s` is a 3:1 EWMA, so after any real
    /// drain it decays toward zero asymptotically and NEVER REACHES IT: measured on the television
    /// 2026-08-25, a buffer sitting flat at 11,918 ms reported −16, −12, −9, −6, −4 ms/s over
    /// successive segments, every one of them "draining" to a sign test. The upshift gate requires
    /// `!draining`, so Auto sat on the 10 Mbps rung with a 25 Mbit/s safe budget and a full reserve
    /// for the rest of the film. The same shape as `ui::idle`'s rest test, and the same fix: judge
    /// the travel, not the sign of it.
    pub(crate) fn draining(&self) -> bool {
        self.slope_ms_per_s < -DRAIN_EPS_MS_PER_S
    }

    pub(crate) fn starving(&self) -> bool {
        self.buffered_ms <= 2_000
            || (self.buffered_ms <= 6_000 && self.draining_samples >= 2)
    }
}

/// Below this, a slope is noise around flat: 50 ms of content per second is 5% of real time, which
/// no reserve notices and no decision should turn on.
const DRAIN_EPS_MS_PER_S: i64 = 50;

/// Deterministic starvation math under a constant-rate approximation. This is deliberately not
/// a prediction of when playback will stop; it is a comparable risk horizon across candidates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StarvationHorizon {
    pub(crate) seconds: Option<u32>,
    pub(crate) drain_per_s: i64,
}

pub(crate) fn starvation_horizon(
    buffer_ms: i64,
    requirement_kbps: u32,
    capacity_kbps: u32,
) -> StarvationHorizon {
    if capacity_kbps >= requirement_kbps || requirement_kbps == 0 {
        return StarvationHorizon { seconds: None, drain_per_s: 0 };
    }
    let deficit = i64::from(requirement_kbps - capacity_kbps);
    let drain_per_s = (i64::from(buffer_ms) * i64::from(requirement_kbps)) / i64::from(deficit);
    let seconds = u32::try_from(drain_per_s / 1_000).ok();
    StarvationHorizon { seconds, drain_per_s }
}

/// One PMS operating point. `request_kbps` is what goes on the wire as the ceiling; the other two
/// are what the server was measured to DO with it, and they are separate fields precisely because
/// the request is not a promise in either direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HlsCandidate {
    pub(crate) rung: Rung,
    pub(crate) request_kbps: u32,
    pub(crate) expected_wire_kbps: u32,
    /// Relative PMS transcoding work, per mille of the 1080p/20 Mbps point. **Two values are
    /// measured and the rest are an ordering assumption**, which matters when reading a refusal:
    /// the 1080p high point produced segments at a 0.21 production ratio and the 4K point at 0.44,
    /// i.e. **the wire cost rose 4% while the server's work roughly doubled** — so 1000 and 2100
    /// are evidence. Everything else is estimated from RASTER, with only a slight slope across
    /// bitrate at one raster, because that is where a video encoder's cost actually is: decode,
    /// scale and per-pixel analysis dominate, while rate control at a fixed size is nearly free.
    /// The 4K measurement supports exactly that reading — 2.1x the work for the same 1080p-class
    /// bitrate. Used only comparatively ("would this candidate cost the server more than the one
    /// now running"), never as a predicted absolute time.
    pub(crate) production_load_pm: u32,
}

/// The compact actuator catalog: the fixed request values, and beside each one what this PMS was
/// measured to produce for it.
///
/// **The 4K entry is the reason this type exists rather than a bare `Rung::kbps()` call.** Measured
/// against the probe's Generic HLS / H.264 / AAC profile: a request of up to 21,750 kbps with a
/// 3840x2160 ceiling stays 1920x1080 and the decision tops out near 20,011 kbps, while 22,000
/// kbps flips the output to 3840x2160 advertised at about 20,895 kbps — and every request from 22
/// to 60 Mbps produced that same output. So asking for 20,895 does NOT get 4K, and asking for
/// 22,000 does not get 22 Mbit/s of bits. Both halves have to be stored, or the controller spends
/// a budget it does not have on a raster it did not ask for.
///
/// None of it is a claim about Plex in general — it is this server, this profile, this media
/// shape, taken by `tools/pms-hls-probe.py` (see `docs/pms-hls-protocol-probe.md`). A different
/// PMS may hold a different boundary, which is survivable exactly because the transaction in
/// [`Controller::candidate_ready`] grades the actual segment rather than trusting this table.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HlsActuatorCatalog {
    candidates: [HlsCandidate; 13],
    /// Feasibility, not preference: the widest raster the DEVICE's own codec table admits. A
    /// candidate above it is removed before anything is scored, because no utility weight should
    /// have to outvote a decoder that cannot decode.
    raster_limit: (u16, u16),
    /// The source's own raster, for the smallest-sufficient-box rule in [`Self::admits`]. `(0, 0)`
    /// when the server did not say, which means "no bound".
    source: (u16, u16),
}

impl HlsActuatorCatalog {
    pub(crate) const fn measured() -> Self {
        const fn point(rung: Rung, expected_wire_kbps: u32, production_load_pm: u32) -> HlsCandidate {
            HlsCandidate {
                rung,
                request_kbps: rung.kbps(),
                expected_wire_kbps,
                production_load_pm,
            }
        }
        Self {
            candidates: [
                point(Rung::P240, 320, 90),
                point(Rung::P480, 720, 180),
                point(Rung::P720Low, 2_000, 420),
                point(Rung::P720, 4_000, 450),
                point(Rung::P1080M6, 6_000, 900),
                point(Rung::P1080, 8_000, 930),
                point(Rung::P1080M10, 10_000, 950),
                point(Rung::P1080M12, 12_000, 970),
                point(Rung::P1080M14, 14_000, 980),
                point(Rung::P1080M16, 16_000, 990),
                point(Rung::P1080M18, 18_000, 995),
                point(Rung::P1080High, 20_011, 1_000),
                point(Rung::Uhd, 20_895, 2_100),
            ],
            raster_limit: (u16::MAX, u16::MAX),
            source: (0, 0),
        }
    }

    /// Restrict the catalog to rasters this playback can actually use. Both bounds matter and they
    /// are different questions: `device` is what the SoC decodes (`devcaps`, the television's own
    /// table), and `source` is the picture that exists — asking PMS to UPSCALE a 1080p master to
    /// 4K buys nothing and costs the measured 2.1x of server work, so a candidate wider than the
    /// source is infeasible rather than merely unattractive.
    /// **A zero on either axis means NOBODY SAID, and is treated as unbounded** — not as a
    /// forbidden zero-pixel picture. PMS omits source dimensions often enough that the other
    /// reading would empty the catalog and park Auto on whatever the floor happens to be, which is
    /// the opposite of what a missing field justifies. (This is the mirror image of
    /// `plex::Ceiling::admits`, where an unmeasured source fails CLOSED — deliberately: there, `0`
    /// is being asked to honour an explicit user instruction, and here it is being asked to
    /// forbid a device capability nobody has contradicted.)
    pub(crate) fn limited_to(mut self, device: (u16, u16), source: (u16, u16)) -> Self {
        fn axis(value: u16) -> u16 {
            if value == 0 { u16::MAX } else { value }
        }
        self.raster_limit = (axis(device.0), axis(device.1));
        self.source = source;
        self
    }

    /// Does the DEVICE decode this raster? A hard bound, and the only unconditional one.
    fn decodable(&self, candidate: HlsCandidate) -> bool {
        let (width, height) = candidate.rung.raster();
        width <= self.raster_limit.0 && height <= self.raster_limit.1
    }

    /// Is this box big enough that PMS would not scale the source down at all?
    fn covers_source(&self, candidate: HlsCandidate) -> bool {
        let (width, height) = candidate.rung.raster();
        self.source.0 > 0 && self.source.1 > 0 && width >= self.source.0 && height >= self.source.1
    }

    /// **A rung's raster is a BOUNDING BOX, not a target**, and reading it as a target is a bug
    /// this shipped for one afternoon: PMS fits the source inside the box and never upscales, so
    /// the per-axis test that seemed obvious — box must not exceed the source on either axis —
    /// threw away every 1080p rung for a 1918x802 scope film, which is to say for most films.
    /// Measured on the television against a real library item: Auto capped at 4 Mbps / 720p on a
    /// gigabit LAN, and the log looked healthy while it did it.
    ///
    /// The rule that survives both readings is **the smallest sufficient box**: keep every box that
    /// actually constrains the source (those are real quality steps), keep the smallest box that
    /// covers it (that is "do not scale at all"), and drop the larger ones — a bigger box buys the
    /// same picture and, for the 4K point, would price it with a production load measured on an
    /// output this source cannot produce.
    fn admits(&self, candidate: HlsCandidate) -> bool {
        if !self.decodable(candidate) {
            return false;
        }
        if !self.covers_source(candidate) {
            return true;
        }
        // Dominance is compared on the BOX, never on the bitrate: the six 1080p rungs share one
        // raster and differ only in bits, so a bitrate comparison here would keep the cheapest of
        // them and silently delete the other five.
        let (width, height) = candidate.rung.raster();
        !self.candidates.iter().any(|other| {
            let (other_w, other_h) = other.rung.raster();
            let strictly_smaller =
                other_w <= width && other_h <= height && (other_w < width || other_h < height);
            strictly_smaller && self.decodable(*other) && self.covers_source(*other)
        })
    }

    /// Every candidate this playback may move to, cheapest first. The current rung is deliberately
    /// NOT filtered by [`Self::candidate`] — a state already running has to remain describable
    /// even when a later feasibility bound would exclude it.
    pub(crate) fn feasible(&self) -> impl Iterator<Item = HlsCandidate> + '_ {
        self.candidates.iter().copied().filter(move |c| self.admits(*c))
    }

    pub(crate) fn candidate(self, rung: Rung) -> HlsCandidate {
        self.candidates
            .iter()
            .copied()
            .find(|candidate| candidate.rung == rung)
            .unwrap_or(HlsCandidate {
                rung,
                request_kbps: rung.kbps(),
                expected_wire_kbps: rung.kbps(),
                production_load_pm: 1_000,
            })
    }

    /// The best FEASIBLE actuator whose measured output fits the budget — chosen directly, so a
    /// jump from 8 Mbps to a 15 Mbit/s budget primes the 14 Mbps encoder once instead of walking
    /// 10, 12, 14 and paying three encoder creations for one move.
    pub(crate) fn best_for_budget(&self, safe_budget_kbps: u32) -> Option<HlsCandidate> {
        self.feasible()
            .filter(|candidate| candidate.expected_wire_kbps <= safe_budget_kbps)
            .max_by_key(|candidate| candidate.expected_wire_kbps)
    }

    /// The same selection with the PMS production estimate applied as a second, independent
    /// constraint (see [`ProductionEstimate::predicted_ratio_pm`]). This is what stops a fast link
    /// in front of a loaded server from committing 4K: the network says yes and the server's own
    /// measured cadence says it would fall behind real time.
    pub(crate) fn best_sustainable(
        &self,
        safe_budget_kbps: u32,
        production: &ProductionEstimate,
        current: HlsCandidate,
        policy: &AbrPolicy,
    ) -> Option<HlsCandidate> {
        self.feasible()
            .filter(|candidate| candidate.expected_wire_kbps <= safe_budget_kbps)
            .filter(|candidate| {
                production
                    .predicted_ratio_pm(*candidate, current, policy)
                    .is_none_or(|ratio| ratio <= policy.production_safe_pm)
            })
            .max_by_key(|candidate| candidate.expected_wire_kbps)
    }
}

/// Validated timing for one completed segment. Invalid/zero timing is absence of evidence, never
/// infinite bandwidth or perfect production.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SegmentSample {
    bytes: u64,
    active_fetch_us: u64,
    total_fetch_us: u64,
    media_duration_ms: u32,
    pub(crate) buffer: BufferSnapshot,
}

impl SegmentSample {
    pub(crate) fn new(
        bytes: u64,
        active_fetch_us: u64,
        total_fetch_us: u64,
        media_duration_ms: u32,
        buffer: BufferSnapshot,
    ) -> Option<Self> {
        (bytes > 0
            && active_fetch_us > 0
            && total_fetch_us >= active_fetch_us
            && media_duration_ms > 0)
            .then_some(Self {
                bytes,
                active_fetch_us,
                total_fetch_us,
                media_duration_ms,
                buffer,
            })
    }

    pub(crate) fn network_kbps(self) -> u32 {
        (self.bytes.saturating_mul(8_000) / self.active_fetch_us)
            .min(u64::from(u32::MAX)) as u32
    }

    pub(crate) fn media_duration_ms(self) -> u32 {
        self.media_duration_ms
    }

    /// **What this segment actually WAS, on the wire** — delivered bytes over its content
    /// duration, not what the rung asked PMS for. `kbps` is bits per millisecond, so
    /// `bits / duration_ms` is already kbps and there is no scale factor here.
    ///
    /// It exists because the reachable buffer ceiling is `queue_bytes / media_rate` (plus the feed
    /// lead), so every question about how deep the reserve can get is a question about THIS number
    /// and not about `Rung::kbps()`. Eleven of the thirteen catalog entries carry the request as
    /// their planning rate (`abr.rs`'s catalog note), so the two differ by an unmeasured amount at
    /// exactly the rungs where the ceiling is tightest. Measurement step M4 reads it.
    pub(crate) fn media_kbps(self) -> u32 {
        (self.bytes.saturating_mul(8) / u64::from(self.media_duration_ms))
            .min(u64::from(u32::MAX)) as u32
    }

    /// Per-mille total acquisition time / content duration. This includes PMS JIT production and
    /// TTFB; a two-second segment arriving in 1.9 seconds has almost no production headroom even
    /// if its response body crosses the LAN quickly.
    pub(crate) fn production_ratio_pm(self) -> u32 {
        (self.total_fetch_us.saturating_mul(1_000)
            / u64::from(self.media_duration_ms).saturating_mul(1_000))
            .min(u64::from(u32::MAX)) as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsReason {
    SafeBudgetIncrease,
    UnsafeCurrentState,
    ProductionConstraint,
    BufferConstraint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecisionReason {
    Hls(HlsReason),
}

/// The whole basis of one steady-state decision, for the event log. Every field here was an input
/// to the decision published beside it — which is the property that makes the line worth reading
/// six weeks later, and the reason it is assembled by the controller rather than re-derived at the
/// log site from whatever happened to be reachable.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ControllerTelemetry {
    pub(crate) current: Rung,
    pub(crate) safe_budget_kbps: u32,
    /// What the model would pick for this link RIGHT NOW, ignoring the hysteresis that keeps the
    /// current rung. `None` while nothing is sustainable (a cold start, or a link that cannot
    /// carry the bottom of the ladder). The read-out's "current / optimal" pair.
    pub(crate) optimal: Option<HlsCandidate>,
    pub(crate) delivery: CapacityEstimate,
    pub(crate) production: ProductionEstimate,
    pub(crate) buffer: BufferEstimate,
    pub(crate) risk: CandidateRisk,
    pub(crate) pending: Option<Proposal>,
    pub(crate) reason: Option<DecisionReason>,
}

/// **Every tunable in one place, and every field answers "what product behaviour is this?"** —
/// which is the test a number has to pass to live here at all. What this type replaced was a
/// scatter of `3 good samples`, `8 cooldown samples`, `2 bad windows` and a bare `1_100`, none of
/// which said what it was for, so none of them could be argued with.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AbrPolicy {
    /// PMS is comfortably ahead of real time below this segment-acquisition ratio. Above it a
    /// candidate may still play, but it has no margin left for a slower scene.
    pub(crate) production_safe_pm: u32,
    /// At or above this, the server is producing at or slower than real time: a JIT encoder that
    /// cannot keep up will drain any buffer eventually, whatever the network does.
    pub(crate) production_max_pm: u32,
    /// **The part of segment acquisition that is not production** — connection, request, time to
    /// first byte, playlist latency — as a per-mille of content duration. 250 is half a second of a
    /// two-second segment, which is an ordinary round trip to a remote PMS. It exists so
    /// [`ProductionEstimate::predicted_ratio_pm`] scales only the work, and never reads a LAN's
    /// round trips as a struggling encoder.
    pub(crate) production_floor_pm: u32,
    /// The content reserve an upshift is expected to leave intact. Below it, spend the budget on
    /// refilling instead of on picture.
    pub(crate) minimum_buffer_ms: i64,
    /// Below this the next stall is close enough that "wait and see" is no longer a policy.
    pub(crate) emergency_buffer_ms: i64,
    /// **VBR headroom over a whole-file average.** A file averaging 60 Mbit/s contains scenes well
    /// above it, so the average is a lower bound on demand, not the demand. Spending the entire
    /// measured link on the average merely postpones starvation to the first busy scene.
    pub(crate) vbr_allowance_pm: u32,
    /// Cold-start Original admission, where there is exactly one probe and no history. Higher than
    /// [`Self::vbr_allowance_pm`] on purpose: at that moment the estimate has no dispersion to
    /// discount, so the margin has to carry the uncertainty itself.
    pub(crate) bootstrap_confidence_pm: u32,
    /// How fast an unmeasured gap costs confidence. One of these is a widening; four is a demotion
    /// to a prior ([`CapacityEstimate::age_ms`]).
    pub(crate) stale_half_life_ms: u32,
    /// Below this starvation horizon, a mode change is worth its visible cost — the buffer will
    /// not survive the wait for a better answer.
    pub(crate) starvation_fallback_secs: u32,
    /// Above this horizon the deficit is arithmetic rather than a problem: 60 s of reserve against
    /// a 3% shortfall is half an hour away, and abandoning Original for it would be the old
    /// two-slow-windows bug in a new costume.
    pub(crate) starvation_safe_secs: u32,
    /// Utility cost of a switch the VIEWER SEES — a reload, a black frame, a re-Load. Denominated
    /// in the same units as the quality score below, so the two can be compared at all: on that
    /// scale 15 is about one step of the ladder (2 Mbps to 4), which is the right order for a
    /// two-second interruption. It was 30 for one afternoon, and the device run says what that
    /// buys: 30 plus a fresh switch's penalty outprices Original's entire quality advantage, so
    /// Auto would not return to a recovered link for about four minutes after a fallback.
    pub(crate) visible_switch_cost: i64,
    /// Extra cost per visible switch already made in this playback. One switch is a decision; four
    /// is flapping, and this is what makes the fourth expensive without a hard cooldown counter.
    /// At 15 the arithmetic is: a first move costs 15, the return trip 30 (still inside Original's
    /// 40-point advantage, so one round trip is allowed), and a third 45 (refused).
    pub(crate) visible_switch_penalty: i64,
    /// Half-life of that penalty. A switch fifteen minutes ago is history; one fifteen seconds ago
    /// is a pattern.
    pub(crate) visible_switch_decay_ms: u64,
    /// What Original is worth over the best HLS rendition, before any risk or cost: no generation
    /// loss, source audio, Dolby Vision and Atmos preserved, and zero server video encoding.
    pub(crate) original_quality_bonus: i64,
    /// Added when the source actually carries those features, so the bonus is about this file
    /// rather than about Original in the abstract.
    pub(crate) original_feature_bonus: i64,
    /// Playback remaining at which a mode's benefit counts in full. Below it the benefit is scaled
    /// down linearly, which is what makes a reload with twenty seconds left lose to doing nothing
    /// without anybody writing `if remaining < 20`. Two minutes: the point of the ramp is to price
    /// a benefit against the INTERRUPTION that buys it, and once the remainder dwarfs a two-second
    /// reload there is nothing left to discount for.
    pub(crate) benefit_horizon_ms: i64,
    /// Weight on [`CandidateRisk::score`] in the utility sum.
    pub(crate) risk_weight: i64,
    /// Weight on ongoing PMS transcoding work. Small, because a watchable picture beats a tidy
    /// server — but not zero, because 2.1x the work for 4% more bits is a real trade.
    pub(crate) server_cost_weight: i64,
}

impl AbrPolicy {
    pub(crate) fn measured() -> Self {
        Self {
            production_safe_pm: 750,
            production_max_pm: 1_100,
            production_floor_pm: 250,
            minimum_buffer_ms: 2_500,
            emergency_buffer_ms: 2_000,
            vbr_allowance_pm: 1_350,
            bootstrap_confidence_pm: 1_350,
            stale_half_life_ms: 30_000,
            starvation_fallback_secs: 20,
            starvation_safe_secs: 60,
            visible_switch_cost: 15,
            visible_switch_penalty: 15,
            visible_switch_decay_ms: 120_000,
            original_quality_bonus: 40,
            original_feature_bonus: 25,
            benefit_horizon_ms: 120_000,
            risk_weight: 2,
            server_cost_weight: 4,
        }
    }
}

/// What one second of this source actually demands, average plus VBR headroom. The whole-file
/// average is what PMS reports and what every caller has; this is the number a delivery estimate
/// has to beat.
pub(crate) fn source_requirement_kbps(source_kbps: u32, policy: &AbrPolicy) -> u32 {
    (u64::from(source_kbps).saturating_mul(u64::from(policy.vbr_allowance_pm)) / 1_000)
        .min(u64::from(u32::MAX)) as u32
}

/// **The single number every measurement reaches the decision through.** Delivery variance, VBR
/// headroom, buffer level and slope, and PMS cadence all end here, per candidate, so the utility
/// comparison below has one risk term instead of one term per telemetry field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CandidateRisk {
    pub(crate) starvation_seconds: Option<u32>,
    pub(crate) production_ratio_pm: Option<u32>,
    pub(crate) production_risk: bool,
    pub(crate) buffer_risk: bool,
    pub(crate) score: u32,
}

pub(crate) fn candidate_risk(
    candidate: HlsCandidate,
    current: HlsCandidate,
    capacity: &CapacityEstimate,
    production: &ProductionEstimate,
    buffer: &BufferEstimate,
    policy: &AbrPolicy,
) -> CandidateRisk {
    let conservative = capacity.conservative_kbps();
    let horizon = starvation_horizon(
        buffer.buffered_ms,
        candidate.expected_wire_kbps,
        conservative,
    );
    // The candidate's OWN predicted server cost, not the ratio measured on the rung now running:
    // "the server is at 0.5 on 1080p" and "the server would be at 1.05 on 4K" are different facts,
    // and only the second one decides a move to 4K.
    let predicted = production.predicted_ratio_pm(candidate, current, policy);
    let production_risk = predicted.is_some_and(|ratio| {
        ratio > policy.production_max_pm
            || (ratio > policy.production_safe_pm && production.uncertainty_pm >= 500)
    });
    let buffer_risk = buffer.buffered_ms < policy.emergency_buffer_ms
        || (buffer.starving() && buffer.draining());
    let mut score = 0;
    score += match horizon.seconds {
        None => 0,
        Some(seconds) if seconds >= policy.starvation_safe_secs => 1,
        Some(seconds) if seconds >= policy.starvation_fallback_secs => 4,
        Some(seconds) if seconds >= policy.starvation_fallback_secs / 2 => 12,
        Some(_) => 40,
    };
    if production_risk {
        score += 20;
    }
    if buffer_risk {
        score += 30;
    }
    CandidateRisk {
        starvation_seconds: horizon.seconds,
        production_ratio_pm: predicted,
        production_risk,
        buffer_risk,
        score,
    }
}

/// The continuous budget the actuator is then chosen FROM — never "one rung up". Three separate
/// discounts, each with a reason: uncertainty (inside `conservative_kbps`), a server that is
/// already behind, and a reserve that needs refilling more than the picture needs bits.
pub(crate) fn hls_safe_budget(
    capacity: &CapacityEstimate,
    production: &ProductionEstimate,
    buffer: &BufferEstimate,
    policy: &AbrPolicy,
) -> u32 {
    let mut budget = capacity.conservative_kbps();
    if production.ratio_pm > policy.production_safe_pm {
        budget = budget.saturating_mul(policy.production_safe_pm).max(1)
            / production.ratio_pm.max(1);
    }
    if buffer.buffered_ms < policy.minimum_buffer_ms {
        let deficit = policy.minimum_buffer_ms - buffer.buffered_ms;
        budget = budget.saturating_sub(u32::try_from(deficit).unwrap_or(u32::MAX));
    }
    budget
}

/// The two states Auto can be in. They are not two ends of one ladder: Original is a different
/// pipeline with different costs and a different failure mode, and the whole reason this is an
/// enum rather than a top rung is that "the highest bitrate" and "no server video encoding at
/// all" are not comparable by bitrate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeKind {
    Original,
    Hls,
}

/// Visible mode switches already spent in this playback. Captured on the main thread when a worker
/// starts, then advanced by the worker's own elapsed time — there is no shared clock between them
/// and inventing one would be a race for no gain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TransitionHistory {
    pub(crate) visible_switches: u32,
    pub(crate) since_last_ms: Option<u64>,
}

impl TransitionHistory {
    pub(crate) fn advanced_by(self, elapsed_ms: u64) -> Self {
        Self {
            visible_switches: self.visible_switches,
            since_last_ms: self.since_last_ms.map(|ms| ms.saturating_add(elapsed_ms)),
        }
    }

    /// The decaying half of the switch cost. Halves every [`AbrPolicy::visible_switch_decay_ms`],
    /// so hysteresis comes from history rather than from a fixed sample-count cooldown — one
    /// switch is a decision, a fourth one inside two minutes has to buy a lot to be worth it.
    fn penalty(self, policy: &AbrPolicy) -> i64 {
        if self.visible_switches == 0 {
            return 0;
        }
        let base = policy
            .visible_switch_penalty
            .saturating_mul(i64::from(self.visible_switches));
        let Some(since) = self.since_last_ms else { return base };
        let halvings = since / policy.visible_switch_decay_ms.max(1);
        base >> halvings.min(16)
    }
}

/// **Asymmetric, because the transitions are.** An HLS rung change is a background prime the
/// viewer never sees; leaving Original tears down a direct-play session and re-Loads the pipeline;
/// returning to Original does the same and additionally bets that a link which just failed will
/// hold. A single "switch cost" constant cannot say that.
pub(crate) fn transition_cost(
    from: ModeKind,
    to: ModeKind,
    history: TransitionHistory,
    policy: &AbrPolicy,
) -> i64 {
    if from == to {
        return 0;
    }
    policy.visible_switch_cost + history.penalty(policy)
}

/// One candidate state, scored. Kept as its component terms rather than a bare total because the
/// event log prints them: "Original lost" is not a diagnosis, "Original lost 40 of quality to 60
/// of transition cost with 90 s left" is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ModeUtility {
    pub(crate) quality: i64,
    pub(crate) features: i64,
    pub(crate) risk: i64,
    pub(crate) server: i64,
    pub(crate) transition: i64,
    pub(crate) total: i64,
}

/// Everything the mode comparison needs, gathered once so both callers — the Original watchdog
/// deciding whether to leave, and the HLS controller deciding whether to return — run the SAME
/// formula on the same inputs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ModeInputs {
    pub(crate) current: ModeKind,
    pub(crate) source_kbps: u32,
    /// Delivery evidence for the SOURCE request — a bootstrap or recovery probe, or the live
    /// progressive transfer. Not interchangeable with the HLS estimate; see
    /// [`CapacityEstimate::demote_to_prior`].
    pub(crate) source_delivery: CapacityEstimate,
    pub(crate) hls_delivery: CapacityEstimate,
    pub(crate) production: ProductionEstimate,
    pub(crate) buffer: BufferEstimate,
    pub(crate) remaining_ms: i64,
    pub(crate) history: TransitionHistory,
    /// Feasibility, decided elsewhere and passed in as a fact: codec support, subtitle burn-in,
    /// relay, and whether PMS offered a playable source URL at all.
    pub(crate) original_feasible: bool,
    /// The source carries something a transcode would destroy — Dolby Vision, Atmos, lossless
    /// audio. Makes the Original bonus about THIS file.
    pub(crate) original_features: bool,
    /// **How long the deficit has persisted, in measurement windows.** A dip is noise and a regime
    /// change is not, and only elapsed time tells them apart: without this term the utility
    /// comparison sees a 40-second starvation horizon identically whether the link wobbled once or
    /// has been short for ten seconds straight. It raises Original's risk, so a deficit that will
    /// not go away eventually loses the argument on its own — before starvation is imminent, and
    /// without a hard counter deciding anything.
    pub(crate) persistent_deficit_windows: u8,
}

/// Benefit accrues over the remaining playback; cost is paid once, now. Below the policy horizon
/// the benefit is scaled linearly, which is the whole of "do not reload with twenty seconds left"
/// — no threshold, no special case, and it degrades smoothly rather than at a cliff.
fn benefit_scale_pm(remaining_ms: i64, policy: &AbrPolicy) -> i64 {
    if remaining_ms <= 0 {
        return 0;
    }
    let horizon = policy.benefit_horizon_ms.max(1);
    (remaining_ms.min(horizon) * 1_000) / horizon
}

fn scaled(value: i64, scale_pm: i64) -> i64 {
    value * scale_pm / 1_000
}

/// Quality score of an HLS operating point, in the same units as
/// [`AbrPolicy::original_quality_bonus`]. Concave on purpose: 2 to 4 Mbit/s is a transformation of
/// the picture, 18 to 20 is not, and a linear score would happily pay a visible reload for the
/// second one.
fn hls_quality_score(candidate: HlsCandidate) -> i64 {
    match candidate.expected_wire_kbps {
        0..=500 => 0,
        501..=1_000 => 10,
        1_001..=2_500 => 25,
        2_501..=5_000 => 40,
        5_001..=7_000 => 50,
        7_001..=9_000 => 58,
        9_001..=13_000 => 66,
        13_001..=17_000 => 72,
        _ => 76,
    }
}

pub(crate) fn hls_utility(
    candidate: HlsCandidate,
    current: HlsCandidate,
    inputs: &ModeInputs,
    policy: &AbrPolicy,
) -> ModeUtility {
    let scale = benefit_scale_pm(inputs.remaining_ms, policy);
    let risk = candidate_risk(
        candidate,
        current,
        &inputs.hls_delivery,
        &inputs.production,
        &inputs.buffer,
        policy,
    );
    let quality = scaled(hls_quality_score(candidate), scale);
    let server = policy.server_cost_weight * i64::from(candidate.production_load_pm) / 1_000;
    let transition = transition_cost(inputs.current, ModeKind::Hls, inputs.history, policy);
    let risk_cost = policy.risk_weight * i64::from(risk.score);
    ModeUtility {
        quality,
        features: 0,
        risk: risk_cost,
        server,
        transition,
        total: quality - risk_cost - server - transition,
    }
}

pub(crate) fn original_utility(inputs: &ModeInputs, policy: &AbrPolicy) -> Option<ModeUtility> {
    if !inputs.original_feasible {
        return None;
    }
    let scale = benefit_scale_pm(inputs.remaining_ms, policy);
    // Original's requirement is the source's, with VBR headroom, and its delivery evidence is the
    // source probe's — never the HLS estimate, which measured a different request.
    let requirement = source_requirement_kbps(inputs.source_kbps, policy);
    let horizon = starvation_horizon(
        inputs.buffer.buffered_ms,
        requirement,
        inputs.source_delivery.conservative_kbps(),
    );
    let mut score = match horizon.seconds {
        None => 0,
        Some(seconds) if seconds >= policy.starvation_safe_secs => 2,
        Some(seconds) if seconds >= policy.starvation_fallback_secs => 10,
        Some(seconds) if seconds >= policy.starvation_fallback_secs / 2 => 25,
        Some(_) => 60,
    };
    if inputs.source_delivery.samples == 0 {
        score += 20; // no measurement of THIS request is not the same as a good one
    }
    // Persistence, priced. Four points per window, so about nine seconds of continuous shortfall
    // costs Original the argument even while starvation is still a minute away — and a single
    // wobble, which resets the counter, costs nothing.
    score += u32::from(inputs.persistent_deficit_windows.min(15)).saturating_mul(4);
    let quality = scaled(
        policy.original_quality_bonus + i64::from(hls_quality_score(
            HlsActuatorCatalog::measured().candidate(Rung::P1080High),
        )),
        scale,
    );
    let features = scaled(
        if inputs.original_features { policy.original_feature_bonus } else { 0 },
        scale,
    );
    let transition = transition_cost(inputs.current, ModeKind::Original, inputs.history, policy);
    let risk_cost = policy.risk_weight * i64::from(score);
    Some(ModeUtility {
        quality,
        features,
        // Original asks the server for no video encoding at all — the one term where it wins
        // outright, and the reason a healthy LAN should never be transcoding.
        server: 0,
        risk: risk_cost,
        transition,
        total: quality + features - risk_cost - transition,
    })
}

/// Why a mode comparison came out the way it did — logged verbatim, so "why did Auto choose this"
/// is answerable from the event log alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeReason {
    /// Original wins on utility and its delivery evidence supports it.
    OriginalWorthIt,
    /// Original is technically impossible here (codec, burn-in, relay, no source URL).
    OriginalInfeasible,
    /// Feasible, but the sums say no — usually a visible switch costing more than the remaining
    /// playback can earn back.
    OriginalNotWorthIt,
    /// No HLS candidate is feasible at all, so there is nothing to compare against.
    NoHlsCandidate,
}

/// **The selection step: argmax over the feasible states.** Deliberately only two contenders —
/// Original, and the single best HLS candidate the budget and the server allow — because the rung
/// question was already answered upstream by the safe budget, and re-litigating it here as a
/// thirteen-way utility comparison would let a quality curve override a measured capacity bound.
pub(crate) fn choose_mode(
    inputs: &ModeInputs,
    current_hls: HlsCandidate,
    best_hls: Option<HlsCandidate>,
    policy: &AbrPolicy,
) -> (ModeKind, ModeReason, ModeUtility, Option<ModeUtility>) {
    let original = original_utility(inputs, policy);
    let hls = best_hls.map(|candidate| hls_utility(candidate, current_hls, inputs, policy));
    match (original, hls) {
        (Some(orig), Some(h)) if orig.total > h.total => {
            (ModeKind::Original, ModeReason::OriginalWorthIt, orig, Some(h))
        }
        (Some(orig), Some(h)) => (ModeKind::Hls, ModeReason::OriginalNotWorthIt, h, Some(orig)),
        (Some(orig), None) => (ModeKind::Original, ModeReason::NoHlsCandidate, orig, None),
        (None, Some(h)) => (ModeKind::Hls, ModeReason::OriginalInfeasible, h, None),
        (None, None) => (
            ModeKind::Hls,
            ModeReason::NoHlsCandidate,
            ModeUtility::default(),
            None,
        ),
    }
}

/// How the media reaches this television, as PMS classifies it. Not a preference — a different
/// amount of prior knowledge, which is why bootstrap branches on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkKind {
    Local,
    Remote,
    Relay,
}

/// Why bootstrap chose what it chose. Printed verbatim into the event log: the startup decision is
/// the one nobody can re-run, so it has to explain itself the first time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BootstrapReason {
    /// A verified LAN carrying a file the device can play. No probe: a measurement to prove that a
    /// local network can carry local media would only cost the viewer a second of black screen.
    LocalDirect,
    /// Original is not technically possible at all — codec, container, burn-in, or PMS offering no
    /// playable source URL.
    OriginalInfeasible,
    /// Relay. Plex's relay is bandwidth-limited by design, so Original is not a candidate and
    /// measuring it would be theatre.
    RelayLimited,
    /// A bounded probe cleared the cold-start confidence margin.
    ProbeSustainable,
    /// The probe completed and did not clear it. Its VALUE is still the best evidence there is,
    /// and it picks the starting rung.
    ProbeBelowRequirement,
    /// The probe never finished inside its budget, or the source bitrate is unknown, so there is
    /// nothing to reason from. Conservative HLS, and playback still starts — a link this client
    /// could not measure is not a reason to refuse to play.
    ProbeInconclusive,
}

/// The startup state, plus what to hand the steady-state controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BootstrapDecision {
    pub(crate) original: bool,
    /// The starting rung when `original` is false. Chosen from the same catalog steady-state
    /// selection uses, so a 17 Mbit/s probe on a 60 Mbit/s file opens at a 12-14 Mbps rendition
    /// instead of at an emergency floor it would then have to climb out of for a minute.
    pub(crate) rung: Rung,
    pub(crate) reason: BootstrapReason,
    /// The probe, as a weak prior for the live estimator — so the first HLS segment refines a
    /// measurement instead of starting from nothing. `None` when there was no usable probe.
    pub(crate) prior: Option<CapacityEstimate>,
}

/// **Cold start, where every estimator is empty and the viewer is looking at a black screen.**
///
/// This is a separate decision from steady state and must not pretend otherwise: there is no
/// history, no buffer, no production evidence, and a strict latency budget on acquiring any. So it
/// branches on how much is knowable for free, and its worst case is "start conservative HLS and
/// let the real controller recover", never "hold the screen black until the link is proven".
pub(crate) fn bootstrap(
    link: LinkKind,
    original_feasible: bool,
    source_kbps: u32,
    probe: Option<CapacityObservation>,
    catalog: &HlsActuatorCatalog,
    policy: &AbrPolicy,
) -> BootstrapDecision {
    let fallback_rung = catalog
        .best_for_budget(policy_startup_floor_kbps(policy))
        .or_else(|| catalog.feasible().next())
        .map(|candidate| candidate.rung)
        .unwrap_or(Rung::P480);
    let deny = |reason| BootstrapDecision {
        original: false,
        rung: fallback_rung,
        reason,
        prior: None,
    };
    if !original_feasible {
        return deny(BootstrapReason::OriginalInfeasible);
    }
    match link {
        LinkKind::Local => BootstrapDecision {
            original: true,
            rung: fallback_rung,
            reason: BootstrapReason::LocalDirect,
            prior: None,
        },
        LinkKind::Relay => deny(BootstrapReason::RelayLimited),
        LinkKind::Remote => {
            let Some(probe) = probe.filter(|p| p.kbps > 0 && source_kbps > 0) else {
                return deny(BootstrapReason::ProbeInconclusive);
            };
            if !probe.completed {
                // A probe that ran out of budget measured how far it got, not what the link can
                // do. Its rate is a floor, so it still beats guessing for the starting rung.
                let prior = CapacityEstimate::from_prior(probe.kbps);
                return BootstrapDecision {
                    original: false,
                    rung: startup_rung(probe.kbps, catalog, fallback_rung),
                    reason: BootstrapReason::ProbeInconclusive,
                    prior: Some(prior),
                };
            }
            let sustainable =
                original_sustainable(source_kbps, probe.kbps, probe.completed, policy);
            let mut prior = CapacityEstimate::default();
            prior.update(probe);
            // Explicitly weak: the probe measured the SOURCE request over this link, and the HLS
            // segments about to arrive are a different request to a server doing different work.
            prior.demote_to_prior();
            BootstrapDecision {
                original: sustainable,
                rung: startup_rung(probe.kbps, catalog, fallback_rung),
                reason: if sustainable {
                    BootstrapReason::ProbeSustainable
                } else {
                    BootstrapReason::ProbeBelowRequirement
                },
                prior: Some(prior),
            }
        }
    }
}

/// Startup keeps a fifth of the measurement in reserve rather than spending all of it: there is no
/// buffer yet, so the opening seconds are the least forgiving moment of the whole playback.
fn startup_rung(measured_kbps: u32, catalog: &HlsActuatorCatalog, fallback: Rung) -> Rung {
    catalog
        .best_for_budget(measured_kbps.saturating_mul(4) / 5)
        .map(|candidate| candidate.rung)
        .unwrap_or(fallback)
}

/// The opening rung when nothing at all is known — one the link almost certainly carries, chosen
/// so the first upshift has real evidence behind it rather than being an immediate correction.
fn policy_startup_floor_kbps(_policy: &AbrPolicy) -> u32 {
    Rung::P480.kbps()
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

/// An upshift candidate that cannot deliver one complete segment inside the same production
/// headroom threshold used by [`Controller::candidate_ready`] can never be committed. Give the
/// transport that exact budget so it returns to the active encoder before the playback reserve
/// drains. Downshifts have no such deadline: they are the recovery path when the current rung is
/// already unsustainable.
pub(crate) fn candidate_prime_budget(
    proposal: Proposal,
    media_duration: std::time::Duration,
) -> Option<std::time::Duration> {
    if proposal.direction == Direction::Down {
        return None;
    }
    let micros = media_duration.as_micros().saturating_mul(4) / 5;
    Some(std::time::Duration::from_micros(
        micros.min(u128::from(u64::MAX)) as u64,
    ))
}

/// A new PMS encoder's first segment includes decoder/encoder cold start and is not a
/// steady-state production sample. Give that one warm-up segment a bounded 1.5 content-duration
/// window, then apply [`candidate_prime_budget`] to the following segment before committing the
/// encoder. The proposal gate already requires at least three segments of reserve, so the warm-up
/// plus the graded segment still fits inside the buffer available when an upshift starts.
/// Downshifts keep their established recovery behavior and do not acquire a deadline here.
pub(crate) fn candidate_warmup_budget(
    proposal: Proposal,
    media_duration: std::time::Duration,
) -> Option<std::time::Duration> {
    if proposal.direction == Direction::Down {
        return None;
    }
    let micros = media_duration.as_micros().saturating_mul(3) / 2;
    Some(std::time::Duration::from_micros(
        micros.min(u128::from(u64::MAX)) as u64,
    ))
}

/// Integer-only estimator and transaction state. Current-session samples decide whether to
/// propose. Candidate-session measurements decide whether that proposal may commit.
pub(crate) struct Controller {
    current: Rung,
    pending: Option<Proposal>,
    delivery: CapacityEstimate,
    production: ProductionEstimate,
    buffer: BufferEstimate,
    catalog: HlsActuatorCatalog,
    policy: AbrPolicy,
    samples_on_rung: u8,
    stable_samples: u8,
    cooldown: u8,
    last_reason: Option<DecisionReason>,
    last_safe_budget_kbps: u32,
    /// **Dev-only actuator pin — see [`Self::pinned_to`].** `None` in every production path.
    pin: Option<Rung>,
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
            delivery: prior.unwrap_or_default(),
            production: ProductionEstimate::default(),
            buffer: BufferEstimate::default(),
            catalog,
            policy: AbrPolicy::measured(),
            samples_on_rung: 0,
            stable_samples: 0,
            cooldown: 0,
            last_reason: None,
            last_safe_budget_kbps: 0,
            pin: None,
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

    pub(crate) fn current(&self) -> Rung {
        self.current
    }

    #[cfg(test)]
    pub(crate) fn pending(&self) -> Option<Proposal> {
        self.pending
    }

    pub(crate) fn catalog(&self) -> HlsActuatorCatalog {
        self.catalog
    }

    pub(crate) fn delivery(&self) -> CapacityEstimate {
        self.delivery
    }

    pub(crate) fn buffer(&self) -> BufferEstimate {
        self.buffer
    }

    /// A pause is wall-clock time with no measurement — the one gap where the estimate really has
    /// aged (backpressure is not: a full buffer stops the reader on purpose).
    pub(crate) fn on_resume(&mut self, paused_ms: u64) {
        self.delivery.age_ms(paused_ms, &self.policy);
        self.buffer = BufferEstimate::default();
        self.stable_samples = 0;
    }

    /// Everything one decision was made on, in one struct, for one event-log line. Assembled here
    /// rather than in `ff.rs` so the numbers logged are the numbers used.
    pub(crate) fn telemetry(&self) -> ControllerTelemetry {
        let current = self.catalog.candidate(self.current);
        ControllerTelemetry {
            current: self.current,
            safe_budget_kbps: self.last_safe_budget_kbps,
            // The SAME selection [`Self::observe`]'s upshift arm makes, four fifths of the budget
            // included — so the read-out cannot advertise an operating point the controller would
            // not actually choose. It is the answer to "what is this link worth", which is a
            // different question from "what is playing" and the one a viewer photographing the
            // panel is usually asking.
            optimal: self.catalog.best_sustainable(
                self.last_safe_budget_kbps * 4 / 5,
                &self.production,
                current,
                &self.policy,
            ),
            delivery: self.delivery,
            production: self.production,
            buffer: self.buffer,
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
        }
    }

    pub(crate) fn observe(&mut self, sample: SegmentSample) -> Decision {
        let ratio = sample.production_ratio_pm();
        let current_candidate = self.catalog.candidate(self.current);
        // A segment at a low rung on a fast link is too small to time; clamp what it may claim to
        // what it can actually support. See `CapacityObservation::clamped_to_evidence`.
        let observation = CapacityObservation {
            kbps: sample.network_kbps(),
            bytes: sample.bytes,
            active_us: sample.active_fetch_us,
            completed: true,
        }
        .clamped_to_evidence(current_candidate.expected_wire_kbps);
        let network = observation.kbps;
        if observation.is_collapse(&self.delivery) {
            self.delivery.collapse(network);
        }
        self.delivery.update(observation);
        let cold_start = self.samples_on_rung == 0;
        self.production
            .observe(ratio, current_candidate.production_load_pm, cold_start);
        self.buffer
            .update(sample.buffer.buffered_ms(), i64::from(sample.media_duration_ms));
        self.samples_on_rung = self.samples_on_rung.saturating_add(1);

        let buffered = self.buffer.buffered_ms;
        let draining = self.buffer.draining();
        let segment = i64::from(sample.media_duration_ms);

        if self.pending.is_some() {
            return Decision::Stay;
        }
        if self.cooldown > 0 {
            self.cooldown -= 1;
        }
        // The dev pin (`pinned_to`) short-circuits the decision and NOTHING above it: every
        // estimator has already taken this segment. Reaching the pinned rung goes through the
        // ordinary prime/validate/commit transaction, so a pinned run exercises the real transport
        // path rather than a shortcut into it.
        if let Some(pin) = self.pin {
            if self.current == pin {
                return Decision::Stay;
            }
            // Wait for a reserve the transaction can be paid out of; see PIN_MIN_RESERVE_SEGMENTS.
            if buffered < segment.saturating_mul(PIN_MIN_RESERVE_SEGMENTS) {
                return Decision::Stay;
            }
            let direction = if pin.kbps() > self.current.kbps() { Direction::Up } else { Direction::Down };
            let proposal = Proposal { rung: pin, direction };
            self.pending = Some(proposal);
            return Decision::Prime(proposal);
        }

        // Fast-down: either current-sustainability signal may fail. A JIT ratio around 1.0 is
        // merely real-time production; it forces a move only when the content reserve is draining.
        let immediate_network = network.min(self.delivery.fast_kbps);
        let current_risk = candidate_risk(
            current_candidate,
            current_candidate,
            &self.delivery,
            &self.production,
            &self.buffer,
            &self.policy,
        );
        let network_bad = immediate_network < current_candidate.expected_wire_kbps;
        let production_bad =
            current_risk.production_risk && self.buffer.draining_samples >= 8;
        let safe_budget =
            hls_safe_budget(&self.delivery, &self.production, &self.buffer, &self.policy);
        self.last_safe_budget_kbps = safe_budget;
        let buffer_bad = buffered < segment || self.buffer.starving();
        if buffer_bad || network_bad || production_bad {
            self.stable_samples = 0;
            // A measured link collapse must not walk the ladder one oversized encoder at a time.
            // Select the best actuator that fits the new safe budget, still bounded below current.
            let target = if network_bad || buffered < segment / 2 {
                self.catalog
                    .best_for_budget(self.delivery.conservative_kbps())
                    .map(|candidate| candidate.rung)
                    .unwrap_or(Rung::P240)
                    .min(self.current.below())
            } else {
                self.current.below()
            };
            if target != self.current {
                let proposal = Proposal { rung: target, direction: Direction::Down };
                self.pending = Some(proposal);
                self.last_reason = Some(DecisionReason::Hls(if network_bad {
                    HlsReason::UnsafeCurrentState
                } else if production_bad {
                    HlsReason::ProductionConstraint
                } else {
                    HlsReason::BufferConstraint
                }));
                return Decision::Prime(proposal);
            }
            return Decision::Stay;
        }

        if self.cooldown > 0 || self.samples_on_rung < 2 {
            self.stable_samples = 0;
            return Decision::Stay;
        }
        // TWO independent constraints, deliberately not collapsed into one budget: the network has
        // to carry the bits AND the server has to produce them ahead of real time. This is what
        // refuses 4K on a fast link in front of a loaded PMS — the measured 4K point costs 4% more
        // wire and 110% more server, so a bitrate-only budget would wave it through.
        let Some(target_candidate) = self.catalog.best_sustainable(
            safe_budget * 4 / 5,
            &self.production,
            current_candidate,
            &self.policy,
        ) else {
            self.stable_samples = 0;
            return Decision::Stay;
        };
        let target = target_candidate.rung;
        if target == self.current {
            self.stable_samples = 0;
            return Decision::Stay;
        }
        if target < self.current {
            // The budget shrank without any current-state signal failing. Nothing is wrong with
            // what is playing, so this is not a downshift trigger — it is a reason not to climb.
            self.stable_samples = 0;
            return Decision::Stay;
        }

        // Upshift requires every resource signal to pass simultaneously. The target is selected
        // directly from the actuator catalog, so 8 -> 14-class budgets skip intermediate encoders.
        let all_good = safe_budget >= target_candidate.expected_wire_kbps
            && self.production.ratio_pm <= self.policy.production_safe_pm
            && buffered >= segment.saturating_mul(3)
            && !draining;
        if !all_good {
            self.stable_samples = 0;
            return Decision::Stay;
        }
        self.stable_samples = self.stable_samples.saturating_add(1);
        if self.stable_samples < 3 {
            return Decision::Stay;
        }
        self.stable_samples = 0;
        let proposal = Proposal { rung: target, direction: Direction::Up };
        self.pending = Some(proposal);
        self.last_reason = Some(DecisionReason::Hls(HlsReason::SafeBudgetIncrease));
        Decision::Prime(proposal)
    }

    /// Candidate-session acceptance. Downshifts need a decodable complete segment and a surviving
    /// reserve; upshifts additionally require both network and JIT-production headroom at the
    /// candidate's actual rendition. The controller still does not mutate until `commit`.
    pub(crate) fn candidate_ready(&self, proposal: Proposal, sample: SegmentSample) -> bool {
        if self.pending != Some(proposal) {
            return false;
        }
        let buffered = sample.buffer.buffered_ms();
        let segment = i64::from(sample.media_duration_ms);
        if buffered < segment {
            return false;
        }
        match proposal.direction {
            Direction::Down => true,
            Direction::Up => {
                let candidate = self.catalog.candidate(proposal.rung);
                sample.network_kbps() >= candidate.expected_wire_kbps
                    && sample.production_ratio_pm() <= 800
                    && buffered >= segment.saturating_mul(2)
            }
        }
    }

    pub(crate) fn commit(&mut self, proposal: Proposal) -> bool {
        if self.pending != Some(proposal) {
            return false;
        }
        self.current = proposal.rung;
        self.pending = None;
        self.samples_on_rung = 0;
        self.stable_samples = 0;
        self.cooldown = match proposal.direction {
            Direction::Down => 8,
            Direction::Up => 3,
        };
        true
    }

    pub(crate) fn reject(&mut self, proposal: Proposal) -> bool {
        if self.pending != Some(proposal) {
            return false;
        }
        self.pending = None;
        self.stable_samples = 0;
        self.cooldown = 1;
        true
    }
}

/// The closed-loop plant (I0-B/C). Host-only: it never ships.
#[cfg(test)]
mod sim;

#[cfg(test)]
mod tests {
    use super::*;

    /// A segment described by what actually crossed the wire, for the cases where the SIZE of the
    /// transfer is the point rather than the rate it works out to.
    fn sample_bytes(bytes: u64, active_us: u64, ratio_pm: u32, buffered_ms: i64) -> SegmentSample {
        let media_ms = 2_000;
        let total_us = u64::from((media_ms * ratio_pm / 1_000).max(1)) * 1_000;
        SegmentSample::new(
            bytes,
            active_us,
            total_us.max(active_us),
            media_ms,
            BufferSnapshot {
                playback: MediaTimeMs(10_000),
                video_tail: MediaTimeMs(10_000 + buffered_ms),
                audio_tail: Some(MediaTimeMs(10_000 + buffered_ms)),
                audio_expected: true,
            },
        )
        .unwrap()
    }

    fn sample(network_kbps: u32, ratio_pm: u32, buffered_ms: i64) -> SegmentSample {
        let media_ms = 2_000;
        let total_us = u64::from((media_ms * ratio_pm / 1_000).max(1)) * 1_000;
        let active_us = total_us.min(200_000);
        let bytes = u64::from(network_kbps) * active_us / 8_000;
        SegmentSample::new(
            bytes,
            active_us,
            total_us,
            media_ms,
            BufferSnapshot {
                playback: MediaTimeMs(10_000),
                video_tail: MediaTimeMs(10_000 + buffered_ms),
                audio_tail: Some(MediaTimeMs(10_000 + buffered_ms)),
                audio_expected: true,
            },
        )
        .unwrap()
    }

    /// The ordinary case: a 1080p source on a device that decodes it. Most tests want this,
    /// because a catalog that reaches 4K reaches it only when the SOURCE does — and asserting on
    /// an unbounded catalog would grade a configuration no real playback has.
    fn hd_catalog() -> HlsActuatorCatalog {
        HlsActuatorCatalog::measured().limited_to((3840, 2176), (1920, 1080))
    }

    /// A UHD source on the dev set's own decode bound, which is what makes the 4K actuator
    /// feasible at all.
    fn uhd_catalog() -> HlsActuatorCatalog {
        HlsActuatorCatalog::measured().limited_to((3840, 2176), (3840, 2160))
    }

    fn controller_at(rung: Rung) -> Controller {
        Controller::starting_at(rung, None, hd_catalog())
    }

    /// Unknown links start at 480p/720 kbit/s; P240 stays available as the emergency floor.
    fn bootstrap_controller() -> Controller {
        controller_at(Rung::P480)
    }

    fn original(source_kbps: u32) -> OriginalModeController {
        OriginalModeController::new(
            source_kbps,
            AbrPolicy::measured(),
            hd_catalog(),
            TransitionHistory::default(),
            false,
        )
        .unwrap()
    }

    /// Bytes that make one 750 ms window measure exactly `kbps`.
    fn window_bytes(kbps: u64) -> u64 {
        kbps * ORIGINAL_WINDOW_US / 8_000
    }

    const HOUR_MS: i64 = 3_600_000;

    fn prime_up(controller: &mut Controller) -> Proposal {
        for _ in 0..4 {
            if let Decision::Prime(proposal) = controller.observe(sample(20_000, 200, 10_000)) {
                return proposal;
            }
        }
        panic!("no proposal")
    }

    fn settle_link(network_kbps: u32) -> Rung {
        let mut controller = bootstrap_controller();
        for _ in 0..80 {
            if let Decision::Prime(proposal) =
                controller.observe(sample(network_kbps, 400, 10_000))
            {
                let candidate = sample(network_kbps, 400, 12_000);
                if controller.candidate_ready(proposal, candidate) {
                    controller.commit(proposal);
                } else {
                    controller.reject(proposal);
                }
            }
        }
        controller.current()
    }

    #[test]
    fn zero_or_inconsistent_measurements_are_not_infinite_headroom() {
        let b = BufferSnapshot {
            playback: MediaTimeMs(0),
            video_tail: MediaTimeMs(5_000),
            audio_tail: None,
            audio_expected: false,
        };
        assert!(SegmentSample::new(1, 0, 1, 2_000, b).is_none());
        assert!(SegmentSample::new(1, 2, 1, 2_000, b).is_none());
        assert!(SegmentSample::new(0, 1, 1, 2_000, b).is_none());
        assert!(SegmentSample::new(1, 1, 1, 0, b).is_none());
    }

    #[test]
    fn audio_expected_without_a_tail_has_no_buffer_but_silent_video_does() {
        let mut b = BufferSnapshot {
            playback: MediaTimeMs(1_000),
            video_tail: MediaTimeMs(5_000),
            audio_tail: None,
            audio_expected: true,
        };
        assert_eq!(b.buffered_ms(), 0);
        b.audio_expected = false;
        assert_eq!(b.buffered_ms(), 4_000);
    }

    /// A window shorter than the measurement window is not a measurement.
    #[test]
    fn original_windows_shorter_than_the_measurement_window_are_not_evidence() {
        let mut mode = original(28_000);
        assert!(mode
            .observe(window_bytes(4_000), ORIGINAL_WINDOW_US - 1, Some(3_000), HOUR_MS)
            .is_none());
        let first = mode
            .observe(window_bytes(4_000), ORIGINAL_WINDOW_US, Some(3_000), HOUR_MS)
            .unwrap();
        assert_eq!(first.measured_kbps, 4_000);
        assert_eq!(first.requirement_kbps, 37_800, "28 Mbit/s average + VBR headroom");
    }

    /// **The 2026-08-25 rewrite, in one assertion.** 4 Mbit/s against a 28 Mbit/s file used to be
    /// "two slow windows, switch". Whether it is a problem depends entirely on the reserve, and
    /// the old rule could not see that: 60 s of buffer survives this deficit for a minute.
    #[test]
    fn a_deficit_with_a_deep_reserve_is_arithmetic_not_an_emergency() {
        let mut mode = original(28_000);
        for window in 1..=4 {
            let observation = mode
                .observe(
                    window_bytes(4_000) * window,
                    ORIGINAL_WINDOW_US * window,
                    Some(60_000),
                    HOUR_MS,
                )
                .unwrap();
            assert!(
                observation.fallback.is_none(),
                "window {window}: {}s of headroom is not a reason to reload the pipeline",
                observation.horizon_secs.unwrap_or(0),
            );
            assert!(observation.horizon_secs.unwrap_or(0) >= 60);
        }
    }

    /// The same rate against the same file with a shallow reserve IS an emergency, and the reason
    /// code says which rule fired. It also names the replacement state — the best the collapsed
    /// link sustains, never the bottom of the ladder.
    #[test]
    fn a_collapse_leaves_original_for_the_best_sustainable_state() {
        let mut mode = original(28_000);
        let observation = mode
            .observe(window_bytes(4_000), ORIGINAL_WINDOW_US, Some(8_000), HOUR_MS)
            .unwrap();
        assert_eq!(observation.fallback, Some(OriginalExit::ImminentStarvation));
        assert_eq!(observation.target, Some(Rung::P720Low), "3.2 Mbit/s of proven capacity");
    }

    /// A moderate deficit that will not go away eventually loses the argument on its own — before
    /// starvation is imminent, and with no counter deciding anything by itself.
    #[test]
    fn a_deficit_that_persists_costs_original_the_argument() {
        let mut mode = original(60_000);
        let mut exits = Vec::new();
        for window in 1..=14 {
            let observation = mode
                .observe(
                    window_bytes(50_000) * window,
                    ORIGINAL_WINDOW_US * window,
                    Some(30_000),
                    HOUR_MS,
                )
                .unwrap();
            assert!(
                observation.horizon_secs.unwrap_or(0) > AbrPolicy::measured().starvation_fallback_secs,
                "this scenario must never become imminent, or it grades the wrong rule",
            );
            exits.push((window, observation.fallback));
        }
        assert!(
            exits
                .iter()
                .take(usize::from(ORIGINAL_DEFICIT_WINDOWS) - 1)
                .all(|(_, exit)| exit.is_none()),
            "anything shorter than the persistence window is a dip: {exits:?}",
        );
        assert!(
            exits
                .iter()
                .any(|(_, exit)| *exit == Some(OriginalExit::SustainedDeficit)),
            "a shortfall that persists must eventually move: {exits:?}",
        );
    }

    /// The **labelled emergency guard**: estimates say the link is fine, the reserve says otherwise.
    /// Unreachable when the model works, which is exactly why it is named in the log.
    #[test]
    fn the_emergency_guard_fires_when_the_reserve_is_gone_anyway() {
        let mut mode = original(28_000);
        // Two agreeing windows are what make 60 Mbit/s believable enough to clear a 37.8 Mbit/s
        // requirement outright — one sample is discounted by half on principle.
        for window in 1..=2 {
            let healthy = mode
                .observe(
                    window_bytes(60_000) * window,
                    ORIGINAL_WINDOW_US * window,
                    Some(5_000),
                    HOUR_MS,
                )
                .unwrap();
            assert!(healthy.fallback.is_none());
        }
        assert_eq!(
            mode.observe(
                window_bytes(60_000) * 3,
                ORIGINAL_WINDOW_US * 3,
                Some(5_000),
                HOUR_MS,
            )
            .unwrap()
            .horizon_secs,
            None,
            "60 Mbit/s carries a 28 Mbit/s file, and by now the estimate agrees",
        );
        let collapsed = mode
            .observe(
                window_bytes(60_000) * 4,
                ORIGINAL_WINDOW_US * 4,
                Some(1_500),
                HOUR_MS,
            )
            .unwrap();
        assert_eq!(collapsed.horizon_secs, None, "the delivery estimate still says it is fine");
        assert_eq!(collapsed.fallback, Some(OriginalExit::EmergencyLowBuffer));
    }

    /// Nothing can starve a reserve that outlasts the content, which is also why the closing
    /// minutes need no special case anywhere.
    #[test]
    fn a_reserve_that_covers_the_rest_of_the_film_never_falls_back() {
        let mut mode = original(28_000);
        let observation = mode
            .observe(window_bytes(1_000), ORIGINAL_WINDOW_US, Some(20_000), 15_000)
            .unwrap();
        assert!(observation.horizon_secs.unwrap_or(u32::MAX) < 60, "a real deficit");
        assert!(observation.fallback.is_none(), "20 s buffered, 15 s left to play");
    }

    /// A seek keeps the link estimate and drops everything positional. The link did not change
    /// because the viewer jumped.
    #[test]
    fn a_seek_keeps_the_link_estimate_and_drops_the_position() {
        let mut mode = original(28_000);
        for window in 1..=3 {
            mode.observe(
                window_bytes(50_000) * window,
                ORIGINAL_WINDOW_US * window,
                Some(30_000),
                HOUR_MS,
            );
        }
        let before = mode.delivery;
        mode.on_seek(0, 0);
        assert_eq!(mode.delivery, before, "a seek is not news about the network");
        assert_eq!(mode.buffer, BufferEstimate::default());
        assert_eq!(mode.deficit_windows, 0);
        // ...and the counters really did rewind, so the next window measures from zero rather than
        // reading a negative delta as a collapse.
        assert!(mode
            .observe(window_bytes(50_000), ORIGINAL_WINDOW_US, Some(1_000), HOUR_MS)
            .is_some());
    }

    /// A pause is the one gap where wall-clock time passes with nothing measured.
    #[test]
    fn a_long_pause_turns_the_estimate_into_a_weak_prior() {
        let mut mode = original(28_000);
        for window in 1..=4 {
            mode.observe(
                window_bytes(50_000) * window,
                ORIGINAL_WINDOW_US * window,
                Some(30_000),
                HOUR_MS,
            );
        }
        let confident = mode.delivery.conservative_kbps();
        mode.on_resume(10 * 60 * 1_000);
        let stale = mode.delivery.conservative_kbps();
        assert!(stale < confident, "{stale} vs {confident}");
        assert_eq!(mode.delivery.slow_kbps, 50_000, "the VALUE survives; the confidence does not");
        assert_eq!(mode.delivery.samples, 1);
    }

    #[test]
    fn measured_runtime_fallback_avoids_an_unnecessarily_low_bootstrap() {
        let policy = AbrPolicy::measured();
        let rung = |measured| original_fallback_rung(measured, &hd_catalog(), &policy);
        assert_eq!(rung(512), Rung::P240);
        assert_eq!(rung(4_000), Rung::P720Low);
        assert_eq!(rung(7_000), Rung::P720);
        assert_eq!(rung(30_000), Rung::P1080High, "a fast link is not a reason to hold back");
        assert_eq!(
            rung(1),
            Rung::P240,
            "below every candidate, take the floor rather than refusing to move",
        );
        assert_eq!(Rung::from_ceiling(Rung::P720Low.ceiling()), Some(Rung::P720Low));
    }

    #[test]
    fn realtime_jit_blocks_upshift_without_forcing_a_downshift() {
        let mut controller = bootstrap_controller();
        for _ in 0..10 {
            assert_eq!(controller.observe(sample(20_000, 1_000, 10_000)), Decision::Stay);
        }
        assert_eq!(controller.current(), Rung::P480);
    }

    #[test]
    fn a_proposal_does_not_mutate_current_until_candidate_commit() {
        let mut controller = bootstrap_controller();
        let proposal = prime_up(&mut controller);
        assert_eq!(proposal.rung, Rung::P1080M12);
        assert_eq!(controller.current(), Rung::P480);
        assert_eq!(controller.pending(), Some(proposal));
        assert!(controller.candidate_ready(proposal, sample(20_000, 200, 12_000)));
        assert!(controller.commit(proposal));
        assert_eq!(controller.current(), Rung::P1080M12);
    }

    #[test]
    fn rejected_candidate_preserves_current_and_clears_pending() {
        let mut controller = bootstrap_controller();
        let proposal = prime_up(&mut controller);
        assert!(!controller.candidate_ready(proposal, sample(2_100, 950, 12_000)));
        assert!(controller.reject(proposal));
        assert_eq!(controller.current(), Rung::P480);
        assert_eq!(controller.pending(), None);
    }

    #[test]
    fn startup_does_not_issue_back_to_back_encoder_swaps() {
        let mut controller = bootstrap_controller();
        let proposal = prime_up(&mut controller);
        controller.commit(proposal);
        for _ in 0..3 {
            assert_eq!(controller.observe(sample(20_000, 200, 12_000)), Decision::Stay);
        }
    }

    /// One slow sample is acted on immediately — a downshift is an invisible transaction and the
    /// alternative is a stall — but it is acted on CONSERVATIVELY: a single measurement carries the
    /// maximum discount, so 1 Mbit/s is treated as 0.5 Mbit/s of proven capacity and the target is
    /// the emergency floor rather than the rung just below. The next agreeing samples are what buy
    /// the way back up.
    #[test]
    fn a_single_slow_network_sample_jumps_to_the_measured_sustainable_rung() {
        let mut controller = bootstrap_controller();
        controller.current = Rung::P720;
        let decision = controller.observe(sample(1_000, 400, 8_000));
        assert_eq!(
            decision,
            Decision::Prime(Proposal { rung: Rung::P240, direction: Direction::Down })
        );
        assert_eq!(controller.current(), Rung::P720);
    }

    #[test]
    fn a_runtime_collapse_from_the_top_does_not_prime_oversized_intermediate_rungs() {
        let mut controller = bootstrap_controller();
        controller.current = Rung::P1080High;
        assert_eq!(
            controller.observe(sample(512, 1_000, 8_000)),
            Decision::Prime(Proposal { rung: Rung::P240, direction: Direction::Down })
        );
    }

    #[test]
    fn draining_jit_session_downshifts_but_stable_jit_does_not() {
        let mut controller = bootstrap_controller();
        assert_eq!(controller.observe(sample(20_000, 1_200, 8_000)), Decision::Stay);
        assert_eq!(controller.observe(sample(20_000, 1_200, 6_000)), Decision::Stay);
        assert_eq!(controller.observe(sample(20_000, 1_200, 4_000)), Decision::Stay);
        assert_eq!(controller.observe(sample(20_000, 1_200, 3_000)), Decision::Stay);
        assert_eq!(controller.observe(sample(20_000, 1_200, 2_500)), Decision::Stay);
        assert_eq!(controller.observe(sample(20_000, 1_200, 2_000)),
            Decision::Prime(Proposal { rung: Rung::P240, direction: Direction::Down }));
    }

    fn recovery(source_kbps: u32) -> OriginalRecovery {
        OriginalRecovery::new(
            source_kbps,
            AbrPolicy::measured(),
            false,
            TransitionHistory::default(),
        )
        .unwrap()
    }

    /// The HLS session the recovery decision is compared AGAINST — healthy, since that is the
    /// interesting case: a starved one would make Original win by default.
    fn healthy_hls() -> CapacityEstimate {
        CapacityEstimate::from_prior(30_000)
    }

    fn healthy_buffer() -> BufferEstimate {
        BufferEstimate { buffered_ms: 12_000, slope_ms_per_s: 0, ..Default::default() }
    }

    fn probe(kbps: u32, completed: bool) -> CapacityObservation {
        CapacityObservation { kbps, bytes: 2_000_000, active_us: 400_000, completed }
    }

    /// **A mid-ladder rung with spare capacity may probe.** The old gate required the TOP rung,
    /// which measured the wrong resource: PMS producing 20 Mbit/s of H.264 says the server can
    /// encode, not that the link can carry a 28 Mbit/s remux.
    #[test]
    fn original_recovery_probes_from_any_rung_with_measured_headroom() {
        let mut gate = recovery(28_000);
        let current = hd_catalog().candidate(Rung::P720);
        let spare = CapacityEstimate::from_prior(30_000);
        for n in 1..=ORIGINAL_PROBE_SPACING {
            assert_eq!(
                gate.probe_due(current, sample(20_000, 500, 10_000), healthy_buffer(), &spare, HOUR_MS),
                n == ORIGINAL_PROBE_SPACING,
                "spacing window {n}",
            );
        }
    }

    /// No measurable headroom, a thin reserve, or a draining one: no probe, whatever the rung. A
    /// probe reads real bytes over the link the segments need.
    #[test]
    fn original_recovery_refuses_to_probe_without_room_to_do_it_safely() {
        let current = hd_catalog().candidate(Rung::P1080High);
        let spare = CapacityEstimate::from_prior(60_000);
        let no_headroom = CapacityEstimate::from_prior(20_011);
        for _ in 0..ORIGINAL_PROBE_SPACING * 2 {
            assert!(
                !recovery(28_000).probe_due(
                    current,
                    sample(60_000, 500, 10_000),
                    healthy_buffer(),
                    &no_headroom,
                    HOUR_MS,
                ),
                "segments prove a LOWER bound; at the wire rate there is no evidence of more",
            );
            assert!(
                !recovery(28_000).probe_due(
                    current,
                    sample(60_000, 500, 2_000),
                    healthy_buffer(),
                    &spare,
                    HOUR_MS,
                ),
                "one segment of reserve is not room to spend on a measurement",
            );
            assert!(
                !recovery(28_000).probe_due(
                    current,
                    sample(60_000, 500, 10_000),
                    BufferEstimate { buffered_ms: 12_000, slope_ms_per_s: -400, ..Default::default() },
                    &spare,
                    HOUR_MS,
                ),
                "a draining reserve is not the moment to add a second transfer",
            );
        }
    }

    /// **Confidence, not a count.** The requirement here is 37.8 Mbit/s (28 Mbit/s average plus VBR
    /// headroom), and what has to clear it is the estimate DISCOUNTED BY ITS OWN UNCERTAINTY. So a
    /// decisive probe recovers alone, a marginal one has to be confirmed, and the number of probes
    /// is an output of the rule rather than part of it.
    #[test]
    fn original_recovery_is_decided_by_confidence_rather_than_probe_count() {
        let mut decisive = recovery(28_000);
        assert_eq!(
            decisive.observe_probe(probe(80_000, true), healthy_buffer(), &healthy_hls(), HOUR_MS),
            RecoveryVerdict::Recover,
            "80 Mbit/s leaves nothing for a second probe to add",
        );

        let mut marginal = recovery(28_000);
        let verdicts: Vec<RecoveryVerdict> = (0..3)
            .map(|_| marginal.observe_probe(probe(50_000, true), healthy_buffer(), &healthy_hls(), HOUR_MS))
            .collect();
        assert_eq!(
            verdicts,
            vec![
                RecoveryVerdict::Insufficient,
                RecoveryVerdict::Insufficient,
                RecoveryVerdict::Recover,
            ],
            "50 Mbit/s is only 1.3x the requirement, so it takes agreement to believe",
        );
    }

    /// A truncated probe is an ABSENT measurement, not a slow link: folding its rate in would
    /// poison the next decision with a number no transfer ever sustained.
    #[test]
    fn a_truncated_probe_is_absence_of_evidence() {
        let mut gate = recovery(28_000);
        assert_eq!(
            gate.observe_probe(probe(2_000, false), healthy_buffer(), &healthy_hls(), HOUR_MS),
            RecoveryVerdict::Insufficient,
        );
        assert_eq!(
            gate.observe_probe(probe(80_000, true), healthy_buffer(), &healthy_hls(), HOUR_MS),
            RecoveryVerdict::Recover,
            "the aborted attempt left no trace to drag the estimate down",
        );
    }

    /// The benefit of Original accrues over the remaining playback; the reload is paid once, now.
    #[test]
    fn recovery_does_not_pay_for_a_reload_at_the_end_of_a_film() {
        let mut gate = recovery(28_000);
        assert_eq!(
            gate.observe_probe(probe(80_000, true), healthy_buffer(), &healthy_hls(), 8_000),
            RecoveryVerdict::NotWorthIt,
        );
        let current = hd_catalog().candidate(Rung::P720);
        let spare = CapacityEstimate::from_prior(80_000);
        for _ in 0..ORIGINAL_PROBE_SPACING * 2 {
            assert!(
                !recovery(28_000).probe_due(
                    current,
                    sample(20_000, 500, 10_000),
                    healthy_buffer(),
                    &spare,
                    8_000,
                ),
                "and it does not spend a probe finding that out",
            );
        }
    }

    #[test]
    fn a_downshift_holds_long_enough_to_avoid_immediate_top_rung_flapping() {
        let mut controller = controller_at(Rung::P1080High);
        let Decision::Prime(down) = controller.observe(sample(12_000, 500, 8_000)) else {
            panic!("the collapsed link must propose a downshift")
        };
        assert!(controller.commit(down));
        for _ in 0..9 {
            assert_eq!(controller.observe(sample(60_000, 400, 10_000)), Decision::Stay);
        }
        assert_eq!(
            controller.observe(sample(60_000, 400, 10_000)),
            Decision::Prime(Proposal { rung: Rung::P1080High, direction: Direction::Up }),
        );
    }

    #[test]
    fn the_auto_ladder_matches_fixed_quality_wire_values() {
        assert_eq!(Rung::P480.kbps(), 720);
        assert_eq!(Rung::P720Low.kbps(), 2_000);
        assert_eq!(Rung::P720.kbps(), 4_000);
        assert_eq!(Rung::P1080.kbps(), 8_000);
        assert_eq!(Rung::P1080High.kbps(), 20_000);
        assert_eq!(Rung::P1080High.raster(), (1920, 1080));
        // The ladder must stay sorted and unique on the request axis: `below()` walks it by index
        // and `from_ceiling` recovers a rung from a stored ceiling by exact match.
        for pair in LADDER.windows(2) {
            assert!(pair[0].kbps() < pair[1].kbps(), "{:?} then {:?}", pair[0], pair[1]);
        }
        for rung in LADDER {
            assert_eq!(Rung::from_ceiling(rung.ceiling()), Some(rung));
        }
    }

    /// The measured 4K operating point: the REQUEST is 22 Mbps and the OUTPUT is neither 22 Mbit/s
    /// nor 1080p. Asking for 20,895 would get 1080p; asking for 22,000 gets 3840x2160 at about
    /// 20,895 kbit/s. Both halves have to be stored or a budget is spent on the wrong thing.
    #[test]
    fn the_uhd_actuator_separates_its_request_from_its_measured_output() {
        let candidate = uhd_catalog().candidate(Rung::Uhd);
        assert_eq!(candidate.request_kbps, 22_000);
        assert_eq!(candidate.expected_wire_kbps, 20_895);
        assert_eq!(candidate.rung.raster(), (3840, 2160));
        let high = uhd_catalog().candidate(Rung::P1080High);
        assert_eq!(high.expected_wire_kbps, 20_011);
        // 4% more wire, 110% more server. The whole reason production is a separate constraint.
        assert!(candidate.expected_wire_kbps < high.expected_wire_kbps * 11 / 10);
        assert_eq!(candidate.production_load_pm, high.production_load_pm * 21 / 10);
    }

    /// Feasibility is a FILTER, applied before anything is scored — and it removes 4K for two
    /// independent reasons, either of which is enough.
    #[test]
    fn an_infeasible_raster_is_removed_before_any_scoring() {
        let huge_budget = 60_000;
        assert_eq!(
            uhd_catalog().best_for_budget(huge_budget).map(|c| c.rung),
            Some(Rung::Uhd),
        );
        assert_eq!(
            hd_catalog().best_for_budget(huge_budget).map(|c| c.rung),
            Some(Rung::P1080High),
            "a 1080p source must not make PMS upscale — 2.1x the server work for no picture",
        );
        let hd_only_device = HlsActuatorCatalog::measured().limited_to((1920, 1088), (3840, 2160));
        assert_eq!(
            hd_only_device.best_for_budget(huge_budget).map(|c| c.rung),
            Some(Rung::P1080High),
            "a 1080p-limited decoder cannot be talked into 4K by a fast link",
        );
        // A zero on either axis is "nobody said", not a forbidden zero-pixel picture.
        let unmeasured = HlsActuatorCatalog::measured().limited_to((3840, 2176), (0, 0));
        assert_eq!(unmeasured.best_for_budget(huge_budget).map(|c| c.rung), Some(Rung::Uhd));
        assert!(HlsActuatorCatalog::measured()
            .limited_to((0, 0), (0, 0))
            .best_for_budget(320)
            .is_some());
    }

    /// **A rung's raster is a bounding box.** Measured on the television against a real library
    /// item: a 1918x802 scope film had every 1080p rung ruled infeasible by a per-axis test (1080 >
    /// 802), so Auto capped at 4 Mbps / 720p on a gigabit LAN — for most films, since most films
    /// are wider than 16:9.
    #[test]
    fn a_scope_source_keeps_every_rung_that_would_not_scale_it() {
        let scope = HlsActuatorCatalog::measured().limited_to((3840, 2176), (1918, 802));
        let rungs: Vec<Rung> = scope.feasible().map(|c| c.rung).collect();
        assert!(
            rungs.contains(&Rung::P1080High) && rungs.contains(&Rung::P1080M12),
            "PMS fits 1918x802 inside a 1920x1080 box and never upscales: {rungs:?}",
        );
        assert!(
            !rungs.contains(&Rung::Uhd),
            "but a 4K box buys the same picture, priced with a 4K server load: {rungs:?}",
        );
        assert!(rungs.contains(&Rung::P720), "and the real downscale steps all survive");
        assert_eq!(
            scope.best_for_budget(60_000).map(|c| c.rung),
            Some(Rung::P1080High),
            "so a fast link spends its budget on bits rather than stopping at 720p",
        );

        // The same rule on a 4K source keeps the 4K point, because nothing smaller covers it.
        let uhd = HlsActuatorCatalog::measured().limited_to((3840, 2176), (3840, 2160));
        assert_eq!(uhd.best_for_budget(60_000).map(|c| c.rung), Some(Rung::Uhd));
        // ...and on a scope 4K master, where the box matches on width alone.
        let uhd_scope = HlsActuatorCatalog::measured().limited_to((3840, 2176), (3840, 1600));
        assert_eq!(uhd_scope.best_for_budget(60_000).map(|c| c.rung), Some(Rung::Uhd));
    }

    /// A 2 s segment at the 320 kbps floor is 80 KB; a LAN delivers it in under a millisecond, and
    /// the honest arithmetic then reads 865 Gbit/s — which is what the television reported, and
    /// what every budget downstream was computed from.
    #[test]
    fn a_transfer_too_short_to_time_cannot_claim_a_gigabit_link() {
        let floor = hd_catalog().candidate(Rung::P240);
        let tiny = CapacityObservation {
            kbps: 865_219,
            bytes: 80_000,
            active_us: 700,
            completed: true,
        };
        assert_eq!(tiny.quality(), ObservationQuality::Weak);
        let clamped = tiny.clamped_to_evidence(floor.expected_wire_kbps);
        assert_eq!(clamped.kbps, floor.expected_wire_kbps * 8, "a bounded claim, not a fantasy");
        assert!(
            clamped.kbps > hd_catalog().candidate(Rung::P720Low).expected_wire_kbps,
            "and still enough to climb out of the floor promptly",
        );
        // A transfer big enough to time is left alone, whatever it says.
        let real = CapacityObservation {
            kbps: 90_000,
            bytes: 4_000_000,
            active_us: 355_000,
            completed: true,
        };
        assert_eq!(real.clamped_to_evidence(floor.expected_wire_kbps), real);

        // End to end: the floor rung on a fast LAN climbs instead of parking.
        let mut controller = controller_at(Rung::P240);
        let mut reached = Rung::P240;
        for _ in 0..40 {
            let segment = sample_bytes(80_000, 700, 400, 12_000);
            if let Decision::Prime(proposal) = controller.observe(segment) {
                if controller.candidate_ready(proposal, sample(20_000, 400, 12_000)) {
                    controller.commit(proposal);
                    reached = controller.current();
                } else {
                    controller.reject(proposal);
                }
            }
        }
        assert!(reached > Rung::P240, "a LAN must not leave Auto on the emergency floor");
    }

    /// **Network yes, server no.** The two constraints are evaluated independently, so a fast link
    /// in front of a PMS already near real time cannot commit the 4K point.
    #[test]
    fn a_fast_link_in_front_of_a_loaded_server_does_not_choose_4k() {
        let catalog = uhd_catalog();
        let current = catalog.candidate(Rung::P1080High);
        let fast = CapacityEstimate::from_prior(80_000);
        let policy = AbrPolicy::measured();
        let buffer = BufferEstimate { buffered_ms: 20_000, ..Default::default() };

        let mut quick_server = ProductionEstimate::default();
        for _ in 0..4 {
            quick_server.observe(200, current.production_load_pm, false);
        }
        assert_eq!(
            catalog
                .best_sustainable(60_000, &quick_server, current, &policy)
                .map(|c| c.rung),
            Some(Rung::Uhd),
            "a server at a fifth of real time has room for the measured 2.1x",
        );

        let mut loaded_server = ProductionEstimate::default();
        for _ in 0..4 {
            loaded_server.observe(700, current.production_load_pm, false);
        }
        assert_eq!(
            catalog
                .best_sustainable(60_000, &loaded_server, current, &policy)
                .map(|c| c.rung),
            Some(Rung::P1080High),
            "0.7 of real time on 1080p predicts 1.47 on 4K — behind, whatever the link does",
        );
        assert!(
            candidate_risk(
                catalog.candidate(Rung::Uhd),
                current,
                &fast,
                &loaded_server,
                &buffer,
                &policy,
            )
            .production_risk,
            "and the risk model says so too, so the two agree rather than one overriding the other",
        );
    }

    /// A budget jump primes the far candidate ONCE, instead of paying for three encoder creations
    /// to walk 10, 12, 14.
    #[test]
    fn a_budget_jump_skips_the_intermediate_encoders() {
        let mut controller = controller_at(Rung::P1080);
        let mut proposal = None;
        for _ in 0..6 {
            if let Decision::Prime(p) = controller.observe(sample(22_000, 200, 12_000)) {
                proposal = Some(p);
                break;
            }
        }
        assert_eq!(
            proposal,
            Some(Proposal { rung: Rung::P1080M14, direction: Direction::Up }),
            "8 Mbps now, a ~14 Mbit/s safe budget: one prime, not three",
        );
    }

    #[test]
    fn only_upshift_primes_receive_the_exact_acceptance_budget() {
        let media = std::time::Duration::from_millis(2_002);
        let up = Proposal { rung: Rung::P720Low, direction: Direction::Up };
        let down = Proposal { rung: Rung::P240, direction: Direction::Down };
        assert_eq!(
            candidate_prime_budget(up, media),
            Some(std::time::Duration::from_micros(1_601_600))
        );
        assert_eq!(
            candidate_warmup_budget(up, media),
            Some(std::time::Duration::from_micros(3_003_000))
        );
        assert_eq!(candidate_prime_budget(down, media), None);
        assert_eq!(candidate_warmup_budget(down, media), None);
    }

    /// The four shaped links from the device session, re-graded on today's ladder. The 17.5 Mbit/s
    /// leg lands a rung higher than the 8 Mbps it committed on the six-rung ladder — that leg had
    /// to choose between 8 and 20 Mbps and spent 12 Mbit/s of a measured link on nothing.
    ///
    /// It also reaches it in TWO moves rather than one, and that is the production model being
    /// honest rather than a flaw: from 480p, extrapolating the server's cost five raster-steps
    /// ahead is a guess, so the controller takes the step its evidence supports and re-measures.
    /// Skipping intermediate encoders is bounded by what has actually been observed.
    #[test]
    fn lg_network_legs_settle_on_sustainable_rungs() {
        assert_eq!(settle_link(512), Rung::P240);
        assert_eq!(settle_link(1_200), Rung::P480);
        assert_eq!(settle_link(7_000), Rung::P720);
        assert_eq!(settle_link(17_500), Rung::P1080M10);
    }

    #[test]
    fn capacity_estimation_separates_stable_and_volatile_history() {
        let mut stable = CapacityEstimate::default();
        for kbps in [59_000, 60_000, 61_000, 60_000, 60_000] {
            stable.update(CapacityObservation {
                kbps,
                bytes: 2_000_000,
                active_us: 300_000,
                completed: true,
            });
        }
        let mut volatile = CapacityEstimate::default();
        for kbps in [60_000, 10_000, 60_000, 12_000, 60_000] {
            volatile.update(CapacityObservation {
                kbps,
                bytes: 2_000_000,
                active_us: 300_000,
                completed: true,
            });
        }
        assert!(stable.conservative_kbps() > volatile.conservative_kbps());
        assert!(stable.uncertainty_pm < volatile.uncertainty_pm);
    }

    #[test]
    fn a_sudden_collapse_reduces_confidence_in_the_old_slow_estimate() {
        let mut capacity = CapacityEstimate::default();
        for kbps in [80_000, 80_000, 80_000] {
            capacity.update(CapacityObservation {
                kbps,
                bytes: 2_000_000,
                active_us: 300_000,
                completed: true,
            });
        }
        let observation = CapacityObservation {
            kbps: 5_000,
            bytes: 2_000_000,
            active_us: 300_000,
            completed: true,
        };
        assert!(observation.is_collapse(&capacity));
        capacity.collapse(5_000);
        assert!(capacity.fast_kbps <= 5_000);
        assert!(capacity.uncertainty_pm > 250);
    }

    #[test]
    fn starvation_horizon_distinguishes_harmless_deficit_from_emergency() {
        // Capacity at or above the requirement: no network-driven depletion at all.
        assert_eq!(starvation_horizon(20_000, 40_000, 60_000).seconds, None);
        // A 1.7% shortfall against a minute of reserve is an hour away.
        let healthy = starvation_horizon(60_000, 60_000, 59_000);
        assert_eq!(healthy.seconds, Some(3_600));
        // A 12x shortfall against ten seconds is ten seconds away.
        let severe = starvation_horizon(10_000, 60_000, 5_000);
        assert_eq!(severe.seconds, Some(10));
        let current = HlsActuatorCatalog::measured().candidate(Rung::P1080High);
        assert!(candidate_risk(
            current,
            current,
            &CapacityEstimate { slow_kbps: 59_000, fast_kbps: 59_000, uncertainty_pm: 0, samples: 8 },
            &ProductionEstimate::default(),
            &BufferEstimate { buffered_ms: 60_000, slope_ms_per_s: 0, samples: 8, ..Default::default() },
            &AbrPolicy::measured(),
        )
        .score < 5);
    }

    #[test]
    fn a_safe_budget_selects_the_best_actuator_directly() {
        let catalog = hd_catalog();
        assert_eq!(catalog.best_for_budget(15_000).map(|c| c.rung), Some(Rung::P1080M14));
        assert_eq!(catalog.best_for_budget(3_000).map(|c| c.rung), Some(Rung::P720Low));
        assert_eq!(catalog.candidate(Rung::P1080High).expected_wire_kbps, 20_011);
        assert_eq!(catalog.best_for_budget(100).map(|c| c.rung), None, "nothing fits, and it says so");
    }

    /// Throughput is a RATE, so the size and duration of the transfer decide how much it proves.
    #[test]
    fn observation_quality_weights_a_tiny_read_below_a_sustained_one() {
        let tiny = CapacityObservation { kbps: 100_000, bytes: 40_000, active_us: 3_000, completed: true };
        let normal = CapacityObservation { kbps: 20_000, bytes: 400_000, active_us: 160_000, completed: true };
        let sustained = CapacityObservation { kbps: 20_000, bytes: 4_000_000, active_us: 1_600_000, completed: true };
        let truncated = CapacityObservation { completed: false, ..sustained };
        assert_eq!(tiny.quality(), ObservationQuality::Weak);
        assert_eq!(normal.quality(), ObservationQuality::Normal);
        assert_eq!(sustained.quality(), ObservationQuality::Strong);
        assert_eq!(truncated.quality(), ObservationQuality::Weak, "a truncated read proves a floor");
        assert!(tiny.weight() < normal.weight() && normal.weight() < sustained.weight());

        // ...and the weighting is real: a Strong observation pulls the estimate harder than a
        // Normal one. The outlier stays INSIDE the regime factor on purpose — a bigger one would
        // restart the estimate instead of blending, which is a different rule (below).
        let settle = |first: CapacityObservation| {
            let mut estimate = CapacityEstimate::default();
            for _ in 0..6 {
                estimate.update(first);
            }
            estimate.update(CapacityObservation { kbps: 8_000, ..first });
            estimate.slow_kbps
        };
        assert!(settle(sustained) < settle(normal), "a strong sample pulls harder");
    }

    /// **The 2026-08-25 device finding.** A shaped leg ending is not an outlier to average in: the
    /// television's Original recovery probe measured 3,952 kbps during the slow leg and 28,116 kbps
    /// after it, blended to 9,993 against a 10,800 requirement, and Auto never returned to
    /// Original. Seven times apart is a different link.
    #[test]
    fn a_measurement_a_factor_of_four_away_restarts_the_estimate() {
        let obs = |kbps| CapacityObservation {
            kbps,
            bytes: 999_424,
            active_us: 300_000,
            completed: true,
        };
        let mut estimate = CapacityEstimate::default();
        estimate.update(obs(3_952));
        assert!(obs(28_116).is_regime_change(&estimate), "seven times is not jitter");
        estimate.update(obs(28_116));
        assert_eq!(estimate.slow_kbps, 28_116, "the new regime is the estimate, not a blend of two");
        assert_eq!(estimate.samples, 1, "with one sample's worth of confidence, no more");
        assert!(
            estimate.conservative_kbps() >= source_requirement_kbps(8_000, &AbrPolicy::measured()),
            "which is what makes the second probe decisive, as the device run needed",
        );

        // Symmetric, and ordinary variance is nowhere near it.
        let mut falling = CapacityEstimate::default();
        for _ in 0..4 {
            falling.update(obs(40_000));
        }
        assert!(!obs(30_000).is_regime_change(&falling), "a 25% dip is the link breathing");
        assert!(obs(2_000).is_regime_change(&falling));
        falling.update(obs(2_000));
        assert_eq!(falling.slow_kbps, 2_000);
        // A first sample has no history to be a change FROM.
        assert!(!obs(80_000).is_regime_change(&CapacityEstimate::default()));
    }

    /// The other half of the same device run: an EWMA slope decays toward zero and never arrives,
    /// so a sign test calls a flat reserve "draining" forever — and the upshift gate requires
    /// `!draining`, which is how Auto sat on 10 Mbps with a 25 Mbit/s budget and a full buffer.
    #[test]
    fn a_flat_reserve_is_not_draining() {
        let flat = |slope| BufferEstimate { buffered_ms: 12_000, slope_ms_per_s: slope, ..Default::default() };
        for slope in [0, -4, -16, -49, 100] {
            assert!(!flat(slope).draining(), "slope {slope} is noise around flat");
        }
        for slope in [-51, -400, -2_000] {
            assert!(flat(slope).draining(), "slope {slope} is a reserve actually going away");
        }
        // And the counter behind `starving` follows the same test, so the two cannot disagree.
        let mut buffer = BufferEstimate::default();
        buffer.update(12_000, 2_000);
        for step in 1..=3 {
            buffer.update(12_000 - step * 20, 2_000);
        }
        assert_eq!(buffer.draining_samples, 0, "20ms a segment is not a drain");
    }

    /// A demoted prior keeps its number and loses its confidence — the one operation behind
    /// bootstrap seeding, a path change, and a long pause.
    #[test]
    fn a_demoted_prior_keeps_its_value_and_gives_up_its_confidence() {
        let mut estimate = CapacityEstimate::default();
        for _ in 0..5 {
            estimate.update(CapacityObservation {
                kbps: 40_000,
                bytes: 4_000_000,
                active_us: 800_000,
                completed: true,
            });
        }
        let confident = estimate.conservative_kbps();
        estimate.demote_to_prior();
        assert_eq!(estimate.slow_kbps, 40_000);
        assert_eq!(estimate.samples, 1);
        assert_eq!(estimate.uncertainty_pm, MAX_UNCERTAINTY_PM);
        assert!(estimate.conservative_kbps() < confident);
        assert_eq!(estimate.conservative_kbps(), 20_000, "at most half of an unconfirmed number");
        // An empty estimate has nothing to demote and must not invent a prior.
        let mut empty = CapacityEstimate::default();
        empty.demote_to_prior();
        assert_eq!(empty, CapacityEstimate::default());
    }

    #[test]
    fn staleness_widens_uncertainty_before_it_gives_up_entirely() {
        let policy = AbrPolicy::measured();
        let fresh = || {
            let mut e = CapacityEstimate::default();
            e.update(CapacityObservation {
                kbps: 40_000,
                bytes: 4_000_000,
                active_us: 800_000,
                completed: true,
            });
            e.update(CapacityObservation {
                kbps: 40_000,
                bytes: 4_000_000,
                active_us: 800_000,
                completed: true,
            });
            e
        };
        let mut brief = fresh();
        brief.age_ms(u64::from(policy.stale_half_life_ms) / 2, &policy);
        assert_eq!(brief, fresh(), "a gap shorter than a half-life is not staleness");

        let mut aged = fresh();
        aged.age_ms(u64::from(policy.stale_half_life_ms) * 2, &policy);
        assert!(aged.uncertainty_pm > fresh().uncertainty_pm);
        assert!(aged.samples > 1, "still a history, just a less certain one");

        let mut ancient = fresh();
        ancient.age_ms(u64::from(policy.stale_half_life_ms) * 10, &policy);
        assert_eq!(ancient.samples, 1, "past four half-lives it is a memory, not a measurement");
    }

    /// **The bootstrap table.** One row per link class, plus the three ways a Remote probe can end.
    #[test]
    fn bootstrap_decides_from_the_link_class_and_one_bounded_probe() {
        let policy = AbrPolicy::measured();
        let catalog = hd_catalog();
        let go = |link, feasible, source, probe| {
            bootstrap(link, feasible, source, probe, &catalog, &policy)
        };
        let complete = |kbps| {
            Some(CapacityObservation { kbps, bytes: 2_000_000, active_us: 400_000, completed: true })
        };

        // A verified LAN carrying a playable file needs no measurement to prove it.
        let local = go(LinkKind::Local, true, 28_000, None);
        assert!(local.original && local.reason == BootstrapReason::LocalDirect);
        assert!(local.prior.is_none(), "nothing was measured, so nothing is claimed");

        // Relay is bandwidth-limited by design; measuring it would be theatre.
        assert_eq!(go(LinkKind::Relay, true, 28_000, None).reason, BootstrapReason::RelayLimited);
        assert!(!go(LinkKind::Relay, true, 28_000, None).original);

        // Original impossible for this item (codec, burn-in, no source URL) — link is irrelevant.
        assert_eq!(
            go(LinkKind::Local, false, 28_000, None).reason,
            BootstrapReason::OriginalInfeasible,
        );

        // A fast Remote probe: Original, and the probe survives as an explicitly weak prior.
        let fast = go(LinkKind::Remote, true, 28_000, complete(80_000));
        assert!(fast.original && fast.reason == BootstrapReason::ProbeSustainable);
        let prior = fast.prior.expect("a completed probe is evidence");
        assert_eq!(prior.samples, 1, "weak on purpose: a different request to a different server");
        assert_eq!(prior.uncertainty_pm, MAX_UNCERTAINTY_PM);

        // Borderline: above the average but inside the cold-start margin. HLS, and the measurement
        // still picks the opening rung.
        let borderline = go(LinkKind::Remote, true, 28_000, complete(30_000));
        assert!(!borderline.original);
        assert_eq!(borderline.reason, BootstrapReason::ProbeBelowRequirement);
        assert_eq!(borderline.rung, Rung::P1080High, "24 Mbit/s of budget, spent");

        // A slow Remote opens where the measurement says, NOT at an emergency floor it would then
        // spend a minute climbing out of.
        let slow = go(LinkKind::Remote, true, 60_000, complete(17_000));
        assert!(!slow.original);
        assert_eq!(slow.rung, Rung::P1080M12, "13.6 Mbit/s of budget → the 12 Mbps point");

        // Nothing to reason from: playback still starts, conservatively.
        for inconclusive in [None, Some(CapacityObservation { kbps: 9_000, bytes: 100_000, active_us: 90_000, completed: false })] {
            let decision = go(LinkKind::Remote, true, 60_000, inconclusive);
            assert!(!decision.original);
            assert_eq!(decision.reason, BootstrapReason::ProbeInconclusive);
            assert!(decision.rung.kbps() <= Rung::P1080.kbps(), "conservative, not paralysed");
        }
        // An unknown source bitrate cannot be reasoned about either, and says so.
        assert_eq!(
            go(LinkKind::Remote, true, 0, complete(80_000)).reason,
            BootstrapReason::ProbeInconclusive,
        );
    }

    /// The bootstrap measurement is not thrown away: the live estimator starts from it, so the
    /// first segment refines a number instead of being the only one.
    #[test]
    fn the_bootstrap_prior_seeds_the_steady_state_estimator() {
        let seeded = Controller::starting_at(
            Rung::P480,
            Some(CapacityEstimate::from_prior(40_000)),
            hd_catalog(),
        );
        assert_eq!(seeded.delivery().slow_kbps, 40_000);
        assert_eq!(seeded.delivery().samples, 1);
        let cold = controller_at(Rung::P480);
        assert_eq!(cold.delivery().samples, 0);
        // The seed is weak enough that ONE real segment dominates it rather than being averaged
        // into irrelevance.
        let mut seeded = seeded;
        seeded.observe(sample(4_000, 400, 10_000));
        assert!(seeded.delivery().slow_kbps < 25_000, "{}", seeded.delivery().slow_kbps);
    }

    /// **Transitions are asymmetric and their cost decays.** An HLS rung change is free here (the
    /// viewer never sees it); a mode change is not; and a mode change right after another one is
    /// worse than the first.
    #[test]
    fn transition_cost_is_asymmetric_and_decays_with_time() {
        let policy = AbrPolicy::measured();
        let none = TransitionHistory::default();
        assert_eq!(transition_cost(ModeKind::Hls, ModeKind::Hls, none, &policy), 0);
        let first = transition_cost(ModeKind::Original, ModeKind::Hls, none, &policy);
        assert_eq!(first, policy.visible_switch_cost);

        let just_switched = TransitionHistory { visible_switches: 2, since_last_ms: Some(1_000) };
        let long_ago = TransitionHistory {
            visible_switches: 2,
            since_last_ms: Some(policy.visible_switch_decay_ms * 4),
        };
        let recent = transition_cost(ModeKind::Hls, ModeKind::Original, just_switched, &policy);
        let old = transition_cost(ModeKind::Hls, ModeKind::Original, long_ago, &policy);
        assert!(recent > old && old >= first, "recent={recent} old={old} first={first}");
        assert!(
            transition_cost(
                ModeKind::Hls,
                ModeKind::Original,
                TransitionHistory { visible_switches: 6, since_last_ms: Some(1_000) },
                &policy,
            ) > recent,
            "the sixth switch in two minutes has to buy a lot",
        );
    }

    /// The same evidence that recovers on a first switch stops recovering once this playback has
    /// been flapping — no cooldown counter, just history the utility model can see.
    #[test]
    fn repeated_visible_switches_stop_paying_for_themselves() {
        let calm = OriginalRecovery::new(
            28_000,
            AbrPolicy::measured(),
            false,
            TransitionHistory::default(),
        )
        .unwrap();
        let flapping = OriginalRecovery::new(
            28_000,
            AbrPolicy::measured(),
            false,
            TransitionHistory { visible_switches: 5, since_last_ms: Some(2_000) },
        )
        .unwrap();
        let good = probe(90_000, true);
        let mut calm = calm;
        let mut flapping = flapping;
        assert_eq!(
            calm.observe_probe(good, healthy_buffer(), &healthy_hls(), 600_000),
            RecoveryVerdict::Recover,
        );
        assert_eq!(
            flapping.observe_probe(good, healthy_buffer(), &healthy_hls(), 600_000),
            RecoveryVerdict::NotWorthIt,
            "same link, same film, fifth visible switch",
        );
    }

    /// A whole-file average is a LOWER BOUND on demand. The requirement carries VBR headroom, so a
    /// link that merely matches the average is already at risk before any busy scene arrives.
    #[test]
    fn vbr_headroom_makes_the_whole_file_average_a_lower_bound() {
        let policy = AbrPolicy::measured();
        assert_eq!(source_requirement_kbps(40_000, &policy), 54_000);
        assert_eq!(source_requirement_kbps(0, &policy), 0);
        // 40 Mbit/s average, 41 Mbit/s of measured capacity, and the model still sees a deficit —
        // which is the whole point: the file contains scenes above its own average.
        let mut mode = original(40_000);
        let observation = mode
            .observe(window_bytes(41_000), ORIGINAL_WINDOW_US, Some(30_000), HOUR_MS)
            .unwrap();
        assert!(observation.horizon_secs.is_some(), "a bare average is not headroom");
        assert!(observation.fallback.is_none(), "but 30 s of reserve is not an emergency either");
    }

    /// Utility is not a bitrate comparison: Original wins from BEHIND on wire rate because it has
    /// no generation loss and asks the server for no video encoding at all.
    #[test]
    fn original_beats_the_top_rung_on_utility_at_equal_risk() {
        let policy = AbrPolicy::measured();
        let inputs = ModeInputs {
            current: ModeKind::Original,
            source_kbps: 28_000,
            source_delivery: CapacityEstimate::from_prior(80_000),
            hls_delivery: CapacityEstimate::from_prior(80_000),
            production: ProductionEstimate::default(),
            buffer: BufferEstimate { buffered_ms: 30_000, ..Default::default() },
            remaining_ms: HOUR_MS,
            history: TransitionHistory::default(),
            original_feasible: true,
            original_features: false,
            persistent_deficit_windows: 0,
        };
        let current = hd_catalog().candidate(Rung::P1080High);
        let (mode, reason, chosen, other) =
            choose_mode(&inputs, current, Some(current), &policy);
        assert_eq!((mode, reason), (ModeKind::Original, ModeReason::OriginalWorthIt));
        assert_eq!(chosen.server, 0, "no server video encoding is the term HLS cannot match");
        assert!(chosen.total > other.expect("both were feasible").total);

        // Infeasible is not a low score — it is not a candidate.
        let (mode, reason, _, other) = choose_mode(
            &ModeInputs { original_feasible: false, ..inputs },
            current,
            Some(current),
            &policy,
        );
        assert_eq!((mode, reason), (ModeKind::Hls, ModeReason::OriginalInfeasible));
        assert!(other.is_none());
    }
}
