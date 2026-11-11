use super::*;
use plx_plex::plex::{session, ClientKeys, ServerId};

#[test]
fn store_session_capture_rejects_adversarial_publication_without_partial_commit() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-coherent-capture");
    session::save(&session::Session { client_id: "person-a".into(), account_token: "token-a".into(), ..Default::default() });
    let mut capture = StoreSessionCapture::default();
    assert!(capture.refill_store_session(session::screen_input_revision));
    let retained = capture.observed.clone();
    let retained_generation = capture.visible_generation;
    let retained_revision = capture.revision;
    let mut calls = 0;
    // Force a capture, and publish between the reads and the closing fence.
    capture.controlled = true;
    assert!(!capture.refill_store_session(|| {
        calls += 1;
        if calls == 2 { session::save(&session::Session { client_id: "person-b".into(), account_token: "token-b".into(), ..Default::default() }); }
        session::screen_input_revision()
    }));
    assert!(std::sync::Arc::ptr_eq(&capture.observed, &retained));
    assert_eq!(capture.visible_generation, retained_generation);
    assert_eq!(capture.revision, retained_revision);
    assert!(capture.refill_store_session(session::screen_input_revision));
    assert!(std::sync::Arc::ptr_eq(capture.session.as_ref().unwrap(), &session::peek_settled().unwrap()));
    assert_eq!(capture.visible_generation, session::visible_generation());
    assert_eq!(capture.profile_generation, session::current_snapshot().generation);
    assert_eq!(capture.tenure, session::tenure());
    assert_eq!(capture.revision, Some(session::screen_input_revision()));
    capture.controlled = false;
    let retained = capture.observed.clone();
    let key = capture.profile_key.as_ptr();
    for _ in 0..8 { assert!(capture.refill_store_session(session::screen_input_revision)); }
    assert!(std::sync::Arc::ptr_eq(&capture.observed, &retained));
    assert_eq!(capture.profile_key.as_ptr(), key);
}

#[test]
fn actual_runner_launch_refusal_is_consumed_in_the_same_bridge_drain() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-same-drain-refusal");
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("person-refusal", "127.0.0.1", 1, "token", "cid");
    let mut bridge = Bridge::for_test(|| 0);
    bridge.person_run(plx_data::stores::person::PersonCmd::Open {
        sid, key: "7".into(), guid: "".into(), name: "Actor".into(), thumb: "".into(),
    });
    assert!(bridge.capture_store_session());
    super::super::adapters::stores::refill_client_keys(&mut bridge.client_keys);
    let owner = bridge.session.read().0;
    let input = bridge.person_capture.input(&bridge.client_keys, &bridge.store_session, owner.scope.0, owner.flow_epoch);
    bridge.stores.person_pump(&input);
    let jobs = bridge.stores.take_jobs_of(StoreId::Person);
    assert_eq!(jobs.len(), 1);
    let adapter = bridge.stores.person.adapter_for_test();
    let slot = sid.raw() as usize * 3 + 1;
    assert!(adapter.busy_for_test(slot));
    let notice = bridge.stores.person.gen();
    // The real runner resolves the captured key here and refuses this retired client.
    plx_plex::plex::reset_servers_for_test();
    assert_eq!(bridge.start_jobs(jobs), 1);
    assert!(!adapter.busy_for_test(slot));
    assert!(!adapter.has_mail_for_test(slot), "refusal did not masquerade as a network answer");
    assert!(bridge.stores.person.gen() > notice, "the owner observed the addressed terminal in this call");
    assert!(!bridge.stores.person.has_jobs(), "the refusal pass never re-emits work");
    bridge.assert_no_store_jobs_left_for_test();
}

#[test]
fn person_capture_carries_session_machine_scope_epoch_and_retained_profile_tenure() {
    let capture = StoreSessionCapture { profile_generation: 19, tenure: 23, profile_key: "tenant".into(), ..Default::default() };
    let person = PersonCapture::default();
    let keys = ClientKeys::default();
    let input = person.input(&keys, &capture, 29, 31);
    assert_eq!((input.profile_scope, input.owner_epoch, input.profile_generation, input.tenure), (29, 31, 19, 23));
    assert_eq!(input.profile_key, "tenant");
    assert_eq!(input.keys as *const ClientKeys, &keys as *const ClientKeys);
    assert_eq!(ServerId::UNSET, keys.current());
}
