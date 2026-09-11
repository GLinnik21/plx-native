//! The key ladders of the non-player routes and the pointer state: OK/BACK/direction handling per
//! screen, the card activation, the onboarding and account arms, the press-and-hold path. Moved
//! out of `app.rs` verbatim in phase 1a (a pure move; `pub(crate)` widening only).
//!
//! **Phase 5b (2026-09-07) took the consent and settings arms out of this file.** The Settings
//! family — Settings root, Legal, first-run/Settings consent, the Home-sources editor — is now
//! owned `Screen`s mounted through `app::bridge`, and their keys reach them as `InputEvent`s the
//! loop hands the dispatcher before this ladder is ever consulted; see the deleted-function notes
//! near `key_onboarding` and `enter_profiles_from_onboard` for what used to live here. What this
//! file keeps of consent is only the boot-time GATE deciding *when* to open the screen
//! (`maybe_ask_consent`), never an arm that reads its keys.
//!
//! **Phase 6 (2026-09-07) did the same to the QR sign-in and the who's-watching picker.** Both
//! are owned screens now (`screens::login`/`screens::profiles`), so `key_onboarding` itself —
//! along with the pure `onboarding_back`/`OnboardBack`/`profiles_pin_pad_open` rule it called —
//! is gone; see the note where it stood. What survives is the one piece of its job that is still
//! genuinely the loop's: `login_or_profiles_root_back`, performed from `LoopReq::AuthBackAtRoot`
//! rather than read out of a raw key, and the boot-time phase→route follower in `app/run.rs`
//! (unchanged in shape, only relieved of the `ui::login`/`ui::profiles::enter()` calls an owned
//! screen's own construction now does the work of).

use super::*;


/// The Magic Remote POINTER, as one value: which input mode the remote is in, what the
/// cursor is doing, and the two gestures that outlive a single event (a scrub drag and the
/// wheel's own debounce).
///
/// `dpad_mode`/`cur_hidden`/`mot_accum` are one rule between them and are why this is a
/// type: the first D-pad press hides the cursor and switches modes, and motion only switches
/// back once it has accumulated past the gate (see `remote_synth_ptr`, which has to defeat
/// that gate to click at all).
pub(crate) struct Pointer {
    pub(crate) dpad_mode: bool,  // D-pad input owns focus; pointer motion below the gate is ignored
    pub(crate) cur_hidden: bool, // the LG cursor is hidden right now
    pub(crate) mot_accum: f32,   // motion accumulated since D-pad mode was entered, in logical px
    pub(crate) prev_mx: f32,     // last motion's position, for that accumulation (-1 = none yet)
    pub(crate) prev_my: f32,
    pub(crate) last_motion: u32, // last motion tick — playback hides an idle cursor off this
    /// **A pointer button is DOWN right now** — so this motion is a DRAG (§7.5), and the idle
    /// cursor is not hidden out from under a hand that is holding something.
    ///
    /// It was `drag`, "a click is dragging the HUD scrub band", and it meant the player's scrub
    /// gesture: `app/run.rs`'s own click block set it, its motion arm moved the preview and its
    /// button-up committed the seek. That gesture belongs to `PlayerScreen` (restructure phase 12,
    /// PX-PLAYER), so the flag went with it (`screens::player::input::Scrub::drag`) and what is
    /// left here is the pointer-machine FACT the loop still has to know: the button's state, which
    /// decides whether a motion event is dispatched as `InputKind::Pointer` or `InputKind::Drag`.
    pub(crate) button_down: bool,
    pub(crate) last_wheel: u32,  // last wheel tick, for the wheel's own debounce
}
impl Pointer {
    /// Pointer mode, cursor shown, nothing held or dragging — where the loop starts.
    pub(crate) const IDLE: Pointer = Pointer {
        dpad_mode: false,
        cur_hidden: false,
        mot_accum: 0.0,
        prev_mx: -1.0,
        prev_my: -1.0,
        last_motion: 0,
        button_down: false,
        last_wheel: 0,
    };
}

// `home_activate` (the OK/pointer activation ladder for the legacy Home grid) was retired with
// the legacy `ui::home` module when phase 8 made Home an owned `Screen` — its job (trail reset,
// status/pill/card dispatch through `activate_card`) now lives in the owned screen's own input
// handling (`screens::home`, wired through `app::content`/`app::bridge`). Comments elsewhere in
// this file and in `run.rs`/`nav.rs`/`boot.rs`/`playback.rs`/`metadata.rs` that still name it are
// historical references to the extraction that produced `activate_card`, not live call sites.

/// **A show/season Play (`activate_card`'s non-movie/episode arm), between its ASYNC detail
/// request and the landing that decides play-vs-open.**
///
/// D7: this used to be `MetadataCmd::LoadDetailNow` — a BLOCKING fetch run on the press frame —
/// followed immediately by a read of `crate::metadata::current()`, which only worked BECAUSE the
/// load had already finished by the next statement (`load_detail_now`'s own doc: "every remaining
/// call of this is a deliberate freeze"). `activate_card` now fires `MetadataCmd::RequestDetail`
/// (non-blocking) and arms one of these; [`menu_play_tick`] is the continuation, run every frame
/// from the same site `app/run.rs` already pumps the detail landing from, never on the press frame
/// itself.
#[derive(Clone)]
pub(crate) struct MenuPlayAwait {
    sid: crate::plex::ServerId,
    /// The rk the press is actually waiting for: the SHOW's, for both the show and season arms
    /// (a season's own `rk` names no page of its own — see `activate_card`'s original comment,
    /// preserved on [`menu_play_tick`]).
    expect: String,
    /// `Some(season index)` for a season row — resolved into the loaded show's season list only
    /// once the parent has actually landed as `expect` (a strictly narrower guard than the old
    /// blocking arm's, which read whatever `crate::metadata::current()` happened to hold even
    /// when the fetch had failed and it was a stale, unrelated show).
    season_index: Option<i64>,
    hud_ms: u32,
    /// The frame clock past which the wait gives up and lands on the page anyway — the same
    /// shape and the same 12s ceiling `dev::scenarios::play_arm`'s own `play_await` uses for
    /// "this never got where it was going".
    deadline: u32,
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
///
/// `menu_play_await`/`now` are D7's continuation seam: the show/season arm no longer decides
/// play-vs-open on this call at all (see [`MenuPlayAwait`]/[`menu_play_tick`]).
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn activate_card(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    mm: &crate::pms::PmsMovie,
    want_play: bool,
    hud_ms: u32,
    mut ret: Option<crate::ui::screen::ReturnState<u32, crate::screens::registry::PageMemory>>,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
    menu_play_await: &mut Option<MenuPlayAwait>,
    now: u32,
) {
    let rk = mm.rk.clone();
    if want_play {
        match mm.kind {
            0 | 3 => play_item_now(ps, pa, mm, false, Origin::Here, hud_ms, ret.take(), pages, bridge),
            _ => {
                // show / season: request its page ASYNCHRONOUSLY and fire its Play once the
                // load actually LANDS on the expected item — a failed fetch leaves the
                // PREVIOUS detail in place, so blindly firing on_ok would play whatever page
                // was open before. The decision itself (play vs. land on the page) is
                // [`menu_play_tick`]'s job now, run once the landing (or its settled failure,
                // or the ceiling) says the wait is over — never on this call.
                let expect = if mm.kind == 2 {
                    mm.show_rk.clone()
                } else {
                    rk.clone()
                };
                // a show/season row's parent lives on the SAME server as the row itself
                let sid = mm.sid;
                crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::RequestDetail { sid, rk: expect.clone() });
                *menu_play_await = Some(MenuPlayAwait {
                    sid,
                    expect,
                    season_index: (mm.kind == 2).then_some(mm.season_index as i64),
                    hud_ms,
                    deadline: now.wrapping_add(12_000),
                });
            }
        }
    } else if mm.kind == 2 {
        // season: open the SHOW page with that season selected
        super::bridge::open_detail(pages, bridge, mm.sid, &mm.show_rk, Some(mm.season_index), ret.take());
    } else if mm.kind == 3 {
        // **An episode opens its OWN page**, which is the same page the card menu's "Go to Episode"
        // opens (`Action::GoToItem`) — `detail.rs` serves leaves. It used to open the SHOW's page
        // with the episode's season selected, on the reasoning that the item the tile advertised
        // should be in view; but a season tab is not the episode, and the tile the user pressed
        // named one episode. With a press on a discovery shelf now MEANING "show me this", the
        // most specific page that answers is the episode's.
        //
        // The show is still one press away: it is the card menu's second navigation row, and the
        // episode page's own BACK returns to the shelf.
        super::bridge::open_detail(pages, bridge, mm.sid, &rk, None, ret.take());
    } else {
        super::bridge::open_detail(pages, bridge, mm.sid, &rk, None, ret.take());
    }
}

/// The landing half of `activate_card`'s show/season Play — see [`MenuPlayAwait`]. Called every
/// frame, route-unconditional, from the same site `app/run.rs` already pumps the detail landing
/// from (right beside `crate::stores::metadata::pump_detail()`): a landing must never depend on
/// which screen is mounted, since the press that started the wait may have come from Home while a
/// different page is up by the time it settles.
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn menu_play_tick(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
    menu_play_await: &mut Option<MenuPlayAwait>,
    now: u32,
) {
    let Some(MenuPlayAwait { sid, expect, season_index, hud_ms, deadline }) = menu_play_await.clone() else {
        return;
    };
    // Something else already moved the app onto the player this frame (a different press, an
    // auto-advance) — the wait is moot and must not fire a second Play on top of it.
    if super::bridge::player(pages).is_some() {
        *menu_play_await = None;
        return;
    }
    let landed = crate::metadata::current()
        .map(|d| crate::plex::same_item((d.sid, &d.rk), (sid, &expect)))
        .unwrap_or(false);
    if !landed {
        // Give up once the addressed request has SETTLED without landing this item (a failed or
        // refused fetch keeps the previous item in place — `detail_request_status` answers
        // `Some(false)`), or past the ceiling — the same two ways `dev::scenarios::play_arm`'s own
        // wait ends without a play, both logged rather than silent there for the same reason: a
        // wait that neither played nor said why would read as a hang.
        let settled = crate::metadata::detail_request_status(sid, &expect) == Some(false);
        let expired = now.wrapping_sub(deadline) < u32::MAX / 2;
        if !settled && !expired {
            return; // still waiting — try again next frame
        }
        *menu_play_await = None;
        // nothing playable / load failed — land on the page, through the transition. `season:
        // None`: the container's own reuse rule is what keeps a re-open from stacking a second
        // copy of a page the user is already standing on (`bridge::open_detail`).
        super::bridge::open_detail(pages, bridge, sid, &expect, None, None);
        return;
    }
    *menu_play_await = None;
    // The season resolution reads whatever `current()` holds — which, now that `landed` is
    // known true, is genuinely `expect`'s own detail. The old blocking arm ran this same lookup
    // BEFORE checking `loaded` at all, so on a failed fetch it could resolve a season index
    // against a stale, unrelated show that happened to still be loaded; gating it on `landed`
    // here is strictly narrower, not a new capability.
    if let Some(i) = season_index {
        if let Some(idx) = crate::metadata::current().and_then(|d| d.seasons.iter().position(|s| s.index == i)) {
            crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::LoadSeasonNow(idx));
        }
    }
    if let Some(resume_ns) = request_loaded_hero(ps) {
        start_playback(ps, pa, resume_ns, Origin::Here, hud_ms, None, pages, bridge);
    } else {
        super::bridge::open_detail(pages, bridge, sid, &expect, None, None);
    }
}

/// **D7 — reproduced against the historical (pre-fix) tree, not merely simulated.** Before this
/// package's fix, `activate_card`'s show/season arm called `MetadataCmd::LoadDetailNow` — a
/// BLOCKING fetch on the calling thread — and decided play-vs-open on the very next statement.
/// Run against the pre-fix worktree (base `3100d981`, `activate_card`'s OLD 11-argument
/// signature, no `menu_play_await`/`now`) with a registered-but-refused test server
/// (`127.0.0.1:1`, `detail_panel_tests.rs`'s own established pattern — no real PMS involved):
///
/// ```text
/// d7-repro: detail_loading()=false immediately after the press call
/// test app::input::d7_repro::a_show_play_decides_synchronously_on_the_press_call ... ok
/// ```
///
/// `detail_loading()` was ALREADY `false` right after the call returned — the load had already
/// run to completion, synchronously, on the thread that is supposed to be servicing input and
/// drawing frames. Because the fix changes `activate_card`'s own signature (it gains
/// `menu_play_await`/`now`), that exact test cannot compile against the fixed code, so this
/// module asserts the SAME observable — but the opposite way, which is what the fix buys: the
/// call must return with the load still IN FLIGHT and the decision deferred to
/// [`menu_play_tick`], never resolved on the press frame.
///
/// **Integration fix (phase 12 wave-0):** gated on `hostsim` — `menu_play_tick`'s landed arm
/// calls `start_playback`, which reaches `player::adapter`/`player::engine` and, through them,
/// the real `player::ffi::sf_load`/`vp_mode`/`vp_create_window` `extern "C"` declarations. Those
/// resolve only against the television's own `libplayerAPIs`/ACB seam (via the Makefile's C link)
/// or against `player/ffi_host.rs`'s `hostsim` stand-ins; a bare `cargo test --lib` on macOS has
/// neither, so an ungated module here made every `player::ffi` wrapper a hard undefined symbol in
/// the DEFAULT feature pass the moment this test made that call chain reachable — a bare `make
/// check` failed to link with `_sf_load`/`_vp_create_window`/`_vp_mode` undefined. This is the
/// same reason `player::engine`'s own `native_lifecycle_host_seam_tests` (right beside its
/// `MainThread`-driven `sf_*` calls) carries the identical gate: see AGENTS.md's testing section
/// on `player/ffi_host.rs` existing "ONLY in the hostsim configuration".
#[cfg(all(test, feature = "hostsim"))]
mod activate_card_tests {
    use super::*;

    /// A show/season Play must not decide play-vs-open on the press frame: it fires an ASYNC
    /// `RequestDetail` and arms [`MenuPlayAwait`], leaving `metadata::detail_loading()` `true`
    /// until [`menu_play_tick`] is driven by a subsequent frame's landing (or its settled
    /// failure, or the ceiling) — see this function's own doc for the pre-fix run that motivated
    /// it.
    #[test]
    fn a_show_or_season_play_no_longer_decides_on_the_press_frame() {
        let _guard = crate::testlock::serial();
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
                crate::plex::reset_servers_for_test();
            }
        }
        let _cleanup = Cleanup;
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test("press-frame", "127.0.0.1", 1, "t", "c-press-frame");
        let mm = crate::pms::PmsMovie { sid, rk: "show-1".into(), kind: 1, ..Default::default() };

        let mut ps = crate::route::PlaybackSession::default();
        let mt = unsafe { crate::task::MainThread::assume() };
        let mut pa = crate::player::adapter::PlayerAdapter::new(mt);
        let mut pages = crate::ui::dispatch::Dispatcher::<super::bridge::AppHost>::new();
        let mut bridge = super::bridge::Bridge::for_test(|| 0);
        let mut menu_play_await = None;

        unsafe {
            activate_card(&mut ps, &mut pa, &mm, true, 1000, None,
                &mut pages, &mut bridge, &mut menu_play_await, 0);
        }

        assert!(
            crate::metadata::detail_loading(),
            "the parent detail must still be IN FLIGHT right after the press — the play/open \
             decision must wait for menu_play_tick, not run on this call"
        );
        assert!(menu_play_await.is_some(), "the continuation state must be armed");
        // the CONTAINER must not have moved yet either — no premature page, no play. Nothing has
        // ever been asked of this tree, so any pending op at all is one this press made.
        assert!(!pages.has_pending_navigation(), "no premature navigation");

        // Drive the continuation before anything can land (no worker has had a chance to run on
        // a fresh test process): it must still be waiting, not give up early.
        unsafe {
            menu_play_tick(&mut ps, &mut pa, &mut pages, &mut bridge, &mut menu_play_await, 0);
        }
        assert!(menu_play_await.is_some(), "with nothing settled and the ceiling not reached, the wait continues");
        assert!(!pages.has_pending_navigation(), "…and still nothing has been navigated to");

        // Past the 12s ceiling (`dev::scenarios::play_arm`'s own), the wait must give up and land
        // on the page rather than hang forever.
        unsafe {
            menu_play_tick(&mut ps, &mut pa, &mut pages, &mut bridge, &mut menu_play_await, 12_001);
        }
        assert!(menu_play_await.is_none(), "the ceiling must end the wait");
        assert!(pages.has_pending_navigation(),
            "…and land on the page rather than leaving the press with no effect at all");
    }
}

/// Perform an item-menu [`Action`](crate::screens::item_menu::Action) — the ONE dispatch, drained
/// from `AppFx::ItemMenu` after every dispatcher frame (`content::content_requests`). The menu
/// itself only reports the choice; every route flip, server call and refresh is here.
///
/// It was reached from TWO call sites, the OK key's ladder arm and the pointer click's — the exact
/// shape `home_activate` and `activate_ctrl_row` were unified out of, and it had the same latent
/// drift. The surface owns both edges now (`Activate::Immediate` over the container's stop
/// registry), so there is one producer and one drain.
///
/// **`req` carries what `MenuHost` used to say, and it is two bits rather than six variants.**
/// `loaded_episode` is the only one that changes what an action MEANS: only the detail page's
/// episode filmstrip holds an item that is a leaf of the loaded season, so its Play from Start goes
/// through that page's own episode path and its scrobble makes the page re-read itself. Every other
/// entry point — Home, the Library grid, a Search shelf, a person's filmography, and the detail
/// page's own RELATED shelf, which stands on that page while its tiles are OTHER items — is a card
/// row, and they are all the same arm: the row rides on the request instead of being looked up in
/// the hub catalog, which only Home's cards are ever in.
///
/// **`from_home` no longer decides anything here.** It selected `app::nav::menu_leave`, whose whole
/// body was `if from_home { trail.reset() }` — spending the history behind Home because the menu on
/// the root is the user acting on the root. Home IS the container's root, and a menu opened on it
/// is opened with nothing above that root, so the reset was a no-op in the only case that could
/// reach it; expressing it as a `Root(Home)` op would have been actively wrong, since the `Push`
/// that follows in the same frame SUPERSEDES the newest request. The bit stays on the argument
/// (and in the recorded state) as the fact it is.
pub(crate) unsafe fn apply_item_action(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    req: crate::screens::registry::ItemMenuReq,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
) {
    use crate::screens::item_menu::Action;
    let crate::screens::registry::ItemMenuReq { act, sid, item, loaded_episode, from_home } = req;
    // `sid` is WHICH SERVER this menu's rows are about, captured when the panel was presented.
    // Every arm below turns an rk into a fetch, a scrobble or a play, and resolving one against
    // `plex::current_server()` is the reported bug itself: on a merged Continue Watching shelf,
    // Play from Start on a friend's episode found OUR row with the same key and played a different
    // film under the friend's title.
    //
    // The empty-rk guard the head of this function used to carry is the SURFACE's now
    // (`ItemMenuScreen::activate`): a row with no target never becomes a request at all.
    match act {
        Action::None => {}
        Action::GoToItem(rk) => {
            let _ = from_home; // see the note below `apply_item_action`
            super::bridge::open_detail(pages, bridge, sid, &rk, None, None);
        }
        Action::GoToShow(show_rk, season) => {
            super::bridge::open_detail(pages, bridge, sid, &show_rk, (season > 0).then_some(season), None);
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
            let detail = loaded_episode.then(|| rk.clone());
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
            if loaded_episode {
                if request_loaded_episode(ps, &rk) {
                    start_playback(ps, pa, 0, Origin::Here, HUD_LINGER_MS, None, pages, bridge);
                }
                return;
            }
            // **The row the menu was opened ON, not a re-resolve by key.** This used to walk the
            // HOME hub catalog (`pms::index_of_rk`), which is a lookup that only ever answers for a
            // card that is on a Home shelf — so on the Library grid, a Search result or a person's
            // filmography the arm found nothing and the press did nothing at all, silently. The
            // panel is about ONE item and captured it when it was presented; `ItemMenuArg`'s row is
            // that capture, which is both the fix and the smaller claim (it also cannot be
            // re-pointed by a hub refetch rebuilding the catalog under an open panel — the reason
            // the old lookup deferred in the first place).
            //
            // The `rk` guard is what keeps the two in step: every other arm acts on the action's
            // own key, so playing a row that does not carry it would be this dispatch disagreeing
            // with itself.
            if let Some(mm) = item.as_ref().filter(|m| m.rk == rk) {
                play_item_now(ps, pa, mm, true, Origin::Here, HUD_LINGER_MS, None, pages, bridge);
            }
        }
    }
}

// ---- the key ladder: one function per arm ------------------------------------------------------
//
// The run loop's key handler is a LADDER: a key-up, a hardware auto-repeat and the preamble every
// fresh press runs; then the DISPATCHER's arm and the route-scoped ones after it, each
// `continue`ing; then one chained `else if` on key identity. Each arm's BODY is a function here,
// in the order the ladder tries them — bar the ones with no body to name (the dispatcher's arm is
// one `app.inputs.push`, the pointer-hidden arm is empty). No count is given, deliberately: three
// arms left this ladder in phase 5b alone and every remaining player arm left it in phase 12, and
// a number here rots without anything failing.
//
// Every guard, every `continue` and the order itself stay at the CALL SITE, because the order is
// part of the behaviour: an earlier guard subsumes later ones it overlaps with, and that is only
// legible while the tests sit in one list, in order, in one place. (The clearest example used to
// be `key_player_failed`, whose guard deliberately subsumed every player arm below it; the whole
// group is `PlayerScreen`'s since phase 12, where the same precedence is the FIRST test in
// `handle_key` rather than the height of an arm in a chain.)
//
// No host test executes any of this: it runs inside the SDL event loop. The gate over it is
// `tools/keytable.py`, which drives the simulator through (screen x key) and diffs the focus
// fingerprint each press produces against a recorded table.

/// A key-up: the reliable release (this remote sends exactly one per press). Retires this sym from
/// the physically-down slot and springs a deferred grid-card press back.
///
/// It cleared a second slot — `HeldKey::sym`, the client-side hold-repeat's own — until phase 10
/// deleted that timer with its last consumer (the item menu). What is left is `down_sym`, which is
/// a fact about the PHYSICAL key rather than about whoever read it, and the press machine's
/// release, which is a fact about the LOOP's own deferred press. **Both are why this still runs
/// unconditionally, above the tree-ownership question**: a key the ladders never saw go down would
/// otherwise look held for as long as a surface was up.
///
/// **The scrub half is gone** (restructure phase 12, PX-PLAYER). It ran on
/// `PlayerScreen::scrub` — a field of a screen that has owned the whole gesture since it began
/// answering `HitSource::Engine`, and which arms its own `TAP_COMMIT_MS` debounce on
/// `Edge::Up` — so the two together issued TWO seeks for one tap. The commit, the debounce and
/// the reveal-cancel are `screens::player::PlayerScreen::key_scrub_release`'s now, and
/// `repause_at` reaches `commit_seek` through `PlayerReq::CommitSeek` instead of through here.
pub(crate) fn on_key_up(
    sym: c_uint,
    ok_armed: bool,
    down_sym: &mut u32,
    press: &mut crate::ui::press::Press,
) {
    if sym == *down_sym {
        *down_sym = 0;
    }
    if is_ok(sym) && ok_armed {
        // OK released over a grid card: start the spring-back; the deferred
        // activation commits from the per-frame loop once the bounce has shown.
        press.release(clock::now());
    }
}

/// A hardware AUTO-REPEAT (held key). **All that is left of it is the deferred press's liveness
/// beat** — the dropped-key-up net in `ui::press`, which is the loop's own machine and belongs to
/// no screen.
///
/// The client-side hold-repeat timer went in phase 10: every discrete focus list in the app — the
/// home grid, detail, the Settings family, the player's four panels and, last of them, the item
/// context menu — is an owned screen or a surface on the dispatcher, so a repeat reaching one is
/// an `InputEvent` carrying `Edge::Repeat` handed over before this function is reached, and the
/// cadence is applied THERE (`RepeatGate`, to the DIRECTIONS only).
///
/// The player's continuous scrub was the last thing this drove directly, and it went the same way
/// in phase 12 (PX-PLAYER): `PlayerScreen::key_scrub_repeat` engages the ramp off its own
/// `Edge::Repeat`, which is the same event one layer down, and the per-frame advance is that
/// screen's `Tick`. Nothing about the gesture is read or written from here any more, which is what
/// makes `PlayerScreen::scrub` a field with ONE owner.
pub(crate) fn on_auto_repeat(sym: c_uint, ok_armed: bool, press: &mut crate::ui::press::Press) {
    if ok_armed && is_ok(sym) {
        press.note_alive(clock::now()); // OK held: keep the dropped-key-up net honest
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
/// Three things stay UNCONDITIONAL and each for its own reason. `down_sym` is bookkeeping
/// about the physical key, not a side effect: without it a held unsupported key's auto-repeats
/// would each arrive as a fresh press (`state & 0x100 != 0 && sym == app.down_sym` in the
/// caller). The D-pad cursor gate is already narrower than `is_bound` — it takes the four plain
/// direction syms and nothing else — so it needs no second guard. And the caller's `last_input`
/// stamp is a local read only by arms that run in the same iteration, so an unbound press cannot
/// carry it anywhere.
pub(crate) unsafe fn begin_fresh_press(
    ps: &crate::route::PlaybackSession,
    key: Key,
    sym: c_uint,
    wcode: c_uint,
    now: u32,
    down_sym: &mut u32,
    hud: Option<&mut HudState>,
    ptr: &mut Pointer,
    ok_armed: &mut bool,
    press: &mut crate::ui::press::Press,
) {
    *down_sym = sym;
    note_global_press(ps, sym, wcode, now, hud, ok_armed, press);
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
pub(crate) fn note_global_press(
    ps: &crate::route::PlaybackSession,
    sym: c_uint,
    wcode: c_uint,
    now: u32,
    hud: Option<&mut HudState>,
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
    if let Some(hud) = hud {
        hud.note_fresh_press(ps, now, paused());
    }
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
    fn press(ps: &crate::route::PlaybackSession, sym: c_uint, wcode: c_uint) -> (bool, bool) {
        let mut hud = HudState::IDLE;
        let mut ok_armed = true; // a click is in flight, as if OK were still down on a card
        hud.dismissed = true; // …and the transport was hidden by hand (UP from the control row)
        let mut p = crate::ui::press::Press::new();
        p.begin(1_000);
        note_global_press(ps, sym, wcode, 1_000, Some(&mut hud), &mut ok_armed, &mut p);
        let out = (p.is_active() && ok_armed, hud.dismissed);
        p.cancel();
        out
    }

    /// A key the app binds behaves exactly as it always has: it un-dismisses the HUD and aborts the
    /// click it slid off. BACK is the case to use — it is not OK, so it takes the abort branch.
    #[test]
    fn a_bound_key_still_wakes_the_hud_and_aborts_the_click() {
        let ps = crate::route::PlaybackSession::IDLE;
        let _g = crate::testlock::serial();
        let (armed, dismissed) = press(&ps, SDLK_ESCAPE, 0);
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
        let ps = crate::route::PlaybackSession::IDLE;
        let _g = crate::testlock::serial();
        for (sym, wcode, what) in [
            (0, 269, "HOME"),
            (0, 270, "AC_BACK"),
            (b'a' as c_uint, 4, "a letter"),
        ] {
            let (armed, dismissed) = press(&ps, sym, wcode);
            assert!(armed, "{what} must not cancel the armed click");
            assert!(dismissed, "{what} must not un-dismiss the HUD");
        }
    }

    /// The digits are the one place [`is_bound`] deliberately over-approximates (its doc argues
    /// it): the who's-watching PIN keypad types from them, so they count as bound everywhere.
    /// Pinned so the trade-off stays a decision on record rather than something a reader finds.
    #[test]
    fn a_number_key_counts_as_bound_because_the_pin_keypad_types_from_it() {
        let ps = crate::route::PlaybackSession::IDLE;
        let _g = crate::testlock::serial();
        let (armed, dismissed) = press(&ps, b'5' as c_uint, 34);
        assert!(!armed);
        assert!(!dismissed);
    }
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
pub(crate) enum AfterCancel {
    /// There was somewhere to go inside the app; the main loop's phase→route follower takes it
    /// from here. Nothing to ask the platform for.
    BackedOut,
    /// Nowhere to go inside the app, and nothing was disturbed. Straight to the television's Home.
    Home,
}

/// The whole of the rule, pure so the pairing with the log line is gradeable: a `cancel` that
/// backed out is not a root press after all; one that refused leaves everything as it was and hands
/// the screen to the television.
pub(crate) fn after_cancel(backed_out: bool) -> AfterCancel {
    if backed_out {
        AfterCancel::BackedOut
    } else {
        AfterCancel::Home
    }
}

// `key_onboarding`, `OnboardBack`, `onboarding_back` and `profiles_pin_pad_open` stood here.
// **Phase 6 retired all four.** Login and Who's Watching are OWNED screens now
// (`screens::login`/`screens::profiles`, mounted through `app::bridge`), so their keys are
// `InputEvent`s the loop hands the dispatcher before this ladder is ever consulted — the same
// coexistence rule phase 5b applied to the Settings family and to first-run Favourites, above.
// The one piece of `key_onboarding`'s body that survives is its BACK-at-root arm
// (`login_or_profiles_root_back`, immediately below): issues #16-#18's rule, unmoved, but reached
// from a different door. Under the old ladder, `onboarding_back`'s SECOND job — telling a real
// root press apart from the picker's own PIN-keypad BACK — needed `profiles_pin_pad_open`, which
// read the pad's open flag off `ui::profiles`' legacy statics; that state now lives on
// `screens::profiles::ProfilesScreen`'s own focus engine, which this module cannot see (and must
// not: the layer rule is `app/` never names a sibling `screens/` module's internals). So the
// screen decides that half itself — closing its own pad and answering `Handled::Yes` with no
// request, exactly as `screens::consent`'s Settings-mode BACK declines rather than asking the loop
// — and pushes `LoopReq::AuthBackAtRoot` only once it has concluded the press really is its root.
// `after_cancel`/`AfterCancel` above are untouched: the SAME decision, just reached from a request
// drain instead of a raw key.

/// **Phase 6's `LoopReq::AuthBackAtRoot`, performed.** The screen has already decided this BACK is
/// its ROOT press — nothing of this app is behind it — so this is the whole of what `key_onboarding`
/// used to do in its `OnboardBack::Root` arm, unchanged: rate-limit the platform call
/// (`webos::take_root_press`, taken HERE and not inside `webos::go_home`, because a `cancel` that
/// CAN back out is destructive — it retires the switch worker and resumes the stored session — so
/// limiting only the platform call would still let a burst of root presses back out once per
/// press), ask `auth::cancel()` whether there is a stored session to fall back to, and either
/// resume it (nothing left to ask the platform for — the claim is handed straight back so the
/// REAL root BACK the user is about to press on the Home they just returned to is not swallowed)
/// or hand the screen to the television's Home.
///
/// `route` is read only for the one word the log line prints; the decision itself does not depend
/// on which of the two screens asked; that is `LoopReq::AuthBackAtRoot`'s whole point.
pub(crate) fn login_or_profiles_root_back(route: &AppArg) {
    if crate::webos::take_root_press() {
        let backed_out = crate::auth::cancel();
        let phase = crate::auth::phase();
        let plan = after_cancel(backed_out);
        // ONE line saying what this press decided and on what evidence, for the person reading
        // the device log who is not the person who wrote this. The phase is evidence, not an
        // input: it says what the refused press left running. Every field is an enum name: no
        // identity, no content.
        crate::log(&format!(
            "back: root route={} phase={phase:?} backed_out={backed_out} action={plan:?}",
            if matches!(route, AppArg::Login) {
                "login"
            } else {
                "profiles"
            }
        ));
        match plan {
            AfterCancel::BackedOut => crate::webos::release_root_press(),
            AfterCancel::Home => crate::webos::go_home(),
        }
    }
}

// `settings_root_owns_input` stood here, with `settings_child_input_tests` beside it: "Settings
// remains open behind its Home editor, but it must not own input while that child route is
// visible." Both are gone with the two-route choreography they arbitrated — the Home editor is a
// PAGE of the surface's own stack now, so the surface's top page IS the input owner and the
// question is `Dispatcher::owns_input` (spec §11: this test module dissolves into `input_owner()`).
// `commit_onboarding` went with them: the first-run screen arms and commits its own press.

/// BACK from Shared Sources returns to the identity step and records no source answer. Starting a
/// ChangeProfile flow re-seeds the roster from the persisted session immediately, so this is a
/// real usable picker rather than a static screen with no worker behind it.
///
/// **No `ui::profiles::enter()`** (phase 6, mirroring first-run Favourites' own removal in 5b):
/// the picker is an OWNED screen now, and naming the route is the whole of mounting a fresh one —
/// `AppMounter::mount` constructs a new `ProfilesScreen` the moment the tree follows this route.
pub(crate) fn enter_profiles_from_onboard(pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>) {
    crate::auth::start_switch(crate::auth::Picker::ChangeProfile);
    super::bridge::nav_root(pages, AppArg::Profiles);
}

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
/// automated boot, so every call site can simply ask. Nothing is stored by asking — and that is a
/// property of the SURFACE, not of this function: presenting `AppArg::FirstRunConsent` mounts a
/// `ConsentPage` holding a draft, and only its two answer pills reach `ConsentCmd::Record`.
///
/// Phase 5b: the screen is the tree's, so this presents rather than opens. `bridge::open_*` is
/// itself idempotent while the surface is up (any phase), which is what lets the three per-frame
/// routing call sites go on simply asking.
pub(crate) fn maybe_ask_consent(pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>) {
    let c = crate::telemetry::consent::current().unwrap_or_default();
    // dev: /tmp/plxnative-consent[=<crash|product>] forces either first-run purpose even on an
    // automated boot. This screen is suppressed BY the presence of any trigger, so without an
    // override it cannot be reached headlessly at all. Selecting Product changes display state
    // only; no answer is stored by a harness boot — the stage byte is where the surface STARTS,
    // and reaching stage 1 that way skips the crash question rather than answering it.
    if let Some(target) = crate::dev::scenarios::consent_override() {
        // `screens::consent`'s `STAGE_PRODUCT`, spelled here because it is that module's private
        // encoding of `SettingsPage::ConsentStage` and this is its only outside caller. The
        // companion bit (`ERRORS_SHARED`, 0x10) is deliberately NOT set: a dev boot has answered
        // nothing, so the product stage opens with the crash answer at its default.
        let stage = u8::from(target.trim() == "product");
        super::bridge::open_first_run_consent_at(pages, stage);
        return;
    }
    if crate::screens::consent::should_show(&c, crate::dev::any_trigger_present()) {
        super::bridge::open_first_run_consent(pages);
    }
}

/// Leave the first-run question for Home — `LoopReq::OnboardDone`.
///
/// The trail is RESET rather than pushed to: this route is the last of the onboarding gates and
/// Home is the root behind it, so a BACK from Home must reach the ROOT PRESS exactly as it does on
/// any other boot — not walk back into a question that has already been answered. (That press is
/// [`back_at_root`], the television's own Home; what this reset guarantees is that Home is still the
/// root when it lands, which is what puts the user one BACK from it either way.)
///
/// **It has only the first-run half now** (phase 5b). The Settings-hosted editor used to come
/// through here too, and the branch that served it was the whole reason `SETTINGS_HOME_RETURN`
/// existed: the editor was a `Route` drawn from outside the Settings modal, so leaving it had to
/// restore the page the modal was standing on. It is a PAGE of the surface's own stack now — its
/// Done/Cancel is one `NavOp::Pop` inside the surface and the host route never moved — so there is
/// no parked route to restore and no second exit to tell apart from this one. The screen's two
/// exits are therefore two different `LoopReq`s rather than one `Action` with four variants.
pub(crate) fn enter_home_from_onboard(pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>) {
    // The consent pair is NOT asked here any more: it is the sign-in's decision, shared by every
    // profile on the account, and is put before the profile picker, which is upstream of this whole step. See `maybe_ask_consent`.
    // The selection just recorded is an input to Home's merge (`pms::feeds_home`), and the merge
    // re-runs off `browse`'s section generation — which `apply_pins` (the editor's one commit
    // write) has already bumped. Nothing to kick here; Home builds from the answer on its first
    // frame.
    //
    // `Root(Home)` IS the reset: it unwinds every entry above the root and, the root being the
    // onboarding gate rather than Home, covers it with the Home this lands on.
    super::bridge::nav_root(pages, AppArg::Home);
}

/// What a confirmed **Delete all local data** does next, given how many files could not be
/// unlinked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct DeleteOutcome {
    /// Leave for the sign-in screen.
    pub(crate) to_sign_in: bool,
    /// Write the leftovers to the event log.
    pub(crate) report_leftovers: bool,
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
pub(crate) fn delete_outcome(leftovers: usize) -> DeleteOutcome {
    DeleteOutcome {
        to_sign_in: true,
        report_leftovers: leftovers > 0,
    }
}

/// The one destructive Settings operation. Individual UI rows never remove their own files.
///
/// Returns the paths it could NOT unlink — a report, never a verdict. The irreversible half
/// (`auth::erase_local_state`) runs whatever the file sweep managed; see [`delete_outcome`].
pub(crate) fn delete_all_local_data() -> Vec<String> {
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
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
    // No explicit `ClearRecents` here (phase 7 Search cutover retired the legacy screen's own
    // thin `recents::clear()` wrapper this used to call): recent Search terms
    // live INSIDE the session file (`crate::search::recents`'s doc — "profile-scoped … the
    // session's atomic worker door"), and `erase_local_state` below deletes that file
    // SYNCHRONOUSLY. An explicit clear here would spawn its own async save
    // (`recents::clear`'s `task::spawn_small("recents-save", …)`) racing the synchronous
    // deletion two lines down — the worse of the two orders resurrects a stub session file
    // AFTER "delete everything" already removed it. Letting the file deletion alone answer
    // for recents removes that race rather than leaving it to chance ordering.
    //
    // The telemetry decision, both identifiers, the spool and the native backend go with the
    // account: `erase_local_state` → `forget_account` → `telemetry::forget`, the same door
    // Sign out uses. The sweep above already unlinked the files; `forget` finds them gone.
    crate::auth::erase_local_state();
    failures
}

// (`key_item_menu` stood here — the item menu's own arm of the loop's key ladder: OK committed
// and flipped `app.route` back to the host, BACK closed, UP/DOWN moved the cursor and armed the
// client-side hold-repeat timer. The menu is a `ModalStack` surface since phase 10, so every one
// of those is the container's: the dispatcher hands it the key, `ItemMenuScreen::step` answers
// BACK with `NavOp::Dismiss` and paces the repeats itself (`RepeatGate`/`PANEL_REPEAT_MS`), the
// focus engine walks the rows, and `Activate::Immediate` commits. It was the LAST caller of
// `HeldKey::arm`, which is why `App::held_key` went with it.)

// `key_move_focus` and `top_focus` (the D-pad-direction and shared-top-bar-focus dispatch for
// Home/Library/Search) are retired: since the phase 8 Search cutover, every non-player route
// reaching this file is an OWNED page whose directions and top-bar focus are taken by the
// dispatcher/container tree before this ladder is ever consulted (see the retirement notes at
// `run.rs`'s nav-direction arm and its former `top_focus` call site). Both were already
// unconditional no-ops for those routes by the time main's copy above was written.
/// The profile chip's activation, shared by the OK key and the pointer click — the top bar is one
/// control on three screens and this is the one thing it does.
///
/// It deliberately does NOT go through `home_activate`'s `trail.reset()` the way Home's chip press
/// used to: the account menu is a POPOVER over whatever page is showing, not a navigation, and on
/// the Library or Search a reset would throw away history the user is still standing on. (At Home
/// the reset was a no-op anyway — arriving at Home is itself the trail's reset, so the stack there
/// is already just the root.)
///
/// **The popover is a SURFACE since phase 10, which is what finally makes that sentence
/// structural.** `Route::Account` was a unit variant meaning "Home, plus the panel", so a press on
/// the Library's chip swapped the page underneath to Home on the press frame and dropped the user
/// there when they dismissed it; `Route::Account { over: BarHost }` fixed that by NAMING the host,
/// and a `ModalStack` surface needs no name at all — the page it was presented over stays the top
/// page and is never replaced. A route with no chip on it opens nothing.
pub(crate) fn chip_activate(
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
) {
    use crate::ui::screen::ScreenArg;
    if pages.top_arg().map(|a| a.chrome()) != Some(crate::ui::machine::Chrome::TabBar) {
        return;
    }
    // Search USED to need an explicit keyboard-dismissal nudge here (`BarHost::Search =>` the
    // retired legacy screen's own `end_editing()`) for the one path that could still reach this
    // function with the television's own keyboard up — a pointer click on the chip from inside
    // the field.
    // That path is retired (phase 7 Search cutover): `owns_input()` in `app/run.rs` now takes
    // every Search click before it ever reaches `chip_clicked`/`chip_activate`, and the owned
    // path that replaces it (`content.rs`'s `SearchReq::Account`) does not call this function at
    // all — the owned screen releases its own keyboard before emitting the request, and calling
    // `end_editing` here a second time on an instance that already dismissed it would be the
    // stale-request problem `an_old_search_keyboard_request_cannot_close_the_new_instances_keyboard`
    // guards against.
    super::bridge::open_account_menu(pages);
}

/// The bar-wearing pages, i.e. the ones with a profile chip on them at all — `BarHost::of`'s
/// successor, and the whole of what that type was still doing once the menu became a surface.
/// DERIVED from [`crate::ui::screen::ScreenArg::chrome`] rather than listing Home/Library/Search a second time: the
/// chip is a control ON the shared bar, so "is there a chip to press" is "does this page wear the
/// bar", and the two cannot drift.
pub(crate) fn wears_the_chip(route: &AppArg) -> bool {
    use crate::ui::screen::ScreenArg;
    route.chrome() == crate::ui::machine::Chrome::TabBar
}

/// Did this click land on the profile chip of a screen that is WEARING the shared bar? The pointer
/// twin of [`chip_activate`]'s key path, and the route test is the whole of what makes it safe:
/// `widgets::CHIP_FRAME` is a constant (the chip never moves), so nothing else bounds it to the
/// screens that actually draw one.
pub(crate) fn chip_clicked(route: &AppArg, ev: &[u8]) -> bool {
    if !wears_the_chip(route) {
        return false;
    }
    let (mx, my) = ptr_xy(ev);
    crate::ui::widgets::profile_chip_at(mx, my)
}

#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn key_ok(
    ps: &crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    now: u32,
    _ptr: &mut Pointer,
    ok_armed: &mut bool,
    press: &mut crate::ui::press::Press,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
) {
    // The shared top bar's PROFILE CHIP used to be answered here, ahead of the per-route ladder
    // below, off `top_focus` — retired with that function (Home/Library/Search are all owned
    // screens now, so an OK on the chip is taken by `tree_owns_key` in `app/run.rs`'s ingest,
    // well above this chain, and never reaches here).
    if super::bridge::player(pages).is_some() {
        // The cursor and the pre-press visibility are the mounted screen's, read ONCE into copies
        // so the arms below are free to present a panel on the same container (`open_player_overlay`
        // takes it mutably). The pre-press sample is the one the other player arms take too:
        // `begin_fresh_press` has already cleared `dismissed`, so re-asking calls a hand-hidden
        // transport visible and this arm would open a panel from behind it
        // (`HudState::visible_at_press`).
        let Some((vis, focus, tab)) = super::bridge::player(pages)
            .map(|player| (player.hud.visible_at_press, player.hud.nav.focus, player.hud.nav.tab))
        else {
            return;
        };
        // Row 1 is the transport's CONTROL ROW — the Subtitles / Audio / ⋯ discs, or whichever
        // stand-in has taken their place (Skip, Up Next). Every occupant is a control FACE with a
        // pop of its own (`player_hud::ROW_POP`), so OK takes the tvOS press: dip now, act on the
        // spring-back, in `activate_player_row` from the per-frame loop. Both of its arms open
        // something OVER this HUD rather than leaving the route, which makes this the one control
        // row in the app where the whole dip → ring is on screen either side of the activation.
        if vis && focus == 1 {
            press.begin_ctl(now);
            *ok_armed = true;
        } else if vis && focus == 2 {
            if tab == 0 {
                super::bridge::open_player_overlay(ps, pages, crate::screens::player::overlay::OverlayKind::Info);
            } else if tab == 1 {
                super::bridge::open_player_overlay(ps, pages, crate::screens::player::overlay::OverlayKind::Chapters);
            }
        } else {
            let np = !paused();
            if np {
                if set_transport_paused(pa, true) {
                    crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
                        feature: crate::diag::schema::Feature::Pause,
                    });
                }
            } else {
                set_transport_paused(pa, false);
            }
        }
        if let Some(player) = super::bridge::player_mut(pages) {
            player.hud.extend(now, HUD_LINGER_MS);
            player.publish();
        }
    }
    // `Route::Search` is deliberately absent here (phase 7 Search cutover, mirroring
    // `Route::Library`'s own removal): Search is unconditionally an owned screen, so its OK key —
    // the field, the recents rows and a result tile's tvOS press alike — was already taken by
    // `tree_owns_key` in `app/run.rs`'s ingest, well above this chain, and this function is never
    // reached for it.
}


/// webOS BACK: this Magic Remote sends wcode 482 (0x1E2); 461 kept for others.
///
/// Back stack: player -> the PAGE STACK (detail/person, at any depth) -> library -> grid -> hero ->
/// exit. Inside the Library, BACK first walks menu -> tab bar (library::back), THEN leaves to Home.
/// The ORDER is unchanged; what changed is that detail/person pop the CONTAINER's own stack
/// (`NavOp::Pop`, through `bridge::nav_pop`) instead of consulting two booleans that had one slot
/// per screen KIND and so could not describe a detail page standing on another one. It was a
/// second stack of the app's own (`ui::trail`) between those booleans and this; D1 deleted it,
/// because two histories that must agree are one bug waiting to be found.
///
/// A BACK inside the page fade's 70 ms window WITHDRAWS the transition rather than acting on a
/// screen that is already half gone: the request is at most four frames old and nothing has changed
/// yet, so it can still be un-asked. `nav_cancel` refuses once the swap has happened, and then this
/// is an ordinary BACK on the NEW screen — the press is never dropped, only ever spent on exactly
/// one of the two. (It matters most at Home's root, where "what BACK would otherwise do" is hand
/// the screen back to the television — see [`back_at_root`].)
pub(crate) fn key_back(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    refresh_hubs_at: &mut u32,
    pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
) {
    if super::bridge::nav_cancel(pages) {
    } else if super::bridge::player(pages).is_some() {
        exit_player(ps, pa, refresh_hubs_at, pages);
    } else if matches!(pages.top_arg(), Some(AppArg::Content(_))) {
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
        // Content screens own BACK, including their page-local panels.
    }
    // `Route::Search` is deliberately absent here too (phase 7 Search cutover): BACK on Search —
    // both closing the raised keyboard first and, once there is nothing left to close, leaving to
    // Home — is fully handled inside the owned screen (`SearchReq::Back`, drained by
    // `content::search_requests`) and, upstream of that, `tree_owns_key` in `app/run.rs`'s ingest
    // already took the key before it could reach this function at all.
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
pub(crate) fn back_at_root() {
    if crate::webos::take_root_press() {
        crate::webos::go_home();
    }
}

/// **Delete all local data, confirmed** — `LoopReq::DeleteAllLocalData`, the one Settings
/// operation that outlives the screen that asked for it.
///
/// It was `commit_consent`'s tail: the legacy consent screen latched a delete REQUEST and the key
/// ladder collected it on the next press, because the press and the sweep had no other way to meet.
/// The owned screen's decision alert emits the request as an effect instead, so this is now
/// reached from exactly one place — the loop's request drain — and does only the part that is
/// genuinely the loop's: the file sweep, the report, and the route to sign-in.
///
/// The SURFACE is dismissed by the caller, not here: the screen under it is going, and there is no
/// host left for a fade to run over (what `settings::hide()` used to say).
pub(crate) fn delete_all_local_data_and_sign_out(pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>) {
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
        // Tell the read-out what the sweep actually achieved BEFORE it is mounted: a survivor
        // can be the telemetry decision, which comes back on the next launch, so the screen
        // must not claim to have removed it.
        //
        // **The leftovers count lives on `auth` now, not on the legacy `ui::login::Scene`
        // (phase-6 retirement).** The owned `screens::login::LoginScreen::resync` reads this
        // sweep's count through `auth::delete_leftovers()`, and this is the one place that
        // count is produced — a plain module-level cell rather than a screen's own state,
        // because the value has to survive from HERE (before the fresh Login route is even
        // mounted) to the first frame the new screen draws, and no screen instance exists yet
        // to hold it. Writing it into the now-deleted `ui::login::Scene` instead would silently
        // stop working the moment nothing calls `ui::login::init()` any more to allocate that
        // static — which is exactly what retiring the legacy module does.
        crate::auth::note_delete_leftovers(leftovers.len());
        super::bridge::nav_root(pages, AppArg::Login);
    }
}
