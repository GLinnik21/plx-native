//! ViewState optimistic edits and Hubs/Search pumps against the frame-retained Browse
//! directory, plus scrim-chrome isolation between two bridges.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::{directory_policy_fixture, DirectoryPolicyCleanup};

#[test]
fn viewstate_optimistic_home_edit_keeps_the_frame_directory_policy() {
    let _guard = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test(
        "bridge-viewstate-directory", "127.0.0.1", 9, "synthetic", "fixture");
    let _cleanup = DirectoryPolicyCleanup;
    let mut rig = Bridge::for_test(|| 0);
    rig.directory = directory_policy_fixture(sid, sid);
    crate::pms::seed_two_library_home_for_test(sid, rig.directory.view());
    rig.stores.viewstate.borrow_mut().hold_inflight_for_test(sid, "held");
    let _ = rig.stores.take_notices();

    assert!(rig.viewstate_run(crate::stores::viewstate::ViewStateCmd::Request {
        sid,
        rk: "alpha".into(),
        write: crate::viewstate::Write::Watched,
        detail: None,
        guid: String::new(),
    }));

    let snapshot = crate::pms::hubs_snapshot();
    let hub = snapshot.view().hub(0).expect("the pinned library's shelf");
    assert_eq!(hub.items.iter().map(|item| item.rk.as_str()).collect::<Vec<_>>(), ["alpha"],
        "the optimistic callback cannot restore the unpinned sibling library");
    assert!(hub.items[0].watched);
    let notices = rig.stores.take_notices();
    for store in [StoreId::Browse, StoreId::Hubs, StoreId::ViewState] {
        assert_eq!(notices.iter().filter(|(id, _)| *id == store).count(), 1,
            "the synchronous edit preserves one notice for {}", store.name());
    }
}

#[test]
fn separate_bridges_do_not_share_any_viewstate_owner_state_or_notice() {
    let _guard = crate::testlock::serial();
    let first = Bridge::for_test(|| 0);
    let mut second = Bridge::for_test(|| 0);

    first.stores.viewstate.borrow_mut().seed_ownership_fixture_for_test();
    let before_reset = first.stores.viewstate.borrow().ownership_fixture_for_test();
    second.viewstate_run(crate::stores::viewstate::ViewStateCmd::Reset);
    let after_reset = first.stores.viewstate.borrow().ownership_fixture_for_test();

    first.stores.viewstate.borrow_mut().seed_ownership_fixture_for_test();
    let before_pump = first.stores.viewstate.borrow().ownership_fixture_for_test();
    let _ = second.viewstate_pump();
    let after_pump = first.stores.viewstate.borrow().ownership_fixture_for_test();

    let second_notices = second.stores.take_notices();
    let first_notices = first.stores.take_notices();
    assert_eq!((after_reset, after_pump), (before_reset, before_pump),
        "resetting or pumping Bridge B must not clear or consume Bridge A's queue, flight, mailbox, retry/refresh latches");
    assert_eq!(second_notices.iter().filter(|(id, _)| *id == StoreId::ViewState).count(), 1,
        "Bridge B owns only its reset notice");
    assert_eq!(first_notices.iter().filter(|(id, _)| *id == StoreId::ViewState).count(), 1,
        "Bridge B draining its notices must leave Bridge A's notice untouched");
}

#[test]
fn reset_fences_a_late_old_viewstate_worker_from_the_post_reset_request() {
    let _guard = crate::testlock::serial();
    let mut bridge = Bridge::for_test(|| 0);
    bridge.stores.viewstate.borrow_mut().seed_post_reset_flight_for_test();
    let old_adapter = bridge.stores.viewstate.borrow().adapter_for_test();
    let finish_old_worker = bridge.stores.viewstate.borrow().late_completion_for_test();

    bridge.viewstate_run(crate::stores::viewstate::ViewStateCmd::Reset);
    bridge.stores.viewstate.borrow_mut().seed_post_reset_flight_for_test();
    let new_adapter = bridge.stores.viewstate.borrow().adapter_for_test();
    assert!(!std::sync::Arc::ptr_eq(&old_adapter, &new_adapter),
        "reset must rotate the ViewState worker adapter");
    finish_old_worker();
    let _ = bridge.viewstate_pump();

    assert_eq!(bridge.stores.viewstate.borrow().ownership_fixture_for_test().sent.as_deref(), Some("post-reset"),
        "a completion from the retired adapter must not satisfy the replacement request");
}

fn person_open(sid: crate::plex::ServerId, name: &str) -> crate::stores::person::PersonCmd {
    crate::stores::person::PersonCmd::Open {
        sid,
        key: "person-key".into(),
        guid: "plex://person/person-guid".into(),
        name: name.into(),
        thumb: String::new(),
    }
}

fn person_item(sid: crate::plex::ServerId, rk: &str, watched: bool) -> crate::pms::PmsMovie {
    crate::pms::PmsMovie {
        sid,
        rk: rk.into(),
        watched,
        unwatched: !watched,
        ..Default::default()
    }
}

fn deliver_person(rig: &mut Bridge, command: crate::stores::person::PersonCmd) {
    let parts = CxParts { tick: Tick::default(), press: Default::default(),
        focus: Default::default(), owner: InputOwner::Entry(EntryId(0)) };
    let mut out = Vec::new();
    let mut present = crate::ui::present::Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Store(StoreId::Person.ord()), &mut present);
    assert_eq!(Rig::<AppHost>::deliver(rig, MachineId::Store(StoreId::Person.ord()),
        &AppMsg::Store(StoreCmd::Person(command)), &parts, &mut fx), Handled::Yes);
}

#[test]
fn separate_bridges_do_not_share_any_person_owner_state_or_notice() {
    let _guard = crate::testlock::serial();
    let mut first = Bridge::for_test(|| 0);
    let mut second = Bridge::for_test(|| 0);
    let sid = crate::plex::ServerId::from_raw(0);

    first.person_run(person_open(sid, "first-owner"));
    first.stores.person.seed_ownership_fixture_for_test();
    let before_reset = first.stores.person.ownership_fixture_for_test();
    second.person_run(crate::stores::person::PersonCmd::Reset);
    let after_reset = first.stores.person.ownership_fixture_for_test();

    first.stores.person.seed_ownership_fixture_for_test();
    let before_pump = first.stores.person.ownership_fixture_for_test();
    let _ = second.person_pump();
    let after_pump = first.stores.person.ownership_fixture_for_test();

    assert_eq!((after_reset, after_pump), (before_reset, before_pump),
        "resetting or pumping Bridge B must not alter Bridge A's model, generation, retry, flight or mailbox");
    assert_eq!(first.person_view().current().map(|person| person.name.as_str()), Some("first-owner"));
    assert!(second.person_view().current().is_none());
    let second_notices = second.stores.take_notices();
    let first_notices = first.stores.take_notices();
    assert_eq!(second_notices.iter().filter(|(id, _)| *id == StoreId::Person).count(), 1,
        "Bridge B owns only its reset notice");
    assert_eq!(first_notices.iter().filter(|(id, _)| *id == StoreId::Person).count(), 1,
        "Bridge B draining its notices must leave Bridge A's notice untouched");
}

#[test]
fn person_reset_rotates_the_adapter_and_fences_a_late_old_worker() {
    let _guard = crate::testlock::serial();
    let mut bridge = Bridge::for_test(|| 0);
    let sid = crate::plex::ServerId::from_raw(0);
    bridge.person_run(person_open(sid, "old-person"));
    let old_adapter = bridge.stores.person.adapter_for_test();
    let finish_old_worker = bridge.stores.person.late_completion_for_test();

    bridge.person_run(crate::stores::person::PersonCmd::Reset);
    bridge.person_run(person_open(sid, "post-reset-person"));
    let new_adapter = bridge.stores.person.adapter_for_test();
    assert!(!std::sync::Arc::ptr_eq(&old_adapter, &new_adapter),
        "reset must rotate the Person worker adapter");
    finish_old_worker();
    let _ = bridge.person_pump();

    let person = bridge.person_view().current().expect("post-reset person remains open");
    assert_eq!(person.name, "post-reset-person");
    assert!(person.bio.is_empty(), "the retired worker cannot write the replacement person");
}

#[test]
fn addressed_person_store_command_changes_and_notifies_only_its_bridge() {
    let _guard = crate::testlock::serial();
    let mut first = Bridge::for_test(|| 0);
    let mut second = Bridge::for_test(|| 0);
    let sid = crate::plex::ServerId::from_raw(0);
    first.person_run(person_open(sid, "first-reader"));
    second.person_run(person_open(sid, "second-reader"));
    first.stores.person.install_for_test(vec![person_item(sid, "movie", false)], Vec::new());
    second.stores.person.install_for_test(vec![person_item(sid, "movie", false)], Vec::new());
    let _ = first.stores.take_notices();
    let _ = second.stores.take_notices();

    deliver_person(&mut first, crate::stores::person::PersonCmd::SetWatchedLocal {
        sid, rk: "movie".into(), on: true,
    });

    assert!(first.person_view().current().unwrap().shelf(0)[0].watched,
        "the addressed Person reader sees the optimistic edit immediately");
    assert!(!second.person_view().current().unwrap().shelf(0)[0].watched,
        "an unaddressed Person/Filmography reader keeps its own publication");
    assert_eq!(first.stores.take_notices().iter().filter(|(id, _)| *id == StoreId::Person).count(), 1);
    assert_eq!(second.stores.take_notices().iter().filter(|(id, _)| *id == StoreId::Person).count(), 0);
}

#[test]
fn profile_activation_clears_the_same_bridge_person_before_a_new_mount() {
    let _guard = crate::testlock::serial();
    let mut bridge = Bridge::for_test(|| 0);
    let sid = crate::plex::ServerId::from_raw(0);
    bridge.person_run(person_open(sid, "outgoing-profile"));
    assert!(bridge.person_view().current().is_some());

    let _ = crate::app::boot::activate_server_owned(&mut bridge);

    assert!(bridge.person_view().current().is_none(),
        "profile activation must clear the Person owner on the same Bridge before any new page mounts");
}

#[test]
fn viewstate_optimistic_edit_mutates_only_its_bridge_person_store() {
    let _guard = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test(
        "bridge-person-viewstate", "127.0.0.1", 9, "synthetic", "fixture");
    let mut first = Bridge::for_test(|| 0);
    let mut second = Bridge::for_test(|| 0);
    for bridge in [&mut first, &mut second] {
        bridge.person_run(person_open(sid, "shared-subject"));
        bridge.stores.person.install_for_test(
            vec![person_item(sid, "movie", false)], Vec::new());
        bridge.stores.viewstate.borrow_mut().hold_inflight_for_test(sid, "held");
        let _ = bridge.stores.take_notices();
    }

    assert!(first.viewstate_run(crate::stores::viewstate::ViewStateCmd::Request {
        sid,
        rk: "movie".into(),
        write: crate::viewstate::Write::Watched,
        detail: None,
        guid: String::new(),
    }));

    assert!(first.person_view().current().unwrap().shelf(0)[0].watched);
    assert!(!second.person_view().current().unwrap().shelf(0)[0].watched,
        "ViewState's callback must address the Person owner beside its own queue");
    assert_eq!(first.stores.take_notices().iter().filter(|(id, _)| *id == StoreId::Person).count(), 1);
    assert_eq!(second.stores.take_notices().iter().filter(|(id, _)| *id == StoreId::Person).count(), 0);
    crate::plex::reset_servers_for_test();
}

/// Detail owns the press decision, but the Bridge owns the retained Browse directory needed
/// by ViewState's optimistic Hubs edit. Keep both halves on the production dispatcher path;
/// the screen must not regain a free ViewState facade that can bypass this owner.
#[test]
fn detail_watch_activation_dispatches_the_addressed_store_effect_in_the_press_frame() {
    use crate::ui::dispatch::Tap;

    struct ViewStateDispatch {
        sid: crate::plex::ServerId,
        app_effects: usize,
        store_deliveries: usize,
    }

    impl Tap<AppHost> for ViewStateDispatch {
        fn effect(&mut self, _: u64, stamped: &crate::ui::machine::Stamped<AppHost>) {
            match &stamped.fx {
                Fx::App(AppFx::Store(StoreId::ViewState,
                    StoreCmd::ViewState(crate::stores::viewstate::ViewStateCmd::Request {
                        sid, rk, write, detail, guid,
                    }))) => {
                    assert_eq!((*sid, rk.as_str(), *write, detail.as_ref(), guid.as_str()),
                        (self.sid, "movie", crate::viewstate::Write::Watched,
                            Some(&crate::stores::viewstate::DetailRefresh {
                                sid: self.sid, rk: "movie".into(), keep: None,
                            }), "plex://movie"));
                    self.app_effects += 1;
                }
                Fx::Deliver(MachineId::Store(ord), Delivery::Machine(AppMsg::Store(
                    StoreCmd::ViewState(crate::stores::viewstate::ViewStateCmd::Request {
                        sid, rk, write, detail, guid,
                    })))) => {
                    assert_eq!(*ord, StoreId::ViewState.ord());
                    assert_eq!((*sid, rk.as_str(), *write, detail.as_ref(), guid.as_str()),
                        (self.sid, "movie", crate::viewstate::Write::Watched,
                            Some(&crate::stores::viewstate::DetailRefresh {
                                sid: self.sid, rk: "movie".into(), keep: None,
                            }), "plex://movie"));
                    self.store_deliveries += 1;
                }
                _ => {}
            }
        }
    }

    let _guard = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test(
        "bridge-detail-viewstate", "127.0.0.1", 9, "synthetic", "fixture");
    let _cleanup = DirectoryPolicyCleanup;
    let route = AppArg::Content(ContentArg::Detail { sid, rk: "movie".into() });
    let mut dispatcher = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    dispatcher.request(MachineId::Nav, NavOp::Root(route));
    dispatcher.frame_with(&mut rig, tick(0), Vec::new(), Vec::new(), &mut NoTap, false);

    crate::metadata::set_current_for_test(Some(crate::metadata::Detail {
        sid,
        rk: "movie".into(),
        kind: "movie".into(),
        watched: false,
        guid: "plex://movie".into(),
        ..Default::default()
    }));
    dispatcher.frame_with(&mut rig, tick(1), Vec::new(), Vec::new(), &mut NoTap, false);
    let play = dispatcher.focus().expect("Detail seats its Play control");
    dispatcher.frame_with(&mut rig, tick(2), script_key(Key::Right, tick(2)), Vec::new(),
        &mut NoTap, false);
    let watch = dispatcher.focus().expect("RIGHT reaches the watch control");
    assert_ne!(watch.elem, play.elem);

    rig.stores.viewstate.borrow_mut().hold_inflight_for_test(sid, "held");
    let instance = dispatcher.nav.top_page().and_then(|entry| entry.inst.as_ref())
        .expect("the Detail page is mounted").id;
    dispatcher.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
        Delivery::Screen(ScreenEvent::Activate(watch.elem))));
    let mut tap = ViewStateDispatch { sid, app_effects: 0, store_deliveries: 0 };
    dispatcher.frame_with(&mut rig, tick(3), Vec::new(), Vec::new(), &mut tap, false);

    assert_eq!((tap.app_effects, tap.store_deliveries), (1, 1),
        "the Detail effect must cross the addressed Bridge store delivery exactly once");
    assert!(crate::metadata::current().is_some_and(|detail| detail.watched),
        "the owning Bridge applies the optimistic edit before the press frame ends");
    assert_ne!(dispatcher.focus().expect("the watch control remains focused").elem, watch.elem,
        "same-frame reconciliation follows the watch control to its new identity");
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
}

#[test]
fn hubs_land_and_tick_keep_the_frame_directory_policy() {
    let _guard = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let own = crate::plex::register_for_test(
        "bridge-hubs-own", "127.0.0.1", 9, "synthetic", "fixture");
    let hidden = crate::plex::register_for_test(
        "bridge-hubs-hidden", "127.0.0.1", 10, "synthetic", "fixture");
    let _cleanup = DirectoryPolicyCleanup;
    let mut rig = Bridge::for_test(|| 0);
    rig.directory = directory_policy_fixture(own, hidden);
    let directory = rig.directory.clone();

    crate::pms::with_refused_fetches_for_test(|| {
        let _ = crate::stores::hubs::apply_with_directory(
            crate::stores::hubs::HubsCmd::Reset, directory.view());
        let parts = CxParts { tick: tick(0), press: Default::default(), focus: Default::default(),
            owner: InputOwner::Entry(EntryId(0)) };
        let mut present = Present::new();
        let mut effects = Vec::new();
        let mut fx = Effects::new(&mut effects, MachineId::Store(StoreId::Hubs.ord()), &mut present);
        assert_eq!(rig.deliver(MachineId::Store(StoreId::Hubs.ord()),
            &AppMsg::Store(StoreCmd::Hubs(crate::stores::hubs::HubsCmd::RefetchHubs)),
            &parts, &mut fx), Handled::Yes);
        drop(fx);
        assert_eq!(effects.iter().map(|effect| match &effect.fx {
            Fx::App(AppFx::Session(crate::auth::SessionCmd::RequestEndpoint { sid })) => *sid,
            _ => panic!("Hubs recovery was not preserved as a Session effect"),
        }).collect::<Vec<_>>(), [own],
            "the command must use the retained directory before landing or ticking");
        effects.clear();
        let _ = crate::stores::take_notices();

        crate::pms::queue_test_landing(Some(1));
        let result = crate::stores::hubs::take_results().pop().unwrap();
        let generation_before_landing = crate::stores::gen(StoreId::Hubs);
        let mut fx = Effects::new(&mut effects, MachineId::Store(StoreId::Hubs.ord()), &mut present);
        assert_eq!(rig.deliver(MachineId::Store(StoreId::Hubs.ord()),
            &AppMsg::HubsResult(result), &parts, &mut fx), Handled::Yes);
        drop(fx);
        assert!(effects.is_empty());
        let generation = crate::stores::gen(StoreId::Hubs);
        assert_eq!(generation, generation_before_landing + 1);
        assert_eq!(crate::stores::take_notices(), [(StoreId::Hubs, generation)],
            "one changed landing owes exactly one Hubs notice");
        let sources_after_land = crate::stores::hubs::apply_with_directory(
            crate::stores::hubs::HubsCmd::Retry, directory.view());
        assert_eq!(sources_after_land.endpoints.iter().map(|request| request.sid).collect::<Vec<_>>(), [own],
            "landing must keep the retained frame directory");

        let _ = crate::stores::hubs::apply_with_directory(
            crate::stores::hubs::HubsCmd::Reset, directory.view());
        let _ = crate::stores::hubs::apply_with_directory(
            crate::stores::hubs::HubsCmd::RefetchHubs, directory.view());
        let _ = crate::stores::take_notices();
        effects.clear();
        let mut fx = Effects::new(&mut effects, MachineId::Store(StoreId::Hubs.ord()), &mut present);
        assert_eq!(rig.deliver(MachineId::Store(StoreId::Hubs.ord()),
            &AppMsg::StoreWork(crate::stores::StoreWork::Hubs), &parts, &mut fx), Handled::Yes);
        drop(fx);
        assert!(effects.is_empty(),
            "tick must not admit the source excluded by this frame's retained directory");
        assert!(crate::stores::take_notices().is_empty(),
            "an idle retained-directory tick invents no Hubs notice");
    });
}

#[test]
fn search_capture_and_pump_keep_the_frame_directory_policy() {
    let _guard = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let own = crate::plex::register_for_test(
        "bridge-search-own", "127.0.0.1", 9, "synthetic", "fixture");
    let hidden = crate::plex::register_for_test(
        "bridge-search-hidden", "127.0.0.1", 10, "synthetic", "fixture");
    let _cleanup = DirectoryPolicyCleanup;
    let mut rig = Bridge::for_test(|| 0);
    rig.search_run(crate::stores::search::SearchCmd::Reset);

    let mut pages = Dispatcher::<AppHost>::new();
    rig.capture_views(&mut pages);
    assert!(rig.directory.view().sections().is_empty(),
        "a fresh Bridge owner starts empty");
    assert!(rig.search.view().scope().sources().iter().all(|source| source.libraries.is_empty()),
        "Search capture must describe the same retained directory as the rest of the frame");

    let directory = directory_policy_fixture(own, hidden);
    rig.directory = directory.clone();
    rig.search_run(crate::stores::search::SearchCmd::SetQuery("same frame".into()));
    let query_generation = rig.stores.search.query_gen();
    let _ = crate::stores::take_notices();
    let parts = CxParts { tick: tick(0), press: Default::default(), focus: Default::default(),
        owner: InputOwner::Entry(EntryId(0)) };
    let mut present = Present::new();
    let mut effects = Vec::new();
    let mut fx = Effects::new(&mut effects, MachineId::Store(StoreId::Search.ord()), &mut present);
    assert_eq!(rig.deliver(MachineId::Store(StoreId::Search.ord()),
        &AppMsg::StoreWork(crate::stores::StoreWork::Search { dt_us: 0 }), &parts, &mut fx), Handled::Yes);
    assert_eq!(rig.stores.search.query_gen(), query_generation,
        "the pump must not supersede against a different directory in the same frame");
    assert!(crate::stores::take_notices().is_empty(),
        "an idle retained-directory Search pump invents no notice");
}

/// The Search two-owner regression the store-ownership contract requires: two `Bridge`s must
/// share neither Search's query/shelves/notice-generation state nor its notice queue — including
/// a LANDED result, which is the part the query/notice checks alone cannot see, since both live on
/// `SearchState` and neither exercises the adapter mailbox a real fetch actually lands through.
/// Simulated RED against the pre-port process-wide `static`s this ported, and independently
/// against a shared-adapter regression a query/notice-only test would miss: temporarily making
/// `SearchStore::default` hand out one process-wide `Arc<SearchAdapter>` to every owner makes the
/// row `land_for_test` posts for A's adapter visible to whichever Bridge pumps next, so Bridge B's
/// pump would pick up the row landed for A (B's `pump` returns `true`, and its snapshot's shelves
/// are no longer empty) — that older process-wide code no longer exists on disk to run directly (see
/// `person.rs`'s and `viewstate.rs`'s sibling tests for the same shape, ported the same way), so
/// the red here is simulated rather than historical, exactly as `reproduce-before-fixing` requires
/// when a fix changes the seam a test would otherwise call.
#[test]
fn separate_bridges_do_not_share_any_search_owner_state_or_notice() {
    let _guard = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let server = crate::plex::register_for_test(
        "bridge-search-landing", "127.0.0.1", 11, "synthetic", "landing");
    let _cleanup = DirectoryPolicyCleanup;
    let mut first = Bridge::for_test(|| 0);
    let mut second = Bridge::for_test(|| 0);
    // Settle each owner's roster-generation baseline BEFORE landing anything: a store's first pump
    // ever sees the registry as "changed" from its fresh `visible = 0`, and that path supersedes —
    // wiping the very mailbox this test is about to fill.
    first.stores.search.pump(0.0);
    second.stores.search.pump(0.0);

    first.search_run(crate::stores::search::SearchCmd::SetQuery("first-owner".into()));
    let before_reset = first.stores.search.query().to_string();
    second.search_run(crate::stores::search::SearchCmd::Reset);
    let after_reset = first.stores.search.query().to_string();

    assert_eq!(before_reset, after_reset, "resetting Bridge B must not clear Bridge A's query");
    assert_eq!(after_reset, "first-owner");
    assert!(second.stores.search.query().is_empty(), "Bridge B starts with no query of its own");

    // Bridge A searches again (Bridge B's Reset above only had to leave A's TEXT alone; a fresh
    // query is what actually arms a fetch) and land a row straight into A's adapter, at A's own
    // generation, bypassing the worker.
    first.search_run(crate::stores::search::SearchCmd::SetQuery("landed-owner".into()));
    let gen = first.stores.search.query_gen();
    let idx = server.raw() as usize;
    let item = crate::search::Item::Media(crate::pms::PmsMovie {
        sid: server, rk: "landed-row".into(), title: "Landed row".into(), ..Default::default()
    });
    crate::search::land_for_test(&first.stores.search.adapter_for_test(), idx, gen, item);

    // Bridge B pumps FIRST: if the two owners shared an adapter, this is the call that would have
    // picked A's landing up.
    second.stores.search.pump(0.0);
    assert!(second.stores.search.snapshot().view().shelves().is_empty(),
        "Bridge B must not see a row landed only in Bridge A's adapter");

    assert!(first.stores.search.pump(0.0), "Bridge A's own pump must land its own row");
    let shelves = first.stores.search.snapshot().view().shelves().to_vec();
    assert!(shelves.iter().flat_map(|s| &s.items).any(|it| matches!(it,
        crate::search::Item::Media(m) if m.rk == "landed-row")),
        "Bridge A's pump must land the row addressed to its own adapter");

    let second_notices = second.stores.take_notices();
    let first_notices = first.stores.take_notices();
    assert_eq!(second_notices.iter().filter(|(id, _)| *id == StoreId::Search).count(), 1,
        "Bridge B owns only its own reset notice");
    assert_eq!(first_notices.iter().filter(|(id, _)| *id == StoreId::Search).count(), 1,
        "Bridge A's own notices survive Bridge B draining its own queue");
}

/// The other half of the contract's Required #3: `Reset` must rotate the live adapter so a worker
/// spawned before the reset can only land into the retired `Arc`, never the replacement's mailbox
/// — the same fetch/adapter-bundling pattern `person.rs`'s
/// `person_reset_rotates_the_adapter_and_fences_a_late_old_worker` pins for Person. Landing into an
/// adapter NOTHING has spawned against (the shape this test had before) passes even without
/// rotation, since `reset()` also clears every mailbox on the CURRENT adapter regardless — that
/// assertion cannot tell "rotated" from "cleared in place" apart. Landing into the RETIRED
/// `old_adapter`, at the POST-reset generation, is the one scenario only rotation defeats: without
/// it `old_adapter` and the store's live adapter are the same `Arc`, so this late "worker" writes
/// straight into the mailbox the next pump reads. Simulated RED the same way: temporarily skip the
/// `Arc::new(Default::default())` rotation in `SearchStore::run`'s `Reset` arm and this landing
/// reaches the pump (`pump` returns `true`, the row is in the snapshot's shelves) — that unrotated
/// shape no longer exists on disk to run directly, so the red is simulated per
/// `reproduce-before-fixing`.
#[test]
fn search_reset_rotates_the_adapter_and_fences_a_late_old_worker() {
    let _guard = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let server = crate::plex::register_for_test(
        "bridge-search-late-worker", "127.0.0.1", 12, "synthetic", "late-worker");
    let _cleanup = DirectoryPolicyCleanup;
    let mut bridge = Bridge::for_test(|| 0);
    let old_adapter = bridge.stores.search.adapter_for_test();

    bridge.search_run(crate::stores::search::SearchCmd::Reset);
    let new_adapter = bridge.stores.search.adapter_for_test();
    assert!(!std::sync::Arc::ptr_eq(&old_adapter, &new_adapter),
        "reset must rotate the Search worker adapter");

    // A worker spawned before the reset captured `old_adapter`, not `bridge`'s (now different)
    // live one; simulate it finishing late by landing straight into the RETIRED `Arc`, at the
    // generation the store carries right now (i.e. exactly the generation a real late completion
    // would still match, if it could reach the current adapter at all).
    let gen = bridge.stores.search.query_gen();
    let idx = server.raw() as usize;
    let item = crate::search::Item::Media(crate::pms::PmsMovie {
        sid: server, rk: "late-row".into(), title: "Late row".into(), ..Default::default()
    });
    crate::search::land_for_test(&old_adapter, idx, gen, item);

    // The retired adapter is orphaned, not observed: `pump` only ever drains the CURRENT adapter,
    // so a worker that captured `old_adapter` before the reset has nothing left to land into.
    assert!(!bridge.stores.search.pump(0.0), "an idle rotated adapter has nothing to land");
    assert!(bridge.stores.search.snapshot().view().shelves().is_empty(),
        "a late landing into the retired adapter must never reach the current store's shelves");
}

/// The account-menu lift is a second PAINT of this bridge's captured chrome, not a second
/// publication. Changing the process globals after A captured must not make A draw B's chip.
#[test]
fn two_bridges_keep_their_own_captured_profile_and_labels_for_a_scrim_lift() {
    let _guard = crate::testlock::serial();
    struct Restore(std::sync::Arc<crate::plex::session::CurrentProfile>);
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::plex::session::publish_profile_for_test(self.0.user.clone(), self.0.generation);
        }
    }
    let _restore = Restore(crate::plex::session::current_snapshot());
    let mut a = Bridge::for_test(|| 0);
    let mut b = Bridge::for_test(|| 0);
    a.seed_chrome_for_test("Owner A", "A", &["Home", "Movies", ""]);
    b.seed_chrome_for_test("Owner B", "B", &["Home", "TV Shows", ""]);
    crate::plex::session::publish_profile_for_test(Some(crate::plex::session::UserRef {
        title: "Global B".into(),
        ..Default::default()
    }), 41);

    let ar = <Bridge as crate::ui::dispatch::Rig<AppHost>>::scrim_chrome_read(&a)
        .expect("a bar-wearing bridge publishes lift chrome");
    let br = <Bridge as crate::ui::dispatch::Rig<AppHost>>::scrim_chrome_read(&b)
        .expect("the second bridge publishes its own lift chrome");
    assert_eq!(ar.profile.name.to_bytes(), b"Owner A");
    assert_eq!(ar.profile.initial.to_bytes(), b"A");
    assert_eq!(ar.labels.labels, ["Home", "Movies", ""]);
    assert_eq!(br.profile.name.to_bytes(), b"Owner B");
    assert_eq!(br.labels.labels, ["Home", "TV Shows", ""]);
}
