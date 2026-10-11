//! The menu-play season wait: Play on a card whose season's episode list is not loaded yet.
//!
//! Every test drives `menu_play_tick` inside `task::FrameScope::enter()` and never opens an
//! `allow_blocking` exception, so a season fetch issued ON the frame trips `assert_may_block` in
//! `http::request_with`. The season rides the same async mailbox the season tabs use, against a
//! one-thread loopback server.

use super::*;
use plx_data::metadata::{Detail, Season};
use plx_data::stores::metadata::MetadataCmd;
use plx_plex::plex::ServerId;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const SHOW: &str = "show-1";

/// Serves every connection a two-episode `/children` answer until dropped. Dropping it stops and
/// joins the thread, so a test that never dials still ends.
struct Pms {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    port: i32,
}

impl Pms {
    fn serve() -> Pms {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port() as i32;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                let Ok((mut conn, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    continue;
                };
                conn.set_nonblocking(false).unwrap();
                let mut buf = [0u8; 2048];
                let _ = conn.read(&mut buf);
                let body = r#"{"MediaContainer":{"size":2,"Metadata":[
                    {"ratingKey":"ep-first","type":"episode","title":"First"},
                    {"ratingKey":"ep-second","type":"episode","title":"Second"}]}}"#;
                let _ = write!(
                    conn,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
            }
        });
        Pms { stop, thread: Some(thread), port }
    }
}

impl Drop for Pms {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Rig {
    ps: plx_media::route::PlaybackSession,
    pa: plx_media::player::adapter::PlayerAdapter,
    pages: plx_ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: super::bridge::Bridge,
    wait: Option<MenuPlayAwait>,
    sid: ServerId,
    _guard: plx_base::testlock::Serial,
}

impl Rig {
    /// A show whose season 1 (`cur_season` 0, one stale episode) is listed; the press asks for
    /// season 2, which is not. `port` names the PMS the server slot dials.
    fn new(port: i32) -> Rig {
        let guard = plx_base::testlock::serial();
        // Under the lock: libcurl's one-time init and its capability probes are not for several
        // test threads at once.
        assert!(plx_net::net::global_init() && plx_net::net::available() && plx_net::net::threaded_tls_ready());
        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut bridge = super::bridge::Bridge::for_test(|| 0);
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("menu-season", "127.0.0.1", port, "t", "c-menu-season");
        plx_plex::plex::client_for(sid).unwrap().set_link(plx_plex::plex::probe::Location::Local);
        let season = |rk: &str, index| Season {
            rk: rk.into(), index, title: format!("Season {index}"), leaf_count: 0, viewed_leaf_count: 0 };
        plx_data::metadata::set_current_for_test(bridge.metadata_mut().state_mut(), Some(Detail {
            sid,
            rk: SHOW.into(),
            is_show: true,
            kind: "show".into(),
            seasons: vec![season("sk1", 1), season("sk2", 2)],
            cur_season: 0,
            episodes: vec![plx_data::metadata::Episode { rk: "stale".into(), ..Default::default() }].into(),
            ..Default::default()
        }));
        Rig {
            ps: plx_media::route::PlaybackSession::default(),
            pa: plx_media::player::adapter::PlayerAdapter::new(mt),
            pages: plx_ui::dispatch::Dispatcher::<super::bridge::AppHost>::new(),
            bridge,
            wait: Some(MenuPlayAwait::new(sid, SHOW.into(), Some(2), 1000, 0)),
            sid,
            _guard: guard,
        }
    }

    /// One run-loop frame's worth of the wait, exactly as `app/run.rs` drives it: inside a
    /// `FrameScope`, with no exception open.
    fn tick(&mut self, now: u32) {
        let _frame = plx_base::task::FrameScope::enter();
        unsafe {
            menu_play_tick(&mut self.ps, &mut self.pa, &mut self.pages, &mut self.bridge, &mut self.wait, now);
        }
    }

    /// Pump the season mailbox until a landing lands on the page, or give up after ~5 s (the
    /// `wall` gate keeps clock reads out of `app/`, so the bound is a count of 2 ms naps).
    fn pump_until_landed(&mut self) -> bool {
        for _ in 0..2500 {
            if self.bridge.metadata_pump_season() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        false
    }

    /// Wait until no season fetch is in flight (a failed one settles without a page change).
    fn pump_until_settled(&mut self) {
        for _ in 0..2500 {
            if !self.bridge.metadata_view().season_loading() {
                return;
            }
            self.bridge.metadata_pump_season();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    fn episodes(&self) -> Vec<String> {
        self.bridge.metadata_view().current()
            .map(|d| d.episodes.iter_loaded().map(|(_, e)| e.rk.clone()).collect()).unwrap_or_default()
    }

    fn played(&self) -> Option<String> {
        self.bridge.metadata_view().now_playing().map(|n| n.ep_title.clone())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.bridge.metadata_mut().run(MetadataCmd::Clear);
        plx_plex::plex::reset_servers_for_test();
    }
}

/// THE regression: the retired arm (`MetadataCmd::LoadSeasonNow`) fetched `/children` ON the frame,
/// under `allow_blocking("menu-play season load")`, and played `episodes[0]` on the same statement.
/// The new contract: the frame returns with the wait still armed and the season in flight on a
/// worker; the landing plays the REQUESTED season's first episode.
#[test]
fn an_unloaded_season_is_fetched_off_the_frame_and_its_first_episode_plays_on_landing() {
    let pms = Pms::serve();
    let mut rig = Rig::new(pms.port);

    rig.tick(0);
    assert!(rig.wait.is_some(), "the wait must outlive the frame that issued the season fetch");
    assert!(rig.bridge.metadata_view().season_loading(), "the season is in flight on a worker");
    assert_eq!(rig.episodes(), ["stale"], "nothing was fetched on the frame");
    assert_eq!(rig.played(), None, "nothing plays before the season lands");

    assert!(rig.pump_until_landed(), "the season must land through the async mailbox");
    rig.tick(16);
    assert!(rig.wait.is_none(), "the landing ends the wait");
    assert_eq!(rig.episodes(), ["ep-first", "ep-second"]);
    assert_eq!(rig.played().as_deref(), Some("First"), "episodes[0] of the requested season plays");
    assert!(rig.pages.has_pending_navigation(), "and the player is entered");
}

/// A landing the wait no longer owns plays nothing: another season press (the generation moved)
/// replaced the fetch this wait was for.
#[test]
fn a_superseded_season_landing_plays_nothing() {
    let pms = Pms::serve();
    let mut rig = Rig::new(pms.port);

    rig.tick(0);
    rig.bridge.metadata_mut().run(MetadataCmd::LoadSeason(0)); // the user's own tab press
    assert!(rig.pump_until_landed());
    rig.tick(16);

    assert!(rig.wait.is_none(), "a wait whose fetch was superseded is dropped");
    assert_eq!(rig.played(), None, "a stale landing must not start playback");
    assert!(!rig.pages.has_pending_navigation(), "and must not navigate anywhere either");
}

/// BACK (or any navigation) while the season is loading: the page's `Clear` drops the item, so the
/// wait has nothing to play and ends without a trace.
#[test]
fn leaving_the_show_during_the_season_wait_cancels_it() {
    let pms = Pms::serve();
    let mut rig = Rig::new(pms.port);

    rig.tick(0);
    assert!(rig.wait.is_some());
    rig.bridge.metadata_mut().run(MetadataCmd::Clear);
    rig.pump_until_settled();
    rig.tick(16);

    assert!(rig.wait.is_none());
    assert_eq!(rig.played(), None);
    assert!(!rig.pages.has_pending_navigation());
}

/// A failed `/children` fetch ends the wait like a failed detail fetch does: no play, land on the
/// page. (The tab is released back to the season whose episodes are listed, so nothing plays under
/// the wrong season's name.)
#[test]
fn a_failed_season_fetch_opens_the_page_instead_of_playing() {
    let mut rig = Rig::new(1); // refused port: the worker's GET fails
    rig.tick(0);
    assert!(rig.wait.is_some());
    rig.pump_until_settled();
    rig.tick(16);

    assert!(rig.wait.is_none(), "a settled failure ends the wait");
    assert_eq!(rig.played(), None, "the stale list's first episode must not play");
    assert!(rig.pages.has_pending_navigation(), "the show's page opens instead");
    assert_eq!(rig.bridge.metadata_view().current().map(|d| d.cur_season), Some(0));
    let _ = rig.sid;
}

/// The 12s ceiling that bounds the detail phase bounds the season phase too.
#[test]
fn the_ceiling_ends_a_season_wait_that_never_lands() {
    let pms = Pms::serve();
    let mut rig = Rig::new(pms.port);

    rig.tick(0);
    assert!(rig.wait.is_some());
    rig.tick(12_001);
    assert!(rig.wait.is_none(), "past the ceiling the wait gives up");
    assert_eq!(rig.played(), None);
    assert!(rig.pages.has_pending_navigation(), "and lands on the page");
}

/// The requested season is already the listed one: no fetch at all, the press plays what is on
/// the page (season 1 is `cur_season` 0 in the rig).
#[test]
fn an_already_listed_season_plays_without_a_fetch() {
    let mut rig = Rig::new(1);
    rig.wait = Some(MenuPlayAwait::new(rig.sid, SHOW.into(), Some(1), 1000, 0));
    rig.tick(0);
    assert!(rig.wait.is_none());
    assert!(!rig.bridge.metadata_view().season_loading(), "no season request was issued");
    assert_eq!(rig.episodes(), ["stale"]);
    assert!(rig.pages.has_pending_navigation(), "the listed hero starts playing");
}
