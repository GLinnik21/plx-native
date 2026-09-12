//! Reserved controller regression migration child module; see the parent-approved handoff.
//! Use the real Bridge/owner/dispatcher and instance-local resource/worker fixtures.
//! Core owns production seams and old auth test removal after assertion mapping review.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::auth::{Phase, Picker, SessionCmd, SessionInit, LoginProgress};
    use crate::auth::owner::{ReplyTo, SessionEnvelope, SessionEvent, SessionOp, SessionWork};
    use crate::plex::session::{Session, ServerRef, UserRef, HomeUserRef};

    const INITIAL_EPOCH: u64 = u32::MAX as u64 + 40;

    fn stored(protected: bool) -> Session {
        let uuid = if protected { "synthetic-adult" } else { "synthetic-kid" };
        Session {
            client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
            server: ServerRef { machine_id: "stored-server".into(), name: "Stored".into(),
                address: "127.0.0.1".into(), port: 32400, token: "synthetic-server-token".into(),
                ..Default::default() },
            user: UserRef { uuid: uuid.into(), token: "synthetic-user-token".into(), ..Default::default() },
            home_users: vec![HomeUserRef { uuid: uuid.into(), protected, ..Default::default() }],
            ..Default::default()
        }
    }

    fn rig(stored: Session) -> Bridge {
        let mut init = SessionInit::captured(stored);
        init.epoch = INITIAL_EPOCH;
        init.picker = Picker::Boot;
        Bridge::for_session_test(init)
    }

    fn step(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, records: Vec<SessionEnvelope>) {
        let results = records.into_iter().map(|record|
            (record.addr, AppMsg::Session(SessionEvent::Result(record)))).collect();
        d.frame_with(rig, Tick::default(), Vec::new(), results, &mut NoTap, false);
    }

    fn command(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, command: SessionCmd) {
        execute_session_command(d, command);
        step(rig, d, Vec::new());
    }

    fn resource_state(rig: &mut Bridge) -> serde_json::Value {
        let r = rig.session_adapter.fixture_resources();
        serde_json::json!({ "disk": r.disk, "registry": r.registry_writes })
    }

    fn back() -> SessionCmd {
        SessionCmd::BackAtRoot { reply: ReplyTo { instance: 71, correlation: 1 } }
    }

    // Synthetic account observations, not a live pin-poll worker. The injected producer completes
    // normally and leaves real Landing receipts; the owner retains interest until they are drained.
    // This tests cancellation of logical interest and receipt ownership, not a running worker's
    // physical acknowledgement. No reservation is manually released by this fixture.
    fn inject_login(rig: &mut Bridge, req: u32, epoch: u64) {
        rig.session_adapter.inject_fixture_work(req, move |output, input| {
            assert!(matches!(input, SessionWork::Login { .. }));
            assert!(!output.cancelled());
            output.progress(LoginProgress::CodeReady {
                epoch, code: "ABCD".into(), qr_png: vec![1, 2, 3],
            }.into()).unwrap();
            output.progress(LoginProgress::Authorized {
                epoch, token: "synthetic-new-account".into(),
            }.into()).unwrap();
            output.complete(LoginProgress::SignedIn {
                epoch,
                server: ServerRef { machine_id: "different-discovery-server".into(),
                    address: "127.0.0.2".into(), port: 32400, token: "synthetic-new-server".into(),
                    ..Default::default() },
                sources: Vec::new(), users: Vec::new(),
            }.into()).unwrap();
        });
    }

    fn waiting(rig: &mut Bridge, d: &mut Dispatcher<AppHost>) -> Vec<SessionEnvelope> {
        let next = rig.session.snapshot_init().next_req.checked_add(1).unwrap();
        let epoch = rig.auth_read().0.flow_epoch + 1;
        inject_login(rig, next, epoch);
        command(rig, d, SessionCmd::StartLogin);
        let mut records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 3);
        assert!(!records[0].terminal && !records[1].terminal && records[2].terminal);
        assert!(records.iter().all(|record| rig.session_adapter.admitted(record)));
        let code = records.remove(0);
        step(rig, d, vec![code.clone()]);
        assert!(!rig.session_adapter.admitted(&code), "processed QR receipt is acknowledged");
        assert_eq!(rig.auth_read().0.phase, Phase::Waiting);
        assert_eq!(rig.auth_read().0.flow_epoch, epoch);
        assert_eq!(rig.auth_read().0.code.as_ref(), "ABCD");
        let state = rig.session.snapshot_init();
        assert!(matches!(state.pending.get(&next).map(|p| p.key.op), Some(SessionOp::Login)));
        records
    }

    #[test]
    fn a_refused_back_leaves_the_live_pin_poll_running() {
        for session in [Session::default(), stored(true)] {
            let mut rig = rig(session);
            let mut d = Dispatcher::<AppHost>::new();
            let mut records = waiting(&mut rig, &mut d);
            let publication = rig.session.publication();
            let before = rig.session.subhash();
            let resources = resource_state(&mut rig);
            command(&mut rig, &mut d, back());
            assert_eq!(rig.session_adapter.fixture_resources().back_results, [false]);
            assert_eq!(rig.session.subhash(), before, "refused BACK preserves the whole owner");
            assert!(std::sync::Arc::ptr_eq(&publication, &rig.session.publication()));
            assert_eq!(rig.auth_read().0.phase, Phase::Waiting);
            let state = rig.session.snapshot_init();
            assert!(state.signin_active);
            assert!(state.pending.contains_key(&records[0].addr.req.0));
            assert!(records.iter().all(|record| rig.session_adapter.admitted(record)));
            assert_eq!(resource_state(&mut rig), resources);
            // Repeated root press is cooldown-denied; it cannot cancel owner interest either.
            command(&mut rig, &mut d, back());
            assert_eq!(rig.session.subhash(), before);
            let authorized = records.remove(0);
            step(&mut rig, &mut d, vec![authorized.clone()]);
            assert_eq!(rig.auth_read().0.phase, Phase::Discovering,
                "the same admitted poll stream still advances after refusal");
            assert_eq!(rig.session.snapshot_init().persisted.account_token, "synthetic-new-account");
            assert!(!rig.session_adapter.admitted(&authorized));
            step(&mut rig, &mut d, records);
            assert_eq!(rig.auth_read().0.phase, Phase::Ready);
        }

        let mut rig = rig(stored(false));
        let mut d = Dispatcher::<AppHost>::new();
        let records = waiting(&mut rig, &mut d);
        let old_epoch = rig.auth_read().0.flow_epoch;
        command(&mut rig, &mut d, back());
        assert_eq!(rig.session_adapter.fixture_resources().back_results, [true]);
        assert_eq!(rig.auth_read().0.flow_epoch, old_epoch + 1);
        assert_eq!(rig.auth_read().0.phase, Phase::Ready);
        let state = rig.session.snapshot_init();
        assert!(state.apply_pending);
        assert!(!state.signin_active);
        assert!(!state.pending.contains_key(&records[0].addr.req.0));
        assert!(records.iter().all(|record| rig.session_adapter.admitted(record)),
            "logical cancel must not prematurely acknowledge carried receipts");
        step(&mut rig, &mut d, records.clone());
        assert!(records.iter().all(|record| !rig.session_adapter.admitted(record)));
    }

    #[test]
    fn a_refused_restart_invalidates_nothing() {
        // Refusal's complete Ready/phase/code/exhaustion table is already checked by
        // session_protocol_tests::refused_restart_preserves_the_exact_owner_state_through_dispatch.
        // This mapping supplies its original accepted and unconditional-start contrast.
        let mut rig = rig(Session::default());
        let mut d = Dispatcher::<AppHost>::new();
        let records = waiting(&mut rig, &mut d);
        let epoch = rig.auth_read().0.flow_epoch;
        let generation = rig.auth_read().0.qr_generation;
        let req = rig.session.snapshot_init().next_req.checked_add(1).unwrap();
        inject_login(&mut rig, req, epoch + 1);
        command(&mut rig, &mut d, SessionCmd::RestartWait {
            phase: Phase::Waiting, qr_generation: generation,
            reply: ReplyTo { instance: 71, correlation: 2 },
        });
        assert_eq!(rig.auth_read().0.flow_epoch, epoch + 1, "matching restart advances exactly once");
        assert_eq!(rig.auth_read().0.phase, Phase::Creating);
        let state = rig.session.snapshot_init();
        assert!(!state.pending.contains_key(&records[0].addr.req.0));
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.pending[&req].key.epoch, epoch + 1);
        assert!(records.iter().all(|record| rig.session_adapter.admitted(record)));
        let before = rig.session.subhash();
        step(&mut rig, &mut d, records.clone());
        assert_eq!(rig.session.subhash(), before);
        assert!(records.iter().all(|record| !rig.session_adapter.admitted(record)));
        let fresh = rig.session_adapter.take_results();
        assert_eq!(fresh.len(), 3);
        step(&mut rig, &mut d, fresh);
        assert_eq!(rig.auth_read().0.phase, Phase::Ready);
        assert!(rig.session.snapshot_init().apply_pending);
        let settled = rig.auth_read().0.flow_epoch;
        let req = rig.session.snapshot_init().next_req.checked_add(1).unwrap();
        inject_login(&mut rig, req, settled + 1);
        command(&mut rig, &mut d, SessionCmd::StartLogin);
        assert_eq!(rig.auth_read().0.flow_epoch, settled + 1);
        assert_eq!(rig.auth_read().0.phase, Phase::Creating);
        assert!(!rig.session.snapshot_init().apply_pending,
            "unconditional StartLogin can replace a completed-but-unconsumed handoff");
    }

    #[test]
    fn apply_progress_drops_an_observation_from_a_retired_epoch() {
        let mut rig = rig(Session::default());
        let mut d = Dispatcher::<AppHost>::new();
        let records = waiting(&mut rig, &mut d);
        let old_epoch = rig.auth_read().0.flow_epoch;
        let req = rig.session.snapshot_init().next_req.checked_add(1).unwrap();
        inject_login(&mut rig, req, old_epoch + 1);
        command(&mut rig, &mut d, SessionCmd::StartLogin);
        let before = rig.session.subhash();
        let publication = rig.session.publication();
        let resources = resource_state(&mut rig);
        step(&mut rig, &mut d, records.clone());
        assert!(rig.session.snapshot_init().persisted.account_token.is_empty());
        assert_eq!(rig.auth_read().0.phase, Phase::Creating,
            "the real newer StartLogin owns Creating; old test manufactured Idle");
        assert_eq!(rig.auth_read().0.flow_epoch, old_epoch + 1);
        assert_eq!(rig.session.subhash(), before);
        assert!(std::sync::Arc::ptr_eq(&publication, &rig.session.publication()));
        assert_eq!(resource_state(&mut rig), resources);
        assert!(records.iter().all(|record| !rig.session_adapter.admitted(record)));
        assert_eq!(rig.session.snapshot_init().pending.len(), 1);
        assert!(rig.session.snapshot_init().pending.contains_key(&req));
    }

    #[test]
    fn a_cancel_mid_flight_cannot_be_overtaken_by_an_in_flight_success() {
        let mut rig = rig(stored(false));
        let mut d = Dispatcher::<AppHost>::new();
        let mut records = waiting(&mut rig, &mut d);
        let authorized = records.remove(0);
        step(&mut rig, &mut d, vec![authorized]);
        assert_eq!(rig.auth_read().0.phase, Phase::Discovering);
        let old_epoch = rig.auth_read().0.flow_epoch;
        command(&mut rig, &mut d, back());
        assert_eq!(rig.session_adapter.fixture_resources().back_results, [true]);
        assert_eq!(rig.auth_read().0.phase, Phase::Ready);
        assert_eq!(rig.auth_read().0.flow_epoch, old_epoch + 1);
        assert!(rig.session.snapshot_init().apply_pending);
        assert!(rig.session.snapshot_init().pending.is_empty());
        let before = rig.session.subhash();
        let resources = resource_state(&mut rig);
        step(&mut rig, &mut d, records.clone());
        assert_eq!(rig.session.subhash(), before);
        assert_eq!(rig.auth_read().0.phase, Phase::Ready);
        assert_eq!(rig.session.snapshot_init().persisted.server.machine_id, "stored-server");
        assert_eq!(rig.session.snapshot_init().persisted.account_token, "synthetic-account");
        assert_eq!(resource_state(&mut rig), resources);
        assert!(records.iter().all(|record| !rig.session_adapter.admitted(record)));
        command(&mut rig, &mut d, SessionCmd::TakeReady);
        assert!(rig.take_session_ready().is_some());
        assert!(rig.take_session_ready().is_none());
        assert_eq!(rig.session_adapter.fixture_resources().disk.server.machine_id, "stored-server");
    }

    // Endpoint lifetimes: completion guards own physical reservations, the Session owner owns
    // logical pending entries, and transferred envelopes retain receipts until main ACKs them.
    fn endpoint_rig() -> (Bridge, crate::plex::ServerId) {
        let mut saved = stored(false);
        saved.sources.push(crate::plex::session::SourceRef {
            machine_id: "stored-server".into(), address: "127.0.0.1".into(), port: 32400,
            origin_url: "http://127.0.0.1:32400".into(), token: "synthetic-server-token".into(),
            owned: true, ..Default::default()
        });
        let mut rig = rig(saved);
        // Legacy admission used 32 slots; the real registry bounds requests to MAX_SERVERS (16).
        let sid = crate::plex::ServerId::from_raw(15);
        rig.session_adapter.fixture_resources().endpoints.insert(
            sid.raw(),
            crate::auth::owner::EndpointCapture {
                lifecycle: crate::auth::owner::ServerLifecycle {
                    sid: sid.raw(), instance_gen: 17, token_gen: 23,
                },
                machine_id: "stored-server".into(),
            },
        );
        (rig, sid)
    }

    fn endpoint_drop(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, sid: crate::plex::ServerId) -> u32 {
        let req = rig.session.snapshot_init().next_req.checked_add(1).unwrap();
        rig.session_adapter.inject_fixture_work(req, move |output, input| {
            let SessionWork::Endpoint { lifecycle, machine_id, .. } = input else {
                panic!("endpoint request must launch endpoint work");
            };
            assert_eq!(lifecycle.sid, sid.raw());
            assert_eq!(machine_id, "stored-server");
            assert!(!output.cancelled());
            // No invented result and no manual release: returning without a terminal exercises
            // the actual launch_correlated completion guard, which records Lane::Dropped.
            drop(output);
        });
        command(rig, d, SessionCmd::RequestEndpoint { sid });
        let state = rig.session.snapshot_init();
        assert_eq!(state.pending[&req].key.epoch, state.epoch);
        assert!(state.epoch > u64::from(u32::MAX));
        assert!(state.pending[&req].key.op == SessionOp::Endpoint(sid.raw()));
        req
    }

    fn endpoint_terminal(rig: &mut Bridge, req: u32, epoch: u64) -> SessionEnvelope {
        let mut records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 1);
        let record = records.pop().unwrap();
        assert_eq!(record.addr, crate::ui::machine::Addr {
            to: MachineId::Session, req: crate::ui::machine::RequestId(req),
        });
        assert_eq!(record.key.epoch, epoch);
        assert!(record.key.op == SessionOp::Endpoint(15));
        assert!(record.terminal);
        assert!(matches!(record.outcome, crate::auth::owner::SessionArrival::Dropped));
        assert!(rig.session_adapter.admitted(&record));
        record
    }

    fn endpoint_still_pending(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, sid: crate::plex::ServerId, req: u32) {
        let before = rig.session.subhash();
        let next = rig.session.snapshot_init().next_req;
        command(rig, d, SessionCmd::RequestEndpoint { sid });
        assert_eq!(rig.session.subhash(), before, "same SID cannot readmit before main retirement");
        assert_eq!(rig.session.snapshot_init().next_req, next);
        assert!(rig.session.snapshot_init().pending.contains_key(&req));
    }

    #[test]
    fn endpoint_terminal_release_is_main_applied_and_flight_matched() {
        let (mut rig, sid) = endpoint_rig();
        let mut d = Dispatcher::<AppHost>::new();
        let resources = resource_state(&mut rig);
        let first = endpoint_drop(&mut rig, &mut d, sid);
        // The worker guard has completed physically, but main has not applied its terminal.
        endpoint_still_pending(&mut rig, &mut d, sid, first);
        let old = endpoint_terminal(&mut rig, first, INITIAL_EPOCH);
        endpoint_still_pending(&mut rig, &mut d, sid, first);
        step(&mut rig, &mut d, vec![old.clone()]);
        assert!(!rig.session.snapshot_init().pending.contains_key(&first));
        assert!(!rig.session_adapter.admitted(&old), "main application ACKs the transfer receipt");

        let second = endpoint_drop(&mut rig, &mut d, sid);
        assert_ne!(first, second);
        let successor = endpoint_terminal(&mut rig, second, INITIAL_EPOCH);
        assert_ne!(old.addr, successor.addr);
        assert_ne!(old.arrival, successor.arrival);
        let before = rig.session.subhash();
        step(&mut rig, &mut d, vec![old]);
        assert_eq!(rig.session.subhash(), before, "old-flight duplicate cannot retire successor");
        assert!(rig.session_adapter.admitted(&successor));
        endpoint_still_pending(&mut rig, &mut d, sid, second);
        step(&mut rig, &mut d, vec![successor.clone()]);
        assert!(!rig.session_adapter.admitted(&successor));
        assert!(rig.session.snapshot_init().pending.is_empty());
        assert_eq!(resource_state(&mut rig), resources);
    }

    #[test]
    fn endpoint_unwind_and_cancel_still_queue_a_main_thread_release() {
        // Real unwind of the completion guard is mapped separately to the existing adapter test
        // running_worker_unwind_uses_the_guard_terminal. Injection runs synchronously inside the
        // Bridge effect drain: catching a panic inside this producer would NOT unwind that guard.
        // This trace covers the approved logical-cancel / carried-receipt half using real Drop.
        let (mut rig, sid) = endpoint_rig();
        let mut d = Dispatcher::<AppHost>::new();
        let resources = resource_state(&mut rig);
        let first = endpoint_drop(&mut rig, &mut d, sid);
        let old = endpoint_terminal(&mut rig, first, INITIAL_EPOCH);
        endpoint_still_pending(&mut rig, &mut d, sid, first);
        let before = rig.session.subhash();
        execute_session_command(&mut d, back());
        assert_eq!(rig.session.subhash(), before, "queued cancellation is not yet main-applied");
        assert!(rig.session.snapshot_init().pending.contains_key(&first));
        assert!(rig.session_adapter.admitted(&old));
        step(&mut rig, &mut d, Vec::new());
        assert_eq!(rig.session_adapter.fixture_resources().back_results, [true]);
        assert_eq!(rig.auth_read().0.flow_epoch, INITIAL_EPOCH + 1);
        assert!(!rig.session.snapshot_init().pending.contains_key(&first));
        assert!(rig.session_adapter.admitted(&old), "logical cancellation must not ACK carried data");

        let second = endpoint_drop(&mut rig, &mut d, sid);
        assert_ne!(first, second);
        let before = rig.session.subhash();
        assert!(rig.session_adapter.take_results().is_empty(), "old transferred batch still owns its credit");
        step(&mut rig, &mut d, vec![old.clone()]);
        assert!(!rig.session_adapter.admitted(&old));
        assert_eq!(rig.session.subhash(), before, "stale receipt ACK cannot alter successor's logical state");
        let successor = endpoint_terminal(&mut rig, second, INITIAL_EPOCH + 1);
        assert_ne!(old.addr, successor.addr);
        assert_ne!(old.key.epoch, successor.key.epoch);
        step(&mut rig, &mut d, vec![old]);
        assert_eq!(rig.session.subhash(), before);
        assert!(rig.session_adapter.admitted(&successor), "duplicate old ACK cannot release successor credit");
        endpoint_still_pending(&mut rig, &mut d, sid, second);
        step(&mut rig, &mut d, vec![successor.clone()]);
        assert!(!rig.session_adapter.admitted(&successor));
        assert!(rig.session.snapshot_init().pending.is_empty());
        assert_eq!(resource_state(&mut rig), resources);
    }

    // Active producer coverage is intentionally adapter-level, separate from the Bridge's logical
    // pending/receipt tests. No network is performed by the real host thread below.
    mod active_worker_reservation {
        use crate::app::adapters::session::SessionAdapter;
        use crate::auth::owner::{SessionOp, SessionWorkKey, SESSION_TOTAL_RESERVATIONS};
        use crate::ui::landing::AdmissionError;
        use crate::ui::machine::RequestId;
        use std::sync::mpsc::{sync_channel, SyncSender};
        use std::time::Duration;

        const EPOCH: u64 = u32::MAX as u64 + 80;
        const WAIT: Duration = Duration::from_secs(10);

        fn key() -> SessionWorkKey {
            SessionWorkKey { epoch: EPOCH, op: SessionOp::Login }
        }

        struct Jobs {
            adapter: SessionAdapter,
            observe: SyncSender<()>,
            finish: SyncSender<()>,
            running: Option<std::thread::JoinHandle<()>>,
            delayed: Vec<Box<dyn FnOnce() + Send>>,
        }

        impl Jobs {
            fn defer(&mut self, req: u32) -> Result<(), AdmissionError> {
                let delayed = &mut self.delayed;
                self.adapter.launch(RequestId(req), key(), true,
                    |job| { delayed.push(job); true },
                    |_| panic!("delayed filler must be cancelled before its body runs"))
            }

            fn drain_delayed(&mut self) {
                self.adapter.cancel_all();
                for job in self.delayed.drain(..) { job(); }
            }
        }

        impl Drop for Jobs {
            fn drop(&mut self) {
                // Cancellation precedes both wakeups even when a main-thread assertion failed.
                // Capacity-one channels never block cleanup; worker receives are also bounded.
                self.adapter.cancel_all();
                let _ = self.observe.try_send(());
                let _ = self.finish.try_send(());
                if let Some(worker) = self.running.take() { let _ = worker.join(); }
                for job in self.delayed.drain(..) {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                }
            }
        }

        fn prove_active_cancel(unwind: bool) {
            let (entered_tx, entered) = sync_channel(1);
            let (observe, observe_rx) = sync_channel(1);
            let (observed_tx, observed) = sync_channel(1);
            let (finish, finish_rx) = sync_channel(1);
            let main_thread = std::thread::current().id();
            let mut jobs = Jobs { adapter: SessionAdapter::fixture(), observe, finish,
                running: None, delayed: Vec::new() };
            let running = &mut jobs.running;
            jobs.adapter.launch(RequestId(1), key(), true,
                |job| { *running = Some(std::thread::spawn(job)); true },
                move |output| {
                    assert_ne!(std::thread::current().id(), main_thread);
                    assert!(!output.cancelled());
                    entered_tx.send(()).unwrap(); // actual body entered, guard is already on stack
                    observe_rx.recv_timeout(WAIT).expect("main must cancel then release observation");
                    assert!(output.cancelled());
                    let progress = crate::auth::LoginProgress::CodeReady {
                        epoch: EPOCH, code: "ABCD".into(), qr_png: Vec::new(),
                    }.into();
                    assert!(!crate::auth::owner::ObservationSink::progress(&output, progress),
                        "the production worker sink must refuse late progress");
                    let terminal = crate::auth::LoginProgress::Failed {
                        epoch: EPOCH, message: "Synthetic late failure".into(),
                    }.into();
                    assert!(!crate::auth::owner::ObservationSink::terminal(&output, terminal),
                        "the production worker sink must refuse a late terminal");
                    observed_tx.send(()).unwrap();
                    finish_rx.recv_timeout(WAIT).expect("main must finish its capacity assertions");
                    if unwind { panic!("synthetic active producer unwind"); }
                    // Normal return or real stack unwind exits the launch closure's completion guard.
                }).unwrap();
            entered.recv_timeout(WAIT).expect("worker must ENTER before cancellation");
            for req in 2..=SESSION_TOTAL_RESERVATIONS { jobs.defer(req).unwrap(); }
            assert!(matches!(jobs.defer(SESSION_TOTAL_RESERVATIONS + 1), Err(AdmissionError::Capacity)));
            jobs.adapter.cancel(RequestId(1));
            assert!(matches!(jobs.defer(SESSION_TOTAL_RESERVATIONS + 2), Err(AdmissionError::Capacity)),
                "main cancellation cannot free an actually running producer's reservation");
            jobs.observe.send(()).unwrap();
            observed.recv_timeout(WAIT).expect("active worker must observe cancellation and refuse late writes");
            assert!(jobs.adapter.take_results().is_empty());
            assert!(matches!(jobs.defer(SESSION_TOTAL_RESERVATIONS + 3), Err(AdmissionError::Capacity)),
                "observing cancellation/refusing output is not completion of the held body");
            jobs.finish.send(()).unwrap();
            let outcome = jobs.running.take().unwrap().join();
            if unwind {
                let payload = outcome.expect_err("actual active body must unwind through its guard");
                assert_eq!(payload.downcast_ref::<&str>(), Some(&"synthetic active producer unwind"));
            } else {
                outcome.expect("active body must return normally");
            }
            // Guard completion queues a terminal, still bounded until the ordinary main drain.
            assert!(matches!(jobs.defer(SESSION_TOTAL_RESERVATIONS + 4), Err(AdmissionError::Capacity)));
            assert!(jobs.adapter.take_results().is_empty(), "drain cancelled guard terminal, not payload");
            jobs.defer(SESSION_TOTAL_RESERVATIONS + 5).expect("completed guard plus main drain must release one slot");
            assert!(matches!(jobs.defer(SESSION_TOTAL_RESERVATIONS + 6), Err(AdmissionError::Capacity)),
                "exactly the completed producer's slot became available");
            jobs.drain_delayed();
            assert!(jobs.adapter.take_results().is_empty(), "cancelled workers must not leak records");
        }

        #[test]
        fn active_worker_cancel_holds_capacity_until_normal_guard_completion() {
            prove_active_cancel(false);
        }

        #[test]
        fn active_worker_cancel_holds_capacity_until_unwind_guard_completion() {
            prove_active_cancel(true);
        }
    }

    mod generation_regressions {
        use super::*;

        fn inject_activation(rig: &mut Bridge, req: u32, epoch: u64) {
            let captured_client = rig.session.snapshot_init().persisted.client_id.clone();
            rig.session_adapter.inject_fixture_work(req, move |output, input| {
                let SessionWork::Login { client_id } = input else { panic!("expected captured login work") };
                assert_eq!(client_id, captured_client);
                assert!(!output.cancelled());
                output.progress(LoginProgress::CodeReady {
                    epoch, code: "ABCD".into(), qr_png: vec![1, 2, 3],
                }.into()).unwrap();
                output.progress(LoginProgress::Authorized {
                    epoch, token: "synthetic-authorized-account".into(),
                }.into()).unwrap();
                let activation = serde_json::from_value(serde_json::json!({"Activate": {
                    "epoch":epoch, "expected":null,
                    "candidate":{"machine_id":"generation-server", "token":"synthetic-token",
                        "name":"Generation", "credit":"", "owned":true,
                        "origin":"https://192-0-2-10.h.plex.direct:32400", "address":"192.0.2.10",
                        "location":crate::plex::probe::Location::Local, "ipv6":false}
                }})).unwrap();
                output.progress(crate::auth::AuthProgress::Registry(activation)).unwrap();
                // Synthetic valid activation-contract observation, then real producer Drop.
                // No claim about a completed network discovery or successful sign-in policy.
            });
        }

        fn transferred(rig: &mut Bridge, req: u32, epoch: u64) -> Vec<SessionEnvelope> {
            let records = rig.session_adapter.take_results();
            assert_eq!(records.len(), 4);
            for record in &records {
                assert_eq!(record.addr, crate::ui::machine::Addr {
                    to: MachineId::Session, req: crate::ui::machine::RequestId(req),
                });
                assert_eq!(record.key.epoch, epoch);
                assert!(epoch > u64::from(u32::MAX));
                assert!(record.key.op == SessionOp::Login);
                assert!(rig.session_adapter.admitted(record));
            }
            assert!(records[..3].iter().all(|record| !record.terminal) && records[3].terminal);
            assert!(matches!(records[3].outcome, crate::auth::owner::SessionArrival::Dropped));
            records
        }

        fn start_wait(rig: &mut Bridge, d: &mut Dispatcher<AppHost>) -> Vec<SessionEnvelope> {
            let req = rig.session.snapshot_init().next_req + 1;
            let epoch = rig.auth_read().0.flow_epoch + 1;
            inject_activation(rig, req, epoch);
            command(rig, d, SessionCmd::StartLogin);
            let mut records = transferred(rig, req, epoch);
            let code = records.remove(0);
            step(rig, d, vec![code]);
            assert_eq!(rig.auth_read().0.phase, Phase::Waiting);
            assert!(rig.session_adapter.fixture_resources().registry_writes.is_empty());
            records
        }

        fn apply_fresh(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, req: u32, epoch: u64) {
            let mut fresh = transferred(rig, req, epoch);
            let terminal = fresh.pop().unwrap();
            step(rig, d, fresh.clone());
            assert_eq!(rig.auth_read().0.phase, Phase::Discovering, "fresh authorization precedes activation");
            let resources = rig.session_adapter.fixture_resources();
            let [crate::auth::owner::RegistryPlan::Activate { source, .. }] = resources.registry_writes.as_slice()
                else { panic!("fresh activation must execute exactly one registry effect") };
            assert_eq!(source.machine_id, "generation-server");
            assert_eq!(source.origin_url, "https://192-0-2-10.h.plex.direct:32400");
            assert!(fresh.iter().all(|record| !rig.session_adapter.admitted(record)));
            step(rig, d, vec![terminal.clone()]);
            assert!(!rig.session_adapter.admitted(&terminal));
            assert!(rig.session.snapshot_init().pending.is_empty());
            assert!(rig.session.snapshot_init().pending_commit.is_none());
        }

        #[test]
        fn invalidating_an_epoch_while_activation_waits_prevents_stale_publication() {
            let mut rig = rig(stored(false));
            let mut d = Dispatcher::<AppHost>::new();
            let stale = start_wait(&mut rig, &mut d);
            let old_epoch = rig.auth_read().0.flow_epoch;
            let req = rig.session.snapshot_init().next_req + 1;
            let before_queue = rig.session.subhash();
            inject_activation(&mut rig, req, old_epoch + 1);
            execute_session_command(&mut d, SessionCmd::StartLogin);
            assert_eq!(rig.session.subhash(), before_queue, "queued retirement is not yet applied");
            assert!(stale.iter().all(|record| rig.session_adapter.admitted(record)));
            step(&mut rig, &mut d, Vec::new());
            assert_eq!(rig.auth_read().0.flow_epoch, old_epoch + 1);
            assert!(!rig.session.snapshot_init().pending.contains_key(&stale[0].addr.req.0));
            let before_stale = rig.session.subhash();
            let resources = resource_state(&mut rig);
            step(&mut rig, &mut d, stale.clone());
            assert_eq!(rig.session.subhash(), before_stale);
            assert_eq!(resource_state(&mut rig), resources, "old activation cannot execute resource effects");
            assert!(stale.iter().all(|record| !rig.session_adapter.admitted(record)));
            assert!(rig.session.snapshot_init().pending.contains_key(&req));
            // Same candidate shape on a genuinely current admitted request succeeds, so the stale
            // rejection is not explained by malformed activation data or a dead fixture effect sink.
            apply_fresh(&mut rig, &mut d, req, old_epoch + 1);
        }

        #[test]
        fn beginning_a_new_flow_invalidates_the_old_epoch_at_the_same_capture_boundary() {
            // The old arbitrary callback returned Switching while setting Profiles. Production has
            // no such callback: RestartWait guards CURRENT Waiting+QR and captures new Login inputs
            // at its Waiting -> Creating transition. Test that meaningful boundary, not invented API.
            let mut rig = rig(stored(false));
            let mut d = Dispatcher::<AppHost>::new();
            let stale = start_wait(&mut rig, &mut d);
            let read = rig.session.publication();
            let old_epoch = rig.auth_read().0.flow_epoch;
            let generation = rig.auth_read().0.qr_generation;
            let before = rig.session.subhash();
            for (phase, qr_generation) in [(Phase::Profiles, generation), (Phase::Waiting, generation + 1)] {
                command(&mut rig, &mut d, SessionCmd::RestartWait { phase, qr_generation,
                    reply: ReplyTo { instance: 91, correlation: 1 } });
                assert_eq!(rig.session.subhash(), before, "phase/code guard refuses before any epoch advance");
                assert!(stale.iter().all(|record| rig.session_adapter.admitted(record)));
            }
            let req = rig.session.snapshot_init().next_req + 1;
            inject_activation(&mut rig, req, old_epoch + 1);
            execute_session_command(&mut d, SessionCmd::RestartWait {
                phase: Phase::Waiting, qr_generation: generation,
                reply: ReplyTo { instance: 91, correlation: 2 },
            });
            assert_eq!(rig.session.subhash(), before);
            step(&mut rig, &mut d, Vec::new());
            let current = rig.session.snapshot_init();
            assert_eq!(rig.auth_read().0.phase, Phase::Creating);
            assert_eq!(current.epoch, old_epoch + 1, "exactly one epoch increment");
            assert_eq!(current.next_req, req, "exactly one new request capture");
            assert_eq!(current.pending.len(), 1);
            assert_eq!(current.pending[&req].key.epoch, old_epoch + 1);
            assert!(current.pending[&req].key.op == SessionOp::Login);
            assert_eq!(read.flow_epoch, old_epoch);
            assert_eq!(read.phase, Phase::Waiting, "retained read cannot acquire the new epoch/state");
            let after = rig.session.subhash();
            step(&mut rig, &mut d, stale.clone());
            assert_eq!(rig.session.subhash(), after, "old epoch is no longer eligible");
            assert!(rig.session_adapter.fixture_resources().registry_writes.is_empty());
            assert!(stale.iter().all(|record| !rig.session_adapter.admitted(record)));
            apply_fresh(&mut rig, &mut d, req, old_epoch + 1);
        }
    }
}
