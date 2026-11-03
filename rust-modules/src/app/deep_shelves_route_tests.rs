//! **Deep pages under a key** (#858, #860, #861): the Library's Recommended page keeps its place when a
//! publication arrives, a deep Home walked with a held Down reaches every row on a card that has landed,
//! and a Home card's Detail returns onto that card at rest.

use super::*;
use super::test_support::{tick, DipImage, ImageHeld, World};
use plx_ui::containers::transition::{PageDip, PagePaint};

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
        let (at, rk) = w.home_card();
        assert_eq!(at, Some((row, 0)), "Down {row} lands on the next shelf's first card");
        assert!(rk.is_some_and(|rk| !rk.is_empty()), "the card Down {row} lands on has landed: a placeholder behind the key is a band that hugged the view");
    }
}

/// `frames` frames in which the held rows of section 0 answer, as a server answers them (`land_held_rows_for_test`),
/// once per frame (`World::settle` answers nothing).
fn land_and_step(w: &mut World, frames: u32) {
    for _ in 0..frames {
        w.bridge.stores.browse.borrow_mut().land_held_rows_for_test(0);
        w.step(vec![]);
    }
}

/// The `row=`, `col=`, `sid=` and `rk=` fields of the fingerprint: the seat a return must put back.
fn seat(probe: &str) -> [&str; 4] {
    ["row=", "col=", "sid=", "rk="].map(|key| probe.split_whitespace().find(|t| t.starts_with(key)).unwrap_or(""))
}

/// Is the card of this seat a published one? Its `rk=` field is neither absent (`rk=-`, no card) nor
/// empty (`rk=`, a placeholder slot with no read behind it).
fn published(seat: &[&str; 4]) -> bool { !matches!(seat[3], "" | "rk=" | "rk=-") }

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
    land_and_step(&mut w, 40);
    let page = AppArg::LibraryRecommended { section: 0 };
    assert!(w.on(&page), "premise: the Recommended page is on top");
    assert!(content_probe(&w.d, &w.bridge).contains(" region=shelf "), "premise: the page arrives in its first shelf");
    for _ in 0..24 {
        w.tap(Key::Down, 0);
        land_and_step(&mut w, 4);
    }
    land_and_step(&mut w, 40);
    let (_, band) = w.bridge.stores.browse.borrow().released_rows_for_test(0);
    assert!(band.0 > 0, "premise: the held band moved past the first sixteen shelves: {band:?}");
    w.tap(Key::Right, 0);
    land_and_step(&mut w, 30);
    let before = content_probe(&w.d, &w.bridge);
    assert!(before.contains(" region=shelf row=24 col=1 "), "premise: shelf 24, card 1: {before}");
    assert!(published(&seat(&before)), "premise: the card it will leave is a published one: {before}");

    let performed = w.press(Key::Ok);
    assert!(performed.iter().any(|p| p.starts_with("library:Detail")), "OK on the card opens its Detail: {performed:?}");
    assert!(!w.on(&page), "premise: Detail is on top of the page");
    let performed = w.press(Key::Back);
    assert!(performed.iter().any(|p| p == "content:Back"), "BACK leaves Detail: {performed:?}");
    land_and_step(&mut w, 60);
    assert!(w.on(&page), "the Recommended page is on top again");

    let after = content_probe(&w.d, &w.bridge);
    let (was, now) = (seat(&before), seat(&after));
    assert_eq!(now, was, "BACK lands on the card it left\nbefore: {before}\nafter:  {after}");
    assert!(published(&now), "the card is a published one: {after}");
    let (released, (lo, hi)) = w.bridge.stores.browse.borrow().released_rows_for_test(0);
    for row in 23..=25 {
        assert!(lo <= row && row <= hi, "shelf {row}, in view, is inside the band the store holds ({lo}..={hi})");
        assert!(!released.contains(&row), "shelf {row}, in view, is published, not a placeholder row: released {released:?}");
    }
}

/// **Home -> a card's Detail -> BACK lands on the card it left, and every released card is at exact
/// rest** (#858's remount path and #861's rest together). The OK raises `HomeReq::Detail` with the page's
/// `ReturnState`, and the World's pump performs it the way the loop does for a shelf card
/// (`perform_home_detail`: the loop's own `activate_deck_card`, so `activate_card` and `open_detail` carry the
/// return state). What this does NOT cover: `content::home_requests`' own arm and `activate_home_item` take the
/// whole `App`, which the World does not build, so a loop that dropped the `ReturnState` between them and
/// `activate_deck_card` passes this test.
#[test]
fn home_back_from_detail_lands_on_the_card_it_left_at_rest() {
    let mut w = World::home_grid("route-home-return", 5, 8);
    for _ in 0..3 { w.tap(Key::Down, 2); }
    for _ in 0..4 { w.tap(Key::Right, 2); }
    w.frames(60);
    let (at, rk) = w.home_card();
    assert_eq!(at, Some((2, 4)), "premise: the walk ended on shelf 2, card 4");
    w.sole_card_off_rest("premise: before Detail");

    // The Detail record is installed before the press, as `detail_with_disc` does it.
    let (sid, detail_rk) = w.home_focused_item().expect("premise: a card holds focus");
    assert_eq!(Some(&detail_rk), rk.as_ref(), "premise: the focused card is the one the walk reached");
    plx_data::metadata::set_current_for_test(w.bridge.stores.metadata.state_mut(), Some(plx_data::metadata::Detail {
        sid, rk: detail_rk.clone(), kind: "movie".into(), ..Default::default() }));
    let performed = w.press(Key::Ok);
    assert!(performed.iter().any(|p| p == "home:Detail"), "OK on a shelf card asks for its Detail: {performed:?}");
    w.frames(60);
    assert!(w.on(&AppArg::Content(ContentArg::Detail { sid, rk: detail_rk })), "premise: Detail is on top of Home");

    let performed = w.press(Key::Back);
    assert!(performed.iter().any(|p| p == "content:Back"), "BACK leaves Detail: {performed:?}");
    w.frames(60);
    assert!(w.on(&AppArg::Home), "Home is on top again");
    assert_eq!(w.home_card(), (at, rk), "BACK lands on the card it left");
    w.sole_card_off_rest("after the return");
}

/// **A dived Home comes back from a Detail with its focused card lifted and every word where it
/// stood, from the first frame of the BACK, under the dip's image.** Home's shelves are dormant only
/// while its hero is up (`snap.pos <= 0.5`), never for the route dip's image, so the seat a Detail
/// covered is the seat the page returns with and no band reopens under the shelf below it. That is
/// the rule `Shelf::dormant` gives the Recommended page's shelves, which follow the image: this pins
/// that Home needs none of it, and fails if the two ever come apart. Every text run the page draws is
/// compared with the words it drew before the Detail (`World::drawn`), so a heading or a caption
/// that moved by a band's height shows.
#[test]
fn home_back_from_detail_returns_its_focused_card_lifted_and_every_word_where_it_stood() {
    let _held = ImageHeld;
    let mut w = World::home_grid("route-home-return-lifted", 5, 8);
    w.d.nav.tabs.stack.transition = Box::new(PageDip::new());
    for _ in 0..3 { w.tap(Key::Down, 2); }
    for _ in 0..4 { w.tap(Key::Right, 2); }
    w.frames(60);
    let (at, rk) = w.home_card();
    let (shelf, card) = at.expect("premise: the walk ended on a card");
    let lift = plx_ui::cards::RowStyle::HOME.focus_scale;
    let words = |w: &mut World| {
        let mut runs: Vec<(String, f32)> = w.drawn().runs.into_iter().map(|run| (run.text, run.at.y)).collect();
        runs.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        runs
    };
    let stood = words(&mut w);
    assert!((w.card_pops()[shelf][card] - lift).abs() < 1e-4, "premise: the card stands in its full lift");

    let (sid, detail_rk) = w.home_focused_item().expect("premise: a card holds focus");
    plx_data::metadata::set_current_for_test(w.bridge.stores.metadata.state_mut(), Some(plx_data::metadata::Detail {
        sid, rk: detail_rk.clone(), kind: "movie".into(), ..Default::default() }));
    let mut dip = DipImage::default();
    let mut frame = |w: &mut World, inputs: Vec<InputEvent<u32>>| {
        let paint = w.step_dip(&mut dip, inputs).expect("a page is on top");
        plx_ui::containers::transition::set_page_image_held(!matches!(paint, PagePaint::Live));
    };
    let t = tick(w.frame + 1);
    frame(&mut w, script_key(Key::Ok, t));
    for _ in 0..90 { frame(&mut w, vec![]); }
    assert!(w.on(&AppArg::Content(ContentArg::Detail { sid, rk: detail_rk })), "premise: Detail is on top of Home");

    let t = tick(w.frame + 1);
    frame(&mut w, script_key(Key::Back, t));
    let mut home = 0;
    // the dip, its image and the settle after it (the hold hint, which teaches after a dwell, is later)
    for n in 0..60 {
        if n > 0 { frame(&mut w, vec![]); }
        if !w.on(&AppArg::Home) { continue; }
        home += 1;
        let pops = w.card_pops();
        assert!((pops[shelf][card] - lift).abs() < 1e-4, "frame {n}: the card is drawn at {:.4}, not in its full lift", pops[shelf][card]);
        let drawn = words(&mut w);
        plx_ui::containers::transition::set_page_image_held(!matches!(w.d.nav.tabs.stack.transition.phase_word(), "live"));
        // the bar's own words fade with the dip, so it is every word DRAWN that must have stood
        let moved: Vec<&(String, f32)> = drawn.iter()
            .filter(|(text, y)| !stood.iter().any(|(was, at)| was == text && (at - y).abs() < 0.5)).collect();
        assert!(moved.is_empty(), "frame {n}: words not where they stood: {moved:?}");
    }
    assert!(home > 30, "premise: Home was on top for most of the BACK ({home} frames)");
    assert_eq!(w.home_card(), (at, rk), "BACK lands on the card it left");
}
