//! Same-call M4 landing drains, Direct admission neutrality, and controlled-owner source isolation.
use super::*;
use plx_data::metadata::{self, Detail};
use plx_data::stores::{StoreJob, tape};
use plx_machine::job::Port;

struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        plx_base::task::drain_workers_for_test();
        tape::reset_for_test();
        plx_machine::landgate::disarm();
        plx_plex::plex::reset_servers_for_test();
    }
}
fn servers() -> plx_plex::plex::ServerId {
    plx_plex::plex::reset_servers_for_test();
    let a = plx_plex::plex::register_for_test("bridge-m4-a", "127.0.0.1", 9, "a", "cid");
    plx_plex::plex::register_for_test("bridge-m4-b", "127.0.0.1", 9, "b", "cid");
    a
}
fn ready_detail(bridge: &mut Bridge, sid: plx_plex::plex::ServerId) {
    let store = &mut bridge.stores.metadata;
    let gen = metadata::begin_detail_for_test(store.adapter_ref(), sid, "item");
    store.detail_port().done(plx_machine::machine::Addr {
        to: MachineId::Store(StoreId::Metadata.ord()), req: plx_machine::machine::RequestId(gen),
    }, ((sid, "item".into()), Some(Detail { sid, rk: "item".into(), guid: "plex://movie/m4".into(),
        source_override: Some("live-only".into()), ..Default::default() })));
}

#[test]
fn detail_landing_emits_and_drains_alt_in_the_same_bridge_call_live_and_controlled() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    for controlled in [false, true] {
        tape::reset_for_test();
        let sid = servers();
        let mut bridge = Bridge::for_test(|| 0);
        bridge.store_runner = super::super::adapters::stores::StoreRunner::held_all();
        if controlled {
            tape::init(None, false);
            tape::begin(Default::default(), Default::default());
            bridge.stores.metadata.arm_detail_tracker(true);
        }
        ready_detail(&mut bridge, sid);
        assert!(bridge.metadata_pump_detail());
        bridge.assert_no_store_jobs_left_for_test();
        assert_eq!(bridge.late_store_jobs, 0);
        let jobs = bridge.take_held_store_jobs_for_test();
        assert_eq!(jobs.len(), 1, "detail landing's alt request drained in THIS call");
        let StoreJob::MetaAltSources(job, port) = jobs.into_iter().next().unwrap() else { panic!("alt job") };
        assert_eq!(job.req.sid, sid);
        assert_eq!(job.req.others.len(), 2);
        assert_eq!(job.req.keys, bridge.client_keys);
        if controlled {
            assert!(bridge.metadata_view().current().unwrap().source_override.is_none());
            assert_eq!(tape::finish(), (Vec::new(), None), "Direct alt has no admission bytes");
        }
        port.refused(job.addr);
    }
}

#[test]
fn aggregate_metadata_pump_also_drains_landing_jobs_before_return() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let sid = servers();
    let mut bridge = Bridge::for_test(|| 0);
    bridge.store_runner = super::super::adapters::stores::StoreRunner::held_all();
    ready_detail(&mut bridge, sid);
    assert!(bridge.metadata_pump());
    bridge.assert_no_store_jobs_left_for_test();
    let jobs = bridge.take_held_store_jobs_for_test();
    assert_eq!(jobs.len(), 1);
    let StoreJob::MetaAltSources(job, port) = jobs.into_iter().next().unwrap() else { panic!("alt job") };
    port.refused(job.addr);
}

#[test]
fn direct_alt_drain_adds_no_admission_bytes_in_record_or_replay() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    for replay in [false, true] {
        tape::reset_for_test();
        let sid = servers();
        let mut bridge = Bridge::for_test(|| 0);
        bridge.store_runner = super::super::adapters::stores::StoreRunner::held_all();
        ready_detail(&mut bridge, sid);
        // Explicit input store compatibility path; it owes the jobs to this caller.
        super::super::adapters::stores::refill_client_keys(&mut bridge.client_keys);
        assert!(bridge.stores.metadata.pump_detail(&bridge.client_keys, false));
        let jobs = bridge.stores.metadata.take_jobs();
        assert_eq!(jobs.len(), 1);
        tape::init(None, replay);
        tape::begin(Default::default(), Default::default());
        assert_eq!(bridge.start_jobs(jobs), 1);
        assert_eq!(tape::finish(), (Vec::new(), None));
        let held = bridge.take_held_store_jobs_for_test();
        let StoreJob::MetaAltSources(job, port) = held.into_iter().next().unwrap() else { panic!("alt job") };
        port.refused(job.addr);
        bridge.assert_no_store_jobs_left_for_test();
    }
}

#[test]
fn entering_controlled_home_never_reuses_a_live_detail_source_override() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let mut live = Bridge::for_test(|| 0);
    metadata::set_current_for_test(live.stores.metadata.state_mut(), Some(Detail {
        source_override: Some("live-only".into()), ..Default::default()
    }));
    let initial = super::super::bootstrap::Initial::synthetic_home(41, 17, None).unwrap();
    let mt = unsafe { plx_base::task::MainThread::assume() };
    let controlled = Bridge::controlled_home(|| 0, &initial, &mt, false);
    assert_eq!(live.metadata_view().current().unwrap().source(), "live-only");
    assert!(controlled.metadata_view().current().is_none(),
        "the existing controlled host boundary constructs a fresh Metadata owner, not a live projection");
    assert!(!std::sync::Arc::ptr_eq(&live.stores.metadata.adapter_for_test(),
        &controlled.stores.metadata.adapter_for_test()));
}

#[test]
fn ready_real_alt_wins_fallback_registration_once_then_next_pass_observes_roster() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let sid = servers();
    let mut bridge = Bridge::for_test(|| 0);
    bridge.store_runner = super::super::adapters::stores::StoreRunner::held_all();
    ready_detail(&mut bridge, sid);
    assert!(bridge.metadata_pump_detail());
    let held = bridge.take_held_store_jobs_for_test();
    let StoreJob::MetaAltSources(job, port) = held.into_iter().next().unwrap() else { panic!("alt job") };
    port.done(job.addr, vec![metadata::AltCopy { sid, rk: "real".into(), ..Default::default() }]);
    let _ = bridge.stores.metadata.take_notice();
    let before = bridge.stores.metadata.gen();
    let roster_before = plx_plex::plex::server_roster_gen();
    assert!(bridge.metadata_pump_alt_sources_with(|request, _library| {
        // Script the host's fallback registration, AFTER begin has retained the READY answer's
        // roster. Host unit tests separately grade token/Origin/pin/facts registration behavior.
        let theirs = plx_plex::plex::register_for_test("bridge-m4-fallback", "127.0.0.1", 9, "t", "cid");
        assert_ne!(plx_plex::plex::server_roster_gen(), roster_before);
        let list = metadata::alt_stand_in("friend", "Shelf", &request.rk, &request.res,
            request.dur_ms, sid, theirs);
        request.answer(Some(list))
    }));
    assert_eq!(bridge.metadata_view().alt_copies(sid, "item")[0].rk, "real", "real wins SAME call");
    assert_eq!(bridge.stores.metadata.gen(), before + 1, "one folded notice, not stand-in then real notices");
    assert_eq!(bridge.client_keys, job.req.keys, "no intervening refill after registration");
    assert!(bridge.stores.metadata.take_notice().is_some());
    assert!(bridge.stores.metadata.take_notice().is_none());
    bridge.metadata_pump_alt_sources_with(|_, _| panic!("rk chance already consumed"));
    assert_eq!(bridge.client_keys.iter().count(), 3, "NEXT pass refills newly registered roster");
    assert!(!metadata::alt_sources_owed_for_test(bridge.stores.metadata.adapter_ref()));
    bridge.assert_no_store_jobs_left_for_test();
}
