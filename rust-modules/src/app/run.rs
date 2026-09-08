//! The frame loop — phase 1b-ii's THIN COORDINATOR (restructure spec §13).
//!
//! `run` is the one `while app.running` loop; each phase of an iteration is a function below,
//! cut VERBATIM out of `plex_run`'s former body at the eight `FRAMEDROP` stamps (spec §8.4) so
//! that a diff of this commit is moves and renames only — `app.<field>` for what used to be a
//! loop-local, `fr.<field>` ([`Frame`]) for the handful of per-iteration values that cross a
//! phase boundary, and `app.dev.<flag>` for the boot-time trigger flags. Nothing is reordered;
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
//! * `bridge::page_owned` — whether the dispatcher draws the top PAGE too, which is the one bit
//!   `Dispatcher::draw`'s single call per frame needs.
//! * `Bridge::take_reqs` — what an owned screen asked this loop to do (`loop_requests`), because
//!   the machines that own sign-in and the app's real stack are not on the dispatcher yet.
use super::*;
use crate::screens::registry::{HomeCmd, HomeTab};

/// The per-iteration values that cross a phase boundary. Reset at the top of every iteration
/// (`Frame::begin`); every field is written by exactly one phase and read by the ones after it.
pub(super) struct Frame {
    /// The HUD's control slot for this frame, sampled once before input is read.
    pub(super) ctrl: crate::ui::player_hud::ControlSlot,
    /// `clock::now()` at the ingest boundary — THE frame time every phase after it uses.
    pub(super) now: u32,
    /// Seconds since the previous frame's `now`, clamped to 50 ms (the animation timestep).
    pub(super) dt: f32,
    /// Something under a modal surface is still moving (its host must not freeze yet).
    pub(super) underlay_moving: bool,
    /// The route is the player: the video plane owns the picture and the present gate is off.
    pub(super) player: bool,
    /// This iteration draws and swaps (the present decision, spec §3.3 step 8).
    pub(super) present: bool,
    /// The heartbeat's route word for this frame (`route_word`).
    pub(super) rn: &'static str,
}

impl Frame {
    fn begin() -> Frame {
        Frame {
            ctrl: crate::ui::player_hud::slot(),
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
pub(super) unsafe fn run(app: &mut App, mt: &crate::task::MainThread) {
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
        let mut fr = Frame::begin();
        let fr = &mut fr;
        app.instr.mark(crate::diag::heartbeat::Phase::Top);
        app.budget.begin_frame(crate::diag::heartbeat::now_us());
        // REPLAY (`plxnative-recplay`): this frame runs on the recorded tick — set BEFORE
        // ingest, whose key arms stamp `last_input` from the clock — and the frame's recorded
        // inputs are re-injected through the same synthesis the remote FIFO uses, so the poll
        // below consumes them exactly as it consumed the originals.
        if let Some(t) = app.rec.replay_tick() {
            clock::set_replay(t.ms);
            for v in app.rec.replay_inputs() {
                replay_inject(&v);
            }
        }
        crate::system::ls2_pump();
        ingest(app, mt, fr);
        app.instr.mark(crate::diag::heartbeat::Phase::Ingest); // ingest

        fr.now = clock::now();
        // dev: /tmp/plxnative-autoplay auto-presses OK once
        //
        // **Never from the sign-in or the picker.** The auth flow hands its credentials to
        // the main thread through `take_ready`, which is polled only on those two routes; a
        // trigger that jumped to the player from the picker left a HALF-seated profile —
        // the worker had re-keyed the registry, but the session was never persisted and
        // `apply_pending` stayed set — so the next boot came up as the previous profile
        // and the offline-pick harness case found no cache record (device, 2026-09-06).
        // Waiting for the handoff costs a headless run the seconds the seating takes.
        if !dev_scripts(app, mt, fr) {
            continue;
        }
        playback_tick(app, mt, fr);
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
        land_results(app, mt, fr);
        app.instr.mark(crate::diag::heartbeat::Phase::Results); // results
        // NAV COMMIT — the route flips here (a transition at its floor, a cut now).
        capture_content_request(app);
        let returning_from_player = app.pages.nav.top_page().is_some_and(|e| matches!(e.arg, super::bridge::AppArg::Legacy(Route::Player { .. }))) && !matches!(app.route, Route::Player { .. });
        nav_commit(app, mt, fr);
        // The container tree follows the committed route (a Replace CUT on a flip) and runs its
        // frame — the owned screens' inputs, ticks, timers and effects. `app/bridge.rs` is the seam.
        let (_word, _report) = super::bridge::frame_with_tap(
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
        if returning_from_player { restore_played_entry(app); }
        content_requests(app, mt, fr);
        advance_content_boot(app, fr);
        loop_requests(app);
        app.instr.mark(crate::diag::heartbeat::Phase::NavCommit); // navcommit
        update(app, mt, fr);
        app.instr.mark(crate::diag::heartbeat::Phase::TickDrain); // tick_drain
        // The poster adapter's frame (spec §3.3 steps 3 and 9, on the legacy loop): a new frame
        // for the slot LRU, every decoded image handed to the render cache, and the cache's
        // upload step under the frame budget's Poster class. Kept before the present decision
        // as `poster_pump(3)` was — a landed texture invalidates, so the frame presents.
        super::adapters::poster::begin_frame();
        super::adapters::poster::drain_decoded();
        {
            let mut ph = crate::ui::machine::PresentHandle::of(&mut app.present);
            super::adapters::poster::prepare(&mut app.budget, &mut ph, crate::diag::heartbeat::now_us);
        }

        fr.player = matches!(app.route, Route::Player { .. });
        // EXPERIMENT (`/tmp/plxnative-opaque`): one `static` read and a return when the trigger
        // is absent. Route-scoped and edge-triggered — see `system.rs`.
        crate::system::opaque_route(fr.player);
        app.instr.mark(crate::diag::heartbeat::Phase::Prepare); // prepare
        // `worstprep=`: the prepare phase is timed on EVERY iteration, presented or not — a
        // settled screen must never run untimed work at the loop rate (spec §8.3).
        app.instr.note_prepare();
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
        // The PLAYER route is deliberately excluded. `system.rs::clear_opaque_region`
        // documents the hardware video plane as *slaved* to this wayland surface, and
        // "we stop presenting while a plane is slaved to it" is a claim about this
        // compositor that reading cannot settle. Home has no video plane active, which is
        // what makes it the safe place to prove the mechanism. Playback also spends ~99% of
        // its time with the HUD auto-hidden, where the frame is already 0 draw calls.
        // `should_present` is on the LEFT so the short-circuit can never skip it: it
        // takes-and-clears the discrete flag, and on the player route (which always presents)
        // a skipped take would leave a stale flag to fire spuriously on the way back out.
        fr.present = crate::ui::idle::should_present(fr.now) || fr.player;
        app.rec.present(fr.present);
        // Hoisted: the frame-drop detector reads these after the gate. Seeded to the pump
        // stamp so a skipped frame reports zero draw/cap/swap rather than a stale delta.
        app.instr.skip_present_phases();
        if fr.present {
            let (_vx, _vy, _vw, _vh) = draw(app, mt, fr);
            app.instr.mark(crate::diag::heartbeat::Phase::Draw); // draw
            // dev capture stream: grab this finished frame before the swap (after the last draw,
            // so the copy's pass-flush is work the swap would submit anyway). One atomic when idle.
            // Deliberately NOT on the player route (the UI plane is transparent over video, so
            // there is nothing to grab) — capture.rs's 5s keepalive resend covers the host's
            // deadness timer while playback is up.
            if !fr.player {
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
        report(app, mt, fr);
        heartbeat(app, mt, fr);
    }
}

/// Ingest: the lab command channel, the remote FIFO and the SDL event queue (spec §3.3 step 2).
/// Every key, pointer, text and lifecycle event the frame acts on enters here.
pub(super) unsafe fn ingest(app: &mut App, mt: &crate::task::MainThread, fr: &mut Frame) {
        // Cloud Test Lab has no SSH/FIFO. Its LAB build long-polls outward, then leaves each
        // command here for the SDL thread so the same dispatcher and event queue remain the
        // only input path. Acknowledge acceptance after dispatch, before polling SDL below.
        for command in crate::lab::take_commands() {
            let ok = dispatch_remote_token(&command.token);
            if ok && token_is_direct(&command.token) {
                app.rec.input(super::recorder::enc_token(&command.token));
            }
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
            if dispatch_remote_token(&tok) && token_is_direct(&tok) {
                app.rec.input(super::recorder::enc_token(&tok));
            }
        }
        while SDL_PollEvent(app.ev.as_mut_ptr() as *mut c_void) != 0 {
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
                    matches!(app.route, Route::Player { .. }) as i32
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
                // predictable nothing on every other route.
                crate::ui::search::leave();
                if matches!(app.route, Route::Player { .. }) && !app.foreground.awaiting_load() {
                    // INTENDED, not published: this snapshot is the only thing the foreground
                    // restore has, and `suspend_bufferfeed` below drops the pending seek target
                    // with the session — so a background that lands while a seek is still
                    // resolving would otherwise save (and restore to) the spot the user just
                    // seeked AWAY from, with nothing left to correct it. See `intended_pos`.
                    let saved_ns = intended_pos();
                    let clock = app.foreground.clock_for_suspend(paused());
                    app.foreground.suspend(saved_ns, clock);
                    app.scrubber.disengage();
                    app.ptr.drag = false;
                    app.held_key.sym = 0; // this async route flip must not leave a held key repeating into Home
                    set_scrub(-1);
                    close_player_overlays();
                    crate::player::suspend_bufferfeed(mt); // preserve the session for a clean fg reload
                                                           // …and drop any play resolve still in flight. `start_playback` flips to
                                                           // Route::Player as soon as a resolve starts, with NO engine behind it, so
                                                           // this arm fires during that whole window — and `suspend_bufferfeed` is a
                                                           // no-op when there is no engine yet. Without this the plan lands later in
                                                           // the route-UNCONDITIONAL `pump_play` arm and starts playback with the UI
                                                           // on Home, where OK/Stop/seek and the EOS teardown are all route-gated:
                                                           // audio and video running that the user cannot pause or end.
                    crate::route::cancel_play();
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
                    app.foreground.awaiting_load() as i32
                ));
                if et == 0x106 {
                    let activation = drive_foreground(
                        &mut app.foreground,
                        ForegroundInput::DidForeground,
                        &mut PlayerForegroundActuator {
                            mt,
                            repause_at: &mut app.repause_at,
                        },
                    );
                    if matches!(activation, ForegroundActivation::Launched) {
                        app.route = Route::Player {
                            overlay: Overlay::None,
                        };
                        set_hud(clock::now() + HUD_LINGER_MS);
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
                        &mut app.held_key,
                        &mut app.scrubber,
                        &mut app.repause_at,
                        &mut app.input.press,
                    );
                    continue;
                }
                // A repeat is only a repeat if we watched the key go down. See
                // `HeldKey::down_sym`: the system keyboard eats key-ups, so the driver stamps
                // 0x100 on presses that are the FIRST of their own gesture, and dropping those
                // loses one press in two.
                if state & 0x100 != 0 && sym == app.held_key.down_sym {
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
                        continue;
                    }
                    on_auto_repeat(
                        sym,
                        isnav,
                        app.route,
                        app.ok_armed,
                        app.hud.nav,
                        &mut app.held_key,
                        &mut app.scrubber,
                        &mut app.input.press,
                    );
                    continue;
                }
                // From here down this IS a fresh press, whatever the driver stamped on it.
                app.last_input = clock::now();
                begin_fresh_press(
                    key,
                    sym,
                    wcode,
                    app.last_input,
                    &mut app.held_key,
                    &mut app.hud,
                    &mut app.ptr,
                    &mut app.ok_armed,
                    &mut app.input.press,
                );

                // LAB BUILDS ONLY, and above every arm below including the modals: the
                // diagnostics trigger. It has to outrank the chain because the screen a tester
                // most needs a snapshot of is the playback failure read-out, whose own arm
                // `continue`s on every key — and because a snapshot changes no app state, so
                // there is nothing for a later arm to have wanted first. Compiles to `false`
                // in every other build (`crate::lab::key_press`).
                if crate::lab::key_press(sym, wcode) {
                    continue;
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
                    continue;
                }
                // `Route::Login | Route::Profiles` is deliberately absent here (phase 6, mirroring
                // `Route::Onboard`'s own removal in 5b): both are OWNED screens now, so a key on
                // either was already taken by `tree_owns_key` above and never reaches this chain.
                if let Route::Account { over } = app.route {
                    key_account(over, sym, wcode, &mut app.route, &mut app.pages);
                    continue;
                }
                if let Route::ItemMenu { over } = app.route {
                    key_item_menu(
                        mt,
                        over,
                        sym,
                        wcode,
                        app.last_input,
                        &mut app.route,
                        &mut app.play_from,
                        &mut app.trail,
                        &mut app.hud.nav,
                        &mut app.nav_pending,
                        &mut app.held_key,
                    );
                    continue;
                }
                // A failure owns the frame, except for the recovery-quality popover it opened
                // itself.  Stale Menu / Info / Chapters panels remain unreachable; More is the
                // one drawn and drivable escape promised by the failure read-out.
                if matches!(app.route, Route::Player { .. })
                    && crate::ui::player_hud::transport_hidden()
                    && !matches!(
                        app.route,
                        Route::Player {
                            overlay: Overlay::More
                        }
                    )
                {
                    key_player_failed(
                        mt,
                        sym,
                        wcode,
                        &mut app.route,
                        &app.play_from,
                        &mut app.refresh_hubs_at,
                        &mut app.trail,
                    );
                    continue;
                }
                // Each of these three ALSO gates on `overlay_swallows_key`: a modal overlay
                // swallows everything except the transport keys (Pause/Play/PlayPause), which
                // fall through — not `continue` here — to the ordinary Key::Pause/Key::Play/
                // Key::PlayPause arms further down this chain, none of which carry an overlay
                // term of their own. That reaches the exact toggle the HUD uses with no
                // overlay open, and leaves `route` (so the open panel) untouched. See
                // `overlay_swallows_key`'s doc comment for why `Overlay::More` keeps the old
                // swallow-everything behaviour.
                if matches!(
                    app.route,
                    Route::Player {
                        overlay: Overlay::Menu
                    }
                ) && overlay_swallows_key(app.route, key)
                {
                    key_track_menu(sym, wcode, app.last_input, &mut app.route, &mut app.held_key);
                    continue;
                }
                if matches!(
                    app.route,
                    Route::Player {
                        overlay: Overlay::More
                    }
                ) {
                    key_more_menu(mt, sym, wcode, app.last_input, &mut app.route, &mut app.held_key);
                    continue;
                }
                if matches!(
                    app.route,
                    Route::Player {
                        overlay: Overlay::Info
                    }
                ) && overlay_swallows_key(app.route, key)
                {
                    key_info_panel(
                        mt,
                        sym,
                        wcode,
                        app.last_input,
                        &mut app.route,
                        &app.play_from,
                        &mut app.refresh_hubs_at,
                        &mut app.trail,
                        &mut app.hud.nav,
                        &mut app.held_key,
                        &mut app.ok_armed,
                        &mut app.input.press,
                    );
                    continue;
                }
                if matches!(
                    app.route,
                    Route::Player {
                        overlay: Overlay::Chapters
                    }
                ) && overlay_swallows_key(app.route, key)
                {
                    key_chapters(
                        mt,
                        key,
                        sym,
                        wcode,
                        app.last_input,
                        &mut app.route,
                        &mut app.hud.nav,
                        &mut app.held_key,
                    );
                    continue;
                }
                if matches!(app.route, Route::Player { .. }) && matches!(key, Key::Up | Key::Down) {
                    key_player_updown(key, app.last_input, &mut app.hud, &mut app.scrubber);
                    continue;
                }
                // Search's field takes the press first — but this arm has no body to name,
                // because `search::key` IS the body: it handles the key and returns whether it
                // did. So the route test is a guard around CALLING it, not a term to be `&&`ed
                // with it, and it is written as the nested `if` it always meant. Off Search the
                // call must not happen at all; on Search, a key it declines falls through to
                // the chain below exactly as it did.
                if matches!(app.route, Route::Search) {
                    if crate::ui::search::key(sym) {
                        continue;
                    }
                }
                // ---- and the arms on key IDENTITY, which the routes above have already had
                // their pick of. Still one `else if` chain, still in this order: four of its
                // nine tests carry a route term as well as a key one (this first arm, Stop, the
                // player's LEFT/RIGHT and the Library pager), so the order is behaviour too.
                //
                // The plain syms only (`alt: false`) — the alternate D-pad codes reach no arm
                // that navigates a non-player screen. See `Key::Left`, which carries that
                // asymmetry between this test and the player's scrub arm below.
                if !matches!(app.route, Route::Player { .. })
                    && matches!(
                        key,
                        Key::Up
                            | Key::Down
                            | Key::Left { alt: false }
                            | Key::Right { alt: false }
                    )
                {
                    key_move_focus(key, sym, app.route, app.last_input, &mut app.held_key);
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
                    key_ok(
                        mt,
                        app.last_input,
                        &mut app.route,
                        &mut app.hud,
                        &mut app.ptr,
                        &mut app.trail,
                        &mut app.nav_pending,
                        &mut app.play_from,
                        &mut app.ok_armed,
                        &mut app.input.press,
                    );
                } else if matches!(key, Key::Pause) {
                    key_pause(mt, app.route, app.last_input);
                } else if matches!(key, Key::Play) {
                    key_play(
                        mt,
                        app.last_input,
                        &mut app.foreground,
                        &mut app.repause_at,
                        &mut app.route,
                        &mut app.play_from,
                        &mut app.ptr,
                        &app.trail,
                    );
                } else if matches!(key, Key::PlayPause) {
                    // ONE key, both directions. `key_play`/`key_pause` are each half of the
                    // toggle, so this arm picks; off the player route `key_play` is what starts
                    // playback, which is the right answer for a PLAYPAUSE press on a card.
                    if paused() || !matches!(app.route, Route::Player { .. }) {
                        key_play(
                            mt,
                            app.last_input,
                            &mut app.foreground,
                            &mut app.repause_at,
                            &mut app.route,
                            &mut app.play_from,
                            &mut app.ptr,
                        &app.trail,
                        );
                    } else {
                        key_pause(mt, app.route, app.last_input);
                    }
                } else if matches!(key, Key::Exit) {
                    // The remote's EXIT key — LG's checklist item 38 wants the app terminated,
                    // and unlike BACK at Home's root there is nothing ambiguous about a key
                    // labelled EXIT, so unlike BACK it really does end the process — and it
                    // is now the only key that does.
                    log("EXIT key: terminating");
                    app.running = false;
                } else if matches!(app.route, Route::Player { .. }) && matches!(key, Key::Stop) {
                    // Stop — the whole arm is the one ritual, already named.
                    exit_player(mt, &mut app.route, &app.play_from, &mut app.refresh_hubs_at, &mut app.trail);
                } else if matches!(app.route, Route::Player { .. })
                    && matches!(key, Key::Left { .. } | Key::Right { .. })
                {
                    key_scrub(key, app.last_input, fr.ctrl, &mut app.hud, &mut app.ptr, &mut app.scrubber);
                } else if let (Route::Library, Some(dir)) =
                    (app.route, crate::ui::consts::page_dir(sym, wcode))
                {
                    key_library_page(dir);
                } else if matches!(key, Key::Back) {
                    key_back(
                        mt,
                        &mut app.route,
                        &mut app.nav_pending,
                        &mut app.trail,
                        &app.play_from,
                        &mut app.refresh_hubs_at,
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
                if matches!(app.route, Route::Player { .. }) {
                    // Player owns this arm before the generic per-route hover ladder below, so
                    // the overflow popover must be dispatched here.  Otherwise its later
                    // `Overlay::More` arm is unreachable for every playback, including the
                    // terminal recovery picker.
                    if matches!(
                        app.route,
                        Route::Player {
                            overlay: Overlay::More
                        }
                    ) {
                        crate::ui::more_menu::pointer_focus(mx, my);
                        continue;
                    }
                    app.hud.dismissed = false;
                    extend_hud(app.last_input, HUD_LINGER_MS);
                    if app.ptr.drag && dur() > 0 {
                        let frac = crate::ui::player_hud::scrub_frac_x(mx) as f64;
                        set_scrub((frac * dur() as f64) as i64);
                    }
                    continue;
                }
                if app.ptr.dpad_mode {
                    if app.ptr.mot_accum < 120.0 {
                        continue;
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
                    continue;
                }
                // `Route::Profiles` is deliberately absent (phase 6): the picker is an owned
                // screen, so its hover was already taken by `owns_input()` above.
                if matches!(app.route, Route::Account { .. }) {
                    crate::ui::account_menu::pointer_focus(mx, my);
                } else if matches!(app.route, Route::ItemMenu { .. }) {
                    crate::ui::item_menu::pointer_focus(mx, my);
                } else if matches!(app.route, Route::Library) {
                    // the same trade Detail makes below: hover that MOVES the focus stop
                    // aborts a press armed on the stop it left, or the click commits — or the
                    // press-and-hold menu opens — on a tile the user is no longer pressing
                    if crate::ui::library::pointer_focus(mx, my) && app.ok_armed {
                        app.input.press.cancel();
                        app.ok_armed = false;
                    }
                } else if matches!(app.route, Route::Search) {
                    let (mx, my) = ptr_xy(&app.ev);
                    crate::ui::search::pointer_focus(mx, my);
                }
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
                    continue;
                }
                // …and the pointer's half of the same rule.  The erased transport geometry is
                // still inert, but the read-out now exposes one real target: choose quality.
                // Once that opens More, the popover owns clicks through the ordinary modal arm
                // below; every other click on the failed frame remains nothing.
                if matches!(app.route, Route::Player { .. })
                    && crate::ui::player_hud::transport_hidden()
                    && !matches!(
                        app.route,
                        Route::Player {
                            overlay: Overlay::More
                        }
                    )
                {
                    let (cx, cy) = ptr_xy(&app.ev);
                    if crate::ui::player_hud::failure_quality_hit(cx, cy) {
                        crate::ui::more_menu::open_quality();
                        app.route = Route::Player {
                            overlay: Overlay::More,
                        };
                    }
                    continue;
                }
                if matches!(app.route, Route::Player { .. }) {
                    // Sample HUD visibility BEFORE re-arming it: a click must only act on
                    // transport geometry the user can SEE (the key path's vis gate — a
                    // hidden-HUD OK falls through to play/pause). Without this, a click in
                    // the invisible timed-out scrub band committed a blind seek.
                    let hud_vis = hud_visible(app.last_input, hud_until(), paused(), app.hud.dismissed);
                    app.hud.dismissed = false;
                    let (cx, cy) = ptr_xy(&app.ev);
                    // Which control-row ITEM the click landed on, resolved ONCE: the arm below
                    // both guards on it and parks the ring with it, and re-asking would be two
                    // derivations of one answer — the thing `ControlSlot` exists to prevent.
                    // `None` for the discs, whose own `icon_hit` is consulted further down.
                    let ctrl_click = if hud_vis { fr.ctrl.hit(cx, cy) } else { None };
                    // An open panel owns the click: dismiss it and STOP. The transport is
                    // partly hidden while a panel is up (draw_hud gets transport:false), so
                    // its rects must not be consulted — mirrors the modal key arms above.
                    match modal_of(app.route) {
                        Modal::Menu => {
                            crate::ui::track_menu::close();
                            app.route = Route::Player {
                                overlay: Overlay::None,
                            };
                        }
                        Modal::Info => {
                            crate::ui::info_panel::close();
                            app.route = Route::Player {
                                overlay: Overlay::None,
                            };
                        }
                        Modal::Chapters => {
                            crate::ui::chapters_panel::close();
                            app.route = Route::Player {
                                overlay: Overlay::None,
                            };
                        }
                        // Unlike the panels above, this popover's rows are ACTIONS, so a click
                        // that lands on one commits it (and a click outside reports None and
                        // just dismisses) — `account_menu`'s contract, same as its key path.
                        Modal::More => {
                            apply_more_action(mt, crate::ui::more_menu::click(cx, cy));
                            app.route = Route::Player {
                                overlay: Overlay::None,
                            };
                        }
                        // The stand-ins are HUD furniture, so both are gated on the transport
                        // actually being on screen — `hud_vis` is sampled before the click
                        // re-arms it, exactly like the rects below. One shared dispatch with
                        // the key path, from the same resolved slot — and the click PARKS the
                        // ring on what it hit first, because Up Next's row holds two items
                        // and `activate_ctrl_row` reads the cursor, not the coordinates.
                        _ if ctrl_click.is_some() => {
                            app.hud.nav.focus = 1;
                            app.hud.nav.btn = ctrl_click.unwrap_or(0);
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
                                app.hud.nav.focus = 1;
                                app.hud.nav.btn = idx;
                                app.input.press.begin_ctl(app.last_input);
                                app.ok_armed = true;
                            } else if let Some(frac) = on_scrub {
                                let mut t = (frac as f64 * dur() as f64) as i64;
                                let cap = dur() - 3 * 1_000_000_000;
                                if cap > 0 && t > cap {
                                    t = cap;
                                }
                                set_scrub(t);
                                app.ptr.drag = true;
                            } else {
                                let np = !paused();
                                if np {
                                    if set_transport_paused(mt, true) {
                                        crate::diag::event(
                                            crate::diag::schema::DiagEvent::FeatureUsed {
                                                feature: crate::diag::schema::Feature::Pause,
                                            },
                                        );
                                    }
                                } else {
                                    set_transport_paused(mt, false);
                                }
                            }
                        }
                    }
                    extend_hud(app.last_input, HUD_LINGER_MS);
                } else if chip_clicked(app.route, &app.ev) {
                    // the shared bar's profile chip, on whichever of the three screens is up —
                    // the pointer twin of `key_ok`'s own `TopFocus::Chip` arm. It sits ahead of
                    // all three so none of them has to carry a copy of the rule (Home did, and
                    // that is why the other two had a chip nothing could press).
                    chip_activate(&mut app.route);
                } else if matches!(app.route, Route::Search) {
                    let (cx, cy) = ptr_xy(&app.ev);
                    // The strip is shared chrome and is hit-tested here, not by the screen —
                    // `tab_pill_at` owns the clipped rects, so a pill scrolled half out of the
                    // track is clickable across exactly the half you can see.
                    if let Some(i) = crate::ui::widgets::tab_pill_at(cx, cy) {
                        match crate::ui::widgets::pill_at(i) {
                            Pill::Search => {} // the screen we are already on
                            Pill::Section(kind) => {
                                nav_to(app.route, Nav::Library(kind), &mut app.nav_pending)
                            }
                            Pill::Home => nav_to(
                                app.route,
                                Nav::Home {
                                    focus_pill: Some(crate::ui::widgets::Pill::Home),
                                },
                                &mut app.nav_pending,
                            ),
                        }
                    } else if let crate::ui::search::Action::Open(node) =
                        crate::ui::search::click(cx, cy)
                    {
                        nav_open(app.route, node, None, &mut app.nav_pending);
                    }
                } else if matches!(app.route, Route::Library) {
                    let (cx, cy) = ptr_xy(&app.ev);
                    match crate::ui::library::click(cx, cy) {
                        crate::ui::library::Action::GoSearch => {
                            nav_to(app.route, Nav::Search, &mut app.nav_pending);
                        }
                        crate::ui::library::Action::GoHome => {
                            // `library::click` has already parked focus on the Home pill, so
                            // `focused_pill()` is the pill the capsule is under
                            nav_to(
                                app.route,
                                Nav::Home {
                                    focus_pill: crate::ui::library::focused_pill(),
                                },
                                &mut app.nav_pending,
                            )
                        }
                        crate::ui::library::Action::Card => {
                            open_library_card(app.route, &mut app.nav_pending);
                        }
                        // a CLICK on a shelf tile: same rule as the OK press above, and it
                        // reaches the same `activate_card`, so the pointer and the remote
                        // cannot disagree about what a deck tile does
                        crate::ui::library::Action::ShelfCard { from_deck } => {
                            if let Some(mm) = crate::ui::library::focused_item() {
                                // the DECK plays; every other shelf navigates — see `home_activate`
                                let want_play = from_deck;
                                activate_card(
                                    mt,
                                    mm,
                                    want_play,
                                    HUD_LINGER_MS,
                                    &mut app.route,
                                    &mut app.play_from,
                                    &app.trail,
                                    &mut app.hud.nav,
                                    &mut app.nav_pending,
                                );
                            }
                        }
                        crate::ui::library::Action::None => {}
                    }
                } else if let Route::Account { over } = app.route {
                    let (cx, cy) = ptr_xy(&app.ev);
                    // a click on a row commits it; anywhere else dismisses the popover
                    match crate::ui::account_menu::click(cx, cy) {
                        // No `enter()` on any of these three (phase 6): naming the route is the
                        // whole of mounting the owned screen it lands on, exactly as
                        // `key_account`'s twin below no longer calls it.
                        crate::ui::account_menu::Action::ChangeProfile => {
                            crate::auth::start_switch(crate::auth::Picker::ChangeProfile);
                            app.route = Route::Profiles;
                        }
                        crate::ui::account_menu::Action::SignIn => {
                            crate::auth::start_login();
                            app.route = Route::Login;
                        }
                        crate::ui::account_menu::Action::SignOut => {
                            crate::auth::sign_out();
                            app.route = Route::Login;
                        }
                        // the pointer twin of `key_account`'s Settings arm
                        crate::ui::account_menu::Action::Settings => {
                            super::bridge::open_settings(&mut app.pages);
                            app.route = over.route();
                        }
                        // the pointer twin of `key_account`'s arm — lab builds only
                        crate::ui::account_menu::Action::SendDiagnostics => {
                            crate::lab::request_upload("menu");
                            app.route = over.route();
                        }
                        crate::ui::account_menu::Action::None => {
                            // back to the PAGE the popover is on — the pointer's twin of
                            // `key_account`'s BACK arm
                            crate::ui::account_menu::close();
                            app.route = over.route();
                        }
                    }
                } else if let Route::ItemMenu { over } = app.route {
                    let (cx, cy) = ptr_xy(&app.ev);
                    // a click on a row commits it; anywhere else dismisses the popover. THIS arm
                    // existing before the Home arm below is what keeps a click off the panel
                    // from falling through onto the shelf and launching whatever card it hit —
                    // the failure `modal_of` was written for. (`modal_of` itself is only
                    // consulted inside the Player branch, so its ItemMenu case is there for the
                    // same completeness as `Modal::Account`, not because this arm reads it.)
                    let act = crate::ui::item_menu::click(cx, cy);
                    app.route = over.route();
                    apply_item_action(
                        mt,
                        act,
                        over,
                        &mut app.route,
                        &mut app.play_from,
                        &mut app.trail,
                        &mut app.hud.nav,
                        &mut app.nav_pending,
                    );
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
                    if scrub() >= 0 {
                        commit_seek(scrub(), &mut app.repause_at);
                    }
                    extend_hud(app.last_input, HUD_LINGER_MS);
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
                        continue;
                    }
                    if super::bridge::owns_input(&app.pages, app.route) {
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
                    } else if matches!(app.route, Route::Library) {
                        crate::ui::library::wheel(dy);
                    } else if matches!(app.route, Route::Search) {
                        crate::ui::search::wheel(dy as f32);
                    }
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
                crate::textinput::on_event(&app.ev);
            }
        }
}

/// The boot-trigger SCRIPTS (autoplay, grid, settings, press, detail, play, seek, quality, pause,
/// menu, marker) — dev-only schedules keyed on `app.t0`; each fires once and latches. `false`
/// means a refused trigger ended the iteration early (the loop `continue`s, as it always did).
pub(super) unsafe fn dev_scripts(app: &mut App, mt: &crate::task::MainThread, fr: &mut Frame) -> bool {
        if !app.auto_tried
            && !matches!(app.route, Route::Player { .. } | Route::Login | Route::Profiles)
            && fr.now.wrapping_sub(app.t0) > 2000
        {
            app.auto_tried = true;
            // dev: /tmp/plxnative-playurl is the player-PIPELINE tier's entry — a URL and
            // its Load declaration, with no library item behind it — so it shares this
            // autoplay ritual and skips the catalog lookup entirely. It stands on its own
            // (arming it alone enters the player), because the tier's whole premise is a boot
            // with no Plex session at all, which is a boot with no home grid to press OK on.
            let playurl = crate::dev::flag("playurl");
            if crate::dev::flag("autoplay") || playurl {
                // The `||` is load-bearing rather than stylistic: two `else if` arms with
                // identical bodies is `clippy::if_same_then_else`, one of the three named
                // lints `make lint` runs, and warnings are denied.
                let requested = if playurl || crate::dev::flag("h265") {
                    // Leave the URL empty so start_bufferfeed reads the trigger — the H265
                    // probe feeds the local /tmp/sample.h265 through the H265 Load payload,
                    // and playurl feeds the URL its own spec names. Nothing is mounted and
                    // nothing is fetched on either path.
                    crate::route::clear_url();
                    true
                } else {
                    let pidx = crate::dev::read("playidx")
                        .and_then(|s| s.parse::<c_int>().ok())
                        .unwrap_or(0);
                    if let Some(pmm) = usize::try_from(pidx).ok().and_then(|i| crate::pms::hub_item(i / COLS as usize, i % COLS as usize)) {
                        let requested = crate::route::request_play_movie(pmm);
                        if requested {
                            crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::LoadDetailNow { sid: pmm.sid, rk: pmm.rk.to_string() });
                        }
                        requested
                    } else {
                        false
                    }
                };
                if requested {
                    start_playback(
                        mt,
                        0,
                        origin_here(app.route, &app.trail),
                        HUD_HEADLESS_MS,
                        &mut app.route,
                        &mut app.play_from,
                        &mut app.hud.nav,
                    );
                }
            }
        }
        if !app.grid_tried && fr.now.wrapping_sub(app.t0) > 400 {
            app.grid_tried = true;
            // (plxnative-itemmenu rides along: its popover anchors off a GRID card, so the
            // headless entry has to snap into the grid first, exactly like plxnative-grid.)
            if crate::dev::flag("grid") || crate::dev::flag("itemmenu") {
                app.bridge.home_command(HomeCmd::FocusGrid { row: 0, col: 0 });
            }
            // dev: /tmp/plxnative-library[=N] boots straight into the Library browse grid on
            // TAB PILL N (empty file = 0) — the deterministic entry for the library FPS scenes.
            //
            // N names a TYPE, not a section index: 0=Movies, 1=TV Shows. It is resolved
            // against the vocabulary rather than against the DRAWN strip, so a set whose film
            // libraries are all switched off still boots `=0` to Movies and lands on that
            // type's read-out — a trigger that silently meant a different tab depending on the
            // favourite set would make every library scene unreproducible.
            if let Some(s) = crate::dev::read("library") {
                let kind = match s.parse::<usize>().unwrap_or(0) {
                    1 => crate::browse::SecKind::Show,
                    _ => crate::browse::SecKind::Movie,
                };
                // A HARD CUT, deliberately: a transition means "this screen replaced that one",
                // and at boot there is no outgoing screen to replace. The fade-in that IS wanted
                // here belongs to the screen (`enter`'s own `xf().mount()`); dipping the whole
                // page would fade the tab bar up from nothing too, which reads as a slow app
                // rather than a navigated one.
                crate::ui::library::enter(kind, crate::ui::library::Arrival::Cut);
                app.route = Route::Library;
            }
            // dev: /tmp/plxnative-search[=<query>] boots straight into Search, with the field
            // already holding <query>. The seed is the whole point — `sim-shot` and the TV
            // harness both run with no keyboard, so without it every headless look at this
            // screen would be the empty state.
            if let Some(q) = crate::dev::read("search") {
                crate::ui::search::enter(q.trim());
                // …and STAND on it, exactly as the interactive arrival does. Without the push
                // a result opened from a trigger-booted Search stacks straight onto Home and
                // BACK behaves differently from every hand-driven run — which is the one thing
                // a headless entry point must never do, since it is what the harness grades.
                app.trail.push(Node::Search);
                app.route = Route::Search;
            }
            // dev: /tmp/plxnative-heroidx=<n> jumps the rotating hero to pool index n (flip capture)
            if let Some(s) = crate::dev::read("heroidx") {
                if let Ok(n) = s.parse::<c_int>() {
                    app.bridge.home_command(HomeCmd::SelectHero(n));
                }
            }
        }
        // Retry until Home exists: an injected test identity can still spend the first few
        // frames in bootstrap, and a one-shot timestamp would turn a slow sign-in into a
        // misleading "never entered overlay=settings" performance failure.
        if !app.settings_tried && fr.now.wrapping_sub(app.t0) > 800 {
            if matches!(app.route, Route::Home) {
                app.settings_tried = true;
                // The target is the surface's inner ROOT rather than a page pushed onto it —
                // `bridge::AppArg` carries the whole argument, and the short version is that the
                // Settings root's row indices belong to `RootPage` and the loop must not encode
                // them. `home` keeps its spelling: the Home-sources editor was a `Route` and is a
                // page of the family now, so the trigger's word outlived the mechanism.
                let page = match app.dev.settings_boot.as_deref().map(str::trim).unwrap_or("root") {
                    "" | "root" => crate::screens::family::SettingsPage::Root,
                    "home" => crate::screens::family::SettingsPage::Favourites,
                    "privacy" => crate::screens::family::SettingsPage::Privacy,
                    "legal" => crate::screens::family::SettingsPage::Legal,
                    other => {
                        // A value here can only come from a hand-typed or scripted
                        // `/tmp/plxnative-settings` trigger — this whole arm compiles out of a
                        // RELEASE build along with the rest of `devtriggers` — so it is either a
                        // typo an operator will want to see immediately, or a regression that
                        // stopped some legitimate boot-target string from matching the arms
                        // above. Opening Root rather than refusing outright keeps an interactive
                        // session usable (a dead screen or a panic on `/tmp` content this app has
                        // always treated as untrusted input would be strictly worse than a
                        // visibly-wrong-but-working one), but a QUIET fallback here is exactly
                        // what let `fps:settings-home` go silently blind for as long as it did:
                        // Root prints the SAME `overlay=settings` word `settings-root` does (see
                        // `app/mod.rs`'s `overlay_word` and its word-table test), so a scene
                        // landing here by mistake still collected >=5 matching heartbeat samples
                        // and PASSED — measuring the root page while believing it measured
                        // whichever page the broken trigger asked for. `BADTRIGGER` is a marker no
                        // other log call in this crate ever emits (grep the tree before reusing it
                        // elsewhere), chosen so a saved fps log can be found by a human — or by a
                        // future `tests/run.py` check, which does not scan arbitrary log prose for
                        // any scene today and so cannot yet fail loudly on this by itself — even
                        // though the printed `route=`/`overlay=` pair alone would read as a clean
                        // pass.
                        log(&format!("BADTRIGGER settings-boot target {other:?} unknown; opened root instead"));
                        crate::screens::family::SettingsPage::Root
                    }
                };
                super::bridge::open_settings_at(&mut app.pages, page);
            } else if fr.now.wrapping_sub(app.t0) > 12_000 {
                app.settings_tried = true;
                log("settings: boot target timed out before Home became available");
            }
        }
        // dev: /tmp/plxnative-press simulates a real OK TAP on the focused grid card ONCE, so a
        // headless run exercises the whole dip → bounce → deferred-activate path end to end.
        // The release is scheduled explicitly rather than left to the lost-key-up net: that net
        // only fires at `press::MAX_HOLD_MS` (1000 ms), which is PAST `press::LONG_MS`, so a
        // down with no up is a press-and-HOLD — it latches long, never commits, and now opens
        // the item menu instead. A tap has to be a tap.
        if !app.press_tried && fr.now.wrapping_sub(app.t0) > 1600 {
            app.press_tried = true;
            if crate::dev::flag("press")
                && ((matches!(app.route, Route::Home) && app.bridge.home_grid_focused(&app.pages))
                    || (matches!(app.route, Route::Library) && crate::ui::library::focus_is_card()))
            {
                if matches!(app.route, Route::Home) {
                    app.inputs.push(super::bridge::script_key(crate::ui::machine::Key::Ok,
                        crate::ui::machine::Tick { ms: fr.now, dt_us: 0 })[0]);
                } else {
                    app.input.press.begin(fr.now);
                    app.ok_armed = true;
                }
                // past MIN_DIP_MS (the dip must be seen), well short of LONG_MS
                app.press_release_at = fr.now.wrapping_add(150).max(1);
            }
        }
        if app.press_release_at != 0 && fr.now.wrapping_sub(app.press_release_at) < 0x8000_0000 {
            app.press_release_at = 0;
            app.input.press.release(fr.now);
            app.inputs.push(super::bridge::script_key(crate::ui::machine::Key::Ok,
                crate::ui::machine::Tick { ms: fr.now, dt_us: 0 })[1]);
        }
        // dev: /tmp/plxnative-itemmenu opens the press-and-hold card menu on the focused grid
        // card once the snap has settled — the headless entry for the item-menu FPS scene and
        // its capture (the interactive path is a real hold, which no boot trigger can express).
        // Late enough that `focused_card_rect`'s `base_y`/scroll have reached the grid layout,
        // or the panel would anchor off the hero-view position the card no longer occupies.
        // RETRIES until it takes (or gives up at 12s): `open_item_menu` needs a card, so a
        // single attempt at a fixed instant fails outright whenever the hub fetch is slow — and
        // an FPS scene that never opened reads as "the scene never entered this screen", i.e. a
        // flaky FAIL that looks like a regression.
        if !app.itemmenu_tried && fr.now.wrapping_sub(app.t0) > 1800 {
            if crate::dev::flag("itemmenu") && matches!(app.route, Route::Home) {
                app.itemmenu_tried = app.bridge.request_home_menu(&app.pages) || fr.now.wrapping_sub(app.t0) > 12_000;
            } else {
                app.itemmenu_tried = true;
            }
        }
        // dev: /tmp/plxnative-detail=<ratingKey> opens that catalog item's detail page once
        if !app.detail_tried && fr.now.wrapping_sub(app.t0) > 500 {
            app.detail_tried = true;
            if let Some(rk) = crate::dev::read("detail") {
                let rk = rk.as_str();
                if !rk.is_empty() {
                    // in-catalog rk keeps the catalog backdrop; an off-catalog rk still opens the
                    // page (open_rk falls back to the item's own art) so tests can target ANY rk.
                    // A bare trigger keeps its original current-server meaning.  `tv-session
                    // --server N` supplies the other half of the item identity explicitly for
                    // a multi-server boot; an invalid/missing slot fails closed here rather
                    // than opening the same numeric rk on another PMS.
                    let sid = match direct_trigger_server() {
                        Ok(sid) => sid,
                        Err(e) => {
                            log(&format!("plxnative-detail: refused: {e}"));
                            return false;
                        }
                    };
                    // BLOCKING, deliberately: the sub-triggers below replay move_focus/on_ok
                    // in THIS frame, and they walk sections() — which is hero-only until the
                    // item lands. `open_rk_now` resolves the catalog index itself; the old
                    // `open(idx)` arm here was the one caller that made `open` block for
                    // everyone, including Home's OK on a cold card (the "freeze for a second").
                    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::LoadDetailNow { sid, rk: rk.to_string() });
                    log(&format!("plxnative-detail: rk={rk} server={} start", sid.raw()));
                    push_detail(&mut app.trail, &mut app.route, sid, rk);
                    app.bridge.seed_node(app.trail.top());
                    app.content_boot = Some(ContentBoot::new(app.trail.top().clone()));
                }
            }
        }
        // dev: /tmp/plxnative-play=<ratingKey> plays ANY library item (regression harness).
        // Unlike plxnative-detail it does NOT depend on the item being in the home catalog:
        // it fetches the item's metadata fresh and drives the same field-based play
        // path the detail Play button uses (route::play_episode is generic — movie or
        // episode), so tests can target arbitrary rks deterministically.
        // …and never from the sign-in or the picker, for the reason the autoplay trigger above
        // gives: `take_ready` is polled only on those two routes, so a play that left them
        // mid-switch stranded a half-seated profile (registry re-keyed, session never saved).
        // The harness's own `offline_pick_cached` priming launch is what showed it (2026-09-06):
        // the switch line arrived AFTER `plxnative-play: … start`, and the next boot came up
        // as the previous profile.
        if !app.play_tried
            && !matches!(app.route, Route::Player { .. } | Route::Login | Route::Profiles)
            && fr.now.wrapping_sub(app.t0) > 500
        {
            app.play_tried = true;
            if let Some(rk) = crate::dev::read("play") {
                let rk = rk.as_str();
                if !rk.is_empty() {
                    // BLOCKING on purpose: the leaf extraction below reads current() on the
                    // next statement, and this block sits behind a one-shot `play_tried`
                    // latch, so a deferred landing would have nothing left to consume it —
                    // every case in tests/manifest.json drives through here.
                    let sid = match direct_trigger_server() {
                        Ok(sid) => sid,
                        Err(e) => {
                            log(&format!("plxnative-play: refused: {e}"));
                            return false;
                        }
                    };
                    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::LoadDetailNow { sid: sid, rk: rk.to_string() }); // fetch ANY rk (movie/show/episode)
                                                               // a movie/episode leaf carries its own part+codecs; a show has an
                                                               // empty part, so fall back to its first episode.
                    let leaf = crate::metadata::current().map(|d| {
                        if !d.part.is_empty() {
                            (
                                d.part.clone(),
                                d.vcodec.clone(),
                                d.acodec.clone(),
                                d.title.clone(),
                                d.resume_ms,
                                d.dur_ms,
                            )
                        } else if let Some(ep) = d.episodes.first() {
                            (
                                ep.part.clone(),
                                ep.vcodec.clone(),
                                ep.acodec.clone(),
                                d.title.clone(),
                                ep.resume_ms,
                                ep.dur_ms,
                            )
                        } else {
                            (
                                String::new(),
                                String::new(),
                                String::new(),
                                d.title.clone(),
                                0,
                                0,
                            )
                        }
                    });
                    if let Some((part, vc, ac, title, resume_ms, dur_ms)) = leaf {
                        if !part.is_empty() {
                            log(&format!(
                                "plxnative-play: rk={rk} server={} start",
                                sid.raw()
                            ));
                            if crate::route::request_play(sid, rk, &part, &vc, &ac, &title, "")
                            {
                                let resume = crate::metadata::resume_ns(resume_ms, dur_ms);
                                start_playback(
                                    mt,
                                    resume,
                                    origin_here(app.route, &app.trail),
                                    HUD_HEADLESS_MS,
                                    &mut app.route,
                                    &mut app.play_from,
                                    &mut app.hud.nav,
                                );
                            }
                        }
                    }
                }
            }
        }
        // resume is armed BEFORE start_bufferfeed (crate::player::arm_seek) so the very
        // first Load opens at the viewOffset — no play-from-start flash, no post-frames seek.
        // dev: /tmp/plxnative-autoseek — headless seek driver. An EMPTY file fires one seek
        // to 140s (the classic trigger). Otherwise the file is a seek SCRIPT: an optional
        // first token `gap=<ms>` (default 300 — a rapid-tap cadence), then comma-separated
        // steps fired one per gap: absolute seconds ("120") or tap-relative "+10"/"-10"
        // (relative to the previously REQUESTED target, like a user rapid-tapping LEFT/RIGHT
        // while the prior seek is still resolving — exercises the pump's seek coalescing).
        if !app.seek_tried
            && matches!(app.route, Route::Player { .. })
            && dur() > 0
            && fr.now.wrapping_sub(app.t0) > 12000
        {
            app.seek_tried = true;
            if let Some(s) = crate::dev::read("autoseek") {
                let mut steps: Vec<String> = s
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
                // `gap=` is the cadence BETWEEN steps; `delay=` is how long to wait before the
                // FIRST one. They are two different quantities and conflating them costs a
                // whole class of case: an ABR transaction has to COMMIT before a seek can
                // exercise what happens either side of one, and a commit needs tens of seconds
                // of samples, while the first step otherwise fires at the fixed ~12 s above.
                // Expressing that with `gap=` alone forces a throwaway first seek to soak up
                // the wait — which puts a seek the case did not ask for into the log it grades.
                // Either order, so a script never has to remember which came first.
                let mut first_delay_ms = 0u32;
                loop {
                    let Some(head) = steps.first().cloned() else {
                        break;
                    };
                    if let Some(g) = head.strip_prefix("gap=") {
                        app.seek_gap_ms = g.parse().unwrap_or(300).max(50);
                    } else if let Some(d) = head.strip_prefix("delay=") {
                        first_delay_ms = d.parse().unwrap_or(0);
                    } else {
                        break;
                    }
                    steps.remove(0);
                }
                if steps.is_empty() {
                    steps.push("140".to_string());
                }
                app.seek_script_last = crate::player::playpos_ns();
                // The fire test is `now - seek_script_at >= seek_gap_ms`, so backing the origin
                // off by one gap fires the first step at once and adding the delay pushes it
                // out by exactly that much. `delay=0` is the historical behaviour, unchanged.
                app.seek_script_at = fr.now.wrapping_sub(app.seek_gap_ms).wrapping_add(first_delay_ms);
                app.seek_script = steps;
            }
        }
        if !app.seek_script.is_empty()
            && matches!(app.route, Route::Player { .. })
            && script_step_due(fr.now, app.seek_script_at, app.seek_gap_ms)
        {
            let step = app.seek_script.remove(0);
            app.seek_script_at = fr.now;
            let t = if let Some(r) = step.strip_prefix('+') {
                app.seek_script_last + r.parse::<i64>().unwrap_or(0) * 1_000_000_000
            } else if let Some(r) = step.strip_prefix('-') {
                app.seek_script_last - r.parse::<i64>().unwrap_or(0) * 1_000_000_000
            } else {
                step.parse::<i64>().unwrap_or(140) * 1_000_000_000
            }
            .max(0);
            app.seek_script_last = t;
            log(&format!(
                "autoseek: step → {}s ({} left)",
                t / 1_000_000_000,
                app.seek_script.len()
            ));
            request_seek(t);
        }
        // dev: /tmp/plxnative-qualityswitch — change the playback quality WHILE IT PLAYS,
        // which is what a person does at the television and what no boot override can reach:
        // it re-asks the routing question against a stream already on screen, reloads if the
        // answer moved, and on the way out of Auto tears down a running ABR controller.
        // Armed on the same gate as the seek script, so the two are comparable and neither
        // fires into a session that has not settled.
        if !app.quality_tried {
            // Explicit test SLO: observe twelve uninterrupted seconds of PLAYING before the
            // first request. This is not playback policy; it keeps a slow boot or pre-roll
            // from consuming the observation window that is meant to establish the initial
            // route and, when present, its ABR controller.
            const QUALITY_SWITCH_OBSERVE_MS: u32 = 12_000;
            let playing = matches!(app.route, Route::Player { .. })
                && dur() > 0
                && crate::player::is_playing();
            if !playing {
                app.quality_playing_since = None;
            } else {
                let since = *app.quality_playing_since.get_or_insert(fr.now);
                if fr.now.wrapping_sub(since) >= QUALITY_SWITCH_OBSERVE_MS {
                    app.quality_tried = true;
                    if let Some((gap, qs)) = crate::dev::quality_switch_script() {
                        app.quality_gap_ms = gap;
                        app.quality_script_at = fr.now.wrapping_sub(gap); // fire the first step now
                        app.quality_script = qs;
                    }
                }
            }
        }
        if !app.quality_script.is_empty()
            && matches!(app.route, Route::Player { .. })
            && script_step_due(fr.now, app.quality_script_at, app.quality_gap_ms)
        {
            let q = app.quality_script.remove(0);
            app.quality_script_at = fr.now;
            // Logged BEFORE the call, because `set_quality` may reload the engine and the
            // line has to survive that to say what was asked for. The harness reads this to
            // pair each switch with what playback did after it.
            log(&format!(
                "quality: switch → {} ({} left)",
                crate::dev::quality_wire_name(q),
                app.quality_script.len()
            ));
            crate::route::set_quality(q);
        }
        // dev: /tmp/plxnative-autopause pauses once (headless paused-HUD capture), or carries
        // `delay=<ms>,hold=<ms>` for a deterministic Pause -> Resume playback transaction.
        if !app.pause_tried && matches!(app.route, Route::Player { .. }) && fr.now.wrapping_sub(app.t0) > 6000
        {
            app.pause_tried = true;
            if let Some(script) = crate::dev::pause_script() {
                app.pause_script = Some((fr.now.wrapping_add(script.delay_ms), script.hold_ms));
            }
        }
        if let Some((pause_at, hold_ms)) = app.pause_script {
            if matches!(app.route, Route::Player { .. }) && script_step_due(fr.now, pause_at, 0) {
                if set_transport_paused(mt, true) {
                    log(&format!(
                        "autopause: Pause accepted hold={}ms",
                        hold_ms.map_or_else(|| "forever".to_string(), |ms| ms.to_string()),
                    ));
                    app.pause_script = None;
                    app.pause_resume_at = hold_ms.map(|hold| fr.now.wrapping_add(hold));
                    set_hud(fr.now + HUD_HEADLESS_MS);
                }
            }
        }
        if let Some(resume_at) = app.pause_resume_at {
            if matches!(app.route, Route::Player { .. }) && script_step_due(fr.now, resume_at, 0) {
                if set_transport_paused(mt, false) {
                    log("autopause: Resume accepted");
                    app.pause_resume_at = None;
                }
            }
        }
        // dev: /tmp/plxnative-menu=<tab> opens the in-player track menu once (headless capture)
        if !app.menu_tried && matches!(app.route, Route::Player { .. }) && fr.now.wrapping_sub(app.t0) > 6000 {
            app.menu_tried = true;
            if let Some(t) = crate::dev::read("menu") {
                crate::ui::track_menu::open_tab(t.parse::<c_int>().unwrap_or(0));
                app.route = Route::Player {
                    overlay: Overlay::Menu,
                };
                set_hud(fr.now + HUD_HEADLESS_MS);
            }
            // dev: /tmp/plxnative-info opens the Info card once (headless capture)
            if crate::dev::flag("info") {
                crate::ui::info_panel::open();
                app.route = Route::Player {
                    overlay: Overlay::Info,
                };
                app.hud.nav.focus = 2;
                app.hud.nav.tab = 0;
                set_hud(fr.now + HUD_HEADLESS_MS);
            }
            // dev: /tmp/plxnative-chapters opens the Chapters strip once (headless capture)
            if crate::dev::flag("chapters") {
                crate::ui::chapters_panel::open();
                app.route = Route::Player {
                    overlay: Overlay::Chapters,
                };
                app.hud.nav.focus = 2;
                app.hud.nav.tab = 1;
                set_hud(fr.now + HUD_HEADLESS_MS);
            }
        }
        // dev: /tmp/plxnative-menupick="<tab>,<row>" opens the menu, selects that row, and
        // confirms it (headless track switch: e.g. "0,4" = audio tab, row 4).
        if !app.menupick_tried
            && matches!(app.route, Route::Player { .. })
            && fr.now.wrapping_sub(app.t0) > 7000
        {
            app.menupick_tried = true;
            if let Some(s) = crate::dev::read("menupick") {
                let mut it = s.split(',');
                let tab = it
                    .next()
                    .and_then(|x| x.trim().parse::<c_int>().ok())
                    .unwrap_or(0);
                let row = it
                    .next()
                    .and_then(|x| x.trim().parse::<c_int>().ok())
                    .unwrap_or(0);
                crate::ui::track_menu::open_tab(tab);
                // ABSOLUTE row (the initial focus is the active track now, not row 0)
                crate::ui::track_menu::focus_row(row);
                crate::ui::track_menu::on_ok();
            }
        }
        // dev: /tmp/plxnative-marker[=intro|credits] (default credits) seeks to 5s before that
        // marker's start, so the skip pill — and, on a `final` credits marker, the whole
        // finish → Up Next → auto-advance chain — is reachable in seconds instead of after 50
        // minutes of episode. Retried until the markers land (the playing-item store is
        // installed by the resolve, a beat after the first frames); a missing file settles it
        // once so the read isn't repeated every frame for the rest of the session.
        if !app.marker_tried && matches!(app.route, Route::Player { .. }) && crate::player::is_playing()
        {
            match crate::dev::read("marker") {
                Some(s) => {
                    let want = if s.eq_ignore_ascii_case("intro") {
                        crate::metadata::MarkerKind::Intro
                    } else {
                        crate::metadata::MarkerKind::Credits
                    };
                    // Latch on the STORE landing, not on a match: an item that carries no
                    // marker of the requested kind (credits-only items are common) otherwise
                    // left this re-reading the file every frame for the whole session.
                    let markers = crate::metadata::playing_markers();
                    if !markers.is_empty() {
                        app.marker_tried = true;
                        if let Some(m) = markers.iter().find(|m| m.kind == want) {
                            let t = (m.start_ms - 5_000).max(0) * 1_000_000;
                            log(&format!(
                                "marker trigger: seek to {}s (5s before {:?})",
                                t / 1_000_000_000,
                                want
                            ));
                            request_seek(t);
                        } else {
                            log(&format!("marker trigger: item has no {want:?} marker"));
                        }
                    }
                }
                None => app.marker_tried = true,
            }
        }
    true
}

/// The playback side of an iteration before the tick: the engine pump, the foreground load
/// poll, the reporter, EOS / Up Next, the hub refresh, the held key, the scrubber and the paused
/// seek — everything that reads `fr.now` and no `dt`.
pub(super) unsafe fn playback_tick(app: &mut App, mt: &crate::task::MainThread, fr: &mut Frame) {
        if is_started() {
            crate::player::pump(mt, fr.now);
        }
        let _ = poll_foreground_load(
            &mut app.foreground,
            &mut PlayerForegroundActuator {
                mt,
                repause_at: &mut app.repause_at,
            },
        );
        // **Unconditional, and NOT inside the `is_started` block above.** `player::state()`
        // derives two of its answers outside the pump entirely — `Resolving` while a plan is in
        // flight, and `Error` for a `/decision` refusal, which happens before an engine exists —
        // so gating this on a started engine would silently miss the earliest and most certain
        // failure there is. It observes the value the HUD renders and reports only transitions,
        // so the steady-state cost is one atomic load.
        crate::player::report::tick();
        // end-of-stream: the pipeline drained at the credits → hand off to Up Next when the
        // show has another episode queued, else leave the player (back to the detail page or
        // home, whichever is behind), instead of freezing on the last frame.
        if matches!(app.route, Route::Player { .. }) && crate::player::ended() {
            finish_playback(
                mt,
                &mut app.route,
                &mut app.play_from,
                &mut app.refresh_hubs_at,
                &mut app.hud.nav,
                &mut app.trail,
            );
            app.held_key.sym = 0; // async route flip: don't repeat a still-held key into detail/home
                              // dev: REPLAY AFTER COMPLETION (#46). `finish_playback` has just left the player —
                              // an Up Next handoff would have RETURNED there, and `matches!` below is what tells
                              // the two apart, so a replay can never cut into an auto-advance chain. Re-arming
                              // `auto_tried` sends the next frame back through the `playurl` entry, which calls
                              // `route::clear_url()` and lets `start_bufferfeed` read the trigger again.
                              //
                              // The trigger is read once at boot (`replay_left`), so this cannot be turned into
                              // an endless loop by a file appearing mid-run, and `dev::flag` is `false` at
                              // COMPILE time in a release build.
            if app.replay_left > 0
                && !matches!(app.route, Route::Player { .. })
                && crate::dev::flag("playurl")
            {
                app.replay_left -= 1;
                app.auto_tried = false;
                log(&format!(
                    "replay: starting the finished stream again ({} left)", app.replay_left
                ));
            }
        }
        // Up Next countdown elapsed → start the queued episode on its own. Beside the EOS
        // handoff so the whole auto-advance chain reads in one place.
        if matches!(app.route, Route::Player { .. }) && crate::ui::up_next::expired(fr.now) {
            if !play_up_next(mt, HUD_LINGER_MS, &mut app.route, &mut app.play_from, &mut app.hud.nav) {
                crate::ui::up_next::cancel(); // nothing queued after all — don't re-fire
            }
            app.held_key.sym = 0;
        }
        // post-playback home refresh (armed by every exit_player): refetch the hubs so
        // Continue Watching shows the new resume point / next episode; the small delay lets
        // the final timeline PUT land server-side first. The request is worker-only; the
        // landing logs the resulting item count when it actually commits.
        if app.refresh_hubs_at != 0
            && fr.now.wrapping_sub(app.refresh_hubs_at) < 0x8000_0000
            && !matches!(app.route, Route::Player { .. })
        {
            app.refresh_hubs_at = 0;
            crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::RefetchHubs);
            // …and every library's OWN shelves, for the same reason and at the same moment:
            // a finished playback moves Continue Watching and watch state, and a section deck
            // is as stale as the global one (`browse::section_hubs::invalidate_all`).
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::HubsInvalidateAll);
            log("home: hubs refresh queued after playback");
        }
        // lost-keyup safety: the remote streams 0x101 repeats (~50ms) while a key is physically down,
        // so once past the initial settle a stale heartbeat means the release keyup was dropped —
        // clear the held key so it can't repeat forever (mirrors the scrub's SCRUB_LOST_MS). The
        // 500ms gate leaves the first repeat and the heartbeat's own start-up untouched; a normal
        // release clears via the keyup long before this fires.
        if app.held_key.sym != 0
            && fr.now.wrapping_sub(app.held_key.since) > 500
            && fr.now.wrapping_sub(app.held_key.alive) > 350
        {
            app.held_key.sym = 0;
        }
        // client-side long-press repeat — the ONE hold-to-move path for every discrete focus list
        // (home grid, detail, track menu, info card, chapters). Driven by a held-key timer so it's
        // identical everywhere and independent of the remote's hardware auto-repeat delay.
        // `HeldKey::arm` is what each view's fresh-press handler calls (always with a standard
        // SDLK_*), and the keyup clears `sym`. The player scrubber is deliberately excluded —
        // holding it runs the continuous scrub.
        if app.held_key.sym != 0
            && fr.now.wrapping_sub(app.held_key.since) > 380
            && fr.now.wrapping_sub(app.held_key.last_rep) > 110
        {
            app.held_key.last_rep = fr.now;
            match app.route {
                Route::ItemMenu { .. } => {
                    crate::ui::item_menu::move_focus(app.held_key.sym as c_int)
                }
                Route::Library => crate::ui::library::move_focus(app.held_key.sym),
                Route::Search => crate::ui::search::move_focus(app.held_key.sym),
                Route::Player {
                    overlay: Overlay::Menu,
                } => {
                    crate::ui::track_menu::move_focus(app.held_key.sym as c_int);
                    extend_hud(fr.now, HUD_MENU_MS);
                }
                Route::Player {
                    overlay: Overlay::More,
                } => {
                    crate::ui::more_menu::move_focus(app.held_key.sym as c_int);
                    extend_hud(fr.now, HUD_MENU_MS);
                }
                Route::Player {
                    overlay: Overlay::Info,
                } => {
                    crate::ui::info_panel::move_focus(app.held_key.sym as c_int);
                    extend_hud(fr.now, HUD_MENU_MS);
                }
                Route::Player {
                    overlay: Overlay::Chapters,
                } => {
                    crate::ui::chapters_panel::move_focus(app.held_key.sym as c_int);
                    extend_hud(fr.now, HUD_MENU_MS);
                }
                _ => {}
            }
        }
        // keep the HUD alive while the track menu / Info card / Chapters strip is open
        if matches!(app.route, Route::Player { overlay } if overlay != Overlay::None) {
            extend_hud(fr.now, HUD_LINGER_MS);
        }
        // scrub: continuous accelerating advance while a key is held (`hold` set by 0x101).
        if app.scrubber.dir != 0 && app.scrubber.hold && scrub() >= 0 && !app.ptr.drag {
            let held = fr.now.wrapping_sub(app.scrubber.hold_since) as f32 / 1000.0;
            let speed = (SCRUB_BASE + SCRUB_ACCEL * held).min(SCRUB_MAX);
            let mut sdt = fr.now.wrapping_sub(app.scrubber.t) as f32 / 1000.0;
            if sdt > 0.1 {
                sdt = 0.1;
            }
            let was = scrub();
            let mut s = was + (app.scrubber.dir as f64 * speed as f64 * sdt as f64 * 1e9) as i64;
            let cap = dur() - 3 * 1_000_000_000;
            if s < 0 {
                s = 0;
            }
            if cap > 0 && s > cap {
                s = cap;
            }
            set_scrub(s);
            // Real travel is what turns a reveal into a scrub — not the hold edge, which fires
            // a beat earlier with nothing moved yet (`on_auto_repeat`). Once the preview has
            // left the seed the release commits like any other held gesture.
            if s != was {
                app.scrubber.reveal = false;
            }
            extend_hud(fr.now, HUD_LINGER_MS);
            app.scrubber.t = fr.now;
            // lost-keyup safety: commit if the 0x101 repeats stop without a keyup
            if fr.now.wrapping_sub(app.scrubber.alive) > SCRUB_LOST_MS {
                commit_seek(scrub(), &mut app.repause_at);
                app.scrubber.disengage();
            }
        }
        // tap release debounce: commit the accumulated jump(s) once no further tap arrives
        if app.scrubber.commit_at != 0 && fr.now.wrapping_sub(app.scrubber.commit_at) < 0x8000_0000 {
            if scrub() >= 0 {
                log(&format!("scrub: tap commit {}s", scrub() / 1_000_000_000));
                commit_seek(scrub(), &mut app.repause_at);
            } else {
                set_scrub(-1);
            }
            app.scrubber.disengage();
            app.scrubber.commit_at = 0;
        }
        // Focus follows the control row's OCCUPANT, on both edges. Driven by slot identity
        // rather than a "was something shown" bool, because the two edges have different jobs
        // and the previous bool implemented neither of the ones its comment promised.
        if matches!(
            app.route,
            Route::Player {
                overlay: Overlay::None
            }
        ) {
            // Keyed on the SEGMENT, not the slot, and `last_offer` is only ever advanced to
            // a real offer — never cleared back to None. `active_marker` is gated on `is_playing`,
            // so a momentary drop out of Playing mid-segment reads as "no segment" and flips the
            // row to the discs and back; keyed on the slot that round trip looked like a new
            // offer and re-raised the HUD over an intro the user was simply watching.
            let offer = fr.ctrl.offer();
            let fresh = offer.is_some() && offer != app.hud.last_offer;
            if offer.is_some() {
                app.hud.last_offer = offer;
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
                app.hud.raise_for_offer(fr.now, fr.ctrl.primary_btn());
            } else if crate::ui::player_hud::standin_left_the_ring(
                app.hud.was_standin,
                fr.ctrl,
                app.hud.nav.focus == 1,
            ) {
                // The stand-in went away under the focus ring. Without this the row swaps back
                // to the discs with focus still on it and `btn` still 0, so the next OK opened
                // the SUBTITLES menu instead of toggling pause — exactly the bug class HudNav's
                // own doc says it exists to kill. Strictly the EDGE: as a steady state it also
                // fired on a user who walked UP to the discs on purpose, yanking the ring back
                // the same frame and making OK on a disc unreachable by remote.
                app.hud.nav = HudNav::HOME;
            }
            app.hud.was_standin = !fr.ctrl.is_discs();
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
        if matches!(app.route, Route::Player { .. }) && crate::ui::up_next::armed() {
            if crate::ui::up_next::countdown_may_run(
                matches!(
                    app.route,
                    Route::Player {
                        overlay: Overlay::None
                    }
                ),
                app.hud.nav.focus == 1,
                app.hud.nav.btn,
            ) {
                app.hud.dismissed = false;
                extend_hud(fr.now, HUD_LINGER_MS);
            } else {
                crate::ui::up_next::cancel();
            }
        }
        // when the HUD auto-hides, park focus back on the scrubber so the next reveal is clean
        if matches!(app.route, Route::Player { .. })
            && !hud_visible(fr.now, hud_until(), paused(), app.hud.dismissed)
        {
            app.hud.nav = HudNav::HOME;
        }
        // hide the idle pointer during playback
        if matches!(app.route, Route::Player { .. })
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
            && matches!(app.route, Route::Player { .. })
            && crate::player::seek_preroll_active()
            && seek_pending() < 0
            && frames() >= 1
            && playpos() + 15 * 1_000_000_000 >= app.repause_at
        {
            crate::player::finish_paused_seek(mt);
        }
}

/// Results and requests that land BEFORE nav commit: the OK arm and the press machine, the
/// person / detail open requests, the login and profiles landings (`install_pms`), the glass
/// load, and the nav oscillator.
pub(super) unsafe fn land_results(app: &mut App, mt: &crate::task::MainThread, fr: &mut Frame) {
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
                    Route::Library => open_tile_menu(
                        &mut app.route,
                        MenuHost::Library,
                        crate::ui::library::focused_item(),
                        Opener {
                            rect: crate::ui::library::focused_card_rect(),
                            redraw: crate::ui::library::redraw_focused_card,
                        },
                        // …and THIS is the caller that has a deck: the library's own Continue
                        // Watching shelf, which the A–Z grid below it never is.
                        crate::ui::library::focused_from_deck(),
                    ),
                    Route::Search => open_tile_menu(
                        &mut app.route,
                        MenuHost::Search,
                        crate::ui::search::focused_media(),
                        Opener {
                            rect: crate::ui::search::focused_tile_rect(),
                            redraw: crate::ui::search::redraw_focused_tile,
                        },
                        false, // results are a query's answer, not a deck
                    ),
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
                    Route::Library => match crate::ui::library::on_ok() {
                        crate::ui::library::Action::ShelfCard { from_deck } => {
                            if let Some(mm) = crate::ui::library::focused_item() {
                                // the DECK plays; every other shelf navigates — see `home_activate`
                                let want_play = from_deck;
                                activate_card(
                                    mt,
                                    mm,
                                    want_play,
                                    HUD_LINGER_MS,
                                    &mut app.route,
                                    &mut app.play_from,
                                    &app.trail,
                                    &mut app.hud.nav,
                                    &mut app.nav_pending,
                                );
                            }
                        }
                        // the paged grid, and — for totality — the zones that cannot arm a
                        // press at all (`focus_is_card` is Grid or Shelf only)
                        _ => open_library_card(app.route, &mut app.nav_pending),
                    },
                    // ONE arm for the page's cards AND its hero control row: `on_ok`
                    // already resolves which, exactly as it does on the immediate path.
                    Route::Search => {
                        if let crate::ui::search::Action::Open(node) =
                            crate::ui::search::on_ok()
                        {
                            nav_open(app.route, node, None, &mut app.nav_pending);
                        }
                    }
                    // `Route::Profiles` is deliberately absent (phase 6): the picker is an owned
                    // screen now, so its own press machine arms and commits this activation —
                    // nothing here ever arms `app.input.press` for it any more (see the removed
                    // key/pointer arms above), so this deferred commit can never see that route.
                    // the transport's control row (discs or a stand-in)
                    Route::Player {
                        overlay: Overlay::None,
                    } => activate_player_row(
                        mt,
                        fr.ctrl,
                        fr.now,
                        &mut app.route,
                        &mut app.hud,
                        &mut app.held_key,
                        &mut app.trail,
                        &mut app.play_from,
                        &mut app.refresh_hubs_at,
                    ),
                    // the Info card's action column
                    Route::Player {
                        overlay: Overlay::Info,
                    } => commit_info_panel(
                        mt,
                        fr.now,
                        &mut app.route,
                        &app.play_from,
                        &mut app.refresh_hubs_at,
                        &mut app.trail,
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
        // the Account popover, still on `Route::Account`, before the loop has moved to
        // `Route::Profiles`), and a route gate here would leave that progress queued an extra
        // frame — or, worse, forever, if the route never becomes Login/Profiles at all before
        // something else empties `app.route` back to Home. Draining BEFORE the poll below is what
        // makes THIS frame's `auth::phase()`/`take_ready()` see whatever the worker reported this
        // frame rather than lagging it by one iteration.
        for progress in crate::auth::take_progress() {
            // `mt` is `land_results`'s own proof this really runs on the main thread — the same
            // token every other main-thread-only call in this function already carries. Not a
            // finding of this pass: `auth::apply_progress` grew this parameter as part of a
            // concurrent lane's own work on `auth.rs`, and this is the one call site (in a file
            // that lane does not own) its signature change left needing an update.
            crate::auth::apply_progress(mt, progress);
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
            let want = crate::ui::glassload::wants_account();
            if want && app.route == Route::Home {
                crate::ui::account_menu::open();
                app.route = Route::Account {
                    over: BarHost::Home,
                };
            } else if !want {
                if let Route::Account { over } = app.route {
                    crate::ui::account_menu::close();
                    app.route = over.route();
                }
            }
        }
        // dev: navosc bounces the route Home↔Library through the real request path (the
        // `home-library-nav` FPS scene). Route-unconditional, because it is the ROUTE it drives;
        // it goes through `nav_to` rather than assigning `route` so the scene measures exactly
        // what a tab press does, transition included.
        if app.dev.nav_osc && fr.now.wrapping_sub(app.nav_osc_last) > 1400 {
            app.nav_osc_last = fr.now;
            match app.route {
                // the DETAIL bounce is `nav_open` out and `nav_back` home — the same pair the
                // grid card and the BACK key raise, teardown included, so the scene measures
                // the whole round trip and not just its cheaper half
                Route::Home if !app.dev.nav_osc_rk.is_empty() => {
                    // a dev trigger names a bare rk, so it means "on the server we are signed
                    // in to" — the only server a headless boot has
                    nav_open(
                        app.route,
                        to_detail(crate::plex::current_server(), &app.dev.nav_osc_rk),
                        None,
                        &mut app.nav_pending,
                    )
                }
                Route::Detail => nav_back(app.route, &app.trail, &mut app.nav_pending),
                // the FIRST TYPE the strip actually draws — not `Movies` by name, which a
                // set with no favourite film library does not have a pill for at all
                Route::Home => {
                    if let Some(kind) = crate::browse::tab_kind(0) {
                        nav_to(app.route, Nav::Library(kind), &mut app.nav_pending)
                    }
                }
                // pill 1 is that same first TAB — not "the first section", which stopped being
                // the same thing when the strip became a projection of the table (`browse::tabs`):
                // several libraries can share one pill. Home comes back in the hero view with the
                // top band on the pill the round trip started from, so the scene is a loop
                Route::Library => nav_to(
                    app.route,
                    Nav::Home {
                        // the pill this round trip started from — by TYPE, so the scene is a
                        // loop whichever position that type's pill happens to occupy
                        focus_pill: Some(crate::ui::widgets::pill_at(1)),
                    },
                    &mut app.nav_pending,
                ),
                _ => {}
            }
        }

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
pub(super) unsafe fn nav_commit(app: &mut App, _mt: &crate::task::MainThread, fr: &mut Frame) {
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
                        // `resume`, NOT `enter("")`: the trail reset above throws away the way
                        // IN, never the screen's own state. The pill is a way back to a search
                        // you already made — `library::enter`'s `restore_view` one screen over
                        // — and a fresh profile needs no special case for it, since the store
                        // it returns to is empty until something is typed into it.
                        crate::ui::search::resume();
                        app.trail.push(Node::Search);
                        app.route = Route::Search;
                    }
                    Nav::Library(kind) => {
                        // every teleport `enter` performs (the store swap, `restore_view`'s
                        // scroll jump, the focus band) happens HERE, at alpha 0, off screen
                        crate::ui::library::enter(kind, crate::ui::library::Arrival::Faded);
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
                            crate::ui::machine::NavOp::Push(super::bridge::AppArg::from_node(&node)), ret);
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
        }
    }
}

/// The update phase: per-route screen updates (which run the route-gated store pumps), the
/// dev oscillators, the play landing and the remaining pumps — `tick_drain` on the FRAMEDROP
/// line.
pub(super) unsafe fn update(app: &mut App, mt: &crate::task::MainThread, fr: &mut Frame) {

        // `Route::Login`/`Route::Profiles` no longer have a route-gated `update(dt)` call here
        // (phase 6, mirroring `Route::Onboard`'s own removal in 5b): both are owned screens now,
        // and whatever per-frame animation the legacy `ui::login`/`ui::profiles` scenes used to
        // step belongs to their `Screen::step`/`prepare` on a `Tick` the dispatcher already
        // delivers every frame through `bridge::frame` — not a second call from this function.
        if matches!(app.route, Route::Profiles)
            && app.pick_user.is_some()
            && crate::auth::phase() == crate::auth::Phase::Profiles
            && !crate::auth::users().is_empty()
        {
            let idx = app.pick_user.take().unwrap();
            // **Ask the SAME question `screens::profiles::ProfilesScreen::select` asks before
            // acting, because this call site cannot reach that method to ask it FOR us.** That
            // owned screen holds its own focus and PIN-pad state on the engine, which `app/`
            // must not reach into directly (only the dispatcher's `InputEvent`s may drive a
            // screen's fields) — and there is today no PUBLIC door on `Dispatcher`/
            // `ProfilesScreen` that lets an external caller deliver a targeted "commit roster
            // tile N" event to a specific mounted instance (see this lane's report for the
            // method that would need to live in one of those two files, neither of which this
            // lane owns). Calling `auth::select_profile(idx)` unconditionally — what this arm did
            // until this fix — is `ProfilesScreen::select`'s UNPROTECTED branch with no gate in
            // front of it, so a protected index reached plex.tv's `/switch` with no PIN instead
            // of opening the pad. plex.tv still refuses that call server-side (a 401, not a
            // client-side bypass — `auth::switch_thread`'s `Refused` arm), so this was never a
            // security hole; it was the documented headless capture door
            // (`tests/manifest.json`'s "Tiles 1 and 2 must be UNPROTECTED — the harness cannot
            // type a PIN it does not know") silently attempting, and failing, the one case it
            // says it cannot cover. Refusing here instead — before any network call — turns that
            // into an honest skip with a reason, rather than a `Phase::Switching` flicker that
            // resolves to a refused-HTTP-401 log line nobody arming this trigger asked for.
            let protected = crate::auth::users()
                .get(idx)
                .map(|u| u.protected)
                .unwrap_or(false);
            if protected {
                log(&format!(
                    "pickuser: roster index {idx} is PROTECTED — refusing rather than attempting \
                     a PIN-less switch plex.tv would refuse anyway; this trigger has no door onto \
                     the owned picker's own PIN pad yet (see this pass's report for what a real \
                     fix needs)"
                ));
            } else {
                log(&format!("pickuser: auto-selecting roster index {idx}"));
                crate::auth::select_profile(idx);
            }
        }
        // **`page_of`, not the bare route, for every screen below.** A popover still DRAWS the
        // page it was opened over, but whether that page also UPDATES is the explicit
        // `host_page_updates` policy above.  ItemMenu keeps its anchored host live; Account
        // freezes its host so invisible hero/shelf work cannot steal frames from the menu.
        // Asking `page_of` here keeps the host identity in one place while the lifecycle policy
        // remains separately testable instead of being inferred from route shape.
        if host_page_updates(app.route, super::bridge::host_frozen(&app.pages))
            && matches!(page_of(app.route), Route::Home)
        {
            if app.dev.hero_osc && fr.now.wrapping_sub(app.hero_osc_last) > 700 {
                app.hero_osc_last = fr.now;
                app.bridge.home_command(HomeCmd::Flip(1));
            }
            if app.dev.home_fold_osc && fr.now.wrapping_sub(app.home_fold_osc_last) > 700 {
                app.home_fold_osc_last = fr.now;
                if app.home_fold_down {
                    app.bridge.home_command(HomeCmd::FocusGrid { row: 0, col: 0 });
                } else {
                    app.bridge.home_command(HomeCmd::Hero);
                }
                app.home_fold_down = !app.home_fold_down;
            }
            // dev: sweep the grid focus top↔bottom to reproduce the vertical-scroll judder headlessly
            if app.dev.home_osc && fr.now.wrapping_sub(app.home_osc_last) > 350 {
                app.home_osc_last = fr.now;
                let sym = if (fr.now / 3000) % 2 == 0 {
                    SDLK_DOWN
                } else {
                    SDLK_UP
                };
                app.inputs.extend(super::bridge::script_key(
                    if sym == SDLK_DOWN { crate::ui::machine::Key::Down } else { crate::ui::machine::Key::Up },
                    crate::ui::machine::Tick { ms: fr.now, dt_us: 0 }));
            }
            // only when home is actually drawn — stepping its 16×24 cell springs during
            // Player/Detail frames was pure waste on the A53 (the ui::press dip/commit is driven
            // route-agnostically right after `dt` above)
            let (_, moving) = crate::ui::idle::scoped_motion(|| {
                app.bridge.update_home_chrome(&mut app.pages, fr.dt);
            });
            fr.underlay_moving |= moving;
        } else if host_page_updates(app.route, super::bridge::host_frozen(&app.pages))
            && matches!(page_of(app.route), Route::Library)
        {
            // dev: libosc sweeps the browse-grid focus down↔up (the library_scroll FPS scene).
            // Only while the PAGE holds focus, for `detail_osc`'s reason: the context-menu
            // popover is modal, and sweeping focus under it walks the anchor out from under it.
            if app.dev.lib_osc
                && matches!(app.route, Route::Library)
                && fr.now.wrapping_sub(app.lib_osc_last) > 350
            {
                app.lib_osc_last = fr.now;
                // …but NOT on homeosc's 3s reversal: this document opens with the library chip
                // and one rung per shelf, so at 350ms a 12-shelf library reverses before it
                // ever reaches the grid — and the seam between the last shelf and the poster
                // wall is the thing this scene exists to sweep. `osc_step` reverses at the
                // document's own ends instead (`ui::library::osc_step`).
                crate::ui::library::osc_step();
            }
            // dev: libswitch cycles EVERY switch (tabs, sort menu, unwatched, filter) on a
            // timer so the re-query + popover paths are FPS-gated too
            if app.dev.lib_switch
                && matches!(app.route, Route::Library)
                && fr.now.wrapping_sub(app.lib_switch_last) > 1400
            {
                app.lib_switch_last = fr.now;
                crate::ui::library::switch_step(app.lib_switch_step);
                app.lib_switch_step = app.lib_switch_step.wrapping_add(1);
            }
            // scoped like Home's above, because this page can be the one UNDER the account
            // popover now and its glass backdrop is refreshed off the underlay's motion
            let (_, moving) = crate::ui::idle::scoped_motion(|| {
                crate::ui::library::update(fr.dt);
            });
            fr.underlay_moving |= moving;
        }
        if host_page_updates(app.route, super::bridge::host_frozen(&app.pages))
            && matches!(page_of(app.route), Route::Search)
        {
            // dev: searchosc sweeps the result shelves' focus down↔up (the fps:search-type
            // scene). Same 350ms step / 3s reversal as homeosc and libosc, so the three read
            // the same in a log and one settle predicate covers all of them. Frozen under the
            // context menu, for `detail_osc`'s reason.
            if app.dev.search_osc
                && matches!(app.route, Route::Search)
                && fr.now.wrapping_sub(app.search_osc_last) > 350
            {
                app.search_osc_last = fr.now;
                let sym = if (fr.now / 3000) % 2 == 0 {
                    SDLK_DOWN
                } else {
                    SDLK_UP
                };
                crate::ui::search::move_focus(sym);
            }
            let (_, moving) = crate::ui::idle::scoped_motion(|| {
                crate::ui::widgets::tab_row_update(
                    crate::ui::search::selected_pill(),
                    crate::ui::search::top_focus(),
                    fr.dt,
                );
                crate::ui::search::update(fr.dt);
            });
            fr.underlay_moving |= moving;
        }
        // (The television's keyboard used to be dismissed HERE, by an `else` that called
        // `textinput::stop()` on every frame of every other route — because `search::leave`
        // was reached by no route off the screen. It is `forward_leave`'s job now: Search is
        // not a trail page, so every way off it carries its teardown to the fade floor, which
        // is where the panel is meant to come down and is also the half the poll never did —
        // it cleared `textinput`'s own flag and left `search::EDITING` set.)
        if app.dev.account_osc && matches!(app.route, Route::Account { .. }) {
            // `wake`, not `invalidate`: this buys the continuous present the scene grades
            // without claiming the PAGE changed — an unscoped per-frame invalidate here read
            // as page damage and re-rendered the frozen host under the menu on every frame
            // (26 fps against the 50 floor, 2026-09-04), grading the oscillator, not the app.
            crate::ui::idle::wake();
            if fr.now.wrapping_sub(app.account_osc_last) > 520 {
                app.account_osc_last = fr.now;
                let sym = if app.account_osc_down { SDLK_DOWN } else { SDLK_UP };
                app.account_osc_down = !app.account_osc_down;
                crate::ui::account_menu::move_focus(sym as c_int);
            }
        }
        // ---- the Settings family's dev oscillators, on the tree -------------------------
        //
        // All six drive the SAME screens through the same door the remote does: a synthesised
        // `InputEvent` with `Source::Script`, queued on `app.inputs` for the dispatcher's next
        // frame. That is what replaced calling `on_updown`/`on_ok` on four modules by hand and
        // then asking four `is_open()` flags to decide which — the top page of the surface takes
        // the key whatever it is, so `settings_osc` no longer needs to know that Legal and
        // Privacy exist. **Every interval below is unchanged**, because these scenes' gates are
        // read against a cadence (`fps:modal-ramp`, `legal-document`, `decision-alert`,
        // `settings-*`): 1500 ms for the modal ramp, 520 ms for the three focus sweeps.
        let script_tick = crate::ui::machine::Tick {
            ms: fr.now,
            dt_us: (fr.dt * 1_000_000.0) as u32,
        };
        if app.dev.modal_osc && app.settings_tried && fr.now.wrapping_sub(app.modal_osc_last) > 1500 {
            app.modal_osc_last = fr.now;
            if super::bridge::settings_up(&app.pages) {
                // A DISMISS, not a teardown: the interactive exit runs the fade, and the fade is
                // the half of the ramp this scene exists to grade. `settings_up` is true through
                // that fade as `settings::is_open()` was, so the surface is never re-presented
                // over one still closing.
                super::bridge::dismiss_surfaces(&mut app.pages);
            } else {
                super::bridge::open_settings(&mut app.pages);
            }
        }
        if app.dev.legal_doc
            && !app.legal_doc_tried
            && super::bridge::surface_word(&app.pages) == Some(crate::screens::registry::word::LEGAL)
        {
            // The Legal index opens on its first row, so one OK is the whole trigger. Waiting on
            // the WORD rather than on `settings_up` is what makes it a one-shot on the right page:
            // `plxnative-settings=legal` roots the surface at the index, and until that page has
            // mounted the surface still names `settings`.
            app.legal_doc_tried = true;
            app.inputs.extend(super::bridge::script_key(crate::ui::machine::Key::Ok, script_tick));
        }
        if app.dev.alert_boot
            && !app.alert_tried
            && super::bridge::surface_word(&app.pages) == Some(crate::screens::registry::word::PRIVACY)
        {
            // Walk to *Delete all local data* and press it. See `boot`'s `alert_step` for why
            // this is a bounded count of DOWNs at one press per frame rather than a row index:
            // the row is the last of that table, DOWN at the last row is an `Outcome::Edge`, and
            // the table's length is the screen's own business.
            const ALERT_WALK: u8 = 16;
            let key = if app.alert_step < ALERT_WALK {
                app.alert_step += 1;
                crate::ui::machine::Key::Down
            } else {
                app.alert_tried = true;
                crate::ui::machine::Key::Ok
            };
            app.inputs.extend(super::bridge::script_key(key, script_tick));
        }
        if app.dev.settings_osc && super::bridge::settings_up(&app.pages) {
            // This is deliberately continuous. Row springs naturally settle between D-pad
            // steps, so measuring only their duty cycle would grade timing policy rather than
            // the GPU cost of the Settings composition the user asked to hold at 50 fps.
            // `wake` rather than `invalidate`, for `account_osc`'s reason above.
            crate::ui::idle::wake();
            // Keep presenting continuously, but move focus at a human D-pad cadence. At 120ms
            // the target alternated before TableView's pill spring could reach either row: ink
            // changed immediately while the white plate hovered at their midpoint, a test-only
            // picture that looked like broken production focus.
            if fr.now.wrapping_sub(app.settings_osc_last) > 520 {
                app.settings_osc_last = fr.now;
                let key = if app.settings_osc_down {
                    crate::ui::machine::Key::Down
                } else {
                    crate::ui::machine::Key::Up
                };
                app.settings_osc_down = !app.settings_osc_down;
                app.inputs.extend(super::bridge::script_key(key, script_tick));
            }
        }
        if app.dev.consent_osc && super::bridge::consent_up(&app.pages) {
            // The FIRST-RUN question, never the Settings Privacy page — which is what
            // `consent::is_open() && !settings::is_open()` used to say, and what asking the
            // surface's own identity says now without the two flags having to be ordered.
            crate::ui::idle::invalidate();
            if fr.now.wrapping_sub(app.consent_osc_last) > 520 {
                app.consent_osc_last = fr.now;
                let key = if app.consent_osc_down {
                    crate::ui::machine::Key::Down
                } else {
                    crate::ui::machine::Key::Up
                };
                app.consent_osc_down = !app.consent_osc_down;
                app.inputs.extend(super::bridge::script_key(key, script_tick));
            }
        }
        if app.dev.onboard_osc && matches!(app.route, Route::Onboard) {
            crate::ui::idle::invalidate();
            if fr.now.wrapping_sub(app.onboard_osc_last) > 520 {
                app.onboard_osc_last = fr.now;
                let key = if app.onboard_osc_right {
                    crate::ui::machine::Key::Right
                } else {
                    crate::ui::machine::Key::Left
                };
                app.onboard_osc_right = !app.onboard_osc_right;
                app.inputs.extend(super::bridge::script_key(key, script_tick));
            }
        }
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
        crate::ui::account_menu::update(fr.dt);
        crate::ui::item_menu::update(fr.dt);
        if app.dev.detail_osc && matches!(app.route, Route::Detail) {
            let key = if (fr.now / 450) % 2 == 0 { crate::ui::machine::Key::Down } else { crate::ui::machine::Key::Up };
            app.inputs.extend(super::bridge::script_key(key, crate::ui::machine::Tick { ms: fr.now, dt_us: 0 }));
        }
        if matches!(
            app.route,
            Route::Player {
                overlay: Overlay::Menu
            }
        ) {
            crate::ui::track_menu::update(fr.dt); // pill slide + open fade
        }
        if matches!(
            app.route,
            Route::Player {
                overlay: Overlay::More
            }
        ) {
            crate::ui::more_menu::update(fr.dt);
        }
        // Re-samples on its own 2 Hz hold; a no-op when the panel is off.
        crate::ui::stats::update(fr.now);
        // …and the lab upload's toast, which expires on a clock rather than a spring.
        crate::lab::update(fr.now);
        if matches!(
            app.route,
            Route::Player {
                overlay: Overlay::Info
            }
        ) {
            crate::ui::info_panel::update(fr.dt);
        }
        if matches!(
            app.route,
            Route::Player {
                overlay: Overlay::Chapters
            }
        ) {
            crate::ui::chapters_panel::update(fr.dt);
        }
        // Stepped for the WHOLE player route, not per-overlay like the panels above: the
        // countdown must keep running whichever overlay state the route reports.
        // Arm the Up Next countdown the frame it takes the control row. Nothing to step: both
        // stand-ins are drawn by `draw_hud`, so they inherit the transport's visibility rather
        // than owning any motion of their own.
        if matches!(app.route, Route::Player { .. }) {
            crate::ui::up_next::tick(fr.ctrl, fr.now);
            // …and the transport discs' focus pop, for the reason its own doc gives: it must be
            // stepped once per FRAME, and `draw_hud` does not run on every frame of this route.
            crate::ui::player_hud::update(fr.ctrl, app.hud.nav.focus, app.hud.nav.btn, fr.dt, fr.now);
        }
        // Async play resolve: install the worker's plan and start the engine. Route-
        // unconditional — a landing must never depend on which screen is mounted.
        land_play_then_observe(
            || {
                if let Some(r) = crate::route::pump_play() {
                    crate::ui::idle::invalidate();
                    let resume_prepared = r <= 0
                        || matches!(
                            crate::player::resume_at(r),
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
                        && crate::player::start_bufferfeed(mt)
                        && !matches!(app.route, Route::Player { .. })
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
                        app.route = Route::Player {
                            overlay: Overlay::None,
                        };
                    }
                }
            },
            // `pump_play` can install a refused `/decision` after the earlier report
            // observation but before this frame draws the Error screen. Observe again at that
            // exact publication boundary; latches make a healthy/no-change frame idempotent.
            crate::player::report::tick,
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
        // mounts. It invalidates from inside `alt_sources::install`, since a landing that grows
        // the actions row must be drawn without waiting for a keypress.
        crate::stores::metadata::pump_alt_sources();
}

/// The draw phase, entered only on a presenting frame: `clear_opaque_region` at entry, the
/// viewport, glass owners' prepare, the page pass, the surfaces bottom-to-top and the
/// instruments. Returns the viewport for the host-side screenshot that follows the draw.
pub(super) unsafe fn draw(app: &mut App, _mt: &crate::task::MainThread, fr: &mut Frame) -> (i32, i32, i32, i32) {
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
            crate::ui::glassload::prepare(fr.now);
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
                        crate::system::clear_opaque_region();
                        glClearColor(0.0, 0.0, 0.0, 0.0);
                        glClear(GL_COLOR_BUFFER_BIT);
                        let hud_up = hud_visible(fr.now, hud_until(), paused(), app.hud.dismissed);
                        // ONE resolve of which surface owns the "pipeline is working" signal, handed to
                        // both draws, so the centred read-out and the transport's inline spinner can
                        // never both light in the same frame. Resolved HERE (not beside `ctrl` at the
                        // top of the iteration) because `player::pump` republishes the state
                        // mid-iteration and this must be the post-pump value.
                        let busy = crate::ui::player_hud::busy();
                        // Both subtitle paths lift clear of the transport for the same reason and by
                        // the same test — an open track menu counts, since that is exactly when the
                        // user is reading the bottom of the screen.
                        let subs_lift = hud_up
                            || matches!(
                                app.route,
                                Route::Player {
                                    overlay: Overlay::Menu
                                }
                            );
                        crate::ui::player_hud::draw_subtitle_bitmap(subs_lift); // PGS/VobSub image subs
                        crate::ui::player_hud::draw_subtitles(subs_lift);
                        if hud_up
                            || !matches!(
                                app.route,
                                Route::Player {
                                    overlay: Overlay::None
                                }
                            )
                        {
                            // hide the transport middle behind the Info card / Chapters strip
                            crate::ui::player_hud::draw_hud(
                                fr.ctrl,
                                busy,
                                app.hud.nav.focus,
                                app.hud.nav.btn,
                                app.hud.nav.tab,
                                fr.now,
                                !matches!(
                                    app.route,
                                    Route::Player {
                                        overlay: Overlay::Info | Overlay::Chapters
                                    }
                                ),
                            );
                        }
                        // The read-out is NOT transport chrome — it is drawn whether or not the HUD is
                        // up, so a terminal `Error` (which is not `is_busy()`, so it does not pin the
                        // HUD) keeps its message instead of vanishing with the 4.5 s linger. AFTER the
                        // transport, so it is never dimmed by the scrim; BEFORE the overlay panels
                        // below, so an open Info card / Chapters strip still covers it.
                        crate::ui::player_hud::draw_readout(busy, fr.now);
                        // Stale content panels are gated on the SAME failure as the transport.
                        // More is the deliberate exception: the failed read-out opens that shared
                        // quality picker as its recovery path, so it must remain visible and
                        // drivable over the black failure ground.
                        let panels = !crate::ui::player_hud::transport_hidden();
                        if panels
                            && matches!(
                                app.route,
                                Route::Player {
                                    overlay: Overlay::Menu
                                }
                            )
                        {
                            crate::ui::track_menu::draw();
                        }
                        if panels
                            && matches!(
                                app.route,
                                Route::Player {
                                    overlay: Overlay::Info
                                }
                            )
                        {
                            crate::ui::info_panel::draw();
                        }
                        if panels
                            && matches!(
                                app.route,
                                Route::Player {
                                    overlay: Overlay::Chapters
                                }
                            )
                        {
                            crate::ui::chapters_panel::draw();
                        }
                        if matches!(
                            app.route,
                            Route::Player {
                                overlay: Overlay::More
                            }
                        ) {
                            crate::ui::more_menu::draw();
                        }
                        // LAST, over everything including the centred "Buffering…" read-out whose
                        // block sits where this panel wants to be. It is not chrome and not an
                        // overlay route: it stays up until it is turned off. Nothing here occludes
                        // its own off-switch: the panel is top-left and `more_menu`, which carries
                        // the toggle, is a right-edge popover.
                        crate::ui::stats::draw();
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
                            if matches!(page_of(app.route), Route::Home) {
                                app.bridge.prepare_home_chrome();
                            } else {
                                crate::ui::widgets::tab_glass_prepare();
                            }
                        }
                        // The episode tiles' frosted label band — `/tmp/plxnative-tileglass`,
                        // the 2026-09-05 experiment. Self-gated on the trigger, so a default
                        // build resolves nothing here; unlike the two owners either side of it
                        // this one is not routed at all, because the shelves it rides appear on
                        // Home, the Library and Search and the trigger is the whole condition.
                        crate::ui::widgets::tile_glass_prepare();
                        // The person page's bio alert, the other refreshing backdrop in the app.
                        // It needs the cadence resolved for the reason its own `POP` records: a
                        // CACHED snapshot would be taken on the frame it opened, with its scrim
                        // still ramping through zero, and would frost an undimmed page for the
                        // rest of the session.
                        if matches!(app.route, Route::Person) {
                            crate::ui::person_bio::prepare_present(
                                fr.underlay_moving || crate::ui::idle::present_dirty(),
                            );
                        }
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
                        // **`page_of`, not the bare route** — the same reason one altitude up: a
                        // POPOVER names the page it stands on, and BOTH of them can stand on more
                        // than one. Both stand on a FROZEN host now (`Popover::caching_host()`,
                        // `popover::host`) — ItemMenu is no longer the live-page exception it was —
                        // and that is exactly why the page still matters: the snapshot's FIRST frame
                        // draws the real tree, and it has to be the right tree. Account may sit over
                        // any of the three screens that wear the top bar. Spelled out as
                        // routes, only the detail arm ever said so and this closure's `else` meant
                        // "Home" — so the Library, Search and person page all fell through to
                        // `home_draw` the moment they became menu hosts, and the account popover
                        // drew Home over the Library the moment the profile chip became pressable
                        // there.
                        //
                        // It used to have a fourth case, and its removal is the whole of what
                        // phase 5b did to this line: while the Home-sources editor was a ROUTE
                        // borrowed by Settings, `Route::Onboard` in settings mode had to draw
                        // the page the modal was STANDING on (`SETTINGS_HOME_RETURN`) rather
                        // than the editor. The editor is a page of the surface's own stack now,
                        // so the route under it never moved and `page_of` is the whole answer.
                        let page_route = page_of(app.route);
                        // Does the DISPATCHER draw the top page (first-run Favourites today)?
                        // Sampled once, before the closure, because both the visible pass and the
                        // blur source pass below are gated on it — and because `page` borrows
                        // nothing of `app`, which is what keeps this readable.
                        let page_owned = super::bridge::page_owned(&app.pages, app.route);
                        // …and the host fold: an opaque surface whose ground has drawn REPLACES
                        // the page, so the legacy page pass is skipped wholesale. This was two
                        // `host_ground_ready()` reads, one per full-screen popover, which is
                        // exactly the list that could not be extended without editing this line.
                        let host_replaced = super::bridge::host_replaced(&app.pages);
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
                            // This was `account_menu`'s private `FrameCache` and applied to
                            // exactly one popover. Every popover that asked for it now gets it,
                            // including the four that draw from INSIDE their page and so could
                            // never have used the old shape.
                            let _host = crate::ui::popover::host::page_pass();
                            // `Route::Login`/`Route::Profiles` are deliberately absent (phase 6,
                            // mirroring `Route::Onboard`'s own absence here since 5b): both are
                            // owned screens now, so `page_owned` reports `true` for either and
                            // this whole closure is skipped in favour of the dispatcher's own
                            // page pass — see the `page_owned`/`host_replaced` guards below.
                            if page_owned {
                                app.pages.draw(&mut app.bridge, true);
                            } else if matches!(page_route, Route::Library) {
                                crate::ui::library::draw();
                            } else if matches!(page_route, Route::Search) {
                                crate::ui::search::draw();
                            } else {
                                // A loop request may have selected Home after this frame's
                                // commit. Its owned body mounts next frame; never draw a retired
                                // singleton while waiting for that lifecycle boundary.
                                crate::gfx::frame_clear(crate::ui::theme::CLEAR_RGB.0, crate::ui::theme::CLEAR_RGB.1, crate::ui::theme::CLEAR_RGB.2);
                            }
                            // **A popover drawn AFTER this closure owes its scrim TO it.** That is
                            // the rule, and these are the two popovers in that class — every other
                            // one draws inside its own page (`alt_sources`, the Library's sort
                            // menu) or is player-route, where there is no page closure and the dim
                            // is meant to cover the HUD as well.
                            //
                            // The scrim sits between the page and the popover's glass, so it is
                            // part of what that glass looks through, and this closure is what the
                            // direct source path re-renders. Drawn with the panel instead it
                            // reaches the visible frame but never the snapshot, and the frosted
                            // ground comes out at full page brightness inside a dimmed screen —
                            // which is exactly what the profile menu did.
                            //
                            // **Both, not just the dynamic one.** `item_menu` is served by the
                            // capture path today and so picks its scrim up for free — but only
                            // because no dynamic owner is live while a popover is open, so nothing
                            // invalidates and the direct path never runs. Three modules holding up
                            // one invariant, already false under `/tmp/plxnative-glassboth`. Each
                            // call self-gates on its own `is_open`, so there is no route test here:
                            // the closure states a rule rather than naming a screen.
                            //
                            // Each also LIFTS its own opener back out of the dim — the focused
                            // tile, the profile chip — and that belongs here for the same reason
                            // and one more: the un-dimmed copy has to be in the SNAPSHOT too, or
                            // the panel's glass frosts a dimmed picture of the very card it is
                            // about (`Popover::scrim_lifting`).
                            //
                            // **The account arm's special case is gone.** It used to skip this
                            // call on the visible pass and repeat it further down, after
                            // `capture_host` — because the capture had to land between the page
                            // and the dim. `popover::host::live` now owns that instant for every
                            // popover: the first lift of the frame takes the snapshot, so the
                            // scrim and its lift are live above the quad on both passes, in one
                            // place, in the draw order they always occupied.
                            crate::ui::account_menu::draw_scrim();
                            crate::ui::item_menu::draw_scrim();
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
                        // `page_owned` is the second guard, and it is not the same question:
                        // `host_replaced` says a SURFACE has taken the screen, while this says
                        // the top PAGE itself is an owned screen the dispatcher draws below — so
                        // `page()` would render whatever legacy route sat in its `else`. Home,
                        // today, which is exactly the failure the `page_route` note two altitudes
                        // up describes for the popovers.
                        if !host_replaced || page_owned {
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
                        // something DOES sit in its corner — `account_menu`, which carries the row
                        // that turns it off. Drawn last it covered the account chip and then the
                        // popover itself, so the switch could only be found by pressing keys at a
                        // menu you cannot see. A control must be visible over the thing it
                        // controls. Two call sites, and the `else` covers every non-player route,
                        // so a new route still cannot be forgotten.
                        crate::ui::stats::draw();
                        // Both self-gated on `Popover::visible`, not on the route: a dismissed
                        // menu's route flips back to its host on the press frame while the
                        // panel is still fading out over it (`Popover::dismiss`).
                        crate::ui::account_menu::draw(); // profile popover, over the page it opened on
                        crate::ui::item_menu::draw(); // press-and-hold card menu, over the live screen
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
                        // `pages` is `page_owned` and it must be exactly that: the argument says
                        // whether the PAGE pass runs here too, and this is the only call, so a
                        // `true` for a legacy top page would draw a `LegacyPage` (which paints
                        // nothing) instead of the page closure above, while a `false` for an
                        // owned one would leave first-run Favourites unpainted. `Dispatcher::draw`
                        // also fills and SWAPS the hit map, so calling it twice in a frame — once
                        // inside the page closure and once here — would register the surfaces'
                        // stops twice and hand the pointer a map from the wrong pass.
                        //
                        // The consequence of drawing an owned PAGE here rather than in the page
                        // closure is that it lands over `stats`, `account_menu` and `item_menu`.
                        // That is sound today and stated rather than assumed: the three owned
                        // pages are `Route::Onboard`, `Route::Login` and `Route::Profiles` (phase
                        // 6 added the last two), none of which wears a tab bar (so the profile
                        // chip that opens the account menu is on none of them), grows a card
                        // context menu, or is reached before any of it. It stops being sound the
                        // moment a page that CAN host a popover is migrated, which is phase 5c's
                        // problem and is why the popovers move onto the tree with it.
                        if !page_owned { app.pages.draw(&mut app.bridge, false); }
                        // dev: the blurred route transition, then the load dial's glass surfaces.
                        // LAST on the non-player path, so the snapshot either takes is of the
                        // COMPLETE page — which is the honest source for a surface that sits on
                        // top of everything, and the one thing the tab track (drawn inside the
                        // page) cannot have.
                        crate::ui::glassload::draw_nav_blur();
                        crate::ui::glassload::draw();
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
                    crate::lab::draw();
                });
            });
    (vx, vy, vw, vh)
}

/// The iteration's report: the route word (on change), the lab route note, the focus probe
/// and the FRAMEDROP line.
pub(super) unsafe fn report(app: &mut App, _mt: &crate::task::MainThread, fr: &mut Frame) {
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
                Route::Account { over } => crate::focusprobe::Screen::Account {
                    over: probe_bar_host(over),
                },
                Route::ItemMenu { over } => crate::focusprobe::Screen::ItemMenu {
                    over: probe_host(over),
                },
                Route::Library => crate::focusprobe::Screen::Library,
                Route::Detail => crate::focusprobe::Screen::Detail,
                Route::Person => crate::focusprobe::Screen::Person,
                Route::Search => crate::focusprobe::Screen::Search,
                Route::Player { overlay } => crate::focusprobe::Screen::Player {
                    // the same words the heartbeat's `overlay=` uses, below
                    overlay: match overlay {
                        Overlay::None => "none",
                        Overlay::Menu => "menu",
                        Overlay::Info => "info",
                        Overlay::Chapters => "chapters",
                        Overlay::More => "more",
                    },
                },
        };
        let probe_hud = |app: &App| crate::focusprobe::Hud {
            focus: app.hud.nav.focus,
            btn: app.hud.nav.btn,
            tab: app.hud.nav.tab,
            visible: hud_visible(app.last_input, hud_until(), paused(), app.hud.dismissed),
        };
        if crate::focusprobe::armed() {
            let content = super::bridge::content_probe(&app.pages, &app.bridge);
            crate::focusprobe::sample(fr.rn, probe_screen(app), probe_hud(app), fr.ctrl, &content);
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
            let focus = crate::focusprobe::line(fr.rn, probe_screen(app), probe_hud(app), fr.ctrl, &content);
            let ov = overlay_word(&app.pages, app.route);
            let tree = app.pages.state_hash();
            let press = &app.input.press;
            let done = app
                .rec
                .end_frame(&|| super::recorder::state_hash(press, rn, ov, &focus, tree));
            if done {
                app.running = false;
            }
        }
        // frame-drop detector: attribute slow frames to pump(uploads)/draw/swap(GPU). Drains the
        // per-frame upload counters every frame (so the count is per-frame, not cumulative).
        // ONE tail for every route — this used to live only on the non-player path, which left
        // /tmp/plxnative-framedrop dead during playback (the timings were collected, then a
        // `continue` threw them away).
        // `present` gates this too: a frame the idle gate skipped drew nothing, so grading it
        // would drag `worstframe` toward zero and read as a perf WIN. A skipped frame is not a
        // fast frame — it is an absent one, and `fps=` on the heartbeat is where it shows up.
        if fr.present {
            if let Some(line) = app.instr.frame_drop_line(&|| {
                let (up, px) = super::adapters::poster::take_upload_stats();
                let (cards, cards_off) = crate::gfx::take_card_stats();
                format!(
                    "up={up} px={px} cards={cards} off={cards_off} route={rn} load={} snapt={:.2}",
                    crate::ui::glassload::step_index(),
                    app.bridge.home_snap_target(&app.pages)
                )
            }) {
                log(&line);
            }
        }
}

/// The once/sec heartbeat line and its counters.
pub(super) unsafe fn heartbeat(app: &mut App, _mt: &crate::task::MainThread, fr: &mut Frame) {
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
            let ov = overlay_word(&app.pages, app.route);
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
            let playing = crate::player::is_playing() && pos_ns > 0;
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
            let vp = if crate::player::is_playing() {
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
            let ld = if crate::ui::glassload::armed() || app.dev.glass_hz_armed {
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
            // `worstprep=` follows it, ungated by present. Both are empty unarmed.
            let tail = app.instr.heartbeat_tail(app.rec.take_spent_us());
            log(&format!(
                "loop={} route={rn}{ov}{pos}{vp} fps={pres}{ld}{tail}{SIM_TAG}",
                app.loop_shown
            ));
        }
}

/// Teardown after the loop: abandon the pending report, stop the feed, wait for the stop
/// scrobble, close the capture stream and the poster workers, quit SDL.
pub(super) unsafe fn shutdown(mt: &crate::task::MainThread) {
    crate::player::report::abandon_pending();
    if is_started() {
        crate::player::stop_bufferfeed(mt);
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
unsafe fn replay_inject(v: &serde_json::Value) {
    let kind = v["kind"].as_str().unwrap_or("");
    let i = |k: &str| v[k].as_i64().unwrap_or(0) as i32;
    let u = |k: &str| v[k].as_u64().unwrap_or(0) as u32;
    match kind {
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
            let _ = dispatch_remote_token(v["tok"].as_str().unwrap_or(""));
        }
        other => log(&format!("replay: input kind {other:?} is not replayable; skipped")),
    }
}
