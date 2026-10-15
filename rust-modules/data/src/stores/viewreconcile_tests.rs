//! The read-back worker against a loopback PMS: a refused write puts the tile back, a taken one
//! leaves it, and a press made while the read was out is never undone by that read's older body.

use super::*;
use crate::stores::{browse::DirectorySnapshot, viewstate::ViewStateCmd, Stores};
use crate::viewstate::Write;
use std::collections::HashSet;
use std::io::{Read, Write as _};
use std::sync::atomic::AtomicUsize;

/// What the fake server holds and how it behaves. One item, rating key `1`.
#[derive(Default)]
struct Truth {
    watched: Mutex<HashSet<String>>,
    refuse_scrobble: AtomicBool,
    /// The next item read answers from a snapshot taken on arrival, then waits for `release`.
    hold_next_read: AtomicBool,
    holding: AtomicBool,
    release: AtomicBool,
    reads: AtomicUsize,
    scrobbles: AtomicUsize,
}

struct Pms {
    port: i32,
    truth: Arc<Truth>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn reply(conn: &mut std::net::TcpStream, status: &str, body: &str) {
    let _ = write!(conn, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
}

fn serve_connection(mut conn: std::net::TcpStream, truth: Arc<Truth>) {
    let mut buf = [0u8; 4096];
    let n = conn.read(&mut buf).unwrap_or(0);
    let request = String::from_utf8_lossy(&buf[..n]).into_owned();
    let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
    if path.starts_with("/:/scrobble") || path.starts_with("/:/unscrobble") {
        truth.scrobbles.fetch_add(1, Ordering::SeqCst);
        if path.starts_with("/:/scrobble") && truth.refuse_scrobble.load(Ordering::SeqCst) {
            return reply(&mut conn, "500 Internal Server Error", "{}");
        }
        let mut watched = truth.watched.lock().unwrap();
        if path.starts_with("/:/scrobble") { watched.insert("1".into()); } else { watched.remove("1"); }
        return reply(&mut conn, "200 OK", "{}");
    }
    if path.starts_with("/library/metadata/") {
        truth.reads.fetch_add(1, Ordering::SeqCst);
        let seen = truth.watched.lock().unwrap().contains("1"); // the body is the truth ON ARRIVAL
        if truth.hold_next_read.swap(false, Ordering::SeqCst) {
            truth.holding.store(true, Ordering::SeqCst);
            while !truth.release.load(Ordering::SeqCst) { std::thread::sleep(std::time::Duration::from_millis(2)); }
            truth.holding.store(false, Ordering::SeqCst);
        }
        let count = if seen { r#","viewCount":1"# } else { "" };
        return reply(&mut conn, "200 OK", &format!(
            r#"{{"MediaContainer":{{"size":1,"Metadata":[{{"ratingKey":"1","type":"movie","title":"t"{count}}}]}}}}"#));
    }
    reply(&mut conn, "200 OK", r#"{"MediaContainer":{"size":0}}"#);
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
        self.truth.release.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() { let _ = t.join(); }
        plx_plex::plex::reset_servers_for_test();
    }
}

/// A Stores whose Library grid holds item `1` of the server, neither watched nor unwatched.
fn library_holding_item(pms: &Pms) -> (Stores, ServerId) {
    assert!(plx_net::net::global_init() && plx_net::net::available() && plx_net::net::threaded_tls_ready());
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("reconcile", "127.0.0.1", pms.port, "token", "reconcile-client");
    plx_plex::plex::client_for(sid).unwrap().set_link(plx_plex::plex::probe::Location::Local);
    let stores = Stores::default();
    stores.browse.borrow_mut().seed_registered_table_for_test([sid, sid]);
    stores.browse.borrow_mut().seed_items_for_test(1);
    (stores, sid)
}

fn tile(stores: &Stores) -> (bool, bool) {
    let mut browse = stores.browse.borrow_mut();
    let snapshot = browse.listing_snapshot();
    let item = snapshot.view().item(0).unwrap();
    (item.watched, item.unwatched)
}

fn press(stores: &mut Stores, sid: ServerId, write: Write) {
    let directory = DirectorySnapshot::default();
    assert!(stores.viewstate_run(ViewStateCmd::Request {
        sid, rk: "1".into(), write, scope: Scope::Leaf, detail: None, guid: String::new(),
    }, directory.view()));
}

/// Pump both owners as the frame loop does until `done`, failing after ~5 s.
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

/// The plan's test: the server refuses a scrobble, the item reads back unwatched, and the grid
/// tile returns to unwatched.
#[test]
fn a_refused_scrobble_reads_back_and_the_grid_tile_returns_to_unwatched() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    pms.truth.refuse_scrobble.store(true, Ordering::SeqCst);
    let (mut stores, sid) = library_holding_item(&pms);
    press(&mut stores, sid, Write::Watched);
    assert_eq!(tile(&stores), (true, false), "the press shows on the frame of the press");
    drive(&mut stores, |s| tile(s) == (false, true));
    assert_eq!(pms.truth.scrobbles.load(Ordering::SeqCst), 1);
    assert_eq!(pms.truth.reads.load(Ordering::SeqCst), 1, "one read-back, of the one item");
    drop(stores);
}

#[test]
fn a_taken_scrobble_reads_back_and_the_tile_stays_watched() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    let (mut stores, sid) = library_holding_item(&pms);
    press(&mut stores, sid, Write::Watched);
    drive(&mut stores, |s| pms.truth.reads.load(Ordering::SeqCst) == 1 && settled(s));
    assert_eq!(tile(&stores), (true, false));
    {
        let mut browse = stores.browse.borrow_mut();
        let snapshot = browse.listing_snapshot();
        assert_eq!(snapshot.view().item(0).unwrap().resume_ms, 0);
    }
    drop(stores);
}

/// A press made while the read-back is out is not undone by that read's body, which describes the
/// server from before the press. The press's own write queues a second read, which settles it.
#[test]
fn a_read_back_overtaken_by_a_press_is_dropped_and_the_item_asked_again() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    pms.truth.hold_next_read.store(true, Ordering::SeqCst);
    let (mut stores, sid) = library_holding_item(&pms);
    press(&mut stores, sid, Write::Watched);
    drive(&mut stores, |_| pms.truth.holding.load(Ordering::SeqCst)); // the read-back is out, held

    press(&mut stores, sid, Write::Unwatched); // …and the user changes their mind
    assert_eq!(tile(&stores), (false, true));
    pms.truth.release.store(true, Ordering::SeqCst); // the held read answers: WATCHED, the old truth
    let directory = DirectorySnapshot::default();
    for _ in 0..1000 {
        stores.reconcile_pump(directory.view()); // the reconcile owner alone: the second write has not landed
        if stores.reconcile.flights_for_test() == 0 { break; }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(stores.reconcile.flights_for_test(), 0, "the read-back landed");
    assert_eq!(tile(&stores), (false, true), "its older body did not undo the press");

    drive(&mut stores, |s| settled(s) && pms.truth.reads.load(Ordering::SeqCst) == 2);
    assert_eq!(tile(&stores), (false, true), "the second read-back confirms the press");
    drop(stores);
}

#[test]
fn an_item_asked_for_while_its_read_is_out_is_not_read_twice_at_once() {
    let mut store = ViewReconcileStore::default();
    let key = (ServerId::from_raw(0), "1".to_string());
    store.enqueue([key.clone(), key.clone()]);
    assert_eq!(store.pending_for_test(), [key.clone()], "asked twice, queued once");
    store.flights.push(Flight { id: 1, sid: key.0, keys: vec!["1".into()], stamp: 0 });
    store.pending.clear();
    store.enqueue([key.clone(), key.clone()]);
    assert!(store.pending_for_test().is_empty(), "its read is out");
    assert_eq!(store.dirty_for_test(), [key], "marked dirty once, to be asked again when it lands");
}

#[test]
fn reset_forgets_the_queue_and_the_flights() {
    let mut store = ViewReconcileStore::default();
    let sid = ServerId::from_raw(0);
    store.enqueue([(sid, "1".to_string())]);
    store.flights.push(Flight { id: 1, sid, keys: vec!["2".into()], stamp: 0 });
    store.enqueue([(sid, "2".to_string())]);
    store.reset();
    assert!(!store.is_busy() && store.dirty_for_test().is_empty());
}

/// A read is stamped when it is SPAWNED, so a press whose write has not landed yet would sit before
/// that stamp and be undone by the read's older body. The read is therefore not spawned while the
/// owner has a write queued or out; it waits in the queue and goes when the burst is idle.
#[test]
fn a_read_is_not_spawned_while_a_write_is_queued_so_the_press_survives_it() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    let (mut stores, sid) = library_holding_item(&pms);
    press(&mut stores, sid, Write::Watched);
    assert_eq!(tile(&stores), (true, false));
    assert!(stores.viewstate.borrow().is_busy_for_test(), "the write has not landed");
    stores.reconcile.enqueue([(sid, "1".to_string())]); // an earlier burst's read-back, still waiting
    let directory = DirectorySnapshot::default();
    for _ in 0..200 { // the reconcile owner alone, as when a press lands between two frames
        stores.reconcile_pump(directory.view());
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(pms.truth.reads.load(Ordering::SeqCst), 0, "no read goes out over a queued write");
    assert_eq!(stores.reconcile.pending_for_test().len(), 1, "it waits");
    assert_eq!(tile(&stores), (true, false), "the press stands");

    drive(&mut stores, |s| settled(s) && pms.truth.reads.load(Ordering::SeqCst) >= 1);
    assert_eq!(tile(&stores), (true, false), "the read that follows the write confirms it");
    drop(stores);
}
