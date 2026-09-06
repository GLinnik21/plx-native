//! plex_run — the Rust app core (was the body of src/main.c). Owns SDL init, the
//! event loop, input decode, the per-frame tick, draw orchestration, app lifecycle,
//! the buffer-feed pump orchestration, and the dev triggers. The C boot shim
//! (main.c) sets up the log and fallback crash tracer, calls the Rust image-marker and native-spool
//! entries when required, then calls `plex_run`. The only application subsystem left in C is the
//! starfish.c C++/ACB seam (the engine itself is Rust: crate::player).
#![allow(non_upper_case_globals)]
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::sync::atomic::Ordering::Relaxed;

// ---- constants (SDL 2.0.4 + GLES2 + app) ----
const SDL_INIT_VIDEO: u32 = 0x20;
/// Appended to every heartbeat on a simulator build, and empty on a television.
///
/// The heartbeat is the app's perf surface: `tests/run.py --fps` grades `loop=` and `fps=` from it,
/// and the floors are calibrated to the SM9000's Mali. A Mac renders the same interface through a
/// completely different GPU, driver and compositor, so those numbers are not merely optimistic —
/// they are about a different machine. A log line is the unit that gets pasted into an issue or
/// handed between agents, so the disclaimer has to travel ON the line rather than sit in a doc.
const SIM_TAG: &str = if cfg!(feature = "hostsim") {
    " sim=1"
} else {
    ""
};

/// OPENGL | FULLSCREEN on the television, which owns the whole panel.
///
/// The desktop asks for OPENGL | ALLOW_HIGHDPI — no fullscreen grab (hostile on a laptop) and
/// **not RESIZABLE**: `surface::probe` reads the drawable once, at boot, so a dragged edge would
/// leave the viewport describing a window that no longer exists, and the interface would sit in a
/// 1920x1080-shaped corner of the new one with every pointer hit landing somewhere else. The window
/// opens at an exact divisor of the canvas instead — see `desktop_window_size`.
///
/// ALLOW_HIGHDPI is what makes that divisor land on a **1:1 surface** on the Mac people actually
/// have: without it a Retina display gives a drawable equal to the window in POINTS, which the
/// compositor then doubles, so the whole interface is an upscale of a half-size render. With it,
/// the 960x540-point window `desktop_window_size` picks on a laptop has a 1920x1080 drawable —
/// `surface::scale() == 1.0`, the same 1:1 texel contract the television gets.
const SDL_WINDOW_FLAGS: u32 = if cfg!(feature = "hostsim") {
    0x2 | 0x2000
} else {
    0x2 | 0x1
};
/// `SDL_WINDOW_INPUT_FOCUS`. Note it is NOT among the flags requested above — no window flag can
/// ask for it; SDL sets it when the compositor gives this surface the keyboard. Read, never asked
/// for, and read by exactly one thing: `crate::textinput`, whose panel it silently gates.
pub(crate) const SDL_WINDOW_INPUT_FOCUS: u32 = 0x200;
const GL_COLOR_BUFFER_BIT: c_uint = 0x0000_4000;
const GL_RENDERER: c_uint = 0x1F01;
const GL_VERSION: c_uint = 0x1F02;
// SDL_GLattr enum
const A_RED: c_int = 0;
const A_GREEN: c_int = 1;
const A_BLUE: c_int = 2;
const A_ALPHA: c_int = 3;
const A_BUFFER_SIZE: c_int = 4;
const A_DEPTH: c_int = 6;
const A_STENCIL: c_int = 7;
const A_CTX_MAJOR: c_int = 17;
const A_CTX_MINOR: c_int = 18;
const A_CTX_PROFILE_MASK: c_int = 21;
const CTX_PROFILE_ES: c_int = 0x0004;
/// `SDL_GL_CONTEXT_PROFILE_CORE` — the simulator's only option on macOS. See the context request.
const CTX_PROFILE_CORE: c_int = 0x0001;
// event types
const SDL_QUIT: u32 = 0x100;
const SDL_KEYDOWN: u32 = 0x300;
const SDL_KEYUP: u32 = 0x301;
const SDL_MOUSEMOTION: u32 = 0x400;
const SDL_MOUSEBUTTONDOWN: u32 = 0x401;
const SDL_MOUSEBUTTONUP: u32 = 0x402;
const SDL_MOUSEWHEEL: u32 = 0x403;
/// The IME's in-progress COMPOSITION. Not acted on — the search field shows what has been
/// committed, so a preedit would put characters on screen the query does not contain — but LOGGED,
/// because the panel's word prediction is a replace and this is where its delete half would arrive
/// if it arrives at all. See the `"edit"` arm in the event ladder.
const SDL_TEXTEDITING: u32 = 0x302;
/// Text COMMITTED by the system keyboard — `crate::textinput`.
pub(crate) const SDL_TEXTINPUT: u32 = 0x303;
// keysyms, the OK/BACK predicates and `classify` — the key VOCABULARY the ladder below dispatches
// on — live in ui::consts (the single keycode home)
use crate::ui::consts::{
    classify, is_back, is_bound, is_ok, Key, SDLK_DOWN, SDLK_ESCAPE, SDLK_LEFT, SDLK_PAGEDOWN,
    SDLK_PAGEUP, SDLK_RETURN, SDLK_RIGHT, SDLK_UP, WCODE_CH_DOWN_KEY, WCODE_CH_UP_KEY, WCODE_PAUSE,
    WCODE_PLAY, WCODE_POINTER_HIDDEN, WCODE_STOP,
};
// The window we ASK SDL for. `surface::probe` then reads back what we actually got.
const SCR_W: c_int = crate::surface::LOGICAL_W as c_int;
const SCR_H: c_int = crate::surface::LOGICAL_H as c_int;
const COLS: c_int = 10;
const RESUME_REWIND_NS: i64 = 5_000_000_000;

// `SDL_webOSCursorVisibility` is declared apart from the rest because it exists ONLY in LG's
// SDL fork. Naming it in the shared block would make the host simulator fail to link.
#[cfg(not(feature = "hostsim"))]
extern "C" {
    fn SDL_webOSCursorVisibility(visible: c_int) -> c_int;
}

// Desktop-only window management. Apart for the mirror-image reason: a television owns the whole
// panel and never asks how big a display is, so on that build these would be dead code — which
// `[lints.rust] warnings = "deny"` makes a build failure, not a warning.
#[cfg(feature = "hostsim")]
extern "C" {
    /// `SDL_GetDisplayUsableBounds` — the display minus the menu bar and the Dock, which is what
    /// a window may actually occupy. The out parameter is an `SDL_Rect`: exactly four `c_int`.
    fn SDL_GetDisplayUsableBounds(display: c_int, rect: *mut c_int) -> c_int;
}

// Phase 1a of the UI restructure split this file: everything above `plex_run` moved into the
// submodules below as a PURE move (`pub(super)` widening only), glob-imported here so the
// loop body reads exactly as before. `plex_run` itself is phase 1b.
mod boot;
mod events;
mod lifecycle;
mod playback;
mod nav;
mod input;
use self::boot::*;
use self::events::*;
use self::lifecycle::*;
use self::playback::*;
use self::nav::*;
use self::input::*;

extern "C" {
    fn SDL_SetMainReady();
    fn SDL_SetHint(name: *const c_char, value: *const c_char) -> c_int;
    fn SDL_Init(flags: u32) -> c_int;
    fn SDL_GetCurrentVideoDriver() -> *const c_char;
    fn SDL_GL_SetAttribute(attr: c_int, value: c_int) -> c_int;
    fn SDL_CreateWindow(
        title: *const c_char,
        x: c_int,
        y: c_int,
        w: c_int,
        h: c_int,
        flags: u32,
    ) -> *mut c_void;
    fn SDL_GL_CreateContext(win: *mut c_void) -> *mut c_void;
    fn SDL_GL_SetSwapInterval(interval: c_int) -> c_int;
    fn SDL_GetTicks() -> u32;
    fn SDL_Delay(ms: u32);
    fn SDL_GetPerformanceCounter() -> u64;
    fn SDL_GetPerformanceFrequency() -> u64;
    fn SDL_PollEvent(event: *mut c_void) -> c_int;
    fn SDL_PushEvent(event: *const c_void) -> c_int;
    fn SDL_GL_SwapWindow(win: *mut c_void);
    fn SDL_Quit();
    // The system on-screen keyboard. A PLAIN link, not `dynlib!`, and the rule in `dynlib.rs` is
    // why: that module is for libraries whose SONAME moves, and this is stock public SDL2 API —
    // `tools/fwcompat.py --lib libSDL2-2.0.so.0 --grep TextInput` finds the whole family exported
    // by all 14 firmware inventories, so there is nothing here for a runtime bind to tolerate.
    //
    // `pub(crate)` on these five alone because `crate::textinput` owns this seam and is the only
    // caller; the declarations stay here with the rest of SDL rather than being duplicated into a
    // second `extern` block, where a signature could drift from this one unnoticed.
    //
    // The `allow(dead_code)` below is a consequence of that ownership: under `cfg(test)` the only
    // caller swaps itself for `textinput::host_test_sdl`'s stubs, so these three lose their last
    // use in the TEST build alone and warn there. The allow is narrower than it looks — a real
    // orphan would be silent in every configuration, and these are live on device.
    #[allow(dead_code)]
    pub(crate) fn SDL_StartTextInput();
    #[allow(dead_code)]
    pub(crate) fn SDL_StopTextInput();
    pub(crate) fn SDL_IsTextInputActive() -> c_int;
    pub(crate) fn SDL_HasScreenKeyboardSupport() -> c_int;
    /// LG's `WebOSIsScreenKeyboardShown`, the fourth of the four hooks its Wayland driver installs.
    /// Exported by all 14 inventories, and **it does not answer the question its name asks** on this
    /// firmware — `textinput`'s note has the measurement and what replaced it. Declared, unused, and
    /// kept so the next person finds the finding before they find the symbol.
    #[allow(dead_code)]
    pub(crate) fn SDL_IsScreenKeyboardShown(w: *mut c_void) -> c_int;
    pub(crate) fn SDL_GetWindowFlags(w: *mut c_void) -> u32;
    /// Turn the DRIVER's own tracing on for one category. LG's `WebOSShowScreenKeyboard` /
    /// `Hide` / `TextModelLeave` / `TextModelInputPanelState` all log through SDL at
    /// `SDL_LOG_CATEGORY_INPUT`, which is silent at the default priority — so this is how the
    /// keyboard's real lifecycle becomes readable without patching SDL.
    #[allow(dead_code)] // test build only — see the note above `SDL_StartTextInput`
    pub(crate) fn SDL_LogSetPriority(category: c_int, priority: c_int);
    fn glGetString(name: c_uint) -> *const c_char;
    fn glViewport(x: c_int, y: c_int, w: c_int, h: c_int);
    fn glClearColor(r: f32, g: f32, b: f32, a: f32);
    fn glClear(mask: c_uint);
}

use crate::log;
/// The BACK trail's vocabulary — `Trail` is a run-loop local, `Node` its pages, `Spot` the place a
/// detail page is restored to. See `ui/trail.rs`.
use crate::ui::detail::Spot;
use crate::ui::popover::Opener;
use crate::ui::trail::{Node, Trail};
/// The shared top strip's vocabulary: what a pill INDEX means. Every site that turns a pill into a
/// destination `match`es on this, so a pill the app has not been taught about is a compile error
/// rather than a silent library open — see `widgets::Pill`.
use crate::ui::widgets::Pill;

#[no_mangle]
pub extern "C" fn plex_run(pms_host: *const c_char, pms_port: c_int) -> c_int {
    install_panic_logger();
    // WHICH INSTALL wrote this log. First line, before anything can fail.
    //
    // Two builds can sit on one television — the app users get, and a developer one beside it
    // (`paths::app_id`) — and until this line nothing in the system said which of them produced a
    // given log. The obvious witnesses do not work: both binaries are named `plxnative`, so
    // `pidof` cannot tell them apart on this busybox set; `pkg/plxnative` is a path EVERY
    // configuration writes, so an md5 against the local build proves only that some flavour of
    // some configuration matches. That ambiguity is the "plausible wrong data" failure this
    // project's testing section is built around: a harness that graded the other install's log
    // would report a regression that is not there, or miss one that is.
    //
    // `APPID_env` is here for a second reason, and it is evidence rather than configuration.
    // Nothing this project can read off a desk says whether SAM exports `APPID` to a native app on
    // this firmware, or what it sets it to — and `engine::acb_init_acb` used to depend on it. It
    // does not any more (the install directory is the authority), so this line turns an unanswered
    // device question into something every single run answers for free.
    log(&format!(
        "install: id={} flavour={} runtime={} features={} APPID_env={}",
        crate::paths::app_id(),
        crate::paths::flavour().unwrap_or("-"),
        crate::paths::runtime_dir().display(),
        if crate::dev::ENABLED {
            "dev"
        } else {
            "release"
        },
        std::env::var("APPID").unwrap_or_else(|_| "unset".into()),
    ));
    // ...and the app directory on the NEXT line, from `app_dir()` itself, which logs its own
    // provenance (`from current_exe` / `PLXNATIVE_APP_DIR` / `macOS bundle`) — strictly more than
    // repeating the path here would say. Forced now rather than left to whoever calls it first,
    // so the two lines are adjacent and the pair is what a triage reader sees at the top.
    //
    // This ORDER is the reason `install:` does not carry an `appdir=` field: evaluating
    // `app_dir()` inside the `format!` above would emit ITS line first, and every document that
    // tells a human to read the first line to learn which install wrote a log would have been
    // wrong by one line.
    let _ = crate::paths::app_dir();
    // Before the crash backend is armed, identify the firmware it would need to report. Sentry's
    // scope is snapshotted into the crash event file during `telemetry::boot`; probing afterwards
    // leaves only `Linux 4.4.84`, which does not distinguish webOS releases at all. This reads one
    // flat platform file and cannot fail the boot. The crash channel receives only the reviewed
    // compatibility fields (webOS/API/model/SoC/hardware revision), never device identifiers.
    crate::webos::probe();
    // The stored telemetry decision, BEFORE the first event can be reported — `diag::event` reads
    // a snapshot this publishes, and with none installed it refuses everything. So the ordering is
    // the fail-closed guarantee, not a convenience.
    let _telemetry_guard = crate::telemetry::boot();
    // …and then, if asked, DIE. `plxnative-crashtest` is the instrument for the instrument: both
    // the C fallback and (when consented/configured) the out-of-process native recorder are now
    // armed, so this trigger grades the reporter users actually run. It remains before SDL so a
    // playback/UI regression cannot make the instrument unreachable. Compiled out with
    // `devtriggers`; a no-op in every other build.
    crate::dev::crash_on_purpose();
    // The first reportable event, and it is a marker with no fields on purpose — everything that
    // would qualify a launch (model, firmware, version, locale) is a session constant and belongs
    // in a sender's envelope, not repeated on every record. It reaches PostHog when the usage
    // switch is on and this build carries a key; `crate::diag::event` is the gate and fails closed
    // on either. (This comment said "nothing listens today" for as long as that was true and for a
    // while after.)
    crate::diag::event(crate::diag::schema::DiagEvent::AppLaunch);
    // And what it DECODES, from the device's own codec table — the capability profile and the
    // direct-play gate derive from this instead of asserting the dev TV's abilities as universal
    // (issue #22's bug class; docs/plex-pass-audit.md's closing section). Same contract as
    // above: one file read, cannot fail the boot, falls back to the profile that always shipped.
    crate::devcaps::probe();
    // …and, in a LAB build only, the diagnostics bridge: read `lab.json` out of the app directory
    // and start the ring's clock. After the two probes above so its first log line can be read
    // beside the firmware and codec lines it will be uploaded with; a no-op at compile time in
    // every build that is not a lab build (`crate::lab`).
    crate::lab::boot();
    // If armed, hand LG's own media pipeline its logging configuration BEFORE anything can create
    // a player. libpf reads these four environment variables inside `PlayerFactory::create`, and
    // its GStreamer is lazily initialised, so this is early enough and a later arming would be
    // read by nobody. It is the only instrument that can see inside the closed Dolby Vision chain.
    crate::dev::arm_gst_logging();
    // Playback tests photograph the television as well as grading its log. This keeps the same
    // ABR/pipeline evidence visible for every automated playback, rather than depending on the
    // previous manual toggle surviving into a new session.
    if crate::dev::flag("stats") {
        crate::ui::stats::open();
    }
    // THE main-thread token, minted once — this function IS the SDL main thread. Everything that
    // touches the ACB/Starfish seam or the Engine slot takes it by reference, and `&MainThread` is
    // !Send, so `task::spawn` rejects any closure that captured one. See `task::MainThread`.
    let main_thread = unsafe { crate::task::MainThread::assume() };
    let mt = &main_thread;
    unsafe {
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
            return 1;
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
            return 1;
        }
        let ctx = SDL_GL_CreateContext(win);
        if ctx.is_null() {
            log("GL ctx failed");
            return 1;
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

        // Everything that has to happen when the server `plex::client()` answers with CHANGES —
        // whether because a new identity signed in or because the user walked into another source.
        // EVERY store below is keyed to whichever server was current when it was filled, and none
        // of them carries a server in its keys, so leaving one behind means server A's ratingKeys
        // being fetched from server B: the same catalog index opening a different film.
        let activate_server = || {
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
        };
        // Install the PMS client (the read layer AND the playback path) as the CURRENT server,
        // then fetch the catalog. Used by the boot gate and again when a login resolves; a later
        // call for the same address just swaps the token (profile switch).
        // Takes an ORIGIN and not a `(host, port)` pair: the pair cannot say `https`, and the host
        // a certificate is issued for is the `plex.direct` NAME rather than the address behind it
        // (`plex::origin`). Discovery and persisted sessions may supply either scheme; the client
        // routes control and media requests through the matching transport.
        let install_pms = |origin: &crate::plex::Origin,
                           token: &str,
                           tier: Option<crate::plex::probe::Location>,
                           pin: Option<&crate::plex::ResolvePin>| {
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
        };

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
        let mut pick_user: Option<usize> =
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
        let mut home_osc_last = 0u32;
        // dev: the two Home transition scenes the old home-hero/home-grid pair could not see.
        // `heroosc` continuously pages the real carousel; `homefoldosc` alternates the real
        // hero↔first-shelf snap. Their intervals overlap the spring lifetime so the FPS heartbeat
        // samples motion rather than the efficient idle gaps at either end.
        let hero_osc = crate::dev::flag("heroosc");
        let mut hero_osc_last = 0u32;
        let home_fold_osc = crate::dev::flag("homefoldosc");
        let mut home_fold_osc_last = 0u32;
        let mut home_fold_down = true;
        // dev: /tmp/plxnative-libosc — the Library twin of homeosc: sweep the browse grid focus
        // down↔up perpetually for the library_scroll FPS scene.
        let lib_osc = crate::dev::flag("libosc");
        let mut lib_osc_last = 0u32;
        // dev: /tmp/plxnative-libswitch — exercise EVERY Library switch on a timer (tab switch,
        // sort menu open/move/close, unwatched on/off, filter open/close) for the library_switch
        // FPS scene, so the re-query + popover paths are perf-gated, not just the scroll.
        let lib_switch = crate::dev::flag("libswitch");
        let mut lib_switch_last = 0u32;
        let mut lib_switch_step = 0u32;
        // dev: /tmp/plxnative-searchosc — the Search twin of homeosc/libosc: sweep the result
        // shelves' focus down↔up perpetually for the `fps:search-type` scene. It does NOT reach the
        // screen on its own — pair it with `/tmp/plxnative-search=<query>`, and with a query the
        // library actually matches, or there are no shelves to sweep and the scene grades nothing.
        let search_osc = crate::dev::flag("searchosc");
        let mut search_osc_last = 0u32;
        // dev: /tmp/plxnative-settings=<root|home|privacy|legal> opens the Settings modal (and,
        // optionally, one of its real child panels) once Home is available. `settingsosc` turns
        // that settled modal into a continuous render-throughput scene: it alternates the focused
        // row and explicitly keeps the present gate awake. Without the latter an efficient,
        // completely healthy modal intentionally reports ~0 fps after its springs settle, which
        // cannot grade the screen's fill cost. The paired settings-idle scene omits the oscillator
        // and guards the inverse contract.
        let settings_boot = crate::dev::read("settings");
        let settings_osc = crate::dev::flag("settingsosc");
        let mut settings_osc_last = 0u32;
        let mut settings_osc_down = true;
        // dev: /tmp/plxnative-modalosc — with `plxnative-settings=root`, OPEN and DISMISS the
        // Settings modal every 1500 ms through the same `open`/`on_back` the chip and BACK use, so
        // `fps:modal-ramp` grades the appear/disappear RAMP (host snapshot, scrim, ground) under
        // `worst_ceiling_ms` rather than a settled modal. It reverses on a clock because the ramp
        // itself has no end the app reports.
        let modal_osc = crate::dev::flag("modalosc");
        let mut modal_osc_last = 0u32;
        // dev: /tmp/plxnative-legaldoc — with `plxnative-settings=legal`, press OK on the Legal
        // index ONCE so the boot lands on a pushed DOCUMENT (the reader over the frozen ground),
        // which no boot trigger reached before: `fps:legal-document`.
        let legal_doc = crate::dev::flag("legaldoc");
        let mut legal_doc_tried = false;
        // dev: /tmp/plxnative-alert — with `plxnative-settings=privacy`, open the "Delete all local
        // data?" DECISION ALERT once the privacy panel is up. It is the one shared yes/no alert in
        // the app and nothing headless could reach it: `fps:decision-alert`. Opening it is all this
        // does — nothing is deleted, and Cancel is what a BACK would press.
        let alert_boot = crate::dev::flag("alert");
        let mut alert_tried = false;
        // The profile menu freezes its host and uses one cached backdrop. Drive the menu's own
        // TableView for a strict FPS scene; reusing `homeosc` would now correctly move nothing and
        // would grade the idle keepalive rather than the popover.
        let account_osc = crate::dev::flag("acctosc");
        let mut account_osc_last = 0u32;
        let mut account_osc_down = true;
        // First-run route oscillators keep their real focus models moving so the device FPS suite
        // grades the composition rather than a settled screen that correctly stops presenting.
        let consent_osc = crate::dev::flag("consentosc");
        let mut consent_osc_last = 0u32;
        let mut consent_osc_down = true;
        let onboard_osc = crate::dev::flag("onboardosc");
        let mut onboard_osc_last = 0u32;
        let mut onboard_osc_right = true;
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
        let mut nav_osc_last = 0u32;

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
        let perf_freq = SDL_GetPerformanceFrequency() as f64;
        let perf_ms = |c: u64| c as f64 * 1000.0 / perf_freq;
        let mut fd_worst = 0.0f64; // worst frame-total this second, for a once/sec peak line
        let mut fd_worst_prep = 0.0f64; // worst prepare phase this second, timed on every iteration
        let mut fd_stamps = [0u64; 9]; // the nine phase stamps of one iteration (see the loop top)

        let mut last_input = SDL_GetTicks();
        let t0 = last_input;
        let mut loop_t = t0;
        let mut iters_ct = 0i32;
        let mut loop_shown = 0i32;
        // The dev number painted in the top-right corner. Unlike `loop_shown`, this is a real
        // presentation rate: the same completed-window value the heartbeat publishes as `fps=`.
        // It is updated only when that heartbeat drains PRESENTS, so pixels and logs cannot
        // disagree by observing two different counters. On a settled screen the number changes
        // only when the ordinary keepalive next buys a frame; the diagnostic must never defeat
        // the present gate merely to repaint itself.
        #[cfg(feature = "devtools")]
        let mut fps_shown = 0i32;
        // (media ns, SDL ticks) at the previous heartbeat, for `play=` below. `None` while
        // nothing is presenting, so the first beat of a playback reports no rate rather than a
        // fabricated one.
        let mut play_prev: Option<(i64, u32)> = None;
        let mut running = true;
        // Dev-only panel proof: advance a red/green counter phase only after SDL_GL_SwapWindow
        // returns. Hold each colour for 30 swaps: per-buffer alternation blends yellow at 60 Hz,
        // while this ~2 Hz change is human-visible and still freezes immediately with presentation.
        #[cfg(feature = "devtools")]
        let mut buffer_flip_count = 0u8;

        let mut held_key = HeldKey::IDLE;
        let mut scrubber = Scrub::IDLE;
        // Item 13: rate-limits a hardware auto-repeat (or a wheel tick) forwarded into
        // Settings/Consent/Legal's `on_updown`/`on_left_right` — see `on_auto_repeat`'s doc.
        let mut modal_repeat = RepeatGate::IDLE;
        let mut hud = HudState::IDLE;
        let mut marker_tried = false; // dev: the /tmp/plxnative-marker jump has been resolved
        let mut foreground = ForegroundLifecycle::IDLE;
        let mut repause_at = 0i64;
        // ui::press click state: a grid-card OK is deferred (press-in on down, activate on the
        // spring-back after key-up) so `ok_armed` marks "a press is in flight, commit it from the
        // per-frame loop when press::take_commit fires". Only ever set on Home's grid.
        let mut ok_armed = false;
        // Which route name was last REPORTED as an event. Not `route` itself: several `Route`
        // values share one name (every `Route::Player { overlay }` is "player"), and an overlay
        // opening is not a screen change.
        let mut last_route_reported: &'static str = "";
        let mut press_tried = false; // dev: /tmp/plxnative-press fires one simulated grid-card press
        let mut press_release_at = 0u32; // …and the tick at which that simulated press releases
        let mut itemmenu_tried = false; // dev: /tmp/plxnative-itemmenu opens the card context menu once
        let mut ptr = Pointer::IDLE;

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
        let mut play_from = Node::Home;
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
        let mut trail = crate::ui::trail::Trail::new();
        // The route change the page cross-fade is carrying, applied at its floor. `None` whenever
        // no transition is in flight — which is every path that deliberately keeps today's hard cut
        // (a boot trigger, a player exit, the app-switch lifecycle, a login landing), so the
        // default really is "nothing changes".
        let mut nav_pending: Option<NavReq> = None;

        let mut auto_tried = false;
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
        let mut replay_left: u32 = replay_budget(crate::dev::read("replay").as_deref());
        let mut grid_tried = false;
        let mut settings_tried = settings_boot.is_none();
        let mut seek_tried = false;
        // /tmp/plxnative-autoseek seek script (see the parse site): pending steps, the tick of
        // the last fired step, the gap between steps, and the last REQUESTED target (the base
        // for "+10"/"-10" tap-relative steps, like taps on the HUD's frozen scrub playhead).
        let mut seek_script: Vec<String> = Vec::new();
        let mut seek_script_at = 0u32;
        let mut seek_gap_ms = 300u32;
        let mut seek_script_last = 0i64;
        // /tmp/plxnative-qualityswitch: the rungs still to switch to, the tick of the last one
        // fired, and the gap between them. Same shape as the seek script above, for the same
        // reason — a person changing quality mid-playback does it more than once.
        let mut quality_script: Vec<crate::plex::session::PlaybackQuality> = Vec::new();
        let mut quality_script_at = 0u32;
        let mut quality_gap_ms = 0u32;
        let mut quality_tried = false;
        let mut quality_playing_since: Option<u32> = None;
        let mut detail_tried = false;
        let mut play_tried = false;
        let mut menu_tried = false;
        let mut menupick_tried = false;
        let mut pause_tried = false;
        // `/tmp/plxnative-autopause`: an authored Pause edge, plus the optional Resume edge which
        // owns the same script. External effects retry until the synchronized player state machine
        // accepts them; a busy native transition cannot silently consume the test operation.
        let mut pause_script: Option<(u32, Option<u32>)> = None;
        let mut pause_resume_at: Option<u32> = None;
        let mut prev = 0u32;
        // Home data refresh, armed on every player exit (Stop/BACK/EOS): the hubs are refetched a
        // beat later so the final timeline PUT lands first — Continue Watching then shows the new
        // resume point / next episode instead of the state from boot.
        let mut refresh_hubs_at = 0u32;

        let mut ev = [0u8; 128];
        // dev/testing remote: drain any tokens written to /tmp/plxnative-remote and push
        // them as synthetic key events BEFORE the poll loop, so they're consumed this frame
        // by the ONE real key handler (see crate::remote / tools/stream-screen.py).
        let mut remote = crate::remote::Remote::open();
        // A LAB package may opt into the outbound long-poll command channel. Start only now: curl
        // has been initialised and, unlike the earlier boot/discovery work, the SDL loop below is
        // ready to dispatch a delivered command within one frame. Compile-time no-op otherwise.
        crate::lab::start_control();
        while running {
            // Resolve the control row ONCE per iteration, before the event pump, and pass this
            // value to input, update and draw alike. `player_hud::slot()` reads `playpos_ns`, which
            // LG's media thread writes and `player::pump` advances mid-iteration — deriving it per
            // call site let a keypress activate a control this same frame then declined to draw.
            let ctrl = crate::ui::player_hud::slot();
            // Frame-drop detector, stamp 0 of 9: the TOP of the iteration, before the ls2 pump
            // and the event poll — `worstframe=` covers the whole iteration, not the half after
            // the input. Eight phases between the nine stamps, named after the frame algorithm
            // the restructure moves this loop onto (spec §3.3/§8.4); on THIS loop navcommit
            // precedes tick_drain, and the FRAMEDROP line prints them in the algorithm's order.
            let fd = &mut fd_stamps;
            fd[0] = if framedrop_on { SDL_GetPerformanceCounter() } else { 0 };
            crate::system::ls2_pump();
            // Cloud Test Lab has no SSH/FIFO. Its LAB build long-polls outward, then leaves each
            // command here for the SDL thread so the same dispatcher and event queue remain the
            // only input path. Acknowledge acceptance after dispatch, before polling SDL below.
            for command in crate::lab::take_commands() {
                let ok = dispatch_remote_token(&command.token);
                crate::lab::command_done(command.id, ok);
            }
            if let Some(r) = remote.as_mut() {
                r.drain(|tok| {
                    let _ = dispatch_remote_token(tok);
                });
            }
            while SDL_PollEvent(ev.as_mut_ptr() as *mut c_void) != 0 {
                let et = rd_u32(&ev, 0);
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
                    for b in &ev[..48] {
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
                        SDL_GetTicks()
                    ));
                }
                if et == SDL_QUIT {
                    running = false;
                } else if et == 0x103 || et == 0x104 {
                    // WILL/DID ENTER BACKGROUND
                    log(&format!(
                        "LIFECYCLE: background (playing={})",
                        matches!(route, Route::Player { .. }) as i32
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
                    if matches!(route, Route::Player { .. }) && !foreground.awaiting_load() {
                        // INTENDED, not published: this snapshot is the only thing the foreground
                        // restore has, and `suspend_bufferfeed` below drops the pending seek target
                        // with the session — so a background that lands while a seek is still
                        // resolving would otherwise save (and restore to) the spot the user just
                        // seeked AWAY from, with nothing left to correct it. See `intended_pos`.
                        let saved_ns = intended_pos();
                        let clock = foreground.clock_for_suspend(paused());
                        foreground.suspend(saved_ns, clock);
                        scrubber.disengage();
                        ptr.drag = false;
                        held_key.sym = 0; // this async route flip must not leave a held key repeating into Home
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
                        route = Route::Home;
                    }
                } else if et == 0x105 || et == 0x106 {
                    // WILL/DID ENTER FOREGROUND
                    log(&format!(
                        "LIFECYCLE: foreground (wasPlaying={})",
                        foreground.awaiting_load() as i32
                    ));
                    if et == 0x106 {
                        let activation = drive_foreground(
                            &mut foreground,
                            ForegroundInput::DidForeground,
                            &mut PlayerForegroundActuator {
                                mt,
                                repause_at: &mut repause_at,
                            },
                        );
                        if matches!(activation, ForegroundActivation::Launched) {
                            route = Route::Player {
                                overlay: Overlay::None,
                            };
                            set_hud(SDL_GetTicks() + HUD_LINGER_MS);
                        }
                    }
                } else if et == SDL_KEYDOWN || et == SDL_KEYUP {
                    let (state, wcode, sym) = decode_key(&ev);
                    // The press's IDENTITY, resolved once from the two raw fields
                    // (`ui::consts::classify`, which is where the spellings live and where they
                    // are tested). `sym` and `wcode` are still read raw by the arms below — the
                    // ones that forward them to a screen's own `move_focus`/`key`, and the modal
                    // panels and the CH▲/CH▼ pager, which still spell their own key tests.
                    let key = classify(sym, wcode);
                    let isnav = matches!(key, Key::Left { .. } | Key::Right { .. });
                    if (state & 0xff) != 1 {
                        on_key_up(
                            sym,
                            isnav,
                            route,
                            ok_armed,
                            &mut held_key,
                            &mut scrubber,
                            &mut repause_at,
                        );
                        continue;
                    }
                    // A repeat is only a repeat if we watched the key go down. See
                    // `HeldKey::down_sym`: the system keyboard eats key-ups, so the driver stamps
                    // 0x100 on presses that are the FIRST of their own gesture, and dropping those
                    // loses one press in two.
                    if state & 0x100 != 0 && sym == held_key.down_sym {
                        on_auto_repeat(
                            sym,
                            isnav,
                            route,
                            ok_armed,
                            hud.nav,
                            &mut held_key,
                            &mut scrubber,
                            &mut modal_repeat,
                        );
                        continue;
                    }
                    // From here down this IS a fresh press, whatever the driver stamped on it.
                    last_input = SDL_GetTicks();
                    begin_fresh_press(
                        key,
                        sym,
                        wcode,
                        last_input,
                        &mut held_key,
                        &mut hud,
                        &mut ptr,
                        &mut ok_armed,
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
                    // Legal is high in the chain, and the ordering is load-bearing rather than
                    // arbitrary: the notice is opened from the account menu, which is reachable
                    // from Home's ROOT — so with this arm any lower, BACK out of the privacy
                    // notice would be read as the ROOT PRESS and hand the screen to the
                    // television's Home instead of closing the notice. It is a `Popover` and not a
                    // `Route` (one owner, `ui::legal`), so it takes its turn by being high in the
                    // chain and `continue`ing on every key; that IS its modality.
                    // ABOVE Legal, and therefore above everything: the consent question is the
                    // one panel that must be answered before the app is usable, and it is not
                    // answering a press the person just made — it is the reason the boot stopped,
                    // which is why its BACK is navigation and never an answer (below). Same
                    // mechanism as the arm below it (a `Popover`,
                    // not a `Route`, taking its turn by height in the chain and `continue`ing on
                    // every key), which is also the whole of its modality. First-run BACK is
                    // navigation, never an answer: Product returns to Crash. Only the explicit
                    // Share / Don’t Share ANSWERS write a decision — they are the route's
                    // action band, not rows. Settings BACK discards its draft.
                    //
                    // **AT `Stage::Crash` THIS ARM SWALLOWS BACK, AND THAT IS THE ONE ROOT THE
                    // 2026-09-03 root rule does not yet reach.** The comment here used to say that
                    // press "restores Profiles or Shared Sources", which `consent::on_back` has
                    // never done — it returns `true` having done nothing, because sign-in is behind
                    // this question and cannot be undone. Under the new rule that is a root like
                    // any other and should call `back_at_root()`: going to the television's Home
                    // neither answers nor dismisses the question, so nothing is stranded and
                    // selecting the tile again comes straight back to it. It is NOT done here for
                    // one mechanical reason — `consent::on_back` reports `true` for BOTH the
                    // stepped-back and the swallowed case, so this arm cannot tell them apart, and
                    // teaching it to means changing `ui/consent.rs`'s return type (`Consumed |
                    // Root`), which belongs with that module rather than in a BACK arm guessing at
                    // its stage.
                    if crate::ui::consent::is_open() {
                        if is_ok(sym) {
                            if crate::ui::consent::focus_is_ctl() {
                                // An answer pill is a control face with a pop of its own
                                // (`route_screen::ActionRow`), so OK takes the tvOS press: dip
                                // now, commit in `commit_consent` on the spring-back — the shared
                                // decision alert's shape, for the same reason (the sheet is up
                                // through the whole animation, so the answer being taken stays
                                // legible).
                                // `arm_key` records WHICH control and that the press came from the
                                // KEY, so hover judges it by the focus stop rather than by the
                                // coordinates it never had (`route_screen::PressFrom`).
                                crate::ui::consent::arm_key();
                                crate::ui::press::begin_ctl(last_input);
                                ok_armed = true;
                            } else {
                                // a TableView row (the two documents) commits on the key-down,
                                // as every row in the app does
                                commit_consent(&mut route, &mut trail);
                            }
                        } else if is_back(sym, wcode) {
                            // BACK reverses Product → Crash and is swallowed at Crash: the step
                            // behind the consent question is sign-in, which cannot be undone.
                            crate::ui::consent::on_back();
                        } else if sym == SDLK_UP {
                            crate::ui::consent::on_updown(-1);
                        } else if sym == SDLK_DOWN {
                            crate::ui::consent::on_updown(1);
                        } else if sym == SDLK_LEFT {
                            crate::ui::consent::on_left_right(-1);
                        } else if sym == SDLK_RIGHT {
                            crate::ui::consent::on_left_right(1);
                        }
                        continue;
                    }
                    if crate::ui::legal::is_open() {
                        if is_ok(sym) {
                            crate::ui::legal::on_ok();
                        } else if is_back(sym, wcode) {
                            crate::ui::legal::on_back();
                        } else if sym == SDLK_UP {
                            crate::ui::legal::on_updown(-1);
                        } else if sym == SDLK_DOWN {
                            crate::ui::legal::on_updown(1);
                        } else if sym == SDLK_LEFT {
                            crate::ui::legal::on_left_right(-1);
                        } else if sym == SDLK_RIGHT {
                            crate::ui::legal::on_left_right(1);
                        }
                        continue;
                    }
                    if settings_root_owns_input(
                        route,
                        crate::ui::settings::is_open(),
                        crate::ui::onboard::settings_mode(),
                    ) {
                        if is_ok(sym) {
                            let action = crate::ui::settings::on_ok();
                            perform_settings_action(action, &mut route);
                        } else if is_back(sym, wcode) {
                            crate::ui::settings::on_back();
                        } else if sym == SDLK_UP {
                            crate::ui::settings::on_updown(-1);
                        } else if sym == SDLK_DOWN {
                            crate::ui::settings::on_updown(1);
                        } else if sym == SDLK_LEFT || sym == SDLK_RIGHT {
                            // `ui::route_screen`'s rules 8 and 9: RIGHT enters the row under
                            // focus, LEFT leaves a screen that has no action band. The root
                            // answered neither key at all until the family was given one model.
                            let action = crate::ui::settings::on_left_right(
                                if sym == SDLK_LEFT { -1 } else { 1 },
                            );
                            perform_settings_action(action, &mut route);
                        }
                        continue;
                    }
                    if matches!(route, Route::Login | Route::Profiles | Route::Onboard) {
                        let action = key_onboarding(route, sym, wcode, &mut ok_armed);
                        if let Some(next) = apply_onboarding_action(action, &mut trail) {
                            route = next;
                        }
                        continue;
                    }
                    if let Route::Account { over } = route {
                        key_account(over, sym, wcode, &mut route);
                        continue;
                    }
                    if let Route::ItemMenu { over } = route {
                        key_item_menu(
                            mt,
                            over,
                            sym,
                            wcode,
                            last_input,
                            &mut route,
                            &mut play_from,
                            &mut trail,
                            &mut hud.nav,
                            &mut nav_pending,
                            &mut held_key,
                        );
                        continue;
                    }
                    // A failure owns the frame, except for the recovery-quality popover it opened
                    // itself.  Stale Menu / Info / Chapters panels remain unreachable; More is the
                    // one drawn and drivable escape promised by the failure read-out.
                    if matches!(route, Route::Player { .. })
                        && crate::ui::player_hud::transport_hidden()
                        && !matches!(
                            route,
                            Route::Player {
                                overlay: Overlay::More
                            }
                        )
                    {
                        key_player_failed(
                            mt,
                            sym,
                            wcode,
                            &mut route,
                            &play_from,
                            &mut refresh_hubs_at,
                            &mut trail,
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
                        route,
                        Route::Player {
                            overlay: Overlay::Menu
                        }
                    ) && overlay_swallows_key(route, key)
                    {
                        key_track_menu(sym, wcode, last_input, &mut route, &mut held_key);
                        continue;
                    }
                    if matches!(
                        route,
                        Route::Player {
                            overlay: Overlay::More
                        }
                    ) {
                        key_more_menu(mt, sym, wcode, last_input, &mut route, &mut held_key);
                        continue;
                    }
                    if matches!(
                        route,
                        Route::Player {
                            overlay: Overlay::Info
                        }
                    ) && overlay_swallows_key(route, key)
                    {
                        key_info_panel(
                            mt,
                            sym,
                            wcode,
                            last_input,
                            &mut route,
                            &play_from,
                            &mut refresh_hubs_at,
                            &mut trail,
                            &mut hud.nav,
                            &mut held_key,
                            &mut ok_armed,
                        );
                        continue;
                    }
                    if matches!(
                        route,
                        Route::Player {
                            overlay: Overlay::Chapters
                        }
                    ) && overlay_swallows_key(route, key)
                    {
                        key_chapters(
                            mt,
                            key,
                            sym,
                            wcode,
                            last_input,
                            &mut route,
                            &mut hud.nav,
                            &mut held_key,
                        );
                        continue;
                    }
                    if matches!(route, Route::Player { .. }) && matches!(key, Key::Up | Key::Down) {
                        key_player_updown(key, last_input, &mut hud, &mut scrubber);
                        continue;
                    }
                    // Search's field takes the press first — but this arm has no body to name,
                    // because `search::key` IS the body: it handles the key and returns whether it
                    // did. So the route test is a guard around CALLING it, not a term to be `&&`ed
                    // with it, and it is written as the nested `if` it always meant. Off Search the
                    // call must not happen at all; on Search, a key it declines falls through to
                    // the chain below exactly as it did.
                    if matches!(route, Route::Search) {
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
                    if !matches!(route, Route::Player { .. })
                        && matches!(
                            key,
                            Key::Up
                                | Key::Down
                                | Key::Left { alt: false }
                                | Key::Right { alt: false }
                        )
                    {
                        key_move_focus(key, sym, route, last_input, &mut held_key);
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
                            last_input,
                            &mut route,
                            &mut hud,
                            &mut ptr,
                            &mut trail,
                            &mut nav_pending,
                            &mut play_from,
                            &mut ok_armed,
                        );
                    } else if matches!(key, Key::Pause) {
                        key_pause(mt, route, last_input);
                    } else if matches!(key, Key::Play) {
                        key_play(
                            mt,
                            last_input,
                            &mut foreground,
                            &mut repause_at,
                            &mut route,
                            &mut play_from,
                            &mut ptr,
                        );
                    } else if matches!(key, Key::PlayPause) {
                        // ONE key, both directions. `key_play`/`key_pause` are each half of the
                        // toggle, so this arm picks; off the player route `key_play` is what starts
                        // playback, which is the right answer for a PLAYPAUSE press on a card.
                        if paused() || !matches!(route, Route::Player { .. }) {
                            key_play(
                                mt,
                                last_input,
                                &mut foreground,
                                &mut repause_at,
                                &mut route,
                                &mut play_from,
                                &mut ptr,
                            );
                        } else {
                            key_pause(mt, route, last_input);
                        }
                    } else if matches!(key, Key::Exit) {
                        // The remote's EXIT key — LG's checklist item 38 wants the app terminated,
                        // and unlike BACK at Home's root there is nothing ambiguous about a key
                        // labelled EXIT, so unlike BACK it really does end the process — and it
                        // is now the only key that does.
                        log("EXIT key: terminating");
                        running = false;
                    } else if matches!(route, Route::Player { .. }) && matches!(key, Key::Stop) {
                        // Stop — the whole arm is the one ritual, already named.
                        exit_player(mt, &mut route, &play_from, &mut refresh_hubs_at, &mut trail);
                    } else if matches!(route, Route::Player { .. })
                        && matches!(key, Key::Left { .. } | Key::Right { .. })
                    {
                        key_scrub(key, last_input, ctrl, &mut hud, &mut ptr, &mut scrubber);
                    } else if let (Route::Library, Some(dir)) =
                        (route, crate::ui::consts::page_dir(sym, wcode))
                    {
                        key_library_page(dir);
                    } else if matches!(key, Key::Back) {
                        key_back(
                            mt,
                            &mut route,
                            &mut nav_pending,
                            &mut trail,
                            &play_from,
                            &mut refresh_hubs_at,
                        );
                    }
                } else if et == SDL_MOUSEMOTION {
                    last_input = SDL_GetTicks();
                    ptr.last_motion = last_input;
                    ptr.cur_hidden = false;
                    let (mx, my) = ptr_xy(&ev);
                    if ptr.prev_mx >= 0.0 {
                        ptr.mot_accum += (mx - ptr.prev_mx).abs() + (my - ptr.prev_my).abs();
                    }
                    ptr.prev_mx = mx;
                    ptr.prev_my = my;
                    if matches!(route, Route::Player { .. }) {
                        // Player owns this arm before the generic per-route hover ladder below, so
                        // the overflow popover must be dispatched here.  Otherwise its later
                        // `Overlay::More` arm is unreachable for every playback, including the
                        // terminal recovery picker.
                        if matches!(
                            route,
                            Route::Player {
                                overlay: Overlay::More
                            }
                        ) {
                            crate::ui::more_menu::pointer_focus(mx, my);
                            continue;
                        }
                        hud.dismissed = false;
                        extend_hud(last_input, HUD_LINGER_MS);
                        if ptr.drag && dur() > 0 {
                            let frac = crate::ui::player_hud::scrub_frac_x(mx) as f64;
                            set_scrub((frac * dur() as f64) as i64);
                        }
                        continue;
                    }
                    if ptr.dpad_mode {
                        if ptr.mot_accum < 120.0 {
                            continue;
                        }
                        ptr.dpad_mode = false;
                    }
                    // The Settings family is a chain of POPOVERS over whatever route is behind
                    // them, so a hover ladder keyed by `route` never reached any of it — every
                    // pointer move across Settings, Privacy & data or Legal drove HOME's focus
                    // underneath instead. `ui::route_screen`'s rule 11 says hover parks focus on
                    // every screen in the family, so they take their turn here in exactly the
                    // order the key ladder gives them.
                    if crate::ui::consent::is_open() {
                        // `ui::press` assumes focus cannot move while a press is in flight — the
                        // nav keys pay that by calling `press::cancel`, and hover owes the same.
                        // Otherwise a pointer-down on `Share reports` plus ordinary Magic Remote
                        // jitter records the OTHER answer, or — the case the first version of this
                        // guard missed — slides off every control and records the ORIGINAL one
                        // anyway, because a miss leaves focus where it was. `pointer_hold` parks
                        // focus as usual and reports whether the pointer is still on the thing the
                        // press was armed on, dead space included.
                        let held = if crate::ui::consent::alert_is_open() {
                            crate::ui::consent::alert_hold(mx, my)
                        } else {
                            crate::ui::consent::pointer_hold(mx, my)
                        };
                        if ok_armed && !held {
                            crate::ui::press::cancel();
                            ok_armed = false;
                        }
                        continue;
                    }
                    if crate::ui::legal::is_open() {
                        crate::ui::legal::pointer_focus(mx, my);
                        continue;
                    }
                    if settings_root_owns_input(
                        route,
                        crate::ui::settings::is_open(),
                        crate::ui::onboard::settings_mode(),
                    ) {
                        crate::ui::settings::pointer_focus(mx, my);
                        continue;
                    }
                    if matches!(route, Route::Profiles) {
                        crate::ui::profiles::pointer_focus(mx, my);
                    } else if matches!(route, Route::Onboard) {
                        // The same guard the consent arm above pays, and for the same reason: this
                        // screen's action pill is the one control face in the family that has been
                        // press-armed from the pointer since it was written, so hover sliding off
                        // it mid-press could commit from a control the ring had already left.
                        if ok_armed && !crate::ui::onboard::pointer_hold(mx, my) {
                            crate::ui::press::cancel();
                            ok_armed = false;
                        } else if !ok_armed {
                            crate::ui::onboard::pointer_focus(mx, my);
                        }
                    } else if matches!(route, Route::Account { .. }) {
                        crate::ui::account_menu::pointer_focus(mx, my);
                    } else if matches!(route, Route::ItemMenu { .. }) {
                        crate::ui::item_menu::pointer_focus(mx, my);
                    } else if matches!(route, Route::Library) {
                        // the same trade Detail makes below: hover that MOVES the focus stop
                        // aborts a press armed on the stop it left, or the click commits — or the
                        // press-and-hold menu opens — on a tile the user is no longer pressing
                        if crate::ui::library::pointer_focus(mx, my) && ok_armed {
                            crate::ui::press::cancel();
                            ok_armed = false;
                        }
                    } else if matches!(route, Route::Detail) {
                        // the detail page owns its own screen, so hover moves ITS focus (the rule
                        // above); it declines the moves that would scroll the page under a
                        // stationary pointer — see detail::hover_allows
                        if crate::ui::detail::pointer_focus(mx, my) && ok_armed {
                            // the pointer slid off the control the click was armed on: abort the
                            // press without activating, exactly as a nav key does above
                            crate::ui::press::cancel();
                            ok_armed = false;
                        }
                    } else if matches!(route, Route::Person) {
                        crate::ui::person::pointer_focus(mx, my);
                    } else if matches!(route, Route::Search) {
                        let (mx, my) = ptr_xy(&ev);
                        crate::ui::search::pointer_focus(mx, my);
                    } else if matches!(route, Route::Home) {
                        // hover moves focus on the route that owns the screen — and ONLY there
                        // (Detail/Login hover used to silently mutate home's focus behind them)
                        if crate::ui::home::snap_pos() < 0.5 {
                            crate::ui::home::hero_pointer_focus(mx, my);
                            // the centered tab pills are hoverable in hero view too — Home
                            // included: it is a real focus stop, it just has nowhere to go
                            if let Some(i) = crate::ui::widgets::tab_pill_at(mx, my) {
                                crate::ui::home::set_hero_focus(
                                    crate::ui::home::hero_focus_for_pill(i),
                                );
                            }
                        } else {
                            crate::ui::home::home_pointer_focus(mx, my);
                        }
                    }
                } else if et == SDL_MOUSEBUTTONDOWN {
                    last_input = SDL_GetTicks();
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
                    if ok_armed {
                        crate::ui::press::cancel();
                        ok_armed = false;
                    }
                    // Rule 11's click half. These two used to `continue` unconditionally, which
                    // is why Privacy & Data answered neither hover nor click: an answer pill, a
                    // Done, a document row and a delete-confirmation answer were all unclickable.
                    if crate::ui::consent::is_open() {
                        let (cx, cy) = ptr_xy(&ev);
                        // a control FACE dips and commits on the spring-back, exactly as its OK
                        // does; a table row commits on the button-down like every row in the app
                        if crate::ui::consent::alert_press_at(cx, cy)
                            || crate::ui::consent::press_at(cx, cy)
                        {
                            crate::ui::press::begin_ctl(last_input);
                            ok_armed = true;
                        } else if crate::ui::consent::click_row(cx, cy) {
                            commit_consent(&mut route, &mut trail);
                        }
                        continue;
                    }
                    if crate::ui::legal::is_open() {
                        let (cx, cy) = ptr_xy(&ev);
                        crate::ui::legal::click(cx, cy);
                        continue;
                    }
                    if settings_root_owns_input(
                        route,
                        crate::ui::settings::is_open(),
                        crate::ui::onboard::settings_mode(),
                    ) {
                        let (cx, cy) = ptr_xy(&ev);
                        let action = crate::ui::settings::click(cx, cy);
                        perform_settings_action(action, &mut route);
                        continue;
                    }
                    // …and the pointer's half of the same rule.  The erased transport geometry is
                    // still inert, but the read-out now exposes one real target: choose quality.
                    // Once that opens More, the popover owns clicks through the ordinary modal arm
                    // below; every other click on the failed frame remains nothing.
                    if matches!(route, Route::Player { .. })
                        && crate::ui::player_hud::transport_hidden()
                        && !matches!(
                            route,
                            Route::Player {
                                overlay: Overlay::More
                            }
                        )
                    {
                        let (cx, cy) = ptr_xy(&ev);
                        if crate::ui::player_hud::failure_quality_hit(cx, cy) {
                            crate::ui::more_menu::open_quality();
                            route = Route::Player {
                                overlay: Overlay::More,
                            };
                        }
                        continue;
                    }
                    if matches!(route, Route::Player { .. }) {
                        // Sample HUD visibility BEFORE re-arming it: a click must only act on
                        // transport geometry the user can SEE (the key path's vis gate — a
                        // hidden-HUD OK falls through to play/pause). Without this, a click in
                        // the invisible timed-out scrub band committed a blind seek.
                        let hud_vis = hud_visible(last_input, hud_until(), paused(), hud.dismissed);
                        hud.dismissed = false;
                        let (cx, cy) = ptr_xy(&ev);
                        // Which control-row ITEM the click landed on, resolved ONCE: the arm below
                        // both guards on it and parks the ring with it, and re-asking would be two
                        // derivations of one answer — the thing `ControlSlot` exists to prevent.
                        // `None` for the discs, whose own `icon_hit` is consulted further down.
                        let ctrl_click = if hud_vis { ctrl.hit(cx, cy) } else { None };
                        // An open panel owns the click: dismiss it and STOP. The transport is
                        // partly hidden while a panel is up (draw_hud gets transport:false), so
                        // its rects must not be consulted — mirrors the modal key arms above.
                        match modal_of(route) {
                            Modal::Menu => {
                                crate::ui::track_menu::close();
                                route = Route::Player {
                                    overlay: Overlay::None,
                                };
                            }
                            Modal::Info => {
                                crate::ui::info_panel::close();
                                route = Route::Player {
                                    overlay: Overlay::None,
                                };
                            }
                            Modal::Chapters => {
                                crate::ui::chapters_panel::close();
                                route = Route::Player {
                                    overlay: Overlay::None,
                                };
                            }
                            // Unlike the panels above, this popover's rows are ACTIONS, so a click
                            // that lands on one commits it (and a click outside reports None and
                            // just dismisses) — `account_menu`'s contract, same as its key path.
                            Modal::More => {
                                apply_more_action(mt, crate::ui::more_menu::click(cx, cy));
                                route = Route::Player {
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
                                hud.nav.focus = 1;
                                hud.nav.btn = ctrl_click.unwrap_or(0);
                                // …then the tvOS press, exactly as the key arm does it:
                                // `activate_player_row` reads `hud.nav`, which the two lines
                                // above have just parked on what was clicked.
                                crate::ui::press::begin_ctl(last_input);
                                ok_armed = true;
                            }
                            _ => {
                                // shared HUD geometry: player_hud owns the button rects + scrub
                                // band — consulted only while that geometry is on screen
                                let icon = if hud_vis {
                                    crate::ui::player_hud::icon_hit(ctrl, cx, cy)
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
                                    hud.nav.focus = 1;
                                    hud.nav.btn = idx;
                                    crate::ui::press::begin_ctl(last_input);
                                    ok_armed = true;
                                } else if let Some(frac) = on_scrub {
                                    let mut t = (frac as f64 * dur() as f64) as i64;
                                    let cap = dur() - 3 * 1_000_000_000;
                                    if cap > 0 && t > cap {
                                        t = cap;
                                    }
                                    set_scrub(t);
                                    ptr.drag = true;
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
                        extend_hud(last_input, HUD_LINGER_MS);
                    } else if chip_clicked(route, &ev) {
                        // the shared bar's profile chip, on whichever of the three screens is up —
                        // the pointer twin of `key_ok`'s own `TopFocus::Chip` arm. It sits ahead of
                        // all three so none of them has to carry a copy of the rule (Home did, and
                        // that is why the other two had a chip nothing could press).
                        chip_activate(&mut route);
                    } else if matches!(route, Route::Home) {
                        let (cx, cy) = ptr_xy(&ev);
                        if let Some(i) = crate::ui::widgets::tab_pill_at(cx, cy) {
                            // the centered tab pills work from BOTH hero and grid views
                            match crate::ui::widgets::pill_at(i) {
                                Pill::Search => nav_to(route, Nav::Search, &mut nav_pending),
                                Pill::Section(kind) => {
                                    nav_to(route, Nav::Library(kind), &mut nav_pending)
                                }
                                // Home is the screen we are on, so a click there just parks focus
                                // on the pill — in hero view, which is where the band's focus is
                                // visible — unless there is a section switch still fading out to
                                // take back, which is the key twin's rule.
                                Pill::Home => {
                                    if !nav_cancel(route, &mut nav_pending)
                                        && crate::ui::home::snap_pos() < 0.5
                                    {
                                        crate::ui::home::set_hero_focus(
                                            crate::ui::home::hero_focus_for_pill(0),
                                        );
                                    }
                                }
                            }
                        } else if crate::ui::home::snap_pos() < 0.5 {
                            // hero visible: clicks act on the action row via the ONE activation.
                            // Only the two CONTROLS are hit-tested — the pager chevron beside them
                            // is an indicator with no rect, so paging is the D-pad's and the
                            // auto-flip's (this used to arm a click-hold pager here).
                            let b = crate::ui::home::hero_button_at(cx, cy);
                            if b >= 0 {
                                // Park the ring FIRST — the commit re-reads `hero_focus` — then dip
                                // the face and let the per-frame arm run the ONE activation. The
                                // status read-out's Retry is hit-tested through this same rect
                                // array and is not a control face, so it keeps acting at once
                                // (`home::focus_is_ctl` is what tells them apart, here as on OK).
                                crate::ui::home::set_hero_focus(b);
                                if crate::ui::home::focus_is_ctl() {
                                    crate::ui::press::begin_ctl(last_input);
                                    ok_armed = true;
                                } else {
                                    home_activate(
                                        mt,
                                        b,
                                        HUD_LINGER_MS,
                                        &mut route,
                                        &mut play_from,
                                        &mut trail,
                                        &mut hud.nav,
                                        &mut nav_pending,
                                    );
                                }
                            }
                        } else if crate::ui::home::home_card_click(cx, cy) {
                            // grid card: click = OK (play a Continue-Watching tile / open detail)
                            home_activate(
                                mt,
                                c_int::MIN,
                                HUD_LINGER_MS,
                                &mut route,
                                &mut play_from,
                                &mut trail,
                                &mut hud.nav,
                                &mut nav_pending,
                            );
                        }
                    } else if matches!(route, Route::Search) {
                        let (cx, cy) = ptr_xy(&ev);
                        // The strip is shared chrome and is hit-tested here, not by the screen —
                        // `tab_pill_at` owns the clipped rects, so a pill scrolled half out of the
                        // track is clickable across exactly the half you can see.
                        if let Some(i) = crate::ui::widgets::tab_pill_at(cx, cy) {
                            match crate::ui::widgets::pill_at(i) {
                                Pill::Search => {} // the screen we are already on
                                Pill::Section(kind) => {
                                    nav_to(route, Nav::Library(kind), &mut nav_pending)
                                }
                                Pill::Home => nav_to(
                                    route,
                                    Nav::Home {
                                        focus_pill: Some(crate::ui::widgets::Pill::Home),
                                    },
                                    &mut nav_pending,
                                ),
                            }
                        } else if let crate::ui::search::Action::Open(node) =
                            crate::ui::search::click(cx, cy)
                        {
                            nav_open(route, node, None, &mut nav_pending);
                        }
                    } else if matches!(route, Route::Library) {
                        let (cx, cy) = ptr_xy(&ev);
                        match crate::ui::library::click(cx, cy) {
                            crate::ui::library::Action::GoSearch => {
                                nav_to(route, Nav::Search, &mut nav_pending);
                            }
                            crate::ui::library::Action::GoHome => {
                                // `library::click` has already parked focus on the Home pill, so
                                // `focused_pill()` is the pill the capsule is under
                                nav_to(
                                    route,
                                    Nav::Home {
                                        focus_pill: crate::ui::library::focused_pill(),
                                    },
                                    &mut nav_pending,
                                )
                            }
                            crate::ui::library::Action::Card => {
                                open_library_card(route, &mut nav_pending);
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
                                        &mut route,
                                        &mut play_from,
                                        &mut hud.nav,
                                        &mut nav_pending,
                                    );
                                }
                            }
                            crate::ui::library::Action::None => {}
                        }
                    } else if matches!(route, Route::Detail) {
                        // Magic-Remote click on the detail page: focus what was clicked, then run the
                        // SAME activation the OK key does (detail::click did the hit-test) — a CARD
                        // (episode / Related / Cast) gets the tvOS press dip, committed on the
                        // button-up spring-back below — and so, since the control faces landed, do
                        // the Play pill, the watched discs and the season tabs. Every one of them
                        // defers now; this comment said they still acted at once.
                        let (cx, cy) = ptr_xy(&ev);
                        if crate::ui::detail::click(cx, cy) {
                            if crate::ui::detail::focus_is_card() {
                                crate::ui::press::begin(last_input);
                                ok_armed = true;
                            } else if crate::ui::detail::focus_is_ctl() {
                                crate::ui::press::begin_ctl(last_input);
                                ok_armed = true;
                            } else if crate::ui::detail::on_ok() {
                                start_playback(
                                    mt,
                                    crate::ui::detail::last_resume_ns(),
                                    origin_here(route), // Stop/BACK/EOS returns to this detail page
                                    HUD_LINGER_MS,
                                    &mut route,
                                    &mut play_from,
                                    &mut hud.nav,
                                );
                            }
                        }
                    } else if matches!(route, Route::Person) {
                        let (cx, cy) = ptr_xy(&ev);
                        if matches!(
                            crate::ui::person::click(cx, cy),
                            crate::ui::person::Action::Card
                        ) {
                            open_person_card(route, &mut nav_pending);
                        }
                    } else if let Route::Account { over } = route {
                        let (cx, cy) = ptr_xy(&ev);
                        // a click on a row commits it; anywhere else dismisses the popover
                        match crate::ui::account_menu::click(cx, cy) {
                            crate::ui::account_menu::Action::ChangeProfile => {
                                crate::auth::start_switch(crate::auth::Picker::ChangeProfile);
                                crate::ui::profiles::enter();
                                route = Route::Profiles;
                            }
                            crate::ui::account_menu::Action::SignIn => {
                                crate::auth::start_login();
                                crate::ui::login::enter();
                                route = Route::Login;
                            }
                            crate::ui::account_menu::Action::SignOut => {
                                crate::auth::sign_out();
                                crate::ui::login::enter();
                                route = Route::Login;
                            }
                            // the pointer twin of `key_account`'s Legal arm
                            crate::ui::account_menu::Action::Settings => {
                                crate::ui::settings::open();
                                route = over.route();
                            }
                            // the pointer twin of `key_account`'s arm — lab builds only
                            crate::ui::account_menu::Action::SendDiagnostics => {
                                crate::lab::request_upload("menu");
                                route = over.route();
                            }
                            crate::ui::account_menu::Action::None => {
                                // back to the PAGE the popover is on — the pointer's twin of
                                // `key_account`'s BACK arm
                                crate::ui::account_menu::close();
                                route = over.route();
                            }
                        }
                    } else if let Route::ItemMenu { over } = route {
                        let (cx, cy) = ptr_xy(&ev);
                        // a click on a row commits it; anywhere else dismisses the popover. THIS arm
                        // existing before the Home arm below is what keeps a click off the panel
                        // from falling through onto the shelf and launching whatever card it hit —
                        // the failure `modal_of` was written for. (`modal_of` itself is only
                        // consulted inside the Player branch, so its ItemMenu case is there for the
                        // same completeness as `Modal::Account`, not because this arm reads it.)
                        let act = crate::ui::item_menu::click(cx, cy);
                        route = over.route();
                        apply_item_action(
                            mt,
                            act,
                            over,
                            &mut route,
                            &mut play_from,
                            &mut trail,
                            &mut hud.nav,
                            &mut nav_pending,
                        );
                    } else if matches!(route, Route::Profiles) {
                        let (cx, cy) = ptr_xy(&ev);
                        // an avatar (a card) or the Sign-out footer (a control face): park focus,
                        // dip it, and let `activate_focused` spend the press on the spring-back —
                        // the same two predicates the key arm asks, in the same order.
                        if crate::ui::profiles::press_at(cx, cy) {
                            if crate::ui::profiles::focus_is_avatar() {
                                crate::ui::press::begin(last_input);
                            } else {
                                crate::ui::press::begin_ctl(last_input);
                            }
                            ok_armed = true;
                        } else {
                            crate::ui::profiles::click(cx, cy);
                        }
                    } else if matches!(route, Route::Onboard) {
                        let (cx, cy) = ptr_xy(&ev);
                        // the action PILL is a control face → press it; a list row is not and
                        // still flips its pin on the button-down. `commit_onboarding` is what can
                        // finish the flow now, from the per-frame arm.
                        if crate::ui::onboard::press_at(cx, cy) {
                            crate::ui::press::begin_ctl(last_input);
                            ok_armed = true;
                        } else {
                            crate::ui::onboard::click(cx, cy);
                        }
                    } else if matches!(route, Route::Login) {
                        // one actionable thing on the login screen (retry on error) — click = OK
                        crate::ui::login::key(SDLK_RETURN, 0);
                    }
                } else if et == SDL_MOUSEBUTTONUP {
                    last_input = SDL_GetTicks();
                    // a click that armed the tvOS press (a detail card) releases on the button-up,
                    // the pointer's twin of the OK key-up: without it the dip would sit there until
                    // press.rs's dropped-key-up ceiling fired. A no-op when no press is in flight.
                    crate::ui::press::release(last_input);
                    if ptr.drag {
                        ptr.drag = false;
                        if scrub() >= 0 {
                            commit_seek(scrub(), &mut repause_at);
                        }
                        extend_hud(last_input, HUD_LINGER_MS);
                    }
                } else if et == SDL_MOUSEWHEEL {
                    last_input = SDL_GetTicks();
                    if last_input.wrapping_sub(ptr.last_wheel) > 250 {
                        ptr.last_wheel = last_input;
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
                            rd_f32(&ev, 32).round() as i32
                        } else {
                            rd_i32(&ev, 20)
                        };
                        // the wheel scrolls VERTICALLY only, and only on routes with a vertical
                        // flow (it used to drive home's focus behind every other screen)
                        //
                        // Item 13: a modal overlay takes the wheel BEFORE the route dispatch below
                        // ever sees it — otherwise a wheel tick over Settings/Consent/Legal fell
                        // through to whatever route sat behind the popover (Home's own hero/grid
                        // dive, a Detail scroll, …), which is the same ownership question the key
                        // ladder answers for a fresh press, asked here for the wheel instead.
                        // `on_updown` already forwards to the open document's own `move_by` once
                        // one is pushed (`legal.rs`/`consent.rs`), so there is no separate reader
                        // case to spell out here.
                        if dy == 0 {
                            // a fractional trackpad tick that rounded to nothing (hostsim), or a
                            // `wheel:0` token — not a step in either direction
                            continue;
                        }
                        let delta = if dy < 0 { 1 } else { -1 };
                        if crate::ui::consent::is_open() {
                            crate::ui::consent::on_updown(delta);
                        } else if crate::ui::legal::is_open() {
                            crate::ui::legal::on_updown(delta);
                        } else if settings_root_owns_input(
                            route,
                            crate::ui::settings::is_open(),
                            crate::ui::onboard::settings_mode(),
                        ) {
                            crate::ui::settings::on_updown(delta);
                        } else if matches!(route, Route::Home) {
                            if crate::ui::home::snap_pos() < 0.5 {
                                if dy < 0 {
                                    set_snap(1.0); // hero → dive into the grid
                                    set_fr(0);
                                }
                            } else if dy > 0 && g_fr() == 0 {
                                set_snap(0.0); // grid top → back up to the hero
                            } else {
                                crate::ui::home::home_wheel(dy);
                            }
                        } else if matches!(route, Route::Detail) {
                            crate::ui::detail::move_focus(
                                if dy < 0 { SDLK_DOWN } else { SDLK_UP } as c_int
                            );
                        } else if matches!(route, Route::Person) {
                            crate::ui::person::move_focus(if dy < 0 { SDLK_DOWN } else { SDLK_UP });
                        } else if matches!(route, Route::Library) {
                            crate::ui::library::wheel(dy);
                        } else if matches!(route, Route::Search) {
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
                    crate::textinput::on_event(&ev);
                }
            }
            fd[1] = if framedrop_on { SDL_GetPerformanceCounter() } else { fd[0] }; // ingest

            let now = SDL_GetTicks();
            // dev: /tmp/plxnative-autoplay auto-presses OK once
            //
            // **Never from the sign-in or the picker.** The auth flow hands its credentials to
            // the main thread through `take_ready`, which is polled only on those two routes; a
            // trigger that jumped to the player from the picker left a HALF-seated profile —
            // the worker had re-keyed the registry, but the session was never persisted and
            // `apply_pending` stayed set — so the next boot came up as the previous profile
            // and the offline-pick harness case found no cache record (device, 2026-09-06).
            // Waiting for the handoff costs a headless run the seconds the seating takes.
            if !auto_tried
                && !matches!(route, Route::Player { .. } | Route::Login | Route::Profiles)
                && now.wrapping_sub(t0) > 2000
            {
                auto_tried = true;
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
                        if let Some(pmm) = crate::ui::home::movie_at(pidx / COLS, pidx % COLS) {
                            let requested = crate::route::request_play_movie(pmm);
                            if requested {
                                crate::metadata::load_detail_now(pmm.sid, &pmm.rk);
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
                            origin_here(route),
                            HUD_HEADLESS_MS,
                            &mut route,
                            &mut play_from,
                            &mut hud.nav,
                        );
                    }
                }
            }
            if !grid_tried && now.wrapping_sub(t0) > 400 {
                grid_tried = true;
                // (plxnative-itemmenu rides along: its popover anchors off a GRID card, so the
                // headless entry has to snap into the grid first, exactly like plxnative-grid.)
                if crate::dev::flag("grid") || crate::dev::flag("itemmenu") {
                    set_snap(1.0);
                    set_fr(0);
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
                    route = Route::Library;
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
                    trail.push(Node::Search);
                    route = Route::Search;
                }
                // dev: /tmp/plxnative-heroidx=<n> jumps the rotating hero to pool index n (flip capture)
                if let Some(s) = crate::dev::read("heroidx") {
                    if let Ok(n) = s.parse::<c_int>() {
                        crate::ui::home::set_hero_idx(n);
                    }
                }
            }
            // Retry until Home exists: an injected test identity can still spend the first few
            // frames in bootstrap, and a one-shot timestamp would turn a slow sign-in into a
            // misleading "never entered overlay=settings" performance failure.
            if !settings_tried && now.wrapping_sub(t0) > 800 {
                if matches!(route, Route::Home) {
                    settings_tried = true;
                    crate::ui::settings::open();
                    match settings_boot.as_deref().map(str::trim).unwrap_or("root") {
                        "" | "root" => {}
                        "home" => {
                            perform_settings_action(crate::ui::settings::Action::Home, &mut route)
                        }
                        "privacy" => {
                            let current = crate::telemetry::consent::current().unwrap_or_default();
                            crate::ui::consent::open_settings(&current);
                        }
                        "legal" => crate::ui::legal::open(),
                        other => log(&format!(
                            "settings: unknown boot target {other:?}; opened root"
                        )),
                    }
                } else if now.wrapping_sub(t0) > 12_000 {
                    settings_tried = true;
                    log("settings: boot target timed out before Home became available");
                }
            }
            // dev: /tmp/plxnative-press simulates a real OK TAP on the focused grid card ONCE, so a
            // headless run exercises the whole dip → bounce → deferred-activate path end to end.
            // The release is scheduled explicitly rather than left to the lost-key-up net: that net
            // only fires at `press::MAX_HOLD_MS` (1000 ms), which is PAST `press::LONG_MS`, so a
            // down with no up is a press-and-HOLD — it latches long, never commits, and now opens
            // the item menu instead. A tap has to be a tap.
            if !press_tried && now.wrapping_sub(t0) > 1600 {
                press_tried = true;
                if crate::dev::flag("press")
                    && ((matches!(route, Route::Home) && crate::ui::home::focus_is_card())
                        || (matches!(route, Route::Library) && crate::ui::library::focus_is_card()))
                {
                    crate::ui::press::begin(now);
                    ok_armed = true;
                    // past MIN_DIP_MS (the dip must be seen), well short of LONG_MS
                    press_release_at = now.wrapping_add(150).max(1);
                }
            }
            if press_release_at != 0 && now.wrapping_sub(press_release_at) < 0x8000_0000 {
                press_release_at = 0;
                crate::ui::press::release(now);
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
            if !itemmenu_tried && now.wrapping_sub(t0) > 1800 {
                if crate::dev::flag("itemmenu") && matches!(route, Route::Home) {
                    itemmenu_tried = open_item_menu(&mut route) || now.wrapping_sub(t0) > 12_000;
                } else {
                    itemmenu_tried = true;
                }
            }
            // dev: /tmp/plxnative-detail=<ratingKey> opens that catalog item's detail page once
            if !detail_tried && now.wrapping_sub(t0) > 500 {
                detail_tried = true;
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
                                continue;
                            }
                        };
                        // BLOCKING, deliberately: the sub-triggers below replay move_focus/on_ok
                        // in THIS frame, and they walk sections() — which is hero-only until the
                        // item lands. `open_rk_now` resolves the catalog index itself; the old
                        // `open(idx)` arm here was the one caller that made `open` block for
                        // everyone, including Home's OK on a cold card (the "freeze for a second").
                        crate::ui::detail::open_rk_now(sid, rk);
                        log(&format!(
                            "plxnative-detail: rk={rk} server={} start",
                            sid.raw()
                        ));
                        push_detail(&mut trail, &mut route, sid, rk);
                        // dev: /tmp/plxnative-detailsec=N presses DOWN N times (headless episode/row
                        // capture). One press is one section EXCEPT inside a 2D block, where the first
                        // one moves within it: the episode filmstrip's still→metadata sub-row
                        // (`detail::EpRow`) and About's card→columns each take a press of their own.
                        if let Some(n) = crate::dev::read("detailsec") {
                            for _ in 0..n.parse::<u32>().unwrap_or(0) {
                                crate::ui::detail::move_focus(SDLK_DOWN as c_int);
                            }
                        }
                        // dev: /tmp/plxnative-detailcol=N then moves the focus N to the right
                        if let Some(n) = crate::dev::read("detailcol") {
                            for _ in 0..n.parse::<u32>().unwrap_or(0) {
                                crate::ui::detail::move_focus(SDLK_RIGHT as c_int);
                            }
                        }
                        // dev: /tmp/plxnative-tracks[=<page>] opens the Track-information panel
                        // over the page, at that 1-based page of its body. `detailsec`+`detailcol`
                        // +`detailok` can reach it by hand now that it opens from the About
                        // footer's Languages column, which is a FIXED index (section 5, col 2) —
                        // unlike the hero disc it used to hang off, whose index moved with the
                        // control set. This stays because it is still the only way to capture a
                        // SCROLL state: nothing else can page a body headlessly.
                        if let Some(pg) = crate::dev::read("tracks") {
                            if crate::ui::tracks_panel::is_available() {
                                crate::ui::tracks_panel::open();
                                crate::ui::tracks_panel::set_page(
                                    pg.trim().parse::<c_int>().unwrap_or(1),
                                );
                            }
                        }
                        // dev: /tmp/plxnative-detailok presses OK on whatever the two triggers
                        // above focused, WITHOUT the play path — the deterministic one-boot route
                        // to a section whose OK navigates rather than plays. Today that means the
                        // cast row → the person page (detailsec/detailcol pick the headshot); the
                        // press animation is skipped on purpose, this is the activation only.
                        if crate::dev::flag("detailok") {
                            crate::ui::detail::on_ok(); // a cast row raises a person request; the
                                                        // per-frame drain below routes on it, like every other OK path
                            // dev: /tmp/plxnative-filmography opens the person page's Filmography
                            // route straight away — the design's `startOn: filmography`. It is
                            // armed HERE, on the same frame the cast OK mounted the person store,
                            // because that route is an overlay the person SCREEN owns rather than
                            // a `Route` app.rs could navigate to. Pair it with
                            // `/tmp/plxnative-personcredits`, without which the list is empty on
                            // any automated boot (that trigger's doc says why).
                            if crate::dev::flag("filmography") {
                                crate::ui::filmography::open();
                            }
                        }
                        // dev: /tmp/plxnative-detailplay activates the focused control (headless play test)
                        if crate::dev::flag("detailplay") && crate::ui::detail::on_ok() {
                            start_playback(
                                mt,
                                crate::ui::detail::last_resume_ns(),
                                origin_here(route),
                                HUD_HEADLESS_MS,
                                &mut route,
                                &mut play_from,
                                &mut hud.nav,
                            );
                        }
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
            if !play_tried
                && !matches!(route, Route::Player { .. } | Route::Login | Route::Profiles)
                && now.wrapping_sub(t0) > 500
            {
                play_tried = true;
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
                                continue;
                            }
                        };
                        crate::metadata::load_detail_now(sid, rk); // fetch ANY rk (movie/show/episode)
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
                                        origin_here(route),
                                        HUD_HEADLESS_MS,
                                        &mut route,
                                        &mut play_from,
                                        &mut hud.nav,
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
            if !seek_tried
                && matches!(route, Route::Player { .. })
                && dur() > 0
                && now.wrapping_sub(t0) > 12000
            {
                seek_tried = true;
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
                            seek_gap_ms = g.parse().unwrap_or(300).max(50);
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
                    seek_script_last = crate::player::playpos_ns();
                    // The fire test is `now - seek_script_at >= seek_gap_ms`, so backing the origin
                    // off by one gap fires the first step at once and adding the delay pushes it
                    // out by exactly that much. `delay=0` is the historical behaviour, unchanged.
                    seek_script_at = now.wrapping_sub(seek_gap_ms).wrapping_add(first_delay_ms);
                    seek_script = steps;
                }
            }
            if !seek_script.is_empty()
                && matches!(route, Route::Player { .. })
                && script_step_due(now, seek_script_at, seek_gap_ms)
            {
                let step = seek_script.remove(0);
                seek_script_at = now;
                let t = if let Some(r) = step.strip_prefix('+') {
                    seek_script_last + r.parse::<i64>().unwrap_or(0) * 1_000_000_000
                } else if let Some(r) = step.strip_prefix('-') {
                    seek_script_last - r.parse::<i64>().unwrap_or(0) * 1_000_000_000
                } else {
                    step.parse::<i64>().unwrap_or(140) * 1_000_000_000
                }
                .max(0);
                seek_script_last = t;
                log(&format!(
                    "autoseek: step → {}s ({} left)",
                    t / 1_000_000_000,
                    seek_script.len()
                ));
                request_seek(t);
            }
            // dev: /tmp/plxnative-qualityswitch — change the playback quality WHILE IT PLAYS,
            // which is what a person does at the television and what no boot override can reach:
            // it re-asks the routing question against a stream already on screen, reloads if the
            // answer moved, and on the way out of Auto tears down a running ABR controller.
            // Armed on the same gate as the seek script, so the two are comparable and neither
            // fires into a session that has not settled.
            if !quality_tried {
                // Explicit test SLO: observe twelve uninterrupted seconds of PLAYING before the
                // first request. This is not playback policy; it keeps a slow boot or pre-roll
                // from consuming the observation window that is meant to establish the initial
                // route and, when present, its ABR controller.
                const QUALITY_SWITCH_OBSERVE_MS: u32 = 12_000;
                let playing = matches!(route, Route::Player { .. })
                    && dur() > 0
                    && crate::player::is_playing();
                if !playing {
                    quality_playing_since = None;
                } else {
                    let since = *quality_playing_since.get_or_insert(now);
                    if now.wrapping_sub(since) >= QUALITY_SWITCH_OBSERVE_MS {
                        quality_tried = true;
                        if let Some((gap, qs)) = crate::dev::quality_switch_script() {
                            quality_gap_ms = gap;
                            quality_script_at = now.wrapping_sub(gap); // fire the first step now
                            quality_script = qs;
                        }
                    }
                }
            }
            if !quality_script.is_empty()
                && matches!(route, Route::Player { .. })
                && script_step_due(now, quality_script_at, quality_gap_ms)
            {
                let q = quality_script.remove(0);
                quality_script_at = now;
                // Logged BEFORE the call, because `set_quality` may reload the engine and the
                // line has to survive that to say what was asked for. The harness reads this to
                // pair each switch with what playback did after it.
                log(&format!(
                    "quality: switch → {} ({} left)",
                    crate::dev::quality_wire_name(q),
                    quality_script.len()
                ));
                crate::route::set_quality(q);
            }
            // dev: /tmp/plxnative-autopause pauses once (headless paused-HUD capture), or carries
            // `delay=<ms>,hold=<ms>` for a deterministic Pause -> Resume playback transaction.
            if !pause_tried && matches!(route, Route::Player { .. }) && now.wrapping_sub(t0) > 6000
            {
                pause_tried = true;
                if let Some(script) = crate::dev::pause_script() {
                    pause_script = Some((now.wrapping_add(script.delay_ms), script.hold_ms));
                }
            }
            if let Some((pause_at, hold_ms)) = pause_script {
                if matches!(route, Route::Player { .. }) && script_step_due(now, pause_at, 0) {
                    if set_transport_paused(mt, true) {
                        log(&format!(
                            "autopause: Pause accepted hold={}ms",
                            hold_ms.map_or_else(|| "forever".to_string(), |ms| ms.to_string()),
                        ));
                        pause_script = None;
                        pause_resume_at = hold_ms.map(|hold| now.wrapping_add(hold));
                        set_hud(now + HUD_HEADLESS_MS);
                    }
                }
            }
            if let Some(resume_at) = pause_resume_at {
                if matches!(route, Route::Player { .. }) && script_step_due(now, resume_at, 0) {
                    if set_transport_paused(mt, false) {
                        log("autopause: Resume accepted");
                        pause_resume_at = None;
                    }
                }
            }
            // dev: /tmp/plxnative-menu=<tab> opens the in-player track menu once (headless capture)
            if !menu_tried && matches!(route, Route::Player { .. }) && now.wrapping_sub(t0) > 6000 {
                menu_tried = true;
                if let Some(t) = crate::dev::read("menu") {
                    crate::ui::track_menu::open_tab(t.parse::<c_int>().unwrap_or(0));
                    route = Route::Player {
                        overlay: Overlay::Menu,
                    };
                    set_hud(now + HUD_HEADLESS_MS);
                }
                // dev: /tmp/plxnative-info opens the Info card once (headless capture)
                if crate::dev::flag("info") {
                    crate::ui::info_panel::open();
                    route = Route::Player {
                        overlay: Overlay::Info,
                    };
                    hud.nav.focus = 2;
                    hud.nav.tab = 0;
                    set_hud(now + HUD_HEADLESS_MS);
                }
                // dev: /tmp/plxnative-chapters opens the Chapters strip once (headless capture)
                if crate::dev::flag("chapters") {
                    crate::ui::chapters_panel::open();
                    route = Route::Player {
                        overlay: Overlay::Chapters,
                    };
                    hud.nav.focus = 2;
                    hud.nav.tab = 1;
                    set_hud(now + HUD_HEADLESS_MS);
                }
            }
            // dev: /tmp/plxnative-menupick="<tab>,<row>" opens the menu, selects that row, and
            // confirms it (headless track switch: e.g. "0,4" = audio tab, row 4).
            if !menupick_tried
                && matches!(route, Route::Player { .. })
                && now.wrapping_sub(t0) > 7000
            {
                menupick_tried = true;
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
            if !marker_tried && matches!(route, Route::Player { .. }) && crate::player::is_playing()
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
                            marker_tried = true;
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
                    None => marker_tried = true,
                }
            }
            if is_started() {
                crate::player::pump(mt, now);
            }
            let _ = poll_foreground_load(
                &mut foreground,
                &mut PlayerForegroundActuator {
                    mt,
                    repause_at: &mut repause_at,
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
            if matches!(route, Route::Player { .. }) && crate::player::ended() {
                finish_playback(
                    mt,
                    &mut route,
                    &mut play_from,
                    &mut refresh_hubs_at,
                    &mut hud.nav,
                    &mut trail,
                );
                held_key.sym = 0; // async route flip: don't repeat a still-held key into detail/home
                                  // dev: REPLAY AFTER COMPLETION (#46). `finish_playback` has just left the player —
                                  // an Up Next handoff would have RETURNED there, and `matches!` below is what tells
                                  // the two apart, so a replay can never cut into an auto-advance chain. Re-arming
                                  // `auto_tried` sends the next frame back through the `playurl` entry, which calls
                                  // `route::clear_url()` and lets `start_bufferfeed` read the trigger again.
                                  //
                                  // The trigger is read once at boot (`replay_left`), so this cannot be turned into
                                  // an endless loop by a file appearing mid-run, and `dev::flag` is `false` at
                                  // COMPILE time in a release build.
                if replay_left > 0
                    && !matches!(route, Route::Player { .. })
                    && crate::dev::flag("playurl")
                {
                    replay_left -= 1;
                    auto_tried = false;
                    log(&format!(
                        "replay: starting the finished stream again ({replay_left} left)"
                    ));
                }
            }
            // Up Next countdown elapsed → start the queued episode on its own. Beside the EOS
            // handoff so the whole auto-advance chain reads in one place.
            if matches!(route, Route::Player { .. }) && crate::ui::up_next::expired(now) {
                if !play_up_next(mt, HUD_LINGER_MS, &mut route, &mut play_from, &mut hud.nav) {
                    crate::ui::up_next::cancel(); // nothing queued after all — don't re-fire
                }
                held_key.sym = 0;
            }
            // post-playback home refresh (armed by every exit_player): refetch the hubs so
            // Continue Watching shows the new resume point / next episode; the small delay lets
            // the final timeline PUT land server-side first. The request is worker-only; the
            // landing logs the resulting item count when it actually commits.
            if refresh_hubs_at != 0
                && now.wrapping_sub(refresh_hubs_at) < 0x8000_0000
                && !matches!(route, Route::Player { .. })
            {
                refresh_hubs_at = 0;
                crate::pms::request_refetch_hubs();
                // …and every library's OWN shelves, for the same reason and at the same moment:
                // a finished playback moves Continue Watching and watch state, and a section deck
                // is as stale as the global one (`browse::section_hubs::invalidate_all`).
                crate::browse::section_hubs::invalidate_all();
                log("home: hubs refresh queued after playback");
            }
            // lost-keyup safety: the remote streams 0x101 repeats (~50ms) while a key is physically down,
            // so once past the initial settle a stale heartbeat means the release keyup was dropped —
            // clear the held key so it can't repeat forever (mirrors the scrub's SCRUB_LOST_MS). The
            // 500ms gate leaves the first repeat and the heartbeat's own start-up untouched; a normal
            // release clears via the keyup long before this fires.
            if held_key.sym != 0
                && now.wrapping_sub(held_key.since) > 500
                && now.wrapping_sub(held_key.alive) > 350
            {
                held_key.sym = 0;
            }
            // client-side long-press repeat — the ONE hold-to-move path for every discrete focus list
            // (home grid, detail, track menu, info card, chapters). Driven by a held-key timer so it's
            // identical everywhere and independent of the remote's hardware auto-repeat delay.
            // `HeldKey::arm` is what each view's fresh-press handler calls (always with a standard
            // SDLK_*), and the keyup clears `sym`. The player scrubber is deliberately excluded —
            // holding it runs the continuous scrub.
            if held_key.sym != 0
                && now.wrapping_sub(held_key.since) > 380
                && now.wrapping_sub(held_key.last_rep) > 110
            {
                held_key.last_rep = now;
                match route {
                    Route::Home if g_snap() > 0.5 => crate::ui::home::home_move_focus(held_key.sym),
                    Route::Home => crate::ui::home::home_hero_key(held_key.sym), // hero view: hold LEFT/RIGHT pages the billboard
                    Route::ItemMenu { .. } => {
                        crate::ui::item_menu::move_focus(held_key.sym as c_int)
                    }
                    Route::Library => crate::ui::library::move_focus(held_key.sym),
                    Route::Search => crate::ui::search::move_focus(held_key.sym),
                    Route::Detail => crate::ui::detail::move_focus(held_key.sym as c_int),
                    // **The person page, which was the one focus surface missing from this table**
                    // — so holding a direction there moved nothing, on the page itself and on the
                    // Filmography route over it. Reported 2026-09-06 against the filmography, where
                    // a career is hundreds of rows and stepping them one press at a time is not a
                    // list you can read; but the shelves underneath had the same hole, and adding
                    // the route alone would have left the page it stands on still unable to repeat.
                    //
                    // `person::move_focus` already forwards to whichever overlay is up, which is
                    // why ONE arm covers both and why nothing here needs to know the route exists.
                    Route::Person => crate::ui::person::move_focus(held_key.sym),
                    Route::Player {
                        overlay: Overlay::Menu,
                    } => {
                        crate::ui::track_menu::move_focus(held_key.sym as c_int);
                        extend_hud(now, HUD_MENU_MS);
                    }
                    Route::Player {
                        overlay: Overlay::More,
                    } => {
                        crate::ui::more_menu::move_focus(held_key.sym as c_int);
                        extend_hud(now, HUD_MENU_MS);
                    }
                    Route::Player {
                        overlay: Overlay::Info,
                    } => {
                        crate::ui::info_panel::move_focus(held_key.sym as c_int);
                        extend_hud(now, HUD_MENU_MS);
                    }
                    Route::Player {
                        overlay: Overlay::Chapters,
                    } => {
                        crate::ui::chapters_panel::move_focus(held_key.sym as c_int);
                        extend_hud(now, HUD_MENU_MS);
                    }
                    _ => {}
                }
            }
            // keep the HUD alive while the track menu / Info card / Chapters strip is open
            if matches!(route, Route::Player { overlay } if overlay != Overlay::None) {
                extend_hud(now, HUD_LINGER_MS);
            }
            // scrub: continuous accelerating advance while a key is held (`hold` set by 0x101).
            if scrubber.dir != 0 && scrubber.hold && scrub() >= 0 && !ptr.drag {
                let held = now.wrapping_sub(scrubber.hold_since) as f32 / 1000.0;
                let speed = (SCRUB_BASE + SCRUB_ACCEL * held).min(SCRUB_MAX);
                let mut sdt = now.wrapping_sub(scrubber.t) as f32 / 1000.0;
                if sdt > 0.1 {
                    sdt = 0.1;
                }
                let was = scrub();
                let mut s = was + (scrubber.dir as f64 * speed as f64 * sdt as f64 * 1e9) as i64;
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
                    scrubber.reveal = false;
                }
                extend_hud(now, HUD_LINGER_MS);
                scrubber.t = now;
                // lost-keyup safety: commit if the 0x101 repeats stop without a keyup
                if now.wrapping_sub(scrubber.alive) > SCRUB_LOST_MS {
                    commit_seek(scrub(), &mut repause_at);
                    scrubber.disengage();
                }
            }
            // tap release debounce: commit the accumulated jump(s) once no further tap arrives
            if scrubber.commit_at != 0 && now.wrapping_sub(scrubber.commit_at) < 0x8000_0000 {
                if scrub() >= 0 {
                    log(&format!("scrub: tap commit {}s", scrub() / 1_000_000_000));
                    commit_seek(scrub(), &mut repause_at);
                } else {
                    set_scrub(-1);
                }
                scrubber.disengage();
                scrubber.commit_at = 0;
            }
            // Focus follows the control row's OCCUPANT, on both edges. Driven by slot identity
            // rather than a "was something shown" bool, because the two edges have different jobs
            // and the previous bool implemented neither of the ones its comment promised.
            if matches!(
                route,
                Route::Player {
                    overlay: Overlay::None
                }
            ) {
                // Keyed on the SEGMENT, not the slot, and `last_offer` is only ever advanced to
                // a real offer — never cleared back to None. `active_marker` is gated on `is_playing`,
                // so a momentary drop out of Playing mid-segment reads as "no segment" and flips the
                // row to the discs and back; keyed on the slot that round trip looked like a new
                // offer and re-raised the HUD over an intro the user was simply watching.
                let offer = ctrl.offer();
                let fresh = offer.is_some() && offer != hud.last_offer;
                if offer.is_some() {
                    hud.last_offer = offer;
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
                    hud.raise_for_offer(now, ctrl.primary_btn());
                } else if crate::ui::player_hud::standin_left_the_ring(
                    hud.was_standin,
                    ctrl,
                    hud.nav.focus == 1,
                ) {
                    // The stand-in went away under the focus ring. Without this the row swaps back
                    // to the discs with focus still on it and `btn` still 0, so the next OK opened
                    // the SUBTITLES menu instead of toggling pause — exactly the bug class HudNav's
                    // own doc says it exists to kill. Strictly the EDGE: as a steady state it also
                    // fired on a user who walked UP to the discs on purpose, yanking the ring back
                    // the same frame and making OK on a disc unreachable by remote.
                    hud.nav = HudNav::HOME;
                }
                hud.was_standin = !ctrl.is_discs();
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
            if matches!(route, Route::Player { .. }) && crate::ui::up_next::armed() {
                if crate::ui::up_next::countdown_may_run(
                    matches!(
                        route,
                        Route::Player {
                            overlay: Overlay::None
                        }
                    ),
                    hud.nav.focus == 1,
                    hud.nav.btn,
                ) {
                    hud.dismissed = false;
                    extend_hud(now, HUD_LINGER_MS);
                } else {
                    crate::ui::up_next::cancel();
                }
            }
            // when the HUD auto-hides, park focus back on the scrubber so the next reveal is clean
            if matches!(route, Route::Player { .. })
                && !hud_visible(now, hud_until(), paused(), hud.dismissed)
            {
                hud.nav = HudNav::HOME;
            }
            // hide the idle pointer during playback
            if matches!(route, Route::Player { .. })
                && !ptr.cur_hidden
                && !ptr.drag
                && ptr.last_motion != 0
                && now.wrapping_sub(ptr.last_motion) > 3000
            {
                hide_cursor();
                ptr.cur_hidden = true;
            }
            // re-pause after a resume the INSTANT the seek's frame is on screen. `frames()` counts
            // real "frame presented" callbacks (reset on seek), so >= 1 means the target frame is
            // already composited — re-freezing then shows it with the shortest possible play-blip
            // (a paused scrub must briefly Play to decode the frame; buffer-feed has no preroll).
            if resume_pend()
                && matches!(route, Route::Player { .. })
                && crate::player::seek_preroll_active()
                && seek_pending() < 0
                && frames() >= 1
                && playpos() + 15 * 1_000_000_000 >= repause_at
            {
                crate::player::finish_paused_seek(mt);
            }

            let dt = {
                let mut d = if prev != 0 {
                    now.wrapping_sub(prev) as f32 / 1000.0
                } else {
                    0.016
                };
                if d > 0.05 {
                    d = 0.05;
                }
                d
            };
            prev = now;
            // Whole-frame present gate (`ui::idle`): forget last frame's motion BEFORE the update
            // phase below re-steps every spring, so the flag it leaves describes THIS frame, and
            // stamp `dt` so a spring's velocity can be judged as travel-this-frame rather than as
            // a bare units-per-second. The decision itself is taken just above `glViewport`.
            crate::ui::idle::frame_begin(dt);
            // ui::press (tvOS click) — advance the dip/spring every frame; when a deferred activation
            // commits (the spring-back bounce has played), run it for whichever CARD view armed the
            // press. A long-press does NOT commit (`press::tick` clears `want_commit` at `LONG_MS`):
            // on Home it opens the item menu below, and anywhere else it just springs back.
            let (_, press_moving) = crate::ui::idle::scoped_motion(|| {
                crate::ui::press::tick(now, dt);
            });
            // The motion of whatever page is UNDER a popover — Home, the Library or Search, since
            // the profile chip is a stop on all three. It was `home_underlay_moving` while only Home
            // could be underneath. The account popover's glass re-snapshots off this.
            let mut underlay_moving = press_moving;
            if ok_armed {
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
                let held_menu = crate::ui::press::is_long(now)
                    && match route {
                        // the grid, not the hero: the hero has no card to anchor a panel beside
                        Route::Home => {
                            crate::ui::home::snap_pos() >= 0.5 && open_item_menu(&mut route)
                        }
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
                        Route::Detail => {
                            open_season_menu(&mut route)
                                || open_episode_menu(&mut route)
                                || open_tile_menu(
                                    &mut route,
                                    MenuHost::Related,
                                    crate::ui::detail::focused_related(),
                                    Opener {
                                        rect: crate::ui::detail::focused_related_rect(),
                                        redraw: crate::ui::detail::redraw_focused_related,
                                    },
                                    false, // Related is a recommendation shelf, never a deck
                                )
                        }
                        // …and the three other card surfaces, which armed this press already and
                        // did nothing with it. Each declines by handing `None` — a grid page still
                        // loading, focus in Search's field or on a `Tag` shelf, the person page's
                        // header row — and the hold then falls through to the ordinary spring-back.
                        Route::Library => open_tile_menu(
                            &mut route,
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
                            &mut route,
                            MenuHost::Search,
                            crate::ui::search::focused_media(),
                            Opener {
                                rect: crate::ui::search::focused_tile_rect(),
                                redraw: crate::ui::search::redraw_focused_tile,
                            },
                            false, // results are a query's answer, not a deck
                        ),
                        Route::Person => open_tile_menu(
                            &mut route,
                            MenuHost::Person,
                            crate::ui::person::focused_item(),
                            Opener {
                                rect: crate::ui::person::focused_tile_rect(),
                                redraw: crate::ui::person::redraw_focused_tile,
                            },
                            false, // a filmography is a credit list, not a deck
                        ),
                        _ => false,
                    };
                if held_menu {
                    ok_armed = false;
                    crate::ui::press::cancel();
                } else if crate::ui::press::take_commit(now) {
                    ok_armed = false;
                    // The deferred activation, dispatched by asking the SAME questions the key
                    // ladder asked when it armed the press, in the SAME order. The modal panel
                    // comes first here because it comes first there: consent stands OVER a route
                    // that has its own arm below, so a match on `route` alone would commit a
                    // consent press as a Home activation.
                    if crate::ui::consent::is_open() {
                        commit_consent(&mut route, &mut trail);
                    } else if matches!(route, Route::Onboard) {
                        if let Some(next) = apply_onboarding_action(commit_onboarding(), &mut trail)
                        {
                            route = next;
                        }
                    } else {
                        match route {
                            // `Account { over: Home }` and not every `Account`: the popover can stand on
                            // three pages now, and a press armed on a Library card must not commit as a
                            // HOME activation because a panel happened to open over it. (Reaching either
                            // is near-impossible — a nav key cancels the press — but the arm has to say
                            // which page it means.)
                            Route::Home
                            | Route::Account {
                                over: BarHost::Home,
                            } => {
                                // WHICH press this was, re-asked rather than remembered: the hero's
                                // action row arms one and so does the grid, and `home_activate`
                                // needs the focus value to tell them apart. Sound because focus
                                // cannot move under a press (a nav key cancels it), so the answer is
                                // the one that was true when the key went down — and the grid's
                                // sentinel is what a hero pill or a Retry press could never be,
                                // since neither arms a press at all.
                                let hf = if crate::ui::home::focus_is_ctl() {
                                    crate::ui::home::hero_focus()
                                } else {
                                    c_int::MIN
                                };
                                home_activate(
                                    mt,
                                    hf,
                                    HUD_LINGER_MS,
                                    &mut route,
                                    &mut play_from,
                                    &mut trail,
                                    &mut hud.nav,
                                    &mut nav_pending,
                                );
                            }
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
                                            &mut route,
                                            &mut play_from,
                                            &mut hud.nav,
                                            &mut nav_pending,
                                        );
                                    }
                                }
                                // the paged grid, and — for totality — the zones that cannot arm a
                                // press at all (`focus_is_card` is Grid or Shelf only)
                                _ => open_library_card(route, &mut nav_pending),
                            },
                            // ONE arm for the page's cards AND its hero control row: `on_ok`
                            // already resolves which, exactly as it does on the immediate path.
                            Route::Detail => {
                                if crate::ui::detail::on_ok() {
                                    start_playback(
                                        mt,
                                        crate::ui::detail::last_resume_ns(),
                                        origin_here(route),
                                        HUD_LINGER_MS,
                                        &mut route,
                                        &mut play_from,
                                        &mut hud.nav,
                                    );
                                }
                            }
                            Route::Person => {
                                if matches!(
                                    crate::ui::person::on_ok(),
                                    crate::ui::person::Action::Card
                                ) {
                                    open_person_card(route, &mut nav_pending);
                                }
                            }
                            Route::Search => {
                                if let crate::ui::search::Action::Open(node) =
                                    crate::ui::search::on_ok()
                                {
                                    nav_open(route, node, None, &mut nav_pending);
                                }
                            }
                            // an avatar or the Sign-out footer — the screen resolves which
                            Route::Profiles => crate::ui::profiles::activate_focused(),
                            // the transport's control row (discs or a stand-in)
                            Route::Player {
                                overlay: Overlay::None,
                            } => activate_player_row(
                                mt,
                                ctrl,
                                now,
                                &mut route,
                                &mut hud,
                                &mut held_key,
                                &mut trail,
                                &mut play_from,
                                &mut refresh_hubs_at,
                            ),
                            // the Info card's action column
                            Route::Player {
                                overlay: Overlay::Info,
                            } => commit_info_panel(
                                mt,
                                now,
                                &mut route,
                                &play_from,
                                &mut refresh_hubs_at,
                                &mut trail,
                            ),
                            _ => {}
                        }
                    }
                } else if !crate::ui::press::is_active() {
                    ok_armed = false; // long-press / cancelled — disarm without activating
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
            if crate::ui::person::take_request() && !matches!(route, Route::Player { .. }) {
                if let Some(p) = crate::person::current() {
                    // the store was installed by `person::open` on this same press, so these four
                    // are the header the cast row handed over — `person::reopen`'s arguments
                    let node = Node::Person {
                        sid: p.sid,
                        key: p.key.clone(),
                        guid: p.guid.clone(),
                        name: p.name.clone(),
                        thumb: p.thumb.clone(),
                    };
                    nav_open(route, node, None, &mut nav_pending);
                }
            }

            // Its twin for a detail page opening ANOTHER detail page — the episode filmstrip's text
            // row and the Related shelf, which used to call `open_rk` themselves and leave the trail
            // describing a page that was no longer on screen (the reported bug: BACK from an episode
            // page went to Home). Drained here for the same reason the cast request is: `on_ok` is
            // reached from four places and a poll beside each of them is a latch waiting to fire on
            // an unrelated OK.
            //
            // The route guard is what keeps the one Detail→Detail transition honest: `on_ok` also
            // runs from `home_activate`'s play-a-show arm and the `plxnative-detailok` trigger, and
            // a request raised off-route must not push. Drained unconditionally either way, because
            // a latch left set is exactly what it must never become.
            //
            // Detail→Detail is also the arm that forced the MOUNT to the fade floor for every
            // destination: `open_rk` clears the loaded item, so calling it on the press frame would
            // collapse the outgoing page to a hero-and-spinner *while it is still fading out*.
            // Where the outgoing page was standing rides `NavReq::spot` like every other navigation
            // off a detail page — `nav_req` reads `leaving_spot` on this same frame — so the request
            // itself carries only the destination.
            if let Some((sid, rk)) = crate::ui::detail::take_open_request() {
                if matches!(route, Route::Detail) {
                    nav_open(route, to_detail(sid, &rk), None, &mut nav_pending);
                }
            }

            // "Also available" (`ui::alt_sources`): the detail page reports the press, the panel is
            // PRESENTED here — beside the control's drawn rect, the same division `item_menu` keeps
            // with `home::focused_card_rect`. It is not a route: the page stays live behind it, and
            // `detail::back()` is what a BACK spends on it.
            if crate::ui::detail::take_alt_request() && matches!(route, Route::Detail) {
                if let Some(r) = crate::ui::detail::alt_btn_rect() {
                    crate::ui::alt_sources::open(r);
                }
            }
            // …and a copy CHOSEN in that panel: open that server's own page for the film. Handled
            // here rather than by the screen for the same reason every other navigation request is
            // — `app.rs` owns the route and the trail — and NOT because anything needs re-pointing.
            if let Some((sid, rk)) = crate::ui::detail::take_alt_open() {
                // **Opening the other copy is a NAVIGATION, not a session change.**
                //
                // This used to `set_current(sid)` + `activate_server()` + `trail.reset()`. That
                // wiped the section table and re-discovered only the newly-current server, so one
                // press on "Also available" replaced the whole top tab strip with the friend's
                // single library — owner-reported, and visible in the log as
                // `altsources: source switched to slot 1` followed by `nsections=1`.
                //
                // Nothing needs re-pointing: `to_detail` carries the pair, `Detail` is parsed with
                // that `sid`, and every surface the page draws — art, logo, cast, Related, Play,
                // the watched toggle — resolves its own server from the item. Same rule as browsing
                // a shared library (`browse::activate_source_of`), which is now a documented no-op:
                // "current" is the SESSION's server, and neither of these is a session change.
                //
                // The trail survives too. It was reset because the pages behind could not name
                // their machine; `Node::Detail`/`Node::Person` carry a `ServerId` now.
                if matches!(route, Route::Detail) {
                    if crate::plex::client_for(sid).is_some() {
                        log(&format!("altsources: opening slot {} rk={rk}", sid.raw()));
                        nav_open(route, to_detail(sid, &rk), None, &mut nav_pending);
                    } else {
                        // a copy whose source is not registered (a share dropped from the roster,
                        // or the headless stand-in): say so and stay put, rather than opening this
                        // ratingKey on whatever machine happens to be current — which would
                        // confidently show a different film
                        log(&format!(
                            "altsources: no client for slot {} — not navigating",
                            sid.raw()
                        ));
                    }
                }
            }

            // login flow: install resolved creds on the MAIN thread, then follow the flow phase →
            // route (Login while creating/waiting/discovering/error, Profiles while picking/switching).
            if matches!(route, Route::Login | Route::Profiles) {
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
                    trail.reset();
                    // …and only NOW can the first-run question be asked: `install_pms` registers
                    // the granted roster, which is the stable input to this decision even before
                    // asynchronous section discovery lands. It is asked per PROFILE, which is why
                    // it sits after the switch rather than after the sign-in.
                    // The sign-in's question first, before any per-profile step. On a Plex Home
                    // account it was already asked at the picker below and this is a no-op; on a
                    // single-user account this is the earliest authorized moment there is.
                    maybe_ask_consent();
                    if crate::ui::onboard::asks() {
                        log("login: server installed — asking which sources feed Home");
                        crate::ui::onboard::enter();
                        route = Route::Onboard;
                    } else {
                        log("login: server installed — entering Home");
                        route = Route::Home;
                    }
                } else {
                    match crate::auth::phase() {
                        crate::auth::Phase::Profiles | crate::auth::Phase::Switching => {
                            // BEFORE the picker: the account is authorized, so the consent
                            // question is answerable, and the person holding the remote at this
                            // moment is the one who signed the television in. It draws over the
                            // picker's route on its own opaque ground.
                            maybe_ask_consent();
                            if route != Route::Profiles {
                                crate::ui::profiles::enter();
                            }
                            route = Route::Profiles;
                        }
                        _ => {
                            if route != Route::Login {
                                crate::ui::login::enter();
                            }
                            route = Route::Login;
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
                if want && route == Route::Home {
                    crate::ui::account_menu::open();
                    route = Route::Account {
                        over: BarHost::Home,
                    };
                } else if !want {
                    if let Route::Account { over } = route {
                        crate::ui::account_menu::close();
                        route = over.route();
                    }
                }
            }
            // dev: navosc bounces the route Home↔Library through the real request path (the
            // `home-library-nav` FPS scene). Route-unconditional, because it is the ROUTE it drives;
            // it goes through `nav_to` rather than assigning `route` so the scene measures exactly
            // what a tab press does, transition included.
            if nav_osc && now.wrapping_sub(nav_osc_last) > 1400 {
                nav_osc_last = now;
                match route {
                    // the DETAIL bounce is `nav_open` out and `nav_back` home — the same pair the
                    // grid card and the BACK key raise, teardown included, so the scene measures
                    // the whole round trip and not just its cheaper half
                    Route::Home if !nav_osc_rk.is_empty() => {
                        // a dev trigger names a bare rk, so it means "on the server we are signed
                        // in to" — the only server a headless boot has
                        nav_open(
                            route,
                            to_detail(crate::plex::current_server(), &nav_osc_rk),
                            None,
                            &mut nav_pending,
                        )
                    }
                    Route::Detail => nav_back(route, &trail, &mut nav_pending),
                    // the FIRST TYPE the strip actually draws — not `Movies` by name, which a
                    // set with no favourite film library does not have a pill for at all
                    Route::Home => {
                        if let Some(kind) = crate::browse::tab_kind(0) {
                            nav_to(route, Nav::Library(kind), &mut nav_pending)
                        }
                    }
                    // pill 1 is that same first TAB — not "the first section", which stopped being
                    // the same thing when the strip became a projection of the table (`browse::tabs`):
                    // several libraries can share one pill. Home comes back in the hero view with the
                    // top band on the pill the round trip started from, so the scene is a loop
                    Route::Library => nav_to(
                        route,
                        Nav::Home {
                            // the pill this round trip started from — by TYPE, so the scene is a
                            // loop whichever position that type's pill happens to occupy
                            focus_pill: Some(crate::ui::widgets::pill_at(1)),
                        },
                        &mut nav_pending,
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
            fd[2] = if framedrop_on { SDL_GetPerformanceCounter() } else { fd[0] }; // results
            if crate::ui::nav::tick(dt) {
                // Superseded: something else moved the app while this was fading. Drop the
                // request — the fader still completes, fading the screen the user actually has
                // back in — rather than flipping the screen out from under whatever landed.
                let req = nav_pending.take().filter(|r| route == r.from);
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
                        trail.set_top_spot(s);
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
                            trail.reset();
                            // `resume`, NOT `enter("")`: the trail reset above throws away the way
                            // IN, never the screen's own state. The pill is a way back to a search
                            // you already made — `library::enter`'s `restore_view` one screen over
                            // — and a fresh profile needs no special case for it, since the store
                            // it returns to is empty until something is typed into it.
                            crate::ui::search::resume();
                            trail.push(Node::Search);
                            route = Route::Search;
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
                            trail.reset();
                            trail.push(Node::Library);
                            route = Route::Library;
                        }
                        Nav::Home { focus_pill } => {
                            // keep the pill the user was standing on under focus, and put Home in
                            // the view where the top band's focus is visible. Resolved HERE, at the
                            // fade floor, against the strip as it is now — which is the whole point
                            // of carrying an identity: the pill may have arrived or gone since the
                            // press frame.
                            if let Some(i) = focus_pill.and_then(crate::ui::widgets::pill_of) {
                                crate::ui::home::set_hero_focus(
                                    crate::ui::home::hero_focus_for_pill(i),
                                );
                                set_snap(0.0);
                            }
                            // Home IS the root, so ARRIVING there is the trail's reset — which
                            // is also what makes BACK out of the Library correct without the arm
                            // popping anything itself, and cancel-safe: a withdrawn transition
                            // never reaches this frame.
                            trail.reset();
                            route = Route::Home;
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
                            if let (Node::Detail { sid, rk, .. }, Some(s)) = (&node, season) {
                                crate::ui::detail::open_rk_on_season(*sid, rk, s);
                            }
                            enter_node(&node, &mut route);
                            // AFTER the entry: the guard inside it asks what is currently loaded,
                            // and the push is what makes this page the one a later BACK leaves.
                            trail.push(node);
                        }
                        Nav::Back { .. } => {
                            // The pop, at the floor — with the teardown already spent above, in the
                            // same order the old instant arm ran them. `unwrap_or(Node::Home)` is
                            // the anti-strand floor: it cannot fire (the trail is rooted at Home and
                            // only Home/Library are ever terminal), but if it ever did, BACK must
                            // still go SOMEWHERE.
                            let under = trail.back().unwrap_or(Node::Home);
                            enter_node(&under, &mut route);
                        }
                    }
                }
            }
            fd[3] = if framedrop_on { SDL_GetPerformanceCounter() } else { fd[0] }; // navcommit

            if matches!(route, Route::Login) {
                crate::ui::login::update(dt);
            } else if matches!(route, Route::Onboard) {
                crate::ui::onboard::update(dt);
            } else if matches!(route, Route::Profiles) {
                crate::ui::profiles::update(dt);
                if pick_user.is_some()
                    && crate::auth::phase() == crate::auth::Phase::Profiles
                    && !crate::auth::users().is_empty()
                {
                    let idx = pick_user.take().unwrap();
                    log(&format!("pickuser: auto-selecting roster index {idx}"));
                    // through the screen's own select, so a protected tile opens the PIN pad
                    // (headless pad capture) exactly like OK on the remote
                    crate::ui::profiles::pick(idx);
                }
            // **`page_of`, not the bare route, for every screen below.** A popover still DRAWS the
            // page it was opened over, but whether that page also UPDATES is the explicit
            // `host_page_updates` policy above.  ItemMenu keeps its anchored host live; Account
            // freezes its host so invisible hero/shelf work cannot steal frames from the menu.
            // Asking `page_of` here keeps the host identity in one place while the lifecycle policy
            // remains separately testable instead of being inferred from route shape.
            } else if host_page_updates(
                route,
                crate::ui::settings::is_open() || crate::ui::consent::freezes_host(),
            ) && matches!(page_of(route), Route::Home)
            {
                if hero_osc && now.wrapping_sub(hero_osc_last) > 700 {
                    hero_osc_last = now;
                    crate::ui::home::dev_flip_hero();
                }
                if home_fold_osc && now.wrapping_sub(home_fold_osc_last) > 700 {
                    home_fold_osc_last = now;
                    if home_fold_down {
                        set_snap(1.0);
                        set_fr(0);
                    } else {
                        set_snap(0.0);
                        crate::ui::home::set_hero_focus(0);
                    }
                    home_fold_down = !home_fold_down;
                }
                // dev: sweep the grid focus top↔bottom to reproduce the vertical-scroll judder headlessly
                if home_osc && now.wrapping_sub(home_osc_last) > 350 {
                    home_osc_last = now;
                    let sym = if (now / 3000) % 2 == 0 {
                        SDLK_DOWN
                    } else {
                        SDLK_UP
                    };
                    crate::ui::home::home_move_focus(sym as c_uint);
                }
                // only when home is actually drawn — stepping its 16×24 cell springs during
                // Player/Detail frames was pure waste on the A53 (the ui::press dip/commit is driven
                // route-agnostically right after `dt` above)
                let (_, moving) = crate::ui::idle::scoped_motion(|| {
                    crate::ui::home::home_update(dt);
                });
                underlay_moving |= moving;
            } else if host_page_updates(
                route,
                crate::ui::settings::is_open() || crate::ui::consent::freezes_host(),
            ) && matches!(page_of(route), Route::Library)
            {
                // dev: libosc sweeps the browse-grid focus down↔up (the library_scroll FPS scene).
                // Only while the PAGE holds focus, for `detail_osc`'s reason: the context-menu
                // popover is modal, and sweeping focus under it walks the anchor out from under it.
                if lib_osc
                    && matches!(route, Route::Library)
                    && now.wrapping_sub(lib_osc_last) > 350
                {
                    lib_osc_last = now;
                    // …but NOT on homeosc's 3s reversal: this document opens with the library chip
                    // and one rung per shelf, so at 350ms a 12-shelf library reverses before it
                    // ever reaches the grid — and the seam between the last shelf and the poster
                    // wall is the thing this scene exists to sweep. `osc_step` reverses at the
                    // document's own ends instead (`ui::library::osc_step`).
                    crate::ui::library::osc_step();
                }
                // dev: libswitch cycles EVERY switch (tabs, sort menu, unwatched, filter) on a
                // timer so the re-query + popover paths are FPS-gated too
                if lib_switch
                    && matches!(route, Route::Library)
                    && now.wrapping_sub(lib_switch_last) > 1400
                {
                    lib_switch_last = now;
                    crate::ui::library::switch_step(lib_switch_step);
                    lib_switch_step = lib_switch_step.wrapping_add(1);
                }
                // scoped like Home's above, because this page can be the one UNDER the account
                // popover now and its glass backdrop is refreshed off the underlay's motion
                let (_, moving) = crate::ui::idle::scoped_motion(|| {
                    crate::ui::library::update(dt);
                });
                underlay_moving |= moving;
            }
            if host_page_updates(
                route,
                crate::ui::settings::is_open() || crate::ui::consent::freezes_host(),
            ) && matches!(page_of(route), Route::Search)
            {
                // dev: searchosc sweeps the result shelves' focus down↔up (the fps:search-type
                // scene). Same 350ms step / 3s reversal as homeosc and libosc, so the three read
                // the same in a log and one settle predicate covers all of them. Frozen under the
                // context menu, for `detail_osc`'s reason.
                if search_osc
                    && matches!(route, Route::Search)
                    && now.wrapping_sub(search_osc_last) > 350
                {
                    search_osc_last = now;
                    let sym = if (now / 3000) % 2 == 0 {
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
                        dt,
                    );
                    crate::ui::search::update(dt);
                });
                underlay_moving |= moving;
            }
            // (The television's keyboard used to be dismissed HERE, by an `else` that called
            // `textinput::stop()` on every frame of every other route — because `search::leave`
            // was reached by no route off the screen. It is `forward_leave`'s job now: Search is
            // not a trail page, so every way off it carries its teardown to the fade floor, which
            // is where the panel is meant to come down and is also the half the poll never did —
            // it cleared `textinput`'s own flag and left `search::EDITING` set.)
            if account_osc && matches!(route, Route::Account { .. }) {
                // `wake`, not `invalidate`: this buys the continuous present the scene grades
                // without claiming the PAGE changed — an unscoped per-frame invalidate here read
                // as page damage and re-rendered the frozen host under the menu on every frame
                // (26 fps against the 50 floor, 2026-09-04), grading the oscillator, not the app.
                crate::ui::idle::wake();
                if now.wrapping_sub(account_osc_last) > 520 {
                    account_osc_last = now;
                    let sym = if account_osc_down { SDLK_DOWN } else { SDLK_UP };
                    account_osc_down = !account_osc_down;
                    crate::ui::account_menu::move_focus(sym as c_int);
                }
            }
            if modal_osc && settings_tried && now.wrapping_sub(modal_osc_last) > 1500 {
                modal_osc_last = now;
                if crate::ui::settings::is_open() {
                    // `on_back`, not `close`: the interactive exit runs the dismiss FADE, and the
                    // fade is the half of the ramp this scene exists to grade.
                    let _ = crate::ui::settings::on_back();
                } else {
                    crate::ui::settings::open();
                }
            }
            if legal_doc && !legal_doc_tried && crate::ui::legal::is_open() {
                legal_doc_tried = true;
                let _ = crate::ui::legal::on_ok();
            }
            if alert_boot && !alert_tried && crate::ui::consent::is_open() {
                alert_tried = true;
                crate::ui::consent::dev_open_delete_alert();
            }
            if settings_osc && crate::ui::settings::is_open() {
                // This is deliberately continuous. Row springs naturally settle between D-pad
                // steps, so measuring only their duty cycle would grade timing policy rather than
                // the GPU cost of the Settings composition the user asked to hold at 50 fps.
                // `wake` rather than `invalidate`, for `account_osc`'s reason above.
                crate::ui::idle::wake();
                // Keep presenting continuously, but move focus at a human D-pad cadence. At 120ms
                // the target alternated before TableView's pill spring could reach either row: ink
                // changed immediately while the white plate hovered at their midpoint, a test-only
                // picture that looked like broken production focus.
                if now.wrapping_sub(settings_osc_last) > 520 {
                    settings_osc_last = now;
                    let delta = if settings_osc_down { 1 } else { -1 };
                    settings_osc_down = !settings_osc_down;
                    if matches!(route, Route::Onboard) && crate::ui::onboard::settings_mode() {
                        let sym = if delta > 0 { SDLK_DOWN } else { SDLK_UP };
                        crate::ui::onboard::key(sym, 0);
                    } else if crate::ui::legal::is_open() {
                        crate::ui::legal::on_updown(delta);
                    } else if crate::ui::consent::is_open() {
                        crate::ui::consent::on_updown(delta);
                    } else {
                        crate::ui::settings::on_updown(delta);
                    }
                }
            }
            if consent_osc && crate::ui::consent::is_open() && !crate::ui::settings::is_open() {
                crate::ui::idle::invalidate();
                if now.wrapping_sub(consent_osc_last) > 520 {
                    consent_osc_last = now;
                    let delta = if consent_osc_down { 1 } else { -1 };
                    consent_osc_down = !consent_osc_down;
                    crate::ui::consent::on_updown(delta);
                }
            }
            if onboard_osc && matches!(route, Route::Onboard) {
                crate::ui::idle::invalidate();
                if now.wrapping_sub(onboard_osc_last) > 520 {
                    onboard_osc_last = now;
                    let sym = if onboard_osc_right {
                        SDLK_RIGHT
                    } else {
                        SDLK_LEFT
                    };
                    onboard_osc_right = !onboard_osc_right;
                    crate::ui::onboard::key(sym, 0);
                }
            }
            // Self-gated like the alert, and for the same reason: not a route, so there is no
            // route term to test it with.
            crate::ui::legal::update(dt);
            crate::ui::consent::update(dt);
            crate::ui::settings::update(dt);
            // Self-gated on `Popover::visible`, NOT on the route — the same rule the draw sites
            // below obey, and for the same reason. These two popovers are also ROUTES, so
            // dismissing one flips `route` back to its host page on the press frame while the
            // panel is still fading out over it. `update` is the only place `Popover`'s `closing`
            // flag is ever cleared, so a route term here strands a dismissed panel at full opacity
            // for the rest of the session and every panel opened afterwards stacks on top of it —
            // reported off a television on 2026-09-03. Both modules already return early unless
            // they are `visible()`, so the guard bought nothing and cost the fade.
            crate::ui::account_menu::update(dt);
            crate::ui::item_menu::update(dt);
            if matches!(page_of(route), Route::Detail) {
                // dev: plxnative-detailosc swings the scroll hero<->bottom so the FPS heartbeat samples the
                // transition (the settled ends already hold 60). Only while the PAGE holds focus: the
                // popover is modal, and sweeping focus under it would walk the anchor out from under it.
                if detail_osc && matches!(route, Route::Detail) {
                    let sym = if (now / 450) % 2 == 0 {
                        SDLK_DOWN
                    } else {
                        SDLK_UP
                    };
                    crate::ui::detail::move_focus(sym as c_int);
                }
                crate::ui::detail::update(dt);
            }
            if matches!(page_of(route), Route::Person) {
                // owns the `/library/people/{id}/media` pump — the shelves land here, and the
                // retry backoff only ticks while the page is actually up
                crate::ui::person::update(dt);
            }
            if matches!(
                route,
                Route::Player {
                    overlay: Overlay::Menu
                }
            ) {
                crate::ui::track_menu::update(dt); // pill slide + open fade
            }
            if matches!(
                route,
                Route::Player {
                    overlay: Overlay::More
                }
            ) {
                crate::ui::more_menu::update(dt);
            }
            // Re-samples on its own 2 Hz hold; a no-op when the panel is off.
            crate::ui::stats::update(now);
            // …and the lab upload's toast, which expires on a clock rather than a spring.
            crate::lab::update(now);
            if matches!(
                route,
                Route::Player {
                    overlay: Overlay::Info
                }
            ) {
                crate::ui::info_panel::update(dt);
            }
            if matches!(
                route,
                Route::Player {
                    overlay: Overlay::Chapters
                }
            ) {
                crate::ui::chapters_panel::update(dt);
            }
            // Stepped for the WHOLE player route, not per-overlay like the panels above: the
            // countdown must keep running whichever overlay state the route reports.
            // Arm the Up Next countdown the frame it takes the control row. Nothing to step: both
            // stand-ins are drawn by `draw_hud`, so they inherit the transport's visibility rather
            // than owning any motion of their own.
            if matches!(route, Route::Player { .. }) {
                crate::ui::up_next::tick(ctrl, now);
                // …and the transport discs' focus pop, for the reason its own doc gives: it must be
                // stepped once per FRAME, and `draw_hud` does not run on every frame of this route.
                crate::ui::player_hud::update(ctrl, hud.nav.focus, hud.nav.btn, dt, now);
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
                            && !matches!(route, Route::Player { .. })
                        {
                            log("pump_play: engine started off-route → restoring Route::Player");
                            // The page is being taken off screen by a LANDING, not by a navigation, so no
                            // transition runs and nothing else would spend its teardown. `forward_leave`
                            // and not `leave_of`: the page stays on the trail if it is a trail page, and
                            // this repair must not blank the detail page the player will exit back to.
                            // What it does cover is Search, where the television's keyboard would
                            // otherwise be left up over playback (`textinput`'s trap 3: once the user
                            // closes it themselves, the field can never be typed into again this session).
                            if let Some(f) = forward_leave(route) {
                                f();
                            }
                            route = Route::Player {
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
            if crate::metadata::pump_detail() {
                crate::ui::idle::invalidate(); // a detail landing rewrites the page under us
            }
            // Server-side view-state WRITES (Mark as Watched / Unwatched, Remove from Deck): send
            // the next queued one, land the last one's answer and kick the refresh it owes. Route-
            // unconditional for the same reason as the two pumps around it — the user can walk off
            // Home or off the detail page between pressing and the server answering, and the refresh
            // is owed either way. Invalidates from inside, per landing.
            crate::viewstate::pump();
            // …and the cross-source resolve it kicked off. Route-unconditional for the same reason,
            // and separate because it lands one round trip per source LATER than the page does —
            // "Also available" appears when the other servers have answered, not when the page
            // mounts. It invalidates from inside `alt_sources::install`, since a landing that grows
            // the actions row must be drawn without waiting for a keypress.
            crate::metadata::pump_alt_sources();
            fd[4] = if framedrop_on { SDL_GetPerformanceCounter() } else { fd[0] }; // tick_drain
            crate::posters::poster_pump(3); // invalidates from inside, per texture installed

            let player = matches!(route, Route::Player { .. });
            // EXPERIMENT (`/tmp/plxnative-opaque`): one `static` read and a return when the trigger
            // is absent. Route-scoped and edge-triggered — see `system.rs`.
            crate::system::opaque_route(player);
            fd[5] = if framedrop_on { SDL_GetPerformanceCounter() } else { fd[0] }; // prepare
            // `worstprep=`: the prepare phase is timed on EVERY iteration, presented or not — a
            // settled screen must never run untimed work at the loop rate (spec §8.3).
            if framedrop_on {
                let prep = perf_ms(fd[5].wrapping_sub(fd[4]));
                if prep > fd_worst_prep {
                    fd_worst_prep = prep;
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
            // The PLAYER route is deliberately excluded. `system.rs::clear_opaque_region`
            // documents the hardware video plane as *slaved* to this wayland surface, and
            // "we stop presenting while a plane is slaved to it" is a claim about this
            // compositor that reading cannot settle. Home has no video plane active, which is
            // what makes it the safe place to prove the mechanism. Playback also spends ~99% of
            // its time with the HUD auto-hidden, where the frame is already 0 draw calls.
            // `should_present` is on the LEFT so the short-circuit can never skip it: it
            // takes-and-clears the discrete flag, and on the player route (which always presents)
            // a skipped take would leave a stale flag to fire spuriously on the way back out.
            let present = crate::ui::idle::should_present(now) || player;
            // Hoisted: the frame-drop detector reads these after the gate. Seeded to the pump
            // stamp so a skipped frame reports zero draw/cap/swap rather than a stale delta.
            fd[6] = fd[5];
            fd[7] = fd[5];
            fd[8] = fd[5];
            if present {
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
                crate::ui::glassload::prepare(now);
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
                        if player {
                            crate::system::clear_opaque_region();
                            glClearColor(0.0, 0.0, 0.0, 0.0);
                            glClear(GL_COLOR_BUFFER_BIT);
                            let hud_up = hud_visible(now, hud_until(), paused(), hud.dismissed);
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
                                    route,
                                    Route::Player {
                                        overlay: Overlay::Menu
                                    }
                                );
                            crate::ui::player_hud::draw_subtitle_bitmap(subs_lift); // PGS/VobSub image subs
                            crate::ui::player_hud::draw_subtitles(subs_lift);
                            if hud_up
                                || !matches!(
                                    route,
                                    Route::Player {
                                        overlay: Overlay::None
                                    }
                                )
                            {
                                // hide the transport middle behind the Info card / Chapters strip
                                crate::ui::player_hud::draw_hud(
                                    ctrl,
                                    busy,
                                    hud.nav.focus,
                                    hud.nav.btn,
                                    hud.nav.tab,
                                    now,
                                    !matches!(
                                        route,
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
                            crate::ui::player_hud::draw_readout(busy, now);
                            // Stale content panels are gated on the SAME failure as the transport.
                            // More is the deliberate exception: the failed read-out opens that shared
                            // quality picker as its recovery path, so it must remain visible and
                            // drivable over the black failure ground.
                            let panels = !crate::ui::player_hud::transport_hidden();
                            if panels
                                && matches!(
                                    route,
                                    Route::Player {
                                        overlay: Overlay::Menu
                                    }
                                )
                            {
                                crate::ui::track_menu::draw();
                            }
                            if panels
                                && matches!(
                                    route,
                                    Route::Player {
                                        overlay: Overlay::Info
                                    }
                                )
                            {
                                crate::ui::info_panel::draw();
                            }
                            if panels
                                && matches!(
                                    route,
                                    Route::Player {
                                        overlay: Overlay::Chapters
                                    }
                                )
                            {
                                crate::ui::chapters_panel::draw();
                            }
                            if matches!(
                                route,
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
                            crate::ui::popover::host::begin_frame(underlay_moving);
                            // Resolve every glass owner BEFORE anything on this route draws — that is
                            // `Glass::prepare`'s contract, and the shared top tab track is an owner on
                            // every route that wears it.
                            if route_wears_tab_bar(route) {
                                crate::ui::widgets::tab_glass_prepare();
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
                            if matches!(route, Route::Person) {
                                crate::ui::person_bio::prepare_present(
                                    underlay_moving || crate::ui::idle::present_dirty(),
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
                            let page_route = if matches!(route, Route::Onboard)
                                && crate::ui::onboard::settings_mode()
                            {
                                std::ptr::addr_of!(SETTINGS_HOME_RETURN)
                                    .read()
                                    .unwrap_or(Route::Home)
                            } else {
                                page_of(route)
                            };
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
                                if matches!(page_route, Route::Login) {
                                    crate::ui::login::draw();
                                } else if matches!(page_route, Route::Onboard) {
                                    crate::ui::onboard::draw();
                                } else if matches!(page_route, Route::Profiles) {
                                    crate::ui::profiles::draw();
                                } else if matches!(page_route, Route::Detail) {
                                    crate::ui::detail::draw();
                                } else if matches!(page_route, Route::Person) {
                                    crate::ui::person::draw();
                                } else if matches!(page_route, Route::Library) {
                                    crate::ui::library::draw();
                                } else if matches!(page_route, Route::Search) {
                                    crate::ui::search::draw();
                                } else {
                                    crate::ui::home::home_draw();
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
                                // The rest of that class, and the ones with no opener to lift: a
                                // notice is about the APP, not about anything on the page behind it.
                                crate::ui::settings::draw_scrim();
                                crate::ui::legal::draw_scrim();
                                // And the consent question over all of them, mirroring the key ladder.
                                crate::ui::consent::draw_scrim();
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
                            if !crate::ui::settings::host_ground_ready()
                                && !crate::ui::consent::host_ground_ready()
                            {
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
                            // …and the notice over all of it, mirroring the key ladder: whichever
                            // answers BACK first must also be the one on top.
                            crate::ui::settings::draw();
                            // The Settings-hosted Home editor, drawn through its OWN push
                            // (`settings::HOME_PUSH`, split off `settings::CHILD` 2026-09-04 so
                            // Privacy/Legal opening cannot also satisfy this gate). Gated on the
                            // push's own amount (`settings::home_editor_visible`), not on
                            // `route == Route::Onboard && onboard::settings_mode()`: that
                            // flag-based gate is what used to mount the editor at full opacity on
                            // its first frame (nothing ever painted it through `RoutePush::child`)
                            // and drop it with no reverse animation at all
                            // (`perform_settings_action`'s Done/Cancel exit flips both the route
                            // and `settings_mode()` away on the SAME frame the reverse spring
                            // starts). The amount-based gate keeps drawing it, fading and sliding,
                            // for exactly as long as `settings::update`'s `HOME_PUSH` says there is
                            // still something on screen — in both directions — and is `false`
                            // throughout an ordinary first-run boot, which never touches that push.
                            if crate::ui::settings::home_editor_visible() {
                                crate::ui::onboard::draw();
                            }
                            crate::ui::legal::draw();
                            // Top of the stack, mirroring the top of the key ladder — the boot stopped
                            // for this, so nothing may be drawn over it.
                            crate::ui::consent::draw();
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
                                let fps_col = if buffer_flip_count < 30 {
                                    crate::ui::theme::DIAG_FLIP_A
                                } else {
                                    crate::ui::theme::DIAG_FLIP_B
                                };
                                crate::gfx::draw_number(
                                    fps_shown,
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
                fd[6] = if framedrop_on { SDL_GetPerformanceCounter() } else { fd[0] }; // draw
                // dev capture stream: grab this finished frame before the swap (after the last draw,
                // so the copy's pass-flush is work the swap would submit anyway). One atomic when idle.
                // Deliberately NOT on the player route (the UI plane is transparent over video, so
                // there is nothing to grab) — capture.rs's 5s keepalive resend covers the host's
                // deadness timer while playback is up.
                if !player {
                    crate::capture::tick(now);
                }
                fd[7] = if framedrop_on { SDL_GetPerformanceCounter() } else { fd[0] }; // capture
                // Before the swap, never after: the back buffer is undefined once presented.
                #[cfg(feature = "hostsim")]
                crate::shot::maybe_capture(vx, vy, vw, vh);
                SDL_GL_SwapWindow(win);
                // One increment, then nothing: re-ask EGL for the back buffer's AGE after real
                // presents have happened. The boot reading is 0 by construction. See `egl.rs`.
                crate::egl::late_probe();
                #[cfg(feature = "devtools")]
                {
                    buffer_flip_count = (buffer_flip_count + 1) % 60;
                }
                crate::ui::widgets::glass_presented();
                fd[8] = if framedrop_on { SDL_GetPerformanceCounter() } else { fd[0] }; // swap
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
                crate::ui::idle::note_present(now);
            } else {
                // The swap is this loop's ONLY blocking call — there is no SDL_Delay, nanosleep
                // or frame budget anywhere else in it. Skipping the present without sleeping here
                // would turn a 16%-of-a-core app into a 100% spinner: strictly worse than the
                // problem. One frame period, so input latency is exactly what it is today.
                SDL_Delay(crate::ui::idle::IDLE_POLL_MS);
            }
            let rn = route_word(route);
            // …and the same name as a reportable event, on CHANGE only. Per-frame would be a
            // firehose of one fact; what is worth knowing is which screens get used, which is a
            // transition count. `&'static str` from the table above, so nothing runtime-built can
            // reach the wire — see `diag::schema`.
            if rn != last_route_reported {
                last_route_reported = rn;
                crate::diag::event(crate::diag::schema::DiagEvent::RouteEntered { screen: rn });
            }
            // The lab envelope's `route` field, from the SAME name the heartbeat and the focus
            // fingerprint print — a snapshot that disagreed with the log about which screen the
            // tester was on would be worse than one that omitted the field. Compiles away in every
            // build that is not a lab build.
            crate::lab::note_route(rn);
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
            if crate::focusprobe::armed() {
                let screen = match route {
                    Route::Login => crate::focusprobe::Screen::Login,
                    Route::Profiles => crate::focusprobe::Screen::Profiles,
                    Route::Onboard => crate::focusprobe::Screen::Onboard,
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
                crate::focusprobe::sample(
                    rn,
                    screen,
                    crate::focusprobe::Hud {
                        focus: hud.nav.focus,
                        btn: hud.nav.btn,
                        tab: hud.nav.tab,
                        visible: hud_visible(last_input, hud_until(), paused(), hud.dismissed),
                    },
                    ctrl,
                );
            }
            // frame-drop detector: attribute slow frames to pump(uploads)/draw/swap(GPU). Drains the
            // per-frame upload counters every frame (so the count is per-frame, not cumulative).
            // ONE tail for every route — this used to live only on the non-player path, which left
            // /tmp/plxnative-framedrop dead during playback (the timings were collected, then a
            // `continue` threw them away).
            // `present` gates this too: a frame the idle gate skipped drew nothing, so grading it
            // would drag `worstframe` toward zero and read as a perf WIN. A skipped frame is not a
            // fast frame — it is an absent one, and `fps=` on the heartbeat is where it shows up.
            if framedrop_on && present {
                let ph = |k: usize| perf_ms(fd[k].wrapping_sub(fd[k - 1]));
                let (ingest, results, navcommit, tick_drain, prepare, draw, cap, swap) =
                    (ph(1), ph(2), ph(3), ph(4), ph(5), ph(6), ph(7), ph(8));
                let total = perf_ms(fd[8].wrapping_sub(fd[0]));
                let (up, px) = crate::posters::take_upload_stats();
                let (cards, cards_off) = crate::gfx::take_card_stats();
                if total > fd_worst {
                    fd_worst = total;
                }
                if total > framedrop_thresh {
                    // Printed in the frame ALGORITHM's order (spec §8.4), which on this loop is
                    // not the order they ran: navcommit ran before tick_drain here.
                    log(&format!(
                        "FRAMEDROP total={total:.1} ingest={ingest:.1} results={results:.1} tick_drain={tick_drain:.1} navcommit={navcommit:.1} prepare={prepare:.1} draw={draw:.1} capture={cap:.1} swap={swap:.1} up={up} px={px} cards={cards} off={cards_off} route={rn} load={} snap={:.2}",
                        crate::ui::glassload::step_index(),
                        crate::ui::home::snap_pos()
                    ));
                }
            }
            if loop_tick(&mut iters_ct, &mut loop_t, &mut loop_shown, now) {
                // once/sec render heartbeat — greppable without reading the on-screen counter.
                // The harness parses `loop=(\d+) route=(\w+)(?: overlay=(\w+))?` (tests/run.py), so
                // the player's overlay tag stays right after route= and worstframe= stays LAST.
                //
                // RENAMED 2026-08-01, and the old name was REUSED, so a log predating this reads
                // as the opposite of what it says: the field that used to be `FPS=` is now `loop=`,
                // and `fps=` now means what it always should have — frames actually presented,
                // previously `pres=`. An old `FPS=60` is a LOOP rate and says nothing about frames.
                let ov = overlay_word(route);
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
                    if let Some((prev_ns, prev_ticks)) = play_prev {
                        let wall_ms = i64::from(now.wrapping_sub(prev_ticks));
                        if wall_ms > 0 {
                            let media_ms = (pos_ns - prev_ns) / 1_000_000;
                            pos.push_str(&format!(" play={}pm", media_ms * 1000 / wall_ms));
                        }
                    }
                }
                play_prev = if playing { Some((pos_ns, now)) } else { None };
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
                    fps_shown = pres.min(i32::MAX as u32) as i32;
                }
                // dev: which LOAD-DIAL step these frames belong to, the blur refreshes
                // actually TAKEN in that second, and the cadence in force. Absent unless the
                // dial or the cadence knob is armed, and placed after `fps=` / before
                // `worstframe=` so both harness regexes are untouched. `snap=` is the one
                // thing a cadence claim cannot be trusted without: it is the rate that RAN,
                // not the rate that was requested.
                let ld = if crate::ui::glassload::armed() || glass_hz_armed {
                    format!(
                        " load={} snap={} period={}",
                        crate::ui::glassload::step_index(),
                        crate::gfx::take_blur_snapshots(),
                        crate::ui::widgets::dynamic_period()
                    )
                } else {
                    String::new()
                };
                if framedrop_on {
                    // `worstframe=` stays LAST of the graded fields (both harness regexes anchor
                    // on it); `worstprep=` follows it, ungated by present.
                    log(&format!("loop={loop_shown} route={rn}{ov}{pos}{vp} fps={pres}{ld} worstframe={fd_worst:.1}ms worstprep={fd_worst_prep:.1}ms{SIM_TAG}"));
                    fd_worst = 0.0;
                    fd_worst_prep = 0.0;
                } else {
                    log(&format!(
                        "loop={loop_shown} route={rn}{ov}{pos}{vp} fps={pres}{ld}{SIM_TAG}"
                    ));
                }
            }
        }

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
        crate::posters::posters_shutdown();
        SDL_Quit();
        0
    }
}

#[cfg(test)]
mod route_tests {
    //! The route-classification rules — pure functions of a `Route`, which is why they were lifted
    //! out of `plex_run`'s body: they decide something that has shipped wrong twice and no test
    //! could see them in there.
    //!
    //! Nothing here draws, touches a global or RUNS a teardown: `leave_of` hands back a `fn()` and
    //! these grade which answer it gives, never call it. So they are ordinary parallel tests, and
    //! what they deliberately cannot say is whether the panel actually comes down on the
    //! television — that is a device check (`tv-session`, the keyboard up over Home).
    use super::*;

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

    /// The generalisation a reviewer already caught, as an assertion. Making a forward navigation
    /// blanket-carry `leave_of(cur)` is the obvious move and it is WRONG: Detail and Person stay on
    /// the BACK trail, so `detail::close` (and its `metadata::clear`) would empty the page the user
    /// is about to press BACK to, *during its own fade-out*.
    #[test]
    fn a_forward_navigation_never_tears_down_a_page_the_trail_can_put_back() {
        for r in [Route::Home, Route::Library, Route::Detail, Route::Person] {
            assert!(
                stays_on_trail(r),
                "a page with a `Node` is a page BACK can return to"
            );
            assert!(
                forward_leave(r).is_none(),
                "going deeper must leave the page behind it standing"
            );
        }
        // …and the two that HAVE a teardown really do, so the line above is about the RULE rather
        // than about there being nothing to run either way.
        assert!(
            leave_of(Route::Detail).is_some(),
            "a BACK off a detail page still closes it"
        );
        assert!(leave_of(Route::Person).is_some());
    }

    /// Search is the other half, and the reason the rule is trail membership rather than direction:
    /// it has no `Node`, the commit frame resets the trail on arrival, so nothing is ever behind it
    /// and every way off it — three of the four are FORWARD navigations (a section pill, the Home
    /// pill, opening a result) — is leaving it for good.
    ///
    /// The regression this replaces: `leave_of`'s Search arm was consulted only by `nav_back`,
    /// which this screen never reaches, so the television's keyboard was dismissed by polling the
    /// route on every frame of the app's life instead.
    #[test]
    fn every_way_off_search_carries_its_teardown() {
        assert!(
            !stays_on_trail(Route::Search),
            "nothing stacks ON Search — its results stack on Home"
        );
        assert!(
            forward_leave(Route::Search).is_some(),
            "a pill press or an opened result takes the keyboard with it"
        );
        assert!(
            leave_of(Route::Search).is_some(),
            "…and a BACK runs `leave_of` outright"
        );
    }

    /// A popover is not a page: `page_of` resolves both of them onto the screen underneath, so a
    /// navigation out of the item menu over a detail page must behave exactly like one off that
    /// detail page — otherwise opening a card's menu would change what BACK finds behind it.
    #[test]
    fn a_popover_answers_for_the_screen_it_sits_on() {
        let menu = Route::ItemMenu {
            over: MenuHost::Detail,
        };
        assert!(stays_on_trail(menu));
        assert!(
            forward_leave(menu).is_none(),
            "the detail page under the menu stays mounted"
        );
        assert!(
            leave_of(menu).is_some(),
            "…and a BACK off it still closes that page"
        );
        assert!(
            forward_leave(Route::Account {
                over: BarHost::Home
            })
            .is_none(),
            "the Home under the profile menu stays mounted too"
        );
    }

    /// **The profile popover stands on the page it was opened from — on all three, not on Home.**
    ///
    /// The chip is a stop on every screen that wears the shared top bar, so `Route::Account` carries
    /// the page underneath exactly as `ItemMenu` does. It was a UNIT variant while Home was the only
    /// screen that could press it, and the dozen places that read it therefore said Home outright:
    /// the page drawn under the panel, the arm that keeps that page's springs stepping, and where a
    /// dismissal lands. Left that way, pressing the Library's chip would have cut to Home under the
    /// popover and then stranded the user there.
    ///
    /// Graded through [`page_of`], because that is the one answer all three of those readers take —
    /// the draw dispatch, the update arm and [`leave_of`]/[`stays_on_trail`].
    #[test]
    fn the_profile_popover_stands_on_the_page_it_was_opened_from() {
        for over in [BarHost::Home, BarHost::Library, BarHost::Search] {
            let pop = Route::Account { over };
            assert!(
                page_of(pop) == over.route(),
                "the page under the panel is the one it opened on"
            );
            assert!(
                route_wears_tab_bar(pop),
                "every host of this popover wears the bar"
            );
            // it answers for that page in both halves of the teardown rule, exactly as the item
            // menu answers for the screen its card is on
            assert_eq!(stays_on_trail(pop), stays_on_trail(over.route()));
            assert_eq!(
                forward_leave(pop).is_some(),
                forward_leave(over.route()).is_some()
            );
            // and the page it opens ON is the page it closes BACK to
            assert!(BarHost::of(over.route()) == Some(over));
        }
        // a route with no chip on it opens no popover at all — the guard in `chip_activate`
        for r in [
            Route::Detail,
            Route::Person,
            Route::Login,
            Route::Profiles,
            Route::Onboard,
            Route::Player {
                overlay: Overlay::None,
            },
            Route::ItemMenu {
                over: MenuHost::Detail,
            },
        ] {
            assert!(
                BarHost::of(r).is_none(),
                "only the three bar screens carry the profile chip"
            );
        }
    }

    /// **Every menu host answers as its own screen, on all four route questions.**
    ///
    /// The menu had two hosts and now has six, and the way that goes wrong is silent: each of
    /// these questions used to name `MenuHost::Detail` (or `Home`) in a `matches!`, so a new host
    /// simply fell into the default arm — Search's popover would have drawn HOME behind it, and
    /// the Library's would have lost its tab bar mid-hold. Each one is `page_of` now, and this is
    /// what says so for every host at once rather than for the one a reviewer thought of.
    ///
    /// [`MenuHost::Related`] is the case that shows why the list is worth keeping exhaustive: it is
    /// the SECOND host whose page is the detail page, so it is the first one for which "which
    /// screen is underneath" and "which host is this" stopped being the same question.
    #[test]
    fn every_menu_host_answers_exactly_as_the_screen_underneath_it() {
        for host in [
            MenuHost::Home,
            MenuHost::Detail,
            MenuHost::Related,
            MenuHost::Library,
            MenuHost::Search,
            MenuHost::Person,
        ] {
            let page = host.route();
            let menu = Route::ItemMenu { over: host };
            assert!(
                !matches!(page, Route::ItemMenu { .. }),
                "a host is a live PAGE, never another popover"
            );
            // `==`, not `assert_eq!`: `Route` has no `Debug` (it is a run-loop vocabulary, not a
            // logged value — the heartbeat's `route=` word is built by its own `match`)
            assert!(
                page_of(menu) == page,
                "the popover draws and updates the screen it sits on"
            );
            // the chrome question: a menu over the Library wears the tab bar because the Library
            // does, and one over the detail page does not because that page does not
            assert_eq!(
                route_wears_tab_bar(menu),
                route_wears_tab_bar(page),
                "the popover must wear exactly the chrome of the page under it"
            );
            // …and the trail questions, which decide whether a navigation OUT of the menu empties
            // the page a BACK is about to return to
            assert_eq!(stays_on_trail(menu), stays_on_trail(page));
            assert_eq!(forward_leave(menu).is_some(), forward_leave(page).is_some());
            assert_eq!(leave_of(menu).is_some(), leave_of(page).is_some());
        }
    }

    /// The coupling that keeps [`stays_on_trail`] honest. It claims to be exactly the set of pages
    /// a `Node` names, and `node_route` is where that set is written down — so a new `Node` whose
    /// route answered `false` here would tear its page down on the way deeper, which is the first
    /// test's bug arriving through the other door. The two lists are exhaustive `match`es that the
    /// compiler cannot relate; this is what relates them.
    #[test]
    fn every_trail_node_names_a_page_that_stays_on_the_trail() {
        let sid = crate::plex::ServerId::UNSET;
        let nodes = [
            Node::Home,
            Node::Library,
            Node::Person {
                sid,
                key: String::new(),
                guid: String::new(),
                name: String::new(),
                thumb: String::new(),
            },
            Node::Detail {
                sid,
                rk: String::new(),
                spot: Spot::default(),
            },
        ];
        for n in &nodes {
            assert!(
                stays_on_trail(node_route(n)),
                "a page the trail holds must survive a forward navigation off it"
            );
        }
    }
}

/// Is the next step of a dev script (`plxnative-autoseek`, `plxnative-qualityswitch`) due?
///
/// `at` is the origin the gap is measured from. Both scripts fire their first step by backing the
/// origin off by one gap; the seek script additionally pushes it out by `delay=<ms>`.
///
/// **The difference is read as SIGNED, and that is the whole function.** `at` is deliberately set
/// into the FUTURE when a delay is armed (`now - gap + delay`), so `now - at` is negative until
/// the delay elapses. Compared as `u32` that negative value wraps to about 4.29 billion, clears
/// any gap, and fires the step immediately — the exact inverse of what `delay=` asks for. Casting
/// the difference to `i32` reads it as the small negative number it is. Safe for any tick spacing
/// under ~24 days, and it keeps working across the 2^32 ms tick wrap because the SUBTRACTION still
/// wraps; only its interpretation changes.
fn script_step_due(now: u32, at: u32, gap_ms: u32) -> bool {
    (now.wrapping_sub(at) as i32) >= gap_ms as i32
}

#[cfg(test)]
mod script_schedule_tests {
    use super::script_step_due;

    /// **`delay=<ms>` must actually delay.** `plxnative-autoseek` arms the first step with
    /// `at = now - gap + delay`, so at the moment of arming `now - at` is `gap - delay`. That is
    /// NEGATIVE whenever the delay exceeds the gap, and in `u32` it wraps to about 4.29 billion —
    /// which clears any gap, so the step fires AT ONCE instead of after the delay.
    ///
    /// It shipped that way, and it silently invalidated every case built on it: a manifest seek
    /// declaring `delay_ms: 95000` (gap 300) ran before the quality switch it was written to
    /// follow, while `op_seek_transcode` still passed because a quality switch emits the same
    /// `reload_transcode: fresh Load at offset` line a seek does. `auto_seek_after_switch` has
    /// carried it since it was written and never showed it, because that case skips without a
    /// conditioned link.
    #[test]
    fn a_delay_longer_than_the_gap_does_not_fire_at_once() {
        let (now, gap, delay) = (1_000_000u32, 300u32, 95_000u32);
        let at = now.wrapping_sub(gap).wrapping_add(delay);
        assert!(
            !script_step_due(now, at, gap),
            "the delayed step fired immediately"
        );
        assert!(
            !script_step_due(now.wrapping_add(delay - 1), at, gap),
            "fired one ms early"
        );
        assert!(
            script_step_due(now.wrapping_add(delay), at, gap),
            "never fired at the delay"
        );
    }

    /// The ordinary arming — no delay — still fires the first step immediately, and the next one
    /// exactly one gap later. This is what every existing script depends on.
    #[test]
    fn an_undelayed_script_still_fires_at_once_then_one_gap_apart() {
        let (now, gap) = (1_000_000u32, 300u32);
        let at = now.wrapping_sub(gap);
        assert!(
            script_step_due(now, at, gap),
            "the first step must fire on arming"
        );
        assert!(
            !script_step_due(now.wrapping_add(gap - 1), now, gap),
            "second step fired early"
        );
        assert!(
            script_step_due(now.wrapping_add(gap), now, gap),
            "second step never fired"
        );
    }

    /// SDL ticks wrap at 2^32 ms (~49 days). The predicate must survive the origin sitting just
    /// below the wrap and `now` just above it.
    #[test]
    fn the_predicate_survives_the_tick_wrap() {
        let (gap, at) = (300u32, u32::MAX - 100);
        assert!(!script_step_due(at.wrapping_add(299), at, gap));
        assert!(script_step_due(at.wrapping_add(300), at, gap));
    }
}

#[cfg(test)]
mod key_layout_tests {
    use super::{decode_key, encode_key, encode_key_repeat};
    use crate::ui::consts::{SDLK_DOWN, SDLK_RETURN, WCODE_BACK, WCODE_PAUSE};

    /// `encode_key` and `decode_key` must agree, in whichever layout this build compiled.
    ///
    /// This is the regression test for a bug that shipped: the two ends disagreed about
    /// `SDL_KeyboardEvent`'s field offsets, so every remote-FIFO token was accepted, decoded into
    /// nonsense, and silently dropped — no error on either side. Nothing in the compiler couples a
    /// reader and a writer of raw byte offsets, so this does.
    ///
    /// `make check` builds the television layout, so that is the one graded by default; a
    /// `--features hostsim` test run grades the stock-SDL2 one. Both arms are compiled either way
    /// (they are `cfg!`, not `#[cfg]`), so neither can rot.
    #[test]
    fn key_bytes_round_trip() {
        // The wcode-only case is the one that breaks a sym-derived mapping, and the one a naive
        // host layout loses: `pause` carries no sym at all.
        //
        // **`(8, 42)` is the case that MATTERS and it was missing.** It is the `backspace` token,
        // and 8 is one of the four syms `host_wcode` maps a desktop key onto — to `WCODE_BACK`.
        // The only sym-plus-wcode case here used to be `(8, WCODE_BACK)`, the single pair where
        // the stand-in and the carrier agree, so a decode that consulted the stand-in FIRST passed
        // this test while turning the panel's delete key into a navigation. Every one of those
        // four syms belongs here for the same reason.
        for (sym, wcode) in [
            (SDLK_DOWN, 0),
            (SDLK_RETURN, 0),
            (0, WCODE_PAUSE),
            (8, WCODE_BACK),
            (8, 42),   // backspace: sym 8, SDL_SCANCODE_BACKSPACE — NOT BACK
            (32, 44),  // space, 'p', 's': the other three syms the stand-in claims, each
            (112, 19), // beside its own real scancode, which must survive unchanged
            (115, 22),
        ] {
            for down in [true, false] {
                let ev = encode_key(sym, wcode, down);
                let (state, got_wcode, got_sym) = decode_key(&ev);
                assert_eq!(got_sym, sym, "sym lost (wcode={wcode}, down={down})");
                assert_eq!(got_wcode, wcode, "wcode lost (sym={sym}, down={down})");
                assert_eq!(
                    state & 0xff,
                    u32::from(down),
                    "press/release lost — the low byte is what every handler tests (sym={sym})"
                );
                assert_eq!(
                    state & 0x100,
                    0,
                    "a synthetic edge must never look like auto-repeat"
                );
            }
        }
    }

    /// `encode_key_repeat`'s twin of the round trip above: a `holdrep:<name>` token must decode as
    /// a genuine hardware auto-repeat (`state & 0x100 != 0`), the exact shape `on_auto_repeat`'s
    /// caller gates on (`state & 0x100 != 0 && sym == held_key.down_sym`) — the one case
    /// `key_bytes_round_trip` just pinned an ordinary edge must NEVER produce.
    #[test]
    fn encode_key_repeat_round_trips_as_a_hardware_repeat() {
        for (sym, wcode) in [(SDLK_DOWN, 0), (0, WCODE_PAUSE), (8, 42)] {
            let ev = encode_key_repeat(sym, wcode);
            let (state, got_wcode, got_sym) = decode_key(&ev);
            assert_eq!(got_sym, sym, "sym lost (wcode={wcode})");
            assert_eq!(got_wcode, wcode, "wcode lost (sym={sym})");
            assert_eq!(state & 0xff, 1, "a repeat is a DOWN edge, not a release");
            assert_eq!(
                state & 0x100,
                0x100,
                "must decode as auto-repeat, or `on_auto_repeat` never sees it (sym={sym})"
            );
        }
    }

    /// **`k:<sym>,<wcode>` — the only token that can press a key the map does NOT name**, which is
    /// what LG checklist item 40 needs: a named-token map can by construction never send an
    /// unsupported key. Both fields are required and decimal; a half-parsed pair must be REFUSED
    /// rather than silently become a press of something else, because the drain's `else` logs an
    /// unknown token and a wrong pair would log nothing at all.
    #[test]
    fn the_raw_key_token_carries_both_fields_or_none() {
        use super::remote_token_key;
        assert_eq!(
            remote_token_key("k:0,269"),
            Some((0, 269)),
            "HOME, which nothing else can send"
        );
        assert_eq!(
            remote_token_key("k:53,34"),
            Some((53, 34)),
            "the digit 5 as the TV spells it"
        );
        for bad in [
            "k:", "k:1", "k:1,", "k:,1", "k:a,1", "k:1,b", "k:1,2,3", "k:-1,2", "k: 1,2",
        ] {
            assert_eq!(
                remote_token_key(bad),
                None,
                "{bad:?} must not become a keypress"
            );
        }
        // …and it must not shadow the named tokens or the other prefixed ones.
        assert!(
            remote_token_key("ck:10,20").is_none(),
            "a click token is not a key token"
        );
        assert_eq!(
            remote_token_key("chup"),
            Some((0, crate::ui::consts::WCODE_CH_UP_KEY))
        );
        assert_eq!(
            remote_token_key("pageup"),
            Some((crate::ui::consts::SDLK_PAGEUP, 0))
        );
    }
}

/// The heartbeat's `route=` WORD for a route — the string `tests/run.py` selects samples by
/// (`LOOP_RE`/`FPS_RE`) against `manifest.json`'s `route` field, and the one the focus
/// fingerprint, the diag `RouteEntered` event and the lab envelope all print. ONE function so the
/// four cannot disagree, and so `heartbeat_word_tests` can grade the table against the manifest:
/// a route renamed here without its scenes following would otherwise fail on the device as
/// "never entered this screen", which reads exactly like a total regression.
fn route_word(route: Route) -> &'static str {
    match route {
        Route::Login => "login",
        Route::Profiles => "profiles",
        Route::Onboard => "onboard",
        Route::Account { .. } => "account",
        Route::ItemMenu { .. } => "itemmenu",
        Route::Library => "library",
        Route::Detail => "detail",
        Route::Person => "person",
        Route::Search => "search",
        Route::Player { .. } => "player",
        _ => "home",
    }
}

/// The heartbeat's ` overlay=<word>` suffix (leading space included, empty when there is none),
/// from the SAME open-state reads the key ladder uses. The Settings family outranks the player's
/// overlays because it can only be open over a bar page, where no player overlay exists.
fn overlay_word(route: Route) -> &'static str {
    if crate::ui::settings::is_open() {
        if crate::ui::legal::is_open() {
            " overlay=legal"
        } else if crate::ui::consent::is_open() {
            " overlay=privacy"
        } else {
            " overlay=settings"
        }
    } else if crate::ui::consent::is_open() {
        " overlay=consent"
    } else {
        match route {
            Route::Player {
                overlay: Overlay::Info,
            } => " overlay=info",
            Route::Player {
                overlay: Overlay::Chapters,
            } => " overlay=chapters",
            Route::Player {
                overlay: Overlay::Menu,
            } => " overlay=menu",
            Route::Player {
                overlay: Overlay::More,
            } => " overlay=more",
            Route::Player {
                overlay: Overlay::None,
            } => " overlay=none",
            _ => "",
        }
    }
}

/// Every `route=` word [`route_word`] can print, and every ` overlay=` word [`overlay_word`] can —
/// the heartbeat WORD TABLE. Read by `heartbeat_word_tests` only; the functions above are the
/// source, and the arrays exist so the manifest can be graded against a finite alphabet.
#[cfg(test)]
const ROUTE_WORDS: [&str; 11] = [
    "login", "profiles", "onboard", "account", "itemmenu", "library", "detail", "person",
    "search", "player", "home",
];
#[cfg(test)]
const OVERLAY_WORDS: [&str; 9] = [
    "legal", "privacy", "settings", "consent", "info", "chapters", "menu", "more", "none",
];

/// The heartbeat word table versus `tests/manifest.json`. Every fps scene selects its samples by
/// a `route` word and an optional `overlay` word; a word the app cannot print makes that scene
/// fail on the television as "only 0 post-warmup samples — scene never entered this screen",
/// which is indistinguishable from a real regression. This is the host-side half of that gate,
/// and it is what lets the route-name source move (from this `match` to `Screen::name` later)
/// without the fps tier silently disarming.
#[cfg(test)]
mod heartbeat_word_tests {
    use super::{overlay_word, route_word, Overlay, Route, OVERLAY_WORDS, ROUTE_WORDS};

    const MANIFEST: &str = include_str!("../../../tests/manifest.json");

    fn scenes() -> Vec<serde_json::Value> {
        let v: serde_json::Value = serde_json::from_str(MANIFEST).expect("manifest.json parses");
        v["fps_scenes"]
            .as_array()
            .expect("fps_scenes is an array")
            .clone()
    }

    #[test]
    fn every_manifest_route_word_is_one_the_heartbeat_prints() {
        for s in scenes() {
            let name = s["name"].as_str().unwrap_or("?");
            let route = s["route"].as_str().expect("scene has a route word");
            assert!(
                ROUTE_WORDS.contains(&route),
                "scene {name}: route word {route:?} is not in the heartbeat table {ROUTE_WORDS:?}"
            );
            if let Some(ov) = s.get("overlay").and_then(|o| o.as_str()) {
                assert!(
                    OVERLAY_WORDS.contains(&ov),
                    "scene {name}: overlay word {ov:?} is not in the table {OVERLAY_WORDS:?}"
                );
            }
        }
    }

    #[test]
    fn the_table_is_exactly_what_the_functions_print() {
        // Every arm of `route_word` lands in ROUTE_WORDS, and every word in ROUTE_WORDS has an
        // arm — a word added to one side only is what this catches.
        let routes = [
            Route::Login,
            Route::Profiles,
            Route::Onboard,
            Route::Library,
            Route::Detail,
            Route::Person,
            Route::Search,
            Route::Home,
            Route::Player {
                overlay: Overlay::None,
            },
        ];
        let mut seen: Vec<&str> = routes.iter().map(|r| route_word(*r)).collect();
        seen.push("account");
        seen.push("itemmenu");
        for w in ROUTE_WORDS {
            assert!(seen.contains(&w), "ROUTE_WORDS has {w:?} but no route prints it");
        }
        for w in &seen {
            assert!(ROUTE_WORDS.contains(w), "route_word prints {w:?}, missing from ROUTE_WORDS");
        }
        for (ov, word) in [
            (Overlay::Info, " overlay=info"),
            (Overlay::Chapters, " overlay=chapters"),
            (Overlay::Menu, " overlay=menu"),
            (Overlay::More, " overlay=more"),
            (Overlay::None, " overlay=none"),
        ] {
            let got = overlay_word(Route::Player { overlay: ov });
            assert_eq!(got, word);
            let bare = word.trim_start_matches(" overlay=");
            assert!(OVERLAY_WORDS.contains(&bare));
        }
        assert_eq!(overlay_word(Route::Home), "");
    }
}

#[cfg(test)]
mod root_back_tests {
    //! **BACK at a ROOT hands the screen back to the television, and the app keeps running.**
    //!
    //! Two halves, and they are graded differently on purpose. [`back_at_root`] is driven for real
    //! — it is the app's whole answer to "there is nowhere further back to go", and the regression
    //! to catch is a future edit putting `running = false`, or a modal question, back where the
    //! platform call now goes. [`onboarding_back`] is pure, because its caller (`key_onboarding`)
    //! is `unsafe`, arms tvOS presses and reaches `auth`, none of which a host test wants to drive.
    //!
    //! What NO host test can say is that the television actually shows its launcher and that the
    //! process survives it. That is `webos::go_home`'s device half — `gohome: SAM accepted`, a
    //! capture of the launcher (on webOS 4 a RIBBON over the still-running app, so no lifecycle
    //! event at all) and `fuser` reporting one pid throughout — and it is why this file's
    //! `home_requests` counter grades the DECISION and never the outcome.
    use super::*;

    /// **The one that matters (issue #16).** The root press asks the platform for its Home screen.
    ///
    /// Observed RED against the shipped `back_at_root`, which raised the "Exit PlxNative?" alert
    /// and asked webOS for nothing: `left: 0, right: 1`.
    #[test]
    fn back_at_home_root_shows_the_platform_home() {
        let _g = crate::testlock::serial();
        crate::webos::release_root_press();
        let before = crate::webos::home_requests();
        back_at_root();
        assert_eq!(
            crate::webos::home_requests(),
            before + 1,
            "BACK at Home's root must ask webOS for its Home screen"
        );
        crate::webos::release_root_press();
    }

    /// **A refused root BACK leaves the sign-in it refused to leave RUNNING, and asks for the
    /// television's Home.** This branch used to restart the flow first (`RestartAndHome`), because
    /// `auth::cancel` invalidated the worker before it decided; that ordering is gone (issue #30,
    /// `auth::a_refused_back_leaves_the_live_pin_poll_running`), and a restart on top of a live
    /// poll would mint a fresh code over one the user's phone may already have answered. Observed
    /// RED against the shipped `after_cancel`, which answered `RestartAndHome` for `Waiting`.
    #[test]
    fn a_root_back_out_of_a_running_sign_in_leaves_it_running() {
        assert_eq!(
            after_cancel(false),
            AfterCancel::Home,
            "nothing was disturbed, so there is nothing to restart — go to the television's Home"
        );
    }

    /// A cancel that SUCCEEDED went somewhere inside the app: nothing to ask the platform for, and
    /// the claim goes back so the real root BACK a moment later is not swallowed.
    #[test]
    fn a_cancel_that_backed_out_asks_the_platform_for_nothing() {
        assert_eq!(after_cancel(true), AfterCancel::BackedOut);
    }

    /// **Issue #18.** The QR sign-in is the first screen of a first-ever launch and has nothing
    /// behind it, so every BACK there is the root press — there is no panel on that screen for one
    /// to mean anything else.
    #[test]
    fn back_on_the_qr_sign_in_is_always_the_root_press() {
        assert_eq!(
            onboarding_back(Route::Login, false),
            OnboardBack::Root
        );
        assert_eq!(
            onboarding_back(Route::Login, true),
            OnboardBack::Root,
            "the picker's keypad is not this screen's, so it cannot claim this press"
        );
    }

    /// **Issue #17.** BACK on the who's-watching picker is the root press — *unless* its own PIN
    /// keypad is up, which is the one thing on that screen a BACK can close.
    #[test]
    fn back_on_the_picker_is_the_root_press_unless_the_pin_pad_is_up() {
        assert_eq!(
            onboarding_back(Route::Profiles, false),
            OnboardBack::Root
        );
        assert_eq!(
            onboarding_back(Route::Profiles, true),
            OnboardBack::Screen,
            "an open PIN keypad takes the press — closing it is not leaving the app"
        );
    }

    /// **A profile switch in flight is a root press like any other** — since `auth::cancel` stopped
    /// invalidating on refusal there is no worker for the press to strand: either `cancel` backs out
    /// (retiring the switch through the epoch, the picker's own BACK as it always was) or it refuses
    /// and the switch runs on behind the television's Home. The keypad still comes first. Observed
    /// RED against the shipped rule, which answered `Ignore` for both routes.
    #[test]
    fn back_during_a_profile_switch_is_a_root_press() {
        assert_eq!(onboarding_back(Route::Profiles, false), OnboardBack::Root);
        assert_eq!(
            onboarding_back(Route::Login, false),
            OnboardBack::Root,
            "the follower has one frame in which the route can still say Login"
        );
        assert_eq!(
            onboarding_back(Route::Profiles, true),
            OnboardBack::Screen,
            "a protected profile submits its PIN while switching — that BACK closes the pad, which \
             never reaches auth at all"
        );
    }

    /// The first-run sources question is NOT a root: the picker is behind it and `Action::Back`
    /// returns there. Pinned because it is the one onboarding route where "nothing is behind this
    /// screen" is false, and a rule that swept it in would strand the user outside the app halfway
    /// through setting it up.
    #[test]
    fn the_first_run_sources_question_still_steps_back_into_the_picker() {
        assert_eq!(
            onboarding_back(Route::Onboard, false),
            OnboardBack::Screen
        );
    }

    /// Every route that is not one of the three is `Screen`, which is the conservative answer: the
    /// press behaves as it did before this rule existed rather than leaving the app from a page
    /// that has a history behind it.
    #[test]
    fn a_route_this_rule_does_not_own_never_leaves_the_app() {
        for (route, what) in [
            (Route::Home, "Home"),
            (Route::Detail, "Detail"),
            (Route::Library, "Library"),
            (Route::Search, "Search"),
            (Route::Person, "Person"),
        ] {
            assert_eq!(
                onboarding_back(route, false),
                OnboardBack::Screen,
                "{what}"
            );
        }
    }
}

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

#[cfg(test)]
mod player_return_tests {
    //! **Where playback returns to.** Pure, parallel, and touching no global: every launch site and
    //! the exit ritual itself live inside the SDL event loop where no host test can reach them, so
    //! the decision they share is lifted into [`return_page`] / [`set_origin`] and graded here.
    //!
    //! What these deliberately cannot say is whether the page LOOKS restored — that is
    //! `detail.rs`'s `Spot` tests plus a device capture — nor whether each call site passes the
    //! right `Origin`, which is a reading of `app.rs` and a press on a television.
    use super::*;
    use crate::plex::ServerId;

    const A: ServerId = ServerId::from_raw(0);
    const B: ServerId = ServerId::from_raw(1);

    fn det(sid: ServerId, rk: &str) -> Node {
        Node::Detail {
            sid,
            rk: rk.to_string(),
            spot: Spot::default(),
        }
    }
    fn person(key: &str) -> Node {
        Node::Person {
            sid: A,
            key: key.into(),
            guid: String::new(),
            name: String::new(),
            thumb: String::new(),
        }
    }
    /// The live stores as a test sees them: a detail page on `A` showing item 7, and a person page.
    fn page(r: Route) -> Node {
        return_page(r, Some(det(A, "7")), Some(person("9")))
    }

    /// The rule, over every screen playback can be started from: **you come back to the page you
    /// were standing on.** Home is the one that was already right; the other four were all landing
    /// on Home, because the origin was a `from_detail: bool` and everything that was not the detail
    /// page fell into its `else`.
    #[test]
    fn a_session_returns_to_the_screen_it_was_launched_from() {
        assert_eq!(page(Route::Home), Node::Home);
        assert_eq!(
            page(Route::Library),
            Node::Library,
            "a Library-grid card menu's Play"
        );
        assert_eq!(
            page(Route::Search),
            Node::Search,
            "a Search result shelf's Play"
        );
        assert_eq!(
            page(Route::Person),
            person("9"),
            "a person page's filmography"
        );
        assert_eq!(
            page(Route::Detail),
            det(A, "7"),
            "the detail page's Play/Resume and filmstrip"
        );
        // The three boot gates and the player itself are unreachable as launch origins; they must
        // still name a page, and Home is the one that is always there.
        for r in [
            Route::Login,
            Route::Profiles,
            Route::Onboard,
            Route::Player {
                overlay: Overlay::None,
            },
        ] {
            assert_eq!(page(r), Node::Home);
        }
    }

    /// **The reported bug.** A long press on a RELATED tile opens the card menu over the detail
    /// page, and its *Play from Start* is dispatched with the route already flipped back to the
    /// host — so both detail-page hosts must answer with that page, and not with Home.
    ///
    /// Both, because the two menus stand on ONE page: the filmstrip's Play returned to it and the
    /// Related shelf's did not, which is a single detail page with two Play rows that go to
    /// different screens.
    #[test]
    fn a_card_menu_returns_to_the_page_it_was_opened_over() {
        for over in [MenuHost::Detail, MenuHost::Related] {
            assert_eq!(
                page(Route::ItemMenu { over }),
                det(A, "7"),
                "both hosts stand on the page"
            );
        }
        assert_eq!(
            page(Route::ItemMenu {
                over: MenuHost::Home
            }),
            Node::Home
        );
        assert_eq!(
            page(Route::ItemMenu {
                over: MenuHost::Library
            }),
            Node::Library
        );
        assert_eq!(
            page(Route::ItemMenu {
                over: MenuHost::Search
            }),
            Node::Search
        );
        assert_eq!(
            page(Route::ItemMenu {
                over: MenuHost::Person
            }),
            person("9")
        );
        // …and the account popover resolves the same way, through `page_of`.
        assert_eq!(
            page(Route::Account {
                over: BarHost::Library
            }),
            Node::Library
        );
        assert_eq!(
            page(Route::Account {
                over: BarHost::Search
            }),
            Node::Search
        );
        assert_eq!(
            page(Route::Account {
                over: BarHost::Home
            }),
            Node::Home
        );
    }

    /// A detail return names the SAME ITEM that was mounted — the whole reason the origin is a
    /// `Node` and not a `Route`. `Route::Detail` cannot say which page, and by the time BACK is
    /// pressed the PLAYED leaf's own detail is what is loaded, so re-deriving the target at the
    /// exit reads the wrong item by construction.
    ///
    /// The server is part of that identity for `Node`'s own reason: with a share registered, item 7
    /// exists on both machines and is two different films.
    #[test]
    fn a_detail_return_names_the_item_that_was_mounted() {
        assert_eq!(
            return_page(Route::Detail, Some(det(A, "7")), None),
            det(A, "7")
        );
        assert_ne!(
            return_page(Route::Detail, Some(det(B, "7")), None),
            det(A, "7")
        );
        assert!(
            !det(A, "7").same_page(&det(A, "8")),
            "a different item is a different page"
        );
        assert!(
            !det(A, "7").same_page(&det(B, "7")),
            "…and so is the share's copy of 7"
        );
    }

    /// A page that never mounted is not a page anyone can be returned to. `origin_here` passes
    /// `None` for an empty mounted rk (and for a person page with nothing loaded), and the fallback
    /// is Home rather than a `Node::Detail` with an empty key — which would put a blank page on the
    /// trail and re-fetch nothing on the way back.
    #[test]
    fn a_screen_with_nothing_mounted_falls_back_to_home() {
        assert_eq!(return_page(Route::Detail, None, None), Node::Home);
        assert_eq!(return_page(Route::Person, None, None), Node::Home);
        assert_eq!(
            return_page(
                Route::ItemMenu {
                    over: MenuHost::Related
                },
                None,
                None
            ),
            Node::Home
        );
    }

    /// **Up Next must not rewrite the return route.** An auto-advance starts a NEW item while the
    /// player is already up: the user chose nothing, and the page on screen is the player itself.
    /// `Origin::Unchanged` is what keeps the chain pointing at the page they actually came from,
    /// however many episodes it runs for.
    #[test]
    fn auto_advance_keeps_the_page_the_user_came_from() {
        let mut from = det(A, "7");
        for _ in 0..4 {
            set_origin(&mut from, Origin::Unchanged); // episode → episode → episode → …
        }
        assert_eq!(
            from,
            det(A, "7"),
            "four auto-advances later, still the show page"
        );
        // …and a fresh launch DOES take the page it was launched from.
        set_origin(&mut from, Origin::From(Node::Library));
        assert_eq!(from, Node::Library);
    }

    /// The route each return lands on, through the ONE `Node`→`Route` mapping the trail already
    /// owns. `exit_player` re-enters via `enter_node`, so this is what the heartbeat reports after
    /// a BACK — and the Home row is the no-op the fix is careful to keep.
    #[test]
    fn the_route_after_back_is_the_page_the_node_names() {
        assert!(matches!(node_route(&Node::Home), Route::Home));
        assert!(matches!(node_route(&Node::Library), Route::Library));
        assert!(matches!(node_route(&Node::Search), Route::Search));
        assert!(
            matches!(node_route(&det(A, "7")), Route::Detail),
            "the reported bug, as a route"
        );
        assert!(matches!(node_route(&person("9")), Route::Person));
    }
}
