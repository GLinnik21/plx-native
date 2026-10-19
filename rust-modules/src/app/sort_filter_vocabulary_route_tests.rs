//! **H2a at the app tier: one vocabulary for the Sort & Filter control and its read-outs** (design
//! review S15, S16, S17, N1), on the one route rig ([`World`]): a real `Dispatcher<AppHost>` over a
//! real store, every change a remote key.
//!
//! The rule each test pins is shared by every page that wears the thing, so a test visits both the
//! Library and the Watchlist: a page cannot keep a private wording.

use super::*;
use super::test_support::World;
use plx_plex::plex::watchlist::WatchKind;
use plx_screens::watchlist::page::{WatchlistScreen, PILL_ELEM};

fn watchlist(w: &World) -> &WatchlistScreen {
    w.d.top_screen().and_then(|s| s.as_any()).and_then(|a| a.downcast_ref()).expect("the Watchlist is the top page")
}

/// Forty titles, every even one a show.
fn mixed_list(name: &str) -> World {
    let script = super::watchlist_press_tests::Script::with_kinds(&(1..=40).collect::<Vec<u8>>(),
        |n| if n % 2 == 0 { WatchKind::Show } else { WatchKind::Movie });
    World::watchlist_over(name, script)
}

/// **S15: the pill's value is the current SORT, on every page, and a filter never shows in it.**
/// The Watchlist used to read "Default" while its panel ticked Date Added, and joined the filters
/// it had changed into the same text ("Movies, Title"); a filter shows only as the "N of M" read-out.
#[test]
fn the_pill_names_the_sort_on_both_pages_and_a_filter_shows_only_as_n_of_m() {
    let mut w = mixed_list("h2a-pill-watchlist");
    assert_eq!(watchlist(&w).probe_pill_value(), "Date Added", "at rest the pill names the sort the panel ticks");

    // Type > TV Shows (the menu's third row): twenty of forty, and the pill still names the sort.
    w.press(Key::Up);
    assert_eq!(w.elem(), Some(PILL_ELEM), "premise: focus is on the pill");
    w.press(Key::Ok);
    for _ in 0..2 { w.press(Key::Down); }
    w.press(Key::Ok);
    assert_eq!(watchlist(&w).count().and_then(|c| c.readout()).as_deref(), Some("20 of 40"), "premise: the filter narrowed the list");
    assert_eq!(watchlist(&w).probe_pill_value(), "Date Added", "a filter never shows in the pill");

    // Sort > Title (row 4): the pill follows the sort and only the sort.
    for _ in 0..2 { w.press(Key::Down); }
    assert_eq!(w.elem(), Some(4), "premise: the highlight is on Sort > Title");
    w.press(Key::Ok);
    assert_eq!(watchlist(&w).probe_pill_value(), "Title", "choosing a sort renames the pill");
    assert_eq!(watchlist(&w).count().and_then(|c| c.readout()).as_deref(), Some("20 of 40"), "and the filter is still the read-out's alone");

    // The Library's pill is the same rule: the sort in force. (One world at a time: the rig's lock.)
    drop(w);
    let library = World::movies_library("h2a-pill-library", false);
    assert_eq!(library.library().probe_sort_value().as_deref(), Some("Title"), "the Library's pill names its sort");
}
