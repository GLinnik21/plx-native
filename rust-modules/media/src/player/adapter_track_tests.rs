//! Adapter integration tests for source-selected URI controls and route projection.
use super::*;
use std::collections::VecDeque;
use std::sync::Mutex;
use plx_platform::tv::uri::{CloseStatus, RequestId, SessionId, Shape, UriMedia, UriPoll, UriReply};
use plx_play_uri::backend::NativeTrackIndex;
use plx_playback_contract::{Fidelity, InitialPlayback, Requirements, SourceKind, TrackChoice,
    TrackKind, TrackSelection};

struct Clock;
impl MonotonicClock for Clock { fn now_ms(&self) -> u64 { 0 } }

struct BusState {
    session: SessionId,
    serial: u64,
    calls: Vec<(&'static str, RequestId)>,
    replies: VecDeque<UriReply>,
    close: CloseStatus,
    hold_close: bool,
}
struct Bus(Mutex<BusState>);
impl Bus {
    fn new() -> Self { Self(Mutex::new(BusState { session: SessionId::new(0), serial: 0,
        calls: Vec::new(), replies: VecDeque::new(), close: CloseStatus::Released,
        hold_close: false })) }
    fn say(&self, method: &'static str, payload: &str) {
        let mut state = self.0.lock().unwrap();
        let request = if method == "register" { RequestId::new(0) } else {
            state.calls.iter().rev().find(|(name, _)| *name == method)
                .map(|(_, request)| *request).expect("method was called")
        };
        let session = state.session;
        state.replies.push_back(UriReply { session, request, method,
            payload: payload.into(), ok: true });
    }
    fn calls(&self, method: &str) -> usize {
        self.0.lock().unwrap().calls.iter().filter(|(name, _)| *name == method).count()
    }
    fn hold_close(&self) { self.0.lock().unwrap().hold_close = true; }
    fn release(&self) { self.0.lock().unwrap().close = CloseStatus::Released; }
}
impl UriMedia for Bus {
    fn open(&self, _shape: Shape) -> Result<SessionId, String> {
        let mut state = self.0.lock().unwrap();
        state.serial += 1;
        state.session = SessionId::new(state.serial);
        state.close = CloseStatus::Open;
        Ok(state.session)
    }
    fn call(&self, _session: SessionId, request: RequestId, method: &'static str,
        _payload: &str) -> Result<(), String> {
        self.0.lock().unwrap().calls.push((method, request));
        Ok(())
    }
    fn poll(&self, _session: SessionId) -> Result<UriPoll, String> {
        let mut state = self.0.lock().unwrap();
        Ok(UriPoll { replies: state.replies.drain(..).collect(), close: state.close })
    }
    fn close(&self, _session: SessionId) -> Result<(), String> {
        let mut state = self.0.lock().unwrap();
        state.close = if state.hold_close { CloseStatus::Closing } else { CloseStatus::Released };
        Ok(())
    }
}

fn opened() -> (crate::route::PlaybackSession, PlayerAdapter, &'static Bus,
    crate::route::RouteStartAttempt) {
    let mut ps = crate::route::PlaybackSession::IDLE;
    crate::route::reset_player_control_for_test(&ps);
    let route_attempt = crate::route::RouteStartAttempt::fixture();
    ps.begin_attempt_view(route_attempt, super::super::attempt_view::Backend::Uri);
    let bus = Box::leak(Box::new(Bus::new()));
    let mut pa = PlayerAdapter::new(unsafe { MainThread::assume() });
    assert!(pa.install_uri_backend_config(UriConfig {
        transport: bus, display: Box::leak(Box::new(plx_platform::tv::sink::NoSink)),
        clock: Box::new(Clock), app_id: "fixture".into(), binding: UriBinding::Unbound,
        capabilities: Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
            pause: true, seek: true, audio_track: true, subtitle_track: true,
            audio_off: false, subtitle_off: false },
        startup_timeout_ms: 500, release_allowance_ms: 100, source_ttl_ms: 100,
    }).is_ok());
    let attempt = route_attempt.contract_id();
    let source = pa.stage_uri_source(attempt, UriSource::new(
        "https://example.invalid/selected.mkv".into(), vec![
            NativeTrackIndex { selection: TrackSelection { kind: TrackKind::Audio, id: 42 }, index: 3 },
            NativeTrackIndex { selection: TrackSelection { kind: TrackKind::Audio, id: 43 }, index: 4 },
            NativeTrackIndex { selection: TrackSelection { kind: TrackKind::Subtitle, id: 77 }, index: 5 },
        ])).expect("staged source");
    let requirements = Requirements { source: SourceKind::ProgressiveUri,
        video: Fidelity::Exact, audio: Fidelity::Exact,
        audio_track: TrackChoice::Unspecified, subtitle_track: TrackChoice::Unspecified };
    let at = pa.backend_tick();
    let started = pa.backend_send_at(Intent::Start { backend: BackendKind::Uri, attempt, source,
        requirements, initial: InitialPlayback { position: None, paused: true } },
        super::super::attempt_metrics::Origin::Fresh, at);
    assert!(!started.playback.iter().any(|fx| matches!(fx,
        plx_playback_contract::Fx::Rejected { .. })));
    pa.bind_uri_attempt(route_attempt);
    bus.say("register", "");
    let at = pa.backend_tick();
    let _ = pa.backend_poll_session_at(&mut ps, at);
    bus.say("load", r#"{"returnValue":true,"mediaId":"m1","loadCompleted":true}"#);
    let at = pa.backend_tick();
    let _ = pa.backend_poll_session_at(&mut ps, at);
    assert_eq!(pa.backend_snapshot().phase, plx_playback_contract::Phase::Active);
    (ps, pa, bus, route_attempt)
}

#[test]
fn external_sidecar_with_positive_native_id_never_dispatches_a_uri_track() {
    let _serial = plx_base::testlock::serial();
    let (mut ps, mut pa, bus, _) = opened();
    assert!(!pa.commit_subtitle_track(&mut ps, 2, 77,
        Some("/library/parts/sidecar".into()), "srt".into(), None));
    assert_eq!(bus.calls("selectTrack"), 0);
    assert_eq!(crate::route::cur_sub_sid(&ps), 0);
    assert!(pa.pending_uri_subtitle.is_none());
    crate::route::reset_player_control_for_test(&ps);
}

#[test]
fn queued_and_acknowledged_uri_picks_keep_the_prior_route_until_exact_confirmation() {
    let _serial = plx_base::testlock::serial();
    let (mut ps, mut pa, bus, _) = opened();
    assert!(pa.commit_audio_track(&mut ps, crate::route::CarriedAudio::named(42, 3), None));
    let first = pa.pending_uri_audio.as_ref().expect("queued audio command").command;
    assert_eq!(bus.calls("selectTrack"), 1);
    assert_eq!(crate::route::cur_audio_sid(&ps), 0, "queue success is not selection");

    // A second same-kind request is refused while the first native request is unresolved.
    assert!(!pa.commit_audio_track(&mut ps, crate::route::CarriedAudio::named(43, 4), None));
    assert_eq!(pa.pending_uri_audio.as_ref().map(|pick| pick.command), Some(first));
    assert_eq!(crate::route::cur_audio_sid(&ps), 0);

    bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
    let at = pa.backend_tick();
    let _ = pa.backend_poll_session_at(&mut ps, at);
    assert_eq!(crate::route::cur_audio_sid(&ps), 0, "native ACK is not selected-track evidence");
    bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":3}}"#);
    let at = pa.backend_tick();
    let _ = pa.backend_poll_session_at(&mut ps, at);
    assert_eq!(crate::route::cur_audio_sid(&ps), 42);
    assert!(pa.pending_uri_audio.is_none());

    assert!(pa.commit_audio_track(&mut ps, crate::route::CarriedAudio::named(43, 4), None));
    let second = pa.pending_uri_audio.as_ref().expect("successor command").command;
    assert_ne!(first, second);
    bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":3}}"#);
    let at = pa.backend_tick();
    let _ = pa.backend_poll_session_at(&mut ps, at);
    assert_eq!(crate::route::cur_audio_sid(&ps), 42,
        "late old selection cannot commit the pending successor");
    bus.say("selectTrack", r#"{"returnValue":false,"mediaId":"m1"}"#);
    let at = pa.backend_tick();
    let _ = pa.backend_poll_session_at(&mut ps, at);
    assert!(pa.pending_uri_audio.is_none(), "exact refusal retires only the pending command");
    assert_eq!(crate::route::cur_audio_sid(&ps), 42);
    assert!(pa.commit_subtitle_track(&mut ps, 5, 77, None, String::new(), None),
        "a later interactive refusal cannot poison another selected track");
    bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"text","index":5}}"#);
    let at = pa.backend_tick();
    let _ = pa.backend_poll_session_at(&mut ps, at);
    bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
    let at = pa.backend_tick();
    let _ = pa.backend_poll_session_at(&mut ps, at);
    assert_eq!(crate::route::cur_sub_sid(&ps), 77);
    let at = pa.backend_tick();
    let attempt = pa.active_uri_attempt().expect("URI attempt").contract_id();
    let resumed = pa.backend_send_at(Intent::Control { backend: BackendKind::Uri,
        attempt,
        operation: Operation::Play }, super::super::attempt_metrics::Origin::Fresh, at);
    assert!(!resumed.playback.iter().any(|fx| matches!(fx,
        plx_playback_contract::Fx::Rejected { .. } | plx_playback_contract::Fx::CommandRefused { .. })));
    assert_eq!(bus.calls("play"), 1, "the paused viewer may resume after an interactive refusal");
    crate::route::reset_player_control_for_test(&ps);
}

#[test]
fn uri_subtitle_pick_commits_only_after_exact_ack_and_selected_report() {
    let _serial = plx_base::testlock::serial();
    let (mut ps, mut pa, bus, _) = opened();
    assert!(pa.commit_subtitle_track(&mut ps, 5, 77, None, String::new(), None));
    assert_eq!(crate::route::cur_sub_sid(&ps), 0);
    bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"text","index":5}}"#);
    let at = pa.backend_tick();
    let _ = pa.backend_poll_session_at(&mut ps, at);
    assert_eq!(crate::route::cur_sub_sid(&ps), 0, "uncorrelated native report is not a commit");
    bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
    let at = pa.backend_tick();
    let _ = pa.backend_poll_session_at(&mut ps, at);
    assert_eq!(crate::route::cur_sub_sid(&ps), 77);
    crate::route::reset_player_control_for_test(&ps);
}

#[test]
fn a_qualified_uri_successor_waits_for_the_prior_registration_to_release() {
    let _serial = plx_base::testlock::serial();
    let (mut ps, mut pa, bus, first) = opened();
    bus.hold_close();
    let at = pa.backend_tick();
    let _ = pa.backend_send_at(Intent::Stop { backend: BackendKind::Uri,
        attempt: first.contract_id(), reason: plx_playback_contract::StopReason::Replace },
        super::super::attempt_metrics::Origin::Fresh, at);
    let at = pa.backend_tick();
    let _ = pa.backend_poll_session_at(&mut ps, at);
    assert!(pa.release_busy());

    crate::route::set_url(&mut ps, "https://example.invalid/successor.mkv");
    let ticket = crate::route::begin_route_start().expect("successor route transaction");
    let offer = QualifiedUriOffer::new(
        crate::route::backend_policy::OfferId::URI_ORIGINAL, ticket, ps.uri_selection(),
        UriSource::new("https://example.invalid/successor.mkv".into(), Vec::new()),
        Requirements { source: SourceKind::ProgressiveUri,
            video: Fidelity::Exact, audio: Fidelity::Exact,
            audio_track: TrackChoice::Unspecified, subtitle_track: TrackChoice::Unspecified },
        InitialPlayback { position: None, paused: true }, 60_000,
        std::time::Instant::now() + std::time::Duration::from_secs(5))
        .expect("exact qualification");
    assert!(pa.offer_qualified_uri(&mut ps, offer).is_ok());
    assert_eq!(super::super::route_start::start_bufferfeed_for(&mut ps, &mut pa, ticket),
        super::super::route_start::BufferfeedStartOutcome::Deferred);
    pa.watch_queued_start();
    assert_eq!(bus.0.lock().unwrap().serial, 1,
        "no second native registration while closing");
    assert!(pa.qualified_uri.is_some(), "the pending URL stays in one-shot escrow");

    bus.release();
    let at = pa.backend_tick();
    let _ = pa.backend_poll_session_at(&mut ps, at);
    assert!(matches!(pa.take_queued_landing(), QueuedLanding::Launched(_)));
    assert!(pa.qualified_uri.is_none(), "the exact Start consumed its authorization once");
    assert_eq!(bus.0.lock().unwrap().serial, 2,
        "exact release permits one successor native registration");
    crate::route::reset_player_control_for_test(&ps);
}
