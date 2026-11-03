//! Product URI boot and resolve regressions using the app lifecycle loopback rig.

#[cfg(feature = "play-uri")]
mod uri_boot_tests {
    use super::*;
    use plx_media::route::uri_preflight::{KnownSource, OriginalQualifier, OriginalRequest,
        ProductEvidence, ResolvePort, Verdict};
    use plx_play_uri::backend::{Binding, NativeTrackIndex, UriSource};
    use plx_playback_contract::{BackendKind, Capabilities, Fidelity, Requirements,
        SourceKind, TrackChoice, TrackKind, TrackSelection};
    use plx_platform::tv::sink::{DisplaySink, NoSink, VP_EXPORTED};
    use plx_platform::tv::uri::{CloseStatus, RequestId, SessionId, Shape, UriMedia,
        UriPoll, UriReply};
    use std::ffi::{c_char, c_int, c_long, CStr};

    struct QualifiedOriginal;
    impl OriginalQualifier for QualifiedOriginal {
        fn qualify(&self, _: &plx_base::task::OffFrame, request: OriginalRequest) -> Verdict {
            let audio = u64::try_from(request.selection.audio_sid).ok()
                .filter(|id| *id > 0).map(TrackChoice::Selected)
                .unwrap_or(TrackChoice::Unspecified);
            let tracks = match audio {
                TrackChoice::Selected(id) => vec![NativeTrackIndex {
                    selection: TrackSelection { kind: TrackKind::Audio, id }, index: 0,
                }],
                _ => Vec::new(),
            };
            Verdict::Known(KnownSource {
                selection: request.selection,
                source: UriSource::new(request.source_url, tracks),
                requirements: Requirements { source: SourceKind::ProgressiveUri,
                    video: Fidelity::Exact, audio: Fidelity::Exact,
                    audio_track: audio, subtitle_track: TrackChoice::Unspecified },
                expires_at: std::time::Instant::now() + Duration::from_secs(30),
                certified_dolby_vision: None,
                certified_subtitle_source: None,
            })
        }
    }

    struct Bus(Mutex<(bool, CloseStatus, Vec<(&'static str, String)>)>);
    impl Bus {
        fn new() -> Self { Self(Mutex::new((false, CloseStatus::Open, Vec::new()))) }
        fn load_payload(&self) -> Option<String> {
            self.0.lock().unwrap().2.iter().find(|(method, _)| *method == "load")
                .map(|(_, payload)| payload.clone())
        }
    }
    impl UriMedia for Bus {
        fn open(&self, _: Shape) -> Result<SessionId, String> {
            self.0.lock().unwrap().0 = true;
            Ok(SessionId::new(1))
        }
        fn call(&self, _: SessionId, _: RequestId, method: &'static str,
            payload: &str) -> Result<(), String> {
            self.0.lock().unwrap().2.push((method, payload.to_owned()));
            Ok(())
        }
        fn poll(&self, session: SessionId) -> Result<UriPoll, String> {
            let mut state = self.0.lock().unwrap();
            let replies = if std::mem::take(&mut state.0) {
                vec![UriReply { session, request: RequestId::new(0), method: "register",
                    payload: String::new(), ok: true }]
            } else { Vec::new() };
            Ok(UriPoll { replies, close: state.1 })
        }
        fn close(&self, _: SessionId) -> Result<(), String> {
            self.0.lock().unwrap().1 = CloseStatus::Released;
            Ok(())
        }
    }

    struct Display;
    impl DisplaySink for Display {
        fn window_mode(&self) -> c_int { VP_EXPORTED }
        fn window_id(&self) -> &'static CStr { c"uri-boot-window" }
        unsafe fn create_window(&self, _: &plx_base::task::MainThread) -> *const c_char {
            c"uri-boot-window".as_ptr()
        }
        unsafe fn place_window(&self, _: &plx_base::task::MainThread,
            _: c_int, _: c_int, _: c_int, _: c_int, _: c_int, _: c_int) -> c_int { 1 }
        unsafe fn destroy_window(&self, mt: &plx_base::task::MainThread) {
            NoSink.destroy_window(mt)
        }
        unsafe fn plane_create(&self, mt: &plx_base::task::MainThread,
            app_id: *const c_char, player_type: c_int) -> c_long {
            NoSink.plane_create(mt, app_id, player_type)
        }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_initialized(&self, mt: &plx_base::task::MainThread) -> bool {
            NoSink.plane_probe_initialized(mt)
        }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_dass_available(&self, mt: &plx_base::task::MainThread) -> bool {
            NoSink.plane_probe_dass_available(mt)
        }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_media_id(&self, mt: &plx_base::task::MainThread,
            id: *const c_char) -> c_int { NoSink.plane_probe_media_id(mt, id) }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_loaded(&self, mt: &plx_base::task::MainThread) -> c_int {
            NoSink.plane_probe_loaded(mt)
        }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_connect_dass(&self, mt: &plx_base::task::MainThread) -> c_int {
            NoSink.plane_probe_connect_dass(mt)
        }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_finish(&self, mt: &plx_base::task::MainThread) -> c_int {
            NoSink.plane_probe_finish(mt)
        }
        unsafe fn plane_bind(&self, mt: &plx_base::task::MainThread, id: *const c_char) {
            NoSink.plane_bind(mt, id)
        }
        unsafe fn plane_send_video_data(&self, mt: &plx_base::task::MainThread,
            info: *const c_char) -> c_int { NoSink.plane_send_video_data(mt, info) }
        unsafe fn plane_send_atmos(&self, mt: &plx_base::task::MainThread,
            id: *const c_char) -> c_int { NoSink.plane_send_atmos(mt, id) }
        unsafe fn plane_start(&self, mt: &plx_base::task::MainThread,
            x: c_long, y: c_long, w: c_long, h: c_long) {
            NoSink.plane_start(mt, x, y, w, h)
        }
        unsafe fn plane_unload(&self, mt: &plx_base::task::MainThread) {
            NoSink.plane_unload(mt)
        }
        unsafe fn plane_pause(&self, mt: &plx_base::task::MainThread) {
            NoSink.plane_pause(mt)
        }
        unsafe fn plane_resume(&self, mt: &plx_base::task::MainThread) {
            NoSink.plane_resume(mt)
        }
    }

    #[test]
    fn boot_evidence_qualifies_resolved_part_and_dispatches_that_source() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else { return };
        plx_plex::plex::client_for(rig.sid).unwrap()
            .set_link(plx_plex::plex::probe::Location::Local);
        let bus: &'static Bus = Box::leak(Box::new(Bus::new()));
        let display: &'static Display = Box::leak(Box::new(Display));
        let caps = Capabilities { progressive_uri: true, hls_uri: false,
            packet_feed: false, pause: true, seek: true, audio_track: true,
            subtitle_track: true, audio_off: false, subtitle_off: false };
        let port = ResolvePort::new(ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) },
            callable: caps, certified: caps, preview_rectangle: false },
            Arc::new(QualifiedOriginal));
        assert!(rig.app.adapters.player.install_uri_product_with_ports(
            &mut rig.app.player.session, port, bus, display));
        rig.request();
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("qualified URI resolve did not land", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        let offer = resolved.unwrap().uri_offer.expect("the worker kept its owned URI source");
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session,
            offer).is_ok());
        assert!(matches!(rig.app.player.session.backend_offer(),
            plx_media::route::backend_policy::Selection::Open(choice)
                if choice.backend == BackendKind::Uri));
        let started = plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player);
        assert!(matches!(started, plx_media::player::BufferfeedStartOutcome::Launched(_)));
        let at = rig.app.adapters.player.backend_tick();
        let _ = rig.app.adapters.player.backend_poll_session_at(&mut rig.app.player.session, at);
        let load = bus.load_payload().expect("resolved URI reached the native transport");
        let load: serde_json::Value = serde_json::from_str(&load).unwrap();
        assert!(load["uri"].as_str().unwrap().contains("/library/parts/1/file.mkv"));
        assert_eq!(load["payload"]["option"]["windowId"], "uri-boot-window");
        assert!(!rig.requests.lock().unwrap().iter().any(|request|
            request.contains("/decision?")), "qualified original bypassed Feed MDE");
    }

    #[test]
    fn boot_without_source_certificate_keeps_feed_offer() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else { return };
        plx_plex::plex::client_for(rig.sid).unwrap()
            .set_link(plx_plex::plex::probe::Location::Local);
        let bus: &'static Bus = Box::leak(Box::new(Bus::new()));
        let display: &'static Display = Box::leak(Box::new(Display));
        let port = ResolvePort::new(ProductEvidence::unqualified(display.window_mode()),
            Arc::new(QualifiedOriginal));
        assert!(rig.app.adapters.player.install_uri_product_with_ports(
            &mut rig.app.player.session, port, bus, display));
        rig.request();
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("unqualified URI resolve did not land", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        assert!(resolved.unwrap().uri_offer.is_none());
        assert!(matches!(rig.app.player.session.backend_offer(),
            plx_media::route::backend_policy::Selection::Open(choice)
                if choice.backend == BackendKind::Feed));
        assert!(bus.load_payload().is_none(), "unknown evidence never dispatches a URI URL");
    }
}

