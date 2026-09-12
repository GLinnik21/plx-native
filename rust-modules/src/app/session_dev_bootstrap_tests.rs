//! Production boot capture + Bridge authority tests, with synthetic inputs only.
use super::*;

fn frame(rig: &mut Bridge, d: &mut Dispatcher<AppHost>) {
    d.frame_with(rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
}

fn dev_fixture() -> Bridge {
    let saved = crate::plex::session::Session { client_id: "synthetic-device".into(),
        account_token: "synthetic-saved-a".into(), ..Default::default() };
    let dev = crate::plex::session::ServerRef { address: "127.0.0.2".into(), port: 32400,
        token: "synthetic-dev-b".into(), ..Default::default() };
    let mut rig = Bridge::for_session_test(super::super::boot::captured_session_for_boot(saved.clone(), Some(dev), Vec::new()));
    // Explicit resource baseline is the actual saved A, not sanitized in-memory auth input.
    rig.session_adapter.fixture_resources().disk = saved;
    rig
}

#[test]
fn dev_revoke_resource_completes_before_carried_ack_and_stale_ack_after_erase_is_inert() {
    use crate::auth::owner::{BootstrapAuthority, CommitReply, SessionEvent};
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    let mut witnessed = false;
    // Cut the normal dispatcher budget at each nearby step; require the precise post-resource,
    // pre-ACK cut, not merely the earlier carried-effect case. No dispatcher seam is changed.
    for remaining in 1..16 {
        let mut rig = dev_fixture();
        let mut d = Dispatcher::<AppHost>::new();
        let ran = Arc::new(AtomicUsize::new(0));
        let signal = Arc::clone(&ran);
        rig.session_adapter.inject_fixture_work(2, move |_, _| { signal.fetch_add(1, Ordering::AcqRel); });
        for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST - remaining {
            execute_session_command(&mut d, crate::auth::SessionCmd::NoteDeleteLeftovers(0));
        }
        execute_session_command(&mut d, crate::auth::SessionCmd::StartLogin);
        frame(&mut rig, &mut d);
        let revoked = rig.session_adapter.fixture_resources().registry_writes.iter()
            .any(|p| matches!(p, crate::auth::owner::RegistryPlan::Revoke));
        if !revoked || ran.load(Ordering::Acquire) != 0 { continue; }
        let state = rig.session.snapshot_init();
        let Some(commit) = state.pending_commit.as_ref() else { continue };
        witnessed = true;
        assert!(matches!(state.authority, BootstrapAuthority::DevPms { .. }));
        assert_eq!(state.phase, crate::auth::Phase::Creating);
        let restored: crate::auth::SessionInit = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(crate::auth::SessionMachine::from_init(restored).subhash(), rig.session.subhash(),
            "pending dev delta, reserved request and disk comparison survive init roundtrip");
        let ack = CommitReply { req: commit.req, epoch: commit.epoch, arrival: commit.arrival, accepted: true };
        frame(&mut rig, &mut d);
        assert_eq!(ran.load(Ordering::Acquire), 1);
        assert!(matches!(rig.session.snapshot_init().authority, BootstrapAuthority::Account { .. }));
        execute_session_command(&mut d, crate::auth::SessionCmd::EraseLocal);
        frame(&mut rig, &mut d);
        assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Deleted);
        let before = rig.session.subhash();
        d.emit(MachineId::Session, Fx::Deliver(MachineId::Session,
            Delivery::Machine(AppMsg::Session(SessionEvent::Commit(ack)))));
        frame(&mut rig, &mut d);
        assert_eq!(rig.session.subhash(), before);
        assert_eq!(ran.load(Ordering::Acquire), 1);
        assert!(rig.take_session_ready().is_none());
        break;
    }
    assert!(witnessed, "must actually carry ACK after Revoke executed");
}

#[test]
fn dev_login_preflights_both_request_slots_and_coalesces_pending_intent() {
    for (epoch, next_req) in [(u64::MAX, 0), (1, u32::MAX - 1), (1, u32::MAX)] {
        let mut init = dev_fixture().session.snapshot_init();
        init.epoch = epoch;
        init.next_req = next_req;
        let mut rig = Bridge::for_session_test(init);
        let before = rig.session.subhash();
        let mut d = Dispatcher::<AppHost>::new();
        execute_session_command(&mut d, crate::auth::SessionCmd::StartLogin);
        frame(&mut rig, &mut d);
        assert_eq!(rig.session.subhash(), before);
        assert!(rig.session_adapter.fixture_resources().registry_writes.is_empty());
    }
    let mut rig = dev_fixture();
    let mut d = Dispatcher::<AppHost>::new();
    execute_session_command(&mut d, crate::auth::SessionCmd::StartLogin);
    execute_session_command(&mut d, crate::auth::SessionCmd::StartLogin);
    frame(&mut rig, &mut d);
    assert_eq!(rig.auth_read().0.flow_epoch, 2);
    assert_eq!(rig.session.snapshot_init().next_req, 2);
    assert_eq!(rig.session_adapter.fixture_resources().registry_writes.iter()
        .filter(|p| matches!(p, crate::auth::owner::RegistryPlan::Revoke)).count(), 1);
}

#[test]
fn dev_erase_and_signout_retire_pending_boundaries_without_reinstalling_grants() {
    use crate::auth::owner::{BootstrapAuthority, CommitReply, CoordinatorAction, SessionEvent, SessionWork};
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    for sign_out in [false, true] {
        for boundary in 0..3 {
            let mut rig = dev_fixture();
            let mut d = Dispatcher::<AppHost>::new();
            let ran = Arc::new(AtomicUsize::new(0));
            if sign_out {
                let signal = Arc::clone(&ran);
                rig.session_adapter.inject_fixture_work(boundary + 1, move |_, input| {
                    let SessionWork::Login { client_id } = input else { panic!("sign-out must start clean Account") };
                    assert_eq!(client_id, "synthetic-client", "new fixture identity is minted only after erasure");
                    signal.fetch_add(1, Ordering::AcqRel);
                });
            }
            if boundary == 1 { execute_session_command(&mut d, crate::auth::SessionCmd::ActivateDevBootstrap); }
            if boundary == 2 { execute_session_command(&mut d, crate::auth::SessionCmd::StartLogin); }
            execute_session_command(&mut d, if sign_out { crate::auth::SessionCmd::SignOut } else { crate::auth::SessionCmd::EraseLocal });
            frame(&mut rig, &mut d);
            let state = rig.session.snapshot_init();
            assert!(matches!(state.authority, BootstrapAuthority::Account { ref extras } if extras.is_empty()));
            assert!(state.persisted.account_token.is_empty());
            assert!(state.disk_identity.account_token.is_empty());
            assert!(rig.session_adapter.fixture_resources().disk.account_token.is_empty());
            assert!(rig.session_adapter.fixture_resources().registry_writes.iter()
                .all(|p| !matches!(p, crate::auth::owner::RegistryPlan::DevInstall { .. })),
                "cancelled activation cannot execute its registry grant");
            assert_eq!(ran.load(Ordering::Acquire), usize::from(sign_out));
            assert_eq!(state.phase, if sign_out { crate::auth::Phase::Creating } else { crate::auth::Phase::Deleted });
            let events = &rig.session_adapter.fixture_resources().coordinator_events;
            assert!(matches!(events.first(), Some(CoordinatorAction::CloseTelemetry)));
            if sign_out { assert!(events.iter().any(|e| matches!(e, CoordinatorAction::SignInStarted))); }
            let before = rig.session.subhash();
            if boundary != 0 {
                d.emit(MachineId::Session, Fx::Deliver(MachineId::Session,
                    Delivery::Machine(AppMsg::Session(SessionEvent::Commit(CommitReply {
                        req: 1, epoch: u64::from(boundary), arrival: 0, accepted: true,
                    })))));
                frame(&mut rig, &mut d);
                assert_eq!(rig.session.subhash(), before);
                assert_eq!(ran.load(Ordering::Acquire), usize::from(sign_out));
            }
            assert!(rig.take_session_ready().is_none());
        }
    }
}

#[test]
fn carried_dev_ready_is_not_handed_off_after_erase() {
    let mut witnessed = false;
    for remaining in 1..16 {
        let mut rig = dev_fixture();
        let mut d = Dispatcher::<AppHost>::new();
        for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST - remaining {
            execute_session_command(&mut d, crate::auth::SessionCmd::NoteDeleteLeftovers(0));
        }
        execute_session_command(&mut d, crate::auth::SessionCmd::ActivateDevBootstrap);
        frame(&mut rig, &mut d);
        if rig.auth_read().0.phase != crate::auth::Phase::Ready || rig.session_ready.is_some() { continue; }
        witnessed = true;
        execute_session_command(&mut d, crate::auth::SessionCmd::EraseLocal);
        frame(&mut rig, &mut d);
        assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Deleted);
        assert!(rig.take_session_ready().is_none());
        assert!(matches!(rig.session_adapter.fixture_resources().registry_writes.last(),
            Some(crate::auth::owner::RegistryPlan::Revoke)));
        break;
    }
    assert!(witnessed, "actual Ready effect must be carried after activation ACK");
}

#[test]
fn mounted_login_try_again_recovers_dev_boundary_errors_through_revoke_ack() {
    use crate::auth::owner::{BootstrapAuthority, CommitReply, SessionEvent, SessionWork};
    use crate::plex::session::{Session, ServerRef};
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    let _lock = crate::testlock::serial();
    for failures in [1, 2] {
        let saved = Session { client_id: "synthetic-device".into(),
            account_token: "synthetic-saved-a".into(), ..Default::default() };
        let dev = ServerRef { address: "127.0.0.2".into(), port: 32400,
            token: "synthetic-dev-b".into(), ..Default::default() };
        let mut rig = Bridge::for_session_test(super::super::boot::captured_session_for_boot(saved, Some(dev), Vec::new()));
        let mut d = Dispatcher::<AppHost>::new();
        for attempt in 0..failures {
            let state = rig.session.snapshot_init();
            let (command, epoch) = if attempt == 0 {
                (crate::auth::SessionCmd::ActivateDevBootstrap, state.epoch)
            } else { (crate::auth::SessionCmd::StartLogin, state.epoch + 1) };
            execute_session_command(&mut d, command);
            // Inject an explicit negative resource reply through the real queue. No native
            // failure is claimed: this grades the ACK/UI protocol, not Revoke's infallible IO.
            d.emit(MachineId::Session, Fx::Deliver(MachineId::Session,
                Delivery::Machine(AppMsg::Session(SessionEvent::Commit(CommitReply {
                    req: state.next_req + 1, epoch, arrival: 0, accepted: false,
                })))));
            frame(&mut rig, &mut d);
            assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Error);
            assert!(matches!(rig.session.snapshot_init().authority, BootstrapAuthority::DevPms { .. }));
            assert!(rig.session.snapshot_init().pending.is_empty());
        }
        d.request(MachineId::Nav, NavOp::Root(AppArg::Login));
        frame(&mut rig, &mut d);
        assert_eq!(d.top_screen().unwrap().name(), "login");
        assert_eq!(d.focus_record().unwrap().1, 0, "real Error action is focused");
        let instance = d.top_page().unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&ran);
        let req = rig.session.snapshot_init().next_req + 2;
        rig.session_adapter.inject_fixture_work(req, move |_, input| {
            let SessionWork::Login { client_id } = input else { panic!("retry must start clean account Login") };
            assert_eq!(client_id, "synthetic-device");
            signal.store(true, Ordering::Release);
        });
        d.emit(MachineId::Input, Fx::Deliver(MachineId::Instance(instance),
            Delivery::Screen(ScreenEvent::Activate(0))));
        frame(&mut rig, &mut d);
        assert!(ran.load(Ordering::Acquire), "mounted Try Again must cross revoke ACK and launch clean Login");
        let state = rig.session.snapshot_init();
        assert!(matches!(state.authority, BootstrapAuthority::Account { ref extras } if extras.is_empty()));
        assert!(state.persisted.account_token.is_empty());
        assert!(state.committed_credentials.account_token.is_empty());
        assert!(rig.session_adapter.fixture_resources().registry_writes.iter()
            .any(|p| matches!(p, crate::auth::owner::RegistryPlan::Revoke)));
    }
}

struct Cleanup<'a>(&'a crate::task::MainThread);

#[test]
fn dev_retry_is_inert_outside_error_and_restart_wait_remains_account_only() {
    use crate::auth::{Phase, SessionCmd};
    use crate::auth::owner::ReplyTo;
    for phase in [Phase::Idle, Phase::Creating, Phase::Ready, Phase::Error] {
        let mut init = dev_fixture().session.snapshot_init();
        init.phase = phase;
        let mut rig = Bridge::for_session_test(init);
        rig.session_adapter.fixture_resources().disk.account_token = "synthetic-saved-a".into();
        let before = rig.session.subhash();
        let mut d = Dispatcher::<AppHost>::new();
        if phase != Phase::Error { execute_session_command(&mut d, SessionCmd::Retry); }
        execute_session_command(&mut d, SessionCmd::RestartWait { phase, qr_generation: 0,
            reply: ReplyTo { instance: 19, correlation: 1 } });
        execute_session_command(&mut d, SessionCmd::StartSwitch(crate::auth::Picker::ChangeProfile));
        execute_session_command(&mut d, SessionCmd::SelectProfile { index: 0, pin: None });
        execute_session_command(&mut d, SessionCmd::BackAtRoot { reply: ReplyTo { instance: 19, correlation: 2 } });
        execute_session_command(&mut d, SessionCmd::RefreshRoster);
        execute_session_command(&mut d, SessionCmd::RequestEndpoint { sid: crate::plex::ServerId::from_raw(0) });
        execute_session_command(&mut d, SessionCmd::TakeReady);
        frame(&mut rig, &mut d);
        assert_eq!(rig.session.subhash(), before);
        assert!(rig.session.snapshot_init().pending.is_empty());
        assert_eq!(rig.session_adapter.fixture_resources().disk.account_token, "synthetic-saved-a");
        assert!(rig.session_adapter.fixture_resources().registry_writes.is_empty());
    }
}
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        crate::plex::reset_servers_for_test();
        crate::plex::session::ProfilePublisher::new(self.0).publish(None, 0);
    }
}

#[test]
fn dev_native_activation_is_ephemeral_and_revoke_ack_precedes_clean_login_work() {
    use crate::auth::owner::{BootstrapAuthority, ReadyInstall, SessionWork};
    use crate::plex::session::{self, Session, ServerRef, SourceRef};
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    let _lock = crate::testlock::serial();
    let mt = unsafe { crate::task::MainThread::assume() };
    for saved_account in [false, true] {
        let tmp = session::TempSession::new("dev-bootstrap-owner");
        let _cleanup = Cleanup(&mt);
        tmp.assert_only_target();
        crate::plex::reset_servers_for_test();
        let saved = Session { client_id: "synthetic-device".into(),
            account_token: if saved_account { "synthetic-account-a".into() } else { String::new() },
            ..Default::default() };
        if saved_account { session::save(&saved); }
        else { std::fs::remove_file(tmp.path()).unwrap(); }
        let before = std::fs::read(tmp.path()).ok();
        let primary = ServerRef { address: "127.0.0.2".into(), port: 32400,
            origin_url: "http://127.0.0.2:32400".into(), token: "synthetic-pms-b".into(),
            tier: Some(crate::plex::probe::Location::Local), ..Default::default() };
        let extra = SourceRef { machine_id: "synthetic-extra".into(), name: "Synthetic extra".into(),
            address: "192.0.2.3".into(), port: 32400,
            origin_url: "https://192-0-2-3.synthetic.plex.direct:32400".into(),
            token: "synthetic-extra-token".into(), shared_by: "Synthetic owner".into(), owned: false,
            tier: Some(crate::plex::probe::Location::Remote), ..Default::default() };
        let init = super::super::boot::captured_session_for_boot(saved, Some(primary), vec![extra.clone()]);
        let mut rig = Bridge::for_session_test(init);
        rig.session_adapter = super::super::adapters::session::SessionAdapter::live_resources_for_test(&mt, false);
        let mut d = Dispatcher::<AppHost>::new();
        execute_session_command(&mut d, crate::auth::SessionCmd::ActivateDevBootstrap);
        assert_eq!(crate::plex::server_ids().count(), 0);
        assert!(rig.take_session_ready().is_none());
        frame(&mut rig, &mut d);
        let ready = rig.take_session_ready().unwrap();
        assert_eq!(ready.token, "synthetic-pms-b");
        assert!(matches!(ready.install, ReadyInstall::AlreadyInstalled));
        assert!(rig.take_session_ready().is_none());
        assert!(rig.auth_read().0.profile.is_none());
        assert_eq!(session::current_gen(), rig.auth_read().0.scope.0);
        let ids: Vec<_> = crate::plex::server_ids().collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(crate::plex::client_for(ids[0]).unwrap().origin().host(), "127.0.0.2");
        let shared = crate::plex::client_for(ids[1]).unwrap();
        assert_eq!(shared.machine_id(), extra.machine_id);
        assert_eq!(shared.origin().base(), extra.origin_url);
        assert_eq!(shared.resolve_pin(), extra.resolve_pin().as_ref());
        assert_eq!(shared.link(), extra.tier);
        let facts = crate::plex::server_facts(ids[1]).unwrap();
        assert_eq!(facts.name, extra.name);
        assert_eq!(facts.handle, extra.shared_by);
        assert_eq!(facts.owned, extra.owned);
        assert_eq!(std::fs::read(tmp.path()).ok(), before, "activation must neither save A/B nor mint a file");
        let scope = rig.auth_read().0.scope;
        execute_session_command(&mut d, crate::auth::SessionCmd::ActivateDevBootstrap);
        frame(&mut rig, &mut d);
        assert_eq!(rig.auth_read().0.scope.0, scope.0);
        assert!(rig.take_session_ready().is_none());

        let ran = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&ran);
        let req = rig.session.snapshot_init().next_req + 2;
        let epoch = rig.auth_read().0.flow_epoch + 1;
        rig.session_adapter.inject_fixture_work(req, move |output, input| {
            assert_eq!(crate::plex::server_ids().count(), 0, "revoke must execute BEFORE new account work");
            let SessionWork::Login { client_id } = input else { panic!("dev exit must start clean Login") };
            assert_eq!(client_id, "synthetic-device");
            signal.store(true, Ordering::Release);
            output.complete(crate::auth::LoginProgress::Failed { epoch, message: "synthetic stop".into() }.into()).unwrap();
        });
        for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST - 1 {
            execute_session_command(&mut d, crate::auth::SessionCmd::NoteDeleteLeftovers(0));
        }
        execute_session_command(&mut d, crate::auth::SessionCmd::StartLogin);
        frame(&mut rig, &mut d);
        assert!(!ran.load(Ordering::Acquire));
        assert!(rig.session.snapshot_init().pending_commit.is_some());
        assert_eq!(crate::plex::server_ids().count(), 2, "revoke effect is genuinely carried");
        frame(&mut rig, &mut d);
        assert!(ran.load(Ordering::Acquire));
        let state = rig.session.snapshot_init();
        assert!(matches!(&state.authority, BootstrapAuthority::Account { extras } if extras.is_empty()));
        assert!(state.persisted.account_token.is_empty());
        assert!(state.committed_credentials.account_token.is_empty());
        assert_eq!(std::fs::read(tmp.path()).ok(), before, "opening login is not erasure/persistence");
    }
}

#[test]
fn dev_boot_capture_cannot_keep_saved_account_as_worker_authority() {
    use crate::plex::session::{Session, ServerRef, UserRef};
    let saved = Session { client_id: "synthetic-client".into(), account_token: "synthetic-account-a".into(),
        user: UserRef { uuid: "synthetic-a".into(), token: "synthetic-profile-a".into(), ..Default::default() },
        ..Default::default() };
    let dev = ServerRef { address: "127.0.0.2".into(), port: 32400,
        origin_url: "http://127.0.0.2:32400".into(), token: "synthetic-pms-b".into(), ..Default::default() };
    // This is the same capture function actual boot uses before creating its sole Bridge.
    let init = super::super::boot::captured_session_for_boot(saved, Some(dev), Vec::new());
    let rig = Bridge::for_session_test(init);
    let state = rig.session.snapshot_init();
    assert!(state.persisted.account_token.is_empty(), "dev B must not inherit saved A's worker authority");
    assert!(state.persisted.user.token.is_empty());
    assert!(state.committed_credentials.account_token.is_empty(), "BACK must not recover A");
}

#[test]
fn clean_login_replacement_checks_disk_identity_and_keeps_best_effort_ack_contract() {
    use crate::plex::session::{self, Session, ServerRef};
    let _lock = crate::testlock::serial();
    let mt = unsafe { crate::task::MainThread::assume() };
    for disk_case in 0..3 {
        let tmp = session::TempSession::new("dev-login-replacement");
        let _cleanup = Cleanup(&mt);
        tmp.assert_only_target();
        crate::plex::reset_servers_for_test();
        let saved = Session { client_id: "synthetic-device".into(),
            account_token: "synthetic-account-a".into(), ..Default::default() };
        session::save(&saved);
        let dev = ServerRef { address: "127.0.0.2".into(), port: 32400,
            token: "synthetic-pms-b".into(), ..Default::default() };
        let mut rig = Bridge::for_session_test(super::super::boot::captured_session_for_boot(saved, Some(dev), Vec::new()));
        rig.session_adapter = super::super::adapters::session::SessionAdapter::live_resources_for_test(&mt, false);
        let mut d = Dispatcher::<AppHost>::new();
        // StartLogin also works before dev activation: it must still revoke before account work.
        rig.session_adapter.inject_fixture_work(2, |output, _| {
            output.progress(crate::auth::LoginProgress::Authorized { epoch: 2,
                token: "synthetic-new-account".into() }.into()).unwrap();
            output.complete(crate::auth::LoginProgress::SignedIn { epoch: 2,
                server: ServerRef { address: "127.0.0.3".into(), port: 32400,
                    token: "synthetic-new-pms".into(), ..Default::default() },
                sources: Vec::new(), users: Vec::new() }.into()).unwrap();
        });
        execute_session_command(&mut d, crate::auth::SessionCmd::StartLogin);
        frame(&mut rig, &mut d);
        let records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 2);
        if disk_case == 0 {
            assert!(session::set_auto_sign_in(true), "newer preference after login worker capture");
        }
        if disk_case == 1 {
            session::update(|disk| Some(Session { account_token: "synthetic-external-replacement".into(), ..disk.clone() }));
        } else if disk_case == 2 {
            // Existing best-effort policy: an unavailable session candidate cannot be updated.
            // Preserve the real original bytes in this same scratch scope, no IO mock/reducer.
            std::fs::rename(tmp.path(), tmp.path().with_file_name("preserved-a.json")).unwrap();
            std::fs::create_dir(tmp.path()).unwrap();
        }
        let results = records.into_iter().map(|r| (r.addr,
            AppMsg::Session(crate::auth::owner::SessionEvent::Result(r)))).collect();
        d.frame_with(&mut rig, Tick::default(), Vec::new(), results, &mut NoTap, false);
        let state = rig.session.snapshot_init();
        if disk_case == 1 {
            assert_eq!(session::peek().account_token, "synthetic-external-replacement");
            assert_eq!(state.disk_identity.account_token, "synthetic-account-a");
            assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Error);
        } else {
            assert_eq!(state.disk_identity.account_token, "synthetic-new-account");
            assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Ready);
            if disk_case == 0 {
                let disk = session::peek();
                assert_eq!(disk.account_token, "synthetic-new-account");
                assert!(disk.auto_sign_in(), "Dev-to-Account replacement preserves the newer preference");
            }
            else {
                assert!(tmp.path().is_dir(), "accepted reply did not claim a durable write");
                let old: Session = serde_json::from_slice(&std::fs::read(tmp.path().with_file_name("preserved-a.json")).unwrap()).unwrap();
                assert_eq!(old.account_token, "synthetic-account-a");
            }
        }
    }
}
