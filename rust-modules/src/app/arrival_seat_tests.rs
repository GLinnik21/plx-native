//! **Every page lands in its content, and no arrival seat is a control that leaves the page**
//! (design review B1, B2, S1; focus model §4.1). Table-driven over the pages that wear the tab bar
//! and the Collection screen, key-driven on a real `Dispatcher<AppHost>` over a real store
//! (`World`, the app tier's one rig).
//!
//! The Library's header row leads with controls whose OK leaves or changes the page: the Back
//! crumb pops the page it was opened from, "Collections · N" pushes another. A page that rested on
//! one of them made the reflex OK undo the navigation that had just opened the page, and every
//! visit cost a Down before browsing. The seat a page arrives on is therefore its first tile, with
//! the header one Up away.
//!
//! "Leaves the page" is read as the loop's own requests: `library:BackToHome`, `content:Back`,
//! `watchlist:Back` (a pop) and `library:OpenListing` (the header's push). A tile that opens its
//! own Detail is the seat doing its job and is not one of them.

use super::*;
use super::test_support::World;
use plx_screens::library::{LibraryScreen, RecommendedScreen};
use plx_screens::watchlist::page::WatchlistScreen;
use plx_ui::dispatch::STRIP_BASE;

/// The requests that pop the page, and the header's push (`LibraryCmd::OpenListing`).
const LEAVES: [&str; 4] = ["library:BackToHome", "content:Back", "watchlist:Back", "library:OpenListing"];

/// Where focus is, judged apart from the code under test: the bar is `elem >= STRIP_BASE` (a strip
/// key carries the page's entry), the header is one of the page's own header stops, and everything
/// else the page holds is content.
fn region(w: &World) -> &'static str {
    let Some(focus) = w.focus() else { return "nowhere" };
    if focus.elem >= STRIP_BASE { return "nav"; }
    let any = w.focused_screen().and_then(|screen| screen.as_any());
    let stops = if let Some(page) = any.and_then(|any| any.downcast_ref::<LibraryScreen>()) { page.probe_header_stops() }
        else if let Some(page) = any.and_then(|any| any.downcast_ref::<RecommendedScreen>()) { page.probe_header().1 }
        else if let Some(page) = any.and_then(|any| any.downcast_ref::<WatchlistScreen>()) { page.probe_header().1 }
        else { Vec::new() };
    if stops.iter().any(|(_, elem)| *elem == focus.elem) { "header" } else { "content" }
}

/// The Collection screen of the first collection the Collections page lists, its members landed.
pub(super) fn collection_screen(name: &str) -> World {
    collection_screen_of(name, |sid| (0..9).map(|i| plx_data::pms::PmsMovie { sid, rk: format!("m{i}"), title: format!("Movie {i}"), ..Default::default() }).collect())
}

/// [`collection_screen`] whose members are `members` of the collection's own server.
pub(super) fn collection_screen_of(name: &str, members: impl FnOnce(plx_plex::plex::ServerId) -> Vec<plx_data::pms::PmsMovie>) -> World {
    let mut w = World::collections(name, false);
    w.bridge.stores.browse.borrow_mut().seed_collections_for_test(12);
    w.frames(40);
    // The Collections page arrives on its first collection's tile.
    w.press(Key::Ok);
    w.frames(40);
    assert!(matches!(w.d.top_arg(), Some(AppArg::Content(ContentArg::Collection(_)))), "premise: OK on a collection opened its screen");
    let items = members(w.own);
    w.bridge.stores.collection.install_for_test(items, plx_data::collection::CollectionStatus::Ready);
    w.frames(40);
    w
}

struct Row {
    name: &'static str,
    mount: fn() -> World,
}

const ROWS: [Row; 6] = [
    Row { name: "library, two libraries", mount: || World::movies_library("arrival-multi", true) },
    Row { name: "library, one library", mount: || World::movies_library("arrival-one", false) },
    Row { name: "collections", mount: || World::collections("arrival-collections", false) },
    Row { name: "recommended", mount: || World::recommended("arrival-recommended") },
    Row { name: "watchlist", mount: || World::watchlist("arrival-watchlist") },
    Row { name: "collection screen", mount: || collection_screen("arrival-collection") },
];

/// **OK on the arrival seat never leaves the page**: the reflex press, straight after the page
/// settles, raises none of the requests that pop it or push the header's pages.
#[test]
fn ok_on_the_arrival_seat_never_leaves_the_page() {
    let mut fails = Vec::new();
    let mut seen = 0;
    for row in &ROWS {
        let mut w = (row.mount)();
        let seat = (region(&w), w.elem());
        let performed = w.press(Key::Ok);
        seen += 1;
        if let Some(leave) = performed.iter().find(|label| LEAVES.iter().any(|leave| label.starts_with(leave))) {
            fails.push(format!("{}: OK on the arrival seat ({seat:?}) raised {leave}", row.name));
        }
    }
    assert_eq!(seen, ROWS.len());
    assert!(fails.is_empty(), "{} pages arrive on a control that leaves them:\n{}", fails.len(), fails.join("\n"));
}

/// **The arrival seat is a tile**: not the bar, not a header stop. The header is one Up away.
#[test]
fn every_page_arrives_in_its_content() {
    let mut fails = Vec::new();
    for row in &ROWS {
        let w = (row.mount)();
        let seat = region(&w);
        if seat != "content" { fails.push(format!("{}: arrives in the {seat} (elem {:?}), not in its content", row.name, w.elem())); }
    }
    assert!(fails.is_empty(), "{} pages do not arrive in their content:\n{}", fails.len(), fails.join("\n"));
}

/// **Up then Down is a round trip** (design review S2, routes.md R3): from a cell off the first
/// column, Up reaches the header, Left and Right walk along it, and Down returns to the cell Up
/// left from, not to the one projected under the stop the walk ended on. The pages are the four
/// that wear the tab bar; the seat is declared once for them in `PageRegions`.
const ROUND_TRIP: [&str; 4] = ["library, one library", "collections", "recommended", "watchlist"];

#[test]
fn up_then_down_is_a_round_trip_on_every_page() {
    let mut fails = Vec::new();
    let mut seen = 0;
    for row in ROWS.iter().filter(|row| ROUND_TRIP.contains(&row.name)) {
        let mut w = (row.mount)();
        seen += 1;
        // A cell off the first column, and one row down where the page has rows.
        for key in [Key::Right, Key::Right, Key::Right, Key::Down] { w.press(key); }
        if region(&w) != "content" { fails.push(format!("{}: premise: the walk left the content", row.name)); continue; }
        // Up until the header, remembering the last content cell on the way.
        let mut last = w.elem();
        for _ in 0..8 {
            if region(&w) == "header" { break; }
            last = w.elem();
            w.press(Key::Up);
        }
        if region(&w) != "header" { fails.push(format!("{}: premise: Up never reached the header", row.name)); continue; }
        // Walk along the header: to its far end and back one stop, so the stop is not the entry's.
        let entry = w.elem();
        for _ in 0..8 { w.press(Key::Right); }
        w.press(Key::Left);
        if w.elem() == entry && region(&w) == "header" { w.press(Key::Left); }
        if region(&w) != "header" { fails.push(format!("{}: premise: the walk left the header", row.name)); continue; }
        w.press(Key::Down);
        if w.elem() != last {
            fails.push(format!("{}: Down from the header landed on {:?}, not on the cell Up left from ({last:?})", row.name, w.elem()));
        }
    }
    assert_eq!(seen, ROUND_TRIP.len());
    assert!(fails.is_empty(), "{} pages do not return to the remembered cell:\n{}", fails.len(), fails.join("\n"));
}

/// The Collections page opened with OK on the Library header's control, its listing NOT answered:
/// the frames after the press run with the page dissolved in and nothing to show yet.
fn collections_awaiting_its_listing(name: &str) -> World {
    let mut w = World::movies_library(name, false);
    w.walk_header_to(8);
    w.tap(Key::Ok, 0);
    w.frames(90);
    assert!(w.on(&AppArg::LibraryCollections { section: 0 }), "premise: the Collections page is on top");
    w
}

/// **A listing that lands after the page has dissolved in lifts its first poster, it does not pop
/// it** (the late seat is the standard focus-lift spring from rest, as the dissolve's last frame
/// starts it): frame by frame from the frame the poster is seated, the drawn lift is below its full
/// scale and rises monotonically to it. It used to adopt the lift whole, the focus mark appearing in
/// one frame.
#[test]
fn a_listing_that_lands_after_the_dissolve_lifts_its_first_poster_from_rest() {
    let mut w = collections_awaiting_its_listing("late-lift");
    w.bridge.stores.browse.borrow_mut().seed_collections_for_test(12);
    let mut lift = Vec::new();
    for _ in 0..90 {
        w.frames(1);
        let seated = w.library().probe_viewport(w.focus()).0 == "grid";
        let scale = w.library().probe_arrival(w.focus()).1;
        if seated || !lift.is_empty() { lift.push(scale); }
    }
    assert!(!lift.is_empty(), "premise: the first poster was seated once the listing landed");
    let full = *lift.last().unwrap();
    assert!(full > 1.02, "premise: the seated poster ends lifted: {full}");
    assert!(lift[0] < 1.0 + (full - 1.0) * 0.5, "the lift is adopted whole on the frame the listing lands: {} of {full}: {lift:?}", lift[0]);
    assert!(lift.windows(2).all(|pair| pair[1] >= pair[0] - 1e-4), "the lift is not a monotone rise: {lift:?}");
}

/// **While a page's listing is still loading the page seats nothing** (the arrival seat is never a
/// control whose OK leaves the page, and with no tile yet there is none to take): no focus mark, OK
/// does nothing, BACK leaves as it does from a page with nothing focused, Up seats the engine's
/// no-focus fallback (the reader's own move, which the cells that land afterwards do not undo), Down,
/// Left and Right are held (nothing seated until the cells land, then the first poster once), and with
/// no key the cells that land seat the first poster.
struct Loading {
    name: &'static str,
    mount: fn() -> World,
    /// The answer the page is waiting for.
    land: fn(&mut World),
    /// The request BACK raises from a page with nothing focused.
    back: &'static str,
}

fn land_films(w: &mut World) { w.bridge.stores.browse.borrow_mut().seed_items_for_test(120); }
fn land_collections(w: &mut World) { w.bridge.stores.browse.borrow_mut().seed_collections_for_test(12); }

fn land_watchlist(w: &mut World) { w.land_watchlist(); }

fn land_shelves(w: &mut World) { w.land_shelves(); }

const LOADING: [Loading; 5] = [
    Loading { name: "library, two libraries", mount: || World::movies_library_loading("loading-multi", true), land: land_films, back: "library:BackToHome" },
    Loading { name: "library, one library", mount: || World::movies_library_loading("loading-one", false), land: land_films, back: "library:BackToHome" },
    Loading { name: "collections", mount: || World::collections_loading("loading-collections"), land: land_collections, back: "content:Back" },
    Loading { name: "watchlist", mount: || World::watchlist_loading("loading-watchlist"), land: land_watchlist, back: "watchlist:Back" },
    Loading { name: "recommended", mount: || World::recommended_pending("loading-recommended"), land: land_shelves, back: "content:Back" },
];

#[test]
fn a_page_whose_listing_is_loading_seats_nothing_and_ok_does_nothing() {
    let mut fails = Vec::new();
    for row in &LOADING {
        let mut w = (row.mount)();
        if w.focus().is_some() { fails.push(format!("{}: seated {:?} ({}) while loading", row.name, w.elem(), region(&w))); }
        let top = w.d.top_arg().cloned();
        let performed = w.press(Key::Ok);
        if !performed.is_empty() { fails.push(format!("{}: OK with nothing seated raised {performed:?}", row.name)); }
        if w.d.top_arg().cloned() != top { fails.push(format!("{}: OK with nothing seated changed the page", row.name)); }
        if w.focus().is_some() { fails.push(format!("{}: OK seated {:?}", row.name, w.elem())); }
    }
    assert!(fails.is_empty(), "{} pages seat while loading:\n{}", fails.len(), fails.join("\n"));
}

/// BACK from a page with nothing focused is the leave it is from the bar: the Library goes Home, the
/// Collections page pops to the Library that opened it.
#[test]
fn back_from_a_page_that_seats_nothing_leaves_it() {
    let mut fails = Vec::new();
    for row in &LOADING {
        let mut w = (row.mount)();
        let performed = w.press(Key::Back);
        if !performed.iter().any(|label| label.starts_with(row.back)) {
            fails.push(format!("{}: BACK with nothing seated performed {performed:?}, not {}", row.name, row.back));
        }
    }
    assert!(fails.is_empty(), "{} pages do not leave on BACK:\n{}", fails.len(), fails.join("\n"));
}

/// **Up seats the bar, and the cells that land afterwards leave it there**: Up is the reader's own
/// move to the bar, so the landing does not pull focus to the first poster. The seat is the engine's
/// no-focus fallback, the first group the page can offer, which on every page that wears the bar is
/// the bar, on the page's own pill (OK on it is the tab it is already on); the header is one Down.
#[test]
fn up_seats_the_bar_and_the_landing_cells_do_not_take_it_back() {
    let mut fails = Vec::new();
    for row in &LOADING {
        let mut w = (row.mount)();
        let performed = w.press(Key::Up);
        if region(&w) != "nav" { fails.push(format!("{}: Up seated the {} ({:?}), not the bar", row.name, region(&w), w.elem())); continue; }
        if !performed.is_empty() { fails.push(format!("{}: Up raised {performed:?}", row.name)); }
        let seat = w.elem();
        (row.land)(&mut w);
        w.frames(90);
        if w.elem() != seat { fails.push(format!("{}: after Up the landing cells moved focus from {seat:?} to {:?} ({})", row.name, w.elem(), region(&w))); }
        // And the header is one Down from there, as on any page.
        w.press(Key::Down);
        if region(&w) != "header" { fails.push(format!("{}: Down from the bar reached the {}, not the header", row.name, region(&w))); }
    }
    assert!(fails.is_empty(), "{} pages take the seat back:\n{}", fails.len(), fails.join("\n"));
}

/// **Down, Left and Right HOLD the pending arrival** (design review CONFIRM judgment 6): a page whose
/// listing is still loading has nothing to move to, and the bar is not where the reader was going. The
/// key seats nothing and raises nothing, nothing is drawn until the cells land, and then the first
/// poster is seated ONCE, the same one a page the reader never touched seats: never the bar, never the
/// poster and then a move off it, and no later landing re-seats it. A key pressed after the seat is an
/// ordinary move.
#[test]
fn down_left_and_right_hold_the_pending_arrival_and_the_first_poster_lights_once() {
    let mut fails = Vec::new();
    for row in &LOADING {
        // The reference: the same page, the reader pressing nothing.
        // (A `World` holds the suite's serial lock, so it is dropped before the next is mounted.)
        let arrival = {
            let mut reference = (row.mount)();
            (row.land)(&mut reference);
            reference.frames(90);
            (reference.elem(), region(&reference))
        };
        for key in [Key::Down, Key::Left, Key::Right] {
            let mut w = (row.mount)();
            let performed = w.press(key);
            if w.focus().is_some() { fails.push(format!("{}: {key:?} seated {:?} ({}) while loading", row.name, w.elem(), region(&w))); continue; }
            if !performed.is_empty() { fails.push(format!("{}: {key:?} raised {performed:?}", row.name)); }
            // Held across more frames and a second press: still nothing drawn.
            w.frames(20);
            w.press(key);
            if w.focus().is_some() { fails.push(format!("{}: a second {key:?} seated {:?} ({}) while loading", row.name, w.elem(), region(&w))); continue; }
            (row.land)(&mut w);
            // Frame by frame: the focus is nothing and then the first poster, never anything between.
            let mut seen = Vec::new();
            for _ in 0..90 {
                w.frames(1);
                let at = w.focus().map(|_| (w.elem(), region(&w)));
                if seen.last() != Some(&at) { seen.push(at); }
            }
            let want = [None, Some(arrival)];
            let want_seated = [Some(arrival)];
            if seen != want && seen != want_seated {
                fails.push(format!("{}: after {key:?} the focus went {seen:?}, not once to the first poster {arrival:?}", row.name));
            }
        }
    }
    assert!(fails.is_empty(), "{} pages do not hold the arrival:\n{}", fails.len(), fails.join("\n"));
}

/// With no key, the cells that land seat the first poster (the arrival, which lifts from rest:
/// `a_listing_that_lands_after_the_dissolve_lifts_its_first_poster_from_rest`).
#[test]
fn the_cells_that_land_seat_the_first_poster_when_the_reader_has_not_moved() {
    let mut fails = Vec::new();
    for row in &LOADING {
        let mut w = (row.mount)();
        (row.land)(&mut w);
        w.frames(90);
        if region(&w) != "content" { fails.push(format!("{}: the landing seated the {} ({:?}), not a poster", row.name, region(&w), w.elem())); }
    }
    assert!(fails.is_empty(), "{} pages do not seat their first poster:\n{}", fails.len(), fails.join("\n"));
}
