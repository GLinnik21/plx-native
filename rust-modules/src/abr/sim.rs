//! **The closed-loop plant the controller is measured against** — host-only, `#[cfg(test)]`, and
//! deliberately small. Increment I0-B/C of `docs/adaptive-playback-plan.md`.
//!
//! # Why this exists
//!
//! Every host test in `abr.rs` hands the controller a `SegmentSample` somebody wrote by hand, so it
//! grades the controller against a world the test author invented. Nothing checks that the world is
//! reachable: eight existing tests pass 20-60 s reserves against 28-60 Mbit/s sources, where the
//! byte-capped AU queues cannot hold more than about four seconds (`§8.7` of the plan). This module
//! closes the loop — the controller's own decisions choose the next rung, the plant decides what
//! that rung costs, and the buffer is whatever the two of them produce.
//!
//! # The independence rule, which is the whole point
//!
//! **The plant must not call the controller's arithmetic.** If `B_max` here were
//! `abr::b_max_est(...)`, every reachability test would be the implementation agreeing with itself,
//! and the defect this module was built to catch — a formula off by a factor of 1000 — would pass.
//! So [`Plant`] carries queue sizes and feed leads as its own explicit parameters, sourced from
//! `player::engine`'s constants by VALUE with a comment, and computes its ceiling itself. When the
//! controller eventually grows a `B_max_est` of its own (increment I5), the two must agree — and
//! *that disagreement is a test*, not a refactor.
//!
//! # The plant
//!
//! Two lanes feed one playable reserve. `aq_push` blocks the single demux thread on either lane's
//! byte cap (`aq.rs`), the pump throttles the video lane to `MAX_FEED_AHEAD_NS` ahead of the
//! presented position and the audio lane to that plus `AUDIO_SLACK_NS` (`player/engine.rs`), and
//! the controller sees `min(video, audio)` (`BufferSnapshot::buffered_ms`). So
//!
//! ```text
//! B_max(R_video, R_audio) = min( video_lead_ms + video_queue_bits / R_video ,
//!                                audio_lead_ms + audio_queue_bits / R_audio )
//! ```
//!
//! **Dimensions.** `kbps` is kilobits per second, which is bits per millisecond. So `bits / kbps`
//! is **already milliseconds** and there is no `* 1000` anywhere in this file. That factor is
//! exactly the bug the first draft of the plan shipped, and it survived review because the
//! reviewer's expected value came from the same expression.
//!
//! # State equations
//!
//! Per segment of content duration `d` ms at delivered media rate `R` kbps over capacity `C` kbps:
//!
//! ```text
//! bytes      = R_ts · d / 8                    (kbps · ms = bits)
//! fetch_ms   = R_ts · d / C                    (bits / kbps = ms)   ← transfer only
//! acquire_ms = fetch_ms + overhead_ms                               ← request → demux complete
//! wall_ms    = max(acquire_ms, B + d - B_max)                       ← backpressure blocks demux
//! stall_ms   = max(0, wall_ms - B)
//! B'         = min(B_max, max(0, B - wall_ms) + d)
//! ```
//!
//! **`acquire_ms` and `wall_ms` are different quantities and the split is load-bearing.** The
//! controller is told ACQUISITION (`total_fetch_us`), because that is what production stamps; the
//! PLANT advances by WALL, because that is what actually elapsed. Conflating them is the defect
//! this module shipped with — see [`step`].
//!
//! The `max(0, ...)` on the reserve and the `stall_ms` line are the same event seen twice: the
//! buffer cannot go negative, and the wall time it could not cover is a stall. The segment is
//! modelled as arriving atomically at the end of its fetch, which is the one deliberate
//! simplification here — it makes a stall at most one segment pessimistic and never optimistic.
//!
//! Differentially, while nothing is blocked or empty, `dB/dt = C/R - 1`, which
//! [`the identity test`](tests::the_plant_reproduces_the_drain_identity) pins directly.
//!
//! # A transaction is not free
//!
//! On `Decision::Prime` the demux worker runs the candidate transaction **inline on its own loop**:
//! the current stream is not read while it runs and candidate segments are fed only after the
//! commit (`ff.rs`'s prime arm). So a prime costs wall time with **no fill**, and a rejected one
//! costs it for nothing. That cost is a [`TransactionModel`] split four ways — up/down x
//! commit/reject, each with its control-plane and media legs — and every leg is `Option`, because
//! none of them is measured yet. [`run`] REFUSES an unmeasured leg rather than substituting a
//! constant: one flat number for all four made `T_down` growing on a collapsing link
//! unrepresentable.
//!
//! # Determinism
//!
//! Virtual time only. No `Instant`, no sleep, no thread. The same trace and the same plant produce
//! the same [`Report`] on every machine, which is what makes a two-parameter-set A/B meaningful.

use super::{BufferSnapshot, Controller, Decision, Direction, MediaTimeMs, Rung, SegmentSample};

/// Video AU queue byte cap. `player::engine::AQ_VIDEO_BYTES` = `8 * 1024 * 1024`, carried by value
/// because this plant must not depend on the app's own model of itself (see the module note). If
/// that constant moves, [`tests::the_plant_constants_still_match_the_pipeline`] fails.
pub(super) const VIDEO_QUEUE_BYTES: u64 = 8 * 1024 * 1024;
/// Audio AU queue byte cap. `player::engine::AQ_AUDIO_BYTES` = `1024 * 1024`.
pub(super) const AUDIO_QUEUE_BYTES: u64 = 1024 * 1024;
/// Video feed-ahead throttle. `player::engine::MAX_FEED_AHEAD_NS` = 1.6 s.
pub(super) const VIDEO_LEAD_MS: i64 = 1_600;
/// Audio feed-ahead throttle: `MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS` = 1.6 s + 2.0 s.
pub(super) const AUDIO_LEAD_MS: i64 = 3_600;

/// **One measured operating point.** Three DIFFERENT rates, and conflating any two of them is how
/// this plant was wrong before.
///
/// * `ts_kbps` — R_ts, what the NETWORK must carry: the muxed transport stream, container overhead
///   included. It sets acquisition time.
/// * `video_es_kbps` — R_video_es, what the VIDEO AU queue holds: demuxed elementary bytes, no
///   container. It sets the video lane's ceiling.
/// * `audio_es_kbps` — R_audio_es, the same for the audio lane, whose queue is an eighth the size
///   and whose rate does not move with the ladder.
/// * `overhead_ms` — the non-transfer part of acquisition at this point (connect, request, TTFB,
///   JIT production), MEASURED as `A - active`, not assumed.
///
/// A catalog request rate is none of these. `expected_wire_kbps` is 8.4% above the delivered TS
/// rate at P1080High and 92% BELOW it at P480, so deriving a queue ceiling from it is a different
/// number in both directions.
#[derive(Clone, Copy, Debug)]
pub(super) struct OperatingPoint {
    pub(super) ts_kbps: u32,
    pub(super) video_es_kbps: u32,
    pub(super) audio_es_kbps: u32,
    pub(super) overhead_ms: i64,
}

/// What one candidate transaction costs, split the four ways the device can distinguish. Every
/// field is wall milliseconds of playback that is NOT being refilled.
///
/// None of these is measured yet — increment I2's instrumentation exists to fill them — so this
/// type is deliberately awkward to fabricate: [`TransactionModel`] holds `Option`s and [`run`]
/// REFUSES rather than substituting a number.
#[derive(Clone, Copy, Debug)]
pub(super) struct TransactionCost {
    /// `control.prime` + master playlist + media playlist. Covered by NO deadline in `ff.rs`.
    pub(super) control_plane_ms: i64,
    /// Acquisition of the candidate's cold first segment.
    pub(super) warmup_acq_ms: i64,
    /// Acquisition of the graded segment, where one is fetched at all.
    pub(super) graded_acq_ms: i64,
}

impl TransactionCost {
    pub(super) fn total_ms(&self) -> i64 {
        self.control_plane_ms
            .saturating_add(self.warmup_acq_ms)
            .saturating_add(self.graded_acq_ms)
    }
}

/// The four legs a transaction can take. An absent leg is an ADMISSION THAT IT WAS NEVER MEASURED,
/// and [`run`] returns an error the moment it needs one — a normative simulation may not run on a
/// fabricated transaction cost. The previous plant charged one flat 4600 ms constant to all four,
/// which made `T_down` growing on a collapsing link literally unrepresentable.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TransactionModel {
    pub(super) up_commit: Option<TransactionCost>,
    pub(super) up_reject: Option<TransactionCost>,
    pub(super) down_commit: Option<TransactionCost>,
    pub(super) down_reject: Option<TransactionCost>,
}

impl TransactionModel {
    /// Nothing measured. Legal only for a trace that never primes.
    pub(super) fn unmeasured() -> Self {
        Self::default()
    }

    fn leg(&self, direction: Direction, commit: bool) -> Option<TransactionCost> {
        match (direction, commit) {
            (Direction::Up, true) => self.up_commit,
            (Direction::Up, false) => self.up_reject,
            (Direction::Down, true) => self.down_commit,
            (Direction::Down, false) => self.down_reject,
        }
    }
}

/// The physical pipeline, as parameters. Nothing here is policy and nothing here is read from
/// [`super::AbrPolicy`].
#[derive(Clone, Copy, Debug)]
pub(super) struct Plant {
    pub(super) video_queue_bytes: u64,
    pub(super) audio_queue_bytes: u64,
    pub(super) video_lead_ms: i64,
    pub(super) audio_lead_ms: i64,
    /// Content duration of one segment, ms. The client asks PMS for 2 s.
    pub(super) segment_ms: i64,
}

impl Default for Plant {
    fn default() -> Self {
        Self {
            video_queue_bytes: VIDEO_QUEUE_BYTES,
            audio_queue_bytes: AUDIO_QUEUE_BYTES,
            video_lead_ms: VIDEO_LEAD_MS,
            audio_lead_ms: AUDIO_LEAD_MS,
            segment_ms: 2_000,
        }
    }
}

impl Plant {
    /// **The reachable reserve, from queue geometry and MEASURED elementary rates.** The
    /// independent oracle.
    ///
    /// Takes the two ES rates directly: nothing here derives a lane rate from a wire rate or from
    /// a catalog entry. Widened to `u64`/`i64` throughout — a LAN observation of 865 Gbit/s is on
    /// record in `abr.rs`, release builds have `overflow-checks` off and host tests have them on,
    /// and an expression that panics on one and wraps on the other is two different models.
    pub(super) fn b_max_ms(&self, point: &OperatingPoint) -> i64 {
        let video_bits = self.video_queue_bytes.saturating_mul(8);
        let audio_bits = self.audio_queue_bytes.saturating_mul(8);
        // bits / kbps = ms. No scale factor. See the module note.
        let video = self.video_lead_ms.saturating_add(
            (video_bits / u64::from(point.video_es_kbps.max(1))).min(i64::MAX as u64) as i64,
        );
        let audio = self.audio_lead_ms.saturating_add(
            (audio_bits / u64::from(point.audio_es_kbps.max(1))).min(i64::MAX as u64) as i64,
        );
        video.min(audio)
    }
}

/// A capacity schedule in virtual time: `(until_ms, kbps)`, last leg extends forever.
#[derive(Clone, Debug)]
pub(super) struct Trace(Vec<(i64, u32)>);

impl Trace {
    /// `[(seconds, kbps), ...]` — the same shape `tests/manifest.json`'s `network_profile` uses.
    pub(super) fn new(legs: &[(f64, u32)]) -> Self {
        Self(legs.iter().map(|&(s, k)| ((s * 1000.0) as i64, k.max(1))).collect())
    }

    pub(super) fn capacity_kbps(&self, now_ms: i64) -> u32 {
        self.0
            .iter()
            .find(|&&(until, _)| now_ms < until)
            .map(|&(_, k)| k)
            .unwrap_or_else(|| self.0.last().map(|&(_, k)| k).unwrap_or(1))
    }

    pub(super) fn horizon_ms(&self) -> i64 {
        self.0.last().map(|&(u, _)| u).unwrap_or(0)
    }
}

/// **Measured operating points, from the I1 device census** (`docs/measurements/`).
///
/// Three rungs only. Everything else on the thirteen-point ladder is UNCALIBRATED, and
/// [`Calibration::point`] returns `None` for it rather than interpolating — a normative simulation
/// may not run on a fabricated operating point, which is the whole reason the flat
/// `overhead_ms: 120` this replaced had to go.
///
/// **One derived quantity, flagged as such.** `video_es_kbps` is `(ts - audio) / 1.04`: the AU
/// queues hold demuxed ELEMENTARY bytes while the census measured the TS wire, and 1.04 is an
/// assumed transport-stream overhead. It is an assumption, not a measurement, and increment I2's
/// per-lane ES byte counters replace it. `ts_kbps`, `audio_es_kbps` (ffprobe) and `overhead_ms`
/// (`A - active`) are measured.
pub(super) struct Calibration;

impl Calibration {
    pub(super) fn point(rung_request_kbps: u32) -> Option<OperatingPoint> {
        let (ts, audio, overhead) = match rung_request_kbps {
            720 => (1_381u32, 131u32, 38i64),
            4_000 => (3_183, 160, 63),
            20_000 => (18_456, 192, 281),
            _ => return None,
        };
        Some(OperatingPoint {
            ts_kbps: ts,
            video_es_kbps: (u64::from(ts - audio) * 100 / 104) as u32,
            audio_es_kbps: audio,
            overhead_ms: overhead,
        })
    }
}

/// One observed segment, as the plant produced it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Observed {
    pub(super) at_ms: i64,
    pub(super) rung_kbps: u32,
    pub(super) capacity_kbps: u32,
    pub(super) ts_kbps: u32,
    /// Acquisition time — request to demux complete. NOT wall time.
    pub(super) acquire_ms: i64,
    /// Wall time the plant advanced, backpressure included.
    pub(super) wall_ms: i64,
    pub(super) buf_ms: i64,
    pub(super) b_max_ms: i64,
    pub(super) stall_ms: i64,
}

/// What a run produced.
#[derive(Clone, Debug, Default)]
pub(super) struct Report {
    pub(super) samples: Vec<Observed>,
    pub(super) commits: Vec<(i64, u32)>,
    pub(super) primes: u32,
    pub(super) rejects: u32,
    pub(super) stall_ms_total: i64,
    pub(super) stall_ms_max: i64,
    /// Wall time spent inside transactions, split the way the device can distinguish it.
    pub(super) tx_control_plane_ms: i64,
    pub(super) tx_media_ms: i64,
    pub(super) first_decision: Option<Decision>,
    pub(super) first_buf_ms: i64,
}

impl Report {
    pub(super) fn min_buf_ms(&self) -> i64 {
        self.samples.iter().map(|s| s.buf_ms).min().unwrap_or(0)
    }

    pub(super) fn final_rung_kbps(&self) -> u32 {
        self.samples.last().map(|s| s.rung_kbps).unwrap_or(0)
    }

    pub(super) fn visited_kbps(&self) -> Vec<u32> {
        self.samples.iter().map(|s| s.rung_kbps).collect()
    }
}

/// Fetch one segment and advance the plant.
///
/// **The measurement split this function exists to keep honest:**
/// `active_fetch_us` is transfer only; `total_fetch_us` is ACQUISITION — request to demux
/// complete — and is what production stamps (`ff.rs`, `total_us` from `request_started`, taken
/// before `hls_feed_segment` blocks on `aq_push`). The plant's own clock advances by WALL time,
/// which includes queue backpressure. Passing wall time as `total_fetch_us` is the defect this
/// module shipped with: in the settled state `blocked_until` is exactly `d`, so the controller
/// read `production_ratio_pm = 1000` at every rung and every link speed — against a device 225-318
/// — which permanently failed the upshift gate and made the plant veto the very behaviour it was
/// built to study.
fn step(
    plant: &Plant,
    trace: &Trace,
    point: &OperatingPoint,
    now_ms: &mut i64,
    buf_ms: &mut i64,
    rung_kbps: u32,
) -> (Observed, SegmentSample) {
    let d = plant.segment_ms;
    let capacity = trace.capacity_kbps(*now_ms);
    let b_max = plant.b_max_ms(point);

    // The NETWORK carries the transport stream: bits = kbps * ms.
    let bits = u64::from(point.ts_kbps).saturating_mul(d.max(0) as u64);
    let fetch_ms = (bits / u64::from(capacity.max(1))).min(i64::MAX as u64) as i64;
    let acquire_ms = fetch_ms.saturating_add(point.overhead_ms).max(1);

    // Backpressure: the demuxer blocks rather than exceeding the queue cap, so WALL time stretches.
    let blocked_until = buf_ms.saturating_add(d).saturating_sub(b_max);
    let wall_ms = acquire_ms.max(blocked_until).max(1);

    let stall_ms = (wall_ms - *buf_ms).max(0);
    let drained = (*buf_ms - wall_ms).max(0);
    *buf_ms = (drained + d).min(b_max);
    *now_ms += wall_ms;

    let observed = Observed {
        at_ms: *now_ms,
        rung_kbps,
        capacity_kbps: capacity,
        ts_kbps: point.ts_kbps,
        acquire_ms,
        wall_ms,
        buf_ms: *buf_ms,
        b_max_ms: b_max,
        stall_ms,
    };
    let snapshot = BufferSnapshot {
        playback: MediaTimeMs(0),
        video_tail: MediaTimeMs(*buf_ms),
        audio_tail: Some(MediaTimeMs(*buf_ms)),
        audio_expected: true,
    };
    let sample = SegmentSample::new(
        (bits / 8).max(1),
        (fetch_ms.max(1) as u64).saturating_mul(1_000),
        // ACQUISITION, never wall. See this function's doc.
        (acquire_ms as u64).saturating_mul(1_000),
        u32::try_from(d).unwrap_or(2_000),
        snapshot,
    )
    .expect("plant produced a degenerate segment");
    (observed, sample)
}

/// Run `controller` against `trace` on `plant`.
///
/// Returns `Err` rather than fabricating anything: an uncalibrated rung or an unmeasured
/// transaction leg stops the run, because a normative simulation built on an invented operating
/// point or an invented transaction cost grades the invention.
pub(super) fn run(
    plant: &Plant,
    trace: &Trace,
    controller: &mut Controller,
    tx: &TransactionModel,
) -> Result<Report, String> {
    let mut report = Report::default();
    let mut now_ms: i64 = 0;
    let mut buf_ms: i64 = 0;
    let horizon = trace.horizon_ms();

    let point_for = |rung: Rung| -> Result<OperatingPoint, String> {
        Calibration::point(rung.kbps())
            .ok_or_else(|| format!("rung {}kbps is UNCALIBRATED — measure it before simulating it", rung.kbps()))
    };

    while now_ms < horizon {
        let rung = controller.current();
        let point = point_for(rung)?;
        let (observed, sample) = step(plant, trace, &point, &mut now_ms, &mut buf_ms, rung.kbps());
        report.stall_ms_total += observed.stall_ms;
        report.stall_ms_max = report.stall_ms_max.max(observed.stall_ms);
        report.samples.push(observed);

        let decision = controller.observe(sample);
        if report.first_decision.is_none() {
            report.first_decision = Some(decision);
            report.first_buf_ms = observed.buf_ms;
        }
        let Decision::Prime(proposal) = decision else { continue };
        report.primes += 1;
        let cand_point = point_for(proposal.rung)?;

        // Decide the outcome FIRST, because the cost of a transaction differs by outcome and the
        // plant may not charge one number for all four legs.
        let capacity = trace.capacity_kbps(now_ms);
        let bits = u64::from(cand_point.ts_kbps).saturating_mul(plant.segment_ms.max(0) as u64);
        let cand_fetch = (bits / u64::from(capacity.max(1))).min(i64::MAX as u64) as i64;
        let cand_acquire = cand_fetch.saturating_add(cand_point.overhead_ms).max(1);

        let would_commit = {
            let probe = SegmentSample::new(
                (bits / 8).max(1),
                (cand_fetch.max(1) as u64).saturating_mul(1_000),
                (cand_acquire as u64).saturating_mul(1_000),
                u32::try_from(plant.segment_ms).unwrap_or(2_000),
                BufferSnapshot {
                    playback: MediaTimeMs(0),
                    video_tail: MediaTimeMs(buf_ms),
                    audio_tail: Some(MediaTimeMs(buf_ms)),
                    audio_expected: true,
                },
            )
            .expect("plant produced a degenerate candidate segment");
            controller.candidate_ready(proposal, probe)
        };
        let cost = tx.leg(proposal.direction, would_commit).ok_or_else(|| {
            format!(
                "transaction leg {:?}/{} is UNMEASURED — increment I2 exists to measure it",
                proposal.direction,
                if would_commit { "commit" } else { "reject" },
            )
        })?;

        // The current stream is not read for the whole transaction and nothing is fed until
        // commit (`ff.rs`'s prime arm), so the reserve drains for all of it and a REJECT costs the
        // same as a commit.
        let spent = cost.total_ms();
        let tx_stall = (spent - buf_ms).max(0);
        buf_ms = (buf_ms - spent).max(0);
        now_ms += spent;
        report.stall_ms_total += tx_stall;
        report.stall_ms_max = report.stall_ms_max.max(tx_stall);
        report.tx_control_plane_ms += cost.control_plane_ms;
        report.tx_media_ms += cost.warmup_acq_ms.saturating_add(cost.graded_acq_ms);

        if would_commit && controller.commit(proposal) {
            buf_ms = buf_ms
                .saturating_add(plant.segment_ms)
                .min(plant.b_max_ms(&cand_point));
            report.commits.push((now_ms, proposal.rung.kbps()));
        } else {
            controller.reject(proposal);
            report.rejects += 1;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::super::Rung;
    use super::*;

    fn cat() -> super::super::HlsActuatorCatalog {
        super::super::HlsActuatorCatalog::measured()
    }

    /// The three census points, so a test reads like the device record.
    fn p720() -> OperatingPoint { Calibration::point(720).expect("720 is calibrated") }
    fn p4000() -> OperatingPoint { Calibration::point(4_000).expect("4000 is calibrated") }
    fn p20000() -> OperatingPoint { Calibration::point(20_000).expect("20000 is calibrated") }

    /// MATHEMATICAL INVARIANT.
    ///
    /// **The regression test for the defect this module shipped with.** In a settled, backpressured
    /// state `blocked_until` is exactly `d`, so wall time is `d` and a plant that passed wall time
    /// as `total_fetch_us` reported `production_ratio_pm = 1000` at every rung and every link
    /// speed. The controller's upshift gate is `ratio_pm <= 750`, so that plant vetoed upshifting
    /// out of any settled reserve — the exact behaviour it was built to study.
    ///
    /// This asserts the two quantities are DIFFERENT and that the controller is told the smaller
    /// one. It is differential: it cannot pass against the old expression.
    #[test]
    fn the_controller_is_told_acquisition_time_and_the_plant_advances_by_wall_time() {
        let plant = Plant::default();
        let point = p20000();
        // 103 Mbit/s: the link the I1 census actually ran on, so `prod` below is comparable to
        // the device reading rather than to an arbitrary fast link (prod scales with fetch time).
        let trace = Trace::new(&[(600.0, 103_000)]);
        let mut now = 0;
        let mut buf = plant.b_max_ms(&point);          // settled AT the ceiling: fully backpressured
        let before = now;
        let (observed, sample) = step(&plant, &trace, &point, &mut now, &mut buf, 20_000);

        assert_eq!(observed.wall_ms, plant.segment_ms, "settled: wall time is one segment");
        assert!(observed.acquire_ms < observed.wall_ms,
                "acquisition {}ms must be strictly under wall {}ms in a backpressured state",
                observed.acquire_ms, observed.wall_ms);
        assert_eq!(now - before, observed.wall_ms, "the plant advances by WALL time");
        // What the controller reads:
        let prod = sample.production_ratio_pm();
        assert_eq!(prod as i64, observed.acquire_ms * 1_000 / plant.segment_ms);
        assert!(prod < 1_000, "wall time would read 1000pm — the defect — got {prod}");
        // And on the census link it REPRODUCES the device reading at this rung: 313-318 pm
        // measured, 319 computed (fetch 358 + overhead 281 = 639 ms over a 2000 ms segment).
        assert!((300..=340).contains(&prod), "expected the device band 313-318, got {prod}pm");
    }

    /// DEVICE-FINDING REGRESSION.
    ///
    /// The calibrated points reproduce the I1 census within 5%. Predictions come from queue
    /// geometry and the ES rates; the observed medians are the device's
    /// (`docs/measurements/i1-abr-baseline.md`). Neither is derived from the other.
    #[test]
    fn the_calibration_reproduces_the_device_census() {
        let plant = Plant::default();
        for (point, observed_median, lane) in [
            (p720(), 59_001i64, "video"),
            (p4000(), 24_835, "video"),
            (p20000(), 5_335, "video"),
        ] {
            let pred = plant.b_max_ms(&point);
            let err = (pred - observed_median).abs() * 100 / observed_median;
            assert!(err <= 5, "{lane} lane: predicted {pred}ms vs measured {observed_median}ms ({err}% off)");
        }
    }

    /// MATHEMATICAL INVARIANT: three rates, and a lane ceiling never comes from a wire rate.
    ///
    /// Hand-computed from queue geometry; nothing here calls `b_max_ms` to build its expectation.
    /// video queue 8 MiB = 67 108 864 bits, audio queue 1 MiB = 8 388 608 bits, leads 1600 / 3600.
    ///   * 20000: video ES 17 561 -> 67 108 864/17 561 = 3821, +1600 = 5421; audio 192 -> 47 290
    ///   * 720:   video ES  1201  -> 67 108 864/1201  = 55 877, +1600 = 57 477; audio 131 -> 67 635
    #[test]
    fn the_lane_ceilings_come_from_elementary_rates() {
        let plant = Plant::default();
        assert_eq!(plant.b_max_ms(&p20000()), 5_421);
        assert_eq!(plant.b_max_ms(&p720()), 57_477);
        // The audio lane binds only when its own ceiling is the smaller one; at 131 kbps that is
        // 67 635 ms, above both video ceilings above — so the video lane binds at every measured
        // point, which is what the device showed (video 85/85, 70/70, 57/57).
        let audio_only = OperatingPoint { video_es_kbps: 200, ..p720() };
        assert_eq!(plant.b_max_ms(&audio_only), 3_600 + 8_388_608 / 131);
    }

    /// MATHEMATICAL INVARIANT: the plant's constants are still the pipeline's.
    #[test]
    fn the_plant_constants_still_match_the_pipeline() {
        let (video, audio) = crate::player::aq_caps();
        assert_eq!(video as u64, VIDEO_QUEUE_BYTES);
        assert_eq!(audio as u64, AUDIO_QUEUE_BYTES);
    }

    /// MATHEMATICAL INVARIANT: dB/dt = C/R_ts - 1, read off the plant.
    #[test]
    fn the_plant_reproduces_the_drain_identity() {
        let point = OperatingPoint { overhead_ms: 0, ..p4000() };
        let plant = Plant::default();
        for &(capacity, want) in &[(20_000u32, 1i8), (point.ts_kbps, 0), (1_500, -1)] {
            let trace = Trace::new(&[(600.0, capacity)]);
            // Deep enough that the deficit leg does not hit the zero floor, which would clamp the
            // delta and hide the identity being asserted.
            let (mut now, mut buf) = (0, 12_000);
            let before = buf;
            let (_, _) = step(&plant, &trace, &point, &mut now, &mut buf, 4_000);
            let fetch = i64::from(point.ts_kbps) * plant.segment_ms / i64::from(capacity);
            assert_eq!((buf - before).signum() as i8, want, "C={capacity}");
            assert_eq!(buf - before, plant.segment_ms - fetch.max(1), "C={capacity}");
        }
    }

    /// MATHEMATICAL INVARIANT: extreme magnitudes neither panic on the host (overflow-checks ON)
    /// nor wrap on the device (OFF). 865 Gbit/s is a real reading from this project's event log.
    #[test]
    fn the_plant_survives_the_magnitudes_a_lan_actually_produces() {
        let plant = Plant::default();
        for &es in &[1u32, 320, 22_000, 865_000_000, u32::MAX] {
            let point = OperatingPoint { ts_kbps: es, video_es_kbps: es, audio_es_kbps: es.max(1), overhead_ms: 0 };
            assert!(plant.b_max_ms(&point) > 0, "es={es}");
            let trace = Trace::new(&[(600.0, u32::MAX)]);
            let (mut now, mut buf) = (0, 0);
            let (o, _) = step(&plant, &trace, &point, &mut now, &mut buf, 320);
            assert!(o.buf_ms >= 0 && now > 0);
        }
    }

    /// INTEGRATION: a normative run REFUSES a rung nobody measured.
    #[test]
    fn an_uncalibrated_rung_stops_the_run_instead_of_being_invented() {
        assert!(Calibration::point(14_000).is_none(), "14000 was never measured");
        let plant = Plant::default();
        let trace = Trace::new(&[(30.0, 40_000)]);
        let mut c = Controller::starting_at(Rung::P1080M14, None, cat());
        let err = run(&plant, &trace, &mut c, &TransactionModel::unmeasured())
            .expect_err("an uncalibrated rung must not simulate");
        assert!(err.contains("UNCALIBRATED"), "{err}");
    }

    /// INTEGRATION: a normative run REFUSES an unmeasured transaction leg.
    ///
    /// This is what stops the plant scoring a transaction policy against a number somebody made
    /// up. It is expected to fail until increment I2 measures the four legs on the television.
    #[test]
    fn an_unmeasured_transaction_leg_stops_the_run() {
        let plant = Plant::default();
        let trace = Trace::new(&[(400.0, 60_000)]);
        // Pinned to a CALIBRATED rung, so the run stops on the transaction leg and not on an
        // uncalibrated operating point — the two refusals must be distinguishable.
        let mut c = Controller::starting_at(Rung::P480, None, cat()).pinned_to(Some(Rung::P720));
        let err = run(&plant, &trace, &mut c, &TransactionModel::unmeasured())
            .expect_err("an unmeasured transaction leg must not simulate");
        assert!(err.contains("UNMEASURED"), "{err}");
    }

    /// INTEGRATION: with every leg supplied the loop closes and is deterministic.
    #[test]
    fn the_loop_closes_and_is_deterministic_on_a_fully_specified_model() {
        let leg = TransactionCost { control_plane_ms: 300, warmup_acq_ms: 900, graded_acq_ms: 700 };
        let tx = TransactionModel {
            up_commit: Some(leg), up_reject: Some(leg), down_commit: Some(leg), down_reject: Some(leg),
        };
        let plant = Plant::default();
        let trace = Trace::new(&[(200.0, 60_000)]);
        let mut a = Controller::starting_at(Rung::P480, None, cat()).pinned_to(Some(Rung::P720));
        let mut b = Controller::starting_at(Rung::P480, None, cat()).pinned_to(Some(Rung::P720));
        let ra = run(&plant, &trace, &mut a, &tx).expect("calibrated");
        let rb = run(&plant, &trace, &mut b, &tx).expect("calibrated");
        assert_eq!(ra.visited_kbps(), rb.visited_kbps());
        assert_eq!(ra.stall_ms_total, rb.stall_ms_total);
        assert!(ra.samples.iter().all(|s| s.buf_ms <= s.b_max_ms));
        assert!(ra.min_buf_ms() >= 0 && ra.final_rung_kbps() > 0);
        // The three fields a device trace is compared against: time advances, the trace is
        // followed, and the TS rate carried is the calibrated one rather than a catalog entry.
        assert!(ra.samples.windows(2).all(|w| w[1].at_ms > w[0].at_ms), "time went backwards");
        assert!(ra.samples.iter().all(|s| s.capacity_kbps == 60_000));
        // Every sample carries the CALIBRATED transport rate of the rung it was taken at — never
        // the catalog's request rate, which is 92% low at P480 and 8.4% high at P1080High.
        assert!(
            ra.samples.iter().all(|s| {
                Calibration::point(s.rung_kbps).map(|p| p.ts_kbps) == Some(s.ts_kbps)
            }),
            "a sample carried a rate that is not its rung's calibrated one",
        );
    }

    /// CHARACTERISATION / BASELINE — not a policy assertion.
    ///
    /// Plan §0.3(1): one segment of reserve trips `starving()`, so the first segment of every
    /// playback may propose a downshift on any link. Asserted here is only the STRUCTURAL
    /// precondition; the decision is recorded, never graded, because increment I3 is expected to
    /// change it and this test must keep passing when it does.
    #[test]
    fn characterise_the_first_segment_of_a_fast_link() {
        let plant = Plant::default();
        let trace = Trace::new(&[(60.0, 400_000)]);
        let point = p720();
        let (mut now, mut buf) = (0, 0);
        let (o, sample) = step(&plant, &trace, &point, &mut now, &mut buf, 720);
        assert!(o.buf_ms <= plant.segment_ms, "one segment in, the reserve is one segment");
        let mut c = Controller::starting_at(Rung::P480, None, cat());
        let decision = c.observe(sample);
        println!(
            "CHARACTERISATION first-segment: buf={}ms acquire={}ms prod={}pm decision={:?}",
            o.buf_ms, o.acquire_ms, sample.production_ratio_pm(), decision,
        );
    }
}
