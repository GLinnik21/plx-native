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
    calls: Vec<(&'static str, RequestId)>,
    replies: VecDeque<UriReply>,
}
struct Bus(Mutex<BusState>);
impl Bus {
    fn new() -> Self { Self(Mutex::new(BusState { calls: Vec::new(), replies: VecDeque::new() })) }
    fn say(&self, method: &'static str, payload: &str) {
        let mut state = self.0.lock().unwrap();
        let request = if method == "register" { RequestId::new(0) } else {
            state.calls.iter().rev().find(|(name, _)| *name == method)
                .map(|(_, request)| *request).expect("method was called")
        };
        state.replies.push_back(UriReply { session: SessionId::new(1), request, method,
            payload: payload.into(), ok: true });
    }
    fn calls(&self, method: &str) -> usize {
        self.0.lock().unwrap().calls.iter().filter(|(name, _)| *name == method).count()
    }
}
impl UriMedia for Bus {
    fn open(&self, _shape: Shape) -> Result<SessionId, String> { Ok(SessionId::new(1)) }
    fn call(&self, _session: SessionId, request: RequestId, method: &'static str,
        _payload: &str) -> Result<(), String> {
        self.0.lock().unwrap().calls.push((method, request));
        Ok(())
    }
    fn poll(&self, _session: SessionId) -> Result<UriPoll, String> {
        Ok(UriPoll { replies: self.0.lock().unwrap().replies.drain(..).collect(),
            close: CloseStatus::Open })
    }
    fn close(&self, _session: SessionId) -> Result<(), String> { Ok(()) }
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
