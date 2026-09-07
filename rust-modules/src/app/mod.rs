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
pub(crate) mod adapters;
mod boot;
pub(super) mod clock;
mod recorder;
mod events;
mod lifecycle;
mod playback;
mod nav;
mod input;
mod legacy;
mod run;
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
    fn SDL_Delay(ms: u32);
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


/// The app core's state, gathered from `plex_run`'s loop-locals (UI restructure spec v4 §13,
/// phase 1b-i: FIELDS ONLY — `plex_run` keeps its shape and reads `app.<field>` where it read a
/// local). Phase 1b-ii extracts the coordinator's functions over `&mut App`; the machines of §2.2
/// replace these fields one phase at a time. Every field was a `let mut` before the `while
/// running` loop; the immutable boot-time values (dev flags, closures, the window) stay locals.
struct App {
    pick_user: Option<usize>,
    home_osc_last: u32,
    hero_osc_last: u32,
    home_fold_osc_last: u32,
    home_fold_down: bool,
    lib_osc_last: u32,
    lib_switch_last: u32,
    lib_switch_step: u32,
    search_osc_last: u32,
    settings_osc_last: u32,
    settings_osc_down: bool,
    modal_osc_last: u32,
    legal_doc_tried: bool,
    alert_tried: bool,
    account_osc_last: u32,
    account_osc_down: bool,
    consent_osc_last: u32,
    consent_osc_down: bool,
    onboard_osc_last: u32,
    onboard_osc_right: bool,
    nav_osc_last: u32,
    last_input: u32,
    loop_t: u32,
    iters_ct: i32,
    loop_shown: i32,
    #[cfg(feature = "devtools")]
    fps_shown: i32,
    play_prev: Option<(i64, u32)>,
    running: bool,
    #[cfg(feature = "devtools")]
    buffer_flip_count: u8,
    held_key: HeldKey,
    scrubber: Scrub,
    modal_repeat: RepeatGate,
    hud: HudState,
    marker_tried: bool,
    foreground: ForegroundLifecycle,
    repause_at: i64,
    ok_armed: bool,
    last_route_reported: &'static str,
    press_tried: bool,
    press_release_at: u32,
    itemmenu_tried: bool,
    ptr: Pointer,
    route: Route,
    play_from: Node,
    trail: crate::ui::trail::Trail,
    nav_pending: Option<NavReq>,
    auto_tried: bool,
    replay_left: u32,
    grid_tried: bool,
    settings_tried: bool,
    seek_tried: bool,
    seek_script: Vec<String>,
    seek_script_at: u32,
    seek_gap_ms: u32,
    seek_script_last: i64,
    quality_script: Vec<crate::plex::session::PlaybackQuality>,
    quality_script_at: u32,
    quality_gap_ms: u32,
    quality_tried: bool,
    quality_playing_since: Option<u32>,
    detail_tried: bool,
    play_tried: bool,
    menu_tried: bool,
    menupick_tried: bool,
    pause_tried: bool,
    pause_script: Option<(u32, Option<u32>)>,
    pause_resume_at: Option<u32>,
    prev: u32,
    refresh_hubs_at: u32,
    ev: [u8; 128],
    remote: Option<crate::remote::Remote>,
    /// The SDL window (`SDL_CreateWindow`), for the swap.
    win: *mut c_void,
    /// Boot time (`SDL_GetTicks` at the end of boot): the origin of every dev-script delay.
    t0: u32,
    /// The frame's instruments: the eight phase stamps, FRAMEDROP, the per-second peaks
    /// (`diag::heartbeat`), armed by `plxnative-framedrop`.
    instr: crate::diag::heartbeat::Instruments,
    /// The boot-time dev trigger flags the loop consults.
    dev: DevFlags,
    /// The Input machine: owner of the press (restructure spec §2.2); the ladders borrow it.
    input: crate::ui::input::Input,
    /// `text::take_measure_fault` has been reported once (the report is once per process).
    measure_fault_logged: bool,
    /// The recorder / replay driver (`plxnative-rec` / `plxnative-recplay`, spec §5.3/§5.5).
    rec: recorder::Recplay,
    /// The present gate as a machine (spec §4.4). `ui::idle` is still the product's verdict on
    /// this loop; this one receives the render cache's notes and is what `dispatch` takes over.
    present: crate::ui::present::Present,
    /// The frame budget (spec §8.1): the poster upload quota, spent by the render cache.
    budget: crate::ui::frame::Budget,
    /// The SHADOW container tree (phase 3b(c), `app/legacy.rs`): a dispatcher over
    /// `LegacyPage(Route)`, mirrored from the committed route after every NAV COMMIT.
    pages: crate::ui::dispatch::Dispatcher<legacy::AppHost>,
    /// What that dispatcher borrows: a mounter that only knows `LegacyPage`, no-op hooks.
    shadow: legacy::ShadowRig,
}

/// The dev triggers read ONCE at boot and consulted by the loop (each is documented where it
/// is read in `boot`). Phase 2 turns them into recorded `Sys` results (spec §5.3).
struct DevFlags {
    detail_osc: bool,
    home_osc: bool,
    hero_osc: bool,
    home_fold_osc: bool,
    lib_osc: bool,
    lib_switch: bool,
    search_osc: bool,
    settings_boot: Option<String>,
    settings_osc: bool,
    modal_osc: bool,
    legal_doc: bool,
    alert_boot: bool,
    account_osc: bool,
    consent_osc: bool,
    onboard_osc: bool,
    nav_osc: bool,
    nav_osc_rk: String,
    glass_hz_armed: bool,
}


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
    crate::dev::softfloat_probe();
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
    let mut app = match unsafe { boot(pms_host, pms_port, mt) } {
        Ok(app) => app,
        Err(code) => return code,
    };
    unsafe {
        run::run(&mut app, mt);
        std::mem::replace(&mut app.rec, recorder::Recplay::Off).finish();
        run::shutdown(mt);
    }
    0
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
