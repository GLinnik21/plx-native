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

/// One lazy product frame-tail hash path for recording and replay grading alike.
pub(super) fn recorder_end_frame(
    rec: &mut super::recorder::Recplay,
    bridge: &super::bridge::Bridge,
    press: &plx_ui::press::Press,
    route: &str,
    overlay: &str,
    focus: &str,
    tree: u64,
) -> bool {
    rec.measurements(bridge);
    rec.content_end();
    rec.end_frame_with_gate(&|| super::recorder::state_hash(
        press, route, overlay, focus, tree, bridge.session_subhash(), bridge.consent_subhash(),
        bridge.initial_subhash(),
    ), &|id| bridge.store_gen(id), bridge.landgate())
}

/// The per-iteration values that cross a phase boundary. Reset at the top of every iteration
/// (`Frame::begin`); every field is written by exactly one phase and read by the ones after it.
pub(crate) struct Frame {
    /// The HUD's control slot for this frame, sampled once before input is read.
    pub(crate) ctrl: plx_appkit::player_hud::ControlSlot,
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
    /// This iteration did NOT present only because a page capture is still in flight on the GPU
    /// (`gfx::snapshot_pending`): the window was active, so the fence was the one reason. Pure
    /// bookkeeping for the present-interval stamps and the bench; it never feeds a decision.
    pub(crate) deferred: bool,
    /// The heartbeat's route word for this frame (`route_word`).
    pub(crate) rn: &'static str,
}

impl Frame {
    fn begin(ps: &plx_media::route::PlaybackSession, meta: plx_data::metadata::MetadataView<'_>) -> Frame {
        Frame {
            ctrl: plx_appkit::player_hud::slot(ps, meta),
            now: 0,
            dt: 0.0,
            underlay_moving: false,
            player: false,
            present: false,
            deferred: false,
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
    let watchdog = plx_base::task::watchdog::LoopWatch::start();
    let mut clock_notice = super::clock_notice::ClockNotice::new();
    let mut hint_taught = super::hint_taught::HintTaught::new();
    while app.running {
        watchdog.advance();
        plx_telemetry::telemetry::session::tick(matches!(app.route(), AppArg::Player));
        let _frame_scope = plx_base::task::FrameScope::enter();
        #[cfg(all(feature = "hostsim", target_os = "linux"))]
        let wslg_frame_budget = app.wslg_frame_pacing.then(super::window_activity::WslgFrameBudget::begin);
        // Resolve the control row ONCE per iteration, before the event pump, and pass this
        // value to input, update and draw alike. `player_hud::slot()` reads `playpos_ns`, which
        // LG's media thread writes and `player::pump` advances mid-iteration — deriving it per
        // call site let a keypress activate a control this same frame then declined to draw.
        // Frame-drop detector, stamp 0 of 9: the TOP of the iteration, before the ls2 pump
        // and the event poll — `worstframe=` covers the whole iteration, not the half after
        // the input. Eight phases between the nine stamps, named after the frame algorithm
        // the restructure moves this loop onto (spec §3.3/§8.4); on THIS loop navcommit
        // precedes tick_drain, and the FRAMEDROP line prints them in the algorithm's order.
        let mut fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
        let first_controlled_frame = app.boot_initial.is_some() && app.prev == 0;
        let fr = &mut fr;
        app.instr.mark(plx_base::diag::heartbeat::Phase::Top); // also starts the visit's hang interval
        // The frame index the LANDING SCHEDULE stamps against (§3.3 step 3, `plx_machine::landgate`),
        // published at the TOP because a landing site is reachable from the dev scenarios below
        // as well as from `land_results` and the dispatcher's own frame. One relaxed atomic load
        // unless `plxnative-rec` or `plxnative-recplay` is armed.
        app.rec.begin_frame(app.bridge.landgate());
        // No modal dim has been drawn yet this frame, on any route (the player and a bound preview never
        // reach `popover::host::begin_frame`, which used to be the only place this was cleared).
        plx_ui::popover::host::clear_dims();
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
        // The frame dump (`dev::framedump`) owns the clock and the placeholder count from here:
        // it must run before anything reads `clock::now()`.
        #[cfg(all(feature = "hostsim", feature = "devtriggers"))]
        if crate::dev::framedump::requested() {
            crate::dev::framedump::iteration_begin(app);
        }
        plx_platform::tv::window::pump_bus();
        plx_platform::tv::home::poll();
        // The one toast key mode owes the viewer (`net::keypin`'s facts), on every route.
        clock_notice.poll();
        // What the hold hint has taught since last frame, written once to the install's session.
        hint_taught.poll(app.boot_initial.is_some());
        ingest(app, fr);
        app.instr.mark(plx_base::diag::heartbeat::Phase::Ingest); // ingest

        fr.now = clock::now();
        // Share the actual/replay frame timestamp, before spring dt is clamped.
        plx_ui::card_motion::begin_frame(fr.now);
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
        app.rec.content_begin();
        // The `results` phase is four steps (scenarios, playback pump, clock/press, result
        // landing); three of them carry their own FRAMEDROP spans (`scen`, `pump`, `land`), so a
        // slow `results=` names which held the frame.
        let scenarios_ok = plx_base::diag::spans::span("scen", || {
            if app.boot_initial.is_some() {
                crate::dev::scenarios::controlled_each_frame(app, fr)
            } else {
                unsafe { crate::dev::scenarios::each_frame(app, fr) }
            }
        });
        if !scenarios_ok {
            // This iteration presents nothing, so its spans belong to no FRAMEDROP line.
            let _ = plx_base::diag::spans::take();
            continue;
        }
        plx_base::diag::spans::span("pump", || unsafe { playback_tick(app, fr) });
        clock_and_press(app, fr);
        plx_base::diag::spans::span("land", || unsafe { land_results(app, fr) });
        app.instr.mark(plx_base::diag::heartbeat::Phase::Results); // results
        // **Was the player the page on top going INTO this frame?** The container's own commit
        // may take it off during `bridge::frame_with_tap` below, and the detail page it uncovers
        // has to be told to reveal the episode that played. This used to be a disagreement between
        // the tree and the loop's route mirror ("the tree still says player, the route already
        // says home"); with one authority it is an EDGE, read either side of the frame.
        let was_player = super::bridge::player(&app.pages).is_some();
        // **Publish the playback session into the tree** (spec §2.3) before the dispatcher's frame
        // and the draw that follows it, so every owned screen in one frame reads one consistent
        // picture. Only while something that reads it is mounted — see `Bridge::publish_playback`.
        if app.adapters.player.poll_repair(&mut app.player.repair) {
            plx_machine::idle::invalidate();
        }
        // A timed-out native Load parked by teardown is released here, on the main thread, the
        // first frame after its media thread returns (`player::engine::AbandonedLoad`).
        plx_media::player::engine::reap_abandoned_load(&mut app.adapters.player);
        // …and a real stop's off-frame `Unload` gets its main-thread tail (gate, epoch, ACB,
        // D1) the first frame after it returns (`player::engine::PendingRelease`).
        app.adapters.player.poll_release();
        app.player.session.repair_status = app.player.repair.state();
        app.bridge.publish_playback(&app.player.session, was_player);
        // The container runs its frame: the pending navigation's commit (at `PageDip`'s floor),
        // then the owned screens' inputs, ticks, timers and effects. `app/bridge.rs` is the seam.
        let supplied = match app.rec.replay_results(|id| app.bridge.recorded_client(id)) {
            Ok(results) => results,
            Err(reason) => {
                plx_base::eventlog::log(&format!("replay: REFUSED — {reason}"));
                app.running = false;
                break;
            }
        };
        let tick = plx_machine::machine::Tick { ms: fr.now, dt_us: (fr.dt * 1_000_000.0) as u32 };
        if first_controlled_frame { plx_base::eventlog::log(&format!("bootstrap: pre-dispatch dt={} transition={:?} alpha={} flight={}",
            tick.dt_us, app.pages.nav.tabs.stack.transition.commit_point(),
            app.pages.nav.tabs.stack.transition.page_alpha(), app.pages.nav.tabs.stack.transition.in_flight())); }
        app.rec.prepare_resources(&mut app.bridge);
        let (_word, tree_report) = if let Some(results) = supplied {
            let mut stores = std::collections::BTreeSet::new();
            for (address, _) in &results {
                if let plx_machine::machine::MachineId::Store(ord) = address.to {
                    stores.insert(ord.0);
                }
            }
            for ord in stores {
                app.bridge.landgate().landed(plx_machine::machine::StoreOrd(ord));
            }
            super::bridge::frame_with_results(&mut app.pages, &mut app.bridge, tick,
                std::mem::take(&mut app.inputs), || {
                    // One consumed batch per supported store, exactly as the live suppliers
                    // stamp it. No mailbox is polled and no worker is needed for these facts.
                    results
                }, &mut app.rec)
        } else { super::bridge::frame_with_tap(
            &mut app.pages,
            &mut app.bridge,
            plx_machine::machine::Tick {
                ms: fr.now,
                dt_us: (fr.dt * 1_000_000.0) as u32,
            },
            std::mem::take(&mut app.inputs),
            &mut app.rec,
        ) };
        if first_controlled_frame { plx_base::eventlog::log(&format!("bootstrap: post-dispatch alpha={} flight={}",
            app.pages.nav.tabs.stack.transition.page_alpha(), app.pages.nav.tabs.stack.transition.in_flight())); }
        app.rec.resource_requests(app.bridge.take_resource_requests());
        if let Some(reason) = app.rec.failure().or_else(|| app.bridge.controlled_failure()) {
            plx_base::eventlog::log(&format!("replay: REFUSED — {reason}"));
            app.running = false;
            break;
        }
        // **The OWNED pages' share of the underlay verdict.** The three legacy `|=` sites below
        // report the motion of the screens this loop still steps itself; this is the same term
        // for the screens the dispatcher steps, and it was DROPPED for the whole of phase 8 —
        // `frame_with_tap`'s report was bound to `_report` and thrown away. The dispatcher keeps
        // the verdict on its own ledger (`Present::page_moving`, scoped `Page` by default and
        // `Surface` around every modal tick and surface step — §4.4), so it is exactly the term
        // the legacy loop got from stepping `library::update` inside `idle::scoped_motion`.
        //
        // Its host-cache reader is `popover::host::begin_frame` below — `host_refresh`'s `page_moving`
        // term, which decides whether the frozen snapshot under a FADING panel is taken again,
        // and which could not see an owned page move. (It fed `gfx::page_wash_dither` too until
        // 2026-09-19; a page-wide verdict turned out to be the wrong question for a wash, since
        // the wash's own dissolve is in it, and the function itself is gone now — every wash
        // dithers on every frame.)
        fr.underlay_moving |= tree_report.underlay_moving;
        if was_player && super::bridge::player(&app.pages).is_none() { restore_played_entry(app); }
        content_requests(app, fr);
        crate::dev::scenarios::advance_content_boot(app, fr);
        loop_requests(app);
        app.instr.mark(plx_base::diag::heartbeat::Phase::NavCommit); // navcommit
        update(app, fr);
        app.rec.content_results();
        app.instr.mark(plx_base::diag::heartbeat::Phase::TickDrain); // tick_drain
        plx_platform::tv::window::frame_probe_prepare_begin();
        prepare_window(app, fr);
        plx_platform::tv::window::frame_probe_prepare_end();
        present_and_swap(
            app,
            fr,
            #[cfg(all(feature = "hostsim", target_os = "linux"))]
            wslg_frame_budget,
        );
        if fr.present { watchdog.presented(); }
        report(app, fr);
        heartbeat(app, fr);
    }
}

/// **The frame's clock and the click's spring** (spec §3.3 step 3's head).
///
/// `dt` is clamped at 50 ms: a frame that took longer than that is a stall, and integrating one
/// as if it were real time teleports every spring. It is stamped into `plx_machine::idle::frame_begin`
/// BEFORE the update phase re-steps anything, so the motion flag the gate reads at the bottom
/// describes THIS frame and a spring's velocity can be judged as travel-this-frame rather than as
/// a bare units-per-second.
fn clock_and_press(app: &mut App, fr: &mut Frame) {
    if app.boot_initial.is_some() && app.prev == 0 {
        plx_base::eventlog::log(&format!("bootstrap: first-loop tree={:016x} now={}", app.pages.state_hash(), fr.now));
    }
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
    // Whole-frame present gate (`plx_machine::idle`): forget last frame's motion BEFORE the update
    // phase below re-steps every spring, so the flag it leaves describes THIS frame, and
    // stamp `dt` so a spring's velocity can be judged as travel-this-frame rather than as
    // a bare units-per-second. The decision itself is taken just above `glViewport`.
    // A dump's held repeat judges rest with its first pass's `dt` (`framedump::idle_dt`); the
    // clock `now_ms` reads still advances by `fr.dt` alone.
    #[cfg(all(feature = "hostsim", feature = "devtriggers"))]
    let rest_dt = if crate::dev::framedump::active() { crate::dev::framedump::idle_dt(fr.dt) } else { fr.dt };
    #[cfg(not(all(feature = "hostsim", feature = "devtriggers")))]
    let rest_dt = fr.dt;
    plx_machine::idle::frame_begin_judging_rest_with(fr.dt, rest_dt);
    // Is a page capture still in flight on the GPU, and is recorded text still warming? Latched
    // once, before the springs step, so the held appear spring and the present gate below read
    // the same answer — and, being the machine's speed rather than the inputs, recorded or
    // supplied by the recorder like any other environmental observation.
    let text = plx_gfx::text::prewarm_pending();
    plx_gfx::gfx::snapshot_frame_begin(|snapshot| {
        let seen = app.rec.capture_readiness(plx_ui::rec::Readiness { snapshot, text });
        plx_gfx::text::latch_surface_text_pending(seen.text);
        seen.snapshot
    });
    // ui::press (tvOS click) — advance the dip/spring every frame; when a deferred activation
    // commits (the spring-back bounce has played), run it for whichever CARD view armed the
    // press. A long-press does NOT commit (`press::tick` clears `want_commit` at `LONG_MS`):
    // on Home it opens the item menu below, and anywhere else it just springs back.
    let (_, press_moving) = plx_machine::idle::scoped_motion(|| {
        app.input.press.tick(fr.now, fr.dt);
    });
    // The motion of whatever page is UNDER a popover — Home, the Library or Search, since
    // the profile chip is a stop on all three. It was `home_underlay_moving` while only Home
    // could be underneath. The account popover's glass re-snapshots off this.
    fr.underlay_moving = press_moving;
}

/// **The prepare window** (spec §3.3 steps 8-9, §8.1): the frame budget's own frame, the poster
/// adapter's, the player page's clock-motion report, the whole-frame present DECISION, and — on
/// the presenting side of it — the render cache's upload.
unsafe fn prepare_window(app: &mut App, fr: &mut Frame) {
    // ---- the PREPARE WINDOW opens here (spec §3.3 steps 8-9, §8.1) -------------------
    //
    // **The budget's frame opens at the start of this window, not at the top of the
    // iteration**, and that placement is the whole of its ceiling's meaning. `take` admits
    // while `now - frame_start + worst_us <= PREPARE_MAX_US`; opened at the loop top, the
    // elapsed term already contained ingest, the result landings, the nav commit and the
    // tick drain, so on a cold-open frame (~62 ms) every upload was over the ceiling and
    // only the forward-progress escape let ONE through. A quota of three that was really a
    // quota of one, on exactly the frames with the most textures waiting.
    app.pages.budget.begin_frame(plx_base::diag::heartbeat::now_us());
    // The poster adapter's frame (spec §3.3 step 3's tail): a new frame for the slot LRU and
    // every decoded image handed to the render cache as owned pixels. No GL here — the
    // upload is below, on the presenting side of the decision.
    super::adapters::poster::begin_frame();
    super::adapters::poster::drain_decoded();

    fr.player = matches!(app.route(), AppArg::Player);
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
                plx_machine::idle::invalidate();
            }
        }
    }
    // ---- whole-frame present gate (`plx_machine::idle`) --------------------------------------
    // A screen with nothing moving on it does not need to be re-sent to the panel. This
    // skips `glViewport`…`SDL_GL_SwapWindow` WHOLESALE — it is not dirty-RECTANGLE
    // tracking, which `plx_ui`'s `lib.rs` renderer doc rejects: when this says yes, the frame
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
    // **The foreground demand is taken ONCE and has TWO terms** (spec §3.3 step 8): the
    // gate above, and whether the frame budget holds queued prepare work. The second term is what makes
    // the upload step below legal on the presenting side of the decision — a texture waiting
    // to be uploaded is itself a reason to present, so nothing sits in the queue behind a
    // settled screen. It was inert before phase 11: nothing in the product ever published a
    // queue to the budget, so `has_queued_work()` was permanently false and the upload had to
    // run BEFORE the decision (and invalidate) to happen at all.
    plx_ui::tex::note_queued(&mut app.pages.budget);
    // Lifecycle is a hard outer gate: even noidle, a bound plane or queued uploads must
    // not reach EGL/Mali while SDL's window is backgrounded.
    //
    // A third term goes FIRST and short-circuits the other two: a page capture still in flight on
    // the GPU (`gfx::SNAPSHOT_THIS_FRAME`'s doc). Presenting now would only wait a whole vsync for
    // a buffer; not asking `should_present` leaves its damage and motion for the frame that does.
    // The simulator's settled capture (`PLXNATIVE_SHOT_SETTLE`): once nothing has changed for the
    // asked-for quiet, invalidate so the NEXT present is the settled frame and `maybe_capture`
    // takes it. Before the decision below, so that invalidate selects this very frame.
    #[cfg(feature = "hostsim")]
    crate::shot::tick(fr.now, app.pages.budget.has_queued_work() || plx_gfx::gfx::snapshot_pending()
        || app.scenarios.shots.pending());
    fr.present = !plx_gfx::gfx::snapshot_pending()
        && app.window_activity.allow_present(
            plx_machine::idle::should_present(fr.now) || app.pages.budget.has_queued_work(),
        );
    // Read-only: was the fence the only thing standing between this iteration and a present? An
    // inactive window would not have presented either way. (`should_present` is deliberately not
    // asked: it takes-and-clears the damage the deferred frame still needs.)
    fr.deferred = !fr.present && plx_gfx::gfx::snapshot_pending() && app.window_activity.allow_present(true);
    // A dump draws every iteration: a held repeat and a written frame alike are a draw.
    #[cfg(all(feature = "hostsim", feature = "devtriggers"))]
    if crate::dev::framedump::active() {
        fr.present = true;
    }
    app.rec.present(fr.present);
    if fr.present {
        app.window_activity.begin_present(fr.player);
    }
    // ---- step 9's UPLOAD, on the presenting side of the decision ---------------------
    //
    // The render cache's upload step under the frame budget (§3.3 step 9, §10: "a frame that
    // does not present uploads nothing"). Two placement rules, both load-bearing:
    //
    // * **Only on a presenting frame.** The spec's rule: a frame that is skipped wholesale
    //   uploads nothing and the queue waits (`GfxUploader::warm` is `gfx::warm_tex`, which
    //   also draws, so it wants the frame's GL state in place).
    // * **Before the draw.** `warm_tex` draws into its own tiny offscreen target and never
    //   touches framebuffer 0 (the first framebuffer-0 draw of a frame is where the back-buffer
    //   wait lands, and it must stay with the page's `frame_clear`, not with this window), so
    //   there is no pixel to show over the picture — film included — wherever it runs; it stays
    //   here so the upload's cost is bounded by the budget and off the draw.
    if fr.present {
        // A Tracks/More page's text, recorded in `update` or, on the mount frame, in the overlay's
        // `prepare` (`PanelMotion::prewarm_text`), and uploaded here: `fr.present` already carries the window-activity gate, so a frame that
        // does not present, or one while the window is backgrounded, uploads nothing.
        plx_ui::panel_motion::PanelMotion::drain_queued_text(plx_base::diag::heartbeat::now_us);
        let mut ph = plx_machine::machine::PresentHandle::of(&mut app.present);
        super::adapters::poster::prepare(
            &mut app.pages.budget,
            &mut ph,
            plx_base::diag::heartbeat::now_us,
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
    plx_platform::tv::window::opaque_route(app.player.video_plane_bound);
    app.instr.mark(plx_base::diag::heartbeat::Phase::Prepare); // prepare
    // `worstprep=`: the prepare phase is timed on EVERY iteration, presented or not — a
    // settled screen must never run untimed work at the loop rate (spec §8.3).
    app.instr.note_prepare();
    // Hoisted: the frame-drop detector reads these after the gate. Seeded to the pump
    // stamp so a skipped frame reports zero draw/cap/swap rather than a stale delta.
    app.instr.seed_present_phases();
}

/// **The `Rig`-delegated privileged primitives** (spec §15.2, D4). `Bridge`'s `Rig` impl
/// (`app/bridge.rs`) cannot itself move here: a trait's implementation for a type is one
/// syntactic unit, and that `impl Rig<AppHost> for Bridge` block carries two dozen other methods
/// beside these three — so this is the "or just its three privileged methods" half of the D4
/// move. `Bridge::opaque_route`/`Bridge::clear_opaque_region` call these instead of
/// `plx_platform::tv::window::` directly, which is what keeps the OS-facing text — the thing the `frame`
/// gate greps for — in this ONE file rather than split across the loop and the bridge. Each is a
/// pure pass-through: the loop's own per-frame call above (`plx_platform::tv::window::opaque_route`, from
/// `fr`) and the dispatcher's `Rig` hook (from `Bridge`'s own copy of the same bit) are two
/// independent callers of one primitive, not two implementations of it. `ls2_pump` needs no
/// twin here — `Bridge::ls2_pump` is or stays a no-op, since the dispatcher does not yet run a
/// phase this early in the frame.
pub(crate) fn rig_opaque_route(video_plane_bound: bool) {
    plx_platform::tv::window::opaque_route(video_plane_bound);
}

/// See [`rig_opaque_route`]. The `if self.video_plane` guard stays on `Bridge`'s side — this is
/// the OS call alone, exactly what `plx_platform::tv::window::clear_opaque_region` was before the move.
pub(crate) fn rig_clear_opaque_region() {
    plx_platform::tv::window::clear_opaque_region();
}

/// One idle iteration's font-warming slice: a key pressed during it waits at most this long more
/// than the idle poll already makes it wait.
const FONT_WARM_SLICE_US: u64 = 2_000;

/// **Draw, capture, swap — or sleep one frame period** (spec §3.3 step 10). Everything in here is
/// inside the present gate's decision, which `prepare_window` has already taken into `fr.present`.
unsafe fn present_and_swap(
    app: &mut App,
    fr: &mut Frame,
    #[cfg(all(feature = "hostsim", target_os = "linux"))]
    wslg_frame_budget: Option<super::window_activity::WslgFrameBudget>,
) {
    if fr.present {
        // The dump's full prewarm drain and pre-draw debt sample, after the takes and before the draw.
        #[cfg(all(feature = "hostsim", feature = "devtriggers"))]
        if crate::dev::framedump::active() {
            crate::dev::framedump::pre_draw(app);
        }
        // the glyph cache's frame serial (phase 11, text.rs's hot window): a drawn frame
        plx_gfx::text::begin_frame();
        // A finished underlay-field reduction is read HERE, before framebuffer 0 holds anything
        // of this frame — a read between the page and the surfaces splits its render pass.
        plx_gfx::gfx::field_frame_begin();
        let (_vx, _vy, _vw, _vh) = draw(app, fr);
        app.instr.mark(plx_base::diag::heartbeat::Phase::Draw); // draw
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
        app.instr.mark(plx_base::diag::heartbeat::Phase::Capture); // capture
        // `plxnative-simvideo`: the decoded picture goes UNDER the finished UI, as the television's
        // compositor puts its video plane under ours — before the capture, so shots include it.
        #[cfg(feature = "hostsim")]
        if fr.player {
            plx_media::player::sim_video::composite_under();
        }
        // Before the swap, never after: the back buffer is undefined once presented.
        // A dump decides here whether this frame is written (`dev::framedump`); otherwise the
        // ordinary screenshot seam.
        #[cfg(all(feature = "hostsim", feature = "devtriggers"))]
        let dumping = crate::dev::framedump::active();
        #[cfg(all(feature = "hostsim", feature = "devtriggers"))]
        if dumping && crate::dev::framedump::after_draw(app, _vx, _vy, _vw, _vh) {
            app.running = false;
        }
        #[cfg(all(feature = "hostsim", feature = "devtriggers"))]
        let screenshot_seam = !dumping;
        #[cfg(all(feature = "hostsim", not(feature = "devtriggers")))]
        let screenshot_seam = true;
        #[cfg(feature = "hostsim")]
        if screenshot_seam && crate::shot::maybe_capture(_vx, _vy, _vw, _vh) {
            // The headless one-shot is done: finish this frame, then leave through the ordinary
            // shutdown rather than exiting from inside the frame (see `shot::maybe_capture`).
            app.running = false;
        }
        {
            #[cfg(feature = "threadcheck")]
            let _present_scope = plx_base::task::watchdog::present_scope();
            #[cfg(feature = "hostsim")]
            {
                // `present_supersampled` disables the scissor test itself (a clip left armed would
                // crop the window copy) - below gfx's `LIVE_SCISSOR`, which it cannot name. Clearing
                // the clip through gfx first leaves the two in step for the next frame.
                plx_gfx::gfx::clip_clear();
                plx_base::surface::present_supersampled();
            }
            // dev (`/tmp/plxnative-framecb`): this frame's compositor callback, requested before
            // the swap that commits it. One latched bool unarmed.
            plx_platform::tv::window::frame_probe_request();
            SDL_GL_SwapWindow(app.win);
        }
        app.window_activity.presented(fr.player);
        // dev diagnostic, OFF unless `/tmp/plxnative-eglage` was present at boot: re-ask EGL for
        // the back buffer's AGE after real presents have happened (the boot reading is 0 by
        // construction). One null-handle branch per present when unarmed; armed, its one query
        // blocks that present's swap 13-16 ms on the T820 (2026-10-24). See `egl.rs`.
        plx_gfx::egl::late_probe();
        #[cfg(feature = "devtools")]
        {
            app.buffer_flip_count = (app.buffer_flip_count + 1) % 60;
        }
        app.instr.mark(plx_base::diag::heartbeat::Phase::Swap); // swap
        // Inside the gate: `frame_end` is the end of a DRAWN frame. Counting frames the
        // idle gate skipped would pace the profiler's once-per-N-frames log off frames
        // that ran no phases at all.
        plx_ui::profile::frame_end();
        plx_gfx::overdraw::frame_end();
        // Same reason, same gate: the blur's region accounting is per DRAWN frame. It rolls
        // "what every glass surface asked for this frame" into the region the next frame's
        // first snapshot is taken at. Once that union is known, several surfaces share one
        // capture; a first discovery frame may still need a second non-contained grab.
        plx_gfx::gfx::blur_frame_end();
        app.glass.sources.borrow_mut().finish();
        // …and a queued underlay-field reduction has had one more drawn frame to finish in.
        plx_gfx::gfx::field_frame_end();
        // …and a frame that captured the page leaves a fence the next frames wait on.
        plx_gfx::gfx::snapshot_frame_end();
        // …and a ground probe's queued copy is read back here, between frames, if it is done.
        plx_gfx::gfx::ground_probes_frame_end();
        plx_machine::idle::note_present(fr.now);
        // The swap has returned: this frame's interval, for the visit's slow and frozen counts.
        app.instr.frame_presented();
        // The poster-gate scenes' frame counter, at the same post-swap seam as the present count
        // above. It was called from inside `note_present`; `plx_machine::idle` is the machine layer and
        // no longer names `ui`'s metrics.
        #[cfg(feature = "devtriggers")]
        plx_ui::card_motion_metrics::presented(fr.now);
        #[cfg(all(feature = "hostsim", target_os = "linux"))]
        if let Some(budget) = wslg_frame_budget {
            budget.finish();
        }
    } else {
        if fr.deferred {
            // The fence held a present back: the panel kept the old picture for this iteration, so
            // the next presented frame's interval must span it. An idle gap breaks the cadence; a
            // deferral is part of it.
            app.instr.defer_present_phases();
        } else {
            app.instr.skip_present_phases();
        }
        // The one stretch of main-thread time nothing is waiting on: spend a bounded slice of it
        // opening the theme faces and loading their glyph metrics (`text::warm_fonts_idle`), so
        // a page's first layout does not pay for them inside a transition. A no-op once warm.
        plx_gfx::text::warm_fonts_idle(FONT_WARM_SLICE_US, plx_base::diag::heartbeat::now_us);
        // Device and macOS presented frames block in swap; WSLg/X11 presented frames use the
        // software budget above. A skipped frame reaches neither path, so sleep here to keep a
        // settled screen from becoming a CPU spinner.
        SDL_Delay(plx_machine::idle::IDLE_POLL_MS);
    }
}

/// Ingest: the lab command channel, the remote FIFO and the SDL event queue (spec §3.3 step 2).
/// Every key, pointer, text and lifecycle event the frame acts on enters here.
pub(crate) unsafe fn ingest(app: &mut App, fr: &mut Frame) {
        let supplied = controlled_replay(app);
        // Cloud Test Lab has no SSH/FIFO. Its LAB build long-polls outward, then leaves each
        // command here for the SDL thread so the same dispatcher and event queue remain the
        // only input path. Acknowledge acceptance after dispatch, before polling SDL below.
        for command in crate::lab::take_commands() {
            let ok = !supplied && ingress_token(app, fr, &command.token);
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
            if !supplied { let _ = ingress_token(app, fr, &tok); }
        }
        drain_sdl(app, fr);
}

unsafe fn drain_sdl(app: &mut App, fr: &mut Frame) {
    while SDL_PollEvent(app.ev.as_mut_ptr() as *mut c_void) != 0 {
        ingest_sdl_event(app, fr);
    }
}

unsafe fn ingress_token(app: &mut App, fr: &mut Frame, token: &str) -> bool {
    #[cfg(feature = "devtriggers")]
    if let Some(probe) = crate::remote::HangProbe::parse(token) {
        // FIFO, LAB and replay all enter here on the frame thread inside FrameScope.
        // Earlier keys/clicks may still be in SDL: handle and record them before the stall.
        drain_sdl(app, fr);
        app.rec.input(super::recorder::enc_token(token));
        probe.run();
        return true;
    }
    // Keys and clicks use SDL's existing synthesis while coexistence lasts. Consume those
    // before a following direct text token, rather than moving all text ahead of all keys.
    if super::bridge::search_owns_input(&app.pages) { drain_sdl(app, fr); }
    if super::bridge::search_owns_input(&app.pages) {
        if let Some(text) = token.strip_prefix("txt:") {
            ingest_text(app, &text.replace('+', " "), crate::textinput::available(),
                plx_machine::machine::Source::RemoteFifo);
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

fn ingest_text(app: &mut App, text: &str, panel: bool, source: plx_machine::machine::Source) {
    if text.is_empty() { return; }
    let at = plx_machine::machine::Tick { ms: clock::now(), dt_us: 0 };
    app.rec.input(super::recorder::enc_text(text, panel, at, source));
    app.inputs.extend(text_inputs(text, panel, at, source));
    app.last_input = at.ms;
    plx_machine::idle::invalidate();
}

/// Revoke the borrowed SDL window handles on background. `release` answers whether a surface was
/// held; only a real release is a crash-report step, since WILL and DID background both arrive.
fn release_borrowed_window(release: impl FnOnce() -> bool) {
    if release() {
        window_step(plx_telemetry::telemetry::window::Stage::WaylandReleased, None);
    }
}

/// One closed lifecycle step on a native crash event (`window::Stage`): consent-gated and a no-op
/// while the crash backend is off. `playing` is the same flag the WILL/DID edges carry.
fn window_step(stage: plx_telemetry::telemetry::window::Stage, playing: Option<bool>) {
    plx_telemetry::telemetry::window::record(
        plx_telemetry::telemetry::window::Observation::step(stage, playing),
    );
}

/// One polled event. Shared by ordinary polling and ordered FIFO/replay ingestion.
unsafe fn ingest_sdl_event(app: &mut App, fr: &mut Frame) {
    ingest_sdl_event_with_window(app, fr, plx_platform::tv::window::grab);
}

/// The window reacquisition is a platform operation, supplied separately so the
/// recorded/live ingress boundary can be exercised without an SDL/GL context.
unsafe fn ingest_sdl_event_with_window(app: &mut App, fr: &mut Frame,
    restore_window: impl FnOnce(*mut c_void)) {
    let et = rd_u32(&app.ev, 0);
    app.window_activity.event(et);
    let playing = matches!(app.route(), AppArg::Player);
    plx_telemetry::telemetry::window::lifecycle(et, playing);
    // Release health: the foreground edge begins a session (the background edge ends it further
    // down, after the player's park), and a key or pointer event is the activity that keeps one
    // open (`telemetry::session`; consent-gated inside, a no-op while crash reports are off).
    if et == 0x106 { plx_telemetry::telemetry::session::lifecycle(et); }
    if is_input_event(et) { plx_telemetry::telemetry::session::input(); }
    if controlled_replay(app) {
        // The tape owns logical ingress; real SDL startup notifications must not be
        // counted a second time. The real compositor still owns window safety.
        // Only its DID foreground reacquires native handles. Recorded notifications
        // below never reacquire them, revoke grants or resume a native player.
        if et == 0x106 { restore_window(app.win); }
        if matches!(et, 0x103 | 0x104 | SDL_QUIT) {
            app.rec.refuse("live window exit interrupted controlled replay");
            app.running = false;
        }
        return;
    }
    // INPUT while a popover holds the page frozen is the POPOVER's: every invalidate
    // such an event raises — the one below, and whatever its handler adds — is
    // attributed to it, so the frozen host is not re-rendered on every key-up
    // (`popover::host::input_scope`). Only input: a lifecycle or window event is the
    // APP's and may change the page under the panel (backgrounding drops Search's
    // editing layout), so its damage stays the page's and the snapshot is retaken.
    let _own_input = if is_input_event(et) {
        plx_ui::popover::host::input_scope()
    } else {
        None
    };
    // ANY event is a reason to repaint (`plx_machine::idle`): a key changes focus or a label,
    // a lifecycle event changes the whole screen. Marked here — once, for every event
    // kind — rather than in each of the ~30 arms below, where the next one added would
    // silently draw nothing.
    plx_machine::idle::invalidate();
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
        // The visit ends here: nothing is measured in the background, and the app may never come
        // back (the system kills it). The event is only QUEUED to the spool on disk (see
        // `diag::event_for_impl`), so it is sent by the next flush, this process's or the next
        // launch's. The second edge (`DID`) finds an empty visit and emits nothing; the return to
        // the foreground starts a fresh visit of the same screen.
        end_route_visit(app, false);
        app.instr.set_suspended(true);
        log(&format!(
            "LIFECYCLE: background (playing={})",
            matches!(app.route(), AppArg::Player) as i32
        ));
        // A freeze is only reported in the foreground (`watchdog::FREEZE_THRESHOLD_MS`): a parked
        // app may idle or wait on the system for as long as the system keeps it there.
        plx_base::task::watchdog::set_foreground(false);
        // **The TELEVISION'S KEYBOARD goes with the panel, and it is not ours to keep.**
        // The compositor tears its own IME down when it takes the screen away, and it
        // tells the app nothing — so a field left `editing` comes back to the
        // foreground drawing an editing layout and a blinking caret over a keyboard
        // that is gone, and typing is dead in a way no press can recover:
        // `textinput::start` early-returns while its own `STARTED` is set, so OK on the
        // field would toggle our flag and raise nothing. This is the same DISMISSAL a
        // navigation away from Search runs — there is no teardown table any more (D1
        // deleted `nav::leave_of`); the container delivers `WillLeave`/`Unmount`/`Cover`
        // to `SearchScreen`, which drops the keyboard on all three, and the event pushed
        // just below is that same drop asked for directly. Deliberately NOT the commit
        // path `leave_field` takes: the OS moving the screen is not the user saying
        // "that is the search I meant", and a half-typed term must not be filed in
        // their recent searches by an app switch. Unconditional because `EDITING` is
        // this screen's alone and both calls under it are guarded, so it costs a
        // predictable nothing on every other route. Search is unconditionally owned, so
        // there is no legacy fallback left to dispatch to.
        // Keep dismissal after earlier text/keys in this input batch. The dispatcher
        // releases both system ownership and the native start latch at delivery.
        app.inputs.push(plx_machine::machine::InputEvent {
            at: plx_machine::machine::Tick { ms: fr.now, dt_us: 0 }, source: plx_machine::machine::Source::Sdl,
            kind: plx_machine::machine::InputKind::SystemKeyboard(false),
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
        super::bridge::background(&mut app.pages);
        // The notification feed is a live socket the OS will not keep: cancel it at the first edge
        // that says the screen is going (idempotent on the second), reopened by DID foreground.
        app.bridge.notify_suspend();
        // Revoke our borrowed SDL proxies before another frame can use them while backgrounded.
        // SDL owns their lifetime; foreground must query its current window again.
        release_borrowed_window(plx_platform::tv::window::release);
        // A trailer preview is not parked: the OS taking the screen ends it, like any other way
        // off its page (`content::halt_preview_off_its_page`). A Load that has not returned is
        // left Abandoning, and a preview session is never parked for the foreground reload —
        // that reload would bring the trailer back as ordinary playback.
        super::content::halt_preview_now(&mut app.player.session, &mut app.adapters.player);
        // A second background while the foreground restore waits on its flight (the resume's rebuild,
        // an Original rollback): drop the flight — its landing is discarded and only the replacement
        // it registered is stopped — and park the session back where the first suspend left it.
        // The suspend below would otherwise be refused (`awaiting_load`) and the flight would run
        // on, landing a Load on a backgrounded window.
        if super::lifecycle::park_pending_foreground_flight(
            &mut app.player.lifecycle,
            &mut app.player.session,
            &mut app.adapters.player,
        ) {
            plx_machine::idle::invalidate();
        }
        // Playback owns its resolve and Engine before the queued Player page mounts. Page
        // presence only governs the screen-local cleanup below, never resource suspension.
        if !plx_media::route::preview_request(&app.player.session)
            && (plx_media::route::play_pending()
                || app.adapters.player.is_live()
                || plx_media::route::has_url(&app.player.session))
            && !app.player.lifecycle.awaiting_load()
        {
            park_playback(app);
            // The OS took the screen; whatever the pointer was doing, its button-up is never
            // going to arrive here.
            app.ptr.button_down = false;
            // **The PAGE STACK is deliberately not touched**, which is the whole of what a park
            // is: the OS is taking the screen away, not the user navigating, and the foreground
            // arm below reloads straight back into the player. It USED to write
            // `route = Route::Home` here — a statement about the loop's mirror, which
            // `bridge::sync_page` turned into a `Root(Home)` that retired the player entry AND
            // the page it was launched from, so the foreground minted a fresh player over a
            // stack that was just Home. Nothing noticed, because the playback session and
            // `App.play_from` both lived outside the tree; the player's origin is an `EntryId`
            // now, so destroying entries destroys the way back. See
            // `an_app_switch_parks_the_page_stack_and_gives_the_same_entries_back`.
        }
        // The session ends only after the park above: a crash inside the buffer-feed suspend
        // leaves it open, to be reported crashed or abnormal on the next boot.
        plx_telemetry::telemetry::session::lifecycle(et);
        // The DID edge finished its work (the WILL edge is only the notice): a crash after this
        // crumb is not inside the background arm.
        if et == 0x104 {
            window_step(plx_telemetry::telemetry::window::Stage::BackgroundDone, Some(playing));
        }
    } else if et == 0x105 || et == 0x106 {
        // WILL/DID ENTER FOREGROUND
        app.instr.set_suspended(false);
        log(&format!(
            "LIFECYCLE: foreground (wasPlaying={})",
            app.player.lifecycle.awaiting_load() as i32
        ));
        plx_base::task::watchdog::set_foreground(true);
        // The other half of the park (§12.1): `ScreenEvent::Resume` down the tree and
        // `Navigation.suspended` cleared. On BOTH edges, matching the suspend above — webOS sends
        // `will` and `did` and the app is told nothing about which of the two it will get first;
        // `Navigation::resume` is idempotent, so the pair is safe and a lost one is not.
        super::bridge::foreground(&mut app.pages);
        if et == 0x106 {
            // Nothing proves the television is still on the network the person allowed
            // unencrypted connections on: every plaintext grant ends here, and each server that
            // lost one is queued for a fresh discovery (requested by the upgrade retry's frame
            // step) that re-proves eligibility before minting again (`plex::grant`).
            plx_plex::plex::grant::network_changed();
            // …and the same doubt applies to the notification feed's open sockets: drop them, let
            // the feed connect again (a backoff is forgiven). This is no feed gap: `reread.resume()` below
            // re-reads every page, so the reconnect owes no second resync.
            app.bridge.notify_network_changed();
            app.bridge.notify_resume();
            // The television may have been away long enough for every page to be stale.
            app.reread.resume();
            // Reacquire only on DID foreground, before playback restoration and rendering.
            restore_window(app.win);
            plx_machine::idle::invalidate();
            // A decoder-release park is claimed by this edge like any other park.
            app.decoder_parked = false;
            let activation = did_foreground_with_link(app);
            if matches!(activation, ForegroundActivation::Launched) {
                finish_foreground_launch(app);
            }
            window_step(plx_telemetry::telemetry::window::Stage::ForegroundDone, Some(playing));
        }
    } else if et == SDL_KEYDOWN || et == SDL_KEYUP {
        let (state, wcode, sym) = decode_key(&app.ev);
        // The press's IDENTITY, resolved once from the two raw fields
        // (`ui::consts::classify`, which is where the spellings live and where they
        // are tested). `sym` and `wcode` are still read raw by the arms below — the
        // ones that forward them to a screen's own `move_focus`/`key`, and the modal
        // panels and the CH▲/CH▼ pager, which still spell their own key tests.
        let key = classify(sym, wcode);
        if app.boot_initial.is_some() {
            let event = super::bridge::key_input(sym, wcode, state,
                plx_machine::machine::Tick { ms: clock::now(), dt_us: 0 }, plx_machine::machine::Source::Sdl);
            controlled_key_input(app, event);
            return;
        }
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
        let tree_owns_key = super::bridge::owns_input(&app.pages);
        // `clock::now()`, not `fr.now`: INGEST runs before the frame stamps its own time
        // (see `run`'s phase order), so `fr.now` is still 0 here — the same reason the
        // ladder below reaches for the clock to fill `app.last_input`. `dt_us` is 0
        // because an event's `at` is a stamp, not a timestep: every machine the
        // dispatcher steps is driven by the FRAME's tick, which `bridge::frame` supplies.
        let tree_tick = plx_machine::machine::Tick {
            ms: clock::now(),
            dt_us: 0,
        };
        if (state & 0xff) != 1 {
            if tree_owns_key {
                app.inputs.push(super::bridge::key_input(sym, wcode, state, tree_tick, plx_machine::machine::Source::Sdl));
            }
            // …and the loop's own key-up bookkeeping runs either way: it retires the sym
            // from the physically-down slot and releases a deferred press, both of which are
            // state about the PHYSICAL key rather than about whoever read it. Skipping it
            // while a surface is up would leave a key the ladders never saw go down looking
            // held the moment the surface closes.
            //
            // **It no longer touches the scrub** (restructure phase 12, PX-PLAYER). It used to
            // commit, cancel or debounce `PlayerScreen::scrub` from here — a field the page
            // itself has owned since it began answering `HitSource::Engine`, and which arms its
            // own `TAP_COMMIT_MS` debounce on the very same `Edge::Up` — so one tap of RIGHT
            // issued two seeks. The publish that followed went with it: nothing here moves a
            // published field any more.
            on_key_up(sym, app.ok_armed, &mut app.down_sym, &mut app.input.press);
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
                    app.inputs.push(super::bridge::key_input(sym, wcode, state, tree_tick, plx_machine::machine::Source::Sdl));
                }
                return;
            }
            // All that is left of the legacy repeat path is the deferred press's liveness
            // beat: the player's continuous scrub, its last direct consumer, engages from
            // `PlayerScreen`'s own `Edge::Repeat` (phase 12, PX-PLAYER).
            on_auto_repeat(sym, app.ok_armed, &mut app.input.press);
            return;
        }
        // From here down this IS a fresh press, whatever the driver stamped on it.
        app.last_input = clock::now();
        // The press that pays for a decoder-release restore also says which clock it restores
        // (`wake_input`): PLAY plays, the PLAYPAUSE toggle plays a park saved Paused, and every
        // other key — OK included — is only a wake and keeps the saved clock.
        let wake = match key {
            Key::Play => WakePress::Play,
            Key::PlayPause => WakePress::Toggle,
            _ => WakePress::Wake,
        };
        if claim_press_after_decoder_release(app, wake) {
            // `down_sym` is unconditional for every fresh press (`begin_fresh_press`'s own
            // doc), and this press never reaches that function: skipping it here left a key
            // still physically held after the restore read as a FRESH press on its next
            // hardware auto-repeat, firing the input ladder for real a tick after the restore.
            app.down_sym = sym;
            return;
        }
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
            app.inputs.push(super::bridge::key_input(sym, wcode, state, tree_tick, plx_machine::machine::Source::Sdl));
            return;
        }
        // `Route::Login | Route::Profiles` is deliberately absent here (phase 6, mirroring
        // `Route::Onboard`'s own removal in 5b): both are OWNED screens now, so a key on
        // either was already taken by `tree_owns_key` above and never reaches this chain.
        // (`Route::ItemMenu`'s arm stood here, above every route-scoped one — it was MODAL, so it
        // took every key. The item menu is a `ModalStack` surface since phase 10, so its keys were
        // already claimed by `tree_owns_key` well above this chain and it can never be reached.)
        // (The failure read-out's own key arm stood here — OK to the quality ladder, BACK out of
        // the playback, everything else swallowed, and its guard deliberately subsuming every
        // player arm below it. It was already unreachable when phase 12 found it, having grown a
        // `!matches!(app.route, Route::Player)` term that can never hold beside the
        // `Route::Player` test above it; what it was FOR is `PlayerScreen::handle_key`'s own
        // `transport_hidden` gate, which asks `failed_key_action` — the same pure policy — before
        // any ordinary transport arm.)
        // (Four arms stood here — the track menu, the `…` popover, the Info card and the
        // Chapters strip, each gated on `overlay_swallows_key` so that a transport key FELL
        // THROUGH to the ordinary Pause/Play arms below while everything else was swallowed. The
        // panels are SURFACES on the player page's own `ModalStack` since restructure phase
        // 9, so `tree_owns_key` at the top of this chain has already handed the key to the one
        // that is open and returned; the fall-through, which a surface cannot perform, is
        // `PlayerReq::Transport` — see `screens::player::overlay`'s module doc.)
        // (The player's UP/DOWN arm stood here. It walked the HUD ring on the mounted
        // `PlayerScreen`'s own `hud`/`scrub` — which the page also does, from `key_updown`, for the
        // same key — and was unreachable behind `tree_owns_key` above from the moment the page
        // answered `FocusSource::Engine`. Deleted in phase 12, PX-PLAYER.)
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
        if !matches!(app.route(), AppArg::Player)
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
                &mut app.ptr,
                &mut app.ok_armed,
                &mut app.input.press,
                &mut app.pages,
                &mut app.bridge,
            );
        } else if matches!(key, Key::Pause) {
            key_pause(&mut app.adapters.player, app.last_input, &mut app.pages);
        } else if matches!(key, Key::Play) {
            key_play(&mut app.player.session,
                &mut app.adapters.player,
                app.last_input,
                &mut app.player.lifecycle,
                &mut app.repause_at,
                &mut app.ptr,
                &mut app.pages,
                &mut app.bridge,
            );
        } else if matches!(key, Key::PlayPause) {
            // ONE key, both directions. `key_play`/`key_pause` are each half of the
            // toggle, so this arm picks; off the player route `key_play` is what starts
            // playback, which is the right answer for a PLAYPAUSE press on a card.
            if paused() || super::bridge::player(&app.pages).is_none() {
                key_play(&mut app.player.session,
                    &mut app.adapters.player,
                    app.last_input,
                    &mut app.player.lifecycle,
                    &mut app.repause_at,
                    &mut app.ptr,
                    &mut app.pages,
                    &mut app.bridge,
                );
            } else {
                key_pause(&mut app.adapters.player, app.last_input, &mut app.pages);
            }
        } else if matches!(key, Key::Exit) {
            // The remote's EXIT key — LG's checklist item 38 wants the app terminated,
            // and unlike BACK at Home's root there is nothing ambiguous about a key
            // labelled EXIT, so unlike BACK it really does end the process — and it
            // is now the only key that does.
            log("EXIT key: terminating");
            app.running = false;
        // (Two player arms stood here — STOP's `exit_player`, and LEFT/RIGHT's `key_scrub`. Both
        // are `PlayerScreen`'s: `Key::Stop` asks for `PlayerReq::Exit` and reaches the very same
        // ritual through `player_requests`, and the scrub ladder is the page's three edges. Their
        // route terms were two of the four that made this chain's ORDER behaviour, which is why
        // they are recorded rather than silently dropped. Phase 12, PX-PLAYER.)
        } else if matches!(key, Key::Back) {
            key_back(&mut app.player,
                &mut app.adapters.player,
                &mut app.repause_at,
                &mut app.refresh_hubs,
                &mut app.pages,
            );
        }
    } else if et == SDL_MOUSEMOTION {
        app.last_input = clock::now();
        app.ptr.last_motion = app.last_input;
        if app.ptr.cur_hidden {
            show_cursor_after_motion();
        }
        app.ptr.cur_hidden = false;
        let (mx, my) = ptr_xy(&app.ev);
        if app.boot_initial.is_none() {
            app.rec.input(super::recorder::enc_pointer("pointer", mx as i32, my as i32));
        }
        if app.ptr.prev_mx >= 0.0 {
            app.ptr.mot_accum += (mx - app.ptr.prev_mx).abs() + (my - app.ptr.prev_my).abs();
        }
        app.ptr.prev_mx = mx;
        app.ptr.prev_my = my;
        // **The BARE transport's pointer, handed to the page that owns it** (phase 12,
        // PX-PLAYER). This arm used to raise the HUD and move the scrub preview HERE, on the
        // screen's own fields, and then `return` — so the player page never saw a pointer event
        // at all, and the drag preview kept reading a flag whose producer had become unreachable.
        // Both halves are `PlayerScreen`'s now; what is left is the choice of EVENT.
        //
        // It stays AHEAD of the D-pad accumulation gate below, deliberately and as it always was:
        // over full-screen video a mouse nudge is how the transport is found, and 120 px of
        // accumulated travel is not what that costs. (`ui::hit`'s own dpad gate still suppresses
        // hover-driven focus underneath, which is the half that rule is actually about.)
        //
        // A button held down makes this a DRAG rather than a move — §7.5, the one pointer kind
        // that drives a control with no hover and no click.
        if matches!(app.route(), AppArg::Player)
            && !app.pages.surface_up()
            && super::bridge::owns_input(&app.pages)
        {
            let at = plx_machine::machine::Tick { ms: app.last_input, dt_us: 0 };
            app.inputs.push(if app.ptr.button_down {
                super::bridge::drag_input(mx, my, at)
            } else {
                super::bridge::pointer_input(mx, my, at)
            });
            return;
        }
        if app.ptr.dpad_mode {
            if app.ptr.mot_accum < super::input::DPAD_MOTION_GATE {
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
        if super::bridge::owns_input(&app.pages) {
            // `app.last_input`, stamped from the clock at the top of this arm: `fr.now`
            // is not written until after ingest (`tree_tick` above says why).
            app.inputs.push(super::bridge::pointer_input(
                mx,
                my,
                plx_machine::machine::Tick { ms: app.last_input, dt_us: 0 },
            ));
            return;
        }
        // `Route::Profiles | Route::Search | Route::ItemMenu` are deliberately absent (phases
        // 6/7/10): all three are owned screens or surfaces now, so their hover was already taken
        // by `owns_input()` above.
    } else if et == SDL_MOUSEBUTTONDOWN {
        // OK on a set whose pointer the app can only make transparent: see `click_is_ok`. Pushed
        // as an OK key-down, so it reaches the focused control by the key path.
        let on_control = {
            let (cx, cy) = ptr_xy(&app.ev);
            app.pages.input.hit.top_at(cx, cy).is_some()
        };
        let as_ok = super::input::click_is_ok(
            app.ptr.dpad_mode,
            app.ptr.mot_accum,
            on_control,
            unsafe { super::boot::cursor_is_transparent() },
        );
        if as_ok {
            app.ptr.ok_from_click = true;
            super::events::remote_synth_key_edge(SDLK_RETURN, 0, true);
            return;
        }
        app.last_input = clock::now();
        {
            let (cx, cy) = ptr_xy(&app.ev);
            if app.boot_initial.is_none() {
                app.rec.input(super::recorder::enc_pointer("click", cx as i32, cy as i32));
            }
        }
        // The button is DOWN from here until its up, which is the whole of what the pointer
        // machine now records about it: motion in that window is a DRAG (§7.5) rather than a
        // move, and the idle-cursor hide below leaves a held pointer alone.
        app.ptr.button_down = true;
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
        // A click on NO control is the player's play/pause toggle, so it wakes a paused park
        // Playing (`wake_input`); a click on a control (the scrub bar, a disc, a tab) is that
        // control's, which a paused park keeps drawn, so it is only a wake. The OK key a set
        // delivers as a click never gets here: `click_is_ok` above re-sent it as OK, which is
        // only a wake too.
        let press = if on_control { WakePress::Wake } else { WakePress::Toggle };
        if claim_press_after_decoder_release(app, press) {
            return;
        }
        // Rule 11's click half, and the same one arm the hover path takes. The three it
        // replaces each spelled out the two activation shapes by hand — a control FACE
        // dips and commits on the spring-back, a table row commits on the button-down —
        // which is exactly what `ElemKind` says once, per element, for every owned screen.
        if super::bridge::owns_input(&app.pages) {
            let (cx, cy) = ptr_xy(&app.ev);
            app.inputs.push(super::bridge::click_input(
                cx,
                cy,
                plx_machine::machine::Tick { ms: app.last_input, dt_us: 0 },
            ));
            return;
        }
        // (The player's two click blocks stood here — the failure read-out's `failure_quality_hit`
        // band, and the transport's own `icon_hit`/`scrub_hit` ladder with the control row's
        // deferred press, the scrub-band seek and the fall-through play/pause toggle. Every one of
        // those regions is a registered `Stop` now (`PlayerScreen::draw`), resolved by the shared
        // hit map and answered by `PlayerScreen::handle_click`, which is what closed
        // `ci/check-deps.sh`'s `hittest` gate: the three raw hit-tester calls this block made were
        // the only ones in `app/`. They had also been unreachable since the page began answering
        // `HitSource::Engine` — the `owns_input()` early return above claims the click first —
        // which is exactly the shape phase 12 exists to end: a live path and a dead one on one
        // piece of state. Phase 12, PX-PLAYER. This chain's head was that block's
        // `matches!(app.route, Route::Player)`, so what follows is now the head itself.)
        // **A click that gets past `owns_input()` lands on nothing.** The shared bar's profile chip and
        // pills are stops in the hit map like any other control (`Dispatcher::draw` registers them at the
        // place the bar is DRAWN, tucked or not), so the arm above claims their clicks for every mounted
        // page; the old `chip_clicked` ladder here tested the chip's RESTING rect against the route
        // alone, so it opened the account menu over a bar that was lifted away or not drawn. A page
        // whose body has not mounted yet draws no bar, so there is nothing here for a click to press.
        //
        // (`Route::Profiles`, `Route::Login`, `Route::Search` and `Route::ItemMenu` once had arms in
        // this chain; each is an owned screen or a surface now, so `owns_input()`'s arm above takes
        // its clicks. The menu's panel in particular is why the container answers a click off a
        // surface by construction: a click cannot fall through onto the shelf under it.)
    } else if et == SDL_MOUSEBUTTONUP {
        if std::mem::take(&mut app.ptr.ok_from_click) {
            super::events::remote_synth_key_edge(SDLK_RETURN, 0, false);
            return;
        }
        app.last_input = clock::now();
        {
            let (cx, cy) = ptr_xy(&app.ev);
            if app.boot_initial.is_none() {
                app.rec.input(super::recorder::enc_pointer("release", cx as i32, cy as i32));
            }
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
        app.inputs.push(super::bridge::release_input(plx_machine::machine::Tick {
            ms: app.last_input,
            dt_us: 0,
        }));
        // …and the button is up, which is all this arm still says. Committing the player's drag
        // from here was the other half of the split ownership phase 12 retires: the release above
        // reaches `PlayerScreen::handle_release` as the `Ok`/`Edge::Up` the tree already gets, and
        // the page commits its own preview with `PlayerReq::CommitSeek`.
        app.ptr.button_down = false;
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
            if super::bridge::search_owns_input(&app.pages) {
                app.inputs.push(plx_machine::machine::InputEvent {
                    at: plx_machine::machine::Tick { ms: app.last_input, dt_us: 0 },
                    source: plx_machine::machine::Source::Sdl,
                    kind: plx_machine::machine::InputKind::Wheel { dy: dy as f32 },
                });
            } else if super::bridge::owns_input(&app.pages) {
                // A tick becomes the DIRECTION KEY it stands for (`bridge::wheel_input`),
                // rather than a `Wheel` the family's tables would each have to interpret:
                // one tick is one row, which is what `on_updown(±1)` meant. `RepeatGate`
                // is deliberately NOT applied — a wheel gesture's ticks are the user's own
                // cadence and the gate is armed against the remote's 50 ms hardware
                // repeat, so sharing it would let a held key mute a scroll and vice versa.
                app.inputs.extend(super::bridge::wheel_input(
                    dy,
                    plx_machine::machine::Tick { ms: app.last_input, dt_us: 0 },
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
        if super::bridge::search_owns_input(&app.pages) {
            let text = crate::textinput::decode(&app.ev);
            ingest_text(app, &text, crate::textinput::available(), plx_machine::machine::Source::Sdl);
        } else {
            crate::textinput::on_event(&app.ev);
        }
    }
}

/// Park the playing session for a later restore at the same spot: save its intended position and
/// clock, drop the Engine, keep the session and the page stack. An app switch does this, and so
/// does the pipeline taking the video decoder back ([`park_on_decoder_release`]); the restore is
/// the foreground lifecycle's claim either way.
fn park_playback(app: &mut App) {
    // INTENDED, not published: this snapshot is the only thing the foreground
    // restore has, and `suspend_bufferfeed` below drops the pending seek target
    // with the session — so a background that lands while a seek is still
    // resolving would otherwise save (and restore to) the spot the user just
    // seeked AWAY from, with nothing left to correct it. See `intended_pos`.
    let saved_ns = intended_pos(&mut app.player.session);
    // The transport is read HERE, as the viewer sees it (a claim's presentation hold keeps a pause
    // that is the hold's loan, not the viewer's): the machine reads no global.
    let _ = app.player.lifecycle.step(
        &ForegroundEvent::Park { saved_ns, viewer_paused: viewer_paused() },
        &mut Vec::new(),
    );
    // The session is preserved for the foreground reload, so
    // this is the RELOAD half of the reset — the in-flight gesture goes, the transport's
    // timer and the countdown stay. It is spelled as a delivery to the owner because
    // `player::TX::reset_for_reload` no longer clears `scrub_ns` from under it (§2.3).
    if let Some(player) = super::bridge::player_mut(&mut app.pages) {
        player.transport_reset(true);
    }
    close_player_overlays(&mut app.pages);
    plx_media::player::suspend_bufferfeed(&mut app.player.session, &mut app.adapters.player);
    // Also retire the resolve generation: its worker may finish after backgrounding,
    // including before the accepted start's Player page has ever mounted.
    plx_media::route::cancel_play(&mut app.player.session);
}

/// **The pipeline took the video decoder back** (`player::take_decoder_revoked`): park the session
/// as an app switch would, and leave the restore to the viewer's next press
/// ([`claim_press_after_decoder_release`]). Measured 2026-10-13 on the dev set: a paused direct play
/// left until the screensaver came on lost its decoder with NO `LIFECYCLE: background`, and the
/// dead pipeline refused every Play after it, so the only way back was closing the app. Not
/// rebuilt at once: whatever took the decoder (the screensaver) may still hold it, and a Load it
/// refuses ends on the failure read-out.
fn park_on_decoder_release(app: &mut App) {
    if !plx_media::player::take_decoder_revoked() {
        return;
    }
    if plx_media::route::preview_request(&app.player.session) {
        // A trailer is never parked (an app switch ends it too): restoring it would bring the
        // trailer back as ordinary playback.
        super::content::halt_preview_now(&mut app.player.session, &mut app.adapters.player);
        return;
    }
    if !app.adapters.player.is_live() || app.player.lifecycle.awaiting_load() {
        return;
    }
    log("player: decoder released by the pipeline — parked until the next press");
    park_playback(app);
    app.decoder_parked = true;
    plx_machine::idle::invalidate();
}

/// What the press that wakes a decoder-release park asks of the transport, as far as the restore
/// is concerned ([`wake_input`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WakePress {
    /// The remote's PLAY key.
    Play,
    /// A play/pause toggle: the PLAYPAUSE key, or a pointer click that hits no control (on the
    /// player only that click is the toggle, `PlayerScreen`'s pointer arm).
    Toggle,
    /// Any other press, PAUSE, OK and a click on a control included: OK is also how a viewer
    /// dismisses the screensaver, and whether that press reaches the app is unmeasured, so it
    /// never starts the video.
    Wake,
}

/// The claim a decoder-release wake press makes, given the clock the park saved
/// ([`ForegroundLifecycle::claimable_clock`]):
///
/// | press  | saved Paused             | saved Playing            |
/// |--------|--------------------------|--------------------------|
/// | Play   | `PlayKey`: Playing       | `PlayKey`: Playing       |
/// | Toggle | `PlayKey`: Playing       | `DidForeground`: Playing |
/// | Wake   | `DidForeground`: Paused  | `DidForeground`: Playing |
///
/// `PlayKey` restores a paused save at its exact offset (no resume rewind). A toggle on a playing
/// save means pause, but the machine has no claim that restores a playing save Paused: that press
/// is spent as a wake, and the viewer's next one pauses. The `Wake` row is every other press: PAUSE
/// too, which over a playing save is spent the same way, and a click on a control.
fn wake_input(press: WakePress, saved: Option<ForegroundClock>) -> ForegroundInput {
    match (press, saved) {
        (WakePress::Play, _) | (WakePress::Toggle, Some(ForegroundClock::Paused)) => {
            ForegroundInput::PlayKey
        }
        _ => ForegroundInput::DidForeground,
    }
}

/// The first key or click after [`park_on_decoder_release`] rebuilds the picture at the saved spot,
/// through the same claim the DID foreground edge makes. The press is spent on that, like a press
/// that finds the HUD hidden: the viewer cannot see what it would have acted on — except for the
/// clock, which [`wake_input`] reads off `press`: a press that asks to play brings a paused save
/// back Playing, any other keeps the saved clock. `false` when nothing is parked this way.
///
/// **A surface gets first refusal.** `park_on_decoder_release` parks without asking whether
/// something other than the player owns input, because the dead pipeline has to be dropped either
/// way; this is the one check that matters for INPUT. If a `ModalStack` surface (Settings, a track
/// menu, …) is open over the parked player, its press is its own — stealing it for the restore
/// would answer a key the viewer aimed at the surface. The park stays pending (not stale: only a
/// superseded session below clears it) for a later press with nothing in front of it.
fn claim_press_after_decoder_release(app: &mut App, press: WakePress) -> bool {
    if !app.decoder_parked {
        return false;
    }
    if !app.player.lifecycle.awaiting_load() {
        app.decoder_parked = false;
        return false;
    }
    if app.pages.surface_up() {
        return false;
    }
    app.decoder_parked = false;
    let input = wake_input(press, app.player.lifecycle.claimable_clock());
    let activation = drive_foreground(
        &mut app.player.lifecycle,
        &mut app.player.session,
        input,
        &mut PlayerForegroundActuator {
            pa: &mut app.adapters.player,
            repause_at: &mut app.repause_at,
        },
    );
    if matches!(activation, ForegroundActivation::Launched) {
        finish_foreground_launch(app);
    }
    plx_machine::idle::invalidate();
    true
}

/// The warm-launch DID composes its two children in order: the launch link sees the arrival
/// first, and its output picks the lifecycle's input. The parent reads no child's state: a link
/// that ends playback (`leaves_playback`, true only when its step emitted `PlayerReq::Exit`)
/// makes the DID claim nothing, so the parked film is not restored just to be stopped; the Exit,
/// drained at the next frame, retires it (`Superseded(Exit)`).
fn did_foreground_with_link(app: &mut App) -> ForegroundActivation {
    let now = clock::now();
    let stands = link_session_stands(app);
    let (event, env) = super::deeplink::link_inputs(
        &app.pages, app.window_activity.allow_present(true), stands, now);
    let out = app.deeplink.step(event, &env);
    let input = if out.leaves_playback {
        ForegroundInput::DidForegroundSuperseded
    } else {
        ForegroundInput::DidForeground
    };
    let activation = drive_foreground(
        &mut app.player.lifecycle,
        &mut app.player.session,
        input,
        &mut PlayerForegroundActuator {
            pa: &mut app.adapters.player,
            repause_at: &mut app.repause_at,
        },
    );
    super::deeplink::perform(out, &mut app.pages, &mut app.menu_play_await, now);
    activation
}

/// The facts of whether a playback session stands, for the launch link's step
/// (`deeplink::session_stands`).
fn link_session_stands(app: &App) -> bool {
    super::deeplink::session_stands(
        super::bridge::player(&app.pages).is_some(),
        app.player.lifecycle.awaiting_load(),
        app.adapters.player.is_live(),
        plx_media::route::is_preview(&app.player.session),
    )
}

/// A foreground Load launched — straight from the DID edge, or from the frame that landed a flight
/// the restore was waiting on (`poll_foreground_flight`): mount the player and pin its transport.
fn finish_foreground_launch(app: &mut App) {
    // The park kept the page, so this is ordinarily a no-op; it is written out
    // because a foreground that finds the player gone has to put it back rather
    // than resume a session with nothing on screen.
    super::bridge::show_page(&mut app.pages, AppArg::Player);
    // A live page pins its own transport from the instant it mounts
    // (`AppMounter::player_hud_ms`); a live one is re-pinned here, which is the
    // foreground half of `start_playback`'s two paths.
    app.bridge.seed_player_hud(HUD_LINGER_MS);
    if let Some(player) = super::bridge::player_mut(&mut app.pages) {
        player.hud.extend(clock::now(), HUD_LINGER_MS);
        player.publish();
    }
}

/// Permission to advance playback is independent of the retained page stack. WILL foreground
/// does not reopen WindowActivity; a parked session must first claim its foreground Load.
fn playback_may_run(app: &App) -> bool {
    app.window_activity.allow_present(true) && !app.player.lifecycle.awaiting_load()
}

/// The playback side of an iteration before the tick: the engine pump, the foreground load
/// poll, the reporter, EOS / Up Next, the hub refresh, the held key, the scrubber and the paused
/// seek — everything that reads `fr.now` and no `dt`.
pub(crate) unsafe fn playback_tick(app: &mut App, fr: &mut Frame) {
        park_on_decoder_release(app);
        if playback_may_run(app) {
            if is_started() {
                plx_media::player::pump(&mut app.player.session, &mut app.adapters.player, fr.now);
            }
            // Outside `is_started`: a refused preview landing or a stale machine has no engine,
            // and is exactly what must still be settled back to Idle.
            plx_media::player::preview::after_pump(
                &mut app.player.session,
                &mut app.adapters.player,
                fr.now,
            );
        }
        // **The ONE place the Player machine is asked whether the hardware video plane is bound**
        // (spec §9), immediately after the pump that advances the ACB bind transaction and BEFORE
        // the container tree's frame, so the whole iteration reads one answer. Outside the
        // `is_started` guard on purpose: a stop retires the engine and `started` with it, and the
        // FALSE edge is exactly the one nobody would otherwise carry.
        //
        // Everything downstream READS `app.player.video_plane_bound`; nothing recomputes it, and
        // nothing keys the plane's consequences on the route any more.
        if let Some(bound) = plx_media::player::observe_video_plane(&mut app.player, &mut app.adapters.player) {
            // One edge, three gates. `set_video_plane_bound` has already told the LIVE one
            // (`plx_machine::idle`); these are the two §4.4 machines — the one `App` owns and the
            // dispatcher's, which is what answers `Rig::opaque_route` at step 9 — plus the rig's
            // own copy for the draw-entry call. `PresentEvent::VideoPlane` has no other source.
            app.present.note(plx_machine::present::PresentEvent::VideoPlane(bound));
            app.pages.present.note(plx_machine::present::PresentEvent::VideoPlane(bound));
            app.bridge.publish_video_plane(bound);
        }
        // A late landing of a flight nobody waits on any more (a Back, a suspend, a newer request
        // ended it while its worker ran) is reaped here, every frame: a flight with no Engine has no
        // pump to do it, and the landing's replacement encoder runs on the server until it is.
        plx_media::route::discard_stale_flight_landing();
        if app.window_activity.allow_present(true) && app.player.lifecycle.flight_pending() {
            // The foreground restore waits on a flight (its resume's rebuild or an Original
            // rollback): the lifecycle machine drains it, not the pump (there is no Engine) and not
            // the cold-resume drain (`playback_may_run` is false while the restore is parked).
            let activation = poll_foreground_flight(
                &mut app.player.lifecycle,
                &mut app.player.session,
                &mut PlayerForegroundActuator {
                    pa: &mut app.adapters.player,
                    repause_at: &mut app.repause_at,
                },
            );
            plx_machine::idle::invalidate();
            if matches!(activation, ForegroundActivation::Launched) {
                finish_foreground_launch(app);
            }
        } else if playback_may_run(app) {
            // An Original rollback no foreground machine owns (`start_original_trial_reload`'s
            // Load failed before an Engine existed): its landing reloads onto the restored HLS.
            plx_media::player::drain_engineless_rollback(&mut app.player.session, &mut app.adapters.player);
        }
        if playback_may_run(app) {
            let _ = poll_foreground_load(
                &mut app.player.lifecycle,
                &mut app.player.session,
                &mut PlayerForegroundActuator {
                    pa: &mut app.adapters.player,
                    repause_at: &mut app.repause_at,
                },
            );
        }
        // **Unconditional, and NOT inside the `is_started` block above.** `player::state()`
        // derives two of its answers outside the pump entirely — `Resolving` while a plan is in
        // flight, and `Error` for a `/decision` refusal, which happens before an engine exists —
        // so gating this on a started engine would silently miss the earliest and most certain
        // failure there is. It observes the value the HUD renders and reports only transitions,
        // so the steady-state cost is one atomic load.
        plx_media::player::report::tick(&mut app.player.session);
        // end-of-stream: the pipeline drained at the credits → hand off to Up Next when the
        // show has another episode queued and the Next episode preference is not Off, else leave
        // the player (back to the detail page or home, whichever is behind), instead of freezing
        // on the last frame.
        if playback_may_run(app) && super::bridge::player(&app.pages).is_some() && plx_media::player::ended() {
            let handed_off_to_up_next = finish_playback(&mut app.player,
                &mut app.adapters.player,
                &mut app.repause_at,
                &mut app.refresh_hubs,
                &mut app.pages,
                &mut app.bridge,
            );
            // dev: REPLAY AFTER COMPLETION (#46) — `crate::dev::scenarios::maybe_replay_after_eos`.
            // `finish_playback`'s return, not `app.route()`: `exit_player`'s `PopTo` only PARKS
            // the navigation (applied at the next commit), so the route still reads `Player` here
            // even on a real exit — the return value is the only live signal for it this frame.
            crate::dev::scenarios::maybe_replay_after_eos(app, handed_off_to_up_next);
        } else if playback_may_run(app)
            && plx_media::player::ended()
            && super::content::off_route_orphan(
                app.adapters.player.is_live(),
                plx_media::route::is_preview(&app.player.session),
                super::bridge::player(&app.pages).is_some(),
            )
        {
            // The same end with no player page to leave: nothing else would ever stop this
            // engine, and its reporter would keep posting the final position forever.
            log("EOS: stopping an off-route engine nothing owns");
            plx_media::player::stop_bufferfeed(&mut app.player.session, &mut app.adapters.player);
        }
        // Up Next countdown elapsed → start the queued episode on its own. Beside the EOS
        // handoff so the whole auto-advance chain reads in one place.
        if playback_may_run(app)
            && super::bridge::player(&app.pages).is_some_and(|p| p.up_next.expired(fr.now))
        {
            if !play_up_next(&mut app.player.session,
                &mut app.adapters.player,
                HUD_LINGER_MS,
                &mut app.pages,
                &mut app.bridge,
            ) {
                if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                    player.up_next.cancel(); // nothing queued after all — don't re-fire
                }
            }
        }
        // The return hold (`playback::ReturnHold`): armed for an exit that reported a stop, ended
        // by a fresh key press or click. That press is NOT delivered this frame: the page that
        // would take it is still the player (input is step 2 of the frame, the dip crosses its
        // floor at step 4), so the whole batch goes to Home on the next frame.
        super::playback::return_hold_inputs(app, fr.now);
        // post-playback home refresh (armed by every exit_player): refetch the hubs so
        // Continue Watching shows the new resume point / next episode. It waits for the stop
        // report to have reached the server (`timeline_stop_landed`), not for a clock: a read that
        // lands before the `stopped` timeline POST is the stale resume point, and a slow share
        // makes that window seconds wide. It is owed once per exit and held while the player is
        // on top — except on the return to Home, where the dip waits at its floor for this very
        // read (`CW_RETURN_HOLD_MS`) and the route still reads Player until it crosses. The request
        // is worker-only; the landing logs the resulting item count when it actually commits.
        if app.refresh_hubs
            && (!matches!(app.route(), AppArg::Player) || app.pages.nav.tabs.stack.transition.floor_held())
            && plx_media::route::timeline_stop_landed()
        {
            app.refresh_hubs = false;
            app.return_hold.refetch_issued();
            super::bridge::execute_endpoint_outcomes(
                &mut app.pages,
                app.bridge.hubs_run(plx_data::stores::hubs::HubsCmd::RefetchHubs).endpoints,
            );
            // …and every library's OWN shelves, for the same reason and at the same moment:
            // a finished playback moves Continue Watching and watch state, so invalidate each
            // Bridge-owned Browse store through `Bridge::browse_run`.
            app.bridge.browse_run(plx_data::stores::browse::BrowseCmd::HubsInvalidateAll);
            log("home: hubs refresh queued after playback");
        }
        // The return hold ends when the refetch it waits for has SETTLED for the played item's
        // source: issued (the owed flag is spent) and that source's read taken, whether it brought
        // a catalog or failed (a failure keeps the content and moves no generation, so the
        // generation is not the signal). A refused launch is settled by the next Hubs step, one frame later. Nothing here can extend
        // the hold: the bound is the dip's own, on the frame clock.
        super::playback::return_hold_settle(app, fr.now);
        // What quiet reads staged for Home lands here when the store says nothing in view moves
        // (`HubsCmd::Publish`); with nothing staged this is a no-op that raises no notice.
        super::content::publish_staged_hubs(app);
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
        // (The scrub gesture's per-frame block stood here: the continuous accelerating advance,
        // the `SCRUB_LOST_MS` lost-keyup safety net and the tap-release debounce, all three run
        // against the mounted `PlayerScreen`'s own `scrub` from the loop. They are that screen's
        // `Tick` now — `step_scrub_hold`, formula for formula, and the debounce gone with
        // click-to-confirm (a key target waits for OK) — which is what makes the field have one
        // owner. Phase 12, PX-PLAYER.)
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
                player.raise_for_offer(fr.now, fr.ctrl.primary_btn());
            } else if plx_appkit::player_hud::standin_left_the_ring(
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
        if matches!(app.route(), AppArg::Player) {
            let paused_now = paused();
            if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                if player.up_next.armed() {
                    if plx_appkit::up_next::countdown_may_run(
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
        if matches!(app.route(), AppArg::Player)
            && !app.ptr.cur_hidden
            && !app.ptr.button_down
            && app.ptr.last_motion != 0
            && fr.now.wrapping_sub(app.ptr.last_motion) > 3000
        {
            hide_cursor();
            app.ptr.cur_hidden = true;
        }
        // re-pause the foreground restore's paused load the INSTANT its resume frame is on screen
        // (a seek armed under a viewer Pause is closed by the pump itself, `pump::settle_paused_seek`).
        // `frames()` counts real "frame presented" callbacks, so >= 1 means the target frame is
        // already composited — re-freezing then shows it with the shortest possible play-blip
        // (a paused load must briefly Play to decode the frame; buffer-feed has no preroll).
        if resume_pend()
            && matches!(app.route(), AppArg::Player)
            && plx_media::player::seek_preroll_active()
            && seek_pending() < 0
            && frames() >= 1
            && playpos() + 15 * 1_000_000_000 >= app.repause_at
        {
            plx_media::player::finish_paused_seek(&mut app.adapters.player);
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
            // key-up commits nothing. A SHORT press is untouched on BOTH screens — what it does
            // on a Continue Watching tile is `route::deck_press()`'s answer (resume, or open the
            // page), and OK on an episode still still plays it, by design; the hold is the other
            // half of those interactions either way. Ordered ahead of the commit
            // arm (and exclusive with it) so the two can never both run.
            //
            // `is_long` leads and short-circuits, deliberately: everything after it OPENS a
            // menu, so evaluating the arms first would put the popover up on the key-DOWN of
            // every tap.
            let held_menu = app.input.press.is_long(fr.now)
                && match app.route() {
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
                match app.route() {
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
                    AppArg::Player
                        if matches!(
                            super::bridge::player_overlay_kind(&app.pages),
                            Some(plx_screens::player::overlay::OverlayKind::Info)
                        ) =>
                    {
                        commit_info_press(&mut app.player,
                            &mut app.adapters.player,
                            &mut app.repause_at,
                            &mut app.refresh_hubs,
                            &mut app.pages,
                            &mut app.bridge,
                        )
                    }
                    AppArg::Player => activate_player_row(&mut app.player,
                        &mut app.adapters.player,
                        &mut app.repause_at,
                        fr.ctrl,
                        fr.now,
                        &mut app.refresh_hubs,
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
        // Session observations are ingested unconditionally by Bridge, in Landing arrival
        // order before Hubs. This route follower consumes only its exact committed handoff;
        // it neither drains a process-global mailbox nor applies worker facts itself. A
        // carried credential commit keeps its flow page until the queued ACK finishes.
        //
        // The routing decision itself is `bridge::follow_auth_landing` — ONE production function,
        // called from here and from the two tests that used to keep their own inline copy of this
        // exact `match` (see that function's doc for why sharing it, rather than restating it, is
        // what makes those tests prove anything).
        if matches!(app.route(), AppArg::Login | AppArg::Profiles) {
            super::bridge::follow_auth_landing(&mut app.pages, &mut app.bridge);
        }
        // dev: an `acct` step on the LOAD DIAL asks for the REAL Account popover, so the
        // shipped surface and a synthetic one can be interleaved inside ONE launch. Assigning
        // the route directly (rather than through `nav_to`) is deliberate: the question is what
        // the PANEL costs, and a page transition on the step boundary would put a cross-fade in
        // the middle of the leg being measured.
        if plx_ui::glassload::armed() {
            let want = app.glass.wants_account();
            if want && app.route() == AppArg::Home {
                super::bridge::open_account_menu(&mut app.pages);
            } else if !want && super::bridge::account_menu_up(&app.pages) {
                super::bridge::dismiss_surfaces(&mut app.pages);
            }
        }
        // dev: /tmp/plxnative-navosc — `crate::dev::scenarios::nav_osc_tick`.
        crate::dev::scenarios::nav_osc_tick(app, fr.now);
        // dev: /tmp/plxnative-pushbench — `crate::dev::scenarios::push_bench_tick`.
        crate::dev::scenarios::push_bench_tick(app, fr.now);
        // dev: /tmp/plxnative-deepbench — `crate::dev::scenarios::deep_bench_tick`.
        crate::dev::scenarios::deep_bench_tick(app, fr.now);
        // dev: /tmp/plxnative-herobench — `crate::dev::scenarios::hero_bench_tick`.
        crate::dev::scenarios::hero_bench_tick(app, fr.now);
        // dev: /tmp/plxnative-holdbench — `crate::dev::scenarios::hold_bench_tick`.
        crate::dev::scenarios::hold_bench_tick(app, fr.now);

        // ---- the page cross-fade's commit frame ------------------------------------------
        // Stepped UNCONDITIONALLY, never per-route: a fader only one screen advances is a fader
        // parked at alpha 0 the moment that screen is not the one mounted. Placed AFTER every
        // route change above (input, the async person request, the login landing) so a
        // superseded request is visible as `route != req.from`, and BEFORE the per-route
        // `update(dt)` below so the incoming screen steps its springs on the same frame it first
        // draws — otherwise its first drawn frame is one update stale.
}

// (`nav_commit` stood here — 120 lines that ran at `ui::nav`'s fade floor, took the loop's
// `NavReq`, applied the supersede test, wrote the trail, wrote the route and, LAST, told the
// container. Every one of those five acts is the container's now: `NavStack::request` captures the
// outgoing page's `ReturnState` at the PRESS frame, `PageDip` holds the op until its own floor,
// `NavStack::cancel` is the supersede test over `EntryId` rather than route kind, and the op
// itself is the history. The four `Nav` destinations became four `NavOp`s at their own press
// sites; what each arm did BESIDE the flip — the Library's store swap, Home's strip focus, the
// season a show opens on — are seeds and queued commands the mount consumes, so they are set at
// the press and spent at the floor exactly as they were.)

/// **What an owned screen asked the LOOP to do this frame** (spec §14, `screens::registry`'s
/// `LoopReq`), drained immediately after the dispatcher's frame so the route it may flip is the
/// one the rest of the iteration sees.
///
/// Each variant is a DEBT with a phase number on it: the machine that should own the decision —
/// Session, or Navigation over the app's real stack — is not on the dispatcher yet, so the screen
/// names the outcome and the loop performs it. None of them is a general escape hatch; they are
/// the four things a Settings-family screen can decide that outlive the surface it decided them in.
pub(super) fn request_local_erasure(rec: &mut super::recorder::Recplay,
    bridge: &mut super::bridge::Bridge,
    pages: &mut plx_ui::dispatch::Dispatcher<super::bridge::AppHost>) -> bool {
    if matches!(rec, super::recorder::Recplay::Replaying(_)) || bridge.is_controlled_replay() {
        rec.refuse("replay cannot authorize local erasure");
        return false;
    }
    let failed = super::finish_recording(rec, bridge.landgate());
    bridge.recording_retired_for_erasure(failed);
    delete_all_local_data_and_sign_out(pages);
    true
}

pub(super) fn finish_local_erasure(bridge: &super::bridge::Bridge,
    pages: &mut plx_ui::dispatch::Dispatcher<super::bridge::AppHost>) {
    if delete_outcome(bridge.auth_read().0.delete_leftovers).to_sign_in {
        super::bridge::nav_root(pages, AppArg::Login);
    }
    super::bridge::dismiss_surfaces_now(pages);
    plx_ui::popover::host::blur_invalidate();
}

/// The request-to-performer boundary for root navigation. The live loop and owned-screen
/// tests consume the same emitted enum here, without constructing a native App or a route oracle.
pub(super) fn reduce_navigation_request(request: plx_screens::registry::LoopReq,
    pages: &mut plx_ui::dispatch::Dispatcher<super::bridge::AppHost>)
    -> Result<(), plx_screens::registry::LoopReq> {
    match request {
        // Root BACK belongs to platform Home; first-run Onboard BACK has an in-app destination.
        plx_screens::registry::LoopReq::BackAtRoot => back_at_root(),
        plx_screens::registry::LoopReq::OnboardBack => enter_profiles_from_onboard(pages),
        other => return Err(other),
    }
    Ok(())
}

fn loop_requests(app: &mut App) {
    for req in app.bridge.take_reqs() {
        let req = match reduce_navigation_request(req, &mut app.pages) {
            Ok(()) => continue,
            Err(req) => req,
        };
        match req {
            // BACK at a root the platform owns. The FIRST consent stage is the fourth such root
            // and the one the 2026-09-03 rule could not reach until this phase — see the key
            // ladder's dispatcher arm for why a `Popover` could not tell the two BACKs apart.
            plx_screens::registry::LoopReq::BackAtRoot |
            plx_screens::registry::LoopReq::OnboardBack => unreachable!("root request reduced above"),
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
            // that one instant either, only the popover behind it — so its panel is still
            // fading over the frozen host snapshot of the Settings page for the length of that
            // fade. `blur_invalidate()` is the same fix legacy reached for at the same call
            // site and for the same reason (`consent.rs::close_delete_and_menu`'s comment): it
            // forces a recapture, so the alert's exit shows whatever is actually on screen now —
            // the incoming sign-in page — rather than a ghost of the screen the sweep just erased.
            plx_screens::registry::LoopReq::DeleteAllLocalData => {
                if request_local_erasure(&mut app.rec, &mut app.bridge, &mut app.pages) {
                    app.boot_initial = None;
                }
            }
            plx_screens::registry::LoopReq::LocalDataErased => {
                finish_local_erasure(&app.bridge, &mut app.pages);
            }
            // First-run Favourites finished (Start watching, or the retry that found nothing to
            // pin): Home, with the trail reset so BACK from it is the root press.
            plx_screens::registry::LoopReq::OnboardDone => {
                enter_home_from_onboard(&mut app.pages);
            }
            // Phase 10, the profile menu's five rows. The surface has already asked the container
            // to dismiss it in the same drain — what is left is the part a screen may not do.
            //
            // No `enter()` on any of the three route flips (phase 6): naming the route is the
            // whole of mounting the owned screen it lands on — see
            // `enter_profiles_from_onboard`'s doc.
            plx_screens::registry::LoopReq::AccountChangeProfile => {
                super::bridge::execute_session_command(&mut app.pages,
                    plx_session::auth::SessionCmd::StartSwitch(plx_session::auth::Picker::ChangeProfile));
                // **`switch_profile`, not a bare `Root(Profiles)`.** Every page of the outgoing
                // profile is dropped with its `ReturnState` and its body — see
                // `switching_profile_leaves_the_container_holding_nothing_of_the_previous_profile`.
                super::bridge::switch_profile(&mut app.pages);
            }
            plx_screens::registry::LoopReq::AccountSignIn => {
                super::bridge::execute_session_command(&mut app.pages, plx_session::auth::SessionCmd::StartLogin);
                super::bridge::nav_root(&mut app.pages, AppArg::Login);
            }
            plx_screens::registry::LoopReq::AccountSignOut => {
                super::bridge::execute_session_command(&mut app.pages, plx_session::auth::SessionCmd::SignOut);
                super::bridge::nav_root(&mut app.pages, AppArg::Login);
            }
            // The host route does NOT move: Settings is a surface presented over the same page,
            // and its Privacy, Legal and Favourite-libraries children are pages of that surface's
            // own inner stack. The account sheet's dismissal and this presentation are two parked
            // ops, so the host fold walks Sheet-Closing `(Live, Cached)` → Opaque-Opening
            // `(Frozen, Cached)` and never returns the page to `HostRender::Live` — the snapshot
            // survives the handover, which is what
            // `account_to_settings_never_unfreezes_the_host` pins.
            plx_screens::registry::LoopReq::AccountSettings => {
                super::bridge::open_settings(&mut app.pages);
            }
            // `ps` is threaded rather than read from a global (lane D's lab gate: `player::diag`
            // has taken a `&PlaybackSession` since phase 9, and `lab::request_upload` with it).
            plx_screens::registry::LoopReq::AccountSendDiagnostics => {
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

        // **One arm for every page that wears the shared bar** (`bridge::wears_tab_bar`, the same
        // predicate `capture_chrome` publishes the strip on). The springs it steps — the capsule,
        // the scroll, the chip — live in `Bridge`'s own `strip: StripRender`, not in any screen:
        // `SearchScreen::tick` and the rest only step their OWN rows. Without this the springs
        // freeze at whatever the previously-drawn page left them at, and `ChromeSnapshot::members`'s
        // published rects (read by pointer hit-testing and focus) go stale. Four arms spelled the
        // routes by name (Home, Library, Search|Watchlist) and left Collections and Recommended
        // out, which is how those two pages wore a bar nobody stepped.
        if !super::bridge::host_frozen(&app.pages) && super::bridge::wears_tab_bar(&app.route()) {
            match app.route() {
                AppArg::Home => {
                    crate::dev::scenarios::hero_osc_tick(app, fr.now);
                    crate::dev::scenarios::home_fold_osc_tick(app, fr.now);
                    crate::dev::scenarios::home_osc_tick(app, fr.now);
                }
                // (Each tick gates itself on the pages it drives: the sweep on the library family,
                // the switch cycle on the full grid.)
                arg if arg.is_library_family() => {
                    crate::dev::scenarios::lib_osc_tick(app, fr.now);
                    crate::dev::scenarios::lib_switch_tick(app, fr.now);
                }
                _ => {}
            }
            // only when a bar-wearing page is actually drawn — stepping the cell springs during
            // Player/Detail frames was pure waste on the A53. Scoped, because the page can be the
            // one UNDER the account popover and its glass backdrop is refreshed off the
            // underlay's motion.
            let (_, moving) = plx_machine::idle::scoped_motion(|| {
                app.bridge.update_home_chrome(&mut app.pages, &mut app.glass, fr.dt);
            });
            fr.underlay_moving |= moving;
        }
        // dev: /tmp/plxnative-searchosc — `crate::dev::scenarios::search_osc_tick`.
        crate::dev::scenarios::search_osc_tick(app, fr.now);
        // (The television's keyboard used to be dismissed HERE, by an `else` that called
        // `textinput::stop()` on every frame of every other route — because `search::leave`
        // was reached by no route off the screen. It is the CONTAINER's now: `nav::forward_leave`
        // and the whole teardown table went with `app/nav.rs` in D1, and every way off Search
        // ends in a `WillLeave`/`Unmount`/`Cover` on `SearchScreen`, which drops the keyboard on
        // each. That is also the half the per-frame poll never did — it cleared `textinput`'s own
        // flag and left the screen's editing state set.)
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
        // dev: /tmp/plxnative-modalbench — `crate::dev::scenarios::modal_bench_tick`.
        crate::dev::scenarios::modal_bench_tick(app, fr.now);
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
        #[cfg(feature = "threadcheck")]
        plx_ui::runtime_warning::update(app.boot_initial.is_some());
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
        // Async play resolve: install the worker's plan and start the engine. Permission comes
        // from the window/playback lifecycle, independently of which screen is mounted.
        // A fresh accepted resolve owns a different playback from any parked foreground retry.
        // Tell the machine at this transaction boundary (`Superseded(Playback)`): it retires the
        // parked owner and leaves a pending Load to its own settlement. A same-session Prepared
        // retry does not set play_pending and remains owned by drive_foreground.
        if app.window_activity.allow_present(true) && plx_media::route::play_pending() {
            let _ = drive_foreground(
                &mut app.player.lifecycle,
                &mut app.player.session,
                ForegroundInput::Superseded(Successor::Playback),
                &mut PlayerForegroundActuator {
                    pa: &mut app.adapters.player,
                    repause_at: &mut app.repause_at,
                },
            );
        }
        land_play_then_observe(
            app,
            |app| {
                if !playback_may_run(app) {
                    return;
                }
                // A cold resume's rebuild landed (`player::land_resume`): the Load the pending play
                // was holding for starts now.
                if let Some(outcome) = plx_media::player::land_resume(&mut app.player.session) {
                    plx_machine::idle::invalidate();
                    finish_landed_play(app, matches!(outcome, plx_media::player::ResumeOutcome::Prepared));
                }
                if let Some(r) = plx_media::route::pump_play(&mut app.player.session, app.bridge.metadata_mut()) {
                    plx_machine::idle::invalidate();
                    // A preview landing nobody is waiting for any more (the page halted it while
                    // the resolve was in flight) must not start an engine: nothing would track
                    // it, so nothing would stop it at its end or hand the engine to a later Play.
                    if plx_media::route::is_preview(&app.player.session)
                        && !plx_media::player::preview::expects_landing()
                    {
                        if let Some(transaction) = plx_media::route::pending_route_start() {
                            let _ = plx_media::route::reject_route_start_preparation(transaction);
                        }
                        plx_media::player::stop_bufferfeed(&mut app.player.session, &mut app.adapters.player);
                        plx_media::route::clear_preview(&mut app.player.session);
                        return;
                    }
                    // A transcode with a saved position rebuilds at that offset on a worker (`begin_resume`):
                    // the play stays pending, the spinner stays up, and the drain at the top of this landing
                    // starts the Load when the flight lands.
                    match if r <= 0 {
                        plx_media::player::ResumeStart::Settled(plx_media::player::ResumeOutcome::Prepared)
                    } else {
                        plx_media::player::begin_resume(&mut app.player.session, r)
                    } {
                        plx_media::player::ResumeStart::Pending => {}
                        plx_media::player::ResumeStart::Settled(outcome) => finish_landed_play(
                            app,
                            matches!(outcome, plx_media::player::ResumeOutcome::Prepared),
                        ),
                    }
                }
            },
            // `pump_play` can install a refused `/decision` after the earlier report
            // observation but before this frame draws the Error screen. Observe again at that
            // exact publication boundary; latches make a healthy/no-change frame idempotent.
            |app| plx_media::player::report::tick(&app.player.session),
        );
        // Async detail load: install the worker's item into CURRENT. Route-unconditional for
        // the same reason as pump_play — play_item_now requests a detail from Home and flips
        // straight to the player, so a Detail-gated pump would never land it.
        if app.bridge.metadata_pump_detail() {
            plx_machine::idle::invalidate(); // a detail landing rewrites the page under us
        }
        // Async season load: install the worker's episode list into CURRENT. Route-unconditional
        // for the same reason as pump_detail above; see `stores/metadata.rs`'s module doc for why
        // this call site went missing for a whole migration and how it was found.
        // MUST run AFTER pump_detail(): a landed detail's `install_landed_detail` calls
        // `supersede_season()`, invalidating any season fetch for the item being replaced.
        // Pumping season first could apply a stale season landing to CURRENT in the one frame
        // before pump_detail() replaces it.
        if app.bridge.metadata_pump_season() {
            plx_machine::idle::invalidate(); // a season landing rewrites the episode row under us
        }
        // D7: the continuation half of `activate_card`'s show/season Play — see
        // `App::menu_play_await`/`input::menu_play_tick`'s own doc. Right beside the pump above
        // for the same route-unconditional reason: the press that armed the wait may have come
        // from Home while a different page is up by the time the landing (or its settled
        // failure, or the ceiling) says the wait is over. A title the launch asked for
        // (`deeplink`) arms the same wait once Home is up.
        let stands = link_session_stands(app);
        super::deeplink::frame(&mut app.pages, &mut app.deeplink, &mut app.menu_play_await,
            app.window_activity.allow_present(true), stands, fr.now);
        if app.menu_play_await.is_some() {
            menu_play_tick(&mut app.player.session, &mut app.adapters.player,
                &mut app.pages, &mut app.bridge, &mut app.menu_play_await, fr.now);
        }
        // Server-side view-state WRITES (Mark as Watched / Unwatched, Remove from Deck): send
        // the next queued one, land the last one's answer and kick the refresh it owes. Route-
        // unconditional for the same reason as the two pumps around it — the user can walk off
        // Home or off the detail page between pressing and the server answering, and the refresh
        // is owed either way. Invalidates from inside, per landing.
        // The player is on top: the reconcile reads stay owed until it leaves (an Up Next chain
        // costs one batch at its exit); the patches below are not held.
        app.bridge.hold_network(matches!(app.route(), AppArg::Player));
        // Stops first: the resume patch lands before any read of the item is asked for.
        let endpoints = app.bridge.stops_pump();
        super::bridge::execute_endpoint_outcomes(&mut app.pages, endpoints);
        app.bridge.catalog_feed(&mut app.pages);
        let endpoints = app.bridge.viewstate_pump();
        super::bridge::execute_endpoint_outcomes(&mut app.pages, endpoints);
        // …and the read-back it owes: what the server now says about the item(s) just written, on
        // every route for the same reason (a refused write is put right here, not by a refetch).
        let endpoints = app.bridge.reconcile_pump();
        super::bridge::execute_endpoint_outcomes(&mut app.pages, endpoints);
        // The PMS notification feed (on, for owned servers only: opens nothing until one is described) and
        // the reads its hints owe the pages on the stack, visible and covered. Route-unconditional
        // like its neighbours; it never touches the stop queue or the playback Offset.
        let content: Vec<plx_screens::registry::ContentArg> = app.pages.nav.tabs.stack.entries.iter()
            .filter_map(|e| match &e.arg { AppArg::Content(c) => Some(c.clone()), _ => None })
            .collect();
        let shown = app.pages.nav.tabs.stack.entries.iter().fold(
            plx_data::stores::notify_map::Shown::default(), |mut shown, e| {
                match e.arg {
                    AppArg::Home => shown.home = true,
                    ref arg if arg.is_library_family() => shown.library = true,
                    AppArg::Search => shown.search = true,
                    AppArg::Watchlist => shown.watchlist = true,
                    _ => {}
                }
                shown
            });
        let (resynced, endpoints) = app.bridge.notify_pump(&content, shown);
        super::bridge::execute_endpoint_outcomes(&mut app.pages, endpoints);
        if resynced {
            app.reread.resync(); // a lost stream: events were missed, every covered page is owed a re-read
        }
        // …and the covered pages' own re-read, once each cover episode (`app/reread.rs`).
        super::reread::run(&mut app.reread, &mut app.pages, &mut app.bridge);
        // A server on a plaintext grant keeps trying HTTPS (`plex::grant::UpgradeRetry`): its
        // endpoint re-discovery races every HTTPS route first and retires the grant when one
        // verifies. Route-unconditional — the upgrade is owed whatever is on screen.
        let upgrades = app.plaintext_upgrade.due(fr.now);
        super::bridge::execute_endpoint_outcomes(&mut app.pages, upgrades);
        app.bridge.person_pump();
        app.bridge.collection_pump();
        // …and the Detail pages whose item, season or show it just spoke about: re-read from the
        // server's word, wherever on the stack they are (`content::reconcile_details`).
        let landed = app.bridge.take_landed_views();
        if !landed.is_empty() {
            reconcile_details(&mut app.pages, &mut app.bridge, &landed);
        }
        // …and the cross-source resolve it kicked off. Route-unconditional for the same reason,
        // and separate because it lands one round trip per source LATER than the page does —
        // "Also available" appears when the other servers have answered, not when the page
        // mounts. It raises the Metadata store's notice, since a landing that grows
        // the actions row must be drawn without waiting for a keypress.
        app.bridge.metadata_pump_alt_sources();
}

/// What a landed play does once its resume is settled: the Load, and the off-route backstop. Reached
/// from the frame that lands a plan whose resume settled on the spot, and from the frame that lands
/// a cold resume's flight (`player::land_resume`) — the same step, taken after the rebuild landed.
fn finish_landed_play(app: &mut App, resume_prepared: bool) {
    if !resume_prepared {
        if let Some(transaction) = plx_media::route::pending_route_start() {
            let _ = plx_media::route::reject_route_start_preparation(transaction);
        }
    }
    // A live engine with the route anywhere but Player is unrecoverable BY THE USER —
    // every transport key and the EOS teardown are route-gated — so repair the
    // invariant here rather than trust that no path can violate it. The one that
    // could is cancelled above; this is the backstop, and it is the cheaper half.
    // A preview is the exception: the detail page stays mounted, and an off-route
    // engine is the feature, not a violation.
    if resume_prepared {
        let started = plx_media::player::start_bufferfeed(
            &mut app.player.session,
            &mut app.adapters.player,
        );
        let preview = plx_media::route::is_preview(&app.player.session);
        if started && !matches!(app.route(), AppArg::Player) && !preview {
            log("pump_play: engine started off-route → restoring AppArg::Player");
            // The page is being taken off screen by a LANDING, not by a navigation. It
            // carried a `forward_leave(app.route())` teardown here until phase 12; every
            // arm of that table was `None` and the pages it named are owned screens that
            // drop what they loaded on the container's own `Unmount` — including
            // Search's keyboard (`SearchScreen::step`), which is the case this line was
            // written for.
            super::bridge::show_page(&mut app.pages, AppArg::Player);
        }
        if preview {
            // A host Load of 0 with the clock sink off is "no video path", not an
            // admitted slot. Counting it spends the cycle and the later failure
            // opens the breaker, so every later title is skipped.
            let seam_absent = cfg!(feature = "hostsim") && !plx_base::devtrig::flag("clocksink");
            if started && !seam_absent {
                plx_media::player::preview::note_admitted();
            } else if plx_media::route::url(&app.player.session).is_empty() {
                plx_media::player::preview::note_refused_direct(
                    plx_media::route::cur_sid(&app.player.session),
                    &plx_media::route::cur_rk(&app.player.session),
                );
            } else {
                plx_media::player::preview::note_admission_refused();
            }
        }
    }
}

/// The draw phase, entered only on a presenting frame: `clear_opaque_region` at entry, the
/// viewport, glass owners' prepare, the page pass, the surfaces bottom-to-top and the
/// instruments. Returns the viewport for the host-side screenshot that follows the draw.
pub(crate) unsafe fn draw(app: &mut App, fr: &mut Frame) -> (i32, i32, i32, i32) {
    #[cfg(feature = "threadcheck")]
    let _draw_scope = plx_base::task::watchdog::draw_scope();
            // EXPERIMENT (`/tmp/plxnative-egldamage`), no-op without the trigger. FIRST, before
            // any GL command of this frame: `EGL_KHR_partial_update` only permits a damage
            // region to be declared before rendering begins. See `egl.rs`.
            plx_gfx::egl::frame_damage();
            // dev: the backdrop-glass LOAD DIAL and the blurred-transition prototype
            // (`/tmp/plxnative-glassload`, `/tmp/plxnative-navblur`). Both are no-ops when
            // their trigger is absent. HERE and not below the gate, because the dial's cadence
            // is counted in PRESENTS — a loop iteration the gate skipped drew no glass — and
            // because a step rollover invalidates the snapshot, which must precede every glass
            // surface drawn by the synthetic dial.
            app.glass.prepare_dial(fr.now);
            // The authored canvas, scaled UNIFORMLY into the drawable and centred. The shaders
            // divide every coordinate by `u_screen` (which stays 1920x1080), so this one call
            // is the entire logical->physical mapping — nothing else in the renderer knows the
            // drawable size. At 1:1 unless a webOS 1-3 owner picks a smaller Interface resolution (or the debug drawable trigger asks for one); on any 16:9 surface a
            // plain scale with zero letterbox (1080p->4K is exactly 2x); and on an unexpected
            // aspect, letterboxed rather than stretched or stuffed into a corner. See `surface`.
            let (vx, vy, vw, vh) = plx_base::surface::viewport();
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
            plx_ui::profile::phase("profile.empty", || {});
            plx_ui::profile::phase("frame.ui", || {
                plx_ui::guard(|| {
                    if fr.player {
                        // `plx_platform::tv::window::clear_opaque_region()` used to be called here. It is
                        // §3.3 step 10's privileged call and belongs at the container library's
                        // OWN draw entry, which `app.pages.draw` below reaches in this same frame:
                        // `Bridge::clear_opaque_region` performs it, keyed on the plane's bit
                        // rather than on this route test. Calling it in both places would send the
                        // same wayland request twice per frame for the whole of a playback.
                        glClearColor(0.0, 0.0, 0.0, 0.0);
                        // The frame's first framebuffer-0 command, where this driver parks the
                        // wait for a free back buffer: spanned like every other route's `clear`.
                        // dev (`/tmp/plxnative-framecb`): the frame thread's cost over the wait
                        // and, from the acquired buffer to the swap, over the commit phase.
                        plx_platform::tv::window::frame_probe_waiting();
                        plx_base::diag::spans::span("clear", || glClear(GL_COLOR_BUFFER_BIT));
                        plx_platform::tv::window::frame_probe_acquired();
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
                        let busy = plx_appkit::player_hud::busy(&app.player.session);
                        let bare_middle = !matches!(
                            super::bridge::player_overlay_kind(&app.pages),
                            Some(plx_screens::player::overlay::OverlayKind::Info)
                                | Some(plx_screens::player::overlay::OverlayKind::Chapters)
                        );
                        // Both subtitle paths lift clear of the transport for the same reason and
                        // by the same test — an open track menu counts, since that is exactly when
                        // the user is reading the bottom of the screen. The Timing capsule is the
                        // one panel that instead HIDES the transport outright (`HudPolicy::Hidden`
                        // outranks `Lifted` — see `bridge::player_hud_policy`).
                        let hud_policy = super::bridge::player_hud_policy(&app.pages);
                        if let Some(player) = super::bridge::player_mut(&mut app.pages) {
                            player.busy = busy;
                            player.transport = bare_middle;
                            player.set_hud_policy(hud_policy);
                        }
                        // THE PAGE, and its panels: `Dispatcher::draw(.., true)` runs the page
                        // pass (subtitles, the transport, the read-out — `PlayerScreen::draw`) and
                        // then the surfaces bottom-to-top, which is exactly the order the four
                        // hand-written `*_panel::draw()` calls used to state by hand.
                        //
                        // `popover::host::begin_frame` is deliberately NOT called on this path and
                        // never was: what is behind these panels is a hardware video plane GL
                        // cannot read back, so there is no host snapshot to take
                        // (`RenderStrategy::VideoPlane`).
                        //
                        // During HDR playback the TV lifts the graphics plane, so the whole pass
                        // draws under the viewer's HDR overlay brightness (`ui::output_gain`: RGB
                        // only, alpha untouched). 1.0 for SDR, a re-encode, or the 100% default.
                        let source_hdr = app.bridge.metadata_view().playing().is_some_and(|p| p.hdr);
                        let gain = plx_media::route::overlay_gain(&app.player.session, source_hdr);
                        plx_ui::output_gain(gain, || app.pages.draw(&mut app.bridge, true));
                        // Skipped rather than drawn while one of the player's own panels is
                        // up — `More` carries this very panel's own toggle, and the panel's
                        // content routinely spans wide enough to reach any of their bottom-right
                        // rects. See `bridge::player_diagnostics_visible` (issue #163).
                        if super::bridge::player_diagnostics_visible(&app.pages) {
                            app.diagnostics.draw();
                        }
                    } else {
                        // dev (`/tmp/plxnative-framecb`): the page's first clear is this route's
                        // first framebuffer command, so `gfx` brackets the back-buffer wait there.
                        plx_gfx::gfx::arm_clear_probe();
                        // Open the shared popover frame FIRST: it takes this frame's page damage
                        // and decides whether the frozen-host snapshot still describes the page. Route-agnostic by construction —
                        // see `ui::popover::host::begin_frame`.
                        // A bound preview owns the plane the same way the player branch does: there
                        // is no framebuffer to snapshot. Skip the door instead of tripping the
                        // debug assertion. A popover from this page halts the preview first, so
                        // this frame only skips while the picture is the intended ground.
                        if !(app.player.video_plane_bound && plx_media::route::is_preview(&app.player.session)) {
                            plx_base::diag::spans::span("host", || plx_ui::popover::host::begin_frame(fr.underlay_moving));
                        }
                        use plx_ui::frame::backdrop::Z;
                        let layers = app.pages.backdrop_layers(plx_ui::nav::page_alpha());
                        app.glass.sources.borrow_mut().begin(layers);
                        let sources = app.glass.sources.clone();
                        let _visible_walk = app.glass.walk(Z::ALL);
                        let page_owned = super::bridge::page_owned(&app.pages);
                        let host_replaced = super::bridge::host_replaced(&app.pages);
                        let plan = super::bridge::page_plan(host_replaced, page_owned);
                        // One dispatcher walk supplies declarations, strict source prefixes and
                        // the visible frame. Only the visible walk advances the host cache.
                        let mut page = |ceiling: Z| {
                            let _host = (!plx_ui::frame::backdrop::discovering() &&
                                (ceiling == Z::ALL || plx_ui::popover::host::held_ceiling().is_some_and(|z| z < ceiling)))
                                .then(plx_ui::popover::host::page_pass);
                            if page_owned {
                                app.pages.draw_with_glass_below(&mut app.bridge, &mut app.glass, true, ceiling);
                            } else {
                                plx_gfx::gfx::frame_clear(plx_ui::theme::CLEAR_RGB.0, plx_ui::theme::CLEAR_RGB.1, plx_ui::theme::CLEAR_RGB.2);
                            }
                        };
                        {
                            let _declarations = plx_ui::frame::backdrop::discover(sources.clone());
                            plx_base::diag::spans::span("disc", || page(Z::ALL));
                        }
                        sources.borrow_mut().resolve();
                        // `plxnative-framedrop` armed: which recorded draw invalidated a source.
                        // Empty (no allocation, no work) unarmed.
                        for line in sources.borrow_mut().take_diffs() {
                            log(&line);
                        }
                        let jobs = sources.borrow().jobs();
                        // Independent bands replay only the strict prefix below their ceiling.
                        // A band intersecting lower glass captures that composite in visible order.
                        for (ceiling, rect) in jobs {
                            let _source_walk = plx_ui::frame::backdrop::enter(sources.clone(), ceiling);
                            let reg = plx_gfx::gfx::blur_region_for(plx_ui::frame::backdrop::rises(ceiling), rect.x, rect.y, rect.w, rect.h);
                            // `src`: one blur source job, its replay of the page prefix included.
                            plx_base::diag::spans::span("src", || {
                                if plx_gfx::gfx::blur_snapshot_direct(reg, &mut || page(ceiling)) {
                                    plx_gfx::gfx::retain_backdrop(ceiling);
                                }
                            });
                        }
                        // An opaque route ground replaces the page entirely.
                        if plan != super::bridge::PagePlan::SurfacesOnly {
                            plx_ui::profile::phase("main.ui", || page(Z::ALL));
                        }
                        // The diagnostics read-out, off the player. It drew ONLY inside the branch
                        // above until 2026-08-29, which is why its module doc had to warn that a
                        // toggle offered anywhere else would tick a box and show nothing — and why
                        // the failure this app is reported for most ("it opens and finds nothing")
                        // could produce no artefact at all: it never reaches a player.
                        //
                        // **Here rather than on the frame's common tail, and the simulator is what
                        // settled it.** On the player path the panel is genuinely last, except
                        // while one of the player's own panels is open (issue #163 — skipped
                        // there instead, see `bridge::player_diagnostics_visible`); here it is
                        // over the PAGE and under the app's modal surfaces, because on this path
                        // something DOES sit in its corner — the profile menu, which carries the row
                        // that turns it off. Drawn last it covered the account chip and then the
                        // popover itself, so the switch could only be found by pressing keys at a
                        // menu you cannot see. A control must be visible over the thing it
                        // controls. Two call sites, and the `else` covers every non-player route,
                        // so a new route still cannot be forgotten.
                        app.diagnostics.draw();
                        // Owned pages already dispatched their surfaces in the visible walk.
                        if plan != super::bridge::PagePlan::Owned {
                            app.pages.draw_with_glass_below(&mut app.bridge, &mut app.glass, false, Z::ALL);
                        }
                        // dev: the blurred route transition, then the load dial's glass surfaces.
                        // LAST on the non-player path, so the snapshot either takes is of the
                        // COMPLETE page — which is the honest source for a surface that sits on
                        // top of everything, and the one thing the tab track (drawn inside the
                        // page) cannot have.
                        drop(_visible_walk);
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
                        // A supersampled simulator render (`surface::render_scale`) exists to be
                        // captured, so it leaves the diagnostic digits out of the picture.
                        #[cfg(feature = "devtools")]
                        if plx_base::surface::render_scale() == 1 {
                            let fps_col = if app.buffer_flip_count < 30 {
                                plx_ui::theme::DIAG_FLIP_A
                            } else {
                                plx_ui::theme::DIAG_FLIP_B
                            };
                            plx_gfx::gfx::draw_number(
                                app.fps_shown,
                                SCR_W as f32 - 70.0,
                                64.0,
                                46.0,
                                fps_col.as_ptr(),
                            );
                        }
                    }
                    plx_ui::anim::draw_overlay(); // dev diagnostic overlay (all routes)
                                                     // The lab upload read-out, over everything, on every route — including the
                                                     // player, where the two branches above diverge and this one must not.
                    crate::lab::draw(app.diagnostics.frame_if_shown());
                    #[cfg(feature = "threadcheck")]
                    plx_ui::runtime_warning::draw(app.boot_initial.is_some());
                });
            });
    (vx, vy, vw, vh)
}

/// **A screen visit ended: report how the app felt on it (`route.left`) and start the next one
/// from nothing.** The frame side is `Instruments`' (`heartbeat::health`, every build), the
/// repeated-key side the dispatcher's (`Dispatcher::take_dead_presses`); both are read and reset
/// here, together, so a visit's counts never leak into the next screen's. The first call, from
/// the empty route before the first screen, reports nothing and clears the boot's warm-up.
///
/// `transition` is a route change seen in `report()`, AFTER this iteration's `present_and_swap`:
/// the frame just presented is the first of the screen being opened (its route was committed
/// before it was drawn) and this iteration's stall is added at the next iteration, after the
/// visit reset, to the new visit. So the frame moves to the new visit too, and the cost of
/// opening a screen is never split across two. A visit that ends for the background or quit has
/// no new screen: `transition` is false. The dispatcher's repeated-key run is dropped with the
/// visit either way.
pub(crate) fn end_route_visit(app: &mut App, transition: bool) {
    let dead = app.pages.end_visit_dead_presses();
    let health = if transition { app.instr.finish_visit_at_transition(dead) } else { app.instr.finish_visit(dead) };
    let Some(health) = health else { return };
    if app.last_route_reported.is_empty() {
        return;
    }
    plx_telemetry::diag::event(plx_telemetry::diag::schema::DiagEvent::RouteLeft {
        screen: app.last_route_reported,
        slow_frames: health.slow_frames,
        frozen_frames: health.frozen_frames,
        hang: health.hang,
        dead_presses: health.dead_presses,
    });
}

/// The iteration's report: the route word (on change), the lab route note, the focus probe
/// and the FRAMEDROP line.
pub(crate) unsafe fn report(app: &mut App, fr: &mut Frame) {
        // dev: the stress-bench oscillators' own frame-time accumulator — right after Swap is
        // stamped (`present_and_swap`, just above this call in `run()`), the earliest point this
        // iteration's total is known. See `crate::dev::scenarios::bench_frame_tick`'s doc.
        crate::dev::scenarios::bench_frame_tick(app, fr.present, fr.deferred, fr.now);
        fr.rn = super::words::route_word(&app.route());
    let rn = fr.rn;
    if plx_gfx::text::take_measure_fault() && !app.measure_fault_logged {
        // once per process: the layout that was built on estimates is the thing to go and look at
        app.measure_fault_logged = true;
        log("text: a width was measured with NO FONT loaded — layout is on average-advance estimates");
    }
        // …and the same name as a reportable event, on CHANGE only. Per-frame would be a
        // firehose of one fact; what is worth knowing is which screens get used, which is a
        // transition count. `&'static str` from the table above, so nothing runtime-built can
        // reach the wire — see `diag::schema`.
        if fr.rn != app.last_route_reported {
            // The visit that just ended reports first, so the stream reads left-then-entered.
            end_route_visit(app, true);
            app.last_route_reported = fr.rn;
            plx_telemetry::diag::event(plx_telemetry::diag::schema::DiagEvent::RouteEntered { screen: fr.rn });
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
        // looping — is still observed. The probe reports nothing to `plx_machine::idle` in return: a
        // frame gate that a diagnostic could hold open would stop being measurable.
        //
        // `rn` is passed rather than re-derived so the fingerprint's `route=` is the same
        // string the heartbeat prints; the `Screen` beside it is what the probe DISPATCHES on,
        // and its match is exhaustive so a new route cannot fingerprint as nothing.
        let probe_screen = |app: &App| match app.route() {
                // The escape/retry/restart control is the screen's only focusable element, and
                // it appears and disappears on the `ESCAPE_AFTER_MS`/`QR_ESCAPE_AFTER_MS` clocks
                // with no phase change of its own — `focusprobe::Screen::Login`'s own doc says
                // why `focus_record().is_some()` decodes it exactly, the same one-element read
                // `Route::Profiles`/`Route::Onboard` already do below.
                AppArg::Login => crate::focusprobe::Screen::Login {
                    phase: app.bridge.auth_read().0.phase,
                    has_control: app.pages.focus_record().is_some(),
                },
                // Phase 6: the picker's cursor comes from the focus engine rather than from a
                // module global, exactly as `Route::Onboard` does below — see `Screen::Profiles`'s
                // own doc for the grammar this replaced and why a bare element number is as far as
                // this crate-level module can decode it without naming `screens::profiles`.
                AppArg::Profiles => crate::focusprobe::Screen::Profiles {
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
                AppArg::Onboard => {
                    let (list, row) = match app.pages.focus_record() {
                        Some((_, elem, _)) if elem < plx_screens::registry::BAND => {
                            (true, elem as i32)
                        }
                        _ => (false, -1),
                    };
                    crate::focusprobe::Screen::Onboard { list, row }
                }
                AppArg::Home => crate::focusprobe::Screen::Home,
                // (`Route::ItemMenu`'s arm stood here, mapping the popover's `MenuHost` to the
                // probe's own `Host` mirror and recursing into that screen's fields. The menu is a
                // SURFACE since phase 10: the page under it is the top page, which this line
                // already names as `route=`, and the panel's own fields ride on `content` like
                // every other surface's — `bridge::content_probe`.)
                AppArg::Library | AppArg::LibraryCollections { .. } | AppArg::LibraryRecommended { .. } => crate::focusprobe::Screen::Library,
                AppArg::Content(plx_screens::registry::ContentArg::Detail { .. }) =>
                    crate::focusprobe::Screen::Detail,
                AppArg::Content(plx_screens::registry::ContentArg::Person { .. }
                    | plx_screens::registry::ContentArg::Filmography { .. }) => crate::focusprobe::Screen::Person,
                AppArg::Content(plx_screens::registry::ContentArg::Collection(_)) =>
                    crate::focusprobe::Screen::Collection,
                AppArg::Content(plx_screens::registry::ContentArg::Catalog { .. }) =>
                    crate::focusprobe::Screen::Catalog,
                AppArg::Search => crate::focusprobe::Screen::Search,
                AppArg::Watchlist => crate::focusprobe::Screen::Watchlist,
                // The same words the heartbeat's `overlay=` uses, and — since phase 9 — from the
                // same place: the SURFACE that is up. There is no second table; see
                // `heartbeat_word_tests::focusprobe_player_overlay_delegates_to_the_shared_overlay_word_function`.
                AppArg::Player => crate::focusprobe::Screen::Player {
                    overlay: super::words::overlay_word(&app.pages, &app.route()).unwrap_or(super::words::NO_OVERLAY),
                },
                // Not pages: a surface never reaches the top of the PAGE stack, so none of these
                // can be the probe's `route=`. Named rather than swept into a `_` so a new page
                // variant is a compile error here rather than a silently mis-probed screen.
                AppArg::Settings(_)
                | AppArg::FirstRunConsent(_)
                | AppArg::LibraryMenu(_)
                | AppArg::AccountMenu
                | AppArg::WatchlistMenu(_)
                | AppArg::ItemMenu(_)
                | AppArg::PlayerOverlay(_)
                | AppArg::AltSources(_)
                | AppArg::VersionsPanel(_)
                | AppArg::TracksPanel(_)
                | AppArg::AboutPanel
                | AppArg::PersonBio
                | AppArg::CollectionAbout => crate::focusprobe::Screen::Home,
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
            crate::focusprobe::sample(&app.player.session, fr.rn, probe_screen(app), probe_hud(app), fr.ctrl, &content, app.bridge.metadata_view());
        }
        // The recorder's frame tail (spec §5.3): on an event frame the logical-state hash —
        // the press machine, the route and overlay words, the focus fingerprint and, since phase
        // 5b, the CONTAINER TREE's own hash, plus Session's cached logical subhash, sampled
        // here for the probe's own reason — then the frame's records flushed. Under a replay the
        // same hash is graded against the recorded one; the run ends when the recording does.
        //
        // The tree term is what makes a press inside the Settings family gradeable at all: none
        // of those screens has a global for the fingerprint to read, so without it every frame of
        // that whole flow hashed the same and a divergence there was structurally invisible.
        // Session similarly changes independently of those visible terms. Its cached digest is
        // read lazily by recorder_end_frame for both recording and replay; no raw state is logged.
        if !matches!(app.rec, super::recorder::Recplay::Off) {
            let content = super::bridge::content_probe(&app.pages, &app.bridge);
            let focus = crate::focusprobe::line(&app.player.session, fr.rn, probe_screen(app), probe_hud(app), fr.ctrl, &content, app.bridge.metadata_view());
            // The bare WORD, not the heartbeat's ` overlay=<word>` spelling: the prefix is that
            // line's grammar and has no business in a state hash (phase 10 item 4).
            let ov = super::words::overlay_word(&app.pages, &app.route()).unwrap_or("");
            let tree = app.pages.state_hash();
            let press = &app.input.press;
            let done = recorder_end_frame(&mut app.rec, &app.bridge, press, rn, ov, &focus, tree);
            if done {
                app.running = false;
            }
        }
        // `plxnative-focusx`: the card read-out belongs to the frame just sampled.
        #[cfg(feature = "devtriggers")]
        plx_ui::card_probe::clear();
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
            let (cards, cards_off) = plx_gfx::gfx::take_card_stats();
            app.instr.note_frame_counters(plx_base::diag::heartbeat::FrameCounters {
                uploads,
                upload_px,
                cards,
                cards_off,
            });
            // Taken on every presented frame for the counters' reason above: a span belongs to
            // the frame it ran in, never to the next slow one.
            let spans = plx_base::diag::spans::take();
            for line in app.instr.frame_drop_lines(&|| {
                format!(
                    "route={rn} dip={} load={} snapt={:.2} snap={:.3} {spans}{}",
                    app.pages.dip_word(),
                    plx_ui::glassload::step_index(),
                    app.bridge.home_snap_target(&app.pages),
                    app.bridge.home_snap_pos(&app.pages),
                    plx_platform::tv::window::frame_probe_fields()
                )
            }) {
                log(&line);
            }
        } else {
            // Spans recorded on a frame that did not present (`scen`/`pump`/`land`, `feed`, the
            // engine's `sfv`/`sfa`) belong to no FRAMEDROP line: left in place they would print on
            // the next presented frame's.
            let _ = plx_base::diag::spans::take();
        }
}

/// The once/sec heartbeat line and its counters.
pub(crate) unsafe fn heartbeat(app: &mut App, fr: &mut Frame) {
    let rn = fr.rn;
        if loop_tick(&mut app.iters_ct, &mut app.loop_t, &mut app.loop_shown, fr.now) {
            // Dev only: a deliberate main-thread stall, for exercising the freeze report.
            crate::dev::freeze_on_purpose();
            if crate::dev::scenarios::imagecache_stats_armed() {
                super::adapters::poster::log_cache_stats();
            }
            // once/sec render heartbeat — greppable without reading the on-screen counter.
            // The harness parses `loop=(\d+) route=(\w+)(?: overlay=(\w+))?` (tests/run.py), so
            // the player's overlay tag stays right after route= and worstframe= stays LAST.
            //
            // RENAMED 2026-08-01, and the old name was REUSED, so a log predating this reads
            // as the opposite of what it says: the field that used to be `FPS=` is now `loop=`,
            // and `fps=` now means what it always should have — frames actually presented,
            // previously `pres=`. An old `FPS=60` is a LOOP rate and says nothing about frames.
            let ov = super::words::overlay_suffix(&app.pages, &app.route());
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
            let playing = plx_media::player::is_playing(&app.player.session) && pos_ns > 0;
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
            let vp = if plx_media::player::is_playing(&app.player.session) {
                let (vtick, vgap) = plx_media::player::vplane_take();
                format!(" vtick={vtick} vgap={vgap}ms")
            } else {
                String::new()
            };
            // `fps=<n>` — frames actually SWAPPED this second, which is what `plx_machine::idle` moves,
            // and the only field here that is a frame rate. `loop=` counts LOOP iterations: it
            // is the app's liveness signal and `pos=` is anchored to it, so it must not read 0
            // on a screen that is merely idle. The pair is the diagnostic — `loop=62 fps=0` is
            // a settled screen doing its job, `loop=0` is an app in trouble, and `fps=0` on its
            // own is not a fault at all. The dev on-screen counter draws this same drained
            // value, cached below; it never reads a second presentation counter.
            let pres = plx_machine::idle::take_presents();
            #[cfg(feature = "devtools")]
            {
                app.fps_shown = pres.min(i32::MAX as u32) as i32;
            }
            // dev: which LOAD-DIAL step these frames belong to, the blur refreshes
            // actually TAKEN in that second. Absent unless the dial is armed, and placed
            // after `fps=` / before
            // `worstframe=` so both harness regexes are untouched. `snap=` is the one
            // thing a cadence claim cannot be trusted without: it is the rate that RAN,
            // not the rate that was requested.
            let ld = if plx_ui::glassload::armed() {
                format!(
                    " load={} snap={}",
                    plx_ui::glassload::step_index(),
                    plx_gfx::gfx::take_blur_snapshots()
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
                plx_base::diag::heartbeat::HeartbeatFields {
                    carried,
                    dropped,
                    admitted: budget.admitted,
                    refused: budget.refused,
                    solo: budget.solo.map(|c| c.name()),
                    evicted_hot: plx_gfx::text::take_evicted_hot(),
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
    ps: &mut plx_media::route::PlaybackSession,
    pa: &mut plx_media::player::adapter::PlayerAdapter,
) {
    plx_media::player::report::abandon_pending(ps);
    if is_started() {
        plx_media::player::stop_bufferfeed(ps, pa);
    }
    // The stop's `Unload` runs off-frame; the process must not exit inside it, nor with the
    // object it is releasing left undestroyed.
    pa.finish_release();
    // The stop scrobble is posted off-thread now, and this process is about to die with any
    // worker still running — so THIS is the one place its result has to be waited for, or the
    // resume point the user just earned is silently dropped. Same cost the old inline call
    // paid, except now it is paid once at exit instead of on every BACK out of a movie.
    plx_media::route::drain_scrobble();
    crate::capture::shutdown();
    crate::app::adapters::poster::shutdown();
    // A clean exit ends the release-health session as `exited`, after the native teardown: a death
    // anywhere above leaves its state for the next boot to classify as crashed or abnormal.
    plx_telemetry::telemetry::session::shutdown();
    SDL_Quit();
}

/// Re-inject one recorded input (`app::recorder`'s encodings) for the frame being replayed: SDL
/// kinds through the same synthesis the remote FIFO uses, direct tokens through the dispatcher.
/// An unknown kind is logged once per kind rather than silently skipped.
unsafe fn replay_inject(app: &mut App, fr: &mut Frame, v: &serde_json::Value) {
    if let Some(token) = super::recorder::direct_token(v) {
        if !ingress_token(app, fr, token) { app.rec.refuse("recorded direct token was not accepted"); }
        return;
    }
    if app.boot_initial.is_some() {
        match super::recorder::controlled_foreground(v) {
            Ok(Some(code)) => {
                app.rec.input(super::recorder::enc_lifecycle(code));
                // Same navigation owner as ordinary foreground, with platform and
                // player restoration kept behind actual SDL lifecycle ingress.
                super::bridge::foreground(&mut app.pages);
                plx_machine::idle::invalidate();
                return;
            }
            Err(reason) => { app.rec.refuse(reason); return; }
            Ok(None) => {}
        }
        match super::recorder::decode_input(v) {
            Ok(input) if input.source == plx_machine::machine::Source::Script => {
                // The typed initial scenario regenerates this internal step at its recorded
                // clock. Dispatch still observes and grades it once; it is not external ingress.
            }
            Ok(input) => controlled_key_input(app, input),
            Err(reason) => app.rec.refuse(reason),
        }
        return;
    }
    if super::bridge::search_owns_input(&app.pages) { drain_sdl(app, fr); }
    let kind = v["kind"].as_str().unwrap_or("");
    let i = |k: &str| v[k].as_i64().unwrap_or(0) as i32;
    let u = |k: &str| v[k].as_u64().unwrap_or(0) as u32;
    match kind {
        "text" => {
            if !super::bridge::search_owns_input(&app.pages) {
                log("replay: text has no owned Search recipient");
            } else if let Some(events) = super::recorder::dec_text(v) {
                app.inputs.extend(events);
                plx_machine::idle::invalidate();
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

fn controlled_replay(app: &App) -> bool {
    app.boot_initial.is_some() && matches!(&app.rec, super::recorder::Recplay::Replaying(_))
}

/// The same bounded physical-key bookkeeping for controlled recording and supplied replay.
/// The original source and event timestamp survive; replay does not synthesize a new SDL time.
unsafe fn controlled_key_input(app: &mut App, event: plx_machine::machine::InputEvent<u32>) {
    use plx_machine::machine::{InputKind, Key, Edge};
    if matches!(event.kind, InputKind::Pointer{..}|InputKind::Click{..}|InputKind::Drag{..}) {
        app.last_input=event.at.ms;
        app.ptr.last_motion=event.at.ms;
        app.ptr.cur_hidden=false;
        if matches!(event.kind,InputKind::Click{..}) { app.ptr.button_down=true; }
        plx_machine::idle::invalidate();
        app.inputs.push(event);
        return;
    }
    if matches!(event.kind,InputKind::Key { key:Key::Ok,sym:0,wcode:0,edge:Edge::Up,at_edge:false })
        && event.source==plx_machine::machine::Source::Sdl {
        app.last_input=event.at.ms;
        app.input.press.release(event.at.ms);
        app.ptr.button_down=false;
        app.inputs.push(event);
        return;
    }
    let InputKind::Key { key, sym, wcode, edge, at_edge: false } = event.kind else {
        app.rec.refuse("unsupported controlled Home input"); return;
    };
    if !matches!(key, Key::Up | Key::Down | Key::Left | Key::Right | Key::Ok | Key::Back)
        || !super::bridge::owns_input(&app.pages) {
        app.rec.refuse("unsupported controlled key destination"); return;
    }
    match edge {
        Edge::Up => on_key_up(sym, app.ok_armed, &mut app.down_sym, &mut app.input.press),
        Edge::Repeat if sym == app.down_sym => {
            if !app.modal_repeat.ready(event.at.ms) { return; }
        }
        _ => {
            app.last_input = event.at.ms;
            begin_fresh_press(&mut app.player.session, classify(sym, wcode), sym, wcode,
                event.at.ms, &mut app.down_sym, None, &mut app.ptr, &mut app.ok_armed, &mut app.input.press);
        }
    }
    app.inputs.push(event);
}

#[cfg(all(test, feature = "hostsim"))]
mod lifecycle_regression_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    };
    use std::time::Duration;

    fn poll_until(reason: &str, mut ready: impl FnMut() -> bool) {
        for _ in 0..5000 {
            if ready() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("{reason}");
    }

    /// `TX.paused` is a process-global and these lifecycle tests are serialized, so restore the
    /// previous value even when an assertion panics. The decoder-release park reads the clock
    /// through `viewer_paused()`, which is `TX.paused` minus a claim hold's loan.
    struct TransportPaused(bool);
    impl TransportPaused {
        /// Capture the current value without changing it: declare it BEFORE the rig, so it drops
        /// AFTER the rig's stop has reset the transport.
        fn keep() -> Self {
            Self(paused())
        }
        fn set(value: bool) -> Self {
            let previous = paused();
            set_paused(value);
            Self(previous)
        }
    }
    impl Drop for TransportPaused {
        fn drop(&mut self) {
            set_paused(self.0);
        }
    }

    // No boot, renderer or device: the real App/event/frame phases with Bridge's host rig.
    fn app() -> App {
        plx_media::player::SHARED.reset_session();
        let app = App {
            last_input: Default::default(),
            loop_t: Default::default(),
            iters_ct: Default::default(),
            loop_shown: Default::default(),
            #[cfg(feature = "devtools")]
            fps_shown: Default::default(),
            play_prev: None,
            running: Default::default(),
            window_activity: crate::app::window_activity::WindowActivity::new(),
            #[cfg(feature = "devtools")]
            buffer_flip_count: Default::default(),
            down_sym: Default::default(),
            modal_repeat: RepeatGate::IDLE,
            diagnostics: Default::default(),
            player: plx_media::player::machine::Player::new(),
            adapters: Adapters {
                player: plx_media::player::adapter::PlayerAdapter::new(unsafe {
                    plx_base::task::MainThread::assume()
                }),
            },
            repause_at: -1,
            decoder_parked: false,
            ok_armed: Default::default(),
            last_route_reported: Default::default(),
            ptr: Pointer::IDLE,
            menu_play_await: Default::default(),
            deeplink: Default::default(),
            prev: Default::default(),
            refresh_hubs: Default::default(),
            return_hold: Default::default(),
            plaintext_upgrade: Default::default(),
            reread: Default::default(),
            ev: [0; 128],
            remote: Default::default(),
            win: Default::default(),
            #[cfg(target_os = "linux")]
            wslg_frame_pacing: false,
            t0: Default::default(),
            instr: plx_base::diag::heartbeat::Instruments::new(false, 22.0),
            scenarios: crate::dev::scenarios::Scenarios {
            #[cfg(feature = "devtriggers")]
            poster_gate: Default::default(),
                pick_user: Default::default(),
                home_osc_last: Default::default(),
                hero_osc_last: Default::default(),
                home_fold_osc_last: Default::default(),
                home_fold_down: Default::default(),
                lib_osc_last: Default::default(),
                lib_switch_last: Default::default(),
                lib_switch_step: Default::default(),
                search_osc_last: Default::default(),
                settings_osc_last: Default::default(),
                settings_osc_down: Default::default(),
                modal_osc_last: Default::default(),
                legal_doc_tried: Default::default(),
                alert_tried: Default::default(),
                alert_step: Default::default(),
                account_osc_last: Default::default(),
                account_osc_down: Default::default(),
                consent_osc_last: Default::default(),
                consent_osc_down: Default::default(),
                onboard_osc_last: Default::default(),
                onboard_osc_right: Default::default(),
                nav_osc_last: Default::default(),
                marker_tried: Default::default(),
                press_tried: Default::default(),
                press_release_at: Default::default(),
                itemmenu_tried: Default::default(),
                acct_tried: Default::default(),
                acct_rest: Default::default(),
                shots: Default::default(),
                auto_tried: Default::default(),
                replay_left: Default::default(),
                grid_tried: Default::default(),
                settings_tried: Default::default(),
                seek_tried: Default::default(),
                seek_script: Default::default(),
                seek_script_at: Default::default(),
                seek_gap_ms: Default::default(),
                seek_script_last: Default::default(),
                quality_script: Default::default(),
                quality_script_at: Default::default(),
                quality_gap_ms: Default::default(),
                quality_tried: Default::default(),
                quality_playing_since: Default::default(),
                detail_tried: Default::default(),
                collection_tried: Default::default(),
                content_boot: Default::default(),
                play_tried: Default::default(),
                play_await: Default::default(),
                menu_tried: Default::default(),
                menupick_tried: Default::default(),
                menupick_target: Default::default(),
                subtiming: Default::default(),
                submenu_osc: Default::default(),
                more_osc: Default::default(),
                pause_tried: Default::default(),
                pause_script: Default::default(),
                pause_resume_at: Default::default(),
                push_bench: Default::default(),
                modal_bench: Default::default(),
                deep_bench: Default::default(),
                hero_bench: Default::default(),
                hold_bench: Default::default(),
                dev: crate::dev::scenarios::DevFlags {
                    detail_osc: Default::default(),
                    home_osc: Default::default(),
                    hero_osc: Default::default(),
                    home_fold_osc: Default::default(),
                    lib_osc: Default::default(),
                    lib_switch: Default::default(),
                    search_osc: Default::default(),
                    settings_boot: Default::default(),
                    settings_osc: Default::default(),
                    modal_osc: Default::default(),
                    legal_doc: Default::default(),
                    alert_boot: Default::default(),
                    account_osc: Default::default(),
                    consent_osc: Default::default(),
                    onboard_osc: Default::default(),
                    nav_osc: Default::default(),
                    nav_osc_rk: Default::default(),
                    nobudget: Default::default(),
                },
            },
            input: plx_ui::input::Input::new(),
            measure_fault_logged: Default::default(),
            rec: super::super::recorder::Recplay::Off,
            boot_initial: Default::default(),
            telemetry_guard: Default::default(),
            present: plx_machine::present::Present::new(),
            glass: Default::default(),
            pages: plx_ui::dispatch::Dispatcher::new(),
            inputs: Default::default(),
            bridge: super::super::bridge::Bridge::for_test(|| 0),
        };
        plx_media::route::reset_player_control_for_test(&app.player.session);
        app
    }

    fn frame(app: &mut App, now: u32) {
        super::super::bridge::frame(
            &mut app.pages,
            &mut app.bridge,
            plx_machine::machine::Tick {
                ms: now,
                dt_us: 16_000,
            },
            vec![],
        );
        app.bridge.assert_no_store_jobs_left_for_test();
    }

    fn event(app: &mut App, fr: &mut Frame, et: u32) {
        app.ev[..4].copy_from_slice(&et.to_ne_bytes());
        unsafe {
            ingest_sdl_event(app, fr);
        }
    }

    /// Crash-report consent for the test's duration, so `window::record` reaches its test capture;
    /// the previous answer comes back on drop.
    struct CrashReports(Option<plx_telemetry::telemetry::consent::Consent>);

    impl CrashReports {
        fn granted() -> Self {
            use plx_telemetry::telemetry::consent::{self, Consent};
            let previous = consent::current();
            let mut granted = Consent::default();
            granted.errors = true;
            granted.asked_version = consent::POLICY_VERSION;
            consent::install(granted);
            plx_telemetry::telemetry::window::take_recorded_for_test();
            Self(previous)
        }
    }

    impl Drop for CrashReports {
        fn drop(&mut self) {
            plx_telemetry::telemetry::consent::install(self.0.take().unwrap_or_default());
            plx_telemetry::telemetry::window::take_recorded_for_test();
        }
    }

    /// S12a: one WILL/DID background and foreground cycle leaves exactly these closed steps, in
    /// this order. `*_done` close the DID edges only (the WILL edge has not finished the step), and
    /// the rig holds no Wayland surface, so no `wayland_released` appears.
    #[test]
    fn background_cycle_records_closed_steps_in_order() {
        let _serial = plx_base::testlock::serial();
        let _consent = CrashReports::granted();
        let mut app = app();
        let mut fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
        for et in [0x103, 0x104, 0x105, 0x106] {
            event(&mut app, &mut fr, et);
        }
        assert_eq!(
            plx_telemetry::telemetry::window::take_recorded_for_test(),
            [
                "will_background",
                "did_background",
                "background_done",
                "will_foreground",
                "did_foreground",
                "foreground_done",
            ],
        );
    }

    /// `wayland_released` is a step only when a borrowed surface was actually released (the second
    /// of WILL + DID background finds none), so a reader can tell "released" from "nothing held".
    #[test]
    fn wayland_release_is_a_step_only_when_a_surface_was_held() {
        let _serial = plx_base::testlock::serial();
        let _consent = CrashReports::granted();
        release_borrowed_window(|| true);
        release_borrowed_window(|| false);
        assert_eq!(plx_telemetry::telemetry::window::take_recorded_for_test(), ["wayland_released"]);
    }

    /// The Watchlist page's `OpenCatalog` request reaches `content::watchlist_requests` and pushes
    /// the catalog page of the tile's id; a guid the service does not form pushes nothing.
    #[test]
    fn a_watchlist_open_catalog_request_pushes_the_catalog_page() {
        use plx_machine::machine::MachineId;
        use plx_screens::registry::{ContentArg, HomeTab};
        use plx_screens::watchlist::page::WatchlistOut;
        let _serial = plx_base::testlock::serial();
        let mut app = app();
        super::super::bridge::nav_root(&mut app.pages, AppArg::Home);
        frame(&mut app, 0);
        super::super::bridge::nav_tab(&mut app.pages, &mut app.bridge, HomeTab::Watchlist, None, None);
        for i in 1..40 {
            frame(&mut app, i * 16);
        }
        assert!(app.route() == AppArg::Watchlist);
        let instance = app.pages.nav.top_page().and_then(|e| e.inst.as_ref()).map(|i| i.id).expect("the page is mounted");
        let hex = [0x5d, 0x77, 0x6b, 0x1d, 0x96, 0xb6, 0x55, 0x00, 0x1f, 0x2a, 0x9c, 0x3e];
        let fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
        app.bridge.push_watchlist_req_for_test(MachineId::Instance(instance), WatchlistOut::OpenCatalog("plex://movie/not-a-hex".into()));
        content_requests(&mut app, &fr);
        for i in 40..80 {
            frame(&mut app, i * 16);
        }
        assert!(app.route() == AppArg::Watchlist, "a guid with no catalog id opens nothing");
        app.bridge.push_watchlist_req_for_test(
            MachineId::Instance(instance),
            WatchlistOut::OpenCatalog("plex://movie/5d776b1d96b655001f2a9c3e".into()),
        );
        content_requests(&mut app, &fr);
        for i in 80..140 {
            frame(&mut app, i * 16);
        }
        assert!(app.route() == AppArg::Content(ContentArg::Catalog { hex }));
    }

    /// **The Watchlist is left through the strip, the way it was entered** (owner, on the TV: "I
    /// can't leave Watchlist"). The whole loop: Home's strip opens the page, and OK on the Home pill
    /// takes the route back to Home through `content::watchlist_requests`. Every strip press used to
    /// die in the page, so the route stayed `watchlist` whatever was pressed but BACK.
    #[test]
    fn the_strip_leaves_the_watchlist_it_opened() {
        use plx_machine::machine::{InputEvent, Key, Tick};
        use plx_ui::widgets::{TAB_KEY_HOME, TAB_KEY_WATCHLIST};
        let _serial = plx_base::testlock::serial();
        let mut app = app();
        super::super::bridge::nav_root(&mut app.pages, AppArg::Home);
        let mut t = 0u32;
        let step = |app: &mut App, keys: Vec<InputEvent<u32>>, t: &mut u32| {
            for n in 0..20 {
                *t += 16;
                let mut fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
                fr.now = *t;
                let inputs = if n == 0 { keys.clone() } else { vec![] };
                super::super::bridge::frame(&mut app.pages, &mut app.bridge, Tick { ms: *t, dt_us: 16_000 }, inputs);
                content_requests(app, &fr);
                loop_requests(app);
            }
        };
        let press = |app: &mut App, key: Key, t: &mut u32| {
            let keys = super::super::bridge::script_key(key, Tick { ms: *t, dt_us: 16_000 });
            step(app, keys, t);
        };
        let focus = |app: &App| app.pages.focus().map(|k| k.elem);
        // Walk the strip to `want`: UP onto it, then RIGHT from its left end.
        let walk_to = |app: &mut App, want: u32, t: &mut u32| {
            for _ in 0..4 {
                if focus(app).is_some_and(|e| e >= plx_ui::dispatch::STRIP_BASE) { break; }
                press(app, Key::Up, t);
            }
            for _ in 0..app.pages.nav.tabs.strip.len() { press(app, Key::Left, t); }
            while focus(app) != Some(want) {
                let before = focus(app);
                press(app, Key::Right, t);
                assert_ne!(focus(app), before, "{want:#x} is on the strip");
            }
        };
        step(&mut app, vec![], &mut t);
        walk_to(&mut app, TAB_KEY_WATCHLIST, &mut t);
        press(&mut app, Key::Ok, &mut t);
        assert!(app.route() == AppArg::Watchlist, "the strip opens the Watchlist");
        walk_to(&mut app, TAB_KEY_HOME, &mut t);
        press(&mut app, Key::Ok, &mut t);
        assert_eq!(super::super::words::route_word(&app.route()), "home", "OK on the Home pill leaves the Watchlist");
    }

    /// **A strip tab's arrival seats in the page it opened, on every tab** (the Watchlist seated on
    /// the account chip). The mint asks for the page's group 0; a page whose content is still
    /// landing has none yet, and the fallback used to take the first non-empty group in the
    /// composed order — the strip, which the container lists first — at its left end, the chip.
    /// Search (its field) and the Library (its content) seat in the page; so must the Watchlist.
    #[test]
    fn a_strip_tab_arrival_seats_in_the_page_it_opened() {
        use plx_machine::machine::{InputEvent, Key, Tick};
        use plx_ui::widgets::{TAB_KEY_SEARCH, TAB_KEY_WATCHLIST};
        let _serial = plx_base::testlock::serial();
        for (tab, route) in [(TAB_KEY_SEARCH, "search"), (TAB_KEY_WATCHLIST, "watchlist")] {
            let mut app = app();
            super::super::bridge::nav_root(&mut app.pages, AppArg::Home);
            let mut t = 0u32;
            let step = |app: &mut App, keys: Vec<InputEvent<u32>>, t: &mut u32| {
                for n in 0..20 {
                    *t += 16;
                    let mut fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
                    fr.now = *t;
                    let inputs = if n == 0 { keys.clone() } else { vec![] };
                    super::super::bridge::frame(&mut app.pages, &mut app.bridge, Tick { ms: *t, dt_us: 16_000 }, inputs);
                    content_requests(app, &fr);
                    loop_requests(app);
                }
            };
            let press = |app: &mut App, key: Key, t: &mut u32| {
                let keys = super::super::bridge::script_key(key, Tick { ms: *t, dt_us: 16_000 });
                step(app, keys, t);
            };
            let focus = |app: &App| app.pages.focus().map(|k| k.elem);
            step(&mut app, vec![], &mut t);
            for _ in 0..4 {
                if focus(&app).is_some_and(|e| e >= plx_ui::dispatch::STRIP_BASE) { break; }
                press(&mut app, Key::Up, &mut t);
            }
            for _ in 0..app.pages.nav.tabs.strip.len() { press(&mut app, Key::Left, &mut t); }
            while focus(&app) != Some(tab) {
                let before = focus(&app);
                press(&mut app, Key::Right, &mut t);
                assert_ne!(focus(&app), before, "{tab:#x} is on the strip");
            }
            press(&mut app, Key::Ok, &mut t);
            assert_eq!(super::super::words::route_word(&app.route()), route);
            let page = app.pages.nav.top_page().map(|e| e.id);
            let seat = app.pages.focus();
            assert!(seat.is_some_and(|k| Some(k.entry) == page && k.elem < plx_ui::dispatch::STRIP_BASE),
                "{route}: the arrival seats in the page, not on the strip: {seat:?}");
        }
    }

    #[test]
    fn controlled_content_boot_waits_for_the_queued_home_root() {
        use plx_machine::machine::{Fx, MachineId, NavOp};
        let _serial = plx_base::testlock::serial();
        let mut app = app();
        let encoded = super::super::synthetic_home_initial(1, 32517, Some("flow12".into())).unwrap();
        app.boot_initial = Some(serde_json::from_str(&encoded).unwrap());
        app.pages.emit(MachineId::Nav, Fx::Nav(NavOp::Root(AppArg::Home)));
        let mut fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
        fr.now = 1500; // Cold startup already exceeded the scenario's 500 ms delay.
        crate::dev::scenarios::controlled_each_frame(&mut app, &mut fr);
        assert!(!app.scenarios.detail_tried, "Root(Home) has not mounted yet");
        frame(&mut app, fr.now);
        assert!(app.pages.top_screen().is_some());
        crate::dev::scenarios::controlled_each_frame(&mut app, &mut fr);
        assert!(app.scenarios.detail_tried);
        assert!(app.scenarios.content_boot.is_some());
        frame(&mut app, fr.now + 16);
        assert!(matches!(app.pages.top_arg(), Some(AppArg::Content(
            plx_screens::registry::ContentArg::Detail { rk, .. })) if rk == "1001"));
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn a_recorded_raw_hang_probe_replays_without_missing_input() {
        use super::super::{bootstrap::{Initial, Preflight}, recorder::{Recplay, ReplayMode, state_fp}};
        use plx_ui::{dispatch::Tap, rec::{Header, MemSink, Recording}};
        let _serial = plx_base::testlock::serial();
        let mut app = app();
        let mut fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
        let initial = Initial::synthetic_home(1, 32517, None).unwrap();
        let sink = MemSink::default();
        let segments = sink.segments.clone();
        let manifest = Header::new(state_fp(), &initial).to_json().to_string();
        app.rec = Recplay::recording_with_sink(&initial, Box::new(sink)).unwrap();
        app.rec.tick(initial.clock_start, 0.0);
        {
            let _frame = plx_base::task::FrameScope::enter();
            assert!(unsafe { ingress_token(&mut app, &mut fr, "hang-raw:1") });
        }
        app.rec.end_frame(&|| 7);
        std::mem::replace(&mut app.rec, Recplay::Off).finish(plx_machine::landgate::fixture_gate());
        let recording = Recording::parse(&manifest,
            &segments.borrow().iter().map(Vec::as_slice).collect::<Vec<_>>(), state_fp()).unwrap();
        assert_eq!(recording.frames[0].inputs[0]["tok"], "hang-raw:1");
        for controlled in [false, true] {
            let recording = Recording { header: recording.header.clone(), frames: recording.frames.clone(),
                metrics: recording.metrics.clone(), stopped_at: recording.stopped_at };
            app.boot_initial = controlled.then(|| initial.clone());
            app.rec = Recplay::controlled(Preflight::Replay {
                initial: initial.clone(), recording, mode: ReplayMode::Resolve }, &initial).unwrap();
            for value in app.rec.replay_inputs() {
                let _frame = plx_base::task::FrameScope::enter();
                unsafe { replay_inject(&mut app, &mut fr, &value); }
            }
            Tap::focus(&mut app.rec, 0, None);
            assert!(app.rec.end_frame(&|| 7));
            assert!(!app.rec.outcome_failed(), "the injected probe must consume its recorded input, controlled={controlled}");
        }
    }

    #[test]
    fn controlled_foreground_replays_once_without_platform_authority() {
        use super::super::{bootstrap::{Initial, Preflight}, recorder::{Recplay, ReplayMode, state_fp}};
        use plx_ui::{dispatch::Tap, rec::{Header, Recording}};
        use plx_machine::{machine::Tick};
        let _serial = plx_base::testlock::serial();
        for mode in [ReplayMode::Targets, ReplayMode::Resolve] {
            let mut app = app();
            let initial = Initial::synthetic_home(1, 32517, None).unwrap();
            let recording = Recording {
                header: Header::new(state_fp(), &initial),
                frames: vec![
                    plx_ui::rec::Frame { f: 0, tick: Some(Tick { ms: 0, dt_us: 0 }),
                        st: Some(7), focus: Some(None), ..Default::default() },
                    plx_ui::rec::Frame { f: 1, tick: Some(Tick { ms: 16, dt_us: 16_000 }),
                        present: Some(true), st: Some(7), focus: Some(None),
                        inputs: vec![serde_json::json!({"f":1,"t":"in","kind":"lifecycle","code":0x105}),
                            serde_json::json!({"f":1,"t":"in","kind":"lifecycle","code":0x106})],
                        ..Default::default() },
                ], metrics: Default::default(), stopped_at: None,
            };
            app.boot_initial = Some(initial.clone());
            app.rec = Recplay::controlled(Preflight::Replay { initial: initial.clone(), recording, mode },
                &initial).unwrap();
            Tap::focus(&mut app.rec, 0, None);
            assert!(!app.rec.end_frame(&|| 7));

            app.pages.suspend();
            app.window_activity.event(0x104);
            let revision = plx_plex::plex::grant::revision();
            let mut fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
            for value in app.rec.replay_inputs() {
                unsafe { replay_inject(&mut app, &mut fr, &value); }
            }
            assert!(!app.pages.nav.suspended, "recorded foreground reaches the navigation owner");
            assert!(!app.window_activity.allow_present(true),
                "a recording cannot authorize EGL presentation while the real window is backgrounded");
            assert_eq!(plx_plex::plex::grant::revision(), revision,
                "recorded foreground has no network-grant authority");

            // The actual compositor's startup pair may arrive at another frame. It
            // restores physical window safety without consuming extra tape inputs.
            let mut restored_windows = 0;
            for code in [0x105u32, 0x106, SDL_KEYDOWN] {
                app.ev[..4].copy_from_slice(&code.to_ne_bytes());
                unsafe { ingest_sdl_event_with_window(&mut app, &mut fr, |_| restored_windows += 1); }
            }
            assert_eq!(restored_windows, 1, "only the real DID foreground restores native handles");
            assert!(app.window_activity.allow_present(true));
            assert!(app.inputs.is_empty(), "ambient keys cannot enter the recorded scenario");
            assert_eq!(plx_plex::plex::grant::revision(), revision);
            app.rec.present(true);
            Tap::focus(&mut app.rec, 1, None);
            assert!(app.rec.end_frame(&|| 7));
            assert!(!app.rec.outcome_failed(), "each recorded foreground input is graded exactly once");

            app.running = true;
            event(&mut app, &mut fr, 0x104);
            assert!(!app.running);
            assert!(!app.window_activity.allow_present(true));
            assert_eq!(app.rec.failure(), Some("live window exit interrupted controlled replay"));
        }
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    #[should_panic(expected = "main-thread block: dev hang probe")]
    fn hang_probe_dispatch_uses_frame_guard() {
        let _serial = plx_base::testlock::serial();
        let mut app = app();
        let mut fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
        let _scope = plx_base::task::FrameScope::enter();
        unsafe { ingress_token(&mut app, &mut fr, "hang:1"); }
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn hang_probe_dispatch_raw_bypasses_frame_guard() {
        let _serial = plx_base::testlock::serial();
        let mut app = app();
        let mut fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
        let _scope = plx_base::task::FrameScope::enter();
        assert!(unsafe { ingress_token(&mut app, &mut fr, "hang-raw:1") });
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn hang_probe_dispatch_preserves_key_order() {
        let _serial = plx_base::testlock::serial();
        // Only SDL's event subsystem: no window, renderer, boot or device.
        assert_eq!(unsafe { SDL_Init(0x4000) }, 0);
        struct Events;
        impl Drop for Events {
            fn drop(&mut self) { unsafe { SDL_Quit(); } }
        }
        let _events = Events;
        for fifo in [true, false] {
            let mut app = app();
            super::super::bridge::show_page(&mut app.pages,
                AppArg::Settings(plx_screens::family::SettingsPage::Root));
            frame(&mut app, 0);
            let mut fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
            if fifo {
                app.remote = Some(crate::remote::Remote::buffered_for_test("down\nhang:1\nup\n"));
            }
            // The labelled probe's guard panic is the observation boundary BEFORE sleeping.
            // No final drain can make an incorrectly ordered dispatch pass this assertion.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _scope = plx_base::task::FrameScope::enter();
                unsafe {
                    if fifo {
                        ingest(&mut app, &mut fr);
                    } else {
                        // LAB delivers commands to precisely this same entry point.
                        assert!(ingress_token(&mut app, &mut fr, "down"));
                        assert!(app.inputs.is_empty(), "key should still be pending in SDL");
                        ingress_token(&mut app, &mut fr, "hang:1");
                        ingress_token(&mut app, &mut fr, "up");
                    }
                }
            }));
            let panic = result.expect_err("labelled probe must reach its frame guard");
            let message = panic.downcast_ref::<String>().map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied()).unwrap_or("");
            assert!(message.contains("main-thread block: dev hang probe"), "{message}");
            assert_eq!(app.inputs.len(), 2,
                "both earlier key edges must be handled before the probe (fifo={fifo})");
        }
    }

    #[test]
    fn remote_fifo_exposes_independent_pointer_edges() {
        use plx_machine::machine::{Edge, InputKind, Key};
        let _serial = plx_base::testlock::serial();
        // SDL's event subsystem only: no window, renderer, native playback or device.
        assert_eq!(unsafe { SDL_Init(0x4000) }, 0);
        struct Events;
        impl Drop for Events {
            fn drop(&mut self) { unsafe { SDL_Quit(); } }
        }
        let _events = Events;
        let mut app = app();
        super::super::bridge::show_page(&mut app.pages, AppArg::Player);
        frame(&mut app, 0);
        let mut fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
        for (token, held) in [("pd:900,870", true), ("pm:1200,870", true),
            ("pm:1400,870", true), ("pu:1400,870", false)] {
            assert!(unsafe { ingress_token(&mut app, &mut fr, token) },
                "the FIFO must express each edge of a held pointer: {token}");
            unsafe { drain_sdl(&mut app, &mut fr); }
            assert_eq!(app.ptr.button_down, held, "{token}");
        }
        assert_eq!(app.inputs.len(), 4, "no hidden motions, clicks or releases");
        assert!(matches!(app.inputs[0].kind, InputKind::Click { x: 900.0, y: 870.0, .. }));
        assert!(matches!(app.inputs[1].kind, InputKind::Drag { x: 1200.0, y: 870.0, .. }));
        assert!(matches!(app.inputs[2].kind, InputKind::Drag { x: 1400.0, y: 870.0, .. }));
        assert!(matches!(app.inputs[3].kind, InputKind::Key { key: Key::Ok, edge: Edge::Up, .. }));

        // Deliver the same collected edges through the dispatcher and the real player, one
        // frame per edge. Screen tests separately grade these coordinates against record_stops;
        // this map isolates the FIFO -> SDL -> app -> dispatcher -> scrub ownership boundary.
        use plx_ui::{screen::{Activate, Hover, Stop}, Rect};
        use plx_machine::{machine::{FocusKey, Tick}};
        use plx_screens::registry::PlayerReq;
        let key = FocusKey { elem: plx_appkit::player_hud::ELEM_SCRUB,
            ..app.pages.focus().expect("the player has a scrub seat") };
        let rect = plx_appkit::player_hud::scrub_hit_rect();
        app.pages.input.hit.fill(vec![Stop { key, rect, rest_rect: rect, clip: Rect::FULL,
            hover: Hover::Ignore, activate: Activate::Direct }]);
        app.pages.input.hit.swap();
        plx_media::player::SHARED.duration_ns.store(100_000_000_000, Ordering::Relaxed);
        for (index, input) in std::mem::take(&mut app.inputs).into_iter().enumerate() {
            super::super::bridge::frame(&mut app.pages, &mut app.bridge,
                Tick { ms: 1_000 + index as u32 * 500, dt_us: 16_000 }, vec![input]);
            let reqs = app.bridge.take_player_reqs();
            if index < 3 {
                assert!(reqs.is_empty(), "a held drag previews without any transport request");
                let page = super::super::bridge::player(&app.pages).unwrap();
                assert!(page.scrub.drag);
                assert!(page.scrub.ns > 0);
            } else {
                let expected = (plx_appkit::player_hud::scrub_frac_x(1400.0) as f64 * 100_000_000_000.0) as i64;
                assert_eq!(reqs, vec![PlayerReq::CommitSeek(expected)], "release commits once at the final motion");
            }
        }
        // Neither the key's tap debounce nor a repeated pointer-up can replay this seek.
        assert!(unsafe { ingress_token(&mut app, &mut fr, "pu:1400,870") });
        unsafe { drain_sdl(&mut app, &mut fr); }
        super::super::bridge::frame(&mut app.pages, &mut app.bridge,
            Tick { ms: 4_000, dt_us: 16_000 }, std::mem::take(&mut app.inputs));
        assert!(app.bridge.take_player_reqs().is_empty());
    }

    #[test]
    fn controlled_sdl_pointer_gestures_match_ordinary_ingress() {
        let _serial=plx_base::testlock::serial();
        let mut observed=Vec::new();
        for controlled in [false,true] {
            let mut app=app();
            super::super::bridge::show_page(&mut app.pages,
                AppArg::Settings(plx_screens::family::SettingsPage::Root));
            frame(&mut app,0);
            if controlled {
                app.boot_initial=Some(super::super::bootstrap::Initial::synthetic_home(1,32517,Some("root".into())).unwrap());
            }
            let mut fr=Frame::begin(&app.player.session, app.bridge.metadata_view());
            let mut steps=Vec::new();
            for (et,x,y) in [(SDL_MOUSEBUTTONDOWN,200i32,300i32),
                (SDL_MOUSEMOTION,400,300),(SDL_MOUSEBUTTONUP,400,300)] {
                clock::set_replay(100);
                app.ev[20..24].copy_from_slice(&x.to_ne_bytes());
                app.ev[24..28].copy_from_slice(&y.to_ne_bytes());
                event(&mut app,&mut fr,et);
                steps.push((app.inputs.iter().map(|e|super::super::bootstrap::effects::input(e).unwrap()).collect::<Vec<_>>(),
                    app.ptr.button_down,app.ptr.prev_mx.to_bits(),app.ptr.mot_accum.to_bits()));
                app.inputs.clear();
            }
            app.ev=encode_key(plx_ui::consts::SDLK_DOWN,0,true);
            event(&mut app,&mut fr,SDL_KEYDOWN);
            assert!(app.ptr.dpad_mode);
            assert_eq!(app.ptr.mot_accum,0.0);
            steps.push((app.inputs.iter().map(|e|super::super::bootstrap::effects::input(e).unwrap()).collect::<Vec<_>>(),
                app.ptr.button_down,app.ptr.prev_mx.to_bits(),app.ptr.mot_accum.to_bits()));
            app.inputs.clear();
            app.ev=[0;128];
            app.ev[24..28].copy_from_slice(&300i32.to_ne_bytes());
            for x in [405i32,410,415] {
                app.ev[20..24].copy_from_slice(&x.to_ne_bytes());
                event(&mut app,&mut fr,SDL_MOUSEMOTION);
                steps.push((app.inputs.iter().map(|e|super::super::bootstrap::effects::input(e).unwrap()).collect::<Vec<_>>(),
                    app.ptr.button_down,app.ptr.prev_mx.to_bits(),app.ptr.mot_accum.to_bits()));
                app.inputs.clear();
            }
            observed.push(steps);
        }
        assert_eq!(observed[0],observed[1],"arming capture must preserve SDL gesture policy and bookkeeping");
    }

    #[test]
    fn controlled_sdl_records_one_owned_event_after_hitmap_for_each_pointer_edge() {
        use plx_ui::{rec::{MemSink,Header,Recording}, screen::{Stop,Hover,Activate}, Rect};
        use plx_machine::{machine::Tick};
        let _serial=plx_base::testlock::serial();
        let mut app=app();
        super::super::bridge::show_page(&mut app.pages,AppArg::Settings(plx_screens::family::SettingsPage::Root));
        frame(&mut app,0);
        let key=app.pages.focus().unwrap();
        app.pages.input.hit.fill(vec![Stop {key,rect:Rect::FULL,rest_rect:Rect::FULL,clip:Rect::FULL,
            hover:Hover::Focus,activate:Activate::Press}]);
        app.pages.input.hit.swap();
        let initial=super::super::bootstrap::Initial::synthetic_home(1,32517,Some("root".into())).unwrap();
        let sink=MemSink::default();
        let segments=sink.segments.clone();
        let manifest=Header::new(super::super::recorder::state_fp(),&initial).to_json().to_string();
        app.rec=super::super::recorder::Recplay::recording_with_sink(&initial,Box::new(sink)).unwrap();
        app.rec.arm_landgate(app.bridge.landgate());
        app.boot_initial=Some(initial);
        app.rec.prepare_resources(&mut app.bridge);
        app.rec.tick(100,0.016);
        clock::set_replay(100);
        let mut fr=Frame::begin(&app.player.session, app.bridge.metadata_view());
        for et in [SDL_MOUSEBUTTONDOWN,SDL_MOUSEMOTION,SDL_MOUSEBUTTONUP] {
            app.ev[20..24].copy_from_slice(&400i32.to_ne_bytes());
            app.ev[24..28].copy_from_slice(&300i32.to_ne_bytes());
            event(&mut app,&mut fr,et);
        }
        assert_eq!(app.inputs.len(),3);
        app.pages.frame_with(&mut app.bridge,Tick {ms:100,dt_us:16000},
            std::mem::take(&mut app.inputs),Vec::new(),&mut app.rec,false);
        app.rec.measurements(&app.bridge);
        app.rec.end_frame(&||0);
        app.rec.finish(app.bridge.landgate());
        let recording=Recording::parse(&manifest,
            &segments.borrow().iter().map(Vec::as_slice).collect::<Vec<_>>(),super::super::recorder::state_fp()).unwrap();
        let inputs=&recording.frames[0].inputs;
        assert_eq!(inputs.len(),3,"never record raw+owned duplicates");
        assert!(inputs.iter().all(|v|v["kind"]=="owned"));
        assert_eq!(inputs[0]["body"]["kind"],"click");
        assert_eq!(inputs[1]["body"]["kind"],"pointer");
        for v in &inputs[..2] { assert_eq!(v["body"]["hit"],key.elem); }
        assert_eq!(inputs[2]["body"]["edge"],"Up");
        plx_machine::landgate::disarm();
    }

    /// The fixture's original answers, verbatim: `/decision` direct-plays, anything else is the
    /// PlayQueue (two episodes, or one film when `successor` is false).
    fn queue_body(request: &str, successor: bool) -> &'static str {
        if request.contains("/decision?") {
            r#"{"MediaContainer":{"Metadata":[{"Media":[{"Part":[{"decision":"directplay"}]}]}]}}"#
        } else if !successor {
            r#"{"MediaContainer":{"machineIdentifier":"lifecycle-fixture","size":1,
            "playQueueID":1,"playQueueSelectedItemID":11,"playQueueTotalCount":1,
            "Metadata":[
            {"ratingKey":"1","playQueueItemID":11,"type":"movie","title":"First",
             "duration":60000,
             "Media":[{"videoCodec":"h264","audioCodec":"aac","width":1280,"height":720,
             "Part":[{"id":1,"key":"/library/parts/1/file.mkv","Stream":[
             {"id":1,"streamType":1,"codec":"h264"},
             {"id":2,"streamType":2,"codec":"aac","selected":true}]}]}]}]}}"#
        } else {
            r#"{"MediaContainer":{"machineIdentifier":"lifecycle-fixture","size":2,
            "playQueueID":1,"playQueueSelectedItemID":11,"playQueueTotalCount":2,
            "Metadata":[
            {"ratingKey":"1","playQueueItemID":11,"type":"episode","title":"First",
             "parentIndex":1,"index":1,"duration":60000,
             "Media":[{"videoCodec":"h264","audioCodec":"aac","width":1280,"height":720,
             "Part":[{"id":1,"key":"/library/parts/1/file.mkv","Stream":[
             {"id":1,"streamType":1,"codec":"h264"},
             {"id":2,"streamType":2,"codec":"aac","selected":true}]}]}]},
            {"ratingKey":"2","playQueueItemID":12,"type":"episode","title":"Next",
             "parentIndex":1,"index":2,"duration":60000,
             "Media":[{"videoCodec":"h264","audioCodec":"aac",
             "Part":[{"id":2,"key":"/library/parts/2/file.mkv"}]}]}]}}"#
        }
    }

    struct Rig {
        app: App,
        sid: plx_plex::plex::ServerId,
        release: mpsc::Sender<()>,
        entered: mpsc::Receiver<()>,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Rig {
        /// `None` means `plx_base::task::spawn_small_keeping` was refused by the OS (Finding 4: the
        /// rig must honour `task.rs`'s "a refused spawn is a return value, not a panic" contract
        /// instead of `.expect`-ing it into a panic that reads as a product regression). Nothing
        /// past the spawn point has been armed yet at that moment — `register_for_test` and
        /// `app()` both run AFTER a successful spawn — so the only cleanup owed on the refused
        /// path is re-running `reset_servers_for_test()` to leave the registry exactly as clean
        /// as it was on entry; the listener, channels and `Arc`s all drop normally when this
        /// function returns `None`.
        ///
        /// **RED observed for this contract, SIMULATED (not a real OS refusal):** temporarily
        /// change the `match plx_base::task::spawn_small_keeping(...)` below to unconditionally
        /// evaluate to `None` (discarding the real handle), leaving the closure and everything
        /// else untouched, then run the five tests in this module. Before this fix that
        /// substitution panicked every one of them — at the old `.expect("spawn lifecycle
        /// fixture")` here, and, once that alone was removed, at `self.worker.take().unwrap()` in
        /// `Drop for Rig` — each reading exactly like a product regression in code the failing
        /// test never touches. After this fix the same substitution makes all five tests print a
        /// `SKIPPED …` line naming the reason and return cleanly, with no panic anywhere. The
        /// substitution was reverted before committing; the host's real thread budget was never
        /// actually exhausted, so this is a SIMULATED red, not an observed historical one.
        fn new() -> Option<Rig> {
            Self::serving(true)
        }

        /// `successor: false` serves a one-row queue — a film, with no Up Next to hand off to — so
        /// an end of stream leaves the player instead of starting the next item.
        fn serving(successor: bool) -> Option<Rig> {
            Self::serving_with(move |request: &str| queue_body(request, successor).to_owned())
        }

        /// [`Self::serving`] with the server's answers chosen by `respond(request line)`. The first
        /// request still parks until [`Self::resolve`] releases it.
        fn serving_with(respond: impl Fn(&str) -> String + Send + 'static) -> Option<Rig> {
            plx_plex::plex::reset_servers_for_test();
            plx_media::route::retire_pending_stops(); // the stop mailbox is process-global
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let (release, wait) = mpsc::channel();
            let (arrived, entered) = mpsc::channel();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let log = requests.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = stop.clone();
            let worker = match plx_base::task::spawn_small_keeping("lifecycle-fixture", move || {
                let mut first = true;
                while !stopping.load(Ordering::Acquire) {
                    let (mut socket, _) = match plx_base::testnet::accept(&listener) {
                        Ok(v) => v,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                            continue;
                        }
                        Err(e) => panic!("fixture accept: {e}"),
                    };
                    // Darwin accept inherits the nonblocking listener flag. Reads must wait
                    // for request bytes rather than racing the loopback client's first write.
                    socket.set_nonblocking(false).unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let mut reader = BufReader::new(socket.try_clone().unwrap());
                    let mut request = String::new();
                    reader.read_line(&mut request).unwrap();
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" || line.is_empty() {
                            break;
                        }
                        if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            length = n.trim().parse().unwrap();
                        }
                    }
                    reader.read_exact(&mut vec![0; length]).unwrap();
                    log.lock().unwrap().push(request.clone());
                    if first {
                        first = false;
                        arrived.send(()).unwrap();
                        let _ = wait.recv_timeout(Duration::from_secs(5));
                    }
                    let body = respond(&request);
                    // a handler answers `!503` for a refusal: a 5xx with an empty body
                    let (status, body) = match body.as_str() {
                        "!503" => ("503 Service Unavailable", String::new()),
                        _ => ("200 OK", body),
                    };
                    let _ = write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                }
            }) {
                Some(h) => h,
                None => {
                    // See the doc comment above: nothing past this point has run yet, so undoing
                    // the initial reset is the whole cleanup owed.
                    plx_plex::plex::reset_servers_for_test();
                    return None;
                }
            };
            let sid = plx_plex::plex::register_for_test(
                "lifecycle-fixture",
                "127.0.0.1",
                port as i32,
                "synthetic",
                "lifecycle-fixture",
            );
            let mut app = app();
            super::super::bridge::nav_root(&mut app.pages, AppArg::Home);
            frame(&mut app, 0);
            Some(Self {
                app,
                sid,
                release,
                entered,
                requests,
                stop,
                worker: Some(worker),
            })
        }

        fn request(&mut self) {
            assert!(plx_media::route::request_play(
                &mut self.app.player.session,
                self.app.bridge.metadata_mut(),
                self.sid,
                "1",
                "/library/parts/1/file.mkv",
                "h264",
                "aac",
                "First",
                "",
                None,
            ));
            self.entered.recv_timeout(Duration::from_secs(3)).unwrap();
        }

        fn accept_start(&mut self) {
            assert!(start_playback(
                &mut self.app.player.session,
                &mut self.app.adapters.player,
                0,
                Origin::Here,
                HUD_LINGER_MS,
                None,
                &mut self.app.pages,
                &mut self.app.bridge
            ));
        }

        fn resolve(&mut self) {
            self.release.send(()).unwrap();
            poll_until("fixture plan did not land", || {
                plx_media::route::pump_play(&mut self.app.player.session, self.app.bridge.metadata_mut()).is_some()
            });
            assert_eq!(
                plx_media::route::up_next(&self.app.player.session).unwrap().rk,
                "2"
            );
        }

        fn due_up_next(&mut self) -> Frame {
            self.request();
            self.resolve();
            self.expire_up_next_countdown()
        }

        /// The resolved item's Player page, mounted, with its Up Next countdown run out.
        fn expire_up_next_countdown(&mut self) -> Frame {
            // A mounted, resolved Player whose Engine has already drained still owns its
            // session and successor. No native decoding is needed to exercise the countdown.
            super::super::bridge::nav_push(&mut self.app.pages, AppArg::Player);
            frame(&mut self.app, 16);
            let player = super::super::bridge::player_mut(&mut self.app.pages).unwrap();
            player.up_next.tick(
                plx_appkit::player_hud::ControlSlot::UpNext(plx_data::metadata::Marker {
                    kind: plx_data::metadata::MarkerKind::Credits,
                    start_ms: 0,
                    end_ms: 60_000,
                    final_seg: true,
                }),
                20,
            );
            let mut fr = Frame::begin(&self.app.player.session, self.app.bridge.metadata_view());
            fr.now = 20 + plx_appkit::up_next::COUNTDOWN_MS;
            assert!(player.up_next.expired(fr.now));
            fr
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = self.release.send(());
            plx_media::route::cancel_play(&mut self.app.player.session);
            // `cancel_play` withdraws the landing but leaves the detached resolve worker running,
            // and that worker finishes by sending a `stop?closeResourceSession=1` for its
            // abandoned plan to the server its ServerId names. `ServerId`s repeat from test to
            // test, so a worker that outlives this rig delivered that stop to the NEXT rig's
            // fixture as its first request, which is what
            // `did_background_cancels_accepted_resolve_before_player_mount` then read as a
            // resolve that had cleaned up early. The fixture is still serving here (it is stopped
            // below) and `release` is already sent, so the worker can finish.
            let _ = plx_media::route::wait_resolve_workers_for_test(Duration::from_secs(10));
            plx_media::player::stop_bufferfeed(
                &mut self.app.player.session,
                &mut self.app.adapters.player,
            );
            plx_media::route::drain_scrobble();
            self.stop.store(true, Ordering::Release);
            // Finding `lifecycle-rig-drop-still-unwraps-join`: a panicking fixture worker must
            // not re-panic here. `.join().unwrap()` used to propagate the worker's `Err` into
            // this destructor, and a panic inside a destructor while another panic is already
            // unwinding is a Rust abort (SIGABRT) that takes down the whole test binary — every
            // other module's result in that `make check` run along with it. `plx_base::task::join`
            // (task.rs's own documented contract: a bare `.join()` outside that module is a
            // stall nobody can see) logs a panicked worker instead of re-panicking, and the
            // `if let` tolerates an absent handle on the refused-spawn path this rig can no
            // longer actually reach (`Rig` is only ever constructed with `worker: Some(..)`).
            if let Some(h) = self.worker.take() {
                plx_base::task::join("lifecycle-fixture", h);
            }
            plx_media::player::SHARED.reset_session();
            plx_media::route::reset_player_control_for_test(&self.app.player.session);
            plx_plex::plex::reset_servers_for_test();
        }
    }

    // ---- track memory: what the viewer picked on one episode is what the next episode plays -----

    /// One stream of a [`show_episode`], as the server describes it.
    fn show_stream(id: i64, kind: i64, index: i64, codec: &str, lang: Option<(&str, &str)>, selected: bool) -> String {
        let (code, name) = lang.unwrap_or(("", ""));
        format!(
            r#"{{"id":{id},"streamType":{kind},"index":{index},"codec":"{codec}","channels":{},
            "languageCode":"{code}","language":"{name}","selected":{selected}}}"#,
            if kind == 2 { 6 } else { 0 },
        )
    }

    /// An episode of show 4242 with English and Japanese E-AC3 5.1 and English and Spanish SRT.
    /// `audio` and `subs` list `(language, id)` in file order; the server has the FIRST audio and
    /// the SPANISH subtitle selected, which is what plays unless something remembered overrides it.
    fn show_episode(rk: &str, item: i64, index: i64, audio: [(&str, i64); 2], subs: [(&str, i64); 2]) -> String {
        let name = |code: &str| match code {
            "eng" => "English",
            "jpn" => "Japanese",
            _ => "Spanish",
        };
        let mut streams = vec![show_stream(item * 10 + 1, 1, 0, "hevc", None, false)];
        for (i, (code, id)) in audio.into_iter().enumerate() {
            streams.push(show_stream(id, 2, 1 + i as i64, "eac3", Some((code, name(code))), i == 0));
        }
        for (i, (code, id)) in subs.into_iter().enumerate() {
            streams.push(show_stream(id, 3, 3 + i as i64, "srt", Some((code, name(code))), code == "spa"));
        }
        format!(
            r#"{{"ratingKey":"{rk}","playQueueItemID":{item},"type":"episode","title":"Episode {index}",
            "grandparentRatingKey":"4242","parentIndex":1,"index":{index},"duration":60000,
            "Media":[{{"videoCodec":"hevc","audioCodec":"eac3","width":1920,"height":1080,"bitrate":8000,
            "Part":[{{"id":{item},"key":"/library/parts/{rk}/file.mkv","Stream":[{}]}}]}}]}}"#,
            streams.join(","),
        )
    }

    /// E1's streams and E2's: the SAME tracks under other ids and in another order, so only a
    /// remembered DESCRIPTOR (never a stream id) can carry a pick from one to the other.
    const E1_AUDIO: [(&str, i64); 2] = [("eng", 1012), ("jpn", 1013)];
    const E1_SUBS: [(&str, i64); 2] = [("spa", 1014), ("eng", 1015)];
    const E2_AUDIO: [(&str, i64); 2] = [("eng", 2012), ("jpn", 2013)];
    const E2_SUBS: [(&str, i64); 2] = [("eng", 2014), ("spa", 2015)];

    /// A PMS that serves the two episodes of show 4242 per ratingKey: the PlayQueue carries both,
    /// `/library/metadata/<rk>` answers with the one asked for, `/decision` direct-plays, and every
    /// other request (a PUT, the show's preferences, a timeline) is an empty container. It keeps NO
    /// state: a `PUT /library/parts` selection is never echoed into the next episode's answer, so
    /// nothing but the app's own memory can carry a pick from E1 to E2.
    fn show_fixture(request: &str) -> String {
        serve_show(request, show_episode("1", 11, 1, E1_AUDIO, E1_SUBS), show_episode("2", 12, 2, E2_AUDIO, E2_SUBS))
    }

    /// [`show_fixture`]'s answers for any pair of episode bodies `e1` and `e2`.
    fn serve_show(request: &str, e1: String, e2: String) -> String {
        let path = request.split_whitespace().nth(1).unwrap_or("").split('?').next().unwrap_or("");
        if request.contains("/decision?") {
            queue_body(request, true).to_owned()
        } else if path.starts_with("/playQueues") {
            format!(
                r#"{{"MediaContainer":{{"machineIdentifier":"lifecycle-fixture","size":2,"playQueueID":1,
                "playQueueSelectedItemID":11,"playQueueTotalCount":2,"Metadata":[{e1},{e2}]}}}}"#
            )
        } else if request.starts_with("GET ") && path == "/library/metadata/1" {
            format!(r#"{{"MediaContainer":{{"size":1,"Metadata":[{e1}]}}}}"#)
        } else if request.starts_with("GET ") && path == "/library/metadata/2" {
            format!(r#"{{"MediaContainer":{{"size":1,"Metadata":[{e2}]}}}}"#)
        } else {
            r#"{"MediaContainer":{"size":0}}"#.to_owned()
        }
    }

    /// An episode of show 4242 for Forced only: English and Japanese E-AC3 5.1, and three SRT tracks
    /// `(language, id, forced)` in file order. NO subtitle is selected on the server, so what plays
    /// is whatever the viewer's memory says.
    fn forced_show_episode(
        rk: &str, item: i64, index: i64, audio: [(&str, i64); 2], subs: [(&str, i64, bool); 3],
    ) -> String {
        let name = |code: &str| if code == "jpn" { "Japanese" } else { "English" };
        let mut streams = vec![show_stream(item * 10 + 1, 1, 0, "hevc", None, false)];
        for (i, (code, id)) in audio.into_iter().enumerate() {
            streams.push(show_stream(id, 2, 1 + i as i64, "eac3", Some((code, name(code))), i == 0));
        }
        for (i, (code, id, forced)) in subs.into_iter().enumerate() {
            streams.push(format!(
                r#"{{"id":{id},"streamType":3,"index":{},"codec":"srt","channels":0,
                "languageCode":"{code}","language":"{}","forced":{forced},"selected":false}}"#,
                3 + i as i64,
                name(code),
            ));
        }
        format!(
            r#"{{"ratingKey":"{rk}","playQueueItemID":{item},"type":"episode","title":"Episode {index}",
            "grandparentRatingKey":"4242","parentIndex":1,"index":{index},"duration":60000,
            "Media":[{{"videoCodec":"hevc","audioCodec":"eac3","width":1920,"height":1080,"bitrate":8000,
            "Part":[{{"id":{item},"key":"/library/parts/{rk}/file.mkv","Stream":[{}]}}]}}]}}"#,
            streams.join(","),
        )
    }

    /// [`show_fixture`] for Forced only. E2 has the same tracks as E1 under other ids and in another
    /// order (the Japanese forced track first), so only the remembered MODE can find E2's.
    fn forced_show_fixture(request: &str) -> String {
        serve_show(
            request,
            forced_show_episode("1", 11, 1, E1_AUDIO, [("eng", 1014, false), ("eng", 1015, true), ("jpn", 1016, true)]),
            forced_show_episode("2", 12, 2, E2_AUDIO, [("jpn", 2014, true), ("eng", 2015, true), ("eng", 2016, false)]),
        )
    }

    /// Sign the fixture in as the account owner and give track memory a mirror seeded for that
    /// account, empty: the state of a stored-session boot (the harness's seated cases).
    fn seated_with_empty_memory(tag: &str) -> plx_plex::plex::session::TempSession {
        let session = plx_plex::plex::session::TempSession::new(tag);
        session.seated("", "acct");
        plx_media::route::seed_memory_for_test::<plx_plex::plex::session::prefs::TrackMemoryEntry>(
            &plx_plex::plex::session::Session {
                client_id: "cid-1".into(),
                account_token: "acct".into(),
                ..Default::default()
            },
        );
        session
    }

    impl Rig {
        /// Play E1 of [`show_fixture`] and wait for its plan to land.
        fn play_show_episode_one(&mut self) {
            assert!(plx_media::route::request_play(
                &mut self.app.player.session,
                self.app.bridge.metadata_mut(),
                self.sid,
                "1",
                "/library/parts/1/file.mkv",
                "hevc",
                "eac3",
                "Episode 1",
                "",
                None,
            ));
            self.entered.recv_timeout(Duration::from_secs(3)).unwrap();
            self.resolve();
        }

        /// Pick the subtitle `id` through the track menu, as OK on its row, and perform the commit
        /// the way the loop does.
        fn pick_subtitle(&mut self, id: i64) {
            let meta = self.app.bridge.metadata_view();
            let playing = meta.playing().expect("the item is installed");
            let index = playing.subs.iter().position(|s| s.id == id).unwrap_or_else(|| panic!("no subtitle {id}"));
            let mut menu = plx_appkit::track_menu::TrackMenuState::new(&self.app.player.session, meta, 1, Vec::new());
            let plx_appkit::track_menu::TrackOk::Commit { commit, .. } = menu.commit_sub_track(index, meta) else {
                panic!("the menu committed nothing for subtitle {id}");
            };
            super::super::playback::commit_track(&mut self.app.player.session, commit);
        }

        /// Pick the Subtitles tab's Forced only row through the track menu: focus it, OK, and perform
        /// the commit the way the loop does.
        fn pick_forced_only(&mut self) {
            let meta = self.app.bridge.metadata_view();
            let mut menu = plx_appkit::track_menu::TrackMenuState::new(&self.app.player.session, meta, 1, Vec::new());
            assert!(menu.focus_id(plx_appkit::track_menu::TrackRow::SubForcedOnly), "the Forced only row is on the page");
            let plx_appkit::track_menu::TrackOk::Commit { commit, .. } = menu.on_ok(meta) else {
                panic!("the menu committed nothing for Forced only");
            };
            super::super::playback::commit_track(&mut self.app.player.session, commit);
        }

        /// Pick the audio track `id` through the track menu: focus its row, OK.
        fn pick_audio(&mut self, id: i64) {
            let meta = self.app.bridge.metadata_view();
            let playing = meta.playing().expect("the item is installed");
            let index = playing.audio.iter().position(|s| s.id == id).unwrap_or_else(|| panic!("no audio {id}"));
            let mut menu = plx_appkit::track_menu::TrackMenuState::new(&self.app.player.session, meta, 0, Vec::new());
            assert!(menu.focus_id(plx_appkit::track_menu::TrackRow::Audio(index)), "the audio row is on the page");
            let plx_appkit::track_menu::TrackOk::Commit { commit, .. } = menu.on_ok(meta) else {
                panic!("the menu committed nothing for audio {id}");
            };
            super::super::playback::commit_track(&mut self.app.player.session, commit);
        }

        /// Advance to the queue's next episode the way the countdown does (the loop's
        /// `play_up_next`, run by `playback_tick` once the countdown expires), and wait for its plan.
        fn advance_to_next_episode(&mut self) {
            let mut fr = self.expire_up_next_countdown();
            unsafe {
                playback_tick(&mut self.app, &mut fr);
            }
            assert!(plx_media::route::play_pending(), "the countdown requested the successor");
            poll_until("E2's plan did not land", || {
                plx_media::route::pump_play(&mut self.app.player.session, self.app.bridge.metadata_mut()).is_some()
            });
            assert_eq!(plx_media::route::cur_rk(&self.app.player.session), "2");
        }
    }

    /// **The bug, end to end (Step 0).** A viewer who picks a subtitle and an audio track on episode 1
    /// finds them again on episode 2 of the same show: the pick goes through the track menu's OK
    /// and the loop's `commit_track`, the next episode is resolved against a server that remembers
    /// nothing, and E2's plan carries the remembered English subtitle and Japanese audio instead of
    /// the server's Spanish and English.
    #[test]
    fn a_pick_on_one_episode_is_what_the_next_episode_plays() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving_with(show_fixture) else {
            eprintln!("SKIPPED a_pick_on_one_episode_is_what_the_next_episode_plays: no fixture worker thread");
            return;
        };
        let _session = seated_with_empty_memory("track-memory-step-0");

        rig.play_show_episode_one();
        let ps = &rig.app.player.session;
        assert_eq!(plx_media::route::cur_sub_sid(ps), 1014, "E1 starts on the server's Spanish subtitle");
        assert_eq!(plx_media::route::cur_audio_sid(ps), 1012, "and its first audio, English");
        rig.pick_subtitle(1015);
        rig.pick_audio(1013);
        let ps = &rig.app.player.session;
        assert_eq!((plx_media::route::cur_sub_sid(ps), plx_media::route::cur_audio_sid(ps)), (1015, 1013), "the picks stood");

        rig.advance_to_next_episode();
        let ps = &rig.app.player.session;
        assert_eq!(
            (plx_media::route::cur_sub_sid(ps), plx_media::route::cur_audio_sid(ps)),
            (2014, 2013),
            "E2 shows the English subtitle and plays the Japanese audio the viewer chose on E1 \
             (the server's own selection there is Spanish, English)"
        );
    }

    /// **Forced only, end to end (FO, §4.9).** The viewer picks the Subtitles tab's Forced only row on
    /// episode 1 (which shows its English forced track), and episode 2, whose forced tracks have
    /// other ids and sit in another order, draws ITS English forced track: the mode is what was
    /// remembered, not a track. An audio switch on E2 then re-points the subtitle to the Japanese
    /// forced track, which a remembered TRACK would not do.
    #[test]
    fn forced_only_on_one_episode_draws_the_next_episodes_forced_track() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving_with(forced_show_fixture) else {
            eprintln!("SKIPPED forced_only_on_one_episode_draws_the_next_episodes_forced_track: no fixture worker thread");
            return;
        };
        let _session = seated_with_empty_memory("track-memory-forced-only");

        rig.play_show_episode_one();
        let ps = &rig.app.player.session;
        assert_eq!(
            (plx_media::route::cur_sub_sid(ps), plx_media::route::cur_audio_sid(ps)),
            (0, 1012),
            "E1 starts with no subtitle (the server selected none) and its first audio"
        );
        rig.pick_forced_only();
        let ps = &rig.app.player.session;
        assert_eq!(
            (plx_media::route::cur_sub_sid(ps), plx_media::route::cur_sub_mode(ps)),
            (1015, plx_media::route::SubMode::ForcedOnly),
            "Forced only drew E1's English forced track"
        );

        rig.advance_to_next_episode();
        let ps = &rig.app.player.session;
        assert_eq!(
            (plx_media::route::cur_sub_sid(ps), plx_media::route::cur_sub_mode(ps)),
            (2015, plx_media::route::SubMode::ForcedOnly),
            "E2 draws ITS English forced track (not the Japanese one listed first, not the full track)"
        );
        assert_eq!(plx_media::route::cur_audio_sid(ps), 2012, "under the English audio it plays");

        rig.pick_audio(2013);
        let ps = &rig.app.player.session;
        assert_eq!(
            (plx_media::route::cur_audio_sid(ps), plx_media::route::cur_sub_sid(ps)),
            (2013, 2014),
            "the Japanese audio re-points Forced only at the Japanese forced track"
        );
    }

    /// **The GL-while-backgrounded audit (S12a), as far as a host can run it.** One iteration of
    /// `run`'s loop from the playback tick to the end of `present_and_swap`, the way `step` runs
    /// its first half, and then the two phases `step` leaves out. With the gate closed
    /// (`fr.present == false`) `present_and_swap` takes only its `else` branch, so this is the
    /// whole frame the loop runs between WILL background and DID foreground.
    fn backgrounded_iteration(app: &mut App, t: &mut u32) -> bool {
        app.bridge.assert_no_store_jobs_left_for_test();
        *t += 16;
        let mut fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
        fr.now = *t;
        fr.dt = 0.016;
        let was_player = super::super::bridge::player(&app.pages).is_some();
        unsafe { playback_tick(app, &mut fr); }
        super::super::bridge::frame(&mut app.pages, &mut app.bridge,
            plx_machine::machine::Tick { ms: *t, dt_us: 16_000 }, std::mem::take(&mut app.inputs));
        if was_player && super::super::bridge::player(&app.pages).is_none() {
            restore_played_entry(app);
        }
        content_requests(app, &fr);
        loop_requests(app);
        unsafe {
            update(app, &mut fr);
            prepare_window(app, &mut fr);
            if !fr.present {
                present_and_swap(
                    app,
                    &mut fr,
                    #[cfg(all(feature = "hostsim", target_os = "linux"))]
                    None,
                );
            }
        }
        fr.present
    }

    /// Is a texture specified or deleted (through `gfx::tex_ledger`, which every `upload_rgba` and
    /// `delete_tex` goes through) anywhere between WILL background and DID foreground, on the frames
    /// the loop runs while the player page is mounted? The meter is proved live first: a delete
    /// moves it. Not covered, because they bypass the ledger: VAO/VBO/FBO creation and deletion, and
    /// `glCopyTexSubImage2D` into existing textures (the snapshot, blur, field and ground-probe
    /// captures), all of which sit under `if fr.present` or in `frame_end` calls that do.
    #[test]
    fn no_texture_work_between_background_and_did_foreground() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::new() else {
            eprintln!("SKIPPED no_texture_work_between_background_and_did_foreground: \
                 lifecycle fixture worker thread could not be spawned");
            return;
        };
        let before_probe = plx_gfx::gfx::tex_ledger::mutations();
        plx_gfx::gfx::delete_tex(0xfeed);
        assert_eq!(plx_gfx::gfx::tex_ledger::mutations(), before_probe + 1, "the meter is live");

        let mut fr = rig.due_up_next();
        assert!(super::super::bridge::player(&rig.app.pages).is_some(), "the player page is mounted");
        let mut t = fr.now;
        let start = plx_gfx::gfx::tex_ledger::mutations();
        event(&mut rig.app, &mut fr, 0x103);
        event(&mut rig.app, &mut fr, 0x104);
        for i in 0..30 {
            let presented = backgrounded_iteration(&mut rig.app, &mut t);
            assert!(!presented, "iteration {i} presented while backgrounded");
        }
        assert_eq!(
            plx_gfx::gfx::tex_ledger::mutations(), start,
            "texture work between background and DID foreground"
        );
        event(&mut rig.app, &mut fr, 0x105);
        assert!(!rig.app.window_activity.allow_present(true), "WILL foreground authorizes nothing");
        event(&mut rig.app, &mut fr, 0x106);
        assert!(rig.app.window_activity.allow_present(true), "DID foreground allows the next present");
    }

    /// A launch link that arrives while the window is backgrounded acts on nothing until DID
    /// foreground: the page stays where it was and no request leaves, then the link lands.
    #[test]
    fn a_link_waits_for_the_foreground() {
        use plx_screens::registry::ContentArg;
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving_with(|request: &str| {
            if request.starts_with("GET /library/metadata/777") {
                return r#"{"MediaContainer":{"size":1,"Metadata":[
                    {"ratingKey":"777","type":"movie","title":"Linked","duration":60000}]}}"#.into();
            }
            queue_body(request, false).to_owned()
        }) else {
            eprintln!("SKIPPED a_link_waits_for_the_foreground: fixture worker thread could not be spawned");
            return;
        };
        // The fixture parks its first answer until released; release it ahead of time.
        rig.release.send(()).unwrap();
        let _ = plx_platform::tv::launch::take_all();
        let sid = rig.sid;
        let top = |app: &App| app.pages.nav.tabs.stack.entries.last().map(|e| e.arg.clone());
        let linked = AppArg::Content(ContentArg::Detail { sid, rk: "777".into() });
        let held = AppArg::Content(ContentArg::Detail { sid, rk: "other".into() });
        super::super::bridge::nav_push(&mut rig.app.pages, held.clone());
        let mut t = 0;
        settle(&mut rig.app, &mut t);
        assert!(top(&rig.app) == Some(held.clone()), "premise: a detail page is on top");

        let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
        event(&mut rig.app, &mut fr, 0x103);
        event(&mut rig.app, &mut fr, 0x104);
        plx_platform::tv::launch::offer(
            r#"{"event":"launch","parameters":{"plex":{"ratingKey":"777","action":"open"}}}"#);
        for i in 0..30 {
            assert!(!backgrounded_iteration(&mut rig.app, &mut t), "iteration {i} presented while backgrounded");
        }
        assert!(top(&rig.app) == Some(held), "the link moved the page while the window was hidden");
        assert!(!rig.requests.lock().unwrap().iter().any(|r| r.contains("/library/metadata/777")),
            "the link asked the server for its item while the window was hidden");

        event(&mut rig.app, &mut fr, 0x105);
        event(&mut rig.app, &mut fr, 0x106);
        for _ in 0..1500 {
            // A test build parks a detail fetch until the test runs it, off the frame.
            rig.app.bridge.run_held_store_jobs_for_test();
            step(&mut rig.app, &mut t, vec![]);
            if top(&rig.app).as_ref() == Some(&linked) { break; }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(top(&rig.app) == Some(linked), "the link lands once the window is foregrounded");
    }

    /// A rig whose server also knows the link's item (777): the stage for the tests of a link that
    /// arrives over playback.
    fn linked_rig(name: &str) -> Option<Rig> {
        let rig = Rig::serving_with(|request: &str| {
            if request.starts_with("GET /library/metadata/777") {
                return r#"{"MediaContainer":{"size":1,"Metadata":[
                    {"ratingKey":"777","type":"movie","title":"Linked","duration":60000}]}}"#.into();
            }
            queue_body(request, true).to_owned()
        });
        if rig.is_none() {
            eprintln!("SKIPPED {name}: fixture worker thread could not be spawned");
        }
        rig
    }

    /// The film started and its player mounted, as the first half of `refuse_restore` does it.
    fn start_film(rig: &mut Rig) {
        rig.request();
        rig.resolve();
        rig.accept_start();
        frame(&mut rig.app, 16);
        let _ = plx_platform::tv::launch::take_all();
    }

    const LINK_777: &str = r#"{"event":"launch","parameters":{"plex":{"ratingKey":"777","action":"open"}}}"#;

    /// Frame the app until the link's detail page is on top (or the budget runs out).
    fn run_until_linked(rig: &mut Rig, t: &mut u32) -> bool {
        use plx_screens::registry::ContentArg;
        let linked = AppArg::Content(ContentArg::Detail { sid: rig.sid, rk: "777".into() });
        for _ in 0..1500 {
            // A test build parks a detail fetch until the test runs it, off the frame.
            rig.app.bridge.run_held_store_jobs_for_test();
            step(&mut rig.app, t, vec![]);
            if rig.app.pages.nav.tabs.stack.entries.last().map(|e| &e.arg) == Some(&linked) { return true; }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    /// A launch link over a playing film ends the film the way STOP does, then lands: the engine
    /// is released and the foreground lifecycle is Idle, not just the player page popped.
    #[test]
    fn a_link_over_the_player_stops_it() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = linked_rig("a_link_over_the_player_stops_it") else { return };
        start_film(&mut rig);
        assert!(super::super::bridge::player(&rig.app.pages).is_some() && rig.app.adapters.player.is_live(),
            "premise: the film is playing");
        plx_platform::tv::launch::offer(LINK_777);
        let mut t = 16;
        assert!(run_until_linked(&mut rig, &mut t), "the link lands over the film");
        settle(&mut rig.app, &mut t);
        assert!(!rig.app.adapters.player.is_live(), "the engine outlived the link");
        assert!(super::super::bridge::player(&rig.app.pages).is_none());
        assert_eq!(rig.app.player.lifecycle.state(), ForegroundState::Idle);
    }

    /// A film parked by the background, then a link, then the foreground: the DID must not bring
    /// the film back behind the page the link opened.
    #[test]
    fn a_link_over_a_parked_film_does_not_bring_it_back() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = linked_rig("a_link_over_a_parked_film_does_not_bring_it_back") else { return };
        start_film(&mut rig);
        let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
        event(&mut rig.app, &mut fr, 0x103);
        event(&mut rig.app, &mut fr, 0x104);
        plx_platform::tv::launch::offer(LINK_777);
        let mut t = 16;
        for _ in 0..5 { backgrounded_iteration(&mut rig.app, &mut t); }
        event(&mut rig.app, &mut fr, 0x105);
        event(&mut rig.app, &mut fr, 0x106);
        assert!(run_until_linked(&mut rig, &mut t), "the link lands once foregrounded");
        settle(&mut rig.app, &mut t);
        assert!(super::super::bridge::player(&rig.app.pages).is_none(), "the film came back");
        assert!(!rig.app.adapters.player.is_live(), "the film came back");
        assert_eq!(rig.app.player.lifecycle.state(), ForegroundState::Idle);
    }

    /// The link arrives in the very DID foreground that wakes a parked film: the DID must restore
    /// nothing (no Prepare, no Load in flight, no engine), so the link's Exit retires a parked
    /// session instead of racing a restore.
    #[test]
    fn a_warm_link_at_did_restores_nothing() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = linked_rig("a_warm_link_at_did_restores_nothing") else { return };
        start_film(&mut rig);
        assert_eq!(plx_plex::plex::current_server(), rig.sid, "premise: a server-less link resolves");
        let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
        event(&mut rig.app, &mut fr, 0x103);
        event(&mut rig.app, &mut fr, 0x104);
        assert!(rig.app.player.lifecycle.awaiting_load(), "premise: the film is parked");
        plx_platform::tv::launch::offer(LINK_777);
        event(&mut rig.app, &mut fr, 0x105);
        event(&mut rig.app, &mut fr, 0x106);
        assert!(matches!(rig.app.player.lifecycle.state(), ForegroundState::Suspended { .. }),
            "the DID claimed a restore the link was about to end (state: {:?})",
            rig.app.player.lifecycle.state());
        assert!(rig.app.player.lifecycle.pending_load_attempt().is_none()
            && !rig.app.player.lifecycle.flight_pending(),
            "the DID claimed a restore the link was about to end");
        assert!(!rig.app.adapters.player.is_live(), "the DID started the film's engine");
        let mut t = 32;
        assert!(run_until_linked(&mut rig, &mut t), "the link lands");
        settle(&mut rig.app, &mut t);
        assert!(super::super::bridge::player(&rig.app.pages).is_none());
        assert_eq!(rig.app.player.lifecycle.state(), ForegroundState::Idle);
    }

    /// A link that arrives while the window is hidden ends nothing until the DID: an Exit drained
    /// in the background would tear the session down (scrobble, transcode stop, hub refetch) while
    /// hidden, and a 0x106 right behind it would find the link already leaving and restore the
    /// film just to be stopped. The Exit and the superseding DID happen together at the DID.
    #[test]
    fn a_link_arriving_in_the_background_ends_playback_only_at_the_did() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = linked_rig("a_link_arriving_in_the_background_ends_playback_only_at_the_did") else { return };
        start_film(&mut rig);
        let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
        event(&mut rig.app, &mut fr, 0x103);
        event(&mut rig.app, &mut fr, 0x104);
        plx_platform::tv::launch::offer(LINK_777);
        let mut t = 16;
        backgrounded_iteration(&mut rig.app, &mut t);
        assert!(!rig.app.deeplink.is_leaving(), "the link started leaving while the window was hidden");
        event(&mut rig.app, &mut fr, 0x105);
        event(&mut rig.app, &mut fr, 0x106);
        assert!(matches!(rig.app.player.lifecycle.state(), ForegroundState::Suspended { .. }),
            "the DID restored a film the link ends (state: {:?})", rig.app.player.lifecycle.state());
        assert!(run_until_linked(&mut rig, &mut t), "the link lands");
        settle(&mut rig.app, &mut t);
        assert_eq!(rig.app.player.lifecycle.state(), ForegroundState::Idle);
    }

    #[test]
    fn did_background_cancels_accepted_resolve_before_player_mount() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::new() else {
            eprintln!(
                "SKIPPED did_background_cancels_accepted_resolve_before_player_mount: \
                 lifecycle fixture worker thread could not be spawned"
            );
            return;
        };
        rig.request();
        rig.accept_start();
        assert!(super::super::bridge::player(&rig.app.pages).is_none());
        assert!(plx_media::route::play_pending());
        let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
        fr.now = 16;
        event(&mut rig.app, &mut fr, 0x104);
        assert!(
            !plx_media::route::play_pending(),
            "DID must cancel the pre-mount resolve"
        );
        assert!(
            rig.app.player.lifecycle.awaiting_load(),
            "accepted playback must be suspended"
        );
        assert!(!rig.app.window_activity.allow_present(true));
        assert!(
            !rig.requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.contains("closeResourceSession=1")),
            "the blocked resolve cannot clean up before it is released: {:?}", rig.requests.lock().unwrap()
        );
        rig.release.send(()).unwrap();
        // Give the real worker its successful response after cancellation, then run the
        // production landing/pump phases while the queued Player page is still uncommitted.
        poll_until("cancelled worker did not retire its completed plan", || {
            unsafe {
                playback_tick(&mut rig.app, &mut fr);
                update(&mut rig.app, &mut fr);
            }
            assert!(
                !rig.app.adapters.player.is_live(),
                "late resolve started a background Engine"
            );
            // The real worker retires its abandoned plan only after finishing resolution.
            // Observe that effect instead of assuming a sleep was long enough for publication.
            rig
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.contains("closeResourceSession=1"))
        });
        assert!(!plx_media::route::has_url(&rig.app.player.session));
        let requests = rig.requests.lock().unwrap();
        let cleanup = requests
            .iter()
            .find(|r| r.contains("closeResourceSession=1"))
            .expect("the completed late plan must retire its exact server resource");
        assert!(cleanup.contains("session="), "{cleanup}");
        assert!(cleanup.contains("X-Plex-Session-Identifier="), "{cleanup}");
        drop(requests);
        frame(&mut rig.app, 32);
        assert!(super::super::bridge::player(&rig.app.pages).is_some());
        assert!(rig.app.player.lifecycle.awaiting_load());
    }

    #[test]
    fn did_background_suspends_created_engine_before_player_mount() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::new() else {
            eprintln!(
                "SKIPPED did_background_suspends_created_engine_before_player_mount: \
                 lifecycle fixture worker thread could not be spawned"
            );
            return;
        };
        rig.request();
        rig.resolve();
        rig.accept_start();
        assert!(
            rig.app.adapters.player.is_live(),
            "fixture must create the actual Engine"
        );
        assert!(super::super::bridge::player(&rig.app.pages).is_none());
        let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
        event(&mut rig.app, &mut fr, 0x104);
        assert!(
            !rig.app.adapters.player.is_live(),
            "DID must retire the pre-mount Engine"
        );
        assert!(rig.app.player.lifecycle.awaiting_load());
        assert!(
            plx_media::route::has_url(&rig.app.player.session),
            "suspension preserves the session"
        );
        let suspended = rig.app.player.lifecycle.state();
        event(&mut rig.app, &mut fr, 0x103);
        assert_eq!(
            rig.app.player.lifecycle.state(), suspended,
            "duplicate background must not resnapshot"
        );
    }

    /// **A decoder the pipeline takes back parks the session, and the next press brings it back.**
    /// Device log, 2026-10-13: a paused direct play under the screensaver got `type=18 num=600`
    /// with no app-switch edge, and every Play after it was refused until the app was closed. The
    /// release now parks exactly as an app switch does (session and page kept, Engine dropped),
    /// and the first press is spent on the foreground claim that rebuilds it; the press after
    /// that is an ordinary one again. This test pins a press that does not ask to play: DOWN is
    /// only a wake press, so the saved Paused clock is kept and the paused-seek preroll shows the
    /// resume frame. PLAY, PLAYPAUSE and a click are pinned by their own tests below.
    #[test]
    fn a_decoder_released_by_the_pipeline_parks_until_the_next_press_restores_it() {
        let _serial = plx_base::testlock::serial();
        let _paused = TransportPaused::keep();
        let Some(mut rig) = Rig::new() else {
            eprintln!(
                "SKIPPED a_decoder_released_by_the_pipeline_parks_until_the_next_press_restores_it: \
                 lifecycle fixture worker thread could not be spawned"
            );
            return;
        };
        rig.request();
        rig.resolve();
        rig.accept_start();
        assert!(rig.app.adapters.player.is_live(), "fixture must create the actual Engine");
        // The measured decoder-release case: a paused video behind the screensaver.
        set_paused(true);
        let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());

        plx_media::player::note_decoder_revoked_for_test();
        unsafe { playback_tick(&mut rig.app, &mut fr); }
        assert!(!rig.app.adapters.player.is_live(), "the dead pipeline is dropped");
        assert!(rig.app.player.lifecycle.awaiting_load(), "…and the session parked for a restore");
        assert!(plx_media::route::has_url(&rig.app.player.session), "the park keeps the session");
        let parked = rig.app.player.lifecycle.state();
        unsafe { playback_tick(&mut rig.app, &mut fr); }
        assert_eq!(rig.app.player.lifecycle.state(), parked, "nothing rebuilds it before a press");

        rig.app.ev = encode_key(plx_ui::consts::SDLK_DOWN, 0, true);
        event(&mut rig.app, &mut fr, SDL_KEYDOWN);
        assert!(!rig.app.player.lifecycle.awaiting_load(), "the press claimed the restore");
        assert!(rig.app.inputs.is_empty(), "…and was spent on it, not handed to the page");
        assert!(
            matches!(
                rig.app.player.lifecycle.state(),
                ForegroundState::LoadPending { clock: ForegroundClock::Paused, .. }
            ),
            "a non-PLAY wake press keeps the saved pause, got {:?}",
            rig.app.player.lifecycle.state()
        );
        assert!(
            plx_media::player::seek_preroll_active(),
            "…and rebuilds through the paused-seek preroll"
        );
        assert!(
            !claim_press_after_decoder_release(&mut rig.app, WakePress::Wake),
            "the next press is ordinary"
        );
    }

    /// The press that wakes a decoder-release park in [`wake_a_decoder_park`].
    #[derive(Clone, Copy, Debug)]
    enum WakeWith {
        Key { sym: c_uint, wcode: c_uint },
        /// A click on no control (the hit map is empty: the rig never presents a frame).
        Click,
        /// A click with a scrub-bar stop registered over the whole screen.
        ClickOnControl,
    }

    /// A started direct play, parked by a decoder release with the viewer's clock `paused`, then
    /// woken by `press`. Returns the rig with the foreground Load the press claimed still pending,
    /// or `None` when the fixture worker could not be spawned. The guard is the FIRST field and the
    /// caller binds the tuple, so the rig drops first (its stop resets the transport), then the
    /// guard puts the previous `TX.paused` back.
    fn wake_a_decoder_park(paused: bool, press: WakeWith) -> Option<(TransportPaused, Rig, Frame)> {
        let guard = TransportPaused::keep();
        let mut rig = Rig::new()?;
        rig.request();
        rig.resolve();
        rig.accept_start();
        assert!(rig.app.adapters.player.is_live(), "fixture must create the actual Engine");
        set_paused(paused);
        let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());

        plx_media::player::note_decoder_revoked_for_test();
        unsafe { playback_tick(&mut rig.app, &mut fr); }
        assert!(rig.app.player.lifecycle.awaiting_load(), "{press:?}: the session parked");

        match press {
            WakeWith::Key { sym, wcode } => {
                rig.app.ev = encode_key(sym, wcode, true);
                event(&mut rig.app, &mut fr, SDL_KEYDOWN);
            }
            WakeWith::Click | WakeWith::ClickOnControl => {
                if matches!(press, WakeWith::ClickOnControl) {
                    // The stop `top_at` finds, registered the way the scrub test above does,
                    // over the whole screen so no coordinate mapping can miss it.
                    use plx_ui::{screen::{Activate, Hover, Stop}, Rect};
                    use plx_machine::machine::{EntryId, FocusKey};
                    // `top_at` reads only the rect, so any entry will do.
                    let key = FocusKey { entry: EntryId(0), elem: plx_appkit::player_hud::ELEM_SCRUB };
                    let rect = Rect::FULL;
                    rig.app.pages.input.hit.fill(vec![Stop {
                        key, rect, rest_rect: rect, clip: Rect::FULL,
                        hover: Hover::Ignore, activate: Activate::Direct,
                    }]);
                    rig.app.pages.input.hit.swap();
                }
                rig.app.ev[20..24].copy_from_slice(&960i32.to_ne_bytes());
                rig.app.ev[24..28].copy_from_slice(&540i32.to_ne_bytes());
                event(&mut rig.app, &mut fr, SDL_MOUSEBUTTONDOWN);
            }
        }
        assert!(!rig.app.player.lifecycle.awaiting_load(), "{press:?}: the press claimed the restore");
        assert!(rig.app.inputs.is_empty(), "{press:?}: …and was spent on it, not handed to the page");
        Some((guard, rig, fr))
    }

    /// Land the foreground Load a wake press launched, as the pump would on `loadCompleted`, and
    /// run the frame that applies its clock.
    fn land_the_wake_load(rig: &mut Rig, fr: &mut Frame) {
        let attempt = rig
            .app
            .player
            .lifecycle
            .pending_load_attempt()
            .expect("the wake press launched a tracked Load");
        assert!(plx_media::route::settle_route_start(
            &mut rig.app.player.session,
            attempt,
            plx_media::route::RouteStartResult::Started,
        ));
        unsafe { playback_tick(&mut rig.app, fr); }
    }

    /// **A press that asks to play wakes a PAUSED decoder-release park Playing, on the first
    /// press.** The park saves the viewer's clock — Paused in the measured screensaver case. PLAY
    /// always asks to play; PLAYPAUSE and a pointer click are the player's play/pause toggle
    /// (`PlayerScreen`'s click on no control), which on a paused picture means play. Each claims
    /// the restore with `ForegroundInput::PlayKey`: the exact saved offset, the clock Playing.
    ///
    /// The Load must also START unpaused. The park keeps `TX.paused` (a reload reset), and the
    /// Load arms its initial clock hold from it: under the stale pause the hold was the
    /// viewer's, which fenced the Load's first Play behind a prime and left the actual Play to a
    /// later `PlayClock`. `clock_user_held_for_test` is that fence.
    #[test]
    fn a_press_that_asks_to_play_wakes_a_paused_decoder_park_playing() {
        let _serial = plx_base::testlock::serial();
        for press in [
            WakeWith::Key { sym: 0, wcode: plx_ui::consts::WCODE_PLAY },
            WakeWith::Key { sym: 0, wcode: plx_ui::consts::WCODE_PLAYPAUSE },
            WakeWith::Click,
        ] {
            let Some((_paused, mut rig, mut fr)) = wake_a_decoder_park(true, press) else {
                eprintln!(
                    "SKIPPED a_press_that_asks_to_play_wakes_a_paused_decoder_park_playing: \
                     lifecycle fixture worker thread could not be spawned"
                );
                return;
            };
            assert!(
                matches!(
                    rig.app.player.lifecycle.state(),
                    ForegroundState::LoadPending { clock: ForegroundClock::Playing, .. }
                ),
                "{press:?} must restore the paused snapshot Playing, got {:?}",
                rig.app.player.lifecycle.state()
            );
            assert!(!paused(), "{press:?}: the viewer's resume is published before the Load");
            assert!(
                !plx_media::player::clock_user_held_for_test(),
                "{press:?}: the Load's first Play must not be fenced behind the viewer's pause"
            );
            assert!(
                !plx_media::player::seek_preroll_active(),
                "{press:?}: a Playing restore must not open the paused-seek preroll"
            );

            land_the_wake_load(&mut rig, &mut fr);
            assert_eq!(rig.app.player.lifecycle.state(), ForegroundState::Idle, "{press:?}");
            assert!(!paused(), "{press:?}: the started restore plays");
            assert!(!plx_media::player::seek_preroll_active(), "{press:?}: and shows no preroll");
            assert!(
                !claim_press_after_decoder_release(&mut rig.app, WakePress::Wake),
                "{press:?}: the next press is ordinary"
            );
        }
    }

    /// **OK only wakes a paused decoder-release park; it comes back paused.** OK is the player's
    /// toggle too, but it is also the key a viewer presses to get rid of the screensaver, and
    /// whether the press that dismisses it reaches the app is unmeasured, so it keeps the saved
    /// clock rather than starting the video. On a set where the OK key arrives as a click
    /// (`input::click_is_ok`), the click is re-sent as this same key.
    #[test]
    fn an_ok_press_wakes_a_paused_decoder_park_paused() {
        let _serial = plx_base::testlock::serial();
        let ok = WakeWith::Key { sym: plx_ui::consts::SDLK_RETURN, wcode: 0 };
        let Some((_paused, rig, _fr)) = wake_a_decoder_park(true, ok) else {
            eprintln!(
                "SKIPPED an_ok_press_wakes_a_paused_decoder_park_paused: \
                 lifecycle fixture worker thread could not be spawned"
            );
            return;
        };
        assert!(
            matches!(
                rig.app.player.lifecycle.state(),
                ForegroundState::LoadPending { clock: ForegroundClock::Paused, .. }
            ),
            "OK keeps the saved pause, got {:?}",
            rig.app.player.lifecycle.state()
        );
        assert!(paused(), "the viewer's pause stands");
        assert!(plx_media::player::seek_preroll_active(), "…and the resume frame shows through the preroll");
    }

    /// **A toggle that wakes a PLAYING decoder-release park is spent as a wake.** PLAYPAUSE (or
    /// a click) on a playing picture means pause, but the foreground machine restores a snapshot
    /// in its saved clock or Playing, never Paused over a Playing save: the restore plays as it
    /// was, and the viewer's next press pauses it. Pinned so the table in
    /// `claim_press_after_decoder_release`'s doc stays what the code does.
    #[test]
    fn a_toggle_on_a_playing_decoder_park_restores_it_playing() {
        let _serial = plx_base::testlock::serial();
        for press in [WakeWith::Key { sym: 0, wcode: plx_ui::consts::WCODE_PLAYPAUSE }, WakeWith::Click] {
            let Some((_paused, mut rig, mut fr)) = wake_a_decoder_park(false, press) else {
                eprintln!(
                    "SKIPPED a_toggle_on_a_playing_decoder_park_restores_it_playing: \
                     lifecycle fixture worker thread could not be spawned"
                );
                return;
            };
            assert!(
                matches!(
                    rig.app.player.lifecycle.state(),
                    ForegroundState::LoadPending { clock: ForegroundClock::Playing, .. }
                ),
                "{press:?} restores a playing save Playing, got {:?}",
                rig.app.player.lifecycle.state()
            );
            land_the_wake_load(&mut rig, &mut fr);
            assert_eq!(rig.app.player.lifecycle.state(), ForegroundState::Idle, "{press:?}");
            assert!(!paused(), "{press:?}: the restore plays");
        }
    }

    /// **A click on a drawn control is only a wake.** The paused park keeps its transport drawn
    /// (`hud_visible` holds while paused), and the player's pointer arm turns only a click on NO
    /// control into the play/pause toggle: the scrub bar seeks, a disc arms the row, a tab opens
    /// Info or Chapters. A click on one of those must not bring the video back Playing; it keeps
    /// the saved pause, like OK.
    #[test]
    fn a_click_on_a_control_wakes_a_paused_decoder_park_paused() {
        let _serial = plx_base::testlock::serial();
        let Some((_paused, rig, _fr)) = wake_a_decoder_park(true, WakeWith::ClickOnControl) else {
            eprintln!(
                "SKIPPED a_click_on_a_control_wakes_a_paused_decoder_park_paused: \
                 lifecycle fixture worker thread could not be spawned"
            );
            return;
        };
        assert!(
            matches!(
                rig.app.player.lifecycle.state(),
                ForegroundState::LoadPending { clock: ForegroundClock::Paused, .. }
            ),
            "a click on a control keeps the saved pause, got {:?}",
            rig.app.player.lifecycle.state()
        );
        assert!(paused(), "the viewer's pause stands");
        assert!(plx_media::player::seek_preroll_active(), "…and the resume frame shows through the preroll");
        assert!(!rig.app.ptr.ok_from_click, "the click was not re-sent as OK, so this is the wake path");
    }

    /// A foreground actuator which prepares the resume on the spot and refuses the Load, touching
    /// no player: it leaves the machine `Prepared` (the Load's refusal retains the prepared route)
    /// with nothing else armed, which is the state a test then claims with the real actuator.
    struct RefusedLoad;

    impl ForegroundActuator for RefusedLoad {
        type Attempt = plx_media::route::RouteStartAttempt;

        fn prepare_resume(
            &mut self,
            _ps: &mut plx_media::route::PlaybackSession,
            _resume_ns: i64,
        ) -> plx_media::player::ResumeStart {
            plx_media::player::ResumeStart::Settled(plx_media::player::ResumeOutcome::Prepared)
        }

        fn land_prepare(
            &mut self,
            _ps: &mut plx_media::route::PlaybackSession,
        ) -> FlightLanding<plx_media::player::ResumeOutcome> {
            unreachable!("RefusedLoad prepares on the spot")
        }

        fn land_recovery(
            &mut self,
            _ps: &mut plx_media::route::PlaybackSession,
        ) -> FlightLanding<ForegroundLoadStart<Self::Attempt>> {
            unreachable!("RefusedLoad starts no rollback")
        }

        fn before_load(&mut self, _resume_ns: i64, _clock: ForegroundClock) {}

        fn start_load(
            &mut self,
            _ps: &mut plx_media::route::PlaybackSession,
        ) -> ForegroundLoadStart<Self::Attempt> {
            ForegroundLoadStart::Failed
        }

        fn load_status(&mut self, _attempt: Self::Attempt) -> ForegroundLoadStatus<Self::Attempt> {
            unreachable!("RefusedLoad launches no attempt")
        }

        fn after_load(
            &mut self,
            _ps: &mut plx_media::route::PlaybackSession,
            _attempt: Option<Self::Attempt>,
            _clock: ForegroundClock,
            _started: bool,
        ) {
        }

        fn play_clock(&mut self) -> bool {
            unreachable!("RefusedLoad starts no clock")
        }
    }

    /// **A Playing Load over a LIVE Engine must not publish the viewer's resume.** `before_load`
    /// commits the resume so a Load that BUILDS the Engine does not arm its clock hold under the
    /// stale pause. `start_load` over an Engine the pump already launched (a replacement after a
    /// refused restore) builds nothing: the Engine keeps the viewer's pause and `PlayClock` has to
    /// resume it for real, which it only does while `TX.paused` still reads Paused.
    #[test]
    fn a_playing_load_over_a_live_engine_does_not_publish_the_viewers_resume() {
        let _serial = plx_base::testlock::serial();
        let _paused = TransportPaused::keep();
        let Some(mut rig) = Rig::new() else {
            eprintln!(
                "SKIPPED a_playing_load_over_a_live_engine_does_not_publish_the_viewers_resume: \
                 lifecycle fixture worker thread could not be spawned"
            );
            return;
        };
        rig.request();
        rig.resolve();
        rig.accept_start();
        assert!(rig.app.adapters.player.is_live(), "fixture must create the actual Engine");
        // The viewer pauses the live Engine through the real transport, so it owns a user hold
        // that only a real resume releases.
        assert!(
            plx_media::player::transport::set_transport_paused(&mut rig.app.adapters.player, true),
            "the fixture's pause is accepted"
        );
        assert!(paused(), "the Engine is under the viewer's pause");
        // Prepared after a refused foreground Load, with the pump's replacement Engine live. (The
        // `PlayClock` that follows a CurrentAttempt landing is not driven here: the host rig arms
        // the Engine's initial hold from the pause at its START, so a pause pressed afterwards
        // leaves `resume` no hold to release.) The machine is brought there through its events: a
        // park, then a DID foreground whose Load is refused.
        rig.app.player.lifecycle.step(
            &ForegroundEvent::Park { saved_ns: 0, viewer_paused: true },
            &mut Vec::new(),
        );
        drive_foreground(
            &mut rig.app.player.lifecycle,
            &mut rig.app.player.session,
            ForegroundInput::DidForeground,
            &mut RefusedLoad,
        );
        assert!(
            matches!(
                rig.app.player.lifecycle.state(),
                ForegroundState::Prepared { resume_ns: 0, clock: ForegroundClock::Paused, .. }
            ),
            "the refused Load leaves the restore Prepared: {:?}",
            rig.app.player.lifecycle.state()
        );
        drive_foreground(
            &mut rig.app.player.lifecycle,
            &mut rig.app.player.session,
            ForegroundInput::PlayKey,
            &mut PlayerForegroundActuator {
                pa: &mut rig.app.adapters.player,
                repause_at: &mut rig.app.repause_at,
            },
        );
        assert!(
            paused(),
            "a Load that builds no Engine must not publish the viewer's resume over the live Engine's pause"
        );
    }

    /// **The transport guard restores AFTER the rig stops.** The rig's stop resets `TX.paused`,
    /// so a guard that drops first has its restore overwritten.
    #[test]
    fn the_transport_guard_restores_after_the_rig_stops() {
        let _serial = plx_base::testlock::serial();
        let _outer = TransportPaused::set(true);
        {
            let Some((_guard, _rig, _fr)) = wake_a_decoder_park(
                true,
                WakeWith::Key { sym: plx_ui::consts::SDLK_DOWN, wcode: 0 },
            ) else {
                eprintln!(
                    "SKIPPED the_transport_guard_restores_after_the_rig_stops: \
                     lifecycle fixture worker thread could not be spawned"
                );
                return;
            };
        }
        assert!(paused(), "the guard's restore must outlive the rig's reset");
    }

    /// **A surface open over the parked player gets its own press, not the restore.**
    /// `park_on_decoder_release` parks without asking whether a `ModalStack` surface owns input
    /// (the dead pipeline has to be dropped either way), so the claim itself is the one check that
    /// matters for input: Settings opening while the decoder is parked must still see the next key,
    /// and the restore must stay pending rather than being spent on a press the surface never got.
    #[test]
    fn a_surface_open_over_a_parked_decoder_release_keeps_its_own_press() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::new() else {
            eprintln!(
                "SKIPPED a_surface_open_over_a_parked_decoder_release_keeps_its_own_press: \
                 lifecycle fixture worker thread could not be spawned"
            );
            return;
        };
        rig.request();
        rig.resolve();
        rig.accept_start();
        frame(&mut rig.app, 0); // settle the pending Player page mount before opening a surface
        let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());

        plx_media::player::note_decoder_revoked_for_test();
        unsafe { playback_tick(&mut rig.app, &mut fr); }
        assert!(rig.app.player.lifecycle.awaiting_load(), "the session is parked for a restore");

        super::super::bridge::open_settings(&mut rig.app.pages);
        frame(&mut rig.app, fr.now);
        assert!(rig.app.pages.surface_up(), "Settings must mount as a ModalStack surface");

        rig.app.ev = encode_key(plx_ui::consts::SDLK_DOWN, 0, true);
        event(&mut rig.app, &mut fr, SDL_KEYDOWN);
        assert!(!rig.app.inputs.is_empty(), "the open surface must get this press");
        assert!(rig.app.player.lifecycle.awaiting_load(), "…and the restore must stay pending");
    }

    #[test]
    fn did_background_prevents_due_up_next_from_launching_while_suspended() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::new() else {
            eprintln!(
                "SKIPPED did_background_prevents_due_up_next_from_launching_while_suspended: \
                 lifecycle fixture worker thread could not be spawned"
            );
            return;
        };
        let mut fr = rig.due_up_next();
        let entry = rig.app.pages.nav.top_page().unwrap().id;
        let instance = rig.app.pages.nav.instance_of(entry);
        event(&mut rig.app, &mut fr, 0x104);
        assert!(rig.app.player.lifecycle.awaiting_load());
        let suspended = rig.app.player.lifecycle.state();
        let url = plx_media::route::url(&rig.app.player.session);
        assert!(super::super::bridge::player(&rig.app.pages)
            .unwrap()
            .up_next
            .expired(fr.now));
        unsafe {
            playback_tick(&mut rig.app, &mut fr);
        }
        assert!(
            !plx_media::route::play_pending(),
            "due Up Next must not request its successor in background"
        );
        assert!(!rig.app.adapters.player.is_live());
        assert_eq!(plx_media::route::cur_rk(&rig.app.player.session), "1");
        assert_eq!(plx_media::route::url(&rig.app.player.session), url);
        assert_eq!(
            plx_media::route::up_next(&rig.app.player.session).unwrap().rk,
            "2"
        );
        assert_eq!(rig.app.player.lifecycle.state(), suspended);
        assert_eq!(rig.app.pages.nav.instance_of(entry), instance);
        event(&mut rig.app, &mut fr, 0x105);
        unsafe {
            playback_tick(&mut rig.app, &mut fr);
        }
        assert!(
            !plx_media::route::play_pending(),
            "WILL foreground does not grant playback permission"
        );
    }

    #[test]
    fn foreground_due_up_next_still_requests_its_successor() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::new() else {
            eprintln!(
                "SKIPPED foreground_due_up_next_still_requests_its_successor: \
                 lifecycle fixture worker thread could not be spawned"
            );
            return;
        };
        let mut fr = rig.due_up_next();
        unsafe {
            playback_tick(&mut rig.app, &mut fr);
        }
        assert!(
            plx_media::route::play_pending(),
            "foreground auto-advance must remain enabled"
        );
        assert!(plx_media::route::up_next(&rig.app.player.session).is_none());
    }

    /// The first half of a foreground restore the engine refuses: playback, an OS background, DID
    /// foreground, and a Load the route answers with `StartFailed`, leaving the machine `Prepared`
    /// with no live engine. Returns the frame the rest of the test keeps driving.
    fn refuse_restore(rig: &mut Rig) -> Frame {
        rig.request();
        rig.resolve();
        rig.accept_start();
        frame(&mut rig.app, 16);
        assert!(super::super::bridge::player(&rig.app.pages).is_some());

        let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
        fr.now = 32;
        event(&mut rig.app, &mut fr, 0x104);
        assert!(rig.app.player.lifecycle.awaiting_load());
        // The host test has no GL context for `tv::window::grab`. Drive the remaining production
        // DID-foreground seams directly after granting the same WindowActivity permission.
        rig.app.window_activity.event(0x106);
        super::super::bridge::foreground(&mut rig.app.pages);
        assert_eq!(
            drive_foreground(
                &mut rig.app.player.lifecycle,
                &mut rig.app.player.session,
                ForegroundInput::DidForeground,
                &mut PlayerForegroundActuator {
                    pa: &mut rig.app.adapters.player,
                    repause_at: &mut rig.app.repause_at,
                },
            ),
            ForegroundActivation::Launched
        );
        let failed_attempt = rig
            .app
            .player
            .lifecycle
            .pending_load_attempt()
            .expect("DID foreground must launch the tracked Load");
        assert!(plx_media::route::settle_route_start(
            &mut rig.app.player.session,
            failed_attempt,
            plx_media::route::RouteStartResult::StartFailed,
        ));
        unsafe { playback_tick(&mut rig.app, &mut fr); }
        assert!(matches!(
            rig.app.player.lifecycle.state(),
            ForegroundState::Prepared { .. }
        ));
        assert!(!rig.app.adapters.player.is_live());
        fr
    }

    /// An exit over a parked restore whose Load was refused (`Prepared`) must retire it; main left
    /// it standing, so `playback_may_run` stayed false until the next play released it.
    #[test]
    fn exit_over_a_refused_restore_retires_it() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::new() else {
            eprintln!(
                "SKIPPED exit_over_a_refused_restore_retires_it: \
                 lifecycle fixture worker thread could not be spawned"
            );
            return;
        };
        let fr = refuse_restore(&mut rig);

        player_requests(
            &mut rig.app.player,
            &mut rig.app.adapters.player,
            &mut rig.app.repause_at,
            vec![plx_screens::registry::PlayerReq::Exit],
            fr.now,
            &mut rig.app.refresh_hubs,
            &mut rig.app.pages,
            &mut rig.app.bridge,
            &mut rig.app.ok_armed,
            &mut rig.app.input.press,
        );
        frame(&mut rig.app, 48);
        assert!(super::super::bridge::player(&rig.app.pages).is_none());
        assert_eq!(rig.app.player.lifecycle.state(), ForegroundState::Idle);
        assert!(playback_may_run(&rig.app));
    }

    /// The failure screen's Retry over a refused restore: its fresh resolve supersedes the parked
    /// `Prepared` at the `play_pending` boundary and the retry runs. Pins what replacing
    /// `replace_with_new_playback` by `Superseded(Playback)` must keep.
    #[test]
    fn a_retry_from_a_refused_restore_still_runs() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::new() else {
            eprintln!(
                "SKIPPED a_retry_from_a_refused_restore_still_runs: \
                 lifecycle fixture worker thread could not be spawned"
            );
            return;
        };
        let mut fr = refuse_restore(&mut rig);

        player_requests(
            &mut rig.app.player,
            &mut rig.app.adapters.player,
            &mut rig.app.repause_at,
            vec![plx_screens::registry::PlayerReq::RetryPlayback],
            fr.now,
            &mut rig.app.refresh_hubs,
            &mut rig.app.pages,
            &mut rig.app.bridge,
            &mut rig.app.ok_armed,
            &mut rig.app.input.press,
        );
        assert!(plx_media::route::play_pending());
        rig.accept_start();
        poll_until("the retry did not land past the refused restore", || {
            unsafe {
                playback_tick(&mut rig.app, &mut fr);
                update(&mut rig.app, &mut fr);
            }
            rig.app.adapters.player.is_live()
        });
        assert!(!plx_media::route::play_pending());
        assert_eq!(rig.app.player.lifecycle.state(), ForegroundState::Idle);
        assert!(playback_may_run(&rig.app));
    }

    #[test]
    fn replacement_play_retires_failed_foreground_owner_and_lands() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::new() else {
            eprintln!(
                "SKIPPED replacement_play_retires_failed_foreground_owner_and_lands: \
                 lifecycle fixture worker thread could not be spawned"
            );
            return;
        };
        rig.request();
        rig.resolve();
        rig.accept_start();
        frame(&mut rig.app, 16);
        assert!(super::super::bridge::player(&rig.app.pages).is_some());

        let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
        fr.now = 32;
        event(&mut rig.app, &mut fr, 0x104);
        assert!(rig.app.player.lifecycle.awaiting_load());
        // The host test has no GL context for `tv::window::grab`. Drive the remaining production
        // DID-foreground seams directly after granting the same WindowActivity permission.
        rig.app.window_activity.event(0x106);
        super::super::bridge::foreground(&mut rig.app.pages);
        assert_eq!(
            drive_foreground(
                &mut rig.app.player.lifecycle,
                &mut rig.app.player.session,
                ForegroundInput::DidForeground,
                &mut PlayerForegroundActuator {
                    pa: &mut rig.app.adapters.player,
                    repause_at: &mut rig.app.repause_at,
                },
            ),
            ForegroundActivation::Launched
        );
        let failed_attempt = rig
            .app
            .player
            .lifecycle
            .pending_load_attempt()
            .expect("DID foreground must launch the tracked Load");
        assert!(plx_media::route::settle_route_start(
            &mut rig.app.player.session,
            failed_attempt,
            plx_media::route::RouteStartResult::StartFailed,
        ));
        unsafe { playback_tick(&mut rig.app, &mut fr); }
        assert!(matches!(
            rig.app.player.lifecycle.state(),
            ForegroundState::Prepared { .. }
        ));
        assert!(!rig.app.adapters.player.is_live());

        player_requests(
            &mut rig.app.player,
            &mut rig.app.adapters.player,
            &mut rig.app.repause_at,
            vec![plx_screens::registry::PlayerReq::Exit],
            fr.now,
            &mut rig.app.refresh_hubs,
            &mut rig.app.pages,
            &mut rig.app.bridge,
            &mut rig.app.ok_armed,
            &mut rig.app.input.press,
        );
        frame(&mut rig.app, 48);
        assert!(super::super::bridge::player(&rig.app.pages).is_none());
        assert_eq!(rig.app.player.lifecycle.state(), ForegroundState::Idle);

        assert!(plx_media::route::request_play(
            &mut rig.app.player.session,
            rig.app.bridge.metadata_mut(),
            rig.sid,
            "replacement",
            "/library/parts/2/file.mkv",
            "h264",
            "aac",
            "Replacement",
            "",
            None,
        ));
        rig.accept_start();
        assert!(plx_media::route::play_pending());
        assert_eq!(rig.app.player.lifecycle.state(), ForegroundState::Idle);
        poll_until("replacement landing remained blocked by the old lifecycle", || {
            unsafe {
                playback_tick(&mut rig.app, &mut fr);
                update(&mut rig.app, &mut fr);
            }
            rig.app.adapters.player.is_live()
        });
        assert!(!plx_media::route::play_pending());
        assert_eq!(rig.app.player.lifecycle.state(), ForegroundState::Idle);
        assert!(plx_media::route::has_url(&rig.app.player.session));
    }

    /// One loop iteration's navigation-bearing phases, in `run`'s order: the playback tick (EOS),
    /// the container's frame, the post-player restore, the drained requests and `update` (where a
    /// resolved plan lands).
    use plx_ui::screen::ScreenArg;

    fn step(app: &mut App, t: &mut u32, inputs: Vec<plx_machine::machine::InputEvent<u32>>) {
        // Nothing may sit in a store's job outbox between frames, and no pump may have had to start
        // one late (`late_store_jobs`): a sender that went around `Bridge::metadata_run` is red here
        // even though `update()`'s detail pump below would have started its job.
        app.bridge.assert_no_store_jobs_left_for_test();
        *t += 16;
        app.inputs.extend(inputs);
        let mut fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
        fr.now = *t;
        fr.dt = 0.016;
        let was_player = super::super::bridge::player(&app.pages).is_some();
        unsafe { playback_tick(app, &mut fr); }
        super::super::bridge::frame(&mut app.pages, &mut app.bridge,
            plx_machine::machine::Tick { ms: *t, dt_us: 16_000 }, std::mem::take(&mut app.inputs));
        if was_player && super::super::bridge::player(&app.pages).is_none() {
            restore_played_entry(app);
        }
        content_requests(app, &fr);
        loop_requests(app);
        unsafe { update(app, &mut fr); }
        app.bridge.assert_no_store_jobs_left_for_test();
    }

    /// One frame, then the jobs the stores sent in it run on this thread (the test runner parks
    /// them, as it parks a detail fetch): a read-back's request reaches the server in the frame
    /// that sent it, as a worker's would, and its answer is the next frame's to take.
    fn step_and_run_jobs(rig: &mut Rig, t: &mut u32) {
        step(&mut rig.app, t, vec![]);
        rig.app.bridge.run_held_store_jobs_for_test();
    }

    /// The page stack on the transition `boot.rs` builds it with. `app()`'s dispatcher commits
    /// immediately, and an immediate commit never has a dip-out to prepare a destination in —
    /// which is the whole window the tests below are about. Returns the frame clock's start.
    fn product_transition(app: &mut App) -> u32 {
        app.pages.nav.tabs.stack.transition =
            Box::new(plx_ui::containers::transition::PageDip::new());
        0
    }

    fn settle(app: &mut App, t: &mut u32) {
        for _ in 0..24 {
            step(app, t, vec![]);
        }
    }

    /// `settle` for the frames after BACK from the player to Home: the page dip may wait dark at its
    /// floor for the hubs refetch for up to `CW_RETURN_HOLD_MS` (300 ms of frame time, 19 frames),
    /// and a stop that is still in flight keeps the refetch from being issued, so the whole bound
    /// is spent before Home is on top.
    fn settle_after_back(app: &mut App, t: &mut u32) {
        for _ in 0..24 + (super::super::playback::CW_RETURN_HOLD_MS / 16 + 2) {
            step(app, t, vec![]);
        }
    }

    /// A REAL card hold on Home's grid, driven through the whole chain the television runs: the OK
    /// key goes down and stays down, the dispatcher's `InputMachine::tick` delivers `PressHold`
    /// at `LONG_MS`, Home answers it with `HomeReq::ItemMenu`, `content_requests` drains that and
    /// `bridge::open_item_menu` presents the menu (or the app declines). Steps frame by frame
    /// until the dispatcher abandons the press (the frame the hold was delivered and answered) and
    /// returns the press as it stands on that frame, `(active, scale)`, with the app.
    fn hold_a_home_card(declined: bool) -> (bool, f32, App, u32) {
        let mut app = app();
        let mut t = 0u32;
        super::super::bridge::show_page(&mut app.pages, AppArg::Home);
        app.bridge.seed_hub_grid_for_test(3, 4);
        if declined {
            // The first card is a collection: the menu has nothing to offer it.
            app.bridge.retag_hub_item_as_collection_for_test("1");
        }
        settle(&mut app, &mut t);
        let entry = app.pages.nav.top_page().unwrap().id;
        let instance = app.pages.nav.instance_of(entry).unwrap();
        app.pages.emit(plx_machine::machine::MachineId::Nav, plx_machine::machine::Fx::Deliver(
            plx_machine::machine::MachineId::Instance(instance),
            plx_machine::machine::Delivery::Screen(plx_ui::screen::ScreenEvent::App(
                plx_screens::registry::AppMsg::Home(plx_screens::registry::HomeCmd::FocusGrid { row: 0, col: 0 })))));
        for _ in 0..40 { step(&mut app, &mut t, vec![]); }
        assert!(!super::super::bridge::item_menu_up(&app.pages), "premise: no menu yet");

        // OK goes down and STAYS down: a hold, never a release.
        let down = plx_ui::fixture::key(plx_machine::machine::Key::Ok, plx_machine::machine::Tick { ms: t + 16, dt_us: 16_000 });
        step(&mut app, &mut t, vec![down]);
        for _ in 0..6 { step(&mut app, &mut t, vec![]); }
        assert!(app.pages.input.press.is_live() && app.pages.input.press.scale() < 0.99,
            "premise: the hold is armed on the dispatcher's press and dipped");
        for _ in 0..80 {
            step(&mut app, &mut t, vec![]);
            if !app.pages.input.press.is_live() {
                return (app.pages.input.press.is_active(), app.pages.input.press.scale(), app, t);
            }
        }
        panic!("the hold never reached the app");
    }

    /// **A card hold the app takes (the item menu opens) springs the card from its dip up to its
    /// pop, as the menu opens.** The dip is a press-in on the focus pop (`press::DIP` is the card's
    /// unpopped size), and a card that goes from there to its popped size in ONE frame has lost its
    /// pop animation (0.8.0-rc.3: "still no animation" on a long-press). The press is the one owner
    /// of that motion: the dispatcher abandons it when the hold is answered (the release spring,
    /// the underdamped one a normal OK press gets), and nothing ends it early when the menu opens.
    /// The scale therefore leaves the dip, crosses the pop within a bounded number of frames and
    /// rests, never moving more than a fraction of the dip per frame.
    #[test]
    fn a_real_card_hold_that_opens_the_item_menu_springs_the_card_up_to_its_pop() {
        let _serial = plx_base::testlock::serial();
        let (active, scale, mut app, mut t) = hold_a_home_card(false);
        assert!(active, "the press is still springing the frame the menu is asked for");
        assert!(scale < 0.93, "…from the dip (not already at rest): {scale}");
        let mut prev = scale;
        let mut worst = 0.0f32;
        let mut crossed_at = None;
        let mut rose = false;
        for frame in 1..=60 {
            step(&mut app, &mut t, vec![]);
            let now = app.pages.input.press.scale();
            worst = worst.max((now - prev).abs());
            rose |= now > prev;
            if crossed_at.is_none() && now >= 1.0 { crossed_at = Some(frame); }
            prev = now;
        }
        assert!(super::super::bridge::item_menu_up(&app.pages), "the hold opened the item menu");
        assert!(rose, "the card rises from the dip");
        assert!(crossed_at.is_some_and(|f| f <= 20), "it reaches its pop within ~0.3 s, got {crossed_at:?}");
        assert!(worst < 0.04, "no frame jumps the card (biggest step {worst}); the dip is 0.082 deep");
        assert!(!app.pages.input.press.is_active(), "…and the spring comes to rest under the menu");
        assert_eq!(app.pages.input.press.scale(), 1.0);
    }

    /// What `hold_bench_tick` put on the input queue in one frame: `(ok_down, ok_up, back, direction)`
    /// counts, plus whether an item menu was up when it ran (so an OK edge can be held against it).
    #[derive(Default, Debug)]
    struct BenchInputs { ok_down: u32, ok_up: u32, back: u32, dir: u32, ok_with_menu_up: u32 }

    /// One frame of the press bench exactly as the loop runs it: the tick, then the frame.
    fn bench_step(app: &mut App, t: &mut u32, seen: &mut BenchInputs) {
        use plx_machine::machine::{Edge, InputKind, Key};
        let before = app.inputs.len();
        let menu_up = super::super::bridge::item_menu_up(&app.pages);
        crate::dev::scenarios::hold_bench_tick(app, *t + 16);
        for ev in &app.inputs[before..] {
            if let InputKind::Key { key, edge, .. } = ev.kind {
                match (key, edge) {
                    (Key::Ok, Edge::Down) => seen.ok_down += 1,
                    (Key::Ok, Edge::Up) => seen.ok_up += 1,
                    (Key::Back, Edge::Down) => seen.back += 1,
                    (Key::Left | Key::Right, Edge::Down) => seen.dir += 1,
                    _ => {}
                }
                if key == Key::Ok && menu_up { seen.ok_with_menu_up += 1; }
            }
        }
        step(app, t, vec![]);
    }

    /// Home with the first grid card focused and a press bench of `n` cycles armed and past its
    /// settle gate (the gate waits on presented frames, which this host loop does not produce).
    fn home_press_bench(n: u32, kind: crate::dev::scenarios::bench::PressKind) -> (App, u32) {
        let mut app = app();
        let mut t = 0u32;
        super::super::bridge::show_page(&mut app.pages, AppArg::Home);
        app.bridge.seed_hub_grid_for_test(3, 4);
        settle(&mut app, &mut t);
        let entry = app.pages.nav.top_page().unwrap().id;
        let instance = app.pages.nav.instance_of(entry).unwrap();
        app.pages.emit(plx_machine::machine::MachineId::Nav, plx_machine::machine::Fx::Deliver(
            plx_machine::machine::MachineId::Instance(instance),
            plx_machine::machine::Delivery::Screen(plx_ui::screen::ScreenEvent::App(
                plx_screens::registry::AppMsg::Home(plx_screens::registry::HomeCmd::FocusGrid { row: 0, col: 0 })))));
        for _ in 0..40 { step(&mut app, &mut t, vec![]); }
        assert!(app.bridge.home_grid_focused(&app.pages), "premise: a grid card is focused");
        use crate::dev::scenarios::bench;
        let period = match kind { bench::PressKind::Hold => bench::DEFAULT_HOLD_PERIOD_MS, bench::PressKind::Tap => bench::DEFAULT_TAP_PERIOD_MS };
        let mut b = bench::PressBench::new(n, period, kind);
        b.clock.phase = bench::BenchPhase::Waiting;
        b.clock.last = t;
        app.scenarios.hold_bench = Some(b);
        (app, t)
    }

    /// **The hold trigger performs a real hold.** Every cycle: ONE OK-down and no OK-up (the press is
    /// never released), the hold fill runs past `LONG_MS` and the dispatcher answers it with the item
    /// menu, then ONE Back dismisses it. No OK edge is ever queued while a menu is up (a hold menu
    /// opens on its Play row), the page is still Home at the end, and the bench counts its cycles
    /// through to `done`.
    #[test]
    fn the_hold_bench_opens_the_item_menu_from_a_real_hold_and_dismisses_it_with_back_only() {
        let _serial = plx_base::testlock::serial();
        let (mut app, mut t) = home_press_bench(3, crate::dev::scenarios::bench::PressKind::Hold);
        let (mut seen, mut menu_frames, mut menu_cycles, mut was_up) = (BenchInputs::default(), 0u32, 0u32, false);
        for _ in 0..1500 {
            bench_step(&mut app, &mut t, &mut seen);
            let up = super::super::bridge::item_menu_up(&app.pages);
            menu_frames += u32::from(up);
            menu_cycles += u32::from(up && !was_up);
            was_up = up;
            if app.scenarios.hold_bench.is_none() { break; }
        }
        assert!(app.scenarios.hold_bench.is_none(), "the bench ran its cycles to `done`: {seen:?}");
        assert_eq!((seen.ok_down, seen.back), (3, 3), "one press and one dismissal per cycle: {seen:?}");
        assert_eq!(seen.ok_up, 0, "a hold is never released: {seen:?}");
        assert_eq!(seen.ok_with_menu_up, 0, "no OK is queued onto an open menu: {seen:?}");
        assert_eq!(menu_cycles, 3, "every hold opened the menu (and every Back closed it)");
        assert!(menu_frames > 30, "the menu stayed up across frames, not for one: {menu_frames}");
        for _ in 0..120 { step(&mut app, &mut t, vec![]); }
        assert!(!super::super::bridge::item_menu_up(&app.pages), "Back closed the last menu");
        assert!(matches!(app.route(), AppArg::Home), "nothing navigated away");
    }

    /// The tap-and-cancel variant: OK down, then a direction key BEFORE the hold threshold, so the
    /// press is abandoned (the real cancel path) without activating a card or opening a menu.
    #[test]
    fn the_tap_bench_cancels_the_press_without_activating_or_opening_a_menu() {
        let _serial = plx_base::testlock::serial();
        let (mut app, mut t) = home_press_bench(4, crate::dev::scenarios::bench::PressKind::Tap);
        let (mut seen, mut menu_seen) = (BenchInputs::default(), false);
        for _ in 0..1500 {
            bench_step(&mut app, &mut t, &mut seen);
            menu_seen |= super::super::bridge::item_menu_up(&app.pages);
            assert!(matches!(app.route(), AppArg::Home), "a cancelled press never navigates: {seen:?}");
            if app.scenarios.hold_bench.is_none() { break; }
        }
        assert!(app.scenarios.hold_bench.is_none(), "the bench ran to `done`: {seen:?}");
        assert_eq!((seen.ok_down, seen.dir), (4, 4), "{seen:?}");
        assert_eq!((seen.ok_up, seen.back, seen.ok_with_menu_up), (0, 0, 0), "{seen:?}");
        assert!(!menu_seen, "a press cancelled under LONG_MS never opens the menu");
        for _ in 0..60 { step(&mut app, &mut t, vec![]); }
        assert!(!app.pages.input.press.is_active(), "the spring came to rest");
    }

    /// A hold the app DECLINES (nothing to offer a collection) is not taken: the dispatcher
    /// abandons the press and it springs back, then comes to rest.
    #[test]
    fn a_real_card_hold_the_app_declines_springs_back() {
        let _serial = plx_base::testlock::serial();
        let (active, scale, mut app, mut t) = hold_a_home_card(true);
        assert!(active && scale < 0.99, "the declined hold springs back: active={active} scale={scale}");
        for _ in 0..4 { step(&mut app, &mut t, vec![]); }
        assert!(!super::super::bridge::item_menu_up(&app.pages), "a collection has no item menu");
        for _ in 0..40 { step(&mut app, &mut t, vec![]); }
        assert!(!app.pages.input.press.is_active(), "…and the spring comes to rest");
    }

    /// **Press Play, and let the plan land INSIDE the player push's dip-out** — the order the
    /// television produces whenever the resolve is quicker than the 140 ms fade. `start_playback`
    /// pushes the player while the plan is still resolving; the next frame's dip-out PREPARES the
    /// destination (`NavStack::stage_pending_target`), which mounts the player and so consumes its
    /// origin seed; and when the plan then lands, the committed route still names the page under
    /// the dip, so `update`'s landing arm asks for the player a second time (`pump_play: engine
    /// started off-route`). Returns the entry the session was launched from.
    fn play_with_a_landing_inside_the_dip(rig: &mut Rig, t: &mut u32) -> plx_machine::machine::EntryId {
        let origin = rig.app.pages.nav.top_page().expect("a page is on top").id;
        rig.request();
        rig.accept_start();
        assert!(plx_media::route::play_pending(), "the plan is still resolving");
        step(&mut rig.app, t, vec![]);
        assert!(super::super::bridge::player(&rig.app.pages).is_none(), "the push has not committed");
        assert!(
            rig.app.pages.nav.tabs.stack.pending_target_mut().is_some_and(|e| e.inst.is_some()),
            "premise: the dip-out has already prepared the player it is pushing",
        );
        rig.release.send(()).unwrap();
        poll_until("the plan did not land through update", || {
            let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
            fr.now = *t;
            unsafe { update(&mut rig.app, &mut fr); }
            rig.app.adapters.player.is_live()
        });
        settle(&mut rig.app, t);
        assert!(super::super::bridge::player(&rig.app.pages).is_some(), "the player is on top");
        origin
    }

    /// BACK as the remote spells it (ESC — the dev remote's own spelling of BACK in the field
    /// report): the player classifies `sym`/`wcode`, not the fixture's `Key`.
    fn back_key(ms: u32) -> plx_machine::machine::InputEvent<u32> {
        let mut event = plx_ui::fixture::key(plx_machine::machine::Key::Back, plx_ui::fixture::tick(ms));
        if let plx_machine::machine::InputKind::Key { sym, .. } = &mut event.kind {
            *sym = plx_ui::consts::SDLK_ESCAPE;
        }
        event
    }

    fn detail_arg(sid: plx_plex::plex::ServerId) -> AppArg {
        AppArg::Content(plx_screens::registry::ContentArg::Detail { sid, rk: "1".into() })
    }

    /// Field report: Home → a detail page → Play → BACK landed on HOME. BACK must land on the
    /// detail page the film was started from — the same entry, not a fresh copy of it.
    #[test]
    fn back_from_the_player_returns_to_the_detail_page_it_was_started_from() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else {
            eprintln!("SKIPPED back_from_the_player_returns_to_the_detail_page_it_was_started_from: \
                lifecycle fixture worker thread could not be spawned");
            return;
        };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        super::super::bridge::open_detail(&mut rig.app.pages, &mut rig.app.bridge, rig.sid, "1", None, None);
        settle(&mut rig.app, &mut t);
        let origin = play_with_a_landing_inside_the_dip(&mut rig, &mut t);
        let back = back_key(t + 16);
        step(&mut rig.app, &mut t, vec![back]);
        settle(&mut rig.app, &mut t);
        assert!(
            rig.app.route().same_instance(&detail_arg(rig.sid)),
            "BACK from the player landed on {:?}, not the detail page",
            rig.app.route().id(),
        );
        assert_eq!(rig.app.pages.nav.top_page().map(|e| e.id), Some(origin));
    }

    /// **Issue #575: a stop is not reflected in the page it returns to.** The Detail page keeps
    /// the `viewOffset` it fetched when it opened, and Play reads its resume point from that copy,
    /// so a second Play after a stop starts from where the FIRST one did (or from 0:00 when the
    /// page opened on an item with no offset). The return must re-read the item once the stop
    /// report has landed - the server is the only one that knows what it committed.
    #[test]
    fn back_from_the_player_rereads_the_page_so_play_resumes_where_the_stop_left_off() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else {
            eprintln!("SKIPPED back_from_the_player_rereads_the_page_so_play_resumes_where_the_stop_left_off: \
                lifecycle fixture worker thread could not be spawned");
            return;
        };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        super::super::bridge::open_detail(&mut rig.app.pages, &mut rig.app.bridge, rig.sid, "1", None, None);
        settle(&mut rig.app, &mut t);
        play_with_a_landing_inside_the_dip(&mut rig, &mut t);
        let reads_before = plx_data::metadata::detail_generation_for_test(rig.app.bridge.metadata_mut().adapter_ref());
        let back = back_key(t + 16);
        step(&mut rig.app, &mut t, vec![back]);
        settle(&mut rig.app, &mut t);
        assert!(super::super::bridge::player(&rig.app.pages).is_none(), "premise: BACK left the player");
        // The re-read waits for the stop report AND the read-back it owes (`Stores::stops_landed`),
        // both of which a worker thread answers, and these frames take no wall time: wait for the
        // generation itself rather than counting frames.
        // The rig's engine decodes no frame, so its own teardown has no final position to report:
        // post the stop it would have (the production path past that point, mail and read-back).
        plx_media::route::scrobble_stop(&mut rig.app.player.session, Some(("1".into(), 30_000, 60_000)), None);
        plx_media::route::drain_scrobble();
        poll_until("returning to the page re-read nothing: its resume point is still the one it opened with", || {
            step_and_run_jobs(&mut rig, &mut t);
            plx_data::metadata::detail_generation_for_test(rig.app.bridge.metadata_mut().adapter_ref()) > reads_before
        });
    }

    /// **Issue #680**: a show's Detail page, an episode played to its end, and the episode strip
    /// must show what the server now holds (watched), not what the page read when it opened.
    #[test]
    fn issue_680_an_episode_played_to_its_end_is_watched_on_the_detail_page_it_returns_to() {
        let _serial = plx_base::testlock::serial();
        let played = Arc::new(AtomicBool::new(false));
        let flag = played.clone();
        let Some(mut rig) = Rig::serving_with(move |request: &str| {
            let watched = flag.load(Ordering::SeqCst);
            if request.contains("/decision?") {
                return r#"{"MediaContainer":{"Metadata":[{"Media":[{"Part":[{"decision":"directplay"}]}]}]}}"#.into();
            }
            if request.contains("/library/metadata/200/children") {
                return format!(r#"{{"MediaContainer":{{"size":2,"totalSize":2,"Metadata":[
                  {{"ratingKey":"1","type":"episode","index":1,"parentIndex":1,"title":"First","duration":60000{}}},
                  {{"ratingKey":"2","type":"episode","index":2,"parentIndex":1,"title":"Next","duration":60000}}]}}}}"#,
                  if watched { r#","viewCount":1"# } else { "" });
            }
            if request.contains("/library/metadata/100/children") {
                return r#"{"MediaContainer":{"Metadata":[{"ratingKey":"200","type":"season","index":1,"title":"Season 1","leafCount":2}]}}"#.into();
            }
            if request.contains("/related") || request.contains("/extras") || request.contains("/:/timeline") {
                return r#"{"MediaContainer":{}}"#.into();
            }
            if request.contains("/library/metadata/100") {
                return r#"{"MediaContainer":{"Metadata":[{"ratingKey":"100","type":"show","title":"Show","childCount":1,"leafCount":2}]}}"#.into();
            }
            // the play queue: one episode, nothing after it
            if request.contains("playQueue") { flag.store(true, Ordering::SeqCst); }
            r#"{"MediaContainer":{"machineIdentifier":"lifecycle-fixture","size":1,
            "playQueueID":1,"playQueueSelectedItemID":11,"playQueueTotalCount":1,
            "Metadata":[
            {"ratingKey":"1","playQueueItemID":11,"type":"episode","title":"First",
             "grandparentRatingKey":"100","parentIndex":1,"index":1,"duration":60000,
             "Media":[{"videoCodec":"h264","audioCodec":"aac","width":1280,"height":720,
             "Part":[{"id":1,"key":"/library/parts/1/file.mkv","Stream":[
             {"id":1,"streamType":1,"codec":"h264"},
             {"id":2,"streamType":2,"codec":"aac","selected":true}]}]}]}]}}"#.into()
        }) else { return };
        // Nothing parks: the first request (the page's own read) answers at once.
        rig.release.send(()).unwrap();
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        super::super::bridge::open_detail(&mut rig.app.pages, &mut rig.app.bridge, rig.sid, "100", None, None);
        settle(&mut rig.app, &mut t);
        rig.app.bridge.run_held_store_jobs_for_test();
        rig.app.bridge.metadata_mut().pump_detail();
        settle(&mut rig.app, &mut t);
        let watched = |rig: &mut Rig| rig.app.bridge.metadata_view().current()
            .filter(|d| d.rk == "100")
            .and_then(|d| d.episodes.get(0).map(|e| (e.rk.clone(), e.watched)));
        assert_eq!(watched(&mut rig), Some(("1".into(), false)), "premise: the page opened on an unwatched episode");
        let origin = rig.app.pages.nav.top_page().unwrap().id;
        assert!(matches!(rig.app.pages.nav.top_page().unwrap().arg,
            AppArg::Content(plx_screens::registry::ContentArg::Detail { .. })));
        rig.request();
        rig.accept_start();
        poll_until("the plan did not land through update", || {
            let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
            fr.now = t;
            unsafe { update(&mut rig.app, &mut fr); }
            rig.app.adapters.player.is_live()
        });
        settle(&mut rig.app, &mut t);
        assert!(super::super::bridge::player(&rig.app.pages).is_some(), "the player is on top");
        assert!(played.load(Ordering::SeqCst), "premise: the server saw the play");
        plx_media::player::SHARED.ended.store(true, std::sync::atomic::Ordering::Relaxed);
        settle(&mut rig.app, &mut t);
        assert_eq!(rig.app.pages.nav.top_page().map(|e| e.id), Some(origin), "EOS returned to the page");
        // The rig's engine decodes no frame, so its own teardown has no final position to report:
        // post the stop it would have (the production path past that point, mail and read-back).
        plx_media::route::scrobble_stop(&mut rig.app.player.session, Some(("1".into(), 60_000, 60_000)), None);
        plx_media::route::drain_scrobble();
        // The page learns of the stop from the read-back of the episode (and of its show, which the
        // episode's answer names), each a worker round trip: wait for the re-read, not for frames.
        poll_until("the episode strip still shows the read the page opened with", || {
            step(&mut rig.app, &mut t, vec![]);
            rig.app.bridge.run_held_store_jobs_for_test();
            rig.app.bridge.metadata_mut().pump_detail();
            watched(&mut rig) == Some(("1".into(), true))
        });
        settle(&mut rig.app, &mut t);
        assert_eq!(watched(&mut rig), Some(("1".into(), true)));
    }

    /// A show (100) of one season (200) of two episodes, served as the server holds it: nothing is
    /// watched until a `/:/scrobble` arrives, then everything is (the show's and the season's
    /// `viewedLeafCount` included). `marked` is that flag; `reads` counts the metadata reads the
    /// server answered after it, so a test can see a re-read go out.
    /// `partial`: the server takes the write but only gets as far as the first episode, so what it
    /// then holds (one of two watched) differs from what the optimistic patch drew (both).
    fn marked_show_server(marked: Arc<AtomicBool>, partial: bool) -> impl Fn(&str) -> String + Send + 'static {
        move |request: &str| {
            let done = marked.load(Ordering::SeqCst);
            let (count, viewed) = if done { (r#","viewCount":1"#, if partial { 1 } else { 2 }) } else { ("", 0) };
            let second = if done && !partial { count } else { "" };
            if request.contains("/:/scrobble") {
                marked.store(true, Ordering::SeqCst);
                return "{}".into();
            }
            if request.contains("/library/metadata/200/children") {
                return format!(r#"{{"MediaContainer":{{"size":2,"totalSize":2,"Metadata":[
                  {{"ratingKey":"1","type":"episode","index":1,"parentIndex":1,"title":"First","duration":60000{count}}},
                  {{"ratingKey":"2","type":"episode","index":2,"parentIndex":1,"title":"Next","duration":60000{second}}}]}}}}"#);
            }
            if request.contains("/library/metadata/100/children") {
                return format!(r#"{{"MediaContainer":{{"Metadata":[{{"ratingKey":"200","type":"season","index":1,"title":"Season 1","leafCount":2,"viewedLeafCount":{viewed}}}]}}}}"#);
            }
            if request.contains("/related") || request.contains("/extras") || request.contains("/:/timeline") {
                return r#"{"MediaContainer":{}}"#.into();
            }
            if request.contains("/library/metadata/100") {
                return format!(r#"{{"MediaContainer":{{"Metadata":[{{"ratingKey":"100","type":"show","title":"Show","childCount":1,"leafCount":2,"viewedLeafCount":{viewed}}}]}}}}"#);
            }
            r#"{"MediaContainer":{}}"#.into()
        }
    }

    /// The show page open and landed (the fixture parks its first request until released).
    fn open_marked_show(rig: &mut Rig, t: &mut u32) {
        rig.release.send(()).unwrap();
        settle(&mut rig.app, t);
        super::super::bridge::open_detail(&mut rig.app.pages, &mut rig.app.bridge, rig.sid, "100", None, None);
        settle(&mut rig.app, t);
        land_detail_reads(rig, t);
    }

    /// Run the held detail fetches (tests do not race a worker) and install what they brought.
    fn land_detail_reads(rig: &mut Rig, t: &mut u32) {
        rig.app.bridge.run_held_store_jobs_for_test();
        rig.app.bridge.metadata_mut().pump_detail();
        settle(&mut rig.app, t);
    }

    /// A Person page pushed over the page on top (a cast member opened from it).
    fn cover_with_person(rig: &mut Rig, t: &mut u32) {
        let below = rig.app.pages.nav.top_page().unwrap().id;
        super::super::bridge::nav_push(&mut rig.app.pages, AppArg::Content(plx_screens::registry::ContentArg::Person {
            sid: rig.sid, key: "9".into(), guid: "tag://9".into(), name: String::new(), thumb: String::new(),
        }));
        settle(&mut rig.app, t);
        assert_ne!(rig.app.pages.nav.top_page().map(|e| e.id), Some(below), "premise: the Person page covers it");
    }

    /// The filmography menu's Mark watched on the show (a Show-scope press from a catalog row), run
    /// until the server has taken it and been asked what it now holds.
    fn mark_show_and_read_back(rig: &mut Rig, t: &mut u32) {
        assert!(rig.app.bridge.viewstate_run(plx_data::stores::viewstate::ViewStateCmd::Request {
            sid: rig.sid, rk: "100".into(), write: plx_data::viewstate::Write::Watched,
            scope: plx_data::viewstate::Scope::Show, guid: String::new(),
        }).changed);
        poll_until("the write and its read-back never finished", || {
            step_and_run_jobs(rig, t);
            let seen = rig.requests.lock().unwrap();
            seen.iter().position(|r| r.contains("/:/scrobble"))
                .is_some_and(|at| seen.iter().skip(at).any(|r| r.contains("GET /library/metadata/100") && !r.contains("children")))
        });
        for _ in 0..12 { step(&mut rig.app, t, vec![]); }
    }

    /// The loaded show's episodes as `(rk, watched)`, and its season's `viewedLeafCount`.
    fn strip(rig: &Rig) -> (Vec<(String, bool)>, Option<i64>) {
        let view = rig.app.bridge.metadata_view();
        let Some(d) = view.current().filter(|d| d.rk == "100") else { return (Vec::new(), None) };
        let eps = (0..2).filter_map(|i| d.episodes.get(i).map(|e| (e.rk.clone(), e.watched))).collect();
        (eps, d.seasons.first().map(|s| s.viewed_leaf_count))
    }

    /// **Plan item 9**: Detail(show S) -> Person -> mark S watched from the filmography menu ->
    /// Back. The Detail page was COVERED when the server's answer came back, and the only thing
    /// that reached it was the optimistic patch; the page it returns to must show what the server
    /// now holds for every episode, season tab included.
    #[test]
    fn a_show_marked_watched_under_a_covered_detail_page_is_all_watched_on_return() {
        let _serial = plx_base::testlock::serial();
        let marked = Arc::new(AtomicBool::new(false));
        let Some(mut rig) = Rig::serving_with(marked_show_server(marked.clone(), false)) else { return };
        let mut t = product_transition(&mut rig.app);
        open_marked_show(&mut rig, &mut t);
        assert_eq!(strip(&rig), (vec![("1".into(), false), ("2".into(), false)], Some(0)),
            "premise: the page opened on an unwatched show");
        let detail = rig.app.pages.nav.top_page().unwrap().id;
        cover_with_person(&mut rig, &mut t);
        mark_show_and_read_back(&mut rig, &mut t);
        let back = back_key(t + 16);
        step(&mut rig.app, &mut t, vec![back]);
        settle(&mut rig.app, &mut t);
        assert_eq!(rig.app.pages.nav.top_page().map(|e| e.id), Some(detail), "premise: Back returned to the show");
        land_detail_reads(&mut rig, &mut t);
        assert_eq!(strip(&rig), (vec![("1".into(), true), ("2".into(), true)], Some(2)),
            "the strip is not all watched on return");
    }

    /// The same covered page when what the server holds is NOT what the optimistic patch drew:
    /// the write is taken but only the first episode got marked. The patch is all a covered page
    /// ever received, so without a re-read of its own it comes back showing a lie.
    #[test]
    fn a_covered_detail_page_shows_what_the_server_holds_after_a_partial_write() {
        let _serial = plx_base::testlock::serial();
        let marked = Arc::new(AtomicBool::new(false));
        let Some(mut rig) = Rig::serving_with(marked_show_server(marked.clone(), true)) else { return };
        let mut t = product_transition(&mut rig.app);
        open_marked_show(&mut rig, &mut t);
        let detail = rig.app.pages.nav.top_page().unwrap().id;
        cover_with_person(&mut rig, &mut t);
        mark_show_and_read_back(&mut rig, &mut t);
        assert_eq!(strip(&rig), (vec![("1".into(), true), ("2".into(), true)], Some(2)),
            "premise: the covered page holds the optimistic patch");
        let back = back_key(t + 16);
        step(&mut rig.app, &mut t, vec![back]);
        settle(&mut rig.app, &mut t);
        assert_eq!(rig.app.pages.nav.top_page().map(|e| e.id), Some(detail), "premise: Back returned to the show");
        land_detail_reads(&mut rig, &mut t);
        assert_eq!(strip(&rig), (vec![("1".into(), true), ("2".into(), false)], Some(1)),
            "the page came back showing the optimistic patch, not the server's word");
    }

    /// **Layer-1 view state, G1**: Home's hubs (Continue Watching) are re-read after a playback
    /// only once the stop has reached the server, not 800 ms after the exit. The `stopped`
    /// timeline reply is held, the frame clock runs far past the old timer, and no refetch may
    /// have started; once the reply is released the refetch runs exactly once.
    #[test]
    fn hubs_are_refetched_after_the_stop_lands_not_after_a_timer() {
        let _serial = plx_base::testlock::serial();
        let (hold, held) = mpsc::channel::<()>();
        let held = Mutex::new(held);
        let stop_seen = Arc::new(AtomicBool::new(false));
        let seen = stop_seen.clone();
        let Some(mut rig) = Rig::serving_with(move |request: &str| {
            if request.contains("/:/timeline") && request.contains("state=stopped") {
                seen.store(true, Ordering::SeqCst);
                let _ = held.lock().unwrap().recv_timeout(Duration::from_secs(20));
                return r#"{"MediaContainer":{}}"#.into();
            }
            if request.contains("/:/timeline") {
                return r#"{"MediaContainer":{}}"#.into();
            }
            queue_body(request, false).to_owned()
        }) else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        play_with_a_landing_inside_the_dip(&mut rig, &mut t);
        let before = rig.app.bridge.hub_refetches_for_test();
        let back = back_key(t + 16);
        step(&mut rig.app, &mut t, vec![back]);
        // The rig has no native engine, so teardown posts no `stopped` report. The engine makes
        // exactly this call (`engine.rs`, `route::scrobble_stop`); it is made here, once BACK's own
        // transcode stop has drained, so the held report is the only stop in flight.
        plx_media::route::scrobble_stop(&mut rig.app.player.session, Some(("1".into(), 30_000, 60_000)), None);
        settle_after_back(&mut rig.app, &mut t);
        assert!(super::super::bridge::player(&rig.app.pages).is_none(), "premise: BACK left the player");
        poll_until("premise: the stopped timeline never reached the server", || stop_seen.load(Ordering::SeqCst));
        assert!(!plx_media::route::scrobble_settled(), "premise: the stop is still in flight");
        // The old 800 ms timer is long past; the stop has not landed.
        t += 5_000;
        for _ in 0..4 { step(&mut rig.app, &mut t, vec![]); }
        assert_eq!(
            rig.app.bridge.hub_refetches_for_test(), before,
            "the hubs were re-read before the stop reached the server",
        );
        hold.send(()).unwrap();
        poll_until("the stop never settled", plx_media::route::scrobble_settled);
        step(&mut rig.app, &mut t, vec![]);
        assert_eq!(rig.app.bridge.hub_refetches_for_test(), before + 1, "the refetch runs once the stop landed");
        for _ in 0..4 { step(&mut rig.app, &mut t, vec![]); }
        assert_eq!(rig.app.bridge.hub_refetches_for_test(), before + 1, "and only once");
    }

    /// **Layer-1 view state, G1 on a transcoded play.** The stop worker posts `stopped` and THEN
    /// asks the server to stop the encoder; a slow PMS holds that second round trip for a full
    /// timeout. The resume point is committed by the first, so the hubs refetch must not wait for
    /// the second. The rig plays direct, so the encoder is registered through the media crate's
    /// test seam (what `apply_plan` does for a real transcode); the encoder stop is held while
    /// `stopped` is answered at once.
    #[test]
    fn hubs_are_refetched_once_stopped_lands_even_while_the_encoder_stop_is_held() {
        let _serial = plx_base::testlock::serial();
        let (hold, held) = mpsc::channel::<()>();
        let held = Mutex::new(held);
        let encoder_stop_seen = Arc::new(AtomicBool::new(false));
        let seen = encoder_stop_seen.clone();
        let Some(mut rig) = Rig::serving_with(move |request: &str| {
            if request.contains("/video/:/transcode/universal/stop?session=rig-encoder") {
                seen.store(true, Ordering::SeqCst);
                let _ = held.lock().unwrap().recv_timeout(Duration::from_secs(20));
                return String::new();
            }
            if request.contains("/:/timeline") {
                return r#"{"MediaContainer":{}}"#.into();
            }
            queue_body(request, false).to_owned()
        }) else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        play_with_a_landing_inside_the_dip(&mut rig, &mut t);
        let before = rig.app.bridge.hub_refetches_for_test();
        let back = back_key(t + 16);
        step(&mut rig.app, &mut t, vec![back]);
        // As in `hubs_are_refetched_after_the_stop_lands_not_after_a_timer`: the engine's own
        // `scrobble_stop` call, made once BACK's stop has drained. The encoder is registered
        // first, so the worker's second leg is the held `transcode_stop`.
        plx_media::route::install_active_encoder_for_test("rig-encoder");
        plx_media::route::scrobble_stop(&mut rig.app.player.session, Some(("1".into(), 30_000, 60_000)), None);
        settle_after_back(&mut rig.app, &mut t);
        assert!(super::super::bridge::player(&rig.app.pages).is_none(), "premise: BACK left the player");
        poll_until("premise: the encoder stop never reached the server", || encoder_stop_seen.load(Ordering::SeqCst));
        assert!(!plx_media::route::scrobble_settled(), "premise: the worker is inside the held encoder stop");
        t += 5_000;
        for _ in 0..4 { step(&mut rig.app, &mut t, vec![]); }
        let refetches = rig.app.bridge.hub_refetches_for_test();
        hold.send(()).unwrap();
        plx_media::route::drain_scrobble();
        assert_eq!(
            refetches, before + 1,
            "the hubs waited for the server-side encoder stop, though `stopped` had landed",
        );
    }

    // ---- layer-1 view state, item 10c: Home's return hold ---------------------------------------

    /// A server whose Continue Watching holds episode `1` of show `S` (and two movies) until a
    /// `stopped` timeline has arrived, then the show's NEXT episode `2` (the movie `7` gone, a new
    /// movie `9` in). While `hold_hubs` is set the hub read parks before it answers.
    struct ReturnServer {
        stopped: Arc<AtomicBool>,
        hold_hubs: Arc<AtomicBool>,
        /// The hub read answers with something that is not a catalog (the fetch fails).
        break_hubs: Arc<AtomicBool>,
        /// Continue Watching is ordered by last viewed: before the stop the show's episode is the
        /// LAST tile, and after it the next episode comes FIRST.
        jump: Arc<AtomicBool>,
    }

    impl ReturnServer {
        fn new() -> Self {
            Self {
                stopped: Arc::new(AtomicBool::new(false)),
                hold_hubs: Arc::new(AtomicBool::new(false)),
                break_hubs: Arc::new(AtomicBool::new(false)),
                jump: Arc::new(AtomicBool::new(false)),
            }
        }

        fn rig(&self) -> Option<Rig> {
            let (stopped, hold, broken, jump) =
                (self.stopped.clone(), self.hold_hubs.clone(), self.break_hubs.clone(), self.jump.clone());
            Rig::serving_with(move |request: &str| {
                if request.contains("/decision?") {
                    return r#"{"MediaContainer":{"Metadata":[{"Media":[{"Part":[{"decision":"directplay"}]}]}]}}"#.into();
                }
                if request.contains("/:/timeline") {
                    if request.contains("state=stopped") {
                        stopped.store(true, Ordering::SeqCst);
                    }
                    return r#"{"MediaContainer":{}}"#.into();
                }
                if request.starts_with("GET /hubs/continueWatching") {
                    // Bounded by count, not by a clock: a failing test must not hang.
                    for _ in 0..5_000 {
                        if !hold.load(Ordering::SeqCst) { break; }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    if broken.load(Ordering::SeqCst) { return "not a catalog".into(); }
                    let done = stopped.load(Ordering::SeqCst);
                    let (episode, movies) = if done { ("2", ["8", "9"]) } else { ("1", ["7", "8"]) };
                    let episode_card = format!(r#"{{"ratingKey":"{episode}","type":"episode","title":"Episode {episode}","thumb":"/t/{episode}",
                        "grandparentRatingKey":"S","grandparentTitle":"Show","parentIndex":1,"index":{episode},"duration":60000}}"#);
                    let movie = |rk: &str| format!(r#"{{"ratingKey":"{rk}","type":"movie","title":"Movie","thumb":"/t/m","duration":60000}}"#);
                    // `jump`: the episode is last until it is watched, then first.
                    let cards = if jump.load(Ordering::SeqCst) && !done {
                        [movie(movies[0]), movie(movies[1]), episode_card]
                    } else {
                        [episode_card, movie(movies[0]), movie(movies[1])]
                    };
                    return format!(r#"{{"MediaContainer":{{"size":1,"Hub":[{{"key":"/hubs/continueWatching",
                      "title":"Continue Watching","hubIdentifier":"home.continue","size":3,"Metadata":[{}]}}]}}}}"#, cards.join(","));
                }
                if request.starts_with("GET /hubs") {
                    return r#"{"MediaContainer":{"size":0}}"#.into();
                }
                queue_body(request, false).to_owned()
            })
        }
    }

    /// `step` with the frame clock moved by `advance` ms instead of 16: `0` is a PARKED clock, the
    /// same `Tick::ms` again, for waiting on a worker without the hold's frame-time bound running.
    fn step_clock(app: &mut App, t: &mut u32, advance: u32) {
        *t += advance;
        let mut fr = Frame::begin(&app.player.session, app.bridge.metadata_view());
        fr.now = *t;
        fr.dt = advance as f32 / 1000.0;
        let was_player = super::super::bridge::player(&app.pages).is_some();
        unsafe { playback_tick(app, &mut fr); }
        super::super::bridge::frame(&mut app.pages, &mut app.bridge,
            plx_machine::machine::Tick { ms: *t, dt_us: advance * 1000 }, std::mem::take(&mut app.inputs));
        if was_player && super::super::bridge::player(&app.pages).is_none() {
            restore_played_entry(app);
        }
        content_requests(app, &fr);
        loop_requests(app);
        unsafe { update(app, &mut fr); }
    }

    fn home_probe(app: &App) -> plx_screens::home::HomeProbe {
        let entry = app.pages.nav.tabs.stack.entries.iter().find(|e| e.arg == AppArg::Home).expect("a Home entry").id;
        app.pages.nav.entry(entry).and_then(|e| e.inst.as_ref()).and_then(|i| i.screen.as_any())
            .and_then(|s| s.downcast_ref::<plx_screens::home::HomeScreen>())
            .expect("a mounted Home").probe_for_test()
    }

    fn home_focus(app: &App) -> Option<u32> {
        let entry = app.pages.nav.tabs.stack.entries.iter().find(|e| e.arg == AppArg::Home)?.id;
        app.pages.input.engine.current(plx_machine::machine::InputOwner::Entry(entry)).map(|key| key.elem)
    }

    /// The first Continue Watching card's rating key in the hubs the app holds right now.
    fn first_cw_rk(app: &App) -> String { cw_rk(app, 0) }

    fn cw_rk(app: &App, col: usize) -> String {
        let snapshot = app.bridge.hubs_snapshot();
        snapshot.view().hub(0).and_then(|hub| hub.items.get(col).map(|item| item.rk.to_string())).unwrap_or_default()
    }

    /// Home with its Continue Watching row read from the server (episode 1 first), focus on that
    /// first tile, and the product's page transition on the stack.
    fn home_with_continue_watching(rig: &mut Rig, t: &mut u32) {
        home_with_continue_watching_at(rig, t, 0);
    }

    /// [`home_with_continue_watching`] with focus on Continue Watching tile `col`, which must hold
    /// episode 1.
    fn home_with_continue_watching_at(rig: &mut Rig, t: &mut u32, col: usize) {
        rig.release.send(()).unwrap(); // nothing parks: the first request answers at once
        rig.app.bridge.seed_registered_browse_for_test([rig.sid, rig.sid]);
        let _ = rig.app.bridge.hubs_run(plx_data::stores::hubs::HubsCmd::RefetchHubs);
        poll_until("Home never read its Continue Watching", || {
            step(&mut rig.app, t, vec![]);
            rig.app.bridge.hub_len_for_test(0) == 3
        });
        settle(&mut rig.app, t);
        let entry = rig.app.pages.nav.top_page().unwrap().id;
        let instance = rig.app.pages.nav.instance_of(entry).unwrap();
        rig.app.pages.emit(plx_machine::machine::MachineId::Nav, plx_machine::machine::Fx::Deliver(
            plx_machine::machine::MachineId::Instance(instance),
            plx_machine::machine::Delivery::Screen(plx_ui::screen::ScreenEvent::App(
                plx_screens::registry::AppMsg::Home(plx_screens::registry::HomeCmd::FocusGrid { row: 0, col })))));
        for _ in 0..12 { step(&mut rig.app, t, vec![]); }
        assert_eq!(cw_rk(&rig.app, col), "1", "premise: the focused Continue Watching tile holds episode 1");
        assert_eq!(home_focus(&rig.app), home_probe(&rig.app).cw_elems.get(col).copied(),
            "premise: focus is on that Continue Watching tile");
    }

    /// Play episode 1 from Home and leave the player with BACK, the stop the engine would report
    /// posted at once. Returns the frames until the dip reaches its dark floor (`(alpha, route)`
    /// sampled after each), stopping there.
    fn back_out_to_the_floor(rig: &mut Rig, t: &mut u32) {
        play_with_a_landing_inside_the_dip(rig, t);
        let back = back_key(*t + 16);
        step(&mut rig.app, t, vec![back]);
        plx_media::route::scrobble_stop(&mut rig.app.player.session, Some(("1".into(), 30_000, 60_000)), None);
        for _ in 0..40 {
            if rig.app.pages.nav.tabs.stack.transition.page_alpha() == 0.0 { return; }
            step(&mut rig.app, t, vec![]);
        }
        panic!("the dip never reached its floor");
    }

    /// **10c (i)**: the hubs answer within the bound. The dip waits dark at its floor for the
    /// refetch, and the first frame Home is on top shows the NEXT episode in the tile that held
    /// the first, focused, with nothing animating: Home appears already correct.
    #[test]
    fn a_refetch_landing_inside_the_return_hold_shows_on_the_first_home_frame() {
        let _serial = plx_base::testlock::serial();
        let server = ReturnServer::new();
        let Some(mut rig) = server.rig() else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        home_with_continue_watching(&mut rig, &mut t);
        let tile = home_focus(&rig.app).expect("focus on the tile");
        back_out_to_the_floor(&mut rig, &mut t);
        let at_floor = t;
        // The clock is parked while the worker answers (real time), so the bound cannot run out.
        // (Without a hold the dip has already crossed its floor and there is nothing to wait for.)
        if super::super::bridge::player(&rig.app.pages).is_some() {
            // (Settled, not merely changed: the refetch owes one read per source in scope.)
            poll_until("the refetch never landed", || {
                step_clock(&mut rig.app, &mut t, 0);
                !rig.app.refresh_hubs && !rig.app.bridge.hubs_owed()
            });
        }
        assert_eq!(t, at_floor, "premise: the landing came inside the hold");
        // Frame by frame to the first frame Home is the page on top.
        for _ in 0..40 {
            if super::super::bridge::player(&rig.app.pages).is_none() { break; }
            step(&mut rig.app, &mut t, vec![]);
        }
        assert!(t - at_floor < 300, "premise: inside the 300 ms hold ({} ms)", t - at_floor);
        assert!(matches!(rig.app.route(), AppArg::Home), "premise: Home is on top");
        assert_eq!(first_cw_rk(&rig.app), "2", "the refetch is in");
        let probe = home_probe(&rig.app);
        assert_eq!(probe.cw_elems.first().copied(), Some(tile), "episode 2 sits in episode 1's tile");
        assert_eq!(home_focus(&rig.app), Some(tile), "and the focus never left it");
        assert!(!probe.animating, "Home is already correct: no row animation on its first frame");
    }

    /// **10c (ii)**: the hubs answer after the bound. The hold ends at 300 ms past the floor with the
    /// OLD content on the first Home frame; when the read lands the same tile shows episode 2 (the
    /// element is the same one, no ghost for it) while the other cells animate.
    #[test]
    fn a_refetch_later_than_the_return_hold_animates_in_after_the_hold_is_released() {
        let _serial = plx_base::testlock::serial();
        let server = ReturnServer::new();
        let Some(mut rig) = server.rig() else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        home_with_continue_watching(&mut rig, &mut t);
        let tile = home_focus(&rig.app).expect("focus on the tile");
        server.hold_hubs.store(true, Ordering::SeqCst);
        back_out_to_the_floor(&mut rig, &mut t);
        let at_floor = t;
        for _ in 0..40 {
            if super::super::bridge::player(&rig.app.pages).is_none() { break; }
            step(&mut rig.app, &mut t, vec![]);
        }
        let held = t - at_floor;
        assert!(matches!(rig.app.route(), AppArg::Home), "premise: Home is on top");
        assert!((284..=316).contains(&held), "the hold ran its 300 ms and no longer: {held} ms past the floor");
        assert_eq!(first_cw_rk(&rig.app), "1", "the refetch is still out: Home shows what it had");
        assert!(!home_probe(&rig.app).animating);
        // 600 ms past the floor the read lands.
        while t - at_floor < 600 { step(&mut rig.app, &mut t, vec![]); }
        assert_eq!(first_cw_rk(&rig.app), "1");
        server.hold_hubs.store(false, Ordering::SeqCst);
        let mut animated = false;
        // Until the tile reads episode 2 (the read answers in real time, whatever else is landing),
        // watching every frame for the row's animation.
        poll_until("the late refetch never landed", || {
            step(&mut rig.app, &mut t, vec![]);
            animated |= home_probe(&rig.app).animating;
            first_cw_rk(&rig.app) == "2"
        });
        for _ in 0..4 {
            step(&mut rig.app, &mut t, vec![]);
            animated |= home_probe(&rig.app).animating;
        }
        assert_eq!(first_cw_rk(&rig.app), "2");
        let probe = home_probe(&rig.app);
        assert_eq!(probe.cw_elems.first().copied(), Some(tile), "the same tile now shows episode 2");
        assert_eq!(home_focus(&rig.app), Some(tile), "and holds the focus");
        assert!(animated, "the other cells animate in (a card left and a card arrived)");
    }

    /// **A quiet read out when the player exits is not the return hold's wait, and the prompted
    /// refetch supersedes it.** A quiet read of the played source is out (parked at the server) when
    /// the person backs out. The prompted refetch the exit owes supersedes it, so the dip waits at
    /// its floor on the prompted read alone, and when that lands Home shows its content: the quiet
    /// read's landing neither stands in for it nor takes its place.
    #[test]
    fn a_quiet_read_out_at_the_exit_is_superseded_by_the_prompted_refetch_the_hold_waits_on() {
        let _serial = plx_base::testlock::serial();
        let server = ReturnServer::new();
        let Some(mut rig) = server.rig() else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        home_with_continue_watching(&mut rig, &mut t);
        let tile = home_focus(&rig.app).expect("focus on the tile");
        // Back out as `back_out_to_the_floor` does, but with the quiet read going out between the
        // server taking the stop and the app issuing the refetch it owes (the fixture serves one
        // request at a time, so the parked read must come after the stop).
        play_with_a_landing_inside_the_dip(&mut rig, &mut t);
        let back = back_key(t + 16);
        step(&mut rig.app, &mut t, vec![back]);
        plx_media::route::scrobble_stop(&mut rig.app.player.session, Some(("1".into(), 30_000, 60_000)), None);
        poll_until("the server never took the stop", || server.stopped.load(Ordering::SeqCst));
        server.hold_hubs.store(true, Ordering::SeqCst);
        let gen = rig.app.bridge.hub_refetches_for_test();
        let _ = rig.app.bridge.hubs_run(plx_data::stores::hubs::HubsCmd::RefetchSourceQuiet { sid: rig.sid });
        assert_eq!(rig.app.bridge.hub_refetches_for_test(), gen, "a quiet ask never moves the generation");
        assert!(rig.app.bridge.hub_source_settled(rig.sid), "premise: a quiet read is nobody's wait");
        assert!(rig.app.bridge.hubs_owed(), "premise: and it is out");
        for _ in 0..40 {
            if rig.app.pages.nav.tabs.stack.transition.page_alpha() == 0.0 { break; }
            step(&mut rig.app, &mut t, vec![]);
        }
        assert!(rig.app.pages.nav.tabs.stack.transition.page_alpha() == 0.0, "the dip reached its floor");
        let at_floor = t;
        poll_until("the prompted refetch never went out", || {
            step_clock(&mut rig.app, &mut t, 0);
            !rig.app.bridge.hub_source_settled(rig.sid)
        });
        assert!(rig.app.bridge.hub_refetches_for_test() > gen, "the exit's refetch is authoritative");
        for _ in 0..8 { step_clock(&mut rig.app, &mut t, 0); }
        assert!(rig.app.pages.nav.tabs.stack.transition.floor_held(), "the dip waits dark at its floor");
        assert!(!rig.app.bridge.hub_source_settled(rig.sid), "on the prompted read, which is still out");
        assert_eq!(t, at_floor);
        server.hold_hubs.store(false, Ordering::SeqCst);
        poll_until("the prompted read never landed", || {
            step_clock(&mut rig.app, &mut t, 0);
            !rig.app.refresh_hubs && rig.app.bridge.hub_source_settled(rig.sid)
        });
        for _ in 0..40 {
            if super::super::bridge::player(&rig.app.pages).is_none() { break; }
            step(&mut rig.app, &mut t, vec![]);
        }
        assert!(matches!(rig.app.route(), AppArg::Home), "premise: Home is on top");
        assert_eq!(first_cw_rk(&rig.app), "2", "the prompted read's content is what Home shows");
        assert_eq!(home_probe(&rig.app).cw_elems.first().copied(), Some(tile), "episode 2 sits in the same tile");
    }

    /// **10c, a key during the hold**: the dip is dark and waiting on a refetch that has not
    /// answered. A fresh key press ends the wait on the spot, so input never queues behind the
    /// read; Home appears with the content it has. The press is NOT delivered to the player that is
    /// still on top for this frame: it is held out of the frame that releases the dip and goes to
    /// Home, the page the viewer is looking at, on the next one (focus moves one tile right).
    #[test]
    fn a_key_press_ends_the_return_hold_at_once() {
        let _serial = plx_base::testlock::serial();
        plx_base::eventlog::with_private_log(|| {
        let server = ReturnServer::new();
        let Some(mut rig) = server.rig() else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        home_with_continue_watching(&mut rig, &mut t);
        let tiles = home_probe(&rig.app).cw_elems;
        server.hold_hubs.store(true, Ordering::SeqCst);
        back_out_to_the_floor(&mut rig, &mut t);
        let at_floor = t;
        for _ in 0..4 { step(&mut rig.app, &mut t, vec![]); }
        assert!(super::super::bridge::player(&rig.app.pages).is_some(), "premise: the dip is still waiting");
        assert!(rig.app.pages.nav.tabs.stack.transition.floor_held(), "premise: the hold is armed");
        assert_eq!(home_focus(&rig.app), Some(tiles[0]), "premise: focus is on the first tile");
        let key = plx_ui::fixture::key(plx_machine::machine::Key::Right, plx_ui::fixture::tick(t + 16));
        step(&mut rig.app, &mut t, vec![key]);
        assert_eq!(home_focus(&rig.app), Some(tiles[0]),
            "the press went to no page this frame: the player is still on top and must not get it");
        for _ in 0..40 {
            if super::super::bridge::player(&rig.app.pages).is_none() { break; }
            step(&mut rig.app, &mut t, vec![]);
        }
        assert!(matches!(rig.app.route(), AppArg::Home), "Home is on top");
        assert!(t - at_floor < 150, "the key ended the wait ({} ms past the floor, bound 300)", t - at_floor);
        assert_eq!(first_cw_rk(&rig.app), "1", "the refetch was still out");
        for _ in 0..3 { step(&mut rig.app, &mut t, vec![]); }
        assert_eq!(home_focus(&rig.app), Some(tiles[1]), "the press reached Home: focus moved one tile right");
        let log = std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default();
        assert!(log.contains("return hold released by key after"), "the release is logged with its cause: {log}");
        server.hold_hubs.store(false, Ordering::SeqCst);
        });
    }

    /// **10c, the key that left the player is not the key that ends the hold.** BACK exits on its
    /// Down; its own Up arrives a frame or two later, while the dip is dark. A release, a repeat, a
    /// pointer move or a system-keyboard event is not a decision to act on the new screen.
    #[test]
    fn the_backs_own_key_up_does_not_end_the_return_hold() {
        let _serial = plx_base::testlock::serial();
        let server = ReturnServer::new();
        let Some(mut rig) = server.rig() else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        home_with_continue_watching(&mut rig, &mut t);
        server.hold_hubs.store(true, Ordering::SeqCst);
        play_with_a_landing_inside_the_dip(&mut rig, &mut t);
        let back = back_key(t + 16);
        step(&mut rig.app, &mut t, vec![back]);
        plx_media::route::scrobble_stop(&mut rig.app.player.session, Some(("1".into(), 30_000, 60_000)), None);
        step(&mut rig.app, &mut t, vec![]);
        let up = plx_ui::fixture::key_up(plx_machine::machine::Key::Back, plx_ui::fixture::tick(t + 16));
        step(&mut rig.app, &mut t, vec![up]);
        let mut repeat = plx_ui::fixture::key(plx_machine::machine::Key::Back, plx_ui::fixture::tick(t + 32));
        if let plx_machine::machine::InputKind::Key { edge, .. } = &mut repeat.kind {
            *edge = plx_machine::machine::Edge::Repeat;
        }
        step(&mut rig.app, &mut t, vec![repeat]);
        assert!(rig.app.pages.nav.tabs.stack.transition.floor_held(),
            "BACK's own Up and Repeat leave the hold armed");
        assert!(super::super::bridge::player(&rig.app.pages).is_some());
        server.hold_hubs.store(false, Ordering::SeqCst);
    }

    /// **10c, nothing was stopped: nothing is waited for.** A play that never started (a refused
    /// decision, a BACK during the load) announces no stop, so the return is not held for a refetch
    /// that has no reason to differ.
    #[test]
    fn an_exit_that_reported_no_stop_does_not_hold_the_return() {
        let _serial = plx_base::testlock::serial();
        let server = ReturnServer::new();
        let Some(mut rig) = server.rig() else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        home_with_continue_watching(&mut rig, &mut t);
        server.hold_hubs.store(true, Ordering::SeqCst);
        play_with_a_landing_inside_the_dip(&mut rig, &mut t);
        let back = back_key(t + 16);
        step(&mut rig.app, &mut t, vec![back]);
        // The stop this exit announced is accounted for, as an exit that announces none leaves it.
        rig.app.return_hold.refetch_issued();
        let at = t;
        let mut held = false;
        for _ in 0..40 {
            held |= rig.app.pages.nav.tabs.stack.transition.floor_held();
            if super::super::bridge::player(&rig.app.pages).is_none() { break; }
            step(&mut rig.app, &mut t, vec![]);
        }
        assert!(matches!(rig.app.route(), AppArg::Home), "Home is on top");
        assert!(!held, "no stop was announced, so the dip never waited");
        assert!(t - at < 200, "and Home came at the dip's own pace ({} ms)", t - at);
        server.hold_hubs.store(false, Ordering::SeqCst);
    }

    /// **10c, the played item's source is the one waited for.** A second source that is slow to
    /// answer keeps the adapter owing a landing, but it is not what the viewer returned to see:
    /// the hold ends when the played item's own server has answered.
    #[test]
    fn a_slow_second_source_does_not_spend_the_return_hold() {
        let _serial = plx_base::testlock::serial();
        let server = ReturnServer::new();
        let Some(mut rig) = server.rig() else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        home_with_continue_watching(&mut rig, &mut t);
        // A friend's server that accepts the connection and never answers.
        let hole = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        hole.set_nonblocking(true).unwrap();
        let hole_port = hole.local_addr().unwrap().port();
        let hole_open = Arc::new(AtomicBool::new(true));
        let open = hole_open.clone();
        // Accepts and holds every connection without a word until the test is over.
        let holder = plx_base::task::spawn_small_keeping("return-hold-silent-server", move || {
            let mut held = Vec::new();
            while open.load(Ordering::SeqCst) {
                match plx_base::testnet::accept(&hole) {
                    Ok((socket, _)) => held.push(socket),
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        let slow = plx_plex::plex::register_for_test("slow-friend", "127.0.0.1", hole_port as i32,
            "synthetic", "slow-friend");
        // (Not in Browse's pin table, so undecided, so it feeds Home: `pms::feeds_home`.)
        let _ = slow;
        back_out_to_the_floor(&mut rig, &mut t);
        let at_floor = t;
        // Parked clock while the played source answers (real time).
        poll_until("the played source never answered", || {
            step_clock(&mut rig.app, &mut t, 0);
            first_cw_rk(&rig.app) == "2"
        });
        assert!(rig.app.bridge.hubs_owed(), "premise: the slow source still owes its landing");
        for _ in 0..40 {
            if super::super::bridge::player(&rig.app.pages).is_none() { break; }
            step(&mut rig.app, &mut t, vec![]);
        }
        assert!(matches!(rig.app.route(), AppArg::Home), "Home is on top");
        assert!(t - at_floor < 150, "the slow source did not hold the return ({} ms past the floor, bound 300)", t - at_floor);
        assert_eq!(first_cw_rk(&rig.app), "2");
        hole_open.store(false, Ordering::SeqCst);
        holder.expect("the silent server's thread").join().unwrap();
    }

    /// **10b/10c, the next episode comes first.** Continue Watching is ordered by last viewed, so
    /// the finished episode's tile (the last one) is answered by the next episode in column 0. The
    /// element goes with the card: the focus never leaves it, and it is the first tile now.
    #[test]
    fn a_next_episode_that_jumps_to_column_zero_keeps_the_focus_inside_the_hold() {
        let _serial = plx_base::testlock::serial();
        let server = ReturnServer::new();
        server.jump.store(true, Ordering::SeqCst);
        let Some(mut rig) = server.rig() else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        home_with_continue_watching_at(&mut rig, &mut t, 2);
        let tile = home_focus(&rig.app).expect("focus on the tile");
        back_out_to_the_floor(&mut rig, &mut t);
        if super::super::bridge::player(&rig.app.pages).is_some() {
            poll_until("the refetch never settled", || {
                step_clock(&mut rig.app, &mut t, 0);
                !rig.app.refresh_hubs && !rig.app.bridge.hubs_owed()
            });
        }
        for _ in 0..40 {
            if super::super::bridge::player(&rig.app.pages).is_none() { break; }
            step(&mut rig.app, &mut t, vec![]);
        }
        assert!(matches!(rig.app.route(), AppArg::Home), "premise: Home is on top");
        assert_eq!(first_cw_rk(&rig.app), "2", "the next episode is the first card");
        assert_eq!(home_probe(&rig.app).cw_elems.first().copied(), Some(tile), "on the element that held episode 1");
        assert_eq!(home_focus(&rig.app), Some(tile), "and the focus never left it");
    }

    /// The same, landing after the hold was released: the old order shows first, then the tile
    /// moves to column 0 with the focus on it.
    #[test]
    fn a_next_episode_that_jumps_to_column_zero_keeps_the_focus_when_it_lands_late() {
        let _serial = plx_base::testlock::serial();
        let server = ReturnServer::new();
        server.jump.store(true, Ordering::SeqCst);
        let Some(mut rig) = server.rig() else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        home_with_continue_watching_at(&mut rig, &mut t, 2);
        let tile = home_focus(&rig.app).expect("focus on the tile");
        server.hold_hubs.store(true, Ordering::SeqCst);
        back_out_to_the_floor(&mut rig, &mut t);
        for _ in 0..40 {
            if super::super::bridge::player(&rig.app.pages).is_none() { break; }
            step(&mut rig.app, &mut t, vec![]);
        }
        assert!(matches!(rig.app.route(), AppArg::Home), "premise: Home is on top");
        assert_eq!(cw_rk(&rig.app, 2), "1", "premise: the refetch is still out");
        while t % 16 != 0 || t < 700 { step(&mut rig.app, &mut t, vec![]); }
        server.hold_hubs.store(false, Ordering::SeqCst);
        poll_until("the late refetch never landed", || {
            step(&mut rig.app, &mut t, vec![]);
            first_cw_rk(&rig.app) == "2"
        });
        for _ in 0..30 { step(&mut rig.app, &mut t, vec![]); }
        assert_eq!(home_probe(&rig.app).cw_elems.first().copied(), Some(tile), "the element moved to column 0 with its card");
        assert_eq!(home_focus(&rig.app), Some(tile), "and holds the focus");
    }

    /// **10c, a refetch that fails**: a failure moves no catalog generation, but it still settles
    /// the hold: the dip does not sit out its bound for an answer that is never coming.
    #[test]
    fn a_failed_refetch_ends_the_return_hold_at_once() {
        let _serial = plx_base::testlock::serial();
        let server = ReturnServer::new();
        let Some(mut rig) = server.rig() else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        home_with_continue_watching(&mut rig, &mut t);
        server.break_hubs.store(true, Ordering::SeqCst);
        back_out_to_the_floor(&mut rig, &mut t);
        let at_floor = t;
        // Parked clock while the worker fails (real time), so the bound cannot be what ends it.
        if super::super::bridge::player(&rig.app.pages).is_some() {
            poll_until("the failed refetch never settled", || {
                step_clock(&mut rig.app, &mut t, 0);
                !rig.app.refresh_hubs && !rig.app.bridge.hubs_owed()
            });
        }
        for _ in 0..40 {
            if super::super::bridge::player(&rig.app.pages).is_none() { break; }
            step(&mut rig.app, &mut t, vec![]);
        }
        assert!(matches!(rig.app.route(), AppArg::Home), "Home is on top");
        assert!(t - at_floor < 150, "the failure ended the wait ({} ms past the floor, bound 300)", t - at_floor);
        assert_eq!(first_cw_rk(&rig.app), "1", "Home keeps what it had");
    }

    // ---- layer-1 view state, item 6: what a stop tells the screens -----------------------------

    /// A server for the stop tests. Item `1` reads back as watched once a `stopped` timeline has
    /// arrived and `ends` is set (the stream reached its end), else with a resume point of 150 s (PMS
    /// stores no `stopped` under about a minute, so a mid-way stop in these tests is a realistic one).
    /// Metadata reads park while `hold_reads` is set, so a test can look at the screens between the
    /// stop and the server's answer.
    struct StopServer {
        stopped: Arc<AtomicBool>,
        hold_reads: Arc<AtomicBool>,
        /// While set, the `stopped` timeline POST parks before it is answered.
        hold_stop: Arc<AtomicBool>,
        /// While set, the `stopped` timeline POST is answered 503 and the server keeps nothing.
        refuse_stop: Arc<AtomicBool>,
    }

    impl StopServer {
        fn new() -> Self {
            Self {
                stopped: Arc::new(AtomicBool::new(false)),
                hold_reads: Arc::new(AtomicBool::new(false)),
                hold_stop: Arc::new(AtomicBool::new(false)),
                refuse_stop: Arc::new(AtomicBool::new(false)),
            }
        }

        fn rig(&self, ends: bool) -> Option<Rig> {
            let (stopped, hold, hold_stop) = (self.stopped.clone(), self.hold_reads.clone(), self.hold_stop.clone());
            let refuse = self.refuse_stop.clone();
            Rig::serving_with(move |request: &str| {
                if request.contains("/decision?") {
                    return r#"{"MediaContainer":{"Metadata":[{"Media":[{"Part":[{"decision":"directplay"}]}]}]}}"#.into();
                }
                if request.contains("/:/timeline") {
                    if request.contains("state=stopped") {
                        for _ in 0..5_000 {
                            if !hold_stop.load(Ordering::SeqCst) { break; }
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        if refuse.load(Ordering::SeqCst) {
                            return "!503".into();
                        }
                        stopped.store(true, Ordering::SeqCst);
                    }
                    return r#"{"MediaContainer":{}}"#.into();
                }
                if request.contains("/:/scrobble") {
                    return "{}".into();
                }
                if request.starts_with("GET /library/metadata/") {
                    // Bounded by count, not by a clock: a failing test must not hang.
                    for _ in 0..5_000 {
                        if !hold.load(Ordering::SeqCst) { break; }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    let seen = stopped.load(Ordering::SeqCst);
                    let facts = match (seen, ends) {
                        (true, true) => r#","viewCount":1"#,
                        (true, false) => r#","viewOffset":150000"#,
                        (false, _) => "",
                    };
                    return format!(r#"{{"MediaContainer":{{"Metadata":[{{"ratingKey":"1","type":"movie","title":"First","duration":600000{facts}}}]}}}}"#);
                }
                queue_body(request, false).to_owned()
            })
        }
    }

    fn requests_matching(rig: &Rig, needle: &str, from: usize) -> usize {
        rig.requests.lock().unwrap().iter().skip(from).filter(|r| r.contains(needle)).count()
    }

    /// The Library page on top of a grid holding `items` unwatched items of the rig's server.
    fn library_with_grid(rig: &mut Rig, t: &mut u32, items: usize) {
        rig.release.send(()).unwrap(); // nothing parks: the first request answers at once
        rig.app.bridge.seed_library_grid_for_test(rig.sid, items);
        super::super::bridge::nav_select_tab(&mut rig.app.pages, AppArg::Library);
        settle(&mut rig.app, t);
        assert!(matches!(rig.app.route(), AppArg::Library), "premise: the Library is on top");
    }

    /// Play item 1 from the page on top (the fixture's first request is already released) and wait
    /// for the player to be mounted.
    fn play_from_here(rig: &mut Rig, t: &mut u32) {
        rig.request();
        rig.accept_start();
        poll_until("the plan did not land through update", || {
            let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
            fr.now = *t;
            unsafe { update(&mut rig.app, &mut fr); }
            rig.app.adapters.player.is_live()
        });
        settle(&mut rig.app, t);
        assert!(super::super::bridge::player(&rig.app.pages).is_some(), "premise: the player is on top");
    }

    /// **The plan's headline test.** Library -> play -> the stream reaches its end -> back on the
    /// Library: the grid tile reads watched, without the page being re-opened, and the origin
    /// server was not scrobbled a second time (the stop it reported made the view count).
    #[test]
    fn library_play_to_the_end_returns_with_the_tile_watched() {
        let _serial = plx_base::testlock::serial();
        let server = StopServer::new();
        let Some(mut rig) = server.rig(true) else { return };
        // A second source: with one server the end-of-stream fan-out has nowhere to go and is never
        // queued, which would leave the "no second scrobble" check below guarding nothing.
        plx_plex::plex::register_for_test("lifecycle-fixture-copy", "127.0.0.1", 9, "synthetic", "lifecycle-copy");
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        library_with_grid(&mut rig, &mut t, 1);
        assert_eq!(rig.app.bridge.grid_tile_for_test(0), Some((false, false, 0)), "premise: an untouched tile");
        let origin = rig.app.pages.nav.top_page().unwrap().id;
        play_from_here(&mut rig, &mut t);
        plx_media::player::SHARED.ended.store(true, Ordering::Relaxed);
        settle(&mut rig.app, &mut t);
        assert_eq!(rig.app.pages.nav.top_page().map(|e| e.id), Some(origin), "the end returned to the Library");
        // the rig has no engine, so teardown posts no `stopped`: the engine's own call, at the end
        plx_media::route::scrobble_stop(&mut rig.app.player.session, Some(("1".into(), 600_000, 600_000)), None);
        plx_media::route::drain_scrobble();
        poll_until("the grid tile never read watched", || {
            step_and_run_jobs(&mut rig, &mut t);
            rig.app.bridge.grid_tile_for_test(0) == Some((true, false, 0))
        });
        // The read-back that found the end queued the fan-out-only request (a second server is
        // registered, so there is a copy to reach); only once ViewState has run it is "no second
        // scrobble" a statement about it and not about the moment before it started.
        poll_until("ViewState never went idle", || {
            step(&mut rig.app, &mut t, vec![]);
            !rig.app.bridge.viewstate_busy_for_test()
        });
        assert_eq!(requests_matching(&rig, "/:/scrobble", 0), 0, "no second scrobble on the origin");
    }

    /// The Recommended page's rows ARE the server's hubs, and nothing else fetches them: a page
    /// mounted on `LibraryRecommended` (no shelf seeded by the test) must reach the server's
    /// `/hubs/sections/{id}`. The publish request it sends was once dropped because only the
    /// full grid's route was let through.
    #[test]
    fn the_recommended_page_fetches_the_sections_hubs() {
        let _serial = plx_base::testlock::serial();
        let server = StopServer::new();
        let Some(mut rig) = server.rig(true) else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        library_with_grid(&mut rig, &mut t, 1);
        assert_eq!(requests_matching(&rig, "/hubs/sections/", 0), 0, "premise: the grid page alone asks for no hubs");
        super::super::bridge::nav_push(&mut rig.app.pages, AppArg::LibraryRecommended { section: 0 });
        settle(&mut rig.app, &mut t);
        assert!(matches!(rig.app.route(), AppArg::LibraryRecommended { .. }), "premise: the Recommended page is on top");
        poll_until("the Recommended page never fetched the section's hubs", || {
            step(&mut rig.app, &mut t, vec![]);
            requests_matching(&rig, "/hubs/sections/", 0) > 0
        });
    }

    /// **The owner's path, through the whole application**: the Library's Collections page (its
    /// listing fetched from a server answering as PMS does), OK on a collection tile, OK on a member
    /// of the collection. Returns the app once the member's OK has been given, with the requests
    /// the server saw.
    fn collections_page_to_member_ok() -> Option<(Rig, u32)> {
        let rig = Rig::serving_with(|request: &str| {
            if request.contains("type=18") && request.contains("/library/sections/") {
                return r#"{"MediaContainer":{"size":1,"totalSize":1,"Metadata":[
                    {"ratingKey":"50001","key":"/library/collections/50001/children","type":"collection",
                     "subtype":"movie","title":"Set","titleSort":"Set","childCount":"3","smart":"0",
                     "librarySectionID":1,"librarySectionTitle":"Movies"}]}}"#.into();
            }
            if request.contains("/library/collections/50001/children") {
                return r#"{"MediaContainer":{"size":3,"totalSize":3,"Metadata":[
                    {"ratingKey":"7001","type":"movie","title":"First","year":2001,"librarySectionID":1},
                    {"ratingKey":"7002","type":"movie","title":"Second","year":2002,"librarySectionID":1},
                    {"ratingKey":"7003","type":"movie","title":"Third","year":2003,"librarySectionID":1}]}}"#.into();
            }
            if request.starts_with("GET /library/metadata/") || request.starts_with("GET /library/collections/") {
                return r#"{"MediaContainer":{"size":1,"Metadata":[{"ratingKey":"50001","type":"collection","title":"Set","childCount":"3"}]}}"#.into();
            }
            r#"{"MediaContainer":{"size":0}}"#.into()
        })?;
        let mut rig = rig;
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        rig.release.send(()).unwrap();
        rig.app.bridge.seed_registered_browse_for_test([rig.sid, rig.sid]);
        super::super::bridge::nav_select_tab(&mut rig.app.pages, AppArg::Library);
        settle(&mut rig.app, &mut t);
        super::super::bridge::nav_push(&mut rig.app.pages, AppArg::LibraryCollections { section: 0 });
        for _ in 0..120 { step(&mut rig.app, &mut t, vec![]); std::thread::sleep(Duration::from_millis(2)); }
        let page = rig.app.pages.nav.top_page().unwrap();
        assert!(page.arg == AppArg::LibraryCollections { section: 0 }, "premise: the Collections page is on top");
        let instance = rig.app.pages.nav.instance_of(page.id).unwrap();
        rig.app.pages.emit(plx_machine::machine::MachineId::Nav, plx_machine::machine::Fx::Deliver(
            plx_machine::machine::MachineId::Instance(instance),
            plx_machine::machine::Delivery::Screen(plx_ui::screen::ScreenEvent::App(
                plx_screens::registry::AppMsg::Library(plx_screens::registry::LibraryCmd::FocusGrid { row: 0, col: 0 })))));
        for _ in 0..40 { step(&mut rig.app, &mut t, vec![]); }
        assert!(rig.app.bridge.grid_tile_for_test(0).is_some(), "premise: the page lists the server's collection");
        press_ok(&mut rig.app, &mut t);
        // A test build parks the collection's requests until the test runs them, off the frame
        // (`StoreRunner::Held`): each frame's pump emits the next one, which runs before the next frame.
        for _ in 0..60 {
            rig.app.bridge.run_held_store_jobs_for_test();
            step(&mut rig.app, &mut t, vec![]);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(super::super::words::route_word(&rig.app.route()), "collection",
            "OK on the collection tile opens the collection");
        for _ in 0..40 {
            rig.app.bridge.run_held_store_jobs_for_test();
            step(&mut rig.app, &mut t, vec![]);
            std::thread::sleep(Duration::from_millis(2));
        }
        // Down off the header onto the first member, rest, then OK.
        let tick = |t: u32| plx_machine::machine::Tick { ms: t + 16, dt_us: 16_000 };
        for _ in 0..3 {
            if rig.app.bridge.collection_card_focused(&rig.app.pages) { break; }
            let down = plx_ui::fixture::key(plx_machine::machine::Key::Down, tick(t));
            step(&mut rig.app, &mut t, vec![down]);
            let up = plx_ui::fixture::key_up(plx_machine::machine::Key::Down, tick(t));
            step(&mut rig.app, &mut t, vec![up]);
            for _ in 0..40 {
                rig.app.bridge.run_held_store_jobs_for_test();
                step(&mut rig.app, &mut t, vec![]);
            }
        }
        assert!(rig.app.bridge.collection_card_focused(&rig.app.pages), "premise: a member holds the focus");
        press_ok(&mut rig.app, &mut t);
        for _ in 0..40 { step(&mut rig.app, &mut t, vec![]); }
        Some((rig, t))
    }

    fn press_ok(app: &mut App, t: &mut u32) {
        let tick = |t: u32| plx_machine::machine::Tick { ms: t + 16, dt_us: 16_000 };
        let down = plx_ui::fixture::key(plx_machine::machine::Key::Ok, tick(*t));
        step(app, t, vec![down]);
        for _ in 0..3 { step(app, t, vec![]); }
        let up = plx_ui::fixture::key_up(plx_machine::machine::Key::Ok, tick(*t));
        step(app, t, vec![up]);
    }

    #[test]
    fn a_member_ok_on_a_collection_opened_from_the_collections_page_opens_its_detail() {
        let _serial = plx_base::testlock::serial();
        let Some((mut rig, mut t)) = collections_page_to_member_ok() else { return };
        assert_eq!(super::super::words::route_word(&rig.app.route()), "detail",
            "OK on a member opens its detail page");
        // BACK walks the way it came: the collection, then the Collections page.
        for (word, listing) in [("collection", false), ("library", true)] {
            let tick = plx_machine::machine::Tick { ms: t + 16, dt_us: 16_000 };
            step(&mut rig.app, &mut t, vec![plx_ui::fixture::key(plx_machine::machine::Key::Back, tick)]);
            let tick = plx_machine::machine::Tick { ms: t + 16, dt_us: 16_000 };
            step(&mut rig.app, &mut t, vec![plx_ui::fixture::key_up(plx_machine::machine::Key::Back, tick)]);
            for _ in 0..60 { step(&mut rig.app, &mut t, vec![]); }
            assert_eq!(super::super::words::route_word(&rig.app.route()), word, "BACK returns to the {word} page");
            assert_eq!(matches!(rig.app.route(), AppArg::LibraryCollections { .. }), listing);
        }
    }

    /// **`/tmp/plxnative-libosc` on the Collections page** (`fps:library-collections`): the sweep is
    /// the library family's, so it moves the Collections page's scroll, and the focus fingerprint
    /// (`--arm focus`) reports that scroll as `viewport_y`. Both were gated on `AppArg::Library`
    /// alone: the TV run saw 3 focus lines in 80 s and a median of 1 fps against a floor of 30.
    #[test]
    fn the_library_sweep_moves_the_collections_page_and_the_fingerprint_reports_it() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving_with(|request: &str| {
            if request.contains("type=18") && request.contains("/library/sections/") {
                let tiles: Vec<String> = (0..60).map(|i| format!(
                    r#"{{"ratingKey":"{}","key":"/library/collections/{}/children","type":"collection","subtype":"movie","title":"Set {i}","titleSort":"Set {i}","childCount":"3","smart":"0","librarySectionID":1,"librarySectionTitle":"Movies"}}"#,
                    50000 + i, 50000 + i)).collect();
                return format!(r#"{{"MediaContainer":{{"size":60,"totalSize":60,"Metadata":[{}]}}}}"#, tiles.join(","));
            }
            r#"{"MediaContainer":{"size":0}}"#.into()
        }) else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        rig.release.send(()).unwrap();
        rig.app.bridge.seed_registered_browse_for_test([rig.sid, rig.sid]);
        super::super::bridge::nav_select_tab(&mut rig.app.pages, AppArg::Library);
        settle(&mut rig.app, &mut t);
        super::super::bridge::nav_push(&mut rig.app.pages, AppArg::LibraryCollections { section: 0 });
        for _ in 0..120 { step(&mut rig.app, &mut t, vec![]); std::thread::sleep(Duration::from_millis(2)); }
        assert!(matches!(rig.app.route(), AppArg::LibraryCollections { .. }), "premise: the Collections page is on top");
        let page = rig.app.pages.nav.top_page().unwrap();
        let instance = rig.app.pages.nav.instance_of(page.id).unwrap();
        rig.app.pages.emit(plx_machine::machine::MachineId::Nav, plx_machine::machine::Fx::Deliver(
            plx_machine::machine::MachineId::Instance(instance),
            plx_machine::machine::Delivery::Screen(plx_ui::screen::ScreenEvent::App(
                plx_screens::registry::AppMsg::Library(plx_screens::registry::LibraryCmd::FocusGrid { row: 0, col: 0 })))));
        for _ in 0..40 { step(&mut rig.app, &mut t, vec![]); }
        assert!(rig.app.bridge.grid_tile_for_test(0).is_some(), "premise: the page lists the server's collections");
        let scroll = |app: &App| app.pages.nav.top_page().and_then(|page| page.inst.as_ref())
            .and_then(|inst| inst.screen.as_any()).and_then(|any| any.downcast_ref::<plx_screens::library::LibraryScreen>())
            .expect("the Collections page is a LibraryScreen").probe_scroll_target();
        let viewport_y = |app: &App| {
            let line = super::super::bridge::content_probe(&app.pages, &app.bridge);
            line.split("viewport_y=").nth(1).and_then(|rest| rest.split_whitespace().next()).and_then(|v| v.parse::<f32>().ok())
        };
        assert_eq!(scroll(&rig.app), 0.0, "premise: the page rests at its top");
        assert!(viewport_y(&rig.app).is_some(), "the fingerprint reports the Collections page's viewport");
        rig.app.scenarios.dev.lib_osc = true;
        for _ in 0..900 {
            step(&mut rig.app, &mut t, vec![]);
            if scroll(&rig.app) > 0.5 { break; }
        }
        assert!(scroll(&rig.app) > 0.5, "the sweep moved the Collections page's scroll, now {}", scroll(&rig.app));
        for _ in 0..30 { step(&mut rig.app, &mut t, vec![]); }
        assert!(viewport_y(&rig.app).is_some_and(|y| y > 0.0),
            "the fingerprint reports the scroll the sweep made: {:?}", viewport_y(&rig.app));
    }

    /// A stop in the middle: the tile shows the new resume point on the frame the stop lands, with
    /// the server's answer still parked, and the answer then agrees with it.
    #[test]
    fn a_stop_in_the_middle_shows_its_resume_point_before_the_server_answers() {
        let _serial = plx_base::testlock::serial();
        let server = StopServer::new();
        let Some(mut rig) = server.rig(false) else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        library_with_grid(&mut rig, &mut t, 1);
        play_from_here(&mut rig, &mut t);
        server.hold_reads.store(true, Ordering::SeqCst); // the play's own reads are done
        let back = back_key(t + 16);
        step(&mut rig.app, &mut t, vec![back]);
        plx_media::route::scrobble_stop(&mut rig.app.player.session, Some(("1".into(), 150_000, 600_000)), None);
        plx_media::route::drain_scrobble();
        poll_until("the tile never showed the resume point", || {
            step(&mut rig.app, &mut t, vec![]);
            rig.app.bridge.grid_tile_for_test(0).is_some_and(|tile| tile.2 == 150_000)
        });
        server.hold_reads.store(false, Ordering::SeqCst);
        for _ in 0..8 { step(&mut rig.app, &mut t, vec![]); }
        assert_eq!(rig.app.bridge.grid_tile_for_test(0).map(|tile| tile.2), Some(150_000));
    }

    /// **A stop never outlives the identity it was made under.** A `stopped` POST stalled on a slow
    /// server returns after the user switched profile; the new identity must get no read-back (a
    /// request on its token) from the old one's stop.
    #[test]
    fn a_stop_that_lands_after_an_identity_change_reads_nothing_back() {
        let _serial = plx_base::testlock::serial();
        let server = StopServer::new();
        let Some(mut rig) = server.rig(false) else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        library_with_grid(&mut rig, &mut t, 1);
        play_from_here(&mut rig, &mut t);
        let back = back_key(t + 16);
        step(&mut rig.app, &mut t, vec![back]);
        let from = rig.requests.lock().unwrap().len();
        server.hold_stop.store(true, Ordering::SeqCst);
        plx_media::route::scrobble_stop(&mut rig.app.player.session, Some(("1".into(), 150_000, 600_000)), None);
        poll_until("premise: the stopped POST never reached the server", || {
            requests_matching(&rig, "state=stopped", from) == 1
        });
        let _ = super::super::boot::activate_server_owned(&mut rig.app.bridge); // the identity changed
        server.hold_stop.store(false, Ordering::SeqCst);
        plx_media::route::drain_scrobble();
        assert!(!plx_media::route::stops_owed(), "the retired stop is no longer owed");
        // before any frame: the mailbox itself, which the app would otherwise drain into the new stores
        assert!(plx_media::route::take_landed_stops().is_empty(), "the old identity's stop reached the mailbox");
        // the read-back is a `StoreJob`: the rig's `Held` runner parks it, and only the sweep runs it
        for _ in 0..12 { step_and_run_jobs(&mut rig, &mut t); }
        assert_eq!(requests_matching(&rig, "GET /library/metadata/", from), 0, "the old identity's stop was read back");
    }

    /// **A stop the server refused still reaches the page it returns to** (a #575 regression the
    /// read-back alone cannot fix: a refused stop asks for none). The Detail page shows the
    /// position the viewer reached the frame the stop lands, with its own re-read still out, and
    /// that re-read, asked for by the stop itself, then puts the server's word back (nothing was
    /// stored, so the resume point is the one the server holds).
    #[test]
    fn a_refused_stop_patches_the_detail_page_and_the_page_then_re_reads_itself() {
        let _serial = plx_base::testlock::serial();
        let server = StopServer::new();
        let Some(mut rig) = server.rig(false) else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        rig.release.send(()).unwrap(); // nothing parks: the first request answers at once
        super::super::bridge::open_detail(&mut rig.app.pages, &mut rig.app.bridge, rig.sid, "1", None, None);
        settle(&mut rig.app, &mut t);
        land_detail_reads(&mut rig, &mut t);
        let resume = |rig: &Rig| rig.app.bridge.metadata_view().current().map(|d| (d.rk.clone(), d.resume_ms));
        assert_eq!(resume(&rig), Some(("1".into(), 0)), "premise: the Detail page is loaded, unplayed");
        play_from_here(&mut rig, &mut t);
        server.refuse_stop.store(true, Ordering::SeqCst);
        let back = back_key(t + 16);
        step(&mut rig.app, &mut t, vec![back]);
        settle(&mut rig.app, &mut t);
        assert!(rig.app.route().same_instance(&detail_arg(rig.sid)), "premise: Back returned to the Detail page");
        // Back re-requests the page it uncovers; that read is out (held) when the stop lands
        let asked = plx_data::metadata::detail_generation_for_test(rig.app.bridge.metadata_mut().adapter_ref());
        plx_media::route::scrobble_stop(&mut rig.app.player.session, Some(("1".into(), 150_000, 600_000)), None);
        plx_media::route::drain_scrobble();
        poll_until("the Detail page never showed the position the viewer reached", || {
            step(&mut rig.app, &mut t, vec![]);
            resume(&rig) == Some(("1".into(), 150_000))
        });
        assert!(!server.stopped.load(Ordering::SeqCst), "premise: the server kept nothing");
        assert!(plx_data::metadata::detail_generation_for_test(rig.app.bridge.metadata_mut().adapter_ref()) > asked,
            "the refused stop asked the page for a re-read of its own");
        let from = rig.requests.lock().unwrap().len();
        land_detail_reads(&mut rig, &mut t);
        assert!(requests_matching(&rig, "GET /library/metadata/1", from) >= 1, "the page read the server");
        assert_eq!(resume(&rig), Some(("1".into(), 0)), "and settled on the server's word");
    }

    /// Network reconcile work is held while the player is on top: a mark-watched that lands under
    /// the player owes a read-back and a hubs refetch, and neither goes out until the player leaves.
    #[test]
    fn no_reconcile_read_goes_out_while_the_player_is_on_top() {
        let _serial = plx_base::testlock::serial();
        let server = StopServer::new();
        let Some(mut rig) = server.rig(false) else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        library_with_grid(&mut rig, &mut t, 1);
        play_from_here(&mut rig, &mut t);
        let (reads_from, refetches) = (rig.requests.lock().unwrap().len(), rig.app.bridge.hub_refetches_for_test());
        assert!(rig.app.bridge.viewstate_run(plx_data::stores::viewstate::ViewStateCmd::Request {
            sid: rig.sid, rk: "1".into(), write: plx_data::viewstate::Write::Watched,
            scope: plx_data::viewstate::Scope::Leaf, guid: String::new(),
        }).changed);
        poll_until("the write never reached the server", || {
            step_and_run_jobs(&mut rig, &mut t);
            requests_matching(&rig, "/:/scrobble", reads_from) == 1
        });
        // The held jobs run in these frames too: a read-back sent under the player would reach the
        // server here, so the count below is a statement about what the store sent.
        for _ in 0..12 { step_and_run_jobs(&mut rig, &mut t); }
        assert!(super::super::bridge::player(&rig.app.pages).is_some(), "premise: still under the player");
        assert_eq!(requests_matching(&rig, "GET /library/metadata/", reads_from), 0, "a read-back went out under the player");
        assert_eq!(rig.app.bridge.hub_refetches_for_test(), refetches, "a hubs refetch started under the player");
        let back = back_key(t + 16);
        step(&mut rig.app, &mut t, vec![back]);
        poll_until("the read-back never went out after the player left", || {
            step_and_run_jobs(&mut rig, &mut t);
            requests_matching(&rig, "GET /library/metadata/", reads_from) >= 1
        });
        for _ in 0..12 { step_and_run_jobs(&mut rig, &mut t); }
        assert_eq!(requests_matching(&rig, "GET /library/metadata/", reads_from), 1, "one batch, at the exit");
    }

    /// An Up Next chain stops one episode after another with no frame between them (the previous
    /// worker may be stalled, and the main thread joins it before the next stop): every episode
    /// still gets its own patch.
    #[test]
    fn two_stops_with_no_frame_between_them_each_patch_their_own_tile() {
        let _serial = plx_base::testlock::serial();
        let server = StopServer::new();
        let Some(mut rig) = server.rig(false) else { return };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        library_with_grid(&mut rig, &mut t, 2);
        play_from_here(&mut rig, &mut t);
        server.hold_reads.store(true, Ordering::SeqCst); // the play's own reads are done
        plx_media::route::scrobble_stop(&mut rig.app.player.session, Some(("1".into(), 30_000, 60_000)), None);
        plx_media::route::scrobble_stop(&mut rig.app.player.session, Some(("2".into(), 20_000, 60_000)), None);
        plx_media::route::drain_scrobble();
        step(&mut rig.app, &mut t, vec![]);
        let resumes = |rig: &Rig| [0, 1].map(|i| rig.app.bridge.grid_tile_for_test(i).map(|tile| tile.2));
        assert_eq!(resumes(&rig), [Some(30_000), Some(20_000)], "an episode's stop was lost");
        server.hold_reads.store(false, Ordering::SeqCst);
    }

    /// The same session drained to its end with nothing queued after it: `finish_playback` leaves
    /// through `exit_player`, so an end of stream lands where BACK and Stop do — the page the
    /// session was launched from (§5.1), not Home.
    #[test]
    fn an_end_of_stream_without_up_next_returns_to_the_detail_page() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else {
            eprintln!("SKIPPED an_end_of_stream_without_up_next_returns_to_the_detail_page: \
                lifecycle fixture worker thread could not be spawned");
            return;
        };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        super::super::bridge::open_detail(&mut rig.app.pages, &mut rig.app.bridge, rig.sid, "1", None, None);
        settle(&mut rig.app, &mut t);
        let origin = play_with_a_landing_inside_the_dip(&mut rig, &mut t);
        assert!(plx_media::route::up_next(&rig.app.player.session).is_none(), "a film: nothing queued");
        plx_media::player::SHARED.ended.store(true, std::sync::atomic::Ordering::Relaxed);
        settle(&mut rig.app, &mut t);
        assert!(
            rig.app.route().same_instance(&detail_arg(rig.sid)),
            "end of stream landed on {:?}, not the detail page",
            rig.app.route().id(),
        );
        assert_eq!(rig.app.pages.nav.top_page().map(|e| e.id), Some(origin));
    }

    /// A session started straight from Home (the hero's Play, or a deck tile in Play mode) has no
    /// detail page in between, so the page the session returns to is Home itself — the same entry,
    /// with its memory.
    #[test]
    fn back_from_a_session_started_on_home_returns_to_home() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(false) else {
            eprintln!("SKIPPED back_from_a_session_started_on_home_returns_to_home: \
                lifecycle fixture worker thread could not be spawned");
            return;
        };
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        let origin = play_with_a_landing_inside_the_dip(&mut rig, &mut t);
        let back = back_key(t + 16);
        step(&mut rig.app, &mut t, vec![back]);
        settle_after_back(&mut rig.app, &mut t);
        assert!(matches!(rig.app.route(), AppArg::Home));
        assert_eq!(rig.app.pages.nav.top_page().map(|e| e.id), Some(origin));
    }

    /// Puts the Next episode preference back whatever the test did.
    struct NextEpisodeGuard(plx_media::route::NextEpisodeMode);
    impl NextEpisodeGuard {
        fn set(mode: plx_media::route::NextEpisodeMode) -> Self {
            let prior = plx_media::route::next_episode_mode();
            plx_media::route::restore_next_episode_mode(mode);
            Self(prior)
        }
    }
    impl Drop for NextEpisodeGuard {
        fn drop(&mut self) {
            plx_media::route::restore_next_episode_mode(self.0);
        }
    }

    /// A resolved, mounted Player whose playhead sits inside a `final` credits marker, so the REAL
    /// `player_hud::slot` (via `Frame::begin`) decides the control row instead of a slot the test
    /// built itself. Returns the frame clock after one settled frame.
    fn playing_inside_final_credits(rig: &mut Rig) -> u32 {
        rig.request();
        rig.resolve();
        super::super::bridge::nav_push(&mut rig.app.pages, AppArg::Player);
        frame(&mut rig.app, 16);
        plx_data::metadata::set_playing_markers_for_test(
            rig.app.bridge.metadata_mut().state_mut(),
            vec![plx_data::metadata::Marker {
                kind: plx_data::metadata::MarkerKind::Credits,
                start_ms: 0,
                end_ms: 60_000,
                final_seg: true,
            }],
        );
        plx_media::player::restore_state_for_test(plx_media::player::PlaybackState::Playing as u8);
        plx_media::player::SHARED
            .playpos_ns
            .store(30_000_000_000, std::sync::atomic::Ordering::Relaxed);
        let mut t = 16;
        // The slot reaches the screen in `update`, which runs after the frame that ticks the
        // countdown — so it takes two frames for the countdown to see it.
        step(&mut rig.app, &mut t, vec![]);
        step(&mut rig.app, &mut t, vec![]);
        t
    }

    /// **Next episode: After credits.** The credits of an episode with a successor queued arm no
    /// Up Next countdown, so nothing is requested however long they run; the end of the stream
    /// then plays the successor exactly as it always has.
    #[test]
    fn after_credits_requests_no_successor_until_the_stream_ends() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::new() else {
            eprintln!("SKIPPED after_credits_requests_no_successor_until_the_stream_ends: \
                lifecycle fixture worker thread could not be spawned");
            return;
        };
        let _mode = NextEpisodeGuard::set(plx_media::route::NextEpisodeMode::AfterCredits);
        let mut t = playing_inside_final_credits(&mut rig);
        let fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
        assert!(fr.ctrl.is_discs(), "inside the credits the row keeps the discs");
        t += plx_appkit::up_next::COUNTDOWN_MS;
        step(&mut rig.app, &mut t, vec![]);
        let player = super::super::bridge::player(&rig.app.pages).unwrap();
        assert!(!player.up_next.expired(t), "no countdown was armed for the credits");
        assert!(!plx_media::route::play_pending(), "the credits alone must not request the successor");
        assert_eq!(plx_media::route::up_next(&rig.app.player.session).unwrap().rk, "2");

        plx_media::player::SHARED.ended.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
        fr.now = t;
        unsafe { playback_tick(&mut rig.app, &mut fr); }
        assert!(plx_media::route::play_pending(), "the end of the stream still plays the next episode");
    }

    /// **Next episode: Countdown** on the same rig as the test above, so the two differ only in
    /// the preference: the credits put the Up Next tile up and its countdown requests the
    /// successor before the stream ends.
    #[test]
    fn countdown_requests_the_successor_from_inside_the_credits() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::new() else {
            eprintln!("SKIPPED countdown_requests_the_successor_from_inside_the_credits: \
                lifecycle fixture worker thread could not be spawned");
            return;
        };
        let _mode = NextEpisodeGuard::set(plx_media::route::NextEpisodeMode::Countdown);
        let mut t = playing_inside_final_credits(&mut rig);
        let fr = Frame::begin(&rig.app.player.session, rig.app.bridge.metadata_view());
        assert!(
            matches!(fr.ctrl, plx_appkit::player_hud::ControlSlot::UpNext(_)),
            "inside the credits the tile takes over",
        );
        assert!(!plx_media::route::play_pending(), "the countdown is still running");
        t += plx_appkit::up_next::COUNTDOWN_MS;
        step(&mut rig.app, &mut t, vec![]);
        assert!(plx_media::route::play_pending(), "the countdown requests the successor");
    }

    /// **Next episode: Off.** The end of an episode that HAS a successor leaves the player like a
    /// film does: back to the page the session was launched from, nothing requested.
    #[test]
    fn off_returns_to_the_detail_page_at_the_end_of_an_episode_with_a_successor() {
        let _serial = plx_base::testlock::serial();
        let Some(mut rig) = Rig::serving(true) else {
            eprintln!("SKIPPED off_returns_to_the_detail_page_at_the_end_of_an_episode_with_a_successor: \
                lifecycle fixture worker thread could not be spawned");
            return;
        };
        let _mode = NextEpisodeGuard::set(plx_media::route::NextEpisodeMode::Off);
        let mut t = product_transition(&mut rig.app);
        settle(&mut rig.app, &mut t);
        super::super::bridge::open_detail(&mut rig.app.pages, &mut rig.app.bridge, rig.sid, "1", None, None);
        settle(&mut rig.app, &mut t);
        let origin = play_with_a_landing_inside_the_dip(&mut rig, &mut t);
        assert!(plx_media::route::up_next(&rig.app.player.session).is_some(), "premise: a successor is queued");
        plx_media::player::SHARED.ended.store(true, std::sync::atomic::Ordering::Relaxed);
        settle(&mut rig.app, &mut t);
        assert!(!plx_media::route::play_pending(), "Off must not request the successor");
        assert!(
            rig.app.route().same_instance(&detail_arg(rig.sid)),
            "end of stream landed on {:?}, not the detail page",
            rig.app.route().id(),
        );
        assert_eq!(rig.app.pages.nav.top_page().map(|e| e.id), Some(origin));
    }
}

#[cfg(test)]
mod video_plane_gate_tests {
    //! **The present gate's plane term, end to end.** It began in `plx_machine::idle`'s tests and lives here
    //! because it needs three things no lower layer owns together: a `Player` (the bit's one
    //! writer), the gate (`plx_machine::idle`, the machine layer) and this file's own source (the loop is
    //! the one consumer a unit test cannot drive). The gate-only half stayed beside the gate:
    //! `plx_machine::idle`'s `the_present_gate_answers_true_only_while_the_plane_bit_is_set`.
    use plx_machine::idle::{
        invalidate, reset_for_test, should_present, take_local_damage, video_plane_bound,
    };

    /// **Spec §9, §4.4, §16 risk 10 — the whole-frame gate is turned off by the PLANE'S BIT, not
    /// by the player route, and both of its edges are carried.**
    ///
    /// Three separate claims, because three separate things were wrong before phase 9.
    ///
    /// 1. *Only while bound.* The term used to be `|| fr.player` at the call site, i.e. "the
    ///    player SCREEN is up" — true through the whole pre-bind spinner and the whole post-unbind
    ///    fade, when the compositor has an ordinary UI surface and nothing is slaved to it.
    /// 2. *The false edge presents.* The frame the plane goes away on is very often one the gate
    ///    would otherwise skip: the picture is gone and no spring is moving. If that frame is not
    ///    presented, the surface keeps whatever the last video frame left and the opaque region is
    ///    asserted for a plane that is no longer there (§3.3 step 9).
    /// 3. *`opaque_route` is asked on every frame, from the bit.* Pinned from the loop's own source
    ///    — this is the one consumer a unit test cannot drive, `run` needing a live SDL window.
    ///
    /// Observed RED (simulated — the fix changes the signatures the old code called, so the test
    /// cannot be compiled against 88841d3e): restoring `idle::should_present`'s pre-phase-9 body by
    /// deleting the `|| VIDEO_PLANE.load(Relaxed)` term fails claim 1 at
    /// "while the plane is bound every frame presents"; deleting the `!bound` `invalidate()` in
    /// `Player::set_video_plane_bound` fails claim 2 at "the unbind frame presents"; and putting
    /// `fr.player` back as `opaque_route`'s argument fails claim 3.
    #[test]
    fn the_present_gate_answers_true_only_while_the_plane_is_bound() {
        let _g = plx_base::testlock::serial();
        reset_for_test();
        let mut player = plx_media::player::machine::Player::new();
        assert!(!video_plane_bound(), "a fresh machine has no plane");

        invalidate();
        assert!(should_present(0), "the damage just raised selects this frame");
        assert!(!should_present(16), "settled, inside the keepalive: nothing to send");

        // ---- the TRUE edge, and what it buys ----
        player.set_video_plane_bound(true);
        assert!(video_plane_bound(), "the machine's edge is the gate's only input");
        for t in [32u32, 48, 64, 80] {
            assert!(
                should_present(t),
                "while the plane is bound every frame presents, unconditionally — nothing about                  this frame moved",
            );
        }

        // A LEVEL is not an edge. Writing the same value again must publish nothing: a second
        // formula term feeding the gate is exactly what risk 10 names.
        let _ = take_local_damage();
        player.set_video_plane_bound(true);
        assert_eq!(
            take_local_damage(),
            0,
            "re-asserting the same bit raised damage — the bit is published on EDGES only",
        );

        // ---- the FALSE edge, on a frame that would otherwise not present ----
        // Nothing else has happened: no input, no landing, no spring, and the keepalive is not due
        // (LAST_PRESENT is 0 and KEEPALIVE_MS is 2000). Without the edge's own report this frame
        // is skipped, and the last video frame stays on the panel behind a stale opaque region.
        player.set_video_plane_bound(false);
        assert!(!video_plane_bound());
        assert!(
            should_present(96),
            "the unbind frame presents even though nothing else about it moved",
        );
        assert!(
            !should_present(112),
            "…and the frame after it is an ordinary idle frame again, which is the whole point",
        );

        // ---- claim 3: the loop asks the compositor on EVERY frame, from the bit ----
        let whole = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/run.rs"),
        )
        .expect("read run.rs");
        // The loop's own text — everything above this module — so the strings this pin searches
        // for are not found in the pin itself.
        let src = whole
            .split("#[cfg(test)]\nmod video_plane_gate_tests")
            .next()
            .expect("the loop's source");
        assert!(
            !src.contains("plx_platform::tv::window::opaque_route(fr.player)"),
            "the opaque region must not be keyed on the ROUTE — the plane's bit is the question",
        );
        // **UNCONDITIONAL.** The claim this pins is not where the call sits relative to the
        // present decision — spec §3.3 step 9 puts it AFTER, and since phase 11 so does the loop,
        // because the render cache's upload step now runs on the PRESENTING side of that decision
        // and this call has to follow it. The claim is that the call is never nested inside an
        // `if fr.present` block: the false edge after an unbind may land on a frame the gate does
        // not present, and nothing else in the loop would carry it. This assertion used to be
        // `call < gate`, which was a proxy for that and stopped being one when the upload moved.
        //
        // D1 extracted the prepare window into `prepare_window(app, fr)` to get `run` under its
        // 200-line budget, so "the loop body's own depth" is now two claims: the call is at
        // `prepare_window`'s own body depth (four spaces, never inside that function's one
        // `if fr.present`), and `prepare_window` itself is called at the loop body's (eight).
        const CALL: &str = "    plx_platform::tv::window::opaque_route(app.player.video_plane_bound);";
        assert_eq!(
            src.lines().filter(|l| *l == CALL).count(),
            1,
            "`opaque_route` must be called exactly once, unnested — inside `if fr.present` it is \
             lost on exactly the frames it matters on",
        );
        assert!(
            src.lines().any(|l| l == "        prepare_window(app, fr);"),
            "…and the window that holds it runs on every iteration, at the loop body's own depth",
        );
        let call = src.find(CALL.trim_start()).expect("the call");
        let draw = src
            .find("let (_vx, _vy, _vw, _vh) = draw(app, fr);")
            .expect("the loop's draw");
        assert!(call < draw, "the compositor is told before the frame is drawn");
    }
}

#[cfg(test)]
mod pump_wiring_tests {
    //! `app/run.rs`'s route-unconditional per-frame block has no host fixture — `App` needs a real
    //! SDL/GL context — so this pins the source text itself, the same idiom
    //! `app::words::focusprobe_player_overlay_delegates_to_the_shared_overlay_word_function` uses
    //! for the identical reason. Why `pump_season()` needs this pin at all: see
    //! `plx_data::stores::metadata`'s module doc, and the commit that added this test.
    /// **A frame's modal-dim count is cleared at the head of EVERY iteration**, on the player path
    /// and the preview-bound page too: neither calls `popover::host::begin_frame` (a hardware
    /// plane has no host to snapshot), so a count left by the last popover frame survived into the
    /// first one after the player. `App` has no host fixture, so this pins the source text: one
    /// whole-line call at the loop body's depth, before the frame's draw.
    #[test]
    fn the_frame_head_clears_the_modal_dims_on_every_route() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/run.rs"),
        )
        .expect("read run.rs");
        const CALL: &str = "        plx_ui::popover::host::clear_dims();";
        assert_eq!(
            src.lines().filter(|l| *l == CALL).count(),
            1,
            "`clear_dims` must be called exactly once, at the loop body's own depth — unnested, so \
             no route skips it",
        );
        let call = src.find(CALL.trim_start()).expect("the call");
        let draw = src.find("let (_vx, _vy, _vw, _vh) = draw(app, fr);").expect("the loop's draw");
        assert!(call < draw, "the count is cleared before the frame is drawn");
    }

    #[test]
    fn the_route_unconditional_frame_update_pumps_the_season_landing_after_detail() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/run.rs"),
        )
        .expect("read run.rs");
        // The exact statement shape `pump_detail()`'s own call already has, indentation included:
        // a bare `.contains("pump_season()")` would still pass if the call were wrapped in a
        // route/feature gate, buried behind a route check, or merely mentioned in a comment (none
        // of which pump the mailbox route-unconditionally). Matching by WHOLE LINE (not a
        // substring search, which extra leading indentation would still satisfy) against the
        // literal `if app.bridge.metadata_pump_season() {` at the SAME indentation as
        // `metadata_pump_detail()`'s own `if` proves it is a sibling statement in the same block
        // — not nested one level deeper inside some other conditional.
        //
        // Stage C1 note: these two now name `Bridge`'s own owned-store wrappers
        // (`metadata_pump_detail`/`metadata_pump_season`), not the old free
        // `plx_data::stores::metadata::pump_detail`/`pump_season` — Metadata moved from a
        // crate-global dispatcher to a per-`Bridge` owner in Stage B, and `run.rs`'s call site
        // moved with it. The pin exists to catch exactly that kind of silent drop, so it must
        // track the real call shape rather than the pre-ownership one.
        const DETAIL_STMT: &str = "        if app.bridge.metadata_pump_detail() {";
        const SEASON_STMT: &str = "        if app.bridge.metadata_pump_season() {";
        let line_index = |needle: &str| {
            src.lines().position(|line| line == needle)
        };
        let detail_at = line_index(DETAIL_STMT)
            .expect("run.rs must still pump the async detail landing every frame, at this exact indentation");
        let season_at = line_index(SEASON_STMT).unwrap_or_else(|| {
            panic!(
                "metadata_pump_season() must be called route-unconditionally, at the same nesting \
                 depth as metadata_pump_detail() (found no `{SEASON_STMT}` line) — its call site \
                 went missing in the phase-7 owned-screens migration and nothing replaced it, so \
                 season_loading() never clears after a season switch: the episode row's spinner \
                 spins forever and every episode press is refused (episodes::action gates on \
                 season_loading())."
            )
        });
        // metadata_pump_detail() must run FIRST: a landed detail's `install_landed_detail` calls
        // `supersede_season()`, invalidating any season fetch for the item being replaced.
        // Pumping season first could apply a stale season landing to CURRENT in the one frame
        // before metadata_pump_detail() replaces it.
        assert!(
            season_at > detail_at,
            "metadata_pump_season() must be pumped AFTER metadata_pump_detail(), not before — \
             metadata_pump_detail() is what supersedes a stale in-flight season fetch when a \
             fresh detail lands"
        );
        // Both statements must be in the SAME enclosing function: no line starting a new `fn` —
        // a new function's own leading `fn`, not the word appearing mid-identifier — between them.
        let no_intervening_fn = src.lines().skip(detail_at + 1).take(season_at - detail_at - 1)
            .all(|line| !line.trim_start().starts_with("fn ")
                && !line.trim_start().starts_with("pub"));
        assert!(
            no_intervening_fn,
            "pump_detail() and pump_season() must be pumped from the same function — found what \
             looks like an intervening function boundary between them"
        );
    }
}

#[cfg(test)]
mod wake_input_tests {
    use super::{wake_input, ForegroundClock, ForegroundInput, WakePress};

    /// **`wake_input`'s doc table, cell for cell.** Only a press that asks to play claims with
    /// `PlayKey`, and a toggle asks to play only over a paused save; with nothing claimable (no
    /// saved clock) only PLAY still asks.
    #[test]
    fn only_a_press_that_asks_to_play_claims_playing() {
        use ForegroundClock::{Paused, Playing};
        use ForegroundInput::{DidForeground, PlayKey};
        for (press, saved, want) in [
            (WakePress::Play, Some(Paused), PlayKey),
            (WakePress::Play, Some(Playing), PlayKey),
            (WakePress::Play, None, PlayKey),
            (WakePress::Toggle, Some(Paused), PlayKey),
            (WakePress::Toggle, Some(Playing), DidForeground),
            (WakePress::Toggle, None, DidForeground),
            (WakePress::Wake, Some(Paused), DidForeground),
            (WakePress::Wake, Some(Playing), DidForeground),
            (WakePress::Wake, None, DidForeground),
        ] {
            assert_eq!(wake_input(press, saved), want, "{press:?} over {saved:?}");
        }
    }
}
