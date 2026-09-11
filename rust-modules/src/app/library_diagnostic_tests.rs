#[test]
fn keyboard_rail_ok_down_up_returns_without_arming_or_requesting_a_card() {
    let _guard = crate::testlock::serial();
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
            crate::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    let session = crate::plex::session::TempSession::new("library-rail-key-return");
    session.watching("u-library-rail-key-return");
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test("rail-own", "127.0.0.1", 9, "synthetic", "fixture");
    let shared =
        crate::plex::register_for_test("rail-shared", "127.0.0.1", 10, "synthetic", "fixture");
    crate::plex::set_current(sid);
    crate::browse::seed_registered_table_for_test([sid, shared]);
    let mut directory = crate::stores::browse::DirectorySnapshot::default();
    directory.capture();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::SetCur(0));
    crate::browse::seed_items_for_test(120);
    crate::browse::seed_letter_counts_for_test(&[("A", 60), ("Z", 60)]);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Library, tick(0), vec![]);
    Bridge::library_command(
        &mut d,
        crate::screens::registry::LibraryCmd::FocusGrid { row: 2, col: 5 },
    );
    for i in 1..80 {
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
    }
    let grid = d.focus().unwrap();
    frame(
        &mut d,
        &mut rig,
        AppArg::Library,
        tick(80),
        script_key(Key::Right, tick(80)),
    );
    assert_ne!(d.focus(), Some(grid));
    let page = d
        .top_screen()
        .unwrap()
        .as_any()
        .unwrap()
        .downcast_ref::<crate::screens::library::LibraryScreen>()
        .unwrap();
    assert_eq!(page.probe_viewport(d.focus()).0, "rail");
    rig.take_library_reqs();
    #[derive(Default)]
    struct PressTap(usize);
    impl crate::ui::dispatch::Tap<AppHost> for PressTap {
        fn effect(&mut self, _: u64, effect: &crate::ui::machine::Stamped<AppHost>) {
            if matches!(effect.fx, Fx::Press(_)) {
                self.0 += 1;
            }
        }
    }
    let mut tap = PressTap::default();
    for (i, edge) in [(81, Edge::Down), (82, Edge::Up)] {
        frame_with_tap(
            &mut d,
            &mut rig,
            AppArg::Library,
            tick(i),
            vec![InputEvent {
                at: tick(i),
                source: Source::Script,
                kind: InputKind::Key {
                    key: Key::Ok,
                    sym: 0,
                    wcode: 0,
                    edge,
                    at_edge: false,
                },
            }],
            &mut tap,
        );
        assert_eq!(
            d.focus(),
            Some(grid),
            "both key edges keep the exact midway-letter item"
        );
        assert!(
            !d.input.press.is_live(),
            "rail OK must not arm a card press on its returned grid item"
        );
        assert!(rig.take_library_reqs().iter().all(|(_, req, _)| !matches!(
            req,
            LibraryReq::Detail { .. } | LibraryReq::Play { .. } | LibraryReq::ItemMenu { .. }
        )));
    }
    assert_eq!(tap.0, 0, "neither keyboard edge may emit a PressArm");
}

#[test]
fn library_switch_menu_steps_drive_the_input_owning_menu_and_genre_return() {
    use crate::screens::registry::{LibraryCmd, LibraryMenuArg, LibraryMenuKind};
    let _guard = crate::testlock::serial();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Library, tick(0), vec![]);
    let host = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    d.nav.next_style = crate::ui::containers::modal::Style::Compact;
    d.request(
        MachineId::Nav,
        NavOp::Present(AppArg::LibraryMenu(LibraryMenuArg {
            host,
            kind: LibraryMenuKind::Filter,
            anchor: [0; 4],
            target: crate::stores::browse::SectionAddress {
                epoch: 1,
                sid: crate::plex::ServerId::from_raw(0),
                section: 1,
            },
        })),
    );
    for i in 1..80 {
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
    }
    let menu_entry = match d.nav.input_owner().unwrap() {
        InputOwner::Entry(entry) => entry,
        _ => unreachable!(),
    };
    let first = d.focus().unwrap();
    Bridge::library_command(&mut d, LibraryCmd::SwitchStep(8));
    frame(&mut d, &mut rig, AppArg::Library, tick(80), vec![]);
    let genre_row = d.focus().unwrap();
    assert_eq!(genre_row.entry, menu_entry);
    assert_ne!(
        genre_row, first,
        "the script moves the actual menu engine, not the covered Library"
    );
    Bridge::library_command(&mut d, LibraryCmd::SwitchStep(9));
    for i in 81..85 {
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
    }
    let row_count = |d: &Dispatcher<AppHost>, rig: &mut Bridge| {
        let parts = CxParts {
            tick: tick(85),
            press: Default::default(),
            focus: crate::ui::machine::FocusRead { current: d.focus(), ..Default::default() },
            owner: InputOwner::Entry(menu_entry),
        };
        let split = rig.split();
        let cx = parts.cx::<AppHost>(split.views, split.measure);
        let mut groups = Vec::new();
        d.nav
            .entry(menu_entry)
            .unwrap()
            .inst
            .as_ref()
            .unwrap()
            .screen
            .groups(&cx, &mut groups);
        groups[0].len
    };
    assert_eq!(row_count(&d, &mut rig), 1, "Genre has its All genres row");
    Bridge::library_command(&mut d, LibraryCmd::SwitchStep(10));
    frame(&mut d, &mut rig, AppArg::Library, tick(85), vec![]);
    assert_eq!(
        row_count(&d, &mut rig),
        2,
        "Back returns to the two Filter rows"
    );
    Bridge::library_command(&mut d, LibraryCmd::SwitchStep(11));
    for i in 86..166 {
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
    }
    assert_ne!(d.nav.input_owner(), Some(InputOwner::Entry(menu_entry)));
}

#[test]
fn library_sweep_visits_the_whole_document_and_reverses_at_its_ends() {
    let _guard = crate::testlock::serial();
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
            crate::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    let session = crate::plex::session::TempSession::new("library-diagnostic-sweep");
    session.watching("u-library-diagnostic-sweep");
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test("sweep-own", "127.0.0.1", 9, "synthetic", "fixture");
    let shared =
        crate::plex::register_for_test("sweep-shared", "127.0.0.1", 10, "synthetic", "fixture");
    crate::plex::set_current(sid);
    crate::browse::seed_registered_table_for_test([sid, shared]);
    let mut directory = crate::stores::browse::DirectorySnapshot::default();
    directory.capture();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::ApplyPins(vec![
        (0, true),
        (2, true),
    ]));
    directory.capture();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::SetCur(0));
    crate::browse::seed_items_for_test(120);
    crate::browse::section_hubs::seed_shelves_for_test(
        0,
        &["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"],
        4,
    );
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    for i in 0..80 {
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
    }
    let instance = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    d.emit(
        MachineId::Nav,
        Fx::Deliver(
            MachineId::Instance(instance),
            Delivery::Screen(ScreenEvent::Enter(crate::ui::screen::Enter::Fresh {
                focus: crate::ui::screen::FocusTarget::ContainerGroup(
                    crate::screens::library::LIBRARY_GROUP,
                ),
            })),
        ),
    );
    frame(&mut d, &mut rig, AppArg::Library, tick(80), vec![]);
    let mut visited = std::collections::BTreeSet::new();
    let mut shelves = std::collections::BTreeSet::new();
    let mut grid_rows = std::collections::BTreeSet::new();
    let mut returned = false;
    let mut path = Vec::new();
    for i in 81..281 {
        let page = d
            .top_screen()
            .unwrap()
            .as_any()
            .unwrap()
            .downcast_ref::<crate::screens::library::LibraryScreen>()
            .unwrap();
        let (region, row, _, _, _) = page.probe_viewport(d.focus());
        path.push((region, row));
        visited.insert(region);
        if region == "shelf" {
            shelves.insert(row);
        }
        if region == "grid" {
            grid_rows.insert(row);
        }
        if region == "library" && grid_rows.contains(&19) {
            returned = true;
            break;
        }
        Bridge::library_command(&mut d, crate::screens::registry::LibraryCmd::Sweep);
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
    }
    assert_eq!(
        visited,
        ["library", "shelf", "toolbar", "grid"]
            .into_iter()
            .collect(),
        "{path:?}"
    );
    assert_eq!(shelves, (0..12).collect());
    assert_eq!(grid_rows, (0..20).collect());
    assert!(
        returned,
        "the real engine sweep reverses and returns to the document head"
    );
}
