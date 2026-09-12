//! Serial resource-path evidence: real session file and native registry, not fixture mirrors.
//! Account/probe transport and auxiliary telemetry/cache/runtime sweeps remain injected.
#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::auth::owner::{SessionEnvelope, SessionEvent, SessionWork};
    use crate::auth::{Phase, SessionCmd};
    use crate::plex::session::{self, ProfileCreds, ServerRef, Session, SourceRef, UserRef};

    struct ResourceCleanup<'a>(&'a crate::task::MainThread);
    impl Drop for ResourceCleanup<'_> {
        fn drop(&mut self) {
            crate::plex::reset_servers_for_test();
            // Publish the neutral test resource value, without allocating another profile scope.
            session::ProfilePublisher::new(self.0).publish(None, 0);
        }
    }

    struct Offline;

    struct Online {
        probes: usize,
    }
    impl crate::auth::ProfileWorkIo for Online {
        fn switch(
            &mut self,
            _: &crate::plex::account::AccountClient,
            uuid: &str,
            _: Option<&str>,
        ) -> crate::plex::account::SwitchOutcome {
            assert_eq!(uuid, "kid");
            crate::plex::account::SwitchOutcome::Switched(crate::plex::account::SwitchedUser {
                uuid: uuid.into(),
                title: "Kid".into(),
                auth_token: "synthetic-switched-account".into(),
                ..Default::default()
            })
        }
        fn resources(
            &mut self,
            _: &crate::plex::account::AccountClient,
        ) -> Option<Vec<crate::plex::account::Resource>> {
            Some(vec![
                serde_json::from_value(serde_json::json!({"clientIdentifier":"resource-server",
                    "name":"Synthetic", "provides":"server", "owned":true,
                    "accessToken":"synthetic-kid-token"}))
                .unwrap(),
                serde_json::from_value(serde_json::json!({"clientIdentifier":"resource-share",
                    "name":"Synthetic share", "provides":"server", "owned":false,
                    "accessToken":"synthetic-share-token"}))
                .unwrap(),
            ])
        }
        fn probe(
            &mut self,
            resource: &crate::plex::account::Resource,
            _: &[i64],
        ) -> (Option<SourceRef>, crate::auth::SettledProbe) {
            assert_eq!(
                resource.client_identifier,
                if self.probes == 0 {
                    "resource-server"
                } else {
                    "resource-share"
                }
            );
            assert!(self.probes < 2);
            self.probes += 1;
            let address = if resource.owned {
                "127.0.0.2"
            } else {
                "127.0.0.3"
            };
            (
                Some(SourceRef {
                    machine_id: resource.client_identifier.clone(),
                    name: resource.name.clone(),
                    token: resource.access_token.clone(),
                    owned: resource.owned,
                    address: address.into(),
                    port: 32400,
                    origin_url: format!("http://{address}:32400"),
                    ..Default::default()
                }),
                crate::auth::settled_probe(
                    &crate::plex::probe::plan(resource),
                    crate::plex::probe::Outcome::Reachable,
                    Some(crate::plex::probe::Location::Local),
                ),
            )
        }
        fn gap(&mut self) {
            assert_eq!(self.probes, 1);
        }
    }

    #[test]
    fn held_online_roster_uses_real_disk_registry_and_cannot_resurrect_after_erase() {
        let _lock = crate::testlock::serial();
        let mt = unsafe { crate::task::MainThread::assume() };
        for erase_before_roster in [false, true] {
            let tmp = session::TempSession::new("owner-native-held-roster");
            let _cleanup = ResourceCleanup(&mt);
            tmp.assert_only_target();
            crate::plex::reset_servers_for_test();
            let saved = stored().with_auto_sign_in(true);
            session::save(&saved);
            let before = std::fs::read(tmp.path()).unwrap();
            crate::plex::register_for_test(
                "resource-server",
                "127.0.0.1",
                32400,
                "synthetic-admin-token",
                "synthetic-resource-client",
            );
            let mut init = crate::auth::SessionInit::captured(saved);
            init.phase = Phase::Profiles;
            init.epoch = u64::from(u32::MAX) + 80;
            init.users = vec![crate::auth::UserTile {
                uuid: "kid".into(),
                title: "Kid".into(),
                ..Default::default()
            }];
            let epoch = init.epoch + 1;
            let mut rig = Bridge::for_session_test(init);
            rig.session_adapter =
                crate::app::adapters::session::SessionAdapter::live_resources_for_test(&mt, false);
            rig.session_adapter
                .inject_fixture_work(1, move |output, input| {
                    std::thread::spawn(move || {
                        let SessionWork::ProfileSwitch {
                            session,
                            tile,
                            pin,
                            recently_unreachable,
                            ..
                        } = input
                        else {
                            panic!("wrong worker family")
                        };
                        let expected = crate::auth::SessionIdentity::of(&session);
                        crate::auth::profile_switch_worker_with_io(
                            epoch,
                            expected,
                            session,
                            tile,
                            pin,
                            recently_unreachable,
                            &output,
                            &mut Online { probes: 0 },
                        );
                    })
                    .join()
                    .expect("online policy worker panicked");
                });
            let mut d = Dispatcher::<AppHost>::new();
            command(
                &mut rig,
                &mut d,
                SessionCmd::SelectProfile {
                    index: 0,
                    pin: None,
                },
            );
            let mut records = rig.session_adapter.take_results();
            assert_eq!(records.len(), 2);
            assert!(!records[0].terminal && records[1].terminal);
            assert_eq!(records[0].addr, records[1].addr);
            assert_eq!(std::fs::read(tmp.path()).unwrap(), before);
            assert!(session::set_auto_sign_in(false)); // after online worker captured true
            let before = std::fs::read(tmp.path()).unwrap();
            let late = records.pop().unwrap();
            frame(&mut rig, &mut d, records);
            assert_eq!(rig.auth_read().0.phase, Phase::Ready);
            assert_eq!(std::fs::read(tmp.path()).unwrap(), before);
            command(&mut rig, &mut d, SessionCmd::TakeReady);
            assert!(rig.take_session_ready().is_some());
            assert!(rig.take_session_ready().is_none());
            assert_eq!(session::peek().user.uuid, "kid");
            assert_eq!(session::peek().server.address, "127.0.0.2");
            assert!(
                !session::peek().auto_sign_in(),
                "TakeReady preserves the newer opt-out"
            );
            assert!(
                rig.session_adapter.admitted(&late),
                "held terminal retains its transfer credit"
            );
            if erase_before_roster {
                tmp.assert_only_target();
                command(&mut rig, &mut d, SessionCmd::EraseLocal);
                assert!(!tmp.path().exists());
                assert_eq!(crate::plex::server_ids().count(), 0);
            } else {
                session::update(|disk| {
                    let mut next = disk.clone();
                    next.recent_searches = vec![session::RecentSearches {
                        user: "kid".into(),
                        terms: vec!["preference after handoff".into()],
                    }];
                    Some(next)
                });
            }
            frame(&mut rig, &mut d, vec![late.clone()]);
            assert!(!rig.session_adapter.admitted(&late));
            if erase_before_roster {
                assert_eq!(rig.auth_read().0.phase, Phase::Deleted);
                assert!(
                    !tmp.path().exists(),
                    "late roster cannot recreate the real session file"
                );
                assert_eq!(crate::plex::server_ids().count(), 0);
                assert!(session::peek().account_token.is_empty());
            } else {
                let disk = session::peek();
                assert!(
                    !disk.auto_sign_in(),
                    "late roster preserves opt-out against captured opt-in"
                );
                assert_eq!(disk.recent_searches[0].terms, ["preference after handoff"]);
                assert!(disk
                    .sources
                    .iter()
                    .any(|s| s.machine_id == "resource-share" && s.address == "127.0.0.3"));
                assert!(
                    crate::plex::server_ids().any(|id| crate::plex::client_for(id)
                        .is_some_and(|client| client.machine_id() == "resource-share"))
                );
            }
            assert!(
                rig.take_session_ready().is_none(),
                "roster never emits another handoff"
            );
        }
    }

    impl crate::auth::ProfileWorkIo for Offline {
        fn switch(
            &mut self,
            _: &crate::plex::account::AccountClient,
            _: &str,
            _: Option<&str>,
        ) -> crate::plex::account::SwitchOutcome {
            crate::plex::account::SwitchOutcome::Unreachable
        }
        fn resources(
            &mut self,
            _: &crate::plex::account::AccountClient,
        ) -> Option<Vec<crate::plex::account::Resource>> {
            panic!("offline worker fetched resources")
        }
        fn probe(
            &mut self,
            _: &crate::plex::account::Resource,
            _: &[i64],
        ) -> (Option<SourceRef>, crate::auth::SettledProbe) {
            panic!("offline worker probed")
        }
        fn gap(&mut self) {
            panic!("offline worker waited between probes")
        }
    }

    fn stored() -> Session {
        let source = SourceRef {
            machine_id: "resource-server".into(),
            name: "Synthetic".into(),
            address: "127.0.0.1".into(),
            port: 32400,
            origin_url: "http://127.0.0.1:32400".into(),
            token: "synthetic-kid-token".into(),
            owned: true,
            ..Default::default()
        };
        let server = ServerRef {
            machine_id: source.machine_id.clone(),
            name: source.name.clone(),
            address: source.address.clone(),
            port: source.port,
            origin_url: source.origin_url.clone(),
            token: source.token.clone(),
            ..Default::default()
        };
        let kid = UserRef {
            uuid: "kid".into(),
            title: "Kid".into(),
            token: source.token.clone(),
            ..Default::default()
        };
        Session {
            client_id: "synthetic-resource-client".into(),
            account_token: "synthetic-account".into(),
            user: UserRef {
                uuid: "admin".into(),
                title: "Admin".into(),
                token: "synthetic-admin-token".into(),
                ..Default::default()
            },
            server: ServerRef {
                token: "synthetic-admin-token".into(),
                ..server.clone()
            },
            sources: vec![SourceRef {
                token: "synthetic-admin-token".into(),
                ..source.clone()
            }],
            profiles: vec![ProfileCreds {
                uuid: kid.uuid.clone(),
                user: kid,
                server,
                sources: vec![source],
                pin: None,
            }],
            ..Default::default()
        }
    }

    fn frame(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, records: Vec<SessionEnvelope>) {
        let results = records
            .into_iter()
            .map(|r| (r.addr, AppMsg::Session(SessionEvent::Result(r))))
            .collect();
        d.frame_with(rig, Tick::default(), Vec::new(), results, &mut NoTap, false);
    }

    fn command(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, cmd: SessionCmd) {
        execute_session_command(d, cmd);
        frame(rig, d, Vec::new());
    }

    #[test]
    fn admitted_failure_payload_preserves_arbitrary_error_and_denial() {
        // Observation contract, deliberately NOT a claim about the worker's failure policy.
        let mut init = crate::auth::SessionInit::captured(stored());
        init.phase = Phase::Profiles;
        init.users = vec![crate::auth::UserTile {
            uuid: "kid".into(),
            ..Default::default()
        }];
        let epoch = init.epoch + 1;
        let mut rig = Bridge::for_session_test(init);
        rig.session_adapter
            .inject_fixture_work(1, move |output, input| {
                let SessionWork::ProfileSwitch { session, .. } = input else {
                    panic!("wrong work")
                };
                let payload = serde_json::from_value(serde_json::json!({
                    "epoch": epoch, "expected": crate::auth::SessionIdentity::of(&session),
                    "outcome": { "Failed": { "error": "synthetic switch refusal", "pin_denied": true } }
                }))
                .expect("valid serialized observation contract");
                assert!(output
                    .complete(crate::auth::AuthProgress::ProfileSwitch(payload))
                    .is_ok());
            });
        let mut d = Dispatcher::<AppHost>::new();
        command(
            &mut rig,
            &mut d,
            SessionCmd::SelectProfile {
                index: 0,
                pin: None,
            },
        );
        let records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 1);
        assert_eq!(rig.auth_read().0.phase, Phase::Switching);
        assert!(rig.auth_read().0.error.is_empty());
        assert!(!rig.auth_read().0.pin_denied);
        frame(&mut rig, &mut d, records);
        assert_eq!(rig.auth_read().0.phase, Phase::Profiles);
        assert_eq!(&*rig.auth_read().0.error, "synthetic switch refusal");
        assert!(rig.auth_read().0.pin_denied);
    }

    #[test]
    fn real_offline_worker_preserves_native_client_and_file_until_owner_handoff_and_erase() {
        let _lock = crate::testlock::serial();
        let mt = unsafe { crate::task::MainThread::assume() };
        for erase_before_apply in [false, true] {
            let tmp = session::TempSession::new("owner-native-profile");
            let _cleanup = ResourceCleanup(&mt);
            tmp.assert_only_target();
            crate::plex::reset_servers_for_test();
            let saved = stored();
            session::save(&saved);
            let before = std::fs::read(tmp.path()).unwrap();
            let id = crate::plex::register_for_test(
                "resource-server",
                "127.0.0.1",
                32400,
                "synthetic-admin-token",
                "synthetic-resource-client",
            );
            let client = crate::plex::client_for(id).unwrap();
            let generation = client.token_gen();
            let mut init = crate::auth::SessionInit::captured(saved);
            init.phase = Phase::Profiles;
            init.epoch = u64::from(u32::MAX) + 40;
            init.users = vec![crate::auth::UserTile {
                uuid: "kid".into(),
                title: "Kid".into(),
                ..Default::default()
            }];
            let epoch = init.epoch + 1;
            let mut rig = Bridge::for_session_test(init);
            rig.session_adapter =
                crate::app::adapters::session::SessionAdapter::live_resources_for_test(&mt, false);
            rig.session_adapter
                .inject_fixture_work(1, move |output, input| {
                    std::thread::spawn(move || {
                        let SessionWork::ProfileSwitch {
                            session,
                            tile,
                            pin,
                            recently_unreachable,
                            ..
                        } = input
                        else {
                            panic!("wrong worker family")
                        };
                        let expected = crate::auth::SessionIdentity::of(&session);
                        crate::auth::profile_switch_worker_with_io(
                            epoch,
                            expected,
                            session,
                            tile,
                            pin,
                            recently_unreachable,
                            &output,
                            &mut Offline,
                        );
                    })
                    .join()
                    .expect("offline worker panicked");
                });
            let mut d = Dispatcher::<AppHost>::new();
            command(
                &mut rig,
                &mut d,
                SessionCmd::SelectProfile {
                    index: 0,
                    pin: None,
                },
            );
            let records = rig.session_adapter.take_results();
            assert_eq!(records.len(), 1);
            assert!(records[0].terminal);
            let stale = records.clone();
            assert_eq!(rig.auth_read().0.phase, Phase::Switching);
            assert_eq!(rig.session.snapshot_init().persisted.user.uuid, "admin");
            assert_eq!(client.token_gen(), generation);
            assert!(std::ptr::eq(client, crate::plex::client_for(id).unwrap()));
            assert_eq!(std::fs::read(tmp.path()).unwrap(), before);
            if erase_before_apply {
                tmp.assert_only_target();
                command(&mut rig, &mut d, SessionCmd::EraseLocal);
                assert!(!tmp.path().exists());
                assert_eq!(crate::plex::server_ids().count(), 0);
                frame(&mut rig, &mut d, records); // A transferred, never-applied worker completion.
                assert_eq!(rig.auth_read().0.phase, Phase::Deleted);
                assert!(!tmp.path().exists());
                assert_eq!(crate::plex::server_ids().count(), 0);
                assert!(rig.take_session_ready().is_none());
                continue;
            }
            assert!(session::set_auto_sign_in(true)); // after offline worker captured false
            let before = std::fs::read(tmp.path()).unwrap();
            frame(&mut rig, &mut d, records);
            assert_eq!(rig.auth_read().0.phase, Phase::Ready);
            assert_eq!(rig.session.snapshot_init().persisted.user.uuid, "kid");
            assert!(std::ptr::eq(client, crate::plex::client_for(id).unwrap()));
            assert_ne!(client.token_gen(), generation);
            assert_eq!(
                std::fs::read(tmp.path()).unwrap(),
                before,
                "Ready must not save before handoff"
            );
            command(&mut rig, &mut d, SessionCmd::TakeReady);
            assert!(rig.take_session_ready().is_some());
            assert!(rig.take_session_ready().is_none());
            assert_eq!(session::peek().user.uuid, "kid");
            assert!(
                session::peek().auto_sign_in(),
                "offline handoff preserves the newer preference"
            );
            // Two legitimate endpoint requests replace the old test's impossible second terminal
            // ProfileRoster. Each captures the CURRENT native lifecycle through the real adapter.
            for address in ["127.0.0.2", "127.0.0.3"] {
                let req = rig.session.snapshot_init().next_req + 1;
                let flow_epoch = rig.auth_read().0.flow_epoch;
                rig.session_adapter
                    .inject_fixture_work(req, move |output, input| {
                        let SessionWork::Endpoint {
                            expected,
                            lifecycle,
                            machine_id,
                            session,
                        } = input
                        else {
                            panic!("endpoint request launched another worker family")
                        };
                        let fresh = SourceRef {
                            address: address.into(),
                            origin_url: format!("http://{address}:32400"),
                            tier: Some(crate::plex::probe::Location::Local),
                            ..session.sources[0].clone()
                        };
                        assert!(output
                            .complete(crate::auth::endpoint_work_fact(
                                flow_epoch,
                                expected,
                                lifecycle,
                                machine_id,
                                Some(fresh)
                            ))
                            .is_ok());
                    });
                command(&mut rig, &mut d, SessionCmd::RequestEndpoint { sid: id });
                let endpoint = rig.session_adapter.take_results();
                assert_eq!(endpoint.len(), 1);
                assert_eq!(endpoint[0].addr.req.0, req);
                // A different resource consumer saves a preference AFTER the worker captured input.
                let auto_sign_in = address == "127.0.0.3";
                assert!(session::set_auto_sign_in(auto_sign_in));
                session::update(|disk| {
                    let mut next = disk.clone();
                    next.recent_searches = vec![session::RecentSearches {
                        user: "kid".into(),
                        terms: vec![format!("newer preference {address}")],
                    }];
                    Some(next)
                });
                frame(&mut rig, &mut d, endpoint);
                let disk = session::peek();
                assert_eq!(disk.server.address, address);
                assert_eq!(
                    disk.auto_sign_in(),
                    auto_sign_in,
                    "endpoint patch preserves latest opt-in/out"
                );
                assert_eq!(disk.sources[0].address, address);
                assert_eq!(
                    disk.recent_searches[0].terms,
                    [format!("newer preference {address}")]
                );
            }
            tmp.assert_only_target();
            command(&mut rig, &mut d, SessionCmd::EraseLocal);
            assert_eq!(rig.auth_read().0.phase, Phase::Deleted);
            assert!(!tmp.path().exists());
            assert_eq!(crate::plex::server_ids().count(), 0);
            frame(&mut rig, &mut d, stale);
            assert!(!tmp.path().exists());
            assert_eq!(crate::plex::server_ids().count(), 0);
            assert!(rig.take_session_ready().is_none());
        }
    }

    // These are live disk/registry proofs, not the private-disk mirror in session_protocol_tests.
    mod native_endpoint_regressions {
        use super::*;
        const EPOCH: u64 = u32::MAX as u64 + 120;

        fn live(saved: Session, mt: &crate::task::MainThread) -> Bridge {
            let mut init = crate::auth::SessionInit::captured(saved);
            init.epoch = EPOCH;
            let mut rig = Bridge::for_session_test(init);
            rig.session_adapter = crate::app::adapters::session::SessionAdapter::live_resources_for_test(mt, false);
            rig
        }

        fn assert_receipt(rig: &Bridge, record: &SessionEnvelope, req: u32) {
            assert_eq!(record.addr, crate::ui::machine::Addr {
                to: MachineId::Session, req: crate::ui::machine::RequestId(req),
            });
            assert_eq!(record.key.epoch, EPOCH);
            assert!(rig.session_adapter.admitted(record));
        }

        #[test]
        fn accepted_activation_preserves_https_and_resolve_pin() {
            let _lock = crate::testlock::serial();
            let mt = unsafe { crate::task::MainThread::assume() };
            let tmp = session::TempSession::new("native-owner-activation");
            let _cleanup = ResourceCleanup(&mt);
            tmp.assert_only_target();
            crate::plex::reset_servers_for_test();
            session::save(&stored());
            let before = std::fs::read(tmp.path()).unwrap();
            let origin = crate::plex::Origin::parse("https://192-0-2-10.h.plex.direct:32400").unwrap();
            let pin = crate::plex::ResolvePin::for_origin(&origin, "192.0.2.10").unwrap();
            let mut rig = live(stored(), &mt);
            let candidate_origin = origin.base();
            rig.session_adapter.inject_fixture_work(1, move |output, input| {
                let SessionWork::ServerRoster { session, .. } = input else { panic!("expected roster work") };
                let expected = crate::auth::SessionIdentity::of(&session);
                // Existing serialized observation seam; unlike the old unsolicited Registry event,
                // this nonterminal belongs to the actual RefreshRoster request and captured identity.
                let activation = serde_json::from_value(serde_json::json!({"Activate": {
                    "epoch": EPOCH, "expected": expected,
                    "candidate": {"machine_id":"tls-test", "token":"synthetic", "name":"Synthetic",
                        "credit":"", "owned":true, "origin":candidate_origin, "address":"192.0.2.10",
                        "location":crate::plex::probe::Location::Local, "ipv6":false}
                }})).unwrap();
                output.progress(crate::auth::AuthProgress::Registry(activation)).unwrap();
                // Synthetic activation-contract proof, not a successful network-roster policy.
                // Return without terminal: the actual launch completion guard emits Dropped.
            });
            let mut d = Dispatcher::<AppHost>::new();
            command(&mut rig, &mut d, SessionCmd::RefreshRoster);
            let mut records = rig.session_adapter.take_results();
            assert_eq!(records.len(), 2);
            for record in &records { assert_receipt(&rig, record, 1); }
            assert!(!records[0].terminal && records[1].terminal);
            assert!(matches!(records[1].outcome, crate::auth::owner::SessionArrival::Dropped));
            assert_eq!(crate::plex::server_ids().count(), 0, "worker has no registry write authority");
            assert_eq!(std::fs::read(tmp.path()).unwrap(), before);
            let terminal = records.pop().unwrap();
            let activation = records.pop().unwrap();
            frame(&mut rig, &mut d, vec![activation.clone()]);
            let ids: Vec<_> = crate::plex::server_ids().collect();
            assert_eq!(ids.len(), 1);
            let client = crate::plex::client_for(ids[0]).unwrap();
            assert_eq!(client.machine_id(), "tls-test");
            assert_eq!(client.origin(), &origin);
            assert_eq!(client.resolve_pin(), Some(&pin));
            assert!(!rig.session_adapter.admitted(&activation));
            assert!(rig.session.snapshot_init().pending.contains_key(&1));
            frame(&mut rig, &mut d, vec![terminal.clone()]);
            assert!(!rig.session_adapter.admitted(&terminal));
            assert!(rig.session.snapshot_init().pending.is_empty());
            assert!(rig.session.snapshot_init().pending_commit.is_none());
            assert_eq!(std::fs::read(tmp.path()).unwrap(), before,
                "candidate activation changes registry, not persisted credentials");
            tmp.assert_only_target();
        }

        #[test]
        fn endpoint_result_from_a_replaced_client_incarnation_cannot_overwrite_its_route() {
            let _lock = crate::testlock::serial();
            let mt = unsafe { crate::task::MainThread::assume() };
            let tmp = session::TempSession::new("native-owner-endpoint-incarnation");
            let _cleanup = ResourceCleanup(&mt);
            tmp.assert_only_target();
            crate::plex::reset_servers_for_test();
            let saved = stored();
            session::save(&saved);
            let original_disk = std::fs::read(tmp.path()).unwrap();
            let id = crate::plex::register_for_test("resource-server", "127.0.0.1", 32400,
                "synthetic-admin-token", "synthetic-resource-client");
            let old_client = crate::plex::client_for(id).unwrap();
            let old_instance = old_client.instance_gen();
            let old_token = old_client.token_gen();
            let mut rig = live(saved.clone(), &mt);
            rig.session_adapter.inject_fixture_work(1, move |output, input| {
                let SessionWork::Endpoint { session, expected, lifecycle, machine_id } = input
                    else { panic!("expected actual endpoint capture") };
                assert_eq!(lifecycle.sid, id.raw());
                assert_eq!(lifecycle.instance_gen, old_instance);
                assert_eq!(lifecycle.token_gen, old_token);
                let stale = SourceRef { address: "127.0.0.9".into(),
                    origin_url: "http://127.0.0.9:32400".into(),
                    token: "account-token-not-authoritative".into(), ..session.sources[0].clone() };
                output.complete(crate::auth::endpoint_work_fact(EPOCH, expected, lifecycle,
                    machine_id, Some(stale))).unwrap();
            });
            let mut d = Dispatcher::<AppHost>::new();
            command(&mut rig, &mut d, SessionCmd::RequestEndpoint { sid: id });
            let records = rig.session_adapter.take_results();
            assert_eq!(records.len(), 1);
            assert_receipt(&rig, &records[0], 1);
            assert!(records[0].terminal);
            assert!(std::ptr::eq(old_client, crate::plex::client_for(id).unwrap()));
            assert_eq!(std::fs::read(tmp.path()).unwrap(), original_disk);
            let mut newer = saved;
            newer.server.address = "127.0.0.2".into();
            newer.server.origin_url = "http://127.0.0.2:32400".into();
            newer.server.token = "synthetic-new-profile-token".into();
            newer.user.token = newer.server.token.clone();
            newer.sources[0].address = newer.server.address.clone();
            newer.sources[0].origin_url = newer.server.origin_url.clone();
            newer.sources[0].token = newer.server.token.clone();
            assert!(rig.session.snapshot_init().pending[&1].expected.matches(&newer),
                "identity still matches: this test must exercise native incarnation rejection");
            session::save(&newer);
            let disk = std::fs::read(tmp.path()).unwrap();
            let replacement = crate::plex::register_for_test("resource-server", "127.0.0.2", 32400,
                "synthetic-new-profile-token", "synthetic-resource-client");
            assert_eq!(replacement, id);
            let new_client = crate::plex::client_for(id).unwrap();
            assert!(!std::ptr::eq(old_client, new_client));
            assert_ne!(new_client.instance_gen(), old_instance);
            assert_ne!(new_client.token_gen(), old_token);
            let replacement_generation = new_client.token_gen();
            // URL construction reads the actual Client token without making any request.
            let replacement_url = new_client.direct_play_url("/synthetic", "test").to_url();
            assert!(replacement_url.contains("X-Plex-Token=synthetic-new-profile-token"));
            let terminal = records[0].clone();
            frame(&mut rig, &mut d, records);
            assert!(std::ptr::eq(new_client, crate::plex::client_for(id).unwrap()));
            assert_eq!(new_client.host(), "127.0.0.2");
            assert_eq!(new_client.token_gen(), replacement_generation);
            assert_eq!(new_client.direct_play_url("/synthetic", "test").to_url(), replacement_url);
            assert_eq!(std::fs::read(tmp.path()).unwrap(), disk, "stale result cannot rewrite actual file");
            assert_eq!(session::peek().sources[0].address, "127.0.0.2");
            assert_eq!(session::peek().server.address, "127.0.0.2");
            assert!(rig.session.snapshot_init().pending.is_empty());
            assert!(rig.session.snapshot_init().pending_commit.is_none());
            assert!(!rig.session_adapter.admitted(&terminal));
            // Real readmission, then guard terminal/main application: no manual flight release.
            let req = rig.session.snapshot_init().next_req + 1;
            rig.session_adapter.inject_fixture_work(req, |_, input| {
                assert!(matches!(input, SessionWork::Endpoint { .. }));
            });
            command(&mut rig, &mut d, SessionCmd::RequestEndpoint { sid: id });
            assert!(rig.session.snapshot_init().pending.contains_key(&req));
            let next = rig.session_adapter.take_results();
            assert_eq!(next.len(), 1);
            assert_receipt(&rig, &next[0], req);
            frame(&mut rig, &mut d, next);
            assert!(rig.session.snapshot_init().pending.is_empty());
            assert_eq!(std::fs::read(tmp.path()).unwrap(), disk);
            tmp.assert_only_target();
        }
    }

    // Private resource fixture, matching the old local-Ctl test's scope (no actual disk claim).
    mod picker_refresh_regression {
        use super::*;

        fn refresh(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, found: Vec<SourceRef>, wrong: bool) {
            let req = rig.session.snapshot_init().next_req + 1;
            let epoch = rig.auth_read().0.flow_epoch;
            rig.session_adapter.inject_fixture_work(req, move |output, input| {
                let SessionWork::ServerRoster { session, expected } = input else {
                    panic!("RefreshRoster must capture real roster work");
                };
                assert!(expected.matches(&session));
                let mut identity = serde_json::to_value(crate::auth::SessionIdentity::of(&session)).unwrap();
                if wrong { identity["account_token"] = "another-account".into(); }
                let resources: Vec<_> = found.iter().map(|source| serde_json::json!({
                    "clientIdentifier":source.machine_id, "name":source.name, "provides":"server",
                    "owned":source.owned, "accessToken":source.token
                })).collect();
                let progress = serde_json::from_value(serde_json::json!({
                    "epoch":epoch, "expected":identity, "outcome":{"Reconcile":{
                        "resources":resources, "found":found, "household":[], "settled":[]
                    }}
                })).unwrap();
                output.complete(crate::auth::AuthProgress::ServerRoster(progress)).unwrap();
            });
            command(rig, d, SessionCmd::RefreshRoster);
            let records = rig.session_adapter.take_results();
            assert_eq!(records.len(), 1);
            let record = records[0].clone();
            assert_eq!(record.addr, crate::ui::machine::Addr {
                to: MachineId::Session, req: crate::ui::machine::RequestId(req),
            });
            assert_eq!(record.key.epoch, epoch);
            assert!(epoch > u64::from(u32::MAX));
            assert!(record.key.op == crate::auth::owner::SessionOp::ServerRoster);
            assert!(record.terminal && rig.session_adapter.admitted(&record));
            frame(rig, d, records);
            // Deliberately no wrong-terminal pending-count assertion: core owns that retirement fix.
        }

        fn data(rig: &mut Bridge) -> serde_json::Value {
            let owner = rig.session.snapshot_init().persisted;
            let resources = rig.session_adapter.fixture_resources();
            serde_json::json!({"owner":owner, "disk":resources.disk, "registry":resources.registry_writes})
        }

        #[test]
        fn a_refresh_reconciles_the_picker_snapshot_before_take_ready_can_save_it() {
            let mut init = crate::auth::SessionInit::captured(stored());
            init.phase = Phase::Profiles;
            init.epoch = u64::from(u32::MAX) + 160;
            init.users = vec![crate::auth::UserTile { uuid: "admin".into(), protected: false,
                ..Default::default() }];
            let mut rig = Bridge::for_session_test(init);
            let mut d = Dispatcher::<AppHost>::new();
            let primary = SourceRef { address: "127.0.0.42".into(),
                origin_url: "http://127.0.0.42:32400".into(), token: "new-primary-token".into(),
                ..stored().sources[0].clone() };
            let share = SourceRef { machine_id: "refresh-share".into(), name: "Share".into(),
                address: "127.0.0.43".into(), origin_url: "http://127.0.0.43:32400".into(),
                port: 32400, token: "share-token".into(), owned: false, ..Default::default() };
            refresh(&mut rig, &mut d, vec![primary, share], false);
            assert_eq!(rig.auth_read().0.phase, Phase::Profiles);
            let snapshot = rig.session.snapshot_init().persisted;
            assert_eq!(snapshot.server.address, "127.0.0.42");
            assert_eq!(snapshot.pms_token(), "new-primary-token");
            assert_eq!(snapshot.sources.len(), 2);
            assert!(!rig.session.snapshot_init().apply_pending);
            let retained = data(&mut rig);
            let contrasting = vec![SourceRef { machine_id: "different-primary".into(),
                name: "Different grant".into(), address: "127.0.0.99".into(), port: 32400,
                origin_url: "http://127.0.0.99:32400".into(), token: "contrasting-token".into(),
                owned: true, ..Default::default() }];
            // Keep the original wrong-account + empty-roster case, but do not rely on it alone:
            // current policy can treat an empty reconcile as a no-op even without the identity guard.
            for found in [Vec::new(), contrasting.clone()] {
                refresh(&mut rig, &mut d, found, true);
                assert_eq!(data(&mut rig), retained, "wrong-account data cannot replace or empty the picker");
                assert_eq!(rig.auth_read().0.phase, Phase::Profiles);
                assert_eq!(rig.session.snapshot_init().persisted.sources.len(), 2);
            }
            // Legitimate same-user choice both retires any obsolete interest and permits Ready.
            // No owner replacement/raw epoch write/manual pending or receipt release.
            command(&mut rig, &mut d, SessionCmd::SelectProfile { index: 0, pin: None });
            assert_eq!(rig.auth_read().0.phase, Phase::Ready);
            assert!(rig.session.snapshot_init().apply_pending);
            assert_eq!(rig.session.snapshot_init().persisted.pms_token(), "new-primary-token");
            command(&mut rig, &mut d, SessionCmd::TakeReady);
            let ready = rig.take_session_ready().expect("refreshed credentials must be handed off");
            assert_eq!(ready.origin.host(), "127.0.0.42");
            assert_eq!(ready.token, "new-primary-token");
            assert!(rig.take_session_ready().is_none());
            let disk = &rig.session_adapter.fixture_resources().disk;
            assert_eq!(disk.server.address, "127.0.0.42");
            assert_eq!(disk.pms_token(), "new-primary-token");
            assert_eq!(disk.sources.len(), 2);
            // Positive control for the exact contrasting data: with current identity it changes
            // primary, token AND grants, proving the wrong-identity rejection was not an empty no-op.
            refresh(&mut rig, &mut d, contrasting, false);
            let final_state = rig.session.snapshot_init().persisted;
            assert_eq!(final_state.server.machine_id, "different-primary");
            assert_eq!(final_state.server.address, "127.0.0.99");
            assert_eq!(final_state.pms_token(), "contrasting-token");
            assert_eq!(final_state.sources.len(), 1);
        }
    }
}
