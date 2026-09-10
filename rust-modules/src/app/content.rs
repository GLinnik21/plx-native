//! Content navigation during the legacy route transition: entries own screens and return state.

use super::*;
use super::run::Frame;
use crate::screens::registry::{AppMsg, ContentArg, ContentReq, HomeHubIdentity, HomeItemIdentity, HomeReq, HomeTab, PageMemory};
use crate::ui::machine::{Delivery, EntryId, Fx, InputOwner, MachineId, NavOp};
use crate::ui::screen::{ReturnState, ScreenEvent};

fn node(arg: ContentArg) -> Option<Node> {
    match arg {
        ContentArg::Detail { sid, rk } => Some(to_detail(sid, &rk)),
        ContentArg::Person { sid, key, guid, name, thumb } =>
            Some(Node::Person { sid, key, guid, name, thumb }),
        ContentArg::Filmography { .. } => None,
    }
}

pub(crate) fn content_requests(app: &mut App, fr: &Frame) {
    home_requests(app);
    library_requests(app);
    search_requests(app);
    // The player's four overlays are surfaces on its own stack, so what they decide reaches the
    // loop the same way every other owned screen's decision does — as requests, drained here,
    // after the dispatcher and before the frame's own arms (`playback::player_requests`).
    let player_reqs = app.bridge.take_player_reqs();
    if !player_reqs.is_empty() {
        super::playback::player_requests(&mut app.player.session, 
            &mut app.adapters.player,
            player_reqs,
            fr.now,
            &mut app.route,
            &app.play_from,
            &mut app.refresh_hubs_at,
            &mut app.trail,
            &mut app.pages,
            &mut app.ok_armed,
            &mut app.input.press,
        );
    }
    // …and the item context menu's committed row, for the same reason and at the same moment: it
    // is a surface on the shared stack, so what it decides reaches the loop as a request. The
    // dispatch flips `app.route`, takes the playback session's `&mut` and writes the trail, none of
    // which a screen may name (§2.1).
    for req in app.bridge.take_item_menu_reqs() {
        unsafe {
            apply_item_action(&mut app.player.session, &mut app.adapters.player, req,
                &mut app.route, &mut app.play_from, &mut app.trail, &mut app.pages,
                &mut app.bridge, &mut app.nav_pending);
        }
    }
    for (source, request, ret) in app.bridge.take_content_reqs() {
        let MachineId::Instance(instance) = source else { continue };
        let Some(entry) = app.pages.nav.entry_of_instance(instance) else { continue };
        // A delayed effect cannot navigate for an instance that has already been covered.
        if app.pages.nav.input_owner() != Some(InputOwner::Entry(entry)) { continue; }
        let page_entry = app.pages.nav.top_page().map(|e| e.id);
        match request {
            ContentReq::Push(arg) => {
                if let Some(target) = node(arg) {
                    nav_open(app.route, target, None, &mut app.nav_pending);
                    freeze_request(app, page_entry, ret);
                }
            }
            ContentReq::Present(arg) => {
                app.pages.nav.next_style = crate::ui::containers::modal::Style::Opaque { snapshot: true };
                app.pages.request_with_return(source, NavOp::Present(AppArg::Content(arg)), ret);
            }
            ContentReq::Back if cancel_content_navigation(app) => {}
            ContentReq::Back if app.pages.nav.is_surface(entry) =>
                app.pages.request_with_return(source, NavOp::Dismiss(entry), ret),
            ContentReq::Back if app.pages.nav.pending_surface().is_some() =>
                app.pages.request_with_return(source, NavOp::Dismiss(app.pages.nav.pending_surface().unwrap()), ret),
            ContentReq::Back => {
                let bar = app.pages.nav.tabs.stack.entries.iter().rev().nth(1)
                    .and_then(|e| e.arg.route()).is_some_and(route_wears_tab_bar);
                nav_req(app.route, Nav::Back { bar }, None, &mut app.nav_pending);
                freeze_request(app, page_entry, ret);
            }
            ContentReq::Play { play, resume_ns } => {
                // The PAGE decided which item; the LOOP performs the request, because
                // `route::request_play` takes the playback session's `&mut` and a screen is only
                // ever shown the frame's publication (§2.2). A refusal (a PMS/native route
                // transition still owns the reducer) leaves the page exactly where it was, which
                // is what the page's own discarded `started` bool used to decide.
                let started = match play {
                    crate::screens::registry::PlayIntent::Item {
                        sid, rk, part, vcodec, acodec, title, context,
                    } => crate::route::request_play(
                        &mut app.player.session, sid, &rk, &part, &vcodec, &acodec, &title, &context,
                    ),
                    crate::screens::registry::PlayIntent::Movie(m) =>
                        crate::route::request_play_movie(&mut app.player.session, m),
                };
                if !started { continue; }
                if let PageMemory::Detail(spot) = &ret.memory {
                    app.trail.set_top_spot(spot.spot.clone());
                }
                let origin = origin_here(app.route, &app.trail);
                start_playback(&mut app.player.session, &mut app.adapters.player, resume_ns, origin,
                    if crate::dev::scenarios::detailplay_forces_headless_hud() { HUD_HEADLESS_MS } else { HUD_LINGER_MS },
                    &mut app.route, &mut app.play_from, &mut app.pages, &mut app.bridge);
                if matches!(app.route, Route::Player) {
                    app.pages.request_with_return(source, NavOp::Push(AppArg::Legacy(app.route)), ret);
                }
            }
            ContentReq::Panel(panel) => {
                // **The SUBJECT is the page's item, and a page need not have one.** A panel that
                // needs `(sid, rk)` — *Also available*, whose store is addressed by it — is
                // refused rather than presented against whatever item happened to land last; a
                // panel that describes the person, or the item the store already holds, is
                // presented from a page with no item at all. `ContentPanel::surface` decides
                // which is which, and answers `None` for the pairing this page cannot offer.
                let subject = match app.pages.nav.entry(entry).map(|e| &e.arg) {
                    Some(AppArg::Content(ContentArg::Detail { sid, rk })) => Some((*sid, rk.clone())),
                    _ => None,
                };
                let subject = subject.as_ref().map(|(sid, rk)| (*sid, rk.as_str()));
                bridge::open_content_panel(&mut app.pages, instance, subject, panel);
            }
            ContentReq::ItemMenu => {
                if let PageMemory::Detail(spot) = &ret.memory {
                    app.trail.set_top_spot(spot.spot.clone());
                }
                // The ROUTE does not move: a surface is presented over the top page and never
                // replaces it, which is the whole of what `Route::ItemMenu { over: MenuHost }`
                // was arranging by hand.
                if let Some(arg) = app.bridge.content_menu_arg(&app.pages, entry, &ret) {
                    bridge::open_item_menu(&mut app.pages, arg);
                    app.input.press.cancel();
                    app.ok_armed = false;
                }
            }
        }
    }
}

/// Home chooses an action and a stable item; only the application performs navigation,
/// playback or legacy-modal work. Recheck the emitting entry before every queued action.
fn home_requests(app: &mut App) {
    for (source, request, ret) in app.bridge.take_home_reqs() {
        let MachineId::Instance(instance) = source else { continue };
        let Some(entry) = app.pages.nav.entry_of_instance(instance) else { continue };
        if app.pages.nav.input_owner() != Some(InputOwner::Entry(entry))
            || !matches!(app.route, Route::Home) { continue; }
        match request {
            HomeReq::FoldToHero => {
                // The screen owns the fold; the input engine owns the hero group's last
                // control. Restore that cursor without introducing a Home-local focus copy.
                let remembered = app.pages.input.engine.remembered_for(entry).into_iter()
                    .find(|(group, _)| *group == crate::ui::machine::GroupId(0)).map(|(_, elem)| elem);
                let focus = remembered.map(|elem| crate::ui::screen::FocusTarget::Elem(
                    crate::ui::machine::FocusKey { entry, elem }))
                    .unwrap_or(crate::ui::screen::FocusTarget::ContainerGroup(crate::ui::machine::GroupId(0)));
                app.pages.emit(MachineId::Nav, Fx::Deliver(source,
                    Delivery::Screen(ScreenEvent::Enter(crate::ui::screen::Enter::Fresh { focus }))));
            }
            HomeReq::Account => chip_activate(app.route, &mut app.pages),
            HomeReq::Tab(tab) => {
                // A pointer may name the last presented map after favourites changed. The
                // stable key must not resurrect a destination the current strip withdrew.
                let kind = match tab { HomeTab::Movies => Some(crate::browse::SecKind::Movie),
                    HomeTab::Shows => Some(crate::browse::SecKind::Show), _ => None };
                if kind.is_some_and(|kind| crate::browse::tab_of_kind(kind).is_none()) { continue; }
                app.trail.reset();
                match tab {
                    HomeTab::Home => { nav_cancel(app.route, &mut app.nav_pending); }
                    HomeTab::Movies => nav_to(app.route, Nav::Library(crate::browse::SecKind::Movie), &mut app.nav_pending),
                    HomeTab::Shows => nav_to(app.route, Nav::Library(crate::browse::SecKind::Show), &mut app.nav_pending),
                    HomeTab::Search => nav_to(app.route, Nav::Search, &mut app.nav_pending),
                }
                freeze_request(app, Some(entry), ret);
            }
            HomeReq::Play { sid, rk, resume_ns } =>
                activate_home_item(app, source, entry, sid, &rk, Some(resume_ns), ret),
            HomeReq::Detail { sid, rk } =>
                activate_home_item(app, source, entry, sid, &rk, None, ret),
            HomeReq::ItemMenu { sid, rk } => {
                let snapshot = crate::pms::hubs_snapshot();
                let Some(item) = home_item(snapshot.view(), sid, &rk)
                    .filter(|item| crate::screens::item_menu::has_actions(item)) else { continue };
                let from_deck = home_menu_from_deck(&ret);
                let opener = app.bridge.home_opener(&app.pages, entry, ret.focus);
                // `from_home: true` is the ONE thing `MenuHost::Home` still decided by phase 10:
                // a navigation out of a menu opened on the root resets the trail (`menu_leave`).
                let arg = bridge::card_menu_arg(item, from_deck, true, entry, ret.focus, opener.rect);
                bridge::open_item_menu(&mut app.pages, arg);
                app.input.press.cancel();
                app.ok_armed = false;
            }
        }
    }
}

fn home_item<'a>(view: crate::pms::HubsView<'a>, sid: crate::plex::ServerId, rk: &str) -> Option<&'a crate::pms::PmsMovie> {
    (0..view.hub_count()).find_map(|i| view.hub(i)?.items.iter()
        .find(|item| crate::plex::same_item((item.sid, &item.rk), (sid, rk))))
}

/// Resolve identity against the retained selected item, never against the current server.
fn search_target(item: &crate::search::Item, request: &crate::screens::registry::SearchReq) -> Option<Node> {
    use crate::screens::registry::SearchReq;
    match (item, request) {
        (crate::search::Item::Media(item), SearchReq::Detail { sid, rk })
            if item.sid == *sid && item.rk == *rk && !rk.is_empty() => Some(to_detail(*sid, rk)),
        (crate::search::Item::Tag(item), SearchReq::Person { sid, key, guid, .. }) => {
            let current = if item.id.is_empty() || item.id == "0" { &item.tag_key } else { &item.id };
            (item.sid == *sid && current == key && !key.is_empty() && item.tag_key == *guid)
                .then(|| Node::Person { sid: *sid, key: current.clone(), guid: item.tag_key.clone(),
                    name: item.name.clone(), thumb: item.thumb.clone() })
        }
        _ => None,
    }
}

fn search_requests(app: &mut App) {
    use crate::screens::registry::SearchReq;
    for (source, request, ret) in app.bridge.take_search_reqs() {
        let MachineId::Instance(instance) = source else { continue };
        let Some(entry) = app.pages.nav.entry_of_instance(instance) else { continue };
        if app.route != Route::Search || app.pages.nav.input_owner() != Some(InputOwner::Entry(entry)) { continue; }
        match &request {
            SearchReq::Back => {
                nav_to(app.route, Nav::Home { focus_pill: None }, &mut app.nav_pending);
                freeze_request(app, Some(entry), ret);
            }
            SearchReq::Tab(tab) => {
                if !app.bridge.search_tab_available(*tab) { continue; }
                let target = match tab {
                    HomeTab::Home => Nav::Home { focus_pill: Some(crate::ui::widgets::Pill::Home) },
                    HomeTab::Movies => Nav::Library(crate::browse::SecKind::Movie),
                    HomeTab::Shows => Nav::Library(crate::browse::SecKind::Show),
                    HomeTab::Search => continue,
                };
                nav_to(app.route, target, &mut app.nav_pending);
                freeze_request(app, Some(entry), ret);
            }
            SearchReq::Account => {
                // The owned screen releases its keyboard before emitting this request. Do not
                // call chip_activate's legacy Search editing-state teardown a second time.
                bridge::open_account_menu(&mut app.pages);
            }
            SearchReq::Detail { .. } | SearchReq::Person { .. } => {
                let Some((item, _)) = app.bridge.search_selection(&app.pages, entry, ret.focus) else { continue };
                let Some(target) = search_target(&item, &request) else { continue };
                nav_open(app.route, target, None, &mut app.nav_pending);
                freeze_request(app, Some(entry), ret);
            }
            SearchReq::ItemMenu { sid, rk } => {
                let Some((crate::search::Item::Media(item), opener)) = app.bridge.search_selection(&app.pages, entry, ret.focus) else { continue };
                if item.sid != *sid || item.rk != *rk || !crate::screens::item_menu::has_actions(&item) { continue; }
                let arg = bridge::card_menu_arg(&item, false, false, entry, ret.focus, opener.rect);
                bridge::open_item_menu(&mut app.pages, arg);
                app.input.press.cancel();
                app.ok_armed = false;
            }
        }
    }
}

#[cfg(test)]
mod search_action_tests {
    use super::*;
    use crate::screens::registry::SearchReq;
    use crate::search::{Item, TagHit};

    #[test]
    fn owned_search_targets_validate_retained_identity_and_use_retained_labels() {
        let _serial = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let a = crate::plex::register_for_test("action-a", "127.0.0.1", 1, "synthetic", "fixture");
        let b = crate::plex::register_for_test("action-b", "127.0.0.1", 2, "synthetic", "fixture");
        let media = Item::Media(crate::pms::PmsMovie { sid: a, rk: "same".into(), ..Default::default() });
        assert!(matches!(search_target(&media, &SearchReq::Detail { sid: a, rk: "same".into() }),
            Some(Node::Detail { sid, rk, .. }) if sid == a && rk == "same"));
        assert!(search_target(&media, &SearchReq::Detail { sid: b, rk: "same".into() }).is_none());
        assert!(search_target(&media, &SearchReq::Detail { sid: a, rk: "old".into() }).is_none());
        let person = |sid, key: &str, guid: &str| SearchReq::Person {
            sid, key: key.into(), guid: guid.into(), name: "stale name".into(), thumb: "stale thumb".into(),
        };
        for id in ["42", "0", ""] {
            let tag = Item::Tag(TagHit { sid: a, id: id.into(), tag_key: "person-guid".into(),
                name: "Current name".into(), thumb: "current-thumb".into(), ..Default::default() });
            let key = if id == "42" { "42" } else { "person-guid" };
            assert!(matches!(search_target(&tag, &person(a, key, "person-guid")),
                Some(Node::Person { sid, name, thumb, .. })
                    if sid == a && name == "Current name" && thumb == "current-thumb"));
            assert!(search_target(&tag, &person(b, key, "person-guid")).is_none());
            assert!(search_target(&tag, &person(a, "old", "person-guid")).is_none());
            assert!(search_target(&tag, &person(a, key, "old-guid")).is_none());
            assert!(search_target(&tag, &SearchReq::Detail { sid: a, rk: key.into() }).is_none());
        }
        assert!(search_target(&Item::Tag(TagHit::default()), &person(Default::default(), "", "")).is_none());
        crate::plex::reset_servers_for_test();
    }
}

fn library_requests(app: &mut App) {
    use crate::screens::registry::{LibraryReq, LibraryMenuArg};
    for (source, request, ret) in app.bridge.take_library_reqs() {
        let MachineId::Instance(instance) = source else { continue };
        let Some(entry) = app.pages.nav.entry_of_instance(instance) else { continue };
        if let LibraryReq::PublishShelves { target, hidden_page, at_head } = request {
            if app.pages.nav.top_page().is_some_and(|page| page.id == entry) && app.route == Route::Library {
                // Apply through the store vocabulary at this boundary. The press check and commit
                // are adjacent; no queued boolean can outlive an input arm created later.
                crate::stores::browse::apply(library_publication_command(&app.pages, target, hidden_page, at_head));
            }
            continue;
        }
        if app.pages.nav.input_owner() != Some(InputOwner::Entry(entry)) || app.route != Route::Library { continue; }
        match request {
            LibraryReq::PublishShelves { .. } => unreachable!(),
            LibraryReq::Menu { kind, anchor, target } => {
                app.pages.nav.next_style = crate::ui::containers::modal::Style::Compact;
                app.pages.request(source, NavOp::Present(AppArg::LibraryMenu(LibraryMenuArg { host: instance, target, kind, anchor })));
            }
            LibraryReq::Account => chip_activate(app.route, &mut app.pages),
            LibraryReq::BackToHome { kind } => {
                nav_to(app.route, Nav::Home { focus_pill: Some(crate::ui::widgets::Pill::Section(kind)) }, &mut app.nav_pending);
                freeze_request(app, Some(entry), ret);
            }
            LibraryReq::Tab(tab) => {
                let nav = match tab {
                    HomeTab::Home => Nav::Home { focus_pill: Some(crate::ui::widgets::Pill::Home) },
                    HomeTab::Movies => Nav::Library(crate::browse::SecKind::Movie),
                    HomeTab::Shows => Nav::Library(crate::browse::SecKind::Show),
                    HomeTab::Search => Nav::Search,
                };
                nav_to(app.route, nav, &mut app.nav_pending);
                freeze_request(app, Some(entry), ret);
            }
            LibraryReq::ItemMenu { sid, rk, from_deck } => {
                let Some((item, opener)) = app.bridge.library_selection(&app.pages, entry, ret.focus) else { continue };
                if item.sid != sid || item.rk != rk || !crate::screens::item_menu::has_actions(&item) { continue; }
                let arg = bridge::card_menu_arg(&item, from_deck, false, entry, ret.focus, opener.rect);
                bridge::open_item_menu(&mut app.pages, arg);
                app.input.press.cancel();
                app.ok_armed = false;
            }
            LibraryReq::Detail { sid, ref rk } | LibraryReq::Play { sid, ref rk, .. } => {
                let Some((mut item, _)) = app.bridge.library_selection(&app.pages, entry, ret.focus) else { continue };
                if item.sid != sid || &item.rk != rk { continue; }
                let play = matches!(request, LibraryReq::Play { .. });
                if let LibraryReq::Play { resume_ns, .. } = request { item.resume_ms = resume_ns / 1_000_000; }
                unsafe { activate_card(&mut app.player.session, &mut app.adapters.player, &item, play, HUD_LINGER_MS, &mut app.route, &mut app.play_from,
                    &app.trail, &mut app.pages, &mut app.bridge, &mut app.nav_pending); }
                freeze_request(app, Some(entry), ret);
            }
        }
    }
}

fn library_publication_command(
    dispatcher: &crate::ui::dispatch::Dispatcher<bridge::AppHost>,
    target: crate::stores::browse::SectionAddress,
    hidden_page: bool,
    at_head: bool,
) -> crate::stores::browse::BrowseCmd {
    crate::stores::browse::BrowseCmd::Addressed { target,
        work: crate::stores::browse::LibraryWork::Hubs {
            may_publish: hidden_page || (at_head && !dispatcher.input.press.is_live()),
        },
    }
}

#[cfg(test)]
mod library_publication_tests {
    use super::*;
    #[test]
    fn a_fresh_arm_at_rest_scale_still_blocks_visible_shelf_publication() {
        let mut dispatcher = crate::ui::dispatch::Dispatcher::<bridge::AppHost>::new();
        let target = crate::stores::browse::SectionAddress { epoch: 1, sid: crate::plex::ServerId::from_raw(0), section: 1 };
        let allowed = |d: &crate::ui::dispatch::Dispatcher<bridge::AppHost>, hidden, head| {
            matches!(library_publication_command(d, target, hidden, head),
                crate::stores::browse::BrowseCmd::Addressed { work: crate::stores::browse::LibraryWork::Hubs { may_publish: true }, .. })
        };
        assert!(allowed(&dispatcher, false, true));
        dispatcher.input.press.begin(0);
        assert_eq!(dispatcher.input.press.scale(), 1.0);
        assert!(!allowed(&dispatcher, false, true));
        assert!(!allowed(&dispatcher, false, false));
        assert!(allowed(&dispatcher, true, false));
        dispatcher.input.press.cancel();
        assert!(allowed(&dispatcher, false, true));
    }
}

fn home_menu_from_deck(ret: &ReturnState<u32, PageMemory>) -> bool {
    let (Some(focus), PageMemory::Home(memory)) = (ret.focus, &ret.memory) else { return false };
    memory.items.iter().any(|key| key.elem == focus.elem && matches!(&key.identity,
        HomeItemIdentity::Item { hub: HomeHubIdentity::ContinueWatching, .. }))
}

#[allow(clippy::too_many_arguments)]
fn activate_home_item(app: &mut App, source: MachineId, entry: EntryId,
    sid: crate::plex::ServerId, rk: &str, resume_ns: Option<i64>, ret: ReturnState<u32, PageMemory>) {
    let snapshot = crate::pms::hubs_snapshot();
    let Some(mut item) = home_item(snapshot.view(), sid, rk).cloned() else { return };
    if let Some(resume_ns) = resume_ns { item.resume_ms = resume_ns.max(0) / 1_000_000; }
    app.trail.reset();
    unsafe { activate_card(&mut app.player.session, &mut app.adapters.player, &item, resume_ns.is_some(), HUD_LINGER_MS, &mut app.route,
        &mut app.play_from, &app.trail, &mut app.pages, &mut app.bridge, &mut app.nav_pending); }
    if matches!(app.route, Route::Player) {
        app.pages.request_with_return(source, NavOp::Push(AppArg::Legacy(app.route)), ret);
    } else {
        freeze_request(app, Some(entry), ret);
    }
}

fn cancel_content_navigation(app: &mut App) -> bool {
    let current = app.nav_pending.as_ref().is_some_and(|r| r.is_current(app.route,
        app.pages.nav.top_page().map(|e| e.id), app.pages.nav.input_owner()));
    current && nav_cancel(app.route, &mut app.nav_pending)
}

fn freeze_request(app: &mut App, entry: Option<EntryId>, ret: ReturnState<u32, PageMemory>) {
    if let Some(request) = &mut app.nav_pending {
        request.entry = entry;
        request.owner = app.pages.nav.input_owner();
        request.spot = match &ret.memory { PageMemory::Detail(s) => Some(s.spot.clone()), _ => None };
        request.ret = Some(ret);
    }
}

/// A legacy context-menu request still leaves an owned page. Capture before its fade advances.
pub(crate) fn capture_content_request(app: &mut App) {
    if let Some(request) = &mut app.nav_pending {
        if request.ret.is_none() {
            let ret = app.pages.return_state();
            request.entry = app.pages.nav.top_page().map(|e| e.id);
            request.owner = app.pages.nav.input_owner();
            request.spot = match &ret.memory { PageMemory::Detail(s) => Some(s.spot.clone()), _ => None };
            request.ret = Some(ret);
        }
    }
}

/// Playback restores the retained origin, then asks that instance to reveal the played episode.
pub(crate) fn restore_played_entry(app: &mut App) {
    let Some(entry) = app.pages.nav.top_page() else { return };
    let AppArg::Content(ContentArg::Detail { sid, rk }) = &entry.arg else { return };
    let Node::Detail { sid: origin_sid, rk: origin_rk, spot } = &app.play_from else { return };
    if sid != origin_sid || rk != origin_rk { return; }
    let Some(instance) = entry.inst.as_ref().map(|i| i.id) else { return };
    let mut spot = spot.clone();
    let episode = crate::metadata::playing().filter(|p| p.sid == *sid)
        .and_then(|_| crate::metadata::now_playing())
        .filter(|n| n.is_episode && n.detail_rk == *rk)
        .map(|n| { spot.season = Some(n.season); crate::route::cur_rk(&app.player.session) });
    if episode.is_some() {
        app.pages.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
            Delivery::Screen(ScreenEvent::App(AppMsg::DetailRestore { spot, episode }))));
    }
}

pub(crate) fn refresh_content(app: &mut App, keep: String) {
    let Some(loaded) = crate::metadata::current() else { return };
    // A write may finish after Person has covered its Detail origin. That origin still
    // owns the metadata and must hear the reconciliation before Back uncovers it.
    let Some(entry) = app.pages.nav.tabs.stack.entries.iter().rev().find(|e| {
        matches!(&e.arg, AppArg::Content(ContentArg::Detail { sid, rk })
            if *sid == loaded.sid && *rk == loaded.rk)
    }) else { return };
    let AppArg::Content(ContentArg::Detail { sid, rk }) = &entry.arg else { return };
    let Some(instance) = entry.inst.as_ref().map(|i| i.id) else { return };
    let memory = if app.pages.nav.top_page().is_some_and(|top| top.id == entry.id) {
        app.pages.return_state().memory
    } else { entry.ret.memory.clone() };
    let PageMemory::Detail(spot) = memory else { return };
    let spot = spot.spot;
    let cmd = crate::stores::metadata::MetadataCmd::RequestDetail { sid: *sid, rk: rk.clone() };
    app.pages.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
        Delivery::Screen(ScreenEvent::App(AppMsg::DetailRestore { spot, episode: (!keep.is_empty()).then_some(keep) }))));
    app.pages.emit(MachineId::Nav, Fx::App(crate::screens::registry::AppFx::Store(
        crate::stores::StoreId::Metadata, crate::stores::StoreCmd::Metadata(cmd))));
}
