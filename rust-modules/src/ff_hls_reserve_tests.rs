//! HLS reserve-deadline and transport-liveness classification: pause/rebuffer
//! projections, quality-trial gating, and candidate transition disposition.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use crate::abr::Direction;

#[test]
fn fractional_hls_duration_is_rounded_up_as_a_reserve_obligation() {
    assert_eq!(
        hls_duration_obligation_ms(std::time::Duration::from_micros(551_044)),
        Some(552),
    );
    assert_eq!(
        hls_duration_obligation_ms(std::time::Duration::from_millis(2_000)),
        Some(2_000),
    );
}

#[test]
fn a_terminal_floor_response_cannot_loop_only_because_pms_exceeded_its_box() {
    let floor = crate::abr::Proposal {
        rung: crate::abr::Rung::P240,
        direction: Direction::Down,
    };
    assert!(!hls_candidate_requires_rung_box(
        floor,
        crate::abr::ReservePolicy::TerminalFloor,
    ));
    assert!(hls_candidate_requires_rung_box(
        floor,
        crate::abr::ReservePolicy::Preserve,
    ));
}

#[test]
fn a_quality_trial_never_extends_an_active_rebuffer_hold() {
    assert!(hls_quality_trial_may_start(
        Direction::Up,
        false,
        false,
        false
    ));
    assert!(!hls_quality_trial_may_start(
        Direction::Up,
        true,
        false,
        false
    ));
    assert!(!hls_quality_trial_may_start(
        Direction::Up,
        false,
        true,
        false
    ));
    assert!(!hls_quality_trial_may_start(
        Direction::Up,
        true,
        true,
        false
    ));
}

#[test]
fn an_abort_at_the_exact_wait_boundary_wins_over_reserve_expiry() {
    let mut aq = crate::aq::aq_new(1 << 20);
    crate::aq::aq_abort(&mut *aq);
    let result = hls_wait(
        &mut *aq,
        std::time::Duration::ZERO,
        Some(std::time::Instant::now()),
    );
    assert!(matches!(result, Err(HlsExit::Aborted)));
    crate::aq::aq_destroy(&mut *aq);
}

#[test]
fn a_downshift_remains_available_because_it_is_the_recovery_edge() {
    assert!(hls_quality_trial_may_start(
        Direction::Down,
        true,
        true,
        false
    ));
}

#[test]
fn teardown_wins_over_an_expired_plaintext_open_snapshot() {
    assert!(matches!(
        classify_plaintext_open_failure(crate::stream::HttpOpenError::Aborted),
        HlsExit::Aborted
    ));
}

#[test]
fn classify_curl_open_err_matches_its_plaintext_sibling() {
    assert!(matches!(
        classify_curl_open_err(crate::curlio::OpenErr::Deadline),
        HlsExit::PrimeExpired
    ));
    assert!(matches!(
        classify_curl_open_err(crate::curlio::OpenErr::Aborted),
        HlsExit::Aborted
    ));
    assert!(matches!(
        classify_curl_open_err(crate::curlio::OpenErr::Status(404)),
        HlsExit::NotReady
    ));
    SHARED.dg_http_status.store(0, Ordering::Relaxed);
    assert!(matches!(
        classify_curl_open_err(crate::curlio::OpenErr::Status(500)),
        HlsExit::Failed(_)
    ));
    assert_eq!(
        SHARED.dg_http_status.load(Ordering::Relaxed),
        500,
        "a non-404 status is still recorded for diagnostics"
    );
    assert!(matches!(
        classify_curl_open_err(crate::curlio::OpenErr::Local),
        HlsExit::Failed(_)
    ));
}

#[test]
fn every_ffmpeg_phase_reduces_callback_facts_with_one_priority_table() {
    let transfer = SegmentTransfer {
        bytes: 123,
        active_us: 456,
        total_us: 789,
        audio_expected: true,
    };
    assert!(matches!(
        classify_hls_avio_facts(Some(transfer), true, true, true),
        Some(HlsExit::StallAbort(observed)) if observed == transfer
    ));
    assert!(matches!(
        classify_hls_avio_facts(None, true, true, true),
        Some(HlsExit::PrimeExpired)
    ));
    assert!(matches!(
        classify_hls_avio_facts(None, false, true, true),
        Some(HlsExit::Failed("segment body transport failed"))
    ));
    assert!(
        classify_hls_avio_facts(None, false, true, false).is_none(),
        "libavformat recovery clears a callback error instead of inventing a terminal",
    );
}

#[test]
fn rejected_candidate_media_has_only_the_discard_transition() {
    assert_eq!(
        hls_candidate_disposition(true, true, crate::abr::CandidateVerdict::Unfunded,),
        HlsCandidateDisposition::Discard {
            cause: crate::abr::RejectCause::Candidate,
            outcome: "not_ready_discarded",
        },
    );
    assert_eq!(
        hls_candidate_disposition(true, true, crate::abr::CandidateVerdict::Ready),
        HlsCandidateDisposition::Commit,
    );
}

#[test]
fn a_user_pause_fills_the_active_queues_without_starting_a_private_trial() {
    assert!(!hls_quality_trial_may_start(
        Direction::Up,
        false,
        false,
        true
    ));
    assert!(!hls_quality_trial_may_start(
        Direction::Down,
        false,
        false,
        true
    ));
}

#[test]
fn a_latched_original_switch_preempts_an_unstarted_hls_prime() {
    let proposal = crate::abr::Proposal {
        rung: crate::abr::Rung::P1080M12,
        direction: Direction::Up,
    };
    assert_eq!(
        latched_original_recovery(
            24_000,
            None,
            crate::abr::Decision::Prime(proposal),
            true,
            Some(2_000),
            2_000,
        ),
        LatchedOriginalRecovery::Switch {
            kbps: 24_000,
            cancel: Some(proposal),
        },
    );
    assert_eq!(
        latched_original_recovery(
            24_000,
            None,
            crate::abr::Decision::Prime(proposal),
            false,
            Some(2_000),
            2_000,
        ),
        LatchedOriginalRecovery::Wait,
        "a live cleanup obligation still keeps both actuator transitions quiescent",
    );
}

/// Device regression, 2026-09-01: the raw-Part source probe completed with `Recover`, then
/// PMS returned 404 for every later segment under the shared Streaming Resource.  The old
/// loop copied the result into `recover_kbps`, continued, and could publish it only after one
/// more successful HLS object — an impossible transition on that server.  A fresh verdict is
/// already on the completed media boundary and must therefore produce `Switch` immediately.
#[test]
fn a_fresh_original_probe_switches_on_its_own_media_boundary() {
    assert_eq!(
        latched_original_recovery(
            0,
            Some(49_041),
            crate::abr::Decision::Stay,
            true,
            Some(4_000),
            2_000,
        ),
        LatchedOriginalRecovery::Switch {
            kbps: 49_041,
            cancel: None,
        },
    );
}

#[test]
fn a_post_feed_transition_defaults_to_fenced_until_teardown() {
    let _guard = crate::testlock::serial();
    let stable = SHARED
        .hls_candidate_generation
        .load(std::sync::atomic::Ordering::Acquire);
    assert_eq!(stable & 1, 0, "test starts outside a transition");
    struct Restore(u64);
    impl Drop for Restore {
        fn drop(&mut self) {
            SHARED
                .hls_candidate_generation
                .store(self.0, std::sync::atomic::Ordering::Release);
        }
    }
    let _restore = Restore(stable.wrapping_add(2));

    drop(HlsCandidateTransition::arm_unowned_for_test().expect("single transition"));
    assert!(
        SHARED
            .hls_candidate_generation
            .load(std::sync::atomic::Ordering::Acquire)
            & 1
            != 0,
        "dropping a fatal partial transition reopened Play before queue teardown",
    );
}

#[test]
fn proven_candidate_transition_settlement_reopens_generation() {
    let _guard = crate::testlock::serial();
    let stable = SHARED
        .hls_candidate_generation
        .load(std::sync::atomic::Ordering::Acquire);
    assert_eq!(stable & 1, 0, "test starts outside a transition");
    struct Restore(u64);
    impl Drop for Restore {
        fn drop(&mut self) {
            SHARED
                .hls_candidate_generation
                .store(self.0, std::sync::atomic::Ordering::Release);
        }
    }
    let _restore = Restore(stable.wrapping_add(2));

    HlsCandidateTransition::arm_unowned_for_test()
        .expect("single transition")
        .settle();
    assert_eq!(
        SHARED
            .hls_candidate_generation
            .load(std::sync::atomic::Ordering::Acquire),
        stable.wrapping_add(2),
        "explicit settlement did not publish the next stable generation",
    );
}

#[test]
fn unwind_after_candidate_media_publication_stays_fenced_until_teardown() {
    let _guard = crate::testlock::serial();
    let stable = SHARED
        .hls_candidate_generation
        .load(std::sync::atomic::Ordering::Acquire);
    assert_eq!(stable & 1, 0, "test starts outside a transition");
    struct Restore(u64);
    impl Drop for Restore {
        fn drop(&mut self) {
            SHARED
                .hls_candidate_generation
                .store(self.0, std::sync::atomic::Ordering::Release);
        }
    }
    let _restore = Restore(stable.wrapping_add(2));

    let unwind = std::panic::catch_unwind(|| {
        let _transition =
            HlsCandidateTransition::arm_unowned_for_test().expect("single transition");
        panic!("candidate publication unwound before commit or cursor realignment");
    });
    assert!(unwind.is_err());
    assert!(
        SHARED
            .hls_candidate_generation
            .load(std::sync::atomic::Ordering::Acquire)
            & 1
            != 0,
        "unwind made a partially-published candidate generation look stable",
    );
}

#[test]
fn a_floor_deadline_can_only_promote_while_the_fetch_is_in_flight() {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let mut floor = ReserveDeadlineState::new(Some(deadline), true);
    assert_eq!(
        floor.active(false),
        Some(deadline),
        "a bounded floor rollback starts with its proved reserve deadline",
    );
    assert_eq!(
        floor.active(true),
        None,
        "once the internal clock is held, the already-spent rollback deadline disappears",
    );
    assert_eq!(
        floor.active(false),
        None,
        "a later Resume cannot resurrect reserve already released by this transaction",
    );

    let mut ordinary = ReserveDeadlineState::new(Some(deadline), false);
    assert_eq!(
        ordinary.active(true),
        Some(deadline),
        "an upshift or a non-floor downshift cannot inherit the floor exception",
    );
}

#[test]
fn a_hold_racing_a_blocking_timeout_releases_instead_of_expiring_the_floor() {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let mut floor = ReserveDeadlineState::new(Some(deadline), true);
    let attempted_deadline = floor.active(false);

    assert!(attempted_deadline.is_some());
    assert!(
        !floor.note_transport_deadline(attempted_deadline, true),
        "the timeout belongs to the superseded rollback deadline, not transport liveness",
    );
    assert!(!floor.expired);
    assert_eq!(
        floor.active(false),
        None,
        "promotion is monotone even after the main-thread hold later clears",
    );

    let expired_at = std::time::Instant::now();
    let mut ordinary = ReserveDeadlineState::new(Some(expired_at), false);
    assert!(ordinary.note_transport_deadline(Some(expired_at), true));
    assert!(
        ordinary.expired,
        "ordinary candidates retain the deadline result"
    );
}

#[test]
fn a_user_pause_does_not_spend_media_reserve_but_the_playhead_does() {
    let _guard = crate::testlock::serial();
    let start_ns = 40_000_000_000;
    let old = SHARED
        .playpos_ns
        .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
    let old_paused = crate::player::TX
        .paused
        .swap(false, std::sync::atomic::Ordering::AcqRel);
    struct Restore {
        playpos_ns: i64,
        paused: bool,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            SHARED
                .playpos_ns
                .store(self.playpos_ns, std::sync::atomic::Ordering::Release);
            crate::player::TX
                .paused
                .store(self.paused, std::sync::atomic::Ordering::Release);
        }
    }
    let _restore = Restore {
        playpos_ns: old,
        paused: old_paused,
    };
    let mut reserve = candidate_reserve_deadline(
        None,
        Some(std::time::Duration::from_millis(1)),
        false,
        start_ns,
        Direction::Up,
    );
    let attempted_deadline = reserve.active(false).expect("one millisecond reserve");

    crate::player::TX
        .paused
        .store(true, std::sync::atomic::Ordering::Release);
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(
        !reserve.note_transport_deadline(Some(attempted_deadline), false),
        "elapsed wall time cannot spend reserve while the presentation clock is frozen",
    );
    assert!(!reserve.expired);
    crate::player::TX
        .paused
        .store(false, std::sync::atomic::Ordering::Release);
    assert!(
        reserve.active(false).is_some(),
        "Resume restores the same unspent playhead balance",
    );

    SHARED
        .playpos_ns
        .store(start_ns + 1_000_000, std::sync::atomic::Ordering::Release);
    assert!(
        reserve.expire_if_due(false),
        "advancing the playhead by exactly B must spend exactly B",
    );
}

#[test]
fn a_pause_cannot_revive_reserve_already_spent_by_the_playhead() {
    let _guard = crate::testlock::serial();
    let start_ns = 50_000_000_000;
    let old = SHARED
        .playpos_ns
        .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
    let old_paused = crate::player::TX
        .paused
        .swap(false, std::sync::atomic::Ordering::AcqRel);
    struct Restore {
        playpos_ns: i64,
        paused: bool,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            SHARED
                .playpos_ns
                .store(self.playpos_ns, std::sync::atomic::Ordering::Release);
            crate::player::TX
                .paused
                .store(self.paused, std::sync::atomic::Ordering::Release);
        }
    }
    let _restore = Restore {
        playpos_ns: old,
        paused: old_paused,
    };

    let budget = std::time::Duration::from_millis(1);
    let mut polled = ReserveDeadlineState::from_playhead(budget, false);
    let mut blocked = ReserveDeadlineState::from_playhead(budget, false);
    let attempted_deadline = blocked.active(false).expect("running reserve snapshot");

    SHARED
        .playpos_ns
        .store(start_ns + 1_000_000, std::sync::atomic::Ordering::Release);
    crate::player::TX
        .paused
        .store(true, std::sync::atomic::Ordering::Release);

    assert!(
        polled.expire_if_due(false),
        "Pause freezes only a positive balance; equality is already terminal",
    );
    assert!(
        blocked.note_transport_deadline(Some(attempted_deadline), false),
        "an in-flight snapshot remains final when the playhead spent the budget before Pause",
    );
    assert!(polled.expired && blocked.expired);
}

#[test]
fn wall_time_between_projection_and_classification_cannot_spend_playhead_reserve() {
    let _guard = crate::testlock::serial();
    let start_ns = 60_000_000_000;
    let old = SHARED
        .playpos_ns
        .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
    let old_paused = crate::player::TX
        .paused
        .swap(false, std::sync::atomic::Ordering::AcqRel);
    struct Restore {
        playpos_ns: i64,
        paused: bool,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            SHARED
                .playpos_ns
                .store(self.playpos_ns, std::sync::atomic::Ordering::Release);
            crate::player::TX
                .paused
                .store(self.paused, std::sync::atomic::Ordering::Release);
        }
    }
    let _restore = Restore {
        playpos_ns: old,
        paused: old_paused,
    };

    let mut reserve =
        ReserveDeadlineState::from_playhead(std::time::Duration::from_nanos(1), false);
    let attempted_deadline = reserve
        .active(false)
        .expect("positive balance projects a wake");
    std::thread::yield_now();
    assert!(
        !reserve.note_transport_deadline(Some(attempted_deadline), false),
        "an expired wall projection is obsolete while Δplayhead remains strictly below E",
    );
    assert!(!reserve.expired);
}

#[test]
fn stale_reserve_wakes_cannot_renew_transport_liveness() {
    let _guard = crate::testlock::serial();
    let start_ns = 65_000_000_000;
    let old = SHARED
        .playpos_ns
        .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
    let old_paused = crate::player::TX
        .paused
        .swap(false, std::sync::atomic::Ordering::AcqRel);
    struct Restore {
        playpos_ns: i64,
        paused: bool,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            SHARED
                .playpos_ns
                .store(self.playpos_ns, std::sync::atomic::Ordering::Release);
            crate::player::TX
                .paused
                .store(self.paused, std::sync::atomic::Ordering::Release);
        }
    }
    let _restore = Restore {
        playpos_ns: old,
        paused: old_paused,
    };

    let mut reserve = ReserveDeadlineState::from_playhead_at(
        start_ns,
        std::time::Duration::from_millis(2),
        false,
    );
    let watchdog = TransportWatchdog::with_inactivity(std::time::Duration::from_millis(18));
    for _ in 0..2 {
        let snapshot = reserve.active(false).expect("running projection");
        let attempted = watchdog.effective(Some(snapshot));
        std::thread::sleep(std::time::Duration::from_millis(3));
        let wake = observe_hls_deadline(Some(&mut reserve), attempted, &watchdog, false);
        assert!(
            classify_hls_deadline(wake).is_none(),
            "a stale reserve wake is retryable before independent liveness expires",
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(15));
    let attempted = watchdog.effective(reserve.active(false));
    let wake = observe_hls_deadline(Some(&mut reserve), attempted, &watchdog, false);
    let outcome = classify_hls_deadline(wake);
    assert!(matches!(outcome, Some(HlsExit::Failed(_))));
    assert!(
        !reserve.expired,
        "transport failure is not censored rung evidence"
    );
}

#[test]
fn spent_playhead_reserve_wins_before_live_transport_watchdog() {
    let _guard = crate::testlock::serial();
    let start_ns = 66_000_000_000;
    let old = SHARED
        .playpos_ns
        .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
    struct Restore(i64);
    impl Drop for Restore {
        fn drop(&mut self) {
            SHARED
                .playpos_ns
                .store(self.0, std::sync::atomic::Ordering::Release);
        }
    }
    let _restore = Restore(old);
    let mut reserve = ReserveDeadlineState::from_playhead_at(
        start_ns,
        std::time::Duration::from_millis(1),
        false,
    );
    let watchdog = TransportWatchdog::with_inactivity(std::time::Duration::from_secs(1));
    let attempted = watchdog.effective(reserve.active(false));
    SHARED
        .playpos_ns
        .store(start_ns + 1_000_000, std::sync::atomic::Ordering::Release);
    let wake = observe_hls_deadline(Some(&mut reserve), attempted, &watchdog, false);
    assert!(matches!(
        classify_hls_deadline(wake),
        Some(HlsExit::PrimeExpired)
    ));
    assert!(reserve.expired);
    assert!(!watchdog.expired());
}

#[test]
fn an_earlier_liveness_boundary_stays_transport_even_if_reserve_spends_before_classification() {
    let _guard = crate::testlock::serial();
    let start_ns = 67_000_000_000;
    let old = SHARED
        .playpos_ns
        .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
    struct Restore(i64);
    impl Drop for Restore {
        fn drop(&mut self) {
            SHARED
                .playpos_ns
                .store(self.0, std::sync::atomic::Ordering::Release);
        }
    }
    let _restore = Restore(old);
    let mut reserve = ReserveDeadlineState::from_playhead_at(
        start_ns,
        std::time::Duration::from_millis(20),
        false,
    );
    let watchdog = TransportWatchdog::with_inactivity(std::time::Duration::from_millis(1));
    let attempted = watchdog.effective(reserve.active(false));

    std::thread::sleep(std::time::Duration::from_millis(2));
    let wake = observe_hls_deadline(Some(&mut reserve), attempted, &watchdog, false);
    SHARED
        .playpos_ns
        .store(start_ns + 20_000_000, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        classify_hls_deadline(wake),
        Some(HlsExit::Failed(_))
    ));
    assert!(
        !reserve.expired,
        "a later reserve observation cannot relabel the earlier transport boundary",
    );
}

#[test]
fn bounded_downshift_spends_internal_hold_but_excludes_a_hidden_user_pause_cycle() {
    let _guard = crate::testlock::serial();
    let old_paused = crate::player::TX
        .paused
        .load(std::sync::atomic::Ordering::Acquire);
    crate::player::TX.commit_paused(false);
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::player::TX.commit_paused(self.0);
        }
    }
    let _restore = Restore(old_paused);

    let mut reserve = ReserveDeadlineState::from_unpaused_elapsed(
        std::time::Duration::from_millis(80),
        false,
    );
    let attempted = reserve.active(false).expect("bounded recovery deadline");
    std::thread::sleep(std::time::Duration::from_millis(20));
    crate::player::TX.commit_paused(true);
    std::thread::sleep(std::time::Duration::from_millis(80));
    crate::player::TX.commit_paused(false);

    assert!(
        !reserve.note_transport_deadline(Some(attempted), true),
        "a complete accepted Pause->Resume inside one blocked call spends no recovery budget",
    );
    assert!(!reserve.expired);

    // `rebuffering=true` models B=0 with the native clock held. A non-floor recovery remains
    // bounded and therefore spends this involuntary stall even though Δplayhead is zero.
    std::thread::sleep(std::time::Duration::from_millis(70));
    assert!(reserve.expire_if_due(true));
    assert!(reserve.expired);
}

/// Device regression, 2026-09-01. A 16→18 Mbps exploration armed 3.251 s, spent 1.421 s in
/// control, then the viewer paused during media warm-up. The old code replaced the transaction
/// clock with the remaining wall instant and emitted `warmup_deadline` after 3.337 s while the
/// buffer moved only 5.251→4.834 s. The SAME playhead clock must cross control and media.
#[test]
fn a_pause_cannot_turn_an_upshift_control_snapshot_into_a_media_deadline() {
    let _guard = crate::testlock::serial();
    let start_ns = 70_000_000_000;
    let old = SHARED
        .playpos_ns
        .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
    let old_paused = crate::player::TX
        .paused
        .swap(false, std::sync::atomic::Ordering::AcqRel);
    struct Restore {
        playpos_ns: i64,
        paused: bool,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            SHARED
                .playpos_ns
                .store(self.playpos_ns, std::sync::atomic::Ordering::Release);
            crate::player::TX
                .paused
                .store(self.paused, std::sync::atomic::Ordering::Release);
        }
    }
    let _restore = Restore {
        playpos_ns: old,
        paused: old_paused,
    };

    let mut exploration = Some(ReserveDeadlineState::from_playhead(
        std::time::Duration::from_millis(1),
        false,
    ));
    let control_snapshot = exploration_snapshot(&mut exploration).expect("running clock");
    crate::player::TX
        .paused
        .store(true, std::sync::atomic::Ordering::Release);
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(
        !exploration_timeout_is_final(&mut exploration, Some(control_snapshot)),
        "the expired wall snapshot cannot censor an unspent playhead budget",
    );

    let mut media = candidate_reserve_deadline(
        exploration,
        // A fresh media-only clock would expire immediately; carrying the exploration state
        // is therefore the differential, not merely another direct test of `from_playhead`.
        Some(std::time::Duration::ZERO),
        false,
        start_ns,
        Direction::Up,
    );
    assert!(!media.expired);
    assert!(
        media.active(false).is_some(),
        "Pause retains a scheduled re-check for a Resume that races the blocked read"
    );
    crate::player::TX
        .paused
        .store(false, std::sync::atomic::Ordering::Release);
    assert!(
        media.active(false).is_some(),
        "Resume restores the original balance"
    );
    SHARED
        .playpos_ns
        .store(start_ns + 1_000_000, std::sync::atomic::Ordering::Release);
    assert!(
        media.expire_if_due(false),
        "only Δplayhead spends the exploration"
    );
}

#[test]
fn resume_during_a_blocked_pause_projection_cannot_overspend_reserve() {
    let _guard = crate::testlock::serial();
    let start_ns = 71_000_000_000;
    let old_pos = SHARED
        .playpos_ns
        .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
    let old_paused = crate::player::TX
        .paused
        .swap(true, std::sync::atomic::Ordering::AcqRel);
    struct Restore {
        playpos_ns: i64,
        paused: bool,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            SHARED
                .playpos_ns
                .store(self.playpos_ns, std::sync::atomic::Ordering::Release);
            crate::player::TX
                .paused
                .store(self.paused, std::sync::atomic::Ordering::Release);
        }
    }
    let _restore = Restore {
        playpos_ns: old_pos,
        paused: old_paused,
    };

    let mut reserve = ReserveDeadlineState::from_playhead_at(
        start_ns,
        std::time::Duration::from_millis(100),
        false,
    );
    let watchdog = TransportWatchdog::with_inactivity(std::time::Duration::from_secs(1));
    let attempted = reserve
        .active(false)
        .expect("a paused issue still schedules the unchanged reserve boundary");
    let attempted = watchdog.effective(Some(attempted));

    crate::player::TX
        .paused
        .store(false, std::sync::atomic::Ordering::Release);
    SHARED
        .playpos_ns
        .store(start_ns + 100_000_000, std::sync::atomic::Ordering::Release);
    let wake = observe_hls_deadline(Some(&mut reserve), attempted, &watchdog, false);
    assert!(matches!(
        classify_hls_deadline(wake),
        Some(HlsExit::PrimeExpired)
    ));
    assert!(!watchdog.expired());
}

#[test]
fn a_pause_projection_spends_neither_reserve_nor_transport_liveness() {
    let _guard = crate::testlock::serial();
    let start_ns = 72_000_000_000;
    let old_pos = SHARED
        .playpos_ns
        .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
    let old_paused = crate::player::TX
        .paused
        .swap(true, std::sync::atomic::Ordering::AcqRel);
    struct Restore {
        playpos_ns: i64,
        paused: bool,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            SHARED
                .playpos_ns
                .store(self.playpos_ns, std::sync::atomic::Ordering::Release);
            crate::player::TX
                .paused
                .store(self.paused, std::sync::atomic::Ordering::Release);
        }
    }
    let _restore = Restore {
        playpos_ns: old_pos,
        paused: old_paused,
    };

    let mut reserve = ReserveDeadlineState::from_playhead_at(
        start_ns,
        std::time::Duration::from_millis(1),
        false,
    );
    let watchdog = TransportWatchdog::with_inactivity(std::time::Duration::from_millis(50));
    let liveness_deadline = watchdog.deadline;
    let attempted = reserve.active(false).expect("paused reserve projection");
    let attempted = watchdog.effective(Some(attempted));
    std::thread::sleep(std::time::Duration::from_millis(2));
    let wake = observe_hls_deadline(Some(&mut reserve), attempted, &watchdog, false);
    assert!(classify_hls_deadline(wake).is_none());
    assert!(!reserve.expired);
    assert_eq!(watchdog.deadline, liveness_deadline);
}
