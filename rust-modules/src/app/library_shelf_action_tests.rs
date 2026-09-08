#[test]
fn shelf_physical_hold_captures_engine_item_deck_flag_and_bridge_rest_opener() {
    let _guard = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("library-shelf-hold");
    session.watching("u-library-shelf-hold");
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
            crate::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    for row in 0..2 {
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
        let own =
            crate::plex::register_for_test("shelf-own", "127.0.0.1", 9, "synthetic", "fixture");
        let shared =
            crate::plex::register_for_test("shelf-shared", "127.0.0.1", 10, "synthetic", "fixture");
        crate::plex::set_current(own);
        crate::browse::seed_registered_table_for_test([own, shared]);
        let mut directory = crate::stores::browse::DirectorySnapshot::default();
        directory.capture();
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::SetCur(0));
        crate::browse::seed_items_for_test(12);
        crate::browse::section_hubs::seed_shelves_for_test(
            0,
            &["movie.inprogress.1", "tv.recentlyreleased.1"],
            3,
        );
        crate::browse::section_hubs::seed_landscape_for_test(0, "Synthetic show");
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        for i in 0..80 {
            frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
        }
        let position = |d: &Dispatcher<AppHost>| {
            d.top_screen()
                .unwrap()
                .as_any()
                .unwrap()
                .downcast_ref::<crate::screens::library::LibraryScreen>()
                .unwrap()
                .probe_viewport(d.focus())
        };
        assert_eq!(rig.section_hubs.view().shelves().len(), 2);
        let mut i = 80;
        while position(&d).0 != "shelf" || position(&d).1 != row {
            assert!(
                i < 86,
                "directional fixture must reach shelf {row}: {:?}",
                position(&d)
            );
            frame(
                &mut d,
                &mut rig,
                Route::Library,
                tick(i),
                script_key(Key::Down, tick(i)),
            );
            i += 1;
        }
        frame(
            &mut d,
            &mut rig,
            Route::Library,
            tick(i),
            script_key(Key::Right, tick(i)),
        );
        i += 1;
        for n in i..i + 80 {
            frame(&mut d, &mut rig, Route::Library, tick(n), vec![]);
        }
        i += 80;
        assert_eq!(
            (position(&d).0, position(&d).1, position(&d).2),
            ("shelf", row, 1)
        );
        let key = d.focus().unwrap();
        let (item, _) = rig.library_selection(&d, key.entry, Some(key)).unwrap();
        assert_eq!(item.kind, 3);
        assert!(item.rk.ends_with("-1"), "not the first card: {}", item.rk);
        let key_input = |edge, n| {
            vec![InputEvent {
                at: tick(n),
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
        rig.take_library_reqs();
        frame(
            &mut d,
            &mut rig,
            Route::Library,
            tick(i),
            key_input(Edge::Down, i),
        );
        assert_eq!(d.input.arm.as_ref().map(|arm| arm.key), Some(key));
        assert!(d.input.press.is_live());
        let mut requests = Vec::new();
        let mut saw_scaled_press = false;
        let rect = |r: crate::ui::Rect| [r.x, r.y, r.w, r.h];
        for n in i + 1..i + 80 {
            frame(&mut d, &mut rig, Route::Library, tick(n), vec![]);
            assert_eq!(d.focus(), Some(key));
            let page = d
                .nav
                .entry(key.entry)
                .unwrap()
                .inst
                .as_ref()
                .unwrap()
                .screen
                .as_any()
                .unwrap()
                .downcast_ref::<crate::screens::library::LibraryScreen>()
                .unwrap();
            let parts = CxParts {
                tick: tick(n),
                press: crate::ui::machine::PressRead {
                    scale: d.input.press.scale(),
                    is_long: d.input.press.was_long(),
                },
                focus: d.input.engine.read(InputOwner::Entry(key.entry)),
                owner: InputOwner::Entry(key.entry),
            };
            let cx = parts.cx::<AppHost>(
                AppViews {
                    hubs: rig.hubs.view(),
                    listing: rig.listing.view(),
                    directory: rig.directory.view(),
                    section_hubs: rig.section_hubs.view(),
                },
                rig.measure,
            );
            let placed = page.place(&key.elem, &cx, At::Drawn).unwrap();
            let (selected, opener) = rig.library_selection(&d, key.entry, Some(key)).unwrap();
            assert_eq!(
                (selected.sid, selected.rk.as_str()),
                (item.sid, item.rk.as_str())
            );
            assert_eq!(rect(opener.rect.unwrap()), rect(placed.rest_rect));
            if d.input.press.is_live() && d.input.press.scale() < 0.99 {
                saw_scaled_press = true;
                assert!(
                    placed.rect.w < placed.rest_rect.w,
                    "the bridge opener must not inherit the pressed card's shrink"
                );
            }
            requests.extend(rig.take_library_reqs().into_iter().filter(|(_, req, _)| {
                matches!(
                    req,
                    LibraryReq::ItemMenu { .. }
                        | LibraryReq::Play { .. }
                        | LibraryReq::Detail { .. }
                )
            }));
        }
        assert!(
            saw_scaled_press,
            "exercise a real armed press, not a default scale fixture"
        );
        assert_eq!(
            requests.len(),
            1,
            "hold produces one menu, never play/detail"
        );
        assert_eq!(
            requests[0].1,
            LibraryReq::ItemMenu {
                sid: item.sid,
                rk: item.rk.clone(),
                from_deck: row == 0
            }
        );
        assert_eq!(requests[0].2.focus, Some(key));
        let (selected, opener) = rig
            .library_selection(&d, key.entry, requests[0].2.focus)
            .unwrap();
        assert_eq!((selected.sid, selected.rk), (item.sid, item.rk));
        assert!(opener.rect.is_some());
        frame(
            &mut d,
            &mut rig,
            Route::Library,
            tick(i + 80),
            key_input(Edge::Up, i + 80),
        );
        for n in i + 81..i + 120 {
            frame(&mut d, &mut rig, Route::Library, tick(n), vec![]);
        }
        assert!(
            !rig.take_library_reqs().iter().any(|(_, req, _)| matches!(
                req,
                LibraryReq::ItemMenu { .. } | LibraryReq::Play { .. } | LibraryReq::Detail { .. }
            )),
            "release after handled hold must not activate again"
        );
    }
}
