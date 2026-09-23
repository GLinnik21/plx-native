//! PMS plan/decision tests: plan round-trips, MDE transcode decisions and subtitle
//! selection.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::apply_plan;

/// The identity round trip: `request_play`'s captured id reaches `cur_sid` unchanged, through
/// `ResolveEnv` (the main-thread snapshot) and `Plan` (the worker's output).
///
/// Graded on the resolve that FAILS, deliberately — it is the exit `build_stream` takes first,
/// before any network, and a plan that carries no server is one `apply_plan` cannot install an
/// honest `cur_sid` from. Every richer exit builds on the same field.
#[test]
#[cfg(feature = "devtriggers")]
fn a_plan_round_trips_the_server_the_request_captured() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let sid = unregistered_sid();

    let env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-7");
    assert_eq!(
        env.sid, sid,
        "the snapshot carries the id the request was made with"
    );

    let plan = build_stream("rk-7", "/library/parts/5/1/f.mkv", "h264", "ac3", &env);
    assert_eq!(
        plan.sid, sid,
        "a plan that could not resolve still names its server"
    );
    assert!(
        plan.url.is_empty(),
        "no client for that slot, so nothing resolved"
    );
    assert_eq!(
        plan.part_id, 5,
        "…and the rest of the plan is built as usual"
    );

    apply_plan(&mut ps, plan, "rk-7");
    assert_eq!(cur_sid(&ps), sid, "the installed identity is the captured one");
    assert_eq!(cur_rk(&ps), "rk-7", "and its other half");
}

/// **A plan that never reached the codec gate must not claim the source is undecodable.**
///
/// `Plan::source_decodable` is a `bool`, `bool::default()` is `false`, and `false` is a CLAIM —
/// the quality menu renders it as "Converts on server" on the Original row. `build_stream` has
/// an exit (no client for the server slot) that returns before the gate runs at all, so the
/// value has to be `true` on the way in and be overwritten by evidence, not the other way
/// round.
///
/// Differential: with the initializer's explicit `true` removed this fails, and the failure is
/// a line of copy asserting something about a file nobody opened.
#[test]
fn a_plan_that_never_resolved_makes_no_claim_about_the_source() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let sid = unregistered_sid();
    let env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-7");

    let plan = build_stream("rk-7", "/library/parts/5/1/f.mkv", "h264", "ac3", &env);
    assert!(
        plan.url.is_empty(),
        "the test needs the exit that precedes the codec gate"
    );
    assert!(
        plan.source_decodable,
        "nobody looked at this file, so nothing may be said about it",
    );
    apply_plan(&mut ps, plan, "rk-7");
    assert!(
        source_decodable(&ps),
        "and the session carries the same silence"
    );
}

#[test]
#[cfg(feature = "devtriggers")]
fn remux_review_probe_installs_effective_selection_before_decision_and_start() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(crate::net::global_init() && crate::curlio::available());
    restore_quality(Quality::Auto);
    // Cold client-rendered subtitles stay off server-side; an explicit burn remains a burn.
    for burn in [0, 9] {
        let (port, done, server) = selection_probe_pms(false, burn);
        let sid = crate::plex::register_for_test("selection-probe", "127.0.0.1", port, "token", "selection-probe-client");
        crate::plex::client_for(sid).unwrap().set_link(crate::plex::probe::Location::Remote);
        let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
        env.audio_sid = 1;
        env.sub_sid = burn;
        let mut item = fourk_item_with_subs(sid, vec![
            crate::metadata::Stream { id: 1, index: 0, lang_code: "eng".into(), codec: "truehd".into(), channels: 8, default: true, selected: true, ..Default::default() },
            crate::metadata::Stream { id: 2, index: 1, lang_code: "eng".into(), codec: "ac3".into(), channels: 6, ..Default::default() },
        ], vec![selected_sub(9, "srt")]);
        item.bitrate = 320;
        env.cached_item = Some(item);
        let plan = build_stream("rk-4k", "/library/parts/36013/1/file.mkv", "hevc", "truehd", &env);
        done.send(()).unwrap();
        let requests = server.join().unwrap();
        let decision = requests.iter().position(|(r, _)| r.contains("/decision?") && !r.contains("hasMDE=1")).expect("probe decision");
        let put = requests.iter().position(|(r, _)| r.starts_with("PUT /library/parts/")).expect("selection PUT");
        assert!(put < decision, "PUT must precede probe decision: {requests:?}");
        let start = requests.iter().position(|(r, _)| r.contains("start.mkv")).expect("sample GET");
        assert!(decision < start);
        for index in [decision, start] {
            let (request, state) = &requests[index];
            assert_eq!(*state, (2, burn), "effective PMS selection");
            assert_eq!(query_param(request, "audioStreamID"), Some("2"));
            // Progressive requests omit zero; the prior PUT is what suppresses defaults.
            assert_eq!(query_param(request, "subtitleStreamID"), if burn == 0 { None } else { Some("9") });
            assert_eq!(query_param(request, "subtitles"), if burn == 0 { None } else { Some("burn") });
        }
        assert!(plan.remux && plan.url.contains("start.mkv"), "state-dependent sample must admit remux: {}", plan.url);
    }
    restore_quality(Quality::Original);
    crate::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn remux_review_http_200_refusal_never_gets_media() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(crate::net::global_init() && crate::curlio::available());
    let (port, done, server) = selection_probe_pms(true, 0);
    let sid = crate::plex::register_for_test("refused-probe", "127.0.0.1", port, "token", "refused-probe-client");
    let sample = measure_remote_remux(crate::plex::client_for(sid).unwrap(), "rk", "refused-session", 2, 0, 320);
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    assert!(sample.is_none());
    assert!(requests.iter().any(|(r, _)| r.contains("/decision?")));
    assert!(!requests.iter().any(|(r, _)| r.contains("start.mkv")), "refusal must prevent media GET: {requests:?}");
    assert!(!requests.iter().any(|(r, _)| r.contains("closeResourceSession=1")));
    crate::plex::reset_servers_for_test();
}

/// PMS 1.43 503s a Part GET whose session has no MDE decision. A 4K HEVC+EAC3 title used
/// to skip `/decision` because EAC3 is a direct-play audio codec; the plan URL must not
/// be that part until MDE has registered the session.
#[test]
#[cfg(feature = "devtriggers")]
fn original_hevc_eac3_registers_mde_before_returning_the_part() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, rx, server) = plan_pms(2, MDE_DIRECTPLAY);
    let sid = crate::plex::register_for_test(
        "mde-dp-test",
        "127.0.0.1",
        port,
        "token",
        "mde-dp-client",
    );
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.cached_item = Some(fourk_item(sid, vec![eac3_track()]));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "eac3",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    let decision = requests
        .iter()
        .find(|line| line.contains("/decision?"))
        .unwrap_or_else(|| panic!("MDE was never asked: {requests:?}"));
    assert!(decision.contains("hasMDE=1"), "{decision}");
    assert!(decision.contains("directPlay=1"), "{decision}");
    assert!(decision.contains("audioStreamID=36014"), "{decision}");
    assert_eq!(
        query_param(decision, "subtitleStreamID"),
        Some("0"),
        "no selectable embedded sub → explicit 0: {decision}"
    );
    assert_eq!(
        query_param(decision, "subtitles"),
        Some("none"),
        "MDE must name client-rendered mode; auto 400s a selected sub: {decision}"
    );
    assert!(
        plan.url.contains("/library/parts/36013/"),
        "admitted Original is the raw part: {}",
        plan.url
    );
    assert!(
        !plan.url.contains("start.mkv"),
        "MDE said directplay, so this is not a transcode: {}",
        plan.url
    );
    crate::plex::reset_servers_for_test();
}

/// Smart-DP used to skip MDE so a TrueHD default would not veto the AC3 sibling. The
/// `/decision` query must name that sibling; otherwise PMS evaluates TrueHD and the part
/// GET 503s or the title is sent to a video-downscaling transcode.
#[test]
#[cfg(feature = "devtriggers")]
fn smart_dp_names_the_ac3_sibling_on_mde() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, rx, server) = plan_pms(2, MDE_DIRECTPLAY);
    let sid = crate::plex::register_for_test(
        "mde-smart-dp",
        "127.0.0.1",
        port,
        "token",
        "mde-smart-client",
    );
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.cached_item = Some(fourk_item(
        sid,
        vec![
            crate::metadata::Stream {
                id: 1,
                index: 0,
                lang_code: "eng".into(),
                codec: "truehd".into(),
                channels: 8,
                default: true,
                selected: true,
                ..Default::default()
            },
            crate::metadata::Stream {
                id: 2,
                index: 1,
                lang_code: "eng".into(),
                codec: "ac3".into(),
                channels: 6,
                ..Default::default()
            },
        ],
    ));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "truehd",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    let decision = requests
        .iter()
        .find(|line| line.contains("/decision?"))
        .unwrap_or_else(|| panic!("MDE was never asked: {requests:?}"));
    assert!(
        decision.contains("audioStreamID=2"),
        "MDE must see the AC3 sibling, not TrueHD: {decision}"
    );
    assert_eq!(
        query_param(decision, "subtitleStreamID"),
        Some("0"),
        "{decision}"
    );
    assert!(plan.url.contains("/library/parts/36013/"), "{}", plan.url);
    crate::plex::reset_servers_for_test();
}

/// OpenAPI: a Part GET whose decision is a transcode is HTTP 503. Honour MDE rather than
/// returning the part URL the local codec test would have chosen. A `/decision` body that
/// names no video stream cannot claim the video must re-encode, so this unnamed shape remuxes.
#[test]
#[cfg(feature = "devtriggers")]
fn mde_transcode_does_not_return_the_part_url() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, rx, server) = plan_pms(4, MDE_TRANSCODE);
    let sid = crate::plex::register_for_test(
        "mde-tc-test",
        "127.0.0.1",
        port,
        "token",
        "mde-tc-client",
    );
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.cached_item = Some(fourk_item(sid, vec![eac3_track()]));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "eac3",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    assert!(
        requests
            .iter()
            .any(|line| line.contains("hasMDE=1") && line.contains("directPlay=1")),
        "the first ask is still the MDE handshake: {requests:?}"
    );
    assert!(
        plan.url.contains("start.mkv"),
        "MDE said transcode, so the plan must not GET the part: {}",
        plan.url
    );
    assert!(
        !plan.url.contains("/library/parts/36013/"),
        "a transcode decision must not be served as Original: {}",
        plan.url
    );
    assert!(
        plan.remux,
        "Part.decision=transcode with no Stream[] cannot claim a video re-encode: {}",
        plan.url
    );
    crate::plex::reset_servers_for_test();
}

/// HEVC+TrueHD with no AAC/AC3/EAC3 sibling: MDE transcodes the part (TrueHD is not in the
/// profile) but the video can still be copied. A Part-level veto would re-encode 4K for an
/// audio problem.
#[test]
#[cfg(feature = "devtriggers")]
fn mde_transcode_for_truehd_only_still_remuxes() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, rx, server) = plan_pms(4, MDE_TRANSCODE_COPY);
    let sid = crate::plex::register_for_test(
        "mde-truehd",
        "127.0.0.1",
        port,
        "token",
        "mde-truehd-client",
    );
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.cached_item = Some(fourk_item(
        sid,
        vec![crate::metadata::Stream {
            id: 36014,
            index: 1,
            lang_code: "eng".into(),
            codec: "truehd".into(),
            channels: 8,
            default: true,
            selected: true,
            ..Default::default()
        }],
    ));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "truehd",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    assert!(
        requests
            .iter()
            .any(|line| line.contains("/decision?") && line.contains("hasMDE=1")),
        "MDE was still asked: {requests:?}"
    );
    assert!(
        !plan.url.contains("/library/parts/36013/"),
        "TrueHD-only cannot Original: {}",
        plan.url
    );
    assert!(
        plan.remux,
        "TrueHD-only must codec-copy remux, not re-encode 4K: {}",
        plan.url
    );
    crate::plex::reset_servers_for_test();
}

/// A video-stream `transcode` (bit depth past the profile, …) is the copy veto. Remux here
/// would ship pixels the decoder cannot take.
#[test]
#[cfg(feature = "devtriggers")]
fn mde_video_stream_transcode_forbids_remux() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, rx, server) = plan_pms(4, MDE_TRANSCODE_VIDEO);
    let sid = crate::plex::register_for_test(
        "mde-vid-tc",
        "127.0.0.1",
        port,
        "token",
        "mde-vid-tc-client",
    );
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.cached_item = Some(fourk_item(sid, vec![eac3_track()]));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "eac3",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    assert!(
        requests
            .iter()
            .any(|line| line.contains("/decision?") && line.contains("hasMDE=1")),
        "MDE was still asked: {requests:?}"
    );
    assert!(
        !plan.url.contains("/library/parts/36013/"),
        "video transcode must not Original: {}",
        plan.url
    );
    assert!(
        !plan.remux,
        "video-stream transcode forbids a codec-copy remux"
    );
    crate::plex::reset_servers_for_test();
}

/// Declared Profile 5 can Original, but a remux copy carries no `DolbyHdrInfo`. MDE's
/// video=`copy` (the measured P5 shape) must not override `no_video_copy`.
#[test]
#[cfg(feature = "devtriggers")]
fn mde_transcode_copy_still_refuses_a_profile_5_remux() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, rx, server) = plan_pms(4, MDE_TRANSCODE_COPY);
    let sid = crate::plex::register_for_test(
        "mde-p5-copy",
        "127.0.0.1",
        port,
        "token",
        "mde-p5-copy-client",
    );
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    let mut item = fourk_item(sid, vec![eac3_track()]);
    item.dovi = p5();
    env.cached_item = Some(item);
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "eac3",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    assert!(
        requests
            .iter()
            .any(|line| line.contains("/decision?") && line.contains("hasMDE=1")),
        "declared P5 still asks MDE: {requests:?}"
    );
    assert!(
        !plan.remux,
        "a P5 remux is the IPT-PQ bitstream with no declaration: {}",
        plan.url
    );
    assert!(
        plan.no_video_copy,
        "the copy permission has to be withdrawn or PMS copies anyway"
    );
    crate::plex::reset_servers_for_test();
}

/// Remote Auto used to probe the Part after MDE registered transcode, which 503s, so bootstrap
/// fell through to HLS and re-encoded 4K for an audio-only veto. The probe has to sample the
/// remux `start.mkv` we would actually play.
#[test]
#[cfg(feature = "devtriggers")]
fn remote_auto_truehd_remux_probes_start_mkv_not_the_part() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    if !crate::net::global_init() || !crate::curlio::available() {
        return;
    }
    restore_quality(Quality::Auto);
    let probe_bytes = crate::abr::source_probe_plan(320, crate::abr::PROBE_BUDGET_MS)
        .expect("tiny source still has a probe object")
        .target_bytes;
    let (port, rx, server) = plan_pms_with_start_mkv(6, MDE_TRANSCODE_COPY, probe_bytes);
    let sid = crate::plex::register_for_test(
        "mde-remote-truehd",
        "127.0.0.1",
        port,
        "token",
        "mde-remote-truehd-client",
    );
    crate::plex::client_for(sid)
        .expect("registered")
        .set_link(crate::plex::probe::Location::Remote);
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    let mut item = fourk_item(
        sid,
        vec![crate::metadata::Stream {
            id: 36014,
            index: 1,
            lang_code: "eng".into(),
            codec: "truehd".into(),
            channels: 8,
            default: true,
            selected: true,
            ..Default::default()
        }],
    );
    item.bitrate = 320;
    env.cached_item = Some(item);
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "truehd",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    assert!(
        requests
            .iter()
            .any(|line| line.contains("/decision?") && line.contains("hasMDE=1")),
        "MDE was still asked: {requests:?}"
    );
    assert!(
        !requests
            .iter()
            .any(|line| line.contains("GET /library/parts/")),
        "a Part GET after transcode MDE is 503: {requests:?}"
    );
    assert!(
        requests.iter().any(|line| line.contains("start.mkv")),
        "Remote Auto must probe the remux: {requests:?}"
    );
    assert!(
        plan.remux,
        "TrueHD-only Remote Auto must codec-copy remux, not HLS-encode 4K: {}",
        plan.url
    );
    assert!(
        plan.url.contains("start.mkv"),
        "the installed route is the remux: {}",
        plan.url
    );
    assert!(
        !requests.iter().any(|line| line.contains("/transcode/universal/stop")),
        "a successful remux Original leaves the session for the play-path decision: {requests:?}"
    );
    restore_quality(Quality::Original);
    crate::plex::reset_servers_for_test();
}

/// TrueHD default `id=1` is what `env.audio_sid` still carries at resolve start.
/// MDE, the remux probe, the play-path PUT, and the installed start.mkv must all name the
/// AC3 sibling `id=2` that smart-DP will actually feed.
#[test]
#[cfg(feature = "devtriggers")]
fn remote_auto_truehd_remux_probe_names_the_ac3_sibling_not_env_audio_sid() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    if !crate::net::global_init() || !crate::curlio::available() {
        return;
    }
    restore_quality(Quality::Auto);
    let probe_bytes = crate::abr::source_probe_plan(320, crate::abr::PROBE_BUDGET_MS)
        .expect("tiny source still has a probe object")
        .target_bytes;
    let (port, rx, server) = plan_pms_with_start_mkv(6, MDE_TRANSCODE_COPY, probe_bytes);
    let sid = crate::plex::register_for_test(
        "mde-remote-truehd-ids",
        "127.0.0.1",
        port,
        "token",
        "mde-remote-truehd-ids-client",
    );
    crate::plex::client_for(sid)
        .expect("registered")
        .set_link(crate::plex::probe::Location::Remote);
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.audio_sid = 1;
    let mut item = fourk_item(
        sid,
        vec![
            crate::metadata::Stream {
                id: 1,
                index: 0,
                lang_code: "eng".into(),
                codec: "truehd".into(),
                channels: 8,
                default: true,
                selected: true,
                ..Default::default()
            },
            crate::metadata::Stream {
                id: 2,
                index: 1,
                lang_code: "eng".into(),
                codec: "ac3".into(),
                channels: 6,
                ..Default::default()
            },
        ],
    );
    item.bitrate = 320;
    env.cached_item = Some(item);
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "truehd",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    let mde = requests
        .iter()
        .find(|line| line.contains("/decision?") && line.contains("hasMDE=1"))
        .unwrap_or_else(|| panic!("MDE was never asked: {requests:?}"));
    assert_eq!(
        query_param(mde, "audioStreamID"),
        Some("2"),
        "MDE must see the AC3 sibling, not env.audio_sid=1: {mde}"
    );
    let remux_probe = requests
        .iter()
        .find(|line| {
            line.contains("start.mkv")
                || (line.contains("/decision?")
                    && line.contains("directPlay=0")
                    && !line.contains("hasMDE=1"))
        })
        .unwrap_or_else(|| panic!("remux probe was never asked: {requests:?}"));
    assert_eq!(
        query_param(remux_probe, "audioStreamID"),
        Some("2"),
        "remux probe must name the AC3 sibling, not env.audio_sid=1: {remux_probe}"
    );
    let put = requests
        .iter()
        .find(|line| line.contains("PUT /library/parts/"))
        .unwrap_or_else(|| panic!("play-path PUT was never asked: {requests:?}"));
    assert_eq!(
        query_param(put, "audioStreamID"),
        Some("2"),
        "PUT must select the AC3 sibling, not env.audio_sid=1: {put}"
    );
    assert!(
        plan.url.contains("start.mkv"),
        "Remote Auto remux installs start.mkv: {}",
        plan.url
    );
    assert_eq!(
        query_param(&plan.url, "audioStreamID"),
        Some("2"),
        "playback URL must name the AC3 sibling, not env.audio_sid=1: {}",
        plan.url
    );
    restore_quality(Quality::Original);
    crate::plex::reset_servers_for_test();
}

/// 720p denies remux, so the encoder can transcode the selected DTS. Putting the smart-DP
/// AC3 sibling here replaced English with the Russian default (reproduced on PMS 1.43.4).
#[test]
#[cfg(feature = "devtriggers")]
fn a_720p_reencode_puts_the_selected_dts_not_the_ac3_sibling() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::P720);
    let (port, rx, server) = plan_pms(3, EMPTY_MC);
    let sid = crate::plex::register_for_test(
        "reencode-selected-dts",
        "127.0.0.1",
        port,
        "token",
        "reencode-selected-dts-client",
    );
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.quality = Quality::P720;
    env.src_kbps = 48_000;
    env.audio_sid = 0;
    env.cached_item = Some(fourk_item(
        sid,
        vec![
            crate::metadata::Stream {
                id: 2663,
                index: 0,
                lang_code: "rus".into(),
                codec: "ac3".into(),
                channels: 6,
                default: true,
                ..Default::default()
            },
            crate::metadata::Stream {
                id: 2669,
                index: 1,
                lang_code: "eng".into(),
                codec: "dca".into(),
                channels: 6,
                selected: true,
                ..Default::default()
            },
        ],
    ));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "dca",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    assert!(
        !plan.remux,
        "720p must re-encode, not copy: {}",
        plan.url
    );
    assert!(
        plan.url.contains("start.mkv"),
        "re-encode installs start.mkv: {}",
        plan.url
    );
    let put = requests
        .iter()
        .find(|line| line.contains("PUT /library/parts/"))
        .unwrap_or_else(|| panic!("play-path PUT was never asked: {requests:?}"));
    assert_eq!(
        query_param(put, "audioStreamID"),
        Some("2669"),
        "PUT must keep selected English DTS, not the Russian AC3 sibling: {put}"
    );
    assert_eq!(
        query_param(&plan.url, "audioStreamID"),
        Some("2669"),
        "start.mkv must name that DTS, not the AC3 sibling: {}",
        plan.url
    );
    assert_eq!(plan.audio_sid, 2669);
    restore_quality(Quality::Original);
    crate::plex::reset_servers_for_test();
}

/// #202 end to end on the re-encode path: a French file whose default (echoed back as
/// `selected`) is French, beside an English track, with no Plex language preference. The 720p
/// re-encode must PUT and name the French default — no built-in English outranks the file.
#[test]
#[cfg(feature = "devtriggers")]
fn a_720p_reencode_keeps_the_files_default_language_over_english() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::P720);
    let (port, rx, server) = plan_pms(3, EMPTY_MC);
    let sid = crate::plex::register_for_test(
        "reencode-default-echo",
        "127.0.0.1",
        port,
        "token",
        "reencode-default-echo-client",
    );
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.quality = Quality::P720;
    env.src_kbps = 48_000;
    env.audio_sid = 0;
    env.cached_item = Some(fourk_item(
        sid,
        vec![
            crate::metadata::Stream {
                id: 10975,
                index: 0,
                lang_code: "fre".into(),
                codec: "eac3".into(),
                channels: 6,
                default: true,
                selected: true,
                ..Default::default()
            },
            crate::metadata::Stream {
                id: 10976,
                index: 1,
                lang_code: "eng".into(),
                codec: "eac3".into(),
                channels: 6,
                ..Default::default()
            },
        ],
    ));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "eac3",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    assert!(
        !plan.remux,
        "720p must re-encode, not copy: {}",
        plan.url
    );
    assert!(
        plan.url.contains("start.mkv"),
        "re-encode installs start.mkv: {}",
        plan.url
    );
    let put = requests
        .iter()
        .find(|line| line.contains("PUT /library/parts/"))
        .unwrap_or_else(|| panic!("play-path PUT was never asked: {requests:?}"));
    assert_eq!(
        query_param(put, "audioStreamID"),
        Some("10975"),
        "PUT must keep the French default, not English: {put}"
    );
    assert_eq!(
        query_param(&plan.url, "audioStreamID"),
        Some("10975"),
        "start.mkv must name the French default, not English: {}",
        plan.url
    );
    assert_eq!(plan.audio_sid, 10975);
    restore_quality(Quality::Original);
    crate::plex::reset_servers_for_test();
}

/// 720p with no language preference keeps the file's direct-playable default (Russian AC3)
/// rather than switching to an unselected English DTS: the transcode speaks the language direct
/// play would have. Only a real pick or a Plex preference moves it (#202).
#[test]
#[cfg(feature = "devtriggers")]
fn a_720p_reencode_keeps_the_default_ac3_over_an_unselected_english_dts() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::P720);
    let (port, rx, server) = plan_pms(3, EMPTY_MC);
    let sid = crate::plex::register_for_test(
        "reencode-pref-lang-dts",
        "127.0.0.1",
        port,
        "token",
        "reencode-pref-lang-dts-client",
    );
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.quality = Quality::P720;
    env.src_kbps = 48_000;
    env.audio_sid = 0;
    env.cached_item = Some(fourk_item(
        sid,
        vec![
            crate::metadata::Stream {
                id: 2663,
                index: 0,
                lang_code: "rus".into(),
                codec: "ac3".into(),
                channels: 6,
                default: true,
                selected: true,
                ..Default::default()
            },
            crate::metadata::Stream {
                id: 2669,
                index: 1,
                lang_code: "eng".into(),
                codec: "dca".into(),
                channels: 6,
                ..Default::default()
            },
        ],
    ));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "dca",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    assert!(
        !plan.remux,
        "720p must re-encode, not copy: {}",
        plan.url
    );
    assert!(
        plan.url.contains("start.mkv"),
        "re-encode installs start.mkv: {}",
        plan.url
    );
    let put = requests
        .iter()
        .find(|line| line.contains("PUT /library/parts/"))
        .unwrap_or_else(|| panic!("play-path PUT was never asked: {requests:?}"));
    assert_eq!(
        query_param(put, "audioStreamID"),
        Some("2663"),
        "PUT must name the Russian AC3 default, not the unselected English DTS: {put}"
    );
    assert_eq!(
        query_param(&plan.url, "audioStreamID"),
        Some("2663"),
        "start.mkv must name the default AC3, not the DTS: {}",
        plan.url
    );
    assert_eq!(plan.audio_sid, 2663);
    restore_quality(Quality::Original);
    crate::plex::reset_servers_for_test();
}

/// Relay Auto cannot Original, so bootstrap installs HLS. The play-path PUT and start.m3u8
/// must still name selected English DTS, not the Russian AC3 sibling a remux would copy.
#[test]
#[cfg(feature = "devtriggers")]
fn a_auto_hls_reencode_puts_the_selected_dts_not_the_ac3_sibling() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::Auto);
    let (port, rx, server) = plan_pms(3, EMPTY_MC);
    let sid = crate::plex::register_for_test(
        "reencode-auto-hls-dts",
        "127.0.0.1",
        port,
        "token",
        "reencode-auto-hls-dts-client",
    );
    crate::plex::client_for(sid)
        .expect("registered")
        .set_link(crate::plex::probe::Location::Relay);
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.quality = Quality::Auto;
    env.src_kbps = 48_000;
    env.audio_sid = 0;
    env.cached_item = Some(fourk_item(
        sid,
        vec![
            crate::metadata::Stream {
                id: 2663,
                index: 0,
                lang_code: "rus".into(),
                codec: "ac3".into(),
                channels: 6,
                default: true,
                ..Default::default()
            },
            crate::metadata::Stream {
                id: 2669,
                index: 1,
                lang_code: "eng".into(),
                codec: "dca".into(),
                channels: 6,
                selected: true,
                ..Default::default()
            },
        ],
    ));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "dca",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    assert!(
        !plan.remux,
        "Relay Auto must HLS-encode, not copy: {}",
        plan.url
    );
    assert!(
        plan.url.contains("start.m3u8"),
        "Relay Auto installs start.m3u8: {}",
        plan.url
    );
    let put = requests
        .iter()
        .find(|line| line.contains("PUT /library/parts/"))
        .unwrap_or_else(|| panic!("play-path PUT was never asked: {requests:?}"));
    assert_eq!(
        query_param(put, "audioStreamID"),
        Some("2669"),
        "PUT must keep selected English DTS, not the Russian AC3 sibling: {put}"
    );
    assert_eq!(
        query_param(&plan.url, "audioStreamID"),
        Some("2669"),
        "start.m3u8 must name that DTS, not the AC3 sibling: {}",
        plan.url
    );
    assert_eq!(plan.audio_sid, 2669);
    restore_quality(Quality::Original);
    crate::plex::reset_servers_for_test();
}

/// A remux probe that registers `/decision` and then gets no `start.mkv` body must
/// physical-stop (`closeResourceSession=0`) so the HLS `/decision` on the same identity
/// is not 503'd. Closing the Streaming Resource would.
#[test]
#[cfg(feature = "devtriggers")]
fn remote_auto_failed_remux_sample_physical_stops_before_hls() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    if !crate::net::global_init() || !crate::curlio::available() {
        return;
    }
    restore_quality(Quality::Auto);
    let (port, rx, server) = plan_pms_with_start_mkv(8, MDE_TRANSCODE_COPY, 0);
    let sid = crate::plex::register_for_test(
        "mde-remote-truehd-stop",
        "127.0.0.1",
        port,
        "token",
        "mde-remote-truehd-stop-client",
    );
    crate::plex::client_for(sid)
        .expect("registered")
        .set_link(crate::plex::probe::Location::Remote);
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    let mut item = fourk_item(
        sid,
        vec![crate::metadata::Stream {
            id: 36014,
            index: 1,
            lang_code: "eng".into(),
            codec: "truehd".into(),
            channels: 8,
            default: true,
            selected: true,
            ..Default::default()
        }],
    );
    item.bitrate = 320;
    env.cached_item = Some(item);
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "truehd",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    assert!(
        requests.iter().any(|line| line.contains("start.mkv")),
        "the remux probe still ran: {requests:?}"
    );
    let stop = requests
        .iter()
        .find(|line| line.contains("/transcode/universal/stop"))
        .unwrap_or_else(|| panic!("failed remux sample must /stop: {requests:?}"));
    assert_eq!(
        query_param(stop, "closeResourceSession"),
        Some("0"),
        "physical-stop keeps the Streaming Resource for the HLS that follows: {stop}"
    );
    assert!(
        !requests.iter().any(|line| {
            line.contains("/transcode/universal/stop")
                && query_param(line, "closeResourceSession") == Some("1")
        }),
        "closeResourceSession=1 would 503 the next start: {requests:?}"
    );
    assert!(
        !plan.remux,
        "no remux sample → Auto falls through to HLS: {}",
        plan.url
    );
    restore_quality(Quality::Original);
    crate::plex::reset_servers_for_test();
}

/// An empty / unusable MDE body must not fall back to Original — that Part GET 503s on 1.43.
/// Remux/re-encode via a separate registering decision is still allowed.
#[test]
#[cfg(feature = "devtriggers")]
fn unreachable_mde_does_not_return_the_part_url() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, rx, server) = plan_pms(4, EMPTY_MC);
    let sid = crate::plex::register_for_test(
        "mde-empty",
        "127.0.0.1",
        port,
        "token",
        "mde-empty-client",
    );
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.cached_item = Some(fourk_item(sid, vec![eac3_track()]));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "eac3",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    assert!(
        requests
            .iter()
            .any(|line| line.contains("/decision?") && line.contains("hasMDE=1")),
        "MDE was still asked: {requests:?}"
    );
    assert!(
        plan.url.contains("start.mkv"),
        "no usable MDE → remux/re-encode, not Original: {}",
        plan.url
    );
    assert!(
        !plan.url.contains("/library/parts/36013/"),
        "unreachable MDE must not serve the Part: {}",
        plan.url
    );
    assert!(
        plan.remux,
        "HEVC+EAC3 with unreachable MDE still codec-copy remuxes: {}",
        plan.url
    );
    crate::plex::reset_servers_for_test();
}

/// Selected embedded SRT is client-rendered on Original. MDE must name that stream id
/// *and* `subtitles=none`: 1.43.4 400s hasMDE+directPlay with a selected subtitle and
/// the default `auto`. The mock PMS rejects that shape, so omitting the mode fail-closes
/// into remux + PUT sub=0 (the selected subtitle disappears).
#[test]
#[cfg(feature = "devtriggers")]
fn selected_embedded_srt_names_id_and_client_rendered_mode_on_mde() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, rx, server) = plan_pms(2, MDE_DIRECTPLAY);
    let sid = crate::plex::register_for_test(
        "mde-srt",
        "127.0.0.1",
        port,
        "token",
        "mde-srt-client",
    );
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.cached_item = Some(fourk_item_with_subs(
        sid,
        vec![eac3_track()],
        vec![selected_sub(55001, "srt")],
    ));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "eac3",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    let decision = requests
        .iter()
        .find(|line| line.contains("/decision?"))
        .unwrap_or_else(|| panic!("MDE was never asked: {requests:?}"));
    assert_eq!(
        query_param(decision, "subtitleStreamID"),
        Some("55001"),
        "MDE must evaluate the SRT track Original will render: {decision}"
    );
    assert_eq!(
        query_param(decision, "subtitles"),
        Some("none"),
        "client-rendered mode; auto is the 1.43.4 400: {decision}"
    );
    assert!(
        plan.url.contains("/library/parts/36013/"),
        "selected SRT must stay Original, not remux: {}",
        plan.url
    );
    crate::plex::reset_servers_for_test();
}

/// Selected PGS is client-rendered on Original; MDE must see that stream id (and the profile
/// must list pgs) so the decision stays directplay.
#[test]
#[cfg(feature = "devtriggers")]
fn selected_pgs_names_subtitle_stream_id_on_mde() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, rx, server) = plan_pms(2, MDE_DIRECTPLAY);
    let sid =
        crate::plex::register_for_test("mde-pgs", "127.0.0.1", port, "token", "mde-pgs-client");
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.cached_item = Some(fourk_item_with_subs(
        sid,
        vec![eac3_track()],
        vec![selected_sub(99001, "pgs")],
    ));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "eac3",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    let decision = requests
        .iter()
        .find(|line| line.contains("/decision?"))
        .unwrap_or_else(|| panic!("MDE was never asked: {requests:?}"));
    assert_eq!(
        query_param(decision, "subtitleStreamID"),
        Some("99001"),
        "MDE must evaluate the PGS track Original will render: {decision}"
    );
    assert_eq!(
        query_param(decision, "subtitles"),
        Some("none"),
        "named bitmap still client-rendered, not auto/burn: {decision}"
    );
    assert!(
        plan.url.contains("/library/parts/36013/"),
        "PGS + EAC3 stays Original when MDE allows: {}",
        plan.url
    );
    crate::plex::reset_servers_for_test();
}

/// A selected external sidecar is not in the container. MDE must see subtitleStreamID=0
/// so it does not force a burn; renderable text sidecars are restored by the client at landing.
#[test]
#[cfg(feature = "devtriggers")]
fn external_selected_sub_sends_subtitle_stream_id_zero_on_mde() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, rx, server) = plan_pms(2, MDE_DIRECTPLAY);
    let sid = crate::plex::register_for_test(
        "mde-ext-sub",
        "127.0.0.1",
        port,
        "token",
        "mde-ext-client",
    );
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.cached_item = Some(fourk_item_with_subs(
        sid,
        vec![eac3_track()],
        vec![{
            let mut s = selected_sub(88001, "srt");
            s.external = true;
            s
        }],
    ));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "eac3",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    let decision = requests
        .iter()
        .find(|line| line.contains("/decision?"))
        .unwrap_or_else(|| panic!("MDE was never asked: {requests:?}"));
    assert_eq!(
        query_param(decision, "subtitleStreamID"),
        Some("0"),
        "sidecar is not burned on Original: {decision}"
    );
    assert_ne!(
        query_param(decision, "subtitleStreamID"),
        Some("88001"),
        "must not advertise the external id: {decision}"
    );
    assert!(
        plan.url.contains("/library/parts/36013/"),
        "MDE with subs off stays Original: {}",
        plan.url
    );
    crate::plex::reset_servers_for_test();
}

/// Selected embedded `mov_text` (iTunes MP4) is client-rendered; the profile lists it so
/// MDE must see the stream id and stay Original rather than re-encoding.
#[test]
#[cfg(feature = "devtriggers")]
fn selected_mov_text_names_subtitle_stream_id_on_mde() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, rx, server) = plan_pms(2, MDE_DIRECTPLAY);
    let sid = crate::plex::register_for_test(
        "mde-mov-text",
        "127.0.0.1",
        port,
        "token",
        "mde-mov-client",
    );
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.cached_item = Some(fourk_item_with_subs(
        sid,
        vec![eac3_track()],
        vec![selected_sub(77001, "mov_text")],
    ));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "eac3",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    let decision = requests
        .iter()
        .find(|line| line.contains("/decision?"))
        .unwrap_or_else(|| panic!("MDE was never asked: {requests:?}"));
    assert_eq!(
        query_param(decision, "subtitleStreamID"),
        Some("77001"),
        "MDE must evaluate the mov_text track Original will render: {decision}"
    );
    assert!(
        plan.url.contains("/library/parts/36013/"),
        "mov_text + EAC3 stays Original when MDE allows: {}",
        plan.url
    );
    crate::plex::reset_servers_for_test();
}

/// PMS reports DVD bitmaps as `dvd_subtitle`; listing only `dvd` would MDE-transcode and
/// then forbid remux. The stream id must land on `/decision`.
#[test]
#[cfg(feature = "devtriggers")]
fn selected_dvd_subtitle_names_subtitle_stream_id_on_mde() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, rx, server) = plan_pms(2, MDE_DIRECTPLAY);
    let sid = crate::plex::register_for_test(
        "mde-dvd-sub",
        "127.0.0.1",
        port,
        "token",
        "mde-dvd-client",
    );
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.cached_item = Some(fourk_item_with_subs(
        sid,
        vec![eac3_track()],
        vec![selected_sub(66001, "dvd_subtitle")],
    ));
    let plan = build_stream(
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "eac3",
        &env,
    );
    let requests = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PMS never saw the resolve");
    server.join().unwrap();

    let decision = requests
        .iter()
        .find(|line| line.contains("/decision?"))
        .unwrap_or_else(|| panic!("MDE was never asked: {requests:?}"));
    assert_eq!(
        query_param(decision, "subtitleStreamID"),
        Some("66001"),
        "MDE must evaluate the dvd_subtitle track: {decision}"
    );
    assert!(
        plan.url.contains("/library/parts/36013/"),
        "dvd_subtitle + EAC3 stays Original when MDE allows: {}",
        plan.url
    );
    crate::plex::reset_servers_for_test();
}
