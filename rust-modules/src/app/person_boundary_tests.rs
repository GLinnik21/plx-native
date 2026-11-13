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

#[test]
fn mounted_person_recovers_from_rejected_capture_without_enter_or_uncover() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-mounted-capture-retry");
    let mut bridge = Bridge::for_test(|| 0);
    bridge.person_session_capture = |_| false;
    let mut dispatcher = Dispatcher::<AppHost>::new();
    show_page(&mut dispatcher, super::test_support::person_arg("pending-actor"));
    frame(&mut dispatcher, &mut bridge, super::test_support::tick(0), vec![]);
    let entry = dispatcher.nav.top_page().unwrap().id;
    assert!(bridge.person_view().current().is_none(), "incoherent input cannot Open");
    assert!(!bridge.stores.person.has_jobs(), "no claim before coherent input");
    bridge.person_session_capture = Bridge::capture_store_session_live;
    assert!(bridge.person_pump(), "the owner must deliver its deferred Open on recovery");
    assert!(bridge.pending_person_open.is_none(), "successful delivery consumes the obligation");
    let notice = bridge.stores.person.gen();
    assert_eq!(bridge.person_view().current().unwrap().key, "pending-actor");
    for n in 1..4 {
        frame(&mut dispatcher, &mut bridge, super::test_support::tick(n), vec![]);
        bridge.person_pump();
        assert_eq!(dispatcher.nav.top_page().unwrap().id, entry, "the page stayed mounted");
        assert_eq!(bridge.stores.person.gen(), notice, "recovery delivers Open exactly once");
    }
    bridge.assert_no_store_jobs_left_for_test();
}

fn deferred_open(key: &str) -> plx_data::stores::person::PersonCmd {
    plx_data::stores::person::PersonCmd::Open {
        sid: ServerId::UNSET, key: key.into(), guid: String::new(),
        name: key.into(), thumb: String::new(),
    }
}

#[test]
fn close_or_reset_before_capture_recovery_cancels_the_owed_open() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-deferred-close");
    for cancel in [plx_data::stores::person::PersonCmd::Close, plx_data::stores::person::PersonCmd::Reset] {
        let mut bridge = Bridge::for_test(|| 0);
        bridge.person_session_capture = |_| false;
        assert!(!bridge.person_run(deferred_open("cancelled")));
        assert!(bridge.pending_person_open.is_some(), "the bridge accepted responsibility for delivery");
        bridge.person_run(cancel);
        assert!(bridge.pending_person_open.is_none());
        let notice = bridge.stores.person.gen();
        bridge.person_session_capture = Bridge::capture_store_session_live;
        bridge.person_pump();
        assert!(bridge.person_view().current().is_none());
        assert_eq!(bridge.stores.person.gen(), notice, "a cancelled Open cannot reappear");
        bridge.assert_no_store_jobs_left_for_test();
    }
}

#[test]
fn replacement_deferred_open_is_latest_wins_and_delivered_once() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-deferred-replacement");
    let mut bridge = Bridge::for_test(|| 0);
    bridge.person_session_capture = |_| false;
    bridge.person_run(deferred_open("old"));
    bridge.person_run(deferred_open("latest"));
    assert!(bridge.pending_person_open.is_some());
    assert!(bridge.person_view().current().is_none());
    assert_eq!(bridge.stores.person.gen(), 0, "no speculative Open or claim");
    bridge.person_session_capture = Bridge::capture_store_session_live;
    assert!(bridge.person_pump());
    assert_eq!(bridge.person_view().current().unwrap().key, "latest");
    assert!(bridge.pending_person_open.is_none(), "successful delivery consumes the obligation");
    let notice = bridge.stores.person.gen();
    for _ in 0..3 { bridge.person_pump(); }
    assert_eq!(bridge.stores.person.gen(), notice);
    bridge.assert_no_store_jobs_left_for_test();
}

#[test]
fn profile_edge_before_capture_recovery_cannot_reopen_the_old_person() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-deferred-profile-edge");
    let mut bridge = Bridge::for_test(|| 0);
    bridge.live_screen_inputs = false;
    bridge.person_session_capture = |_| false;
    bridge.person_run(deferred_open("old-profile"));
    assert!(bridge.pending_person_open.is_some());
    bridge.screen_inputs.profile = std::sync::Arc::new(session::CurrentProfile {
        generation: bridge.screen_inputs.profile.generation.wrapping_add(1), user: None, stamp: String::new(),
    });
    bridge.person_session_capture = Bridge::capture_store_session_live;
    bridge.person_pump();
    assert!(bridge.person_view().current().is_none());
    assert_eq!(bridge.stores.person.gen(), 0);
    bridge.assert_no_store_jobs_left_for_test();
}

#[test]
fn leaving_an_empty_mounted_person_cancels_its_deferred_open() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-deferred-navigation");
    let mut bridge = Bridge::for_test(|| 0);
    bridge.person_session_capture = |_| false;
    let mut dispatcher = Dispatcher::<AppHost>::new();
    show_page(&mut dispatcher, super::test_support::person_arg("left-actor"));
    frame(&mut dispatcher, &mut bridge, super::test_support::tick(0), vec![]);
    let entry = dispatcher.nav.top_page().unwrap().id;
    assert!(bridge.pending_person_open.is_some());
    nav_root(&mut dispatcher, super::test_support::detail_arg("replacement-page"));
    for n in 1..40 { frame(&mut dispatcher, &mut bridge, super::test_support::tick(n), vec![]); }
    assert_ne!(dispatcher.nav.top_page().unwrap().id, entry);
    assert!(bridge.pending_person_open.is_none(), "teardown cancels even before recovery");
    assert!(bridge.person_view().current().is_none());
    bridge.person_session_capture = Bridge::capture_store_session_live;
    bridge.person_pump();
    assert!(bridge.person_view().current().is_none(), "navigation cannot resurrect the retired page");
    bridge.assert_no_store_jobs_left_for_test();
}

#[test]
fn session_owner_edge_before_capture_recovery_cannot_reopen_the_old_person() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-deferred-owner-edge");
    let mut bridge = Bridge::for_test(|| 0);
    bridge.person_session_capture = |_| false;
    bridge.person_run(deferred_open("retired-owner"));
    assert!(bridge.pending_person_open.is_some());
    let before = { let owner = bridge.session.read().0; (owner.scope.0, owner.flow_epoch) };
    let mut dispatcher = Dispatcher::<AppHost>::new();
    execute_session_command(&mut dispatcher, plx_session::auth::SessionCmd::SignOut);
    frame(&mut dispatcher, &mut bridge, super::test_support::tick(0), vec![]);
    let after = { let owner = bridge.session.read().0; (owner.scope.0, owner.flow_epoch) };
    assert_ne!(before, after, "exercise the real session owner cancellation edge");
    bridge.person_session_capture = Bridge::capture_store_session_live;
    bridge.person_pump();
    assert!(bridge.person_view().current().is_none());
    bridge.assert_no_store_jobs_left_for_test();
}

#[test]
fn controlled_account_edge_before_recovery_cannot_reopen_the_old_person() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-deferred-controlled-account");
    let mut bridge = Bridge::for_test(|| 0);
    bridge.live_screen_inputs = false;
    bridge.screen_inputs.session = std::sync::Arc::new(session::Session {
        client_id: "account-a".into(), account_token: "token-a".into(), ..Default::default()
    });
    bridge.person_session_capture = |_| false;
    bridge.person_run(deferred_open("old-account"));
    assert!(bridge.pending_person_open.is_some());
    bridge.screen_inputs.session = std::sync::Arc::new(session::Session {
        client_id: "account-b".into(), account_token: "token-b".into(), ..Default::default()
    });
    bridge.person_session_capture = Bridge::capture_store_session_live;
    bridge.person_pump();
    assert!(bridge.pending_person_open.is_none());
    assert!(bridge.person_view().current().is_none());
    assert_eq!(bridge.stores.person.gen(), 0);
    bridge.assert_no_store_jobs_left_for_test();
}

#[test]
fn live_tenure_revocation_before_recovery_cannot_reopen_the_old_person() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-deferred-live-tenure");
    let mut bridge = Bridge::for_test(|| 0);
    bridge.live_screen_inputs = true;
    bridge.person_session_capture = |_| false;
    bridge.person_run(deferred_open("revoked-account"));
    assert!(bridge.pending_person_open.is_some());
    let tenure = session::tenure();
    session::revoke_cached_session();
    assert_ne!(session::tenure(), tenure);
    bridge.person_session_capture = Bridge::capture_store_session_live;
    bridge.person_pump();
    assert!(bridge.pending_person_open.is_none());
    assert!(bridge.person_view().current().is_none());
    assert_eq!(bridge.stores.person.gen(), 0);
    bridge.assert_no_store_jobs_left_for_test();
}

#[test]
fn retired_sender_cannot_cancel_or_close_its_replacement() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-deferred-sender-replacement");
    let mut bridge = Bridge::for_test(|| 0);
    let old = Some(MachineId::Instance(InstanceId(901)));
    let latest = Some(MachineId::Instance(InstanceId(902)));
    bridge.person_session_capture = |_| false;
    bridge.person_run_from(old, deferred_open("old"));
    bridge.person_run_from(latest, deferred_open("latest"));
    bridge.person_run_from(old, plx_data::stores::person::PersonCmd::Close);
    assert!(bridge.pending_person_open.is_some());
    bridge.person_session_capture = Bridge::capture_store_session_live;
    assert!(bridge.person_pump());
    assert_eq!(bridge.person_view().current().unwrap().key, "latest");
    let notice = bridge.stores.person.gen();
    assert!(!bridge.person_run_from(old, plx_data::stores::person::PersonCmd::Close));
    assert_eq!(bridge.stores.person.gen(), notice, "retired Close cannot mutate its replacement");
    for _ in 0..3 { bridge.person_pump(); }
    assert_eq!(bridge.stores.person.gen(), notice, "only one successful Open");
    bridge.person_run_from(latest, plx_data::stores::person::PersonCmd::Close);
    assert!(bridge.person_view().current().is_none());
    bridge.assert_no_store_jobs_left_for_test();
}

#[test]
fn mounted_person_replacement_survives_old_empty_page_teardown() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-mounted-deferred-replacement");
    let mut bridge = Bridge::for_test(|| 0);
    bridge.person_session_capture = |_| false;
    let mut dispatcher = Dispatcher::<AppHost>::new();
    show_page(&mut dispatcher, super::test_support::person_arg("old-mounted"));
    frame(&mut dispatcher, &mut bridge, super::test_support::tick(0), vec![]);
    let old = dispatcher.nav.top_page().unwrap().id;
    nav_root(&mut dispatcher, super::test_support::person_arg("latest-mounted"));
    for n in 1..40 { frame(&mut dispatcher, &mut bridge, super::test_support::tick(n), vec![]); }
    assert_ne!(dispatcher.nav.top_page().unwrap().id, old);
    assert!(bridge.pending_person_open.is_some());
    bridge.person_session_capture = Bridge::capture_store_session_live;
    assert!(bridge.person_pump());
    assert_eq!(bridge.person_view().current().unwrap().key, "latest-mounted");
    let notice = bridge.stores.person.gen();
    for n in 40..44 { frame(&mut dispatcher, &mut bridge, super::test_support::tick(n), vec![]); bridge.person_pump(); }
    assert_eq!(bridge.stores.person.gen(), notice);
    bridge.assert_no_store_jobs_left_for_test();
}

#[test]
fn idle_person_100_host_passes_do_not_read_developer_seeds() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-idle-seed-reads");
    let mut bridge = Bridge::for_test(|| 0);
    let mut reads = 0;
    for _ in 0..100 {
        bridge.person_pump_with_seed_read(&mut |_| { reads += 1; None });
    }
    println!("100 idle Person host passes: {reads} seed reads");
    assert_eq!(reads, 0, "closed Person must not touch trigger paths or files");
    assert!(bridge.person_view().current().is_none());
    bridge.assert_no_store_jobs_left_for_test();
}

#[test]
fn person_seed_reads_wait_for_admission_and_track_open_capture_changes() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-open-seed-reads");
    let mut bridge = Bridge::for_test(|| 0);
    bridge.person_session_capture = |_| false;
    bridge.person_run(deferred_open("seeded"));
    let mut reads = Vec::new();
    assert!(!bridge.person_pump_with_seed_read(&mut |name| { reads.push(name.to_owned()); Some("2".into()) }));
    assert!(reads.is_empty(), "rejected capture cannot read speculative Open seeds");
    bridge.person_session_capture = Bridge::capture_store_session_live;
    assert!(bridge.person_pump_with_seed_read(&mut |name| { reads.push(name.to_owned()); Some("2".into()) }));
    assert_eq!(reads.iter().filter(|s| s.as_str() == "personbio").count(), 1);
    assert!(reads.iter().any(|s| s == "personcredits"));
    assert_eq!(bridge.person_capture.dev_bio.as_deref(), Some("2"));
    assert_eq!(bridge.person_capture.dev_credits.as_deref(), Some("2"));
    let initial_total = plx_data::person::filmography_total(bridge.person_view().current().unwrap());
    assert!(initial_total > 0, "the admitted Open seeds the actual Person filmography");
    reads.clear();
    bridge.person_pump_with_seed_read(&mut |name| { reads.push(name.to_owned()); Some("3".into()) });
    assert_eq!(reads, ["personcredits"], "an open page refreshes credits, not its Open-only biography");
    assert_eq!(bridge.person_capture.dev_credits.as_deref(), Some("3"));
    assert_eq!(plx_data::person::filmography_total(bridge.person_view().current().unwrap()), initial_total + 1);
    assert_eq!(bridge.person_capture.dev_bio.as_deref(), Some("2"));
    bridge.person_run(plx_data::stores::person::PersonCmd::Close);
    reads.clear();
    bridge.person_pump_with_seed_read(&mut |name| { reads.push(name.to_owned()); None });
    assert!(reads.is_empty(), "closing retires seed IO immediately");
    bridge.assert_no_store_jobs_left_for_test();
}

#[test]
fn controlled_person_capture_never_calls_the_live_seed_reader() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("person-controlled-seed-reads");
    let mut bridge = Bridge::for_test(|| 0);
    for live in [false, true] {
        bridge.live_screen_inputs = live;
        bridge.person_pump_with_seed_read(&mut |_| panic!("closed Person must not read live trigger files"));
        assert_eq!(bridge.person_capture.controlled, !live, "closed pumps still capture control identity");
    }
    let mut capture = PersonCapture::default();
    capture.refresh("", true, true, &mut |_| panic!("controlled input must not read live trigger files"));
    assert!(capture.controlled);
    assert!(capture.dev_bio.is_none());
}

impl Bridge {
    pub(crate) fn assert_person_provenance_at_dispatch(tap: &mut dyn plx_ui::dispatch::Tap<AppHost>) {
        use plx_data::stores::person::PersonCmd;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CAPTURES: AtomicUsize = AtomicUsize::new(0);
        let sender = MachineId::Instance(InstanceId(3));
        let other = MachineId::Instance(InstanceId(4));
        let mut bridge = Bridge::for_test(|| 0);
        bridge.live_screen_inputs = false;
        assert!(bridge.person_run_from(Some(sender), deferred_open("admitted")));
        bridge.stores.person.seed_ownership_fixture_for_test();
        let publication = bridge.stores.person.ownership_fixture_for_test();
        let generation = bridge.stores.person.gen();
        let owner = bridge.person_owner;
        let captured = bridge.store_session.observed.clone();
        let capture_guid = bridge.person_capture.guid.clone();
        bridge.person_session_capture = |_| { CAPTURES.fetch_add(1, Ordering::SeqCst); false };
        CAPTURES.store(0, Ordering::SeqCst);
        let mut dispatcher = Dispatcher::<AppHost>::new();
        let parts = CxParts { tick: Tick::default(), press: Default::default(),
            focus: Default::default(), owner: InputOwner::Entry(EntryId(0)) };
        let mut report = FrameReport::default();
        // The tap sees the forged envelope, but cannot prevent its execution.
        for (from, embedded) in [(MachineId::Nav, sender), (sender, MachineId::Nav), (other, sender)] {
            for command in [PersonCmd::Reset, deferred_open("forged")] {
                dispatcher.emit(from, Fx::Deliver(MachineId::Store(StoreId::Person.ord()),
                    Delivery::Machine(AppMsg::PersonCommand { from: embedded, command })));
                assert_eq!(dispatcher.drain(&mut bridge, &parts, 1, &mut report, tap), 1);
                assert_eq!(bridge.stores.person.ownership_fixture_for_test(), publication,
                    "mismatched Person provenance must not mutate its publication or flights");
                assert_eq!(bridge.stores.person.gen(), generation);
                assert_eq!(bridge.person_owner, owner);
                assert!(bridge.pending_person_open.is_none(), "forged Open must not become deferred work");
                assert!(std::sync::Arc::ptr_eq(&bridge.store_session.observed, &captured));
                assert_eq!(bridge.person_capture.guid, capture_guid);
                assert_eq!(CAPTURES.load(Ordering::SeqCst), 0, "rejection must precede captured-input IO");
                assert!(!bridge.stores.person.has_jobs());
                assert!(bridge.take_held_store_jobs_for_test().is_empty(), "no runner IO admitted");
                bridge.assert_no_store_jobs_left_for_test();
            }
        }
        let mut out = Vec::new();
        let mut present = Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Store(StoreId::Person.ord()), &mut present);
        assert_eq!(Rig::deliver_from(&mut bridge, MachineId::Nav, MachineId::Store(StoreId::Person.ord()),
            &AppMsg::PersonCommand { from: sender, command: PersonCmd::Reset }, &parts, &mut fx), Handled::No);
        assert!(out.is_empty(), "refusal emits no work");
        // Matching provenance still executes in this drain, including after tap refusal.
        bridge.person_session_capture = Bridge::capture_store_session_live;
        dispatcher.emit(other, Fx::Deliver(MachineId::Store(StoreId::Person.ord()),
            Delivery::Machine(AppMsg::PersonCommand { from: other, command: deferred_open("matching") })));
        assert_eq!(dispatcher.drain(&mut bridge, &parts, 1, &mut report, tap), 1);
        assert_eq!(bridge.person_view().current().unwrap().key, "matching");
        assert!(bridge.pending_person_open.is_none());
        let matching = bridge.stores.person.ownership_fixture_for_test();
        dispatcher.emit(sender, Fx::Deliver(MachineId::Store(StoreId::Person.ord()),
            Delivery::Machine(AppMsg::PersonCommand { from: sender, command: PersonCmd::Close })));
        dispatcher.drain(&mut bridge, &parts, 1, &mut report, tap);
        assert_eq!(bridge.stores.person.ownership_fixture_for_test(), matching,
            "matching provenance must not bypass retired-instance authorization");
        dispatcher.emit(other, Fx::Deliver(MachineId::Store(StoreId::Person.ord()),
            Delivery::Machine(AppMsg::PersonCommand { from: other, command: PersonCmd::Reset })));
        assert_eq!(dispatcher.drain(&mut bridge, &parts, 1, &mut report, tap), 1);
        assert!(bridge.person_view().current().is_none());
        assert!(bridge.stores.person.gen() > generation);
        assert_eq!(CAPTURES.load(Ordering::SeqCst), 0);
        bridge.assert_no_store_jobs_left_for_test();
    }
}
