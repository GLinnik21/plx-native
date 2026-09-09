fn menu_arg(kind: LibraryMenuKind, anchor: [u32; 4]) -> LibraryMenuArg {
    LibraryMenuArg {
        host: crate::ui::machine::InstanceId(8),
        target: SectionAddress {
            epoch: 11,
            sid: ServerId::from_raw(1),
            section: 7,
        },
        kind,
        anchor,
    }
}

fn anchor_bits(rect: Rect) -> [u32; 3] {
    [rect.x.to_bits(), rect.y.to_bits(), rect.w.to_bits()]
}

fn source_cx<'a>(
    listing: &'a crate::browse::view::ListingSnapshot,
    directory: &'a crate::stores::browse::DirectorySnapshot,
    hubs: &'a crate::browse::section_hubs::HubsSnapshot,
    measure: &'a FixtureMeasure,
    tick: Tick,
) -> Cx<'a, HostFixture> {
    Cx {
        views: Views {
            listing: listing.view(),
            directory: directory.view(),
            hubs: hubs.view(),
        },
        tick,
        measure,
        focus: FocusRead { current: None, ..Default::default() },
        press: PressRead::default(),
        owner: InputOwner::Entry(EntryId(7)),
    }
}

#[test]
fn menu_anchor_is_frozen_and_a_new_open_uses_the_new_anchor() {
    let _guard = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("menu-anchor-contract");
    session.watching("u-menu-anchor-contract");
    crate::browse::seed_two_source_table_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::SetCur(0));
    let listing = crate::stores::browse::listing_snapshot();
    let hubs = crate::stores::browse::hubs_snapshot();
    let measure = FixtureMeasure;
    let mut first = crate::stores::browse::DirectorySnapshot::default();
    first.capture();
    let old_anchor = [100.0f32.to_bits(), 400.0f32.to_bits(), 220.0f32.to_bits(), 60.0f32.to_bits()];
    let new_anchor = [760.0f32.to_bits(), 260.0f32.to_bits(), 240.0f32.to_bits(), 60.0f32.to_bits()];
    let mut old = LibraryMenu::new(EntryId(7), menu_arg(LibraryMenuKind::Sources, old_anchor));
    let mut output = Vec::new();
    let mut present = crate::ui::present::Present::new();
    let cx_first = source_cx(&listing, &first, &hubs, &measure, Tick { ms: 1, dt_us: 16_000 });
    old.step(
        &ScreenEvent::Tick(Tick { ms: 1, dt_us: 16_000 }),
        &cx_first,
        &mut Effects::new(&mut output, MachineId::Instance(crate::ui::machine::InstanceId(7)), &mut present),
    );

    let mut old_groups = Vec::new();
    old.groups(&cx_first, &mut old_groups);
    let old_extent = old_groups[0].extent;

    crate::browse::append_section_for_test(1, 3, "New Films", SecKind::Movie);
    crate::browse::set_pinned_for_test(4, true);
    let mut changed = crate::stores::browse::DirectorySnapshot::default();
    changed.capture();
    let cx_changed = source_cx(&listing, &changed, &hubs, &measure, Tick { ms: 2, dt_us: 16_000 });
    old.step(
        &ScreenEvent::Tick(Tick { ms: 2, dt_us: 16_000 }),
        &cx_changed,
        &mut Effects::new(&mut output, MachineId::Instance(crate::ui::machine::InstanceId(7)), &mut present),
    );
    let mut old_after_host_move = Vec::new();
    old.groups(&cx_changed, &mut old_after_host_move);
    assert_eq!(anchor_bits(old_after_host_move[0].extent), anchor_bits(old_extent), "an open menu stays on its frozen host anchor");

    let mut fresh = LibraryMenu::new(EntryId(8), menu_arg(LibraryMenuKind::Sources, new_anchor));
    fresh.step(
        &ScreenEvent::Tick(Tick { ms: 3, dt_us: 16_000 }),
        &cx_changed,
        &mut Effects::new(&mut output, MachineId::Instance(crate::ui::machine::InstanceId(8)), &mut present),
    );
    let mut fresh_groups = Vec::new();
    fresh.groups(&cx_changed, &mut fresh_groups);
    assert_ne!(anchor_bits(fresh_groups[0].extent), anchor_bits(old_extent), "a new open releases the old anchor");
    assert_eq!(fresh_groups[0].extent.x, 760.0);
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
}

#[test]
fn menu_side_actions_keep_source_sort_and_filter_row_identity() {
    let _guard = crate::testlock::serial();
    let mut sort = LibraryMenu::new(EntryId(7), menu_arg(LibraryMenuKind::Sort, [0; 4]));
    let sorts = vec![
        SortEntry { key: "titleSort".into(), title: "Title".into(), default_desc: false },
        SortEntry { key: "addedAt".into(), title: "Added".into(), default_desc: true },
    ];
    sort.apply_draft(sort_draft(&sorts, 0, false));
    let sort_key = sort.rows[1].key;
    let mut filter = LibraryMenu::new(EntryId(7), menu_arg(LibraryMenuKind::Filter, [0; 4]));
    filter.apply_draft(filter_draft(false, None));
    let filter_key = filter.rows[0].key;
    let (groups, sections) = source_sections();
    let mut sources = LibraryMenu::new(EntryId(7), menu_arg(LibraryMenuKind::Sources, [0; 4]));
    sources.apply_draft(source_draft(11, 0, &groups, &sections));
    let source_key = sources.rows[1].key;

    with_cx(|cx| {
        assert!(matches!(sort.rows.iter().find(|row| row.key == sort_key).map(|row| &row.action), Some(Action::Edit(QueryEdit::Sort { key, desc: true })) if key == "addedAt"));
        assert!(matches!(filter.rows.iter().find(|row| row.key == filter_key).map(|row| &row.action), Some(Action::Edit(QueryEdit::Unwatched(true)))));
        assert!(matches!(sources.rows.iter().find(|row| row.key == source_key).map(|row| &row.action), Some(Action::Select(SectionAddress { sid, section: 7, .. })) if *sid == ServerId::from_raw(2)));
        assert!(<LibraryMenu as Focusable<HostFixture>>::place(&sort, &sort_key, cx, At::Drawn).is_some());
        assert!(<LibraryMenu as Focusable<HostFixture>>::place(&filter, &filter_key, cx, At::Drawn).is_some());
        assert!(<LibraryMenu as Focusable<HostFixture>>::place(&sources, &source_key, cx, At::Drawn).is_some());

        let mut output = Vec::new();
        let mut present = crate::ui::present::Present::new();
        sort.step(
            &ScreenEvent::Activate(sort_key),
            cx,
            &mut Effects::new(&mut output, MachineId::Instance(crate::ui::machine::InstanceId(7)), &mut present),
        );
        assert!(output.iter().any(|effect| matches!(
            &effect.fx,
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::App(AppMsg::LibraryEdit {
                edit: QueryEdit::Sort { key, desc: true }, ..
            }))) if key == "addedAt"
        )));
        assert!(output.iter().any(|effect| matches!(
            &effect.fx,
            Fx::Nav(NavOp::Dismiss(EntryId(7)))
        )));

        output.clear();
        filter.step(
            &ScreenEvent::Activate(filter_key),
            cx,
            &mut Effects::new(&mut output, MachineId::Instance(crate::ui::machine::InstanceId(7)), &mut present),
        );
        assert!(output.iter().any(|effect| matches!(
            &effect.fx,
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::App(AppMsg::LibraryEdit {
                edit: QueryEdit::Unwatched(true), ..
            })))
        )));
        assert!(!output
            .iter()
            .any(|effect| matches!(effect.fx, Fx::Nav(NavOp::Dismiss(_)))));

        output.clear();
        sources.step(
            &ScreenEvent::Activate(source_key),
            cx,
            &mut Effects::new(&mut output, MachineId::Instance(crate::ui::machine::InstanceId(7)), &mut present),
        );
        assert!(output.iter().any(|effect| matches!(
            &effect.fx,
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::App(AppMsg::LibrarySelect(
                SectionAddress { sid, section: 7, .. }
            )))) if *sid == ServerId::from_raw(2)
        )));
        assert!(output.iter().any(|effect| matches!(
            &effect.fx,
            Fx::Nav(NavOp::Dismiss(EntryId(7)))
        )));
    });
}

#[test]
fn sources_menu_left_is_an_engine_edge_not_an_editor_transition() {
    use crate::ui::focus::{FocusEngine, Outcome};

    let _guard = crate::testlock::serial();
    let (groups, sections) = source_sections();
    let mut menu = LibraryMenu::new(EntryId(7), menu_arg(LibraryMenuKind::Sources, [0; 4]));
    menu.apply_draft(source_draft(11, 0, &groups, &sections));
    let owner = InputOwner::Entry(EntryId(7));
    with_cx(|cx| {
        let mut engine = FocusEngine::new();
        engine.enter(owner, &menu, crate::ui::screen::FocusTarget::ContainerGroup(GroupId(0)), None, cx);
        let before = engine.current(owner).expect("source menu seats its first row");
        assert!(matches!(engine.move_dir(owner, &menu, &[], Dir::Left, cx), Outcome::Nothing));
        assert_eq!(engine.current(owner), Some(before));

        let mut output = Vec::new();
        let mut present = crate::ui::present::Present::new();
        assert_eq!(
            menu.step(
                &ScreenEvent::Input(crate::ui::machine::InputEvent {
                    at: Tick::default(),
                    source: crate::ui::machine::Source::Script,
                    kind: InputKind::Key {
                        key: Key::Left,
                        sym: 0,
                        wcode: 0,
                        edge: Edge::Down,
                        at_edge: false,
                    },
                }),
                cx,
                &mut Effects::new(&mut output, MachineId::Instance(crate::ui::machine::InstanceId(7)), &mut present),
            ),
            Handled::No
        );
        assert_eq!(menu.kind, LibraryMenuKind::Sources, "LEFT cannot enter an editor");
        assert!(output.is_empty(), "LEFT emits no source/editor action");
    });
}

#[test]
fn open_sources_refreshes_metadata_once_then_settles() {
    let _guard = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("menu-refresh-contract");
    session.watching("u-menu-refresh-contract");
    crate::browse::seed_two_source_table_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::SetCur(0));
    let listing = crate::stores::browse::listing_snapshot();
    let hubs = crate::stores::browse::hubs_snapshot();
    let measure = FixtureMeasure;
    let mut first = crate::stores::browse::DirectorySnapshot::default();
    first.capture();
    let mut menu = LibraryMenu::new(EntryId(7), menu_arg(LibraryMenuKind::Sources, [0; 4]));
    assert_eq!(first.view().current(), Some(0), "the added Movie row must affect the menu's current kind");
    let mut output = Vec::new();
    let mut present = crate::ui::present::Present::new();
    let mut effects = |menu: &mut LibraryMenu, cx: &Cx<'_, HostFixture>| {
        menu.step(
            &ScreenEvent::Tick(Tick { ms: 1, dt_us: 16_000 }),
            cx,
            &mut Effects::new(&mut output, MachineId::Instance(crate::ui::machine::InstanceId(7)), &mut present),
        );
    };

    let cx_first = source_cx(&listing, &first, &hubs, &measure, Tick { ms: 1, dt_us: 16_000 });
    effects(&mut menu, &cx_first);
    let first_key = menu.rows[0].key;
    let first_stamp = menu.stamp.clone();
    assert_eq!(menu.draft_rebuilds, 1);
    effects(&mut menu, &cx_first);
    assert_eq!(menu.stamp, first_stamp);
    assert_eq!(menu.rows[0].key, first_key, "an unchanged open source menu settles");
    let first_selection = menu.table.sel;
    assert_eq!(menu.draft_rebuilds, 1);

    crate::browse::append_section_for_test(1, 3, "New Films", SecKind::Movie);
    crate::browse::set_pinned_for_test(4, true);
    let mut changed = crate::stores::browse::DirectorySnapshot::default();
    changed.capture();
    let cx_changed = source_cx(&listing, &changed, &hubs, &measure, Tick { ms: 2, dt_us: 16_000 });
    effects(&mut menu, &cx_changed);
    assert_ne!(menu.stamp, first_stamp, "source metadata refresh rebuilds the open menu");
    assert_eq!(menu.rows[0].key, first_key, "refresh preserves the row identity");
    assert_eq!(menu.draft_rebuilds, 2);
    let changed_stamp = menu.stamp.clone();
    effects(&mut menu, &cx_changed);
    assert_eq!(menu.stamp, changed_stamp, "the changed source menu settles after one rebuild");
    assert_eq!(menu.rows[0].key, first_key);
    assert_eq!(menu.table.sel, first_selection);
    assert_eq!(menu.draft_rebuilds, 2);
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
}
