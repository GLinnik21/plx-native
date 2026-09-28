//! Issue #266 PR 3: the LIVE audio-enhancement state machine — what a toggle, a track pick or a
//! subtitle pick does to a route that is already playing, and what a claimed `Retranscode` makes
//! of it ("reconcile at claim"). The step and dispatch tables are graded pure; everything that
//! reaches PMS runs against the loopback `enhancement_pms` and grades the wire it saw.
//!
//! Fixture vocabulary: the playing item has a capable AC3 2.0 track (`A1`), a capable E-AC3 JOC
//! 5.1 (`A2`), a capable AC3 5.1 (`A3`), a capable TrueHD the TV cannot decode (`A4`), and an AC3
//! 2.0 the server has no loudness analysis for (`A5`). "Enhanced" = the Normalize Loudness pref.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::apply_plan;
use crate::plex::serverinfo::Subscription;

const PREF: crate::plex::AudioEnhancements = crate::plex::AudioEnhancements {
    boost_dialog: false,
    normalize_loudness: true,
};
const NONE: crate::plex::AudioEnhancements = crate::plex::AudioEnhancements::NONE;

fn track(sid: i64, ordinal: i32, codec: &str, channels: i64, capable: bool, immersive: bool) -> CarriedAudio {
    CarriedAudio {
        sid,
        ordinal,
        codec: codec.into(),
        channels,
        can_normalize_loudness: capable,
        immersive,
    }
}
fn a1() -> CarriedAudio { track(11, 1, "ac3", 2, true, false) }
fn a2() -> CarriedAudio { track(12, 2, "eac3", 6, true, true) }
fn a3() -> CarriedAudio { track(13, 3, "ac3", 6, true, false) }
fn a4() -> CarriedAudio { track(14, 4, "truehd", 8, true, false) }
fn a5() -> CarriedAudio { track(15, 5, "ac3", 2, false, false) }

fn candidate(direct: bool, audio: CarriedAudio, subtitle_ordinal: Option<i32>) -> AutoOriginalCandidate {
    AutoOriginalCandidate {
        direct,
        audio: Some(audio),
        ..test_original_candidate(subtitle_ordinal)
    }
}

fn enhanced_contract(audio: crate::plex::AudioEnhancements) -> crate::plex::EncodeContract {
    crate::plex::EncodeContract {
        remux: true,
        delivery: crate::plex::TranscodeDelivery::ProgressiveMkv,
        no_video_copy: false,
        ceiling: None,
        audio,
    }
}

/// A registered loopback Plex Pass server for one test.
struct Live {
    sid: ServerId,
    port: i32,
    done: std::sync::mpsc::Sender<()>,
    server: std::thread::JoinHandle<Vec<String>>,
}

impl Live {
    fn start(mode: EnhMode) -> Self {
        assert!(crate::net::global_init() && crate::curlio::available());
        let (port, done, server) = enhancement_pms(MDE_DIRECTPLAY, mode, 0);
        let sid = crate::plex::register_for_test("enh-live", "127.0.0.1", port, "token", "enh-client");
        crate::plex::client_for(sid).unwrap().set_link(crate::plex::probe::Location::Local);
        crate::plex::serverinfo::store_for_test(sid, Subscription::Yes, "1.43.4");
        Live { sid, port, done, server }
    }

    /// Stop the fixture and hand back every request line it saw, in order.
    fn finish(self) -> Vec<String> {
        self.done.send(()).unwrap();
        let requests = self.server.join().unwrap();
        crate::plex::reset_servers_for_test();
        requests
    }
}

#[derive(Clone, Copy)]
enum Delivery {
    Direct,
    Remux(crate::plex::AudioEnhancements),
    Hls,
}

/// Land a playing route: `audio` carried, `sub_sid` shown, `cand` as the Original candidate.
fn install(
    ps: &mut PlaybackSession,
    live: &Live,
    route: Delivery,
    audio: CarriedAudio,
    cand: Option<AutoOriginalCandidate>,
    sub_sid: i64,
) {
    let port = live.port;
    let (url, tsession, contract, enhancement) = match route {
        Delivery::Direct => (
            format!("http://127.0.0.1:{port}/library/parts/960001/1/file.mkv"),
            String::new(),
            crate::plex::EncodeContract::default(),
            EnhancementOutcome::Off,
        ),
        Delivery::Remux(a) => (
            format!("http://127.0.0.1:{port}/video/:/transcode/universal/start.mkv?session=enh-remux-1"),
            "enh-remux-1".to_owned(),
            enhanced_contract(a),
            if a.any() { EnhancementOutcome::Applied } else { EnhancementOutcome::Off },
        ),
        Delivery::Hls => (
            format!("http://127.0.0.1:{port}/video/:/transcode/universal/start.m3u8"),
            "enh-hls-1".to_owned(),
            crate::plex::EncodeContract {
                delivery: crate::plex::TranscodeDelivery::FixedHls { seconds_per_segment: 2 },
                ceiling: Some(crate::abr::Rung::P1080High.ceiling()),
                ..Default::default()
            },
            EnhancementOutcome::Off,
        ),
    };
    apply_plan(
        ps,
        Plan {
            sid: live.sid,
            url,
            tsession,
            sess: "enh-logical".into(),
            part_id: 960001,
            vcodec: "hevc".into(),
            acodec: audio.codec.clone(),
            src_vcodec: "hevc".into(),
            src_acodec: audio.codec.clone(),
            contract,
            enhancement,
            transport_kbps: 28_000,
            audio: Some(audio),
            auto_original: cand,
            sub_sid,
            ..Default::default()
        },
        "960001",
    );
}

/// The toggle row minus persistence (`request_audio_enhancement` also retains a session write,
/// which a host test must not aim at the developer's real session file).
fn toggle(ps: &mut PlaybackSession, a: crate::plex::AudioEnhancements) -> bool {
    crate::player::restore_audio_enhancements(a);
    reconcile_enhancement(ps, false)
}

fn claim(ps: &mut PlaybackSession) -> (ClaimedRouteAction, ClaimTail) {
    let action = claim_route_action().expect("a queued user action");
    let tail = match action.intent {
        RouteIntent::User(UserRouteIntent::Retranscode) => execute_retranscode_claim(ps, &action, 60),
        RouteIntent::User(UserRouteIntent::RecoverOriginal(cause)) => {
            execute_recover_original_claim(ps, &action, 60, cause)
        }
        ref other => panic!("unexpected claim {other:?}"),
    };
    (action, tail)
}

/// What the pump's tail does for a PMS half that prepared a route.
fn settle(ps: &mut PlaybackSession, action: &ClaimedRouteAction, tail: ClaimTail) {
    match tail {
        ClaimTail::Retranscode | ClaimTail::NativeAudio => {
            finish_route_action(ps, action, RouteApplyResult::Prepared)
        }
        ClaimTail::Original(_) => {}
        ClaimTail::Rejected(_) => finish_route_action(ps, action, RouteApplyResult::Rejected),
    }
}

fn desired_audio_idx() -> i32 {
    crate::player::SHARED.desired_audio_idx.load(std::sync::atomic::Ordering::Relaxed)
}

fn decisions(requests: &[String]) -> Vec<&String> {
    requests.iter().filter(|r| r.contains("/decision?") && !r.contains("hasMDE=1")).collect()
}

fn phase() -> ControlPhase {
    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).phase
}

fn cleanup(ps: &mut PlaybackSession) {
    let _ = take_pending_original();
    restore_quality(Quality::Original);
    crate::player::restore_audio_enhancements(NONE);
    reset_session(ps);
    install_active_encoder("");
    reset_player_control_for_test(ps);
    crate::player::reset_audio_track();
    crate::player::reset_subtitle();
}

// ---- step and dispatch ---------------------------------------------------------------------

#[test]
fn step_decides_from_session_without_metadata() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), None)), 0);
    assert_eq!(enhancement_step(&ps), EnhancementStep::NotInvolved, "pref off");
    crate::player::restore_audio_enhancements(PREF);
    // No metadata store exists anywhere in this test: the facts are the session's own.
    assert_eq!(enhancement_step(&ps), EnhancementStep::Remux(enhanced_contract(PREF)));
    live.finish();
    cleanup(&mut ps);
}

#[test]
fn step_table_each_row() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    // Row 1: wanted and different from applied.
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), None)), 0);
    assert_eq!(enhancement_step(&ps), EnhancementStep::Remux(enhanced_contract(PREF)));
    // Wanted and already applied: nothing to do.
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    assert_eq!(enhancement_step(&ps), EnhancementStep::NotInvolved);
    crate::player::restore_audio_enhancements(NONE);
    // Row 2: applied, no longer wanted, direct candidate.
    assert_eq!(enhancement_step(&ps), EnhancementStep::ReleaseToDirect);
    // Row 3: same, remux candidate.
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(false, a1(), None)), 0);
    assert_eq!(enhancement_step(&ps), EnhancementStep::Remux(enhanced_contract(NONE)));
    // Row 4: no candidate at all.
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), None, 0);
    assert_eq!(enhancement_step(&ps), EnhancementStep::NotInvolved);
    // HLS is never offered, whatever the preference.
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Hls, a1(), Some(candidate(true, a1(), None)), 0);
    assert_eq!(enhancement_step(&ps), EnhancementStep::NotInvolved);
    live.finish();
    cleanup(&mut ps);
}

#[test]
fn claim_dispatch_table_each_cell() {
    let c = enhanced_contract(PREF);
    use ClaimFallback::{Legacy, Reject};
    let cells = [
        (EnhancementStep::ReleaseToDirect, false, ClaimPrimary::ReleaseToDirect, Reject),
        (EnhancementStep::ReleaseToDirect, true, ClaimPrimary::ReleaseToDirect, Legacy),
        (EnhancementStep::Remux(c), false, ClaimPrimary::Remux(c), Reject),
        (EnhancementStep::Remux(c), true, ClaimPrimary::Remux(c), Legacy),
        (EnhancementStep::NotInvolved, true, ClaimPrimary::Legacy, Reject),
        (EnhancementStep::NotInvolved, false, ClaimPrimary::Retranscode, Reject),
    ];
    for (step, displaced, primary, on_failure) in cells {
        assert_eq!(
            claim_dispatch(step, displaced),
            Dispatch { primary, on_failure },
            "{step:?} displaced={displaced}",
        );
    }
}

// ---- recovery ------------------------------------------------------------------------------

#[test]
fn recovery_flavour_uses_candidate_family_not_live_hls() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    let cand = candidate(true, a1(), None);
    install(&mut ps, &live, Delivery::Hls, a1(), Some(cand.clone()), 0);
    assert!(matches!(ps.cur_contract.delivery, crate::plex::TranscodeDelivery::FixedHls { .. }));
    assert_eq!(recovery_flavour(&cand, recovery_want(&ps, &cand)), RecoveryFlavour::Remux(PREF));
    crate::player::restore_audio_enhancements(NONE);
    assert_eq!(recovery_flavour(&cand, recovery_want(&ps, &cand)), RecoveryFlavour::Direct);
    live.finish();
    cleanup(&mut ps);
}

#[test]
fn recovery_hls_bootstrap_pref_on_is_enhanced_remux() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    restore_quality(Quality::Auto);
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Hls, a1(), Some(candidate(true, a1(), None)), 0);
    let ticket = worker_ticket();
    assert_eq!(
        recover_auto_to_original_for(&mut ps, &ticket, 60, RecoveryCause::Automatic),
        Some(AutoOriginalReload::Remux),
    );
    assert_eq!(query_param(&ps.url, "normalizeLoudness"), Some("1"), "{}", ps.url);
    assert_eq!(ps.cur_contract, enhanced_contract(PREF));
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Applied);
    assert_eq!(ps.stream_acodec, "ac3", "I4: the decision's output codec");
    let requests = live.finish();
    assert_eq!(decisions(&requests).len(), 1, "{requests:?}");
    cleanup(&mut ps);
}

#[test]
fn recovery_hls_bootstrap_pref_off_is_direct() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    restore_quality(Quality::Auto);
    install(&mut ps, &live, Delivery::Hls, a1(), Some(candidate(true, a1(), None)), 0);
    let ticket = worker_ticket();
    assert_eq!(
        recover_auto_to_original_for(&mut ps, &ticket, 60, RecoveryCause::Automatic),
        Some(AutoOriginalReload::Direct),
    );
    assert_eq!(ps.cur_contract.audio, NONE);
    let requests = live.finish();
    assert!(decisions(&requests).is_empty(), "{requests:?}");
    cleanup(&mut ps);
}

#[test]
fn enhancement_released_contract_allows_auto_and_original() {
    for (quality, watched) in [(Quality::Auto, true), (Quality::Original, false), (Quality::P720, false)] {
        let mut ps = PlaybackSession::IDLE;
        let _g = fresh_registry(&mut ps);
        let live = Live::start(EnhMode::Honor("ac3"));
        restore_quality(quality);
        // The applied half of the reducer follows the restored quality from here on.
        reset_player_control_for_test(&ps);
        install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
        let ticket = worker_ticket();
        let got = recover_auto_to_original_for(&mut ps, &ticket, 60, RecoveryCause::EnhancementReleased);
        if quality == Quality::P720 {
            assert_eq!(got, None, "a fixed rung is not the Original family");
        } else {
            assert_eq!(got, Some(AutoOriginalReload::Direct), "{quality:?}");
            assert_eq!(ps.cur_auto_original_watched, watched, "{quality:?}");
            assert_eq!(ps.cur_contract.audio, NONE);
            assert_eq!(ps.cur_enhancement, EnhancementOutcome::Off);
        }
        live.finish();
        cleanup(&mut ps);
    }
}

// ---- toggling ------------------------------------------------------------------------------

#[test]
fn toggle_on_from_direct_queues_retranscode_then_remux_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), None)), 0);
    assert!(toggle(&mut ps, PREF));
    assert!(pending_user_route_intent(UserRouteIntent::Retranscode));
    let (action, tail) = claim(&mut ps);
    assert!(!action.displaced_pick, "a bare toggle displaces no pick");
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert_eq!(ps.cur_contract, enhanced_contract(PREF));
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Applied);
    assert_eq!(ps.stream_acodec, "ac3");
    assert!(ps.tsession.starts_with("enh-logical-"), "same logical session: {}", ps.tsession);
    let requests = live.finish();
    let d = decisions(&requests);
    assert_eq!(d.len(), 1, "{requests:?}");
    assert_eq!(query_param(d[0], "normalizeLoudness"), Some("1"));
    cleanup(&mut ps);
}

#[test]
fn toggle_off_with_direct_candidate_releases_to_direct() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    restore_quality(Quality::Auto);
    reset_player_control_for_test(&ps);
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    assert!(toggle(&mut ps, NONE));
    let (_, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Original(AutoOriginalReload::Direct));
    assert!(original_recovery_pending());
    assert!(!is_transcoding(&ps));
    assert_eq!(ps.cur_contract.audio, NONE);
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Off);
    assert!(ps.cur_auto_original_watched, "an Auto playback keeps its watchdog");
    assert_eq!(applied_quality(), Quality::Auto, "Auto quality preserved");
    let requests = live.finish();
    assert!(decisions(&requests).is_empty(), "a release opens the raw Part: {requests:?}");
    cleanup(&mut ps);
}

#[test]
fn toggle_off_with_remux_candidate_is_plain_remux() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(false, a1(), None)), 0);
    assert!(toggle(&mut ps, NONE));
    let (action, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert_eq!(ps.cur_contract, enhanced_contract(NONE));
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Off);
    let requests = live.finish();
    assert!(!requests.iter().any(|r| r.contains("normalizeLoudness")), "{requests:?}");
    cleanup(&mut ps);
}

#[test]
fn toggle_during_pending_original_sets_deferred_reconcile() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    restore_quality(Quality::Auto);
    install(&mut ps, &live, Delivery::Hls, a1(), Some(candidate(true, a1(), None)), 0);
    let ticket = worker_ticket();
    assert_eq!(
        recover_auto_to_original_for(&mut ps, &ticket, 60, RecoveryCause::Automatic),
        Some(AutoOriginalReload::Direct),
    );
    assert!(!toggle(&mut ps, PREF), "the trial owns the route");
    assert!(!pending_user_route_intent(UserRouteIntent::Retranscode));
    assert!(PLAYER_CONTROL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pending_original
        .as_ref()
        .is_some_and(|p| p.deferred_reconcile));
    settle_pending_native_start(&mut ps, RouteStartResult::Started);
    confirm_original_recovery(&mut ps);
    assert!(pending_user_route_intent(UserRouteIntent::Retranscode), "commit runs the reconcile");
    let (action, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert_eq!(ps.cur_contract, enhanced_contract(PREF));
    live.finish();
    cleanup(&mut ps);
}

#[test]
fn toggle_during_pending_original_rollback_drops_the_flag() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    restore_quality(Quality::Auto);
    install(&mut ps, &live, Delivery::Hls, a1(), Some(candidate(true, a1(), None)), 0);
    let ticket = worker_ticket();
    recover_auto_to_original_for(&mut ps, &ticket, 60, RecoveryCause::Automatic).unwrap();
    assert!(!toggle(&mut ps, PREF));
    assert!(rollback_original_recovery(&mut ps).is_some());
    // The flag travels with the rollback's deferred effects and applies to the restored HLS
    // route, where the enhancement is never offered: nothing is queued.
    settle_pending_native_start(&mut ps, RouteStartResult::Started);
    assert!(!pending_user_route_intent(UserRouteIntent::Retranscode));
    assert_eq!(ps.cur_contract.audio, NONE);
    live.finish();
    cleanup(&mut ps);
}

// ---- audio picks ---------------------------------------------------------------------------

#[test]
fn capable_pick_while_enhanced_then_off_releases_to_new_track() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    commit_audio_selection(&mut ps, a3());
    assert_eq!(ps.auto_original.as_ref().and_then(|c| c.audio.clone()), Some(a3()), "retargeted");
    assert!(toggle(&mut ps, NONE));
    let (_, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Original(AutoOriginalReload::Direct));
    assert_eq!(ps.cur_audio, Some(a3()));
    assert_eq!(ps.stream_acodec, "ac3");
    assert_eq!(desired_audio_idx(), 3, "the direct branch feeds the NEW track");
    live.finish();
    cleanup(&mut ps);
}

/// A pick that leaves the offer standing keeps the enhanced remux (`retranscode_contract`): the
/// step is `NotInvolved` because the params already match, and today's re-encode would drop them.
#[test]
fn capable_pick_while_enhanced_keeps_the_enhanced_remux() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    commit_audio_selection(&mut ps, a3());
    let (action, tail) = claim(&mut ps);
    assert!(!action.displaced_pick);
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert_eq!(ps.cur_contract, enhanced_contract(PREF));
    assert_eq!(ps.cur_audio, Some(a3()));
    let requests = live.finish();
    let d = decisions(&requests);
    assert_eq!(query_param(d[0], "audioStreamID"), Some("13"));
    assert_eq!(query_param(d[0], "normalizeLoudness"), Some("1"));
    cleanup(&mut ps);
}

#[test]
fn eac3_joc_to_ac3_release_immersive_false() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a2(), Some(candidate(true, a2(), None)), 0);
    let stream = crate::metadata::Stream {
        id: 13,
        index: 3,
        codec: "ac3".into(),
        channels: 6,
        can_normalize_loudness: true,
        ..Default::default()
    };
    crate::app::playback::commit_track(
        &mut ps,
        crate::ui::track_menu::TrackCommit::Audio(CarriedAudio::from_stream(&stream, 3)),
    );
    assert!(toggle(&mut ps, NONE));
    let (_, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Original(AutoOriginalReload::Direct));
    assert_eq!(ps.stream_acodec, "ac3");
    assert!(!ps.stream_immersive, "AC3 is never the Atmos path");
    live.finish();
    cleanup(&mut ps);
}

#[test]
fn pending_release_plus_audio_pick_uses_new_track() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    assert!(toggle(&mut ps, NONE));
    let (_, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Original(AutoOriginalReload::Direct));
    commit_audio_selection(&mut ps, a3());
    assert_eq!(ps.cur_audio, Some(a1()), "deferred behind the trial");
    settle_pending_native_start(&mut ps, RouteStartResult::Started);
    confirm_original_recovery(&mut ps);
    assert_eq!(ps.cur_audio, Some(a3()));
    assert!(pending_user_route_intent(UserRouteIntent::NativeAudioReload));
    assert_eq!(desired_audio_idx(), 3);
    assert_eq!(ps.stream_acodec, "ac3");
    live.finish();
    cleanup(&mut ps);
}

#[test]
fn native_capable_pick_with_pref_on_goes_remux_not_native() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Direct, a5(), Some(candidate(true, a5(), None)), 0);
    assert_eq!(enhancement_step(&ps), EnhancementStep::NotInvolved, "A5 is not capable");
    commit_audio_selection(&mut ps, a1());
    assert!(pending_user_route_intent(UserRouteIntent::Retranscode));
    let (action, tail) = claim(&mut ps);
    assert!(action.displaced_pick);
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert_eq!(ps.cur_contract, enhanced_contract(PREF));
    assert_eq!(ps.cur_audio, Some(a1()));
    let requests = live.finish();
    assert_eq!(query_param(decisions(&requests)[0], "audioStreamID"), Some("11"));
    cleanup(&mut ps);
}

#[test]
fn non_direct_playable_pick_drops_candidate_legacy_retranscode_rows_absent() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Direct, a5(), Some(candidate(true, a5(), None)), 0);
    commit_audio_selection(&mut ps, a4());
    assert!(ps.auto_original.is_none(), "a direct candidate cannot feed TrueHD");
    assert!(pending_user_route_intent(UserRouteIntent::Retranscode));
    let (action, tail) = claim(&mut ps);
    assert!(!action.displaced_pick, "the legacy reload was queued itself");
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert!(!ps.cur_contract.remux);
    assert_eq!(ps.cur_contract.audio, NONE);
    assert!(!audio_enhancements_offered_live(&ps), "rows absent");
    let requests = live.finish();
    assert!(!requests.iter().any(|r| r.contains("normalizeLoudness")), "{requests:?}");
    cleanup(&mut ps);
}

// ---- subtitle picks ------------------------------------------------------------------------

#[test]
fn subtitle_off_on_direct_with_pref_converges_to_remux() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), Some(2))), 77);
    assert_eq!(enhancement_step(&ps), EnhancementStep::NotInvolved, "a shown subtitle hides it");
    commit_subtitle_selection(&mut ps, -1, 0, false);
    assert!(pending_user_route_intent(UserRouteIntent::Retranscode));
    let (action, tail) = claim(&mut ps);
    assert!(!action.displaced_pick, "a direct subtitle never reloads");
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert_eq!(ps.cur_contract, enhanced_contract(PREF));
    let requests = live.finish();
    let put = requests
        .iter()
        .position(|r| r.starts_with("PUT /library/parts/960001") && query_param(r, "subtitleStreamID") == Some("0"))
        .expect("Off PUTs subtitleStreamID=0");
    let decision = requests
        .iter()
        .position(|r| r.contains("/decision?") && r.contains("normalizeLoudness=1"))
        .expect("the enhanced decision");
    assert!(put < decision, "{requests:?}");
    cleanup(&mut ps);
}

#[test]
fn subtitle_off_then_rejected_decision_keeps_sub0_no_burn_on_seek() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Refuse);
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), Some(2))), 77);
    commit_subtitle_selection(&mut ps, -1, 0, false);
    let (action, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Rejected(ENHANCEMENT_REJECTED));
    settle(&mut ps, &action, tail);
    assert_eq!(ps.cur_sub_sid, 0, "the Off was applied in place and survives the rejection");
    assert!(!is_transcoding(&ps));
    assert_eq!(ps.cur_contract.audio, NONE);
    assert_eq!(transcode_seek(&mut ps, 90), None, "direct play seeks by byte, never a burn");
    let requests = live.finish();
    assert!(!requests.iter().any(|r| query_param(r, "subtitleStreamID") == Some("77")), "{requests:?}");
    cleanup(&mut ps);
}

#[test]
fn subtitle_pick_while_enhanced_auto_releases_client_rendered() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    restore_quality(Quality::Auto);
    reset_player_control_for_test(&ps);
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    commit_subtitle_selection(&mut ps, 2, 77, true);
    assert_eq!(ps.auto_original.as_ref().and_then(|c| c.subtitle_ordinal), Some(2));
    let (action, tail) = claim(&mut ps);
    assert!(action.displaced_pick, "on a remux the subtitle's own refresh was displaced");
    assert_eq!(tail, ClaimTail::Original(AutoOriginalReload::Direct));
    assert_eq!(
        crate::player::SHARED.desired_sub_idx.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "client-rendered on the direct play",
    );
    assert!(ps.cur_auto_original_watched);
    let requests = live.finish();
    assert!(decisions(&requests).is_empty(), "no burn: {requests:?}");
    cleanup(&mut ps);
}

#[test]
fn subtitle_pick_while_enhanced_original_releases_client_rendered() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    // An external sidecar: no demuxer ordinal, drawn by `player::sidecar` once direct.
    commit_subtitle_selection(&mut ps, -1, 78, true);
    assert!(ps.auto_original.is_some());
    let (_, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Original(AutoOriginalReload::Direct));
    assert!(!ps.cur_auto_original_watched, "Original quality is not watched");
    let requests = live.finish();
    assert!(
        !requests.iter().any(|r| r.contains("/decision?") && query_param(r, "subtitleStreamID") == Some("78")),
        "no burn subtitleStreamID: {requests:?}",
    );
    cleanup(&mut ps);
}

#[test]
fn subtitle_commit_not_deferred_during_pending_original() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    restore_quality(Quality::Auto);
    install(&mut ps, &live, Delivery::Hls, a1(), Some(candidate(true, a1(), Some(2))), 77);
    let ticket = worker_ticket();
    recover_auto_to_original_for(&mut ps, &ticket, 60, RecoveryCause::Automatic).unwrap();
    commit_subtitle_selection(&mut ps, -1, 0, false);
    assert_eq!(ps.cur_sub_sid, 0, "Off takes effect immediately");
    live.finish();
    cleanup(&mut ps);
}

// ---- rejections and the displaced pick -----------------------------------------------------

#[test]
fn rejected_remux_with_displaced_audio_runs_native_in_claim() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Refuse);
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Direct, a5(), Some(candidate(true, a5(), None)), 0);
    commit_audio_selection(&mut ps, a3());
    let (action, tail) = claim(&mut ps);
    let revision = desired_contract_revision();
    assert!(action.displaced_pick);
    assert_eq!(tail, ClaimTail::NativeAudio);
    assert_eq!(desired_audio_idx(), 3);
    assert_eq!(ps.stream_acodec, "ac3");
    assert!(matches!(phase(), ControlPhase::Applying(_)), "the PMS half never settles a reload tail");
    assert_eq!(desired_contract_revision(), revision, "no user-contract advance mid-claim");
    settle(&mut ps, &action, tail);
    assert!(matches!(phase(), ControlPhase::Prepared(_)), "settled once, Prepared");
    assert!(claim_route_action().is_none(), "nothing else queued");
    live.finish();
    cleanup(&mut ps);
}

#[test]
fn rejected_release_with_displaced_subtitle_runs_legacy_burn() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    commit_subtitle_selection(&mut ps, 2, 77, true);
    // The release is refused at claim (the applied contract is no longer Original-family).
    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).applied_quality = Quality::P720;
    let (action, tail) = claim(&mut ps);
    assert!(action.displaced_pick);
    assert_eq!(tail, ClaimTail::Retranscode, "the subtitle's own burn refresh ran instead");
    settle(&mut ps, &action, tail);
    assert!(!ps.cur_contract.remux);
    assert_eq!(ps.cur_contract.audio, NONE, "a burn never keeps the params (I6)");
    let requests = live.finish();
    let d = decisions(&requests);
    assert_eq!(d.len(), 1, "{requests:?}");
    assert!(!d[0].contains("normalizeLoudness"));
    assert!(requests.iter().any(|r| r.starts_with("PUT") && query_param(r, "subtitleStreamID") == Some("77")));
    cleanup(&mut ps);
}

#[test]
fn bare_toggle_rejected_retained_row_shows_applied() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Refuse);
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), None)), 0);
    assert!(toggle(&mut ps, PREF));
    assert_eq!(displayed_audio_enhancements(&ps), PREF, "in flight: what was asked");
    let (action, tail) = claim(&mut ps);
    assert!(!action.displaced_pick);
    assert_eq!(tail, ClaimTail::Rejected(ENHANCEMENT_REJECTED));
    settle(&mut ps, &action, tail);
    assert_eq!(displayed_audio_enhancements(&ps), NONE, "settled: what plays");
    assert!(!is_transcoding(&ps), "current stream retained");
    assert!(audio_enhancements_offered_live(&ps), "pressing the row again retries");
    live.finish();
    cleanup(&mut ps);
}

#[test]
fn not_involved_with_displaced_pick_native_switch_no_decision() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Direct, a5(), Some(candidate(true, a5(), None)), 0);
    commit_audio_selection(&mut ps, a2());
    assert!(!toggle(&mut ps, NONE), "nothing applied, nothing wanted");
    let (action, tail) = claim(&mut ps);
    assert!(action.displaced_pick);
    assert_eq!(tail, ClaimTail::NativeAudio);
    assert_eq!(desired_audio_idx(), 2);
    assert_eq!(ps.stream_acodec, "eac3");
    let requests = live.finish();
    assert!(decisions(&requests).is_empty(), "{requests:?}");
    cleanup(&mut ps);
}

#[test]
fn not_involved_latest_pick_wins_native_a3() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Direct, a5(), Some(candidate(true, a5(), None)), 0);
    commit_audio_selection(&mut ps, a2());
    commit_audio_selection(&mut ps, a3());
    toggle(&mut ps, NONE);
    let (_, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::NativeAudio);
    assert_eq!(desired_audio_idx(), 3);
    assert_eq!(ps.stream_acodec, "ac3", "A3's payload codec");
    live.finish();
    cleanup(&mut ps);
}

#[test]
fn absorbed_native_without_enhancement_unchanged() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), None)), 0);
    commit_audio_selection(&mut ps, a3());
    assert!(pending_user_route_intent(UserRouteIntent::NativeAudioReload));
    crate::player::request_transcode_refresh(&ps);
    let (action, tail) = claim(&mut ps);
    assert!(!action.displaced_pick, "only an enhancement reconcile sets the marker");
    assert_eq!(tail, ClaimTail::Retranscode, "retranscode_for, exactly as before");
    settle(&mut ps, &action, tail);
    assert!(!ps.cur_contract.remux);
    assert_eq!(ps.cur_contract.audio, NONE);
    live.finish();
    cleanup(&mut ps);
}

#[test]
fn displaced_pick_consumed_by_recover_original_runs_once() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Direct, a5(), Some(candidate(true, a5(), None)), 0);
    commit_audio_selection(&mut ps, a3());
    crate::player::request_original_recovery(&ps);
    let (action, tail) = claim(&mut ps);
    assert_eq!(
        action.intent,
        RouteIntent::User(UserRouteIntent::RecoverOriginal(RecoveryCause::ManualOriginal)),
    );
    assert!(action.displaced_pick, "the marker rides the intent that absorbed it");
    assert_eq!(tail, ClaimTail::NativeAudio, "Original refused on direct play: the pick runs");
    settle(&mut ps, &action, tail);
    assert!(!PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).displaced_pick);
    assert!(claim_route_action().is_none(), "runs once");
    live.finish();
    cleanup(&mut ps);
}

#[test]
fn request_play_clears_displaced_pick() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    queue_user_route_intent(&ps, UserRouteIntent::Retranscode, true);
    assert!(PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).displaced_pick);
    assert!(begin_playback_request());
    assert!(!PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).displaced_pick);
    queue_user_route_intent(&ps, UserRouteIntent::Retranscode, true);
    begin_engine_teardown(false);
    assert!(!PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).displaced_pick, "teardown too");
    cleanup(&mut ps);
}

#[test]
fn other_arm_with_displaced_pick_stores_index_before_reload() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    { let s = &mut ps; s.cur_audio = Some(a3()); s.stream_acodec = "eac3".into(); }
    crate::player::set_audio_track(1);
    honour_displaced_pick(&mut ps, false, "NativeAudioReload");
    assert_eq!(desired_audio_idx(), 1, "no marker, no change");
    honour_displaced_pick(&mut ps, true, "NativeAudioReload");
    assert_eq!(desired_audio_idx(), 3);
    assert_eq!(ps.stream_acodec, "ac3");
    cleanup(&mut ps);
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "legacy_action Direct+None")]
fn legacy_action_direct_none_routes_retranscode() {
    let _ = legacy_action(&PlaybackSession::IDLE);
}

#[test]
#[cfg(not(debug_assertions))]
fn legacy_action_direct_none_routes_retranscode() {
    assert_eq!(legacy_action(&PlaybackSession::IDLE), LegacyAction::Retranscode);
}

// ---- rollback and HLS ----------------------------------------------------------------------

#[test]
fn remux_release_open_failure_rolls_back_with_audio_intact() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    assert_eq!(worker_ticket().encoder(), "enh-remux-1");
    assert!(toggle(&mut ps, NONE));
    let (_, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Original(AutoOriginalReload::Direct));
    assert!(sync_active_hls_to_session(&mut ps).is_none(), "a remux is not an HLS route");
    assert!(rollback_original_recovery(&mut ps).is_some());
    assert_eq!(ps.tsession, "enh-remux-1");
    assert_eq!(active_encoder(), "enh-remux-1");
    assert_eq!(ps.cur_contract, enhanced_contract(PREF), "audio intact");
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Applied);
    // The pump rebases the restored route through `transcode_seek`; it carries the params.
    assert!(transcode_seek(&mut ps, 60).is_some());
    assert_eq!(query_param(&ps.url, "normalizeLoudness"), Some("1"), "{}", ps.url);
    let requests = live.finish();
    // Only the rebase, once its replacement decision was accepted, may retire the remux key.
    let rebase = requests.iter().position(|r| r.contains("/decision?")).expect("rebase decision");
    let stop = requests.iter().position(|r| r.contains("/stop") && r.contains("enh-remux-1"));
    assert!(
        stop.is_none_or(|s| s > rebase),
        "the remux encoder is not stopped before first frames or a rebase: {requests:?}",
    );
    cleanup(&mut ps);
}

#[test]
fn hls_audio_and_subtitle_picks_still_drop_candidate() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    install(&mut ps, &live, Delivery::Hls, a1(), Some(candidate(true, a1(), Some(2))), 77);
    commit_subtitle_selection(&mut ps, -1, 0, false);
    assert_eq!(ps.auto_original.as_ref().map(|c| c.subtitle_ordinal), Some(None), "Off edits");
    commit_audio_selection(&mut ps, a3());
    assert!(ps.auto_original.is_none(), "an audio pick on HLS drops");
    install(&mut ps, &live, Delivery::Hls, a1(), Some(candidate(true, a1(), None)), 0);
    commit_subtitle_selection(&mut ps, 2, 77, true);
    assert!(ps.auto_original.is_none(), "a subtitle pick on HLS drops");
    live.finish();
    cleanup(&mut ps);
}

#[test]
fn transcode_seek_preserves_enhancement_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    assert!(transcode_seek(&mut ps, 120).is_some());
    assert_eq!(query_param(&ps.url, "normalizeLoudness"), Some("1"), "{}", ps.url);
    assert_eq!(ps.cur_contract, enhanced_contract(PREF));
    let requests = live.finish();
    assert_eq!(query_param(decisions(&requests)[0], "normalizeLoudness"), Some("1"));
    cleanup(&mut ps);
}

// ---- review fixes: a fixed rung, and a refused recovery ------------------------------------

/// D1 (I5): a fixed rung picked on an enhanced Original remux is a bitrate cap, and the enhanced
/// remux is uncapped by definition — the rebuild must honour the ceiling and drop the params, not
/// keep the enhanced remux and erase the cap the picker now shows.
#[test]
fn fixed_quality_pick_on_enhanced_remux_honours_ceiling_drops_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    restore_quality(Quality::Original);
    reset_player_control_for_test(&ps);
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    set_quality(&mut ps, Quality::P1080);
    let (action, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    let cap = Quality::P1080.ceiling();
    assert!(cap.is_some());
    assert_eq!(ps.cur_contract.ceiling, cap, "the picked cap is what plays");
    assert!(!ps.cur_contract.remux, "a capped route is a re-encode, not the Original remux");
    assert_eq!(ps.cur_contract.audio, NONE, "the params never ride a fixed rung");
    assert_ne!(ps.cur_enhancement, EnhancementOutcome::Applied);
    assert!(!audio_enhancements_offered_live(&ps));
    let requests = live.finish();
    let d = decisions(&requests);
    assert_eq!(d.len(), 1, "{requests:?}");
    assert!(!d[0].contains("normalizeLoudness"), "{}", d[0]);
    assert!(query_param(d[0], "maxVideoBitrate").is_some(), "{}", d[0]);
    cleanup(&mut ps);
}

/// D2: a server that will not apply the params must not strand Auto on HLS. The recovery
/// re-decides once as the plain Original the candidate already proved, and records Refused so no
/// later reconcile asks again this playback.
fn recovery_enhanced_remux_falls_back(mode: EnhMode, direct: bool) {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(mode);
    restore_quality(Quality::Auto);
    reset_player_control_for_test(&ps);
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Hls, a1(), Some(candidate(direct, a1(), None)), 0);
    let ticket = worker_ticket();
    let got = recover_auto_to_original_for(&mut ps, &ticket, 60, RecoveryCause::Automatic);
    assert_eq!(
        got,
        Some(if direct { AutoOriginalReload::Direct } else { AutoOriginalReload::Remux }),
        "{mode:?} direct={direct}",
    );
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Refused, "{mode:?} direct={direct}");
    assert_eq!(ps.cur_contract.audio, NONE);
    assert!(!ps.url.contains("normalizeLoudness"), "{}", ps.url);
    assert_eq!(enhancement_step(&ps), EnhancementStep::NotInvolved, "Refused ends the offer");
    let requests = live.finish();
    let d = decisions(&requests);
    assert_eq!(d.len(), if direct { 1 } else { 2 }, "{requests:?}");
    assert!(d[0].contains("normalizeLoudness"));
    if !direct {
        assert!(!d[1].contains("normalizeLoudness"), "{}", d[1]);
    }
    cleanup(&mut ps);
}

#[test]
fn recovery_enhanced_remux_refused_falls_back_to_plain_original_refused() {
    recovery_enhanced_remux_falls_back(EnhMode::Refuse, true);
    recovery_enhanced_remux_falls_back(EnhMode::Refuse, false);
}

#[test]
fn recovery_enhanced_remux_ignored_falls_back_to_plain_original_refused() {
    recovery_enhanced_remux_falls_back(EnhMode::Ignore, true);
    recovery_enhanced_remux_falls_back(EnhMode::Ignore, false);
}
