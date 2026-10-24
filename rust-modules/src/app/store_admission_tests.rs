//! What the stores that start workers admit through the tape, observed from the `Bridge` that
//! pumps them. The recorded admission is the one value a replay compares byte for byte
//! (`stores::tape::admit`), so it is pinned here as a literal, independent of how the request
//! reaches its worker: a store that starts the worker itself and a store that emits a job for
//! the runner to start must record the same bytes.
//!
//! Each test makes ONE pump, so it does not depend on when any worker completes. It is written
//! against signatures that did not change when the Collection store moved onto the job lane
//! (`Bridge::collection_run`, `Bridge::collection_pump`, `assert_no_store_jobs_left_for_test`), so
//! the same file compiles and passes on both sides of that change.

use plx_data::collection::{CollectionStatus, CollectionTarget, PAGE_SIZE};
use plx_data::stores::collection::CollectionCmd;
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
