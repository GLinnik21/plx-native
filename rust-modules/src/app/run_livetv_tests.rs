// Live lifecycle regressions use the existing loopback PMS rig and real app event/stop path.
fn live_body(request: &str) -> String {
    if request.contains("/tune?") {
        return r#"{"MediaContainer":{"key":"/livetv/sessions/12345678","ratingKey":"live-airing"}}"#.into();
    }
    if request.contains("/decision?") {
        return r#"{"MediaContainer":{"Metadata":[{"Media":[{"videoCodec":"h264","audioCodec":"aac","width":1280,"height":720,"audioChannels":2,"Part":[{"key":"/livetv/sessions/12345678/stream/index.m3u8"}]}]}]}}"#.into();
    }
    r#"{"MediaContainer":{}}"#.into()
}

fn tuned_live(rig: &mut Rig) -> plx_machine::machine::EntryId {
    let origin = rig.app.pages.nav.top_page().unwrap().id;
    assert!(plx_media::route::request_live(&mut rig.app.player.session,
        rig.app.bridge.metadata_mut(), plx_platform::devcaps::caps(), rig.sid, "7", "C101", "Family Channel"));
    rig.entered.recv_timeout(Duration::from_secs(3)).unwrap();
    rig.accept_start();
    frame(&mut rig.app, 16);
    rig.release.send(()).unwrap();
    poll_until("live fixture plan did not land", || {
        plx_media::route::pump_play(&mut rig.app.player.session, rig.app.bridge.metadata_mut()).is_some()
    });
    assert!(plx_media::route::is_live(&rig.app.player.session));
    assert!(plx_media::route::has_url(&rig.app.player.session));
    assert_eq!(super::super::bridge::player(&rig.app.pages).unwrap().origin.unwrap().entry, origin);
    origin
}

fn live_stop_count(rig: &Rig) -> usize {
    rig.requests.lock().unwrap().iter()
        .filter(|request| request.contains("/:/timeline?") && request.contains("state=stopped")).count()
}

#[test]
fn background_while_live_stops_the_tuner_and_returns_to_the_original_entry() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("live-background");
    let Some(mut rig) = Rig::serving_with(live_body) else {
        eprintln!("SKIPPED background_while_live_stops_the_tuner_and_returns_to_the_original_entry: fixture worker refused");
        return;
    };
    let origin = tuned_live(&mut rig);
    let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
    event(&mut rig.app, &mut fr, 0x103);
    event(&mut rig.app, &mut fr, 0x104);
    assert!(!plx_media::route::is_live(&rig.app.player.session));
    assert!(!rig.app.player.lifecycle.awaiting_load());
    assert_eq!(rig.app.player.lifecycle.state(), ForegroundState::Idle);
    plx_media::route::drain_scrobble();
    assert_eq!(live_stop_count(&rig), 1, "both background edges release one tuner exactly once");
    event(&mut rig.app, &mut fr, 0x105);
    event(&mut rig.app, &mut fr, 0x106);
    let mut now = 32;
    settle(&mut rig.app, &mut now);
    assert_eq!(rig.app.pages.nav.top_page().unwrap().id, origin);
    assert!(super::super::bridge::player(&rig.app.pages).is_none());
    assert!(!rig.app.adapters.player.is_live());
}

#[test]
fn background_after_leaving_live_keeps_the_page_the_viewer_chose() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("live-left-background");
    let Some(mut rig) = Rig::serving_with(live_body) else {
        eprintln!("SKIPPED background_after_leaving_live_keeps_the_page_the_viewer_chose: fixture worker refused");
        return;
    };
    tuned_live(&mut rig);
    player_requests(&mut rig.app.player, &mut rig.app.adapters.player, &mut rig.app.repause_at,
        vec![plx_screens::registry::PlayerReq::Exit], 32, &mut rig.app.refresh_hubs,
        &mut rig.app.pages, &mut rig.app.bridge, &mut rig.app.ok_armed, &mut rig.app.input.press);
    let mut now = 32;
    settle(&mut rig.app, &mut now);
    assert!(!plx_media::route::is_live(&rig.app.player.session));
    super::super::bridge::show_page(&mut rig.app.pages, AppArg::Library);
    settle(&mut rig.app, &mut now);
    let chosen = rig.app.pages.nav.top_page().unwrap().id;
    assert!(rig.app.route() == AppArg::Library);
    let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
    for edge in [0x103, 0x104, 0x105, 0x106] { event(&mut rig.app, &mut fr, edge); }
    settle(&mut rig.app, &mut now);
    assert_eq!(rig.app.pages.nav.top_page().unwrap().id, chosen, "a retired live request navigated away on background");
    assert!(rig.app.route() == AppArg::Library);
    plx_media::route::drain_scrobble();
    assert_eq!(live_stop_count(&rig), 1);
}
