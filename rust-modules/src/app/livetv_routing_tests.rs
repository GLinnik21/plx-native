//! Live TV through the mounted dispatcher and the loop's real request drain. These tests stop at
//! the accepted playback request: no device backend or PMS client is installed for a channel.

use super::*;
use crate::app::{run::{lifecycle_regression_tests::app, Frame}, App};
use plx_data::livetv::{Channel, LiveTvStatus};
use plx_machine::machine::{InputEvent, Key, Stamped};
use plx_screens::registry::{ContentReq, HomeReq, PlayIntent};
use plx_ui::dispatch::Tap;

fn channel(vcn: &str, name: &str) -> Channel {
    Channel { sid: plx_plex::plex::ServerId::from_raw(0), dvr: "7".into(),
        channel: format!("C{vcn}.242.ersatztv.org"), vcn: vcn.into(), name: name.into(),
        thumb: String::new(), now: "Family Show".into(), now_thumb: String::new(), ends_at: 0 }
}

#[derive(Default)]
struct Observed {
    plays: Vec<PlayIntent>,
    enters: usize,
}
impl Tap<AppHost> for Observed {
    fn effect(&mut self, _frame: u64, effect: &Stamped<AppHost>) {
        match &effect.fx {
            Fx::App(AppFx::Content(ContentReq::Play { play, .. })) => self.plays.push(play.clone()),
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(_))) => self.enters += 1,
            _ => {}
        }
    }
}

fn step(app: &mut App, now: &mut u32, inputs: Vec<InputEvent<u32>>) -> Observed {
    *now += 16;
    let mut observed = Observed::default();
    super::frame_with_tap(&mut app.pages, &mut app.bridge,
        Tick { ms: *now, dt_us: 16_000 }, inputs, &mut observed);
    let mut frame = Frame::begin(&app.player.session, app.bridge.metadata_view());
    frame.now = *now;
    crate::app::content::content_requests(app, &mut frame);
    app.bridge.assert_no_store_jobs_left_for_test();
    observed
}

fn settle(app: &mut App, now: &mut u32) {
    for _ in 0..24 { step(app, now, Vec::new()); }
}

fn mounted(app: &App) -> MachineId {
    MachineId::Instance(app.pages.top_page().expect("the asking page is mounted"))
}

fn open_live(app: &mut App, now: &mut u32) {
    app.bridge.stores.livetv.install_for_test(vec![channel("101", "Family Channel"), channel("102", "Cartoons")]);
    super::nav_root(&mut app.pages, AppArg::LiveTv);
    settle(app, now);
    assert!(app.route() == AppArg::LiveTv);
}

fn activate(app: &mut App, now: &mut u32, key: u32) {
    let source = mounted(app);
    app.pages.emit(source, Fx::Deliver(source, Delivery::Screen(ScreenEvent::Activate(key))));
    settle(app, now);
}

fn player_request(app: &mut App, now: u32, req: plx_screens::registry::PlayerReq) {
    crate::app::playback::player_requests(
        &mut app.player, &mut app.adapters.player, &mut app.repause_at, vec![req], now,
        &mut app.refresh_hubs, &mut app.pages, &mut app.bridge, &mut app.ok_armed, &mut app.input.press,
    );
}

fn start_channel(app: &mut App, now: &mut u32) {
    let ch = &app.bridge.livetv_view().channels[0];
    let intent = PlayIntent::Live { sid: ch.sid, dvr: ch.dvr.clone(), channel: ch.channel.clone(), title: ch.name.clone() };
    assert!(crate::app::content::request_play_intent(&mut app.player.session, app.bridge.metadata_mut(), &intent));
    assert!(crate::app::playback::start_playback(&mut app.player.session, &mut app.adapters.player,
        0, crate::app::playback::Origin::Here, 4_500, None, &mut app.pages, &mut app.bridge));
    settle(app, now);
    assert!(app.route() == AppArg::Player);
}

#[test]
fn channel_requests_wrap_the_lineup_and_keep_the_original_return_entry() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("livetv-player-channel");
    plx_plex::plex::reset_servers_for_test();
    let mut app = app();
    let mut now = 0;
    open_live(&mut app, &mut now);
    // An equal DVR/channel on a different server cannot steal the current channel's identity.
    let mut other = channel("101", "Other server");
    other.sid = plx_plex::plex::ServerId::from_raw(1);
    app.bridge.stores.livetv.install_for_test(vec![channel("101", "Family Channel"), channel("102", "Cartoons"), other]);
    let origin_entry = app.pages.nav.top_page().unwrap().id;
    start_channel(&mut app, &mut now);
    let player_entry = app.pages.nav.top_page().unwrap().id;
    let origin = super::player(&app.pages).unwrap().origin.unwrap();
    assert_eq!(origin.entry, origin_entry);
    for (direction, sid, title) in [(1, 0, "Cartoons"), (1, 1, "Other server"), (1, 0, "Family Channel"), (-1, 1, "Other server")] {
        app.repause_at = 50;
        player_request(&mut app, now, plx_screens::registry::PlayerReq::Channel(direction));
        settle(&mut app, &mut now);
        let (actual, _) = plx_media::route::live_request(&app.player.session).unwrap();
        assert_eq!(actual.raw(), sid);
        // SAFETY: the session owns the NUL-terminated banner for this assertion.
        assert_eq!(unsafe { std::ffi::CStr::from_ptr(plx_media::route::title_cptr(&app.player.session)) }.to_str().unwrap(), title);
        assert_eq!(app.repause_at, -1, "the old channel's pending pause was retired");
        assert_eq!(app.pages.nav.top_page().unwrap().id, player_entry);
        assert_eq!(super::player(&app.pages).unwrap().origin.unwrap().entry, origin_entry);
    }
    player_request(&mut app, now, plx_screens::registry::PlayerReq::Exit);
    settle(&mut app, &mut now);
    assert_eq!(app.pages.nav.top_page().unwrap().id, origin_entry);
    assert!(!plx_media::route::is_live(&app.player.session));
}

#[test]
fn channel_request_on_nonlive_and_an_empty_or_single_lineup_keeps_the_session() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("livetv-player-no-neighbour");
    plx_plex::plex::reset_servers_for_test();
    let mut app = app();
    let mut now = 0;
    open_live(&mut app, &mut now);
    player_request(&mut app, now, plx_screens::registry::PlayerReq::Channel(1));
    assert!(!plx_media::route::is_live(&app.player.session));
    start_channel(&mut app, &mut now);
    for channels in [vec![channel("101", "Family Channel")], Vec::new()] {
        app.bridge.stores.livetv.install_for_test(channels);
        player_request(&mut app, now, plx_screens::registry::PlayerReq::Channel(1));
        assert_eq!(plx_media::route::live_request(&app.player.session).unwrap().1.channel, "C101.242.ersatztv.org");
    }
    player_request(&mut app, now, plx_screens::registry::PlayerReq::Exit);
}

#[test]
fn live_banner_uses_the_existing_caption_and_tracks_the_airing() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("livetv-player-banner");
    plx_plex::plex::reset_servers_for_test();
    let mut app = app();
    let mut now = 0;
    open_live(&mut app, &mut now);
    start_channel(&mut app, &mut now);
    for (number, programme, expected) in [("101", "Family Show", "101 · Family Show"),
        ("101", "Next Show", "101 · Next Show"), ("", "Next Show", "Next Show"), ("101", "", "101"), ("", "", "")] {
        let mut ch = channel(number, "Family Channel");
        ch.channel = "C101.242.ersatztv.org".into();
        ch.now = programme.into();
        crate::app::playback::publish_live_banner(&mut app.player.session, &[ch]);
        // SAFETY: the session owns the NUL-terminated banner for this assertion.
        assert_eq!(unsafe { std::ffi::CStr::from_ptr(plx_media::route::ctxline_cptr(&app.player.session)) }.to_str().unwrap(), expected);
    }
    player_request(&mut app, now, plx_screens::registry::PlayerReq::Exit);
}

#[test]
fn a_channel_ok_reaches_the_real_content_drain_and_live_playback_request() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("livetv-content-play");
    plx_plex::plex::reset_servers_for_test();
    let mut app = app();
    let mut now = 0;
    open_live(&mut app, &mut now);
    assert!(!plx_media::route::is_live(&app.player.session));
    assert_eq!(app.pages.focus().map(|k| k.elem), Some(0x1000));
    let keys = super::script_key(Key::Right, Tick { ms: now + 16, dt_us: 16_000 });
    step(&mut app, &mut now, keys);
    settle(&mut app, &mut now);
    assert_eq!(app.pages.focus().map(|k| k.elem), Some(0x1002));
    let keys = super::script_key(Key::Ok, Tick { ms: now + 16, dt_us: 16_000 });
    let mut observed = step(&mut app, &mut now, keys);
    // The real press machine commits after the release animation, on a later frame.
    for _ in 0..24 { observed.plays.extend(step(&mut app, &mut now, Vec::new()).plays); }
    assert_eq!(observed.plays, vec![PlayIntent::Live {
        sid: plx_plex::plex::ServerId::from_raw(0), dvr: "7".into(),
        channel: "C102.242.ersatztv.org".into(), title: "Cartoons".into(),
    }]);
    assert!(plx_media::route::is_live(&app.player.session), "the loop accepted the channel's real Play request");
    // SAFETY: the session owns this NUL-terminated HUD title for the duration of the assertion.
    assert_eq!(unsafe { std::ffi::CStr::from_ptr(plx_media::route::title_cptr(&app.player.session)) }.to_str().unwrap(), "Cartoons");
}

#[test]
fn a_covered_live_tv_page_cannot_start_a_delayed_play() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("livetv-covered-play");
    plx_plex::plex::reset_servers_for_test();
    let mut app = app();
    let mut now = 0;
    open_live(&mut app, &mut now);
    let source = mounted(&app);
    activate(&mut app, &mut now, plx_ui::widgets::TAB_KEY_CHIP);
    assert!(super::account_menu_up(&app.pages));
    let MachineId::Instance(instance) = source else { unreachable!() };
    let entry = app.pages.nav.entry_of_instance(instance).expect("the covered page stays mounted");
    assert_ne!(app.pages.nav.input_owner(), Some(InputOwner::Entry(entry)));
    app.pages.emit(source, Fx::App(AppFx::Content(ContentReq::Play { play: PlayIntent::Live {
        sid: plx_plex::plex::ServerId::from_raw(0), dvr: "7".into(),
        channel: "C101.242.ersatztv.org".into(), title: "Family Channel".into(),
    }, resume_ns: 0 })));
    let observed = step(&mut app, &mut now, Vec::new());
    assert_eq!(observed.plays.len(), 1, "the stale request reached the real drain");
    assert!(!plx_media::route::is_live(&app.player.session));
    assert!(app.route() == AppArg::LiveTv);
    assert!(super::account_menu_up(&app.pages));
}

#[test]
fn live_tv_uses_the_real_home_request_lane_for_tabs_back_and_the_profile_surface() {
    use plx_ui::widgets::{TAB_KEY_CHIP, TAB_KEY_LIVETV, TAB_KEY_SEARCH};
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("livetv-home-requests");
    plx_plex::plex::reset_servers_for_test();
    let mut app = app();
    let mut now = 0;
    open_live(&mut app, &mut now);
    let live_instance = mounted(&app);
    activate(&mut app, &mut now, TAB_KEY_LIVETV);
    assert_eq!(mounted(&app), live_instance, "the current TV pill keeps the mounted page");
    app.pages.emit(live_instance, Fx::App(AppFx::Home(HomeReq::Tab(HomeTab::LiveTv))));
    settle(&mut app, &mut now);
    assert_eq!(mounted(&app), live_instance, "the request drain also treats the current tab as a no-op");
    activate(&mut app, &mut now, TAB_KEY_SEARCH);
    assert!(app.route() == AppArg::Search, "a TV strip request is accepted by the loop");

    open_live(&mut app, &mut now);
    for _ in 0..4 {
        if app.route() == AppArg::Home { break; }
        let keys = super::script_key(Key::Back, Tick { ms: now + 16, dt_us: 16_000 });
        step(&mut app, &mut now, keys);
        settle(&mut app, &mut now);
    }
    assert!(app.route() == AppArg::Home, "BACK leaves through the mounted page's Home request");

    open_live(&mut app, &mut now);
    activate(&mut app, &mut now, TAB_KEY_CHIP);
    assert!(super::account_menu_up(&app.pages));
    assert!(app.route() == AppArg::LiveTv, "the profile is a surface over the same page");
}

#[test]
fn home_only_requests_from_live_tv_are_discarded() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("livetv-home-only-requests");
    plx_plex::plex::reset_servers_for_test();
    let mut app = app();
    let mut now = 0;
    open_live(&mut app, &mut now);
    app.bridge.stores.hubs.seed_for_test(1, plx_data::pms::HubState::Ready);
    let snapshot = app.bridge.hubs_snapshot();
    let item = snapshot.view().hub(0).expect("a seeded Home hub").items[0].clone();
    let source = mounted(&app);
    for request in [HomeReq::FoldToHero,
        HomeReq::Play { sid: item.sid, rk: item.rk.clone(), resume_ns: 0 },
        HomeReq::Detail { sid: item.sid, rk: item.rk.clone() },
        HomeReq::ItemMenu { sid: item.sid, rk: item.rk.clone() }] {
        app.pages.emit(source, Fx::App(AppFx::Home(request)));
        step(&mut app, &mut now, Vec::new());
        assert!(!app.pages.has_pending_navigation(), "a Home-only request starts no navigation");
        let after = step(&mut app, &mut now, Vec::new());
        assert_eq!(after.enters, 0, "Home's fold must not reseat the TV page");
        assert!(app.route() == AppArg::LiveTv);
        assert_eq!(mounted(&app), source);
        assert!(!super::account_menu_up(&app.pages));
        assert!(!plx_media::route::is_live(&app.player.session));
    }
}

#[test]
fn a_library_pill_reopens_a_remembered_library_page_over_its_grid() {
    use plx_plex::plex::session::LibraryPage;
    let mut world = super::test_support::World::movies_library("livetv-remembered-library-page", false);
    let key = world.bridge.directory.view().sections()[0].key;
    world.bridge.remember_library_page("route-own", key, LibraryPage::Recommended);
    let mut app = app();
    app.pages = std::mem::take(&mut world.d);
    std::mem::swap(&mut app.bridge, &mut world.bridge);
    let mut now = world.frame * 16;
    open_live(&mut app, &mut now);
    activate(&mut app, &mut now, plx_ui::widgets::TAB_KEY_MOVIES);
    let want = AppArg::LibraryRecommended { section: u32::try_from(key).unwrap() };
    assert!(app.route() == want, "the real request drain uses the remembered library destination");
    let stack = &app.pages.nav.tabs.stack.entries;
    assert!(stack.len() >= 2);
    assert!(stack[stack.len() - 2].arg == AppArg::Library);
    assert!(stack[stack.len() - 1].arg == want);
    let keys = super::script_key(Key::Back, Tick { ms: now + 16, dt_us: 16_000 });
    step(&mut app, &mut now, keys);
    settle(&mut app, &mut now);
    assert!(app.route() == AppArg::Library, "BACK from the remembered child reveals its grid");
}

#[test]
fn the_live_tv_tab_tracks_the_store_and_an_open_empty_page_stays_open() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("livetv-store-availability");
    plx_plex::plex::reset_servers_for_test();
    let mut app = app();
    let mut now = 0;
    assert!(!app.bridge.search_tab_available(HomeTab::LiveTv));
    open_live(&mut app, &mut now);
    assert!(app.bridge.search_tab_available(HomeTab::LiveTv));
    let instance = mounted(&app);
    app.bridge.stores.livetv.install_for_test(Vec::new());
    settle(&mut app, &mut now);
    assert!(!app.bridge.search_tab_available(HomeTab::LiveTv));
    assert_eq!(app.bridge.livetv_view().status, LiveTvStatus::Empty);
    assert!(app.route() == AppArg::LiveTv);
    assert_eq!(mounted(&app), instance, "withdrawing the tab does not evict its open page");
    app.bridge.livetv_run(plx_data::livetv::LiveTvCmd::Reset);
    assert!(!app.bridge.search_tab_available(HomeTab::LiveTv));
}
