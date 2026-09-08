#[test]
fn bookmark_commands_are_addressed_and_identical_snapshots_are_quiet() {
    use crate::stores::browse::{BrowseCmd, LibraryWork, SectionAddress};
    let _guard = crate::testlock::serial();
    crate::stores::browse::apply(BrowseCmd::Reset);
    crate::browse::seed_two_source_table_for_test();
    let mut directory = crate::stores::browse::DirectorySnapshot::default();
    directory.capture();
    let section = &directory.view().sections()[0];
    let target = SectionAddress {
        epoch: directory.view().epoch().unwrap(),
        sid: section.sid.unwrap(),
        section: section.key,
    };
    crate::stores::browse::apply(BrowseCmd::SetCur(0));
    let mut retained = None;
    for at in [
        crate::browse::CursorAt::SlotIndex(52),
        crate::browse::CursorAt::ItemKey {
            sid: target.sid,
            rk: "synthetic-52".into(),
            slot: 52,
        },
    ] {
        let cursor = crate::browse::Cursor { at, scroll: 1200.0 };
        let command = BrowseCmd::Addressed {
            target,
            work: LibraryWork::SaveCursor {
                query: crate::browse::query_gen(),
                cursor: cursor.clone(),
            },
        };
        assert!(crate::stores::browse::apply(command.clone()));
        let snapshot = crate::stores::browse::listing_snapshot();
        assert_eq!(snapshot.view().cursor(), Some(&cursor));
        if let Some((old, value)) = &retained {
            let old: &crate::stores::browse::ListingSnapshot = old;
            assert_eq!(
                old.view().cursor(),
                Some(value),
                "a later save cannot mutate a retained frame"
            );
        }
        retained = Some((snapshot, cursor.clone()));
        crate::stores::take_notices();
        let generation = crate::stores::gen(StoreId::Browse);
        assert!(!crate::stores::browse::apply(command));
        assert_eq!(crate::stores::gen(StoreId::Browse), generation);
        assert!(
            crate::stores::take_notices().is_empty(),
            "identical snapshots owe no store notice"
        );
        assert!(!crate::stores::browse::apply(BrowseCmd::Addressed {
            target: SectionAddress {
                epoch: target.epoch.wrapping_add(1),
                ..target
            },
            work: LibraryWork::SaveCursor {
                query: crate::browse::query_gen(),
                cursor: cursor.clone()
            },
        }));
        assert_eq!(crate::stores::gen(StoreId::Browse), generation);
        assert!(
            crate::stores::take_notices().is_empty(),
            "rejected snapshots owe no store notice"
        );
        assert!(!crate::stores::browse::apply(BrowseCmd::Addressed {
            target,
            work: LibraryWork::SaveCursor {
                query: crate::browse::query_gen().wrapping_add(1),
                cursor
            },
        }));
        assert_eq!(crate::stores::gen(StoreId::Browse), generation);
        assert!(
            crate::stores::take_notices().is_empty(),
            "an obsolete query snapshot is quiet too"
        );
    }
    crate::stores::browse::apply(BrowseCmd::Reset);
    assert!(crate::stores::browse::listing_snapshot()
        .view()
        .cursor()
        .is_none());
    let (snapshot, value) = retained.unwrap();
    assert_eq!(
        snapshot.view().cursor(),
        Some(&value),
        "profile reset drops live bookmarks, not retained frames"
    );
}

#[test]
fn retired_library_entry_seeds_its_previous_section_card_and_viewport() {
    #[derive(Default)]
    struct Saves(Vec<(crate::stores::browse::SectionAddress, crate::browse::Cursor)>);
    impl crate::ui::dispatch::Tap<AppHost> for Saves {
        fn effect(&mut self, _: u64, effect: &crate::ui::machine::Stamped<AppHost>) {
            if let Fx::App(AppFx::Store(
                _,
                crate::stores::StoreCmd::Browse(crate::stores::browse::BrowseCmd::Addressed {
                    target,
                    work: crate::stores::browse::LibraryWork::SaveCursor { cursor, .. },
                }),
            )) = &effect.fx
            {
                self.0.push((*target, cursor.clone()));
            }
        }
    }
    let _guard = crate::testlock::serial();
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
            crate::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    let session = crate::plex::session::TempSession::new("library-bookmark-return");
    session.watching("u-library-bookmark-return");
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::plex::reset_servers_for_test();
    let sid =
        crate::plex::register_for_test("bookmark-own", "127.0.0.1", 9, "synthetic", "fixture");
    let shared =
        crate::plex::register_for_test("bookmark-shared", "127.0.0.1", 10, "synthetic", "fixture");
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
    let old_entry = d.nav.top_page().unwrap().id;
    let (item, opener) = rig.library_selection(&d, old_entry, d.focus()).unwrap();
    let before = opener.rect.unwrap();
    let select = |d: &mut Dispatcher<AppHost>, rig: &Bridge, index: usize| {
        let view = rig.directory.view();
        let section = &view.sections()[index];
        let target = crate::stores::browse::SectionAddress {
            epoch: view.epoch().unwrap(),
            sid: section.sid.unwrap(),
            section: section.key,
        };
        let instance = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
        d.emit(
            MachineId::Nav,
            Fx::Deliver(
                MachineId::Instance(instance),
                Delivery::Screen(ScreenEvent::App(AppMsg::LibrarySelect(target))),
            ),
        );
    };
    // Menu selection addresses a covered host: its Cx must read that host entry's
    // engine memory, never the menu's current row or its unrelated remembered groups.
    let listing_id = rig.listing.view().id().unwrap();
    d.nav.next_style = crate::ui::containers::modal::Style::Compact;
    d.request(
        MachineId::Nav,
        NavOp::Present(AppArg::LibraryMenu(
            crate::screens::registry::LibraryMenuArg {
                host: d.nav.top_page().unwrap().inst.as_ref().unwrap().id,
                kind: crate::screens::registry::LibraryMenuKind::Filter,
                anchor: [0; 4],
                target: crate::stores::browse::SectionAddress {
                    epoch: listing_id.epoch,
                    sid: listing_id.sid,
                    section: listing_id.section,
                },
            },
        )),
    );
    frame(&mut d, &mut rig, Route::Library, tick(80), vec![]);
    let InputOwner::Entry(menu_entry) = d.nav.input_owner().unwrap() else {
        unreachable!()
    };
    assert_ne!(menu_entry, old_entry);
    select(&mut d, &rig, 2);
    let mut saves = Saves::default();
    let trail = super::super::Trail::new();
    for i in 81..120 {
        super::frame_with_tap(
            &mut d,
            &mut rig,
            Route::Library,
            &trail,
            tick(i),
            vec![],
            &mut saves,
        );
    }
    assert!(saves.0.iter().any(|(target, cursor)| target.sid == item.sid
        && matches!(&cursor.at, crate::browse::CursorAt::ItemKey { sid, rk, slot: 52 } if *sid == item.sid && rk == &item.rk)),
        "section switching must emit the outgoing A snapshot through the StoreCmd drain");
    assert_eq!(
        rig.directory.view().current(),
        Some(2),
        "the actual deferred transaction enters section B"
    );
    d.request(MachineId::Nav, NavOp::Dismiss(menu_entry));
    crate::browse::seed_items_for_test(120);
    frame(&mut d, &mut rig, Route::Library, tick(120), vec![]);
    Bridge::library_command(
        &mut d,
        crate::screens::registry::LibraryCmd::FocusGrid { row: 3, col: 1 },
    );
    for i in 121..200 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    let (b_item, _) = rig.library_selection(&d, old_entry, d.focus()).unwrap();
    assert_ne!(
        b_item.sid, item.sid,
        "same rating-key namespace on two actual servers"
    );
    for i in 200..210 {
        let (_, report) = super::frame_with_tap(
            &mut d,
            &mut rig,
            Route::Home,
            &trail,
            tick(i),
            vec![],
            &mut saves,
        );
        d.prune(&report.unmounted);
    }
    assert!(saves.0.iter().any(|(target, cursor)| target.sid == b_item.sid
        && matches!(&cursor.at, crate::browse::CursorAt::ItemKey { sid, rk, slot: 19 } if *sid == b_item.sid && rk == &b_item.rk)),
        "outgoing WillLeave must emit B's snapshot before its engine memory is retired");
    assert!(
        d.nav.entry(old_entry).is_none(),
        "the test must retire, not merely evict, the old entry"
    );
    rig.enter_library(crate::browse::SecKind::Movie);
    for i in 210..290 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    let entry = d.nav.top_page().unwrap().id;
    assert_ne!(entry, old_entry);
    let (returned_b, _) = rig
        .library_selection(&d, entry, d.focus())
        .expect("new entry restores B's saved card");
    assert_eq!(
        (returned_b.sid, returned_b.rk.as_str()),
        (b_item.sid, b_item.rk.as_str())
    );
    select(&mut d, &rig, 0);
    for i in 290..370 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    assert_eq!(rig.directory.view().current(), Some(0));
    let (returned, opener) = rig
        .library_selection(&d, entry, d.focus())
        .expect("new entry restores the saved card");
    assert_eq!(
        (returned.sid, returned.rk.as_str()),
        (item.sid, item.rk.as_str())
    );
    let after = opener.rect.unwrap();
    assert!((before.y - after.y).abs() < 0.01, "{before:?} -> {after:?}");
}
