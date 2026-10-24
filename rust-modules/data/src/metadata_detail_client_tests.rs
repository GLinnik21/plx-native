//! The detail chain runs through the client it was handed (`finish_detail_fetch`), never through the
//! registry: every GET of `fetch_full` but the opening season's episodes (`fetch_episodes`, which
//! still resolves its own client), and the one `fetch_playing_item` makes, goes to the server the
//! runner resolved for the job, whatever the registry says by the time each GET is made.
//!
//! The rig is two loopback PMSes. The slot is registered against `A` and its client taken, as the
//! runner does at the admission; then the registry moves on (the slot re-pointed at `B`, or emptied
//! by `reset_servers_for_test`, which neither raises the floor nor blanks the token, so this rig
//! does not model a sign-out) and the chain runs with the client it already holds. A chain that
//! asked the registry for a client in any sub-fetch would reach `B` (or none), and the recorded
//! paths say so. The loopback PMS ignores `X-Plex-Token`, so these tests pin the routing, not that
//! a chain survives the event: a real re-point or sign-out blanks the held client's token in place,
//! and a real PMS refuses the tokenless GETs that follow.

use super::*;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

/// A loopback PMS that answers the pages below and records the path of every request it gets.
struct Pms {
    port: i32,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Pms {
    /// Every path asked of this server so far, sorted: the extras and the related GETs run on two
    /// threads, so their order is not the chain's.
    fn paths(&self) -> Vec<String> {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        seen.sort();
        seen
    }
}

const EMPTY: &str = r#"{"MediaContainer":{"size":0}}"#;

fn container(rows: &[String]) -> String {
    format!(r#"{{"MediaContainer":{{"size":{0},"totalSize":{0},"Metadata":[{1}]}}}}"#, rows.len(), rows.join(","))
}

/// A trailer row of `/extras`: no Part, so the picker finds nothing playable and the chain pays the
/// ONE follow-up metadata GET (`resolve_trailer`).
fn bare_trailer(rk: &str) -> String {
    format!(r#"{{"ratingKey":"{rk}","type":"clip","subtype":"trailer","title":"Trailer"}}"#)
}

/// The trailer's own page, with the Part that makes it playable.
fn playable_trailer(rk: &str) -> String {
    container(&[format!(
        r#"{{"ratingKey":"{rk}","type":"clip","subtype":"trailer","Media":[{{"Part":[{{"key":"/library/parts/{rk}"}}]}}]}}"#
    )])
}

/// `(status, body)` for a path. `movie-1` names its trailer through `/extras`; `movie-2`'s `/extras`
/// is refused and its `primaryExtraKey` names the trailer; `show-1` has one season of two episodes.
fn route(path: &str) -> (u16, String) {
    let path = path.split('?').next().unwrap_or(path);
    let ok = |body: String| (200, body);
    match path {
        "/library/metadata/movie-1" => ok(container(&[
            r#"{"ratingKey":"movie-1","type":"movie","title":"Movie One"}"#.to_string()
        ])),
        "/library/metadata/movie-1/extras" => ok(container(&[bare_trailer("trailer-1")])),
        "/library/metadata/trailer-1" => ok(playable_trailer("trailer-1")),
        "/library/metadata/movie-2" => ok(container(&[
            r#"{"ratingKey":"movie-2","type":"movie","title":"Movie Two","primaryExtraKey":"/library/metadata/trailer-2"}"#
                .to_string(),
        ])),
        "/library/metadata/movie-2/extras" => (404, String::new()),
        "/library/metadata/trailer-2" => ok(playable_trailer("trailer-2")),
        "/library/metadata/show-1" => ok(container(&[
            r#"{"ratingKey":"show-1","type":"show","title":"Show"}"#.to_string()
        ])),
        "/library/metadata/show-1/children" => ok(container(&[
            r#"{"ratingKey":"sk1","type":"season","index":1,"title":"Season 1"}"#.to_string(),
        ])),
        "/library/metadata/sk1/children" => ok(container(&[
            r#"{"ratingKey":"s1e1","type":"episode","index":1,"parentIndex":1}"#.to_string(),
            r#"{"ratingKey":"s1e2","type":"episode","index":2,"parentIndex":1}"#.to_string(),
        ])),
        "/library/metadata/s1e1" => ok(container(&[
            r#"{"ratingKey":"s1e1","type":"episode","index":1,"parentIndex":1}"#.to_string()
        ])),
        _ => ok(EMPTY.to_string()),
    }
}

fn serve() -> Pms {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (record, halt) = (Arc::clone(&seen), Arc::clone(&stop));
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
            let target = request.split_whitespace().nth(1).unwrap_or("/");
            let path = target.split('?').next().unwrap_or(target).to_string();
            record.lock().unwrap_or_else(|e| e.into_inner()).push(path);
            let (status, body) = route(target);
            let _ = write!(
                conn,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
        }
    });
    Pms { port, seen, stop, thread: Some(thread) }
}

impl Drop for Pms {
    fn drop(&mut self) {
        self.stop.store(true, AtomicOrdering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Two servers, the slot registered against the first, and the client the runner would have
/// resolved for a job on it. The registry is emptied again when the rig drops.
struct Rig {
    a: Pms,
    b: Pms,
    sid: plx_plex::plex::ServerId,
    /// The client for `a`, taken before the registry moves on.
    client: &'static plx_plex::plex::Client,
}

impl Rig {
    fn new() -> Self {
        assert!(plx_net::net::global_init() && plx_net::net::available() && plx_net::net::threaded_tls_ready());
        plx_plex::plex::reset_servers_for_test();
        let (a, b) = (serve(), serve());
        let sid = plx_plex::plex::register_for_test("detail-client", "127.0.0.1", a.port, "token-a", "detail-client-a");
        let client = plx_plex::plex::client_for(sid).expect("the slot just registered");
        client.set_link(plx_plex::plex::probe::Location::Local);
        Rig { a, b, sid, client }
    }

    /// The registry moves on: the same slot now answers with a client for the second server (a
    /// re-registration at another address). It also blanks the first client's token in place, as a
    /// real re-point does, which the loopback PMS does not look at.
    fn repoint(&self) {
        let again = plx_plex::plex::register_for_test("detail-client", "127.0.0.1", self.b.port, "token-b", "detail-client-b");
        assert_eq!(again, self.sid, "the same slot");
        let now = plx_plex::plex::client_for(self.sid).expect("the slot still resolves");
        assert!(!std::ptr::eq(now, self.client), "and it holds a different client now");
        now.set_link(plx_plex::plex::probe::Location::Local);
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        plx_plex::plex::reset_servers_for_test();
    }
}

fn paths(rows: &[&str]) -> Vec<String> {
    rows.iter().map(|p| p.to_string()).collect()
}

#[test]
fn a_movie_chain_keeps_the_client_it_was_handed_when_the_slot_is_repointed() {
    let _serial = plx_base::testlock::serial();
    let rig = Rig::new();
    rig.repoint();
    let d = fetch_full(rig.client, rig.sid, "movie-1", None).expect("the first server answers the page");
    assert_eq!(d.title, "Movie One");
    assert_eq!(d.trailer_rk, "trailer-1", "the follow-up GET that fills the trailer's Part went the same way");
    assert_eq!(
        rig.a.paths(),
        paths(&[
            "/library/metadata/movie-1",
            "/library/metadata/movie-1/extras",
            "/library/metadata/movie-1/related",
            "/library/metadata/trailer-1",
        ]),
        "every GET of the chain reached the server the client was resolved for",
    );
    assert_eq!(rig.b.paths(), Vec::<String>::new(), "and none reached the server the registry holds now");
}

#[test]
fn a_chain_whose_extras_were_refused_asks_the_primary_trailer_through_the_same_client() {
    let _serial = plx_base::testlock::serial();
    let rig = Rig::new();
    rig.repoint();
    let d = fetch_full(rig.client, rig.sid, "movie-2", None).expect("the first server answers the page");
    assert_eq!(d.trailer_rk, "trailer-2", "the refused /extras fell back to primaryExtraKey");
    assert_eq!(
        rig.a.paths(),
        paths(&[
            "/library/metadata/movie-2",
            "/library/metadata/movie-2/extras",
            "/library/metadata/movie-2/related",
            "/library/metadata/trailer-2",
        ]),
    );
    assert_eq!(rig.b.paths(), Vec::<String>::new());
}

#[test]
fn a_show_chain_keeps_the_client_it_was_handed_except_for_its_opening_season() {
    let _serial = plx_base::testlock::serial();
    let rig = Rig::new();
    rig.repoint();
    let d = fetch_full(rig.client, rig.sid, "show-1", None).expect("the first server answers the page");
    assert_eq!((d.seasons.len(), d.is_show), (1, true));
    assert_eq!(
        rig.a.paths(),
        paths(&[
            "/library/metadata/s1e1",
            "/library/metadata/show-1",
            "/library/metadata/show-1/children",
            "/library/metadata/show-1/extras",
            "/library/metadata/show-1/related",
        ]),
        "the item, its seasons, the stream backfill's episode, its extras and its related",
    );
    // The opening season's episodes are the one read the chain still resolves for itself:
    // `fetch_episodes` is shared with the season worker, which has not moved to a job yet. When it
    // does, this list is empty, as the movie's is.
    assert_eq!(rig.b.paths(), paths(&["/library/metadata/sk1/children"]));
}

#[test]
fn an_emptied_registry_neither_redirects_nor_skips_the_chain() {
    let _serial = plx_base::testlock::serial();
    let rig = Rig::new();
    plx_plex::plex::reset_servers_for_test();
    assert!(plx_plex::plex::client_for(rig.sid).is_none(), "setup: the registry holds nothing for the slot");
    let d = fetch_full(rig.client, rig.sid, "movie-1", None).expect("the chain finishes with the client it holds");
    assert_eq!(d.title, "Movie One");
    assert_eq!(d.trailer_rk, "trailer-1", "the last GET of the chain is made as well");
    assert_eq!(rig.a.paths().len(), 4, "all four GETs were made");
}

#[test]
fn the_playing_item_is_read_through_the_callers_client() {
    let _serial = plx_base::testlock::serial();
    let rig = Rig::new();
    rig.repoint();
    let (item, played) = fetch_playing_item(rig.client, rig.sid, "movie-1", None).expect("an item for a key");
    assert!(played.is_none(), "no version was picked");
    assert_eq!(item.rk, "movie-1");
    assert_eq!(rig.a.paths(), paths(&["/library/metadata/movie-1"]));
    assert_eq!(rig.b.paths(), Vec::<String>::new());
}
