//! The key ladders of the non-player routes and the pointer state: OK/BACK/direction handling per
//! screen, the card activation, the onboarding and consent arms, the account and settings arms,
//! the press-and-hold path. Moved out of `app.rs` verbatim in phase 1a (a pure move;
//! `pub(super)` widening only).

use super::*;

// ui focus state lives in ui::home; reach it through its accessors
#[inline]
pub(super) fn g_fr() -> c_int {
    crate::ui::home::row()
}
#[inline]
pub(super) fn g_snap() -> f32 {
    crate::ui::home::snap_target()
}
#[inline]
pub(super) fn set_fr(v: c_int) {
    crate::ui::home::set_row(v)
}
#[inline]
pub(super) fn set_snap(v: f32) {
    crate::ui::home::set_snap_target(v)
}

/// UP/DOWN as a step of ±1, or `None` for anything else — the mapping the fresh-press ladder
/// spells out arm by arm for Settings/Consent/Legal (`sym == SDLK_UP` → `on_updown(-1)`, …),
/// pulled out so a REPEAT (a forwarded hardware auto-repeat, or one wheel tick) can reuse the
/// same mapping instead of re-deriving which sym means which direction.
pub(super) fn updown_delta(sym: c_uint) -> Option<i32> {
    if sym == SDLK_UP {
        Some(-1)
    } else if sym == SDLK_DOWN {
        Some(1)
    } else {
        None
    }
}

/// LEFT/RIGHT as a step of ±1 — `updown_delta`'s twin, for Consent's and Legal's `on_left_right`.
pub(super) fn leftright_delta(sym: c_uint) -> Option<i32> {
    if sym == SDLK_LEFT {
        Some(-1)
    } else if sym == SDLK_RIGHT {
        Some(1)
    } else {
        None
    }
}

/// The Magic Remote POINTER, as one value: which input mode the remote is in, what the
/// cursor is doing, and the two gestures that outlive a single event (a scrub drag and the
/// wheel's own debounce).
///
/// `dpad_mode`/`cur_hidden`/`mot_accum` are one rule between them and are why this is a
/// type: the first D-pad press hides the cursor and switches modes, and motion only switches
/// back once it has accumulated past the gate (see `remote_synth_ptr`, which has to defeat
/// that gate to click at all).
pub(super) struct Pointer {
    pub(super) dpad_mode: bool,  // D-pad input owns focus; pointer motion below the gate is ignored
    pub(super) cur_hidden: bool, // the LG cursor is hidden right now
    pub(super) mot_accum: f32,   // motion accumulated since D-pad mode was entered, in logical px
    pub(super) prev_mx: f32,     // last motion's position, for that accumulation (-1 = none yet)
    pub(super) prev_my: f32,
    pub(super) last_motion: u32, // last motion tick — playback hides an idle cursor off this
    pub(super) drag: bool,       // a click is dragging the HUD scrub band
    pub(super) last_wheel: u32,  // last wheel tick, for the wheel's own debounce
}
impl Pointer {
    /// Pointer mode, cursor shown, nothing held or dragging — where the loop starts.
    pub(super) const IDLE: Pointer = Pointer {
        dpad_mode: false,
        cur_hidden: false,
        mot_accum: 0.0,
        prev_mx: -1.0,
        prev_my: -1.0,
        last_motion: 0,
        drag: false,
        last_wheel: 0,
    };
}

/// The ONE home activation (OK key AND pointer click): `hf` is the hero action-row focus
/// (0 pill / 1 info, or a tab pill's packed negative) in hero view, `i32::MIN` for a
/// grid card. **The chip (-1) never arrives here** — it is the shared bar's control and both input
/// paths answer it above, in `chip_activate`.
/// Pill / Continue-Watching tiles / episodes launch playback immediately (a show or season
/// opens its page under the hood and fires its Play, which resolves the right episode +
/// resume); the info circle and ordinary grid cards open the detail page.
pub(super) unsafe fn home_activate(
    mt: &crate::task::MainThread,
    hf: c_int,
    hud_ms: u32,
    route: &mut Route,
    play_from: &mut Node,
    trail: &mut Trail,
    hud_nav: &mut HudNav,
    nav: &mut Option<NavReq>,
) {
    // every Home-originated activation clears the return trail HERE (it was hand-reset at
    // each call site before — a set-a-flag-in-N-places smell). Home is the trail's ROOT, so
    // acting on it means everything that was behind the user is spent: a page reached from a
    // person page or from the Library is as stale as any other once they are back on Home.
    trail.reset();
    // A Home with no shelves is the loading/empty/error read-out, whose only control is
    // Retry — it takes the press unless it was the top band (chip / tab pills), which
    // stay usable precisely because they are the escapes from an empty Home.
    // NB the trail is truncated ABOVE this early return: a Retry press is still the user
    // acting on Home, so a stale trail must not survive it.
    if crate::ui::home::status_activate(hf) {
        return;
    }
    let hero_view = hf != c_int::MIN;
    // a tab pill in the top band. (The grid-card sentinel is rejected by hero_pill_index
    // itself — see its doc comment.)
    if let Some(pill) = crate::ui::home::hero_pill_index(hf) {
        match crate::ui::widgets::pill_at(pill) {
            Pill::Search => nav_to(*route, Nav::Search, nav),
            // that section's grid, through the page cross-fade: `library::enter` and the
            // route flip both land at the fade floor, while the selection capsule starts
            // travelling on THIS frame (`nav::view_tab`).
            Pill::Section(kind) => nav_to(*route, Nav::Library(kind), nav),
            // Home is the screen we are on, so OK on its pill is a deliberate no-op —
            // EXCEPT that it withdraws a section switch that is still fading out: the user
            // changed their mind inside the 70 ms window, and the capsule springs back on
            // its own.
            Pill::Home => {
                nav_cancel(*route, nav);
            }
        }
        return;
    }
    let m = if hero_view {
        crate::ui::home::hero_item()
    } else {
        crate::ui::home::movie_at(crate::ui::home::row(), crate::ui::home::col())
    };
    let Some(mm) = m else { return };
    let rk = mm.rk.clone();
    if rk.is_empty() {
        return;
    }
    // **The DECK plays; every other shelf navigates.** `|| mm.kind == 3` used to be here, which
    // made an episode play immediately from ANY shelf — so "Recently Released Episodes" started
    // something the user was browsing. The rule the whole app now states is one sentence: a visual
    // play indicator means the press plays, a progress bar means viewing progress, and a card with
    // no play indicator navigates. Continue Watching is the surface that promises playback (it
    // draws the amber ▶) and it is the surface that delivers it.
    let want_play = hf == 0
        || (!hero_view && crate::pms::hub_is_continue(crate::ui::home::row().max(0) as usize));
    activate_card(mt, mm, want_play, hud_ms, route, play_from, hud_nav, nav);
}

/// **What a card ACTIVATION does, once its screen has decided whether the press means PLAY.**
///
/// Extracted from `home_activate` on 2026-09-05, when the Library grew shelves of its own and
/// therefore a Continue Watching deck of its own. Everything here is a property of the ITEM and of
/// that one boolean — a movie or episode plays; a show or season opens its page and fires Play once
/// the load has landed on the expected item; anything else opens a page, with a season selected
/// where the row names one. None of it is a property of Home, which is why forking a second copy
/// for the Library would have been two places to keep the "a failed fetch leaves the PREVIOUS
/// detail in place, so do not blindly fire on_ok" rule correct in.
///
/// `want_play` is the caller's, because only the screen knows: on Home it is the hero's Play
/// button, a deck row, or an episode tile; on the Library it is a tile on that library's own
/// `*.inprogress.*` shelf.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn activate_card(
    mt: &crate::task::MainThread,
    mm: &crate::pms::PmsMovie,
    want_play: bool,
    hud_ms: u32,
    route: &mut Route,
    play_from: &mut Node,
    hud_nav: &mut HudNav,
    nav: &mut Option<NavReq>,
) {
    let rk = mm.rk.clone();
    if want_play {
        match mm.kind {
            0 | 3 => play_item_now(
                mt,
                mm,
                false,
                origin_here(*route),
                hud_ms,
                route,
                play_from,
                hud_nav,
            ),
            _ => {
                // show / season: open its page (blocking) and fire its Play — but only
                // once the load actually landed on the expected item (a failed fetch
                // leaves the PREVIOUS detail in place; blindly firing on_ok would play
                // whatever page was open before).
                let expect = if mm.kind == 2 {
                    mm.show_rk.clone()
                } else {
                    rk.clone()
                };
                // a show/season row's parent lives on the SAME server as the row itself
                let sid = mm.sid;
                if mm.kind == 2 {
                    crate::ui::detail::open_rk_season(sid, &expect, mm.season_index);
                } else {
                    crate::ui::detail::open_rk_now(sid, &expect); // BLOCKING: `loaded` below gates the play
                }
                let loaded = crate::metadata::current()
                    .map(|d| crate::plex::same_item((d.sid, &d.rk), (sid, &expect)))
                    .unwrap_or(false);
                if loaded && crate::ui::detail::on_ok() {
                    start_playback(
                        mt,
                        crate::ui::detail::last_resume_ns(),
                        origin_here(*route),
                        hud_ms,
                        route,
                        play_from,
                        hud_nav,
                    );
                } else {
                    // nothing playable / load failed — land on the page, through the
                    // transition. `season: None`: the mount already happened above (this
                    // arm has to read the loaded item to decide at all), and `enter_node`'s
                    // re-open guard is what turns the floor's mount into a route flip.
                    nav_open(*route, to_detail(sid, &expect), None, nav);
                }
            }
        }
    } else if mm.kind == 2 {
        // season: open the SHOW page with that season selected
        nav_open(
            *route,
            to_detail(mm.sid, &mm.show_rk),
            Some(mm.season_index),
            nav,
        );
    } else if mm.kind == 3 {
        // **An episode opens its OWN page**, which is the same page `item_menu`'s "Go to Episode"
        // opens (`Action::GoToItem`) — `detail.rs` serves leaves. It used to open the SHOW's page
        // with the episode's season selected, on the reasoning that the item the tile advertised
        // should be in view; but a season tab is not the episode, and the tile the user pressed
        // named one episode. With a press on a discovery shelf now MEANING "show me this", the
        // most specific page that answers is the episode's.
        //
        // The show is still one press away: it is `item_menu`'s second navigation row, and the
        // episode page's own BACK returns to the shelf.
        nav_open(*route, to_detail(mm.sid, &rk), None, nav);
    } else {
        nav_open(*route, to_detail(mm.sid, &rk), None, nav);
    }
}

/// Perform an item-menu [`Action`](crate::ui::item_menu::Action) — the ONE dispatch shared by
/// the OK key and the pointer click, exactly like `home_activate` and `activate_ctrl_row`
/// (the two paths for the profile menu had already drifted before those were unified).
/// The menu itself only reports the choice; every route flip, server call and refresh is here.
///
/// `host` is the screen the popover was over, and it still changes what ONE action means — but the
/// question is [`MenuHost::is_loaded_episode`], not "is this the detail page". Only
/// [`MenuHost::Detail`], the episode filmstrip, holds an item that is a leaf of the loaded season:
/// its Play from Start goes through that page's own episode path and its scrobble makes the page
/// re-read itself. Every other host — Home, the Library grid, a Search shelf, a person's
/// filmography, and the detail page's own RELATED shelf, which stands on that page while its tiles
/// are OTHER items — is a card row, and they are all the same arm: the row rides in the menu
/// (`item_menu::item`) instead of being looked up in the hub catalog, which only Home's cards are
/// ever in.
pub(super) unsafe fn apply_item_action(
    mt: &crate::task::MainThread,
    act: crate::ui::item_menu::Action,
    host: MenuHost,
    route: &mut Route,
    play_from: &mut Node,
    trail: &mut Trail,
    hud_nav: &mut HudNav,
    nav: &mut Option<NavReq>,
) {
    use crate::ui::item_menu::Action;
    // WHICH SERVER this menu's rows are about — captured when the popover opened, from the
    // row it was opened on (`item_menu::SID`). Every arm below turns an rk into a fetch, a
    // scrobble or a play, and resolving one against `plex::current_server()` is the reported
    // bug itself: on a merged Continue Watching shelf, Play from Start on a friend's episode
    // found OUR row with the same key and played a different film under the friend's title.
    let sid = crate::ui::item_menu::item_sid();
    // Every arm below turns an rk into a blocking fetch or a play; an empty one would fetch
    // nothing and land on a blank page. `build` already refuses to offer such a row — this
    // is the belt to that braces, since the menu is data-driven off the hub rows.
    let rk_of = |a: &Action| match a {
        Action::GoToItem(rk)
        | Action::MarkWatched(rk)
        | Action::MarkUnwatched(rk)
        | Action::PlayFromStart(rk)
        | Action::RemoveFromDeck(rk) => rk.clone(),
        Action::GoToShow(rk, _) => rk.clone(),
        Action::None => String::new(),
    };
    if !matches!(act, Action::None) && rk_of(&act).is_empty() {
        return;
    }
    match act {
        Action::None => {}
        Action::GoToItem(rk) => {
            menu_leave(trail, host);
            nav_open(*route, to_detail(sid, &rk), None, nav);
        }
        Action::GoToShow(show_rk, season) => {
            menu_leave(trail, host);
            // the season arm is BLOCKING (it indexes the loaded show's seasons) — the same
            // trade `home_activate` makes for a season tile, now paid at the fade floor
            // where the stall is behind a screen that is already at alpha 0
            nav_open(
                *route,
                to_detail(sid, &show_rk),
                (season > 0).then_some(season),
                nav,
            );
        }
        // The two watch-state rows, and they are TWO because a part-watched item offers both:
        // `Action::watch_write` reads the verb off the ROW the user aimed at. It used to be one
        // variant carrying what the item was NOW, inverted here — which with a pair of rows would
        // give both of them the same bool and make one do the opposite of its own label.
        //
        // Otherwise the same ritual as the detail page's watch discs, and the same CODE: flip every
        // surface that describes the item at once, write on a worker, refetch the hubs when the
        // write lands so Continue Watching reflects it (a watched episode leaves the shelf; its
        // successor takes the slot).
        //
        // All three used to run inline, on this thread, justified as "~100ms LAN and deliberately
        // so". That priced one server on one LAN; with a share registered the item's server is
        // routinely remote or asleep, and the same press parked the whole UI for seconds — see
        // `crate::viewstate`, which is where the reasoning, the ordering rules and the
        // `client_for(sid)`-never-`client()` note now live.
        //
        // When the popover was over the DETAIL page, that page is the surface the user is watching,
        // so it is re-read too — the rk rides along so the filmstrip lands back on the episode that
        // changed (`detail::KEEP_EP`).
        ref a @ (Action::MarkWatched(ref rk) | Action::MarkUnwatched(ref rk)) => {
            // Unreachable by construction — this arm matches exactly the two variants
            // `watch_write` answers for — and a `return` rather than an `expect` because a
            // panic here unwinds out of the SDL loop and kills the app. If a third write row
            // is ever added to this pattern without a verb, it does nothing instead.
            let Some(w) = a.watch_write() else { return };
            // Only the FILMSTRIP's host re-reads the page: its rk is an episode of the loaded
            // season, so the tab ticks, the checks and the hero's own discs all change with it. A
            // RELATED tile is a different item — the page it is drawn on says nothing about it, and
            // asking for a refetch here would re-read the mounted show for a write that never
            // touched it. The tile's own tick is flipped by `metadata::set_watched_local` instead,
            // which walks the Related shelf for exactly this case.
            let detail = host.is_loaded_episode().then(|| rk.clone());
            // NO GUID from here, and deliberately: a catalog row carries none, and the guid the
            // detail page is holding belongs to the SHOW when this rk is one of its episodes. A
            // guid that is merely close marks a DIFFERENT title watched on every other source, so
            // `viewstate` looks the right one up from `(sid, rk)` on its own worker instead.
            crate::stores::viewstate::apply(crate::stores::viewstate::ViewStateCmd::Request { sid, rk: rk.to_string(), write: w, detail, guid: String::new() });
        }
        Action::RemoveFromDeck(rk) => {
            // A HIDE, not a reset: the server keeps the item's `viewOffset`, so the card leaves
            // the shelf while the resume point survives and playing it again picks up where it
            // left off. That is why this is NOT `unscrobble`, which would throw the position
            // away. See `plex::Client::remove_from_continue_watching`.
            //
            // The card leaves the deck on THIS frame (`pms::LocalEdit::LeftTheDeck` — it must
            // not still sit under the user's cursor after they removed it) and the refetch
            // follows the write. The shelf is sourced from `/hubs/continueWatching`, which is
            // the hub this action actually affects — built from `/hubs`'s `home.continue` it
            // would come back still listing the item (see `pms::project`).
            //
            // No detail refresh: this row exists only on a Continue Watching card.
            // No guid, and it would be ignored if there were one: a deck removal does not follow
            // the title across sources (`viewstate::Write::propagates`) — your Continue Watching
            // row is yours, and hiding a friend's item from it is not a claim about their deck.
            crate::stores::viewstate::apply(crate::stores::viewstate::ViewStateCmd::Request { sid, rk: rk.to_string(), write: crate::viewstate::Write::RemoveFromDeck, detail: None, guid: String::new() });
        }
        Action::PlayFromStart(rk) => {
            // On the detail page the target is an episode of the LOADED SEASON, which the hub
            // catalog usually doesn't hold at all (only the one Continue Watching is showing
            // ever does) — so it plays through the page's own episode path, the same one OK
            // on the still uses, with the resume dropped.
            // …the FILMSTRIP's host only. A Related tile is not among the loaded episodes, so this
            // lookup would miss and the press would do nothing — it takes the card-row arm below,
            // which plays the row the menu captured.
            if host.is_loaded_episode() {
                if crate::ui::detail::play_episode_rk_from_start(&rk) {
                    let resume = crate::ui::detail::last_resume_ns();
                    start_playback(
                        mt,
                        resume,
                        origin_here(*route),
                        HUD_LINGER_MS,
                        route,
                        play_from,
                        hud_nav,
                    );
                }
                return;
            }
            // **The row the menu was opened ON, not a re-resolve by key.** This used to walk the
            // HOME hub catalog (`pms::index_of_rk`), which is a lookup that only ever answers for a
            // card that is on a Home shelf — so on the Library grid, a Search result or a person's
            // filmography the arm found nothing and the press did nothing at all, silently. The
            // popover is about ONE item and captured it at `open`; `item_menu::ITEM` is that
            // capture, which is both the fix and the smaller claim (it also cannot be re-pointed by
            // a hub refetch rebuilding the catalog under an open panel — the reason the old lookup
            // deferred in the first place).
            //
            // The `rk` guard is what keeps the two in step: every other arm acts on the action's
            // own key, so playing a row that does not carry it would be this dispatch disagreeing
            // with itself.
            if let Some(mm) = crate::ui::item_menu::item().filter(|m| m.rk == rk) {
                play_item_now(
                    mt,
                    mm,
                    true,
                    origin_here(*route),
                    HUD_LINGER_MS,
                    route,
                    play_from,
                    hud_nav,
                );
            }
        }
    }
}

// ---- the key ladder: one function per arm ------------------------------------------------------
//
// The run loop's key handler is a LADDER: a key-up, a hardware auto-repeat and the preamble every
// fresh press runs; then ten route-scoped arms that each `continue`; then one chained `else if` on
// key identity. Each arm's BODY is a function here, in the order the ladder tries them — bar three
// with no body to name (the pointer-hidden arm is empty, Stop is one call to the exit ritual, and
// Search's body IS `search::key`; see the note at its guard).
//
// Every guard, every `continue` and the order itself stay at the CALL SITE, because the order is
// part of the behaviour: an earlier guard subsumes later ones it overlaps with — `key_player_failed`
// does, on purpose — and that is only legible while the tests sit in one list, in order, in one
// place.
//
// No host test executes any of this: it runs inside the SDL event loop. The gate over it is
// `tools/keytable.py`, which drives the simulator through (screen x key) and diffs the focus
// fingerprint each press produces against a recorded table.

/// A key-up: the reliable release (this remote sends exactly one per press). Clears this sym out of
/// both held-key slots, springs a deferred grid-card press back, and ends or debounces a scrub.
///
/// `repause_at` is handed straight to [`commit_seek`] — see its doc for what it means.
pub(super) unsafe fn on_key_up(
    sym: c_uint,
    isnav: bool,
    route: Route,
    ok_armed: bool,
    held: &mut HeldKey,
    scrubber: &mut Scrub,
    repause_at: &mut i64,
    press: &mut crate::ui::press::Press,
) {
    if sym == held.sym {
        held.sym = 0;
    }
    if sym == held.down_sym {
        held.down_sym = 0;
    }
    if is_ok(sym) && ok_armed {
        // OK released over a grid card: start the spring-back; the deferred
        // activation commits from the per-frame loop once the bounce has shown.
        press.release(clock::now());
    }
    if matches!(route, Route::Player { .. }) && scrubber.dir != 0 && isnav {
        if scrubber.reveal {
            // The press only raised the HUD (`Scrub::reveal`) and the preview never left the seed,
            // so there is nothing to commit. Tested BEFORE `hold`, not after: a hold that engaged
            // but has not travelled yet is still this case, and committing it would seek to where
            // playback already is. The advance is what retires the flag, on real travel.
            set_scrub(-1);
            scrubber.disengage();
        } else if scrubber.hold {
            log(&format!(
                "scrub: keyup commit (held) {}s",
                scrub() / 1_000_000_000
            ));
            commit_seek(scrub(), repause_at); // a held scrub → commit on release
            scrubber.disengage();
        } else {
            // a tap → commit on a short debounce so quick taps accumulate first
            scrubber.commit_at = clock::now().wrapping_add(TAP_COMMIT_MS);
        }
    }
}

/// A hardware AUTO-REPEAT (held key). Over playback the ONLY thing it drives directly is the
/// player's continuous accelerating scrub (a ramp, not a discrete move); every OTHER discrete focus
/// list — home grid, detail, track menu, info, chapters — repeats through the unified client-side
/// held-key timer in the loop, so hold-to-move feels identical everywhere and doesn't depend on the
/// remote's hardware repeat delay.
///
/// **Settings, Consent and Legal are the one exception**, and deliberately not routed through that
/// same client-side timer: they are `Popover`s layered over a `Route`, not a route themselves, so
/// their fresh-press arms (the ladder just above `settings_root_owns_input`'s call site) never call
/// `HeldKey::arm` the way `key_move_focus` does for an actual route. Rather than teach that ladder a
/// second focus-list shape, a held key's own hardware repeat is forwarded here, straight to
/// `on_updown`/`on_left_right`, in the SAME priority order the ladder tries them (consent above
/// legal above the settings root) — one ownership question, asked the same way whether the press is
/// fresh or repeating. [`RepeatGate`] throttles it: unthrottled ~50ms hardware repeats would blur
/// past rows and reading text nobody could track (item 13).
pub(super) unsafe fn on_auto_repeat(
    sym: c_uint,
    isnav: bool,
    route: Route,
    ok_armed: bool,
    hud_nav: HudNav,
    held: &mut HeldKey,
    scrubber: &mut Scrub,
    modal_repeat: &mut RepeatGate,
    press: &mut crate::ui::press::Press,
) {
    let n = clock::now();
    if held.sym != 0 && sym == held.sym {
        held.alive = n; // heartbeat: this held key's hardware repeats are still arriving
    }
    if ok_armed && is_ok(sym) {
        press.note_alive(n); // OK held: keep the dropped-key-up net honest
    }
    if matches!(route, Route::Player { .. }) && hud_nav.focus == 0 && scrubber.dir != 0 && isnav {
        scrubber.alive = n;
        scrubber.commit_at = 0; // holding → not a tap
        if !scrubber.hold {
            scrubber.hold = true;
            scrubber.hold_since = n;
            scrubber.t = n;
            // `reveal` is deliberately NOT cleared here. Engaging the hold is not the same event
            // as the preview MOVING: this block also sets `scrubber.t = n`, so the advance's first
            // pass computes `sdt ≈ 0` and travels nothing, and at ~10 s/s it takes ~100 ms before
            // the preview has moved even a second. A firm tap that trips one hardware repeat and
            // releases inside that window would otherwise commit a seek to the spot playback is
            // already sitting on — a full reopen + prime and a visible stall, out of a press the
            // reveal rule promises moves nothing. The advance clears it once there is real travel.
            log("scrub: hold engaged (0x101 repeat)");
        }
    } else if crate::ui::consent::is_open() {
        if let Some(delta) = updown_delta(sym) {
            if modal_repeat.ready(n) {
                crate::ui::consent::on_updown(delta);
            }
        } else if let Some(delta) = leftright_delta(sym) {
            if modal_repeat.ready(n) {
                crate::ui::consent::on_left_right(delta);
            }
        }
    } else if crate::ui::legal::is_open() {
        if let Some(delta) = updown_delta(sym) {
            if modal_repeat.ready(n) {
                crate::ui::legal::on_updown(delta);
            }
        } else if let Some(delta) = leftright_delta(sym) {
            if modal_repeat.ready(n) {
                crate::ui::legal::on_left_right(delta);
            }
        }
    } else if settings_root_owns_input(
        route,
        crate::ui::settings::is_open(),
        crate::ui::onboard::settings_mode(),
    ) {
        if let Some(delta) = updown_delta(sym) {
            if modal_repeat.ready(n) {
                crate::ui::settings::on_updown(delta);
            }
        }
    }
}

/// What EVERY fresh press does before the ladder sees it: remember the sym as physically down,
/// un-dismiss the HUD, abort an armed click that a non-OK key slid off, and — the LG pointer
/// convention, global to every screen including the onboarding ones the ladder dispatches first —
/// let the first D-pad press dismiss the Magic-Remote cursor and put input in D-pad mode. Pointer
/// motion brings it back.
///
/// The cursor gate takes the plain syms only (`alt: false`), which is exactly the set the four
/// spelled-out `sym ==` comparisons here took. Whether the alternate D-pad codes BELONG in it is an
/// open behavioural question — the Chapters strip accepts them and does not hide the cursor — and
/// naming the identity did not settle it.
///
/// # The unsupported-key invariant (LG checklist item 40)
///
/// **This function runs BEFORE the ladder has decided whether anything takes the press**, and two
/// of the things it does are GLOBAL rather than local to an arm: un-dismissing the player HUD, and
/// aborting a tvOS click in flight. So until 2026-08-23 an unsupported key — a colour button,
/// GUIDE, INFO, a universal remote's extra half — raised the transport over playback and cancelled
/// a press the user was in the middle of, and neither is a thing "the app ignored that key" is
/// allowed to mean. Both are now gated on [`is_bound`], whose doc carries the whole map and the one
/// place it deliberately over-approximates.
///
/// **The invariant is about CONSUMPTION, not about [`Key::Other`].** `Other` is a legitimate
/// identity for real, handled keys — the Library pager (a separate `page_dir` predicate) and
/// Search's Backspace/Clear both classify as `Other` and are then taken by hand — so making the
/// variant inert would break all of them. `is_bound` is the superset that answers the actual
/// question.
///
/// Three things stay UNCONDITIONAL and each for its own reason. `held.down_sym` is bookkeeping
/// about the physical key, not a side effect: without it a held unsupported key's auto-repeats
/// would each arrive as a fresh press (`state & 0x100 != 0 && sym == held.down_sym` in the
/// caller). The D-pad cursor gate is already narrower than `is_bound` — it takes the four plain
/// direction syms and nothing else — so it needs no second guard. And the caller's `last_input`
/// stamp is a local read only by arms that run in the same iteration, so an unbound press cannot
/// carry it anywhere.
pub(super) unsafe fn begin_fresh_press(
    key: Key,
    sym: c_uint,
    wcode: c_uint,
    now: u32,
    held: &mut HeldKey,
    hud: &mut HudState,
    ptr: &mut Pointer,
    ok_armed: &mut bool,
    press: &mut crate::ui::press::Press,
) {
    held.down_sym = sym;
    note_global_press(sym, wcode, now, hud, ok_armed, press);
    if matches!(
        key,
        Key::Up | Key::Down | Key::Left { alt: false } | Key::Right { alt: false }
    ) {
        if !ptr.dpad_mode || !ptr.cur_hidden {
            hide_cursor();
        }
        ptr.dpad_mode = true;
        ptr.cur_hidden = true;
        ptr.mot_accum = 0.0;
    }
}

/// **The two GLOBAL effects of a fresh press, and the guard that decides whether they happen** —
/// the half of [`begin_fresh_press`] that LG checklist item 40 is about, and its only caller.
///
/// Split out for one blunt reason: `begin_fresh_press` calls `hide_cursor`, which names a
/// webOS-only SDL symbol, so a host test that reaches it fails at `ld` rather than at an assertion
/// (the boundary the testing section of `docs/agent-reference.md` describes — the crate links today only
/// because nothing reachable from a test calls it and the linker dead-strips it). This half touches
/// no SDL at all, so the invariant is gradeable by `make check` instead of only by a television.
pub(super) fn note_global_press(
    sym: c_uint,
    wcode: c_uint,
    now: u32,
    hud: &mut HudState,
    ok_armed: &mut bool,
    press: &mut crate::ui::press::Press,
) {
    if !is_bound(sym, wcode) {
        return; // an unsupported key is not input the app acted on — see `begin_fresh_press`
    }
    // What the user could SEE, and only then the un-dismiss — one operation, because the order is
    // load-bearing (`HudState::note_fresh_press`). Taken for every BOUND key on every screen: it is
    // one cheap predicate, and the alternative is each player arm remembering to ask first, which
    // is exactly the ordering the pointer path had to be fixed for once already.
    hud.note_fresh_press(now);
    // a fresh non-OK key (navigation / BACK) while a click is armed aborts the press — spring the
    // card back to rest WITHOUT activating (you "slid off" the control). A key the app does not
    // bind is not sliding off anything: nothing moved, so nothing is abandoned.
    if *ok_armed && !is_ok(sym) {
        press.cancel();
        *ok_armed = false;
    }
}

/// **The unsupported-key invariant, graded.** `tools/keytable.py` grades the other half — that an
/// unbound press moves no focus on any screen — and cannot see either of these two, because neither
/// appears in a focus fingerprint.
#[cfg(test)]
mod unsupported_key_tests {
    use super::*;

    /// Drive one fresh press through [`note_global_press`] and report what it left behind:
    /// `(a click is still armed, the HUD is still dismissed)`. `press::*`, `hud_until()` and
    /// `hud_until()` and `paused()` are crate globals, so every caller holds `testlock::serial()`;
    /// the press is the test's own `Press`.
    fn press(sym: c_uint, wcode: c_uint) -> (bool, bool) {
        let mut hud = HudState::IDLE;
        let mut ok_armed = true; // a click is in flight, as if OK were still down on a card
        hud.dismissed = true; // …and the transport was hidden by hand (UP from the control row)
        let mut p = crate::ui::press::Press::new();
        p.begin(1_000);
        note_global_press(sym, wcode, 1_000, &mut hud, &mut ok_armed, &mut p);
        let out = (p.is_active() && ok_armed, hud.dismissed);
        p.cancel();
        out
    }

    /// A key the app binds behaves exactly as it always has: it un-dismisses the HUD and aborts the
    /// click it slid off. BACK is the case to use — it is not OK, so it takes the abort branch.
    #[test]
    fn a_bound_key_still_wakes_the_hud_and_aborts_the_click() {
        let _g = crate::testlock::serial();
        let (armed, dismissed) = press(SDLK_ESCAPE, 0);
        assert!(
            !armed,
            "BACK slides off the control — the press is cancelled"
        );
        assert!(!dismissed, "…and any key un-dismisses the transport");
    }

    /// **The regression.** An unsupported key must do NEITHER — it is not input the app acted on,
    /// so it may not raise the transport over playback and it may not abandon a press in flight.
    /// 269 is HOME (`SDL_SCANCODE_AC_HOME`, evdev 172 `KEY_HOMEPAGE`); every other unbound
    /// scancode takes the same branch.
    #[test]
    fn an_unsupported_key_wakes_nothing_and_abandons_nothing() {
        let _g = crate::testlock::serial();
        for (sym, wcode, what) in [
            (0, 269, "HOME"),
            (0, 270, "AC_BACK"),
            (b'a' as c_uint, 4, "a letter"),
        ] {
            let (armed, dismissed) = press(sym, wcode);
            assert!(armed, "{what} must not cancel the armed click");
            assert!(dismissed, "{what} must not un-dismiss the HUD");
        }
    }

    /// The digits are the one place [`is_bound`] deliberately over-approximates (its doc argues
    /// it): the who's-watching PIN keypad types from them, so they count as bound everywhere.
    /// Pinned so the trade-off stays a decision on record rather than something a reader finds.
    #[test]
    fn a_number_key_counts_as_bound_because_the_pin_keypad_types_from_it() {
        let _g = crate::testlock::serial();
        let (armed, dismissed) = press(b'5' as c_uint, 34);
        assert!(!armed);
        assert!(!dismissed);
    }
}

/// What a BACK press MEANS on an onboarding screen.
///
/// Two answers, not a bool, because each is decided by something different: [`OnboardBack::Screen`]
/// is *this screen still has something of its own open*, [`OnboardBack::Root`] is *nothing of this
/// app is behind this screen at all*. There was a third, `Ignore`, for a profile switch in flight —
/// retired 2026-09-04 with the reason for it: `auth::cancel` used to invalidate the switch worker
/// before deciding whether it could back out, so a refused BACK stranded the picker's spinner. A
/// refused BACK now changes nothing (`auth::cancel`'s doc), so the switch simply keeps running.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum OnboardBack {
    /// Give the key to the screen — it has a panel of its own to close.
    Screen,
    /// The ROOT press: back out to a stored session if there is one, otherwise the television's
    /// own Home.
    Root,
}

/// The rule, **pure**, so "which press is the root press on each screen" is gradeable on the host —
/// [`key_onboarding`] itself is `unsafe`, arms tvOS presses and reaches `auth`, none of which a
/// unit test wants to drive.
///
/// * **Login** — the QR sign-in. It has no panel of its own; BACK is `auth::cancel` and nothing
///   else, so every BACK here is the root press.
/// * **Profiles** — the who's-watching picker. Its PIN keypad is a modal the screen owns, and BACK
///   closes it; that is the one press here that is NOT the root press. Everything else is.
/// * **Onboard** — the first-run "which sources feed Home" question. The picker is behind it
///   (`Action::Back` → `enter_profiles_from_onboard`), so this is never a root press.
///
/// **A profile switch in flight (`Phase::Switching`) is a root press like any other on these two
/// screens, and it is safe BECAUSE `auth::cancel` no longer invalidates on refusal.** Where the
/// picker can back out (a boot picker over an unprotected stored profile) `cancel` retires the
/// switch worker through the epoch and resumes the stored session — the same answer the picker's
/// own BACK always gave. Where it refuses (a `Change profile` picker, a PIN boundary, no session
/// to back out to) nothing is touched: the switch runs on, the app goes to the television's Home,
/// and the profile it was switching to is what the user comes back to. The one press that still
/// outranks the root rule is the PIN keypad's, which closes the pad and never reaches `auth` at all
/// — a protected profile submits its PIN while the switch is already running, and that BACK is the
/// screen's own.
///
/// Anything else reaching this function is a caller bug, and `Screen` is the conservative answer:
/// worst case the press behaves as it did before this rule existed.
pub(super) fn onboarding_back(route: Route, pin_pad_open: bool) -> OnboardBack {
    match route {
        // The keypad first: its BACK closes the pad and never reaches `auth` at all, so it is safe
        // even mid-submit — and a protected profile's switch is exactly the case where the pad is
        // up and the phase is already `Switching`.
        Route::Profiles if pin_pad_open => OnboardBack::Screen,
        Route::Login | Route::Profiles => OnboardBack::Root,
        _ => OnboardBack::Screen,
    }
}

/// Is the who's-watching picker's PIN keypad up?
///
/// **Derived, because `ui::profiles` does not publish the pad** — and exactly, not approximately.
/// Both focus predicates are `!pad.open && …` (`focus_is_avatar` also wants a non-empty roster,
/// `focus_is_ctl` wants the footer), so `!avatar && !ctl` is `pad || (empty && !footer)`; ANDing the
/// non-empty roster back in leaves `pad` alone, and a pad can only be opened from a protected
/// roster tile, so the roster is never empty while it is up.
///
/// The failure direction is deliberate: if this ever answered `true` wrongly, the press falls
/// through to `profiles::key` and behaves exactly as it did before the root rule existed. A
/// `profiles::pin_pad_open()` accessor is the shape this wants to be, and is the first thing to do
/// when that module is next open.
pub(super) fn profiles_pin_pad_open() -> bool {
    !crate::ui::profiles::focus_is_avatar()
        && !crate::ui::profiles::focus_is_ctl()
        && !crate::auth::users().is_empty()
}

/// What the root press does once `auth::cancel` has answered.
///
/// A plan rather than a bool so the production code visibly OWNS each call and the log line names
/// what was decided. It used to have a third arm, `RestartAndHome`, because `auth::cancel`
/// invalidated the running sign-in BEFORE deciding whether it could back out, so a `false` on the
/// QR screen left a dead poller behind a live code and the press had to `auth::retry` on the way
/// out. That ordering was issue #30 and is gone: a refused `cancel` changes nothing (`auth::cancel`'s
/// doc, `a_refused_back_leaves_the_live_pin_poll_running`), so the flow it refused to leave is
/// still running and a restart here would DISCARD it — a fresh code over a poll the user's phone may
/// already have answered. Retired 2026-09-04 on Codex's integrated review.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum AfterCancel {
    /// There was somewhere to go inside the app; the main loop's phase→route follower takes it
    /// from here. Nothing to ask the platform for.
    BackedOut,
    /// Nowhere to go inside the app, and nothing was disturbed. Straight to the television's Home.
    Home,
}

/// The whole of the rule, pure so the pairing with the log line is gradeable: a `cancel` that
/// backed out is not a root press after all; one that refused leaves everything as it was and hands
/// the screen to the television.
pub(super) fn after_cancel(backed_out: bool) -> AfterCancel {
    if backed_out {
        AfterCancel::BackedOut
    } else {
        AfterCancel::Home
    }
}

/// Onboarding screens (login / who's-watching / the Home-sources question) own every fresh key —
/// nothing is behind them, so route the key to the active screen and skip all other handlers.
///
/// Returns the source-picker action. Login and Who's Watching remain worker-driven and therefore
/// report `None`; Shared Sources reports an explicit commit, Settings cancellation, or first-run
/// BACK as three different outcomes so dismissal can never be mistaken for an answer.
pub(super) unsafe fn key_onboarding(
    route: Route,
    sym: c_uint,
    wcode: c_uint,
    ok_armed: &mut bool,
    press: &mut crate::ui::press::Press,
) -> crate::ui::onboard::Action {
    // **BACK at one of these screens' own ROOT is the root press** — the same one Home's is, and
    // for the same reason: nothing of this app is behind it. Issues #17 and #18 are both this
    // branch missing. Both screens used to hand BACK to `auth::cancel`, whose whole job is to back
    // out to a stored session; when there is no session to back out TO it reports `false` and the
    // press was simply DROPPED — on the boot picker with a PIN-protected profile, on the roster
    // straight after a sign-out, and on the QR sign-in of a first-ever launch, which is the first
    // screen a new user ever sees. `auth::cancel` still decides whether there is somewhere to go
    // INSIDE the app; only its `false` is now answered instead of ignored.
    //
    // Pre-empting the screen's own handler (rather than adding a fallback behind it) is exact, not
    // a shortcut: at these two stops BACK reaches `auth::cancel` and nothing else. What it does
    // reach first — the profile picker's PIN keypad — is why [`onboarding_back`] exists and why
    // this is not a bare `is_back`.
    if is_back(sym, wcode) {
        match onboarding_back(route, profiles_pin_pad_open()) {
            // the screen's own handler has it — fall through to the dispatch below
            OnboardBack::Screen => {}
            OnboardBack::Root => {
                // **The latch is taken HERE, before `auth::cancel`, and not inside
                // `webos::go_home`.** A `cancel` that CAN back out is destructive (it retires the
                // worker and resumes the stored session), so rate-limiting only the platform call
                // would still let a burst of taps back out once per press. One root press per
                // cooldown means one `cancel`.
                if crate::webos::take_root_press() {
                    let backed_out = crate::auth::cancel();
                    let phase = crate::auth::phase();
                    let plan = after_cancel(backed_out);
                    // ONE line saying what this press decided and on what evidence, for the person
                    // reading the device log who is not the person who wrote this. The phase is
                    // evidence, not an input: it says what the refused press left running. Every
                    // field is an enum name: no identity, no content.
                    crate::log(&format!(
                        "back: root route={} phase={phase:?} backed_out={backed_out} action={plan:?}",
                        if matches!(route, Route::Login) {
                            "login"
                        } else {
                            "profiles"
                        }
                    ));
                    match plan {
                        // …and a press that turned out to have somewhere to go INSIDE the app was
                        // never a root press, so it hands the claim straight back rather than
                        // swallowing the real root BACK the user is about to press on the Home it
                        // just returned to.
                        AfterCancel::BackedOut => crate::webos::release_root_press(),
                        AfterCancel::Home => crate::webos::go_home(),
                    }
                }
                return crate::ui::onboard::Action::None;
            }
        }
    }
    if matches!(route, Route::Onboard) {
        // The action PILL is a control face (`onboard`'s `ACTION_POP`) → tvOS press, committed on
        // the spring-back by `commit_onboarding`. A `TableView` row is not a control face and keeps
        // flipping its pin on the key-down.
        if is_ok(sym) && crate::ui::onboard::focus_is_ctl() {
            // Record WHICH action is being pressed, not merely that one is: the roster can land
            // during the spring-back and turn `Try again` into `Start watching` under the same
            // focus stop — see `onboard::ActionKind`.
            crate::ui::onboard::arm_action();
            press.begin_ctl(clock::now());
            *ok_armed = true;
            return crate::ui::onboard::Action::None;
        }
        return crate::ui::onboard::key(sym, wcode);
    }
    if matches!(route, Route::Profiles) {
        // BOTH of this screen's press surfaces defer, for the one reason: each has a spring that
        // folds `press::scale()` in and so has a dip to show. The roster avatar is a card
        // (`card_row`'s), the Sign-out footer is a control face (`FOOTER_POP`) — which is why they
        // arm through different doors and commit through one, `profiles::activate_focused`. The PIN
        // keypad's keys have neither and act on the key-down.
        if is_ok(sym) && crate::ui::profiles::focus_is_avatar() {
            press.begin(clock::now());
            *ok_armed = true;
        } else if is_ok(sym) && crate::ui::profiles::focus_is_ctl() {
            press.begin_ctl(clock::now());
            *ok_armed = true;
        } else {
            crate::ui::profiles::key(sym, wcode);
        }
    } else {
        crate::ui::login::key(sym, wcode);
    }
    crate::ui::onboard::Action::None
}

/// Every non-None answer leaves this instance of the source picker, but WHERE it leaves is retained
/// in the action: Done/Cancel go forward or back to Settings; first-run Back goes to Profiles.
#[cfg(test)]
pub(super) fn onboarding_action_leaves(action: crate::ui::onboard::Action) -> bool {
    matches!(
        action,
        crate::ui::onboard::Action::Done
            | crate::ui::onboard::Action::Back
            | crate::ui::onboard::Action::Cancel
    )
}

/// Settings remains open behind its Home editor, but it must not own input while that child route
/// is visible. The draw stack and input stack must answer the same ownership question.
pub(super) fn settings_root_owns_input(route: Route, settings_open: bool, home_editor: bool) -> bool {
    settings_open && !(matches!(route, Route::Onboard) && home_editor)
}

#[cfg(test)]
mod settings_child_input_tests {
    use super::*;

    #[test]
    fn home_editor_owns_input_while_settings_remains_open_behind_it() {
        assert!(!settings_root_owns_input(Route::Onboard, true, true));
        assert!(settings_root_owns_input(Route::Home, true, false));
        assert!(!settings_root_owns_input(Route::Home, false, false));
    }

    #[test]
    fn back_cancel_leaves_the_settings_home_editor() {
        assert!(onboarding_action_leaves(crate::ui::onboard::Action::Cancel));
        assert!(onboarding_action_leaves(crate::ui::onboard::Action::Done));
        assert!(onboarding_action_leaves(crate::ui::onboard::Action::Back));
        assert!(!onboarding_action_leaves(crate::ui::onboard::Action::None));
    }
}

/// Commit the onboarding-question screen's focused stop — the deferred half of [`key_onboarding`]'s
/// `Onboard` arm. Returns what [`key_onboarding`] returns: whether the flow is finished and the
/// caller should route Home.
pub(super) fn commit_onboarding() -> crate::ui::onboard::Action {
    crate::ui::onboard::on_ok()
}

/// BACK from Shared Sources returns to the identity step and records no source answer. Starting a
/// ChangeProfile flow re-seeds the roster from the persisted session immediately, so this is a
/// real usable picker rather than a static screen with no worker behind it.
pub(super) fn enter_profiles_from_onboard() -> Route {
    crate::auth::start_switch(crate::auth::Picker::ChangeProfile);
    crate::ui::profiles::enter();
    Route::Profiles
}

pub(super) fn apply_onboarding_action(action: crate::ui::onboard::Action, trail: &mut Trail) -> Option<Route> {
    match action {
        crate::ui::onboard::Action::None => None,
        crate::ui::onboard::Action::Back => Some(enter_profiles_from_onboard()),
        crate::ui::onboard::Action::Done | crate::ui::onboard::Action::Cancel => {
            Some(enter_home_from_onboard(trail))
        }
    }
}

/// Leave the first-run question for Home.
///
/// The trail is RESET rather than pushed to: this route is the last of the onboarding gates and
/// Home is the root behind it, so a BACK from Home must reach the ROOT PRESS exactly as it does on
/// any other boot — not walk back into a question that has already been answered. (That press is
/// [`back_at_root`], the television's own Home; what this reset guarantees is that Home is still the
/// root when it lands, which is what puts the user one BACK from it either way.) `enter` is what the
/// route's own BACK and its `Start watching` both come through, which is why there is one exit and
/// not two.
/// Put the telemetry question on screen, if this boot is one that should see it.
///
/// **Asked as soon as there is an AUTHORIZED ACCOUNT, and before the profile picker.**
///
/// The decision belongs to the SIGN-IN — `telemetry_candidates()` is one file with no profile
/// key, shared by every profile on the account, and `auth::forget_account` unlinks it when the
/// account signs out — so the person who signed the television in is the person who should answer
/// it, and the next account to sign in is asked afresh. Asking after the picker (which is what
/// shipped until 2026-09-02) put a data-protection question to whichever household member
/// happened to be selected, up to and including a managed child profile, and dressed an
/// account-wide answer as a personal setting.
///
/// It is still not asked at BOOT: a fresh install boots to the QR screen with nothing to consent
/// about yet, and asking before somebody has managed to sign in is asking while they have nothing
/// to lose by walking away.
///
/// Cheap and idempotent: `should_show` is false once a decision has been recorded, and false on any
/// automated boot, so every call site can simply ask. Nothing is stored by asking.
pub(super) fn maybe_ask_consent() {
    let c = crate::telemetry::consent::current().unwrap_or_default();
    // dev: /tmp/plxnative-consent[=<crash|product>] forces either first-run purpose even on an
    // automated boot. This screen is suppressed BY the presence of any trigger, so without an
    // override it cannot be reached headlessly at all. Selecting Product changes display state
    // only; no answer is stored by a harness boot.
    if let Some(target) = crate::dev::read("consent") {
        // `fresh` for the same reason `open` is idempotent: this is now asked from a per-frame
        // routing site, and re-selecting the second purpose every frame would PIN the stage there
        // and make the seam undriveable.
        let fresh = !crate::ui::consent::is_open();
        crate::ui::consent::open(&c);
        if fresh && target.trim() == "product" {
            crate::ui::consent::show_product_for_dev();
        }
        return;
    }
    if crate::ui::consent::should_show(&c, crate::dev::any_trigger_present()) {
        crate::ui::consent::open(&c);
    }
}

pub(super) fn enter_home_from_onboard(trail: &mut Trail) -> Route {
    if crate::ui::onboard::settings_mode() {
        crate::ui::settings::refresh();
        let saved = unsafe {
            let p = std::ptr::addr_of_mut!(SETTINGS_HOME_RETURN);
            let saved = p.read().unwrap_or(Route::Home);
            p.write(None);
            saved
        };
        crate::ui::onboard::finish_settings();
        return saved;
    }
    trail.reset();
    // The consent pair is NOT asked here any more: it is the sign-in's decision, shared by every
    // profile on the account, and is put before the profile picker, which is upstream of this whole step. See `maybe_ask_consent`.
    // The selection just recorded is an input to Home's merge (`pms::feeds_home`), and the merge
    // re-runs off `browse`'s section generation — which `apply_pins` (the editor's one commit
    // write) has already bumped. Nothing to kick here; Home builds from the answer on its first
    // frame.
    Route::Home
}

/// The profile menu is modal — rows nav, OK commits, BACK closes back to `over`: the page the chip
/// was pressed on, which is any of the three that wear the shared top bar.
pub(super) fn key_account(over: BarHost, sym: c_uint, wcode: c_uint, route: &mut Route) {
    if is_ok(sym) {
        match crate::ui::account_menu::on_ok() {
            crate::ui::account_menu::Action::ChangeProfile => {
                crate::auth::start_switch(crate::auth::Picker::ChangeProfile);
                crate::ui::profiles::enter();
                *route = Route::Profiles;
            }
            crate::ui::account_menu::Action::SignIn => {
                crate::auth::start_login();
                crate::ui::login::enter();
                *route = Route::Login;
            }
            crate::ui::account_menu::Action::SignOut => {
                crate::auth::sign_out();
                crate::ui::login::enter();
                *route = Route::Login;
            }
            // Opens the Settings popover over the same page, so the ROUTE does not move. Its
            // Privacy and Legal children take the key ladder while they are open, then reveal this
            // root again. Reachable signed OUT as well: a person who cannot sign in has still
            // received a copy of this software, and LG requires the privacy notice to be readable
            // in the app rather than only on the store listing.
            crate::ui::account_menu::Action::Settings => {
                crate::ui::settings::open();
                *route = over.route();
            }
            // Lab builds only, and it changes no route: the tester stays where they were, and the
            // toast says what happened. Returning to the page the popover stood on is the same
            // dismissal `Action::None` does.
            crate::ui::account_menu::Action::SendDiagnostics => {
                crate::lab::request_upload("menu");
                *route = over.route();
            }
            // …and a dismissal returns to the PAGE the popover is standing on, not to Home. It
            // said Home outright while Home was the only screen whose chip could be pressed.
            crate::ui::account_menu::Action::None => *route = over.route(),
        }
    } else if is_back(sym, wcode) {
        crate::ui::account_menu::close();
        *route = over.route();
    } else {
        crate::ui::account_menu::move_focus(sym as c_int);
    }
}

pub(super) fn perform_settings_action(action: crate::ui::settings::Action, route: &mut Route) {
    match action {
        crate::ui::settings::Action::Home => {
            unsafe { SETTINGS_HOME_RETURN = Some(*route) };
            crate::ui::onboard::enter_settings();
            *route = Route::Onboard;
        }
        crate::ui::settings::Action::Privacy => {
            let current = crate::telemetry::consent::current().unwrap_or_default();
            crate::ui::consent::open_settings(&current);
        }
        crate::ui::settings::Action::Legal => crate::ui::legal::open(),
        crate::ui::settings::Action::About => crate::ui::legal::open_about(),
        crate::ui::settings::Action::None => {}
    }
}

/// What a confirmed **Delete all local data** does next, given how many files could not be
/// unlinked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct DeleteOutcome {
    /// Leave for the sign-in screen.
    pub(super) to_sign_in: bool,
    /// Write the leftovers to the event log.
    pub(super) report_leftovers: bool,
}

/// **Two independent facts, and conflating them was the bug.**
///
/// [`delete_all_local_data`] calls `auth::erase_local_state()` UNCONDITIONALLY and only then
/// returns whatever it failed to unlink, so by the time this is asked the session is already gone.
/// Routing on the cleanup result therefore answered the wrong question: one unremovable file — and
/// the candidate lists span BOTH install prefixes, whose jail profiles disagree about which are
/// writable, so a leftover is an ordinary outcome on a healthy set — left the user sitting in
/// Settings on top of an app that had just signed itself out. The next BACK dropped them onto an
/// empty Home with no session, no servers and no route to sign-in short of relaunching.
///
/// It is a function rather than a branch because the branch lives inside the SDL key loop, where
/// no host test can reach it.
pub(super) fn delete_outcome(leftovers: usize) -> DeleteOutcome {
    DeleteOutcome {
        to_sign_in: true,
        report_leftovers: leftovers > 0,
    }
}

/// The one destructive Settings operation. Individual UI rows never remove their own files.
///
/// Returns the paths it could NOT unlink — a report, never a verdict. The irreversible half
/// (`auth::erase_local_state`) runs whatever the file sweep managed; see [`delete_outcome`].
pub(super) fn delete_all_local_data() -> Vec<String> {
    let remove = |path: &std::path::Path| match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    };
    let mut failures = Vec::new();
    for path in crate::paths::obsolete_last_place_candidates()
        .into_iter()
        .chain(crate::paths::telemetry_candidates())
        .chain(crate::paths::telemetry_spool_candidates())
        .chain(crate::paths::telemetry_crashmark_candidates())
    {
        if let Err(e) = remove(&path) {
            failures.push(e);
        }
    }
    for name in [
        "plxnative-events.log",
        "plxnative-crash.log",
        "plxnative-stderr.log",
        "plxnative-anim.log",
        "plxnative-gst.log",
        "plxnative-gputime.jsonl",
        "plxnative-hwcnt.jsonl",
    ] {
        if let Err(e) = remove(&crate::paths::in_runtime_dir(name)) {
            failures.push(e);
        }
    }
    crate::ui::search::recents::clear();
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
    // The telemetry decision, both identifiers, the spool and the native backend go with the
    // account: `erase_local_state` → `forget_account` → `telemetry::forget`, the same door
    // Sign out uses. The sweep above already unlinked the files; `forget` finds them gone.
    crate::auth::erase_local_state();
    failures
}

/// The press-and-hold item menu is modal too — rows nav, OK commits, BACK closes back to the shelf
/// (or filmstrip) the card is still sitting on. `over` is the screen it is a popover on.
pub(super) unsafe fn key_item_menu(
    mt: &crate::task::MainThread,
    over: MenuHost,
    sym: c_uint,
    wcode: c_uint,
    now: u32,
    route: &mut Route,
    play_from: &mut Node,
    trail: &mut Trail,
    hud_nav: &mut HudNav,
    nav: &mut Option<NavReq>,
    held: &mut HeldKey,
) {
    if is_ok(sym) {
        let act = crate::ui::item_menu::on_ok();
        *route = over.route(); // the dispatch overrides this when it navigates/plays
        apply_item_action(mt, act, over, route, play_from, trail, hud_nav, nav);
        held.sym = 0; // an async route flip must not repeat a held key into the next screen
    } else if is_back(sym, wcode) {
        crate::ui::item_menu::close();
        *route = over.route();
    } else if sym == SDLK_UP || sym == SDLK_DOWN {
        // move once on the fresh press; holding repeats via the shared
        // client-side timer. Armed ONLY for the two keys the menu acts on, so a
        // held key it ignores can't sit in `HeldKey::sym` driving a per-frame
        // no-op.
        crate::ui::item_menu::move_focus(sym as c_int);
        held.arm(sym, now);
    }
}

/// D-pad on a NON-player screen: hand the direction to whichever screen owns focus, then arm the
/// client-side hold-repeat.
pub(super) fn key_move_focus(key: Key, sym: c_uint, route: Route, now: u32, held: &mut HeldKey) {
    if matches!(route, Route::Detail) {
        crate::ui::detail::move_focus(sym as c_int);
    } else if matches!(route, Route::Person) {
        crate::ui::person::move_focus(sym);
    } else if matches!(route, Route::Library) {
        crate::ui::library::move_focus(sym);
    } else if matches!(route, Route::Search) {
        crate::ui::search::move_focus(sym);
    } else if g_snap() < 0.5 {
        if matches!(key, Key::Down) {
            if crate::ui::home::hero_focus() < 0 {
                crate::ui::home::set_hero_focus(0); // chip → back to the action row
            } else {
                set_snap(1.0);
                set_fr(0);
            }
        } else if matches!(key, Key::Left { alt: false } | Key::Right { alt: false }) {
            crate::ui::home::home_hero_key(sym); // walk the action row; RIGHT at its end pages
        } else if matches!(key, Key::Up) {
            // hero view: UP focuses the profile chip (OK then opens the menu —
            // the chip is selectable, it no longer springs the menu unbidden)
            crate::ui::home::set_hero_focus(-1);
        }
    } else if matches!(key, Key::Up) && g_fr() == 0 {
        set_snap(0.0);
    } else {
        crate::ui::home::home_move_focus(sym);
    }
    held.arm(sym, now);
}

/// **Where the SHARED top bar's focus is, for the route that is up.** The bar is one control across
/// Home, the Library and Search, so the question is asked once here rather than three times — and
/// every other route has no bar at all, which is what `TopFocus::Away` says.
///
/// It exists because of the CHIP. A pill's press leads somewhere that depends on the screen you are
/// standing on (Home's own pill is a no-op, the Library's is a tab switch), so each screen still
/// performs its own; the chip's press is the account menu wherever you are, so it is answered once,
/// in [`chip_activate`], off this one answer.
pub(super) fn top_focus(route: Route) -> crate::ui::widgets::TopFocus {
    use crate::ui::widgets::TopFocus;
    match route {
        Route::Home => crate::ui::home::top_focus(),
        Route::Library => crate::ui::library::top_focus(),
        Route::Search => crate::ui::search::top_focus(),
        _ => TopFocus::Away,
    }
}

/// The profile chip's activation, shared by the OK key and the pointer click — the top bar is one
/// control on three screens and this is the one thing it does.
///
/// It deliberately does NOT go through `home_activate`'s `trail.reset()` the way Home's chip press
/// used to: the account menu is a POPOVER over whatever page is showing, not a navigation, and on
/// the Library or Search a reset would throw away history the user is still standing on. (At Home
/// the reset was a no-op anyway — arriving at Home is itself the trail's reset, so the stack there
/// is already just the root.)
///
/// The popover therefore records the page it OPENED ON ([`BarHost`]), which is what keeps that
/// sentence true: `Route::Account` used to be a unit variant meaning "Home, plus the panel", so a
/// press on the Library's chip swapped the page underneath to Home on the press frame and dropped
/// the user there when they dismissed it. A route with no chip on it opens nothing.
pub(super) fn chip_activate(route: &mut Route) {
    let Some(over) = BarHost::of(*route) else {
        return;
    };
    // On Search, the television's own keyboard may still be up — it is a SYSTEM panel, so a modal
    // of ours neither covers nor suppresses it, and the page under a popover keeps updating, so its
    // characters would go on landing in the field behind the menu. Only the pointer can reach the
    // chip from inside the field (the D-pad leaves it through `leave_field`, which commits), so
    // this is that path's half of the same rule.
    if matches!(over, BarHost::Search) {
        crate::ui::search::end_editing();
    }
    crate::ui::account_menu::open();
    *route = Route::Account { over };
}

/// Did this click land on the profile chip of a screen that is WEARING the shared bar? The pointer
/// twin of [`chip_activate`]'s key path, and the route test is the whole of what makes it safe:
/// `widgets::CHIP_FRAME` is a constant (the chip never moves), so nothing else bounds it to the
/// screens that actually draw one.
pub(super) fn chip_clicked(route: Route, ev: &[u8]) -> bool {
    if BarHost::of(route).is_none() {
        return false;
    }
    // A screen's own modal owns the frame, and the Library's sort/filter panel is INTERNAL state
    // rather than a route, so nothing above this can see it: without the test, a click on the
    // avatar with that panel up would open the account popover over a menu still standing behind
    // it. The key path needs no equivalent — `library::top_focus` already declines while a menu is
    // open, so the chip is not the focused thing to press.
    if matches!(route, Route::Library) && crate::ui::library::menu_open() {
        return false;
    }
    let (mx, my) = ptr_xy(ev);
    crate::ui::widgets::profile_chip_at(mx, my)
}

pub(super) unsafe fn key_ok(
    mt: &crate::task::MainThread,
    now: u32,
    route: &mut Route,
    hud: &mut HudState,
    ptr: &mut Pointer,
    trail: &mut Trail,
    nav: &mut Option<NavReq>,
    play_from: &mut Node,
    ok_armed: &mut bool,
    press: &mut crate::ui::press::Press,
) {
    // The shared top bar's PROFILE CHIP, ahead of the per-route ladder below: it is one control on
    // three screens and its destination never depends on which of them you are standing on, which
    // is exactly why each screen used to draw it and only Home could press it.
    if matches!(top_focus(*route), crate::ui::widgets::TopFocus::Chip) {
        chip_activate(route);
        return;
    }
    if matches!(*route, Route::Player { .. }) {
        // the pre-press sample, like the other two player arms — `begin_fresh_press` has already
        // cleared `dismissed`, so re-asking calls a hand-hidden transport visible and this arm
        // would open a panel from behind it (`HudState::visible_at_press`)
        let vis = hud.visible_at_press;
        // Row 1 is the transport's CONTROL ROW — the Subtitles / Audio / ⋯ discs, or whichever
        // stand-in has taken their place (Skip, Up Next). Every occupant is a control FACE with a
        // pop of its own (`player_hud::ROW_POP`), so OK takes the tvOS press: dip now, act on the
        // spring-back, in `activate_player_row` from the per-frame loop. Both of its arms open
        // something OVER this HUD rather than leaving the route, which makes this the one control
        // row in the app where the whole dip → ring is on screen either side of the activation.
        if vis && hud.nav.focus == 1 {
            press.begin_ctl(now);
            *ok_armed = true;
        } else if vis && hud.nav.focus == 2 {
            if hud.nav.tab == 0 {
                crate::ui::info_panel::open(); // Info card
                *route = Route::Player {
                    overlay: Overlay::Info,
                };
            } else if hud.nav.tab == 1 {
                crate::ui::chapters_panel::open(); // Chapters strip
                *route = Route::Player {
                    overlay: Overlay::Chapters,
                };
            }
        } else {
            let np = !paused();
            if np {
                if set_transport_paused(mt, true) {
                    crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
                        feature: crate::diag::schema::Feature::Pause,
                    });
                }
            } else {
                set_transport_paused(mt, false);
            }
        }
        extend_hud(now, HUD_LINGER_MS);
    } else if matches!(*route, Route::Search) {
        // A result tile takes the tvOS press (dip now, commit on the spring-back
        // — `ok_armed` runs `on_ok` then); the field and the recents rows commit
        // immediately inside the screen.
        // the pill under the ring, off the same one answer `tab_row_update` animates the bar from
        // (the CHIP half of it was already spent above, in `key_ok`'s own opening arm)
        if let crate::ui::widgets::TopFocus::Pill(spill) = top_focus(*route) {
            match crate::ui::widgets::pill_at(spill) {
                // the screen we are already on — a deliberate no-op, as Home's
                // own pill is on Home
                Pill::Search => {}
                Pill::Section(kind) => nav_to(*route, Nav::Library(kind), nav),
                // focus lands on the Home pill, which is the pill Home selects
                // anyway — the strip must not appear to move under the swap
                Pill::Home => nav_to(
                    *route,
                    Nav::Home {
                        focus_pill: Some(crate::ui::widgets::Pill::Home),
                    },
                    nav,
                ),
            }
        } else if crate::ui::search::focus_is_card() {
            press.begin(clock::now());
            *ok_armed = true;
        } else if let crate::ui::search::Action::Open(node) = crate::ui::search::on_ok() {
            nav_open(*route, node, None, nav);
        }
    } else if matches!(*route, Route::Library) {
        // OK on a browse-grid card → the same tvOS press as home's grid;
        // tabs / toolbar / menus commit immediately inside the screen.
        if crate::ui::library::focus_is_card() {
            press.begin(clock::now());
            *ok_armed = true;
        } else {
            match crate::ui::library::on_ok() {
                crate::ui::library::Action::GoHome => nav_to(
                    *route,
                    Nav::Home {
                        focus_pill: crate::ui::library::focused_pill(),
                    },
                    nav,
                ),
                crate::ui::library::Action::GoSearch => nav_to(*route, Nav::Search, nav),
                // A SHELF tile, which the grid's own `Card` arm cannot serve: it is not in the
                // paged store, and a tile on the library's own Continue Watching row must RESUME
                // rather than open a page. Same `activate_card` Home's deck goes through.
                crate::ui::library::Action::ShelfCard { from_deck } => {
                    if let Some(mm) = crate::ui::library::focused_item() {
                        // the DECK plays; every other shelf navigates — see `home_activate`
                        let want_play = from_deck;
                        unsafe {
                            activate_card(
                                mt, mm, want_play, HUD_LINGER_MS, route, play_from, &mut hud.nav,
                                nav,
                            )
                        };
                    }
                }
                crate::ui::library::Action::Card | crate::ui::library::Action::None => {}
            }
        }
    } else if matches!(*route, Route::Detail) {
        // OK on a detail CARD (episode / Related / Cast) → tvOS press: dip now,
        // commit on the spring-back (the route-agnostic press handler runs on_ok
        // then). So does the hero's CONTROL ROW — the same press with the hold
        // gesture left off, since no context menu grows out of a Play pill.
        // Season tabs, About rows and the filmstrip's metadata block still
        // activate immediately: none of them draws `press::scale()`.
        if crate::ui::detail::focus_is_card() {
            press.begin(clock::now());
            *ok_armed = true;
        } else if crate::ui::detail::focus_is_ctl() {
            press.begin_ctl(now);
            *ok_armed = true;
        } else if crate::ui::detail::on_ok() {
            start_playback(
                mt,
                crate::ui::detail::last_resume_ns(),
                origin_here(*route), // Stop/BACK/EOS returns to this detail page
                HUD_LINGER_MS,
                route,
                play_from,
                &mut hud.nav,
            );
        }
    } else if matches!(*route, Route::Person) {
        // every focusABLE thing on the person page is a poster card → the
        // same tvOS press as home's grid, committed on the spring-back
        if crate::ui::person::focus_is_card() {
            press.begin(clock::now());
            *ok_armed = true;
        } else {
            // …and the page's two CARDLESS focus rows: the HEADER, where OK
            // opens the bio alert when there is more biography than the band
            // shows, and the FILMOGRAPHY ENTRY at the end of the shelves, where
            // it replaces the page with that route. No press is armed for
            // either, because a tvOS dip needs something to dip — neither the
            // band nor a row spanning the frame draws `press::scale()` — and
            // waiting for a spring-back nobody can see would only add latency.
            // `person::header_ok` owns every test (which row is it, is the bio
            // actually truncated, is there a filmography at all) and answers
            // false when it did nothing.
            crate::ui::person::header_ok();
        }
    } else {
        // home: dispatch through the ONE activation (shared with pointer
        // clicks). Gate hero-vs-grid on the spring POSITION (what's on
        // screen), not the snap target: a DOWN press flips the target to grid
        // instantly while the hero stays visible ~130ms, so a quick DOWN→OK
        // must still act on the hero shown, not the grid's card 0.
        if crate::ui::home::snap_pos() < 0.5 {
            // hero: its ACTION ROW (the Play/Continue pill, the info disc) takes
            // the tvOS press like a card, with the hold gesture left off — the
            // commit below re-reads `hero_focus` and hands it to this same
            // activation. The rest of the hero band activates immediately: the
            // top band's pills are controls in a TRACK and the status read-out's
            // Retry belongs to no `CtlPop`, so neither has a dip to show
            // (`home::focus_is_ctl`).
            if crate::ui::home::focus_is_ctl() {
                press.begin_ctl(now);
                *ok_armed = true;
            } else {
                let hf = crate::ui::home::hero_focus();
                home_activate(
                    mt,
                    hf,
                    HUD_LINGER_MS,
                    route,
                    play_from,
                    trail,
                    &mut hud.nav,
                    nav,
                );
            }
        } else {
            // grid card: tvOS press — dip the focused card now, activate on the
            // spring-back (committed from the per-frame loop). Nav cancels, so the
            // focused cell can't move while the press is armed.
            press.begin(clock::now());
            *ok_armed = true;
        }
        if !ptr.dpad_mode {
            hide_cursor();
            ptr.dpad_mode = true;
            ptr.cur_hidden = true;
        }
    }
}

/// CH▲/CH▼ page the browse grid a screenful of rows per press.
pub(super) fn key_library_page(dir: c_int) {
    crate::ui::library::page(dir);
}

/// webOS BACK: this Magic Remote sends wcode 482 (0x1E2); 461 kept for others.
///
/// Back stack: player -> the TRAIL (detail/person, at any depth) -> library -> grid -> hero ->
/// exit. Inside the Library, BACK first walks menu -> tab bar (library::back), THEN leaves to Home.
/// The ORDER is unchanged; what changed is that detail/person pop a real trail (`ui::trail`)
/// instead of consulting two booleans that had one slot per screen KIND and so could not describe a
/// detail page standing on another one.
///
/// A BACK inside the page fade's 70 ms window WITHDRAWS the transition rather than acting on a
/// screen that is already half gone: the request is at most four frames old and nothing has changed
/// yet, so it can still be un-asked. `nav_cancel` refuses once the swap has happened, and then this
/// is an ordinary BACK on the NEW screen — the press is never dropped, only ever spent on exactly
/// one of the two. (It matters most at Home's root, where "what BACK would otherwise do" is hand
/// the screen back to the television — see [`back_at_root`].)
pub(super) fn key_back(
    mt: &crate::task::MainThread,
    route: &mut Route,
    nav: &mut Option<NavReq>,
    trail: &mut Trail,
    play_from: &Node,
    refresh_hubs_at: &mut u32,
) {
    if nav_cancel(*route, nav) {
    } else if matches!(*route, Route::Player { .. }) {
        exit_player(mt, route, play_from, refresh_hubs_at, trail);
    } else if matches!(*route, Route::Detail | Route::Person) {
        // The two stacking screens, through the page transition. All three
        // halves of the pop — the outgoing page's teardown, the trail move and
        // the re-entry — land together at the fade FLOOR (`nav_back`), because
        // a pop is always all three and splitting them across the 70 ms window
        // is how you get a page blanking during its own fade-out or a second
        // BACK popping a node whose page is still on screen. Only the PEEK
        // (does the page underneath wear the tab bar?) happens here.
        //
        // …but a panel the SCREEN has open takes the press first and the page
        // stays: `back()` is `library::back()`'s shape one screen over ("Also
        // available" is part of the detail page, so leaving the page must not
        // be the way to close it).
        //
        // EACH ROUTE ANSWERS FOR ITS OWN PANELS. This was a bare
        // `detail::back()` across both arms, resting on it answering false on
        // Person — which was true only while Person had no panel of its own,
        // and stopped being true when the bio alert landed. A page-owned modal
        // BACK cannot close is a screen the user is stuck on.
        let panel_took_it = match *route {
            Route::Person => crate::ui::person::back(),
            _ => crate::ui::detail::back(),
        };
        if !panel_took_it {
            nav_back(*route, trail, nav);
        }
    } else if matches!(*route, Route::Search) {
        // `back()` answers true while it still had something to close (the
        // raised keyboard); false means leave, and the destination is Home —
        // Search is a peer of it, not a page stacked on it.
        if !crate::ui::search::back() {
            nav_to(*route, Nav::Home { focus_pill: None }, nav);
        }
    } else if matches!(*route, Route::Library) {
        // read BEFORE `back()`: its first press moves focus ONTO the tab row, so
        // asking afterwards would report the pill it just landed on rather than
        // the one the user was standing on when they chose to leave.
        //
        // No `trail.back()` here: the destination is Home, and the commit frame
        // of the page transition truncates the trail to its root — which is both
        // stronger and cancel-safe (a BACK withdrawn inside the 70 ms window
        // must not have moved the history).
        let pill = crate::ui::library::focused_pill();
        if !crate::ui::library::back() {
            nav_to(*route, Nav::Home { focus_pill: pill }, nav);
        }
    } else if g_snap() > 0.5 {
        set_snap(0.0);
    } else {
        // Home is the ROOT and BACK there LEAVES THE APP'S OWN NAVIGATION —
        // deliberately NOT trail-driven. The background-suspend arm drops to
        // Home without touching the trail, so route and trail can legitimately
        // disagree; keeping this branch blind to the trail is what stops that
        // divergence teleporting the user into a page they did not navigate to,
        // and what keeps the true root leaving whatever the trail happens to
        // hold.
        //
        // What the LAST STEP is has changed twice. It quit outright until
        // 2026-08-21, then raised an "Exit PlxNative?" alert, and since
        // 2026-09-03 it hands the screen back to the television
        // (`webos::go_home`) with the process still alive — which is what the
        // platform itself does at an app's entry page on this firmware. The
        // divergence argument above is untouched: this is still the one branch
        // that reaches the root press, and it still does not consult the trail.
        //
        back_at_root();
    }
}

/// **BACK at a ROOT — the press that leaves the app's own navigation**, lifted out of [`key_back`]'s
/// last `else` so a host test can press it.
///
/// It is one call and it is worth its own function for exactly one reason: this is where the app
/// answers "there is nowhere further back to go", and the regression to guard is a future edit
/// putting `running = false` — or a modal question — back where the platform call now goes.
/// `key_back` itself is unreachable from a unit test (its Player arm calls `exit_player`, which
/// pulls the Starfish/ACB seam into the link), so without this split the one branch that matters
/// most could only be graded by reading it.
///
/// **It does not end the process, and nothing about a BACK press does any more.** The remote's own
/// EXIT key still terminates (LG checklist item 38), and a script that wants the app closed uses
/// SAM's `closeByAppId` exactly as `make kill`, `tests/run.py` and `tools/tv-session.sh` already do.
/// That is why the old `/tmp/plxnative-noexitconfirm` bypass went with the alert: it existed to let
/// a headless caller quit by pressing BACK, and BACK is no longer a quit for anybody.
pub(super) fn back_at_root() {
    if crate::webos::take_root_press() {
        crate::webos::go_home();
    }
}

/// Commit the consent screen's focused stop — the answer pill on the press spring-back
/// (`consent::focus_is_ctl`), or a document row on its key-down. One function for both, because the
/// erase-everything outcome underneath is the same whichever way the press arrived.
pub(super) fn commit_consent(route: &mut Route, trail: &mut crate::ui::trail::Trail) {
    crate::ui::consent::on_ok();
    if crate::ui::consent::take_delete_request() {
        let leftovers = delete_all_local_data();
        let outcome = delete_outcome(leftovers.len());
        if outcome.report_leftovers {
            crate::log(&format!(
                "privacy: local data erased; {} file(s) could not be removed: {}",
                leftovers.len(),
                leftovers.join("; ")
            ));
        }
        if outcome.to_sign_in {
            crate::ui::settings::hide(); // the screen under it is going — no fade to run over
            // Tell the read-out what the sweep actually achieved BEFORE it is mounted: a survivor
            // can be the telemetry decision, which comes back on the next launch, so the screen
            // must not claim to have removed it.
            crate::ui::login::note_delete_leftovers(leftovers.len());
            crate::ui::login::enter();
            trail.reset();
            *route = Route::Login;
        }
    }
}

