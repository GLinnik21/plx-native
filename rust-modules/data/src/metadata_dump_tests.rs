//! Dump mode at the Metadata sites (detail, season, alt-sources): the real pumps, a dump-armed
//! local gate and a worker that posts late. Each test fails if its site's take reverts to a plain
//! one (the first poll finds an empty mailbox and the landing is missed by that pump), and the
//! superseded variants fail if a stale answer ends the wait or settles the new request. See
//! `plx_machine::landgate`'s module doc for which sites dump mode covers.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;

const WORKER_DELAY: std::time::Duration = std::time::Duration::from_millis(25);
const STALE_DELAY: std::time::Duration = std::time::Duration::from_millis(5);

fn dump_gate(timeout: std::time::Duration) -> plx_machine::landgate::Gate {
    let gate = plx_machine::landgate::Gate::default();
    gate.arm_dump(timeout);
    gate
}

fn detail(rk: &str) -> Option<Detail> {
    Some(Detail { rk: rk.to_string(), ..Default::default() })
}

fn copies() -> Vec<AltCopy> {
    vec![
        AltCopy { sid: SRV_A, rk: "4".into(), ..Default::default() },
        AltCopy { sid: SRV_B, rk: "318".into(), ..Default::default() },
    ]
}

/// A detail request issued before the pump runs lands on THAT pump, however late its worker posts.
/// `detail_done` is written after the take, so the wait decides from the batch it has taken.
#[test]
fn dump_mode_a_detail_request_out_lands_on_the_pump_that_runs_whatever_the_worker() {
    let _serial = plx_base::testlock::serial();
    let adapter = Arc::clone(test_adapter());
    let gen = begin_detail_for_test(&adapter, SRV_A, "42");
    let worker = Arc::clone(&adapter);
    let join = std::thread::spawn(move || {
        std::thread::sleep(WORKER_DELAY);
        land_detail(&worker, SRV_A, "42", gen, detail("42"));
    });
    let gate = dump_gate(std::time::Duration::from_secs(20));
    assert!(pump_detail_with_gate(test_state(), &adapter, &gate),
        "the answer the pump owed must be taken by the pump that asked");
    assert_eq!(current(test_state()).map(|d| d.rk.clone()).as_deref(), Some("42"));
    assert!(!detail_loading(&adapter), "the landing settled the request");
    join.join().unwrap();
}

/// A superseded detail request's stale record is already queued when the pump runs: it must not
/// end the wait (it is not addressed to the current generation), so the current answer lands on
/// this pump too.
#[test]
fn dump_mode_a_superseded_detail_requests_stale_record_does_not_end_the_wait() {
    let _serial = plx_base::testlock::serial();
    let adapter = Arc::clone(test_adapter());
    let stale = begin_detail_for_test(&adapter, SRV_A, "42");
    let gen = begin_detail_for_test(&adapter, SRV_A, "42"); // supersedes the first
    assert_ne!(stale, gen);
    let worker = Arc::clone(&adapter);
    let join = std::thread::spawn(move || {
        std::thread::sleep(STALE_DELAY);
        land_detail(&worker, SRV_A, "42", stale, detail("stale"));
        std::thread::sleep(WORKER_DELAY);
        land_detail(&worker, SRV_A, "42", gen, detail("42"));
    });
    let gate = dump_gate(std::time::Duration::from_secs(20));
    assert!(pump_detail_with_gate(test_state(), &adapter, &gate),
        "the current request was out when the pump ran, so its answer lands on this pump");
    assert_eq!(current(test_state()).map(|d| d.rk.clone()).as_deref(), Some("42"));
    assert!(!detail_loading(&adapter));
    join.join().unwrap();
}

/// No request out, nothing owed: the pump must not wait (a short timeout would fail closed).
#[test]
fn dump_mode_a_detail_pump_with_no_request_out_does_not_wait() {
    let _serial = plx_base::testlock::serial();
    let gate = dump_gate(std::time::Duration::from_millis(100));
    assert!(!pump_detail_with_gate(test_state(), test_adapter(), &gate));
    let adapter = Arc::clone(test_adapter());
    begin_detail_for_test(&adapter, SRV_A, "42");
    supersede_detail(&adapter); // the page closed: DONE catches up, nothing awaits an answer
    assert!(!pump_detail_with_gate(test_state(), &adapter, &gate));
}

/// A season request out when the pump runs lands on THAT pump, however late its worker posts.
#[test]
fn dump_mode_a_season_request_out_lands_on_the_pump_that_runs_whatever_the_worker() {
    let _serial = plx_base::testlock::serial();
    install_show("show-1", 0, &["s1e1", "s1e2"]);
    let adapter = Arc::clone(test_adapter());
    let (gen, prev) = begin_switch(1);
    assert!(season_loading(&adapter));
    let worker = Arc::clone(&adapter);
    let join = std::thread::spawn(move || {
        std::thread::sleep(WORKER_DELAY);
        land_season(&worker, gen, SRV_A, "show-1".to_string(), 1, prev, Some(Vec::new()));
    });
    let gate = dump_gate(std::time::Duration::from_secs(20));
    assert!(pump_season_with_gate(test_state(), &adapter, &gate),
        "the answer the pump owed must be taken by the pump that asked");
    assert!(listed_eps().is_empty(), "the new season's (empty) list is installed");
    assert!(!season_loading(&adapter));
    join.join().unwrap();
}

/// A superseded season request's stale answer must neither end the wait nor settle the new
/// request: the current answer lands on this pump.
#[test]
fn dump_mode_a_superseded_season_requests_stale_answer_does_not_end_the_wait() {
    let _serial = plx_base::testlock::serial();
    install_show("show-1", 0, &["s1e1", "s1e2"]);
    let adapter = Arc::clone(test_adapter());
    let (stale, prev) = begin_switch(1);
    let (gen, _) = begin_switch(0); // supersedes the first
    assert_ne!(stale, gen);
    let worker = Arc::clone(&adapter);
    let join = std::thread::spawn(move || {
        std::thread::sleep(STALE_DELAY);
        land_season(&worker, stale, SRV_A, "show-1".to_string(), 1, prev, Some(Vec::new()));
        std::thread::sleep(WORKER_DELAY);
        land_season(&worker, gen, SRV_A, "show-1".to_string(), 0, 1, Some(Vec::new()));
    });
    let gate = dump_gate(std::time::Duration::from_secs(20));
    assert!(pump_season_with_gate(test_state(), &adapter, &gate),
        "the current request was out when the pump ran, so its answer lands on this pump");
    assert!(!season_loading(&adapter));
    join.join().unwrap();
}

/// An alt-sources request out when the pump runs lands on THAT pump, however late its worker posts.
#[test]
fn dump_mode_an_alt_request_out_lands_on_the_pump_that_runs_whatever_the_worker() {
    let _serial = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    alt_clear(test_state());
    let adapter = Arc::clone(test_adapter());
    let roster = plx_plex::plex::server_roster_gen();
    adapter.alt_roster_gen.store(roster, Ordering::SeqCst);
    let gen = adapter.alt_gen.fetch_add(1, Ordering::SeqCst) + 1; // a request is out: not settled
    let worker = Arc::clone(&adapter);
    let join = std::thread::spawn(move || {
        std::thread::sleep(WORKER_DELAY);
        *worker.alt_slot.lock().unwrap() = Some(AltResult {
            gen, roster_gen: roster, sid: SRV_A, rk: "4".into(), list: copies(),
        });
    });
    let gate = dump_gate(std::time::Duration::from_secs(20));
    assert!(pump_alt_sources_with_gate(test_state(), &adapter, &gate),
        "the answer the pump owed must be taken by the pump that asked");
    assert!(alt_available(test_state(), SRV_A, "4"));
    assert_eq!(adapter.alt_done.load(Ordering::SeqCst), gen, "the take settled the request");
    join.join().unwrap();
    alt_clear(test_state());
}

/// A superseded alt request's stale answer must neither end the wait nor settle the new request.
#[test]
fn dump_mode_a_superseded_alt_requests_stale_answer_does_not_end_the_wait() {
    let _serial = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    alt_clear(test_state());
    let adapter = Arc::clone(test_adapter());
    let roster = plx_plex::plex::server_roster_gen();
    adapter.alt_roster_gen.store(roster, Ordering::SeqCst);
    let stale = adapter.alt_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let gen = adapter.alt_gen.fetch_add(1, Ordering::SeqCst) + 1; // supersedes the first
    let worker = Arc::clone(&adapter);
    let join = std::thread::spawn(move || {
        std::thread::sleep(STALE_DELAY);
        *worker.alt_slot.lock().unwrap() = Some(AltResult {
            gen: stale, roster_gen: roster, sid: SRV_A, rk: "9".into(), list: copies(),
        });
        std::thread::sleep(WORKER_DELAY);
        *worker.alt_slot.lock().unwrap() = Some(AltResult {
            gen, roster_gen: roster, sid: SRV_A, rk: "4".into(), list: copies(),
        });
    });
    let gate = dump_gate(std::time::Duration::from_secs(20));
    assert!(pump_alt_sources_with_gate(test_state(), &adapter, &gate),
        "the current request was out when the pump ran, so its answer lands on this pump");
    assert!(alt_available(test_state(), SRV_A, "4"));
    join.join().unwrap();
    alt_clear(test_state());
}

/// A roster change makes the in-flight answer stale with NO replacement request: it must settle the
/// claim, or the site would wait for an answer nobody owes until the timeout fails the run.
#[test]
fn dump_mode_a_roster_change_settles_the_alt_claim_so_the_site_never_waits_forever() {
    let _serial = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    alt_clear(test_state());
    let adapter = Arc::clone(test_adapter());
    adapter.alt_roster_gen.store(plx_plex::plex::server_roster_gen(), Ordering::SeqCst);
    adapter.alt_gen.fetch_add(1, Ordering::SeqCst); // a request is out…
    plx_plex::plex::register_for_test("alt-roster", "127.0.0.1", 1, "a", "cid"); // …and the roster moves
    // any wait on the abandoned request would fail closed (panic) within 100 ms
    let gate = dump_gate(std::time::Duration::from_millis(100));
    pump_alt_sources_with_gate(test_state(), &adapter, &gate);
    assert_eq!(adapter.alt_gen.load(Ordering::SeqCst), adapter.alt_done.load(Ordering::SeqCst),
        "the roster re-stamp settled the claim");
    plx_plex::plex::reset_servers_for_test();
}

/// A request that never goes out (no portable guid, a one-server install) owes no answer.
#[test]
fn dump_mode_an_alt_request_that_is_never_spawned_owes_nothing() {
    let _serial = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    let adapter = Arc::clone(test_adapter());
    request_alt_sources(test_state(), &adapter, SRV_A, "4", ""); // no guid
    assert_eq!(adapter.alt_gen.load(Ordering::SeqCst), adapter.alt_done.load(Ordering::SeqCst));
    request_alt_sources(test_state(), &adapter, SRV_A, "4", "plex://movie/abc"); // no second server
    assert_eq!(adapter.alt_gen.load(Ordering::SeqCst), adapter.alt_done.load(Ordering::SeqCst));
    let gate = dump_gate(std::time::Duration::from_millis(100));
    assert!(!pump_alt_sources_with_gate(test_state(), &adapter, &gate));
}
