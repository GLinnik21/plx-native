fn owned_search_probe(d: &Dispatcher<AppHost>) -> String {
    let screen = d.top_screen().expect("Search mounted");
    assert!(screen.as_any().unwrap().is::<crate::screens::search::SearchScreen>());
    let mut probe = String::new();
    screen.state().probe(&mut probe);
    probe
}

#[test]
fn owned_search_external_departure_never_submits_the_draft() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-external-leave");
    session.watching("synthetic-external-leave");
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::SetQuery("unfinished draft".into()));
    let mut rig = Bridge::for_test(|| 0);
    rig.search = crate::stores::search::snapshot();
    let parts = CxParts { tick: tick(0), press: Default::default(), focus: Default::default(),
        owner: InputOwner::Entry(EntryId(1)) };
    let split = rig.split();
    let cx = parts.cx::<AppHost>(split.views, split.measure);
    for event in [ScreenEvent::Suspend, ScreenEvent::Cover, ScreenEvent::Unmount,
        ScreenEvent::WillLeave(crate::ui::machine::Leave::Deeper)] {
        let mut screen = crate::screens::search::SearchScreen::new(EntryId(1), InstanceId(1));
        let mut out = Vec::new();
        let mut present = Present::new();
        {
            let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(1)), &mut present);
            screen.step(&ScreenEvent::Mount, &cx, &mut fx);
            screen.step(&ScreenEvent::Input(InputEvent { at: tick(0), source: Source::Sdl,
                kind: InputKind::SystemKeyboard(true) }), &cx, &mut fx);
            screen.step(&event, &cx, &mut fx);
        }
        assert!(out.iter().any(|effect| matches!(&effect.fx,
            Fx::Deliver(_, Delivery::Keyboard { up: false }))));
        assert!(!out.iter().any(|effect| matches!(&effect.fx,
            Fx::App(AppFx::Store(StoreId::Search, StoreCmd::Search(
                crate::stores::search::SearchCmd::RememberRecent { .. }))))),
            "external lifecycle events are not search submissions");
    }
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

#[test]
fn owned_search_ticks_request_search_work_once_after_step() {
    let _serial = crate::testlock::serial();
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    let mut rig = Bridge::for_test(|| 0);
    let parts = CxParts { tick: tick(0), press: Default::default(), focus: Default::default(),
        owner: InputOwner::Entry(EntryId(1)) };
    let split = rig.split();
    let cx = parts.cx::<AppHost>(split.views, split.measure);
    let mut screen = crate::screens::search::SearchScreen::new(EntryId(1), InstanceId(1));
    let mut out = Vec::new();
    let mut present = Present::new();
    {
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(1)), &mut present);
        screen.step(&ScreenEvent::Mount, &cx, &mut fx);
        screen.step(&ScreenEvent::Tick(tick(1)), &cx, &mut fx);
    }
    assert_eq!(out.iter().filter(|effect| matches!(&effect.fx,
        Fx::App(AppFx::StoreWork(work)) if work.store() == StoreId::Search)).count(), 1,
        "each owned tick owes the Search debounce/landing pass");
    assert_eq!(out.iter().filter(|effect| matches!(&effect.fx,
        Fx::App(AppFx::StoreWork(crate::stores::StoreWork::BrowseDiscovery)))).count(), 1);
}

#[test]
fn owned_search_wheel_scrolls_without_moving_focus_and_dpad_reveals_again() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-wheel");
    session.watching("synthetic-wheel");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::SetQuery("wheel".into()));
    crate::search::publish_shelves_for_test([crate::search::Kind::Movie, crate::search::Kind::Show,
        crate::search::Kind::Episode].into_iter().map(|kind| crate::search::Shelf { kind,
            items: vec![crate::search::Item::Media(crate::pms::PmsMovie {
                rk: format!("synthetic-{kind:?}"), title: "Synthetic wheel result".into(), ..Default::default()
            })] }).collect());
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);
    let field = d.focus().unwrap();
    let field_y = |d: &Dispatcher<AppHost>, rig: &Bridge| {
        let parts = CxParts { tick: tick(0), press: Default::default(),
            focus: d.input.engine.read(InputOwner::Entry(field.entry)), owner: InputOwner::Entry(field.entry) };
        let cx = parts.cx::<AppHost>(AppViews { hubs: rig.hubs.view(), listing: rig.listing.view(),
            directory: rig.directory.view(), section_hubs: rig.section_hubs.view(), search: rig.search.view(), session: crate::route::idle_session_for_test() }, rig.measure);
        d.top_screen().unwrap().place(&field.elem, &cx, At::Drawn).unwrap().rest_rect.y
    };
    let before = field_y(&d, &rig);
    frame(&mut d, &mut rig, Route::Search, tick(1), vec![InputEvent { at: tick(1), source: Source::Sdl,
        kind: InputKind::Wheel { dy: -1.0 } }]);
    for i in 2..80 { frame(&mut d, &mut rig, Route::Search, tick(i), vec![]); }
    assert_eq!(d.focus(), Some(field));
    assert!(field_y(&d, &rig) < before - 10.0, "wheel motion persists after the next Tick");
    {
        let parts = CxParts { tick: tick(79), press: Default::default(), focus: Default::default(),
            owner: InputOwner::Entry(field.entry) };
        let split = rig.split();
        let cx = parts.cx::<AppHost>(split.views, split.measure);
        let placed = d.top_screen().unwrap().place(&field.elem, &cx, At::Drawn).unwrap();
        assert_eq!(placed.clip.y, crate::ui::widgets::TOP_BAR_BOTTOM,
            "scrolling under the tab track must not leave an active page hit above its floor");
    }
    frame(&mut d, &mut rig, Route::Search, tick(80), script_key(Key::Down, tick(80)));
    frame(&mut d, &mut rig, Route::Search, tick(81), script_key(Key::Up, tick(81)));
    for i in 82..160 { frame(&mut d, &mut rig, Route::Search, tick(i), vec![]); }
    assert_eq!(d.focus(), Some(field));
    assert!((field_y(&d, &rig) - before).abs() < 0.5, "D-pad focus reveals the field again");
    frame(&mut d, &mut rig, Route::Search, tick(160), script_key(Key::Ok, tick(160)));
    frame(&mut d, &mut rig, Route::Search, tick(161), vec![InputEvent { at: tick(161), source: Source::Sdl,
        kind: InputKind::Wheel { dy: -1.0 } }]);
    for i in 162..200 { frame(&mut d, &mut rig, Route::Search, tick(i), vec![]); }
    assert!((field_y(&d, &rig) - before).abs() < 0.5, "the keyboard's editing flow stays parked");
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

#[test]
fn owned_search_dispatch_advances_debounce_once_and_only_while_page_updates() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-debounce");
    session.watching("synthetic-debounce");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::SetQuery("not sent to any server".into()));
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    let step = |ms| Tick { ms, dt_us: 100_000 };
    frame(&mut d, &mut rig, Route::Search, Tick { ms: 0, dt_us: 0 }, vec![]);
    frame(&mut d, &mut rig, Route::Search, step(100), vec![]);
    assert_eq!(crate::search::debounce_elapsed_for_test(), 0.1);
    assert!(crate::search::settling());
    frame(&mut d, &mut rig, Route::Search, step(200), vec![]);
    assert_eq!(crate::search::debounce_elapsed_for_test(), 0.2);
    assert!(crate::search::settling(), "a double pump would already have passed the 250ms debounce");
    frame(&mut d, &mut rig, Route::Search, step(300), vec![]);
    assert!(!crate::search::settling(), "the owned page must release the debounce");
    frame(&mut d, &mut rig, Route::Home, step(400), vec![]);
    crate::stores::search::apply(crate::stores::search::SearchCmd::SetQuery("another pending query".into()));
    for ms in [500, 600, 700, 800] { frame(&mut d, &mut rig, Route::Home, step(ms), vec![]); }
    assert!(crate::search::settling(), "a hidden Search entry must not keep pumping");
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

#[test]
fn owned_search_carried_work_keeps_the_originating_tick_delta() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-carried-pump");
    session.watching("synthetic-carried-pump");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::SetQuery("carried search".into()));
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, Route::Search, Tick { ms: 0, dt_us: 0 }, vec![]);
    for ms in [100, 200] { frame(&mut d, &mut rig, Route::Search, Tick { ms, dt_us: 100_000 }, vec![]); }
    assert!((crate::search::debounce_elapsed_for_test() - 0.2).abs() < 0.000001);
    let instance = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    for _ in 0..(crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST) {
        d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
            Delivery::Screen(ScreenEvent::StoreChanged(StoreId::Browse.ord(), 0))));
    }
    let (_, carried) = frame(&mut d, &mut rig, Route::Search, Tick { ms: 210, dt_us: 10_000 }, vec![]);
    assert!(carried.carried > 0, "the fixture must actually exhaust the bounded drain");
    let (_, drained) = frame(&mut d, &mut rig, Route::Search, Tick { ms: 240, dt_us: 30_000 }, vec![]);
    assert_eq!(drained.carried, 0);
    assert!((crate::search::debounce_elapsed_for_test() - 0.24).abs() < 0.000001,
        "each queued pump keeps its own delta: {}", crate::search::debounce_elapsed_for_test());
    assert!(crate::search::settling(), "240ms must not cross the 250ms debounce");
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

#[test]
fn owned_search_observed_keyboard_dismissal_releases_native_latch_and_reopens() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-observed-dismissal");
    session.watching("synthetic-observed-dismissal");
    crate::plex::reset_servers_for_test();
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);
    let mut events = script_key(Key::Ok, tick(1));
    events.push(InputEvent { at: tick(1), source: Source::Sdl, kind: InputKind::SystemKeyboard(false) });
    frame(&mut d, &mut rig, Route::Search, tick(1), events);
    assert!(!d.input.keyboard);
    assert_eq!(rig.keyboard_calls, [true, false], "the native STARTED latch must also be released");
    frame(&mut d, &mut rig, Route::Search, tick(2), script_key(Key::Ok, tick(2)));
    assert!(d.input.keyboard);
    assert_eq!(rig.keyboard_calls, [true, false, true]);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
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
            directory: rig.directory.view(), section_hubs: rig.section_hubs.view(), search: rig.search.view(), session: crate::route::idle_session_for_test() }, rig.measure);
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
            directory: rig.directory.view(), section_hubs: rig.section_hubs.view(), search: rig.search.view(), session: crate::route::idle_session_for_test() }, rig.measure);
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

/// Legacy `ui/search/mod.rs`'s `the_strip_is_a_zone_with_its_own_cursor` and
/// `the_profile_chip_is_the_bars_leftmost_stop`, on the shared container: Search no longer keeps
/// a strip cursor of its own — `TabContainer` publishes the members and the focus engine walks
/// them — so what is left to grade is the seam. ▲ from the field lands under OUR pill (the one the
/// screen is drawn as selected on), ◀ walks to the profile chip at the left end and cannot
/// underflow it, ▶ comes back onto the first pill, and ▼ off the bar is the field again.
#[test]
fn owned_search_walks_the_shared_strip_to_the_chip_and_back_to_the_field() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-strip");
    session.watching("synthetic-strip");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);
    let field = d.focus().unwrap();
    let base = crate::ui::dispatch::STRIP_BASE;

    frame(&mut d, &mut rig, Route::Search, tick(1), script_key(Key::Up, tick(1)));
    assert_eq!(d.focus().map(|key| key.elem), Some(base + 3),
        "▲ lands under the pill the screen is drawn as selected on");

    for i in 2..10 {
        frame(&mut d, &mut rig, Route::Search, tick(i), script_key(Key::Left, tick(i)));
    }
    assert_eq!(d.focus().map(|key| key.elem), Some(base + 4),
        "◀ walks the pills to the profile chip and stops there rather than wrapping");

    frame(&mut d, &mut rig, Route::Search, tick(10), script_key(Key::Right, tick(10)));
    assert_eq!(d.focus().map(|key| key.elem), Some(base),
        "▶ walks back onto the FIRST pill, the one it left");

    frame(&mut d, &mut rig, Route::Search, tick(11), script_key(Key::Down, tick(11)));
    assert_eq!(d.focus(), Some(field), "▼ off the bar lands on the field, whatever pill it was on");
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

/// What the `/tmp/plxnative-search[=<query>]` boot trigger now does, once the retired legacy
/// screen's own `enter()` no longer exists to seed it: write the query into the STORE before the
/// fresh `SearchScreen`'s first `sync()` ever runs, stand on Search exactly as an interactive
/// arrival does, and let that first sync pick the query up off `AppViews.search` exactly as an
/// interactive keystroke would land there.
///
/// **This drives `dev::scenarios::apply_search_boot_trigger` directly** — the exact function
/// `dev::scenarios::grid_library_search_heroidx_arm`'s `super::read("search")` arm calls with the
/// trigger file's (already-trimmed) content — rather than re-typing its store command by hand as
/// an earlier version of this test did. A dropped route flip, a dropped trail push or a wrong trim
/// inside that function now fails here. **What this still cannot see is the one line above it in
/// that arm itself** (`if let Some(q) = super::read("search")`): driving the real trigger-FILE
/// read needs a full `App`/SDL frame, which this suite does not construct (see `dev.rs`'s own
/// tests for host coverage of `dev::read`'s file-reading half in isolation).
///
/// The draft is `pub(crate)` to `screens::search` alone, so this cannot read it directly — the
/// proof goes through the same observable the wheel/scroll tests already use: a query the draft
/// never held is not a "real query" (`SearchScreen::real_query`), so no published shelf is ever
/// turned into a row. Seeing `rows` non-empty on the very first frame after mount is therefore
/// exactly the same evidence "the draft holds the query" would be, one layer down.
#[test]
fn a_seeded_boot_query_survives_the_freshly_mounted_screens_first_sync() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-boot-seed");
    session.watching("synthetic-boot-seed");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    let mut route = Route::Home;
    let mut trail = super::super::Trail::new();
    crate::dev::scenarios::apply_search_boot_trigger("dune", &mut route, &mut trail);
    assert!(matches!(route, Route::Search), "the trigger must stand on Search, not just seed the query");
    assert_eq!(*trail.top(), super::super::Node::Search,
        "the trigger must push a trail node so BACK behaves like an interactive arrival");
    crate::search::publish_shelves_for_test([crate::search::Kind::Movie].into_iter()
        .map(|kind| crate::search::Shelf { kind, items: vec![crate::search::Item::Media(
            crate::pms::PmsMovie { rk: "synthetic-boot-seed".into(), title: "Dune".into(), ..Default::default() })] })
        .collect());
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    // Mirror the loop's own next step: the first frame mounts on the route the trigger just stood
    // on.
    frame(&mut d, &mut rig, route, tick(0), vec![]);
    let probe = owned_search_probe(&d);
    assert!(probe.contains("rows=1"),
        "the freshly-mounted screen's first sync must have read the seeded query, not an empty one: {probe}");
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

/// The evidence behind retiring `nav.rs`'s `leave_of(Route::Search)` arm and folding it into the
/// `None` bucket beside Detail/Person (`app/mod.rs`'s `every_way_off_search_carries_its_teardown`
/// used to pin the opposite). This is a REAL forward navigation, through `route` alone — exactly
/// what `nav_commit` leaves behind after applying `Nav::Home` — and NOT a hand-fired `ScreenEvent`:
/// no `leave_of`, no call to the retired legacy screen's own `leave()`, no manual `NavOp` is
/// touched anywhere in this test. `sync_page` (called by `frame` on every call, exactly as
/// production's own frame loop
/// does) is the only thing that notices `route` no longer names the mounted page and retires the
/// Search entry — which is the SAME generic `Unmount` delivery every owned page's teardown already
/// rides (Detail's `metadata::clear`, Person's `person::leave`, and now Search's own
/// `SearchScreen::step`, which already answers `ScreenEvent::Unmount` by dropping its keyboard).
///
/// RED-then-GREEN, read literally: on THIS tree (`leave_of(Route::Search)` still calling the
/// retired legacy screen's own `leave()`), this test already goes GREEN, because `d.input.keyboard`
/// is dispatcher-owned state that the legacy call never touches (it flips a separate, doomed
/// `EDITING` static of its own instead, calling `crate::textinput::stop()` directly rather than
/// through `rig.system_keyboard`). That is exactly the fact this test exists to pin down BEFORE
/// `leave_of`'s Search arm is deleted: the generic path already reaches the same real
/// `crate::textinput::stop()` through `Delivery::Keyboard`'s acceptance in `dispatch.rs`
/// (`rig.system_keyboard(false)`), so removing the redundant legacy arm changes no observable
/// behaviour — it deletes a call that was never the one doing the work.
#[test]
fn leaving_owned_search_through_a_real_route_change_releases_its_keyboard() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-real-nav-teardown");
    session.watching("synthetic-real-nav-teardown");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);
    frame(&mut d, &mut rig, Route::Search, tick(1), script_key(Key::Ok, tick(1)));
    assert!(d.input.keyboard, "OK on the field opens the television's keyboard");
    assert_eq!(rig.keyboard_calls, [true]);
    // The real navigation: nothing here names `leave_of`, `forward_leave` or the retired legacy
    // screen's own `leave()` — only the route argument moves, exactly as it does in
    // `app::run::nav_commit` after `Nav::Home` commits.
    frame(&mut d, &mut rig, Route::Home, tick(2), vec![]);
    assert!(!d.input.keyboard,
        "the generic Unmount lifecycle must dismiss Search's keyboard with no bespoke teardown");
    assert_eq!(rig.keyboard_calls, [true, false]);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}

/// **Regression: Search wears the shared top bar (`nav::route_wears_tab_bar(Route::Search)` is
/// `true`) but `Bridge::draw_chrome` used to return before painting it**, because its guard only
/// matched `Some(Route::Home | Route::Library)`. The bar was still LIVE in every other respect —
/// `capture_chrome` publishes its strip members for Search, `route_wears_tab_bar` routes the
/// dispatcher's chrome pass to it, and a user could walk focus onto the invisible pills and the
/// invisible profile chip — so nothing except the paint itself was missing.
///
/// **This drives `Bridge::draws_chrome_for` directly** — the exact predicate `draw_chrome` calls
/// as its guard — rather than re-deriving the same condition inline: `draw_chrome`'s own body
/// calls into real text measurement (`crate::text::text_width`, off `self.chrome.labels()`'s real
/// strings) with no font loaded on a host test, so a test cannot drive the paint itself end to end
/// and must instead pin the exact decision the guard makes. The fix derives that guard FROM
/// `route_wears_tab_bar` instead of listing routes a second time (the drift this bug was), so a
/// future bar-wearing route added there alone stays covered here too, without anyone updating a
/// second list.
///
/// Observed RED against the shipped `draw_chrome` (a literal `Some(Route::Home | Route::Library)`
/// guard, with no `draws_chrome_for` to call): reproduced by mutating `draws_chrome_for` back to
/// that literal match — the `Route::Search` case below failed, since that guard admits Home and
/// Library only.
#[test]
fn every_route_wearing_the_shared_bar_reaches_the_chrome_paint_guard() {
    for (route, name) in [(Route::Home, "Home"), (Route::Library, "Library"), (Route::Search, "Search")] {
        assert!(route_wears_tab_bar(route), "{name} must still wear the shared bar");
        assert!(
            Bridge::draws_chrome_for(&AppArg::Legacy(route)),
            "{name} wears the shared top bar (route_wears_tab_bar) but Bridge::draw_chrome's own \
             guard (`draws_chrome_for`) would not reach its paint body"
        );
    }
    // The negative control: a page with no bar must still be refused, so this is not a guard that
    // vacuously admits everything.
    assert!(!Bridge::draws_chrome_for(&AppArg::Legacy(Route::Detail)),
        "Detail has no shared bar and must not reach draw_chrome's paint body");
}

/// **Pins the USE SITE of `draws_chrome_for`, not just the predicate.**
///
/// The test above (`every_route_wearing_the_shared_bar_reaches_the_chrome_paint_guard`) drives
/// `Bridge::draws_chrome_for` directly, which pins that *predicate's* answer — but nothing pinned
/// that `Rig::draw_chrome`'s own guard actually CALLS it, rather than re-deriving the same-looking
/// condition inline with a route literal. That is exactly the shape the original bug shipped in
/// (`if !matches!(arg.route(), Some(Route::Home | Route::Library)) { return; }`), and it is the
/// SECOND time this guard drifted from `route_wears_tab_bar` silently — with
/// `#![allow(dead_code)]` on `ui/mod.rs` meaning a stranded `draws_chrome_for` would not even draw
/// a compiler warning. `draw_chrome`'s body calls into real text measurement with no font loaded
/// on a host test, so this cannot be driven end to end here; instead it reads the guard's own
/// source text, the same idiom `diag::scrub`'s `no_log_call_site_interpolates_viewing_content`
/// uses to pin a call-site property no runtime assertion can see.
///
/// Observed RED against the reintroduced bug: reverting only `draw_chrome`'s guard line to the
/// literal `matches!(arg.route(), Some(Route::Home | Route::Library))` (leaving
/// `draws_chrome_for` itself untouched) makes both assertions below fail, while
/// `cargo +nightly test --lib` on that same mutation reports every OTHER test passing — which is
/// the exact blind spot this test exists to close.
#[test]
fn draw_chromes_guard_calls_the_named_predicate_and_names_no_route_literal() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/bridge.rs"),
    ).expect("read bridge.rs");
    let fn_start = src.find("fn draw_chrome(").expect("Bridge::draw_chrome must exist");
    let body_start = src[fn_start..]
        .find('{')
        .map(|i| fn_start + i)
        .expect("draw_chrome must have a body");
    // The guard is the function's very first statement, so the text from the opening brace up to
    // its own early `return;` is the guard alone — no brace balancing needed for the rest of the
    // (much longer) paint body.
    let guard_end = src[body_start..]
        .find("return;")
        .map(|i| body_start + i)
        .expect("draw_chrome must guard with an early return");
    let guard = &src[body_start..guard_end];
    assert!(
        guard.contains("draws_chrome_for(arg)"),
        "draw_chrome's guard must call the named predicate draws_chrome_for(arg) rather than \
         re-deriving the condition inline — got: {guard:?}"
    );
    assert!(
        !guard.contains("Route::"),
        "draw_chrome's guard must not name a Route variant directly — a literal match is exactly \
         how the Search-chrome regression shipped, by drifting silently from \
         route_wears_tab_bar — got: {guard:?}"
    );
}

/// **Pins the `Route::Search` arm of `app::run::update` that keeps the shared top strip's springs
/// and hit rects live while Search is on screen.**
///
/// `SearchScreen::tick` steps only this screen's OWN rows/springs — it never touches the strip's
/// travelling capsules, its scroll spring or the profile chip's unfurl, which all live in
/// `ui::widgets` behind `tab_row_update_with` and are stepped by `Bridge::update_home_chrome`.
/// Home and Library each get an arm in `update` that calls it every frame they are on screen; if
/// Search's mirroring arm (added alongside the owned cutover) were ever deleted, those springs
/// would freeze at whatever the previously-drawn page left them, and `ChromeSnapshot::members`'s
/// published strip rects (read by pointer hit-testing and by focus) would go stale under Search —
/// exactly the regression `search_route_steps_the_shared_strip_so_its_published_rects_do_not_go_stale`
/// (this file, above) proves `update_home_chrome` FIXES once it is actually called.
///
/// That test cannot see the arm itself: it calls `Bridge::update_home_chrome` directly, never
/// `app::run::update`, so it stays green whether or not `update`'s `Route::Search` branch exists
/// at all — which is exactly how this arm could vanish with every test still passing. And
/// `app::run::update` cannot be driven end to end from a host test either: it is `unsafe fn
/// update(app: &mut App, fr: &mut Frame)`, and `App`/`Frame` carry the live SDL/GL window state
/// boot creates — there is no host constructor for either. So, like
/// `draw_chromes_guard_calls_the_named_predicate_and_names_no_route_literal` above (the same
/// shape of gap, one function over), this reads the source text of the arm itself rather than
/// running it.
///
/// Observed RED: commenting out the `else if !host_frozen(&app.pages) && matches!(app.route,
/// Route::Search) { ... }` block in `run.rs` (leaving the Home/Library arms above it and the
/// `search_osc` block below it untouched) makes this test fail with "update must still branch on
/// Route::Search to step the shared strip", while `cargo +nightly test --lib` on that same
/// mutation reports every other test passing — the disabled-arm-passes-every-test blind spot the
/// finding named directly.
#[test]
fn update_still_steps_the_shared_strip_on_search_the_way_home_and_library_do() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/run.rs"),
    ).expect("read run.rs");
    let fn_start = src.find("unsafe fn update(app: &mut App, fr: &mut Frame)")
        .expect("app::run::update must exist with its documented signature");
    // `page_of(app.route)` until restructure phase 10, when the two popover routes it resolved
    // were deleted and `page_of` became the identity function and went with them.
    let needle = "matches!(app.route, Route::Search)";
    let arm_at = src[fn_start..].find(needle).map(|i| fn_start + i).expect(
        "update must still branch on Route::Search to step the shared strip — no arm of this \
         shape was found after fn update's own start",
    );
    let body_start = src[arm_at..].find('{').map(|i| arm_at + i)
        .expect("the Route::Search condition must open a block");
    // Bounded by this arm's OWN closing statement rather than a raw character window, so the
    // search cannot spill into the unrelated `search_osc` block right after it (which mentions
    // neither `update_home_chrome` nor `scoped_motion`) and read that as a false pass.
    let body_end = src[body_start..].find("fr.underlay_moving |= moving;").map(|i| body_start + i)
        .expect("the Route::Search arm must still report underlay motion the way Home/Library do");
    let body = &src[body_start..body_end];
    assert!(
        body.contains("update_home_chrome"),
        "the Route::Search arm must call Bridge::update_home_chrome — without it the shared \
         strip's springs and published hit rects freeze while Search is on screen — got: {body:?}"
    );
}

/// **Finding 1 — the Search focus fingerprint (`app::bridge::content_probe`'s `Route::Search`
/// arm) reports real facts as focus actually moves**, not the empty string it fell through to
/// before that arm existed (the fall-through guard a few lines down, `if
/// !matches!(page.arg, AppArg::Content(_)) { return String::new(); }`, admits neither
/// `AppArg::Legacy(Route::Search)` nor `Route::Home`/`Route::Library`, which is exactly why those
/// two needed their own arms above it and Search did too).
///
/// Drives one mounted `SearchScreen` through the real dispatch (`frame`, the same helper every
/// other owned-Search test in this file uses) with one seeded shelf holding one result, and reads
/// `content_probe` after each move: mount (the field), Down into the shelf's one card, Up back to
/// the field, and Up again onto the shared strip's Search pill — the same four-stop lap
/// `tests/keytable.json`'s Search rows already record for the up/down pair (this test's mount and
/// shelf stops are new coverage that recorded golden never exercised).
///
/// Observed RED before this finding's fix: with no `Route::Search` arm in `content_probe`,
/// EVERY assertion below failed on the empty string — `content_probe(&d, &rig).contains("zone=Field")`
/// is false against `""`. Reproduced directly: reverting `content_probe` to omit the
/// `AppArg::Legacy(Route::Search)` branch (leaving `SearchScreen::probe`/`probe_below` unused)
/// makes this test fail immediately on the very first assertion, at mount, before any key is even
/// pressed — the exact "grades nothing about Search" gap the finding named.
#[test]
fn owned_search_content_probe_reports_zone_row_col_pill_and_card_as_focus_moves() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-fingerprint");
    session.watching("synthetic-fingerprint");
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
    crate::stores::search::apply(crate::stores::search::SearchCmd::SetQuery("fingerprint".into()));
    crate::search::publish_shelves_for_test(vec![crate::search::Shelf { kind: crate::search::Kind::Movie,
        items: vec![crate::search::Item::Media(crate::pms::PmsMovie {
            rk: "fp-result".into(), title: "Fingerprint result".into(), ..Default::default()
        })] }]);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, Route::Search, tick(0), vec![]);

    // Mount seats focus on the field: no shelf/strip position, not editing.
    let on_field = content_probe(&d, &rig);
    assert!(on_field.contains("zone=Field"), "mount must seat focus on the field: {on_field}");
    assert!(on_field.contains(" row=-1 col=-1 recent=-1 pill=-1 card=0 "),
        "field focus carries no shelf or strip position: {on_field}");
    assert!(on_field.contains("editing=0"), "not editing at mount: {on_field}");
    assert!(on_field.contains("below=Results"), "a real query with a shelf draws Results below the field: {on_field}");

    // Down, with no recents remembered, reaches the one seeded shelf's one card directly.
    frame(&mut d, &mut rig, Route::Search, tick(1), script_key(Key::Down, tick(1)));
    for i in 2..10 { frame(&mut d, &mut rig, Route::Search, tick(i), vec![]); }
    let on_shelf = content_probe(&d, &rig);
    assert!(on_shelf.contains("zone=Results"), "Down from the field must reach the shelf: {on_shelf}");
    assert!(on_shelf.contains(" row=0 col=0 recent=-1 pill=-1 card=1 "),
        "the one seeded Movie result sits at (0,0) and is a card, off the strip: {on_shelf}");

    // Up leaves the shelf for the field again.
    frame(&mut d, &mut rig, Route::Search, tick(10), script_key(Key::Up, tick(10)));
    for i in 11..20 { frame(&mut d, &mut rig, Route::Search, tick(i), vec![]); }
    let back_on_field = content_probe(&d, &rig);
    assert!(back_on_field.contains("zone=Field"), "Up from the shelf must return to the field: {back_on_field}");
    assert!(back_on_field.contains(" row=-1 col=-1 "), "the field carries no shelf position: {back_on_field}");

    // Up again, from the field itself, reaches the shared strip's Search pill — the same
    // transition `tests/keytable.json`'s Search "up" row records. With no library tabs seeded
    // here the strip holds only Home (index 0) and Search (index 1), so the pill lands at 1
    // rather than the real 4-slot device's 3 — the field is a POSITION, not a fixed constant,
    // and this asserts it moved off -1 onto the strip's own answer rather than hardcoding a
    // count this test's fixture does not seed.
    frame(&mut d, &mut rig, Route::Search, tick(20), script_key(Key::Up, tick(20)));
    for i in 21..30 { frame(&mut d, &mut rig, Route::Search, tick(i), vec![]); }
    let on_strip = content_probe(&d, &rig);
    assert!(on_strip.contains("zone=Strip"), "Up from the field must reach the shared strip: {on_strip}");
    assert!(on_strip.contains(" row=-1 col=-1 recent=-1 pill=1 card=0 "),
        "the Search pill (Home, Search — no library tabs seeded here) sits at index 1, off this screen's own groups: {on_strip}");

    crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
}
