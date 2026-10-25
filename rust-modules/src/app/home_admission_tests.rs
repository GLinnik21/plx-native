//! What a controlled Home records for Hubs, observed from the `Bridge` that dispatches it. A
//! controlled run (`Bridge::controlled_home`, `replay == false`, the recording side) writes one
//! admission per request the stores ask for, in the order they are asked
//! (`bootstrap::HomeIo::admit`), and a replay compares that list byte for byte. The values are
//! pinned here as literals, independent of how the Hubs request reaches its worker: a store that
//! starts its own worker and a store that emits a job for the runner to start must record the
//! same bytes, and the Hubs request must still be recorded BEFORE the discovery request of the
//! same frame (fixtures 1, 6 and 12 carry hubs on line 16 and discovery on line 17).
//!
//! It is written against signatures that do not change when Hubs moves onto the job lane
//! (`Bridge::deliver` of `StoreCmd::Hubs` and `StoreWork::BrowseDiscovery`,
//! `take_resource_requests`, `assert_no_store_jobs_left_for_test`), so the same file compiles and
//! passes on both sides of that change, and stays unedited through every commit of the lane.

use plx_data::stores::hubs::HubsCmd;
use plx_data::stores::StoreWork;
use plx_plex::plex::ServerId;

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

/// One server registered at a port nothing listens on, so a fetch that does start fails at once,
/// and a controlled-home Bridge recording over it (`replay == false`: the launch runs and its
/// answer is written down).
fn recording_home() -> (Bridge, ServerId, &'static plx_plex::plex::Client) {
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("home-admission", "127.0.0.1", 9, "t", "cid");
    assert_eq!(sid.raw(), 0, "the first server registered is slot 0, which the recorded descriptors name");
    assert!(plx_plex::plex::set_current(sid));
    let client = plx_plex::plex::client_for(sid).expect("registered");
    let mt = unsafe { plx_base::task::MainThread::assume() };
    let initial = super::super::bootstrap::Initial::synthetic_home(17, 32517, None).unwrap();
    (Bridge::controlled_home(|| 0, &initial, &mt, false), sid, client)
}

/// One message to a store, as the central dispatcher delivers it.
fn deliver(bridge: &mut Bridge, store: StoreId, msg: AppMsg) {
    let parts = CxParts { tick: Tick::default(), press: Default::default(),
        focus: Default::default(), owner: InputOwner::Entry(EntryId(0)) };
    let mut present = plx_machine::present::Present::default();
    let mut out = Vec::new();
    let mut fx = Effects::new(&mut out, MachineId::Store(store.ord()), &mut present);
    bridge.deliver(MachineId::Store(store.ord()), &msg, &parts, &mut fx);
}

/// A refetch and the discovery work of the same frame record hubs first and discovery second, each
/// admitted, in the recorded shape. (The hubs `epoch` is the store's refetch generation, 1 after
/// the one refetch; the discovery `epoch` is Browse's own and is still 0.) The hubs `page` branch of
/// the descriptor needs a landed hub with a pageable shelf in a retained directory, so it is not
/// pinned here: a fresh store never emits a page request in one pump.
#[test]
fn a_refetch_then_discovery_in_one_frame_record_hubs_then_discovery() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let (mut bridge, _sid, client) = recording_home();

    deliver(&mut bridge, StoreId::Hubs, AppMsg::Store(StoreCmd::Hubs(HubsCmd::RefetchHubs)));
    deliver(&mut bridge, StoreId::Browse, AppMsg::StoreWork(StoreWork::BrowseDiscovery));

    let (instance, token) = (client.instance_gen(), client.token_gen());
    let requests = bridge.take_resource_requests();
    assert_eq!(requests, vec![
        serde_json::json!({"kind":"hubs","epoch":1,"req":1,"sid":0,"client":instance,
            "token_gen":token,"admitted":true}),
        serde_json::json!({"epoch":0,"source":0,"sid":0,"client":instance,"token_gen":token,
            "name":true,"sections":true,"counts":[],"admitted":true}),
    ], "hubs is recorded first, discovery second");
    assert_eq!(bridge.controlled_failure(), None);
    bridge.assert_no_store_jobs_left_for_test();
}

