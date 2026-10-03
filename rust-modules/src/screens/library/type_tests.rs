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

/// A movie section listed as its collections: the fixture's Cinema section, 36 slots, the first
/// three of them collection rows.
fn collections_fixture(total: usize) -> Fixture {
    let mut fixture = Fixture::new();
    let sid = crate::plex::ServerId::from_raw(0);
    let rows = (0..total.min(3)).map(|i| crate::pms::PmsMovie {
        sid, rk: format!("{}", 50_001 + i), title: format!("Collection {i}"),
        kind: crate::pms::KIND_COLLECTION, child_count: i as i64 + 1, ..Default::default()
    }).collect();
    fixture.listing = fixture.listing.with_library_type(LibraryType::Collections).with_total(total).with_page(0, rows);
    fixture
}

#[test]
fn every_library_kind_offers_the_type_selector_and_it_opens_its_own_menu() {
    let _guard = plx_base::testlock::serial();
    let movies = Fixture::new();
    let movie_page = movies.screen();
    assert_eq!(movie_page.toolbar_elems(), [TYPE, SORT, FILTER], "Movies / Collections is a real choice");
    assert!(<LibraryScreen as Focusable<HostFixture>>::place(&movie_page, &TYPE, &movies.cx(None), At::Drawn).is_some());
    assert_eq!(movie_page.toolbar_chip(TYPE, &movies.cx(None)).value.to_str().unwrap(), " · Movies");
    let fixture = tv_fixture(LibraryType::Primary, 36);
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
    let mut present = plx_machine::present::Present::new();
    page.activate(TYPE, false, &fixture.cx(Some(page.key(TYPE))),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(output.iter().any(|effect| matches!(effect.fx,
        Fx::App(AppFx::Library(LibraryReq::Menu { kind: crate::screens::registry::LibraryMenuKind::Type, .. })))));
}

#[test]
fn episode_navigation_and_page_jumps_follow_four_columns() {
    let _guard = plx_base::testlock::serial();
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
    let mut present = plx_machine::present::Present::new();
    page.command(LibraryCmd::Page(1), &fixture.cx(Some(focused)),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(output.iter().any(|effect| matches!(&effect.fx,
        Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(key) })))
            if key.elem == page.pair.detail.elems[12])));
}

#[test]
fn empty_episode_results_keep_type_selector_available() {
    let _guard = plx_base::testlock::serial();
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
    let _guard = plx_base::testlock::serial();
    assert_ne!(TYPE, PLAINTEXT_CANCEL);
    assert_ne!(TYPE, PLAINTEXT_CONNECT);
    let fixture = tv_fixture(LibraryType::Primary, 36);
    let mut page = tv_page(&fixture);
    let mut output = Vec::new();
    let mut present = plx_machine::present::Present::new();
    assert_eq!(page.command(LibraryCmd::OpenMenu(crate::screens::registry::LibraryMenuKind::Type),
        &fixture.cx(None),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present)), Handled::Yes);
    assert!(output.iter().any(|effect| matches!(effect.fx,
        Fx::App(AppFx::Library(LibraryReq::Menu { kind: crate::screens::registry::LibraryMenuKind::Type, .. })))));
}

#[test]
fn empty_tv_library_names_the_selected_listing_type() {
    let _guard = plx_base::testlock::serial();
    for (kind, expected) in [
        (LibraryType::Primary, "No shows in Television"),
        (LibraryType::Seasons, "No seasons in Television"),
        (LibraryType::Episodes, "No episodes in Television"),
    ] {
        let fixture = tv_fixture(kind, 0);
        let mut page = tv_page(&fixture);
        page.wanted_kind = None;
        page.readout = Readout::Empty;
        let (caption, reason) = page.status_text(&fixture.cx(None));
        assert_eq!(caption.to_str().unwrap(), expected);
        assert!(reason.is_none());
    }
}

/// **Collections take no filters.** The listing carries neither the Unwatched nor the Genre
/// filter, so the heading row drops FILTER rather than offering a control that changes nothing,
/// and the TYPE chip names what is listed.
#[test]
fn a_collections_listing_hides_the_filter_control() {
    let _guard = plx_base::testlock::serial();
    let fixture = collections_fixture(36);
    let page = fixture.screen();
    assert_eq!(page.listed(), LibraryType::Collections);
    assert_eq!(page.toolbar_elems(), [TYPE, SORT]);
    assert!(<LibraryScreen as Focusable<HostFixture>>::place(&page, &FILTER, &fixture.cx(None), At::Drawn).is_none());
    assert_eq!(page.toolbar_chip(TYPE, &fixture.cx(None)).value.to_str().unwrap(), " · Collections");
}

/// An empty Collections answer is the ordinary empty read-out, worded for what was asked for, and
/// it keeps the heading row so TYPE can leave it.
#[test]
fn an_empty_collections_listing_says_so_and_keeps_the_type_selector() {
    let _guard = plx_base::testlock::serial();
    let fixture = collections_fixture(0);
    let mut page = fixture.screen();
    page.grid_fade = Xfade::new();
    page.page_fade = Xfade::new();
    page.relayout(None);
    assert_eq!(page.readout, Readout::Empty);
    assert!(page.layout.grid_head);
    assert!(<LibraryScreen as Focusable<HostFixture>>::place(&page, &TYPE, &fixture.cx(None), At::Drawn).is_some());
    let (caption, reason) = page.status_text(&fixture.cx(None));
    assert_eq!(caption.to_str().unwrap(), "No collections in Cinema");
    assert!(reason.is_none());
}

/// OK on a collection card asks for its page: the request names the row, and the app's shared
/// `activate_card` routes kind 4 to `ContentArg::Collection` (pinned in `app/input.rs`).
#[test]
fn ok_on_a_collection_card_requests_its_page() {
    let _guard = plx_base::testlock::serial();
    let fixture = collections_fixture(36);
    let mut page = fixture.screen();
    page.initial = false;
    let elem = page.pair.detail.elems[1];
    let mut output = Vec::new();
    let mut present = plx_machine::present::Present::new();
    assert_eq!(page.activate(elem, false, &fixture.cx(Some(page.key(elem))),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present)), Handled::Yes);
    assert!(output.iter().any(|effect| matches!(&effect.fx,
        Fx::App(AppFx::Library(LibraryReq::Detail { rk, .. })) if rk == "50002")),
        "OK on a collection opens it, never plays it");
}

/// The dev/scenario path to a listing type: `SetType` is the menu row's own edit, addressed to the
/// section on the page.
#[test]
fn the_set_type_command_is_the_menu_rows_edit() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    let mut output = Vec::new();
    let mut present = plx_machine::present::Present::new();
    assert_eq!(page.command(LibraryCmd::SetType(LibraryType::Collections), &fixture.cx(None),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present)), Handled::Yes);
    assert!(matches!(page.pending.grid(), Some((_, GridAction::LibraryType(LibraryType::Collections)))),
        "the command stages the same grid transaction the TYPE menu's row does");
}

/// **An empty answer under shelves stands below its heading, not on a shelf.** The fixed status
/// region sits a third of the way down the panel, which with shelves above the grid is the middle
/// of a shelf: the read-out drew over the posters. The document reserves one grid pitch under the
/// heading row instead, and the read-out stands in it and scrolls with the page.
#[test]
fn an_empty_answer_under_shelves_stands_below_its_heading() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::shelves(&["Recently Added"], 6);
    fixture.listing = fixture.listing.clone().with_library_type(LibraryType::Collections).with_fetch(SecFetch::Ready, 0);
    let mut page = fixture.screen();
    page.grid_fade = Xfade::new();
    page.page_fade = Xfade::new();
    page.relayout(None);
    assert_eq!(page.readout, Readout::Empty);
    assert_eq!(page.shelves.len(), 1);
    let (top, h) = page.layout.empty_band().expect("an empty answer reserves its band");
    assert_eq!(top, page.layout.grid_top(), "directly under the heading row");
    assert!(page.layout.doc_h() >= top + h, "the band is part of the document, so it can scroll into view");
    let frame = page.status_frame();
    let shelf_bottom = page.layout.shelf_y(0, page.scroll.pos) + page.layout.shelf_pitch(0);
    assert!(frame.y >= shelf_bottom, "the read-out ({}) must not overlap the shelf above ({shelf_bottom})", frame.y);
}
