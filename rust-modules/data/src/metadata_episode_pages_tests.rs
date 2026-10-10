//! A long season's episodes page in as the screen reaches them and out as it leaves, against a
//! loopback server that honours `X-Plex-Container-Start/Size` and counts what it was asked.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use crate::stores::page_cache::MAX_LOADED;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

/// A `/children` server for one season of `total` episodes named `e0..`. `honour_paging = false`
/// answers every request with the whole season from index 0, as a server that ignores the window.
struct Season {
    port: i32,
    asked: Arc<Mutex<Vec<(usize, usize)>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn param(request: &str, name: &str) -> Option<usize> {
    let query = request.split_once('?')?.1.split_whitespace().next()?;
    query.split('&').find_map(|kv| kv.strip_prefix(name)?.strip_prefix('=')?.parse().ok())
}

fn serve(total: usize, honour_paging: bool) -> Season {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (log, halt) = (asked.clone(), stop.clone());
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
            let start = param(&request, "X-Plex-Container-Start").unwrap_or(0);
            let size = param(&request, "X-Plex-Container-Size").unwrap_or(usize::MAX);
            log.lock().unwrap().push((start, size));
            let (from, to) = if honour_paging { (start, (start.saturating_add(size)).min(total)) } else { (0, total) };
            let rows: Vec<String> = (from.min(total)..to)
                .map(|i| format!(r#"{{"ratingKey":"e{i}","type":"episode","title":"E{i}","index":{}}}"#, i + 1))
                .collect();
            let body = format!(
                r#"{{"MediaContainer":{{"size":{},"totalSize":{total},"offset":{from},"Metadata":[{}]}}}}"#,
                rows.len(),
                rows.join(","),
            );
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
        }
    });
    Season { port, asked, stop, thread: Some(thread) }
}

impl Season {
    fn requests(&self) -> usize {
        self.asked.lock().unwrap().len()
    }
}

impl Drop for Season {
    fn drop(&mut self) {
        self.stop.store(true, AtomicOrdering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        plx_plex::plex::reset_servers_for_test();
    }
}

/// The show page of `sid` with the first page of season `sk1` as `fetch_episodes` lands it.
fn open_season(server: &Season) -> plx_plex::plex::ServerId {
    assert!(plx_net::net::global_init() && plx_net::net::available() && plx_net::net::threaded_tls_ready());
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("episode-pages", "127.0.0.1", server.port, "token", "episode-pages-client");
    plx_plex::plex::client_for(sid).unwrap().set_link(plx_plex::plex::probe::Location::Local);
    install_show_on(sid, "show-1", 0, &[]);
    let first = fetch_episodes(sid, "sk1").expect("page 0 answers");
    test_state().current.as_mut().unwrap().episodes = first;
    sid
}

fn list() -> &'static EpisodeList {
    &test_state().current.as_ref().unwrap().episodes
}

fn want(lo: usize, hi: usize, focus: Option<usize>, restore: Option<usize>) {
    let keep = crate::stores::page_cache::Keep { wanted: lo..hi, focus, restore };
    want_episodes(test_state(), test_adapter(), keep);
}

/// Pump until `done` holds (a bounded wait, so a page that never arrives fails rather than hangs).
fn settle(done: impl Fn() -> bool) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !done() && std::time::Instant::now() < until {
        pump_episode_pages(test_state(), test_adapter());
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(done(), "the wanted page never landed");
}

fn show(i: usize) -> Option<String> {
    list().get(i).map(|e| e.rk.clone())
}

/// A walk over 1,000 episodes loads pages as it reaches them and never holds more than
/// `MAX_LOADED`; walking back refetches the evicted pages and every index shows the same episode.
#[test]
fn a_thousand_episodes_are_walked_with_at_most_eight_pages_loaded() {
    let _serial = plx_base::testlock::serial();
    let server = serve(1000, true);
    open_season(&server);
    assert_eq!(list().len(), 1000);
    assert_eq!(list().loaded_pages(), 1, "the season lands as page 0 only");
    assert_eq!(server.requests(), 1);

    for lo in (0..1000).step_by(30) {
        want(lo, (lo + 30).min(1000), Some(lo), None);
        settle(|| show(lo).is_some() && show((lo + 29).min(999)).is_some());
        assert!(list().loaded_pages() <= MAX_LOADED, "{} pages loaded at {lo}", list().loaded_pages());
        assert_eq!(show(lo), Some(format!("e{lo}")));
    }
    assert_eq!(show(999).as_deref(), Some("e999"), "the last episode is reachable");
    let forward = server.requests();
    assert!(forward <= 1 + 17, "each page is fetched about once on the way: {forward}");

    // Back to the middle: those pages were evicted, so they are asked for again, and it is the same data.
    assert!(show(190).is_none(), "page 3 was evicted on the walk");
    want(180, 210, Some(190), None);
    settle(|| show(190).is_some());
    assert!(server.requests() > forward, "an evicted page is fetched again");
    for i in 180..210 {
        assert_eq!(show(i), Some(format!("e{i}")));
    }
    assert!(list().loaded_pages() <= MAX_LOADED);
}

/// Opening at episode 700 fetches around it (not the 700 before it) and keeps page 0.
#[test]
fn a_restore_onto_episode_700_loads_around_it() {
    let _serial = plx_base::testlock::serial();
    let server = serve(1000, true);
    open_season(&server);
    want(690, 720, None, Some(700));
    settle(|| show(700).is_some() && show(719).is_some());
    assert_eq!(show(700).as_deref(), Some("e700"));
    assert!(list().loaded_pages() <= 3, "only the pages around the target: {}", list().loaded_pages());
    assert!(show(0).is_some(), "page 0 stays");
    assert!(show(300).is_none(), "nothing between was fetched");
}

/// A server that ignores the window answers with the whole season; the wanted page is kept and the
/// rest dropped, so memory stays bounded by the page directory, not by what the server sent.
#[test]
fn a_server_that_ignores_paging_still_gives_the_wanted_page() {
    let _serial = plx_base::testlock::serial();
    let server = serve(130, false);
    open_season(&server);
    assert_eq!(list().len(), 130);
    assert_eq!(list().loaded_pages(), 1);
    assert_eq!(show(59).as_deref(), Some("e59"));
    assert!(show(60).is_none(), "page 0 is the first 60 rows, not the whole answer");
    want(60, 120, Some(60), None);
    settle(|| show(60).is_some());
    assert_eq!(show(60).as_deref(), Some("e60"));
    assert_eq!(show(119).as_deref(), Some("e119"));
    assert!(show(120).is_none());
}

/// A season that fits in one page is one request, a complete list, and the plain array on the wire.
#[test]
fn a_ten_episode_season_is_one_request_and_a_plain_array() {
    let _serial = plx_base::testlock::serial();
    let server = serve(10, true);
    open_season(&server);
    assert_eq!(server.requests(), 1);
    assert!(list().is_complete());
    want(0, 10, Some(0), None);
    assert_eq!(server.requests(), 1, "nothing to fetch");
    let json = serde_json::to_value(list()).unwrap();
    assert_eq!(json.as_array().map(Vec::len), Some(10));
}

/// The page holding the focus, and the one a pending restore will seat on, are not evicted while
/// the wanted range is elsewhere.
#[test]
fn the_focused_page_survives_eviction() {
    let mut list = EpisodeList::with_total(1000);
    for page in 0..12 {
        let rows = (page * 60..(page + 1) * 60).map(|i| episode(&format!("e{i}"))).collect();
        list.set_page(page, rows);
    }
    let rev = list.rev();
    list.evict(&crate::stores::page_cache::Keep { wanted: 660..700, focus: Some(70), restore: Some(310) });
    assert!(list.rev() > rev, "an eviction is a change");
    assert!(list.get(70).is_some(), "the focused page stays");
    assert!(list.get(310).is_some(), "the restore target's page stays");
    assert!(list.get(0).is_some(), "so does page 0");
    assert!(list.get(660).is_some(), "and the wanted range");
    assert!(list.get(200).is_none(), "the rest goes");
    assert!(list.loaded_pages() <= MAX_LOADED);
    // The list a screen reads back is the same episode at the same index, evicted or not.
    assert_eq!(list.get(70).map(|e| e.rk.as_str()), Some("e70"));
}

/// A restore target outside the range on screen is still fetched, without anything between.
#[test]
fn a_restore_target_outside_the_wanted_range_is_fetched() {
    let _serial = plx_base::testlock::serial();
    let server = serve(1000, true);
    open_season(&server);
    want(0, 30, Some(0), Some(700));
    settle(|| show(700).is_some());
    assert_eq!(show(700).as_deref(), Some("e700"));
    assert!(show(300).is_none());
}

/// An episode watch flip on an evicted index is a no-op, not a panic.
#[test]
fn a_watch_flip_on_an_evicted_episode_does_nothing() {
    let _serial = plx_base::testlock::serial();
    let server = serve(1000, true);
    open_season(&server);
    assert!(!test_state().current.as_mut().unwrap().episodes.edit("e500", |e| e.watched = true));
    assert!(!set_watched_local(test_state(), test_state().current.as_ref().unwrap().sid, "e500", true));
}
