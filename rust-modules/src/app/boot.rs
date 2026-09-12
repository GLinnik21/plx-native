//! Boot-time helpers of the app core: the desktop window size, the panic logger, the replay
//! budget, the direct-server trigger, the cursor and the loop-rate tick. Moved out of `app.rs`
//! verbatim in phase 1a of the UI restructure (a pure move; `pub(crate)` widening only).

use super::*;

/// The window size a DESKTOP should open at, in points: the authored 1920x1080 canvas divided by
/// the smallest whole number that fits the usable display area.
///
/// **An exact divisor, never a best fit.** `surface::scale` will letterbox any drawable it is
/// given, so an arbitrary size would *work* — it would just be soft, because every glyph and icon
/// mask in this app is rasterized for a 1:1 surface (`gfx::snap`, and the crispness contract in
/// `theme.rs`) and a fractional scale resamples all of it. 1/1, 1/2 and 1/3 keep whole texels whole.
///
/// The television is untouched by any of this: it takes the panel, and the canvas IS the surface.
/// A Mac is the case the `surface` doc was written against — 1920x1080 exceeds the usable area of
/// every laptop display Apple ships, so asking for it flatly would put the title bar above the
/// screen and the bottom of the interface under the Dock.
///
/// Falls back to the canvas size if SDL cannot answer, which is the behaviour this replaced.
#[cfg(feature = "hostsim")]
pub(crate) fn desktop_window_size() -> (c_int, c_int) {
    // `PLXNATIVE_WIN=<w>x<h>` overrides the fit entirely — `make sim-shot SIM_W=1920 SIM_H=1080`.
    // It exists because the fit below is chosen for a HUMAN looking at a window, and a screenshot
    // is not that: on a 1x display the divisor lands on 2 and every shot comes back 960x540, which
    // is half the canvas the UI is authored at. A hairline, a 1px edge-sheen and a snapped glyph
    // are exactly the things that do not survive that, so a shot taken to JUDGE the interface has
    // to be asked for at full size. Off-screen edges are fine for a headless grab: the drawable is
    // the window's own framebuffer, not the part of it the compositor happens to show.
    if let Some(v) = std::env::var_os("PLXNATIVE_WIN") {
        let v = v.to_string_lossy().to_lowercase();
        if let Some((w, h)) = v.split_once('x') {
            if let (Ok(w), Ok(h)) = (w.trim().parse::<c_int>(), h.trim().parse::<c_int>()) {
                if w > 0 && h > 0 {
                    return (w, h);
                }
            }
        }
    }
    let mut r = [0 as c_int; 4]; // SDL_Rect: x, y, w, h
    let ok = unsafe { SDL_GetDisplayUsableBounds(0, r.as_mut_ptr()) } == 0;
    let (uw, uh) = (r[2], r[3]);
    if !ok || uw <= 0 || uh <= 0 {
        return (SCR_W, SCR_H);
    }
    // A little headroom under the usable bounds: a window flush against them reads as a fullscreen
    // that went wrong rather than as a deliberate size.
    for div in 1..=3 {
        let (w, h) = (SCR_W / div, SCR_H / div);
        if w <= (uw as f32 * 0.95) as c_int && h <= (uh as f32 * 0.95) as c_int {
            return (w, h);
        }
    }
    (SCR_W / 3, SCR_H / 3)
}

/// Log every Rust panic (message + source location + thread) to the event log AND the
/// persistent crash log BEFORE it unwinds. A panic that crosses an extern "C" boundary
/// (e.g. libav calling ff::read_cb/seek_cb) aborts the process (SIGABRT) — by then the
/// message is gone, so capturing it here is the only way to see WHAT panicked. Pairs with
/// `src/crashtrace.c` — not `main.c`, which the tracer left in 2026-08-29 — whose re-raise buys SAM
/// a real `WIFSIGNALED` status and **nothing else**: `core_pattern` on this firmware is the bare
/// string `core` and `RLIMIT_CORE` is 0, so no core is written and no crashd report is ever
/// generated. `crashtrace.c` says so itself. Two deliberate SIGSEGVs produced the signal status and
/// an empty `/var/log/reports/librdx/`.
///
/// The line this hook writes is also the crash channel's PANIC input: `telemetry::crashreport`
/// reads the log on the next launch, hashes the message and sends the location only.
pub(crate) fn install_panic_logger() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "?".into());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic>".into());
        let cur = std::thread::current();
        let thread = cur.name().unwrap_or("?");
        let line = format!("*** RUST PANIC [{thread}] at {loc}: {msg}");
        log(&line);
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&crate::paths::in_runtime_dir("plxnative-crash.log"))
        {
            let _ = writeln!(f, "{line}");
        }
        default(info); // preserve default behaviour (stderr -> plxnative-stderr.log)
    }));
}

/// Hide the Magic Remote's on-screen pointer. A webOS-only concept: there is no such cursor to
/// hide on a desktop, and `SDL_webOSCursorVisibility` exists in no SDL but LG's fork.
///
/// One door rather than a branch at each of the five call sites, so the platform question is
/// asked once and the call sites read the same on both.
#[inline]
pub(crate) unsafe fn hide_cursor() {
    #[cfg(not(feature = "hostsim"))]
    {
        SDL_webOSCursorVisibility(0);
    }
}
/// Advance the once-per-second LOOP-RATE window: bump `iters_ct` and, when a full second has
/// elapsed, recompute `loop_shown`, reset the window, and return `true` so the caller logs the
/// heartbeat with its own route/overlay tag. Shared by the player and home/detail draw paths.
///
/// This counts **loop iterations, not frames**. Since the present gate (`ui::idle`) landed the two
/// are different numbers, and conflating them is the single most reliable way to misread this app:
/// a settled screen runs the loop at the `IDLE_POLL_MS` rate while swapping nothing. The frame
/// count lives beside it in the heartbeat as `fps=`, from `ui::idle::take_presents`.
pub(crate) fn loop_tick(iters_ct: &mut i32, loop_t: &mut u32, loop_shown: &mut i32, now: u32) -> bool {
    *iters_ct += 1;
    if now.wrapping_sub(*loop_t) < 1000 {
        return false;
    }
    *loop_shown = (*iters_ct as f32 * 1000.0 / now.wrapping_sub(*loop_t) as f32 + 0.5) as i32;
    *iters_ct = 0;
    *loop_t = now;
    true
}
/// How many times a finished `plxnative-playurl` playback may start itself AGAIN — the
/// `/tmp/plxnative-replay` trigger's content, as a number (LG App Self Checklist #46).
///
/// A named function rather than a closure at the one call site, for `note_global_press`'s reason
/// one step removed: the call site is inside the SDL event loop, which no host test can enter, and
/// this half touches no SDL at all. Splitting it puts the parsing under `make check` instead of
/// leaving it gradeable only by a television — which matters more here than it looks, because
/// EVERY value this returns is a plausible one and a misparse is invisible on the panel: 0 reads
/// as "the replay arm is missing from this binary" and 2 reads as a loop.
///
/// `None` (no file) is 0 — the one-shot behaviour every other boot has always had. An EMPTY file
/// is 1, which is the whole idiom of this trigger surface (`touch` it and get the obvious thing).
/// An explicit `0` is honoured, so a script can arm the file and turn it off without deleting it.
/// Anything unparseable is 1 rather than 0: this file is armed by hand, and answering a typo with
/// "silently do nothing" is how a green run comes to mean the opposite of what it says.
pub(crate) fn replay_budget(raw: Option<&str>) -> u32 {
    match raw {
        None => 0,
        Some(s) => match s.trim() {
            "" => 1,
            t => t.parse::<u32>().unwrap_or(1),
        },
    }
}

#[cfg(test)]
mod replay_budget_tests {
    #[test]
    fn absent_is_one_shot_and_empty_is_one_replay() {
        assert_eq!(
            super::replay_budget(None),
            0,
            "no trigger must change nothing"
        );
        assert_eq!(super::replay_budget(Some("")), 1);
        assert_eq!(super::replay_budget(Some("  ")), 1);
    }

    #[test]
    fn a_number_is_honoured_including_a_deliberate_zero() {
        assert_eq!(super::replay_budget(Some("1")), 1);
        assert_eq!(super::replay_budget(Some("3")), 3);
        assert_eq!(super::replay_budget(Some(" 2 ")), 2);
        // An armed-but-off file, so a script can stop replaying without deleting the trigger.
        assert_eq!(super::replay_budget(Some("0")), 0);
    }

    #[test]
    fn a_typo_replays_once_rather_than_silently_doing_nothing() {
        // The file is armed by hand. Answering `-1` or `one` with 0 would make the case fail as
        // "the app never re-entered the player", i.e. as a missing feature rather than a typo.
        for bad in ["one", "-1", "1.5", "999999999999999999999"] {
            assert_eq!(super::replay_budget(Some(bad)), 1, "{bad}");
        }
    }
}

/// Resolve the server half of a direct dev-screen request.
///
/// An absent trigger preserves the historical `plxnative-play=<rk>` contract and uses the current
/// server. Once an explicit slot was written, however, failure is terminal: rating keys are local
/// to one PMS, so falling back could open a different item on another server.
pub(crate) fn resolve_direct_server(
    requested: Option<Result<u16, String>>,
    current: crate::plex::ServerId,
    registered: impl Fn(crate::plex::ServerId) -> bool,
) -> Result<crate::plex::ServerId, String> {
    let Some(slot) = requested else {
        return Ok(current);
    };
    let raw = slot?;
    let sid = crate::plex::ServerId::from_raw(raw);
    registered(sid)
        .then_some(sid)
        .ok_or_else(|| format!("server slot {raw} is not registered"))
}

#[cfg(test)]
mod direct_server_tests {
    //! The direct-screen server rule, beside the function that decides it. It stood in
    //! `app/mod.rs`'s `route_tests` — a module named for a concept D1 deleted — with a doc that
    //! called it a "route-classification rule"; it never was one. Pure, parallel, no global.
    use super::resolve_direct_server;

    #[test]
    fn an_explicit_direct_screen_server_never_falls_back_to_current() {
        let current = crate::plex::ServerId::from_raw(0);
        let secondary = crate::plex::ServerId::from_raw(1);
        assert_eq!(
            resolve_direct_server(None, current, |_| false),
            Ok(current),
            "an absent selector preserves the historical current-server contract"
        );
        assert_eq!(
            resolve_direct_server(Some(Ok(1)), current, |sid| sid == secondary),
            Ok(secondary)
        );
        let missing = resolve_direct_server(Some(Ok(2)), current, |sid| sid == secondary)
            .expect_err("an explicit missing slot must not become current");
        assert!(missing.contains("slot 2"), "{missing}");
        assert_eq!(
            resolve_direct_server(Some(Err("bad selector".into())), current, |_| true),
            Err("bad selector".into())
        );
    }
}

pub(crate) fn direct_trigger_server() -> Result<crate::plex::ServerId, String> {
    resolve_direct_server(
        crate::dev::server_slot(),
        crate::plex::current_server(),
        |sid| crate::plex::client_for(sid).is_some(),
    )
}

// ---- boot, and the loop's own between-frame state ---------------------------------------------
/// Which screen the boot gate landed on — see the gate itself in `plex_run`, which is where the
/// order of its four cases is argued.
pub(crate) enum BootTo {
    Home,
    Login,
    Profiles,
}

/// Pure capture boundary shared by actual boot and owner bootstrap regression fixtures.
pub(crate) fn captured_session_for_boot(saved: crate::plex::session::Session,
    dev_primary: Option<crate::plex::session::ServerRef>,
    extras: Vec<crate::plex::session::SourceRef>) -> crate::auth::SessionInit {
    crate::auth::SessionInit::captured_boot(saved, dev_primary, extras)
}

fn captured_dev_sources(servers: &[crate::dev::DevServer]) -> Vec<crate::plex::session::SourceRef> {
    servers.iter().filter(|s| s.usable()).filter_map(|s| {
        let origin = s.origin()?;
        Some(crate::plex::session::SourceRef { machine_id: s.machine_id.clone(), name: s.name.clone(),
            address: s.resolve_pin().map_or_else(|| s.host.clone(), |pin| pin.addr().to_string()),
            port: s.port, origin_url: origin.base(), token: s.token.clone(),
            shared_by: s.handle.clone(), owned: s.handle.is_empty(), tier: s.tier, ..Default::default() })
    }).collect()
}

    // Everything that has to happen when the server `plex::client()` answers with CHANGES —
    // whether because a new identity signed in or because the user walked into another source.
    // EVERY store below is keyed to whichever server was current when it was filled, and none
    // of them carries a server in its keys, so leaving one behind means server A's ratingKeys
    // being fetched from server B: the same catalog index opening a different film.
pub(crate) fn activate_server() -> crate::stores::EndpointRefreshSet {
        // the browse store must never carry the previous user's (or server's) cached grid,
        // watched-state angles, or section tabs forward
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        // …and the search store, for the same reason: a query, its results and the recent
        // terms are all one person's.
        crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
        // …and the hub twin: a FAILED fetch now keeps the catalog it already had (so one
        // wifi hiccup can't blank a populated Home), which makes this the one place that
        // must still wipe it — otherwise a profile switch whose fetch fails would leave the
        // previous user's shelves on screen.
        let _ = crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
        crate::stores::person::apply(crate::stores::person::PersonCmd::Reset); // ditto for an open person page's shelves
                                // …and any view-state write still queued or owed a refresh. It belongs to the account
                                // that pressed it, and the refresh it owes would land on shelves this reset just wiped.
        crate::stores::viewstate::apply(crate::stores::viewstate::ViewStateCmd::Reset);
        // Catalog activation is request-only. Home and section discovery both use their
        // existing worker/mailbox pumps, so a remote endpoint cannot park the SDL loop here.
        let mut endpoints = crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::RefetchHubs).endpoints;
        endpoints.merge(crate::stores::browse::discover_pump());
        log("pms: catalog activation queued");
        endpoints
}

    // Install the PMS client (the read layer AND the playback path) as the CURRENT server,
    // then fetch the catalog. Used by the boot gate and again when a login resolves; a later
    // call for the same address just swaps the token (profile switch).
    // Takes an ORIGIN and not a `(host, port)` pair: the pair cannot say `https`, and the host
    // a certificate is issued for is the `plex.direct` NAME rather than the address behind it
    // (`plex::origin`). Discovery and persisted sessions may supply either scheme; the client
    // routes control and media requests through the matching transport.
pub(crate) fn install_pms(
    origin: &crate::plex::Origin,
    token: &str,
    tier: Option<crate::plex::probe::Location>,
    pin: Option<&crate::plex::ResolvePin>,
    install: &crate::auth::owner::ReadyInstall,
) -> crate::stores::EndpointRefreshSet {
    if let crate::auth::owner::ReadyInstall::PrimaryAndExtras(extras) = install {
        crate::auth::install_captured_registry(origin, token, tier, pin, extras, None);
    }
    // Dev activation already installed its captured primary/extras under the acknowledged
    // owner permit. Never reinstall them or reread developer grants after that boundary.
    activate_server()
}
/// Everything before the loop: SDL and the window, GL, text, the poster workers, the boot gate
/// (login / token / session / picker), every dev trigger read once, and the `App` literal —
/// `plex_run`'s former body up to `while app.running`, moved verbatim in phase 1b-ii. An early
/// exit is the process exit code `plex_run` returns.
///
/// `mt`, THE main-thread token, is minted once by `plex_run` itself (`crate::task::MainThread::
/// assume()` — that function IS the SDL main thread) and MOVES into `App.adapters.player` here,
/// from where a `&mut PlayerAdapter` is the proof it is still held: the ACB/Starfish seam takes
/// `&MainThread` (which is `!Send`, so `task::spawn` rejects any closure that captured one), and
/// the native session slot takes the adapter itself. See `task::MainThread` and `player::adapter`.
pub(crate) unsafe fn boot(
    pms_host: *const c_char,
    pms_port: c_int,
    mt: crate::task::MainThread,
    preflight: super::bootstrap::Preflight,
) -> Result<App, c_int> {
    let initial = match &preflight {
        super::bootstrap::Preflight::Live => None,
        super::bootstrap::Preflight::Record => {
            let host = std::ffi::CStr::from_ptr(pms_host).to_string_lossy();
            match super::bootstrap::Initial::capture_home(&host, pms_port) {
                Ok(initial) => Some(initial),
                Err(reason) => { log(&format!("rec: REFUSED — {reason}")); return Err(1); }
            }
        }
        super::bootstrap::Preflight::Replay { initial, .. } => Some((initial.clone(), None)),
    };
    match initial {
        Some((initial, deferred)) => App::from_init(initial, preflight, pms_host, pms_port, mt, deferred),
        None => construct(pms_host, pms_port, mt, preflight, None, None),
    }
}

pub(super) fn apply_deferred_capture(rec: &mut super::recorder::Recplay,
    deferred: crate::plex::session::DeferredLoad) -> Result<(), &'static str> {
    if let Err(reason) = deferred.apply() {
        rec.abort_startup()?;
        return Err(reason);
    }
    Ok(())
}

pub(crate) unsafe fn construct(
    pms_host: *const c_char, pms_port: c_int, mt: crate::task::MainThread,
    preflight: super::bootstrap::Preflight, initial: Option<super::bootstrap::Initial>,
    deferred: Option<crate::plex::session::DeferredLoad>,
) -> Result<App, c_int> {
    let controlled = preflight.controlled();
    if let Some(initial) = &initial {
        initial.home.restore_boot(&mt).map_err(|_| 1)?;
        crate::plex::Client::restore_generation_seed(initial.primary_client).map_err(|_| 1)?;
    }
    SDL_SetMainReady();
    // DEAD END, measured 2026-07-31 — do not re-try this. The obvious answer to "a parked TV
    // should blank itself" is to stop inhibiting the platform screensaver here (and re-allow it
    // per route, since webOS BACKGROUNDS the app to run one and `0x103` suspends the
    // buffer-feed, so it could never be on during playback). It does not work, for a reason
    // upstream of this app: the TV's SDL 2.0.4 fork carries the
    // `SDL_VIDEO_ALLOW_SCREENSAVER` hint STRING but implements no wayland idle-inhibit
    // (`strings libSDL2-2.0.so.0` finds no `idle_inhibit`/`suspend_screensaver` symbol), so
    // this call and `SDL_EnableScreenSaver` are both no-ops. Soaked 34 min on Home with the
    // TV's own `screenSaverEnabled: on`: no screensaver, no `LIFECYCLE: background`, CPU flat,
    // our UI still at full brightness on the panel. webOS does not blank a foreground native
    // app, and nothing reachable from SDL changes that. The line stays because it costs
    // nothing and states the intent; it is not what keeps the screensaver away.
    SDL_SetHint(c"SDL_VIDEO_ALLOW_SCREENSAVER".as_ptr(), c"0".as_ptr());
    if SDL_Init(SDL_INIT_VIDEO) != 0 {
        log("SDL_Init failed");
        return Err(1);
    }
    {
        let d = SDL_GetCurrentVideoDriver();
        if !d.is_null() {
            log(&format!(
                "video driver: {}",
                std::ffi::CStr::from_ptr(d).to_string_lossy()
            ));
        }
    }
    // The television has a real GLES2 driver (a shim over libmali). macOS has none at all —
    // Apple ships desktop GL only, capped at 4.1 core — so asking for ES here fails context
    // creation outright. 4.1 core is the closest thing that exists, and it is a superset for
    // everything this renderer does: a real VBO (never client arrays) and RGBA/UNSIGNED_BYTE
    // textures, both core-profile-legal. The shader sources are adapted at compile time by
    // `gfx::glsl_preamble`, which reads the driver's GLSL version rather than assuming.
    if cfg!(feature = "hostsim") {
        SDL_GL_SetAttribute(A_CTX_PROFILE_MASK, CTX_PROFILE_CORE);
        SDL_GL_SetAttribute(A_CTX_MAJOR, 4);
        SDL_GL_SetAttribute(A_CTX_MINOR, 1);
    } else {
        SDL_GL_SetAttribute(A_CTX_PROFILE_MASK, CTX_PROFILE_ES);
        SDL_GL_SetAttribute(A_CTX_MAJOR, 2);
        SDL_GL_SetAttribute(A_CTX_MINOR, 0);
    }
    // full 32-bit RGBA so the video plane shows through
    SDL_GL_SetAttribute(A_RED, 8);
    SDL_GL_SetAttribute(A_GREEN, 8);
    SDL_GL_SetAttribute(A_BLUE, 8);
    SDL_GL_SetAttribute(A_ALPHA, 8);
    SDL_GL_SetAttribute(A_BUFFER_SIZE, 32);
    // ...and NO depth or stencil, which SDL would otherwise give us anyway: its defaults are
    // 16 bits of depth and 0 of stencil, and asking for neither had simply never been written
    // down. **This renderer has no use for either.** There is no `GL_DEPTH_TEST`, no
    // `glDepthFunc`, no `glDepthMask` and no `glClear(GL_DEPTH_BUFFER_BIT)` anywhere in the
    // crate — every screen is painter's-algorithm 2-D, drawn back to front — and the one
    // scissor user (`gfx::clip_set`) is a scissor, not a stencil.
    //
    // On a TILER this is not merely 4 MB of address space. Midgard allocates the depth buffer
    // per tile alongside colour and, unless the driver proves it dead, RESOLVES it to memory at
    // end-of-frame: 1920x1080x2 bytes written per presented frame for a buffer nothing ever
    // reads. `system.rs` logs what the config actually came back with — a request is not a
    // grant, and the only honest confirmation is `FB bits: … depth=0`.
    SDL_GL_SetAttribute(A_DEPTH, 0);
    SDL_GL_SetAttribute(A_STENCIL, 0);
    // The television is placed at 0,0 at exactly canvas size and takes the panel. A desktop
    // window is centred (`SDL_WINDOWPOS_CENTERED`) at whatever fits — see `desktop_window_size`.
    #[cfg(feature = "hostsim")]
    let (wx, wy, ww_req, wh_req) = {
        let (w, h) = desktop_window_size();
        (0x2FFF_0000u32 as c_int, 0x2FFF_0000u32 as c_int, w, h)
    };
    #[cfg(not(feature = "hostsim"))]
    let (wx, wy, ww_req, wh_req) = (0, 0, SCR_W, SCR_H);
    // The title is furniture a television never draws (no window manager, no decoration) and
    // the first thing a desktop shows, so the two builds spell it differently: the device keeps
    // the process-shaped name every log, `pidof` recipe and skill already uses.
    #[cfg(feature = "hostsim")]
    let title = c"PlxNative";
    #[cfg(not(feature = "hostsim"))]
    let title = c"plxnative";
    let win = SDL_CreateWindow(title.as_ptr(), wx, wy, ww_req, wh_req, SDL_WINDOW_FLAGS);
    if win.is_null() {
        log("CreateWindow failed");
        return Err(1);
    }
    let ctx = SDL_GL_CreateContext(win);
    if ctx.is_null() {
        log("GL ctx failed");
        return Err(1);
    }
    crate::surface::probe(win);
    // vsync on → the frame rate locks to the panel refresh. `/tmp/plxnative-novsync` uncaps it so the
    // FPS counter reports the TRUE GPU render rate (a diagnostic: if fps then jumps well past the
    // vsynced number, we were panel/refresh-bound, not GPU-bound).
    SDL_GL_SetSwapInterval(if !controlled && crate::dev::scenarios::novsync_armed() { 0 } else { 1 });
    {
        let r = glGetString(GL_RENDERER);
        let v = glGetString(GL_VERSION);
        if !r.is_null() && !v.is_null() {
            log(&format!(
                "GL: {} / {}",
                std::ffi::CStr::from_ptr(r).to_string_lossy(),
                std::ffi::CStr::from_ptr(v).to_string_lossy()
            ));
        }
    }
    // The system on-screen keyboard, PROBED — see `crate::textinput`'s module doc. Both facts
    // on this line are preconditions that fail in complete silence, and nothing in this tree
    // had ever read either of them:
    //   support= `SDL_HasScreenKeyboardSupport` — does this firmware's SDL have a panel at all.
    //   focus=   `SDL_WINDOW_INPUT_FOCUS` — `SDL_StartTextInput` shows the panel only
    //            `if (SDL_GetKeyboardFocus())`. Clear, and it enables text events, returns
    //            void, and no panel appears.
    //   active=  whether text events are already on. It is 1 on a desktop and 0 here, because
    //            SDL only auto-starts text input on platforms with NO screen keyboard — which
    //            is precisely why `textinput` tracks its own started flag instead of this one.
    // A `focus=0` HERE is not yet a verdict: the flag arrives with the wayland keyboard
    // `enter`, which needs the event loop below. `textinput::start` logs it again at the
    // moment the field asks for the panel, which is the reading that decides anything.
    // What EGL this set has — extension string, swap behaviour, buffer age. One boot-time
    // read, logged and used for nothing: `docs/egl-partial-update-and-damage.md` is what it
    // was for. Deliberately NOT a new link dependency; see `egl.rs`'s module doc for why
    // `-lEGL` would kill the process at exec() on the very firmwares this app runs on.
    crate::egl::probe();
    crate::textinput::bind(win);
    // …and the same handshake for the ROOT press: `webos::go_home`'s fallback leg minimizes
    // this window, and the window is created here, a long way from where BACK is decided.
    crate::webos::bind_window(win);
    let wflags = SDL_GetWindowFlags(win);
    log(&format!(
        "keyboard: support={} active={} focus={} winflags=0x{wflags:x}",
        SDL_HasScreenKeyboardSupport(),
        SDL_IsTextInputActive(),
        i32::from(wflags & SDL_WINDOW_INPUT_FOCUS != 0)
    ));

    crate::system::sys_grab_wayland(win);
    // EXPERIMENT (`/tmp/plxnative-opaque`), no-op without the trigger: build the full-surface
    // wl_region once, so `opaque_route` below can declare the UI plane opaque on every screen
    // that has nothing behind it. See `system.rs`'s section on it.
    crate::system::opaque_region_init();
    crate::gfx::init_gl();
    crate::text::init_text();
    crate::gfx::init_image();
    crate::gfx::init_blur();
    // One-time libcurl bind + init (main thread) before any threaded HTTPS call. A false here
    // means this device has no libcurl we can bind, so plex.tv sign-in will not work — the app
    // still runs, and `net::global_init` has already said so in the event log.
    let _ = crate::net::global_init();
    // Drain whatever the LAST session left behind, on a worker — and **after `global_init`,
    // which is the whole reason this line is here and not beside `telemetry::boot()` 170 lines
    // up.** It was there first, and the end-to-end run showed why that was wrong: the worker
    // reached `post_ca` before libcurl was bound, `net::available()` was false, every record
    // came back Keep, and the log read `holding 5 records` immediately ABOVE `net: bound
    // libcurl`. So the first flush of every launch failed, always, and the failure was
    // indistinguishable from a television with no network. Worse than the lost flush: curl's
    // own init is documented as not thread-safe, and a worker that got there first would have
    // been doing it off the main thread.
    //
    // Boot is the right cadence for a television. Sessions are long, and the reports most worth
    // having are about how one ENDED — a crash is the end, so the record was written by a
    // process that no longer exists and this is the first moment anything can send it. A record
    // queued during THIS session goes out at the next launch, or sooner if a consent change
    // flushes.
    if !preflight.controlled() { crate::telemetry::flush_soon(); }

    // NO token is compiled into this binary. PMS access comes from the signed-in session,
    // or — for automated runs only (the regression harness, headless captures) — from the
    // /tmp/plxnative-token dev trigger. The value is NEVER logged (only that one is in effect).
    let dev_token = match &initial {
        Some(initial) => {
            let crate::auth::owner::BootstrapAuthority::DevPms { primary, .. } = &initial.session.authority
                else { unreachable!("preflight validated authority") };
            primary.token.clone()
        }
        _ => crate::dev::scenarios::dev_token(),
    };
    // dev: /tmp/plxnative-servers — credentials for a SECOND (third, …) server, so an automated
    // run can reach a friend's SHARED server beside the one above. A shared server is its own
    // authority: its own machineIdentifier, its own per-(user,server) access token, and a 401
    // for anybody else's — which is precisely what ONE `plxnative-token` cannot express, and
    // why no two-source state could be graded headlessly before this.
    //
    // ADDITIVE and nothing more. The primary is still `plxnative-token` (or the stored session)
    // against the compiled-in host/port, byte for byte, so a run that names one server behaves
    // exactly as it always did. `dev::servers()` is the accessor — memoized, so the harness's
    // /tmp wipe cannot change what this boot was handed — and `dev::DevServer` is the shape.
    //
    // It is NOT on the DIAG exemption list (`dev.rs`), deliberately: unlike a log or the anim
    // overlay, this file names a host AND the token to trust it with, so it must mark the boot
    // automated and skip the who's-watching picker exactly as `plxnative-token` does. A run
    // that landed on the picker instead of Home would grade the wrong screen.
    //
    // Tokens are never logged: `DevServer` has no `Debug`, and `describe()` prints all of it
    // except the token.
    let dev_servers = if controlled { Ok(Vec::new()) } else { crate::dev::servers() };
    match &dev_servers {
        Err(e) => log(&format!(
            "servers: /tmp/plxnative-servers IGNORED — not valid JSON: {e}"
        )),
        Ok(v) if !v.is_empty() => {
            let usable = v.iter().filter(|s| s.usable()).count();
            for (i, s) in v.iter().enumerate() {
                let creds = if s.usable() {
                    "ok"
                } else {
                    "MISSING (empty host/port or token)"
                };
                log(&format!("servers: #{i} {} creds={creds}", s.describe()));
            }
            log(&format!(
                "servers: {} extra server(s) injected, {usable} usable",
                v.len()
            ));
        }
        Ok(_) => {}
    }
    let host_s = std::ffi::CStr::from_ptr(pms_host)
        .to_string_lossy()
        .into_owned();

    // UI infra + poster workers always come up — the owned login/profiles screens use them too.
    //
    // **No `ui::login::init()` / `ui::profiles::init()` here any more (phase 6 retirement).**
    // Both calls used to allocate the legacy `Scene` statics `ui::login`/`ui::profiles` drew
    // from; `screens::login::LoginScreen`/`screens::profiles::ProfilesScreen` are owned
    // `Screen`s constructed fresh by `AppMounter::mount` on each entry (`app/bridge.rs`) and
    // read no module global at all, so nothing left in the boot path calls into either legacy
    // module's `scene()` and the `.expect("…::init not called")` guard those calls carried is no
    // longer reachable. BOTH legacy modules are deleted outright: `ui/login.rs`'s one surviving
    // reader (`screens::login::LoginScreen::resync`'s `delete_leftovers` count) moved to `auth`,
    // and `ui/profiles.rs`'s last reader was a single `const` — `screens::onboard.rs`'s
    // `CRUMB_PROFILES`, the breadcrumb over the Favourites editor — which now reads
    // `screens::profiles::TITLE`, the owned screen's own title. Worth remembering when the next
    // family is retired: what kept 1,250 dead lines in the tree was not a hard dependency but one
    // string constant nobody had moved, and it was invisible because the file still compiled.
    if !preflight.controlled() {
        super::adapters::poster::init();
        crate::capture::init(); // dev live UI capture stream
    }

    // Any dev trigger under /tmp marks the boot as automated (the harness token override,
    // autoplay/detail captures, playback-path knobs): those runs need a deterministic Home,
    // so the boot who's-watching picker is skipped. Pure diagnostics (the logs, the profiler,
    // the anim overlay) don't count as automation.
    // The scan itself, and the DIAG exemption list that decides what does NOT count as
    // automation, live in `dev::any_trigger_present` — together with the `plxnative-anim.log`
    // bug that list was rewritten for. It is the one dev-trigger surface that names no file,
    // so it is also the one a release build had to be taught about explicitly.
    let automated_boot = || controlled || crate::dev::any_trigger_present();

    // Boot gate. Order matters:
    //  1. /tmp/plxnative-login forces the QR login screen (to exercise the flow on demand).
    //  2. /tmp/plxnative-token (the harness / headless runs) beats the stored session — automation
    //     must run as the injected test identity no matter who is signed in on the TV.
    //  3. A stored session (offline-capable LAN server) → Home. A multi-user Plex Home roster
    //     still goes through the who's-watching picker first on interactive boots, unless
    //     Automatically Sign In is on and a profile is already seated (that skip is an explicit
    //     opt-in, PIN included). Automated boots and a roster of one still skip the picker.
    //  4. Nothing → the QR sign-in flow (no credentials are compiled in — like a real client).
    // The destination itself is [`BootTo`], at module scope with the rest of the vocabulary.
    //
    // dev: /tmp/plxnative-pickuser=<index> — force the boot picker even on an automated boot and
    // auto-select that roster tile once it's up (headless exercise of the who's-watching flow).
    let pick_user: Option<usize> = if controlled { None } else { crate::dev::scenarios::pickuser_index() };
    let session = match &initial {
        Some(initial) => initial.session.persisted.clone(),
        None => crate::plex::session::load(),
    };
    let forced_login = !controlled && crate::dev::scenarios::login_forced();
    let dev_primary = (!forced_login && !dev_token.is_empty()).then(|| crate::plex::session::ServerRef {
        address: host_s.clone(), port: i64::from(pms_port),
        origin_url: crate::plex::Origin::http(&host_s, pms_port).base(), token: dev_token.clone(),
        tier: Some(crate::plex::probe::configured_tier(&host_s)), ..Default::default()
    });
    let session_init = match &initial {
        Some(initial) => initial.session.clone(),
        None => captured_session_for_boot(session.clone(), dev_primary,
            captured_dev_sources(&dev_servers.unwrap_or_default())),
    };
    let mut bridge = if controlled {
        super::bridge::Bridge::controlled_home(crate::diag::heartbeat::now_us,
            initial.as_ref().expect("controlled initialization"), &mt, preflight.replay())
    } else {
        super::bridge::Bridge::new(crate::diag::heartbeat::now_us, session_init, &mt)
    };
    // Construct the one dispatcher before bootstrap commands; move this same queue into App.
    let mut pages = crate::ui::dispatch::Dispatcher::with_transition(Box::new(
        crate::ui::containers::transition::PageDip::new(),
    ));
    // Install-wide playback preference, restored before any route can resolve a stream.
    // A legacy file with no value resolves to Original; a new file can choose Auto only
    // through route's explicit readiness gate (session::load records that decision once).
    crate::route::restore_quality(
        if controlled { session.playback_quality() }
        else { crate::dev::playback_quality_override().unwrap_or_else(|| session.playback_quality()) },
    );
    let primary_binding = initial.as_ref().map(|initial| initial.primary_client);
    let activate_session = |bridge: &mut super::bridge::Bridge,
        pages: &mut crate::ui::dispatch::Dispatcher<super::bridge::AppHost>,
        rec: &mut super::recorder::Recplay| {
        if forced_login {
        super::bridge::execute_session_command(pages, crate::auth::SessionCmd::StartLogin);
        pages.frame_with(bridge, crate::ui::machine::Tick::default(), Vec::new(), Vec::new(),
            rec, false);
        log("boot: /tmp/plxnative-login — starting QR login");
        BootTo::Login
    } else if !dev_token.is_empty() {
        // `Origin::http` names the assumption out loud: the host and port compiled into the
        // C shim are a plaintext address, with no scheme to read off them.
        //
        // The tier is classified from that address rather than left `None`. There is no
        // plex.tv connection list on this path to read a `local` flag off, and `None` means
        // "nothing has said" — which left every automated run unable to reach Auto's Original
        // bootstrap, since `abr::bootstrap` is only consulted once a tier exists. See
        // `probe::configured_tier` for why address shape is honest enough here.
        let tier = crate::plex::probe::configured_tier(&host_s);
        log(&format!(
            "boot: dev token — link={tier:?} (classified from the configured address)"
        ));
        super::bridge::execute_session_command(pages, crate::auth::SessionCmd::ActivateDevBootstrap);
        pages.frame_with(bridge, crate::ui::machine::Tick::default(), Vec::new(), Vec::new(),
            rec, false);
        if let Some(ready) = bridge.take_session_ready() {
            if controlled {
                if bridge.bind_primary(primary_binding.expect("controlled initial binding")).is_err() {
                    log("bootstrap: primary resource binding refused");
                    return BootTo::Login;
                }
                use crate::stores::{StoreCmd, StoreWork};
                use crate::ui::machine::{Fx, MachineId};
                use crate::screens::registry::AppFx;
                for cmd in [
                    StoreCmd::Browse(crate::stores::browse::BrowseCmd::Reset),
                    StoreCmd::Hubs(crate::stores::hubs::HubsCmd::Reset),
                    StoreCmd::Hubs(crate::stores::hubs::HubsCmd::RefetchHubs),
                ] { pages.emit(MachineId::Nav, Fx::App(AppFx::Store(cmd.store(), cmd))); }
                pages.emit(MachineId::Nav, Fx::App(AppFx::StoreWork(StoreWork::BrowseDiscovery)));
                pages.frame_with(bridge, crate::ui::machine::Tick::default(), Vec::new(), Vec::new(), rec, false);
            } else {
                let endpoints = install_pms(&ready.origin, &ready.token, ready.tier, ready.pin.as_ref(), &ready.install);
                super::bridge::execute_endpoint_outcomes(pages, endpoints);
            }
            BootTo::Home
        } else { BootTo::Login }
    } else if session.can_go_local() {
        if session.boot_shows_picker(automated_boot(), pick_user.is_some()) {
            // The Session owner installs the avatar read client and retained grants, publishes
            // the captured profile, then owns the picker/refresh flows. It does NOT issue the
            // Ready handoff before a viewer is selected. Boot BACK therefore retains its
            // protected/unknown-profile refusal policy rather than silently entering Home.
            super::bridge::execute_session_command(pages,
                crate::auth::SessionCmd::StartSwitch(crate::auth::Picker::Boot));
            pages.frame_with(bridge, crate::ui::machine::Tick::default(), Vec::new(), Vec::new(),
                rec, false);
            log("boot: stored session — who's watching");
            BootTo::Profiles
        } else {
            // The owner restores the granted roster and publishes WHO is watching before
            // install_pms can read any per-profile stores. Stored boot does not re-save its
            // credentials. This is the normal bounded dispatcher drain over the same owner
            // and queue later moved into App, not a recursive bootstrap reducer.
            super::bridge::execute_session_command(pages, crate::auth::SessionCmd::ResumeStored);
            pages.frame_with(bridge, crate::ui::machine::Tick::default(), Vec::new(), Vec::new(),
                rec, false);
            if let Some(ready) = bridge.take_session_ready() {
                let endpoints = install_pms(&ready.origin, &ready.token, ready.tier, ready.pin.as_ref(), &ready.install);
                super::bridge::execute_endpoint_outcomes(pages, endpoints);
                // Refresh only AFTER installing the captured primary, so a fast accepted
                // endpoint observation cannot be overwritten by that older boot snapshot.
                super::bridge::execute_session_command(pages, crate::auth::SessionCmd::RefreshRoster);
                pages.frame_with(bridge, crate::ui::machine::Tick::default(), Vec::new(), Vec::new(),
                    rec, false);
                if session.auto_sign_in()
                    && session.home_users.len() > 1
                    && session.seated_in_roster()
                    && !automated_boot()
                    && pick_user.is_none()
                {
                    log("boot: stored session — auto sign-in");
                } else {
                    log("boot: stored session — local server (offline-capable)");
                }
                BootTo::Home
            } else {
                super::bridge::execute_session_command(pages, crate::auth::SessionCmd::StartLogin);
                log("boot: stored session could not be activated — starting QR sign-in");
                BootTo::Login
            }
        }
    } else {
        super::bridge::execute_session_command(pages, crate::auth::SessionCmd::StartLogin);
        pages.frame_with(bridge, crate::ui::machine::Tick::default(), Vec::new(), Vec::new(),
            rec, false);
        log("boot: no session — starting QR sign-in");
        BootTo::Login
    }
    };
    // Controlled Home constructs its App before executing the same bootstrap command path.
    let boot_to = if controlled { BootTo::Home } else {
        activate_session(&mut bridge, &mut pages, &mut super::recorder::Recplay::Off)
    };
    if !controlled {
        crate::player::acb_init(&mt);
        crate::ff::boot(); // Live playback resource boot; controlled Home cannot invoke ABI probes.
    }
                       // dev: /tmp/plxnative-logintest validates the plex.tv account path end-to-end on the device — a
                       // real typed create_pin() through the libcurl transport + DTO deserialize. Logs only the
                       // public pin id + code length + that authToken is still null (never a token/secret).
    if !controlled { crate::dev::scenarios::arm_logintest(); }
    // dev: the animation-diagnostic overlay is OFF by default; /tmp/plxnative-anim enables it (its
    // trace goes to /tmp/plxnative-anim.log, a separate stream from the main event log)
    if !controlled { crate::dev::scenarios::arm_anim(); }
    // dev: profile is asynchronous EXT_disjoint_timer_query timing; hwcnt is the serialized
    // direct Mali counter-attribution run. Their content names ONE phase (empty = frame.ui).
    // Combining them would perturb the timer result, so fail closed when both are present.
    // dev: /tmp/plxnative-glassload is the backdrop-glass LOAD DIAL — a sweep of glass-surface
    // count, size and refresh cadence that cycles its own steps inside one launch, so legs are
    // interleaved by construction. /tmp/plxnative-navblur is the blurred-route-transition
    // prototype. Both live in `ui::glassload`; both are absent from a release build.
    // The plan is built HERE rather than in the `App` literal below so the dial is armed at the
    // point in boot it always was: `configure` logs what it will run, and that line's position in
    // the event log is what a sweep is read against.
    let mut glass = crate::ui::frame::glass::GlassPlan::new();
    if !controlled {
        crate::dev::scenarios::arm_glassload(&mut glass);
        crate::dev::scenarios::arm_navblur(&mut glass);
    }
    // dev: the two OVERDRAW surfaces (`ui::overdraw`, docs/backdrop-blur-profiling.md Part 5).
    // `plxnative-overdraw` arms the CPU-side per-draw-class ledger — how much screen-visible
    // quad area this app submits, per primitive family, per frame. It is not billed for the
    // wayland compositor's work and is not `glFinish`-serialised, which is what the GPU's
    // global FRAG_QUADS_RAST cannot say. `plxnative-drawmask=<classes>` REFUSES every draw of
    // the named classes, so a whole-frame `frame.ui` A/B against the unmasked control prices
    // that class as the frame sees it; `all` draws nothing and is therefore the compositor
    // floor. A masked leg is a broken picture on purpose.
    if !controlled {
        crate::dev::scenarios::arm_overdraw();
        crate::dev::scenarios::arm_drawmask();
    }
    // dev: /tmp/plxnative-heroground — draw the hero's photograph and BOTH of its scrim fields
    // in one pass instead of the art plus four blended gradient quads over it. Absent, the
    // shipped four-quad path draws, which is what makes this an A/B on one binary.
    if !controlled { crate::dev::scenarios::arm_heroground(); }
    // dev: /tmp/plxnative-glasshz=<presents-per-refresh> moves the shared dynamic-backdrop
    // cadence for the cost curve in `docs/backdrop-blur-profiling.md` — 1 is a refresh on every
    // present (60 Hz while the UI presents at 60), 3 is ~20 Hz, 4 is 15 Hz.
    // ABSENT, nothing here runs and the cadence is exactly the shipped one. It is a profiling
    // knob, so it also turns on the heartbeat's `snap=` field (refreshes per second), which
    // is the only way to check the cadence that RAN against the one that was asked for — and
    // The production Account menu no longer arms or consumes this path: its host is frozen and
    // its glass snapshot is cached for the whole open lifetime.  The knob remains for explicit
    // material profiling, not as part of an Account FPS scene.
    let glass_hz_armed = !controlled && crate::dev::scenarios::arm_glasshz();
    // dev: /tmp/plxnative-nobudget — the frame budget's A/B CONTROL leg (spec §8.1). Read here
    // with the other boot triggers; applied to the one `Budget` below, once the tree exists.
    let nobudget = !controlled && crate::dev::scenarios::nobudget_armed();
    if !controlled { crate::dev::scenarios::arm_profile_hwcnt(); }
    // dev: /tmp/plxnative-cpuprof — the render thread's OWN time per phase, every phase at
    // once, no glFinish. The one mode that can see a frame the frame-drop detector reports as
    // all `draw=` and no `swap=`; the two GPU modes above are blind to it by construction.
    if !controlled { crate::dev::scenarios::arm_cpuprof(); }
    // dev: /tmp/plxnative-noidle turns the whole-frame present gate (ui::idle) OFF, so a still
    // screen goes back to repainting at panel rate. It is a DIAG trigger (see the list above)
    // precisely so an A/B costs one file and does not also change which screen you boot to —
    // and so that if a frame ever looks wrong on the panel, ruling this feature out is one
    // `rm` rather than a redeploy.
    if !controlled {
        crate::ui::testpat::boot();
        crate::player::seed_dev_track_names();
    }
    if let Some(initial) = &initial {
        crate::ui::idle::set_enabled(!initial.triggers.iter().any(|t| t == "plxnative-noidle"));
    } else { crate::dev::scenarios::arm_noidle(); }
    // dev: /tmp/plxnative-detailosc (read once at boot, like the other triggers) makes the detail scroll
    // perpetually swing hero<->bottom so the FPS heartbeat samples the transition, not the ends.
    let detail_osc = !controlled && crate::dev::scenarios::detailosc_armed();
    // dev: /tmp/plxnative-homeosc — perpetually sweep the home grid focus DOWN to the bottom then
    // UP to the top (~3s each way, one row per 350ms), so a headless run reproduces the top↔bottom
    // vertical-scroll judder for the frame-drop detector / retui profiler.
    let home_osc = !controlled && crate::dev::scenarios::homeosc_armed();
    let home_osc_last = 0u32;
    // dev: the two Home transition scenes the old home-hero/home-grid pair could not see.
    // `heroosc` continuously pages the real carousel; `homefoldosc` alternates the real
    // hero↔first-shelf snap. Their intervals overlap the spring lifetime so the FPS heartbeat
    // samples motion rather than the efficient idle gaps at either end.
    let hero_osc = !controlled && crate::dev::scenarios::heroosc_armed();
    let hero_osc_last = 0u32;
    let home_fold_osc = !controlled && crate::dev::scenarios::homefoldosc_armed();
    let home_fold_osc_last = 0u32;
    let home_fold_down = true;
    // dev: /tmp/plxnative-libosc — the Library twin of homeosc: sweep the browse grid focus
    // down↔up perpetually for the library_scroll FPS scene.
    let lib_osc = !controlled && crate::dev::scenarios::libosc_armed();
    let lib_osc_last = 0u32;
    // dev: /tmp/plxnative-libswitch — exercise EVERY Library switch on a timer (tab switch,
    // sort menu open/move/close, unwatched on/off, filter open/close) for the library_switch
    // FPS scene, so the re-query + popover paths are perf-gated, not just the scroll.
    let lib_switch = !controlled && crate::dev::scenarios::libswitch_armed();
    let lib_switch_last = 0u32;
    let lib_switch_step = 0u32;
    // dev: /tmp/plxnative-searchosc — the Search twin of homeosc/libosc: sweep the result
    // shelves' focus down↔up perpetually for the `fps:search-type` scene. It does NOT reach the
    // screen on its own — pair it with `/tmp/plxnative-search=<query>`, and with a query the
    // library actually matches, or there are no shelves to sweep and the scene grades nothing.
    let search_osc = !controlled && crate::dev::scenarios::searchosc_armed();
    let search_osc_last = 0u32;
    // dev: /tmp/plxnative-settings=<root|home|privacy|legal> opens the Settings modal (and,
    // optionally, one of its real child panels) once Home is available. `settingsosc` turns
    // that settled modal into a continuous render-throughput scene: it alternates the focused
    // row and explicitly keeps the present gate awake. Without the latter an efficient,
    // completely healthy modal intentionally reports ~0 fps after its springs settle, which
    // cannot grade the screen's fill cost. The paired settings-idle scene omits the oscillator
    // and guards the inverse contract.
    let settings_boot = if controlled { None } else { crate::dev::scenarios::settings_boot_value() };
    let settings_osc = !controlled && crate::dev::scenarios::settingsosc_armed();
    let settings_osc_last = 0u32;
    let settings_osc_down = true;
    // dev: /tmp/plxnative-modalosc — with `plxnative-settings=root`, OPEN and DISMISS the
    // Settings modal every 1500 ms through the same `open`/`on_back` the chip and BACK use, so
    // `fps:modal-ramp` grades the appear/disappear RAMP (host snapshot, scrim, ground) under
    // `worst_ceiling_ms` rather than a settled modal. It reverses on a clock because the ramp
    // itself has no end the app reports.
    let modal_osc = !controlled && crate::dev::scenarios::modalosc_armed();
    let modal_osc_last = 0u32;
    // dev: /tmp/plxnative-legaldoc — with `plxnative-settings=legal`, press OK on the Legal
    // index ONCE so the boot lands on a pushed DOCUMENT (the reader over the frozen ground),
    // which no boot trigger reached before: `fps:legal-document`.
    let legal_doc = !controlled && crate::dev::scenarios::legaldoc_armed();
    let legal_doc_tried = false;
    // dev: /tmp/plxnative-alert — with `plxnative-settings=privacy`, open the "Delete all local
    // data?" DECISION ALERT once the privacy panel is up. It is the one shared yes/no alert in
    // the app and nothing headless could reach it: `fps:decision-alert`. Opening it is all this
    // does — nothing is deleted, and Cancel is what a BACK would press.
    let alert_boot = !controlled && crate::dev::scenarios::alert_armed();
    let alert_tried = false;
    // …and the DOWN presses that walk to the delete row before the OK that opens it. A count
    // rather than an index: the row is the LAST of the privacy table, whose length is that
    // screen's own business, and DOWN at the last row of a document with no action band is an
    // `Outcome::Edge` — i.e. idempotent — so any count comfortably past the table's length lands
    // exactly on it whatever the table becomes. One press per frame, for the reason every other
    // oscillator here steps at a human cadence: a whole table walked inside one frame would be a
    // slow frame, and this scene's gates (`worst_ceiling_ms`, `stall_ceiling_ms`) include warmup.
    let alert_step = 0u8;
    // The profile menu freezes its host and uses one cached backdrop. Drive the menu's own
    // TableView for a strict FPS scene; reusing `homeosc` would now correctly move nothing and
    // would grade the idle keepalive rather than the popover.
    let account_osc = !controlled && crate::dev::scenarios::acctosc_armed();
    let account_osc_last = 0u32;
    let account_osc_down = true;
    // First-run route oscillators keep their real focus models moving so the device FPS suite
    // grades the composition rather than a settled screen that correctly stops presenting.
    let consent_osc = !controlled && crate::dev::scenarios::consentosc_armed();
    let consent_osc_last = 0u32;
    let consent_osc_down = true;
    let onboard_osc = !controlled && crate::dev::scenarios::onboardosc_armed();
    let onboard_osc_last = 0u32;
    let onboard_osc_right = true;
    // dev: /tmp/plxnative-navosc — bounce the ROUTE on a timer, so the page cross-fade
    // (`ui::nav`) is FPS-gated like every other motion in the app. These are the only scenes
    // that change route, and therefore the only ones that sample a whole-screen cascade alpha
    // over both screens' full draw. 1400 ms matches `libswitch`: long enough that the ~225 ms
    // transition is measured against a settled screen on either side.
    //
    // EMPTY file = Home↔the first library section (the `home-library-nav` scene, whose two
    // pages share the top tab bar). A `<ratingKey>` = Home↔that item's DETAIL page instead
    // (`home-detail-nav`) — the arm phase 2 added, and a genuinely different cost: no shared
    // chrome, a hero backdrop and an ambient wash on the far side, and a real teardown at the
    // floor. Both bounce through the SAME `nav_open`/`nav_back` the interactive presses use, so
    // the scene measures the transition rather than an imitation of it.
    let nav_osc_rk = if controlled { None } else { crate::dev::scenarios::navosc_value() };
    let nav_osc = nav_osc_rk.is_some();
    let nav_osc_rk = nav_osc_rk.unwrap_or_default();
    let nav_osc_last = 0u32;

    // dev: /tmp/plxnative-framedrop — the FRAME-DROP DETECTOR. When present, each frame is timed with
    // the high-res perf counter (pump / draw / swap, NO glFinish so it doesn't perturb the pipeline),
    // and any frame whose total exceeds a threshold (ms; file content overrides the 22ms default) is
    // logged with its phase breakdown + GL texture-upload count — so a scroll judder shows *what* stalled
    // (high `pump`+`up` ⇒ synchronous poster uploads; high `swap` with low pump/draw ⇒ GPU fill).
    let framedrop = if controlled { None } else { crate::dev::scenarios::framedrop_value() };
    let framedrop_on = framedrop.is_some();
    let framedrop_thresh: f64 = framedrop
        .and_then(|s| s.parse().ok())
        .filter(|v: &f64| *v > 0.0)
        .unwrap_or(22.0);
    let instr = crate::diag::heartbeat::Instruments::new(framedrop_on, framedrop_thresh);

    let last_input = initial.as_ref().map_or_else(clock::now, |initial| initial.clock_start);
    let t0 = last_input;
    let loop_t = t0;
    let iters_ct = 0i32;
    let loop_shown = 0i32;
    // The dev number painted in the top-right corner. Unlike `loop_shown`, this is a real
    // presentation rate: the same completed-window value the heartbeat publishes as `fps=`.
    // It is updated only when that heartbeat drains PRESENTS, so pixels and logs cannot
    // disagree by observing two different counters. On a settled screen the number changes
    // only when the ordinary keepalive next buys a frame; the diagnostic must never defeat
    // the present gate merely to repaint itself.
    #[cfg(feature = "devtools")]
    let fps_shown = 0i32;
    // (media ns, SDL ticks) at the previous heartbeat, for `play=` below. `None` while
    // nothing is presenting, so the first beat of a playback reports no rate rather than a
    // fabricated one.
    let play_prev: Option<(i64, u32)> = None;
    let running = true;
    // Dev-only panel proof: advance a red/green counter phase only after SDL_GL_SwapWindow
    // returns. Hold each colour for 30 swaps: per-buffer alternation blends yellow at 60 Hz,
    // while this ~2 Hz change is human-visible and still freezes immediately with presentation.
    #[cfg(feature = "devtools")]
    let buffer_flip_count = 0u8;

    // All that is left of `HeldKey` on the loop's side (phase 10) — see `App::down_sym`.
    let down_sym = 0u32;
    // Item 13: rate-limits a hardware auto-repeat forwarded into the Settings family, which is
    // owned by the dispatcher since phase 5b — so the gate is applied at the loop's hand-over,
    // to the DIRECTIONS only. See `run`'s auto-repeat arm for why the OK edges go through
    // ungated, and `on_auto_repeat`'s doc for the one thing left on the legacy side (the
    // deferred press's liveness beat, and nothing else since phase 12).
    let modal_repeat = RepeatGate::IDLE;
    let marker_tried = false; // dev: the /tmp/plxnative-marker jump has been resolved
    let player = crate::player::machine::Player::new();
    // The token stops being an argument here and becomes a field. `PlayerAdapter::new` CONSUMES
    // it, so the adapter is the only thing in the process that holds one, and a `&mut` to it is
    // what every native-session call now asks for. See `player::adapter`.
    let adapters = super::Adapters {
        player: crate::player::adapter::PlayerAdapter::new(mt),
    };
    let repause_at = 0i64;
    // ui::press click state: a grid-card OK is deferred (press-in on down, activate on the
    // spring-back after key-up) so `ok_armed` marks "a press is in flight, commit it from the
    // per-frame loop when press::take_commit fires". Only ever set on Home's grid.
    let ok_armed = false;
    // Which route name was last REPORTED as an event. Not `route` itself: several `Route`
    // values share one name (every `Route::Player` is "player"), and an overlay
    // opening is not a screen change.
    let last_route_reported: &'static str = "";
    let press_tried = false; // dev: /tmp/plxnative-press fires one simulated grid-card press
    let press_release_at = 0u32; // …and the tick at which that simulated press releases
    let itemmenu_tried = false; // dev: /tmp/plxnative-itemmenu opens the card context menu once
    let acct_tried = false; // dev: /tmp/plxnative-acct opens the profile menu once
    let ptr = Pointer::IDLE;

    // Initial route from the boot gate: Login when we have no usable creds, Profiles for the
    // boot who's-watching picker, else Home.
    //
    // …and Home is intercepted by the first-run question when this profile has never been
    // asked it and the roster holds more than one source (`screens::onboard`). It belongs HERE as
    // well as on the login path, because a single-Plex-Home-user account never meets the
    // picker at all: the two paths into Home are the picker's `take_ready` and this gate, and
    // a question asked on only one of them is a question half the accounts never see.
    // `install_pms` above has already registered the stored roster, so "more than one source"
    // has a real answer by this line. An AUTOMATED boot is exempt for the reason the picker is
    // — a harness run must land on a deterministic Home.
    //
    // dev: `/tmp/plxnative-firstrun` forces it — a screen that is by definition asked once is
    // otherwise unreachable the moment you have answered it, and the two-source roster it
    // needs comes from `/tmp/plxnative-servers`, which marks the boot automated. Both halves
    // are why looking at this screen headlessly requires a trigger of its own.
    let ask_first_run =
        || !controlled && (crate::dev::scenarios::firstrun_armed() || (!automated_boot() && crate::screens::onboard::asks()));
    // The sign-in's telemetry question is PRESENTED on the container tree, and the tree lives on
    // the `App` this function is still assembling — so this boot arm records that it owes the
    // question and `maybe_ask_consent` is called once the struct exists, a few dozen lines down.
    // Deferring it changes nothing about when it is ASKED: the surface would not have drawn until
    // the loop's first frame either way, and the route below is a page underneath it.
    let mut owes_consent_question = false;
    let route = match boot_to {
        // **Both Home arms ask, and the shared call is the point.** This is the one boot that
        // has no earlier hook — an install already signed in, either never asked or asked
        // against an older policy — and the sign-in's question has to come before every
        // per-profile step, the Home-sources wizard included. Asking only in the second arm
        // (which is what shipped for an hour) meant a stored session that still owed the
        // sources answer walked Onboard → Home and was never asked at all.
        BootTo::Home => {
            owes_consent_question = true;
            if ask_first_run() {
                log("boot: asking which sources feed Home");
                // No `enter()`: the first-run editor is an OWNED screen, and naming the route is
                // the whole of mounting it — `bridge`'s mounter builds `OnboardScreen::first_run`
                // when the tree follows this route on the loop's first NAV COMMIT.
                AppArg::Onboard
            } else {
                AppArg::Home
            }
        }
        // Both of these enter the `Route::Login | Route::Profiles` block below, which asks as
        // soon as the account is authorized — earlier than here, and before the picker.
        BootTo::Login => AppArg::Login,
        BootTo::Profiles => AppArg::Profiles,
    };
    // (dev: /tmp/plxnative-acct used to open the profile menu HERE, beside a
    // `route = Route::Account { over: BarHost::Home }`. The menu is a `ModalStack` surface since
    // phase 10 and the container does not exist yet at this point in the boot, so the trigger is
    // an ordinary per-frame arm — `dev::scenarios::acct_arm`, beside `itemmenu_arm`.)
    // Home is the product landing after the credential gates; its Hero / Continue Watching
    // rows own resume. Never override this route from an old last-page bookmark. The cleanup is
    // intentionally unconditional so automated and ordinary upgrades retire the same state.
    crate::coldstart::retire();
    // (`play_from`, the BACK trail and `nav_pending` were three run-loop locals here — the page
    // the live session returns to, the pages behind the one on screen, and the route change a fade
    // is carrying. All three are the container's since restructure phase 12: `PlayerScreen::origin`
    // is an `EntryId`, the `NavStack` IS the history, and a pending op lives on it under a
    // `PageDip`.)

    let auto_tried = false;
    // dev: `/tmp/plxnative-replay[=N]` — how many times a finished `plxnative-playurl`
    // playback may be started AGAIN (LG App Self Checklist #46, "replay after completion").
    //
    // A COUNTER re-arming `auto_tried`, rather than the latch being lifted: `auto_tried` also
    // guards the `autoplay`+`playidx` arm below, which does a `request_play_movie` +
    // `load_detail_now`, so an unconditionally re-armable latch would re-fetch a catalog item
    // on every player exit and loop a real playback forever. Bounded and opt-in instead — an
    // absent file is 0, which leaves every existing boot byte-identical, and every pipeline
    // case but the one that asks for a replay is untouched.
    //
    // Why the app needs this at all: the synthetic tier boots with NO Plex session, so after a
    // stream ends there is no detail page, no Play control and no key path back into the
    // player. Everything else was already in place — `teardown` clears the URL and `ended` on a
    // real stop, and `engine::start_bufferfeed` re-reads `dev::playurl()` whenever
    // `route::url()` is empty — so a replay is a second trip through the entry below.
    let replay_left: u32 = if controlled { 0 } else { replay_budget(crate::dev::scenarios::replay_trigger_value().as_deref()) };
    let grid_tried = false;
    let settings_tried = settings_boot.is_none();
    let seek_tried = false;
    // /tmp/plxnative-autoseek seek script (see the parse site): pending steps, the tick of
    // the last fired step, the gap between steps, and the last REQUESTED target (the base
    // for "+10"/"-10" tap-relative steps, like taps on the HUD's frozen scrub playhead).
    let seek_script: Vec<String> = Vec::new();
    let seek_script_at = 0u32;
    let seek_gap_ms = 300u32;
    let seek_script_last = 0i64;
    // /tmp/plxnative-qualityswitch: the rungs still to switch to, the tick of the last one
    // fired, and the gap between them. Same shape as the seek script above, for the same
    // reason — a person changing quality mid-playback does it more than once.
    let quality_script: Vec<crate::plex::session::PlaybackQuality> = Vec::new();
    let quality_script_at = 0u32;
    let quality_gap_ms = 0u32;
    let quality_tried = false;
    let quality_playing_since: Option<u32> = None;
    let detail_tried = false;
    let play_tried = false;
    let menu_tried = false;
    let menupick_tried = false;
    let menupick_row = None;
    let pause_tried = false;
    // `/tmp/plxnative-autopause`: an authored Pause edge, plus the optional Resume edge which
    // owns the same script. External effects retry until the synchronized player state machine
    // accepts them; a busy native transition cannot silently consume the test operation.
    let pause_script: Option<(u32, Option<u32>)> = None;
    let pause_resume_at: Option<u32> = None;
    let prev = 0u32;
    // Home data refresh, armed on every player exit (Stop/BACK/EOS): the hubs are refetched a
    // beat later so the final timeline PUT lands first — Continue Watching then shows the new
    // resume point / next episode instead of the state from boot.
    let refresh_hubs_at = 0u32;

    let ev = [0u8; 128];
    // dev/testing remote: drain any tokens written to /tmp/plxnative-remote and push
    // them as synthetic key events BEFORE the poll loop, so they're consumed this frame
    // by the ONE real key handler (see crate::remote / tools/stream-screen.py).
    let remote = crate::remote::Remote::open();
    // A LAB package may opt into the outbound long-poll command channel. Start only now: curl
    // has been initialised and, unlike the earlier boot/discovery work, the SDL loop below is
    // ready to dispatch a delivered command within one frame. Compile-time no-op otherwise.
    if !controlled { crate::lab::start_control(); }
    let mut app = App {
        last_input,
        loop_t,
        iters_ct,
        loop_shown,
        #[cfg(feature = "devtools")]
        fps_shown,
        play_prev,
        running,
        window_activity: crate::system::WindowActivity::new(),
        #[cfg(feature = "devtools")]
        buffer_flip_count,
        down_sym,
        modal_repeat,
        diagnostics: crate::app::diagnostics::Diagnostics::default(),
        player,
        adapters,
        repause_at,
        ok_armed,
        last_route_reported,
        ptr,
        menu_play_await: None,
        prev,
        refresh_hubs_at,
        ev,
        remote,
        win,
        t0,
        instr,
        measure_fault_logged: false,
        input: crate::ui::input::Input::new(),
        rec: super::recorder::Recplay::Off,
        boot_initial: initial,
        telemetry_guard: None,
        present: crate::ui::present::Present::new(),
        glass,
        // **The application's page stack runs the route DIP** (§6.2). It ran `Immediate` until
        // phase 12 while `ui::nav` held a second fader and the loop applied its own route change
        // at THAT floor; `PageDip` is the same schedule in the container that owns the op.
        pages,
        inputs: Vec::new(),
        bridge,
        // Every dev-trigger arm's own state (spec: `dev/scenarios.rs`'s module doc).
        scenarios: crate::dev::scenarios::Scenarios {
            pick_user,
            home_osc_last,
            hero_osc_last,
            home_fold_osc_last,
            home_fold_down,
            lib_osc_last,
            lib_switch_last,
            lib_switch_step,
            search_osc_last,
            settings_osc_last,
            settings_osc_down,
            modal_osc_last,
            legal_doc_tried,
            alert_tried,
            alert_step,
            account_osc_last,
            account_osc_down,
            consent_osc_last,
            consent_osc_down,
            onboard_osc_last,
            onboard_osc_right,
            nav_osc_last,
            marker_tried,
            press_tried,
            press_release_at,
            itemmenu_tried,
            acct_tried,
            auto_tried,
            replay_left,
            grid_tried,
            settings_tried,
            seek_tried,
            seek_script,
            seek_script_at,
            seek_gap_ms,
            seek_script_last,
            quality_script,
            quality_script_at,
            quality_gap_ms,
            quality_tried,
            quality_playing_since,
            detail_tried,
            content_boot: None,
            play_tried,
            play_await: None,
            menu_tried,
            menupick_tried,
            menupick_row,
            pause_tried,
            pause_script,
            pause_resume_at,
            dev: crate::dev::scenarios::DevFlags {
                detail_osc,
                home_osc,
                hero_osc,
                home_fold_osc,
                lib_osc,
                lib_switch,
                search_osc,
                settings_boot,
                settings_osc,
                modal_osc,
                legal_doc,
                alert_boot,
                account_osc,
                consent_osc,
                onboard_osc,
                nav_osc,
                nav_osc_rk,
                glass_hz_armed,
                nobudget,
            },
        },
    };
    // dev: /tmp/plxnative-nobudget — put the ONE frame budget (the tree's, spec §2.2) into its
    // pre-phase-11 shape for the A/B's control leg. The tree exists now, which is why this is
    // here rather than beside the trigger read.
    if app.scenarios.dev.nobudget {
        app.pages.budget = crate::ui::frame::Budget::pre_phase_11();
        crate::log("budget: pre-phase-11 admission (quota only) by /tmp/plxnative-nobudget");
    }
    if controlled {
        let initial = app.snapshot_init().expect("controlled constructor retains initial inputs");
        initial.validate().map_err(|reason| { log(&format!("rec: REFUSED — {reason}")); 1 })?;
        let replay = preflight.replay();
        app.rec = super::recorder::Recplay::controlled(preflight, &initial)
            .map_err(|reason| { log(&format!("rec: REFUSED — {reason}")); 1 })?;
        log("bootstrap: captured pre-effect initial state");
        if let Some(deferred) = deferred {
            apply_deferred_capture(&mut app.rec, deferred)
                .map_err(|reason| { log(&format!("rec: REFUSED — {reason}")); 1 })?;
            log("bootstrap: captured session persistence applied");
        }
        if !replay {
            app.telemetry_guard = Some(crate::telemetry::activate_initial(initial.consent.clone()));
        }
        if !replay { super::adapters::poster::init(); }
        app.rec.tick(initial.clock_start, 0.0);
        app.rec.prepare_resources(&mut app.bridge);
        if !matches!(activate_session(&mut app.bridge, &mut app.pages, &mut app.rec), BootTo::Home) {
            log("bootstrap: controlled Home activation refused");
            return Err(1);
        }
        let tree = app.pages.state_hash();
        app.rec.resource_requests(app.bridge.take_resource_requests());
        if let Some(reason) = app.rec.failure().or_else(|| app.bridge.controlled_failure()) {
            log(&format!("bootstrap: REFUSED — {reason}"));
            return Err(1);
        }
        super::run::recorder_end_frame(&mut app.rec, &app.bridge, &app.input.press,
            "home", "", "bootstrap", tree);
        if let Some(ms) = app.rec.clock_start() {
            super::clock::set_replay(ms);
            app.t0 = ms;
            app.loop_t = ms;
            app.last_input = ms;
        }
    }
    // **ROOT THE TREE AT THE BOOT PAGE.** The `route` chosen above used to be a field of `App`
    // that `bridge::sync_page` mirrored onto the container on the loop's first frame; there is no
    // mirror, so the boot says what it wants once, here, the moment the tree exists. A `Root` on
    // an empty stack mints the first entry, which is what makes this a hard CUT — there is no
    // outgoing screen to dip.
    if controlled {
        app.pages.emit(crate::ui::machine::MachineId::Nav,
            crate::ui::machine::Fx::Nav(crate::ui::machine::NavOp::Root(route)));
    } else {
        super::bridge::nav_root(&mut app.pages, route);
    }
    if controlled { log(&format!("bootstrap: root-request tree={:016x}", app.pages.state_hash())); }
    // …the deferred half of the `BootTo::Home` arm above: the tree exists now, so the question can
    // be presented. Idempotent and cheap (`should_show` is false once a decision is recorded and
    // on any automated boot), so the flag is the only thing carrying the decision forward.
    if owes_consent_question {
        if let Some(initial) = &app.boot_initial {
            // The same policy, evaluated over captured consent and automation inputs. A
            // recplay trigger alone intentionally does not grant live automation authority.
            // Preflight refuses the still-unsupported first-run surface before resource boot.
            if crate::screens::consent::should_show(&initial.consent, initial.automated) {
                log("bootstrap: REFUSED — unsupported initial consent route");
                return Err(1);
            }
        } else {
            maybe_ask_consent(&mut app.pages);
        }
    }
    Ok(app)
}
