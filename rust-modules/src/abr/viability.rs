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

/// The continuous budget the actuator is then chosen FROM — never "one rung up". Two discounts,
/// each with a reason: uncertainty (inside `conservative_kbps`) and a server that is already
/// behind.
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
pub(crate) fn hls_safe_budget(
    capacity: &CapacityEstimate,
    production: &ProductionEstimate,
    buffer: &BufferEstimate,
    policy: &AbrPolicy,
) -> u32 {
    let _ = buffer; // the reserve is condition (2)'s business, not the budget's — see above
    let mut budget = capacity.conservative_kbps();
    if production.ratio_pm > policy.production_safe_pm {
        budget = budget.saturating_mul(policy.production_safe_pm).max(1)
            / production.ratio_pm.max(1);
    }
    budget
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
/// The upshift bound is unchanged in the case that matters: the proposal gate requires three
/// segments of reserve and the two upshift budgets sum to about 2.6, so condition 1 does not bind
/// on a healthy upshift. It binds when the reserve fell between the proposal and the fetch — which
/// is a real transaction, several hundred milliseconds long, on a link that has just deteriorated.
pub(crate) fn candidate_warmup_budget(
    proposal: Proposal,
    media_duration: std::time::Duration,
    reserve: std::time::Duration,
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
        // bound. `MAX` is not a deadline; it is the identity element of the `min` below, written
        // that way so there is exactly one place where a candidate transfer's budget is decided.
        Direction::Down => std::time::Duration::MAX,
    };
    cold_start.min(reserve)
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

/// The playable reserve as a wall-clock budget. A reserve at or below zero is `ZERO`, which makes
/// the deadline "now" — correct, because a transaction starting with no reserve has already
/// stalled and every further millisecond it spends is a millisecond of stall.
pub(crate) fn reserve_as_budget(reserve_ms: i64) -> std::time::Duration {
    std::time::Duration::from_millis(u64::try_from(reserve_ms.max(0)).unwrap_or(0))
}
