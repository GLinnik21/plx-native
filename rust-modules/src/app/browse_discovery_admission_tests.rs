//! A controlled Home admits the Library's source discovery as a Home job, in record mode
//! (`replay == false`: the launch runs and its answer is written down) and in replay mode (the
//! recording answers, nothing runs). `StoreJob::BrowseDiscovery` is `Admission::Home`, which is only
//! sound because exactly one controlled entry drains it:
//!
//! - the arm of `StoreWork::BrowseDiscovery` records the sections request as a literal, in the
//!   shape the replay fixtures hold (`bootstrap::HomeIo::admit`), and leaves nothing on the outbox;
//! - under replay the recorded line answers the same request, nothing is launched, and the claim
//!   the step raised stays up until the recorded `Discovery` landing is applied;
//! - the effect encoder (`bootstrap::effects`) encodes `StoreWork::BrowseDiscovery` and `Hubs` and
//!   refuses every other way to step or retry the Library, so no recording holds a discovery job
//!   that was not started inside the arm.

use std::collections::VecDeque;

use plx_data::stores::browse::BrowseCmd;
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

/// One server registered at a port nothing listens on, so a read that does start fails at once.
fn one_server() -> (ServerId, &'static plx_plex::plex::Client) {
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("discovery-admission", "127.0.0.1", 9, "t", "cid");
    assert_eq!(sid.raw(), 0, "the first server registered is slot 0, which the recorded descriptors name");
    assert!(plx_plex::plex::set_current(sid));
    (sid, plx_plex::plex::client_for(sid).expect("registered"))
}

fn controlled(replay: bool) -> Bridge {
    let mt = unsafe { plx_base::task::MainThread::assume() };
    let initial = crate::app::bootstrap::Initial::synthetic_home(17, 32517, None).unwrap();
    Bridge::controlled_home(|| 0, &initial, &mt, replay)
}

/// One message to the Browse store, as the central dispatcher delivers it.
fn deliver(bridge: &mut Bridge, msg: AppMsg) {
    let parts = CxParts { tick: Tick::default(), press: Default::default(),
        focus: Default::default(), owner: InputOwner::Entry(EntryId(0)) };
    let mut present = plx_machine::present::Present::default();
    let mut out = Vec::new();
    let to = MachineId::Store(StoreId::Browse.ord());
    let mut fx = Effects::new(&mut out, to, &mut present);
    bridge.deliver(to, &msg, &parts, &mut fx);
}

/// The sections request of the first step, as the fixtures record it.
fn sections_request(client: &plx_plex::plex::Client, admitted: bool) -> serde_json::Value {
    serde_json::json!({"epoch":0,"source":0,"sid":0,"client":client.instance_gen(),
        "token_gen":client.token_gen(),"name":true,"sections":true,"counts":[],"admitted":admitted})
}

/// A discovery landing for source 0 that brings a Movies section, as a recording carries it.
fn sections_landing(bridge: &Bridge, client: &'static plx_plex::plex::Client) -> plx_data::browse::record::Result {
    let epoch = bridge.stores.browse.borrow().table_epoch_for_test();
    plx_data::browse::record::decode(serde_json::json!({
        "kind": "discovery", "version": 1, "epoch": epoch, "source": 0, "sid": client.id().raw(),
        "client": client.instance_gen(), "token_gen": client.token_gen(), "name": "home",
        "what": { "Sections": [[1, "Movies", "movie"]] },
    }), |instance| (instance == client.instance_gen()).then_some(client)).expect("a well-formed landing")
}

/// Record mode: the arm admits the job it drained, the line is the literal, the launch ran (the
/// claim is up until a landing applies), and nothing was left for a later frame to start.
#[test]
fn recording_the_discovery_arm_writes_the_sections_request_and_starts_the_job_inside_the_arm() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let (_sid, client) = one_server();
    let mut bridge = controlled(false);

    deliver(&mut bridge, AppMsg::StoreWork(StoreWork::BrowseDiscovery));

    assert_eq!(bridge.take_resource_requests(), vec![sections_request(client, true)]);
    assert!(bridge.stores.browse.borrow().discovery_owed(), "the launch ran: its claim is up");
    assert_eq!(bridge.controlled_failure(), None);
    bridge.assert_no_store_jobs_left_for_test();
}

/// Replay mode: the recorded line answers the very request the step emits, no launch happens (no
/// worker, no failure), and the claim stays up until the recorded landing applies and lowers it.
#[test]
fn replaying_the_discovery_arm_answers_from_the_line_and_launches_nothing() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let (_sid, client) = one_server();
    let mut bridge = controlled(true);
    bridge.supply_admissions(VecDeque::from([sections_request(client, true)]));

    deliver(&mut bridge, AppMsg::StoreWork(StoreWork::BrowseDiscovery));

    assert_eq!(bridge.take_resource_requests(), vec![sections_request(client, true)],
        "the replayed run records the same line as the recorded one");
    assert_eq!(bridge.controlled_failure(), None, "the recorded line matched: nothing mismatched");
    bridge.assert_no_store_jobs_left_for_test();
    assert!(bridge.stores.browse.borrow().discovery_owed(),
        "nothing launched, and no landing yet: the claim waits for the recording's landing");

    let landing = sections_landing(&bridge, client);
    deliver(&mut bridge, AppMsg::Store(StoreCmd::Browse(BrowseCmd::Discovery(landing))));
    assert!(!bridge.stores.browse.borrow().discovery_owed(), "the recorded landing lowered the claim");
    assert_eq!(bridge.controlled_failure(), None);
}

/// Replay refuses a line that is not the request the step emits: the run fails instead of the
/// store guessing.
#[test]
fn replaying_the_discovery_arm_against_a_different_line_fails_the_run() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let (_sid, client) = one_server();
    let mut bridge = controlled(true);
    let mut line = sections_request(client, true);
    line["counts"] = serde_json::json!([1]);
    bridge.supply_admissions(VecDeque::from([line]));

    deliver(&mut bridge, AppMsg::StoreWork(StoreWork::BrowseDiscovery));

    assert_eq!(bridge.controlled_failure(), Some("mismatched synchronous admission"));
}

/// The encoder accepts the two pumps a controlled run delivers (`Hubs` and `BrowseDiscovery`) and
/// refuses the Library's other pump and every discovery command the store takes outside a
/// controlled run, so a frame carrying one fails the run before it reaches the Bridge.
#[test]
fn the_controlled_encoder_encodes_only_the_discovery_pump_and_refuses_its_retries() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let encode_work = |work: StoreWork| crate::app::bootstrap::effects::app(&AppFx::StoreWork(work));
    let encode_cmd = |cmd: BrowseCmd| {
        crate::app::bootstrap::effects::app(&AppFx::Store(StoreId::Browse, StoreCmd::Browse(cmd)))
    };

    assert!(encode_work(StoreWork::BrowseDiscovery).is_ok());
    assert!(encode_work(StoreWork::Hubs).is_ok());
    assert_eq!(encode_work(StoreWork::Browse), Err("unsupported Home store work"));

    let (sid, _client) = one_server();
    for cmd in [BrowseCmd::RetryDiscovery, BrowseCmd::RetrySource { epoch: 0, sid }, BrowseCmd::RecheckShares] {
        assert_eq!(encode_cmd(cmd.clone()), Err("unsupported Home store command"), "{cmd:?}");
    }
}
