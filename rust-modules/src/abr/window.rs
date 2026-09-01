//! **The acquisition window, and the admission rule built on it.** `docs/adaptive-playback-spec.md`
//! §2a and §4.
//!
//! This module replaces a chain of estimates — a throughput EWMA, an uncertainty discount, a
//! production fold, a `4/5` haircut and a bare `800` — with two sums over the last `n` segments.
//! Nothing here is fitted and nothing here is a float.
//!
//! # What it computes
//!
//! Given a window of observed `(bytes, acquisition)` pairs and a candidate's worst-case byte count
//! `q`, the **transfer bound** says what that candidate's acquisition can cost:
//!
//! ```text
//! T_i(q) = A_i * max(1, q / b_i)
//! ```
//!
//! Both halves of that are tight. `O0 >= 0` gives `tau <= A_i/b_i`, so an upshift costs at most
//! `A_i * q/b_i`; `tau >= 0` gives `A_j <= A_i` for a downshift. So `T` is the exact worst case
//! over every split of `A_i` between the two coefficients of `A = O0 + bytes*tau` — which is
//! precisely the split this project's corpus was shown unable to identify (R7). **No estimator for
//! `O0` or `tau` exists here because none is needed.**
//!
//! One asymmetry decides how much of the ladder needs a size prediction at all, and it is worth
//! stating precisely because the loose version of it is wrong. `T_i(q) = A_i` exactly when
//! `q <= b_i` — so the transfer is free of `q` not for "a downshift" in general but **whenever the
//! candidate's worst case is under what the sample actually weighed**:
//!
//! ```text
//! sigma_j * W_j * D / 8000  <=  b_i
//! ```
//!
//! That is not independent of `sigma_j`; it is INSENSITIVE to it, which is a weaker claim and the
//! true one. `W_j` is a cap (§3), so a real downshift moves the rate by 1.1x to 60x, and the
//! condition then holds for any `sigma_j` up to `8000*b_i/(W_j*D)` — a threshold in the tens at the
//! bottom of the ladder against measured spreads under 1. **This is why the three rungs where
//! `sigma` has no usable ceiling (320, 720, 2000) are still admissible: they are downshift targets,
//! and the margin there is orders of magnitude, not a fitted number.** An earlier draft of this
//! paragraph said `T = A_i` held "whatever the candidate turns out to weigh", which drops the
//! condition entirely and would license an UPSHIFT on the same reasoning.
//!
//! Nothing in the code depends on which case applies — [`AcquisitionWindow::transferred_us`] takes
//! `q` and is correct either way. The distinction decides only where a good `sigma` is needed, and
//! therefore which candidates the rule can price today.
//!
//! # What `eps` is, and what it is NOT — downgraded 2026-08-29
//!
//! This section used to open with a probability:
//!
//! ```text
//! P( A_next > k-th largest of { T_i(q) } )  <=  k/(n+1)
//! ```
//!
//! and argue it via domination: the transferred values are not exchangeable (the map `g_q` is
//! indexed by the query), but the RAW order statistic is "the identity map, genuinely fixed", and
//! every transfer factor is `>= 1`, so the transferred bound dominates the raw one pointwise and
//! the exceedance probability can only fall.
//!
//! **The domination step is real; the raw bound it dominates is not available.** The quantity being
//! bounded is the cost of the CANDIDATE at query bytes `q`. An upshift — the only direction this
//! rule gates — has `q > b_i`, so `A_next` is the cost of a strictly larger transfer than any
//! sample in the window: stochastically larger, not exchangeable with the raw `A_i`, and the raw
//! inequality fails in exactly the direction that matters. Dominating a bound that does not hold
//! proves nothing. The sound repair routes through counterfactual same-size costs
//! `A_i(q) = O0_i + q*tau_i` — but that needs the affine model with per-segment coefficients
//! identically distributed across the window and the next draw, which is the same-link precondition
//! this project's own corpus refutes on 36.6% of pairs.
//!
//! Two further reasons the coverage reading is unavailable, either sufficient alone: the controller
//! INVOKES this rule only at moments selected by the same recent data (dwell expired, reserve above
//! the gate, not draining, no reject block), and order-statistic coverage is marginal rather than
//! conditional; and the window's own contents are shaped by the collapse reset, which guarantees an
//! evaluated window holds only post-collapse samples. [`AdmissionPolicy`] has the full account.
//!
//! **What the rule delivers instead**, and it is enough: a DETERMINISTIC property — at most `k-1` of
//! the last `n` transferred values exceed the bound — plus conditions (1) and (2) as deterministic
//! statements about the last `n*D` of media under the worst-case transfer. `eps` is the design ratio
//! `k/(n+1)`, chosen for the window length it implies. Nothing in the decision path reads it as a
//! probability: `bound_us` is telemetry with no consumer outside the read-out, and `admits` consumes
//! only `n`.
//!
//! The empirical record stays worth having and stays EMPIRICAL: the raw control
//! (`tools/abr-transfer-bound.py`, `RAW ctrl`) lands at nominal on the stationary device corpus
//! while the transferred column sits 2-4x under, and about 2x OVER on swept legs. An earlier draft
//! read the under-shoot as evidence that exchangeability holds; it is not, being forced by the
//! domination. A ratio realized 2-4x off in either direction is a design dial, not a coverage
//! guarantee.
//!
//! # The two admission conditions
//!
//! ```text
//! (1) sum_i T_i(q)  <=  n * D                 sustainability, on the AVERAGE
//! (2) B  >=  sum_i ( T_i(q) - D )+            the reserve covers the peak excursion
//! ```
//!
//! (1) is exact and has no margin in it: the reserve moves by `D - A` per segment, so `sum A <= nD`
//! is precisely "this rung does not drain the buffer over the window". (2) sums every excess rather
//! than taking the observed maximum drawdown because under exchangeability the ORDER of the window
//! carries no information, so the worst ordering — every hard segment consecutive — is the only one
//! that may be assumed.
//!
//! **(2) proves survival for the span of the evidence, `n*D` of media, and not one segment further.**
//! That is sound only because the controller re-evaluates every segment, and it is why `n` is
//! load-bearing rather than free.
//!
//! # Why there is no step-size cap
//!
//! A large jump is not forbidden, it is PRICED, and the price is measured. `E_tx_up` grows roughly
//! linearly in the jump ratio — about 5 s per unit of ratio on the device corpus: 4.4 s at 1.14x,
//! 8.0 s at 2x, 12.5 s at 3x, 22.2 s at 5x, 63.4 s at 8x. The decision arm charges it by re-running
//! (2) against `B - E_tx_up`. Since `B_max` falls as `1/R`, a big jump at a high rung is
//! unaffordable automatically. A per-decision rung cap would be exactly the unexplained
//! rung-walking rule the design directive forbids, and it would reintroduce the encoder churn that
//! jumping straight to the best candidate exists to avoid.
//!
//! **That charge is not in this module yet, deliberately.** It needs a candidate's byte count
//! (`sigma * W_j * D / 8000`), and `sigma` has a usable ceiling only at rungs >= 4000; both arrive
//! with the decision. Writing the method here ahead of its one caller would have been an
//! unexercised branch in the file whose entire purpose is that every branch is proven.

/// **Storage bound on the window, not a policy choice.** The window length that decides behaviour
/// is `n = k/eps - 1` (see [`AdmissionPolicy`]); this is only how many samples the ring can hold,
/// and it is an implementation limit stated as one.
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
}

/// **The two numbers that decide `n`, both explicit choices under the classification rule.**
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
/// 3. **The sample is shaped.** The collapse reset guarantees an evaluated window holds only
///    post-collapse samples (survivorship, in the anti-conservative direction), and the window
///    survives a pause while `on_resume` demotes the ESTIMATE — pre-pause acquisitions keep
///    informing the bound after the estimator has retracted its confidence in that era.
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
    /// `sum T_i <= n*D`.
    pub(crate) sustainable: bool,
    /// `B >= sum (T_i - D)+`.
    pub(crate) survivable: bool,
    /// `sum T_i`, microseconds.
    pub(crate) demand_us: i64,
    /// `n * D`, microseconds — what (1) compares against.
    pub(crate) supply_us: i64,
    /// `sum (T_i - D)+`, microseconds — what (2) needs the reserve to cover.
    pub(crate) excess_us: i64,
    /// Samples the verdict rests on.
    pub(crate) samples: usize,
}

impl Admission {
    pub(crate) fn admitted(self) -> bool {
        self.sustainable && self.survivable
    }
}

/// **Everything the §4 rule concluded, in one struct, for one event-log line.**
///
/// Assembled inside this module so the numbers logged are the numbers computed, and so that a
/// clamped window or a short one cannot be read as a verdict. It exists because the rule's claim is
/// that it tracks the same segments the shipped estimators see, and that claim is only testable if
/// every term is on the wire — which is how it was graded before it was allowed to decide anything
/// (`docs/measurements/j3a-window-shadow.md`).
///
/// `have < n` is the ordinary state for the first `n` segments of a playback and is reported as
/// such, with `admission: None`. It is not a failure and must not read as one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AdmissionReadout {
    /// Samples the window actually holds.
    pub(crate) have: usize,
    /// `n = k/eps - 1`, the length the SLO asks for.
    pub(crate) want: usize,
    /// `k/(n+1)` at the length actually used, per-mille. Equals the requested eps unless clamped.
    pub(crate) effective_epsilon_pm: u32,
    /// The requested `n` exceeded [`WINDOW_CAPACITY`], so the guarantee on offer is weaker than
    /// the one asked for. Reported rather than silently absorbed.
    pub(crate) clamped: bool,
    /// The k-th largest transferred acquisition, microseconds. `None` while `have < want`.
    pub(crate) bound_us: Option<i64>,
    /// Both conditions. `None` while `have < want`.
    pub(crate) admission: Option<Admission>,
    /// Cumulative [`AcquisitionWindow::reset`] count. Monotone over a playback.
    pub(crate) resets: u32,
}

impl AdmissionReadout {
    /// `Some(true)`/`Some(false)` once the window is long enough; `None` while it is filling.
    pub(crate) fn admitted(self) -> Option<bool> {
        self.admission.map(Admission::admitted)
    }

    /// **The event-log line, formatted here so the shape is testable beside the arithmetic.**
    ///
    /// A line of its own rather than fields appended to `abr: sample`, for two reasons. The shipped
    /// line is a parsed compatibility surface (`RE_ABR_SAMPLE` in `tests/run.py`) and the whole
    /// value of this increment is that it is *comparable* against an unmodified baseline — the same
    /// corpus has to be readable by the harness that graded the estimators this rule is meant to
    /// replace. And the two lines answer different questions: `abr: sample` says what happened,
    /// this says what a rule nobody is listening to would have concluded about it.
    ///
    /// * `have`/`want` — samples held against `n = k/eps - 1`. `have < want` is the ordinary state
    ///   for the first `n` segments and prints `verdict=filling`, not a failure. `have` keeps
    ///   climbing past `want` to [`WINDOW_CAPACITY`] — only the newest `want` are used, so the
    ///   excess is not evidence being ignored but a reading of how long the window has gone
    ///   without a reset, which is the context a verdict after a regime change has to be read in.
    /// * `eps` — `k/(n+1)` at the length actually USED. It differs from the requested eps exactly
    ///   when `clamp=1`, which is the only way the guarantee offered is weaker than the one asked.
    /// * `bound` — the k-th largest transferred acquisition, milliseconds: the order statistic the design
    ///   ratio names, and a bound on
    ///   what the next one costs.
    /// * `demand`/`supply` — condition (1), `sum T_i` against `n*D`, both in milliseconds so the
    ///   comparison is readable without arithmetic.
    /// * `excess` — condition (2)'s `sum (T_i - D)+`, the reserve the worst ordering of this window
    ///   would consume. Graded against `buf` on the `abr: sample` line of the same segment.
    ///
    /// Every unavailable number prints `-1` rather than `0`: while the window is filling those
    /// quantities are NOT COMPUTED, and a zero cannot say the difference — a zero `excess` is a
    /// perfectly ordinary healthy verdict.
    ///
    /// `reset` is cumulative and monotone. Without it a regime-change reset shows up only as
    /// `have` dropping back to 1 with nothing saying why, which is indistinguishable in a captured
    /// trace from the window having lost its history for some other reason.
    ///
    /// The `bytes` field is the query, which for this shadow is the segment's OWN size — so the
    /// line records the current rung's sustainability, the one admission question needing no size
    /// prediction and therefore no `sigma`.
    pub(crate) fn log_line(self, current_kbps: u32, bytes: u64, media_duration_ms: u32) -> String {
        let verdict = match self.admitted() {
            None => "filling",
            Some(true) => "admit",
            Some(false) => "refuse",
        };
        let ms = |us: i64| us / 1_000;
        let (demand, supply, excess) = self
            .admission
            .map(|a| (ms(a.demand_us), ms(a.supply_us), ms(a.excess_us)))
            .unwrap_or((-1, -1, -1));
        format!(
            "abr: window current={current_kbps}kbps verdict={verdict} have={}/{} eps={}pm \
             clamp={} bound={}ms demand={demand}ms supply={supply}ms excess={excess}ms \
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

/// **The bytes a candidate could demand for one segment, worst case** — the admission rule's query.
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
/// A zero or missing declared rate returns 0, and **every caller must treat that as a refusal**: a
/// zero query makes every transfer factor 1, which is the most PERMISSIVE the rule can be. That is
/// the one input where "unknown" and "free" would look the same.
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

/// A ring of recent acquisitions. One per fetched segment, oldest evicted.
///
/// **The window is NOT reset on a rung commit**, and that is the property that makes the transfer
/// form worth having: `T` transfers by BYTES, so a sample taken at the old rung is still evidence
/// about the new one. It IS reset on a link regime change ([`Self::reset`]), because there the
/// history describes a link that no longer exists — measured, and the failure it causes is real:
/// on a deliberately swept link the bound runs about 2x anti-conservative.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AcquisitionWindow {
    ring: [Acquisition; WINDOW_CAPACITY],
    len: usize,
    next: usize,
    /// How many times [`Self::reset`] has run, for the whole playback.
    ///
    /// **It exists because a reset is otherwise invisible in the trace.** `have` simply drops back
    /// to 1, with nothing saying why, and a reader — or a grader replaying the segment stream —
    /// cannot tell a legitimate regime-change reset from the window having lost its history for
    /// some other reason. Monotone, so a drop in `have` WITHOUT this moving is a real defect and
    /// still reads as one.
    resets: u32,
}

impl Default for AcquisitionWindow {
    fn default() -> Self {
        Self {
            ring: [Acquisition::default(); WINDOW_CAPACITY],
            len: 0,
            next: 0,
            resets: 0,
        }
    }
}

impl AcquisitionWindow {
    pub(crate) fn observe(&mut self, bytes: u64, acquisition_us: u64) {
        if bytes == 0 || acquisition_us == 0 {
            // A malformed observation must not enter the window: `bytes` is a divisor.
            return;
        }
        self.ring[self.next] = Acquisition {
            bytes,
            acquisition_us,
        };
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

    /// Most recent first.
    fn recent(&self, want: usize) -> impl Iterator<Item = Acquisition> + '_ {
        let take = want.min(self.len);
        (0..take).map(move |back| {
            let idx = (self.next + WINDOW_CAPACITY - 1 - back) % WINDOW_CAPACITY;
            self.ring[idx]
        })
    }

    /// `A_i * max(1, q / b_i)`, in microseconds, rounded UP.
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
        let bytes = u128::from(sample.bytes.max(1));
        let scaled =
            (u128::from(sample.acquisition_us) * u128::from(query_bytes) + bytes - 1) / bytes;
        scaled.min(i64::MAX as u128) as i64
    }

    /// The `k`-th largest transferred value — the order statistic `eps` names. A deterministic
    /// property of the window (at most `k-1` samples exceed it), not a coverage bound on the next
    /// acquisition; see [`AdmissionPolicy`].
    ///
    /// `None` when the window is shorter than `n`: a bound from fewer samples than the SLO asks for
    /// does not carry the SLO, and returning a number anyway is how an unearned guarantee ships.
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
    pub(crate) fn admits(
        &self,
        query_bytes: u64,
        media_duration_ms: i64,
        reserve_ms: i64,
        policy: AdmissionPolicy,
    ) -> Option<Admission> {
        let n = policy.window_len();
        if self.len < n || media_duration_ms <= 0 {
            return None;
        }
        let duration_us = media_duration_ms.saturating_mul(1_000);
        let mut demand_us: i64 = 0;
        let mut excess_us: i64 = 0;
        for sample in self.recent(n) {
            let transferred = Self::transferred_us(sample, query_bytes);
            demand_us = demand_us.saturating_add(transferred);
            excess_us = excess_us.saturating_add((transferred - duration_us).max(0));
        }
        let supply_us = duration_us.saturating_mul(n as i64);
        Some(Admission {
            sustainable: demand_us <= supply_us,
            survivable: reserve_ms.saturating_mul(1_000) >= excess_us,
            demand_us,
            supply_us,
            excess_us,
            samples: n,
        })
    }

    /// Both conditions plus every term they rest on, for telemetry.
    ///
    /// One call, one `policy.window_len()`, so the reported `want`, `bound_us` and `admission`
    /// cannot describe three different window lengths — which they could if the log line assembled
    /// them from three separate calls at the call site.
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
            w.observe(bytes, us);
        }
        w
    }

    fn acq(bytes: u64, acquisition_us: u64) -> Acquisition {
        Acquisition {
            bytes,
            acquisition_us,
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
            w.observe(1_000, i);
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
    fn reset_empties_the_window_so_a_regime_change_cannot_be_bounded_by_the_old_link() {
        let mut w = window(&[(1_000, 100), (1_000, 200)]);
        w.reset();
        assert_eq!(w.len(), 0);
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
        // Differential against "max drawdown". Under exchangeability the ORDER of the window
        // carries no information, so the worst ordering -- every hard segment consecutive -- is the
        // only one that may be assumed. Two segments 500 ms over each: 1000 ms, not 500 ms.
        let p = policy(250, 1);
        let w = window(&[(1_000, 2_500_000), (1_000, 2_500_000), (1_000, 1_000_000)]);
        let a = w.admits(1_000, 2_000, 10_000, p).unwrap();
        assert_eq!(a.excess_us, 1_000_000);
    }

    #[test]
    fn a_reserve_exactly_covering_the_excess_survives_and_one_millisecond_less_does_not() {
        let p = policy(250, 1);
        let w = window(&[(1_000, 2_500_000), (1_000, 2_500_000), (1_000, 1_000_000)]);
        assert!(w.admits(1_000, 2_000, 1_000, p).unwrap().survivable);
        assert!(!w.admits(1_000, 2_000, 999, p).unwrap().survivable);
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
    fn the_window_survives_a_rung_commit_because_the_bound_transfers_by_bytes() {
        // The design claim the transfer form exists for: a sample taken at one rung is still
        // evidence about another. Nothing here resets, and the verdict after a size change rests
        // on all three samples rather than on however many arrived since the commit.
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
    fn the_readout_reports_a_filling_window_as_filling_rather_than_as_a_refusal() {
        // `have < want` is the ordinary state for the first n segments of every playback. A
        // read-out that renders it as `refuse` would make every startup look like a failure.
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

    /// Canonical full verdict. Read by `tests/test_harness.py`; keep the marker comment.
    const WIRE_ADMIT: &str = // wire-example
        "abr: window current=4000kbps verdict=admit have=3/3 eps=250pm clamp=0 bound=1000ms \
         demand=2600ms supply=6000ms excess=0ms sus=1 sur=1 reset=0 bytes=1000 dur=2000ms";

    /// Canonical filling verdict — every uncomputed number is `-1`, never `0`.
    const WIRE_FILLING: &str = // wire-example
        "abr: window current=720kbps verdict=filling have=1/3 eps=250pm clamp=0 bound=-1ms \
         demand=-1ms supply=-1ms excess=-1ms sus=0 sur=0 reset=2 bytes=500 dur=2000ms";

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
        w.observe(500, 400_000);
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
        w.observe(1_000, 300);
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
