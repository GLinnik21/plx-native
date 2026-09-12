//! Core endpoint worker-policy ports. Real worker body; explicit IO and actual resource guards.
#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::auth::owner::{SessionEvent, SessionWork};
    use crate::plex::session::{self, Session, SourceRef, ServerRef, UserRef};

    struct Cleanup<'a>(&'a crate::task::MainThread);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            crate::plex::reset_servers_for_test();
            session::ProfilePublisher::new(self.0).publish(None, 0);
        }
    }

    fn frame(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, records: Vec<crate::auth::owner::SessionEnvelope>) {
        let results = records.into_iter().map(|r| (r.addr, AppMsg::Session(SessionEvent::Result(r)))).collect();
        d.frame_with(rig, Tick::default(), Vec::new(), results, &mut NoTap, false);
    }

    #[test]
    fn endpoint_request_refusal_early_return_and_retoken_release_exact_admission() {
        let _lock = crate::testlock::serial();
        let mt = unsafe { crate::task::MainThread::assume() };
        for scenario in 0..4 {
            let tmp = session::TempSession::new("owner-endpoint-policy");
            let _cleanup = Cleanup(&mt);
            tmp.assert_only_target();
            crate::plex::reset_servers_for_test();
            let source = SourceRef { machine_id: "synthetic-server".into(), address: "127.0.0.1".into(),
                port: 32400, origin_url: "http://127.0.0.1:32400".into(), token: "synthetic-old-token".into(),
                owned: true, ..Default::default() };
            let saved = Session { client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
                user: UserRef { uuid: "synthetic-profile".into(), token: source.token.clone(), ..Default::default() },
                server: ServerRef { machine_id: source.machine_id.clone(), address: source.address.clone(),
                    port: source.port, origin_url: source.origin_url.clone(), token: source.token.clone(), ..Default::default() },
                sources: vec![source], ..Default::default() };
            session::save(&saved);
            let before = std::fs::read(tmp.path()).unwrap();
            let id = crate::plex::register_for_test("synthetic-server", "127.0.0.1", 32400,
                "synthetic-old-token", "synthetic-client");
            let client = crate::plex::client_for(id).unwrap();
            let mut init = crate::auth::SessionInit::captured(saved);
            init.epoch = u64::from(u32::MAX) + 81;
            let epoch = init.epoch;
            let mut rig = Bridge::for_session_test(init);
            rig.session_adapter = crate::app::adapters::session::SessionAdapter::live_resources_for_test(&mt, false);
            if scenario != 0 {
                rig.session_adapter.inject_fixture_work(1, move |output, input| {
                    std::thread::spawn(move || {
                        let SessionWork::Endpoint { session, expected, lifecycle, machine_id } = input
                            else { panic!("wrong worker family") };
                        let old_source = session.sources[0].clone();
                        crate::auth::endpoint_worker_with_io(epoch, session, expected, lifecycle, machine_id, &output,
                            |_| match scenario {
                                1 => None,
                                2 => Some(Vec::new()),
                                _ => Some(vec![serde_json::from_value(serde_json::json!({
                                    "clientIdentifier":"synthetic-server", "provides":"server"
                                })).unwrap()]),
                            },
                            |resource, _| {
                                assert_eq!(scenario, 3, "early resource return must not probe");
                                (Some(SourceRef { address: "127.0.0.9".into(),
                                    origin_url: "http://127.0.0.9:32400".into(), ..old_source }),
                                    crate::auth::settled_probe(&crate::plex::probe::plan(resource),
                                        crate::plex::probe::Outcome::Reachable, None))
                            });
                    }).join().expect("endpoint worker failed");
                });
            }
            let mut d = Dispatcher::<AppHost>::new();
            execute_session_command(&mut d, crate::auth::SessionCmd::RequestEndpoint { sid: id });
            frame(&mut rig, &mut d, Vec::new());
            assert!(rig.session.snapshot_init().pending.contains_key(&1));
            let records = rig.session_adapter.take_results();
            assert_eq!(records.len(), 1);
            assert!(records[0].terminal);
            if scenario == 0 { assert!(matches!(records[0].outcome, crate::auth::owner::SessionArrival::Refused)); }
            let before_owner = rig.session.subhash();
            execute_session_command(&mut d, crate::auth::SessionCmd::RequestEndpoint { sid: id });
            frame(&mut rig, &mut d, Vec::new());
            assert_eq!(rig.session.subhash(), before_owner, "same SID stays occupied until main applies result");
            if scenario == 3 {
                let generation = client.token_gen();
                client.set_token("synthetic-new-token");
                assert_ne!(client.token_gen(), generation);
                assert!(std::ptr::eq(client, crate::plex::client_for(id).unwrap()));
            }
            frame(&mut rig, &mut d, records);
            assert!(!rig.session.snapshot_init().pending.contains_key(&1));
            assert_eq!(client.host(), "127.0.0.1");
            assert_eq!(session::peek().sources[0].address, "127.0.0.1");
            assert_eq!(std::fs::read(tmp.path()).unwrap(), before);
            execute_session_command(&mut d, crate::auth::SessionCmd::RequestEndpoint { sid: id });
            frame(&mut rig, &mut d, Vec::new());
            assert!(rig.session.snapshot_init().pending.contains_key(&2), "matching retirement permits successor");
            let terminal = rig.session_adapter.take_results();
            frame(&mut rig, &mut d, terminal);
        }
        // Actual guard unwind/cancel is retained in adapter tests and active_worker_reservation;
        // this joined IO thread is not claimed to unwind the outer synchronous injection guard.
    }
}
