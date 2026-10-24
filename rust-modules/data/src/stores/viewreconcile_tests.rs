//! The read-back worker against a loopback PMS: a refused write puts the tile back, a taken one
//! leaves it, and a press made while the read was out is never undone by that read's older body.

use super::*;
use crate::stores::{browse::DirectorySnapshot, viewstate::ViewStateCmd, Stores};
use crate::viewstate::Write;
use std::collections::HashSet;
use std::io::{Read, Write as _};
use std::sync::atomic::{AtomicI64, AtomicUsize};

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
    /// The `viewOffset` a read answers with (0: none), whatever the watched state.
    view_offset: AtomicI64,
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
    if path.starts_with("/library/all?") {
        // the guid lookup a fan-out resolves the title's copies with: every server holds it as `1`
        return reply(&mut conn, "200 OK", r#"{"MediaContainer":{"size":1,"Metadata":[{"ratingKey":"1","type":"movie","title":"t","guid":"plex://movie/x"}]}}"#);
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
        let offset = match truth.view_offset.load(Ordering::SeqCst) {
            0 => String::new(),
            ms => format!(r#","viewOffset":{ms}"#),
        };
        return reply(&mut conn, "200 OK", &format!(
            r#"{{"MediaContainer":{{"size":1,"Metadata":[{{"ratingKey":"1","type":"movie","title":"t","guid":"plex://movie/x"{count}{offset}}}]}}}}"#));
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
        sid, rk: "1".into(), write, scope: Scope::Leaf, guid: String::new(),
    }, directory.view(), &crate::pms::registry_keys_for_test()).changed);
}

/// Pump both owners as the frame loop does until `done`, failing after ~5 s.
fn drive(stores: &mut Stores, done: impl Fn(&Stores) -> bool) {
    let directory = DirectorySnapshot::default();
    for _ in 0..1000 {
        let _ = stores.viewstate_pump(directory.view(), &crate::pms::registry_keys_for_test());
        super::rig::frame(stores, &directory);
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
        super::rig::frame(&mut stores, &directory); // the reconcile owner alone: the second write has not landed
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
    store.flights.push(Flight { id: 1, sid: key.0, keys: vec!["1".into()], parents: Vec::new(), stamp: 0 });
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
    store.flights.push(Flight { id: 1, sid, keys: vec!["2".into()], parents: Vec::new(), stamp: 0 });
    store.enqueue([(sid, "2".to_string())]);
    store.reset();
    assert!(!store.is_busy() && store.dirty_for_test().is_empty());
}

/// A read is stamped when it is SENT, so a press whose write has not landed yet would sit before
/// that stamp and be undone by the read's older body. The read is therefore not sent while the
/// owner has a write queued or out; it waits in the queue and goes when the burst is idle.
#[test]
fn a_read_is_not_sent_while_a_write_is_queued_so_the_press_survives_it() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    let (mut stores, sid) = library_holding_item(&pms);
    press(&mut stores, sid, Write::Watched);
    assert_eq!(tile(&stores), (true, false));
    assert!(stores.viewstate.borrow().is_busy_for_test(), "the write has not landed");
    stores.reconcile.enqueue([(sid, "1".to_string())]); // an earlier burst's read-back, still waiting
    let directory = DirectorySnapshot::default();
    for _ in 0..200 { // the reconcile owner alone, as when a press lands between two frames
        super::rig::frame(&mut stores, &directory);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(pms.truth.reads.load(Ordering::SeqCst), 0, "no read goes out over a queued write");
    assert_eq!(stores.reconcile.pending_for_test().len(), 1, "it waits");
    assert_eq!(tile(&stores), (true, false), "the press stands");

    drive(&mut stores, |s| settled(s) && pms.truth.reads.load(Ordering::SeqCst) >= 1);
    assert_eq!(tile(&stores), (true, false), "the read that follows the write confirms it");
    drop(stores);
}

// ---- a stopped playback (the StopRecord) ---------------------------------------------------------

fn stop(sid: ServerId, pos_ms: i64, was_watched: bool, ok: bool) -> crate::viewstate::StopRecord {
    crate::viewstate::StopRecord {
        sid, rk: "1".into(), pos_ms, dur_ms: 60_000, ok, was_watched,
    }
}

fn tile_resume(stores: &Stores) -> i64 {
    let mut browse = stores.browse.borrow_mut();
    let snapshot = browse.listing_snapshot();
    snapshot.view().item(0).unwrap().resume_ms
}

/// Two servers holding the same title (`1` on each); the Library grid shows A's copy.
fn two_servers(a: &Pms, b: &Pms) -> (Stores, ServerId, ServerId) {
    assert!(plx_net::net::global_init() && plx_net::net::available() && plx_net::net::threaded_tls_ready());
    plx_plex::plex::reset_servers_for_test();
    let sid_a = plx_plex::plex::register_for_test("reconcile-a", "127.0.0.1", a.port, "token", "reconcile-a");
    let sid_b = plx_plex::plex::register_for_test("reconcile-b", "127.0.0.1", b.port, "token", "reconcile-b");
    for sid in [sid_a, sid_b] {
        plx_plex::plex::client_for(sid).unwrap().set_link(plx_plex::plex::probe::Location::Local);
    }
    let stores = Stores::default();
    stores.browse.borrow_mut().seed_registered_table_for_test([sid_a, sid_b]);
    stores.browse.borrow_mut().seed_items_for_test(1);
    (stores, sid_a, sid_b)
}

/// The tile reads the position the stop reported before any read has been asked for, and the
/// read-back that follows confirms it.
#[test]
fn a_stop_patches_the_resume_point_before_the_read_back_is_asked() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    pms.truth.hold_next_read.store(true, Ordering::SeqCst);
    let (mut stores, sid) = library_holding_item(&pms);
    let directory = DirectorySnapshot::default();
    let _ = stores.stops_landed(vec![stop(sid, 30_000, true, true)], directory.view(), &crate::pms::registry_keys_for_test());
    assert_eq!(pms.truth.reads.load(Ordering::SeqCst), 0, "nothing has been read yet");
    assert_eq!(tile_resume(&stores), 30_000, "the tile already shows where the viewer stopped");
    drive(&mut stores, |_| pms.truth.holding.load(Ordering::SeqCst));
    assert_eq!(tile_resume(&stores), 30_000, "the patch stands while the read is out");
    drop(stores);
}

/// A stop the server did not take still shows the position, and asks for no read: the answer
/// would be the server's old resume point and would undo what the viewer watched.
#[test]
fn a_stop_the_server_refused_patches_the_tile_and_reads_nothing_back() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    let (mut stores, sid) = library_holding_item(&pms);
    let directory = DirectorySnapshot::default();
    let _ = stores.stops_landed(vec![stop(sid, 30_000, true, false)], directory.view(), &crate::pms::registry_keys_for_test());
    drive(&mut stores, |s| settled(s));
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(pms.truth.reads.load(Ordering::SeqCst), 0);
    assert_eq!(tile_resume(&stores), 30_000);
    drop(stores);
}

/// The same refused stop still owes the Detail pages that show the item a re-read of THEIR own: the
/// page was patched with the position the viewer reached, and only a read of the page puts the
/// server's word back, which for a refused write is the old resume point. The re-read is the page's
/// (`take_landed_views`), not a read-back through the reconcile worker, which stays unasked.
#[test]
fn a_stop_the_server_refused_still_owes_the_detail_pages_a_re_read() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    let (mut stores, sid) = library_holding_item(&pms);
    let directory = DirectorySnapshot::default();
    let _ = stores.stops_landed(vec![stop(sid, 30_000, true, false)], directory.view(), &crate::pms::registry_keys_for_test());
    let owed = stores.take_landed_views();
    assert_eq!(owed.len(), 1, "one page re-read for the one refused stop");
    assert!(plx_plex::plex::same_item((owed[0].0, &owed[0].1), (sid, "1")));
    assert!(stores.take_landed_views().is_empty(), "owed once");
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(pms.truth.reads.load(Ordering::SeqCst), 0, "and no read-back undoes the patch");
    drop(stores);
}

/// L2-5 re-reads: a covered page's read-back publishes the facts and owes NO Detail page a re-read
/// (the visible Detail would otherwise be re-fetched seconds after it opened). A read-back asked
/// for the owing way still owes the page.
#[test]
fn a_quiet_read_back_publishes_facts_but_owes_no_detail_page() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    pms.truth.view_offset.store(30_000, Ordering::SeqCst);
    let (mut stores, sid) = library_holding_item(&pms);
    stores.owe_read_back(vec![(sid, "1".to_string())]);
    drive(&mut stores, |s| pms.truth.reads.load(Ordering::SeqCst) == 1 && settled(s));
    assert_eq!(tile_resume(&stores), 30_000, "the facts were published");
    assert!(stores.take_landed_views().is_empty(), "but no Detail page is owed a re-read");
    stores.reconcile.enqueue([(sid, "1".to_string())]);
    drive(&mut stores, |s| pms.truth.reads.load(Ordering::SeqCst) == 2 && settled(s));
    assert_eq!(stores.take_landed_views().len(), 1, "the owing way still owes the page");
    drop(stores);
}

/// The player is on top: the stop still patches the tile, but its read-back waits for the player to
/// leave, then goes out once.
#[test]
fn the_read_back_of_a_stop_waits_while_the_network_is_held() {
    let _g = plx_base::testlock::serial();
    let pms = serve();
    let (mut stores, sid) = library_holding_item(&pms);
    let directory = DirectorySnapshot::default();
    stores.hold_network(true);
    let _ = stores.stops_landed(vec![stop(sid, 30_000, true, true)], directory.view(), &crate::pms::registry_keys_for_test());
    for _ in 0..40 {
        let _ = stores.viewstate_pump(directory.view(), &crate::pms::registry_keys_for_test());
        super::rig::frame(&mut stores, &directory);
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    assert_eq!(pms.truth.reads.load(Ordering::SeqCst), 0, "no read under the player");
    assert_eq!(tile_resume(&stores), 30_000);
    stores.hold_network(false);
    drive(&mut stores, |s| settled(s) && pms.truth.reads.load(Ordering::SeqCst) == 1);
    drop(stores);
}

/// **Owner decision 4.** The stream reached its end on A (A's own scrobble made the title watched);
/// B's copy of the title is scrobbled too, and A is NOT scrobbled a second time.
#[test]
fn an_end_of_stream_scrobbles_the_other_servers_copy_but_not_the_origin_again() {
    let _g = plx_base::testlock::serial();
    let (a, b) = (serve(), serve());
    a.truth.watched.lock().unwrap().insert("1".into()); // what A holds once the stream has ended
    let (mut stores, sid_a, _sid_b) = two_servers(&a, &b);
    let directory = DirectorySnapshot::default();
    let _ = stores.stops_landed(vec![stop(sid_a, 60_000, false, true)], directory.view(), &crate::pms::registry_keys_for_test());
    drive(&mut stores, |s| settled(s) && b.truth.scrobbles.load(Ordering::SeqCst) == 1);
    assert_eq!(a.truth.scrobbles.load(Ordering::SeqCst), 0, "the origin already counted the view");
    assert_eq!(tile(&stores), (true, false), "A's tile reads watched");
    drop(stores);
}

/// The same end of stream, seen from the other server's Search screen: B's copy of the title is
/// scrobbled by the fan-out, and the tile Search holds for it reads watched without a search being
/// run again. B's tile already reads watched but carries a stale resume point, and B's server still
/// answers its read-back with a `viewOffset` of its own: the fan-out's optimistic `Watched` would
/// leave `0`, so only the read-back's `Facts` reaching Search can put the server's word on the tile.
#[test]
fn after_an_end_of_stream_the_other_servers_search_tile_reads_watched() {
    use crate::search::{Item, Kind, Shelf};
    let _g = plx_base::testlock::serial();
    let (a, b) = (serve(), serve());
    a.truth.watched.lock().unwrap().insert("1".into());
    b.truth.view_offset.store(7_000, Ordering::SeqCst);
    let (mut stores, sid_a, sid_b) = two_servers(&a, &b);
    stores.search.publish_shelves_for_test(vec![Shelf {
        epoch: 0, window: Default::default(), kind: Kind::Movie,
        items: vec![Item::Media(crate::pms::PmsMovie {
            sid: sid_b, rk: "1".into(), watched: true, unwatched: false, resume_ms: 20_000, ..Default::default()
        })],
    }]);
    let search_tile = |stores: &Stores| {
        let directory = DirectorySnapshot::default();
        let snap = stores.search_snapshot(directory.view(), &[]);
        snap.view().shelves().iter().flat_map(|s| &s.items).find_map(|it| match it {
            Item::Media(m) if m.sid == sid_b => Some((m.watched, m.unwatched, m.resume_ms)),
            _ => None,
        })
    };
    assert_eq!(
        search_tile(&stores), Some((true, false, 20_000)),
        "premise: B's tile already reads watched but still carries a stale resume point"
    );
    let directory = DirectorySnapshot::default();
    let _ = stores.stops_landed(vec![stop(sid_a, 60_000, false, true)], directory.view(), &crate::pms::registry_keys_for_test());
    drive(&mut stores, |s| settled(s) && b.truth.scrobbles.load(Ordering::SeqCst) == 1);
    assert_eq!(
        search_tile(&stores), Some((true, false, 7_000)),
        "the read-back's Facts replace the stale resume point with the server's word"
    );
    drop(stores);
}

/// A rewatch (already watched when the play began) does not inflate the other servers' counts.
#[test]
fn a_rewatch_does_not_fan_out() {
    let _g = plx_base::testlock::serial();
    let (a, b) = (serve(), serve());
    a.truth.watched.lock().unwrap().insert("1".into());
    let (mut stores, sid_a, _sid_b) = two_servers(&a, &b);
    let directory = DirectorySnapshot::default();
    let _ = stores.stops_landed(vec![stop(sid_a, 60_000, true, true)], directory.view(), &crate::pms::registry_keys_for_test());
    drive(&mut stores, |s| settled(s) && a.truth.reads.load(Ordering::SeqCst) == 1);
    std::thread::sleep(std::time::Duration::from_millis(50));
    let _ = stores.viewstate_pump(directory.view(), &crate::pms::registry_keys_for_test());
    assert_eq!(b.truth.scrobbles.load(Ordering::SeqCst), 0);
    assert_eq!(a.truth.scrobbles.load(Ordering::SeqCst), 0);
    drop(stores);
}

/// A stop in the middle of a film (still unwatched on the read-back) is not an end of stream.
#[test]
fn a_stop_in_the_middle_does_not_fan_out() {
    let _g = plx_base::testlock::serial();
    let (a, b) = (serve(), serve());
    let (mut stores, sid_a, _sid_b) = two_servers(&a, &b);
    let directory = DirectorySnapshot::default();
    let _ = stores.stops_landed(vec![stop(sid_a, 30_000, false, true)], directory.view(), &crate::pms::registry_keys_for_test());
    drive(&mut stores, |s| settled(s) && a.truth.reads.load(Ordering::SeqCst) == 1);
    std::thread::sleep(std::time::Duration::from_millis(50));
    let _ = stores.viewstate_pump(directory.view(), &crate::pms::registry_keys_for_test());
    assert_eq!(b.truth.scrobbles.load(Ordering::SeqCst), 0);
    drop(stores);
}

/// **A landing settles only a candidate its own flight could have seen.** A read sent BEFORE the
/// stop was enqueued describes the server from before the stop; it must neither drop the candidate
/// (our own fan-out would be lost) nor decide it. The dirty re-read sent after the stop does, and
/// the other server's copy is then scrobbled.
#[test]
fn a_flight_sent_before_the_candidate_does_not_settle_it() {
    let _g = plx_base::testlock::serial();
    let (a, b) = (serve(), serve());
    a.truth.hold_next_read.store(true, Ordering::SeqCst);
    let (mut stores, sid, _sid_b) = two_servers(&a, &b);
    let directory = DirectorySnapshot::default();
    let key = (sid, "1".to_string());
    stores.reconcile.enqueue([key.clone()]);
    drive(&mut stores, |_| a.truth.holding.load(Ordering::SeqCst)); // the early read is out, held
    stores.reconcile.enqueue_stop(key.clone(), false); // the stop arrives after it was sent
    a.truth.watched.lock().unwrap().insert("1".into()); // the stream ended: later reads see it
    stores.hold_network(true); // landings are taken, the re-read waits: the stale landing is observed alone
    a.truth.release.store(true, Ordering::SeqCst); // the early read answers with its OLD body
    for _ in 0..1000 {
        super::rig::frame(&mut stores, &directory);
        if stores.reconcile.flights_for_test() == 0 { break; }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(stores.reconcile.candidates_for_test(), [key.clone()], "the stale landing kept the candidate");
    stores.hold_network(false);
    drive(&mut stores, |s| settled(s) && b.truth.scrobbles.load(Ordering::SeqCst) == 1);
    assert!(stores.reconcile.candidates_for_test().is_empty(), "the later flight settled it");
    assert_eq!(a.truth.scrobbles.load(Ordering::SeqCst), 0, "the origin already counted the view");
    drop(stores);
}
