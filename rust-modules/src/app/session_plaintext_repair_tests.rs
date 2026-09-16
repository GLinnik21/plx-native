//! Issue #95 step 6: a session stored with a plaintext `http://` primary before pinning existed
//! must be repaired end to end through the real endpoint-repair pipeline — including the active
//! profile's cached record, which `Observation::Endpoint`'s commit left stale (fixed beside the
//! `next.refresh_profile_record()` call in `auth/owner.rs`, matching what `ProfileRoster`'s commit
//! already did a few lines above it).
#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::auth::owner::{SessionEvent, SessionWork};
    use crate::plex::session::{self, ProfileCreds, Session, ServerRef, SourceRef, UserRef};

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
    fn endpoint_repair_upgrades_a_stored_plaintext_session_and_its_cached_profile() {
        let _lock = crate::testlock::serial();
        let mt = unsafe { crate::task::MainThread::assume() };
        let tmp = session::TempSession::new("owner-plaintext-repair");
        let _cleanup = Cleanup(&mt);
        tmp.assert_only_target();
        crate::plex::reset_servers_for_test();

        let plain = SourceRef {
            machine_id: "synthetic-server".into(),
            address: "192.0.2.10".into(),
            port: 32400,
            origin_url: "http://192.0.2.10:32400".into(),
            token: "synthetic-token".into(),
            owned: true,
            ..Default::default()
        };
        let kid = UserRef { uuid: "kid".into(), title: "Kid".into(), token: plain.token.clone(), ..Default::default() };
        let server = ServerRef {
            machine_id: plain.machine_id.clone(), address: plain.address.clone(), port: plain.port,
            origin_url: plain.origin_url.clone(), token: plain.token.clone(), ..Default::default()
        };
        let saved = Session {
            client_id: "synthetic-client".into(),
            account_token: "synthetic-account".into(),
            user: kid.clone(),
            server: server.clone(),
            sources: vec![plain.clone()],
            profiles: vec![ProfileCreds {
                uuid: kid.uuid.clone(), user: kid, server: server.clone(),
                sources: vec![plain.clone()], pin: None,
            }],
            ..Default::default()
        };
        session::save(&saved);

        let id = crate::plex::register_for_test("synthetic-server", "192.0.2.10", 32400,
            "synthetic-token", "synthetic-client");
        let mut init = crate::auth::SessionInit::captured(saved);
        init.epoch = u64::from(u32::MAX) + 91;
        let epoch = init.epoch;
        let mut rig = Bridge::for_session_test(init);
        rig.session_adapter = crate::app::adapters::session::SessionAdapter::live_resources_for_test(&mt, false);
        rig.session_adapter.inject_fixture_work(1, move |output, input| {
            std::thread::spawn(move || {
                let SessionWork::Endpoint { session, expected, lifecycle, machine_id } = input
                    else { panic!("wrong worker family") };
                crate::auth::endpoint_worker_with_io(epoch, session, expected, lifecycle, machine_id, &output,
                    |_| Some(vec![serde_json::from_value(serde_json::json!({
                        "clientIdentifier": "synthetic-server", "provides": "server"
                    })).unwrap()]),
                    |resource, _| {
                        // `apply_refreshed_endpoint` reads only address/port/origin_url/tier off
                        // the fresh source and keeps the stored entry's token/machine_id/name/
                        // shared_by/owned, so a bare `SourceRef::default()` base is enough here.
                        let fresh = SourceRef {
                            address: "192.0.2.10".into(),
                            origin_url: "https://192-0-2-10.example.plex.direct:32400".into(),
                            tier: Some(crate::plex::probe::Location::Local),
                            ..SourceRef::default()
                        };
                        (Some(fresh), crate::auth::settled_probe(
                            &crate::plex::probe::plan(resource, crate::plex::CredentialPolicy::HttpsOnly),
                            crate::plex::probe::Outcome::Reachable,
                            Some(crate::plex::probe::Location::Local),
                            Some("192.0.2.10".into())))
                    });
            }).join().expect("endpoint worker failed");
        });

        let mut d = Dispatcher::<AppHost>::new();
        execute_session_command(&mut d, crate::auth::SessionCmd::RequestEndpoint { sid: id });
        frame(&mut rig, &mut d, Vec::new());
        let records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 1);
        frame(&mut rig, &mut d, records);

        let disk = session::peek();
        assert_eq!(
            disk.server.origin_url, "https://192-0-2-10.example.plex.direct:32400",
            "the primary must repair to the eligible https origin"
        );
        assert_eq!(disk.sources[0].origin_url, "https://192-0-2-10.example.plex.direct:32400");
        let cached = disk.profiles.iter().find(|p| p.uuid == "kid")
            .expect("the active profile's cached record must survive the repair");
        assert_eq!(
            cached.server.origin_url, "https://192-0-2-10.example.plex.direct:32400",
            "the cached ProfileCreds record must repair too, or an offline reseat of this \
             profile keeps dialling the stale plaintext origin forever"
        );
        assert_eq!(cached.sources[0].origin_url, "https://192-0-2-10.example.plex.direct:32400");
    }
}
