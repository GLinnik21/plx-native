use super::*;

/// What one second of this source consumes on average.  PMS does not expose a peak envelope, so
/// inflating the average by a fixed multiplier would be an unmeasured heuristic, not a bound.
/// Short-term VBR demand is instead visible where it matters: in the playable-buffer derivative.
pub(crate) fn source_requirement_kbps(source_kbps: u32, _policy: &AbrPolicy) -> u32 {
    source_kbps
}

/// **The single number every measurement reaches the decision through.** Delivery variance, VBR
/// headroom and buffer level/slope end here, per candidate, so the utility comparison below has one
/// risk term instead of one term per telemetry field. PMS cadence is published separately because
/// the available observation is total acquisition, not an independent encoder clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CandidateRisk {
    pub(crate) starvation_seconds: Option<u32>,
    pub(crate) production_ratio_pm: Option<u32>,
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
/// The two coefficients are not new either — 40 / 30 are the network and reserve ladder's own
/// worst-case values. Two endpoint changes ARE deliberate: a comfortable horizon now scores **0**
/// where the ladder charged 1, and an imminent one still scores 40.
///
/// **Rounding is toward MORE risk**, which is the opposite of every other truncation in this
/// module — those all round toward safety, and here safety is the larger number.
///
/// `buffer_risk` stays a labelled boolean hard guard and is deliberately NOT normalised.
pub(crate) fn risk_score(horizon_secs: Option<u32>, buffer_risk: bool, policy: &AbrPolicy) -> u32 {
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
    // `round_half_up` on a non-negative numerator is `(x + half) / whole`.
    let round_half_up = |weight: i64, pm: i64| ((weight * pm) + 500) / 1_000;
    let mut score = round_half_up(RISK_NET, r_net_pm);
    if buffer_risk {
        score += RISK_BUFFER;
    }
    u32::try_from(score).unwrap_or(u32::MAX)
}

/// The two weights [`risk_score`] sums. Named so that [`RISK_SCORE_MAX`] can be their sum rather
/// than a fourth number that has to be edited alongside them: the const's whole purpose is that the
/// panel cannot go on dividing by a stale denominator after a coefficient moves, and while the
/// coefficients were literals it did not achieve that — moving one still meant editing the 90 by
/// hand, which is the edit it exists to make unnecessary.
const RISK_NET: i64 = 40;
const RISK_BUFFER: i64 = 30;

/// The largest value [`risk_score`] can return. It is the DENOMINATOR the read-out renders with,
/// and it is here rather than as a literal at the render site.
pub(crate) const RISK_SCORE_MAX: u32 = (RISK_NET + RISK_BUFFER) as u32;

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
    // Project the active end-to-end acquisition cadence onto the candidate's calibrated work
    // class. This remains telemetry: the observation spans server wait, pacing and path transfer,
    // so it cannot identify an independent server constraint.
    let predicted = production.predicted_ratio_pm(candidate, current, policy);
    // This estimate is total end-to-end acquisition, not an independently identified encoder
    // clock. Keep the projection in telemetry, but do not charge it a second time after network and
    // exact candidate acquisition already charged the same service episode.
    let buffer_risk =
        buffer.buffered_ms < policy.emergency_buffer_ms || (buffer.starving() && buffer.draining());
    let score = risk_score(horizon.seconds, buffer_risk, policy);
    CandidateRisk {
        starvation_seconds: horizon.seconds,
        production_ratio_pm: predicted,
        buffer_risk,
        score,
    }
}

/// The continuous NETWORK budget the actuator is then chosen FROM — never "one rung up".
/// [`CapacityEstimate::conservative_kbps`] has already applied the estimator's uncertainty;
/// nothing else may discount this rate. Reserve remains an independent feasibility constraint,
/// evaluated in its own units by the candidate filter and acquisition window. The available
/// "production" observation is total acquisition and therefore is not independent of this budget.
///
/// # [DELETED] Production pressure was folded into network capacity
///
/// ```text
/// budget = budget * production_safe_pm / production.ratio_pm;
/// ```
///
/// A server taking longer to produce a segment does not reduce the link's bit rate. Until the
/// transport exposes an encoder-only clock, total acquisition remains telemetry and an upward
/// candidate's own `A<=D` observation is its physical feasibility test. A terminal downshift has
/// the explicit funded-floor exception documented by [`candidate_media_reserve_deadline`].
/// Representing total acquisition here as fewer network kilobits double-counted it and mixed two
/// actuators into a number named for one.
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

/// **The reserve-funded budget for a candidate's initial media transfer.**
///
/// At the transaction boundary `reserve` is playable presentation time. While the picture runs and
/// the current stream is not being acquired, it falls by one millisecond per millisecond of
/// playhead advance: `B_after = B_start - Δplayhead`, with no coefficient. This helper computes
/// the duration grant; the transport seam chooses its clock. Upward exploration projects the
/// playhead balance, while downshift recovery spends non-user-paused elapsed time so an internal
/// stall remains recovery cost. Native-accepted user Pause spends neither.
///
/// Direction changes what that boundary means. An upshift buys quality while a picture remains, so
/// its initial media budget is exactly the reserve already granted to the end-to-end exploration.
/// Admission is evaluated on the completed object; a setup-bearing object and at most one
/// repeatable observation remain staged under that same grant until one final verdict. A
/// downshift restores the picture, so refusing it before the measured whole-acquisition prediction
/// would create an absorbing stall; its budget is therefore floored at that prediction below. Once
/// either the current bag cannot replay its observed chronology at `B<R_o` or the main thread has
/// observed terminal `B=0`, [`candidate_media_reserve_deadline`] separately removes the
/// rollback-reserve deadline from the ladder-floor response because no cheaper actuator exists.
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
/// * a bound on the reserve the new rung needs on arrival, `A_j` — which is exactly what the
///   acquisition window would supply, and exactly what it cannot supply here: a delivery collapse
///   resets the window, and a collapse is the event that produced the downshift.
///
/// An in-response prefix-rate projection is not an available alternative: a growing PMS HLS body
/// right-censors server production together with network service. Absent an arrival-reserve bound,
/// the alternative to the physical bound is a fraction of it — which would be the
/// unexplained multiplier the design rule forbids, doing the work of a model nobody has written.
/// So the deadline is the reserve while that reserve exists, and the effect on `E_tx_down` is that
/// it becomes bounded BY CONSTRUCTION rather than by luck. The already-stalled terminal floor is
/// not `E_tx_down`: it is the only remaining recovery actuator and runs to a transport result.
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
/// `fixed_overhead` is the per-segment cost that does not move bytes — connection, request, the
/// AVIO open, FFmpeg's probe — taken from the acquisition window's worst observation. It is ADDED
/// to the predicted transfer rather than folded into it, because the two are measured separately
/// and only their sum is what a warm-up actually has to fit inside.
///
/// **Without it the downshift floor pays for the body read alone**, and the device showed what
/// that costs: nineteen consecutive `outcome=warmup_deadline` with `warmup=nonems`, deadlines of
/// 430-723 ms against segments that measured `total_ms=582` in the STEADY state, on a freshly
/// created encoder session that is dearer than steady state. See
/// `a_downshift_warmup_budget_must_cover_the_whole_acquisition_not_only_the_body_read`.
pub(crate) fn candidate_warmup_budget(
    proposal: Proposal,
    _media_duration: std::time::Duration,
    reserve: std::time::Duration,
    predicted_transfer: std::time::Duration,
    fixed_overhead: std::time::Duration,
) -> std::time::Duration {
    // A NEW PMS encoder's first segment carries decoder and encoder cold start and is not the
    // cadence the replacement will sustain. An upshift therefore spends exactly the exploration
    // reserve its caller proved disposable above the current runway; imposing `3/2·D` here was an
    // unrelated timer that could both waste a deep reserve and reject a safely funded cold start.
    let cold_start = match proposal.direction {
        Direction::Up => reserve,
        // A downshift has no acceptance test at all — see above — so the reserve is its only
        // acceptance bound. `MAX` is not a deadline; it is the identity element of the `min`
        // below, written that way so there is exactly one place where a candidate transfer's
        // budget is decided.
        Direction::Down => std::time::Duration::MAX,
    };
    // **The floor, and it applies to a DOWNSHIFT only.** See the section above it.
    match proposal.direction {
        // The upshift budget already covers the whole transaction in presentation-reserve time,
        // including fixed overhead while the playhead advances; adding that overhead again would
        // double-count it. User Pause freezes the same budget in the transport layer.
        Direction::Up => cold_start.min(reserve),
        Direction::Down => cold_start
            .min(reserve)
            .max(predicted_transfer.saturating_add(fixed_overhead)),
    }
}

/// Apply the reserve-funded media deadline, except when the controller or the media clock has
/// already removed the robust rollback guarantee that deadline protects.
///
/// A candidate deadline protects a still-playing picture from spending more reserve than the
/// transaction was funded with. There are two exact terminal certificates:
///
/// * the main thread has observed `B = 0` and already holds the media clock; or
/// * the completed current-rung bag has `B < R_o`, where `R_o` is its exact ordered-replay runway.
///   `ReservePolicy::TerminalFloor` is the controller's typed transaction contract for that state.
///
/// The second certificate necessarily arrives first in the measured failure: the controller
/// decides after a completed segment while `B` is still positive, then session creation and the
/// playlists spend the remainder. Waiting for the strictly later `B=0` latch makes the floor
/// response inherit a reserve deadline after its robust rollback premise has already been lost. It
/// aborts, the old rung completes one more segment, and the same cycle starts again.
///
/// If the proposal is a downshift to the ladder floor, no cheaper response exists under either
/// certificate. The floor response therefore runs to an actual transport result; HTTP, playlist
/// and body liveness limits still apply, only the rollback-reserve deadline does not. Every other
/// proposal retains its exact budget. This adds no threshold: `B<R_o` is the conservation
/// predicate that selected the floor in the first place.
pub(crate) fn candidate_media_reserve_deadline(
    proposal: Proposal,
    reserve_policy: ReservePolicy,
    rebuffering: bool,
    budget: std::time::Duration,
) -> Option<std::time::Duration> {
    let no_rollback_path = rebuffering || reserve_policy == ReservePolicy::TerminalFloor;
    (!(no_rollback_path && proposal.direction == Direction::Down && proposal.rung.at_floor()))
        .then_some(budget)
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

/// Historical unconditional second-segment budget retained for the archived plant and tests.
///
/// The live path does not call this helper. A boundary object which satisfies
/// `A <= D && B_post >= A` decides immediately; only a funded setup-bearing object with `A > D`
/// conditionally buys one ordinary observation from freshly credited reserve. This helper preserves
/// the former unconditional `min(D, reserve)` calculation solely for compatibility fixtures.
#[allow(dead_code)]
pub(crate) fn candidate_prime_budget(
    media_duration: std::time::Duration,
    _policy: &AbrPolicy,
    reserve: std::time::Duration,
) -> std::time::Duration {
    // Real time is the physical boundary: a JIT encoder whose completed steady segment costs more
    // wall time than the media it contributes drains reserve indefinitely.  `1000 pm` is a unit
    // identity, not a product margin, so express it directly as the duration itself.
    media_duration.min(reserve)
}

/// Historical `E_tx` compatibility readout retained for archived transaction-ledger tests.
///
/// Live exploration has no fixed post-transaction time debt: its initial phase spends the exact
/// playhead-funded surplus, a conditional setup-bearing continuation is funded only after real
/// media credit, and failure release requires strictly more actually executed disposable reserve.
/// Returning zero prevents an old caller from reintroducing a second time charge after those
/// physical clocks have already accounted for the transaction.
#[allow(dead_code)] // compatibility seam for historical transaction-ledger tests
pub(crate) fn upshift_transaction_cost(
    _media_duration: std::time::Duration,
    _policy: &AbrPolicy,
) -> std::time::Duration {
    // Retained as a compatibility/read-out seam. Exploration no longer has a time debt: its
    // transaction is bounded by spendable reserve and another attempt is released by strictly
    // more spendable reserve, not by waiting a computed interval.
    std::time::Duration::ZERO
}

/// The playable reserve as a wall-clock budget. A reserve at or below zero is `ZERO`, which makes
/// the deadline "now" — correct, because a transaction starting with no reserve has already
/// stalled and every further millisecond it spends is a millisecond of stall.
pub(crate) fn reserve_as_budget(reserve_ms: i64) -> std::time::Duration {
    std::time::Duration::from_millis(u64::try_from(reserve_ms.max(0)).unwrap_or(0))
}
