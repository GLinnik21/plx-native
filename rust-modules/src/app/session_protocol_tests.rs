//! Core-owned child module for the approved Session transfer/Pump production traces.
//! Tests use super's real Bridge and dispatcher boundaries, not a replacement Rig.

use super::*;

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
