#[test]
fn back_dismisses_the_filter_menu_before_the_floor_without_cancelling_its_query() {
    #[derive(Default)]
    struct Trace {
        edits: usize,
        commits: Vec<crate::stores::browse::QueryEdit>,
    }
    impl crate::ui::dispatch::Tap<AppHost> for Trace {
        fn effect(&mut self, _: u64, effect: &crate::ui::machine::Stamped<AppHost>) {
            match &effect.fx {
                Fx::Deliver(_, Delivery::Screen(ScreenEvent::App(AppMsg::LibraryEdit { .. }))) => {
                    self.edits += 1
                }
                Fx::App(AppFx::Store(
                    _,
                    crate::stores::StoreCmd::Browse(crate::stores::browse::BrowseCmd::Addressed {
                        work:
                            crate::stores::browse::LibraryWork::Commit {
                                query: Some(edit), ..
                            },
                        ..
                    }),
                )) => self.commits.push(edit.clone()),
                _ => {}
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
    let session = crate::plex::session::TempSession::new("library-back-before-floor");
    session.watching("u-library-back-before-floor");
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test("back-own", "127.0.0.1", 9, "synthetic", "fixture");
    let shared =
        crate::plex::register_for_test("back-shared", "127.0.0.1", 10, "synthetic", "fixture");
    crate::plex::set_current(sid);
    crate::browse::seed_registered_table_for_test([sid, shared]);
    let mut directory = crate::stores::browse::DirectorySnapshot::default();
    directory.capture();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::SetCur(0));
    crate::browse::seed_items_for_test(120);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, Route::Library, tick(0), vec![]);
    let page = d.nav.top_page().unwrap().id;
    let host = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    let listing = rig.listing.view().id().unwrap();
    d.nav.next_style = crate::ui::containers::modal::Style::Compact;
    d.request(
        MachineId::Nav,
        NavOp::Present(AppArg::LibraryMenu(
            crate::screens::registry::LibraryMenuArg {
                host,
                kind: crate::screens::registry::LibraryMenuKind::Filter,
                anchor: [0; 4],
                target: crate::stores::browse::SectionAddress {
                    epoch: listing.epoch,
                    sid: listing.sid,
                    section: listing.section,
                },
            },
        )),
    );
    for i in 1..40 {
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    let menu = d.focus().unwrap().entry;
    assert_ne!(menu, page);
    let trail = super::super::Trail::new();
    let mut trace = Trace::default();
    super::frame_with_tap(
        &mut d,
        &mut rig,
        Route::Library,
        &trail,
        tick(40),
        script_key(Key::Ok, tick(40)),
        &mut trace,
    );
    let mut request_frame = None;
    for i in 41..80 {
        super::frame_with_tap(
            &mut d,
            &mut rig,
            Route::Library,
            &trail,
            tick(i),
            vec![],
            &mut trace,
        );
        if trace.edits > 0 {
            request_frame = Some(i);
            break;
        }
    }
    let request_frame =
        request_frame.expect("the real menu activation must enqueue its desired filter");
    assert!(
        trace.commits.is_empty(),
        "the test must press BACK before the fade floor"
    );
    assert!(!rig.listing.view().unwatched());
    super::frame_with_tap(
        &mut d,
        &mut rig,
        Route::Library,
        &trail,
        tick(request_frame + 1),
        script_key(Key::Back, tick(request_frame + 1)),
        &mut trace,
    );
    assert_ne!(
        d.nav.input_owner(),
        Some(InputOwner::Entry(menu)),
        "the menu consumed BACK and dismissed"
    );
    assert_eq!(
        d.nav.top_page().unwrap().id,
        page,
        "BACK did not leave Library"
    );
    for i in request_frame + 2..request_frame + 60 {
        super::frame_with_tap(
            &mut d,
            &mut rig,
            Route::Library,
            &trail,
            tick(i),
            vec![],
            &mut trace,
        );
    }
    assert_eq!(
        trace.commits,
        vec![crate::stores::browse::QueryEdit::Unwatched(true)],
        "dismissal preserves the pending desired action and commits it exactly once"
    );
    assert!(rig.listing.view().unwatched());
    assert_ne!(rig.listing.view().id().unwrap().query, listing.query);
}
