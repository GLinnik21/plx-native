use super::*;

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

/// **N5's continuous risk, and the thing it replaced.**
///
/// The network term was a four-step bucket ladder — `1 / 4 / 12 / 40` on the starvation horizon —
/// and a ladder is a set of cliffs: a horizon of 60 s scored 1 and 59 s scored 4, for one second.
/// The previous plan proposed `tau/(T + tau)` instead and it is VOID: it is globally rather than
/// locally sensitive to an unstated `tau`, and its `1/T` tail charges 20-40 points at a 60 s
/// horizon where the ladder charged 1 — contradicting the deficit principle it sits beside.
///
/// ```text
/// r_net(T) = 0                                   T infinite, or T >= starvation_safe_secs
///          = (T_safe - T) / (T_safe - T_fallback)   in between
///          = 1                                   T <= starvation_fallback_secs
/// ```
///
/// Continuous, monotone, bounded, **and it introduces no free parameter**: both endpoints are
/// horizons that already exist and already have product meanings (60 s and 20 s). `r_net = 1`
/// below the fallback horizon is consistent by construction, because that region is an EMERGENCY
/// and is decided by a hard guard rather than by utility.
///
/// The production term is the same shape between the two ratios that already name "comfortably
/// ahead" and "at or slower than real time".
///
/// **The three coefficients are not new either** — 40 / 20 / 30 are the ladder's own worst-case
/// values, so `score_max` stays 90 and every existing ratio to `visible_switch_cost` is unchanged
/// at the endpoints. Two endpoint changes ARE deliberate: a comfortable horizon now scores **0**
/// where the ladder charged 1, and an imminent one still scores 40.
///
/// **Rounding is toward MORE risk**, which is the opposite of every other truncation in this
/// module — those all round toward safety, and here safety is the larger number. `.max(1)` on the
/// production divisor because `AbrPolicy` derives nothing that guarantees the two ratios differ.
///
/// `buffer_risk` stays a labelled boolean hard guard and is deliberately NOT normalised.
pub(crate) fn risk_score(
    horizon_secs: Option<u32>,
    predicted_ratio_pm: Option<u32>,
    buffer_risk: bool,
    policy: &AbrPolicy,
) -> u32 {
    let safe = i64::from(policy.starvation_safe_secs);
    let fallback = i64::from(policy.starvation_fallback_secs);
    // Per mille of the band, so the composition below is one integer multiply and one rounded
    // divide rather than a float. `safe - fallback` is the band width and cannot be zero here
    // without the policy being incoherent, but `.max(1)` costs nothing and the host panics on a
    // division by zero while the television does not.
    let r_net_pm: i64 = match horizon_secs {
        None => 0,
        Some(t) if i64::from(t) >= safe => 0,
        Some(t) if i64::from(t) <= fallback => 1_000,
        Some(t) => (safe - i64::from(t)) * 1_000 / (safe - fallback).max(1),
    };
    let r_prod_pm: i64 = match predicted_ratio_pm {
        None => 0,
        Some(ratio) => {
            let over = i64::from(ratio) - i64::from(policy.production_safe_pm);
            let band = i64::from(policy.production_max_pm)
                .saturating_sub(i64::from(policy.production_safe_pm))
                .max(1);
            (over * 1_000 / band).clamp(0, 1_000)
        }
    };
    // `round_half_up` on a non-negative numerator is `(x + half) / whole`.
    let round_half_up = |weight: i64, pm: i64| ((weight * pm) + 500) / 1_000;
    let mut score = round_half_up(40, r_net_pm) + round_half_up(20, r_prod_pm);
    if buffer_risk {
        score += 30;
    }
    u32::try_from(score).unwrap_or(u32::MAX)
}

/// The largest value [`risk_score`] can return: 40 + 20 + 30. It is the DENOMINATOR the read-out
/// renders with, and it is here rather than as a literal at the render site so the panel cannot
/// go on dividing by 90 after a coefficient moves.
pub(crate) const RISK_SCORE_MAX: u32 = 90;

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
    let score = risk_score(horizon.seconds, predicted, buffer_risk, policy);
    CandidateRisk {
        starvation_seconds: horizon.seconds,
        production_ratio_pm: predicted,
        production_risk,
        buffer_risk,
        score,
    }
}

/// The continuous NETWORK budget the actuator is then chosen FROM — never "one rung up".
/// [`CapacityEstimate::conservative_kbps`] has already applied the estimator's uncertainty;
/// nothing else may discount this rate. Production and reserve are independent feasibility
/// constraints, evaluated in their own units by the candidate filter and the acquisition window.
///
/// # [DELETED] Production pressure was folded into network capacity
///
/// ```text
/// budget = budget * production_safe_pm / production.ratio_pm;
/// ```
///
/// A server taking longer to produce a segment does not reduce the link's bit rate. Production
/// remains an independent feasibility test in both candidate selection and the final upshift
/// guard; representing the same fact here as fewer network kilobits double-counted it and mixed
/// two actuators into a number named for one.
///
/// # [DELETED] The third discount subtracted MILLISECONDS from KILOBITS PER SECOND
///
/// ```text
/// let deficit = policy.minimum_buffer_ms - buffer.buffered_ms;   // milliseconds
/// budget = budget.saturating_sub(deficit);                       // kilobits per second
/// ```
///
/// There is no reading of that under which the units work. It removed up to
/// `minimum_buffer_ms` kbps of budget — thousands of kilobits per second — because the *reserve*
/// was short, and the amount removed had nothing to do with the link. The plan names this deletion
/// (`I4`, "delete the ms-from-kbps branch") and it is one of the stacked margins whose product put
/// Auto at 0.24–0.51 of the measured link.
///
/// **The intent behind it was real and now has a derived home.** "A reserve that needs refilling
/// more than the picture needs bits" is §4's condition (2): `B ≥ Σ(T_i − D)⁺`, evaluated against
/// the reserve in the units of the reserve, on measured acquisitions. Keeping a dimensionally
/// broken copy of the same idea in the budget would be exactly the double-counting the design rule
/// forbids — and the copy fires on the *selection* side, where it is invisible.
pub(crate) fn hls_safe_budget(capacity: &CapacityEstimate) -> u32 {
    capacity.conservative_kbps()
}

/// **The wall-clock a candidate transfer may spend, and it is bounded in EVERY direction.**
///
/// Two independent bounds, minimised — a conjunction of two physical facts, not a margin stacked
/// on a margin. Each answers a different question and each can bind alone:
///
/// 1. **Can the reserve pay for it.** During a candidate fetch the current stream is not being
///    acquired (the transaction runs inline on the demux worker) and the candidate's own output is
///    staged privately until commit, so the playable reserve falls at exactly one millisecond of
///    reserve per millisecond of wall clock. After `reserve` milliseconds it is gone. This is the
///    conservation identity `B_after = B_start - t` evaluated at `B_after = 0`; it carries no
///    coefficient, and it applies to an upshift and a downshift alike because it is a statement
///    about the buffer rather than about the direction.
/// 2. **Would the result be admissible anyway** — [`candidate_prime_budget`] below, upshift only.
///
/// # Why the downshift had NO deadline, and what that measured
///
/// This function opened with `if direction == Down { return None }`, and so did
/// [`candidate_prime_budget`], on the reasoning that a downshift "is the recovery path when the
/// current rung is already unsustainable" — i.e. that refusing it could only make things worse.
/// The premise is right and the conclusion does not follow, because "no acceptance test" was
/// implemented as "no bound of any kind".
///
/// The corpus says what that cost. Across 65 committed `Down`/commit records the decision cost is
/// min 26 ms, median 916, p95 2 198 — **and max 36 164**, a 16x jump from p95 with nothing in
/// between. The record is a 14000 -> 8000 downshift on a link that had fallen to 9 593 kbps: the
/// target was barely under the delivered rate, so the warm-up fetch took 36.2 seconds, against a
/// reserve that cannot have exceeded `B_max(14000)`. That is not a slow transaction. It is a
/// transaction that spent the whole reserve, stalled, and kept going — committing to a target
/// chosen from evidence 36 seconds stale, on a link that was collapsing at the time.
///
/// # Why the bound is the WHOLE reserve and not a fraction of it
///
/// `reserve` is the last point at which this transaction can still be doing the thing it exists
/// to do, so it is an upper bound on any correct deadline. Firing exactly there is the weakest
/// enforceable rule, and that is deliberate. A tighter one needs one of two things that do not
/// exist yet:
///
/// * a projection of the REMAINING transfer from an in-segment rate quantile (the plan's R16),
///   which is real, open, and needs chunk-level instrumentation this transport does not have; or
/// * a bound on the reserve the new rung needs on arrival, `A_j` — which is exactly what the
///   acquisition window would supply, and exactly what it cannot supply here: a delivery collapse
///   resets the window, and a collapse is the event that produced the downshift.
///
/// Absent either, the alternative to the physical bound is a fraction of it — which would be the
/// unexplained multiplier the design rule forbids, doing the work of a model nobody has written.
/// So the deadline is the reserve, the tightening is a named open item, and the effect on
/// `E_tx_down` is that it becomes bounded BY CONSTRUCTION rather than by luck.
///
/// # Why a DOWNSHIFT's deadline is floored at the transfer's own requirement
///
/// The reserve bound above expresses "abandon a transaction that can no longer do what it exists
/// to do", and for an upshift that is exactly right: an upshift buys more quality on a picture
/// that is still playing, so once the reserve is gone the benefit is gone with it. **For a
/// downshift the same sentence is false, and the device measured what that costs.** A downshift's
/// benefit is the picture RESTARTING, which is available precisely when the reserve is exhausted —
/// so abandoning it there means staying on the rung that caused the stall, and the exhausted state
/// becomes ABSORBING.
///
/// Measured 2026-08-28, `pipe_abr_down_outrun` with the abort rule armed: the first downshift
/// spent the whole 6 084 ms reserve on its warm-up and missed the deadline by 31 ms; every one
/// after that was issued with `warmup_dl=168ms` and could not have completed. The controller
/// decided correctly 321 consecutive times — every abort logged `decision=prime_down` — and every
/// transaction died `outcome=warmup_deadline` on a deadline of its own predecessor's making. 74 s
/// of stall, `play=617`, the rung never leaving 18000. `docs/measurements/j3b-downshift-floor.md`
/// has the trace.
///
/// So the deadline is floored at [`predicted_transfer`] — `R_target * D / C`, the time the fetch
/// physically needs at the measured capacity. Refusing a transfer less time than it requires is
/// not bounding it, it is refusing it. Two properties are worth stating because neither is
/// obvious:
///
/// * **It does not loosen the 36-second bound this function was written for.** That record was a
///   14000 -> 8000 downshift on a link measured at 9 593 kbps: `8000 * 2000 / 9593` = **1 667 ms**,
///   which is tighter than the reserve was. The floor binds only where the reserve has collapsed
///   below what any transfer needs, which is the absorbing state and nothing else.
/// * **It cannot run away**, because every term is measured: a link that is genuinely dead
///   drives `capacity_kbps` down, and a capacity of zero yields `ZERO`, restoring the reserve
///   bound exactly.
///
/// **And the floor is the prediction PLUS the estimate's stated error, not the prediction.** The
/// first version granted exactly `R * D / C`, which is a central estimate and is therefore
/// exceeded about half the time — a deadline with no slack is a coin flip, and the device landed
/// it wrong 53 consecutive times on one rung pair: `warmup_dl=1314ms` against `decided=1327ms`,
/// missing by 13 ms, over and over, with the absorbing state back. That was invisible in the first
/// device leg because its targets were far below the current rung (8000, 720, 320 out of 20000),
/// where the prediction is generous by a wide margin; it appears only for a NEAR target, where
/// prediction and reality meet. The widening factor is `CapacityEstimate::uncertainty_pm` — the
/// `unc=` the steady line already publishes — so it is the estimator's own opinion of how wrong it
/// might be rather than a margin chosen here, and it shrinks to nothing as the estimate settles.
///
/// The upshift bound is unchanged in the case that matters: the proposal gate requires three
/// segments of reserve and the two upshift budgets sum to about 2.6, so condition 1 does not bind
/// on a healthy upshift. It binds when the reserve fell between the proposal and the fetch — which
/// is a real transaction, several hundred milliseconds long, on a link that has just deteriorated.
/// **Does a candidate warm-up carry the abort rule?** Down, and not at the ladder floor.
///
/// The two halves are the two arguments that were already made elsewhere, applied here.
///
/// `Direction::Down`, because the abort rule protects the PICTURE and the question is whether the
/// picture is still being fed while the warm-up runs. On an upshift it is: the current rung is
/// affordable by construction — that is why a dearer one was proposed — so a warm-up that
/// overruns costs a probe and nothing else. On a downshift it is not: the current rung being
/// unaffordable IS the trigger, so the reserve drains for the whole warm-up and the budget the
/// deadline is built from is the reserve itself. `candidate_warmup_budget` turns on the same
/// asymmetry for the same reason.
///
/// **Not at the floor**, which is R12 exactly as `hls_read_loop` applies it to the active cursor:
/// aborting buys an escape to a cheaper rung, and where there is no cheaper rung it re-fetches
/// the same bytes and buys a loop instead of a picture.
pub(crate) fn candidate_warmup_is_guarded(proposal: Proposal) -> bool {
    proposal.direction == Direction::Down && proposal.rung.below() != proposal.rung
}

pub(crate) fn candidate_warmup_budget(
    proposal: Proposal,
    media_duration: std::time::Duration,
    reserve: std::time::Duration,
    predicted_transfer: std::time::Duration,
) -> std::time::Duration {
    // A NEW PMS encoder's first segment carries decoder and encoder cold start and is not the
    // cadence the replacement will sustain, so it is not held to the acceptance threshold; it gets
    // a bounded content-duration window instead. `3/2` is the one number here that is a stated
    // product choice rather than a measurement, and it is confined to the UPSHIFT, where the
    // consequence of being wrong is a refused climb.
    let cold_start = match proposal.direction {
        Direction::Up => std::time::Duration::from_micros(
            (media_duration.as_micros().saturating_mul(3) / 2).min(u128::from(u64::MAX)) as u64,
        ),
        // A downshift has no acceptance test at all — see above — so the reserve is its only
        // acceptance bound. `MAX` is not a deadline; it is the identity element of the `min`
        // below, written that way so there is exactly one place where a candidate transfer's
        // budget is decided.
        Direction::Down => std::time::Duration::MAX,
    };
    // **The floor, and it applies to a DOWNSHIFT only.** See the section above it.
    match proposal.direction {
        Direction::Up => cold_start.min(reserve),
        Direction::Down => cold_start.min(reserve).max(predicted_transfer),
    }
}

/// **How long one segment at `target_wire_kbps` physically needs on a link measured at
/// `capacity_kbps`** — `bits / rate`, and nothing else.
///
/// ```text
/// bits for one segment at rung R over D ms of media   =  R * D      (kbps * ms = bits)
/// time to move them over a link of capacity C         =  R * D / C  (bits / kbps = ms)
/// ```
///
/// Both inputs are measurements: `target_wire_kbps` is the catalog's *observed* output for that
/// rung (not its request ceiling) and `capacity_kbps` is the delivery estimate's conservative
/// reading from completed segments. There is no margin and no multiplier, because this is not a
/// budget — it is the transfer's own requirement, and it exists so that a *deadline* can be
/// stopped from falling below it.
///
/// **The conservative capacity is the right one, and the direction matters.** A lower `C` yields a
/// LARGER requirement, hence a larger floor, hence more time granted — so an under-confident
/// estimate errs toward letting the downshift complete, which is the whole point of the floor. The
/// optimistic reading would shrink the floor exactly when the estimate is least trustworthy.
///
/// **Zero capacity means no prediction**, which is `ZERO` rather than a fallback: as the identity
/// element of the `max` in [`candidate_warmup_budget`] it leaves the reserve bound exactly as it
/// was, so an unmeasured link changes no behaviour rather than inheriting an invented one.
pub(crate) fn predicted_transfer(
    target_wire_kbps: u32,
    media_duration: std::time::Duration,
    capacity_kbps: u32,
    uncertainty_pm: u32,
) -> std::time::Duration {
    if capacity_kbps == 0 {
        return std::time::Duration::ZERO;
    }
    let bits = u128::from(target_wire_kbps).saturating_mul(media_duration.as_millis());
    let ms = bits / u128::from(capacity_kbps);
    // **Plus the estimate's own stated error, because a CENTRAL estimate is exceeded half the
    // time.** See the section above: a deadline set to the prediction exactly is a coin flip, and
    // the device measured that coin landing wrong — `warmup_dl=1314ms` against `decided=1327ms`,
    // 53 times in a row on one rung pair. The multiplier is not chosen: it is
    // `CapacityEstimate::uncertainty_pm`, the same number the `abr: steady` line publishes as
    // `unc=`, so a link the estimator is unsure about buys proportionally more time and a
    // well-measured one buys almost none.
    let widened = ms.saturating_mul(u128::from(1_000 + uncertainty_pm)) / 1_000;
    std::time::Duration::from_millis(u64::try_from(widened).unwrap_or(u64::MAX))
}

/// The GRADED segment's budget, upshift only — the segment that decides whether the candidate is
/// admitted. Bounded by the same reserve as the warm-up, and additionally by the ACCEPTANCE
/// threshold: a candidate that cannot deliver one complete segment inside the production headroom
/// [`Controller::candidate_ready`] requires can never be committed, so returning to the active
/// encoder early costs nothing and saves the reserve.
///
/// **The threshold is READ from the policy, and it used to be a literal `4/5` that silently
/// stopped matching.** The doc here has always claimed parity with `candidate_ready`, and it was
/// true while that test was a bare `production_ratio_pm <= 800`. When the §4 admission work
/// replaced the bare 800 with `production_max_pm` (1100 — "this JIT encoder cannot keep up"), this
/// literal was left behind, and the two stopped meaning the same thing: the transport aborted a
/// candidate at 0.8·D that the acceptance test would have admitted up to 1.1·D. Candidates in that
/// band died at the deadline and never reached the rule at all.
///
/// That is a stacked margin of exactly the kind the design rule forbids — one threshold enforced
/// twice at two different values, the stricter one invisible because it fires in the transport. So
/// there is one number now and both sites read it.
pub(crate) fn candidate_prime_budget(
    media_duration: std::time::Duration,
    policy: &AbrPolicy,
    reserve: std::time::Duration,
) -> std::time::Duration {
    let micros = media_duration
        .as_micros()
        .saturating_mul(u128::from(policy.production_max_pm))
        / 1_000;
    std::time::Duration::from_micros(micros.min(u128::from(u64::MAX)) as u64).min(reserve)
}

/// **`E_tx`: what one upshift transaction costs in unrefilled playback**, and the derivation the
/// ledger left as "TBD from `E_tx`" for [`AbrPolicy`]'s two operational guards (N10, N11).
///
/// It is **the sum of the two enforced deadlines** and nothing else — R19's own form, which is the
/// only bound on this transaction that is a fact rather than an estimate. The warm-up fetch may
/// run to [`candidate_warmup_budget`] and the graded fetch to [`candidate_prime_budget`]; the
/// reserve does not refill during either, because the transaction runs inline on the demux worker
/// and the candidate's output is staged privately until commit. So the cost is bounded by their
/// sum, by construction, whatever the link does.
///
/// **No new number enters.** `3/2` and `production_max_pm` already exist, already have written
/// derivations at their own sites, and are already enforced. At the 2 s segment this pipeline
/// requests that is `3 s + 2.2 s = 5.2 s`. (`docs/adaptive-playback-plan.md` §6.2 records `E_tx`
/// as "~4 600 (2.3·d)", which was written while `candidate_prime_budget` was a literal `4/5·d`;
/// the ledger row is stale, not this function — see that function's own account of the `4/5`.)
///
/// The `reserve` argument the two budgets take is deliberately absent: this is the cost of a
/// transaction that runs to its deadlines, and clamping it to the reserve of the moment would make
/// a guard derived from it *shorter* exactly when the reserve is thin, which is backwards.
pub(crate) fn upshift_transaction_cost(
    media_duration: std::time::Duration,
    policy: &AbrPolicy,
) -> std::time::Duration {
    let unbounded = std::time::Duration::MAX;
    let warmup = candidate_warmup_budget(
        Proposal { rung: Rung::P240, direction: Direction::Up },
        media_duration,
        unbounded,
        // The downshift floor by construction cannot reach an `Up` proposal, and this ledger
        // prices the UP path alone. `ZERO` is the identity element, so it is stated rather than
        // fabricating a capacity this function has no access to.
        std::time::Duration::ZERO,
    );
    let prime = candidate_prime_budget(media_duration, policy, unbounded);
    warmup.saturating_add(prime)
}

/// The playable reserve as a wall-clock budget. A reserve at or below zero is `ZERO`, which makes
/// the deadline "now" — correct, because a transaction starting with no reserve has already
/// stalled and every further millisecond it spends is a millisecond of stall.
pub(crate) fn reserve_as_budget(reserve_ms: i64) -> std::time::Duration {
    std::time::Duration::from_millis(u64::try_from(reserve_ms.max(0)).unwrap_or(0))
}
