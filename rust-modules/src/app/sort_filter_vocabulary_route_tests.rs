//! **H2a at the app tier: one vocabulary for the Sort & Filter control and its read-outs** (design
//! review S15, S16, S17, N1), on the one route rig ([`World`]): a real `Dispatcher<AppHost>` over a
//! real store, every change a remote key.
//!
//! The rule each test pins is shared by every page that wears the thing, so a test visits both the
//! Library and the Watchlist: a page cannot keep a private wording.

use super::*;
use super::test_support::World;
use plx_plex::plex::watchlist::WatchKind;
use plx_screens::library::menu::LibraryMenu;
use plx_screens::watchlist::menu::WatchlistMenuScreen;
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
/// The Watchlist used to read "Default" while its panel ticked Date added, and joined the filters
/// it had changed into the same text ("Movies, Title"); a filter shows only as the "N of M" read-out.
#[test]
fn the_pill_names_the_sort_on_both_pages_and_a_filter_shows_only_as_n_of_m() {
    let mut w = mixed_list("h2a-pill-watchlist");
    assert_eq!(watchlist(&w).probe_pill_value(), "Date added", "at rest the pill names the sort the panel ticks");

    // Type > TV Shows (the menu's third row): twenty of forty, and the pill still names the sort.
    w.press(Key::Up);
    assert_eq!(w.elem(), Some(PILL_ELEM), "premise: focus is on the pill");
    w.press(Key::Ok);
    for _ in 0..2 { w.press(Key::Down); }
    w.press(Key::Ok);
    assert_eq!(watchlist(&w).count().and_then(|c| c.readout()).as_deref(), Some("20 of 40"), "premise: the filter narrowed the list");
    assert_eq!(watchlist(&w).probe_pill_value(), "Date added", "a filter never shows in the pill");

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

/// A panel as it reads, from the menu screen on top: `(heading, [(label, value)])` per section.
type Panel = Vec<(String, Vec<(String, Option<String>)>)>;

fn watchlist_panel(w: &World) -> Panel {
    w.focused_screen().and_then(|s| s.as_any()).and_then(|a| a.downcast_ref::<WatchlistMenuScreen>())
        .expect("the Watchlist's Sort & Filter panel is open").probe_panel()
}

fn library_panel(w: &World) -> Panel {
    w.focused_screen().and_then(|s| s.as_any()).and_then(|a| a.downcast_ref::<LibraryMenu>())
        .expect("the Library's Sort & Filter panel is open").probe_panel()
}

fn section<'a>(panel: &'a Panel, heading: &str) -> &'a [(String, Option<String>)] {
    &panel.iter().find(|(h, _)| h == heading).unwrap_or_else(|| panic!("no {heading:?} section in {:?}",
        panel.iter().map(|(h, _)| h.as_str()).collect::<Vec<_>>())).1
}

/// **S17 + N1: the two Sort & Filter panels speak one vocabulary.** Section words (Show / Sort /
/// Filter), one sentence-case name per fact ("Score", not "Critic Rating" on one page and
/// "Rating" on the other), and the checked sort row's value is a WORD for the direction, the same
/// word for the same order on either page ("A–Z" for a title, "Newest first" for a date).
#[test]
fn both_sort_and_filter_panels_name_facts_and_directions_the_same_way() {
    // The Watchlist: Date added, newest first, is what a fresh list ticks.
    let mut w = mixed_list("h2a-vocab-watchlist");
    w.press(Key::Up);
    w.press(Key::Ok);
    let wl = watchlist_panel(&w);
    assert_eq!(wl.iter().map(|(h, _)| h.as_str()).collect::<Vec<_>>(), ["Show", "Sort", "Filter"]);
    let sort = section(&wl, "Sort");
    assert_eq!(sort.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(), ["Date added", "Title", "Release date", "Score"]);
    assert_eq!(sort[0].1.as_deref(), Some("Newest first"), "the direction is the checked row's value word");
    assert!(sort[1..].iter().all(|(_, v)| v.is_none()), "only the checked row carries one");
    // Sort > Title: ascending is its natural order.
    for _ in 0..4 { w.press(Key::Down); }
    w.press(Key::Ok);
    assert_eq!(section(&watchlist_panel(&w), "Sort")[1].1.as_deref(), Some("A–Z"));
    drop(w);

    // The Library: the same facts, the same words, from the server's own list.
    let mut w = World::movies_library("h2a-vocab-library", false);
    let entry = |key: &str, title: &str, default_desc: bool| plx_data::browse::SortEntry {
        key: key.into(), desc_key: String::new(), title: title.into(), default_desc };
    // What a PMS declares for a movie library (tests/mock_pms.py), titles as the server writes them.
    w.bridge.stores.browse.borrow_mut().seed_query_choices_for_test(vec![
        entry("titleSort", "Title", false), entry("addedAt", "Date Added", true),
        entry("originallyAvailableAt", "Release Date", true), entry("rating", "Critic Rating", true),
        entry("lastViewedAt", "Date Viewed", true)], Vec::new());
    w.frames(30);
    // The page arrives in its first poster (design review F2): the Sort & Filter control is one Up
    // and a walk along the header away, and OK on it opens the panel.
    let sort = w.library().probe_header_stops().into_iter().find(|(name, _)| *name == "sort").map(|(_, elem)| elem)
        .expect("premise: the Library's header has a Sort & Filter stop");
    w.walk_header_to(sort);
    w.press(Key::Ok);
    let lib = library_panel(&w);
    let headings: Vec<_> = lib.iter().map(|(h, _)| h.as_str()).collect();
    assert_eq!(headings, ["Sort", "Filter"], "a Movies library offers one type, so no Show section");
    let sort = section(&lib, "Sort");
    let labels: Vec<_> = sort.iter().map(|(l, _)| l.as_str()).collect();
    for (ours, theirs) in [("Title", "Title"), ("Date added", "Date Added"), ("Release date", "Release Date"), ("Score", "Critic Rating")] {
        assert!(labels.contains(&ours), "the Library names the fact {ours:?}: {labels:?}");
        assert!(ours == theirs || !labels.contains(&theirs), "and not by the server's {theirs:?}: {labels:?}");
    }
    assert_eq!(sort.iter().find(|(l, _)| l == "Title").and_then(|(_, v)| v.as_deref()), Some("A–Z"),
        "the Library's checked Title row says the same word the Watchlist's does");
}
