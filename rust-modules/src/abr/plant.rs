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
        let delta = buffered_ms - self.buffered_ms;
        self.last_delta_ms = if self.samples == 0 { 0 } else { delta };
        let sample_slope = (delta * 1_000) / media_duration_ms;
        self.slope_ms_per_s = if self.samples == 0 {
            sample_slope
        } else {
            (self.slope_ms_per_s * 3 + sample_slope) / 4
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

