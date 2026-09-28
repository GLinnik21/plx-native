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
use crate::plex::serverinfo::Subscription;

const PREF: crate::plex::AudioEnhancements = crate::plex::AudioEnhancements {
    boost_dialog: false,
    normalize_loudness: true,
};

fn track(id: i64, codec: &str, channels: i64, capable: bool) -> crate::metadata::Stream {
    crate::metadata::Stream {
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
    item: impl FnOnce(ServerId) -> crate::metadata::PlayingItem,
    setup: impl FnOnce(&mut ResolveEnv, &crate::plex::Client),
) -> Resolve {
    assert!(crate::net::global_init() && crate::curlio::available());
    let (port, done, server) = enhancement_pms(mde, mode, media_bytes);
    let sid = crate::plex::register_for_test("enh-pms", "127.0.0.1", port, "token", "enh-client");
    let client = crate::plex::client_for(sid).unwrap();
    client.set_link(crate::plex::probe::Location::Local);
    let mut env = ResolveEnv::snapshot(ps, crate::stores::metadata::MetadataStore::default().view(), sid, "rk-enh");
    env.quality = Quality::Original;
    env.pass = Subscription::Yes;
    env.audio_enhancements = PREF;
    env.cached_item = Some(item(sid));
    setup(&mut env, client);
    let plan = build_stream("rk-enh", part, "hevc", acodec, &env);
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    Resolve { plan, requests }
}

const MKV: &str = "/library/parts/960001/1/file.mkv";

fn ac3_item(sid: ServerId) -> crate::metadata::PlayingItem {
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
    assert_eq!(r.plan.contract.audio, crate::plex::AudioEnhancements::NONE);
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
        crate::plex::serverinfo::store_for_test(env.sid, Subscription::Yes, "1.43.4");
        env.pass = Subscription::Unknown;
        env.pass = ResolveEnv::snapshot(&PlaybackSession::IDLE, crate::stores::metadata::MetadataStore::default().view(), env.sid, "rk-enh").pass;
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
    crate::plex::reset_servers_for_test();
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
    crate::plex::reset_servers_for_test();
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
    crate::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn incapable_track_is_direct_no_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        |sid| fourk_item(sid, vec![track(1, "ac3", 2, false)]), |_, _| {});
    assert_direct_no_params(&r);
    crate::plex::reset_servers_for_test();
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
    crate::plex::reset_servers_for_test();
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
    crate::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn dv_declared_p8_is_direct_no_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        |sid| {
            let mut item = ac3_item(sid);
            item.dovi = p8();
            item
        },
        |env, _| env.dv_capability = Some(crate::webos::caps::DvCapability::Supported));
    assert!(
        r.plan.dv_decision.presentation.declared().is_some(),
        "precondition: the P8 direct play declares DolbyHdrInfo",
    );
    assert_direct_no_params(&r);
    crate::plex::reset_servers_for_test();
}

/// I7's other half, at the predicate: a base whose DV base layer is not self-displayable is never
/// offered, whatever else holds (end to end such a file is refused Original before a candidate
/// exists, so the predicate is where the rule can be seen on its own).
#[test]
fn dv_unusable_base_is_direct_no_params() {
    let carried = CarriedAudio::from_stream(&track(1, "ac3", 2, true), 0);
    let mut base = test_original_candidate(None);
    base.audio = Some(carried.clone());
    let facts = |b| EnhancementFacts { pass: Subscription::Yes, base: Some(b), carried: Some(&carried), subtitle_shown: false, refused: false };
    assert!(enhancements_offered(&facts(&base), RouteFamily::Direct), "control: the clean base is offered");
    let mut unusable = base.clone();
    unusable.dovi = p5();
    assert!(unusable.dovi.base_layer_unusable());
    assert!(!enhancements_offered(&facts(&unusable), RouteFamily::Direct));
    assert_eq!(desired_audio(PREF, enhancements_offered(&facts(&unusable), RouteFamily::Direct)), crate::plex::AudioEnhancements::NONE);
}

#[test]
#[cfg(feature = "devtriggers")]
fn embedded_default_subtitle_is_direct_no_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3",
        |sid| fourk_item_with_subs(sid, vec![track(1, "ac3", 2, true)], vec![selected_sub(9, "srt")]),
        |_, _| {});
    assert!(r.plan.sub_render_ordinal.is_some(), "precondition: the embedded subtitle is shown");
    assert_direct_no_params(&r);
    crate::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn server_selected_external_srt_is_direct_sidecar_restored() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let external = crate::metadata::Stream {
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
    assert_direct_no_params(&r);
    assert!(
        !r.requests.iter().any(|l| l.starts_with("PUT ") && query_param(l, "subtitleStreamID").is_some_and(|v| v != "0")),
        "no subtitle PUT: {:?}", r.requests,
    );
    apply_plan(&mut ps, r.plan, "rk-enh");
    assert!(crate::player::sidecar::selected(), "the server's sidecar is restored on the direct play");
    crate::player::sidecar::deselect();
    crate::plex::reset_servers_for_test();
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
    assert_eq!(r.plan.contract.audio, crate::plex::AudioEnhancements::NONE);
    crate::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn remote_auto_hls_no_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    // No media bytes: the Remote probe of the Part fails, so Auto starts on HLS.
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3", ac3_item, |env, client| {
        client.set_link(crate::plex::probe::Location::Remote);
        env.quality = Quality::Auto;
    });
    assert!(matches!(r.plan.contract.delivery, crate::plex::TranscodeDelivery::FixedHls { .. }), "precondition: HLS");
    assert!(!any_param(&r.requests), "{:?}", r.requests);
    assert_eq!(r.plan.contract.audio, crate::plex::AudioEnhancements::NONE);
    crate::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn relay_no_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0, MKV, "ac3", ac3_item, |_, client| {
        client.set_link(crate::plex::probe::Location::Relay);
    });
    assert!(!any_param(&r.requests), "{:?}", r.requests);
    assert_eq!(r.plan.contract.audio, crate::plex::AudioEnhancements::NONE);
    crate::plex::reset_servers_for_test();
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
    crate::plex::reset_servers_for_test();
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
    crate::plex::reset_servers_for_test();
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
        crate::plex::reset_servers_for_test();
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
        crate::plex::reset_servers_for_test();
    }
}

#[track_caller]
fn assert_fell_back_to_direct(r: &Resolve) {
    assert!(r.plan.url.contains("/library/parts/") && !r.plan.url.contains("start.mkv"), "{}", r.plan.url);
    assert!(r.plan.tsession.is_empty());
    assert!(!r.plan.contract.remux);
    assert_eq!(r.plan.contract.audio, crate::plex::AudioEnhancements::NONE);
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
    crate::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn ignored_params_audio_copy_falls_back_refused() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let r = resolve(&mut ps, MDE_DIRECTPLAY, EnhMode::Ignore, 0, MKV, "ac3", ac3_item, |_, _| {});
    assert_fell_back_to_direct(&r);
    crate::plex::reset_servers_for_test();
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
            client.set_link(crate::plex::probe::Location::Remote);
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
    crate::plex::reset_servers_for_test();
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
            client.set_link(crate::plex::probe::Location::Remote);
            env.quality = Quality::Auto;
        });
    assert!(r.plan.url.contains("start.mkv"), "the Original remux, not HLS: {} {:?}", r.plan.url, r.requests);
    assert!(!r.plan.url.contains(".m3u8"), "{}", r.plan.url);
    assert!(r.plan.contract.remux, "an Original remux");
    assert_eq!(r.plan.contract.delivery, crate::plex::TranscodeDelivery::ProgressiveMkv);
    assert_eq!(r.plan.contract.audio, crate::plex::AudioEnhancements::NONE);
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
    crate::plex::reset_servers_for_test();
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
    let mc: crate::plex::MediaContainer = serde_json::from_str(
        r#"{"Metadata":[{"Media":[{"Part":[{"decision":"transcode","Stream":[{"streamType":1,"decision":"copy"},{"streamType":2,"codec":"ac3","decision":"transcode"}]}]}]}]}"#,
    ).unwrap();
    let copyable = CarriedAudio::from_stream(&track(1, "ac3", 2, true), 0);
    let not_copyable = CarriedAudio::from_stream(&track(1, "truehd", 8, true), 0);
    assert_eq!(classify_outcome(Some(&mc), Some(&copyable), PREF), EnhancementOutcome::Applied);
    assert_eq!(classify_outcome(Some(&mc), Some(&not_copyable), PREF), EnhancementOutcome::Unverified);
    assert_eq!(classify_outcome(None, Some(&copyable), PREF), EnhancementOutcome::Unverified);
    assert_eq!(classify_outcome(Some(&mc), Some(&copyable), crate::plex::AudioEnhancements::NONE), EnhancementOutcome::Off);
}
