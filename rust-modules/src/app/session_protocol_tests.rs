//! Core-owned child module for the approved Session transfer/Pump production traces.
//! Tests use super's real Bridge and dispatcher boundaries, not a replacement Rig.

use super::*;

#[cfg(test)]
mod carry_matrix {
    use super::*;
    use crate::auth::{AuthProgress, LoginProgress, RegistryProgress, SessionCmd};
    use crate::auth::owner::{AdmissionId, AdmissionState, CommitReply, Identity, Pending, Receipt,
        RegistryPlan, SessionEnvelope, SessionEvent, SessionFx, SessionOp, SessionWorkKey, StreamPhase};
    use crate::plex::session::{Session, SourceRef, UserRef, ServerRef};
    use crate::ui::machine::{RequestId, Stamped};

    const EPOCH: u64 = u32::MAX as u64 + 191;
    const BUDGET: usize = crate::ui::dispatch::MAX_STEPS_PRE as usize + crate::ui::dispatch::MAX_STEPS_POST as usize;

    #[derive(Default)]
    struct Trace {
        acks: Vec<Receipt>,
        order: Vec<(&'static str, u32)>,
        publications: usize,
        ready: usize,
    }
    impl crate::ui::dispatch::Tap<AppHost> for Trace {
        fn effect(&mut self, _: u64, s: &Stamped<AppHost>) {
            match &s.fx {
                Fx::App(AppFx::SessionEffect(SessionFx::Acknowledge(receipts))) => self.acks.extend(receipts),
                Fx::App(AppFx::SessionEffect(SessionFx::Pump)) => self.order.push(("pump-app", 0)),
                Fx::Deliver(_, Delivery::Machine(AppMsg::Session(SessionEvent::Pump))) => self.order.push(("pump-event", 0)),
                Fx::Deliver(_, Delivery::Machine(AppMsg::Session(SessionEvent::Commit(reply)))) => self.order.push(("commit-ack", reply.req)),
                Fx::Deliver(_, Delivery::Machine(AppMsg::Session(SessionEvent::Command(SessionCmd::StartSwitch(_))))) => self.order.push(("switch", 0)),
                Fx::App(AppFx::SessionEffect(SessionFx::PublishProfile(_))) => {
                    self.publications += 1;
                    self.order.push(("profile", 0));
                }
                Fx::App(AppFx::SessionEffect(SessionFx::Ready { .. })) => {
                    self.ready += 1;
                    self.order.push(("ready", 0));
                }
                _ => {}
            }
        }
    }
    fn stored() -> Session {
        let source = SourceRef { machine_id: "primary".into(), address: "127.0.0.1".into(),
            origin_url: "http://127.0.0.1:32400".into(), port: 32400, token: "profile-a".into(),
            owned: true, ..Default::default() };
        Session { client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
            user: UserRef { uuid: "profile-a".into(), token: "profile-a".into(), ..Default::default() },
            server: ServerRef { machine_id: source.machine_id.clone(), address: source.address.clone(),
                origin_url: source.origin_url.clone(), port: source.port, token: source.token.clone(), ..Default::default() },
            sources: vec![source], home_users: ["profile-a", "profile-b"].into_iter().map(|uuid|
                crate::plex::session::HomeUserRef { uuid: uuid.into(), title: uuid.into(), ..Default::default() }
            ).collect(), ..Default::default() }
    }
    fn rig(ops: &[(u32, SessionOp)]) -> Bridge {
        let mut init = crate::auth::SessionInit::captured(stored());
        init.epoch = EPOCH;
        init.phase = if ops[0].1 == SessionOp::ProfileSwitch { crate::auth::Phase::Switching }
            else { crate::auth::Phase::Discovering };
        init.authorized_in_flow = true;
        for &(req, op) in ops {
            init.next_req = init.next_req.max(req);
            init.pending.insert(req, Pending { key: SessionWorkKey { epoch: EPOCH, op },
                expected: Identity::of(&init.persisted), lifecycle: None, last_arrival: None,
                phase: StreamPhase::Running, capture: None, admission: AdmissionState::Awaiting(AdmissionId(req)) });
        }
        Bridge::for_session_test(init)
    }
    fn pad(d: &mut Dispatcher<AppHost>, count: usize) {
        for _ in 0..count { execute_session_command(d, SessionCmd::DismissPinError); }
    }
    fn queue(d: &mut Dispatcher<AppHost>, event: SessionEvent) {
        d.emit(MachineId::Session, Fx::Deliver(MachineId::Session, Delivery::Machine(AppMsg::Session(event))));
    }
    fn frame(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, records: Vec<SessionEnvelope>, trace: &mut Trace)
        -> crate::ui::dispatch::FrameReport {
        let results = records.into_iter().map(|r| (r.addr, AppMsg::Session(SessionEvent::Result(r)))).collect();
        d.frame_with(rig, Tick::default(), Vec::new(), results, trace, false)
    }
    fn registry_stream(rig: &mut Bridge) -> Vec<SessionEnvelope> {
        let key = rig.session.snapshot_init().pending[&1].key;
        rig.session_adapter.launch(RequestId(1), key, true, |job| { job(); true }, move |output| {
            for _ in 0..2 {
                output.progress(AuthProgress::Registry(RegistryProgress::Install {
                    epoch: key.epoch, expected: None, sources: Vec::new(), primary: None,
                })).unwrap();
            }
            output.complete(LoginProgress::Failed { epoch: key.epoch, message: "old flow".into() }.into()).unwrap();
        }).unwrap();
        let records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 3);
        records
    }

    #[test]
    fn active_inbox_and_carried_records_keep_unique_credits_through_cancel_and_duplicates() {
        let mut rig = rig(&[(1, SessionOp::Login)]);
        let records = registry_stream(&mut rig);
        let (a, b, c) = (records[0].clone(), records[1].clone(), records[2].clone());
        let mut d = Dispatcher::<AppHost>::new();
        let mut trace = Trace::default();
        pad(&mut d, BUDGET - 4);
        for r in [&a, &b, &a, &b] { queue(&mut d, SessionEvent::Result(r.clone())); }
        execute_session_command(&mut d, SessionCmd::EraseLocal);
        for _ in 0..2 {
            d.emit(MachineId::Session, Fx::App(AppFx::SessionEffect(SessionFx::Acknowledge(vec![Receipt::of(&a)]))));
        }
        pad(&mut d, BUDGET - 3);
        queue(&mut d, SessionEvent::Result(c.clone()));
        assert!(frame(&mut rig, &mut d, Vec::new(), &mut trace).carried > 0);
        let state = rig.session.snapshot_init();
        assert!(state.pending_commit.as_ref().unwrap().receipt == Some(Receipt::of(&a)));
        assert_eq!(state.inbox.len(), 1);
        assert!(Receipt::of(&state.inbox[0]) == Receipt::of(&b));
        assert!(trace.acks.is_empty(), "duplicate active/inbox envelopes must not ACK their originals");
        assert!(records.iter().all(|r| rig.session_adapter.admitted(r)));
        let next_key = SessionWorkKey { epoch: EPOCH + 1, op: SessionOp::Login };
        rig.session_adapter.launch(RequestId(2), next_key, true, |job| { job(); true }, move |output| {
            output.complete(LoginProgress::Failed { epoch: next_key.epoch, message: "next batch".into() }.into()).unwrap();
        }).unwrap();
        assert!(rig.session_adapter.take_results().is_empty());
        assert!(frame(&mut rig, &mut d, Vec::new(), &mut trace).carried > 0);
        let state = rig.session.snapshot_init();
        assert!(state.pending_erase.is_some() && state.pending_commit.is_none() && state.inbox.is_empty());
        assert!(!rig.session_adapter.admitted(&a));
        assert!(rig.session_adapter.admitted(&b) && rig.session_adapter.admitted(&c));
        assert!(rig.session_adapter.take_results().is_empty(), "duplicate old ACK cannot release B/C credits");
        frame(&mut rig, &mut d, Vec::new(), &mut trace);
        assert!(records.iter().all(|r| !rig.session_adapter.admitted(r)));
        for r in [&b, &c] { assert_eq!(trace.acks.iter().filter(|ack| **ack == Receipt::of(r)).count(), 1); }
        let next = rig.session_adapter.take_results();
        assert_eq!(next.len(), 1);
        let before = rig.session_subhash();
        queue(&mut d, SessionEvent::Commit(CommitReply { req: 1, epoch: EPOCH, arrival: a.arrival, accepted: true }));
        for r in &records { queue(&mut d, SessionEvent::Result(r.clone())); }
        d.emit(MachineId::Session, Fx::App(AppFx::SessionEffect(SessionFx::Acknowledge(vec![Receipt::of(&a), Receipt::of(&a)]))));
        frame(&mut rig, &mut d, Vec::new(), &mut trace);
        assert_eq!(rig.session_subhash(), before);
        assert!(rig.session_adapter.admitted(&next[0]), "stale replies/receipts cannot acknowledge the new batch");
        assert!(rig.session_adapter.fixture_resources().registry_writes.iter().all(|p| matches!(p, RegistryPlan::Revoke)));
        frame(&mut rig, &mut d, next.clone(), &mut trace); // Unknown unique result still owes final ACK.
        assert!(!rig.session_adapter.admitted(&next[0]));
        assert!(rig.session_adapter.take_results().is_empty());
    }

    #[test]
    fn one_pump_survives_cancellation_in_both_carried_representations() {
        for event_form in [false, true] {
            let mut rig = rig(&[(1, SessionOp::Login)]);
            let records = registry_stream(&mut rig);
            let mut d = Dispatcher::<AppHost>::new();
            let mut trace = Trace::default();
            pad(&mut d, BUDGET - 4);
            assert!(frame(&mut rig, &mut d, records.clone(), &mut trace).carried > 0);
            assert!(rig.session.snapshot_init().pending_commit.is_some());
            assert_eq!(rig.session_adapter.fixture_resources().registry_writes.len(), 1,
                "actual commit executed; its typed ACK, not a fabricated reply, is carried");
            if event_form {
                // The real screen-command effect adds one normal delivery hop. Old ACK schedules
                // Pump(App); that becomes Pump(Event) before this command cancels the old flow.
                d.emit(MachineId::Session, Fx::App(AppFx::Session(SessionCmd::StartSwitch(crate::auth::Picker::ChangeProfile))));
                pad(&mut d, BUDGET - 5);
            } else {
                execute_session_command(&mut d, SessionCmd::StartSwitch(crate::auth::Picker::ChangeProfile));
                pad(&mut d, BUDGET - 2);
            }
            let second = frame(&mut rig, &mut d, Vec::new(), &mut trace);
            assert!(second.carried > 0);
            let state = rig.session.snapshot_init();
            assert_eq!(state.epoch, EPOCH + 1);
            assert_eq!(state.pending_commit.as_ref().unwrap().req, 2);
            assert!(state.inbox.is_empty() && state.pump_pending);
            assert_eq!(trace.order.iter().filter(|x| x.0 == "pump-app").count(), usize::from(event_form));
            assert!(!trace.order.iter().any(|x| x.0 == "pump-event"));
            queue(&mut d, SessionEvent::Commit(CommitReply {
                req: 1, epoch: EPOCH, arrival: records[0].arrival, accepted: true,
            }));
            assert!(second.carried < BUDGET);
            pad(&mut d, BUDGET - second.carried - 1);
            assert!(frame(&mut rig, &mut d, Vec::new(), &mut trace).carried > 0);
            assert_eq!(rig.session.snapshot_init().pending_commit.as_ref().unwrap().req, 2,
                "old ACK must not settle the new commit before its real resource reply");
            frame(&mut rig, &mut d, Vec::new(), &mut trace);
            assert_eq!(trace.order.iter().filter(|x| x.0 == "pump-app").count(), 1);
            assert_eq!(trace.order.iter().filter(|x| x.0 == "pump-event").count(), 1);
            let pump = trace.order.iter().position(|x| x.0 == "pump-event").unwrap();
            let ack = trace.order.iter().position(|x| *x == ("commit-ack", 2)).unwrap();
            assert!(pump < ack, "carried Pump is consumed while the new commit is busy");
            assert!(!rig.session.snapshot_init().pump_pending);
            let fresh = rig.session_adapter.take_results();
            frame(&mut rig, &mut d, fresh, &mut trace);
            let state = rig.session.snapshot_init();
            assert!(state.pending.is_empty() && state.pending_commit.is_none() && state.inbox.is_empty() && !state.pump_pending);
            assert!(records.iter().all(|r| !rig.session_adapter.admitted(r)));
        }
    }

    #[test]
    fn seating_cancels_obsolete_interests_and_busy_pump_reschedules_late_roster_after_handoff_ack() {
        use std::sync::mpsc::{sync_channel, SyncSender};
        use std::time::Duration;
        struct Producer { release: SyncSender<()>, worker: Option<std::thread::JoinHandle<()>> }
        impl Drop for Producer {
            fn drop(&mut self) {
                let _ = self.release.try_send(());
                if let Some(worker) = self.worker.take() { let _ = worker.join(); }
            }
        }
        let mut rig = rig(&[(1, SessionOp::ProfileSwitch), (2, SessionOp::ServerRoster), (3, SessionOp::ServerRoster)]);
        let old = stored();
        let mut seated = old.clone();
        seated.user.uuid = "profile-b".into();
        seated.user.token = "profile-b".into();
        seated.server.address = "127.0.0.2".into();
        seated.server.origin_url = "http://127.0.0.2:32400".into();
        seated.server.token = "profile-b".into();
        seated.sources[0].address = seated.server.address.clone();
        seated.sources[0].origin_url = seated.server.origin_url.clone();
        seated.sources[0].token = "profile-b".into();
        let ready = serde_json::from_value(serde_json::json!({
            "epoch":EPOCH, "expected":crate::auth::SessionIdentity::of(&old),
            "outcome":{"Ready":{"delta":{"server":seated.server,"sources":seated.sources,
                "user":seated.user,"cache":null},"probes":[]}}
        })).unwrap();
        let mut late = seated.sources[0].clone();
        late.address = "127.0.0.3".into();
        late.origin_url = "http://127.0.0.3:32400".into();
        let roster = serde_json::from_value(serde_json::json!({
            "epoch":EPOCH, "expected":crate::auth::SessionIdentity::of(&seated),
            "resources":[{"clientIdentifier":"primary","provides":"server","owned":true,"accessToken":"profile-b"}],
            "reached":[late],"probes":[]
        })).unwrap();
        let (entered_tx, entered) = sync_channel(1);
        let (release, release_rx) = sync_channel(1);
        let mut producer = Producer { release, worker: None };
        let worker = &mut producer.worker;
        rig.session_adapter.launch(RequestId(1), SessionWorkKey { epoch: EPOCH, op: SessionOp::ProfileSwitch }, true,
            |job| { *worker = Some(std::thread::spawn(job)); true }, move |output| {
                output.progress(AuthProgress::ProfileSwitch(ready)).unwrap();
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).expect("main interleaves independent producer");
                output.complete(AuthProgress::ProfileRoster(roster)).unwrap();
            }).unwrap();
        entered.recv_timeout(Duration::from_secs(10)).unwrap();
        let expected = crate::auth::SessionIdentity::of(&old);
        rig.session_adapter.launch(RequestId(2), SessionWorkKey { epoch: EPOCH, op: SessionOp::ServerRoster }, true,
            |job| { job(); true }, move |output| {
                output.progress(AuthProgress::Registry(RegistryProgress::Install {
                    epoch: EPOCH, expected: Some(expected), primary: None,
                    sources: vec![SourceRef { machine_id: "obsolete-grant".into(), address: "127.0.0.9".into(),
                        port: 32400, token: "old-profile-token".into(), ..Default::default() }],
                })).unwrap();
            }).unwrap(); // Its real guard supplies the terminal after its progress.
        producer.release.send(()).unwrap();
        producer.worker.take().unwrap().join().unwrap();
        rig.session_adapter.launch(RequestId(3), SessionWorkKey { epoch: EPOCH, op: SessionOp::ServerRoster }, true,
            |job| { job(); true }, |_| {}).unwrap();
        let mut records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 5);
        assert_eq!(records.iter().map(|r| r.addr.req.0).collect::<Vec<_>>(), [1, 2, 2, 1, 3],
            "actual Landing arrival order, not a reordered fixture vector");
        let all = records.clone();
        let carried_obsolete = records.pop().unwrap();
        let mut d = Dispatcher::<AppHost>::new();
        let mut trace = Trace::default();
        pad(&mut d, BUDGET - 5); // Four observations + actual Ready resource commit; ACK is carried.
        assert!(frame(&mut rig, &mut d, records, &mut trace).carried > 0);
        assert_eq!(rig.session_adapter.fixture_resources().registry_writes.len(), 1);
        assert!(rig.session.snapshot_init().pending_commit.is_some());
        assert_eq!(rig.session.snapshot_init().inbox.len(), 3);
        assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Switching);
        execute_session_command(&mut d, SessionCmd::TakeReady);
        pad(&mut d, BUDGET - 2); // Real old ACK then TakeReady; C remains in the dispatcher.
        queue(&mut d, SessionEvent::Result(carried_obsolete.clone()));
        assert!(frame(&mut rig, &mut d, Vec::new(), &mut trace).carried > 0);
        let state = rig.session.snapshot_init();
        assert_eq!(state.persisted.user.uuid, "profile-b");
        assert!(state.pending[&1].phase == StreamPhase::ProfileSeated);
        assert!(!state.pending.contains_key(&2) && !state.pending.contains_key(&3));
        assert_eq!(state.pending_commit.as_ref().unwrap().req, 4);
        assert_eq!(state.inbox.len(), 1);
        assert!(state.pump_pending && rig.session_adapter.admitted(&carried_obsolete));
        assert!(all.iter().all(|r| rig.session_adapter.admitted(r)), "logical cancellation has not run queued receipt ACKs");
        frame(&mut rig, &mut d, Vec::new(), &mut trace);
        let state = rig.session.snapshot_init();
        assert!(state.pending.is_empty() && state.pending_commit.is_none() && state.inbox.is_empty() && !state.pump_pending);
        assert_eq!(state.persisted.user.uuid, "profile-b");
        assert_eq!(state.persisted.sources[0].address, "127.0.0.3");
        assert_eq!(rig.session_adapter.fixture_resources().disk.sources[0].address, "127.0.0.3");
        assert_eq!(trace.publications, 1);
        assert_eq!(trace.ready, 1);
        assert_eq!(state.profile_scope.0, 1);
        assert_eq!(trace.order.iter().filter(|x| x.0 == "pump-app").count(), 2);
        let pumps: Vec<_> = trace.order.iter().enumerate().filter_map(|(i, x)| (x.0 == "pump-event").then_some(i)).collect();
        assert_eq!(pumps.len(), 2);
        let handoff = trace.order.iter().position(|x| *x == ("commit-ack", 4)).unwrap();
        assert!(pumps[0] < handoff && handoff < pumps[1], "busy marker consumed, then ACK reschedules retained roster");
        let profile = trace.order.iter().position(|x| x.0 == "profile").unwrap();
        let ready = trace.order.iter().position(|x| x.0 == "ready").unwrap();
        assert!(profile < ready);
        for r in &all {
            assert!(!rig.session_adapter.admitted(r));
            assert_eq!(trace.acks.iter().filter(|ack| **ack == Receipt::of(r)).count(), 1);
        }
        assert!(rig.session_adapter.fixture_resources().registry_writes.iter().all(|plan| match plan {
            RegistryPlan::Install { sources, .. } => sources.iter().all(|s| s.machine_id == "primary"),
            _ => false,
        }), "obsolete grants never reach the resource sink");
        assert_eq!(rig.take_session_ready().unwrap().token, "profile-b");
        assert!(rig.take_session_ready().is_none());
    }
}

fn home_roster_failure_history(cached: bool, failure: u8) {
    use crate::auth::{SessionCmd, Phase};
    use crate::auth::owner::{SessionEvent, SessionWork, SessionOp, SessionWorkKey, SESSION_TOTAL_RESERVATIONS};
    use crate::ui::machine::RequestId;
    let mut stored = crate::plex::session::Session {
        client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
        ..Default::default()
    };
    if cached {
        stored.home_users.push(crate::plex::session::HomeUserRef {
            uuid: "cached-user".into(), title: "Cached".into(), ..Default::default()
        });
    }
    let mut rig = Bridge::for_session_test(crate::auth::SessionInit::captured(stored));
    let mut delayed = Vec::new();
    if failure == 4 {
        for req in 100..100 + SESSION_TOTAL_RESERVATIONS {
            rig.session_adapter.launch(RequestId(req), SessionWorkKey { epoch: 1, op: SessionOp::Login }, true,
                |job| { delayed.push(job); true }, |_| {}).unwrap();
        }
        // Superseded physical work still occupies capacity until its real closure/guard runs.
        rig.session_adapter.cancel_all();
    } else if failure != 0 {
        rig.session_adapter.inject_fixture_work(3, move |output, input| {
            let SessionWork::HomeRoster { expected, .. } = input else { panic!("home roster capture") };
            if failure == 1 { return; }
            let mut expected = serde_json::to_value(expected).unwrap();
            if failure == 2 { expected["account_token"] = "wrong-account".into(); }
            let fact = serde_json::from_value(serde_json::json!({
                "epoch":2, "expected":expected, "users":null
            })).unwrap();
            output.complete(crate::auth::AuthProgress::HomeRoster(fact)).unwrap();
        });
    } // 0: real spawn refusal; 1: real Dropped guard; 2: rejected body; 3: accepted None.
    let mut d = Dispatcher::<AppHost>::new();
    execute_session_command(&mut d, SessionCmd::StartSwitch(crate::auth::Picker::ChangeProfile));
    d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    if failure != 4 { assert_eq!(rig.auth_read().0.phase, Phase::Profiles); }
    let records = rig.session_adapter.take_results();
    assert_eq!(records.len(), if failure == 4 { 0 } else { 2 });
    let results = records.into_iter().map(|r| (r.addr, AppMsg::Session(SessionEvent::Result(r)))).collect();
    d.frame_with(&mut rig, Tick::default(), Vec::new(), results, &mut NoTap, false);
    assert_eq!(rig.auth_read().0.phase, if cached { Phase::Profiles } else { Phase::Error },
        "failure kind {failure}, cached={cached}: completed work cannot leave an empty picker loading");
    assert_eq!(rig.auth_read().0.users.len(), usize::from(cached));
    assert_eq!(&*rig.auth_read().0.error, if cached { "" } else { "Couldn't load profiles — check the connection." });
    assert!(rig.session.snapshot_init().pending.is_empty());
    assert!(rig.take_session_ready().is_none());
    rig.session_adapter.cancel_all();
    for job in delayed { job(); }
    assert!(rig.session_adapter.take_results().is_empty());
}

#[test]
fn failed_home_roster_terminal_finishes_empty_picker_but_preserves_cached_tiles() {
    for cached in [false, true] {
        for failure in 0..4 { home_roster_failure_history(cached, failure); }
    }
}

#[test]
fn home_roster_admission_failure_finishes_empty_picker_but_preserves_cached_tiles() {
    for cached in [false, true] { home_roster_failure_history(cached, 4); }
}

#[cfg(test)]
mod qr_exhaustion_guard {
    use super::*;
    use crate::auth::owner::{AdmissionId, AdmissionState, Identity, Pending, SessionEvent,
        SessionFx, SessionOp, SessionWorkKey, StreamPhase, SESSION_TOTAL_RESERVATIONS};
    use crate::ui::landing::AdmissionError;
    use crate::ui::machine::{RequestId, Stamped};
    use std::sync::mpsc::{sync_channel, SyncSender};
    use std::time::Duration;

    struct Jobs {
        rig: Bridge,
        finish: SyncSender<()>,
        running: Option<std::thread::JoinHandle<()>>,
        delayed: Vec<Box<dyn FnOnce() + Send>>,
    }
    impl Jobs {
        fn defer(&mut self, req: u32, key: SessionWorkKey) -> Result<(), AdmissionError> {
            let delayed = &mut self.delayed;
            self.rig.session_adapter.launch(RequestId(req), key, true,
                |job| { delayed.push(job); true }, |_| {})
        }
    }
    impl Drop for Jobs {
        fn drop(&mut self) {
            self.rig.session_adapter.cancel_all();
            let _ = self.finish.try_send(());
            if let Some(worker) = self.running.take() { let _ = worker.join(); }
            for job in self.delayed.drain(..) {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
            }
        }
    }
    #[derive(Default)]
    struct Cancellation {
        cancelled: Vec<u32>,
        retired: Vec<u32>,
    }
    impl crate::ui::dispatch::Tap<AppHost> for Cancellation {
        fn effect(&mut self, _: u64, effect: &Stamped<AppHost>) {
            match &effect.fx {
                Fx::App(AppFx::SessionEffect(SessionFx::Cancel { requests, .. })) =>
                    self.cancelled.extend(requests.iter().copied()),
                Fx::App(AppFx::SessionEffect(SessionFx::Retire { req })) => self.retired.push(*req),
                _ => {}
            }
        }
    }

    #[test]
    fn qr_allocator_exhaustion_cancel_and_retire_hold_running_producer_until_guard_ack() {
        let key = SessionWorkKey { epoch: u64::from(u32::MAX) + 181, op: SessionOp::Login };
        let mut init = crate::auth::SessionInit::captured(crate::plex::session::Session {
            client_id: "synthetic-client".into(), ..Default::default()
        });
        init.epoch = key.epoch;
        init.phase = crate::auth::Phase::Creating;
        init.next_qr = u64::MAX;
        init.next_req = SESSION_TOTAL_RESERVATIONS;
        init.pending.insert(1, Pending { key, expected: Identity::of(&init.persisted),
            lifecycle: None, last_arrival: None, phase: StreamPhase::Running, capture: None,
            admission: AdmissionState::Awaiting(AdmissionId(1)) });
        let (entered_tx, entered) = sync_channel(1);
        let (finish, finish_rx) = sync_channel(1);
        let mut jobs = Jobs { rig: Bridge::for_session_test(init), finish,
            running: None, delayed: Vec::new() };
        let running = &mut jobs.running;
        let main_thread = std::thread::current().id();
        jobs.rig.session_adapter.launch(RequestId(1), key, true,
            |job| { *running = Some(std::thread::spawn(job)); true }, move |output| {
                assert_ne!(std::thread::current().id(), main_thread);
                output.progress(crate::auth::LoginProgress::CodeReady {
                    epoch: key.epoch, code: "ABCD".into(), qr_png: vec![1, 2, 3],
                }.into()).unwrap();
                entered_tx.send(()).unwrap();
                finish_rx.recv_timeout(Duration::from_secs(10)).expect("main must release producer");
                assert!(output.cancelled());
                // Actual CodeReady producer and running guard, not a network PIN-poll claim.
            }).unwrap();
        entered.recv_timeout(Duration::from_secs(10)).expect("producer must actually enter");
        for req in 2..=SESSION_TOTAL_RESERVATIONS { jobs.defer(req, key).unwrap(); }
        let records = jobs.rig.session_adapter.take_results();
        assert_eq!(records.len(), 1);
        assert!(!records[0].terminal && jobs.rig.session_adapter.admitted(&records[0]));
        let mut d = Dispatcher::<AppHost>::new();
        let mut tap = Cancellation::default();
        let results = records.iter().cloned().map(|r|
            (r.addr, AppMsg::Session(SessionEvent::Result(r)))).collect();
        d.frame_with(&mut jobs.rig, Tick::default(), Vec::new(), results, &mut tap, false);
        assert_eq!(tap.cancelled, [1]);
        assert_eq!(tap.retired, [1]);
        assert_eq!(jobs.rig.auth_read().0.phase, crate::auth::Phase::Error);
        assert_eq!(jobs.rig.auth_read().0.qr_generation, 0);
        assert_eq!(jobs.rig.session.snapshot_init().next_qr, u64::MAX);
        assert!(jobs.rig.session.snapshot_init().pending.is_empty());
        assert!(!jobs.rig.session_adapter.admitted(&records[0]), "processed CodeReady receipt returned");
        assert!(jobs.rig.session_adapter.take_results().is_empty());
        assert!(matches!(jobs.defer(100, key), Err(AdmissionError::Capacity)),
            "logical Cancel+Retire and receipt ACK cannot release this still-running producer");
        jobs.finish.send(()).unwrap();
        jobs.running.take().unwrap().join().unwrap();
        assert!(matches!(jobs.defer(101, key), Err(AdmissionError::Capacity)), "guard terminal still needs main drain");
        assert!(jobs.rig.session_adapter.take_results().is_empty());
        jobs.defer(102, key).expect("completed guard plus main drain releases exactly one reservation");
        assert!(matches!(jobs.defer(103, key), Err(AdmissionError::Capacity)));
    }
}

#[test]
fn admission_refusal_correlation_survives_carried_acceptance_and_transferred_terminal() {
    use crate::auth::owner::{AdmissionId, AdmissionReply, AdmissionState, Identity, Pending,
        SessionEvent, SessionOp, SessionWork, SessionWorkKey, StreamPhase, SESSION_TOTAL_RESERVATIONS};
    use crate::ui::machine::{Addr, RequestId};
    fn deliver(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, event: SessionEvent) {
        d.emit(MachineId::Session, Fx::Deliver(MachineId::Session, Delivery::Machine(AppMsg::Session(event))));
        d.frame_with(rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    }
    let key = SessionWorkKey { epoch: u64::from(u32::MAX) + 171, op: SessionOp::Login };
    let fixture = |req| {
        let mut init = crate::auth::SessionInit::captured(crate::plex::session::Session {
            client_id: "synthetic-client".into(), ..Default::default()
        });
        init.epoch = key.epoch;
        init.phase = crate::auth::Phase::Creating;
        init.next_req = req;
        init.pending.insert(req, Pending { key, expected: Identity::of(&init.persisted),
            lifecycle: None, last_arrival: None, phase: StreamPhase::Running, capture: None,
            admission: AdmissionState::Awaiting(AdmissionId(req)) });
        Bridge::for_session_test(init)
    };
    let mut rig = fixture(1);
    let mut delayed = Vec::new();
    rig.session_adapter.launch(RequestId(1), key, true,
        |job| { delayed.push(job); true }, |_| {}).unwrap();
    let negative = AdmissionReply { addr: Addr { to: MachineId::Session, req: RequestId(1) },
        key, correlation: AdmissionId(1), accepted: false };
    assert!(rig.session_adapter.resource_admitted(&negative));
    let before = rig.session_subhash();
    let mut d = Dispatcher::<AppHost>::new();
    deliver(&mut rig, &mut d, SessionEvent::Admission(negative));
    assert_eq!(rig.session_subhash(), before, "resource acceptance protects even before its first observation/reply");
    for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST {
        execute_session_command(&mut d, crate::auth::SessionCmd::DismissPinError);
    }
    d.emit(MachineId::Session, Fx::Deliver(MachineId::Session, Delivery::Machine(AppMsg::Session(
        SessionEvent::Admission(AdmissionReply { accepted: true, ..negative })))));
    let carried = d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    assert!(carried.carried > 0);
    assert!(rig.session.snapshot_init().pending[&1].admission == AdmissionState::Awaiting(AdmissionId(1)));
    d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    assert!(rig.session.snapshot_init().pending[&1].admission == AdmissionState::Accepted(AdmissionId(1)));
    let accepted = rig.session_subhash();
    deliver(&mut rig, &mut d, SessionEvent::Admission(negative));
    assert_eq!(rig.session_subhash(), accepted);
    delayed.pop().unwrap()(); // Real completion guard supplies one terminal; no payload is fabricated.
    let terminal = rig.session_adapter.take_results();
    assert_eq!(terminal.len(), 1);
    assert!(rig.session_adapter.admitted(&terminal[0]));
    assert!(rig.session_adapter.resource_admitted(&negative), "receipt metadata protects after launch metadata is removed");
    deliver(&mut rig, &mut d, SessionEvent::Admission(negative));
    assert_eq!(rig.session_subhash(), accepted);
    deliver(&mut rig, &mut d, SessionEvent::Result(terminal[0].clone()));
    assert!(!rig.session_adapter.admitted(&terminal[0]));
    assert!(rig.session.snapshot_init().pending.is_empty());

    // Independent history: producer already completed/transferred, but neither acceptance nor
    // an observation has reached the owner. Only the adapter's retained receipt can protect it.
    let mut rig = fixture(1);
    let mut d = Dispatcher::<AppHost>::new();
    rig.session_adapter.launch(RequestId(1), key, true, |job| { job(); true }, |_| {}).unwrap();
    let terminal = rig.session_adapter.take_results();
    assert_eq!(terminal.len(), 1);
    assert!(rig.session.snapshot_init().pending[&1].admission == AdmissionState::Awaiting(AdmissionId(1)));
    assert!(rig.session_adapter.resource_admitted(&negative));
    let before = rig.session_subhash();
    deliver(&mut rig, &mut d, SessionEvent::Admission(negative));
    assert_eq!(rig.session_subhash(), before);
    assert!(rig.session_adapter.admitted(&terminal[0]));
    deliver(&mut rig, &mut d, SessionEvent::Result(terminal[0].clone()));
    assert!(rig.session.snapshot_init().pending.is_empty());

    // Contrast with a genuinely capacity-refused request: only its exact negative correlation
    // retires it. All fillers stay in private adapter resources and are cancelled on teardown.
    let req = SESSION_TOTAL_RESERVATIONS + 1;
    let mut rig = fixture(req);
    for id in 1..=SESSION_TOTAL_RESERVATIONS {
        rig.session_adapter.launch(RequestId(id), key, true,
            |job| { delayed.push(job); true }, |_| {}).unwrap();
    }
    let refusal = rig.session_adapter.start_work(RequestId(req), key, AdmissionId(req),
        SessionWork::Login { client_id: "synthetic-client".into() }).unwrap_err();
    assert!(!refusal.accepted && !rig.session_adapter.resource_admitted(&refusal));
    let before = rig.session_subhash();
    deliver(&mut rig, &mut d, SessionEvent::Admission(AdmissionReply {
        correlation: AdmissionId(req + 1), ..refusal
    }));
    assert_eq!(rig.session_subhash(), before, "stale rejection cannot retire never-admitted current work either");
    deliver(&mut rig, &mut d, SessionEvent::Admission(refusal));
    assert!(rig.session.snapshot_init().pending.is_empty());
    assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Error);
    rig.session_adapter.cancel_all();
    for job in delayed { job(); }
    assert!(rig.session_adapter.take_results().is_empty());
}

#[test]
fn bridge_cached_session_hash_distinguishes_logical_state_without_ui_damage() {
    let init = crate::auth::SessionInit::captured(Default::default());
    let mut other = init.clone();
    other.next_req = 1;
    let mut a = Bridge::for_session_test(init);
    let b = Bridge::for_session_test(other);
    assert_eq!(a.auth_read().0.phase, b.auth_read().0.phase);
    assert_ne!(a.session_subhash(), b.session_subhash());
    assert_eq!(a.session_subhash(), a.session.subhash());
    assert!(a.session.take_logical_dirty());
    let before = a.session_subhash();
    let read = a.session.publication();
    let mut d = Dispatcher::<AppHost>::new();
    execute_session_command(&mut d, crate::auth::SessionCmd::DismissPinError);
    d.frame_with(&mut a, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    assert_eq!(a.session_subhash(), before);
    assert!(!a.session.take_logical_dirty());
    assert!(std::sync::Arc::ptr_eq(&read, &a.session.publication()));
}

#[test]
fn dismissing_the_keypad_clears_the_pin_verdict() {
    let mut init = crate::auth::SessionInit::captured(Default::default());
    init.phase = crate::auth::Phase::Profiles;
    init.pin_denied = true;
    init.error = "Couldn't switch profile — check the connection.".into();
    let mut rig = Bridge::for_session_test(init);
    let retained = rig.session.publication();
    let mut d = Dispatcher::<AppHost>::new();
    execute_session_command(&mut d, crate::auth::SessionCmd::DismissPinError);
    assert!(rig.auth_read().0.pin_denied, "emission alone cannot dismiss the verdict");
    d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    assert!(!rig.auth_read().0.pin_denied, "the verdict goes with the pad");
    assert_eq!(&*rig.auth_read().0.error, "Couldn't switch profile — check the connection.",
        "the non-PIN roster banner is not collateral");
    assert!(retained.pin_denied, "a retained old view remains coherent");
    let after = rig.session.subhash();
    execute_session_command(&mut d, crate::auth::SessionCmd::DismissPinError);
    d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    assert_eq!(rig.session.subhash(), after, "repeat dismissal is a logical no-op");
}

#[test]
fn refused_restart_preserves_the_exact_owner_state_through_dispatch() {
    use crate::auth::{Phase, SessionCmd};
    use crate::auth::owner::ReplyTo;
    // Ready, changed phase, and replacement code are the three old wait-identity refusals.
    // Checked allocator exhaustion must also refuse BEFORE cancellation of a matching wait.
    for (phase, generation, epoch, next_req) in [
        (Phase::Ready, 7, 19, 3),
        (Phase::Discovering, 7, 19, 3),
        (Phase::Waiting, 8, 19, 3),
        (Phase::Waiting, 7, u64::MAX, 3),
        (Phase::Waiting, 7, 19, u32::MAX),
    ] {
        let mut init = crate::auth::SessionInit::captured(Default::default());
        init.phase = phase;
        init.qr_gen = generation;
        init.epoch = epoch;
        init.next_req = next_req;
        init.signin_active = true;
        init.apply_pending = true;
        let mut rig = Bridge::for_session_test(init);
        let retained = rig.session.publication();
        let before = rig.session.subhash();
        let mut d = Dispatcher::<AppHost>::new();
        execute_session_command(&mut d, SessionCmd::RestartWait {
            phase: Phase::Waiting, qr_generation: 7,
            reply: ReplyTo { instance: 41, correlation: 9 },
        });
        d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
        assert_eq!(rig.session.subhash(), before, "refusal must leave ALL logical state unchanged");
        assert!(std::sync::Arc::ptr_eq(&retained, &rig.session.publication()));
        let state = rig.session.snapshot_init();
        assert_eq!(state.epoch, epoch);
        assert_eq!(state.next_req, next_req);
        assert!(state.signin_active && state.apply_pending);
        assert!(rig.session_adapter.fixture_resources().registry_writes.is_empty());
        assert!(rig.session_adapter.fixture_resources().coordinator_events.is_empty());
    }
    // This covers owner refusal, not live-screen delivery of the reply; mounted Login tests
    // retain that separate responsibility. The allowed restart branch is mapped separately.
}

#[test]
fn mounted_profiles_selection_crosses_owner_and_live_ack_with_constructor_and_command_carry() {
    use crate::auth::owner::Command;
    use crate::ui::machine::{PressId, RequestId};
    let _guard = crate::testlock::serial();
    let epoch = u64::from(u32::MAX) + 31;
    let mut init = crate::auth::SessionInit::captured(crate::plex::session::Session {
        client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
        user: crate::plex::session::UserRef { uuid: "synthetic-user".into(),
            token: "synthetic-profile-token".into(), ..Default::default() }, ..Default::default()
    });
    init.epoch = epoch;
    init.phase = crate::auth::Phase::Profiles;
    init.pin_denied = true;
    init.users = vec![crate::auth::UserTile { uuid: "synthetic-user".into(),
        title: "Synthetic user".into(), ..Default::default() }];
    let mut rig = Bridge::for_session_test(init);
    let mut d = Dispatcher::<AppHost>::new();
    let probe = |d: &Dispatcher<AppHost>| {
        let mut text = String::new();
        d.top_screen().unwrap().state().probe(&mut text);
        text
    };
    for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST {
        execute_session_command(&mut d, Command::NoteDeleteLeftovers(0));
    }
    d.request(MachineId::Nav, NavOp::Root(AppArg::Profiles));
    let mounted = d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    assert!(mounted.carried > 0);
    let instance = d.top_page().expect("actual AppMounter must construct the Profiles instance");
    assert_eq!(d.top_screen().unwrap().name(), "profiles");
    assert!(rig.auth_read().0.pin_denied, "constructor dismissal must still be carried");
    assert!(probe(&d).contains("denied=false"), "first-paint state must not inherit the stale denial");
    d.frame_with(&mut rig, Tick { ms: 16, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
    assert!(!rig.auth_read().0.pin_denied, "typed constructor dismissal reaches the owner");
    assert_eq!(d.focus_record().unwrap().1, 0);

    // A committed press reaches the actual mounted screen and its real focus-selected action.
    // Gesture recognition itself is covered by the existing press tests; no Session command
    // is manufactured here for the selection.
    for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST - 1 {
        execute_session_command(&mut d, Command::NoteDeleteLeftovers(0));
    }
    d.emit(MachineId::Input, Fx::Deliver(MachineId::Instance(instance),
        Delivery::Screen(ScreenEvent::PressCommit(PressId(1)))));
    let pressed = d.frame_with(&mut rig, Tick { ms: 32, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
    assert!(pressed.carried > 0);
    assert_eq!(rig.auth_read().0.flow_epoch, epoch, "UI emission is not owner acceptance");
    assert!(probe(&d).contains("selection_pending=true"));
    assert!(probe(&d).contains("selection_accepted=false"));
    for (to, request, correlation) in [
        (InstanceId(u32::MAX), 1, 1), // foreign instance
        (instance, 0, 0), // stale/nonmatching correlation
        (instance, 2, 1), // mismatched async header and payload
    ] {
        d.emit(MachineId::Session, Fx::Deliver(MachineId::Instance(to), Delivery::Screen(
            ScreenEvent::Async(RequestId(request), AppMsg::SelectionReply {
                correlation, accepted: true, flow_epoch: u64::MAX,
            }))));
    }
    let accepted = d.frame_with(&mut rig, Tick { ms: 48, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
    assert!(accepted.dropped_deliveries > 0, "foreign-instance delivery must be rejected by the dispatcher");
    assert_eq!(rig.auth_read().0.flow_epoch, epoch + 1);
    assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Ready);
    assert!(probe(&d).contains("selection_accepted=true"),
        "the real live screen, not a Tap, must receive the owner's acceptance");
    d.emit(MachineId::Session, Fx::Deliver(MachineId::Instance(instance), Delivery::Screen(
        ScreenEvent::Async(RequestId(1), AppMsg::SelectionReply {
            correlation: 1, accepted: true, flow_epoch: epoch + 1,
        }))));
    d.frame_with(&mut rig, Tick { ms: 64, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
    assert!(probe(&d).contains("selection_pending=false"), "the next matching retained read settles the screen");
    assert!(probe(&d).contains("selection_accepted=false"));
    assert_eq!(rig.auth_read().0.flow_epoch, epoch + 1, "duplicate ACK does not execute selection again");
    // A top-page change can retain cached live instances. Use the actual profile-reset
    // retirement boundary before proving rejection of a genuinely retired instance.
    d.reset_for_profile();
    d.request(MachineId::Nav, NavOp::Root(AppArg::Login));
    let retired = d.frame_with(&mut rig, Tick { ms: 80, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
    assert!(retired.unmounted.contains(&instance));
    d.prune(&retired.unmounted); // the same frame-tail step performed by frame_ingest
    let replacement = d.top_page().unwrap();
    assert_ne!(replacement, instance);
    d.emit(MachineId::Session, Fx::Deliver(MachineId::Instance(instance), Delivery::Screen(
        ScreenEvent::Async(RequestId(1), AppMsg::SelectionReply {
            correlation: 1, accepted: true, flow_epoch: epoch + 1,
        }))));
    let stale = d.frame_with(&mut rig, Tick { ms: 96, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
    assert!(stale.dropped_deliveries > 0);
    assert_eq!(d.top_page(), Some(replacement));
}

#[test]
fn erased_publication_waits_for_carried_resource_completion() {
    use crate::auth::owner::Command;
    let mut init = crate::auth::SessionInit::captured(crate::plex::session::Session {
        client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
        ..Default::default()
    });
    init.phase = crate::auth::Phase::Profiles;
    init.pin_code = "old-code".into();
    init.signin_active = true;
    let mut rig = Bridge::for_session_test(init);
    rig.session_adapter.fixture_resources().sweep_leftovers = 2;
    let mut d = Dispatcher::<AppHost>::new();
    for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST - 1 {
        execute_session_command(&mut d, Command::DismissPinError);
    }
    execute_session_command(&mut d, Command::EraseLocal);
    let first = d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
    assert!(first.carried > 0);
    assert!(!rig.session_adapter.fixture_resources().disk.account_token.is_empty());
    assert_ne!(rig.auth_read().0.phase, crate::auth::Phase::Deleted,
        "logical cancellation is not completion of the still-carried disk/resource erase");
    let erased_epoch = rig.auth_read().0.flow_epoch;
    let pending = rig.session.snapshot_init();
    let restored = crate::auth::SessionMachine::from_init(pending);
    assert_eq!(restored.subhash(), rig.session.subhash(), "pending erase is canonical/init state");
    execute_session_command(&mut d, Command::StartLogin);
    d.frame_with(&mut rig, Tick { ms: 16, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
    assert!(rig.session_adapter.fixture_resources().disk.account_token.is_empty());
    assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Deleted);
    // Preserve the former deleted_ctl constructor assertions through actual queued erasure.
    let erased = rig.session.snapshot_init();
    assert!(erased.persisted.client_id.is_empty());
    assert!(erased.persisted.account_token.is_empty());
    assert!(!erased.signin_active);
    assert!(!erased.apply_pending);
    assert!(erased.pin_code.is_empty());
    assert_eq!(rig.auth_read().0.delete_leftovers, 2);
    assert!(rig.session.snapshot_init().pending_erase.is_none());
    assert!(rig.take_reqs().iter().any(|req| matches!(req, LoopReq::LocalDataErased)));
    let events = &rig.session_adapter.fixture_resources().coordinator_events;
    assert!(matches!(events.first(), Some(crate::auth::owner::CoordinatorAction::CloseTelemetry)));
    assert!(!events.iter().any(|event| matches!(event, crate::auth::owner::CoordinatorAction::SignInStarted)),
        "a carried start cannot launch before erase completion");
    execute_session_command(&mut d, Command::StartLogin);
    d.emit(MachineId::Session, Fx::Deliver(MachineId::Session, Delivery::Machine(AppMsg::Session(
        crate::auth::owner::SessionEvent::Erased { epoch: erased_epoch, leftovers: 99 }))));
    d.frame_with(&mut rig, Tick { ms: 32, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
    assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Creating);
    assert_eq!(rig.auth_read().0.delete_leftovers, 2, "duplicate old completion cannot overwrite a newer flow");
}

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
            assert!(output.complete(crate::auth::endpoint_work_fact(1, expected, lifecycle,
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
