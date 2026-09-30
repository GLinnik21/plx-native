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

/// A registered loopback Plex Pass server for one test.
struct Live {
    sid: ServerId,
    port: i32,
    done: std::sync::mpsc::Sender<()>,
    server: std::thread::JoinHandle<Vec<String>>,
}

impl Live {
    fn start(mode: EnhMode) -> Self {
        Self::start_with_parts(mode, 0, PartAnswer::Serve)
    }

    /// A server whose raw Part GETs answer `parts`, and whose media GETs serve `media_bytes`.
    fn start_with_parts(mode: EnhMode, media_bytes: usize, parts: PartAnswer) -> Self {
        assert!(crate::net::global_init() && crate::curlio::available());
        let (port, done, server) = enhancement_pms_parts(MDE_DIRECTPLAY, mode, media_bytes, parts);
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
    /// M7 follow-up: an ALREADY-APPLIED Burn (`remux: false`, forced re-encode) — for tests that
    /// need to start mid-play already burning a subtitle, rather than transitioning into one.
    Burn(crate::plex::AudioEnhancements),
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
            enhanced_remux_contract(a, false),
            if a.any() { EnhancementOutcome::Applied } else { EnhancementOutcome::Off },
        ),
        Delivery::Burn(a) => (
            format!("http://127.0.0.1:{port}/video/:/transcode/universal/start.mkv?session=enh-remux-1"),
            "enh-remux-1".to_owned(),
            enhanced_remux_contract(a, true),
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
        RouteIntent::User(UserRouteIntent::Retranscode) => {
            match execute_retranscode_claim(ps, &action, 60, -1, 0) {
                RetranscodeClaimDispatch::Sync(tail) => tail,
                // The PMS half now runs on a worker (the freeze fix): wait for its landing the
                // same way the pump does on a later frame, against the SAME loopback fixture this
                // test already drives, then apply it exactly as `take_ready_retranscode_claim`
                // would.
                RetranscodeClaimDispatch::Pending => {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                    loop {
                        if let Some((_, tail, _, _)) = take_ready_retranscode_claim(ps) {
                            break tail;
                        }
                        assert!(std::time::Instant::now() < deadline, "retranscode claim worker never landed");
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            }
        }
        RouteIntent::User(UserRouteIntent::RecoverOriginal(cause)) => {
            execute_recover_original_claim(ps, &action, 60, cause)
        }
        ref other => panic!("unexpected claim {other:?}"),
    };
    (action, tail)
}

/// What the pump's tail does for a PMS half that prepared a route. Returns whether the action was
/// still owned (`ControlPhase::Applying(action.serial)`) at settle time — `false` means a teardown
/// or later action already superseded it, exactly the case `finish_route_action`'s own `bool`
/// return exists to report; callers that need it use it, callers that always expect ownership just
/// discard it as before.
fn settle(ps: &mut PlaybackSession, action: &ClaimedRouteAction, tail: ClaimTail) -> bool {
    match tail {
        ClaimTail::Retranscode | ClaimTail::NativeAudio => {
            finish_route_action(ps, action, RouteApplyResult::Prepared)
        }
        ClaimTail::Original(_) => true,
        ClaimTail::Rejected(_) => finish_route_action(ps, action, RouteApplyResult::Rejected),
    }
}

fn desired_audio_idx() -> i32 {
    crate::player::SHARED.desired_audio_idx.load(std::sync::atomic::Ordering::Relaxed)
}

fn decisions(requests: &[String]) -> Vec<&String> {
    requests.iter().filter(|r| r.contains("/decision?") && !r.contains("hasMDE=1")).collect()
}

/// Same shape as [`Live::start`], but the live `/decision` answer (not the `hasMDE=1` probe) is
/// held for `delay` before it is written. Gives a test a reliable window in which the claim's
/// worker is known to be "in flight" — past its own initial ticket check, waiting on the network —
/// so a concurrent, main-thread route event started from the test can land inside that window
/// without racing thread start-up.
fn slow_live(delay: std::time::Duration) -> Live {
    slow_live_logged(delay).0
}

/// [`slow_live`] plus a request log the test can read WHILE the fixture runs (the ordinary
/// `Live::finish` only hands the requests back once the fixture is stopped). Each request line is
/// pushed before its response is written, so a client that has been answered can rely on the log
/// already holding the line: a "was X stopped yet?" assertion never races the fixture's own
/// bookkeeping.
type RequestLog = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

fn slow_live_logged(delay: std::time::Duration) -> (Live, RequestLog) {
    assert!(crate::net::global_init() && crate::curlio::available());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port() as i32;
    listener.set_nonblocking(true).unwrap();
    let (done, stop) = std::sync::mpsc::channel();
    let log: RequestLog = Default::default();
    let shared = log.clone();
    let server = std::thread::spawn(move || {
        loop {
            match crate::testnet::accept(&listener) {
                Ok((mut socket, _)) => {
                    let line = drain_http(&mut socket);
                    shared.lock().unwrap_or_else(|e| e.into_inner()).push(line.clone());
                    if line.contains("/decision?") && line.contains("hasMDE=1") {
                        write_json(&mut socket, MDE_DIRECTPLAY);
                    } else if line.contains("/decision?") {
                        std::thread::sleep(delay);
                        write_json(&mut socket, MDE_TRANSCODE_COPY);
                    } else {
                        write_json(&mut socket, EMPTY_MC);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if stop.try_recv().is_ok() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(e) => panic!("fixture accept: {e}"),
            }
        }
        shared.lock().unwrap_or_else(|e| e.into_inner()).clone()
    });
    let sid = crate::plex::register_for_test("enh-live-slow", "127.0.0.1", port, "token", "enh-client");
    crate::plex::client_for(sid).unwrap().set_link(crate::plex::probe::Location::Local);
    crate::plex::serverinfo::store_for_test(sid, Subscription::Yes, "1.43.4");
    (Live { sid, port, done, server }, log)
}

/// Whether the fixture has been asked to stop `session` (an encoder's transcode session id).
fn stop_seen(log: &RequestLog, session: &str) -> bool {
    log.lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|r| r.contains("/video/:/transcode/universal/stop") && r.contains(&format!("session={session}")))
}

/// Any encoder stop at all (for a session whose id the worker minted).
fn any_stop_seen(log: &RequestLog) -> bool {
    log.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|r| r.contains("/video/:/transcode/universal/stop"))
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !ready() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn landing_posted() -> bool {
    RETRANSCODE_CLAIM_SLOT.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// A server whose every live `/decision` (not the `hasMDE=1` probe) answers `generalDecisionCode
/// 2000`, regardless of whether the request carries the enhancement params. `enhancement_pms`'s own
/// `EnhMode::Refuse` only fires for an ENHANCED request (issue #266's fixture is scoped to that
/// case), so a plain rebuild with no enhancement params — a Legacy displaced-pick burn, for
/// instance — always succeeds against it. This fixture refuses everything, for a test that needs a
/// PLAIN retranscode attempt to fail server-side.
fn always_refusing_live() -> Live {
    assert!(crate::net::global_init() && crate::curlio::available());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port() as i32;
    listener.set_nonblocking(true).unwrap();
    let (done, stop) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        loop {
            match crate::testnet::accept(&listener) {
                Ok((mut socket, _)) => {
                    let line = drain_http(&mut socket);
                    if line.contains("/decision?") && line.contains("hasMDE=1") {
                        write_json(&mut socket, MDE_DIRECTPLAY);
                    } else if line.contains("/decision?") {
                        write_json(
                            &mut socket,
                            br#"{"MediaContainer":{"generalDecisionCode":2000,"transcodeDecisionCode":4020,"transcodeDecisionText":"synthetic plain refusal","Metadata":[]}}"#,
                        );
                    } else {
                        write_json(&mut socket, EMPTY_MC);
                    }
                    requests.push(line);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if stop.try_recv().is_ok() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(e) => panic!("fixture accept: {e}"),
            }
        }
        requests
    });
    let sid = crate::plex::register_for_test("enh-live-refuse", "127.0.0.1", port, "token", "enh-client");
    crate::plex::client_for(sid).unwrap().set_link(crate::plex::probe::Location::Local);
    crate::plex::serverinfo::store_for_test(sid, Subscription::Yes, "1.43.4");
    Live { sid, port, done, server }
}

/// The staleness protection the async split newly depends on: the old synchronous
/// `retranscode_as` held the frame thread for its whole `/decision` round trip, so nothing else
/// on that thread could run while it waited. Now a claim's PMS half runs on a worker, which opens
/// a real window in which some OTHER main-thread route event (leaving this item to start a fresh
/// Load, an ABR commit, a stop) can change the ticket the worker snapshotted. `try_retranscode`'s
/// `is_worker_ticket_current` check and `replace_active_encoder_for`'s own commit-time check are
/// exactly what must catch that: the stale worker's landing must discard cleanly (no session
/// mutation), and the phase it releases must not block whatever happens next.
#[test]
fn claim_ticket_invalidated_while_worker_in_flight_is_discarded_then_a_fresh_pick_applies() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = slow_live(std::time::Duration::from_millis(150));
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), None)), 0);
    assert!(toggle(&mut ps, PREF));
    let action = claim_route_action().expect("a queued user action");
    let dispatch = execute_retranscode_claim(&mut ps, &action, 60, -1, 0);
    assert!(
        matches!(dispatch, RetranscodeClaimDispatch::Pending),
        "expected the PMS half to move to a worker, got {dispatch:?}",
    );

    // A concurrent, newer route event bumps the engine/media epoch while the worker above is
    // still waiting on the (deliberately slow) `/decision` response.
    begin_engine_teardown(true);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let (action, tail, ..) = loop {
        if let Some(landing) = take_ready_retranscode_claim(&mut ps) {
            break landing;
        }
        assert!(std::time::Instant::now() < deadline, "retranscode claim worker never landed");
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    assert!(
        matches!(tail, ClaimTail::Rejected(_)),
        "a ticket invalidated mid-flight must discard the claim, got {tail:?}",
    );
    settle(&mut ps, &action, tail);
    assert_eq!(
        ps.cur_enhancement,
        EnhancementOutcome::Off,
        "the discarded worker must not have touched the session projection",
    );
    assert_eq!(phase(), ControlPhase::Stable, "the discard must release the reducer");

    // The second, current pick is unaffected by the first's discard: a fresh claim against the
    // now-current ticket applies normally.
    assert!(toggle(&mut ps, PREF));
    let action2 = claim_route_action().expect("phase returned to Stable after the discard");
    let dispatch2 = execute_retranscode_claim(&mut ps, &action2, 60, -1, 0);
    let tail2 = match dispatch2 {
        RetranscodeClaimDispatch::Sync(tail) => tail,
        RetranscodeClaimDispatch::Pending => {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Some((_, tail, _, _)) = take_ready_retranscode_claim(&mut ps) {
                    break tail;
                }
                assert!(std::time::Instant::now() < deadline, "second worker never landed");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    };
    assert_eq!(tail2, ClaimTail::Retranscode, "the second, current pick must still apply");
    settle(&mut ps, &action2, tail2);
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Applied);

    live.finish();
    cleanup(&mut ps);
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
    assert_eq!(enhancement_step(&ps), EnhancementStep::Remux(enhanced_remux_contract(PREF, false)));
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
    assert_eq!(enhancement_step(&ps), EnhancementStep::Remux(enhanced_remux_contract(PREF, false)));
    // Wanted and already applied: nothing to do.
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    assert_eq!(enhancement_step(&ps), EnhancementStep::NotInvolved);
    crate::player::restore_audio_enhancements(NONE);
    // Row 2: applied, no longer wanted, direct candidate.
    assert_eq!(enhancement_step(&ps), EnhancementStep::ReleaseToDirect);
    // Row 3: same, remux candidate.
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(false, a1(), None)), 0);
    assert_eq!(enhancement_step(&ps), EnhancementStep::Remux(enhanced_remux_contract(NONE, false)));
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
    let c = enhanced_remux_contract(PREF, false);
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
    assert_eq!(ps.cur_contract, enhanced_remux_contract(PREF, false));
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
    assert_eq!(ps.cur_contract, enhanced_remux_contract(PREF, false));
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Applied);
    assert_eq!(ps.stream_acodec, "ac3");
    assert!(ps.tsession.starts_with("enh-logical-"), "same logical session: {}", ps.tsession);
    let requests = live.finish();
    let d = decisions(&requests);
    assert_eq!(d.len(), 1, "{requests:?}");
    assert_eq!(query_param(d[0], "normalizeLoudness"), Some("1"));
    cleanup(&mut ps);
}

/// The freeze this guards against: `execute_retranscode_claim` used to run `put_selection` (a
/// synchronous PUT) and `/decision` (a synchronous GET, up to 15s on stable HTTP) right on
/// whichever thread called it — in production, the per-frame pump thread. `FrameScope` marks a
/// thread as that frame thread; `assert_may_block`, wired into `http::request_with` (the one PMS
/// dispatch chokepoint), panics the instant PMS I/O happens inside a `FrameScope` without an
/// explicit `allow_blocking` escape (see `task::blocking::tests::a_helper_call_inside_a_frame_is_rejected`
/// for the same mechanism guarding a storage call). If this call still ran `put_selection`/
/// `/decision` inline, the `execute_retranscode_claim` call below would panic with "main-thread
/// block: PMS HTTP" instead of returning `Pending`.
#[test]
fn claim_frees_the_frame_thread_pms_call_runs_on_a_worker() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), None)), 0);
    assert!(toggle(&mut ps, PREF));
    let action = claim_route_action().expect("a queued user action");

    let _frame = crate::task::FrameScope::enter();
    let dispatch = execute_retranscode_claim(&mut ps, &action, 60, -1, 0);
    assert!(
        matches!(dispatch, RetranscodeClaimDispatch::Pending),
        "expected the PMS half to move to a worker, got {dispatch:?}",
    );
    drop(_frame);

    // The worker landed on its own thread (FrameScope's depth is thread-local and starts at 0
    // there), so it was free to block on the loopback fixture; the frame thread above never did.
    // Drain the mailbox exactly the way the pump does on a later frame.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let (action, tail, ..) = loop {
        if let Some(landing) = take_ready_retranscode_claim(&mut ps) {
            break landing;
        }
        assert!(std::time::Instant::now() < deadline, "retranscode claim worker never landed");
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Applied);
    assert_eq!(ps.stream_acodec, "ac3");
    live.finish();
    cleanup(&mut ps);
}

/// A refused spawn (the OS could not create the worker thread) must settle exactly like a
/// refused decision: `ControlPhase` returns to `Stable` and the session is untouched, never left
/// stuck in `Applying` forever. `force_next_retranscode_spawn_refusal` is a test-only seam
/// (`spawn_small`'s stack size is fixed, so the `unsatisfiable stack` trick `task::tests` uses is
/// not reachable here) that makes `spawn_retranscode_claim` report refusal without actually
/// spawning — see its doc comment in `route::decision` for why this mirrors
/// `storage_worker::Writer::start_refused`.
#[test]
fn a_refused_spawn_settles_like_a_refused_decision_not_a_stuck_applying() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), None)), 0);
    assert!(toggle(&mut ps, PREF));
    let action = claim_route_action().expect("a queued user action");

    force_next_retranscode_spawn_refusal();
    let dispatch = execute_retranscode_claim(&mut ps, &action, 60, -1, 0);
    let tail = match dispatch {
        RetranscodeClaimDispatch::Sync(tail) => tail,
        RetranscodeClaimDispatch::Pending => panic!("a forced spawn refusal must resolve synchronously"),
    };
    assert!(
        matches!(tail, ClaimTail::Rejected(_)),
        "a refused spawn must settle as a rejection, got {tail:?}",
    );
    settle(&mut ps, &action, tail);
    assert_eq!(phase(), ControlPhase::Stable, "a refused spawn must not leave the reducer Applying");
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Off, "nothing reached PMS; the session is untouched");

    // The reducer is usable again: a fresh claim (this time with a real spawn) applies normally.
    assert!(toggle(&mut ps, PREF));
    let (action, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Applied);

    live.finish();
    cleanup(&mut ps);
}

/// The async split moved WHERE `try_retranscode` runs, never WHAT it does on a refusal: it still
/// speculatively starts a physical encoder (`next_encoder_session` + `transcode_start_url`)
/// before it can know the server will refuse the params, and still must stop that exact session
/// rather than leak it. This is unchanged production code (`try_retranscode`'s own refusal arms),
/// graded here through the new async claim path to prove the split did not drop it.
#[test]
fn a_refused_retranscode_still_stops_the_speculative_encoder() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Refuse);
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), None)), 0);
    assert!(toggle(&mut ps, PREF));
    let (action, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Rejected(ENHANCEMENT_REJECTED));
    settle(&mut ps, &action, tail);
    assert_eq!(
        ps.cur_enhancement,
        EnhancementOutcome::Off,
        "refusal falls back to the prior, unenhanced route",
    );
    assert!(
        !is_transcoding(&ps),
        "still Direct: the refused speculative encoder never became the live route",
    );
    let requests = live.finish();
    assert!(
        requests.iter().any(|r| r.contains("/video/:/transcode/universal/stop")),
        "a refused decision must stop the encoder it speculatively started: {requests:?}",
    );
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

/// A direct candidate whose Original is this server's own Part (the shape a real resolve
/// installs: `probe_part` is the Part key, not a fixture URL).
fn server_part_candidate(audio: CarriedAudio) -> AutoOriginalCandidate {
    AutoOriginalCandidate {
        probe_part: "/library/parts/960001/1/file.mkv".into(),
        ..candidate(true, audio, None)
    }
}

fn part_gets(requests: &[String]) -> Vec<&String> {
    requests.iter().filter(|r| r.starts_with("GET /library/parts/")).collect()
}

/// PR 4 device run (PMS 1.43.4, rk=72): the release of a cold-started enhanced remux opened the
/// Original Part, PMS answered **503**, and the rollback restored the ENHANCED remux — so the
/// viewer who switched Normalize Loudness off kept hearing it, under a menu and a persisted
/// preference that both said off. A Part the server will not serve is never opened as the trial:
/// the release lands on the candidate's plain remux (the same codec-copy Original, no DSP), which
/// is what the resolve builds whenever the server will not direct-play.
#[test]
fn release_with_refused_part_lands_on_the_plain_remux_not_the_enhanced_route() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start_with_parts(EnhMode::Honor("ac3"), 4096, PartAnswer::Refuse);
    restore_quality(Quality::Original);
    reset_player_control_for_test(&ps);
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(server_part_candidate(a1())), 0);
    assert!(toggle(&mut ps, NONE));
    let (_, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Original(AutoOriginalReload::Remux), "a refused Part is not a direct trial");
    assert!(original_recovery_pending(), "the plain remux is still a trial the held route backs");
    assert!(ps.url.contains("start.mkv"), "{}", ps.url);
    assert!(ps.cur_contract.remux);
    assert_eq!(ps.cur_contract.audio, NONE, "the preference the viewer set is what plays");
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Off);
    let requests = live.finish();
    assert_eq!(part_gets(&requests).len(), 1, "one admission request: {requests:?}");
    assert!(
        part_gets(&requests)[0].contains("X-Plex-Session-Identifier=enh-remux-1"),
        "the admission asks on the exact identity the Part body would use: {requests:?}"
    );
    let d = decisions(&requests);
    assert_eq!(d.len(), 1, "{requests:?}");
    assert_eq!(query_param(d[0], "normalizeLoudness"), None, "a PLAIN remux: {}", d[0]);
    assert_eq!(query_param(d[0], "directStreamAudio"), Some("1"), "{}", d[0]);
    cleanup(&mut ps);
}

/// The admitted case is unchanged: the Part answers, so the release opens it as the trial.
#[test]
fn release_with_admitted_part_is_still_direct_play() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start_with_parts(EnhMode::Honor("ac3"), 4096, PartAnswer::Serve);
    restore_quality(Quality::Original);
    reset_player_control_for_test(&ps);
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(server_part_candidate(a1())), 0);
    assert!(toggle(&mut ps, NONE));
    let (_, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Original(AutoOriginalReload::Direct));
    assert!(ps.url.contains("/library/parts/960001/1/file.mkv"), "{}", ps.url);
    assert!(!is_transcoding(&ps));
    assert_eq!(ps.cur_contract.audio, NONE);
    let requests = live.finish();
    assert_eq!(part_gets(&requests).len(), 1, "{requests:?}");
    assert!(decisions(&requests).is_empty(), "no remux was registered: {requests:?}");
    cleanup(&mut ps);
}

/// A transport failure mid-body is not the server's own refusal. `admit_original_part`'s own doc
/// says an unanswered question keeps the trial: `ThroughputFailure::BodyRead` — a known status
/// followed by a connection that dies before delivering the promised body — belongs in that
/// "let the trial's own open decide" bucket, not in `Refused`. Before the fix this fell into
/// `Refused` alongside a real `503`, and the release landed on the plain remux exactly as
/// `release_with_refused_part_lands_on_the_plain_remux_not_the_enhanced_route` does; this test
/// asserts the opposite outcome for the opposite kind of failure.
#[test]
fn release_with_body_reset_on_part_is_still_admitted_not_refused() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start_with_parts(EnhMode::Honor("ac3"), 4096, PartAnswer::Reset);
    restore_quality(Quality::Original);
    reset_player_control_for_test(&ps);
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(server_part_candidate(a1())), 0);
    assert!(toggle(&mut ps, NONE));
    let (_, tail) = claim(&mut ps);
    assert_eq!(
        tail,
        ClaimTail::Original(AutoOriginalReload::Direct),
        "a transport failure reading the body is not a refusal; the trial's own open still decides"
    );
    assert!(ps.url.contains("/library/parts/960001/1/file.mkv"), "{}", ps.url);
    assert!(!is_transcoding(&ps));
    let requests = live.finish();
    assert_eq!(part_gets(&requests).len(), 1, "{requests:?}");
    assert!(decisions(&requests).is_empty(), "no remux was registered: {requests:?}");
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
    assert_eq!(ps.cur_contract, enhanced_remux_contract(NONE, false));
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
    assert_eq!(ps.cur_contract, enhanced_remux_contract(PREF, false));
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
    assert_eq!(ps.cur_contract, enhanced_remux_contract(PREF, false));
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
    assert_eq!(ps.cur_contract, enhanced_remux_contract(PREF, false));
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
    // M7: an embedded subtitle already on screen no longer hides the offer — it wants a burn.
    assert_eq!(enhancement_step(&ps), EnhancementStep::Remux(enhanced_remux_contract(PREF, true)));
    commit_subtitle_selection(&mut ps, -1, 0, false);
    assert!(pending_user_route_intent(UserRouteIntent::Retranscode));
    let (action, tail) = claim(&mut ps);
    assert!(!action.displaced_pick, "a direct subtitle never reloads");
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert_eq!(ps.cur_contract, enhanced_remux_contract(PREF, false));
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

/// M7 / owner decision superseded the old I6 behaviour this test's name once described: picking
/// an EMBEDDED subtitle while a Plex Pass enhancement is live no longer releases to Direct with
/// the subtitle client-rendered — it stays on the enhanced route and forces a burn, keeping BOTH
/// the subtitle and the audio processing in the one re-encode.
#[test]
fn subtitle_pick_while_enhanced_auto_burns_embedded() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    restore_quality(Quality::Auto);
    reset_player_control_for_test(&ps);
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    commit_subtitle_selection(&mut ps, 2, 77, true);
    // The candidate still follows the pick (a later Off can still recover to Direct with it
    // client-rendered), but the LIVE route does not release to it — it burns instead.
    assert_eq!(ps.auto_original.as_ref().and_then(|c| c.subtitle_ordinal), Some(2));
    assert_eq!(enhancement_step(&ps), EnhancementStep::Remux(enhanced_remux_contract(PREF, true)));
    let (action, tail) = claim(&mut ps);
    // `reconcile_enhancement` marks its own queued Retranscode as a displaced pick whenever it
    // fires mid-transcode (`legacy_reloads == transcoding`) — the burn IS the subtitle's own
    // reload, so this is the correct bookkeeping, not a separate legacy fallback underneath it.
    assert!(action.displaced_pick);
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert_eq!(ps.cur_contract, enhanced_remux_contract(PREF, true));
    assert!(!ps.cur_contract.remux, "a burn is a real re-encode");
    let requests = live.finish();
    let d = decisions(&requests);
    assert_eq!(d.len(), 1, "{requests:?}");
    assert_eq!(query_param(d[0], "normalizeLoudness"), Some("1"), "the burn keeps the enhancement");
    assert!(
        requests.iter().any(|r| r.starts_with("PUT") && query_param(r, "subtitleStreamID") == Some("77")),
        "{requests:?}"
    );
    cleanup(&mut ps);
}

/// A subtitle the client renders itself (an external sidecar) is UNAFFECTED **by the
/// enhancement's own route** (M7 / owner decision): [`enhancement_step`] sees no reason to
/// change anything, and the DSP preference is never dropped. Picking ANY subtitle while already
/// transcoding still gets the pre-existing, M7-unrelated safety burn (`commit_track`'s own doc:
/// "while transcoding the commit above already asked for a burn ... selecting here is harmless")
/// — that mechanism does not know or care whether the pick was a sidecar or embedded, and this
/// test is not the place to change it. What M7 owns, and what this proves, is narrower: the burn
/// request rides ALONGSIDE the still-live `normalizeLoudness` param, never displacing it, and
/// [`enhancement_step`] itself never treats a sidecar as something to release or re-route for.
#[test]
fn subtitle_pick_while_enhanced_sidecar_keeps_the_enhancement() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    // An external sidecar: no demuxer ordinal, drawn by `player::sidecar` once direct.
    commit_subtitle_selection(&mut ps, -1, 78, true);
    assert!(ps.auto_original.is_some(), "the candidate still follows the pick for a later Off");
    assert_eq!(
        enhancement_step(&ps),
        EnhancementStep::NotInvolved,
        "a sidecar is never something the ENHANCEMENT'S OWN route reacts to"
    );
    let (_, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Retranscode, "the pre-existing while-transcoding refresh, not a release");
    let requests = live.finish();
    let d = decisions(&requests);
    assert!(!d.is_empty(), "{requests:?}");
    assert!(
        d.iter().all(|r| query_param(r, "normalizeLoudness") == Some("1")),
        "the enhancement is never dropped for a sidecar pick: {requests:?}",
    );
    cleanup(&mut ps);
}

// ---- M7 follow-up: mid-play picks while a Burn is ALREADY the live route -------------------
//
// The tests above (`subtitle_pick_while_enhanced_auto_burns_embedded` etc.) all TRANSITION into
// a Burn from a plain remux or Direct. These four instead start already burning
// (`Delivery::Burn`) and change ONE thing at a time, to pin down the gap the owner's follow-up
// named directly: a subtitle or audio change while enhanced must never re-request with a STALE
// `subtitleStreamID`/`audioStreamID` — `commit_audio_selection`/`commit_subtitle_selection` write
// `ps.cur_audio`/`ps.cur_sub_sid` unconditionally, before `reconcile_enhancement` ever queues the
// claim that reads them back ("reconcile at claim"), so the wire request is built from what was
// JUST picked, never from what was playing before it.

/// Picking a DIFFERENT embedded subtitle while already burning must burn the NEW id, not the one
/// that was already on screen.
#[test]
fn subtitle_change_while_burning_uses_the_new_id_not_the_stale_one() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    restore_quality(Quality::Auto);
    reset_player_control_for_test(&ps);
    crate::player::restore_audio_enhancements(PREF);
    // Already burning subtitle id 77 (embedded ordinal 2).
    install(&mut ps, &live, Delivery::Burn(PREF), a1(), Some(candidate(true, a1(), Some(2))), 77);
    assert!(live_is_own_burn(&ps), "fixture models an ALREADY-applied Burn");
    // Pick a different embedded subtitle: ordinal 5, PMS stream id 88. The route FLAVOUR does not
    // change (still an enhanced Burn either way), so `enhancement_step` itself reads `NotInvolved`
    // — the id change rides `commit_subtitle_selection`'s own unconditional
    // `request_transcode_refresh` (the pre-existing "already transcoding" safety burn), not a
    // route the enhancement machinery thinks it owns.
    commit_subtitle_selection(&mut ps, 5, 88, true);
    assert_eq!(ps.cur_sub_sid, 88, "the fresh pick, written unconditionally");
    assert_eq!(enhancement_step(&ps), EnhancementStep::NotInvolved, "same route flavour, still a Burn");
    let (action, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    let requests = live.finish();
    assert!(
        requests.iter().any(|r| r.starts_with("PUT") && query_param(r, "subtitleStreamID") == Some("88")),
        "the new id: {requests:?}"
    );
    assert!(
        !requests.iter().any(|r| query_param(r, "subtitleStreamID") == Some("77")),
        "must not still carry the stale id: {requests:?}"
    );
    cleanup(&mut ps);
}

/// Turning the subtitle Off while already burning must drop the burn (a plain enhanced remux —
/// no `subtitles=burn`, no `subtitleStreamID`) while keeping the DSP preference itself.
#[test]
fn subtitle_off_while_burning_drops_the_burn_keeps_the_enhancement() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    restore_quality(Quality::Auto);
    reset_player_control_for_test(&ps);
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Burn(PREF), a1(), Some(candidate(true, a1(), Some(2))), 77);
    commit_subtitle_selection(&mut ps, -1, 0, true);
    assert_eq!(ps.cur_sub_sid, 0);
    assert_eq!(
        enhancement_step(&ps),
        EnhancementStep::Remux(enhanced_remux_contract(PREF, false)),
        "back to a plain enhanced remux, no burn"
    );
    let (action, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert!(ps.cur_contract.remux, "no longer forced to re-encode");
    let requests = live.finish();
    let d = decisions(&requests);
    let last = d.last().expect("a decision was made");
    assert_eq!(query_param(last, "normalizeLoudness"), Some("1"), "the enhancement itself survives");
    assert_eq!(query_param(last, "subtitles"), None, "no burn request once Off: {last}");
    assert!(
        !requests.iter().any(|r| query_param(r, "subtitleStreamID") == Some("77")),
        "must not still ask PMS to burn the stale id: {requests:?}"
    );
    cleanup(&mut ps);
}

/// An audio change while already burning must carry the NEW `audioStreamID`, never the one that
/// was playing before the pick, and must keep the burn (the on-screen subtitle did not change).
#[test]
fn audio_change_while_burning_uses_the_new_id_and_keeps_the_burn() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    restore_quality(Quality::Auto);
    reset_player_control_for_test(&ps);
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Burn(PREF), a1(), Some(candidate(true, a1(), Some(2))), 77);
    commit_audio_selection(&mut ps, a3());
    assert_eq!(ps.cur_audio, Some(a3()), "the fresh pick, written unconditionally");
    let (action, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert!(!ps.cur_contract.remux, "still a real re-encode: the burn is unaffected by the audio pick");
    let requests = live.finish();
    let d = decisions(&requests);
    let last = d.last().expect("a decision was made");
    assert_eq!(query_param(last, "audioStreamID"), Some("13"), "A3's id — the NEW pick");
    assert_ne!(query_param(last, "audioStreamID"), Some("11"), "not A1's stale id");
    assert_eq!(query_param(last, "normalizeLoudness"), Some("1"));
    cleanup(&mut ps);
}

/// Turning the enhancement preference off while it is burning must release all the way back to
/// direct play, with the subtitle the candidate carries restored client-side — never left
/// pointing at the server burn that no longer exists.
#[test]
fn enhancement_off_while_burning_releases_to_direct_with_the_subtitle_restored() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    restore_quality(Quality::Auto);
    reset_player_control_for_test(&ps);
    crate::player::restore_audio_enhancements(PREF);
    // The candidate is a DIRECT one carrying subtitle ordinal 2 (client-renderable) — the state a
    // real resolve leaves when the embedded pick first forced the burn.
    install(&mut ps, &live, Delivery::Burn(PREF), a1(), Some(candidate(true, a1(), Some(2))), 77);
    assert!(toggle(&mut ps, NONE));
    let (_, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Original(AutoOriginalReload::Direct));
    assert!(original_recovery_pending());
    assert!(!is_transcoding(&ps), "back to the raw Part");
    assert_eq!(ps.cur_contract.audio, NONE);
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Off);
    assert_eq!(
        ps.auto_original.as_ref().and_then(|c| c.subtitle_ordinal),
        Some(2),
        "the candidate still names the subtitle to render client-side once Direct lands"
    );
    let requests = live.finish();
    assert!(decisions(&requests).is_empty(), "a release opens the raw Part, no /decision: {requests:?}");
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

/// M7 narrowed this: the enhancement only ever burns when IT is what wants the burn
/// (`enhancement_step`), which requires `enhancement_quality()` — Auto or Original. Under a fixed
/// rung the enhancement is never involved at all (I5), so an embedded subtitle pick there still
/// runs the ordinary "displaced pick's own reload" path (`legacy_action`) — which burns the
/// subtitle on its own account, carrying no Plex Pass params, because there is no offer here to
/// carry them from.
#[test]
fn displaced_subtitle_on_a_fixed_rung_runs_legacy_burn_without_enhancement_params() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    restore_quality(Quality::P720);
    crate::player::restore_audio_enhancements(PREF);
    // The applied route is still Remux-shaped (a fixed-rung refresh has not landed yet) — the
    // same in-flight seam the original regression exercised, now driven by the quality gate
    // itself rather than a field mutation `enhancement_step` never reads.
    install(&mut ps, &live, Delivery::Remux(PREF), a1(), Some(candidate(true, a1(), None)), 0);
    assert_eq!(enhancement_step(&ps), EnhancementStep::NotInvolved, "a fixed rung is never the enhancement's business (I5)");
    commit_subtitle_selection(&mut ps, 2, 77, true);
    let (action, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Retranscode, "the subtitle's own burn refresh ran instead");
    settle(&mut ps, &action, tail);
    assert!(!ps.cur_contract.remux);
    assert_eq!(ps.cur_contract.audio, NONE, "no offer was ever standing here to carry params from");
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
    assert_eq!(ps.cur_contract, enhanced_remux_contract(PREF, false), "audio intact");
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
    assert_eq!(ps.cur_contract, enhanced_remux_contract(PREF, false));
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

// ---- device bug (PR #309): a live re-pick of an already-burning embedded subtitle -----------
//
// The device case `audio_enhancement_burns_embedded_subtitle` (rk 1804) boots straight onto the
// enhancement's own Burn — PMS remembers the part's subtitle selection across runs, so a fresh
// boot with an embedded default already lands on `enhancement_route == Burn` (M7), not on a plain
// enhanced remux the way `subtitle_pick_while_enhanced_auto_burns_embedded` transitions INTO one.
// The `install()` fixture used everywhere else in this file writes `ps.cur_contract` etc. by hand
// and never exercises the real cold-start plan builder, so it cannot see whatever the resolve path
// leaves behind that `commit_subtitle_selection` then reads differently than the synthetic fixture
// does. This test goes through `build_stream`/`apply_plan` for the COLD START, exactly like
// `plan_audio_enhancement_tests::embedded_default_subtitle_with_enhancement_is_burned`, then drives
// the SAME live re-pick the device's menupick trigger made (the identical subtitle id already
// burning) through the real `commit_subtitle_selection`/claim path.
#[test]
#[cfg(feature = "devtriggers")]
fn subtitle_repick_while_cold_start_burn_keeps_the_burn() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::Auto);
    reset_player_control_for_test(&ps);
    crate::player::restore_audio_enhancements(PREF);

    let (port, done, server) = enhancement_pms(MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0);
    let sid = crate::plex::register_for_test("enh-repick", "127.0.0.1", port, "token", "enh-client");
    let client = crate::plex::client_for(sid).unwrap();
    client.set_link(crate::plex::probe::Location::Local);
    crate::plex::serverinfo::store_for_test(sid, Subscription::Yes, "1.43.4");

    let audio = crate::metadata::Stream {
        id: 10976,
        index: 1,
        lang_code: "eng".into(),
        codec: "ac3".into(),
        channels: 2,
        default: true,
        selected: true,
        can_normalize_loudness: true,
        ..Default::default()
    };
    let sub = selected_sub(10980, "srt");
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "1804");
    env.quality = Quality::Auto;
    env.pass = Subscription::Yes;
    env.audio_enhancements = PREF;
    env.cached_item = Some(fourk_item_with_subs(sid, vec![audio], vec![sub]));
    let plan = build_stream("1804", "/library/parts/3058/1/file.mkv", "hevc", "ac3", &env);
    assert!(!plan.contract.remux, "cold start must be a real burn re-encode, not a plain remux");
    assert_eq!(query_param(&plan.url, "subtitleStreamID"), Some("10980"), "{}", plan.url);
    assert_eq!(query_param(&plan.url, "subtitles"), Some("burn"), "{}", plan.url);
    apply_plan(&mut ps, plan, "1804");
    assert!(is_transcoding(&ps), "cold start landed on a transcode");
    assert!(live_is_own_burn(&ps), "the installed route must read as the enhancement's own Burn");

    // The device's menupick trigger re-picks the SAME embedded subtitle already burning — the
    // harness pressed OK on a track menu row that was already selected. `sub_idx` is arbitrary (a
    // burn never fills `sub_render_ordinal`; the menu computes its own from metadata).
    commit_subtitle_selection(&mut ps, 2, 10980, true);
    assert_eq!(enhancement_step(&ps), EnhancementStep::NotInvolved, "same route flavour, still a Burn");
    let (action, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert!(!ps.cur_contract.remux, "must still be a real re-encode after the re-pick");
    assert_eq!(ps.cur_contract.audio, PREF, "the DSP preference must survive the re-pick");

    done.send(()).unwrap();
    let requests = server.join().unwrap();
    let d = decisions(&requests);
    assert_eq!(d.len(), 2, "{requests:?}");
    assert_eq!(query_param(d[1], "normalizeLoudness"), Some("1"), "the re-pick keeps the enhancement: {}", d[1]);
    assert_eq!(query_param(d[1], "subtitleStreamID"), Some("10980"), "the re-pick must still burn: {}", d[1]);
    assert_eq!(query_param(d[1], "subtitles"), Some("burn"), "the re-pick must still burn: {}", d[1]);

    cleanup(&mut ps);
    crate::plex::reset_servers_for_test();
}

/// The FIRST device run's actual entry state (`/tmp/enh-burn-tv/logs/...log`, before its own PUT
/// persisted `sub=10980` and contaminated the retry): cold start has NO subtitle selected
/// (`select streams: ... sub=0`), so it lands on the ORDINARY enhanced remux — matching the
/// manifest's own description of this case. The live pick that follows is the FIRST time this
/// playback asks for the embedded subtitle at all, unlike the re-pick above.
#[test]
#[cfg(feature = "devtriggers")]
fn subtitle_first_pick_while_plain_enhanced_remux_burns_it() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::Auto);
    reset_player_control_for_test(&ps);
    crate::player::restore_audio_enhancements(PREF);

    let (port, done, server) = enhancement_pms(MDE_DIRECTPLAY, EnhMode::Honor("ac3"), 0);
    let sid = crate::plex::register_for_test("enh-firstpick", "127.0.0.1", port, "token", "enh-client");
    let client = crate::plex::client_for(sid).unwrap();
    client.set_link(crate::plex::probe::Location::Local);
    crate::plex::serverinfo::store_for_test(sid, Subscription::Yes, "1.43.4");

    let audio = crate::metadata::Stream {
        id: 10976,
        index: 1,
        lang_code: "eng".into(),
        codec: "ac3".into(),
        channels: 2,
        default: true,
        selected: true,
        can_normalize_loudness: true,
        ..Default::default()
    };
    // No track carries `selected: true` and no account/show subtitle preference is set, so
    // `pick_dp_subtitle_account` returns `None` — the cold start's own `sub=0`, same as the
    // device's first run.
    let sub = crate::metadata::Stream {
        id: 10980,
        index: 0,
        lang_code: "eng".into(),
        codec: "srt".into(),
        selected: false,
        ..Default::default()
    };
    let mut env = ResolveEnv::snapshot(&ps, crate::stores::metadata::MetadataStore::default().view(), sid, "1804");
    env.quality = Quality::Auto;
    env.pass = Subscription::Yes;
    env.audio_enhancements = PREF;
    env.cached_item = Some(fourk_item_with_subs(sid, vec![audio], vec![sub]));
    let plan = build_stream("1804", "/library/parts/3058/1/file.mkv", "hevc", "ac3", &env);
    assert!(plan.contract.remux, "cold start must be the ORDINARY enhanced remux, no subtitle yet");
    assert_eq!(query_param(&plan.url, "subtitleStreamID"), None, "{}", plan.url);
    assert_eq!(query_param(&plan.url, "normalizeLoudness"), Some("1"), "{}", plan.url);
    apply_plan(&mut ps, plan, "1804");
    assert!(is_transcoding(&ps), "cold start landed on a transcode");
    assert_eq!(ps.cur_sub_sid, 0);
    assert!(!live_is_own_burn(&ps), "cold start is a plain enhanced remux, not yet a Burn");

    // The FIRST-ever pick of the embedded subtitle (ordinal arbitrary; a burn never fills
    // `sub_render_ordinal`, so the menu computes its own from metadata).
    commit_subtitle_selection(&mut ps, 0, 10980, true);
    assert_eq!(
        enhancement_step(&ps),
        EnhancementStep::Remux(enhanced_remux_contract(PREF, true)),
        "an embedded pick while enhanced must force a Burn"
    );
    let (action, tail) = claim(&mut ps);
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    assert!(!ps.cur_contract.remux, "the pick must turn the plain remux into a real re-encode");
    assert_eq!(ps.cur_contract.audio, PREF, "the DSP preference must survive the pick");

    done.send(()).unwrap();
    let requests = server.join().unwrap();
    let d = decisions(&requests);
    assert_eq!(d.len(), 2, "{requests:?}");
    assert_eq!(query_param(d[1], "normalizeLoudness"), Some("1"), "the pick keeps the enhancement: {}", d[1]);
    assert_eq!(query_param(d[1], "subtitleStreamID"), Some("10980"), "the pick must burn: {}", d[1]);
    assert_eq!(query_param(d[1], "subtitles"), Some("burn"), "the pick must burn: {}", d[1]);

    cleanup(&mut ps);
    crate::plex::reset_servers_for_test();
}

// ---- PR review follow-up: off-main-thread claim PMS I/O, mailbox lifetime, snapshot isolation --

/// Finding: `execute_retranscode_claim`'s `primary_reject` match had collapsed
/// `ClaimPrimary::Legacy` into the same `RETRANSCODE_REJECTED` arm as `ClaimPrimary::Retranscode`
/// (pre-PR code had them as separate `match` arms — see `git show 4bf2bef27^`). A Legacy primary is
/// a displaced pick's own reload, not a plain user retranscode, so its refusal must keep
/// `LEGACY_REJECTED`. `ClaimPrimary::Legacy` is only reached through `claim_dispatch`'s
/// `(NotInvolved, displaced=true)` row, which the ordinary track/enhancement flows do not produce
/// on their own (a displaced pick is normally merged behind a `Remux`/`ReleaseToDirect` primary
/// instead — see `claim_dispatch_table_each_cell`), so this test builds the `ClaimedRouteAction`
/// directly rather than trying to walk the UI there.
#[test]
fn legacy_primary_rejection_keeps_the_legacy_rejected_label() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = always_refusing_live();
    // No offer (`auto_original: None`, preference off) makes `enhancement_step` `NotInvolved`;
    // TrueHD is not direct-playable, so `legacy_action` returns `Retranscode`.
    install(&mut ps, &live, Delivery::Direct, a4(), None, 0);
    assert_eq!(enhancement_step(&ps), EnhancementStep::NotInvolved);
    assert_eq!(legacy_action(&ps), LegacyAction::Retranscode);
    assert_eq!(
        claim_dispatch(EnhancementStep::NotInvolved, true),
        Dispatch { primary: ClaimPrimary::Legacy, on_failure: ClaimFallback::Reject },
    );
    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).phase = ControlPhase::Applying(1);
    let action = ClaimedRouteAction {
        serial: 1,
        ticket: worker_ticket(),
        intent: RouteIntent::User(UserRouteIntent::Retranscode),
        displaced_pick: true,
        claim_snapshot: None,
    };
    let dispatch = execute_retranscode_claim(&mut ps, &action, 60, -1, 0);
    let tail = match dispatch {
        RetranscodeClaimDispatch::Sync(tail) => tail,
        RetranscodeClaimDispatch::Pending => {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Some((_, tail, _, _)) = take_ready_retranscode_claim(&mut ps) {
                    break tail;
                }
                assert!(std::time::Instant::now() < deadline, "worker never landed");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    };
    assert_eq!(
        tail,
        ClaimTail::Rejected(LEGACY_REJECTED),
        "a Legacy primary's own reload must keep its own label, not the plain retranscode's",
    );
    live.finish();
    reset_player_control_for_test(&mut ps);
    reset_session(&mut ps);
    crate::player::restore_audio_enhancements(NONE);
    crate::player::reset_audio_track();
    crate::player::reset_subtitle();
}

/// Finding: `commit_audio_selection`/`commit_subtitle_selection` used to call `put_selection`
/// synchronously on whatever thread called them, including the frame thread reaching them from
/// `app/playback.rs::commit_track` inside the run-loop's own `FrameScope`. `assert_may_block`
/// panics in `cfg(test)` for any blocking PMS call made while a `FrameScope` is entered and the
/// call site has no `allow_blocking` guard — so this test alone would have panicked on
/// `4bf2bef27` (`main-thread block: select stream selection`), before `queue_put_selection` moved
/// the PUT onto a serial worker.
#[test]
fn commit_track_inside_a_frame_does_not_trip_the_blocking_guard() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    install(&mut ps, &live, Delivery::Direct, a5(), Some(candidate(true, a5(), None)), 0);
    let _frame = crate::task::FrameScope::enter();
    commit_audio_selection(&mut ps, a3()); // native pick: PUT must not run inline here
    drop(_frame);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let idle = !SELECTION_WORKER_RUNNING.load(Ordering::Acquire)
            && SELECTION_QUEUE.lock().unwrap_or_else(|e| e.into_inner()).is_empty();
        if idle {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "selection worker never drained");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let requests = live.finish();
    assert!(
        requests.iter().any(|r| r.starts_with("PUT") && query_param(r, "audioStreamID") == Some("13")),
        "the PUT must still really reach PMS, just off the frame thread: {requests:?}",
    );
    cleanup(&mut ps);
}

/// Finding: a worker panicking mid-`/decision` used to leave `ControlPhase::Applying` stuck
/// forever — nothing else could ever post to `RETRANSCODE_CLAIM_SLOT` for that serial, and
/// `claim_route_action` refuses a new claim while any `Applying` holds. `spawn_retranscode_claim`'s
/// `PostRejectedOnPanic` guard is what makes this test terminate at all: without it, the poll loop
/// below spins until its own deadline assertion fails, which is the "stuck forever" this proves is
/// fixed.
#[test]
fn a_worker_panic_still_releases_applying_instead_of_hanging_forever() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), None)), 0);
    assert!(toggle(&mut ps, PREF));
    let action = claim_route_action().expect("a queued user action");
    force_next_retranscode_worker_panic();
    let dispatch = execute_retranscode_claim(&mut ps, &action, 60, -1, 0);
    assert!(matches!(dispatch, RetranscodeClaimDispatch::Pending));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let (action, tail, ..) = loop {
        if let Some(landing) = take_ready_retranscode_claim(&mut ps) {
            break landing;
        }
        assert!(std::time::Instant::now() < deadline, "a panicked worker left Applying stuck");
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    assert_eq!(tail, ClaimTail::Rejected(RETRANSCODE_WORKER_PANICKED));
    settle(&mut ps, &action, tail);
    assert_eq!(phase(), ControlPhase::Stable, "the panic must still release the reducer");
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Off, "nothing reached PMS session state");

    // The reducer is usable again after the panic released it.
    assert!(toggle(&mut ps, PREF));
    let (action2, tail2) = claim(&mut ps);
    assert_eq!(tail2, ClaimTail::Retranscode);
    settle(&mut ps, &action2, tail2);
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Applied);

    live.finish();
    cleanup(&mut ps);
}

/// Finding: a second edit that queues while a claim's worker is still in flight advances
/// `desired_revision`/`ps` right away (nothing blocks the frame thread from taking it), so by the
/// time the FIRST claim's landing arrives, reading those live would record the landing as if it
/// had applied the SECOND edit's revision. `execute_retranscode_claim` now snapshots
/// revision/quality/projection at claim time (`ClaimSnapshot`) and `finish_route_action` publishes
/// that snapshot instead of re-reading `ps`. Before the fix, the assertion on `applied_revision`
/// below observed the SECOND toggle's revision after the FIRST claim settled.
#[test]
fn a_second_edit_queued_mid_flight_does_not_get_marked_as_the_first_claims_landing() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = slow_live(std::time::Duration::from_millis(150));
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), None)), 0);
    assert!(toggle(&mut ps, PREF));
    let first_revision = desired_contract_revision();
    let action = claim_route_action().expect("a queued user action");
    let dispatch = execute_retranscode_claim(&mut ps, &action, 60, -1, 0);
    assert!(matches!(dispatch, RetranscodeClaimDispatch::Pending));

    // A second edit crosses its own user-contract boundary (`begin_user_contract_boundary`, which
    // every track/quality setter calls up front regardless of `ControlPhase`) and so advances
    // `desired_revision` right away, while the first claim's worker is still waiting on the
    // deliberately slow `/decision` response — exactly the freeze fix's own window.
    commit_audio_selection(&mut ps, a3());
    let second_revision = desired_contract_revision();
    assert_ne!(first_revision, second_revision, "the second edit really did advance the contract");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let (action, tail, ..) = loop {
        if let Some(landing) = take_ready_retranscode_claim(&mut ps) {
            break landing;
        }
        assert!(std::time::Instant::now() < deadline, "first claim worker never landed");
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    assert_eq!(tail, ClaimTail::Retranscode, "the first claim's own attempt still applies");
    settle(&mut ps, &action, tail);
    assert_eq!(
        PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).applied_revision,
        first_revision,
        "the landing must publish what IT was built from, not whatever queued after it",
    );

    live.finish();
    cleanup(&mut ps);
}

/// Finding: `claim_snapshot.projection` was built from `ps` BEFORE the worker ran, and
/// `finish_route_action` published it as the applied projection — so after an ACCEPTED claim the
/// reducer's restore point still described the stream that was just replaced, and a later REJECTED
/// claim reinstalled the old direct-play url / tsession / enhancement over an engine that plays the
/// new stream (leaking its encoder).
#[test]
fn a_rejected_claim_after_an_accepted_one_restores_the_stream_that_plays() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), None)), 0);
    assert!(ps.tsession.is_empty(), "the fixture starts on a direct route");
    assert!(toggle(&mut ps, PREF));
    // The landing's OWN action, as the pump settles it: it carries the claim-time snapshot that
    // `claim()` (which settles the original, snapshot-less action) never exercises.
    let claimed = claim_route_action().expect("a queued user action");
    assert!(matches!(
        execute_retranscode_claim(&mut ps, &claimed, 60, -1, 0),
        RetranscodeClaimDispatch::Pending
    ));
    let mut landed = None;
    wait_until("the worker's landing", || {
        landed = take_ready_retranscode_claim(&mut ps);
        landed.is_some()
    });
    let (action, tail, ..) = landed.unwrap();
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    let (live_url, live_tsession) = (ps.url.clone(), ps.tsession.clone());
    assert!(!live_tsession.is_empty(), "the accepted claim installed the new encoder");
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Applied);

    // The reload started the new Engine: the reducer is Stable again. A later claim is REJECTED.
    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).phase = ControlPhase::Stable;
    request_user_route_intent(&ps, UserRouteIntent::Retranscode);
    let second = claim_route_action().expect("a queued user action");
    assert!(finish_route_action(&mut ps, &second, RouteApplyResult::Rejected));

    assert_eq!(ps.tsession, live_tsession, "a rejection restored the pre-claim encoder session");
    assert_eq!(ps.url, live_url, "a rejection restored the pre-claim url");
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Applied, "the enhancement that plays was un-applied");
    assert_eq!(ps.cur_contract.audio, PREF);
    assert_eq!(ps.cur_audio.as_ref().map(|a| a.sid), Some(a1().sid));

    live.finish();
    cleanup(&mut ps);
}

/// Finding: the NativeAudio fallback landing changes `ps.stream_acodec` (`stage_native_audio`)
/// AFTER the claim snapshot was captured, so `finish_route_action` published the OLD codec as the
/// applied projection and a later rejected claim restored the wrong `stream_acodec` over an Engine
/// playing the picked track.
#[test]
fn a_rejected_claim_after_a_native_audio_landing_restores_the_codec_that_plays() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Refuse);
    crate::player::restore_audio_enhancements(PREF);
    install(&mut ps, &live, Delivery::Direct, a5(), Some(candidate(true, a5(), None)), 0);
    assert_eq!(ps.stream_acodec, "ac3", "the fixture starts on the AC3 track");
    commit_audio_selection(&mut ps, a2());
    let claimed = claim_route_action().expect("a queued user action");
    assert!(matches!(
        execute_retranscode_claim(&mut ps, &claimed, 60, -1, 0),
        RetranscodeClaimDispatch::Pending
    ));
    let mut landed = None;
    wait_until("the worker's landing", || {
        landed = take_ready_retranscode_claim(&mut ps);
        landed.is_some()
    });
    let (action, tail, ..) = landed.unwrap();
    assert_eq!(tail, ClaimTail::NativeAudio, "the primary was refused and the pick runs natively");
    assert_eq!(ps.stream_acodec, "eac3");
    settle(&mut ps, &action, tail);

    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).phase = ControlPhase::Stable;
    request_user_route_intent(&ps, UserRouteIntent::Retranscode);
    let second = claim_route_action().expect("a queued user action");
    assert!(finish_route_action(&mut ps, &second, RouteApplyResult::Rejected));

    assert_eq!(ps.stream_acodec, "eac3", "a rejection restored the pre-pick codec over the Engine that plays the pick");

    live.finish();
    cleanup(&mut ps);
}

/// Finding: the old encoder was stopped inside the worker's attempt, seconds before the landing
/// reloads — the engine (paused at the claim offset) was still reading a stream PMS had just
/// killed. It must stay alive until the landing is installed, and be stopped only after.
#[test]
fn the_replaced_encoder_stays_alive_until_the_landing_is_installed() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (live, log) = slow_live_logged(std::time::Duration::from_millis(20));
    install(&mut ps, &live, Delivery::Remux(NONE), a1(), Some(candidate(true, a1(), None)), 0);
    assert_eq!(worker_ticket().encoder(), "enh-remux-1", "the fixture's playing encoder");
    assert!(toggle(&mut ps, PREF));
    let action = claim_route_action().expect("a queued user action");
    assert!(matches!(
        execute_retranscode_claim(&mut ps, &action, 60, -1, 0),
        RetranscodeClaimDispatch::Pending
    ));
    wait_until("the worker's landing", landing_posted);
    assert!(
        !stop_seen(&log, "enh-remux-1"),
        "the playing stream's encoder was stopped before its replacement landed: {:?}",
        log.lock().unwrap_or_else(|e| e.into_inner()),
    );

    let (action, tail, ..) = take_ready_retranscode_claim(&mut ps).expect("the landing");
    assert_eq!(tail, ClaimTail::Retranscode);
    settle(&mut ps, &action, tail);
    // The pump reloads onto the new stream, THEN retires the one it replaced.
    retire_superseded_encoder();
    wait_until("the replaced encoder's stop", || stop_seen(&log, "enh-remux-1"));

    live.finish();
    cleanup(&mut ps);
}

/// Finding: an engine failure (`load_failed` / `demux_io_failed` / `demux_failed`) while a claim's
/// worker owned `Applying` was only RECORDED (`deferred_engine_failure`) for `finish_route_action`
/// to publish — but the pump returns on the failure before it ever drains the landing, so the phase
/// stayed `Applying`, `claim_hold::active()` stayed true and `state()` drew the Buffering spinner
/// forever instead of the error read-out.
#[test]
fn an_engine_failure_during_the_flight_ends_in_error_with_no_spinner() {
    use crate::player::PlaybackState;
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (live, log) = slow_live_logged(std::time::Duration::from_millis(80));
    install(&mut ps, &live, Delivery::Remux(NONE), a1(), Some(candidate(true, a1(), None)), 0);
    assert!(toggle(&mut ps, PREF));
    let action = claim_route_action().expect("a queued user action");
    assert!(matches!(
        execute_retranscode_claim(&mut ps, &action, 60, -1, 0),
        RetranscodeClaimDispatch::Pending
    ));
    crate::player::claim_hold::hold_for_test(action.serial(), true);
    assert!(crate::player::claim_hold::active(), "during the flight the spinner is up");

    // What the pump does on a terminal failure.
    fail_current_engine();
    crate::player::SHARED.pb_state.store(PlaybackState::Error as u8, std::sync::atomic::Ordering::Relaxed);

    assert!(matches!(phase(), ControlPhase::Failed(_)), "the failure was hidden behind Applying: {:?}", phase());
    assert!(!crate::player::claim_hold::active(), "the hold's spinner outlived the failure");
    assert_eq!(crate::player::state(&ps), PlaybackState::Error);

    // The worker lands late: the drain discards it (the pump never reaches the drain after a
    // failure, so the teardown's `discard_retranscode_claim_slot` is what would find it) and the
    // encoder it started is stopped rather than leaked.
    wait_until("the worker's landing", landing_posted);
    let before = ps.tsession.clone();
    assert!(take_ready_retranscode_claim(&mut ps).is_none(), "a landing for a failed engine was applied");
    assert_eq!(ps.tsession, before);
    // Whatever encoder the worker managed to start before the failure revoked its ticket is
    // stopped, not leaked (a worker that noticed in time never asked for one).
    let asked_for_an_encoder = log
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|r| r.contains("/decision?") && !r.contains("hasMDE=1"));
    if asked_for_an_encoder {
        wait_until("the failed claim's encoder to be stopped", || any_stop_seen(&log));
    }

    crate::player::SHARED.pb_state.store(PlaybackState::Idle as u8, std::sync::atomic::Ordering::Relaxed);
    crate::player::claim_hold::clear();
    live.finish();
    cleanup(&mut ps);
}

/// Finding: the mailbox used to survive a full teardown/fresh playback request untouched, so a
/// slow claim's landing from the OLD item could apply to whatever plays next. `begin_playback_request`
/// now discards any landing still sitting in the slot before it does anything else.
#[test]
fn a_landing_from_a_torn_down_item_never_reaches_the_next_playback() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (live, log) = slow_live_logged(std::time::Duration::from_millis(150));
    install(&mut ps, &live, Delivery::Direct, a1(), Some(candidate(true, a1(), None)), 0);
    assert!(toggle(&mut ps, PREF));
    let action = claim_route_action().expect("a queued user action");
    let dispatch = execute_retranscode_claim(&mut ps, &action, 60, -1, 0);
    assert!(matches!(dispatch, RetranscodeClaimDispatch::Pending));

    // Wait for the worker to actually post its landing before tearing the item down, so this
    // exercises the mailbox-clearing itself rather than a claim that never got the chance to run.
    wait_until("the worker's landing", landing_posted);

    // Back, then a fresh playback request for a different item. Both run on the frame thread, so
    // the encoder stop the discard owes the landing must not block it (`FrameScope` panics a
    // blocking call outside `allow_blocking`).
    {
        let _frame = crate::task::FrameScope::enter();
        begin_engine_teardown(false);
        assert!(begin_playback_request());
    }
    assert!(
        RETRANSCODE_CLAIM_SLOT.lock().unwrap_or_else(|e| e.into_inner()).is_none(),
        "the old item's landing must not survive into the new playback",
    );
    assert!(
        take_ready_retranscode_claim(&mut ps).is_none(),
        "nothing is left to apply to the new session",
    );
    wait_until("the discarded landing's encoder to be stopped", || any_stop_seen(&log));

    live.finish();
    reset_player_control_for_test(&mut ps);
    reset_session(&mut ps);
    crate::player::restore_audio_enhancements(NONE);
    crate::player::reset_audio_track();
    crate::player::reset_subtitle();
}

/// The two stale arms of `take_ready_retranscode_claim` (the claim's own phase moved on; the
/// ticket went stale in the drain gap) both stop the encoder session the worker started. They run
/// on the frame thread, where a synchronous `transcode_stop` is a blocking PMS call.
#[test]
fn the_stale_arms_of_the_drain_stop_their_encoder_off_the_frame_thread() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (live, log) = slow_live_logged(std::time::Duration::ZERO);
    install(&mut ps, &live, Delivery::Direct, a1(), None, 0);
    let client = cur_client(&ps).expect("install() registered this session's server");
    let landing = |qsess: &str, ticket: WorkerTicket, serial: u64| RetranscodeClaimLanding {
        action: ClaimedRouteAction {
            serial,
            ticket: ticket.clone(),
            intent: RouteIntent::User(UserRouteIntent::Retranscode),
            displaced_pick: false,
            claim_snapshot: None,
        },
        pending_seek: -1,
        user_target: 0,
        result: RetranscodeClaimResult::Retranscode {
            qsess: qsess.to_owned(),
            url: String::new(),
            vcodec: "hevc".into(),
            acodec: "ac3".into(),
            contract: crate::plex::EncodeContract::default(),
            enhancement: EnhancementOutcome::Off,
            ticket,
            client,
            superseded: String::new(),
        },
    };

    // Arm 1: the phase is no longer `Applying(serial)` (a teardown/new request superseded it).
    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).phase = ControlPhase::Stable;
    *RETRANSCODE_CLAIM_SLOT.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(landing("stale-phase-session", worker_ticket(), 1));
    {
        let _frame = crate::task::FrameScope::enter();
        assert!(take_ready_retranscode_claim(&mut ps).is_none());
    }
    wait_until("the stale-phase encoder stop", || stop_seen(&log, "stale-phase-session"));

    // Arm 2: the claim still owns the phase but its ticket went stale in the drain gap.
    let starting = worker_ticket();
    let stale = replace_active_encoder_for(&starting, "stale-ticket-session").expect("commit");
    replace_active_encoder_for(&stale, "concurrent-abr-session").expect("concurrent commit");
    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).phase = ControlPhase::Applying(2);
    *RETRANSCODE_CLAIM_SLOT.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(landing("stale-ticket-session", stale, 2));
    {
        let _frame = crate::task::FrameScope::enter();
        let (_, tail, ..) = take_ready_retranscode_claim(&mut ps).expect("the landing posted above");
        assert_eq!(tail, ClaimTail::Rejected(RETRANSCODE_REJECTED));
    }
    wait_until("the stale-ticket encoder stop", || stop_seen(&log, "stale-ticket-session"));

    live.finish();
    cleanup(&mut ps);
}

/// Finding: `transcode_seek`/`commit_user_seek` used to run for an unrelated claim's own seek
/// branches while `ControlPhase::Applying` was held by a DIFFERENT in-flight claim — replacing the
/// encoder or bumping `media_epoch` out from under the worker's own ticket, so the worker's
/// landing silently discarded even though nothing actually superseded it. The pump now skips its
/// seek branches entirely while `claim_in_flight()` holds; `transcode_seek` also refuses directly.
/// This test exercises `transcode_seek` itself: called during another claim's flight it must
/// return `None` rather than proceeding, which on `4bf2bef27` it did not.
#[test]
fn transcode_seek_refuses_while_an_unrelated_claim_is_in_flight() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = slow_live(std::time::Duration::from_millis(150));
    // A LIVE transcode (an enhanced remux): `transcode_seek` returns `None` early for a Direct route, which would
    // make the assertion below pass without the `claim_in_flight()` refusal ever being consulted.
    install(&mut ps, &live, Delivery::Remux(NONE), a1(), Some(candidate(true, a1(), None)), 0);
    assert!(toggle(&mut ps, PREF)); // enhance: a claim whose worker will hold `Applying`
    let action = claim_route_action().expect("a queued user action");
    let dispatch = execute_retranscode_claim(&mut ps, &action, 60, -1, 0);
    assert!(matches!(dispatch, RetranscodeClaimDispatch::Pending));
    assert!(claim_in_flight(), "the claim above is still Applying");

    assert_eq!(
        transcode_seek(&mut ps, 90),
        None,
        "a seek reaching in during another claim's flight must be refused, not silently corrupt it",
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let (action, tail, ..) = loop {
        if let Some(landing) = take_ready_retranscode_claim(&mut ps) {
            break landing;
        }
        assert!(std::time::Instant::now() < deadline, "claim worker never landed");
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    assert_eq!(tail, ClaimTail::Retranscode, "the seek attempt above must not have disturbed it");
    settle(&mut ps, &action, tail);
    assert_eq!(ps.cur_enhancement, EnhancementOutcome::Applied);

    live.finish();
    cleanup(&mut ps);
}

/// Finding 2's narrower race: `try_retranscode`'s own `replace_active_encoder_for` check runs
/// (and commits) entirely inside the worker, before the landing ever reaches
/// `RETRANSCODE_CLAIM_SLOT` — but a same-item route change (a concurrent ABR commit, modelled
/// directly here with a second `replace_active_encoder_for` call) can still land in the window
/// between that worker-side commit and `take_ready_retranscode_claim`'s later drain on the main
/// thread. Unlike `claim_ticket_invalidated_while_worker_in_flight_is_discarded_then_a_fresh_pick_applies`
/// (which invalidates the ticket BEFORE the worker's own commit, so `try_retranscode` itself
/// refuses), this constructs a landing that already reflects a successful `Applied` outcome and
/// checks the drain-time re-check at `take_ready_retranscode_claim`'s own `is_worker_ticket_current`
/// arm: the stale landing must not install its session projection, and the encoder session it
/// already started on the server (`qsess`) must be stopped rather than left running unowned.
#[test]
fn a_stale_landing_at_drain_time_stops_the_leaked_encoder_instead_of_installing_it() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    install(&mut ps, &live, Delivery::Direct, a1(), None, 0);

    // The worker's own commit inside `try_retranscode` (`replace_active_encoder_for`), captured
    // directly rather than through a real worker so the race is deterministic instead of timing-
    // dependent.
    let starting_ticket = worker_ticket();
    let leaked_ticket = replace_active_encoder_for(&starting_ticket, "leaked-worker-session")
        .expect("the worker's own commit-time check must still pass against a fresh ticket");

    // The concurrent route event that lands in the gap before the drain — an ABR commit or a
    // fresh claim, anything that moves `control.active` again while this landing is still in
    // flight to the mailbox.
    replace_active_encoder_for(&leaked_ticket, "concurrent-abr-session")
        .expect("the concurrent commit itself must succeed against the ticket the worker left");

    let action = ClaimedRouteAction {
        serial: 1,
        ticket: starting_ticket,
        intent: RouteIntent::User(UserRouteIntent::Retranscode),
        displaced_pick: false,
        claim_snapshot: None,
    };
    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).phase = ControlPhase::Applying(action.serial);
    *RETRANSCODE_CLAIM_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(RetranscodeClaimLanding {
        action: action.clone(),
        pending_seek: -1,
        user_target: 0,
        result: RetranscodeClaimResult::Retranscode {
            qsess: "leaked-worker-session".to_string(),
            url: format!(
                "http://127.0.0.1:{}/video/:/transcode/universal/start.mkv?session=leaked-worker-session",
                live.port,
            ),
            vcodec: "hevc".into(),
            acodec: "ac3".into(),
            contract: crate::plex::EncodeContract::default(),
            enhancement: EnhancementOutcome::Off,
            ticket: leaked_ticket,
            client: cur_client(&ps).expect("install() registered this session's server"),
            superseded: String::new(),
        },
    });

    let before_tsession = ps.tsession.clone();
    let (_, tail, ..) = take_ready_retranscode_claim(&mut ps).expect("the landing posted above");
    assert_eq!(
        tail,
        ClaimTail::Rejected(RETRANSCODE_REJECTED),
        "a landing whose ticket went stale in the drain gap must be rejected, not installed",
    );
    assert_eq!(
        ps.tsession, before_tsession,
        "the stale landing must never write its session projection over the live route",
    );

    let requests = live.finish();
    assert!(
        requests
            .iter()
            .any(|r| r.contains("/video/:/transcode/universal/stop") && r.contains("session=leaked-worker-session")),
        "the leaked encoder session must be stopped, not left running unowned on the server: {requests:?}",
    );
    cleanup(&mut ps);
}

/// `commit_audio_selection` is also reached off the frame thread's own `FrameScope` through
/// [`settle_route_start`] → [`apply_deferred_original_effects`] (an audio pick made while a native
/// Original trial or an ordinary route restart was still `Starting` gets deferred and replayed
/// once the attempt settles — the same replay path `confirm_original_recovery` uses). Unlike
/// `commit_track_inside_a_frame_does_not_trip_the_blocking_guard`, which calls
/// `commit_audio_selection` directly, this drives it through the deferred-effects replay so the
/// coverage is of the actual call site, not just the function it eventually reaches.
#[test]
fn a_deferred_audio_pick_replayed_from_settle_route_start_does_not_trip_the_blocking_guard() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let live = Live::start(EnhMode::Honor("ac3"));
    install(&mut ps, &live, Delivery::Direct, a5(), Some(candidate(true, a5(), None)), 0);

    let transaction = begin_route_start().expect("route start transaction");
    assert!(prepare_route_start(transaction));
    let attempt = claim_route_start_attempt(transaction).expect("Load attempt");
    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).start_deferred = Some((
        attempt.serial,
        DeferredOriginalEffects {
            quality: None,
            audio: Some(a3()),
            reconcile: false,
        },
    ));

    let _frame = crate::task::FrameScope::enter();
    assert!(settle_route_start(&mut ps, attempt, RouteStartResult::Started));
    drop(_frame);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let idle = !SELECTION_WORKER_RUNNING.load(Ordering::Acquire)
            && SELECTION_QUEUE.lock().unwrap_or_else(|e| e.into_inner()).is_empty();
        if idle {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "selection worker never drained");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let requests = live.finish();
    assert!(
        requests.iter().any(|r| r.starts_with("PUT") && query_param(r, "audioStreamID") == Some("13")),
        "the deferred pick's PUT must still reach PMS, off the frame thread: {requests:?}",
    );
    cleanup(&mut ps);
}
