//! **R14 at the app tier: the Watchlist's routes, key by key** (`routes.md` R14), on the one route
//! rig ([`World`]): a real `Dispatcher<AppHost>` and `Bridge` over a scripted plex.tv, the loop's
//! feeds and request pump, every press a remote key.
//!
//! The disc's halves were each proven alone: the hero tests (`screens/src/home/hero_watchlist_tests`,
//! `detail/hero_watchlist_tests`) show a press EMITS an Add or Remove, `watchlist_press_tests` show a
//! request WRITES. Here the chain runs whole: keys to the disc, OK, the write plex.tv is sent, the
//! face the page then wears, OK again, the removal. And the Watchlist page's own header and Sort &
//! Filter menu, walked by keys.
//!
//! plex.tv is scripted in every test (`watchlist_press_tests::Script`): nothing here has a live path.

use super::*;
use super::test_support::{detail_arg, World};
use plx_data::catalog::Membership;
use plx_data::watchlist::ledger::Intent;
use plx_plex::plex::watchlist::WatchKind;
use plx_screens::watchlist::page::{WatchlistScreen, PILL_ELEM};

/// The movie the Home hero and the Detail page stand for: a catalog id the scripted list (titles
/// `[1; 12]` .. `[40; 12]`) does not hold, so its disc reads "Add" once the list has landed.
const HEX: [u8; 12] = [0xa1; 12];

fn home(w: &World) -> &plx_screens::home::HomeScreen {
    w.d.top_screen().and_then(|s| s.as_any()).and_then(|a| a.downcast_ref()).expect("Home is the top page")
}

fn detail(w: &World) -> &plx_screens::detail::DetailScreen {
    w.d.top_screen().and_then(|s| s.as_any()).and_then(|a| a.downcast_ref()).expect("a Detail page is the top page")
}

fn page(w: &World) -> &WatchlistScreen {
    w.d.top_screen().and_then(|s| s.as_any()).and_then(|a| a.downcast_ref()).expect("the Watchlist is the top page")
}

/// The membership the Home hero's disc wears, and the title it was fed for.
fn home_disc(w: &World) -> Option<([u8; 12], Membership)> {
    home(w).probe_for_test().disc
}

/// Is a menu (the Sort & Filter surface) up over the page?
fn menu_up(w: &World) -> bool {
    w.d.nav.modals.top().is_some()
}

/// **The Home hero's disc, by keys:** Right from Play reaches the disc, OK sends plex.tv exactly one
/// add for the hero's title and the disc wears the filled face, OK again sends the removal and the
/// face goes back, and BACK opens nothing and writes nothing.
#[test]
fn the_home_heros_disc_adds_then_removes_the_title_by_keys() {
    let mut w = World::home_with_disc("r14-home-disc", HEX);
    w.frames(40);
    assert_eq!(home(&w).watchlist_hex(), Some(HEX), "premise: the hero stands for the title");
    assert_eq!(home_disc(&w), Some((HEX, Membership::Off)), "premise: the list landed and the title is not on it");
    assert!(w.writes().is_empty(), "premise: nothing was written");
    let play = w.elem();
    assert!(play.is_some(), "premise: focus is on the hero");

    // Right from Play is the disc (the row is Play, the disc, Info).
    w.press(Key::Right);
    let disc = w.elem();
    assert_ne!(disc, play, "Right from Play moves to the next control");
    assert_eq!(w.writes(), vec![], "moving to the disc writes nothing");

    w.press(Key::Ok);
    assert_eq!(w.writes(), vec![(HEX, Intent::Add)], "OK on the disc sends exactly one add for the hero's title");
    assert_eq!(home_disc(&w), Some((HEX, Membership::On)), "and the disc wears the face of a title on the Watchlist");
    assert_eq!(w.elem(), disc, "the press leaves focus on the disc");

    w.press(Key::Ok);
    assert_eq!(w.writes(), vec![(HEX, Intent::Add), (HEX, Intent::Remove)], "OK again sends the removal");
    assert_eq!(home_disc(&w), Some((HEX, Membership::Off)), "and the face goes back");

    w.press(Key::Back);
    assert!(w.on(&AppArg::Home), "BACK on the disc leaves Home on top");
    assert!(!menu_up(&w), "and opens no menu");
    assert_eq!(w.elem(), disc, "with focus where it was");
    assert_eq!(w.writes().len(), 2, "and writes nothing");
}

/// **The same chain on a movie's Detail page:** Right from Play reaches the bookmark disc, OK adds,
/// OK again removes, and BACK pops the page.
#[test]
fn a_movie_details_disc_adds_then_removes_the_title_by_keys() {
    let mut w = World::detail_with_disc("r14-detail-disc", HEX);
    w.frames(40);
    assert_eq!(detail(&w).watchlist_hex(), Some(HEX), "premise: the page names the title's catalog id");
    assert_eq!(detail(&w).watchlist(), Membership::Off, "premise: the list landed and the title is not on it");
    assert!(w.writes().is_empty(), "premise: nothing was written");
    let play = w.elem();
    assert!(play.is_some(), "premise: focus is on the hero");

    w.press(Key::Right);
    let disc = w.elem();
    assert_ne!(disc, play, "Right from Play moves to the next control");
    assert_eq!(w.writes(), vec![], "moving to the disc writes nothing");

    w.press(Key::Ok);
    assert_eq!(w.writes(), vec![(HEX, Intent::Add)], "OK on the disc sends exactly one add for the page's title");
    assert_eq!(detail(&w).watchlist(), Membership::On, "and the disc wears the face of a title on the Watchlist");
    assert_eq!(w.elem(), disc, "the press leaves focus on the disc");

    w.press(Key::Ok);
    assert_eq!(w.writes(), vec![(HEX, Intent::Add), (HEX, Intent::Remove)], "OK again sends the removal");
    assert_eq!(detail(&w).watchlist(), Membership::Off, "and the face goes back");

    w.press(Key::Back);
    assert!(!w.on(&detail_arg("movie")), "BACK leaves the Detail page");
    assert!(w.on(&AppArg::Home), "for the page it was opened over");
    assert_eq!(w.writes().len(), 2, "and writes nothing");
}

/// **The Watchlist page's header row, by keys.** The row has ONE stop (the Sort & Filter pill: the
/// title is a plate, not a control), so "walking it" is: Up from the first tile reaches the pill,
/// Left and Right keep focus on the pill (they neither leave sideways to the strip nor fall into the
/// grid), and Down enters the grid on the first row's tile under the pill (the engine's geometric Down:
/// the pill is right-aligned, over the sixth column).
#[test]
fn the_watchlist_header_row_holds_left_and_right_and_down_enters_the_grid() {
    let mut w = World::watchlist("r14-header");
    let first = page(&w).elem_at(0);
    assert!(first.is_some(), "premise: the list landed");
    assert_eq!(w.elem(), first, "premise: the arrival is seated on the first tile");
    assert_eq!(page(&w).probe_header().1, vec![("sort", PILL_ELEM)], "the row's stops, in order: just the pill");

    w.press(Key::Up);
    assert_eq!(w.elem(), Some(PILL_ELEM), "Up from the first tile reaches the header's pill");
    for key in [Key::Left, Key::Right, Key::Right, Key::Left] {
        w.press(key);
        assert_eq!(w.elem(), Some(PILL_ELEM), "{key:?} along the header never leaves it sideways");
    }
    assert!(!w.on_nav(), "and never reaches the strip");

    // The pill is right-aligned over the sixth column, and the engine's Down lands on the tile under it.
    let under_pill = page(&w).elem_at(5);
    w.press(Key::Down);
    assert_eq!(w.elem(), under_pill, "Down enters the grid, on the first row's tile under the pill");
    assert_ne!(w.elem(), Some(PILL_ELEM));
}

/// **Sort & Filter, by keys:** OK on the pill opens the menu; choosing Title reorders the grid;
/// choosing TV Shows narrows it and the header reads "N of M"; choosing the defaults again (the menu
/// has no Reset row) puts back the full list in its first order and takes the read-out away.
#[test]
fn the_sort_and_filter_menu_reorders_narrows_and_resets_the_grid_by_keys() {
    // Forty titles, every even one a show: Type > TV Shows leaves twenty of forty.
    let script = super::watchlist_press_tests::Script::with_kinds(&(1..=40).collect::<Vec<u8>>(),
        |n| if n % 2 == 0 { WatchKind::Show } else { WatchKind::Movie });
    let mut w = World::watchlist_over("r14-sort-filter", script);
    let order = |w: &World| page(w).tiles().iter().map(|t| t.row.guid.clone()).collect::<Vec<_>>();
    let default_order = order(&w);
    assert_eq!(default_order.len(), 40, "premise: the whole list is on the page");
    assert_eq!(page(&w).count().and_then(|c| c.readout()), None, "premise: nothing narrows the list, nothing is read out");

    // The menu opens on the first row (Type > Movies & Shows); the rows run Type (3), Sort (4), Show (3).
    w.press(Key::Up);
    assert_eq!(w.elem(), Some(PILL_ELEM), "premise: focus is on the pill");
    w.press(Key::Ok);
    assert!(menu_up(&w), "OK on the pill opens the Sort & Filter menu");
    assert_eq!(w.elem(), Some(0), "on its first row");

    // Sort > Title (row 4): the grid is in title order, which is not the date order.
    for _ in 0..4 { w.press(Key::Down); }
    assert_eq!(w.elem(), Some(4), "premise: the highlight is on Sort > Title");
    w.press(Key::Ok);
    let by_title = order(&w);
    assert_eq!(by_title.len(), 40, "a sort drops nothing");
    assert_ne!(by_title, default_order, "choosing Title reorders the grid");
    let titles: Vec<String> = page(&w).tiles().iter().map(|t| t.row.title.clone()).collect();
    assert!(titles.windows(2).all(|pair| pair[0] <= pair[1]), "and the order is the titles': {titles:?}");
    assert_eq!(page(&w).count().and_then(|c| c.readout()), None, "a sort alone narrows nothing");

    // Type > TV Shows (row 2): twenty of the forty, and the header says so.
    for _ in 0..2 { w.press(Key::Up); }
    assert_eq!(w.elem(), Some(2), "premise: the highlight is on Type > TV Shows");
    w.press(Key::Ok);
    assert_eq!(page(&w).tiles().len(), 20, "the grid holds the shows");
    assert_eq!(page(&w).count(), Some(plx_screens::watchlist::header::Count::Of { shown: 20, total: 40 }));
    let readout = page(&w).count().and_then(|c| c.readout()).expect("a narrowed list reads out");
    assert!(readout.contains("20") && readout.contains("40"), "the header's read-out names both numbers: {readout:?}");

    // Back to the defaults: Type > Movies & Shows (row 0), then Sort > Date added (row 3).
    for _ in 0..2 { w.press(Key::Up); }
    w.press(Key::Ok);
    for _ in 0..3 { w.press(Key::Down); }
    assert_eq!(w.elem(), Some(3), "premise: the highlight is on Sort > Date added");
    w.press(Key::Ok);
    assert_eq!(order(&w), default_order, "the full list is back in its first order");
    assert_eq!(page(&w).count().and_then(|c| c.readout()), None, "and the read-out is gone");

    w.press(Key::Back);
    assert!(!menu_up(&w), "BACK closes the menu");
    assert_eq!(w.elem(), Some(PILL_ELEM), "with focus back on the pill");
}
