//! **One route table for the pages that wear the tab bar** (focus model §8, T1): the Library, the
//! Collections page, the Recommended page and the Watchlist, driven through the real
//! `Dispatcher<AppHost>` + `Bridge` with scripted keys and the loop's own navigation
//! (`bridge_test_support::pump_navigation`, which calls the functions `content_requests` calls).
//!
//! The four pages are one column of regions, NAV (the container's strip), HEADER (one row of
//! stops), CONTENT and, on the Library, the RAIL. What a key does between the regions is the same
//! on every page, so it is asserted ONCE, here, row by row, not once per screen: the first seat, Up
//! from the content to the header, Up from the header onto the page's OWN pill, Down back, the ends
//! of the header row, and BACK from deep in the content, from the header and from the nav.
//!
//! **The regions are judged here, independently of the code under test.** NAV is `elem >=
//! STRIP_BASE` (a strip key carries the PAGE's entry, so the entry says nothing); HEADER is "the
//! element is one of the screen's own header stops". A table that classified focus with the
//! library the fix introduces could not see a misclassification, and the Library's BACK loop between
//! header and nav WAS one.
//!
//! A later route-test lane extends the table by adding a `Row` and, if its page opens differently,
//! a `mount`. Everything a row needs is in `Rig`: `press`, `place`, `stop`, `frames`.

use super::*;
use super::test_support::{pump_navigation, tick};
use plx_machine::machine::GroupId;
use plx_screens::library::{LibraryScreen, RecommendedScreen};
use plx_screens::watchlist::page::WatchlistScreen;
use plx_ui::widgets::{TAB_KEY_MOVIES, TAB_KEY_WATCHLIST};

/// Where focus is, in the model's terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place { Nav, Header, Content }

/// A route's name for a failure line (`AppArg` has no `Debug`; `route_word` calls all three library
/// pages "library").
fn named(arg: Option<&AppArg>) -> &'static str {
    match arg {
        None => "no page",
        Some(AppArg::LibraryCollections { .. }) => "collections",
        Some(AppArg::LibraryRecommended { .. }) => "recommended",
        Some(arg) => crate::app::words::route_word(arg),
    }
}

/// What one BACK did: left focus in a region of the page, or left the page with a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Out { Stay(Place), Leave(&'static str) }

/// The requests that END a page (the pump's labels start with them).
const LEAVES: [&str; 3] = ["library:BackToHome", "content:Back", "watchlist:Back"];

struct Row {
    name: &'static str,
    mount: fn() -> Rig,
    /// The region the page seats on its first arrival (model §4.1): the content, on every page.
    arrival: Place,
    /// The header stop Up from the content lands on when focus has not been on the row yet (the
    /// crumb root, the Sort & Filter pill). `None`: the stop focus last used on the row, which is
    /// where the arrival seated it (the Library's row has no library-name stop while the server
    /// holds a single library, so the stop is the row's own first arrival).
    up_lands: Option<&'static str>,
    /// The strip pill that is the page's own.
    own_pill: u32,
    /// BACK pressed repeatedly from deep in the content.
    back_from_content: &'static [Out],
    /// …from the header row.
    back_from_header: &'static [Out],
    /// …from the strip (every page: one press leaves).
    back_from_nav: &'static [Out],
    /// The page on top once the page has left.
    leaves_to: AppArg,
    /// The header stop of the PARENT page that opened this one; BACK returns focus to it.
    opener: Option<&'static str>,
}

const ROWS: [Row; 4] = [
    Row {
        name: "Library", mount: movies_library, arrival: Place::Content, up_lands: Some("collections"), own_pill: TAB_KEY_MOVIES,
        back_from_content: &[Out::Stay(Place::Content), Out::Stay(Place::Header), Out::Stay(Place::Nav), Out::Leave("library:BackToHome")],
        back_from_header: &[Out::Stay(Place::Nav), Out::Leave("library:BackToHome")],
        back_from_nav: &[Out::Leave("library:BackToHome")],
        leaves_to: AppArg::Home, opener: None,
    },
    Row {
        name: "Collections", mount: collections, arrival: Place::Content, up_lands: Some("crumb"), own_pill: TAB_KEY_MOVIES,
        back_from_content: &[Out::Stay(Place::Content), Out::Leave("content:Back")],
        back_from_header: &[Out::Leave("content:Back")],
        back_from_nav: &[Out::Leave("content:Back")],
        leaves_to: AppArg::Library, opener: Some("collections"),
    },
    Row {
        name: "Recommended", mount: recommended, arrival: Place::Content, up_lands: Some("crumb"), own_pill: TAB_KEY_MOVIES,
        back_from_content: &[Out::Stay(Place::Content), Out::Leave("content:Back")],
        back_from_header: &[Out::Leave("content:Back")],
        back_from_nav: &[Out::Leave("content:Back")],
        leaves_to: AppArg::Library, opener: Some("recommended"),
    },
    Row {
        name: "Watchlist", mount: watchlist, arrival: Place::Content, up_lands: Some("sort"), own_pill: TAB_KEY_WATCHLIST,
        back_from_content: &[Out::Stay(Place::Content), Out::Leave("watchlist:Back")],
        back_from_header: &[Out::Leave("watchlist:Back")],
        back_from_nav: &[Out::Leave("watchlist:Back")],
        leaves_to: AppArg::Home, opener: None,
    },
];

// ---- the rig ------------------------------------------------------------------------------------

/// A page mounted the way the app mounts it, and the means to press keys at it.
struct Rig {
    d: Dispatcher<AppHost>,
    bridge: Bridge,
    frame: u32,
    /// Landing what a page's store brought (the Watchlist's scripted plex.tv), once per frame.
    feed: Option<fn(&mut Dispatcher<AppHost>, &mut Bridge)>,
    /// Keeps the scripted plex.tv alive for the Watchlist row.
    _script: Option<std::sync::Arc<super::watchlist_press_tests::Script>>,
    performed: Vec<String>,
}

impl Rig {
    /// Two registered servers, 120 films in the store and six shelves of three cards.
    fn new() -> Rig {
        plx_plex::plex::reset_servers_for_test();
        crate::app::collections_probe::clear_seeds_for_test();
        let own = plx_plex::plex::register_for_test("regions-own", "127.0.0.1", 9, "synthetic", "fixture");
        let shared = plx_plex::plex::register_for_test("regions-shared", "127.0.0.1", 10, "synthetic", "fixture");
        plx_plex::plex::set_current(own);
        let d = Dispatcher::<AppHost>::new();
        let mut bridge = Bridge::for_test(|| 0);
        bridge.stores.browse.borrow_mut().seed_registered_table_for_test([own, shared]);
        bridge.refresh_browse_directory();
        bridge.seed_collections_for_test(plx_plex::plex::collections::CollectionsCount::Count(super::test_support::RIG_COLLECTIONS));
        bridge.browse_run(plx_data::stores::browse::BrowseCmd::SetCur(0));
        {
            let mut browse = bridge.stores.browse.borrow_mut();
            browse.seed_items_for_test(120);
            browse.seed_shelves_for_test(0, &["One", "Two", "Three", "Four", "Five", "Six"], 3);
            browse.seed_landscape_for_test(0, "Synthetic show");
        }
        Rig { d, bridge, frame: 0, feed: None, _script: None, performed: Vec::new() }
    }

    fn step(&mut self, inputs: Vec<InputEvent<u32>>) {
        self.frame += 1;
        if let Some(feed) = self.feed { feed(&mut self.d, &mut self.bridge); }
        plx_machine::idle::frame_begin(0.016);
        super::frame(&mut self.d, &mut self.bridge, tick(self.frame), inputs);
        self.performed.extend(pump_navigation(&mut self.d, &mut self.bridge));
    }

    fn frames(&mut self, n: u32) {
        for _ in 0..n { self.step(vec![]); }
    }

    /// One key, both edges, then enough frames for a reseat, a press dip and the navigation it
    /// commits to play out. Returns what the pump performed meanwhile.
    fn press(&mut self, key: Key) -> Vec<String> {
        self.performed.clear();
        self.step(script_key(key, tick(self.frame + 1)));
        self.frames(28);
        std::mem::take(&mut self.performed)
    }

    /// The page's header: its group and stops, from the screen on top.
    fn header(&self) -> (GroupId, Vec<(&'static str, u32)>) {
        let any = self.d.top_screen().and_then(|screen| screen.as_any()).expect("a page is on top");
        if let Some(page) = any.downcast_ref::<LibraryScreen>() { return (page.toolbar_group(), page.probe_header_stops()); }
        if let Some(page) = any.downcast_ref::<RecommendedScreen>() { return page.probe_header(); }
        if let Some(page) = any.downcast_ref::<WatchlistScreen>() { return page.probe_header(); }
        panic!("the top page is none of the four");
    }

    /// The scroll the page on top is travelling to (0 at its top).
    fn scroll_target(&self) -> f32 {
        let any = self.d.top_screen().and_then(|screen| screen.as_any()).expect("a page is on top");
        if let Some(page) = any.downcast_ref::<LibraryScreen>() { return page.probe_scroll_target(); }
        if let Some(page) = any.downcast_ref::<RecommendedScreen>() { return page.probe_scroll_target(); }
        if let Some(page) = any.downcast_ref::<WatchlistScreen>() { return page.probe_scroll_target(); }
        panic!("the top page is none of the four");
    }

    /// The focused key and the region it is in.
    fn place(&self) -> Option<(Place, FocusKey<u32>)> {
        let key = self.d.focus()?;
        if key.elem >= plx_ui::dispatch::STRIP_BASE { return Some((Place::Nav, key)); }
        let (_, stops) = self.header();
        Some((if stops.iter().any(|(_, elem)| *elem == key.elem) { Place::Header } else { Place::Content }, key))
    }

    /// The name of the header stop focus is on, if it is on one.
    fn stop(&self) -> Option<&'static str> {
        let key = self.d.focus()?;
        self.header().1.into_iter().find(|(_, elem)| *elem == key.elem).map(|(name, _)| name)
    }

    fn region(&self) -> Option<Place> { self.place().map(|(place, _)| place) }

    fn elem(&self) -> Option<u32> { self.d.focus().map(|key| key.elem) }

    /// Is the page on top `arg`'s kind of page? (A library page's `section` is the store's business.)
    fn on(&self, arg: &AppArg) -> bool { named(self.d.top_arg()) == named(Some(arg)) }

    /// Walk Right along the header to the stop named `name` and press OK on it; the page it opens
    /// is on top and settled afterwards.
    fn open_from_header(&mut self, name: &str, want: AppArg) {
        let (_, stops) = self.header();
        let elem = stops.iter().find(|(stop, _)| *stop == name).unwrap_or_else(|| panic!("no {name} stop in {stops:?}")).1;
        // The page arrives in its first poster: Up reaches the header row.
        for _ in 0..3 {
            if self.region() == Some(Place::Header) { break; }
            self.press(Key::Up);
        }
        for _ in 0..stops.len() + 1 {
            if self.elem() == Some(elem) { break; }
            self.press(Key::Right);
        }
        assert_eq!(self.elem(), Some(elem), "premise: focus reached the {name} control");
        self.press(Key::Ok);
        self.frames(60);
        assert!(self.on(&want), "premise: OK on {name} opened {}, not {}", named(Some(&want)), named(self.d.top_arg()));
    }
}

/// The Movies library, reached by its strip pill from Home, settled.
fn movies_library() -> Rig {
    let mut r = Rig::new();
    super::show_page(&mut r.d, AppArg::Home);
    r.frames(40);
    super::nav_tab(&mut r.d, &mut r.bridge, HomeTab::Movies, None, None);
    r.frames(90);
    assert!(r.on(&AppArg::Library), "premise: the library is on top");
    r
}

/// The Collections page, opened with OK on the Library header's Collections control.
fn collections() -> Rig {
    let mut r = movies_library();
    r.open_from_header("collections", AppArg::LibraryCollections { section: 0 });
    // The page asked the store for its own listing; its answer is what it shows.
    r.bridge.stores.browse.borrow_mut().seed_items_for_test(120);
    r.frames(60);
    r
}

/// The Recommended page, opened with OK on the Library header's Recommended control.
fn recommended() -> Rig {
    let mut r = movies_library();
    r.open_from_header("recommended", AppArg::LibraryRecommended { section: 0 });
    r.frames(60);
    r
}

/// The Watchlist over a scripted plex.tv holding forty titles, opened by its strip pill.
fn watchlist() -> Rig {
    let script = super::watchlist_press_tests::Script::new(&(1..=40).collect::<Vec<u8>>());
    let (d, bridge) = super::watchlist_press_tests::rig_over(&script);
    let mut r = Rig { d, bridge, frame: 1, feed: Some(|d, bridge| {
        bridge.watchlist_for_test().pump();
        bridge.watchlist_feed(d);
    }), _script: Some(script), performed: Vec::new() };
    super::nav_tab(&mut r.d, &mut r.bridge, HomeTab::Watchlist, None, None);
    r.frames(120);
    assert!(r.on(&AppArg::Watchlist), "premise: the Watchlist is on top");
    r
}

// ---- the table ----------------------------------------------------------------------------------

/// Every cell's failure, collected so one red run names ALL of them (a first panic in one row would
/// hide the rest).
#[derive(Default)]
struct Fails(Vec<String>);

impl Fails {
    fn check(&mut self, ok: bool, what: impl FnOnce() -> String) {
        if !ok { self.0.push(what()); }
    }
}

/// The outcome of one BACK, from what the page did and where focus is now.
fn outcome(r: &Rig, performed: &[String]) -> Out {
    match performed.iter().find_map(|label| LEAVES.iter().find(|leave| label.starts_with(**leave))) {
        Some(leave) => Out::Leave(leave),
        None => match r.region() {
            Some(place) => Out::Stay(place),
            None => Out::Stay(Place::Content),
        },
    }
}

/// BACK pressed until the page leaves or `expected` is spent; each press's outcome is compared.
fn back_sequence(r: &mut Rig, row: &Row, what: &str, expected: &[Out], f: &mut Fails) {
    let mut seen = Vec::new();
    for _ in 0..expected.len() {
        let performed = r.press(Key::Back);
        let out = outcome(r, &performed);
        seen.push(out);
        if matches!(out, Out::Leave(_)) { break; }
    }
    f.check(seen == expected, || format!("{}: BACK {what}: expected {expected:?}, saw {seen:?}", row.name));
    if let Some(Out::Leave(_)) = expected.last() {
        r.frames(80);
        f.check(r.on(&row.leaves_to), || format!("{}: BACK {what} should end on {}, the top page is {}", row.name, named(Some(&row.leaves_to)), named(r.d.top_arg())));
    }
}

/// Up from the content head onto the header, then onto the nav, and the way back down.
fn columns(row: &Row, f: &mut Fails) {
    let mut r = (row.mount)();
    let name = row.name;
    f.check(!r.d.nav.tabs.strip.is_empty(), || format!("{name}: the strip is published (its pills are not in focus without it)"));
    f.check(r.region() == Some(row.arrival), || format!("{name}: arrival seat is {:?}, expected {:?}", r.region(), row.arrival));
    let arrived_on = r.stop();
    if r.region() == Some(Place::Header) {
        r.press(Key::Down);
        f.check(r.region() == Some(Place::Content), || format!("{name}: Down from the header reaches the content, focus is {:?}", r.place()));
    }
    r.press(Key::Up);
    f.check(r.region() == Some(Place::Header), || format!("{name}: Up from the content head reaches the header, focus is {:?}", r.place()));
    let want = row.up_lands.or(arrived_on);
    f.check(want.is_some() && r.stop() == want, || format!("{name}: Up from the content lands on the {want:?} stop, not {:?}", r.stop()));
    let header_stop = r.elem();
    r.press(Key::Up);
    f.check(r.region() == Some(Place::Nav), || format!("{name}: Up from the header reaches the nav, focus is {:?}", r.place()));
    f.check(r.elem() == Some(row.own_pill), || format!(
        "{name}: Up lands on the page's own pill {:#x}, not {:?}", row.own_pill, r.elem().map(|e| format!("{e:#x}"))));
    r.press(Key::Down);
    f.check(r.region() == Some(Place::Header) && r.elem() == header_stop, || format!(
        "{name}: Down from the nav returns to the header stop it left ({header_stop:?}), focus is {:?}", r.place()));
    // The ends of the header row: no wrap, no leaving it.
    let (_, stops) = r.header();
    for _ in 0..stops.len() + 1 { r.press(Key::Left); }
    f.check(r.elem() == Some(stops[0].1) && r.region() == Some(Place::Header), || format!(
        "{name}: Left stops at the first header stop {:?}, focus is {:?}", stops[0], r.place()));
    for (index, (stop, elem)) in stops.iter().enumerate().skip(1) {
        r.press(Key::Right);
        f.check(r.elem() == Some(*elem), || format!("{name}: Right #{index} reaches the {stop} stop, focus is {:?}", r.place()));
    }
    r.press(Key::Right);
    f.check(r.elem() == Some(stops[stops.len() - 1].1) && r.region() == Some(Place::Header), || format!(
        "{name}: Right stops at the last header stop, focus is {:?}", r.place()));
}

/// BACK from deep in the content, from the header and from the nav.
fn back(row: &Row, f: &mut Fails) {
    // The first BACK from a scrolled page returns it to its top, and focus stays in the content.
    let mut scrolled = (row.mount)();
    for _ in 0..8 { scrolled.press(Key::Down); }
    f.check(scrolled.scroll_target() > 0.5, || format!("{}: premise: eight Downs scrolled the page, target {}", row.name, scrolled.scroll_target()));
    scrolled.press(Key::Back);
    f.check(scrolled.scroll_target() < 0.5 && scrolled.region() == Some(Place::Content), || format!(
        "{}: BACK from deep in the content returns to the top, target {} focus {:?}", row.name, scrolled.scroll_target(), scrolled.place()));

    let mut deep = (row.mount)();
    for _ in 0..8 { deep.press(Key::Down); }
    back_sequence(&mut deep, row, "from deep in the content", row.back_from_content, f);

    let mut header = (row.mount)();
    if header.region() != Some(Place::Header) { header.press(Key::Up); }
    f.check(header.region() == Some(Place::Header), || format!("{}: premise for BACK from the header: focus is {:?}", row.name, header.place()));
    back_sequence(&mut header, row, "from the header", row.back_from_header, f);

    let mut nav = (row.mount)();
    for _ in 0..2 { if nav.region() != Some(Place::Nav) { nav.press(Key::Up); } }
    f.check(nav.region() == Some(Place::Nav), || format!("{}: premise for BACK from the nav: focus is {:?}", row.name, nav.place()));
    back_sequence(&mut nav, row, "from the nav", row.back_from_nav, f);
}

/// A pushed page returns focus to the control that opened it.
fn opener(row: &Row, f: &mut Fails) {
    let Some(opener) = row.opener else { return };
    let mut r = (row.mount)();
    // Leave by the page's own BACK, however many presses that takes.
    for _ in 0..4 {
        if r.on(&row.leaves_to) { break; }
        r.press(Key::Back);
    }
    r.frames(80);
    f.check(r.on(&row.leaves_to), || format!("{}: premise: BACK left the page, top is {}", row.name, named(r.d.top_arg())));
    f.check(r.stop() == Some(opener), || format!(
        "{}: BACK returns focus to the {opener} control that opened the page, focus is {:?} ({:?})", row.name, r.place(), r.stop()));
}

#[test]
fn the_pages_that_wear_the_tab_bar_share_one_route_table() {
    let _guard = plx_base::testlock::serial();
    struct Cleanup;
    impl Drop for Cleanup { fn drop(&mut self) { plx_plex::plex::reset_servers_for_test(); } }
    let _cleanup = Cleanup;
    let session = plx_plex::plex::session::TempSession::new("page-regions");
    session.watching("u-page-regions");
    let mut fails = Fails::default();
    for row in &ROWS {
        columns(row, &mut fails);
        back(row, &mut fails);
        opener(row, &mut fails);
    }
    assert!(fails.0.is_empty(), "{} cells of the route table failed:\n{}", fails.0.len(), fails.0.join("\n"));
}

/// **T2: the strip is published exactly when the route wears the bar.** Over EVERY page argument
/// (`every_route()`, which now holds the library's Collections and Recommended pages): after the
/// page has been through its frames, the container holds strip members if and only if
/// `chrome() == TabBar`. The two predicates the loop used to spell by route name (what
/// `capture_chrome` publishes, what `update` steps) are now ONE, `bridge::wears_tab_bar`, and this
/// is what says it is the one the registry's table writes: a page added with `Chrome::TabBar` and
/// no strip, or a strip on a page that draws no bar, fails here and not on the television as Up
/// finding nothing to land on.
#[test]
fn the_strip_is_published_exactly_when_the_route_wears_the_tab_bar() {
    use plx_ui::screen::ScreenArg;
    let _guard = plx_base::testlock::serial();
    for route in super::super::words::every_route() {
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        super::test_support::frame(&mut d, &mut rig, route.clone(), tick(0), vec![]);
        for t in 1..4 {
            super::frame(&mut d, &mut rig, tick(t), vec![]);
        }
        let word = super::super::words::route_word(&route);
        let wears = route.chrome() == plx_machine::machine::Chrome::TabBar;
        assert_eq!(wears_tab_bar(&route), wears, "{word}: the app's predicate is the registry's table");
        assert_eq!(!d.nav.tabs.strip.is_empty(), wears,
            "{word} ({}): the strip is published exactly when the route wears the tab bar",
            named(Some(&route)));
        assert_eq!(d.nav.tabs.strip_selected.is_some(), wears,
            "{word}: the page's own pill is published with the strip, and only with it");
    }
}

/// **Every page that wears the bar owns the pointer once it is up** (`owns_input`). The run loop's
/// MOUSEBUTTONDOWN used to try a second path for the profile chip, `chip_clicked`, for the window
/// in which `owns_input` was false while a bar was drawn; there is no such window — the bar is
/// drawn only for a mounted page, and every page that wears it answers `FocusSource::Engine` — so
/// the second path was dead and is gone, and the chip and the pills are hit-map stops registered
/// where the bar is drawn (`Dispatcher::bar_stops`). This is what keeps that true: a page added
/// with `Chrome::TabBar` that answered `FocusSource::Legacy` would hand its pointer to a loop that
/// has no chip path any more, and fails here.
#[test]
fn every_route_that_wears_the_tab_bar_owns_the_pointer() {
    use plx_ui::screen::ScreenArg;
    let _guard = plx_base::testlock::serial();
    let mut wearers = 0;
    for route in super::super::words::every_route() {
        if route.chrome() != plx_machine::machine::Chrome::TabBar { continue; }
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        super::test_support::frame(&mut d, &mut rig, route.clone(), tick(0), vec![]);
        for t in 1..4 {
            super::frame(&mut d, &mut rig, tick(t), vec![]);
        }
        wearers += 1;
        assert!(owns_input(&d),
            "{} ({}): a page that wears the tab bar owns the pointer (no legacy path for the chip)",
            super::super::words::route_word(&route), named(Some(&route)));
    }
    assert!(wearers >= 4, "the loop saw the Library family and the Watchlist, not {wearers} routes");
}

/// **Home follows the same landing rule** (the strip seat is shared by every page that wears the
/// bar): Up from a card at the far right of the grid lands on the HOME pill — the page's own — and
/// not on whichever pill sits above that card (it used to be the nearest by x, which from there is
/// the Watchlist or Search).
#[test]
fn home_up_from_a_far_card_lands_on_the_home_pill() {
    let _guard = plx_base::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.hubs.seed_grid_for_test(3, 24);
    let mut i = 0;
    fn run(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, i: &mut u32, key: Option<Key>, n: u32) {
        for k in 0..n {
            let inputs = match key { Some(key) if k == 0 => script_key(key, tick(*i)), _ => vec![] };
            super::test_support::frame(d, rig, AppArg::Home, tick(*i), inputs);
            *i += 1;
        }
    }
    run(&mut d, &mut rig, &mut i, None, 40);
    assert!(rig.home_command(HomeCmd::FocusGrid { row: 0, col: 18 }));
    run(&mut d, &mut rig, &mut i, None, 40);
    let start = d.focus().expect("a card is focused");
    assert!(start.elem < plx_ui::dispatch::STRIP_BASE, "premise: focus starts in the page, not on the strip");
    for _ in 0..4 {
        if d.focus().is_some_and(|k| k.elem >= plx_ui::dispatch::STRIP_BASE) { break; }
        run(&mut d, &mut rig, &mut i, Some(Key::Up), 30);
    }
    assert_eq!(d.focus().map(|k| k.elem), Some(plx_ui::widgets::TAB_KEY_HOME),
        "Up from the page reaches the nav on the Home pill");
}
