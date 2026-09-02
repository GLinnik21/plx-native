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
        let Some(since) = self.since_last_ms else {
            return base;
        };
        let halvings = since / policy.visible_switch_decay_ms.max(1);
        base >> halvings.min(16)
    }
}

/// **Asymmetric between an HLS rung change and a mode change.** A rung change is a background
/// prime and reaches this function as `Hls -> Hls`, so the viewer pays zero mode cost. Either
/// Original/HLS direction tears down one stream and re-Loads the pipeline, so both currently pay
/// the same visible base cost plus history. Returning to Original additionally bets on the source
/// path, but that uncertainty is carried by the mandatory completed source probe rather than by an
/// invented directional multiplier here.
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
    /// The source carries Dolby Vision — a visible panel-mode change a transcode destroys.
    /// **Split from one `original_features` boolean** (N16), which was
    /// `dovi.profile > 0 || immersive` and priced an Atmos-only film identically to a DV one.
    pub(crate) source_dv: bool,
    /// The source carries Atmos or lossless audio. See [`Self::source_dv`].
    pub(crate) source_atmos: bool,
    /// **How long the deficit has persisted, in measurement windows.** A dip is noise and a regime
    /// change is not, and only elapsed time tells them apart: without this term the utility
    /// comparison sees a 40-second starvation horizon identically whether the link wobbled once or
    /// has been short for ten seconds straight. It raises Original's risk, so a deficit that will
    /// not go away eventually loses the argument on its own — before starvation is imminent, and
    /// without a hard counter deciding anything.
    pub(crate) unsafe_deficit_ms: i64,
}

/// **What this source carries that a transcode would destroy** — one value where there was one
/// boolean (N16).
///
/// `route::auto_original_features` returned `dovi.profile > 0 || immersive`, so the two collapsed
/// into a single flat bonus and an Atmos-only film bought two visible reloads for a benefit
/// inaudible on television speakers, priced identically to a Dolby Vision panel-mode change. They
/// are ORDERED, not equal; see `AbrPolicy::dv_bonus`.
///
/// Both are recorded off the Original CANDIDATE rather than inferred from the stream now playing,
/// which mid-HLS is a re-encode of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SourceFeatures {
    pub(crate) dv: bool,
    pub(crate) atmos: bool,
}

/// **A term paid for every remaining segment is scaled; a term paid once is not.** Below the
/// policy horizon the recurring terms are scaled linearly, which is the whole of "do not reload
/// with twenty seconds left" — no threshold, no special case, and it degrades smoothly rather than
/// at a cliff. Quality, features, risk and the server's production load all accrue over what is
/// left of the film; only `transition` is a reload and sits outside this. The benefit-versus-cost
/// reading this doc used to give is the one `hls_utility` records as a defect: it kept `risk` and
/// `server` at full weight on one side of the argmax only.
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

/// Quality score of a picture delivered at `wire_kbps`, in the same units as
/// [`AbrPolicy::original_quality_bonus`]. Concave on purpose: 2 to 4 Mbit/s is a transformation of
/// the picture, 18 to 20 is not, and a linear score would happily pay a visible reload for the
/// second one.
///
/// **It takes a RATE, not a candidate, because a rate is all it ever read.** The candidate form it
/// replaced forced both sides of `choose_mode`'s argmax to present one: `hls_utility` cloned a
/// candidate solely to change that field, and `source_quality_score` — which has no candidate at
/// all — invented `rung: Rung::P240` and `production_load_pm: 0` to call it. Those two were lies a
/// later reader could believe, and the second is the shape N14 site 1 was already caught
/// fabricating. Scoring a source is not scoring a rung, and the signature now says so.
pub(crate) fn quality_score_at_kbps(wire_kbps: u32) -> i64 {
    match wire_kbps {
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

/// **The rate an HLS candidate may be SCORED at: `transcode <= source`** (plan R5, §7.B).
///
/// [`quality_score_at_kbps`] reads a rung's nominal wire rate, and until 2026-08-28 that was the
/// whole input on this side of the argmax — while [`source_quality_score`] on the OTHER side has
/// always capped the source by its own rate and raster. The comparison was therefore asymmetric in
/// the one direction that matters: a rendition may be requested at any rung the ladder offers, so
/// a 20 Mbit/s transcode of an 8 Mbit/s master scored the ladder's top band while the master
/// itself scored the band its real rate falls in.
///
/// R5 stated the size of that ("an 8.5 Mbit/s 1080p source scores three steps below a 20 Mbit/s
/// transcode of itself") and this reproduces it exactly: 8000 lands in `7001..=9000` for 58, 18000
/// lands in the open band for 76, and 58 -> 66 -> 72 -> 76 is three steps.
///
/// Measured consequence, host 2026-08-28 (`pipe_auto_original_slow_recover`): with HLS settled at
/// 18000 against an 8000 kbps source, `OriginalRecovery::probe_due` refuses with
/// `reason=not_worth_it` and Original is never recovered — the controller declining to return to
/// the master because it scores a re-encode of that master above it.
///
/// The cap is STRUCTURAL, not a tuning weight: it is `transcode <= source` in R5's own words, the
/// same bound `source_quality_score` already applies, and it introduces no constant.
///
/// **It is a named function rather than a `min` inside `hls_utility` because the invariant is a
/// relation between two things and had no home.** As a local it was unreachable from the test that
/// claimed to guard it — `a_rendition_cannot_score_above_the_master_it_encodes` re-implemented the
/// clamp in its own body and stayed green with the production line deleted.
///
/// The two sides cap differently and that asymmetry is deliberate, so it is written down here
/// rather than left to be inferred: this caps by RATE alone, because an HLS candidate's raster is
/// already bounded by `route::auto_catalog`'s `limited_to`, while a source arrives with a raster
/// nobody has bounded and `source_quality_score` must cap by rate AND raster.
pub(crate) fn hls_scoring_kbps(candidate: HlsCandidate, source_kbps: u32) -> u32 {
    if source_kbps == 0 {
        // Zero means "nobody said what the source is" (`ModeInputs::source_kbps`) and must not
        // clamp to nothing, which would score every rung 0 and refuse every upshift.
        candidate.expected_wire_kbps
    } else {
        candidate.expected_wire_kbps.min(source_kbps)
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
    // R5's `transcode <= source` lives in `hls_scoring_kbps`, which is where the invariant is
    // stated, tested and reachable from — not inline here, where it was a local nothing could name.
    let quality = scaled(
        quality_score_at_kbps(hls_scoring_kbps(candidate, inputs.source_kbps)),
        scale,
    );
    // **N18 applies to BOTH sides of the argmax, and it was applied to one.** The rule it states
    // is a partition, not a preference: a term paid once is outside the scale and a term paid for
    // every remaining segment is inside it. `original_utility` scales quality, features and risk
    // and leaves `transition` out; here only quality was scaled, so `risk` and `server` — both
    // recurring, both charged per segment for the rest of the film — kept full weight while
    // Original's shrank with the horizon.
    //
    // That is not symmetric bookkeeping, it decides reloads. In `OriginalRecovery` the Original
    // side's risk score is identically zero (both paths reach `original_utility` only past a
    // capacity test that empties the starvation band, and `inputs` hardcodes `unsafe_deficit_ms:
    // 0`), so near the end of a film the comparison reduces to `-transition` against
    // `-(risk + server)` with one side scaled to almost nothing and the other not. Worked at 8 s
    // remaining (`scale` = 66 pm) with a loaded PMS holding the best rung at P480: Original
    // totalled -9 and HLS -60, i.e. tear the encoder down and reload the pipeline with eight
    // seconds of film left. Scaled consistently the same state gives HLS -3 and the reload does
    // not happen. `benefit_scale_pm` exists to make exactly that decision.
    let server = scaled(
        policy.server_cost_weight * i64::from(candidate.production_load_pm) / 1_000,
        scale,
    );
    let transition = transition_cost(inputs.current, ModeKind::Hls, inputs.history, policy);
    let risk_cost = scaled(policy.risk_weight * i64::from(risk.score), scale);
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
    // Original's average requirement is the source's own measured average, and its delivery
    // evidence is the source probe's — never the HLS estimate, which measured a different finite
    // request. Short-term VBR is observed through the reserve derivative, not guessed by a fixed
    // multiplier here.
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
    // **Persistence, priced on the wall clock the policy is now stated in** (N13). It was
    // `min(windows, 15) * 4`, and a "window" was 750 ms of ACTIVE BODY-READ time — a clock that
    // stops under backpressure, i.e. exactly when the buffer is healthy.
    //
    // **Both endpoints are the old rule's own values**, the same technique N5 used for the network
    // term: at the threshold that ends the deficit it charges 24, which is what six windows charged
    // at the six-window threshold, and it saturates at 60, which is what the old `.min(15)` cap
    // charged — 15/6 of the threshold, so the cap is 2.5x expressed as a multiple rather than as a
    // second count. No number is introduced; what changes is that it is continuous in elapsed time
    // instead of stepped in windows, so a deficit lasting 1.5 thresholds is priced between them
    // rather than at whichever step it lands on.
    let threshold = policy.sustained_unsafe_deficit_ms.max(1);
    let held_pm = (inputs.unsafe_deficit_ms.max(0).saturating_mul(1_000) / threshold).min(2_500);
    score += u32::try_from(60 * held_pm / 2_500).unwrap_or(60);
    // **Original's quality is scored from the SOURCE, not from a synthetic HLS reference** (N14
    // site 3). It was `original_quality_bonus + quality_score_at_kbps(candidate(P1080High).expected_wire_kbps)` — a
    // constant 116 whatever it was being compared against, so the structural advantage the policy
    // comment reasons about as "40" was +40 against P1080High, +76 against P720 and +116 against
    // P240. A bonus that grows as the alternative gets worse is not a bonus, it is a thumb.
    let quality = scaled(
        policy.original_quality_bonus + i64::from(source_quality_score(inputs)),
        scale,
    );
    // **Three terms, ordered, where there was one flat bonus behind one boolean** (N16).
    // `generation_loss_bonus` is unconditional: no re-encode at all is true of every Original, and
    // pricing it at zero for a plain file while pricing DV and Atmos together at 25 is exactly the
    // conflation. Only the ORDER of the three is a claim; see `AbrPolicy::dv_bonus`.
    let features = scaled(
        policy.generation_loss_bonus
            + if inputs.source_dv { policy.dv_bonus } else { 0 }
            + if inputs.source_atmos {
                policy.atmos_bonus
            } else {
                0
            },
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
///   quality curve has saturated anyway (`quality_score_at_kbps`'s last band is open-ended), so
///   clamping costs nothing real and stops an enormous remux rate reading as an enormous score.
/// * the raster is a CAP, not a bonus. A source smaller than 1080p cannot be worth more than the
///   rungs that reproduce it exactly, so it scores at the best rung the LADDER ITSELF admits for
///   that source — `HlsActuatorCatalog::limited_to(unbounded device, source).feasible()`, i.e.
///   exactly `admits`. An UNSTATED raster `(0, 0)` applies no cap, because `covers_source` reads a
///   zero as "nobody said" and every rung is then admitted; the conservative direction here, since
///   refusing to credit a source nobody measured would silently prefer transcoding.
///
///   **Asking the catalog is not tidiness — restating the rule got it backwards, in the exact
///   shape `admits` already records as device-measured.** This function first wrote the cap as its
///   own per-axis filter, `rung_w <= source_w && rung_h <= source_h`. That is the inverted
///   containment test: a rung's raster is a BOUNDING BOX that PMS fits the source inside, so the
///   question is whether the box COVERS the source, not whether it fits within it. Under the
///   inverted form a 1920x800 scope master — which is to say most films — admitted no 1080p rung
///   at all and capped at 4000 kbps, scoring 40 where a 16:9 master of the same bitrate scored 76.
///   Since that score is one side of `choose_mode`'s argmax, Auto would have refused to recover
///   Original on a scope film while recovering it on a 16:9 one over the same link. `admits`'s own
///   doc describes the same defect from the ladder's side, measured on the television.
///
/// It is deliberately expressed in `quality_score_at_kbps`'s own units by evaluating that function, so
/// the two sides of the comparison cannot drift apart when the curve is re-shaped.
fn source_quality_score(inputs: &ModeInputs) -> i64 {
    let top = HlsActuatorCatalog::measured().candidate(Rung::P1080High);
    // **The raster caps the RATE**, and then the curve is evaluated once. Expressing it as a rate
    // cap rather than as a filter over rungs is not a simplification for its own sake: the first
    // version filtered the ladder by raster and then took `max` with the source's own rate as a
    // floor, and the floor silently defeated the cap — a 28 Mbps 720p master scored the same as a
    // 28 Mbps 1080p one, which is the exact defect this function exists to remove.
    //
    // The DEVICE bound is deliberately unbounded here: this asks what the source is worth, which
    // is a property of the picture that exists, not of the SoC that would decode a transcode of
    // it. `choose_mode`'s HLS side is already scored on a catalog the device bounded.
    let raster_cap = HlsActuatorCatalog::measured()
        .limited_to((0, 0), inputs.source_raster)
        .feasible()
        .map(|candidate| candidate.expected_wire_kbps)
        .max()
        .unwrap_or(0);
    let rate = inputs
        .source_kbps
        .min(top.expected_wire_kbps)
        .min(raster_cap);
    quality_score_at_kbps(rate)
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
        Some(orig) if orig.total > hls.total => (
            ModeKind::Original,
            ModeReason::OriginalWorthIt,
            orig,
            Some(hls),
        ),
        Some(orig) if orig.total == hls.total && inputs.current == ModeKind::Original => {
            // A tie contains no benefit with which to pay for a visible reload.
            (
                ModeKind::Original,
                ModeReason::OriginalWorthIt,
                orig,
                Some(hls),
            )
        }
        Some(orig) => (
            ModeKind::Hls,
            ModeReason::OriginalNotWorthIt,
            hls,
            Some(orig),
        ),
        None => (ModeKind::Hls, ModeReason::OriginalInfeasible, hls, None),
    }
}
