// Product URI boot and resolve regressions using the app lifecycle loopback rig.

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
                    expires_at: plx_media::player::adapter::uri_source_expiry_after(
                        Duration::from_secs(30)),
                certified_dolby_vision: None,
                certified_subtitle_source: None,
            })
        }
    }

    struct Bus(Mutex<(bool, CloseStatus, Vec<(&'static str, RequestId, String)>, Vec<UriReply>)>);
    impl Bus {
        fn new() -> Self { Self(Mutex::new((false, CloseStatus::Open, Vec::new(), Vec::new()))) }
        fn load_payload(&self) -> Option<String> {
            self.0.lock().unwrap().2.iter().find(|(method, _, _)| *method == "load")
                .map(|(_, _, payload)| payload.clone())
        }
        fn say(&self, method: &'static str, payload: &str) {
            let mut state = self.0.lock().unwrap();
            let request = state.2.iter().rev().find(|(name, _, _)| *name == method)
                .map(|(_, request, _)| *request).expect("the native method was queued");
            state.3.push(UriReply { session: SessionId::new(1), request,
                method, payload: payload.to_owned(), ok: true });
        }
        #[cfg(feature = "play-feed")]
        fn release(&self) { self.0.lock().unwrap().1 = CloseStatus::Released; }
    }
    impl UriMedia for Bus {
        fn open(&self, _: Shape) -> Result<SessionId, String> {
            self.0.lock().unwrap().0 = true;
            Ok(SessionId::new(1))
        }
        fn call(&self, _: SessionId, request: RequestId, method: &'static str,
            payload: &str) -> Result<(), String> {
            self.0.lock().unwrap().2.push((method, request, payload.to_owned()));
            Ok(())
        }
        fn poll(&self, session: SessionId) -> Result<UriPoll, String> {
            let mut state = self.0.lock().unwrap();
            let mut replies = if std::mem::take(&mut state.0) {
                vec![UriReply { session, request: RequestId::new(0), method: "register",
                    payload: String::new(), ok: true }]
            } else { Vec::new() };
            replies.append(&mut state.3);
            Ok(UriPoll { replies, close: state.1 })
        }
        fn close(&self, _: SessionId) -> Result<(), String> {
            self.0.lock().unwrap().1 = CloseStatus::Closing;
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
        let metric_request = rig.app.player.session.metric_request().expect("accepted URI request");
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
        let summary = rig.app.adapters.player.backend_metrics_snapshot()
            .expect("the product URI Start reached the common metrics owner");
        assert_eq!(summary.id, metric_request);
        assert_eq!(summary.attempt_count, 1);
        let physical = summary.current_attempt.expect("one addressed URI attempt");
        assert_eq!(physical.backend, plx_media::player::attempt_metrics::Backend::Uri);
        assert!(!physical.evidence.picture_presented,
            "dispatch and native Load request are not picture evidence");
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

    #[cfg(feature = "play-feed")]
    #[test]
    fn refused_product_uri_load_recovers_the_same_request_only_after_native_release() {
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
        let request = rig.app.player.session.metric_request().expect("logical request");
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("qualified URI source did not resolve", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        let offer = resolved.unwrap().uri_offer.expect("qualified original source");
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session,
            offer).is_ok());
        assert!(matches!(plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player),
            plx_media::player::BufferfeedStartOutcome::Launched(_)));
        let frame = |rig: &mut Rig| {
            let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
            unsafe { playback_tick(&mut rig.app, &mut fr); }
        };
        frame(&mut rig); // register opens the actual native Load
        bus.say("load", r#"{"returnValue":false}"#);
        frame(&mut rig); // fatal addressed Load response begins native close
        assert!(bus.0.lock().unwrap().1 == CloseStatus::Closing);
        assert_eq!(rig.app.player.session.metric_request(), Some(request),
            "Closing is not a handled terminal error while a Feed candidate remains");
        assert!(!plx_media::route::play_pending(),
            "the replacement resolve must wait for exact native release");
        bus.release();
        frame(&mut rig);
        assert!(plx_media::route::play_pending(),
            "an exact Released proof starts the same-episode Feed resolve");
        assert_eq!(rig.app.player.session.metric_request(), Some(request),
            "automatic handoff retains the logical request identity");
    }

    #[cfg(feature = "play-feed")]
    #[test]
    fn viewer_speed_edit_re_resolves_qualified_uri_source_for_feed_at_its_known_pause_and_clock() {
        let _serial = plx_base::testlock::serial();
        struct RestoreDecoder;
        impl Drop for RestoreDecoder {
            fn drop(&mut self) { plx_media::ff::force_speed_decoder_for_test(false); }
        }
        let _decoder = RestoreDecoder;
        // The host has no bundled TV FFmpeg; exercise the existing test-only decoder seam.
        plx_media::ff::force_speed_decoder_for_test(true);
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
        poll_until("qualified URI source did not resolve", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        let offer = resolved.unwrap().uri_offer.expect("the original source was certified");
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session,
            offer).is_ok());
        assert!(matches!(plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player),
            plx_media::player::BufferfeedStartOutcome::Launched(_)));
        let poll = |rig: &mut Rig| {
            let at = rig.app.adapters.player.backend_tick();
            let _ = rig.app.adapters.player.backend_poll_session_at(&mut rig.app.player.session, at);
        };
        poll(&mut rig); // addressed registration opens Load
        bus.say("load", r#"{"returnValue":true,"mediaId":"m1","loadCompleted":true}"#);
        poll(&mut rig); // Ready awaits the selected source audio track
        bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":0}}"#);
        poll(&mut rig);
        bus.say("play", r#"{"returnValue":true,"mediaId":"m1","playing":true,"currentTime":12000}"#);
        poll(&mut rig);
        assert_eq!(plx_media::player::intended_pos_known_ns(&rig.app.player.session),
            Some(12_000_000_000));
        assert_eq!(rig.app.adapters.player.backend_transport_control(true), Some(true));
        bus.say("pause", r#"{"returnValue":true,"mediaId":"m1","paused":true}"#);
        poll(&mut rig);
        assert_eq!(rig.app.adapters.player.backend_viewer_paused(), Some(true));
        let uri_metrics = rig.app.adapters.player.backend_metrics_snapshot()
            .expect("the original URI request reached the metrics owner");
        let uri_request = uri_metrics.id;
        assert_eq!(uri_metrics.attempt_count, 1);
        assert_eq!(uri_metrics.current_attempt.expect("URI physical attempt").backend,
            plx_media::player::attempt_metrics::Backend::Uri);
        let source = rig.app.player.session.source_selection();
        assert!(plx_media::route::source_decodable(&rig.app.player.session));
        assert!(plx_media::player::speed_offer(&rig.app.player.session).contains(&1500));
        crate::app::playback::apply_more_action(&mut rig.app.player.session,
            &mut rig.app.adapters.player, &mut rig.app.bridge,
            plx_appkit::more_menu::Action::SetSpeed(1500));
        assert_eq!(rig.app.player.session.requested_rate_permille(), 1500);
        assert_eq!(rig.app.player.session.reconfigure_pause_goal(), Some(true));
        assert_eq!(rig.app.player.session.attempt_origin(),
            plx_media::player::attempt_metrics::Origin::ViewerReconfigure);
        let revised_request = rig.app.player.session.metric_request()
            .expect("accepted viewer edit has its own request identity");
        assert_ne!(revised_request, uri_request);
        assert!(!plx_media::route::request_viewer_reconfigure(&mut rig.app.player.session,
            rig.app.bridge.metadata_mut(), plx_media::route::ViewerReconfigure::Speed(1500),
            Some(12_000_000_000), Some(true)), "the same viewer choice is a no-op");

        let mut revised = None;
        poll_until("viewer speed edit did not re-resolve", || {
            revised = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            revised.is_some()
        });
        let revised = revised.unwrap();
        assert!(revised.uri_offer.is_none(), "URI cannot silently play the new speed at 1x");
        assert_eq!(revised.resume_ns, 12_000_000_000);
        assert_eq!(rig.app.player.session.source_selection(), source,
            "executor substitution preserves the viewer's Part and selected source tracks");
        assert!(matches!(rig.app.player.session.backend_offer(),
            plx_media::route::backend_policy::Selection::Open(choice)
                if choice.backend == BackendKind::Feed));
        assert_eq!(rig.app.player.session.reconfigure_pause_goal(), Some(true),
            "the next physical Start still owes the paused viewer goal");
        assert!(matches!(plx_media::player::begin_resume(&mut rig.app.player.session,
            revised.resume_ns), plx_media::player::ResumeStart::Settled(
                plx_media::player::ResumeOutcome::Prepared)));
        let next = plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player);
        assert_eq!(next, plx_media::player::BufferfeedStartOutcome::Deferred,
            "the qualified Feed successor waits for addressed URI release");
        assert!(rig.app.adapters.player.release_has_queued_start());
        poll(&mut rig);
        assert!(rig.app.adapters.player.release_has_queued_start(),
            "Closing alone cannot admit the successor");
        bus.release();
        poll(&mut rig);
        assert!(!rig.app.adapters.player.release_has_queued_start());
        assert_eq!(rig.app.adapters.player.backend_snapshot().backend, Some(BackendKind::Feed));
        assert_eq!(rig.app.adapters.player.backend_viewer_paused(), Some(true));
        let feed_metrics = rig.app.adapters.player.backend_metrics_snapshot()
            .expect("the queued Feed successor reached the metrics owner");
        assert_eq!(feed_metrics.id, revised_request);
        assert_eq!(feed_metrics.origin,
            plx_media::player::attempt_metrics::Origin::ViewerReconfigure);
        assert_eq!(feed_metrics.attempt_count, 1);
        assert_eq!(feed_metrics.handoff_count, 0,
            "an explicit viewer edit is a new request, not an automatic handoff");
        assert_eq!(feed_metrics.current_attempt.expect("Feed physical attempt").backend,
            plx_media::player::attempt_metrics::Backend::Feed);
        assert_eq!(plx_media::player::play_rate_permille(), 1500,
            "Feed physical preparation applied the requested rate before its first Play");
    }
}

