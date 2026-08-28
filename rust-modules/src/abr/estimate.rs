use super::*;

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
        // **AN ABANDONED TRANSFER MAY LOWER THIS ESTIMATE AND MAY NEVER RAISE IT.**
        //
        // The rule is not a safety margin; it is what the observation MEANS. A fetch is abandoned
        // because its projected remainder did not fit the reserve — the event is evidence that the
        // link is INSUFFICIENT. Its bytes, meanwhile, are the receive buffer's opening burst
        // draining, measured over a few hundred microseconds, so as an estimate of SUSTAINED
        // capacity they are biased upward by construction. Reading them as "the link is fast"
        // inverts the meaning of the very event that produced them.
        //
        // Marking the sample incomplete was not enough, and the device showed exactly why. That
        // only raised `uncertainty_pm`, while the rate still entered — and a prefix four times the
        // history trips `is_regime_change` below, which RESTARTS the estimate at the prefix's own
        // value. So each abort reset the estimate upward: measured on `pipe_abr_down_outrun` with
        // the shaper holding **500 kbps**, the estimate walked 5 632 -> 28 744 -> 101 078 kbps
        // across successive aborts, and a 50% uncertainty discount on 101 Mbit/s is still two
        // orders of magnitude above the truth. Every downshift then chose an unaffordable target,
        // overran, aborted, and fed the estimate again.
        //
        // A slower-than-history prefix is kept, because that IS the abort's message and it is the
        // direction the evidence supports. A faster one contributes its uncertainty and nothing
        // else.
        if !observation.completed && self.samples > 0 && observation.kbps >= self.slow_kbps {
            self.uncertainty_pm = MAX_UNCERTAINTY_PM;
            self.samples = self.samples.saturating_add(1);
            return;
        }
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
    ///
    /// **`saturating_mul`, matching [`CapacityObservation::is_collapse`]'s own arithmetic.** The
    /// bare `measured_kbps * 4` this replaced wraps above ~1.07 Gbit/s, and that is reachable: an
    /// **865 Gbit/s** reading is on record from this television, which is exactly why
    /// `clamped_to_evidence` further down this file exists. The two halves of the split are what
    /// make it worth fixing rather than filing — `overflow-checks` is ON under `cargo test`, so the
    /// host PANICS, and OFF in release, so the set WRAPS to a small number and declares a collapse
    /// on the fastest link it has ever seen. Neither is a rounding error.
    pub(crate) fn collapse(&mut self, measured_kbps: u32) {
        if self.fast_kbps > 0 && measured_kbps.saturating_mul(4) < self.fast_kbps {
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
    /// **Rebuild an estimate from a snapshot of its own four fields** (I8) — the seek path, where
    /// the engine is destroyed and a fresh `Controller` is built on the other side.
    ///
    /// It is NOT `from_prior`, and the difference is the whole increment: `from_prior` pins
    /// `uncertainty_pm` at its cap and claims one sample, which is the correct reading of a
    /// bootstrap PROBE and a false one for an estimate that has just watched a link for a minute.
    /// Carrying the uncertainty and the sample count is what stops the ladder re-ramping.
    ///
    /// `samples == 0` is not an estimate and returns `None`; so is a zero rate, which is what an
    /// unwritten snapshot reads as.
    pub(crate) fn from_snapshot(
        slow_kbps: u32,
        fast_kbps: u32,
        uncertainty_pm: u32,
        samples: u32,
    ) -> Option<Self> {
        (samples > 0 && slow_kbps > 0).then_some(Self {
            fast_kbps,
            slow_kbps,
            uncertainty_pm: uncertainty_pm.min(MAX_UNCERTAINTY_PM),
            samples,
        })
    }

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
    /// **The interval floor applies to every tier, not only to `Strong`.** The two axes answer
    /// different questions and were being asked in the wrong order: the DURATION decides whether a
    /// rate measurement means anything at all, and the SIZE grades how much to trust one that
    /// does. `Strong` tested both and `Normal` tested size alone, so a transfer that was large but
    /// far too brief to measure — the exact case `clamped_to_evidence` exists for — was classified
    /// `Normal` and skipped the clamp.
    ///
    /// The consequence is not a slightly noisy estimate. Measured on the host simulator over a
    /// shaped 20 Mbit/s link (2026-08-28, leg D): after a link collapse and recovery the
    /// controller reached rung 2000 and never proposed another upshift for the rest of the run —
    /// no dwell, no reject block, no transaction attempted at all. A 2 s segment at that rung is
    /// ~500 KB, which crosses `NORMAL_OBSERVATION_BYTES` on size while arriving in ~200 us, so
    /// `network_kbps` read tens of millions of kbps unclamped. Consecutive samples read
    /// `18074, 18434921, 271729, 23303152, ...` kbps; each extreme reading inflated `fast_kbps`,
    /// the next honest one was then more than a factor of four below it, and `is_collapse` fired —
    /// which is the ONE call site of `AcquisitionWindow::reset`. The window was wiped six times
    /// running and never again passed 4 of the 19 samples an upshift needs. The estimator's own
    /// guard could not see the thing it was written for.
    ///
    /// **No new quantity is introduced.** `STRONG_OBSERVATION_US` is promoted from one half of the
    /// `Strong` test to the validity floor it always described, and `Strong` keeps its own meaning
    /// as "valid AND megabyte-scale". Below the floor a sample is `Weak`, which is what
    /// `ObservationQuality::Weak`'s own doc has always said — "truncated, tiny, **or over too
    /// short an interval to have left TCP's opening burst**" — and which routes it through
    /// `clamped_to_evidence`, so it claims `WEAK_SAMPLE_HEADROOM` times the rung it was measured
    /// on rather than a fabricated ceiling. That is the geometric ramp the clamp was designed
    /// around: the rung climbs, its segments get bigger and slower, and they become measurable.
    /// The cost of that ramp is real and is recorded beside the census assertion in `sim.rs`.
    pub(crate) fn quality(self) -> ObservationQuality {
        if !self.completed || self.active_us < MEASURABLE_OBSERVATION_US {
            return ObservationQuality::Weak;
        }
        if self.bytes >= STRONG_OBSERVATION_BYTES {
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
pub(crate) const REGIME_FACTOR: u32 = 4;

/// The largest discount [`CapacityEstimate::conservative_kbps`] will apply, and therefore the
/// value a demoted prior carries: half the estimate. A cap is needed at all because the discount
/// multiplies — an uncapped one would drive a volatile link's budget to zero and park Auto on the
/// emergency floor for the rest of the film.
pub(crate) const MAX_UNCERTAINTY_PM: u32 = 500;
/// How much more than the current rung a transfer too small to measure is allowed to claim. Eight
/// is one or two ladder steps: enough to climb out of a low rung promptly, small enough that the
/// climb is re-measured at every step instead of being asserted once.
pub(crate) const WEAK_SAMPLE_HEADROOM: u32 = 8;
pub(crate) const STRONG_OBSERVATION_BYTES: u64 = 1_048_576;
/// **The interval below which a transfer reports latency rather than capacity**, and therefore the
/// floor on a rate measurement being admissible at all. It was `STRONG_OBSERVATION_US` and was
/// asked only alongside `STRONG_OBSERVATION_BYTES`; `CapacityObservation::quality` now asks it
/// first, for every tier, which is what `ObservationQuality::Weak`'s doc always claimed. The value
/// is unchanged — this is a promotion, not a new threshold.
pub(crate) const MEASURABLE_OBSERVATION_US: u64 = 250_000;
pub(crate) const NORMAL_OBSERVATION_BYTES: u64 = 256 * 1024;

pub(crate) fn weighted_mean(old: u32, new: u32, weight: u32, denominator: u64) -> u32 {
    if old == 0 {
        return new;
    }
    let new_weight = u64::from(weight.min(u32::try_from(denominator).unwrap_or(u32::MAX)));
    ((u64::from(old) * (denominator - new_weight) + u64::from(new) * new_weight) / denominator)
        .min(u64::from(u32::MAX)) as u32
}

/// Validated timing for one completed segment. Invalid/zero timing is absence of evidence, never
/// infinite bandwidth or perfect production.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SegmentSample {
    pub(super) bytes: u64,
    pub(super) active_fetch_us: u64,
    total_fetch_us: u64,
    pub(super) media_duration_ms: u32,
    pub(crate) buffer: BufferSnapshot,
    /// Did the fetch that produced these bytes RUN TO COMPLETION. See [`Self::abandoned`] — this
    /// is the one input `Controller::observe` used to hardcode, and the hardcoding is what let an
    /// abandoned prefix set the budget its own abandonment disproved.
    completed: bool,
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
                completed: true,
            })
    }

    /// **Mark this sample as an ABANDONED transfer** — bytes that really crossed the wire, from a
    /// fetch that was cut off rather than finished.
    ///
    /// It exists because `CapacityObservation::completed` was already modelled, already drives
    /// `MAX_UNCERTAINTY_PM`, and was already the right answer — and `Controller::observe` passed a
    /// hardcoded `true`, so the one caller that had something else to say could not say it.
    ///
    /// The cost of that was measured. `ff::StallGuard` abandons a fetch after a few kilobytes, and
    /// those kilobytes are the receive buffer draining rather than the link: the device logged
    /// `bytes=1448 ... at 42277kbps` while the shaper held the link at **500 kbps**. Entered as a
    /// completed observation it kept `conservative_kbps` near 16 Mbit/s, so every downshift the
    /// controller correctly decided to make picked a target 30x too dear, overran, aborted, and
    /// decided again — 53 times on one rung pair. The decision was never wrong; the number it was
    /// made from was.
    ///
    /// Declaring it incomplete does not discard it. The bytes still count, and the estimate still
    /// moves; what changes is that it moves with `MAX_UNCERTAINTY_PM` attached, and
    /// `conservative_kbps` treats uncertainty as a DISCOUNT — so an abandoned fetch lowers the
    /// budget it is asked to justify instead of raising it.
    pub(crate) fn abandoned(mut self) -> Self {
        self.completed = false;
        self
    }

    pub(crate) fn completed(self) -> bool {
        self.completed
    }

    pub(crate) fn network_kbps(self) -> u32 {
        (kbps_from(self.bytes, self.active_fetch_us))
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

    /// End-to-end acquisition, microseconds -- what `abr/window.rs` transfers between rungs.
    /// `active_fetch_us` is the transfer alone and is what `network_kbps` divides by; this is the
    /// whole cost the reserve actually pays for, which is the quantity section 4 compares to `D`.
    pub(crate) fn total_fetch_us(self) -> u64 {
        self.total_fetch_us
    }

    /// Delivered bytes. The transfer bound's `b_i`, and the query for the current rung's own
    /// admission -- so it is the one field of `abr/window.rs`'s arithmetic that is not derivable
    /// from anything else already on the wire.
    pub(crate) fn bytes(self) -> u64 {
        self.bytes
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

/// **Bits over microseconds, as kbps** — `bytes * 8 / us` with the two thousands folded, so the
/// unit conversion exists once.
///
/// It is a free function rather than a method because three callers hold the same two numbers in
/// three different shapes: `SegmentSample` (a completed segment), `ff.rs`'s `StallGuard` (a body
/// still arriving) and `OriginalRecovery` (a delta between two progress readings). The last two
/// open-coded it, and `ff.rs`'s log line carried a comment asking for exactly this — "spelled the
/// same way `should_abort` spells it, so a log line and the decision behind it cannot drift
/// apart" — settling for a comment where a call would enforce it.
///
/// `us` is clamped to 1: a zero divisor is a caller that has measured nothing, and every caller
/// guards that case for its own reasons before reaching here.
pub(crate) fn kbps_from(bytes: u64, us: u64) -> u64 {
    bytes.saturating_mul(8_000) / us.max(1)
}
