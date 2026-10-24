//! The Hubs claim dump mode waits on: [`PmsAdapter::owed`] is raised at launch, dropped by a refused
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

/// Launch `request` as the runner does, but with a worker that answers late (see
/// [`launch_late_hubs_job_for_test`]) on a thread of its own.
fn launch_late(adapter: &Arc<PmsAdapter>, request: HubRequest, extra: u32, items: usize) -> bool {
    let client = request.client_for_test();
    let job = Job { addr: hubs_addr(request.seq), req: request };
    launch_late_hubs_job_for_test(job, HubsPort::of(adapter), client, |work| { std::thread::spawn(work); true }, extra, items)
}

/// The request one kick of `s` asks for.
fn kicked(o: &Owner, s: &mut Src) -> HubRequest {
    let mut held = None;
    assert!(kick_with(o.state.hub_gen, &o.adapter, s, &BrowseScope::standalone(), |r| held = Some(r)).is_none());
    held.expect("the kick asked for a read")
}

#[test]
fn a_launch_raises_the_claim_and_the_take_of_its_landing_releases_it() {
    let _g = plx_base::testlock::serial();
    let o = Owner::default();
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("owed-hub", "127.0.0.1", 9, "synthetic", "cid");
    let mut s = Src::new(sid, String::new(), &registry_keys_for_test());
    assert!(!owed(&o.adapter), "nothing spawned, nothing owed");
    let request = kicked(&o, &mut s);
    assert!(!owed(&o.adapter), "the step emits the request and starts nothing: no claim until the runner launches it");
    assert!(launch_late(&o.adapter, request, 0, 1));
    assert!(s.fetching && owed(&o.adapter), "the claim is raised on the launching thread, before the worker");
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
    let mut o = Owner::default();
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("owed-refused", "127.0.0.1", 9, "synthetic", "cid");
    let mut s = Src::new(sid, String::new(), &registry_keys_for_test());
    let request = kicked(&o, &mut s);
    let (addr, client) = (hubs_addr(request.seq), request.client_for_test());
    let port = HubsPort::of(&o.adapter);
    let started = launch_hubs_job(Job { addr, req: request }, port.clone(), client, |_work| false);
    assert!(!started, "the runner learns that nothing was started");
    assert!(!owed(&o.adapter), "nothing will ever answer, so nothing is owed");
    port.refused(addr); // the runner's one answer for a job it did not start
    assert!(s.fetching, "the refused read is in flight until that step settles it");
    seed(&mut o.state, vec![s]);
    let settled = step_landings(&mut o.state, &o.adapter, None, Vec::new);
    assert_eq!(settled.iter().map(|r| r.sid).collect::<Vec<_>>(), [sid], "a refused launch is an endpoint failure");
    assert!(!o.state.srcs[0].fetching && !owed(&o.adapter));
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_request_nobody_launched_is_never_counted() {
    let _g = plx_base::testlock::serial();
    let o = Owner::default();
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("owed-held", "127.0.0.1", 9, "synthetic", "cid");
    let mut s = Src::new(sid, String::new(), &registry_keys_for_test());
    let held = kicked(&o, &mut s);
    assert!(s.fetching);
    assert!(!owed(&o.adapter), "a request nobody launched is not this mailbox's debt (replay drops it unanswered)");
    drop(held);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn exactly_one_landing_answers_each_launched_request_and_the_claim_counts_them_down() {
    let _g = plx_base::testlock::serial();
    let o = Owner::default();
    plx_plex::plex::reset_servers_for_test();
    let a = plx_plex::plex::register_for_test("owed-a", "127.0.0.1", 9, "synthetic", "cid");
    let b = plx_plex::plex::register_for_test("owed-b", "127.0.0.1", 10, "synthetic", "cid");
    let (mut sa, mut sb) = (Src::new(a, String::new(), &registry_keys_for_test()), Src::new(b, String::new(), &registry_keys_for_test()));
    for s in [&mut sa, &mut sb] {
        let request = kicked(&o, s);
        assert!(launch_late(&o.adapter, request, 0, 1));
    }
    assert_eq!(o.adapter.owed.load(Ordering::SeqCst), 2);
    let landed = take_n(&o.adapter, 2);
    assert_eq!(landed.len(), 2, "one landing per launched request, failures included");
    assert!(!owed(&o.adapter));
    assert!(take_landings(&o.adapter).is_empty(), "and no third");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_queued_landing_without_a_launch_cannot_underflow_the_claim() {
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

#[test]
fn a_refusal_for_a_request_no_source_has_in_flight_is_dropped() {
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("owed-stale-refusal", "127.0.0.1", 9, "synthetic", "cid");
    let mut s = Src::new(sid, String::new(), &registry_keys_for_test());
    let request = kicked(&o, &mut s);
    let addr = hubs_addr(request.seq);
    s.fetching = false; // superseded: a refetch, a cancel or a landing released the latch
    s.state = HubState::Ready;
    seed(&mut o.state, vec![s]);
    HubsPort::of(&o.adapter).refused(addr);
    let settled = step_landings(&mut o.state, &o.adapter, None, Vec::new);
    assert_eq!(settled.iter().count(), 0, "a superseded request's refusal blames no source");
    assert_eq!(o.state.srcs[0].state, HubState::Ready);
    assert!(o.adapter.refused.lock().unwrap().is_empty(), "and is not kept for a later step");
    plx_plex::plex::reset_servers_for_test();
}
