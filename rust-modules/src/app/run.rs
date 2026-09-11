//! The frame loop — phase 1b-ii's THIN COORDINATOR (restructure spec §13).
//!
//! `run` is the one `while app.running` loop; each phase of an iteration is a function below,
//! cut VERBATIM out of `plex_run`'s former body at the eight `FRAMEDROP` stamps (spec §8.4) so
//! that a diff of this commit is moves and renames only — `app.<field>` for what used to be a
//! loop-local, `fr.<field>` ([`Frame`]) for the handful of per-iteration values that cross a
//! phase boundary. Nothing is reordered;
//! the positional frame points the restructure names (NAV COMMIT, `opaque_route`, the present
//! decision, `clear_opaque_region` inside [`draw`], the swap) are the labelled lines of `run`
//! itself, which is why they are not folded into a phase function.
//!
//! Phase 2 replaces this loop with `ui::dispatch`'s ten-step algorithm (spec §3.3); the phase
//! names here are that algorithm's, mapped onto today's order (`navcommit` still runs BEFORE
//! `tick_drain` on this loop, and the `FRAMEDROP` line prints them in the spec's order).
//!
//! **Phase 5b is where the replacement stopped being a shadow.** The Settings family — the root,
//! Legal and its documents, Privacy & data, the first-run consent question and Favourite libraries
//! in both its modes — is now OWNED by the dispatcher, and this loop asks it four questions and
//! obeys one queue:
//!
//! * `Dispatcher::owns_input` — asked by the key, pointer, click and wheel arms alike. When it is
//!   true the arm pushes an `InputEvent` onto `app.inputs` and `continue`s; the ladders see
//!   nothing. Four hand-written modal arms and their ordering left this file with it.
//! * `bridge::host_frozen` / `host_replaced` — the container's fold, replacing the pairs of
//!   `is_open()` / `host_ground_ready()` reads the update and draw phases each had to keep in step.
//! * `bridge::page_owned` — whether the dispatcher draws the top PAGE too, folded with
//!   `host_replaced` into `bridge::page_plan` (`Owned` / `LegacyThenSurfaces` / `SurfacesOnly`),
//!   which is what the page closure and `Dispatcher::draw`'s single call per frame both read.
//! * `Bridge::take_reqs` — what an owned screen asked this loop to do (`loop_requests`), because
//!   the machines that own sign-in and the app's real stack are not on the dispatcher yet.
//!
//! **Phase 10 moved the boot-trigger scripts out of this file.** Where this loop used to call a
//! local `dev_scripts` and hold each arm's own retry counters and oscillator phases as fields on
//! `App` directly (`app.dev.<flag>`), it now calls [`crate::dev::scenarios::each_frame`] — one
//! call at the same position `dev_scripts` occupied — and the per-arm state lives on
//! `app.scenarios`. The oscillator continuations invoked from `update()` (`nav_osc_tick`,
//! `hero_osc_tick`, `search_osc_tick`, …) and the replay-after-EOS check in `playback_tick` moved
//! the same way; see `dev::scenarios`'s own module doc for the full inventory.
use super::*;
use crate::screens::registry::{HomeCmd, HomeTab};

/// The per-iteration values that cross a phase boundary. Reset at the top of every iteration
/// (`Frame::begin`); every field is written by exactly one phase and read by the ones after it.
pub(crate) struct Frame {
    /// The HUD's control slot for this frame, sampled once before input is read.
    pub(crate) ctrl: crate::ui::player_hud::ControlSlot,
    /// `clock::now()` at the ingest boundary — THE frame time every phase after it uses.
    pub(crate) now: u32,
    /// Seconds since the previous frame's `now`, clamped to 50 ms (the animation timestep).
    pub(crate) dt: f32,
    /// Something under a modal surface is still moving (its host must not freeze yet).
    pub(crate) underlay_moving: bool,
    /// The route is the player. NOT the present-gate question, which is
    /// `Player::video_plane_bound` (phase 9) — this is only "which screen is up", and the one
    /// thing still keyed on it is the draw's own `clear_opaque_region` + transparent clear.
    pub(crate) player: bool,
    /// This iteration draws and swaps (the present decision, spec §3.3 step 8).
    pub(crate) present: bool,
    /// The heartbeat's route word for this frame (`route_word`).
    pub(crate) rn: &'static str,
}

impl Frame {
    fn begin(ps: &crate::route::PlaybackSession) -> Frame {
        Frame {
            ctrl: crate::ui::player_hud::slot(ps),
            now: 0,
            dt: 0.0,
            underlay_moving: false,
            player: false,
            present: false,
            rn: "",
        }
    }
}

/// The loop. `plex_run` calls this once, after `boot`, and `shutdown` after it returns.
///
/// **No `mt: &MainThread` parameter since phase 9.** The token is a FIELD now
/// (`app.adapters.player`, see `player::adapter`), so the proof travels with `&mut App` — every
/// phase function below reaches the native session as `&mut app.adapters.player` and the seam as
/// `app.adapters.player.mt()`. Threading a second `&MainThread` beside `&mut App` would have
/// needed a second token, and minting one is the hole `MainThread::assume` documents.
pub(crate) unsafe fn run(app: &mut App) {
    while app.running {
        // Resolve the control row ONCE per iteration, before the event pump, and pass this
        // value to input, update and draw alike. `player_hud::slot()` reads `playpos_ns`, which
        // LG's media thread writes and `player::pump` advances mid-iteration — deriving it per
        // call site let a keypress activate a control this same frame then declined to draw.
        // Frame-drop detector, stamp 0 of 9: the TOP of the iteration, before the ls2 pump
        // and the event poll — `worstframe=` covers the whole iteration, not the half after
        // the input. Eight phases between the nine stamps, named after the frame algorithm
        // the restructure moves this loop onto (spec §3.3/§8.4); on THIS loop navcommit
        // precedes tick_drain, and the FRAMEDROP line prints them in the algorithm's order.
        let mut fr = Frame::begin(&app.player.session);
        let fr = &mut fr;
        app.instr.mark(crate::diag::heartbeat::Phase::Top);
        // The frame index the LANDING SCHEDULE stamps against (§3.3 step 3, `ui::landgate`),
        // published at the TOP because a landing site is reachable from the dev scenarios below
        // as well as from `land_results` and the dispatcher's own frame. One relaxed atomic load
        // unless `plxnative-rec` or `plxnative-recplay` is armed.
        app.rec.begin_frame();
        // REPLAY (`plxnative-recplay`): this frame runs on the recorded tick — set BEFORE
        // ingest, whose key arms stamp `last_input` from the clock — and the frame's recorded
        // inputs are re-injected through the same synthesis the remote FIFO uses, so the poll
        // below consumes them exactly as it consumed the originals.
        if let Some(t) = app.rec.replay_tick() {
            clock::set_replay(t.ms);
            for v in app.rec.replay_inputs() {
                replay_inject(app, fr, &v);
            }
        }
        crate::system::ls2_pump();
        ingest(app, fr);
        app.instr.mark(crate::diag::heartbeat::Phase::Ingest); // ingest

        fr.now = clock::now();
        // **The Player machine's tick** (spec §4.1): set ONCE per iteration, from the same
        // `fr.now` every other phase of this frame reads. `PlaybackSession::auto_last_switch` — the
        // adaptive controller's "how long since the last visible rung change" — is stamped from
        // it, so that stamp cannot disagree with the frame it belongs to. It used to come from
        // `player::vclock_ms()`, read at whatever depth of the call stack happened to need it.
        app.player.set_now(fr.now);
        // dev: /tmp/plxnative-autoplay auto-presses OK once
        //
        // **Never from the sign-in or the picker.** The auth flow hands its credentials to
        // the main thread through `take_ready`, which is polled only on those two routes; a
        // trigger that jumped to the player from the picker left a HALF-seated profile —
        // the worker had re-keyed the registry, but the session was never persisted and
        // `apply_pending` stayed set — so the next boot came up as the previous profile
        // and the offline-pick harness case found no cache record (device, 2026-09-06).
        // Waiting for the handoff costs a headless run the seconds the seating takes.
        if !crate::dev::scenarios::each_frame(app, fr) {
            continue;
        }
        playback_tick(app, fr);
        fr.dt = {
            let mut d = if app.prev != 0 {
                fr.now.wrapping_sub(app.prev) as f32 / 1000.0
            } else {
                0.016
            };
            if d > 0.05 {
                d = 0.05;
            }
            d
        };
        app.prev = fr.now;
        app.rec.tick(fr.now, fr.dt);
        // Whole-frame present gate (`ui::idle`): forget last frame's motion BEFORE the update
        // phase below re-steps every spring, so the flag it leaves describes THIS frame, and
        // stamp `dt` so a spring's velocity can be judged as travel-this-frame rather than as
        // a bare units-per-second. The decision itself is taken just above `glViewport`.
        crate::ui::idle::frame_begin(fr.dt);
        // ui::press (tvOS click) — advance the dip/spring every frame; when a deferred activation
        // commits (the spring-back bounce has played), run it for whichever CARD view armed the
        // press. A long-press does NOT commit (`press::tick` clears `want_commit` at `LONG_MS`):
        // on Home it opens the item menu below, and anywhere else it just springs back.
        let (_, press_moving) = crate::ui::idle::scoped_motion(|| {
            app.input.press.tick(fr.now, fr.dt);
        });
        // The motion of whatever page is UNDER a popover — Home, the Library or Search, since
        // the profile chip is a stop on all three. It was `home_underlay_moving` while only Home
        // could be underneath. The account popover's glass re-snapshots off this.
        fr.underlay_moving = press_moving;
        land_results(app, fr);
        app.instr.mark(crate::diag::heartbeat::Phase::Results); // results
        // NAV COMMIT — the route flips here (a transition at its floor, a cut now).
        capture_content_request(app);
        let returning_from_player = app.pages.nav.top_page().is_some_and(|e| matches!(e.arg, super::AppArg::Legacy(Route::Player))) && !matches!(app.route, Route::Player);
        nav_commit(app, fr);
        // **Publish the playback session into the tree** (spec §2.3) before the dispatcher's frame
        // and the draw that follows it, so every owned screen in one frame reads one consistent
        // picture. Only while something that reads it is mounted — see `Bridge::publish_playback`.
        {
            let live = matches!(app.route, Route::Player);
            app.bridge.publish_playback(&app.player.session, live);
        }
        // The container tree follows the committed route (a Replace CUT on a flip) and runs its
        // frame — the owned screens' inputs, ticks, timers and effects. `app/bridge.rs` is the seam.
        let (_word, tree_report) = super::bridge::frame_with_tap(
            &mut app.pages,
            &mut app.bridge,
            app.route,
            &app.trail,
            crate::ui::machine::Tick {
                ms: fr.now,
                dt_us: (fr.dt * 1_000_000.0) as u32,
            },
            std::mem::take(&mut app.inputs),
            &mut app.rec,
        );
        // **The OWNED pages' share of the underlay verdict.** The three legacy `|=` sites below
        // report the motion of the screens this loop still steps itself; this is the same term
        // for the screens the dispatcher steps, and it was DROPPED for the whole of phase 8 —
        // `frame_with_tap`'s report was bound to `_report` and thrown away. The dispatcher keeps
        // the verdict on its own ledger (`Present::page_moving`, scoped `Page` by default and
        // `Surface` around every modal tick and surface step — §4.4), so it is exactly the term
        // the legacy loop got from stepping `library::update` inside `idle::scoped_motion`.
        //
        // Its readers are `popover::host::begin_frame` below: `gfx::page_wash_dither`, which
        // costs a 2M-fragment dither on a wash that is about to slide, and `host_refresh`'s
        // `page_moving` term for a page under a FADING panel. Neither could see an owned page
        // move.
        fr.underlay_moving |= tree_report.underlay_moving;
        if returning_from_player { restore_played_entry(app); }
        content_requests(app, fr);
        crate::dev::scenarios::advance_content_boot(app, fr);
        loop_requests(app);
        app.instr.mark(crate::diag::heartbeat::Phase::NavCommit); // navcommit
        update(app, fr);
        app.instr.mark(crate::diag::heartbeat::Phase::TickDrain); // tick_drain
        // ---- the PREPARE WINDOW opens here (spec §3.3 steps 8-9, §8.1) -------------------
        //
        // **The budget's frame opens at the start of this window, not at the top of the
        // iteration**, and that placement is the whole of its ceiling's meaning. `take` admits
        // while `now - frame_start + worst_us <= PREPARE_MAX_US`; opened at the loop top, the
        // elapsed term already contained ingest, the result landings, the nav commit and the
        // tick drain, so on a cold-open frame (~62 ms) every upload was over the ceiling and
        // only the forward-progress escape let ONE through. A quota of three that was really a
        // quota of one, on exactly the frames with the most textures waiting.
        app.pages.budget.begin_frame(crate::diag::heartbeat::now_us());
        // The poster adapter's frame (spec §3.3 step 3's tail): a new frame for the slot LRU and
        // every decoded image handed to the render cache as owned pixels. No GL here — the
        // upload is below, on the presenting side of the decision.
        super::adapters::poster::begin_frame();
        super::adapters::poster::drain_decoded();

        fr.player = matches!(app.route, Route::Player);
        // ---- the player page's CLOCK-DRIVEN motion report (spec §8.3, §9) ----
        //
        // After the update phase, so this frame's springs and this frame's pump have been seen,
        // and before the gate below reads the verdict. Everything the player page draws from a
        // clock rather than from a spring is folded into ONE value and compared with last frame's;
        // `PlayerScreen::clock_fingerprint` is the inventory and the argument. Nothing to do off
        // the route — the page is not mounted and nothing it owns is on screen.
        if fr.player {
            let fp = super::bridge::player(&app.pages)
                .map(|p| p.clock_fingerprint(&app.player.session, fr.now));
            if let Some(fp) = fp {
                if app.player.note_clock(fp) {
                    crate::ui::idle::invalidate();
                }
            }
        }
        // ---- whole-frame present gate (`ui::idle`) --------------------------------------
        // A screen with nothing moving on it does not need to be re-sent to the panel. This
        // skips `glViewport`…`SDL_GL_SwapWindow` WHOLESALE — it is not dirty-RECTANGLE
        // tracking, which `ui/mod.rs`'s renderer doc rejects: when this says yes, the frame
        // below is byte-for-byte the immediate-mode frame it always was, clear and all.
        //
        // Measured cost of not doing this (2026-07-31): a still Home grid burns 16.0% of one
        // A53 core here plus ~19.4 points inside `surface-manager`, which must blend our
        // 1080p surface on every present — a charge that measured identical on three
        // different screens, so it is per-PRESENT, not per-pixel. ~35 points of a core to
        // re-send an unchanged picture, on a fan-less SoC that sits on Home for hours.
        //
        // **The exclusion is the BOUND PLANE, not the player ROUTE** (spec §9, phase 9).
        // `system.rs::clear_opaque_region` documents the hardware video plane as *slaved* to this
        // wayland surface, and "we stop presenting while a plane is slaved to it" is a claim about
        // this compositor that reading cannot settle — so while the plane is bound the gate
        // answers `true` unconditionally, and that term now lives INSIDE `should_present`, fed by
        // `Player::set_video_plane_bound`'s edges and by nothing else.
        //
        // What that changes is the frames on either side of a playback. Pre-bind (the spinner
        // while the route resolves and the Load is in flight) and post-unbind (the HUD fading out,
        // the failure read-out) are ORDINARY idle frames: nothing is slaved to the surface, so the
        // 35-points-of-a-core argument applies to them exactly as it does to Home — and every
        // player-side animator that advances from a clock has to report, or it freezes. Playback
        // itself still spends ~99% of its time with the HUD auto-hidden, where the frame is
        // already 0 draw calls.
        //
        // **The decision is taken ONCE and has TWO terms** (spec §3.3 step 8): the gate above,
        // and whether the frame budget holds queued prepare work. The second term is what makes
        // the upload step below legal on the presenting side of the decision — a texture waiting
        // to be uploaded is itself a reason to present, so nothing sits in the queue behind a
        // settled screen. It was inert before phase 11: nothing in the product ever published a
        // queue to the budget, so `has_queued_work()` was permanently false and the upload had to
        // run BEFORE the decision (and invalidate) to happen at all.
        crate::ui::tex::note_queued(&mut app.pages.budget);
        fr.present = crate::ui::idle::should_present(fr.now) || app.pages.budget.has_queued_work();
        app.rec.present(fr.present);
        // ---- step 9's UPLOAD, on the presenting side of the decision ---------------------
        //
        // The render cache's upload step under the frame budget (§3.3 step 9, §10: "a frame that
        // does not present uploads nothing"). Two placement rules, both load-bearing:
        //
        // * **Only on a presenting frame.** `GfxUploader::warm` is `gfx::warm_tex`, which DRAWS
        //   a 1x1 quad to force residency; there is no GL scope to draw into on a frame that is
        //   skipped wholesale.
        // * **Before the draw, never after.** That quad goes into framebuffer 0, and the page's
        //   own `frame_clear` overwrites the pixel. After the draw it would be a white pixel over
        //   the finished picture — over FILM, on a player frame.
        if fr.present {
            let mut ph = crate::ui::machine::PresentHandle::of(&mut app.present);
            super::adapters::poster::prepare(
                &mut app.pages.budget,
                &mut ph,
                crate::diag::heartbeat::now_us,
            );
        }
        // EXPERIMENT (`/tmp/plxnative-opaque`): one `static` read and a return when the trigger
        // is absent. Edge-triggered — see `system.rs`.
        //
        // Called on EVERY frame, from the BIT rather than from the route. The false edge after an
        // unbind may land on a frame that would otherwise not present (spec §3.3 step 9), and
        // there is nothing else in the loop that would carry it: a `return` above, or a term that
        // only ran on player frames, would leave the compositor believing our surface is still
        // opaque with an ordinary UI on it.
        crate::system::opaque_route(app.player.video_plane_bound);
        app.instr.mark(crate::diag::heartbeat::Phase::Prepare); // prepare
        // `worstprep=`: the prepare phase is timed on EVERY iteration, presented or not — a
        // settled screen must never run untimed work at the loop rate (spec §8.3).
        app.instr.note_prepare();
        // Hoisted: the frame-drop detector reads these after the gate. Seeded to the pump
        // stamp so a skipped frame reports zero draw/cap/swap rather than a stale delta.
        app.instr.skip_present_phases();
        if fr.present {
            // the glyph cache's frame serial (phase 11, text.rs's hot window): a drawn frame
            crate::text::begin_frame();
            let (_vx, _vy, _vw, _vh) = draw(app, fr);
            app.instr.mark(crate::diag::heartbeat::Phase::Draw); // draw
            // dev capture stream: grab this finished frame before the swap (after the last draw,
            // so the copy's pass-flush is work the swap would submit anyway). One atomic when idle.
            // Deliberately NOT while the video plane is BOUND (the UI plane is transparent over
            // video, so there is nothing to grab) — capture.rs's 5s keepalive resend covers the
            // host's deadness timer while playback is up. Keyed on the bit rather than the route
            // since phase 9: a pre-bind spinner and a post-unbind read-out ARE ordinary UI-plane
            // pictures, and the operator watching a stream had no reason to lose them.
            if !app.player.video_plane_bound {
                crate::capture::tick(fr.now);
            }
            app.instr.mark(crate::diag::heartbeat::Phase::Capture); // capture
            // Before the swap, never after: the back buffer is undefined once presented.
            #[cfg(feature = "hostsim")]
            crate::shot::maybe_capture(_vx, _vy, _vw, _vh);
            SDL_GL_SwapWindow(app.win);
            // One increment, then nothing: re-ask EGL for the back buffer's AGE after real
            // presents have happened. The boot reading is 0 by construction. See `egl.rs`.
            crate::egl::late_probe();
            #[cfg(feature = "devtools")]
            {
                app.buffer_flip_count = (app.buffer_flip_count + 1) % 60;
            }
            crate::ui::widgets::glass_presented();
            app.instr.mark(crate::diag::heartbeat::Phase::Swap); // swap
            // Inside the gate: `frame_end` is the end of a DRAWN frame. Counting frames the
            // idle gate skipped would pace the profiler's once-per-N-frames log off frames
            // that ran no phases at all.
            crate::ui::profile::frame_end();
            crate::ui::overdraw::frame_end();
            // Same reason, same gate: the blur's region accounting is per DRAWN frame. It rolls
            // "what every glass surface asked for this frame" into the region the next frame's
            // first snapshot is taken at. Once that union is known, several surfaces share one
            // capture; a first discovery frame may still need a second non-contained grab.
            crate::gfx::blur_frame_end();
            crate::ui::idle::note_present(fr.now);
        } else {
            // The swap is this loop's ONLY blocking call — there is no SDL_Delay, nanosleep
            // or frame budget anywhere else in it. Skipping the present without sleeping here
            // would turn a 16%-of-a-core app into a 100% spinner: strictly worse than the
            // problem. One frame period, so input latency is exactly what it is today.
            SDL_Delay(crate::ui::idle::IDLE_POLL_MS);
        }
        report(app, fr);
        heartbeat(app, fr);
    }
}

/// Ingest: the lab command channel, the remote FIFO and the SDL event queue (spec §3.3 step 2).
/// Every key, pointer, text and lifecycle event the frame acts on enters here.
pub(crate) unsafe fn ingest(app: &mut App, fr: &mut Frame) {
        // Cloud Test Lab has no SSH/FIFO. Its LAB build long-polls outward, then leaves each
        // command here for the SDL thread so the same dispatcher and event queue remain the
        // only input path. Acknowledge acceptance after dispatch, before polling SDL below.
        for command in crate::lab::take_commands() {
            let ok = ingress_token(app, fr, &command.token);
            crate::lab::command_done(command.id, ok);
        }
        // The FIFO's tokens are collected first so the recorder (a field beside `remote`) can
        // be written per token; a token that pushes SDL events is recorded where they are
        // polled, a DIRECT one (`pat:`, `shot`, `txt:`) here — `token_is_direct` is the split.
        let mut toks: Vec<String> = Vec::new();
        if let Some(r) = app.remote.as_mut() {
            r.drain(|tok| toks.push(tok.to_string()));
        }
        for tok in toks {
            let _ = ingress_token(app, fr, &tok);
        }
        drain_sdl(app, fr);
}

unsafe fn drain_sdl(app: &mut App, fr: &mut Frame) {
    while SDL_PollEvent(app.ev.as_mut_ptr() as *mut c_void) != 0 {
        ingest_sdl_event(app, fr);
    }
}

unsafe fn ingress_token(app: &mut App, fr: &mut Frame, token: &str) -> bool {
    // Keys and clicks use SDL's existing synthesis while coexistence lasts. Consume those
    // before a following direct text token, rather than moving all text ahead of all keys.
    if super::bridge::search_owns_input(&app.pages, app.route) { drain_sdl(app, fr); }
    if super::bridge::search_owns_input(&app.pages, app.route) {
        if let Some(text) = token.strip_prefix("txt:") {
            ingest_text(app, &text.replace('+', " "), crate::textinput::available(),
                crate::ui::machine::Source::RemoteFifo);
            return true;
        }
    }
    let accepted = dispatch_remote_token(token, &app.player.session);
    if accepted && token_is_direct(token) { app.rec.input(super::recorder::enc_token(token)); }
    accepted
}

/// **Pin the transport for a headless run, and park its ring** — the dev triggers' half of what
/// `start_playback` does for a real one.
///
/// One helper rather than three copies of `set_hud(now + HUD_HEADLESS_MS)`: the deadline and the
/// cursor are the player INSTANCE's since restructure phase 9, so every one of those copies would
/// have had to learn the same `player_mut` lookup and the same `publish`. `tab` parks the bottom
/// tab row (the Info card is tab 0, the Chapters strip tab 1) and moves the ring onto it; `None`
/// leaves the cursor alone, which is what the track menu and the auto-pause script want.
pub(crate) fn pin_headless_hud(app: &mut App, now: u32, tab: Option<i32>) {
    if let Some(player) = super::bridge::player_mut(&mut app.pages) {
        player.hud.extend(now, HUD_HEADLESS_MS);
        if let Some(tab) = tab {
            player.hud.nav.focus = 2;
            player.hud.nav.tab = tab;
        }
        player.publish();
    }
}

fn ingest_text(app: &mut App, text: &str, panel: bool, source: crate::ui::machine::Source) {
    if text.is_empty() { return; }
    let at = crate::ui::machine::Tick { ms: clock::now(), dt_us: 0 };
    app.rec.input(super::recorder::enc_text(text, panel, at, source));
    app.inputs.extend(text_inputs(text, panel, at, source));
    app.last_input = at.ms;
    crate::ui::idle::invalidate();
}

/// One polled event. Shared by ordinary polling and ordered FIFO/replay ingestion.
unsafe fn ingest_sdl_event(app: &mut App, fr: &mut Frame) {
    let et = rd_u32(&app.ev, 0);
    // INPUT while a popover holds the page frozen is the POPOVER's: every invalidate
    // such an event raises — the one below, and whatever its handler adds — is
    // attributed to it, so the frozen host is not re-rendered on every key-up
    // (`popover::host::input_scope`). Only input: a lifecycle or window event is the
    // APP's and may change the page under the panel (backgrounding drops Search's
    // editing layout), so its damage stays the page's and the snapshot is retaken.
    let _own_input = if is_input_event(et) {
        crate::ui::popover::host::input_scope()
    } else {
        None
    };
    // ANY event is a reason to repaint (`ui::idle`): a key changes focus or a label,
    // a lifecycle event changes the whole screen. Marked here — once, for every event
    // kind — rather than in each of the ~30 arms below, where the next one added would
    // silently draw nothing.
    crate::ui::idle::invalidate();
    if et == SDL_KEYDOWN
        || et == SDL_KEYUP
        || et == SDL_TEXTINPUT
        || et == SDL_TEXTEDITING
    {
        // 48 bytes, not 32: a TEXTINPUT event's `text[32]` starts at +16 on the
        // television (LG's `inputSource` shifts it), so it ENDS at exactly +48. At the
        // old width the one event whose payload the offsets are most easily wrong
        // about would have left a forensic trail that stopped just before the payload.
        let mut hex = String::with_capacity(96);
        for b in &app.ev[..48] {
            hex.push_str(&format!("{b:02x}"));
        }
        let what = match et {
            SDL_TEXTINPUT => "text",
            // **The IME's PRE-EDIT, and the reason it is logged at all.** The panel's
            // word prediction is a REPLACE — tapping "summer" under a typed "summ"
            // means *delete what I was predicting on, then commit this* — and the app
            // sees only the commit, so the field reads "summsummer" (reported from the
            // couch 2026-08-15). Whether the delete half reaches us as `SDL_TEXTEDITING`
            // (`text_model.delete_surrounding_text` mapped onto SDL's pre-edit) or as
            // nothing at all decides whether the fix can be exact or has to be a
            // heuristic — and this arm is the only way to find out, since nothing in
            // the app has ever read this event.
            SDL_TEXTEDITING => "edit",
            _ => "key",
        };
        log(&format!(
            "[{}] {what} type=0x{et:x} raw={hex}",
            clock::now()
        ));
    }
    if (0x103..=0x106).contains(&et) {
        app.rec.input(super::recorder::enc_lifecycle(et));
    }
    if et == SDL_QUIT {
        app.running = false;
    } else if et == 0x103 || et == 0x104 {
        // WILL/DID ENTER BACKGROUND
        log(&format!(
            "LIFECYCLE: background (playing={})",
            matches!(app.route, Route::Player) as i32
        ));
        // **The TELEVISION'S KEYBOARD goes with the panel, and it is not ours to keep.**
        // The compositor tears its own IME down when it takes the screen away, and it
        // tells the app nothing — so a field left `editing` comes back to the
        // foreground drawing an editing layout and a blinking caret over a keyboard
        // that is gone, and typing is dead in a way no press can recover:
        // `textinput::start` early-returns while its own `STARTED` is set, so OK on the
        // field would toggle our flag and raise nothing. This is `leave` — the same
        // dismissal a route change runs (`leave_of`) — and deliberately NOT the commit
        // path `leave_field` takes: the OS moving the screen is not the user saying
        // "that is the search I meant", and a half-typed term must not be filed in
        // their recent searches by an app switch. Unconditional because `EDITING` is
        // this screen's alone and both calls under it are guarded, so it costs a
        // predictable nothing on every other route. Search is unconditionally owned, so
        // there is no legacy fallback left to dispatch to.
        // Keep dismissal after earlier text/keys in this input batch. The dispatcher
        // releases both system ownership and the native start latch at delivery.
        app.inputs.push(crate::ui::machine::InputEvent {
            at: crate::ui::machine::Tick { ms: fr.now, dt_us: 0 }, source: crate::ui::machine::Source::Sdl,
            kind: crate::ui::machine::InputKind::SystemKeyboard(false),
        });
        // **The TREE hears it too** (spec §9, §12.1) — phase 9. `ScreenEvent::Suspend` down every
        // mounted body at this frame's NAV COMMIT, and `Navigation.suspended` set. The vocabulary
        // and the delivery have existed in `ui/containers` since the containers landed
        // (`Navigation::suspend`, `Dispatcher::suspend`) and NOTHING CALLED THEM: the screens that
        // handle the event — Settings, which forwards it down its own stack, and Search, which
        // drops the television's keyboard — were answering an event that could not arrive.
        //
        // Outside the player guard below on purpose: this is the OS taking the screen from
        // whatever is on it, not a fact about playback.
        app.pages.suspend();
        // The compositor may destroy the wl_surface when it takes the screen. Null the
        // cached Wayland pointers now so any frame that still runs before the loop gate
        // drops will find G_WL_SURFACE null and short-circuit in clear_opaque_region
        // rather than calling wl_proxy_marshal on a freed proxy (SIGSEGV, PLX-NATIVE-K).
        crate::system::sys_release_wayland();
        if matches!(app.route, Route::Player) && !app.player.lifecycle.awaiting_load() {
            // INTENDED, not published: this snapshot is the only thing the foreground
            // restore has, and `suspend_bufferfeed` below drops the pending seek target
            // with the session — so a background that lands while a seek is still
            // resolving would otherwise save (and restore to) the spot the user just
            // seeked AWAY from, with nothing left to correct it. See `intended_pos`.
            let saved_ns = intended_pos(&mut app.player.session);
            let clock = app.player.lifecycle.clock_for_suspend(paused());
            app.player.lifecycle.suspend(saved_ns, clock);
            app.ptr.drag = false;
            // The OS took the screen; the session is preserved for the foreground reload, so
            // this is the RELOAD half of the reset — the in-flight gesture goes, the transport's
            // timer and the countdown stay. It is spelled as a delivery to the owner because
            // `player::TX::reset_for_reload` no longer clears `scrub_ns` from under it (§2.3).
            if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                player.transport_reset(true);
            }
            close_player_overlays(&mut app.pages);
            crate::player::suspend_bufferfeed(&mut app.player.session, &mut app.adapters.player); // preserve the session for a clean fg reload
                                                   // …and drop any play resolve still in flight. `start_playback` flips to
                                                   // Route::Player as soon as a resolve starts, with NO engine behind it, so
                                                   // this arm fires during that whole window — and `suspend_bufferfeed` is a
                                                   // no-op when there is no engine yet. Without this the plan lands later in
                                                   // the route-UNCONDITIONAL `pump_play` arm and starts playback with the UI
                                                   // on Home, where OK/Stop/seek and the EOS teardown are all route-gated:
                                                   // audio and video running that the user cannot pause or end.
            crate::route::cancel_play(&mut app.player.session);
            // The BACK trail is deliberately NOT touched: this is the OS taking the
            // screen away, not the user navigating, and the foreground arm below reloads
            // straight back into the player. Route and trail may therefore disagree for
            // as long as the app is backgrounded, which is safe because Home's BACK
            // branch never consults the trail and the first Home activation truncates it.
            app.route = Route::Home;
        }
    } else if et == 0x105 || et == 0x106 {
        // WILL/DID ENTER FOREGROUND
        log(&format!(
            "LIFECYCLE: foreground (wasPlaying={})",
            app.player.lifecycle.awaiting_load() as i32
        ));
        // The other half of the park (§12.1): `ScreenEvent::Resume` down the tree and
        // `Navigation.suspended` cleared. On BOTH edges, matching the suspend above — webOS sends
        // `will` and `did` and the app is told nothing about which of the two it will get first;
        // `Navigation::resume` is idempotent, so the pair is safe and a lost one is not.
        app.pages.resume();
        if et == 0x106 {
            // Re-acquire the Wayland surface pointer: the compositor may have recreated the
            // wl_surface since sys_release_wayland nulled it on background. This restores
            // clear_opaque_region to normal operation before the first rendered frame.
            crate::system::sys_grab_wayland(app.win);
            let activation = drive_foreground(
                &mut app.player.lifecycle,
                &mut app.player.session,
                ForegroundInput::DidForeground,
                &mut PlayerForegroundActuator {
                    pa: &mut app.adapters.player,
                    repause_at: &mut app.repause_at,
                },
            );
            if matches!(activation, ForegroundActivation::Launched) {
                app.route = Route::Player;
                // The page mounts at this frame's NAV COMMIT and pins its own transport from
                // the instant it does (`AppMounter::player_hud_ms`); a live one is re-pinned
                // here, which is the foreground half of `start_playback`'s two paths.
                app.bridge.seed_player_hud(HUD_LINGER_MS);
                if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                    player.hud.extend(clock::now(), HUD_LINGER_MS);
                    player.publish();
                }
            }
        }
    } else if et == SDL_KEYDOWN || et == SDL_KEYUP {
        let (state, wcode, sym) = decode_key(&app.ev);
        // The press's IDENTITY, resolved once from the two raw fields
        // (`ui::consts::classify`, which is where the spellings live and where they
        // are tested). `sym` and `wcode` are still read raw by the arms below — the
        // ones that forward them to a screen's own `move_focus`/`key`, and the modal
        // panels and the CH▲/CH▼ pager, which still spell their own key tests.
        let key = classify(sym, wcode);
        let isnav = matches!(key, Key::Left { .. } | Key::Right { .. });
        app.rec.input(super::recorder::enc_key(
            sym,
            wcode,
            (state & 0xff) == 1,
            state & 0x100 != 0,
        ));
        // **Does the TREE own this key?** (phase 5b, `app/bridge.rs`'s coexistence
        // contract.) Asked once, here, above all three edges — because the dispatcher's
        // press machine wants ALL of them: `Edge::Down` arms, `Edge::Repeat` is the
        // dropped-key-up net's liveness beat, and `Edge::Up` is the release. Handing it
        // only the fresh presses would leave a dipped control committing a second later
        // at `press::MAX_HOLD_MS` instead of on the button coming up.
        let tree_owns_key = super::bridge::owns_input(&app.pages, app.route);
        // `clock::now()`, not `fr.now`: INGEST runs before the frame stamps its own time
        // (see `run`'s phase order), so `fr.now` is still 0 here — the same reason the
        // ladder below reaches for the clock to fill `app.last_input`. `dt_us` is 0
        // because an event's `at` is a stamp, not a timestep: every machine the
        // dispatcher steps is driven by the FRAME's tick, which `bridge::frame` supplies.
        let tree_tick = crate::ui::machine::Tick {
            ms: clock::now(),
            dt_us: 0,
        };
        if (state & 0xff) != 1 {
            if tree_owns_key {
                app.inputs.push(super::bridge::key_input(sym, wcode, state, tree_tick, crate::ui::machine::Source::Sdl));
            }
            // …and the loop's own key-up bookkeeping runs either way: it retires the sym
            // from both held-key slots, which is state about the PHYSICAL key rather than
            // about whoever read it. Skipping it while a surface is up would leave a key
            // the ladders never saw go down looking held the moment the surface closes.
            on_key_up(
                sym,
                isnav,
                app.route,
                app.ok_armed,
                &mut app.down_sym,
                super::bridge::player_mut(&mut app.pages).map(|player| &mut player.scrub),
                &mut app.repause_at,
                &mut app.input.press,
            );
            // ONE publish after the step that moved it (§2.3): a key-up can commit or abandon a
            // scrub, so the worker-visible `TX.scrub_ns` is refreshed from the owner right here
            // rather than by the release path writing the atomic itself.
            if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                player.publish();
            }
            return;
        }
        // A repeat is only a repeat if we watched the key go down. See `App::down_sym`:
        // the system keyboard eats key-ups, so the driver stamps 0x100 on presses that are
        // the FIRST of their own gesture, and dropping those loses one press in two.
        if state & 0x100 != 0 && sym == app.down_sym {
            if tree_owns_key {
                // **Item 13 survives the migration, and only the DIRECTIONS are gated.**
                // A held key repeats every ~50 ms; a settings row or a line of reading
                // text per beat is a blur nobody can track, and the focus engine has no
                // cadence of its own — so `RepeatGate` still throttles the moves. The OK
                // edges go through UNGATED, deliberately: `dispatch`'s ingest reads an OK
                // `Edge::Repeat` as `press.note_alive`, so a swallowed beat is a hold that
                // looks like a lost key-up and springs back without activating.
                let ok = is_ok(sym);
                if ok || app.modal_repeat.ready(tree_tick.ms) {
                    app.inputs.push(super::bridge::key_input(sym, wcode, state, tree_tick, crate::ui::machine::Source::Sdl));
                }
                return;
            }
            // The cursor and the scrub are two fields of ONE owner, so they are borrowed out of
            // it together — reading the ring from `App` and the ramp from the screen is exactly
            // the split ownership phase 9 removes.
            let (hud_nav, scrub) = match super::bridge::player_mut(&mut app.pages) {
                Some(player) => (player.hud.nav, Some(&mut player.scrub)),
                None => (HudNav::HOME, None),
            };
            on_auto_repeat(
                sym,
                isnav,
                app.route,
                app.ok_armed,
                hud_nav,
                scrub,
                &mut app.input.press,
            );
            return;
        }
        // From here down this IS a fresh press, whatever the driver stamped on it.
        app.last_input = clock::now();
        begin_fresh_press(&mut app.player.session, 
            key,
            sym,
            wcode,
            app.last_input,
            &mut app.down_sym,
            super::bridge::player_mut(&mut app.pages).map(|player| &mut player.hud),
            &mut app.ptr,
            &mut app.ok_armed,
            &mut app.input.press,
        );
        // `note_fresh_press` may have extended the auto-hide deadline; publish it once, here.
        if let Some(player) = super::bridge::player_mut(&mut app.pages) {
            player.publish();
        }

        // LAB BUILDS ONLY, and above every arm below including the modals: the
        // diagnostics trigger. It has to outrank the chain because the screen a tester
        // most needs a snapshot of is the playback failure read-out, whose own arm
        // `continue`s on every key — and because a snapshot changes no app state, so
        // there is nothing for a later arm to have wanted first. Compiles to `false`
        // in every other build (`crate::lab::key_press`).
        if crate::lab::key_press(sym, wcode, &app.player.session) {
            return;
        }

        // ---- the route-scoped arms, each of which `continue`s once it has taken the
        // press. That makes the chain itself the priority statement: an earlier guard
        // subsumes each later one it overlaps with, which the playback-failure guard
        // below does deliberately. Keep it a chain — a `match` over the same routes
        // compiles and keeps the suite green while silently reordering it, because
        // exhaustiveness cannot see subsumption.
        //
        // **THE DISPATCHER'S ARM, and it is one line because that is the whole point**
        // (phase 5b). Three hand-written arms stood here — consent above legal above the
        // settings root — and the height of each in this chain WAS its modality: each
        // `continue`d on every key, and the ordering was load-bearing because BACK out of
        // the privacy notice would otherwise have been read as Home's ROOT PRESS and
        // handed the screen to the television. That ordering is now a container's: the
        // Settings family is a modal SURFACE on the app's `ModalStack`, whose inner
        // `NavStack` walks itself on BACK and only then lets the container dismiss it, so
        // "which of the four answers this key" is a tree walk rather than a chain the next
        // editor has to keep in order. `owns_input` is the same question the pointer,
        // click and wheel arms below ask, which is what stops the four drifting apart.
        //
        // It stays HIGH in the chain for the reason the old arms did: an owned surface is
        // modal over whatever route is behind it, and every route arm below would
        // otherwise act on a page the user cannot see.
        //
        // **The fourth root is closed here** (`app/input.rs`'s `back_at_root`). BACK at
        // the FIRST consent stage used to be swallowed — the step behind it is sign-in,
        // which cannot be undone — and the old comment recorded why the 2026-09-03 root
        // rule could not reach it: `ui::consent::on_back` reported `true` for both the
        // stepped-back and the swallowed case, so a BACK arm could not tell them apart
        // without changing that module's return type. The owned screen simply says which
        // it is: `ConsentPage` answers `Handled::No` at a Settings BACK and pushes
        // `LoopReq::BackAtRoot` at the first stage, and the request drain below performs
        // it. Nothing is stranded by going to the television's Home — the question is
        // neither answered nor dismissed, and selecting the tile again comes back to it.
        if tree_owns_key {
            app.inputs.push(super::bridge::key_input(sym, wcode, state, tree_tick, crate::ui::machine::Source::Sdl));
            return;
        }
        // `Route::Login | Route::Profiles` is deliberately absent here (phase 6, mirroring
        // `Route::Onboard`'s own removal in 5b): both are OWNED screens now, so a key on
        // either was already taken by `tree_owns_key` above and never reaches this chain.
        // (`Route::ItemMenu`'s arm stood here, above every route-scoped one — it was MODAL, so it
        // took every key. The item menu is a `ModalStack` surface since phase 10, so its keys were
        // already claimed by `tree_owns_key` well above this chain and it can never be reached.)
        // A failure owns the frame, except for the recovery-quality popover it opened
        // itself.  Stale Menu / Info / Chapters panels remain unreachable; More is the
        // one drawn and drivable escape promised by the failure read-out.
        if matches!(app.route, Route::Player)
            && crate::ui::player_hud::transport_hidden(&mut app.player.session)
            && !matches!(
                app.route,
                Route::Player
            )
        {
            key_player_failed(&mut app.player.session, 
                &mut app.adapters.player,
                sym,
                wcode,
                &mut app.route,
                &app.play_from,
                &mut app.refresh_hubs_at,
                &mut app.trail,
                &mut app.pages,
            );
            return;
        }
        // (Four arms stood here — the track menu, the `…` popover, the Info card and the
        // Chapters strip, each gated on `overlay_swallows_key` so that a transport key FELL
        // THROUGH to the ordinary Pause/Play arms below while everything else was swallowed. The
        // four panels are SURFACES on the player page's own `ModalStack` since restructure phase
        // 9, so `tree_owns_key` at the top of this chain has already handed the key to the one
        // that is open and returned; the fall-through, which a surface cannot perform, is
        // `PlayerReq::Transport` — see `screens::player::overlay`'s module doc.)
        if matches!(app.route, Route::Player) && matches!(key, Key::Up | Key::Down) {
            if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                key_player_updown(key, app.last_input, &mut player.hud, &mut player.scrub);
                player.publish();
            }
            return;
        }
        // `Route::Search` is deliberately absent here (phase 6/7 Search cutover, mirroring
        // `Route::Login | Route::Profiles` above): Search is unconditionally an OWNED screen
        // now, so `tree_owns_key` above already took every key on this route and returned —
        // this chain can never see one.
        // ---- and the arms on key IDENTITY, which the routes above have already had
        // their pick of. Still one `else if` chain, still in this order: four of its
        // nine tests carry a route term as well as a key one (this first arm, Stop, the
        // player's LEFT/RIGHT and the Library pager), so the order is behaviour too.
        //
        // The plain syms only (`alt: false`) — the alternate D-pad codes reach no arm
        // that navigates a non-player screen. See `Key::Left`, which carries that
        // asymmetry between this test and the player's scrub arm below.
        if !matches!(app.route, Route::Player)
            && matches!(
                key,
                Key::Up
                    | Key::Down
                    | Key::Left { alt: false }
                    | Key::Right { alt: false }
            )
        {
            // Every non-player route reaching this far down the ladder is an OWNED page
            // (Home/Library/Search included, since the Search cutover), and an owned page's
            // directions are taken by `tree_owns_key`'s arm well above this chain — this branch
            // is reached by nothing and does nothing (`key_move_focus`, the function that used
            // to live here, was itself already an unconditional no-op and is retired). The
            // condition stays so this arm keeps its PRECEDENCE over `wcode == WCODE_POINTER_HIDDEN`
            // below for the documented direction+wcode-hidden combination noted there; removing
            // the branch instead of its body would let that combination fall through to it.
        } else if wcode == WCODE_POINTER_HIDDEN {
            // LG pointer auto-hidden; ignore.
            //
            // THE RAW `wcode`, not `Key::PointerHidden`, and this is the one arm in the
            // ladder that cannot use the classified value. Its precedence here is
            // ROUTE-DEPENDENT: the nav arm above it is `!Player && <direction>`, so for
            // an event carrying BOTH a direction sym and this wcode, Home moves focus
            // (the nav arm wins, being higher) while the player swallows it (the nav arm
            // is skipped, and this one catches it before the scrub arm below).
            //
            // `classify` is a pure function of the pair and cannot express that — it has
            // one linear order and no route. Ordering it directions-first reproduces
            // Home and makes the player SEEK on a pointer notification; ordering it
            // pointer-first reproduces the player and freezes Home's navigation. So the
            // classifier keeps directions first (Home correct) and the raw test stays
            // here, at the position that was always the player's answer.
            //
            // Whether any real event carries that pair is unrecorded: nothing in the
            // tree names the sym beside wcode 0x1e4, `remote_token_key` never emits it,
            // and the simulator cannot produce one — so `tools/keytable.py` is blind to
            // this by construction. Settling it needs a `key` line off the television.
        } else if matches!(key, Key::Ok) {
            key_ok(&mut app.player.session, 
                &mut app.adapters.player,
                app.last_input,
                &mut app.route,
                &mut app.ptr,
                &mut app.trail,
                &mut app.play_from,
                &mut app.ok_armed,
                &mut app.input.press,
                &mut app.pages,
            );
        } else if matches!(key, Key::Pause) {
            key_pause(&mut app.adapters.player, app.route, app.last_input, &mut app.pages);
        } else if matches!(key, Key::Play) {
            key_play(&mut app.player.session, 
                &mut app.adapters.player,
                app.last_input,
                &mut app.player.lifecycle,
                &mut app.repause_at,
                &mut app.route,
                &mut app.play_from,
                &mut app.ptr,
                &app.trail,
                &mut app.pages,
            );
        } else if matches!(key, Key::PlayPause) {
            // ONE key, both directions. `key_play`/`key_pause` are each half of the
            // toggle, so this arm picks; off the player route `key_play` is what starts
            // playback, which is the right answer for a PLAYPAUSE press on a card.
            if paused() || !matches!(app.route, Route::Player) {
                key_play(&mut app.player.session, 
                    &mut app.adapters.player,
                    app.last_input,
                    &mut app.player.lifecycle,
                    &mut app.repause_at,
                    &mut app.route,
                    &mut app.play_from,
                    &mut app.ptr,
                    &app.trail,
                    &mut app.pages,
                );
            } else {
                key_pause(&mut app.adapters.player, app.route, app.last_input, &mut app.pages);
            }
        } else if matches!(key, Key::Exit) {
            // The remote's EXIT key — LG's checklist item 38 wants the app terminated,
            // and unlike BACK at Home's root there is nothing ambiguous about a key
            // labelled EXIT, so unlike BACK it really does end the process — and it
            // is now the only key that does.
            log("EXIT key: terminating");
            app.running = false;
        } else if matches!(app.route, Route::Player) && matches!(key, Key::Stop) {
            // Stop — the whole arm is the one ritual, already named.
            exit_player(&mut app.player.session, &mut app.adapters.player, &mut app.route, &app.play_from, &mut app.refresh_hubs_at, &mut app.trail, &mut app.pages);
        } else if matches!(app.route, Route::Player)
            && matches!(key, Key::Left { .. } | Key::Right { .. })
        {
            if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                key_scrub(&mut app.player.session, key, app.last_input, fr.ctrl, player, &mut app.ptr);
                player.publish();
            }
        } else if matches!(key, Key::Back) {
            key_back(&mut app.player.session, 
                &mut app.adapters.player,
                &mut app.route,
                &mut app.nav_pending,
                &mut app.trail,
                &app.play_from,
                &mut app.refresh_hubs_at,
                &mut app.pages,
            );
        }
    } else if et == SDL_MOUSEMOTION {
        app.last_input = clock::now();
        app.ptr.last_motion = app.last_input;
        app.ptr.cur_hidden = false;
        let (mx, my) = ptr_xy(&app.ev);
        app.rec.input(super::recorder::enc_pointer("pointer", mx as i32, my as i32));
        if app.ptr.prev_mx >= 0.0 {
            app.ptr.mot_accum += (mx - app.ptr.prev_mx).abs() + (my - app.ptr.prev_my).abs();
        }
        app.ptr.prev_mx = mx;
        app.ptr.prev_my = my;
        // The BARE transport's hover. A player panel is a surface and takes its own pointer
        // events through `owns_input` below (the `…` popover's hover-to-focus among them), so the
        // arm that used to dispatch it from inside this one is gone with the route field it
        // matched on.
        if matches!(app.route, Route::Player) && !app.pages.surface_up() {
            if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                player.hud.dismissed = false;
                player.hud.extend(app.last_input, HUD_LINGER_MS);
                if app.ptr.drag && dur() > 0 {
                    let frac = crate::ui::player_hud::scrub_frac_x(mx) as f64;
                    player.scrub.ns = (frac * dur() as f64) as i64;
                }
                player.publish();
            }
            return;
        }
        if app.ptr.dpad_mode {
            if app.ptr.mot_accum < 120.0 {
                return;
            }
            app.ptr.dpad_mode = false;
        }
        // Hover is the tree's on every screen it owns — `ui::route_screen`'s rule 11
        // ("hover parks focus on every screen in the family") is the HIT MAP's job now.
        // Three hand-written arms stood here, keyed by the same open-state flags the key
        // ladder used, and they existed because a hover ladder keyed by `route` reached
        // none of the family: a pointer move across Settings, Privacy & data or Legal
        // drove HOME's focus underneath instead. The press-cancel each of them paid for
        // by hand — a pointer sliding off the control it armed must abandon that press —
        // is `dispatch`'s ingest, which cancels an arm whose hit no longer resolves to it.
        if super::bridge::owns_input(&app.pages, app.route) {
            // `app.last_input`, stamped from the clock at the top of this arm: `fr.now`
            // is not written until after ingest (`tree_tick` above says why).
            app.inputs.push(super::bridge::pointer_input(
                mx,
                my,
                crate::ui::machine::Tick { ms: app.last_input, dt_us: 0 },
            ));
            return;
        }
        // `Route::Profiles | Route::Search | Route::ItemMenu` are deliberately absent (phases
        // 6/7/10): all three are owned screens or surfaces now, so their hover was already taken
        // by `owns_input()` above.
    } else if et == SDL_MOUSEBUTTONDOWN {
        app.last_input = clock::now();
        {
            let (cx, cy) = ptr_xy(&app.ev);
            app.rec.input(super::recorder::enc_pointer("click", cx as i32, cy as i32));
        }
        // A FRESH click supersedes a press still in flight from the previous one — the
        // pointer's twin of `begin_fresh_press`'s nav-key abort. Without it, clicking a
        // control and then something else inside the ~210 ms commit window let the first
        // click's deferred activation fire AFTER the second had already acted (two
        // `on_ok`s: the watched toggle flipped twice, each with its own blocking
        // refetch). Every arm below re-arms from scratch.
        //
        // It sits at the TOP of the pointer handler rather than inside one route's arm,
        // where it lived while only detail cards armed a press from a click. Every
        // control face defers now, so the interleaving it guards is reachable on every
        // screen — including across arms, e.g. Home's hero pill pressed and then a tab
        // pill clicked, which navigates at once and would otherwise have played the
        // hero a moment later on the page it had just left.
        if app.ok_armed {
            app.input.press.cancel();
            app.ok_armed = false;
        }
        // Rule 11's click half, and the same one arm the hover path takes. The three it
        // replaces each spelled out the two activation shapes by hand — a control FACE
        // dips and commits on the spring-back, a table row commits on the button-down —
        // which is exactly what `ElemKind` says once, per element, for every owned screen.
        if super::bridge::owns_input(&app.pages, app.route) {
            let (cx, cy) = ptr_xy(&app.ev);
            app.inputs.push(super::bridge::click_input(
                cx,
                cy,
                crate::ui::machine::Tick { ms: app.last_input, dt_us: 0 },
            ));
            return;
        }
        // …and the pointer's half of the same rule.  The erased transport geometry is
        // still inert, but the read-out now exposes one real target: choose quality.
        // Once that opens More, the popover owns clicks through the ordinary modal arm
        // below; every other click on the failed frame remains nothing.
        // A FAILED playback owns the frame, except for the `…` recovery picker it opened
        // itself — which is a surface, so `owns_input` above has already claimed its clicks.
        if matches!(app.route, Route::Player)
            && crate::ui::player_hud::transport_hidden(&mut app.player.session)
            && !app.pages.surface_up()
        {
            let (cx, cy) = ptr_xy(&app.ev);
            if crate::ui::player_hud::failure_quality_hit(cx, cy) {
                super::bridge::open_player_overlay(&mut app.player.session, 
                    &mut app.pages,
                    crate::screens::player::overlay::OverlayKind::More { quality: true },
                );
            }
            return;
        }
        if matches!(app.route, Route::Player) {
            // Sample HUD visibility BEFORE re-arming it: a click must only act on
            // transport geometry the user can SEE (the key path's vis gate — a
            // hidden-HUD OK falls through to play/pause). Without this, a click in
            // the invisible timed-out scrub band committed a blind seek.
            let hud_vis = super::bridge::player(&app.pages)
                .is_some_and(|player| player.hud.visible(&mut app.player.session, app.last_input, paused()));
            if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                player.hud.dismissed = false;
            }
            let (cx, cy) = ptr_xy(&app.ev);
            // Which control-row ITEM the click landed on, resolved ONCE: the arm below
            // both guards on it and parks the ring with it, and re-asking would be two
            // derivations of one answer — the thing `ControlSlot` exists to prevent.
            // `None` for the discs, whose own `icon_hit` is consulted further down.
            let ctrl_click = match super::bridge::player_mut(&mut app.pages) {
                Some(player) if hud_vis => fr.ctrl.hit(&mut player.row, cx, cy),
                _ => None,
            };
            // (Four `modal_of` arms stood here — "an open panel owns the click: dismiss it and
            // STOP" — because the transport is partly hidden while a panel is up and its rects
            // must not be consulted. A panel is a surface now and `owns_input` above claims the
            // click before this arm is reached, so the rule is the same and the owner is one.)
            match () {
                // The stand-ins are HUD furniture, so both are gated on the transport
                // actually being on screen — `hud_vis` is sampled before the click
                // re-arms it, exactly like the rects below. One shared dispatch with
                // the key path, from the same resolved slot — and the click PARKS the
                // ring on what it hit first, because Up Next's row holds two items
                // and `activate_ctrl_row` reads the cursor, not the coordinates.
                _ if ctrl_click.is_some() => {
                    if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                        player.hud.nav.focus = 1;
                        player.hud.nav.btn = ctrl_click.unwrap_or(0);
                    }
                    // …then the tvOS press, exactly as the key arm does it:
                    // `activate_player_row` reads `hud.nav`, which the two lines
                    // above have just parked on what was clicked.
                    app.input.press.begin_ctl(app.last_input);
                    app.ok_armed = true;
                }
                _ => {
                    // shared HUD geometry: player_hud owns the button rects + scrub
                    // band — consulted only while that geometry is on screen
                    let icon = if hud_vis {
                        crate::ui::player_hud::icon_hit(fr.ctrl, cx, cy)
                    } else {
                        None
                    };
                    let on_scrub = if hud_vis && dur() > 0 {
                        crate::ui::player_hud::scrub_hit(cx, cy)
                    } else {
                        None
                    };
                    if let Some(idx) = icon {
                        // Park the ring on the disc, then dip it — which panel opens is
                        // `activate_player_row`'s to decide on the spring-back, off the
                        // same `hud.nav.btn` the key path hands it. The panel used to
                        // open here, on the button-DOWN, and the disc's own dip could
                        // never be seen under it.
                        if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                            player.hud.nav.focus = 1;
                            player.hud.nav.btn = idx;
                        }
                        app.input.press.begin_ctl(app.last_input);
                        app.ok_armed = true;
                    } else if let Some(frac) = on_scrub {
                        let mut t = (frac as f64 * dur() as f64) as i64;
                        let cap = dur() - 3 * 1_000_000_000;
                        if cap > 0 && t > cap {
                            t = cap;
                        }
                        if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                            player.scrub.ns = t;
                            player.publish();
                        }
                        app.ptr.drag = true;
                    } else {
                        let np = !paused();
                        if np {
                            if set_transport_paused(&mut app.adapters.player, true) {
                                crate::diag::event(
                                    crate::diag::schema::DiagEvent::FeatureUsed {
                                        feature: crate::diag::schema::Feature::Pause,
                                    },
                                );
                            }
                        } else {
                            set_transport_paused(&mut app.adapters.player, false);
                        }
                    }
                }
            }
            if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                player.hud.extend(app.last_input, HUD_LINGER_MS);
                player.publish();
            }
        } else if chip_clicked(app.route, &app.ev) {
            // The shared bar's profile chip, on whichever of the three screens is up. `key_ok`
            // carries no matching `TopFocus::Chip` arm any more — Home, Library and Search are
            // all owned screens now, so the chip's focus/OK path is the shared engine's, not this
            // ladder's — and this arm itself is close to unreachable in practice: the
            // `owns_input()` early return above already claims every click for a mounted owned
            // page, and Home/Library/Search (the only pages `chip_clicked` recognises) are
            // always mounted once their frame runs. What is left for this to catch is the
            // one-frame window before a page's body mounts.
            chip_activate(app.route, &mut app.pages);
        // `Route::Search` is deliberately absent here (phase 7 Search cutover, mirroring
        // `Route::Profiles`'s own removal): Search is unconditionally an owned screen, so
        // its clicks — including the shared strip's pills — were already taken by
        // `owns_input()`'s click arm above and never reach this chain.
        //
        // (`Route::ItemMenu`'s arm stood here, ABOVE the Home arm below, which is what kept a
        // click off the panel from falling through onto the shelf and launching whatever card it
        // hit — the failure `modal_of` was written for. Phase 10 made the menu a surface, so the
        // container answers that by construction: `owns_input()` consults it first, and the
        // panel's rows are stops in the shared hit map.)
        }
        // `Route::Profiles` and `Route::Login` are deliberately absent here (phase 6):
        // both are owned screens now, so a click on either was already taken by
        // `owns_input()`'s click arm above and never reaches this chain.
    } else if et == SDL_MOUSEBUTTONUP {
        app.last_input = clock::now();
        {
            let (cx, cy) = ptr_xy(&app.ev);
            app.rec.input(super::recorder::enc_pointer("release", cx as i32, cy as i32));
        }
        // a click that armed the tvOS press (a detail card) releases on the button-up,
        // the pointer's twin of the OK key-up: without it the dip would sit there until
        // press.rs's dropped-key-up ceiling fired. A no-op when no press is in flight.
        app.input.press.release(app.last_input);
        // …and the same release for the TREE's press machine, which has no pointer-up of
        // its own: `dispatch`'s ingest reaches `InputMachine::release` from exactly one
        // place, an `Ok` key on the `Up` edge, so that is what a button-up becomes here
        // (`bridge::release_input` carries the reasoning and why it is inert everywhere
        // else). Unconditional on ownership for `press.release`'s reason above — a
        // release with nothing armed is a no-op, and asking `owns_input` would drop the
        // release of a press armed on the frame a surface began to close.
        app.inputs.push(super::bridge::release_input(crate::ui::machine::Tick {
            ms: app.last_input,
            dt_us: 0,
        }));
        if app.ptr.drag {
            app.ptr.drag = false;
            if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                if player.scrub.ns >= 0 {
                    let target = player.scrub.ns;
                    commit_seek(&mut player.scrub, target, &mut app.repause_at);
                }
                player.hud.extend(app.last_input, HUD_LINGER_MS);
                player.publish();
            }
        }
    } else if et == SDL_MOUSEWHEEL {
        app.last_input = clock::now();
        if app.last_input.wrapping_sub(app.ptr.last_wheel) > 250 {
            app.ptr.last_wheel = app.last_input;
            // **The host reads a DIFFERENT offset, and this one is not the LG-fork
            // shift `decode_key` documents — it is macOS `libSDL2` again being
            // sdl2-compat forwarding into SDL3.** `SDL_MouseWheelEvent` on real SDL2
            // (what the television runs) carries a plain `Sint32 y` at +20, which is
            // what production code always read. Measured 2026-09-02 by dumping the
            // polled bytes of an injected -40 tick on this host: `+20` came back 0, and
            // -40.0 arrived instead as the FLOAT `preciseY` field at `+32` — SDL2's own
            // struct gained that field in 2.0.18 for fractional trackpad scroll, and
            // sdl2-compat's round trip apparently only forwards it, leaving the legacy
            // integer field zeroed for both a genuine trackpad tick and anything
            // `SDL_PushEvent`d in this shape (item 13's `wheel:<dy>` FIFO token hit
            // this the first time anything synthesized a wheel event at all — nothing
            // needed one before). `cfg!`, not `#[cfg]`, so both arms keep compiling.
            let dy = if cfg!(feature = "hostsim") {
                rd_f32(&app.ev, 32).round() as i32
            } else {
                rd_i32(&app.ev, 20)
            };
            app.rec.input(super::recorder::enc_pointer("wheel", 0, dy));
            // the wheel scrolls VERTICALLY only, and only on routes with a vertical
            // flow (it used to drive home's focus behind every other screen)
            //
            // Item 13: an owned surface takes the wheel BEFORE the route dispatch below
            // ever sees it — otherwise a wheel tick over Settings/Privacy/Legal fell
            // through to whatever route sat behind it (Home's own hero/grid dive, a
            // Detail scroll, …), which is the same ownership question the key ladder
            // answers for a fresh press, asked here for the wheel instead. It is the
            // same `owns_input` the other three arms ask, so the four cannot drift.
            if dy == 0 {
                // a fractional trackpad tick that rounded to nothing (hostsim), or a
                // `wheel:0` token — not a step in either direction
                return;
            }
            if super::bridge::search_owns_input(&app.pages, app.route) {
                app.inputs.push(crate::ui::machine::InputEvent {
                    at: crate::ui::machine::Tick { ms: app.last_input, dt_us: 0 },
                    source: crate::ui::machine::Source::Sdl,
                    kind: crate::ui::machine::InputKind::Wheel { dy: dy as f32 },
                });
            } else if super::bridge::owns_input(&app.pages, app.route) {
                // A tick becomes the DIRECTION KEY it stands for (`bridge::wheel_input`),
                // rather than a `Wheel` the family's tables would each have to interpret:
                // one tick is one row, which is what `on_updown(±1)` meant. `RepeatGate`
                // is deliberately NOT applied — a wheel gesture's ticks are the user's own
                // cadence and the gate is armed against the remote's 50 ms hardware
                // repeat, so sharing it would let a held key mute a scroll and vice versa.
                app.inputs.extend(super::bridge::wheel_input(
                    dy,
                    crate::ui::machine::Tick { ms: app.last_input, dt_us: 0 },
                ));
            }
            // `Route::Search` is deliberately absent here: `search_owns_input()` above
            // already took every wheel tick on this route and returned.
        }
    } else if et == SDL_TEXTINPUT {
        // The television's own keyboard committing text. Route-UNCONDITIONAL, and
        // `textinput::on_event` queues unconditionally too — it does NOT check
        // whether we asked for the panel, deliberately. This is the raw platform
        // seam: SDL delivered a character because SDL believes text input is on, and
        // discarding it against our own flag would silently eat REAL typing the first
        // time the two disagree (a panel dismissed from outside the app, or any
        // future caller that enables text events another way). Dropping input is the
        // worse failure, so instead both leaks are closed downstream, where they can
        // be closed completely: `textinput::start` clears the queue, so nothing typed
        // before the field opened can arrive in it, and `MAX_PENDING` bounds a queue
        // nobody drains. Gating here would also be a second, weaker copy of a rule
        // that lives in one place — and it would flip a frame away from the field's
        // own edit state, because the route changes at the fade floor.
        if super::bridge::search_owns_input(&app.pages, app.route) {
            let text = crate::textinput::decode(&app.ev);
            ingest_text(app, &text, crate::textinput::available(), crate::ui::machine::Source::Sdl);
        } else {
            crate::textinput::on_event(&app.ev);
        }
    }
}

/// The playback side of an iteration before the tick: the engine pump, the foreground load
/// poll, the reporter, EOS / Up Next, the hub refresh, the held key, the scrubber and the paused
/// seek — everything that reads `fr.now` and no `dt`.
pub(crate) unsafe fn playback_tick(app: &mut App, fr: &mut Frame) {
        if is_started() {
            crate::player::pump(&mut app.player.session, &mut app.adapters.player, fr.now);
        }
        // **The ONE place the Player machine is asked whether the hardware video plane is bound**
        // (spec §9), immediately after the pump that advances the ACB bind transaction and BEFORE
        // the container tree's frame, so the whole iteration reads one answer. Outside the
        // `is_started` guard on purpose: a stop retires the engine and `started` with it, and the
        // FALSE edge is exactly the one nobody would otherwise carry.
        //
        // Everything downstream READS `app.player.video_plane_bound`; nothing recomputes it, and
        // nothing keys the plane's consequences on the route any more.
        if let Some(bound) = crate::player::observe_video_plane(&mut app.player, &mut app.adapters.player) {
            // One edge, three gates. `set_video_plane_bound` has already told the LIVE one
            // (`ui::idle`); these are the two §4.4 machines — the one `App` owns and the
            // dispatcher's, which is what answers `Rig::opaque_route` at step 9 — plus the rig's
            // own copy for the draw-entry call. `PresentEvent::VideoPlane` has no other source.
            app.present.note(crate::ui::present::PresentEvent::VideoPlane(bound));
            app.pages.present.note(crate::ui::present::PresentEvent::VideoPlane(bound));
            app.bridge.publish_video_plane(bound);
        }
        let _ = poll_foreground_load(
            &mut app.player.lifecycle,
            &mut app.player.session,
            &mut PlayerForegroundActuator {
                pa: &mut app.adapters.player,
                repause_at: &mut app.repause_at,
            },
        );
        // **Unconditional, and NOT inside the `is_started` block above.** `player::state()`
        // derives two of its answers outside the pump entirely — `Resolving` while a plan is in
        // flight, and `Error` for a `/decision` refusal, which happens before an engine exists —
        // so gating this on a started engine would silently miss the earliest and most certain
        // failure there is. It observes the value the HUD renders and reports only transitions,
        // so the steady-state cost is one atomic load.
        crate::player::report::tick(&mut app.player.session);
        // end-of-stream: the pipeline drained at the credits → hand off to Up Next when the
        // show has another episode queued, else leave the player (back to the detail page or
        // home, whichever is behind), instead of freezing on the last frame.
        if matches!(app.route, Route::Player) && crate::player::ended() {
            finish_playback(&mut app.player.session, 
                &mut app.adapters.player,
                &mut app.route,
                &mut app.play_from,
                &mut app.refresh_hubs_at,
                &mut app.trail,
                &mut app.pages,
                &mut app.bridge,
            );
            // dev: REPLAY AFTER COMPLETION (#46) — `crate::dev::scenarios::maybe_replay_after_eos`.
            crate::dev::scenarios::maybe_replay_after_eos(app);
        }
        // Up Next countdown elapsed → start the queued episode on its own. Beside the EOS
        // handoff so the whole auto-advance chain reads in one place.
        if matches!(app.route, Route::Player)
            && super::bridge::player(&app.pages).is_some_and(|p| p.up_next.expired(fr.now))
        {
            if !play_up_next(&mut app.player.session, 
                &mut app.adapters.player,
                HUD_LINGER_MS,
                &mut app.route,
                &mut app.play_from,
                &mut app.pages,
                &mut app.bridge,
            ) {
                if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                    player.up_next.cancel(); // nothing queued after all — don't re-fire
                }
            }
        }
        // post-playback home refresh (armed by every exit_player): refetch the hubs so
        // Continue Watching shows the new resume point / next episode; the small delay lets
        // the final timeline PUT land server-side first. The request is worker-only; the
        // landing logs the resulting item count when it actually commits.
        if app.refresh_hubs_at != 0
            && fr.now.wrapping_sub(app.refresh_hubs_at) < 0x8000_0000
            && !matches!(app.route, Route::Player)
        {
            app.refresh_hubs_at = 0;
            crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::RefetchHubs);
            // …and every library's OWN shelves, for the same reason and at the same moment:
            // a finished playback moves Continue Watching and watch state, and a section deck
            // is as stale as the global one (`browse::section_hubs::invalidate_all`).
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::HubsInvalidateAll);
            log("home: hubs refresh queued after playback");
        }
        // (The lost-keyup safety net and the CLIENT-SIDE LONG-PRESS REPEAT stood here — the one
        // hold-to-move path for every discrete focus list, driven by `App::held_key` at 110 ms so
        // the feel was identical everywhere and independent of the remote's hardware repeat delay.
        // Both are gone with phase 10, and it is the same removal twice over: the net only existed
        // to stop that timer running forever on a dropped key-up.
        //
        // Every list it ever served is an owned screen or a surface on the dispatcher now. The
        // Settings family left in 5b, Home and the Library in 8, Search in 7, the player's four
        // panels in 9 — each applying the SAME 110 ms cadence to the hardware's ~50 ms
        // `Edge::Repeat` itself (`screens::registry::PANEL_REPEAT_MS` / `RepeatGate`) — and
        // the item context menu, the last consumer, in 10. With no arm left to fire, `HeldKey::arm`
        // had no caller and `App::held_key`'s `sym`/`since`/`last_rep`/`alive` had no producer;
        // `down_sym`, which is a fact about the PHYSICAL key rather than about this timer, survives
        // as `App::down_sym`.)
        // (The HUD used to be held up from here while a panel was open. The panel that IS open
        // says so itself now, from its own `Tick` — `PlayerOverlayScreen::step` pushes
        // `PlayerReq::ExtendHud`.)
        // scrub: continuous accelerating advance while a key is held (`hold` set by 0x101).
        // The gesture and the HUD's deadline are both the player instance's; one lookup covers
        // the advance, the lost-keyup commit and the tap-release debounce below, and one
        // `publish` at the end mirrors the preview into `TX.scrub_ns` for the draw path.
        let dragging = app.ptr.drag;
        let repause_at = &mut app.repause_at;
        if let Some(player) = super::bridge::player_mut(&mut app.pages) {
            if player.scrub.dir != 0 && player.scrub.hold && player.scrub.ns >= 0 && !dragging {
                let held = fr.now.wrapping_sub(player.scrub.hold_since) as f32 / 1000.0;
                let speed = (SCRUB_BASE + SCRUB_ACCEL * held).min(SCRUB_MAX);
                let mut sdt = fr.now.wrapping_sub(player.scrub.t) as f32 / 1000.0;
                if sdt > 0.1 {
                    sdt = 0.1;
                }
                let was = player.scrub.ns;
                let mut s = was + (player.scrub.dir as f64 * speed as f64 * sdt as f64 * 1e9) as i64;
                let cap = dur() - 3 * 1_000_000_000;
                if s < 0 {
                    s = 0;
                }
                if cap > 0 && s > cap {
                    s = cap;
                }
                player.scrub.ns = s;
                // Real travel is what turns a reveal into a scrub — not the hold edge, which
                // fires a beat earlier with nothing moved yet (`on_auto_repeat`). Once the
                // preview has left the seed the release commits like any other held gesture.
                if s != was {
                    player.scrub.reveal = false;
                }
                player.hud.extend(fr.now, HUD_LINGER_MS);
                player.scrub.t = fr.now;
                // lost-keyup safety: commit if the 0x101 repeats stop without a keyup
                if fr.now.wrapping_sub(player.scrub.alive) > SCRUB_LOST_MS {
                    let target = player.scrub.ns;
                    commit_seek(&mut player.scrub, target, repause_at);
                    player.scrub.disengage();
                }
            }
            // tap release debounce: commit the accumulated jump(s) once no further tap arrives
            if player.scrub.commit_at != 0
                && fr.now.wrapping_sub(player.scrub.commit_at) < 0x8000_0000
            {
                if player.scrub.ns >= 0 {
                    let target = player.scrub.ns;
                    log(&format!("scrub: tap commit {}s", target / 1_000_000_000));
                    commit_seek(&mut player.scrub, target, repause_at);
                } else {
                    player.scrub.ns = -1;
                }
                player.scrub.disengage();
                player.scrub.commit_at = 0;
            }
            player.publish();
        }
        // Focus follows the control row's OCCUPANT, on both edges. Driven by slot identity
        // rather than a "was something shown" bool, because the two edges have different jobs
        // and the previous bool implemented neither of the ones its comment promised.
        if let Some(player) = super::bridge::player_mut(&mut app.pages) {
            // Keyed on the SEGMENT, not the slot, and `last_offer` is only ever advanced to
            // a real offer — never cleared back to None. `active_marker` is gated on `is_playing`,
            // so a momentary drop out of Playing mid-segment reads as "no segment" and flips the
            // row to the discs and back; keyed on the slot that round trip looked like a new
            // offer and re-raised the HUD over an intro the user was simply watching.
            let offer = fr.ctrl.offer();
            let fresh = offer.is_some() && offer != player.hud.last_offer;
            if offer.is_some() {
                player.hud.last_offer = offer;
            }
            if fresh {
                // One line per SEGMENT offered — the on-device suite grades this feature from
                // the event log like everything else, and "the control row offered a skip" had
                // no observable signal at all before it.
                if let Some((kind, start)) = offer {
                    log(&format!("marker offer: {kind:?} at {}s", start / 1000));
                }
                // A segment beginning puts the HUD ON SCREEN and offers the row — the timer,
                // the DISMISSAL and the ring in one act, because raising the timer alone left
                // the tile behind a transport nobody drew. `HudState::raise_for_offer` is where
                // that rule, its resting-position clause and the bug are written down.
                player.hud.raise_for_offer(fr.now, fr.ctrl.primary_btn());
            } else if crate::ui::player_hud::standin_left_the_ring(
                player.hud.was_standin,
                fr.ctrl,
                player.hud.nav.focus == 1,
            ) {
                // The stand-in went away under the focus ring. Without this the row swaps back
                // to the discs with focus still on it and `btn` still 0, so the next OK opened
                // the SUBTITLES menu instead of toggling pause — exactly the bug class HudNav's
                // own doc says it exists to kill. Strictly the EDGE: as a steady state it also
                // fired on a user who walked UP to the discs on purpose, yanking the ring back
                // the same frame and making OK on a disc unreachable by remote.
                player.hud.nav = HudNav::HOME;
            }
            player.hud.was_standin = !fr.ctrl.is_discs();
            player.publish();
        }
        // While the countdown runs, hold the HUD up — a timer nobody can see is a cut to the
        // next episode out of nowhere. `hud.dismissed` has to clear with it, not just the
        // timer: a user who UP-hid the HUD and then touched nothing until the credits still
        // carries the dismissal, which BEATS `extend_hud` inside `hud_visible`, so the
        // countdown would run behind a tile `draw_hud` never draws. Whether they may
        // re-dismiss it is the cancel's business, one line below — a dismissed HUD is focus
        // off the row.
        //
        // …and that cancel is `up_next::countdown_may_run`, the ONE rule, applied here rather
        // than at each key arm because every way of taking hold of the row (arrows, a click,
        // walking away to the tabs, opening a panel) ends up as a cursor position and a route
        // by the time this frame draws. Reading it as a steady state is what makes that true.
        //
        // Outside the `Overlay::None` gate above, deliberately: an overlay is the one way of
        // taking hold of the transport that never moves the ring, and `draw_hud` draws the
        // control row only for the BARE transport — so gated with the edges above, this block
        // simply did not run for a tile that was already counting down when the Info card
        // opened, and it cut to the next episode from behind a panel. The route is the rule's
        // third input rather than a condition here, so there is still exactly one place that
        // decides.
        // `bare` is the rule's third input and is the CONTAINER's answer now: "no panel is
        // up". It used to be `matches!(route, Route::Player { overlay: Overlay::None })`.
        let bare = !app.pages.surface_up();
        if matches!(app.route, Route::Player) {
            let paused_now = paused();
            if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                if player.up_next.armed() {
                    if crate::ui::up_next::countdown_may_run(
                        bare,
                        player.hud.nav.focus == 1,
                        player.hud.nav.btn,
                    ) {
                        player.hud.dismissed = false;
                        player.hud.extend(fr.now, HUD_LINGER_MS);
                    } else {
                        player.up_next.cancel();
                    }
                }
                // when the HUD auto-hides, park focus back on the scrubber so the next reveal
                // is clean
                if !player.hud.visible(&mut app.player.session, fr.now, paused_now) {
                    player.hud.nav = HudNav::HOME;
                }
                player.publish();
            }
        }
        // hide the idle pointer during playback
        if matches!(app.route, Route::Player)
            && !app.ptr.cur_hidden
            && !app.ptr.drag
            && app.ptr.last_motion != 0
            && fr.now.wrapping_sub(app.ptr.last_motion) > 3000
        {
            hide_cursor();
            app.ptr.cur_hidden = true;
        }
        // re-pause after a resume the INSTANT the seek's frame is on screen. `frames()` counts
        // real "frame presented" callbacks (reset on seek), so >= 1 means the target frame is
        // already composited — re-freezing then shows it with the shortest possible play-blip
        // (a paused scrub must briefly Play to decode the frame; buffer-feed has no preroll).
        if resume_pend()
            && matches!(app.route, Route::Player)
            && crate::player::seek_preroll_active()
            && seek_pending() < 0
            && frames() >= 1
            && playpos() + 15 * 1_000_000_000 >= app.repause_at
        {
            crate::player::finish_paused_seek(&mut app.adapters.player);
        }
}

/// Results and requests that land BEFORE nav commit: the OK arm and the press machine, the
/// person / detail open requests, the login and profiles landings (`install_pms`), the glass
/// load, and the nav oscillator.
pub(crate) unsafe fn land_results(app: &mut App, fr: &mut Frame) {
        if app.ok_armed {
            // PRESS-AND-HOLD → the item context menu, on the latch `press::tick` has always set
            // and nothing ever read (`LONG_MS`, `is_long`). It fires while the key is still DOWN,
            // which is what makes the menu feel like a hold rather than a delayed tap; the press
            // is cancelled so the card springs back, and `ok_armed` is dropped so the eventual
            // key-up commits nothing. A SHORT press is untouched on BOTH screens — a Continue
            // Watching tile still resumes on OK, and OK on an episode still still plays it, by
            // design; this is the other half of those interactions. Ordered ahead of the commit
            // arm (and exclusive with it) so the two can never both run.
            //
            // `is_long` leads and short-circuits, deliberately: everything after it OPENS a
            // menu, so evaluating the arms first would put the popover up on the key-DOWN of
            // every tap.
            let held_menu = app.input.press.is_long(fr.now)
                && match app.route {
                    // The detail page has THREE hold surfaces and tries them in turn. Each
                    // declines by section (`focused_season` answers only on the tab strip,
                    // `focused_episode` only on the filmstrip, `focused_related` only on the
                    // Related shelf), so at most one can open and the order between them is not
                    // a precedence — it is just an order.
                    //
                    // The season strip is the newest and the reason is worth keeping: the page
                    // can mark at three grains and only two of them had a door. An episode has
                    // its still's hold and the show has the hero's toggle, so "I have seen
                    // season 3" meant opening eleven menus — a missing control, not a workflow.
                    //
                    // The CAST shelf arms the same press and still falls through to the
                    // ordinary spring-back, deliberately: a headshot is a person, with no
                    // ratingKey and no watch state, so every row this menu builds would be
                    // absent and the panel would open empty.
                    //
                    // `Route::Search` is deliberately absent (phase 7 Search cutover): its own
                    // press-and-hold item menu opener is `SearchReq::ItemMenu`, drained by
                    // `content::search_requests` — an owned screen's press arms and commits
                    // through the tree's own press machine end to end, so it cannot arrive here
                    // at all (`app.ok_armed` is armed only by the legacy `key_ok`/pointer-click
                    // arms below, neither of which reaches Route::Search any more).
                    _ => false,
                };
            if held_menu {
                app.ok_armed = false;
                app.input.press.cancel();
            } else if app.input.press.take_commit(fr.now) {
                app.ok_armed = false;
                // The deferred activation, dispatched by asking the SAME questions the key
                // ladder asked when it armed the press, in the SAME order.
                //
                // **The two family arms that stood at the top of this dispatch are gone** — the
                // consent question's answer pill and the first-run editor's action. Both were
                // here for one reason: the press was the LOOP's, so the loop had to remember, a
                // frame or two later and from nothing but the route, which screen had armed it.
                // The tree owns its own press end to end (`InputMachine::arm` records the owner,
                // and `PressEvent::Commit` is delivered back to exactly that machine), so an
                // owned screen's press cannot arrive here at all — which also retires the
                // ordering hazard the old comment recorded, that consent stood OVER a route with
                // its own arm below and a match on `route` alone would have committed a consent
                // press as a Home activation.
                match app.route {
                    // `Account { over: Home }` and not every `Account`: the popover can stand on
                    // three pages now, and a press armed on a Library card must not commit as a
                    // HOME activation because a panel happened to open over it. (Reaching either
                    // is near-impossible — a nav key cancels the press — but the arm has to say
                    // which page it means.)
                    // Re-ASKED, not remembered, exactly as the Detail arm below does —
                    // and it has to be asked now that the shelves arm this press too. The
                    // grid's answer is `Card` (open the page); a SHELF tile's is
                    // `ShelfCard`, which a section deck turns into a RESUME. Dispatching
                    // both here rather than calling `open_library_card` unconditionally is
                    // what stops a held-then-released press on a Continue Watching tile
                    // opening the detail page the immediate path would have resumed past.
                    // `Route::Profiles | Route::Search` is deliberately absent (phase 6/7): both
                    // are owned screens now, so their own press machine arms and commits this
                    // activation — nothing here ever arms `app.input.press` for either any more
                    // (see the removed key/pointer arms above), so this deferred commit can never
                    // see either route.
                    // the transport's control row (discs or a stand-in)
                    //
                    // The Info card's own action column is the SURFACE's press, so it is asked
                    // first and by identity rather than by a second `Route::Player` arm — the two
                    // used to be two arms of one route, which reads as an ordering and compiles as
                    // an unreachable branch.
                    Route::Player
                        if matches!(
                            super::bridge::player_overlay_kind(&app.pages),
                            Some(crate::screens::player::overlay::OverlayKind::Info)
                        ) =>
                    {
                        commit_info_press(&mut app.player.session, 
                            &mut app.adapters.player,
                            &mut app.route,
                            &mut app.play_from,
                            &mut app.refresh_hubs_at,
                            &mut app.trail,
                            &mut app.pages,
                        )
                    }
                    Route::Player => activate_player_row(&mut app.player.session, 
                        &mut app.adapters.player,
                        fr.ctrl,
                        fr.now,
                        &mut app.route,
                        &mut app.trail,
                        &mut app.play_from,
                        &mut app.refresh_hubs_at,
                        &mut app.pages,
                        &mut app.bridge,
                    ),
                    _ => {}
                }
            } else if !app.input.press.is_active() {
                app.ok_armed = false; // long-press / cancelled — disarm without activating
            }
        }

        // The ONE consumer of a cast-row person request, drained every frame whatever the
        // route. `detail::on_ok`'s cast arm raises it, and it is reached from three places
        // (the immediate OK, the press-commit above, and the `plxnative-detailok`/-detailplay
        // dev triggers) — polling next to each of those left the flag SET on any path that
        // didn't poll, and a set flag then fired on an unrelated OK several screens later.
        // One drain cannot latch. The push is what STACKS the new page: the detail page being
        // left stays on the trail underneath it, which is how person → detail → person → detail
        // comes back through every step instead of falling to Home at the second BACK.
        // Through the page transition, like every other navigation: the push and the route flip
        // both wait for the fade floor, and where the detail page underneath was standing rides
        // the request as `NavReq::spot` (recorded uniformly by `nav_req`, no longer by this arm).
        // The person STORE is deliberately still installed on the press frame by `person::open`
        // — the detail page fading out reads none of it, so nothing blanks, and `enter_node`'s
        // re-open guard then makes the floor's entry a pure route flip.
        // **Phase 6: the sign-in worker no longer writes `auth`'s controller from its own
        // thread.** `login_thread` publishes what it observed as `LoginProgress`, and this is the
        // one place on the main thread that turns each of them into the controller's next state
        // (`auth::apply_progress`) — the same "drain a mailbox, then act on the result" shape
        // `run()`'s own `super::adapters::poster::drain_decoded()` uses for landed images, just
        // addressed (each `LoginProgress` names the flow epoch it came from) rather than global.
        // Unconditional on `app.route`, unlike the phase→route follower two lines down: the
        // worker can report progress while some OTHER screen is showing (a switch started from
        // the Account surface, whose host page is still Home, before the loop has moved to
        // `Route::Profiles`), and a route gate here would leave that progress queued an extra
        // frame — or, worse, forever, if the route never becomes Login/Profiles at all before
        // something else empties `app.route` back to Home. Draining BEFORE the poll below is what
        // makes THIS frame's `auth::phase()`/`take_ready()` see whatever the worker reported this
        // frame rather than lagging it by one iteration.
        for progress in crate::auth::take_progress() {
            // The token is `land_results`'s own proof this really runs on the main thread — the same
            // token every other main-thread-only call in this function already carries. Not a
            // finding of this pass: `auth::apply_progress` grew this parameter as part of a
            // concurrent lane's own work on `auth.rs`, and this is the one call site (in a file
            // that lane does not own) its signature change left needing an update.
            crate::auth::apply_progress(app.adapters.player.mt(), progress);
        }
        // login flow: install resolved creds on the MAIN thread, then follow the flow phase →
        // route (Login while creating/waiting/discovering/error, Profiles while picking/switching).
        if matches!(app.route, Route::Login | Route::Profiles) {
            if let Some(c) = crate::auth::take_ready() {
                // A sign-out followed by a fresh sign-in can replace the session without
                // restarting the process. Re-read only at this one credentials handoff so the
                // old account's in-memory preference cannot leak into the new session.
                let saved = crate::plex::session::peek();
                crate::route::restore_quality(
                    crate::dev::playback_quality_override()
                        .unwrap_or_else(|| saved.playback_quality()),
                );
                install_pms(&c.origin, &c.token, c.tier, c.pin.as_ref());
                // the fourth store an identity change must not survive, beside the
                // `browse`/`pms`/`person` resets `install_pms` performs: a new user must never
                // be able to walk BACK into the previous one's pages. Reset at the CALL SITE
                // because `install_pms` is a closure that cannot also hold `&mut trail`.
                app.trail.reset();
                // …and only NOW can the first-run question be asked: `install_pms` registers
                // the granted roster, which is the stable input to this decision even before
                // asynchronous section discovery lands. It is asked per PROFILE, which is why
                // it sits after the switch rather than after the sign-in.
                // The sign-in's question first, before any per-profile step. On a Plex Home
                // account it was already asked at the picker below and this is a no-op; on a
                // single-user account this is the earliest authorized moment there is.
                maybe_ask_consent(&mut app.pages);
                if crate::screens::onboard::asks() {
                    log("login: server installed — asking which sources feed Home");
                    // no `enter()`: naming the route is what mounts the owned screen (`boot.rs`)
                    app.route = Route::Onboard;
                } else {
                    log("login: server installed — entering Home");
                    app.route = Route::Home;
                }
            } else {
                match crate::auth::phase() {
                    crate::auth::Phase::Profiles | crate::auth::Phase::Switching => {
                        // BEFORE the picker: the account is authorized, so the consent
                        // question is answerable, and the person holding the remote at this
                        // moment is the one who signed the television in. It draws over the
                        // picker's route on its own opaque ground.
                        maybe_ask_consent(&mut app.pages);
                        // No `enter()`-on-change guard any more (phase 6): the picker is an
                        // owned screen, so a route that is ALREADY `Profiles` mints nothing
                        // (`bridge::frame`'s `Some(_) => {}` arm) and the existing instance's
                        // state — the roster cursor, an open PIN pad — rides across this
                        // assignment untouched, exactly as it did behind the old guard; a route
                        // that is NOT yet `Profiles` gets a fresh `ProfilesScreen` the moment the
                        // tree follows it, which is the whole of what `ui::profiles::enter()`
                        // used to reset by hand.
                        app.route = Route::Profiles;
                    }
                    _ => {
                        app.route = Route::Login;
                    }
                }
            }
        }
        // dev: an `acct` step on the LOAD DIAL asks for the REAL Account popover, so the
        // shipped surface and a synthetic one can be interleaved inside ONE launch. Assigning
        // the route directly (rather than through `nav_to`) is deliberate: the question is what
        // the PANEL costs, and a page transition on the step boundary would put a cross-fade in
        // the middle of the leg being measured.
        if crate::ui::glassload::armed() {
            let want = app.glass.wants_account();
            if want && app.route == Route::Home {
                super::bridge::open_account_menu(&mut app.pages);
            } else if !want && super::bridge::account_menu_up(&app.pages) {
                super::bridge::dismiss_surfaces(&mut app.pages);
            }
        }
        // dev: /tmp/plxnative-navosc — `crate::dev::scenarios::nav_osc_tick`.
        crate::dev::scenarios::nav_osc_tick(app, fr.now);

        // ---- the page cross-fade's commit frame ------------------------------------------
        // Stepped UNCONDITIONALLY, never per-route: a fader only one screen advances is a fader
        // parked at alpha 0 the moment that screen is not the one mounted. Placed AFTER every
        // route change above (input, the async person request, the login landing) so a
        // superseded request is visible as `route != req.from`, and BEFORE the per-route
        // `update(dt)` below so the incoming screen steps its springs on the same frame it first
        // draws — otherwise its first drawn frame is one update stale.
}

/// NAV COMMIT (spec §3.3 step 7 on today's loop): `nav::tick` and the route flip at the fade
/// floor, with the per-route landing that follows a commit.
pub(crate) unsafe fn nav_commit(app: &mut App, fr: &mut Frame) {
        if crate::ui::nav::tick(fr.dt) {
            // Superseded: something else moved the app while this was fading. Drop the
            // request — the fader still completes, fading the screen the user actually has
            // back in — rather than flipping the screen out from under whatever landed.
            let req = app.nav_pending.take().filter(|r| r.is_current(app.route,
                app.pages.nav.top_page().map(|e| e.id), app.pages.nav.input_owner()));
            // The OUTGOING page's teardown, at the floor: `detail::close` / `person::leave`
            // queued with the request by `nav_back`. Unconditional call, conditional run — see
            // `nav::spend_leave`. It happens BEFORE the entry below for the same reason the old
            // BACK arm ran `leave_page` first: `enter_node`'s re-open guard reads
            // `detail::mounted_rk()`, which this is what clears.
            crate::ui::nav::spend_leave(req.is_some());
            if let Some(req) = req {
                // Where the page being left was standing, onto ITS trail node — before
                // anything is pushed over it, and while it is still the top.
                if let Some(s) = req.spot {
                    app.trail.set_top_spot(s);
                }
                match req.to {
                    Nav::Search => {
                        // Search is a PEER of Home reached from the strip, so arriving RESETS
                        // the trail exactly as arriving at Home does — then stands on it. The
                        // reset is what stops the way in deciding the way out: reach Search
                        // from the Library without it and the trail is `[Home, Library,
                        // Search]`, so BACK off a result eventually lands on the browse grid
                        // for one user and Home for another.
                        //
                        // The PUSH is the half that was missing (`trail::Node::Search`): with
                        // no node of its own, a result opened from here stacked straight onto
                        // Home and BACK threw away the query and every shelf under it.
                        app.trail.reset();
                        // No explicit `resume()` here (phase 7 Search cutover retired the legacy
                        // screen's own `enter`/`resume` pair): `app.route = Route::Search` below
                        // mounts a FRESH `SearchScreen` (`bridge::mount`'s
                        // `AppArg::Legacy(Route::Search)` arm), and that mount's own first
                        // `sync()` reads whatever the store already publishes — the query, the
                        // recent terms, both cursors — exactly as a `resume()` used to reconstruct
                        // by hand. The pill is a way back to a search you already made, and a
                        // fresh profile needs no special case for it, since the store it returns
                        // to is empty until something is typed into it.
                        app.trail.push(Node::Search);
                        app.route = Route::Search;
                    }
                    Nav::Library(kind) => {
                        // every teleport `enter` performs (the store swap, `restore_view`'s
                        // scroll jump, the focus band) happens HERE, at alpha 0, off screen
                        app.bridge.enter_library(kind);
                        // The grid sits directly on Home. `home_activate` truncates on the press
                        // frame for the Home→Library case, but the strip is a row of PEERS and
                        // Search is now one of them that stands on the trail — so arriving from
                        // there would otherwise stack `[Home, Search, Library]` and make BACK
                        // out of a library land on a search nobody was doing. Reset first: it
                        // is idempotent for the press-frame truncation Home already did.
                        app.trail.reset();
                        app.trail.push(Node::Library);
                        app.route = Route::Library;
                    }
                    Nav::Home { focus_pill } => {
                        // keep the pill the user was standing on under focus, and put Home in
                        // the view where the top band's focus is visible. Resolved HERE, at the
                        // fade floor, against the strip as it is now — which is the whole point
                        // of carrying an identity: the pill may have arrived or gone since the
                        // press frame.
                        if let Some(pill) = focus_pill.filter(|pill| crate::ui::widgets::pill_of(*pill).is_some()) {
                            let tab = match pill {
                                Pill::Home => HomeTab::Home, Pill::Search => HomeTab::Search,
                                Pill::Section(crate::browse::SecKind::Movie) => HomeTab::Movies,
                                Pill::Section(crate::browse::SecKind::Show) => HomeTab::Shows,
                            };
                            app.bridge.home_command(HomeCmd::FocusStrip(tab));
                        }
                        // Home IS the root, so ARRIVING there is the trail's reset — which
                        // is also what makes BACK out of the Library correct without the arm
                        // popping anything itself, and cancel-safe: a withdrawn transition
                        // never reaches this frame.
                        app.trail.reset();
                        app.route = Route::Home;
                    }
                    Nav::Open { node, season } => {
                        // The one mount a `Node` cannot express, and the only thing that has to
                        // happen before the shared entry: a SHOW opened on one particular
                        // season. ASYNC since 2026-09-03: this used to call the blocking
                        // `open_rk_season` here, "behind a page already at alpha 0" — which
                        // meant the route dip STOPPED at its floor for the two to five PMS
                        // round trips a show costs (the hero's Info press on a Continue
                        // Watching episode; reported as a freeze with the counter at ~16 fps).
                        // The page now mounts on the row this frame and `detail::pump_pending`
                        // selects the season when the seasons land. `enter_node` then finds
                        // the page mounted and only flips the route.
                        let mut node = node;
                        if let (Node::Detail { spot, .. }, Some(season)) = (&mut node, season) {
                            spot.season = Some(season as i64);
                        }
                        let ret = req.ret.clone().unwrap_or_else(|| app.pages.return_state());
                        app.pages.request_with_return(crate::ui::machine::MachineId::Nav,
                            crate::ui::machine::NavOp::Push(super::AppArg::from_node(&node)), ret);
                        app.bridge.seed_node(&node);
                        enter_node(&node, &mut app.route);
                        // AFTER the entry: the guard inside it asks what is currently loaded,
                        // and the push is what makes this page the one a later BACK leaves.
                        app.trail.push(node);
                    }
                    Nav::Back { .. } => {
                        // The pop, at the floor — with the teardown already spent above, in the
                        // same order the old instant arm ran them. `unwrap_or(Node::Home)` is
                        // the anti-strand floor: it cannot fire (the trail is rooted at Home and
                        // only Home/Library are ever terminal), but if it ever did, BACK must
                        // still go SOMEWHERE.
                        let under = app.pages.nav.tabs.stack.entries.iter().rev().nth(1)
                            .and_then(|e| e.arg.node(&e.ret.memory)).unwrap_or(Node::Home);
                        app.trail.back();
                        app.trail.ensure(&under);
                        app.pages.request_with_return(crate::ui::machine::MachineId::Nav,
                            crate::ui::machine::NavOp::Pop, req.ret.clone().unwrap_or_else(|| app.pages.return_state()));
                        enter_node(&under, &mut app.route);
                    }
                }
            }
        }
}

/// **What an owned screen asked the LOOP to do this frame** (spec §14, `screens::registry`'s
/// `LoopReq`), drained immediately after the dispatcher's frame so the route it may flip is the
/// one the rest of the iteration sees.
///
/// Each variant is a DEBT with a phase number on it: the machine that should own the decision —
/// Session, or Navigation over the app's real stack — is not on the dispatcher yet, so the screen
/// names the outcome and the loop performs it. None of them is a general escape hatch; they are
/// the four things a Settings-family screen can decide that outlive the surface it decided them in.
fn loop_requests(app: &mut App) {
    for req in app.bridge.take_reqs() {
        match req {
            // BACK at a root the platform owns. The FIRST consent stage is the fourth such root
            // and the one the 2026-09-03 rule could not reach until this phase — see the key
            // ladder's dispatcher arm for why a `Popover` could not tell the two BACKs apart.
            crate::screens::registry::LoopReq::BackAtRoot => back_at_root(),
            // Privacy & data → Delete all local data, confirmed. The sweep signs the account out,
            // so the surface goes with the screen under it: there is no host left for its
            // dismissal fade to run over, which is what `settings::hide()` used to say by hand.
            //
            // **Both halves of that old `settings::hide()` matter, and this arm dropped both for
            // a while.** `dismiss_surfaces_now` (not the ordinary, spring-driven
            // `dismiss_surfaces`) is `ModalStack::hide`'s caller — see that function's doc for why
            // a queued fade is actively wrong here: `delete_all_local_data_and_sign_out` above has
            // already flipped `app.route` to `Route::Login`, so a Settings surface that took even
            // one more frame to start fading, let alone the several a spring takes to settle,
            // would composite its cached snapshot of the now-gone Settings/Home page over the
            // freshly-mounted sign-in screen. And the confirmed answer's own decision alert
            // (`screens::consent.rs`'s `DecisionAlert`, nested inside this same surface) DOES
            // still fade on its own spring — legacy's `close_delete_and_menu(true)` never made
            // that one instant either, only the popover behind it — so its `Glass::CACHED` panel
            // is still serving the one snapshot it took of the Settings page for the length of
            // that fade. `blur_invalidate()` is the same fix legacy reached for at the same call
            // site and for the same reason (`consent.rs::close_delete_and_menu`'s comment): it
            // forces a recapture, so the alert's exit shows whatever is actually on screen now —
            // the incoming sign-in page — rather than a ghost of the screen the sweep just erased.
            crate::screens::registry::LoopReq::DeleteAllLocalData => {
                delete_all_local_data_and_sign_out(&mut app.route, &mut app.trail);
                super::bridge::dismiss_surfaces_now(&mut app.pages);
                crate::gfx::blur_invalidate();
            }
            // First-run Favourites finished (Start watching, or the retry that found nothing to
            // pin): Home, with the trail reset so BACK from it is the root press.
            crate::screens::registry::LoopReq::OnboardDone => {
                app.route = enter_home_from_onboard(&mut app.trail);
            }
            // …and its BACK: the picker behind it, re-seeded from the persisted session.
            crate::screens::registry::LoopReq::OnboardBack => {
                app.route = enter_profiles_from_onboard();
            }
            // Phase 6: BACK at the QR sign-in's or the who's-watching picker's own ROOT — the
            // screen has already decided (its own PIN pad closed, or it has none) that nothing of
            // this app is behind the press. `route` is still the loop's, so the log line's one
            // word comes from here rather than being threaded through the request.
            crate::screens::registry::LoopReq::AuthBackAtRoot => {
                login_or_profiles_root_back(app.route);
            }
            // Phase 10, the profile menu's five rows. The surface has already asked the container
            // to dismiss it in the same drain — what is left is the part a screen may not do.
            //
            // No `enter()` on any of the three route flips (phase 6): naming the route is the
            // whole of mounting the owned screen it lands on — see
            // `enter_profiles_from_onboard`'s doc.
            crate::screens::registry::LoopReq::AccountChangeProfile => {
                crate::auth::start_switch(crate::auth::Picker::ChangeProfile);
                app.route = Route::Profiles;
            }
            crate::screens::registry::LoopReq::AccountSignIn => {
                crate::auth::start_login();
                app.route = Route::Login;
            }
            crate::screens::registry::LoopReq::AccountSignOut => {
                crate::auth::sign_out();
                app.route = Route::Login;
            }
            // The host route does NOT move: Settings is a surface presented over the same page,
            // and its Privacy, Legal and Favourite-libraries children are pages of that surface's
            // own inner stack. The account sheet's dismissal and this presentation are two parked
            // ops, so the host fold walks Sheet-Closing `(Live, Cached)` → Opaque-Opening
            // `(Frozen, Cached)` and never returns the page to `HostRender::Live` — the snapshot
            // survives the handover, which is what
            // `account_to_settings_never_unfreezes_the_host` pins.
            crate::screens::registry::LoopReq::AccountSettings => {
                super::bridge::open_settings(&mut app.pages);
            }
            // `ps` is threaded rather than read from a global (lane D's lab gate: `player::diag`
            // has taken a `&PlaybackSession` since phase 9, and `lab::request_upload` with it).
            crate::screens::registry::LoopReq::AccountSendDiagnostics => {
                crate::lab::request_upload("menu", &app.player.session);
            }
        }
    }
}

/// The update phase: per-route screen updates (which run the route-gated store pumps), the
/// dev oscillators, the play landing and the remaining pumps — `tick_drain` on the FRAMEDROP
/// line.
pub(crate) unsafe fn update(app: &mut App, fr: &mut Frame) {

        // `Route::Login`/`Route::Profiles` no longer have a route-gated `update(dt)` call here
        // (phase 6, mirroring `Route::Onboard`'s own removal in 5b): both are owned screens now,
        // and whatever per-frame animation the legacy `ui::login`/`ui::profiles` scenes used to
        // step belongs to their `Screen::step`/`prepare` on a `Tick` the dispatcher already
        // delivers every frame through `bridge::frame` — not a second call from this function.
        // dev: /tmp/plxnative-pickuser=<index> — `crate::dev::scenarios::pickuser_tick`.
        crate::dev::scenarios::pickuser_tick(app);
        // **The bare route, for every screen below** — `page_of` stood on each of these three
        // guards while a popover was a ROUTE that had to be resolved onto the page beneath it.
        // Neither menu is a route since phase 10, so a `Route` names exactly one page. Whether
        // that page also UPDATES is the CONTAINER's question and is asked beside it, once, as
        // `bridge::host_frozen` — the host fold, off the surfaces' own `Style`.

        if !super::bridge::host_frozen(&app.pages)
            && matches!(app.route, Route::Home)
        {
            crate::dev::scenarios::hero_osc_tick(app, fr.now);
            crate::dev::scenarios::home_fold_osc_tick(app, fr.now);
            crate::dev::scenarios::home_osc_tick(app, fr.now);
            // only when home is actually drawn — stepping its 16×24 cell springs during
            // Player/Detail frames was pure waste on the A53 (the ui::press dip/commit is driven
            // route-agnostically right after `dt` above)
            let (_, moving) = crate::ui::idle::scoped_motion(|| {
                app.bridge.update_home_chrome(&mut app.pages, fr.dt);
            });
            fr.underlay_moving |= moving;
        } else if !super::bridge::host_frozen(&app.pages)
            && matches!(app.route, Route::Library)
        {
            crate::dev::scenarios::lib_osc_tick(app, fr.now);
            crate::dev::scenarios::lib_switch_tick(app, fr.now);
            // scoped like Home's above, because this page can be the one UNDER the account
            // popover now and its glass backdrop is refreshed off the underlay's motion
            let (_, moving) = crate::ui::idle::scoped_motion(|| {
                app.bridge.update_home_chrome(&mut app.pages, fr.dt);
            });
            fr.underlay_moving |= moving;
        } else if !super::bridge::host_frozen(&app.pages)
            && matches!(app.route, Route::Search)
        {
            // Search wears the same shared bar as Home/Library (`route_wears_tab_bar`), and
            // `SearchScreen::tick` only steps the screen's OWN rows/springs — it never touches
            // the strip's capsule/scroll/chip springs, which live in `ui::widgets` behind
            // `tab_row_update_with`. Without this arm those springs freeze at whatever the
            // previously-drawn page left them at, and `ChromeSnapshot::members`'s published
            // strip rects (read by pointer hit-testing and focus) go stale. This mirrors the
            // Home/Library arms above rather than adding a fourth call site.
            let (_, moving) = crate::ui::idle::scoped_motion(|| {
                app.bridge.update_home_chrome(&mut app.pages, fr.dt);
            });
            fr.underlay_moving |= moving;
        }
        // dev: /tmp/plxnative-searchosc — `crate::dev::scenarios::search_osc_tick`.
        crate::dev::scenarios::search_osc_tick(app, fr.now);
        // (The television's keyboard used to be dismissed HERE, by an `else` that called
        // `textinput::stop()` on every frame of every other route — because `search::leave`
        // was reached by no route off the screen. It is `forward_leave`'s job now: Search is
        // not a trail page, so every way off it carries its teardown to the fade floor, which
        // is where the panel is meant to come down and is also the half the poll never did —
        // it cleared `textinput`'s own flag and left `search::EDITING` set.)
        // dev: /tmp/plxnative-acctosc — `crate::dev::scenarios::account_osc_tick`.
        crate::dev::scenarios::account_osc_tick(app, fr.now);
        // ---- the Settings family's dev oscillators, on the tree -------------------------
        //
        // All six drive the SAME screens through the same door the remote does: a synthesised
        // `InputEvent` with `Source::Script`, queued on `app.inputs` for the dispatcher's next
        // frame. **Every interval is unchanged**, because these scenes' gates are read against a
        // cadence (`fps:modal-ramp`, `legal-document`, `decision-alert`, `settings-*`): 1500 ms
        // for the modal ramp, 520 ms for the three focus sweeps. Bodies live in
        // `crate::dev::scenarios` now; this frame still runs them at the same phase boundary.
        crate::dev::scenarios::modal_osc_tick(app, fr.now);
        crate::dev::scenarios::legal_doc_tick(app, fr.now, fr.dt);
        crate::dev::scenarios::alert_tick(app, fr.now, fr.dt);
        crate::dev::scenarios::settings_osc_tick(app, fr.now, fr.dt);
        crate::dev::scenarios::consent_osc_tick(app, fr.now, fr.dt);
        crate::dev::scenarios::onboard_osc_tick(app, fr.now, fr.dt);
        // (`legal::update` / `consent::update` / `settings::update` stood here, self-gated on
        // their own `Popover::visible` because none of them was a route. The surface and its
        // pages are TICKED by the dispatcher — `RouteSurface::tick` steps the push spring and
        // then delivers `ScreenEvent::Tick` to every body on its stack — so there is nothing
        // left for this phase to advance.)
        // Self-gated on `Popover::visible`, NOT on the route — the same rule the draw sites
        // below obey, and for the same reason. These two popovers are also ROUTES, so
        // dismissing one flips `route` back to its host page on the press frame while the
        // panel is still fading out over it. `update` is the only place `Popover`'s `closing`
        // flag is ever cleared, so a route term here strands a dismissed panel at full opacity
        // for the rest of the session and every panel opened afterwards stacks on top of it —
        // reported off a television on 2026-09-03. Both modules already return early unless
        // they are `visible()`, so the guard bought nothing and cost the fade.
        crate::dev::scenarios::detail_osc_tick(app, fr.now);
        // (Four per-overlay `update` calls stood here — the track menu's pill slide, the `…`
        // popover's, the Info card's control pops and the Chapters strip's two springs. Each
        // panel is a surface now and the dispatcher ticks it, containers before pages, once per
        // frame: `PlayerOverlayScreen::step`'s `Tick` arm.)
        // Re-samples on its own 2 Hz hold; a no-op when the panel is off.
        app.diagnostics.update(&app.player.session, fr.now);
        // …and the lab upload's toast, which expires on a clock rather than a spring.
        crate::lab::update(fr.now);
        // The Up Next countdown and the control row's focus pop are the PLAYER INSTANCE's and
        // are stepped from its own `Tick` for the reason `TransportRow::step` gives — the row is
        // not drawn on every frame of the route, so a spring advanced in the draw would run at a
        // rate that depended on which panel was open. What the loop still owes the instance is
        // this frame's ONE resolve of the control-row slot (`slot()`'s own doc: `playpos_ns` is
        // written by LG's media thread and `player::pump` runs between the input handlers and
        // the draw, so re-deriving it per call site let a keypress dispatch to a control the
        // same frame then declined to draw).
        if let Some(player) = super::bridge::player_mut(&mut app.pages) {
            player.slot = fr.ctrl;
        }
        // Async play resolve: install the worker's plan and start the engine. Route-
        // unconditional — a landing must never depend on which screen is mounted.
        land_play_then_observe(
            app,
            |app| {
                if let Some(r) = crate::route::pump_play(&mut app.player.session) {
                    crate::ui::idle::invalidate();
                    let resume_prepared = r <= 0
                        || matches!(
                            crate::player::resume_at(&mut app.player.session, r),
                            crate::player::ResumeOutcome::Prepared
                        );
                    if !resume_prepared {
                        if let Some(transaction) = crate::route::pending_route_start() {
                            let _ = crate::route::reject_route_start_preparation(transaction);
                        }
                    }
                    // A live engine with the route anywhere but Player is unrecoverable BY THE USER —
                    // every transport key and the EOS teardown are route-gated — so repair the
                    // invariant here rather than trust that no path can violate it. The one that
                    // could is cancelled above; this is the backstop, and it is the cheaper half.
                    if resume_prepared
                        && crate::player::start_bufferfeed(&mut app.player.session, &mut app.adapters.player)
                        && !matches!(app.route, Route::Player)
                    {
                        log("pump_play: engine started off-route → restoring Route::Player");
                        // The page is being taken off screen by a LANDING, not by a navigation, so no
                        // transition runs and nothing else would spend its teardown. `forward_leave`
                        // and not `leave_of`: the page stays on the trail if it is a trail page, and
                        // this repair must not blank the detail page the player will exit back to.
                        // What it does cover is Search, where the television's keyboard would
                        // otherwise be left up over playback (`textinput`'s trap 3: once the user
                        // closes it themselves, the field can never be typed into again this session).
                        if let Some(f) = forward_leave(app.route) {
                            f();
                        }
                        app.route = Route::Player;
                    }
                }
            },
            // `pump_play` can install a refused `/decision` after the earlier report
            // observation but before this frame draws the Error screen. Observe again at that
            // exact publication boundary; latches make a healthy/no-change frame idempotent.
            |app| crate::player::report::tick(&app.player.session),
        );
        // Async detail load: install the worker's item into CURRENT. Route-unconditional for
        // the same reason as pump_play — play_item_now requests a detail from Home and flips
        // straight to the player, so a Detail-gated pump would never land it.
        if crate::stores::metadata::pump_detail() {
            crate::ui::idle::invalidate(); // a detail landing rewrites the page under us
        }
        // Server-side view-state WRITES (Mark as Watched / Unwatched, Remove from Deck): send
        // the next queued one, land the last one's answer and kick the refresh it owes. Route-
        // unconditional for the same reason as the two pumps around it — the user can walk off
        // Home or off the detail page between pressing and the server answering, and the refresh
        // is owed either way. Invalidates from inside, per landing.
        crate::stores::viewstate::pump();
        crate::stores::person::pump();
        if let Some(keep) = crate::viewstate::take_detail_refresh() {
            refresh_content(app, keep);
        }
        // …and the cross-source resolve it kicked off. Route-unconditional for the same reason,
        // and separate because it lands one round trip per source LATER than the page does —
        // "Also available" appears when the other servers have answered, not when the page
        // mounts. It raises the Metadata store's notice, since a landing that grows
        // the actions row must be drawn without waiting for a keypress.
        crate::stores::metadata::pump_alt_sources();
}

/// The draw phase, entered only on a presenting frame: `clear_opaque_region` at entry, the
/// viewport, glass owners' prepare, the page pass, the surfaces bottom-to-top and the
/// instruments. Returns the viewport for the host-side screenshot that follows the draw.
pub(crate) unsafe fn draw(app: &mut App, fr: &mut Frame) -> (i32, i32, i32, i32) {
            // EXPERIMENT (`/tmp/plxnative-egldamage`), no-op without the trigger. FIRST, before
            // any GL command of this frame: `EGL_KHR_partial_update` only permits a damage
            // region to be declared before rendering begins. See `egl.rs`.
            crate::egl::frame_damage();
            // dev: the backdrop-glass LOAD DIAL and the blurred-transition prototype
            // (`/tmp/plxnative-glassload`, `/tmp/plxnative-navblur`). Both are no-ops when
            // their trigger is absent. HERE and not below the gate, because the dial's cadence
            // is counted in PRESENTS — a loop iteration the gate skipped drew no glass — and
            // because a step rollover invalidates the snapshot, which must precede every glass
            // surface in the frame exactly as `Glass::prepare` does.
            app.glass.prepare_dial(fr.now);
            // The authored canvas, scaled UNIFORMLY into the drawable and centred. The shaders
            // divide every coordinate by `u_screen` (which stays 1920x1080), so this one call
            // is the entire logical->physical mapping — nothing else in the renderer knows the
            // drawable size. At 1:1 on every television seen so far; on any 16:9 surface a
            // plain scale with zero letterbox (1080p->4K is exactly 2x); and on an unexpected
            // aspect, letterboxed rather than stretched or stuffed into a corner. See `surface`.
            let (vx, vy, vw, vh) = crate::surface::viewport();
            glViewport(vx, vy, vw, vh);
            // EVERY screen draws inside ONE panic barrier. `plex_run` is `extern "C"` (main.c calls
            // it), so a panic unwinding out of a screen's draw is UB the toolchain turns into
            // abort() — the app dies and a live Starfish session is torn down mid-Feed(), on a
            // device with no debugger. Guarding HERE, at the route→screen dispatch, is what makes
            // that structural: a screen added later is covered without its author remembering,
            // which is exactly how every module but home.rs ended up unguarded. The barrier wraps
            // the WHOLE dispatch rather than each `::draw()` so draw ORDER and z-stacking are
            // untouched — a panic in the HUD abandons the rest of the frame instead of stacking the
            // overlays onto a half-built one — and so `ui::guard`'s scissor repair runs once, after
            // the last screen that could have left a clip armed. See `ui::guard` for what this does
            // NOT cover (worker-thread panics, aborts, half-mutated state). Everything inside is a
            // read of loop state, so the closure only borrows; nothing is moved out of the loop.
            // An empty selected phase provides the profiler floor for this build; it
            // deliberately issues no GL commands between its two boundaries.
            crate::ui::profile::phase("profile.empty", || {});
            crate::ui::profile::phase("frame.ui", || {
                crate::ui::guard(|| {
                    if fr.player {
                        // `crate::system::clear_opaque_region()` used to be called here. It is
                        // §3.3 step 10's privileged call and belongs at the container library's
                        // OWN draw entry, which `app.pages.draw` below reaches in this same frame:
                        // `Bridge::clear_opaque_region` performs it, keyed on the plane's bit
                        // rather than on this route test. Calling it in both places would send the
                        // same wayland request twice per frame for the whole of a playback.
                        glClearColor(0.0, 0.0, 0.0, 0.0);
                        glClear(GL_COLOR_BUFFER_BIT);
                        // ONE resolve of which surface owns the "pipeline is working" signal,
                        // handed to both the transport and the read-out, so the centred read-out
                        // and the transport's inline spinner can never both light in the same
                        // frame. Resolved HERE (not beside `ctrl` at the top of the iteration)
                        // because `player::pump` republishes the state mid-iteration and this must
                        // be the post-pump value.
                        //
                        // `transport` is the other per-frame fact the instance cannot resolve for
                        // itself: the transport's MIDDLE is hidden behind an open Info card or
                        // Chapters strip, and which panel is up is the container's answer.
                        let busy = crate::ui::player_hud::busy(&app.player.session);
                        let bare_middle = !matches!(
                            super::bridge::player_overlay_kind(&app.pages),
                            Some(crate::screens::player::overlay::OverlayKind::Info)
                                | Some(crate::screens::player::overlay::OverlayKind::Chapters)
                        );
                        // Both subtitle paths lift clear of the transport for the same reason and
                        // by the same test — an open track menu counts, since that is exactly when
                        // the user is reading the bottom of the screen.
                        let lifted = app.pages.surface_up();
                        if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                            player.busy = busy;
                            player.transport = bare_middle;
                            player.lifted = lifted;
                        }
                        // THE PAGE, and its four panels: `Dispatcher::draw(.., true)` runs the page
                        // pass (subtitles, the transport, the read-out — `PlayerScreen::draw`) and
                        // then the surfaces bottom-to-top, which is exactly the order the four
                        // hand-written `*_panel::draw()` calls used to state by hand.
                        //
                        // `popover::host::begin_frame` is deliberately NOT called on this path and
                        // never was: what is behind these panels is a hardware video plane GL
                        // cannot read back, so there is no host snapshot to take
                        // (`RenderStrategy::VideoPlane`).
                        app.pages.draw(&mut app.bridge, true);
                        app.diagnostics.draw();
                    } else {
                        // Open the shared popover frame FIRST: it takes this frame's own-damage
                        // ledger, which every glass owner below then reads through
                        // `Popover::prepare_present`, and decides whether the frozen-host
                        // snapshot still describes the page. Route-agnostic by construction —
                        // see `ui::popover::host::begin_frame`.
                        crate::ui::popover::host::begin_frame(fr.underlay_moving);
                        // Resolve every glass owner BEFORE anything on this route draws — that is
                        // `Glass::prepare`'s contract, and the shared top tab track is an owner on
                        // every route that wears it.
                        if route_wears_tab_bar(app.route) {
                            // Search publishes its own chrome labels through `capture_chrome`
                            // exactly like Home/Library (its `|| search` clause), so every
                            // bar-wearing route resolves the shared tab track's glass the same
                            // way — from the app's OWN captured vocabulary. The retired
                            // `ui::widgets` legacy label cache this used to fall back to for
                            // every other bar-wearing route (Search, plus either MENU over one of
                            // them back when each was a route of its own) is gone with this call.
                            app.bridge.prepare_home_chrome(app.glass.clock());
                        }
                        // The episode tiles' frosted label band — `/tmp/plxnative-tileglass`,
                        // the 2026-09-05 experiment. Self-gated on the trigger, so a default
                        // build resolves nothing here; unlike the two owners either side of it
                        // this one is not routed at all, because the shelves it rides appear on
                        // Home, the Library and Search and the trigger is the whole condition.
                        app.glass.prepare_tile_band();
                        // **Every REFRESHING backdrop on the tree, resolved before anything on
                        // this route draws** — `Dispatcher::prepare_present`, which folds the
                        // caller's belief about the page together with each surface's own appear
                        // state and the shared own-damage ledger and hands each screen one bit.
                        //
                        // The slot is load-bearing at BOTH ends and neither boundary exists inside
                        // the dispatcher's own frame: `popover::host::begin_frame` above latches
                        // the ledger the fold reads, and `gfx::blur_direct_region()` is sampled
                        // BELOW, so an invalidation raised here reaches this frame's own blur
                        // source instead of the next one's.
                        //
                        // This line NAMED a screen until phase 10 (`ui::person_bio::prepare_present`,
                        // and before that with a `matches!(app.route, Route::Person)` around it —
                        // a rule stated in one module and enforced by a route test three modules
                        // away, §14). The block states what it resolves rather than who owns it,
                        // so the next dynamic backdrop joins by being written.
                        app.pages.prepare_present(
                            &mut app.glass,
                            fr.underlay_moving || crate::ui::idle::present_dirty(),
                        );
                        // THE PAGE, named once because it is drawn TWICE: the direct source path
                        // produces a glass surface's backdrop by rendering the page again into a
                        // small FBO, and that has to happen HERE, before the visible pass — the
                        // capture path's hook is inside the glass surface itself, far too late to
                        // run a second scene pass. The visible full-resolution draw is untouched
                        // either way.
                        //
                        // **The route has to be the same on both passes**, and until 2026-08-19 it
                        // was not: only Home was reachable from this arm, and the source pass drew
                        // `home_draw` whatever the route actually was. The Library's and Search's
                        // tab track therefore blurred HOME — a stale hero from a screen the user
                        // had left, brighter than the grey it sat on and carrying that page's
                        // colour. Measured in the simulator on the Library: page ground (44,44,46),
                        // "glass" track (72,77,59). A track whose whole job is to DARKEN was 1.6x
                        // brighter than its own ground and green. That is the artefact the material
                        // was rejected for by eye, and it was this dispatch, not the material.
                        //
                        // **`page_of`, not the bare route** used to matter here for its own
                        // binding — a POPOVER ROUTE named the page it stood on, and the closure's
                        // `else` used to dispatch by hand between `home_draw` and the retired
                        // legacy screen's own `draw()`, so getting the wrong route (Library,
                        // Search or the person page falling through to `home_draw` the moment
                        // they became menu hosts, or the account popover drawing Home over the
                        // Library once its chip became pressable) was a real, once-shipped bug.
                        // Phase 7 (the Search cutover) retired the last per-route arm that needed
                        // its own `page_route` binding, and phase 10 retired `page_of` itself
                        // along with both popover routes: a `Route` names exactly one page now, so
                        // `page_owned` below is already the right answer for Home, Library,
                        // Detail, Person and Search alike, and the closure's `else` is a
                        // route-agnostic fallback for the one-frame window between a loop request
                        // selecting a new page and that page's owned body mounting.
                        //
                        // Does the DISPATCHER draw the top page (Home, Library, Search, Favourites, Login, Profiles today)?
                        // Sampled once, before the closure, because both the visible pass and the
                        // blur source pass below are gated on it — and because `page` borrows
                        // nothing of `app`, which is what keeps this readable.
                        let page_owned = super::bridge::page_owned(&app.pages, app.route);
                        // …and the host fold: an opaque surface whose ground has drawn REPLACES
                        // the page, so the legacy page pass is skipped wholesale. This was two
                        // `host_ground_ready()` reads, one per full-screen popover, which is
                        // exactly the list that could not be extended without editing this line.
                        let host_replaced = super::bridge::host_replaced(&app.pages);
                        // …folded with `page_owned` into the one plan both guards below read, so
                        // the two bits cannot be combined two different ways two screens apart.
                        let plan = super::bridge::page_plan(host_replaced, page_owned);
                        let mut page = || {
                            // A compact modal still exposes most of its host, so unlike
                            // Settings it cannot replace the page with an opaque ground. It
                            // freezes that page into one framebuffer texture instead: the first
                            // frame draws the real tree once, later frames submit one quad while
                            // the popover's own scrim, springs and panel stay live above it.
                            //
                            // **Inside the closure, so BOTH passes get it** — the visible one
                            // and the direct blur-source one below, which re-renders this same
                            // closure into a small FBO. The guard is RAII because `home_draw`
                            // catches panics; see `popover::host::PagePass`.
                            //
                            // This was the profile menu's private `FrameCache` and applied to
                            // exactly one popover. Every popover that asked for it now gets it,
                            // including the four that draw from INSIDE their page and so could
                            // never have used the old shape.
                            let _host = crate::ui::popover::host::page_pass();
                            // `Route::Login`/`Route::Profiles`/`Route::Search` are deliberately
                            // absent (phase 6/7, mirroring `Route::Onboard`'s own absence here
                            // since 5b): all three are owned screens now, so `page_owned` reports
                            // `true` for any of them and this whole closure is skipped in favour
                            // of the dispatcher's own page pass — see the `page_owned`/
                            // `host_replaced` guards below.
                            if page_owned {
                                app.pages.draw(&mut app.bridge, true);
                            } else {
                                // A loop request may have selected Home after this frame's
                                // commit. Its owned body mounts next frame; never draw a retired
                                // singleton while waiting for that lifecycle boundary.
                                crate::gfx::frame_clear(crate::ui::theme::CLEAR_RGB.0, crate::ui::theme::CLEAR_RGB.1, crate::ui::theme::CLEAR_RGB.2);
                            }
                            // **A popover drawn AFTER this closure owes its scrim TO it.** That is
                            // the rule, and these are the two popovers in that class — every other
                            // one is a SURFACE the container draws (the Library's sort menu, *Also available*)
                            // or is player-route, where there is no page closure and the dim
                            // is meant to cover the HUD as well.
                            //
                            // The scrim sits between the page and the popover's glass, so it is
                            // part of what that glass looks through, and this closure is what the
                            // direct source path re-renders. Drawn with the panel instead it
                            // reaches the visible frame but never the snapshot, and the frosted
                            // ground comes out at full page brightness inside a dimmed screen —
                            // which is exactly what the profile menu did.
                            //
                            // **The DIM itself is the container's since phase 10** — one
                            // `ModalStack::draw_scrims` at the end of the dispatcher's own page
                            // pass, off each surface's `Screen::scrim`, which is inside
                            // `app.pages.draw(.., true)` above and therefore still inside this
                            // closure and still before the snapshot. Two hand-written calls stood
                            // here (`item_menu::draw_scrim` and, until item 2, the account menu's)
                            // and each self-gated on its own `is_open`.
                            //
                            // What is left is the LIFT: the focused tile repainted above that dim.
                            // The un-dimmed copy has to be in the SNAPSHOT too, or the panel's
                            // glass frosts a dimmed picture of the very card it is about — and
                            // only the page that drew the element knows where it landed, which is
                            // why this half cannot be a `Scrim::lift`'s bare `fn()` the way the
                            // profile chip's is.
                            app.bridge.redraw_opener(&app.pages);
                            // (`settings`/`legal`/`consent`'s scrims stood here — "a notice is
                            // about the APP, not about anything on the page behind it" — for the
                            // same reason the two above still do: a popover drawn AFTER this
                            // closure owes its scrim TO it, or the frosted ground comes out at
                            // full page brightness inside a dimmed screen. The family draws its
                            // own scrim INSIDE its page pass now (`RouteSurface::draw` opens with
                            // it), which satisfies the rule from the other side: the scrim and
                            // the surface are one draw, so they cannot be separated by a pass.)
                        };
                        if let Some(reg) = crate::gfx::blur_direct_region() {
                            crate::gfx::blur_snapshot_direct(reg, &mut page);
                        }
                        // Settings owns a frozen, already-blurred image of the host. After its
                        // first visible draw, repainting the full Home hero and shelves beneath
                        // an opaque full-screen modal only burns fill-rate on the T820. Closing
                        // Settings clears the flag, so the live page resumes on the next frame.
                        //
                        // The compact modals do NOT take this branch: they expose most of their
                        // host, so the page still has to be on the framebuffer. `page` runs, and
                        // the freeze inside it is what makes running it cheap.
                        //
                        // **A Replaced host is drawn by nobody, owned or legacy.** This read
                        // `!host_replaced || page_owned` through phase 7, when `page_owned`
                        // meant only "the dispatcher draws the page, not this closure" and no
                        // owned page could host an opaque surface. Phase 8 made Home both, and
                        // the closure then ran under the Settings ground every frame — its
                        // `page_pass` serving the frozen snapshot as a full-screen quad beneath
                        // the ground's own full-screen wash, which is the 60 → 40 fps the
                        // Settings family measured in TV session 4. `bridge::page_plan` is the
                        // one fold of the two bits; the surfaces of an owned page under a
                        // Replaced host are drawn by the call below, as a legacy page's are.
                        if plan != super::bridge::PagePlan::SurfacesOnly {
                            crate::ui::profile::phase("main.ui", || page());
                        }
                        // The diagnostics read-out, off the player. It drew ONLY inside the branch
                        // above until 2026-08-29, which is why its module doc had to warn that a
                        // toggle offered anywhere else would tick a box and show nothing — and why
                        // the failure this app is reported for most ("it opens and finds nothing")
                        // could produce no artefact at all: it never reaches a player.
                        //
                        // **Here rather than on the frame's common tail, and the simulator is what
                        // settled it.** On the player path the panel is genuinely last; here it is
                        // over the PAGE and under the app's modal surfaces, because on this path
                        // something DOES sit in its corner — the profile menu, which carries the row
                        // that turns it off. Drawn last it covered the account chip and then the
                        // popover itself, so the switch could only be found by pressing keys at a
                        // menu you cannot see. A control must be visible over the thing it
                        // controls. Two call sites, and the `else` covers every non-player route,
                        // so a new route still cannot be forgotten.
                        app.diagnostics.draw();
                        // **THE TREE, once, at the slot the family always occupied** — over the
                        // two compact popovers, under the dev glass. Five calls stood here:
                        // `settings::draw()`, the Home editor behind its own second push spring,
                        // `legal::draw()` and `consent::draw()` on top, each self-gated and each
                        // ordered by hand to mirror the key ladder ("whichever answers BACK first
                        // must also be the one on top"). One `Dispatcher::draw` states the same
                        // order structurally: the surfaces are drawn bottom to top and the inner
                        // stack's push is the surface's own, so Privacy over the root, or the
                        // second consent stage over the first, is a stack rather than a pair of
                        // springs somebody has to keep in step.
                        //
                        // `pages` is `false` here and it must be: the argument says whether the
                        // PAGE pass runs too, and an owned page's `true` pass is the one INSIDE
                        // the closure above — so this call is skipped on the `Owned` plan, and
                        // runs with the page pass off for a legacy page (whose closure painted
                        // it) and for either kind under a Replaced host (which paints nothing).
                        // `Dispatcher::draw` also fills and SWAPS the hit map, so calling it
                        // twice in a frame — once inside the page closure and once here — would
                        // register the surfaces' stops twice and hand the pointer a map from the
                        // wrong pass.
                        //
                        // What this call draws lands over the diagnostics read-out — the profile
                        // and card menus are surfaces of this very stack since phase 10 and are
                        // drawn BY it — and since phase 8 that is only ever SURFACES: an owned
                        // page (Home, Library and Search included, all of which wear the tab bar
                        // and can host both popovers) is painted inside the closure above, under
                        // them, and reaches this call only under a Replaced host — where the
                        // page paints nothing and the compact popovers cannot be open, because an
                        // opaque ground is up. The ordering question this comment used to settle
                        // by listing the owned pages is therefore the same one the legacy page
                        // answered on every frame before the migration.
                        if plan != super::bridge::PagePlan::Owned { app.pages.draw(&mut app.bridge, false); }
                        // dev: the blurred route transition, then the load dial's glass surfaces.
                        // LAST on the non-player path, so the snapshot either takes is of the
                        // COMPLETE page — which is the honest source for a surface that sits on
                        // top of everything, and the one thing the tab track (drawn inside the
                        // page) cannot have.
                        app.glass.draw_nav_blur();
                        app.glass.draw_dial();
                        // The on-screen counter, off the player route (chrome over video). It draws
                        // the last completed `fps=` window — frames actually swapped, not loop
                        // iterations. It necessarily HOLDS its last painted value on a settled
                        // screen until the ordinary keepalive buys another present; waking the
                        // renderer for the diagnostic would falsify the number it is showing.
                        //
                        // NOT in a release build (`make RELEASE=1` → --no-default-features). This
                        // costs the fps scenes nothing: they grade the once/sec heartbeat in the
                        // EVENT LOG, never the pixels, so `loop_floor`/`fps_floor`/`fps_ceiling`
                        // are unaffected by whether the digits are painted.
                        #[cfg(feature = "devtools")]
                        {
                            let fps_col = if app.buffer_flip_count < 30 {
                                crate::ui::theme::DIAG_FLIP_A
                            } else {
                                crate::ui::theme::DIAG_FLIP_B
                            };
                            crate::gfx::draw_number(
                                app.fps_shown,
                                SCR_W as f32 - 70.0,
                                64.0,
                                46.0,
                                fps_col.as_ptr(),
                            );
                        }
                    }
                    crate::ui::anim::draw_overlay(); // dev diagnostic overlay (all routes)
                                                     // The lab upload read-out, over everything, on every route — including the
                                                     // player, where the two branches above diverge and this one must not.
                    crate::lab::draw(app.diagnostics.frame_if_shown());
                });
            });
    (vx, vy, vw, vh)
}

/// The iteration's report: the route word (on change), the lab route note, the focus probe
/// and the FRAMEDROP line.
pub(crate) unsafe fn report(app: &mut App, fr: &mut Frame) {
        fr.rn = route_word(app.route);
    let rn = fr.rn;
    if crate::text::take_measure_fault() && !app.measure_fault_logged {
        // once per process: the layout that was built on estimates is the thing to go and look at
        app.measure_fault_logged = true;
        log("text: a width was measured with NO FONT loaded — layout is on average-advance estimates");
    }
        // …and the same name as a reportable event, on CHANGE only. Per-frame would be a
        // firehose of one fact; what is worth knowing is which screens get used, which is a
        // transition count. `&'static str` from the table above, so nothing runtime-built can
        // reach the wire — see `diag::schema`.
        if fr.rn != app.last_route_reported {
            app.last_route_reported = fr.rn;
            crate::diag::event(crate::diag::schema::DiagEvent::RouteEntered { screen: fr.rn });
        }
        // The lab envelope's `route` field, from the SAME name the heartbeat and the focus
        // fingerprint print — a snapshot that disagreed with the log about which screen the
        // tester was on would be worse than one that omitted the field. Compiles away in every
        // build that is not a lab build.
        crate::lab::note_route(fr.rn);
        // dev: the FOCUS FINGERPRINT (`/tmp/plxnative-focus`, see `crate::focusprobe`). One
        // ordered line naming everything the key ladder above can move, logged only when it
        // changes, so a (route x key) characterization run can read what a press did out of the
        // diff instead of out of `route=` alone.
        //
        // HERE, at the tail of the iteration, for two reasons. The frame's input has already
        // been handled and the screen already drawn, so what it samples is the state a press
        // MOVED rather than the state it was about to act on; and this point is outside the
        // idle gate's `present` block, so a settled screen — which stops presenting but keeps
        // looping — is still observed. The probe reports nothing to `ui::idle` in return: a
        // frame gate that a diagnostic could hold open would stop being measurable.
        //
        // `rn` is passed rather than re-derived so the fingerprint's `route=` is the same
        // string the heartbeat prints; the `Screen` beside it is what the probe DISPATCHES on,
        // and its match is exhaustive so a new route cannot fingerprint as nothing.
        let probe_screen = |app: &App| match app.route {
                // The escape/retry/restart control is the screen's only focusable element, and
                // it appears and disappears on the `ESCAPE_AFTER_MS`/`QR_ESCAPE_AFTER_MS` clocks
                // with no phase change of its own — `focusprobe::Screen::Login`'s own doc says
                // why `focus_record().is_some()` decodes it exactly, the same one-element read
                // `Route::Profiles`/`Route::Onboard` already do below.
                Route::Login => crate::focusprobe::Screen::Login {
                    has_control: app.pages.focus_record().is_some(),
                },
                // Phase 6: the picker's cursor comes from the focus engine rather than from a
                // module global, exactly as `Route::Onboard` does below — see `Screen::Profiles`'s
                // own doc for the grammar this replaced and why a bare element number is as far as
                // this crate-level module can decode it without naming `screens::profiles`.
                Route::Profiles => crate::focusprobe::Screen::Profiles {
                    elem: app
                        .pages
                        .focus_record()
                        .map_or(-1, |(_, elem, _)| elem as i32),
                },
                // The one OWNED page, so its cursor comes from the focus engine rather than
                // from a module global. A key at or above `registry::BAND` is the action band,
                // not a row — and the row it retired to is deliberately NOT carried: the engine
                // has one cursor and it is on the band, which is the honest reading and the one
                // difference in this line's values across phase 5b (`focusprobe::Screen`).
                Route::Onboard => {
                    let (list, row) = match app.pages.focus_record() {
                        Some((_, elem, _)) if elem < crate::screens::registry::BAND => {
                            (true, elem as i32)
                        }
                        _ => (false, -1),
                    };
                    crate::focusprobe::Screen::Onboard { list, row }
                }
                Route::Home => crate::focusprobe::Screen::Home,
                // (`Route::ItemMenu`'s arm stood here, mapping the popover's `MenuHost` to the
                // probe's own `Host` mirror and recursing into that screen's fields. The menu is a
                // SURFACE since phase 10: the page under it is the top page, which this line
                // already names as `route=`, and the panel's own fields ride on `content` like
                // every other surface's — `bridge::content_probe`.)
                Route::Library => crate::focusprobe::Screen::Library,
                Route::Detail => crate::focusprobe::Screen::Detail,
                Route::Person => crate::focusprobe::Screen::Person,
                Route::Search => crate::focusprobe::Screen::Search,
                // The same words the heartbeat's `overlay=` uses, and — since phase 9 — from the
                // same place: the SURFACE that is up. There is no second table; see
                // `heartbeat_word_tests::focusprobe_player_overlay_delegates_to_the_shared_overlay_word_function`.
                Route::Player => crate::focusprobe::Screen::Player {
                    overlay: overlay_word(&app.pages, app.route).unwrap_or(super::NO_OVERLAY),
                },
        };
        // The probe reads the OWNER, and answers the resting cursor when no player is mounted —
        // which is what every non-player screen used to read off `App`'s second copy.
        let probe_hud = |app: &App| {
            super::bridge::player(&app.pages).map_or(
                crate::focusprobe::Hud {
                    focus: HudNav::HOME.focus,
                    btn: HudNav::HOME.btn,
                    tab: HudNav::HOME.tab,
                    visible: false,
                },
                |player| crate::focusprobe::Hud {
                    focus: player.hud.nav.focus,
                    btn: player.hud.nav.btn,
                    tab: player.hud.nav.tab,
                    visible: player.hud.visible(&app.player.session, app.last_input, paused()),
                },
            )
        };
        if crate::focusprobe::armed() {
            let content = super::bridge::content_probe(&app.pages, &app.bridge);
            crate::focusprobe::sample(&app.player.session, fr.rn, probe_screen(app), probe_hud(app), fr.ctrl, &content);
        }
        // The recorder's frame tail (spec §5.3): on an event frame the logical-state hash —
        // the press machine, the route and overlay words, the focus fingerprint and, since phase
        // 5b, the CONTAINER TREE's own hash, i.e. the state a press MOVED this frame, sampled
        // here for the probe's own reason — then the frame's records flushed. Under a replay the
        // same hash is graded against the recorded one; the run ends when the recording does.
        //
        // The tree term is what makes a press inside the Settings family gradeable at all: none
        // of those screens has a global for the fingerprint to read, so without it every frame of
        // that whole flow hashed the same and a divergence there was structurally invisible.
        if !matches!(app.rec, super::recorder::Recplay::Off) {
            let content = super::bridge::content_probe(&app.pages, &app.bridge);
            let focus = crate::focusprobe::line(&app.player.session, fr.rn, probe_screen(app), probe_hud(app), fr.ctrl, &content);
            // The bare WORD, not the heartbeat's ` overlay=<word>` spelling: the prefix is that
            // line's grammar and has no business in a state hash (phase 10 item 4).
            let ov = overlay_word(&app.pages, app.route).unwrap_or("");
            let tree = app.pages.state_hash();
            let press = &app.input.press;
            let done = app
                .rec
                .end_frame(&|| super::recorder::state_hash(press, rn, ov, &focus, tree));
            if done {
                app.running = false;
            }
        }
        // `coldopen screen=<name> ms=<n> prepared=<bool>` — one line per screen MOUNT, on every
        // build, no trigger (spec §8.4). The dispatcher owns both ends of the measurement (the
        // mount at nav commit, the first prepared+drawn frame); this drains what it closed.
        // Ungated by `fr.present` on purpose: a line closes on a frame that DID draw, so by the
        // time it is here the presenting is already decided and done.
        for line in app.pages.take_cold_open_lines() {
            log(&line);
        }
        // frame-drop detector: attribute slow frames to pump(uploads)/draw/swap(GPU).
        // ONE tail for every route — this used to live only on the non-player path, which left
        // /tmp/plxnative-framedrop dead during playback (the timings were collected, then a
        // `continue` threw them away).
        // `present` gates this too: a frame the idle gate skipped drew nothing, so grading it
        // would drag `worstframe` toward zero and read as a perf WIN. A skipped frame is not a
        // fast frame — it is an absent one, and `fps=` on the heartbeat is where it shows up.
        if fr.present {
            // **The four upload/card counters are drained HERE, on every presented frame** — not
            // inside the closure below, which the instrument calls only after the threshold
            // check. That is what they used to be: a slow frame's `up=`/`px=`/`cards=`/`off=`
            // covered every frame since the PREVIOUS slow one, and read as this frame's cost.
            // Two atomic swaps a frame is what "per frame" costs; the comment here claimed it
            // was already being paid.
            let (uploads, upload_px) = super::adapters::poster::take_upload_stats();
            let (cards, cards_off) = crate::gfx::take_card_stats();
            app.instr.note_frame_counters(crate::diag::heartbeat::FrameCounters {
                uploads,
                upload_px,
                cards,
                cards_off,
            });
            if let Some(line) = app.instr.frame_drop_line(&|| {
                format!(
                    "route={rn} load={} snapt={:.2}",
                    crate::ui::glassload::step_index(),
                    app.bridge.home_snap_target(&app.pages)
                )
            }) {
                log(&line);
            }
        }
}

/// The once/sec heartbeat line and its counters.
pub(crate) unsafe fn heartbeat(app: &mut App, fr: &mut Frame) {
    let rn = fr.rn;
        if loop_tick(&mut app.iters_ct, &mut app.loop_t, &mut app.loop_shown, fr.now) {
            // once/sec render heartbeat — greppable without reading the on-screen counter.
            // The harness parses `loop=(\d+) route=(\w+)(?: overlay=(\w+))?` (tests/run.py), so
            // the player's overlay tag stays right after route= and worstframe= stays LAST.
            //
            // RENAMED 2026-08-01, and the old name was REUSED, so a log predating this reads
            // as the opposite of what it says: the field that used to be `FPS=` is now `loop=`,
            // and `fps=` now means what it always should have — frames actually presented,
            // previously `pres=`. An old `FPS=60` is a LOOP rate and says nothing about frames.
            let ov = overlay_suffix(&app.pages, app.route);
            // `pos=<s>` rides the heartbeat while frames are actually being presented: the
            // same SHARED.playpos_ns the /:/timeline reporter posts, but at 1 Hz instead of
            // that reporter's 10s cadence. tests/run.py grades playback progress from this.
            // The cadence is the point: to OBSERVE a 15s climb through 10s samples you must
            // play ~30s, so the sparse signal was charging every case double its real floor.
            // Gated on is_playing() (not is_started()) — see that fn for the resume trap.
            let pos_ns = playpos(); // one read — the test and the value must agree
                                    // `play=<pm>` — MEDIA time advanced per WALL millisecond since the previous
                                    // heartbeat, in per mille. 1000 is the film running at speed; 670 is it crawling.
                                    //
                                    // **It is the only field on this line that can see a slow film**, and the reason
                                    // is worth carrying. Every buffer signal the adaptive controller reads is a
                                    // RESERVE, and a reserve is media time measured against this same playhead — so
                                    // when the playhead slows, the reserve stops draining, `slope` goes quiet and
                                    // every drain-derived trigger falls silent at exactly the moment the picture is
                                    // worst. `fps=` cannot see it either: that counts OUR GL swaps, and it sits at 60
                                    // through a stream the television is decoding at two thirds speed. `vtick`/`vgap`
                                    // come closest — they are the pipeline's own 5 Hz callback and they do respond to
                                    // gross starvation — but they are a cadence, not a rate, and the healthy reading
                                    // is 5/201 whatever the media clock is doing.
                                    //
                                    // NO magnitude gate, deliberately. A seek reads as a huge or negative value and a
                                    // catch-up leg as something above 1000; both are real observations and both are
                                    // things a reader wants to see. Inventing a "that must be a seek" threshold would
                                    // be a constant nobody can derive, and the analysis side (`tests/run.py`'s
                                    // `playback_rate`) already splits legs on the discontinuity itself.
            let playing = crate::player::is_playing(&app.player.session) && pos_ns > 0;
            let mut pos = String::new();
            if playing {
                pos = format!(" pos={}s", pos_ns / 1_000_000_000);
                if let Some((prev_ns, prev_ticks)) = app.play_prev {
                    let wall_ms = i64::from(fr.now.wrapping_sub(prev_ticks));
                    if wall_ms > 0 {
                        let media_ms = (pos_ns - prev_ns) / 1_000_000;
                        pos.push_str(&format!(" play={}pm", media_ms * 1000 / wall_ms));
                    }
                }
            }
            app.play_prev = if playing { Some((pos_ns, fr.now)) } else { None };
            // `vtick=<n> vgap=<n>ms` — the media pipeline's own `FRAMEREADY` cadence, the only
            // field on this line that comes from the VIDEO plane's side of the house. Every
            // other number here describes the graphics plane: `fps=` counts our GL swaps and
            // `worstframe=` times our own draw, and both sit at a healthy 60 / sub-millisecond
            // through playback that visibly stutters, because the decoded picture is
            // composited by the television on a surface we never touch.
            //
            // **It is not a frame rate** — `player::vplane_take` says why, and the healthy
            // reading is 5 / 201 on every stream. Read it as liveness: `vtick=0` is a pipeline
            // that has stopped, and a steady 5 / 201 under a stutter report is the pipeline
            // saying the fault is not in anything this process can reach. Drained here, so it
            // must be taken every heartbeat while playing or the worst gap accumulates.
            let vp = if crate::player::is_playing(&app.player.session) {
                let (vtick, vgap) = crate::player::vplane_take();
                format!(" vtick={vtick} vgap={vgap}ms")
            } else {
                String::new()
            };
            // `fps=<n>` — frames actually SWAPPED this second, which is what `ui::idle` moves,
            // and the only field here that is a frame rate. `loop=` counts LOOP iterations: it
            // is the app's liveness signal and `pos=` is anchored to it, so it must not read 0
            // on a screen that is merely idle. The pair is the diagnostic — `loop=62 fps=0` is
            // a settled screen doing its job, `loop=0` is an app in trouble, and `fps=0` on its
            // own is not a fault at all. The dev on-screen counter draws this same drained
            // value, cached below; it never reads a second presentation counter.
            let pres = crate::ui::idle::take_presents();
            #[cfg(feature = "devtools")]
            {
                app.fps_shown = pres.min(i32::MAX as u32) as i32;
            }
            // dev: which LOAD-DIAL step these frames belong to, the blur refreshes
            // actually TAKEN in that second, and the cadence in force. Absent unless the
            // dial or the cadence knob is armed, and placed after `fps=` / before
            // `worstframe=` so both harness regexes are untouched. `snap=` is the one
            // thing a cadence claim cannot be trusted without: it is the rate that RAN,
            // not the rate that was requested.
            let ld = if crate::ui::glassload::armed() || app.scenarios.dev.glass_hz_armed {
                format!(
                    " load={} snap={} period={}",
                    crate::ui::glassload::step_index(),
                    crate::gfx::take_blur_snapshots(),
                    crate::ui::widgets::dynamic_period()
                )
            } else {
                String::new()
            };
            // `worstframe=` stays LAST of the graded fields (both harness regexes anchor on it);
            // `worstprep=` follows it, ungated by present. Both are empty unarmed. After them
            // ride the frame plan's four (spec §8.4), which print in every build because none of
            // them costs a measurement — every one is a counter its owner already keeps:
            //
            // * `carried=`/`dropped=` — the dispatcher's queue depth carried into the next frame,
            //   and the deliveries dropped since the last heartbeat. `carried` had only ever
            //   surfaced as the GROWTH-streak warning (`dispatch: carried=N growing`), which fires
            //   at a slope and says nothing about the level; `dropped` had never surfaced at all.
            // * `budget=<admitted>/<refused>[/solo:<class>]` — the frame budget's second, drained
            //   here and nowhere else (`take_frame_stats` accumulates until its reader takes it,
            //   which is why this is the once-a-second call and not a per-frame one).
            // * `evicted_hot=` — glyphs evicted inside their hot window (`text.rs`, phase 11).
            let budget = app.pages.budget.take_frame_stats();
            let (carried, dropped) = app.pages.take_heartbeat_counters();
            let tail = app.instr.heartbeat_tail(
                crate::diag::heartbeat::HeartbeatFields {
                    carried,
                    dropped,
                    admitted: budget.admitted,
                    refused: budget.refused,
                    solo: budget.solo.map(|c| c.name()),
                    evicted_hot: crate::text::take_evicted_hot(),
                },
                app.rec.take_spent_us(),
            );
            log(&format!(
                "loop={} route={rn}{ov}{pos}{vp} fps={pres}{ld}{tail}{SIM_TAG}",
                app.loop_shown
            ));
        }
}

/// Teardown after the loop: abandon the pending report, stop the feed, wait for the stop
/// scrobble, close the capture stream and the poster workers, quit SDL.
pub(crate) unsafe fn shutdown(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
) {
    crate::player::report::abandon_pending(ps);
    if is_started() {
        crate::player::stop_bufferfeed(ps, pa);
    }
    // The stop scrobble is posted off-thread now, and this process is about to die with any
    // worker still running — so THIS is the one place its result has to be waited for, or the
    // resume point the user just earned is silently dropped. Same cost the old inline call
    // paid, except now it is paid once at exit instead of on every BACK out of a movie.
    crate::route::drain_scrobble();
    crate::capture::shutdown();
    crate::app::adapters::poster::shutdown();
    SDL_Quit();
}

/// Re-inject one recorded input (`app::recorder`'s encodings) for the frame being replayed: SDL
/// kinds through the same synthesis the remote FIFO uses, direct tokens through the dispatcher.
/// An unknown kind is logged once per kind rather than silently skipped.
unsafe fn replay_inject(app: &mut App, fr: &mut Frame, v: &serde_json::Value) {
    if super::bridge::search_owns_input(&app.pages, app.route) { drain_sdl(app, fr); }
    let kind = v["kind"].as_str().unwrap_or("");
    let i = |k: &str| v[k].as_i64().unwrap_or(0) as i32;
    let u = |k: &str| v[k].as_u64().unwrap_or(0) as u32;
    match kind {
        "text" => {
            if !super::bridge::search_owns_input(&app.pages, app.route) {
                log("replay: text has no owned Search recipient");
            } else if let Some(events) = super::recorder::dec_text(v) {
                app.inputs.extend(events);
                crate::ui::idle::invalidate();
            } else { log("replay: malformed text input"); }
        }
        "key" => {
            if v["repeat"].as_bool().unwrap_or(false) {
                remote_synth_key_repeat(u("sym"), u("wcode"));
            } else {
                remote_synth_key_edge(u("sym"), u("wcode"), v["down"].as_bool().unwrap_or(true));
            }
        }
        "pointer" => remote_synth_pointer(SDL_MOUSEMOTION, i("x"), i("y")),
        "click" => remote_synth_pointer(SDL_MOUSEBUTTONDOWN, i("x"), i("y")),
        "release" => remote_synth_pointer(SDL_MOUSEBUTTONUP, i("x"), i("y")),
        "wheel" => remote_synth_wheel(i("y")),
        "lifecycle" => remote_synth_lifecycle(u("code")),
        "token" => {
            let _ = ingress_token(app, fr, v["tok"].as_str().unwrap_or(""));
        }
        other => log(&format!("replay: input kind {other:?} is not replayable; skipped")),
    }
}
