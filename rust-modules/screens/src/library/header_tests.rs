// The library page header (redesign PR4): geometry, controls per kind, the focus table and the
// "focus never switches the library" rule. Included from `navigation_tests` for its `Fixture`.
use super::*;
use super::header::pack_names;
use plx_machine::machine::{InputEvent, InputKind, Source};

fn key_press(key: Key) -> ScreenEvent<TestHost> {
    ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Sdl,
        kind: InputKind::Key { key, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
    })
}

/// A settled grid page (no hub shelves) of `libraries` libraries with the first row focused.
fn grid_page(libraries: usize) -> (Fixture, LibraryScreen, FocusEngine<u32>) {
    let fixture = Fixture::new(libraries, 36, 0);
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    page.sync(&fixture.cx(&engine));
    page.relayout(None);
    let first = page.key(page.pair.detail.elem_at(0).unwrap());
    engine.set(OWNER, first, Some(page.pair.groups_config().detail), By::Restore);
    (fixture, page, engine)
}

fn at(engine: &FocusEngine<u32>) -> u32 { engine.current(OWNER).unwrap().elem }

#[test]
fn the_header_row_stands_at_148_and_the_grid_starts_at_244() {
    let _guard = plx_base::testlock::serial();
    let (fixture, page, engine) = grid_page(2);
    let cx = fixture.cx(&engine);
    let control = <LibraryScreen as Focusable<TestHost>>::place(&page, &SORTFILTER, &cx, At::Drawn).unwrap().rect;
    assert_eq!(control.y + control.h * 0.5, layout::HEADER_Y + plx_ui::page_header::HEIGHT * 0.5);
    assert_eq!(layout::HEADER_Y, 148.0);
    assert_eq!(layout::LIBRARY_GRID_TOP, 244.0);
    assert_eq!(page.layout.row_y(0, 0.0), layout::LIBRARY_GRID_TOP, "the first poster row's top sits 96 under the header row's top");
    let tile = <LibraryScreen as Focusable<TestHost>>::place(&page, &page.pair.detail.elem_at(0).unwrap(), &cx, At::Drawn).unwrap().rect;
    assert!(tile.y <= layout::LIBRARY_GRID_TOP && tile.y + tile.h > layout::LIBRARY_GRID_TOP, "the first tile stands on that row");
}

#[test]
fn the_controls_are_collections_recommended_and_sort_filter_until_collections_is_known_empty() {
    let _guard = plx_base::testlock::serial();
    let (_, mut page, _) = grid_page(1);
    assert_eq!(page.toolbar_elems(), [COLLECTIONS, RECOMMENDED, SORTFILTER], "an unknown count still shows the control");
    for (answer, control) in [
        (None, CollectionsControl::Unknown),
        (Some(Ok(0)), CollectionsControl::Hidden),
        (Some(Err(crate::registry::Forbidden)), CollectionsControl::Hidden),
        (Some(Ok(7)), CollectionsControl::Count(7)),
    ] {
        assert_eq!(CollectionsControl::of(answer), control);
    }
    page.collections = CollectionsControl::Count(7);
    assert_eq!(page.toolbar_elems(), [COLLECTIONS, RECOMMENDED, SORTFILTER]);
    page.collections = CollectionsControl::Hidden;
    assert_eq!(page.toolbar_elems(), [RECOMMENDED, SORTFILTER], "0 collections or a 403 hides the control");
}

#[test]
fn one_library_draws_no_names_and_two_draw_both() {
    let _guard = plx_base::testlock::serial();
    assert!(grid_page(1).1.libraries.is_empty(), "one library: only the three controls");
    assert_eq!(grid_page(2).1.libraries.len(), 2);
}

#[test]
fn the_picked_library_takes_the_last_visible_slot() {
    let widths = [200.0, 180.0, 220.0, 160.0, 240.0];
    assert_eq!(pack_names(&widths, 0, 4000.0, 90.0, 8), [0, 1, 2, 3, 4], "everything fits: all names, in order");
    let tight = 200.0 + 180.0 + 220.0 + 2.0 * plx_ui::page_header::GAP + 90.0 + plx_ui::page_header::GAP + 1.0;
    let cut = pack_names(&widths, 1, tight, 90.0, 8);
    assert_eq!(cut, [0, 1, 2], "a pick inside the cut keeps its place");
    let cut = pack_names(&widths, 4, tight, 90.0, 8);
    assert_eq!(cut.last(), Some(&4), "a pick past the cut replaces the last visible name");
    assert_eq!(cut.len(), 2, "[0, 4] is what the room holds");
    assert_eq!(pack_names(&widths, 3, 10.0, 90.0, 8), [3], "never fewer than the picked library");
    assert!(pack_names(&[], 0, 100.0, 10.0, 8).is_empty());
}

#[test]
fn focus_on_a_library_name_never_switches_the_library() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(3);
    let other = page.libraries.iter().find(|(_, index)| Some(*index) != page.view_section(&fixture.cx(&engine))).unwrap().0;
    let from = engine.current(OWNER);
    engine.set(OWNER, page.key(other), Some(LIBRARY_GROUP), By::Dir);
    let to = page.key(other);
    let apps = fixture.step(&mut page, &mut engine, ScreenEvent::FocusMoved { from, to, by: By::Dir });
    assert!(page.pending.section().is_none(), "landing on a name requests nothing");
    assert!(!page.switched);
    assert!(apps.iter().all(|fx| !matches!(fx, AppFx::Library(LibraryReq::Menu { .. }))));
    fixture.step(&mut page, &mut engine, ScreenEvent::Activate(other));
    assert!(page.pending.section().is_some(), "OK switches");
    assert!(page.switched);
}

#[test]
fn up_lands_on_the_selected_library_then_on_the_last_control_used() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(2);
    let selected = page.libraries.iter().find(|(_, index)| Some(*index) == page.view_section(&fixture.cx(&engine))).unwrap().0;
    fixture.direction(&mut page, &mut engine, Dir::Up);
    assert_eq!(at(&engine), selected, "the first Up lands on the library on view");
    fixture.direction(&mut page, &mut engine, Dir::Right);
    fixture.direction(&mut page, &mut engine, Dir::Right);
    assert_eq!(at(&engine), COLLECTIONS, "Right walks names then the controls");
    fixture.direction(&mut page, &mut engine, Dir::Right);
    assert_eq!(at(&engine), RECOMMENDED);
    fixture.direction(&mut page, &mut engine, Dir::Down);
    assert!(region_of_elem(at(&engine)).is_none() || !page.is_header_elem(at(&engine)), "Down leaves the header for the grid");
    fixture.direction(&mut page, &mut engine, Dir::Up);
    assert_eq!(at(&engine), RECOMMENDED, "after that, Up returns to the last control used");
}

#[test]
fn back_walks_grid_to_header_to_nav_to_home_without_changing_the_library() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(2);
    let library = page.view_section(&fixture.cx(&engine));
    // Deep in the grid: to the top of the page, still on the grid.
    page.scroll_target = 900.0;
    page.scroll.jump(900.0);
    fixture.step(&mut page, &mut engine, key_press(Key::Back));
    assert_eq!(page.scroll_target, 0.0, "deep grid goes to the top of the page");
    // Top of the page: to the header.
    fixture.step(&mut page, &mut engine, key_press(Key::Back));
    assert!(page.is_header_elem(at(&engine)), "the top of the page goes to the header");
    // Header: to the nav.
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&key_press(Key::Back), &fixture.cx(&engine),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(out.iter().any(|e| matches!(&e.fx,
        Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::ContainerGroup(STRIP) }))))),
        "the header goes to the nav");
    assert!(page.pending.section().is_none() && !page.switched, "Back never changes the library");
    assert_eq!(page.view_section(&fixture.cx(&engine)), library);
}

#[test]
fn back_from_the_nav_leaves_for_home() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, _) = grid_page(2);
    let engine = FocusEngine::new(); // nothing of this page holds focus: the nav does
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&key_press(Key::Back), &fixture.cx(&engine),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(out.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Library(LibraryReq::BackToHome { .. })))));
}

/// The right cluster in its widest case leaves the names at least 40% of the row in every shipped
/// language (measured with the device's whole-pixel advances).
#[test]
fn the_header_cluster_leaves_the_names_room_in_every_shipped_language() {
    use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
    let _guard = plx_base::testlock::serial();
    let (fixture, page, engine) = grid_page(1);
    let measure = plx_base::fontcov::advances::ShippedMeasure;
    let mut cx = fixture.cx(&engine);
    cx.measure = &measure;
    let mut out = Vec::new();
    for language in SHIPPED {
        let _lang = language_on_this_thread_for_test(language);
        let (_, widest) = page.header_right(&cx);
        let width = plx_ui::page_header::cluster_width(&widest, &measure);
        let row = plx_ui::page_header::TEXT_X1 - plx_ui::page_header::TEXT_X0;
        if width > row * 0.6 {
            out.push(format!("{}: right cluster {width:.0}px of a {row:.0}px row", language.tag()));
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// The "+N" stays on the row wherever focus sits: only the right cluster is fixed-width, and the
/// fold the left group makes must not depend on which header stop holds focus.
#[test]
fn the_overflow_stays_visible_wherever_focus_is_in_the_row() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(12);
    assert!(page.libraries.iter().any(|(elem, _)| *elem == MORE), "twelve libraries fold into +N");
    let before = page.libraries.clone();
    fixture.direction(&mut page, &mut engine, Dir::Up);
    for _ in 0..16 {
        fixture.direction(&mut page, &mut engine, Dir::Right);
        page.sync(&fixture.cx(&engine));
        assert_eq!(page.libraries, before, "focus on {} changed the fold", at(&engine));
        assert!(page.header.as_ref().unwrap().plate(MORE).is_some());
    }
}
