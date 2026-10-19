//! **The driver the route-test lanes' key-driven tests share**: a real `Dispatcher<AppHost>` and
//! `Bridge`, a Library mounted the way the app mounts it, keys through `script_key`, and the loop's
//! own navigation (`test_support::pump_navigation`) once per frame.
//!
//! It is the same shape as `page_regions_tests::Rig` (that table owns its private copy, and its lane
//! keeps extending it), with two things that table's rig does not do: it can PIN a second movie
//! library, so the header row names two libraries (one library draws no names, no plate and no scroll
//! capsule), and it presses at two cadences (`press` waits for everything to settle; `tap` leaves a
//! frame or two between presses, the way a held remote repeats).

use super::*;
use super::test_support::{pump_navigation, tick};
use plx_screens::library::LibraryScreen;

/// The serial lock, the registered servers' cleanup and a signed-in-looking session: what every
/// test that mounts a store-backed page holds for its whole body.
pub(super) struct Guard {
    _session: plx_plex::plex::session::TempSession,
    _cleanup: Cleanup,
    _serial: plx_base::testlock::Serial,
}

struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) { plx_plex::plex::reset_servers_for_test(); }
}

impl Guard {
    pub(super) fn new(name: &str) -> Guard {
        let serial = plx_base::testlock::serial();
        let session = plx_plex::plex::session::TempSession::new(name);
        session.watching(&format!("u-{name}"));
        Guard { _session: session, _cleanup: Cleanup, _serial: serial }
    }
}

/// The section table's index of "Film Club" (a Movie library of the second source): the library the
/// rig pins beside "Movies" (index 0) when it is asked for two.
pub(super) const FILM_CLUB: usize = 2;

pub(super) struct Rig {
    pub(super) d: Dispatcher<AppHost>,
    pub(super) bridge: Bridge,
    pub(super) frame: u32,
    /// What the pump performed since the last `press`.
    pub(super) performed: Vec<String>,
    /// Landing what a page's store brought (the Watchlist's scripted plex.tv), once per frame.
    feed: Option<fn(&mut Dispatcher<AppHost>, &mut Bridge)>,
    /// Keeps the scripted plex.tv alive for the Watchlist.
    _script: Option<std::sync::Arc<super::watchlist_press_tests::Script>>,
}

impl Rig {
    /// Two registered servers and a section table of one Movie library per source, 120 films in the
    /// store; `two_libraries` pins "Film Club" beside "Movies" so the Movies header names both.
    pub(super) fn new(two_libraries: bool) -> Rig {
        plx_plex::plex::reset_servers_for_test();
        let own = plx_plex::plex::register_for_test("route-own", "127.0.0.1", 9, "synthetic", "fixture");
        let shared = plx_plex::plex::register_for_test("route-shared", "127.0.0.1", 10, "synthetic", "fixture");
        plx_plex::plex::set_current(own);
        let d = Dispatcher::<AppHost>::new();
        let mut bridge = Bridge::for_test(|| 0);
        bridge.stores.browse.borrow_mut().seed_registered_table_for_test([own, shared]);
        // Pinned the way the Home editor pins: one RECORDED answer for the profile. The table-only
        // `set_pinned_for_test` is overwritten by the next roster sync (it re-resolves every row from
        // the profile's record, on the discovery pump's wall-clock schedule), which moved the store
        // back to Movies a second or so into a test and made the header name one library again.
        if two_libraries {
            bridge.browse_run(plx_data::stores::browse::BrowseCmd::ApplyPins(vec![(FILM_CLUB, true)]));
        }
        bridge.refresh_browse_directory();
        bridge.browse_run(plx_data::stores::browse::BrowseCmd::SetCur(0));
        {
            let mut browse = bridge.stores.browse.borrow_mut();
            browse.seed_items_for_test(120);
            browse.seed_shelves_for_test(0, &["One", "Two", "Three", "Four", "Five", "Six"], 3);
            browse.seed_landscape_for_test(0, "Synthetic show");
        }
        Rig { d, bridge, frame: 0, performed: Vec::new(), feed: None, _script: None }
    }

    /// The Movies library, reached by its strip pill from Home, settled.
    pub(super) fn movies_library(two_libraries: bool) -> Rig {
        let mut r = Rig::new(two_libraries);
        super::show_page(&mut r.d, AppArg::Home);
        r.frames(40);
        super::nav_tab(&mut r.d, &mut r.bridge, HomeTab::Movies, None, None);
        r.frames(90);
        assert!(r.on(&AppArg::Library), "premise: the library is on top");
        r
    }

    /// The Collections page, opened with OK on the Library header's Collections control.
    pub(super) fn collections(two_libraries: bool) -> Rig {
        let mut r = Rig::movies_library(two_libraries);
        r.open_from_header(8, AppArg::LibraryCollections { section: 0 });
        // The page asked the store for its own listing; its answer is what it shows.
        r.bridge.stores.browse.borrow_mut().seed_items_for_test(120);
        r.frames(60);
        r
    }

    /// The Recommended page, opened with OK on the Library header's Recommended control.
    pub(super) fn recommended() -> Rig {
        let mut r = Rig::movies_library(false);
        r.open_from_header(9, AppArg::LibraryRecommended { section: 0 });
        r.frames(60);
        r
    }

    /// The Watchlist over a scripted plex.tv holding forty titles, opened by its strip pill.
    pub(super) fn watchlist() -> Rig {
        let script = super::watchlist_press_tests::Script::new(&(1..=40).collect::<Vec<u8>>());
        let (d, bridge) = super::watchlist_press_tests::rig_over(&script);
        let mut r = Rig { d, bridge, frame: 1, performed: Vec::new(), _script: Some(script), feed: Some(|d, bridge| {
            bridge.watchlist_for_test().pump();
            bridge.watchlist_feed(d);
        }) };
        super::nav_tab(&mut r.d, &mut r.bridge, HomeTab::Watchlist, None, None);
        r.frames(120);
        assert!(r.on(&AppArg::Watchlist), "premise: the Watchlist is on top");
        r
    }

    /// The scroll the page on top is travelling to (0 at its top).
    pub(super) fn scroll_target(&self) -> f32 {
        let any = self.d.top_screen().and_then(|screen| screen.as_any()).expect("a page is on top");
        if let Some(page) = any.downcast_ref::<LibraryScreen>() { return page.probe_scroll_target(); }
        if let Some(page) = any.downcast_ref::<plx_screens::watchlist::page::WatchlistScreen>() { return page.probe_scroll_target(); }
        panic!("the top page is neither a Library page nor the Watchlist");
    }

    /// Walk Right along the Library header to the stop `elem` and press OK on it; the page it opens
    /// is on top and settled afterwards.
    pub(super) fn open_from_header(&mut self, elem: u32, want: AppArg) {
        self.walk_header_to(elem);
        self.press(Key::Ok);
        self.frames(60);
        assert!(self.on(&want), "premise: OK on the header stop {elem} opened the wanted page");
    }

    pub(super) fn step(&mut self, inputs: Vec<InputEvent<u32>>) {
        self.frame += 1;
        if let Some(feed) = self.feed { feed(&mut self.d, &mut self.bridge); }
        plx_machine::idle::frame_begin(0.016);
        super::frame(&mut self.d, &mut self.bridge, tick(self.frame), inputs);
        self.performed.extend(pump_navigation(&mut self.d, &mut self.bridge));
    }

    pub(super) fn frames(&mut self, n: u32) {
        for _ in 0..n { self.step(vec![]); }
    }

    /// One key, both edges, then enough frames for a reseat, a press dip, a scroll spring and the
    /// navigation it commits to to play out. Returns what the pump performed meanwhile.
    pub(super) fn press(&mut self, key: Key) -> Vec<String> {
        self.tap(key, 28)
    }

    /// One key, then only `after` frames: a held remote's cadence, nothing settled in between.
    pub(super) fn tap(&mut self, key: Key, after: u32) -> Vec<String> {
        self.performed.clear();
        self.step(script_key(key, tick(self.frame + 1)));
        self.frames(after);
        std::mem::take(&mut self.performed)
    }

    /// Present the hit map the bar registers for this frame (`Dispatcher::present_bar_stops`): what a pointer
    /// event after it resolves against. A real page cannot be DRAWN on a host (no GL context, no font), so
    /// the page's own stops are not in this map; the bar's are, and they are the ones `Dispatcher::draw`
    /// registers, from the page's real `chrome_tuck` and the application's real strip members.
    pub(super) fn present_bar(&mut self) {
        self.d.present_bar_stops(&self.bridge);
    }

    /// A pointer click at `(x, y)`, then enough frames for the press and what it opens to play out.
    pub(super) fn click(&mut self, x: f32, y: f32) {
        self.step(vec![click_input(x, y, tick(self.frame + 1))]);
        self.frames(40);
    }

    /// Is the page on top `arg`'s kind of page? (A library page's `section` is the store's business.)
    pub(super) fn on(&self, arg: &AppArg) -> bool {
        match (self.d.top_arg(), arg) {
            (Some(AppArg::Library), AppArg::Library) | (Some(AppArg::Watchlist), AppArg::Watchlist) => true,
            (Some(AppArg::LibraryCollections { .. }), AppArg::LibraryCollections { .. }) => true,
            (Some(AppArg::LibraryRecommended { .. }), AppArg::LibraryRecommended { .. }) => true,
            _ => false,
        }
    }

    /// The Library page on top (the Library itself or its Collections page).
    pub(super) fn library(&self) -> &LibraryScreen {
        self.d.top_screen().and_then(|screen| screen.as_any()).and_then(|any| any.downcast_ref::<LibraryScreen>())
            .expect("a Library page is on top")
    }

    pub(super) fn elem(&self) -> Option<u32> { self.d.focus().map(|key| key.elem) }

    /// Is focus on the top bar's strip (the nav)?
    pub(super) fn on_nav(&self) -> bool { self.elem().is_some_and(|elem| elem >= plx_ui::dispatch::STRIP_BASE) }

    /// Is focus on one of the Library header's stops?
    pub(super) fn on_header(&self) -> bool {
        let stops = self.library().probe_header_stops();
        self.elem().is_some_and(|elem| stops.iter().any(|(_, stop)| *stop == elem))
    }

    /// Is focus on a grid card, and which `(row, col)`?
    pub(super) fn grid_cell(&self) -> Option<(usize, usize)> { self.library().grid_position(self.d.focus()) }

    /// How far the page on top has tucked the top chrome away.
    pub(super) fn tuck(&self) -> f32 { self.d.top_screen().expect("a page is on top").chrome_tuck() }

    /// How many header rows the page on top has built (any of the four pages that wear one).
    pub(super) fn header_builds(&self) -> u32 {
        let any = self.d.top_screen().and_then(|screen| screen.as_any()).expect("a page is on top");
        if let Some(page) = any.downcast_ref::<LibraryScreen>() { return page.probe_header_builds(); }
        if let Some(page) = any.downcast_ref::<plx_screens::library::RecommendedScreen>() { return page.probe_header_builds(); }
        if let Some(page) = any.downcast_ref::<plx_screens::watchlist::page::WatchlistScreen>() { return page.probe_header_builds(); }
        panic!("the top page wears no header row");
    }

    /// Walk Right along the Library header to the stop `elem` (an element id) and stop there.
    pub(super) fn walk_header_to(&mut self, elem: u32) {
        let n = self.library().probe_header_stops().len();
        for _ in 0..n + 1 {
            if self.elem() == Some(elem) { return; }
            self.press(Key::Right);
        }
        assert_eq!(self.elem(), Some(elem), "premise: Right along the header reached the stop");
    }
}
