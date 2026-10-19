//! **R12, scrolling**: how the Library, its Collections page, the Recommended page and the
//! Watchlist tuck the top chrome as a long grid scrolls under the keys, where a focused row rests,
//! and what Up reaches first while the chrome is tucked.
//!
//! Key-driven on a real `Dispatcher<AppHost>` (`bridge_test_support::World`): `chrome_tuck` is read from
//! the screen on top, the way the dispatcher reads it, so a screen that stopped tucking, or tucked
//! at the wrong scroll, fails here and not on the television.

use super::*;
use super::test_support::World;
use super::test_support::tick;
use plx_screens::library::RecommendedScreen;

/// Where focus is, coarsely: enough to state what an Up reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum At { Grid(usize), Header, Nav }

fn at(r: &World) -> Option<At> {
    if r.on_nav() { Some(At::Nav) }
    else if r.on_header() { Some(At::Header) }
    else { r.grid_cell().map(|(row, _)| At::Grid(row)) }
}

/// The scroll target of the Recommended page on top.
fn recommended_target(r: &World) -> f32 {
    r.d.top_screen().and_then(|screen| screen.as_any()).and_then(|any| any.downcast_ref::<RecommendedScreen>())
        .expect("the Recommended page is on top").probe_scroll_target()
}

fn near(a: f32, b: f32) -> bool { (a - b).abs() < 0.5 }

/// Down from the header into the grid, then `rows` more: settled on row `rows`.
fn down_to_row(r: &mut World, rows: usize) {
    r.press(Key::Down);
    assert_eq!(at(r), Some(At::Grid(0)), "premise: Down from the header enters the grid's first row");
    for row in 1..=rows {
        r.press(Key::Down);
        assert_eq!(at(r), Some(At::Grid(row)), "premise: Down #{row} moved one row");
    }
}

#[test]
fn down_tucks_the_chrome_and_each_row_rests_on_the_first_rows_line() {
    let mut r = World::movies_library("route-scroll-down", true);
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
    let mut r = World::movies_library("route-scroll-up", true);
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
    // One world at a time (it holds the serial lock): each is built, asked and dropped in turn.
    let wanted = |two_libraries| World::movies_library("route-scroll-capsule", two_libraries).library().probe_capsule_wanted();
    assert!(wanted(true), "two libraries on the header: the capsule restates the one on view");
    assert!(!wanted(false), "one library: nothing to restate");
}

#[test]
fn the_collections_page_tucks_but_wears_no_capsule() {
    let mut r = World::collections("route-scroll-collections", true);
    assert!(!r.library().probe_capsule_wanted(), "the Collections header is a crumb: it names no libraries");
    r.press(Key::Down);
    for _ in 0..3 { r.press(Key::Down); }
    assert!(r.grid_cell().is_some_and(|(row, _)| row >= 2), "premise: three Downs moved down the grid, {:?}", r.grid_cell());
    assert!(near(r.tuck(), 1.0), "the Collections page tucks the chrome like the grid (it is the same screen), tuck {}", r.tuck());
}

#[test]
fn the_recommended_page_scrolls_but_never_tucks_the_chrome() {
    let mut r = World::recommended("route-scroll-recommended");
    for _ in 0..8 { r.press(Key::Down); }
    assert!(recommended_target(&r) > 0.5, "premise: eight Downs scrolled the shelves");
    assert!(near(r.tuck(), 0.0), "the Recommended page does not override `chrome_tuck`: the bar stays, tuck {}", r.tuck());
}

#[test]
fn the_watchlist_tucks_and_draws_no_capsule() {
    let mut r = World::watchlist("route-scroll-watchlist");
    for _ in 0..8 { r.press(Key::Down); }
    assert!(near(r.tuck(), 1.0), "the Watchlist tucks the chrome once its first row has gone, tuck {}", r.tuck());
}

/// The frames a route to the account menu is watched on: the chrome is tucked away while the menu
/// is up, and the strip holds focus with the page not resting at its top.
#[derive(Default)]
struct Watched { menu_frames: u32, menu_at_tuck: Vec<String>, strip_off_top: Vec<String> }

fn watch(r: &World, seen: &mut Watched) {
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
fn drive(r: &mut World, keys: &[Key], gap: u32, seen: &mut Watched) {
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
/// What this does NOT cover: a pointer click on the (undrawn) chip's place at tuck 1
/// (`chip_clicked` has no tuck gate); no key reaches that state.
fn the_account_menu_never_opens_over_a_tucked_page(name: &str, mount: fn() -> World, deep: fn(&mut World), leaves: &[&[Key]], gaps: &[u32]) {
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
    the_account_menu_never_opens_over_a_tucked_page("library", || World::movies_library("route-chip-library", true), |r| down_to_row(r, 8),
        &[&[Key::Back; 3], &[Key::Up; 10]], &[0, 1, 2]);
}

#[test]
fn the_account_menu_never_opens_over_a_tucked_collections_page() {
    // BACK on the Collections page leaves it (its first press returns the scroll to the top, its
    // second pops the page), so only Up walks to the strip here.
    the_account_menu_never_opens_over_a_tucked_page("collections", || World::collections("route-chip-collections", true), |r| down_to_row(r, 8), &[&[Key::Up; 10]], &[0, 1, 2]);
}

#[test]
fn the_account_menu_never_opens_over_a_tucked_watchlist() {
    the_account_menu_never_opens_over_a_tucked_page("watchlist", || World::watchlist("route-chip-watchlist"), |r| for _ in 0..8 { r.press(Key::Down); }, &[&[Key::Up; 16]], &[1, 2, 3]);
}
