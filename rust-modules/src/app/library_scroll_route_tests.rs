//! **R12, scrolling**: how the Library, its Collections page, the Recommended page and the
//! Watchlist tuck the top chrome as a long grid scrolls under the keys, where a focused row rests,
//! and what Up reaches first while the chrome is tucked.
//!
//! Key-driven on a real `Dispatcher<AppHost>` (`route_test_support::Rig`): `chrome_tuck` is read from
//! the screen on top, the way the dispatcher reads it, so a screen that stopped tucking, or tucked
//! at the wrong scroll, fails here and not on the television.

use super::*;
use super::route_test_support::{Guard, Rig};
use super::test_support::tick;
use plx_screens::library::RecommendedScreen;

/// Where focus is, coarsely: enough to state what an Up reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum At { Grid(usize), Header, Nav }

fn at(r: &Rig) -> Option<At> {
    if r.on_nav() { Some(At::Nav) }
    else if r.on_header() { Some(At::Header) }
    else { r.grid_cell().map(|(row, _)| At::Grid(row)) }
}

/// The scroll target of the Recommended page on top.
fn recommended_target(r: &Rig) -> f32 {
    r.d.top_screen().and_then(|screen| screen.as_any()).and_then(|any| any.downcast_ref::<RecommendedScreen>())
        .expect("the Recommended page is on top").probe_scroll_target()
}

fn near(a: f32, b: f32) -> bool { (a - b).abs() < 0.5 }

/// Down from the header into the grid, then `rows` more: settled on row `rows`.
fn down_to_row(r: &mut Rig, rows: usize) {
    r.press(Key::Down);
    assert_eq!(at(r), Some(At::Grid(0)), "premise: Down from the header enters the grid's first row");
    for row in 1..=rows {
        r.press(Key::Down);
        assert_eq!(at(r), Some(At::Grid(row)), "premise: Down #{row} moved one row");
    }
}

#[test]
fn down_tucks_the_chrome_and_each_row_rests_on_the_first_rows_line() {
    let _guard = Guard::new("route-scroll-down");
    let mut r = Rig::movies_library(true);
    assert!(near(r.tuck(), 0.0), "the chrome is shown on arrival");
    r.press(Key::Down);
    assert_eq!(at(&r), Some(At::Grid(0)));
    let (y, first, _) = r.library().probe_rest_line(r.d.focus()).expect("a card is focused");
    assert!(near(y, first) && near(r.tuck(), 0.0), "the first row rests where it is drawn at scroll 0 ({y} vs {first}) with the chrome shown");
    for row in 1..=6 {
        r.press(Key::Down);
        r.frames(60);
        assert_eq!(at(&r), Some(At::Grid(row)), "Down moves one row");
        let (y, first, unsettled) = r.library().probe_rest_line(r.d.focus()).expect("a card is focused");
        assert!(unsettled.abs() < 0.5, "row {row}: the scroll spring has arrived ({unsettled} short)");
        assert!(near(y, first), "row {row} rests on the first row's line: poster top {y}, first row's {first}");
        assert!(y >= 120.0, "row {row} rests at {y}, under the 120 settle line");
        assert!(near(r.tuck(), 1.0), "row {row}: the chrome is tucked once the first row has gone (tuck {})", r.tuck());
    }
}

#[test]
fn up_from_a_tucked_row_walks_the_rows_first_then_the_header_then_the_nav() {
    let _guard = Guard::new("route-scroll-up");
    let mut r = Rig::movies_library(true);
    down_to_row(&mut r, 5);
    assert!(near(r.tuck(), 1.0), "premise: tucked on row 5");
    // What scrolls first: the rows. Each Up moves one row and the page follows it, the chrome
    // staying tucked until row 0, where the page is back at scroll 0.
    let mut seen = Vec::new();
    for _ in 0..8 {
        r.press(Key::Up);
        seen.push((at(&r), r.tuck() > 0.99));
        if at(&r) == Some(At::Nav) { break; }
    }
    assert_eq!(seen, vec![
        (Some(At::Grid(4)), true), (Some(At::Grid(3)), true), (Some(At::Grid(2)), true), (Some(At::Grid(1)), true),
        (Some(At::Grid(0)), false), (Some(At::Header), false), (Some(At::Nav), false),
    ], "Up walks the rows, then reaches the header, then the nav; the chrome is shown again by row 0");
    assert!(near(r.library().probe_scroll_target(), 0.0));
}

#[test]
fn the_capsule_is_wanted_only_where_the_header_names_two_libraries() {
    let _guard = Guard::new("route-scroll-capsule");
    let two = Rig::movies_library(true);
    assert!(two.library().probe_capsule_wanted(), "two libraries on the header: the capsule restates the one on view");
    let one = Rig::movies_library(false);
    assert!(!one.library().probe_capsule_wanted(), "one library: nothing to restate");
}

#[test]
fn the_collections_page_tucks_but_wears_no_capsule() {
    let _guard = Guard::new("route-scroll-collections");
    let mut r = Rig::collections(true);
    assert!(!r.library().probe_capsule_wanted(), "the Collections header is a crumb: it names no libraries");
    r.press(Key::Down);
    for _ in 0..3 { r.press(Key::Down); }
    assert!(r.grid_cell().is_some_and(|(row, _)| row >= 2), "premise: three Downs moved down the grid, {:?}", r.grid_cell());
    assert!(near(r.tuck(), 1.0), "the Collections page tucks the chrome like the grid (it is the same screen), tuck {}", r.tuck());
}

#[test]
fn the_recommended_page_scrolls_but_never_tucks_the_chrome() {
    let _guard = Guard::new("route-scroll-recommended");
    let mut r = Rig::recommended();
    for _ in 0..8 { r.press(Key::Down); }
    assert!(recommended_target(&r) > 0.5, "premise: eight Downs scrolled the shelves");
    assert!(near(r.tuck(), 0.0), "the Recommended page does not override `chrome_tuck`: the bar stays, tuck {}", r.tuck());
}

#[test]
fn the_watchlist_tucks_and_draws_no_capsule() {
    let _guard = Guard::new("route-scroll-watchlist");
    let mut r = Rig::watchlist();
    for _ in 0..8 { r.press(Key::Down); }
    assert!(near(r.tuck(), 1.0), "the Watchlist tucks the chrome once its first row has gone, tuck {}", r.tuck());
}

/// The frames a route to the account menu is watched on: the chrome is tucked away while the menu
/// is up, and the strip holds focus with the page not resting at its top.
#[derive(Default)]
struct Watched { menu_frames: u32, menu_at_tuck: Vec<String>, strip_off_top: Vec<String> }

fn watch(r: &Rig, seen: &mut Watched) {
    if super::account_menu_up(&r.d) {
        seen.menu_frames += 1;
        if r.tuck() > 0.99 { seen.menu_at_tuck.push(format!("frame {}: the account menu is up and the chrome fully tucked", r.frame)); }
    }
    // The structural reason: a stop of the strip is only reached from the header row, which rests
    // the page at its top, so the tuck is already on its way back to 0.
    if r.on_nav() && r.scroll_target() > 0.5 {
        seen.strip_off_top.push(format!("frame {}: focus on the strip with the page travelling to {}", r.frame, r.scroll_target()));
    }
}

/// `keys`, one every `gap + 1` frames, watched on every frame.
fn drive(r: &mut Rig, keys: &[Key], gap: u32, seen: &mut Watched) {
    for key in keys {
        r.step(script_key(*key, tick(r.frame + 1)));
        watch(r, seen);
        for _ in 0..gap { r.step(vec![]); watch(r, seen); }
    }
}

/// **No key route opens the account menu over a fully tucked page.** From a deep row the remote can
/// reach the strip while the scroll spring is still carrying the page home (BACK three times, or
/// Up ten, a frame or two apart: focus is on the strip for a handful of frames with the chrome
/// still tucked), and the chip is the strip's first member. But the strip is only reached from the
/// header row, which rests the page at the top, so the tuck is already returning, and the menu's
/// press commits only after it is back. That matters because the dispatcher does not draw the bar
/// at tuck 1 (`draw_chrome` is skipped), so nothing republishes the bar's glass face that frame and
/// the menu's chip lift (`scrim_lift_read`) would read the previous one.
///
/// What this does NOT cover: the pointer. A click does not need a key route to the strip; it resolves
/// against the hit map, and `a_click_on_the_chip_lands_only_where_the_bar_is_drawn` below is the test
/// of that (the bar's stops are registered where the bar is drawn, and none once it is not).
fn the_account_menu_never_opens_over_a_tucked_page(name: &str, mount: fn() -> Rig, deep: fn(&mut Rig), leaves: &[&[Key]], gaps: &[u32]) {
    for &gap in gaps {
        for leave in leaves {
            let mut r = mount();
            deep(&mut r);
            assert!(r.tuck() > 0.99, "{name}: premise: tucked before the route starts");
            let mut seen = Watched::default();
            drive(&mut r, leave, gap, &mut seen);
            drive(&mut r, &[Key::Left; 5], gap, &mut seen);
            drive(&mut r, &[Key::Ok], gap, &mut seen);
            for _ in 0..40 { r.step(vec![]); watch(&r, &mut seen); }
            assert!(seen.menu_frames > 0, "{name} (gap {gap}): premise: the route opened the account menu from the chip");
            assert!(seen.menu_at_tuck.is_empty(), "{name} (gap {gap}): {:?}", seen.menu_at_tuck);
            assert!(seen.strip_off_top.is_empty(), "{name} (gap {gap}): {:?}", seen.strip_off_top);
        }
    }
}

#[test]
fn the_account_menu_never_opens_over_a_tucked_library() {
    let _guard = Guard::new("route-chip-library");
    the_account_menu_never_opens_over_a_tucked_page("library", || Rig::movies_library(true), |r| down_to_row(r, 8),
        &[&[Key::Back; 3], &[Key::Up; 10]], &[0, 1, 2]);
}

#[test]
fn the_account_menu_never_opens_over_a_tucked_collections_page() {
    let _guard = Guard::new("route-chip-collections");
    // BACK on the Collections page leaves it (its first press returns the scroll to the top, its
    // second pops the page), so only Up walks to the strip here.
    the_account_menu_never_opens_over_a_tucked_page("collections", || Rig::collections(true), |r| down_to_row(r, 8), &[&[Key::Up; 10]], &[0, 1, 2]);
}

#[test]
fn the_account_menu_never_opens_over_a_tucked_watchlist() {
    let _guard = Guard::new("route-chip-watchlist");
    the_account_menu_never_opens_over_a_tucked_page("watchlist", Rig::watchlist, |r| for _ in 0..8 { r.press(Key::Down); }, &[&[Key::Up; 16]], &[1, 2, 3]);
}

/// The chip's stop on the last presented hit map: its rect and clip, or `None` when the bar registered none.
fn chip_stop(r: &Rig) -> Option<(plx_ui::Rect, plx_ui::Rect)> {
    r.d.last_stops().iter().find(|s| s.key.elem == plx_ui::widgets::TAB_KEY_CHIP).map(|s| (s.rect, s.clip))
}

/// Press Down until the chrome is part way up (`lo < tuck < hi`), checking every frame of the scroll.
fn until_tuck_between(r: &mut Rig, lo: f32, hi: f32) {
    for _ in 0..8 {
        r.step(script_key(Key::Down, tick(r.frame + 1)));
        for _ in 0..40 {
            let tuck = r.tuck();
            if tuck > lo && tuck < hi { return; }
            if tuck >= hi { break; }
            r.step(vec![]);
        }
    }
    panic!("the chrome was never between {lo} and {hi} tucked");
}

/// **A pointer click lands on the chip where the bar is DRAWN, and nowhere when it is not.** The chip and
/// the pills are stops of the shared hit map, registered by the dispatcher at the bar's drawn place: lifted
/// by `chrome_lift` while the page tucks it, and not at all once it is gone. A click at the chip's RESTING
/// rect used to open the account menu over a bar that was lifted away (the stops stayed at rest), so the
/// click is graded in all three states of the same page, on a real tree:
///
/// * untucked: the chip is at its frame and a click on it opens the menu (the premise);
/// * part way up: the chip's stop is the lifted rect, a click there opens the menu, and a click at the
///   part of the resting rect the bar has left does not;
/// * fully tucked: the bar registers no stop and a click at the chip's resting place opens nothing.
fn a_click_on_the_chip_lands_only_where_the_bar_is_drawn(name: &str, mount: fn() -> Rig, deep: fn(&mut Rig)) {
    use plx_ui::widgets::CHIP_FRAME;
    let menu_after = |r: &mut Rig, x: f32, y: f32| { r.click(x, y); super::account_menu_up(&r.d) };
    let rest = CHIP_FRAME;

    let mut r = mount();
    r.present_bar();
    let (rect, _) = chip_stop(&r).unwrap_or_else(|| panic!("{name}: an untucked bar registers the chip"));
    assert_eq!((rect.x, rect.y, rect.w, rect.h), (rest.x, rest.y, rest.w, rest.h), "{name}: untucked, the stop is the frame");
    assert!(menu_after(&mut r, rest.cx(), rest.cy()), "{name}: premise: a click on the untucked chip opens the account menu");

    // Part way up: the click at the bottom edge of the RESTING rect is below the lifted chip.
    let mut r = mount();
    until_tuck_between(&mut r, 0.15, 0.6);
    let lift = plx_ui::screen::chrome_lift(r.tuck());
    r.present_bar();
    let below = (rest.cx(), rest.y + rest.h - 1.0);
    assert!(below.1 > rest.y - lift + rest.h, "{name}: premise: the resting rect's bottom edge is below the lifted chip");
    let (rect, clip) = chip_stop(&r).unwrap_or_else(|| panic!("{name}: a half-tucked bar still registers the chip"));
    assert!(!menu_after(&mut r, below.0, below.1), "{name}: a click where the chip RESTS, with the bar lifted off it, opens no menu");
    assert_eq!((rect.x, rect.y, rect.w, rect.h), (rest.x, rest.y - lift, rest.w, rest.h), "{name}: the stop is where the lifted chip is drawn");
    let seen = rect.intersect(clip);
    assert!(seen.w > 0.0 && seen.h > 0.0, "{name}: premise: part of the lifted chip is on canvas");

    // …and the click where the lifted chip IS drawn opens it.
    let mut r = mount();
    until_tuck_between(&mut r, 0.15, 0.6);
    r.present_bar();
    let (rect, clip) = chip_stop(&r).expect("a half-tucked bar still registers the chip");
    let seen = rect.intersect(clip);
    assert!(menu_after(&mut r, seen.cx(), seen.cy()), "{name}: a click on the lifted chip, where it is drawn, opens the menu");

    // Fully tucked: no bar, no stop, and a click at the chip's resting place reaches no chip.
    let mut r = mount();
    deep(&mut r);
    assert!(r.tuck() > 0.99, "{name}: premise: tucked");
    r.present_bar();
    let registered = r.d.last_stops().iter().filter(|s| s.key.elem >= plx_ui::dispatch::STRIP_BASE).count();
    assert!(!menu_after(&mut r, rest.cx(), rest.cy()), "{name}: a click at the chip's resting place, with no bar drawn, opens no menu");
    assert_eq!(registered, 0, "{name}: a fully tucked bar registers no stop for its chip or its pills");
}

#[test]
fn a_click_on_the_chip_lands_only_where_the_library_bar_is_drawn() {
    let _guard = Guard::new("route-chip-click-library");
    a_click_on_the_chip_lands_only_where_the_bar_is_drawn("library", || Rig::movies_library(true), |r| down_to_row(r, 8));
}

#[test]
fn a_click_on_the_chip_lands_only_where_the_collections_bar_is_drawn() {
    let _guard = Guard::new("route-chip-click-collections");
    a_click_on_the_chip_lands_only_where_the_bar_is_drawn("collections", || Rig::collections(true), |r| down_to_row(r, 8));
}

#[test]
fn a_click_on_the_chip_lands_only_where_the_watchlist_bar_is_drawn() {
    let _guard = Guard::new("route-chip-click-watchlist");
    a_click_on_the_chip_lands_only_where_the_bar_is_drawn("watchlist", Rig::watchlist, |r| for _ in 0..8 { r.press(Key::Down); });
}
