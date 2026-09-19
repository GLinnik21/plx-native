//! Resolve/seek/route state-machine tests: worker fencing, teardown, encoder cleanup,
//! source probe/preflight and remux-preview detection.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::apply_plan;

#[test]
#[cfg(feature = "devtriggers")]
fn a_user_contract_requested_during_resolve_survives_the_landing() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    begin_playback_request();
    request_user_route_intent(&ps, UserRouteIntent::Retranscode);

    assert!(
        claim_route_action().is_none(),
        "pre-roll cannot consume a route rebuild"
    );
    let start = prepare_playback_landing(&ps, true);
    settle_plan_start_in_unit_test(&mut ps, start);

    let action = claim_route_action().expect("the landed Engine inherits the explicit request");
    assert_eq!(
        action.intent,
        RouteIntent::User(UserRouteIntent::Retranscode)
    );
    finish_route_action(&mut ps, &action, RouteApplyResult::Prepared);
}

#[test]
fn a_cancelled_resolve_has_an_explicit_terminal_phase() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    begin_playback_request();
    cancel_playback_request(&mut ps, false);
    assert!(
        claim_route_action().is_none(),
        "an empty cancelled resolve lands Idle"
    );

    reset_player_control_for_test(&ps);
    begin_playback_request();
    request_user_route_intent(&ps, UserRouteIntent::AdaptiveReload);
    cancel_playback_request(&mut ps, true);
    let action = claim_route_action().expect("the retained route lands Stable");
    assert_eq!(
        action.intent,
        RouteIntent::User(UserRouteIntent::AdaptiveReload)
    );
    finish_route_action(&mut ps, &action, RouteApplyResult::Prepared);
}

#[test]
#[cfg(feature = "devtriggers")]
fn cancelling_resolve_restores_failed_even_when_its_projection_has_a_url() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    reset_session(&mut ps);
    { let s = &mut ps; {
        s.url = "https://example.invalid/failed-candidate.mkv".into();
        s.cur_audio_sid = 17;
    } };
    let failed_projection = route_projection(&ps);
    {
        let mut control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        control.applied_projection = Some(failed_projection);
        control.phase = ControlPhase::Failed(73);
    }

    begin_playback_request();
    { let s = &mut ps; {
        s.url = "https://example.invalid/incoming.mkv".into();
        s.cur_audio_sid = 0;
    } };
    cancel_playback_request(&mut ps, true);

    let control = PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(control.phase, ControlPhase::Failed(73));
    assert_eq!(url(&ps), "https://example.invalid/failed-candidate.mkv");
    assert_eq!(cur_audio_sid(&ps), 17);
    drop(control);
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
}

#[test]
fn a_new_load_attempt_supersedes_the_old_observer_and_rejects_its_late_results() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    reset_player_control_for_test(&ps);
    let transaction = begin_route_start().expect("route start transaction");
    assert!(prepare_route_start(transaction));
    let first = claim_route_start_attempt(transaction).expect("first Load attempt");
    assert_eq!(
        classify_live_engine_start(first),
        LiveEngineStartRelation::CurrentAttempt,
        "rediscovering the Engine for the same Load is idempotent",
    );

    begin_engine_teardown(true);
    let second = claim_route_start_attempt(transaction).expect("replacement Load attempt");
    assert_ne!(first.attempt, second.attempt);
    assert_eq!(
        classify_live_engine_start(first),
        LiveEngineStartRelation::Conflict(transaction),
    );
    assert_eq!(
        classify_live_engine_start(second),
        LiveEngineStartRelation::CurrentAttempt,
    );
    assert_eq!(
        route_start_status(first),
        RouteStartStatus::Superseded(second),
    );
    assert_eq!(route_start_status(second), RouteStartStatus::Pending);

    assert!(!settle_route_start(&mut ps, first, RouteStartResult::Started));
    assert!(!settle_route_start(&mut ps, first, RouteStartResult::StartFailed));
    assert_eq!(route_start_status(second), RouteStartStatus::Pending);
    assert!(settle_route_start(&mut ps, second, RouteStartResult::Started));
    assert_eq!(route_start_status(second), RouteStartStatus::Started);
    assert_eq!(
        PLAYER_CONTROL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase,
        ControlPhase::Stable,
    );
    reset_player_control_for_test(&ps);
}

#[test]
#[cfg(feature = "devtriggers")]
fn backgrounding_an_unproven_original_rearms_frame_proof_on_a_new_load() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    reset_session(&mut ps);
    { let s = &mut ps; {
        s.url = "http://fixture.invalid/hls/master.m3u8".into();
        s.tsession = "foreground-held-hls".into();
        s.cur_delivery = crate::plex::TranscodeDelivery::FixedHls {
            seconds_per_segment: 2,
        };
        s.cur_ceiling = Some(crate::abr::Rung::P480.ceiling());
    } };
    install_active_hls(
        "foreground-held-hls",
        "http://fixture.invalid/hls/master.m3u8",
        crate::abr::Rung::P480,
    );
    reset_player_control_for_test(&ps);
    let pending = snapshot_route(&ps, "foreground-held-hls".into(), 44);
    { let s = &mut ps; {
        s.url = "https://example.invalid/source.mkv".into();
        s.tsession.clear();
        s.cur_delivery = crate::plex::TranscodeDelivery::ProgressiveMkv;
        s.cur_ceiling = None;
    } };
    set_pending_original(&ps, pending, true);

    let first = settle_pending_native_start(&mut ps, RouteStartResult::Started);
    assert!(matches!(
        PLAYER_CONTROL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase,
        ControlPhase::OriginalTrial(OriginalTrialPhase::AwaitingFrame(_)),
    ));
    begin_engine_teardown(true);
    let transaction = pending_route_start().expect("Original transaction re-prepared");
    let second = claim_route_start_attempt(transaction).expect("replacement Original Load");
    assert_ne!(first, second);
    assert_eq!(
        route_start_status(first),
        RouteStartStatus::Superseded(second),
    );
    assert!(!settle_route_start(&mut ps, first, RouteStartResult::Started));
    assert!(settle_route_start(&mut ps, second, RouteStartResult::Started));
    assert!(matches!(
        PLAYER_CONTROL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase,
        ControlPhase::OriginalTrial(OriginalTrialPhase::AwaitingFrame(_)),
    ));

    assert_eq!(rollback_seconds(&mut ps), Some(44));
    settle_pending_native_start(&mut ps, RouteStartResult::Started);
    reset_session(&mut ps);
    install_active_encoder("");
    reset_player_control_for_test(&ps);
}

#[test]
#[cfg(feature = "devtriggers")]
fn resolve_cannot_hide_a_live_start_transaction() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    reset_player_control_for_test(&ps);
    let transaction = begin_route_start().expect("route start transaction");
    assert!(prepare_route_start(transaction));
    let attempt = claim_route_start_attempt(transaction).expect("physical Load attempt");
    let before_revision = desired_contract_revision();

    assert!(!begin_playback_request());
    assert_eq!(desired_contract_revision(), before_revision);
    assert_eq!(route_start_status(attempt), RouteStartStatus::Pending);
    assert!(settle_route_start(&mut ps, attempt, RouteStartResult::Started));
    assert_eq!(route_start_status(attempt), RouteStartStatus::Started);

    reset_player_control_for_test(&ps);
}

#[test]
#[cfg(feature = "devtriggers")]
fn accepted_original_load_stays_in_trial_until_a_frame_or_rollback() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    reset_session(&mut ps);
    { let s = &mut ps; {
        s.url = "http://fixture.invalid/hls/master.m3u8".into();
        s.tsession = "held-hls".into();
        s.cur_delivery = crate::plex::TranscodeDelivery::FixedHls {
            seconds_per_segment: 2,
        };
        s.cur_ceiling = Some(crate::abr::Rung::P480.ceiling());
    } };
    install_active_hls(
        "held-hls",
        "http://fixture.invalid/hls/master.m3u8",
        crate::abr::Rung::P480,
    );
    reset_player_control_for_test(&ps);
    let pending = snapshot_route(&ps, "held-hls".into(), 31);
    { let s = &mut ps; {
        s.url = "https://example.invalid/source.mkv".into();
        s.tsession.clear();
        s.cur_delivery = crate::plex::TranscodeDelivery::ProgressiveMkv;
        s.cur_ceiling = None;
    } };
    set_pending_original(&ps, pending, true);

    let attempt = settle_pending_native_start(&mut ps, RouteStartResult::Started);
    assert_eq!(route_start_status(attempt), RouteStartStatus::Started);
    assert!(matches!(
        PLAYER_CONTROL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase,
        ControlPhase::OriginalTrial(OriginalTrialPhase::AwaitingFrame(_)),
    ));
    assert!(
        claim_route_action().is_none(),
        "Load acceptance is not frame proof"
    );

    assert_eq!(rollback_seconds(&mut ps), Some(31));
    settle_pending_native_start(&mut ps, RouteStartResult::Started);
    reset_session(&mut ps);
    install_active_encoder("");
    reset_player_control_for_test(&ps);
}

#[test]
#[cfg(feature = "devtriggers")]
fn automatic_publication_is_busy_for_the_whole_staged_user_edit() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    reset_player_control_for_test(&ps);
    install_active_encoder("staging-owner");
    let ticket = worker_ticket();
    let edit = begin_user_quality_boundary(Quality::P720);

    let intent = || AutomaticRouteIntent::HlsToOriginal {
        ticket: ticket.clone(),
        evidence_kbps: 40_000,
        position_ns: 12_000_000_000,
    };
    assert_eq!(
        publish_automatic_route_intent(intent()),
        AutomaticIntentResult::Busy,
    );
    drop(edit);
    assert_eq!(
        publish_automatic_route_intent(intent()),
        AutomaticIntentResult::Accepted,
    );
    reset_player_control_for_test(&ps);
}

#[test]
fn a_failed_resolve_spawn_preserves_the_old_playable_route() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    { let s = &mut ps; s.url = "https://example.invalid/still-playing.mkv".into() };
    begin_playback_request();
    request_user_route_intent(&ps, UserRouteIntent::AdaptiveReload);

    settle_failed_resolve_spawn(&mut ps);

    let action = claim_route_action().expect("the retained route must return to Stable");
    assert_eq!(
        action.intent,
        RouteIntent::User(UserRouteIntent::AdaptiveReload),
    );
    finish_route_action(&mut ps, &action, RouteApplyResult::Prepared);
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
}

#[test]
fn a_failed_resolve_spawn_without_an_old_url_lands_idle() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .phase = ControlPhase::Idle;
    begin_playback_request();

    settle_failed_resolve_spawn(&mut ps);

    assert!(claim_route_action().is_none());
    assert_eq!(
        PLAYER_CONTROL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase,
        ControlPhase::Idle,
    );
    reset_player_control_for_test(&ps);
}

#[test]
#[cfg(feature = "devtriggers")]
fn quality_changed_during_resolve_cannot_land_the_old_contract() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
    begin_playback_request();
    let old_contract = desired_contract_revision();
    let gen = 41;
    PLAY_GEN.store(gen, Ordering::SeqCst);
    PLAY_BUSY.store(true, Ordering::SeqCst);
    *PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(PlayLanding {
        gen,
        trace_generation: 7,
        contract_revision: old_contract,
        plan: Plan {
            url: "https://example.invalid/old-contract.mkv".into(),
            delivery: crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2,
            },
            ceiling: Some(crate::abr::Rung::P480.ceiling()),
            ..Default::default()
        },
        rk: "rk-old-contract".into(),
    });

    // This is the reducer half of a quality edit after ResolveEnv was snapshotted.
    begin_user_contract_boundary();
    assert_eq!(pump_play(&mut ps, &mut crate::stores::metadata::MetadataStore::default()), None);
    assert!(
        url(&ps).is_empty(),
        "the stale plan must never become the applied URL"
    );
    assert_ne!(cur_ceiling(&ps), Some(crate::abr::Rung::P480.ceiling()));

    PLAY_BUSY.store(false, Ordering::SeqCst);
    reset_player_control_for_test(&ps);
    reset_session(&mut ps);
}

#[test]
#[cfg(feature = "devtriggers")]
fn a_seek_revokes_automatic_evidence_without_erasing_the_user_contract() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    install_active_hls(
        "seek-owner",
        "http://fixture.invalid/live.m3u8",
        crate::abr::Rung::P480,
    );
    let before_seek = worker_ticket();
    request_user_route_intent(&ps, UserRouteIntent::AdaptiveReload);
    note_user_seek_intent(90_000_000_000);

    assert!(pending_user_route_intent(UserRouteIntent::AdaptiveReload));
    assert_eq!(
        publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
            ticket: before_seek.clone(),
            evidence_kbps: 50_000,
            position_ns: 90_000_000_000,
        }),
        AutomaticIntentResult::Busy,
    );
    let action = claim_route_action().expect("seek and quality coalesce into the user action");
    assert_eq!(
        action.intent,
        RouteIntent::User(UserRouteIntent::AdaptiveReload)
    );
    finish_route_action(&mut ps, &action, RouteApplyResult::Prepared);
    assert!(commit_user_seek());
    assert_ne!(worker_ticket(), before_seek);
}

#[test]
#[cfg(feature = "devtriggers")]
fn a_seek_retargets_an_accepted_handoff_instead_of_erasing_its_only_producer() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    install_active_encoder("direct-owner");
    let worker = worker_ticket();
    assert_eq!(
        publish_automatic_route_intent(AutomaticRouteIntent::OriginalToHls {
            ticket: worker,
            conservative_kbps: 4_000,
            position_ns: 12_000_000_000,
        }),
        AutomaticIntentResult::Accepted,
    );

    note_user_seek_intent(90_000_000_000);
    let action =
        claim_route_action().expect("the accepted handoff still owns the stopped worker");
    assert_eq!(action.ticket, worker_ticket());
    assert!(matches!(
        action.intent,
        RouteIntent::Automatic(AutomaticRouteIntent::OriginalToHls {
            position_ns: 90_000_000_000,
            ..
        })
    ));
    finish_route_action(&mut ps, &action, RouteApplyResult::Prepared);
    assert!(commit_user_seek());
}

#[test]
#[cfg(feature = "devtriggers")]
fn rejected_transcode_seek_preserves_hls_worker_authority() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    install_active_hls(
        "seek-refusal-owner",
        "http://fixture.invalid/live.m3u8",
        crate::abr::Rung::P480,
    );
    let worker = worker_ticket();
    note_user_seek_intent(90_000_000_000);
    assert_eq!(
        publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
            ticket: worker.clone(),
            evidence_kbps: 50_000,
            position_ns: 12_000_000_000,
        }),
        AutomaticIntentResult::Busy,
    );

    reject_user_seek();
    assert_eq!(worker_ticket(), worker);
    assert_eq!(
        publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
            ticket: worker,
            evidence_kbps: 50_000,
            position_ns: 12_000_000_000,
        }),
        AutomaticIntentResult::Accepted,
    );
    reset_player_control_for_test(&ps);
}

#[test]
fn a_rejected_user_action_leaves_the_accepted_handoff_owned_for_the_next_tick() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    install_active_encoder("direct-owner");
    assert_eq!(
        publish_automatic_route_intent(AutomaticRouteIntent::OriginalToHls {
            ticket: worker_ticket(),
            conservative_kbps: 4_000,
            position_ns: 12_000_000_000,
        }),
        AutomaticIntentResult::Accepted,
    );
    request_user_route_intent(&ps, UserRouteIntent::Retranscode);

    let user = claim_route_action().expect("user action has priority");
    assert_eq!(user.intent, RouteIntent::User(UserRouteIntent::Retranscode));
    finish_route_action(&mut ps, &user, RouteApplyResult::Rejected);

    let automatic = claim_route_action().expect("the stopped producer's handoff was not lost");
    assert!(matches!(automatic.intent, RouteIntent::Automatic(_)));
    finish_route_action(&mut ps, &automatic, RouteApplyResult::Prepared);
}

#[test]
#[cfg(feature = "devtriggers")]
fn rejected_user_action_preserves_old_applied_auto_handoff_without_rebinding_it_to_desired() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::Auto);
    reset_player_control_for_test(&ps);
    install_active_encoder("direct-auto-owner");
    let applied = worker_ticket();
    assert_eq!(applied_quality(), Quality::Auto);
    assert_eq!(
        publish_automatic_route_intent(AutomaticRouteIntent::OriginalToHls {
            ticket: applied.clone(),
            conservative_kbps: 4_000,
            position_ns: 12_000_000_000,
        }),
        AutomaticIntentResult::Accepted,
    );

    begin_user_quality_boundary(Quality::P720);
    request_user_route_intent(&ps, UserRouteIntent::Retranscode);
    let user = claim_route_action().expect("the newer explicit contract has priority");
    assert_eq!(user.intent, RouteIntent::User(UserRouteIntent::Retranscode));
    finish_route_action(&mut ps, &user, RouteApplyResult::Rejected);

    assert_eq!(
        applied_quality(),
        Quality::Auto,
        "PMS refusal changed the policy which owns the unchanged physical stream",
    );
    let automatic =
        claim_route_action().expect("the stopped Auto producer retained its handoff");
    assert_eq!(
        automatic.ticket, applied,
        "old Auto evidence was rebound to a desired contract it never observed",
    );
    assert!(matches!(
        automatic.intent,
        RouteIntent::Automatic(AutomaticRouteIntent::OriginalToHls { .. })
    ));
    finish_route_action(&mut ps, &automatic, RouteApplyResult::Prepared);
    assert_eq!(
        applied_quality(),
        Quality::Auto,
        "an automatic route transition must not commit the rejected Fixed preference",
    );
    assert_eq!(
        worker_ticket(),
        applied,
        "automatic completion rebound its worker to the rejected desired revision",
    );

    restore_quality(Quality::Original);
    reset_player_control_for_test(&ps);
}

#[test]
#[cfg(feature = "devtriggers")]
fn rejected_route_effect_restores_the_whole_applied_projection() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let previous = route_projection(&ps);
    { let s = &mut ps; {
        s.url = "http://fixture.invalid/applied-480.m3u8".into();
        s.tsession = "applied-480".into();
        s.cur_remux = false;
        s.cur_delivery = crate::plex::TranscodeDelivery::FixedHls {
            seconds_per_segment: 2,
        };
        s.cur_no_video_copy = true;
        s.cur_ceiling = Some(crate::abr::Rung::P480.ceiling());
        s.cur_auto_original_watched = false;
        s.cur_audio_sid = 17;
        s.cur_sub_sid = 23;
        s.stream_vcodec = "h264".into();
        s.stream_acodec = "aac".into();
        s.stream_fps = 0.0;
        s.stream_dovi = crate::metadata::Dovi::NONE;
        s.stream_immersive = false;
    } };
    restore_quality(Quality::Auto);
    reset_player_control_for_test(&ps);

    begin_user_quality_boundary(Quality::P1080High);
    { let s = &mut ps; {
        s.url = "http://fixture.invalid/not-yet-applied-4k.m3u8".into();
        s.tsession = "not-yet-applied-4k".into();
        s.cur_delivery = crate::plex::TranscodeDelivery::ProgressiveMkv;
        s.cur_no_video_copy = false;
        s.cur_ceiling = Some(crate::abr::Rung::Uhd.ceiling());
        s.cur_auto_original_watched = true;
        s.cur_audio_sid = 99;
        s.cur_sub_sid = 101;
        s.stream_vcodec = "hevc".into();
        s.stream_acodec = "eac3".into();
        s.stream_fps = 23.976;
        s.stream_immersive = true;
    } };
    request_user_route_intent(&ps, UserRouteIntent::Retranscode);
    let action = claim_route_action().expect("staged user route");
    finish_route_action(&mut ps, &action, RouteApplyResult::Rejected);

    let restored = route_projection(&ps);
    assert_eq!(restored.url, "http://fixture.invalid/applied-480.m3u8");
    assert_eq!(restored.tsession, "applied-480");
    assert_eq!(
        restored.delivery,
        crate::plex::TranscodeDelivery::FixedHls {
            seconds_per_segment: 2,
        },
    );
    assert_eq!(restored.ceiling, Some(crate::abr::Rung::P480.ceiling()));
    assert_eq!(restored.audio_sid, 17);
    assert_eq!(restored.subtitle_sid, 23);
    assert_eq!(restored.stream_vcodec, "h264");
    assert_eq!(restored.stream_acodec, "aac");
    assert!(!restored.auto_original_watched);
    assert!(!restored.stream_immersive);

    install_route_projection(&mut ps, &previous);
    install_active_encoder("");
    restore_quality(Quality::Original);
    reset_player_control_for_test(&ps);
}

#[test]
#[cfg(feature = "devtriggers")]
fn hls_commit_during_a_staged_user_contract_merges_only_physical_fields() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let previous = route_projection(&ps);
    { let s = &mut ps; {
        s.url = "http://fixture.invalid/old-480.m3u8".into();
        s.tsession = "old-480".into();
        s.cur_delivery = crate::plex::TranscodeDelivery::FixedHls {
            seconds_per_segment: 2,
        };
        s.cur_ceiling = Some(crate::abr::Rung::P480.ceiling());
        s.cur_audio_sid = 17;
        s.cur_sub_sid = 23;
        s.stream_vcodec = "h264".into();
        s.stream_acodec = "aac".into();
    } };
    restore_quality(Quality::Auto);
    install_active_hls(
        "old-480",
        "http://fixture.invalid/old-480.m3u8",
        crate::abr::Rung::P480,
    );
    reset_player_control_for_test(&ps);
    let worker = worker_ticket();

    begin_user_contract_boundary();
    { let s = &mut ps; s.cur_audio_sid = 99 };
    request_user_route_intent(&ps, UserRouteIntent::Retranscode);
    assert!(replace_active_hls_for(
        &worker,
        "new-720",
        "http://fixture.invalid/new-720.m3u8",
        crate::abr::Rung::P720,
        None,
    )
    .is_some());
    sync_active_hls_to_session(&mut ps).expect("physical HLS commit");

    let action = claim_route_action().expect("staged audio rebuild");
    finish_route_action(&mut ps, &action, RouteApplyResult::Rejected);
    let restored = route_projection(&ps);
    assert_eq!(restored.url, "http://fixture.invalid/new-720.m3u8");
    assert_eq!(restored.tsession, "new-720");
    assert_eq!(restored.ceiling, Some(crate::abr::Rung::P720.ceiling()));
    assert_eq!(
        restored.audio_sid, 17,
        "unaccepted track leaked into applied route"
    );
    assert_eq!(restored.subtitle_sid, 23);

    install_route_projection(&mut ps, &previous);
    restore_quality(Quality::Original);
    reset_player_control_for_test(&ps);
}

#[test]
#[cfg(feature = "devtriggers")]
fn rejected_user_retranscode_keeps_the_physical_worker_authorized() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    install_active_hls(
        "still-serving-hls",
        "http://fixture.invalid/live.m3u8",
        crate::abr::Rung::P480,
    );
    let physical_worker = worker_ticket();

    request_user_route_intent(&ps, UserRouteIntent::Retranscode);
    let action = claim_route_action().expect("user application");
    finish_route_action(&mut ps, &action, RouteApplyResult::Rejected);

    assert_eq!(
        worker_ticket(),
        physical_worker,
        "a refused desired route must not revoke the unchanged applied worker"
    );
    assert_eq!(
        publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
            ticket: physical_worker,
            evidence_kbps: 50_000,
            position_ns: 12_000_000_000,
        }),
        AutomaticIntentResult::Accepted,
        "the retained HLS worker must resume adaptive publication after refusal"
    );
    reset_player_control_for_test(&ps);
}

#[test]
#[cfg(feature = "devtriggers")]
fn pinning_the_live_auto_hls_rung_fences_its_worker_before_projection_changes() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::Auto);
    apply_plan(&mut ps, 
        Plan {
            url: "http://fixture.invalid/4000/master.m3u8".into(),
            sess: "logical-auto".into(),
            tsession: "encoder-auto-720".into(),
            delivery: crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2,
            },
            ceiling: Some(crate::abr::Rung::P720.ceiling()),
            src_measure: (22_000, 3_840, 2_160),
            auto_original: Some(test_original_candidate(None)),
            ..Default::default()
        },
        "rk-auto-720",
    );
    let outgoing = worker_ticket();
    assert_eq!(
        publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
            ticket: outgoing.clone(),
            evidence_kbps: 50_000,
            position_ns: 90_000_000_000,
        }),
        AutomaticIntentResult::Accepted,
    );

    set_quality(&mut ps, Quality::P720);

    assert_eq!(quality(), Quality::P720);
    assert_eq!(
        cur_delivery(&ps),
        crate::plex::TranscodeDelivery::ProgressiveMkv,
    );
    assert_eq!(
        publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
            ticket: outgoing,
            evidence_kbps: 60_000,
            position_ns: 91_000_000_000,
        }),
        AutomaticIntentResult::Busy,
        "the old Auto worker is paused while the desired pin is applying, but remains the applied owner until commit",
    );
    let user = claim_route_action().expect("pinning HLS queues a manual transcode");
    assert_eq!(user.intent, RouteIntent::User(UserRouteIntent::Retranscode));
    finish_route_action(&mut ps, &user, RouteApplyResult::Rejected);
    let automatic = claim_route_action()
        .expect("the already accepted handoff remains owned after the user action");
    assert!(matches!(automatic.intent, RouteIntent::Automatic(_)));
    finish_route_action(&mut ps, &automatic, RouteApplyResult::Prepared);

    restore_quality(Quality::Original);
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
}

#[test]
#[cfg(feature = "devtriggers")]
fn reselecting_the_exact_quality_does_not_fence_the_current_worker() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::Auto);
    apply_plan(&mut ps, 
        Plan {
            url: "http://fixture.invalid/4000/master.m3u8".into(),
            tsession: "encoder-auto-720".into(),
            delivery: crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2,
            },
            ceiling: Some(crate::abr::Rung::P720.ceiling()),
            src_measure: (22_000, 3_840, 2_160),
            auto_original: Some(test_original_candidate(None)),
            ..Default::default()
        },
        "rk-auto-720",
    );
    let before = worker_ticket();

    set_quality(&mut ps, Quality::Auto);

    assert_eq!(worker_ticket(), before);
    assert!(
        claim_route_action().is_none(),
        "an identical Auto selection must not restart or re-fence its live HLS worker",
    );

    restore_quality(Quality::Original);
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
}

#[test]
fn a_pending_retranscode_cannot_be_weakened_into_a_native_reload() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    request_user_route_intent(&ps, UserRouteIntent::Retranscode);
    request_user_route_intent(&ps, UserRouteIntent::NativeAudioReload);

    let action = claim_route_action().expect("merged user obligation");
    assert_eq!(
        action.intent,
        RouteIntent::User(UserRouteIntent::Retranscode)
    );
    finish_route_action(&mut ps, &action, RouteApplyResult::Prepared);
}

#[test]
#[cfg(feature = "devtriggers")]
fn subtitle_off_keeps_a_pending_original_recovery() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::Original);
    apply_plan(&mut ps, 
        Plan {
            url: "http://fixture.invalid/4000/master.m3u8".into(),
            tsession: "encoder-subtitle-off".into(),
            delivery: crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2,
            },
            ceiling: Some(crate::abr::Rung::P720.ceiling()),
            sub_sid: 77,
            auto_original: Some(test_original_candidate(Some(3))),
            ..Default::default()
        },
        "rk-subtitle-off",
    );
    request_user_route_intent(&ps, UserRouteIntent::RecoverOriginal);

    commit_subtitle_selection(&mut ps, -1, 0);

    assert_eq!(cur_sub_sid(&ps), 0);
    assert_eq!(
        ps
            .auto_original
            .as_ref()
            .and_then(|candidate| candidate.subtitle_ordinal),
        None,
        "the retained source declaration must carry subtitles Off",
    );
    let action = claim_route_action().expect("Original recovery remains the owned action");
    assert_eq!(
        action.intent,
        RouteIntent::User(UserRouteIntent::RecoverOriginal),
    );
    finish_route_action(&mut ps, &action, RouteApplyResult::Prepared);

    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
    crate::player::reset_subtitle();
}

#[test]
fn a_direct_subtitle_change_keeps_the_original_watchdog_ticket_current() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::Auto);
    apply_plan(&mut ps, 
        Plan {
            url: "https://example.invalid/source.mkv".into(),
            delivery: crate::plex::TranscodeDelivery::ProgressiveMkv,
            transport_kbps: 22_000,
            auto_original_watched: true,
            auto_original: Some(test_original_candidate(None)),
            ..Default::default()
        },
        "rk-direct-subtitle",
    );
    let watchdog = worker_ticket();

    commit_subtitle_selection(&mut ps, 2, 88);

    assert_eq!(worker_ticket(), watchdog);
    assert!(auto_original_watch(&ps).is_some());
    assert!(
        claim_route_action().is_none(),
        "client-rendered subtitles do not replace the direct media route",
    );

    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
    restore_quality(Quality::Original);
    crate::player::reset_subtitle();
}

#[test]
#[cfg(feature = "devtriggers")]
fn subtitle_on_invalidates_a_pending_original_recovery() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::Original);
    apply_plan(&mut ps, 
        Plan {
            url: "http://fixture.invalid/4000/master.m3u8".into(),
            tsession: "encoder-subtitle-on".into(),
            delivery: crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2,
            },
            ceiling: Some(crate::abr::Rung::P720.ceiling()),
            auto_original: Some(test_original_candidate(None)),
            ..Default::default()
        },
        "rk-subtitle-on",
    );
    request_user_route_intent(&ps, UserRouteIntent::RecoverOriginal);

    commit_subtitle_selection(&mut ps, 2, 88);

    assert!(ps.auto_original.is_none());
    let action = claim_route_action().expect("the burned subtitle needs HLS retranscode");
    assert_eq!(
        action.intent,
        RouteIntent::User(UserRouteIntent::Retranscode)
    );
    finish_route_action(&mut ps, &action, RouteApplyResult::Prepared);

    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
    crate::player::reset_subtitle();
}

#[test]
#[cfg(feature = "devtriggers")]
fn audio_change_invalidates_a_pending_original_recovery() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::Original);
    apply_plan(&mut ps, 
        Plan {
            url: "http://fixture.invalid/4000/master.m3u8".into(),
            tsession: "encoder-audio-change".into(),
            delivery: crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2,
            },
            ceiling: Some(crate::abr::Rung::P720.ceiling()),
            auto_original: Some(test_original_candidate(None)),
            ..Default::default()
        },
        "rk-audio-change",
    );
    request_user_route_intent(&ps, UserRouteIntent::RecoverOriginal);

    commit_audio_selection(&mut ps, 1, "aac", 99);

    assert!(ps.auto_original.is_none());
    let action = claim_route_action().expect("the new audio track needs HLS retranscode");
    assert_eq!(
        action.intent,
        RouteIntent::User(UserRouteIntent::Retranscode)
    );
    finish_route_action(&mut ps, &action, RouteApplyResult::Prepared);

    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
    crate::player::reset_audio_track();
}

#[test]
#[cfg(feature = "devtriggers")]
fn an_original_trial_is_busy_not_stale_to_its_new_watchdog() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    install_active_encoder("original-owner");
    let ticket = worker_ticket();
    PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .phase = ControlPhase::OriginalTrial(OriginalTrialPhase::Prepared(1));

    assert_eq!(
        publish_automatic_route_intent(AutomaticRouteIntent::OriginalToHls {
            ticket,
            conservative_kbps: 4_000,
            position_ns: 12_000_000_000,
        }),
        AutomaticIntentResult::Busy,
    );
    reset_player_control_for_test(&ps);
}

#[test]
#[cfg(feature = "devtriggers")]
fn teardown_invalidates_the_worker_before_it_can_publish() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    install_active_hls(
        "teardown-owner",
        "http://fixture.invalid/live.m3u8",
        crate::abr::Rung::P480,
    );
    let outgoing = worker_ticket();
    begin_engine_teardown(false);

    assert_eq!(
        publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
            ticket: outgoing,
            evidence_kbps: 50_000,
            position_ns: 90_000_000_000,
        }),
        AutomaticIntentResult::Stale,
    );
    assert!(
        claim_route_action().is_none(),
        "Stopping owns the transition boundary"
    );
    reset_player_control_for_test(&ps);
}

/// The other half of the test above: `Stopping` fences the workers for the DURATION of the
/// synchronous teardown, and `finish_engine_teardown` retires that fence when the teardown
/// returns. A latched `Stopping` refused LG App Self Checklist #46's replay outright
/// (`start_bufferfeed: route reducer refused a start owner`), because the dev fixture asks
/// `begin_route_start` for an owner directly rather than through `begin_playback_request`.
///
/// The RED here is structural rather than historical — `finish_engine_teardown` did not exist
/// against the broken build. The failure as reported is reproduced by
/// `player::engine::replay_after_stop_tests::a_completed_stop_grants_the_next_start_a_route_owner`,
/// which drives the real `stop_bufferfeed` and was watched red.
#[test]
fn a_completed_teardown_releases_the_fence_it_raised() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    reset_player_control_for_test(&ps);
    install_active_hls(
        "replay-owner",
        "http://fixture.invalid/live.m3u8",
        crate::abr::Rung::P480,
    );

    begin_engine_teardown(false);
    assert!(
        begin_route_start().is_none(),
        "a start may not be minted while the teardown still owns the loop",
    );

    finish_engine_teardown();

    assert!(
        claim_route_action().is_none(),
        "a completed stop is still not a publishable route",
    );
    assert!(
        pending_route_start().is_none(),
        "a completed stop owns no transaction of its own",
    );
    let start =
        begin_route_start().expect("a completed stop must grant the next start an owner");
    assert!(
        prepare_route_start(start),
        "the replay owns the transaction it was granted",
    );
    let attempt = claim_route_start_attempt(start)
        .expect("a granted transaction mints one physical Load attempt");
    assert!(settle_route_start(&mut ps, attempt, RouteStartResult::Started));
    assert_eq!(
        PLAYER_CONTROL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase,
        ControlPhase::Stable,
        "the replayed Load settles into an ordinary publishable route",
    );

    reset_player_control_for_test(&ps);
}

#[test]
fn a_route_commit_between_automatic_publication_and_claim_discards_the_stale_action() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    install_active_hls(
        "auto-owner",
        "http://fixture.invalid/live.m3u8",
        crate::abr::Rung::P480,
    );
    let outgoing = worker_ticket();
    assert_eq!(
        publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
            ticket: outgoing.clone(),
            evidence_kbps: 50_000,
            position_ns: 90_000_000_000,
        }),
        AutomaticIntentResult::Accepted,
    );
    assert!(
        replace_active_encoder_for(&outgoing, "new-owner").is_some(),
        "the candidate wins before the main thread claims the automatic handoff",
    );

    assert!(
        claim_route_action().is_none(),
        "the queued evidence belongs to the retired route and is discarded",
    );
}

#[test]
fn a_claimed_route_action_fences_worker_candidate_commits() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    install_active_hls(
        "action-owner",
        "http://fixture.invalid/live.m3u8",
        crate::abr::Rung::P480,
    );
    let worker = worker_ticket();
    request_user_route_intent(&ps, UserRouteIntent::Retranscode);
    let action = claim_route_action().expect("user action claimed");

    assert_eq!(
        replace_active_hls_with(
            &worker,
            "candidate-owner",
            "http://fixture.invalid/candidate.m3u8",
            crate::abr::Rung::P720Low,
            None,
            || Some(()),
        ),
        Err(ActiveHlsCommitRefusal::RouteMoved),
        "Applying is the exclusive route-mutation phase",
    );
    finish_route_action(&mut ps, &action, RouteApplyResult::Prepared);
}

/// Regression for the worker handoff race: a boolean ownership check followed by a mailbox
/// store let seek replace ACTIVE in between. The callback door must both reject an already
/// moved route without touching the mailbox and hold ACTIVE throughout an accepted store.
#[test]
#[cfg(feature = "devtriggers")]
fn source_recovery_publication_is_atomic_with_route_ownership() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let owner = "source-recovery-owner";
    install_active_hls(
        owner,
        "http://fixture.invalid/live.m3u8",
        crate::abr::Rung::P480,
    );
    let owner_lease = active_route_lease();
    let superseded = RouteLease {
        epoch: owner_lease.epoch,
        encoder: "superseded-owner".into(),
    };
    let mailbox = std::sync::atomic::AtomicI64::new(0);

    assert_eq!(
        with_active_route(&superseded, || { mailbox.store(49_041, Ordering::Release) }),
        Err(ActiveEncoderRefusal::RouteMoved),
    );
    assert_eq!(
        mailbox.load(Ordering::Acquire),
        0,
        "a superseded worker cannot publish its Original handoff",
    );

    assert_eq!(
        with_active_route(&owner_lease, || {
            assert!(matches!(
                PLAYER_CONTROL.try_lock(),
                Err(std::sync::TryLockError::WouldBlock),
            ));
            mailbox.store(49_041, Ordering::Release);
        }),
        Ok(()),
    );
    assert_eq!(mailbox.load(Ordering::Acquire), 49_041);
    assert!(
        replace_active_encoder(owner, "seek-owner"),
        "the route may move only after the publication callback releases ACTIVE",
    );
    install_active_encoder("");
}

/// A semantic route change can keep the same PMS resource id: direct Original deliberately
/// retains the HLS Streaming Resource while dropping its HLS projection. Comparing only the
/// id therefore admits an outgoing HLS worker after ownership has changed (same-id ABA).
#[test]
#[cfg(feature = "devtriggers")]
fn a_same_id_route_change_invalidates_the_outgoing_worker() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let owner = "same-resource-owner";
    install_active_hls(
        owner,
        "http://fixture.invalid/live.m3u8",
        crate::abr::Rung::P480,
    );
    let outgoing_owner = active_route_lease();

    assert!(
        replace_active_encoder(owner, owner),
        "direct Original keeps the exact PMS resource id",
    );
    assert_eq!(
        with_active_route(&outgoing_owner, || ()),
        Err(ActiveEncoderRefusal::RouteMoved),
        "the old worker lease must not survive a same-id semantic route change",
    );
    install_active_encoder("");
}

#[test]
fn a_resume_intent_belongs_to_exactly_one_resolve_generation() {
    let mut pending = Some((41, 3_600_000_000_000));
    assert_eq!(take_resume_for(&mut pending, 40), 0);
    assert_eq!(pending, Some((41, 3_600_000_000_000)));
    assert_eq!(take_resume_for(&mut pending, 41), 3_600_000_000_000);
    assert_eq!(pending, None);
}

#[test]
#[cfg(feature = "devtriggers")]
fn abandoned_resolves_retire_the_streaming_resources_they_created() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    use std::io::{BufRead, BufReader, Write};

    let _g = fresh_registry(&mut ps);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind cleanup server");
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    let (tx, rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        // A FAILURE BOUND, NOT A RUNTIME: the loop returns the moment the request arrives,
        // so a passing run never spends this. One second was not one — under a loaded
        // 1900-test parallel run the client had not been scheduled yet, the loop gave up, and
        // the count assertion below failed with an empty vec. Observed twice in ordinary runs
        // on 2026-09-02, never when the module ran alone.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut requests = Vec::new();
        while std::time::Instant::now() < deadline && requests.len() < 2 {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    let mut request = String::new();
                    let mut reader = BufReader::new(socket.try_clone().expect("clone socket"));
                    reader
                        .read_line(&mut request)
                        .expect("read cleanup request");
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("cleanup response");
                    requests.push(request);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("accept cleanup request: {error}"),
            }
        }
        tx.send(requests).unwrap();
    });
    let sid = crate::plex::register_for_test(
        "stale-resolve",
        "127.0.0.1",
        port,
        "token",
        "stale-resolve-client",
    );

    PLAY_GEN.store(2, Ordering::SeqCst);
    *PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(PlayLanding {
        gen: 1,
        trace_generation: 1,
        contract_revision: desired_contract_revision(),
        plan: Plan {
            sid,
            sess: "abandoned-logical-resource".into(),
            url: "https://example.invalid/source.mkv".into(),
            ..Default::default()
        },
        rk: "abandoned-rk".into(),
    });

    assert_eq!(
        pump_play(&mut ps, &mut crate::stores::metadata::MetadataStore::default()),
        None,
        "the superseded plan may not be installed"
    );

    PLAY_GEN.store(3, Ordering::SeqCst);
    *PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(PlayLanding {
        gen: 3,
        trace_generation: 2,
        contract_revision: desired_contract_revision(),
        plan: Plan {
            sid,
            sess: "refused-logical-resource".into(),
            verdict: Some("server refused this route".into()),
            ..Default::default()
        },
        rk: "refused-rk".into(),
    });
    assert_eq!(pump_play(&mut ps, &mut crate::stores::metadata::MetadataStore::default()), None, "a refusal has no playable URL");
    assert!(
        play_refused(&ps),
        "its server verdict still reaches the error read-out"
    );

    let requests = rx.recv().expect("cleanup observations");
    server.join().unwrap();
    assert_eq!(
        requests.len(),
        2,
        "both ownerless resources need an exact close: {requests:?}"
    );
    for session in ["abandoned-logical-resource", "refused-logical-resource"] {
        let request = requests
            .iter()
            .find(|request| request.contains(&format!("session={session}")))
            .unwrap_or_else(|| panic!("missing cleanup for {session}: {requests:?}"));
        assert!(
            request.contains("/video/:/transcode/universal/stop?"),
            "{request}"
        );
        assert!(request.contains("closeResourceSession=1"), "{request}");
    }

    crate::plex::reset_servers_for_test();
    reset_session(&mut ps);
}

/// **The visible-switch stamp is THIS FRAME'S TICK** (spec §4.1), and the tick has exactly one
/// writer.
///
/// Before phase 9 both readers took `crate::player::vclock_ms()` — a second monotonic clock,
/// read at whatever depth of the call stack happened to need it, so two stamps taken in one
/// frame could differ and the anti-flapping penalty was priced against a clock nothing else in
/// the frame agreed with. `Player::set_now` is now the only writer and the loop calls it once
/// per iteration from the same `fr.now` every other phase reads.
///
/// The second assertion is the part that is easy to break silently: `apply_plan` REPLACES the
/// whole session, and a landing that rewound the stamp would age every subsequent switch
/// against a value from before the film started.
#[test]
fn the_visible_switch_stamp_is_the_frame_tick_and_a_landing_cannot_rewind_it() {
    let _g = crate::testlock::serial();
    let mut player = crate::player::machine::Player::new();

    player.set_now(10_000);
    let stamp = player.session.now_ms;
    note_visible_switch(&mut player.session, stamp);
    player.set_now(12_500);
    assert_eq!(
        auto_history(&player.session, player.session.now_ms).since_last_ms,
        Some(2_500),
        "the age is measured between two frame ticks, not against a private clock",
    );

    player.set_now(20_000);
    apply_plan(
        &mut player.session,
        Plan {
            url: "https://example.invalid/next.mkv".into(),
            ..Default::default()
        },
        "item-1",
    );
    assert_eq!(
        player.session.now_ms, 20_000,
        "a landing replaces the session's contents and must not rewind the frame tick",
    );
    install_active_encoder("");
}

#[test]
fn a_refused_retry_keeps_its_position_and_full_request_for_the_next_quality() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = crate::testlock::serial();
    reset_session(&mut ps);
    let request = PlaybackRequest {
        sid: ServerId::UNSET,
        rk: "episode-42".into(),
        part: "/library/parts/987/1700000000/file.mkv".into(),
        vcodec: "hevc".into(),
        acodec: "eac3".into(),
        title: "Episode".into(),
        ctx: "S01 E02".into(),
        preview: false,
    };
    { let s = &mut ps; {
        s.request = Some(request.clone());
        s.requested_resume_ns = 3_600_000_000_000;
        s.cur_audio_sid = 17;
        s.cur_sub_sid = 23;
    } };

    assert_eq!(
        current_retry_context(&ps, 3_600_000_000_000),
        RetryContext {
            resume_ns: 3_600_000_000_000,
            audio_sid: 17,
            sub_sid: 23,
        },
        "a rescue must not silently restore the server-default tracks",
    );

    apply_plan(&mut ps, 
        Plan {
            verdict: Some("temporary refusal".into()),
            ..Default::default()
        },
        "episode-42",
    );

    assert_eq!(ps.request.as_ref(), Some(&request));
    assert_eq!(unpresented_resume_ns(&ps), 3_600_000_000_000);
    confirm_resume_presented(&mut ps);
    assert_eq!(unpresented_resume_ns(&ps), 0);
    reset_session(&mut ps);
    install_active_encoder("");
}

/// `part_id_of` gates the server-side stream selection: `put_selection` returns early on
/// `<= 0`, so a parse miss silently disables subtitle suppression and audio selection for
/// the whole item — no error, no log line, just a burned-in subtitle nobody asked for.

// ---- the QUALITY ceiling as a ROUTING policy --------------------------------------------
//
// These grade `flavors_allowed(link_policy(link), quality_policy(q, auto_original, …))`, which is
// the expression `build_stream` itself evaluates — not a re-derivation of it. `build_stream`
// is unreachable from the host (it needs a `Client` and a PMS), and the composition is the
// half that can silently go wrong, so it is the half that is factored out and pinned.


/// A stop acknowledgement is not a release event. The ledger must coalesce concurrent
/// checks, retain present/unknown sessions, retry a stop only when it was not accepted, and
/// release one server independently only after physical absence and exact logical close.
#[test]
fn encoder_cleanup_ledger_releases_only_on_exact_absence() {
    let sid_a = ServerId::from_raw(1);
    let sid_b = ServerId::from_raw(2);
    let mut ledger = EncoderCleanupLedger::default();

    assert!(ledger.remember(sid_a, "candidate-a"));
    assert!(
        !ledger.remember(sid_a, "candidate-a"),
        "one physical key, one cleanup owner"
    );
    assert!(!ledger.is_clear(sid_a));
    assert!(
        ledger.is_clear(sid_b),
        "another PMS has independent resource accounting"
    );

    let first = ledger.take_unchecked(sid_a);
    assert_eq!(first.len(), 1);
    assert!(
        first[0].stop_needed,
        "the first worker must issue the one requested stop"
    );
    assert!(
        ledger.take_unchecked(sid_a).is_empty(),
        "an in-flight ping is single-owner"
    );
    ledger.finish(
        first.into_iter().next().unwrap(),
        Some(true),
        Some(true),
        None,
    );

    let ping = ledger.take_unchecked(sid_a);
    assert_eq!(ping.len(), 1);
    assert!(
        !ping[0].stop_needed,
        "accepted stop is polled with ping, not re-enqueued"
    );
    ledger.finish(ping.into_iter().next().unwrap(), None, None, None);
    assert!(
        !ledger.is_clear(sid_a),
        "transport uncertainty is not absence"
    );

    let absent = ledger.take_unchecked(sid_a);
    ledger.finish(absent.into_iter().next().unwrap(), Some(false), None, None);
    assert!(
        !ledger.is_clear(sid_a),
        "404 ping cannot prove that the separately-owned Streaming Resource was released",
    );

    let close = ledger.take_unchecked(sid_a);
    assert_eq!(close.len(), 1);
    assert!(
        close[0].physical_absent,
        "a known-absent encoder must not be pinged again"
    );
    assert!(
        !close[0].stop_needed,
        "a known-absent encoder must not be stopped again"
    );
    ledger.finish(
        close.into_iter().next().unwrap(),
        Some(false),
        None,
        Some(true),
    );
    assert!(
        ledger.is_clear(sid_a),
        "only the logical close completes exact cleanup"
    );

    assert!(ledger.remember(sid_a, "candidate-retry"));
    let failed_stop = ledger.take_unchecked(sid_a).into_iter().next().unwrap();
    ledger.finish(failed_stop, Some(true), Some(false), None);
    let retry = ledger.take_unchecked(sid_a).into_iter().next().unwrap();
    assert!(
        retry.stop_needed,
        "an unaccepted stop must be retried from later media evidence"
    );
}

/// The live Mandalorian regression, reproduced at the PMS resource boundary.  A raw Part GET
/// first exact-looks up the supplied Streaming Resource and only enters AdHoc MDE when that
/// lookup (and its token alias fallback) misses.  For this file AdHoc rejects Original at
/// `99_341 > 92_000` kbps and PMS 1.43.4 turns the missing decision code into HTTP 500.  The
/// previous implementation CAUSED that path by stopping and exact-closing the live HLS
/// resource before measuring the Part.
///
/// Pin the opposite client ordering: the finite source read borrows the exact active HLS
/// identity and no control-plane request precedes or follows it. This proves the local route
/// remains selected; it deliberately does not infer PMS-side cursor continuity.
#[test]
#[cfg(feature = "devtriggers")]
fn source_probe_reuses_live_hls_resource_instead_of_entering_adhoc_mde() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    use std::io::{BufRead, BufReader, Write};

    let _g = fresh_registry(&mut ps);
    if !crate::net::global_init() || !crate::curlio::available() {
        return;
    }
    const SOURCE_KBPS: u32 = 25_264;
    let source_plan = crate::abr::source_probe_plan(SOURCE_KBPS, crate::abr::PROBE_BUDGET_MS)
        .expect("a measured source has a finite probe plan");
    assert_eq!(
        source_plan.budget_ms, 1_000,
        "the source body and PMS control plane must exercise different horizons",
    );
    let probe_bytes = source_plan.target_bytes;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port() as i32;
    let (tx, rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept source request");
        let mut reader = BufReader::new(socket.try_clone().expect("clone socket"));
        let mut first = String::new();
        reader.read_line(&mut first).expect("request line");
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("request header");
            if line == "\r\n" || line.is_empty() {
                break;
            }
            headers.push(line);
        }
        tx.send((first, headers)).expect("publish request");
        write!(
            socket,
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            probe_bytes - 1,
            probe_bytes * 2,
            probe_bytes,
        )
        .expect("source headers");
        socket
            .write_all(&vec![0x55; probe_bytes])
            .expect("source body");
    });

    let sid = crate::plex::register_for_test(
        "probe-lifecycle",
        "127.0.0.1",
        port,
        "tok",
        "cid-probe-lifecycle",
    );
    let rung = crate::abr::Rung::P480;
    let active = "probe-live";
    install_active_hls(active, "http://fixture.invalid/old.m3u8", rung);
    let active_route = worker_ticket();
    let control = HlsAbrControl {
        trace_generation: 0,
        sid,
        rating_key: "1".into(),
        logical_session: "probe-logical".into(),
        audio_stream_id: 0,
        subtitle_stream_id: 0,
        seconds_per_segment: 2,
        initial_rung: rung,
        initial_observed: None,
        fixture_base: String::new(),
        original_probe_part: "/library/parts/1/file.mkv".into(),
        original_source_kbps: SOURCE_KBPS,
        catalog: crate::abr::HlsActuatorCatalog::measured(),
        prior: None,
        history: crate::abr::TransitionHistory::default(),
        original_features: crate::abr::SourceFeatures::default(),
    };
    let result = control.probe_original_while_hls(&active_route, source_plan);
    assert!(
        matches!(result, OriginalProbeResult::Measured(sample) if sample.target_reached),
        "the finite source response is the measurement: {result:?}",
    );

    let request = rx.recv().expect("captured source request");
    assert!(
        request.0.starts_with("GET /library/parts/1/file.mkv?"),
        "{:?}",
        request.0
    );
    assert!(
        request.0.contains("X-Plex-Session-Identifier=probe-live"),
        "the finite read must exact-reuse the active HLS Streaming Resource: {:?}",
        request.0,
    );
    assert!(
        request
            .1
            .iter()
            .any(|line| line.to_ascii_lowercase().starts_with("range: bytes=0-")),
        "the source experiment must be one finite HTTP response",
    );
    assert_eq!(
        active_encoder(),
        active,
        "a measurement cannot replace the selected client-side HLS route",
    );

    server.join().unwrap();
    install_active_encoder("");
    crate::plex::reset_servers_for_test();
    reset_session(&mut ps);
}

/// PMS 1.43.4 turns an AdHoc bandwidth refusal into 500.  That status is a request failure,
/// not a zero-rate sample, and an optional source check must never make the live HLS route
/// fatal or replace it.
#[test]
#[cfg(feature = "devtriggers")]
fn a_rejected_original_probe_keeps_hls_and_produces_no_capacity_observation() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    use std::io::{BufRead, BufReader, Write};

    let _g = fresh_registry(&mut ps);
    if !crate::net::global_init() || !crate::curlio::available() {
        return;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port() as i32;
    let (tx, rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept source request");
        let mut reader = BufReader::new(socket.try_clone().expect("clone socket"));
        let mut first = String::new();
        reader.read_line(&mut first).expect("request line");
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("request header");
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        tx.send(first).expect("publish request");
        socket
            .write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .expect("server refusal");
    });

    let sid = crate::plex::register_for_test(
        "probe-bodyless",
        "127.0.0.1",
        port,
        "tok",
        "cid-probe-bodyless",
    );
    let rung = crate::abr::Rung::P480;
    let active = "probe-bodyless-live";
    install_active_hls(active, "http://fixture.invalid/old.m3u8", rung);
    let active_route = worker_ticket();
    let control = HlsAbrControl {
        trace_generation: 0,
        sid,
        rating_key: "1".into(),
        logical_session: "probe-bodyless-logical".into(),
        audio_stream_id: 0,
        subtitle_stream_id: 0,
        seconds_per_segment: 2,
        initial_rung: rung,
        initial_observed: None,
        fixture_base: String::new(),
        original_probe_part: "/library/parts/1/file.mkv".into(),
        original_source_kbps: 320,
        catalog: crate::abr::HlsActuatorCatalog::measured(),
        prior: None,
        history: crate::abr::TransitionHistory::default(),
        original_features: crate::abr::SourceFeatures::default(),
    };
    let plan = crate::abr::source_probe_plan(320, crate::abr::PROBE_BUDGET_MS).unwrap();
    let result = control.probe_original_while_hls(&active_route, plan);
    assert_eq!(
        result,
        OriginalProbeResult::Failed {
            outcome: crate::player::report::TraceOutcome::ServerState,
            failure: OriginalProbeFailure::HttpStatus(500),
        },
        "HTTP 500 stays exact for the panel and is never a zero-rate observation",
    );
    let request = rx.recv().expect("captured source request");
    assert!(request.contains("X-Plex-Session-Identifier=probe-bodyless-live"));
    assert_eq!(
        active_encoder(),
        active,
        "the rejected check leaves the client-side HLS route selected",
    );

    server.join().unwrap();
    install_active_encoder("");
    crate::plex::reset_servers_for_test();
    reset_session(&mut ps);
}

/// The worker may finish a bounded response after a concurrent quality change has installed a
/// different HLS resource.  Bytes charged to the old identity are not evidence for the new
/// route: keep the replacement intact and discard the completed sample.
#[test]
#[cfg(feature = "devtriggers")]
fn a_source_sample_from_a_superseded_hls_resource_is_discarded() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    use std::io::{BufRead, BufReader, Write};

    let _g = fresh_registry(&mut ps);
    if !crate::net::global_init() || !crate::curlio::available() {
        return;
    }
    let plan = crate::abr::source_probe_plan(320, crate::abr::PROBE_BUDGET_MS).unwrap();
    let probe_bytes = plan.target_bytes;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept source request");
        let mut reader = BufReader::new(socket.try_clone().expect("clone socket"));
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("request line or header");
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        install_active_hls(
            "probe-new",
            "http://fixture.invalid/new.m3u8",
            crate::abr::Rung::P720,
        );
        write!(
            socket,
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            probe_bytes - 1,
            probe_bytes * 2,
            probe_bytes,
        )
        .expect("source headers");
        socket
            .write_all(&vec![0x55; probe_bytes])
            .expect("source body");
    });

    let sid = crate::plex::register_for_test(
        "probe-stale",
        "127.0.0.1",
        port,
        "tok",
        "cid-probe-stale",
    );
    let active = "probe-old";
    install_active_hls(
        active,
        "http://fixture.invalid/old.m3u8",
        crate::abr::Rung::P480,
    );
    let active_route = worker_ticket();
    let control = HlsAbrControl {
        trace_generation: 0,
        sid,
        rating_key: "1".into(),
        logical_session: "probe-logical".into(),
        audio_stream_id: 0,
        subtitle_stream_id: 0,
        seconds_per_segment: 2,
        initial_rung: crate::abr::Rung::P480,
        initial_observed: None,
        fixture_base: String::new(),
        original_probe_part: "/library/parts/1/file.mkv".into(),
        original_source_kbps: 320,
        catalog: crate::abr::HlsActuatorCatalog::measured(),
        prior: None,
        history: crate::abr::TransitionHistory::default(),
        original_features: crate::abr::SourceFeatures::default(),
    };

    assert_eq!(
        control.probe_original_while_hls(&active_route, plan),
        OriginalProbeResult::Stale,
    );
    assert_eq!(
        active_encoder(),
        "probe-new",
        "the concurrent replacement wins"
    );

    server.join().unwrap();
    install_active_encoder("");
    crate::plex::reset_servers_for_test();
    reset_session(&mut ps);
}

/// Cold Auto measures the Part under the playback's durable logical owner.  The bounded read
/// must not manufacture a `source-N` identity or exact-close the resource before the selected
/// Original/HLS route can reuse it.
#[test]
#[cfg(feature = "devtriggers")]
fn cold_source_preflight_uses_the_playback_identity_and_does_not_close_it() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    use std::io::{BufRead, BufReader, Write};

    let _g = fresh_registry(&mut ps);
    if !crate::net::global_init() || !crate::curlio::available() {
        return;
    }
    let plan = crate::abr::source_probe_plan(320, crate::abr::PROBE_BUDGET_MS).unwrap();
    let probe_bytes = plan.target_bytes;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    let (tx, rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept cold source request");
        let mut reader = BufReader::new(socket.try_clone().expect("clone socket"));
        let mut requests = Vec::new();
        let mut first = String::new();
        reader.read_line(&mut first).expect("request line");
        requests.push(first);
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("request header");
            if line == "\r\n" || line.is_empty() {
                break;
            }
            requests.push(line);
        }
        write!(
            socket,
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            probe_bytes - 1,
            probe_bytes * 2,
            probe_bytes,
        )
        .expect("source headers");
        socket
            .write_all(&vec![0x55; probe_bytes])
            .expect("source body");
        drop(socket);

        listener.set_nonblocking(true).unwrap();
        for _ in 0..50 {
            match listener.accept() {
                Ok((socket, _)) => {
                    let mut extra = String::new();
                    BufReader::new(socket)
                        .read_line(&mut extra)
                        .expect("extra request line");
                    requests.push(extra);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(4));
                }
                Err(error) => panic!("accept extra request: {error}"),
            }
        }
        tx.send(requests).expect("publish cold request set");
    });

    let sid = crate::plex::register_for_test(
        "probe-cold",
        "127.0.0.1",
        port,
        "tok",
        "cid-probe-cold",
    );
    let client = crate::plex::client_for(sid).expect("test server installed");
    let url = client
        .direct_play_url("/library/parts/1/file.mkv", "cold-logical")
        .to_url();
    let sample = measure_remote_original(&url, 320).expect("completed cold sample");
    assert!(sample.completed);

    let requests = rx.recv().expect("captured cold requests");
    assert_eq!(
        requests
            .iter()
            .filter(|line| line.starts_with("GET ") || line.starts_with("POST "))
            .count(),
        1,
        "the preflight is one bounded Part request and no exact-close: {requests:?}",
    );
    assert!(requests[0].starts_with("GET /library/parts/1/file.mkv?"));
    assert!(requests[0].contains("X-Plex-Session-Identifier=cold-logical"));
    assert!(
        requests
            .iter()
            .any(|line| line.to_ascii_lowercase().starts_with("range: bytes=0-")),
        "the logical resource is sampled with one finite response",
    );

    server.join().unwrap();
    crate::plex::reset_servers_for_test();
    reset_session(&mut ps);
}

// ---- the two reads that FEED the ceiling: which detail describes the leaf, and at what rate



// ---- pick_dp_audio: the direct-play audio selection ladder ------------------------------
// Never host-testable before: it read `metadata::playing()`'s `&'static` store. Making it
// take the tracks explicitly (step 6 of docs/async-model-decision.md) turned the ladder into
// a pure function, and these pin the order the comments claim.







// ---- rung 1: the selection the SERVER already holds --------------------------------------
// `Stream.selected` is the part's current pick — what `put_selection` writes and what a pick
// made on a phone / Plex Web / another TV shows up as. We wrote it for a long time and never
// read it, so our own ladder silently overwrote every cross-client choice on the next play.
// The shapes below are the ones the live server actually serves (probed per-identity while
// this landed), which is where the two gates on the rung come from.





// ---- pick_dp_subtitle: the read-back half of put_selection -------------------------------






// ---- video_direct_plays: the local codec + resolution + Dolby Vision direct-play gate ----







// ---- video_direct_plays: the local codec + resolution direct-play gate -------------------






/// The preview's THIRD answer, which is the one the UI hangs a Plex Pass claim on.
///
/// While `Preview` had two values, everything that was not a direct play collapsed into
/// `Converts` — and `detail::play_note` read that as "the server re-encodes the picture", which
/// is false for the two cases below: `build_stream` answers both of them with
/// `plan.remux = video_dp`, i.e. ask Plex to copy the codecs into MKV. So a 4K HDR HEVC file in
/// a `.mov`, and any mkv whose only fault is an audio track that must be converted, drew
/// "HDR → SDR · tone-mapping needs \[PLEX PASS\]" on a proven-Pass-less server while the picture
/// arrived HDR10 intact. This grades the SPLIT; the truth table it feeds is `detail.rs`'s.
///
/// The device table is `Caps::assumed` here (nothing in the host suite calls `devcaps::probe`),
/// so h264 at 3840×2160 clears the codec and resolution gates and the container/audio halves
/// are what move.
#[test]
fn the_preview_tells_a_container_remux_apart_from_a_re_encode() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::Original);
    fn item(vcodec: &str, part: &str, acodec: &str) -> crate::metadata::Detail {
        crate::metadata::Detail {
            vcodec: vcodec.to_string(),
            part: part.to_string(),
            width: 3840,
            height: 2160,
            audio: vec![crate::metadata::Stream {
                codec: acodec.to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }
    const MKV: &str = "/library/parts/1/2/file.mkv";
    const MOV: &str = "/library/parts/1/2/file.mov";
    // we pull the file ourselves — nothing on the server touches it
    assert_eq!(
        playback_preview(&item("h264", MKV, "aac")),
        Some(Preview::DirectPlay)
    );
    // the container is one the buffer-feed demuxer cannot stream → the server REPACKAGES it
    assert_eq!(
        playback_preview(&item("h264", MOV, "aac")),
        Some(Preview::Remux)
    );
    // …and so it does for a streamable container whose only audio track has to be converted
    assert_eq!(
        playback_preview(&item("h264", MKV, "truehd")),
        Some(Preview::Remux)
    );
    // a codec the pipeline cannot decode at all is the only real re-encode
    assert_eq!(
        playback_preview(&item("vp9", MKV, "aac")),
        Some(Preview::Converts)
    );
    // …including when the container and the audio would otherwise have been fine
    assert_eq!(
        playback_preview(&item("vp9", MOV, "truehd")),
        Some(Preview::Converts)
    );
    // nothing playable loaded (a show still resolving its episode) answers nothing at all
    assert_eq!(playback_preview(&item("h264", "", "aac")), None);
}

#[test]
fn restored_sidecar_is_part_of_the_route_contract() {
    let mut ps = PlaybackSession::IDLE;
    let _guard = fresh_registry(&mut ps);
    let sid = ServerId::from_raw(0);
    let mut meta = crate::stores::metadata::MetadataStore::default();
    let playing = Some(fourk_item_with_subs(
        sid, vec![], vec![crate::metadata::Stream {
            id: 77, external: true, selected: true, codec: "srt".into(),
            key: "/library/streams/77".into(), ..Default::default()
        }],
    ));
    let start = super::apply_plan(&mut ps, &mut meta, Plan {
        sid, playing, url: "http://fixture.invalid/movie.mkv".into(), ..Default::default()
    }, "rk-4k");
    settle_plan_start_in_unit_test(&mut ps, start);
    assert_eq!(cur_sub_sid(&ps), 77, "timeline and later audio/quality transcodes must retain the restored sidecar");
    crate::player::sidecar::reset();
}
#[test]
#[cfg(feature = "devtriggers")]
fn sidecar_on_invalidates_a_pending_original_recovery() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::Original);
    apply_plan(&mut ps,
        Plan {
            url: "http://fixture.invalid/4000/master.m3u8".into(),
            tsession: "encoder-subtitle-on".into(),
            delivery: crate::plex::TranscodeDelivery::FixedHls {
                seconds_per_segment: 2,
            },
            ceiling: Some(crate::abr::Rung::P720.ceiling()),
            auto_original: Some(test_original_candidate(None)),
            ..Default::default()
        },
        "rk-subtitle-on",
    );
    request_user_route_intent(&ps, UserRouteIntent::RecoverOriginal);

    commit_subtitle_selection(&mut ps, -1, 88);

    assert!(ps.auto_original.is_none());
    let action = claim_route_action().expect("the burned subtitle needs HLS retranscode");
    assert_eq!(
        action.intent,
        RouteIntent::User(UserRouteIntent::Retranscode)
    );
    finish_route_action(&mut ps, &action, RouteApplyResult::Prepared);

    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
    crate::player::reset_subtitle();
}
