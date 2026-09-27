//! Strict original-stream override and ordinary disabled-direct-play routing.
use super::*;
use super::test_support::*;
#[cfg(feature = "devtriggers")]
use std::time::Duration;

#[test]
fn direct_play_modes_compose_with_each_quality_ceiling() {
    for q in QUALITY_LADDER {
        for measured in [(0, 0, 0), (3_000, 1280, 720), (60_000, 3840, 2160)] {
            let ordinary = quality_policy(q, false, measured.0, measured.1, measured.2);
            let auto = direct_play_policy(DirectPlayMode::Auto, ordinary);
            assert_eq!((auto.direct_play, auto.remux), (ordinary.direct_play, ordinary.remux));
            let disabled = direct_play_policy(DirectPlayMode::Disabled, ordinary);
            assert!(!disabled.direct_play);
            assert_eq!(disabled.remux, ordinary.remux);
            let forced = direct_play_policy(DirectPlayMode::Forced, ordinary);
            assert!(forced.direct_play && !forced.remux);
        }
    }
}

#[test]
#[cfg(feature = "devtriggers")]
fn force_registers_original_despite_saved_quality_relay_and_device_raster() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(crate::net::global_init());
    let (port, rx, server) = plan_pms(2, MDE_DIRECTPLAY);
    let sid = crate::plex::register_for_test("forced-original", "127.0.0.1", port, "token", "forced-client");
    crate::plex::client_for(sid).unwrap().set_link(crate::plex::probe::Location::Relay);
    restore_quality(Quality::P480);
    restore_direct_play_mode(DirectPlayMode::Forced);
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    let mut item = fourk_item(sid, vec![eac3_track()]);
    item.width = 7680;
    item.height = 4320;
    env.cached_item = Some(item);
    let plan = build_stream("rk-4k", "/library/parts/36013/1/file.mkv", "hevc", "eac3", &env);
    let requests = rx.recv_timeout(Duration::from_secs(15)).unwrap();
    server.join().unwrap();
    assert!(plan.url.contains("/library/parts/36013/"));
    assert!(plan.tsession.is_empty() && plan.ceiling.is_none() && !plan.remux);
    assert!(plan.auto_original.is_none() && !plan.auto_original_watched);
    assert_eq!(plan.direct_play_mode, DirectPlayMode::Forced);
    let decision = requests.iter().find(|r| r.contains("/decision?")).unwrap();
    assert_eq!(query_param(decision, "directPlay"), Some("1"));
    assert_eq!(query_param(decision, "directStream"), Some("0"));
    assert!(!requests.iter().any(|r| r.contains("start.") || r.starts_with("PUT ")));
    assert_eq!(quality(), Quality::P480, "Force must retain the saved ceiling");
    restore_quality(Quality::Original);
    restore_direct_play_mode(DirectPlayMode::Auto);
    crate::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn force_server_refusal_or_missing_mde_never_attempts_conversion() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(crate::net::global_init());
    for body in [MDE_TRANSCODE, EMPTY_MC] {
        let (port, rx, server) = plan_pms(2, body);
        let sid = crate::plex::register_for_test("forced-refusal", "127.0.0.1", port, "token", "forced-client");
        let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
        env.direct_play_mode = DirectPlayMode::Forced;
        env.cached_item = Some(fourk_item(sid, vec![eac3_track()]));
        let plan = build_stream("rk-4k", "/library/parts/36013/1/file.mkv", "hevc", "eac3", &env);
        let requests = rx.recv_timeout(Duration::from_secs(15)).unwrap();
        server.join().unwrap();
        assert!(plan.url.is_empty() && plan.tsession.is_empty());
        assert!(plan.verdict.as_deref().unwrap().contains("Return Direct Play to Auto"));
        assert_eq!(requests.iter().filter(|r| r.contains("/decision?")).count(), 1);
        assert!(!requests.iter().any(|r| r.starts_with("PUT ") || r.contains("start.")));
        crate::plex::reset_servers_for_test();
    }
}

#[test]
#[cfg(feature = "devtriggers")]
fn disabling_direct_play_keeps_codec_preserving_remux() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(crate::net::global_init());
    let (port, rx, server) = plan_pms(3, MDE_TRANSCODE_COPY);
    let sid = crate::plex::register_for_test("disabled-original", "127.0.0.1", port, "token", "disabled-client");
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.direct_play_mode = DirectPlayMode::Disabled;
    env.cached_item = Some(fourk_item(sid, vec![eac3_track()]));
    let plan = build_stream("rk-4k", "/library/parts/36013/1/file.mkv", "hevc", "eac3", &env);
    let requests = rx.recv_timeout(Duration::from_secs(15)).unwrap();
    server.join().unwrap();
    assert!(plan.remux && plan.url.contains("start.mkv") && !plan.tsession.is_empty());
    assert!(!requests.iter().any(|r| query_param(r, "directPlay") == Some("1")));
    crate::plex::reset_servers_for_test();
}

#[test]
fn force_retains_feed_limits_and_session_snapshot_across_retry_and_track_edits() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    ps.direct_play_mode = DirectPlayMode::Forced;
    restore_direct_play_mode(DirectPlayMode::Disabled);
    assert!(forced_direct_play(&ps));
    assert_eq!(current_retry_context(&ps, 123).direct_play_mode, DirectPlayMode::Forced);
    assert!(forced_direct_play(&ps.publication()));
    assert!(audio_track_direct_plays(&ps, "dts", 8), "Force bypasses device channels");
    assert!(!audio_track_direct_plays(&ps, "truehd", 8));
    assert!(!audio_track_direct_plays(&ps, "", 0));
    assert!(!video_feed_supported("vp9", crate::metadata::DvPresentation::NotDv));
    let blocked_dv = crate::metadata::Dovi { present: true, profile: 5, bl_compat: 0, ..crate::metadata::Dovi::NONE }
        .presentation(false, crate::webos::caps::DvCapability::Unsupported, true);
    assert!(!video_feed_supported("hevc", blocked_dv));
    assert!(hls_abr_control(&ps).is_none());
    assert!(auto_original_watch(&ps).is_none());
    assert!(fallback_auto_to_hls(&mut ps, 1000, 0).is_none());
    assert!(transcode_seek(&mut ps, 0).is_none());
    assert!(recover_auto_to_original(&mut ps, 0).is_none());
    commit_audio_selection(&mut ps, 0, "truehd", 9, 8);
    assert!(play_verdict(&ps).unwrap().contains("Return Direct Play to Auto"));
    assert_eq!(cur_audio_sid(&ps), 0, "refusing an unsupported track leaves the current selection intact");
    restore_direct_play_mode(DirectPlayMode::Auto);
}

#[test]
fn disabled_mode_refuses_an_original_only_url_without_a_pms_item() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), unregistered_sid(), "");
    env.direct_play_mode = DirectPlayMode::Disabled;
    let plan = build_stream("", "/movie.mkv", "h264", "aac", &env);
    assert!(plan.url.is_empty());
    assert!(plan.verdict.as_deref().unwrap().contains("Direct Play is disabled"));
}
