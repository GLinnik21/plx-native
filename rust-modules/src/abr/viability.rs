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

/// An upshift candidate that cannot deliver one complete segment inside the same production
/// headroom threshold used by [`Controller::candidate_ready`] can never be committed. Give the
/// transport that exact budget so it returns to the active encoder before the playback reserve
/// drains. Downshifts have no such deadline: they are the recovery path when the current rung is
/// already unsustainable.
///
/// **The threshold is READ from the policy, and it used to be a literal `4/5` that silently
/// stopped matching.** The doc above has always claimed parity with `candidate_ready`, and it was
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
    proposal: Proposal,
    media_duration: std::time::Duration,
    policy: &AbrPolicy,
) -> Option<std::time::Duration> {
    if proposal.direction == Direction::Down {
        return None;
    }
    let micros = media_duration
        .as_micros()
        .saturating_mul(u128::from(policy.production_max_pm))
        / 1_000;
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

