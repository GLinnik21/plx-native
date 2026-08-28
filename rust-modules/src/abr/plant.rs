use super::*;

/// Tail timestamps from the demuxer after normalization. `audio_expected` distinguishes genuinely
/// silent media from an A/V session whose audio lane has not produced a timestamp yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BufferSnapshot {
    pub(crate) playback: MediaTimeMs,
    pub(crate) video_tail: MediaTimeMs,
    pub(crate) audio_tail: Option<MediaTimeMs>,
    pub(crate) audio_expected: bool,
}

impl BufferSnapshot {
    /// The playable reserve, `min(video, audio) - playback`, or **`None` when it is not knowable
    /// yet** — an A/V session whose audio lane has produced no timestamp since the open or the
    /// seek.
    ///
    /// # Why this is an `Option` and not a zero
    ///
    /// It returned `0` for that case, and every caller read `0` as *empty*. Those are opposite
    /// facts. "The audio lane has not spoken yet" is what a session looks like for the first
    /// segment after every open and every seek — with the video queue holding whatever it holds,
    /// which on a fast link is the full 8 MiB. So the one input the emergency path is keyed on
    /// reported an empty reserve while the reserve was full, and `buffer_bad` (`buffered < segment
    /// || starving()`) fired a downshift that nothing about the link or the server justified.
    ///
    /// The same condition is already an `Option` one function down (`progressive_buffered_ms`
    /// returns `None` when either lane is missing) and has been since it was written, which is
    /// what makes this a transcription error rather than a design question: the two paths encode
    /// the same physical situation and disagreed about whether it was a number.
    ///
    /// Every caller now states what it does with "unknown" — and the answers differ, which is the
    /// point: [`Controller::steady`] makes NO decision, [`Controller::candidate_ready`] refuses,
    /// the Original probe declines to spend a probe, and the two log sites print `none`. A single
    /// fabricated value could not have served four different correct answers.
    pub(crate) fn buffered_ms(self) -> Option<i64> {
        let tail = match (self.audio_expected, self.audio_tail) {
            (true, None) => return None,
            (_, Some(audio)) => audio.min(self.video_tail),
            (false, None) => self.video_tail,
        };
        Some(tail.saturating_since(self.playback))
    }

    /// The VIDEO lane's own buffered duration, ignoring audio. Diagnostic only: nothing in the
    /// controller reads it, and nothing may — [`Self::buffered_ms`] is the quantity every decision
    /// is made on. It exists because the playable reserve is `min(video, audio)` and the two lanes
    /// have DIFFERENT ceilings (the video queue is 8 MiB against a multi-Mbit stream, the audio
    /// queue 1 MiB against ~192 kbps), so which one binds changes with the rung — and a `buf=`
    /// alone cannot say which. See `docs/adaptive-playback-plan.md` §0.1.
    pub(crate) fn video_buffered_ms(self) -> i64 {
        self.video_tail.saturating_since(self.playback)
    }

    /// The AUDIO lane's own buffered duration, or `None` when this stream has no audio or the lane
    /// has not yet produced a timestamp. Diagnostic only, as [`Self::video_buffered_ms`].
    pub(crate) fn audio_buffered_ms(self) -> Option<i64> {
        self.audio_tail.map(|a| a.saturating_since(self.playback))
    }
}

/// **Can the SERVER keep up** — the resource constraint that is not the network.
///
/// `ratio_pm` is per-mille of segment acquisition time over content duration, so 1000 is exactly
/// real time: below it PMS is running ahead of playback, above it the encoder is losing ground
/// whatever the link does. The two constraints have to be separate because they move
/// independently — the measured 4K point costs 4% more bits and 110% more server work — and
/// because only one of them can be fixed by asking for less picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProductionEstimate {
    pub(crate) ratio_pm: u32,
    pub(crate) uncertainty_pm: u32,
    pub(crate) samples: u32,
    /// The measured ratio divided by the load of the candidate that produced it, i.e. how fast
    /// this server is per unit of transcoding work. This is the part that transfers between
    /// candidates; the ratio itself does not.
    server_pm: u32,
}

impl ProductionEstimate {
    /// One steady-state segment. `cold_start` is a NEW ENCODER's first segment, which carries
    /// decoder and encoder start-up and is not the cadence the replacement will sustain — it is
    /// admitted at low weight rather than discarded, because a cold start bad enough to matter is
    /// still evidence about the server.
    pub(crate) fn observe(&mut self, ratio_pm: u32, load_pm: u32, cold_start: bool) {
        let weight = if cold_start { 1 } else { 3 };
        let normalized = u32::try_from(
            u64::from(ratio_pm).saturating_mul(1_000) / u64::from(load_pm.max(1)),
        )
        .unwrap_or(u32::MAX);
        let (ratio, server) = if self.samples == 0 {
            (ratio_pm, normalized)
        } else {
            (
                weighted_mean(self.ratio_pm, ratio_pm, weight, 8),
                weighted_mean(self.server_pm, normalized, weight, 8),
            )
        };
        self.ratio_pm = ratio;
        self.server_pm = server;
        self.samples = self.samples.saturating_add(1);
        self.uncertainty_pm = if self.samples < 3 {
            250
        } else if ratio_pm.abs_diff(self.ratio_pm) > 200 {
            500
        } else {
            250
        };
    }

    /// What this server would probably spend on `candidate`, given what it is spending on
    /// `current`. `None` until there is a measurement to scale — absence of evidence is not a
    /// prediction of success, and the callers treat it that way.
    ///
    /// **Only part of the measurement scales, and getting that wrong makes this unusable.** The
    /// ratio is total ACQUISITION time over content duration, so it contains a fixed per-segment
    /// cost — connection, request, time to first byte, playlist latency — that does not care how
    /// hard the encode was. Extrapolating the whole number by the load ratio therefore reads a
    /// LAN's 300 ms of round trips on a 480p segment as a struggling server and vetoes every
    /// upshift out of the opening rung (measured on this suite: 480p at 0.4 predicted 1080p at
    /// 1.0, and Auto never left 480p on a 7 Mbit/s link). Split the measurement at
    /// [`AbrPolicy::production_floor_pm`] and scale only the part above it.
    pub(crate) fn predicted_ratio_pm(
        &self,
        candidate: HlsCandidate,
        current: HlsCandidate,
        policy: &AbrPolicy,
    ) -> Option<u32> {
        if self.samples == 0 {
            return None;
        }
        // Same operating point: the measurement IS the prediction. Going through the load model
        // for that case would substitute an interpolated constant for a real number.
        if candidate.rung == current.rung {
            return Some(self.ratio_pm);
        }
        let overhead = self.ratio_pm.min(policy.production_floor_pm);
        let work = u64::from(self.ratio_pm - overhead);
        let scaled = work.saturating_mul(u64::from(candidate.production_load_pm))
            / u64::from(current.production_load_pm.max(1));
        Some(
            u32::try_from(u64::from(overhead).saturating_add(scaled)).unwrap_or(u32::MAX),
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BufferEstimate {
    pub(crate) buffered_ms: i64,
    pub(crate) slope_ms_per_s: i64,
    /// The LAST raw change, unsmoothed. Kept beside the smoothed slope because the two answer
    /// different questions and one of them cannot wait: `slope_ms_per_s` is a 3:1 EWMA, so a single
    /// sharp drop after a healthy stretch still reads POSITIVE — correct for "is this a trend",
    /// useless for "did the reserve just fall off a cliff". The emergency guard reads this one.
    pub(crate) last_delta_ms: i64,
    pub(super) samples: u32,
    pub(super) draining_samples: u32,
}

impl BufferEstimate {
    /// One segment's observation. **`None` is not an observation** — it is the audio lane having
    /// said nothing yet ([`BufferSnapshot::buffered_ms`]) — so it advances no counter and moves no
    /// estimate. Folding it in as a zero would enter a full reserve into the EWMA as a cliff, and
    /// `last_delta_ms` (which the emergency guard reads precisely because it is unsmoothed) would
    /// carry the whole fabricated drop.
    pub(crate) fn update(&mut self, buffered_ms: Option<i64>, media_duration_ms: i64) {
        let Some(buffered_ms) = buffered_ms else {
            return;
        };
        if media_duration_ms <= 0 {
            return;
        }
        // **A slope needs TWO observations, and the first sample has one.** `self.buffered_ms`
        // starts at zero, so on the first update `delta` is the whole reserve rather than a
        // change in it — a playback opening with a 20 s reserve and 2 s segments manufactures
        // `+10000 ms/s`. The guard above already exists for exactly this ("folding it in as a zero
        // would enter a full reserve into the EWMA as a cliff") and was applied to `last_delta_ms`
        // alone, so the same fabricated delta still SEEDED the slope, and a 3:1 EWMA needs about
        // twenty samples to forget it. The direction of harm is the one
        // `[[reserve-cannot-see-a-slow-film]]` names: the fabrication reads as the reserve FILLING
        // fast, which is the reading that masks a real drain.
        //
        // So the seed moves one sample later, to the first update that has a real delta. Sample
        // zero records the level and nothing else; `slope_ms_per_s` stays at its default, which
        // says "no rate of change is known" — the honest answer, and a safe one, because no
        // upshift can occur on the first sample anyway (the acquisition window needs nineteen).
        let delta = buffered_ms - self.buffered_ms;
        let first = self.samples == 0;
        self.last_delta_ms = if first { 0 } else { delta };
        let sample_slope = (delta * 1_000) / media_duration_ms;
        self.slope_ms_per_s = match self.samples {
            0 => self.slope_ms_per_s,
            1 => sample_slope,
            _ => (self.slope_ms_per_s * 3 + sample_slope) / 4,
        };
        if self.draining() {
            self.draining_samples = self.draining_samples.saturating_add(1);
        } else {
            self.draining_samples = 0;
        }
        self.buffered_ms = buffered_ms;
        self.samples = self.samples.saturating_add(1);
    }

    /// **Is the reserve actually shrinking** — a magnitude test, not a sign test, and that is a
    /// device finding rather than a refinement. `slope_ms_per_s` is a 3:1 EWMA, so after any real
    /// drain it decays toward zero asymptotically and NEVER REACHES IT: measured on the television
    /// 2026-08-25, a buffer sitting flat at 11,918 ms reported −16, −12, −9, −6, −4 ms/s over
    /// successive segments, every one of them "draining" to a sign test. The upshift gate requires
    /// `!draining`, so Auto sat on the 10 Mbps rung with a 25 Mbit/s safe budget and a full reserve
    /// for the rest of the film. The same shape as `ui::idle`'s rest test, and the same fix: judge
    /// the travel, not the sign of it.
    pub(crate) fn draining(&self) -> bool {
        self.slope_ms_per_s < -DRAIN_EPS_MS_PER_S
    }

    pub(crate) fn starving(&self) -> bool {
        self.buffered_ms <= 2_000
            || (self.buffered_ms <= 6_000 && self.draining_samples >= 2)
    }
}

/// Below this, a slope is noise around flat: 50 ms of content per second is 5% of real time, which
/// no reserve notices and no decision should turn on.
pub(crate) const DRAIN_EPS_MS_PER_S: i64 = 50;

/// Deterministic starvation math under a constant-rate approximation. This is deliberately not
/// a prediction of when playback will stop; it is a comparable risk horizon across candidates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StarvationHorizon {
    pub(crate) seconds: Option<u32>,
}

pub(crate) fn starvation_horizon(
    buffer_ms: i64,
    requirement_kbps: u32,
    capacity_kbps: u32,
) -> StarvationHorizon {
    if capacity_kbps >= requirement_kbps || requirement_kbps == 0 {
        return StarvationHorizon { seconds: None };
    }
    let deficit = i64::from(requirement_kbps - capacity_kbps);
    // **`time_to_empty_ms`, and the name matters.** This was a public field called `drain_per_s`
    // with no reader anywhere in the crate — and the name asserted the wrong dimension:
    // `(ms · kbps) / kbps` is MILLISECONDS, which is why the next line divides by 1000 to get
    // seconds. A rate would not need that division. Dead and mislabelled, so both go.
    let time_to_empty_ms = (i64::from(buffer_ms) * i64::from(requirement_kbps)) / i64::from(deficit);
    StarvationHorizon { seconds: u32::try_from(time_to_empty_ms / 1_000).ok() }
}

/// **[`starvation_horizon`] run backwards: how long a surplus takes to REFILL a spent reserve.**
///
/// Same algebra, opposite sign. Playback consumes one millisecond of media per millisecond of wall
/// clock while the link delivers `C/R` of it, so the reserve grows at `(C - R)/R` per wall
/// millisecond and closing a gap of `cost_ms` takes
///
/// ```text
/// t_refill = cost_ms * R / (C - R)      [ms]
/// ```
///
/// **`None` when `C <= R`, and that is the useful half.** A link with no surplus never repays the
/// gap, so a guard built on this correctly refuses to release on the clock and must wait for the
/// evidence to change instead. The same structural protection [`starvation_horizon`] has, for the
/// same reason: the quantity is undefined rather than large, and saying so beats returning a
/// number that reads as an answer.
///
/// **It introduces no constant.** `cost_ms` is [`crate::abr::viability::upshift_transaction_cost`]
/// — itself the sum of two deadlines that already exist — and `R`/`C` are the rung's rate and the
/// measured one. This is the whole derivation of `reject_backoff_ms`, which
/// `docs/adaptive-playback-plan.md` §6.2 records as "TBD from `E_tx`".
pub(crate) fn refill_time_ms(cost_ms: i64, requirement_kbps: u32, capacity_kbps: u32) -> Option<i64> {
    if capacity_kbps <= requirement_kbps || cost_ms <= 0 {
        return None;
    }
    let surplus = i64::from(capacity_kbps - requirement_kbps);
    Some(cost_ms.saturating_mul(i64::from(requirement_kbps)) / surplus.max(1))
}

/// **The physically reachable reserve at a given pair of elementary rates** — N3, and the quantity
/// `B* = 10 s` was written without.
///
/// Two lanes feed one playable reserve. The demux thread blocks on either lane's byte cap
/// (`aq.rs`), the pump throttles video to `MAX_FEED_AHEAD_NS` ahead of the presented position and
/// audio to that plus `AUDIO_SLACK_NS` (`player/engine.rs`), and the controller sees the minimum.
/// So
///
/// ```text
/// B_max(R_v, R_a) = min( video_lead + video_queue_bits / R_v ,
///                        audio_lead + audio_queue_bits / R_a )   [ms]
/// ```
///
/// **`kbps` IS bits per millisecond, so `bits / kbps` is ALREADY milliseconds.** There is no scale
/// factor in this function and a `* 1000` anywhere in it is the defect the first draft of the plan
/// shipped — it survived review because the reviewer's expected value came from the same
/// expression.
///
/// **Every input is read from `player::engine` at run time**, never transcribed: `aq_caps()` for
/// the byte caps and `feed_leads_ms()` for the two throttles. `abr/sim.rs` deliberately keeps its
/// own copy of the same geometry, sourced by VALUE with a comment, so that the plant grading this
/// controller is not the controller agreeing with itself — and
/// `sim::tests::the_plant_constants_still_match_the_pipeline` is what keeps the two honest.
///
/// **This is a MODEL, and the device census is what says it is a good one**: seven pinned rungs,
/// every prediction within 5% of the `buf=` the television settled at
/// (`sim::tests::the_calibration_reproduces_the_device_census`). It shares no term with that
/// measurement.
pub(crate) fn b_max_est_ms(video_es_kbps: u32, audio_es_kbps: u32) -> i64 {
    let (video_bytes, audio_bytes) = crate::player::aq_caps();
    let (video_lead_ms, audio_lead_ms) = crate::player::feed_leads_ms();
    let bits = |bytes: i64| u64::try_from(bytes).unwrap_or(0).saturating_mul(8);
    // `.max(1)` on both divisors: a rate of zero is reachable (an audio-less stream, or an ES rate
    // that has not been measured yet) and `overflow-checks` is ON under `cargo test` and OFF in
    // release, so a bare division panics on the Mac and traps on the television.
    let video = video_lead_ms
        .saturating_add((bits(video_bytes) / u64::from(video_es_kbps.max(1))).min(i64::MAX as u64) as i64);
    let audio = audio_lead_ms
        .saturating_add((bits(audio_bytes) / u64::from(audio_es_kbps.max(1))).min(i64::MAX as u64) as i64);
    video.min(audio)
}

/// **The reserve we ASK for at a given rate** — `B*(R)` — and why it is not a constant.
///
/// `B* = min(buffer_target_ms, alpha * B_max_est(R))`. The second term is what stops the target
/// from being a promise the plant cannot keep: asking for 10 s at the top of the ladder, where the
/// byte caps top out under 6 s, would make the deficit permanently positive and install a
/// rung-dependent haircut of 0.67-0.71 of the measured link — larger and more hidden than the
/// explicit 0.8 this whole effort exists to make visible.
///
/// At the shipped `buffer_target_ms = 2 500` the first term binds on eleven of thirteen rungs, so
/// alpha is INERT almost everywhere. That is the intended shape for landing: the corrected formula
/// arrives without moving an expected value, and M4 decides whether either number moves.
pub(crate) fn buffer_target_at_ms(
    video_es_kbps: u32,
    audio_es_kbps: u32,
    policy: &AbrPolicy,
) -> i64 {
    let reachable = b_max_est_ms(video_es_kbps, audio_es_kbps)
        .saturating_mul(i64::from(policy.buffer_reserve_fraction_pm))
        / 1_000;
    policy.buffer_target_ms.min(reachable)
}

/// **The per-candidate refill filter** — N3's `R_j <= R_max_j`, and the resolution of an ambiguity
/// the previous plan carried: `R` appears on BOTH sides of the algebra, so a single scalar budget
/// compared against every candidate is not well defined. It is evaluated per candidate.
///
/// ```text
/// D_j     = max(0, B*(R_j) - B)                        the deficit against THIS candidate's target
/// R_max_j = C_safe * H / (H + D_j)                     what may be spent while still closing it
/// ```
///
/// The reading: a candidate that leaves the reserve short has to leave room to refill that
/// shortfall inside the horizon `H`, and the rate it may claim shrinks in proportion. With no
/// deficit `D_j = 0` and `R_max_j = C_safe` exactly — the filter is then the identity, which is
/// the state every healthy playback is in and the reason this lands without moving anything.
///
/// **Integer, and associated so it cannot divide first.** `C_safe * H` before the division by
/// `H + D_j`; at `C_safe = 20 000` and `H = 10 000` that is 2e8, nowhere near `i64`.
///
/// **The monotonicity obligation, because the predicate is not trivially well-posed.**
/// `B_max_est` decreases in `R`, so `D_j` decreases in `R_j`, so `R_max_j` INCREASES in `R_j` —
/// both sides of `R_j <= R_max_j` move the same way, which is the shape that can admit a scattered
/// set rather than a prefix of the ladder. It is a prefix today only because `B*` is capped at
/// `buffer_target_ms`, which pins `R_max_j` into `[0.8*C_safe, C_safe]` at `H = 10 s`. That is a
/// property of the current numbers, not of the form, so
/// `tests::the_refill_filter_admits_a_prefix_of_the_ladder` asserts it over a magnitude sweep and
/// is what protects it when `buffer_target_ms` moves after M4.
///
/// **One limitation, stated rather than fixed by mixing dimensions.** `C_safe` is measured over
/// `active_fetch_us`, which EXCLUDES PMS production time, while "close the deficit within `H`" is
/// a wall-clock promise. So the guarantee over-promises by exactly the factor N6 forbids folding
/// in — production is an independent feasibility constraint and stays one.
pub(crate) fn refill_admits(
    candidate_wire_kbps: u32,
    candidate_video_es_kbps: u32,
    audio_es_kbps: u32,
    buffered_ms: i64,
    safe_budget_kbps: u32,
    policy: &AbrPolicy,
) -> bool {
    let target = buffer_target_at_ms(candidate_video_es_kbps, audio_es_kbps, policy);
    let deficit = (target - buffered_ms).max(0);
    let horizon = policy.buffer_refill_horizon_ms.max(1);
    let r_max = i64::from(safe_budget_kbps).saturating_mul(horizon) / horizon.saturating_add(deficit);
    i64::from(candidate_wire_kbps) <= r_max
}
