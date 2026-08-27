//! Client-managed adaptive-quality policy for Plex's measured fixed-rendition HLS sessions.
//!
//! The PMS probe proved that one HLS encoder session has one fixed rendition. A quality move is
//! therefore a transaction: propose a rung, prime a separately named encoder, then commit only
//! after that candidate has delivered a decodable segment with enough headroom. A rejected prime
//! leaves this controller's current rung untouched.
//!
//! All presentation values are normalized [`MediaTimeMs`] values. Raw FFmpeg PTS, stream time
//! bases, segment-local offsets and discontinuity counters never cross this boundary.
//!
//! # The decision pipeline
//!
//! Every Auto decision — the first one at startup and every one during playback — runs the same
//! ordered stages, and the ordering is the design:
//!
//! ```text
//! feasibility  -> which playback states are technically possible at all
//! estimation   -> delivery capacity, PMS production, buffer, each with UNCERTAINTY
//! risk         -> per-candidate starvation horizon + production + buffer stress
//! utility      -> compare feasible states: quality + features - risk - server - transition
//! selection    -> argmax utility
//! validation   -> prime the winner off-screen and grade the actual media
//! commit       -> or keep the current state, untouched
//! ```
//!
//! Three consequences are worth stating, because each replaced an earlier rule that looked
//! reasonable and was wrong:
//!
//! * **Feasibility is not a utility term.** A candidate the decoder cannot decode, or a raster
//!   the device's own codec table refuses, is removed before anything is scored. No weight can be
//!   large enough to make an impossible state the argmax, so no weight is asked to.
//! * **Measurements feed [`CandidateRisk`], not the utility formula.** Variance, VBR headroom,
//!   buffer slope and PMS cadence all reach the decision through one risk number per candidate.
//!   The alternative — one term per telemetry field — is how a utility function becomes
//!   untunable, since every new measurement silently reweights every old one.
//! * **A deficit is not an emergency.** `C < R` says the buffer drains, not that playback stops:
//!   [`starvation_horizon`] turns the pair into seconds, and 60 s of reserve against a 3% deficit
//!   is half an hour away from trouble. Auto used to abandon Original on two slow windows.
//!
//! # What this module deliberately does not model
//!
//! * **Decoder/render health.** This television publishes no trustworthy dropped-frame or
//!   decoder-starvation counter — the heartbeat's `vtick=`/`vgap=` pair counts a 5 Hz position
//!   callback and reads flat straight through a visible stutter (see the CLAUDE.md instrument
//!   note). A proxy invented here would be an unfalsifiable input to every decision below, so
//!   candidate feasibility asks the device's codec table (a fact) and nothing asks the decoder
//!   how it feels.
//! * **Thermal state.** Both a throttling SoC and a throttling server arrive as what they
//!   actually are here: production ratio drift, delivery drift, buffer slope.
//! * **Anything learned.** Every number below is a measurement or a policy constant with a
//!   product meaning in [`AbrPolicy`].

// The split is by DECISION STAGE, matching the pipeline in the doc above: `plant` is what the
// world does, `estimate` is what we believe about it, `viability`/`mode` are the comparisons,
// `controller` is the transaction that acts. `ladder` and `units` are the vocabulary all of them
// share. Re-exported flat, because `abr::Rung` is the name the rest of the crate has always used
// and a split is not a reason to churn 26 call sites.

mod bootstrap;
mod controller;
mod estimate;
mod ladder;
mod mode;
mod original;
mod plant;
mod units;
mod viability;
mod window;

pub(crate) use bootstrap::*;
pub(crate) use controller::*;
pub(crate) use estimate::*;
pub(crate) use ladder::*;
pub(crate) use mode::*;
pub(crate) use original::*;
pub(crate) use plant::*;
pub(crate) use units::*;
pub(crate) use viability::*;
pub(crate) use window::*;

/// **Every tunable in one place, and every field answers "what product behaviour is this?"** —
/// which is the test a number has to pass to live here at all. What this type replaced was a
/// scatter of `3 good samples`, `8 cooldown samples`, `2 bad windows` and a bare `1_100`, none of
/// which said what it was for, so none of them could be argued with.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AbrPolicy {
    /// **The section 4 admission rule's two explicit choices** -- see [`AdmissionPolicy`]. Both are
    /// classification (4), and both now GATE EVERY UPSHIFT: `n = k/eps - 1` is the evidence the
    /// controller must hold before it may propose one, and the same window decides whether the
    /// primed candidate commits.
    pub(crate) admission: AdmissionPolicy,
    /// PMS is comfortably ahead of real time below this segment-acquisition ratio. Above it a
    /// candidate may still play, but it has no margin left for a slower scene.
    pub(crate) production_safe_pm: u32,
    /// At or above this, the server is producing at or slower than real time: a JIT encoder that
    /// cannot keep up will drain any buffer eventually, whatever the network does.
    pub(crate) production_max_pm: u32,
    /// **The part of segment acquisition that is not production** — connection, request, time to
    /// first byte, playlist latency — as a per-mille of content duration. 250 is half a second of a
    /// two-second segment, which is an ordinary round trip to a remote PMS. It exists so
    /// [`ProductionEstimate::predicted_ratio_pm`] scales only the work, and never reads a LAN's
    /// round trips as a struggling encoder.
    pub(crate) production_floor_pm: u32,
    /// Below this the next stall is close enough that "wait and see" is no longer a policy.
    pub(crate) emergency_buffer_ms: i64,
    /// **VBR headroom over a whole-file average.** A file averaging 60 Mbit/s contains scenes well
    /// above it, so the average is a lower bound on demand, not the demand. Spending the entire
    /// measured link on the average merely postpones starvation to the first busy scene.
    pub(crate) vbr_allowance_pm: u32,
    /// Cold-start Original admission, where there is exactly one probe and no history. Higher than
    /// [`Self::vbr_allowance_pm`] on purpose: at that moment the estimate has no dispersion to
    /// discount, so the margin has to carry the uncertainty itself.
    pub(crate) bootstrap_confidence_pm: u32,
    /// How fast an unmeasured gap costs confidence. One of these is a widening; four is a demotion
    /// to a prior ([`CapacityEstimate::age_ms`]).
    pub(crate) stale_half_life_ms: u32,
    /// Below this starvation horizon, a mode change is worth its visible cost — the buffer will
    /// not survive the wait for a better answer.
    pub(crate) starvation_fallback_secs: u32,
    /// Above this horizon the deficit is arithmetic rather than a problem: 60 s of reserve against
    /// a 3% shortfall is half an hour away, and abandoning Original for it would be the old
    /// two-slow-windows bug in a new costume.
    pub(crate) starvation_safe_secs: u32,
    /// Utility cost of a switch the VIEWER SEES — a reload, a black frame, a re-Load. Denominated
    /// in the same units as the quality score below, so the two can be compared at all: on that
    /// scale 15 is about one step of the ladder (2 Mbps to 4), which is the right order for a
    /// two-second interruption. It was 30 for one afternoon, and the device run says what that
    /// buys: 30 plus a fresh switch's penalty outprices Original's entire quality advantage, so
    /// Auto would not return to a recovered link for about four minutes after a fallback.
    pub(crate) visible_switch_cost: i64,
    /// Extra cost per visible switch already made in this playback. One switch is a decision; four
    /// is flapping, and this is what makes the fourth expensive without a hard cooldown counter.
    /// At 15 the arithmetic is: a first move costs 15, the return trip 30 (still inside Original's
    /// 40-point advantage, so one round trip is allowed), and a third 45 (refused).
    pub(crate) visible_switch_penalty: i64,
    /// Half-life of that penalty. A switch fifteen minutes ago is history; one fifteen seconds ago
    /// is a pattern.
    pub(crate) visible_switch_decay_ms: u64,
    /// What Original is worth over the best HLS rendition, before any risk or cost: no generation
    /// loss, source audio, Dolby Vision and Atmos preserved, and zero server video encoding.
    pub(crate) original_quality_bonus: i64,
    /// Added when the source actually carries those features, so the bonus is about this file
    /// rather than about Original in the abstract.
    pub(crate) original_feature_bonus: i64,
    /// Playback remaining at which a mode's benefit counts in full. Below it the benefit is scaled
    /// down linearly, which is what makes a reload with twenty seconds left lose to doing nothing
    /// without anybody writing `if remaining < 20`. Two minutes: the point of the ramp is to price
    /// a benefit against the INTERRUPTION that buys it, and once the remainder dwarfs a two-second
    /// reload there is nothing left to discount for.
    pub(crate) benefit_horizon_ms: i64,
    /// Weight on [`CandidateRisk::score`] in the utility sum.
    pub(crate) risk_weight: i64,
    /// Weight on ongoing PMS transcoding work. Small, because a watchable picture beats a tidy
    /// server — but not zero, because 2.1x the work for 4% more bits is a real trade.
    pub(crate) server_cost_weight: i64,
}

impl AbrPolicy {
    pub(crate) fn measured() -> Self {
        Self {
            // **eps = 50pm, k = 1, so n = 19.** Stated as what it costs a viewer rather than as
            // a percentage: one acquisition in twenty may exceed the bound, and at the 2 s segment
            // this pipeline requests that is **one exceedance per ~40 s of playback**. An
            // exceedance is not a stall — it is one segment arriving later than the bound
            // promised, which condition (2) has already required the reserve to absorb — so the
            // quantity being chosen is how often the reserve is drawn on, not how often the
            // picture stops.
            //
            // `k = 1` takes the window's maximum: the tightest bound available at this eps, and
            // the most sensitive to a single outlier. It is the conservative end of the one axis
            // that is free once eps is fixed. Raising `k` buys a longer proof horizon at the same
            // eps (n = k/eps - 1) and should be argued from a stated passage length, which this
            // project does not have yet.
            admission: AdmissionPolicy { epsilon_pm: 50, k: 1 },
            production_safe_pm: 750,
            production_max_pm: 1_100,
            production_floor_pm: 250,
            emergency_buffer_ms: 2_000,
            vbr_allowance_pm: 1_350,
            bootstrap_confidence_pm: 1_350,
            stale_half_life_ms: 30_000,
            starvation_fallback_secs: 20,
            starvation_safe_secs: 60,
            visible_switch_cost: 15,
            visible_switch_penalty: 15,
            visible_switch_decay_ms: 120_000,
            original_quality_bonus: 40,
            original_feature_bonus: 25,
            benefit_horizon_ms: 120_000,
            risk_weight: 2,
            server_cost_weight: 4,
        }
    }
}

/// The closed-loop plant (I0-B/C). Host-only: it never ships.
#[cfg(test)]
mod sim;

#[cfg(test)]
mod tests;
