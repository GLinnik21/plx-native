//! Shared driver helpers for bridge.rs's split test modules: frame/tick drivers, route
//! argument builders, and the directory-policy fixture used by the ViewState/chrome tests.

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
    presented(super::frame(d, rig, tick, inputs))
}

/// …and the same driver for the two frames that also want the effect tap or a supplied result
/// set. Only the `goto` is the test's: everything after it is production's own frame.
pub(super) fn frame_with_tap(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: AppArg, tick: Tick,
    inputs: Vec<InputEvent<u32>>, tap: &mut dyn plx_ui::dispatch::Tap<AppHost>)
    -> (&'static str, FrameReport) {
    goto(d, route);
    presented(super::frame_with_tap(d, rig, tick, inputs, tap))
}

pub(super) fn frame_with_results(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: AppArg, tick: Tick,
    inputs: Vec<InputEvent<u32>>, take: impl FnOnce() -> AppResults,
    tap: &mut dyn plx_ui::dispatch::Tap<AppHost>) -> (&'static str, FrameReport) {
    goto(d, route);
    presented(super::frame_with_results(d, rig, tick, inputs, take, tap))
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
/// `"content:Back"`, `"watchlist:Back"`), so a table can assert which request a key produced. A
/// request that is not a navigation (a menu, a play) is labelled
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
    use crate::app::content::{content_back, library_navigation, library_page_request, library_publish_shelves, watchlist_navigation};
    use plx_screens::registry::ContentReq;
    // The entry that asked, while it is still the one holding input.
    fn asker(d: &Dispatcher<AppHost>, source: MachineId) -> Option<EntryId> {
        let MachineId::Instance(instance) = source else { return None };
        let entry = d.nav.entry_of_instance(instance)?;
        (d.nav.input_owner() == Some(InputOwner::Entry(entry))).then_some(entry)
    }
    let mut seen = Vec::new();
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
    for (source, request, ret) in rig.take_watchlist_reqs() {
        let Some(_) = asker(d, source) else { continue };
        if d.top_arg() != Some(&AppArg::Watchlist) { continue; }
        let label = format!("{request:?}");
        seen.push(match watchlist_navigation(d, rig, &request, ret) {
            None => format!("watchlist:{label}"),
            Some(_) => format!("unperformed watchlist:{label}"),
        });
    }
    seen
}

/// **A route-test world**: the Dispatcher and the Bridge over two registered servers, 120 films in
/// the store and six shelves of three cards, with the loop's request [`Pump`] and a `press` that
/// plays one key out the way a person's press does (both edges, then the frames a reseat, a press
/// dip and the navigation it commits to take). It owns the serial lock, the scratch session and the
/// server registry's reset, so a route test is its rows and nothing else.
///
/// The world has no page on top: a row mounts one the way the app does (`nav_tab` from Home, then a
/// press on the page's own control), so what it asserts is what a key did on the way there.
pub(super) struct World {
    pub(super) d: Dispatcher<AppHost>,
    pub(super) bridge: Bridge,
    pub(super) frame: u32,
    pump: Pump,
    performed: Vec<String>,
    /// The registered server the library's first section belongs to.
    pub(super) own: plx_plex::plex::ServerId,
    _session: plx_plex::plex::session::TempSession,
    _serial: plx_base::testlock::Serial,
}

impl Drop for World {
    fn drop(&mut self) { plx_plex::plex::reset_servers_for_test(); }
}

impl World {
    /// `name` names the scratch session; each test passes its own.
    pub(super) fn new(name: &str) -> World {
        let serial = plx_base::testlock::serial();
        let session = plx_plex::plex::session::TempSession::new(name);
        session.watching(&format!("u-{name}"));
        plx_plex::plex::reset_servers_for_test();
        let own = plx_plex::plex::register_for_test("route-own", "127.0.0.1", 9, "synthetic", "fixture");
        let shared = plx_plex::plex::register_for_test("route-shared", "127.0.0.1", 10, "synthetic", "fixture");
        plx_plex::plex::set_current(own);
        let d = Dispatcher::<AppHost>::new();
        let mut bridge = Bridge::for_test(|| 0);
        bridge.stores.browse.borrow_mut().seed_registered_table_for_test([own, shared]);
        bridge.refresh_browse_directory();
        bridge.browse_run(plx_data::stores::browse::BrowseCmd::SetCur(0));
        {
            let mut browse = bridge.stores.browse.borrow_mut();
            browse.seed_items_for_test(120);
            browse.seed_shelves_for_test(0, &["One", "Two", "Three", "Four", "Five", "Six"], 3);
            browse.seed_landscape_for_test(0, "Synthetic show");
        }
        World { d, bridge, frame: 0, pump: Pump::new(), performed: Vec::new(), own, _session: session, _serial: serial }
    }

    /// One frame with `inputs`, then the requests it raised, performed. What the pump did is kept
    /// for [`Self::press`] to return.
    pub(super) fn step(&mut self, inputs: Vec<InputEvent<u32>>) {
        self.frame += 1;
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

    /// A key's two edges on consecutive frames and nothing after: two of these are a double press.
    pub(super) fn tap(&mut self, key: Key) {
        self.step(script_key(key, tick(self.frame + 1)));
    }

    /// One key, both edges, then 28 frames; returns what the pump performed meanwhile.
    pub(super) fn press(&mut self, key: Key) -> Vec<String> {
        self.performed.clear();
        self.step(script_key(key, tick(self.frame + 1)));
        self.frames(28);
        std::mem::take(&mut self.performed)
    }

    /// The page on top is `arg`'s kind of page (a library page's `section` is the store's business).
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

    /// The Movies library, reached by its strip pill from Home, settled.
    pub(super) fn open_library(&mut self) {
        super::show_page(&mut self.d, AppArg::Home);
        self.frames(40);
        super::nav_tab(&mut self.d, &mut self.bridge, HomeTab::Movies, None, None);
        self.frames(90);
        assert!(self.on(&AppArg::Library), "premise: the library is on top");
    }

    /// The top page, as the Library-family screen it is.
    pub(super) fn library_page(&self) -> &plx_screens::library::LibraryScreen {
        self.d.top_screen().and_then(|screen| screen.as_any()).and_then(|any| any.downcast_ref())
            .expect("a Library page (the grid or the Collections page) is on top")
    }
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
