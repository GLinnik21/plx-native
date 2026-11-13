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

    struct QualifiedTwoAudio;
    impl OriginalQualifier for QualifiedTwoAudio {
        fn qualify(&self, _: &plx_base::task::OffFrame, request: OriginalRequest) -> Verdict {
            let selected = request.selection;
            Verdict::Known(KnownSource {
                selection: selected,
                source: UriSource::new(request.source_url, vec![
                    NativeTrackIndex { selection: TrackSelection {
                        kind: TrackKind::Audio, id: 2 }, index: 0 },
                    NativeTrackIndex { selection: TrackSelection {
                        kind: TrackKind::Audio, id: 3 }, index: 1 },
                ]),
                requirements: Requirements { source: SourceKind::ProgressiveUri,
                    video: Fidelity::Exact, audio: Fidelity::Exact,
                    audio_track: TrackChoice::Selected(selected.audio_sid as u64),
                    subtitle_track: TrackChoice::Unspecified },
                expires_at: plx_media::player::adapter::uri_source_expiry_after(
                    Duration::from_secs(30)),
                certified_dolby_vision: None, certified_subtitle_source: None,
            })
        }
    }

    /// `.0` a registration is waiting to be answered, `.1` the close status, `.2` the native calls
    /// queued, `.3` the replies waiting, `.4` the identities registered as, in order, `.5` how many
    /// of the next registrations the hub refuses (the first rung's, then the second's).
    struct Bus(Mutex<(bool, CloseStatus, Vec<(&'static str, RequestId, String)>, Vec<UriReply>, Vec<Shape>, usize)>);
    impl Bus {
        fn new() -> Self { Self(Mutex::new((false, CloseStatus::Open, Vec::new(), Vec::new(), Vec::new(), 0))) }
        /// The identities the driver registered as, in order.
        fn shapes(&self) -> Vec<Shape> { self.0.lock().unwrap().4.clone() }
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
        /// How many native calls of `method` were queued.
        fn calls(&self, method: &str) -> usize {
            self.0.lock().unwrap().2.iter().filter(|(name, _, _)| *name == method).count()
        }
        /// The request id of the latest native call of `method`.
        fn last_request(&self, method: &str) -> RequestId {
            self.0.lock().unwrap().2.iter().rev().find(|(name, _, _)| *name == method)
                .map(|(_, request, _)| *request).expect("the native method was queued")
        }
        /// The payload of the latest native call of `method`.
        fn last_payload(&self, method: &str) -> String {
            self.0.lock().unwrap().2.iter().rev().find(|(name, _, _)| *name == method)
                .map(|(_, _, payload)| payload.clone()).expect("the native method was queued")
        }
        /// A reply to one named earlier request, not the latest of its method.
        fn say_request(&self, method: &'static str, request: RequestId, payload: &str) {
            self.0.lock().unwrap().3.push(UriReply { session: SessionId::new(1), request,
                method, payload: payload.to_owned(), ok: true });
        }
        #[cfg(feature = "play-feed")]
        fn release(&self) { self.0.lock().unwrap().1 = CloseStatus::Released; }
    }
    impl UriMedia for Bus {
        fn open(&self, shape: Shape) -> Result<SessionId, String> {
            let mut state = self.0.lock().unwrap();
            state.0 = true;
            state.1 = CloseStatus::Open;
            state.4.push(shape);
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
                let refused = state.5 > 0;
                if refused {
                    // The worker's answer to a registration the hub refused: a failed `register`
                    // reply, and the release it confirms (no live handle was ever made).
                    state.5 -= 1;
                    state.1 = CloseStatus::Released;
                }
                vec![UriReply { session, request: RequestId::new(0), method: "register",
                    payload: if refused { "code -1027: Invalid permissions".to_owned() } else { String::new() },
                    ok: !refused }]
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
        unsafe fn plane_initialized(&self, mt: &plx_base::task::MainThread) -> bool {
            NoSink.plane_initialized(mt)
        }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_dass_available(&self, mt: &plx_base::task::MainThread) -> bool {
            NoSink.plane_probe_dass_available(mt)
        }
        unsafe fn plane_media_id(&self, mt: &plx_base::task::MainThread,
            id: *const c_char) -> c_int { NoSink.plane_media_id(mt, id) }
        unsafe fn plane_loaded(&self, mt: &plx_base::task::MainThread) -> c_int {
            NoSink.plane_loaded(mt)
        }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_connect_dass(&self, mt: &plx_base::task::MainThread) -> c_int {
            NoSink.plane_probe_connect_dass(mt)
        }
        unsafe fn plane_finish(&self, mt: &plx_base::task::MainThread)
            -> plx_platform::tv::sink::PlaneFinish {
            NoSink.plane_finish(mt)
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
        unsafe fn plane_unload(&self, mt: &plx_base::task::MainThread)
            -> plx_platform::tv::sink::PlaneUnload { NoSink.plane_unload(mt) }
        fn plane_completions(&self, mt: &plx_base::task::MainThread)
            -> Vec<plx_platform::tv::sink::PlaneCompletion> { NoSink.plane_completions(mt) }
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
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, port, bus, display));
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

    /// The port the product composition builds for a viewer who opted in to the URI engine, over the
    /// exported-window `Display`: the binding of a major the product's exported-window table,
    /// `EXPORTED_STRATA`, has not admitted (`Unbound`: the host has no handle and the major is not
    /// listed) handed through the debug-only override's
    /// `compose` (`dev::uri_exported`) as `install_uri_product` does, and the production evidence
    /// (`unqualified_on` the display's mode, which an opted-in request certifies).
    #[cfg(feature = "devtriggers")]
    fn composed_port(exported_override: bool, display: &Display) -> ResolvePort {
        let binding = crate::dev::uri_exported::compose(Binding::Unbound, display.window_mode(),
            exported_override);
        ResolvePort::new(ProductEvidence::unqualified_on(display.window_mode(), binding).opted_in(),
            Arc::new(QualifiedOriginal))
    }

    /// With the exported-window override the composition binds the exported window on a `VP_EXPORTED`
    /// display, so the opted-in viewer's original is offered to the URI backend, and its `load` carries
    /// the exported window's id (webOS 5+: the window is created at the start and named in the load).
    #[cfg(feature = "devtriggers")]
    #[test]
    fn the_exported_override_offers_the_uri_backend_and_loads_with_the_exported_window() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else { return };
        plx_plex::plex::client_for(rig.sid).unwrap()
            .set_link(plx_plex::plex::probe::Location::Local);
        let bus: &'static Bus = Box::leak(Box::new(Bus::new()));
        let display: &'static Display = Box::leak(Box::new(Display));
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session,
            composed_port(true, display), bus, display));
        rig.request();
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("the override's URI resolve did not land", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        let offer = resolved.unwrap().uri_offer.expect("an exported display bound by the override offers the original");
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session, offer).is_ok());
        assert!(matches!(rig.app.player.session.backend_offer(),
            plx_media::route::backend_policy::Selection::Open(choice)
                if choice.backend == BackendKind::Uri));
        assert!(matches!(plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player),
            plx_media::player::BufferfeedStartOutcome::Launched(_)));
        let at = rig.app.adapters.player.backend_tick();
        let _ = rig.app.adapters.player.backend_poll_session_at(&mut rig.app.player.session, at);
        let load: serde_json::Value = serde_json::from_str(
            &bus.load_payload().expect("the offered URI reached the native transport")).unwrap();
        assert_eq!(load["payload"]["option"]["windowId"], "uri-boot-window");
    }

    /// Without the override the same exported display composes `Unbound`: nothing is offered, Feed
    /// plays, and no URI `load` is ever sent (production is unchanged).
    #[cfg(feature = "devtriggers")]
    #[test]
    fn without_the_exported_override_the_same_display_keeps_the_feed_offer() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else { return };
        plx_plex::plex::client_for(rig.sid).unwrap()
            .set_link(plx_plex::plex::probe::Location::Local);
        let bus: &'static Bus = Box::leak(Box::new(Bus::new()));
        let display: &'static Display = Box::leak(Box::new(Display));
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session,
            composed_port(false, display), bus, display));
        rig.request();
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("the unarmed URI resolve did not land", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        assert!(resolved.unwrap().uri_offer.is_none(), "an unbound display offers nothing");
        assert!(matches!(rig.app.player.session.backend_offer(),
            plx_media::route::backend_policy::Selection::Open(choice)
                if choice.backend == BackendKind::Feed));
        assert!(bus.load_payload().is_none());
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
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, port, bus, display));
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

    /// The 4.5 service, as captured on the set: it reports the audio track it selected
    /// (`audioInfo.track`) before `loadCompleted`, acknowledges the startup `selectTrack` of that
    /// same track, and never sends `trackSelected` for a selection that changes nothing. The
    /// startup plan holds the first Play behind that selection, so the route must still reach
    /// `play` on the acknowledgement alone (RED against the driver that waited for a
    /// `trackSelected` which never comes: no `play` for 15 s, then a refused control and a Feed
    /// fallback). Every other fixture here injects the `trackSelected` the real service omits.
    #[test]
    fn product_uri_plays_when_the_service_never_confirms_the_default_audio_track() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else { return };
        plx_plex::plex::client_for(rig.sid).unwrap()
            .set_link(plx_plex::plex::probe::Location::Local);
        let bus: &'static Bus = Box::leak(Box::new(Bus::new()));
        let display: &'static Display = Box::leak(Box::new(Display));
        let caps = Capabilities { progressive_uri: true, hls_uri: false,
            packet_feed: false, pause: true, seek: true, audio_track: true,
            subtitle_track: false, audio_off: false, subtitle_off: false };
        let port = ResolvePort::new(ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) },
            callable: caps, certified: caps, preview_rectangle: false },
            Arc::new(QualifiedOriginal));
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, port, bus, display));
        rig.request();
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("qualified URI source did not resolve", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session,
            resolved.unwrap().uri_offer.expect("qualified source")).is_ok());
        assert!(matches!(plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player),
            plx_media::player::BufferfeedStartOutcome::Launched(_)));
        let frame = |rig: &mut Rig| {
            let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
            unsafe { playback_tick(&mut rig.app, &mut fr); }
        };
        frame(&mut rig);
        bus.say("load", r#"{"returnValue":true,"mediaId":"m1"}"#);
        frame(&mut rig);
        // The subscription's events, in the captured order.
        bus.say("subscribe", r#"{"audioInfo":{"track":0,"immersive":"none","mediaId":"m1"}}"#);
        frame(&mut rig);
        bus.say("subscribe", r#"{"loadCompleted":{"mediaId":"m1"}}"#);
        frame(&mut rig);
        frame(&mut rig);
        assert_eq!(bus.calls("play"), 1,
            "the service has loaded and the startup selection is settled: the first Play goes out");
        assert_eq!(bus.calls("selectTrack"), 0,
            "the track the service already selected needs no native selection");
    }

    /// The same captured 4.5 service, for a start that carries a resume position: the startup plan
    /// sequences the audio selection, then Seek(resume), then Play, each confirmed. The service
    /// answers the seek call with a plain reply that has no `seekDone` in it and reports the
    /// `seekDone` on the subscription about a hundred milliseconds later (the ls-monitor capture
    /// of a bound session). RED against the driver that looked for `seekDone` only inside the seek's
    /// own reply: the seek stayed pending, no `play` for 15 s, then a refused control and a Feed
    /// fallback (the TV run of the `..._bound_collapse` case). The only fixture here that follows
    /// the device's order for a resume; the others inject events the real service never sends.
    #[test]
    fn product_uri_plays_after_a_resume_seek_the_service_reports_on_its_subscription() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else { return };
        plx_plex::plex::client_for(rig.sid).unwrap()
            .set_link(plx_plex::plex::probe::Location::Local);
        let bus: &'static Bus = Box::leak(Box::new(Bus::new()));
        let display: &'static Display = Box::leak(Box::new(Display));
        let caps = Capabilities { progressive_uri: true, hls_uri: false,
            packet_feed: false, pause: true, seek: true, audio_track: true,
            subtitle_track: false, audio_off: false, subtitle_off: false };
        let port = ResolvePort::new(ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) },
            callable: caps, certified: caps, preview_rectangle: false },
            Arc::new(QualifiedOriginal));
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, port, bus, display));
        rig.request();
        assert!(plx_media::route::arm_play_resume(&mut rig.app.player.session, 12_000_000_000),
            "the viewer's resume point rides the resolve in flight");
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("qualified URI source did not resolve", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session,
            resolved.unwrap().uri_offer.expect("qualified source")).is_ok());
        assert!(matches!(plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player),
            plx_media::player::BufferfeedStartOutcome::Launched(_)));
        let frame = |rig: &mut Rig| {
            let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
            unsafe { playback_tick(&mut rig.app, &mut fr); }
        };
        frame(&mut rig);
        bus.say("load", r#"{"errorCode":0,"returnValue":true,"errorText":"No Error","mediaId":"m1"}"#);
        frame(&mut rig);
        // The subscription's events, in the captured order.
        bus.say("subscribe", r#"{"audioInfo":{"track":0,"immersive":"none","mediaId":"m1"}}"#);
        frame(&mut rig);
        bus.say("subscribe", r#"{"loadCompleted":{"mediaId":"m1"}}"#);
        for _ in 0..3 { frame(&mut rig); }
        assert_eq!(bus.calls("selectTrack"), 0, "the audio track the service chose needs no call");
        assert_eq!(bus.calls("seek"), 1, "the resume position is sought before the first Play");
        let sought: serde_json::Value = serde_json::from_str(&bus.last_payload("seek")).unwrap();
        assert_eq!(sought["position"], 12_000);
        assert_eq!(bus.calls("play"), 0, "the seek has not landed");
        bus.say("seek", r#"{"errorCode":0,"returnValue":true,"errorText":"No Error","mediaId":"m1"}"#);
        for _ in 0..3 { frame(&mut rig); }
        assert_eq!(bus.calls("play"), 0, "the reply to the seek call is not its landing");
        bus.say("subscribe", r#"{"seekDone":{"mediaId":"m1"}}"#);
        for _ in 0..3 { frame(&mut rig); }
        assert_eq!(bus.calls("play"), 1,
            "the service reported the seek done: the startup plan goes on to the first Play");
    }

    // ---- the URI dev route (`plxnative-uridev`): the production composition, ACB-bound ----

    /// The pair `install_uri_product` builds on an ACB-bound set (evidence nothing certifies, the
    /// production qualifier), handed through the dev route's `compose` with `trigger`. The host has
    /// no ACB plane, so the evidence is built the way `product_binding` yields it for one rather
    /// than read from the process; the OFFER is what the host proves, the bind is TV-only.
    #[cfg(feature = "devtriggers")]
    fn acb_port(trigger: Option<&str>) -> ResolvePort {
        use crate::dev::uri_product as dev_route;
        let evidence = ProductEvidence::unqualified_on(plx_platform::tv::sink::VP_ACB,
            Binding::Acb);
        let (evidence, qualifier) = dev_route::compose(evidence,
            Arc::new(plx_media::route::uri_preflight::ProductQualifier),
            trigger.map(str::to_owned));
        ResolvePort::new(evidence, qualifier)
    }

    /// A rig whose boot composed the ACB port for `trigger`, and the play of its one Part (id 1, on
    /// the server it registered as `lifecycle-fixture`) landed: the bus, and what the plan resolved.
    #[cfg(feature = "devtriggers")]
    fn land_acb_play(rig: &mut Rig, trigger: Option<&str>)
        -> (&'static Bus, plx_media::route::ResolvedStart) {
        plx_plex::plex::client_for(rig.sid).unwrap()
            .set_link(plx_plex::plex::probe::Location::Local);
        let bus: &'static Bus = Box::leak(Box::new(Bus::new()));
        let display: &'static Display = Box::leak(Box::new(Display));
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, acb_port(trigger), bus,
            display));
        rig.request();
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("ACB URI resolve did not land", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        (bus, resolved.unwrap())
    }

    #[cfg(feature = "devtriggers")]
    const DEV_URL: &str = "http://10.9.8.7:8080/dev/index.m3u8";

    /// Green guard: with no trigger the ACB-bound composition is today's, and Feed is offered.
    #[cfg(feature = "devtriggers")]
    #[test]
    fn uri_dev_unarmed_acb_port_keeps_todays_offer() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else { return };
        let (bus, resolved) = land_acb_play(&mut rig, None);
        assert!(resolved.uri_offer.is_none(), "no trigger, no URI offer on ACB");
        assert!(matches!(rig.app.player.session.backend_offer(),
            plx_media::route::backend_policy::Selection::Open(choice)
                if choice.backend == BackendKind::Feed));
        assert!(bus.load_payload().is_none(), "unknown evidence never dispatches a URI URL");
    }

    /// The trigger the harness writes for the rig's server: `pms=` is the ORIGIN its client holds
    /// (`http://127.0.0.1:<the fixture's port>`, what `PMS_HOST:PMS_PORT` is on a real boot).
    #[cfg(feature = "devtriggers")]
    fn armed_for(rig: &Rig, part: i64) -> String {
        let pms = plx_plex::plex::client_for(rig.sid).unwrap().origin().authority();
        format!("acb part={part} pms={pms} url={DEV_URL}")
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn uri_dev_armed_acb_port_offers_the_trigger_url() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else { return };
        let trigger = armed_for(&rig, 1);
        let (_, resolved) = land_acb_play(&mut rig, Some(&trigger));
        let offer = resolved.uri_offer.expect("the armed dev route certified the pinned Part");
        assert_eq!(offer.requirements.source, SourceKind::ProgressiveUri);
        let (url, _) = offer.source.into_parts();
        assert_eq!(url, DEV_URL, "the trigger's URL, not the PMS Part's");
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn uri_dev_armed_acb_offer_is_accepted_and_opens_the_uri_backend() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else { return };
        let trigger = armed_for(&rig, 1);
        let (_, resolved) = land_acb_play(&mut rig, Some(&trigger));
        let offer = resolved.uri_offer.expect("the armed dev route certified the pinned Part");
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session,
            offer).is_ok());
        assert!(matches!(rig.app.player.session.backend_offer(),
            plx_media::route::backend_policy::Selection::Open(choice)
                if choice.backend == BackendKind::Uri));
    }

    /// The harness's boot: `--guest --mock` registers the primary by ADDRESS ONLY (`""` for its
    /// machine identifier, nothing learns it before the first play), and the trigger names that
    /// server the only way the harness can, by the origin its client holds. The armed route must
    /// still offer the trigger's URL. (RED against the machine-identifier pin this replaced: the
    /// request carried `""`, the qualifier answered Unknown and the Feed played.)
    #[cfg(feature = "devtriggers")]
    #[test]
    fn uri_dev_armed_on_an_address_only_slot_offers_the_trigger_url() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving_by_address(false) else { return };
        assert_eq!(plx_plex::plex::client_for(rig.sid).unwrap().machine_id(), "",
            "the rig registered the slot by address only");
        let trigger = armed_for(&rig, 1);
        let (_, resolved) = land_acb_play(&mut rig, Some(&trigger));
        let offer = resolved.uri_offer.expect("the trigger pins the slot's address, not its id");
        let (url, _) = offer.source.into_parts();
        assert_eq!(url, DEV_URL, "the trigger's URL, not the PMS Part's");
    }

    /// The pin is on the server's ORIGIN, which the plan's request carries from the client: the same
    /// Part id at another address, or on another port of the same one, is not certified.
    #[cfg(feature = "devtriggers")]
    #[test]
    fn uri_dev_armed_for_another_origin_keeps_the_feed_offer() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else { return };
        let port = plx_plex::plex::client_for(rig.sid).unwrap().port();
        let other = format!("acb part=1 pms=10.9.8.7:{port} url={DEV_URL}");
        let (bus, resolved) = land_acb_play(&mut rig, Some(&other));
        assert!(resolved.uri_offer.is_none());
        assert!(matches!(rig.app.player.session.backend_offer(),
            plx_media::route::backend_policy::Selection::Open(choice)
                if choice.backend == BackendKind::Feed));
        assert!(bus.load_payload().is_none());
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn uri_dev_armed_for_another_port_of_the_same_host_keeps_the_feed_offer() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else { return };
        let port = plx_plex::plex::client_for(rig.sid).unwrap().port() ^ 1;
        let other = format!("acb part=1 pms=127.0.0.1:{port} url={DEV_URL}");
        let (_, resolved) = land_acb_play(&mut rig, Some(&other));
        assert!(resolved.uri_offer.is_none());
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn uri_dev_armed_for_another_part_keeps_the_feed_offer() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else { return };
        let trigger = armed_for(&rig, 2);
        let (_, resolved) = land_acb_play(&mut rig, Some(&trigger));
        assert!(resolved.uri_offer.is_none());
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
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, port, bus, display));
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
        // The first rung's denied Load is not yet a failure (`register_ladder`): its registration
        // closes quietly, and the anonymous client registers once that release is proven.
        frame(&mut rig);
        assert!(bus.0.lock().unwrap().1 == CloseStatus::Closing);
        assert!(!plx_media::route::play_pending(), "a refused rung is no handoff");
        bus.release();
        frame(&mut rig); // exact release of the named registration: the anonymous one registers
        assert_eq!(bus.shapes(), [Shape::Named, Shape::Anon]);
        assert!(!plx_media::route::play_pending(), "the ladder still has its second rung");
        frame(&mut rig); // the anonymous registration is granted and opens its own native Load
        assert_eq!(bus.calls("load"), 2);
        bus.say("load", r#"{"returnValue":false}"#);
        frame(&mut rig); // the second rung's denied Load is fatal and begins native close
        assert!(bus.0.lock().unwrap().1 == CloseStatus::Closing);
        assert_eq!(rig.app.player.session.metric_request(), Some(request),
            "Closing is not a handled terminal error while a Feed candidate remains");
        assert!(!plx_media::route::play_pending(),
            "the replacement resolve must wait for exact native release");
        bus.release();
        frame(&mut rig);
        assert!(plx_media::route::play_pending(),
            "an exact Released proof starts the same-episode Feed resolve");
        assert_eq!(bus.shapes(), [Shape::Named, Shape::Anon], "no third registration");
        assert_eq!(rig.app.player.session.metric_request(), Some(request),
            "automatic handoff retains the logical request identity");
        // The real URI driver's refused Load is ONE typed failed-start record at the Open stage,
        // its handoff to Feed is ONE record naming the same source attempt, and the clean
        // release that follows records nothing.
        use plx_media::player::attempt_metrics::{Backend, Incident, Stage};
        let records: Vec<_> = rig.app.adapters.player.backend_metric_notices().filter_map(|n| match n {
            plx_media::player::backend::MetricNotice::Measured(
                plx_media::player::attempt_metrics::Fx::Incident(record)) => Some(*record),
            _ => None,
        }).collect();
        let failed: Vec<_> = records.iter().filter(|r| matches!(r.incident, Incident::Failed(_))).collect();
        let handoffs: Vec<_> = records.iter().filter(|r| r.incident == Incident::Handoff).collect();
        assert_eq!(failed.len(), 1, "{records:?}");
        assert_eq!((failed[0].backend, failed[0].stage), (Backend::Uri, Stage::Open));
        assert_eq!(handoffs.len(), 1, "{records:?}");
        assert_eq!((handoffs[0].backend, handoffs[0].attempt), (Backend::Uri, failed[0].attempt));
        assert_eq!(records.len(), 2, "no release or rebuffer record for a clean close: {records:?}");
    }

    #[cfg(feature = "play-feed")]
    #[test]
    fn a_hub_that_refuses_both_register_rungs_hands_the_same_request_to_feed() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else { return };
        plx_plex::plex::client_for(rig.sid).unwrap()
            .set_link(plx_plex::plex::probe::Location::Local);
        let bus: &'static Bus = Box::leak(Box::new(Bus::new()));
        bus.0.lock().unwrap().5 = 2; // the hub refuses the named client, then the anonymous one
        let display: &'static Display = Box::leak(Box::new(Display));
        let caps = Capabilities { progressive_uri: true, hls_uri: false,
            packet_feed: false, pause: true, seek: true, audio_track: true,
            subtitle_track: true, audio_off: false, subtitle_off: false };
        let port = ResolvePort::new(ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) },
            callable: caps, certified: caps, preview_rectangle: false },
            Arc::new(QualifiedOriginal));
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, port, bus, display));
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
        frame(&mut rig); // the named client's registration is refused: released quietly, no Load
        assert_eq!(bus.shapes(), [Shape::Named, Shape::Anon]);
        assert_eq!(bus.calls("load"), 0);
        assert!(!plx_media::route::play_pending(), "a refused rung is no handoff");
        assert_eq!(rig.app.player.session.metric_request(), Some(request));
        frame(&mut rig); // the anonymous client is refused too: the one failed start, then its release
        assert_eq!(bus.shapes(), [Shape::Named, Shape::Anon], "no third registration");
        assert_eq!(bus.calls("load"), 0, "no Load was ever sent");
        assert!(plx_media::route::play_pending(),
            "the confirmed release of the refused registrations starts the same-episode Feed resolve");
        assert_eq!(rig.app.player.session.metric_request(), Some(request),
            "automatic handoff retains the logical request identity");
        // The real URI driver's refusal of both rungs is ONE typed failed-start record at the Open stage,
        // its handoff to Feed is ONE record naming the same source attempt, and the clean
        // release that follows records nothing.
        use plx_media::player::attempt_metrics::{Backend, Incident, Stage};
        let records: Vec<_> = rig.app.adapters.player.backend_metric_notices().filter_map(|n| match n {
            plx_media::player::backend::MetricNotice::Measured(
                plx_media::player::attempt_metrics::Fx::Incident(record)) => Some(*record),
            _ => None,
        }).collect();
        let failed: Vec<_> = records.iter().filter(|r| matches!(r.incident, Incident::Failed(_))).collect();
        let handoffs: Vec<_> = records.iter().filter(|r| r.incident == Incident::Handoff).collect();
        assert_eq!(failed.len(), 1, "{records:?}");
        assert_eq!((failed[0].backend, failed[0].stage), (Backend::Uri, Stage::Open));
        assert_eq!(handoffs.len(), 1, "{records:?}");
        assert_eq!((handoffs[0].backend, handoffs[0].attempt), (Backend::Uri, failed[0].attempt));
        assert_eq!(records.len(), 2, "no release or rebuffer record for a clean close: {records:?}");
    }

    #[cfg(feature = "play-feed")]
    #[test]
    fn failed_playing_uri_recovers_its_known_clock_pause_and_selected_source() {
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
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, port, bus, display));
        rig.request();
        let request = rig.app.player.session.metric_request().expect("logical request");
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("qualified URI source did not resolve", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session,
            resolved.unwrap().uri_offer.expect("qualified source")).is_ok());
        assert!(matches!(plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player),
            plx_media::player::BufferfeedStartOutcome::Launched(_)));
        let frame = |rig: &mut Rig| {
            let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
            unsafe { playback_tick(&mut rig.app, &mut fr); }
        };
        frame(&mut rig);
        bus.say("load", r#"{"returnValue":true,"mediaId":"m1","loadCompleted":true}"#);
        frame(&mut rig);
        bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":0}}"#);
        frame(&mut rig);
        bus.say("play", r#"{"returnValue":true,"mediaId":"m1","playing":true,"currentTime":12000}"#);
        frame(&mut rig);
        assert_eq!(plx_media::player::intended_pos_known_ns(&rig.app.player.session),
            Some(12_000_000_000));
        assert_eq!(rig.app.adapters.player.backend_transport_control(&mut rig.app.player.session, true),
            Some(true));
        bus.say("pause", r#"{"returnValue":true,"mediaId":"m1","paused":true}"#);
        frame(&mut rig);
        let reports_before_failure = plx_telemetry::telemetry::playback::report_calls_for_test();
        let uri_metrics = rig.app.adapters.player.backend_metrics_snapshot()
            .expect("URI physical attempt reached the measurement owner");
        assert_eq!(uri_metrics.id, request);
        assert_eq!(uri_metrics.attempt_count, 1);
        assert!(!uri_metrics.current_attempt.expect("URI attempt").evidence.picture_presented,
            "native Playing and a 12-second clock are not proof of a presented picture");
        let source = rig.app.player.session.source_selection();
        bus.say("subscribe", r#"{"mediaId":"m1","error":"native playback failed","errorCode":104}"#);
        frame(&mut rig);
        assert_eq!(rig.app.player.session.metric_request(), Some(request));
        assert!(!plx_media::route::play_pending());
        let closing = rig.app.adapters.player.backend_metrics_snapshot().unwrap();
        assert_eq!(closing.attempt_count, 1);
        assert_eq!(closing.handoff_count, 1);
        assert!(closing.outcome.is_none(), "native Closing is a recoverable physical failure");
        assert_eq!(plx_telemetry::telemetry::playback::report_calls_for_test(),
            reports_before_failure, "Closing must not send a handled terminal error");
        bus.release();
        frame(&mut rig);
        assert!(plx_media::route::play_pending());
        assert_eq!(rig.app.player.session.metric_request(), Some(request));
        assert_eq!(rig.app.player.session.reconfigure_pause_goal(), Some(true));
        let mut replacement = None;
        poll_until("automatic Feed rescue did not resolve", || {
            replacement = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            replacement.is_some()
        });
        let replacement = replacement.unwrap();
        assert!(replacement.uri_offer.is_none(), "automatic rescue cannot loop back to URI");
        assert_eq!(replacement.resume_ns, 12_000_000_000);
        assert_eq!(rig.app.player.session.source_selection(), source);
        assert_eq!(rig.app.player.session.metric_request(), Some(request));
        assert!(matches!(plx_media::player::begin_resume(&mut rig.app.player.session,
            &mut rig.app.adapters.player, replacement.resume_ns), plx_media::player::ResumeStart::Settled(
                plx_media::player::ResumeOutcome::Prepared)));
        assert!(matches!(plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player),
            plx_media::player::BufferfeedStartOutcome::Launched(_)));
        let feed_metrics = rig.app.adapters.player.backend_metrics_snapshot()
            .expect("the same request now owns its Feed attempt");
        assert_eq!(feed_metrics.id, request);
        assert_eq!(feed_metrics.attempt_count, 2);
        assert_eq!(feed_metrics.handoff_count, 1);
        assert!(feed_metrics.outcome.is_none());
        assert_eq!(feed_metrics.current_attempt.expect("Feed successor").backend,
            plx_media::player::attempt_metrics::Backend::Feed);
        assert!(!feed_metrics.recent_attempts[0].evidence.picture_presented,
            "the old URI service clock did not become picture evidence");
        assert_eq!(plx_telemetry::telemetry::playback::report_calls_for_test(),
            reports_before_failure, "a successful handoff has no terminal handled report");
    }

    #[cfg(feature = "play-feed")]
    #[test]
    fn viewer_speed_edit_re_resolves_qualified_uri_source_for_feed_at_its_known_pause_and_clock() {
        let _serial = plx_base::testlock::serial();
        struct RestoreDecoder;
        impl Drop for RestoreDecoder {
            fn drop(&mut self) { plx_media_codecs::ffi::force_speed_decoder_for_test(false); }
        }
        let _decoder = RestoreDecoder;
        // The host has no bundled TV FFmpeg; exercise the existing test-only decoder seam.
        plx_media_codecs::ffi::force_speed_decoder_for_test(true);
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
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, port, bus, display));
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
        assert_eq!(rig.app.adapters.player.backend_transport_control(&mut rig.app.player.session, true),
            Some(true));
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
            plx_appkit::more_menu::Action::SetSpeed(1500), &mut rig.app.pages.present);
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
            &mut rig.app.adapters.player, revised.resume_ns), plx_media::player::ResumeStart::Settled(
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
        assert_eq!(plx_media::player::play_rate_permille(&rig.app.player.session), 1500,
            "Feed physical preparation applied the requested rate before its first Play");
    }

    /// The native `selectTrack` protocol through the product driver (`UriBackend`) and the adapter,
    /// read only through what the adapter publishes: a pick commits on the exact request's
    /// acknowledgement AND the matching `trackSelected`, a retired request cannot answer for its
    /// successor, and the latest refused pick is the one the resolver is asked about.
    #[test]
    fn a_native_audio_pick_commits_on_the_exact_request_and_a_refusal_falls_back_once() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving_with(|request| {
            queue_body(request, false).replace(
                r#"{"id":2,"streamType":2,"codec":"aac","selected":true}"#,
                r#"{"id":2,"streamType":2,"codec":"aac","selected":true},{"id":3,"streamType":2,"codec":"ac3","channels":6}"#)
        }) else { return };
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
            Arc::new(QualifiedTwoAudio));
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, port, bus, display));
        rig.request();
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("qualified URI source did not resolve", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session,
            resolved.unwrap().uri_offer.expect("qualified original source")).is_ok());
        assert!(matches!(plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player),
            plx_media::player::BufferfeedStartOutcome::Launched(_)));
        let poll = |rig: &mut Rig| {
            let at = rig.app.adapters.player.backend_tick();
            let _ = rig.app.adapters.player.backend_poll_session_at(&mut rig.app.player.session, at);
        };
        let audio = |rig: &Rig| rig.app.player.session.source_selection().map(|s| s.audio_sid);
        poll(&mut rig);
        bus.say("load", r#"{"returnValue":true,"mediaId":"m1","loadCompleted":true}"#);
        poll(&mut rig);
        bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":0}}"#);
        poll(&mut rig);
        bus.say("play", r#"{"returnValue":true,"mediaId":"m1","playing":true,"currentTime":12000}"#);
        poll(&mut rig);
        assert_eq!(audio(&rig), Some(2));

        let pick = |sid: i64, ordinal: i32| plx_media::route::CarriedAudio::named(sid, ordinal);
        assert!(rig.app.adapters.player.commit_audio_track(&mut rig.app.player.session, pick(3, 1), None));
        let first = bus.last_request("selectTrack");
        assert_eq!(audio(&rig), Some(2), "queue success is not selection");
        assert!(!rig.app.adapters.player.commit_audio_track(&mut rig.app.player.session, pick(2, 0), None),
            "a second same-kind request is refused while the first native request is unresolved");
        assert_eq!(bus.last_request("selectTrack"), first, "the refused request was never sent");
        bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        poll(&mut rig);
        assert_eq!(audio(&rig), Some(2), "native ACK is not selected-track evidence");
        bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":1}}"#);
        poll(&mut rig);
        assert_eq!(audio(&rig), Some(3));

        assert!(rig.app.adapters.player.commit_audio_track(&mut rig.app.player.session, pick(2, 0), None));
        let successor = bus.last_request("selectTrack");
        assert_ne!(successor, first);
        bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":1}}"#);
        poll(&mut rig);
        assert_eq!(audio(&rig), Some(3), "late old selection cannot commit the pending successor");
        bus.say_request("selectTrack", first, r#"{"returnValue":false,"mediaId":"m1"}"#);
        poll(&mut rig);
        assert!(rig.app.adapters.player.take_uri_track_fallback().is_none(),
            "a retired native request cannot schedule an old source reconfiguration");
        bus.say_request("selectTrack", successor, r#"{"returnValue":false,"mediaId":"m1"}"#);
        poll(&mut rig);
        assert_eq!(audio(&rig), Some(3), "a refused pick never moves the route");
        assert!(matches!(rig.app.adapters.player.take_uri_track_fallback(),
            Some(plx_media::player::adapter::UriTrackFallback::Audio { audio, .. }) if audio.sid == 2),
            "the exact refused command asks the resolver, once");
        assert!(rig.app.adapters.player.take_uri_track_fallback().is_none());
    }

    #[cfg(feature = "play-feed")]
    #[test]
    fn audio_pick_missing_from_uri_native_map_re_resolves_the_same_source() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving_with(|request| {
            queue_body(request, false).replace(
                r#"{"id":2,"streamType":2,"codec":"aac","selected":true}"#,
                r#"{"id":2,"streamType":2,"codec":"aac","selected":true},{"id":3,"streamType":2,"codec":"ac3","channels":6}"#)
        }) else { return };
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
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, port, bus, display));
        rig.request();
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("qualified URI source did not resolve", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session,
            resolved.unwrap().uri_offer.expect("qualified original source")).is_ok());
        assert!(matches!(plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player),
            plx_media::player::BufferfeedStartOutcome::Launched(_)));
        let poll = |rig: &mut Rig| {
            let at = rig.app.adapters.player.backend_tick();
            let _ = rig.app.adapters.player.backend_poll_session_at(&mut rig.app.player.session, at);
        };
        poll(&mut rig);
        bus.say("load", r#"{"returnValue":true,"mediaId":"m1","loadCompleted":true}"#);
        poll(&mut rig);
        bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":0}}"#);
        poll(&mut rig);
        bus.say("play", r#"{"returnValue":true,"mediaId":"m1","playing":true,"currentTime":12000}"#);
        poll(&mut rig);
        let before = rig.app.player.session.metric_request();
        assert_eq!(rig.app.player.session.source_selection().map(|s| s.audio_sid), Some(2));
        rig.pick_audio(3);
        assert!(plx_media::route::play_pending(),
            "a source track absent from the URI native map needs the common same-source resolve");
        assert_ne!(rig.app.player.session.metric_request(), before,
            "the explicit pick starts its own bounded viewer episode");
        assert_eq!(rig.app.player.session.attempt_origin(),
            plx_media::player::attempt_metrics::Origin::ViewerReconfigure);
    }

    #[cfg(feature = "play-feed")]
    #[test]
    fn forced_only_audio_pick_with_native_index_still_re_resolves_the_source() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving_with(|request| {
            queue_body(request, false).replace(
                r#"{"id":2,"streamType":2,"codec":"aac","selected":true}"#,
                r#"{"id":2,"streamType":2,"codec":"aac","selected":true},{"id":3,"streamType":2,"codec":"ac3","channels":6}"#)
        }) else { return };
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
            Arc::new(QualifiedTwoAudio));
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, port, bus, display));
        rig.request();
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("qualified URI source did not resolve", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session,
            resolved.unwrap().uri_offer.expect("qualified original source")).is_ok());
        assert!(matches!(plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player),
            plx_media::player::BufferfeedStartOutcome::Launched(_)));
        let poll = |rig: &mut Rig| {
            let at = rig.app.adapters.player.backend_tick();
            let _ = rig.app.adapters.player.backend_poll_session_at(&mut rig.app.player.session, at);
        };
        poll(&mut rig);
        bus.say("load", r#"{"returnValue":true,"mediaId":"m1","loadCompleted":true}"#);
        poll(&mut rig);
        bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":0}}"#);
        poll(&mut rig);
        bus.say("play", r#"{"returnValue":true,"mediaId":"m1","playing":true,"currentTime":12000}"#);
        poll(&mut rig);
        assert_eq!(rig.app.adapters.player.uri_maps_track(&rig.app.player.session,
            TrackKind::Audio, 3), Some(true));
        plx_media::route::set_sub_mode_for_test(&mut rig.app.player.session,
            plx_media::route::SubMode::ForcedOnly);
        rig.pick_audio(3);
        assert!(plx_media::route::play_pending(),
            "an audio pick under Forced only must re-resolve when native control refuses it");
    }

    #[cfg(feature = "play-feed")]
    #[test]
    fn sidecar_pick_on_uri_re_resolves_with_the_selected_external_source_sid() {
        let _serial = plx_base::testlock::serial();
        let sidecar_listed = Arc::new(AtomicBool::new(false));
        let listing = sidecar_listed.clone();
        let Some(mut rig) = Rig::serving_with(move |request| {
            let body = queue_body(request, false).to_owned();
            if listing.load(Ordering::Acquire) && request.contains("/library/metadata/") {
                body.replace(
                    r#"{"id":2,"streamType":2,"codec":"aac","selected":true}"#,
                    r#"{"id":2,"streamType":2,"codec":"aac","selected":true},{"id":3,"streamType":3,"codec":"srt","languageCode":"eng","key":"/library/streams/3"}"#)
            } else { body }
        }) else { return };
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
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, port, bus, display));
        rig.request();
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("qualified URI source did not resolve", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session,
            resolved.unwrap().uri_offer.expect("qualified original source")).is_ok());
        assert!(matches!(plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player),
            plx_media::player::BufferfeedStartOutcome::Launched(_)));
        let poll = |rig: &mut Rig| {
            let at = rig.app.adapters.player.backend_tick();
            let _ = rig.app.adapters.player.backend_poll_session_at(&mut rig.app.player.session, at);
        };
        poll(&mut rig);
        bus.say("load", r#"{"returnValue":true,"mediaId":"m1","loadCompleted":true}"#);
        poll(&mut rig);
        bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":0}}"#);
        poll(&mut rig);
        bus.say("play", r#"{"returnValue":true,"mediaId":"m1","playing":true,"currentTime":12000}"#);
        poll(&mut rig);
        // A sidecar becomes available while the original URI is playing. The initial source
        // contained no subtitle, so its Unspecified native subtitle requirement was truthful.
        sidecar_listed.store(true, Ordering::Release);
        let client = plx_plex::plex::client_for(rig.sid).unwrap();
        let (refreshed, _) = plx_data::metadata::fetch_playing_item(client, rig.sid, "1", None)
            .expect("the refreshed item lists the newly available sidecar");
        assert_eq!(refreshed.subs.first().map(|sub| sub.id), Some(3));
        rig.app.bridge.metadata_mut().run(
            plx_data::stores::metadata::MetadataCmd::InstallPlaying(Some(refreshed)));
        let before = rig.app.player.session.metric_request();
        rig.pick_subtitle(3);
        assert!(plx_media::route::play_pending(),
            "a sidecar is a source reconfiguration because native URI cannot draw it");
        assert_ne!(rig.app.player.session.metric_request(), before);
        assert!(matches!(rig.app.player.session.attempt_origin(),
            plx_media::player::attempt_metrics::Origin::ViewerReconfigure));
        let mut replacement = None;
        poll_until("external subtitle resolve did not land", || {
            replacement = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            replacement.is_some()
        });
        assert!(replacement.unwrap().uri_offer.is_none(),
            "a client sidecar cannot be certified by the original native URI output");
        assert_eq!(rig.app.player.session.source_selection().expect("source intent").subtitle_sid, 3,
            "the Feed successor must keep the sidecar SID, not treat it as subtitle Off");
    }

    #[cfg(feature = "play-feed")]
    #[test]
    fn force_audio_pick_missing_from_uri_map_qualifies_only_the_exact_original_source() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving_with(|request| {
            queue_body(request, false).replace(
                r#"{"id":2,"streamType":2,"codec":"aac","selected":true}"#,
                r#"{"id":2,"streamType":2,"codec":"aac","selected":true},{"id":3,"streamType":2,"codec":"ac3","channels":6}"#)
        }) else { return };
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
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, port, bus, display));
        rig.request();
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("qualified URI source did not resolve", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session,
            resolved.unwrap().uri_offer.expect("qualified original source")).is_ok());
        assert!(matches!(plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player),
            plx_media::player::BufferfeedStartOutcome::Launched(_)));
        let poll = |rig: &mut Rig| {
            let at = rig.app.adapters.player.backend_tick();
            let _ = rig.app.adapters.player.backend_poll_session_at(&mut rig.app.player.session, at);
        };
        poll(&mut rig);
        bus.say("load", r#"{"returnValue":true,"mediaId":"m1","loadCompleted":true}"#);
        poll(&mut rig);
        bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":0}}"#);
        poll(&mut rig);
        bus.say("play", r#"{"returnValue":true,"mediaId":"m1","playing":true,"currentTime":12000}"#);
        poll(&mut rig);
        plx_media::route::set_direct_play_mode_for_test(&mut rig.app.player.session,
            plx_media::route::DirectPlayMode::Forced);
        rig.pick_audio(3);
        assert!(plx_media::route::play_pending());
        let mut replacement = None;
        poll_until("Force selected source did not resolve", || {
            replacement = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            replacement.is_some()
        });
        let replacement = replacement.unwrap();
        assert_eq!(replacement.resume_ns, 12_000_000_000);
        assert_eq!(rig.app.player.session.source_selection().expect("source intent").audio_sid, 3);
        assert_eq!(replacement.uri_offer.expect("exact original source remains qualified").offer_id(),
            plx_media::route::backend_policy::OfferId::URI_ORIGINAL,
            "Force may choose the selected original URI; it cannot convert or pick sibling audio");
    }

    #[cfg(feature = "play-feed")]
    #[test]
    fn uri_track_pick_without_a_known_playhead_cannot_restart_the_source_at_zero() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving_with(|request| {
            queue_body(request, false).replace(
                r#"{"id":2,"streamType":2,"codec":"aac","selected":true}"#,
                r#"{"id":2,"streamType":2,"codec":"aac","selected":true},{"id":3,"streamType":2,"codec":"ac3","channels":6}"#)
        }) else { return };
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
        assert!(crate::app::adapters::backends::install_uri_product_with_ports(
            &mut rig.app.adapters.player, &mut rig.app.player.session, port, bus, display));
        rig.request();
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("qualified URI source did not resolve", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session,
            resolved.unwrap().uri_offer.expect("qualified original source")).is_ok());
        assert!(matches!(plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player),
            plx_media::player::BufferfeedStartOutcome::Launched(_)));
        let at = rig.app.adapters.player.backend_tick();
        let _ = rig.app.adapters.player.backend_poll_session_at(&mut rig.app.player.session, at);
        assert!(bus.load_payload().is_some(), "the URI attempt owns an active Load");
        assert_eq!(plx_media::player::intended_pos_known_ns(&rig.app.player.session), None,
            "neither dispatch nor Load is a content position");
        let request = rig.app.player.session.metric_request();
        rig.pick_audio(3);
        assert!(!plx_media::route::play_pending(),
            "without a known playhead the source cannot be re-resolved at a guessed zero");
        assert_eq!(rig.app.player.session.metric_request(), request);
        assert_eq!(rig.app.player.session.source_selection().expect("source intent").audio_sid, 2);
    }

    // ---- the exported window: never two live at once (the webOS 5+ plan, PR 2) -----------------

    /// The display the URI driver is given in the window rig. Its window verbs go to the host
    /// seam's `HOST_SINK`, the sink Feed's hooks run over in this rig (`backends::TEST_HOOKS`), so
    /// ONE instrument (`plx_play_feed::ffi_host::window_counts_for_test`, the C seam's single window
    /// slot) counts the windows the URI driver makes and the windows Feed makes. Its plane verbs are
    /// `Display`'s: refused.
    struct CountingDisplay;
    impl DisplaySink for CountingDisplay {
        fn window_mode(&self) -> c_int { crate::app::adapters::hostsim::HOST_SINK.window_mode() }
        fn window_id(&self) -> &'static CStr { crate::app::adapters::hostsim::HOST_SINK.window_id() }
        unsafe fn create_window(&self, mt: &plx_base::task::MainThread) -> *const c_char {
            crate::app::adapters::hostsim::HOST_SINK.create_window(mt)
        }
        unsafe fn place_window(&self, mt: &plx_base::task::MainThread, src_w: c_int, src_h: c_int,
            dst_x: c_int, dst_y: c_int, dst_w: c_int, dst_h: c_int) -> c_int {
            crate::app::adapters::hostsim::HOST_SINK
                .place_window(mt, src_w, src_h, dst_x, dst_y, dst_w, dst_h)
        }
        unsafe fn destroy_window(&self, mt: &plx_base::task::MainThread) {
            crate::app::adapters::hostsim::HOST_SINK.destroy_window(mt)
        }
        unsafe fn plane_create(&self, mt: &plx_base::task::MainThread,
            app_id: *const c_char, player_type: c_int) -> c_long {
            NoSink.plane_create(mt, app_id, player_type)
        }
        unsafe fn plane_initialized(&self, mt: &plx_base::task::MainThread) -> bool {
            NoSink.plane_initialized(mt)
        }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_dass_available(&self, mt: &plx_base::task::MainThread) -> bool {
            NoSink.plane_probe_dass_available(mt)
        }
        unsafe fn plane_media_id(&self, mt: &plx_base::task::MainThread,
            id: *const c_char) -> c_int { NoSink.plane_media_id(mt, id) }
        unsafe fn plane_loaded(&self, mt: &plx_base::task::MainThread) -> c_int {
            NoSink.plane_loaded(mt)
        }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_connect_dass(&self, mt: &plx_base::task::MainThread) -> c_int {
            NoSink.plane_probe_connect_dass(mt)
        }
        unsafe fn plane_finish(&self, mt: &plx_base::task::MainThread)
            -> plx_platform::tv::sink::PlaneFinish { NoSink.plane_finish(mt) }
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
        unsafe fn plane_unload(&self, mt: &plx_base::task::MainThread)
            -> plx_platform::tv::sink::PlaneUnload { NoSink.plane_unload(mt) }
        fn plane_completions(&self, mt: &plx_base::task::MainThread)
            -> Vec<plx_platform::tv::sink::PlaneCompletion> { NoSink.plane_completions(mt) }
        unsafe fn plane_pause(&self, mt: &plx_base::task::MainThread) { NoSink.plane_pause(mt) }
        unsafe fn plane_resume(&self, mt: &plx_base::task::MainThread) { NoSink.plane_resume(mt) }
    }

    /// Arms the host seam's exported-window path (`VP_EXPORTED`, `vp_create_window` answering) and
    /// zeroes the window instrument; disarms and zeroes it again on drop, panic or not. Declare it
    /// BEFORE the rig so the rig's own teardown still sees the armed seam.
    struct ExportedSeam;
    impl ExportedSeam {
        fn arm() -> Self {
            plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
            plx_play_feed::ffi_host::force_clocksink_for_test(true);
            Self
        }
    }
    impl Drop for ExportedSeam {
        fn drop(&mut self) {
            plx_play_feed::ffi_host::force_clocksink_for_test(false);
            plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        }
    }

    /// The URI driver's clock in the window rig, advanced by the test.
    struct StepClock(Arc<std::sync::atomic::AtomicU64>);
    impl plx_play_uri::backend::MonotonicClock for StepClock {
        fn now_ms(&self) -> u64 { self.0.load(Ordering::Relaxed) }
    }

    /// The driver's startup and release allowances in the window rig.
    const WINDOW_STARTUP_MS: u64 = 1_000;
    const WINDOW_RELEASE_MS: u64 = 500;

    /// Compose the URI driver as the boot does (`install_uri_product_with_ports`: the opted-in
    /// viewer's capabilities over an exported canvas), but on `clock` with short allowances, over
    /// `display`, so a startup timeout and an unreleased close are reachable by moving the clock.
    fn install_exported_uri(rig: &mut Rig, bus: &'static Bus, display: &'static CountingDisplay,
        clock: Arc<std::sync::atomic::AtomicU64>) {
        let caps = Capabilities { progressive_uri: true, hls_uri: false,
            packet_feed: false, pause: true, seek: true, audio_track: true,
            subtitle_track: true, audio_off: false, subtitle_off: false };
        let port = ResolvePort::new(ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) },
            callable: caps, certified: caps, preview_rectangle: false },
            Arc::new(QualifiedOriginal));
        let evidence = port.evidence();
        let settings = crate::app::adapters::backends::UriDriverSettings {
            clock: Box::new(StepClock(clock)),
            app_id: plx_base::paths::app_id().to_owned(),
            binding: evidence.binding,
            capabilities: evidence.opted_in().certified,
            startup_timeout_ms: WINDOW_STARTUP_MS,
            release_allowance_ms: WINDOW_RELEASE_MS,
            source_ttl_ms: 30_000,
        };
        assert!(crate::app::adapters::backends::install_uri_backend_with_ports(
            &mut rig.app.adapters.player, settings, bus, display));
        rig.app.player.session.install_uri_preflight_port(port);
    }

    /// One frame of the app loop's playback half, as the other URI fixtures run it.
    fn window_frame(rig: &mut Rig) {
        let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
        unsafe { playback_tick(&mut rig.app, &mut fr); }
    }

    /// Resolve the rig's pending request to its URI offer and start it on the URI driver.
    fn launch_uri(rig: &mut Rig) {
        rig.release.send(()).unwrap();
        let mut resolved = None;
        poll_until("qualified URI source did not resolve", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        assert!(rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session,
            resolved.unwrap().uri_offer.expect("qualified original source")).is_ok());
        assert!(matches!(plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player),
            plx_media::player::BufferfeedStartOutcome::Launched(_)));
    }

    fn windows() -> plx_play_feed::ffi_host::WindowCounts {
        plx_play_feed::ffi_host::window_counts_for_test()
    }

    /// The URI driver's startup times out and its close completes: the SAME request's Feed resolve
    /// begins only once the URI window is destroyed (`ReleaseStatus::Released`), and not while the
    /// registration is still closing. This pins that ordering through the real adapter and driver
    /// with the instrument; it does not run Feed's own start, which is where Feed makes its window.
    #[test]
    fn a_timed_out_uri_start_destroys_its_live_window_before_the_feed_resolve() {
        let _serial = plx_base::testlock::serial();
        let _seam = ExportedSeam::arm();
        let Some(mut rig) = Rig::serving(false) else { return };
        plx_plex::plex::client_for(rig.sid).unwrap()
            .set_link(plx_plex::plex::probe::Location::Local);
        let bus: &'static Bus = Box::leak(Box::new(Bus::new()));
        let display: &'static CountingDisplay = Box::leak(Box::new(CountingDisplay));
        let clock = Arc::new(std::sync::atomic::AtomicU64::new(0));
        install_exported_uri(&mut rig, bus, display, clock.clone());
        rig.request();
        launch_uri(&mut rig);
        window_frame(&mut rig); // registration opens
        let one = plx_play_feed::ffi_host::WindowCounts { creates: 1, destroys: 0, live: 1, peak: 1, slot: true };
        assert_eq!(windows(), one, "the URI driver made its window at its start");
        // The service never answers `load`: past the startup budget the attempt fails and closes.
        clock.store(WINDOW_STARTUP_MS, Ordering::Relaxed);
        window_frame(&mut rig);
        assert!(bus.0.lock().unwrap().1 == CloseStatus::Closing, "the timed-out attempt closes");
        assert_eq!(windows(), one, "a closing registration still holds its window");
        assert!(!plx_media::route::play_pending(), "no handoff before the exact release");
        bus.release();
        window_frame(&mut rig);
        assert_eq!(windows(), plx_play_feed::ffi_host::WindowCounts {
            creates: 1, destroys: 1, live: 0, peak: 1, slot: false },
            "the confirmed release destroyed the URI window");
        assert!(plx_media::route::play_pending(), "the release starts the same-episode Feed resolve");
        let mut replacement = None;
        poll_until("automatic Feed rescue did not resolve", || {
            replacement = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            replacement.is_some()
        });
        assert!(replacement.unwrap().uri_offer.is_none(), "automatic rescue cannot loop back to URI");
        // The successor's own `Launched` start is not run here: under the armed seam its demux
        // worker outlives the rig and lands its failure in the NEXT test's route control (the
        // sibling fixtures in this module then fail). So Feed's window create is not counted by
        // this test; what it pins is the ordering above.
    }

    /// A request while the previous request's URI registration is quarantined (its close never
    /// completed inside the allowance), so the driver still holds its exported window: the new
    /// request must not be handed to Feed or to the URI driver again, because either would create a
    /// second window over the one the quarantined driver cannot destroy. The C seam's single slot
    /// would be overwritten and the first window orphaned (`src/video_plane.c`), so the instrument's
    /// peak must stay at one whatever the player does with the request.
    #[test]
    fn a_new_request_over_a_quarantined_uri_driver_makes_no_second_live_window() {
        let _serial = plx_base::testlock::serial();
        let _seam = ExportedSeam::arm();
        let Some(mut rig) = Rig::serving(false) else { return };
        plx_plex::plex::client_for(rig.sid).unwrap()
            .set_link(plx_plex::plex::probe::Location::Local);
        let bus: &'static Bus = Box::leak(Box::new(Bus::new()));
        let display: &'static CountingDisplay = Box::leak(Box::new(CountingDisplay));
        let clock = Arc::new(std::sync::atomic::AtomicU64::new(0));
        install_exported_uri(&mut rig, bus, display, clock.clone());
        rig.request();
        launch_uri(&mut rig);
        window_frame(&mut rig); // registration opens: the URI driver owns its window
        clock.store(WINDOW_STARTUP_MS, Ordering::Relaxed);
        window_frame(&mut rig); // startup timed out: the close begins and is never confirmed
        assert!(bus.0.lock().unwrap().1 == CloseStatus::Closing);
        clock.store(WINDOW_STARTUP_MS + WINDOW_RELEASE_MS, Ordering::Relaxed);
        window_frame(&mut rig); // the allowance ran out: the registration is quarantined
        assert_eq!(rig.app.adapters.player.backend_snapshot().phase,
            plx_playback_contract::Phase::Quarantined, "premise: the URI release is quarantined");
        let held = plx_play_feed::ffi_host::WindowCounts { creates: 1, destroys: 0, live: 1, peak: 1, slot: true };
        assert_eq!(windows(), held, "premise: the quarantined driver still holds its window");
        // The viewer asks for the title again.
        assert!(plx_media::route::request_play(&mut rig.app.player.session,
            rig.app.bridge.metadata_mut(), plx_platform::devcaps::caps(), rig.sid, "1",
            "/library/parts/1/file.mkv", "h264", "aac", "First", "", None, ""));
        let mut resolved = None;
        poll_until("the new request did not resolve", || {
            resolved = plx_media::route::pump_play(&mut rig.app.player.session,
                rig.app.bridge.metadata_mut());
            resolved.is_some()
        });
        if let Some(offer) = resolved.unwrap().uri_offer {
            let _ = rig.app.adapters.player.offer_qualified_uri(&mut rig.app.player.session, offer);
        }
        let started = plx_media::player::start_playback_tracked(
            &mut rig.app.player.session, &mut rig.app.adapters.player);
        window_frame(&mut rig);
        window_frame(&mut rig);
        assert_eq!(windows(), held, "the new request made another window (start outcome {started:?})");
        assert!(matches!(started, plx_media::player::BufferfeedStartOutcome::Failed),
            "the start over the quarantined registration is refused, not launched: {started:?}");
    }
}
