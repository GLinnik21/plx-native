//! Playback orchestration from the app core's side: the scrub/seek/position accessors, the
//! start/exit/finish of a playback, and the performers behind the requests the player page raises
//! ([`player_requests`]). Moved out of `app.rs` verbatim in phase 1a (a pure move; `pub(crate)`
//! widening only).
//!
//! **The HUD's timer, its cursor, the scrub gesture and the repeat gates are no longer here.**
//! Restructure phase 9 moved them into the player's own `Screen` instance
//! (`crate::screens::player`), with the types and the pure predicates in
//! `crate::screens::player::input`; this module re-exports the few names the loop still spells
//! unqualified. What that changed for a reader of the code below: `hud_until()`/`set_hud`/
//! `extend_hud`/`scrub()`/`set_scrub` are gone as free functions, because the values they read and
//! wrote are fields of a screen now (§2.3), and every arm that touches them takes that screen.
//!
//! **And the player's KEY HANDLERS are no longer here either** (phase 12, PX-PLAYER).
//! `key_player_failed`, `key_player_updown`, `key_scrub` and `seed_scrub` are deleted, not moved:
//! `PlayerScreen::step` had been answering the same keys through the dispatcher since the page
//! began reporting `FocusSource::Engine`, and these four went on writing that page's `hud`/`scrub`
//! from the loop beside it. What is left of the player here is the half a screen may not do —
//! [`commit_seek`], [`exit_player`], [`activate_player_row`], the transport toggle — each reached
//! through a `PlayerReq`.

use super::*;
// The list is what the LOOP still spells, and phase 12 (PX-PLAYER) is why it is short: the scrub
// gesture's constants and its two pure policies (`scrub_press`, `failed_key_action`) are read by
// `screens::player` alone now, so re-exporting them here would be this module claiming a
// vocabulary it no longer uses.
pub(crate) use crate::screens::player::input::{
    HudNav, HudState, Scrub, HUD_HEADLESS_MS, HUD_LINGER_MS,
};
// The modal repeat cadence is `screens::registry`'s (phase 10 merge): the item context menu is a
// surface of its own family and needs the same gate, so it is shared vocabulary rather than the
// player's. `App::modal_repeat` still reaches it through this re-export.
pub(crate) use crate::screens::registry::RepeatGate;


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
pub(crate) fn commit_seek(target: i64, repause_at: &mut i64) {
    crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
        feature: crate::diag::schema::Feature::Seek,
    });
    request_seek(target);
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

/// **Where a session that is STARTING returns to** — the whole of what `app::nav::Origin` was,
/// once the origin became an `EntryId` rather than a described page.
///
/// It carried an `Origin::From(Node)` — a whole history entry, re-derived from the live stores at
/// every launch site by `origin_here` — because a `Route` names a KIND of page and BACK has to
/// land on the RIGHT one. The container already holds the right one: it is the entry on top when
/// Play was pressed, which is why this is a two-value CHOICE now rather than a payload.
///
/// [`Self::Unchanged`] is the auto-advance rule and the reason this is not a bool at a call site:
/// `play_up_next` starts a new item while the player is already up (so "the page on screen" is the
/// player, and the user chose nothing), and a PLAY key that resumes a session the app-switch
/// lifecycle suspended is resuming the same session from a page it was never launched from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Origin {
    /// A fresh launch: Stop/BACK/EOS lands on the page that is on top right now.
    Here,
    /// Keep whatever the live session already returns to.
    Unchanged,
}

/// **Put the player on the page stack, and record where BACK, Stop and EOS land** (§5.1).
///
/// The origin is the entry that is on top RIGHT NOW — the page the user pressed Play on — seeded
/// for the mount the push performs and read back off the mounted screen at the exit. It replaces
/// `App.play_from: Node` plus `enter_node` plus `Trail::ensure`, which between them said "re-derive
/// a page from a remembered description, then make the history agree".
///
/// **An auto-advance is not a navigation, and this is where that is decided.** `NavOp::Push`
/// always MINTS an entry — it has no reuse rule (`NavStack::apply`) — so re-entering with the
/// player already on top would stack a SECOND player over the first, mount a fresh screen that
/// consumed no seed, and land the exit on Home from the second episode of every chain. The page
/// the user is standing on has not changed; only the media has. `Origin::Unchanged` says the same
/// thing about the SEED, and the two agree at every call site; both are stated because they are
/// answers to different questions (which page is on top, and whose choice this was).
///
/// Its own function so that the page-level half of the ritual is reachable without an engine —
/// `screens::player::input::player_return_tests` drives exactly this, over a real container.
pub(crate) fn enter_player(
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
    from: Origin,
    ret: Option<crate::ui::screen::ReturnState<u32, crate::screens::registry::PageMemory>>,
) {
    let already_up = pages
        .nav
        .top_page()
        .map(|e| matches!(e.arg, AppArg::Player))
        .unwrap_or(false);
    if already_up {
        return;
    }
    if matches!(from, Origin::Here) {
        if let Some(entry) = pages.nav.top_page() {
            bridge.seed_player_origin(crate::screens::player::Origin {
                entry: entry.id,
                instance: entry.inst.as_ref().map(|i| i.id),
            });
        }
    }
    match ret {
        Some(ret) => super::bridge::nav_push_with_return(pages, AppArg::Player, ret),
        None => super::bridge::nav_push(pages, AppArg::Player),
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
    ret: Option<crate::ui::screen::ReturnState<u32, crate::screens::registry::PageMemory>>,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
) -> bool {
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
        enter_player(pages, bridge, from, ret);
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
    entering
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
    refresh_hubs_at: &mut u32,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
) {
    let Some(action) = super::bridge::player_overlay_mut(pages).and_then(|o| o.info_press_action())
    else {
        return;
    };
    close_player_overlays(pages);
    apply_info_action(ps, pa, action, refresh_hubs_at, pages);
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
    refresh_hubs_at: &mut u32,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    _bridge: &mut super::bridge::Bridge,
    ok_armed: &mut bool,
    press: &mut crate::ui::press::Press,
    repause_at: &mut i64,
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
            // …and its twin, which does NOT resume: the scrub bar is how a paused film is moved.
            // `repause_at` is an `App` field, which is exactly why this is a request.
            PlayerReq::CommitSeek(ns) => commit_seek(ns, repause_at),
            PlayerReq::More(action) => apply_more_action(ps, pa, action),
            PlayerReq::CommitTrack(commit) => commit_track(ps, commit),
            PlayerReq::ArmInfoPress => {
                press.begin_ctl(now);
                *ok_armed = true;
            }
            PlayerReq::Info(action) => {
                apply_info_action(ps, pa, action, refresh_hubs_at, pages)
            }
            // The old `key_ok`'s tabs-row (`focus == 2`) and failure-read-out (`ChooseQuality`)
            // arms, both of which presented a panel immediately rather than through the deferred
            // press `ArmControlRow`/`ArmInfoPress` use.
            PlayerReq::OpenOverlay(kind) => {
                super::bridge::open_player_overlay(ps, pages, kind);
            }
            // The old `key_ok`'s `focus == 1` arm: dip the same tvOS press `ArmInfoPress` does,
            // for the transport's OWN control row rather than a panel's action column. The loop's
            // existing commit-frame dispatch (`Route::Player => activate_player_row(...)`, below
            // in this same call chain) is unchanged and performs whatever the row decides.
            PlayerReq::ArmControlRow => {
                press.begin_ctl(now);
                *ok_armed = true;
            }
            // The STOP key, a BACK with nothing else open and the failure read-out's own BACK
            // escape all reach the same ritual.
            PlayerReq::Exit => {
                exit_player(ps, pa, refresh_hubs_at, pages);
            }
        }
    }
}

// (`reveal_played_episode` stood here. It answered one question — "is the page this session was
// launched from the SHOW the played episode belongs to?" — and its only use was choosing between
// two ways of writing the same route: `*route = Route::Detail` or `enter_node(play_from)`. Both
// spellings are retired with D1;
// are `NavOp::PopTo(origin.entry)` now, so the question has no consumer: the REVEAL itself was
// always `content::restore_played_entry`'s, which addresses the uncovered entry and delivers
// `AppMsg::DetailRestore`.)

/// The ONE leave-playback ritual (Stop key, BACK, EOS): close the overlays, stop the
/// engine, put the page the session was LAUNCHED FROM back on screen, and arm the deferred
/// hub refresh so Continue Watching reflects the session that just ended. A new exit path
/// that skips this quietly re-introduces the stale-CW bug.
///
/// The return target is [`Origin`]'s `entry` — the container entry that was on top when Play was
/// pressed, seeded at the player's own mount by [`enter_player`] and read back off the mounted
/// screen here. Re-entry is `NavOp::PopTo(entry)` (`bridge::nav_pop_to`), the same container op a
/// BACK off a stacking page performs, so a player exit cannot reach a page in a way nothing else
/// does. There is no re-derivation and no second history to reconcile: `App.play_from: Node` plus
/// `enter_node` plus `Trail::ensure` are all deleted (D1).
pub(crate) fn exit_player(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    refresh_hubs_at: &mut u32,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
) {
    crate::route::cancel_play(ps); // BACK during a load: supersede, drop the landing
    close_player_overlays(pages);
    crate::player::stop_bufferfeed(ps, pa);
    // `stop_bufferfeed` reports/clears a real engine through `report::ended`, but a refusal or a
    // BACK during resolve has no engine for teardown to take. The exit ritual still ends that
    // attempt, so retire its in-memory trace here as the common backstop.
    crate::player::report::clear_error_trace();
    // **The return is a `PopTo` of the ORIGIN ENTRY** (§5.1) — the page that was on top when the
    // push was asked for, captured at the player's own mount and read back off the instance. It
    // replaces `App.play_from: Node` plus `enter_node` plus `Trail::ensure`, which between them
    // said "re-derive a page from a remembered description, then make the history agree". An
    // entry needs no re-deriving and there is no second history to reconcile; and an origin that
    // is no longer on the stack falls back to Home (`nav_pop_to`), which is the same anti-strand
    // floor `return_page`'s `unwrap_or(Node::Home)` was.
    match super::bridge::player(pages).and_then(|p| p.origin) {
        Some(origin) => super::bridge::nav_pop_to(pages, origin.entry),
        None => super::bridge::nav_root(pages, AppArg::Home),
    }
    *refresh_hubs_at = clock::now().wrapping_add(800).max(1);
}

/// The episode is OVER — drained to EOS, or the user skipped a `final` credits marker.
/// Starts the queued episode when the show has one, else leaves the player exactly as
/// `exit_player` would. There is no interstitial: "always the next episode".
pub(crate) fn finish_playback(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    refresh_hubs_at: &mut u32,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
) {
    if play_up_next(ps, pa, HUD_LINGER_MS, pages, bridge) {
        return;
    }
    exit_player(ps, pa, refresh_hubs_at, pages);
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
    refresh_hubs_at: &mut u32,
    btn: c_int,
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
                play_up_next(ps, pa, HUD_LINGER_MS, pages, bridge)
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
                    finish_playback(ps, pa, refresh_hubs_at, pages, bridge);
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
        None,
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
    ret: Option<crate::ui::screen::ReturnState<u32, crate::screens::registry::PageMemory>>,
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
        ret,
        pages,
        bridge,
    );
}

// (`key_player_failed` stood here — the arm `app/run.rs` reached for a key arriving while a
// terminal read-out owned the frame: OK opened the quality ladder, BACK closed the `…` popover if
// one was up and otherwise left the playback, everything else was swallowed. Restructure phase 12
// (PX-PLAYER) retired it with the rest of the loop's player input path: `PlayerScreen::handle_key`
// asks `player_hud::transport_hidden` first and dispatches the SAME pure policy
// (`screens::player::input::failed_key_action`) into `PlayerReq::OpenOverlay`/`PlayerReq::Exit`.
// The popover-first half of its BACK is the container's by construction — an open `…` is a surface
// and answers the key before the page beneath it ever sees one.)

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
    refresh_hubs_at: &mut u32,
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
                exit_player(ps, pa, refresh_hubs_at, pages);
                // A LANDING, not a navigation, so the page is SHOWN rather than pushed blindly:
                // the exit above has usually already put this very page on top (the show playback
                // started from), and `show_page` is a no-op there. It is also strictly better than
                // the flag it replaces — a Library -> detail -> play -> "Go to Show" returns to
                // the Library instead of to Home.
                super::bridge::show_page(pages, AppArg::Content(
                    crate::screens::registry::ContentArg::Detail { sid, rk: rk.clone() },
                ));
            }
        }
        crate::ui::info_panel::InfoAction::None => {}
    }
}

// (`key_player_updown` stood here — UP/DOWN walking the HUD's ring, or spending the press on
// REVEALING a hidden transport. Ported to `PlayerScreen::key_updown` in phase 9 and DELETED with
// its caller in phase 12 (PX-PLAYER): the loop kept calling this on the screen's own `hud`/`scrub`
// while the screen answered the same key through the dispatcher.)

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
            refresh_hubs_at,
            btn,
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
    now: u32,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
) {
    if super::bridge::player(pages).is_some() && !paused() {
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
    ptr: &mut Pointer,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
) {
    let was_off_player = super::bridge::player(pages).is_none();
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
        // **A suspended session keeps its ORIGIN, and now it keeps its ENTRY too.** The lifecycle
        // arm used to force the route to Home, which retired the player entry and the page under
        // it; a park moves no page at all, so the only thing left to do here is put the player
        // back on top if something else took it — which, while the tree is parked, nothing does.
        super::bridge::show_page(pages, AppArg::Player);
    } else if matches!(activation, ForegroundActivation::Ordinary) {
        if was_off_player {
            if crate::player::start_bufferfeed(ps, pa) {
                // The origin is the page the PLAY key was pressed on — through the same one door
                // `start_playback` uses, so the seed and the push cannot drift apart here.
                enter_player(pages, bridge, Origin::Here, None);
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

// (`key_scrub` and `seed_scrub` stood here — the LEFT/RIGHT ladder for the player route and the
// in-flight-target seed a fresh gesture starts from. Both are `PlayerScreen`'s since phase 12
// (PX-PLAYER): `key_scrub_fresh` runs the same `ScrubPress` dispatch, and the seed is
// `player::intended_pos_ns` read at the same moment for the same reason — while a prior commit's
// seek is still landing, `playpos()` reports the PRE-seek spot, so a quick re-press seeded off it
// would jump back to where the last one started.
//
// The rules those two docs carried are not lost, and they are worth restating where the behaviour
// now lives: a press that finds the HUD HIDDEN is spent RAISING it and moves nothing (the
// transport sits on a 4.5 s timer over full-screen video, so "where am I" and "take me back ten
// seconds" are two intentions the remote cannot tell apart, and reading every LEFT as the second
// cost a viewer their place); and a HOLD is not a tap, so the reveal still arms the gesture and
// the ordinary continuous scrub grows out of it as the repeats arrive. `ScrubPress` and
// `screens::player::input`'s own `a_hidden_hud_spends_the_press_on_itself` are where both are
// pinned.)

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

/// **The loop's half of the scrub commit: a paused film stays paused across the seek.**
///
/// `PlayerReq::CommitSeek` promises this and `screens::player`'s own
/// `a_scrub_seek_taken_while_paused_holds_the_pause` grades that the screen ASKS for it; this
/// grades what the arm then does, which is the half no screen test can reach — `repause_at` is an
/// `App` field, `resume_pend` and the seek-preroll override are the pipeline's, and none of the
/// three is nameable from `screens/` (`ci/check-deps.sh`'s `layer` gate).
#[cfg(test)]
mod paused_seek_tests {
    use super::*;

    /// Every state this touches is a crate global, so the body holds `testlock::serial()`.
    #[test]
    fn a_scrub_commit_taken_while_paused_arms_the_repause_instead_of_resuming() {
        let _g = crate::testlock::serial();
        let was_paused = crate::player::TX.paused.load(Relaxed);
        crate::player::TX.paused.store(true, Relaxed);
        let mut repause_at = 0i64;

        commit_seek(42_000_000_000, &mut repause_at);

        assert_eq!(seek_pending(), 42_000_000_000, "the seek really was requested");
        assert_eq!(repause_at, 42_000_000_000, "…and the landed-frame wait target is its own");
        assert!(resume_pend(), "the per-frame loop is asked to close the bounded override");
        assert!(
            crate::player::seek_preroll_active(),
            "…which is what lets the pipeline decode the landed frame with the transport still \
             saying Paused — the whole difference from PlayerReq::SeekTo",
        );

        crate::player::TX.finish_seek_preroll();
        crate::player::TX.paused.store(was_paused, Relaxed);
    }

    /// …and the complement: while PLAYING there is nothing to hold, so the commit is a bare seek
    /// and arms no re-pause at all. Without this the case above passes for a `commit_seek` that
    /// armed the override unconditionally, which would re-pause a film nobody had paused.
    #[test]
    fn a_scrub_commit_taken_while_playing_arms_no_repause() {
        let _g = crate::testlock::serial();
        let was_paused = crate::player::TX.paused.load(Relaxed);
        crate::player::TX.paused.store(false, Relaxed);
        set_resume_pend(false);
        let mut repause_at = 7i64;

        commit_seek(11_000_000_000, &mut repause_at);

        assert_eq!(seek_pending(), 11_000_000_000);
        assert_eq!(repause_at, 7, "untouched: there is no pause to hold");
        assert!(!resume_pend());
        assert!(!crate::player::seek_preroll_active());
        crate::player::TX.paused.store(was_paused, Relaxed);
    }
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

// D8 (UI restructure phase 12): relocated verbatim from `app/mod.rs`'s `delete_local_data_tests`.
// `delete_outcome` (the function under test) stays in `app/input.rs`, beside
// `delete_all_local_data_and_sign_out` — its own caller — which is `input.rs`'s one Settings
// operation that outlives the screen that asked for it; this module is where D8 asks the TEST to
// land, matching that the operation ends a live session exactly as every other exit-of-playback
// path this file owns does.
#[cfg(test)]
mod delete_local_data_tests {
    //! **Where the app lands after Delete all local data.** The branch itself is inside the SDL
    //! key loop, so the decision is lifted into [`delete_outcome`] and graded here.
    use super::*;

    /// **Reported 2026-09-02: deleting everything left the user in Settings, and BACK out of it
    /// landed on an empty Home.** Both halves are this one branch. `delete_all_local_data` erases
    /// the session unconditionally and only then reports what it could not unlink, so gating the
    /// navigation on that report meant a single leftover file stranded a signed-out app on a
    /// browsing screen — with no route back to sign-in short of relaunching.
    ///
    /// A leftover is not exotic: the candidate lists span BOTH webOS install prefixes, and the two
    /// jail profiles disagree about which of those are writable, so `EACCES`/`EROFS` on a path
    /// this profile was never going to own is an ordinary outcome on a healthy television.
    #[test]
    fn a_file_that_could_not_be_removed_still_returns_the_user_to_sign_in() {
        assert!(
            delete_outcome(0).to_sign_in,
            "a clean delete goes to sign-in"
        );
        assert!(
            delete_outcome(3).to_sign_in,
            "and so does one that left files behind — the session is gone either way"
        );
    }

    /// The leftovers are still worth saying out loud; they are just not a reason to stay put.
    #[test]
    fn leftovers_are_reported_but_a_clean_sweep_says_nothing() {
        assert!(delete_outcome(1).report_leftovers);
        assert!(!delete_outcome(0).report_leftovers);
    }
}

/// **Where playback returns to.**
///
/// `app/mod.rs`'s `player_return_tests` stood here until D1, over an `Origin::From(Node)` — a whole
/// described history entry, re-derived from the live stores at every launch site by `origin_here`,
/// because a `Route` names a KIND of page and BACK has to land on the RIGHT one. The origin is an
/// [`Origin`] now: the `EntryId` (plus instance) of the page that was on top when Play was pressed,
/// seeded at the player's own mount and read back off the mounted screen. There is nothing to
/// re-derive and no second history to reconcile, so what is graded here is the whole ritual end to
/// end — push, exit, and the page you are handed back — rather than a pure function over a
/// description of one.
///
/// The two halves the old module could not reach are here for the same reason: the entry that
/// comes back is the entry that left, so its MEMORY comes back with it (the `Spot`), and an origin
/// that is no longer on the stack is a real container state rather than an `unwrap_or`.
#[cfg(test)]
mod player_return_tests {
    use super::super::bridge::{self, AppHost, Bridge};
    use super::{enter_player, Origin};
    use crate::plex::ServerId;
    use crate::screens::registry::{AppArg, ContentArg, PageMemory};
    use crate::ui::dispatch::Dispatcher;
    use crate::ui::fixture::tick;
    use crate::ui::screen::ScreenArg;

    const A: ServerId = ServerId::from_raw(0);
    const B: ServerId = ServerId::from_raw(1);

    fn detail(sid: ServerId, rk: &str) -> AppArg {
        AppArg::Content(ContentArg::Detail { sid, rk: rk.into() })
    }
    fn person(key: &str) -> AppArg {
        AppArg::Content(ContentArg::Person {
            sid: A,
            key: key.into(),
            guid: String::new(),
            name: String::new(),
            thumb: String::new(),
        })
    }

    /// The container, driven by the app's own frame — no `App`, no SDL, no engine.
    struct Pages {
        d: Dispatcher<AppHost>,
        rig: Bridge,
        t: u32,
    }

    impl Pages {
        fn new() -> Self {
            crate::plex::reset_servers_for_test();
            Self {
                d: Dispatcher::<AppHost>::new(),
                rig: Bridge::for_test(|| 0),
                t: 0,
            }
        }

        /// Enough frames for a `PageDip` to reach its floor, apply the op and ramp back up.
        fn settle(&mut self) {
            for _ in 0..24 {
                self.t += 16;
                bridge::frame(&mut self.d, &mut self.rig, tick(self.t), vec![]);
            }
        }

        fn stand_on(&mut self, arg: AppArg) -> &mut Self {
            bridge::show_page(&mut self.d, arg);
            self.settle();
            self
        }

        fn top(&self) -> AppArg {
            self.d.top_arg().cloned().expect("a page is on top")
        }

        fn top_entry(&self) -> crate::ui::machine::EntryId {
            self.d.nav.top_page().expect("a page is on top").id
        }

        /// **Press Play** — production's own page-level ritual, called directly:
        /// `start_playback`'s `if entering { … }` block and the PLAY key's foreground start are
        /// both exactly this call. Nothing is re-typed here, which is what lets the auto-advance
        /// case below grade the real reuse rule rather than a copy of it.
        fn play(&mut self, from: Origin) {
            enter_player(&mut self.d, &mut self.rig, from, None);
            self.settle();
        }

        /// One real key press, through the app's own frame.
        fn press(&mut self, key: crate::ui::machine::Key) {
            self.t += 16;
            let at = tick(self.t);
            bridge::frame(&mut self.d, &mut self.rig, at, vec![crate::ui::fixture::key(key, at)]);
            self.settle();
        }

        /// The `Spot` an entry's return memory is holding.
        fn spot_of(&self, entry: crate::ui::machine::EntryId) -> crate::metadata::Spot {
            match &self.d.nav.entry(entry).expect("the entry is on the stack").ret.memory {
                PageMemory::Detail(memory) => memory.spot.clone(),
                other => panic!("a detail entry remembers a detail page, not {other:?}"),
            }
        }

        /// **Stop / BACK / EOS**, exactly as `playback::exit_player`'s return branch does it.
        fn back_out(&mut self) {
            match bridge::player(&self.d).and_then(|p| p.origin) {
                Some(origin) => bridge::nav_pop_to(&mut self.d, origin.entry),
                None => bridge::nav_root(&mut self.d, AppArg::Home),
            }
            self.settle();
        }
    }

    /// The rule, over every screen playback can be started from: **you come back to the page you
    /// were standing on.** Home was the one that was already right before the fix this module was
    /// written for; the other four were all landing on Home.
    ///
    /// It is one assertion per launch page and it no longer needs a `Node` alphabet to say so:
    /// the page that comes back is compared to the page that was left, as an argument.
    #[test]
    fn a_session_returns_to_the_screen_it_was_launched_from() {
        let _serial = crate::testlock::serial();
        let _session = crate::plex::session::TempSession::new("player-return-launch");
        for launched_from in [
            AppArg::Home,
            AppArg::Library,
            AppArg::Search,
            person("9"),
            detail(A, "7"),
        ] {
            let mut p = Pages::new();
            p.stand_on(launched_from.clone());
            let before = p.top_entry();
            p.play(Origin::Here);
            assert!(
                matches!(p.top(), AppArg::Player),
                "the player is the page that was pushed",
            );
            p.back_out();
            assert!(
                p.top().same_instance(&launched_from),
                "a session launched from {:?} came back to {:?}",
                launched_from.id(),
                p.top().id(),
            );
            assert_eq!(
                p.top_entry(),
                before,
                "…and to the SAME entry, not a second copy of that kind of page",
            );
        }
        crate::plex::reset_servers_for_test();
    }

    /// A detail return names the SAME ITEM that was mounted — the whole reason the origin was a
    /// `Node` and not a `Route`, and the whole reason it is an `EntryId` now. `Route::Detail`
    /// could not say which page, and by the time BACK is pressed the PLAYED leaf's own detail is
    /// what is loaded, so re-deriving the target at the exit read the wrong item by construction.
    ///
    /// The server is part of that identity for the same reason it was part of `Node`'s: with a
    /// share registered, item 7 exists on both machines and is two different films. Here that is
    /// no longer a claim about a comparison — both pages are really on the stack, and the exit
    /// picks one of them.
    #[test]
    fn a_detail_return_names_the_item_that_was_mounted() {
        let _serial = crate::testlock::serial();
        let _session = crate::plex::session::TempSession::new("player-return-detail");
        let mut p = Pages::new();
        p.stand_on(AppArg::Home);
        p.stand_on(detail(B, "7"));
        let shares_copy = p.top_entry();
        p.stand_on(detail(A, "7"));
        let ours = p.top_entry();
        assert_ne!(ours, shares_copy, "two servers' item 7 are two entries");
        p.play(Origin::Here);
        p.back_out();
        assert_eq!(p.top_entry(), ours, "the exit named the entry that was mounted");
        assert!(p.top().same_instance(&detail(A, "7")));
        crate::plex::reset_servers_for_test();
    }

    /// **The page comes back at the spot it was left at** — the half the old module could not
    /// reach at all, because `Node::Detail`'s `spot` was a payload the exit re-derived rather than
    /// a page it kept. The entry the player is pushed over is the entry the `PopTo` lands on, so
    /// the `ReturnState` it wrote on the way out (`Dispatcher::return_state` → the screen's own
    /// `memory_at`) is the one it is handed back.
    ///
    /// The cursor is moved off the hero's first control FIRST, so that both the captured `Spot`
    /// and the restored focus are things a default page would not have — an assertion that a page
    /// comes back at `col = 0` grades nothing.
    #[test]
    fn the_page_underneath_keeps_the_spot_it_was_left_at() {
        let _serial = crate::testlock::serial();
        let _session = crate::plex::session::TempSession::new("player-return-spot");
        let mut p = Pages::new();
        p.stand_on(AppArg::Home);
        p.stand_on(detail(A, "7"));
        let page = p.top_entry();
        p.press(crate::ui::machine::Key::Right);
        let left_on = p.d.focus().expect("the detail page holds the cursor");
        p.play(Origin::Here);
        let spot = p.spot_of(page);
        assert_ne!(
            spot,
            crate::metadata::Spot::default(),
            "the cursor really was moved before the session started: {spot:?}",
        );
        p.back_out();
        assert_eq!(p.top_entry(), page);
        assert_eq!(
            p.spot_of(page),
            spot,
            "the spot rode the entry through the session",
        );
        assert_eq!(
            p.d.focus(),
            Some(left_on),
            "…and so did the cursor, which is what the spot is a description of",
        );
        crate::plex::reset_servers_for_test();
    }

    /// **Up Next must not rewrite the return page.** An auto-advance starts a NEW item while the
    /// player is already up: the user chose nothing, and the page on screen is the player itself.
    ///
    /// `Origin::Unchanged` is what keeps the chain pointing at the page they actually came from,
    /// however many episodes it runs for — and since D1 the mechanism is the container's: a second
    /// `NavOp::Push` would MINT a second player entry (push has no reuse rule), mount a screen
    /// with no seed, and land the exit on Home from the second episode onward.
    #[test]
    fn auto_advance_keeps_the_page_the_user_came_from() {
        let _serial = crate::testlock::serial();
        let _session = crate::plex::session::TempSession::new("player-return-upnext");
        let mut p = Pages::new();
        p.stand_on(AppArg::Home);
        p.stand_on(detail(A, "7"));
        let show = p.top_entry();
        p.play(Origin::Here);
        let player = p.top_entry();
        for _ in 0..4 {
            p.play(Origin::Unchanged); // episode → episode → …
            assert_eq!(
                p.top_entry(),
                player,
                "an auto-advance changes the media, not the page",
            );
        }
        p.back_out();
        assert_eq!(
            p.top_entry(),
            show,
            "four auto-advances later, still the show page",
        );
        crate::plex::reset_servers_for_test();
    }

    /// **An origin that is no longer on the stack falls back to Home** — the anti-strand floor
    /// that `return_page`'s `unwrap_or(Node::Home)` was, restated over a real container by
    /// `bridge::nav_pop_to`. It is reachable rather than hypothetical: a profile switch
    /// (`Dispatcher::reset_for_profile`) retires every entry there is, so a session that outlives
    /// one holds an `EntryId` that names nothing.
    #[test]
    fn an_origin_that_is_gone_falls_back_to_home() {
        let _serial = crate::testlock::serial();
        let _session = crate::plex::session::TempSession::new("player-return-strand");
        let mut p = Pages::new();
        p.stand_on(AppArg::Home);
        p.stand_on(detail(A, "7"));
        p.play(Origin::Here);
        let stale = bridge::player(&p.d)
            .and_then(|player| player.origin)
            .expect("the mount consumed the seed");
        assert!(
            p.d.nav.entry(stale.entry).is_some(),
            "…and it named a real entry while the page was there",
        );

        // The switch every profile pick performs: the tree keeps nothing of the outgoing profile.
        bridge::switch_profile(&mut p.d);
        p.settle();
        assert!(p.d.nav.entry(stale.entry).is_none(), "the origin entry is gone");

        // A session that survived it re-enters the player still holding that id.
        p.rig.seed_player_origin(stale);
        bridge::nav_push(&mut p.d, AppArg::Player);
        p.settle();
        assert_eq!(
            bridge::player(&p.d).and_then(|player| player.origin).map(|o| o.entry),
            Some(stale.entry),
            "the stale origin really is what the exit will be asked about",
        );

        p.back_out();
        assert!(
            matches!(p.top(), AppArg::Home),
            "a stranded return lands on Home rather than on nothing: {:?}",
            p.top().id(),
        );
        crate::plex::reset_servers_for_test();
    }
}
