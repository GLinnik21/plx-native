//! **The container requests an owned PAGE raises, performed** — Home's, the Library's, Search's,
//! the player overlays' and the item menu's. Each names a destination or an action the screen may
//! not take itself (§2.1); this module turns it into a container op through `app::bridge` or into
//! a call the loop owns. Entries own their screens and their return state, so a push carries the
//! `ReturnState` the asking screen froze on ITS press frame rather than one re-read at the commit.
//!
//! It was "content navigation during the legacy route transition" while there were two navigation
//! systems; phase 12 (D1) left one, and what is here is the application's half of it.

use super::*;
use super::run::Frame;
use crate::screens::registry::{AppMsg, ContentArg, ContentReq, HomeHubIdentity, HomeItemIdentity, HomeReq, HomeTab, PageMemory};
use crate::ui::machine::{Delivery, EntryId, Fx, InputOwner, MachineId, NavOp};
use crate::ui::screen::{ReturnState, ScreenEvent};

// (`node` stood here — `ContentArg` → `ui::trail::Node`, one of the two conversions the trail
// needed. A `ContentArg` IS the page's identity; there is nothing to convert it to.)

/// Drain the real bridge queue after menu activation. Resource injection is below the shared
/// performer, so production and tests consume precisely the same emitted requests.
pub(super) fn drain_item_menu_requests<R: super::playback::PlaybackResources>(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
    resources: &mut R,
) {
    for req in bridge.take_item_menu_reqs() {
        unsafe { super::input::apply_item_action(ps, pa, req, pages, bridge, resources); }
    }
}

struct HeldFeature {
    play: crate::screens::registry::PlayIntent,
    resume_ns: i64,
    /// `Some` for a page navigation's own captured spot (BACK returns there); `None` for an
    /// item-menu-initiated play, which never carried one — see [`hold_feature`].
    ret: Option<ReturnState<u32, PageMemory>>,
    hud_ms: u32,
}

std::thread_local! {
    static HELD_FEATURE: std::cell::RefCell<Option<HeldFeature>> = const { std::cell::RefCell::new(None) };
}

fn halt_preview(app: &mut App) {
    halt_preview_now(&mut app.player.session, &mut app.adapters.player);
}

/// The `&mut App`-free half of [`halt_preview`], for call sites (the item menu's own action
/// dispatch in `app::input`) that only have the playback session and adapter, not the whole
/// frame. Both must start abandonment before checking [`crate::player::preview::occupies`] —
/// see [`hold_feature`]'s doc for why a still-abandoning preview needs a hold rather than a
/// synchronous play.
pub(super) fn halt_preview_now(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
) {
    crate::player::preview::halt(ps, pa);
}

/// Queue a play for [`drain_held_feature`] to perform once a still-abandoning preview's Load
/// thread has actually released the engine. `halt_preview`/`halt_preview_now` only STARTS
/// abandonment — the preview may still hold `pa.engine()` installed for a moment after, and
/// starting a second Load against that installed engine hits the double-start conflict guard
/// and refuses instead of queuing. Every caller that can reach a still-occupied preview
/// (an ordinary page Play, and the item menu's Play Trailer) must hold through here rather
/// than call `request_play`/`start_playback` directly.
pub(super) fn hold_feature(
    play: crate::screens::registry::PlayIntent,
    resume_ns: i64,
    ret: Option<ReturnState<u32, PageMemory>>,
) {
    HELD_FEATURE.with(|slot| {
        *slot.borrow_mut() = Some(HeldFeature {
            play,
            resume_ns,
            ret,
            hud_ms: if crate::dev::scenarios::detailplay_forces_headless_hud() {
                HUD_HEADLESS_MS
            } else {
                HUD_LINGER_MS
            },
        });
    });
}

/// After `request_play` accepts, extras (trailers included) install an Info-card descriptor so
/// the card names the extra rather than the parent. Feature plays leave `now_playing` alone.
fn note_extra_now_playing(sid: crate::plex::ServerId, rk: &str, context: &str) {
    if crate::metadata::context_omits_queue_continuous(context) {
        crate::stores::metadata::apply(
            crate::stores::metadata::MetadataCmd::SetNowPlaying(
                crate::metadata::trailer_now_playing(sid, rk),
            ),
        );
    }
}

pub(super) fn request_play_intent(
    session: &mut crate::route::PlaybackSession,
    play: &crate::screens::registry::PlayIntent,
) -> bool {
    match play {
        crate::screens::registry::PlayIntent::Item {
            sid, rk, part, vcodec, acodec, title, context,
        } => {
            let ok = crate::route::request_play(
                session, *sid, rk, part, vcodec, acodec, title, context,
            );
            if ok {
                note_extra_now_playing(*sid, rk, context);
            }
            ok
        }
        crate::screens::registry::PlayIntent::Movie(m) =>
            crate::route::request_play_movie(session, m),
    }
}

fn drain_held_feature(app: &mut App) {
    if crate::player::preview::occupies() {
        return;
    }
    let held = HELD_FEATURE.with(|slot| slot.borrow_mut().take());
    let Some(held) = held else { return };
    if !request_play_intent(&mut app.player.session, &held.play) {
        return;
    }
    start_playback(
        &mut app.player.session,
        &mut app.adapters.player,
        held.resume_ns,
        super::playback::Origin::Here,
        held.hud_ms,
        held.ret,
        &mut app.pages,
        &mut app.bridge,
    );
}

#[cfg(test)]
mod held_feature_tests {
    use super::*;
    use crate::screens::registry::PlayIntent;

    fn parent_with_extra() -> crate::metadata::Detail {
        crate::metadata::Detail {
            sid: crate::plex::ServerId::UNSET,
            rk: "movie".into(),
            kind: "movie".into(),
            title: "Movie".into(),
            extras: vec![crate::metadata::Extra {
                rk: "9".into(),
                title: "Official Trailer".into(),
                dur_ms: 120_000,
                part: "/p".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn leftover_episode_now_playing() -> crate::metadata::NowPlaying {
        crate::metadata::NowPlaying {
            is_episode: true,
            is_real_episode: true,
            title: "Show".into(),
            ep_title: "Pilot".into(),
            season: 1,
            index: 1,
            summary: String::new(),
            year: 2020,
            dur_ms: 1_800_000,
            rating: String::new(),
            thumb: String::new(),
            detail_rk: "show".into(),
        }
    }

    fn trailer_play(context: &str) -> PlayIntent {
        PlayIntent::Item {
            sid: crate::plex::ServerId::UNSET,
            rk: "9".into(),
            part: "/p".into(),
            vcodec: String::new(),
            acodec: String::new(),
            title: "Official Trailer".into(),
            context: context.into(),
        }
    }

    fn install_held(play: PlayIntent) {
        hold_feature(play, 0, Some(ReturnState::default()));
    }

    /// The post-accept half of [`request_play_intent`] / [`drain_held_feature`]. A full
    /// `request_play` would leave the process-wide player in Resolving and poison parallel tests.
    fn drain_held_play_now_playing() {
        if crate::player::preview::occupies() {
            return;
        }
        let Some(held) = HELD_FEATURE.with(|slot| slot.borrow_mut().take()) else { return };
        if let PlayIntent::Item { sid, rk, context, .. } = &held.play {
            note_extra_now_playing(*sid, rk, context);
        }
    }

    #[test]
    fn a_held_trailer_play_installs_now_playing_for_the_info_card() {
        let _g = crate::testlock::serial();
        crate::metadata::set_current_for_test(Some(parent_with_extra()));
        crate::stores::metadata::apply(
            crate::stores::metadata::MetadataCmd::SetNowPlaying(Some(leftover_episode_now_playing())),
        );
        install_held(trailer_play(crate::metadata::TRAILER_CONTEXT));
        drain_held_play_now_playing();
        let np = crate::metadata::now_playing().expect("held trailer Play must install NowPlaying");
        assert!(!np.is_episode);
        assert_eq!(np.title, "Movie");
        assert_eq!(np.ep_title, "Official Trailer");
        assert_eq!(np.dur_ms, 120_000);
        assert_eq!(np.detail_rk, "movie");
        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::SetNowPlaying(None));
        crate::metadata::set_current_for_test(None);
    }

    #[test]
    fn a_held_show_trailer_play_labels_the_info_card_with_the_extra() {
        let _g = crate::testlock::serial();
        crate::metadata::set_current_for_test(Some(crate::metadata::Detail {
            sid: crate::plex::ServerId::UNSET,
            rk: "show".into(),
            kind: "show".into(),
            is_show: true,
            title: "Show".into(),
            extras: vec![crate::metadata::Extra {
                rk: "9".into(),
                title: "Official Trailer".into(),
                dur_ms: 90_000,
                part: "/p".into(),
                ..Default::default()
            }],
            ..Default::default()
        }));
        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::SetNowPlaying(None));
        install_held(trailer_play(crate::metadata::TRAILER_CONTEXT));
        drain_held_play_now_playing();
        let np = crate::metadata::now_playing().expect("held show trailer Play must install NowPlaying");
        assert!(np.is_episode, "a show parent labels Go to Show");
        assert_eq!(np.title, "Show");
        assert_eq!(np.ep_title, "Official Trailer");
        assert_eq!(np.dur_ms, 90_000);
        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::SetNowPlaying(None));
        crate::metadata::set_current_for_test(None);
    }

    #[test]
    fn a_held_extra_play_installs_now_playing_the_same_way() {
        let _g = crate::testlock::serial();
        crate::metadata::set_current_for_test(Some(parent_with_extra()));
        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::SetNowPlaying(None));
        install_held(trailer_play(crate::metadata::EXTRA_CONTEXT));
        drain_held_play_now_playing();
        assert_eq!(
            crate::metadata::now_playing().map(|n| n.ep_title.as_str()),
            Some("Official Trailer"),
        );
        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::SetNowPlaying(None));
        crate::metadata::set_current_for_test(None);
    }

    #[test]
    fn a_held_feature_play_does_not_replace_now_playing() {
        let _g = crate::testlock::serial();
        crate::metadata::set_current_for_test(Some(parent_with_extra()));
        crate::stores::metadata::apply(
            crate::stores::metadata::MetadataCmd::SetNowPlaying(Some(leftover_episode_now_playing())),
        );
        install_held(trailer_play(""));
        drain_held_play_now_playing();
        assert!(
            crate::metadata::now_playing().is_some_and(|n| n.detail_rk == "show"),
            "ordinary Play must not install a trailer descriptor"
        );
        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::SetNowPlaying(None));
        crate::metadata::set_current_for_test(None);
    }

    /// [`HeldFeature::ret`] widened to `Option<ReturnState<..>>` so `app::input::apply_item_action`
    /// (which has no captured return spot — an item-menu play, unlike a page's own `ContentReq::Play`,
    /// never had one) can hold a play too, alongside a page navigation's real captured `Some(ret)`.
    /// A `None` must round-trip as `None`, not get coerced into a synthesized `Some(default())` —
    /// the two reach different `enter_player` branches (`nav_push` vs `nav_push_with_return`), and
    /// swapping one for the other would seed a BACK-return spot no item-menu play ever had.
    #[test]
    fn a_none_ret_round_trips_through_the_held_queue_unchanged() {
        let _g = crate::testlock::serial();
        hold_feature(trailer_play(crate::metadata::TRAILER_CONTEXT), 0, None);
        let held_ret_is_none = HELD_FEATURE.with(|slot| slot.borrow().as_ref().map(|h| h.ret.is_none()));
        assert_eq!(
            held_ret_is_none,
            Some(true),
            "an item-menu hold must carry no return state at all"
        );
        HELD_FEATURE.with(|slot| { slot.borrow_mut().take(); });
    }
}

pub(crate) fn content_requests(app: &mut App, fr: &Frame) {
    drain_held_feature(app);
    home_requests(app, fr.now);
    library_requests(app, fr.now);
    search_requests(app);
    // The player's four overlays are surfaces on its own stack, so what they decide reaches the
    // loop the same way every other owned screen's decision does — as requests, drained here,
    // after the dispatcher and before the frame's own arms (`playback::player_requests`).
    let mut player_reqs = app.bridge.take_player_reqs();
    if remove_replayed_repairs(&mut player_reqs,
        matches!(app.rec, super::recorder::Recplay::Replaying(_)), app.bridge.is_controlled_replay()) {
        app.rec.refuse("replay cannot authorize sandbox repair");
    }
    if !player_reqs.is_empty() {
        super::playback::player_requests(&mut app.player.repair, &mut app.player.session,
            &mut app.adapters.player,
            player_reqs,
            fr.now,
            &mut app.refresh_hubs_at,
            &mut app.pages,
            &mut app.bridge,
            &mut app.ok_armed,
            &mut app.input.press,
            &mut app.repause_at,
        );
    }
    // …and the item context menu's committed row, for the same reason and at the same moment: it
    // is a surface on the shared stack, so what it decides reaches the loop as a request. The
    // dispatch asks the container for a navigation and takes the playback session's `&mut`,
    // neither of which a screen may name (§2.1).
    drain_item_menu_requests(&mut app.player.session, &mut app.adapters.player,
        &mut app.pages, &mut app.bridge, &mut super::playback::LivePlaybackResources);
    for (source, request, ret) in app.bridge.take_content_reqs() {
        let MachineId::Instance(instance) = source else { continue };
        let Some(entry) = app.pages.nav.entry_of_instance(instance) else { continue };
        // A delayed effect cannot navigate for an instance that has already been covered.
        if app.pages.nav.input_owner() != Some(InputOwner::Entry(entry)) { continue; }
        match request {
            ContentReq::Push(arg) => {
                halt_preview(app);
                // The page's own press frame captured `ret`, so it rides the request rather than
                // being re-read at the commit: the user can still move focus during the dip, and
                // BACK must return them to where they pressed.
                bridge::nav_push_with_return(&mut app.pages, AppArg::Content(arg), ret);
            }
            ContentReq::Present(arg) => {
                halt_preview(app);
                app.pages.nav.next_style = crate::ui::containers::modal::Style::Opaque { snapshot: true };
                app.pages.request_with_return(source, NavOp::Present(AppArg::Content(arg)), ret);
            }
            ContentReq::Back if {
                halt_preview(app);
                bridge::nav_cancel(&mut app.pages)
            } => {}
            ContentReq::Back if app.pages.nav.is_surface(entry) =>
                app.pages.request_with_return(source, NavOp::Dismiss(entry), ret),
            ContentReq::Back if app.pages.nav.pending_surface().is_some() =>
                app.pages.request_with_return(source, NavOp::Dismiss(app.pages.nav.pending_surface().unwrap()), ret),
            // The chrome question (`Nav::Back { bar }`) is not asked here any more: the container
            // answers it itself, over the entry a `Pop` would reveal (`NavStack::continuous_for`).
            ContentReq::Back => bridge::nav_pop_with_return(&mut app.pages, ret),
            ContentReq::Play { play, resume_ns } => {
                halt_preview(app);
                if crate::player::preview::occupies() {
                    hold_feature(play, resume_ns, Some(ret));
                    continue;
                }
                // The PAGE decided which item; the LOOP performs the request, because
                // `route::request_play` takes the playback session's `&mut` and a screen is only
                // ever shown the frame's publication (§2.2). A refusal (a PMS/native route
                // transition still owns the reducer) leaves the page exactly where it was, which
                // is what the page's own discarded `started` bool used to decide.
                // Held-feature drain uses this same helper: a trailer Play pressed while a
                // preview still occupies must install the Info-card descriptor too.
                if !request_play_intent(&mut app.player.session, &play) { continue; }
                // The page's own `ReturnState` rides the push, so BACK out of the playback finds
                // the spot the Play was pressed from. It was `Trail::set_top_spot` plus a
                // second, hand-written `NavOp::Push` after the fact.
                start_playback(&mut app.player.session, &mut app.adapters.player, resume_ns,
                    super::playback::Origin::Here,
                    if crate::dev::scenarios::detailplay_forces_headless_hud() { HUD_HEADLESS_MS } else { HUD_LINGER_MS },
                    Some(ret), &mut app.pages, &mut app.bridge);
            }
            ContentReq::PreviewStart { sid, rk, part, vcodec, acodec, title } => {
                if part.is_empty() {
                    crate::player::preview::note_no_extra(sid, &rk);
                    continue;
                }
                if !matches!(
                    crate::player::preview::request_start(sid, &rk, fr.now),
                    crate::player::preview::Start::Accepted
                ) {
                    continue;
                }
                let ok = crate::route::request_preview(
                    &mut app.player.session,
                    sid,
                    &rk,
                    &part,
                    &vcodec,
                    &acodec,
                    &title,
                );
                if !ok {
                    crate::player::preview::note_admission_refused();
                }
            }
            ContentReq::PreviewStop => halt_preview(app),
            // Full-trailer mode's transport. Unlike every neighbour here it does NOT halt the
            // preview — it is the one request whose whole point is that the session survives it.
            ContentReq::PreviewTransport(play) => {
                crate::player::preview::transport(
                    &mut app.player.session,
                    &mut app.adapters.player,
                    play,
                );
            }
            ContentReq::Panel(panel) => {
                halt_preview(app);
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
                halt_preview(app);
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
fn home_requests(app: &mut App, now: u32) {
    for (source, request, ret) in app.bridge.take_home_reqs() {
        let MachineId::Instance(instance) = source else { continue };
        let Some(entry) = app.pages.nav.entry_of_instance(instance) else { continue };
        if app.pages.nav.input_owner() != Some(InputOwner::Entry(entry))
            || !matches!(app.route(), AppArg::Home) { continue; }
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
            HomeReq::Account => chip_activate(&mut app.pages),
            HomeReq::Tab(tab) => {
                // A pointer may name the last presented map after favourites changed. The
                // stable key must not resurrect a destination the current strip withdrew.
                if !home_tab_available(app.bridge.browse_directory(), tab) { continue; }
                match tab {
                    // The Home pill ON Home: nothing to navigate to, but a transition queued a
                    // moment ago is still withdrawable, and that is what this press means.
                    HomeTab::Home => { bridge::nav_cancel(&mut app.pages); }
                    other => bridge::nav_tab(&mut app.pages, &mut app.bridge, other, None, Some(ret)),
                }
            }
            HomeReq::Play { sid, rk, resume_ns } =>
                activate_home_item(app, source, entry, sid, &rk, Some(resume_ns), ret, now),
            HomeReq::Detail { sid, rk } =>
                activate_home_item(app, source, entry, sid, &rk, None, ret, now),
            HomeReq::ItemMenu { sid, rk } => {
                let snapshot = app.bridge.hubs_snapshot();
                let Some(item) = home_item(snapshot.view(), sid, &rk)
                    .filter(|item| crate::screens::item_menu::has_actions(item)) else { continue };
                let from_deck = home_menu_from_deck(&ret);
                let opener = app.bridge.home_opener(&app.pages, entry, ret.focus);
                // `from_home: true` is the ONE thing `MenuHost::Home` still decided by phase 10;
                // since D1 it carries no navigation of its own — the container is the only history
                // there is, and a menu opened on the root leaves onto the root's own stack.
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
fn search_target(item: &crate::search::Item, request: &crate::screens::registry::SearchReq) -> Option<AppArg> {
    use crate::screens::registry::SearchReq;
    match (item, request) {
        (crate::search::Item::Media(item), SearchReq::Detail { sid, rk })
            if item.sid == *sid && item.rk == *rk && !rk.is_empty() =>
            Some(AppArg::Content(ContentArg::Detail { sid: *sid, rk: rk.clone() })),
        (crate::search::Item::Tag(item), SearchReq::Person { sid, key, guid, .. }) => {
            let current = if item.id.is_empty() || item.id == "0" { &item.tag_key } else { &item.id };
            (item.sid == *sid && current == key && !key.is_empty() && item.tag_key == *guid)
                .then(|| AppArg::Content(ContentArg::Person { sid: *sid, key: current.clone(),
                    guid: item.tag_key.clone(), name: item.name.clone(), thumb: item.thumb.clone() }))
        }
        _ => None,
    }
}

fn search_requests(app: &mut App) {
    use crate::screens::registry::SearchReq;
    for (source, request, ret) in app.bridge.take_search_reqs() {
        let MachineId::Instance(instance) = source else { continue };
        let Some(entry) = app.pages.nav.entry_of_instance(instance) else { continue };
        if app.route() != AppArg::Search || app.pages.nav.input_owner() != Some(InputOwner::Entry(entry)) { continue; }
        match &request {
            SearchReq::Back => bridge::nav_tab(&mut app.pages, &mut app.bridge, HomeTab::Home, None, Some(ret)),
            SearchReq::Tab(tab) => {
                if !app.bridge.search_tab_available(*tab) { continue; }
                if matches!(tab, HomeTab::Search) { continue }
                bridge::nav_tab(&mut app.pages, &mut app.bridge, *tab,
                    Some(crate::app::chrome::Pill::Home), Some(ret));
            }
            SearchReq::Account => {
                // The owned screen releases its keyboard before emitting this request. Do not
                // call chip_activate's legacy Search editing-state teardown a second time.
                bridge::open_account_menu(&mut app.pages);
            }
            SearchReq::Detail { .. } | SearchReq::Person { .. } => {
                let Some((item, _)) = app.bridge.search_selection(&app.pages, entry, ret.focus) else { continue };
                let Some(target) = search_target(&item, &request) else { continue };
                bridge::nav_push_with_return(&mut app.pages, target, ret);
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
            Some(AppArg::Content(ContentArg::Detail { sid, rk })) if sid == a && rk == "same"));
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
                Some(AppArg::Content(ContentArg::Person { sid, name, thumb, .. }))
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

fn library_requests(app: &mut App, now: u32) {
    use crate::screens::registry::{LibraryReq, LibraryMenuArg};
    for (source, request, ret) in app.bridge.take_library_reqs() {
        let MachineId::Instance(instance) = source else { continue };
        let Some(entry) = app.pages.nav.entry_of_instance(instance) else { continue };
        if let LibraryReq::PublishShelves { target, hidden_page, at_head } = request {
            if app.pages.nav.top_page().is_some_and(|page| page.id == entry) && app.route() == AppArg::Library {
                // Apply through the store vocabulary at this boundary. The press check and commit
                // are adjacent; no queued boolean can outlive an input arm created later.
                app.bridge.browse_run(library_publication_command(
                    &app.pages, target, hidden_page, at_head));
            }
            continue;
        }
        if app.pages.nav.input_owner() != Some(InputOwner::Entry(entry)) || app.route() != AppArg::Library { continue; }
        match request {
            LibraryReq::PublishShelves { .. } => unreachable!(),
            LibraryReq::Menu { kind, anchor, target } => {
                app.pages.nav.next_style = crate::ui::containers::modal::Style::Compact;
                app.pages.request(source, NavOp::Present(AppArg::LibraryMenu(LibraryMenuArg { host: instance, target, kind, anchor })));
            }
            LibraryReq::Account => chip_activate(&mut app.pages),
            LibraryReq::BackToHome { kind } => {
                bridge::nav_tab(&mut app.pages, &mut app.bridge, HomeTab::Home,
                    Some(crate::app::chrome::Pill::Section(kind)), Some(ret));
            }
            LibraryReq::Tab(tab) => {
                bridge::nav_tab(&mut app.pages, &mut app.bridge, tab,
                    Some(crate::app::chrome::Pill::Home), Some(ret));
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
                unsafe { activate_card(&mut app.player.session, &mut app.adapters.player, &item, play, HUD_LINGER_MS,
                    Some(ret), &mut app.pages, &mut app.bridge, &mut app.menu_play_await, now); }
            }
        }
    }
}

fn home_tab_available(
    directory: crate::stores::browse::DirectoryView<'_>,
    tab: HomeTab,
) -> bool {
    let kind = match tab {
        HomeTab::Movies => Some(crate::stores::browse::SecKind::Movie),
        HomeTab::Shows => Some(crate::stores::browse::SecKind::Show),
        _ => None,
    };
    kind.is_none_or(|kind| directory.tab_of_kind(kind).is_some())
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

    fn frame(
        pages: &mut crate::ui::dispatch::Dispatcher<bridge::AppHost>,
        rig: &mut bridge::Bridge,
        frame_no: &mut u32,
    ) {
        let (_, report) = bridge::frame(pages, rig, crate::ui::machine::Tick {
            ms: *frame_no * 16,
            dt_us: 16_000,
        }, Vec::new());
        *frame_no += 1;
        pages.prune(&report.unmounted);
    }

    fn seed_detail_return(
        pages: &mut crate::ui::dispatch::Dispatcher<bridge::AppHost>,
        entry: EntryId,
        sid: crate::plex::ServerId,
        episode: &str,
    ) -> (crate::metadata::Spot, crate::ui::machine::FocusKey<u32>) {
        let spot = crate::metadata::Spot {
            section: 2,
            col: 3,
            ep_text: true,
            saved_col: [0, 1, 3, 0, 0, 0, 0],
            season: Some(2),
        };
        let focus = crate::ui::machine::FocusKey { entry, elem: 3003 };
        let retained = pages.nav.entry_mut(entry).expect("Detail A remains on the stack");
        retained.ret.focus = Some(focus);
        retained.ret.memory = PageMemory::Detail(crate::screens::registry::DetailMemory {
            spot: spot.clone(),
            keys: vec![crate::screens::registry::DetailKey {
                identity: crate::screens::registry::DetailIdentity::Episode {
                    sid,
                    rk: episode.into(),
                    text: true,
                },
                elem: focus.elem,
            }],
            next_elem: focus.elem + 1,
        });
        (spot, focus)
    }

    fn detail_restore_target(
        pages: &crate::ui::dispatch::Dispatcher<bridge::AppHost>,
        entry: EntryId,
    ) -> Option<(
        crate::metadata::Spot,
        Option<String>,
        crate::screens::registry::DetailRefreshPhase,
    )> {
        pages.nav.entry(entry).and_then(|entry| entry.inst.as_ref())
            .and_then(|instance| instance.screen.as_any())
            .and_then(|screen| screen.downcast_ref::<crate::screens::detail::DetailScreen>())
            .and_then(|screen| screen.restore_target_for_test()
                .map(|(spot, episode)| (spot, episode, screen.refresh_for_test())))
    }

    fn detail_refresh_phase(
        pages: &crate::ui::dispatch::Dispatcher<bridge::AppHost>,
        entry: EntryId,
    ) -> crate::screens::registry::DetailRefreshPhase {
        pages.nav.entry(entry).and_then(|entry| entry.inst.as_ref())
            .and_then(|instance| instance.screen.as_any())
            .and_then(|screen| screen.downcast_ref::<crate::screens::detail::DetailScreen>())
            .expect("retained Detail instance").refresh_for_test()
    }

    fn detail_return_waiting(
        pages: &crate::ui::dispatch::Dispatcher<bridge::AppHost>,
        entry: EntryId,
    ) -> Option<bool> {
        pages.nav.entry(entry).and_then(|entry| entry.inst.as_ref())
            .and_then(|instance| instance.screen.as_any())
            .and_then(|screen| screen.downcast_ref::<crate::screens::detail::DetailScreen>())
            .map(crate::screens::detail::DetailScreen::return_waiting_for_test)
    }

    struct SettleDetailBeforeRestoredEnter {
        sid: crate::plex::ServerId,
        generation: u32,
        detail: Option<crate::metadata::Detail>,
        expected_fresh: bool,
        landed: bool,
    }

    impl crate::ui::dispatch::Tap<bridge::AppHost> for SettleDetailBeforeRestoredEnter {
        fn effect(&mut self, _frame: u64, stamped: &crate::ui::machine::Stamped<bridge::AppHost>) {
            if self.landed || !matches!(&stamped.fx,
                Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(
                    crate::ui::screen::Enter::Restored)))) { return; }
            self.landed = true;
            let fresh = crate::metadata::land_detail_for_test(
                self.sid,
                "detail-a",
                self.generation,
                self.detail.take(),
            );
            assert_eq!(fresh, self.expected_fresh);
            assert_eq!(crate::metadata::detail_request_status(self.sid, "detail-a"), Some(false));
        }
    }

    fn drain_detail_workers() {
        for _ in 0..100 {
            crate::stores::metadata::pump_detail();
            std::thread::yield_now();
        }
    }

    fn detail_with_episode(
        sid: crate::plex::ServerId,
        rk: &str,
        title: &str,
        watched: bool,
    ) -> crate::metadata::Detail {
        crate::metadata::Detail {
            sid,
            rk: rk.into(),
            title: title.into(),
            watched,
            is_show: true,
            kind: "show".into(),
            seasons: vec![crate::metadata::Season {
                rk: "season-2".into(),
                index: 2,
                title: "Season 2".into(),
                leaf_count: 1,
                viewed_leaf_count: 0,
            }],
            episodes: vec![crate::metadata::Episode {
                rk: "episode-a".into(),
                index: 1,
                season: 2,
                title: "Episode A".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn home_tab_validation_reads_the_supplied_directory() {
        let sid = crate::plex::ServerId::from_raw(3);
        let directory = crate::stores::browse::DirectorySnapshot::fixture(7, 0, vec![
            crate::stores::browse::SectionView {
                borrowed: false,
                sid: Some(sid),
                key: 11,
                kind: crate::stores::browse::SecKind::Movie,
                row: crate::stores::browse::SrcRow {
                    section: 0,
                    title: "Movies".into(),
                    pinned: true,
                    current: true,
                    ..Default::default()
                },
            },
        ]);

        assert!(home_tab_available(directory.view(), HomeTab::Movies));
        assert!(!home_tab_available(directory.view(), HomeTab::Shows));
        assert!(home_tab_available(directory.view(), HomeTab::Home));
    }

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

    #[test]
    fn deferred_viewstate_refresh_cannot_retarget_the_page_navigated_to_later() {
        let sid = crate::plex::ServerId::from_raw(2);
        let target = crate::stores::viewstate::DetailRefresh {
            sid,
            rk: "origin".into(),
            keep: Some("episode".into()),
        };
        let origin = AppArg::Content(ContentArg::Detail { sid, rk: "origin".into() });
        let later = AppArg::Content(ContentArg::Detail { sid, rk: "later".into() });

        assert!(detail_refresh_matches(&origin, &target));
        assert!(!detail_refresh_matches(&later, &target),
            "landing after navigation must still address the Detail that emitted the write");
    }

    #[test]
    fn a_covered_detail_refresh_waits_until_its_page_owns_metadata_again() {
        let _guard = crate::testlock::serial();
        let sid = crate::plex::ServerId::UNSET;
        let a = AppArg::Content(ContentArg::Detail { sid, rk: "detail-a".into() });
        let b = AppArg::Content(ContentArg::Detail { sid, rk: "detail-b".into() });
        let mut pages = crate::ui::dispatch::Dispatcher::<bridge::AppHost>::new();
        let mut rig = bridge::Bridge::for_test(|| 0);
        let mut frame_no = 0;

        bridge::show_page(&mut pages, a.clone());
        frame(&mut pages, &mut rig, &mut frame_no);
        let a_entry = pages.nav.top_page().expect("Detail A mounted").id;
        bridge::nav_push(&mut pages, b.clone());
        frame(&mut pages, &mut rig, &mut frame_no);
        assert!(pages.nav.top_page().is_some_and(|entry| entry.arg == b));
        let (spot, focus) = seed_detail_return(&mut pages, a_entry, sid, "episode-a");

        // Retire the constructors' real worker requests, then hold B in the same global metadata
        // slot the product uses while A's ViewState completion arrives.
        drain_detail_workers();
        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
        crate::metadata::set_current_for_test(Some(crate::metadata::Detail {
            sid,
            rk: "detail-b".into(),
            title: "Visible B".into(),
            ..Default::default()
        }));
        let b_request = crate::metadata::begin_detail_for_test(sid, "detail-b");

        refresh_content(&mut pages, crate::stores::viewstate::DetailRefresh {
            sid,
            rk: "detail-a".into(),
            keep: Some("episode-a".into()),
        });
        frame(&mut pages, &mut rig, &mut frame_no);

        assert_eq!(crate::metadata::current().map(|detail| detail.rk.as_str()), Some("detail-b"),
            "the visible Detail keeps its loaded metadata");
        assert_eq!(crate::metadata::detail_request_status(sid, "detail-b"), Some(true),
            "covered A must not supersede B's in-flight request");
        assert_eq!(crate::metadata::detail_request_status(sid, "detail-a"), None,
            "A's fetch stays deferred while B owns the shared slot");
        assert_eq!(detail_restore_target(&pages, a_entry),
            Some((spot.clone(), Some("episode-a".into()),
                crate::screens::registry::DetailRefreshPhase::Deferred)),
            "covered A retains the addressed episode and focus spot");

        assert!(crate::metadata::land_detail_for_test(sid, "detail-b", b_request,
            Some(crate::metadata::Detail {
                sid,
                rk: "detail-b".into(),
                title: "Visible B".into(),
                ..Default::default()
            })));
        let generation = crate::metadata::detail_generation_for_test();
        let ret = pages.return_state();
        bridge::nav_pop_with_return(&mut pages, ret);
        frame(&mut pages, &mut rig, &mut frame_no);

        assert!(pages.nav.top_page().is_some_and(|entry| entry.id == a_entry && entry.arg == a));
        assert!(crate::metadata::detail_request_status(sid, "detail-a").is_some(),
            "A starts its deferred metadata refresh only after Back uncovers it");
        assert_eq!(crate::metadata::detail_generation_for_test(), generation + 2,
            "B closes its request and A starts exactly one replacement request");
        assert_eq!(detail_restore_target(&pages, a_entry),
            Some((spot, Some("episode-a".into()),
                crate::screens::registry::DetailRefreshPhase::Requested)),
            "RestoreMemory must not overwrite the addressed episode/focus intent");
        assert_eq!(pages.focus(), Some(focus),
            "the restored engine focus remains on the addressed episode row");
        let generation = crate::metadata::detail_generation_for_test();
        frame(&mut pages, &mut rig, &mut frame_no);
        assert_eq!(crate::metadata::detail_generation_for_test(), generation,
            "the deferred refresh is consumed once");

        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
        crate::metadata::set_current_for_test(None);
        drain_detail_workers();
    }

    #[test]
    fn a_detail_covered_by_person_refreshes_even_when_its_metadata_is_still_loaded() {
        let _guard = crate::testlock::serial();
        let sid = crate::plex::ServerId::UNSET;
        let a = AppArg::Content(ContentArg::Detail { sid, rk: "detail-a".into() });
        let person = AppArg::Content(ContentArg::Person {
            sid,
            key: "person".into(),
            guid: "person-guid".into(),
            name: "Person".into(),
            thumb: String::new(),
        });
        let mut pages = crate::ui::dispatch::Dispatcher::<bridge::AppHost>::new();
        let mut rig = bridge::Bridge::for_test(|| 0);
        let mut frame_no = 0;

        bridge::show_page(&mut pages, a.clone());
        frame(&mut pages, &mut rig, &mut frame_no);
        let a_entry = pages.nav.top_page().expect("Detail A mounted").id;
        bridge::nav_push(&mut pages, person.clone());
        frame(&mut pages, &mut rig, &mut frame_no);
        assert!(pages.nav.top_page().is_some_and(|entry| entry.arg == person));
        let (spot, focus) = seed_detail_return(&mut pages, a_entry, sid, "episode-a");

        drain_detail_workers();
        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
        crate::metadata::set_current_for_test(Some(crate::metadata::Detail {
            sid,
            rk: "detail-a".into(),
            title: "Covered A".into(),
            watched: true,
            ..Default::default()
        }));
        let stale_request = crate::metadata::begin_detail_for_test(sid, "detail-a");
        let generation = crate::metadata::detail_generation_for_test();
        assert_eq!(generation, stale_request);

        refresh_content(&mut pages, crate::stores::viewstate::DetailRefresh {
            sid,
            rk: "detail-a".into(),
            keep: Some("episode-a".into()),
        });
        frame(&mut pages, &mut rig, &mut frame_no);

        assert!(pages.nav.top_page().is_some_and(|entry| entry.arg == person));
        assert_eq!(crate::metadata::current().map(|detail| detail.rk.as_str()), Some("detail-a"),
            "the covered Detail's stale-but-visible slot is not displaced under Person");
        assert_eq!(crate::metadata::detail_generation_for_test(), generation,
            "A must not refresh while Person owns the visible page");
        assert_eq!(crate::metadata::detail_request_status(sid, "detail-a"), Some(true),
            "the pre-write A fetch remains in flight while its page is covered");
        assert_eq!(detail_restore_target(&pages, a_entry),
            Some((spot.clone(), Some("episode-a".into()),
                crate::screens::registry::DetailRefreshPhase::Deferred)));

        let ret = pages.return_state();
        bridge::nav_pop_with_return(&mut pages, ret);
        frame(&mut pages, &mut rig, &mut frame_no);

        assert!(pages.nav.top_page().is_some_and(|entry| entry.id == a_entry && entry.arg == a));
        assert_eq!(crate::metadata::detail_generation_for_test(), generation + 1,
            "uncovered A supersedes the pre-write fetch with exactly one reconciliation");
        let reconciliation = crate::metadata::detail_generation_for_test();
        assert!(!crate::metadata::land_detail_for_test(sid, "detail-a", stale_request,
            Some(crate::metadata::Detail {
                sid,
                rk: "detail-a".into(),
                title: "Stale pre-write A".into(),
                watched: false,
                ..Default::default()
            })), "the superseded pre-write landing must be rejected");
        assert!(crate::metadata::current().is_some_and(|detail|
            detail.rk == "detail-a" && detail.watched),
            "the stale landing cannot undo A's optimistic watched state");
        assert_eq!(crate::metadata::detail_generation_for_test(), reconciliation,
            "rejecting the stale landing cannot replace the reconciliation generation");
        assert_eq!(detail_restore_target(&pages, a_entry),
            Some((spot, Some("episode-a".into()),
                crate::screens::registry::DetailRefreshPhase::Requested)),
            "RestoreMemory preserves the ViewState episode/focus intent");
        assert_eq!(pages.focus(), Some(focus));
        let generation = crate::metadata::detail_generation_for_test();
        frame(&mut pages, &mut rig, &mut frame_no);
        assert_eq!(crate::metadata::detail_generation_for_test(), generation,
            "the explicit pending refresh is consumed after one request");

        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
        crate::metadata::set_current_for_test(None);
        drain_detail_workers();
    }

    #[test]
    fn a_requested_detail_refresh_retries_after_a_child_supersedes_its_request() {
        requested_refresh_after_child(false, false);
    }

    #[test]
    fn directional_related_navigation_preserves_the_refresh_obligation_after_back() {
        requested_refresh_after_child(true, false);
    }

    #[test]
    fn visible_refresh_starts_before_queued_tick_or_store_change_can_consume_it() {
        requested_refresh_after_child(true, true);
    }

    fn requested_refresh_after_child(navigate: bool, settled_before_refresh: bool) {
        let _guard = crate::testlock::serial();
        let sid = crate::plex::ServerId::UNSET;
        let a = AppArg::Content(ContentArg::Detail { sid, rk: "detail-a".into() });
        let b = AppArg::Content(ContentArg::Detail { sid, rk: "detail-b".into() });
        let mut pages = crate::ui::dispatch::Dispatcher::<bridge::AppHost>::new();
        let mut rig = bridge::Bridge::for_test(|| 0);
        let mut frame_no = 0;

        bridge::show_page(&mut pages, a.clone());
        frame(&mut pages, &mut rig, &mut frame_no);
        let a_entry = pages.nav.top_page().expect("Detail A mounted").id;

        drain_detail_workers();
        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
        let mut detail = detail_with_episode(sid, "detail-a", "Old A", true);
        detail.related.push(crate::pms::PmsMovie {
            sid, rk: "detail-b".into(), title: "Related B".into(), ..Default::default()
        });
        detail.related.push(crate::pms::PmsMovie {
            sid, rk: "detail-c".into(), title: "Related C".into(), ..Default::default()
        });
        crate::metadata::set_current_for_test(Some(detail));
        pages.store_changed(crate::stores::StoreId::Metadata.ord(), 1);
        frame(&mut pages, &mut rig, &mut frame_no);

        let PageMemory::Detail(memory) = pages.return_state().memory else {
            panic!("Detail A publishes Detail memory");
        };
        let focus = crate::ui::machine::FocusKey {
            entry: a_entry,
            elem: memory.keys.iter().find_map(|key| matches!(&key.identity,
                crate::screens::registry::DetailIdentity::Episode { rk, text: true, .. }
                if rk == "episode-a").then_some(key.elem))
                .expect("the loaded episode text row has a stable key"),
        };
        pages.set_focus(Some(focus));
        let PageMemory::Detail(memory) = pages.return_state().memory else { unreachable!() };
        let spot = memory.spot;
        assert_eq!((spot.section, spot.col, spot.ep_text, spot.season), (2, 0, true, Some(2)));

        let pre_refresh = crate::metadata::begin_detail_for_test(sid, "detail-a");
        if settled_before_refresh {
            let mut settled = detail_with_episode(sid, "detail-a", "Old A", true);
            settled.related.push(crate::pms::PmsMovie {
                sid, rk: "detail-b".into(), title: "Related B".into(), ..Default::default()
            });
            settled.related.push(crate::pms::PmsMovie {
                sid, rk: "detail-c".into(), title: "Related C".into(), ..Default::default()
            });
            assert!(crate::metadata::land_detail_for_test(
                sid, "detail-a", pre_refresh, Some(settled)));
            assert_eq!(crate::metadata::detail_request_status(sid, "detail-a"), Some(false),
                "the reviewer race begins from a normally settled page");
            let initial = crate::app::bootstrap::Initial::synthetic_home(1, 32498, None).unwrap();
            crate::app::bootstrap::stores::init(&initial, true);
            crate::app::bootstrap::stores::begin([
                serde_json::json!({
                    "content_resource": true,
                    "request": {
                        "store": "metadata", "sid": sid.raw(), "rk": "detail-a",
                        "gen": pre_refresh + 1, "client": null,
                    },
                    "admitted": true,
                }),
            ].into(), Default::default());
        }
        refresh_content(&mut pages, crate::stores::viewstate::DetailRefresh {
            sid,
            rk: "detail-a".into(),
            keep: Some("episode-a".into()),
        });
        if settled_before_refresh {
            struct AtomicStart {
                sid: crate::plex::ServerId,
                saw_restore: bool,
                checked_after_restore: bool,
            }
            impl crate::ui::dispatch::Tap<bridge::AppHost> for AtomicStart {
                fn effect(&mut self, _: u64, stamped: &crate::ui::machine::Stamped<bridge::AppHost>) {
                    if self.saw_restore && !self.checked_after_restore {
                        self.checked_after_restore = true;
                        assert_eq!(crate::metadata::detail_request_status(self.sid, "detail-a"), Some(true),
                            "Requested must not be visible before its reconciliation request exists");
                    }
                    if matches!(&stamped.fx, Fx::Deliver(_, Delivery::Screen(ScreenEvent::App(
                        AppMsg::DetailRestore { refresh: crate::screens::registry::DetailRefreshPhase::Requested, .. })))) {
                        self.saw_restore = true;
                    }
                }
            }
            // These deliveries are already queued when DetailRestore executes. The assertion at
            // the next effect boundary observes the state before either can consume a stale status.
            pages.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(
                pages.nav.top_page().unwrap().inst.as_ref().unwrap().id),
                Delivery::Screen(ScreenEvent::Tick(crate::ui::machine::Tick {
                    ms: frame_no * 16, dt_us: 16_000,
                }))));
            pages.store_changed(crate::stores::StoreId::Metadata.ord(), pre_refresh);
            let mut tap = AtomicStart { sid, saw_restore: false, checked_after_restore: false };
            let (_, report) = bridge::frame_with_tap(&mut pages, &mut rig,
                crate::ui::machine::Tick { ms: frame_no * 16, dt_us: 16_000 }, Vec::new(), &mut tap);
            frame_no += 1;
            pages.prune(&report.unmounted);
            assert!(tap.saw_restore && tap.checked_after_restore);
            let (requests, failure) = crate::app::bootstrap::stores::finish();
            assert_eq!(failure, None);
            assert_eq!(requests.len(), 1,
                "the synchronous start remains one recorder-visible resource admission");
            crate::app::bootstrap::stores::reset_for_test();
        } else {
            frame(&mut pages, &mut rig, &mut frame_no);
        }
        let stale_a = crate::metadata::detail_generation_for_test();
        assert_eq!(crate::metadata::detail_request_status(sid, "detail-a"), Some(true));
        assert_eq!(detail_restore_target(&pages, a_entry),
            Some((spot.clone(), Some("episode-a".into()),
                crate::screens::registry::DetailRefreshPhase::Requested)));

        let mut return_focus = focus;
        if navigate {
            detail_key(&mut pages, &mut rig, &mut frame_no, crate::ui::machine::Key::Down,
                crate::ui::machine::Edge::Down);
            let PageMemory::Detail(memory) = pages.return_state().memory else { unreachable!() };
            let related = memory.keys.iter().find(|key| matches!(&key.identity,
                crate::screens::registry::DetailIdentity::Related { rk, .. } if rk == "detail-b"))
                .expect("Related B is a real focus stop");
            return_focus.elem = related.elem;
            assert_eq!(pages.focus(), Some(return_focus), "DOWN walks from episode text to Related B");
            assert_eq!(detail_restore_target(&pages, a_entry), None,
                "directional input cancels the episode/focus restore intent");
            assert_eq!(detail_refresh_phase(&pages, a_entry),
                crate::screens::registry::DetailRefreshPhase::Requested,
                "directional input leaves the server obligation outstanding");
            detail_key(&mut pages, &mut rig, &mut frame_no, crate::ui::machine::Key::Ok,
                crate::ui::machine::Edge::Down);
            detail_key(&mut pages, &mut rig, &mut frame_no, crate::ui::machine::Key::Ok,
                crate::ui::machine::Edge::Up);
            for _ in 0..20 { frame(&mut pages, &mut rig, &mut frame_no); }
            let requests = rig.take_content_reqs();
            assert_eq!(requests.len(), 1, "the Related press emits exactly one activation");
            let (_, request, ret) = requests.into_iter().next().unwrap();
            let ContentReq::Push(arg) = request else { panic!("Related activation must push Detail B") };
            assert!(AppArg::Content(arg.clone()) == b);
            bridge::nav_push_with_return(&mut pages, AppArg::Content(arg), ret);
        } else {
            bridge::nav_push(&mut pages, b.clone());
        }
        frame(&mut pages, &mut rig, &mut frame_no);
        let stale_b = crate::metadata::detail_generation_for_test();
        assert!(pages.nav.top_page().is_some_and(|entry| entry.arg == b));
        assert_eq!(crate::metadata::detail_request_status(sid, "detail-a"), None,
            "opening B supersedes A's requested reconciliation");
        assert_eq!(crate::metadata::detail_request_status(sid, "detail-b"), Some(true));

        if navigate {
            detail_key(&mut pages, &mut rig, &mut frame_no, crate::ui::machine::Key::Back,
                crate::ui::machine::Edge::Down);
            let requests = rig.take_content_reqs();
            assert_eq!(requests.len(), 1);
            let (_, request, ret) = requests.into_iter().next().unwrap();
            assert!(matches!(request, ContentReq::Back));
            bridge::nav_pop_with_return(&mut pages, ret);
        } else {
            let ret = pages.return_state();
            bridge::nav_pop_with_return(&mut pages, ret);
        }
        frame(&mut pages, &mut rig, &mut frame_no);

        assert!(pages.nav.top_page().is_some_and(|entry| entry.id == a_entry && entry.arg == a));
        assert_eq!(crate::metadata::detail_generation_for_test(), stale_b + 1,
            "Back starts one replacement for the requested reconciliation B superseded");
        let reconciliation = crate::metadata::detail_generation_for_test();
        assert_eq!(crate::metadata::detail_request_status(sid, "detail-a"), Some(true));
        if !navigate {
            assert_eq!(detail_restore_target(&pages, a_entry),
                Some((spot.clone(), Some("episode-a".into()),
                    crate::screens::registry::DetailRefreshPhase::Requested)),
                "the retry preserves the episode and focus intent");
        }
        assert_eq!(pages.focus(), Some(return_focus));
        assert_eq!(detail_refresh_phase(&pages, a_entry),
            crate::screens::registry::DetailRefreshPhase::Requested);

        if navigate {
            // BACK may restore its own navigation snapshot. Cancel that too, while the retry
            // is outstanding, so its eventual landing must complete without ANY restore intent.
            detail_key(&mut pages, &mut rig, &mut frame_no, crate::ui::machine::Key::Right,
                crate::ui::machine::Edge::Down);
            let PageMemory::Detail(memory) = pages.return_state().memory else { unreachable!() };
            return_focus.elem = memory.keys.iter().find_map(|key| matches!(&key.identity,
                crate::screens::registry::DetailIdentity::Related { rk, .. } if rk == "detail-c")
                .then_some(key.elem)).expect("Related C exists");
            assert_eq!(pages.focus(), Some(return_focus));
            assert_eq!(detail_restore_target(&pages, a_entry), None);
        }

        frame(&mut pages, &mut rig, &mut frame_no);
        assert_eq!(crate::metadata::detail_generation_for_test(), reconciliation,
            "the outstanding requested reconciliation is not duplicated");

        assert!(!crate::metadata::land_detail_for_test(sid, "detail-a", stale_a,
            Some(detail_with_episode(sid, "detail-a", "Stale A", false))),
            "A's superseded reconciliation cannot land");
        assert!(!crate::metadata::land_detail_for_test(sid, "detail-a", pre_refresh, None),
            "the pre-refresh request was superseded before its completion");
        assert!(!crate::metadata::land_detail_for_test(sid, "detail-b", stale_b,
            Some(crate::metadata::Detail {
                sid,
                rk: "detail-b".into(),
                title: "Stale B".into(),
                ..Default::default()
            })), "B's stale landing cannot replace restored A");
        assert!(crate::metadata::current().is_some_and(|detail|
            detail.rk == "detail-a" && detail.title == "Old A" && detail.watched));

        drain_detail_workers();
        assert_eq!(crate::metadata::detail_request_status(sid, "detail-a"), Some(false));
        pages.store_changed(crate::stores::StoreId::Metadata.ord(), reconciliation);
        frame(&mut pages, &mut rig, &mut frame_no);
        frame(&mut pages, &mut rig, &mut frame_no);
        assert_eq!(pages.focus(), Some(return_focus), "completion cannot yank focus back to the episode");
        assert_eq!(detail_restore_target(&pages, a_entry), None,
            "the settled reconciliation releases restoration");
        assert_eq!(detail_refresh_phase(&pages, a_entry),
            crate::screens::registry::DetailRefreshPhase::None,
            "reconciliation terminates independently of focus restoration");
        assert_eq!(crate::metadata::detail_generation_for_test(), reconciliation,
            "settling restoration cannot start a duplicate request");

        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
        crate::metadata::set_current_for_test(None);
        drain_detail_workers();
    }

    fn detail_key(
        pages: &mut crate::ui::dispatch::Dispatcher<bridge::AppHost>,
        rig: &mut bridge::Bridge,
        frame_no: &mut u32,
        key: crate::ui::machine::Key,
        edge: crate::ui::machine::Edge,
    ) {
        let at = crate::ui::machine::Tick { ms: *frame_no * 16, dt_us: 16_000 };
        let (_, report) = bridge::frame(pages, rig, at, vec![crate::ui::machine::InputEvent {
            at, source: crate::ui::machine::Source::Script,
            kind: crate::ui::machine::InputKind::Key { key, edge, sym: 0, wcode: 0, at_edge: false },
        }]);
        *frame_no += 1;
        pages.prune(&report.unmounted);
    }

    #[test]
    fn a_requested_detail_refresh_that_settled_while_covered_is_not_retried() {
        let _guard = crate::testlock::serial();
        let sid = crate::plex::ServerId::UNSET;
        let a = AppArg::Content(ContentArg::Detail { sid, rk: "detail-a".into() });
        let person = AppArg::Content(ContentArg::Person {
            sid,
            key: "person".into(),
            guid: "person-guid".into(),
            name: "Person".into(),
            thumb: String::new(),
        });
        let mut pages = crate::ui::dispatch::Dispatcher::<bridge::AppHost>::new();
        let mut rig = bridge::Bridge::for_test(|| 0);
        let mut frame_no = 0;

        bridge::show_page(&mut pages, a.clone());
        frame(&mut pages, &mut rig, &mut frame_no);
        let a_entry = pages.nav.top_page().expect("Detail A mounted").id;

        drain_detail_workers();
        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
        crate::metadata::set_current_for_test(Some(detail_with_episode(
            sid, "detail-a", "Old A", true,
        )));
        pages.store_changed(crate::stores::StoreId::Metadata.ord(), 1);
        frame(&mut pages, &mut rig, &mut frame_no);

        let PageMemory::Detail(memory) = pages.return_state().memory else {
            panic!("Detail A publishes Detail memory");
        };
        let focus = crate::ui::machine::FocusKey {
            entry: a_entry,
            elem: memory.keys.iter().find_map(|key| matches!(&key.identity,
                crate::screens::registry::DetailIdentity::Episode { rk, text: true, .. }
                if rk == "episode-a").then_some(key.elem))
                .expect("the loaded episode text row has a stable key"),
        };
        pages.set_focus(Some(focus));
        let PageMemory::Detail(memory) = pages.return_state().memory else { unreachable!() };
        let spot = memory.spot;

        let pre_refresh = crate::metadata::begin_detail_for_test(sid, "detail-a");
        refresh_content(&mut pages, crate::stores::viewstate::DetailRefresh {
            sid,
            rk: "detail-a".into(),
            keep: Some("episode-a".into()),
        });
        frame(&mut pages, &mut rig, &mut frame_no);
        assert!(!crate::metadata::land_detail_for_test(sid, "detail-a", pre_refresh, None));
        let reconciliation = crate::metadata::begin_detail_for_test(sid, "detail-a");
        assert_eq!(detail_restore_target(&pages, a_entry),
            Some((spot, Some("episode-a".into()),
                crate::screens::registry::DetailRefreshPhase::Requested)));

        bridge::nav_push(&mut pages, person.clone());
        frame(&mut pages, &mut rig, &mut frame_no);
        assert!(pages.nav.top_page().is_some_and(|entry| entry.arg == person));
        assert_eq!(crate::metadata::detail_request_status(sid, "detail-a"), Some(true));

        pages.nav.tabs.stack.transition =
            Box::new(crate::ui::containers::transition::Immediate);
        let mut settle = SettleDetailBeforeRestoredEnter {
            sid,
            generation: reconciliation,
            detail: Some(detail_with_episode(sid, "detail-a", "Refreshed A", true)),
            expected_fresh: true,
            landed: false,
        };
        let ret = pages.return_state();
        bridge::nav_pop_with_return(&mut pages, ret);
        let (_, report) = bridge::frame_with_tap(&mut pages, &mut rig,
            crate::ui::machine::Tick { ms: frame_no * 16, dt_us: 16_000 }, Vec::new(), &mut settle);
        frame_no += 1;
        pages.prune(&report.unmounted);

        assert!(settle.landed, "the reconciliation settles at the restored Enter boundary");
        assert!(pages.nav.top_page().is_some_and(|entry| entry.id == a_entry && entry.arg == a));
        assert_eq!(crate::metadata::detail_generation_for_test(), reconciliation,
            "Enter must not duplicate the reconciliation that already settled");
        assert_eq!(crate::metadata::detail_request_status(sid, "detail-a"), Some(false));
        frame(&mut pages, &mut rig, &mut frame_no);
        assert_eq!(pages.focus(), Some(focus), "the addressed episode focus still restores");
        assert_eq!(detail_restore_target(&pages, a_entry), None,
            "the terminal reconciliation consumes the episode restore intent");
        assert_eq!(detail_return_waiting(&pages, a_entry), Some(false),
            "the return cannot remain waiting after the terminal reconciliation");

        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
        crate::metadata::set_current_for_test(None);
        drain_detail_workers();
    }

    #[test]
    fn a_failed_requested_refresh_with_no_cached_detail_is_not_retried_on_restore() {
        let _guard = crate::testlock::serial();
        let sid = crate::plex::ServerId::UNSET;
        let a = AppArg::Content(ContentArg::Detail { sid, rk: "detail-a".into() });
        let person = AppArg::Content(ContentArg::Person {
            sid,
            key: "person".into(),
            guid: "person-guid".into(),
            name: "Person".into(),
            thumb: String::new(),
        });
        let mut pages = crate::ui::dispatch::Dispatcher::<bridge::AppHost>::new();
        let mut rig = bridge::Bridge::for_test(|| 0);
        let mut frame_no = 0;

        bridge::show_page(&mut pages, a.clone());
        frame(&mut pages, &mut rig, &mut frame_no);
        let a_entry = pages.nav.top_page().expect("Detail A mounted").id;

        drain_detail_workers();
        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
        crate::metadata::set_current_for_test(Some(detail_with_episode(
            sid, "detail-a", "Old A", true,
        )));
        pages.store_changed(crate::stores::StoreId::Metadata.ord(), 1);
        frame(&mut pages, &mut rig, &mut frame_no);

        let PageMemory::Detail(memory) = pages.return_state().memory else {
            panic!("Detail A publishes Detail memory");
        };
        let focus = crate::ui::machine::FocusKey {
            entry: a_entry,
            elem: memory.keys.iter().find_map(|key| matches!(&key.identity,
                crate::screens::registry::DetailIdentity::Episode { rk, text: true, .. }
                if rk == "episode-a").then_some(key.elem))
                .expect("the loaded episode text row has a stable key"),
        };
        pages.set_focus(Some(focus));
        let PageMemory::Detail(memory) = pages.return_state().memory else { unreachable!() };
        let spot = memory.spot;

        let pre_refresh = crate::metadata::begin_detail_for_test(sid, "detail-a");
        refresh_content(&mut pages, crate::stores::viewstate::DetailRefresh {
            sid,
            rk: "detail-a".into(),
            keep: Some("episode-a".into()),
        });
        frame(&mut pages, &mut rig, &mut frame_no);
        assert!(!crate::metadata::land_detail_for_test(sid, "detail-a", pre_refresh, None));
        let reconciliation = crate::metadata::begin_detail_for_test(sid, "detail-a");
        assert_eq!(detail_restore_target(&pages, a_entry),
            Some((spot, Some("episode-a".into()),
                crate::screens::registry::DetailRefreshPhase::Requested)));

        bridge::nav_push(&mut pages, person.clone());
        frame(&mut pages, &mut rig, &mut frame_no);
        assert!(pages.nav.top_page().is_some_and(|entry| entry.arg == person));
        assert_eq!(crate::metadata::detail_request_status(sid, "detail-a"), Some(true));
        crate::metadata::set_current_for_test(None);
        assert!(crate::metadata::current().is_none(),
            "the unsuccessful reconciliation has no cached Detail to fall back to");

        pages.nav.tabs.stack.transition =
            Box::new(crate::ui::containers::transition::Immediate);
        let mut settle = SettleDetailBeforeRestoredEnter {
            sid,
            generation: reconciliation,
            detail: None,
            expected_fresh: false,
            landed: false,
        };
        let ret = pages.return_state();
        bridge::nav_pop_with_return(&mut pages, ret);
        let (_, report) = bridge::frame_with_tap(&mut pages, &mut rig,
            crate::ui::machine::Tick { ms: frame_no * 16, dt_us: 16_000 }, Vec::new(), &mut settle);
        frame_no += 1;
        pages.prune(&report.unmounted);

        assert!(settle.landed, "the failed reconciliation settles at restored Enter");
        assert!(pages.nav.top_page().is_some_and(|entry| entry.id == a_entry && entry.arg == a));
        assert_eq!(crate::metadata::detail_generation_for_test(), reconciliation,
            "terminal failure must not be retried through the ordinary cold-body branch");
        assert_eq!(crate::metadata::detail_request_status(sid, "detail-a"), Some(false));

        frame(&mut pages, &mut rig, &mut frame_no);
        assert_eq!(detail_restore_target(&pages, a_entry), None,
            "terminal failure retires the restore intent without a cached Detail");
        assert_eq!(detail_return_waiting(&pages, a_entry), Some(false),
            "failed restoration cannot remain permanently waiting");
        frame(&mut pages, &mut rig, &mut frame_no);
        assert_eq!(crate::metadata::detail_generation_for_test(), reconciliation,
            "settled failure cannot start a delayed duplicate request");

        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
        crate::metadata::set_current_for_test(None);
        drain_detail_workers();
    }
}

fn home_menu_from_deck(ret: &ReturnState<u32, PageMemory>) -> bool {
    let (Some(focus), PageMemory::Home(memory)) = (ret.focus, &ret.memory) else { return false };
    memory.items.iter().any(|key| key.elem == focus.elem && matches!(&key.identity,
        HomeItemIdentity::Item { hub: HomeHubIdentity::ContinueWatching, .. }))
}

#[allow(clippy::too_many_arguments)]
fn activate_home_item(app: &mut App, source: MachineId, entry: EntryId,
    sid: crate::plex::ServerId, rk: &str, resume_ns: Option<i64>, ret: ReturnState<u32, PageMemory>, now: u32) {
    let snapshot = app.bridge.hubs_snapshot();
    let Some(mut item) = home_item(snapshot.view(), sid, rk).cloned() else { return };
    if let Some(resume_ns) = resume_ns { item.resume_ms = resume_ns.max(0) / 1_000_000; }
    // Home is the container's ROOT and a card press is the user acting on it, so there is nothing
    // above it to spend — `Trail::reset()` stood here and was a no-op in the only case that could
    // reach it (see `apply_item_action`'s note on `from_home`).
    let _ = (source, entry);
    unsafe { activate_card(&mut app.player.session, &mut app.adapters.player, &item, resume_ns.is_some(), HUD_LINGER_MS,
        Some(ret), &mut app.pages, &mut app.bridge, &mut app.menu_play_await, now); }
}

// (`cancel_content_navigation` and `freeze_request` stood here. The first re-checked the queued
// request's identity before withdrawing it — a route compare plus an `EntryId` plus an
// `InputOwner`; `NavStack::cancel(from)` is that test, over the entry alone, which strictly
// dominates the route term (two detail pages are one route and two entries). The second stamped
// the outgoing page's `ReturnState` onto the queued request after the fact, because `nav_req` had
// already been called and had nothing to put there; the `ret` rides the request from the start
// now, which is what `NavStack::request` captures.)

// (`capture_content_request` stood here, called from the loop just before the nav commit: it
// stamped the outgoing page's `ReturnState` onto a queued `NavReq` that had been made without
// one. `NavStack::request` captures it at the request itself, so there is no window in which a
// request exists without the state it returns to.)

/// Playback restores the retained origin, then asks that instance to reveal the played episode.
pub(crate) fn restore_played_entry(app: &mut App) {
    // **The origin entry has just been uncovered by `exit_player`'s `PopTo`**, so the page to
    // address is the top one and the `Spot` to restore is its OWN `ReturnState` — the state the
    // container captured when the Play was requested. It was `App.play_from`'s `Node::Detail`
    // spot, a copy of the same thing kept beside the tree.
    let Some(entry) = app.pages.nav.top_page() else { return };
    let AppArg::Content(ContentArg::Detail { sid, rk }) = &entry.arg else { return };
    let Some(instance) = entry.inst.as_ref().map(|i| i.id) else { return };
    let PageMemory::Detail(memory) = &entry.ret.memory else { return };
    let mut spot = memory.spot.clone();
    let episode = crate::metadata::playing().filter(|p| p.sid == *sid)
        .and_then(|_| crate::metadata::now_playing())
        .filter(|n| n.is_episode && n.detail_rk == *rk)
        .map(|n| { spot.season = Some(n.season); crate::route::cur_rk(&app.player.session) });
    if episode.is_some() {
        app.pages.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
            Delivery::Screen(ScreenEvent::App(AppMsg::DetailRestore {
                spot,
                episode,
                refresh: crate::screens::registry::DetailRefreshPhase::None,
            }))));
    }
}

pub(crate) fn refresh_content(
    pages: &mut crate::ui::dispatch::Dispatcher<bridge::AppHost>,
    target: crate::stores::viewstate::DetailRefresh,
) {
    // A write may finish after another Detail has covered its origin. The covered instance must
    // retain the addressed restore intent, but the ONE Metadata slot belongs to the top page: an
    // eager fetch here would supersede that visible Detail's load. The message tells a visible
    // Detail to start synchronously before arming Requested; when Back uncovers a covered entry,
    // its ordinary Enter(Restored) path performs that same atomic transition after it owns the
    // slot again.
    let Some(entry) = pages.nav.tabs.stack.entries.iter().rev()
        .find(|e| detail_refresh_matches(&e.arg, &target)) else { return };
    let AppArg::Content(ContentArg::Detail { .. }) = &entry.arg else { return };
    let Some(instance) = entry.inst.as_ref().map(|i| i.id) else { return };
    let owns_metadata = pages.nav.top_page().is_some_and(|top| top.id == entry.id);
    let memory = if owns_metadata {
        pages.return_state().memory
    } else { entry.ret.memory.clone() };
    let PageMemory::Detail(spot) = memory else { return };
    let spot = spot.spot;
    pages.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
        Delivery::Screen(ScreenEvent::App(AppMsg::DetailRestore {
            spot,
            episode: target.keep,
            refresh: if owns_metadata {
                crate::screens::registry::DetailRefreshPhase::Requested
            } else {
                crate::screens::registry::DetailRefreshPhase::Deferred
            },
        }))));
}

fn detail_refresh_matches(arg: &AppArg, target: &crate::stores::viewstate::DetailRefresh) -> bool {
    matches!(arg, AppArg::Content(ContentArg::Detail { sid, rk })
        if *sid == target.sid && *rk == target.rk)
}

/// Refuse privileged replay before any request reaches the live PlayerAdapter.
fn remove_replayed_repairs(reqs: &mut Vec<crate::screens::registry::PlayerReq>, legacy: bool, controlled: bool) -> bool {
    if !legacy && !controlled { return false; }
    let before = reqs.len();
    reqs.retain(|req| !matches!(req, crate::screens::registry::PlayerReq::RepairSandbox));
    before != reqs.len()
}

#[cfg(test)]
mod repair_replay_tests {
    use super::*;
    use crate::screens::registry::PlayerReq;
    #[test]
    fn both_replay_modes_remove_repair_before_resource_dispatch_but_keep_exit() {
        for (legacy, controlled) in [(true, false), (false, true), (true, true)] {
            let mut reqs = vec![PlayerReq::RepairSandbox, PlayerReq::Exit];
            assert!(remove_replayed_repairs(&mut reqs, legacy, controlled));
            assert_eq!(reqs, vec![PlayerReq::Exit]);
        }
        let mut live = vec![PlayerReq::RepairSandbox, PlayerReq::Exit];
        assert!(!remove_replayed_repairs(&mut live, false, false));
        assert_eq!(live, vec![PlayerReq::RepairSandbox, PlayerReq::Exit]);
    }
}
