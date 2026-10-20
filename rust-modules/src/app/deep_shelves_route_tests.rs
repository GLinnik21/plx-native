//! **Deep pages under a key** (#858, #860): the Library's Recommended page keeps its place when a
//! publication arrives, and a deep Home walked with a held Down reaches every row while the store is
//! told the band far less often than the rows pass.

use super::*;
use super::test_support::World;

/// **A publication that adds a shelf leaves the focused card where it is, and the new shelf is
/// reachable.** The Recommended page rebuilds only the rows whose basis changed (`RowSeats`), so the
/// rows it keeps keep their keys: the engine's focus (an element id) still names the same card.
#[test]
fn a_publication_that_adds_a_shelf_leaves_the_focused_card_where_it_is_and_the_new_shelf_is_reachable() {
    let mut w = World::recommended("route-rec-publication");
    w.press(Key::Right);
    w.press(Key::Down);
    let at = content_probe(&w.d, &w.bridge);
    assert!(at.contains(" region=shelf row=1 col=1 "), "premise: shelf 1, card 1: {at}");
    let seat = w.focus();
    w.bridge.stores.browse.borrow_mut().seed_shelves_for_test(0, &["One", "Two", "Three", "Four", "Five", "Six", "Seven"], 3);
    w.frames(40);
    assert_eq!(w.focus(), seat, "the publication did not move focus off its card");
    for _ in 0..5 { w.press(Key::Down); }
    let last = content_probe(&w.d, &w.bridge);
    assert!(last.contains(" region=shelf row=6 "), "the seventh shelf the publication added is reachable by Down: {last}");
}

/// **A held Down through a deep Home reaches every row in order, on a card that has landed.** The
/// screen tells the store which rows it wants held only when the rows in view near the edge of the
/// band it last sent (`ui::cards::HoldBand`: margin 2, slack 2), so a band that hugged the view would
/// leave the next row's cards behind the key. The rows the ring gives up are answered the way a server
/// answers them (`land_kept_rows_for_test`), as in `home_retention_tests`. The band count itself
/// (`the_hold_band_speaks_once_per_slack_...`) is the cards crate's; the app tier has no counter of
/// the commands the store was sent.
#[test]
fn a_held_down_through_a_deep_home_reaches_every_row_in_order_on_a_card_that_has_landed() {
    const ROWS: usize = 48;
    let mut w = World::home_grid("route-home-deep", ROWS, 4);
    for row in 0..ROWS {
        w.bridge.stores.hubs.land_kept_rows_for_test();
        w.tap(Key::Down, 3);
        let (at, landed) = w.bridge.with_home(&w.d, |home, cx, focus| (home.grid_position::<AppHost>(focus, cx),
            home.focused_item::<AppHost>(focus, cx).is_some_and(|item| !item.rk.is_empty()))).unwrap();
        assert_eq!(at, Some((row, 0)), "Down {row} lands on the next shelf's first card");
        assert!(landed, "the card Down {row} lands on has landed: a placeholder behind the key is a band that hugged the view");
    }
}

/// The held rows of section 0 answer, as a server answers them (`land_held_rows_for_test`), once per frame.
fn settle(w: &mut World, frames: u32) {
    for _ in 0..frames {
        w.bridge.stores.browse.borrow_mut().land_held_rows_for_test(0);
        w.step(vec![]);
    }
}

/// `(row, col, sid, rk)` of the card the fingerprint names: the seat a return must put back.
fn seat(probe: &str) -> Vec<String> {
    ["row=", "col=", "sid=", "rk="].iter()
        .map(|key| probe.split_whitespace().find(|t| t.starts_with(key)).unwrap_or("").to_string())
        .collect()
}

/// **A deep Recommended page returns from Detail onto the card it left, with every shelf in view
/// published** (#860). Forty shelves, the store holding sixteen of them: Down walks the focus past the
/// band the page first held (shelves 0..15), so the rows around it are the ones the page asked for on
/// the way. One Right, OK into Detail, BACK: focus is on the same shelf, column and ratingKey, and the
/// rows in view are inside the band and published, not placeholders with no read behind them (the
/// held rows answer each frame, as the server would; nothing answers while Detail is up).
#[test]
fn a_deep_recommended_page_returns_from_detail_onto_the_same_card_with_its_shelves_published() {
    let mut w = World::recommended("route-rec-deep-return");
    let rows: Vec<(String, String, String)> = (0..40)
        .map(|i| (format!("movie.row{i}"), format!("/hubs/sections/1/row{i}"), format!("Row {i}"))).collect();
    let named: Vec<(&str, &str, &str)> = rows.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str())).collect();
    w.bridge.stores.browse.borrow_mut().seed_named_shelves_for_test(0, &named, 3);
    settle(&mut w, 40);
    let page = AppArg::LibraryRecommended { section: 0 };
    assert!(w.on(&page), "premise: the Recommended page is on top");
    if !content_probe(&w.d, &w.bridge).contains(" region=shelf ") { w.tap(Key::Down, 0); settle(&mut w, 30); }
    for _ in 0..24 {
        w.tap(Key::Down, 0);
        settle(&mut w, 4);
    }
    settle(&mut w, 40);
    let (_, band) = w.bridge.stores.browse.borrow().released_rows_for_test(0);
    assert!(band.0 > 0, "premise: the held band moved past the first sixteen shelves: {band:?}");
    w.tap(Key::Right, 0);
    settle(&mut w, 30);
    let before = content_probe(&w.d, &w.bridge);
    assert!(before.contains(" region=shelf row=24 col=1 "), "premise: shelf 24, card 1: {before}");

    let performed = w.press(Key::Ok);
    assert!(performed.iter().any(|p| p.starts_with("library:Detail")), "OK on the card opens its Detail: {performed:?}");
    assert!(!w.on(&page), "premise: Detail is on top of the page");
    let performed = w.press(Key::Back);
    assert!(performed.iter().any(|p| p == "content:Back"), "BACK leaves Detail: {performed:?}");
    settle(&mut w, 60);
    assert!(w.on(&page), "the Recommended page is on top again");

    let after = content_probe(&w.d, &w.bridge);
    assert_eq!(seat(&after), seat(&before), "BACK lands on the card it left\nbefore: {before}\nafter:  {after}");
    assert!(!seat(&after)[3].is_empty() && seat(&after)[3] != "rk=-", "the card is a published one: {after}");
    let (released, (lo, hi)) = w.bridge.stores.browse.borrow().released_rows_for_test(0);
    for row in 23..=25 {
        assert!(lo <= row && row <= hi, "shelf {row}, in view, is inside the band the store holds ({lo}..={hi})");
        assert!(!released.contains(&row), "shelf {row}, in view, is published, not a placeholder row: released {released:?}");
    }
}
