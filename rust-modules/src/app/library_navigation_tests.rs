#[test]
fn library_strip_profile_navigation_and_armed_pill_use_the_actual_engine_identity() {
    let _guard = crate::testlock::serial();
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
            crate::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    let session = crate::plex::session::TempSession::new("library-strip-navigation");
    session.watching("u-library-strip-navigation");
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::plex::reset_servers_for_test();
    let own = crate::plex::register_for_test("strip-own", "127.0.0.1", 9, "synthetic", "fixture");
    let shared =
        crate::plex::register_for_test("strip-shared", "127.0.0.1", 10, "synthetic", "fixture");
    crate::plex::set_current(own);
    crate::browse::seed_registered_table_for_test([own, shared]);
    let mut directory = crate::stores::browse::DirectorySnapshot::default();
    directory.capture();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::SetCur(0));
    crate::browse::seed_items_for_test(12);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    for i in 0..80 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    let entry = d.nav.top_page().unwrap().id;
    let instance = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    let base = crate::ui::dispatch::STRIP_BASE;
    assert_eq!(
        d.nav
            .tabs
            .strip
            .iter()
            .map(|member| member.elem)
            .collect::<Vec<_>>(),
        vec![base + 4, base, base + 1, base + 2, base + 3]
    );
    let seat = |d: &mut Dispatcher<AppHost>, elem| {
        d.emit(
            MachineId::Nav,
            Fx::Deliver(
                MachineId::Instance(instance),
                Delivery::Screen(ScreenEvent::Enter(crate::ui::screen::Enter::Fresh {
                    focus: crate::ui::screen::FocusTarget::Elem(FocusKey { entry, elem }),
                })),
            ),
        );
    };
    seat(&mut d, base);
    frame(&mut d, &mut rig, Route::Library, tick(80), vec![]);
    for i in 81..86 {
        frame(
            &mut d,
            &mut rig,
            Route::Library,
            tick(i),
            script_key(Key::Left, tick(i)),
        );
        assert_eq!(
            d.focus(),
            Some(FocusKey {
                entry,
                elem: base + 4
            }),
            "the profile chip is the left end, not a wrapping stop"
        );
    }
    assert!(
        content_probe(&d, &rig).contains("pill=-1"),
        "the profile chip is not a remembered pill"
    );
    frame(
        &mut d,
        &mut rig,
        Route::Library,
        tick(86),
        script_key(Key::Right, tick(86)),
    );
    assert_eq!(d.focus(), Some(FocusKey { entry, elem: base }));
    frame(
        &mut d,
        &mut rig,
        Route::Library,
        tick(87),
        script_key(Key::Down, tick(87)),
    );
    let from_home = d.focus();
    assert!(
        from_home.is_some_and(|key| key.elem < base),
        "DOWN genuinely left the strip"
    );
    seat(&mut d, base + 4);
    frame(&mut d, &mut rig, Route::Library, tick(88), vec![]);
    frame(
        &mut d,
        &mut rig,
        Route::Library,
        tick(89),
        script_key(Key::Down, tick(89)),
    );
    assert_eq!(
        d.focus(),
        from_home,
        "profile and pills enter the same remembered document group"
    );
    assert!(content_probe(&d, &rig).contains("pill=-1"));

    let search = FocusKey {
        entry,
        elem: base + 3,
    };
    seat(&mut d, search.elem);
    frame(&mut d, &mut rig, Route::Library, tick(90), vec![]);
    assert_eq!(d.focus(), Some(search));
    assert_eq!(
        d.nav
            .tabs
            .strip
            .iter()
            .position(|member| member.elem == search.elem),
        Some(4),
        "Search's displayed slot is not its stable control identity"
    );
    rig.take_library_reqs();
    let key = |edge, i| {
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
        }]
    };
    frame(
        &mut d,
        &mut rig,
        Route::Library,
        tick(91),
        key(Edge::Down, 91),
    );
    assert!(d.input.press.is_live());
    assert_eq!(d.input.arm.as_ref().map(|arm| arm.key), Some(search));
    for i in 92..95 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    assert!(!rig
        .take_library_reqs()
        .iter()
        .any(|(_, req, _)| matches!(req, LibraryReq::Tab(_))));
    frame(
        &mut d,
        &mut rig,
        Route::Library,
        tick(95),
        key(Edge::Up, 95),
    );
    for i in 96..130 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    let requests = rig.take_library_reqs();
    let tabs: Vec<_> = requests
        .iter()
        .filter_map(|(_, req, ret)| match req {
            LibraryReq::Tab(tab) => Some((*tab, ret.focus)),
            _ => None,
        })
        .collect();
    assert_eq!(
        tabs,
        vec![(crate::screens::registry::HomeTab::Search, Some(search))],
        "the emitted route and captured return focus name the armed pill, not display slot 4"
    );

    let id = rig.listing.view().id().unwrap();
    d.nav.next_style = crate::ui::containers::modal::Style::Compact;
    d.request(
        MachineId::Nav,
        NavOp::Present(AppArg::LibraryMenu(
            crate::screens::registry::LibraryMenuArg {
                host: instance,
                kind: crate::screens::registry::LibraryMenuKind::Filter,
                anchor: [0; 4],
                target: crate::stores::browse::SectionAddress {
                    epoch: id.epoch,
                    sid: id.sid,
                    section: id.section,
                },
            },
        )),
    );
    for i in 130..160 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    assert_ne!(d.focus().unwrap().entry, entry);
    assert_eq!(
        d.input.engine.current(InputOwner::Entry(entry)),
        Some(search)
    );
    let probe = content_probe(&d, &rig);
    assert!(
        probe.contains("pill=-1") && probe.contains("menu=1"),
        "a modal owns focus even though the covered Library remembers Search: {probe}"
    );
}
