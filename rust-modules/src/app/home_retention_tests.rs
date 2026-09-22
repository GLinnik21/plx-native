//! Home/library grid retention and viewport restore after stack eviction, plus home/library
//! worker-result addressing and directory-discovery landing order.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::{frame, frame_with_tap, frame_with_results};

#[test]
fn home_requests_keep_the_emitting_instance_and_captured_return_memory() {
    use crate::screens::registry::{HomeGroupKey, HomeHubIdentity, HomeItemIdentity, HomeItemKey, HomeMemory, HomeTab};
    let _guard = crate::testlock::serial();
    let mut rig = Bridge::for_test(|| 0);
    let sid = crate::plex::ServerId::UNSET;
    let groups = vec![HomeHubIdentity::ContinueWatching,
        HomeHubIdentity::Identifier { sid, id: "recent".into(), key: "/hubs/recent".into() },
        HomeHubIdentity::Key { sid, key: "/library/collections/7/children".into() },
        HomeHubIdentity::Ephemeral { generation: 9, ordinal: 0 }];
    let memory = HomeMemory {
        groups: groups.iter().enumerate().map(|(i, identity)| HomeGroupKey { identity: identity.clone(), group: i as u32 }).collect(),
        items: vec![
            HomeItemKey { elem: 10, identity: HomeItemIdentity::Item { hub: groups[0].clone(), sid, rk: "1".into() }, last_row: 0, last_col: 0 },
            HomeItemKey { elem: 11, identity: HomeItemIdentity::Slot { hub: groups[3].clone(), generation: 9, ordinal: 0 }, last_row: 0, last_col: 0 },
        ], next_elem: 12, next_group: 4, carousel: Some((sid, "1".into())), strip_chosen: false,
        ..Default::default()
    };
    let ret = ReturnState { memory: PageMemory::Home(memory), ..Default::default() };
    let source = MachineId::Instance(InstanceId(20));
    rig.app_return(source, ret.clone());
    let requests = vec![HomeReq::Play { sid, rk: "1".into(), resume_ns: 1_000_000 },
        HomeReq::Detail { sid, rk: "1".into() }, HomeReq::ItemMenu { sid, rk: "1".into() }, HomeReq::Account,
        HomeReq::Tab(HomeTab::Home), HomeReq::Tab(HomeTab::Movies), HomeReq::Tab(HomeTab::Shows), HomeReq::Tab(HomeTab::Search)];
    let parts = CxParts { tick: tick(0), press: Default::default(), focus: Default::default(), owner: InputOwner::Entry(EntryId(1)) };
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, source, &mut present);
    for request in &requests { rig.app_fx(source, AppFx::Home(request.clone()), &parts, &mut fx); }
    let queued = rig.take_home_reqs();
    assert_eq!(queued.len(), requests.len());
    for ((from, request, saved), expected) in queued.iter().zip(requests) {
        assert_eq!(*from, source);
        assert_eq!(*request, expected);
        assert_eq!(saved.memory.hash(), ret.memory.hash());
    }
    assert!(rig.take_home_reqs().is_empty());
    let split = rig.split();
    let cx = parts.cx::<AppHost>(split.views, split.measure);
    assert_eq!(<AppHost as HomeLike>::hubs(&cx).generation, split.views.hubs.generation);
    assert_eq!(crate::screens::registry::word::HOME, "home");
}

#[test]
fn removed_home_items_recover_near_their_old_slot_after_live_or_evicted_return() {
    let _guard = crate::testlock::serial();
    for evict in [false, true] {
        for (row, col, removed_hub, reordered, expected_row, expected_col, expected_rk) in [
            (1, 2, false, false, 1, 2, "4"),
            (1, 3, false, false, 1, 2, "3"),
            (2, 2, true, false, 1, 2, "3"),
            (1, 2, false, true, 1, 2, "1"),
        ] {
            let mut d = Dispatcher::<AppHost>::new();
            let mut rig = Bridge::for_test(|| 0);
            rig.stores.hubs.seed_grid_for_test(3, 4);
            frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
            if reordered { rig.stores.hubs.reverse_test_shelves(); }
            frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
            let entry = d.nav.top_page().unwrap().id;
            let instance = d.nav.instance_of(entry).unwrap();
            d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
                Delivery::Screen(ScreenEvent::App(AppMsg::Home(HomeCmd::FocusGrid { row, col })))));
            for i in 2..40 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
            let removed_rk = rig.with_home(&d, |home, cx, focus|
                home.focused_item::<AppHost>(focus, cx).unwrap().rk.clone()).unwrap();
            d.nav.tabs.stack.transition = Box::new(crate::ui::containers::transition::Immediate);
            let count = if evict { crate::ui::containers::stack::CAP + 1 } else { 1 };
            for i in 0..count {
                d.request(MachineId::Nav, NavOp::Push(AppArg::Library));
                let report = d.frame_with(&mut rig, tick(40 + i as u32), vec![], vec![], &mut NoTap, false);
                d.prune(&report.unmounted);
            }
            assert_eq!(d.nav.entry(entry).unwrap().inst.is_none(), evict);
            if removed_hub { rig.stores.hubs.seed_grid_for_test(2, 4); }
            else { rig.stores.hubs.remove_test_item(&removed_rk); }
            rig.capture_views(&mut d);
            d.request(MachineId::Nav, NavOp::PopTo(entry));
            let report = d.frame_with(&mut rig, tick(80), vec![], vec![], &mut NoTap, false);
            d.prune(&report.unmounted);
            rig.with_home(&d, |home, cx, focus| {
                assert_eq!(home.grid_position::<AppHost>(focus, cx), Some((expected_row, expected_col)),
                    "evict={evict}, old=({row},{col}), removed_hub={removed_hub}");
                assert_eq!(home.focused_item::<AppHost>(focus, cx).unwrap().rk, expected_rk);
            }).unwrap();
        }
    }
}

#[test]
fn mounted_home_navigation_stays_inside_a_full_or_oversized_catalog() {
    let _guard = crate::testlock::serial();
    for offered in [crate::pms::MAX_SHELVES, crate::pms::MAX_SHELVES + 5] {
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        rig.stores.hubs.seed_grid_for_test(offered, 3);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
        let mut i = 2;
        for row in 0..crate::pms::MAX_SHELVES {
            frame(&mut d, &mut rig, AppArg::Home, tick(i), script_key(Key::Down, tick(i)));
            i += 1;
            assert_eq!(rig.with_home(&d, |home, cx, focus| home.grid_position::<AppHost>(focus, cx)),
                Some(Some((row, 0))), "offered={offered}, row={row}");
        }
        let last = d.focus();
        for _ in 0..3 {
            frame(&mut d, &mut rig, AppArg::Home, tick(i), script_key(Key::Down, tick(i)));
            i += 1;
            assert_eq!(d.focus(), last, "DOWN cannot escape the capped final shelf");
        }
        let instance = d.nav.instance_of(last.unwrap().entry).unwrap();
        for (row, col) in [(usize::MAX, 0), (0, usize::MAX)] {
            d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
                Delivery::Screen(ScreenEvent::App(AppMsg::Home(HomeCmd::FocusGrid { row, col })))));
            frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]);
            i += 1;
            assert_eq!(d.focus(), last, "an invalid addressed request must not displace focus");
        }
    }
}

#[test]
fn hero_edge_keys_page_without_seating_a_pager_or_leaving_the_control() {
    let _guard = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.hubs.seed_for_test(3, crate::pms::HubState::Ready);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
    let selected = |rig: &Bridge, d: &Dispatcher<AppHost>| rig.with_home(d,
        |home, cx, _| home.hero_item::<AppHost>(cx).unwrap().rk.clone()).unwrap();
    let mut i = 2;
    for (direction, control) in [(Key::Left, 0), (Key::Right, 1), (Key::Right, 1)] {
        if control == 1 && d.focus().unwrap().elem == 0 {
            frame(&mut d, &mut rig, AppArg::Home, tick(i), script_key(Key::Right, tick(i)));
            i += 1;
        }
        let before = selected(&rig, &d);
        frame(&mut d, &mut rig, AppArg::Home, tick(i), script_key(direction, tick(i)));
        i += 1;
        assert_eq!(d.focus().unwrap().elem, control);
        assert_ne!(selected(&rig, &d), before, "the delivered edge must page, not just report an edge");
        rig.with_home(&d, |home, cx, _| {
            let mut draw = DrawFrame::new(cx, crate::ui::Painter::root());
            home.record_stops(&mut draw, cx.views.hubs);
            let mut keys: Vec<_> = draw.into_stops().into_iter().map(|s| s.key.elem).collect();
            keys.sort();
            assert_eq!(keys, vec![0, 1], "pager chevron/dots are not hit stops");
        }).unwrap();
        for _ in 0..30 {
            frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]);
            i += 1;
        }
    }
}

#[test]
fn a_removed_home_type_tab_recovers_to_home_not_the_profile_chip() {
    let _guard = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.hubs.seed_for_test(3, crate::pms::HubState::Ready);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
    let entry = d.nav.top_page().unwrap().id;
    let movies = crate::screens::home::STRIP_MOVIES_ELEM;
    d.nav.tabs.strip.push(crate::ui::containers::tabs::StripMember::new(movies,
        crate::ui::Rect::new(800.0, 50.0, 160.0, 60.0)));
    d.set_focus_in(Some(FocusKey { entry, elem: movies }), Some(crate::ui::containers::tabs::STRIP));
    // Republish the current empty-library strip: the previous Movies destination is gone.
    frame(&mut d, &mut rig, AppArg::Home, tick(2), vec![]);
    assert!(!d.nav.tabs.strip.iter().any(|member| member.elem == movies));
    assert_eq!(d.focus(), Some(FocusKey { entry, elem: crate::screens::home::STRIP_HOME_ELEM }));
}

#[test]
fn home_return_restores_the_offscreen_item_and_viewport_after_retention_or_eviction() {
    let _guard = crate::testlock::serial();
    for (evict, reorder) in [(true, false), (false, false), (true, true), (false, true)] {
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        rig.stores.hubs.seed_grid_for_test(6, 24);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        d.nav.tabs.stack.transition = Box::new(crate::ui::containers::transition::Immediate);
        let home = d.nav.top_page().unwrap().id;
        let instance = d.nav.instance_of(home).unwrap();
        d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
            Delivery::Screen(ScreenEvent::App(AppMsg::Home(HomeCmd::FocusGrid { row: 4, col: 18 })))));
        for i in 1..80 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
        for i in 80..84 { frame(&mut d, &mut rig, AppArg::Home, tick(i), script_key(Key::Left, tick(i))); }
        for i in 84..160 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
        let focus = d.focus().unwrap();
        let before = rig.with_home(&d, |s, cx, f| {
            assert_eq!(s.grid_position::<AppHost>(f, cx), Some((4, 14)));
            s.focused_rect::<AppHost>(f, cx, At::Drawn).unwrap()
        }).unwrap();
        let count = if evict { crate::ui::containers::stack::CAP + 1 } else { 1 };
        for i in 0..count {
            d.request(MachineId::Nav, NavOp::Push(AppArg::Library));
            let report = d.frame_with(&mut rig, tick(160 + i as u32), vec![], vec![], &mut NoTap, false);
            d.prune(&report.unmounted);
        }
        assert_eq!(d.nav.entry(home).unwrap().inst.is_none(), evict);
        if reorder {
            rig.stores.hubs.reverse_test_hubs();
            rig.capture_views(&mut d);
        }
        d.request(MachineId::Nav, NavOp::PopTo(home));
        for i in 200..280 {
            let report = d.frame_with(&mut rig, tick(i), vec![], vec![], &mut NoTap, false);
            d.prune(&report.unmounted);
            if i == 200 {
                rig.with_home(&d, |s, cx, _| {
                    let mut draw = DrawFrame::new(cx, crate::ui::Painter::root());
                    s.record_stops(&mut draw, cx.views.hubs);
                    let stops = draw.into_stops();
                    let stop = stops.iter().find(|stop| stop.key == focus)
                        .expect("the restored card must be interactive on the first returned frame");
                    assert!(stop.rect.x < stop.clip.x + stop.clip.w && stop.rect.x + stop.rect.w > stop.clip.x
                        && stop.rect.y < stop.clip.y + stop.clip.h && stop.rect.y + stop.rect.h > stop.clip.y,
                        "the restored stop must actually be onscreen: x={} y={}", stop.rect.x, stop.rect.y);
                }).unwrap();
            }
        }
        assert_eq!(d.focus(), Some(focus));
        let after = rig.with_home(&d, |s, cx, f| {
            assert_eq!(s.snap_target(), 1.0, "restored grid must not remain behind the hero");
            s.focused_rect::<AppHost>(f, cx, At::Drawn).unwrap()
        }).unwrap();
        assert!((before.x - after.x).abs() < 1.0, "horizontal viewport changed: {} -> {}", before.x, after.x);
        if !reorder {
            assert!((before.y - after.y).abs() < 1.0, "vertical viewport changed: {} -> {}", before.y, after.y);
        }
    }
}

#[test]
fn library_detail_return_restores_engine_card_and_viewport_after_stack_eviction() {
    let _guard = crate::testlock::serial();
    struct RegistryCleanup;
    impl Drop for RegistryCleanup {
        fn drop(&mut self) { crate::plex::reset_servers_for_test(); }
    }
    let _cleanup = RegistryCleanup;
    let session = crate::plex::session::TempSession::new("owned-library-return");
    session.watching("u-owned-library-return");
    for evict in [false, true] {
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test("library-return-own", "127.0.0.1", 9, "synthetic", "fixture");
        let shared = crate::plex::register_for_test("library-return-shared", "127.0.0.1", 10, "synthetic", "fixture");
        crate::plex::set_current(sid);
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        rig.stores.browse.borrow_mut().seed_registered_table_for_test([sid, shared]);
        rig.refresh_browse_directory();
        rig.browse_run(crate::stores::browse::BrowseCmd::SetCur(0));
        rig.stores.browse.borrow_mut().seed_items_for_test(120);
        frame(&mut d, &mut rig, AppArg::Library, tick(0), vec![]);
        d.nav.tabs.stack.transition = Box::new(crate::ui::containers::transition::Immediate);
        Bridge::library_command(&mut d, crate::screens::registry::LibraryCmd::FocusGrid { row: 8, col: 4 });
        for i in 1..80 { frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]); }
        let entry = d.nav.top_page().unwrap().id;
        let focus = d.focus().unwrap();
        let (item, opener) = rig.library_selection(&d, entry, Some(focus)).unwrap_or_else(|| panic!(
            "fixture must reach grid: focus={focus:?}, listing={:?}, total={}, current={:?}",
            rig.listing.view().id(), rig.listing.view().total(), rig.directory.view().current()));
        let before = opener.rect.unwrap();
        let count = if evict { crate::ui::containers::stack::CAP + 1 } else { 1 };
        for i in 0..count {
            d.request(MachineId::Nav, NavOp::Push(AppArg::Content(ContentArg::Detail {
                sid: item.sid, rk: if i == 0 { item.rk.clone() } else { format!("return-{i}") },
            })));
            let report = d.frame_with(&mut rig, tick(80 + i as u32), vec![], vec![], &mut NoTap, false);
            d.prune(&report.unmounted);
        }
        assert_eq!(d.nav.entry(entry).unwrap().inst.is_none(), evict);
        d.request(MachineId::Nav, NavOp::PopTo(entry));
        for i in 100..180 {
            let report = d.frame_with(&mut rig, tick(i), vec![], vec![], &mut NoTap, false);
            d.prune(&report.unmounted);
            assert_eq!(d.focus(), Some(focus), "every post-return frame keeps the engine card; evicted={evict}");
            let (returned, opener) = rig.library_selection(&d, entry, d.focus()).unwrap();
            assert_eq!((returned.sid, returned.rk.as_str()), (item.sid, item.rk.as_str()));
            let after = opener.rect.unwrap();
            assert!((before.x - after.x).abs() < 0.01 && (before.y - after.y).abs() < 0.01,
                "return geometry changed: {before:?} -> {after:?}, evicted={evict}, frame={i}");
        }
    }
}

#[test]
fn library_publishes_the_actual_container_strip() {
    let _guard = crate::testlock::serial();
    let mut dispatcher = Dispatcher::<AppHost>::new();
    let mut bridge = Bridge::for_test(|| 0);
    bridge.stores.browse.borrow_mut().seed_two_source_table_for_test();
    bridge.refresh_browse_directory();
    super::show_page(&mut dispatcher, AppArg::Library);
    bridge.capture_chrome(&mut dispatcher);
    let base = crate::ui::dispatch::STRIP_BASE;
    assert_eq!(dispatcher.nav.tabs.strip.iter().map(|member| member.elem).collect::<Vec<_>>(),
        vec![base + 4, base, base + 1, base + 2, base + 3]);
    assert_eq!(dispatcher.nav.tabs.strip_fallback, Some(base));
}

#[test]
fn all_splits_in_one_frame_keep_the_same_library_listing() {
    use crate::ui::dispatch::Rig;
    let _guard = crate::testlock::serial();
    let mut rig = super::Bridge::for_test(|| 0);
    {
        let mut browse = rig.stores.browse.borrow_mut();
        browse.seed_two_source_table_for_test();
        browse.seed_items_for_test(3);
        browse.seed_shelves_for_test(0, &["shelves"], 3);
    }
    let mut dispatcher = Dispatcher::<AppHost>::new();
    rig.capture_views(&mut dispatcher);
    let id = rig.split().views.listing.id();
    {
        let split = rig.split();
        assert_eq!(split.views.listing.total(), 3);
        assert_eq!(split.views.directory.sections().len(), 4);
        assert_eq!(split.views.section_hubs.shelves()[0].items.len(), 3);
    }
    let retained = (rig.listing.clone(), rig.directory.clone(), rig.section_hubs.clone());
    rig.browse_run(crate::stores::browse::BrowseCmd::Reset);
    assert!(retained.0.view().item(2).is_some());
    assert_eq!(retained.2.view().shelves()[0].items.len(), 3);
    assert_eq!(retained.1.view().sections().len(), 4);
    assert_eq!(rig.split().views.listing.id(), id);
    assert_eq!(rig.split().views.listing.total(), 3);
    assert_eq!(rig.split().views.section_hubs.shelves()[0].items.len(), 3);
    assert_eq!(rig.split().views.directory.sections().len(), 4);
    rig.capture_views(&mut dispatcher);
    assert!(rig.split().views.listing.id().is_none());
    assert_eq!(rig.split().views.listing.total(), -1);
    assert!(rig.split().views.section_hubs.id().is_none());
    assert!(rig.split().views.directory.sections().is_empty());
}

#[test]
fn all_splits_in_one_frame_keep_the_same_home_publication() {
    use crate::ui::dispatch::Rig;
    let _guard = crate::testlock::serial();
    let mut rig = super::Bridge::for_test(|| 0);
    let mut dispatcher = Dispatcher::<AppHost>::new();
    rig.stores.hubs.seed_for_test(3, crate::pms::HubState::Ready);
    rig.capture_views(&mut dispatcher);
    assert_eq!(rig.split().views.hubs.hub(0).unwrap().items.len(), 3);
    // `split()` reads `rig.hubs`, the last-captured publication — resetting the live store
    // underneath it must not retroactively change what an already-taken split saw.
    let _ = rig.stores.hubs.run(crate::stores::hubs::HubsCmd::Reset);
    assert_eq!(rig.split().views.hubs.hub(0).unwrap().items.len(), 3);
    assert_eq!(rig.split().views.hubs.hub_count(), 1,
        "a post-step draw must not pair new data with the old element projection");
    rig.capture_views(&mut dispatcher);
    assert_eq!(rig.split().views.hubs.hub_count(), 0, "the next frame adopts the publication");
}

#[test]
fn removing_the_pressed_home_item_cancels_instead_of_activating_its_replacement() {
    let _guard = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.hubs.seed_for_test(3, crate::pms::HubState::Ready);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    frame(&mut d, &mut rig, AppArg::Home, tick(1), script_key(Key::Down, tick(1)));
    for i in 2..40 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
    let pressed = d.focus().unwrap();
    let mut down = script_key(Key::Ok, tick(40));
    down.truncate(1);
    frame(&mut d, &mut rig, AppArg::Home, tick(40), down);
    assert_eq!(d.input.arm.unwrap().key, pressed);
    rig.stores.hubs.remove_test_item("1");
    frame(&mut d, &mut rig, AppArg::Home, tick(41), vec![]);
    assert_ne!(d.focus(), Some(pressed));
    frame(&mut d, &mut rig, AppArg::Home, tick(42), vec![release_input(tick(42))]);
    for i in 43..80 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
    assert!(rig.take_home_reqs().is_empty(), "a removed arm must not become a press on the replacement cursor");
}

#[test]
fn a_midframe_reorder_keeps_painted_keys_matched_and_a_click_activates_the_seen_item() {
    let _guard = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.hubs.seed_for_test(3, crate::pms::HubState::Ready);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    frame(&mut d, &mut rig, AppArg::Home, tick(1), script_key(Key::Down, tick(1)));
    for i in 2..40 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
    let focus = d.focus().unwrap();
    let (stops, rect) = rig.with_home(&d, |home, cx, _| {
        assert_eq!(home.focused_item::<AppHost>(Some(focus), cx).unwrap().rk, "1");
        let mut draw = DrawFrame::new(cx, crate::ui::Painter::root());
        home.record_stops::<AppHost>(&mut draw, cx.views.hubs);
        (draw.into_stops(), home.focused_rect::<AppHost>(Some(focus), cx, At::Drawn).unwrap())
    }).unwrap();
    d.input.hit.fill(stops);
    d.input.hit.swap();

    rig.stores.hubs.reverse_test_shelves();
    assert_eq!(rig.stores.hubs.hub_item_for_test(0, 0).unwrap().rk, "3");
    let _ = rig.split(); // the subsequent draw still belongs to this frame's publication
    assert_eq!(rig.with_home(&d, |home, cx, _| home.focused_item::<AppHost>(Some(focus), cx).unwrap().rk.clone()), Some("1".into()));

    frame(&mut d, &mut rig, AppArg::Home, tick(40), vec![click_input(rect.cx(), rect.cy(), tick(40))]);
    frame(&mut d, &mut rig, AppArg::Home, tick(41), vec![release_input(tick(41))]);
    for i in 42..60 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
    let requests = rig.take_home_reqs();
    assert!(requests.iter().any(|(_, request, _)| matches!(request, HomeReq::Play { rk, .. } if rk == "1")),
        "the old presented map named item 1, not the replacement now at its old position");
    assert!(!requests.iter().any(|(_, request, _)| matches!(request, HomeReq::Play { rk, .. } if rk == "3")));
}

#[test]
fn home_worker_results_cross_the_addressed_dispatcher_ingest_once() {
    use crate::ui::dispatch::Tap;
    use crate::ui::machine::Addr;
    #[derive(Default)]
    struct Results(Vec<Addr>);
    impl Tap<AppHost> for Results {
        fn result(&mut self, _f: u64, addr: &Addr, msg: &AppMsg) {
            let AppMsg::HubsResult(result) = msg else { panic!("wrong result type") };
            assert_eq!(addr.req.0, result.request_id());
            self.0.push(*addr);
        }
    }
    let _guard = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.hubs.seed_for_test(2, crate::pms::HubState::Ready);
    let mut tap = Results::default();
    let req = rig.stores.hubs.queue_test_landing(Some(5));
    frame_with_tap(&mut d, &mut rig, AppArg::Home, tick(0), vec![], &mut tap);
    assert_eq!(rig.stores.hubs.hub_len_for_test(0), 5);
    assert_eq!(tap.0, vec![Addr {
        to: MachineId::Store(StoreId::Hubs.ord()), req: crate::ui::machine::RequestId(req),
    }]);
    frame_with_tap(&mut d, &mut rig, AppArg::Home, tick(1), vec![], &mut tap);
    assert_eq!(tap.0.len(), 1, "the store tick must not re-deliver the result");

    rig.stores.hubs.queue_test_landing(Some(9));
    let result = rig.stores.hubs.take_results().pop().unwrap();
    let parts = CxParts { tick: tick(2), press: Default::default(), focus: Default::default(),
        owner: crate::ui::machine::InputOwner::Entry(EntryId(0)) };
    let mut out = Vec::new();
    let mut present = crate::ui::present::Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Nav, &mut present);
    assert_eq!(rig.deliver(MachineId::Store(StoreId::Search.ord()),
        &AppMsg::HubsResult(result), &parts, &mut fx), Handled::No);
    assert_eq!(rig.stores.hubs.hub_len_for_test(0), 5, "a misaddressed result must not apply");
}

#[test]
fn one_home_landing_notifies_the_home_screen_once() {
    use crate::ui::dispatch::Tap;

    #[derive(Default)]
    struct HubsNotices(u32);

    impl Tap<AppHost> for HubsNotices {
        fn effect(&mut self, _frame: u64, stamped: &crate::ui::machine::Stamped<AppHost>) {
            if matches!(
                &stamped.fx,
                Fx::Deliver(
                    MachineId::Instance(_),
                    Delivery::Screen(ScreenEvent::StoreChanged(store, _)),
                ) if *store == StoreId::Hubs.ord()
            ) {
                self.0 += 1;
            }
        }
    }

    let _guard = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.hubs.seed_for_test(2, crate::pms::HubState::Ready);
    let mut tap = HubsNotices::default();
    frame_with_tap(&mut d, &mut rig, AppArg::Home, tick(0), vec![], &mut tap);
    let baseline = tap.0;

    rig.stores.hubs.queue_test_landing(Some(5));
    frame_with_tap(&mut d, &mut rig, AppArg::Home, tick(1), vec![], &mut tap);
    frame_with_tap(&mut d, &mut rig, AppArg::Home, tick(2), vec![], &mut tap);

    assert_eq!(tap.0, baseline + 1, "one Hubs landing must notify Home exactly once");
}

#[test]
fn supplied_home_results_use_the_dispatcher_without_consuming_live_arrivals() {
    let _guard = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.hubs.seed_for_test(2, crate::pms::HubState::Ready);
    rig.stores.hubs.queue_test_landing(Some(5));
    let captured = rig.take_hubs_results().pop().unwrap();
    let AppMsg::HubsResult(result) = captured.1 else { unreachable!() };
    let payload = crate::pms::record::encode(&result);
    let decoded = crate::pms::record::decode(payload, |_| None).unwrap();
    rig.stores.hubs.queue_test_landing(Some(9));

    frame_with_results(&mut d, &mut rig, AppArg::Home, tick(0), vec![],
        || vec![(captured.0, AppMsg::HubsResult(decoded))], &mut NoTap);
    assert_eq!(rig.stores.hubs.hub_len_for_test(0), 5, "the decoded result reaches the actual store");
    frame_with_results(&mut d, &mut rig, AppArg::Home, tick(1), vec![],
        Vec::new, &mut NoTap);
    assert_eq!(rig.stores.hubs.hub_len_for_test(0), 5, "an empty supplied frame cannot fall back to live data");
    frame_with_tap(&mut d, &mut rig, AppArg::Home, tick(2), vec![], &mut NoTap);
    assert_eq!(rig.stores.hubs.hub_len_for_test(0), 9, "the live arrival was preserved for a live ingest");
}

#[test]
fn store_work_is_addressed_and_idle_polling_does_not_invent_a_change() {
    use crate::stores::StoreWork;
    let _guard = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.hubs.seed_for_test(0, crate::pms::HubState::Ready);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    let before = rig.stores.gen(StoreId::Hubs);
    d.emit(MachineId::Nav, Fx::App(AppFx::StoreWork(StoreWork::Hubs)));
    frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
    assert_eq!(rig.stores.gen(StoreId::Hubs), before);

    let parts = CxParts { tick: tick(2), press: Default::default(), focus: Default::default(),
        owner: crate::ui::machine::InputOwner::Entry(EntryId(0)) };
    let mut out = Vec::new();
    let mut present = crate::ui::present::Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Nav, &mut present);
    for work in [StoreWork::Hubs, StoreWork::BrowseDiscovery] {
        assert_eq!(rig.deliver(MachineId::Store(StoreId::Search.ord()),
            &AppMsg::StoreWork(work), &parts, &mut fx), Handled::No);
    }
}

#[test]
fn onboard_frame_lands_owned_discovery_before_capturing_its_directory() {
    let _guard = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test(
        "onboard-same-tick", "127.0.0.1", 9, "synthetic", "fixture");
    let client = crate::plex::client_for(sid).unwrap();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.browse.borrow_mut().queue_discovery_for_test(
        client, client.token_gen(), true);

    assert!(rig.browse_directory().sources().is_empty(),
        "the queued result has not been published before the frame");
    frame(&mut d, &mut rig, AppArg::Onboard, tick(0), vec![]);

    assert_eq!(rig.browse_directory().sources().len(), 1,
        "the pre-capture owner pump publishes discovery to Onboard in the same tick");
    assert_eq!(rig.browse_directory().sources()[0].0, sid);
    assert_eq!(rig.browse_directory().discovery(), crate::browse::SecFetch::Ready);
    crate::plex::reset_servers_for_test();
}

#[test]
fn controlled_discovery_recaptures_the_directory_in_its_delivery_turn() {
    let _guard = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let mut rig = Bridge::for_test(|| 0);
    let sid = crate::plex::register_for_test(
        "same-turn-discovery", "127.0.0.1", 9, "synthetic", "fixture");
    let client = crate::plex::client_for(sid).unwrap();
    rig.stores.browse.borrow_mut().queue_discovery_for_test(
        client, client.token_gen(), true);
    let result = rig.stores.browse.borrow_mut().take_discovery().unwrap();
    let mt = unsafe { crate::task::MainThread::assume() };
    let publisher = crate::plex::session::ProfilePublisher::scoped(&mt);
    rig.home_io = Some(crate::app::HomeIo {
        replay: false,
        preferences: Default::default(),
        requests: Vec::new(),
        admissions: Default::default(),
        failure: None,
        profile: publisher.snapshot(),
    });
    assert!(rig.browse_directory().sources().is_empty());
    let parts = CxParts { tick: tick(0), press: Default::default(), focus: Default::default(),
        owner: InputOwner::Entry(EntryId(0)) };
    let mut present = Present::new();
    let mut effects = Vec::new();
    let mut fx = Effects::new(
        &mut effects, MachineId::Store(StoreId::Browse.ord()), &mut present);

    assert_eq!(rig.deliver(MachineId::Store(StoreId::Browse.ord()),
        &AppMsg::Store(StoreCmd::Browse(crate::stores::browse::BrowseCmd::Discovery(result))),
        &parts, &mut fx), Handled::Yes);
    drop(fx);

    assert_eq!(rig.browse_directory().sources().len(), 1,
        "the result is visible to the following screen Tick, not the next frame capture");
    crate::plex::reset_servers_for_test();
}
