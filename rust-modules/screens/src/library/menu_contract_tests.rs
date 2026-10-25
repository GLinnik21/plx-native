fn menu_arg(kind: LibraryMenuKind, anchor: [u32; 4]) -> LibraryMenuArg {
    LibraryMenuArg {
        host: plx_machine::machine::InstanceId(8),
        target: SectionAddress {
            epoch: 11,
            sid: ServerId::from_raw(1),
            section: 7,
        },
        kind,
        anchor,
        folded: if kind == LibraryMenuKind::Sources { (0..9).collect() } else { Vec::new() },
        opener: None,
    }
}

fn anchor_bits(rect: Rect) -> [u32; 3] {
    [rect.x.to_bits(), rect.y.to_bits(), rect.w.to_bits()]
}

fn source_cx<'a>(
    listing: &'a plx_data::stores::browse::ListingSnapshot,
    directory: &'a plx_data::stores::browse::DirectorySnapshot,
    hubs: &'a plx_data::stores::browse::HubsSnapshot,
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
    let _guard = plx_base::testlock::serial();
    let session = plx_plex::plex::session::TempSession::new("menu-anchor-contract");
    session.watching("u-menu-anchor-contract");
    let stores = plx_data::stores::Stores::default();
    stores.browse.borrow_mut().seed_two_source_table_for_test();
    stores.browse_run_for_test(plx_data::stores::browse::BrowseCmd::SetCur(0));
    let measure = FixtureMeasure;
    let mut first = plx_data::stores::browse::DirectorySnapshot::default();
    let publication = stores.capture_browse(&mut first);
    let listing = publication.listing;
    let hubs = publication.section_hubs;
    let old_anchor = [100.0f32.to_bits(), 400.0f32.to_bits(), 220.0f32.to_bits(), 60.0f32.to_bits()];
    let new_anchor = [760.0f32.to_bits(), 260.0f32.to_bits(), 240.0f32.to_bits(), 60.0f32.to_bits()];
    let mut old = LibraryMenu::new(EntryId(7), menu_arg(LibraryMenuKind::Sources, old_anchor));
    let mut output = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let cx_first = source_cx(&listing, &first, &hubs, &measure, Tick { ms: 1, dt_us: 16_000 });
    old.step(
        &ScreenEvent::Tick(Tick { ms: 1, dt_us: 16_000 }),
        &cx_first,
        &mut Effects::new(&mut output, MachineId::Instance(plx_machine::machine::InstanceId(7)), &mut present),
    );

    let mut old_groups = Vec::new();
    old.groups(&cx_first, &mut old_groups);
    let old_extent = old_groups[0].extent;

    {
        let mut browse = stores.browse.borrow_mut();
        browse.append_section_for_test(1, 3, "New Films", SecKind::Movie);
        browse.set_pinned_for_test(4, true);
    }
    let mut changed = plx_data::stores::browse::DirectorySnapshot::default();
    stores.capture_browse(&mut changed);
    let cx_changed = source_cx(&listing, &changed, &hubs, &measure, Tick { ms: 2, dt_us: 16_000 });
    old.step(
        &ScreenEvent::Tick(Tick { ms: 2, dt_us: 16_000 }),
        &cx_changed,
        &mut Effects::new(&mut output, MachineId::Instance(plx_machine::machine::InstanceId(7)), &mut present),
    );
    let mut old_after_host_move = Vec::new();
    old.groups(&cx_changed, &mut old_after_host_move);
    assert_eq!(anchor_bits(old_after_host_move[0].extent), anchor_bits(old_extent), "an open menu stays on its frozen host anchor");

    let mut fresh = LibraryMenu::new(EntryId(8), menu_arg(LibraryMenuKind::Sources, new_anchor));
    fresh.step(
        &ScreenEvent::Tick(Tick { ms: 3, dt_us: 16_000 }),
        &cx_changed,
        &mut Effects::new(&mut output, MachineId::Instance(plx_machine::machine::InstanceId(8)), &mut present),
    );
    let mut fresh_groups = Vec::new();
    fresh.groups(&cx_changed, &mut fresh_groups);
    assert_ne!(anchor_bits(fresh_groups[0].extent), anchor_bits(old_extent), "a new open releases the old anchor");
    assert_eq!(fresh_groups[0].extent.x, 760.0 - plx_ui::widgets::PILL_FOCUS_OUTSET,
        "the +N list hangs from its +N pill's near edge (the focused pill's outer edge), as Sort & Filter does");
}

#[test]
fn menu_side_actions_keep_source_sort_and_filter_row_identity() {
    let _guard = plx_base::testlock::serial();
    let mut sort = LibraryMenu::new(EntryId(7), menu_arg(LibraryMenuKind::Sort, [0; 4]));
    let sorts = vec![
        SortEntry { key: "titleSort".into(), desc_key: String::new(), title: "Title".into(), default_desc: false },
        SortEntry { key: "addedAt".into(), desc_key: String::new(), title: "Added".into(), default_desc: true },
    ];
    sort.apply_draft(sort_draft(&sorts, 0, false));
    let sort_key = sort.form.key_at(1).unwrap().0;
    let mut filter = LibraryMenu::new(EntryId(7), menu_arg(LibraryMenuKind::Filter, [0; 4]));
    filter.apply_draft(filter_draft(false, None, true));
    let filter_key = filter.form.key_at(0).unwrap().0;
    let (groups, sections) = source_sections();
    let mut sources = LibraryMenu::new(EntryId(7), menu_arg(LibraryMenuKind::Sources, [0; 4]));
    sources.apply_draft(source_draft(11, 0, &groups, &sections, &(0..9).collect::<Vec<u32>>()));
    let source_key = sources.form.key_at(1).unwrap().0;

    with_cx(|cx| {
        assert!(matches!(sort.form.index_of_key(RowKey(sort_key)).and_then(|i| sort.form.binding_at(i)).map(|b| &b.action), Some(Action::Edit(QueryEdit::Sort { key, desc: true })) if key == "addedAt"));
        assert!(matches!(filter.form.index_of_key(RowKey(filter_key)).and_then(|i| filter.form.binding_at(i)).map(|b| &b.action), Some(Action::Edit(QueryEdit::Unwatched(true)))));
        assert!(matches!(sources.form.index_of_key(RowKey(source_key)).and_then(|i| sources.form.binding_at(i)).map(|b| &b.action), Some(Action::Select(SectionAddress { sid, section: 7, .. })) if sid == &ServerId::from_raw(2)));
        assert!(<LibraryMenu as Focusable<HostFixture>>::place(&sort, &sort_key, cx, At::Drawn).is_some());
        assert!(<LibraryMenu as Focusable<HostFixture>>::place(&filter, &filter_key, cx, At::Drawn).is_some());
        assert!(<LibraryMenu as Focusable<HostFixture>>::place(&sources, &source_key, cx, At::Drawn).is_some());

        let mut output = Vec::new();
        let mut present = plx_machine::present::Present::new();
        sort.step(
            &ScreenEvent::Activate(sort_key),
            cx,
            &mut Effects::new(&mut output, MachineId::Instance(plx_machine::machine::InstanceId(7)), &mut present),
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
            &mut Effects::new(&mut output, MachineId::Instance(plx_machine::machine::InstanceId(7)), &mut present),
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
            &mut Effects::new(&mut output, MachineId::Instance(plx_machine::machine::InstanceId(7)), &mut present),
        );
        assert!(output.iter().any(|effect| matches!(
            &effect.fx,
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::App(AppMsg::LibrarySelect(
                SectionAddress { sid, section: 7, .. }
            )))) if sid == &ServerId::from_raw(2)
        )));
        assert!(output.iter().any(|effect| matches!(
            &effect.fx,
            Fx::Nav(NavOp::Dismiss(EntryId(7)))
        )));
    });
}

#[test]
fn sources_menu_left_is_an_engine_edge_not_an_editor_transition() {
    use plx_ui::focus::{FocusEngine, Outcome};

    let _guard = plx_base::testlock::serial();
    let (groups, sections) = source_sections();
    let mut menu = LibraryMenu::new(EntryId(7), menu_arg(LibraryMenuKind::Sources, [0; 4]));
    menu.apply_draft(source_draft(11, 0, &groups, &sections, &(0..9).collect::<Vec<u32>>()));
    let owner = InputOwner::Entry(EntryId(7));
    with_cx(|cx| {
        let mut engine = FocusEngine::new();
        engine.enter(owner, &menu, plx_ui::screen::FocusTarget::ContainerGroup(GroupId(0)), None, cx);
        let before = engine.current(owner).expect("source menu seats its first row");
        assert!(matches!(engine.move_dir(owner, &menu, &[], Dir::Left, cx), Outcome::Nothing));
        assert_eq!(engine.current(owner), Some(before));

        let mut output = Vec::new();
        let mut present = plx_machine::present::Present::new();
        assert_eq!(
            menu.step(
                &ScreenEvent::Input(plx_machine::machine::InputEvent {
                    at: Tick::default(),
                    source: plx_machine::machine::Source::Script,
                    kind: InputKind::Key {
                        key: Key::Left,
                        sym: 0,
                        wcode: 0,
                        edge: Edge::Down,
                        at_edge: false,
                    },
                }),
                cx,
                &mut Effects::new(&mut output, MachineId::Instance(plx_machine::machine::InstanceId(7)), &mut present),
            ),
            Handled::No
        );
        assert_eq!(menu.kind, LibraryMenuKind::Sources, "LEFT cannot enter an editor");
        assert!(output.is_empty(), "LEFT emits no source/editor action");
    });
}

#[test]
fn open_sources_refreshes_metadata_once_then_settles() {
    let _guard = plx_base::testlock::serial();
    let session = plx_plex::plex::session::TempSession::new("menu-refresh-contract");
    session.watching("u-menu-refresh-contract");
    let stores = plx_data::stores::Stores::default();
    stores.browse.borrow_mut().seed_two_source_table_for_test();
    stores.browse_run_for_test(plx_data::stores::browse::BrowseCmd::SetCur(0));
    let measure = FixtureMeasure;
    let mut first = plx_data::stores::browse::DirectorySnapshot::default();
    let publication = stores.capture_browse(&mut first);
    let listing = publication.listing;
    let hubs = publication.section_hubs;
    let mut menu = LibraryMenu::new(EntryId(7), menu_arg(LibraryMenuKind::Sources, [0; 4]));
    assert_eq!(first.view().current(), Some(0), "the added Movie row must affect the menu's current kind");
    let mut output = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let mut effects = |menu: &mut LibraryMenu, cx: &Cx<'_, HostFixture>| {
        menu.step(
            &ScreenEvent::Tick(Tick { ms: 1, dt_us: 16_000 }),
            cx,
            &mut Effects::new(&mut output, MachineId::Instance(plx_machine::machine::InstanceId(7)), &mut present),
        );
    };

    let cx_first = source_cx(&listing, &first, &hubs, &measure, Tick { ms: 1, dt_us: 16_000 });
    effects(&mut menu, &cx_first);
    let first_key = menu.form.key_at(0).unwrap().0;
    let first_stamp = menu.stamp.clone();
    assert_eq!(menu.draft_rebuilds, 1);
    effects(&mut menu, &cx_first);
    assert_eq!(menu.stamp, first_stamp);
    assert_eq!(menu.form.key_at(0).unwrap().0, first_key, "an unchanged open source menu settles");
    let first_selection = menu.form.table.sel;
    assert_eq!(menu.draft_rebuilds, 1);

    {
        let mut browse = stores.browse.borrow_mut();
        browse.append_section_for_test(1, 3, "New Films", SecKind::Movie);
        browse.set_pinned_for_test(4, true);
    }
    let mut changed = plx_data::stores::browse::DirectorySnapshot::default();
    stores.capture_browse(&mut changed);
    let cx_changed = source_cx(&listing, &changed, &hubs, &measure, Tick { ms: 2, dt_us: 16_000 });
    effects(&mut menu, &cx_changed);
    assert_ne!(menu.stamp, first_stamp, "source metadata refresh rebuilds the open menu");
    assert_eq!(menu.form.key_at(0).unwrap().0, first_key, "refresh preserves the row identity");
    assert_eq!(menu.draft_rebuilds, 2);
    let changed_stamp = menu.stamp.clone();
    effects(&mut menu, &cx_changed);
    assert_eq!(menu.stamp, changed_stamp, "the changed source menu settles after one rebuild");
    assert_eq!(menu.form.key_at(0).unwrap().0, first_key);
    assert_eq!(menu.form.table.sel, first_selection);
    assert_eq!(menu.draft_rebuilds, 2);
}

/// S11 + S24: the +N list opens under its own pill (the same `frame` rule as Sort & Filter: the
/// pill's near edge, 16 below it), and is as wide as its widest row up to the safe width, so a long
/// library name is read whole rather than cut in a 650 px panel with room to spare.
#[test]
fn the_plus_n_list_hangs_from_its_pill_and_fits_its_widest_row() {
    let _guard = plx_base::testlock::serial();
    let (groups, mut sections) = source_sections();
    sections[0].row.title = "A long library name that outgrows the shared".into();
    let anchor = [648.0f32.to_bits(), 148.0f32.to_bits(), 59.0f32.to_bits(), 56.0f32.to_bits()];
    let mut menu = LibraryMenu::new(EntryId(7), menu_arg(LibraryMenuKind::Sources, anchor));
    menu.apply_draft(source_draft(11, 0, &groups, &sections, &(0..9).collect::<Vec<u32>>()));
    with_cx(|cx| {
        let frame = menu.frame(cx.measure);
        let widest = menu.form.table.measured_width(cx.measure);
        assert_eq!(frame.x, 648.0 - plx_ui::widgets::PILL_FOCUS_OUTSET, "the near edge is the +N pill's outer edge");
        assert_eq!(frame.y, 148.0 + 56.0 + 16.0, "16 under the pill, as Sort & Filter");
        assert!(widest > plx_ui::table::MENU_MAX_W, "the fixture name must outgrow the shared cap ({widest})");
        assert!(frame.w >= widest, "the panel fits its widest row ({} < {widest})", frame.w);
        let safe = plx_ui::consts::SAFE;
        assert!(frame.x + frame.w <= safe.x + safe.w + 0.5, "inside the safe frame");
    });
}
