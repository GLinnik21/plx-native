//! What a Search query does to the network, pinned at the `Bridge` seam.
//!
//! These tests drive the production pump arm (`StoreWork::Search`, delivered through
//! [`Bridge::deliver`]) against two loopback servers and read back only what is observable from
//! outside the store: how many requests each server saw, which items the shelves hold, and what a
//! late answer to a superseded query does. They name no worker, no job and no mailbox of the
//! fetch machinery (only the landing seam `land_for_test`, which is exactly what a finished fetch
//! posts through), so they hold the behaviour still while the fetch itself moves between a spawned
//! worker and a store job.
//!
//! Between the pump that starts the fetches and the pump that lands them, a test calls BOTH
//! [`plx_base::task::drain_workers_for_test`] and [`Bridge::run_held_store_jobs_for_test`] and
//! asserts neither's count: whichever of the two the fetch rides on has work, the other none.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::{tick, Pms};
use plx_plex::plex::ServerId;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

/// Requests each loopback server answered for `/hubs/search`: server A and server B.
static HITS_A: AtomicUsize = AtomicUsize::new(0);
static HITS_B: AtomicUsize = AtomicUsize::new(0);

/// One movie hub holding one movie, `rk`. Any path but `/hubs/search` is a 404 and is not counted.
fn hub_answer(hits: &AtomicUsize, path: &str, rk: &str) -> (&'static str, String) {
    if path != "/hubs/search" {
        return ("404 Not Found", String::new());
    }
    hits.fetch_add(1, SeqCst);
    ("200 OK", format!(r#"{{"MediaContainer":{{"size":1,"Hub":[
        {{"type":"movie","hubIdentifier":"movie","size":1,"Metadata":[
            {{"ratingKey":"{rk}","type":"movie","title":"Film {rk}"}}]}}]}}}}"#))
}

fn answer_a(path: &str) -> (&'static str, String) {
    hub_answer(&HITS_A, path, "A1")
}

fn answer_b(path: &str) -> (&'static str, String) {
    hub_answer(&HITS_B, path, "B1")
}

/// Two servers on loopback, both registered and reachable, with the request counters at zero.
struct Servers {
    a: ServerId,
    b: ServerId,
    _pms: (Pms, Pms),
    _guard: plx_base::testlock::Serial,
}

impl Servers {
    fn new() -> Servers {
        let guard = plx_base::testlock::serial();
        assert!(plx_net::net::global_init() && plx_net::net::available()
            && plx_net::net::threaded_tls_ready());
        HITS_A.store(0, SeqCst);
        HITS_B.store(0, SeqCst);
        let (pms_a, pms_b) = (Pms::serve(answer_a), Pms::serve(answer_b));
        plx_plex::plex::reset_servers_for_test();
        let register = |name: &str, id: &str, port: i32| {
            let sid = plx_plex::plex::register_for_test(name, "127.0.0.1", port, "t", id);
            plx_plex::plex::client_for(sid).unwrap()
                .set_link(plx_plex::plex::probe::Location::Local);
            sid
        };
        let a = register("search-pin-a", "c-search-pin-a", pms_a.port);
        let b = register("search-pin-b", "c-search-pin-b", pms_b.port);
        assert_ne!(a, b);
        Servers { a, b, _pms: (pms_a, pms_b), _guard: guard }
    }
}

impl Drop for Servers {
    fn drop(&mut self) {
        plx_plex::plex::reset_servers_for_test();
    }
}

/// One delivery of the Search pump arm with `dt_us` of frame time, as the frame loop makes it.
fn pump(rig: &mut Bridge, dt_us: u32) {
    let parts = CxParts { tick: tick(0), press: Default::default(), focus: Default::default(),
        owner: InputOwner::Entry(EntryId(0)) };
    let mut present = Present::new();
    let mut effects = Vec::new();
    let mut fx = Effects::new(&mut effects, MachineId::Store(StoreId::Search.ord()), &mut present);
    assert_eq!(rig.deliver(MachineId::Store(StoreId::Search.ord()),
        &AppMsg::StoreWork(plx_data::stores::StoreWork::Search { dt_us }), &parts, &mut fx),
        Handled::Yes);
}

/// Let every fetch the last pump started finish, wherever it runs, without asserting where.
fn finish_fetches(rig: &mut Bridge) {
    plx_base::task::drain_workers_for_test();
    let _ = rig.run_held_store_jobs_for_test();
}

/// A query over two registered servers makes exactly one request per server, lands both answers,
/// and asks neither again once it has been answered.
#[test]
fn a_query_over_two_slots_makes_one_fetch_per_slot_and_lands_both() {
    let servers = Servers::new();
    let mut rig = Bridge::for_test(|| 0);
    // A store's first pump sees the registry as changed from nothing and supersedes whatever is
    // armed; take that pump before the query so the query is not wiped by it.
    pump(&mut rig, 0);

    rig.search_run(plx_data::stores::search::SearchCmd::SetQuery("film".into()));
    assert_eq!((HITS_A.load(SeqCst), HITS_B.load(SeqCst)), (0, 0), "typing alone asks nothing");

    // 0.3 s clears the 0.25 s settle, so this one pump ends the debounce and starts both fetches.
    pump(&mut rig, 300_000);
    finish_fetches(&mut rig);
    pump(&mut rig, 0);

    assert_eq!((HITS_A.load(SeqCst), HITS_B.load(SeqCst)), (1, 1),
        "one request to each server, not zero and not two");
    let held = rig.stores.search.held_keys();
    assert!(held.iter().any(|(sid, rk)| *sid == servers.a && rk == "A1"),
        "server A's answer reached the shelves: {held:?}");
    assert!(held.iter().any(|(sid, rk)| *sid == servers.b && rk == "B1"),
        "server B's answer reached the shelves: {held:?}");

    // Both answered: further frames ask nobody.
    for _ in 0..3 {
        pump(&mut rig, 100_000);
        finish_fetches(&mut rig);
        pump(&mut rig, 0);
    }
    assert_eq!((HITS_A.load(SeqCst), HITS_B.load(SeqCst)), (1, 1),
        "an answered source is not asked again for the same query");
    rig.assert_no_store_jobs_left_for_test();
}

/// An answer to a query the person has since replaced is dropped, and the machinery that dropped it
/// still lands the answer to the query on screen (so the empty shelves are a verdict, not a dead
/// harness).
#[test]
fn a_superseded_querys_late_landing_is_dropped() {
    let servers = Servers::new();
    let mut rig = Bridge::for_test(|| 0);
    pump(&mut rig, 0);

    rig.search_run(plx_data::stores::search::SearchCmd::SetQuery("fi".into()));
    let old_gen = rig.stores.search.query_gen();
    rig.search_run(plx_data::stores::search::SearchCmd::SetQuery("film".into()));
    let new_gen = rig.stores.search.query_gen();
    assert_ne!(old_gen, new_gen, "a new query is a new generation");

    let movie = |rk: &str| plx_data::search::Item::Media(plx_data::pms::PmsMovie {
        sid: servers.a, rk: rk.into(), title: rk.into(), ..Default::default()
    });
    let idx = servers.a.raw() as usize;
    let adapter = rig.stores.search.adapter_for_test();

    // The first query's worker finishes after the second query was typed.
    plx_data::search::land_for_test(&adapter, idx, old_gen, movie("stale-row"));
    pump(&mut rig, 0);
    assert!(rig.stores.search.held_keys().is_empty(),
        "a landing for a superseded query reaches no shelf: {:?}", rig.stores.search.held_keys());

    // The harness does land: the answer to the query on screen shows.
    plx_data::search::land_for_test(&adapter, idx, new_gen, movie("fresh-row"));
    pump(&mut rig, 0);
    let held = rig.stores.search.held_keys();
    assert!(held.iter().any(|(_, rk)| rk == "fresh-row"), "the current query's landing shows: {held:?}");
    assert!(held.iter().all(|(_, rk)| rk != "stale-row"), "the stale row never shows: {held:?}");
    assert_eq!((HITS_A.load(SeqCst), HITS_B.load(SeqCst)), (0, 0),
        "the debounce is still open, so no request went out for either query");
}
