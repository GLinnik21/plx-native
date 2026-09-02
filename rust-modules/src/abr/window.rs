//! Finite acquisition-bag physics for the active HLS operating point.
//!
//! The live controller never treats `bytes / active_time` from a finite, demand-capped response as
//! the path's unused capacity. For every completed acquisition `i` in the current finite
//! operating-point episode, this module records only facts: end-to-end acquisition time `A_i` and
//! playable media duration `D_i`. It then computes
//!
//! ```text
//! sustainable       <=>  Σ A_i <= Σ D_i
//! ordered runway R_o  =  max_i(Σ_(j<i)(A_j-D_j) + A_i)
//! stress runway R_s   =  Σ (A_i-D_i)+ + max_i min(A_i,D_i)
//! ```
//!
//! `R_o` is the exact starting reserve for the chronology actually observed and is the live
//! current-rung stay/down certificate. `R_s` is the exact worst-permutation replay boundary used
//! to fund discretionary exploration. Its terminal `max min(A,D)` matters because a segment's
//! media is credited only after the acquisition completes. Both are retrospective conservation
//! identities, not confidence margins or a claim about an unseen future draw. Their sufficient
//! statistics are folded into an exact associative summary, so an episode does not forget its
//! first acquisition when the separate diagnostics ring wraps.
//!
//! A higher rendition cannot be inferred from this bag: its larger request may obtain more service
//! than the current demand-capped response ever asked for. The controller therefore spends only
//! reserve above `max(R_s,D_next)` on an actual candidate transaction and grades that candidate
//! directly. A completed one-point upshift reduces to `A <= D && B_post >= A`; an abandoned prefix
//! enters no bag.
//!
//! The bounded ring and its older transferred-byte/order-statistic machinery remain executable
//! below for historical corpus comparisons and diagnostics. They are explicitly not a live ABR
//! gate: adaptive invocation is data-dependent, candidate size changes the queried distribution,
//! and no exchangeability theorem licenses its former probability interpretation.

use super::Rung;

/// **Storage bound on the retired diagnostics ring, not a live policy choice.** The exact live
/// summary is episode-long and does not evict. The retired order-statistic readout asks for
/// `n = k/eps - 1` (see [`AdmissionPolicy`]); that historical request is clamped to this
/// implementation limit but no longer decides playback.
///
/// A previous draft of the specification wrote `n <= 32` into the *derivation*, which silently made
/// several `(n, k)` settings unreachable — at that cap `eps >= k/33`, so `k = 3` could not express
/// an eps below 9.09%. Keeping the storage bound clearly separate from the derived length is what
/// stops that from recurring: raising this constant changes what is *representable*, never what is
/// *chosen*.
pub(crate) const WINDOW_CAPACITY: usize = 64;

/// One observed acquisition. `bytes` on the wire, `acquisition_us` end to end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Acquisition {
    bytes: u64,
    acquisition_us: u64,
    /// Playable media credited only after this acquisition completes.  It belongs to the sample:
    /// EXTINF durations may vary, so substituting the current segment's `D` for every historical
    /// observation breaks both conservation sums dimensionally.
    media_duration_ms: i64,
    /// The part of `acquisition_us` that does NOT grow with `bytes`: connection, request, headers,
    /// the AVIO open, FFmpeg's probe, scheduling. `acquisition_us - active_fetch_us`.
    ///
    /// Kept because [`AcquisitionWindow::transferred_us`] has to scale one part and not the other,
    /// and the two are only separable at observation time. Device-measured 2026-08-30: a 130 284
    /// byte segment cost `total_ms=582` of which `open_ms=370` (`open_probe_ms=207`) was open and
    /// probe, on a server whose `not_ready` was 0 on every segment of the run — so the fixed half
    /// was 64% of the acquisition and multiplying it by a byte ratio invented over a second per
    /// sample.
    overhead_us: u64,
}

/// Exact sufficient statistics for one finite operating-point episode.
///
/// For a sequence `x` followed by `y`, the ordered replay boundary composes as
///
/// ```text
/// delta(x)       = sum_A(x) - sum_D(x)
/// runway(x ++ y) = max(runway(x), delta(x) + runway(y))
/// ```
///
/// while all sums add and all sample maxima take `max`. That makes this a constant-space,
/// associative summary of the complete episode: callers may fold samples one at a time or merge
/// adjacent chunks and obtain exactly the same terms. The two stress-runway components are kept
/// separately because `sum (A-D)+ + max min(A,D)` is the exact worst-permutation boundary.
///
/// Every fallible operation is checked. `overflowed` is absorbing; once exact representation is
/// impossible, both live admission forms return a conservative refusal. Saturating an acquisition
/// sum and a duration sum independently can make them equal and silently turn overflow into an
/// upgrade, so saturation is not a valid implementation of this summary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AdmissionSummary {
    /// Number of completed acquisitions in the episode.
    n: usize,
    /// `Σ A_i`.
    sum_acquisition_us: i64,
    /// `Σ D_i`.
    sum_duration_us: i64,
    /// `Σ(A_i-D_i)`; also the shift applied to a following chunk's prefix runway.
    delta_us: i64,
    /// `max_i(Σ_(j<i)(A_j-D_j) + A_i)` in the episode's observed order.
    max_prefix_runway_us: i64,
    /// `Σ(A_i-D_i)+`, the additive part of the worst-permutation runway.
    positive_slack_sum_us: i64,
    /// `max_i min(A_i,D_i)`, the terminal part of the worst-permutation runway.
    max_capped_delivery_us: i64,
    /// `max_i(A_i-active_i)`, retained independently of the diagnostics ring.
    sample_overhead_max_us: u64,
    overflowed: bool,
}

impl AdmissionSummary {
    fn from_sample(sample: Acquisition) -> Self {
        let overhead = sample.overhead_us;
        let Ok(acquisition_us) = i64::try_from(sample.acquisition_us) else {
            return Self::poisoned(1, overhead);
        };
        let Some(duration_us) = sample.media_duration_ms.checked_mul(1_000) else {
            return Self::poisoned(1, overhead);
        };
        let Some(delta_us) = acquisition_us.checked_sub(duration_us) else {
            return Self::poisoned(1, overhead);
        };
        let summary = Self {
            n: 1,
            sum_acquisition_us: acquisition_us,
            sum_duration_us: duration_us,
            delta_us,
            max_prefix_runway_us: acquisition_us,
            positive_slack_sum_us: delta_us.max(0),
            max_capped_delivery_us: acquisition_us.min(duration_us),
            sample_overhead_max_us: overhead,
            overflowed: false,
        };
        // For one sample the stress runway is exactly A. Keep the derived checked operation in the
        // construction path so a future change cannot add individually representable terms whose
        // live runway is not representable.
        if summary.stress_runway_us().is_none() {
            Self::poisoned(1, overhead)
        } else {
            summary
        }
    }

    /// Concatenate two adjacent episode summaries. Exact inputs either produce their exact
    /// associative composition or the absorbing conservative overflow state.
    fn combine(self, rhs: Self) -> Self {
        let sample_overhead_max_us = self.sample_overhead_max_us.max(rhs.sample_overhead_max_us);
        let Some(n) = self.n.checked_add(rhs.n) else {
            return Self::poisoned(usize::MAX, sample_overhead_max_us);
        };
        if self.overflowed || rhs.overflowed {
            return Self::poisoned(n, sample_overhead_max_us);
        }
        let Some(sum_acquisition_us) = self.sum_acquisition_us.checked_add(rhs.sum_acquisition_us)
        else {
            return Self::poisoned(n, sample_overhead_max_us);
        };
        let Some(sum_duration_us) = self.sum_duration_us.checked_add(rhs.sum_duration_us) else {
            return Self::poisoned(n, sample_overhead_max_us);
        };
        // Derive the canonical delta from the two non-negative monotone sums. Accumulating signed
        // deltas directly would make overflow depend on grouping when later samples cancel it.
        let Some(delta_us) = sum_acquisition_us.checked_sub(sum_duration_us) else {
            return Self::poisoned(n, sample_overhead_max_us);
        };
        let Some(shifted_rhs_runway) = self.delta_us.checked_add(rhs.max_prefix_runway_us) else {
            return Self::poisoned(n, sample_overhead_max_us);
        };
        let max_prefix_runway_us = self.max_prefix_runway_us.max(shifted_rhs_runway.max(0));
        let Some(positive_slack_sum_us) = self
            .positive_slack_sum_us
            .checked_add(rhs.positive_slack_sum_us)
        else {
            return Self::poisoned(n, sample_overhead_max_us);
        };
        let max_capped_delivery_us = self.max_capped_delivery_us.max(rhs.max_capped_delivery_us);
        let summary = Self {
            n,
            sum_acquisition_us,
            sum_duration_us,
            delta_us,
            max_prefix_runway_us,
            positive_slack_sum_us,
            max_capped_delivery_us,
            sample_overhead_max_us,
            overflowed: false,
        };
        if summary.stress_runway_us().is_none() {
            Self::poisoned(n, sample_overhead_max_us)
        } else {
            summary
        }
    }

    fn poisoned(n: usize, sample_overhead_max_us: u64) -> Self {
        Self {
            n,
            sum_acquisition_us: i64::MAX,
            sum_duration_us: i64::MAX,
            delta_us: 0,
            max_prefix_runway_us: i64::MAX,
            positive_slack_sum_us: i64::MAX,
            max_capped_delivery_us: i64::MAX,
            sample_overhead_max_us,
            overflowed: true,
        }
    }

    fn stress_runway_us(self) -> Option<i64> {
        self.positive_slack_sum_us
            .checked_add(self.max_capped_delivery_us)
    }

    fn conservative_refusal(self) -> Admission {
        Admission {
            sustainable: false,
            survivable: false,
            demand_us: i64::MAX,
            supply_us: 0,
            excess_us: i64::MAX,
            runway_us: i64::MAX,
            samples: self.n,
        }
    }

    fn admission(self, reserve_ms: i64, ordered: bool) -> Option<Admission> {
        if self.n == 0 {
            return None;
        }
        if self.overflowed {
            return Some(self.conservative_refusal());
        }
        let (excess_us, runway_us) = if ordered {
            (self.delta_us.max(0), self.max_prefix_runway_us)
        } else {
            let Some(runway_us) = self.stress_runway_us() else {
                return Some(self.conservative_refusal());
            };
            (self.positive_slack_sum_us, runway_us)
        };
        // A reserve outside the microsecond representation is not permission. This is unreachable
        // for real media lengths, but the failure direction remains conservative at the type edge.
        let survivable = reserve_ms
            .checked_mul(1_000)
            .is_some_and(|reserve_us| reserve_us >= runway_us);
        Some(Admission {
            sustainable: self.sum_acquisition_us <= self.sum_duration_us,
            survivable,
            demand_us: self.sum_acquisition_us,
            supply_us: self.sum_duration_us,
            excess_us,
            runway_us,
            samples: self.n,
        })
    }
}

/// **Retired/offline only:** the two numbers that decide `n` in the old classification rule.
///
/// # `eps` is a DESIGN RATIO, not a probability — downgraded 2026-08-29
///
/// This doc said "the tolerated **probability** that one acquisition exceeds the bound", and that
/// reading is not available here. Three independent reasons, any one sufficient:
///
/// 1. **The domination proof needs a raw bound that does not hold.** The module doc argues the
///    transferred bound dominates the raw k-th largest, "and the raw comparison is the identity
///    map on the bag — genuinely fixed, genuinely exchangeable". But the quantity being bounded is
///    the cost of the CANDIDATE at query bytes `q`, and an upshift — the only gated direction —
///    has `q > b_i`, so that draw is stochastically larger than every window sample and is not
///    exchangeable with them. Dominating a bound that does not hold proves nothing. The repair
///    routes through counterfactual same-size costs `A_i(q) = O0_i + q*tau_i`, which needs the
///    affine model with identically-distributed per-segment coefficients — the assumption the
///    project's own corpus refutes on 36.6% of pairs.
/// 2. **Invocation is data-dependent.** Order-statistic coverage is MARGINAL, over the joint draw.
///    The controller consults this rule only when the dwell has expired, a dearer target was
///    selected from the budget, the reserve is above the gate and not draining, and no reject
///    block is live — every one a function of the same recent data. Conditioning on "the window
///    looks healthy" selects windows whose k-th largest is low, so conditional exceedance exceeds
///    `k/(n+1)` even for i.i.d. samples. No exotic dependence is needed.
/// 3. **The sample was shaped.** A detected collapse resets both the retired window and today's
///    live finite bag, so evaluated histories hold only post-collapse samples (survivorship, in
///    the anti-conservative direction), while pauses can survive as the estimator retracts
///    confidence in the preceding era.
///
/// **What the rule does deliver**, and what should be argued about instead: (i) a deterministic
/// property — at most `k-1` of the last `n` transferred values exceed the bound; (ii) conditions
/// (1) and (2) as deterministic statements about the last `n*D` of media under the worst-case
/// transfer; (iii) an EMPIRICAL, marginal exceedance record from the corpus — at nominal on
/// stationary legs, about 2x over on swept ones. A number realized 2-4x off in either direction is
/// a design ratio, not a probability.
///
/// **The downgrade costs nothing operationally**, which is why it is safe to state plainly:
/// `bound_us` is telemetry and has no consumer outside the read-out, and the deciders — `admits`'
/// two conditions — consume only `n`. `eps`'s real content already was "the dial that sets
/// `n = k/eps - 1`", i.e. the evidence length and, through (2), the proof span.
///
/// `eps` is (4), a product/SLO choice: the design exceedance ratio `k/(n+1)`, chosen for the
/// window length it implies. `k` is ALSO (4) and was missed by an earlier draft that said "nothing else is chosen" —
/// `eps` pins only the RATIO `k/(n+1)`, leaving `k` free. It is not neutral, because it sets the
/// window length `n = k/eps - 1`, and the window is two other things at once: the estimator's
/// exposure to a link that is changing, and — through condition (2) — the span of media the reserve
/// condition proves survival over.
///
/// Its meaning, stated so it can be argued with: **how many of the last `n` acquisitions may exceed
/// the bound before the estimate is considered wrong.** `k = 1` is the tightest bound and the most
/// sensitive to a single outlier; larger `k` is robust and buys a longer proof horizon at the same
/// `eps`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AdmissionPolicy {
    /// The design exceedance ratio `k/(n+1)`, in per-mille — NOT a coverage probability; see the
    /// struct doc. (4).
    pub(crate) epsilon_pm: u32,
    /// Order statistic. (4). See the struct doc for what choosing it means.
    pub(crate) k: u32,
}

#[allow(dead_code)] // retired order-statistic design remains executable for offline corpus tests
impl AdmissionPolicy {
    /// `n = k/eps - 1`, clamped to what the ring can hold.
    ///
    /// R28's corrected theorem is `k/eps - 1`, not `1/eps - 1`. The clamp is a storage fact, and it
    /// is reported by [`Self::is_clamped`] rather than hidden, because a silently shortened window
    /// weakens condition (2)'s proof span without weakening anything that says so.
    pub(crate) fn window_len(self) -> usize {
        (self.requested_len() as usize).clamp(1, WINDOW_CAPACITY)
    }

    pub(crate) fn is_clamped(self) -> bool {
        self.requested_len() > WINDOW_CAPACITY as u64
    }

    /// R28's theorem itself, UNCLAMPED — the length the policy asks for. Both readers above are
    /// about the same number, and it was written out twice: `window_len` computed it and clamped,
    /// `is_clamped` recomputed it to compare. One expression could move without the other.
    ///
    /// **CEILED, and every other bound in this file already says why.** The rule wants
    /// `k/(n+1) <= eps`, i.e. `n >= ceil(1000k/eps) - 1`; a floored division returns a window one
    /// sample SHORT whenever `eps` does not divide `1000k`, and a shorter window means a LARGER
    /// realized eps than the one asked for. That is a weakening, and — unlike the clamp, which
    /// `is_clamped` reports — it was silent: `is_clamped()` stays false, and
    /// `AdmissionReadout`'s doc attributed every divergence to clamping.
    ///
    /// Nothing shipped is affected. At `(k=1, eps=50pm)` the division is exact (1000/50 = 20,
    /// n = 19), as it is at all four settings the tests pin. Over `k = 1..4, eps = 10..500pm`,
    /// **1 810 of 1 964 settings carried the silent error**, worst realized inflation 1.497x
    /// (`k=1, eps=334pm`: n = 1, realized 500 pm). So this is robustness for a policy nobody has
    /// chosen yet, not a change to the one in force.
    fn requested_len(self) -> u64 {
        (u64::from(self.k.max(1)) * 1_000)
            .div_ceil(u64::from(self.epsilon_pm.max(1)))
            .saturating_sub(1)
    }

    /// The realized design ratio `k/(n+1)` at the length actually used — which is `eps` only when
    /// the window is neither clamped nor shortened by an inexact division. A RATIO, not a
    /// coverage probability (see the struct doc).
    ///
    /// **Also ceiled**, for the reason every other bound here is: this is a ceiling on exceedance,
    /// so truncating it reports a guarantee STRONGER than the one the window delivers. The error
    /// is under 1 pm and the direction is the one that matters — it is the number the harness
    /// parses as `eps=`.
    pub(crate) fn effective_epsilon_pm(self) -> u32 {
        let n = self.window_len() as u64;
        (u64::from(self.k.max(1)) * 1_000)
            .div_ceil(n + 1)
            .min(1_000) as u32
    }
}

/// What the admission rule concluded, with both conditions readable separately.
///
/// Kept apart deliberately: (1) failing means the rung is not sustainable at all, while (2) failing
/// means it is sustainable on average but the reserve cannot absorb its peaks. Those call for
/// different actions and a single boolean would lose the distinction — which is how the shipped
/// controller ended up with a `4/5` haircut standing in for both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Admission {
    /// `sum A_i <= sum D_i`.
    pub(crate) sustainable: bool,
    /// Whether `B` covers [`Self::runway_us`] under the evaluator's replay order.
    pub(crate) survivable: bool,
    /// `sum A_i`, microseconds.
    pub(crate) demand_us: i64,
    /// `sum D_i`, microseconds — each acquisition keeps its own EXTINF duration.
    pub(crate) supply_us: i64,
    /// Non-negative deficit statistic used by the evaluator: total positive deficits for the
    /// adversarial stress replay, terminal prefix debt for the chronological replay.
    pub(crate) excess_us: i64,
    /// Minimum reserve for the evaluator's replay order when media is credited only at the end of
    /// each acquisition. Stress evaluation uses the worst permutation
    /// `sum(T-D)+ + max(min(T,D))`; current-rung evaluation uses the observed chronology
    /// `max_i(sum_(j<i)(A_j-D_j) + A_i)`.
    pub(crate) runway_us: i64,
    /// Samples the verdict rests on.
    pub(crate) samples: usize,
}

impl Admission {
    pub(crate) fn admitted(self) -> bool {
        self.sustainable && self.survivable
    }
}

/// Everything one acquisition-episode readout concluded, in one event-log line.
///
/// The wire shape is shared deliberately. [`AcquisitionWindow::observed_readout`], the live mode,
/// uses the whole finite episode immediately: `want=have`, `eps=0`, no bound, and only an empty
/// episode is `filling`. [`AcquisitionWindow::readout`] is the retired order-statistic/corpus mode:
/// it can have `have < want`, a non-zero epsilon and a transferred bound. Keeping both generations
/// explicit prevents an archived shadow trace from being mistaken for the live controller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AdmissionReadout {
    /// Complete episode sample count live; samples held by the bounded ring in retired mode.
    pub(crate) have: usize,
    /// Samples used. Equal to the complete episode's `have` live; the requested order-stat length
    /// in retired readouts.
    pub(crate) want: usize,
    /// Zero live; the retired design ratio `k/(n+1)` in an order-statistic readout.
    pub(crate) effective_epsilon_pm: u32,
    /// Always false live; whether a retired order-statistic request exceeded storage.
    pub(crate) clamped: bool,
    /// Always `None` live; the retired k-th-largest transferred acquisition otherwise.
    pub(crate) bound_us: Option<i64>,
    /// Both conservation conditions. `None` only for an empty live episode, or while a retired
    /// order-statistic readout is filling.
    pub(crate) admission: Option<Admission>,
    /// Cumulative [`AcquisitionWindow::reset`] count. Monotone over a playback.
    pub(crate) resets: u32,
}

impl AdmissionReadout {
    /// `Some(true)`/`Some(false)` when terms exist; `None` for a live empty bag or retired fill.
    pub(crate) fn admitted(self) -> Option<bool> {
        self.admission.map(Admission::admitted)
    }

    /// **The event-log line, formatted here so the shape is testable beside the arithmetic.**
    ///
    /// A line of its own rather than fields appended to `abr: sample`: the sample says what arrived
    /// and whether it completed; this line publishes every conservation term derived from the
    /// certified bag. The independent grader needs both and pairs them in log order.
    ///
    /// * `have`/`want` — equal live and cover the complete current-operating-point episode. A retired
    ///   readout may instead fill toward an order-statistic length.
    /// * `eps`/`clamp`/`bound` — `0/0/-1` live; retained only to make old captures unambiguous.
    /// * `demand`/`supply` — `sum A_i` against `sum D_i`, in milliseconds.
    /// * `excess` — `sum (A_i-D_i)+`, the accumulated deficit component.
    /// * `runway` — condition (2)'s complete reserve requirement, including the acquisition that
    ///   must finish before its `D_i` is credited.
    ///
    /// Every unavailable number prints `-1` rather than `0`: while the window is filling those
    /// quantities are NOT COMPUTED, and a zero cannot say the difference — a zero `excess` is a
    /// perfectly ordinary healthy verdict.
    ///
    /// `reset` is cumulative and monotone within one controller. Every committed actuator change
    /// resets the old operating-point bag and seeds the new one from candidate evidence; a new
    /// `abr: seed` starts a fresh controller epoch at zero.
    ///
    /// `bytes` is retained for trace compatibility. Live conservation does not use it to decide
    /// bag membership or project another request size; membership comes from `complete=` and every
    /// stored observation keeps its own actual byte count.
    pub(crate) fn log_line(self, current_kbps: u32, bytes: u64, media_duration_ms: u32) -> String {
        let verdict = match self.admitted() {
            None => "filling",
            Some(true) => "admit",
            Some(false) => "refuse",
        };
        let ms = |us: i64| us / 1_000;
        let (demand, supply, excess, runway) = self
            .admission
            .map(|a| {
                (
                    ms(a.demand_us),
                    ms(a.supply_us),
                    ms(a.excess_us),
                    ms(a.runway_us),
                )
            })
            .unwrap_or((-1, -1, -1, -1));
        format!(
            "abr: window current={current_kbps}kbps verdict={verdict} have={}/{} eps={}pm \
             clamp={} bound={}ms demand={demand}ms supply={supply}ms excess={excess}ms \
             runway={runway}ms \
             sus={} sur={} reset={} bytes={bytes} dur={media_duration_ms}ms",
            self.have,
            self.want,
            self.effective_epsilon_pm,
            u8::from(self.clamped),
            self.bound_us.map(ms).unwrap_or(-1),
            self.admission.map(|a| u8::from(a.sustainable)).unwrap_or(0),
            self.admission.map(|a| u8::from(a.survivable)).unwrap_or(0),
            self.resets,
        )
    }
}

/// **Retired/offline only:** the old admission rule's worst-case candidate byte query.
///
/// ```text
/// q = sigma * W_j * D / 8000            W in bit/s, D in ms, sigma per-mille
/// ```
///
/// `W_j` is the rate the candidate's OWN master playlist declared, not the catalog's guess for that
/// rung — the catalog rate is the input the plan's R1 killed (+5.2% to +31.6% error, item-dependent,
/// non-injective). `sigma` is [`Rung::size_spread_pm`], per-rung and measured.
///
/// **CEILED**, because this is a safety bound and flooring one points the wrong way. The
/// association is the specification's and is not a style question: folding `sigma*W*D*tau` into one
/// product before dividing reaches 1.6e19 at rung 22000, past `i64::MAX`. Computing the byte count
/// first keeps every intermediate under 1e14 — at rung 22000 with D = 2000 the numerator is 3.6e13,
/// five orders inside `u64`.
///
/// A zero or missing declared rate returns 0. Any offline caller must treat that as an incompatible
/// record rather than a free request: a zero query makes every transfer factor 1, the most
/// permissive retired verdict.
#[allow(dead_code)]
pub(crate) fn candidate_worst_case_bytes(
    declared_bps: u64,
    media_duration_ms: i64,
    sigma_pm: u32,
) -> u64 {
    if declared_bps == 0 || media_duration_ms <= 0 {
        return 0;
    }
    let numerator = u64::from(sigma_pm)
        .saturating_mul(declared_bps)
        .saturating_mul(media_duration_ms as u64);
    numerator.saturating_add(7_999_999) / 8_000_000
}

/// One operating-point episode plus a bounded diagnostics ring of its most recent acquisitions.
/// The associative `summary` never evicts; only `ring` does. The live controller calls
/// [`Self::reset`] on every rung commit and immediately seeds the new episode from the completed
/// candidate, so evidence from a smaller or larger request never crosses that actuator boundary.
/// The retired transferred-byte methods below still use the ring for historical corpus work, but
/// the live controller does not.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AcquisitionWindow {
    ring: [Acquisition; WINDOW_CAPACITY],
    len: usize,
    next: usize,
    summary: AdmissionSummary,
    /// How many times [`Self::reset`] has run, for the whole playback.
    ///
    /// A reset is otherwise visible only as a drop in `have`. Commit resets have a matching marker
    /// and transaction seed triple, but the delivery estimator's collapse reset has no boundary
    /// marker. The grader uses this exact increment to detect either case; an unmarked increment
    /// makes the following span unattributable rather than pretending the old episode continued.
    /// A new controller announces `abr: seed` and legitimately starts the counter at zero again.
    resets: u32,
}

impl Default for AcquisitionWindow {
    fn default() -> Self {
        Self {
            ring: [Acquisition::default(); WINDOW_CAPACITY],
            len: 0,
            next: 0,
            summary: AdmissionSummary::default(),
            resets: 0,
        }
    }
}

impl AcquisitionWindow {
    /// `active_us` is the part that moved bytes; the rest of `acquisition_us` is fixed per-segment
    /// cost that [`Self::transferred_us`] must not scale. See [`Acquisition::overhead_us`]. A
    /// caller with no separate measurement passes `acquisition_us` for both, which reproduces the
    /// old all-proportional behaviour exactly.
    pub(crate) fn observe(
        &mut self,
        bytes: u64,
        acquisition_us: u64,
        active_us: u64,
        media_duration_ms: i64,
    ) {
        if bytes == 0 || acquisition_us == 0 || media_duration_ms <= 0 {
            // A malformed observation must not enter the window: `bytes` is a divisor.
            return;
        }
        let overhead_us = acquisition_us.saturating_sub(active_us.min(acquisition_us));
        let sample = Acquisition {
            bytes,
            acquisition_us,
            media_duration_ms,
            overhead_us,
        };
        self.summary = self.summary.combine(AdmissionSummary::from_sample(sample));
        self.ring[self.next] = sample;
        self.next = (self.next + 1) % WINDOW_CAPACITY;
        self.len = (self.len + 1).min(WINDOW_CAPACITY);
    }

    pub(crate) fn reset(&mut self) {
        // The counter is the one thing a reset must NOT clear -- it is the record that the reset
        // happened, and `*self = Self::default()` would erase its own evidence.
        let resets = self.resets.saturating_add(1);
        *self = Self {
            resets,
            ..Self::default()
        };
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    #[cfg(test)]
    pub(crate) fn episode_len(&self) -> usize {
        self.summary.n
    }

    /// **The largest fixed per-segment cost this episode has seen**, microseconds — connection,
    /// request, the AVIO open, FFmpeg's probe. `0` on an empty window, which leaves any caller
    /// adding it exactly where it was.
    ///
    /// The MAXIMUM rather than a mean, because the one caller is a DEADLINE: a budget built from
    /// the average overhead is a coin flip on every segment whose overhead is above it, and
    /// `predicted_transfer`'s own doc records the device measuring that coin landing wrong 53
    /// times in a row.
    ///
    /// **The cost of that choice, stated rather than discovered later.** A max over the whole
    /// finite operating-point episode lets ONE pathological open — a hiccup, a server reconnect —
    /// inflate every warm-up deadline until the episode resets. Over-granting a downshift
    /// deadline spends reserve while waiting for a candidate that is not coming, which is the
    /// mirror of the failure this exists to fix. Max is chosen because the failure that was
    /// actually MEASURED is the under-granting one (nineteen consecutive `warmup_deadline` with
    /// `warmup=nonems`), and because the budget is a `max` with the reserve anyway, so on a healthy
    /// playback the reserve dominates this term entirely. A k-th largest order statistic — the
    /// idiom [`Self::bound_us`] already uses — is the obvious refinement, and there is no evidence
    /// yet that says which `k`; do not add one without a trace that shows the inflation happening.
    pub(crate) fn worst_overhead_us(&self) -> u64 {
        self.summary.sample_overhead_max_us
    }

    /// Most recent first.
    fn recent(&self, want: usize) -> impl Iterator<Item = Acquisition> + '_ {
        let take = want.min(self.len);
        (0..take).map(move |back| {
            let idx = (self.next + WINDOW_CAPACITY - 1 - back) % WINDOW_CAPACITY;
            self.ring[idx]
        })
    }

    /// `overhead_i + body_i * max(1, q / b_i)`, in microseconds, rounded UP.
    ///
    /// **Only the part that moves bytes is scaled**, and that is a correction rather than a
    /// refinement: this read `A_i * q / b_i` over the WHOLE acquisition, which asserts that a
    /// rendition four times the size also takes four times as long to open and four times as long
    /// to probe. Neither is true of either. The device case is in [`Acquisition::overhead_us`];
    /// its consequence was a playback that would not climb off 720 kbps with 68 seconds of reserve
    /// and a link carrying the candidate twice over.
    ///
    /// The overhead is still CHARGED — once, as it is actually incurred. It is real dead time in
    /// which playback receives no segment, which is what the sustainability condition is about, and
    /// dropping it (by scaling `active_fetch_us` alone) would be the opposite error.
    ///
    /// Ceiling, not floor: this is a safety bound and flooring one is a bound in the wrong
    /// direction.
    ///
    /// **`u128`, and not the `i128` a first draft used, for a reason a test caught.** The product
    /// of two `u64`s is `(2^64-1)^2`, which fits `u128` exactly and exceeds `i128::MAX` — so the
    /// signed intermediate overflows at the top of the input domain. Real acquisitions are nowhere
    /// near it (a 60 s fetch is 6e7 us, a segment is under 2e7 bytes), but "unreachable today" is
    /// not a bound, and the two halves of this codebase disagree about what an overflow DOES:
    /// `overflow-checks` is on under `cargo test`, so the host panics inside the demux worker,
    /// and off in release, so the television wraps to a small number — a bound in the unsafe
    /// direction, silently, once per segment. Unsigned throughout, saturating at the end, is the
    /// only form with neither failure.
    fn transferred_us(sample: Acquisition, query_bytes: u64) -> i64 {
        if query_bytes <= sample.bytes {
            return sample.acquisition_us.min(i64::MAX as u64) as i64;
        }
        // Saturating, not a bare subtraction: `SegmentSample::new` requires
        // `total_fetch_us >= active_fetch_us`, so this cannot go negative through that
        // constructor — but the window is also fed by `observe` directly and an unsigned wrap here
        // would be a bound in the unsafe direction, silently, once per segment.
        let body = sample.acquisition_us.saturating_sub(sample.overhead_us);
        let bytes = u128::from(sample.bytes.max(1));
        let scaled = (u128::from(body) * u128::from(query_bytes) + bytes - 1) / bytes;
        let total = scaled.saturating_add(u128::from(sample.overhead_us));
        total.min(i64::MAX as u128) as i64
    }

    /// The `k`-th largest transferred value — the order statistic `eps` names. A deterministic
    /// property of the window (at most `k-1` samples exceed it), not a coverage bound on the next
    /// acquisition; see [`AdmissionPolicy`].
    ///
    /// `None` when the window is shorter than `n`: a bound from fewer samples than the SLO asks for
    /// does not carry the SLO, and returning a number anyway is how an unearned guarantee ships.
    #[allow(dead_code)]
    pub(crate) fn bound_us(&self, query_bytes: u64, policy: AdmissionPolicy) -> Option<i64> {
        let n = policy.window_len();
        if self.len < n {
            return None;
        }
        let mut values: [i64; WINDOW_CAPACITY] = [0; WINDOW_CAPACITY];
        for (slot, sample) in values.iter_mut().zip(self.recent(n)) {
            *slot = Self::transferred_us(sample, query_bytes);
        }
        let used = &mut values[..n];
        used.sort_unstable();
        let k = (policy.k.max(1) as usize).min(n);
        Some(used[n - k])
    }

    /// Both admission conditions of §4, over the last `n` samples.
    ///
    /// All integer, and every accumulation saturates. Real inputs put the sum around 1e8 us, but
    /// [`Self::transferred_us`] is allowed to saturate at `i64::MAX` on a degenerate observation and
    /// a plain `+` over 64 of those would then be the same host-panic/device-wrap split that
    /// method's doc describes. No unsigned subtraction appears anywhere: the one difference taken
    /// (`transferred - duration`) is `i64` and is allowed to be negative, which is exactly the case
    /// condition (2) discards.
    #[allow(dead_code)]
    pub(crate) fn admits(
        &self,
        query_bytes: u64,
        media_duration_ms: i64,
        reserve_ms: i64,
        policy: AdmissionPolicy,
    ) -> Option<Admission> {
        if media_duration_ms <= 0 {
            return None;
        }
        self.evaluate(reserve_ms, policy, |_| query_bytes)
    }

    /// The conservation certificate for one rendition, using each observation's own media
    /// duration both to form that sample's candidate byte query and to credit its supply.
    #[allow(dead_code)]
    pub(crate) fn admits_candidate(
        &self,
        declared_bps: u64,
        rung: Rung,
        reserve_ms: i64,
        policy: AdmissionPolicy,
    ) -> Option<Admission> {
        if declared_bps == 0 {
            return None;
        }
        self.evaluate(reserve_ms, policy, |sample| {
            candidate_worst_case_bytes(
                declared_bps,
                sample.media_duration_ms,
                rung.size_spread_pm(),
            )
        })
    }

    /// Exact conservation certificate for the complete finite episode already observed on the
    /// CURRENT operating point, even while the statistical ring is filling or after it wraps. Every
    /// sample enters at its actual acquisition cost: projecting a larger query here would turn a
    /// demand-capped response into a false capacity ceiling. This makes no claim about an unseen
    /// acquisition; it says only how much reserve the measured episode needs and whether it
    /// replenished itself.
    pub(crate) fn observed_admission(&self, reserve_ms: i64) -> Option<Admission> {
        self.summary.admission(reserve_ms, false)
    }

    /// Replay the observed current-point order. With
    /// `P_i = sum_{j<i}(A_j-D_j)`, acquisition `i` completes without starvation exactly when the
    /// initial reserve covers `P_i+A_i`; the ordered runway is therefore `max_i(P_i+A_i)`.
    /// [`Self::observed_admission`] remains the adversarial-permutation stress certificate.
    pub(crate) fn observed_ordered_admission(&self, reserve_ms: i64) -> Option<Admission> {
        self.summary.admission(reserve_ms, true)
    }

    pub(crate) fn observed_runway_us(&self) -> Option<i64> {
        self.observed_admission(0)
            .map(|admission| admission.runway_us)
    }

    /// Telemetry for the exact full finite episode that the controller actually uses. The legacy
    /// order-statistic fields stay in the wire shape for compatibility, but are deliberately
    /// unavailable (`eps=0`, `bound=None`): no transferred candidate query decides current-point
    /// conservation any more.
    pub(crate) fn observed_readout(&self, reserve_ms: i64) -> AdmissionReadout {
        AdmissionReadout {
            have: self.summary.n,
            want: self.summary.n,
            effective_epsilon_pm: 0,
            clamped: false,
            bound_us: None,
            admission: self.observed_admission(reserve_ms),
            resets: self.resets,
        }
    }

    #[allow(dead_code)]
    fn evaluate(
        &self,
        reserve_ms: i64,
        policy: AdmissionPolicy,
        query_bytes: impl FnMut(Acquisition) -> u64,
    ) -> Option<Admission> {
        let n = policy.window_len();
        if self.len < n {
            return None;
        }
        Some(self.evaluate_recent(n, reserve_ms, query_bytes))
    }

    fn evaluate_recent(
        &self,
        n: usize,
        reserve_ms: i64,
        mut query_bytes: impl FnMut(Acquisition) -> u64,
    ) -> Admission {
        let mut demand_us: i64 = 0;
        let mut supply_us: i64 = 0;
        let mut excess_us: i64 = 0;
        let mut terminal_us: i64 = 0;
        for sample in self.recent(n) {
            let duration_us = sample.media_duration_ms.saturating_mul(1_000);
            let transferred = Self::transferred_us(sample, query_bytes(sample));
            demand_us = demand_us.saturating_add(transferred);
            supply_us = supply_us.saturating_add(duration_us);
            excess_us = excess_us.saturating_add((transferred - duration_us).max(0));
            terminal_us = terminal_us.max(transferred.min(duration_us));
        }
        // Before acquisition m completes, none of D_m is playable yet.  For an unknown future
        // ordering the exact worst permutation places every positive deficit first, then the
        // acquisition with the largest `min(T,D)` terminal cost.
        let runway_us = excess_us.saturating_add(terminal_us);
        Admission {
            sustainable: demand_us <= supply_us,
            survivable: reserve_ms.saturating_mul(1_000) >= runway_us,
            demand_us,
            supply_us,
            excess_us,
            runway_us,
            samples: n,
        }
    }

    /// Both conditions plus every term they rest on, for telemetry.
    ///
    /// One call, one `policy.window_len()`, so the reported `want`, `bound_us` and `admission`
    /// cannot describe three different window lengths — which they could if the log line assembled
    /// them from three separate calls at the call site.
    #[allow(dead_code)]
    pub(crate) fn readout(
        &self,
        query_bytes: u64,
        media_duration_ms: i64,
        reserve_ms: i64,
        policy: AdmissionPolicy,
    ) -> AdmissionReadout {
        AdmissionReadout {
            have: self.len(),
            want: policy.window_len(),
            effective_epsilon_pm: policy.effective_epsilon_pm(),
            clamped: policy.is_clamped(),
            bound_us: self.bound_us(query_bytes, policy),
            admission: self.admits(query_bytes, media_duration_ms, reserve_ms, policy),
            resets: self.resets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `k = 1, eps = 250pm` gives `n = 3` — short enough to write windows out by hand, and the
    /// shortest length at which "k-th largest" is not the same as "the only element".
    fn policy(epsilon_pm: u32, k: u32) -> AdmissionPolicy {
        AdmissionPolicy { epsilon_pm, k }
    }

    fn window(samples: &[(u64, u64)]) -> AcquisitionWindow {
        let mut w = AcquisitionWindow::default();
        for &(bytes, us) in samples {
            // These fixtures predate the fixed/body split and are about the ORDER STATISTIC,
            // not the cost model: passing `us` for both keeps them all-proportional, which is
            // exactly the behaviour their expected values were computed against.
            w.observe(bytes, us, us, 2_000);
        }
        w
    }

    fn acq(bytes: u64, acquisition_us: u64) -> Acquisition {
        Acquisition {
            bytes,
            acquisition_us,
            media_duration_ms: 2_000,
            overhead_us: 0,
        }
    }

    // ---- the transfer bound: the two tight ends, and the rounding direction ----

    #[test]
    fn an_upshift_query_scales_the_bound_by_the_byte_ratio() {
        // tau <= A_i/b_i, attained at O0 = 0: twice the bytes may cost twice the time and no more.
        assert_eq!(
            AcquisitionWindow::transferred_us(acq(1_000, 100), 2_000),
            200
        );
    }

    #[test]
    fn a_downshift_query_does_not_lower_the_bound() {
        // tau >= 0, attained at tau = 0: fewer bytes may cost the same. Claiming less would be a
        // bound in the unsafe direction, and it is the mistake `max(1, .)` exists to prevent.
        assert_eq!(AcquisitionWindow::transferred_us(acq(1_000, 100), 500), 100);
    }

    #[test]
    fn every_downshift_query_gives_the_same_bound_so_none_needs_a_size_prediction() {
        // The property that lets rungs 320/720/2000 be admitted with no usable `sigma`.
        let bounds: Vec<i64> = [1, 10, 500, 999, 1_000]
            .iter()
            .map(|&q| AcquisitionWindow::transferred_us(acq(1_000, 100), q))
            .collect();
        assert!(bounds.iter().all(|&b| b == 100), "{bounds:?}");
    }

    #[test]
    fn the_transfer_rounds_up_not_down() {
        // Differential against the obvious `a * q / b`: 100 * 3 / 7 = 42 by truncation, 43 by
        // ceiling. A floored safety bound is a bound in the wrong direction, once per segment.
        assert_eq!(
            AcquisitionWindow::transferred_us(acq(7, 100), 3),
            100,
            "downshift is flat"
        );
        assert_eq!(AcquisitionWindow::transferred_us(acq(7, 100), 10), 143);
        assert_eq!(
            100 * 10 / 7,
            142,
            "the truncating form this test is differential against"
        );
    }

    #[test]
    fn a_query_that_would_overflow_i64_saturates_instead_of_panicking() {
        // `overflow-checks` is ON under `cargo test` and OFF in release, so an unchecked product
        // panics here and wraps silently on the television. The i128 intermediate is why neither
        // happens; this pins that it stays.
        let huge = AcquisitionWindow::transferred_us(acq(1, u64::MAX), u64::MAX);
        assert_eq!(huge, i64::MAX);
    }

    // ---- the window itself ----

    #[test]
    fn a_zero_byte_or_zero_time_observation_never_enters_the_window() {
        // `bytes` is a divisor, and a malformed line must not take the demux worker with it.
        let w = window(&[(0, 100), (1_000, 0), (1_000, 100)]);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn recent_returns_the_newest_first_across_a_wraparound() {
        let mut w = AcquisitionWindow::default();
        for i in 1..=(WINDOW_CAPACITY as u64 + 3) {
            w.observe(1_000, i, i, 2_000);
        }
        assert_eq!(
            w.len(),
            WINDOW_CAPACITY,
            "the ring saturates rather than growing"
        );
        let newest: Vec<u64> = w.recent(3).map(|a| a.acquisition_us).collect();
        let n = WINDOW_CAPACITY as u64 + 3;
        assert_eq!(newest, vec![n, n - 1, n - 2]);
    }

    #[test]
    fn an_explicit_reset_empties_the_bag() {
        let mut w = window(&[(1_000, 100), (1_000, 200)]);
        w.reset();
        assert_eq!(w.len(), 0);
        assert!(w.observed_admission(10_000).is_none());
        assert_eq!(w.worst_overhead_us(), 0);
        assert!(w.admits(1_000, 2_000, 10_000, policy(250, 1)).is_none());
    }

    // ---- the policy's two explicit choices ----

    #[test]
    fn the_window_length_is_k_over_eps_minus_one() {
        // R28's correction. The `1/eps - 1` this replaced is only right at k = 1, which is why it
        // survived four reviews.
        assert_eq!(policy(250, 1).window_len(), 3);
        assert_eq!(policy(250, 3).window_len(), 11);
        assert_eq!(policy(100, 1).window_len(), 9);
    }

    #[test]
    fn an_unclamped_window_delivers_exactly_the_epsilon_asked_for() {
        for (eps, k) in [(250, 1), (100, 1), (250, 3), (50, 1)] {
            let p = policy(eps, k);
            assert!(!p.is_clamped(), "eps={eps} k={k}");
            assert_eq!(p.effective_epsilon_pm(), eps, "eps={eps} k={k}");
        }
    }

    #[test]
    fn a_clamped_window_says_so_and_reports_the_weaker_guarantee_it_actually_offers() {
        // The failure this prevents: asking for eps = 1pm, silently getting a 64-long window, and
        // reading `k/(n+1) = 15pm` as if it were 1pm. The storage bound is not a policy choice and
        // must not be able to masquerade as one.
        let p = policy(1, 1);
        assert!(p.is_clamped());
        assert_eq!(p.window_len(), WINDOW_CAPACITY);
        // `1/65 = 15.38 pm`, so the honest CEILING is 16. This assertion used to recompute the
        // implementation's own floored division and therefore agreed with it by construction —
        // it could not have caught the direction error it exists to guard, and it asserted 15,
        // a guarantee stronger than the window delivers. Stated as a value now, from the
        // arithmetic rather than from the code.
        assert_eq!(
            p.effective_epsilon_pm(),
            16,
            "1/65 = 15.38pm, and a ceiling rounds UP"
        );
        assert!(
            p.effective_epsilon_pm() > 1,
            "a clamp can only WEAKEN the guarantee"
        );
    }

    // ---- the order statistic ----

    #[test]
    fn the_bound_is_the_kth_largest_and_at_most_k_minus_one_samples_exceed_it() {
        // The defining property of the k-th order statistic, and the whole basis of the eps claim.
        // An off-by-one in the index is invisible to a value assertion on a symmetric window and
        // fatal to the guarantee; this catches it at every k.
        let acqs: Vec<(u64, u64)> = [90u64, 10, 70, 30, 50, 20, 80, 40, 60, 100, 15]
            .iter()
            .map(|&v| (1_000, v))
            .collect();
        for k in 1..=3u32 {
            let p = policy(1_000 * k / 11, k);
            let w = window(&acqs);
            let n = p.window_len();
            let bound = w.bound_us(1_000, p).expect("window is full");
            let over = w
                .recent(n)
                .filter(|s| s.acquisition_us as i64 > bound)
                .count();
            assert!(over < k as usize, "k={k} n={n} bound={bound} over={over}");
        }
    }

    #[test]
    fn a_window_shorter_than_n_yields_no_bound_and_no_verdict() {
        // A bound from fewer samples than the SLO asks for does not carry the SLO. Returning a
        // number anyway is precisely how an unearned guarantee ships.
        let w = window(&[(1_000, 100), (1_000, 200)]);
        let p = policy(250, 1);
        assert_eq!(w.len(), 2);
        assert_eq!(p.window_len(), 3);
        assert!(w.bound_us(1_000, p).is_none());
        assert!(w.admits(1_000, 2_000, 10_000, p).is_none());
        assert_eq!(w.readout(1_000, 2_000, 10_000, p).admitted(), None);
    }

    // ---- condition (1): sustainability, which has no margin in it ----

    #[test]
    fn a_window_summing_to_exactly_n_times_d_is_sustainable_and_one_microsecond_more_is_not() {
        // The boundary is the whole point: `sum A <= nD` IS "this rung does not drain the buffer",
        // so any slack here would be an unexplained safety margin of exactly the kind the design
        // rule forbids. A 2000 ms segment, three samples, 6 000 000 us of supply.
        let p = policy(250, 1);
        let at = window(&[(1_000, 2_000_000), (1_000, 2_000_000), (1_000, 2_000_000)]);
        let a = at.admits(1_000, 2_000, 10_000, p).unwrap();
        assert_eq!((a.demand_us, a.supply_us), (6_000_000, 6_000_000));
        assert!(a.sustainable);

        let over = window(&[(1_000, 2_000_000), (1_000, 2_000_000), (1_000, 2_000_001)]);
        assert!(!over.admits(1_000, 2_000, 10_000, p).unwrap().sustainable);
    }

    // ---- condition (2): the reserve against the worst ordering ----

    #[test]
    fn the_excess_sums_every_overrun_rather_than_taking_the_largest_one() {
        // Differential against "max drawdown". This function's stated replay contract discards
        // observed order and asks for the worst permutation -- every hard segment consecutive.
        // It is deliberately robust retrospective arithmetic, not an exchangeability claim or a
        // forecast. Two segments 500 ms over each therefore contribute 1000 ms, not 500 ms.
        let p = policy(250, 1);
        let w = window(&[(1_000, 2_500_000), (1_000, 2_500_000), (1_000, 1_000_000)]);
        let a = w.admits(1_000, 2_000, 10_000, p).unwrap();
        assert_eq!(a.excess_us, 1_000_000);
    }

    #[test]
    fn a_reserve_exactly_covering_the_runway_survives_and_one_millisecond_less_does_not() {
        let p = policy(250, 1);
        let w = window(&[(1_000, 2_500_000), (1_000, 2_500_000), (1_000, 1_000_000)]);
        let a = w.admits(1_000, 2_000, 3_000, p).unwrap();
        assert_eq!((a.excess_us, a.runway_us), (1_000_000, 3_000_000));
        assert!(a.survivable);
        assert!(!w.admits(1_000, 2_000, 2_999, p).unwrap().survivable);
    }

    #[test]
    fn a_realtime_acquisition_still_needs_runway_until_its_media_is_credited() {
        let p = policy(250, 1);
        let w = window(&[(1_000, 2_000_000); 3]);
        let a = w.admits(1_000, 2_000, 2_000, p).unwrap();
        assert_eq!(a.excess_us, 0, "the long-run drift is exactly flat");
        assert_eq!(
            a.runway_us, 2_000_000,
            "the first acquisition must still finish"
        );
        assert!(a.survivable);
        assert!(!w.admits(1_000, 2_000, 1_999, p).unwrap().survivable);
    }

    #[test]
    fn startup_runway_uses_the_finite_bag_before_the_admission_window_is_full() {
        let p = policy(50, 1); // n=19; this bag deliberately has only two observations.
        let mut w = AcquisitionWindow::default();
        // Huge byte counts keep the candidate query below each observed object, so T is exactly
        // the observed acquisition: 3s and 1s for two 2s media credits.
        w.observe(10_000_000, 3_000_000, 3_000_000, 2_000);
        w.observe(10_000_000, 1_000_000, 1_000_000, 2_000);
        assert_eq!(
            w.observed_runway_us(),
            Some(3_000_000),
            "1s accumulated excess + 2s terminal credit boundary",
        );
        assert!(w
            .admits_candidate(8_000_000, Rung::P1080, 10_000, p)
            .is_none());
    }

    #[test]
    fn worst_permutation_replay_uses_every_observation_still_in_the_finite_bag() {
        let p = policy(50, 1); // the old statistical window is 19 samples
        assert_eq!(p.window_len(), 19);
        let mut w = AcquisitionWindow::default();
        // Put the expensive acquisition just outside that statistical suffix, but keep it inside
        // the finite ring. The full-bag replay readout may not forget it merely because epsilon
        // chose a different evidence length for a retired order-statistic diagnostic.
        w.observe(1_000, 5_000_000, 5_000_000, 2_000);
        for _ in 0..19 {
            w.observe(1_000, 2_000_000, 2_000_000, 2_000);
        }
        assert_eq!(
            w.observed_runway_us(),
            Some(5_000_000),
            "3s accumulated deficit plus the 2s completion boundary",
        );
    }

    #[test]
    fn live_episode_admission_retains_a_costly_first_acquisition_after_the_ring_wraps() {
        let mut w = AcquisitionWindow::default();
        // The first response costs 5 s for 2 s of media, including 4 s of fixed setup. Sixty-four
        // cheap responses then wrap the diagnostic ring and used to erase every trace of that cost
        // from the LIVE stress runway, reopening an upshift with only 1 s of rollback reserve.
        w.observe(1_000, 5_000_000, 1_000_000, 2_000);
        for _ in 0..WINDOW_CAPACITY {
            w.observe(1_000, 1_000_000, 1_000_000, 2_000);
        }

        assert_eq!(
            w.len(),
            WINDOW_CAPACITY,
            "the ordered-statistic ring stays bounded"
        );
        let admission = w
            .observed_admission(i64::MAX / 1_000)
            .expect("non-empty episode");
        assert_eq!(admission.samples, WINDOW_CAPACITY + 1);
        assert_eq!(
            (admission.demand_us, admission.supply_us),
            (69_000_000, 130_000_000)
        );
        assert_eq!(admission.excess_us, 3_000_000);
        assert_eq!(admission.runway_us, 5_000_000);
        assert_eq!(
            (
                w.observed_readout(5_000).have,
                w.observed_readout(5_000).want
            ),
            (WINDOW_CAPACITY + 1, WINDOW_CAPACITY + 1),
            "live telemetry reports the episode count, not the ring occupancy",
        );
        assert_eq!(
            w.readout(1_000, 2_000, 5_000, policy(250, 1)).have,
            WINDOW_CAPACITY,
            "retired ordered statistics still report only their bounded ring",
        );
        assert_eq!(
            w.observed_ordered_admission(i64::MAX / 1_000)
                .unwrap()
                .runway_us,
            5_000_000,
            "the chronological prefix certificate is episode-long too",
        );
        assert_eq!(w.worst_overhead_us(), 4_000_000);
    }

    #[test]
    fn finite_episode_summaries_compose_associatively_with_every_admission_term() {
        let summary = |acquisition_us, media_duration_ms, overhead_us| {
            AdmissionSummary::from_sample(Acquisition {
                bytes: 1,
                acquisition_us,
                media_duration_ms,
                overhead_us,
            })
        };
        let a = summary(3_000_000, 2_000, 1_000_000);
        let b = summary(1_000_000, 2_000, 200_000);
        let c = summary(4_000_000, 3_000, 2_000_000);

        let left_grouped = a.combine(b).combine(c);
        let right_grouped = a.combine(b.combine(c));
        assert_eq!(left_grouped, right_grouped);
        assert_eq!(left_grouped.n, 3);
        assert_eq!(left_grouped.sum_acquisition_us, 8_000_000);
        assert_eq!(left_grouped.sum_duration_us, 7_000_000);
        assert_eq!(left_grouped.delta_us, 1_000_000);
        assert_eq!(left_grouped.max_prefix_runway_us, 4_000_000);
        assert_eq!(left_grouped.positive_slack_sum_us, 2_000_000);
        assert_eq!(left_grouped.max_capped_delivery_us, 3_000_000);
        assert_eq!(left_grouped.sample_overhead_max_us, 2_000_000);
        assert_eq!(left_grouped.stress_runway_us(), Some(5_000_000));
    }

    #[test]
    fn overflowing_equal_sums_cannot_silently_admit_an_upgrade() {
        let mut w = AcquisitionWindow::default();
        // Each sample is exactly real-time and individually representable. Their equal totals are
        // not: independently saturating both sums at i64::MAX makes `demand <= supply` true and
        // would admit with this reserve. The checked episode summary must poison that verdict.
        let duration_ms = 4_700_000_000_000_000_i64;
        let acquisition_us = u64::try_from(duration_ms.checked_mul(1_000).unwrap()).unwrap();
        for _ in 0..2 {
            w.observe(1, acquisition_us, acquisition_us, duration_ms);
        }

        assert!(w.summary.overflowed);
        let stress = w.observed_admission(duration_ms).unwrap();
        let ordered = w.observed_ordered_admission(duration_ms).unwrap();
        assert!(!stress.admitted(), "overflow is not an upgrade certificate");
        assert!(
            !ordered.admitted(),
            "overflow is not a stay certificate either"
        );
        assert!(!stress.sustainable && !stress.survivable);
    }

    #[test]
    fn worst_permutation_replay_can_grow_while_the_observed_order_is_sustainable() {
        let mut w = AcquisitionWindow::default();
        for _ in 0..4 {
            w.observe(1_000, 2_500_000, 2_500_000, 2_000);
            w.observe(1_000, 500_000, 500_000, 2_000);
        }
        let admission = w
            .observed_admission(i64::MAX / 1_000)
            .expect("non-empty bag");
        assert!(
            admission.sustainable,
            "each pair acquires 3s and credits 4s"
        );
        assert_eq!(
            admission.runway_us, 4_000_000,
            "the replay deliberately groups four 500ms deficits before a 2s terminal cost",
        );
        // The observed alternating order itself needs only 2.5s. This difference is the reason
        // R_s is a stress certificate and must never be a static mid-acquisition pause arm.
    }

    #[test]
    fn every_observation_contributes_its_own_media_duration() {
        let p = policy(250, 1);
        let mut w = AcquisitionWindow::default();
        w.observe(1_000, 2_000_000, 2_000_000, 1_000);
        w.observe(1_000, 2_000_000, 2_000_000, 2_000);
        w.observe(1_000, 3_000_000, 3_000_000, 3_000);
        let a = w.admits(1_000, 99_000, 10_000, p).unwrap();
        assert_eq!(
            a.supply_us, 6_000_000,
            "1s + 2s + 3s, not n times the caller's D"
        );
        assert_eq!(a.demand_us, 7_000_000);
        assert!(!a.sustainable);
    }

    #[test]
    fn a_rung_can_be_sustainable_on_average_and_still_unsurvivable() {
        // The reason the two conditions are separate fields rather than one boolean: this is
        // exactly the state a single `4/5` haircut cannot express. Mean load is under 1, but one
        // segment overruns by more than the reserve holds.
        let p = policy(250, 1);
        let w = window(&[(1_000, 5_000_000), (1_000, 500_000), (1_000, 400_000)]);
        let a = w.admits(1_000, 2_000, 2_000, p).unwrap();
        assert!(a.demand_us < a.supply_us, "sustainable on the average");
        assert!(a.sustainable);
        assert!(
            !a.survivable,
            "but one 5 s segment against a 2 s reserve is not survivable"
        );
        assert!(!a.admitted());
    }

    // ---- the two composed, and the transfer between rungs ----

    #[test]
    fn an_upshift_query_can_only_make_the_verdict_stricter() {
        // Pointwise domination: every transfer factor is >= 1, so a larger query raises every
        // transferred value. This is why the guarantee is an inequality rather than an equality,
        // and it is the property that makes the rule safe under non-exchangeability.
        let p = policy(250, 1);
        let w = window(&[(1_000, 1_000_000), (1_000, 1_200_000), (1_000, 900_000)]);
        let here = w.admits(1_000, 2_000, 10_000, p).unwrap();
        for q in [1_001u64, 1_500, 2_000, 8_000] {
            let there = w.admits(q, 2_000, 10_000, p).unwrap();
            assert!(there.demand_us >= here.demand_us, "q={q}");
            assert!(there.excess_us >= here.excess_us, "q={q}");
            assert!(
                here.admitted() || !there.admitted(),
                "q={q}: an upshift cannot become easier"
            );
        }
    }

    #[test]
    fn the_retired_transfer_form_can_evaluate_a_cross_size_counterfactual() {
        // Historical/offline property only: the transferred-byte formula can ask how the same bag
        // prices a larger query. The live controller resets at an actuator commit and never uses
        // this result as new-rung evidence.
        let p = policy(250, 1);
        let w = window(&[(500, 500_000), (500, 500_000), (2_000, 1_900_000)]);
        let a = w.admits(2_000, 2_000, 10_000, p).unwrap();
        assert_eq!(a.samples, 3);
        // 500 B at 500 ms transfers to 2000 B as 2 000 000 us, twice; plus the 1 900 000 us
        // observed at that size. 5.9 s of demand against 6 s of supply: admitted, barely.
        assert_eq!(a.demand_us, 5_900_000);
        assert!(a.sustainable);
    }

    #[test]
    fn the_retired_order_stat_readout_reports_filling_rather_than_refusal() {
        // `have < want` belongs to the retired fixed-length readout. It remains parseable for the
        // historical corpus and must not masquerade as a physical refusal.
        let p = policy(250, 1);
        let r = window(&[(1_000, 100)]).readout(1_000, 2_000, 10_000, p);
        assert_eq!((r.have, r.want), (1, 3));
        assert_eq!(r.admitted(), None);
        assert!(r.bound_us.is_none() && r.admission.is_none());
        assert!(!r.clamped);
        assert_eq!(r.effective_epsilon_pm, 250);
    }

    // ---- the wire form, which `tests/run.py` parses ----
    //
    // **These two literals are one half of a contract with `RE_ABR_WINDOW` in `tests/run.py`.**
    // `test_harness.py::TheAbrWindowLineMatchesTheHarnessRegex` reads them straight out of this
    // file and matches them against that regex, so a field renamed here fails the Python suite and
    // a regex edited there fails against these. Nothing else connects the two sides: the app writes
    // this line on a television and the harness parses it on a Mac, and until this contract existed
    // a drift between them was a silent "no samples" — which reads exactly like a total regression.

    /// Canonical retired order-stat verdict. Read by `tests/test_harness.py`; keep the marker.
    const WIRE_ADMIT: &str = // wire-example
        "abr: window current=4000kbps verdict=admit have=3/3 eps=250pm clamp=0 bound=1000ms \
         demand=2600ms supply=6000ms excess=0ms runway=1000ms sus=1 sur=1 reset=0 bytes=1000 dur=2000ms";

    /// Canonical retired filling verdict — every uncomputed number is `-1`, never `0`.
    const WIRE_FILLING: &str = // wire-example
        "abr: window current=720kbps verdict=filling have=1/3 eps=250pm clamp=0 bound=-1ms \
         demand=-1ms supply=-1ms excess=-1ms runway=-1ms sus=0 sur=0 reset=2 bytes=500 dur=2000ms";

    /// Canonical live exact finite-bag verdict.
    const WIRE_LIVE_EXACT: &str = // wire-example
        "abr: window current=4000kbps verdict=admit have=1/1 eps=0pm clamp=0 bound=-1ms \
         demand=1000ms supply=2000ms excess=0ms runway=1000ms sus=1 sur=1 reset=0 bytes=1000 dur=2000ms";

    #[test]
    fn the_logged_line_is_the_one_the_harness_regex_was_written_against() {
        let p = policy(250, 1);
        let w = window(&[(1_000, 800_000), (1_000, 800_000), (1_000, 1_000_000)]);
        assert_eq!(
            w.readout(1_000, 2_000, 10_000, p)
                .log_line(4_000, 1_000, 2_000),
            WIRE_ADMIT
        );
    }

    #[test]
    fn the_live_exact_line_is_pinned_beside_the_harness_regex() {
        let w = window(&[(1_000, 1_000_000)]);
        assert_eq!(
            w.observed_readout(1_000).log_line(4_000, 1_000, 2_000),
            WIRE_LIVE_EXACT,
        );
    }

    #[test]
    fn a_filling_window_logs_minus_one_for_every_uncomputed_term_and_never_zero() {
        // A zero `excess` is a perfectly ordinary HEALTHY verdict, so printing zero here would make
        // "not computed" indistinguishable from "nothing to absorb" in every startup trace.
        //
        // Two resets first, so the example carries a non-zero `reset=`: a wire example that only
        // ever shows the initial value cannot catch a counter that is cleared by the very call it
        // is meant to count.
        let p = policy(250, 1);
        let mut w = window(&[(500, 400_000), (500, 400_000)]);
        w.reset();
        w.reset();
        w.observe(500, 400_000, 400_000, 2_000);
        assert_eq!(
            w.readout(500, 2_000, 10_000, p).log_line(720, 500, 2_000),
            WIRE_FILLING
        );
    }

    #[test]
    fn a_reset_counts_itself_and_the_count_survives_the_reset_it_records() {
        // `*self = Self::default()` is the obvious body and it erases its own evidence -- after
        // which a `have` dropping to 1 has nothing in the trace to attribute it to.
        let mut w = window(&[(1_000, 100), (1_000, 200)]);
        assert_eq!(w.readout(1_000, 2_000, 0, policy(250, 1)).resets, 0);
        w.reset();
        w.reset();
        w.observe(1_000, 300, 300, 2_000);
        let r = w.readout(1_000, 2_000, 0, policy(250, 1));
        assert_eq!(
            (r.resets, r.have),
            (2, 1),
            "the history is gone, the record of it is not"
        );
    }

    #[test]
    fn the_readouts_terms_all_come_from_one_window_length() {
        // Guards the reason `readout` exists at all: assembled at a call site from three separate
        // calls, `want`, `bound_us` and `admission` could describe three different lengths.
        let p = policy(250, 3);
        let w = window(
            &(1..=20)
                .map(|i| (1_000u64, i * 100_000))
                .collect::<Vec<_>>(),
        );
        let r = w.readout(1_000, 2_000, 10_000, p);
        assert_eq!(r.want, 11);
        assert_eq!(r.admission.unwrap().samples, 11);
        assert_eq!(r.bound_us, w.bound_us(1_000, p));
        // The newest 11 are 1.0 s .. 2.0 s, summing to 16.5 s against 11 x 2 s of supply. Asserting
        // the SUM and not just the verdict is what pins that `recent` took the newest eleven: the
        // oldest eleven (0.1 s .. 1.1 s) sum to 6.6 s and reach the same verdict.
        assert_eq!(r.admission.unwrap().demand_us, 16_500_000);
        assert_eq!(r.admitted(), Some(true));
    }
}
