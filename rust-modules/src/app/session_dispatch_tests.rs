//! Session/browse dispatch mechanics: chrome tab generation ownership, per-bridge browse
//! isolation, and the auth owner's queued commit-reply/carry machinery.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::{frame, frame_with_results};

#[test]
fn browse_tab_generation_is_owned_and_chrome_never_replays_a_stale_shape() {
    let _guard = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("bridge-browse-tabs");
    session.watching("u-bridge-browse-tabs");
    crate::plex::reset_servers_for_test();
    let own = crate::plex::register_for_test(
        "bridge-tabs-own", "127.0.0.1", 9, "synthetic", "fixture");
    let shared = crate::plex::register_for_test(
        "bridge-tabs-shared", "127.0.0.1", 10, "synthetic", "fixture");
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.browse.borrow_mut().seed_registered_table_for_test([own, shared]);
    let mut pages = Dispatcher::<AppHost>::new();
    rig.capture_views(&mut pages);
    rig.capture_chrome(&mut pages);
    let before = rig.chrome.labels();
    assert!(before.labels.iter().any(|label| label == "TV Shows"));
    let before_gen = before.generation;

    rig.stores.browse.borrow_mut().run(
        crate::stores::browse::BrowseCmd::ApplyPins(vec![(1, false)]));
    rig.capture_views(&mut pages);
    rig.capture_chrome(&mut pages);
    let changed = rig.chrome.labels();
    assert!(changed.generation > before_gen);
    assert!(!changed.labels.iter().any(|label| label == "TV Shows"));
    let changed_gen = changed.generation;

    rig.capture_views(&mut pages);
    rig.capture_chrome(&mut pages);
    assert_eq!(rig.chrome.labels().generation, changed_gen,
        "capturing again must not republish the owner's old tab generation");
    assert!(!rig.chrome.labels().labels.iter().any(|label| label == "TV Shows"));
    crate::plex::reset_servers_for_test();
}

#[test]
fn production_bridges_do_not_share_browse_state_or_landings() {
    use std::io::{Read, Write};
    let _guard = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("bridge-browse-owners");
    session.watching("u-bridge-browse-owners");
    crate::plex::reset_servers_for_test();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback bind");
    let port = listener.local_addr().unwrap().port();
    let (accepted_tx, accepted_rx) = std::sync::mpsc::sync_channel(0);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("page request");
        let mut request = Vec::new();
        let mut chunk = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let n = socket.read(&mut chunk).expect("read request");
            assert!(n > 0, "request closed before headers");
            request.extend_from_slice(&chunk[..n]);
        }
        let request = String::from_utf8_lossy(&request);
        assert!(request.contains("GET /library/sections/1/all?"));
        assert!(request.contains("X-Plex-Container-Start=0"));
        assert!(request.contains("X-Plex-Container-Size=60"));
        accepted_tx.send(()).unwrap();
        release_rx.recv().expect("release held response");
        let body = br#"{"MediaContainer":{"totalSize":1,"Metadata":[{"ratingKey":"1","type":"movie","title":"first-owner-only"}]}}"#;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len());
        socket.write_all(head.as_bytes()).unwrap();
        socket.write_all(body).unwrap();
    });
    let own = crate::plex::register_for_test(
        "bridge-browse-own", "127.0.0.1", 9, "synthetic", "fixture");
    let shared = crate::plex::register_for_test(
        "bridge-browse-shared", "127.0.0.1", port as i32, "synthetic", "fixture");
    let mut first = Bridge::for_test(|| 0);
    let mut second = Bridge::for_test(|| 0);
    first.stores.browse.borrow_mut().seed_registered_table_for_test([own, shared]);
    first.stores.browse.borrow_mut().prepare_page_for_test(shared);
    let mut pages = Dispatcher::<AppHost>::new();
    frame(&mut pages, &mut first, AppArg::Library, tick(0), vec![]);
    let _ = first.stores.browse.borrow_mut().pump();
    accepted_rx.recv_timeout(std::time::Duration::from_secs(2))
        .expect("production page request reached loopback");
    let _ = first.stores.take_notices();
    let before = first.stores.gen(StoreId::Browse);
    second.capture_views(&mut pages);
    assert_eq!(second.directory.view().current(), None,
        "a command and its later worker landings belong only to the addressed Bridge store");
    release_tx.send(()).unwrap();
    server.join().unwrap();
    assert!(!second.stores.browse.borrow().has_page_result_for_test(),
        "the origin worker must not write the other Bridge's adapter");
    let mut page_landed = false;
    for _ in 0..100_000 {
        if first.stores.browse.borrow_mut().pump().changed {
            page_landed = true;
            break;
        }
        std::thread::yield_now();
    }
    assert!(page_landed, "page result did not land");
    assert_eq!(first.stores.gen(StoreId::Browse), before + 1,
        "one parsed page landing advances exactly one owned generation");
    assert_eq!(first.stores.browse.borrow_mut().listing_snapshot().view().item(0)
        .map(|item| item.title.as_str()), Some("first-owner-only"));
    assert!(second.stores.browse.borrow_mut().listing_snapshot().view().item(0).is_none());

    struct BrowseNotices(usize);
    impl crate::ui::dispatch::Tap<AppHost> for BrowseNotices {
        fn effect(&mut self, _: u64, stamped: &crate::ui::machine::Stamped<AppHost>) {
            if matches!(&stamped.fx, Fx::Deliver(_, Delivery::Screen(
                ScreenEvent::StoreChanged(ord, _))) if *ord == StoreId::Browse.ord()) {
                self.0 += 1;
            }
        }
    }
    let mut notices = BrowseNotices(0);
    frame_with_results(&mut pages, &mut first, AppArg::Library, tick(1), vec![],
        || Vec::new(), &mut notices);
    assert_eq!(notices.0, 1,
        "capture publication and notice drain must coalesce one page into one StoreChanged");
    assert!(first.stores.take_notices().is_empty());
    crate::plex::reset_servers_for_test();
}

#[test]
fn session_boot_picker_publishes_captured_profile_without_ready_handoff() {
    let stored = crate::plex::session::Session {
        client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
        server: crate::plex::session::ServerRef { machine_id: "synthetic-server".into(),
            address: "192.0.2.1".into(), port: 32400, token: "synthetic-server-token".into(),
            ..Default::default() },
        user: crate::plex::session::UserRef { uuid: "synthetic-user".into(), title: "A".into(),
            ..Default::default() },
        home_users: vec![crate::plex::session::HomeUserRef { uuid: "synthetic-user".into(),
            title: "A".into(), protected: true, ..Default::default() }], ..Default::default()
    };
    let mut rig = Bridge::for_session_test(crate::auth::SessionInit::captured(stored));
    let mut d = Dispatcher::<AppHost>::new();
    execute_session_command(&mut d, crate::auth::SessionCmd::StartSwitch(crate::auth::Picker::Boot));
    d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Profiles);
    let profile = rig.session_adapter.fixture_resources().profile.as_ref()
        .expect("boot profile publication must come from the concrete owner, not prior global seeding");
    assert_eq!(profile.profile.as_ref().unwrap().uuid, "synthetic-user");
    assert_eq!(profile.scope.0, 1);
    assert!(rig.take_session_ready().is_none(), "a protected boot picker has not seated a viewer");
    assert!(rig.session_adapter.fixture_resources().disk.home_users[0].protected);
}

#[test]
fn session_stored_boot_publishes_before_handoff_without_saving_credentials() {
    let stored = crate::plex::session::Session {
        client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
        server: crate::plex::session::ServerRef { machine_id: "synthetic-server".into(),
            address: "192.0.2.1".into(), port: 32400, token: "synthetic-server-token".into(),
            ..Default::default() },
        user: crate::plex::session::UserRef { uuid: "synthetic-user".into(), title: "A".into(),
            ..Default::default() }, ..Default::default()
    };
    assert!(stored.can_go_local());
    let mut rig = Bridge::for_session_test(crate::auth::SessionInit::captured(stored));
    // Stored boot installs the captured authority; it must not rewrite a more recent
    // file merely to publish the initial profile and restore its granted registry.
    rig.session_adapter.fixture_resources().disk.account_token = "synthetic-new-disk-token".into();
    let mut d = Dispatcher::<AppHost>::new();
    assert!(rig.take_session_ready().is_none());
    execute_session_command(&mut d, crate::auth::SessionCmd::ResumeStored);
    assert!(rig.session_adapter.fixture_resources().profile.is_none());
    d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    let publication = rig.session_adapter.fixture_resources().profile.as_ref().unwrap();
    assert_eq!(publication.profile.as_ref().unwrap().uuid, "synthetic-user");
    assert_eq!(publication.scope.0, 1);
    let ready = rig.take_session_ready().unwrap();
    assert_eq!(ready.token, "synthetic-server-token");
    assert!(rig.take_session_ready().is_none(), "the handoff is consumed exactly once");
    assert_eq!(rig.session_adapter.fixture_resources().disk.account_token, "synthetic-new-disk-token");
    execute_session_command(&mut d, crate::auth::SessionCmd::ResumeStored);
    d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    assert_eq!(rig.session.read().0.scope.0, 1, "repeated bootstrap cannot allocate a second profile scope");
    assert!(rig.take_session_ready().is_none());
}

#[test]
fn session_current_negative_commit_reply_unblocks_independent_carried_work() {
    use crate::auth::owner::{AdmissionId, AdmissionState, Command, Identity, Pending,
        SessionEvent, SessionOp, SessionWorkKey, StreamPhase};
    use crate::auth::{AuthProgress, LoginProgress, RegistryProgress};
    use crate::ui::machine::RequestId;
    let mut init = crate::auth::SessionInit::captured(crate::plex::session::Session {
        client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
        ..Default::default()
    });
    init.phase = crate::auth::Phase::Discovering;
    init.next_req = 2;
    let a_key = SessionWorkKey { epoch: 1, op: SessionOp::Login };
    let b_key = SessionWorkKey { epoch: 1, op: SessionOp::ServerRoster };
    for (req, key) in [(1, a_key), (2, b_key)] {
        init.pending.insert(req, Pending { key, expected: Identity::of(&init.persisted),
            lifecycle: None, last_arrival: None, phase: StreamPhase::Running, capture: None,
            admission: AdmissionState::Awaiting(AdmissionId(req)) });
    }
    let expected = crate::auth::SessionIdentity::of(&init.persisted);
    let mut rig = Bridge::for_session_test(init);
    rig.session_adapter.launch(RequestId(1), a_key, true, |job| { job(); true }, |output| {
        assert!(output.complete(LoginProgress::SignedIn { epoch: 1,
            server: crate::plex::session::ServerRef { machine_id: "synthetic-server".into(),
                address: "192.0.2.1".into(), port: 32400, token: "synthetic-token".into(),
                ..Default::default() }, sources: Vec::new(), users: Vec::new() }.into()).is_ok());
    }).unwrap();
    rig.session_adapter.launch(RequestId(2), b_key, true, |job| { job(); true }, move |output| {
        assert!(output.progress(AuthProgress::Registry(RegistryProgress::Install {
            epoch: 1, expected: Some(expected), sources: Vec::new(), primary: None,
        })).is_ok());
        // Actual completion guard supplies the terminal; no test-only retirement path.
    }).unwrap();
    let records = rig.session_adapter.take_results();
    assert_eq!(records.len(), 3);
    let results = records.iter().cloned().map(|envelope| (envelope.addr,
        AppMsg::Session(SessionEvent::Result(envelope)))).collect();
    // The owner permit stays current, but latest disk identity refuses A's credential
    // patch. B has a separate accepted request and must not be discarded with A.
    rig.session_adapter.fixture_resources().disk.client_id = "synthetic-new-disk-identity".into();
    let mut d = Dispatcher::<AppHost>::new();
    for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST - 4 {
        execute_session_command(&mut d, Command::DismissPinError);
    }
    let first = d.frame_with(&mut rig, Tick::default(), Vec::new(), results, &mut NoTap, false);
    assert!(first.carried > 0, "A's negative reply must actually cross the frame boundary");
    assert!(rig.session.commit_is_current(1, 1, records[0].arrival));
    assert_eq!(rig.session.snapshot_init().inbox.len(), 2);
    assert_eq!(rig.session.read().0.phase, crate::auth::Phase::Discovering);
    assert!(rig.session_adapter.fixture_resources().registry_writes.is_empty());
    assert!(records.iter().all(|record| rig.session_adapter.admitted(record)));
    d.frame_with(&mut rig, Tick { ms: 16, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
    assert_eq!(rig.session_adapter.fixture_resources().registry_writes.len(), 1,
        "B's independent commit must execute after A's queued negative reply");
    assert_eq!(rig.session_adapter.fixture_resources().disk.client_id, "synthetic-new-disk-identity");
    let state = rig.session.snapshot_init();
    assert!(state.pending.is_empty());
    assert!(state.pending_commit.is_none());
    assert!(state.inbox.is_empty());
    assert!(!state.pump_pending);
    assert!(records.iter().all(|record| !rig.session_adapter.admitted(record)));
}

#[test]
fn session_cancel_preserves_carried_receipts_until_unique_discard() {
    use crate::auth::owner::{AdmissionId, AdmissionState, Command, Identity, Pending, Receipt,
        SessionEvent, SessionFx, SessionOp, SessionWorkKey, StreamPhase};
    use crate::auth::{AuthProgress, LoginProgress, RegistryProgress};
    use crate::ui::machine::RequestId;
    let mut init = crate::auth::SessionInit::captured(crate::plex::session::Session {
        client_id: "synthetic-client".into(), ..Default::default()
    });
    let key = SessionWorkKey { epoch: 1, op: SessionOp::Login };
    init.next_req = 1;
    init.pending.insert(1, Pending { key, expected: Identity::of(&init.persisted),
        lifecycle: None, last_arrival: None, phase: StreamPhase::Running, capture: None,
        admission: AdmissionState::Awaiting(AdmissionId(1)) });
    let mut rig = Bridge::for_session_test(init);
    rig.session_adapter.launch(RequestId(1), key, true, |job| { job(); true }, |output| {
        assert!(output.progress(AuthProgress::Registry(RegistryProgress::Install {
            epoch: 1, expected: None, sources: Vec::new(), primary: None,
        })).is_ok());
        assert!(output.complete(LoginProgress::Failed { epoch: 1, message: "synthetic".into() }.into()).is_ok());
    }).unwrap();
    let records = rig.session_adapter.take_results();
    assert_eq!(records.len(), 2);
    let a = records[0].clone();
    let b = records[1].clone();
    let old_ack = Receipt::of(&a);
    let mut d = Dispatcher::<AppHost>::new();
    for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST - 2 {
        execute_session_command(&mut d, Command::DismissPinError);
    }
    d.emit(MachineId::Session, Fx::Deliver(MachineId::Session,
        Delivery::Machine(AppMsg::Session(SessionEvent::Result(a.clone())))));
    execute_session_command(&mut d, Command::EraseLocal);
    d.emit(MachineId::Session, Fx::Deliver(MachineId::Session,
        Delivery::Machine(AppMsg::Session(SessionEvent::Result(b.clone())))));
    let first = d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    assert!(first.carried > 0);
    assert!(rig.session.snapshot_init().pending_erase.is_some());
    assert_ne!(rig.session.read().0.phase, crate::auth::Phase::Deleted);
    assert!(rig.session.snapshot_init().pending_commit.is_none());
    assert!(rig.session_adapter.admitted(&b), "cancel cannot return a carried record's credit");

    let next_key = SessionWorkKey { epoch: 2, op: SessionOp::Login };
    rig.session_adapter.launch(RequestId(2), next_key, true, |job| { job(); true }, |output| {
        assert!(output.complete(LoginProgress::Failed { epoch: 2, message: "synthetic-new".into() }.into()).is_ok());
    }).unwrap();
    // Return A's unique credit twice while B is still carried. Neither can release B.
    rig.session_adapter.acknowledge(&[old_ack, old_ack]);
    assert!(rig.session_adapter.take_results().is_empty());
    assert!(rig.session_adapter.admitted(&b));
    for record in [a, b.clone()] {
        d.emit(MachineId::Session, Fx::Deliver(MachineId::Session,
            Delivery::Machine(AppMsg::Session(SessionEvent::Result(record)))));
    }
    d.emit(MachineId::Session, Fx::App(AppFx::SessionEffect(SessionFx::Acknowledge(vec![old_ack]))));
    d.frame_with(&mut rig, Tick { ms: 16, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
    assert!(!rig.session_adapter.admitted(&b));
    let next = rig.session_adapter.take_results();
    assert_eq!(next.len(), 1, "the next batch opens only after B's unique discard ACK");
    rig.session_adapter.acknowledge(&[old_ack]);
    assert!(rig.session_adapter.admitted(&next[0]), "late ACK cannot free a newer batch");
    assert!(rig.session_adapter.fixture_resources().disk.account_token.is_empty());
    assert!(rig.session_adapter.fixture_resources().registry_writes.iter()
        .all(|write| matches!(write, crate::auth::owner::RegistryPlan::Revoke)));
    assert!(rig.session.snapshot_init().pending_commit.is_none());
}

#[test]
fn two_session_bridges_dispatch_without_global_capture_or_a_serial_lock() {
    use crate::auth::{Phase, SessionCmd, SessionInit};
    let init = || SessionInit::captured(crate::plex::session::Session {
        client_id: "synthetic-client".into(), ..Default::default()
    });
    let mut a = Bridge::for_session_test(init());
    let mut b_init = init();
    b_init.phase = Phase::Waiting;
    b_init.pin_code = "BBBB".into();
    b_init.qr_png = vec![2, 3, 4];
    b_init.qr_gen = 7;
    b_init.next_qr = 7;
    let mut b = Bridge::for_session_test(b_init);
    let retained_b = b.session.publication();
    let before_b = b.session.subhash();
    let mut da = Dispatcher::<AppHost>::new();
    let mut db = Dispatcher::<AppHost>::new();
    execute_session_command(&mut da, SessionCmd::StartLogin);
    da.frame_with(&mut a, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    assert_eq!(a.session.read().0.phase, Phase::Creating);
    assert_eq!(b.session.subhash(), before_b);
    assert!(b.session_adapter.take_results().is_empty());
    let results: AppResults = a.session_adapter.take_results().into_iter().map(|envelope|
        (envelope.addr, AppMsg::Session(crate::auth::owner::SessionEvent::Result(envelope)))).collect();
    assert_eq!(results.len(), 1, "fixture spawn refusal must use the production Landing terminal");
    da.frame_with(&mut a, Tick { ms: 16, dt_us: 16_000 }, Vec::new(), results, &mut NoTap, false);
    assert_eq!(a.session.read().0.phase, Phase::Error);
    assert_eq!(b.session.subhash(), before_b);
    assert!(std::sync::Arc::ptr_eq(&retained_b, &b.session.publication()));
    execute_session_command(&mut db, SessionCmd::NoteDeleteLeftovers(3));
    db.frame_with(&mut b, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    assert_eq!(a.session.read().0.delete_leftovers, 0);
    assert_eq!(b.session.read().0.delete_leftovers, 3);
    assert_eq!(retained_b.read().0.phase, Phase::Waiting);
    assert_eq!(&*retained_b.read().0.code, "BBBB");
    assert_eq!(&*retained_b.read().0.png, &[2, 3, 4]);
    assert_eq!(retained_b.read().0.qr_generation, 7);
    assert!(b.session_adapter.fixture_resources().registry_writes.is_empty());
    assert!(a.session_adapter.fixture_resources().registry_writes.is_empty());
    execute_session_command(&mut da, SessionCmd::EraseLocal);
    da.frame_with(&mut a, Tick { ms: 32, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
    assert_eq!(a.session.read().0.phase, Phase::Deleted);
    assert_eq!(a.session.read().0.scope.0, 1);
    let published = a.session_adapter.fixture_resources().profile.as_ref().unwrap();
    assert_eq!(published.scope.0, 1, "the adapter publishes the owner's explicit generation");
    assert!(published.profile.is_none());
    assert!(a.session_adapter.fixture_resources().disk.account_token.is_empty());
    assert_eq!(b.session.read().0.scope.0, 0);
    assert_eq!(b.session.read().0.phase, Phase::Waiting);
    assert!(b.session_adapter.fixture_resources().profile.is_none());
}

#[test]
fn session_registry_then_terminal_waits_for_queued_commit_replies() {
    use crate::auth::owner::{AdmissionId, AdmissionState, Identity, Pending, SessionOp,
        SessionWorkKey, StreamPhase};
    use crate::auth::{AuthProgress, LoginProgress, RegistryProgress};
    use crate::ui::machine::RequestId;
    for (success, carry) in [(true, false), (false, false), (true, true), (false, true)] {
        let mut init = crate::auth::SessionInit::captured(crate::plex::session::Session {
            client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
            ..Default::default()
        });
        init.phase = crate::auth::Phase::Discovering;
        init.next_req = 1;
        let key = SessionWorkKey { epoch: init.epoch, op: SessionOp::Login };
        init.pending.insert(1, Pending { key, expected: Identity::of(&init.persisted),
            lifecycle: None, last_arrival: None, phase: StreamPhase::Running, capture: None,
            admission: AdmissionState::Awaiting(AdmissionId(1)) });
        let mut rig = Bridge::for_session_test(init);
        rig.session_adapter.launch(RequestId(1), key, true, |job| { job(); true }, move |output| {
            for _ in 0..2 {
                assert!(output.progress(AuthProgress::Registry(RegistryProgress::Install {
                    epoch: key.epoch, expected: None, sources: Vec::new(), primary: None,
                })).is_ok());
            }
            let terminal = if success {
                LoginProgress::SignedIn { epoch: key.epoch, server: crate::plex::session::ServerRef {
                    machine_id: "synthetic-server".into(), address: "192.0.2.1".into(),
                    port: 32400, token: "synthetic-server-token".into(), ..Default::default()
                }, sources: Vec::new(), users: Vec::new() }
            } else {
                LoginProgress::Failed { epoch: key.epoch, message: "synthetic failure".into() }
            };
            assert!(output.complete(AuthProgress::Login(terminal)).is_ok());
        }).unwrap();
        let records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 3);
        assert!(rig.session_adapter.fixture_resources().registry_writes.is_empty());
        let results = records.into_iter().map(|envelope| (envelope.addr,
            AppMsg::Session(crate::auth::owner::SessionEvent::Result(envelope)))).collect();
        let mut dispatcher = Dispatcher::<AppHost>::new();
        if carry {
            // Consume the real pre+post budgets so A's commit effect, the terminal, and
            // B retained behind A must survive into the next frame. No test-only drain.
            for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST - 2 {
                dispatcher.emit(MachineId::Nav, Fx::Deliver(MachineId::Session,
                    Delivery::Machine(AppMsg::Session(crate::auth::owner::SessionEvent::Command(
                        crate::auth::owner::Command::DismissPinError)))));
            }
        }
        let report = dispatcher.frame_with(&mut rig, Tick::default(), Vec::new(), results, &mut NoTap, false);
        if carry {
            assert!(report.carried > 0, "the regression must actually exercise cross-frame carry");
            let retained = rig.session.snapshot_init();
            assert!(retained.pending_commit.is_some());
            assert_eq!(retained.inbox.len(), 1);
            assert!(rig.session_adapter.fixture_resources().registry_writes.is_empty());
            dispatcher.frame_with(&mut rig, Tick { ms: 16, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
        }
        assert_eq!(rig.session_adapter.fixture_resources().registry_writes.len(), 2,
            "both commit-bearing progress observations must precede the terminal");
        assert_eq!(rig.session.read().0.phase,
            if success { crate::auth::Phase::Ready } else { crate::auth::Phase::Error });
        let state = rig.session.snapshot_init();
        assert!(state.pending_commit.is_none());
        assert!(state.pending.is_empty());
        assert!(state.inbox.is_empty());
        assert!(!state.pump_pending);
    }
}

#[test]
fn session_replies_cross_the_production_queued_drain_with_exact_correlation() {
    use crate::auth::owner::{Command, ReplyTo, SessionEvent};
    use crate::ui::dispatch::Tap;
    struct Replies(Vec<(u32, u32, bool)>);
    impl Tap<AppHost> for Replies {
        fn effect(&mut self, _: u64, stamped: &crate::ui::machine::Stamped<AppHost>) {
            if let Fx::Deliver(MachineId::Instance(instance),
                Delivery::Screen(ScreenEvent::Async(req, message))) = &stamped.fx {
                match message {
                    AppMsg::RestartReply { correlation, accepted } => {
                        assert_eq!(req.0, *correlation);
                        self.0.push((instance.0, *correlation, *accepted));
                    }
                    AppMsg::BackReply { correlation, resumed } => {
                        assert_eq!(req.0, *correlation);
                        self.0.push((instance.0, *correlation, *resumed));
                    }
                    _ => {}
                }
            }
        }
    }
    // This existing constructor captures other global store publications. This test is
    // dispatch evidence, not the still-owed lock-free two-Bridge fixture proof.
    let _guard = crate::testlock::serial();
    let mut rig = Bridge::for_test(|| 0);
    let mut dispatcher = Dispatcher::<AppHost>::new();
    let mut replies = Replies(Vec::new());
    for command in [
        Command::RestartWait { phase: crate::auth::Phase::Waiting, qr_generation: 9,
            reply: ReplyTo { instance: 41, correlation: 7 } },
        Command::BackAtRoot { reply: ReplyTo { instance: 42, correlation: 8 } },
    ] {
        dispatcher.emit(MachineId::Nav, Fx::Deliver(MachineId::Session,
            Delivery::Machine(AppMsg::Session(SessionEvent::Command(command)))));
    }
    assert!(replies.0.is_empty());
    dispatcher.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut replies, false);
    assert_eq!(replies.0, [(41, 7, false), (42, 8, false)]);
    assert_eq!(rig.session_adapter.fixture_resources().back_results, [false]);
    assert_eq!(rig.session.read().0.phase, crate::auth::Phase::Idle);
}

#[test]
fn session_frame_read_borrows_the_bridge_publication() {
    use crate::screens::registry::AuthLike;
    let _guard = crate::testlock::serial();
    let mut rig = Bridge::for_test(|| 0);
    let retained = rig.session.publication();
    let parts = CxParts { tick: Tick::default(), press: Default::default(),
        focus: Default::default(), owner: InputOwner::Entry(EntryId(0)) };
    let split = rig.split();
    let cx = parts.cx::<AppHost>(split.views, split.measure);
    assert!(std::ptr::eq(AppHost::auth(&cx).0, &*retained));
}
#[test]
fn endpoint_outcomes_cross_central_dispatch_machine_bridge_and_boot() {
    let _g = crate::testlock::serial();
    let _session = crate::plex::session::TempSession::new("endpoint-edges");
    crate::plex::reset_servers_for_test();
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
    let a = crate::plex::register_for_test("endpoint-a", "127.0.0.1", 9, "synthetic", "cid");
    let b = crate::plex::register_for_test("endpoint-b", "127.0.0.1", 10, "synthetic", "cid");
    crate::plex::describe_server(a, "Synthetic", "Synthetic share", false);
    let expected = [b, a]; // Home's own-first observation order, deliberately not slot order.
    crate::pms::with_refused_fetches_for_test(|| {
        for cmd in [crate::stores::hubs::HubsCmd::RefetchHubs, crate::stores::hubs::HubsCmd::Retry] {
            let _ = crate::stores::take_notices();
            let generation = crate::stores::gen(StoreId::Hubs);
            let outcome = crate::stores::apply(StoreCmd::Hubs(cmd));
            assert!(outcome.changed);
            assert_eq!(outcome.endpoints.iter().map(|r| r.sid).collect::<Vec<_>>(), expected);
            assert_eq!(crate::stores::take_notices(), [(StoreId::Hubs, generation + 1)]);
        }
        let mut rig = Bridge::for_test(|| 0);
        let parts = CxParts { tick: Tick::default(), press: Default::default(),
            focus: Default::default(), owner: InputOwner::Entry(EntryId(0)) };
        let mut present = crate::ui::present::Present::default();
        let mut out = Vec::new();
        let mut fx = Effects::new(&mut out, MachineId::Store(StoreId::Hubs.ord()), &mut present);
        rig.deliver(MachineId::Store(StoreId::Hubs.ord()),
            &AppMsg::Store(StoreCmd::Hubs(crate::stores::hubs::HubsCmd::Retry)), &parts, &mut fx);
        drop(fx);
        assert_eq!(out.len(), 2, "one command per observed source");
        let mut executed = Vec::new();
        let mut next = Vec::new();
        let mut fx = Effects::new(&mut next, MachineId::Session, &mut present);
        for stamped in out {
            let Fx::App(command @ AppFx::Session(_)) = stamped.fx
                else { panic!("recovery was not translated into a Session command") };
            rig.app_fx(stamped.from, command, &parts, &mut fx);
        }
        drop(fx);
        for stamped in next {
            let Fx::Deliver(MachineId::Session, Delivery::Machine(AppMsg::Session(
                crate::auth::owner::SessionEvent::Command(crate::auth::SessionCmd::RequestEndpoint { sid })))) = stamped.fx
                else { panic!("endpoint effect did not enter the owner's queued delivery path") };
            executed.push(sid);
        }
        assert_eq!(executed, expected);
        let stores = crate::stores::Stores::default();
        stores.viewstate.borrow_mut().owe_hubs_refresh_for_test();
        let split = rig.split();
        let cx = parts.cx::<AppHost>(split.views, split.measure);
        let mut out = Vec::new();
        let mut fx: Effects<'_, AppHost> = Effects::new(
            &mut out, MachineId::Store(StoreId::ViewState.ord()), &mut present);
        stores.viewstate.borrow_mut().pump(&mut |_| false, &mut |cmd| {
            crate::stores::hubs::apply(cmd)
        }).emit(&mut fx);
        drop(fx);
        drop(cx);
        assert_eq!(out.len(), 2);
        let actual: Vec<_> = out.into_iter().map(|e| match e.fx {
            Fx::App(AppFx::Session(crate::auth::SessionCmd::RequestEndpoint { sid })) => sid,
            _ => panic!("ViewState pump discarded its recovery outcome"),
        }).collect();
        assert_eq!(actual, expected);
        crate::pms::queue_test_landing(None);
        let result = crate::stores::hubs::take_results().pop().unwrap();
        let mut out = Vec::new();
        let mut fx = Effects::new(&mut out, MachineId::Store(StoreId::Hubs.ord()), &mut present);
        rig.deliver(MachineId::Store(StoreId::Hubs.ord()), &AppMsg::HubsResult(result), &parts, &mut fx);
        drop(fx);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0].fx, Fx::App(AppFx::Session(
            crate::auth::SessionCmd::RequestEndpoint { sid })) if sid == b));
        crate::browse::with_refused_discovery_for_test(|| {
            let client = crate::plex::client_for(a).unwrap();
            rig.stores.browse.borrow_mut().queue_discovery_for_test(
                client, client.token_gen(), false);
            let mut out = Vec::new();
            let mut fx = Effects::new(&mut out, MachineId::Store(StoreId::Browse.ord()), &mut present);
            rig.deliver(MachineId::Store(StoreId::Browse.ord()),
                &AppMsg::StoreWork(crate::stores::StoreWork::BrowseDiscovery), &parts, &mut fx);
            drop(fx);
            assert_eq!(out.len(), 1);
            assert!(matches!(out[0].fx, Fx::App(AppFx::Session(
                crate::auth::SessionCmd::RequestEndpoint { sid })) if sid == a));
            let endpoints = crate::app::boot::activate_server_owned(&mut rig);
            let mut boot_executed = Vec::new();
            execute_endpoint_outcomes_with(endpoints, |command| {
                let crate::auth::SessionCmd::RequestEndpoint { sid } = command
                    else { panic!("boot emitted a non-endpoint command") };
                boot_executed.push(sid);
            });
            assert_eq!(boot_executed, expected);
        });
    });
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
    crate::plex::reset_servers_for_test();
}
