//! Which season a show's page opens on: the server's on-deck season on a first open, the season
//! the viewer has selected on a re-read of the same page, the first season when nothing says
//! otherwise — and the episodes of THAT season in the same fetch.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use crate::stores::metadata::MetadataCmd;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;

fn season(index: i64) -> Season {
    Season { rk: format!("sk{index}"), index, title: format!("Season {index}"), leaf_count: 0, viewed_leaf_count: 0 }
}

fn on_deck(season: i64, index: i64) -> Episode {
    Episode { rk: format!("s{season}e{index}"), index, season, ..Default::default() }
}

#[test]
fn the_opening_season_follows_the_on_deck_episode() {
    let seasons: Vec<Season> = (1..=20).map(season).collect();
    assert_eq!(initial_season(&seasons, Some(&on_deck(12, 14)), None), 11, "season 12 of 20 is position 11");
    assert_eq!(initial_season(&seasons, Some(&on_deck(1, 1)), None), 0, "an untouched show's S1E1 is the first tab anyway");
    assert_eq!(initial_season(&seasons, None, None), 0, "no on-deck (never started, finished, omitted): the first season");
}

#[test]
fn an_on_deck_season_the_list_does_not_hold_opens_the_first() {
    let seasons: Vec<Season> = (1..=3).map(season).collect();
    assert_eq!(initial_season(&seasons, Some(&on_deck(9, 2)), None), 0);
    assert_eq!(initial_season(&[], Some(&on_deck(2, 2)), None), 0, "and an empty list has only a position 0 to name");
}

#[test]
fn seasons_are_matched_by_number_never_by_position() {
    // Specials is season 0 and the list skips 3: season 4 sits at position 3, not at 4.
    let seasons: Vec<Season> = [0, 1, 2, 4, 5].into_iter().map(season).collect();
    assert_eq!(initial_season(&seasons, Some(&on_deck(4, 1)), None), 3);
    assert_eq!(initial_season(&seasons, Some(&on_deck(0, 1)), None), 0, "the Specials season is a season");
    assert_eq!(initial_season(&seasons, None, Some(5)), 4);
}

#[test]
fn the_selected_season_outranks_the_on_deck_episode() {
    let seasons: Vec<Season> = (1..=6).map(season).collect();
    assert_eq!(initial_season(&seasons, Some(&on_deck(6, 2)), Some(4)), 3);
    assert_eq!(initial_season(&seasons, Some(&on_deck(6, 2)), Some(40)), 5, "a number the list lost falls back to the on-deck season");
    assert_eq!(initial_season(&seasons, None, Some(40)), 0);
}

// ---- the fetch, against a loopback PMS ---------------------------------------------------------

/// A PMS that answers a show page with seasons 1..=4, an on-deck episode in season 3 (episode 14 of
/// 20) and a season's `/children` for each `sk`. Everything else is an empty container.
struct Pms {
    port: i32,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn route(path: &str) -> String {
    let path = path.split('?').next().unwrap_or(path);
    let rows = match path {
        "/library/metadata/show-1" => {
            return concat!(
                r#"{"MediaContainer":{"size":1,"Metadata":[{"ratingKey":"show-1","type":"show","title":"Show","#,
                r#""OnDeck":{"Metadata":{"ratingKey":"s3e14","type":"episode","index":14,"parentIndex":3}}}]}}"#
            )
            .to_string()
        }
        "/library/metadata/show-1/children" => (1..=4)
            .map(|n| format!(r#"{{"ratingKey":"sk{n}","type":"season","index":{n},"title":"Season {n}"}}"#))
            .collect::<Vec<_>>(),
        p if p.starts_with("/library/metadata/sk") && p.ends_with("/children") => {
            let n = &p["/library/metadata/sk".len()..p.len() - "/children".len()];
            (1..=20)
                .map(|e| format!(r#"{{"ratingKey":"s{n}e{e}","type":"episode","index":{e},"parentIndex":{n}}}"#))
                .collect()
        }
        _ => return r#"{"MediaContainer":{"size":0}}"#.to_string(),
    };
    format!(r#"{{"MediaContainer":{{"size":{0},"totalSize":{0},"Metadata":[{1}]}}}}"#, rows.len(), rows.join(","))
}

fn serve() -> Pms {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    let stop = Arc::new(AtomicBool::new(false));
    let halt = stop.clone();
    let thread = std::thread::spawn(move || {
        while !halt.load(AtomicOrdering::SeqCst) {
            let Ok((mut conn, _)) = listener.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            };
            conn.set_nonblocking(false).unwrap();
            let mut buf = [0u8; 4096];
            let n = conn.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            let body = route(path);
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
        }
    });
    Pms { port, stop, thread: Some(thread) }
}

impl Drop for Pms {
    fn drop(&mut self) {
        self.stop.store(true, AtomicOrdering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        plx_plex::plex::reset_servers_for_test();
    }
}

fn open(server: &Pms) -> plx_plex::plex::ServerId {
    assert!(plx_net::net::global_init() && plx_net::net::available() && plx_net::net::threaded_tls_ready());
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("initial-season", "127.0.0.1", server.port, "token", "initial-season-client");
    plx_plex::plex::client_for(sid).unwrap().set_link(plx_plex::plex::probe::Location::Local);
    sid
}

fn first_episode_rk(d: &Detail) -> Option<String> {
    d.episodes.get(0).map(|e| e.rk.clone())
}

/// Request `show-1` through the store's own command and land it, as a page open or a re-read does.
fn read_show(sid: plx_plex::plex::ServerId) {
    run(test_state(), test_adapter(), MetadataCmd::RequestDetail { sid, rk: "show-1".into() });
    assert_eq!(test_adapter().run_held_detail_fetches_for_test(), 1);
    assert!(pump_detail(test_state(), test_adapter()), "the read landed");
}

#[test]
fn a_show_with_an_on_deck_episode_in_season_three_opens_on_season_three() {
    let _serial = plx_base::testlock::serial();
    let server = serve();
    let sid = open(&server);
    let d = fetch_full(sid, "show-1", None).expect("the show page answers");
    assert_eq!(d.cur_season, 2, "season 3 is the third tab");
    assert_eq!(d.seasons[d.cur_season].index, 3);
    assert_eq!(first_episode_rk(&d).as_deref(), Some("s3e1"), "and its episodes are the ones loaded, not season 1's");
    assert_eq!(d.episodes.len(), 20);
    assert_eq!(d.on_deck.as_ref().map(|e| e.rk.as_str()), Some("s3e14"));
}

#[test]
fn a_first_open_through_the_store_lands_on_the_on_deck_season() {
    let _serial = plx_base::testlock::serial();
    let server = serve();
    let sid = open(&server);
    test_state().current = None;
    read_show(sid);
    let d = current(test_state()).expect("landed");
    assert_eq!((d.cur_season, first_episode_rk(d).as_deref()), (2, Some("s3e1")));
}

#[test]
fn a_re_read_of_the_same_page_keeps_the_season_the_viewer_is_on() {
    let _serial = plx_base::testlock::serial();
    let server = serve();
    let sid = open(&server);
    test_state().current = None;
    read_show(sid);
    // The viewer moves on to season 4 (the optimistic flip `load_season` makes, then its landing).
    test_state().current.as_mut().unwrap().cur_season = 3;

    // A watched toggle / a return from the player re-reads the page that is already loaded.
    read_show(sid);
    let d = current(test_state()).expect("landed");
    assert_eq!(d.cur_season, 3, "the tab did not jump back to the on-deck season");
    assert_eq!(first_episode_rk(d).as_deref(), Some("s4e1"), "and the episodes listed are season 4's");
}

#[test]
fn another_shows_selected_season_does_not_steer_this_shows_first_open() {
    let _serial = plx_base::testlock::serial();
    let server = serve();
    let sid = open(&server);
    install_show_on(sid, "other-show", 1, &[]);
    assert_eq!(selected_season_number(test_state(), sid, "other-show"), Some(2));
    assert_eq!(selected_season_number(test_state(), sid, "show-1"), None);
    read_show(sid);
    assert_eq!(current(test_state()).map(|d| d.cur_season), Some(2));
}
