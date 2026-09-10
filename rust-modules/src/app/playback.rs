//! Playback orchestration from the app core's side: the scrub/seek/position accessors, the
//! start/exit/finish of a playback and the player-route key handlers. Moved out of `app.rs`
//! verbatim in phase 1a (a pure move; `pub(crate)` widening only).
//!
//! **The HUD's timer, its cursor, the scrub gesture and the repeat gates are no longer here.**
//! Restructure phase 9 moved them into the player's own `Screen` instance
//! (`crate::screens::player`), with the types and the pure predicates in
//! `crate::screens::player::input`; this module re-exports the names the loop still spells
//! unqualified. What that changed for a reader of the code below: `hud_until()`/`set_hud`/
//! `extend_hud`/`scrub()`/`set_scrub` are gone as free functions, because the values they read and
//! wrote are fields of a screen now (§2.3), and every arm that touches them takes that screen.

use super::*;
pub(crate) use crate::screens::player::input::{
    failed_key_action, scrub_press, FailedKeyAction, HudNav, HudState,
    Scrub, ScrubPress, HUD_HEADLESS_MS, HUD_LINGER_MS, SCRUB_ACCEL,
    SCRUB_BASE, SCRUB_LOST_MS, SCRUB_MAX, SCRUB_STEP_NS, TAP_COMMIT_MS,
};
// The modal repeat cadence is `screens::registry`'s (phase 10 merge): the item context menu is a
// surface of its own family and needs the same gate, so it is shared vocabulary rather than the
// player's. `App::modal_repeat` still reaches it through this re-export.
pub(crate) use crate::screens::registry::RepeatGate;
pub(crate) use crate::screens::player::PlayerScreen;


#[inline]
pub(crate) fn resume_pend() -> bool {
    crate::player::TX.resume_pend.load(Relaxed)
}
#[inline]
pub(crate) fn set_resume_pend(v: bool) {
    crate::player::TX.resume_pend.store(v, Relaxed)
}
#[inline]
pub(crate) fn dur() -> i64 {
    crate::player::duration_ns()
}
#[inline]
pub(crate) fn playpos() -> i64 {
    crate::player::playpos_ns()
}
/// The playhead the user INTENDED, which is not always the one being published. While a seek is
/// still resolving (request → reopen → prime → Play) `playpos()` keeps reporting the PRE-seek spot,
/// so anything that snapshots "where are we?" inside that window snapshots the position the user
/// just left. The rule — an in-flight seek target wins, else the published position — was open-coded
/// at each reader that remembered it (the scrub seed below; the HUD's frozen playhead in
/// `ui/player_hud.rs`) and simply MISSING at the one that did not: the OS-background save took a bare
/// `playpos()`, so backgrounding right after a seek stored the pre-seek spot and the foreground
/// restore replayed from there — and teardown clears the pending target, so nothing self-corrected.
/// Use this at every reader that means "where the user is"; keep the raw `playpos()` only where the
/// PUBLISHED position is the point (the re-pause gate, which is already behind `seek_pending() < 0`,
/// and the heartbeat's `pos=`, which the harness grades real playback progress from).
#[inline]
pub(crate) fn intended_pos(ps: &crate::route::PlaybackSession) -> i64 {
    crate::player::intended_pos_ns(ps)
}
#[inline]
pub(crate) fn frames() -> i32 {
    crate::player::frames()
}
#[inline]
pub(crate) fn seek_pending() -> i64 {
    crate::player::seek_pending()
}
#[inline]
pub(crate) fn request_seek(x: i64) {
    crate::player::request_seek(x)
}
/// Commit a scrub to `target` and clear the preview. If we were PAUSED, STAY logically paused: a
/// dedicated seek-preroll feed override lets the synchronized native clock decode one landed frame
/// without publishing a false viewer Resume. `resume_pend` asks the per-frame loop to close that
/// bounded override. `repause_at` is the landed-frame wait target.
pub(crate) fn commit_seek(scrubber: &mut Scrub, target: i64, repause_at: &mut i64) {
    crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
        feature: crate::diag::schema::Feature::Seek,
    });
    request_seek(target);
    scrubber.ns = -1;
    if paused() {
        *repause_at = target;
        set_resume_pend(true);
        crate::player::TX.begin_paused_seek();
    }
}
#[inline]
pub(crate) fn is_started() -> bool {
    crate::player::is_started()
}

// ---- the route vocabulary, and the pure questions asked ABOUT a route -------------------------
//
// These are pure functions of a `Route` that read and write no app state, which is what lets
// `route_tests` at the bottom of this file grade them — and grading them is the point, because they
// decide things that have shipped wrong (the teardown rule below, twice), and a `Route` that only
// exists inside the run loop's body is a decision no host test can reach. The loop still owns every
// VALUE — `route` is a local, the trail is a local.

/// Perform what the `…` popover reported. Shared by the OK key and the pointer click, so
/// the two paths can never come to disagree about what a row does.
pub(crate) fn apply_more_action(ps: &mut crate::route::PlaybackSession, pa: &mut crate::player::adapter::PlayerAdapter, a: crate::ui::more_menu::Action) {
    match a {
        crate::ui::more_menu::Action::ToggleStats => crate::app::diagnostics::toggle(),
        // A rung of the playback-quality ladder — a routing POLICY, not a number handed to a
        // running stream. Not deferred either: `route::set_quality` re-asks the routing question
        // for the playback on screen and reloads only when the answer changed.
        crate::ui::more_menu::Action::SetQuality(q) => {
            // A terminal Engine never reaches pump's pending-retranscode arm, and a `/decision`
            // refusal has no Engine at all.  Persist the pick first, then make a fresh playback
            // request at the same user-visible position.  Selecting the already-active rung is
            // therefore the promised plain Retry.
            let failed = matches!(crate::player::state(ps), crate::player::PlaybackState::Error);
            if failed {
                crate::route::set_quality_for_retry(q);
                retry_failed_playback(ps, pa);
            } else {
                crate::route::set_quality(ps, q);
            }
        }
        // Lab builds only. Nothing about playback changes: the snapshot is taken and the toast
        // reports, over whatever the player is doing.
        crate::ui::more_menu::Action::SendDiagnostics => crate::lab::request_upload("menu", ps),
        crate::ui::more_menu::Action::None => {}
    }
}

/// Replace a terminal attempt with a new resolve of the same Plex item.
///
/// This is a REAL stop followed by a new request, not an Engine reload: it covers the pre-flight
/// refusal which never created an Engine, retires a failed server transcode when there was one,
/// and gives telemetry two honest attempts.  The descriptor lives in `route`; the app owns only
/// the current playhead and the Engine lifecycle.
pub(crate) fn retry_failed_playback(ps: &mut crate::route::PlaybackSession, pa: &mut crate::player::adapter::PlayerAdapter) -> bool {
    // URL/dev-trigger playback has no Plex descriptor.  Check BEFORE teardown: extinguishing its
    // Error Engine and only then discovering it cannot be rebuilt would replace an actionable
    // read-out with an idle black frame.
    if !crate::route::can_retry_current_play(ps) {
        log("playback retry: current source has no reusable Plex request");
        return false;
    }
    // A terminal error can race a seek whose requested target has not landed.  Resume what the
    // viewer asked for, not the last frame the dying Engine happened to publish.  If an earlier
    // retry was refused before presenting anything, retain its target too: the stopped Engine now
    // reports zero and must not send a second quality attempt back to the beginning.
    let resume_ns = intended_pos(ps)
        .max(crate::route::unpresented_resume_ns(ps))
        .max(0);
    crate::player::stop_bufferfeed(ps, pa);
    if crate::route::retry_current_play(ps, resume_ns) {
        crate::ui::idle::invalidate();
        true
    } else {
        log("playback retry: current source cannot be resolved again");
        false
    }
}

/// The ONE start-playback ritual (detail OK, home episode OK, and the plxnative-autoplay/
/// -detailplay/-play dev triggers all share it): arm the resume point BEFORE the first
/// Load (direct-play av_seek / transcode &offset restart), start the engine, record the
/// Stop/BACK/EOS return target, reset the HUD focus cursor, and show the HUD. A missed step
/// here used to silently fork behavior between the interactive and headless paths.
pub(crate) fn start_playback(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    resume_ns: i64,
    from: Origin,
    hud_ms: u32,
    route: &mut Route,
    play_from: &mut Node,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
) {
    // A resolve in flight means the route statics are NOT installed yet. Applying the
    // resume now would read a stale/empty TSESSION, so `resume_at` would take its
    // DIRECT-PLAY branch and arm_seek() a transcode — and pump.rs's feed gate requires
    // `seek_to_ns < 0`, so that stray armed seek blocks feeding forever: no frames, no
    // ACB bind, timeline frozen at the resume point. (Exactly what broke
    // transcode_av1_no_dp_audio. Direct-play never noticed because arm_seek is what the
    // correct branch does anyway.) Defer it to `pump_play`, after apply_plan.
    let pending = crate::route::play_pending();
    let resume_prepared = pending
        || resume_ns <= 0
        || matches!(
            crate::player::resume_at(ps, resume_ns),
            crate::player::ResumeOutcome::Prepared
        );
    if !resume_prepared {
        if let Some(transaction) = crate::route::pending_route_start() {
            let _ = crate::route::reject_route_start_preparation(transaction);
        }
    }
    // Flip to the player NOW so the HUD draws its Resolving state this frame; `pump_play`
    // below starts the engine when the plan lands. With nothing pending this is the old
    // synchronous behaviour, byte for byte.
    let entering = if pending {
        crate::route::arm_play_resume(ps, resume_ns);
        true
    } else if resume_prepared {
        crate::player::start_bufferfeed(ps, pa)
    } else {
        false
    };
    if entering {
        set_origin(play_from, from);
        *route = Route::Player;
    }
    // A NEW session starts on the scrubber. The cursor is per-session state that nothing
    // else clears: the auto-hide re-park later in the loop only runs while the route is
    // already Player, and the exit paths leave the player entirely — so leaving a movie
    // with the Subtitles button focused used to carry `focus == 1` into the next one,
    // where the first OK opened the track menu instead of pausing. Unconditional, like the
    // `set_paused`/`set_hud` below it: the HUD that is about to be drawn belongs to THIS
    // attempt either way.
    // A NEW session starts on the scrubber with its transport pinned for `hud_ms`, and both
    // halves of that are now the INSTANCE's: a fresh mount is built at `HudState::IDLE` (which is
    // `HudNav::HOME`, an empty countdown and no dismissal), so the per-session reset that used to
    // be four hand-written assignments here is what mounting one costs.
    //
    // **Both paths, because there are two.** An ordinary start flips the route and the page mounts
    // next frame, so the pin is SEEDED for that mount; an auto-advance chain (episode → episode →
    // …) re-enters here with the player already on screen and never passes through `exit_player`,
    // so the live instance is reset in place. Stamped from NOW rather than from the keypress —
    // callers used to pass `last_input + HUD_LINGER_MS`, a timestamp taken BEFORE the blocking
    // resolve above, so a load longer than the 4.5 s linger expired the HUD before it was ever
    // drawn and the user got a blank screen instead of a transport.
    bridge.seed_player_hud(hud_ms);
    if let Some(player) = super::bridge::player_mut(pages) {
        player.hud = HudState::IDLE;
        player.scrub = Scrub::IDLE;
        player.up_next.reset();
        player.hud.extend(clock::now(), hud_ms);
        player.publish();
    }
    set_paused(false);
}

/// Resume if a seek landed while paused — the twin of `commit_seek`, which is the
/// stay-paused variant. Written out four separate times in this file before it had a name.
pub(crate) fn resume_if_paused(pa: &mut crate::player::adapter::PlayerAdapter) {
    if paused() {
        set_transport_paused(pa, false);
    }
}

/// Legacy card launches resolve media data without creating an invisible Detail screen.
pub(crate) fn request_loaded_hero(ps: &mut crate::route::PlaybackSession) -> Option<i64> {
    let d = crate::metadata::current()?;
    if d.kind == "show" || !d.seasons.is_empty() {
        let started = d.on_deck.as_ref().is_some_and(|e| e.resume_ms > 0)
            || d.seasons.iter().any(|s| s.viewed_leaf_count > 0);
        let ep = (if started { d.on_deck.as_ref() } else { None })
            .or_else(|| d.episodes.first())?;
        request_episode(ps, d, ep).then(|| crate::metadata::resume_ns(ep.resume_ms, ep.dur_ms))
    } else {
        crate::route::request_play(ps, crate::route::item_sid(d.sid), &d.rk, &d.part,
            &d.vcodec, &d.acodec, &d.title, "")
            .then(|| crate::metadata::resume_ns(d.resume_ms, d.dur_ms))
    }
}

pub(crate) fn request_loaded_episode(ps: &mut crate::route::PlaybackSession, rk: &str) -> bool {
    let Some(d) = crate::metadata::current() else { return false };
    d.episodes.iter().find(|e| e.rk == rk).is_some_and(|ep| request_episode(ps, d, ep))
}

fn request_episode(ps: &mut crate::route::PlaybackSession, d: &crate::metadata::Detail, ep: &crate::metadata::Episode) -> bool {
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::SetNowPlaying(Some(crate::metadata::NowPlaying {
        is_episode: true, title: d.title.clone(), ep_title: ep.title.clone(),
        season: ep.season, index: ep.index, summary: ep.summary.clone(),
        year: ep.aired.get(..4).and_then(|s| s.parse().ok()).unwrap_or(0),
        dur_ms: ep.dur_ms, rating: ep.rating.clone(), thumb: ep.thumb.clone(), detail_rk: d.rk.clone(),
    })));
    let title = if ep.title.is_empty() { &d.title } else { &ep.title };
    let context = format!("{}  ·  S{} E{}", d.title, ep.season, ep.index);
    crate::route::request_play(ps, crate::route::item_sid(d.sid), &ep.rk, &ep.part,
        &ep.vcodec, &ep.acodec, title, &context)
}

/// Leaving playback (Stop / BACK / EOS / Info's jump-to-detail): retire every in-player panel.
///
/// **Three of the four things this used to do are gone, and their absence is the phase.** The four
/// panels were module `static mut`s that the route flip merely stopped DRAWING, so each had to be
/// told by hand to forget it was open (the EOS path once forgot the menu); they are entries on the
/// player page's own `ModalStack` now, and unmounting the page unmounts them with it. The Up Next
/// countdown was a pair of statics for the same reason and is a field of the instance. What is
/// left is the one panel that is NOT the player's — the diagnostics read-out (phase 10) — and the
/// stack dismissal, which is here rather than left to the page's unmount because a FAILED playback
/// keeps its `…` popover up over the read-out and BACK must take that panel down first.
pub(crate) fn close_player_overlays(pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>) {
    super::bridge::dismiss_player_overlays(pages);
    crate::app::diagnostics::close(); // a diagnostics panel must not survive into the next session
}

/// **The Info card's tvOS press, committed on the spring-back.** The card's own OK arm cannot
/// perform this: `InfoAction` reaches a detail page or a seek, which needs the route, the trail and
/// the player adapter, and — the reason it is DEFERRED at all — the dip has to be on screen
/// before the card goes away. So the surface arms `PlayerReq::ArmInfoPress`, the loop's press
/// machine holds the frame, and this reads the decision back out of the panel that is still up.
pub(crate) unsafe fn commit_info_press(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    route: &mut Route,
    play_from: &mut Node,
    refresh_hubs_at: &mut u32,
    trail: &mut Trail,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
) {
    let Some(action) = super::bridge::player_overlay_mut(pages).and_then(|o| o.info_press_action())
    else {
        return;
    };
    close_player_overlays(pages);
    apply_info_action(ps, pa, action, route, play_from, refresh_hubs_at, trail, pages);
}

/// **Perform what a player overlay decided** (§14): the panel owns its own state and its own
/// input, but not the playback — seeking, pausing, applying a quality rung and leaving for a
/// detail page all need the player adapter, the route or the trail, none of which a screen may
/// name. Drained once per frame, after the dispatcher's, exactly as the Library's requests are.
#[allow(clippy::too_many_arguments)]
/// **Perform the track menu's pick.**
///
/// `route::commit_audio_selection`/`commit_subtitle_selection` take the playback session's `&mut`,
/// which no screen has (§2.2) — the panel returns a
/// [`TrackCommit`](crate::ui::track_menu::TrackCommit) and this is where it lands, from the
/// overlay's `PlayerReq` and from the headless `plxnative-menupick` trigger alike.
pub(crate) fn commit_track(
    ps: &mut crate::route::PlaybackSession,
    commit: crate::ui::track_menu::TrackCommit,
) {
    use crate::ui::track_menu::TrackCommit;
    match commit {
        TrackCommit::Audio { ordinal, codec, stream_id } =>
            crate::route::commit_audio_selection(ps, ordinal, &codec, stream_id),
        TrackCommit::Subtitle { render_ordinal, stream_id } =>
            crate::route::commit_subtitle_selection(ps, render_ordinal, stream_id),
    }
}

pub(crate) fn player_requests(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    reqs: Vec<crate::screens::registry::PlayerReq>,
    now: u32,
    route: &mut Route,
    play_from: &Node,
    refresh_hubs_at: &mut u32,
    trail: &mut Trail,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    ok_armed: &mut bool,
    press: &mut crate::ui::press::Press,
) {
    use crate::screens::registry::PlayerReq;
    for req in reqs {
        match req {
            PlayerReq::ExtendHud(ms) => {
                if let Some(player) = super::bridge::player_mut(pages) {
                    player.hud.extend(now, ms);
                    player.publish();
                }
            }
            PlayerReq::FocusTabs => {
                if let Some(player) = super::bridge::player_mut(pages) {
                    player.hud.nav.focus = 2;
                }
            }
            // The fall-through a surface cannot perform (`screens::player::overlay`'s module doc):
            // the same toggle the bare transport reaches, with the panel left untouched.
            PlayerReq::Transport(play) => {
                let want_paused = match play {
                    Some(true) => false,
                    Some(false) => true,
                    None => !paused(),
                };
                set_transport_paused(pa, want_paused);
                if let Some(player) = super::bridge::player_mut(pages) {
                    player.hud.extend(now, HUD_LINGER_MS);
                    player.publish();
                }
            }
            PlayerReq::SeekTo(ns) => {
                request_seek(ns);
                resume_if_paused(pa);
            }
            PlayerReq::More(action) => apply_more_action(ps, pa, action),
            PlayerReq::CommitTrack(commit) => commit_track(ps, commit),
            PlayerReq::ArmInfoPress => {
                press.begin_ctl(now);
                *ok_armed = true;
            }
            PlayerReq::Info(action) => {
                apply_info_action(ps, pa, action, route, play_from, refresh_hubs_at, trail, pages)
            }
        }
    }
}

/// Returning to a detail page after an EPISODE lands on its SHOW at the episode that played, not
/// on the episode's own page. Reports whether it mounted anything.
///
/// The play paths load the played LEAF's detail (that is where the HUD caption and Info card come
/// from), so an exit that only flipped the route stranded the user on an episode hero page — even
/// when they had started from the show page, and even after an auto-advance chain had moved them
/// several episodes along. `detail_rk` is already the "Go to Show" target.
///
/// **Gated on the page actually BEING that show**, which the `played_from_detail` bool could not
/// express: a *Play from Start* on a Related tile is an item the page says nothing about, and if
/// it happens to be an episode then revealing its show here would navigate the user to a page they
/// never asked for instead of back to the one they were standing on.
pub(crate) fn reveal_played_episode(from: &Node) -> bool {
    let Node::Detail { sid, rk, .. } = from else {
        return false;
    };
    // The played leaf's own server — `metadata::playing()` is the store the playback was resolved
    // from, so it names the machine the show is on. `plex::current_server()` would be the wrong
    // answer for anything played off a share.
    let psid = crate::metadata::playing()
        .map(|p| p.sid)
        .unwrap_or_else(crate::plex::current_server);
    let Some((show_rk, season)) = crate::metadata::now_playing()
        .filter(|n| n.is_episode && !n.detail_rk.is_empty())
        .map(|n| (n.detail_rk.clone(), n.season))
    else {
        return false;
    };
    if !crate::plex::same_item((psid, &show_rk), (*sid, rk)) {
        return false;
    }
    let _ = season; // The addressed reveal is applied after the origin entry is uncovered.
    true
}

/// The ONE leave-playback ritual (Stop key, BACK, EOS): close the overlays, stop the
/// engine, put the page the session was LAUNCHED FROM back on screen, and arm the deferred
/// hub refresh so Continue Watching reflects the session that just ended. A new exit path
/// that skips this quietly re-introduces the stale-CW bug.
///
/// `from` is [`Origin`]'s payload — the page that was mounted when playback started. Re-entry is
/// [`enter_node`], the same ritual every BACK pop and every forward `Nav::Open` uses, so a player
/// exit cannot mount a page in a way nothing else does.
pub(crate) fn exit_player(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    route: &mut Route,
    play_from: &Node,
    refresh_hubs_at: &mut u32,
    trail: &mut Trail,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
) {
    crate::route::cancel_play(ps); // BACK during a load: supersede, drop the landing
    close_player_overlays(pages);
    crate::player::stop_bufferfeed(ps, pa);
    // `stop_bufferfeed` reports/clears a real engine through `report::ended`, but a refusal or a
    // BACK during resolve has no engine for teardown to take. The exit ritual still ends that
    // attempt, so retire its in-memory trace here as the common backstop.
    crate::player::report::clear_error_trace();
    if reveal_played_episode(play_from) {
        // …the reveal IS the mount, so `enter_node`'s would be a second, competing one.
        *route = Route::Detail;
    } else {
        enter_node(play_from, route);
    }
    // The player is NOT a trail node — it returns to the page it was started from, and that page
    // may be one nothing ever pushed (a dev trigger, or `home_activate` opening a detail page
    // under the hood purely to fire its Play). `ensure` makes the trail agree with where we
    // landed: a no-op in the ordinary case (the page IS still the top, because playing never
    // moved the trail), the root for a return to Home, a push otherwise.
    trail.ensure(play_from);
    *refresh_hubs_at = clock::now().wrapping_add(800).max(1);
}

/// The episode is OVER — drained to EOS, or the user skipped a `final` credits marker.
/// Starts the queued episode when the show has one, else leaves the player exactly as
/// `exit_player` would. There is no interstitial: "always the next episode".
pub(crate) fn finish_playback(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    route: &mut Route,
    play_from: &mut Node,
    refresh_hubs_at: &mut u32,
    trail: &mut Trail,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
) {
    if play_up_next(ps, pa, HUD_LINGER_MS, route, play_from, pages, bridge) {
        return;
    }
    exit_player(ps, pa, route, play_from, refresh_hubs_at, trail, pages);
    // The ring goes back to the scrubber for the NEXT session, and the next session is a fresh
    // instance — so there is nothing to park here any more (`start_playback`'s note).
}

/// Activate whatever occupies the control row. ONE dispatch for both the OK key and the
/// pointer — they used to hold byte-identical copies of this `match`, and had already
/// drifted (the key path cleared the held key, the pointer path did not). Returns true when
/// the route flipped, which is the only thing the two callers still handle differently.
#[allow(clippy::too_many_arguments)]
pub(crate) fn activate_ctrl_row(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    slot: crate::ui::player_hud::ControlSlot,
    route: &mut Route,
    play_from: &mut Node,
    refresh_hubs_at: &mut u32,
    btn: c_int,
    trail: &mut Trail,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
) -> bool {
    use crate::ui::player_hud::ControlSlot;
    use crate::screens::player::skip_pill::SkipAction;
    match slot {
        // The row's two items, off the cursor the caller already parked (a click sets it
        // from the hit-test, a key press moved it). *Next Episode* starts the successor;
        // *Watch Credits* does nothing beyond the cancel the frame block below performs
        // for it — the button exists so that "let it run" is a THING YOU CAN PRESS rather
        // than an absence, which on a countdown is the difference between choosing and
        // being caught out.
        ControlSlot::UpNext(_) => {
            if btn == crate::ui::up_next::BTN_NEXT {
                play_up_next(ps, pa, HUD_LINGER_MS, route, play_from, pages, bridge)
            } else {
                if let Some(player) = super::bridge::player_mut(pages) {
                    player.up_next.cancel();
                }
                false
            }
        }
        ControlSlot::Skip(pr) => {
            crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
                feature: match pr.kind {
                    crate::metadata::MarkerKind::Intro => crate::diag::schema::Feature::SkipIntro,
                    crate::metadata::MarkerKind::Credits => {
                        crate::diag::schema::Feature::SkipCredits
                    }
                },
            });
            match pr.action {
                SkipAction::Seek(ns) => {
                    // Retire the segment FIRST: the seek lands on the preceding keyframe, which
                    // is usually still inside it, so without this the button comes straight back
                    // (see `metadata::mark_skipped`).
                    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::MarkSkipped(pr.marker));
                    request_seek(ns);
                    resume_if_paused(pa);
                    false
                }
                // a `final` credits segment: skipping it IS finishing the item
                SkipAction::Finish => {
                    finish_playback(ps, pa, route, play_from, refresh_hubs_at, trail, pages, bridge);
                    true
                }
            }
        }
        ControlSlot::Discs => false,
    }
}

/// Start the queued episode. Returns false when there is nothing queued (a movie, or the
/// last episode), which is the caller's cue to leave the player.
///
/// It stops the outgoing session ITSELF rather than trusting each call site to: three
/// paths reach here (EOS, Skip Credits on a `final` marker, and OK on the HUD tile while
/// the credits are still rolling) and in all three an Engine is live — `start_bufferfeed`
/// no-ops while one is, so skipping the stop would silently fail to advance. The stop is
/// also what posts the `state=stopped` timeline that commits the watched state, and it
/// must happen BEFORE `request_play_up_next`: teardown reads the outgoing item's session
/// ids and clears the URL, both of which the new plan is about to overwrite.
pub(crate) fn play_up_next(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    hud_ms: u32,
    route: &mut Route,
    play_from: &mut Node,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
) -> bool {
    // clone off the `&'static` store BEFORE anything can replace it (see `Countdown::take`)
    let Some(u) = super::bridge::player_mut(pages).and_then(|player| player.up_next.take(ps)) else {
        return false;
    };
    // The ratingKey, not the episode title: `rk` is the handle every other line and every harness
    // assertion already uses, and the title is LG's "Content Viewing Information" — the one
    // category this app's Data Safety declaration answers "Not collected" to. `diag::scrub` is a
    // backstop for shapes like this; not writing it is the mechanism.
    log(&format!("up next: S{}E{} rk={}", u.season, u.index, u.rk));
    let (rk, resume) = (
        u.rk.clone(),
        crate::metadata::resume_ns(u.resume_ms, u.dur_ms),
    );
    close_player_overlays(pages);
    crate::player::stop_bufferfeed(ps, pa);
    if !crate::route::request_play_up_next(ps, u) {
        return false;
    }
    // Same ritual as `play_item_now`: retire the finished episode's descriptor so the HUD
    // caption and Info card don't label the new playback with the old one's title for the
    // whole pre-roll, and fetch the new leaf off the loop.
    // Read BEFORE `retire_playing` drops the store: the successor is a row of the queue
    // the finished episode created, so it lives on that episode's server.
    let sid = crate::metadata::playing()
        .map(|p| p.sid)
        .unwrap_or_else(crate::plex::current_server);
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::RetirePlaying);
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::RequestDetail { sid: sid, rk: rk.to_string() });
    start_playback(
        ps,
        pa,
        resume,
        Origin::Unchanged,
        hud_ms,
        route,
        play_from,
        pages,
        bridge,
    );
    true
}

/// Direct-play a LEAF catalog item (movie or episode) — the hero-pill / Continue-Watching
/// "play now" ritual: route cfg + streams metadata + the shared start ritual.
/// `from_start` ignores the item's resume point — the item menu's "Play from Start", which is
/// the ONLY difference between restarting a Continue Watching tile and resuming it. Taking it
/// as a flag (rather than a resume_ns the caller computes) keeps Plex's resume rule
/// (`metadata::resume_ns`, which also refuses to resume the last few percent) in one place.
pub(crate) unsafe fn play_item_now(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    mm: &crate::pms::PmsMovie,
    from_start: bool,
    from: Origin,
    hud_ms: u32,
    route: &mut Route,
    play_from: &mut Node,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
) {
    if mm.rk.is_empty() {
        return;
    }
    if !crate::route::request_play_movie(ps, mm) {
        return;
    }
    // resolve OFF the SDL loop — pump_play starts it
    // Fetch OFF the loop too — pump_detail lands it. Nothing here reads current(): every
    // start_playback argument comes from `mm` (the catalog row), and the in-player track
    // menu reads metadata::playing(), which the resolve worker installs. The one consumer
    // is sync_now_playing()'s descriptor for the HUD caption and Info card, so a landing a
    // beat later costs a few frames of missing caption, never a wrong play.
    // Retire the old descriptor first: it describes the PREVIOUSLY played item, and the
    // HUD caption + Info card read it every frame — leaving it up would label this
    // playback with the last one's title for the whole pre-roll. None is honest (the
    // route's own TITLE/CTXLINE, set synchronously by request_play_movie, still carry
    // this item), and the landing refills it via sync_now_playing.
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::SetNowPlaying(None));
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::RequestDetail { sid: mm.sid, rk: mm.rk.to_string() });
    start_playback(
        ps,
        pa,
        if from_start {
            0
        } else {
            crate::metadata::resume_ns(mm.resume_ms, mm.dur_ns / 1_000_000)
        },
        from,
        hud_ms,
        route,
        play_from,
        pages,
        bridge,
    );
}

/// A playback FAILURE owns the whole frame (`player_hud::transport_hidden`): `draw_hud` returns
/// before painting anything and the overlay panels below are gated the same way, so the scrubber,
/// the control row, the bottom tabs and any panel that happened to be open are all absent from the
/// picture. Nothing that is not drawn may be driven — the rule `ControlSlot::UpNext` states and
/// `up_next::card_active` already keeps for the post-play card.
///
/// Two exceptions are painted on the read-out: BACK returns, while OK opens the shared quality
/// ladder on its current rung.  Selecting that rung is a plain retry; selecting another starts the
/// same item under the new policy.  A failure is therefore terminal for the Engine, not a trap for
/// the viewer.
///
/// This arm's guard still swallows Menu / Info / Chapters while the transport is absent.  It
/// explicitly exempts the More route it opened, so only that visible recovery panel reaches the
/// ordinary modal key arm beneath it.
pub(crate) fn key_player_failed(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    sym: c_uint,
    wcode: c_uint,
    route: &mut Route,
    play_from: &Node,
    refresh_hubs_at: &mut u32,
    trail: &mut Trail,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
) {
    match failed_key_action(is_ok(sym), is_back(sym, wcode)) {
        FailedKeyAction::ChooseQuality => {
            super::bridge::open_player_overlay(ps, pages, crate::screens::player::overlay::OverlayKind::More { quality: true });
        }
        // A panel over the read-out (only the `…` popover can be there —
        // `OverlayKind::survives_failure`) takes the BACK first; with nothing up, BACK leaves.
        FailedKeyAction::Return => {
            if super::bridge::player_overlay_up(pages) {
                close_player_overlays(pages);
            } else {
                exit_player(ps, pa, route, play_from, refresh_hubs_at, trail, pages);
            }
        }
        FailedKeyAction::Ignore => {}
    }
}

/// (`overlay_swallows_key`, `key_track_menu`, `key_more_menu`, `key_info_panel`,
/// `commit_info_panel` and `key_chapters` stood here. Restructure phase 9 made the four panels
/// SURFACES on the player page's own `ModalStack`, so each owns its input outright:
/// `screens::player::overlay::PlayerOverlayScreen::key` is the one ladder they now share, and the
/// predicate's `key` term — a transport press must reach the toggle and leave the panel up — is
/// `OverlayKind::swallows_transport` plus the `PlayerReq::Transport` forward that replaces the
/// fall-through a surface cannot perform. The decisions those arms used to make with the player
/// adapter, the route and the trail in scope arrive back here as `PlayerReq`, drained by
/// [`player_requests`].)

/// **Perform the Info card's chosen action** — the half of the old `commit_info_panel` that needs
/// the player adapter, the route and the trail. The card itself decided (its own `on_ok`) and
/// is already dismissing; this is what the loop does about it.
fn apply_info_action(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    action: crate::ui::info_panel::InfoAction,
    route: &mut Route,
    play_from: &Node,
    refresh_hubs_at: &mut u32,
    trail: &mut Trail,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
) {
    match action {
        crate::ui::info_panel::InfoAction::FromBeginning => {
            request_seek(0);
            resume_if_paused(pa);
        }
        crate::ui::info_panel::InfoAction::GoToDetail(rk) => {
            // Leave playback through THE exit ritual, then override where it landed. This arm used
            // to hand-roll the exit — overlays + stop_bufferfeed — which is three quarters of
            // `exit_player` and silently dropped the other quarter: `route::cancel_play()` (a jump
            // taken while a play resolve was still in flight left it to land later on Detail,
            // starting audio the user cannot reach) and the armed hub refresh (Continue Watching
            // kept the resume point from BEFORE this session — exactly the stale-CW bug
            // `exit_player`'s doc warns a new exit path re-introduces). The override is the one
            // real difference: the Info card's "Go to Show/Movie" always lands on THIS rk's page,
            // whatever origin route the ritual would otherwise have chosen.
            if !rk.is_empty() {
                // The played leaf's server, read BEFORE the exit ritual — `detail_rk` is that
                // item's own show, so it is on the same machine, and the store this reads is torn
                // down below.
                let sid = crate::metadata::playing()
                    .map(|p| p.sid)
                    .unwrap_or_else(crate::plex::current_server);
                exit_player(ps, pa, route, play_from, refresh_hubs_at, trail, pages);
                // A LANDING, not a navigation, so the trail is made to agree rather than pushed
                // blindly: the exit above has usually already put this very page on top (the show
                // playback started from), and `ensure_detail` is a no-op there. It is also
                // strictly better than the flag it replaces — a Library -> detail -> play -> "Go to
                // Show" now returns to the Library instead of to Home.
                trail.ensure(&to_detail(sid, &rk));
                *route = Route::Detail;
            }
        }
        crate::ui::info_panel::InfoAction::None => {}
    }
}

/// Playing: UP/DOWN move the HUD focus (scrubber ↔ buttons ↔ tabs). The first press on a hidden HUD
/// just reveals it (focused on the scrubber); pressing UP with nothing focusable above (the buttons
/// row) hides the HUD again.
pub(crate) fn key_player_updown(key: Key, now: u32, hud: &mut HudState, scrubber: &mut Scrub) {
    // the pre-press sample, not a fresh one: `begin_fresh_press` has already cleared `dismissed`,
    // so re-asking would call a hand-hidden transport visible (`HudState::visible_at_press`)
    let vis = hud.visible_at_press;
    let mut hide = false;
    if !vis {
        hud.nav.focus = 0; // reveal, on the scrubber
    } else if matches!(key, Key::Up) {
        // vertical stack, top → bottom: control row, scrubber, tabs. Both
        // marker stand-ins live IN the control row, so the ring is unchanged.
        match hud.nav.focus {
            0 => hud.nav.focus = 1, // scrubber → control row
            2 => hud.nav.focus = 0, // tabs → scrubber
            _ => {
                hide = true; // control row: nothing above → hide the HUD
                hud.nav.focus = 0;
            }
        }
    } else {
        match hud.nav.focus {
            0 => hud.nav.focus = 2, // scrubber → tabs
            1 => hud.nav.focus = 0, // buttons → scrubber
            _ => {}                 // tabs: nothing below → stay
        }
    }
    if hud.nav.focus != 0 || hide {
        // leaving the bar cancels any in-progress scrub preview
        scrubber.ns = -1;
        scrubber.disengage();
    }
    if hide {
        hud.dismissed = true; // stays hidden even while paused, until the next key
    } else {
        hud.extend(now, HUD_LINGER_MS);
    }
}

/// OK, on every screen that has not already `continue`d above.
/// Activate the player transport's focused CONTROL ROW item — the deferred half of [`key_ok`]'s
/// player arm, run from the per-frame loop once the press spring-back has played.
///
/// Two arms, in the order they were written in `key_ok`: a STAND-IN owns the row (Skip, Up Next) and
/// performs its own action, or the row holds the three discs and OK opens that disc's panel. `ctrl`
/// is re-resolved by the caller on the committing frame rather than captured at the press, so the
/// activation acts on the row that is DRAWN — the slot is resolved once per loop iteration for input,
/// update and draw alike (see the `let ctrl` at the top of the loop), and an offer that arrived
/// mid-press has already changed what the user is looking at.
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn activate_player_row(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    ctrl: crate::ui::player_hud::ControlSlot,
    now: u32,
    route: &mut Route,
    trail: &mut Trail,
    play_from: &mut Node,
    refresh_hubs_at: &mut u32,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
) {
    // The cursor is the instance's; read the one field this dispatch turns on, so the container
    // is borrowed once and the arms below are free to present a panel on it.
    let btn = super::bridge::player(pages).map_or(0, |player| player.hud.nav.btn);
    if !ctrl.is_discs() {
        // A stand-in owns row 1 — activate it. Same value the draw used. (Its `true` answer used
        // to clear the loop's client-side hold-repeat sym as well, so an async route flip could
        // not repeat a held key into the next screen. That timer is gone with phase 10 — see
        // `App::down_sym` — and the discrete lists it drove pace their own `Edge::Repeat`, which
        // a route change ends by retiring the surface that was receiving them.)
        activate_ctrl_row(
            ps,
            pa,
            ctrl,
            route,
            play_from,
            refresh_hubs_at,
            btn,
            trail,
            pages,
            bridge,
        );
    } else if btn == crate::ui::player_hud::BTN_MORE {
        // …so the discs are what row 1 holds — the complement of the arm above, and the row's only
        // other occupant. OK on a control disc PRESENTS its panel on this page's own stack.
        super::bridge::open_player_overlay(ps, pages, crate::screens::player::overlay::OverlayKind::More { quality: false });
    } else {
        super::bridge::open_player_overlay(
            ps,
            pages,
            crate::screens::player::overlay::OverlayKind::Tracks { tab: if btn == 0 { 1 } else { 0 } },
        );
    }
    if let Some(player) = super::bridge::player_mut(pages) {
        player.hud.extend(now, HUD_LINGER_MS);
        player.publish();
    }
}

/// PAUSE — the dedicated transport key, which only ever pauses (PLAY is its other half).
pub(crate) fn key_pause(
    pa: &mut crate::player::adapter::PlayerAdapter,
    route: Route,
    now: u32,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
) {
    if matches!(route, Route::Player) && !paused() {
        if set_transport_paused(pa, true) {
            crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
                feature: crate::diag::schema::Feature::Pause,
            });
        }
    }
    if let Some(player) = super::bridge::player_mut(pages) {
        player.hud.extend(now, HUD_LINGER_MS);
        player.publish();
    }
}

/// PLAY — off the player route it starts the buffer-feed and enters the player; on it, it un-pauses.
pub(crate) unsafe fn key_play(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    now: u32,
    foreground: &mut ForegroundLifecycle,
    repause_at: &mut i64,
    route: &mut Route,
    play_from: &mut Node,
    ptr: &mut Pointer,
    trail: &Trail,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
) {
    let was_off_player = !matches!(*route, Route::Player);
    if was_off_player {
        foreground.discard_started_state();
    }
    let activation = drive_foreground(
        foreground,
        ps,
        ForegroundInput::PlayKey,
        &mut PlayerForegroundActuator { pa, repause_at },
    );
    if matches!(activation, ForegroundActivation::Launched) {
        // A suspended session KEEPS its origin. The lifecycle arm forced its route to Home, but
        // that temporary screen is not where Stop/BACK should return.
        *route = Route::Player;
    } else if matches!(activation, ForegroundActivation::Ordinary) {
        if !matches!(*route, Route::Player) {
            if crate::player::start_bufferfeed(ps, pa) {
                if let Origin::From(n) = origin_here(*route, trail) {
                    *play_from = n;
                }
                *route = Route::Player;
                // Keep the ordinary off-route start's existing stale-Pause defense. A foreground
                // transition applies its explicit clock intent through the lifecycle actuator.
                if paused() {
                    set_transport_paused(pa, false);
                }
            }
        } else if paused() {
            set_transport_paused(pa, false);
        }
    }
    if was_off_player && !ptr.dpad_mode {
        hide_cursor();
        ptr.dpad_mode = true;
        ptr.cur_hidden = true;
    }
    if let Some(player) = super::bridge::player_mut(pages) {
        player.hud.extend(now, HUD_LINGER_MS);
        player.publish();
    }
}

/// LEFT/RIGHT while playing: move the focused HUD row's cursor, or — on the scrubber — jump the
/// scrub preview. A fresh press (0x001) is the fixed 10s jump; a held key's 0x101 repeats
/// ([`on_auto_repeat`]) then engage the continuous scrub and the keyup commits.
///
/// **A press that finds the HUD hidden is spent RAISING it, and moves nothing.** The transport is
/// on a 4.5 s timer over full-screen video, so "where am I" and "take me back ten seconds" are two
/// different intentions and the remote has no way to tell them apart — the old ladder read every
/// LEFT as the second, and a viewer glancing at the clock lost their place to it. It is the rule
/// UP/DOWN has always had one arm over ([`key_player_updown`]) and the rule the CLICK path already
/// enforced on this very band (`hud_vis` there is sampled before the click re-arms the timer,
/// after "a click in the invisible timed-out scrub band committed a blind seek"); the key path was
/// the last way in that still acted on geometry nobody could see.
///
/// **A HOLD is not a tap and keeps working.** The reveal still arms `dir` and seeds the preview, so
/// a user who holds LEFT to rewind gets the HUD on screen and then the ordinary continuous scrub as
/// the 0x101 repeats arrive — by which point the band they are dragging IS on screen. Only the
/// discrete 10 s hop waits for a second press, which is what `Scrub::reveal` marks: without it the
/// tap release would commit a seek to the seed, i.e. a full reopen+prime to the spot we are
/// already sitting on.
pub(crate) unsafe fn key_scrub(
    ps: &crate::route::PlaybackSession,
    key: Key,
    now: u32,
    ctrl: crate::ui::player_hud::ControlSlot,
    player: &mut PlayerScreen,
    ptr: &mut Pointer,
) {
    let PlayerScreen { hud, scrub: scrubber, .. } = player;
    if !ptr.cur_hidden {
        hide_cursor();
        ptr.cur_hidden = true;
    }
    if ptr.drag {
        ptr.drag = false;
        scrubber.ns = -1;
    }
    let fwd = matches!(key, Key::Right { .. });
    // the pre-press sample — see `key_player_updown`'s note and `HudState::visible_at_press`
    let act = scrub_press(hud.visible_at_press, hud.nav.focus, dur() > 0);
    hud.extend(now, HUD_LINGER_MS);
    match act {
        ScrubPress::Reveal => {
            hud.nav.focus = 0; // the HUD comes up on the scrubber, ready for the press after this one
                               // …and the gesture is armed but not spent, so a user who keeps HOLDING gets the
                               // ordinary continuous scrub the moment the repeats arrive. Only on something with a
                               // duration: arming a scrub over an item there is nothing to move through would put a
                               // preview on a band that cannot answer for it.
            if dur() > 0 {
                scrubber.begin(now, fwd);
                scrubber.reveal = true; // …but this press hops nothing; only a HOLD grows out of it
                seed_scrub(ps, scrubber);
            }
        }
        ScrubPress::Row => {
            // the row's occupant says how many items it has — no magic pin
            hud.nav.btn = (hud.nav.btn + if fwd { 1 } else { -1 }).clamp(0, ctrl.items() - 1);
        }
        ScrubPress::Tabs => {
            let max_tab = if crate::ui::chapters_panel::has_chapters() {
                1
            } else {
                0
            };
            hud.nav.tab = (hud.nav.tab + if fwd { 1 } else { -1 }).clamp(0, max_tab);
        }
        ScrubPress::Jump => {
            // scrubber focus, FRESH press (0x001): the fixed 10s jump. A held key's
            // 0x101 repeats (handled above) then engage the continuous scrub; the
            // keyup commits. Quick re-taps before scrubber.commit_at accumulate.
            let cap = dur() - 3 * 1_000_000_000;
            scrubber.commit_at = 0; // more input → cancel a pending tap commit
            scrubber.alive = now;
            scrubber.reveal = false; // a visible press is a real gesture whatever raised the HUD
            if scrubber.dir == 0 && scrubber.ns < 0 {
                seed_scrub(ps, scrubber);
            }
            if !scrubber.hold {
                let mut s = scrubber.ns.max(0) + if fwd { SCRUB_STEP_NS } else { -SCRUB_STEP_NS };
                if s < 0 {
                    s = 0;
                }
                if cap > 0 && s > cap {
                    s = cap;
                }
                scrubber.ns = s;
            }
            scrubber.dir = if fwd { 1 } else { -1 };
        }
        ScrubPress::Nothing => {}
    }
}

/// Seed a new scrub at the INTENDED playhead ([`intended_pos`]). If a prior commit's seek is still
/// landing, `playpos()` is stale (it still reports the pre-seek spot), so a quick re-press would
/// jump back to where we started and resume there — interrupting the scrub. The divergence IS "a
/// seek is in flight", so log it when the two disagree rather than re-deriving the condition here.
pub(crate) unsafe fn seed_scrub(ps: &crate::route::PlaybackSession, scrubber: &mut Scrub) {
    let seed = intended_pos(ps);
    let live = playpos();
    if seed != live {
        log(&format!(
            "scrub: seed at in-flight target {}s (playpos {}s stale)",
            seed / 1_000_000_000,
            live / 1_000_000_000
        ));
    }
    scrubber.ns = seed;
}

/// Run a play-plan landing and then observe the derived player state in the same frame. This tiny
/// seam is explicit because a refused `/decision` publishes `Error` inside the landing, after the
/// loop's ordinary report tick; BACK on the next frame can otherwise erase the only observation.
pub(crate) fn land_play_then_observe<S: ?Sized>(
    state: &mut S,
    land: impl FnOnce(&mut S),
    observe: impl FnOnce(&S),
) {
    land(state);
    observe(state);
}

#[cfg(test)]
mod play_landing_order_tests {
    use super::land_play_then_observe;
    use std::cell::RefCell;

    #[test]
    fn the_landing_seam_runs_publication_before_observation() {
        let order = RefCell::new(Vec::new());
        // The seam lends a state to both halves (the loop lends it `App`); what is pinned here is
        // the ORDER, so the state is a unit.
        land_play_then_observe(
            &mut (),
            |_| order.borrow_mut().push("landing"),
            |_| order.borrow_mut().push("observation"),
        );
        assert_eq!(*order.borrow(), ["landing", "observation"]);
    }
}
