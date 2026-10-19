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
    let (fixture, mut page, mut engine) = grid_page(2);
    // The nav holds focus: a strip key, which carries THIS page's entry (an empty engine, where
    // nothing of the page holds focus, is a different case and passed on a fiction).
    let pill = FocusKey { entry: page.entry, elem: plx_ui::widgets::TAB_KEY_MOVIES };
    engine.set(OWNER, pill, Some(STRIP), By::Dir);
    assert!(pill.elem >= plx_ui::dispatch::STRIP_BASE && engine.current(OWNER) == Some(pill));
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&key_press(Key::Back), &fixture.cx(&engine),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(out.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Library(LibraryReq::BackToHome { .. })))),
        "BACK on a nav pill leaves for Home; it does not seat the header again");
    assert!(!out.iter().any(|e| matches!(&e.fx, Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(_))))));
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
        assert!(page.header.row().unwrap().plate(MORE).is_some());
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
    // The pill is the row's motion: it fades in under the focused control over a few frames.
    for frame in 1..=60 { fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 })); }
    let pill = page.header.row().unwrap().plate(SORTFILTER).unwrap().scaled(plx_ui::widgets::CTRL_FOCUS_SCALE);
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
    let pitch = page.layout.grid_pitch();
    draw_kinds_at(fixture, page, engine, pitch)
}

/// The kinds one descriptive draw of `page` declares with the page scrolled to exactly `scroll`.
fn draw_kinds_at(fixture: &Fixture, page: &mut LibraryScreen, engine: &FocusEngine<u32>, scroll: f32) -> Vec<(String, [f32; 4])> {
    use plx_ui::screen::Screen;
    page.scroll.jump(scroll);
    page.scroll_target = scroll;
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

/// **A two-library page declares no live glass while its chrome is still tucking.** The chrome
/// band (the nav pill's live glass) and the capsule are two live glasses whose blur footprints
/// meet; sharing a frame makes the band capture inline in the visible pass, +1.4M GPU cycles per
/// frame and a quarter of the ramp's frames late on the television. So the tuck window hands the
/// place over in sequence: the chrome leaves first (`tuck` reaches 1), and only then does the
/// capsule arrive (`scroll_chrome::capsule_alpha`). At every scroll of the window the page
/// declares the capsule only once the chrome has gone, and a page that has fully tucked declares
/// exactly one, at the capsule's place.
#[test]
fn a_two_library_page_declares_no_live_glass_while_its_chrome_is_still_tucking() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, engine) = grid_page(2);
    let pitch = page.layout.grid_pitch();
    let (mut tucking, mut arriving) = (0, 0);
    let mut s = pitch - plx_ui::scroll_chrome::TUCK_RAMP;
    while s <= pitch {
        let kinds = draw_kinds_at(&fixture, &mut page, &engine, s);
        let glass = kinds.iter().filter(|(k, _)| k == "glass").count();
        let tuck = <LibraryScreen as plx_ui::screen::Screen<TestHost>>::chrome_tuck(&page);
        if tuck < 1.0 {
            tucking += 1;
            assert_eq!(glass, 0, "scroll {s}: the chrome is at tuck {tuck}, still on screen, so the capsule is not");
        } else if glass > 0 {
            arriving += 1;
        }
        s += 5.0;
    }
    assert!(tucking >= 2 && arriving >= 2, "premise: the window has frames of each half ({tucking} tucking, {arriving} arriving)");
    let kinds = draw_kinds_at(&fixture, &mut page, &engine, pitch);
    let capsules: Vec<_> = kinds.iter().filter(|(k, _)| k == "glass").collect();
    assert_eq!(capsules.len(), 1, "fully tucked: exactly one live glass");
    assert_eq!(capsules[0].1[1], plx_ui::scroll_capsule::Y, "and it is the capsule");
}

/// One frame of the page: its `Tick`, then the header drawn at that moment, as its rounded rects.
fn header_rrects_after_a_frame(fixture: &Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, ms: u32) -> Vec<plx_ui::Rect> {
    fixture.step(page, engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
    let cx = fixture.cx(engine);
    let frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
    plx_ui::draw_census::capture(|| page.draw_header(&frame)).into_iter().filter(|(tag, _)| *tag == 2).map(|(_, rect)| rect).collect()
}

/// The focus pill (the one rounded rect taller than the 56 px plates, since it grows by
/// `CTRL_FOCUS_SCALE`) after one frame, as its centre x; `None` when no pill is drawn.
fn pill_centre_after_a_frame(fixture: &Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, ms: u32) -> Option<f32> {
    let pills: Vec<f32> = header_rrects_after_a_frame(fixture, page, engine, ms).iter()
        .filter(|rect| rect.h > plx_ui::page_header::HEIGHT + 1.0 && rect.h < plx_ui::page_header::HEIGHT * 1.2)
        .map(|rect| rect.cx()).collect();
    assert!(pills.len() <= 1, "one focused control wears one pill: {pills:?}");
    pills.first().copied()
}

/// **The header's focus pill travels** (owner: "pills have no transitions, look raw"). Right from
/// one library name to the next slides the pill across the gap on the top bar's spring, one step
/// per frame and never backwards, and it comes to rest on the new name; before, it was redrawn on
/// the new plate in a single frame.
#[test]
fn the_header_focus_pill_travels_to_the_next_name_instead_of_jumping() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(3);
    let (first, second) = (page.libraries[0].0, page.libraries[1].0);
    let key = page.key(first);
    engine.set(OWNER, key, Some(LIBRARY_GROUP), By::Dir);
    fixture.step(&mut page, &mut engine, ScreenEvent::FocusMoved { from: None, to: key, by: By::Dir });
    let mut ms = 0;
    let mut rest = None;
    for _ in 0..120 { ms += 16; rest = pill_centre_after_a_frame(&fixture, &mut page, &mut engine, ms); }
    let plates = page.header.row().unwrap();
    let (from, to) = (plates.plate(first).unwrap().cx(), plates.plate(second).unwrap().cx());
    assert!(to > from + 40.0, "two names with room between them");
    assert!((rest.expect("a focused name wears the pill") - from).abs() < 1.0, "settled on the first name");

    fixture.direction(&mut page, &mut engine, Dir::Right);
    assert_eq!(at(&engine), second);
    let mut path = Vec::new();
    for _ in 0..90 { ms += 16; path.push(pill_centre_after_a_frame(&fixture, &mut page, &mut engine, ms).expect("the pill stays drawn")); }
    assert!(path.windows(2).all(|w| w[1] >= w[0] - 0.01), "the pill never goes backwards: {path:?}");
    assert!(path.iter().any(|x| *x > from + 4.0 && *x < to - 4.0),
        "some frame draws the pill between the two names (it jumped): {path:?}");
    assert!((path.last().unwrap() - to).abs() < 1.0, "it comes to rest on the new name: {path:?}");
}

/// **Switching library keeps the row's motion** (owner: "pills have no transitions"; the screen
/// holds the capsules, never the built row). OK on the name the pill sits on switches the library:
/// the selection plate lands under the pill on that frame, and the pill neither restarts from the
/// left, nor fades, nor moves; the row is rebuilt for the new library and its motion state is not.
#[test]
fn a_library_switch_keeps_the_rows_pill_and_lands_the_plate_under_it() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(3);
    let current = page.view_section(&fixture.cx(&engine));
    let other = page.libraries.iter().find(|(_, index)| Some(*index) != current).unwrap().0;
    let (from, to) = (None, page.key(other));
    engine.set(OWNER, to, Some(LIBRARY_GROUP), By::Dir);
    fixture.step(&mut page, &mut engine, ScreenEvent::FocusMoved { from, to, by: By::Dir });
    let mut ms = 0;
    let mut rest = None;
    for _ in 0..90 { ms += 16; rest = pill_centre_after_a_frame(&fixture, &mut page, &mut engine, ms); }
    let rest = rest.expect("a focused name wears the pill");
    let plate = page.header.row().unwrap().plate(other).unwrap();
    assert!((rest - plate.cx()).abs() < 0.5, "settled under the name that will be picked");
    let selected_under = |rects: &[plx_ui::Rect]| rects.iter().any(|r| (r.h - plx_ui::page_header::HEIGHT).abs() < 0.01 && (r.cx() - plate.cx()).abs() < 1.0);
    assert!(!selected_under(&header_rrects_after_a_frame(&fixture, &mut page, &mut engine, ms + 16)), "premise: the old library wears the plate");

    let motion = |page: &LibraryScreen| {
        let mut canon = plx_machine::machine::Canon::new();
        page.header_capsules.write_motion(&mut canon);
        page.header_pop.write_motion(&mut canon);
        canon.finish()
    };
    let before = motion(&page);
    fixture.step(&mut page, &mut engine, ScreenEvent::Activate(other));
    assert!(page.switched, "OK switched the library");
    assert_eq!(motion(&page), before, "the switch itself touches no motion state: the row is rebuilt, its capsules are not");
    for frame in 1..=30 {
        let rects = header_rrects_after_a_frame(&fixture, &mut page, &mut engine, ms + 16 * (frame + 1));
        let pill = rects.iter().filter(|r| r.h > plx_ui::page_header::HEIGHT + 1.0).map(|r| r.cx()).collect::<Vec<_>>();
        assert_eq!(pill.len(), 1, "frame {frame}: the pill stays drawn through the switch: {rects:?}");
        assert!((pill[0] - rest).abs() < 0.5, "frame {frame}: and stays where it was ({} vs {rest})", pill[0]);
        // Frame 5 is where `flush` hands the pick to the store and `pending` is consumed. The
        // fixture has no store behind it to move the directory's current library, so the page then
        // reads the old library again; while the switch is pending, the plate is under the name.
        assert!(frame > 4 || selected_under(&rects), "frame {frame}: the plate is under the picked name from the first frame");
    }
}

/// **While the Sort & Filter menu is up, the control's accent pill is not under its panel.** The
/// engine's focus is on the menu's own rows (another entry), so the row's motion HOLDS (a pill
/// that faded meanwhile would have to fade back in when the closing morph hands over), and the
/// page must still draw it out: the control is lifted, the menu is its only copy.
#[test]
fn the_held_focus_pill_is_not_drawn_under_an_open_menu_and_is_still_up_when_it_closes() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(2);
    let key = page.key(SORTFILTER);
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    for frame in 1..=60 { fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 })); }
    let pill_w = page.header.row().unwrap().plate(SORTFILTER).unwrap().w * plx_ui::widgets::CTRL_FOCUS_SCALE;
    let pills = |page: &LibraryScreen, engine: &FocusEngine<u32>, lifted: Option<FocusKey<u32>>| {
        let cx = fixture.cx(engine);
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        frame.lifted = lifted;
        plx_ui::draw_census::capture(|| page.draw_header(&frame)).iter()
            .filter(|(tag, rect)| *tag == 2 && (rect.w - pill_w).abs() < 0.5).count()
    };
    assert_eq!(pills(&page, &engine, None), 1, "premise: the focused control wears the pill");
    let motion = |page: &LibraryScreen| {
        let mut canon = plx_machine::machine::Canon::new();
        page.header_capsules.write_motion(&mut canon);
        page.header_pop.write_motion(&mut canon);
        canon.finish()
    };
    let up = motion(&page);

    // The menu opens: its rows hold focus (another entry) and the page lifts the control.
    engine.set(OWNER, FocusKey { entry: EntryId(ENTRY.0 + 1), elem: 0 }, None, By::Dir);
    for frame in 61..=120 { fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 })); }
    assert_eq!(pills(&page, &engine, Some(key)), 0, "its menu lifted it: no pill under the panel");
    assert_eq!(motion(&page), up, "the row's motion held while a surface above had focus");

    // The menu closes: focus is back on the control, the page draws it, and the pill is still up.
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    assert_eq!(pills(&page, &engine, None), 1, "the first frame after the menu is the pill, not a fade-in");
}

/// **The header row is built once per key, not once per frame.** `sync` runs every frame; the row
/// (text measurement, packing) is rebuilt only when something it is made of changed: a library
/// switch builds exactly one, and the frames either side of it build none. The same rule holds the
/// Recommended page's and the Watchlist's rows (`plx_ui::page_header::RowCache`).
#[test]
fn the_header_row_is_built_once_per_key_not_once_per_frame() {
    let _guard = plx_base::testlock::serial();
    let (mut fixture, mut page, engine) = grid_page(2);
    let built = page.header.builds();
    assert!(built >= 1, "premise: the row is built");
    for _ in 0..60 { page.sync(&fixture.cx(&engine)); }
    assert_eq!(page.header.builds(), built, "sixty idle frames build no row");
    fixture.publish(1, 36);
    page.sync(&fixture.cx(&engine));
    assert_eq!(page.header.builds(), built + 1, "a library switch builds the row once");
    for _ in 0..60 { page.sync(&fixture.cx(&engine)); }
    assert_eq!(page.header.builds(), built + 1, "and the frames after it build none");
}

/// One frame of the page, then every text run the header draws at that moment.
fn header_runs_after_a_frame(fixture: &Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, ms: u32) -> Vec<plx_ui::draw_census::Run> {
    fixture.step(page, engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
    let cx = fixture.cx(engine);
    let frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
    plx_ui::draw_census::capture_runs(|| page.draw_header(&frame))
}

/// What the Collections control looks like on one frame: where its label stands, and how opaque each
/// number beside it is, as `(label x, [(number, alpha)])`.
fn collections_face(runs: &[plx_ui::draw_census::Run], numbers: &[&str]) -> Option<(f32, Vec<(String, f32)>)> {
    let label = runs.iter().find(|run| run.text == "Collections")?;
    let counts = runs.iter().filter(|run| numbers.contains(&run.text.as_str()))
        .map(|run| (run.text.clone(), run.ink[3])).collect();
    Some((label.at.x, counts))
}

/// **A library switch moves nothing in the Collections control** (spec: "OK on a library: ... count
/// and Collections number cross-fade ... Nothing resizes"). The row follows the press at once while
/// the probe is still answering for the library it left, and the control used to read "Collections"
/// with no number for that whole window (its label jumped 37 px, then jumped back when the number
/// landed). The count it had stays until the new one is known, then the two cross-fade in place.
#[test]
fn a_library_switch_keeps_the_collections_label_still_and_cross_fades_its_count() {
    let _guard = plx_base::testlock::serial();
    COLLECTION_ANSWERS.with(|answers| answers.borrow_mut().clear());
    land_collections(1, Ok(4));
    let (mut fixture, mut page, mut engine) = grid_page(2);
    let current = page.view_section(&fixture.cx(&engine));
    let other = page.libraries.iter().find(|(_, index)| Some(*index) != current).unwrap().0;
    let mut ms = 0;
    let mut rest = None;
    for _ in 0..10 { ms += 16; rest = collections_face(&header_runs_after_a_frame(&fixture, &mut page, &mut engine, ms), &["4", "2"]); }
    let (label_x, counts) = rest.expect("premise: the control is drawn");
    assert_eq!(counts, [("4".to_string(), 1.0)], "premise: it wears the count the first library has");

    fixture.step(&mut page, &mut engine, ScreenEvent::Activate(other));
    let mut shown = Vec::new();
    for frame in 1..=60 {
        ms += 16;
        // The press reaches the store (the directory's current library moves) a few frames in, and
        // the new library's probe lands a few frames after that.
        if frame == 6 { fixture.publish(1, 36); }
        if frame == 12 { land_collections(2, Ok(2)); }
        let face = collections_face(&header_runs_after_a_frame(&fixture, &mut page, &mut engine, ms), &["4", "2"]);
        let (x, counts) = face.unwrap_or_else(|| panic!("frame {frame}: the Collections control vanished"));
        assert!((x - label_x).abs() < 0.01, "frame {frame}: the label moved ({label_x} -> {x}), counts {counts:?}");
        let visible: f32 = counts.iter().map(|(_, alpha)| *alpha).sum();
        assert!(visible >= 0.99, "frame {frame}: the count dipped to {visible} between two counts: {counts:?}");
        shown.push(counts);
    }
    assert_eq!(shown.first().unwrap().first().map(|(text, _)| text.as_str()), Some("4"), "the old count stays while the new one is unknown");
    assert!(shown.iter().any(|counts| counts.len() == 2), "the two numbers share frames: that is a cross-fade, not a cut: {shown:?}");
    assert_eq!(shown.last().unwrap(), &[("2".to_string(), 1.0)], "and it settles on the new count");
    COLLECTION_ANSWERS.with(|answers| answers.borrow_mut().clear());
}
