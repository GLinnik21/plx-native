#[test]
fn query_reset_intent_and_observed_query_are_canonical_in_return_memory() {
    let _guard = crate::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    let hash = |state: &dyn LogicalState| {
        let mut canon = Canon::new();
        state.write(&mut canon);
        canon.finish()
    };
    let before = hash(&page);
    page.grid_reset_pending = true;
    assert_ne!(hash(&page), before);
    let PageMemory::Library(mut memory) = <LibraryScreen as Screen<HostFixture>>::memory(&page)
    else {
        unreachable!()
    };
    let pending = hash(&PageMemory::Library(memory.clone()));
    memory.grid_reset_pending = false;
    assert_ne!(hash(&PageMemory::Library(memory.clone())), pending);
    let current_query = hash(&PageMemory::Library(memory.clone()));
    memory.query = memory.query.map(|query| query.wrapping_add(1));
    assert_ne!(hash(&PageMemory::Library(memory)), current_query);
}

#[test]
fn accepted_queries_reset_engine_grid_memory_but_keep_the_toolbar_during_loading() {
    let _guard = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("library-query-reset");
    session.watching("u-library-query-reset");
    for edit in [
        QueryEdit::Sort {
            key: "year".into(),
            desc: false,
        },
        QueryEdit::Unwatched(true),
        QueryEdit::Genre(Some("g1".into())),
    ] {
        for evict in [false, true] {
            let mut fixture = Fixture::new();
            crate::browse::seed_two_source_table_for_test();
            fixture.directory.capture();
            crate::stores::browse::apply(BrowseCmd::SetCur(0));
            crate::browse::seed_items_for_test(120);
            crate::browse::seed_query_choices_for_test(
                vec![
                    crate::browse::SortEntry {
                        key: "titleSort".into(),
                        title: "Title".into(),
                        default_desc: false,
                    },
                    crate::browse::SortEntry {
                        key: "year".into(),
                        title: "Year".into(),
                        default_desc: false,
                    },
                ],
                vec![crate::browse::GenreEntry {
                    id: "g1".into(),
                    title: "Drama".into(),
                }],
            );
            fixture.directory.capture();
            fixture.listing = crate::stores::browse::listing_snapshot();
            fixture.hubs = crate::stores::browse::hubs_snapshot();
            let id = fixture.listing.view().id().unwrap();
            let mut page = fixture.screen();
            page.initial = false; // The fixture premise is an already-entered, settled page.
            let mut engine = FocusEngine::new();
            let grid = page.key(page.pair.detail.elems[52]);
            let group = page.pair.groups_config().detail;
            engine.set(OWNER, grid, Some(group), By::Restore);
            let toolbar = page.key(if matches!(edit, QueryEdit::Sort { .. }) {
                SORT
            } else {
                FILTER
            });
            engine.set(OWNER, toolbar, Some(TOOLBAR_GROUP), By::Restore);
            page.scroll.jump(page.target_layout.row_reveal(8));
            page.scroll_target = page.scroll.pos;
            let mut cx = fixture.cx(Some(toolbar));
            cx.focus = engine.read(OWNER);
            let mut output = Vec::new();
            let mut present = crate::ui::present::Present::new();
            page.step(
                &ScreenEvent::App(AppMsg::LibraryEdit {
                    target: SectionAddress {
                        epoch: id.epoch,
                        sid: id.sid,
                        section: id.section,
                    },
                    edit: edit.clone(),
                }),
                &cx,
                &mut Effects::new(
                    &mut output,
                    MachineId::Instance(InstanceId(19)),
                    &mut present,
                ),
            );
            let mut committed = false;
            for frame in 0..30 {
                output.clear();
                let mut cx = fixture.cx(Some(toolbar));
                cx.focus = engine.read(OWNER);
                cx.tick = Tick {
                    ms: frame * 16,
                    dt_us: 16_000,
                };
                page.step(
                    &ScreenEvent::Tick(cx.tick),
                    &cx,
                    &mut Effects::new(
                        &mut output,
                        MachineId::Instance(InstanceId(19)),
                        &mut present,
                    ),
                );
                for effect in output.drain(..) {
                    if let Fx::App(AppFx::Store(_, command)) = effect.fx {
                        if matches!(
                            &command,
                            crate::stores::StoreCmd::Browse(BrowseCmd::Addressed {
                                work: LibraryWork::Commit { query: Some(_), .. },
                                ..
                            })
                        ) {
                            assert!(
                                crate::stores::apply(command),
                                "{edit:?} must be accepted by the real store"
                            );
                            committed = true;
                        }
                    }
                }
                if committed {
                    break;
                }
            }
            assert!(committed);
            fixture.directory.capture();
            fixture.listing = crate::stores::browse::listing_snapshot();
            assert_ne!(fixture.listing.view().id().unwrap().query, id.query);
            assert_eq!(fixture.listing.view().fetch(), SecFetch::Loading);
            let mut cx = fixture.cx(Some(toolbar));
            cx.focus = engine.read(OWNER);
            page.step(
                &ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1),
                &cx,
                &mut Effects::new(
                    &mut output,
                    MachineId::Instance(InstanceId(19)),
                    &mut present,
                ),
            );
            engine.reconcile(OWNER, &page, &cx);
            assert_eq!(
                engine.current(OWNER),
                Some(toolbar),
                "toolbar must survive the unloaded query: {edit:?}"
            );
            if evict {
                let PageMemory::Library(memory) =
                    <LibraryScreen as Screen<HostFixture>>::memory(&page)
                else {
                    unreachable!()
                };
                let mut remounted = LibraryScreen::new(ENTRY, InstanceId(20), SecKind::Movie);
                remounted.restore(&memory);
                page = remounted;
                page.step(
                    &ScreenEvent::StoreChanged(StoreId::Browse.ord(), 2),
                    &cx,
                    &mut Effects::new(
                        &mut output,
                        MachineId::Instance(InstanceId(20)),
                        &mut present,
                    ),
                );
                engine.reconcile(OWNER, &page, &cx);
                assert_eq!(
                    engine.current(OWNER),
                    Some(toolbar),
                    "evicted query keeps the control band"
                );
            }
            for frame in 40..100 {
                let mut cx = fixture.cx(Some(toolbar));
                cx.focus = engine.read(OWNER);
                cx.tick = Tick {
                    ms: frame * 16,
                    dt_us: 16_000,
                };
                page.step(
                    &ScreenEvent::Tick(cx.tick),
                    &cx,
                    &mut Effects::new(
                        &mut output,
                        MachineId::Instance(InstanceId(19)),
                        &mut present,
                    ),
                );
                engine.reconcile(OWNER, &page, &cx);
                assert_eq!(
                    engine.current(OWNER),
                    Some(toolbar),
                    "toolbar must survive a slow query: {edit:?}"
                );
            }
            crate::browse::seed_items_for_test(120);
            fixture.listing = crate::stores::browse::listing_snapshot();
            output.clear();
            let mut cx = fixture.cx(Some(toolbar));
            cx.focus = engine.read(OWNER);
            page.step(
                &ScreenEvent::Tick(Tick {
                    ms: 600,
                    dt_us: 16_000,
                }),
                &cx,
                &mut Effects::new(
                    &mut output,
                    MachineId::Instance(InstanceId(19)),
                    &mut present,
                ),
            );
            for effect in output {
                if let Fx::Remember { group, elem } = effect.fx {
                    engine.remember_projected(ENTRY, group, elem);
                }
            }
            assert_eq!(engine.current(OWNER), Some(toolbar));
            let mut links = Vec::new();
            <LibraryScreen as Screen<HostFixture>>::links(&page, &mut links);
            engine.move_dir(OWNER, &page, &links, Dir::Down, &fixture.cx(Some(toolbar)));
            assert_eq!(
                engine.current(OWNER),
                Some(page.key(page.pair.detail.elems[0])),
                "accepted query must reset the engine's remembered grid target: {edit:?}"
            );
            assert!((page.scroll_target - page.target_layout.row_reveal(0)).abs() < 0.01);
        }
    }
    crate::stores::browse::apply(BrowseCmd::Reset);
}
