use super::*;

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

fn sample(network_kbps: u32, ratio_pm: u32, buffered_ms: i64) -> SegmentSample {
    let media_ms = 2_000;
    let total_us = u64::from((media_ms * ratio_pm / 1_000).max(1)) * 1_000;
    let active_us = total_us.min(200_000);
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
        false,
    )
    .unwrap()
}

/// Bytes that make one 750 ms window measure exactly `kbps`.
fn window_bytes(kbps: u64) -> u64 {
    kbps * ORIGINAL_WINDOW_US / 8_000
}

const HOUR_MS: i64 = 3_600_000;

fn prime_up(controller: &mut Controller) -> Proposal {
    for _ in 0..4 {
        if let Decision::Prime(proposal) = controller.observe(sample(20_000, 200, 10_000)) {
            return proposal;
        }
    }
    panic!("no proposal")
}

fn settle_link(network_kbps: u32) -> Rung {
    let mut controller = bootstrap_controller();
    for _ in 0..80 {
        if let Decision::Prime(proposal) =
            controller.observe(sample(network_kbps, 400, 10_000))
        {
            let candidate = sample(network_kbps, 400, 12_000);
            if controller.candidate_ready(proposal, candidate) {
                controller.commit(proposal);
            } else {
                controller.reject(proposal);
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
    assert_eq!(b.buffered_ms(), 0);
    b.audio_expected = false;
    assert_eq!(b.buffered_ms(), 4_000);
}

/// A window shorter than the measurement window is not a measurement.
#[test]
fn original_windows_shorter_than_the_measurement_window_are_not_evidence() {
    let mut mode = original(28_000);
    assert!(mode
        .observe(window_bytes(4_000), ORIGINAL_WINDOW_US - 1, Some(3_000), HOUR_MS)
        .is_none());
    let first = mode
        .observe(window_bytes(4_000), ORIGINAL_WINDOW_US, Some(3_000), HOUR_MS)
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
            .observe(
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
#[test]
fn a_collapse_leaves_original_for_the_best_sustainable_state() {
    let mut mode = original(28_000);
    let observation = mode
        .observe(window_bytes(4_000), ORIGINAL_WINDOW_US, Some(8_000), HOUR_MS)
        .unwrap();
    assert_eq!(observation.fallback, Some(OriginalExit::ImminentStarvation));
    assert_eq!(observation.target, Some(Rung::P720Low), "3.2 Mbit/s of proven capacity");
}

/// A moderate deficit that will not go away eventually loses the argument on its own — before
/// starvation is imminent, and with no counter deciding anything by itself.
#[test]
fn a_deficit_that_persists_costs_original_the_argument() {
    let mut mode = original(60_000);
    let mut exits = Vec::new();
    for window in 1..=14 {
        let observation = mode
            .observe(
                window_bytes(50_000) * window,
                ORIGINAL_WINDOW_US * window,
                Some(30_000),
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
            .observe(
                window_bytes(60_000) * window,
                ORIGINAL_WINDOW_US * window,
                Some(5_000),
                HOUR_MS,
            )
            .unwrap();
        assert!(healthy.fallback.is_none());
    }
    assert_eq!(
        mode.observe(
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
        .observe(
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
        .observe(window_bytes(1_000), ORIGINAL_WINDOW_US, Some(20_000), 15_000)
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
        mode.observe(
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
    assert_eq!(mode.deficit_windows, 0);
    // ...and the counters really did rewind, so the next window measures from zero rather than
    // reading a negative delta as a collapse.
    assert!(mode
        .observe(window_bytes(50_000), ORIGINAL_WINDOW_US, Some(1_000), HOUR_MS)
        .is_some());
}

/// A pause is the one gap where wall-clock time passes with nothing measured.
#[test]
fn a_long_pause_turns_the_estimate_into_a_weak_prior() {
    let mut mode = original(28_000);
    for window in 1..=4 {
        mode.observe(
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
        assert_eq!(controller.observe(sample(20_000, 1_000, 10_000)), Decision::Stay);
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
    assert!(controller.candidate_ready(proposal, sample(20_000, 200, 12_000)));
    assert!(controller.commit(proposal));
    assert_eq!(controller.current(), Rung::P1080M12);
}

#[test]
fn rejected_candidate_preserves_current_and_clears_pending() {
    let mut controller = bootstrap_controller();
    let proposal = prime_up(&mut controller);
    assert!(!controller.candidate_ready(proposal, sample(2_100, 950, 12_000)));
    assert!(controller.reject(proposal));
    assert_eq!(controller.current(), Rung::P480);
    assert_eq!(controller.pending(), None);
}

#[test]
fn startup_does_not_issue_back_to_back_encoder_swaps() {
    let mut controller = bootstrap_controller();
    let proposal = prime_up(&mut controller);
    controller.commit(proposal);
    for _ in 0..3 {
        assert_eq!(controller.observe(sample(20_000, 200, 12_000)), Decision::Stay);
    }
}

/// One slow sample is acted on immediately — a downshift is an invisible transaction and the
/// alternative is a stall — but it is acted on CONSERVATIVELY: a single measurement carries the
/// maximum discount, so 1 Mbit/s is treated as 0.5 Mbit/s of proven capacity and the target is
/// the emergency floor rather than the rung just below. The next agreeing samples are what buy
/// the way back up.
#[test]
fn a_single_slow_network_sample_jumps_to_the_measured_sustainable_rung() {
    let mut controller = bootstrap_controller();
    controller.current = Rung::P720;
    let decision = controller.observe(sample(1_000, 400, 8_000));
    assert_eq!(
        decision,
        Decision::Prime(Proposal { rung: Rung::P240, direction: Direction::Down })
    );
    assert_eq!(controller.current(), Rung::P720);
}

#[test]
fn a_runtime_collapse_from_the_top_does_not_prime_oversized_intermediate_rungs() {
    let mut controller = bootstrap_controller();
    controller.current = Rung::P1080High;
    assert_eq!(
        controller.observe(sample(512, 1_000, 8_000)),
        Decision::Prime(Proposal { rung: Rung::P240, direction: Direction::Down })
    );
}

#[test]
fn draining_jit_session_downshifts_but_stable_jit_does_not() {
    let mut controller = bootstrap_controller();
    assert_eq!(controller.observe(sample(20_000, 1_200, 8_000)), Decision::Stay);
    assert_eq!(controller.observe(sample(20_000, 1_200, 6_000)), Decision::Stay);
    assert_eq!(controller.observe(sample(20_000, 1_200, 4_000)), Decision::Stay);
    assert_eq!(controller.observe(sample(20_000, 1_200, 3_000)), Decision::Stay);
    assert_eq!(controller.observe(sample(20_000, 1_200, 2_500)), Decision::Stay);
    assert_eq!(controller.observe(sample(20_000, 1_200, 2_000)),
        Decision::Prime(Proposal { rung: Rung::P240, direction: Direction::Down }));
}

fn recovery(source_kbps: u32) -> OriginalRecovery {
    OriginalRecovery::new(
        source_kbps,
        AbrPolicy::measured(),
        false,
        TransitionHistory::default(),
    )
    .unwrap()
}

/// The HLS session the recovery decision is compared AGAINST — healthy, since that is the
/// interesting case: a starved one would make Original win by default.
fn healthy_hls() -> CapacityEstimate {
    CapacityEstimate::from_prior(30_000)
}

fn healthy_buffer() -> BufferEstimate {
    BufferEstimate { buffered_ms: 12_000, slope_ms_per_s: 0, ..Default::default() }
}

fn probe(kbps: u32, completed: bool) -> CapacityObservation {
    CapacityObservation { kbps, bytes: 2_000_000, active_us: 400_000, completed }
}

/// **A mid-ladder rung with spare capacity may probe.** The old gate required the TOP rung,
/// which measured the wrong resource: PMS producing 20 Mbit/s of H.264 says the server can
/// encode, not that the link can carry a 28 Mbit/s remux.
#[test]
fn original_recovery_probes_from_any_rung_with_measured_headroom() {
    let mut gate = recovery(28_000);
    let current = hd_catalog().candidate(Rung::P720);
    let spare = CapacityEstimate::from_prior(30_000);
    for n in 1..=ORIGINAL_PROBE_SPACING {
        assert_eq!(
            gate.probe_due(current, sample(20_000, 500, 10_000), healthy_buffer(), &spare, HOUR_MS),
            n == ORIGINAL_PROBE_SPACING,
            "spacing window {n}",
        );
    }
}

/// No measurable headroom, a thin reserve, or a draining one: no probe, whatever the rung. A
/// probe reads real bytes over the link the segments need.
#[test]
fn original_recovery_refuses_to_probe_without_room_to_do_it_safely() {
    let current = hd_catalog().candidate(Rung::P1080High);
    let spare = CapacityEstimate::from_prior(60_000);
    let no_headroom = CapacityEstimate::from_prior(20_011);
    for _ in 0..ORIGINAL_PROBE_SPACING * 2 {
        assert!(
            !recovery(28_000).probe_due(
                current,
                sample(60_000, 500, 10_000),
                healthy_buffer(),
                &no_headroom,
                HOUR_MS,
            ),
            "segments prove a LOWER bound; at the wire rate there is no evidence of more",
        );
        assert!(
            !recovery(28_000).probe_due(
                current,
                sample(60_000, 500, 2_000),
                healthy_buffer(),
                &spare,
                HOUR_MS,
            ),
            "one segment of reserve is not room to spend on a measurement",
        );
        assert!(
            !recovery(28_000).probe_due(
                current,
                sample(60_000, 500, 10_000),
                BufferEstimate { buffered_ms: 12_000, slope_ms_per_s: -400, ..Default::default() },
                &spare,
                HOUR_MS,
            ),
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
        decisive.observe_probe(probe(80_000, true), healthy_buffer(), &healthy_hls(), HOUR_MS),
        RecoveryVerdict::Recover,
        "80 Mbit/s leaves nothing for a second probe to add",
    );

    let mut marginal = recovery(28_000);
    let verdicts: Vec<RecoveryVerdict> = (0..3)
        .map(|_| marginal.observe_probe(probe(50_000, true), healthy_buffer(), &healthy_hls(), HOUR_MS))
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
        gate.observe_probe(probe(2_000, false), healthy_buffer(), &healthy_hls(), HOUR_MS),
        RecoveryVerdict::Insufficient,
    );
    assert_eq!(
        gate.observe_probe(probe(80_000, true), healthy_buffer(), &healthy_hls(), HOUR_MS),
        RecoveryVerdict::Recover,
        "the aborted attempt left no trace to drag the estimate down",
    );
}

/// The benefit of Original accrues over the remaining playback; the reload is paid once, now.
#[test]
fn recovery_does_not_pay_for_a_reload_at_the_end_of_a_film() {
    let mut gate = recovery(28_000);
    assert_eq!(
        gate.observe_probe(probe(80_000, true), healthy_buffer(), &healthy_hls(), 8_000),
        RecoveryVerdict::NotWorthIt,
    );
    let current = hd_catalog().candidate(Rung::P720);
    let spare = CapacityEstimate::from_prior(80_000);
    for _ in 0..ORIGINAL_PROBE_SPACING * 2 {
        assert!(
            !recovery(28_000).probe_due(
                current,
                sample(20_000, 500, 10_000),
                healthy_buffer(),
                &spare,
                8_000,
            ),
            "and it does not spend a probe finding that out",
        );
    }
}

#[test]
fn a_downshift_holds_long_enough_to_avoid_immediate_top_rung_flapping() {
    let mut controller = controller_at(Rung::P1080High);
    let Decision::Prime(down) = controller.observe(sample(12_000, 500, 8_000)) else {
        panic!("the collapsed link must propose a downshift")
    };
    assert!(controller.commit(down));
    for _ in 0..9 {
        assert_eq!(controller.observe(sample(60_000, 400, 10_000)), Decision::Stay);
    }
    assert_eq!(
        controller.observe(sample(60_000, 400, 10_000)),
        Decision::Prime(Proposal { rung: Rung::P1080High, direction: Direction::Up }),
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

    // End to end: the floor rung on a fast LAN climbs instead of parking.
    let mut controller = controller_at(Rung::P240);
    let mut reached = Rung::P240;
    for _ in 0..40 {
        let segment = sample_bytes(80_000, 700, 400, 12_000);
        if let Decision::Prime(proposal) = controller.observe(segment) {
            if controller.candidate_ready(proposal, sample(20_000, 400, 12_000)) {
                controller.commit(proposal);
                reached = controller.current();
            } else {
                controller.reject(proposal);
            }
        }
    }
    assert!(reached > Rung::P240, "a LAN must not leave Auto on the emergency floor");
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
            .best_sustainable(60_000, &quick_server, current, &policy)
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
            .best_sustainable(60_000, &loaded_server, current, &policy)
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
#[test]
fn a_budget_jump_skips_the_intermediate_encoders() {
    let mut controller = controller_at(Rung::P1080);
    let mut proposal = None;
    for _ in 0..6 {
        if let Decision::Prime(p) = controller.observe(sample(22_000, 200, 12_000)) {
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
    let up = Proposal { rung: Rung::P720Low, direction: Direction::Up };
    let down = Proposal { rung: Rung::P240, direction: Direction::Down };
    assert_eq!(
        candidate_prime_budget(up, media),
        Some(std::time::Duration::from_micros(1_601_600))
    );
    assert_eq!(
        candidate_warmup_budget(up, media),
        Some(std::time::Duration::from_micros(3_003_000))
    );
    assert_eq!(candidate_prime_budget(down, media), None);
    assert_eq!(candidate_warmup_budget(down, media), None);
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

/// Throughput is a RATE, so the size and duration of the transfer decide how much it proves.
#[test]
fn observation_quality_weights_a_tiny_read_below_a_sustained_one() {
    let tiny = CapacityObservation { kbps: 100_000, bytes: 40_000, active_us: 3_000, completed: true };
    let normal = CapacityObservation { kbps: 20_000, bytes: 400_000, active_us: 160_000, completed: true };
    let sustained = CapacityObservation { kbps: 20_000, bytes: 4_000_000, active_us: 1_600_000, completed: true };
    let truncated = CapacityObservation { completed: false, ..sustained };
    assert_eq!(tiny.quality(), ObservationQuality::Weak);
    assert_eq!(normal.quality(), ObservationQuality::Normal);
    assert_eq!(sustained.quality(), ObservationQuality::Strong);
    assert_eq!(truncated.quality(), ObservationQuality::Weak, "a truncated read proves a floor");
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
    buffer.update(12_000, 2_000);
    for step in 1..=3 {
        buffer.update(12_000 - step * 20, 2_000);
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
    seeded.observe(sample(4_000, 400, 10_000));
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
        false,
        TransitionHistory::default(),
    )
    .unwrap();
    let flapping = OriginalRecovery::new(
        28_000,
        AbrPolicy::measured(),
        false,
        TransitionHistory { visible_switches: 5, since_last_ms: Some(2_000) },
    )
    .unwrap();
    let good = probe(90_000, true);
    let mut calm = calm;
    let mut flapping = flapping;
    assert_eq!(
        calm.observe_probe(good, healthy_buffer(), &healthy_hls(), 600_000),
        RecoveryVerdict::Recover,
    );
    assert_eq!(
        flapping.observe_probe(good, healthy_buffer(), &healthy_hls(), 600_000),
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
        assert!(matches!(pinned.observe(sample(40_000, 300, 30_000)), Decision::Stay),
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

    let mut at_once = OriginalRecovery::new(28_000, policy, false, flapping).unwrap();
    assert_eq!(
        at_once.observe_probe(good, healthy_buffer(), &healthy_hls(), 600_000),
        RecoveryVerdict::NotWorthIt,
        "the fifth switch two seconds ago is still expensive",
    );

    let mut later = OriginalRecovery::new(28_000, policy, false, flapping).unwrap();
    // Six half-lives, so the penalty is under 2% of its opening value. The RATE is policy and is
    // not under test here; that the clock advances at all is.
    later.advance_to(policy.visible_switch_decay_ms.saturating_mul(6));
    assert_eq!(
        later.observe_probe(good, healthy_buffer(), &healthy_hls(), 600_000),
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

    let mut once = OriginalRecovery::new(28_000, policy, false, flapping).unwrap();
    once.advance_to(target);

    let mut many = OriginalRecovery::new(28_000, policy, false, flapping).unwrap();
    for _ in 0..40 {
        many.advance_to(target);
    }
    assert_eq!(
        once.observe_probe(good, healthy_buffer(), &healthy_hls(), 600_000),
        many.observe_probe(good, healthy_buffer(), &healthy_hls(), 600_000),
        "forty ticks to the same instant is one tick to that instant",
    );
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
        .observe(window_bytes(41_000), ORIGINAL_WINDOW_US, Some(30_000), HOUR_MS)
        .unwrap();
    assert!(observation.horizon_secs.is_some(), "a bare average is not headroom");
    assert!(observation.fallback.is_none(), "but 30 s of reserve is not an emergency either");
}

/// Utility is not a bitrate comparison: Original wins from BEHIND on wire rate because it has
/// no generation loss and asks the server for no video encoding at all.
#[test]
fn original_beats_the_top_rung_on_utility_at_equal_risk() {
    let policy = AbrPolicy::measured();
    let inputs = ModeInputs {
        current: ModeKind::Original,
        source_kbps: 28_000,
        source_delivery: CapacityEstimate::from_prior(80_000),
        hls_delivery: CapacityEstimate::from_prior(80_000),
        production: ProductionEstimate::default(),
        buffer: BufferEstimate { buffered_ms: 30_000, ..Default::default() },
        remaining_ms: HOUR_MS,
        history: TransitionHistory::default(),
        original_feasible: true,
        original_features: false,
        persistent_deficit_windows: 0,
    };
    let current = hd_catalog().candidate(Rung::P1080High);
    let (mode, reason, chosen, other) =
        choose_mode(&inputs, current, Some(current), &policy);
    assert_eq!((mode, reason), (ModeKind::Original, ModeReason::OriginalWorthIt));
    assert_eq!(chosen.server, 0, "no server video encoding is the term HLS cannot match");
    assert!(chosen.total > other.expect("both were feasible").total);

    // Infeasible is not a low score — it is not a candidate.
    let (mode, reason, _, other) = choose_mode(
        &ModeInputs { original_feasible: false, ..inputs },
        current,
        Some(current),
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
/// `candidate_prime_budget` + `candidate_ready`'s residual — and both of those budgets return
/// `None` for `Direction::Down`. Six segments is 12 000 ms at `D = 2000`, while the reachable
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
        .map(|_| pinned.observe(sample(40_000, 300, reserve_ms)))
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
            matches!(pinned.observe(sample(40_000, 300, reserve_ms)), Decision::Stay),
            "an upshift pin transacted on a reserve smaller than the transaction costs, which is \
             the livelock PIN_MIN_RESERVE_SEGMENTS exists to prevent",
        );
    }
}
