//! Adaptive playback has two deliberately separate controllers.
//!
//! The HLS rung controller is physical. A finite response at the current rendition is
//! demand-capped and therefore cannot reveal unused path capacity. Completed acquisitions form a
//! finite bag `(A_i, D_i)` and decide only what they directly identify:
//!
//! ```text
//! sustainable  <=> ΣA_i <= ΣD_i
//! ordered R_o    =  max_i(Σ_{j<i}(A_j-D_j) + A_i)
//! stress R_s     =  Σ(A_i-D_i)+ + max_i min(A_i,D_i)
//! exploration E  =  (B-max(R_s,D_next))+
//! ```
//!
//! An upshift is the missing excitation. PMS uses fixed-rendition sessions, so the candidate is
//! primed as a separate encoder under the absolute `E` deadline and commits only from its own
//! completed `A <= D && B_post >= A` evidence. `R_o` diagnoses the observed current-point queue;
//! `R_s` remains a retrospective stress diagnostic and conservative experiment-funding runway, not
//! a predictive downshift trigger. There is no dwell, rate headroom, fixed buffer threshold, passive
//! capacity ceiling or probability claim in that decision. Abandoned prefixes are censored and
//! never become capacity samples.
//!
//! Original versus HLS is a product utility decision, not a network theorem. Original creates a
//! new stream and visibly reloads the pipeline, but preserves direct-play/remux quality, DV/Atmos
//! and removes recurring PMS video-encode work. Those recurring effects scale with remaining
//! playback; the one-time visible transition cost does not. The weights are explicit product
//! choices and must never be described as inferred probabilities.
//!
//! # What this module deliberately does not model
//!
//! * **Decoder/render health.** This television publishes no trustworthy dropped-frame or
//!   decoder-starvation counter — the heartbeat's `vtick=`/`vgap=` pair counts a 5 Hz position
//!   callback and reads flat straight through a visible stutter (see the `docs/agent-reference.md` instrument
//!   note). A proxy invented here would be an unfalsifiable input to every decision below, so
//!   candidate feasibility asks the device's codec table (a fact) and nothing asks the decoder
//!   how it feels.
//! * **Thermal state.** Both a throttling SoC and a throttling server arrive as what they
//!   actually are here: production ratio drift, delivery drift, buffer slope.
//! * **Anything learned.** Every number below is a measurement or a policy constant with a
//!   product meaning in [`AbrPolicy`].
//!
//! All presentation values are normalized [`MediaTimeMs`] values. Raw FFmpeg PTS, segment-local
//! offsets and discontinuity counters do not cross this boundary. See
//! `docs/adaptive-playback.md` for the current contract and
//! `docs/adaptive-playback-spec.md` for its derivation/history.

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
/// **Operational ceiling on how long a source probe may run.** Its ordinary deadline is derived
/// from the duration represented by the finite source object; this cap binds only when the minimum
/// byte sample for a tiny source represents longer. [`source_probe_plan`] is the one derivation
/// consumed by both the gate and transfer, so affordability cannot drift from execution.
pub(crate) const PROBE_BUDGET_MS: u64 = 4_000;

#[derive(Clone, Copy, Debug)]
pub(crate) struct AbrPolicy {
    /// Retired order-statistic calibration retained for offline corpus comparisons; see
    /// [`AdmissionPolicy`]. The live controller does not read either value: current-rung physics
    /// uses the whole finite bag and a candidate commits from its own completed acquisition.
    #[allow(dead_code)] // legacy/offline order-statistic calibration; not a live ABR gate
    pub(crate) admission: AdmissionPolicy,
    /// **The part of segment acquisition that is not production** — connection, request, time to
    /// first byte, playlist latency — as a per-mille of content duration. 250 is half a second of a
    /// two-second segment, which is an ordinary round trip to a remote PMS. It exists so
    /// [`ProductionEstimate::predicted_ratio_pm`] scales only the work, and never reads a LAN's
    /// round trips as a struggling encoder.
    pub(crate) production_floor_pm: u32,
    /// Below this the next stall is close enough that "wait and see" is no longer a policy.
    pub(crate) emergency_buffer_ms: i64,
    /// **The reserve `B*` asks for, as a ceiling** (N3). It is the value the deleted
    /// `minimum_buffer_ms` carried, kept deliberately: at 2 500 ms the deficit is zero whenever the
    /// reserve exceeds one and a quarter segments, so the corrected refill formula lands without
    /// moving an expected value and M4 decides whether it rises.
    pub(crate) buffer_target_ms: i64,
    /// **How much of the REACHABLE ceiling `B*` may ask for** (N3's alpha). The target is
    /// `min(buffer_target_ms, alpha * B_max_est(R))`, and this term is what stops it being a
    /// promise the byte caps cannot keep.
    ///
    /// **The inertness arithmetic here was the 8 MiB video queue's and is now stale.** It read:
    /// "at the shipped target it binds only above ~19 700 kbps of video ES — P1080High and Uhd
    /// alone — so it is inert on eleven of thirteen rungs today". That crossover is
    /// `alpha*B_max < 2 500`, i.e. `R_v > 67 108 864/3 400 ~ 19 738`, computed against
    /// `AQ_VIDEO_BYTES = 8 MiB`. Phase 0 grew that cap to **10 MiB** (`player/engine.rs`), so
    /// `alpha*B_max` now runs 2 825 ms (Uhd) to 23 645 ms (P240) — above `buffer_target_ms` at
    /// EVERY rung, and the `min` therefore takes the target at all thirteen.
    ///
    /// So this term is inert in `B*` on thirteen of thirteen rather than eleven — but it is NOT
    /// dead, and the difference matters: it is live in the I3b(b) upshift reserve gate
    /// (`controller.rs`, `min(3*segment, alpha*B_max_est)`) at every rung from 10 000 up, where it
    /// is the binding half. It becomes the live term in `B*` too, and its device validation stops
    /// being optional, if the target rises.
    pub(crate) buffer_reserve_fraction_pm: u32,
    /// **How fast a reserve deficit must close**, wall clock (N3's `H`). A candidate that leaves
    /// the reserve short may claim `C_safe * H / (H + D)`, so the horizon is what converts "we are
    /// `D` milliseconds down" into "you may spend this much". TEMPORARY: inert at today's `B*`,
    /// so unobservable until `buffer_target_ms` moves, and not device-validated.
    pub(crate) buffer_refill_horizon_ms: i64,
    /// **The audio ES rate to use when none has been measured**, for `B_max_est`'s audio lane.
    /// TEMPORARY. 192 kbps is the census value at every rung from 10 000 up; the bottom of the
    /// ladder measures 98-159, and the audio lane is the one that BINDS down there, so an assumed
    /// value there is optimistic and the measured one must be preferred wherever it exists.
    pub(crate) assumed_audio_kbps: u32,
    /// How fast an unmeasured gap costs confidence. One of these is a widening; four is a demotion
    /// to a prior ([`CapacityEstimate::age_ms`]).
    pub(crate) stale_half_life_ms: u32,
    /// Below this starvation horizon, a mode change is worth its visible cost. This is a policy
    /// boundary, not evidence that one derivative is a trend: runtime Original confirms a drain
    /// when the observed runway can afford it, and bypasses confirmation when waiting would spend
    /// that runway down to [`Self::emergency_buffer_ms`].
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
    /// **Each visible switch raises the price of the next by this much**, and that SHAPE is the
    /// claim. The worked refusal boundary that used to be written here — "a first move costs 15,
    /// the return trip 30 (inside Original's 40-point advantage), a third 45 (refused)" — rested on
    /// a flat 40, and there is no flat 40 any more: I7a made Original's quality source-dependent,
    /// and N16 gives every Original an unconditional `generation_loss_bonus` on top. At the
    /// reference case the margin is 48, so the old "third move refused" clears by 3; with DV and
    /// Atmos it is 65 and even a fourth clears. Where the ladder now bites depends on the file,
    /// which is the point of scoring it from the file.
    pub(crate) visible_switch_penalty: i64,
    /// Half-life of that penalty. A switch fifteen minutes ago is history; one fifteen seconds ago
    /// is a pattern.
    pub(crate) visible_switch_decay_ms: u64,
    /// **The structural floor of Original's quality term**, added to the SOURCE-derived score
    /// (`mode::source_quality_score`): zero server video encoding, and the source's own audio.
    ///
    /// It is not "what Original is worth over the best HLS rendition" any more, and it stopped
    /// being that in two steps. I7a made the quality term score against a real alternative, so the
    /// difference moves with the source's rate and raster instead of being a constant. And N16
    /// split generation loss, Dolby Vision and Atmos out into the three bonuses below — so a
    /// sentence claiming this 40 covers them double-books the exact terms whose own doc calls
    /// pricing them together "the conflation N16 names".
    pub(crate) original_quality_bonus: i64,
    /// **What Original preserves about THIS file, split three ways** (N16). It was one flat
    /// `original_feature_bonus = 25` behind one boolean, `dovi.profile > 0 || immersive` — so an
    /// Atmos-only film bought two visible reloads for a benefit inaudible on television speakers,
    /// priced identically to a Dolby Vision panel-mode change.
    ///
    /// **The ORDER is the content; the magnitudes are not identifiable** (§6.2 says so for all
    /// three rows: "ordering yes, magnitude no"). So the split is not three chosen numbers — it is
    /// rank weights 3:2:1 over the same preserved total of 25, and the only claim being made is
    /// `dv > generation_loss > atmos`. A host test asserts the ORDERING and the total, never the
    /// values, so a future measurement can move them without re-fitting anything.
    ///
    /// **Dolby Vision first** because it is a visible panel-mode change the viewer can point at.
    /// **Generation loss second**, and it is the one that applies to EVERY Original — no re-encode
    /// at all — which is why pricing it at zero for a plain file while pricing DV and Atmos
    /// together at 25 was the conflation N16 names. **Atmos last** because the plan says out loud
    /// what the television's own speakers make of it.
    pub(crate) dv_bonus: i64,
    /// See [`Self::dv_bonus`]. Applies to every Original, feature flags or not.
    pub(crate) generation_loss_bonus: i64,
    /// See [`Self::dv_bonus`]. Last of the three, deliberately.
    pub(crate) atmos_bonus: i64,
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
    /// **How long an unsafe deficit must hold before Original is abandoned on JUDGEMENT** — the
    /// `SustainedDeficit` exit, the only one of the three a utility comparison may veto.
    ///
    /// Wall clock, and that is the whole of N13. It was `ORIGINAL_DEFICIT_WINDOWS = 6` counting
    /// 750 ms windows of ACTIVE BODY-READ time, a clock that stops under backpressure — i.e.
    /// exactly when the buffer is healthy — so the rule named no duration. The number is carried
    /// across unchanged (6 x 750); the CONVERSION is not 1:1 in the world, it is conservative in
    /// the safe direction (the wall interval is longer under backpressure, so the new rule is at
    /// least as patient), and the observed ratio is an M2 measurement nobody has taken.
    pub(crate) sustained_unsafe_deficit_ms: i64,
    /// **Operational maximum for a source probe, ms.**
    ///
    /// [`source_probe_plan`] derives the useful deadline from the duration represented by the
    /// exact target bytes and caps it here. Both `route::probe_original` and
    /// `OriginalRecovery::probe_due` consume that same plan, so the gate and transfer cannot drift.
    /// The gate additionally preserves `max(R_s,D_next)`, the larger of the HLS stress-replay
    /// boundary and the exact next parsed media object; charging only the probe used to leave the
    /// following acquisition unfunded.
    pub(crate) probe_budget_ms: u64,
}

impl AbrPolicy {
    pub(crate) fn measured() -> Self {
        Self {
            // Historical comparator only: eps = 50pm and k = 1 imply n = 19. The former marginal
            // probability interpretation is unavailable for selected, demand-capped requests;
            // `AdmissionPolicy` records the full downgrade. Keeping the measured setting makes
            // archived traces reproducible without putting either number back on the live path.
            admission: AdmissionPolicy {
                epsilon_pm: 50,
                k: 1,
            },
            production_floor_pm: 250,
            emergency_buffer_ms: 2_000,
            buffer_target_ms: 2_500,
            buffer_reserve_fraction_pm: 500,
            buffer_refill_horizon_ms: 10_000,
            assumed_audio_kbps: 192,
            stale_half_life_ms: 30_000,
            starvation_fallback_secs: 20,
            starvation_safe_secs: 60,
            visible_switch_cost: 15,
            visible_switch_penalty: 15,
            visible_switch_decay_ms: 120_000,
            original_quality_bonus: 40,
            // Rank weights 3:2:1 over the preserved total of 25. Only the ORDER is a claim.
            dv_bonus: 13,
            generation_loss_bonus: 8,
            atmos_bonus: 4,
            benefit_horizon_ms: 120_000,
            risk_weight: 2,
            server_cost_weight: 4,
            sustained_unsafe_deficit_ms: 4_500,
            // Operational cap; `source_probe_plan` derives the actual transfer deadline.
            probe_budget_ms: PROBE_BUDGET_MS,
        }
    }
}

/// The closed-loop plant (I0-B/C). Host-only: it never ships.
#[cfg(test)]
mod sim;

#[cfg(test)]
mod tests;
