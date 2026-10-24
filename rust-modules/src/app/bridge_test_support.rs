//! Shared driver helpers for bridge.rs's split test modules: frame/tick drivers, route
//! argument builders, the directory-policy fixture used by the ViewState/chrome tests, and the
//! key-driven route-test [`World`] (the one harness the route lanes share).

use super::*;

/// **Put `route` on top, then run one frame** — the TEST driver that replaced `sync_page`.
///
/// It is deliberately the same three-line decision `sync_page` made every frame in production
/// (reuse an entry that is already on the stack, otherwise root a peer or stack a page), kept
/// here so that ~240 existing assertions still read as "drive a frame on this screen". What
/// changed is WHO says it: production asks for the op it wants at the press, and no frame
/// derives a navigation from a second copy of the route.
pub(super) fn goto(d: &mut Dispatcher<AppHost>, want: AppArg) {
    if d.has_pending_navigation() { return; }
    super::show_page(d, want);
}

pub(super) fn frame(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: AppArg, tick: Tick, inputs: Vec<InputEvent<u32>>) -> (&'static str, FrameReport) {
    goto(d, route);
    let out = presented(super::frame(d, rig, tick, inputs));
    rig.assert_no_store_jobs_left_for_test();
    out
}

/// …and the same driver for the two frames that also want the effect tap or a supplied result
/// set. Only the `goto` is the test's: everything after it is production's own frame.
pub(super) fn frame_with_tap(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: AppArg, tick: Tick,
    inputs: Vec<InputEvent<u32>>, tap: &mut dyn plx_ui::dispatch::Tap<AppHost>)
    -> (&'static str, FrameReport) {
    goto(d, route);
    let out = presented(super::frame_with_tap(d, rig, tick, inputs, tap));
    rig.assert_no_store_jobs_left_for_test();
    out
}

pub(super) fn frame_with_results(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: AppArg, tick: Tick,
    inputs: Vec<InputEvent<u32>>, take: impl FnOnce() -> AppResults,
    tap: &mut dyn plx_ui::dispatch::Tap<AppHost>) -> (&'static str, FrameReport) {
    goto(d, route);
    let out = presented(super::frame_with_results(d, rig, tick, inputs, take, tap));
    rig.assert_no_store_jobs_left_for_test();
    out
}

/// A detail page's argument, for the tests that used to name `Route::Detail` and let a trail
/// supply the identity. The fold puts the identity ON the argument, which is what makes the
/// trail seeding this helper used to do unnecessary.
pub(super) fn detail_arg(rk: &str) -> AppArg {
    AppArg::Content(ContentArg::Detail { sid: plx_plex::plex::ServerId::UNSET, rk: rk.into() })
}

/// …and a person page's.
pub(super) fn person_arg(key: &str) -> AppArg {
    AppArg::Content(ContentArg::Person {
        sid: plx_plex::plex::ServerId::UNSET,
        key: key.into(),
        guid: format!("tag://{key}"),
        name: String::new(),
        thumb: String::new(),
    })
}


pub(super) fn tick(i: u32) -> Tick {
    Tick {
        ms: i * 16,
        dt_us: 16_000,
    }
}

/// **Perform the page navigations the screens asked for this frame**, through the SAME functions
/// the loop's `content_requests` calls (`content::library_navigation`, `content_back`,
/// `watchlist_navigation`), under the same two guards: the asking page still holds input, and the
/// route is one that answers the request. A test that presses a key and wants the BACK, the
/// header link or the strip pill to take effect calls this once per frame, in the order the loop
/// does (after the dispatcher's frame).
///
/// Returns one label per request it PERFORMED (`"library:BackToHome { kind: Movie }"`,
/// `"content:Back"`, `"watchlist:Back"`, `"home:FoldToHero"`, `"home:Detail"`), so a table can assert which request a key
/// produced. A request that is not a navigation (a menu, a play) is labelled
/// `"unperformed ..."` and left undone: this pump does not stand in for the rest of the loop, and
/// a test that needs one of those should say so rather than have it dropped (a shelf publication is the
/// store's business and is not listed). [`Pump`] is the one
/// that performs the menus and the poster presses too.
pub(super) fn pump_navigation(d: &mut Dispatcher<AppHost>, rig: &mut Bridge) -> Vec<String> {
    pump_requests(d, rig, None)
}

/// **What the loop owns that a poster press or a menu needs beyond the dispatcher and the
/// bridge**: the playback session, the player adapter, the show/season Play continuation and the
/// OK-armed flag. [`Pump::run`] is [`pump_navigation`] plus the requests that need them, performed
/// by the loop's own `content::library_page_request` and the Collection/Detail pushes by
/// `bridge::nav_push_with_return`: a test that presses OK on a poster sees the page the TV opens.
/// It also drains the item menu's committed rows (`content::drain_item_menu_requests`, the Watchlist
/// disc's Add/Remove among them), performs the Watchlist page's own requests
/// (`content::watchlist_request`: the Sort & Filter menu and the value it chooses) and a Home shelf
/// card's Detail (`perform_home_detail`). The BACK fold (`content::fold_to_hero`) needs none of this
/// state, so [`pump_navigation`] performs it too; Home's other requests are drained and left undone.
pub(super) struct Pump {
    ps: plx_media::route::PlaybackSession,
    pa: plx_media::player::adapter::PlayerAdapter,
    menu_play_await: Option<crate::app::input::MenuPlayAwait>,
    ok_armed: bool,
}

impl Pump {
    pub(super) fn new() -> Pump {
        // SAFETY: the test thread is the main thread of its own world; `port.rs` installs `NoSink`
        // under `cfg(test)`, so no player verb is reached.
        let mt = unsafe { plx_base::task::MainThread::assume() };
        Pump { ps: Default::default(), pa: plx_media::player::adapter::PlayerAdapter::new(mt),
            menu_play_await: None, ok_armed: false }
    }

    /// One frame's requests, `now` being the frame clock in ms.
    pub(super) fn run(&mut self, d: &mut Dispatcher<AppHost>, rig: &mut Bridge, now: u32) -> Vec<String> {
        pump_requests(d, rig, Some((self, now)))
    }
}

fn pump_requests(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, mut item: Option<(&mut Pump, u32)>) -> Vec<String> {
    use crate::app::content::{content_back, library_navigation, library_page_request, library_publish_shelves, watchlist_navigation, watchlist_request};
    use plx_screens::registry::ContentReq;
    use plx_screens::watchlist::page::WatchlistOut;
    // The entry that asked, while it is still the one holding input.
    fn asker(d: &Dispatcher<AppHost>, source: MachineId) -> Option<EntryId> {
        let MachineId::Instance(instance) = source else { return None };
        let entry = d.nav.entry_of_instance(instance)?;
        (d.nav.input_owner() == Some(InputOwner::Entry(entry))).then_some(entry)
    }
    let mut seen = Vec::new();
    // The item menu's committed row, and the hero disc's Add/Remove, are requests the loop drains
    // right after the dispatcher and before the page requests (`content::drain_item_menu_requests`).
    if let Some((pump, now)) = item.as_mut() {
        crate::app::content::drain_item_menu_requests(&mut pump.ps, &mut pump.pa, d, rig,
            &mut crate::app::playback::LivePlaybackResources, &mut pump.menu_play_await, *now);
    }
    for (source, request, ret) in rig.take_library_reqs() {
        let (Some(entry), MachineId::Instance(instance)) = (asker(d, source), source) else { continue };
        if !d.top_arg().is_some_and(|arg| arg.is_library_family()) { continue; }
        // The publication is the store's business and the page re-asks every frame until it lands,
        // so it is performed as the loop does and never listed among what a key caused.
        if let plx_screens::registry::LibraryReq::PublishShelves { target, hidden_page, at_head } = request {
            if item.is_some() { library_publish_shelves(d, rig, entry, target, hidden_page, at_head); }
            continue;
        }
        let label = format!("{request:?}");
        let Some(ret) = library_navigation(d, rig, &request, ret) else { seen.push(format!("library:{label}")); continue };
        match (&request, item.as_mut()) {
            (_, None) => seen.push(format!("unperformed library:{label}")),
            (_, Some((pump, now))) => {
                // SAFETY: the player seam is this pump's own `NoSink` session.
                unsafe {
                    library_page_request(&mut pump.ps, &mut pump.pa, &mut pump.menu_play_await, &mut pump.ok_armed,
                        *now, d, rig, instance, entry, request, ret);
                }
                seen.push(format!("library:{label}"));
            }
        }
    }
    for (source, request, ret) in rig.take_content_reqs() {
        let Some(entry) = asker(d, source) else { continue };
        match request {
            ContentReq::Back => { content_back(d, source, entry, ret); seen.push("content:Back".into()); }
            ContentReq::Push(arg) if item.is_some() => {
                nav_push_with_return(d, AppArg::Content(arg), ret);
                seen.push("content:Push".into());
            }
            _ => seen.push("unperformed content:request".into()),
        }
    }
    // Home's requests (`content::home_requests`), under the loop's guards: the asking page holds input and Home is the
    // route. Two are performed: the BACK fold, the one request of Home's that moves focus rather than navigating (the loop
    // runs `content::fold_to_hero`, so the world does, or a BACK on a shelf would fold the picture and leave the engine's
    // focus where it was), which needs none of the Pump's resources and so is performed for `pump_navigation` as well, and a
    // card's Detail (`perform_home_detail`, the Pump only). The rest are drained and left undone, as they were before this
    // arm (nothing read them), so a test that presses OK on the hero or a pill sees what it always saw.
    for (source, request, ret) in rig.take_home_reqs() {
        let Some(entry) = asker(d, source) else { continue };
        if d.top_arg() != Some(&AppArg::Home) { continue; }
        match (request, item.as_mut()) {
            (plx_screens::registry::HomeReq::FoldToHero, _) => {
                crate::app::content::fold_to_hero(d, source, entry);
                seen.push("home:FoldToHero".into());
            }
            (plx_screens::registry::HomeReq::Detail { sid, rk }, Some((pump, now))) => {
                perform_home_detail(pump, *now, d, rig, sid, &rk, ret);
                seen.push("home:Detail".into());
            }
            _ => {}
        }
    }
    for (source, request, ret) in rig.take_watchlist_reqs() {
        let MachineId::Instance(instance) = source else { continue };
        let Some(entry) = d.nav.entry_of_instance(instance) else { continue };
        // The Sort & Filter value comes from the surface over the page: the loop gates it on
        // nothing but a Watchlist page being on the stack, so the page need not hold input.
        if !matches!(request, WatchlistOut::SetOptions(_))
            && (asker(d, source).is_none() || d.top_arg() != Some(&AppArg::Watchlist))
        {
            continue;
        }
        let label = format!("{request:?}");
        seen.push(match (watchlist_navigation(d, rig, &request, ret), item.as_mut()) {
            (None, _) => format!("watchlist:{label}"),
            (Some(ret), Some((pump, _))) => {
                watchlist_request(d, rig, entry, request, ret, &mut pump.ok_armed);
                format!("watchlist:{label}")
            }
            (Some(_), None) => format!("unperformed watchlist:{label}"),
        });
    }
    // The requests above include an item menu's Play, whose `describe_movie` emits a store job: the
    // drain after it must have started it, as the loop's own `update` pump is not run here.
    rig.assert_no_store_jobs_left_for_test();
    seen
}

/// **A Home card's Detail request, performed with the loop's own opener.** `content::home_requests` hands
/// `HomeReq::Detail` to `activate_home_item`, which finds the card in the published hubs and opens it with
/// `input::activate_deck_card` and the page's `ReturnState`. This finds the card the same way and calls that
/// same `activate_deck_card`, so `activate_card`'s and `open_detail`'s handling of the return state is the
/// loop's. `from_deck` is false: a World's Home has no Continue Watching deck to press. It does NOT run
/// `activate_home_item` itself, which takes the whole `App` this rig does not build, so a loop that dropped
/// `ret` between `home_requests` and `activate_deck_card` is not caught by a test that uses this.
fn perform_home_detail(pump: &mut Pump, now: u32, d: &mut Dispatcher<AppHost>, rig: &mut Bridge,
    sid: plx_plex::plex::ServerId, rk: &str, ret: plx_ui::screen::ReturnState<u32, plx_screens::registry::PageMemory>) {
    let item = {
        let snapshot = rig.hubs_snapshot();
        let view = snapshot.view();
        (0..view.hub_count()).find_map(|i| view.hub(i)?.items.iter()
            .find(|item| plx_plex::plex::same_item((item.sid, &item.rk), (sid, rk))).map(|item| (**item).clone()))
    }.expect("the card the request names is in the published hubs");
    // SAFETY: the player seam is this pump's own `NoSink` session (`Pump::new`).
    unsafe {
        crate::app::input::activate_deck_card(&mut pump.ps, &mut pump.pa, &item, false, false,
            plx_screens::player::input::HUD_LINGER_MS, Some(ret), d, rig, &mut pump.menu_play_await, now);
    }
}

/// The section table's index of "Film Club" (a Movie library of the second source): the library the
/// world pins beside "Movies" (index 0) when it is asked for two.
pub(super) const FILM_CLUB: usize = 2;

/// **The route-test world, the ONE key-driven harness of the app tier**: a real `Dispatcher<AppHost>`
/// and `Bridge` over two registered servers, 120 films in the store and six shelves of three
/// cards, with the loop's request [`Pump`] and a `press` that plays one key out the way a person's
/// press does (both edges, then the frames a reseat, a press dip and the navigation it commits to
/// take). It owns the serial lock, the scratch session and the server registry's reset, so a route
/// test is its rows and nothing else; a test holds ONE world at a time (the lock is not re-entrant).
///
/// [`Self::new`] has no page on top: a row mounts one the way the app does (`nav_tab` from Home,
/// then a press on the page's own control), so what it asserts is what a key did on the way there.
/// [`Self::movies_library`], [`Self::collections`], [`Self::recommended`] and [`Self::watchlist`]
/// are those mounts, settled.
///
/// Libraries are pinned the way the Home editor pins them: ONE recorded answer for the profile
/// (`BrowseCmd::ApplyPins`, then `refresh_browse_directory`). The table-only `set_pinned_for_test` is
/// overwritten by the next roster sync (it re-resolves every row from the profile's record, on the
/// discovery pump's wall-clock schedule, 60-90 frames in), which moved the store back to Movies
/// mid-test and made the header name one library again.
pub(super) struct World {
    pub(super) d: Dispatcher<AppHost>,
    pub(super) bridge: Bridge,
    pub(super) frame: u32,
    pump: Pump,
    performed: Vec<String>,
    /// Landing what a page's store brought (the Watchlist's scripted plex.tv), once per frame.
    feed: Option<fn(&mut Dispatcher<AppHost>, &mut Bridge)>,
    /// The registered server the library's first section belongs to (`UNSET` in the Watchlist's world).
    pub(super) own: plx_plex::plex::ServerId,
    /// The scripted plex.tv a Watchlist world runs over (the writes it was sent are the tests' to read).
    script: Option<std::sync::Arc<super::watchlist_press_tests::Script>>,
    _session: plx_plex::plex::session::TempSession,
    _serial: plx_base::testlock::Serial,
}

impl Drop for World {
    fn drop(&mut self) {
        plx_plex::plex::reset_servers_for_test();
        crate::app::collections_probe::clear_seeds_for_test();
    }
}

/// How many collections the rig's probe stands in for on every section it seeds (a server the rig
/// has no network to ask): enough that the Library header's Collections control is on the row.
pub(crate) const RIG_COLLECTIONS: u32 = 4;

impl Bridge {
    /// Seed the Collections probe's `answer` for every section of the directory captured now (the
    /// probe never reaches a server under test, `collections_probe::seed_for_test`), replacing the
    /// rig's earlier seeds: `Count(n)` puts the control on the Library header, `Count(0)` or
    /// `Forbidden` takes it off, `Unknown` is a probe that has not answered.
    pub(crate) fn seed_collections_for_test(&self, answer: plx_plex::plex::collections::CollectionsCount) {
        for section in self.directory.view().sections() {
            if let Some(sid) = section.sid { crate::app::collections_probe::seed_for_test(sid, section.key, answer); }
        }
    }
}

impl World {
    fn lock(name: &str) -> (plx_base::testlock::Serial, plx_plex::plex::session::TempSession) {
        let serial = plx_base::testlock::serial();
        let session = plx_plex::plex::session::TempSession::new(name);
        session.watching(&format!("u-{name}"));
        (serial, session)
    }

    /// Nothing mounted; `name` names the scratch session, each test passes its own. One Movie
    /// library per source, so the Movies header names one library.
    pub(super) fn new(name: &str) -> World { Self::build(name, false) }

    fn build(name: &str, two_libraries: bool) -> World { Self::build_listing(name, two_libraries, true) }

    /// [`Self::build`], the library's films answered (`answered`) or its listing held LOADING
    /// (`BrowseStore::hold_listing_for_test`): a test server is a closed port whose fetch fails at once,
    /// so a listing that is still on its way needs the hold.
    fn build_listing(name: &str, two_libraries: bool, answered: bool) -> World {
        Self::build_shelves(name, two_libraries, answered, true)
    }

    /// [`Self::build_listing`], the section's hubs seeded as six committed shelves (`shelved`) or
    /// left as a section nothing has published for yet: the Recommended page's FIRST publication.
    fn build_shelves(name: &str, two_libraries: bool, answered: bool, shelved: bool) -> World {
        let (serial, session) = Self::lock(name);
        plx_plex::plex::reset_servers_for_test();
        crate::app::collections_probe::clear_seeds_for_test();
        let own = plx_plex::plex::register_for_test("route-own", "127.0.0.1", 9, "synthetic", "fixture");
        let shared = plx_plex::plex::register_for_test("route-shared", "127.0.0.1", 10, "synthetic", "fixture");
        plx_plex::plex::set_current(own);
        let d = Dispatcher::<AppHost>::new();
        let mut bridge = Bridge::for_test(|| 0);
        bridge.stores.browse.borrow_mut().seed_registered_table_for_test([own, shared]);
        // "Film Club" beside "Movies": the header then names two libraries (one library draws no
        // names, no plate and no scroll capsule).
        if two_libraries {
            bridge.browse_run(plx_data::stores::browse::BrowseCmd::ApplyPins(vec![(FILM_CLUB, true)]));
        }
        bridge.refresh_browse_directory();
        bridge.seed_collections_for_test(plx_plex::plex::collections::CollectionsCount::Count(RIG_COLLECTIONS));
        bridge.browse_run(plx_data::stores::browse::BrowseCmd::SetCur(0));
        {
            let mut browse = bridge.stores.browse.borrow_mut();
            if answered { browse.seed_items_for_test(120); } else { browse.hold_listing_for_test(); }
            if shelved {
                browse.seed_shelves_for_test(0, &["One", "Two", "Three", "Four", "Five", "Six"], 3);
                browse.seed_landscape_for_test(0, "Synthetic show");
            }
        }
        World { d, bridge, frame: 0, pump: Pump::new(), performed: Vec::new(), feed: None, own, script: None,
            _session: session, _serial: serial }
    }

    /// The Movies library, reached by its strip pill from Home, settled. `two_libraries` pins
    /// "Film Club" beside "Movies".
    pub(super) fn movies_library(name: &str, two_libraries: bool) -> World {
        let mut w = Self::build(name, two_libraries);
        w.open_library();
        w
    }

    /// The Movies library whose sections' Collections probe answered `answer` (a refusal, none, or a
    /// count other than the rig's), settled: the header the rig's server would give a library with
    /// that answer.
    pub(super) fn movies_library_answering(name: &str, answer: plx_plex::plex::collections::CollectionsCount) -> World {
        let mut w = Self::build(name, false);
        w.bridge.seed_collections_for_test(answer);
        w.open_library();
        w
    }

    /// The Movies library showing its section's EPISODES listing (the landscape stills), settled.
    pub(super) fn episodes_library(name: &str) -> World {
        let mut w = Self::movies_library(name, false);
        w.bridge.stores.browse.borrow_mut().seed_episodes_for_test(60);
        w.frames(60);
        w
    }

    /// The Movies library whose section has published no shelves yet (its listing answered): the
    /// Recommended page opened from it has a first publication still to come.
    pub(super) fn movies_library_unshelved(name: &str) -> World {
        let mut w = Self::build_shelves(name, false, true, false);
        w.open_library();
        w
    }

    /// The Movies library opened while its listing is still LOADING (held, never answered): the page
    /// that has no tile to seat yet. `two_libraries` pins "Film Club" beside "Movies".
    pub(super) fn movies_library_loading(name: &str, two_libraries: bool) -> World {
        let mut w = Self::build_listing(name, two_libraries, false);
        w.open_library();
        w
    }

    /// The Collections page, opened with OK on the Library header's Collections control, its own
    /// listing held LOADING (the Library under it is answered, and the rig's probe is seeded with a count, so the control is there to press).
    pub(super) fn collections_loading(name: &str) -> World {
        let mut w = Self::movies_library(name, false);
        w.bridge.stores.browse.borrow_mut().hold_listing_for_test();
        w.open_from_header(8, AppArg::LibraryCollections { section: 0 });
        w
    }

    /// The Collections page, opened with OK on the Library header's Collections control.
    pub(super) fn collections(name: &str, two_libraries: bool) -> World {
        let mut w = Self::movies_library(name, two_libraries);
        w.open_from_header(8, AppArg::LibraryCollections { section: 0 });
        // The page asked the store for its own listing; its answer is what it shows.
        w.bridge.stores.browse.borrow_mut().seed_items_for_test(120);
        w.frames(60);
        w
    }

    /// The Recommended page opened from a section nothing has published shelves for yet: its FIRST
    /// publication is still to come (`land_shelves`), so the page has no tile to seat.
    pub(super) fn recommended_pending(name: &str) -> World {
        let mut w = Self::movies_library_unshelved(name);
        w.open_from_header(9, AppArg::LibraryRecommended { section: 0 });
        w
    }

    /// The first hubs of the section the Recommended page waits for: the six shelves and the
    /// landscape hero `build_shelves` seeds for a shelved world.
    pub(super) fn land_shelves(&mut self) {
        let mut browse = self.bridge.stores.browse.borrow_mut();
        browse.seed_shelves_for_test(0, &["One", "Two", "Three", "Four", "Five", "Six"], 3);
        browse.seed_landscape_for_test(0, "Synthetic show");
    }

    /// The Recommended page, opened with OK on the Library header's Recommended control.
    pub(super) fn recommended(name: &str) -> World {
        let mut w = Self::movies_library(name, false);
        w.open_from_header(9, AppArg::LibraryRecommended { section: 0 });
        w.frames(60);
        w
    }

    /// The Watchlist over a scripted plex.tv holding forty titles, opened by its strip pill.
    pub(super) fn watchlist(name: &str) -> World {
        Self::watchlist_over(name, super::watchlist_press_tests::Script::new(&(1..=40).collect::<Vec<u8>>()))
    }

    /// The Watchlist over `script`, opened by its strip pill.
    pub(super) fn watchlist_over(name: &str, script: std::sync::Arc<super::watchlist_press_tests::Script>) -> World {
        Self::watchlist_built(name, script, false)
    }

    /// The Watchlist opened while its first read is still ON ITS WAY (`hold_reads`), the page that
    /// has no tile to seat yet; [`Self::land_watchlist`] lets the read through.
    pub(super) fn watchlist_loading(name: &str) -> World {
        Self::watchlist_built(name, super::watchlist_press_tests::Script::new(&(1..=40).collect::<Vec<u8>>()), true)
    }

    /// The held read of [`Self::watchlist_loading`] answers.
    pub(super) fn land_watchlist(&mut self) {
        super::watchlist_press_tests::release_reads(&mut self.bridge);
    }

    fn watchlist_built(name: &str, script: std::sync::Arc<super::watchlist_press_tests::Script>, held: bool) -> World {
        let (serial, session) = Self::lock(name);
        let (d, mut bridge) = super::watchlist_press_tests::rig_over(&script);
        if held { super::watchlist_press_tests::hold_reads(&mut bridge); }
        let mut w = World { d, bridge, frame: 1, pump: Pump::new(), performed: Vec::new(), own: plx_plex::plex::ServerId::UNSET,
            script: Some(script), _session: session, _serial: serial, feed: Some(|d, bridge| {
                bridge.watchlist_for_test().pump();
                bridge.watchlist_feed(d);
            }) };
        super::nav_tab(&mut w.d, &mut w.bridge, HomeTab::Watchlist, None, None);
        w.frames(120);
        assert!(w.on(&AppArg::Watchlist), "premise: the Watchlist is on top");
        w
    }

    /// Home over a scripted plex.tv (forty titles, none of them `hero`), its one hero standing for
    /// the movie `hero` (a catalog id no list holds, so the disc reads "Add" once the list has landed),
    /// settled. The Home and Detail discs are fed as the loop feeds them (`catalog_feed`).
    pub(super) fn home_with_disc(name: &str, hero: [u8; 12]) -> World {
        let mut w = Self::scripted(name);
        w.bridge.stores.hubs.seed_for_test(1, plx_data::pms::HubState::Ready);
        plx_data::pms::edit_test_item(w.bridge.stores.hubs.state_mut(), "1", &|m| { m.kind = 0; m.guid = Some(hero); });
        w.mount_home()
    }

    /// Home over `rows` shelves of `cols` cards each (`seed_grid_for_test`), settled, focus on the hero. The
    /// world is the Home-disc one (a scripted plex.tv), the one Home mount the rig has with no library registry.
    pub(super) fn home_grid(name: &str, rows: usize, cols: usize) -> World {
        let mut w = Self::scripted(name);
        w.bridge.stores.hubs.seed_grid_for_test(rows, cols);
        w.mount_home()
    }

    /// Home shown over the seeded hubs and settled.
    fn mount_home(mut self) -> World {
        super::show_page(&mut self.d, AppArg::Home);
        self.frames(60);
        assert!(self.on(&AppArg::Home), "premise: Home is on top");
        self
    }

    /// A movie's Detail page (record `rk` carrying the guid of the catalog id `hex`) pushed over Home,
    /// over the same scripted plex.tv, settled.
    pub(super) fn detail_with_disc(name: &str, hex: [u8; 12]) -> World {
        Self::detail_page(name, plx_data::metadata::Detail {
            sid: plx_plex::plex::ServerId::UNSET,
            rk: "movie".into(),
            kind: "movie".into(),
            guid: format!("plex://movie/{}", plx_data::watchlist::encode_hex(&hex)),
            ..Default::default()
        })
    }

    /// The Detail page of record `movie` holding `detail` (its `related` shelf, its collection), pushed
    /// over Home over the same scripted plex.tv, settled.
    pub(super) fn detail_page(name: &str, detail: plx_data::metadata::Detail) -> World {
        let mut w = Self::scripted(name);
        super::show_page(&mut w.d, AppArg::Home);
        w.frames(20);
        // Installed BEFORE the mount: the page keys its element identities from the item it finds current.
        plx_data::metadata::set_current_for_test(w.bridge.stores.metadata.state_mut(), Some(detail));
        super::nav_push_with_return(&mut w.d, detail_arg("movie"), Default::default());
        w.frames(60);
        assert!(w.on(&detail_arg("movie")), "premise: the Detail page is on top");
        w
    }

    /// The Search page with `shelves` (of the world's own server) published as its results, settled.
    /// The query is set the way a typed one is, so the page shows results rather than recents.
    pub(super) fn search_results(name: &str, shelves: impl FnOnce(plx_plex::plex::ServerId) -> Vec<plx_data::search::Shelf>) -> World {
        let mut w = Self::new(name);
        super::show_page(&mut w.d, AppArg::Search);
        w.bridge.search_run(plx_data::stores::search::SearchCmd::SetQuery("synthetic".into()));
        let shelves = shelves(w.own);
        w.bridge.stores.search.publish_shelves_over_directory_for_test(shelves, w.bridge.directory.view());
        w.frames(60);
        assert!(w.on(&AppArg::Search), "premise: Search is on top");
        w
    }

    /// The person page of `actor-a` whose filmography is `movies` and `shows`, `movie_roles` being the
    /// characters the roles batch named the movie tiles' credits as, settled.
    pub(super) fn person_page(name: &str, movies: Vec<plx_data::pms::PmsMovie>, shows: Vec<plx_data::pms::PmsMovie>, movie_roles: &[&str]) -> World {
        let mut w = Self::new(name);
        super::show_page(&mut w.d, person_arg("actor-a"));
        w.frames(30);
        w.bridge.stores.person.install_for_test(movies, shows);
        w.bridge.stores.person.install_roles_for_test(0, movie_roles.iter().map(|r| (*r).to_owned()).collect());
        w.frames(60);
        w
    }

    /// Home over a scripted plex.tv of forty titles, the disc feeds included.
    fn scripted(name: &str) -> World {
        let (serial, session) = Self::lock(name);
        let script = super::watchlist_press_tests::Script::new(&(1..=40).collect::<Vec<u8>>());
        let (d, bridge) = super::watchlist_press_tests::rig_over(&script);
        World { d, bridge, frame: 1, pump: Pump::new(), performed: Vec::new(), own: plx_plex::plex::ServerId::UNSET,
            script: Some(script), _session: session, _serial: serial, feed: Some(|d, bridge| {
                bridge.watchlist_for_test().pump();
                bridge.watchlist_feed(d);
                bridge.catalog_feed(d);
            }) }
    }

    /// The writes the scripted plex.tv was sent, in order (`(catalog id, Add | Remove)`).
    pub(super) fn writes(&self) -> Vec<([u8; 12], plx_data::watchlist::ledger::Intent)> {
        self.script.as_ref().expect("a world over a scripted plex.tv").writes()
    }

    /// One frame with `inputs`, then the requests it raised, performed. What the pump did is kept
    /// for [`Self::press`] to return.
    pub(super) fn step(&mut self, inputs: Vec<InputEvent<u32>>) {
        self.frame += 1;
        if let Some(feed) = self.feed { feed(&mut self.d, &mut self.bridge); }
        plx_machine::idle::frame_begin(0.016);
        super::frame(&mut self.d, &mut self.bridge, tick(self.frame), inputs);
        let now = self.frame * 16;
        self.performed.extend(self.pump.run(&mut self.d, &mut self.bridge, now));
    }

    pub(super) fn frames(&mut self, n: u32) {
        for _ in 0..n { self.step(vec![]); }
    }

    /// `n` idle frames; returns what the pump performed meanwhile (a navigation a press began and
    /// the loop finishes a few frames on).
    pub(super) fn settle(&mut self, n: u32) -> Vec<String> {
        self.performed.clear();
        self.frames(n);
        std::mem::take(&mut self.performed)
    }

    /// One key, both edges, then only `after` frames: a held remote's cadence, nothing settled in
    /// between (two `tap(key, 0)` are a double press). Returns what the pump performed meanwhile.
    pub(super) fn tap(&mut self, key: Key, after: u32) -> Vec<String> {
        self.performed.clear();
        self.step(script_key(key, tick(self.frame + 1)));
        self.frames(after);
        std::mem::take(&mut self.performed)
    }

    /// One key, both edges, then enough frames for a reseat, a press dip, a scroll spring and the
    /// navigation it commits to to play out. Returns what the pump performed meanwhile.
    pub(super) fn press(&mut self, key: Key) -> Vec<String> {
        self.tap(key, 28)
    }

    /// **What the top page DREW this frame**, read off a recording painter (a host has no GL): every
    /// text run as `(words, rect)`. The draw is the product's own, `Dispatcher::draw_with_glass_below`
    /// inside a discovery walk, so the words a test reads are the ones a person reads. It draws the
    /// frame the last [`Self::step`] updated and advances nothing. (A lifted header opener is part of
    /// that same draw: `draw_with`'s `Z::OPENER` pass repaints it. Which element it is, is
    /// `Dispatcher::lifted_opener`.)
    pub(super) fn drawn(&mut self) -> Drawn {
        use plx_ui::frame::backdrop;
        let mut glass = plx_ui::frame::glass::GlassPlan::new();
        let sources = glass.sources.clone();
        sources.borrow_mut().begin(self.d.backdrop_layers(1.0));
        let mut words = Vec::new();
        let mut runs = Vec::new();
        let marks = plx_ui::draw_census::capture_play_marks(|| {
            runs = plx_ui::draw_census::capture_runs(|| {
                let _declarations = backdrop::discover(sources.clone());
                words = plx_ui::draw_census::capture_text(|| {
                    self.d.draw_with_glass_below(&mut self.bridge, &mut glass, true, backdrop::Z::ALL);
                });
            });
        });
        sources.borrow_mut().resolve();
        backdrop::commit(&sources);
        Drawn { words, runs, marks }
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
            (Some(AppArg::LibraryCollections { .. }), AppArg::LibraryCollections { .. })
            | (Some(AppArg::LibraryRecommended { .. }), AppArg::LibraryRecommended { .. }) => true,
            (Some(top), want) => top == want,
            (None, _) => false,
        }
    }

    /// The screen that holds focus: the page, or the menu over it.
    pub(super) fn focused_screen(&self) -> Option<&dyn plx_ui::screen::Screen<AppHost>> {
        let entry = self.d.nav.entry(self.d.focus()?.entry)?;
        entry.inst.as_ref().map(|inst| &*inst.screen)
    }

    /// The focused element (`None` while nothing holds focus).
    pub(super) fn focus(&self) -> Option<FocusKey<u32>> { self.d.focus() }

    pub(super) fn elem(&self) -> Option<u32> { self.d.focus().map(|key| key.elem) }

    /// The Movies library, reached by its strip pill from Home, settled.
    pub(super) fn open_library(&mut self) {
        super::show_page(&mut self.d, AppArg::Home);
        self.frames(40);
        super::nav_tab(&mut self.d, &mut self.bridge, HomeTab::Movies, None, None);
        self.frames(90);
        assert!(self.on(&AppArg::Library), "premise: the library is on top");
    }

    /// The top page, as the Library-family screen it is (the grid or the Collections page).
    pub(super) fn library(&self) -> &plx_screens::library::LibraryScreen {
        self.d.top_screen().and_then(|screen| screen.as_any()).and_then(|any| any.downcast_ref())
            .expect("a Library page (the grid or the Collections page) is on top")
    }

    /// Poster `i`'s settled cell on the grid page on top (the Library grid or the Collections page, the
    /// Watchlist, the Collection screen's members), the caption band folded in.
    pub(super) fn poster_cell(&self, i: usize) -> plx_ui::Rect {
        let any = self.d.top_screen().and_then(|screen| screen.as_any()).expect("a page is on top");
        if let Some(page) = any.downcast_ref::<plx_screens::library::LibraryScreen>() { return page.probe_cell(i).0; }
        if let Some(page) = any.downcast_ref::<plx_screens::watchlist::page::WatchlistScreen>() { return page.probe_cell(i).expect("the Watchlist holds a grid"); }
        if let Some(page) = any.downcast_ref::<plx_screens::collection::CollectionScreen>() { return page.probe_cell(i).expect("the Collection screen holds a grid"); }
        panic!("the top page holds no poster grid");
    }

    /// **Every card's pop as the top page's draw paints it**, shelf by shelf (Home or the Recommended
    /// page): `1.0` is a card at EXACT rest, which `ui::widgets::pop_factor` turns into `0.0` and the card
    /// compositor into the plain program and the resting split. Anything above `1.0` on a card the
    /// focus is not on is a card that never came to rest (`Spring::land`).
    pub(super) fn card_pops(&self) -> Vec<Vec<f32>> {
        let any = self.d.top_screen().and_then(|screen| screen.as_any()).expect("a page is on top");
        if let Some(page) = any.downcast_ref::<plx_screens::home::HomeScreen>() { return page.probe_card_pops(self.d.focus()); }
        if let Some(page) = any.downcast_ref::<plx_screens::library::RecommendedScreen>() { return page.probe_card_pops(self.d.focus()); }
        panic!("the top page is neither Home nor the Recommended page");
    }

    /// **Only the focused card is off exact rest, and it is popped:** every other card's pop is exactly `1.0`
    /// (so `ui::widgets::pop_factor` is `0.0` and it draws with the plain card program and the resting split).
    /// Returns the focused card's `(shelf, card)`; `what` names the page in the failure message.
    pub(super) fn sole_card_off_rest(&self, what: &str) -> (usize, usize) {
        let off: Vec<(usize, usize, f32)> = self.card_pops().iter().enumerate()
            .flat_map(|(shelf, cards)| cards.iter().enumerate().filter(|(_, pop)| **pop != 1.0).map(move |(card, pop)| (shelf, card, *pop)))
            .collect();
        assert_eq!(off.len(), 1, "{what}: only the focused card is off rest, got {off:?}");
        let (shelf, card, pop) = off[0];
        assert!(pop > 1.0, "{what}: the focused card is popped: {off:?}");
        (shelf, card)
    }

    /// The server and ratingKey of the Home card holding focus (`None` while focus is elsewhere).
    pub(super) fn home_focused_item(&self) -> Option<(plx_plex::plex::ServerId, String)> {
        self.bridge.with_home(&self.d, |home, cx, focus| home.focused_item::<super::AppHost>(focus, cx)
            .map(|item| (item.sid, item.rk.clone()))).expect("Home is the top page")
    }

    /// The Home card holding focus: its `(row, col)` in the grid and its ratingKey (`None` while focus is elsewhere).
    pub(super) fn home_card(&self) -> (Option<(usize, usize)>, Option<String>) {
        self.bridge.with_home(&self.d, |home, cx, focus| (home.grid_position::<super::AppHost>(focus, cx),
            home.focused_item::<super::AppHost>(focus, cx).map(|item| item.rk.clone()))).expect("Home is the top page")
    }

    /// The scroll the page on top is travelling to (0 at its top).
    pub(super) fn scroll_target(&self) -> f32 {
        let any = self.d.top_screen().and_then(|screen| screen.as_any()).expect("a page is on top");
        if let Some(page) = any.downcast_ref::<plx_screens::library::LibraryScreen>() { return page.probe_scroll_target(); }
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

    /// Up from where focus is until it stands on one of the Library header's stops (at most three
    /// presses: the page arrives in its first poster, one Up from the header; a scrolled page
    /// takes more). A no-op when focus is already there.
    pub(super) fn climb_into_header(&mut self) {
        for _ in 0..3 {
            if self.on_header() { return; }
            self.press(Key::Up);
        }
    }

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
        if let Some(page) = any.downcast_ref::<plx_screens::library::LibraryScreen>() { return page.probe_header_builds(); }
        if let Some(page) = any.downcast_ref::<plx_screens::library::RecommendedScreen>() { return page.probe_header_builds(); }
        if let Some(page) = any.downcast_ref::<plx_screens::watchlist::page::WatchlistScreen>() { return page.probe_header_builds(); }
        panic!("the top page wears no header row");
    }

    /// Walk along the Library header (Right, or Left when the stop lies behind focus) to the stop
    /// `elem` (an element id) and stop there. The page arrives in its first poster, so a walk that
    /// starts below the header climbs into it with Up first.
    pub(super) fn walk_header_to(&mut self, elem: u32) {
        self.climb_into_header();
        let stops = self.library().probe_header_stops();
        let place = |stop: Option<u32>| stops.iter().position(|(_, e)| Some(*e) == stop);
        for _ in 0..stops.len() + 1 {
            if self.elem() == Some(elem) { return; }
            let behind = place(self.elem()).zip(place(Some(elem))).is_some_and(|(at, want)| want < at);
            self.press(if behind { Key::Left } else { Key::Right });
        }
        assert_eq!(self.elem(), Some(elem), "premise: the arrows along the header reached the stop");
    }
}

/// One frame's draw, as [`World::drawn`] reads it: the text, since a host paints no pixels.
pub(super) struct Drawn {
    pub(super) words: Vec<(String, plx_ui::Rect)>,
    /// The same runs with their style: `ink[3]` is the alpha the page drew each at (the cascade folded in).
    pub(super) runs: Vec<plx_ui::draw_census::Run>,
    /// Every amber play mark the frame drew (a host paints no icon: the census notes each one), as
    /// the rect it stands in.
    pub(super) marks: Vec<plx_ui::Rect>,
}

impl Drawn {
    /// **The lines of the focused tile's label block this frame drew**, top to bottom, with no list of
    /// words to look for: every run `card_row::draw_label_block` drew, whatever its ink
    /// (`plx_ui::draw_census::label_lines`), so a line the setting turned off, or one nothing asked
    /// for, is not there, and one that should not be (a stray UNSHADOWED legacy block under Nothing
    /// included) fails the test that compares this list. A line's shadow and a gliding line's
    /// follower are the same line; two lines that read alike stay two.
    pub(super) fn caption_lines(&self) -> Vec<String> { plx_ui::draw_census::label_lines(&self.runs) }
}

pub(super) use super::super::words::every_route;

pub(super) fn notices(d: &Dispatcher<AppHost>) -> String {
    let mut s = String::new();
    if let Some(sc) = d.top_screen() {
        sc.state().probe(&mut s);
    }
    s
}

pub(super) fn directory_policy_fixture(
    own: plx_plex::plex::ServerId,
    hidden: plx_plex::plex::ServerId,
) -> plx_data::stores::browse::DirectorySnapshot {
    let section = |sid, key, section, title: &str, pinned| {
        plx_data::stores::browse::SectionView {
            sid: Some(sid),
            key,
            kind: plx_data::stores::browse::SecKind::Movie,
            row: plx_data::stores::browse::SrcRow {
                section,
                title: title.into(),
                pinned,
                current: section == 0,
                ..Default::default()
            },
        }
    };
    plx_data::stores::browse::DirectorySnapshot::fixture(41, 0, vec![
        section(own, 1, 0, "Retained Movies", true),
        section(hidden, 2, 1, "Hidden Movies", false),
    ])
}

pub(super) struct DirectoryPolicyCleanup;

impl Drop for DirectoryPolicyCleanup {
    fn drop(&mut self) {
        // Search and Hubs are now owned per-Bridge (`rig`, dropped with the test's own stack
        // frame), so there is no process-wide store state left for this cleanup to reset.
        plx_plex::plex::reset_servers_for_test();
    }
}

/// A test frame is a presenting frame: the product drains the Tracks/More text prewarm on the
/// presenting side of the present decision (`app::run::prepare_window`), after the frame's update.
fn presented<R>(frame: R) -> R {
    plx_ui::panel_motion::PanelMotion::drain_queued_text_for_test();
    frame
}

/// A one-thread loopback PMS for route tests: `respond` maps each request's path to a status line
/// and a JSON body. Dropping it stops and joins the thread, so a test that never dials still ends.
pub(crate) struct Pms {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub(crate) port: i32,
}

impl Pms {
    pub(crate) fn serve(respond: fn(&str) -> (&'static str, String)) -> Pms {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port() as i32;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                let Ok((mut conn, _)) = plx_base::testnet::accept(&listener) else {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    continue;
                };
                let mut buf = [0u8; 4096];
                let n = conn.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]);
                let path = head.split_whitespace().nth(1).unwrap_or("");
                let (status, body) = respond(path.split('?').next().unwrap_or(""));
                let _ = write!(
                    conn,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
            }
        });
        Pms { stop, thread: Some(thread), port }
    }
}

impl Drop for Pms {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
