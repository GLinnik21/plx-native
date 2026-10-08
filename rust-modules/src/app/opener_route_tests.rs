//! The item menu's opener and argument builders answer only for the route that owns the card:
//! `home_opener` anchors at the live drawn rect of a Home grid card (the hero is no card),
//! `content_menu_arg` answers a Detail page's Related shelf at that card's rect, and a page on
//! another route yields `None` from every builder even when its `focused_card` has the right type.

use super::*;
use plx_ui::screen::Focusable;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::frame;

fn home_rig() -> (Dispatcher<AppHost>, Bridge) {
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.hubs.seed_grid_for_test(6, 24);
    frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
    d.nav.tabs.stack.transition = Box::new(plx_ui::containers::transition::Immediate);
    (d, rig)
}

#[test]
fn home_opener_anchors_at_the_drawn_rect_of_a_grid_card_and_at_nothing_for_the_hero() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = home_rig();
    let entry = d.nav.top_page().unwrap().id;
    for i in 1..20 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
    assert!(!rig.home_grid_focused(&d), "Home boots on the hero");
    let hero = d.focus().unwrap();
    assert!(rig.home_opener(&d, entry, Some(hero)).rect.is_none(), "the hero is not a card");

    let instance = d.nav.instance_of(entry).unwrap();
    d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
        Delivery::Screen(ScreenEvent::App(AppMsg::Home(HomeCmd::FocusGrid { row: 2, col: 3 })))));
    let mut live_differs_from_rest = false;
    for i in 20..140 {
        let inputs = if (100..104).contains(&i) { script_key(Key::Left, tick(i)) } else { vec![] };
        frame(&mut d, &mut rig, AppArg::Home, tick(i), inputs);
        if !rig.home_grid_focused(&d) { continue; }
        let focus = d.focus().unwrap();
        let (live, rest) = rig.with_home(&d, |s, cx, f| {
            let placed = Focusable::<AppHost>::place(s, &f.unwrap().elem, cx, At::Drawn).unwrap();
            (placed.rect, placed.rest_rect)
        }).unwrap();
        live_differs_from_rest |= live != rest;
        let opener = rig.home_opener(&d, entry, Some(focus));
        assert_eq!(opener.rect, Some(live), "frame {i}: the opener anchors at the DRAWN rect");
        let foreign = FocusKey { entry: EntryId(entry.0 + 100), elem: focus.elem };
        assert!(rig.home_opener(&d, entry, Some(foreign)).rect.is_none());
    }
    assert!(live_differs_from_rest, "the fixture must catch the drawn rect off its settled one, or a rect/rest_rect swap is invisible");
}

#[test]
fn a_home_card_is_not_answered_by_the_library_search_or_content_builders() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = home_rig();
    let entry = d.nav.top_page().unwrap().id;
    let instance = d.nav.instance_of(entry).unwrap();
    d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
        Delivery::Screen(ScreenEvent::App(AppMsg::Home(HomeCmd::FocusGrid { row: 1, col: 1 })))));
    for i in 1..80 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
    assert!(rig.home_grid_focused(&d));
    let focus = d.focus();
    // `focused_card` yields a PmsMovie with actions on this page; the route is what says no.
    assert!(rig.home_opener(&d, entry, focus).rect.is_some());
    assert!(rig.library_selection(&d, entry, focus).is_none());
    assert!(rig.search_selection(&d, entry, focus).is_none());
    let ret = ReturnState { focus, ..Default::default() };
    assert!(rig.content_menu_arg(&d, entry, &ret).is_none(),
        "a Home card's menu is built from its own request, which carries from_deck and from_home");
}

#[test]
fn a_library_card_is_not_answered_by_the_home_or_content_builders() {
    let _guard = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("opener-route-lib", "127.0.0.1", 9, "synthetic", "fixture");
    let shared = plx_plex::plex::register_for_test("opener-route-lib-shared", "127.0.0.1", 10, "synthetic", "fixture");
    plx_plex::plex::set_current(sid);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.browse.borrow_mut().seed_registered_table_for_test([sid, shared]);
    rig.refresh_browse_directory();
    rig.browse_run(plx_data::stores::browse::BrowseCmd::SetCur(0));
    rig.stores.browse.borrow_mut().seed_items_for_test(120);
    frame(&mut d, &mut rig, AppArg::Library, tick(0), vec![]);
    d.nav.tabs.stack.transition = Box::new(plx_ui::containers::transition::Immediate);
    Bridge::library_command(&mut d, plx_screens::registry::LibraryCmd::FocusGrid { row: 1, col: 1 });
    for i in 1..80 { frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]); }
    let entry = d.nav.top_page().unwrap().id;
    let focus = d.focus();
    assert!(rig.library_selection(&d, entry, focus).is_some(), "the fixture reaches a Library card");
    assert!(rig.home_opener(&d, entry, focus).rect.is_none());
    assert!(rig.search_selection(&d, entry, focus).is_none());
    let ret = ReturnState { focus, ..Default::default() };
    assert!(rig.content_menu_arg(&d, entry, &ret).is_none(), "a Library card's menu carries from_deck");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_detail_related_card_opens_its_menu_at_the_drawn_rect_and_other_shelves_open_none() {
    let _guard = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("opener-route-detail", "127.0.0.1", 9, "synthetic", "fixture");
    let route = AppArg::Content(ContentArg::Detail { sid, rk: "movie".into() });
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    let related = |rk: &str| plx_data::pms::PmsMovie { sid, rk: rk.into(), sec: 1, ..Default::default() };
    // Installed BEFORE the mount: the page keys its element identities from the item it finds
    // current when it mounts.
    plx_data::metadata::set_current_for_test(rig.stores.metadata.state_mut(), Some(plx_data::metadata::Detail {
        sid,
        rk: "movie".into(),
        kind: "movie".into(),
        related: vec![related("rel-a"), related("rel-b")],
        extras: vec![Default::default(), Default::default()],
        cast: (1..=2).map(|id| plx_data::metadata::Cast {
            tag: "Actor".into(), role: "Role".into(), thumb: String::new(), id, tag_key: String::new(),
        }).collect(),
        ..Default::default()
    }));
    d.request(MachineId::Nav, NavOp::Root(route));
    for now in 0..6 { d.frame_with(&mut rig, tick(now), Vec::new(), Vec::new(), &mut NoTap, false); }
    let entry = d.nav.top_page().unwrap().id;
    // The builder is a pure function of the focus key it is handed, so every element key the page
    // could own is asked in turn: exactly the two Related cards answer (not Play, Extras or Cast).
    let answers = |d: &Dispatcher<AppHost>, rig: &Bridge, elem: u32| {
        let focus = Some(FocusKey { entry, elem });
        rig.content_menu_arg(d, entry, &ReturnState { focus, ..Default::default() })
    };
    let drawn = |d: &Dispatcher<AppHost>, rig: &Bridge, elem: u32| {
        let screen = &d.nav.entry(entry).unwrap().inst.as_ref().unwrap().screen;
        let parts = CxParts { tick: Tick::default(), press: Default::default(),
            focus: plx_machine::machine::FocusRead { current: Some(FocusKey { entry, elem }), ..Default::default() },
            owner: InputOwner::Entry(entry) };
        let cx = parts.cx::<AppHost>(rig.views(), &rig.measure);
        let page = screen.as_any().unwrap().downcast_ref::<plx_screens::detail::DetailScreen>().unwrap();
        Focusable::<AppHost>::place(page, &elem, &cx, At::Drawn).expect("a Related card is placed")
    };
    let bits = |r: plx_ui::Rect| [r.x.to_bits(), r.y.to_bits(), r.w.to_bits(), r.h.to_bits()];
    let mut cards = Vec::new();
    for elem in 0..4096u32 + 2048 {
        let Some(arg) = answers(&d, &rig, elem) else { continue };
        assert!(matches!(arg.kind, ItemMenuKind::Card { from_deck: false, .. }) && !arg.from_home);
        cards.push((arg.rk, elem));
    }
    cards.sort();
    assert_eq!(cards.iter().map(|(rk, _)| rk.as_str()).collect::<Vec<_>>(), ["rel-a", "rel-b"],
        "only the Related shelf's cards answer");
    // Focus the card and watch it pop: the menu hangs off the DRAWN rect on every frame, and the
    // fixture catches that rect off the settled one, or a rect/rest_rect swap would be invisible.
    let elem = cards[0].1;
    let to = FocusKey { entry, elem };
    let from = d.focus().unwrap();
    d.set_focus_in(Some(to), None);
    let instance = d.nav.instance_of(entry).unwrap();
    d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
        Delivery::Screen(ScreenEvent::FocusMoved { from: Some(from), to, by: plx_machine::machine::By::Dir })));
    let mut live_differs_from_rest = false;
    for now in 6..40 {
        d.frame_with(&mut rig, tick(now), Vec::new(), Vec::new(), &mut NoTap, false);
        let placed = drawn(&d, &rig, elem);
        assert_eq!(answers(&d, &rig, elem).unwrap().anchor, bits(placed.rect), "frame {now}");
        live_differs_from_rest |= placed.rect != placed.rest_rect;
    }
    assert!(live_differs_from_rest, "the fixture must put the drawn rect off the settled one");
    plx_data::metadata::set_current_for_test(rig.stores.metadata.state_mut(), None);
    plx_plex::plex::reset_servers_for_test();
}
