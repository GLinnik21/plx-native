//! Issue #266 PR 2: the Plex Pass audio enhancement's OFFERING and RESOLVE — who gets it
//! (`enhancements_offered`), what `build_stream` sends for it, and what it does when the server
//! says no. Every playback here runs against a loopback PMS (`enhancement_pms`) and grades the
//! wire it saw, because the invariants are wire facts: the params ride the transcode leg only
//! (I3), the payload codec comes from the decision (I4), and without Plex Pass nothing changes.
//!
//! "Enhanced" below means: a remux whose `start.mkv` carries `normalizeLoudness=1`, a Load payload
//! audio codec of `ac3`, and `cur_enhancement == Applied` once installed.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::apply_plan;
use plx_plex::plex::serverinfo::Subscription;

const PREF: plx_plex::plex::AudioEnhancements = plx_plex::plex::AudioEnhancements {
    boost_dialog: false,
    normalize_loudness: true,
};

fn track(id: i64, codec: &str, channels: i64, capable: bool) -> plx_data::metadata::Stream {
    plx_data::metadata::Stream {
        id,
        index: id,
        lang_code: "eng".into(),
        codec: codec.into(),
        channels,
        default: true,
        selected: true,
        can_normalize_loudness: capable,
        ..Default::default()
    }
}

/// One resolve against a fresh loopback PMS. `setup` edits the env (it starts as a Plex Pass,
/// opted-in, Local, Original playback of `item`) and may re-link the client.
struct Resolve {
    plan: Plan,
    requests: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn resolve(
    ps: &mut PlaybackSession,
    mde: &'static [u8],
    mode: EnhMode,
    media_bytes: usize,
    part: &str,
    acodec: &str,
    item: impl FnOnce(ServerId) -> plx_data::metadata::PlayingItem,
    setup: impl FnOnce(&mut ResolveEnv, &plx_plex::plex::Client),
) -> Resolve {
    assert!(plx_net::net::global_init() && crate::curlio::available());
    let (port, done, server) = enhancement_pms(mde, mode, media_bytes);
    let sid = plx_plex::plex::register_for_test("enh-pms", "127.0.0.1", port, "token", "enh-client");
    let client = plx_plex::plex::client_for(sid).unwrap();
    client.set_link(plx_plex::plex::probe::Location::Local);
    let mut env = ResolveEnv::snapshot(ps, plx_data::stores::metadata::MetadataStore::default().view(), sid, "rk-enh");
    env.quality = Quality::Original;
    env.pass = Subscription::Yes;
    env.audio_enhancements = PREF;
    env.cached_item = Some(item(sid));
    setup(&mut env, client);
    let plan = build_stream(&plx_base::task::OffFrame::for_test(), "rk-enh", part, "hevc", acodec, &env);
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    Resolve { plan, requests }
}

const MKV: &str = "/library/parts/960001/1/file.mkv";

fn ac3_item(sid: ServerId) -> plx_data::metadata::PlayingItem {
    fourk_item(sid, vec![track(1, "ac3", 2, true)])
}

fn any_param(requests: &[String]) -> bool {
    requests.iter().any(|r| r.contains("boostDialog") || r.contains("normalizeLoudness"))
}

fn mde_lines(requests: &[String]) -> Vec<&String> {
    requests.iter().filter(|r| r.contains("/decision?") && r.contains("hasMDE=1")).collect()
}

fn transcode_decisions(requests: &[String]) -> Vec<&String> {
    requests.iter().filter(|r| r.contains("/decision?") && !r.contains("hasMDE=1")).collect()
}

/// The plain direct play, with nothing enhancement-shaped anywhere on the wire.
#[track_caller]
fn assert_direct_no_params(r: &Resolve) {
    assert!(r.plan.url.contains("/library/parts/"), "direct play expected: {}", r.plan.url);
    assert!(!r.plan.url.contains("start.mkv"), "{}", r.plan.url);
    assert!(r.plan.tsession.is_empty());
    assert!(!any_param(&r.requests), "no request may carry a param: {:?}", r.requests);
    assert_eq!(r.plan.contract.audio, plx_plex::plex::AudioEnhancements::NONE);
    assert_eq!(r.plan.enhancement, EnhancementOutcome::Off);
}

#[track_caller]
fn assert_enhanced(r: &Resolve) {
    assert!(r.plan.contract.remux, "an enhanced route is a remux");
    assert_eq!(r.plan.contract.audio, PREF);
    assert!(r.plan.url.contains("start.mkv"), "{}", r.plan.url);
    assert_eq!(query_param(&r.plan.url, "normalizeLoudness"), Some("1"), "{}", r.plan.url);
    assert_eq!(query_param(&r.plan.url, "boostDialog"), None, "only the chosen param rides");
    assert_eq!(r.plan.acodec, "ac3");
    for mde in mde_lines(&r.requests) {
        assert!(!mde.contains("boostDialog") && !mde.contains("normalizeLoudness"), "I3: {mde}");
    }
}

#[test]
#[cfg(feature = "devtriggers")]
fn pass_capable_ac3_local_original_is_enhanced_remux() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3", ac3_item, |env, _| {
        // Production's capture, not the test default: the snapshot reads serverinfo for `sid`.
        plx_plex::plex::serverinfo::store_for_test(env.sid, Subscription::Yes, "1.43.4");
        env.pass = Subscription::Unknown;
        env.pass = ResolveEnv::snapshot(&PlaybackSession::IDLE, plx_data::stores::metadata::MetadataStore::default().view(), env.sid, "rk-enh").pass;
    });
    assert_enhanced(&r);
    let mde = mde_lines(&r.requests);
    let enhanced = transcode_decisions(&r.requests);
    assert_eq!(mde.len(), 1, "{:?}", r.requests);
    assert_eq!(enhanced.len(), 1, "{:?}", r.requests);
    assert_eq!(
        query_param(mde[0], "X-Plex-Session-Identifier"),
        query_param(enhanced[0], "X-Plex-Session-Identifier"),
        "the enhanced decision reuses the MDE's session (M5)",
    );
    assert_eq!(query_param(enhanced[0], "normalizeLoudness"), Some("1"));
    assert_eq!(r.plan.enhancement, EnhancementOutcome::Applied);
    apply_plan(&mut ps, r.plan, "rk-enh");
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Applied);
    assert_eq!(ps.cur_contract.audio, PREF);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn no_pass_is_direct_no_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3", ac3_item, |env, _| {
        env.pass = Subscription::No;
    });
    assert_direct_no_params(&r);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn unknown_subscription_is_direct_no_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3", ac3_item, |env, _| {
        env.pass = Subscription::Unknown;
    });
    assert_direct_no_params(&r);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn incapable_track_is_direct_no_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        |sid| fourk_item(sid, vec![track(1, "ac3", 2, false)]), |_, _| {});
    assert_direct_no_params(&r);
    plx_plex::plex::reset_servers_for_test();
}

/// No fetched track list: the plan names the server default (no id), whose facts are unknown —
/// and unknown fails closed.
#[test]
#[cfg(feature = "devtriggers")]
fn server_default_audio_none_is_direct_no_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        |sid| fourk_item(sid, Vec::new()), |_, _| {});
    assert_direct_no_params(&r);
    assert_eq!(r.plan.audio, None);
    assert!(r.plan.auto_original.as_ref().is_some_and(|c| c.audio.is_none()));
    plx_plex::plex::reset_servers_for_test();
}

/// No fetched track list, but a transcode still NAMES an id (a retry's / the session's earlier
/// pick, sent as `audioStreamID`): the plan keeps that id as [`CarriedAudio::named`] — so the
/// session's timeline goes on reporting it, as before #266 — while every fact stays unknown. (A
/// direct play with no list names no id at all, as before.)
#[test]
#[cfg(feature = "devtriggers")]
fn named_unfetched_audio_keeps_its_id_and_fails_closed() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_TRANSCODE_VIDEO, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        |sid| fourk_item(sid, Vec::new()), |env, _| env.audio_sid = 42);
    assert!(r.plan.url.contains("/transcode/"), "a video transcode expected: {}", r.plan.url);
    assert!(!any_param(&r.requests), "nothing is offered for an unread track: {:?}", r.requests);
    let audio = r.plan.audio.clone().expect("the named id survives");
    assert_eq!(audio.sid, 42);
    assert!(audio.codec.is_empty() && !audio.can_normalize_loudness && !audio.immersive);
    apply_plan(&mut ps, r.plan, "rk-enh");
    assert_eq!(cur_audio_sid(&ps), 42);
    plx_plex::plex::reset_servers_for_test();
}

/// Owner decision: a declared Dolby Vision source whose base layer IS self-displayable (P7/P8)
/// gets the enhancement too — the remux plays the HDR10 base layer and simply never re-declares
/// Dolby Vision (a remux was never able to carry `DolbyHdrInfo` at all — see `fill_direct_plan`'s
/// own doc — so this drops nothing a remux could have kept).
#[test]
#[cfg(feature = "devtriggers")]
fn dv_declared_p8_usable_base_is_enhanced_remux_without_dv() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        |sid| {
            let mut item = ac3_item(sid);
            item.dovi = p8();
            item
        },
        |env, _| env.dv_capability = Some(plx_platform::devcaps::dv::DvCapability::Supported));
    assert!(
        r.plan.auto_original.as_ref().is_some_and(|c| c.dv_decision.presentation.declared().is_some()),
        "precondition: the P8 direct play would have declared DolbyHdrInfo",
    );
    assert_enhanced(&r);
    assert!(
        r.plan.dv_decision.presentation.declared().is_none(),
        "the remux never carries DolbyHdrInfo — Dolby Vision turns off while the enhancement is on",
    );
    plx_plex::plex::reset_servers_for_test();
}

/// I7's other half, at the predicate: a base whose DV base layer is not self-displayable is a
/// plain reason (Disabled, never Hidden — Plex Pass is Yes here), whatever else holds (end to end
/// such a file is refused Original before a candidate exists, so the predicate is where the rule
/// can be seen on its own).
#[test]
fn dv_unusable_base_is_disabled_not_offered() {
    let carried = CarriedAudio::from_stream(&track(1, "ac3", 2, true), 0);
    let mut base = test_original_candidate(None);
    base.audio = Some(carried.clone());
    let facts = |b| EnhancementFacts { pass: Subscription::Yes, base: Some(b), carried: Some(&carried), subtitle_effect: SubtitleEffect::None, refused: false };
    assert!(enhancements_offered(&facts(&base), RouteFamily::Direct), "control: the clean base is offered");
    let mut unusable = base.clone();
    unusable.dovi = p5();
    assert!(unusable.dovi.base_layer_unusable());
    assert!(!enhancements_offered(&facts(&unusable), RouteFamily::Direct));
    assert_eq!(
        enhancement_availability(&facts(&unusable), RouteFamily::Direct),
        EnhancementAvailability::Disabled(DisabledReason::DolbyVisionUnusable),
    );
    assert_eq!(desired_audio(PREF, enhancements_offered(&facts(&unusable), RouteFamily::Direct)), plx_plex::plex::AudioEnhancements::NONE);
}

/// M7 / owner decision: an embedded subtitle keeps playing by BURNING it in — the audio
/// enhancement forces a real re-encode instead of the ordinary uncapped remux.
#[test]
#[cfg(feature = "devtriggers")]
fn embedded_default_subtitle_with_enhancement_is_burned() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        |sid| fourk_item_with_subs(sid, vec![track(1, "ac3", 2, true)], vec![selected_sub(9, "srt")]),
        |_, _| {});
    // `sub_render_ordinal` is only ever filled by `fill_direct_plan` — a burn never calls it, by
    // construction (it is not a direct play) — so the subtitle's identity is proven below instead,
    // on the wire: `subtitleStreamID=9&subtitles=burn` is the only evidence a burn CAN carry.
    assert!(r.plan.url.contains("start.mkv"), "a burn is never a direct play: {}", r.plan.url);
    assert!(!r.plan.contract.remux, "a burn is a real re-encode, not a codec-preserving copy");
    assert_eq!(r.plan.contract.audio, PREF);
    assert_eq!(query_param(&r.plan.url, "subtitleStreamID"), Some("9"), "{}", r.plan.url);
    assert_eq!(query_param(&r.plan.url, "subtitles"), Some("burn"), "{}", r.plan.url);
    assert_eq!(r.plan.enhancement, EnhancementOutcome::Applied);
    plx_plex::plex::reset_servers_for_test();
}

/// The cold-start twin of item 3's fix: the LIVE reconcile path (`install_retranscode_outcome`)
/// prints `enhancement: applied boost=.. loudness=..` to the event log the harness greps, and so
/// does the COLD START branch above — `plan.enhancement = classify_outcome(..)` in `route::plan`,
/// reached with no live pick at all when the item already carries a server-selected embedded
/// subtitle — so a case that boots straight into a Burn has a line to key on. Grade the same
/// scenario as `embedded_default_subtitle_with_enhancement_is_burned` above, but on the event log
/// rather than `r.plan`, the way `tests/run.py::op_audio_enhancement_burn` actually reads it.
#[test]
#[cfg(feature = "devtriggers")]
fn embedded_default_subtitle_with_enhancement_logs_applied_on_cold_start() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let log_path = plx_base::paths::in_runtime_dir(plx_base::paths::runtime_file::EVENTS);
    let before = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        |sid| fourk_item_with_subs(sid, vec![track(1, "ac3", 2, true)], vec![selected_sub(9, "srt")]),
        |_, _| {});
    assert_eq!(r.plan.enhancement, EnhancementOutcome::Applied, "precondition: cold start is a Burn");
    let written = std::fs::read(&log_path).unwrap();
    let appended = String::from_utf8_lossy(&written[before as usize..]);
    assert!(
        appended.contains("enhancement: applied boost=0 loudness=1"),
        "no `enhancement: applied` line from the cold-start branch :: {appended:?}"
    );
    plx_plex::plex::reset_servers_for_test();
}

/// A subtitle the client renders itself (an external sidecar) is UNAFFECTED by the enhancement:
/// the route still becomes the ordinary enhanced remux, and the sidecar restore is independent of
/// it (`player::sidecar`, not the transcoded stream).
#[test]
#[cfg(feature = "devtriggers")]
fn server_selected_external_srt_is_enhanced_remux_sidecar_unaffected() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let external = plx_data::metadata::Stream {
        id: 77,
        codec: "srt".into(),
        key: "/library/streams/77".into(),
        external: true,
        selected: true,
        ..Default::default()
    };
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        move |sid| fourk_item_with_subs(sid, vec![track(1, "ac3", 2, true)], vec![external]),
        |_, _| {});
    assert_enhanced(&r);
    // PMS discards a DOWNLOADED subtitle the moment the part's selection moves off it, so the app
    // that draws the sidecar itself must leave the selection on it: a PUT of 0 here is the bug that
    // made a freshly searched subtitle 404 at the next playback start (docs/pms-api.md, 2026-10-06).
    assert_eq!(selection_subtitles(&r.requests), vec!["77"], "{:?}", r.requests);
    assert_eq!(query_param(&r.plan.url, "subtitles"), Some("none"), "the wire says the app draws it: {}", r.plan.url);
    assert_eq!(query_param(&r.plan.url, "subtitleStreamID"), Some("0"), "{}", r.plan.url);
    assert_eq!(r.plan.sub_sid, 77, "the sidecar stays the session's subtitle");
    plx_plex::plex::reset_servers_for_test();
}

/// The `subtitleStreamID` of every selection PUT, in order.
fn selection_subtitles(requests: &[String]) -> Vec<&str> {
    requests
        .iter()
        .filter(|l| l.starts_with("PUT /library/parts/"))
        .filter_map(|l| query_param(l, "subtitleStreamID"))
        .collect()
}

/// The viewer's own Off is the one case that still sends 0 (and nothing is drawn).
#[test]
#[cfg(feature = "devtriggers")]
fn viewer_off_over_an_enhanced_remux_still_puts_zero() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let external = plx_data::metadata::Stream {
        id: 77,
        codec: "srt".into(),
        key: "/library/streams/77".into(),
        external: true,
        selected: true,
        ..Default::default()
    };
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        move |sid| fourk_item_with_subs(sid, vec![track(1, "ac3", 2, true)], vec![external]),
        |env, _| env.subtitle_override = Some(0));
    assert_enhanced(&r);
    assert_eq!(selection_subtitles(&r.requests), vec!["0"], "{:?}", r.requests);
}

/// A film with one embedded English subtitle (id 5, unselected) and the subtitle the viewer
/// downloaded mid-playback (id 77, an external SRT the server has SELECTED). The language pick
/// would choose the embedded track, so any selection PUT naming it is the app overriding the server.
fn embedded_english_and_selected_download(sid: ServerId) -> plx_data::metadata::PlayingItem {
    let embedded = plx_data::metadata::Stream { selected: false, ..selected_sub(5, "srt") };
    let download = plx_data::metadata::Stream {
        id: 77,
        index: 0,
        lang_code: "eng".into(),
        codec: "srt".into(),
        key: "/library/streams/77".into(),
        external: true,
        selected: true,
        ..Default::default()
    };
    fourk_item_with_subs(sid, vec![track(1, "ac3", 2, true)], vec![embedded, download])
}

/// **Regression (2026-10-06, second sequence):** a start whose server selection is a downloaded
/// sidecar must keep it on the remux — never PUT the embedded track the language pick would take.
#[test]
#[cfg(feature = "devtriggers")]
fn remux_start_keeps_the_servers_selected_download_over_an_embedded_language_match() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        embedded_english_and_selected_download, |_, _| {});
    assert_enhanced(&r);
    assert_eq!(selection_subtitles(&r.requests), vec!["77"], "{:?}", r.requests);
    assert_eq!(r.plan.sub_sid, 77);
    assert_eq!(r.plan.sub_render_ordinal, None, "a sidecar has no side-reader target");
    assert_eq!(query_param(&r.plan.url, "subtitles"), Some("none"), "{}", r.plan.url);
    plx_plex::plex::reset_servers_for_test();
}

/// Direct play never PUTs a subtitle selection at all, and the landing restores the sidecar.
#[test]
#[cfg(feature = "devtriggers")]
fn direct_play_start_leaves_the_servers_selected_download_alone() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        embedded_english_and_selected_download, |env, _| env.audio_enhancements = plx_plex::plex::AudioEnhancements::NONE);
    assert!(r.plan.url.contains("/library/parts/"), "direct play expected: {}", r.plan.url);
    assert!(selection_subtitles(&r.requests).is_empty(), "{:?}", r.requests);
    assert_eq!(r.plan.sub_render_ordinal, None, "the embedded track is not drawn");
    plx_plex::plex::reset_servers_for_test();
}

/// The viewer's own pick for this play (the detail page's track row) still wins over the server.
#[test]
#[cfg(feature = "devtriggers")]
fn a_viewer_pick_made_before_play_still_wins_over_the_servers_selection() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        embedded_english_and_selected_download, |env, _| env.subtitle_override = Some(5));
    // (over this 48 Mbit/s Part the embedded track is burned, M7; only the selection is graded)
    assert_eq!(selection_subtitles(&r.requests), vec!["5"], "{:?}", r.requests);
}

/// A capped re-encode cannot leave the app to draw the sidecar, and a selection moved to 0 would
/// make PMS discard the download: it stays selected and the encoder burns it.
#[test]
#[cfg(feature = "devtriggers")]
fn capped_reencode_start_keeps_the_servers_selected_download_selected() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_TRANSCODE, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        embedded_english_and_selected_download,
        |env, _| { env.quality = Quality::P720; env.audio_enhancements = plx_plex::plex::AudioEnhancements::NONE; });
    assert!(!r.plan.contract.remux, "a re-encode: {}", r.plan.url);
    assert_eq!(selection_subtitles(&r.requests), vec!["77"], "{:?}", r.requests);
    assert_eq!(query_param(&r.plan.url, "subtitleStreamID"), Some("77"), "{}", r.plan.url);
    assert_eq!(r.plan.sub_sid, 77);
    plx_plex::plex::reset_servers_for_test();
}

/// A film whose own audio the TV cannot decode (TrueHD), with a second, decodable track beside
/// it (what the stand-in rule used to reach for) and an embedded subtitle that is selected.
fn truehd_with_ac3_sibling_and_embedded_sub(sid: ServerId, kbps: i64) -> plx_data::metadata::PlayingItem {
    let truehd = track(1, "truehd", 8, true);
    let mut sibling = track(2, "ac3", 2, true);
    sibling.default = false;
    sibling.selected = false;
    sibling.lang_code = "fra".into();
    let mut item = fourk_item_with_subs(sid, vec![truehd, sibling], vec![selected_sub(9, "srt")]);
    item.bitrate = kbps;
    item
}

/// **Cold start, a subtitle the app draws beside the remux.** On a local link with a known, moderate
/// Part bitrate the viewer keeps the audio track they asked for (no direct-playing stand-in chosen
/// for the subtitle's sake), the route is the remux, nothing is burned, and the wire tells the
/// server so: `subtitleStreamID=0&subtitles=none`. The enhancement is offered on that remux.
#[test]
#[cfg(feature = "devtriggers")]
fn cold_start_keeps_intended_audio_and_draws_the_subtitle() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "truehd",
        |sid| truehd_with_ac3_sibling_and_embedded_sub(sid, 7_400),
        |_, _| {});
    assert!(r.plan.url.contains("start.mkv"), "the intended TrueHD is converted by the server: {}", r.plan.url);
    assert!(r.plan.contract.remux, "a copy of the video, not a burn");
    assert_eq!(r.plan.audio.as_ref().map(|a| a.sid), Some(1), "the track asked for, not the stand-in");
    assert_eq!(r.plan.sub_sid, 9, "the subtitle stays the selection");
    assert_eq!(r.plan.sub_render_ordinal, Some(0), "and the app is told which track to draw");
    assert_eq!(query_param(&r.plan.url, "subtitles"), Some("none"), "{}", r.plan.url);
    assert_eq!(query_param(&r.plan.url, "subtitleStreamID"), Some("0"), "{}", r.plan.url);
    assert_eq!(query_param(&r.plan.url, "normalizeLoudness"), Some("1"), "the enhancement is offered: {}", r.plan.url);
    for d in transcode_decisions(&r.requests) {
        assert!(!d.contains("subtitles=burn"), "{d}");
    }
    assert_eq!(selection_subtitles(&r.requests), vec!["9"], "an admitted embedded track stays the selection: {:?}", r.requests);
    plx_plex::plex::reset_servers_for_test();
}

/// The same film over the bound (or unmeasured) is today's behaviour: the stand-in direct-plays and
/// the app draws the subtitle on it.
/// The cold re-resolve re-asks the admission the live route asked: an embedded track the app was
/// drawing (`sub_drawn >= 0`) stays drawn only where THIS plan's link and Part still admit the
/// reader, so the wire and the landed presenter cannot disagree. A sidecar needs none of it.
#[test]
#[cfg(feature = "devtriggers")]
fn cold_reresolve_of_a_drawn_embedded_track_burns_where_the_reader_is_not_admitted() {
    for (drawn, kbps, link, burns) in [
        (Some(0), 40_000, plx_plex::plex::probe::Location::Local, true),
        (Some(0), 0, plx_plex::plex::probe::Location::Local, true),
        (Some(0), 7_400, plx_plex::plex::probe::Location::Remote, true),
        (Some(-1), 40_000, plx_plex::plex::probe::Location::Local, false),
    ] {
        let mut ps = PlaybackSession::IDLE;
        let _g = fresh_registry(&mut ps);
        let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "truehd",
            |sid| {
                let mut item = fourk_item(sid, vec![track(1, "truehd", 8, true)]);
                item.bitrate = kbps;
                item
            },
            |env, client| {
                client.set_link(link);
                env.sub_sid = 77;
                env.sub_drawn = drawn;
            });
        let wire = query_param(&r.plan.url, "subtitles");
        if burns {
            assert_ne!(wire, Some("none"), "{drawn:?} {kbps} {link:?}: the server burns: {}", r.plan.url);
            assert_eq!(r.plan.sub_render_ordinal, None, "{drawn:?} {kbps} {link:?}");
        } else {
            assert_eq!(wire, Some("none"), "{drawn:?} {kbps} {link:?}: {}", r.plan.url);
        }
        plx_plex::plex::reset_servers_for_test();
    }
}

/// **Cold start, a selected subtitle the app draws but the profile does not advertise** (MicroDVD,
/// SAMI, …): `mde_subtitle_id_of` is 0 for it, yet the side reader's decoder set covers it, so the
/// plan must still carry the selection and the track to draw. Without them the route is a
/// `subtitles=none` remux with nobody drawing and the menu reading Off.
#[test]
#[cfg(feature = "devtriggers")]
fn cold_start_draws_a_subtitle_codec_the_profile_does_not_advertise() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "truehd",
        |sid| {
            let truehd = track(1, "truehd", 8, true);
            let mut item = fourk_item_with_subs(sid, vec![truehd], vec![selected_sub(9, "microdvd")]);
            item.bitrate = 7_400;
            item
        },
        |_, _| {});
    assert!(r.plan.contract.remux, "{}", r.plan.url);
    assert_eq!(r.plan.sub_sid, 9, "the selection is the plan's, not 0");
    assert_eq!(r.plan.sub_render_ordinal, Some(0), "and the app is told which track to draw");
    assert_eq!(query_param(&r.plan.url, "subtitles"), Some("none"), "{}", r.plan.url);
    plx_plex::plex::reset_servers_for_test();
}

/// The cold-start plan, LANDED: the session reads it back as an app-drawn embedded track over the
/// remux with a reader target naming the plan's ordinal — what the wire promised.
#[test]
#[cfg(feature = "devtriggers")]
fn cold_start_plan_lands_as_client_over_remux_with_a_reader_target() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "truehd",
        |sid| truehd_with_ac3_sibling_and_embedded_sub(sid, 7_400),
        |_, _| {});
    assert_eq!(r.plan.sub_render_ordinal, Some(0));
    apply_plan(&mut ps, r.plan, "rk-enh");
    // The play request (`request_play`) is what names the Part the reader reads; a plan landing
    // alone does not carry it.
    ps.request = Some(PlaybackRequest {
        sid: ps.cur_sid,
        rk: "rk-enh".into(),
        part: MKV.into(),
        vcodec: "hevc".into(),
        acodec: "truehd".into(),
        title: String::new(),
        ctx: String::new(),
        preview: false,
    });
    assert_eq!(subtitle_presenter(&ps), SubtitlePresenter::ClientOverRemux);
    assert!(!subtitles_burned(&ps));
    let target = side_reader_target(&ps).expect("the landed route starts the reader");
    assert_eq!(target.ordinal, 0);
    assert_eq!(ps.cur_sub_sid, 9);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn cold_start_over_the_bound_keeps_the_stand_in() {
    for kbps in [30_001, 48_000] {
        let mut ps = PlaybackSession::IDLE;
        let _g = fresh_registry(&mut ps);
        let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "truehd",
            |sid| truehd_with_ac3_sibling_and_embedded_sub(sid, kbps),
            |env, _| env.audio_enhancements = plx_plex::plex::AudioEnhancements::NONE);
        assert!(!r.plan.url.contains("start.mkv"), "{kbps}: the stand-in direct-plays: {}", r.plan.url);
        assert_eq!(r.plan.audio.as_ref().map(|a| a.sid), Some(2), "{kbps}");
        plx_plex::plex::reset_servers_for_test();
    }
}

/// A cold re-resolve (a retry, a restart) that carries a subtitle the app is already drawing into a
/// remux-shaped plan: no burn rides the wire, the id stays the selection.
#[test]
#[cfg(feature = "devtriggers")]
fn cold_reresolve_with_a_client_drawn_subtitle_sends_no_burn() {
    for drawn in [Some(-1), Some(0)] {
        let mut ps = PlaybackSession::IDLE;
        let _g = fresh_registry(&mut ps);
        let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "truehd",
            |sid| {
                let mut item = fourk_item(sid, vec![track(1, "truehd", 8, true)]);
                item.bitrate = 7_400; // an embedded track is only drawn beside a measured, moderate Part
                item
            },
            |env, _| {
                env.sub_sid = 77;
                env.sub_drawn = drawn;
            });
        assert!(r.plan.contract.remux, "{drawn:?}: {}", r.plan.url);
        assert_eq!(query_param(&r.plan.url, "subtitles"), Some("none"), "{drawn:?}: {}", r.plan.url);
        assert_eq!(query_param(&r.plan.url, "subtitleStreamID"), Some("0"), "{drawn:?}: {}", r.plan.url);
        for d in transcode_decisions(&r.requests) {
            assert!(!d.contains("subtitles=burn"), "{drawn:?}: {d}");
        }
        assert_eq!(r.plan.sub_sid, 77, "{drawn:?}");
        plx_plex::plex::reset_servers_for_test();
    }
}

#[test]
#[cfg(feature = "devtriggers")]
fn fixed_720p_rung_no_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3", ac3_item, |env, _| {
        env.quality = Quality::P720;
    });
    assert!(r.plan.auto_original.is_none(), "a fixed rung captures no Original candidate");
    assert!(!any_param(&r.requests), "{:?}", r.requests);
    assert_eq!(r.plan.contract.audio, plx_plex::plex::AudioEnhancements::NONE);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn remote_auto_hls_no_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    // No media bytes: the Remote probe of the Part fails, so Auto starts on HLS.
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3", ac3_item, |env, client| {
        client.set_link(plx_plex::plex::probe::Location::Remote);
        env.quality = Quality::Auto;
    });
    assert!(matches!(r.plan.contract.delivery, plx_plex::plex::TranscodeDelivery::FixedHls { .. }), "precondition: HLS");
    assert!(!any_param(&r.requests), "{:?}", r.requests);
    assert_eq!(r.plan.contract.audio, plx_plex::plex::AudioEnhancements::NONE);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn relay_no_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3", ac3_item, |_, client| {
        client.set_link(plx_plex::plex::probe::Location::Relay);
    });
    assert!(!any_param(&r.requests), "{:?}", r.requests);
    assert_eq!(r.plan.contract.audio, plx_plex::plex::AudioEnhancements::NONE);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn forced_direct_play_no_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3", ac3_item, |env, _| {
        env.direct_play_mode = DirectPlayMode::Forced;
    });
    assert_direct_no_params(&r);
    plx_plex::plex::reset_servers_for_test();
}

/// A hero preview is direct play or nothing, so marking the env as a preview must drop the
/// opt-in — otherwise the enhanced remux is what the preview gate then refuses.
#[test]
#[cfg(feature = "devtriggers")]
fn preview_never_carries_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3", ac3_item, |env, _| {
        env.set_preview(true);
    });
    assert_direct_no_params(&r);
    plx_plex::plex::reset_servers_for_test();
}

/// The carried track is the one the route NAMES on the wire (the direct-play pick), not the
/// session's earlier `audio_sid`: a capable TrueHD pick the TV cannot decode gives way to the AC3
/// sibling, and it is the sibling's capability that decides.
#[test]
#[cfg(feature = "devtriggers")]
fn carried_track_is_encode_audio_not_audio_sel() {
    for (truehd_capable, ac3_capable, want_params) in [(true, false, false), (false, true, true)] {
        let mut ps = PlaybackSession::IDLE;
        let _g = fresh_registry(&mut ps);
        let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "truehd",
            |sid| fourk_item(sid, vec![track(1, "truehd", 8, truehd_capable), track(2, "ac3", 6, ac3_capable)]),
            |env, _| env.audio_sid = 1);
        assert_eq!(r.plan.auto_original.as_ref().and_then(|c| c.audio.as_ref()).map(|a| a.sid), Some(2));
        assert_eq!(any_param(&r.requests), want_params, "truehd={truehd_capable} ac3={ac3_capable}: {:?}", r.requests);
        plx_plex::plex::reset_servers_for_test();
    }
}

/// I4: the enhanced Load payload's audio codec is the DECISION's, else `ac3` — never the source's.
#[test]
#[cfg(feature = "devtriggers")]
fn payload_codec_from_decision_else_ac3() {
    let eac3 = |sid| fourk_item(sid, vec![track(1, "eac3", 6, true)]);
    for (mode, want) in [(EnhMode::Honor("aac"), "aac"), (EnhMode::HonorNoCodecs, "ac3")] {
        let mut ps = PlaybackSession::IDLE;
        let _g = fresh_registry(&mut ps);
        let r = resolve(&mut ps, MDE_DIRECTPLAY, mode, 0, MKV, "eac3", eac3, |_, _| {});
        assert!(r.plan.contract.audio.any(), "{mode:?}");
        assert_eq!(r.plan.acodec, want, "{mode:?}");
        assert_ne!(r.plan.acodec, "eac3", "the source codec is silent audio");
        plx_plex::plex::reset_servers_for_test();
    }
}

#[track_caller]
fn assert_fell_back_to_direct(r: &Resolve) {
    assert!(r.plan.url.contains("/library/parts/") && !r.plan.url.contains("start.mkv"), "{}", r.plan.url);
    assert!(r.plan.tsession.is_empty());
    assert!(!r.plan.contract.remux);
    assert_eq!(r.plan.contract.audio, plx_plex::plex::AudioEnhancements::NONE);
    assert_eq!(r.plan.enhancement, EnhancementOutcome::Refused);
    assert_eq!(r.plan.acodec, "ac3", "the direct play's own source codec again");
    assert_eq!(mde_lines(&r.requests).len(), 2, "MDE re-issued before the Part: {:?}", r.requests);
    assert!(r.plan.verdict.is_none(), "a refused ENHANCEMENT is not a refused playback");
}

#[test]
#[cfg(feature = "devtriggers")]
fn refusal_falls_back_to_direct_refused() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Refuse, 0, MKV, "ac3", ac3_item, |_, _| {});
    assert_fell_back_to_direct(&r);
    apply_plan(&mut ps, r.plan, "rk-enh");
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Refused);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn ignored_params_audio_copy_falls_back_refused() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Ignore, 0, MKV, "ac3", ac3_item, |_, _| {});
    assert_fell_back_to_direct(&r);
    plx_plex::plex::reset_servers_for_test();
}

/// A remux-family Original (an `.avi`, so no MDE) on a Remote link: the probe samples the remux
/// that will play, so the probe's decision and the play decision carry the same params, on the
/// same session.
#[test]
#[cfg(feature = "devtriggers")]
fn remote_probe_and_play_decision_carry_same_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let bytes = remote_probe_plan(320).unwrap().target_bytes;
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), bytes, "/library/parts/960001/1/file.avi", "ac3",
        |sid| {
            let mut item = ac3_item(sid);
            item.bitrate = 320;
            item
        },
        |env, client| {
            client.set_link(plx_plex::plex::probe::Location::Remote);
            env.quality = Quality::Auto;
        });
    let decisions = transcode_decisions(&r.requests);
    assert_eq!(decisions.len(), 2, "probe + play: {:?}", r.requests);
    for d in &decisions {
        assert_eq!(query_param(d, "normalizeLoudness"), Some("1"), "{d}");
    }
    assert_eq!(
        query_param(decisions[0], "X-Plex-Session-Identifier"),
        query_param(decisions[1], "X-Plex-Session-Identifier"),
    );
    assert_enhanced(&r);
    plx_plex::plex::reset_servers_for_test();
}

/// The remote remux probe is the FIRST decision the enhanced ask reaches, so a server that refuses
/// (or ignores) it must be caught there. Before this, the probe's refusal read as "no Original" and
/// the play dropped to HLS — where main plays the Original remux — and, with nothing recorded, did
/// so on every play. Now the probe re-asks once without the params on the same session, samples
/// THAT remux, and the play is the plain Original remux with the outcome `Refused`.
#[track_caller]
fn remote_probe_fallback(mode: EnhMode) {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let bytes = remote_probe_plan(320).unwrap().target_bytes;
    let r = resolve(&mut ps, MDE_DIRECTPLAY, mode, bytes, "/library/parts/960001/1/file.avi", "ac3",
        |sid| {
            let mut item = ac3_item(sid);
            item.bitrate = 320;
            item
        },
        |env, client| {
            client.set_link(plx_plex::plex::probe::Location::Remote);
            env.quality = Quality::Auto;
        });
    assert!(r.plan.url.contains("start.mkv"), "the Original remux, not HLS: {} {:?}", r.plan.url, r.requests);
    assert!(!r.plan.url.contains(".m3u8"), "{}", r.plan.url);
    assert!(r.plan.contract.remux, "an Original remux");
    assert_eq!(r.plan.contract.delivery, plx_plex::plex::TranscodeDelivery::ProgressiveMkv);
    assert_eq!(r.plan.contract.audio, plx_plex::plex::AudioEnhancements::NONE);
    assert_eq!(query_param(&r.plan.url, "normalizeLoudness"), None, "{}", r.plan.url);
    assert_eq!(query_param(&r.plan.url, "boostDialog"), None, "{}", r.plan.url);
    assert_eq!(r.plan.enhancement, EnhancementOutcome::Refused);
    let decisions = transcode_decisions(&r.requests);
    assert!(decisions.iter().any(|d| query_param(d, "normalizeLoudness") == Some("1")), "asked once: {decisions:?}");
    // Only the probe's first decision carries the params: its re-ask and the play decision do not,
    // and no enhanced start.mkv was ever fetched — nothing enhanced is left on the session.
    assert_eq!(decisions.iter().filter(|d| any_param(&[d.to_string()])).count(), 1, "{decisions:?}");
    assert!(!r.requests.iter().any(|q| q.contains("start.mkv") && any_param(&[q.clone()])), "{:?}", r.requests);
    let sessions: Vec<_> = decisions.iter().map(|d| query_param(d, "X-Plex-Session-Identifier")).collect();
    assert!(sessions.windows(2).all(|w| w[0] == w[1]), "one session throughout: {sessions:?}");
    assert!(r.plan.verdict.is_none(), "a refused ENHANCEMENT is not a refused playback");
    apply_plan(&mut ps, r.plan, "rk-enh");
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Refused);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn remote_probe_refused_falls_back_to_remux() {
    remote_probe_fallback(EnhMode::Refuse);
}

#[test]
#[cfg(feature = "devtriggers")]
fn remote_probe_ignored_falls_back_to_remux() {
    remote_probe_fallback(EnhMode::Ignore);
}

/// M2's AAC 5.1: the audio is transcoded with or without the params, so a transcode in the answer
/// proves nothing. Graded at the classifier, because on this host's assumed caps every track the
/// direct-play pick can carry is in the profile's copy list — the resolve cannot reach the case,
/// but a device whose caps bound AAC to 2 channels does.
#[test]
fn aac51_source_outcome_unverified() {
    let mc: plx_plex::plex::MediaContainer = serde_json::from_str(
        r#"{"Metadata":[{"Media":[{"Part":[{"decision":"transcode","Stream":[{"streamType":1,"decision":"copy"},{"streamType":2,"codec":"ac3","decision":"transcode"}]}]}]}]}"#,
    ).unwrap();
    let copyable = CarriedAudio::from_stream(&track(1, "ac3", 2, true), 0);
    let not_copyable = CarriedAudio::from_stream(&track(1, "truehd", 8, true), 0);
    assert_eq!(classify_outcome(Some(&mc), Some(&copyable), PREF), EnhancementOutcome::Applied);
    assert_eq!(classify_outcome(Some(&mc), Some(&not_copyable), PREF), EnhancementOutcome::Unverified);
    assert_eq!(classify_outcome(None, Some(&copyable), PREF), EnhancementOutcome::Unverified);
    assert_eq!(classify_outcome(Some(&mc), Some(&copyable), plx_plex::plex::AudioEnhancements::NONE), EnhancementOutcome::Off);
}
