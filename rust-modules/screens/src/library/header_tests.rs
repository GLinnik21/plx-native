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

/// Forget what the fixture's probe has learned, then let it learn `answers` (section key, answer):
/// the library whose key is not named is one whose probe has not answered.
fn knows_collections(answers: &[(u32, Result<u32, crate::registry::Forbidden>)]) {
    COLLECTION_ANSWERS.with(|learned| learned.borrow_mut().clear());
    for &(key, answer) in answers { land_collections(key, answer); }
}

/// **The Collections control is on the row only with a count the page KNOWS for this library**
/// ("Collections always shows its count"). A probe that has not answered is no control at all: it
/// used to read as a bare "Collections", which the next answer then fixed by a number appearing
/// (or by the whole control leaving, for a library with none): a flash of a control that was
/// never going to stay as drawn. 0 collections or a 403 hide it, a count shows it. (This replaces
/// `the_controls_are_collections_recommended_and_sort_filter_until_collections_is_known_empty`,
/// whose first assertion was "an unknown count still shows the control".)
#[test]
fn the_collections_control_is_on_the_row_only_with_a_known_count_for_this_library() {
    let _guard = plx_base::testlock::serial();
    knows_collections(&[]);
    let (fixture, mut page, engine) = grid_page(1);
    assert_eq!(page.collections, CollectionsControl::Unknown, "premise: the probe has not answered");
    assert_eq!(page.toolbar_elems(), [RECOMMENDED, SORTFILTER], "an unknown count is not a control on the row");
    let collections = plx_platform::i18n::msg::library_header_collections();
    let labels = |page: &LibraryScreen| page.header.row().unwrap().page.spec().right.iter().map(|item| item.label.clone()).collect::<Vec<_>>();
    assert!(!labels(&page).iter().any(|label| label == collections), "and it is not laid out: {:?}", labels(&page));
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
    // Through the page's own refresh: the count lands, the control joins the row.
    knows_collections(&[(1, Ok(7))]);
    page.sync(&fixture.cx(&engine));
    assert_eq!(page.collections, CollectionsControl::Count(7));
    assert_eq!(page.toolbar_elems(), [COLLECTIONS, RECOMMENDED, SORTFILTER]);
    assert!(labels(&page).iter().any(|label| label == collections));
}

/// **One rule spaces the right cluster** (the owner's photo: `Collections · 4` and `Recommended`
/// stood a wide gap apart and `Recommended` and `Sort & Filter` a narrow one). Every adjacent pair
/// of the cluster's items stands exactly one `GAP` apart on the drawn text, whatever the count
/// is, or whether the control is there at all. (The slot the count is placed for sizes only the
/// budget the names pack into.)
#[test]
fn every_gap_of_the_right_cluster_is_one_gap_whatever_the_collections_count_is() {
    use plx_ui::page_header::{Slot, GAP};
    let _guard = plx_base::testlock::serial();
    for answer in [Ok(4), Ok(6), Ok(123), Ok(1_234), Ok(0)] {
        knows_collections(&[(1, answer)]);
        let (_, page, _) = grid_page(2);
        let row = page.header.row().unwrap();
        let cluster: Vec<_> = row.layout.items.iter()
            .filter_map(|rect| match rect.slot { Slot::Right(i) => Some((rect, &row.page.spec().right[i])), _ => None }).collect();
        assert_eq!(cluster.len(), if answer == Ok(0) { 2 } else { 3 }, "{answer:?}: the controls on the row");
        for pair in cluster.windows(2) {
            let gap = pair[1].0.text.x - (pair[0].0.text.x + pair[0].0.text.w);
            assert!((gap - GAP).abs() < 0.01, "{answer:?}: {} -> {} is {gap} apart, not {GAP}", pair[0].1.label, pair[1].1.label);
        }
        let last = cluster.last().unwrap().0.text;
        assert!((last.x + last.w - plx_ui::page_header::TEXT_X1).abs() < 0.01, "{answer:?}: the cluster ends at the right text edge");
    }
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
    let gap = plx_ui::page_header::GAP;
    let tight = 200.0 + 180.0 + 220.0 + gap + gap + 90.0 + gap + 1.0;
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
    knows_collections(&[(1, Ok(3))]);
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

/// **The Collections count reserves THREE digits in the widest case**, which sizes the budget the
/// names pack into (design review, judgment 2: a four-digit reserve is too much room taken from
/// the names). A count that needs more digits widens ONLY its own library's widest case. The
/// reserve is the cluster's BUDGET, not air in the row (`every_gap_of_the_right_cluster_...`): a
/// library whose count is not known yet still reserves it (its control will land), and so does one
/// known to have none (a cold cache reads Unknown for a round trip and then Hidden, and the names
/// must not repack between the two: `the_names_do_not_repack_when_the_answer_arrives`). (Rewritten from
/// `the_collections_count_slot_reserves_three_digits_...`, which also pinned the reserve as the
/// width the LIVE item was placed for.)
#[test]
fn the_collections_widest_case_reserves_three_digits_and_a_longer_count_widens_only_its_own() {
    use plx_platform::i18n::{language_on_this_thread_for_test, Preference};
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(Preference::En);
    let (fixture, mut page, engine) = grid_page(1);
    let slots = |page: &LibraryScreen| {
        let (right, widest) = page.header_right(&fixture.cx(&engine));
        let of = |items: &[plx_ui::page_header::HeaderItem]| items.iter().find_map(|i| i.value_slot.clone());
        (of(&right), of(&widest))
    };
    let three = Some("999".to_string());
    for (count, on_row, in_budget) in [
        (CollectionsControl::Unknown, false, true),
        (CollectionsControl::Count(7), true, true),
        (CollectionsControl::Count(999), true, true),
        (CollectionsControl::Hidden, false, true),
    ] {
        page.collections = count;
        assert_eq!(slots(&page), (on_row.then(|| three.clone()).flatten(), in_budget.then(|| three.clone()).flatten()), "{count:?}");
    }
    page.collections = CollectionsControl::Count(1_284);
    assert_eq!(slots(&page), (Some("1,284".into()), Some("1,284".into())), "four digits widen this library's widest case");
}

/// **The names are packed against a budget that does not depend on the Collections answer**: a
/// library on a cold cache reads Unknown for one round trip and then Hidden (none, or a 403) or a
/// count, and the names, the "+N" and the room they were packed into stay exactly as they were
/// (the flash the owner reported, one level down: the control was never drawn, yet the names
/// repacked under the reader). Pinned on the budget itself, which any width of the names could
/// hide, and on the packing with a directory long enough to fold.
#[test]
fn the_names_do_not_repack_when_the_answer_arrives() {
    let _guard = plx_base::testlock::serial();
    let packed = |answers: &[(u32, Result<u32, crate::registry::Forbidden>)]| {
        knows_collections(answers);
        let (_, page, _) = grid_page(12);
        let row = page.header.row().unwrap();
        (row.layout.left_budget, page.libraries.clone(), page.folded.clone())
    };
    let unknown = packed(&[]);
    assert!(unknown.1.iter().any(|(elem, _)| *elem == MORE), "premise: twelve libraries fold into +N");
    for (what, answers) in [
        ("none", vec![(1, Ok(0))]),
        ("a refusal", vec![(1, Err(crate::registry::Forbidden))]),
        ("a count", vec![(1, Ok(7))]),
        ("a three-digit count", vec![(1, Ok(999))]),
    ] {
        let after = packed(&answers);
        assert_eq!(after.0, unknown.0, "{what}: the room the names are packed into moved");
        assert_eq!(after.1, unknown.1, "{what}: the names that fit changed");
        assert_eq!(after.2, unknown.2, "{what}: the +N list changed");
    }
    // The same page, the answer landing under it: the names it holds do not change.
    knows_collections(&[]);
    let (fixture, mut page, engine) = grid_page(12);
    let (shown, folded) = (page.libraries.clone(), page.folded.clone());
    land_collections(1, Ok(0));
    page.sync(&fixture.cx(&engine));
    assert_eq!((&page.libraries, &page.folded), (&shown, &folded), "a library with none repacked its names");
    land_collections(1, Ok(7));
    page.sync(&fixture.cx(&engine));
    assert_eq!((&page.libraries, &page.folded), (&shown, &folded), "a library with a count repacked its names");
    knows_collections(&[]);
}

/// **A count the page knew for THIS library does not leave the row because the probe read nothing
/// again** (a server slot re-pointed at a new client: the cache misses for a round trip). The
/// control and its focus stop stay, and focus on it does not jump to the leftmost library name
/// (`reconcile` seats a key nothing places on the row's first stop). The answer that follows
/// replaces it, 0 or a refusal included. Another library never inherits it
/// (`a_switch_never_draws_the_count_of_the_library_it_left_as_the_new_ones`).
#[test]
fn a_known_count_stays_on_the_row_while_the_same_library_reads_unknown() {
    let _guard = plx_base::testlock::serial();
    knows_collections(&[(1, Ok(4))]);
    let (fixture, mut page, mut engine) = grid_page(2);
    assert_eq!(page.collections, CollectionsControl::Count(4), "premise: the count is known");
    let key = page.key(COLLECTIONS);
    engine.set(OWNER, key, Some(LIBRARY_GROUP), By::Dir);
    let mut ms = 0;
    for _ in 0..10 { ms += 16; fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 })); }
    assert_eq!(at(&engine), COLLECTIONS, "premise: focus is on the control");
    // The cache misses: the same library reads nothing for a while.
    knows_collections(&[]);
    for _ in 0..30 {
        ms += 16;
        fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
        assert_eq!(page.collections, CollectionsControl::Count(4), "the known count left the row");
        assert!(page.toolbar_elems().contains(&COLLECTIONS), "its focus stop left the row");
        let seen = <LibraryScreen as Focusable<TestHost>>::reconcile(&page, page.key(COLLECTIONS), &fixture.cx(&engine));
        assert_eq!(seen.elem, COLLECTIONS, "focus on the control is seated elsewhere");
        let built = page.header.row().unwrap().page.spec().right.iter()
            .find(|item| item.label == plx_platform::i18n::msg::library_header_collections()).map(|item| item.value.clone());
        assert_eq!(built, Some(Some("4".to_string())), "the built row dropped the control");
    }
    // The answer that follows is the truth, whatever it says.
    land_collections(1, Ok(0));
    fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms: ms + 16, dt_us: 16_667 }));
    assert_eq!(page.collections, CollectionsControl::Hidden);
    assert!(!page.toolbar_elems().contains(&COLLECTIONS));
    knows_collections(&[]);
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

/// **A control a menu has lifted wears no pill on the page** (the opener lift draws it, lit); every
/// other focused control still does.
#[test]
fn a_control_its_menu_has_lifted_draws_no_focus_pill() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(2);
    let key = page.key(SORTFILTER);
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    // The pill is the row's motion: it fades in under the focused control over a few frames.
    for frame in 1..=60 { fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 })); }
    let pill = plx_ui::widgets::focus_pill(page.header.row().unwrap().plate(SORTFILTER).unwrap());
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

/// **Every header opener stays lit while its menu is open** (design review S12: one rule). Sort &
/// Filter and `+N` are both lifted out of the page's pass (no capsule, no label: the page draws
/// none of the control) and the opener lift (`Screen::redraw_focused`) paints each as the focused
/// control, the accent pill at the focused extent and every run of the face in the focused ink,
/// above the scrim. The `+N` list used to keep the page's dimmed capsule under the scrim.
#[test]
fn every_header_opener_is_lit_by_the_lift_while_its_menu_is_open() {
    use plx_ui::screen::Screen;
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(12);
    for elem in [SORTFILTER, MORE] {
        let key = page.key(elem);
        engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
        for frame in 1..=60 { fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 })); }
        let plate = page.header.row().unwrap().plate(elem).expect("the opener is a stop of the row");
        let pill = plx_ui::widgets::focus_pill(plate);
        let words = |rect: plx_ui::Rect| rect.cx() > plate.x && rect.cx() < plate.x + plate.w && rect.cy() > plate.y && rect.cy() < plate.y + plate.h;

        // the page's pass, with the opener lifted out of it, draws none of the control
        let cx = fixture.cx(&engine);
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        frame.lifted = Some(key);
        let left = plx_ui::draw_census::capture_runs(|| page.draw_header(&frame));
        assert!(!left.iter().any(|run| words(run.at)), "elem {elem}: the page left words on the opener: {left:?}");

        // the lift paints it: one accent pill, the face in the focused ink
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        let mut pills = Vec::new();
        let runs = plx_ui::draw_census::capture_runs(|| {
            pills = plx_ui::draw_census::capture(|| Screen::<TestHost>::redraw_focused(&page, &mut frame, Some(key)));
        });
        let rects: Vec<_> = pills.iter().filter(|(tag, _)| *tag == 2).map(|(_, r)| *r).collect();
        assert!(rects.len() == 1 && (rects[0].x - pill.x).abs() < 0.5 && (rects[0].w - pill.w).abs() < 0.5 && (rects[0].h - pill.h).abs() < 0.5,
            "elem {elem}: one accent pill at {pill:?}, got {rects:?}");
        assert!(!runs.is_empty() && runs.iter().all(|run| words(run.at)
            && run.ink.iter().zip(plx_ui::ACCENT_INK).all(|(a, b)| (a - b).abs() < 0.01)),
            "elem {elem}: the face is on the pill, in the focused ink: {runs:?}");
    }
}

/// The kinds one descriptive draw of `page`, scrolled one grid pitch (the chrome fully tucked),
/// declares: a capsule is a `glass` paint (the nav pill's live material).
fn tucked_draw_kinds(fixture: &Fixture, page: &mut LibraryScreen, engine: &FocusEngine<u32>) -> Vec<(String, [f32; 4])> {
    let pitch = page.layout.grid_pitch();
    draw_kinds_at(fixture, page, engine, pitch)
}

/// The kinds one descriptive draw of `page` declares with the page put at exactly `scroll` (a
/// jump: the scroll and its target are there, and the handoff lands on that state at once).
fn draw_kinds_at(fixture: &Fixture, page: &mut LibraryScreen, engine: &FocusEngine<u32>, scroll: f32) -> Vec<(String, [f32; 4])> {
    page.scroll.jump(scroll);
    page.scroll_target = scroll;
    let worn = page.capsule_wanted();
    page.handoff.step(0.0, scroll, scroll, page.layout.grid_pitch(), worn);
    draw_kinds_now(fixture, page, engine)
}

/// The kinds one descriptive draw of `page` declares, the page left as it is.
fn draw_kinds_now(fixture: &Fixture, page: &mut LibraryScreen, engine: &FocusEngine<u32>) -> Vec<(String, [f32; 4])> {
    use plx_ui::screen::Screen;
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
    assert!(kinds.iter().any(|(k, r)| k == "rect" && r[0] == 0.0 && r[1] == 0.0 && r[3] == plx_ui::scroll_capsule::FADE_STEP), "the top fade stays");
    assert!(kinds.iter().all(|(k, _)| k != "glass"), "one library: no names in the header, no capsule");
    assert_eq!(kinds.iter().filter(|(k, r)| k == "rect" && r[0] == 0.0 && r[3] == plx_ui::scroll_capsule::FADE_STEP && r[1] < 200.0)
        .map(|(_, r)| r[1] + r[3]).fold(0.0f32, f32::max), plx_ui::scroll_capsule::FADE_H, "and the fade stays the plain 72 deep");
    let (fixture, mut two, engine) = grid_page(2);
    let kinds = tucked_draw_kinds(&fixture, &mut two, &engine);
    assert!(kinds.iter().any(|(k, r)| k == "glass" && r[1] == plx_ui::scroll_capsule::Y), "two libraries: the capsule, as live glass");
    // the ground fade reaches through the capsule's band while the capsule shows, 12 px past the
    // capsule's own measured bottom, and only 72 deep on the one-library page that wears none
    let depth = |kinds: &[(String, [f32; 4])]| {
        kinds.iter().filter(|(k, r)| k == "rect" && r[0] == 0.0 && r[2] == SCR_W && r[3] <= plx_ui::scroll_capsule::FADE_STEP && r[1] < 200.0)
            .map(|(_, r)| r[1] + r[3]).fold(0.0f32, f32::max)
    };
    assert_eq!(depth(&kinds), plx_ui::scroll_capsule::Y + plx_ui::scroll_capsule::H + plx_ui::scroll_capsule::FADE_BELOW_CAPSULE);
    assert!(kinds.iter().all(|(k, _)| k != "glass_frozen"));
}

/// **A two-library page declares no live glass while its chrome is still on screen, either way.**
/// The chrome band (the nav pill's live glass) and the capsule are two live glasses whose blur
/// footprints meet; sharing a frame makes the band capture inline in the visible pass, +1.4M GPU
/// cycles per frame and a quarter of the ramp's frames late on the television. So the page's
/// handoff (`scroll_chrome::Handoff`, stepped by the page's own `Tick`) sequences them: going down
/// the chrome leaves and only then does the capsule arrive; coming up the capsule leaves and only
/// then does the chrome return. Driven through real ticks and real draws: on every frame the page
/// declares the capsule only when the chrome it tells the dispatcher to draw is gone, and a page
/// that has fully tucked declares exactly one live glass, at the capsule's place.
#[test]
fn a_two_library_page_declares_no_live_glass_while_its_chrome_is_still_on_screen() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(2);
    let pitch = page.layout.grid_pitch();
    let mut ms = 0;
    let mut run = |page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, target: f32| {
        page.scroll_target = target;
        let (mut chrome_frames, mut glass_frames) = (0, 0);
        for _ in 0..90 {
            ms += 16;
            fixture.step(page, engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
            let glass = draw_kinds_now(&fixture, page, engine).iter().filter(|(k, _)| k == "glass").count();
            let tuck = <LibraryScreen as plx_ui::screen::Screen<TestHost>>::chrome_tuck(page);
            if tuck < 1.0 {
                chrome_frames += 1;
                assert_eq!(glass, 0, "scroll {} -> {target}: the chrome is at tuck {tuck}, still on screen, so the capsule is not", page.scroll.pos);
            } else if glass > 0 {
                glass_frames += 1;
            }
        }
        (chrome_frames, glass_frames)
    };
    let (leaving, arriving) = run(&mut page, &mut engine, pitch);
    assert!(leaving >= 4 && arriving >= 4, "premise: Down has frames of each half ({leaving} with the chrome up, {arriving} with the capsule)");
    let kinds = draw_kinds_now(&fixture, &mut page, &engine);
    let capsules: Vec<_> = kinds.iter().filter(|(k, _)| k == "glass").collect();
    assert_eq!(capsules.len(), 1, "fully tucked: exactly one live glass");
    assert_eq!(capsules[0].1[1], plx_ui::scroll_capsule::Y, "and it is the capsule");
    let (returning, leaving) = run(&mut page, &mut engine, 0.0);
    assert!(leaving >= 4 && returning >= 4, "premise: Up has frames of each half ({leaving} with the capsule going, {returning} with the chrome coming back)");
    assert!(draw_kinds_now(&fixture, &mut page, &engine).iter().all(|(k, _)| k != "glass"), "back at the top: no capsule");
}

/// One frame of the page: its `Tick`, then the header drawn at that moment, as its rounded rects.
fn header_rrects_after_a_frame(fixture: &Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, ms: u32) -> Vec<plx_ui::Rect> {
    fixture.step(page, engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
    let cx = fixture.cx(engine);
    let frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
    plx_ui::draw_census::capture(|| page.draw_header(&frame)).into_iter().filter(|(tag, _)| *tag == 2).map(|(_, rect)| rect).collect()
}

/// The focus pill (the one rounded rect taller than the 56 px plates, since it stands out by
/// `PILL_FOCUS_OUTSET`) after one frame, as its centre x; `None` when no pill is drawn.
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
/// the selection plate travels to the pill (it is never cut onto it), and the pill neither restarts
/// from the left, nor fades, nor moves; the row is rebuilt for the new library and its motion state is not.
#[test]
fn a_library_switch_keeps_the_rows_pill_and_travels_the_plate_to_the_picked_name() {
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
    let plate_cx = |rects: &[plx_ui::Rect]| rects.iter().find(|r| (r.h - plx_ui::page_header::HEIGHT).abs() < 0.01).map(|r| r.cx());
    let before_rects = header_rrects_after_a_frame(&fixture, &mut page, &mut engine, ms + 16);
    assert!(!selected_under(&before_rects), "premise: the old library wears the plate");
    let old_cx = plate_cx(&before_rects).expect("premise: the old library's plate is drawn");

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
        // reads the old library again; while the switch is pending, the plate TRAVELS to the name
        // (design review S5: it is never cut onto it): on its way on the first frame, nearer
        // the name every frame, on it when it arrives.
        if frame <= 4 {
            let at = plate_cx(&rects).expect("the plate stays drawn through the switch");
            let left = (at - plate.cx()).abs();
            assert!(left < (old_cx - plate.cx()).abs() - 0.5, "frame {frame}: the plate has left the old library ({at} from {old_cx})");
            assert!(frame > 1 || left > 1.0, "the plate is on its way on the first frame, not cut onto the name ({at} vs {})", plate.cx());
        }
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
    let pill_w = page.header.row().unwrap().plate(SORTFILTER).unwrap().w + 2.0 * plx_ui::widgets::PILL_FOCUS_OUTSET;
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

/// What one frame of the header shows of the Collections control, and what the built row says of it.
struct Frame {
    /// `(x, alpha)` of every drawn "Collections" label.
    labels: Vec<(f32, f32)>,
    /// `(text, x, alpha)` of every drawn run that is one of the numbers being watched.
    numbers: Vec<(String, f32, f32)>,
    /// The Collections item of the BUILT row: `None` = the row has no such control, `Some(value)` = its value.
    built: Option<Option<String>>,
    /// Is the Collections control one of the controls the row's focus walks?
    focusable: bool,
    /// Where the "Recommended" label is drawn.
    recommended_x: f32,
    /// Its opacity: the page's own (a page that is arriving fades its whole header in).
    recommended_alpha: f32,
}

/// The resting opacity of a label (its ink's alpha).
fn rest_alpha() -> f32 { plx_ui::theme::CONTROL_TEXT_REST[3] }

fn watch_frame(fixture: &Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, ms: u32, numbers: &[&str]) -> Frame {
    use plx_platform::i18n::msg;
    let runs = header_runs_after_a_frame(fixture, page, engine, ms);
    let collections = msg::library_header_collections();
    Frame {
        labels: runs.iter().filter(|run| run.text == collections).map(|run| (run.at.x, run.ink[3])).collect(),
        numbers: runs.iter().filter(|run| numbers.contains(&run.text.as_str())).map(|run| (run.text.clone(), run.at.x, run.ink[3])).collect(),
        built: page.header.row().and_then(|row| row.page.spec().right.iter().find(|item| item.label == collections).map(|item| item.value.clone())),
        focusable: page.toolbar_elems().contains(&COLLECTIONS),
        recommended_x: runs.iter().find(|run| run.text == msg::library_header_recommended()).map_or(f32::NAN, |run| run.at.x),
        recommended_alpha: runs.iter().find(|run| run.text == msg::library_header_recommended()).map_or(f32::NAN, |run| run.ink[3]),
    }
}

/// Settle `page` for ten frames and read the control, then press the other library's name. The
/// store moves to it on frame 6; its probe is already known (`warm`) or lands on frame 12. Returns
/// the settled frame and the 60 after the press.
fn switch_and_watch(from: u32, to: Result<u32, crate::registry::Forbidden>, warm: bool, numbers: &[&str]) -> (Frame, Vec<Frame>) {
    knows_collections(&[(1, Ok(from))]);
    if warm { land_collections(2, to); }
    let (mut fixture, mut page, mut engine) = grid_page(2);
    let current = page.view_section(&fixture.cx(&engine));
    let other = page.libraries.iter().find(|(_, index)| Some(*index) != current).unwrap().0;
    let mut ms = 0;
    let mut rest = None;
    for _ in 0..10 { ms += 16; rest = Some(watch_frame(&fixture, &mut page, &mut engine, ms, numbers)); }
    fixture.step(&mut page, &mut engine, ScreenEvent::Activate(other));
    let mut frames = Vec::new();
    for frame in 1..=60 {
        ms += 16;
        if frame == 6 { fixture.publish(1, 36); }
        if frame == 12 && !warm { land_collections(2, to); }
        frames.push(watch_frame(&fixture, &mut page, &mut engine, ms, numbers));
    }
    knows_collections(&[]);
    (rest.unwrap(), frames)
}

/// **A library switch never draws the count of the library it left as the new one's** (the
/// control is on the row only with a count KNOWN for the library on view). The probe answers for
/// the new library a few frames after the press, and the control used to stay on the row as it
/// was for that whole window: the previous library's "Collections · 4", at full strength, over the
/// grid dissolving to a library that has another number (or none). Now the row follows the press
/// at once: the built row never carries the old count again, NOTHING of the old control is drawn
/// from the press on (it is the left library's, not this one's: not even fading out), and the new
/// one fades in when its count lands. (Replaces
/// `a_library_switch_keeps_the_collections_label_still_and_cross_fades_its_count`, whose premise
/// was "the old count stays while the new one is unknown".)
#[test]
fn a_switch_never_draws_the_count_of_the_library_it_left_as_the_new_ones() {
    let _guard = plx_base::testlock::serial();
    let (rest, frames) = switch_and_watch(4, Ok(2), false, &["4", "2"]);
    assert_eq!(rest.built, Some(Some("4".to_string())), "premise: the first library's control is on the row");
    for (n, frame) in frames.iter().enumerate() {
        assert_ne!(frame.built, Some(Some("4".to_string())), "frame {}: the built row carries the library left behind", n + 1);
        assert!(frame.numbers.iter().all(|(text, ..)| text != "4"), "frame {}: the old count is drawn: {:?}", n + 1, frame.numbers);
        if n + 1 < 12 {
            assert_eq!(frame.built, None, "frame {}: an unanswered library has no control on the row", n + 1);
            assert!(frame.labels.is_empty() && frame.numbers.is_empty(), "frame {}: Collections is drawn for a library whose count is not known: {:?}", n + 1, frame.labels);
        }
        assert!(!(frame.focusable && frame.built.is_none()), "frame {}: focus would walk a control that is not there", n + 1);
    }
    let new: Vec<f32> = frames.iter().skip(11).filter_map(|frame| frame.numbers.iter().find(|(text, ..)| text == "2").map(|(_, _, alpha)| *alpha)).collect();
    assert!(new.iter().any(|alpha| *alpha > 0.0 && *alpha < 1.0), "the new count fades in, it does not blink on: {new:?}");
    assert!(new.windows(2).all(|pair| pair[1] >= pair[0]), "and only ever rises: {new:?}");
    let last = frames.last().unwrap();
    assert_eq!(last.numbers, [("2".to_string(), last.numbers[0].1, 1.0)], "settled on the new count alone");
    assert_eq!(last.built, Some(Some("2".to_string())));
}

/// **A switch to a library whose count is already known cross-fades the number in place** (the
/// probe is warmed for every library when the directory lands, so this is the usual switch): the
/// built row has the new count from the press on, the label stays where it is when the count keeps
/// its digit count, and the two numbers share frames with opacities that sum to one.
#[test]
fn a_switch_to_a_known_count_cross_fades_the_number_in_place() {
    let _guard = plx_base::testlock::serial();
    let (rest, frames) = switch_and_watch(4, Ok(2), true, &["4", "2"]);
    let label_x = rest.labels[0].0;
    let mut shared = 0;
    for (n, frame) in frames.iter().enumerate() {
        assert_eq!(frame.built, Some(Some("2".to_string())), "frame {}: the new library's count from the press on", n + 1);
        assert_eq!(frame.labels.len(), 1, "frame {}: one label", n + 1);
        assert!((frame.labels[0].0 - label_x).abs() < 0.01, "frame {}: the label moved ({label_x} -> {})", n + 1, frame.labels[0].0);
        let visible: f32 = frame.numbers.iter().map(|(_, _, alpha)| *alpha).sum();
        assert!(visible >= 0.99, "frame {}: the count dipped to {visible}: {:?}", n + 1, frame.numbers);
        shared += usize::from(frame.numbers.len() == 2);
    }
    assert!(shared >= 4, "the two numbers share frames: that is a cross-fade, not a cut ({shared})");
    assert_eq!(frames.last().unwrap().numbers.iter().map(|(text, ..)| text.as_str()).collect::<Vec<_>>(), ["2"]);
}

/// **A count of another width moves its own label and nothing else** (the accepted consequence of
/// placing the control by what it draws): the old face leaves where it stood and the new one
/// arrives where it now stands, on the swap clock, "Recommended" and "Sort & Filter" not moving a
/// pixel. A fourth digit is the same: it moves the label, once, and holds. (Replaces
/// `a_count_of_another_width_lands_without_moving_the_label` and
/// `a_four_digit_count_widens_its_slot_once_and_holds`, which pinned the label in place.)
#[test]
fn a_count_of_another_width_dissolves_at_each_x_and_moves_nothing_else() {
    let _guard = plx_base::testlock::serial();
    for to in [123, 1_234] {
        let text = plx_platform::i18n::current().number(i64::from(to));
        let (rest, frames) = switch_and_watch(4, Ok(to), true, &["4", text.as_str()]);
        let (from_x, to_x) = (rest.labels[0].0, frames.last().unwrap().labels[0].0);
        assert!(to_x < from_x - 1.0, "{to}: premise: the wider count moves the label left ({from_x} -> {to_x})");
        let mut two = 0;
        for (n, frame) in frames.iter().enumerate() {
            assert_eq!(frame.recommended_x, rest.recommended_x, "{to} frame {}: Recommended moved", n + 1);
            assert!(frame.labels.len() <= 2, "{to} frame {}: more than the two faces: {:?}", n + 1, frame.labels);
            if let [(x0, a0), (x1, a1)] = frame.labels[..] {
                assert!([x0, x1].iter().all(|x| (x - from_x).abs() < 0.01 || (x - to_x).abs() < 0.01), "{to} frame {}: a face between the two x", n + 1);
                assert!((a0 + a1 - rest_alpha()).abs() < 1e-3, "{to} frame {}: opacities {a0} + {a1} are not the resting one", n + 1);
                two += 1;
            }
        }
        assert!(two >= 4, "{to}: the faces share frames: that is a dissolve, not a cut ({two})");
        let last = frames.last().unwrap();
        assert_eq!((last.labels.len(), last.numbers.len()), (1, 1), "{to}: settled on one face");
    }
}

/// **A library with no collections draws no Collections control, ever, from the press on**
/// (owner, from the video: "when entering the library without collections, UI briefly shows
/// collections button"), whether its answer was known at the press or lands after. The control the
/// library left is the left library's: the row for another library is the new page's own and does
/// not carry it, not even fading out ([`HeaderSwap::begin_switch`]), and never "· 0". 0 frames of
/// it, label or number. (Replaces `a_library_without_collections_fades_the_control_out_in_place`,
/// which asked for the old control to dissolve over the new library's page: the same label, still
/// drawn for several frames.)
#[test]
fn a_library_without_collections_draws_no_control_from_the_press_on() {
    let _guard = plx_base::testlock::serial();
    for warm in [true, false] {
        let (rest, frames) = switch_and_watch(4, Ok(0), warm, &["4", "0"]);
        assert_eq!(rest.labels.len(), 1, "warm {warm}: premise: the first library draws its control");
        for (n, frame) in frames.iter().enumerate() {
            assert!(frame.labels.is_empty() && frame.numbers.is_empty(), "warm {warm} frame {}: Collections is drawn: {:?} {:?}", n + 1, frame.labels, frame.numbers);
            assert_eq!(frame.built, None, "warm {warm} frame {}: the new library has no control to build", n + 1);
            assert!(!frame.focusable, "warm {warm} frame {}: focus would walk to it", n + 1);
        }
    }
}

/// **The first count fades the WHOLE control in**: a page that has not heard from the probe draws
/// no Collections control (not a bare label that a number then joins), and the count that lands
/// brings label and number up together on the swap clock, the label at the x it will keep and no
/// frame with the label and no number.
#[test]
fn the_first_count_fades_the_whole_control_in() {
    let _guard = plx_base::testlock::serial();
    knows_collections(&[]);
    let (fixture, mut page, mut engine) = grid_page(2);
    let mut ms = 0;
    for _ in 0..10 {
        ms += 16;
        let frame = watch_frame(&fixture, &mut page, &mut engine, ms, &["7"]);
        assert!(frame.labels.is_empty() && frame.built.is_none(), "premise: no control until the probe answers");
        assert!(!frame.focusable);
    }
    land_collections(1, Ok(7));
    let mut seen = Vec::new();
    let mut x = None;
    for n in 1..=30 {
        ms += 16;
        let frame = watch_frame(&fixture, &mut page, &mut engine, ms, &["7"]);
        assert_eq!(frame.labels.is_empty(), frame.numbers.is_empty(), "frame {n}: the label and its number come together: {:?} {:?}", frame.labels, frame.numbers);
        if let Some((label_x, alpha)) = frame.labels.first() {
            assert!((label_x - *x.get_or_insert(*label_x)).abs() < 0.01, "frame {n}: the label moved while it arrived");
            seen.push(*alpha);
        }
    }
    assert!(seen.first().is_some_and(|alpha| *alpha < rest_alpha() - 0.05), "the first frame is not fully there: {seen:?}");
    assert!(seen.windows(2).all(|pair| pair[1] >= pair[0]), "rising: {seen:?}");
    assert!((seen.last().unwrap() - rest_alpha()).abs() < 1e-3, "to fully there: {seen:?}");
    knows_collections(&[]);
}

/// A page of two libraries of different kinds, Movies (key 1) and TV Shows (key 2), settled on
/// section `current`, as the top navigation finds it.
fn two_kind_page(current: usize) -> (Fixture, LibraryScreen, FocusEngine<u32>) {
    let mut fixture = Fixture::new(2, 36, 0);
    fixture.sections[1].kind = SecKind::Show;
    fixture.publish(current, 36);
    let mut page = LibraryScreen::new(ENTRY, InstanceId(19), fixture.sections[current].kind);
    let mut engine = FocusEngine::new();
    page.sync(&fixture.cx(&engine));
    page.initial = false;
    page.sync(&fixture.cx(&engine));
    page.relayout(None);
    let first = page.key(page.pair.detail.elem_at(0).unwrap());
    engine.set(OWNER, first, Some(page.pair.groups_config().detail), By::Restore);
    (fixture, page, engine)
}

/// Enter the other kind of library the way the top navigation does (`LibraryCmd::Enter`), then
/// watch 40 frames: the store moves to it on frame 6, its probe lands `answer` on frame 12 (never,
/// for `None`). Returns the frame before the press and the 40 after.
fn enter_and_watch(from: usize, answer: Option<Result<u32, crate::registry::Forbidden>>) -> (Frame, Vec<Frame>) {
    let (mut fixture, mut page, mut engine) = two_kind_page(from);
    let to = 1 - from;
    let mut ms = 0;
    let mut rest = None;
    for _ in 0..10 { ms += 16; rest = Some(watch_frame(&fixture, &mut page, &mut engine, ms, &["6"])); }
    let mut output = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.command(LibraryCmd::Enter(fixture.sections[to].kind), &fixture.cx(&engine),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present));
    let mut frames = Vec::new();
    for frame in 1..=40 {
        ms += 16;
        if frame == 6 { fixture.publish(to, 36); }
        if frame == 12 { if let Some(answer) = answer { land_collections(to as u32 + 1, answer); } }
        frames.push(watch_frame(&fixture, &mut page, &mut engine, ms, &["6"]));
    }
    (rest.unwrap(), frames)
}

/// **Entering a library without collections never draws the Collections control** (owner, from
/// the video: "when entering the library without collections, UI briefly shows collections
/// button", Movies to TV Shows from the top navigation). The probe answers for TV Shows only
/// after the press; until then the control read as a bare "Collections", which the answer ("none")
/// then took off the row again. No built row of the new library carries the control, before the
/// answer or after it, and focus never walks to it.
#[test]
fn entering_a_library_without_collections_never_builds_the_collections_control() {
    let _guard = plx_base::testlock::serial();
    knows_collections(&[(1, Ok(6))]);
    let (rest, frames) = enter_and_watch(0, Some(Ok(0)));
    assert_eq!(rest.built, Some(Some("6".to_string())), "premise: Movies draws its count");
    assert!(rest.focusable);
    for (n, frame) in frames.iter().enumerate() {
        assert_eq!(frame.built, None, "frame {}: a built row carries the control", n + 1);
        assert!(!frame.focusable, "frame {}: toolbar_elems lists COLLECTIONS", n + 1);
        assert!(frame.labels.is_empty() && frame.numbers.is_empty(),
            "frame {}: Movies' Collections is drawn on the library without: {:?} {:?}", n + 1, frame.labels, frame.numbers);
    }
    knows_collections(&[]);
}

/// **A library whose count is already known has its control from the first frame of its row** (the
/// probe is warmed for every library when the directory lands, so this is the usual entry): label
/// and number together at the page's own opacity from the press on (the page dissolves in as a
/// whole), no frame without it and no swap fading it separately, because the new library's page
/// arrives with the controls it has.
#[test]
fn entering_a_library_whose_count_is_known_has_the_control_from_its_first_frame() {
    let _guard = plx_base::testlock::serial();
    knows_collections(&[(1, Ok(6)), (2, Ok(0))]);
    let (rest, frames) = enter_and_watch(1, None);
    assert_eq!(rest.built, None, "premise: TV Shows has no control");
    for (n, frame) in frames.iter().enumerate() {
        assert_eq!(frame.built, Some(Some("6".to_string())), "frame {}: the built row has the control with its count", n + 1);
        assert_eq!(frame.labels.len(), 1, "frame {}: one label: {:?}", n + 1, frame.labels);
        // The page itself dissolves in on an entry, so "at rest" is the opacity of its other labels.
        assert!((frame.labels[0].1 - frame.recommended_alpha).abs() < 1e-3,
            "frame {}: the label is not at the page's opacity ({}): {:?}", n + 1, frame.recommended_alpha, frame.labels);
        assert_eq!(frame.numbers.len(), 1, "frame {}: the number with it: {:?}", n + 1, frame.numbers);
        assert!((frame.numbers[0].2 * rest_alpha() - frame.labels[0].1).abs() < 1e-3,
            "frame {}: the number is not at the label's opacity ratio: {:?} {:?}", n + 1, frame.numbers, frame.labels);
        assert!(frame.focusable, "frame {}: the control is a stop from the first frame", n + 1);
    }
    knows_collections(&[]);
}

/// **Entering a library with collections draws the control once, with its count, fading in** (the
/// reverse, TV Shows to Movies): the label appeared first and "· 6" dissolved in beside it for a
/// few frames. The built row never has the control without its number, and when the count lands
/// the swap is on from its first frame with the face below full strength.
#[test]
fn entering_a_library_with_collections_fades_the_whole_control_in_when_its_count_lands() {
    let _guard = plx_base::testlock::serial();
    knows_collections(&[(2, Ok(0))]);
    let (rest, frames) = enter_and_watch(1, None);
    assert_eq!(rest.built, None, "premise: TV Shows has no control");
    for (n, frame) in frames.iter().enumerate() {
        assert_ne!(frame.built, Some(None), "frame {}: the control is built without its count", n + 1);
        assert!(frame.labels.is_empty(), "frame {}: a bare label is drawn: {:?}", n + 1, frame.labels);
    }
    // Now the same entry with the count landing on frame 12.
    knows_collections(&[(2, Ok(0))]);
    let (fixture, mut page, mut engine) = two_kind_page(1);
    let mut ms = 0;
    for _ in 0..10 { ms += 16; watch_frame(&fixture, &mut page, &mut engine, ms, &["6"]); }
    let mut fixture = fixture;
    let mut output = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.command(LibraryCmd::Enter(SecKind::Movie), &fixture.cx(&engine),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present));
    for frame in 1..=40 {
        ms += 16;
        if frame == 6 { fixture.publish(0, 36); }
        if frame == 12 { land_collections(1, Ok(6)); }
        let seen = watch_frame(&fixture, &mut page, &mut engine, ms, &["6"]);
        if frame == 12 {
            assert!(page.header_swap.is_active(), "the swap is on from the frame the count lands");
            assert_eq!(seen.labels.len(), 1, "the control arrives");
            assert!(seen.labels[0].1 < rest_alpha() - 0.05, "and its label is not at full strength on its first frame: {:?}", seen.labels);
            assert_eq!(seen.labels.is_empty(), seen.numbers.is_empty(), "with its number: {:?}", seen.numbers);
        }
    }
    knows_collections(&[]);
}
