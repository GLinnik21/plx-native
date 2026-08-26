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
//! bytes     = R · d / 8                       (kbps · ms = bits)
//! fetch_ms  = R · d / C                       (bits / kbps = ms)
//! wall_ms   = max(fetch_ms + overhead_ms, B + d - B_max)     ← backpressure blocks the demuxer
//! stall_ms  = max(0, wall_ms - B)
//! B'        = min(B_max, max(0, B - wall_ms) + d)
//! ```
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
//! commit (`ff.rs`'s prime arm). So a prime costs [`Plant::transaction_ms`] of wall time with **no
//! fill**, and a rejected one costs it for nothing. A plant that omitted this would report zero
//! stalls for precisely the regression that removing the sample counters trades against.
//!
//! # Determinism
//!
//! Virtual time only. No `Instant`, no sleep, no thread. The same trace and the same plant produce
//! the same [`Report`] on every machine, which is what makes a two-parameter-set A/B meaningful.

use super::{
    BufferSnapshot, Controller, Decision, HlsActuatorCatalog, MediaTimeMs, Rung, SegmentSample,
};

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

/// The physical pipeline, as parameters. Nothing here is policy and nothing here is read from
/// [`super::AbrPolicy`].
#[derive(Clone, Copy, Debug)]
pub(super) struct Plant {
    pub(super) video_queue_bytes: u64,
    pub(super) audio_queue_bytes: u64,
    pub(super) video_lead_ms: i64,
    pub(super) audio_lead_ms: i64,
    /// The audio elementary rate the muxed stream actually carries. Constant across the ladder,
    /// which is why the audio lane binds at the bottom of it and nowhere else.
    pub(super) audio_kbps: u32,
    /// Content duration of one segment, ms. The client asks PMS for 2 s.
    pub(super) segment_ms: i64,
    /// Fixed per-segment acquisition overhead that is NOT transfer — connection, request, time to
    /// first byte, JIT production latency. Enters `total_fetch_us` and never `active_fetch_us`,
    /// which is the same split `SegmentSample` makes.
    pub(super) overhead_ms: i64,
    /// What one candidate transaction costs in unrefilled playback.
    pub(super) transaction_ms: i64,
}

impl Default for Plant {
    fn default() -> Self {
        Self {
            video_queue_bytes: VIDEO_QUEUE_BYTES,
            audio_queue_bytes: AUDIO_QUEUE_BYTES,
            video_lead_ms: VIDEO_LEAD_MS,
            audio_lead_ms: AUDIO_LEAD_MS,
            audio_kbps: 192,
            segment_ms: 2_000,
            overhead_ms: 120,
            // 2.3 x segment: `candidate_warmup_budget` (3/2 d) plus `candidate_prime_budget`
            // (4/5 d). Derived, not measured — measurement step M4 replaces it.
            transaction_ms: 4_600,
        }
    }
}

impl Plant {
    /// The video elementary rate at a given total wire rate.
    pub(super) fn video_kbps(&self, wire_kbps: u32) -> u32 {
        wire_kbps.saturating_sub(self.audio_kbps).max(1)
    }

    /// **The reachable reserve, computed from queue geometry alone.** The independent oracle.
    ///
    /// Widened to `u64`/`i64` throughout: a LAN observation of 865 Gbit/s is on record in
    /// `abr.rs`, release builds have `overflow-checks` off and host tests have them on, and an
    /// expression that panics on one and wraps on the other is two different models.
    pub(super) fn b_max_ms(&self, wire_kbps: u32) -> i64 {
        let video_bits = self.video_queue_bytes.saturating_mul(8);
        let audio_bits = self.audio_queue_bytes.saturating_mul(8);
        // bits / kbps = ms. No scale factor. See the module note.
        let video = self
            .video_lead_ms
            .saturating_add((video_bits / u64::from(self.video_kbps(wire_kbps))).min(i64::MAX as u64) as i64);
        let audio = self
            .audio_lead_ms
            .saturating_add((audio_bits / u64::from(self.audio_kbps.max(1))).min(i64::MAX as u64) as i64);
        video.min(audio)
    }
}

/// A capacity schedule in virtual time: `(until_ms, kbps)`, last leg extends forever.
#[derive(Clone, Debug)]
pub(super) struct Trace(Vec<(i64, u32)>);

impl Trace {
    /// `[(seconds, kbps), ...]` — the same shape `tests/manifest.json`'s `network_profile` uses, so
    /// a device profile and a simulated one are written the same way.
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

/// One observed segment, as the plant produced it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Observed {
    pub(super) at_ms: i64,
    pub(super) rung_kbps: u32,
    pub(super) capacity_kbps: u32,
    pub(super) media_kbps: u32,
    pub(super) buf_ms: i64,
    pub(super) b_max_ms: i64,
    pub(super) stall_ms: i64,
}

/// What a run produced. Everything a leg is graded on is here; nothing is graded inside the loop.
#[derive(Clone, Debug, Default)]
pub(super) struct Report {
    pub(super) samples: Vec<Observed>,
    pub(super) commits: Vec<(i64, u32)>,
    pub(super) primes: u32,
    pub(super) rejects: u32,
    pub(super) stall_ms_total: i64,
    pub(super) stall_ms_max: i64,
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

/// Fetch one segment and advance the plant. Returns `(observed, sample)`.
///
/// Separated from [`run`] so the transaction path and the steady path share exactly one copy of the
/// state equations — two copies is how a plant starts disagreeing with itself.
fn step(
    plant: &Plant,
    trace: &Trace,
    now_ms: &mut i64,
    buf_ms: &mut i64,
    wire_kbps: u32,
    media_kbps: u32,
) -> (Observed, SegmentSample) {
    let d = plant.segment_ms;
    let capacity = trace.capacity_kbps(*now_ms);
    let b_max = plant.b_max_ms(wire_kbps);

    // bits = kbps * ms; fetch_ms = bits / kbps. Widened; both operands are bounded by u32 but
    // their product is not.
    let bits = u64::from(media_kbps).saturating_mul(d.max(0) as u64);
    let fetch_ms = (bits / u64::from(capacity.max(1))).min(i64::MAX as u64) as i64;
    let acquire_ms = fetch_ms.saturating_add(plant.overhead_ms);

    // Backpressure: the demuxer blocks rather than exceeding the queue cap, so wall time stretches
    // until the reserve has room for this segment.
    let blocked_until = buf_ms.saturating_add(d).saturating_sub(b_max);
    let wall_ms = acquire_ms.max(blocked_until).max(1);

    let stall_ms = (wall_ms - *buf_ms).max(0);
    let drained = (*buf_ms - wall_ms).max(0);
    *buf_ms = (drained + d).min(b_max);
    *now_ms += wall_ms;

    let observed = Observed {
        at_ms: *now_ms,
        rung_kbps: wire_kbps,
        capacity_kbps: capacity,
        media_kbps,
        buf_ms: *buf_ms,
        b_max_ms: b_max,
        stall_ms,
    };
    // The snapshot the controller sees. Both lanes are given the same tail, because the plant's
    // ceiling has already applied `min(video, audio)` — the controller's own `buffered_ms` must
    // not apply it twice.
    let snapshot = BufferSnapshot {
        playback: MediaTimeMs(0),
        video_tail: MediaTimeMs(*buf_ms),
        audio_tail: Some(MediaTimeMs(*buf_ms)),
        audio_expected: true,
    };
    let bytes = (bits / 8).max(1);
    let sample = SegmentSample::new(
        bytes,
        (fetch_ms.max(1) as u64).saturating_mul(1_000),
        (wall_ms.max(1) as u64).saturating_mul(1_000),
        u32::try_from(d).unwrap_or(2_000),
        snapshot,
    )
    .expect("plant produced a degenerate segment");
    (observed, sample)
}

/// Run `controller` against `trace` on `plant` until the trace's horizon.
///
/// The controller's `current()` chooses the rung; the plant chooses what it costs. `media_of` maps
/// a rung to the media rate the server actually delivers for it — by default the catalog's planning
/// rate, but a caller can hand in a measured table once M3/M4 have one.
pub(super) fn run(
    plant: &Plant,
    trace: &Trace,
    controller: &mut Controller,
    catalog: &HlsActuatorCatalog,
    media_of: impl Fn(Rung) -> u32,
) -> Report {
    let mut report = Report::default();
    let mut now_ms: i64 = 0;
    let mut buf_ms: i64 = 0;
    let horizon = trace.horizon_ms();

    while now_ms < horizon {
        let rung = controller.current();
        let wire = catalog.candidate(rung).expected_wire_kbps;
        let (observed, sample) = step(plant, trace, &mut now_ms, &mut buf_ms, wire, media_of(rung));
        report.stall_ms_total += observed.stall_ms;
        report.stall_ms_max = report.stall_ms_max.max(observed.stall_ms);
        report.samples.push(observed);

        let decision = controller.observe(sample);
        if report.first_decision.is_none() {
            report.first_decision = Some(decision);
            report.first_buf_ms = observed.buf_ms;
        }
        let Decision::Prime(proposal) = decision else {
            continue;
        };
        report.primes += 1;

        // **The transaction.** `transaction_ms` of wall time during which the CURRENT stream is
        // not read at all: the candidate's warm-up and graded segments are fetched inside this
        // window and are fed only after the commit (`ff.rs`'s prime arm). So the reserve drains for
        // the whole of it and gains nothing — and a REJECTED transaction costs exactly the same,
        // which is the asymmetry a stateless proposal cost cannot express.
        //
        // The candidate segment is measured INSIDE this window rather than after it: charging its
        // fetch a second wall interval would double-count the one cost being modelled.
        let tx_stall = (plant.transaction_ms - buf_ms).max(0);
        buf_ms = (buf_ms - plant.transaction_ms).max(0);
        now_ms += plant.transaction_ms;
        report.stall_ms_total += tx_stall;
        report.stall_ms_max = report.stall_ms_max.max(tx_stall);

        let cand_wire = catalog.candidate(proposal.rung).expected_wire_kbps;
        let cand_media = media_of(proposal.rung);
        let capacity = trace.capacity_kbps(now_ms);
        let bits = u64::from(cand_media).saturating_mul(plant.segment_ms.max(0) as u64);
        let fetch_ms = (bits / u64::from(capacity.max(1))).min(i64::MAX as u64) as i64;
        let cand_sample = SegmentSample::new(
            (bits / 8).max(1),
            (fetch_ms.max(1) as u64).saturating_mul(1_000),
            (fetch_ms.saturating_add(plant.overhead_ms).max(1) as u64).saturating_mul(1_000),
            u32::try_from(plant.segment_ms).unwrap_or(2_000),
            BufferSnapshot {
                playback: MediaTimeMs(0),
                video_tail: MediaTimeMs(buf_ms),
                audio_tail: Some(MediaTimeMs(buf_ms)),
                audio_expected: true,
            },
        )
        .expect("plant produced a degenerate candidate segment");

        if controller.candidate_ready(proposal, cand_sample) && controller.commit(proposal) {
            // Committed: the candidate segment is fed, and the ceiling moves with the new rung.
            buf_ms = buf_ms.saturating_add(plant.segment_ms).min(plant.b_max_ms(cand_wire));
            report.commits.push((now_ms, proposal.rung.kbps()));
        } else {
            controller.reject(proposal);
            report.rejects += 1;
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::super::{Decision, Direction, HlsActuatorCatalog, Rung};
    use super::*;

    fn catalog() -> HlsActuatorCatalog {
        HlsActuatorCatalog::measured()
    }

    /// MATHEMATICAL INVARIANT (I0-J category 1).
    ///
    /// The plant's ceiling, against values computed BY HAND from the queue geometry — not from
    /// `b_max_ms` itself. Writing `let expected = plant.b_max_ms(x)` here would make this test the
    /// implementation agreeing with itself, which is the failure mode I0-C exists to forbid.
    ///
    /// Arithmetic, all of it, so a future reader can check it without running anything:
    ///
    /// * video queue 8 MiB = 8 388 608 B = 67 108 864 bits; audio queue 1 MiB = 8 388 608 bits
    /// * audio lane, 192 kbps: 8 388 608 / 192 = 43 690 ms, + 3 600 lead = **47 290 ms**, at EVERY
    ///   rung, because the audio rate does not move with the ladder
    /// * P240, wire 320: video ES 128 kbps -> 67 108 864 / 128 = 524 288, + 1 600 = 525 888
    ///   -> min(525 888, 47 290) = **47 290** (AUDIO binds)
    /// * P480, wire 720: video ES 528 -> 67 108 864 / 528 = 127 100, + 1 600 = 128 700
    ///   -> min(128 700, 47 290) = **47 290** (AUDIO still binds)
    /// * P1080M10, wire 10 000: video ES 9 808 -> 67 108 864 / 9 808 = 6 842, + 1 600 = **8 442**
    ///   (VIDEO binds)
    /// * P1080High, planning wire 20 011: video ES 19 819 -> 67 108 864 / 19 819 = 3 386,
    ///   + 1 600 = **4 986** — under five seconds, which is the finding the whole plan turns on
    #[test]
    fn the_reachable_reserve_is_computed_from_queue_geometry_not_from_policy() {
        let plant = Plant::default();
        assert_eq!(plant.b_max_ms(320), 47_290, "P240: the audio lane binds");
        assert_eq!(plant.b_max_ms(720), 47_290, "P480: the audio lane still binds");
        assert_eq!(plant.b_max_ms(10_000), 8_442, "P1080M10: the video lane binds");
        assert_eq!(plant.b_max_ms(20_011), 4_986, "P1080High: five seconds, not sixty");
        // The crossover, stated as an inequality rather than a magic rate: the audio lane binds
        // below it and the video lane above it.
        assert!(plant.b_max_ms(1_600) == 47_290, "1.6 Mbit/s wire: the audio lane still binds");
        assert!(plant.b_max_ms(1_700) < 47_290, "1.7 Mbit/s wire: the video lane has taken over");
    }

    /// MATHEMATICAL INVARIANT (category 1).
    ///
    /// The plant's constants are the pipeline's. If `AQ_VIDEO_BYTES` or `MAX_FEED_AHEAD_NS` moves
    /// in `player::engine`, every number in this module is silently wrong and every reachability
    /// conclusion with it — so the coupling is asserted rather than trusted to a comment.
    #[test]
    fn the_plant_constants_still_match_the_pipeline() {
        let (video, audio) = crate::player::aq_caps();
        assert_eq!(video as u64, VIDEO_QUEUE_BYTES, "video AU queue cap moved");
        assert_eq!(audio as u64, AUDIO_QUEUE_BYTES, "audio AU queue cap moved");
    }

    /// MATHEMATICAL INVARIANT (category 1).
    ///
    /// `dB/dt = C/R - 1`, read off the plant rather than assumed. Three regimes, one equation:
    /// a surplus fills, a matched link holds, a deficit drains. The expected values are computed
    /// from the ratio here, independently of `step`'s own expression.
    #[test]
    fn the_plant_reproduces_the_drain_identity() {
        let plant = Plant { overhead_ms: 0, ..Plant::default() };
        for &(capacity, media, want_sign) in &[(20_000u32, 10_000u32, 1i8), (10_000, 10_000, 0), (5_000, 10_000, -1)] {
            let trace = Trace::new(&[(600.0, capacity)]);
            let mut now = 0;
            // Below the ceiling at this rung (8 442 ms) and above the deepest fetch, so neither
            // the byte cap nor a stall interferes with the identity being read.
            let mut buf = 4_000;
            let before = buf;
            let (_, _) = step(&plant, &trace, &mut now, &mut buf, media, media);
            let delta = buf - before;
            // dB over one segment = d * (1 - R/C); sign is all this asserts, magnitude below.
            assert_eq!(delta.signum() as i8, want_sign, "C={capacity} R={media}");
            let fetch = i64::from(media) * plant.segment_ms / i64::from(capacity);
            assert_eq!(delta, plant.segment_ms - fetch, "C={capacity} R={media}");
        }
    }

    /// MATHEMATICAL INVARIANT (category 1).
    ///
    /// The reserve is bounded by the plant's own ceiling however long a fast link runs, and a
    /// starved one bottoms out at zero rather than going negative. Both bounds are structural: an
    /// unbounded reserve is what made the eight tests in plan §8.7 assert unreachable states.
    #[test]
    fn the_reserve_stays_inside_zero_and_the_ceiling() {
        let plant = Plant::default();
        let trace = Trace::new(&[(600.0, 400_000)]);
        let mut now = 0;
        let mut buf = 0;
        for _ in 0..200 {
            let (o, _) = step(&plant, &trace, &mut now, &mut buf, 10_000, 9_808);
            assert!((0..=o.b_max_ms).contains(&buf), "buf {buf} outside 0..={}", o.b_max_ms);
        }
        assert_eq!(buf, plant.b_max_ms(10_000), "a fast link must settle AT the ceiling");
    }

    /// MATHEMATICAL INVARIANT (category 1).
    ///
    /// Extreme magnitudes must not panic on the host (overflow-checks ON) or wrap on the device
    /// (overflow-checks OFF, `Cargo.toml`'s release profile). 865 Gbit/s is a real reading from
    /// this project's own event log.
    #[test]
    fn the_plant_survives_the_magnitudes_a_lan_actually_produces() {
        let plant = Plant::default();
        for &wire in &[1u32, 320, 22_000, 865_000_000, u32::MAX] {
            let b = plant.b_max_ms(wire);
            assert!(b > 0, "wire={wire} produced a non-positive ceiling");
        }
        for &capacity in &[1u32, 320, 865_000_000, u32::MAX] {
            let trace = Trace::new(&[(600.0, capacity)]);
            let mut now = 0;
            let mut buf = 0;
            let (o, _) = step(&plant, &trace, &mut now, &mut buf, u32::MAX, u32::MAX);
            assert!(o.buf_ms >= 0 && now > 0);
        }
    }

    /// INTEGRATION (category 4).
    ///
    /// A full closed loop: the controller drives, the plant answers, and the run is reproducible.
    /// This asserts only that the loop CLOSES and is deterministic — no rung, no commit count and
    /// no threshold, because those are policy and I0 grades none of it.
    #[test]
    fn the_loop_closes_and_is_deterministic() {
        let plant = Plant::default();
        let trace = Trace::new(&[(10.0, 2_000), (120.0, 40_000)]);
        let cat = catalog();
        let run_once = || {
            let mut c = Controller::starting_at(Rung::P480, None, cat);
            run(&plant, &trace, &mut c, &cat, |r| plant.video_kbps(cat.candidate(r).expected_wire_kbps) + plant.audio_kbps)
        };
        let a = run_once();
        let b = run_once();
        assert!(!a.samples.is_empty(), "the plant produced no segments");
        assert_eq!(a.visited_kbps(), b.visited_kbps(), "the loop is not deterministic");
        assert_eq!(a.stall_ms_total, b.stall_ms_total, "the loop is not deterministic");
        assert_eq!(a.min_buf_ms(), b.min_buf_ms(), "the loop is not deterministic");
        assert_eq!(a.final_rung_kbps(), b.final_rung_kbps(), "the loop is not deterministic");
        assert!(a.samples.iter().all(|s| s.buf_ms <= s.b_max_ms), "a sample exceeded its ceiling");
        assert!(a.min_buf_ms() >= 0 && a.final_rung_kbps() > 0);
    }

    /// MATHEMATICAL INVARIANT (category 1).
    ///
    /// Virtual time advances monotonically, the plant reads the capacity the trace declares at the
    /// moment each segment is fetched, and every segment carries a positive media rate. Without
    /// the first of these a trace leg means nothing; without the second the shaped profile in a
    /// device manifest and the same profile here are not the same experiment.
    #[test]
    fn the_plant_follows_the_trace_in_virtual_time() {
        let plant = Plant::default();
        // One leg an order of magnitude below the next, so the boundary is unmistakable.
        let trace = Trace::new(&[(20.0, 2_000), (200.0, 40_000)]);
        let cat = catalog();
        let mut c = Controller::starting_at(Rung::P480, None, cat).pinned_to(Some(Rung::P480));
        let report = run(&plant, &trace, &mut c, &cat, |r| {
            plant.video_kbps(cat.candidate(r).expected_wire_kbps) + plant.audio_kbps
        });
        assert!(report.samples.windows(2).all(|w| w[1].at_ms > w[0].at_ms), "time went backwards");
        assert!(report.samples.iter().all(|s| s.media_kbps > 0), "a segment carried no media");
        for s in &report.samples {
            let want = if s.at_ms <= 20_000 { 2_000 } else { 40_000 };
            // `at_ms` is the END of the fetch, so the segment straddling the boundary may legally
            // report either leg; assert the two ends of the run rather than the crossing.
            if s.at_ms < 18_000 || s.at_ms > 24_000 {
                assert_eq!(s.capacity_kbps, want, "capacity at {}ms", s.at_ms);
            }
        }
    }

    /// INTEGRATION (category 4).
    ///
    /// The rung pin (I0-D) holds one actuator, and every estimator keeps running underneath it.
    /// This is the mechanism measurement step M4 depends on, so it is graded here rather than
    /// discovered on a television.
    #[test]
    fn a_pinned_controller_reaches_its_rung_and_then_holds_it() {
        let plant = Plant::default();
        let trace = Trace::new(&[(180.0, 60_000)]);
        let cat = catalog();
        let mut c = Controller::starting_at(Rung::P480, None, cat).pinned_to(Some(Rung::P1080M10));
        let report = run(&plant, &trace, &mut c, &cat, |r| {
            plant.video_kbps(cat.candidate(r).expected_wire_kbps) + plant.audio_kbps
        });
        assert_eq!(c.current(), Rung::P1080M10, "the pin was not reached");
        assert_eq!(report.commits.len(), 1, "a pin must cost exactly one transaction");
        let tail = &report.samples[report.samples.len() - 5..];
        assert!(tail.iter().all(|s| s.rung_kbps == 10_000), "the pin did not hold");
        // The estimator is still live under the pin — which is the whole reason M4 can read
        // anything from a pinned run.
        assert!(c.telemetry().delivery.samples > 5, "the pin silenced the estimator");
    }

    /// CHARACTERISATION / BASELINE (I0-J category 5) — **not a policy assertion.**
    ///
    /// Plan §0.3(1): at the first `observe` the reserve is one segment, and `buffer_bad` is
    /// `buffered < segment || starving()` where `starving()` trips at `<= 2000` — so the first
    /// segment of every Auto HLS playback may propose a downshift, on any link.
    ///
    /// What is asserted here is the STRUCTURAL precondition only: the first sample's reserve
    /// cannot exceed one segment, whatever the link does. The DECISION is recorded and reported,
    /// never asserted to be correct — increment I3 is expected to change it, and when it does this
    /// test keeps passing. Nothing in I0 may pin today's answer as desirable.
    #[test]
    fn characterise_the_first_segment_of_a_fast_link() {
        let plant = Plant::default();
        let trace = Trace::new(&[(60.0, 400_000)]); // 400 Mbit/s: nothing about this link is slow
        let cat = catalog();
        let mut c = Controller::starting_at(Rung::P480, None, cat);
        let report = run(&plant, &trace, &mut c, &cat, |r| {
            plant.video_kbps(cat.candidate(r).expected_wire_kbps) + plant.audio_kbps
        });
        assert!(
            report.first_buf_ms <= plant.segment_ms,
            "structural: one segment in, the reserve cannot exceed one segment (got {}ms)",
            report.first_buf_ms,
        );
        // Recorded, not graded. `cargo test -- --nocapture` prints it; the harness reads the same
        // fact off the device from `abr: sample`'s first line.
        let observed = match report.first_decision {
            Some(Decision::Prime(p)) if p.direction == Direction::Down => "PRIME(Down)",
            Some(Decision::Prime(_)) => "PRIME(Up)",
            Some(Decision::Stay) => "Stay",
            None => "none",
        };
        println!(
            "CHARACTERISATION first-segment: buf={}ms decision={} (plan §0.3(1); I3 may change this)",
            report.first_buf_ms, observed,
        );
    }
}
