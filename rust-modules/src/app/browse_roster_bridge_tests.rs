//! S4 host capture and same-call drain contracts; no network or account writes.
use super::*;
use plx_data::stores::{StoreJob, tape};
use plx_plex::plex::{self, probe::Outcome, GrantEvidence};

struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        plx_base::task::drain_workers_for_test();
        tape::reset_for_test();
        plex::reset_servers_for_test();
    }
}
fn servers() -> (plex::ServerId, plex::ServerId) {
    plex::reset_servers_for_test();
    let a = plex::register_for_test("s4-a", "127.0.0.1", 9, "a", "cid");
    let b = plex::register_for_test("s4-b", "127.0.0.1", 9, "b", "cid");
    assert!(plex::set_current(a));
    plex::describe_server(a, "Before", "owner", GrantEvidence { owned: false, home: false, owner_id: 42 });
    (a, b)
}

#[test]
fn registry_facts_feed_step_and_view_capture_without_admissions_or_late_jobs() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let (a, _) = servers();
    let mut bridge = Bridge::for_test(|| 0);
    bridge.store_runner = super::super::adapters::stores::StoreRunner::held_all();
    tape::init(None, false);
    tape::begin(Default::default(), Default::default());
    bridge.browse_run(plx_data::stores::browse::BrowseCmd::RecheckShares);
    assert_eq!(bridge.client_keys.current(), a);
    assert_eq!(bridge.client_keys.facts(a).unwrap().machine_id, "s4-a");
    assert_eq!(bridge.client_keys.facts(a).unwrap().described.unwrap().name, "Before");
    let mut dispatcher = Dispatcher::<AppHost>::new();
    assert!(bridge.capture_views(&mut dispatcher).0);
    assert_eq!(bridge.browse_directory().sources()[0].1.name, "Before");
    assert!(bridge.take_held_store_jobs_for_test().is_empty(), "capture never emits work");
    bridge.assert_no_store_jobs_left_for_test();
    assert_eq!(bridge.late_store_jobs, 0);
    assert_eq!(tape::finish(), (Vec::new(), None));
}

#[test]
fn view_only_current_probe_and_description_changes_publish_once_and_keep_old_rows() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let (a, b) = servers();
    let mut bridge = Bridge::for_test(|| 0);
    let mut dispatcher = Dispatcher::<AppHost>::new();
    bridge.capture_views(&mut dispatcher);
    bridge.stores.take_notices();
    let old = bridge.directory.clone();
    assert!(!bridge.capture_views(&mut dispatcher).0);
    assert!(bridge.stores.take_notices().is_empty());
    assert!(bridge.directory.same_publication(&old));
    assert!(plex::set_current(b));
    assert!(bridge.capture_views(&mut dispatcher).0);
    assert!(bridge.stores.take_notices().iter().any(|(id, _)| *id == StoreId::Browse));
    assert!(!bridge.capture_views(&mut dispatcher).0);
    assert!(bridge.stores.take_notices().is_empty());
    plex::publish_probe_result(a, Outcome::Unreachable);
    assert!(bridge.capture_views(&mut dispatcher).0);
    assert_eq!(bridge.browse_directory().sources()[0].1.state, plx_data::browse::SourceState::Unreachable);
    plex::describe_server(a, "Before", "", GrantEvidence { owned: true, home: false, owner_id: 0 });
    assert!(bridge.capture_views(&mut dispatcher).0);
    assert_eq!(bridge.browse_directory().sources()[0].1.name, "Before");
    assert_eq!(bridge.browse_directory().sources()[0].1.handle, "");

    assert_eq!(old.view().sources()[0].1.name, "Before");
    assert_eq!(old.view().sources()[0].1.handle, "owner");

    bridge.stores.take_notices();
    assert!(!bridge.capture_views(&mut dispatcher).0);
    assert!(bridge.stores.take_notices().is_empty());
    bridge.assert_no_store_jobs_left_for_test();
    assert_eq!(bridge.late_store_jobs, 0);
}

#[test]
fn discovery_preserves_original_job_and_same_call_drain_under_held_and_live_refusal() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    let (a, _) = servers();
    let mut held = Bridge::for_test(|| 0);
    held.store_runner = super::super::adapters::stores::StoreRunner::held_all();
    let _ = held.browse_discover_pump();
    held.assert_no_store_jobs_left_for_test();
    assert_eq!(held.late_store_jobs, 0);
    let jobs = held.take_held_store_jobs_for_test();
    assert_eq!(jobs.len(), 1);
    let StoreJob::BrowseDiscovery(job, port) = jobs.into_iter().next().unwrap() else { panic!("original discovery kind") };
    assert_eq!(job.req.server().0, a);
    plx_machine::job::Port::refused(&port, job.addr);
    let mut live = Bridge::for_test(|| 0);
    live.store_runner = super::super::adapters::stores::StoreRunner::live();
    super::super::adapters::stores::with_refused_hubs_launches_for_test(|| {
        let _ = live.browse_discover_pump();
    });
    assert!(!live.stores.browse.borrow().discovery_owed());
    live.assert_no_store_jobs_left_for_test();
    assert_eq!(live.late_store_jobs, 0);
}
