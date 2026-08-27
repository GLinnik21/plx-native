use super::*;

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
    /// **The SOURCE's frame, `(0, 0)` for "nobody said"** — the second of the two inputs Original's
    /// quality has to be scored from (N14 site 3). Read off the playback's own catalog, which
    /// `route::auto_catalog` already bounded by it; see `HlsActuatorCatalog::source_raster`.
    pub(crate) source_raster: (u16, u16),
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
pub(crate) fn benefit_scale_pm(remaining_ms: i64, policy: &AbrPolicy) -> i64 {
    if remaining_ms <= 0 {
        return 0;
    }
    let horizon = policy.benefit_horizon_ms.max(1);
    (remaining_ms.min(horizon) * 1_000) / horizon
}

pub(crate) fn scaled(value: i64, scale_pm: i64) -> i64 {
    value * scale_pm / 1_000
}

/// Quality score of an HLS operating point, in the same units as
/// [`AbrPolicy::original_quality_bonus`]. Concave on purpose: 2 to 4 Mbit/s is a transformation of
/// the picture, 18 to 20 is not, and a linear score would happily pay a visible reload for the
/// second one.
pub(crate) fn hls_quality_score(candidate: HlsCandidate) -> i64 {
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
    // **Original's quality is scored from the SOURCE, not from a synthetic HLS reference** (N14
    // site 3). It was `original_quality_bonus + hls_quality_score(candidate(P1080High))` — a
    // constant 116 whatever it was being compared against, so the structural advantage the policy
    // comment reasons about as "40" was +40 against P1080High, +76 against P720 and +116 against
    // P240. A bonus that grows as the alternative gets worse is not a bonus, it is a thumb.
    let quality = scaled(
        policy.original_quality_bonus + i64::from(source_quality_score(inputs, policy)),
        scale,
    );
    let features = scaled(
        if inputs.original_features { policy.original_feature_bonus } else { 0 },
        scale,
    );
    let transition = transition_cost(inputs.current, ModeKind::Original, inputs.history, policy);
    // **Scaled with the other recurring terms** (N18). Risk is a property of the playback that
    // REMAINS, exactly as quality and features are; leaving it outside the scale made effective
    // risk aversion inversely proportional to remaining playback, which is the same defect §7.C
    // rejects for rung selection — at 6 s remaining the argmax was P720 and at 2 s a tie between
    // P240 and P480. `transition` stays outside it: a reload is paid once, now.
    let risk_cost = scaled(policy.risk_weight * i64::from(score), scale);
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

/// **What the SOURCE is worth on the same scale the rungs are scored on** — the honest baseline
/// `original_utility` needed and did not have.
///
/// Two inputs and both are conservative on purpose:
///
/// * the rate is `min(source_kbps, top rung's planning rate)`. Above the ladder's ceiling the
///   quality curve has saturated anyway (`hls_quality_score`'s last band is open-ended), so
///   clamping costs nothing real and stops an enormous remux rate reading as an enormous score.
/// * the raster is a CAP, not a bonus. A source smaller than 1080p cannot be worth more than the
///   rungs that reproduce it exactly, so it scores at the best rung whose frame it fills. An
///   UNSTATED raster `(0, 0)` applies no cap — the same "nobody said is not a forbidden zero"
///   reading `HlsActuatorCatalog::limited_to` gives it, and the conservative direction here,
///   because refusing to credit a source nobody measured would silently prefer transcoding.
///
/// It is deliberately expressed in `hls_quality_score`'s own units by evaluating that function, so
/// the two sides of the comparison cannot drift apart when the curve is re-shaped.
fn source_quality_score(inputs: &ModeInputs, _policy: &AbrPolicy) -> i64 {
    let top = HlsActuatorCatalog::measured().candidate(Rung::P1080High);
    let (sw, sh) = inputs.source_raster;
    // **The raster caps the RATE**, and then the curve is evaluated once. Expressing it as a rate
    // cap rather than as a filter over rungs is not a simplification for its own sake: the first
    // version filtered the ladder by raster and then took `max` with the source's own rate as a
    // floor, and the floor silently defeated the cap — a 28 Mbps 720p master scored the same as a
    // 28 Mbps 1080p one, which is the exact defect this function exists to remove.
    let raster_cap = if sw > 0 && sh > 0 {
        LADDER
            .iter()
            .filter(|rung| {
                let (rw, rh) = rung.raster();
                rw <= sw && rh <= sh
            })
            .map(|rung| HlsActuatorCatalog::measured().candidate(*rung).expected_wire_kbps)
            .max()
            .unwrap_or(0)
    } else {
        // `(0, 0)` is "nobody said", not a forbidden zero-pixel picture — the same reading
        // `HlsActuatorCatalog::limited_to` gives it, and the conservative direction here, because
        // refusing to credit a source nobody measured would silently prefer transcoding.
        u32::MAX
    };
    let rate = inputs
        .source_kbps
        .min(top.expected_wire_kbps)
        .min(raster_cap);
    hls_quality_score(HlsCandidate {
        rung: Rung::P240,
        request_kbps: rate,
        expected_wire_kbps: rate,
        production_load_pm: 0,
    })
}

/// **One whole mode comparison, for one event-log line** (§7.H).
///
/// `ModeUtility`'s own doc has always said the terms are kept apart "because the event log prints
/// them" — *"Original lost is not a diagnosis; Original lost 40 of quality to 60 of transition cost
/// with 90 s left is"*. That was aspirational: all three `choose_mode` call sites discarded the
/// reason and both utilities, so the one question an operator asks after a visible switch was
/// unanswerable from a log. This is the carrier that makes the sentence true.
///
/// Assembled in `abr/`, which never logs — the same division `ControllerTelemetry` keeps, and for
/// the same reason: the numbers printed are then provably the numbers used, rather than a
/// re-derivation at the call site against inputs that have since moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModeComparison {
    pub(crate) chosen: ModeKind,
    pub(crate) reason: ModeReason,
    /// The mode that WON, decomposed.
    pub(crate) winner: ModeUtility,
    /// The mode that lost, when there was one. `None` only for `OriginalInfeasible`, where there
    /// was no second candidate to score rather than a second candidate that scored badly.
    pub(crate) loser: Option<ModeUtility>,
    /// The HLS operating point the comparison was actually against — the thing N14 site 1 was
    /// fabricating. Logged because "Original lost to HLS" means nothing without it.
    pub(crate) hls_rung: Rung,
    /// `benefit_scale_pm` at the moment of the decision, so a comparison taken near the end of a
    /// film is readable as one.
    pub(crate) scale_pm: i64,
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
}

/// **The selection step: argmax over the feasible states.** Deliberately only two contenders —
/// Original, and the single best HLS candidate the budget and the server allow — because the rung
/// question was already answered upstream by the safe budget, and re-litigating it here as a
/// thirteen-way utility comparison would let a quality curve override a measured capacity bound.
/// **`best_hls` is not an `Option`, and making it one cost two unreachable arms and a variant.**
/// All five call sites pass a candidate (`original.rs:141`, `:233`, `:478`, and two in `tests.rs`);
/// `HlsActuatorCatalog` always has a floor rung, so "no HLS candidate at all" is not a state this
/// controller can be in. The `(_, None)` arms could never run and `ModeReason::NoHlsCandidate`
/// could never be produced — a read-out code for a situation that does not exist, and one of the
/// two J1 findings a television could not have caught at all.
pub(crate) fn choose_mode(
    inputs: &ModeInputs,
    current_hls: HlsCandidate,
    best_hls: HlsCandidate,
    policy: &AbrPolicy,
) -> (ModeKind, ModeReason, ModeUtility, Option<ModeUtility>) {
    let original = original_utility(inputs, policy);
    let hls = hls_utility(best_hls, current_hls, inputs, policy);
    match original {
        Some(orig) if orig.total > hls.total => {
            (ModeKind::Original, ModeReason::OriginalWorthIt, orig, Some(hls))
        }
        Some(orig) => (ModeKind::Hls, ModeReason::OriginalNotWorthIt, hls, Some(orig)),
        None => (ModeKind::Hls, ModeReason::OriginalInfeasible, hls, None),
    }
}

