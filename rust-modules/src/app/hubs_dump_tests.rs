//! Dump mode at the Bridge's own landing sites (S5a-3): Home's hubs and the controlled-Home
//! discovery take wait for the answer their request owes, so a request issued in iteration k lands
//! on iteration k+1 whatever the worker's timing, and nothing in the frame took through the
//! unconverted plain take for them.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::{frame, tick};

const DUMP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// A Bridge with `n` registered servers, so Home's first tick spawns one hub request per server.
fn hubs_rig(n: i32) -> Bridge {
    plx_plex::plex::reset_servers_for_test();
    for i in 0..n {
        plx_plex::plex::register_for_test(&format!("hubs-dump-{i}"), "127.0.0.1", 9 + i, "synthetic", "cid");
    }
    Bridge::for_test(|| 0)
}

/// Run Home frames until `claims` hub requests are out, and return the number of the frame that
/// spawned them. Home queues its store work for the end of a frame, so which frame that is belongs
/// to the Bridge; the contract under test is only that the landing is due on the NEXT one.
fn spawn_frame(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, claims: u32) -> u32 {
    for i in 0..8 {
        frame(d, rig, AppArg::Home, tick(i), vec![]);
        if rig.stores.hubs.owed_for_test() == claims { return i; }
    }
    panic!("Home never spawned {claims} hub request(s); {} owed", rig.stores.hubs.owed_for_test());
}

/// Home's hub request is spawned by frame 0's tick and its landing is due on frame 1, for every
/// worker timing. The worker posts only after a take has found the mailbox empty, so a take that
/// does not wait cannot see it; `extra` varies where in the poll loop of one that does it lands.
#[test]
fn home_hubs_land_on_the_next_frame_whatever_the_worker_timing() {
    let _guard = plx_base::testlock::serial();
    for extra in [0u32, 1, 7, 400, 20_000] {
        let mut rig = hubs_rig(1);
        rig.stores.landgate.arm_dump(DUMP_TIMEOUT);
        let mut d = Dispatcher::<AppHost>::new();
        plx_data::pms::with_late_fetches_for_test(extra, 3, || {
            let spawned = spawn_frame(&mut d, &mut rig, 1);
            assert_eq!(rig.stores.hubs.hub_len_for_test(0), 0, "extra={extra}: nothing has landed yet");
            frame(&mut d, &mut rig, AppArg::Home, tick(spawned + 1), vec![]);
        });
        assert_eq!(rig.stores.hubs.hub_len_for_test(0), 3,
            "extra={extra}: the hub request issued in one frame must have landed in the next");
        assert!(!rig.stores.hubs.owed());
        let hub_ord = StoreId::Hubs.ord();
        assert!(rig.stores.landgate.unconverted_takes().iter().all(|(ord, _)| *ord != hub_ord),
            "extra={extra}: Home's hub landing took through the plain take: {:?}",
            rig.stores.landgate.unconverted_takes());
        rig.stores.landgate.disarm();
    }
    plx_plex::plex::reset_servers_for_test();
}

/// Two sources spawned in one tick answer at different points; one iteration's take collects both.
#[test]
fn every_hub_request_spawned_in_one_frame_lands_in_the_next() {
    let _guard = plx_base::testlock::serial();
    let mut rig = hubs_rig(2);
    rig.stores.landgate.arm_dump(DUMP_TIMEOUT);
    let mut d = Dispatcher::<AppHost>::new();
    plx_data::pms::with_late_fetches_for_test(5_000, 2, || {
        let spawned = spawn_frame(&mut d, &mut rig, 2);
        frame(&mut d, &mut rig, AppArg::Home, tick(spawned + 1), vec![]);
    });
    assert_eq!(rig.stores.hubs.owed_for_test(), 0, "both landings were taken in the next frame");
    assert!(rig.stores.hubs.take_results().is_empty(), "and none is left for frame 2");
    rig.stores.landgate.disarm();
    plx_plex::plex::reset_servers_for_test();
}

/// The same frames without dump mode do NOT promise the landing frame: the late worker posts only
/// after a take found the mailbox empty, so an unarmed take sees nothing in frame 1, which is
/// exactly what dump mode replaces.
#[test]
fn without_dump_mode_the_hub_landing_still_follows_the_worker() {
    let _guard = plx_base::testlock::serial();
    let mut rig = hubs_rig(1);
    let mut d = Dispatcher::<AppHost>::new();
    plx_data::pms::with_late_fetches_for_test(0, 3, || {
        let spawned = spawn_frame(&mut d, &mut rig, 1);
        frame(&mut d, &mut rig, AppArg::Home, tick(spawned + 1), vec![]);
        assert_eq!(rig.stores.hubs.hub_len_for_test(0), 0,
            "the next frame saw an empty mailbox: the worker posts only after a take found nothing");
    });
    plx_plex::plex::reset_servers_for_test();
}

/// A Bridge in controlled-Home shape (the only one whose `take_live_results` takes a discovery
/// result), with one registered server.
fn discovery_rig() -> Bridge {
    plx_plex::plex::reset_servers_for_test();
    plx_plex::plex::register_for_test("discovery-dump", "127.0.0.1", 9, "synthetic", "cid");
    let mut rig = Bridge::for_test(|| 0);
    let mt = unsafe { plx_base::task::MainThread::assume() };
    let publisher = plx_plex::plex::session::ProfilePublisher::scoped(&mt);
    rig.home_io = Some(crate::app::HomeIo {
        replay: false,
        preferences: Default::default(),
        requests: Vec::new(),
        admissions: Default::default(),
        failure: None,
        profile: publisher.snapshot(),
    });
    rig
}

#[test]
#[should_panic(expected = "store still owes an answer")]
fn the_controlled_home_discovery_take_waits_for_a_request_it_owes_under_dump_mode() {
    let _guard = plx_base::testlock::serial();
    let mut rig = discovery_rig();
    // A launcher that holds the request instead of spawning: the claim is up and no worker will
    // ever answer.
    let mut held = false;
    rig.stores.browse.borrow_mut().controlled_discover(&mut |_| { held = true; true });
    assert!(held && rig.stores.browse.borrow().discovery_owed(), "the request is out");
    rig.stores.landgate.arm_dump(std::time::Duration::from_millis(20));
    // A take that waits must hit the fail-closed timeout. A plain take would poll once and return
    // nothing, and this would not panic.
    let _ = rig.take_live_results();
}

#[test]
fn the_controlled_home_discovery_take_does_not_wait_without_a_request() {
    let _guard = plx_base::testlock::serial();
    let mut rig = discovery_rig();
    assert!(!rig.stores.browse.borrow().discovery_owed());
    rig.stores.landgate.arm_dump(std::time::Duration::from_millis(20));
    assert!(rig.take_live_results().is_empty());
    let browse_ord = StoreId::Browse.ord();
    assert!(rig.stores.landgate.unconverted_takes().iter().all(|(ord, _)| *ord != browse_ord),
        "the discovery take is a converted site: {:?}", rig.stores.landgate.unconverted_takes());
    rig.stores.landgate.disarm();
    plx_plex::plex::reset_servers_for_test();
}
