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
    let gap = plx_ui::page_header::gap_between;
    let tight = 200.0 + 180.0 + 220.0 + gap(200.0, 180.0) + gap(180.0, 220.0) + 90.0 + gap(220.0, 90.0) + 1.0;
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

#[test]
fn a_one_library_row_stands_at_y_158_and_a_multi_library_row_at_148() {
    let _guard = plx_base::testlock::serial();
    for (libraries, top) in [(1, 158.0), (2, 148.0)] {
        let (fixture, page, engine) = grid_page(libraries);
        let cx = fixture.cx(&engine);
        let control = <LibraryScreen as Focusable<TestHost>>::place(&page, &SORTFILTER, &cx, At::Drawn).unwrap().rect;
        assert_eq!(control.y, top, "{libraries} libraries");
    }
}

#[test]
fn the_plus_n_list_holds_exactly_the_libraries_the_fold_hid() {
    let _guard = plx_base::testlock::serial();
    let (fixture, page, engine) = grid_page(12);
    let shown: Vec<u32> = page.libraries.iter().filter(|(elem, _)| *elem != MORE).map(|(_, index)| *index as u32).collect();
    let current = page.view_section(&fixture.cx(&engine)).unwrap() as u32;
    assert!(!page.folded.is_empty());
    assert_eq!(page.folded.len() + shown.len(), 12, "every library is either a name or in the list");
    assert!(page.folded.iter().all(|index| !shown.contains(index)), "no visible name is repeated in the list");
    assert!(!page.folded.contains(&current), "nor the library on view");
}

/// **A control a menu has lifted wears no pill** (board: "Sort & Filter" is plain text over its
/// open panel); every other focused control still does.
#[test]
fn a_control_its_menu_has_lifted_draws_no_focus_pill() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(2);
    let key = page.key(SORTFILTER);
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    let pill = page.header.as_ref().unwrap().plate(SORTFILTER).unwrap().scaled(plx_ui::widgets::CTRL_FOCUS_SCALE);
    let drawn = |page: &LibraryScreen, lifted: Option<FocusKey<u32>>| {
        let cx = fixture.cx(&engine);
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        frame.lifted = lifted;
        plx_ui::draw_census::capture(|| page.draw_header(&frame)).iter()
            .filter(|(_, rect)| (rect.w - pill.w).abs() < 0.5 && (rect.h - pill.h).abs() < 0.5).count()
    };
    assert_eq!(drawn(&page, None), 1, "a focused control wears the pill");
    assert_eq!(drawn(&page, Some(key)), 0, "its menu lifted it: no plate under the scrim");
    page.scroll.jump(0.0);
}

/// The kinds one descriptive draw of `page`, scrolled one grid pitch (the chrome fully tucked),
/// declares: a capsule is a `glass` paint (the nav pill's live material).
fn tucked_draw_kinds(fixture: &Fixture, page: &mut LibraryScreen, engine: &FocusEngine<u32>) -> Vec<(String, [f32; 4])> {
    use plx_ui::screen::Screen;
    let pitch = page.layout.grid_pitch();
    page.scroll.jump(pitch);
    page.scroll_target = pitch;
    let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_ui::frame::backdrop::Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = plx_ui::frame::backdrop::discover(sources.clone());
        let cx = fixture.cx(engine);
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::root());
        page.draw(&mut frame);
    }
    let paints = sources.borrow().paints.iter().map(|p| (p.kind().to_owned(), p.placed())).collect();
    paints
}

#[test]
fn a_scrolled_page_wears_the_capsule_only_when_its_header_names_libraries() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut one, engine) = grid_page(1);
    let kinds = tucked_draw_kinds(&fixture, &mut one, &engine);
    assert!(kinds.iter().any(|(k, r)| k == "rect" && r[3] == plx_ui::scroll_capsule::FADE_H), "the top fade stays");
    assert!(kinds.iter().all(|(k, _)| k != "glass"), "one library: no names in the header, no capsule");
    let (fixture, mut two, engine) = grid_page(2);
    let kinds = tucked_draw_kinds(&fixture, &mut two, &engine);
    assert!(kinds.iter().any(|(k, r)| k == "glass" && r[1] == plx_ui::scroll_capsule::Y), "two libraries: the capsule, as live glass");
    assert!(kinds.iter().all(|(k, _)| k != "glass_frozen"));
}
