#[test]
fn leaving_after_query_commit_before_arrival_cannot_restore_the_old_query_bookmark() {
    let _guard = crate::testlock::serial();
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
            crate::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    let session = crate::plex::session::TempSession::new("library-query-leave");
    session.watching("u-library-query-leave");
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::plex::reset_servers_for_test();
    let sid =
        crate::plex::register_for_test("query-leave-own", "127.0.0.1", 9, "synthetic", "fixture");
    let shared = crate::plex::register_for_test(
        "query-leave-shared",
        "127.0.0.1",
        10,
        "synthetic",
        "fixture",
    );
    crate::plex::set_current(sid);
    crate::browse::seed_registered_table_for_test([sid, shared]);
    let mut directory = crate::stores::browse::DirectorySnapshot::default();
    directory.capture();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::SetCur(0));
    crate::browse::seed_items_for_test(120);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, Route::Home, tick(0), vec![]);
    d.nav.tabs.stack.transition = Box::new(crate::ui::containers::transition::Immediate);
    frame(&mut d, &mut rig, Route::Library, tick(1), vec![]);
    Bridge::library_command(
        &mut d,
        crate::screens::registry::LibraryCmd::FocusGrid { row: 8, col: 4 },
    );
    for i in 2..80 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    let entry = d.nav.top_page().unwrap().id;
    let id = rig.listing.view().id().unwrap();
    d.emit(
        MachineId::Nav,
        Fx::App(AppFx::Store(
            StoreId::Browse,
            crate::stores::StoreCmd::Browse(crate::stores::browse::BrowseCmd::Addressed {
                target: crate::stores::browse::SectionAddress {
                    epoch: id.epoch,
                    sid: id.sid,
                    section: id.section,
                },
                work: crate::stores::browse::LibraryWork::Commit {
                    select: false,
                    choice: false,
                    query: Some(crate::stores::browse::QueryEdit::Unwatched(true)),
                },
            }),
        )),
    );
    d.request(MachineId::Nav, NavOp::Root(AppArg::Legacy(Route::Home)));
    frame(&mut d, &mut rig, Route::Home, tick(80), vec![]);
    assert!(d.nav.entry(entry).is_none());
    let snapshot = crate::stores::browse::listing_snapshot();
    assert_ne!(snapshot.view().id().unwrap().query, id.query);
    assert_eq!(snapshot.view().total(), -1, "no new page has arrived");
    assert!(snapshot.view().cursor().is_none_or(|cursor| matches!(cursor.at, crate::browse::CursorAt::SlotIndex(0))),
        "the carried WillLeave save must not overwrite the new query with an old deep bookmark: {:?}", snapshot.view().cursor());
    rig.enter_library(crate::browse::SecKind::Movie);
    for i in 81..86 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    crate::browse::seed_items_for_test(120);
    for i in 86..166 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    let new_entry = d.nav.top_page().unwrap().id;
    assert_ne!(entry, new_entry);
    let instance = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    d.emit(
        MachineId::Nav,
        Fx::Deliver(
            MachineId::Instance(instance),
            Delivery::Screen(ScreenEvent::Enter(crate::ui::screen::Enter::Fresh {
                focus: crate::ui::screen::FocusTarget::ContainerGroup(
                    crate::screens::library::TOOLBAR_GROUP,
                ),
            })),
        ),
    );
    frame(&mut d, &mut rig, Route::Library, tick(166), vec![]);
    frame(
        &mut d,
        &mut rig,
        Route::Library,
        tick(167),
        script_key(Key::Down, tick(167)),
    );
    let (item, _) = rig.library_selection(&d, new_entry, d.focus()).unwrap();
    assert_eq!(
        item.rk, "1",
        "the new entry starts the accepted query at its first item"
    );
}

#[test]
fn a_filter_menu_resets_its_covered_library_grid_memory_without_taking_menu_focus() {
    let _guard = crate::testlock::serial();
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
            crate::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    let session = crate::plex::session::TempSession::new("library-covered-query-reset");
    session.watching("u-library-covered-query-reset");
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test("query-own", "127.0.0.1", 9, "synthetic", "fixture");
    let shared =
        crate::plex::register_for_test("query-shared", "127.0.0.1", 10, "synthetic", "fixture");
    crate::plex::set_current(sid);
    crate::browse::seed_registered_table_for_test([sid, shared]);
    let mut directory = crate::stores::browse::DirectorySnapshot::default();
    directory.capture();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::SetCur(0));
    crate::browse::seed_items_for_test(120);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, Route::Library, tick(0), vec![]);
    Bridge::library_command(
        &mut d,
        crate::screens::registry::LibraryCmd::FocusGrid { row: 8, col: 4 },
    );
    for i in 1..80 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    let entry = d.nav.top_page().unwrap().id;
    let instance = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    let grid = d.focus().unwrap();
    let group = d
        .input
        .engine
        .read(InputOwner::Entry(entry))
        .remembered
        .iter()
        .find(|(_, elem)| *elem == grid.elem)
        .unwrap()
        .0;
    d.emit(
        MachineId::Nav,
        Fx::Deliver(
            MachineId::Instance(instance),
            Delivery::Screen(ScreenEvent::Enter(crate::ui::screen::Enter::Fresh {
                focus: crate::ui::screen::FocusTarget::ContainerGroup(
                    crate::screens::library::TOOLBAR_GROUP,
                ),
            })),
        ),
    );
    frame(&mut d, &mut rig, Route::Library, tick(80), vec![]);
    frame(
        &mut d,
        &mut rig,
        Route::Library,
        tick(81),
        script_key(Key::Right, tick(81)),
    );
    let toolbar = d.focus().unwrap();
    let page = d
        .top_screen()
        .unwrap()
        .as_any()
        .unwrap()
        .downcast_ref::<crate::screens::library::LibraryScreen>()
        .unwrap();
    assert_eq!(
        page.probe_viewport(Some(toolbar)).0,
        "toolbar",
        "toolbar seat: {toolbar:?}"
    );
    rig.take_library_reqs();
    frame(
        &mut d,
        &mut rig,
        Route::Library,
        tick(82),
        script_key(Key::Ok, tick(82)),
    );
    for i in 83..106 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    let requests = rig.take_library_reqs();
    let evidence = format!(
        "focus={:?} live={} requests={:?}",
        d.focus(),
        d.input.press.is_live(),
        requests.iter().map(|(_, req, _)| req).collect::<Vec<_>>()
    );
    let (kind, anchor, target) = requests
        .into_iter()
        .find_map(|(_, req, _)| match req {
            LibraryReq::Menu {
                kind,
                anchor,
                target,
            } => Some((kind, anchor, target)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the real toolbar activation must request its menu: {evidence}"));
    assert_eq!(kind, crate::screens::registry::LibraryMenuKind::Filter);
    d.nav.next_style = crate::ui::containers::modal::Style::Compact;
    d.request(
        MachineId::Nav,
        NavOp::Present(AppArg::LibraryMenu(
            crate::screens::registry::LibraryMenuArg {
                host: instance,
                kind,
                anchor,
                target,
            },
        )),
    );
    for i in 106..140 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    let menu_focus = d.focus().unwrap();
    assert_ne!(menu_focus.entry, entry);
    let query = rig.listing.view().id().unwrap().query;
    frame(
        &mut d,
        &mut rig,
        Route::Library,
        tick(140),
        script_key(Key::Ok, tick(140)),
    );
    let mut landed_at = None;
    for i in 141..190 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
        if rig.listing.view().id().unwrap().query != query {
            landed_at = Some(i);
            break;
        }
    }
    let landed_at = landed_at.expect("the actual Filter edit must commit its new query");
    crate::browse::seed_items_for_test(120);
    for i in landed_at + 1..landed_at + 6 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    assert_eq!(
        d.focus(),
        Some(menu_focus),
        "resetting a covered group cannot take menu focus"
    );
    let PageMemory::Library(memory) = d
        .nav
        .entry(entry)
        .unwrap()
        .inst
        .as_ref()
        .unwrap()
        .screen
        .memory()
    else {
        unreachable!()
    };
    let first = memory
        .keys
        .iter()
        .find(|key| {
            matches!(&key.identity,
        crate::screens::registry::LibraryIdentity::Grid { sid: item_sid, rk, section }
            if *item_sid == sid && rk == "1" && section.key == target.section)
        })
        .unwrap()
        .elem;
    assert_eq!(
        d.input
            .engine
            .read(InputOwner::Entry(entry))
            .remembered(group),
        Some(first),
        "the covered Library's emitted remember must reach its own engine group"
    );
    frame(
        &mut d,
        &mut rig,
        Route::Library,
        tick(210),
        script_key(Key::Back, tick(210)),
    );
    for i in 211..290 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    assert_eq!(d.focus(), Some(toolbar));
    frame(
        &mut d,
        &mut rig,
        Route::Library,
        tick(290),
        script_key(Key::Down, tick(290)),
    );
    assert_eq!(d.focus(), Some(FocusKey { entry, elem: first }));
}
