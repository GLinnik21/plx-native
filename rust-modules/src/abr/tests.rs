use super::*;

/// The [`candidate_warmup_budget`] floor, absent. Named rather than written out at each site so a
/// test that is grading the RESERVE bound says so — `predicted_transfer` returns exactly this
/// whenever the link is unmeasured, so it is also the real value on a cold transaction.
const NO_FLOOR: std::time::Duration = std::time::Duration::ZERO;

/// A segment described by what actually crossed the wire, for the cases where the SIZE of the
/// transfer is the point rather than the rate it works out to.
fn sample_bytes(bytes: u64, active_us: u64, ratio_pm: u32, buffered_ms: i64) -> SegmentSample {
    let media_ms = 2_000;
    let total_us = u64::from((media_ms * ratio_pm / 1_000).max(1)) * 1_000;
    SegmentSample::new(
        bytes,
        active_us,
        total_us.max(active_us),
        media_ms,
        BufferSnapshot {
            playback: MediaTimeMs(10_000),
            video_tail: MediaTimeMs(10_000 + buffered_ms),
            audio_tail: Some(MediaTimeMs(10_000 + buffered_ms)),
            audio_expected: true,
        },
    )
    .unwrap()
}

/// A rendition that declares exactly what its rung asked for. Real PMS does not — the catalog's
/// error is +5.2% to +31.6% — but a test that wants to exercise the ADMISSION arithmetic should not
/// also be modelling that gap, and every test here that needs a different one passes it directly.
fn declared_bps(rung: Rung) -> u64 {
    u64::from(rung.kbps()).saturating_mul(1_000)
}

/// A segment on a link of `network_kbps` whose acquisition is `ratio_pm` of its content duration.
///
/// **The two arguments over-determine the sample** — `bytes`, `active_fetch_us` and
/// `total_fetch_us` are three numbers and this fixture is given two — so how the third is resolved
/// is a plant model, and it used to be `active = min(total, 200ms)`. At the `ratio_pm = 400` most
/// tests here use, that made the transfer 200 ms of an 800 ms acquisition: **a 75% fixed-cost
/// share, on every sample, independent of the link argument.**
///
/// Measured on the device (`docs/measurements/j3a-window-logs`, 127 segments over three cases), the
/// real share is **6% median at rung 4000, 16% at rung 20000, and 37% at the 90th percentile of the
/// worst case**. Nothing on that television resembles 75%, and the difference is not cosmetic:
/// §4's admission rule reads total acquisition against bytes, so a fixture where three quarters of
/// the cost does not scale with bytes describes a link four times faster than its own acquisitions
/// admit — and grades the rule against a plant that cannot exist.
///
/// So the resolution is now `active = total × 6/7`, a **14% fixed-cost share**, inside the measured
/// band. Both arguments keep their exact meaning: `network_kbps()` still returns the first and
/// `production_ratio_pm()` still returns the second.
fn sample(network_kbps: u32, ratio_pm: u32, buffered_ms: i64) -> SegmentSample {
    sample_of(2_000, network_kbps, ratio_pm, buffered_ms)
}

/// [`sample`] with the segment's own media duration named. Every fixture in this file is written
/// around the 2 s segment this pipeline requests, and that is the right default — but a wall-clock
/// guard cannot be told apart from a segment counter without varying it, so the one axis the
/// default hides has to be reachable.
fn sample_of(media_ms: u32, network_kbps: u32, ratio_pm: u32, buffered_ms: i64) -> SegmentSample {
    let total_us = u64::from((media_ms * ratio_pm / 1_000).max(1)) * 1_000;
    let active_us = (total_us * 6 / 7).max(1);
    let bytes = u64::from(network_kbps) * active_us / 8_000;
    SegmentSample::new(
        bytes,
        active_us,
        total_us,
        media_ms,
        BufferSnapshot {
            playback: MediaTimeMs(10_000),
            video_tail: MediaTimeMs(10_000 + buffered_ms),
            audio_tail: Some(MediaTimeMs(10_000 + buffered_ms)),
            audio_expected: true,
        },
    )
    .unwrap()
}

/// The ordinary case: a 1080p source on a device that decodes it. Most tests want this,
/// because a catalog that reaches 4K reaches it only when the SOURCE does — and asserting on
/// an unbounded catalog would grade a configuration no real playback has.
fn hd_catalog() -> HlsActuatorCatalog {
    HlsActuatorCatalog::measured().limited_to((3840, 2176), (1920, 1080))
}

/// A UHD source on the dev set's own decode bound, which is what makes the 4K actuator
/// feasible at all.
fn uhd_catalog() -> HlsActuatorCatalog {
    HlsActuatorCatalog::measured().limited_to((3840, 2176), (3840, 2160))
}

fn controller_at(rung: Rung) -> Controller {
    Controller::starting_at(rung, None, hd_catalog())
}

/// Unknown links start at 480p/720 kbit/s; P240 stays available as the emergency floor.
fn bootstrap_controller() -> Controller {
    controller_at(Rung::P480)
}

fn original(source_kbps: u32) -> OriginalModeController {
    OriginalModeController::new(
        source_kbps,
        AbrPolicy::measured(),
        hd_catalog(),
        TransitionHistory::default(),
        SourceFeatures::default(),
    )
    .unwrap()
}

/// Bytes that make one 750 ms window measure exactly `kbps`.
fn window_bytes(kbps: u64) -> u64 {
    kbps * ORIGINAL_WINDOW_US / 8_000
}

const HOUR_MS: i64 = 3_600_000;

/// Drive the controller until it proposes an upshift.
///
/// **The bound is `2n`, not a handful of samples, and that is the admission rule showing through.**
/// An upshift now needs a full acquisition window on BOTH sides — `observe` will not propose one it
/// cannot commit, and `candidate_ready` will not commit one without the evidence — so a helper that
/// stopped at four samples was asserting a climb the controller is designed to refuse. Twice `n`
/// leaves room for the transaction gates that sit in front of it.
fn prime_up(controller: &mut Controller) -> Proposal {
    let n = AbrPolicy::measured().admission.window_len();
    for _ in 0..(n * 2) {
        if let Decision::Prime(proposal) = controller.observe_next(sample(20_000, 200, 10_000)) {
            return proposal;
        }
    }
    panic!("no proposal")
}

/// Drive a controller to rest on a flat link, with a reserve that **integrates the deficit**.
///
/// It used to hand every sample a frozen `buffered_ms: 10_000`, and that fixture is physically
/// impossible: a link short of the rung it is playing drains the reserve by `d*(R/C - 1)` every
/// segment, and one that exceeds it fills until the queue caps. A constant is neither.
///
/// It passed anyway while `network_bad` — a bare `C < R` with no reserve in it — was a downshift
/// TRIGGER, because that predicate cannot tell a full buffer from an empty one. N4 deleted that
/// trigger, and the deficit legs then settled a rung too high here while behaving correctly on a
/// real link. So the fixture was the thing that was wrong, and it was wrong in the way
/// `[[reserve-cannot-see-a-slow-film]]` names: it held constant the very quantity the predicate
/// under test reads.
///
/// The model is one line of the plant's own state equation (`abr/sim.rs`) and deliberately not a
/// call into it — a helper that asked the plant would make this a test of the plant. `R` is the
/// rung's REQUEST rate rather than its measured wire rate, which is the one approximation here and
/// is stated: the catalog's `expected_wire_kbps` differs by up to a few per cent and nothing in
/// this test turns on it.
fn settle_link(network_kbps: u32) -> Rung {
    let mut controller = bootstrap_controller();
    const SEGMENT_MS: i64 = 2_000;
    // A ceiling so a fast link does not integrate to an unreachable reserve — `B_max` at the
    // bottom of the ladder is tens of seconds and this is inside it at every rung these legs use.
    const CEILING_MS: i64 = 30_000;
    let mut buf_ms: i64 = 10_000;
    for _ in 0..80 {
        let rung_kbps = i64::from(controller.current().kbps());
        let fetch_ms = SEGMENT_MS * rung_kbps / i64::from(network_kbps.max(1));
        buf_ms = (buf_ms - fetch_ms + SEGMENT_MS).clamp(0, CEILING_MS);
        if let Decision::Prime(proposal) =
            controller.observe_next(sample(network_kbps, 400, buf_ms))
        {
            let candidate = sample(network_kbps, 400, buf_ms.max(SEGMENT_MS));
            if controller.candidate_ready(proposal, candidate, declared_bps(proposal.rung)) {
                controller.commit(proposal, controller.clock_ms());
            } else {
                controller.reject(proposal, RejectCause::Candidate, controller.clock_ms());
            }
        }
    }
    controller.current()
}

#[test]
fn zero_or_inconsistent_measurements_are_not_infinite_headroom() {
    let b = BufferSnapshot {
        playback: MediaTimeMs(0),
        video_tail: MediaTimeMs(5_000),
        audio_tail: None,
        audio_expected: false,
    };
    assert!(SegmentSample::new(1, 0, 1, 2_000, b).is_none());
    assert!(SegmentSample::new(1, 2, 1, 2_000, b).is_none());
    assert!(SegmentSample::new(0, 1, 1, 2_000, b).is_none());
    assert!(SegmentSample::new(1, 1, 1, 0, b).is_none());
}

#[test]
fn audio_expected_without_a_tail_has_no_buffer_but_silent_video_does() {
    let mut b = BufferSnapshot {
        playback: MediaTimeMs(1_000),
        video_tail: MediaTimeMs(5_000),
        audio_tail: None,
        audio_expected: true,
    };
    assert_eq!(
        b.buffered_ms(),
        None,
        "an A/V session whose audio lane has not spoken has an UNKNOWN reserve, not an empty one"
    );
    b.audio_expected = false;
    assert_eq!(b.buffered_ms(), Some(4_000));
}

/// **At the ladder floor the trigger fires and there is nothing to do — say so.** R12.
///
/// The behaviour is not new and must not be: `Rung::below()` is the identity at the bottom, so the
/// proposal was already skipped and `Stay` was already the answer. What was new is that the answer
/// said nothing — `decision=stay reason=None`, byte-identical to a healthy segment, on the one
/// state where the controller has exhausted every action it has and the picture is about to stop.
///
/// Differential on the reason, not on the decision: against the unmodified controller `reason` is
/// `None` here, and the two assertions below are what separate "there is nothing wrong" from
/// "there is nothing left".
#[test]
fn the_ladder_floor_is_a_stated_terminal_case_and_not_a_silent_stay() {
    let mut c = controller_at(Rung::P240);
    // A link far below what even the bottom rung asks for, and a reserve under one segment: both
    // halves of the emergency trigger, at the rung that has no rung below it.
    let decision = (0..4)
        .map(|_| c.observe_next(sample(64, 900, 500)))
        .last()
        .expect("four samples");
    assert_eq!(decision, Decision::Stay, "there is no lower rung to propose");
    assert_eq!(
        c.telemetry().reason,
        Some(DecisionReason::Hls(HlsReason::LadderFloor)),
        "the floor must be distinguishable from a healthy stay",
    );
}

/// The control case: one rung up from the floor, the same collapsed link proposes a downshift
/// rather than reporting the floor. Without this the test above is satisfied by a controller that
/// reports `LadderFloor` everywhere.
#[test]
fn one_rung_above_the_floor_the_same_link_still_has_somewhere_to_go() {
    let mut c = controller_at(Rung::P480);
    let decision = (0..4)
        .map(|_| c.observe_next(sample(64, 900, 500)))
        .find(|d| !matches!(d, Decision::Stay));
    assert!(
        matches!(decision, Some(Decision::Prime(p)) if p.direction == Direction::Down),
        "got {decision:?}",
    );
    assert_ne!(
        c.telemetry().reason,
        Some(DecisionReason::Hls(HlsReason::LadderFloor)),
        "a rung that CAN move is not at the floor",
    );
}

/// **A quiet audio lane must not be read as an empty buffer.** R11, and the bug it names.
///
/// A `BufferSnapshot` with `audio_expected` and no `audio_tail` is what every session looks like
/// for the first segment after an open and after every seek — with the video queue holding
/// whatever it holds, which on a fast link is the full 8 MiB. The old `buffered_ms` answered `0`,
/// and the emergency trigger reads the reserve as a level (`buffered < segment || starving()`), so
/// a full reserve produced a downshift proposal that nothing about the link or the server
/// justified — a fail-safe firing at the one moment it is guaranteed to be wrong.
///
/// Differential: the video lane here carries 12 s, which is six segments and far above every
/// threshold in the decision, so the ONLY thing that can produce a `Prime(Down)` is reading the
/// missing audio timestamp as an empty buffer. Against the previous code this asserts false.
#[test]
fn a_silent_audio_lane_does_not_fire_a_downshift_on_a_full_reserve() {
    let quiet = |video_ms: i64| {
        SegmentSample::new(
            2_000_000,
            1_000_000,
            1_200_000,
            2_000,
            BufferSnapshot {
                playback: MediaTimeMs(10_000),
                video_tail: MediaTimeMs(10_000 + video_ms),
                audio_tail: None,
                audio_expected: true,
            },
        )
        .unwrap()
    };
    let mut c = controller_at(Rung::P1080);
    // Two ordinary segments first, so the controller is past its cold start and the estimators
    // hold a healthy reserve — the state a mid-playback seek actually lands in.
    for _ in 0..2 {
        c.observe_next(sample(20_000, 400, 12_000));
    }
    let decision = c.observe_next(quiet(12_000));
    assert_eq!(
        decision,
        Decision::Stay,
        "a reserve that cannot be READ is not a reserve that is EMPTY"
    );
    assert!(c.pending().is_none(), "nothing may be proposed on an unknowable reserve");
}

/// The other half: the reserve genuinely being short still fires. Without this the test above is
/// satisfied by a controller that never downshifts at all.
#[test]
fn a_short_reserve_still_fires_a_downshift_when_the_lane_is_speaking() {
    let mut c = controller_at(Rung::P1080);
    for _ in 0..2 {
        c.observe_next(sample(20_000, 400, 12_000));
    }
    let decision = c.observe_next(sample(20_000, 400, 500));
    assert!(
        matches!(decision, Decision::Prime(p) if p.direction == Direction::Down),
        "a reserve below one segment is the emergency trigger; got {decision:?}"
    );
}

/// Plan I3 / integration regression: the first segment after bootstrap is a cold-start sample,
/// not evidence that an otherwise fast link needs a lower rung. At 1 958 ms both arms of the old
/// buffer trigger are true (`buffered < segment` and `starving()`), so this is differential against
/// gating only one of them.
#[test]
fn the_first_bootstrap_segment_cannot_false_downshift() {
    let mut controller = bootstrap_controller();
    assert_eq!(
        controller.observe_next(sample(40_000, 200, 1_958)),
        Decision::Stay,
        "one cold-start segment of reserve on a fast link is not a failing steady state",
    );
    assert!(controller.pending().is_none());
}

/// Resume resets positional state and rung residency, so its first observation has the same
/// cold-start contract as bootstrap. The control assertion on the second observation proves this
/// is a one-sample guard rather than a disabled emergency path.
#[test]
fn the_first_segment_after_resume_cannot_false_downshift() {
    let mut controller = bootstrap_controller();
    assert_eq!(controller.observe_next(sample(40_000, 200, 12_000)), Decision::Stay);
    controller.on_resume(30_000);

    assert_eq!(controller.observe_next(sample(40_000, 200, 1_958)), Decision::Stay);
    assert!(controller.pending().is_none());
    assert!(
        matches!(
            controller.observe_next(sample(40_000, 200, 1_958)),
            Decision::Prime(Proposal { direction: Direction::Down, .. })
        ),
        "the same short reserve on the second sample must reach the emergency path",
    );
}

/// **An unknown reserve is not an observation, and the estimator must not treat it as one.**
///
/// The estimator carries `last_delta_ms` unsmoothed precisely so the emergency guard can see a
/// cliff, so entering a `None` as a zero would manufacture the exact signal that guard exists to
/// react to. Differential: against a `update(0, …)` this fails on every assertion below.
#[test]
fn an_unknown_reserve_advances_no_estimate_and_no_counter() {
    let mut buffer = BufferEstimate::default();
    buffer.update(Some(12_000), 2_000);
    buffer.update(Some(11_000), 2_000);
    let after_two = buffer;
    buffer.update(None, 2_000);
    assert_eq!(buffer.buffered_ms, after_two.buffered_ms, "the level must not move");
    assert_eq!(buffer.slope_ms_per_s, after_two.slope_ms_per_s, "the slope must not move");
    assert_eq!(buffer.last_delta_ms, after_two.last_delta_ms, "no fabricated cliff");
    assert_eq!(buffer.samples, after_two.samples, "an absence is not a sample");
    assert_eq!(buffer.draining_samples, after_two.draining_samples);
}

/// A window shorter than the measurement window is not a measurement.
#[test]
fn original_windows_shorter_than_the_measurement_window_are_not_evidence() {
    let mut mode = original(28_000);
    assert!(mode
        .observe_saturated(window_bytes(4_000), ORIGINAL_WINDOW_US - 1, Some(3_000), HOUR_MS)
        .is_none());
    let first = mode
        .observe_saturated(window_bytes(4_000), ORIGINAL_WINDOW_US, Some(3_000), HOUR_MS)
        .unwrap();
    assert_eq!(first.measured_kbps, 4_000);
    assert_eq!(first.requirement_kbps, 37_800, "28 Mbit/s average + VBR headroom");
}

/// **The 2026-08-25 rewrite, in one assertion.** 4 Mbit/s against a 28 Mbit/s file used to be
/// "two slow windows, switch". Whether it is a problem depends entirely on the reserve, and
/// the old rule could not see that: 60 s of buffer survives this deficit for a minute.
#[test]
fn a_deficit_with_a_deep_reserve_is_arithmetic_not_an_emergency() {
    let mut mode = original(28_000);
    for window in 1..=4 {
        let observation = mode
            .observe_saturated(
                window_bytes(4_000) * window,
                ORIGINAL_WINDOW_US * window,
                Some(60_000),
                HOUR_MS,
            )
            .unwrap();
        assert!(
            observation.fallback.is_none(),
            "window {window}: {}s of headroom is not a reason to reload the pipeline",
            observation.horizon_secs.unwrap_or(0),
        );
        assert!(observation.horizon_secs.unwrap_or(0) >= 60);
    }
}

/// The same rate against the same file with a shallow reserve IS an emergency, and the reason
/// code says which rule fired. It also names the replacement state — the best the collapsed
/// link sustains, never the bottom of the ladder.
///
/// **RE-EXPRESSED 2026-08-27, and it was asserting the defect.** This used to fire on the FIRST
/// window, which is exactly what `docs/measurements/orig-first-window-fallback.md` recorded going
/// wrong on a real film: at window 1 `uncertainty_pm` is pinned to its 500 pm floor, so `safe` is
/// half of `measured` by construction, and there is no reserve derivative to contradict it. The
/// collapse is still graded — it just has to be OBSERVED, over two windows, with the reserve
/// actually falling. That is the differential: the second window is what the old code did not
/// need and the new code does.
#[test]
fn a_collapse_leaves_original_for_the_best_sustainable_state() {
    let mut mode = original(28_000);
    let cold = mode
        .observe_saturated(window_bytes(4_000), ORIGINAL_WINDOW_US, Some(8_000), HOUR_MS)
        .unwrap();
    assert_eq!(cold.fallback, None, "the first window refines the estimators and decides nothing");
    let observation = mode
        .observe_saturated(window_bytes(4_000) * 2, ORIGINAL_WINDOW_US * 2, Some(5_000), HOUR_MS)
        .unwrap();
    assert_eq!(observation.fallback, Some(OriginalExit::ImminentStarvation));
    assert_eq!(observation.target, Some(Rung::P720Low), "3.2 Mbit/s of proven capacity");
}

/// **The device finding, as an assertion** (`docs/measurements/orig-first-window-fallback.md`).
/// A 42 365 kbps link carrying a 25 264 kbps file: the link comfortably covers it, the reserve is
/// 85 ms only because the prime was just consumed, and it is GROWING. The old code returned
/// `ImminentStarvation` here and replaced 4K Dolby Vision direct play with a 1080p transcode for
/// the rest of the film.
///
/// Differential by construction: every term is the one the log carried, and against unmodified
/// code the first `assert` fails.
#[test]
fn the_prime_remnant_is_not_a_starving_reserve() {
    let mut mode = original(25_264);
    let first = mode
        .observe_saturated(window_bytes(42_365), ORIGINAL_WINDOW_US, Some(85), HOUR_MS)
        .unwrap();
    // 42_364 rather than 42_365: `window_bytes` truncates, and so does the kbps division
    // back out of it. One kbit/s in forty-two megabits changes nothing here.
    assert_eq!(first.measured_kbps, 42_364);
    assert!(
        first.requirement_kbps > first.conservative_kbps,
        "the manufactured deficit is still there — {}kbps needed against {}kbps 'safe' — and that \
         is the point: it is arithmetic on an uncertainty floor, not an observation",
        first.requirement_kbps,
        first.conservative_kbps,
    );
    assert_eq!(first.fallback, None, "window 1 may not abandon 4K direct play");
    assert_eq!(first.unsafe_deficit_ms, 0, "nor may it count toward the sustained-deficit tally");

    // …and the reserve then GROWS, exactly as the film's log showed (+113 ms/s). Nothing about
    // the next window is a deficit either.
    let second = mode
        .observe_saturated(window_bytes(42_365) * 2, ORIGINAL_WINDOW_US * 2, Some(1_200), HOUR_MS)
        .unwrap();
    assert_eq!(second.fallback, None, "a filling reserve on a link that covers the file");
}

/// **The same film, eight windows in — where `the_prime_remnant_is_not_a_starving_reserve` stops
/// looking.** (Device, 2026-08-29.)
///
/// That test guards windows 1 and 2, which is where `orig-first-window-fallback.md` found the
/// defect. The guard it installed — `last_delta_ms < 0` — protects those two windows for a reason
/// that expires: window 1 has no derivative at all, and window 2's is the whole of a rising prime.
/// From window 3 on, the raw delta is a **sign test on a signal whose quantisation is comparable
/// to its per-window travel**, and the reserve is `min(video_tail, audio_tail) - playpos` with
/// `playpos` off a 5 Hz callback — so one negative sample out of eight is close to certain on a
/// perfectly healthy link.
///
/// This is that film's actual failure, in its own numbers: a 25 264 kbps 4K Dolby Vision + Atmos
/// source on a link measuring 31 037 kbps, reserve climbing ~600 ms per window, and ONE window
/// where it dips 114 ms — a fifth of the quantisation step. The horizon is 17 s against a
/// `starvation_fallback_secs` of 20, so the old conjunction fires and the film loses 4K direct
/// play for a 720p transcode. The log line was
/// `ImminentStarvation ... buf=4814ms slope=1020ms/s starve=16` — a starvation verdict beside a
/// filling reserve.
///
/// Differential by construction: against unmodified code the final `assert` fails.
#[test]
fn one_quantisation_dip_in_a_filling_reserve_is_not_a_starvation() {
    let mut mode = original(25_264);
    // Reserve as the device carried it: the prime remnant, then ~600 ms of gain per window.
    let climb = [749_i64, 1_300, 1_900, 2_500, 3_100, 3_700, 4_300, 4_814];
    for (i, buffered) in climb.iter().enumerate() {
        let windows = (i + 1) as u64;
        let observation = mode
            .observe_saturated(
                window_bytes(31_037) * windows,
                ORIGINAL_WINDOW_US * windows,
                Some(*buffered),
                HOUR_MS,
            )
            .unwrap();
        assert_eq!(
            observation.fallback, None,
            "window {windows}: the reserve is rising and the film is playing at speed",
        );
    }
    // The dip. 114 ms against a ~200 ms quantisation step and a ~600 ms per-window trend: this is
    // the measurement's noise floor, not an observation about the link.
    let windows = climb.len() as u64 + 1;
    let dip = mode
        .observe_saturated(
            window_bytes(31_037) * windows,
            ORIGINAL_WINDOW_US * windows,
            Some(4_700),
            HOUR_MS,
        )
        .unwrap();
    assert!(
        dip.slope_ms_per_s > 0,
        "the smoothed reserve is still rising ({}ms/s) — which is the evidence the verdict has to \
         answer to",
        dip.slope_ms_per_s,
    );
    // **This assertion was inverted on 2026-08-29 and the inversion is the record of a second,
    // deeper fix.** It used to require the horizon to be INSIDE `starvation_fallback_secs`, on the
    // grounds that only the derivative was keeping the branch shut. That was true while the
    // eviction horizon was computed on `conservative_kbps`: `T = B·R/(R−C)` with `C` discounted
    // put `T` at 17 s here, and — because `T` is increasing in `B` while `B` is capped by the
    // plant ceiling — it could not have escaped the band at ANY reserve this playback could
    // reach. The horizon half was permanently armed and the derivative was the only thing left.
    // Computing it on the measured rate, which is the rule `controller.rs` already followed for
    // HLS, moves it to 52 s. Both guards now hold, independently, and that is the point: the
    // derivative closed the channel, and the basis fix disarmed the condition behind it.
    assert!(
        dip.horizon_secs.is_none_or(|s| s > 20),
        "the eviction horizon is computed on the measured rate now, so a link carrying the file \
         has no imminent horizon at all — got {:?}s",
        dip.horizon_secs,
    );
    assert_eq!(
        dip.fallback, None,
        "one negative sample in a filling reserve may not cost a reload and a visible blink",
    );
}

/// **Conservatism belongs to admission, not to eviction — and Original must say so in the same
/// words `controller.rs` does.**
///
/// The HLS side computes its emergency horizon on `immediate_network`, the measured rate floored
/// by the fast estimate, and its comment calls the choice load-bearing: *"It does not belong to
/// EVICTION, where the claim is that the link in front of you cannot carry what is already
/// playing, and the evidence for that has to be observed rather than discounted into existence."*
/// Original computed the same horizon on `conservative_kbps` and inherited exactly the failure
/// that comment predicts.
///
/// It is not a matter of degree, because the plant has a ceiling. `T = B·R/(R−C)` is increasing in
/// `B`, and `B ≤ B_max = lead + queue_bytes·8/R` — about 5 s for a source this size. On the
/// discounted rate that gives `T_max ≈ 17 s`, permanently inside `starvation_fallback_secs`: the
/// horizon half of the imminent test was satisfied on **every window the playback could produce**,
/// including a completely full buffer. On the measured rate the same ceiling gives ~55 s.
///
/// Both readings are asserted here so the parity cannot regress silently in either direction.
#[test]
fn the_eviction_horizon_is_measured_not_discounted() {
    let source = 25_264;
    let requirement = source_requirement_kbps(source, &AbrPolicy::measured());
    let mut mode = original(source);
    // A link measuring 31 037 kbps against a 25 264 kbps file: 1.23x, carrying it with room.
    for window in 1..=4_u64 {
        mode.observe_saturated(
            window_bytes(31_037) * window,
            ORIGINAL_WINDOW_US * window,
            Some(1_000 * i64::try_from(window).unwrap()),
            HOUR_MS,
        )
        .unwrap();
    }
    let settled = mode
        .observe_saturated(window_bytes(31_037) * 5, ORIGINAL_WINDOW_US * 5, Some(4_814), HOUR_MS)
        .unwrap();

    // The published `safe=` is still the discounted number, because it still chooses the fallback
    // RUNG — an admission decision, and the one place the discount belongs.
    assert!(
        settled.conservative_kbps < requirement,
        "the discounted rate is still below the requirement ({} < {}) — which is precisely why \
         computing the horizon on it manufactured a deficit",
        settled.conservative_kbps,
        requirement,
    );
    // What the DECISION is taken on is the measurement, and the measurement covers the file.
    assert!(
        settled.measured_kbps < requirement,
        "even the raw rate is under the VBR-inflated requirement, so this is not a case the \
         allowance alone rescues",
    );
    assert!(
        settled.horizon_secs.is_none_or(|s| s > AbrPolicy::measured().starvation_fallback_secs),
        "a link delivering 1.23x the source may not read as imminent starvation — got {:?}s",
        settled.horizon_secs,
    );
    assert_eq!(settled.fallback, None);

    // And the rule still bites when the link really does fall behind: same reserve, a rate a
    // quarter of the source.
    let mut collapsed = original(source);
    collapsed
        .observe_saturated(window_bytes(6_000), ORIGINAL_WINDOW_US, Some(6_000), HOUR_MS)
        .unwrap();
    let falling = collapsed
        .observe_saturated(window_bytes(6_000) * 2, ORIGINAL_WINDOW_US * 2, Some(3_000), HOUR_MS)
        .unwrap();
    assert_eq!(
        falling.fallback,
        Some(OriginalExit::ImminentStarvation),
        "an observed collapse still evicts — the basis changed, not the rule",
    );
}

/// **A stream that has just joined has a small reserve BY CONSTRUCTION, and the emergency guard
/// may not read that as the reserve being gone.** (Host simulator, 2026-08-29.)
///
/// Every mode entry begins here: the `Load` completes, the prime is consumed, and the reserve
/// starts at a few hundred milliseconds and climbs. `emergency_buffer_ms` is 2 000, so the whole
/// warm-up sits underneath it, and the only thing separating "warming up" from "about to stall" is
/// the DIRECTION — which is why the level alone cannot decide it.
///
/// This is what the fix for `one_quantisation_dip_in_a_filling_reserve_is_not_a_starvation`
/// uncovered: with the imminent branch and the deficit tally both requiring an observed drain, the
/// simulator's 4K Dolby Vision recovery blinked anyway, five seconds after a CORRECT recovery,
/// on `EmergencyLowBuffer measured=25911kbps safe=21168kbps need=21104kbps buf=1181ms
/// slope=1113ms/s starve=none`. An infinite starvation horizon — the model's own capacity test
/// saying the link was sufficient — beside a reserve refilling at better than real time, and a
/// reload anyway.
///
/// Differential by construction: against unmodified code the final `assert` fails.
#[test]
fn a_reserve_refilling_after_a_join_is_not_an_emergency() {
    let mut mode = original(15_633);
    // The prime remnant, then the reserve climbing ~1.1 s per window — a link comfortably ahead.
    for (window, buffered) in [(1_u64, 300_i64), (2, 1_400)] {
        let observation = mode
            .observe_saturated(
                window_bytes(25_911) * window,
                ORIGINAL_WINDOW_US * window,
                Some(buffered),
                HOUR_MS,
            )
            .unwrap();
        assert_eq!(observation.fallback, None, "window {window} of a warm-up");
    }
    // One negative raw sample, still deep inside `emergency_buffer_ms`. This is the exact shape
    // the guard fired on.
    let dip = mode
        .observe_saturated(window_bytes(25_911) * 3, ORIGINAL_WINDOW_US * 3, Some(1_181), HOUR_MS)
        .unwrap();
    assert!(
        dip.buffered_ms <= AbrPolicy::measured().emergency_buffer_ms,
        "the reserve must really be inside the emergency band, or this grades nothing",
    );
    assert!(
        dip.slope_ms_per_s > 0,
        "and it must really be refilling ({}ms/s)",
        dip.slope_ms_per_s,
    );
    assert_eq!(
        dip.fallback, None,
        "a reserve climbing out of the prime is a stream starting, not a stream starving",
    );
}

/// **The reserve derivative is per WALL second, because that is what spends the reserve.**
///
/// `ORIGINAL_WINDOW_US` deliberately measures capacity over ACTIVE body-read time, so a reader
/// parked on backpressure does not measure as a slow link. Feeding that same denominator to the
/// buffer estimate was a units error: parking is what a HEALTHY link does, so `t_active < t_wall`
/// exactly when the reserve is filling, and the slope came out inflated by `t_wall / t_active`.
/// Device-measured: a printed `slope=1020ms/s` against a real +508 ms per wall second.
///
/// It matters because `slope_ms_per_s` is then compared against `DRAIN_EPS_MS_PER_S`, which is
/// stated in wall seconds, and sits beside `starvation_horizon`, which is wall seconds throughout.
///
/// `observe_saturated` cannot see this — it passes `now_ms = active_us / 1_000`, making the two
/// clocks identical — so this test calls `observe` directly, which is the only way to separate
/// them.
#[test]
fn the_reserve_slope_is_measured_on_the_wall_clock_not_on_read_time() {
    let mut mode = original(25_264);
    // One window of active read that took twice as long in wall time: the reader spent half of it
    // parked on a full queue, which is the healthy case.
    mode.observe(window_bytes(31_037), ORIGINAL_WINDOW_US, Some(1_000), HOUR_MS, 1_500)
        .unwrap();
    let second = mode
        .observe(
            window_bytes(31_037) * 2,
            ORIGINAL_WINDOW_US * 2,
            Some(2_500),
            HOUR_MS,
            3_000,
        )
        .unwrap();
    // 1 500 ms of reserve gained over 1 500 ms of wall clock is +1 000 ms/s. Over the 750 ms of
    // active read the same gain would read +2 000 ms/s — the doubling this test exists to refuse.
    assert_eq!(
        second.slope_ms_per_s, 1_000,
        "gained 1500ms of reserve across 1500ms of wall clock",
    );
}

/// A moderate deficit that will not go away eventually loses the argument on its own — before
/// starvation is imminent, and with no counter deciding anything by itself.
///
/// **RE-EXPRESSED 2026-08-29: the reserve now FALLS, and holding it constant was asserting the
/// defect** — the same shape as `a_collapse_leaves_original_for_the_best_sustainable_state`'s own
/// re-expression, for the same reason.
///
/// This fixture used to hold `Some(30_000)` across all fourteen windows, and that world is
/// self-contradictory: a reserve that neither grows nor shrinks while the film plays says delivery
/// EQUALS consumption exactly, which is the definition of a link that is carrying the file. The
/// only thing asserting a deficit there was `R − C` — `vbr_allowance_pm` inflating the requirement
/// and `uncertainty_pm` discounting the measurement, against each other, with no observation on
/// either side. Counting that toward `sustained_unsafe_deficit_ms` is what let a 4K Dolby Vision
/// film be abandoned with its reserve rising at +783 ms/s (see
/// `one_quantisation_dip_in_a_filling_reserve_is_not_a_starvation`).
///
/// It also could not stay: a flat reserve is what SATURATION looks like — `B_max` is the queue
/// caps plus the pump's feed-ahead lead — so the old fixture's own steady state was the one state
/// that most conclusively refutes starvation.
///
/// So the shortfall is now observable: 200 ms of reserve lost per window, a slope of −266 ms/s,
/// past `DRAIN_EPS_MS_PER_S` and nowhere near imminent. What the test grades is unchanged — a
/// persistent, non-imminent deficit eventually loses the utility argument — and it now grades it
/// on a scenario that can happen.
#[test]
fn a_deficit_that_persists_costs_original_the_argument() {
    let mut mode = original(60_000);
    let mut exits = Vec::new();
    for window in 1..=14 {
        let observation = mode
            .observe_saturated(
                window_bytes(50_000) * window,
                ORIGINAL_WINDOW_US * window,
                Some(20_000 - 200 * i64::try_from(window).unwrap()),
                HOUR_MS,
            )
            .unwrap();
        assert!(
            observation.horizon_secs.unwrap_or(0) > AbrPolicy::measured().starvation_fallback_secs,
            "this scenario must never become imminent, or it grades the wrong rule",
        );
        exits.push((window, observation.fallback));
    }
    assert!(
        exits
            .iter()
            .take(usize::from(ORIGINAL_DEFICIT_WINDOWS) - 1)
            .all(|(_, exit)| exit.is_none()),
        "anything shorter than the persistence window is a dip: {exits:?}",
    );
    assert!(
        exits
            .iter()
            .any(|(_, exit)| *exit == Some(OriginalExit::SustainedDeficit)),
        "a shortfall that persists must eventually move: {exits:?}",
    );
}

/// The **labelled emergency guard**: estimates say the link is fine, the reserve says otherwise.
/// Unreachable when the model works, which is exactly why it is named in the log.
#[test]
fn the_emergency_guard_fires_when_the_reserve_is_gone_anyway() {
    let mut mode = original(28_000);
    // Two agreeing windows are what make 60 Mbit/s believable enough to clear a 37.8 Mbit/s
    // requirement outright — one sample is discounted by half on principle.
    for window in 1..=2 {
        let healthy = mode
            .observe_saturated(
                window_bytes(60_000) * window,
                ORIGINAL_WINDOW_US * window,
                Some(5_000),
                HOUR_MS,
            )
            .unwrap();
        assert!(healthy.fallback.is_none());
    }
    assert_eq!(
        mode.observe_saturated(
            window_bytes(60_000) * 3,
            ORIGINAL_WINDOW_US * 3,
            Some(5_000),
            HOUR_MS,
        )
        .unwrap()
        .horizon_secs,
        None,
        "60 Mbit/s carries a 28 Mbit/s file, and by now the estimate agrees",
    );
    let collapsed = mode
        .observe_saturated(
            window_bytes(60_000) * 4,
            ORIGINAL_WINDOW_US * 4,
            Some(1_500),
            HOUR_MS,
        )
        .unwrap();
    assert_eq!(collapsed.horizon_secs, None, "the delivery estimate still says it is fine");
    assert_eq!(collapsed.fallback, Some(OriginalExit::EmergencyLowBuffer));
}

/// Nothing can starve a reserve that outlasts the content, which is also why the closing
/// minutes need no special case anywhere.
#[test]
fn a_reserve_that_covers_the_rest_of_the_film_never_falls_back() {
    let mut mode = original(28_000);
    let observation = mode
        .observe_saturated(window_bytes(1_000), ORIGINAL_WINDOW_US, Some(20_000), 15_000)
        .unwrap();
    assert!(observation.horizon_secs.unwrap_or(u32::MAX) < 60, "a real deficit");
    assert!(observation.fallback.is_none(), "20 s buffered, 15 s left to play");
}

/// A seek keeps the link estimate and drops everything positional. The link did not change
/// because the viewer jumped.
#[test]
fn a_seek_keeps_the_link_estimate_and_drops_the_position() {
    let mut mode = original(28_000);
    for window in 1..=3 {
        mode.observe_saturated(
            window_bytes(50_000) * window,
            ORIGINAL_WINDOW_US * window,
            Some(30_000),
            HOUR_MS,
        );
    }
    let before = mode.delivery;
    mode.on_seek(0, 0);
    assert_eq!(mode.delivery, before, "a seek is not news about the network");
    assert_eq!(mode.buffer, BufferEstimate::default());
    assert_eq!(mode.unsafe_deficit_ms, 0);
    // ...and the counters really did rewind, so the next window measures from zero rather than
    // reading a negative delta as a collapse.
    assert!(mode
        .observe_saturated(window_bytes(50_000), ORIGINAL_WINDOW_US, Some(1_000), HOUR_MS)
        .is_some());
}

/// A pause is the one gap where wall-clock time passes with nothing measured.
#[test]
fn a_long_pause_turns_the_estimate_into_a_weak_prior() {
    let mut mode = original(28_000);
    for window in 1..=4 {
        mode.observe_saturated(
            window_bytes(50_000) * window,
            ORIGINAL_WINDOW_US * window,
            Some(30_000),
            HOUR_MS,
        );
    }
    let confident = mode.delivery.conservative_kbps();
    mode.on_resume(10 * 60 * 1_000);
    let stale = mode.delivery.conservative_kbps();
    assert!(stale < confident, "{stale} vs {confident}");
    assert_eq!(mode.delivery.slow_kbps, 50_000, "the VALUE survives; the confidence does not");
    assert_eq!(mode.delivery.samples, 1);
}

#[test]
fn measured_runtime_fallback_avoids_an_unnecessarily_low_bootstrap() {
    let policy = AbrPolicy::measured();
    let rung = |measured| original_fallback_rung(measured, &hd_catalog(), &policy);
    assert_eq!(rung(512), Rung::P240);
    assert_eq!(rung(4_000), Rung::P720Low);
    assert_eq!(rung(7_000), Rung::P720);
    assert_eq!(rung(30_000), Rung::P1080High, "a fast link is not a reason to hold back");
    assert_eq!(
        rung(1),
        Rung::P240,
        "below every candidate, take the floor rather than refusing to move",
    );
    assert_eq!(Rung::from_ceiling(Rung::P720Low.ceiling()), Some(Rung::P720Low));
}

#[test]
fn realtime_jit_blocks_upshift_without_forcing_a_downshift() {
    let mut controller = bootstrap_controller();
    for _ in 0..10 {
        assert_eq!(controller.observe_next(sample(20_000, 1_000, 10_000)), Decision::Stay);
    }
    assert_eq!(controller.current(), Rung::P480);
}

#[test]
fn a_proposal_does_not_mutate_current_until_candidate_commit() {
    let mut controller = bootstrap_controller();
    let proposal = prime_up(&mut controller);
    assert_eq!(proposal.rung, Rung::P1080M12);
    assert_eq!(controller.current(), Rung::P480);
    assert_eq!(controller.pending(), Some(proposal));
    assert!(controller.candidate_ready(
        proposal,
        sample(20_000, 200, 12_000),
        declared_bps(proposal.rung),
    ));
    assert!(controller.commit(proposal, controller.clock_ms()));
    assert_eq!(controller.current(), Rung::P1080M12);
}

#[test]
fn rejected_candidate_preserves_current_and_clears_pending() {
    let mut controller = bootstrap_controller();
    let proposal = prime_up(&mut controller);
    // **A candidate whose own encoder is at or past real time.** The old gate here was a bare
    // `800`, i.e. the single-observation form `A <= 0.8 D` the device corpus refutes at ~37%
    // violation; the disqualifier is now `production_max_pm`, which is a named policy threshold
    // meaning "this JIT encoder cannot keep up". 1200 pm is 2.4 s of production for a 2 s segment.
    // The link is not the question — the window is full of fast samples and still cannot save it.
    assert!(!controller.candidate_ready(
        proposal,
        sample(2_100, 1_200, 12_000),
        declared_bps(proposal.rung),
    ));
    assert!(controller.reject(proposal, RejectCause::Candidate, controller.clock_ms()));
    assert_eq!(controller.current(), Rung::P480);
    assert_eq!(controller.pending(), None);
}

#[test]
fn startup_does_not_issue_back_to_back_encoder_swaps() {
    let mut controller = bootstrap_controller();
    let proposal = prime_up(&mut controller);
    controller.commit(proposal, controller.clock_ms());
    for _ in 0..3 {
        assert_eq!(controller.observe_next(sample(20_000, 200, 12_000)), Decision::Stay);
    }
}

/// Once the current rung has one non-cold observation, one slow sample is acted on immediately —
/// a downshift is an invisible transaction and the alternative is a stall — but it is acted on
/// CONSERVATIVELY: a single measurement carries the maximum discount, so 1 Mbit/s is treated as
/// 0.5 Mbit/s of proven capacity and the target is the emergency floor rather than the rung just
/// below. The next agreeing samples are what buy the way back up. The preceding sample separates
/// this runtime-collapse policy test from I3's cold-start suppressor; the expected target is the
/// original policy assertion and is unchanged.
#[test]
fn a_single_slow_network_sample_jumps_to_the_measured_sustainable_rung() {
    let mut controller = bootstrap_controller();
    controller.current = Rung::P720;
    assert_eq!(controller.observe_next(sample(20_000, 400, 8_000)), Decision::Stay);
    let decision = controller.observe_next(sample(1_000, 400, 8_000));
    assert_eq!(
        decision,
        Decision::Prime(Proposal { rung: Rung::P240, direction: Direction::Down })
    );
    assert_eq!(controller.current(), Rung::P720);
}

/// **A failed DOWNSHIFT must not arm the guard that refuses climbing** (a correction to N11's
/// backoff, found by adversarial review of I6).
///
/// `RejectBlock` prices repeating a spend the controller CHOSE to make — that is the whole
/// affordability argument in its doc, and `dwell_until_ms` already draws the same line for the
/// dwell: "a downshift is a recovery action and rate-limiting recovery is how a stall becomes a
/// policy". `reject` did not look at the direction, and on the down path the block it armed had no
/// clock release at all: `refill_time_ms` returns `None` whenever `safe_budget <= R_current`,
/// which is precisely the state a collapse-driven downshift is in. The only remaining exit was the
/// safe budget exceeding `slow_kbps` as of the failure — the link having to beat its own
/// pre-collapse reading — so a partial recovery left every upshift refused for the life of the
/// demux while playback sat far below what the link could carry.
///
/// Differential by construction, and it carries its own control: the same reject on an UP proposal
/// must still arm the block. Asserting only the down half would pass against a `reject` that armed
/// nothing at all.
#[test]
fn a_failed_downshift_does_not_arm_the_guard_that_refuses_climbing() {
    // A collapse from P720 — the shape `a_single_slow_network_sample_jumps_to_the_measured_
    // sustainable_rung` pins, and the state in which `safe_budget <= R_current` holds.
    let mut c = bootstrap_controller();
    c.current = Rung::P720;
    assert_eq!(c.observe_next(sample(20_000, 400, 8_000)), Decision::Stay);
    let Decision::Prime(down) = c.observe_next(sample(1_000, 400, 8_000)) else {
        panic!("a collapsed link must propose a downshift");
    };
    assert_eq!(down.direction, Direction::Down);
    assert!(c.reject(down, RejectCause::Candidate, c.clock_ms()));
    assert_eq!(
        c.telemetry().gates.blocked_kbps,
        0,
        "a downshift the plant compelled must leave no backoff behind: the reserve it spent is not \
         evidence that a later CLIMB is unaffordable, and the estimator can see the collapse \
         directly",
    );

    // The control. An upshift is discretionary, so its failure is exactly what the guard is for.
    let mut c = bootstrap_controller();
    let up = prime_up(&mut c);
    assert_eq!(up.direction, Direction::Up);
    assert!(c.reject(up, RejectCause::Candidate, c.clock_ms()));
    assert!(
        c.telemetry().gates.blocked_kbps > 0,
        "a failed upshift must still arm the block, or the assertion above grades nothing",
    );
}

#[test]
fn a_runtime_collapse_from_the_top_does_not_prime_oversized_intermediate_rungs() {
    let mut controller = bootstrap_controller();
    controller.current = Rung::P1080High;
    assert_eq!(controller.observe_next(sample(40_000, 400, 8_000)), Decision::Stay);
    assert_eq!(
        controller.observe_next(sample(512, 1_000, 8_000)),
        Decision::Prime(Proposal { rung: Rung::P240, direction: Direction::Down })
    );
}

/// A loaded server draining the reserve must move the rung; a loaded server holding it steady must
/// not. The contrast is the whole test — `prod` is identical in both legs, so only the reserve's
/// TRAJECTORY can be doing the work.
///
/// **The draining leg now fires four samples earlier, and that is the point rather than a
/// tolerance change.** It used to walk 8000 -> 6000 -> 4000 -> 3000 -> 2500 -> 2000 and react only
/// at the last one. The reserve loses a full segment's worth of content per segment throughout —
/// draining at real time from the second sample onward — against a server reporting 1200 pm, i.e.
/// 20% behind real time. `production_bad` is exactly the conjunction of those two, so it should
/// have fired at the first sample carrying a real delta. It did not, because
/// `BufferEstimate::update` seeded `slope_ms_per_s` from a FABRICATED one: `self.buffered_ms`
/// starts at zero, so sample one entered the whole 8 s reserve as a `+4000 ms/s` FILL, and the 3:1
/// EWMA still read `+2750` at sample two. `draining()` was therefore false and the conjunction
/// could not hold, so four further samples of `Stay` were not the policy being careful — they were
/// the fabrication decaying. It was optimistic in the one direction that matters, which is
/// `[[reserve-cannot-see-a-slow-film]]`'s shape exactly.
#[test]
fn draining_jit_session_downshifts_but_stable_jit_does_not() {
    // The drain, reacted to on the first sample that can see it. Sample one records the level and
    // no slope — a slope needs two observations — so sample two carries the first real delta.
    let mut draining = bootstrap_controller();
    assert_eq!(draining.observe_next(sample(20_000, 1_200, 8_000)), Decision::Stay);
    assert_eq!(
        draining.observe_next(sample(20_000, 1_200, 6_000)),
        Decision::Prime(Proposal { rung: Rung::P240, direction: Direction::Down }),
        "one real delta is enough here because TWO independent conditions agree on it",
    );
    assert_eq!(
        draining.telemetry().reason,
        Some(DecisionReason::Hls(HlsReason::ProductionConstraint)),
        "and it must be the production arm: `production_bad` is the server being behind real time \
         AND the reserve draining, which is what the fabricated seed used to hide — at this sample \
         the old slope was +2750 ms/s, so `draining()` was false and a server 20% behind real time \
         with a reserve falling at real time read as healthy",
    );

    // The control, and the half the name promises: the same production load with a reserve that is
    // NOT falling must move nothing, however long it runs.
    let mut stable = bootstrap_controller();
    for _ in 0..8 {
        assert_eq!(
            stable.observe_next(sample(20_000, 1_200, 8_000)),
            Decision::Stay,
            "a loaded server is not a reason to move while the reserve is holding",
        );
    }
}

fn recovery(source_kbps: u32) -> OriginalRecovery {
    OriginalRecovery::new(
        source_kbps,
        AbrPolicy::measured(),
        SourceFeatures::default(),
        TransitionHistory::default(),
        hd_catalog(),
    )
    .unwrap()
}

/// The rung the recovery fixtures are PLAYING when the probe lands. It is the value the code used
/// to fabricate for both sides of the comparison, so passing it as `current` holds that half still
/// and isolates what actually moved: the ALTERNATIVE is now chosen by `best_sustainable`, and the
/// server is now the measured one.
fn top_candidate() -> HlsCandidate {
    hd_catalog().candidate(Rung::P1080High)
}

/// The server the recovery fixtures compare against. Its own default — an idle PMS — which is what
/// these tests meant all along; the difference is that the gate now has to be TOLD, so a fixture
/// that wants a loaded server can say so.
fn idle_server() -> ProductionEstimate {
    ProductionEstimate::default()
}

/// The HLS session the recovery decision is compared AGAINST — healthy, since that is the
/// interesting case: a starved one would make Original win by default.
///
/// **It has to be DERIVED from the rung the comparison scores, and a flat `from_prior(30_000)`
/// was not healthy at all.** `from_prior` pins `uncertainty_pm` at its cap, so a prior's
/// conservative reading is exactly half of it — 15 000 kbps against the 20 011 kbps top rung
/// `observe_probe` compares with, i.e. a session whose own conservative arithmetic says it drains
/// a 12 s reserve in 47 s. The old four-step risk ladder flattened that whole band to 4 points and
/// hid it; N5's continuous form charges 13, and
/// `recovery_does_not_pay_for_a_reload_at_the_end_of_a_film` turned out to have been passing by
/// ONE point out of a comparison whose terms are tens — grading the pinned-prior artefact rather
/// than the reload amortisation it is named for.
///
/// So: four times the top rung's wire rate, which after the cap's halving leaves the conservative
/// reading at twice what that rung needs. Nothing here is chosen — the factor is the cap, and the
/// rate is the candidate.
fn healthy_hls() -> CapacityEstimate {
    let top = HlsActuatorCatalog::measured().candidate(Rung::P1080High).expected_wire_kbps;
    CapacityEstimate::from_prior(top.saturating_mul(4))
}

fn healthy_buffer() -> BufferEstimate {
    BufferEstimate { buffered_ms: 12_000, slope_ms_per_s: 0, ..Default::default() }
}

fn probe(kbps: u32, completed: bool) -> CapacityObservation {
    CapacityObservation { kbps, bytes: 2_000_000, active_us: 400_000, completed }
}

/// **A mid-ladder rung with spare capacity may probe, once the spacing has ELAPSED.**
///
/// Category 8.3. Two changes are folded here and both were the same defect at different layers.
/// The old gate required the TOP rung, which measured the wrong resource: PMS producing 20 Mbit/s
/// of H.264 says the SERVER can encode and says nothing about whether the link can carry a
/// 28 Mbit/s remux. And the spacing counted three HLS SEGMENTS (N13) — a segment duration is a
/// client REQUEST the server may ignore, and the constant sat behind an `ORIGINAL_` prefix shared
/// with a counter of 750 ms active-read windows, two unrelated clocks under one name.
///
/// Derived rather than counted: the assertion is that nothing probes before `probe_spacing_ms` of
/// healthy wall clock has accumulated, and that something does at it. `ORIGINAL_PROBE_SPACING`
/// survives `#[cfg(test)]` so the carry-over can be stated — the new duration IS the old count at
/// the segment length this pipeline requests, which is why no expectation moves.
#[test]
fn original_recovery_probes_from_any_rung_once_the_spacing_has_elapsed() {
    let policy = AbrPolicy::measured();
    assert_eq!(
        policy.probe_spacing_ms,
        u64::from(ORIGINAL_PROBE_SPACING) * 2_000,
        "the duration must be the retired count at the requested segment length, or this is a \
         policy change wearing a unit conversion",
    );

    let mut gate = recovery(28_000);
    let current = hd_catalog().candidate(Rung::P720);
    let spare = CapacityEstimate::from_prior(30_000);
    let mut now = 0u64;
    let mut fired_at = None;
    for _ in 0..8 {
        now += 2_000;
        if gate.probe_due(
            current, &idle_server(), sample(20_000, 500, 10_000), healthy_buffer(), &spare,
            HOUR_MS, now,
        ).is_ok() {
            fired_at = Some(now);
            break;
        }
    }
    assert_eq!(
        fired_at,
        Some(policy.probe_spacing_ms),
        "a probe must be due at the spacing and not before it",
    );
}

/// **N13: the probe spacing is WALL clock, so a slow link does not wait longer for it.**
///
/// Differential by construction. The old rule counted three samples, so three segments of ANY
/// duration satisfied it; the new one counts milliseconds, so segments twice as long need half as
/// many. A test that fed both and got the same count would be grading a counter.
#[test]
fn the_probe_spacing_is_a_duration_and_not_a_number_of_segments() {
    let current = hd_catalog().candidate(Rung::P720);
    let spare = CapacityEstimate::from_prior(30_000);
    let samples_to_probe = |step_ms: u64| {
        let mut gate = recovery(28_000);
        let mut now = 0u64;
        for n in 1..40 {
            now += step_ms;
            if gate.probe_due(
                current, &idle_server(), sample(20_000, 500, 10_000), healthy_buffer(), &spare,
                HOUR_MS, now,
            ).is_ok() {
                return n;
            }
        }
        panic!("a healthy link must eventually probe");
    };
    let short = samples_to_probe(1_000);
    let long = samples_to_probe(4_000);
    assert!(
        long < short,
        "the same interval took {short} short segments and {long} long ones — equal counts would \
         mean the spacing is still a segment count",
    );
}

/// No measurable headroom, a thin reserve, or a draining one: no probe, whatever the rung. A
/// probe reads real bytes over the link the segments need, so the gates are about whether spending
/// it is safe — none of them is a rung and none of them is a count.
///
/// The clock is held far past `probe_spacing_ms` throughout, so spacing can never be what refuses:
/// each assertion is about its own gate.
#[test]
fn original_recovery_refuses_to_probe_without_room_to_do_it_safely() {
    let current = hd_catalog().candidate(Rung::P1080High);
    let spare = CapacityEstimate::from_prior(60_000);
    let no_headroom = CapacityEstimate::from_prior(20_011);
    let elapsed = AbrPolicy::measured().probe_spacing_ms * 4;
    for _ in 0..6 {
        assert!(
            recovery(28_000).probe_due(
                current,
                &idle_server(),
                sample(60_000, 500, 10_000),
                healthy_buffer(),
                &no_headroom,
                HOUR_MS,
                elapsed,
            ).is_err(),
            "segments prove a LOWER bound; at the wire rate there is no evidence of more",
        );
        assert!(
            recovery(28_000).probe_due(
                current,
                &idle_server(),
                sample(60_000, 500, 2_000),
                healthy_buffer(),
                &spare,
                HOUR_MS,
                elapsed,
            ).is_err(),
            "one segment of reserve is not room to spend on a measurement",
        );
        assert!(
            recovery(28_000).probe_due(
                current,
                &idle_server(),
                sample(60_000, 500, 10_000),
                BufferEstimate { buffered_ms: 12_000, slope_ms_per_s: -400, ..Default::default() },
                &spare,
                HOUR_MS,
                elapsed,
            ).is_err(),
            "a draining reserve is not the moment to add a second transfer",
        );
    }
}

/// **Confidence, not a count.** The requirement here is 37.8 Mbit/s (28 Mbit/s average plus VBR
/// headroom), and what has to clear it is the estimate DISCOUNTED BY ITS OWN UNCERTAINTY. So a
/// decisive probe recovers alone, a marginal one has to be confirmed, and the number of probes
/// is an output of the rule rather than part of it.
#[test]
fn original_recovery_is_decided_by_confidence_rather_than_probe_count() {
    let mut decisive = recovery(28_000);
    assert_eq!(
        decisive.observe_probe(probe(80_000, true), top_candidate(), &idle_server(), healthy_buffer(), &healthy_hls(), HOUR_MS),
        RecoveryVerdict::Recover,
        "80 Mbit/s leaves nothing for a second probe to add",
    );

    let mut marginal = recovery(28_000);
    let verdicts: Vec<RecoveryVerdict> = (0..3)
        .map(|_| marginal.observe_probe(probe(50_000, true), top_candidate(), &idle_server(), healthy_buffer(), &healthy_hls(), HOUR_MS))
        .collect();
    assert_eq!(
        verdicts,
        vec![
            RecoveryVerdict::Insufficient,
            RecoveryVerdict::Insufficient,
            RecoveryVerdict::Recover,
        ],
        "50 Mbit/s is only 1.3x the requirement, so it takes agreement to believe",
    );
}

/// A truncated probe is an ABSENT measurement, not a slow link: folding its rate in would
/// poison the next decision with a number no transfer ever sustained.
#[test]
fn a_truncated_probe_is_absence_of_evidence() {
    let mut gate = recovery(28_000);
    assert_eq!(
        gate.observe_probe(probe(2_000, false), top_candidate(), &idle_server(), healthy_buffer(), &healthy_hls(), HOUR_MS),
        RecoveryVerdict::Insufficient,
    );
    assert_eq!(
        gate.observe_probe(probe(80_000, true), top_candidate(), &idle_server(), healthy_buffer(), &healthy_hls(), HOUR_MS),
        RecoveryVerdict::Recover,
        "the aborted attempt left no trace to drag the estimate down",
    );
}

/// The benefit of Original accrues over the remaining playback; the reload is paid once, now.
#[test]
fn recovery_does_not_pay_for_a_reload_at_the_end_of_a_film() {
    let mut gate = recovery(28_000);
    assert_eq!(
        gate.observe_probe(probe(80_000, true), top_candidate(), &idle_server(), healthy_buffer(), &healthy_hls(), 8_000),
        RecoveryVerdict::NotWorthIt,
    );
    let current = hd_catalog().candidate(Rung::P720);
    let spare = CapacityEstimate::from_prior(80_000);
    for _ in 0..ORIGINAL_PROBE_SPACING * 2 {
        assert!(
            recovery(28_000).probe_due(
                current,
                &idle_server(),
                sample(20_000, 500, 10_000),
                healthy_buffer(),
                &spare,
                8_000,
                AbrPolicy::measured().probe_spacing_ms * 4,
            ).is_err(),
            "and it does not spend a probe finding that out",
        );
    }
}

/// After a forced downshift the recovery is not immediate — and **the horizon is the ACQUISITION
/// WINDOW, which is now true rather than merely claimed**.
///
/// Category 8.3 (policy choice; N8). The old form asserted `n - 2` samples of `Stay` and then
/// looked for the proposal, with a doc saying the counter had been "subsumed" by §4's rule. The
/// expectation was invalid as a statement about the controller: the window admits the top rung on
/// the sample that brings it to `n` observations, and `stable_samples` then held the proposal back
/// for two more — so `n - 2` was exactly the fudge that hid a counter the doc said was not there.
/// I6 deletes the counter, the proposal arrives on the sample the window admits it, and the old
/// margin is revealed as having been the counter all along.
///
/// **Derived rather than counted, so it cannot rot** (§8.5(c)): the assertion is not a number of
/// samples but the coincidence of two observable facts — `window_len() >= n` and a proposal. A
/// `Stay` on a sample where the window could carry the rung fails, which is precisely what
/// unmodified code does twice, so this is differential by construction.
#[test]
fn a_downshift_recovers_on_the_sample_the_window_admits_and_not_two_later() {
    let mut controller = controller_at(Rung::P1080High);
    // Establish that the encoder is no longer on its cold sample. The collapse below remains the
    // first SLOW sample, which is the decision this test grades.
    assert_eq!(controller.observe_next(sample(40_000, 400, 8_000)), Decision::Stay);
    let Decision::Prime(down) = controller.observe_next(sample(12_000, 500, 8_000)) else {
        panic!("the collapsed link must propose a downshift")
    };
    assert!(controller.commit(down, controller.clock_ms()));

    let n = AbrPolicy::measured().admission.window_len();
    let mut dwell_cleared_at = None;
    let mut recovered = None;
    for i in 0..(n * 2) {
        let decision = controller.observe_next(sample(60_000, 400, 10_000));
        let carried = controller.window_len() >= n;
        if dwell_cleared_at.is_none() && controller.telemetry().gates.dwell_ms == 0 {
            dwell_cleared_at = Some(i);
        }
        match decision {
            Decision::Prime(proposal) => {
                recovered = Some((proposal, controller.window_len(), i));
                break;
            }
            Decision::Stay => assert!(
                !carried,
                "sample {i}: the window holds {} observations and can carry the top rung, and \
                 nothing proposed it — which is a counter, not a model",
                controller.window_len(),
            ),
        }
    }
    let (proposal, window_at_proposal, at) = recovered.expect("the recovery must happen at all");
    assert_eq!(
        proposal,
        Proposal { rung: Rung::P1080High, direction: Direction::Up },
        "and it must recover all the way, not one rung at a time",
    );
    assert_eq!(
        window_at_proposal, n,
        "the proposal must arrive on the FIRST sample the window admits, not a later one",
    );
    // The OTHER guard must have got out of the way long before, or this test would be grading the
    // dwell instead of the window and would stop meaning what it says.
    let dwell_cleared_at = dwell_cleared_at.expect("the dwell must expire");
    assert!(
        dwell_cleared_at < at,
        "the dwell cleared at sample {dwell_cleared_at} and the window admitted at {at}: with the \
         two that close together this test no longer grades the window",
    );
}

/// **N10: the guard between two climbs is WALL CLOCK, and the proof is that a slower link spends
/// FEWER samples inside it.**
///
/// Category 8.3. `cooldown` counted delivered segments — `Up => 3`, `Down => 8` — and a segment
/// ARRIVES in `bytes / C` of wall time, so the guard got longer exactly as the link got worse and
/// had no bound at all. `E_tx` is the sum of the two deadlines this transaction is already held
/// to, and it is the same duration however slowly segments happen to turn up.
///
/// **Differential by construction, and the axis is the one that matters.** Both legs feed the same
/// segment MEDIA duration and differ only in how far apart in wall clock the samples arrive —
/// which is precisely what a degraded link does. A segment counter is invariant to that (three
/// segments is three segments), so unmodified code gives the same count for both; a wall-clock
/// interval admits fewer of the slow ones. The assertion is the INEQUALITY, so no number goes
/// stale.
///
/// **The axis is deliberately NOT the segment's media duration, and the first version of this test
/// got that wrong.** `E_tx` is `3/2 * d + production_max_pm * d`, i.e. proportional to `d` by
/// construction, so counting samples while varying `d` is scale-invariant — 2.6 either way — and
/// an inequality between the two legs can only come from somewhere else. It did: `prime_up` drives
/// the controller through `observe_next`, which advances the clock a segment per call, so by the
/// commit the clock was already past 38 s; the loop then restarted a SECOND origin at zero, and
/// `dwell_remaining_ms`' `saturating_sub` read no elapsed time at all until that origin caught up.
/// The test was dividing 38 000 by two segment sizes and reporting the ratio as the dwell. It is
/// why `Controller::clock_ms` exists: a test that continues the controller's own timeline cannot
/// reintroduce that.
#[test]
fn the_dwell_between_two_climbs_is_wall_clock_and_not_a_segment_count() {
    // Media duration held FIXED across both legs, so the guard's own length is identical and the
    // only thing that changes is how much wall clock passes between samples.
    const SEGMENT_MS: u32 = 2_000;
    fn samples_held(sample_gap_ms: u64) -> usize {
        let mut c = controller_at(Rung::P720);
        let up = prime_up(&mut c);
        assert!(c.commit(up, c.clock_ms()));
        assert!(c.telemetry().gates.dwell_ms > 0, "a commit must arm the dwell");
        // CONTINUE the controller's clock. Starting a second origin here is the defect the doc
        // above describes, and it is silent: the guard simply reads as never having aged.
        let mut clock = c.clock_ms();
        let mut held = 0usize;
        while c.telemetry().gates.dwell_ms > 0 {
            clock += sample_gap_ms;
            let _ = c.observe(sample_of(SEGMENT_MS, 40_000, 200, 20_000), clock);
            held += 1;
            assert!(held < 100, "the dwell must expire");
        }
        held
    }
    // Segments arriving at real time, versus the same segments arriving four times as far apart —
    // a link delivering 2 s of media every 8 s.
    let at_speed = samples_held(u64::from(SEGMENT_MS));
    let slow_link = samples_held(u64::from(SEGMENT_MS) * 4);
    assert!(
        slow_link < at_speed,
        "the same interval admitted {at_speed} samples at speed and {slow_link} on a link four \
         times slower — a guard that gives the same count for both is counting segments, not \
         measuring time",
    );
}

/// **A running dwell has a FIXED length: a longer segment afterwards must not extend it.**
///
/// `E_tx` is `3/2 * d + production_max_pm * d`, a function of the segment's media duration, and
/// `dwell_remaining_ms` recomputed it every sample from whatever `last_segment_ms` happened to
/// hold — so a guard armed against a 2 s segment silently became a guard against a 10 s one the
/// moment such a segment arrived. That is not hypothetical input: HLS segment durations come off
/// `#EXTINF`, which is a duration PMS chooses and may change, and `seconds_per_segment` is a
/// REQUEST the server is free to answer differently.
///
/// Differential by construction: under the recomputing form the second reading is larger than the
/// first despite time having passed, which is the one thing a countdown cannot do.
#[test]
fn a_longer_segment_cannot_retroactively_lengthen_a_running_dwell() {
    const ARMED_ON_MS: u32 = 2_000;
    const MUCH_LONGER_MS: u32 = 10_000;
    let mut c = controller_at(Rung::P720);
    let up = prime_up(&mut c);
    let committed_at = c.clock_ms();
    assert!(c.commit(up, committed_at));
    let armed = c.telemetry().gates.dwell_ms;
    assert!(armed > 0, "a commit must arm the dwell");

    // One much longer segment arrives while the guard is still running.
    let _ = c.observe(
        sample_of(MUCH_LONGER_MS, 40_000, 200, 20_000),
        committed_at + u64::from(ARMED_ON_MS),
    );
    let after = c.telemetry().gates.dwell_ms;
    assert!(
        after < armed,
        "the dwell owed {armed} ms when armed and {after} ms after {ARMED_ON_MS} ms had passed — a \
         countdown that grows is one whose length is being re-derived from a segment that had no \
         part in arming it",
    );
    assert_eq!(
        after,
        armed - u64::from(ARMED_ON_MS),
        "and it must have counted down by exactly the wall time that elapsed",
    );
}

/// **N11's `Circumstance` half, which nothing exercised.** `RejectCause::Circumstance` is
/// constructed only in `ff.rs`; the simulator hardcodes `Candidate`, so the entire "do not block a
/// good rung after a seek" side of the guard had no test at all.
///
/// The rule is the enum's own: a reject that says nothing about the RUNG must arm nothing. A seek
/// makes the reserve unreadable and the route moving underneath changes the origin — in both the
/// transaction that follows starts from different facts, and refusing the next climb on either
/// would be the guard doing harm in the one direction with no recovery path.
///
/// Differential, with its `Candidate` control: asserting only the `Circumstance` half would pass
/// against a `reject` that armed nothing at all.
#[test]
fn a_reject_that_says_nothing_about_the_rung_arms_nothing() {
    let mut circumstantial = bootstrap_controller();
    let up = prime_up(&mut circumstantial);
    assert!(circumstantial.reject(up, RejectCause::Circumstance, circumstantial.clock_ms()));
    assert_eq!(
        circumstantial.telemetry().gates.blocked_kbps,
        0,
        "a seek or an origin change is a statement about the SESSION; the next transaction starts \
         from different facts and must not be refused on this one's account",
    );

    let mut candidate = bootstrap_controller();
    let up = prime_up(&mut candidate);
    assert!(candidate.reject(up, RejectCause::Candidate, candidate.clock_ms()));
    assert!(
        candidate.telemetry().gates.blocked_kbps > 0,
        "a failure about the candidate must still arm the block, or the assertion above grades \
         nothing",
    );
}

/// **N10's dwell is anchored at the COMMIT, not at the proposal that opened the transaction.**
///
/// `Controller::now_ms` is written only by `observe`, so before `commit` took the caller's clock
/// the anchor was whatever `observe` last recorded — the instant the proposal was made. On device
/// a transaction runs `control.prime`, two playlist fetches, a warm-up fetch, a graded fetch and a
/// feed in between, and `E_tx` is by construction the upper BOUND on that work; anchoring there
/// set the guard expiring at about the moment the transaction was guaranteed to be over, so it
/// blocked roughly one sample instead of the interval N10 specifies.
///
/// Differential by construction: the whole assertion is that a transaction taking real time does
/// not consume the guard that is supposed to start when it ENDS. Under the old anchor the dwell
/// owed at the commit is `E_tx - elapsed`, which for a transaction of half `E_tx` is half the
/// guard — so the equality below cannot hold. No host test could have caught it before, because
/// every fixture committed from the proposing `observe` with no clock advance, which reproduces
/// exactly the anchor being corrected.
#[test]
fn a_transaction_that_takes_time_does_not_spend_the_dwell_it_arms() {
    let mut c = controller_at(Rung::P720);
    let up = prime_up(&mut c);
    let proposed_at = c.clock_ms();
    let full_dwell = crate::abr::viability::upshift_transaction_cost(
        std::time::Duration::from_millis(2_000),
        &AbrPolicy::measured(),
    )
    .as_millis() as u64;
    assert!(full_dwell > 0, "E_tx must be a real interval or this test grades nothing");

    // A transaction that really took most of its own budget: a control-plane round trip plus two
    // fetches. Anything strictly inside `E_tx` and strictly positive makes the point.
    let tx_ms = full_dwell / 2;
    let committed_at = proposed_at + tx_ms;
    assert!(c.commit(up, committed_at));

    // The next segment arrives one media duration after the transaction closed. Read the guard
    // THERE — reading it at the commit instant proves nothing, because under either anchor the
    // clock has not moved past the value being compared against.
    const SEGMENT_MS: u32 = 2_000;
    let _ = c.observe(
        sample_of(SEGMENT_MS, 40_000, 200, 20_000),
        committed_at + u64::from(SEGMENT_MS),
    );
    assert_eq!(
        c.telemetry().gates.dwell_ms,
        full_dwell - u64::from(SEGMENT_MS),
        "the dwell must have aged by the wall time since the COMMIT ({SEGMENT_MS} ms) and no more \
         — anchored at the proposal it would additionally have spent {tx_ms} ms of itself on the \
         transaction it exists to follow",
    );
}

/// **N11: a failed attempt is paid for before another is made.**
///
/// Category 8.3. `reject` used to record nothing and set `cooldown = 1`, and the decrement runs
/// BEFORE the check, so `K = 1` has never blocked a single segment — a refusal cost `E_tx` of
/// unrefilled reserve and bought another attempt on the very next sample. That is the livelock N11
/// exists to close.
///
/// **The guard is not keyed on the rejected rung, and writing this test is what showed why.** N11
/// says "refuse to re-prime that rung"; the controller does not re-propose that rung at all — the
/// budget has moved by the next sample, so it proposes a NEIGHBOURING one, which a rung-keyed
/// guard waves straight through while the reserve pays for it identically. See [`RejectBlock`].
///
/// Differential in both directions: unmodified code proposes again immediately (first leg), and a
/// guard that latched would never propose again (second leg).
#[test]
fn a_failed_prime_is_paid_for_before_another_is_attempted() {
    let mut c = controller_at(Rung::P720);
    let refused = prime_up(&mut c);
    let clock_at_reject = 1_000_000u64;
    let _ = c.observe(sample(20_000, 200, 10_000), clock_at_reject);
    assert!(c.reject(refused, RejectCause::Candidate, c.clock_ms()));
    assert_eq!(
        c.telemetry().gates.blocked_kbps,
        refused.rung.kbps(),
        "the reject must record the rung it refused, and the guard must be live",
    );

    // The same evidence at the same instant: no attempt of any kind, at any rung.
    for _ in 0..4 {
        assert_eq!(
            c.observe(sample(20_000, 200, 10_000), clock_at_reject),
            Decision::Stay,
            "another attempt costs another E_tx and nothing has repaid the last one",
        );
        assert!(c.telemetry().gates.blocked_kbps > 0, "the guard must still be holding");
    }

    // And it is not a latch. Wall clock alone releases it — this link has surplus, so the reserve
    // really does earn the attempt back, which is what `refill_time_ms` computes.
    let mut clock = clock_at_reject;
    let mut released = None;
    for _ in 0..(AbrPolicy::measured().admission.window_len() * 2) {
        clock += 2_000;
        let decision = c.observe(sample(20_000, 200, 10_000), clock);
        if c.telemetry().gates.blocked_kbps == 0 {
            released = Some(decision);
            break;
        }
    }
    assert!(released.is_some(), "a link with surplus must repay one attempt in bounded time");
}

/// **N21: the production arm is a magnitude predicate, not a persistence count.**
///
/// Category 8.3, and the derivation is the 2026-08-25 device finding at `BufferEstimate::draining`.
/// The arm required EIGHT consecutive draining segments — about sixteen seconds at the segment
/// this pipeline requests — while `starving()` beside it treats two as enough. It is now
/// `draining()`.
///
/// Stated plainly: this drops the persistence requirement entirely, an 8x increase in sensitivity
/// on an immediate-downshift arm. Differential by construction — the fixture drains for fewer than
/// eight samples, so unmodified code returns `Stay` here.
///
/// The other two downshift arms are held OFF on purpose, or this would grade one of them instead:
/// the reserve stays far above one segment and above `starving()`'s six seconds, and the link
/// delivers the rung's full rate so `starvation_horizon` is `None`.
#[test]
fn a_server_falling_behind_moves_the_rung_without_eight_samples_of_agreement() {
    let policy = AbrPolicy::measured();
    let mut c = controller_at(Rung::P1080);
    let rung_kbps = HlsActuatorCatalog::measured().candidate(Rung::P1080).expected_wire_kbps;
    // Past `production_max_pm` from the first sample, so the ONLY thing this test waits for is the
    // reserve, which is the predicate under change.
    let over = policy.production_max_pm * 2;
    let mut clock = 0u64;

    // **The reserve FILLS before it drains, and that is not decoration.** `BufferEstimate::update`
    // computes its first slope against a zero baseline, so a fixture that opens at a deep reserve
    // hands the 3:1 EWMA a fabricated positive spike of half that depth and needs about twenty
    // samples to forget it — which would make this test grade the estimator's warm-up instead of
    // the predicate. A real session starts at about one segment and climbs, so the artefact is
    // small; the fixture has to do the same or it is not modelling a playback. (Same lesson as
    // `settle_link`'s frozen reserve.)
    let mut buf = 2_000i64;
    for _ in 0..14 {
        clock += 2_000;
        buf += 2_000;
        assert_eq!(
            c.observe(sample_of(2_000, rung_kbps * 4, over, buf), clock),
            Decision::Stay,
            "a filling reserve is not a reason to move, however loaded the server is",
        );
    }
    assert!(!c.buffer().draining(), "the setup must reach a non-draining deep reserve");

    let mut fired = None;
    for i in 0..8 {
        clock += 2_000;
        buf -= 1_500;
        let decision = c.observe(sample_of(2_000, rung_kbps * 4, over, buf), clock);
        assert!(
            c.buffer().buffered_ms > 6_000,
            "sample {i}: the reserve must stay clear of `starving()` or this grades that arm",
        );
        if let Decision::Prime(p) = decision {
            fired = Some((p, c.telemetry().gates.draining));
            break;
        }
    }
    let (proposal, draining) =
        fired.expect("a server past its ceiling with a draining reserve must move the rung");
    assert_eq!(proposal.direction, Direction::Down);
    assert_eq!(
        c.telemetry().reason,
        Some(DecisionReason::Hls(HlsReason::ProductionConstraint)),
        "and it must be the PRODUCTION arm that fired, not a buffer or deadline one",
    );
    assert!(
        draining < 8,
        "it fired after {draining} draining samples — at eight or more this test grades nothing, \
         because that is exactly what the code it replaced already did",
    );
}

#[test]
fn the_auto_ladder_matches_fixed_quality_wire_values() {
    assert_eq!(Rung::P480.kbps(), 720);
    assert_eq!(Rung::P720Low.kbps(), 2_000);
    assert_eq!(Rung::P720.kbps(), 4_000);
    assert_eq!(Rung::P1080.kbps(), 8_000);
    assert_eq!(Rung::P1080High.kbps(), 20_000);
    assert_eq!(Rung::P1080High.raster(), (1920, 1080));
    // The ladder must stay sorted and unique on the request axis: `below()` walks it by index
    // and `from_ceiling` recovers a rung from a stored ceiling by exact match.
    for pair in LADDER.windows(2) {
        assert!(pair[0].kbps() < pair[1].kbps(), "{:?} then {:?}", pair[0], pair[1]);
    }
    for rung in LADDER {
        assert_eq!(Rung::from_ceiling(rung.ceiling()), Some(rung));
    }
}

/// The measured 4K operating point: the REQUEST is 22 Mbps and the OUTPUT is neither 22 Mbit/s
/// nor 1080p. Asking for 20,895 would get 1080p; asking for 22,000 gets 3840x2160 at about
/// 20,895 kbit/s. Both halves have to be stored or a budget is spent on the wrong thing.
#[test]
fn the_uhd_actuator_separates_its_request_from_its_measured_output() {
    let candidate = uhd_catalog().candidate(Rung::Uhd);
    assert_eq!(candidate.request_kbps, 22_000);
    assert_eq!(candidate.expected_wire_kbps, 20_895);
    assert_eq!(candidate.rung.raster(), (3840, 2160));
    let high = uhd_catalog().candidate(Rung::P1080High);
    assert_eq!(high.expected_wire_kbps, 20_011);
    // 4% more wire, 110% more server. The whole reason production is a separate constraint.
    assert!(candidate.expected_wire_kbps < high.expected_wire_kbps * 11 / 10);
    assert_eq!(candidate.production_load_pm, high.production_load_pm * 21 / 10);
}

/// Feasibility is a FILTER, applied before anything is scored — and it removes 4K for two
/// independent reasons, either of which is enough.
#[test]
fn an_infeasible_raster_is_removed_before_any_scoring() {
    let huge_budget = 60_000;
    assert_eq!(
        uhd_catalog().best_for_budget(huge_budget).map(|c| c.rung),
        Some(Rung::Uhd),
    );
    assert_eq!(
        hd_catalog().best_for_budget(huge_budget).map(|c| c.rung),
        Some(Rung::P1080High),
        "a 1080p source must not make PMS upscale — 2.1x the server work for no picture",
    );
    let hd_only_device = HlsActuatorCatalog::measured().limited_to((1920, 1088), (3840, 2160));
    assert_eq!(
        hd_only_device.best_for_budget(huge_budget).map(|c| c.rung),
        Some(Rung::P1080High),
        "a 1080p-limited decoder cannot be talked into 4K by a fast link",
    );
    // A zero on either axis is "nobody said", not a forbidden zero-pixel picture.
    let unmeasured = HlsActuatorCatalog::measured().limited_to((3840, 2176), (0, 0));
    assert_eq!(unmeasured.best_for_budget(huge_budget).map(|c| c.rung), Some(Rung::Uhd));
    assert!(HlsActuatorCatalog::measured()
        .limited_to((0, 0), (0, 0))
        .best_for_budget(320)
        .is_some());
}

/// **A rung's raster is a bounding box.** Measured on the television against a real library
/// item: a 1918x802 scope film had every 1080p rung ruled infeasible by a per-axis test (1080 >
/// 802), so Auto capped at 4 Mbps / 720p on a gigabit LAN — for most films, since most films
/// are wider than 16:9.
#[test]
fn a_scope_source_keeps_every_rung_that_would_not_scale_it() {
    let scope = HlsActuatorCatalog::measured().limited_to((3840, 2176), (1918, 802));
    let rungs: Vec<Rung> = scope.feasible().map(|c| c.rung).collect();
    assert!(
        rungs.contains(&Rung::P1080High) && rungs.contains(&Rung::P1080M12),
        "PMS fits 1918x802 inside a 1920x1080 box and never upscales: {rungs:?}",
    );
    assert!(
        !rungs.contains(&Rung::Uhd),
        "but a 4K box buys the same picture, priced with a 4K server load: {rungs:?}",
    );
    assert!(rungs.contains(&Rung::P720), "and the real downscale steps all survive");
    assert_eq!(
        scope.best_for_budget(60_000).map(|c| c.rung),
        Some(Rung::P1080High),
        "so a fast link spends its budget on bits rather than stopping at 720p",
    );

    // The same rule on a 4K source keeps the 4K point, because nothing smaller covers it.
    let uhd = HlsActuatorCatalog::measured().limited_to((3840, 2176), (3840, 2160));
    assert_eq!(uhd.best_for_budget(60_000).map(|c| c.rung), Some(Rung::Uhd));
    // ...and on a scope 4K master, where the box matches on width alone.
    let uhd_scope = HlsActuatorCatalog::measured().limited_to((3840, 2176), (3840, 1600));
    assert_eq!(uhd_scope.best_for_budget(60_000).map(|c| c.rung), Some(Rung::Uhd));
}

/// A 2 s segment at the 320 kbps floor is 80 KB; a LAN delivers it in under a millisecond, and
/// the honest arithmetic then reads 865 Gbit/s — which is what the television reported, and
/// what every budget downstream was computed from.
#[test]
fn a_transfer_too_short_to_time_cannot_claim_a_gigabit_link() {
    let floor = hd_catalog().candidate(Rung::P240);
    let tiny = CapacityObservation {
        kbps: 865_219,
        bytes: 80_000,
        active_us: 700,
        completed: true,
    };
    assert_eq!(tiny.quality(), ObservationQuality::Weak);
    let clamped = tiny.clamped_to_evidence(floor.expected_wire_kbps);
    assert_eq!(clamped.kbps, floor.expected_wire_kbps * 8, "a bounded claim, not a fantasy");
    assert!(
        clamped.kbps > hd_catalog().candidate(Rung::P720Low).expected_wire_kbps,
        "and still enough to climb out of the floor promptly",
    );
    // A transfer big enough to time is left alone, whatever it says.
    let real = CapacityObservation {
        kbps: 90_000,
        bytes: 4_000_000,
        active_us: 355_000,
        completed: true,
    };
    assert_eq!(real.clamped_to_evidence(floor.expected_wire_kbps), real);

    // End to end: the floor rung on a fast LAN climbs instead of parking. `250pm` is half a
    // second of PMS production for a two-second segment — see the companion test below for why
    // the number is load-bearing here and was not before §4's rule decided.
    let mut controller = controller_at(Rung::P240);
    let mut reached = Rung::P240;
    for _ in 0..40 {
        let segment = sample_bytes(80_000, 700, 250, 12_000);
        if let Decision::Prime(proposal) = controller.observe_next(segment) {
            if controller.candidate_ready(proposal, sample(20_000, 400, 12_000), declared_bps(proposal.rung)) {
                controller.commit(proposal, controller.clock_ms());
                reached = controller.current();
            } else {
                controller.reject(proposal, RejectCause::Candidate, controller.clock_ms());
            }
        }
    }
    assert!(reached > Rung::P240, "a LAN must not leave Auto on the emergency floor");
}

/// **[LIMITATION, pinned deliberately] At the emergency floor the transfer bound licenses a climb
/// only while production is fast, and this test is where that boundary is written down.**
///
/// A 320 kbps segment is ~80 kB. §2a's bound is `A_i·max(1, q/b_i)`, so the largest byte count it
/// will admit is `b_i · D/A` — and the candidate is charged its rung's WORST case, `σ·W_j·D/8000`,
/// while the observation is whatever this content happened to weigh. At the floor those two
/// asymmetries compound: `σ` is **1.418 at rung 720** (the quality-floor regime, where the encoder
/// overshoots its target) against 0.893 above 4000, and 80 kB is only 0.63 of rung 320's own worst
/// case on easy content. The climb therefore needs a **3.19×** byte ratio while `D/A` supplies
/// `1000/ratio_pm × 2`.
///
/// The crossover is `ratio_pm ≈ 313`, and it is arithmetic rather than a tuning knob: above it the
/// controller holds the floor.
///
/// **This is the same shape as the collapse finding** (`docs/measurements/j3a-window-shadow.md` §5)
/// — the rule declining to speak where its evidence cannot carry a conclusion — and it has the same
/// remedy: not a weaker bound, but better evidence. A probe that spends a bounded, affordable
/// transaction to obtain one larger observation is the plan's J4, and until it exists this boundary
/// is real. It is pinned rather than papered over so that a change moving it fails loudly.
#[test]
fn at_the_emergency_floor_a_slow_producing_server_holds_the_rule_below_a_climb() {
    fn reached_from_floor(ratio_pm: u32) -> Rung {
        let mut controller = controller_at(Rung::P240);
        for _ in 0..40 {
            if let Decision::Prime(proposal) =
                controller.observe_next(sample_bytes(80_000, 700, ratio_pm, 12_000))
            {
                if controller.candidate_ready(
                    proposal,
                    sample(20_000, 400, 12_000),
                    declared_bps(proposal.rung),
                ) {
                    controller.commit(proposal, controller.clock_ms());
                } else {
                    controller.reject(proposal, RejectCause::Candidate, controller.clock_ms());
                }
            }
        }
        controller.current()
    }
    assert!(reached_from_floor(300) > Rung::P240, "below the crossover the floor is escapable");
    assert_eq!(
        reached_from_floor(400),
        Rung::P240,
        "above it the bound cannot license a climb, and pretending otherwise is the whole thing \
         this specification exists to stop",
    );
}

/// **Network yes, server no.** The two constraints are evaluated independently, so a fast link
/// in front of a PMS already near real time cannot commit the 4K point.
#[test]
fn a_fast_link_in_front_of_a_loaded_server_does_not_choose_4k() {
    let catalog = uhd_catalog();
    let current = catalog.candidate(Rung::P1080High);
    let fast = CapacityEstimate::from_prior(80_000);
    let policy = AbrPolicy::measured();
    let buffer = BufferEstimate { buffered_ms: 20_000, ..Default::default() };

    let mut quick_server = ProductionEstimate::default();
    for _ in 0..4 {
        quick_server.observe(200, current.production_load_pm, false);
    }
    assert_eq!(
        catalog
            .best_sustainable(60_000, &quick_server, current, &policy, 30_000)
            .map(|c| c.rung),
        Some(Rung::Uhd),
        "a server at a fifth of real time has room for the measured 2.1x",
    );

    let mut loaded_server = ProductionEstimate::default();
    for _ in 0..4 {
        loaded_server.observe(700, current.production_load_pm, false);
    }
    assert_eq!(
        catalog
            .best_sustainable(60_000, &loaded_server, current, &policy, 30_000)
            .map(|c| c.rung),
        Some(Rung::P1080High),
        "0.7 of real time on 1080p predicts 1.47 on 4K — behind, whatever the link does",
    );
    assert!(
        candidate_risk(
            catalog.candidate(Rung::Uhd),
            current,
            &fast,
            &loaded_server,
            &buffer,
            &policy,
        )
        .production_risk,
        "and the risk model says so too, so the two agree rather than one overriding the other",
    );
}

/// A budget jump primes the far candidate ONCE, instead of paying for three encoder creations
/// to walk 10, 12, 14.
///
/// **The loop reaches `2n` because §4's rule gates BOTH sides of the transaction now**, and this
/// test is the clearest place to say that the jump itself is untouched by it: `largest_admissible`
/// walks DOWN from the budget's choice and returns the highest rung the window supports, which is
/// still one proposal and still skips 10 and 12. What the window changed is WHEN the controller may
/// propose, not how far it may reach.
#[test]
fn a_budget_jump_skips_the_intermediate_encoders() {
    let mut controller = controller_at(Rung::P1080);
    let n = AbrPolicy::measured().admission.window_len();
    let mut proposal = None;
    for _ in 0..(n * 2) {
        if let Decision::Prime(p) = controller.observe_next(sample(22_000, 200, 12_000)) {
            proposal = Some(p);
            break;
        }
    }
    assert_eq!(
        proposal,
        Some(Proposal { rung: Rung::P1080M14, direction: Direction::Up }),
        "8 Mbps now, a ~14 Mbit/s safe budget: one prime, not three",
    );
}

#[test]
fn only_upshift_primes_receive_the_exact_acceptance_budget() {
    let media = std::time::Duration::from_millis(2_002);
    let policy = AbrPolicy::measured();
    let up = Proposal { rung: Rung::P720Low, direction: Direction::Up };
    let down = Proposal { rung: Rung::P240, direction: Direction::Down };
    // A reserve far above either acceptance budget, so this test grades the ACCEPTANCE half alone.
    let ample = reserve_as_budget(60_000);
    assert_eq!(
        candidate_prime_budget(media, &policy, ample),
        std::time::Duration::from_micros(2_202_200)
    );
    assert_eq!(
        candidate_warmup_budget(up, media, ample, NO_FLOOR),
        std::time::Duration::from_micros(3_003_000)
    );
    // A downshift has no acceptance test — it is the recovery path — so the reserve is its ONLY
    // bound, and it is now a bound rather than nothing.
    assert_eq!(candidate_warmup_budget(down, media, ample, NO_FLOOR), ample);
}

/// **A downshift transfer may not outlive the reserve it is paid out of.** J3b, and the
/// 36-second transaction it removes.
///
/// Both budget functions opened with `if direction == Down { return None }`, so a downshift
/// warm-up ran until the transport gave up. Over the committed corpus the `Down`/commit decision
/// cost is p95 2 198 ms and **max 36 164** — a 16x jump with nothing in between, on a link that
/// had collapsed to 9 593 kbps. That is not a slow transaction; it is an unbounded one.
///
/// Differential: against the old code every assertion below reads `None`.
#[test]
fn a_downshift_transfer_is_bounded_by_the_reserve_it_spends() {
    let media = std::time::Duration::from_millis(2_000);
    let down = Proposal { rung: Rung::P240, direction: Direction::Down };
    for reserve_ms in [0i64, 250, 5_000, 36_000] {
        let reserve = reserve_as_budget(reserve_ms);
        assert_eq!(
            candidate_warmup_budget(down, media, reserve, NO_FLOOR),
            reserve,
            "a {reserve_ms}ms reserve buys exactly {reserve_ms}ms of transfer",
        );
    }
}

/// The bound is the CONSERVATION identity, so it applies in both directions — a transaction that
/// outlives the reserve stalls whichever way it was going. On a healthy upshift it does not bind
/// (the proposal gate requires three segments and the two budgets sum to ~2.6), which is the
/// signature of a bound that is not doing hidden work; it binds when the reserve fell between the
/// proposal and the fetch.
#[test]
fn a_thin_reserve_bounds_an_upshift_too_and_an_ample_one_does_not() {
    let media = std::time::Duration::from_millis(2_000);
    let policy = AbrPolicy::measured();
    let up = Proposal { rung: Rung::P1080, direction: Direction::Up };

    let healthy = reserve_as_budget(3 * 2_000);
    assert_eq!(
        candidate_warmup_budget(up, media, healthy, NO_FLOOR),
        std::time::Duration::from_millis(3_000),
        "at the proposal gate's own reserve the acceptance budget still decides",
    );
    assert_eq!(
        candidate_prime_budget(media, &policy, healthy),
        std::time::Duration::from_millis(2_200),
    );

    let thin = reserve_as_budget(400);
    assert_eq!(candidate_warmup_budget(up, media, thin, NO_FLOOR), thin);
    assert_eq!(candidate_prime_budget(media, &policy, thin), thin);
}

/// A reserve at or below zero buys no time at all. The deadline is then "now", which aborts the
/// fetch on its first check — correct, because a transaction starting with no reserve has already
/// stalled and every millisecond after that is a millisecond of stall.
#[test]
fn a_reserve_at_or_below_zero_is_a_zero_budget_and_never_a_wrapped_one() {
    for ms in [0i64, -1, -20_000, i64::MIN] {
        assert_eq!(reserve_as_budget(ms), std::time::Duration::ZERO, "at {ms}ms");
    }
    assert_eq!(reserve_as_budget(1), std::time::Duration::from_millis(1));
}

/// **DIMENSIONAL INVARIANT: the network budget has exactly one input, a network estimate.**
///
/// `hls_safe_budget` used to do `budget -= (minimum_buffer_ms - buffered_ms)` — subtracting
/// milliseconds from kilobits per second, removing up to 2 500 kbps of link because the reserve was
/// short, by an amount that had nothing to do with the link. This is differential: it cannot pass
/// against that code, because moving ONLY the reserve moved the answer.
///
/// The intent behind that branch is not lost, it is relocated: "the reserve must cover what the
/// rung will overrun by" is §4's condition (2), evaluated in the units of the reserve against
/// measured acquisitions. Production is likewise a separate candidate feasibility constraint;
/// neither quantity can enter this function by construction.
#[test]
fn the_safe_budget_is_the_final_conservative_network_estimate() {
    let capacity = CapacityEstimate::from_prior(20_000);
    assert_eq!(hls_safe_budget(&capacity), capacity.conservative_kbps());
}

/// A pause breaks uninterrupted rung residency, and **which guards it clears is now an argument
/// rather than a sweep** (N12, re-expressed by I6).
///
/// Category 8.3 (policy choice). The old assertion — "`on_resume` clears `stable`, `cooldown` and
/// `on_rung`" — is invalid because two of those three no longer exist: I6 replaced the sample
/// counters with a wall-clock dwell and a recorded reject block (N10, N11). Re-expressed rather
/// than deleted, because the underlying question survives verbatim: after a pause, which state
/// still describes the world?
///
/// The three answers are now different from each other, and that is the finding:
///
/// * `on_rung` is CLEARED. It counts uninterrupted samples on this rung and a pause ends that.
/// * The reject block is CLEARED. Its evidence release compares against a rate `age_ms` has just
///   widened the uncertainty on, so the recorded number no longer describes anything.
/// * The dwell is NOT cleared, and needs no clearing — thirty seconds of pause really are thirty
///   seconds in which no encoder session was started, so `E_tx` has genuinely elapsed. **This is
///   the differential half**: a segment counter could not represent that at all, because no
///   segments arrived, so under unmodified code the pause released exactly nothing.
#[test]
fn resume_clears_the_state_a_pause_invalidates_and_leaves_the_clock_alone() {
    let mut c = bootstrap_controller();
    let mut now = 0u64;
    for _ in 0..3 {
        now += 2_000;
        let _ = c.observe(sample(40_000, 200, 20_000), now);
    }
    assert!(c.telemetry().gates.on_rung > 0, "the setup must establish rung residency");
    c.on_resume(30_000);
    assert_eq!(c.telemetry().gates.on_rung, 0);

    // A reject records the rung it refused; a resume retires that record.
    let mut blocked = controller_at(Rung::P720);
    let mut now = 0u64;
    let proposal = loop {
        now += 2_000;
        if let Decision::Prime(p) = blocked.observe(sample(40_000, 200, 20_000), now) {
            break p;
        }
        assert!(now < 60_000, "the setup must reach an upshift proposal");
    };
    assert!(blocked.reject(proposal, RejectCause::Candidate, blocked.clock_ms()));
    assert_eq!(
        blocked.telemetry().gates.blocked_kbps,
        proposal.rung.kbps(),
        "the setup must establish a live reject block",
    );
    blocked.on_resume(30_000);
    assert_eq!(blocked.telemetry().gates.blocked_kbps, 0);

    // And the dwell is released by the clock, whether or not anything was resumed.
    let mut dwelling = controller_at(Rung::P720);
    let mut now = 0u64;
    let up = loop {
        now += 2_000;
        if let Decision::Prime(p) = dwelling.observe(sample(40_000, 200, 20_000), now) {
            break p;
        }
        assert!(now < 60_000, "the setup must reach an upshift proposal");
    };
    assert!(dwelling.commit(up, dwelling.clock_ms()));
    assert!(dwelling.telemetry().gates.dwell_ms > 0, "a commit arms the dwell");
    now += 30_000;
    let _ = dwelling.observe(sample(40_000, 200, 20_000), now);
    assert_eq!(
        dwelling.telemetry().gates.dwell_ms, 0,
        "thirty seconds of wall clock is thirty seconds, whether they were paused or played",
    );
}

/// **A link too fast to be believed must not read as a COLLAPSE.**
///
/// `collapse` compared `measured_kbps * 4` with a bare multiply while its own sibling
/// `CapacityObservation::is_collapse` saturated. Above ~1.07 Gbit/s the product wraps — and an
/// 865 Gbit/s reading is on record from this television, which is why `clamped_to_evidence`
/// exists at all. The two configurations disagree about what wrapping DOES: `overflow-checks` is
/// on under `cargo test` so the host panics, and off in release so the set silently declares a
/// collapse on the fastest link it has ever seen.
///
/// So this test does double duty — on the host it proves the panic is gone, and the assertion
/// proves the release wrap is gone with it.
#[test]
fn an_absurdly_fast_reading_neither_panics_nor_reads_as_a_collapse() {
    for measured in [u32::MAX, u32::MAX / 2, 865_000_000, 1_073_741_824] {
        let mut estimate = CapacityEstimate::from_prior(40_000);
        let slow_before = estimate.slow_kbps;
        estimate.collapse(measured);
        assert_eq!(
            estimate.slow_kbps, slow_before,
            "{measured}kbps is faster than the prior, so nothing collapsed",
        );
        assert_eq!(estimate.fast_kbps, measured, "the fast estimate still takes the reading");
    }
}

/// The other side of the same guard: a genuine collapse must still be detected.
#[test]
fn a_reading_a_quarter_of_the_prior_still_collapses() {
    let mut estimate = CapacityEstimate::from_prior(40_000);
    estimate.collapse(1_000);
    assert!(estimate.slow_kbps < 40_000, "a 40x drop is a collapse and must lower the slow prior");
    assert_eq!(estimate.uncertainty_pm, 400);
}

/// **The graded-segment deadline and the acceptance test are the SAME threshold.**
///
/// They were, when both were 0.8·D. The §4 admission work replaced `candidate_ready`'s bare 800
/// with `production_max_pm`, and the transport's literal `4/5` was left behind — so for a while a
/// candidate whose graded segment took between 0.8·D and 1.1·D was aborted by the deadline and
/// never reached the rule that would have admitted it. One threshold, enforced twice at two
/// values, the stricter one invisible because it fired in `ff.rs`.
///
/// This is the test that would have caught it, and it is differential: it fails against any
/// literal that is not the policy's own number.
#[test]
fn the_prime_deadline_is_exactly_the_acceptance_threshold() {
    let policy = AbrPolicy::measured();
    for media_ms in [1_000u64, 2_000, 4_000, 6_006] {
        let media = std::time::Duration::from_millis(media_ms);
        // An ample reserve, so the acceptance threshold is what this grades.
        let budget = candidate_prime_budget(media, &policy, reserve_as_budget(600_000));
        // What `candidate_ready` admits: `production_ratio_pm < production_max_pm`, i.e. an
        // acquisition strictly under `production_max_pm/1000` of the content duration.
        let admits_up_to_us =
            u128::from(media_ms) * 1_000 * u128::from(policy.production_max_pm) / 1_000;
        assert_eq!(
            u128::from(budget.as_micros() as u64),
            admits_up_to_us,
            "at {media_ms}ms the transport aborts at {}us but the rule admits to {admits_up_to_us}us",
            budget.as_micros(),
        );
    }
}

/// The four shaped links from the device session, re-graded on today's ladder. The 17.5 Mbit/s
/// leg lands a rung higher than the 8 Mbps it committed on the six-rung ladder — that leg had
/// to choose between 8 and 20 Mbps and spent 12 Mbit/s of a measured link on nothing.
///
/// It also reaches it in TWO moves rather than one, and that is the production model being
/// honest rather than a flaw: from 480p, extrapolating the server's cost five raster-steps
/// ahead is a guess, so the controller takes the step its evidence supports and re-measures.
/// Skipping intermediate encoders is bounded by what has actually been observed.
#[test]
fn lg_network_legs_settle_on_sustainable_rungs() {
    assert_eq!(settle_link(512), Rung::P240);
    assert_eq!(settle_link(1_200), Rung::P480);
    assert_eq!(settle_link(7_000), Rung::P720);
    assert_eq!(settle_link(17_500), Rung::P1080M10);
}

#[test]
fn capacity_estimation_separates_stable_and_volatile_history() {
    let mut stable = CapacityEstimate::default();
    for kbps in [59_000, 60_000, 61_000, 60_000, 60_000] {
        stable.update(CapacityObservation {
            kbps,
            bytes: 2_000_000,
            active_us: 300_000,
            completed: true,
        });
    }
    let mut volatile = CapacityEstimate::default();
    for kbps in [60_000, 10_000, 60_000, 12_000, 60_000] {
        volatile.update(CapacityObservation {
            kbps,
            bytes: 2_000_000,
            active_us: 300_000,
            completed: true,
        });
    }
    assert!(stable.conservative_kbps() > volatile.conservative_kbps());
    assert!(stable.uncertainty_pm < volatile.uncertainty_pm);
}

#[test]
fn a_sudden_collapse_reduces_confidence_in_the_old_slow_estimate() {
    let mut capacity = CapacityEstimate::default();
    for kbps in [80_000, 80_000, 80_000] {
        capacity.update(CapacityObservation {
            kbps,
            bytes: 2_000_000,
            active_us: 300_000,
            completed: true,
        });
    }
    let observation = CapacityObservation {
        kbps: 5_000,
        bytes: 2_000_000,
        active_us: 300_000,
        completed: true,
    };
    assert!(observation.is_collapse(&capacity));
    capacity.collapse(5_000);
    assert!(capacity.fast_kbps <= 5_000);
    assert!(capacity.uncertainty_pm > 250);
}

#[test]
fn starvation_horizon_distinguishes_harmless_deficit_from_emergency() {
    // Capacity at or above the requirement: no network-driven depletion at all.
    assert_eq!(starvation_horizon(20_000, 40_000, 60_000).seconds, None);
    // A 1.7% shortfall against a minute of reserve is an hour away.
    let healthy = starvation_horizon(60_000, 60_000, 59_000);
    assert_eq!(healthy.seconds, Some(3_600));
    // A 12x shortfall against ten seconds is ten seconds away.
    let severe = starvation_horizon(10_000, 60_000, 5_000);
    assert_eq!(severe.seconds, Some(10));
    let current = HlsActuatorCatalog::measured().candidate(Rung::P1080High);
    assert!(candidate_risk(
        current,
        current,
        &CapacityEstimate { slow_kbps: 59_000, fast_kbps: 59_000, uncertainty_pm: 0, samples: 8 },
        &ProductionEstimate::default(),
        &BufferEstimate { buffered_ms: 60_000, slope_ms_per_s: 0, samples: 8, ..Default::default() },
        &AbrPolicy::measured(),
    )
    .score < 5);
}

#[test]
fn a_safe_budget_selects_the_best_actuator_directly() {
    let catalog = hd_catalog();
    assert_eq!(catalog.best_for_budget(15_000).map(|c| c.rung), Some(Rung::P1080M14));
    assert_eq!(catalog.best_for_budget(3_000).map(|c| c.rung), Some(Rung::P720Low));
    assert_eq!(catalog.candidate(Rung::P1080High).expected_wire_kbps, 20_011);
    assert_eq!(catalog.best_for_budget(100).map(|c| c.rung), None, "nothing fits, and it says so");
}

/// Throughput is a RATE, so the size and duration of the transfer decide how much it proves —
/// and the DURATION decides it first, for every tier.
///
/// **This test's own `normal` fixture was the defect.** It carried `active_us: 160_000` against a
/// 250 ms floor and asserted `Normal`, because `quality()` asked the interval only on the `Strong`
/// arm. That is the case `clamped_to_evidence` exists for — large enough to pass the size test,
/// far too brief to measure — and it skipped the clamp, so a ~500 KB segment arriving in ~200 us
/// reported tens of millions of kbps. Found on the host simulator over a shaped 20 Mbit/s link,
/// where it wiped the acquisition window six times running and left the controller unable to
/// propose any upshift at all (`CapacityObservation::quality`'s doc has the reading).
#[test]
fn observation_quality_weights_a_tiny_read_below_a_sustained_one() {
    let tiny = CapacityObservation { kbps: 100_000, bytes: 40_000, active_us: 3_000, completed: true };
    let normal = CapacityObservation { kbps: 20_000, bytes: 400_000, active_us: 300_000, completed: true };
    let sustained = CapacityObservation { kbps: 20_000, bytes: 4_000_000, active_us: 1_600_000, completed: true };
    let truncated = CapacityObservation { completed: false, ..sustained };
    // Large enough to pass every SIZE test and far too brief to be a rate at all: the shape that
    // escaped both the interval test and the clamp.
    let big_and_brief = CapacityObservation {
        kbps: 24_000_000, bytes: 600_000, active_us: 200, completed: true,
    };
    assert_eq!(tiny.quality(), ObservationQuality::Weak);
    assert_eq!(normal.quality(), ObservationQuality::Normal);
    assert_eq!(sustained.quality(), ObservationQuality::Strong);
    assert_eq!(truncated.quality(), ObservationQuality::Weak, "a truncated read proves a floor");
    assert_eq!(
        big_and_brief.quality(),
        ObservationQuality::Weak,
        "a transfer that never stayed open long enough to measure reports LATENCY, whatever its \
         size — and only a Weak sample reaches `clamped_to_evidence`",
    );
    assert_eq!(
        big_and_brief.clamped_to_evidence(2_000).kbps,
        2_000 * crate::abr::estimate::WEAK_SAMPLE_HEADROOM,
        "so it may claim a multiple of the rung it was measured on, not 24 Gbit/s",
    );
    assert!(tiny.weight() < normal.weight() && normal.weight() < sustained.weight());

    // ...and the weighting is real: a Strong observation pulls the estimate harder than a
    // Normal one. The outlier stays INSIDE the regime factor on purpose — a bigger one would
    // restart the estimate instead of blending, which is a different rule (below).
    let settle = |first: CapacityObservation| {
        let mut estimate = CapacityEstimate::default();
        for _ in 0..6 {
            estimate.update(first);
        }
        estimate.update(CapacityObservation { kbps: 8_000, ..first });
        estimate.slow_kbps
    };
    assert!(settle(sustained) < settle(normal), "a strong sample pulls harder");
}

/// **The 2026-08-25 device finding.** A shaped leg ending is not an outlier to average in: the
/// television's Original recovery probe measured 3,952 kbps during the slow leg and 28,116 kbps
/// after it, blended to 9,993 against a 10,800 requirement, and Auto never returned to
/// Original. Seven times apart is a different link.
#[test]
fn a_measurement_a_factor_of_four_away_restarts_the_estimate() {
    let obs = |kbps| CapacityObservation {
        kbps,
        bytes: 999_424,
        active_us: 300_000,
        completed: true,
    };
    let mut estimate = CapacityEstimate::default();
    estimate.update(obs(3_952));
    assert!(obs(28_116).is_regime_change(&estimate), "seven times is not jitter");
    estimate.update(obs(28_116));
    assert_eq!(estimate.slow_kbps, 28_116, "the new regime is the estimate, not a blend of two");
    assert_eq!(estimate.samples, 1, "with one sample's worth of confidence, no more");
    assert!(
        estimate.conservative_kbps() >= source_requirement_kbps(8_000, &AbrPolicy::measured()),
        "which is what makes the second probe decisive, as the device run needed",
    );

    // Symmetric, and ordinary variance is nowhere near it.
    let mut falling = CapacityEstimate::default();
    for _ in 0..4 {
        falling.update(obs(40_000));
    }
    assert!(!obs(30_000).is_regime_change(&falling), "a 25% dip is the link breathing");
    assert!(obs(2_000).is_regime_change(&falling));
    falling.update(obs(2_000));
    assert_eq!(falling.slow_kbps, 2_000);
    // A first sample has no history to be a change FROM.
    assert!(!obs(80_000).is_regime_change(&CapacityEstimate::default()));
}

/// The other half of the same device run: an EWMA slope decays toward zero and never arrives,
/// so a sign test calls a flat reserve "draining" forever — and the upshift gate requires
/// `!draining`, which is how Auto sat on 10 Mbps with a 25 Mbit/s budget and a full buffer.
#[test]
fn a_flat_reserve_is_not_draining() {
    let flat = |slope| BufferEstimate { buffered_ms: 12_000, slope_ms_per_s: slope, ..Default::default() };
    for slope in [0, -4, -16, -49, 100] {
        assert!(!flat(slope).draining(), "slope {slope} is noise around flat");
    }
    for slope in [-51, -400, -2_000] {
        assert!(flat(slope).draining(), "slope {slope} is a reserve actually going away");
    }
    // And the counter behind `starving` follows the same test, so the two cannot disagree.
    let mut buffer = BufferEstimate::default();
    buffer.update(Some(12_000), 2_000);
    for step in 1..=3 {
        buffer.update(Some(12_000 - step * 20), 2_000);
    }
    assert_eq!(buffer.draining_samples, 0, "20ms a segment is not a drain");
}

/// A demoted prior keeps its number and loses its confidence — the one operation behind
/// bootstrap seeding, a path change, and a long pause.
#[test]
fn a_demoted_prior_keeps_its_value_and_gives_up_its_confidence() {
    let mut estimate = CapacityEstimate::default();
    for _ in 0..5 {
        estimate.update(CapacityObservation {
            kbps: 40_000,
            bytes: 4_000_000,
            active_us: 800_000,
            completed: true,
        });
    }
    let confident = estimate.conservative_kbps();
    estimate.demote_to_prior();
    assert_eq!(estimate.slow_kbps, 40_000);
    assert_eq!(estimate.samples, 1);
    assert_eq!(estimate.uncertainty_pm, MAX_UNCERTAINTY_PM);
    assert!(estimate.conservative_kbps() < confident);
    assert_eq!(estimate.conservative_kbps(), 20_000, "at most half of an unconfirmed number");
    // An empty estimate has nothing to demote and must not invent a prior.
    let mut empty = CapacityEstimate::default();
    empty.demote_to_prior();
    assert_eq!(empty, CapacityEstimate::default());
}

#[test]
fn staleness_widens_uncertainty_before_it_gives_up_entirely() {
    let policy = AbrPolicy::measured();
    let fresh = || {
        let mut e = CapacityEstimate::default();
        e.update(CapacityObservation {
            kbps: 40_000,
            bytes: 4_000_000,
            active_us: 800_000,
            completed: true,
        });
        e.update(CapacityObservation {
            kbps: 40_000,
            bytes: 4_000_000,
            active_us: 800_000,
            completed: true,
        });
        e
    };
    let mut brief = fresh();
    brief.age_ms(u64::from(policy.stale_half_life_ms) / 2, &policy);
    assert_eq!(brief, fresh(), "a gap shorter than a half-life is not staleness");

    let mut aged = fresh();
    aged.age_ms(u64::from(policy.stale_half_life_ms) * 2, &policy);
    assert!(aged.uncertainty_pm > fresh().uncertainty_pm);
    assert!(aged.samples > 1, "still a history, just a less certain one");

    let mut ancient = fresh();
    ancient.age_ms(u64::from(policy.stale_half_life_ms) * 10, &policy);
    assert_eq!(ancient.samples, 1, "past four half-lives it is a memory, not a measurement");
}

/// **The bootstrap table.** One row per link class, plus the three ways a Remote probe can end.
#[test]
fn bootstrap_decides_from_the_link_class_and_one_bounded_probe() {
    let policy = AbrPolicy::measured();
    let catalog = hd_catalog();
    let go = |link, feasible, source, probe| {
        bootstrap(link, feasible, source, probe, &catalog, &policy)
    };
    let complete = |kbps| {
        Some(CapacityObservation { kbps, bytes: 2_000_000, active_us: 400_000, completed: true })
    };

    // A verified LAN carrying a playable file needs no measurement to prove it.
    let local = go(LinkKind::Local, true, 28_000, None);
    assert!(local.original && local.reason == BootstrapReason::LocalDirect);
    assert!(local.prior.is_none(), "nothing was measured, so nothing is claimed");

    // Relay is bandwidth-limited by design; measuring it would be theatre.
    assert_eq!(go(LinkKind::Relay, true, 28_000, None).reason, BootstrapReason::RelayLimited);
    assert!(!go(LinkKind::Relay, true, 28_000, None).original);

    // Original impossible for this item (codec, burn-in, no source URL) — link is irrelevant.
    assert_eq!(
        go(LinkKind::Local, false, 28_000, None).reason,
        BootstrapReason::OriginalInfeasible,
    );

    // A fast Remote probe: Original, and the probe survives as an explicitly weak prior.
    let fast = go(LinkKind::Remote, true, 28_000, complete(80_000));
    assert!(fast.original && fast.reason == BootstrapReason::ProbeSustainable);
    let prior = fast.prior.expect("a completed probe is evidence");
    assert_eq!(prior.samples, 1, "weak on purpose: a different request to a different server");
    assert_eq!(prior.uncertainty_pm, MAX_UNCERTAINTY_PM);

    // Borderline: above the average but inside the cold-start margin. HLS, and the measurement
    // still picks the opening rung.
    let borderline = go(LinkKind::Remote, true, 28_000, complete(30_000));
    assert!(!borderline.original);
    assert_eq!(borderline.reason, BootstrapReason::ProbeBelowRequirement);
    assert_eq!(borderline.rung, Rung::P1080High, "24 Mbit/s of budget, spent");

    // A slow Remote opens where the measurement says, NOT at an emergency floor it would then
    // spend a minute climbing out of.
    let slow = go(LinkKind::Remote, true, 60_000, complete(17_000));
    assert!(!slow.original);
    assert_eq!(slow.rung, Rung::P1080M12, "13.6 Mbit/s of budget → the 12 Mbps point");

    // Nothing to reason from: playback still starts, conservatively.
    for inconclusive in [None, Some(CapacityObservation { kbps: 9_000, bytes: 100_000, active_us: 90_000, completed: false })] {
        let decision = go(LinkKind::Remote, true, 60_000, inconclusive);
        assert!(!decision.original);
        assert_eq!(decision.reason, BootstrapReason::ProbeInconclusive);
        assert!(decision.rung.kbps() <= Rung::P1080.kbps(), "conservative, not paralysed");
    }
    // An unknown source bitrate cannot be reasoned about either, and says so.
    assert_eq!(
        go(LinkKind::Remote, true, 0, complete(80_000)).reason,
        BootstrapReason::ProbeInconclusive,
    );
}

/// The bootstrap measurement is not thrown away: the live estimator starts from it, so the
/// first segment refines a number instead of being the only one.
#[test]
fn the_bootstrap_prior_seeds_the_steady_state_estimator() {
    let seeded = Controller::starting_at(
        Rung::P480,
        Some(CapacityEstimate::from_prior(40_000)),
        hd_catalog(),
    );
    assert_eq!(seeded.delivery().slow_kbps, 40_000);
    assert_eq!(seeded.delivery().samples, 1);
    let cold = controller_at(Rung::P480);
    assert_eq!(cold.delivery().samples, 0);
    // The seed is weak enough that ONE real segment dominates it rather than being averaged
    // into irrelevance.
    let mut seeded = seeded;
    seeded.observe_next(sample(4_000, 400, 10_000));
    assert!(seeded.delivery().slow_kbps < 25_000, "{}", seeded.delivery().slow_kbps);
}

/// **Transitions are asymmetric and their cost decays.** An HLS rung change is free here (the
/// viewer never sees it); a mode change is not; and a mode change right after another one is
/// worse than the first.
#[test]
fn transition_cost_is_asymmetric_and_decays_with_time() {
    let policy = AbrPolicy::measured();
    let none = TransitionHistory::default();
    assert_eq!(transition_cost(ModeKind::Hls, ModeKind::Hls, none, &policy), 0);
    let first = transition_cost(ModeKind::Original, ModeKind::Hls, none, &policy);
    assert_eq!(first, policy.visible_switch_cost);

    let just_switched = TransitionHistory { visible_switches: 2, since_last_ms: Some(1_000) };
    let long_ago = TransitionHistory {
        visible_switches: 2,
        since_last_ms: Some(policy.visible_switch_decay_ms * 4),
    };
    let recent = transition_cost(ModeKind::Hls, ModeKind::Original, just_switched, &policy);
    let old = transition_cost(ModeKind::Hls, ModeKind::Original, long_ago, &policy);
    assert!(recent > old && old >= first, "recent={recent} old={old} first={first}");
    assert!(
        transition_cost(
            ModeKind::Hls,
            ModeKind::Original,
            TransitionHistory { visible_switches: 6, since_last_ms: Some(1_000) },
            &policy,
        ) > recent,
        "the sixth switch in two minutes has to buy a lot",
    );
}

/// The same evidence that recovers on a first switch stops recovering once this playback has
/// been flapping — no cooldown counter, just history the utility model can see.
#[test]
fn repeated_visible_switches_stop_paying_for_themselves() {
    let calm = OriginalRecovery::new(
        28_000,
        AbrPolicy::measured(),
        SourceFeatures::default(),
        TransitionHistory::default(),
        hd_catalog(),
    )
    .unwrap();
    let flapping = OriginalRecovery::new(
        28_000,
        AbrPolicy::measured(),
        SourceFeatures::default(),
        TransitionHistory { visible_switches: 5, since_last_ms: Some(2_000) },
        hd_catalog(),
    )
    .unwrap();
    let good = probe(90_000, true);
    let mut calm = calm;
    let mut flapping = flapping;
    assert_eq!(
        calm.observe_probe(good, top_candidate(), &idle_server(), healthy_buffer(), &healthy_hls(), 600_000),
        RecoveryVerdict::Recover,
    );
    assert_eq!(
        flapping.observe_probe(good, top_candidate(), &idle_server(), healthy_buffer(), &healthy_hls(), 600_000),
        RecoveryVerdict::NotWorthIt,
        "same link, same film, fifth visible switch",
    );
}

/// The safe budget must be PUBLISHED on every observed segment, including the ones where the
/// controller returns before deciding anything.
///
/// It was computed after three early returns — a transaction in flight, and both arms of the dev
/// pin — so on a pinned run, which is every census case, `last_safe_budget_kbps` kept whatever it
/// held before the pin was reached. Measured on the device corpus: 397 of 527 `abr: steady` lines
/// reported `safe=0kbps`, i.e. the central quantity of the admission rule was unobservable on
/// three quarters of the samples, on exactly the runs whose purpose is to characterise a rung.
///
/// Differential: with the computation back below the early return this asserts a positive budget
/// against the zero it was initialised with.
#[test]
fn a_pinned_controller_still_publishes_its_safe_budget() {
    let mut pinned = controller_at(Rung::P1080High).pinned_to(Some(Rung::P1080High));
    assert_eq!(pinned.telemetry().safe_budget_kbps, 0, "nothing observed yet");
    for _ in 0..4 {
        assert!(matches!(pinned.observe_next(sample(40_000, 300, 30_000)), Decision::Stay),
                "already at the pin, so there is nothing to propose\u{2014}but a sample was still taken");
    }
    assert!(
        pinned.telemetry().safe_budget_kbps > 0,
        "a pinned controller updated every estimator from that segment and then reported no \
         budget at all, which is what made three quarters of the census corpus unreadable",
    );
}

/// The visible-switch penalty halves on a CLOCK, and until 2026-08-26 that clock was stopped.
/// The caller passed a literal `0` for elapsed time and the history was frozen into the gate at
/// construction, so a playback that had already switched twice could never return to Original
/// however long it subsequently ran clean — the hysteresis became a latch.
///
/// Differential by construction: with `advance_to` a no-op, the second half of this asserts
/// `Recover` against the `NotWorthIt` the first half just established on identical evidence.
#[test]
fn the_visible_switch_penalty_decays_on_the_worker_clock() {
    let policy = AbrPolicy::measured();
    let flapping = TransitionHistory { visible_switches: 5, since_last_ms: Some(2_000) };
    let good = probe(90_000, true);

    let mut at_once = OriginalRecovery::new(28_000, policy, SourceFeatures::default(), flapping, hd_catalog()).unwrap();
    assert_eq!(
        at_once.observe_probe(good, top_candidate(), &idle_server(), healthy_buffer(), &healthy_hls(), 600_000),
        RecoveryVerdict::NotWorthIt,
        "the fifth switch two seconds ago is still expensive",
    );

    let mut later = OriginalRecovery::new(28_000, policy, SourceFeatures::default(), flapping, hd_catalog()).unwrap();
    // Six half-lives, so the penalty is under 2% of its opening value. The RATE is policy and is
    // not under test here; that the clock advances at all is.
    later.advance_to(policy.visible_switch_decay_ms.saturating_mul(6));
    assert_eq!(
        later.observe_probe(good, top_candidate(), &idle_server(), healthy_buffer(), &healthy_hls(), 600_000),
        RecoveryVerdict::Recover,
        "the same evidence, once the penalty has decayed, is worth acting on",
    );
}

/// `advance_to` is ABSOLUTE, not a delta. The caller ticks it once per segment and segments do
/// not arrive on a regular cadence, so a delta API would make the decay depend on how often it
/// happened to be called — which is a property of the link, not of the switch history.
#[test]
fn advancing_the_switch_clock_is_idempotent_in_the_value_not_the_call_count() {
    let policy = AbrPolicy::measured();
    let flapping = TransitionHistory { visible_switches: 5, since_last_ms: Some(2_000) };
    let good = probe(90_000, true);
    let target = policy.visible_switch_decay_ms.saturating_mul(6);

    let mut once = OriginalRecovery::new(28_000, policy, SourceFeatures::default(), flapping, hd_catalog()).unwrap();
    once.advance_to(target);

    let mut many = OriginalRecovery::new(28_000, policy, SourceFeatures::default(), flapping, hd_catalog()).unwrap();
    for _ in 0..40 {
        many.advance_to(target);
    }
    assert_eq!(
        once.observe_probe(good, top_candidate(), &idle_server(), healthy_buffer(), &healthy_hls(), 600_000),
        many.observe_probe(good, top_candidate(), &idle_server(), healthy_buffer(), &healthy_hls(), 600_000),
        "forty ticks to the same instant is one tick to that instant",
    );
}

/// **N5's endpoints, and the ladder they replaced.** The network term was `1 / 4 / 12 / 40` on
/// four steps of the starvation horizon; it is now linear between the two horizons that already
/// have product meanings, with the ladder's own worst case as its slope.
///
/// Differential by construction — every assertion here contradicts the ladder:
/// a comfortable horizon scored **1** and now scores **0**, the fallback horizon scored **4** and
/// now scores the full **40**, and the band between them was FLAT at 4 where it is now strictly
/// decreasing in `T`. Nothing in this test can be satisfied by the code it replaced.
#[test]
fn the_network_risk_term_is_continuous_between_the_two_horizons_that_already_exist() {
    let policy = AbrPolicy::measured();
    let safe = policy.starvation_safe_secs;
    let fallback = policy.starvation_fallback_secs;
    let net = |secs: Option<u32>| risk_score(secs, None, false, &policy);

    assert_eq!(net(None), 0, "no deficit at all is not a risk");
    assert_eq!(net(Some(safe)), 0, "the ladder charged 1 for a horizon it calls SAFE");
    assert_eq!(net(Some(safe + 600)), 0, "and charges nothing more for being safer still");
    assert_eq!(net(Some(fallback)), 40, "the ladder charged 4 one second above its own floor");
    assert_eq!(net(Some(0)), 40, "below the floor it is an emergency, decided by a hard guard");

    // Strictly decreasing in T across the whole band, which is the property a step ladder cannot
    // have: a 59 s horizon and a 21 s horizon scored the same 4.
    let mut previous = 41;
    for secs in fallback..=safe {
        let score = net(Some(secs));
        assert!(score < previous, "risk must fall as the horizon grows: {secs}s scored {score}");
        assert!(score <= 40, "{secs}s scored {score}, past the term's own ceiling");
        previous = score;
    }

    // The other two terms are unchanged at their endpoints, so every ratio to
    // `visible_switch_cost` that the mode comparison rests on still holds where it was calibrated.
    assert_eq!(risk_score(None, Some(policy.production_safe_pm), false, &policy), 0);
    assert_eq!(risk_score(None, Some(policy.production_max_pm), false, &policy), 20);
    assert_eq!(risk_score(None, None, true, &policy), 30);
    assert_eq!(risk_score(Some(0), Some(policy.production_max_pm), true, &policy), RISK_SCORE_MAX);
}

/// A whole-file average is a LOWER BOUND on demand. The requirement carries VBR headroom, so a
/// link that merely matches the average is already at risk before any busy scene arrives.
#[test]
fn vbr_headroom_makes_the_whole_file_average_a_lower_bound() {
    let policy = AbrPolicy::measured();
    assert_eq!(source_requirement_kbps(40_000, &policy), 54_000);
    assert_eq!(source_requirement_kbps(0, &policy), 0);
    // 40 Mbit/s average, 41 Mbit/s of measured capacity, and the model still sees a deficit —
    // which is the whole point: the file contains scenes above its own average.
    let mut mode = original(40_000);
    let observation = mode
        .observe_saturated(window_bytes(41_000), ORIGINAL_WINDOW_US, Some(30_000), HOUR_MS)
        .unwrap();
    assert!(observation.horizon_secs.is_some(), "a bare average is not headroom");
    assert!(observation.fallback.is_none(), "but 30 s of reserve is not an emergency either");
}

/// A healthy HLS session against a 28 Mbit/s 1080p source, an hour of film left and no switch
/// history — the base every mode-utility fixture varies one or two fields of.
///
/// It exists because five call sites each wrote all fourteen fields out, nine of which were
/// identical in all five: a new `ModeInputs` field was five mechanical edits, and the one or two
/// fields a given test actually varies — the interesting part — were buried in restated
/// boilerplate. Vary with `..mode_inputs()`.
fn mode_inputs() -> ModeInputs {
    ModeInputs {
        current: ModeKind::Hls,
        source_kbps: 28_000,
        source_raster: (1_920, 1_080),
        source_delivery: CapacityEstimate::from_prior(200_000),
        hls_delivery: CapacityEstimate::from_prior(200_000),
        production: ProductionEstimate::default(),
        buffer: BufferEstimate { buffered_ms: 8_000, ..Default::default() },
        remaining_ms: HOUR_MS,
        history: TransitionHistory::default(),
        original_feasible: true,
        source_dv: false,
        source_atmos: false,
        unsafe_deficit_ms: 0,
    }
}

/// **N14 site 3: Original's quality comes from the SOURCE, and a modest source is worth less.**
///
/// Category 8.3. It was `original_quality_bonus + quality_score_at_kbps(candidate(P1080High).expected_wire_kbps)` — a
/// constant 116 regardless of what it was being compared against, so the structural advantage the
/// policy comment reasons about as "40" was in fact +40 against P1080High, +76 against P720 and
/// +116 against P240. A bonus that grows as the alternative worsens is a thumb on the scale.
///
/// Differential by construction: under unmodified code both halves of this are the SAME NUMBER, so
/// the inequality cannot hold. It also pins the direction rather than a value, so the quality
/// curve can be re-shaped without re-fitting it.
#[test]
fn originals_quality_is_scored_from_the_source_and_not_from_a_fabricated_rung() {
    let policy = AbrPolicy::measured();
    let base = ModeInputs {
        current: ModeKind::Hls,
        source_kbps: 28_000,
        source_raster: (1_920, 1_080),
        source_delivery: CapacityEstimate::from_prior(200_000),
        hls_delivery: CapacityEstimate::from_prior(200_000),
        production: ProductionEstimate::default(),
        buffer: BufferEstimate { buffered_ms: 8_000, ..Default::default() },
        remaining_ms: HOUR_MS,
        history: TransitionHistory::default(),
        original_feasible: true,
        source_dv: false,
        source_atmos: false,
        unsafe_deficit_ms: 0,
    };
    let quality_of = |kbps: u32, raster: (u16, u16)| {
        original_utility(
            &ModeInputs { source_kbps: kbps, source_raster: raster, ..base },
            &policy,
        )
        .expect("feasible")
        .quality
    };

    let big = quality_of(28_000, (1_920, 1_080));
    let small = quality_of(2_200, (1_280, 720));
    assert!(
        small < big,
        "a 2.2 Mbps 720p master scored {small} and a 28 Mbps 1080p one {big} — Original's quality \
         must be about the file, and under the fabricated baseline these were the same number",
    );

    // The RASTER is a cap in its own right: the same bitrate in a smaller frame is not worth more.
    assert!(
        quality_of(28_000, (1_280, 720)) < big,
        "a 720p master cannot be worth what a 1080p one is at the same rate",
    );

    // An unstated raster applies no cap — `(0, 0)` is "nobody said", the same reading
    // `HlsActuatorCatalog::limited_to` gives it, and the conservative direction here: refusing to
    // credit a source nobody measured would silently prefer transcoding.
    assert_eq!(
        quality_of(28_000, (0, 0)),
        big,
        "an unmeasured raster must not be read as a forbidden zero-pixel picture",
    );

    // **A rung's raster is a BOUNDING BOX, and every SCOPE film is the case that proves it.** PMS
    // fits the source inside the box and never upscales, so 1920x800 is reproduced exactly by the
    // 1920x1080 rungs and must score as 1080p does. The cap was first written as its own per-axis
    // filter in the inverted direction (`rung_w <= source_w`), which admitted no 1080p rung for a
    // 2.40:1 master and capped it at 4000 kbps — 40 against this 76, on the argmax that decides
    // whether Auto recovers Original at all. `ladder::admits`' doc records the same defect measured
    // on the television from the ladder's side; this asserts it from the mode comparison's.
    for scope in [(1_920u16, 800u16), (1_920, 816), (1_920, 1_040)] {
        assert_eq!(
            quality_of(28_000, scope),
            big,
            "a {}x{} master is reproduced exactly by the 1080p rungs and must score as one does",
            scope.0,
            scope.1,
        );
    }
}

/// **N18: Original's risk is a RECURRING cost and is scaled like one.**
///
/// Category 8.3. `quality` and `features` were already scaled by `benefit_scale_pm`; `risk` was
/// not, which made effective risk aversion inversely proportional to remaining playback — the same
/// defect §7.C rejects for rung selection. `transition` stays outside the scale, because a reload
/// is paid once, now.
///
/// Differential: under unmodified code the two risk terms below are equal.
#[test]
fn originals_risk_shrinks_with_the_playback_it_is_a_risk_to() {
    let policy = AbrPolicy::measured();
    // A source the link cannot carry, so the risk term is non-zero and has something to scale.
    let base = ModeInputs {
        current: ModeKind::Hls,
        source_kbps: 60_000,
        source_raster: (1_920, 1_080),
        source_delivery: CapacityEstimate::from_prior(9_000),
        hls_delivery: CapacityEstimate::from_prior(40_000),
        production: ProductionEstimate::default(),
        buffer: BufferEstimate { buffered_ms: 4_000, ..Default::default() },
        remaining_ms: HOUR_MS,
        history: TransitionHistory::default(),
        original_feasible: true,
        source_dv: false,
        source_atmos: false,
        unsafe_deficit_ms: 0,
    };
    let whole_film = original_utility(&base, &policy).expect("feasible");
    let last_ten_seconds =
        original_utility(&ModeInputs { remaining_ms: 10_000, ..base }, &policy).expect("feasible");
    assert!(whole_film.risk > 0, "the fixture must produce a risk to scale");
    assert!(
        last_ten_seconds.risk < whole_film.risk,
        "risk is a property of the playback that REMAINS: {} with ten seconds left against {} with \
         an hour is the same number, which is the defect",
        last_ten_seconds.risk,
        whole_film.risk,
    );
    assert_eq!(
        last_ten_seconds.transition, whole_film.transition,
        "a reload is paid once, now, and must stay outside the scale",
    );
}

/// **N14 sites 1 and 2: the recovery gate scores the REAL server, not an idle one.**
///
/// Category 8.3. `OriginalRecovery::inputs` hardcoded `ProductionEstimate::default()`, so the HLS
/// side of the argmax was always scored as if PMS could produce anything asked of it — a bias in
/// one direction by exactly the amount the server is actually loaded, on every recovery decision.
///
/// **The fixture has to be CLOSE, and that is what makes it a real test.** On a link this fast
/// Original wins outright the moment the requirement clears, so a loaded server would change
/// nothing observable; three spent visible switches price the reload high enough that the decision
/// is genuinely balanced, and then the server is what tips it. The switch count is a fixture
/// choice, not a threshold — the assertion is that the two verdicts DIFFER, not what they are.
///
/// Differential by construction: unmodified code has no production argument at all, so both gates
/// below are computed from identical inputs and cannot disagree.
#[test]
fn the_recovery_comparison_can_see_a_loaded_server() {
    let policy = AbrPolicy::measured();
    let current = top_candidate();
    let mut loaded = ProductionEstimate::default();
    for _ in 0..6 {
        loaded.observe(policy.production_max_pm * 2, current.production_load_pm, false);
    }
    assert!(loaded.ratio_pm > policy.production_max_pm, "the setup must load the server");

    let spent = TransitionHistory { visible_switches: 3, since_last_ms: Some(0) };
    let gate = || {
        OriginalRecovery::new(28_000, policy, SourceFeatures::default(), spent, hd_catalog()).expect("feasible")
    };
    let (mut idle_gate, mut loaded_gate) = (gate(), gate());
    let verdicts: Vec<(RecoveryVerdict, RecoveryVerdict)> = (0..3)
        .map(|_| {
            (
                idle_gate.observe_probe(
                    probe(50_000, true), current, &idle_server(), healthy_buffer(),
                    &healthy_hls(), HOUR_MS,
                ),
                loaded_gate.observe_probe(
                    probe(50_000, true), current, &loaded, healthy_buffer(),
                    &healthy_hls(), HOUR_MS,
                ),
            )
        })
        .collect();
    assert!(
        verdicts.iter().any(|(idle, busy)| idle != busy),
        "the same probe against the same link gave the same answer on an idle server and on one \
         past its ceiling — which is exactly what a defaulted `ProductionEstimate` guarantees: \
         {verdicts:?}",
    );
}

/// **N14 site 2: the value-of-information gate scores the decision it gates.**
///
/// Category 8.3. `worth_probing` passed the real `current` as BOTH `current_hls` and `best_hls`,
/// so it asked "is Original better than staying exactly here" while the decision it guards asks
/// "is Original better than the best rung this link supports". The app therefore spent real source
/// probes — read over the link the segments need — on questions the decision had already settled
/// the other way.
///
/// Differential: unmodified code cannot distinguish these two runs at all, because the alternative
/// it scores is `current` in both.
#[test]
fn the_probe_gate_weighs_the_rung_the_link_supports_and_not_the_one_it_is_on() {
    let policy = AbrPolicy::measured();
    // Spent switches, so the comparison is close enough that the ALTERNATIVE decides it.
    let spent = TransitionHistory { visible_switches: 3, since_last_ms: Some(0) };
    let gate = || {
        OriginalRecovery::new(28_000, policy, SourceFeatures::default(), spent, hd_catalog()).expect("feasible")
    };
    let floor = hd_catalog().candidate(Rung::P480);
    // A link with room for the top rung. Sitting at the FLOOR, the honest alternative to Original
    // is the top rung this link supports — not the 720 kbps that happens to be playing.
    let roomy = healthy_hls();
    let due_at_floor = gate().probe_due(
        floor, &idle_server(), sample(40_000, 200, 20_000), healthy_buffer(), &roomy, HOUR_MS,
        AbrPolicy::measured().probe_spacing_ms * 4,
    ).is_ok();
    let due_at_top = gate().probe_due(
        top_candidate(), &idle_server(), sample(40_000, 200, 20_000), healthy_buffer(), &roomy,
        HOUR_MS, AbrPolicy::measured().probe_spacing_ms * 4,
    ).is_ok();
    assert_eq!(
        due_at_floor, due_at_top,
        "the alternative is the best rung the link supports either way, so which rung happens to \
         be playing must not change whether a probe is worth spending",
    );
}

/// **N18 is a PARTITION and it must hold on both sides of the argmax** (found by adversarial
/// review of I7a).
///
/// The rule N18 states is not a preference about risk aversion, it is a classification of terms: a
/// cost paid ONCE is outside `benefit_scale_pm` and a cost paid for every remaining segment is
/// inside it. `original_utility` classified quality, features and risk as recurring and
/// `transition` as one-off. `hls_utility` classified quality alone, so `risk` and `server` kept
/// full weight however little film was left — and both are charged per segment for the rest of the
/// playback, exactly as quality is.
///
/// The asymmetry decides reloads rather than tidiness. In `OriginalRecovery` the Original side's
/// risk score is identically zero (both paths reach `original_utility` only past a capacity test
/// that empties the starvation band, and its `inputs` hardcodes `unsafe_deficit_ms: 0`), so with a
/// short horizon the comparison reduces to `-transition` against `-(risk + server)` with one side
/// discounted to almost nothing and the other not.
///
/// Differential by construction, and it asserts the RULE rather than a downstream consequence:
/// under unmodified code the two HLS terms are invariant to `remaining_ms`, so the strict
/// inequalities cannot hold. `transition` is the control — it must NOT move, or the test would
/// pass against code that scaled everything indiscriminately.
#[test]
fn every_recurring_term_scales_with_the_horizon_and_the_reload_does_not() {
    let policy = AbrPolicy::measured();
    let candidate = hd_catalog().candidate(Rung::Uhd);
    let current = hd_catalog().candidate(Rung::P480);
    // A state with a real risk score and a real server cost to discount: a session whose
    // conservative reading does not cover the candidate, against the 4K point's measured 2.1x
    // production load. `history` carries a switch so `transition` is non-zero and can act as the
    // control.
    let inputs = ModeInputs {
        current: ModeKind::Original,
        source_kbps: 28_000,
        source_raster: (1_920, 1_080),
        source_delivery: CapacityEstimate::from_prior(30_000),
        hls_delivery: CapacityEstimate::from_prior(9_000),
        production: ProductionEstimate::default(),
        buffer: BufferEstimate { buffered_ms: 4_000, ..Default::default() },
        remaining_ms: HOUR_MS,
        history: TransitionHistory::default(),
        original_feasible: true,
        source_dv: false,
        source_atmos: false,
        unsafe_deficit_ms: 0,
    };
    let long = hls_utility(candidate, current, &inputs, &policy);
    let short = hls_utility(
        candidate,
        current,
        &ModeInputs { remaining_ms: 8_000, ..inputs },
        &policy,
    );

    assert!(long.risk > 0, "the fixture must carry a real risk cost or this grades nothing");
    assert!(long.server > 0, "and a real server cost");
    assert!(
        short.risk < long.risk,
        "risk is charged on every remaining segment, so it must shrink with the horizon: {} at an \
         hour and {} at eight seconds",
        long.risk,
        short.risk,
    );
    assert!(
        short.server < long.server,
        "so is the server's production load: {} at an hour and {} at eight seconds",
        long.server,
        short.server,
    );
    assert_eq!(
        short.transition, long.transition,
        "the reload is paid ONCE and must stay outside the scale — without this the assertions \
         above would pass against code that scaled every term indiscriminately",
    );

    // The same partition on the Original side, so the two are demonstrably one rule.
    let orig_long = original_utility(&inputs, &policy).expect("feasible");
    let orig_short =
        original_utility(&ModeInputs { remaining_ms: 8_000, ..inputs }, &policy).expect("feasible");
    assert!(orig_short.quality < orig_long.quality);
    assert!(orig_short.features < orig_long.features);
    assert_eq!(orig_short.transition, orig_long.transition);
}

/// **§7.H: the whole comparison is published, so a log can explain a mode switch.**
///
/// `ModeUtility` has always been kept as its component terms "because the event log prints them —
/// *Original lost* is not a diagnosis". Every `choose_mode` call site discarded the reason and both
/// utilities, so the sentence was aspirational and the one question an operator asks after a
/// visible switch had no answer in the log.
///
/// Differential: unmodified code publishes nothing. The `hls_rung` assertion is the second half —
/// it is the value N14 site 1 was fabricating, so a comparison that reported P1080High while
/// playing a 4 Mbps rung would be the defect surviving inside its own instrument.
#[test]
fn a_recovery_decision_publishes_the_comparison_it_was_made_on() {
    let mut gate = recovery(28_000);
    assert!(gate.comparison().is_none(), "nothing has been compared yet");

    let current = hd_catalog().candidate(Rung::P720);
    assert_eq!(
        gate.observe_probe(
            probe(2_000, false), current, &idle_server(), healthy_buffer(), &healthy_hls(), HOUR_MS,
        ),
        RecoveryVerdict::Insufficient,
    );
    assert!(
        gate.comparison().is_none(),
        "a truncated probe never reaches a comparison, so there is nothing to publish — and a \
         stale one beside a fresh verdict is the trap this whole line exists to avoid",
    );

    assert_eq!(
        gate.observe_probe(
            probe(80_000, true), current, &idle_server(), healthy_buffer(), &healthy_hls(), HOUR_MS,
        ),
        RecoveryVerdict::Recover,
    );
    let cmp = gate.comparison().expect("a real decision publishes its basis");
    assert_eq!(cmp.chosen, ModeKind::Original);
    assert_eq!(cmp.reason, ModeReason::OriginalWorthIt);
    assert!(cmp.loser.is_some(), "the alternative was scored, so it must be readable");
    assert_ne!(
        cmp.hls_rung,
        Rung::P720,
        "the comparison must be against the best rung the link supports, not the one playing",
    );

    // **The order that actually exposes staleness, and the one this test was missing.** Truncating
    // the FIRST probe proves only that a comparison starts absent. `ff.rs` emits `abr: mode` off
    // `comparison()` on EVERY probe result, so a probe that fails AFTER one succeeded printed the
    // earlier decision immediately above its own `verdict=Insufficient`, with nothing marking it
    // stale — and `RE_ABR_MODE` parsed it as a decision that had just been taken.
    assert_eq!(
        gate.observe_probe(
            probe(2_000, false), current, &idle_server(), healthy_buffer(), &healthy_hls(), HOUR_MS,
        ),
        RecoveryVerdict::Insufficient,
    );
    assert!(
        gate.comparison().is_none(),
        "a truncated probe must RETIRE the previous comparison, not leave it standing beside a \
         verdict it had no part in",
    );
    assert_eq!(cmp.scale_pm, 1_000, "an hour of film is the full benefit scale");
    let w = cmp.winner;
    assert_eq!(
        w.quality + w.features - w.risk - w.server - w.transition,
        w.total,
        "the terms must reconstruct the total, or decomposing them explains nothing",
    );
}

/// **N13: the Original persistence rule is WALL clock, and the two clocks really do diverge.**
///
/// Category 8.3. `ORIGINAL_DEFICIT_WINDOWS = 6` counted windows of [`ORIGINAL_WINDOW_US`] ACTIVE
/// BODY-READ time. Under backpressure — the healthy full-buffer case, where the reader is parked on
/// purpose — one such window spans unbounded WALL time, so "six windows" named no duration at all,
/// and the module said so twice in two different units: "about four and a half seconds of real
/// transfer" in one doc and "about nine seconds" in another, for the same counter.
///
/// Differential by construction, and it is the divergence itself that is asserted: two runs deliver
/// IDENTICAL bytes over identical active-read time, so the retired counter cannot tell them apart,
/// while their wall clocks differ by 8x. Under unmodified code both fire; under N13 only the one
/// that really spent the time does.
///
/// The direction is the safe one: the wall interval is LONGER under backpressure, so the new rule
/// is at least as patient as the old, never hastier. The observed ratio on a real link is an M2
/// measurement nobody has taken.
#[test]
fn a_deficit_measured_in_active_read_time_is_not_a_duration() {
    let policy = AbrPolicy::measured();
    // **Unsafe but not IMMINENT**, which the fixture has to arrange deliberately: a link far under
    // the requirement puts the horizon inside `starvation_fallback_secs` within a few windows, and
    // then `ImminentStarvation` — a hard guard that consults no utility and no persistence — fires
    // first and this test grades that instead. A deep, almost-flat reserve keeps the horizon in the
    // band between the two policy horizons, which is the only region `SustainedDeficit` owns.
    let starved = |n: u64| window_bytes(9_000) * n;
    let fell = |n: u64| -> i64 { 45_000 - 300 * n as i64 };

    // Wall == active: the saturated reader. This is the case the retired count described.
    let mut saturated = original(60_000);
    let mut fired_saturated = None;
    for n in 1..=10u64 {
        let obs = saturated.observe(
            starved(n), ORIGINAL_WINDOW_US * n, Some(fell(n)), HOUR_MS,
            ORIGINAL_WINDOW_US * n / 1_000,
        );
        if let Some(o) = obs {
            if o.fallback == Some(OriginalExit::SustainedDeficit) {
                fired_saturated = Some(o.unsafe_deficit_ms);
                break;
            }
        }
    }
    let at = fired_saturated.expect("a sustained deficit on a saturated reader must be called");
    assert!(
        at >= policy.sustained_unsafe_deficit_ms,
        "it fired at {at}ms, under its own threshold of {}ms",
        policy.sustained_unsafe_deficit_ms,
    );

    // Same bytes, same active time, one EIGHTH of the wall clock — a reader that spent most of
    // each interval parked on a full buffer would be the other way round; this is the shape that
    // proves the counter and the duration are different quantities.
    let mut throttled = original(60_000);
    let mut fired_throttled = false;
    for n in 1..=10u64 {
        let obs = throttled.observe(
            starved(n), ORIGINAL_WINDOW_US * n, Some(fell(n)), HOUR_MS,
            ORIGINAL_WINDOW_US * n / 8_000,
        );
        fired_throttled |= obs.is_some_and(|o| o.fallback == Some(OriginalExit::SustainedDeficit));
    }
    assert!(
        !fired_throttled,
        "identical bytes over identical active-read time fired the sustained-deficit exit on one \
         eighth of the wall clock — which is what a count of active-read windows guarantees, and \
         what N13 removes",
    );
}

/// **N16: three ordered feature bonuses where there was one flat boolean.**
///
/// Category 8.3. `route::auto_original_features` returned `dovi.profile > 0 || immersive` and it
/// was worth a flat 25, so an Atmos-only film bought two visible reloads for a benefit inaudible on
/// television speakers, priced identically to a Dolby Vision panel-mode change.
///
/// **The ORDER is asserted and the magnitudes are not**, because §6.2 says all three rows are
/// "ordering yes, magnitude no". A test that pinned 13/8/4 would be pinning a rank weighting as if
/// it were a measurement, and would have to be re-fitted by the first person who measures one.
///
/// Differential: unmodified code has a single `original_feature_bonus`, so all four states below
/// collapse to two values.
#[test]
fn the_feature_bonus_is_ordered_and_its_magnitudes_are_not_the_claim() {
    let policy = AbrPolicy::measured();
    assert!(
        policy.dv_bonus > policy.generation_loss_bonus
            && policy.generation_loss_bonus > policy.atmos_bonus
            && policy.atmos_bonus > 0,
        "Dolby Vision is a visible panel-mode change, generation loss is true of every Original, \
         and Atmos is inaudible on this television's speakers — that ORDER is the whole claim",
    );
    assert_eq!(
        policy.dv_bonus + policy.generation_loss_bonus + policy.atmos_bonus,
        25,
        "the total is preserved from the flat bonus, so nothing else in the utility moves",
    );

    let base = ModeInputs {
        current: ModeKind::Hls,
        source_kbps: 28_000,
        source_raster: (1_920, 1_080),
        source_delivery: CapacityEstimate::from_prior(200_000),
        hls_delivery: CapacityEstimate::from_prior(200_000),
        production: ProductionEstimate::default(),
        buffer: BufferEstimate { buffered_ms: 8_000, ..Default::default() },
        remaining_ms: HOUR_MS,
        history: TransitionHistory::default(),
        original_feasible: true,
        source_dv: false,
        source_atmos: false,
        unsafe_deficit_ms: 0,
    };
    let features = |dv: bool, atmos: bool| {
        original_utility(&ModeInputs { source_dv: dv, source_atmos: atmos, ..base }, &policy)
            .expect("feasible")
            .features
    };
    let plain = features(false, false);
    assert!(
        plain > 0,
        "no re-encode at all is a real benefit of EVERY Original, and pricing it at zero for a \
         plain file while pricing DV and Atmos together at 25 is the conflation N16 names",
    );
    assert!(features(true, false) > features(false, true), "DV outranks Atmos");
    assert!(features(false, true) > plain, "and Atmos is still worth something");
    assert_eq!(
        features(true, true),
        plain + policy.dv_bonus + policy.atmos_bonus,
        "the three terms compose by addition; nothing double-counts",
    );
}

/// **I8: a seek carries the link estimate, and `from_prior` cannot express one.**
///
/// Category 8.3. An HLS seek destroys the engine and builds a fresh `Controller`; the only thing
/// that survived was `session().auto_prior_kbps`, whose writer on the Original->HLS fallback path
/// is the rate measured *at the moment the link failed*. So after one bad patch every subsequent
/// seek re-seeded from the worst rate the playback had ever measured, at maximum uncertainty with
/// one sample, and the ladder re-ramped for five to ten segments.
///
/// The two constructors make two different CLAIMS and that is the whole increment: `from_prior`
/// pins uncertainty at its cap and asserts one sample, which is the honest reading of a bootstrap
/// probe and a false one for an estimate that has watched a link for a minute.
///
/// Differential: under unmodified code there is no `from_snapshot`, so a seeded controller cannot
/// hold more than one sample and its conservative budget is exactly half its rate.
#[test]
fn a_carried_estimate_says_more_than_a_prior_of_the_same_rate() {
    let settled = CapacityEstimate::from_snapshot(40_000, 41_000, 200, 19)
        .expect("a settled estimate is an estimate");
    let bootstrap = CapacityEstimate::from_prior(40_000);
    assert_eq!(bootstrap.samples, 1, "a prior asserts one observation");
    assert_eq!(
        bootstrap.conservative_kbps(),
        20_000,
        "and at the uncertainty cap that is half the rate — the right claim about a probe",
    );
    assert!(
        settled.conservative_kbps() > bootstrap.conservative_kbps() * 3 / 2,
        "carrying what was actually observed must be worth substantially more than restating the \
         rate as a probe: {} against {}",
        settled.conservative_kbps(),
        bootstrap.conservative_kbps(),
    );

    // Absence is absence, not a zero-rate estimate: an unwritten snapshot must not seed anything.
    assert!(CapacityEstimate::from_snapshot(0, 0, 0, 0).is_none());
    assert!(CapacityEstimate::from_snapshot(40_000, 40_000, 200, 0).is_none(), "no samples");
    assert!(CapacityEstimate::from_snapshot(0, 40_000, 200, 9).is_none(), "no rate");
    // The cap is a cap on the way in too — a snapshot cannot claim more confidence than the
    // estimator's own floor allows.
    assert_eq!(
        CapacityEstimate::from_snapshot(40_000, 40_000, 900, 9).unwrap().uncertainty_pm,
        MAX_UNCERTAINTY_PM,
    );
}

/// **I8, the other half: what a seek must NOT carry.**
///
/// The link did not change because the viewer jumped, so the delivery estimate crosses. Everything
/// positional does not — the buffer describes a reserve at an offset that no longer exists, the
/// risk history was computed from it, and a pending transaction was proposed for it. The new
/// `Controller` gets those right by CONSTRUCTION, which is worth an assertion precisely because it
/// is the kind of correctness that a later refactor can quietly lose.
#[test]
fn a_seeded_controller_carries_the_link_and_nothing_positional() {
    let carried = CapacityEstimate::from_snapshot(40_000, 41_000, 200, 19).unwrap();
    let c = Controller::starting_at(Rung::P1080, Some(carried), hd_catalog());
    let t = c.telemetry();
    assert_eq!(t.delivery, carried, "the link estimate crosses whole");
    assert_eq!(t.buffer.buffered_ms, 0, "the reserve at the old position is not a reserve here");
    assert_eq!(t.buffer.samples, 0, "nor is its history");
    assert_eq!(t.gates.draining, 0);
    assert_eq!(t.pending, None, "a transaction proposed for the old position must not survive");
    assert_eq!(t.gates.dwell_ms, 0, "and no encoder has been started on this side of the seek");
    assert_eq!(t.gates.blocked_kbps, 0);
}

/// Utility is not a bitrate comparison: Original wins from BEHIND on wire rate because it has
/// no generation loss and asks the server for no video encoding at all.
#[test]
fn original_beats_the_top_rung_on_utility_at_equal_risk() {
    let policy = AbrPolicy::measured();
    let inputs = ModeInputs {
        current: ModeKind::Original,
        source_kbps: 28_000,
        // A 1080p master, the raster the rest of this fixture is written around.
        source_raster: (1_920, 1_080),
        source_delivery: CapacityEstimate::from_prior(80_000),
        hls_delivery: CapacityEstimate::from_prior(80_000),
        production: ProductionEstimate::default(),
        buffer: BufferEstimate { buffered_ms: 30_000, ..Default::default() },
        remaining_ms: HOUR_MS,
        history: TransitionHistory::default(),
        original_feasible: true,
        source_dv: false,
        source_atmos: false,
        unsafe_deficit_ms: 0,
    };
    let current = hd_catalog().candidate(Rung::P1080High);
    let (mode, reason, chosen, other) =
        choose_mode(&inputs, current, current, &policy);
    assert_eq!((mode, reason), (ModeKind::Original, ModeReason::OriginalWorthIt));
    assert_eq!(chosen.server, 0, "no server video encoding is the term HLS cannot match");
    assert!(chosen.total > other.expect("both were feasible").total);

    // Infeasible is not a low score — it is not a candidate.
    let (mode, reason, _, other) = choose_mode(
        &ModeInputs { original_feasible: false, ..inputs },
        current,
        current,
        &policy,
    );
    assert_eq!((mode, reason), (ModeKind::Hls, ModeReason::OriginalInfeasible));
    assert!(other.is_none());
}

/// **A downshift pin must land from the top of the ladder.** It could not, and the M4 census paid
/// for it: four of its seven points never reached their pinned rung and silently recorded the top
/// rung five times instead (`pin_320`, `pin_2000`, `pin_10000`, `pin_16000` all logged
/// `rung=20000` with byte lists identical to `pin_20000`'s).
///
/// The cause is a derivation applied in a direction it was never argued for.
/// `PIN_MIN_RESERVE_SEGMENTS = 6` is built from `candidate_warmup_budget` +
/// `candidate_prime_budget` + `candidate_ready`'s residual — none of which a downshift pays: it
/// has no graded segment, and its warm-up is bounded by the reserve rather than by a multiple of
/// the content duration. Six segments is 12 000 ms at `D = 2000`, while the reachable
/// reserve at rung 20000 is `B_max ≈ 5 421 ms`, so the gate was unsatisfiable by construction
/// exactly where the census most needed it.
///
/// Differential: the reserve here is four segments — above `PIN_MIN_RESERVE_SEGMENTS_DOWN` and
/// below `PIN_MIN_RESERVE_SEGMENTS` — so against the unmodified gate this can only return `Stay`,
/// and it is also inside what the top rung can actually hold, so it is a reachable state rather
/// than a hypothetical one.
#[test]
fn a_downshift_pin_lands_from_the_top_of_the_ladder() {
    let mut pinned = controller_at(Rung::Uhd).pinned_to(Some(Rung::P240));
    let reserve_ms = 4 * 2_000;

    let decision = (0..6)
        .map(|_| pinned.observe_next(sample(40_000, 300, reserve_ms)))
        .find(|decision| !matches!(decision, Decision::Stay));

    match decision {
        Some(Decision::Prime(proposal)) => {
            assert_eq!(proposal.rung, Rung::P240, "the pin is the target, not one rung down");
            assert_eq!(proposal.direction, Direction::Down);
        }
        other => panic!(
            "a pin four segments into a reserve the top rung can actually hold never proposed \
             anything ({other:?}) — this is the gate that cost the census four of seven points",
        ),
    }
}

/// The upshift half of the same gate is unchanged, and this is what stops the fix above from
/// being "lower the threshold until the pin lands". Four segments of reserve is still short of the
/// six an upshift transaction has to afford, so an upward pin must keep waiting.
#[test]
fn an_upshift_pin_still_waits_for_the_full_reserve() {
    let mut pinned = controller_at(Rung::P240).pinned_to(Some(Rung::Uhd));
    let reserve_ms = 4 * 2_000;

    for _ in 0..6 {
        assert!(
            matches!(pinned.observe_next(sample(40_000, 300, reserve_ms)), Decision::Stay),
            "an upshift pin transacted on a reserve smaller than the transaction costs, which is \
             the livelock PIN_MIN_RESERVE_SEGMENTS exists to prevent",
        );
    }
}

/// A rejected candidate still MEASURED the link, so its graded segment is evidence and the window
/// keeps it. Differential: it fails against a `candidate_ready` that leaves the window untouched.
#[test]
fn a_candidates_graded_segment_enters_the_window_even_when_the_candidate_is_refused() {
    let mut controller = Controller::starting_at(Rung::P1080, None, HlsActuatorCatalog::measured());
    let before = controller.window_len();
    controller.observe_candidate(sample_bytes(2_000_000, 1_500_000, 900, 20_000));
    assert_eq!(controller.window_len(), before + 1);
}

/// The window is about the LINK, so a sample taken at one rung bounds another by BYTES. This pins
/// that `observe_candidate` really reaches the same store `observe` fills, rather than a second
/// one that would silently give the rule half its evidence.
#[test]
fn candidate_and_current_observations_share_one_window() {
    let mut controller = Controller::starting_at(Rung::P1080, None, HlsActuatorCatalog::measured());
    controller.observe_next(sample(9_000, 400, 20_000));
    let after_current = controller.window_len();
    controller.observe_candidate(sample_bytes(2_000_000, 1_500_000, 900, 20_000));
    assert_eq!(after_current, 1);
    assert_eq!(controller.window_len(), 2);
}

// ---------------------------------------------------------------------------------------------
// The emergency deadline, and the two ways it was unreachable
// ---------------------------------------------------------------------------------------------

/// **The measured defect, as a unit test.** `pipe_abr_down_collapse`, 2026-08-27: the controller
/// committed to rung 14000 and the link collapsed to 498 kbps during the very next fetch. The
/// first sample of that rung reported `net=498kbps buf=2210ms` and the controller's own
/// `starve=2` — and decided `stay`, because I3's cold-start gate covered every trigger. The next
/// sample arrived **58.3 seconds** later (`total_ms=58301`), and the picture was frozen for 47 of
/// them.
///
/// The reasoning I3 shipped with was that "a real collapse waits one segment rather than being
/// hidden". A segment is `bytes / C` of wall time and `C` is the quantity that collapsed, so that
/// wait is unbounded exactly when it is being relied on.
///
/// Differential: against the gate as shipped this is `Decision::Stay`.
#[test]
fn a_collapse_on_the_first_sample_of_a_rung_is_not_hidden_by_the_cold_start_gate() {
    let mut controller = controller_at(Rung::P1080M14);
    let decision = controller.observe_next(sample(498, 29_150, 2_210));
    assert!(
        matches!(decision, Decision::Prime(p) if p.direction == Direction::Down),
        "a 28x measured rate deficit with 2.2 s of reserve is an emergency on the sample that \
         observes it, not one segment later; got {decision:?}",
    );
    assert_eq!(
        controller.telemetry().reason,
        Some(DecisionReason::Hls(HlsReason::StarvationHorizon)),
        "the deadline is what fired, and the log has to say so",
    );
    assert_eq!(
        controller.telemetry().emergency_horizon_secs,
        Some(2),
        "2210ms * 14000 / (14000 - 498) = 2291ms — the same 2 seconds the television published",
    );
}

/// **Why the emergency horizon may not be computed on `conservative_kbps()`, which is the form the
/// risk score uses one line away.**
///
/// `uncertainty_pm` is pinned at its 500 cap on the first sample of every rung, so the
/// conservative rate is exactly HALF the measured one there. On a link delivering precisely what
/// the rung asks, that reads as a 2x deficit: `T = B * R / (R - R/2) = 2B`, which at a one-segment
/// cold-start reserve is 3.9 s and fires an emergency on the healthiest playback there is.
///
/// Differential in the strict sense — it passes only because the predicate reads the measured
/// rate. Conservatism belongs to admission, where a rung you have not tried might be dearer than
/// you think; eviction has the link in front of it and needs no discount.
#[test]
fn a_link_delivering_exactly_the_rungs_rate_is_not_an_emergency_at_cold_start() {
    let mut controller = controller_at(Rung::P1080M14);
    let decision = controller.observe_next(sample(14_000, 250, 1_958));
    assert_eq!(
        decision,
        Decision::Stay,
        "capacity == requirement is an INFINITE horizon; a 500pm confidence discount is not a \
         measured deficit",
    );
    // The counterfactual, written out rather than described: the same reserve and the same
    // segment, scored on the rate the RISK term uses. `uncertainty_pm` is at its 500 cap here, so
    // that is half the measured rate — a 2x deficit that was never observed, and a horizon inside
    // the window. This is the assertion the test exists for; without it, "measured rather than
    // conservative" is satisfiable by any predicate that happens not to fire.
    let measured = controller.telemetry().delivery.fast_kbps;
    let conservative = controller.telemetry().delivery.conservative_kbps();
    assert_eq!(conservative, measured / 2, "the first sample of a rung is a 500pm discount");
    let counterfactual = starvation_horizon(1_958, 14_000, conservative).seconds;
    assert!(
        counterfactual.is_some_and(|s| s <= AbrPolicy::measured().starvation_fallback_secs),
        "the conservative form fires here — {counterfactual:?} — which is the defect being avoided",
    );
    let measured_horizon = controller.telemetry().emergency_horizon_secs;
    assert!(
        measured_horizon.map_or(true, |s| s > AbrPolicy::measured().starvation_fallback_secs * 100),
        "on the measured rate the horizon is unreachable rather than imminent; got \
         {measured_horizon:?}",
    );
}

/// The exemption priced from the other end, so the cost of ungating is a tested number rather than
/// a claim in a comment. At the cold-start floor of one 2 s segment the deadline reaches
/// `starvation_fallback_secs = 20` at a measured deficit of 10% (`2 / 0.1 = 20`), and not before.
#[test]
fn the_cold_start_floor_fires_at_a_tenth_of_the_rate_and_not_at_a_twentieth() {
    let window = AbrPolicy::measured().starvation_fallback_secs;

    // 10% short of the rung's 14 000 kbps, one segment of reserve: T = 2000 * 14000 / 1400 = 20 s,
    // the window exactly. (`sample` quantizes the rate through a byte count, so the horizon lands
    // a second under; the claim is the bracket, not the digit.)
    let mut fires = controller_at(Rung::P1080M14);
    assert!(
        matches!(fires.observe_next(sample(12_600, 250, 2_000)), Decision::Prime(_)),
        "a tenth short with one segment left is the 20 s deadline",
    );
    assert!(fires.telemetry().emergency_horizon_secs.is_some_and(|s| s <= window));

    // 5% short: T = 2000 * 14000 / 700 = 40 s. Twice the window, so nothing fires — and the
    // cold-start gate then covers the `buffered < segment` disjunct as it always did.
    let mut holds = controller_at(Rung::P1080M14);
    assert_eq!(holds.observe_next(sample(13_300, 250, 2_000)), Decision::Stay);
    assert!(
        holds.telemetry().emergency_horizon_secs.is_some_and(|s| s > window),
        "twice the window is not a deadline; got {:?}",
        holds.telemetry().emergency_horizon_secs,
    );
}

/// **I5's stated host differential**: the emergency predicate must not fire at a full
/// `B_max(P1080High)` reserve with a 5% deficit. 4 960 ms is the DEVICE census of that rung
/// (`sim::Calibration::census_buf_ms(20_000)`), not a modelled ceiling, and
/// `4960 * 20011 / 1001 = 99 s` is nowhere near the 20 s window.
///
/// The controller does still descend here, on `network_bad` — a bare rate comparison with no
/// reserve in it, which N4 deletes as a trigger in a later increment. That is why this asserts the
/// REASON: it grades the deadline, which is the surface this change moves, and it will keep
/// grading it when the other trigger goes.
#[test]
fn a_five_percent_deficit_against_a_full_top_rung_reserve_is_not_a_deadline() {
    let mut controller = controller_at(Rung::P1080High);
    let full = super::sim::Calibration::census_buf_ms(20_000).expect("censused");
    // 5% short of the top rung's 20 011 kbps expected wire rate.
    for _ in 0..2 {
        controller.observe_next(sample(19_010, 250, full));
    }
    assert_eq!(
        controller.telemetry().emergency_horizon_secs,
        Some(99),
        "99 seconds of reserve is not an emergency, and the predicate has to say the number",
    );
    assert_ne!(
        controller.telemetry().reason,
        Some(DecisionReason::Hls(HlsReason::StarvationHorizon)),
        "the deadline must not be what moved this rung",
    );
}

/// **N4's incumbent clause: a deficit is a reason not to CLIMB, not a reason to descend.**
///
/// `network_bad` — `immediate_network < expected_wire_kbps`, a bare rate comparison with no
/// reserve in it — was a downshift trigger, so a link 1% short of the rung it was playing evicted
/// that rung against a completely full buffer. Deleted as a trigger here; the same deficit still
/// narrows `safe_budget`, which is where it belongs.
///
/// Differential in both directions, which is the point: the SAME 5% deficit must be ignored at a
/// deep reserve and acted on at a shallow one, because what separates them is the deadline and
/// nothing else. Against the trigger as shipped, the first half fails.
#[test]
fn a_rate_deficit_evicts_a_rung_only_when_the_deadline_says_so() {
    let policy = AbrPolicy::measured();
    // 5% short of P1080M14's 14 000 kbps expected wire rate.
    let short = 13_300;

    // Deep: the device census of the top rung is 4 960 ms and this is deeper still, so the
    // horizon is minutes. Two samples, because the first is the cold start.
    let mut deep = controller_at(Rung::P1080M14);
    for _ in 0..3 {
        assert_eq!(
            deep.observe_next(sample(short, 250, 20_000)),
            Decision::Stay,
            "a 5% deficit against 20 s of reserve is arithmetic, not an emergency",
        );
    }
    assert!(
        deep.telemetry().emergency_horizon_secs.is_some_and(|s| s > policy.starvation_fallback_secs),
        "and the deadline has to be the thing that says so: {:?}",
        deep.telemetry().emergency_horizon_secs,
    );

    // Shallow: the same deficit, a reserve one segment deep. `2000 * 14000 / 700 = 40 s`, still
    // outside the window — so this leg descends on `starving()`, not on the deadline, and the
    // assertion is that SOMETHING still evicts. Without it the test above is satisfied by a
    // controller that never downshifts at all.
    let mut shallow = controller_at(Rung::P1080M14);
    shallow.observe_next(sample(short, 250, 2_000));
    assert!(
        matches!(shallow.observe_next(sample(short, 250, 1_500)), Decision::Prime(p)
                 if p.direction == Direction::Down),
        "a reserve under one segment is still an emergency on the second sample",
    );
}

/// **N4's affordability disjunct is REDUNDANT at the measured `E_tx_down`, so it is not built.**
///
/// The plan lists `B < E_tx_down` as a hard guard in its own right. `E_tx_down` is measured at
/// 1 424 ms (`docs/measurements/j3b-deadline.md`), and `starving()`'s first arm already fires at
/// `B <= 2 000`. So `B < 1424` implies `starving()` implies the emergency, everywhere, with no
/// reachable state in between — adding it would be one condition under two names, which is the
/// exact defect `candidate_prime_budget` was already caught committing when a literal `4/5` and
/// `production_max_pm` were the same threshold at two values.
///
/// This test is what makes the omission checkable rather than an assertion in a commit message: if
/// either number moves so that the implication fails, it goes red and the guard has to be built.
#[test]
fn the_affordability_guard_is_subsumed_by_the_starvation_arm() {
    const E_TX_DOWN_MS: i64 = 1_424; // j3b-deadline.md, median of 17 committed down-legs
    let policy = AbrPolicy::measured();
    assert!(
        E_TX_DOWN_MS <= policy.emergency_buffer_ms,
        "a reserve too small to afford a downshift ({E_TX_DOWN_MS}ms) must already be starving \
         (arm fires at <= {}ms); if this fails, N4's affordability disjunct is reachable and has \
         to be built",
        policy.emergency_buffer_ms,
    );
    // The implication, exercised rather than argued: a reserve just under E_tx_down is starving.
    let mut buffer = BufferEstimate::default();
    buffer.update(Some(E_TX_DOWN_MS - 1), 2_000);
    assert!(buffer.starving(), "the starvation arm covers the whole unaffordable region");
}

// ---------------------------------------------------------------------------------------------
// N3 — the reachable ceiling, and the two gates derived from it
// ---------------------------------------------------------------------------------------------

/// MATHEMATICAL INVARIANT (N3 point 2, the stated proof obligation).
///
/// The refill filter is `R_j <= C_safe * H / (H + D_j)` with `D_j = max(0, B*(R_j) - B)`.
/// `B_max_est` DECREASES in `R`, so `D_j` decreases in `R_j`, so `R_max_j` INCREASES in `R_j` —
/// both sides of the comparison move the same way, which is precisely the shape that can admit a
/// SCATTERED set rather than a prefix of the ladder. Every selector downstream assumes a prefix.
///
/// It is a prefix today only because `B*` is capped at `buffer_target_ms`, which pins `R_max_j`
/// into `[0.8*C_safe, C_safe]` at `H = 10 s`. That is a property of the current numbers and not of
/// the form, so it is swept rather than argued: if `buffer_target_ms` rises after M4 and the cap
/// stops binding, this is what goes red.
#[test]
fn the_refill_filter_admits_a_prefix_of_the_ladder() {
    let policy = AbrPolicy::measured();
    for &safe in &[320u32, 720, 2_000, 4_000, 10_000, 20_000, 45_000, 120_000] {
        for &buffered in &[0i64, 500, 2_000, 2_500, 6_000, 20_000, 90_000] {
            let admitted: Vec<bool> = LADDER
                .iter()
                .map(|rung| {
                    let wire = HlsActuatorCatalog::measured().candidate(*rung).expected_wire_kbps;
                    plant::refill_admits(
                        wire,
                        wire.saturating_sub(policy.assumed_audio_kbps),
                        policy.assumed_audio_kbps,
                        buffered,
                        safe,
                        &policy,
                    )
                })
                .collect();
            // A prefix: once it stops admitting, it never resumes.
            let first_refusal = admitted.iter().position(|ok| !ok);
            if let Some(cut) = first_refusal {
                assert!(
                    admitted[cut..].iter().all(|ok| !ok),
                    "scattered admissible set at safe={safe}kbps buffered={buffered}ms: {admitted:?}",
                );
            }
        }
    }
}

/// **Where the refill filter BINDS, and where it is shadowed — stated rather than assumed.**
///
/// A filter that can never change an outcome is not a guard, it is dead code that passes. This
/// pins both halves of the truth at today's numbers:
///
/// * it binds on `best_sustainable` itself whenever the reserve is under `buffer_target_ms`, and
///   the haircut at an empty buffer is exactly `H/(H+B*)` = `10000/12500` = 0.8 of `C_safe` —
///   derived, not chosen;
/// * on the DECISION path it is currently shadowed, because the reserve gate that runs after it
///   demands `min(3*segment, alpha*B_max_est)` and that is above `buffer_target_ms` at every rung
///   on this ladder. So the filter's live effect today is on the telemetry `optimal` and on
///   selection at low reserves, not on whether an upshift fires.
///
/// The shadowing is the thing worth having written down: it is one constraint hidden behind a
/// stricter one, which is the shape the plan hunts, and it dissolves the moment either number
/// moves after M4.
#[test]
fn the_refill_filter_binds_at_a_low_reserve_and_is_shadowed_at_the_gate() {
    let policy = AbrPolicy::measured();
    let catalog = hd_catalog();
    let production = ProductionEstimate::default();
    let current = catalog.candidate(Rung::P480);

    // Empty reserve: `D = buffer_target_ms`, so the budget is cut to H/(H+B*) of C_safe.
    let empty = catalog
        .best_sustainable(20_000, &production, current, &policy, 0)
        .expect("something is always affordable at 20 Mbit/s");
    let full = catalog
        .best_sustainable(20_000, &production, current, &policy, 30_000)
        .expect("ditto");
    assert!(
        empty.expected_wire_kbps < full.expected_wire_kbps,
        "the filter must cost something at an empty reserve: {} vs {}",
        empty.expected_wire_kbps,
        full.expected_wire_kbps,
    );
    let cut = i64::from(20_000u32) * policy.buffer_refill_horizon_ms
        / (policy.buffer_refill_horizon_ms + policy.buffer_target_ms);
    assert_eq!(cut, 16_000, "H/(H+B*) of 20 000 kbps is 0.8 — derived, not chosen");
    assert!(i64::from(empty.expected_wire_kbps) <= cut);

    // ...and the shadow: the reserve gate is above `buffer_target_ms` at every rung, so any
    // reserve that clears the gate also zeroes the deficit.
    let segment = 2_000i64;
    for rung in LADDER {
        let wire = HlsActuatorCatalog::measured().candidate(rung).expected_wire_kbps;
        let reachable = plant::b_max_est_ms(
            wire.saturating_sub(policy.assumed_audio_kbps),
            policy.assumed_audio_kbps,
        ) * i64::from(policy.buffer_reserve_fraction_pm)
            / 1_000;
        let gate = (segment * 3).min(reachable);
        assert!(
            gate >= policy.buffer_target_ms,
            "at {}kbps the upshift gate is {gate}ms, BELOW the refill target {}ms — the filter is \
             no longer shadowed and its effect on the decision path needs its own test",
            rung.kbps(),
            policy.buffer_target_ms,
        );
    }
}

/// DEVICE-FINDING REGRESSION, R2: **no reserve gate may demand more than the byte caps can hold.**
///
/// The upshift gate was a flat `3 * segment` = 6 000 ms while `B_max` at the top of the ladder is
/// 5 852 ms — so it was unsatisfiable at exactly the rungs it guarded, whatever the link did. That
/// is the control-law half of R2; Phase 0 fixed the plant half. Derived now as
/// `min(3*segment, alpha * B_max_est(R_target))`.
///
/// Differential: against the flat constant the top three rungs fail.
#[test]
fn no_reserve_gate_asks_for_more_than_the_queues_can_hold() {
    let policy = AbrPolicy::measured();
    let segment = 2_000i64;
    for rung in LADDER {
        let wire = HlsActuatorCatalog::measured().candidate(rung).expected_wire_kbps;
        let ceiling = plant::b_max_est_ms(
            wire.saturating_sub(policy.assumed_audio_kbps),
            policy.assumed_audio_kbps,
        );
        let gate = (segment * 3).min(ceiling * i64::from(policy.buffer_reserve_fraction_pm) / 1_000);
        assert!(
            gate < ceiling,
            "at {}kbps the gate wants {gate}ms of a reserve that tops out at {ceiling}ms",
            rung.kbps(),
        );
        assert!(
            policy.buffer_target_ms <= ceiling && policy.emergency_buffer_ms < ceiling,
            "at {}kbps B* ({}ms) or the emergency floor ({}ms) exceeds the ceiling {ceiling}ms",
            rung.kbps(),
            policy.buffer_target_ms,
            policy.emergency_buffer_ms,
        );
    }
    // The loosening is confined to the top, which is what `min` is for: at the bottom of the
    // ladder the ceiling term is tens of seconds and the constant still binds, unchanged.
    let bottom = HlsActuatorCatalog::measured().candidate(Rung::P240).expected_wire_kbps;
    let bottom_ceiling = plant::b_max_est_ms(
        bottom.saturating_sub(policy.assumed_audio_kbps),
        policy.assumed_audio_kbps,
    ) * i64::from(policy.buffer_reserve_fraction_pm)
        / 1_000;
    assert!(bottom_ceiling > segment * 3, "the constant must still bind at the floor rung");
}



/// **A published comparison must be readable as a decision: the winner out-totals the loser.**
///
/// `choose_mode` picks the larger total, so this holds by construction in the code — which is
/// exactly why nothing checked it, and why both `abr: mode` specimens in `docs/adaptive-playback.md`
/// were IMPOSSIBLE lines for as long as they existed. Each printed `chose=Hls` with the loser
/// out-totalling the winner, and one gave the HLS side `f=8` where `hls_utility` hardcodes
/// `features: 0`. A hand-written specimen of a machine-generated line has nothing holding it to the
/// format, so this test emits the real thing and grades it; the doc's specimens were regenerated
/// from this test's own output.
///
/// It also pins the two fields most easily transcribed wrong: `vs_hls=` is the rung's NOMINAL rate
/// (`rung.kbps()`), not its `expected_wire_kbps` — the docs said 20011 where the code prints 20000
/// — and the HLS side never carries a features term.
#[test]
fn a_published_comparison_is_readable_as_the_decision_it_records() {
    let current = hd_catalog().candidate(Rung::P720);
    let mut any = 0usize;
    for remaining_ms in [HOUR_MS, 600_000i64, 60_000, 20_000] {
        let mut gate = recovery(28_000);
        let _ = gate.observe_probe(
            probe(80_000, true), current, &idle_server(), healthy_buffer(), &healthy_hls(),
            remaining_ms,
        );
        let Some(cmp) = gate.comparison() else { continue };
        any += 1;
        let loser = cmp.loser.unwrap_or_default();
        assert!(
            cmp.winner.total >= loser.total,
            "at {remaining_ms} ms remaining the published winner totalled {} and the loser {} — a \
             line a reader cannot reconcile with `chose=`",
            cmp.winner.total,
            loser.total,
        );
        let hls_side = if cmp.chosen == ModeKind::Hls { cmp.winner } else { loser };
        assert_eq!(hls_side.features, 0, "the HLS side carries no features term");
        assert_eq!(
            cmp.hls_rung.kbps(),
            20_000,
            "`vs_hls=` is the rung's nominal rate, not its expected wire rate",
        );
    }
    assert!(any >= 3, "the fixture must reach a real comparison at most horizons, got {any}");
}

/// **The absorbing state: a downshift issued with an exhausted reserve can never complete, so the
/// exhausted reserve is permanent.** Device-measured 2026-08-28 on `pipe_abr_down_outrun` with
/// the abort rule armed — 321 aborts, every one logging `decision=prime_down`, every transaction
/// dying `outcome=warmup_deadline` with `warmup_dl=168ms`, 74 s of stall and the rung never
/// leaving 18000.
///
/// Differential in both directions, which is what makes it a test of the FLOOR rather than of the
/// reserve: with `NO_FLOOR` the same call returns the 168 ms the device measured, so the assertion
/// below cannot pass against the unmodified function; and the upshift leg pins that the floor is
/// scoped to `Down`, so it cannot be satisfied by simply raising the bound.
#[test]
fn a_downshift_gets_at_least_the_time_its_transfer_physically_needs() {
    let media = std::time::Duration::from_millis(2_000);
    let collapsed = reserve_as_budget(168);
    let down = Proposal { rung: Rung::P720Low, direction: Direction::Down };
    let up = Proposal { rung: Rung::P720Low, direction: Direction::Up };

    // 2000 kbps of output over 2 s of media, on a link measured at 6 000 kbps: 666 ms.
    let need = predicted_transfer(2_000, media, 6_000, 0);
    assert_eq!(need, std::time::Duration::from_millis(666));

    assert_eq!(
        candidate_warmup_budget(down, media, collapsed, NO_FLOOR),
        collapsed,
        "the pre-fix behaviour, and it is the deadline no transfer can meet",
    );
    assert_eq!(
        candidate_warmup_budget(down, media, collapsed, need),
        need,
        "a downshift out of an exhausted reserve gets the time its own transfer requires",
    );
    assert_eq!(
        candidate_warmup_budget(up, media, collapsed, need),
        collapsed,
        "and an upshift does NOT — once the reserve is gone an upshift has already lost",
    );
}

/// **The floor does not loosen the 36-second bound this function was written for.** In that record
/// — a 14000 -> 8000 downshift on a link measured at 9 593 kbps — the transfer's own requirement
/// is 1 667 ms, i.e. TIGHTER than the reserve that record ran against. The floor binds only where
/// the reserve has collapsed below what any transfer needs. (1 667 and not 1 668: the division
/// truncates, so the floor UNDERSTATES the requirement by under a millisecond — the conservative
/// direction for a bound whose job is to stop being smaller than a transfer.)
#[test]
fn the_floor_is_below_the_reserve_on_the_runaway_it_was_written_for() {
    let media = std::time::Duration::from_millis(2_000);
    let down = Proposal { rung: Rung::P1080, direction: Direction::Down };
    let need = predicted_transfer(8_000, media, 9_593, 0);
    assert_eq!(need, std::time::Duration::from_millis(1_667));
    for reserve_ms in [2_000i64, 5_000, 36_000] {
        let reserve = reserve_as_budget(reserve_ms);
        assert_eq!(
            candidate_warmup_budget(down, media, reserve, need),
            reserve,
            "at a {reserve_ms}ms reserve the floor must not be what decides",
        );
    }
}

/// **An unmeasured link changes no behaviour.** `ZERO` is the identity element of the `max`, so a
/// capacity of zero restores the reserve bound exactly rather than inheriting an invented one —
/// which matters because the first transaction of every playback is taken before any completed
/// segment has entered the estimate.
#[test]
fn an_unmeasured_link_predicts_nothing_and_the_reserve_still_bounds() {
    let media = std::time::Duration::from_millis(2_000);
    assert_eq!(predicted_transfer(8_000, media, 0, 500), std::time::Duration::ZERO);
    let down = Proposal { rung: Rung::P480, direction: Direction::Down };
    let reserve = reserve_as_budget(900);
    assert_eq!(
        candidate_warmup_budget(down, media, reserve, predicted_transfer(8_000, media, 0, 500)),
        reserve,
    );
}

/// **A deadline set to a central estimate is a coin flip, and the device landed it wrong 53 times
/// in a row.** The first floor granted exactly `R * D / C`; measured 2026-08-28 on
/// `pipe_abr_down_outrun`, `Down 18000->16000` came back `warmup_dl=1314ms decided=1327ms`,
/// missing by 13 ms, over and over, with the absorbing state back.
///
/// It was invisible in the first device leg because that one's targets were FAR below the current
/// rung — 8000, 720 and 320 out of 20000 — where the prediction is generous by a wide margin. It
/// appears only for a NEAR target, where prediction and reality meet, which is why this test
/// grades the ratio between the two regimes rather than either alone.
///
/// Differential: at `uncertainty_pm = 0` the widened value IS the central one, so the first
/// assertion below is what the pre-fix code returned and the second cannot pass against it.
#[test]
fn the_floor_carries_the_estimates_own_error_not_just_its_centre() {
    let media = std::time::Duration::from_millis(2_000);
    // The device's numbers: a 16000 kbps target on a link the estimator read at 24 353 kbps.
    let centre = predicted_transfer(16_000, media, 24_353, 0);
    assert_eq!(centre, std::time::Duration::from_millis(1_314), "the deadline that missed by 13ms");

    // `unc=500pm` is what the same log line published on those transactions.
    let widened = predicted_transfer(16_000, media, 24_353, 500);
    assert_eq!(widened, std::time::Duration::from_millis(1_971));
    assert!(
        widened > std::time::Duration::from_millis(1_327),
        "and it must clear the transfer that actually happened",
    );

    // A settled estimate buys almost nothing, which is what makes this the estimator's opinion
    // rather than a margin: the widening vanishes as `unc` does.
    assert_eq!(predicted_transfer(16_000, media, 24_353, 20), std::time::Duration::from_millis(1_340));
}

/// The widening is monotone in the estimator's stated error and in nothing else — the property
/// that makes it an uncertainty band rather than a tuning knob.
#[test]
fn the_floors_widening_is_monotone_in_uncertainty_alone() {
    let media = std::time::Duration::from_millis(2_000);
    let mut last = std::time::Duration::ZERO;
    for unc in [0u32, 50, 100, 200, 500, 1_000] {
        let d = predicted_transfer(8_000, media, 10_000, unc);
        assert!(d >= last, "unc {unc} produced {d:?} after {last:?}");
        last = d;
    }
    // ...and it never widens a prediction that does not exist.
    for unc in [0u32, 500, 1_000] {
        assert_eq!(predicted_transfer(8_000, media, 0, unc), std::time::Duration::ZERO);
    }
}

/// **An abandoned fetch must not set the budget its own abandonment disproves.**
///
/// Device-measured 2026-08-28 on `pipe_abr_down_outrun`: the shaper held the link at 500 kbps,
/// `ff::StallGuard` abandoned each fetch after ~1448 bytes, and those bytes timed at 42 277 kbps
/// because they are the receive buffer draining rather than the link. Entered as a COMPLETED
/// observation they held `conservative_kbps` near 16 Mbit/s, so every downshift the controller
/// correctly decided to make chose a target thirty times too dear, overran its deadline, aborted,
/// and decided again — 53 times on one rung pair, with the stall never ending.
///
/// Differential: the two legs differ ONLY in `abandoned()`, and the pre-fix code had no way to
/// express it — `observe` passed a hardcoded `completed: true`.
#[test]
fn an_abandoned_prefix_lowers_the_budget_instead_of_raising_it() {
    let settle = |mark_abandoned: bool| {
        let mut c = Controller::starting_at(Rung::P1080M18, None, hd_catalog());
        for _ in 0..6 {
            c.observe(sample(16_000, 400, 12_000), 0);
        }
        let before = c.delivery().conservative_kbps();
        // The device's own prefix: 1448 bytes in 274 us, which times at 42 Mbit/s. Repeated,
        // because REPETITION is the mechanism — the prefixes agree with each other, so the
        // estimator's dispersion term falls and it becomes CONFIDENT in a rate the link cannot
        // carry. The device log shows exactly that end state: `slow=48672kbps unc=500pm` while
        // the shaper held 500 kbps. Four samples do not get there; the failure needs the
        // convergence.
        for i in 0..24 {
            let prefix = sample_bytes(1_448, 274, 400, 168);
            c.observe(
                if mark_abandoned { prefix.abandoned() } else { prefix },
                1_000 + i * 100,
            );
        }
        (before, c.delivery().conservative_kbps())
    };

    let (before_complete, after_complete) = settle(false);
    let (before_abandoned, after_abandoned) = settle(true);
    assert_eq!(before_complete, before_abandoned, "the two legs must start identical");
    assert!(
        after_abandoned < after_complete,
        "an abandoned prefix must not buy the budget a completed one would: \
         complete {before_complete}->{after_complete}, abandoned \
         {before_abandoned}->{after_abandoned}",
    );
}

/// The flag reaches the estimator's UNCERTAINTY, which is the mechanism — `completed: false` is
/// already wired to `MAX_UNCERTAINTY_PM` and `conservative_kbps` already treats uncertainty as a
/// discount. Nothing new was modelled; a call site stopped overriding what was.
#[test]
fn an_abandoned_sample_reports_maximum_uncertainty() {
    let mut c = Controller::starting_at(Rung::P720, None, hd_catalog());
    for _ in 0..6 {
        c.observe(sample(8_000, 400, 12_000), 0);
    }
    let settled = c.delivery().uncertainty_pm;
    c.observe(sample_bytes(1_448, 274, 400, 168).abandoned(), 1_000);
    assert!(
        c.delivery().uncertainty_pm > settled,
        "abandoned {} must exceed settled {settled}",
        c.delivery().uncertainty_pm,
    );
}

/// **An abandoned prefix must not RESTART the estimate at its own value.** The device's exact
/// walk, reproduced: with the shaper holding 500 kbps, successive aborts drove the estimate
/// 5 632 -> 28 744 -> 101 078 kbps, because a prefix four times the history trips
/// `is_regime_change` and that path resets to the new value with one sample's confidence.
///
/// Differential twice over: marking the sample incomplete alone does NOT fix it (the rate still
/// enters and still trips the regime change), which is why the assertion is on the estimate rather
/// than on the uncertainty.
#[test]
fn an_abandoned_prefix_cannot_restart_the_estimate_upward() {
    let mut c = CapacityEstimate::default();
    for _ in 0..6 {
        c.update(CapacityObservation {
            kbps: 5_600, bytes: 1_400_000, active_us: 2_000_000, completed: true,
        });
    }
    let settled = c.slow_kbps;
    assert!((5_000..=6_200).contains(&settled), "fixture must settle near the shaped rate: {settled}");

    // Three aborts, each timing far above the history — the receive buffer, not the link.
    for kbps in [26_691u32, 35_533, 101_078] {
        c.update(CapacityObservation {
            kbps, bytes: 1_448, active_us: 274, completed: false,
        });
    }
    assert_eq!(c.slow_kbps, settled, "an abandoned prefix may not move the estimate up at all");
    assert_eq!(c.uncertainty_pm, MAX_UNCERTAINTY_PM, "but it must say the estimate is now unsure");
}

/// ...and it may still move it DOWN, because a slow prefix is the abort's actual message and the
/// direction the evidence supports. Without this the rule would be a one-way ratchet that ignores
/// a genuinely collapsing link.
#[test]
fn an_abandoned_prefix_may_still_lower_the_estimate() {
    let mut c = CapacityEstimate::default();
    for _ in 0..6 {
        c.update(CapacityObservation {
            kbps: 20_000, bytes: 5_000_000, active_us: 2_000_000, completed: true,
        });
    }
    let settled = c.slow_kbps;
    c.update(CapacityObservation {
        kbps: 500, bytes: 125_000, active_us: 2_000_000, completed: false,
    });
    assert!(c.slow_kbps < settled, "a slow abandoned prefix is real evidence of a slow link");
}

/// **A `Stay` must say why, and until now the whole UP path did not.**
///
/// `HlsReason::LadderFloor`'s own doc makes this complaint about the DOWN path — "the line read
/// `decision=stay reason=None`, identical to a healthy segment" — and fixed it there. The up path
/// had FIVE silent exits, and the cost showed on device: `pipe_abr_seek_flat` sat at 2000 kbps
/// with `safe=12585kbps`, a 45-second reserve, `risk=0`, `starve=none`, `dwell=0ms`, `block=0kbps`
/// and `reason=None` on **100 of 102** steady lines. Every field a reader would consult said
/// healthy, and the one field that could have named the refusal was empty.
///
/// The exception is deliberate and is the dwell, which reports itself as `dwell=<n>ms` on the same
/// line; `HlsReason::RejectBackoff`'s doc records the argument ("a dwell that is holding returns
/// before any target is selected, so there is no rung to name").
///
/// Structural rather than a value echo: it sweeps states and asserts the INVARIANT, so a sixth
/// silent exit added later fails it without anyone remembering this test exists.
#[test]
fn a_stay_always_names_its_reason_unless_the_dwell_is_holding() {
    let mut checked = 0;
    for link in [400u32, 2_000, 6_000, 20_000, 60_000, 200_000] {
        for buffered in [500i64, 2_000, 8_000, 24_000, 45_000] {
            for ratio in [80u32, 400, 900, 1_400] {
                let mut c = Controller::starting_at(Rung::P720Low, None, hd_catalog());
                for i in 0u32..40 {
                    let s = sample(link, ratio, buffered);
                    let decision = c.observe(s, u64::from(i) * 2_000);
                    if decision != Decision::Stay {
                        continue;
                    }
                    // The two exits that report themselves on the same line — `dwell=<n>ms` and
                    // `pending=<n>kbps` — and so are excluded by the same argument
                    // `HlsReason::RejectBackoff`'s doc makes for the dwell.
                    if c.dwell_left_ms() > 0 || c.has_pending() {
                        continue;
                    }
                    checked += 1;
                    assert!(
                        c.last_reason().is_some(),
                        "silent Stay at link={link} buf={buffered} ratio={ratio} \
                         rung={:?} sample={i}",
                        c.current(),
                    );
                }
            }
        }
    }
    assert!(checked > 200, "the sweep must actually reach Stay decisions, got {checked}");
}

/// **Only a downshift warm-up carries the abort rule, and not at the ladder floor.**
///
/// Differential by construction: before `candidate_warmup_is_guarded` existed, every candidate
/// warm-up passed `None` for its guard, so there was no arrangement of inputs under which the
/// unmodified transport aborted one early. The device trace this replaces
/// (`pipe_abr_down_outrun`, 2026-08-28) shows what that cost: `tx Down 18000->2000
/// outcome=warmup_deadline decided=5948ms warmup_dl=5918ms buf_start=5918ms buf_decided=168ms
/// net=5798kbps` — the whole reserve spent proving a rung unaffordable that a projection off the
/// first measurable 250 ms already implied.
#[test]
fn only_a_downshift_off_the_floor_guards_its_warmup() {
    // The reason the picture is unprotected on the way down and protected on the way up: an
    // upshift's current rung is affordable by construction, a downshift's is the trigger.
    assert!(candidate_warmup_is_guarded(Proposal {
        rung: Rung::P720Low,
        direction: Direction::Down,
    }));
    assert!(!candidate_warmup_is_guarded(Proposal {
        rung: Rung::P720Low,
        direction: Direction::Up,
    }));

    // R12's terminal case, and the one place the rule must NOT arm: `below()` of the floor is the
    // floor, so an abort here re-fetches the same bytes forever.
    assert_eq!(Rung::P240.below(), Rung::P240, "P240 is expected to be the ladder floor");
    assert!(!candidate_warmup_is_guarded(Proposal {
        rung: Rung::P240,
        direction: Direction::Down,
    }));

    // Every non-floor rung guards its downshift — stated over the whole ladder rather than at one
    // sampled rung, so adding a rung cannot silently leave a hole.
    for rung in LADDER {
        assert_eq!(
            candidate_warmup_is_guarded(Proposal { rung, direction: Direction::Down }),
            !rung.at_floor(),
            "downshift guard at {rung:?} must follow the floor test alone",
        );
        assert!(
            !candidate_warmup_is_guarded(Proposal { rung, direction: Direction::Up }),
            "an upshift warm-up never guards: {rung:?}",
        );
    }
}

/// **A commit is a coordinate change, not a drain** (R10), and the device trace it comes from.
///
/// Differential by construction: before `BufferEstimate::rebase` the delta across a commit entered
/// the EWMA as a flow, so the assertion below (`!draining()` immediately after) could not hold for
/// any input where the ceiling shrank. The magnitudes are the measured ones —
/// `pipe_auto_original_slow_recover`, 2026-08-28, one `6000 -> 8000` commit taking the reserve
/// from 31043 ms to 13376 ms with `slope=-1459ms/s`, still `-82` at the end of the run against a
/// `DRAIN_EPS_MS_PER_S` of 50.
#[test]
fn a_commit_does_not_enter_the_ceiling_drop_as_a_drain() {
    let mut buffer = BufferEstimate::default();
    // Settle a genuinely FILLING reserve at the old rung, the way the device did (+1175 ms/s).
    for ms in [21_418, 23_210, 25_043, 27_000, 29_000, 31_043] {
        buffer.update(Some(ms), 2_000);
    }
    assert!(!buffer.draining(), "a filling reserve must not read as draining");
    assert!(buffer.slope_ms_per_s > 0, "the setup itself must be a fill: {buffer:?}");

    // The commit. `B_max` shrinks with the rung, so the SAME queue now measures 13376 ms.
    buffer.rebase();
    buffer.update(Some(13_376), 2_000);

    assert!(
        !buffer.draining(),
        "the ceiling dropping is not the reserve draining — 13.4 s remained: {buffer:?}",
    );
    assert_eq!(
        buffer.slope_ms_per_s, 0,
        "with one observation in the new coordinates the rate of change is UNKNOWN, and 0 is how \
         this type says so",
    );

    // ...and a real drain in the new coordinates is still caught, which is the half a naive
    // "ignore everything after a commit" fix would have broken.
    for ms in [11_000, 8_500, 6_000, 3_500] {
        buffer.update(Some(ms), 2_000);
    }
    assert!(buffer.draining(), "a genuine post-commit drain must still fire: {buffer:?}");
}

/// **The WIRING, not the mechanism** — `Controller::commit` is what must rebase.
///
/// Differential, and it exists because its neighbour above is not. `a_commit_does_not_enter_the_
/// ceiling_drop_as_a_drain` proves `BufferEstimate` forgets its slope when TOLD to: it calls
/// `rebase()` in its own body, so deleting the call in `Controller::commit` leaves it green while
/// the device symptom returns verbatim (`slope=-1459ms/s` on a filling reserve -> `draining()`
/// stuck true -> `probe_due` resetting its spacing every sample -> Original never recovered).
///
/// This asks the controller instead: drive a genuinely FILLING reserve to a real commit and read
/// the slope on the far side. Under unmodified code the commit is transparent to the estimator, so
/// the positive slope asserted one line earlier survives it and the assertion cannot pass.
#[test]
fn the_controller_rebases_its_reserve_when_it_commits() {
    const LINK_KBPS: u32 = 40_000;
    let mut controller = bootstrap_controller();
    // A reserve filling at a steady +400 ms/s, driven rather than modelled: what is under test is
    // the controller's response to a commit, and a plant model here would make the input a
    // function of the very decision being graded.
    for step in 0..40i64 {
        let buf_ms = 8_000 + step * 800;
        let Decision::Prime(proposal) =
            controller.observe_next(sample(LINK_KBPS, 400, buf_ms))
        else {
            continue;
        };
        let candidate = sample(LINK_KBPS, 400, buf_ms);
        if !controller.candidate_ready(proposal, candidate, declared_bps(proposal.rung)) {
            controller.reject(proposal, RejectCause::Candidate, controller.clock_ms());
            continue;
        }
        assert!(
            controller.buffer().slope_ms_per_s > 0,
            "the setup must reach the commit on a FILLING reserve, or this grades nothing: {:?}",
            controller.buffer(),
        );
        assert!(controller.commit(proposal, controller.clock_ms()));
        assert_eq!(
            controller.buffer().slope_ms_per_s,
            0,
            "committing moved `B_max`, so the next reading is in new coordinates and the rate of \
             change is UNKNOWN — `Controller::commit` owes `BufferEstimate::rebase`: {:?}",
            controller.buffer(),
        );
        assert!(!controller.buffer().draining(), "and a rebased reserve is not draining");
        return;
    }
    panic!("no rung transaction was proposed and accepted — the fixture no longer exercises a commit");
}

/// **A transcode may not out-score the master it was made from** (R5, §7.B).
///
/// Differential: before the source clamp in `hls_utility`, the HLS side read the rung's nominal
/// wire rate with no source input at all, so a rendition requested above the source's own rate
/// scored a strictly higher band than the source — for ANY source. There is no input under which
/// the unmodified code satisfies the first assertion below.
///
/// The magnitudes are R5's and the ones measured on the host: an 8000 kbps master scores 58
/// (`7001..=9000`), an 18000 kbps rendition of it scored 76 (the open band), and 58 -> 66 -> 72 ->
/// 76 is the "three steps" R5 named.
#[test]
fn a_rendition_cannot_score_above_the_master_it_encodes() {
    let policy = AbrPolicy::measured();
    let catalog = HlsActuatorCatalog::measured();
    let rich = catalog.candidate(Rung::P1080M18);
    let source_kbps = 8_000;

    // The curve itself is unchanged and still says what it always said about a bare rate.
    assert_eq!(quality_score_at_kbps(rich.expected_wire_kbps), 76, "the ladder's band for 18000");
    assert_eq!(quality_score_at_kbps(source_kbps), 58, "the band an 8000 kbps master falls in");

    // **Asked through the PRODUCTION path.** `hls_utility` is what `choose_mode` argmaxes over, so
    // that is what has to be interrogated. This test used to re-implement the cap in its own body
    // and assert the arithmetic, which stayed green with the production line deleted -- the shape
    // the house rule on differential tests exists to forbid.
    //
    // Scoring the SAME candidate against an 8000 kbps source and against an unknown one must give
    // different quality. Under unmodified code the source was not an input on this side at all, so
    // both calls returned the 18000 band and the inequality cannot hold.
    let quality_against = |src: u32| {
        hls_utility(rich, rich, &ModeInputs { source_kbps: src, ..mode_inputs() }, &policy).quality
    };
    assert!(
        quality_against(source_kbps) < quality_against(0),
        "an 18000 kbps rendition of an 8000 kbps master must not score as an 18000 kbps picture: \
         {} against a known source vs {} against an unknown one",
        quality_against(source_kbps),
        quality_against(0),
    );
    assert_eq!(
        quality_against(source_kbps),
        hls_utility(
            HlsCandidate { expected_wire_kbps: source_kbps, ..rich },
            rich,
            &ModeInputs { source_kbps, ..mode_inputs() },
            &policy,
        )
        .quality,
        "a transcode of an 8000 kbps master is worth an 8000 kbps picture, not an 18000 one",
    );

    // The cap must NOT bite when the rung is genuinely under the source -- the ordinary case, where
    // capping would flatten the whole ladder into one band.
    let modest = catalog.candidate(Rung::P720Low);
    assert!(modest.expected_wire_kbps < source_kbps, "fixture assumption");
    assert_eq!(
        hls_scoring_kbps(modest, source_kbps),
        modest.expected_wire_kbps,
        "a rung below the source is scored exactly as before",
    );
    // ...and an unknown source (`source_kbps == 0`) must not cap to nothing, which would score
    // every rung 0 and refuse every upshift.
    assert_eq!(
        hls_scoring_kbps(rich, 0),
        rich.expected_wire_kbps,
        "unknown source leaves the rate alone",
    );
}

/// **The probe's reserve requirement is the probe's own budget, not a count of segments.**
///
/// Differential: `deep_reserve` read `buffered_ms >= 3 * segment`, so the requirement moved with
/// the segment duration while the cost it guards — `probe_budget_ms` of wall time — does not. The
/// third assertion below is the one no arrangement of inputs could satisfy before: at a 1 s segment
/// the old form demanded 3000 ms of reserve for a 4000 ms probe, i.e. it permitted exactly the
/// starvation the gate exists to prevent.
#[test]
fn a_probe_needs_a_reserve_that_outlasts_the_probe() {
    let policy = AbrPolicy::measured();
    assert_eq!(
        policy.probe_budget_ms, PROBE_BUDGET_MS,
        "the policy default and the shared constant are one number",
    );

    // The requirement no longer depends on how the server happens to cut segments...
    for segment_ms in [1_000u32, 2_000, 4_000, 6_000] {
        let old_form = i64::from(segment_ms) * 3;
        let new_form = i64::try_from(policy.probe_budget_ms).unwrap();
        assert_eq!(
            new_form, 4_000,
            "the requirement is the probe budget at every segment duration ({segment_ms} ms)",
        );
        // ...and the two forms genuinely disagree, in both directions, which is why this matters.
        if segment_ms == 2_000 {
            assert!(old_form > new_form, "at 2 s the old form was over-strict by 1.5x");
        }
        if segment_ms == 1_000 {
            assert!(
                old_form < new_form,
                "at 1 s the old form demanded {old_form} ms of reserve for a {new_form} ms probe — \
                 SHORTER than the thing it guards, which is the direction that matters",
            );
        }
    }
}
