//! Boot-time helpers of the app core: the desktop window size, the panic logger, the replay
//! budget, the direct-server trigger, the cursor and the loop-rate tick. Moved out of `app.rs`
//! verbatim in phase 1a of the UI restructure (a pure move; `pub(super)` widening only).

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
pub(super) fn desktop_window_size() -> (c_int, c_int) {
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
pub(super) fn install_panic_logger() {
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
pub(super) unsafe fn hide_cursor() {
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
pub(super) fn loop_tick(iters_ct: &mut i32, loop_t: &mut u32, loop_shown: &mut i32, now: u32) -> bool {
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
pub(super) fn replay_budget(raw: Option<&str>) -> u32 {
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
pub(super) fn resolve_direct_server(
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

pub(super) fn direct_trigger_server() -> Result<crate::plex::ServerId, String> {
    resolve_direct_server(
        crate::dev::server_slot(),
        crate::plex::current_server(),
        |sid| crate::plex::client_for(sid).is_some(),
    )
}

// ---- boot, and the loop's own between-frame state ---------------------------------------------
/// Which screen the boot gate landed on — see the gate itself in `plex_run`, which is where the
/// order of its four cases is argued.
pub(super) enum BootTo {
    Home,
    Login,
    Profiles,
}

    // Everything that has to happen when the server `plex::client()` answers with CHANGES —
    // whether because a new identity signed in or because the user walked into another source.
    // EVERY store below is keyed to whichever server was current when it was filled, and none
    // of them carries a server in its keys, so leaving one behind means server A's ratingKeys
    // being fetched from server B: the same catalog index opening a different film.
pub(super) fn activate_server() {
        // the browse store must never carry the previous user's (or server's) cached grid,
        // watched-state angles, or section tabs forward
        crate::browse::reset();
        // …and the search store, for the same reason: a query, its results and the recent
        // terms are all one person's.
        crate::search::reset();
        // …and the hub twin: a FAILED fetch now keeps the catalog it already had (so one
        // wifi hiccup can't blank a populated Home), which makes this the one place that
        // must still wipe it — otherwise a profile switch whose fetch fails would leave the
        // previous user's shelves on screen.
        crate::pms::reset();
        crate::person::reset(); // ditto for an open person page's shelves
                                // …and any view-state write still queued or owed a refresh. It belongs to the account
                                // that pressed it, and the refresh it owes would land on shelves this reset just wiped.
        crate::viewstate::reset();
        // Catalog activation is request-only. Home and section discovery both use their
        // existing worker/mailbox pumps, so a remote endpoint cannot park the SDL loop here.
        crate::pms::request_refetch_hubs();
        crate::browse::discover_pump();
        log("pms: catalog activation queued");
}

    // Install the PMS client (the read layer AND the playback path) as the CURRENT server,
    // then fetch the catalog. Used by the boot gate and again when a login resolves; a later
    // call for the same address just swaps the token (profile switch).
    // Takes an ORIGIN and not a `(host, port)` pair: the pair cannot say `https`, and the host
    // a certificate is issued for is the `plex.direct` NAME rather than the address behind it
    // (`plex::origin`). Discovery and persisted sessions may supply either scheme; the client
    // routes control and media requests through the matching transport.
pub(super) fn install_pms(
    origin: &crate::plex::Origin,
    token: &str,
    tier: Option<crate::plex::probe::Location>,
    pin: Option<&crate::plex::ResolvePin>,
) {
        crate::plex::install(origin, token, pin); // a (re)install is a login / profile switch
                                             // `install` may re-point the slot by publishing a fresh Client, whose link starts
                                             // unknown. Restore the persisted/raced winner only after that publication.
        if let Some(link) = tier {
            crate::plex::client()
                .set_connection(link, crate::plex::IpVersion::of_host(origin.host()));
        }
        // Every additional server this boot was handed credentials for joins the REGISTRY
        // beside it — the granted roster `browse` addresses its section table by. Registration
        // is not activation: `install` above has already made the session's own server current,
        // and `register` deliberately does not steal that, so a share appears as a source to
        // browse rather than as a server the app has switched to.
        //
        // AFTER `install`, so slot 0 is always the session's own server and the roster reads in
        // the order the Sources list wants to draw it. Registering here (rather than at the boot
        // gate) also means a profile switch re-registers them, which is what keeps a share in
        // the roster across a switch — and it must precede `activate_server`, whose refetch is
        // what turns a newly registered source into shelves and section tabs.
        for s in crate::dev::servers()
            .unwrap_or_default()
            .iter()
            .filter(|s| s.usable())
        {
            // `usable()` IS `origin().is_some()`, so this `else` cannot be taken; it is a
            // `continue` rather than an `expect` because an injected server has never been
            // allowed to cost more than itself.
            let Some(origin) = s.origin() else { continue };
            let id = crate::plex::register_origin(
                &s.machine_id,
                &origin,
                &s.token,
                s.resolve_pin().as_ref(),
            );
            if let Some(tier) = s.tier {
                // The endpoint may be a LAN conditioner in front of a Remote PMS.  Preserve
                // the discovery fact the harness supplied; the private proxy address itself
                // cannot prove Local, and an omitted tier deliberately proves nothing.
                if let Some(client) = crate::plex::client_for(id) {
                    client.set_link(tier);
                }
            }
            // the roster's own answer about this server: a handle means someone else's.
            crate::plex::describe_server(id, &s.name, &s.handle, s.handle.is_empty());
        }
        activate_server();
}

/// Everything before the loop: SDL and the window, GL, text, the poster workers, the boot gate
/// (login / token / session / picker), every dev trigger read once, and the `App` literal —
/// `plex_run`'s former body up to `while app.running`, moved verbatim in phase 1b-ii. An early
/// exit is the process exit code `plex_run` returns.
pub(super) unsafe fn boot(
    pms_host: *const c_char,
    pms_port: c_int,
    mt: &crate::task::MainThread,
) -> Result<App, c_int> {
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
    SDL_GL_SetSwapInterval(if crate::dev::flag("novsync") { 0 } else { 1 });
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
    crate::telemetry::flush_soon();

    // NO token is compiled into this binary. PMS access comes from the signed-in session,
    // or — for automated runs only (the regression harness, headless captures) — from the
    // /tmp/plxnative-token dev trigger. The value is NEVER logged (only that one is in effect).
    let dev_token = match crate::dev::read("token") {
        Some(s) if !s.is_empty() => {
            log("token: using /tmp/plxnative-token (test identity)");
            s
        }
        _ => String::new(),
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
    match crate::dev::servers() {
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


    // UI infra + poster workers always come up — the login/profiles screens use them too.
    crate::posters::posters_init();
    crate::capture::init(); // dev live UI capture stream (no-op without /tmp/plxnative-capture)
    crate::ui::home::home_init();
    crate::ui::login::init();
    crate::ui::profiles::init();

    // Any dev trigger under /tmp marks the boot as automated (the harness token override,
    // autoplay/detail captures, playback-path knobs): those runs need a deterministic Home,
    // so the boot who's-watching picker is skipped. Pure diagnostics (the logs, the profiler,
    // the anim overlay) don't count as automation.
    // The scan itself, and the DIAG exemption list that decides what does NOT count as
    // automation, live in `dev::any_trigger_present` — together with the `plxnative-anim.log`
    // bug that list was rewritten for. It is the one dev-trigger surface that names no file,
    // so it is also the one a release build had to be taught about explicitly.
    let automated_boot = || crate::dev::any_trigger_present();

    // Boot gate. Order matters:
    //  1. /tmp/plxnative-login forces the QR login screen (to exercise the flow on demand).
    //  2. /tmp/plxnative-token (the harness / headless runs) beats the stored session — automation
    //     must run as the injected test identity no matter who is signed in on the TV.
    //  3. A stored session (offline-capable LAN server) → Home, through the who's-watching
    //     picker first when the account has a multi-user Plex Home roster (interactive boots).
    //  4. Nothing → the QR sign-in flow (no credentials are compiled in — like a real client).
    // The destination itself is [`BootTo`], at module scope with the rest of the vocabulary.
    //
    // dev: /tmp/plxnative-pickuser=<index> — force the boot picker even on an automated boot and
    // auto-select that roster tile once it's up (headless exercise of the who's-watching flow).
    let pick_user: Option<usize> =
        crate::dev::read("pickuser").and_then(|s| s.parse().ok());
    let session = crate::plex::session::load();
    // Install-wide playback preference, restored before any route can resolve a stream.
    // A legacy file with no value resolves to Original; a new file can choose Auto only
    // through route's explicit readiness gate (session::load records that decision once).
    crate::route::restore_quality(
        crate::dev::playback_quality_override().unwrap_or_else(|| session.playback_quality()),
    );
    let boot_to = if crate::dev::flag("login") {
        crate::auth::start_login();
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
        install_pms(
            &crate::plex::Origin::http(&host_s, pms_port),
            &dev_token,
            Some(tier),
            None,
        );
        BootTo::Home
    } else if session.can_go_local() {
        if session.home_users.len() > 1 && (!automated_boot() || pick_user.is_some()) {
            // Who's watching first. Only the read client is installed here (the avatars proxy
            // through the PMS photo transcoder); the catalog fetch + playback config happen in
            // take_ready once a profile is picked — done now they'd be thrown out on a switch.
            crate::plex::install(
                &session.server.origin(),
                session.pms_token(),
                session.server.resolve_pin().as_ref(),
            );
            crate::plex::session::set_current(Some(session.user.clone()));
            // seeds the persisted roster + refreshes it online. `Picker::Boot` is what makes
            // BACK out of this picker refuse to reinstate a PIN-protected profile — nobody has
            // identified themselves yet, so there is no "carry on as me" to fall back on.
            crate::auth::start_switch(crate::auth::Picker::Boot);
            log("boot: stored session — who's watching");
            BootTo::Profiles
        } else {
            // The persisted roster FIRST, then the primary. This is the one boot path that does
            // not go through `auth::start_switch` — a stored session with a single Plex Home
            // user, or any automated run — so without this line it registered exactly one
            // server and every share was invisible until the next sign-in: no second source in
            // the Sources panel, no borrowed shelves, nothing to attribute. `install_roster`
            // leaves `current` alone and sorts owned first, and `install_pms` below retargets to
            // the session's own server regardless, so ordering cannot land us on a friend's box.
            // Before, not after, because `install_pms` ends in the catalog + section fetch that
            // turns a registered source into something on screen.
            crate::auth::install_stored_roster(&session);
            // WHO is watching, before anything reads a per-profile store. It drives the Home
            // profile chip, and it is also what `browse::resolve_pins` and
            // `ui::search::recents` key on — `install_pms` below ends in the section fetch
            // that resolves the Home selection, so set after it that resolve ran against the
            // OWNER's record whoever was actually signed in. (`auth::take_ready`, the other
            // way into Home, already sets it before its own `install_pms` for this reason.)
            crate::plex::session::set_current(Some(session.user.clone()));
            install_pms(
                &session.server.origin(),
                session.pms_token(),
                session.server.tier,
                session.server.resolve_pin().as_ref(),
            );
            // Re-learn the roster only AFTER the spawn-time primary snapshot was installed.
            // If a fast refresh re-pointed first, installing that stale snapshot afterwards
            // put the dead origin back into the live registry for the rest of this run.
            // Non-destructive on failure; the stored roster above remains available offline.
            crate::auth::refresh_roster();
            log("boot: stored session — local server (offline-capable)");
            BootTo::Home
        }
    } else {
        crate::auth::start_login();
        log("boot: no session — starting QR sign-in");
        BootTo::Login
    };
    crate::player::acb_init(mt);
    crate::ff::boot(); // FFmpeg version smoke test + optional /tmp/plxnative-ffprobe ABI probe
                       // dev: /tmp/plxnative-logintest validates the plex.tv account path end-to-end on the device — a
                       // real typed create_pin() through the libcurl transport + DTO deserialize. Logs only the
                       // public pin id + code length + that authToken is still null (never a token/secret).
    if crate::dev::flag("logintest") {
        let _ = crate::task::spawn_small("logintest", || {
            let sess = crate::plex::session::load();
            let ac = crate::plex::account::AccountClient::new(&sess.client_id, None);
            match ac.create_pin() {
                Some(p) => log(&format!(
                    "logintest: create_pin ok id={} code_len={} authToken_null={}",
                    p.id,
                    p.code.len(),
                    p.auth_token.is_none()
                )),
                None => log("logintest: create_pin FAILED (transport/TLS/link/deser)"),
            }
        });
    }
    // dev: the animation-diagnostic overlay is OFF by default; /tmp/plxnative-anim enables it (its
    // trace goes to /tmp/plxnative-anim.log, a separate stream from the main event log)
    if crate::dev::flag("anim") {
        crate::ui::anim::set_enabled(true);
    }
    // dev: profile is asynchronous EXT_disjoint_timer_query timing; hwcnt is the serialized
    // direct Mali counter-attribution run. Their content names ONE phase (empty = frame.ui).
    // Combining them would perturb the timer result, so fail closed when both are present.
    // dev: /tmp/plxnative-glassload is the backdrop-glass LOAD DIAL — a sweep of glass-surface
    // count, size and refresh cadence that cycles its own steps inside one launch, so legs are
    // interleaved by construction. /tmp/plxnative-navblur is the blurred-route-transition
    // prototype. Both live in `ui::glassload`; both are absent from a release build.
    if let Some(v) = crate::dev::read("glassload") {
        crate::ui::glassload::configure(&v);
    }
    if let Some(v) = crate::dev::read("navblur") {
        crate::ui::glassload::configure_navblur(&v);
    }
    // dev: the two OVERDRAW surfaces (`ui::overdraw`, docs/backdrop-blur-profiling.md Part 5).
    // `plxnative-overdraw` arms the CPU-side per-draw-class ledger — how much screen-visible
    // quad area this app submits, per primitive family, per frame. It is not billed for the
    // wayland compositor's work and is not `glFinish`-serialised, which is what the GPU's
    // global FRAG_QUADS_RAST cannot say. `plxnative-drawmask=<classes>` REFUSES every draw of
    // the named classes, so a whole-frame `frame.ui` A/B against the unmasked control prices
    // that class as the frame sees it; `all` draws nothing and is therefore the compositor
    // floor. A masked leg is a broken picture on purpose.
    if crate::dev::flag("overdraw") {
        crate::ui::overdraw::set_ledger(true);
    }
    if let Some(spec) = crate::dev::read("drawmask") {
        crate::ui::overdraw::set_mask(&spec);
    }
    // dev: /tmp/plxnative-heroground — draw the hero's photograph and BOTH of its scrim fields
    // in one pass instead of the art plus four blended gradient quads over it. Absent, the
    // shipped four-quad path draws, which is what makes this an A/B on one binary.
    if crate::dev::flag("heroground") {
        crate::ui::widgets::set_hero_ground(true);
        log("hero: one-pass ground ENABLED by /tmp/plxnative-heroground");
    }
    // dev: /tmp/plxnative-glasshz=<presents-per-refresh> moves the shared dynamic-backdrop
    // cadence for the cost curve in `docs/backdrop-blur-profiling.md` — 1 is a refresh on every
    // present (60 Hz while the UI presents at 60), 3 is ~20 Hz, 4 is 15 Hz.
    // ABSENT, nothing here runs and the cadence is exactly the shipped one. It is a profiling
    // knob, so it also turns on the heartbeat's `snap=` field (refreshes per second), which
    // is the only way to check the cadence that RAN against the one that was asked for — and
    // The production Account menu no longer arms or consumes this path: its host is frozen and
    // its glass snapshot is cached for the whole open lifetime.  The knob remains for explicit
    // material profiling, not as part of an Account FPS scene.
    let glass_hz_armed = if let Some(v) = crate::dev::read("glasshz") {
        let asked: u32 = v.parse().unwrap_or(0);
        let got = crate::ui::widgets::set_dynamic_period(asked);
        log(&format!(
            "blur: dynamic cadence asked={asked} presents-per-refresh={got}"
        ));
        true
    } else {
        false
    };
    match (crate::dev::read("profile"), crate::dev::read("hwcnt")) {
        (Some(_), Some(_)) => {
            log("PROFILE disabled: remove either /tmp/plxnative-profile or /tmp/plxnative-hwcnt");
        }
        (Some(filter), None) => crate::ui::profile::set_enabled(&filter),
        (None, Some(filter)) => crate::ui::profile::set_hwcnt_enabled(&filter),
        (None, None) => {}
    }
    // dev: /tmp/plxnative-cpuprof — the render thread's OWN time per phase, every phase at
    // once, no glFinish. The one mode that can see a frame the frame-drop detector reports as
    // all `draw=` and no `swap=`; the two GPU modes above are blind to it by construction.
    if crate::dev::flag("cpuprof") {
        crate::ui::profile::set_cpu_enabled();
    }
    // dev: /tmp/plxnative-noidle turns the whole-frame present gate (ui::idle) OFF, so a still
    // screen goes back to repainting at panel rate. It is a DIAG trigger (see the list above)
    // precisely so an A/B costs one file and does not also change which screen you boot to —
    // and so that if a frame ever looks wrong on the panel, ruling this feature out is one
    // `rm` rather than a redeploy.
    crate::ui::testpat::boot();
    crate::player::seed_dev_track_names();
    if crate::dev::flag("noidle") {
        crate::ui::idle::set_enabled(false);
        log("idle: present gate DISABLED by /tmp/plxnative-noidle");
    }
    // dev: /tmp/plxnative-detailosc (read once at boot, like the other triggers) makes the detail scroll
    // perpetually swing hero<->bottom so the FPS heartbeat samples the transition, not the ends.
    let detail_osc = crate::dev::flag("detailosc");
    // dev: /tmp/plxnative-homeosc — perpetually sweep the home grid focus DOWN to the bottom then
    // UP to the top (~3s each way, one row per 350ms), so a headless run reproduces the top↔bottom
    // vertical-scroll judder for the frame-drop detector / retui profiler.
    let home_osc = crate::dev::flag("homeosc");
    let home_osc_last = 0u32;
    // dev: the two Home transition scenes the old home-hero/home-grid pair could not see.
    // `heroosc` continuously pages the real carousel; `homefoldosc` alternates the real
    // hero↔first-shelf snap. Their intervals overlap the spring lifetime so the FPS heartbeat
    // samples motion rather than the efficient idle gaps at either end.
    let hero_osc = crate::dev::flag("heroosc");
    let hero_osc_last = 0u32;
    let home_fold_osc = crate::dev::flag("homefoldosc");
    let home_fold_osc_last = 0u32;
    let home_fold_down = true;
    // dev: /tmp/plxnative-libosc — the Library twin of homeosc: sweep the browse grid focus
    // down↔up perpetually for the library_scroll FPS scene.
    let lib_osc = crate::dev::flag("libosc");
    let lib_osc_last = 0u32;
    // dev: /tmp/plxnative-libswitch — exercise EVERY Library switch on a timer (tab switch,
    // sort menu open/move/close, unwatched on/off, filter open/close) for the library_switch
    // FPS scene, so the re-query + popover paths are perf-gated, not just the scroll.
    let lib_switch = crate::dev::flag("libswitch");
    let lib_switch_last = 0u32;
    let lib_switch_step = 0u32;
    // dev: /tmp/plxnative-searchosc — the Search twin of homeosc/libosc: sweep the result
    // shelves' focus down↔up perpetually for the `fps:search-type` scene. It does NOT reach the
    // screen on its own — pair it with `/tmp/plxnative-search=<query>`, and with a query the
    // library actually matches, or there are no shelves to sweep and the scene grades nothing.
    let search_osc = crate::dev::flag("searchosc");
    let search_osc_last = 0u32;
    // dev: /tmp/plxnative-settings=<root|home|privacy|legal> opens the Settings modal (and,
    // optionally, one of its real child panels) once Home is available. `settingsosc` turns
    // that settled modal into a continuous render-throughput scene: it alternates the focused
    // row and explicitly keeps the present gate awake. Without the latter an efficient,
    // completely healthy modal intentionally reports ~0 fps after its springs settle, which
    // cannot grade the screen's fill cost. The paired settings-idle scene omits the oscillator
    // and guards the inverse contract.
    let settings_boot = crate::dev::read("settings");
    let settings_osc = crate::dev::flag("settingsosc");
    let settings_osc_last = 0u32;
    let settings_osc_down = true;
    // dev: /tmp/plxnative-modalosc — with `plxnative-settings=root`, OPEN and DISMISS the
    // Settings modal every 1500 ms through the same `open`/`on_back` the chip and BACK use, so
    // `fps:modal-ramp` grades the appear/disappear RAMP (host snapshot, scrim, ground) under
    // `worst_ceiling_ms` rather than a settled modal. It reverses on a clock because the ramp
    // itself has no end the app reports.
    let modal_osc = crate::dev::flag("modalosc");
    let modal_osc_last = 0u32;
    // dev: /tmp/plxnative-legaldoc — with `plxnative-settings=legal`, press OK on the Legal
    // index ONCE so the boot lands on a pushed DOCUMENT (the reader over the frozen ground),
    // which no boot trigger reached before: `fps:legal-document`.
    let legal_doc = crate::dev::flag("legaldoc");
    let legal_doc_tried = false;
    // dev: /tmp/plxnative-alert — with `plxnative-settings=privacy`, open the "Delete all local
    // data?" DECISION ALERT once the privacy panel is up. It is the one shared yes/no alert in
    // the app and nothing headless could reach it: `fps:decision-alert`. Opening it is all this
    // does — nothing is deleted, and Cancel is what a BACK would press.
    let alert_boot = crate::dev::flag("alert");
    let alert_tried = false;
    // The profile menu freezes its host and uses one cached backdrop. Drive the menu's own
    // TableView for a strict FPS scene; reusing `homeosc` would now correctly move nothing and
    // would grade the idle keepalive rather than the popover.
    let account_osc = crate::dev::flag("acctosc");
    let account_osc_last = 0u32;
    let account_osc_down = true;
    // First-run route oscillators keep their real focus models moving so the device FPS suite
    // grades the composition rather than a settled screen that correctly stops presenting.
    let consent_osc = crate::dev::flag("consentosc");
    let consent_osc_last = 0u32;
    let consent_osc_down = true;
    let onboard_osc = crate::dev::flag("onboardosc");
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
    let nav_osc_rk = crate::dev::read("navosc");
    let nav_osc = nav_osc_rk.is_some();
    let nav_osc_rk = nav_osc_rk.unwrap_or_default();
    let nav_osc_last = 0u32;

    // dev: /tmp/plxnative-framedrop — the FRAME-DROP DETECTOR. When present, each frame is timed with
    // the high-res perf counter (pump / draw / swap, NO glFinish so it doesn't perturb the pipeline),
    // and any frame whose total exceeds a threshold (ms; file content overrides the 22ms default) is
    // logged with its phase breakdown + GL texture-upload count — so a scroll judder shows *what* stalled
    // (high `pump`+`up` ⇒ synchronous poster uploads; high `swap` with low pump/draw ⇒ GPU fill).
    let framedrop = crate::dev::read("framedrop");
    let framedrop_on = framedrop.is_some();
    let framedrop_thresh: f64 = framedrop
        .and_then(|s| s.parse().ok())
        .filter(|v: &f64| *v > 0.0)
        .unwrap_or(22.0);
    let instr = crate::diag::heartbeat::Instruments::new(framedrop_on, framedrop_thresh);

    let last_input = clock::now();
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

    let held_key = HeldKey::IDLE;
    let scrubber = Scrub::IDLE;
    // Item 13: rate-limits a hardware auto-repeat (or a wheel tick) forwarded into
    // Settings/Consent/Legal's `on_updown`/`on_left_right` — see `on_auto_repeat`'s doc.
    let modal_repeat = RepeatGate::IDLE;
    let hud = HudState::IDLE;
    let marker_tried = false; // dev: the /tmp/plxnative-marker jump has been resolved
    let foreground = ForegroundLifecycle::IDLE;
    let repause_at = 0i64;
    // ui::press click state: a grid-card OK is deferred (press-in on down, activate on the
    // spring-back after key-up) so `ok_armed` marks "a press is in flight, commit it from the
    // per-frame loop when press::take_commit fires". Only ever set on Home's grid.
    let ok_armed = false;
    // Which route name was last REPORTED as an event. Not `route` itself: several `Route`
    // values share one name (every `Route::Player { overlay }` is "player"), and an overlay
    // opening is not a screen change.
    let last_route_reported: &'static str = "";
    let press_tried = false; // dev: /tmp/plxnative-press fires one simulated grid-card press
    let press_release_at = 0u32; // …and the tick at which that simulated press releases
    let itemmenu_tried = false; // dev: /tmp/plxnative-itemmenu opens the card context menu once
    let ptr = Pointer::IDLE;

    // Initial route from the boot gate: Login when we have no usable creds, Profiles for the
    // boot who's-watching picker, else Home.
    //
    // …and Home is intercepted by the first-run question when this profile has never been
    // asked it and the roster holds more than one source (`ui::onboard`). It belongs HERE as
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
        || crate::dev::flag("firstrun") || (!automated_boot() && crate::ui::onboard::asks());
    let mut route = match boot_to {
        // **Both Home arms ask, and the shared call is the point.** This is the one boot that
        // has no earlier hook — an install already signed in, either never asked or asked
        // against an older policy — and the sign-in's question has to come before every
        // per-profile step, the Home-sources wizard included. Asking only in the second arm
        // (which is what shipped for an hour) meant a stored session that still owed the
        // sources answer walked Onboard → Home and was never asked at all.
        BootTo::Home => {
            maybe_ask_consent();
            if ask_first_run() {
                log("boot: asking which sources feed Home");
                crate::ui::onboard::enter();
                Route::Onboard
            } else {
                Route::Home
            }
        }
        // Both of these enter the `Route::Login | Route::Profiles` block below, which asks as
        // soon as the account is authorized — earlier than here, and before the picker.
        BootTo::Login => Route::Login,
        BootTo::Profiles => Route::Profiles,
    };
    // dev: /tmp/plxnative-acct auto-opens the profile menu (headless capture of the popover).
    if crate::dev::flag("acct") && matches!(route, Route::Home) {
        crate::ui::account_menu::open();
        route = Route::Account {
            over: BarHost::Home,
        };
    }
    // Home is the product landing after the credential gates; its Hero / Continue Watching
    // rows own resume. Never override this route from an old last-page bookmark. The cleanup is
    // intentionally unconditional so automated and ordinary upgrades retire the same state.
    crate::coldstart::retire();
    // The page the live playback session was LAUNCHED FROM — where Stop/BACK/EOS returns to.
    // Kept OUTSIDE Route (like `foreground` keeps the suspended session): it is navigation
    // history, not the current node, and Route makes every page and Player exclusive so it
    // could not be encoded there. Captured per `start_playback` through `Origin` — see that
    // type for why this is a `Node` and not the `from_detail: bool` it replaced.
    let play_from = Node::Home;
    // The BACK trail (`ui::trail`): the pages behind the one on screen, top = current. It
    // replaces the `opened_from_library` / `opened_from_person` pair, which were a precedence
    // ladder with one slot per screen KIND and so could not describe a detail page standing on
    // another detail page — the episode filmstrip's text row and the Related shelf both do that,
    // and BACK from such a page fell through to Home. A run-loop LOCAL, exactly like the
    // booleans it replaces and like `play_from` beside it: navigation history belongs
    // to the loop that navigates.
    //
    // `play_from` deliberately does NOT fold into it. It answers a different question — where
    // does THIS SESSION return to — and is written per `start_playback` call from the route on
    // screen at the press, which is why `home_activate` opening a detail page under the hood
    // just to fire its Play still returns to Home: the user never left it. The app-switch path
    // depends on that independence (the background arm drops to Home without touching either).
    let trail = crate::ui::trail::Trail::new();
    // The route change the page cross-fade is carrying, applied at its floor. `None` whenever
    // no transition is in flight — which is every path that deliberately keeps today's hard cut
    // (a boot trigger, a player exit, the app-switch lifecycle, a login landing), so the
    // default really is "nothing changes".
    let nav_pending: Option<NavReq> = None;

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
    let replay_left: u32 = replay_budget(crate::dev::read("replay").as_deref());
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
    crate::lab::start_control();
    let mut app = App {
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
        account_osc_last,
        account_osc_down,
        consent_osc_last,
        consent_osc_down,
        onboard_osc_last,
        onboard_osc_right,
        nav_osc_last,
        last_input,
        loop_t,
        iters_ct,
        loop_shown,
        #[cfg(feature = "devtools")]
        fps_shown,
        play_prev,
        running,
        #[cfg(feature = "devtools")]
        buffer_flip_count,
        held_key,
        scrubber,
        modal_repeat,
        hud,
        marker_tried,
        foreground,
        repause_at,
        ok_armed,
        last_route_reported,
        press_tried,
        press_release_at,
        itemmenu_tried,
        ptr,
        route,
        play_from,
        trail,
        nav_pending,
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
        play_tried,
        menu_tried,
        menupick_tried,
        pause_tried,
        pause_script,
        pause_resume_at,
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
        dev: DevFlags {
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
        },
    };
    // The recorder / replay driver, armed ONCE, here, at the end of boot (spec §5.3): the
    // header's initial conditions are what this boot reached — route, sign-in, roster size,
    // consent — and nothing has ticked yet. Phase 2 honesty: the stores have already spawned
    // their first fetches above (they still talk to the network themselves until phase 4), so
    // a replay runs those LIVE against the same synthetic server and grades the machines.
    {
        let consent = crate::telemetry::consent::current();
        let init = super::recorder::AppInit {
            route: route_word(app.route),
            session: !crate::plex::session::peek().account_token.is_empty(),
            servers: crate::plex::server_count() as u32,
            consent_asked: consent.as_ref().map(|c| c.asked_version).unwrap_or(0),
            consent_errors: consent.as_ref().map(|c| c.errors).unwrap_or(false),
            consent_usage: consent.as_ref().map(|c| c.usage).unwrap_or(false),
            seed: 0,
        };
        app.rec = super::recorder::Recplay::arm(&init, crate::dev::armed_triggers());
        if let Some(ms) = app.rec.clock_start() {
            super::clock::set_replay(ms);
            app.t0 = ms;
            app.loop_t = ms;
            app.last_input = ms;
        }
    }
    Ok(app)
}
