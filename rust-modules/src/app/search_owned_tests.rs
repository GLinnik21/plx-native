fn owned_search_probe(d: &Dispatcher<AppHost>) -> String {
    let screen = d.top_screen().expect("Search mounted");
    assert!(screen.as_any().unwrap().is::<crate::screens::search::SearchScreen>());
    let mut probe = String::new();
    screen.state().probe(&mut probe);
    probe
}

#[test]
fn owned_search_adopts_panel_text_without_restarting_or_losing_the_commit() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-adopt");
    session.watching("synthetic-adopt-profile");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::SetQuery("ab".into()));
    crate::search::publish_shelves_for_test(vec![crate::search::Shelf { kind: crate::search::Kind::Movie,
        items: vec![crate::search::Item::Media(crate::pms::PmsMovie {
            rk: "adopt-result".into(), title: "Synthetic result".into(), ..Default::default()
        })] }]);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.mounter.search_owned = true;
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);
    let field = d.focus();
    frame(&mut d, &mut rig, Route::Search, tick(1), script_key(Key::Down, tick(1)));
    assert_ne!(d.focus(), field);
    frame(&mut d, &mut rig, Route::Search, tick(2),
        crate::app::events::text_inputs("stray", false, tick(2), Source::Sdl));
    assert_eq!(crate::search::query(), "ab", "closed desktop fields reject stray text");
    frame(&mut d, &mut rig, Route::Search, tick(3),
        crate::app::events::text_inputs("X", true, tick(3), Source::Sdl));
    assert_eq!(crate::search::query(), "abX");
    assert!(owned_search_probe(&d).contains("editing=true"));
    assert_eq!(rig.keyboard_adoptions, 1);
    assert_eq!(d.focus(), field, "adoption returns focus from results to the editing field");
    assert!(rig.keyboard_calls.is_empty(), "adoption must not call start, which clears pending text");
    let mut events = script_key(Key::Left, tick(4));
    events.extend(crate::app::events::text_inputs("Y", true, tick(4), Source::Sdl));
    frame(&mut d, &mut rig, Route::Search, tick(4), events);
    assert_eq!(crate::search::query(), "abYX", "an already-editing field keeps its chosen caret");
    assert_eq!(rig.keyboard_adoptions, 2);
    assert!(rig.keyboard_calls.is_empty());
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

#[test]
fn owned_search_opens_edits_and_closes_in_one_input_batch_in_order() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-ingress-order");
    session.watching("synthetic-ingress-order");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.mounter.search_owned = true;
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);
    let text = |s: &str, at| InputEvent { at, source: Source::Script,
        kind: InputKind::Text(crate::ui::machine::TextEdit::Commit(s.into())) };
    let mut events = script_key(Key::Ok, tick(1));
    events.push(text("a", tick(1)));
    events.extend(script_key(Key::Left, tick(1)));
    events.push(text("b", tick(1)));
    frame(&mut d, &mut rig, Route::Search, tick(1), events);
    assert_eq!(crate::search::query(), "ba");
    assert_eq!(rig.keyboard_calls, [true]);
    let mut events = script_key(Key::Back, tick(2));
    events.push(text("x", tick(2)));
    frame(&mut d, &mut rig, Route::Search, tick(2), events);
    assert_eq!(crate::search::query(), "ba", "desktop text after dismissal is not a new edit");
    assert_eq!(rig.keyboard_calls, [true, false]);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

#[test]
fn an_old_search_keyboard_request_cannot_close_the_new_instances_keyboard() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-keyboard-owner");
    session.watching("synthetic-keyboard-owner");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.mounter.search_owned = true;
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);
    let old = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    frame(&mut d, &mut rig, Route::Search, tick(1), script_key(Key::Ok, tick(1)));
    assert_eq!(d.input.keyboard_owner, Some(old));
    d.request(MachineId::Nav, NavOp::Push(AppArg::Legacy(Route::Search)));
    frame(&mut d, &mut rig, Route::Search, tick(2), vec![]);
    let current = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    assert_ne!(current, old);
    assert!(!d.input.keyboard, "the departing owner can release its keyboard after navigation commits");
    assert_eq!(rig.keyboard_calls, [true, false]);
    frame(&mut d, &mut rig, Route::Search, tick(3), script_key(Key::Ok, tick(3)));
    assert!(d.input.keyboard);
    assert_eq!(d.input.keyboard_owner, Some(current));
    d.emit(MachineId::Instance(old), Fx::Deliver(MachineId::Instance(old), Delivery::Keyboard { up: false }));
    frame(&mut d, &mut rig, Route::Search, tick(4), vec![]);
    assert!(d.input.keyboard, "a stale request is not an authoritative OS dismissal");
    d.emit(MachineId::Instance(old), Fx::Deliver(MachineId::Instance(old), Delivery::Keyboard { up: true }));
    frame(&mut d, &mut rig, Route::Search, tick(5), vec![]);
    assert_eq!(d.input.keyboard_owner, Some(current));
    assert_eq!(rig.keyboard_calls, [true, false, true], "rejected requests must not reach the native adapter");
    frame(&mut d, &mut rig, Route::Search, tick(6), script_key(Key::Back, tick(6)));
    assert!(!d.input.keyboard, "the current instance can still dismiss its own keyboard");
    assert_eq!(rig.keyboard_calls, [true, false, true, false]);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

#[test]
fn covering_owned_search_releases_its_native_keyboard() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-covered-keyboard");
    session.watching("synthetic-covered-search");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.mounter.search_owned = true;
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);
    frame(&mut d, &mut rig, Route::Search, tick(1), script_key(Key::Ok, tick(1)));
    assert_eq!(rig.keyboard_calls, [true]);
    d.request(MachineId::Nav, NavOp::Present(AppArg::Settings(crate::screens::family::SettingsPage::Root)));
    frame(&mut d, &mut rig, Route::Search, tick(2), vec![]);
    assert!(!d.input.keyboard, "the system keyboard cannot trap input above a new application modal");
    assert!(d.input.keyboard_owner.is_none());
    assert_eq!(rig.keyboard_calls, [true, false]);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

#[test]
fn owned_search_unrelated_store_notice_cannot_ack_a_net_zero_edit_batch() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-ack");
    session.watching("synthetic-ack-profile");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.mounter.search_owned = true;
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);
    let target = MachineId::Instance(d.nav.top_page().unwrap().inst.as_ref().unwrap().id);
    for kind in [InputKind::SystemKeyboard(true),
        InputKind::Text(crate::ui::machine::TextEdit::Commit("a".into())),
        InputKind::Text(crate::ui::machine::TextEdit::Backspace)] {
        d.emit(MachineId::Input, Fx::Deliver(target, Delivery::Screen(ScreenEvent::Input(InputEvent {
            at: tick(1), source: Source::Script, kind,
        }))));
    }
    d.emit(MachineId::Store(StoreId::Browse.ord()), Fx::Deliver(target,
        Delivery::Screen(ScreenEvent::StoreChanged(StoreId::Browse.ord(), 100))));
    frame(&mut d, &mut rig, Route::Search, tick(1), vec![]);
    assert_eq!(crate::search::query(), "");
    assert!(owned_search_probe(&d).contains("pending=true"),
        "matching frozen text is not an acknowledgement from another store");
    frame(&mut d, &mut rig, Route::Search, tick(2), vec![]);
    assert!(owned_search_probe(&d).contains("pending=false"));
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

#[test]
fn owned_search_keeps_several_commits_while_the_frame_view_is_frozen() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-text");
    session.watching("synthetic-search-profile");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.mounter.search_owned = true;
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);
    assert!(owned_search_probe(&d).contains("pending=false"));
    let retained = rig.search.clone();
    let mut input = vec![InputEvent { at: tick(1), source: Source::Script, kind: InputKind::SystemKeyboard(true) }];
    input.extend(["s", "u", "m", "summer "].map(|text| InputEvent {
        at: tick(1), source: Source::Script, kind: InputKind::Text(crate::ui::machine::TextEdit::Commit(text.into())),
    }));
    frame(&mut d, &mut rig, Route::Search, tick(1), input);
    assert_eq!(crate::search::query(), "summer ", "the real store received every committed edit");
    assert_eq!(retained.view().query(), "");
    assert_eq!(rig.search.view().query(), "", "this frame still has its original immutable view");
    assert!(owned_search_probe(&d).contains("pending=true"));
    frame(&mut d, &mut rig, Route::Search, tick(2), vec![]);
    assert_eq!(rig.search.view().query(), "summer ");
    assert!(owned_search_probe(&d).contains("pending=false"));
    assert!(owned_search_probe(&d).contains("caret=7"));
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

#[test]
fn owned_search_opens_system_ownership_and_empty_down_keeps_editing() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-keyboard");
    session.watching("synthetic-empty-search-profile");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.mounter.search_owned = true;
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);
    frame(&mut d, &mut rig, Route::Search, tick(1), script_key(Key::Ok, tick(1)));
    assert!(d.input.keyboard);
    assert!(owned_search_probe(&d).contains("editing=true"));
    let before = d.focus();
    frame(&mut d, &mut rig, Route::Search, tick(2), script_key(Key::Down, tick(2)));
    assert_eq!(d.focus(), before);
    assert!(d.input.keyboard);
    frame(&mut d, &mut rig, Route::Search, tick(3), script_key(Key::Back, tick(3)));
    assert!(!d.input.keyboard);
    assert!(owned_search_probe(&d).contains("editing=false"));
    assert_eq!(d.nav.tabs.stack.depth(), 1);
    frame(&mut d, &mut rig, Route::Search, tick(4), script_key(Key::Up, tick(4)));
    assert_eq!(d.focus().unwrap().elem, crate::ui::dispatch::STRIP_BASE + 3,
        "UP from the field chooses Search's own shared-strip pill");
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

#[test]
fn owned_search_result_keys_are_server_scoped_and_survive_same_query_reordering() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-results");
    session.watching("synthetic-result-profile");
    // This test grades navigation, not disk workers. The accepted term is already first, so
    // RememberRecent remains the production no-op and no detached save can outlive the fixture.
    crate::plex::session::update(|s| {
        let mut next = s.clone();
        next.set_recents_for("synthetic-result-profile", vec!["synthetic".into()]);
        Some(next)
    });
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    let a = crate::plex::register_for_test("search-a", "127.0.0.1", 1, "a", "search");
    let b = crate::plex::register_for_test("search-b", "127.0.0.1", 2, "b", "search");
    let item = |sid| crate::search::Item::Media(crate::pms::PmsMovie {
        sid, rk: "same-local-key".into(), title: "Synthetic movie".into(), ..Default::default()
    });
    crate::stores::search::apply(crate::stores::search::SearchCmd::SetQuery("synthetic".into()));
    crate::search::publish_shelves_for_test(vec![crate::search::Shelf { kind: crate::search::Kind::Movie,
        items: vec![item(a), item(b)] }]);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.mounter.search_owned = true;
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);
    frame(&mut d, &mut rig, Route::Search, tick(1), script_key(Key::Down, tick(1)));
    let first = d.focus().unwrap();
    frame(&mut d, &mut rig, Route::Search, tick(2), script_key(Key::Right, tick(2)));
    let second = d.focus().unwrap();
    assert_ne!(first, second);
    crate::search::publish_shelves_for_test(vec![crate::search::Shelf { kind: crate::search::Kind::Movie,
        items: vec![item(b), item(a)] }]);
    frame(&mut d, &mut rig, Route::Search, tick(3), vec![]);
    assert_eq!(d.focus(), Some(second), "identity survives the changed catalog order");
    frame(&mut d, &mut rig, Route::Search, tick(4), script_key(Key::Ok, tick(4)));
    for i in 5..35 { frame(&mut d, &mut rig, Route::Search, tick(i), vec![]); }
    assert!(rig.search_reqs.iter().any(|(_, req, _)| matches!(req,
        crate::screens::registry::SearchReq::Detail { sid, rk } if *sid == b && rk == "same-local-key")));
    assert!(!rig.search_reqs.iter().any(|(_, req, _)| matches!(req,
        crate::screens::registry::SearchReq::Detail { sid, .. } if *sid == a)));
    let requests = rig.take_search_reqs();
    assert!(!requests.is_empty());
    assert!(rig.take_search_reqs().is_empty(), "requests execute only once");
    let (_, _, ret) = requests.iter().find(|(_, req, _)| matches!(req,
        crate::screens::registry::SearchReq::Detail { .. })).unwrap();
    let entry = d.nav.top_page().unwrap().id;
    let (selected, opener) = rig.search_selection(&d, entry, ret.focus).unwrap();
    assert!(matches!(selected, crate::search::Item::Media(item) if item.sid == b));
    assert!(opener.rect.is_some(), "the menu anchor belongs to the captured selection");
    let mut foreign = ret.focus.unwrap();
    foreign.entry = EntryId(entry.0 + 100);
    assert!(rig.search_selection(&d, entry, Some(foreign)).is_none());
    frame(&mut d, &mut rig, Route::Search, tick(35), script_key(Key::Back, tick(35)));
    assert!(rig.take_search_reqs().iter().any(|(_, req, _)| matches!(req,
        crate::screens::registry::SearchReq::Back)), "BACK differs from selecting the Home pill");
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset); crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset); crate::plex::reset_servers_for_test();
}

#[test]
fn owned_search_rejects_a_queued_query_from_the_departing_profile() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-profile");
    session.watching("synthetic-departing-profile");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    let old_generation = crate::plex::session::current_gen();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.mounter.search_owned = true;
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);
    d.emit(MachineId::Input, Fx::App(AppFx::Store(StoreId::Search, StoreCmd::Search(
        crate::stores::search::SearchCmd::SetQueryScoped { profile_generation: old_generation, query: "departing text".into() }))));
    session.watching("synthetic-replacement-profile");
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    frame(&mut d, &mut rig, Route::Search, tick(1), vec![]);
    assert_eq!(crate::search::query(), "");
    assert!(owned_search_probe(&d).contains("caret=0"));
    let next_generation = crate::plex::session::current_gen();
    d.emit(MachineId::Input, Fx::App(AppFx::Store(StoreId::Search, StoreCmd::Search(
        crate::stores::search::SearchCmd::SetQueryScoped { profile_generation: next_generation, query: "replacement text".into() }))));
    frame(&mut d, &mut rig, Route::Search, tick(2), vec![]);
    assert_eq!(crate::search::query(), "replacement text");
    frame(&mut d, &mut rig, Route::Search, tick(3), vec![]);
    assert!(owned_search_probe(&d).contains("caret=16"));
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

#[test]
fn owned_search_return_memory_reconstructs_positions_with_a_query_guard() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-return");
    session.watching("synthetic-return-profile");
    crate::plex::reset_servers_for_test(); crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset); crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    let catalog = || vec![crate::search::Shelf { kind: crate::search::Kind::Movie,
        items: (0..8).map(|i| crate::search::Item::Media(crate::pms::PmsMovie {
            rk: format!("item-{i}"), title: format!("Synthetic {i}"), ..Default::default()
        })).collect() }];
    crate::stores::search::apply(crate::stores::search::SearchCmd::SetQuery("memory".into()));
    crate::search::publish_shelves_for_test(catalog());
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.mounter.search_owned = true;
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);
    frame(&mut d, &mut rig, Route::Search, tick(1), script_key(Key::Down, tick(1)));
    for i in 2..9 { frame(&mut d, &mut rig, Route::Search, tick(i), script_key(Key::Right, tick(i))); }
    for i in 9..100 { frame(&mut d, &mut rig, Route::Search, tick(i), vec![]); }
    let ret = d.return_state();
    let key = ret.focus.unwrap();
    let memory = match &ret.memory { PageMemory::Search(memory) => memory, _ => panic!("Search must supply entry memory") };
    let parts = CxParts { tick: tick(100), press: Default::default(), focus: d.input.engine.read(InputOwner::Entry(key.entry)), owner: InputOwner::Entry(key.entry) };
    let old_rect = {
        let cx = parts.cx::<AppHost>(AppViews { hubs: rig.hubs.view(), listing: rig.listing.view(),
            directory: rig.directory.view(), section_hubs: rig.section_hubs.view(), search: rig.search.view() }, rig.measure);
        d.top_screen().unwrap().place(&key.elem, &cx, At::Drawn).unwrap().rest_rect
    };
    assert!(old_rect.x < 1800.0, "the last card must have scrolled into view: {old_rect:?}");
    for changed in [false, true] {
        if changed {
            crate::stores::search::apply(crate::stores::search::SearchCmd::SetQuery("replacement".into()));
            crate::search::publish_shelves_for_test(catalog());
            rig.search = crate::stores::search::snapshot();
        }
        let cx = parts.cx::<AppHost>(AppViews { hubs: rig.hubs.view(), listing: rig.listing.view(),
            directory: rig.directory.view(), section_hubs: rig.section_hubs.view(), search: rig.search.view() }, rig.measure);
        let mut restored = crate::screens::search::SearchScreen::new(key.entry, InstanceId(900));
        restored.restore(memory);
        let mut present = crate::ui::present::Present::new();
        let mut commands = Vec::new();
        let mut fx = Effects::new(&mut commands, MachineId::Instance(InstanceId(900)), &mut present);
        restored.step(&ScreenEvent::Mount, &cx, &mut fx);
        let selected = restored.reconcile(key, &cx);
        if changed {
            assert_ne!(selected, key, "a replacement query cannot reuse the old entry's element ids");
            assert!(restored.place(&key.elem, &cx, At::Drawn).is_none());
        } else {
            assert_eq!(selected, key);
            let rect = restored.place(&key.elem, &cx, At::Drawn).unwrap().rest_rect;
            assert_eq!((rect.x, rect.y, rect.w, rect.h), (old_rect.x, old_rect.y, old_rect.w, old_rect.h));
        }
    }
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}
