use super::*;
use crate::browse::LibraryType;

fn tv_fixture(kind: LibraryType, total: usize) -> Fixture {
    let mut fixture = Fixture::new();
    let sid = crate::plex::ServerId::from_raw(0);
    fixture.listing = fixture.listing.with_library_type(kind).with_total(total);
    fixture.directory = crate::browse::view::DirectorySnapshot::fixture(1, 0, vec![
        crate::browse::view::SectionView { sid: Some(sid), key: 1, kind: SecKind::Show,
            row: crate::browse::SrcRow { section: 0, title: "Television".into(), pinned: true, current: true, ..Default::default() } },
    ]);
    fixture
}

fn tv_page(fixture: &Fixture) -> LibraryScreen {
    let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Show);
    page.sync(&fixture.cx(None));
    page
}

#[test]
fn type_selector_is_tv_only_and_opens_its_own_menu() {
    let _guard = crate::testlock::serial();
    let movies = Fixture::new();
    let movie_page = movies.screen();
    assert_eq!(movie_page.toolbar_elems(), [SORT, FILTER]);
    assert!(<LibraryScreen as Focusable<HostFixture>>::place(&movie_page, &TYPE, &movies.cx(None), At::Drawn).is_none());
    let fixture = tv_fixture(LibraryType::Shows, 36);
    let mut page = tv_page(&fixture);
    assert_eq!(page.toolbar_elems(), [TYPE, SORT, FILTER]);
    let mut right = MARGIN_X;
    for &elem in page.toolbar_elems() {
        let rect = page.toolbar_chip_rect(elem, &fixture.cx(None), At::Drawn);
        assert!(rect.x >= right);
        right = rect.x + rect.w;
    }
    assert!(right < layout::GRID_RIGHT);
    let mut output = Vec::new();
    let mut present = crate::ui::present::Present::new();
    page.activate(TYPE, false, &fixture.cx(Some(page.key(TYPE))),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(output.iter().any(|effect| matches!(effect.fx,
        Fx::App(AppFx::Library(LibraryReq::Menu { kind: crate::screens::registry::LibraryMenuKind::Type, .. })))));
}

#[test]
fn episode_navigation_and_page_jumps_follow_four_columns() {
    let _guard = crate::testlock::serial();
    let fixture = tv_fixture(LibraryType::Episodes, 36);
    let mut page = tv_page(&fixture);
    page.initial = false;
    assert_eq!(page.layout.cols(), 4);
    let mut engine = FocusEngine::new();
    let first = page.key(page.pair.detail.elems[0]);
    engine.set(OWNER, first, Some(page.pair.groups_config().detail), By::Restore);
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    assert_eq!(page.grid_position(engine.current(OWNER)), Some((1, 0)));
    assert_eq!(engine.current(OWNER).unwrap().elem, page.pair.detail.elems[4]);
    let focused = engine.current(OWNER).unwrap();
    let rect = <LibraryScreen as Focusable<HostFixture>>::place(&page, &focused.elem, &fixture.cx(Some(focused)), At::SpringTarget).unwrap().rect;
    assert!((rect.w / rect.h - 16.0 / 9.0).abs() < 0.01);
    let mut output = Vec::new();
    let mut present = crate::ui::present::Present::new();
    page.command(LibraryCmd::Page(1), &fixture.cx(Some(focused)),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(output.iter().any(|effect| matches!(&effect.fx,
        Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(key) })))
            if key.elem == page.pair.detail.elems[12])));
}

#[test]
fn empty_episode_results_keep_type_selector_available() {
    let _guard = crate::testlock::serial();
    let fixture = tv_fixture(LibraryType::Episodes, 0);
    let mut page = tv_page(&fixture);
    page.grid_fade = Xfade::new();
    page.page_fade = Xfade::new();
    page.relayout(None);
    assert_eq!(page.readout, Readout::Empty);
    assert!(page.layout.grid_head);
    assert!(<LibraryScreen as Focusable<HostFixture>>::place(&page, &TYPE, &fixture.cx(None), At::Drawn).is_some());
    assert_eq!(page.toolbar_chip(TYPE, &fixture.cx(None)).value.to_str().unwrap(), " · Episodes");
}

#[test]
fn type_menu_command_preserves_plaintext_alert_control_keys() {
    let _guard = crate::testlock::serial();
    assert_ne!(TYPE, PLAINTEXT_CANCEL);
    assert_ne!(TYPE, PLAINTEXT_CONNECT);
    let fixture = tv_fixture(LibraryType::Shows, 36);
    let mut page = tv_page(&fixture);
    let mut output = Vec::new();
    let mut present = crate::ui::present::Present::new();
    assert_eq!(page.command(LibraryCmd::OpenMenu(crate::screens::registry::LibraryMenuKind::Type),
        &fixture.cx(None),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present)), Handled::Yes);
    assert!(output.iter().any(|effect| matches!(effect.fx,
        Fx::App(AppFx::Library(LibraryReq::Menu { kind: crate::screens::registry::LibraryMenuKind::Type, .. })))));
}
