//! Every `Bridge` entry that steps the Library's source discovery starts the read through the
//! application's runner, in the same call: the runner's launch is the only place a discovery
//! request is started, so the seam that stands for the system refusing the thread
//! (`with_refused_hubs_launches_for_test`) reaches it from every entry. A discovery that starts its
//! own worker ignores the seam: its claim stays up after the entry returns, and a controlled run
//! records the launch as admitted.
//!
//! The rig is one server registered at a port nothing listens on, whose sources have not been
//! discovered: the first step the entry runs asks for the first source's sections.

use plx_data::stores::StoreWork;
use plx_plex::plex::ServerId;

use super::test_support::{frame, tick};
use super::*;

/// Puts the registry back to its inactive state when a test ends, however it ends, after any
/// worker the dispatch started has finished. Declare it AFTER the serial guard, so it drops first.
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        plx_base::task::drain_workers_for_test();
        plx_plex::plex::reset_servers_for_test();
    }
}

/// One server registered at a port nothing listens on, as the current server.
fn one_server() -> ServerId {
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("discovery-drain", "127.0.0.1", 9, "t", "cid");
    assert!(plx_plex::plex::set_current(sid));
    sid
}

/// One `StoreWork::BrowseDiscovery` message, as the central dispatcher delivers it.
fn deliver_discovery_work(bridge: &mut Bridge) {
    let parts = CxParts { tick: tick(0), press: Default::default(), focus: Default::default(),
        owner: InputOwner::Entry(EntryId(0)) };
    let mut present = plx_machine::present::Present::default();
    let mut out = Vec::new();
    let to = MachineId::Store(StoreId::Browse.ord());
    let mut fx = Effects::new(&mut out, to, &mut present);
    assert_eq!(bridge.deliver(to, &AppMsg::StoreWork(StoreWork::BrowseDiscovery), &parts, &mut fx),
        Handled::Yes);
}

/// The runner refused the launch, so the claim the step raised went down in the same call, and
/// nothing waits on an outbox.
#[track_caller]
fn assert_refused_in_the_same_call(bridge: &Bridge, entry: &str) {
    assert!(!bridge.stores.browse.borrow().discovery_owed(),
        "{entry}: the runner refused the launch, so the claim is down when the call returns");
    bridge.assert_no_store_jobs_left_for_test();
}

/// The Bridge's own entry (`activate_server_owned`, `boot.rs`, goes through it).
#[test]
fn browse_discover_pump_starts_the_read_its_step_emitted_in_the_same_call() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    one_server();
    let mut bridge = Bridge::for_test(|| 0);
    crate::app::adapters::stores::with_refused_hubs_launches_for_test(|| {
        let _ = bridge.browse_discover_pump();
    });
    assert_refused_in_the_same_call(&bridge, "browse_discover_pump");
}

/// The live `StoreWork::BrowseDiscovery` arm of a Bridge that records nothing.
#[test]
fn the_live_discovery_arm_starts_the_read_its_step_emitted_in_the_same_call() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    one_server();
    let mut bridge = Bridge::for_test(|| 0);
    crate::app::adapters::stores::with_refused_hubs_launches_for_test(|| {
        deliver_discovery_work(&mut bridge);
    });
    assert_refused_in_the_same_call(&bridge, "the live discovery arm");
}

/// The frame's pre-capture owner pump (`frame_ingest`), on a screen whose Tick sends no
/// `StoreWork::BrowseDiscovery` (Login: Home, Search and Onboard all send one, whose live arm
/// would start the job and hide a `frame_ingest` that did not).
#[test]
fn a_frame_on_a_screen_that_asks_for_no_discovery_starts_the_read_its_ingest_emitted() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    one_server();
    let mut d = Dispatcher::<AppHost>::new();
    let mut bridge = Bridge::for_test(|| 0);
    crate::app::adapters::stores::with_refused_hubs_launches_for_test(|| {
        frame(&mut d, &mut bridge, AppArg::Login, tick(0), vec![]);
    });
    assert_refused_in_the_same_call(&bridge, "a Login frame");
    // A claim that is down proves nothing if no read was ever asked: the next frame settles the
    // refusal into the source's backoff, which only a read that was emitted and refused can leave.
    crate::app::adapters::stores::with_refused_hubs_launches_for_test(|| {
        frame(&mut d, &mut bridge, AppArg::Login, tick(1), vec![]);
    });
    let (claimed, retry, _) = bridge.stores.browse.borrow().discovery_policy_for_test();
    assert!(!claimed && retry > 0, "the Login frame asked for the discovery and it was refused (retry_cd {retry})");
}

/// The same frame on Onboard, whose Tick also asks for the discovery pass through the live arm.
#[test]
fn an_onboard_frame_starts_the_read_its_step_emitted_in_the_same_call() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    one_server();
    let mut d = Dispatcher::<AppHost>::new();
    let mut bridge = Bridge::for_test(|| 0);
    crate::app::adapters::stores::with_refused_hubs_launches_for_test(|| {
        frame(&mut d, &mut bridge, AppArg::Onboard, tick(0), vec![]);
    });
    assert_refused_in_the_same_call(&bridge, "an Onboard frame");
}

/// The controlled Home arm: a refused launch is recorded as not admitted, and its claim is down
/// when the arm returns (the job is admitted inside the arm, never left on the outbox).
#[test]
fn the_controlled_discovery_arm_records_a_refused_launch_as_not_admitted() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    one_server();
    let mt = unsafe { plx_base::task::MainThread::assume() };
    let initial = super::super::bootstrap::Initial::synthetic_home(17, 32517, None).unwrap();
    let mut bridge = Bridge::controlled_home(|| 0, &initial, &mt, false);
    crate::app::adapters::stores::with_refused_hubs_launches_for_test(|| {
        deliver_discovery_work(&mut bridge);
    });
    let requests = bridge.take_resource_requests();
    assert_eq!(requests.len(), 1, "one discovery request was recorded: {requests:?}");
    assert_eq!(requests[0]["admitted"], serde_json::json!(false), "the refusal is the recorded answer");
    assert_refused_in_the_same_call(&bridge, "the controlled discovery arm");
    assert_eq!(bridge.controlled_failure(), None);
}
