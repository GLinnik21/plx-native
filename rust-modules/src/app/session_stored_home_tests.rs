//! Stored-home observation contracts through the real owner and actual disk/registry resources.
//! Synthetic admitted observations here do not claim account discovery/network policy coverage.
use super::*;
use crate::auth::owner::{SessionEnvelope, SessionEvent, SessionWork};
use crate::plex::session::{self, Session, SourceRef, ServerRef, UserRef};

struct Cleanup<'a>(&'a crate::task::MainThread);
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        crate::plex::reset_servers_for_test();
        session::ProfilePublisher::new(self.0).publish(None, 0);
    }
}

fn source(address: &str, token: &str) -> SourceRef {
    SourceRef { machine_id: "stored-machine".into(), name: "Synthetic".into(), address: address.into(),
        port: 32400, origin_url: format!("http://{address}:32400"), token: token.into(), owned: true,
        tier: Some(crate::plex::probe::Location::Local), ..Default::default() }
}

fn saved() -> Session {
    let source = source("127.0.0.1", "profile-token-a");
    Session { client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
        user: UserRef { uuid: "synthetic-profile".into(), token: source.token.clone(), ..Default::default() },
        server: ServerRef { machine_id: source.machine_id.clone(), address: source.address.clone(),
            port: source.port, origin_url: source.origin_url.clone(), token: source.token.clone(), ..Default::default() },
        sources: vec![source], ..Default::default() }
}

fn frame(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, records: Vec<SessionEnvelope>) {
    let results = records.into_iter().map(|r| (r.addr, AppMsg::Session(SessionEvent::Result(r)))).collect();
    d.frame_with(rig, Tick::default(), Vec::new(), results, &mut NoTap, false);
}

fn command(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, cmd: crate::auth::SessionCmd) {
    execute_session_command(d, cmd);
    frame(rig, d, Vec::new());
}

fn inject_roster(rig: &mut Bridge, expected: crate::auth::SessionIdentity) {
    inject_roster_terminal(rig, expected.clone(), expected);
}

fn inject_roster_terminal(rig: &mut Bridge, expected: crate::auth::SessionIdentity,
    terminal_expected: crate::auth::SessionIdentity) {
    let req = rig.session.snapshot_init().next_req + 1;
    let epoch = rig.auth_read().0.flow_epoch;
    rig.session_adapter.inject_fixture_work(req, move |output, input| {
        assert!(matches!(input, SessionWork::ServerRoster { .. }));
        let resource: crate::plex::account::Resource = serde_json::from_value(serde_json::json!({
            "clientIdentifier":"stored-machine", "provides":"server"
        })).unwrap();
        output.progress(crate::auth::AuthProgress::Registry(crate::auth::RegistryProgress::Settled {
            epoch, expected: Some(expected.clone()),
            probe: crate::auth::settled_probe(&crate::plex::probe::plan(&resource),
                crate::plex::probe::Outcome::Reachable, Some(crate::plex::probe::Location::Local)),
        })).unwrap();
        let roster = serde_json::from_value(serde_json::json!({
            "epoch":epoch, "expected":terminal_expected,
            "outcome":{"Reconcile":{
                "resources":[{"name":"Synthetic", "clientIdentifier":"stored-machine",
                    "provides":"server", "owned":true, "accessToken":"profile-token-b"}],
                "found":[source("127.0.0.2", "profile-token-b")], "household":[], "settled":[]
            }}
        })).unwrap();
        output.complete(crate::auth::AuthProgress::ServerRoster(roster)).unwrap();
    });
}

fn prove_home_observations(conflicting_owner: bool) {
    let _lock = crate::testlock::serial();
    let mt = unsafe { crate::task::MainThread::assume() };
    let tmp = session::TempSession::new("stored-home-owned-observations");
    let _cleanup = Cleanup(&mt);
    tmp.assert_only_target();
    crate::plex::reset_servers_for_test();
    let disk = saved();
    session::save(&disk);
    let before = std::fs::read(tmp.path()).unwrap();
    let id = crate::plex::register_for_test("stored-machine", "127.0.0.1", 32400,
        "profile-token-a", "synthetic-client");
    let mut owner_input = disk.clone();
    if conflicting_owner {
        owner_input.account_token = "different-account".into();
        owner_input.user.uuid = "different-profile".into();
    }
    let mut init = crate::auth::SessionInit::captured(owner_input);
    init.epoch = u64::from(u32::MAX) + 97;
    if conflicting_owner { init.phase = crate::auth::Phase::Ready; }
    let mut rig = Bridge::for_session_test(init);
    rig.session_adapter = super::super::adapters::session::SessionAdapter::live_resources_for_test(&mt, false);
    let mut d = Dispatcher::<AppHost>::new();
    if !conflicting_owner {
        command(&mut rig, &mut d, crate::auth::SessionCmd::ResumeStored);
        assert!(rig.take_session_ready().is_some());
        assert!(rig.take_session_ready().is_none());
        assert_eq!(std::fs::read(tmp.path()).unwrap(), before);
    }
    // Deliberately use old disk identity in the conflicting case: an observation-contract test,
    // not a claim that the correctly captured worker would fabricate this payload.
    inject_roster(&mut rig, crate::auth::SessionIdentity::of(&disk));
    command(&mut rig, &mut d, crate::auth::SessionCmd::RefreshRoster);
    let mut records = rig.session_adapter.take_results();
    assert_eq!(records.len(), 2);
    let roster = records.pop().unwrap();
    frame(&mut rig, &mut d, records);
    assert_eq!(crate::plex::server_probe_result(id), if conflicting_owner { None }
        else { Some(crate::plex::probe::Outcome::Reachable) });
    frame(&mut rig, &mut d, vec![roster]);
    let address = if conflicting_owner { "127.0.0.1" } else { "127.0.0.2" };
    assert_eq!(session::peek().sources[0].address, address);
    assert_eq!(crate::plex::client_for(id).unwrap().host(), address);
    assert!(rig.session.snapshot_init().persisted.can_go_local(),
        "stored Home now has an explicit owner; it does not require a default global Ctl");

    let req = rig.session.snapshot_init().next_req + 1;
    let epoch = rig.auth_read().0.flow_epoch;
    let wrong_identity = crate::auth::owner::Identity::of(&disk);
    rig.session_adapter.inject_fixture_work(req, move |output, input| {
        let SessionWork::Endpoint { expected, lifecycle, machine_id, .. } = input else { panic!("endpoint capture") };
        output.complete(crate::auth::endpoint_work_fact(epoch,
            if conflicting_owner { wrong_identity } else { expected }, lifecycle, machine_id,
            Some(source("127.0.0.3", "account-token-not-authoritative")))).unwrap();
    });
    command(&mut rig, &mut d, crate::auth::SessionCmd::RequestEndpoint { sid: id });
    let records = rig.session_adapter.take_results();
    assert_eq!(records.len(), 1);
    let original = records[0].clone();
    if conflicting_owner {
        // Bridge-boundary negatives: changed arrival is not an admitted receipt. These prove
        // end-to-end rejection here, NOT discrimination of the owner's conversion guards.
        for variant in 0..7 {
            let mut bad = original.clone();
            bad.arrival += 100 + variant;
            match variant {
                0 => bad.addr.to = crate::ui::machine::MachineId::Player,
                1 => bad.addr.req.0 += 100,
                2 => bad.key.epoch += 1_u64 << 32,
                3 => bad.admission.0 += 100,
                4 => bad.lifecycle = None,
                5 => bad.terminal = false,
                _ => bad.key.op = crate::auth::owner::SessionOp::HomeRoster,
            }
            frame(&mut rig, &mut d, vec![bad]);
            assert!(rig.session.snapshot_init().pending.contains_key(&req), "variant {variant}");
            assert_eq!(std::fs::read(tmp.path()).unwrap(), before);
            assert_eq!(crate::plex::client_for(id).unwrap().host(), "127.0.0.1");
            assert_eq!(crate::plex::server_probe_result(id), None);
        }
    }
    frame(&mut rig, &mut d, records);
    let address = if conflicting_owner { "127.0.0.1" } else { "127.0.0.3" };
    let landed = session::peek();
    assert_eq!(landed.sources[0].address, address);
    assert_eq!(landed.sources[0].token, if conflicting_owner { "profile-token-a" } else { "profile-token-b" });
    assert_eq!(crate::plex::client_for(id).unwrap().host(), address);
    if conflicting_owner { assert_eq!(std::fs::read(tmp.path()).unwrap(), before); }
    assert!(!rig.session.snapshot_init().pending.contains_key(&req),
        "a rejected but matching terminal must release its own endpoint flight");
    assert!(rig.session.snapshot_init().pending.is_empty());
    if conflicting_owner {
        command(&mut rig, &mut d, crate::auth::SessionCmd::RequestEndpoint { sid: id });
        let successor = rig.session.snapshot_init().next_req;
        assert!(successor > req);
        assert!(rig.session.snapshot_init().pending.contains_key(&successor));
        // Bridge rejects this already-ACKed receipt before owner delivery: end-to-end duplicate
        // protection, not a claim that this fixture enters the owner's successor guard.
        frame(&mut rig, &mut d, vec![original.clone(), original]);
        assert!(rig.session.snapshot_init().pending.contains_key(&successor));
        assert_eq!(std::fs::read(tmp.path()).unwrap(), before);
        let refused = rig.session_adapter.take_results();
        frame(&mut rig, &mut d, refused);
        assert!(!rig.session.snapshot_init().pending.contains_key(&successor));
    }
}

#[test]
fn stored_home_without_auth_ctl_accepts_registry_roster_and_endpoint_observations() {
    prove_home_observations(false);
}

#[test]
fn conflicting_live_ctl_rejects_stored_home_observations() {
    prove_home_observations(true);
}

#[test]
fn admitted_endpoint_lifecycle_and_nonterminal_rejections_preserve_current_interest() {
    let _lock = crate::testlock::serial();
    let mt = unsafe { crate::task::MainThread::assume() };
    for nonterminal in [false, true] {
        let tmp = session::TempSession::new("admitted-endpoint-negative");
        let _cleanup = Cleanup(&mt);
        tmp.assert_only_target();
        crate::plex::reset_servers_for_test();
        let disk = saved();
        session::save(&disk);
        let before = std::fs::read(tmp.path()).unwrap();
        let id = crate::plex::register_for_test("stored-machine", "127.0.0.1", 32400,
            "profile-token-a", "synthetic-client");
        let mut rig = Bridge::for_session_test(crate::auth::SessionInit::captured(disk));
        rig.session_adapter = super::super::adapters::session::SessionAdapter::live_resources_for_test(&mt, false);
        let mut d = Dispatcher::<AppHost>::new();
        let req = rig.session.snapshot_init().next_req + 1;
        let epoch = rig.auth_read().0.flow_epoch;
        rig.session_adapter.inject_fixture_work(req, move |output, input| {
            let SessionWork::Endpoint { expected, lifecycle, machine_id, .. } = input else { panic!("endpoint capture") };
            output.complete(crate::auth::endpoint_work_fact(epoch, expected, lifecycle, machine_id,
                Some(source("127.0.0.9", "unused-payload-token")))).unwrap();
        });
        command(&mut rig, &mut d, crate::auth::SessionCmd::RequestEndpoint { sid: id });
        let mut records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 1);
        let mut bad = records.pop().unwrap();
        let receipt = crate::auth::owner::Receipt::of(&bad);
        let admission = bad.admission;
        if nonterminal { bad.terminal = false; } else { bad.lifecycle = None; }
        assert!(crate::auth::owner::Receipt::of(&bad) == receipt);
        assert!(bad.admission == admission);
        assert!(rig.session_adapter.admitted(&bad), "must reach owner, not fail Bridge admission");
        frame(&mut rig, &mut d, vec![bad.clone()]);
        assert!(rig.session.snapshot_init().pending.contains_key(&req),
            "admitted malformed lifecycle/nonterminal cannot retire the current request");
        assert!(!rig.session_adapter.admitted(&bad), "unique delivered receipt is ACKed");
        assert_eq!(std::fs::read(tmp.path()).unwrap(), before);
        assert_eq!(crate::plex::client_for(id).unwrap().host(), "127.0.0.1");
        assert_eq!(crate::plex::server_probe_result(id), None);
        assert!(rig.take_session_ready().is_none());
        // This receipt was consumed; do not relabel/reuse it as a valid terminal. Real restart
        // retires the remaining logical interest (physical producer already returned above).
        command(&mut rig, &mut d, crate::auth::SessionCmd::StartLogin);
        assert!(!rig.session.snapshot_init().pending.contains_key(&req));
    }
}

#[test]
fn rejected_terminal_waits_in_owner_fifo_until_preceding_commit_ack() {
    let mut witnessed = false;
    for remaining in 1..20 {
        let disk = saved();
        let mut rig = Bridge::for_session_test(crate::auth::SessionInit::captured(disk.clone()));
        rig.session_adapter.fixture_resources().disk = disk.clone();
        let mut d = Dispatcher::<AppHost>::new();
        let mut wrong = disk.clone();
        wrong.account_token = "different-account".into();
        inject_roster_terminal(&mut rig, crate::auth::SessionIdentity::of(&disk),
            crate::auth::SessionIdentity::of(&wrong));
        command(&mut rig, &mut d, crate::auth::SessionCmd::RefreshRoster);
        let records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 2);
        let req = records[0].addr.req.0;
        for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST - remaining {
            execute_session_command(&mut d, crate::auth::SessionCmd::NoteDeleteLeftovers(0));
        }
        frame(&mut rig, &mut d, records.clone());
        let state = rig.session.snapshot_init();
        if state.pending_commit.is_none() || state.inbox.len() != 1 { continue; }
        witnessed = true;
        assert!(state.pending.contains_key(&req), "busy is not rejection or completion");
        assert!(records.iter().all(|r| rig.session_adapter.admitted(r)));
        let writes_before = rig.session_adapter.fixture_resources().registry_writes.len();
        frame(&mut rig, &mut d, Vec::new());
        assert!(!rig.session.snapshot_init().pending.contains_key(&req));
        assert!(rig.session.snapshot_init().pending_commit.is_none());
        assert!(rig.session.snapshot_init().inbox.is_empty());
        assert!(records.iter().all(|r| !rig.session_adapter.admitted(r)));
        let resources = rig.session_adapter.fixture_resources();
        assert!(resources.registry_writes.len() >= writes_before);
        assert_eq!(resources.disk.sources[0].address, "127.0.0.1");
        assert!(resources.registry_writes.iter().all(|plan|
            matches!(plan, crate::auth::owner::RegistryPlan::Probe(_))));
        assert!(rig.take_session_ready().is_none());
        break;
    }
    assert!(witnessed, "must carry a rejected terminal behind a real pending resource commit");
}
