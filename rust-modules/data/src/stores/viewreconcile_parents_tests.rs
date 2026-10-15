//! The read-back of a leaf also reads its season and its show: the counts those rows hold moved
//! with the leaf, and a Library grid, a Search shelf or a Home card holding them must say so.
//!
//! The fake server holds one show `1`, its season `2` and two episodes `3` and `4`. Its counts
//! (`leafCount`, `viewedLeafCount`) follow the watched set, as a real server's do.

use super::*;
use crate::pms::PmsMovie;
use crate::search::{Item, Kind, Shelf};
use crate::stores::{browse::DirectorySnapshot, viewstate::ViewStateCmd, Stores};
use crate::viewstate::Write;
use std::collections::HashSet;
use std::io::{Read, Write as _};

#[derive(Default)]
struct Truth {
    watched: Mutex<HashSet<String>>,
    /// The `ratingKey` list of every `/library/metadata/…` request, in arrival order.
    requests: Mutex<Vec<String>>,
}

struct Pms {
    port: i32,
    truth: Arc<Truth>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn reply(conn: &mut std::net::TcpStream, body: &str) {
    let _ = write!(conn, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
}

fn row(truth: &Truth, rk: &str) -> String {
    let viewed = ["3", "4"].iter().filter(|e| truth.watched.lock().unwrap().contains(**e)).count();
    match rk {
        "m" => r#"{"ratingKey":"m","type":"movie","title":"M"}"#.to_string(),
        "1" => format!(r#"{{"ratingKey":"1","type":"show","title":"S","leafCount":2,"viewedLeafCount":{viewed}}}"#),
        "2" => format!(r#"{{"ratingKey":"2","type":"season","title":"S1","index":1,"parentRatingKey":"1","leafCount":2,"viewedLeafCount":{viewed}}}"#),
        ep => {
            let count = if truth.watched.lock().unwrap().contains(ep) { r#","viewCount":1"# } else { "" };
            format!(r#"{{"ratingKey":"{ep}","type":"episode","title":"E","index":1,"parentIndex":1,"parentRatingKey":"2","grandparentRatingKey":"1"{count}}}"#)
        }
    }
}

fn serve_connection(mut conn: std::net::TcpStream, truth: Arc<Truth>) {
    let mut buf = [0u8; 4096];
    let n = conn.read(&mut buf).unwrap_or(0);
    let request = String::from_utf8_lossy(&buf[..n]).into_owned();
    let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
    if let Some(query) = path.strip_prefix("/:/scrobble?").or_else(|| path.strip_prefix("/:/unscrobble?")) {
        let key = query.split('&').find_map(|kv| kv.strip_prefix("key=")).unwrap_or("").to_string();
        if path.starts_with("/:/scrobble") { truth.watched.lock().unwrap().insert(key); } else { truth.watched.lock().unwrap().remove(&key); }
        return reply(&mut conn, "{}");
    }
    if path.starts_with("/library/all?") {
        return reply(&mut conn, r#"{"MediaContainer":{"size":0}}"#);
    }
    if let Some(list) = path.strip_prefix("/library/metadata/") {
        let list = list.split('?').next().unwrap_or("").to_string();
        truth.requests.lock().unwrap().push(list.clone());
        let rows: Vec<String> = list.split(',').map(|rk| row(&truth, rk)).collect();
        return reply(&mut conn, &format!(r#"{{"MediaContainer":{{"size":{},"Metadata":[{}]}}}}"#, rows.len(), rows.join(",")));
    }
    reply(&mut conn, r#"{"MediaContainer":{"size":0}}"#);
}

fn serve() -> Pms {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    let (truth, stop) = (Arc::new(Truth::default()), Arc::new(AtomicBool::new(false)));
    let (t, halt) = (truth.clone(), stop.clone());
    let thread = std::thread::spawn(move || {
        while !halt.load(Ordering::SeqCst) {
            let Ok((conn, _)) = listener.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            };
            conn.set_nonblocking(false).unwrap();
            let truth = t.clone();
            std::thread::spawn(move || serve_connection(conn, truth));
        }
    });
    Pms { port, truth, stop, thread: Some(thread) }
}

impl Drop for Pms {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() { let _ = t.join(); }
        plx_plex::plex::reset_servers_for_test();
    }
}

/// A Stores whose Library grid holds rows `1`..`4` (show, season, episode, episode): none watched.
fn library(pms: &Pms) -> (Stores, ServerId) {
    assert!(plx_net::net::global_init() && plx_net::net::available() && plx_net::net::threaded_tls_ready());
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("parents", "127.0.0.1", pms.port, "token", "parents-client");
    plx_plex::plex::client_for(sid).unwrap().set_link(plx_plex::plex::probe::Location::Local);
    let stores = Stores::default();
    stores.browse.borrow_mut().seed_registered_table_for_test([sid, sid]);
    stores.browse.borrow_mut().seed_items_for_test(4);
    (stores, sid)
}

fn grid_watched(stores: &Stores, index: usize) -> bool {
    let mut browse = stores.browse.borrow_mut();
    let snapshot = browse.listing_snapshot();
    snapshot.view().item(index).unwrap().watched
}

fn press(stores: &mut Stores, sid: ServerId, rk: &str, write: Write) {
    let directory = DirectorySnapshot::default();
    assert!(stores.viewstate_run(ViewStateCmd::Request {
        sid, rk: rk.into(), write, scope: Scope::Leaf, detail: None, guid: String::new(),
    }, directory.view()));
}

fn drive(stores: &mut Stores, done: impl Fn(&Stores) -> bool) {
    let directory = DirectorySnapshot::default();
    for _ in 0..1000 {
        let _ = stores.viewstate_pump(directory.view());
        stores.reconcile_pump(directory.view());
        if done(stores) { return; }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the stores never settled");
}

fn settled(stores: &Stores) -> bool {
    !stores.viewstate.borrow().is_busy_for_test() && !stores.reconcile.is_busy()
}

fn search_row(stores: &Stores, sid: ServerId, rk: &str) -> bool {
    let directory = DirectorySnapshot::default();
    let snap = stores.search_snapshot(directory.view());
    snap.view().shelves().iter().flat_map(|s| &s.items).find_map(|it| match it {
        Item::Media(m) if m.sid == sid && m.rk == rk => Some(m.watched),
        _ => None,
    }).expect("the row is on a shelf")
}

/// The plan's test: a show of two episodes, one already watched. The last is marked watched from a
/// card (a press on the episode's own `(server, ratingKey)`), and the Library grid's show and
/// season tiles — rows the press never touched — get their tick.
#[test]
fn marking_the_last_episode_watched_ticks_the_shows_library_tile_and_its_season() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    pms.truth.watched.lock().unwrap().insert("4".into());
    let (mut stores, sid) = library(&pms);
    assert!(!grid_watched(&stores, 0) && !grid_watched(&stores, 1), "premise: neither container is ticked");
    press(&mut stores, sid, "3", Write::Watched);
    drive(&mut stores, settled);
    assert!(grid_watched(&stores, 2), "the episode itself");
    assert!(grid_watched(&stores, 1), "its season");
    assert!(grid_watched(&stores, 0), "its show");
}

/// A season row and a show row in Search update the same way.
#[test]
fn a_season_row_and_a_show_row_in_search_update_the_same_way() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    pms.truth.watched.lock().unwrap().insert("4".into());
    let (mut stores, sid) = library(&pms);
    let media = |rk: &str, kind| Item::Media(PmsMovie { sid, rk: rk.into(), kind, unwatched: false, ..Default::default() });
    stores.search.publish_shelves_for_test(vec![Shelf {
        epoch: 0, window: Default::default(), kind: Kind::Show,
        items: vec![media("1", 1), media("2", 2)],
    }]);
    assert!(!search_row(&stores, sid, "1") && !search_row(&stores, sid, "2"));
    press(&mut stores, sid, "3", Write::Watched);
    drive(&mut stores, settled);
    assert!(search_row(&stores, sid, "2"), "the season row");
    assert!(search_row(&stores, sid, "1"), "the show row");
}

/// A parent row on another server is not patched by this server's parent: the pair is the identity.
#[test]
fn a_show_row_on_another_server_is_not_patched() {
    let _g = plx_base::testlock::serial();
    let (a, b) = (serve(), serve());
    a.truth.watched.lock().unwrap().insert("4".into());
    let (mut stores, sid_a) = library(&a);
    let sid_b = plx_plex::plex::register_for_test("parents-b", "127.0.0.1", b.port, "token", "parents-b");
    let other = Item::Media(PmsMovie { sid: sid_b, rk: "1".into(), kind: 1, unwatched: true, ..Default::default() });
    stores.search.publish_shelves_for_test(vec![Shelf {
        epoch: 0, window: Default::default(), kind: Kind::Show, items: vec![other],
    }]);
    press(&mut stores, sid_a, "3", Write::Watched);
    drive(&mut stores, settled);
    assert!(grid_watched(&stores, 0), "A's show is ticked");
    assert!(!search_row(&stores, sid_b, "1"), "B's show, which shares the key, is not");
    assert!(b.truth.requests.lock().unwrap().is_empty(), "B was never asked");
}

/// The leaf's answer names its season and show; the keys are not on the row that was pressed, so
/// they cost exactly one more read, which carries both.
#[test]
fn an_episode_costs_one_extra_read_for_its_season_and_show() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    let (mut stores, sid) = library(&pms);
    press(&mut stores, sid, "3", Write::Watched);
    drive(&mut stores, settled);
    assert_eq!(*pms.truth.requests.lock().unwrap(), ["3", "2,1"], "the episode, then its season and show together");
}

/// A movie names no season and no show, so its read-back is the one read.
#[test]
fn a_movie_costs_exactly_one_read() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    let (mut stores, sid) = library(&pms);
    press(&mut stores, sid, "m", Write::Watched);
    drive(&mut stores, settled);
    assert_eq!(*pms.truth.requests.lock().unwrap(), ["m"]);
}

fn parent_row(sid: ServerId, rk: &str, kind: i32, show_rk: &str) -> ReadRow {
    ReadRow {
        movie: PmsMovie { sid, rk: rk.into(), kind, show_rk: show_rk.into(), unwatched: true, ..Default::default() },
        parent: String::new(),
        grandparent: String::new(),
    }
}

/// A show-scope press made while the season/show read is out: the read describes the server from
/// before that press, so its show row and its season row (both under the press) are dropped. A
/// sibling show's row, which the press does not cover, still lands.
#[test]
fn a_show_scope_press_overtakes_an_out_parent_read() {
    let sid = ServerId::from_raw(0);
    let ledger = ViewLedger::default();
    let mut store = ViewReconcileStore::default();
    store.share_ledger(&ledger);
    store.flights.push(Flight {
        id: 1, sid, keys: vec!["2".into(), "1".into(), "9".into()],
        parents: vec!["2".into(), "1".into(), "9".into()], stamp: ledger.stamp(),
    });
    ledger.record(&crate::viewstate::ViewAt { sid, rk: "1".into(), scope: Scope::Show }, ViewEdit::Watched(true));
    store.adapter.mail.lock().unwrap().push(Landing { id: 1, rows: Some(vec![
        parent_row(sid, "2", 2, "1"), parent_row(sid, "1", 1, ""), parent_row(sid, "9", 1, ""),
    ]) });
    let mut published = Vec::new();
    store.pump_with_gate(&plx_machine::landgate::Gate::default(), false, &mut |c| published.push(c.rk));
    assert_eq!(published, ["9"], "the stale show and season rows are dropped; the unrelated show lands");
}

/// A batch that could not be spawned goes back split as it came: leaves to `pending`, the parents
/// that rode with them to `pending_parents`, so the retry batches them the same way.
#[test]
fn an_unspawned_batch_splits_back_into_leaves_and_parents() {
    let sid = ServerId::from_raw(0);
    let mut store = ViewReconcileStore::default();
    store.requeue_unspawned(sid, vec!["3".into(), "4".into(), "2".into(), "1".into()], 2);
    assert_eq!(store.pending_for_test(), [(sid, "3".to_string()), (sid, "4".to_string())]);
    assert_eq!(store.pending_parents, [(sid, "2".to_string()), (sid, "1".to_string())]);
    assert_eq!(store.retry_cd, RETRY_FRAMES);
}

/// Two episodes of one season pressed in a burst: one read of both, one of the season and show —
/// not one pair per episode.
#[test]
fn request_volume_is_one_batch_per_server_per_round() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    let (mut stores, sid) = library(&pms);
    press(&mut stores, sid, "3", Write::Watched);
    press(&mut stores, sid, "4", Write::Watched);
    drive(&mut stores, settled);
    assert_eq!(*pms.truth.requests.lock().unwrap(), ["3,4", "2,1"]);
    assert!(grid_watched(&stores, 0) && grid_watched(&stores, 1));
}

/// Parent facts that agree with the rows change nothing, so no store raises a notice for them.
#[test]
fn parent_facts_that_agree_with_the_rows_raise_no_notice() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    pms.truth.watched.lock().unwrap().extend(["3".to_string(), "4".to_string()]);
    let (mut stores, sid) = library(&pms);
    let media = |rk: &str, kind| Item::Media(PmsMovie { sid, rk: rk.into(), kind, watched: true, unwatched: false, ..Default::default() });
    stores.search.publish_shelves_for_test(vec![Shelf {
        epoch: 0, window: Default::default(), kind: Kind::Show, items: vec![media("1", 1), media("2", 2)],
    }]);
    // the first read-back brings the Library grid up to the server's word; then nothing differs
    stores.reconcile.enqueue([(sid, "3".to_string())]);
    drive(&mut stores, |s| settled(s) && s.reconcile.flights_for_test() == 0
        && pms.truth.requests.lock().unwrap().len() == 2);
    let gens = |s: &Stores| (s.browse.borrow().gen(), s.search.gen(), s.hubs.gen());
    let before = gens(&stores);
    stores.reconcile.enqueue([(sid, "3".to_string())]);
    drive(&mut stores, |s| settled(s) && pms.truth.requests.lock().unwrap().len() == 4);
    assert_eq!(gens(&stores), before, "a read-back that agrees raises no notice");
}
