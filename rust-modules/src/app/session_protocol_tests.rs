//! Core-owned child module for the approved Session transfer/Pump production traces.
//! Tests use super's real Bridge and dispatcher boundaries, not a replacement Rig.

use super::*;

#[test]
fn selection_acceptance_uses_exact_instance_correlation_and_full_epoch_through_carry() {
    use crate::auth::owner::{Command, ReplyTo};
    use crate::ui::dispatch::Tap;
    #[derive(Default)]
    struct Replies(Vec<(u32, u32, bool, u64)>);
    impl Tap<AppHost> for Replies {
        fn effect(&mut self, _: u64, stamped: &crate::ui::machine::Stamped<AppHost>) {
            if let Fx::Deliver(MachineId::Instance(instance), Delivery::Screen(ScreenEvent::Async(req,
                AppMsg::SelectionReply { correlation, accepted, flow_epoch }))) = &stamped.fx {
                assert_eq!(req.0, *correlation);
                self.0.push((instance.0, *correlation, *accepted, *flow_epoch));
            }
        }
    }
    // Fast Ready, ordinary worker selection, invalid tile, request exhaustion, epoch exhaustion.
    for case in 0..5 {
        for carry in [false, true] {
            let epoch = if case == 4 { u64::MAX } else { u64::from(u32::MAX) + 23 };
            let mut init = crate::auth::SessionInit::captured(crate::plex::session::Session {
                client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
                user: crate::plex::session::UserRef { uuid: "synthetic-user".into(),
                    token: "synthetic-profile-token".into(), ..Default::default() }, ..Default::default()
            });
            init.epoch = epoch;
            init.phase = crate::auth::Phase::Profiles;
            init.users = vec![crate::auth::UserTile { uuid: "synthetic-user".into(),
                title: "Synthetic user".into(), protected: case == 1, ..Default::default() }];
            if case == 3 { init.next_req = u32::MAX; }
            let mut rig = Bridge::for_session_test(init);
            let old = rig.session.publication();
            let old_hash = rig.session.subhash();
            let mut d = Dispatcher::<AppHost>::new();
            let mut replies = Replies::default();
            if carry {
                for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST - 1 {
                    execute_session_command(&mut d, Command::DismissPinError);
                }
            }
            execute_session_command(&mut d, Command::SelectProfileWithReply {
                index: if case == 2 { 9 } else { 0 }, pin: (case == 1).then(|| "1234".into()),
                reply: ReplyTo { instance: 41, correlation: 17 },
            });
            let first = d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut replies, false);
            let accepted = case < 2;
            let actual_epoch = if accepted { epoch + 1 } else { epoch };
            assert_eq!(rig.auth_read().0.flow_epoch, actual_epoch);
            assert_eq!(old.flow_epoch, epoch, "retained old reads cannot acquire the new flow identity");
            if carry {
                assert!(first.carried > 0);
                assert!(replies.0.is_empty(), "command execution is not synchronous reply delivery");
                d.frame_with(&mut rig, Tick { ms: 16, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut replies, false);
            }
            assert_eq!(replies.0, [(41, 17, accepted, actual_epoch)]);
            assert_eq!(rig.auth_read().0.flow_epoch, actual_epoch, "selection executes once, not again at ACK");
            if case == 0 {
                assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Ready);
                assert_eq!(rig.session.snapshot_init().next_req, 0, "fast Ready needs no worker request identity");
            } else if case == 1 {
                assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Switching);
                assert_eq!(rig.session.snapshot_init().next_req, 1);
            } else {
                assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Profiles);
                assert_eq!(rig.session.subhash(), old_hash, "refusal does not mutate the existing flow");
                assert!(std::sync::Arc::ptr_eq(&old, &rig.session.publication()));
            }
        }
    }
}

#[test]
fn full_transfer_and_refilled_landing_use_production_ingest_and_carried_owner_acks() {
    use crate::auth::owner::{AdmissionId, AdmissionState, Command, Identity, Pending, Receipt,
        SessionEvent, SessionFx, SessionOp, SessionWorkKey, StreamPhase,
        SESSION_DATA_RECORDS, SESSION_TOTAL_RESERVATIONS, SESSION_TRANSFER_RECORDS};
    use crate::ui::machine::RequestId;
    // frame_ingest also captures the OTHER stores. Serialize that real frame boundary;
    // Session's own resources remain private and every network operation is injected.
    let _guard = crate::testlock::serial();
    let mut init = crate::auth::SessionInit::captured(crate::plex::session::Session {
        client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
        ..Default::default()
    });
    let key = SessionWorkKey { epoch: 1, op: SessionOp::ServerRoster };
    init.next_req = 2 * SESSION_TOTAL_RESERVATIONS;
    for req in 1..=init.next_req {
        init.pending.insert(req, Pending { key, expected: Identity::of(&init.persisted),
            lifecycle: None, last_arrival: None, phase: StreamPhase::Running, capture: None,
            admission: AdmissionState::Awaiting(AdmissionId(req)) });
    }
    let expected = crate::auth::SessionIdentity::of(&init.persisted);
    let mut rig = Bridge::for_session_test(init);
    let fill = |rig: &mut Bridge, first: u32| {
        for offset in 0..SESSION_TOTAL_RESERVATIONS {
            let expected = expected.clone();
            rig.session_adapter.launch(RequestId(first + offset), key, true,
                |job| { job(); true }, move |output| {
                    if offset == 0 {
                        for _ in 0..SESSION_DATA_RECORDS {
                            assert!(output.progress(crate::auth::AuthProgress::Registry(
                                crate::auth::RegistryProgress::Install { epoch: 1,
                                    expected: Some(expected.clone()), sources: Vec::new(), primary: None })).is_ok());
                        }
                    }
                    // One real completion-guard terminal per reservation.
                }).unwrap();
        }
    };
    fill(&mut rig, 1);
    let mut d = Dispatcher::<AppHost>::new();
    for _ in 0..200 { execute_session_command(&mut d, Command::DismissPinError); }
    let mut first = Vec::new();
    let (_, report) = frame_ingest(&mut d, &mut rig, Tick::default(), Vec::new(), |rig| {
        first = rig.session_adapter.take_results();
        assert_eq!(first.len(), SESSION_TRANSFER_RECORDS);
        first.iter().cloned().map(|record| (record.addr, AppMsg::Session(SessionEvent::Result(record)))).collect()
    }, &mut NoTap);
    assert!(report.carried > 0);
    assert!(rig.session.snapshot_init().pending_commit.is_some());
    assert!(!rig.session.snapshot_init().inbox.is_empty());
    assert!(first.iter().all(|record| rig.session_adapter.admitted(record)));
    fill(&mut rig, SESSION_TOTAL_RESERVATIONS + 1);
    assert!(rig.session_adapter.take_results().is_empty(), "96 transferred credits gate the 96 refilled records");
    // Cancel a transferred resource and duplicate the tail envelope. Its unique logical
    // terminal remains in the owner's FIFO; neither action may return its credit early.
    let tail = first.last().unwrap().clone();
    d.emit(MachineId::Session, Fx::App(AppFx::SessionEffect(SessionFx::Cancel {
        requests: vec![tail.addr.req.0], epoch: 1,
    })));
    d.emit(MachineId::Session, Fx::Deliver(MachineId::Session,
        Delivery::Machine(AppMsg::Session(SessionEvent::Result(tail.clone())))));
    let mut frame = 1;
    while first.iter().any(|record| rig.session_adapter.admitted(record)) {
        assert!(frame < 12, "bounded normal frames must drain the owned FIFO");
        frame_ingest(&mut d, &mut rig, Tick { ms: frame * 16, dt_us: 16_000 }, Vec::new(), |rig| {
            let blocked = rig.session_adapter.take_results();
            assert!(blocked.is_empty(), "no second transfer while any unique old credit remains");
            Vec::new()
        }, &mut NoTap);
        if let Some(acked) = first.iter().find(|record| !rig.session_adapter.admitted(record)) {
            let receipt = Receipt::of(acked);
            let mut mismatch = receipt;
            mismatch.addr.req = RequestId(u32::MAX);
            d.emit(MachineId::Session, Fx::App(AppFx::SessionEffect(SessionFx::Acknowledge(
                vec![receipt, receipt, mismatch]))));
        }
        frame += 1;
    }
    assert!(frame > 2, "the test must retain work across several real frames");
    let mut second = Vec::new();
    frame_ingest(&mut d, &mut rig, Tick { ms: frame * 16, dt_us: 16_000 }, Vec::new(), |rig| {
        second = rig.session_adapter.take_results();
        assert_eq!(second.len(), SESSION_TRANSFER_RECORDS);
        second.iter().cloned().map(|record| (record.addr, AppMsg::Session(SessionEvent::Result(record)))).collect()
    }, &mut NoTap);
    assert!(second[0].arrival > first.last().unwrap().arrival);
    assert!(second.windows(2).all(|pair| pair[0].arrival < pair[1].arrival));
    while second.iter().any(|record| rig.session_adapter.admitted(record)) {
        frame += 1;
        assert!(frame < 24);
        frame_ingest(&mut d, &mut rig, Tick { ms: frame * 16, dt_us: 16_000 }, Vec::new(), |_| Vec::new(), &mut NoTap);
    }
    let state = rig.session.snapshot_init();
    assert!(state.pending.is_empty());
    assert!(state.pending_commit.is_none());
    assert!(state.inbox.is_empty());
    assert!(!state.pump_pending);
    assert!(rig.session_adapter.take_results().is_empty());
}

#[test]
fn endpoint_owner_bridge_preserves_https_pin_and_rejects_native_replacements() {
    use crate::auth::owner::{RegistryPlan, SessionEvent, SessionWork};
    let _guard = crate::testlock::serial();
    for replacement in 0..3 {
        crate::plex::reset_servers_for_test();
        let initial_origin = crate::plex::Origin::http("127.0.0.1", 9);
        let sid = crate::plex::register_pinned_with_client_id("synthetic-server", &initial_origin,
            "synthetic-profile-token", None, "synthetic-client");
        let client = crate::plex::client_for(sid).unwrap();
        let instance = client.instance_gen();
        let token_gen = client.token_gen();
        let grant = crate::plex::session::SourceRef { machine_id: "synthetic-server".into(),
            name: "Synthetic server".into(), address: "127.0.0.1".into(), port: 9,
            origin_url: initial_origin.base(), token: "synthetic-profile-token".into(),
            owned: true, ..Default::default() };
        let stored = crate::plex::session::Session {
            client_id: "synthetic-client".into(), account_token: "synthetic-account-token".into(),
            user: crate::plex::session::UserRef { uuid: "synthetic-profile".into(),
                token: grant.token.clone(), ..Default::default() },
            server: crate::plex::session::ServerRef { machine_id: grant.machine_id.clone(),
                address: grant.address.clone(), port: grant.port, origin_url: grant.origin_url.clone(),
                token: grant.token.clone(), ..Default::default() },
            sources: vec![grant.clone()], ..Default::default()
        };
        let fresh = crate::plex::session::SourceRef {
            address: "192.0.2.20".into(), port: 32400,
            origin_url: "https://192-0-2-20.synthetic.plex.direct:32400".into(),
            token: "synthetic-account-grant-not-profile-token".into(),
            tier: Some(crate::plex::probe::Location::Local), ..grant.clone()
        };
        let expected_origin = fresh.origin().unwrap();
        let expected_pin = fresh.resolve_pin().unwrap();
        assert!(expected_origin.is_tls());
        let mut rig = Bridge::for_session_test(crate::auth::SessionInit::captured(stored));
        rig.session_adapter.fixture_resources().native_endpoints.insert(sid.raw(), client);
        rig.session_adapter.inject_fixture_work(1, move |output, input| {
            let SessionWork::Endpoint { expected, lifecycle, machine_id, .. } = input
                else { panic!("endpoint command launched another operation") };
            assert_eq!(lifecycle.sid, sid.raw());
            assert_eq!(lifecycle.instance_gen, instance);
            assert_eq!(lifecycle.token_gen, token_gen);
            assert_eq!(expected.profile_uuid, "synthetic-profile");
            assert!(output.complete(crate::auth::endpoint_work_fact(1, 1, expected, lifecycle,
                machine_id, Some(fresh))).is_ok());
        });
        let mut d = Dispatcher::<AppHost>::new();
        execute_session_command(&mut d, crate::auth::SessionCmd::RequestEndpoint { sid });
        d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
        let records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 1);
        assert!(rig.session_adapter.fixture_resources().registry_writes.is_empty());
        assert_eq!(rig.session_adapter.fixture_resources().disk.sources[0].origin_url, initial_origin.base());
        rig.session_adapter.fixture_resources().disk.playback_quality = Some(crate::plex::session::PlaybackQuality::Original);
        match replacement {
            1 => {
                client.set_token("synthetic-new-profile-token");
                assert!(std::ptr::eq(client, crate::plex::client_for(sid).unwrap()));
                assert_ne!(client.token_gen(), token_gen);
                assert_eq!(client.instance_gen(), instance);
            }
            2 => {
                let newer = crate::plex::Origin::http("127.0.0.1", 10);
                let replaced_sid = crate::plex::register_pinned_with_client_id("synthetic-server", &newer,
                    "synthetic-new-profile-token", None, "synthetic-client");
                assert_eq!(replaced_sid, sid);
                assert!(!std::ptr::eq(client, crate::plex::client_for(sid).unwrap()));
                assert_ne!(crate::plex::client_for(sid).unwrap().instance_gen(), instance);
            }
            _ => {}
        }
        let results = records.iter().cloned().map(|envelope| (envelope.addr,
            AppMsg::Session(SessionEvent::Result(envelope)))).collect();
        d.frame_with(&mut rig, Tick { ms: 16, dt_us: 16_000 }, Vec::new(), results, &mut NoTap, false);
        let resources = rig.session_adapter.fixture_resources();
        assert_eq!(resources.disk.playback_quality, Some(crate::plex::session::PlaybackQuality::Original));
        if replacement == 0 {
            let [RegistryPlan::Endpoint { source, .. }] = resources.registry_writes.as_slice()
                else { panic!("positive endpoint observation did not commit exactly one route") };
            assert_eq!(source.origin().unwrap(), expected_origin);
            assert_eq!(source.resolve_pin().as_ref(), Some(&expected_pin));
            assert_eq!(source.token, "synthetic-profile-token");
            assert_eq!(resources.disk.sources[0].origin_url, expected_origin.base());
            let installed = crate::plex::client_for(sid).unwrap();
            assert_eq!(installed.origin(), &expected_origin);
            assert_eq!(installed.resolve_pin(), Some(&expected_pin));
        } else {
            assert!(resources.registry_writes.is_empty());
            assert_eq!(resources.disk.sources[0].origin_url, initial_origin.base());
            assert_ne!(crate::plex::client_for(sid).unwrap().origin(), &expected_origin);
        }
        assert!(rig.session.snapshot_init().pending.is_empty());
        assert!(rig.session.snapshot_init().pending_commit.is_none());
        assert!(records.iter().all(|record| !rig.session_adapter.admitted(record)));
    }
    crate::plex::reset_servers_for_test();
}
