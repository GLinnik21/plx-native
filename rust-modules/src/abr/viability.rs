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

