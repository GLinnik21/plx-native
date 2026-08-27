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

    /// **The three legs the device has actually produced**, generated by
    /// `tools/abr-calibrate-plant.py --rust-tx` from `abr: tx` across every committed capture and
    /// gated by `tools/test_abr_calibrate_plant.py`. Medians, milliseconds.
    ///
    /// **Until this existed the closed-loop plant could not run a single trace that changed rung**
    /// — [`run`] refuses an absent leg, and all four were absent. That is why every simulator test
    /// in this file either pinned a rung or asserted the refusal itself: the one instrument capable
    /// of comparing two policies without a television had never executed a transaction.
    ///
    /// **`down_reject` stays `None` and that is structural, not an oversight.**
    /// `Controller::candidate_ready` returns `true` for every downshift that produced a decodable
    /// segment and left one segment of reserve, so reaching a down-reject needs a decode or raster
    /// failure. n = 0 across 45 captured logs. Inventing a number here is exactly what the previous
    /// plant did — one flat 4600 ms charged to all four legs — which made `T_down` growing on a
    /// collapsing link literally unrepresentable.
    ///
    /// **`control_plane_ms: 6` is the FIXTURE SERVER's control plane, not a PMS's.** These captures
    /// come from the synthetic pipeline tier, where `tests/serve_fixtures.py` answers a playlist off
    /// local disk. A real PMS is 13-18 ms warm (`docs/measurements/p2h-pms-ladder.md` §5) and can be
    /// far worse cold. So a simulation run on this model understates the control plane of a
    /// deployment by roughly 2-3x, and any conclusion that turns on transaction cost has to say so.
    pub(super) fn measured() -> Self {
        Self {
            up_commit: Some(TransactionCost {
                control_plane_ms: 6,
                warmup_acq_ms: 1185,
                graded_acq_ms: 1278,
            }),
            up_reject: Some(TransactionCost {
                control_plane_ms: 6,
                warmup_acq_ms: 1425,
                graded_acq_ms: 0,
            }),
            down_commit: Some(TransactionCost {
                control_plane_ms: 6,
                warmup_acq_ms: 741,
                graded_acq_ms: 0,
            }),
            down_reject: None,
        }
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

/// **Measured operating points, GENERATED from the committed device census.**
///
/// Do not edit the table by hand. `tools/abr-calibrate-plant.py --rust` emits the match arms below
/// from `docs/measurements/*-logs/pipe_abr_pin_*.log` plus `ffprobe` on the local fixture pack, and
/// `tools/test_abr_calibrate_plant.py` fails `make check` when this file and that derivation
/// disagree.
///
/// **That gate exists because this table was silently stale for a month.** It held three
/// hand-transcribed points from the `p1` capture run, and the fixture pack was rebuilt afterwards:
/// at rung 720 the delivered rate moved **1381 → 806 kbps (1.72×)** and at rung 4000 it moved the
/// other way (**3183 → 4386, 0.74×**). Nothing recomputes a constant, so nothing failed — the
/// closed-loop plant simply modelled a television that no longer existed, at two of its three
/// points. `p1`'s `pin_4000` never even reached rung 4000 (it sat at 6000 the whole run), which is
/// plain in the log and was invisible in the number derived from it.
///
/// **Seven rungs now, from three.** Everything else on the thirteen-point ladder is still
/// UNCALIBRATED and [`Calibration::point`] returns `None` for it rather than interpolating — a
/// normative simulation may not run on a fabricated operating point, which is the whole reason the
/// flat `overhead_ms: 120` that all this replaced had to go.
///
/// **One derived quantity, flagged as such.** `video_es_kbps` is `(ts - audio) / 1.04`: the AU
/// queues hold demuxed ELEMENTARY bytes while the census measured the TS wire, and 1.04 is an
/// assumed transport-stream overhead. It is an assumption, not a measurement, and increment I2's
/// per-lane ES byte counters replace it. `ts_kbps`, `audio_es_kbps` (ffprobe) and `overhead_ms`
/// (`A - active`) are measured.
///
/// **`ts_kbps` is a MEDIAN and the rebuilt fixtures are VBR.** Every rung carries six distinct
/// segment sizes spanning roughly ±10% of it — which the pre-rebuild pack did not, and which is
/// what makes byte variation testable at all. This plant feeds the median, so it under-exercises
/// exactly the variation §2a's transfer bound is built around. Carrying the distribution is a
/// separate increment and is deliberately not smuggled in here.
pub(super) struct Calibration;

/// Every rung [`Calibration::point`] answers for. Generated with the table below; the reproduction
/// test asserts its own length, so adding a rung without censusing it cannot pass quietly.
pub(super) const CALIBRATED: [u32; 7] = [320, 720, 2_000, 4_000, 10_000, 16_000, 20_000];

impl Calibration {
    /// The settled reserve the device actually showed at each pinned rung, median milliseconds.
    ///
    /// The other half of the generated table, and what makes
    /// [`tests::the_calibration_reproduces_the_device_census`] a TEST rather than the plant
    /// agreeing with itself: these are `buf=` off the television, while the prediction is queue
    /// geometry and elementary rates. The two share no term.
    ///
    /// First quarter of each pin is discarded as queue fill-in — the reserve climbs from zero at
    /// the start of a pin, so a median over the whole run measures the ramp as much as the ceiling.
    pub(super) fn census_buf_ms(rung_request_kbps: u32) -> Option<i64> {
        Some(match rung_request_kbps {
            320 => 88_293,
            720 => 67_543,
            2_000 => 37_210,
            4_000 => 18_418,
            10_000 => 8_168,
            16_000 => 5_793,
            20_000 => 4_918,
            _ => return None,
        })
    }

    pub(super) fn point(rung_request_kbps: u32) -> Option<OperatingPoint> {
        let (ts, audio, overhead) = match rung_request_kbps {
            320 => (383u32, 98u32, 18i64),
            720 => (806, 131, 24),
            2_000 => (2_198, 159, 53),
            4_000 => (4_386, 159, 86),
            10_000 => (10_522, 192, 181),
            16_000 => (16_530, 192, 274),
            20_000 => (20_694, 192, 345),
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

    // `who` names the code path that asked. A bare "rung N is UNCALIBRATED" says which census point
    // is missing and NOT how the controller got there, and those are different questions: a rung
    // reached by `current.below()` on a downshift is a hole in the descent, while one reached by a
    // proposal is a hole in the climb, and the fix is a different pin either way.
    let point_for = |rung: Rung, who: &str| -> Result<OperatingPoint, String> {
        Calibration::point(rung.kbps()).ok_or_else(|| {
            format!(
                "rung {}kbps is UNCALIBRATED — measure it before simulating it (reached via {who})",
                rung.kbps(),
            )
        })
    };

    while now_ms < horizon {
        let rung = controller.current();
        let point = point_for(rung, "the rung being played")?;
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
        let cand_point = point_for(proposal.rung, match proposal.direction {
            Direction::Up => "an UPSHIFT proposal",
            Direction::Down => "a DOWNSHIFT proposal",
        })?;

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
            // **The plant declares what it was asked for.** There is no PMS in the simulator to
            // do otherwise, so `sigma` is the only thing standing between the request and the
            // query -- which makes this a modelling assumption and not a measurement, and it is
            // the reason the plant cannot grade the catalog-rate error the device can.
            let declared_bps = u64::from(proposal.rung.kbps()).saturating_mul(1_000);
            controller.observe_candidate(probe);
            controller.candidate_ready(proposal, probe, declared_bps)
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
        // And on the census link it REPRODUCES the device reading at this rung. Measured p10..p90
        // over `p2-logs/pipe_abr_pin_20000.log` at 103-112 Mbit/s: **322..384 pm, median 366**.
        // Computed: fetch 20 694·2000/103 000 = 401 ms + overhead 345 = 746 ms over 2000 = 373 pm.
        //
        // The band both MOVED and WIDENED when the fixture pack was rebuilt — it read 313-318 with
        // a median of 321 before, against 322-384 now — because the rebuilt clips are VBR with six
        // segment sizes per rung while the plant feeds the median. So the plant lands mid-band by
        // construction and the WIDTH here is the fixture's, not the model's error.
        assert!((322..=384).contains(&prod), "expected the device band 322-384, got {prod}pm");
    }

    /// DEVICE-FINDING REGRESSION.
    ///
    /// **The ceiling model against a SEVEN-point device census it was never fitted to.** `b_max_ms`
    /// is queue geometry and elementary rates; `census_buf_ms` is `buf=` off the television. They
    /// share no term.
    ///
    /// It used to grade three points, and passed for a month while two of them described a fixture
    /// pack that had been rebuilt underneath it — because the prediction and the observation went
    /// stale together, both hand-copied out of the same superseded document. Both halves are
    /// generated now (`tools/abr-calibrate-plant.py`), which is what makes agreement mean something.
    #[test]
    fn the_calibration_reproduces_the_device_census() {
        let plant = Plant::default();
        let mut checked = 0;
        for rung in CALIBRATED {
            let point = Calibration::point(rung).expect("CALIBRATED lists it");
            let observed = Calibration::census_buf_ms(rung).expect("censused with it");
            let pred = plant.b_max_ms(&point);
            let err = (pred - observed).abs() * 100 / observed;
            assert!(err <= 5, "rung {rung}: predicted {pred}ms vs measured {observed}ms ({err}% off)");
            checked += 1;
        }
        assert_eq!(checked, CALIBRATED.len(), "every calibrated rung is graded, never a subset");
    }

    /// **The binding lane CROSSES between 720 and 2000, and the model finds it unaided.**
    ///
    /// The audio queue is an eighth the size of the video one and its rate barely moves with the
    /// ladder, so at the bottom audio is the ceiling and at the top video is. A plant that took
    /// `min` of two numbers could pass the census test at every point while getting the mechanism
    /// wrong — and every argument about `B_max` falling as `1/R` depends on which lane binds.
    ///
    /// This was untestable at three calibrated points: all three were video-bound, so the crossover
    /// sat outside the evidence. It is the first thing the widened census bought.
    #[test]
    fn the_binding_lane_crosses_between_720_and_2000() {
        let plant = Plant::default();
        let binds_audio = |rung: u32| {
            let p = Calibration::point(rung).expect("calibrated");
            let video = plant.video_lead_ms
                + (plant.video_queue_bytes * 8 / u64::from(p.video_es_kbps.max(1))) as i64;
            let audio = plant.audio_lead_ms
                + (plant.audio_queue_bytes * 8 / u64::from(p.audio_es_kbps.max(1))) as i64;
            audio < video
        };
        assert!(binds_audio(320), "at the floor the 1 MiB audio queue is the ceiling");
        assert!(binds_audio(720));
        for rung in [2_000, 4_000, 10_000, 16_000, 20_000] {
            assert!(!binds_audio(rung), "rung {rung} must be video-bound");
        }
    }

    /// MATHEMATICAL INVARIANT: three rates, and a lane ceiling never comes from a wire rate.
    ///
    /// Hand-computed from queue geometry; nothing here calls `b_max_ms` to build its expectation.
    /// video queue 8 MiB = 67 108 864 bits, audio queue 1 MiB = 8 388 608 bits, leads 1600 / 3600.
    ///
    /// **The arithmetic is spelled out against the CURRENT calibration**, because the version of
    /// this comment that preceded it quoted ES rates from a superseded fixture pack (video ES
    /// 17 561 at rung 20000, 1201 at 720) and read as a derivation while being a transcription:
    ///   * 20000: ES (20 694 − 192)/1.04 = 19 713 → 67 108 864/19 713 = 3404, +1600 = **5004**;
    ///     audio 192 → 8 388 608/192 = 43 690, +3600 = 47 290, so video binds.
    ///   * 720:   ES (806 − 131)/1.04 = 649 → 67 108 864/649 = 103 403, +1600 = 105 003;
    ///     audio 131 → 8 388 608/131 = 64 035, +3600 = **67 635**, so AUDIO binds.
    #[test]
    fn the_lane_ceilings_come_from_elementary_rates() {
        let plant = Plant::default();
        assert_eq!(plant.b_max_ms(&p20000()), 5_004);
        assert_eq!(plant.b_max_ms(&p720()), 67_635);
        // Forcing the video lane below the audio one flips which ceiling is returned, which is the
        // property `min` has to have and the census alone cannot isolate.
        let video_bound = OperatingPoint { video_es_kbps: 20_000, ..p720() };
        assert_eq!(plant.b_max_ms(&video_bound), 1_600 + 67_108_864 / 20_000);
    }

    /// MATHEMATICAL INVARIANT: the plant's constants are still the pipeline's.
    ///
    /// **All FOUR of them.** `B_max = lead + queue_bytes/rate`, and this test used to pin only the
    /// two byte caps — so either FEED LEAD could move in `player::engine` and this plant would go
    /// on modelling a pipeline that no longer exists, silently. That is not a hypothetical failure
    /// mode here: the operating-point table one level up was hand-transcribed, the fixture pack was
    /// rebuilt under it, and two of its three points described a television that had stopped
    /// existing for a month without a single test going red.
    #[test]
    fn the_plant_constants_still_match_the_pipeline() {
        let (video, audio) = crate::player::aq_caps();
        assert_eq!(video as u64, VIDEO_QUEUE_BYTES);
        assert_eq!(audio as u64, AUDIO_QUEUE_BYTES);
        let (video_lead, audio_lead) = crate::player::feed_leads_ms();
        assert_eq!(video_lead, VIDEO_LEAD_MS);
        assert_eq!(audio_lead, AUDIO_LEAD_MS);
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

    /// The first sample fetched with at least one segment of reserve ALREADY in hand.
    ///
    /// **Startup is not a rebuffer and must not be graded as one.** The plant begins at `B = 0`, so
    /// `stall_ms = max(0, wall - B)` charges the whole first fetch as a stall — which on a device is
    /// the spinner before playback, not a stutter during it. Everything from this index on is a
    /// real interruption of something the viewer was already watching.
    ///
    /// **The predicate is the PREVIOUS sample's reserve, and the difference is not pedantry.**
    /// `Observed::buf_ms` is the reserve *after* that segment landed, so sample 0 always reports a
    /// full segment and a naive `buf_ms >= segment_ms` marks the run warm at index 0 — which then
    /// grades the startup fill as a rebuffer. That is exactly what the first version of this helper
    /// did, and it reported a 292 ms "rebuffer" that was the opening fetch of a healthy run.
    fn warm_index(report: &Report, plant: &Plant) -> usize {
        report
            .samples
            .windows(2)
            .position(|w| w[0].buf_ms >= plant.segment_ms)
            .map(|i| i + 1)
            .unwrap_or(report.samples.len())
    }

    /// **The disturbance matrix, closed loop, on MEASURED transaction costs.**
    ///
    /// Until `TransactionModel::measured()` existed this could not be written: [`run`] refuses an
    /// absent leg and all four were absent, so every closed-loop test here either pinned the rung
    /// or asserted the refusal. The controller had never been graded making its own choices against
    /// a plant that charges what a transaction really costs.
    ///
    /// **What is asserted, and why each is not a chosen threshold.** After the reserve first reaches
    /// one segment, a stall is a viewer-visible rebuffer on a link that can carry the bottom of the
    /// ladder — so zero is the only defensible bound, and it is a product statement rather than a
    /// tuned number. The reserve may never exceed its own ceiling, which is arithmetic. And the
    /// rung the controller settles on must be one the link can actually carry, compared against the
    /// CALIBRATED transport rate rather than the catalog's request.
    ///
    /// Profiles are named for the device cases they mirror, so a divergence between this and the
    /// television is a comparison and not a coincidence.
    #[test]
    fn the_controller_never_rebuffers_on_a_link_that_can_carry_the_ladder() {
        let plant = Plant::default();
        let tx = TransactionModel::measured();
        // (name, legs, the capacity the run ENDS on). Weighted toward the CENSUSED region of the
        // ladder, because that is the only region this instrument can grade — see the skip list
        // below, which pins the gap rather than hiding it.
        //
        // **A leg is `(until_seconds, kbps)` — an absolute BOUNDARY, not a duration.**
        // `Trace::capacity_kbps` takes the first leg whose `until` is still ahead, so a list of
        // durations silently collapses to its first entry: the run then holds the opening capacity
        // for the whole horizon and every step and oscillation in this matrix quietly never
        // happens, while the test goes on passing.
        let matrix: [(&str, &[(f64, u32)], u32); 7] = [
            ("flat-floor", &[(240.0, 1_000)], 1_000),
            ("flat-low", &[(240.0, 3_000)], 3_000),
            ("flat-modest", &[(240.0, 6_000)], 6_000),
            ("slow-start", &[(60.0, 1_000), (240.0, 6_000)], 6_000),
            ("step-down", &[(120.0, 6_000), (300.0, 1_000)], 1_000),
            ("oscillating-low",
             &[(60.0, 6_000), (120.0, 1_200), (180.0, 6_000), (300.0, 1_200)], 1_200),
            ("flat-fast", &[(240.0, 60_000)], 60_000),
        ];
        // **Cells the census cannot grade are LISTED, never silently dropped.** Seven of thirteen
        // rungs are calibrated, and `run` refuses the rest rather than inventing an operating point
        // — so a fast link that climbs into the uncensused middle of the ladder cannot be graded
        // here at all. Reporting that is the difference between "this matrix passed" and "this
        // matrix passed over whatever it happened to reach".
        let mut skipped: Vec<String> = Vec::new();
        let mut graded = 0;
        for (name, legs, final_capacity) in matrix {
            let trace = Trace::new(legs);
            // **An HD catalog, because the census fixture is 1080p and so is every clip in the
            // pipeline pack.** `HlsActuatorCatalog::feasible` removes the 4K rung for an HD source
            // on a real playback too, so this is the configuration the device runs rather than a
            // convenience — and without it three of five cells climb to rung 22000, which no
            // census has ever reached and which the plant rightly refuses to invent.
            let catalog = super::super::HlsActuatorCatalog::measured()
                .limited_to((3840, 2176), (1920, 1080));
            let mut controller = Controller::starting_at(Rung::P480, None, catalog);
            let report = match run(&plant, &trace, &mut controller, &tx) {
                Ok(report) => report,
                Err(e) if e.contains("UNCALIBRATED") => {
                    skipped.push(format!("{name}: {e}"));
                    continue;
                }
                Err(e) => panic!("{name}: {e}"),
            };
            graded += 1;
            let warm = warm_index(&report, &plant);
            assert!(warm < report.samples.len(), "{name}: the reserve never reached one segment");

            let after: i64 = report.samples[warm..].iter().map(|s| s.stall_ms).sum();
            if after != 0 {
                // A bare count says a rebuffer happened; it does not say whether the controller
                // over-committed, the plant over-charged, or the link genuinely could not carry the
                // rung. Print the neighbourhood so the failure is a diagnosis.
                let first = report.samples[warm..].iter().position(|s| s.stall_ms > 0).unwrap() + warm;
                let lo = first.saturating_sub(3);
                let window: Vec<String> = report.samples[lo..(first + 2).min(report.samples.len())]
                    .iter()
                    .map(|s| format!(
                        "\n    t={}ms rung={}kbps cap={}kbps acq={}ms wall={}ms buf={}ms bmax={}ms stall={}ms",
                        s.at_ms, s.rung_kbps, s.capacity_kbps, s.acquire_ms, s.wall_ms,
                        s.buf_ms, s.b_max_ms, s.stall_ms))
                    .collect();
                panic!(
                    "{name}: {after}ms of rebuffer after warm-up (first at sample {first}); \
                     primes={} rejects={} commits={:?}{}",
                    report.primes, report.rejects, report.commits, window.concat(),
                );
            }

            assert!(
                report.samples.iter().all(|s| s.buf_ms <= s.b_max_ms),
                "{name}: the reserve exceeded its own ceiling",
            );

            let settled = report.final_rung_kbps();
            let ts = Calibration::point(settled).expect("run only visits calibrated rungs").ts_kbps;
            let tail = &report.samples[report.samples.len().saturating_sub(3)..];
            assert!(
                ts <= final_capacity,
                "{name}: settled on {settled}kbps, which DELIVERS {ts}kbps into a \
                 {final_capacity}kbps link.\n  commits={:?} primes={} rejects={}\n  tail={}",
                report.commits,
                report.primes,
                report.rejects,
                tail.iter()
                    .map(|s| format!("[t={}ms rung={} cap={} buf={}ms stall={}ms]",
                                     s.at_ms, s.rung_kbps, s.capacity_kbps, s.buf_ms, s.stall_ms))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        assert!(
            graded >= 6,
            "only {graded} of {} cells were gradeable; the census is too narrow for this matrix to \
             mean anything. Skipped:\n  {}",
            matrix.len(),
            skipped.join("\n  "),
        );

        // **The census gap, pinned.** A fast link climbs into the middle of the ladder, and the M4
        // pins covered 320/720/2000/4000/10000/16000/20000 — skipping 6000, 8000, 12000, 14000,
        // 18000 and 22000. So the one instrument that can compare policies without a television
        // CANNOT GRADE A FAST LINK, and that is a device job rather than a modelling one.
        //
        // Asserted as an exact set on purpose. If it shrinks, somebody censused a rung and this
        // list should shrink with it; if it grows, a profile now reaches a gap nobody accounted
        // for. Either way the reader is told, which a bare skip count does not do.
        let mut gaps: Vec<u32> = skipped
            .iter()
            .filter_map(|s| s.split("rung ").nth(1)?.split("kbps").next()?.parse().ok())
            .collect();
        gaps.sort_unstable();
        gaps.dedup();
        assert_eq!(
            gaps,
            vec![18_000],
            "the uncensused rungs this matrix reaches have changed; skipped:\n  {}",
            skipped.join("\n  "),
        );
    }

    /// **The device's own collapse case, closed loop.** `pipe_abr_down_collapse`
    /// (`tests/manifest.json`): a 40 Mbit/s link that drops to 500 kbps mid-stream.
    ///
    /// This is the test the two measurement notes kept asking for and could not have —
    /// `j3a-window-shadow.md` §5 and `j3-decides.md` §3 both end with *"a real comparison is the
    /// closed-loop simulator over frozen traces with a stall disqualifier, which has not been
    /// run"*. It could not be run because `run()` refuses an unmeasured transaction leg and all
    /// four were `None`; `TransactionModel::measured()` is what unblocked it.
    ///
    /// **What it asserts is the SHAPE, not a stall count.** A collapse from the top to 1.25% of the
    /// link is not survivable without interruption: `B_max(20000)` is ~5 s and one segment then
    /// costs ~80 s to fetch, so the reserve is gone long before any transaction can complete. The
    /// device agrees — `abr_shape` on that case records `max_stall_s=34`. Asserting "no stall"
    /// would be asserting something false, and asserting a NUMBER would be grading the plant
    /// against itself.
    ///
    /// What is gradeable, and what a broken controller would fail:
    /// 1. it reaches the floor rather than parking somewhere it cannot sustain;
    /// 2. rung 320 IS sustainable on 500 kbps (383 kbps of media, 1550 ms of acquisition against a
    ///    2000 ms segment), so once settled the stalling must STOP — a controller that keeps
    ///    interrupting on a link that can carry its rung is broken in a way this catches;
    /// 3. the descent terminates. No oscillation, no re-climb into the collapsed link.
    ///
    /// **It starts at rung 4000 and not at the top, and the reason is the census rather than the
    /// controller.** A downshift steps through `current.below()` (`controller.rs`'s fast-down arm),
    /// and above 4000 the M4 pins took every OTHER rung — 320, 720, 2000, 4000, 10000, 16000,
    /// 20000 — so `P1080High.below()` is 18000, which no pin ever visited and which `run` rightly
    /// refuses to invent. 4000 → 2000 → 720 → 320 is the longest descent this census can express.
    /// **A top-of-ladder collapse therefore remains a device job**, and it is the same six missing
    /// rungs the disturbance matrix above reports.
    #[test]
    fn the_device_collapse_case_descends_to_a_sustainable_rung_and_stops_stalling() {
        let plant = Plant::default();
        // A MODEST opening leg, not a fast one, and that is forced by the census too: on a
        // 40 Mbit/s leg the rule correctly admits a 2000 -> 20000 jump in one move, and the
        // emergency downshift out of 20000 then targets `current.below()` = 18000 while the
        // conservative estimate still lags the collapse. 18000 has no pin. 3 Mbit/s keeps the whole
        // run inside 320/720/2000/4000, which is the part of the ladder the census can express.
        let trace = Trace::new(&[(20.0, 3_000), (240.0, 500)]);
        let catalog = super::super::HlsActuatorCatalog::measured()
            .limited_to((3840, 2176), (1920, 1080));
        let mut controller = Controller::starting_at(Rung::P720Low, None, catalog);
        let report = run(&plant, &trace, &mut controller, &TransactionModel::measured())
            .expect("the descent only visits calibrated rungs");

        assert_eq!(report.final_rung_kbps(), Rung::P240.kbps(), "(1) it must reach the floor");

        // (2) Once on the floor with the collapsed link, no further interruption. Take the tail
        // strictly after the last commit, so the transaction that arrives there is not counted.
        let landed_at = report.commits.last().map(|&(at, _)| at).expect("it commits at least once");
        let tail: Vec<&Observed> =
            report.samples.iter().filter(|s| s.at_ms > landed_at + plant.segment_ms).collect();
        assert!(tail.len() >= 10, "not enough settled samples to judge: {}", tail.len());
        let tail_stall: i64 = tail.iter().map(|s| s.stall_ms).sum();
        assert_eq!(
            tail_stall, 0,
            "still rebuffering {tail_stall}ms on a rung the link can carry — 320 delivers {}kbps \
             into 500kbps",
            Calibration::point(320).unwrap().ts_kbps,
        );

        // (3) The descent terminates and never climbs back into the collapsed link.
        let after_landing: Vec<u32> =
            report.commits.iter().filter(|&&(at, _)| at > 20_000).map(|&(_, k)| k).collect();
        assert!(
            after_landing.windows(2).all(|w| w[1] <= w[0]),
            "the controller climbed back into a collapsed link: {after_landing:?}",
        );
    }

    /// **A link below the bottom of the ladder stalls, and the controller still goes to the floor.**
    ///
    /// The complement of the test above, and it is here so that "no rebuffer" cannot be satisfied by
    /// a controller that simply never moves. 300 kbps cannot carry rung 320, which DELIVERS 383 —
    /// so a stall is correct behaviour and the only thing to grade is that the controller ends up as
    /// low as the ladder goes rather than parked somewhere it cannot sustain.
    #[test]
    fn a_link_under_the_floor_rung_drives_the_controller_to_the_floor() {
        let plant = Plant::default();
        let trace = Trace::new(&[(30.0, 40_000), (180.0, 300)]);
        let mut controller = Controller::starting_at(Rung::P480, None, cat());
        let report = run(&plant, &trace, &mut controller, &TransactionModel::measured())
            .expect("calibrated");
        assert_eq!(report.final_rung_kbps(), Rung::P240.kbps(), "the floor is where it belongs");
        assert!(report.stall_ms_total > 0, "a link under the floor MUST stall; hiding that is worse");
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

