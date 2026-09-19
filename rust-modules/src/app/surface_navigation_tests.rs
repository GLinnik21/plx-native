//! Route/chrome identity and the surface architecture: profile/card menus, Settings and
//! player panels, and the heartbeat word each mounted screen reports.

use super::*;
use crate::ui::machine::Chrome;
use crate::ui::screen::ScreenArg;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::{frame, every_route};

#[test]
fn content_instances_compare_item_identity_and_keep_distinct_entries() {
    let a = AppArg::Content(ContentArg::Detail { sid: crate::plex::ServerId::UNSET, rk: "1001".into() });
    let b = AppArg::Content(ContentArg::Detail { sid: crate::plex::ServerId::UNSET, rk: "1002".into() });
    assert_eq!(a.id(), b.id());
    assert!(!a.same_instance(&b));
    assert!(a.same_instance(&a.clone()));
    assert!(!a.same_instance(&AppArg::Home), "a page with an identity is never a bare page");
}

#[test]
fn a_page_arg_wears_the_chrome_its_own_table_says() {
    for r in every_route() {
        let want = if matches!(r, AppArg::Home | AppArg::Library | AppArg::Search) {
            Chrome::TabBar
        } else {
            Chrome::None
        };
        assert_eq!(r.chrome(), want, "{}", super::super::words::route_word(&r));
    }
    assert_eq!(AppArg::Settings(SettingsPage::Root).chrome(), Chrome::None);
    // …and the root payload is a boot address, never an identity: the two Settings arguments
    // below are ONE screen, which is what stops a dev boot target minting a second surface.
    assert!(AppArg::Settings(SettingsPage::Root).same_instance(&AppArg::Settings(SettingsPage::Legal)));
    assert!(!AppArg::Settings(SettingsPage::Root).same_instance(&AppArg::FirstRunConsent(0)));
}

/// **Every route mounts a screen that NAMES the heartbeat word** (§15.2).
///
/// This was an assertion over the retired route-word page's `name()` — a claim about a type
/// nothing mounted, which by phase 10 graded the one screen impl that was never on screen.
/// The intent moves onto the screens that ARE mounted: the mounter is asked for each route's
/// argument and the instance it builds must answer with the word `route_word` gives that
/// route. It is graded against `route_word(r)` and not against `route_word(page_of(r))`
/// because `page_of` is gone with the two routes that made it more than the identity: since
/// phase 10 the profile and card menus are SURFACES, their words are the overlay alphabet's
/// (`overlay_word`), and `every_route()` is nine pages with nothing to fold.
#[test]
fn every_route_mounts_a_screen_that_names_the_heartbeat_word() {
    let _g = crate::testlock::serial();
    for r in every_route() {
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        let (word, _) = frame(&mut d, &mut rig, r.clone(), tick(0), vec![]);
        assert_eq!(
            word,
            super::super::words::route_word(&r),
            "{} mounted {:?}",
            super::super::words::route_word(&r),
            d.top_screen().map(|s| s.name())
        );
    }
}

#[test]
fn a_store_command_through_the_dispatcher_steps_the_store_and_notifies_the_page() {
    let _g = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    let _ = rig.stores.take_notices();
    // **This used to also assert the retired route-word page's generic notice COUNTER, and
    // cannot any more.** Search left that population with the phase 7 cutover
    // and `Player` with phase 9, and `Account`/`ItemMenu` never had a page of their own —
    // `page_of` maps each to its host, which is an owned screen. So what this grades is the
    // half that is still observable here: a store command emitted as an `Fx::App` is applied
    // in the DRAIN, exactly once, and a direct `apply` is not applied a second time by it.
    // The delivery of the resulting `StoreChanged` to a page is graded where the page can be
    // seen — `ui::dispatch`'s own fixture pages, and each owned screen's store arm.
    let route = AppArg::Player;
    frame(&mut d, &mut rig, route.clone(), tick(0), vec![]);
    assert_eq!(d.top_screen().map(|s| s.name()), Some("player"));
    let before = rig.stores.gen(StoreId::Search);
    d.emit(
        MachineId::Nav,
        Fx::App(AppFx::Store(StoreId::Search, StoreCmd::Search(crate::stores::search::SearchCmd::Reset))),
    );
    frame(&mut d, &mut rig, route.clone(), tick(1), vec![]);
    assert_eq!(rig.stores.gen(StoreId::Search), before + 1, "the store was stepped in the drain");
    frame(&mut d, &mut rig, route.clone(), tick(2), vec![]);
    assert_eq!(rig.stores.gen(StoreId::Search), before + 1, "and exactly once");
    let g = rig.stores.gen(StoreId::Search);
    d.emit(
        MachineId::Nav,
        Fx::Deliver(
            MachineId::Store(StoreId::Browse.ord()),
            Delivery::Machine(AppMsg::Store(StoreCmd::Search(crate::stores::search::SearchCmd::Reset))),
        ),
    );
    frame(&mut d, &mut rig, route.clone(), tick(4), vec![]);
    assert_eq!(rig.stores.gen(StoreId::Search), g);
}

/// **A profile switch must leave the container holding nothing of the profile before it —
/// including whatever the STACK itself was mid-transition on, which a bare `Root` request cannot
/// reach until its own floor.**
///
/// RED FIRST (D1), historically: `Dispatcher::reset_for_profile` had NO production caller
/// anywhere in the tree, and the switch worked only because the loop wrote `route = Profiles`
/// and `sync_page` turned that into a `NavOp::Root(Profiles)` over the OLD, shared `Root` arm —
/// which unwound everything ABOVE the root while leaving the previous root COVERED and alive.
///
/// **That depth-survives-the-switch shape is no longer what this test can grade under an
/// `Immediate` transition.** `Root` now truly replaces the whole stack (`NavStack::apply`'s
/// current `Root` arm, §stack.rs), and `Navigation::commit`'s own orphaned-covered-modal sweep
/// (mod.rs: "a removed page cannot leave an orphaned modal in the live index") already retires a
/// surface standing over a page `Root` just retired — so under `Dispatcher::new()`'s `Immediate`
/// stack, the depth/entry/surface state converges to the same place with `reset_for_profile()`
/// deleted from `switch_profile`, because "now" and "at the op's own commit point" are the same
/// instant. **What `reset_for_profile` still uniquely buys is `NavStack::clear_pending`** (its
/// first line) — the one thing that matters under the product's REAL transition, `PageDip`, where
/// an op does not apply until its floor: a page mid fade-out when the switch fires has already
/// reached the stack's OWN `pending`, not merely the dispatcher's incoming queue, and without the
/// clear it would still apply — some frames later, over the tree the reset just emptied — minting
/// an entry nobody asked for post-switch (`ui/containers/tests.rs`'s
/// `reset_for_profile_clears_a_pending_op_so_it_cannot_apply_over_the_emptied_tree` pins the same
/// mechanism against the bare container). This test now drives a `PageDip` stack rather than the
/// file's usual `Immediate` one for exactly that reason — an `Immediate` stack has no window in
/// which an op is parked but not yet applied for `reset_for_profile` to catch.
#[test]
fn switching_profile_leaves_the_container_holding_nothing_of_the_previous_profile() {
    let _g = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::with_transition(
        Box::new(crate::ui::containers::transition::PageDip::new()));
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    for i in 1..20u32 { super::frame(&mut d, &mut rig, tick(i * 16), vec![]); }
    frame(&mut d, &mut rig, detail_arg("1001"), tick(320), vec![]);
    for i in 1..20u32 { super::frame(&mut d, &mut rig, tick(320 + i * 16), vec![]); }
    assert_eq!(d.nav.tabs.stack.depth(), 2, "the outgoing profile browsed two pages deep");
    let before: Vec<_> = d.nav.tabs.stack.entries.iter().map(|e| e.id).collect();

    // Park a THIRD page on the STACK's own pending, mid fade-out — the window `reset_for_profile`
    // exists to close (see the doc above): the switch fires while this is still in flight.
    nav_push(&mut d, detail_arg("2002"));
    super::frame(&mut d, &mut rig, tick(1000), vec![]);
    assert!(d.nav.tabs.stack.is_pending(), "the push reached the stack's own pending, mid fade-out");

    switch_profile(&mut d);
    assert!(!d.nav.tabs.stack.is_pending(), "the reset drops it rather than letting it apply later");
    assert!(d.nav.tabs.stack.entries.is_empty(), "the reset itself emptied the tree");

    for i in 0..20u32 { super::frame(&mut d, &mut rig, tick(2000 + i * 16), vec![]); }

    assert_eq!(
        d.nav.tabs.stack.depth(), 1,
        "the picker is the only entry: {:?}",
        d.nav.tabs.stack.entries.iter().map(|e| e.arg.id()).collect::<Vec<_>>(),
    );
    assert!(d.nav.top_page().is_some_and(|e| e.arg == AppArg::Profiles));
    for id in before {
        assert!(
            !d.nav.tabs.stack.entries.iter().any(|e| e.id == id),
            "an entry of the previous profile is still on the stack",
        );
    }
    assert!(
        !d.nav.tabs.stack.entries.iter().any(|e| e.arg.same_instance(&detail_arg("2002"))),
        "the parked push did not mint itself in behind the reset",
    );
}

/// **An app switch parks the tree; it does not tear the session's page history down.**
///
/// RED FIRST (D1). The 0x103/0x106 lifecycle used to `suspend()` the tree and then write
/// `route = Home`, which `sync_page` turned into a `NavOp::Root(Home)` — unwinding the player
/// entry AND the detail page it was launched from — and the foreground arm wrote
/// `route = Player`, minting a FRESH entry over a stack that was now just Home. The playback
/// SESSION survived that (it is `player::machine`'s, not the container's) and `App.play_from`
/// survived it (a separate field), which is exactly why nothing caught it: the two pieces of
/// state that would have noticed were both mirrors kept outside the tree.
///
/// With one authority they are not, so the park has to be real. `PlayerScreen.origin` is the
/// entry beneath the player, and an exit is a `PopTo` of it — both of which are meaningless if
/// a background destroys the entry.
///
/// Observed RED before the lifecycle arms stopped writing a route: after foreground the stack
/// was `[home, player]` with two fresh ids, so the origin an exit would pop to was Home
/// rather than the detail page the session was launched from.
#[test]
fn an_app_switch_parks_the_page_stack_and_gives_the_same_entries_back() {
    let _g = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    frame(&mut d, &mut rig, detail_arg("1001"), tick(1), vec![]);
    let origin = d.nav.top_page().map(|e| e.id).expect("the page the session is launched from");
    frame(&mut d, &mut rig, AppArg::Player, tick(2), vec![]);
    let player = d.nav.top_page().map(|e| e.id).expect("the player page");
    assert_eq!(d.nav.tabs.stack.depth(), 3);

    // BACKGROUND (0x103/0x104): the tree is parked. Nothing about the page stack moves.
    background(&mut d);
    frame(&mut d, &mut rig, AppArg::Player, tick(3), vec![]);
    assert!(d.nav.suspended, "the tree knows it is parked");
    assert_eq!(d.nav.tabs.stack.depth(), 3, "a park is not a teardown");

    // FOREGROUND (0x105/0x106).
    foreground(&mut d);
    frame(&mut d, &mut rig, AppArg::Player, tick(4), vec![]);
    assert!(!d.nav.suspended);
    assert_eq!(
        d.nav.top_page().map(|e| e.id), Some(player),
        "the SAME player entry came back, not a fresh one",
    );
    assert_eq!(
        d.nav.tabs.stack.under_top().map(|e| e.id), Some(origin),
        "…standing on the page it was launched from, so an exit still lands there",
    );
}

/// **Every test in this module that calls [`frame`] must hold `testlock::serial()`, even when
/// it asserts nothing about the stores.** `frame`'s second act is
/// `crate::stores::take_notices()`, which DRAINS a process-global dirty flag — so a test that
/// merely walks the nav tree still consumes whatever notice another test was about to observe.
/// This one had no guard until 2026-09-07 and stole
/// `a_store_command_through_the_dispatcher_steps_the_store_and_notifies_the_page`'s notice at
/// roughly one run in five, which surfaced there as `home notices=0` — a failure in the other
/// test, in the other direction, that reads exactly like an ordering bug in the bridge and is
/// not one. Note that `Bridge`'s `Drop` is NOT the answer to this class (see its doc): a leaked
/// counter outlives a guard, whereas a drained notice is a pure interleaving and the lock is
/// precisely what fixes it. **The rule is asserted now, not merely written here** — see
/// [`frame_with_results`], which every frame passes through; it was stated in this comment for
/// a month and broken from `app::mod` anyway, through `every_surface_word`.
#[test]
fn route_flips_preserve_content_and_player_origin_entries() {
    let _g = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    assert_eq!(frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]).0, "home");
    assert_eq!(d.nav.tabs.stack.depth(), 1);
    let home = d.nav.top_page().map(|e| e.id);
    assert_eq!(frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]).0, "home");
    assert_eq!(d.nav.top_page().map(|e| e.id), home, "a steady route mints nothing");
    assert_eq!(frame(&mut d, &mut rig, detail_arg("1001"), tick(2), vec![]).0, "detail");
    assert_eq!(d.nav.tabs.stack.depth(), 2, "Detail preserves its legacy Home origin");
    assert_ne!(d.nav.top_page().map(|e| e.id), home);
    let detail = d.nav.top_page().map(|e| e.id);
    let body = d.top_page();
    assert_eq!(
        frame(&mut d, &mut rig, AppArg::Player, tick(3), vec![]).0,
        "player"
    );
    assert_eq!(d.top_screen().map(|s| s.render()), Some(crate::ui::screen::RenderStrategy::VideoPlane));
    assert_eq!(d.nav.tabs.stack.depth(), 3);
    frame(&mut d, &mut rig, detail_arg("1001"), tick(4), vec![]);
    assert_eq!(d.nav.top_page().map(|e| e.id), detail);
    assert_eq!(d.top_page(), body, "player return uncovers the same Detail instance");
}

/// **A player exit whose origin entry is GONE must still land on the EXISTING Home entry, not a
/// freshly minted one.**
///
/// `playback::return_from_player`'s no-origin/identityless fallback and `bridge::nav_pop_to`'s
/// own stale-entry fallback both used to read `nav_root(Home)` — which was correct back when
/// `Root` meant "unwind to the root, covering rather than retiring it" (§`nav.rs`'s old row), but
/// `NavOp::Root` is now a TRUE replace (`stack.rs`'s Root arm): it retires every entry, Home's own
/// root included, and mints a fresh one — losing whatever focus/scroll memory that Home entry
/// carried, for a fallback whose whole point is "there is nowhere better to go, so stay put on
/// Home". `nav_select_tab` is the fix: it `PopTo`s the root that is already there instead of
/// replacing it. Origin going missing is not exotic — an entry evicted past `NavStack::CAP` or a
/// whole branch torn down (a profile switch, a signed-out reset) both leave a player screen
/// holding an `EntryId` nothing on the stack answers to any more; simulated here directly rather
/// than by actually pushing 16 pages, since the fallback does not care HOW the entry went away.
#[test]
fn player_exit_with_a_gone_origin_returns_to_the_existing_home_entry() {
    let _g = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    let home = d.nav.top_page().map(|e| e.id).expect("Home mounted");
    frame(&mut d, &mut rig, detail_arg("1001"), tick(1), vec![]);
    let x = d.nav.top_page().map(|e| e.id).expect("X (Detail) mounted");
    assert_eq!(d.nav.tabs.stack.depth(), 2, "[Home, X]");

    super::super::playback::enter_player(&mut d, &mut rig, super::super::playback::Origin::Here, None);
    super::frame(&mut d, &mut rig, tick(2), vec![]);
    assert_eq!(d.nav.tabs.stack.depth(), 3, "[Home, X, Player]");
    assert_eq!(
        player(&d).and_then(|p| p.origin).map(|o| o.entry),
        Some(x),
        "the player recorded X as where it returns to"
    );

    // X's entry is gone — evicted past CAP, or its branch torn down — while Player is still up.
    d.nav.tabs.stack.entries.retain(|e| e.id != x);
    assert_eq!(d.nav.tabs.stack.depth(), 2, "[Home, Player] — X is gone");

    super::super::playback::return_from_player(&mut d);
    super::frame(&mut d, &mut rig, tick(3), vec![]);

    assert_eq!(
        d.nav.top_page().map(|e| e.id),
        Some(home),
        "the fallback lands on the EXISTING Home entry, not a re-minted one"
    );
    assert_eq!(d.nav.tabs.stack.depth(), 1, "Player left with nothing standing in for the gone X");
}

/// **Leaving the player while one of its panels is still up** — EOS, the Stop key, or an Info
/// press that navigates — and the page has to follow the route in the SAME frame.
///
/// `playback::exit_player` performs its two acts before the loop's `bridge::frame`: it parks a
/// `NavOp::Dismiss` for every open panel (`dismiss_player_overlays`) and then flips the route
/// to the origin page. Both reach the dispatcher through the one queue, so the frame that
/// carries the new route also carries a parked SURFACE op — which `sync_page`'s guard read as
/// "a page navigation is already in flight" and skipped the page sync for, leaving the tree's
/// top page on `player` under a route that already said `home`.
///
/// Reproduced on the simulator by `tests/player_shots.sh`, whose clip reaches EOS with the Info
/// card open: `assertion failed: the tree's top page names the committed route, left: "player",
/// right: "home"` at `frame_with_results`'s `debug_assert_eq!`.
#[test]
fn leaving_the_player_with_a_panel_up_returns_the_page_in_the_route_s_own_frame() {
    use crate::screens::player::overlay::OverlayKind;
    let _g = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    let home = d.nav.top_page().map(|e| e.id);
    assert_eq!(frame(&mut d, &mut rig, AppArg::Player, tick(1), vec![]).0, "player");
    open_player_overlay(crate::route::idle_session_for_test(), crate::stores::metadata::MetadataStore::default().view(), &mut d, OverlayKind::Info);
    frame(&mut d, &mut rig, AppArg::Player, tick(2), vec![]);
    assert!(player_overlay_up(&d), "the panel is on the player page's own ModalStack");
    // …`exit_player`'s own order: the panels are dismissed, then the route is the origin's.
    dismiss_player_overlays(&mut d);
    assert_eq!(frame(&mut d, &mut rig, AppArg::Home, tick(3), vec![]).0, "home");
    assert_eq!(d.nav.top_page().map(|e| e.id), home, "…and it is the origin page, not a new one");
    assert!(!player_overlay_up(&d), "the panel left with the page that hosted it");
}

/// Phase 5b: the Settings surface is PRESENTED on the tree, owns input from its first frame,
/// names the heartbeat word of its top page, walks its own stack on BACK (root → Legal →
/// back → root) and only then lets the container dismiss it — with the app's page untouched.
#[test]
fn the_settings_surface_owns_input_and_walks_its_own_stack() {
    let _g = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    assert!(d.owns_input(), "Home is an owned page too");
    let home_owner = d.nav.input_owner();
    open_settings(&mut d);
    frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
    assert!(d.owns_input());
    assert_eq!(overlay_word(&d), Some("settings"));
    assert_ne!(d.nav.input_owner(), home_owner, "Settings takes input from its Home host");
    assert!(host_frozen(&d));
    // DOWN, DOWN to Legal notices (Favourites is absent signed out: Privacy, Legal, About)
    let mut t = 2;
    let mut press = |d: &mut Dispatcher<AppHost>, rig: &mut Bridge, key: Key| {
        let ev = script_key(key, tick(t));
        frame(d, rig, AppArg::Home, tick(t), ev);
        t += 1;
        frame(d, rig, AppArg::Home, tick(t), vec![]);
        t += 1;
    };
    press(&mut d, &mut rig, Key::Down);
    press(&mut d, &mut rig, Key::Ok);
    assert_eq!(overlay_word(&d), Some("legal"), "OK on Legal notices pushed the index");
    press(&mut d, &mut rig, Key::Back);
    assert_eq!(overlay_word(&d), Some("settings"), "BACK popped the inner stack");
    assert!(settings_up(&d));
    press(&mut d, &mut rig, Key::Back);
    assert_eq!(
        d.nav.modals.top().map(|s| s.phase),
        Some(Phase::Closing),
        "BACK at the surface's own root dismisses it"
    );
    assert_eq!(d.nav.tabs.stack.depth(), 1, "the app's page never moved");
}

/// **Phase 10: the profile menu is a SURFACE over the page whose chip was pressed.**
///
/// `Route::Account { over: BarHost }` existed to answer three questions the container answers
/// for free, and this test is those three: the page under the panel does not change, the panel
/// owns input, and the heartbeat names the pair as `route=<host> overlay=account` rather than
/// as one route word for three different screens. Driven over all three bar-wearing hosts,
/// because the whole reason the route grew an `over` field was that a press on the Library's
/// chip used to cut the page underneath to Home.
#[test]
fn the_profile_menu_is_a_surface_over_the_page_whose_chip_was_pressed() {
    let _g = crate::testlock::serial();
    for (route, word) in [
        (AppArg::Home, "home"),
        (AppArg::Library, "library"),
        (AppArg::Search, "search"),
    ] {
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, route.clone(), tick(0), vec![]);
        let host = d.nav.top_page().expect("a host page").id;
        let host_owner = d.nav.input_owner();
        open_account_menu(&mut d);
        let (top, _) = frame(&mut d, &mut rig, route.clone(), tick(1), vec![]);

        assert!(account_menu_up(&d), "{word}: the chip's press presented the menu");
        assert_eq!(
            d.nav.top_page().map(|e| e.id),
            Some(host),
            "{word}: a surface is presented OVER the top page and never replaces it"
        );
        assert_eq!(top, word, "{word}: the heartbeat's route= is still the HOST's word");
        assert_eq!(overlay_word(&d), Some("account"));
        assert_ne!(
            d.nav.input_owner(),
            host_owner,
            "{word}: the menu takes input from its host"
        );
        // `Style::Sheet` — the page beneath is frozen AND cached, which is what
        // `host_page_updates`'s deleted `Route::Account` arm and `Popover::caching_host()`
        // used to say in two places.
        assert_eq!(d.host_policy(), (HostUpdate::Frozen, HostRender::Cached), "{word}");

        // …and BACK dismisses the surface without moving the page.
        let ev = script_key(Key::Back, tick(2));
        frame(&mut d, &mut rig, route.clone(), tick(2), ev);
        assert_eq!(
            d.nav.modals.top().map(|s| s.phase),
            Some(Phase::Closing),
            "{word}: BACK dismisses the menu"
        );
        assert_eq!(d.nav.top_page().map(|e| e.id), Some(host), "{word}: …and nothing else");
    }
}

/// **The card menu is a surface over the page the hold happened on, and the page stays put.**
///
/// `Route::ItemMenu { over: MenuHost }` is what this replaces, and every claim below is a bug
/// that shape could produce. The route was a UNIT variant meaning "Home, plus the panel" until
/// a second card surface existed; naming the host fixed the cut-to-Home but left the page's
/// identity, its chrome, its focus and its trail answers all derived through `page_of` from a
/// route that was not the page. A surface is presented OVER the top page and never replaces
/// it, so there is nothing to name, nothing to close back to, and no second answer to keep in
/// step.
///
/// Driven on all five card surfaces' routes at once, because the six-variant enum's whole
/// failure mode was per-host and silent — a page falling through to Home's draw, a tab bar
/// disappearing mid-hold — and the assertion that catches it is the same one each time.
#[test]
fn the_card_menu_is_a_surface_over_the_page_the_hold_happened_on() {
    let _g = crate::testlock::serial();
    for (route, word) in [
        (AppArg::Home, "home"),
        (AppArg::Library, "library"),
        (AppArg::Search, "search"),
        (detail_arg("1001"), "detail"),
        (person_arg("9"), "person"),
    ] {
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, route.clone(), tick(0), vec![]);
        let host = d.nav.top_page().expect("a host page").id;
        let host_owner = d.nav.input_owner();
        let mut row = crate::pms::PmsMovie::default();
        row.rk = "42".into();
        row.kind = 3;
        row.show_rk = "7".into();
        open_item_menu(&mut d, card_menu_arg(&row, false, matches!(route, AppArg::Home), host, None, None));
        let (top, _) = frame(&mut d, &mut rig, route.clone(), tick(1), vec![]);

        assert!(item_menu_up(&d), "{word}: the hold presented the menu");
        assert_eq!(
            d.nav.top_page().map(|e| e.id),
            Some(host),
            "{word}: a surface is presented OVER the top page and never replaces it"
        );
        assert_eq!(top, word, "{word}: the heartbeat's route= is still the HOST's word");
        assert_eq!(overlay_word(&d), Some("itemmenu"));
        assert_ne!(d.nav.input_owner(), host_owner, "{word}: the menu takes input from its host");
        // `Style::Compact` — the page beneath is served from the shared snapshot while its
        // own motion keeps running, which is exactly the pair the legacy code stated in two
        // places: `Popover::caching_host()` for the render half, and `host_page_updates`
        // answering TRUE for `Route::ItemMenu` for the update half ("an item menu keeps its
        // anchored page live"). Deliberately not the profile menu's `(Frozen, Cached)`: this
        // panel hangs BESIDE the card it is about and the shelf stays legible behind it.
        assert_eq!(d.host_policy(), (HostUpdate::Live, HostRender::Cached), "{word}");
        // …and the chrome question the `MenuHost` arm of `route_wears_tab_bar` answered by
        // hand: the bar is drawn (or not) because the PAGE wears it, with nothing to derive.
        assert_eq!(
            d.nav.top_page().is_some_and(|e| e.arg.chrome() == Chrome::TabBar),
            route.chrome() == Chrome::TabBar,
            "{word}: the host page answers for its own chrome"
        );

        // BACK dismisses the surface without moving the page…
        let ev = script_key(Key::Back, tick(2));
        frame(&mut d, &mut rig, route.clone(), tick(2), ev);
        assert_eq!(
            d.nav.modals.top().map(|s| s.phase),
            Some(Phase::Closing),
            "{word}: BACK dismisses the menu"
        );
        assert_eq!(d.nav.top_page().map(|e| e.id), Some(host), "{word}: …and nothing else");
        // …and it reported nothing: a dismissal is not a commit.
        assert!(rig.take_item_menu_reqs().is_empty(), "{word}");
    }
}

/// **OK on a row reports ONE request and dismisses in the same drain**, carrying the row and
/// the server the panel captured.
///
/// It was two producers and a static: `key_item_menu` (and a second, drifting copy on the
/// pointer path) called `item_menu::on_ok`, which CLOSED the popover and returned the action,
/// and the dispatch then read `item_menu::ITEM`/`SID` — statics deliberately not cleared by
/// the close, because the drain read them a frame later. The request carries all three, so
/// nothing is read after the dismissal at all.
#[test]
fn a_card_menus_commit_reports_one_request_carrying_the_row_it_captured() {
    let _g = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    let host = d.nav.top_page().unwrap().id;
    let mut row = crate::pms::PmsMovie::default();
    row.rk = "42".into();
    row.kind = 0; // a movie: [Go to Movie, —, Mark as Watched, Play from Start]
    row.unwatched = true;
    row.part = "/library/parts/42/file.mkv".into();
    open_item_menu(&mut d, card_menu_arg(&row, false, true, host, None, None));
    frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
    let menu = d.nav.modals.top().unwrap().entry.id;

    // A movie's rows are [Go to Movie, —, Mark as Watched, Play from Start], so the SEPARATOR
    // is index 1 and the row this test commits is index 3. Two DOWNs reach it, and the first
    // of them is what proves the separator is not a stop: it lands on 2, not on 1.
    let mut t = 2;
    let mut press = |d: &mut Dispatcher<AppHost>, rig: &mut Bridge, key: Key| {
        let ev = script_key(key, tick(t));
        frame(d, rig, AppArg::Home, tick(t), ev);
        t += 1;
    };
    press(&mut d, &mut rig, Key::Down);
    assert_eq!(
        d.focus().map(|k| (k.entry, k.elem)),
        Some((menu, 2)),
        "the engine steps OVER the separator at index 1, which carries no action"
    );
    press(&mut d, &mut rig, Key::Down);
    assert_eq!(d.focus().map(|k| (k.entry, k.elem)), Some((menu, 3)));
    press(&mut d, &mut rig, Key::Ok);

    let reqs = rig.take_item_menu_reqs();
    assert_eq!(reqs.len(), 1, "one commit, one request");
    assert!(matches!(&reqs[0].act, crate::screens::item_menu::Action::PlayFromStart(rk) if rk == "42"));
    assert!(reqs[0].from_home, "…and the trail reset the HOME root earns (`menu_leave`)");
    assert!(!reqs[0].loaded_episode);
    assert_eq!(
        reqs[0].item.as_ref().map(|m| m.part.as_str()),
        Some("/library/parts/42/file.mkv"),
        "the WHOLE row rides on the request — a key alone cannot start playback"
    );
    assert_eq!(
        d.nav.modals.surfaces.iter().find(|s| s.entry.id == menu).map(|s| s.phase),
        Some(Phase::Closing),
        "every commit dismisses, exactly as the legacy `on_ok` closed first"
    );
    assert_eq!(d.nav.top_page().map(|e| e.id), Some(host), "the page never moved");
}

/// **The Settings row hands one surface to another without ever un-freezing the host** (§16.5).
///
/// The two ops are parked one frame apart — the screen's own `NavOp::Dismiss` commits on the
/// press frame, and `LoopReq::AccountSettings` is drained after `bridge::frame` returns, so
/// `open_settings`' `Present` commits on the next one. The window between them is the risk: if
/// the account sheet let go of the host on its press frame, the page would be re-rendered in
/// full for one frame and re-snapshotted for the next, under a panel nobody can see, on the
/// exact frame the Settings ground is being composed over it.
///
/// It does not, and the reason is structural rather than lucky: a dismissed `Style::Sheet` is
/// `Phase::Closing`, whose policy is `(Live, Cached)` — the update half goes live, the RENDER
/// half keeps the snapshot for the length of the fade — and the incoming `Opaque { snapshot:
/// true }` is `(Frozen, Cached)` from its first frame. `HostRender` therefore never returns to
/// `Live` at all, so the fold never passes through `(Live, Live)`.
///
/// **Pinned here rather than on the television** (§16.5): `fps:modal-ramp` would pass either
/// way — the measured difference between a cached host and a live one on that scene is 2.3 ms
/// against 75 — so a device gate could not see the regression this test exists for.
///
/// Red first, simulated: with the screen's `Dismiss` removed from `activate`, the account
/// sheet stays `Open` `(Frozen, Cached)` under the Settings surface and this passes — so the
/// discriminating half is the `Closing` assertion below, which fails without it. With
/// `LoopReq::AccountSettings` mapped to `dismiss_surfaces` + `open_settings` in the WRONG
/// order (present first, then dismiss all) the fold reads `(Frozen, Cached)` throughout and
/// the Settings surface is dismissed with the sheet — caught by the `settings_up` assertion.
#[test]
fn account_to_settings_never_unfreezes_the_host() {
    let _g = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    let host = d.nav.top_page().unwrap().id;
    open_account_menu(&mut d);
    frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
    assert_eq!(d.host_policy(), (HostUpdate::Frozen, HostRender::Cached));

    // Focus the Settings row and commit it. Signed out (a host test has no session file the
    // fixture wrote), the rows are [Sign in, Settings], so one DOWN then OK.
    let menu_entry = d.nav.modals.top().unwrap().entry.id;
    let mut t = 2;
    let mut press = |d: &mut Dispatcher<AppHost>, rig: &mut Bridge, key: Key| {
        let ev = script_key(key, tick(t));
        frame(d, rig, AppArg::Home, tick(t), ev);
        t += 1;
    };
    press(&mut d, &mut rig, Key::Down);
    assert_eq!(
        d.focus().map(|k| (k.entry, k.elem)),
        Some((menu_entry, 1)),
        "the engine walked the menu's own rows"
    );
    press(&mut d, &mut rig, Key::Ok);

    // The commit frame: the sheet is dismissed and the host's RENDER half is still Cached.
    assert_eq!(
        d.nav.modals.surfaces.iter().find(|s| s.entry.id == menu_entry).map(|s| s.phase),
        Some(Phase::Closing),
        "the row's commit dismisses the sheet"
    );
    let mut renders = vec![d.host_policy().1];

    // …and the loop's drain performs the request on the next frame.
    let reqs = rig.take_reqs();
    assert_eq!(reqs, vec![LoopReq::AccountSettings], "one request, and it is the Settings row's");
    open_settings(&mut d);
    for i in 0..12u32 {
        frame(&mut d, &mut rig, AppArg::Home, tick(20 + i), vec![]);
        renders.push(d.host_policy().1);
    }
    assert!(settings_up(&d), "the Settings surface is up");
    assert_eq!(
        d.nav.top_page().map(|e| e.id),
        Some(host),
        "the host page never moved under either surface"
    );
    assert!(
        !renders.contains(&HostRender::Live),
        "the host was re-rendered in full during the handover: {renders:?}"
    );
}

/// **Phase 9: the player's four panels are entries on ITS page's own stack, not on the route.**
///
/// Three claims, and each one is a bug the `Route::Player { overlay }` shape could produce.
/// (1) Presenting a panel makes it the INPUT OWNER, which is what replaces the ladder's four
/// `if …overlay… { continue }` arms. (2) It does NOT move the app's page stack — the player
/// stays the top page, so the video plane, the subtitle bitmaps and the transport's springs
/// are not torn down to open a menu over them. (3) Dismissing it gives input back to the
/// player, with the SAME instance underneath: a remount here would reset the HUD's timer and
/// the control row's springs, which is precisely what `same_instance` ignoring the overlay is
/// for (§16.9).
#[test]
fn a_player_panel_is_a_surface_on_the_players_own_page_and_leaves_the_instance_alone() {
    let ps = crate::route::PlaybackSession::IDLE;
    let _g = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Player, tick(0), vec![]);
    let page = d.nav.top_page().expect("the player is a page").id;
    let instance = d.nav.top_page().and_then(|e| e.inst.as_ref()).map(|i| i.id);
    assert!(instance.is_some(), "…with a live instance");
    let depth = d.nav.tabs.stack.depth();
    for kind in [
        crate::screens::player::overlay::OverlayKind::Tracks { tab: 1 },
        crate::screens::player::overlay::OverlayKind::Info,
        crate::screens::player::overlay::OverlayKind::Chapters,
        crate::screens::player::overlay::OverlayKind::More { quality: false },
    ] {
        open_player_overlay(&ps, crate::stores::metadata::MetadataStore::default().view(), &mut d, kind);
        frame(&mut d, &mut rig, AppArg::Player, tick(1), vec![]);
        assert_eq!(player_overlay_kind(&d), Some(kind), "{kind:?} is up");
        assert_eq!(
            overlay_word(&d),
            Some(kind.word()),
            "and the heartbeat says so from the surface, not from a second table",
        );
        assert_ne!(
            d.nav.input_owner(),
            Some(InputOwner::Entry(page)),
            "{kind:?} owns input while it is up",
        );
        assert_eq!(d.nav.tabs.stack.depth(), depth, "the app's page stack never moved");
        assert_eq!(d.nav.top_page().map(|e| e.id), Some(page), "…and the player is still it");

        dismiss_player_overlays(&mut d);
        frame(&mut d, &mut rig, AppArg::Player, tick(2), vec![]);
        frame(&mut d, &mut rig, AppArg::Player, tick(3), vec![]);
        assert_eq!(player_overlay_kind(&d), None, "{kind:?} dismissed");
        assert_eq!(
            d.nav.top_page().and_then(|e| e.inst.as_ref()).map(|i| i.id),
            instance,
            "{kind:?} closed onto the SAME player instance — no remount",
        );
    }
}

/// **`ScreenArg::same_instance` compares the PLAYBACK, never the panel** (§16.9), and the two
/// arguments are different KINDS: a panel is `AppArg::PlayerOverlay`, so it can never be
/// mistaken for the page it stands on. Two panels of different kinds are two instances —
/// which is what makes `sync_page`'s "is the top page already what I want" answer stable while
/// a menu is open, and what stops the track menu being reused as the Info card.
#[test]
fn same_instance_reads_the_playback_and_not_the_overlay() {
    use crate::screens::player::overlay::{OverlayKind, PlayerOverlayArg};
    use crate::ui::screen::ScreenArg;
    let player = AppArg::Player;
    assert!(player.same_instance(&AppArg::Player));
    for kind in [
        OverlayKind::Tracks { tab: 0 },
        OverlayKind::Info,
        OverlayKind::Chapters,
        OverlayKind::More { quality: false },
    ] {
        let panel = AppArg::PlayerOverlay(PlayerOverlayArg { kind });
        assert!(
            !player.same_instance(&panel) && !panel.same_instance(&player),
            "{kind:?} is not the playback",
        );
        assert!(panel.same_instance(&AppArg::PlayerOverlay(PlayerOverlayArg { kind })));
    }
    assert!(!AppArg::PlayerOverlay(PlayerOverlayArg { kind: OverlayKind::Info })
        .same_instance(&AppArg::PlayerOverlay(PlayerOverlayArg {
            kind: OverlayKind::Chapters
        })));
    // The panel that carries a parameter is still ONE panel: reopening the track menu on the
    // other tab reuses the entry rather than stacking a second one, exactly as
    // `open_player_overlay`'s early return says.
    assert!(AppArg::PlayerOverlay(PlayerOverlayArg { kind: OverlayKind::Tracks { tab: 0 } })
        .same_instance(&AppArg::PlayerOverlay(PlayerOverlayArg {
            kind: OverlayKind::Tracks { tab: 1 }
        })));
}

/// **THE FOURTH ROOT** (`app/input.rs`'s `back_at_root`, and the key ladder's dispatcher arm).
///
/// BACK at the FIRST consent stage was SWALLOWED for as long as that screen was a `Popover`:
/// the step behind it is sign-in, which cannot be undone, and `ui::consent::on_back` reported
/// `true` for both the stepped-back and the swallowed case — so the loop's BACK arm could not
/// tell them apart and the 2026-09-03 root rule could not reach this screen. What is graded
/// here is that the owned screen SAYS which it is, as a request the loop performs, and that it
/// does so WITHOUT dismissing itself: going to the television's Home neither answers nor
/// dismisses the question, so selecting the tile again must come straight back to it.
#[test]
fn back_at_the_first_consent_stage_is_the_root_press_and_leaves_the_question_up() {
    let _g = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Profiles, tick(0), vec![]);
    open_first_run_consent(&mut d);
    frame(&mut d, &mut rig, AppArg::Profiles, tick(1), vec![]);
    assert!(consent_up(&d));
    assert_eq!(overlay_word(&d), Some("consent"));
    let _ = rig.take_reqs(); // the mount's own effects are not what this grades
    frame(&mut d, &mut rig, AppArg::Profiles, tick(2), script_key(Key::Back, tick(2)));
    assert_eq!(
        rig.take_reqs(),
        vec![LoopReq::BackAtRoot],
        "the screen asks the loop for the root press rather than swallowing the key"
    );
    frame(&mut d, &mut rig, AppArg::Profiles, tick(3), vec![]);
    assert!(
        consent_up(&d),
        "…and the question is still up: the platform took the screen, nothing was answered"
    );
}

/// TV session 4 (2026-09-09): `settings-root` fell from 60 to 40 fps the day Home became an
/// owned page. The fold said Replaced, but the loop's guard read `!host_replaced ||
/// page_owned`, so the page closure still ran under the opaque ground and served the frozen
/// full-screen snapshot quad every frame on top of the ground's own wash. A Replaced host
/// receives nothing (§8.3), whoever owns the page — this pins the plan the loop draws by.
#[test]
fn a_replaced_host_draws_surfaces_only_whoever_owns_the_page() {
    assert_eq!(
        page_plan(true, true),
        PagePlan::SurfacesOnly,
        "the owned Home under an opaque Settings ground: no page pass, no cached quad"
    );
    assert_eq!(
        page_plan(true, false),
        PagePlan::SurfacesOnly,
        "a legacy page under one, exactly as before phase 8"
    );
    assert_eq!(page_plan(false, true), PagePlan::Owned, "the closure draws an owned page and its surfaces");
    assert_eq!(page_plan(false, false), PagePlan::LegacyThenSurfaces);
}

/// The other half of the coexistence contract: an OWNED PAGE (first-run Favourites) is the
/// dispatcher's to draw, and [`page_owned`] is what says so.
///
/// **The second half of this test was a stale-frame guard and is retired with D1.** It asserted
/// that `page_owned` answers `false` while the committed route and the tree DISAGREE — a
/// window that existed because the loop held a second copy of "which page is on top" and a
/// `LoopReq` could flip it after the dispatcher's frame. There is one authority now, so the
/// two cannot disagree and the predicate has no route argument left to compare against.
#[test]
fn the_first_run_favourites_page_is_owned_because_the_container_holds_it() {
    let _g = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    assert!(page_owned(&d), "Home now draws through its owned instance");
    assert_eq!(frame(&mut d, &mut rig, AppArg::Onboard, tick(1), vec![]).0, "onboard");
    assert!(d.owns_input(), "an owned page takes the ladders' input too, not only a surface");
    assert!(page_owned(&d), "…and the page the container holds is the one that is owned");
}

/// **The regression this pass exists to close**: the confirmed "Delete all local data" sweep
/// used `dismiss_surfaces` (the ordinary, spring-driven path) to tear down the Settings
/// surface it had just been answered inside of, which left a Cached snapshot of the erased
/// Settings/Home page compositing over the freshly-mounted sign-in screen for as long as the
/// appear spring took to settle. What must be true instead: `dismiss_surfaces_now` puts the
/// surface at ITS OWN `Phase::Closing` target — `motion.appear == 0.0`, already `settled()` —
/// in the SAME call, with no frame boundary in between. Contrast with `dismiss_surfaces`,
/// which only PARKS a `NavOp::Dismiss` that does not even start applying until the next
/// `frame()`'s NAV COMMIT — that one extra frame, running over a route that has already
/// flipped, was the ghost.
#[test]
fn dismiss_surfaces_now_settles_the_surface_in_the_same_call_unlike_the_ordinary_path() {
    let _g = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    open_settings(&mut d);
    frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
    assert!(settings_up(&d));
    // **A second frame, and it is not padding.** The dispatcher's NAV COMMIT is step 7 of its
    // frame and the container Tick is step 4, so the surface `open_settings` parked is mounted
    // AFTER this frame's motion step has already run: at the end of the `tick(1)` frame it
    // exists with its spring untouched at exactly 0.0. That is correct — nothing can integrate
    // motion for a surface that did not exist when the tick ran — but it means one frame is
    // not yet "mid-open", and asserting `appear > 0.0` there fails on the frame ORDER rather
    // than on anything this test is about.
    frame(&mut d, &mut rig, AppArg::Home, tick(2), vec![]);
    // …and a THIRD: the first tick after `present` holds the spring at 0 for the frame that
    // renders the host into its snapshot (`PopoverMotion::hold`), so the ramp starts one later.
    frame(&mut d, &mut rig, AppArg::Home, tick(3), vec![]);
    // Now one tick HAS run over it: the appear spring is climbing toward its open target and
    // the phase has not yet been promoted past `Opening` (`ModalStack::tick` only promotes it
    // to `Open` once `motion.settled()`) — i.e. deliberately mid-open, nowhere near the
    // "already at the closed end" state `dismiss_surfaces_now` must reach in the SAME call.
    let before = d.nav.modals.top().expect("just presented");
    assert_eq!(before.phase, Phase::Opening);
    assert!(before.motion.appear > 0.0, "the spring has moved off its starting rest position");
    assert!(!before.motion.settled(), "one tick cannot have already reached the open target");
    dismiss_surfaces_now(&mut d);
    let top = d
        .nav
        .modals
        .top()
        .expect("hide() only changes phase/motion; the surface is still in the tree until prune() next runs");
    assert_eq!(top.phase, Phase::Closing, "hide() moves the phase immediately, exactly like dismiss()");
    assert!(
        top.motion.settled(),
        "…but hide() also SNAPS the motion to its target, so it is settled with no frame having run"
    );
    assert_eq!(top.motion.appear, 0.0, "settled at the closed end, not merely heading there");
}

/// **Regression pin for `run::update`'s chrome chain's third (Search) arm** — the one that
/// steps `update_home_chrome` while standing on Search because `SearchScreen::tick` only
/// steps its OWN rows and never touches the shared strip's capsule/scroll springs that live in
/// `Bridge`'s own `strip: StripRender` field. Without that arm the strip's published rects
/// (`d.nav.tabs.strip`, read by pointer hit-testing and focus) freeze at whatever the
/// previously-drawn page left them at.
///
/// Reproduced here without a font, `App`, SDL or GL: `Bridge::for_test` supplies a
/// `Measure`-only chrome, and the shared strip's scroll spring is `rig.strip`'s own field —
/// this test seeds it directly (a private field, reachable because this module is `Bridge`'s
/// own) rather than through a process-wide static, driving it away from rest with a synthetic,
/// wide vocabulary (independent of Search's own short "Home"/"Movies"/"TV Shows"/search-icon
/// labels, which never grow wide enough on their own to need scrolling) to stand in for
/// "wherever the previous page left it", then `capture_chrome(Route::Search)` publishes the
/// strip once against that stale scroll — exactly the one-shot capture `run::update`'s NAV
/// COMMIT already does every frame regardless of this arm. What only the missing arm can
/// fix is calling `update_home_chrome` afterwards: this test asserts the published rect
/// travels back to its (correct, un-stale) target once that call is made every frame, as the
/// Search arm does.
#[test]
fn search_route_steps_the_shared_strip_so_its_published_rects_do_not_go_stale() {
    let _guard = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.browse.borrow_mut().seed_two_source_table_for_test();
    rig.refresh_browse_directory();

    // A synthetic, artificially wide vocabulary — nothing to do with Search's real labels —
    // whose reveal target for its last entry sits well past the real strip's viewport, so
    // jumping straight to it (`StripRender::reveal`, a JUMP, not a step) leaves `rig.strip`'s
    // scroll spring far from where Search's own (short) labels want it.
    let wide: Vec<String> = (0..8).map(|i| format!("Wide Destination Label Number {i}")).collect();
    rig.strip.reveal(
        crate::ui::widgets::TabLabels { generation: 1, labels: &wide },
        wide.len() - 1,
    );

    super::show_page(&mut d, AppArg::Search);
    rig.capture_chrome(&mut d);

    let base = crate::ui::dispatch::STRIP_BASE;
    let search_member = |d: &Dispatcher<AppHost>| {
        d.nav.tabs.strip.iter().find(|m| m.elem == base + 3).copied().expect("Search pill published")
    };
    let before = search_member(&d);
    let stale_gap = (before.drawn.x - before.target.x).abs();
    assert!(stale_gap > 10.0,
        "the synthetic wide vocabulary must actually leave the scroll away from Search's own target; gap={stale_gap}");

    // The fix under test: the Search arm of `run::update`'s chrome chain calls exactly this,
    // every frame, while standing on Search.
    let mut glass = crate::ui::frame::glass::GlassPlan::new();
    for _ in 0..60 {
        rig.update_home_chrome(&mut d, &mut glass, 1.0 / 60.0);
    }

    let after = search_member(&d);
    let settled_gap = (after.drawn.x - after.target.x).abs();
    assert!(settled_gap < 1.0,
        "the strip's published rect must travel to its target once Search steps it each frame \
         (stale={stale_gap}, settled after 1s={settled_gap}) — losing the Search chrome arm \
         again silently reproduces the frozen capsule / stale pointer targets this pins");
}
