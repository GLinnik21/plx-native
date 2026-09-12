//! Profile-worker regression slice. Use actual Bridge/owner/Landing and shared worker policy;
//! only account/probe transport and waiting are injected. No global controller fixture.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::auth::owner::{SessionEvent, SessionWork};
    use crate::plex::account::{AccountClient, Resource, SwitchOutcome};
    use crate::auth::{Phase, SessionCmd, ProfileWorkIo};
    use crate::auth::owner::{RegistryPlan, SessionEnvelope};

    // Resource snapshots below deliberately grade private patch/order policy. They are not native
    // Client identity/token-generation or filesystem atomic-write/unlink assertions (see lane report).
    fn resources(rig: &mut Bridge) -> serde_json::Value {
        let r = rig.session_adapter.fixture_resources();
        serde_json::json!({ "disk": r.disk, "registry": r.registry_writes })
    }

    fn frame(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, records: Vec<SessionEnvelope>) {
        let results = records.into_iter().map(|record|
            (record.addr, AppMsg::Session(SessionEvent::Result(record)))).collect();
        d.frame_with(rig, Tick { ms: 16, dt_us: 16_000 }, Vec::new(), results, &mut NoTap, false);
    }

    fn command(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, cmd: SessionCmd) {
        execute_session_command(d, cmd);
        frame(rig, d, Vec::new());
    }

    fn profile_rig(protected: bool) -> Bridge {
        let mut stored = cached_fixture();
        if protected {
            stored.profiles[0].pin = Some(crate::plex::session::PinVerifier::new("4821"));
        }
        let mut init = crate::auth::SessionInit::captured(stored);
        init.epoch = u64::from(u32::MAX) + 20;
        init.phase = Phase::Profiles;
        init.users = vec![
            crate::auth::UserTile { uuid: "synthetic-kid".into(), title: "Kid".into(), protected, ..Default::default() },
            crate::auth::UserTile { uuid: "synthetic-admin".into(), title: "Admin".into(), ..Default::default() },
        ];
        Bridge::for_session_test(init)
    }

    fn inject(rig: &mut Bridge, io: impl ProfileWorkIo + Send + 'static) {
        let epoch = rig.auth_read().0.flow_epoch + 1;
        rig.session_adapter.inject_fixture_work(1, move |output, input| {
            // A genuine worker thread runs the production policy and joins even when it panics.
            std::thread::spawn(move || {
                let SessionWork::ProfileSwitch { session, tile, pin, recently_unreachable, .. } = input
                    else { panic!("wrong worker family") };
                let expected = crate::auth::SessionIdentity::of(&session);
                crate::auth::profile_switch_worker_with_io(epoch, expected, session, tile, pin,
                    recently_unreachable, &output, &mut { io });
            }).join().expect("profile worker panicked");
        });
    }

    struct RefusedIo;
    impl ProfileWorkIo for RefusedIo {
        fn switch(&mut self, _: &AccountClient, _: &str, _: Option<&str>) -> SwitchOutcome {
            SwitchOutcome::Refused(403)
        }
        fn resources(&mut self, _: &AccountClient) -> Option<Vec<Resource>> { panic!("refusal cannot discover") }
        fn probe(&mut self, _: &Resource, _: &[i64]) -> (Option<crate::plex::session::SourceRef>, crate::auth::SettledProbe) { panic!("refusal cannot probe") }
        fn gap(&mut self) { panic!("refusal cannot wait") }
    }

    #[test]
    fn r2a_offline_worker_completion_waits_for_main_application() {
        let mut rig = profile_rig(false);
        let mut d = Dispatcher::<AppHost>::new();
        let before = resources(&mut rig);
        inject(&mut rig, OfflineIo);
        command(&mut rig, &mut d, SessionCmd::SelectProfile { index: 0, pin: None });
        let publication = rig.session.publication();
        let owner = rig.session.hash();
        let records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 1);
        assert!(records[0].terminal);
        assert_eq!(rig.auth_read().0.phase, Phase::Switching);
        assert_eq!(rig.session.snapshot_init().persisted.user.uuid, "synthetic-admin");
        assert_eq!(rig.session.hash(), owner, "collecting worker output is inert");
        assert!(std::sync::Arc::ptr_eq(&publication, &rig.session.publication()));
        assert_eq!(resources(&mut rig), before);
        assert!(rig.take_session_ready().is_none());

        frame(&mut rig, &mut d, records);
        assert_eq!(rig.auth_read().0.phase, Phase::Ready);
        assert_eq!(rig.session.snapshot_init().persisted.user.uuid, "synthetic-kid");
        assert_ne!(resources(&mut rig)["registry"], before["registry"]);
        assert!(rig.session_adapter.fixture_resources().registry_writes.iter().any(|plan|
            matches!(plan, RegistryPlan::Install { sources, .. } if sources.iter().any(|source|
                source.machine_id == "synthetic-server" && source.token == "synthetic-kid-token"))),
            "application requests installation of the selected profile's cached token");
        assert_eq!(resources(&mut rig)["disk"], before["disk"], "Ready must not save credentials");
        assert!(rig.take_session_ready().is_none());
        command(&mut rig, &mut d, SessionCmd::TakeReady);
        assert!(rig.take_session_ready().is_some());
        assert_eq!(rig.session_adapter.fixture_resources().disk.user.uuid, "synthetic-kid");
        let saved = resources(&mut rig);
        command(&mut rig, &mut d, SessionCmd::TakeReady);
        assert!(rig.take_session_ready().is_none());
        assert_eq!(resources(&mut rig), saved, "handoff saves once");
    }

    #[test]
    fn a_worker_observation_is_inert_until_the_main_thread_applies_it() {
        // Refused must not use valid cached credentials, even for the correct cached PIN.
        // The real policy deliberately separates a PIN flash (empty banner) and connection banner.
        for pin in [None, Some("4821".to_string())] {
            let mut rig = profile_rig(pin.is_some());
            let mut d = Dispatcher::<AppHost>::new();
            inject(&mut rig, RefusedIo);
            let before = resources(&mut rig);
            command(&mut rig, &mut d, SessionCmd::SelectProfile { index: 0, pin: pin.clone() });
            let publication = rig.session.publication();
            let hash = rig.session.hash();
            let records = rig.session_adapter.take_results();
            assert_eq!(records.len(), 1);
            assert_eq!(rig.auth_read().0.phase, Phase::Switching);
            assert!(rig.auth_read().0.error.is_empty());
            assert!(!rig.auth_read().0.pin_denied);
            assert_eq!(rig.session.hash(), hash);
            assert!(std::sync::Arc::ptr_eq(&publication, &rig.session.publication()));
            assert_eq!(resources(&mut rig), before);
            frame(&mut rig, &mut d, records);
            assert_eq!(rig.auth_read().0.phase, Phase::Profiles);
            assert_eq!(rig.auth_read().0.pin_denied, pin.is_some());
            assert_eq!(rig.auth_read().0.error.as_ref(), if pin.is_some() { "" }
                else { "Couldn't switch profile — check the connection." });
            assert_eq!(resources(&mut rig), before);
            assert!(rig.take_session_ready().is_none());
        }
        // The same protected cache seats only on its correct PIN when the service is unreachable.
        for (pin, denied) in [("4821", false), ("0000", true)] {
            let mut rig = profile_rig(true);
            let mut d = Dispatcher::<AppHost>::new();
            inject(&mut rig, OfflineIo);
            command(&mut rig, &mut d, SessionCmd::SelectProfile { index: 0, pin: Some(pin.into()) });
            let records = rig.session_adapter.take_results();
            frame(&mut rig, &mut d, records);
            assert_eq!(rig.auth_read().0.phase, if denied { Phase::Profiles } else { Phase::Ready });
            assert_eq!(rig.auth_read().0.pin_denied, denied);
        }
    }

    #[test]
    fn a_late_profile_observation_cannot_replace_a_newer_flow() {
        let mut rig = profile_rig(true);
        let mut d = Dispatcher::<AppHost>::new();
        inject(&mut rig, RefusedIo);
        command(&mut rig, &mut d, SessionCmd::SelectProfile { index: 0, pin: Some("4821".into()) });
        let old_epoch = rig.auth_read().0.flow_epoch;
        let stale = rig.session_adapter.take_results();
        assert_eq!(stale.len(), 1);
        // Real same-user selection supersedes the old worker and takes the owner's fast Ready path.
        command(&mut rig, &mut d, SessionCmd::SelectProfile { index: 1, pin: None });
        assert!(rig.auth_read().0.flow_epoch > old_epoch);
        assert_eq!(rig.auth_read().0.phase, Phase::Ready);
        let before = resources(&mut rig);
        let newer_publication = rig.session.publication();
        frame(&mut rig, &mut d, stale);
        assert!(std::sync::Arc::ptr_eq(&newer_publication, &rig.session.publication()),
            "stale worker failure cannot replace the newer retained publication");
        assert_eq!(rig.auth_read().0.phase, Phase::Ready);
        assert!(rig.auth_read().0.error.is_empty());
        assert!(!rig.auth_read().0.pin_denied);
        assert_eq!(rig.session.snapshot_init().persisted.user.uuid, "synthetic-admin");
        assert_eq!(resources(&mut rig), before);
        command(&mut rig, &mut d, SessionCmd::TakeReady);
        assert!(rig.take_session_ready().is_some(), "the newer flow retains its handoff");
        assert!(rig.take_session_ready().is_none());
    }

    struct OfflineIo;
    struct OnlineIo { probes: usize }
    impl ProfileWorkIo for OnlineIo {
        fn switch(&mut self, _: &AccountClient, uuid: &str, _: Option<&str>) -> SwitchOutcome {
            assert_eq!(uuid, "synthetic-kid");
            SwitchOutcome::Switched(crate::plex::account::SwitchedUser {
                uuid: uuid.into(), title: "Kid".into(), auth_token: "synthetic-switched-account".into(),
                ..Default::default()
            })
        }
        fn resources(&mut self, _: &AccountClient) -> Option<Vec<Resource>> {
            Some(vec![
                serde_json::from_str(r#"{"clientIdentifier":"synthetic-server","name":"Synthetic server","provides":"server","owned":true,"accessToken":"synthetic-kid-token"}"#).unwrap(),
                serde_json::from_str(r#"{"clientIdentifier":"synthetic-share","name":"Synthetic share","provides":"server","owned":false,"accessToken":"synthetic-share-token"}"#).unwrap(),
            ])
        }
        fn probe(&mut self, resource: &Resource, _: &[i64]) -> (Option<crate::plex::session::SourceRef>, crate::auth::SettledProbe) {
            let expected = if self.probes == 0 { "synthetic-server" } else { "synthetic-share" };
            assert_eq!(resource.client_identifier, expected);
            assert!(self.probes < 2);
            self.probes += 1;
            let address = if resource.owned { "127.0.0.2" } else { "127.0.0.3" };
            let source = crate::plex::session::SourceRef {
                machine_id: resource.client_identifier.clone(), name: resource.name.clone(),
                owned: resource.owned, token: resource.access_token.clone(), address: address.into(),
                port: 32400, origin_url: format!("http://{address}:32400"),
                ..Default::default()
            };
            (Some(source), crate::auth::settled_probe(&crate::plex::probe::plan(resource),
                crate::plex::probe::Outcome::Reachable, Some(crate::plex::probe::Location::Local)))
        }
        fn gap(&mut self) {
            assert_eq!(self.probes, 1, "late share probing occurs after the initial Ready");
        }
    }

    fn online_records(rig: &mut Bridge, d: &mut Dispatcher<AppHost>) -> Vec<SessionEnvelope> {
        inject(rig, OnlineIo { probes: 0 });
        let before = resources(rig);
        command(rig, d, SessionCmd::SelectProfile { index: 0, pin: None });
        let records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 2, "real online worker emits Ready then exactly one terminal roster");
        assert!(!records[0].terminal && records[1].terminal);
        assert_eq!(records[0].addr, records[1].addr);
        assert_eq!(rig.auth_read().0.phase, Phase::Switching);
        assert_eq!(resources(rig), before, "online completion is inert before the owner drain too");
        records
    }

    fn drain_carried(rig: &mut Bridge, d: &mut Dispatcher<AppHost>) {
        // Bounded dispatcher turns, no sleeps or alternate reducer. More than the two admission,
        // commit and acknowledgement rounds needed by this fixture; assertions grade the outcome.
        for _ in 0..4 { frame(rig, d, Vec::new()); }
    }

    #[test]
    fn successful_profile_stream_preserves_handoff_preferences_and_erase_order() {
        // Separate legal request histories: a stream permits one terminal roster, not the several
        // terminal ProfileRoster values the legacy mailbox test manufactured on one epoch.
        for roster_before_take in [true, false] {
            let mut rig = profile_rig(false);
            let mut d = Dispatcher::<AppHost>::new();
            let initial_disk = resources(&mut rig)["disk"].clone();
            let mut records = online_records(&mut rig, &mut d);
            let roster = records.pop().unwrap();
            if roster_before_take {
                // Both valid observations are ingested in the same ordinary dispatcher batch. The
                // second reaches the owner while the first observation's commit ACK is queued.
                records.push(roster);
                for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST {
                    execute_session_command(&mut d, SessionCmd::NoteDeleteLeftovers(0));
                }
                let results = records.into_iter().map(|record|
                    (record.addr, AppMsg::Session(SessionEvent::Result(record)))).collect();
                let report = d.frame_with(&mut rig, Tick::default(), Vec::new(), results, &mut NoTap, false);
                assert!(report.carried > 0, "both ingested worker records must survive normal drain budgets");
                assert_eq!(resources(&mut rig)["disk"], initial_disk);
                drain_carried(&mut rig, &mut d);
                assert_eq!(rig.auth_read().0.phase, Phase::Ready);
                assert_eq!(resources(&mut rig)["disk"], initial_disk);
                assert_eq!(rig.session.snapshot_init().persisted.sources.len(), 2);
                command(&mut rig, &mut d, SessionCmd::TakeReady);
            } else {
                frame(&mut rig, &mut d, records);
                assert_eq!(rig.auth_read().0.phase, Phase::Ready);
                assert_eq!(resources(&mut rig)["disk"], initial_disk);
                command(&mut rig, &mut d, SessionCmd::TakeReady);
                let disk = &mut rig.session_adapter.fixture_resources().disk;
                assert_eq!(disk.user.uuid, "synthetic-kid");
                disk.recent_searches.push(crate::plex::session::RecentSearches {
                    user: "synthetic-kid".into(), terms: vec!["newer preference".into()],
                });
                disk.playback_quality = Some(crate::plex::session::PlaybackQuality::Original);
                assert!(!disk.sources.iter().any(|source| source.machine_id == "synthetic-share" && source.usable()));
                frame(&mut rig, &mut d, vec![roster]);
                drain_carried(&mut rig, &mut d);
                let disk = &rig.session_adapter.fixture_resources().disk;
                assert_eq!(disk.recent_searches[0].terms, ["newer preference"]);
                assert_eq!(disk.playback_quality, Some(crate::plex::session::PlaybackQuality::Original));
            }
            assert!(rig.take_session_ready().is_some());
            assert!(rig.take_session_ready().is_none());
            let disk = &rig.session_adapter.fixture_resources().disk;
            assert_eq!(disk.user.uuid, "synthetic-kid");
            assert_eq!(disk.server.address, "127.0.0.2");
            assert!(disk.sources.iter().any(|s| s.machine_id == "synthetic-share" && s.address == "127.0.0.3" && s.usable()));
            let before = resources(&mut rig);
            command(&mut rig, &mut d, SessionCmd::TakeReady);
            assert!(rig.take_session_ready().is_none());
            assert_eq!(resources(&mut rig), before);
        }

        // Erase beats receipt-bearing stale data already collected from the genuine worker.
        for ready_applied in [false, true] {
            let mut rig = profile_rig(false);
            let mut d = Dispatcher::<AppHost>::new();
            let mut stale = online_records(&mut rig, &mut d);
            if ready_applied {
                let ready = stale.remove(0);
                frame(&mut rig, &mut d, vec![ready]);
                command(&mut rig, &mut d, SessionCmd::TakeReady);
                assert!(rig.take_session_ready().is_some());
            }
            command(&mut rig, &mut d, SessionCmd::EraseLocal);
            drain_carried(&mut rig, &mut d);
            assert_eq!(rig.auth_read().0.phase, Phase::Deleted);
            let erased = resources(&mut rig);
            assert!(rig.session_adapter.fixture_resources().disk.account_token.is_empty());
            assert!(matches!(rig.session_adapter.fixture_resources().registry_writes.last(), Some(RegistryPlan::Revoke)));
            frame(&mut rig, &mut d, stale);
            drain_carried(&mut rig, &mut d);
            assert_eq!(rig.auth_read().0.phase, Phase::Deleted);
            assert!(rig.session.snapshot_init().persisted.account_token.is_empty());
            assert!(rig.auth_read().0.profile.is_none());
            assert_eq!(resources(&mut rig), erased, "stale receipts cannot undo erase or reinstall a registry");
            assert!(rig.take_session_ready().is_none());
        }
    }

    impl crate::auth::ProfileWorkIo for OfflineIo {
        fn switch(&mut self, _: &AccountClient, _: &str, _: Option<&str>) -> SwitchOutcome {
            SwitchOutcome::Unreachable
        }
        fn resources(&mut self, _: &AccountClient) -> Option<Vec<Resource>> {
            panic!("offline seating must not fetch resources")
        }
        fn probe(&mut self, _: &Resource, _: &[i64]) -> (Option<crate::plex::session::SourceRef>, crate::auth::SettledProbe) {
            panic!("offline seating must not probe")
        }
        fn gap(&mut self) { panic!("offline seating has no probe gap") }
    }

    fn cached_fixture() -> crate::plex::session::Session {
        use crate::plex::session::{ProfileCreds, ServerRef, Session, SourceRef, UserRef};
        let source = SourceRef { machine_id: "synthetic-server".into(), name: "Synthetic server".into(),
            address: "127.0.0.1".into(), port: 32400, origin_url: "http://127.0.0.1:32400".into(),
            token: "synthetic-kid-token".into(), owned: true, ..Default::default() };
        let server = ServerRef { machine_id: source.machine_id.clone(), name: source.name.clone(),
            address: source.address.clone(), port: source.port, origin_url: source.origin_url.clone(),
            token: source.token.clone(), ..Default::default() };
        let kid = UserRef { uuid: "synthetic-kid".into(), title: "Kid".into(), token: source.token.clone(),
            ..Default::default() };
        Session { client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
            user: UserRef { uuid: "synthetic-admin".into(), title: "Admin".into(),
                token: "synthetic-admin-token".into(), ..Default::default() },
            server: ServerRef { token: "synthetic-admin-token".into(), ..server.clone() },
            sources: vec![SourceRef { token: "synthetic-admin-token".into(), ..source.clone() }],
            profiles: vec![ProfileCreds { uuid: kid.uuid.clone(), user: kid, server,
                sources: vec![source], pin: None }], ..Default::default() }
    }

    #[test]
    fn real_profile_worker_injection_reaches_owner_and_handoff_without_global_state() {
        let mut init = crate::auth::SessionInit::captured(cached_fixture());
        init.phase = crate::auth::Phase::Profiles;
        init.users = vec![crate::auth::UserTile { uuid: "synthetic-kid".into(), title: "Kid".into(),
            ..Default::default() }];
        let mut rig = Bridge::for_session_test(init);
        rig.session_adapter.inject_fixture_work(1, |output, input| {
            let SessionWork::ProfileSwitch { session, tile, pin, recently_unreachable, .. } = input
                else { panic!("selection launched another worker family") };
            let expected = crate::auth::SessionIdentity::of(&session);
            crate::auth::profile_switch_worker_with_io(2, expected, session, tile, pin,
                recently_unreachable, &output, &mut OfflineIo);
        });
        let mut d = Dispatcher::<AppHost>::new();
        execute_session_command(&mut d, crate::auth::SessionCmd::SelectProfile { index: 0, pin: None });
        d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
        assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Switching);
        assert_eq!(rig.session_adapter.fixture_resources().disk.user.uuid, "synthetic-admin");
        assert!(rig.session_adapter.fixture_resources().registry_writes.is_empty());
        let records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 1);
        assert!(records[0].terminal, "actual offline worker completes with terminal Ready");
        let results = records.into_iter().map(|record| (record.addr, AppMsg::Session(SessionEvent::Result(record)))).collect();
        d.frame_with(&mut rig, Tick { ms: 16, dt_us: 16_000 }, Vec::new(), results, &mut NoTap, false);
        assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Ready);
        assert_eq!(rig.session.snapshot_init().persisted.user.uuid, "synthetic-kid");
        assert_eq!(rig.session_adapter.fixture_resources().disk.user.uuid, "synthetic-admin",
            "Ready is not the credential-save handoff");
        execute_session_command(&mut d, crate::auth::SessionCmd::TakeReady);
        d.frame_with(&mut rig, Tick { ms: 32, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
        assert!(rig.take_session_ready().is_some());
        assert!(rig.take_session_ready().is_none());
        assert_eq!(rig.session_adapter.fixture_resources().disk.user.uuid, "synthetic-kid");
    }
}
