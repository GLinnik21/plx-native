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
    sample_bytes_with_total(bytes, active_us, total_us, media_ms, buffered_ms)
}

/// [`sample_bytes`] with the exact acquisition wall time named. Device diagnostics round
/// production to per-mille, so reconstructing a 2.039 s acquisition from the displayed `1019`
/// would silently turn it into 2.038 s and stop being the trace the test claims to preserve.
fn sample_bytes_with_total(
    bytes: u64,
    active_us: u64,
    total_us: u64,
    media_ms: u32,
    buffered_ms: i64,
) -> SegmentSample {
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

#[test]
fn refreshing_a_decision_snapshot_preserves_the_completed_acquisition() {
    let completed = sample_bytes_with_total(777_000, 321_000, 654_000, 2_000, 4_000);
    let decision_buffer = BufferSnapshot {
        playback: MediaTimeMs(13_500),
        video_tail: MediaTimeMs(14_000),
        audio_tail: Some(MediaTimeMs(14_000)),
        audio_expected: true,
    };
    let refreshed = completed.with_buffer(decision_buffer);

    assert_eq!(refreshed.bytes(), completed.bytes());
    assert_eq!(refreshed.active_fetch_us(), completed.active_fetch_us());
    assert_eq!(refreshed.total_fetch_us(), completed.total_fetch_us());
    assert_eq!(refreshed.media_duration_ms(), completed.media_duration_ms());
    assert_eq!(refreshed.completed(), completed.completed());
    assert_eq!(refreshed.buffer, decision_buffer);
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
/// worst case**. Nothing on that television resembles 75%. The live conservation rule reads total
/// acquisition directly, while the active/fixed split still feeds delivery telemetry and the
/// downshift warm-up floor; a fixture with a fabricated 75% fixed share would grade those paths
/// against a plant that cannot exist.
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

/// A fresh encoder's first object contains the one-time PMS/session startup.  The device trace
/// took 3.013 s to obtain 2 s of 4K media while its body transferred at 35 Mbit/s; deciding on that
/// boundary object alone sent 22 Mbps straight to 320 kbps after every seek.  The next object is
/// the first repeatable operating-point observation, while the boundary bytes still seed delivery,
/// production and the prime reserve.
#[test]
fn a_reload_primes_before_it_rejudges_the_carried_rung() {
    let cold = sample(35_194, 1_506, 2_000); // the photographed A=3.012s, D=2s, B=2s shape

    let mut ordinary = Controller::starting_at(Rung::Uhd, None, uhd_catalog());
    assert!(
        matches!(
            ordinary.observe(cold, 3_013),
            Decision::Prime(Proposal {
                direction: Direction::Down,
                ..
            })
        ),
        "control: without the boundary declaration the finite one-object bag is losing",
    );

    let mut reloaded = Controller::starting_at(Rung::Uhd, None, uhd_catalog());
    assert_eq!(
        reloaded.observe_session_boundary(cold, Some(cold.media_duration_ms()), 3_013),
        Decision::Stay,
        "setup cost is buffered, not misclassified as a link collapse",
    );
    assert_eq!(reloaded.current(), Rung::Uhd);
    assert!(!reloaded.has_pending());

    let steady = sample(20_000, 300, 4_000);
    assert_eq!(reloaded.observe(steady, 3_613), Decision::Stay);
    assert_eq!(
        reloaded.current(),
        Rung::Uhd,
        "the carried rung survives a healthy post-seek object"
    );
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
/// [`WINDOW_CAPACITY`] is only a generous finite test budget, not a live sample threshold. A
/// sustainable one-point bag with spendable reserve may propose immediately; the separate
/// candidate transaction is what identifies whether the higher operating point can commit.
fn prime_up(controller: &mut Controller) -> Proposal {
    for _ in 0..WINDOW_CAPACITY {
        if let Decision::Prime(proposal) = controller.observe_next(sample(20_000, 200, 10_000)) {
            return proposal;
        }
    }
    panic!("no proposal")
}

/// Feed setup evidence for a test whose subject is the DOWN path without leaving an unrelated
/// exploratory upshift in flight. A circumstantial refusal says only that this fixture is not
/// exercising the transaction; it arms no candidate block and preserves every observation.
fn observe_without_upshift(controller: &mut Controller, sample: SegmentSample) -> Decision {
    let decision = controller.observe_next(sample);
    if let Decision::Prime(proposal) = decision {
        if proposal.direction == Direction::Up {
            assert!(controller.reject(proposal, RejectCause::Circumstance, controller.clock_ms(),));
            return Decision::Stay;
        }
    }
    decision
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
    // The 17.5 -> 18 Mbit/s edge drains only 2.8% of real time; 80 two-second samples leave the
    // initial reserve largely intact and therefore do not establish a long-run settling point.
    // Run past that physical runway so an unsustainable rung cannot pass as steady merely because
    // the fixture started buffered.
    for _ in 0..400 {
        let rung_kbps = i64::from(controller.current().kbps());
        let fetch_ms = SEGMENT_MS * rung_kbps / i64::from(network_kbps.max(1));
        buf_ms = (buf_ms - fetch_ms + SEGMENT_MS).clamp(0, CEILING_MS);
        let ratio_pm =
            u32::try_from(rung_kbps.saturating_mul(1_000) / i64::from(network_kbps.max(1)))
                .unwrap_or(u32::MAX)
                .max(1);
        if let Decision::Prime(proposal) =
            controller.observe_next(sample(network_kbps, ratio_pm, buf_ms))
        {
            let candidate_ratio_pm =
                proposal.rung.kbps().saturating_mul(1_000) / network_kbps.max(1);
            let candidate = sample(
                network_kbps,
                candidate_ratio_pm.max(1),
                buf_ms.max(SEGMENT_MS),
            );
            if controller.candidate_ready(proposal, candidate, declared_bps(proposal.rung)) {
                controller.commit(proposal, controller.clock_ms());
                controller.commit_candidate_evidence(candidate);
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
    assert_eq!(
        decision,
        Decision::Stay,
        "there is no lower rung to propose"
    );
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
        observe_without_upshift(&mut c, sample(20_000, 400, 12_000));
    }
    let decision = c.observe_next(quiet(12_000));
    assert_eq!(
        decision,
        Decision::Stay,
        "a reserve that cannot be READ is not a reserve that is EMPTY"
    );
    assert!(
        c.pending().is_none(),
        "nothing may be proposed on an unknowable reserve"
    );
}

/// The other half: the reserve genuinely being short still fires. Without this the test above is
/// satisfied by a controller that never downshifts at all.
#[test]
fn a_short_reserve_still_fires_a_downshift_when_the_lane_is_speaking() {
    let mut c = controller_at(Rung::P1080);
    for _ in 0..2 {
        observe_without_upshift(&mut c, sample(20_000, 400, 12_000));
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
    let decision = controller.observe_next(sample(40_000, 200, 1_958));
    assert!(
        !matches!(
            decision,
            Decision::Prime(Proposal {
                direction: Direction::Down,
                ..
            })
        ),
        "one cold-start segment of reserve on a fast link is not a failing steady state: \
         {decision:?}",
    );
}

/// Resume resets positional estimates, but elapsed wall time does not change the completed
/// acquisition certificate. A 400 ms acquisition with 1 958 ms of reserve is safe on the first
/// and every later sample; the old second-sample `B<D` trigger invented a failure here.
#[test]
fn the_first_segment_after_resume_cannot_false_downshift() {
    let mut controller = bootstrap_controller();
    observe_without_upshift(&mut controller, sample(40_000, 200, 12_000));
    controller.on_resume(30_000);

    let first = controller.observe_next(sample(40_000, 200, 1_958));
    assert!(
        !matches!(first, Decision::Prime(Proposal { direction: Direction::Down, .. })),
        "the first post-resume sample may excite a candidate but may not false-downshift: {first:?}",
    );
    if let Decision::Prime(proposal) = first {
        assert!(controller.reject(proposal, RejectCause::Circumstance, controller.clock_ms(),));
    }
    let second = controller.observe_next(sample(40_000, 200, 1_958));
    assert!(
        !matches!(
            second,
            Decision::Prime(Proposal {
                direction: Direction::Down,
                ..
            })
        ),
        "wall time and sample count cannot turn B=1958ms above R_o=400ms into failure: {second:?}",
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
    assert_eq!(
        buffer.buffered_ms, after_two.buffered_ms,
        "the level must not move"
    );
    assert_eq!(
        buffer.slope_ms_per_s, after_two.slope_ms_per_s,
        "the slope must not move"
    );
    assert_eq!(
        buffer.last_delta_ms, after_two.last_delta_ms,
        "no fabricated cliff"
    );
    assert_eq!(
        buffer.samples, after_two.samples,
        "an absence is not a sample"
    );
    assert_eq!(buffer.draining_samples, after_two.draining_samples);
}

/// A window shorter than the measurement window is not a measurement.
#[test]
fn original_windows_shorter_than_the_measurement_window_are_not_evidence() {
    let mut mode = original(28_000);
    assert!(mode
        .observe_saturated(
            window_bytes(4_000),
            ORIGINAL_WINDOW_US - 1,
            Some(3_000),
            HOUR_MS
        )
        .is_none());
    let first = mode
        .observe_saturated(
            window_bytes(4_000),
            ORIGINAL_WINDOW_US,
            Some(3_000),
            HOUR_MS,
        )
        .unwrap();
    assert_eq!(first.measured_kbps, 4_000);
    assert_eq!(
        first.requirement_kbps, 28_000,
        "the reported average is the measured consumption rate"
    );
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
        .observe_saturated(
            window_bytes(4_000),
            ORIGINAL_WINDOW_US,
            Some(8_000),
            HOUR_MS,
        )
        .unwrap();
    assert_eq!(
        cold.fallback, None,
        "the first window refines the estimators and decides nothing"
    );
    let observation = mode
        .observe_saturated(
            window_bytes(4_000) * 2,
            ORIGINAL_WINDOW_US * 2,
            Some(5_000),
            HOUR_MS,
        )
        .unwrap();
    assert_eq!(observation.fallback, Some(OriginalExit::ImminentStarvation));
    assert_eq!(
        observation.target,
        Some(Rung::P720Low),
        "3.2 Mbit/s of proven capacity"
    );
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
    assert_eq!(
        first.fallback, None,
        "window 1 may not abandon 4K direct play"
    );
    assert_eq!(
        first.unsafe_deficit_ms, 0,
        "nor may it count toward the sustained-deficit tally"
    );

    // …and the reserve then GROWS, exactly as the film's log showed (+113 ms/s). Nothing about
    // the next window is a deficit either.
    let second = mode
        .observe_saturated(
            window_bytes(42_365) * 2,
            ORIGINAL_WINDOW_US * 2,
            Some(1_200),
            HOUR_MS,
        )
        .unwrap();
    assert_eq!(
        second.fallback, None,
        "a filling reserve on a link that covers the file"
    );
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
        .observe_saturated(
            window_bytes(31_037) * 5,
            ORIGINAL_WINDOW_US * 5,
            Some(4_814),
            HOUR_MS,
        )
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
        settled.measured_kbps >= requirement,
        "the achieved rate physically carries the average"
    );
    assert!(
        settled
            .horizon_secs
            .is_none_or(|s| s > AbrPolicy::measured().starvation_fallback_secs),
        "a link delivering 1.23x the source may not read as imminent starvation — got {:?}s",
        settled.horizon_secs,
    );
    assert_eq!(settled.fallback, None);

    // And the rule still bites when the link really does fall behind: same reserve, a rate a
    // quarter of the source.
    let mut collapsed = original(source);
    collapsed
        .observe_saturated(
            window_bytes(6_000),
            ORIGINAL_WINDOW_US,
            Some(6_000),
            HOUR_MS,
        )
        .unwrap();
    let falling = collapsed
        .observe_saturated(
            window_bytes(6_000) * 2,
            ORIGINAL_WINDOW_US * 2,
            Some(3_000),
            HOUR_MS,
        )
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
        .observe_saturated(
            window_bytes(25_911) * 3,
            ORIGINAL_WINDOW_US * 3,
            Some(1_181),
            HOUR_MS,
        )
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
    mode.observe(
        window_bytes(31_037),
        ORIGINAL_WINDOW_US,
        Some(1_000),
        HOUR_MS,
        1_500,
    )
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

/// The first unsafe endpoint after a long queue-backpressure gap starts an interval; it cannot
/// classify the preceding, unobserved wall time as unsafe retroactively.
#[test]
fn the_first_unsafe_endpoint_after_backpressure_starts_at_zero_duration() {
    let mut mode = original(60_000);
    let bytes = window_bytes(50_000);
    mode.observe(bytes, ORIGINAL_WINDOW_US, Some(40_000), HOUR_MS, 750)
        .unwrap();
    mode.observe(
        bytes * 2,
        ORIGINAL_WINDOW_US * 2,
        Some(40_000),
        HOUR_MS,
        1_500,
    )
    .unwrap();

    let first = mode
        .observe(
            bytes * 3,
            ORIGINAL_WINDOW_US * 3,
            Some(5_000),
            HOUR_MS,
            61_500,
        )
        .unwrap();
    assert!(first.slope_ms_per_s < -DRAIN_EPS_MS_PER_S);
    assert!(first
        .horizon_secs
        .is_some_and(|s| s < AbrPolicy::measured().starvation_safe_secs));
    assert_eq!(
        first.unsafe_deficit_ms, 0,
        "the 60s gap was not observed unsafe"
    );

    let second = mode
        .observe(
            bytes * 4,
            ORIGINAL_WINDOW_US * 4,
            Some(4_500),
            HOUR_MS,
            62_250,
        )
        .unwrap();
    assert_eq!(
        second.unsafe_deficit_ms, 750,
        "only a known-unsafe interval accrues"
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
                Some(9_000 - 200 * i64::try_from(window).unwrap()),
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
    assert_eq!(
        collapsed.horizon_secs, None,
        "the delivery estimate still says it is fine"
    );
    assert_eq!(collapsed.fallback, Some(OriginalExit::EmergencyLowBuffer));
}

/// Nothing can starve a reserve that outlasts the content, which is also why the closing
/// minutes need no special case anywhere.
#[test]
fn a_reserve_that_covers_the_rest_of_the_film_never_falls_back() {
    let mut mode = original(28_000);
    let observation = mode
        .observe_saturated(
            window_bytes(1_000),
            ORIGINAL_WINDOW_US,
            Some(20_000),
            15_000,
        )
        .unwrap();
    assert!(
        observation.horizon_secs.unwrap_or(u32::MAX) < 60,
        "a real deficit"
    );
    assert!(
        observation.fallback.is_none(),
        "20 s buffered, 15 s left to play"
    );
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
    assert_eq!(
        mode.delivery, before,
        "a seek is not news about the network"
    );
    assert_eq!(mode.buffer, BufferEstimate::default());
    assert_eq!(mode.unsafe_deficit_ms, 0);
    // ...and the counters really did rewind, so the next window measures from zero rather than
    // reading a negative delta as a collapse.
    assert!(mode
        .observe_saturated(
            window_bytes(50_000),
            ORIGINAL_WINDOW_US,
            Some(1_000),
            HOUR_MS
        )
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
    assert_eq!(
        mode.delivery.slow_kbps, 50_000,
        "the VALUE survives; the confidence does not"
    );
    assert_eq!(mode.delivery.samples, 1);
}

#[test]
fn measured_runtime_fallback_avoids_an_unnecessarily_low_bootstrap() {
    let policy = AbrPolicy::measured();
    let catalog = hd_catalog();
    let rung = |measured| original_fallback_rung(measured, &hd_catalog(), &policy);
    assert_eq!(rung(512), Rung::P240);
    assert_eq!(rung(4_000), catalog.best_for_budget(4_000).unwrap().rung);
    assert_eq!(rung(7_000), catalog.best_for_budget(7_000).unwrap().rung);
    assert_eq!(
        rung(30_000),
        Rung::P1080High,
        "a fast link is not a reason to hold back"
    );
    assert_eq!(
        rung(1),
        Rung::P240,
        "below every candidate, take the floor rather than refusing to move",
    );
    assert_eq!(
        Rung::from_ceiling(Rung::P720Low.ceiling()),
        Some(Rung::P720Low)
    );
}

#[test]
fn realtime_current_acquisition_requires_a_candidate_measurement() {
    let mut controller = bootstrap_controller();
    let Decision::Prime(proposal) = controller.observe_next(sample(20_000, 1_000, 10_000)) else {
        panic!("real-time current acquisition says nothing about an untried encoder");
    };
    let too_slow = sample(20_000, 1_200, 10_000);
    assert!(!controller.candidate_ready(proposal, too_slow, declared_bps(proposal.rung)));
    assert!(controller.reject(proposal, RejectCause::Candidate, controller.clock_ms()));
    assert_eq!(controller.current(), Rung::P480);
}

/// Direct one-sample admission is exactly `A <= D && B_post >= A`. Requiring a whole media
/// duration of reserve when the completed acquisition cost less than one adds an unrelated gate
/// and contradicts the conservation boundary.
#[test]
fn a_fast_candidate_needs_its_acquisition_runway_not_a_whole_segment() {
    let mut controller = bootstrap_controller();
    let Decision::Prime(proposal) = controller.observe_next(sample(20_000, 500, 10_000)) else {
        panic!("the current sample must fund a candidate")
    };
    let candidate = sample(20_000, 500, 1_000); // A=1s, D=2s, B_post=1s
    assert!(controller.candidate_ready(proposal, candidate, declared_bps(proposal.rung),));
}

#[test]
fn a_proposal_does_not_mutate_current_until_candidate_commit() {
    let mut controller = bootstrap_controller();
    let proposal = prime_up(&mut controller);
    assert_eq!(proposal.direction, Direction::Up);
    assert!(proposal.rung > Rung::P480);
    assert_eq!(controller.current(), Rung::P480);
    assert_eq!(controller.pending(), Some(proposal));
    assert!(controller.candidate_ready(
        proposal,
        sample(20_000, 200, 12_000),
        declared_bps(proposal.rung),
    ));
    assert!(controller.commit(proposal, controller.clock_ms()));
    assert_eq!(controller.current(), proposal.rung);
}

#[test]
fn rejected_candidate_preserves_current_and_clears_pending() {
    let mut controller = bootstrap_controller();
    let proposal = prime_up(&mut controller);
    // **A candidate whose whole acquisition is slower than real time.** The old gate here was a bare
    // `800`, i.e. the single-observation form `A <= 0.8 D` the device corpus refutes at ~37%
    // violation; the disqualifier is now the dimensional identity `A<=D`. 1200 pm is a 2.4 s
    // acquisition for a 2 s segment. The old current-point window cannot override the candidate's
    // own completed operating-point result.
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
        assert_eq!(
            controller.observe_next(sample(20_000, 200, 12_000)),
            Decision::Stay
        );
    }
}

/// A low body rate on a small response is not a path-capacity measurement. If end-to-end
/// acquisition is 800 ms for 2 s of media, the current operating point is sustainable and must
/// not be evicted merely because `bytes/active` says 1 Mbit/s.
#[test]
fn a_single_slow_network_sample_jumps_to_the_measured_sustainable_rung() {
    let mut controller = bootstrap_controller();
    controller.current = Rung::P720;
    assert_eq!(
        observe_without_upshift(&mut controller, sample(20_000, 400, 8_000)),
        Decision::Stay,
    );
    let decision = controller.observe_next(sample(1_000, 400, 8_000));
    assert!(
        !matches!(
            decision,
            Decision::Prime(Proposal {
                direction: Direction::Down,
                ..
            })
        ),
        "a demand-capped body rate cannot choose a lower actuator: {decision:?}",
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
    // An exact current-point deficit from P720: A=2.4s for D=2s.
    let mut c = bootstrap_controller();
    c.current = Rung::P720;
    let Decision::Prime(down) = c.observe_next(sample(20_000, 1_200, 8_000)) else {
        panic!("an unsustainable acquisition bag must propose a downshift");
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
    assert_eq!(
        controller.observe_next(sample(40_000, 400, 8_000)),
        Decision::Stay
    );
    assert_eq!(
        controller.observe_next(sample(512, 5_000, 8_000)),
        Decision::Prime(Proposal {
            rung: Rung::P240,
            direction: Direction::Down
        }),
        "without a still-valid lower operating point, adjacent descent maximizes recovery latency"
    );
}

/// Server production time is already part of end-to-end acquisition `A`; it needs no second EWMA
/// threshold or reserve-slope heuristic. `A>D` is unsustainable even if a fixture hands it a
/// contradictory flat buffer, while `A<=D` is sustainable when the exact runway is covered.
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
    let mut behind = bootstrap_controller();
    assert!(matches!(
        behind.observe_next(sample(20_000, 1_200, 8_000)),
        Decision::Prime(Proposal {
            direction: Direction::Down,
            ..
        })
    ));
    assert_eq!(
        behind.telemetry().reason,
        Some(DecisionReason::Hls(HlsReason::UnsafeCurrentState)),
    );

    let mut realtime = bootstrap_controller();
    let decision = realtime.observe_next(sample(20_000, 1_000, 8_000));
    assert!(
        !matches!(
            decision,
            Decision::Prime(Proposal {
                direction: Direction::Down,
                ..
            })
        ),
        "A=D replenishes exactly what it spends: {decision:?}",
    );
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
    let top = HlsActuatorCatalog::measured()
        .candidate(Rung::P1080High)
        .expected_wire_kbps;
    CapacityEstimate::from_prior(top.saturating_mul(4))
}

fn healthy_buffer() -> BufferEstimate {
    BufferEstimate {
        buffered_ms: 12_000,
        slope_ms_per_s: 0,
        ..Default::default()
    }
}

fn probe(kbps: u32, completed: bool) -> CapacityObservation {
    CapacityObservation {
        kbps,
        bytes: 2_000_000,
        active_us: 400_000,
        completed,
    }
}

/// HLS first exercises every useful request size it can.  Only at the feasible ceiling does the
/// source request add information, and no arbitrary wall-clock delay is part of that fact.
#[test]
fn original_recovery_waits_for_the_hls_ceiling_not_a_timer() {
    let hls = CapacityEstimate::from_prior(30_000);
    let arguments = |current, frontier_exhausted, now_ms| {
        recovery(28_000).probe_due(
            current,
            frontier_exhausted,
            &idle_server(),
            sample(20_000, 500, 10_000),
            Some(1_000),
            healthy_buffer(),
            &hls,
            HOUR_MS,
            now_ms,
        )
    };
    assert_eq!(
        arguments(hd_catalog().candidate(Rung::P720), false, u64::MAX),
        Err(ProbeBlock::BelowHlsCeiling),
        "waiting cannot turn a demand-capped mid-ladder response into evidence about the link",
    );
    assert!(
        arguments(top_candidate(), true, 0).is_ok(),
        "once useful HLS traffic has reached its ceiling, the source experiment is informative immediately",
    );
}

/// A PMS request ceiling is not the delivered rendition.  The remote server can map every larger
/// request to the same smaller encode; those completed candidates are then structurally rejected
/// because they improve neither raster nor declared bandwidth.  Requiring the rejected actuator
/// to become `current` makes Original recovery unreachable even though HLS has no experiment left
/// and the direct-file request is a different, uncapped service path.
#[test]
fn an_exhausted_hls_frontier_allows_the_source_probe_below_the_top_rung() {
    let mut controller = Controller::starting_at(Rung::P720, None, hd_catalog());
    let observation = sample(20_000, 200, 20_000);

    // Exercise and structurally rule out every larger HLS actuator without ever committing one.
    // This is the controller state from the device trace: 20 Mbps was requested, PMS returned the
    // same ~1.1 Mbps picture, and the strict quality gate correctly kept the current stream.
    loop {
        match controller.observe_next(observation) {
            Decision::Prime(proposal) => {
                assert_eq!(proposal.direction, Direction::Up);
                assert!(controller.reject(
                    proposal,
                    RejectCause::Structural,
                    controller.clock_ms(),
                ));
            }
            Decision::Stay => break,
        }
    }
    assert_eq!(controller.current(), Rung::P720);
    assert_eq!(
        controller.last_reason(),
        Some(DecisionReason::Hls(HlsReason::RejectBackoff)),
        "the fixture must end with every higher HLS experiment classified",
    );

    let current = controller.catalog().candidate(controller.current());
    let hls = CapacityEstimate::from_prior(current.expected_wire_kbps);
    assert!(
        recovery(28_000)
            .probe_due(
                current,
                controller.hls_frontier_exhausted(),
                &idle_server(),
                observation,
                controller.prime_runway_ms(),
                controller.buffer(),
                &hls,
                HOUR_MS,
                controller.clock_ms(),
            )
            .is_ok(),
        "a rejected request must not have to become current before direct Original can be measured",
    );
}

/// A response that completed at exactly the requested HLS rate is a lower bound on service, never
/// proof that no unused service exists.  It therefore cannot veto the source experiment.
#[test]
fn a_demand_capped_hls_response_cannot_veto_the_source_experiment() {
    let current = top_candidate();
    let demand_capped = CapacityEstimate::from_prior(current.expected_wire_kbps);
    assert!(
        recovery(28_000)
            .probe_due(
                current,
                true,
                &idle_server(),
                sample(current.expected_wire_kbps, 500, 10_000),
                Some(1_000),
                healthy_buffer(),
                &demand_capped,
                HOUR_MS,
                0,
            )
            .is_ok(),
        "the HLS response measured its finite object, not the connection's unused tail",
    );
}

/// A thin or draining reserve cannot safely buy the source experiment, and a lower HLS rung still
/// has a useful, non-competing experiment of its own.
#[test]
fn original_recovery_refuses_to_probe_without_room_to_do_it_safely() {
    let current = hd_catalog().candidate(Rung::P1080High);
    let spare = CapacityEstimate::from_prior(60_000);
    assert_eq!(
        recovery(28_000).probe_due(
            current,
            true,
            &idle_server(),
            sample(60_000, 500, 2_000),
            Some(1_000),
            healthy_buffer(),
            &spare,
            HOUR_MS,
            0,
        ),
        Err(ProbeBlock::ShallowReserve),
    );
    assert_eq!(
        recovery(28_000).probe_due(
            current,
            true,
            &idle_server(),
            sample(60_000, 500, 10_000),
            Some(1_000),
            BufferEstimate {
                buffered_ms: 12_000,
                slope_ms_per_s: -400,
                ..Default::default()
            },
            &spare,
            HOUR_MS,
            0,
        ),
        Err(ProbeBlock::Draining),
    );
    assert_eq!(
        recovery(28_000).probe_due(
            hd_catalog().candidate(Rung::P720),
            false,
            &idle_server(),
            sample(60_000, 500, 10_000),
            Some(1_000),
            healthy_buffer(),
            &spare,
            HOUR_MS,
            0,
        ),
        Err(ProbeBlock::BelowHlsCeiling),
    );
}

/// A completed finite response is an exact lower bound on achieved service.  Repeating a lower
/// bound below demand cannot vote it upward, while one bound above demand needs no confidence tax.
#[test]
fn original_recovery_uses_the_completed_lower_bound_without_a_probe_count() {
    let mut short = recovery(28_000);
    for _ in 0..3 {
        assert_eq!(
            short.observe_probe(
                probe(27_999, true),
                top_candidate(),
                &idle_server(),
                healthy_buffer(),
                &healthy_hls(),
                HOUR_MS
            ),
            RecoveryVerdict::Insufficient,
        );
    }
    assert_eq!(
        recovery(28_000).observe_probe(
            probe(50_000, true),
            top_candidate(),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            HOUR_MS
        ),
        RecoveryVerdict::Recover,
        "one achieved lower bound above consumption is sufficient for the bounded trial",
    );
}

/// A terminal source verdict names one HLS counterfactual, not the rest of the playback. If an
/// upshift commits before the source reload can take a quiescent boundary, deleting the latch while
/// leaving `SourceProbeState::Terminal` makes Original unreachable forever: the gate refuses
/// another probe and the completed one is never consulted again.
///
/// The upward HLS commit therefore moves the gate to an explicit reconsideration state. One
/// ordinary observation at the new operating point reuses the exact completed source lower bound,
/// spends no request and increments no probe counter.
#[test]
fn a_terminal_original_verdict_is_reconsidered_after_an_hls_commit() {
    let mut gate = recovery(28_000);
    let measured = 80_000;
    assert_eq!(
        gate.observe_probe(
            probe(measured, true),
            hd_catalog().candidate(Rung::P480),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            HOUR_MS,
        ),
        RecoveryVerdict::Recover,
    );
    assert_eq!(gate.probes(), 1);
    assert!(gate.comparison().is_some());

    gate.on_hls_commit(Direction::Up);
    assert!(
        gate.comparison().is_none(),
        "the comparison against the superseded HLS operating point is no longer a decision",
    );
    let reconsidered = gate
        .reconsider_after_hls_commit(
            hd_catalog().candidate(Rung::P720),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            HOUR_MS,
        )
        .expect("the first ordinary sample after commit consumes the explicit state");
    assert_eq!(reconsidered.verdict, RecoveryVerdict::Recover);
    assert_eq!(reconsidered.measured_kbps, measured);
    assert_eq!(
        gate.probes(),
        1,
        "re-scoring is not a second source request"
    );
    assert!(gate.comparison().is_some());
    assert!(
        gate.reconsider_after_hls_commit(
            hd_catalog().candidate(Rung::P720),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            HOUR_MS,
        )
        .is_none(),
        "one state transition is consumed exactly once",
    );
}

/// A downshift is not merely a different HLS counterfactual: it is the committed consequence of
/// evidence that the previous service regime did not sustain its operating point. An earlier fast
/// source result must therefore not be replayed as though it had been measured after that collapse.
/// Retiring it is not an absorbing failure: the exact source-probe funding gate becomes Fresh and
/// may authorize a new bounded request once HLS has restored continuity.
#[test]
fn a_downshift_retires_a_terminal_source_rate_and_reopens_the_bounded_probe() {
    let mut gate = recovery(28_000);
    assert_eq!(
        gate.observe_probe(
            probe(80_000, true),
            top_candidate(),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            HOUR_MS,
        ),
        RecoveryVerdict::Recover,
    );

    gate.on_hls_commit(Direction::Down);
    assert!(gate.comparison().is_none());
    assert_eq!(
        gate.basis().2,
        0,
        "the historical source lower bound cannot cross the failed service-regime boundary",
    );
    assert!(
        gate.reconsider_after_hls_commit(
            hd_catalog().candidate(Rung::P720),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            HOUR_MS,
        )
        .is_none(),
        "Down requires a fresh source observation rather than reusing the historical rate",
    );
    assert!(
        gate.probe_due(
            hd_catalog().candidate(Rung::P720),
            true,
            &idle_server(),
            sample(60_000, 500, 10_000),
            Some(1_000),
            healthy_buffer(),
            &healthy_hls(),
            HOUR_MS,
            0,
        )
        .is_ok(),
        "the transition is Fresh, not a terminal no-new-evidence trap",
    );
    assert_eq!(
        gate.probes(),
        1,
        "arming a new probe has not performed it yet"
    );
}

/// Re-reading the same source prefix against the same HLS evidence asks the same question and can
/// only consume playback reserve.  A later probe becomes informative when live HLS has established
/// a strictly stronger lower bound than the evidence that existed at the previous experiment.  In
/// particular this is the router-release sequence from the device: a source probe under the cap is
/// insufficient, the cap is removed, then ordinary HLS traffic proves that the link regime changed.
#[test]
fn source_probe_rearms_only_after_stronger_live_link_evidence() {
    let current = top_candidate();
    let before = healthy_hls();
    let mut gate = recovery(28_000);
    let due = |gate: &mut OriginalRecovery, delivery: &CapacityEstimate| {
        gate.probe_due(
            current,
            true,
            &idle_server(),
            sample(20_000, 500, 10_000),
            Some(1_000),
            healthy_buffer(),
            delivery,
            HOUR_MS,
            0,
        )
    };

    assert!(due(&mut gate, &before).is_ok());
    assert_eq!(
        gate.observe_probe(
            probe(10_000, true),
            current,
            &idle_server(),
            healthy_buffer(),
            &before,
            HOUR_MS,
        ),
        RecoveryVerdict::Insufficient,
    );
    assert!(
        due(&mut gate, &before).is_err(),
        "unchanged demand-capped traffic cannot justify downloading the same source prefix again",
    );

    let recovered_link = CapacityEstimate {
        fast_kbps: before.fast_kbps.saturating_add(1),
        slow_kbps: before.fast_kbps.saturating_add(1),
        uncertainty_pm: 0,
        samples: before.samples.saturating_add(1),
    };
    assert!(
        due(&mut gate, &recovered_link).is_ok(),
        "a new conservative bound above the old central estimate is changed physical evidence",
    );
}

/// A truncated probe is an ABSENT measurement, not a slow link: folding its rate in would
/// poison the next decision with a number no transfer ever sustained.
#[test]
fn a_truncated_probe_is_absence_of_evidence() {
    let mut gate = recovery(28_000);
    assert_eq!(
        gate.observe_probe(
            probe(2_000, false),
            top_candidate(),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            HOUR_MS
        ),
        RecoveryVerdict::Insufficient,
    );
    assert_eq!(
        gate.observe_probe(
            probe(80_000, true),
            top_candidate(),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            HOUR_MS
        ),
        RecoveryVerdict::Recover,
        "the aborted attempt left no trace to drag the estimate down",
    );
}

/// A 5xx/transport failure before the response body is not a low throughput observation.  The
/// live HLS stream remains the best available route, but diagnostics must not report that the
/// source failed a bandwidth comparison no source byte ever entered.
#[test]
fn a_source_request_refused_before_body_is_probe_failure_not_insufficient() {
    let mut gate = recovery(28_000);
    let before = healthy_hls();
    assert_eq!(
        gate.observe_probe_failed(&before),
        RecoveryVerdict::ProbeFailed,
    );
    assert_eq!(
        gate.probes(),
        1,
        "the bounded source experiment was still spent"
    );
    assert_eq!(
        gate.basis().2,
        0,
        "an unavailable response contributes no delivery sample",
    );
    assert_eq!(
        gate.probe_due(
            top_candidate(),
            true,
            &idle_server(),
            sample(20_000, 500, 10_000),
            Some(1_000),
            healthy_buffer(),
            &before,
            HOUR_MS,
            0,
        ),
        Err(ProbeBlock::NoNewLinkEvidence),
        "a transient failure is not polled against unchanged evidence",
    );
    let changed = CapacityEstimate {
        fast_kbps: before.fast_kbps.saturating_add(1),
        slow_kbps: before.fast_kbps.saturating_add(1),
        uncertainty_pm: 0,
        samples: before.samples.saturating_add(1),
    };
    assert!(
        gate.probe_due(
            top_candidate(),
            true,
            &idle_server(),
            sample(20_000, 500, 10_000),
            Some(1_000),
            healthy_buffer(),
            &changed,
            HOUR_MS,
            0,
        )
        .is_ok(),
        "a failed request did not prove future unavailability after the link regime changed",
    );
}

/// The benefit of Original accrues over the remaining playback; the reload is paid once, now.
#[test]
fn recovery_does_not_pay_for_a_reload_at_the_end_of_a_film() {
    let mut gate = recovery(28_000);
    assert_eq!(
        gate.observe_probe(
            probe(80_000, true),
            top_candidate(),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            8_000
        ),
        RecoveryVerdict::NotWorthIt,
    );
    let current = top_candidate();
    let spare = CapacityEstimate::from_prior(80_000);
    assert_eq!(
        recovery(28_000).probe_due(
            current,
            true,
            &idle_server(),
            sample(20_000, 500, 10_000),
            Some(1_000),
            healthy_buffer(),
            &spare,
            8_000,
            0,
        ),
        Err(ProbeBlock::NotWorthIt),
        "and it does not spend the source experiment finding that out",
    );
}

/// After a forced downshift, recovery is released by observable surplus above the current
/// operating point's rollback runway. It must not wait for a full `n`-sample bag: `n` describes
/// historical evidence, not unused path capacity, and using it as an upshift gate recreates the
/// absorbing low-tier ceiling.
#[test]
fn a_downshift_recovers_from_finite_bag_surplus_before_the_old_window_fills() {
    let mut controller = controller_at(Rung::P1080High);
    observe_without_upshift(&mut controller, sample(40_000, 400, 8_000));
    let Decision::Prime(down) = controller.observe_next(sample(12_000, 1_500, 8_000).abandoned())
    else {
        panic!("the abandoned live request must propose a downshift")
    };
    assert!(controller.commit(down, controller.clock_ms()));

    let n = AbrPolicy::measured().admission.window_len();
    let mut recovered = None;
    for i in 0..n {
        let decision = controller.observe_next(sample(60_000, 400, 10_000));
        if let Decision::Prime(proposal) = decision {
            recovered = Some((proposal, controller.window_len(), i));
            break;
        }
    }
    let (proposal, window_at_proposal, _) = recovered.expect("surplus must fund a recovery trial");
    assert_eq!(
        proposal,
        Proposal {
            rung: Rung::P1080High,
            direction: Direction::Up
        },
        "and it must recover all the way, not one rung at a time",
    );
    assert!(
        window_at_proposal < n,
        "the proposal waited for all {n} samples instead of the finite-bag surplus",
    );
    assert_eq!(controller.telemetry().gates.dwell_ms, 0);
}

/// A successful excitation carries its own measured evidence. It must not also arm a wall-clock
/// dwell: the only fact that can finance another unknown request is newly observable reserve above
/// the committed operating point's rollback runway.
#[test]
fn a_commit_arms_no_unmeasured_wall_clock_dwell() {
    let mut c = controller_at(Rung::P720);
    let up = prime_up(&mut c);
    let candidate = sample(40_000, 400, 20_000);
    assert!(c.candidate_ready(up, candidate, declared_bps(up.rung)));
    assert!(c.commit(up, c.clock_ms()));
    c.commit_candidate_evidence(candidate);
    assert_eq!(c.telemetry().gates.dwell_ms, 0);
}

/// Elapsed wall time by itself is not new network evidence. After a candidate failure, waiting may
/// allow a different lower excitation, but it may not retry the identical failed request unless
/// the spendable reserve has strictly grown.
#[test]
fn wall_clock_alone_does_not_retry_the_identical_failed_candidate() {
    let mut c = controller_at(Rung::P720);
    let failed = prime_up(&mut c);
    let rejected_at = c.clock_ms();
    assert!(c.reject(failed, RejectCause::Candidate, rejected_at));
    let later = c.observe(sample(20_000, 200, 10_000), rejected_at + 60_000);
    assert!(
        !matches!(later, Decision::Prime(proposal) if proposal.rung == failed.rung),
        "time alone retried the identical failed request",
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

/// The compatibility transaction-cost readout must not resurrect a second time debt. The absolute
/// exploration deadline already bounds the transaction by the measured surplus; charging another
/// duration after it completes would double-count the same reserve.
#[test]
fn an_upshift_transaction_has_no_second_time_debt() {
    let policy = AbrPolicy::measured();
    for media_ms in [1, 2_000, 10_000, u64::MAX] {
        assert_eq!(
            crate::abr::viability::upshift_transaction_cost(
                std::time::Duration::from_millis(media_ms),
                &policy,
            ),
            std::time::Duration::ZERO,
        );
    }
}

/// A censored candidate is retryable only when the physical exploration budget grows. Elapsed
/// time is not repayment; additional playable reserve above the unchanged rollback runway is.
#[test]
fn a_failed_candidate_reopens_exploration_only_with_strictly_more_budget() {
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

    // The same evidence at the same instant cannot repeat THAT rung. A blocked top must not mask
    // lower unknown rungs, so those may still be proposed.
    for _ in 0..4 {
        let decision = c.observe(sample(20_000, 200, 10_000), clock_at_reject);
        assert!(
            !matches!(decision, Decision::Prime(proposal) if proposal.rung == refused.rung),
            "the identical physical budget bought the failed rung again: {decision:?}",
        );
        assert!(
            c.telemetry().gates.blocked_kbps > 0,
            "the guard must still be holding"
        );
        if let Decision::Prime(proposal) = decision {
            assert!(c.reject(proposal, RejectCause::Circumstance, c.clock_ms()));
        }
    }

    // A strictly deeper reserve is new spendable evidence and reopens quality exploration. The
    // failed actuator remains a scheduling endpoint: reopening affordability does not require
    // spending the next epsilon of reserve on the same largest transaction again.
    let decision = c.observe(sample(20_000, 200, 12_000), clock_at_reject + 1);
    let Decision::Prime(reopened) = decision else {
        panic!("new reserve above the same rollback runway must reopen one experiment");
    };
    assert_eq!(reopened.direction, Direction::Up);
    assert!(reopened.rung > c.current());
}

/// A deadline-censored transaction has not produced a candidate media quantum, so it cannot be
/// an ordinal response-size endpoint. The only fact it established is common to every quality
/// excitation: the serial PMS decision/start/playlist/body path did not finish in the disposable
/// reserve it was given.
///
/// Remote-PMS reproduction (2026-08-31): after a 22 Mbps candidate spent its deadline, the
/// old per-rung frontier immediately tried 12, 6 and 22 Mbps candidates on smaller common
/// budgets. Those overlapping encoder resources made PMS map later 22 Mbps requests to 720p and
/// then 404p. A different actuator is not new evidence and must not bypass the censored budget.
#[test]
fn a_censored_candidate_blocks_all_quality_exploration_until_the_common_budget_grows() {
    let mut controller = Controller::starting_at(Rung::P720Low, None, hd_catalog());

    // A=1s, D=2s, R_s=1s, so B=5s exposes exactly E=3s of disposable reserve.
    let first = match controller.observe_next(sample(20_000, 500, 5_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the initial common budget should fund one excitation"),
    };
    assert_eq!(first.rung, Rung::P1080High);
    assert!(controller.reject(first, RejectCause::Censored, controller.clock_ms()));

    // The next ordinary current-rung completion leaves E=2s after the exact rollback boundary.
    // The old scheduler treated the untouched lower rungs as eligible and immediately opened a
    // second encoder. But no candidate response completed, so actuator order supplies no fact
    // that can make a different request finish inside a *smaller* common budget.
    let next = controller.observe_next(sample(20_000, 500, 4_000));
    assert_eq!(next, Decision::Stay);
    assert_eq!(
        controller.last_reason(),
        Some(DecisionReason::Hls(HlsReason::RejectBackoff)),
    );
}

/// Reject release must be evaluated against the bag that INCLUDES the sample making the next
/// decision. Otherwise a deeper buffer can momentarily release the block, after which that same
/// sample raises the physical runway and the identical candidate is retried with less spendable
/// reserve than the failed attempt had.
#[test]
fn a_new_sample_cannot_release_a_candidate_before_its_runway_cost_is_counted() {
    let mut c = controller_at(Rung::P720);
    let Decision::Prime(failed) = c.observe(sample(20_000, 500, 10_000), 2_000) else {
        panic!("the initial 7s smooth surplus must fund a candidate")
    };
    assert_eq!(c.exploration_budget_ms(10_000), Some(8_000));
    assert!(c.reject(failed, RejectCause::Candidate, 2_000));

    // Before this 3s acquisition is inserted, B=11s and the old 1s runway appears to offer 9s
    // after preserving the larger 2s media horizon, enough to release an 8s failure. Once counted,
    // the sustainable two-sample bag needs a 3s replay boundary (already larger than D) and offers
    // only 8s. The failed top request must therefore remain excluded.
    let next = c.observe(sample(20_000, 1_500, 11_000), 4_000);
    assert_eq!(c.exploration_budget_ms(11_000), Some(8_000));
    assert!(
        !matches!(next, Decision::Prime(proposal) if proposal.rung == failed.rung),
        "the identical candidate was retried on a smaller physical budget: {next:?}",
    );
}

/// A discretionary experiment is followed by an ordinary current-rung acquisition when it
/// fails. Let `L=B-E` remain for its ordinary rollback acquisition. Before completion, surviving
/// every still-sustainable unseen response needs `L>=D`; afterwards `B'=L-A+D>=L`, so restoring
/// the stress boundary needs `L>=R_s`. The exact joint obligation is therefore
/// `L>=max(R_s,D)`, not `R_s+D`: the two conditions sit on opposite sides of the same media
/// credit. Neither term is a tuned safety margin.
#[test]
fn exploration_preserves_a_media_horizon_for_smooth_rollback() {
    let mut controller = controller_at(Rung::P720);
    let observation = sample(20_000, 500, 2_000); // A=1s, D=2s, R_s=1s.
    assert_eq!(controller.observe_next(observation), Decision::Stay);
    assert_eq!(
        controller.exploration_budget_ms(2_000),
        None,
        "B=max(R_s,D) has no discretionary millisecond after funding trial failure and rollback",
    );

    let decision = controller.observe_next(sample(20_000, 500, 2_001));
    assert!(matches!(decision, Decision::Prime(_)));
    assert_eq!(controller.exploration_budget_ms(2_001), Some(1));
}

/// The rollback obligation is the NEXT object on the still-live current cursor, not the object
/// which happened to complete before the decision. HLS permits variable EXTINF durations. Using
/// the previous one silently underfunds rollback after a short object followed by a longer one:
/// the next acquisition may still be perfectly sustainable (`A <= D`) while the retained balance
/// cannot survive until its media credit lands.
#[test]
fn exploration_preserves_the_known_next_variable_duration_object() {
    let mut controller = controller_at(Rung::P720);
    let short = sample_of(1_000, 20_000, 500, 2_000); // A=.5s, R_s=.5s.

    assert_eq!(
        controller.observe_with_rollback(short, Some(4_000), 1_000),
        Decision::Stay,
        "B=2s cannot fund an experiment while preserving the known next 4s rollback object",
    );
    assert_eq!(controller.exploration_budget_ms(2_000), None);

    let funded = sample_of(1_000, 20_000, 500, 4_001);
    assert!(matches!(
        controller.observe_with_rollback(funded, Some(4_000), 2_000),
        Decision::Prime(_),
    ));
    assert_eq!(controller.exploration_budget_ms(4_001), Some(1));
}

/// Proposal and execution read the reserve at different instants. Playback may consume enough in
/// between that a failure frontier released by the sample-time snapshot is blocked again at the
/// actual transaction budget. The worker's re-read is authoritative; otherwise a candidate can
/// be retried with LESS reserve than the same candidate already exhausted.
#[test]
fn an_exploration_rechecks_its_failure_frontier_at_the_executed_budget() {
    let mut controller = controller_at(Rung::P720);
    let failed = prime_up(&mut controller);
    assert!(controller.set_executed_exploration_budget(failed, 9_000));
    assert!(controller.reject(failed, RejectCause::Candidate, controller.clock_ms()));

    // Fresh completed service evidence is strong enough to support the old endpoint itself, so
    // the scheduler legitimately crosses its ordinal memory and proposes that exact actuator.
    let reproposed = match controller.observe_next(sample(100_000, 500, 12_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the sample-time 10s surplus should release the old 9s frontier"),
    };
    assert_eq!(reproposed, failed);
    assert!(
        !controller.set_executed_exploration_budget(reproposed, 8_000),
        "the worker re-read bought a 9s failure again with only 8s",
    );
    assert!(
        !controller.has_pending(),
        "a refused time-of-use authorization left a phantom transaction pending",
    );
}

/// The same proposal/execution race applies to the common censored frontier. A sample-time
/// snapshot may expose more reserve than the failed transaction spent, but playback can consume
/// that difference before the worker arms the next request. Untouched rungs must not launder the
/// smaller execution-time budget merely because the proposal was selected at a larger one.
#[test]
fn an_exploration_rechecks_the_common_censored_frontier_at_execution_time() {
    let mut controller = controller_at(Rung::P720);
    let failed = prime_up(&mut controller);
    assert!(controller.set_executed_exploration_budget(failed, 9_000));
    assert!(controller.reject(failed, RejectCause::Censored, controller.clock_ms()));

    let reproposed = match controller.observe_next(sample(20_000, 500, 12_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => {
            panic!("the sample-time 10s surplus should release the common 9s frontier")
        }
    };
    assert!(
        !controller.set_executed_exploration_budget(reproposed, 8_000),
        "the worker opened another encoder after its live budget fell below the common frontier",
    );
    assert!(
        !controller.has_pending(),
        "a refused common-budget authorization left a phantom transaction pending",
    );
    assert_eq!(
        controller.last_reason(),
        Some(DecisionReason::Hls(HlsReason::RejectBackoff)),
    );
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
fn an_end_to_end_acquisition_deficit_moves_without_eight_samples_of_agreement() {
    let mut c = controller_at(Rung::P1080);
    let Decision::Prime(proposal) = c.observe_next(sample(32_000, 1_001, 30_000)) else {
        panic!("A>D must move even with a deep starting reserve")
    };
    assert_eq!(proposal.direction, Direction::Down);
    assert_eq!(
        c.telemetry().reason,
        Some(DecisionReason::Hls(HlsReason::UnsafeCurrentState)),
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
        assert!(
            pair[0].kbps() < pair[1].kbps(),
            "{:?} then {:?}",
            pair[0],
            pair[1]
        );
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
    // 4% more wire, 110% more calibrated server work: a distinct recurring mode-utility cost,
    // not an independent HLS admission constraint.
    assert!(candidate.expected_wire_kbps < high.expected_wire_kbps * 11 / 10);
    assert_eq!(
        candidate.production_load_pm,
        high.production_load_pm * 21 / 10
    );
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
    assert_eq!(
        unmeasured.best_for_budget(huge_budget).map(|c| c.rung),
        Some(Rung::Uhd)
    );
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
    assert!(
        rungs.contains(&Rung::P720),
        "and the real downscale steps all survive"
    );
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
    assert_eq!(
        uhd_scope.best_for_budget(60_000).map(|c| c.rung),
        Some(Rung::Uhd)
    );
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
    assert_eq!(
        clamped.kbps,
        floor.expected_wire_kbps * 8,
        "a bounded claim, not a fantasy"
    );
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
    // second of end-to-end acquisition for a two-second segment — see the companion test below for why
    // the number is load-bearing here and was not before §4's rule decided.
    let mut controller = controller_at(Rung::P240);
    let mut reached = Rung::P240;
    for _ in 0..40 {
        let segment = sample_bytes(80_000, 700, 250, 12_000);
        if let Decision::Prime(proposal) = controller.observe_next(segment) {
            if controller.candidate_ready(
                proposal,
                sample(20_000, 400, 12_000),
                declared_bps(proposal.rung),
            ) {
                controller.commit(proposal, controller.clock_ms());
                reached = controller.current();
            } else {
                controller.reject(proposal, RejectCause::Candidate, controller.clock_ms());
            }
        }
    }
    assert!(
        reached > Rung::P240,
        "a LAN must not leave Auto on the emergency floor"
    );
}

/// A demand-capped floor response cannot identify the unused path tail. The two legs hold bytes,
/// total acquisition and reserve constant and vary only active body time. The retired transfer
/// projection treated the slow-body leg as proof that every larger request was impossible. The
/// live rule uses either leg only to price rollback runway, funds the same bounded experiment, and
/// accepts the independently completed fast candidate.
#[test]
fn a_slow_demand_capped_floor_response_cannot_veto_a_fast_candidate() {
    // `active_us` is the ONLY axis: both legs are 80 kB acquired over 800 ms at ratio_pm 400.
    fn reached_from_floor(active_us: u64) -> Rung {
        let mut controller = controller_at(Rung::P240);
        for _ in 0..40 {
            if let Decision::Prime(proposal) =
                controller.observe_next(sample_bytes(80_000, active_us, 400, 12_000))
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
    assert!(
        reached_from_floor(700) > Rung::P240,
        "0.7ms of body inside an 800ms acquisition: the link moved 80kB instantly and the rest was \
         open, probe and wait — a larger rendition costs more BODY and the same overhead, so the \
         floor is escapable, and refusing it asserts that a bigger file opens more slowly",
    );
    assert!(
        reached_from_floor(700_000) > Rung::P240,
        "a slow demand-capped floor response may price rollback runway, but it cannot veto the \
         independently completed fast candidate segment",
    );
}

/// `ProductionEstimate` observes total acquisition, including the network/open/TTFB already priced
/// elsewhere. Without an independent server clock it may remain telemetry, but cannot veto the same
/// service episode a second time. The candidate's own exact `A<=D` result remains authoritative.
#[test]
fn total_acquisition_is_not_a_second_independent_production_gate() {
    let catalog = uhd_catalog();
    let current = catalog.candidate(Rung::P1080High);
    let fast = CapacityEstimate::from_prior(80_000);
    let policy = AbrPolicy::measured();
    let buffer = BufferEstimate {
        buffered_ms: 20_000,
        ..Default::default()
    };

    let mut quick_server = ProductionEstimate::default();
    for _ in 0..4 {
        quick_server.observe(200, current.production_load_pm, false);
    }
    let quick = catalog
        .best_sustainable(60_000, &policy, 30_000)
        .map(|c| c.rung);
    assert_eq!(quick, Some(Rung::Uhd));

    let mut loaded_server = ProductionEstimate::default();
    for _ in 0..4 {
        loaded_server.observe(700, current.production_load_pm, false);
    }
    let loaded = catalog
        .best_sustainable(60_000, &policy, 30_000)
        .map(|c| c.rung);
    assert_eq!(
        loaded, quick,
        "total acquisition was charged as two independent constraints"
    );
    let quick_risk = candidate_risk(
        catalog.candidate(Rung::Uhd),
        current,
        &fast,
        &quick_server,
        &buffer,
        &policy,
    );
    let loaded_risk = candidate_risk(
        catalog.candidate(Rung::Uhd),
        current,
        &fast,
        &loaded_server,
        &buffer,
        &policy,
    );
    assert_eq!(loaded_risk.score, quick_risk.score);
}

/// A budget jump primes the far candidate ONCE, instead of paying for three encoder creations
/// to walk 10, 12, 14.
///
/// The current bag says only how much reserve can be spent. Since every unknown candidate is
/// bounded by that same surplus, the excitation is the highest feasible unclassified rung rather
/// than a walk through intermediate encoders.
#[test]
fn a_budget_jump_skips_the_intermediate_encoders() {
    let mut controller = controller_at(Rung::P1080);
    let mut proposal = None;
    for _ in 0..WINDOW_CAPACITY {
        if let Decision::Prime(p) = controller.observe_next(sample(22_000, 200, 12_000)) {
            proposal = Some(p);
            break;
        }
    }
    let proposal = proposal.expect("the conservation window carries a dearer rung");
    assert_eq!(proposal.direction, Direction::Up);
    assert!(
        proposal.rung > Rung::P1080M14,
        "the jump must skip the 10/12/14 encoder walk; got {:?}",
        proposal.rung,
    );
}

#[test]
fn only_upshift_primes_receive_the_exact_acceptance_budget() {
    let media = std::time::Duration::from_millis(2_002);
    let policy = AbrPolicy::measured();
    let up = Proposal {
        rung: Rung::P720Low,
        direction: Direction::Up,
    };
    let down = Proposal {
        rung: Rung::P240,
        direction: Direction::Down,
    };
    // A reserve far above either helper's clamp, so this grades their unclamped arithmetic. The
    // prime helper is historical; the warm-up helper remains the live initial-media calculation.
    let ample = reserve_as_budget(60_000);
    assert_eq!(candidate_prime_budget(media, &policy, ample), media);
    assert_eq!(
        candidate_warmup_budget(up, media, ample, NO_FLOOR, NO_FLOOR),
        ample,
    );
    // A downshift has no repeatable-observation acceptance phase. This helper returns its initial
    // reserve budget; the live seam may floor that at the measured acquisition requirement or
    // remove it for terminal floor recovery.
    assert_eq!(
        candidate_warmup_budget(down, media, ample, NO_FLOOR, NO_FLOOR),
        ample
    );
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
    let down = Proposal {
        rung: Rung::P240,
        direction: Direction::Down,
    };
    for reserve_ms in [0i64, 250, 5_000, 36_000] {
        let reserve = reserve_as_budget(reserve_ms);
        assert_eq!(
            candidate_warmup_budget(down, media, reserve, NO_FLOOR, NO_FLOOR),
            reserve,
            "a {reserve_ms}ms reserve buys exactly {reserve_ms}ms of transfer",
        );
    }
}

/// Compatibility arithmetic for the initial reserve cap in both directions. The
/// `candidate_prime_budget` assertions below preserve the archived unconditional-second-phase
/// helper; live setup-bearing continuation instead stays inside the original transaction grant.
#[test]
fn a_thin_reserve_bounds_an_upshift_too_and_an_ample_one_does_not() {
    let media = std::time::Duration::from_millis(2_000);
    let policy = AbrPolicy::measured();
    let up = Proposal {
        rung: Rung::P1080,
        direction: Direction::Up,
    };

    let healthy = reserve_as_budget(3 * 2_000);
    assert_eq!(
        candidate_warmup_budget(up, media, healthy, NO_FLOOR, NO_FLOOR),
        healthy,
        "the cold candidate may spend exactly the exploration reserve granted to the transaction",
    );
    assert_eq!(candidate_prime_budget(media, &policy, healthy), media,);

    let thin = reserve_as_budget(400);
    assert_eq!(
        candidate_warmup_budget(up, media, thin, NO_FLOOR, NO_FLOOR),
        thin
    );
    assert_eq!(candidate_prime_budget(media, &policy, thin), thin);
}

/// A reserve at or below zero buys no time at all. The deadline is then "now", which aborts the
/// fetch on its first check — correct, because a transaction starting with no reserve has already
/// stalled and every millisecond after that is a millisecond of stall.
#[test]
fn a_reserve_at_or_below_zero_is_a_zero_budget_and_never_a_wrapped_one() {
    for ms in [0i64, -1, -20_000, i64::MIN] {
        assert_eq!(
            reserve_as_budget(ms),
            std::time::Duration::ZERO,
            "at {ms}ms"
        );
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

/// A pause ends uninterrupted rung residency, but it is not new network evidence and cannot erase
/// a censored candidate result. Exploration has no wall-clock dwell: the identical request is
/// financed again only by strictly more observed reserve above the current runway.
#[test]
fn resume_clears_rung_residency_but_preserves_candidate_evidence() {
    let mut c = bootstrap_controller();
    let mut now = 0u64;
    for _ in 0..3 {
        now += 2_000;
        if let Decision::Prime(proposal) = c.observe(sample(40_000, 200, 20_000), now) {
            assert!(c.reject(proposal, RejectCause::Circumstance, now));
        }
    }
    assert!(
        c.telemetry().gates.on_rung > 0,
        "the setup must establish rung residency"
    );
    c.on_resume(30_000);
    assert_eq!(c.telemetry().gates.on_rung, 0);

    // A reject records the rung it refused; merely pausing cannot retire that record.
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
    assert_eq!(
        blocked.telemetry().gates.blocked_kbps,
        proposal.rung.kbps(),
        "unmeasured wall time is not a larger physical exploration budget",
    );
    assert_eq!(blocked.telemetry().gates.dwell_ms, 0);
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
        assert_eq!(
            estimate.fast_kbps, measured,
            "the fast estimate still takes the reading"
        );
    }
}

/// The other side of the same guard: a genuine collapse must still be detected.
#[test]
fn a_reading_a_quarter_of_the_prior_still_collapses() {
    let mut estimate = CapacityEstimate::from_prior(40_000);
    estimate.collapse(1_000);
    assert!(
        estimate.slow_kbps < 40_000,
        "a 40x drop is a collapse and must lower the slow prior"
    );
    assert_eq!(estimate.uncertainty_pm, 400);
}

/// **Archived regression: the former graded-segment deadline and acceptance test shared one
/// threshold.**
///
/// When the live transaction had an unconditional graded segment, both were 0.8·D. The §4
/// admission work replaced `candidate_ready`'s bare 800
/// with `production_max_pm`, and the transport's literal `4/5` was left behind — so for a while a
/// candidate whose graded segment took between 0.8·D and 1.1·D was aborted by the deadline and
/// never reached the rule that would have admitted it. One threshold, enforced twice at two
/// values, the stricter one invisible because it fired in `ff.rs`.
///
/// This compatibility test would have caught it and retains the coefficient-free `A <= D`
/// threshold for archived plant fixtures. The live conditional repeatable phase no longer calls
/// this helper.
#[test]
fn the_prime_deadline_is_exactly_the_acceptance_threshold() {
    let policy = AbrPolicy::measured();
    for media_ms in [1_000u64, 2_000, 4_000, 6_006] {
        let media = std::time::Duration::from_millis(media_ms);
        // An ample reserve, so the acceptance threshold is what this grades.
        let budget = candidate_prime_budget(media, &policy, reserve_as_budget(600_000));
        // What `candidate_ready` admits: total acquisition no slower than real time. 1000 pm is
        // the dimensional identity A <= D, not another policy coefficient.
        let admits_up_to_us = u128::from(media_ms) * 1_000;
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
/// It also reaches it in TWO moves rather than one: conservative delivery/refill evidence first
/// authorizes the finite experiment it can fund, then the candidate's own complete acquisition
/// decides whether it commits. Skipping intermediate encoders is bounded by what has actually
/// been observed.
#[test]
fn lg_network_legs_settle_on_sustainable_rungs() {
    let catalog = hd_catalog();
    for link in [512, 1_200, 7_000, 17_500] {
        assert_eq!(
            settle_link(link),
            catalog
                .best_for_budget(link)
                .unwrap_or_else(|| catalog.candidate(Rung::P240))
                .rung,
            "the flat {link}kbps plant should settle on its highest physically affordable rung",
        );
    }
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
    assert!(
        candidate_risk(
            current,
            current,
            &CapacityEstimate {
                slow_kbps: 59_000,
                fast_kbps: 59_000,
                uncertainty_pm: 0,
                samples: 8
            },
            &ProductionEstimate::default(),
            &BufferEstimate {
                buffered_ms: 60_000,
                slope_ms_per_s: 0,
                samples: 8,
                ..Default::default()
            },
            &AbrPolicy::measured(),
        )
        .score
            < 5
    );
}

#[test]
fn a_safe_budget_selects_the_best_actuator_directly() {
    let catalog = hd_catalog();
    assert_eq!(
        catalog.best_for_budget(15_000).map(|c| c.rung),
        Some(Rung::P1080M14)
    );
    assert_eq!(
        catalog.best_for_budget(3_000).map(|c| c.rung),
        Some(Rung::P720Low)
    );
    assert_eq!(
        catalog.candidate(Rung::P1080High).expected_wire_kbps,
        20_011
    );
    assert_eq!(
        catalog.best_for_budget(100).map(|c| c.rung),
        None,
        "nothing fits, and it says so"
    );
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
    let tiny = CapacityObservation {
        kbps: 100_000,
        bytes: 40_000,
        active_us: 3_000,
        completed: true,
    };
    let normal = CapacityObservation {
        kbps: 20_000,
        bytes: 400_000,
        active_us: 300_000,
        completed: true,
    };
    let sustained = CapacityObservation {
        kbps: 20_000,
        bytes: 4_000_000,
        active_us: 1_600_000,
        completed: true,
    };
    let truncated = CapacityObservation {
        completed: false,
        ..sustained
    };
    // Large enough to pass every SIZE test and far too brief to be a rate at all: the shape that
    // escaped both the interval test and the clamp.
    let big_and_brief = CapacityObservation {
        kbps: 24_000_000,
        bytes: 600_000,
        active_us: 200,
        completed: true,
    };
    assert_eq!(tiny.quality(), ObservationQuality::Weak);
    assert_eq!(normal.quality(), ObservationQuality::Normal);
    assert_eq!(sustained.quality(), ObservationQuality::Strong);
    assert_eq!(
        truncated.quality(),
        ObservationQuality::Weak,
        "a truncated read proves a floor"
    );
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
        estimate.update(CapacityObservation {
            kbps: 8_000,
            ..first
        });
        estimate.slow_kbps
    };
    assert!(
        settle(sustained) < settle(normal),
        "a strong sample pulls harder"
    );
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
    assert!(
        obs(28_116).is_regime_change(&estimate),
        "seven times is not jitter"
    );
    estimate.update(obs(28_116));
    assert_eq!(
        estimate.slow_kbps, 28_116,
        "the new regime is the estimate, not a blend of two"
    );
    assert_eq!(
        estimate.samples, 1,
        "with one sample's worth of confidence, no more"
    );
    assert!(
        estimate.conservative_kbps() >= source_requirement_kbps(8_000, &AbrPolicy::measured()),
        "which is what makes the second probe decisive, as the device run needed",
    );

    // Symmetric, and ordinary variance is nowhere near it.
    let mut falling = CapacityEstimate::default();
    for _ in 0..4 {
        falling.update(obs(40_000));
    }
    assert!(
        !obs(30_000).is_regime_change(&falling),
        "a 25% dip is the link breathing"
    );
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
    let flat = |slope| BufferEstimate {
        buffered_ms: 12_000,
        slope_ms_per_s: slope,
        ..Default::default()
    };
    for slope in [0, -4, -16, -49, 100] {
        assert!(
            !flat(slope).draining(),
            "slope {slope} is noise around flat"
        );
    }
    for slope in [-51, -400, -2_000] {
        assert!(
            flat(slope).draining(),
            "slope {slope} is a reserve actually going away"
        );
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
    assert_eq!(
        estimate.conservative_kbps(),
        20_000,
        "at most half of an unconfirmed number"
    );
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
    assert_eq!(
        brief,
        fresh(),
        "a gap shorter than a half-life is not staleness"
    );

    let mut aged = fresh();
    aged.age_ms(u64::from(policy.stale_half_life_ms) * 2, &policy);
    assert!(aged.uncertainty_pm > fresh().uncertainty_pm);
    assert!(aged.samples > 1, "still a history, just a less certain one");

    let mut ancient = fresh();
    ancient.age_ms(u64::from(policy.stale_half_life_ms) * 10, &policy);
    assert_eq!(
        ancient.samples, 1,
        "past four half-lives it is a memory, not a measurement"
    );
}

/// **The bootstrap table.** One row per link class, plus the three ways a Remote probe can end.
#[test]
fn bootstrap_decides_from_the_link_class_and_one_bounded_probe() {
    let policy = AbrPolicy::measured();
    let catalog = hd_catalog();
    let go =
        |link, feasible, source, probe| bootstrap(link, feasible, source, probe, &catalog, &policy);
    let complete = |kbps| {
        Some(CapacityObservation {
            kbps,
            bytes: 2_000_000,
            active_us: 400_000,
            completed: true,
        })
    };

    // A verified LAN carrying a playable file needs no measurement to prove it.
    let local = go(LinkKind::Local, true, 28_000, None);
    assert!(local.original && local.reason == BootstrapReason::LocalDirect);
    assert!(
        local.prior.is_none(),
        "nothing was measured, so nothing is claimed"
    );

    // Relay is bandwidth-limited by design; measuring it would be theatre.
    assert_eq!(
        go(LinkKind::Relay, true, 28_000, None).reason,
        BootstrapReason::RelayLimited
    );
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
    assert_eq!(
        prior.samples, 1,
        "weak on purpose: a different request to a different server"
    );
    assert_eq!(prior.uncertainty_pm, MAX_UNCERTAINTY_PM);

    // Below average: HLS, and the achieved lower bound still picks the opening rung.
    let borderline = go(LinkKind::Remote, true, 28_000, complete(27_000));
    assert!(!borderline.original);
    assert_eq!(borderline.reason, BootstrapReason::ProbeBelowRequirement);
    assert_eq!(
        borderline.rung,
        catalog.best_for_budget(27_000).unwrap().rung
    );

    // A slow Remote opens where the measurement says, NOT at an emergency floor it would then
    // spend a minute climbing out of.
    let slow = go(LinkKind::Remote, true, 60_000, complete(17_000));
    assert!(!slow.original);
    assert_eq!(slow.rung, catalog.best_for_budget(17_000).unwrap().rung);

    // Nothing to reason from: playback still starts, conservatively.
    for inconclusive in [
        None,
        Some(CapacityObservation {
            kbps: 9_000,
            bytes: 100_000,
            active_us: 90_000,
            completed: false,
        }),
    ] {
        let decision = go(LinkKind::Remote, true, 60_000, inconclusive);
        assert!(!decision.original);
        assert_eq!(decision.reason, BootstrapReason::ProbeInconclusive);
        assert!(
            decision.rung.kbps() <= Rung::P1080.kbps(),
            "conservative, not paralysed"
        );
    }
    // An unknown source bitrate cannot be reasoned about either, and says so.
    assert_eq!(
        go(LinkKind::Remote, true, 0, complete(80_000)).reason,
        BootstrapReason::ProbeInconclusive,
    );
}

#[test]
fn an_incomplete_bootstrap_probe_cannot_seed_or_raise_the_opening_rung() {
    let policy = AbrPolicy::measured();
    let catalog = hd_catalog();
    let fallback = catalog
        .best_for_budget(policy_startup_floor_kbps(&policy))
        .or_else(|| catalog.feasible().next())
        .unwrap()
        .rung;
    let burst = CapacityObservation {
        kbps: u32::MAX,
        bytes: 512 * 1_024,
        active_us: 1_000,
        completed: false,
    };
    let decision = bootstrap(
        LinkKind::Remote,
        true,
        60_000,
        Some(burst),
        &catalog,
        &policy,
    );
    assert_eq!(decision.reason, BootstrapReason::ProbeInconclusive);
    assert_eq!(
        decision.rung, fallback,
        "a censored burst is not a capacity floor"
    );
    assert!(decision.prior.is_none(), "a censored burst is not a prior");
}

/// Re-entering Auto is not a cold start.  The current operating point and the carried posterior
/// are both real evidence; the 720 kbps unknown-link floor is only the third, empty-evidence case.
#[test]
fn auto_reentry_preserves_the_operating_point_and_spends_its_posterior() {
    let policy = AbrPolicy::measured();
    let catalog = hd_catalog();

    assert_eq!(
        hls_reentry_rung(Some(Rung::P720), None, &catalog, &policy),
        Rung::P720,
        "a playing 4 Mbps route must not be replaced by the unknown-link floor",
    );

    let settled = CapacityEstimate::from_snapshot(12_000, 12_000, 200, 16)
        .expect("a settled controller has a posterior");
    let posterior = catalog
        .best_for_budget(settled.conservative_kbps())
        .expect("the posterior admits an actuator")
        .rung;
    assert_eq!(
        hls_reentry_rung(Some(Rung::P480), Some(settled), &catalog, &policy),
        posterior,
        "a known posterior may reclaim more than the temporary fixed rung",
    );

    let weak = CapacityEstimate::from_snapshot(2_000, 2_000, 500, 2)
        .expect("even a weak posterior is still explicit evidence");
    assert_eq!(
        hls_reentry_rung(Some(Rung::P720), Some(weak), &catalog, &policy),
        Rung::P720,
        "a low posterior may not force a visible downgrade at the hand-off itself",
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
    assert!(
        seeded.delivery().slow_kbps < 25_000,
        "{}",
        seeded.delivery().slow_kbps
    );
}

/// **Mode transitions are distinct from HLS rung changes and their cost decays.** An HLS rung
/// change is free here (the viewer never sees it); a mode change is not; and a mode change right
/// after another one is worse than the first.
#[test]
fn mode_transition_cost_is_separate_from_hls_and_decays_with_time() {
    let policy = AbrPolicy::measured();
    let none = TransitionHistory::default();
    assert_eq!(
        transition_cost(ModeKind::Hls, ModeKind::Hls, none, &policy),
        0
    );
    let first = transition_cost(ModeKind::Original, ModeKind::Hls, none, &policy);
    assert_eq!(first, policy.visible_switch_cost);

    let just_switched = TransitionHistory {
        visible_switches: 2,
        since_last_ms: Some(1_000),
    };
    let long_ago = TransitionHistory {
        visible_switches: 2,
        since_last_ms: Some(policy.visible_switch_decay_ms * 4),
    };
    let recent = transition_cost(ModeKind::Hls, ModeKind::Original, just_switched, &policy);
    let old = transition_cost(ModeKind::Hls, ModeKind::Original, long_ago, &policy);
    assert!(
        recent > old && old >= first,
        "recent={recent} old={old} first={first}"
    );
    assert!(
        transition_cost(
            ModeKind::Hls,
            ModeKind::Original,
            TransitionHistory {
                visible_switches: 6,
                since_last_ms: Some(1_000)
            },
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
        TransitionHistory {
            visible_switches: 5,
            since_last_ms: Some(2_000),
        },
        hd_catalog(),
    )
    .unwrap();
    let good = probe(90_000, true);
    let mut calm = calm;
    let mut flapping = flapping;
    assert_eq!(
        calm.observe_probe(
            good,
            top_candidate(),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            600_000
        ),
        RecoveryVerdict::Recover,
    );
    assert_eq!(
        flapping.observe_probe(
            good,
            top_candidate(),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            600_000
        ),
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
    assert_eq!(
        pinned.telemetry().safe_budget_kbps,
        0,
        "nothing observed yet"
    );
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
    let flapping = TransitionHistory {
        visible_switches: 5,
        since_last_ms: Some(2_000),
    };
    let good = probe(90_000, true);

    let mut at_once = OriginalRecovery::new(
        28_000,
        policy,
        SourceFeatures::default(),
        flapping,
        hd_catalog(),
    )
    .unwrap();
    assert_eq!(
        at_once.observe_probe(
            good,
            top_candidate(),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            600_000
        ),
        RecoveryVerdict::NotWorthIt,
        "the fifth switch two seconds ago is still expensive",
    );

    let mut later = OriginalRecovery::new(
        28_000,
        policy,
        SourceFeatures::default(),
        flapping,
        hd_catalog(),
    )
    .unwrap();
    // Six half-lives, so the penalty is under 2% of its opening value. The RATE is policy and is
    // not under test here; that the clock advances at all is.
    later.advance_to(policy.visible_switch_decay_ms.saturating_mul(6));
    assert_eq!(
        later.observe_probe(
            good,
            top_candidate(),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            600_000
        ),
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
    let flapping = TransitionHistory {
        visible_switches: 5,
        since_last_ms: Some(2_000),
    };
    let good = probe(90_000, true);
    let target = policy.visible_switch_decay_ms.saturating_mul(6);

    let mut once = OriginalRecovery::new(
        28_000,
        policy,
        SourceFeatures::default(),
        flapping,
        hd_catalog(),
    )
    .unwrap();
    once.advance_to(target);

    let mut many = OriginalRecovery::new(
        28_000,
        policy,
        SourceFeatures::default(),
        flapping,
        hd_catalog(),
    )
    .unwrap();
    for _ in 0..40 {
        many.advance_to(target);
    }
    assert_eq!(
        once.observe_probe(
            good,
            top_candidate(),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            600_000
        ),
        many.observe_probe(
            good,
            top_candidate(),
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            600_000
        ),
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
    let net = |secs: Option<u32>| risk_score(secs, false, &policy);

    assert_eq!(net(None), 0, "no deficit at all is not a risk");
    assert_eq!(
        net(Some(safe)),
        0,
        "the ladder charged 1 for a horizon it calls SAFE"
    );
    assert_eq!(
        net(Some(safe + 600)),
        0,
        "and charges nothing more for being safer still"
    );
    assert_eq!(
        net(Some(fallback)),
        40,
        "the ladder charged 4 one second above its own floor"
    );
    assert_eq!(
        net(Some(0)),
        40,
        "below the floor it is an emergency, decided by a hard guard"
    );

    // Strictly decreasing in T across the whole band, which is the property a step ladder cannot
    // have: a 59 s horizon and a 21 s horizon scored the same 4.
    let mut previous = 41;
    for secs in fallback..=safe {
        let score = net(Some(secs));
        assert!(
            score < previous,
            "risk must fall as the horizon grows: {secs}s scored {score}"
        );
        assert!(
            score <= 40,
            "{secs}s scored {score}, past the term's own ceiling"
        );
        previous = score;
    }

    assert_eq!(risk_score(None, true, &policy), 30);
    assert_eq!(risk_score(Some(0), true, &policy), RISK_SCORE_MAX);
}

/// PMS gives only a whole-file average, not a peak envelope.  Inventing a multiplier does not turn
/// it into one; VBR pressure becomes evidence only when the playable reserve actually drains.
#[test]
fn source_average_is_the_consumption_rate_and_vbr_is_observed_in_the_buffer() {
    let policy = AbrPolicy::measured();
    assert_eq!(source_requirement_kbps(40_000, &policy), 40_000);
    assert_eq!(source_requirement_kbps(0, &policy), 0);
    let mut mode = original(40_000);
    let observation = mode
        .observe_saturated(
            window_bytes(41_000),
            ORIGINAL_WINDOW_US,
            Some(30_000),
            HOUR_MS,
        )
        .unwrap();
    assert_eq!(
        observation.horizon_secs, None,
        "service above average creates no synthetic deficit"
    );
    assert!(observation.fallback.is_none());
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
        buffer: BufferEstimate {
            buffered_ms: 8_000,
            ..Default::default()
        },
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
        buffer: BufferEstimate {
            buffered_ms: 8_000,
            ..Default::default()
        },
        remaining_ms: HOUR_MS,
        history: TransitionHistory::default(),
        original_feasible: true,
        source_dv: false,
        source_atmos: false,
        unsafe_deficit_ms: 0,
    };
    let quality_of = |kbps: u32, raster: (u16, u16)| {
        original_utility(
            &ModeInputs {
                source_kbps: kbps,
                source_raster: raster,
                ..base
            },
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
        buffer: BufferEstimate {
            buffered_ms: 4_000,
            ..Default::default()
        },
        remaining_ms: HOUR_MS,
        history: TransitionHistory::default(),
        original_feasible: true,
        source_dv: false,
        source_atmos: false,
        unsafe_deficit_ms: 0,
    };
    let whole_film = original_utility(&base, &policy).expect("feasible");
    let last_ten_seconds = original_utility(
        &ModeInputs {
            remaining_ms: 10_000,
            ..base
        },
        &policy,
    )
    .expect("feasible");
    assert!(
        whole_film.risk > 0,
        "the fixture must produce a risk to scale"
    );
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

/// Changing only a non-identifiable decomposition of total acquisition must not flip a visible
/// Original/HLS reload while link, reserve, candidate and catalog server cost are unchanged.
#[test]
fn recovery_does_not_double_count_total_acquisition_as_server_load() {
    let policy = AbrPolicy::measured();
    let current = top_candidate();
    let mut loaded = ProductionEstimate::default();
    for _ in 0..6 {
        loaded.observe(2_000, current.production_load_pm, false);
    }
    assert!(
        loaded.ratio_pm > 1_000,
        "the setup must be slower than real time"
    );

    let spent = TransitionHistory {
        visible_switches: 3,
        since_last_ms: Some(0),
    };
    let gate = || {
        OriginalRecovery::new(
            28_000,
            policy,
            SourceFeatures::default(),
            spent,
            hd_catalog(),
        )
        .expect("feasible")
    };
    let (mut idle_gate, mut loaded_gate) = (gate(), gate());
    let verdicts: Vec<(RecoveryVerdict, RecoveryVerdict)> = (0..3)
        .map(|_| {
            (
                idle_gate.observe_probe(
                    probe(50_000, true),
                    current,
                    &idle_server(),
                    healthy_buffer(),
                    &healthy_hls(),
                    HOUR_MS,
                ),
                loaded_gate.observe_probe(
                    probe(50_000, true),
                    current,
                    &loaded,
                    healthy_buffer(),
                    &healthy_hls(),
                    HOUR_MS,
                ),
            )
        })
        .collect();
    assert!(
        verdicts.iter().all(|(idle, busy)| idle == busy),
        "a non-identifiable decomposition flipped a visible reload: {verdicts:?}",
    );
}

/// The source transfer is the experiment HLS cannot perform.  It is deliberately postponed while
/// a larger HLS request remains available, irrespective of how attractive Original looks.
#[test]
fn the_source_experiment_starts_only_after_hls_exhausts_its_request_sizes() {
    let policy = AbrPolicy::measured();
    let spent = TransitionHistory {
        visible_switches: 3,
        since_last_ms: Some(0),
    };
    let gate = || {
        OriginalRecovery::new(
            28_000,
            policy,
            SourceFeatures::default(),
            spent,
            hd_catalog(),
        )
        .expect("feasible")
    };
    let floor = hd_catalog().candidate(Rung::P480);
    let roomy = healthy_hls();
    let due_at_floor = gate().probe_due(
        floor,
        false,
        &idle_server(),
        sample(40_000, 200, 20_000),
        Some(400),
        healthy_buffer(),
        &roomy,
        HOUR_MS,
        0,
    );
    let due_at_top = gate().probe_due(
        top_candidate(),
        true,
        &idle_server(),
        sample(40_000, 200, 20_000),
        Some(400),
        healthy_buffer(),
        &roomy,
        HOUR_MS,
        0,
    );
    assert_eq!(
        due_at_floor,
        Err(ProbeBlock::BelowHlsCeiling),
        "useful HLS traffic is the cheaper experiment",
    );
    assert_ne!(due_at_top, Err(ProbeBlock::BelowHlsCeiling));
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
        buffer: BufferEstimate {
            buffered_ms: 4_000,
            ..Default::default()
        },
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
        &ModeInputs {
            remaining_ms: 8_000,
            ..inputs
        },
        &policy,
    );

    assert!(
        long.risk > 0,
        "the fixture must carry a real risk cost or this grades nothing"
    );
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
    let orig_short = original_utility(
        &ModeInputs {
            remaining_ms: 8_000,
            ..inputs
        },
        &policy,
    )
    .expect("feasible");
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
            probe(2_000, false),
            current,
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            HOUR_MS,
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
            probe(80_000, true),
            current,
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            HOUR_MS,
        ),
        RecoveryVerdict::Recover,
    );
    let cmp = gate
        .comparison()
        .expect("a real decision publishes its basis");
    assert_eq!(cmp.chosen, ModeKind::Original);
    assert_eq!(cmp.reason, ModeReason::OriginalWorthIt);
    assert!(
        cmp.loser.is_some(),
        "the alternative was scored, so it must be readable"
    );
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
            probe(2_000, false),
            current,
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            HOUR_MS,
        ),
        RecoveryVerdict::Insufficient,
    );
    assert!(
        gate.comparison().is_none(),
        "a truncated probe must RETIRE the previous comparison, not leave it standing beside a \
         verdict it had no part in",
    );
    assert_eq!(
        cmp.scale_pm, 1_000,
        "an hour of film is the full benefit scale"
    );
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
    // then `ImminentStarvation` — a hard guard that consults no utility and, before the post-seek
    // confirmation fix, no persistence — fires first and this test grades that instead. A deep,
    // almost-flat reserve keeps the horizon in the band between the two policy horizons, which is
    // the only region `SustainedDeficit` owns.
    let starved = |n: u64| window_bytes(9_000) * n;
    let fell = |n: u64| -> i64 { 45_000 - 300 * n as i64 };

    // Wall == active: the saturated reader. This is the case the retired count described.
    let mut saturated = original(60_000);
    let mut fired_saturated = None;
    for n in 1..=12u64 {
        let obs = saturated.observe(
            starved(n),
            ORIGINAL_WINDOW_US * n,
            Some(fell(n)),
            HOUR_MS,
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
    for n in 1..=12u64 {
        let obs = throttled.observe(
            starved(n),
            ORIGINAL_WINDOW_US * n,
            Some(fell(n)),
            HOUR_MS,
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
        buffer: BufferEstimate {
            buffered_ms: 8_000,
            ..Default::default()
        },
        remaining_ms: HOUR_MS,
        history: TransitionHistory::default(),
        original_feasible: true,
        source_dv: false,
        source_atmos: false,
        unsafe_deficit_ms: 0,
    };
    let features = |dv: bool, atmos: bool| {
        original_utility(
            &ModeInputs {
                source_dv: dv,
                source_atmos: atmos,
                ..base
            },
            &policy,
        )
        .expect("feasible")
        .features
    };
    let plain = features(false, false);
    assert!(
        plain > 0,
        "no re-encode at all is a real benefit of EVERY Original, and pricing it at zero for a \
         plain file while pricing DV and Atmos together at 25 is the conflation N16 names",
    );
    assert!(
        features(true, false) > features(false, true),
        "DV outranks Atmos"
    );
    assert!(
        features(false, true) > plain,
        "and Atmos is still worth something"
    );
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
    assert!(
        CapacityEstimate::from_snapshot(40_000, 40_000, 200, 0).is_none(),
        "no samples"
    );
    assert!(
        CapacityEstimate::from_snapshot(0, 40_000, 200, 9).is_none(),
        "no rate"
    );
    // The cap is a cap on the way in too — a snapshot cannot claim more confidence than the
    // estimator's own floor allows.
    assert_eq!(
        CapacityEstimate::from_snapshot(40_000, 40_000, 900, 9)
            .unwrap()
            .uncertainty_pm,
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
    assert_eq!(
        t.buffer.buffered_ms, 0,
        "the reserve at the old position is not a reserve here"
    );
    assert_eq!(t.buffer.samples, 0, "nor is its history");
    assert_eq!(t.gates.draining, 0);
    assert_eq!(
        t.pending, None,
        "a transaction proposed for the old position must not survive"
    );
    assert_eq!(
        t.gates.dwell_ms, 0,
        "and no encoder has been started on this side of the seek"
    );
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
        buffer: BufferEstimate {
            buffered_ms: 30_000,
            ..Default::default()
        },
        remaining_ms: HOUR_MS,
        history: TransitionHistory::default(),
        original_feasible: true,
        source_dv: false,
        source_atmos: false,
        unsafe_deficit_ms: 0,
    };
    let current = hd_catalog().candidate(Rung::P1080High);
    let (mode, reason, chosen, other) = choose_mode(&inputs, current, current, &policy);
    assert_eq!(
        (mode, reason),
        (ModeKind::Original, ModeReason::OriginalWorthIt)
    );
    assert_eq!(
        chosen.server, 0,
        "no server video encoding is the term HLS cannot match"
    );
    assert!(chosen.total > other.expect("both were feasible").total);

    // Infeasible is not a low score — it is not a candidate.
    let (mode, reason, _, other) = choose_mode(
        &ModeInputs {
            original_feasible: false,
            ..inputs
        },
        current,
        current,
        &policy,
    );
    assert_eq!(
        (mode, reason),
        (ModeKind::Hls, ModeReason::OriginalInfeasible)
    );
    assert!(other.is_none());
}

#[test]
fn equal_mode_utility_preserves_the_current_mode() {
    let mut policy = AbrPolicy::measured();
    policy.visible_switch_cost = 0;
    policy.visible_switch_penalty = 0;
    let inputs = ModeInputs {
        current: ModeKind::Original,
        remaining_ms: 0,
        ..mode_inputs()
    };
    let current = hd_catalog().candidate(Rung::P1080High);
    let original = original_utility(&inputs, &policy).unwrap();
    let hls = hls_utility(current, current, &inputs, &policy);
    assert_eq!(
        original.total, hls.total,
        "the fixture must be an exact tie"
    );

    let (chosen, reason, winner, loser) = choose_mode(&inputs, current, current, &policy);
    assert_eq!(
        (chosen, reason),
        (ModeKind::Original, ModeReason::OriginalWorthIt)
    );
    assert_eq!(winner, original);
    assert_eq!(loser, Some(hls));
}

/// **A downshift pin must land from the top of the ladder.** It could not, and the M4 census paid
/// for it: four of its seven points never reached their pinned rung and silently recorded the top
/// rung five times instead (`pin_320`, `pin_2000`, `pin_10000`, `pin_16000` all logged
/// `rung=20000` with byte lists identical to `pin_20000`'s).
///
/// The cause was an upshift-only tool gate applied in a direction it was never argued for. A
/// downshift has no repeatable candidate observation, and its live deadline is computed at the
/// transaction rather than inherited from this six-segment pin precondition. Six segments is
/// 12 000 ms at `D = 2000`, while the reachable
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
            assert_eq!(
                proposal.rung,
                Rung::P240,
                "the pin is the target, not one rung down"
            );
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
            matches!(
                pinned.observe_next(sample(40_000, 300, reserve_ms)),
                Decision::Stay
            ),
            "an upshift pin transacted on a reserve smaller than the transaction costs, which is \
             the livelock PIN_MIN_RESERVE_SEGMENTS exists to prevent",
        );
    }
}

/// A rejected larger request must not contaminate the rollback certificate for the current
/// operating point. Its prefix/result explains that candidate transaction, while the old bag is
/// what the player must rely on after returning to the old cursor.
#[test]
fn a_rejected_candidate_preserves_the_current_operating_points_bag() {
    let mut controller = Controller::starting_at(Rung::P1080, None, HlsActuatorCatalog::measured());
    let Decision::Prime(proposal) =
        controller.observe_next(sample_bytes(250_000, 300_000, 300, 20_000))
    else {
        panic!("the current bag must fund an excitation");
    };
    let before = controller.window_len();
    let candidate = sample_bytes(2_000_000, 1_500_000, 1_200, 20_000);
    assert!(!controller.candidate_ready(proposal, candidate, declared_bps(proposal.rung)));
    assert!(controller.reject(proposal, RejectCause::Candidate, controller.clock_ms()));
    assert_eq!(controller.window_len(), before);
}

/// A committed candidate starts a new operating-point bag from its own completed steady segment.
/// Keeping the smaller responses beside it would let the demand-capped old tier dominate the
/// exact evidence the excitation just bought.
#[test]
fn a_committed_candidate_replaces_the_smaller_operating_points_bag() {
    let mut controller = Controller::starting_at(Rung::P1080, None, HlsActuatorCatalog::measured());
    let Decision::Prime(proposal) =
        controller.observe_next(sample_bytes(250_000, 300_000, 300, 20_000))
    else {
        panic!("the current bag must fund an excitation");
    };
    let candidate = sample_bytes(2_000_000, 1_500_000, 750, 20_000);
    assert!(controller.candidate_ready(proposal, candidate, declared_bps(proposal.rung)));
    assert!(controller.commit(proposal, controller.clock_ms()));
    controller.commit_candidate_evidence(candidate);
    assert_eq!(
        controller.window_len(),
        1,
        "only the candidate regime survives the commit"
    );
    assert_eq!(controller.prime_runway_ms(), Some(1_500));
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
        Some(DecisionReason::Hls(HlsReason::BufferConstraint)),
        "B<R_o is the binding emergency arm when the same collapse also has A>D",
    );
    assert_eq!(
        controller.telemetry().emergency_horizon_secs,
        None,
        "the retired rate horizon must not masquerade as an input to the conservation decision",
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
    assert!(
        !matches!(
            decision,
            Decision::Prime(Proposal {
                direction: Direction::Down,
                ..
            })
        ),
        "capacity == requirement is an INFINITE horizon; a 500pm confidence discount is not a \
         measured deficit; got {decision:?}",
    );
    // The counterfactual, written out rather than described: the same reserve and the same
    // segment, scored on the rate the RISK term uses. `uncertainty_pm` is at its 500 cap here, so
    // that is half the measured rate — a 2x deficit that was never observed, and a horizon inside
    // the window. This is the assertion the test exists for; without it, "measured rather than
    // conservative" is satisfiable by any predicate that happens not to fire.
    let measured = controller.telemetry().delivery.fast_kbps;
    let conservative = controller.telemetry().delivery.conservative_kbps();
    assert_eq!(
        conservative,
        measured / 2,
        "the first sample of a rung is a 500pm discount"
    );
    let counterfactual = starvation_horizon(1_958, 14_000, conservative).seconds;
    assert!(
        counterfactual.is_some_and(|s| s <= AbrPolicy::measured().starvation_fallback_secs),
        "the conservative form fires here — {counterfactual:?} — which is the defect being avoided",
    );
    let measured_horizon = controller.telemetry().emergency_horizon_secs;
    assert!(
        measured_horizon.map_or(true, |s| s > AbrPolicy::measured().starvation_fallback_secs
            * 100),
        "on the measured rate the horizon is unreachable rather than imminent; got \
         {measured_horizon:?}",
    );
}

/// The current-point boundary has no percentage or horizon: acquisition equal to media duration
/// is sustainable; one micro-step beyond it is not, whatever the starting reserve.
#[test]
fn the_cold_start_floor_fires_at_a_tenth_of_the_rate_and_not_at_a_twentieth() {
    let mut holds = controller_at(Rung::P1080M14);
    let decision = holds.observe_next(sample(14_000, 1_000, 2_000));
    assert!(
        !matches!(
            decision,
            Decision::Prime(Proposal {
                direction: Direction::Down,
                ..
            })
        ),
        "A=D replenishes exactly what it spends: {decision:?}",
    );

    let mut fires = controller_at(Rung::P1080M14);
    assert!(matches!(
        fires.observe_next(sample(14_000, 1_001, 60_000)),
        Decision::Prime(Proposal {
            direction: Direction::Down,
            ..
        })
    ));
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
    // The body-rate label is 5% below a catalog value, but A=500ms for D=2000ms.
    for _ in 0..2 {
        let decision = controller.observe_next(sample(19_010, 250, full));
        assert!(!matches!(
            decision,
            Decision::Prime(Proposal {
                direction: Direction::Down,
                ..
            })
        ));
    }
    assert_eq!(
        controller.telemetry().emergency_horizon_secs,
        None,
        "a catalog-rate horizon is no longer part of the decision",
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
    let short = 13_300;

    // A=500ms. Both a deep reserve and a 1500ms reserve cover the exact runway, despite the
    // response's body rate being below a remembered catalog value.
    let mut deep = controller_at(Rung::P1080M14);
    for _ in 0..3 {
        assert_eq!(
            observe_without_upshift(&mut deep, sample(short, 250, 20_000)),
            Decision::Stay,
            "a 5% deficit against 20 s of reserve is arithmetic, not an emergency",
        );
    }

    let mut shallow = controller_at(Rung::P1080M14);
    let decision = observe_without_upshift(&mut shallow, sample(short, 250, 1_500));
    assert!(
        !matches!(
            decision,
            Decision::Prime(Proposal {
                direction: Direction::Down,
                ..
            })
        ),
        "B=1500ms still covers R_o=500ms: {decision:?}",
    );

    let mut below_runway = controller_at(Rung::P1080M14);
    assert!(
        matches!(
            below_runway.observe_next(sample(short, 250, 499)),
            Decision::Prime(Proposal {
                direction: Direction::Down,
                ..
            })
        ),
        "B<R_o is the exact shallow-buffer failure",
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
    assert!(
        buffer.starving(),
        "the starvation arm covers the whole unaffordable region"
    );
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
                    let wire = HlsActuatorCatalog::measured()
                        .candidate(*rung)
                        .expected_wire_kbps;
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
    // Empty reserve: `D = buffer_target_ms`, so the budget is cut to H/(H+B*) of C_safe.
    let empty = catalog
        .best_sustainable(20_000, &policy, 0)
        .expect("something is always affordable at 20 Mbit/s");
    let full = catalog
        .best_sustainable(20_000, &policy, 30_000)
        .expect("ditto");
    assert!(
        empty.expected_wire_kbps < full.expected_wire_kbps,
        "the filter must cost something at an empty reserve: {} vs {}",
        empty.expected_wire_kbps,
        full.expected_wire_kbps,
    );
    let cut = i64::from(20_000u32) * policy.buffer_refill_horizon_ms
        / (policy.buffer_refill_horizon_ms + policy.buffer_target_ms);
    assert_eq!(
        cut, 16_000,
        "H/(H+B*) of 20 000 kbps is 0.8 — derived, not chosen"
    );
    assert!(i64::from(empty.expected_wire_kbps) <= cut);

    // ...and the shadow: the reserve gate is above `buffer_target_ms` at every rung, so any
    // reserve that clears the gate also zeroes the deficit.
    let segment = 2_000i64;
    for rung in LADDER {
        let wire = HlsActuatorCatalog::measured()
            .candidate(rung)
            .expected_wire_kbps;
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
        let wire = HlsActuatorCatalog::measured()
            .candidate(rung)
            .expected_wire_kbps;
        let ceiling = plant::b_max_est_ms(
            wire.saturating_sub(policy.assumed_audio_kbps),
            policy.assumed_audio_kbps,
        );
        let gate =
            (segment * 3).min(ceiling * i64::from(policy.buffer_reserve_fraction_pm) / 1_000);
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
    let bottom = HlsActuatorCatalog::measured()
        .candidate(Rung::P240)
        .expected_wire_kbps;
    let bottom_ceiling = plant::b_max_est_ms(
        bottom.saturating_sub(policy.assumed_audio_kbps),
        policy.assumed_audio_kbps,
    ) * i64::from(policy.buffer_reserve_fraction_pm)
        / 1_000;
    assert!(
        bottom_ceiling > segment * 3,
        "the constant must still bind at the floor rung"
    );
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
            probe(80_000, true),
            current,
            &idle_server(),
            healthy_buffer(),
            &healthy_hls(),
            remaining_ms,
        );
        let Some(cmp) = gate.comparison() else {
            continue;
        };
        any += 1;
        let loser = cmp.loser.unwrap_or_default();
        assert!(
            cmp.winner.total >= loser.total,
            "at {remaining_ms} ms remaining the published winner totalled {} and the loser {} — a \
             line a reader cannot reconcile with `chose=`",
            cmp.winner.total,
            loser.total,
        );
        let hls_side = if cmp.chosen == ModeKind::Hls {
            cmp.winner
        } else {
            loser
        };
        assert_eq!(
            hls_side.features, 0,
            "the HLS side carries no features term"
        );
        assert_eq!(
            cmp.hls_rung.kbps(),
            20_000,
            "`vs_hls=` is the rung's nominal rate, not its expected wire rate",
        );
    }
    assert!(
        any >= 3,
        "the fixture must reach a real comparison at most horizons, got {any}"
    );
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
    let down = Proposal {
        rung: Rung::P720Low,
        direction: Direction::Down,
    };
    let up = Proposal {
        rung: Rung::P720Low,
        direction: Direction::Up,
    };

    // 2000 kbps of output over 2 s of media, on a link measured at 6 000 kbps: 666 ms.
    let need = predicted_transfer(2_000, media, 6_000, 0);
    assert_eq!(need, std::time::Duration::from_millis(666));

    assert_eq!(
        candidate_warmup_budget(down, media, collapsed, NO_FLOOR, NO_FLOOR),
        collapsed,
        "the pre-fix behaviour, and it is the deadline no transfer can meet",
    );
    assert_eq!(
        candidate_warmup_budget(down, media, collapsed, need, NO_FLOOR),
        need,
        "a downshift out of an exhausted reserve gets the time its own transfer requires",
    );
    assert_eq!(
        candidate_warmup_budget(up, media, collapsed, need, NO_FLOOR),
        collapsed,
        "and an upshift does NOT — once the reserve is gone an upshift has already lost",
    );
}

/// A right-censored current response leaves the pre-collapse delivery estimate unchanged. The
/// request-indexed plant measured the consequence: after the live reserve reached zero, a floor
/// candidate that physically needed about a second inherited a 111 ms deadline from the old fast
/// regime and was abandoned before it could restore a picture. At the floor there is no cheaper
/// transaction to buy, so repeating that deadline makes `B=0` absorbing.
#[test]
fn a_terminal_floor_downshift_runs_to_an_actual_transport_result() {
    let budget = std::time::Duration::from_millis(111);
    let floor_down = Proposal {
        rung: Rung::P240,
        direction: Direction::Down,
    };
    let higher_down = Proposal {
        rung: Rung::P480,
        direction: Direction::Down,
    };
    let floor_up = Proposal {
        rung: Rung::P240,
        direction: Direction::Up,
    };

    assert_eq!(
        candidate_media_reserve_deadline(floor_down, ReservePolicy::Preserve, true, budget,),
        None,
        "once B=0 is observed, aborting the only response can only re-request the same bytes",
    );
    assert_eq!(
        candidate_media_reserve_deadline(floor_down, ReservePolicy::Preserve, false, budget,),
        Some(budget),
    );
    assert_eq!(
        candidate_media_reserve_deadline(higher_down, ReservePolicy::Preserve, true, budget,),
        Some(budget),
    );
    assert_eq!(
        candidate_media_reserve_deadline(floor_up, ReservePolicy::Preserve, true, budget),
        Some(budget),
    );
}

/// Live trace, 2026-09-01: the completed 12 Mbit operating point left 2.168 s of playable media,
/// but its exact ordered replay runway was 2.409 s. The controller therefore could not replay the
/// measured chronology and selected the 320 kbit minimax floor. Transport still treated
/// the 84 ms that survived each control-plane round trip as a rollback reserve, killed the floor
/// response at its reserve deadline, and immediately repeated the same transaction. The probe
/// loop — not the 320 kbit response — kept the picture stalled.
///
/// `B<R_o` is already the no-rollback certificate. It must cross the controller/transport seam even
/// while the main thread has not sampled the strictly later `B=0` state.
#[test]
fn a_runway_emergency_floor_is_terminal_before_the_buffer_reaches_zero() {
    let mut controller = Controller::starting_at(Rung::P1080M12, None, hd_catalog());
    // 2_873_000 bytes / 2 s = 11_492 kbps. A=2.409 s, D=2 s gives R_o=2.409 s;
    // the live trace's B=2.168 s is positive but cannot guarantee the next current completion.
    let current = sample_bytes_with_total(2_873_000, 1_819_000, 2_409_000, 2_000, 2_168);
    let proposal = match controller.observe(current, 2_409) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("B<R_o must issue the minimax recovery response"),
    };
    let telemetry = controller.telemetry();
    let admission = telemetry.window.admission.expect("completed current bag");
    assert_eq!(admission.runway_us, 2_409_000);
    assert!(!admission.survivable);
    assert_eq!(proposal.rung, Rung::P240);
    assert_eq!(
        telemetry.reason,
        Some(DecisionReason::Hls(HlsReason::BufferConstraint)),
    );
    assert_eq!(
        controller.pending_reserve_policy(proposal),
        Some(ReservePolicy::TerminalFloor),
        "the transaction contract must not be reconstructed from the diagnostic reason",
    );
    let completed_floor = sample(320, 1_500, 0);
    assert_eq!(
        controller.candidate_verdict(proposal, completed_floor, declared_bps(proposal.rung),),
        CandidateVerdict::Ready,
        "a completed terminal-floor response has no rollback state to reject back into",
    );

    let warmup_budget = std::time::Duration::from_millis(700);
    assert_eq!(
        candidate_media_reserve_deadline(
            proposal,
            controller
                .pending_reserve_policy(proposal)
                .expect("pending proposal policy"),
            false, // B is still 2.168 s, so the main-thread B=0 latch is not armed yet.
            warmup_budget,
        ),
        None,
        "the old cursor cannot replay its observed chronology once B<R_o; reserve-deadlining the only \
         floor response recreates the measured abort/retry loop",
    );

    let higher_down = Proposal {
        rung: Rung::P480,
        direction: Direction::Down,
    };
    let floor_up = Proposal {
        rung: Rung::P240,
        direction: Direction::Up,
    };
    assert_eq!(
        candidate_media_reserve_deadline(proposal, ReservePolicy::Preserve, false, warmup_budget,),
        Some(warmup_budget),
        "a sustainable-failure downshift still preserves the current cursor's proved runway",
    );
    assert_eq!(
        candidate_media_reserve_deadline(
            higher_down,
            ReservePolicy::TerminalFloor,
            false,
            warmup_budget,
        ),
        Some(warmup_budget),
        "the emergency certificate cannot make a more expensive recovery transaction unbounded",
    );
    assert_eq!(
        candidate_media_reserve_deadline(
            floor_up,
            ReservePolicy::TerminalFloor,
            false,
            warmup_budget,
        ),
        Some(warmup_budget),
        "an upshift never spends recovery reserve after its deadline",
    );

    controller.on_resume(30_000);
    assert_eq!(
        controller.pending_reserve_policy(proposal),
        Some(ReservePolicy::TerminalFloor),
        "a user pause/resume ages measurements but cannot demote an in-flight recovery contract",
    );
    assert!(controller.reject(proposal, RejectCause::Circumstance, 32_409));
    assert_eq!(
        controller.pending_reserve_policy(proposal),
        None,
        "rejecting the transaction clears its terminal capability with the proposal",
    );
}

#[test]
fn a_sustainable_ordered_current_history_does_not_false_downshift() {
    let mut controller = Controller::starting_at(Rung::Uhd, None, uhd_catalog());
    assert_eq!(
        controller.observe_next(sample(40_000, 250, 2_000)),
        Decision::Stay
    );
    assert_eq!(
        controller.observe_next(sample(40_000, 1_250, 2_000)),
        Decision::Stay,
        "the ordered pair needs 1s of initial reserve, not the 2.5s worst permutation",
    );
    let diagnostic = controller.telemetry().window.admission.unwrap();
    assert_eq!(
        diagnostic.runway_us, 2_500_000,
        "stress telemetry remains retrospective"
    );
    assert!(!diagnostic.survivable);
}

#[test]
fn a_declared_capacity_regime_change_rebases_the_ordered_current_queue() {
    let mut controller = controller_at(Rung::P720).pinned_to(Some(Rung::P720));
    for _ in 0..20 {
        assert_eq!(
            controller.observe_next(sample(6_000, 700, 20_000)),
            Decision::Stay
        );
    }

    controller = controller.pinned_to(None);
    let decision = controller.observe_next(sample(1_000, 4_000, 15_000));
    assert!(
        matches!(
            decision,
            Decision::Prime(Proposal {
                direction: Direction::Down,
                ..
            })
        ),
        "pre-collapse surplus hid the completed A>D point in the new regime: {decision:?}",
    );
    assert_eq!(
        controller.telemetry().window.admission.unwrap().samples,
        1,
        "the collapse response must be observation zero of the new current-point bag",
    );
}

/// `B<R_o`, not `B<=R_o`: equality still carries the exact ordered-replay certificate. A losing
/// but survivable bag may choose a modeled lower response, but that transaction must retain the
/// old cursor's reserve deadline.
#[test]
fn equality_with_the_replay_runway_keeps_the_preserve_policy() {
    let mut controller = Controller::starting_at(Rung::P1080M12, None, hd_catalog());
    let current = sample_bytes_with_total(2_873_000, 1_819_000, 2_409_000, 2_000, 2_409);
    let proposal = match controller.observe(current, 2_409) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the unsustainable completed point still needs a lower response"),
    };
    let admission = controller
        .telemetry()
        .window
        .admission
        .expect("completed current bag");
    assert_eq!(admission.runway_us, 2_409_000);
    assert!(admission.survivable);
    assert_eq!(
        controller.pending_reserve_policy(proposal),
        Some(ReservePolicy::Preserve),
        "equality is not terminal recovery",
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
    let down = Proposal {
        rung: Rung::P1080,
        direction: Direction::Down,
    };
    let need = predicted_transfer(8_000, media, 9_593, 0);
    assert_eq!(need, std::time::Duration::from_millis(1_667));
    for reserve_ms in [2_000i64, 5_000, 36_000] {
        let reserve = reserve_as_budget(reserve_ms);
        assert_eq!(
            candidate_warmup_budget(down, media, reserve, need, NO_FLOOR),
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
    assert_eq!(
        predicted_transfer(8_000, media, 0, 500),
        std::time::Duration::ZERO
    );
    let down = Proposal {
        rung: Rung::P480,
        direction: Direction::Down,
    };
    let reserve = reserve_as_budget(900);
    assert_eq!(
        candidate_warmup_budget(
            down,
            media,
            reserve,
            predicted_transfer(8_000, media, 0, 500),
            NO_FLOOR
        ),
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
    assert_eq!(
        centre,
        std::time::Duration::from_millis(1_314),
        "the deadline that missed by 13ms"
    );

    // `unc=500pm` is what the same log line published on those transactions.
    let widened = predicted_transfer(16_000, media, 24_353, 500);
    assert_eq!(widened, std::time::Duration::from_millis(1_971));
    assert!(
        widened > std::time::Duration::from_millis(1_327),
        "and it must clear the transfer that actually happened",
    );

    // A settled estimate buys almost nothing, which is what makes this the estimator's opinion
    // rather than a margin: the widening vanishes as `unc` does.
    assert_eq!(
        predicted_transfer(16_000, media, 24_353, 20),
        std::time::Duration::from_millis(1_340)
    );
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
        assert_eq!(
            predicted_transfer(8_000, media, 0, unc),
            std::time::Duration::ZERO
        );
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
fn an_abandoned_prefix_does_not_change_the_capacity_budget() {
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
                if mark_abandoned {
                    prefix.abandoned()
                } else {
                    prefix
                },
                1_000 + i * 100,
            );
        }
        (before, c.delivery().conservative_kbps())
    };

    let (before_complete, after_complete) = settle(false);
    let (before_abandoned, after_abandoned) = settle(true);
    assert_eq!(
        before_complete, before_abandoned,
        "the two legs must start identical"
    );
    assert_eq!(
        after_abandoned, before_abandoned,
        "a censored deadline event is handled by rollback, not by a point capacity estimate",
    );
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
fn an_abandoned_sample_leaves_the_completed_estimate_unchanged() {
    let mut c = Controller::starting_at(Rung::P720, None, hd_catalog());
    for _ in 0..6 {
        c.observe(sample(8_000, 400, 12_000), 0);
    }
    let settled = c.delivery();
    c.observe(sample_bytes(1_448, 274, 400, 168).abandoned(), 1_000);
    assert_eq!(c.delivery(), settled);
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
            kbps: 5_600,
            bytes: 1_400_000,
            active_us: 2_000_000,
            completed: true,
        });
    }
    let settled = c;
    assert!(
        (5_000..=6_200).contains(&settled.slow_kbps),
        "fixture must settle near the shaped rate: {settled:?}"
    );

    // Three aborts, each timing far above the history — the receive buffer, not the link.
    for kbps in [26_691u32, 35_533, 101_078] {
        c.update(CapacityObservation {
            kbps,
            bytes: 1_448,
            active_us: 274,
            completed: false,
        });
    }
    assert_eq!(
        c, settled,
        "an abandoned prefix may not move a capacity estimate at all"
    );
}

/// A slow prefix is censored too.  Its deadline failure is actionable, but its byte/time ratio does
/// not identify whether path capacity, PMS production, pacing, or startup withheld the remainder.
#[test]
fn a_slow_abandoned_prefix_also_leaves_capacity_unchanged() {
    let mut c = CapacityEstimate::default();
    for _ in 0..6 {
        c.update(CapacityObservation {
            kbps: 20_000,
            bytes: 5_000_000,
            active_us: 2_000_000,
            completed: true,
        });
    }
    let settled = c;
    c.update(CapacityObservation {
        kbps: 500,
        bytes: 125_000,
        active_us: 2_000_000,
        completed: false,
    });
    assert_eq!(
        c, settled,
        "deadline failure is not a point capacity measurement"
    );
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
    for link in [2_000u32, 20_000, 200_000] {
        for buffered in [2_000i64, 8_000, 24_000] {
            for ratio in [80u32, 400, 900, 1_000] {
                // The highest feasible rung has no candidate above it, and A<=D with B>=R_o keeps
                // the exact current-point certificate healthy. Every observation is therefore a
                // genuine, non-pending Stay rather than a transaction the fixture forgot to close.
                let mut c = Controller::starting_at(Rung::P1080High, None, hd_catalog());
                for i in 0u32..40 {
                    let s = sample(link, ratio, buffered);
                    let decision = c.observe(s, u64::from(i) * 2_000);
                    if decision != Decision::Stay {
                        continue;
                    }
                    assert!(!c.has_pending());
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
    assert!(
        checked > 200,
        "the sweep must actually reach Stay decisions, got {checked}"
    );
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
    assert!(
        !buffer.draining(),
        "a filling reserve must not read as draining"
    );
    assert!(
        buffer.slope_ms_per_s > 0,
        "the setup itself must be a fill: {buffer:?}"
    );

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
    assert!(
        buffer.draining(),
        "a genuine post-commit drain must still fire: {buffer:?}"
    );
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
        let Decision::Prime(proposal) = controller.observe_next(sample(LINK_KBPS, 400, buf_ms))
        else {
            continue;
        };
        if controller.buffer().slope_ms_per_s <= 0 {
            assert!(controller.reject(proposal, RejectCause::Circumstance, controller.clock_ms(),));
            continue;
        }
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
        assert!(
            !controller.buffer().draining(),
            "and a rebased reserve is not draining"
        );
        return;
    }
    panic!(
        "no rung transaction was proposed and accepted — the fixture no longer exercises a commit"
    );
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
    assert_eq!(
        quality_score_at_kbps(rich.expected_wire_kbps),
        76,
        "the ladder's band for 18000"
    );
    assert_eq!(
        quality_score_at_kbps(source_kbps),
        58,
        "the band an 8000 kbps master falls in"
    );

    // **Asked through the PRODUCTION path.** `hls_utility` is what `choose_mode` argmaxes over, so
    // that is what has to be interrogated. This test used to re-implement the cap in its own body
    // and assert the arithmetic, which stayed green with the production line deleted -- the shape
    // the house rule on differential tests exists to forbid.
    //
    // Scoring the SAME candidate against an 8000 kbps source and against an unknown one must give
    // different quality. Under unmodified code the source was not an input on this side at all, so
    // both calls returned the 18000 band and the inequality cannot hold.
    let quality_against = |src: u32| {
        hls_utility(
            rich,
            rich,
            &ModeInputs {
                source_kbps: src,
                ..mode_inputs()
            },
            &policy,
        )
        .quality
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
            HlsCandidate {
                expected_wire_kbps: source_kbps,
                ..rich
            },
            rich,
            &ModeInputs {
                source_kbps,
                ..mode_inputs()
            },
            &policy,
        )
        .quality,
        "a transcode of an 8000 kbps master is worth an 8000 kbps picture, not an 18000 one",
    );

    // The cap must NOT bite when the rung is genuinely under the source -- the ordinary case, where
    // capping would flatten the whole ladder into one band.
    let modest = catalog.candidate(Rung::P720Low);
    assert!(
        modest.expected_wire_kbps < source_kbps,
        "fixture assumption"
    );
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

/// The source probe's useful deadline is the media represented by its exact finite object, and its
/// reserve gate funds both transfer phases while preserving the next credited HLS acquisition.
///
/// Waiting longer cannot turn an `A>D` source object into positive sustainability evidence. The
/// operational 4 s policy remains a cap for the minimum-byte sample of a tiny source. Setup and
/// body are two separately bounded phases of that same plan. The raw Part exact-reuses the live
/// HLS Streaming Resource, so no stop, close or cursor restart belongs to this path. Smoothness
/// therefore admits exactly with `P_setup + P_body + max(R_s,D)`. `R_s` and `D` are not added
/// because the media credit that ends A also restores the post-acquisition balance.
#[test]
fn a_probe_deadline_is_its_media_horizon_and_the_gate_preserves_hls_continuity() {
    let policy = AbrPolicy::measured();
    assert_eq!(
        policy.probe_budget_ms, PROBE_BUDGET_MS,
        "the policy default and operational cap are one number",
    );

    let ordinary = source_probe_plan(8_000, policy.probe_budget_ms).expect("known source");
    assert_eq!(ordinary.target_bytes, 1_000_000);
    assert_eq!(
        ordinary.budget_ms, 1_000,
        "one second of bytes gets one second"
    );

    let tiny = source_probe_plan(720, policy.probe_budget_ms).expect("known source");
    assert_eq!(tiny.target_bytes, SOURCE_PROBE_MIN_BYTES);
    assert_eq!(
        tiny.budget_ms, policy.probe_budget_ms,
        "the minimum sample represents over five seconds, so the operational cap binds",
    );
    let huge = source_probe_plan(200_000, policy.probe_budget_ms).expect("known source");
    assert_eq!(huge.target_bytes, SOURCE_PROBE_MAX_BYTES);
    assert_eq!(
        huge.budget_ms, 336,
        "the maximum sample's represented duration is recomputed after clamping",
    );

    let ask = |buffered_ms| {
        recovery(28_000).probe_due(
            top_candidate(),
            true,
            &idle_server(),
            sample(40_000, 500, buffered_ms),
            Some(1_737),
            healthy_buffer(),
            &healthy_hls(),
            HOUR_MS,
            0,
        )
    };
    // source 28 Mbps => a 3.5 MB one-second body; setup + body + D=2000 => 4000.
    assert_eq!(ask(3_999), Err(ProbeBlock::ShallowReserve));
    let permit = ask(4_000)
        .expect("B=P_setup+P_body+max(R_s,D) is the exact inclusive affordability boundary");
    assert_eq!(
        permit.plan,
        source_probe_plan(28_000, policy.probe_budget_ms).unwrap(),
        "the gate and the HTTP transfer must share the exact same finite object and deadlines",
    );
}

#[test]
fn a_source_probe_preserves_the_known_next_variable_duration_object() {
    let mut gate = recovery(28_000);
    let ask = |gate: &mut OriginalRecovery, buffered_ms: i64| {
        gate.probe_due_with_rollback(
            top_candidate(),
            true,
            &idle_server(),
            sample_of(1_000, 40_000, 500, buffered_ms),
            Some(4_000),
            Some(500),
            healthy_buffer(),
            &healthy_hls(),
            HOUR_MS,
            0,
        )
    };

    assert_eq!(ask(&mut gate, 5_999), Err(ProbeBlock::ShallowReserve));
    assert!(
        ask(&mut gate, 6_000).is_ok(),
        "setup + body + the exact next 4s HLS object is the inclusive funding boundary",
    );
}

/// **A starvation band is a TIME, so grade it against the observed time — not against a modelled
/// one whose numerator is an allowance.** (Device, 2026-08-29, the `auto`-stuck film.)
///
/// `starvation_fallback_secs` says "the reserve runs out within twenty seconds". The imminent
/// branch tested that against `T = B·R/(R−C)`, where `R` is the whole-file average inflated by
/// `vbr_allowance_pm` (1 350) and `C` is this window's measured rate. Both terms are about the
/// FILE and the LINK; neither is about the reserve in front of the decoder. The reserve itself
/// publishes the same quantity directly and without either assumption — `buffered_ms / −slope` —
/// and when the two disagree the model is the one making a claim it cannot see.
///
/// This is the disagreement the device produced, in its own numbers: a 25 264 kbps source on a
/// link measuring ~18 000 kbps, reserve at ~5 100 ms falling 146 ms/s. The model reads
/// `(34 106 − 18 000)/34 106` = a 47 % deficit and forecasts starvation in **11 s**. The reserve
/// reads 146 ms of media lost per second of wall clock — a 15 % deficit — and forecasts **35 s**.
/// The reserve is right by construction: it is the residue of everything the model approximates
/// (this section's real bitrate rather than the file's average, the pump's feed-ahead, the actual
/// delivery), measured rather than composed. Thirty-five seconds is also long enough for the
/// recovery this link went on to make, which is what the abandon threw away.
///
/// It does not weaken the branch against a real collapse — `a_genuine_collapse_still_exits_at_once`
/// below is the same shape with the drain that a collapse actually produces, and it fires on the
/// second window. Nor does it touch `EmergencyLowBuffer`, which reads the RAW delta under
/// `emergency_buffer_ms` precisely so that a cliff needs no trend.
///
/// Differential by construction: against unmodified code the assertion fails on window 2.
#[test]
fn a_slow_drain_with_a_long_observed_horizon_is_not_imminent_starvation() {
    let mut mode = original(25_264);
    // 146 ms/s of drain: -110 ms across each 750 ms window, which is the device's slope exactly.
    let reserve = [5_643_i64, 5_533, 5_423, 5_313, 5_203, 5_093];
    for (i, buffered) in reserve.iter().enumerate() {
        let windows = (i + 1) as u64;
        let observation = mode
            .observe_saturated(
                window_bytes(18_000) * windows,
                ORIGINAL_WINDOW_US * windows,
                Some(*buffered),
                HOUR_MS,
            )
            .unwrap();
        if windows >= 2 {
            assert_eq!(
                observation.slope_ms_per_s, -146,
                "window {windows}: the drain the test is about",
            );
            assert!(
                observation.horizon_secs.is_some_and(|s| s <= 20),
                "window {windows}: the MODELLED horizon is inside the band — which is the whole \
                 point, because it is what used to decide this alone (got {:?}s)",
                observation.horizon_secs,
            );
        }
        assert_ne!(
            observation.fallback,
            Some(OriginalExit::ImminentStarvation),
            "window {windows}: {}ms of reserve draining at {}ms/s is {} seconds away from empty, \
             not {:?}",
            observation.buffered_ms,
            observation.slope_ms_per_s,
            observation.buffered_ms / -observation.slope_ms_per_s,
            observation.horizon_secs,
        );
    }
}

/// The control for [`a_slow_drain_with_a_long_observed_horizon_is_not_imminent_starvation`]: the
/// same link and the same source, with the drain a genuine collapse produces. 2 000 ms of reserve
/// lost per 750 ms window is −2 666 ms/s — media leaving nearly three times faster than wall clock
/// — and the observed horizon is 2 s, well inside the band. The branch must still fire, and on the
/// first window that has a derivative at all.
///
/// It stays clear of `emergency_buffer_ms` (2 000 ms) throughout, so what fires here is the
/// imminent branch and not the emergency guard beneath it.
#[test]
fn a_genuine_collapse_still_exits_at_once() {
    let mut mode = original(25_264);
    let reserve = [8_000_i64, 6_000];
    let mut verdicts = Vec::new();
    for (i, buffered) in reserve.iter().enumerate() {
        let windows = (i + 1) as u64;
        verdicts.push(
            mode.observe_saturated(
                window_bytes(18_000) * windows,
                ORIGINAL_WINDOW_US * windows,
                Some(*buffered),
                HOUR_MS,
            )
            .unwrap(),
        );
    }
    let collapse = verdicts.last().unwrap();
    assert_eq!(
        collapse.slope_ms_per_s, -2_666,
        "the drain a collapse actually produces"
    );
    assert!(
        collapse.buffered_ms > 2_000,
        "above the emergency floor, so this is the imminent branch and not the guard below it",
    );
    assert_eq!(
        collapse.fallback,
        Some(OriginalExit::ImminentStarvation),
        "6 000 ms draining at 2 666 ms/s empties in two seconds — the observed horizon agrees \
         with the modelled one ({:?}s) and the branch must act",
        collapse.horizon_secs,
    );
}

/// **A post-seek reserve with time to confirm a modest drain is not an emergency merely because
/// it entered the fallback band.** (Device, 2026-08-30.)
///
/// The film had already passed the Remote Original probe at 45 858 kbps. After an in-place seek it
/// played for another fifty seconds at 0.98–1.00x with the video AU queue near its 10 MiB cap, then
/// abandoned Original on this line:
///
/// ```text
/// ImminentStarvation measured=23742kbps need=34106kbps buf=3077ms
///                    slope=-198ms/s starve=10 held=2406ms
/// ```
///
/// Both horizons were inside `starvation_fallback_secs`, but that constant says when a visible
/// switch is WORTH paying for; it does not make 2.4 seconds of a modest post-seek trend conclusive.
/// At the observed slope the reserve still had fifteen seconds, enough to finish the existing
/// `sustained_unsafe_deficit_ms` confirmation while retaining the emergency reserve. A real cliff
/// is the control immediately above and must remain immediate.
///
/// Differential by construction: the unmodified imminent branch fires before the first assertion.
#[test]
fn a_post_seek_modest_drain_is_confirmed_before_original_is_abandoned() {
    let mut mode = original(25_264);
    let mut bytes = 0_u64;
    let mut active_us = 0_u64;
    let mut now_ms = 0_u64;

    // The pre-seek Original leg: enough agreeing evidence that the probe was not the only fast
    // sample. The seek keeps this delivery estimate and discards only positional state.
    for _ in 0..4 {
        bytes += window_bytes(45_858);
        active_us += ORIGINAL_WINDOW_US;
        now_ms += ORIGINAL_WINDOW_US / 1_000;
        mode.observe(bytes, active_us, Some(5_000), HOUR_MS, now_ms)
            .unwrap();
    }
    mode.on_seek(bytes, active_us);

    // Four post-seek windows, spaced as the device spaced them. A 159 ms loss per 802 ms is the
    // exact -198 ms/s EWMA in the line above. The first window seeds the new position and the
    // second is the first endpoint that can classify the interval as unsafe; only the following
    // two intervals are known unsafe, for 2 * 802 = 1 604 ms at the verdict.
    let mut observation = None;
    for buffered_ms in [3_554_i64, 3_395, 3_236, 3_077] {
        bytes += window_bytes(23_742);
        active_us += ORIGINAL_WINDOW_US;
        now_ms += 802;
        observation = mode.observe(bytes, active_us, Some(buffered_ms), HOUR_MS, now_ms);
    }
    let tentative = observation.unwrap();
    assert_eq!(tentative.buffered_ms, 3_077);
    assert_eq!(tentative.slope_ms_per_s, -198);
    assert_eq!(tentative.unsafe_deficit_ms, 1_604);
    assert_eq!(
        tentative.horizon_secs,
        Some(51),
        "the measured 6% deficit has a 51s runway"
    );
    assert_eq!(mode.buffer.observed_starvation_secs(), Some(15));
    assert_eq!(
        tentative.fallback, None,
        "fifteen seconds of observed runway can afford the remaining confirmation without a blind reload",
    );

    // The protection is confirmation, not immunity. If the same drain continues past the
    // persistence duration, Original has lost the argument and must hand off.
    let mut confirmed = None;
    for buffered_ms in [2_918_i64, 2_759, 2_600, 2_441] {
        bytes += window_bytes(23_742);
        active_us += ORIGINAL_WINDOW_US;
        now_ms += 802;
        confirmed = mode.observe(bytes, active_us, Some(buffered_ms), HOUR_MS, now_ms);
    }
    let confirmed = confirmed.unwrap();
    assert!(
        confirmed.unsafe_deficit_ms >= AbrPolicy::measured().sustained_unsafe_deficit_ms,
        "the continuation must really pay the confirmation duration",
    );
    assert_eq!(
        confirmed.fallback,
        Some(OriginalExit::ImminentStarvation),
        "the observed reserve is now inside the physical fallback horizon even though the file average is not",
    );
}

/// **A cost that does not grow with the segment must not be extrapolated as if it did.** (Device,
/// 2026-08-30 — the film that sat on 720 kbps/480p for twenty minutes.)
///
/// `transferred_us` answers "what would `q` bytes have cost" as `A_i * q / b_i` — the whole
/// end-to-end acquisition, scaled by the byte ratio. That is right for the part of `A_i` that is
/// bytes moving and wrong for the part that is not, and on this device the second part dominates.
/// The per-segment line says so directly, and it is the line that settles it:
///
/// ```text
/// hls: segment=636 bytes=130284 not_ready=0 open_ms=370 ttfb_ms=66 open_probe_ms=207
///      first_au_ms=578 total_ms=582
/// ```
///
/// `not_ready=0` on every segment of the run: the server was never making us wait. `ttfb_ms=66`:
/// its think time is negligible. Of the 582 ms, about **370 ms is `open_ms` — our own AVIO open
/// plus FFmpeg's probe** (`open_probe_ms=207`) — and only ~200 ms is body. Ask for a rendition
/// four times the size and the body quadruples; the open and the probe do not move at all. The
/// rule multiplied all 582.
///
/// **An earlier version of this test blamed PMS JIT production, and `not_ready=0` refutes it.**
/// The apparent corroboration was circular: `prod=` is an EWMA of `total_fetch_us / duration`, the
/// same quantity the window's demand is built from, so of course they agreed. Kept as a note
/// because the arithmetic below is unchanged by the correction and the wrong cause would have led
/// to a fix in the wrong module.
///
/// The refusal, in the log's own numbers — 19 samples of ~127 464 bytes, `demand=11111ms` against
/// `supply=38000ms`, capping the admissible query near 436 000 bytes, while `P720Low` asks
/// `sigma * W * D / 8000 = 1040 * 2e6 * 2000 / 8e6 = 520 000`:
///
/// ```text
/// abr: steady current=720kbps safe=4251kbps buf=68210ms slope=0ms/s risk=0 onrung=143
///      reason=Some(Hls(EvidenceWindow))
/// ```
///
/// Sixty-eight seconds of reserve, zero risk, a safe budget six times the rung being played, and
/// it will not climb. Decomposed — fixed 355 ms plus body scaled — the same window carries the
/// same candidate at 22 s of demand against 38 s of supply.
///
/// A finite response is now used only to price the current rollback runway. It may therefore fund
/// a bounded candidate transaction, but it may not price that candidate before it is requested.
#[test]
fn a_fixed_per_segment_cost_must_not_be_extrapolated_by_byte_count() {
    let mut controller = Controller::starting_at(Rung::P480, None, hd_catalog());
    // ONE segment off the device, repeated. Nothing here is chosen: bytes and body-read time are
    // the logged `net=5080kbps` at the logged size, `278` is the logged `prod=`, and the reserve
    // is the logged `buf=`.
    let device_segment = || sample_bytes(127_464, 200_700, 278, 68_210);
    // One full finite bag: the device had 143 samples and 64 in the ring, so this is not a fixture
    // that stopped before the measured state was represented.
    for _ in 0..WINDOW_CAPACITY {
        if let Decision::Prime(proposal) = controller.observe_next(device_segment()) {
            assert!(
                proposal.rung > Rung::P480,
                "the only climb available here is upward; got {:?}",
                proposal.rung,
            );
            return;
        }
    }
    panic!(
        "never spent the measured rollback surplus on a climb off 720kbps: safe={}kbps, reserve \
         68s, risk 0, link carries the 2 Mbit/s candidate {}x over",
        controller.telemetry().safe_budget_kbps,
        controller.telemetry().safe_budget_kbps / 2_000,
    );
}

/// **A fetch StallGuard abandoned is not an observation of the link or finite bag.** This pins the
/// device regression where the acquisition window still took it (2026-08-30, collapse off
/// 22 Mbit/s).
///
/// The estimator side of this was already decided and is already fixed:
/// [`an_abandoned_prefix_lowers_the_budget_instead_of_raising_it`] is that fix, and
/// `SegmentSample::completed`'s doc names the failure it ended — *"an abandoned prefix set the
/// budget its own abandonment disproved"*. The window side was never given the same guard.
/// `Controller::observe` calls `acquisitions.observe(sample.bytes, sample.total_fetch_us())`
/// unconditionally, above every early return and with no reference to `completed()`, so a prefix
/// enters the transfer bound as though it were a segment that arrived.
///
/// It poisons in BOTH directions, which is the argument for excluding it rather than for trusting
/// its sign. The estimator's case was optimistic — 1 448 bytes in 274 us times at 42 Mbit/s. The
/// device case here is the pessimistic one:
///
/// ```text
/// abr: sample current=22000kbps media=16983kbps net=56660kbps prod=1171pm n=2
/// abr: stall abort seq=491 bytes=212992 of 1916ms reserve at 6197kbps
/// abr: sample current=22000kbps media=851kbps  net=6197kbps  prod=341pm n=3 decision=prime_down
/// ```
///
/// One honest sample said 56 Mbit/s. The next was a fetch the guard cut short while the freshly
/// started 22 Mbit/s encoder was still producing slower than real time (`prod=1171pm`), and its
/// truncated prefix entered as 6 197 kbps. The ladder then walked 22000 -> 4000 -> 2000 -> 720 and
/// never came back. A ratio taken at the moment a transfer was ABANDONED is the rate that caused
/// the abandonment, not the rate of the link — in either direction it is a measurement of the
/// abort.
///
/// Differential by construction: against unmodified code `have` climbs by the abandoned samples.
#[test]
fn an_abandoned_prefix_must_not_enter_the_acquisition_window() {
    let mut controller = Controller::starting_at(Rung::P1080High, None, hd_catalog());
    for _ in 0..6 {
        controller.observe_next(sample(20_000, 400, 12_000));
    }
    let settled = controller.telemetry().window.have;
    assert_eq!(settled, 6, "six completed segments, six observations");

    // The device's own prefix, four times: 212 992 bytes cut short by the stall guard.
    for _ in 0..4 {
        controller.observe_next(sample_bytes(212_992, 275_000, 958, 84).abandoned());
    }
    assert_eq!(
        controller.telemetry().window.have,
        settled,
        "an abandoned prefix is a measurement of the abort, not of the link — it may no more \
         enter the transfer bound than it may set the capacity estimate",
    );
}

/// **The two ends of the pipeline disagree about what a segment costs, in OPPOSITE directions, and
/// this is the other one.** (Device, 2026-08-30 — the 19 consecutive failed downshifts.)
///
/// [`a_fixed_per_segment_cost_must_not_be_extrapolated_by_byte_count`] records the retired window
/// multiplying a fixed cost by the byte ratio, which made every climb look too expensive. This is
/// the mirror: [`predicted_transfer`] is `bits / rate` and its own doc says so — *"and nothing
/// else"* — computed on `conservative_kbps`, which comes from `active_fetch_us`. So the deadline
/// granted to a candidate warm-up covers the BODY READ alone, while the acquisition it must fit
/// inside also contains the open, the probe, and a brand-new encoder session's cold start.
///
/// The per-segment line prices the fixed half at roughly 370 ms in the STEADY state
/// (`open_ms=370`, of which `open_probe_ms=207`), on a server that was never stalling us
/// (`not_ready=0`). A freshly created candidate session is worse, not better. Against that, every
/// one of nineteen transactions:
///
/// ```text
/// abr: tx Down 2000->720kbps outcome=warmup_deadline decided=2800ms total=2807ms
///      control=1537ms prime=397ms master=369ms media=771ms warmup=nonems warmup_dl=549ms
///      buf_start=84ms buf_decided=84ms buf_end=84ms declared=425kbps
/// ```
///
/// `warmup=nonems` — it never finished inside the budget, once. And the loop sustains itself: each
/// failed transaction spends ~2.7 s, which is what holds the reserve at 84 ms, which is what keeps
/// the budget at its floor.
///
/// **The floor is not the bug and the reserve is not the bug**, which is where an earlier version
/// of this test had it wrong. `candidate_warmup_budget` already protects a downshift from its own
/// empty reserve — `reserve.max(predicted_transfer)` — and 549 ms against an 84 ms reserve is that
/// floor working. The bug is that the floor is made of the body read only.
///
/// Differential by construction: the first assertion reproduces the device's `warmup_dl` and
/// passes; the second fails against unmodified code.
#[test]
fn a_downshift_warmup_budget_must_cover_the_whole_acquisition_not_only_the_body_read() {
    use std::time::Duration;
    let media = Duration::from_millis(2_000);
    let proposal = Proposal {
        rung: Rung::P480,
        direction: Direction::Down,
    };
    // Every number is the device's. 425 kbps is the logged `declared=`; 84 ms the logged reserve on
    // all nineteen attempts; 500 pm the logged `unc=`; 2 322 kbps is the `conservative_kbps` those
    // imply, and the first assertion is what proves the inversion is right.
    let predicted = predicted_transfer(425, media, 2_322, 500);
    assert_eq!(
        predicted.as_millis(),
        549,
        "the device's own warmup_dl=549ms, reproduced"
    );
    // What that playback's segments cost before a byte moved: total_ms=582 with ~200ms of body.
    let fixed_overhead = Duration::from_millis(355);
    let budget = candidate_warmup_budget(
        proposal,
        media,
        reserve_as_budget(84),
        predicted,
        fixed_overhead,
    );

    // What a segment on this pipeline was measured to cost END TO END in the steady state, on a
    // server that never stalled us: total_ms=582, of which ~370 is open+probe and ~200 is body. A
    // cold candidate session is dearer than this, so it is a floor and not an estimate.
    let whole_acquisition = Duration::from_millis(582);
    assert!(
        budget >= whole_acquisition,
        "a warm-up deadline of {budget:?} cannot be met when one segment costs \
         {whole_acquisition:?} end to end — the budget is bits/rate over the link and pays for \
         the body read alone, which is why the log has nineteen `outcome=warmup_deadline` in a \
         row with `warmup=nonems`",
    );
}

/// The demand-capped 720 kbps response from the device trace cannot identify whether 2, 8, or 20
/// Mbit/s is available. A remembered manifest declaration cannot repair that identifiability gap.
/// Once the current bag and reserve finance an experiment, selection must issue the most useful
/// feasible request and learn from that request's actual completed segment.
#[test]
fn a_demand_capped_rung_is_retested_by_an_actual_candidate_transaction() {
    let mut controller = Controller::starting_at(Rung::P480, None, hd_catalog());
    let device_segment = || sample_bytes(127_464, 200_700, 278, 68_210);
    for _ in 0..WINDOW_CAPACITY {
        if let Decision::Prime(proposal) = controller.observe_next(device_segment()) {
            assert_eq!(proposal.direction, Direction::Up);
            assert_eq!(proposal.rung, Rung::P1080High);
            return;
        }
    }
    panic!(
        "never spent the measured surplus on an actual candidate: safe={}kbps",
        controller.telemetry().safe_budget_kbps,
    );
}

/// A finite HLS response measures the service obtained by that response; it is not a ceiling on
/// what a larger response may obtain.  This is the state from the 2026-08-30 trace in its smallest
/// differential form: the current finite bag has disposable reserve for an excitation, while the
/// legacy point-capacity path applies two independent 0.8 discounts and never issues the request
/// that could reveal the path's unused tail.
#[test]
fn a_demand_capped_response_cannot_be_an_upshift_ceiling() {
    let mut controller = Controller::starting_at(Rung::P1080M6, None, hd_catalog());
    let completed = || {
        // 1.5 MB in 1.2 s of body time, 1.3 s end-to-end: sustainable at the current point and
        // leaving a deep reserve to fund an independently measured candidate.
        sample_bytes(1_500_000, 1_200_000, 650, 63_000)
    };

    for _ in 0..WINDOW_CAPACITY {
        if let Decision::Prime(proposal) = controller.observe_next(completed()) {
            assert!(
                proposal.direction == Direction::Up && proposal.rung >= Rung::P1080,
                "the demand-capped response must fund an actual higher request; got {proposal:?}",
            );
            return;
        }
    }

    let telemetry = controller.telemetry();
    panic!(
        "the current bag has exploration surplus, but a point-capacity prefilter kept the \
         controller at 6: safe={}kbps window={:?} reason={:?}",
        telemetry.safe_budget_kbps, telemetry.window.admission, telemetry.reason,
    );
}

/// The stronger identifiability case: the current finite object is paced at exactly real time.
/// Projecting that same service rate onto a larger object necessarily gives `T(q) > D`, but this
/// says nothing about the path's unused tail.  The only observation that can distinguish a 6 Mbps
/// path from a 25 Mbps path behind this same response is an actual larger candidate request, so a
/// full reserve must fund that experiment rather than let the projection veto it.
#[test]
fn a_realtime_demand_capped_response_still_excites_an_unknown_higher_tier() {
    let mut controller = Controller::starting_at(Rung::P1080M6, None, hd_catalog());
    let paced = || sample_bytes(1_500_000, 2_000_000, 1_000, 63_000);
    for _ in 0..WINDOW_CAPACITY {
        if let Decision::Prime(proposal) = controller.observe_next(paced()) {
            assert_eq!(proposal.direction, Direction::Up);
            assert!(proposal.rung > Rung::P1080M6);
            return;
        }
    }

    panic!(
        "the current response was demand-capped at real time and became an absorbing ceiling: {:?}",
        controller.telemetry().reason,
    );
}

/// A request ceiling and the response PMS actually attached to it are two different state
/// variables.  Device trace, 2026-08-31: the top `22 Mbps / 4K` actuator returned a
/// `979 kbps / 720x404` master, so recording the actuator as `current` made the ladder terminal
/// even after later segment service proved a stronger regime.  The only honest next experiment
/// is a fresh encoder at the SAME actuator; mapping 979 kbps back to a guessed lower rung would
/// invent a non-existent inverse for PMS's item-dependent request mapping.
#[test]
fn an_underfilled_top_actuator_refreshes_after_stronger_service_evidence() {
    let mut controller = Controller::starting_at(Rung::Uhd, None, uhd_catalog());
    let underfilled = ObservedHlsVariant::new(979_000, 720, 404).expect("valid HLS response");
    controller.observe_active_variant(underfilled, 5_000);

    assert_eq!(
        controller.current(),
        Rung::Uhd,
        "the actuator remains the request PMS received"
    );
    assert_eq!(
        controller.observe_next(sample(5_000, 400, 40_000)),
        Decision::Stay
    );
    assert_eq!(
        controller.telemetry().reason,
        Some(DecisionReason::Hls(HlsReason::ResponseLimited)),
        "the observed response is below the requested/source geometry, not `AtBestRung`",
    );

    let first_refresh = (0..WINDOW_CAPACITY)
        .find_map(|_| match controller.observe_next(sample(10_000, 400, 40_000)) {
            Decision::Prime(proposal) => Some(proposal),
            Decision::Stay => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "a stronger completed-service regime never reopened the underfilled top actuator: {:?}",
                controller.telemetry().reason,
            )
        });
    assert_eq!(
        first_refresh.rung,
        Rung::Uhd,
        "refresh must keep the same wire request"
    );
    assert_eq!(
        first_refresh.direction,
        Direction::Up,
        "refresh spends the exploration budget"
    );

    // The fresh encoder returned the same response. That is neither a permanent structural ban
    // nor permission to allocate another encoder on every segment.
    assert!(controller.reject(
        first_refresh,
        RejectCause::ResponseUnchanged,
        controller.clock_ms(),
    ));
    for _ in 0..8 {
        assert_eq!(
            controller.observe_next(sample(10_000, 400, 40_000)),
            Decision::Stay,
            "unchanged service and disposable reserve polled the same request",
        );
    }

    // Stronger service still cannot identify a hidden response, but the deeper reserve is a
    // strictly larger exact transaction budget. Together they retain the original first-refresh
    // premise and safely release the completed unchanged-response endpoint.
    let second_refresh = (0..WINDOW_CAPACITY)
        .find_map(
            |_| match controller.observe_next(sample(20_000, 400, 80_000)) {
                Decision::Prime(proposal) => Some(proposal),
                Decision::Stay => None,
            },
        )
        .expect("a stronger regime with more disposable reserve must rearm one fresh session");
    assert_eq!(second_refresh.rung, Rung::Uhd);
}

/// A completed same-request refresh can itself be demand-capped. Its transfer rate then measures
/// only the small response PMS chose, not the dormant capacity behind that response, so requiring
/// the live low rendition to confidence-separate above its own fast estimate is an absorbing
/// state. The completed transaction did establish its exact cost, however: a strictly larger
/// disposable reserve can fund one more physical attempt without inventing link capacity.
///
/// Remote-PMS reproduction (2026-08-31): after the link reopened, an underfilled 22 Mbps
/// refresh completed as 1.61 Mbps / 720x404 with E about 14.5 s. The current response then filled
/// the real device queues to about 42 s while reporting 10--16 Mbps body service, but no refresh
/// ever ran because a demand-capped response could not prove a larger hidden service rate.
#[test]
fn an_unchanged_underfilled_response_retries_on_a_strictly_larger_disposable_reserve() {
    let mut controller = Controller::starting_at(Rung::Uhd, None, uhd_catalog());
    controller.observe_active_variant(
        ObservedHlsVariant::new(1_885_000, 720, 404).expect("valid underfilled response"),
        5_000,
    );

    let first = (0..WINDOW_CAPACITY)
        .find_map(
            |_| match controller.observe_next(sample(10_000, 400, 18_000)) {
                Decision::Prime(proposal) => Some(proposal),
                Decision::Stay => None,
            },
        )
        .expect("stronger completed service should authorize the first same-request refresh");
    let first_budget = controller
        .exploration_budget_ms(18_000)
        .expect("the first refresh had a positive disposable reserve");
    assert!(controller.reject(first, RejectCause::ResponseUnchanged, controller.clock_ms(),));

    for _ in 0..8 {
        assert_eq!(
            controller.observe_next(sample(10_000, 400, 18_000)),
            Decision::Stay,
            "the same completed-service regime retried without a larger transaction budget",
        );
    }

    let second = (0..WINDOW_CAPACITY)
        .find_map(|_| match controller.observe_next(sample(10_000, 400, 42_000)) {
            Decision::Prime(proposal) => Some(proposal),
            Decision::Stay => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "a demand-capped response stayed absorbing after reserve grew above {first_budget}ms: {:?}",
                controller.telemetry().reason,
            )
        });
    assert_eq!(second.rung, Rung::Uhd);
    assert_eq!(second.direction, Direction::Up);
    assert!(
        controller
            .exploration_budget_ms(42_000)
            .is_some_and(|budget| budget > first_budget),
        "the retry did not buy genuinely more transaction budget",
    );
}

/// A higher request whose completed PMS response does not improve on the live response is not an
/// ordinal failure of that requested rung. The server answered with a different, demand-capped
/// object, so immediately trying every lower request only allocates a train of physical encoders
/// against the same resource governor. The exact transaction did measure one common disposable-
/// reserve endpoint: hold every quality experiment at that E, then retry the most informative top
/// request once a strictly larger reserve exists.
///
/// Remote-PMS regression (2026-08-31): a 22 Mbps request returned 433 kbps / 720x404 against
/// a live 1.459 Mbps / 720x404 response. Treating that as `Structural` walked 20, 18, 16, 14, 12,
/// 10, 8, 6 and 4 Mbps in one pass; the accumulated encoder churn eventually revoked the live
/// session. Nothing in those response bytes ordered the requested actuators.
#[test]
fn an_underfilled_higher_response_blocks_the_common_budget_not_each_lower_request() {
    let mut controller = Controller::starting_at(Rung::P480, None, hd_catalog());
    let same_budget = || sample(20_000, 500, 10_000); // A=1s, exact E=8s.

    let top = match controller.observe_next(same_budget()) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the first top-response experiment should be funded"),
    };
    assert_eq!(top.rung, Rung::P1080High);
    assert!(controller.reject(top, RejectCause::ResponseUnchanged, controller.clock_ms()));
    assert_eq!(
        controller.hls_exploration_state(),
        HlsExplorationState::CommonBudgetBlocked,
        "the common transaction frontier blocks every untouched HLS request at this exact budget; Original is the remaining informative excitation",
    );

    for _ in 0..8 {
        assert_eq!(
            controller.observe_next(same_budget()),
            Decision::Stay,
            "an untouched lower request laundered the same failed common budget",
        );
    }

    // The failed transaction ended with 3 s less reserve, so merely returning from that endpoint
    // to its original E=8 s has already replaced the drawdown. Adding the same 3 s once more
    // double-counts the debt and can put the frontier above a physically full queue. E=10 s is
    // strictly more disposable reserve than the failed transaction owned and must release it.
    let restored_and_stronger = sample(20_000, 500, 12_000); // Same service, E grows to 10s.
    let retry = match controller.observe_next(restored_and_stronger) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("strictly more physical reserve should reopen one experiment"),
    };
    assert_eq!(
        retry.rung, top.rung,
        "the response gave no ordinal evidence for descending the requested ladder",
    );
}

#[test]
fn a_successful_same_actuator_refresh_becomes_the_observed_top() {
    let mut controller = Controller::starting_at(Rung::Uhd, None, uhd_catalog());
    controller.observe_active_variant(ObservedHlsVariant::new(979_000, 720, 404).unwrap(), 5_000);
    let proposal = (0..WINDOW_CAPACITY)
        .find_map(
            |_| match controller.observe_next(sample(10_000, 400, 80_000)) {
                Decision::Prime(proposal) => Some(proposal),
                Decision::Stay => None,
            },
        )
        .expect("stronger service should refresh the underfilled response");
    assert_eq!(
        proposal.rung,
        controller.current(),
        "this is a refresh, not an upshift"
    );

    let completed = sample(20_000, 400, 78_000);
    let full = ObservedHlsVariant::new(20_895_000, 3_840, 2_160).unwrap();
    assert!(controller.commit_candidate(proposal, completed, full, controller.clock_ms()));
    assert_eq!(
        controller.current(),
        Rung::Uhd,
        "the request actuator never changed"
    );

    assert_eq!(
        controller.observe_next(sample(20_000, 400, 80_000)),
        Decision::Stay
    );
    assert_eq!(
        controller.telemetry().reason,
        Some(DecisionReason::Hls(HlsReason::AtBestRung)),
        "only the observed full response makes the top request terminal",
    );
}

#[test]
fn response_underfill_is_geometry_not_a_bitrate_tolerance() {
    let scope = ObservedHlsVariant::new(4_200_000, 1_918, 802).unwrap();
    assert!(
        !scope.definitively_underfills(Rung::P1080High, (1_918, 802)),
        "a source-sized scope encode satisfies a larger bounding box",
    );

    let fitted_4k = ObservedHlsVariant::new(16_150_000, 1_920, 1_080).unwrap();
    assert!(
        !fitted_4k.definitively_underfills(Rung::P1080High, (3_840, 2_160)),
        "a source larger than the device box is satisfied by reaching the box",
    );
    let short = ObservedHlsVariant::new(979_000, 1_280, 720).unwrap();
    assert!(short.definitively_underfills(Rung::P1080High, (3_840, 2_160)));

    // The declared rate is intentionally absent from the predicate: a ceiling is not a target,
    // and no coefficient-free rule can call two equal rasters at different content complexity
    // underfilled merely because one compressed smaller.
    let compact = ObservedHlsVariant::new(400_000, 1_918, 802).unwrap();
    assert!(!compact.definitively_underfills(Rung::P1080High, (1_918, 802)));
}

/// Once excitation has produced a complete candidate segment which satisfies the real-time
/// boundary law, admission is about that operating point. Old smaller responses are useful
/// rollback evidence, but scaling them up again would discard the only direct answer the
/// transaction just bought.
#[test]
fn one_realtime_candidate_segment_is_direct_admission_evidence() {
    let mut controller = Controller::starting_at(Rung::P1080M6, None, hd_catalog());
    let proposal = (0..WINDOW_CAPACITY)
        .find_map(|_| {
            match controller.observe_next(sample_bytes(1_500_000, 2_000_000, 1_000, 63_000)) {
                Decision::Prime(proposal) => Some(proposal),
                Decision::Stay => None,
            }
        })
        .expect("the reserve should fund an excitation");
    let candidate = sample_bytes(3_500_000, 1_900_000, 950, 61_000);
    assert!(
        controller.candidate_ready(proposal, candidate, 14_000_000),
        "a completed 1.9 s candidate contributes 2 s of media and its post-fetch reserve covers it",
    );
}

/// The exact live failure from the trace: after a successful 16 Mbit/s candidate transaction, the
/// first ordinary fetch was abandoned at 98,304 bytes and 2.759 Mbit/s.  That is a censored
/// deadline failure, not a completed capacity observation.  It may trigger a transactional
/// rollback, but it must not erase the completed acquisition history or turn 2.759 into the target
/// of a 16 -> 2 Mbit/s plunge.
#[test]
fn an_abandoned_live_prefix_is_not_a_capacity_collapse() {
    let mut controller = Controller::starting_at(Rung::P1080M16, None, hd_catalog());
    for _ in 0..6 {
        controller.observe_next(sample_bytes(4_000_000, 1_280_000, 700, 9_300));
    }
    let delivery_before = controller.delivery();
    let window_before = controller.window_len();
    let clock = controller.clock_ms().saturating_add(3_000);
    let prefix = sample_bytes(98_304, 285_000, 1_500, 8_500).abandoned();

    let decision = controller.observe(prefix, clock);

    assert_eq!(
        controller.delivery(),
        delivery_before,
        "censored progress is not capacity"
    );
    assert_eq!(
        controller.window_len(),
        window_before,
        "censored progress is not an acquisition"
    );
    assert!(
        !matches!(decision, Decision::Prime(Proposal { rung, direction: Direction::Down }) if rung <= Rung::P720Low),
        "an abandoned 98 KiB prefix must not select a 2 Mbit/s target: {decision:?}",
    );
}

/// Buffer slope is `delta playable media / delta wall clock`.  Segment duration is the amount of
/// media credited on completion and is not a clock.  The two controllers see the same 1 s reserve
/// loss and differ only in whether it took one or two wall seconds.
#[test]
fn hls_buffer_slope_uses_wall_time_not_media_duration() {
    let mut one_second = Controller::starting_at(Rung::P1080M6, None, hd_catalog());
    one_second.observe(sample(30_000, 200, 10_000), 1_000);
    one_second.observe(sample(30_000, 200, 9_000), 2_000);

    let mut two_seconds = Controller::starting_at(Rung::P1080M6, None, hd_catalog());
    two_seconds.observe(sample(30_000, 200, 10_000), 1_000);
    two_seconds.observe(sample(30_000, 200, 9_000), 3_000);

    assert_eq!(one_second.buffer().slope_ms_per_s, -1_000);
    assert_eq!(two_seconds.buffer().slope_ms_per_s, -500);
}

/// A completed current-point bag is the plant certificate.  If its acquisitions consume more
/// wall time than the media they credit, no amount of starting reserve makes that operating point
/// sustainable: reserve only postpones the loss.  The old eviction path never read this sum and
/// stayed forever when its rate/production heuristics happened to look healthy.
#[test]
fn an_exact_current_point_deficit_requests_a_lower_actuator() {
    let mut controller = Controller::starting_at(Rung::P1080M6, None, hd_catalog());
    let unsustainable = sample_bytes(5_000_000, 1_000_000, 1_050, 60_000);

    assert!(matches!(
        controller.observe_next(unsustainable),
        Decision::Prime(Proposal {
            direction: Direction::Down,
            ..
        })
    ));
}

/// Device regression (remote PMS, 2026-08-31): Auto correctly left Original for an 8 Mbps HLS
/// actuator after the link was shaped to 10 Mbps. The fresh-session object was treated as setup,
/// then the first repeatable object supplied 2.000 s of media in 2.039 s. Its exact finite bag was
/// survivable from the 4 s reserve but not self-replenishing by 39 ms. The controller ignored its
/// already measured delivery/refill model and mapped that epsilon-sized completed deficit to
/// the 320 kbps minimax floor — a rule intended for an ABANDONED fetch with no completed quantum.
///
/// A completed response has enough physical evidence to order a lower actuator: select the
/// highest lower catalog point admitted by the existing conservative delivery and refill
/// equations. The candidate still has to complete and pass the exact transaction law;
/// this assertion adds no dwell, ratio, margin or threshold.
#[test]
fn a_completed_39ms_deficit_recovers_to_the_modeled_lower_actuator() {
    let mut controller = Controller::starting_at(Rung::P1080, None, hd_catalog());
    // 9.157 Mbps of media crossed at 11.532 Mbps active service, matching the device line.
    let segment = || sample_bytes_with_total(2_289_250, 1_588_103, 2_039_000, 2_000, 4_000);

    assert_eq!(
        controller.observe_session_boundary(segment(), Some(2_000), 2_039),
        Decision::Stay,
    );
    let recovery = match controller.observe(segment(), 4_078) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the completed losing operating point needs a lower experiment"),
    };

    let window = controller.telemetry().window.admission.unwrap();
    assert_eq!(window.demand_us, 2_039_000);
    assert_eq!(window.supply_us, 2_000_000);
    assert!(window.survivable);
    assert!(!window.sustainable);
    assert_eq!(recovery.direction, Direction::Down);
    assert_eq!(
        recovery.rung,
        Rung::P1080M6,
        "a completed 39ms deficit was confused with an abandoned no-picture recovery",
    );
}

/// The completed-sample model is an ordering aid, not permission to avoid the bounded recovery
/// floor. If even the smallest catalog point is unsupported by the conservative service evidence,
/// no invented intermediate exists and the original minimax answer remains intact.
#[test]
fn a_completed_collapse_with_no_modeled_lower_point_still_uses_the_floor() {
    let mut controller = Controller::starting_at(Rung::P1080, None, hd_catalog());
    // 16 Mbit delivered over 53.333 s is 300 kbps active service. The 60 s reserve can survive
    // this completed acquisition, but the first-sample conservative budget admits no HLS rung.
    let collapse = sample_bytes_with_total(2_000_000, 53_333_333, 53_333_333, 2_000, 60_000);
    let recovery = match controller.observe(collapse, 53_333) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("a completed collapse must leave the losing operating point"),
    };

    assert_eq!(recovery.direction, Direction::Down);
    assert_eq!(recovery.rung, Rung::P240);
}

/// A completed bag that replenishes itself can still be unable to reach its next completion from
/// the reserve on hand. That is the same time-to-picture emergency as an abandoned fetch: a high
/// measured delivery rate must not turn `B < exact runway` into a quality-preserving experiment.
#[test]
fn an_unsurvivable_completed_bag_keeps_the_minimax_floor() {
    let mut controller = Controller::starting_at(Rung::P1080, None, hd_catalog());
    // A=1.5s <= D=2s, but the exact terminal runway is 1.5s and only B=1s remains. Active body
    // service is 32 Mbps, deliberately high enough that the planning model supports lower rungs.
    let no_runway = sample_bytes_with_total(2_000_000, 500_000, 1_500_000, 2_000, 1_000);
    let recovery = match controller.observe(no_runway, 1_500) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the next credited completion is outside the live reserve"),
    };

    let window = controller.telemetry().window.admission.unwrap();
    assert!(window.sustainable);
    assert!(!window.survivable);
    assert_eq!(recovery.direction, Direction::Down);
    assert_eq!(
        recovery.rung,
        Rung::P240,
        "a runway emergency was weakened into an ordinary modeled downshift",
    );
    assert_eq!(
        controller.telemetry().reason,
        Some(DecisionReason::Hls(HlsReason::BufferConstraint)),
        "the reason must name the runway constraint that selected the emergency branch",
    );
}

/// When both conservation predicates fail, `B < R_o` is the binding constraint: it selects the
/// emergency floor instead of the quality-preserving completed-sample model. Telemetry must name
/// that branch rather than blaming the link-only interpretation of the simultaneous deficit.
#[test]
fn the_runway_constraint_owns_the_reason_when_both_predicates_fail() {
    let mut controller = Controller::starting_at(Rung::P1080, None, hd_catalog());
    // A=3s > D=2s and B=1s < R_o=3s: both predicates fail on the same completed acquisition.
    let exhausted = sample_bytes_with_total(2_000_000, 500_000, 3_000_000, 2_000, 1_000);
    let recovery = match controller.observe(exhausted, 3_000) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("a completed runway emergency must leave the current rung"),
    };

    let window = controller.telemetry().window.admission.unwrap();
    assert!(!window.sustainable);
    assert!(!window.survivable);
    assert_eq!(recovery.rung, Rung::P240);
    assert_eq!(
        controller.telemetry().reason,
        Some(DecisionReason::Hls(HlsReason::BufferConstraint)),
    );
}

/// A shallow buffer is not itself a failure.  If the completed bag costs 200 ms, then 300 ms is
/// enough to reach the next completion even though it is less than the segment's two seconds.
/// `B < D` used to downshift this state despite the exact runway saying it is safe.
#[test]
fn a_shallow_buffer_above_the_exact_runway_does_not_downshift() {
    let mut controller = Controller::starting_at(Rung::P1080M6, None, hd_catalog());
    for _ in 0..2 {
        let decision =
            observe_without_upshift(&mut controller, sample_bytes(500_000, 180_000, 100, 300));
        assert!(
            !matches!(
                decision,
                Decision::Prime(Proposal {
                    direction: Direction::Down,
                    ..
                })
            ),
            "the observed runway is only 200ms; got {decision:?}",
        );
    }
}

/// Acquisitions are denominated in the current operating point.  Keeping a high-rung bag after a
/// downshift makes its old costs look like costs of the cheap recovery stream and can hold the
/// main-thread clock behind an impossible multi-second runway.  Commit must retire it before any
/// next observation; the completed candidate then becomes the sole seed.
#[test]
fn a_downshift_commit_replaces_the_old_operating_point_bag() {
    let mut controller = Controller::starting_at(Rung::P1080M16, None, hd_catalog());
    for _ in 0..3 {
        observe_without_upshift(
            &mut controller,
            sample_bytes(4_000_000, 1_000_000, 700, 9_000),
        );
    }
    assert_eq!(controller.window_len(), 3);

    let down =
        match controller.observe_next(sample_bytes(98_304, 285_000, 1_500, 8_500).abandoned()) {
            Decision::Prime(proposal) => proposal,
            Decision::Stay => panic!("an abandoned live fetch must request rollback"),
        };
    assert_eq!(down.direction, Direction::Down);
    assert!(controller.commit(down, controller.clock_ms()));
    assert_eq!(
        controller.window_len(),
        0,
        "the old rung's acquisitions survived the operating-point change",
    );

    let candidate = sample_bytes(400_000, 180_000, 100, 8_500);
    controller.commit_candidate_evidence(candidate);
    assert_eq!(controller.window_len(), 1);
}

/// With no still-valid lower operating point, recovery must minimize time-to-picture rather than
/// linearly preserve quality.
///
/// Device regression (`pipe_abr_down_staircase`, 2026-08-30): after the link fell from 9.6 Mbps
/// to 500 kbps, the controller paid for 16, 14, 12, 10, 8, 6, 4, 2 and 0.72 Mbps candidates before
/// reaching 0.32 Mbps. Each response was smaller, but every failed control-plane/encoder lifecycle
/// still cost wall time while the playhead was stopped. With response size ordered and the goal
/// defined as the first moving picture, the smallest response is the minimax action; quality is a
/// separate upward search after recovery.
#[test]
fn an_abandoned_active_fetch_recovers_at_the_smallest_actuator() {
    let mut controller = Controller::starting_at(Rung::P1080M18, None, hd_catalog());
    let abandoned = sample(500, 1_000, 4_200).abandoned();

    let recovery = match controller.observe_next(abandoned) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("an abandoned active fetch must start recovery"),
    };
    assert_eq!(recovery.direction, Direction::Down);
    assert_eq!(
        recovery.rung,
        Rung::P240,
        "linear descent makes recovery latency proportional to ladder length",
    );
}

/// The actuator displaced by a just-committed upshift is direct rollback evidence and is tried
/// first. If that transaction fails too, it no longer describes the current service episode and
/// the next recovery action must go to the floor instead of replaying it.
#[test]
fn a_failed_known_good_rollback_continues_at_the_floor() {
    let mut controller = Controller::starting_at(Rung::P480, None, hd_catalog());
    let up = prime_up(&mut controller);
    assert!(controller.commit(up, controller.clock_ms()));
    assert!(up.rung > Rung::P480);

    let abandoned = sample(500, 1_000, 4_200).abandoned();
    let rollback = match controller.observe_next(abandoned) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the displaced working actuator should be the first rollback"),
    };
    assert_eq!(rollback.rung, Rung::P480);
    assert!(controller.reject(rollback, RejectCause::Candidate, controller.clock_ms(),));

    let floor = match controller.observe_next(abandoned) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("a failed rollback still leaves the floor recovery"),
    };
    assert_eq!(floor.rung, Rung::P240);
}

/// A completed-but-unfunded high excitation supplies a response-size endpoint for experiment
/// scheduling, not a reason to pay for every adjacent encoder below it. Bisecting the finite
/// actuator interval minimizes the worst-case number of further transactions and introduces no
/// bitrate or time coefficient. A deadline-censored transaction is covered separately: with no
/// completed response size it blocks the common budget instead of bisecting.
#[test]
fn failed_explorations_narrow_the_remaining_ordinal_interval() {
    let mut controller = Controller::starting_at(Rung::P240, None, hd_catalog());
    let observation = sample(20_000, 500, 10_000); // A=1s, exact E=9s.

    let top = match controller.observe_next(observation) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the initial interval has no upper failure"),
    };
    assert_eq!(top.rung, Rung::P1080High);
    assert!(controller.reject(top, RejectCause::Candidate, controller.clock_ms()));

    let middle = match controller.observe_next(observation) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the lower half of the interval remains unclassified"),
    };
    assert!(
        middle.rung > controller.current() && middle.rung < top.rung,
        "the first endpoint must narrow the interval, got {middle:?} below {top:?}",
    );
    assert!(controller.reject(middle, RejectCause::Candidate, controller.clock_ms(),));

    let lower_middle = match controller.observe_next(observation) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the lower quarter remains unclassified"),
    };
    assert!(
        lower_middle.rung > controller.current() && lower_middle.rung < top.rung,
        "the remaining experiment must stay inside the first endpoint: {lower_middle:?}",
    );
    assert_ne!(lower_middle.rung, middle.rung,
        "the completed current response may strengthen the modeled scheduler, but it may not replay the same failed actuator",
    );
}

/// A demand-capped live response does not prove the link's upper bound, but the controller's
/// conservative delivery/refill model still orders which finite experiment spends the
/// viewer's reserve first.  The remote-device regression (2026-08-31) played a real 8 Mbps HLS
/// response over a settled ~20 Mbps service regime, then repeatedly paid for a 22 Mbps encoder
/// which exhausted its warm-up deadline.  After that real endpoint, `safe_budget` already priced
/// a 14 Mbps intermediate; the live selector erased the endpoint as soon as the reserve grew and
/// unconditionally chose the ladder maximum again.
///
/// This model is only an experiment scheduler.  The selected candidate must still complete its
/// own acquisition and pass the unchanged exact `A <= D && B_post >= A` commit law.
#[test]
fn a_modeled_intermediate_is_explored_before_the_unknown_top() {
    let mut controller =
        Controller::starting_at(Rung::P1080, None, uhd_catalog()).pinned_to(Some(Rung::P1080));
    controller.observe_active_variant(
        ObservedHlsVariant::new(declared_bps(Rung::P1080), 1_920, 1_080).unwrap(),
        20_000,
    );

    // Keep the actuator fixed while the completed current-response samples establish delivery and
    // acquisition evidence. The pin is a measurement tool only; clearing it below enters the
    // ordinary candidate selector without resetting any evidence.
    for _ in 0..4 {
        assert_eq!(
            controller.observe_next(sample(20_000, 300, 5_000)),
            Decision::Stay,
        );
    }
    controller.clear_pin();

    let top = match controller.observe_next(sample(20_000, 300, 5_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the measured reserve should fund one quality experiment"),
    };
    assert_eq!(
        top.rung,
        Rung::Uhd,
        "the first excitation still learns the unknown top"
    );
    assert!(controller.reject(top, RejectCause::Censored, controller.clock_ms()));

    // A deeper reserve releases the exact affordability block. It does not erase the failed top
    // as an ordinal scheduling endpoint; the modeled intermediate is the next useful experiment.
    let proposal = match controller.observe_next(sample(20_000, 300, 7_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("strictly more reserve should fund an intermediate experiment"),
    };
    assert_eq!(proposal.direction, Direction::Up);
    assert_eq!(
        proposal.rung,
        Rung::P1080M14,
        "the live selector ignored its conservative modeled operating point",
    );

    let candidate = sample(20_000, 800, 5_000);
    assert_eq!(
        controller.candidate_verdict(proposal, candidate, declared_bps(proposal.rung)),
        CandidateVerdict::Ready,
    );
    assert!(controller.commit_candidate(
        proposal,
        candidate,
        ObservedHlsVariant::new(14_000_000, 1_920, 1_080).unwrap(),
        controller.clock_ms(),
    ));
    assert_eq!(controller.current(), Rung::P1080M14);
}

/// `candidate_ready` is a controller invariant, not merely a promise made by today's ff caller.
/// A censored prefix can have A<=D and a large post-buffer, but it contains no completed media
/// quantum and cannot seed a newly committed operating point.
#[test]
fn an_abandoned_candidate_can_never_commit() {
    let mut controller = Controller::starting_at(Rung::P480, None, hd_catalog());
    let proposal = prime_up(&mut controller);
    let abandoned = sample(20_000, 400, 12_000).abandoned();
    assert!(!controller.candidate_ready(proposal, abandoned, declared_bps(proposal.rung),));
}

#[test]
fn the_atomic_commit_door_refuses_a_non_dominating_upward_response() {
    let mut controller = Controller::starting_at(Rung::P480, None, hd_catalog());
    let active = ObservedHlsVariant::new(720_000, 854, 480).unwrap();
    controller.observe_active_variant(active, 10_000);
    let proposal = prime_up(&mut controller);
    let completed = sample(20_000, 400, 20_000);

    assert!(!controller.commit_candidate(proposal, completed, active, controller.clock_ms(),));
    assert_eq!(controller.current(), Rung::P480);
    assert_eq!(controller.pending(), Some(proposal));
}

/// A larger reserve can fund a longer experiment; it cannot turn a completed acquisition with
/// `A > D` into a sustainable operating point. Keep that fact distinct from deadline censoring or
/// the controller will retry the same known-losing rung as soon as B grows.
#[test]
fn a_completed_unsustainable_candidate_is_not_released_by_more_buffer() {
    let mut controller = Controller::starting_at(Rung::P480, None, hd_catalog());
    let first = match controller.observe_next(sample(20_000, 500, 6_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the exact current bag funds an excitation"),
    };
    let losing = sample(20_000, 1_100, 6_000); // A=2.2s > D=2s
    assert_eq!(
        controller.candidate_verdict(first, losing, declared_bps(first.rung)),
        CandidateVerdict::Unsustainable,
    );
    assert!(controller.reject(
        first,
        RejectCause::CompletedUnsustainable,
        controller.clock_ms(),
    ));

    let second = match controller.observe_next(sample(20_000, 500, 30_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("a lower unknown actuator remains eligible"),
    };
    assert_ne!(
        second.rung, first.rung,
        "buffer growth incorrectly released a completed A>D certificate",
    );
}

#[test]
fn an_unsustainable_down_candidate_continues_recovery() {
    let mut controller =
        Controller::starting_at(Rung::Uhd, None, uhd_catalog()).pinned_to(Some(Rung::P1080));
    let proposal = match controller.observe_next(sample(40_000, 250, 4_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the down pin should create a candidate transaction"),
    };
    assert_eq!(proposal.direction, Direction::Down);

    let losing = sample(10_000, 1_500, 3_000); // A=3s > D=2s; one object is decodable.
    assert_eq!(
        controller.candidate_verdict(proposal, losing, declared_bps(proposal.rung)),
        CandidateVerdict::Unsustainable,
    );
    assert_eq!(
        controller.candidate_boundary_verdict(proposal, losing, declared_bps(proposal.rung)),
        CandidateVerdict::Unsustainable,
    );
    assert!(controller.reject(
        proposal,
        RejectCause::CompletedUnsustainable,
        controller.clock_ms(),
    ));
    assert_eq!(
        controller.observe_next(sample(10_000, 1_500, 2_000).abandoned()),
        Decision::Prime(Proposal {
            rung: Rung::P240,
            direction: Direction::Down
        }),
    );
}

#[test]
fn an_unsustainable_floor_candidate_is_the_terminal_best_available_rung() {
    let mut controller =
        Controller::starting_at(Rung::P480, None, hd_catalog()).pinned_to(Some(Rung::P240));
    let proposal = match controller.observe_next(sample(40_000, 250, 4_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the floor pin should create a candidate transaction"),
    };
    assert_eq!(
        proposal,
        Proposal {
            rung: Rung::P240,
            direction: Direction::Down
        },
    );

    let losing = sample(10_000, 1_500, 3_000); // A=3s > D=2s; no lower actuator exists.
    assert_eq!(
        controller.candidate_verdict(proposal, losing, declared_bps(proposal.rung)),
        CandidateVerdict::Ready,
        "rejecting the floor would retain an even more expensive losing rung",
    );
    assert_eq!(
        controller.candidate_boundary_verdict(proposal, losing, declared_bps(proposal.rung)),
        CandidateVerdict::Ready,
    );
}

/// A completed `A > D` result is a fact about one actuator under the link/service regime in
/// which it was measured, not a permanent property of that actuator.  Buffer growth alone still
/// says nothing new.  A current-rung delivery distribution whose conservative bound has moved
/// above the old regime's recent estimate is new physical evidence, however, and must authorize
/// one fresh excitation of the failed actuator.  Otherwise a temporary router squeeze leaves the
/// controller at the recovery rung forever even after completed segments demonstrate a different
/// end-to-end service regime.
#[test]
fn a_completed_unsustainable_candidate_rearms_after_stronger_link_evidence() {
    let mut controller = Controller::starting_at(Rung::P480, None, hd_catalog());
    let first = match controller.observe_next(sample(20_000, 500, 6_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the exact current bag funds an excitation"),
    };
    assert!(controller.reject(
        first,
        RejectCause::CompletedUnsustainable,
        controller.clock_ms(),
    ));

    let held = match controller.observe_next(sample(20_000, 500, 30_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("a lower unknown actuator remains eligible"),
    };
    assert_ne!(
        held.rung, first.rung,
        "more reserve on the same measured link must not erase A>D evidence",
    );
    assert!(controller.reject(held, RejectCause::Circumstance, controller.clock_ms(),));

    let retried = match controller.observe_next(sample(100_000, 500, 30_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("a confidence-separated service regime must reopen exploration"),
    };
    assert_eq!(
        retried.rung, first.rung,
        "the old-regime certificate survived direct evidence that the live link changed",
    );
}

/// A fresh PMS encoder's first object is a setup-bearing transaction leg, not yet the
/// repeatable segment service of the rendition it starts. A remote PMS reproduced the distinction
/// on device (2026-08-31): after the shaped link was released, the 22 Mbps request returned real
/// 3840x2160 media, but its first 2 s object took 2464 ms because it included encoder/session
/// startup.  The very next live 12 Mbps object took 1022 ms on the same path.  Classifying the
/// first object as a steady `A>D` result blocked the proved 4K response behind `RejectBackoff`.
///
/// There is no sample-count heuristic here.  Exactly one object has the structural session-boundary
/// bit.  If that complete object leaves enough reserve to fund its exact acquisition, it buys one
/// ordinary observation from the already-running encoder; that ordinary observation still has to
/// satisfy the unchanged `A<=D && B_post>=A` conservation rule.
#[test]
fn a_funded_candidate_session_boundary_buys_one_repeatable_observation() {
    let mut controller = Controller::starting_at(Rung::P720Low, None, uhd_catalog());
    let proposal = match controller.observe_next(sample(50_000, 500, 8_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the healthy current bag must fund a 4K excitation"),
    };

    let setup_bearing = sample(45_689, 1_232, 6_042); // A=2464ms, D=2000ms, B>A
    assert_eq!(
        controller
            .candidate_boundary_verdict(proposal, setup_bearing, declared_bps(proposal.rung),),
        CandidateVerdict::SetupBearing,
    );

    let repeatable = sample(45_689, 511, 7_042); // A=1022ms, D=2000ms, B>A
    assert_eq!(
        controller.candidate_verdict(proposal, repeatable, declared_bps(proposal.rung)),
        CandidateVerdict::Ready,
    );

    let directly_repeatable = sample(45_689, 511, 7_042); // A=1022ms, D=2000ms, B>A
    assert_eq!(
        controller.candidate_boundary_verdict(
            proposal,
            directly_repeatable,
            declared_bps(proposal.rung),
        ),
        CandidateVerdict::Ready,
        "a boundary that already satisfies the steady law must not buy or require another object",
    );

    let setup_unfunded = sample(45_689, 1_232, 2_463); // A=2464ms, D=2000ms, B<A
    assert_eq!(
        controller.candidate_boundary_verdict(
            proposal,
            setup_unfunded,
            declared_bps(proposal.rung),
        ),
        CandidateVerdict::Unfunded,
        "the structural boundary must not manufacture reserve for its ordinary observation",
    );
}

/// A multi-object staged candidate retains the one disposable-reserve endpoint on which the
/// whole transaction started. If its ordinary phase is censored, the same starting surplus cannot
/// immediately buy it again; no uncommitted media was credited to manufacture a second grant.
#[test]
fn a_repeatable_phase_cannot_shrink_the_failed_transactions_budget_frontier() {
    let mut controller = Controller::starting_at(Rung::P720Low, None, uhd_catalog());
    let proposal = match controller.observe_next(sample(20_000, 500, 10_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the initial 8s disposable reserve must fund the candidate"),
    };
    assert!(controller.set_executed_exploration_budget(proposal, 8_000));
    assert!(controller.reject(proposal, RejectCause::Censored, controller.clock_ms()));

    assert_eq!(
        controller.observe_next(sample(20_000, 500, 9_000)),
        Decision::Stay,
        "a 7s disposable reserve replayed the transaction that already owned 8s",
    );
    assert_eq!(
        controller.last_reason(),
        Some(DecisionReason::Hls(HlsReason::RejectBackoff)),
    );
}

/// A raster is a bounding-box refusal, not a blanket ladder failure. A response that does not fit
/// rung j also cannot fit a smaller box, while a larger box may be exactly the remedy.
#[test]
fn a_structural_raster_refusal_keeps_larger_boxes_eligible() {
    let mut controller =
        Controller::starting_at(Rung::P480, None, hd_catalog()).pinned_to(Some(Rung::P720));
    let mid = match controller.observe_next(sample(20_000, 500, 12_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("the pin should request its measured actuator"),
    };
    assert_eq!(mid.rung, Rung::P720);
    assert!(controller.reject(mid, RejectCause::StructuralAtOrBelow, controller.clock_ms(),));

    // Remove only the measurement pin. The controller scope and its structural frontier remain.
    controller.clear_pin();
    let next = match controller.observe_next(sample(20_000, 500, 12_000)) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("a larger raster box remains a valid experiment"),
    };
    assert!(
        next.rung > mid.rung,
        "the raster mask pointed in the wrong direction"
    );
}

/// Failure memory is ordered evidence, not a single replaceable cooldown.  A 20 Mbps failure at
/// E=10 s must survive a later 18 Mbps failure at E=5 s; otherwise E=6 s retries the more expensive
/// experiment on less reserve than it already consumed.
#[test]
fn a_lower_candidate_failure_cannot_erase_a_stronger_higher_failure() {
    let mut controller = Controller::starting_at(Rung::P480, None, hd_catalog());
    let observe_with_budget = |controller: &mut Controller, budget_ms: i64| {
        // Each 500pm sample has A=1000ms and D=2000ms, hence stress runway R_s=1000ms.
        controller.observe_next(sample(20_000, 500, budget_ms + 1_000))
    };

    let top = match observe_with_budget(&mut controller, 10_000) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("E=10s should fund the first excitation"),
    };
    assert_eq!(top.rung, Rung::P1080High);
    assert!(controller.reject(top, RejectCause::Candidate, controller.clock_ms()));

    let lower = match observe_with_budget(&mut controller, 5_000) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("a blocked top must not mask a cheaper unknown rung"),
    };
    assert!(lower.rung < top.rung);
    assert!(controller.reject(lower, RejectCause::Candidate, controller.clock_ms()));

    let retry = match observe_with_budget(&mut controller, 6_000) {
        Decision::Prime(proposal) => proposal,
        Decision::Stay => panic!("E=6s may retry the 5s failure or another cheaper rung"),
    };
    assert_ne!(
        retry.rung, top.rung,
        "the 10s top failure was erased by the later 5s lower failure",
    );
}

/// **The `pipe_abr_reject_up_4000` device leg, replayed against the shipped conservation law.**
///
/// The pipeline case declares a flat 8 300 kbps leg and asserts `settle_max_kbps: 4000`, on the
/// derivation in its own `_reject_note`: "`candidate_prime_budget` gives the candidate 4/5 of one
/// media duration, 1600 ms … rung 6000's warm-up costs 1718 ms and overruns its budget, so every
/// proposal is rejected". That budget is the historical `4/5·d` transaction deadline
/// (`docs/adaptive-playback-plan.md:945`). It is not the shipped admission rule, and its
/// single-observation form `A <= 0.8·D` is listed among the three tests
/// `docs/adaptive-playback-spec.md` §4 REMOVED — "a bare 800 … refuted at ~37%". The shipped
/// contract (`docs/adaptive-playback.md`, "A completed upward candidate is accepted exactly when
/// `A <= D and B_post >= A`") admits on the media duration itself, and
/// `candidate_prime_budget` is `#[allow(dead_code)]` compatibility that the live path never calls.
///
/// So this test pins what the device measured, not what the note predicted. Every number below is
/// read out of `pipe_abr_reject_up_4000.log` (2026-09-01, webOS 4.10.0, the shaped 8 300 kbps leg):
/// the 4000->6000 boundary object acquired 2 000 ms of media in 1 950 ms with 4 709 ms of reserve
/// behind it, and rung 6000 then ran 33 further objects at `demand=62926ms supply=70000ms`
/// (`A/D = 0.899`) with a WORST acquisition of 1 983 ms, a reserve climbing to 13 710 ms and no
/// stall. Rung 6000 is sustainable on that link; refusing it would be refusing an operating point
/// the evidence proves, which is exactly the failure mode §4's removed `0.8` test produced.
///
/// The two proposals the same run DID refuse are pinned beside it, because they are what makes the
/// admission path discriminating rather than permissive.
///
/// Two artifacts in this repository already say the same thing without any of the above.
/// [`lg_network_legs_settle_on_sustainable_rungs`] asserts that a flat **7 000** kbps plant settles
/// on rung **6000**, and `tests/manifest.json`'s own `pipe_abr_steady_modest_link` permits rung
/// 6000 on a flat **6 000** kbps leg — so a rule that refuses rung 6000 at **8 300** kbps makes a
/// faster link settle lower than a slower one, and breaks both.
///
/// # Why re-tuning the leg rate does not rescue the case either
///
/// Under the shipped rule a flat leg cannot both PROPOSE rung 6000 and reliably REFUSE it, and the
/// reason is structural rather than a matter of picking a better number. From the fixture's own
/// medians on this run (1 093 596 B at rung 4000, 1 571 868 B at rung 6000) and the case's measured
/// 272 ms fixed cost, refusing 6000 needs `12575/C + 272 > 2000`, i.e. `C < 7277` kbps; while
/// proposing it at all needs the conservative estimate (`0.8 * slow`) to reach 6000, i.e.
/// `C >= 7500` kbps. The window is empty, and it is empty BY DESIGN:
/// `docs/adaptive-playback-spec.md` §4 requires that "selection and admission must evaluate the
/// same rule or the controller livelocks". A rejected upshift is therefore reachable only from a
/// stimulus in which the link, the encoder or the rendition changes BETWEEN the proposal and the
/// candidate fetch — which is exactly what this run's two real refusals (rungs 8000 and 12000,
/// pinned below) are, and what its `E_tx(up, reject)` evidence comes from.
#[test]
fn the_measured_8300kbps_leg_admits_rung_6000_and_still_refuses_8000_and_12000() {
    // A candidate object exactly as the device stamped it: wire bytes, the body transfer implied
    // by the run's own `net=` reading, and the acquisition `ff.rs` reports as `candidate_acq=`.
    let device_object = |bytes: u64, net_kbps: u64, total_ms: u64, buffered_ms: i64| {
        let active_us = bytes * 8_000 / net_kbps;
        sample_bytes_with_total(bytes, active_us, total_ms * 1_000, 2_000, buffered_ms)
    };
    let propose_up_to = |rung: Rung| {
        let mut controller =
            Controller::starting_at(Rung::P720, None, hd_catalog()).pinned_to(Some(rung));
        let proposal = match controller.observe_next(sample(20_000, 300, 14_000)) {
            Decision::Prime(proposal) => proposal,
            Decision::Stay => panic!("the pinned rung must create one candidate transaction"),
        };
        assert_eq!(proposal.direction, Direction::Up);
        assert_eq!(proposal.rung, rung);
        (controller, proposal)
    };

    // `abr: tx Up 4000->6000kbps outcome=committed … warmup=1950ms buf_decided=4709ms
    //  candidate_bytes=1729224 candidate_dur=2000ms net=7452kbps`
    let (controller, proposal) = propose_up_to(Rung::P1080M6);
    let boundary = device_object(1_729_224, 7_452, 1_950, 4_709);
    assert_eq!(boundary.media_duration_ms(), 2_000);
    assert!(
        boundary.total_fetch_us() <= u64::from(boundary.media_duration_ms()) * 1_000,
        "the device boundary object acquired 2000ms of media in 1950ms",
    );
    assert_eq!(
        controller.candidate_boundary_verdict(proposal, boundary, declared_bps(proposal.rung)),
        CandidateVerdict::Ready,
        "A=1950ms <= D=2000ms with B_post=4709ms >= A is the shipped acceptance law",
    );

    // The first ORDINARY object of the same encoder, one segment later on the live cursor:
    // `hls: segment=1 bytes=1731856 … total_ms=1923` at `net=7711kbps`. The boundary object was
    // the DEARER of the two, which is what makes deciding on it sound rather than lucky.
    let repeatable = device_object(1_731_856, 7_711, 1_923, 6_793);
    assert!(repeatable.total_fetch_us() < boundary.total_fetch_us());
    assert_eq!(
        controller.candidate_verdict(proposal, repeatable, declared_bps(proposal.rung)),
        CandidateVerdict::Ready,
        "the repeatable cadence of rung 6000 on this leg is also inside D",
    );

    // `abr: window current=6000kbps … demand=62926ms supply=70000ms` after 35 objects, and the
    // slowest single acquisition the whole rung produced was 1983ms. Both are strictly inside the
    // conservation law, so the commit above is the correct verdict on this link and not a miss.
    assert!(62_926 <= 70_000, "the settled rung-6000 episode is sustainable");
    let worst = device_object(1_729_224, 7_452, 1_983, 6_793);
    assert_eq!(
        controller.candidate_verdict(proposal, worst, declared_bps(proposal.rung)),
        CandidateVerdict::Ready,
        "even the worst measured rung-6000 acquisition fits inside its own media duration",
    );

    // `abr: tx Up 4000->8000kbps outcome=repeatable_deadline … warmup=2555ms buf_start=5751ms`,
    // logged as `abr: setup-bearing candidate retains one staged observation budget=1126ms`.
    let (controller, proposal) = propose_up_to(Rung::P1080);
    let boundary_8000 = device_object(2_229_680, 7_702, 2_555, 3_184);
    assert_eq!(
        controller.candidate_boundary_verdict(proposal, boundary_8000, declared_bps(proposal.rung)),
        CandidateVerdict::SetupBearing,
        "A=2555ms > D funded by B_post=3184ms buys exactly one ordinary observation",
    );

    // `abr: tx Up 4000->12000kbps outcome=not_ready_discarded … warmup=3640ms graded=3826ms`,
    // logged as `abr: candidate verdict=Unsustainable; discarded 2 staged segment(s)`.
    let (controller, proposal) = propose_up_to(Rung::P1080M12);
    let boundary_12000 = device_object(3_225_140, 7_748, 3_640, 8_736);
    assert_eq!(
        controller.candidate_boundary_verdict(
            proposal,
            boundary_12000,
            declared_bps(proposal.rung),
        ),
        CandidateVerdict::SetupBearing,
    );
    let graded_12000 = device_object(3_383_436, 7_748, 3_826, 4_751);
    assert_eq!(
        controller.candidate_verdict(proposal, graded_12000, declared_bps(proposal.rung)),
        CandidateVerdict::Unsustainable,
        "the ordinary object of a 12000 candidate loses 1826ms of reserve per segment",
    );

    // And the upshift the same run committed one rung earlier, which any change to the admission
    // threshold has to keep: `abr: tx Up 2000->4000kbps outcome=committed … warmup=1228ms`.
    let (controller, proposal) = {
        let mut controller =
            Controller::starting_at(Rung::P720Low, None, hd_catalog()).pinned_to(Some(Rung::P720));
        let proposal = match controller.observe_next(sample(20_000, 300, 14_000)) {
            Decision::Prime(proposal) => proposal,
            Decision::Stay => panic!("the pinned rung must create one candidate transaction"),
        };
        (controller, proposal)
    };
    let sustainable = device_object(1_093_596, 7_865, 1_228, 9_584);
    assert_eq!(
        controller.candidate_boundary_verdict(proposal, sustainable, declared_bps(proposal.rung)),
        CandidateVerdict::Ready,
    );
}
