//! The Hubs claim dump mode waits on: [`PmsAdapter::owed`] is raised at spawn, dropped by a refused
//! launch, and released by the take that moves the landing out (never by the apply).

use super::*;
use super::test_support::{land as queue_land, *};

/// Take until `want` landings have been moved out, counting polls (never a clock).
fn take_n(adapter: &PmsAdapter, want: usize) -> Vec<Landing> {
    let mut got = Vec::new();
    for _ in 0..50_000_000u64 {
        got.extend(take_landings(adapter));
        if got.len() >= want { return got; }
        std::thread::yield_now();
    }
    panic!("the worker never answered");
}

#[test]
fn a_spawn_raises_the_claim_and_the_take_of_its_landing_releases_it() {
    let _g = plx_base::testlock::serial();
    let o = Owner::default();
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("owed-hub", "127.0.0.1", 9, "synthetic", "cid");
    let mut s = Src::new(sid, String::new());
    assert!(!owed(&o.adapter), "nothing spawned, nothing owed");
    with_late_fetches_for_test(0, 1, || {
        assert!(kick_with(o.state.hub_gen, &o.adapter, &mut s, &BrowseScope::standalone(), |r| spawn_fetch(&o.adapter, r)).is_none());
    });
    assert!(s.fetching && owed(&o.adapter), "the claim is raised on the spawning thread, before the worker");
    let first = take_landings(&o.adapter);
    assert!(first.is_empty() && owed(&o.adapter), "an empty take releases nothing");
    let landed = take_n(&o.adapter, 1);
    assert_eq!(landed.len(), 1);
    assert!(!owed(&o.adapter), "the take that moved the landing out released the claim ...");
    assert!(s.fetching, "... while the source's own single-flight flag clears only on APPLY, after it");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_refused_launch_gives_the_claim_back() {
    let _g = plx_base::testlock::serial();
    let o = Owner::default();
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("owed-refused", "127.0.0.1", 9, "synthetic", "cid");
    let mut s = Src::new(sid, String::new());
    let refused = with_refused_fetches_for_test(|| {
        kick_with(o.state.hub_gen, &o.adapter, &mut s, &BrowseScope::standalone(), |r| spawn_fetch(&o.adapter, r))
    });
    assert!(refused.is_some(), "a refused launch is an endpoint failure");
    assert!(!s.fetching && !owed(&o.adapter), "nothing will ever answer, so nothing is owed");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_launcher_that_does_not_spawn_here_is_never_counted() {
    let _g = plx_base::testlock::serial();
    let o = Owner::default();
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("owed-held", "127.0.0.1", 9, "synthetic", "cid");
    let mut s = Src::new(sid, String::new());
    let mut held = None;
    kick_with(o.state.hub_gen, &o.adapter, &mut s, &BrowseScope::standalone(), |r| { held = Some(r); true });
    assert!(held.is_some() && s.fetching);
    assert!(!owed(&o.adapter), "a request another adapter holds is not this mailbox's debt (replay)");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn exactly_one_landing_answers_each_spawned_request_and_the_claim_counts_them_down() {
    let _g = plx_base::testlock::serial();
    let o = Owner::default();
    plx_plex::plex::reset_servers_for_test();
    let a = plx_plex::plex::register_for_test("owed-a", "127.0.0.1", 9, "synthetic", "cid");
    let b = plx_plex::plex::register_for_test("owed-b", "127.0.0.1", 10, "synthetic", "cid");
    let (mut sa, mut sb) = (Src::new(a, String::new()), Src::new(b, String::new()));
    with_late_fetches_for_test(0, 1, || {
        for s in [&mut sa, &mut sb] {
            kick_with(o.state.hub_gen, &o.adapter, s, &BrowseScope::standalone(), |r| spawn_fetch(&o.adapter, r));
        }
    });
    assert_eq!(o.adapter.owed.load(Ordering::SeqCst), 2);
    let landed = take_n(&o.adapter, 2);
    assert_eq!(landed.len(), 2, "one landing per spawned request, failures included");
    assert!(!owed(&o.adapter));
    assert!(take_landings(&o.adapter).is_empty(), "and no third");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_queued_landing_without_a_spawn_cannot_underflow_the_claim() {
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![src(0, "", HubState::Ready, Some(build_test(2)))]);
    queue_land(&o.state, &o.adapter, 0, None);
    assert_eq!(take_landings(&o.adapter).len(), 1);
    assert!(!owed(&o.adapter));
    assert_eq!(o.adapter.owed.load(Ordering::SeqCst), 0, "saturating, not wrapped");
    reset(&mut o.state, &o.adapter);
}
