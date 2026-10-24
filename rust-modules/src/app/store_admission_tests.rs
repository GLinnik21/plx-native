//! What the stores that start workers admit through the tape, observed from the `Bridge` that
//! pumps them. The recorded admission is the one value a replay compares byte for byte
//! (`stores::tape::admit`), so it is pinned here as a literal, independent of how the request
//! reaches its worker: a store that starts the worker itself and a store that emits a job for
//! the runner to start must record the same bytes.
//!
//! Each test makes ONE pump, so it does not depend on when any worker completes. It is written
//! against signatures that did not change when the Collection store moved onto the job lane
//! (`Bridge::collection_run`, `Bridge::collection_pump`, `Bridge::person_run`,
//! `Bridge::person_pump`, `assert_no_store_jobs_left_for_test`), so the same file compiles and
//! passes on both sides of that change.

use plx_data::collection::{CollectionStatus, CollectionTarget, PAGE_SIZE};
use plx_data::stores::collection::CollectionCmd;
use plx_data::stores::person::PersonCmd;
use plx_data::stores::tape;
use plx_plex::plex::collections::CollectionRef;
use plx_plex::plex::ServerId;

use super::Bridge;

/// Puts the tape and the registry back to their inactive state when a test ends, however it ends,
/// after any worker the pump started has finished. Declare it AFTER the serial guard, so it drops
/// first.
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        plx_base::task::drain_workers_for_test();
        tape::reset_for_test();
        plx_plex::plex::reset_servers_for_test();
    }
}

/// A recording armed (the tape in record mode: the launch runs and its answer is written down) and
/// a server registered at a port nothing listens on, so a fetch that does start fails at once.
fn recording() -> ServerId {
    plx_plex::plex::reset_servers_for_test();
    tape::init(None, false);
    plx_plex::plex::register_for_test("store-admission", "127.0.0.1", 9, "t", "cid")
}

fn open(bridge: &mut Bridge, sid: ServerId, rk: &str, sec: i64, tag: i64) {
    assert!(bridge.collection_run(CollectionCmd::Open { target: CollectionTarget {
        id: CollectionRef { sid, rk: rk.into(), sec, tag, name: "Set".into() }, want: PAGE_SIZE } }));
}

/// One frame's collection pump, as `run.rs` makes it.
fn pump_once(bridge: &mut Bridge) {
    let _frame = plx_base::task::FrameScope::enter();
    bridge.collection_pump();
}

/// One frame's person pump and then its collection pump, in the order `run.rs` makes them
/// (`person_pump`, `collection_pump`, then `reconcile_details`).
fn pump_person_then_collection(bridge: &mut Bridge) {
    let _frame = plx_base::task::FrameScope::enter();
    bridge.person_pump();
    bridge.collection_pump();
}

/// The first request of a collection opened by its ratingKey is the header read.
#[test]
fn a_collection_opened_by_rating_key_admits_its_header_read_once() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let sid = recording();
    let client = plx_plex::plex::client_for(sid).expect("registered").instance_gen();
    let mut bridge = Bridge::for_test(|| 0);

    open(&mut bridge, sid, "50001", 1, 7);
    pump_once(&mut bridge);

    assert_eq!(tape::finish(), (vec![serde_json::json!({"content_resource":true,"admitted":true,
        "request":{"store":"collection","slot":0,"gen":1,"sid":sid.raw(),"client":client,
            "job":{"Header":{"rk":"50001"}}}})], None),
        "one header read, admitted, in the recorded shape");
    bridge.assert_no_store_jobs_left_for_test();
}

/// A collection opened by section and tag has no ratingKey yet, so its first request resolves the
/// tag.
#[test]
fn a_collection_opened_by_tag_admits_its_tag_resolution_once() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let sid = recording();
    let client = plx_plex::plex::client_for(sid).expect("registered").instance_gen();
    let mut bridge = Bridge::for_test(|| 0);

    open(&mut bridge, sid, "", 4, 77);
    pump_once(&mut bridge);

    assert_eq!(tape::finish(), (vec![serde_json::json!({"content_resource":true,"admitted":true,
        "request":{"store":"collection","slot":0,"gen":1,"sid":sid.raw(),"client":client,
            "job":{"Resolve":{"sec":4,"tag":77,"name":"Set"}}}})], None),
        "one tag resolution, admitted, in the recorded shape");
    bridge.assert_no_store_jobs_left_for_test();
}

/// A collection whose server is not registered has nothing to ask: no admission is written, the
/// page reads Failed at once and the next attempt is held off.
#[test]
fn a_collection_for_an_unregistered_server_admits_nothing_and_reads_failed() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let _ = recording();
    let mut bridge = Bridge::for_test(|| 0);

    open(&mut bridge, ServerId::UNSET, "50001", 1, 7);
    pump_once(&mut bridge);

    assert_eq!(tape::finish(), (vec![], None), "no request was admitted");
    assert_eq!(bridge.stores.collection_view().current().expect("open").status, CollectionStatus::Failed);
    bridge.assert_no_store_jobs_left_for_test();
}

/// The order across stores inside one frame is part of what a replay compares: the admission list
/// is positional (`stores::tape::admit`), so a frame that has a person and a collection to ask
/// for records the person's requests, in slot order, and then the collection's. Each store's
/// requests leave in its own pump, before the next store's, so moving a drain to the end of the
/// frame (or to a backstop that runs under the detail pump) would swap them and diverge a
/// fixture that carries both.
#[test]
fn a_frame_with_a_person_and_a_collection_admits_the_person_first() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let sid = recording();
    assert_eq!(sid.raw(), 0, "the first server registered is slot 0, which the person slots below name");
    let client = plx_plex::plex::client_for(sid).expect("registered").instance_gen();
    let mut bridge = Bridge::for_test(|| 0);

    assert!(bridge.person_run(PersonCmd::Open { sid, key: "19".into(), guid: "sf06c144a".into(),
        name: "Actor".into(), thumb: String::new() }));
    open(&mut bridge, sid, "50001", 1, 7);
    pump_person_then_collection(&mut bridge);

    let admitted = |request: serde_json::Value| serde_json::json!({
        "content_resource": true, "admitted": true, "request": request });
    assert_eq!(tape::finish(), (vec![
        // the person's source on this server (slot 1: server 0, media), then its two plex.tv reads
        // (slots 48 and 49): the shapes fixture 12 records
        admitted(serde_json::json!({"store":"person","slot":1,"gen":1,"arg":["19"],"guid":"sf06c144a",
            "local":"19","client":client,"sid":sid.raw()})),
        admitted(serde_json::json!({"store":"person","slot":48,"gen":1,"arg":["sf06c144a"],
            "guid":"sf06c144a"})),
        admitted(serde_json::json!({"store":"person","slot":49,"gen":1,"arg":["sf06c144a"],
            "guid":"sf06c144a"})),
        // and only then the collection's header read
        admitted(serde_json::json!({"store":"collection","slot":0,"gen":1,"sid":sid.raw(),
            "client":client,"job":{"Header":{"rk":"50001"}}})),
    ], None), "the person's requests in slot order, then the collection's");
    bridge.assert_no_store_jobs_left_for_test();
}
