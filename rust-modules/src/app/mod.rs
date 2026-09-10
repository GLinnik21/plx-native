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
pub(crate) const COLS: c_int = 10;
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
// submodules below as a PURE move (`pub(crate)` widening only), glob-imported here so the
// loop body reads exactly as before. `plex_run` itself is phase 1b.
pub(crate) mod adapters;
pub(crate) mod boot;
/// **"Stats for nerds"** — the diagnostics read-out (phase 10, was `ui/stats.rs`). Here rather
/// than in `ui/` because it is written from `player::Diag`, `route`, `plex::identity`, `webos`
/// and `devcaps` — application facts — and because its state is now an `App` field.
pub(crate) mod diagnostics;
pub(crate) mod clock;
mod recorder;
pub(crate) mod events;
pub(crate) mod lifecycle;
pub(crate) mod playback;
pub(crate) mod nav;
pub(crate) mod input;
pub(crate) mod bridge;
mod chrome;
pub(crate) mod content;
pub(crate) mod run;
use self::boot::*;
use self::events::*;
use self::lifecycle::*;
use self::playback::*;
use self::nav::*;
use self::input::*;
use self::content::*;

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
use crate::metadata::Spot;
// **The screen ARGUMENT is `screens::registry`'s** since restructure phase 10 (§2.1): the
// registry owns the concrete `ScreenArg` and the one `mount` match, so `app/` reads it here
// rather than declaring it. Imported at the tree's root because every module under `app/`
// that requests a navigation names it.
use crate::screens::registry::AppArg;
use crate::ui::trail::{Node, Trail};
/// The shared top strip's vocabulary: what a pill INDEX means. Every site that turns a pill into a
/// destination `match`es on this, so a pill the app has not been taught about is a compile error
/// rather than a silent library open — see `widgets::Pill`.
use crate::ui::widgets::Pill;


/// The adapter tree (spec §2.2). An adapter owns OS/FFI resources and holds no logical state; the
/// decisions live in the machines beside it.
pub(crate) struct Adapters {
    /// The Starfish/ACB session slot and the `MainThread` token — see [`crate::player::adapter`].
    pub(crate) player: crate::player::adapter::PlayerAdapter,
}

/// The app core's state, gathered from `plex_run`'s loop-locals (UI restructure spec v4 §13,
/// phase 1b-i: FIELDS ONLY — `plex_run` keeps its shape and reads `app.<field>` where it read a
/// local). Phase 1b-ii extracts the coordinator's functions over `&mut App`; the machines of §2.2
/// replace these fields one phase at a time. Every field was a `let mut` before the `while
/// running` loop; the immutable boot-time values (dev flags, closures, the window) stay locals.
///
/// **`pub(crate)`, and every field with it (phase 10, `dev/scenarios.rs`'s prologue):** the dev-
/// trigger arms that used to live inline in `boot`/`run`/`content`/`mod` now live in
/// `crate::dev::scenarios`, a sibling of `app` rather than a descendant of it, so the fields and
/// helpers they still reach through `&mut App` need to be visible outside this module's subtree.
/// Nothing here becomes `pub` — only as wide as the crate.
pub(crate) struct App {
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
    /// The sym we believe is PHYSICALLY DOWN right now — set by a fresh key-down, cleared by its
    /// key-up. It tells a real hardware auto-repeat from a PHANTOM one, which this television emits
    /// routinely: over the system keyboard the panel does not deliver a key-up for the press that
    /// raised it, so LG's key driver still believes OK is held and stamps the NEXT press with
    /// `state & 0x100`. Without this the repeat guard reads that as a repeat and drops it, so the
    /// first OK after every keyboard session does nothing — reported as "I have to click the search
    /// field twice for the keyboard to appear" (device-measured 2026-08-15). A repeat for a key we
    /// never saw pressed is not a repeat.
    ///
    /// **It is all that is left of `App::held_key`** (phase 10). The other five fields of `HeldKey`
    /// were the CLIENT-SIDE hold-to-move timer for every discrete focus list; the last list it
    /// still drove was the item context menu, and that is a `ModalStack` surface now which paces
    /// its own `Edge::Repeat` (`screens::registry::PANEL_REPEAT_MS`). This one is a fact about
    /// the physical key rather than about that timer, so it outlives it — as a bare `u32`, since a
    /// struct with one field is a name for a name. (`HeldKey` itself is gone with the timer:
    /// `PlayerScreen::held` had had no producer since phase 9 and wrote two constant zeros into
    /// the canonical state, which is exactly why it was safe to delete.)
    ///
    /// `scrubber: Scrub` and `hud: HudState` stood beside it through phase 8 and are gone: both are
    /// `PlayerScreen`'s own fields now (§9), so the loop borrows them out of the mounted instance
    /// and there is no second copy to keep in step.
    down_sym: u32,
    modal_repeat: RepeatGate,
    /// **The diagnostics read-out's own state** (phase 10, spec §0 done-criterion 1): what was
    /// nine `static mut`s in `ui/stats.rs`. Not a `ModalStack` surface, and `diagnostics.rs`'s
    /// `Diagnostics` doc says why — the panel takes no keys and draws at two different z-positions.
    pub(crate) diagnostics: diagnostics::Diagnostics,
    /// **The Player machine** (restructure spec §2.2, phase 9): the playback session that was
    /// `route::decision::SESSION`, the app-switch lifecycle that was `App.foreground`, and this
    /// frame's tick. Reached as a parameter from here down — `crate::player::machine`'s doc says
    /// why the pipeline's own handles are a separate field.
    pub(crate) player: crate::player::machine::Player,
    /// **The ADAPTERS** (restructure spec §2.2, phase 9) — the OS/FFI resources the machines act
    /// through. One so far: the Player's, which holds the native session that was
    /// `player::engine::ENGINE` together with the main-thread token that confines it.
    pub(crate) adapters: Adapters,
    repause_at: i64,
    pub(crate) ok_armed: bool,
    last_route_reported: &'static str,
    pub(crate) ptr: Pointer,
    pub(crate) route: Route,
    pub(crate) play_from: Node,
    pub(crate) trail: crate::ui::trail::Trail,
    pub(crate) nav_pending: Option<NavReq>,
    pub(crate) prev: u32,
    pub(crate) refresh_hubs_at: u32,
    ev: [u8; 128],
    remote: Option<crate::remote::Remote>,
    /// The SDL window (`SDL_CreateWindow`), for the swap.
    win: *mut c_void,
    /// Boot time (`SDL_GetTicks` at the end of boot): the origin of every dev-script delay AND
    /// the clock a replay re-seats (`recorder::Recplay::arm`'s `clock_start`) — a field BOTH sides
    /// write, so it stays here rather than on `dev::scenarios::Scenarios`.
    pub(crate) t0: u32,
    /// The frame's instruments: the eight phase stamps, FRAMEDROP, the per-second peaks
    /// (`diag::heartbeat`), armed by `plxnative-framedrop`.
    instr: crate::diag::heartbeat::Instruments,
    /// Every dev-trigger arm's own state — oscillator phases, retry latches, boot-time flags
    /// (formerly `DevFlags`) — gathered on ONE struct (spec: `dev/scenarios.rs`'s module doc).
    pub(crate) scenarios: crate::dev::scenarios::Scenarios,
    /// The Input machine: owner of the press (restructure spec §2.2); the ladders borrow it.
    pub(crate) input: crate::ui::input::Input,
    /// `text::take_measure_fault` has been reported once (the report is once per process).
    measure_fault_logged: bool,
    /// The recorder / replay driver (`plxnative-rec` / `plxnative-recplay`, spec §5.3/§5.5).
    pub(crate) rec: recorder::Recplay,
    /// The present gate as a machine (spec §4.4). `ui::idle` is still the product's verdict on
    /// this loop; this one receives the render cache's notes and is what `dispatch` takes over.
    present: crate::ui::present::Present,
    /// The frame budget (spec §8.1): the poster upload quota, spent by the render cache.
    budget: crate::ui::frame::Budget,
    /// **The container tree, and since phase 5b it is no longer only a shadow.** It still
    /// mirrors the committed route after every NAV COMMIT — an owned screen for each route the
    /// ladders still own — but the Settings family is REAL on it: the surfaces and the first-run
    /// Favourites page are owned screens the loop hands its input to and asks to draw
    /// (`app/bridge.rs`'s coexistence contract).
    pub(crate) pages: crate::ui::dispatch::Dispatcher<bridge::AppHost>,
    /// Inputs collected for the dispatcher this iteration (`bridge` module doc).
    pub(crate) inputs: Vec<crate::ui::machine::InputEvent<u32>>,
    /// What that dispatcher borrows: the mounter, the real `TtfMeasure`, the store deliveries,
    /// the consent machine and the queue of requests an owned screen makes of this loop.
    pub(crate) bridge: bridge::Bridge,
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
    crate::dev::scenarios::pre_boot();
    // THE main-thread token, minted once — this function IS the SDL main thread. `boot` MOVES it
    // into `App.adapters.player`, and from there a `&mut PlayerAdapter` is the proof: the ACB /
    // Starfish seam still takes `&MainThread` (which is !Send, so `task::spawn` rejects any
    // closure that captured one), and the native session slot takes the adapter itself. See
    // `task::MainThread` and `player::adapter`.
    let main_thread = unsafe { crate::task::MainThread::assume() };
    let mut app = match unsafe { boot(pms_host, pms_port, main_thread) } {
        Ok(app) => app,
        Err(code) => return code,
    };
    unsafe {
        run::run(&mut app);
        std::mem::replace(&mut app.rec, recorder::Recplay::Off).finish();
        run::shutdown(&mut app.player.session, &mut app.adapters.player);
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
        // Owned pages receive WillLeave/Unmount through Navigation on a pop; the legacy
        // callback must not clear their shared store a second time before that lifecycle.
        assert!(
            leave_of(Route::Detail).is_none(),
            "Detail teardown belongs to its navigation entry"
        );
        assert!(leave_of(Route::Person).is_none());
    }

    /// Search JOINED the rule above in phase 7 (the Search cutover) rather than staying its own
    /// exception, though it does not join the LOOP: it still answers `stays_on_trail == false` —
    /// unlike the four above, nothing STACKS on Search, and `Node::Search` (the commit frame does
    /// push one) is not a page BACK can put back the way a Detail/Person stack is.
    ///
    /// What changed is `leave_of`. It used to carry a bespoke teardown (the retired legacy
    /// screen's own `leave()` function, dismissing the television's keyboard) that had to ride
    /// EVERY way off the screen, forward or back, because the legacy screen had no `Unmount` lifecycle
    /// of its own to run it from. The owned `SearchScreen` does: `ScreenEvent::Unmount` already
    /// drops its own keyboard, delivered by the same generic tree-retirement path Detail/Person's
    /// teardown moved onto above — proven with a REAL route change through `bridge::frame`, not a
    /// hand-fired `ScreenEvent`
    /// (`app/search_owned_tests.rs::leaving_owned_search_through_a_real_route_change_releases_its_keyboard`).
    /// So `leave_of(Route::Search)` is `None` too, and there is no bespoke Search teardown left
    /// for either direction to carry.
    #[test]
    fn search_has_no_node_the_trail_can_put_back_and_no_bespoke_teardown_either() {
        assert!(
            !stays_on_trail(Route::Search),
            "nothing stacks ON Search — its results stack on Home"
        );
        assert!(
            forward_leave(Route::Search).is_none(),
            "an owned screen's Unmount lifecycle needs no help from a forward navigation"
        );
        assert!(
            leave_of(Route::Search).is_none(),
            "…and neither does a BACK: `leave_of` has nothing bespoke left to run"
        );
    }

    // (`a_popover_answers_for_the_screen_it_sits_on` stood here. It graded `page_of` over
    // `Route::ItemMenu { over: MenuHost::Detail }` — a navigation out of the card menu on a detail
    // page had to behave exactly like one off that detail page, or opening a card's menu would
    // change what BACK found behind it. Neither menu is a route since phase 10, so `page_of` is
    // deleted and there is no second route left for the trail questions to resolve: the answer is
    // the page's own, by construction. `app::bridge`'s
    // `a_compact_surface_over_a_live_page_leaves_its_host_the_top_page` is what says so on a real
    // tree.)

    /// **The profile chip is offered on exactly the pages that wear the shared top bar.**
    ///
    /// This test used to be `the_profile_popover_stands_on_the_page_it_was_opened_from`, and it
    /// graded `Route::Account { over: BarHost }` through `page_of`: the chip is a stop on all
    /// three bar screens, so the route had to CARRY the page underneath or a press on the
    /// Library's chip would cut to Home under the panel and strand the user there on dismissal.
    ///
    /// The menu is a `ModalStack` surface since phase 10, so that whole class of bug is gone by
    /// construction — a surface is presented OVER the top page and never replaces it, which is
    /// why `Route::Account` and `BarHost` are both deleted. What survives of the old rule is the
    /// half `BarHost::of` answered: WHICH pages have a chip to press at all. It is derived from
    /// `route_wears_tab_bar` now (the chip is a control on that bar), so the two cannot drift —
    /// which is exactly what `BarHost::of`'s own hand-written three-route list could do.
    /// `app::bridge`'s `the_profile_menu_is_a_surface_over_the_page_whose_chip_was_pressed`
    /// grades the other half, on a real tree.
    #[test]
    fn the_profile_chip_is_offered_on_exactly_the_bar_wearing_pages() {
        for r in [Route::Home, Route::Library, Route::Search] {
            assert!(input::wears_the_chip(r), "a bar-wearing page carries the chip");
        }
        for r in [
            Route::Detail,
            Route::Person,
            Route::Login,
            Route::Profiles,
            Route::Onboard,
            Route::Player,
        ] {
            assert!(
                !input::wears_the_chip(r),
                "only the bar-wearing screens carry the profile chip"
            );
        }
    }

    // (`every_menu_host_answers_exactly_as_the_screen_underneath_it` stood here — six `MenuHost`
    // variants x four route questions (which page draws, which chrome it wears, and the two trail
    // questions), each answered through `page_of` so a new host could not silently fall into a
    // default arm and draw HOME behind a Search popover. The enum, the route that carried it and
    // `page_of` are all deleted in phase 10: a surface is presented OVER the top page, so every
    // one of those four questions is the page's own answer and there is no second route to relate
    // it to. What the enum still decided by then — whether the item is a leaf of the loaded season,
    // and whether the hold happened on Home's root — is two bools on `ItemMenuArg`.)

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
        Route::Library => "library",
        Route::Detail => "detail",
        Route::Person => "person",
        Route::Search => "search",
        Route::Player => "player",
        Route::Home => "home",
    }
}

/// **Every `Route`, exactly once** — the domain [`route_word`] is applied over to DERIVE the
/// heartbeat's `route=` alphabet, and the list `app::bridge`'s own argument tests walk.
///
/// The compiler is what keeps it complete: the array's length is written out, so an added variant
/// with no entry here fails the exhaustiveness `match` in `heartbeat_word_tests` rather than
/// quietly missing from the table the fps tier selects on. That is the failure this exists for —
/// a word the app cannot print makes a scene fail on the television as "only 0 post-warmup
/// samples", which is indistinguishable from a real regression.
///
/// `#[cfg(test)]`, because nothing in the SHIPPED app ever wants every route at once: the loop
/// holds one and asks about it. Its two readers are this module's word derivation and
/// `app::bridge`'s argument tests, which used to keep a second copy of the same list.
#[cfg(test)]
pub(crate) const EVERY_ROUTE: [Route; 9] = [
    Route::Login,
    Route::Profiles,
    Route::Onboard,
    Route::Home,
    Route::Library,
    Route::Detail,
    Route::Person,
    Route::Search,
    Route::Player,
];

/// The heartbeat's ` overlay=` WORD, or `None` when nothing is over the page.
///
/// **It is the topmost surface's own `Screen::name`, asked FIRST**, and since phase 10's item 4
/// that is the whole of it — `bridge::overlay_word` is a one-line read of the container with no
/// mapping table under it, so the alphabet the fps tier selects on IS the set of names the mounted
/// screens answer. See that function for the two failures the eleven-arm `match` it replaced had
/// already produced.
///
/// [`NO_OVERLAY`] is the one word here that no screen owns, and the reason this function exists at
/// all beside the bridge's: the player with nothing over it prints ` overlay=none`, which is a
/// statement about the ROUTE that the container cannot make. (Five arms stood here, one per
/// `Route::Player { overlay }` value, until phase 9 made the panels surfaces.)
fn overlay_word(pages: &crate::ui::dispatch::Dispatcher<bridge::AppHost>, route: Route) -> Option<&'static str> {
    bridge::overlay_word(pages).or(matches!(route, Route::Player).then_some(NO_OVERLAY))
}

/// The bare player, whose ` overlay=` word belongs to no screen — see [`overlay_word`].
pub(crate) const NO_OVERLAY: &str = "none";

/// The heartbeat's ` overlay=<word>` suffix, prefix and all, empty when there is none. The prefix
/// is built HERE rather than baked into every word because the words are the SCREENS' own and a
/// screen has no business knowing what the heartbeat's grammar looks like.
fn overlay_suffix(pages: &crate::ui::dispatch::Dispatcher<bridge::AppHost>, route: Route) -> String {
    overlay_word(pages, route).map_or(String::new(), |w| format!(" overlay={w}"))
}

/// **Every `route=` word [`route_word`] can print, and every ` overlay=` word [`overlay_word`]
/// can — DERIVED, not transcribed** (restructure phase 10, item 4).
///
/// Both were hand-written arrays, and both had already rotted in the one direction nothing fails
/// on: a word in the table that no function prints keeps a manifest scene looking armed while it
/// selects on a string the app never emits, which on the television reads as "only 0 post-warmup
/// samples — scene never entered this screen", i.e. exactly like a total regression. Phase 10
/// moved TWO words between the tables (`account`, `itemmenu`), which is the transition where a
/// transcription is least likely to survive.
///
/// So the routes come from [`route_word`] applied over [`EVERY_ROUTE`], and the overlays from the
/// MOUNTER: `bridge::every_surface_word()` mounts one instance of every surface `AppArg` variant
/// through the real `Mounter` and reads its `Screen::name`, so the alphabet is what the screens
/// say it is. Neither list can carry a word its source does not produce, and neither can miss one
/// — the exhaustiveness of `EVERY_ROUTE` and of the mounter's own `match` is the compiler's.
#[cfg(test)]
fn route_words() -> Vec<&'static str> {
    EVERY_ROUTE.iter().copied().map(route_word).collect()
}

#[cfg(test)]
fn overlay_words() -> Vec<&'static str> {
    let mut words = bridge::every_surface_word();
    words.push(NO_OVERLAY);
    words
}

/// The heartbeat word table versus `tests/manifest.json`. Every fps scene selects its samples by
/// a `route` word and an optional `overlay` word; a word the app cannot print makes that scene
/// fail on the television as "only 0 post-warmup samples — scene never entered this screen",
/// which is indistinguishable from a real regression. This is the host-side half of that gate,
/// and it is what lets the route-name source move (from this `match` to `Screen::name` later)
/// without the fps tier silently disarming.
#[cfg(test)]
mod heartbeat_word_tests {
    use super::{overlay_word, overlay_words, route_words, Route, EVERY_ROUTE, NO_OVERLAY};

    const MANIFEST: &str = include_str!("../../../tests/manifest.json");

    fn scenes() -> Vec<serde_json::Value> {
        let v: serde_json::Value = serde_json::from_str(MANIFEST).expect("manifest.json parses");
        v["fps_scenes"]
            .as_array()
            .expect("fps_scenes is an array")
            .clone()
    }

    /// The Settings family's INNER pages, which are not `AppArg` variants and so cannot come off
    /// the mounter: `RouteSurface::top_word` answers with whichever page of the family's own stack
    /// is on top, and the family is presented rooted at `Root` (or, for the dev boot targets, at
    /// one of these). They are the registry's own constants rather than string literals, so a
    /// screen renamed there renames the alphabet entry with it.
    ///
    /// **`ONBOARD` is one screen wearing two hats** (§6.2 "Onboard ×2"): the SAME `Screen` impl is
    /// mounted once as a page of the app's outer stack (first run, so `route=onboard`) and once as
    /// a page of the family's INNER stack (`SettingsPage::Favourites`, so ` overlay=onboard`).
    /// Before it had its own word, the settings-mounted instance answered `word::SETTINGS` and the
    /// `fps:settings-home` scene printed a heartbeat BYTE-IDENTICAL to `settings-root` — the
    /// harness could not tell "opened the Home-sources editor" from "opened Settings and did
    /// nothing", so a trigger that silently failed to reach Favourites still produced a scene that
    /// passed, measuring the wrong screen.
    const FAMILY_INNER: [&str; 3] = [
        crate::screens::registry::word::PRIVACY,
        crate::screens::registry::word::LEGAL,
        crate::screens::registry::word::ONBOARD,
    ];

    /// **Every caller holds `crate::testlock::serial()` for its whole body**, because deriving
    /// this alphabet is not a read: [`overlay_words`] goes through
    /// `bridge::every_surface_word`, which mounts each surface by running real
    /// `bridge::frame`s — and a frame pumps every store. `browse`'s pump ends in `sync_roster`,
    /// which calls `browse::reset()` whenever the section table holds a source the live registry
    /// does not; every `browse` fixture in the suite (`seed_two_source_table_for_test` and its
    /// kin) seeds exactly such a table, with `ServerId::UNSET` sources. Unguarded, these three
    /// tests therefore EMPTIED another module's seeded table from a second thread, and the
    /// failure surfaced over there — `app::chrome`'s
    /// `four_libraries_on_two_servers_publish_two_type_destinations` losing both library
    /// destinations, `app::bridge`'s shelf-hold case seeing zero shelves — at a rate low enough
    /// to read as flakiness. The frame trunk (`bridge::frame_with_results`) asserts the lock now
    /// rather than merely documenting it.
    fn overlay_alphabet() -> Vec<&'static str> {
        let mut words = overlay_words();
        words.extend(FAMILY_INNER);
        words
    }

    #[test]
    fn every_manifest_route_word_is_one_the_heartbeat_prints() {
        let _guard = crate::testlock::serial();
        let routes = route_words();
        let overlays = overlay_alphabet();
        for s in scenes() {
            let name = s["name"].as_str().unwrap_or("?");
            let route = s["route"].as_str().expect("scene has a route word");
            assert!(
                routes.contains(&route),
                "scene {name}: route word {route:?} is not in the heartbeat table {routes:?}"
            );
            if let Some(ov) = s.get("overlay").and_then(|o| o.as_str()) {
                assert!(
                    overlays.contains(&ov),
                    "scene {name}: overlay word {ov:?} is not in the table {overlays:?}"
                );
            }
        }
    }

    /// **The two tables are DERIVED from the two sources, and this is what says the derivation is
    /// the whole of them** (restructure phase 10 item 4).
    ///
    /// It used to compare two hand-written arrays against two hand-written lists of routes and
    /// screen kinds — four transcriptions of two alphabets, each able to rot in the direction
    /// nothing fails on. What is left to assert is what a derivation cannot state about itself:
    /// that the two alphabets are DISJOINT bar the one word that is deliberately in both, that
    /// every word is a plausible heartbeat token, and that `overlay_word`'s one non-screen answer
    /// is the player's.
    #[test]
    fn the_tables_are_derived_and_the_two_alphabets_stay_apart() {
        let _guard = crate::testlock::serial();
        let routes = route_words();
        let overlays = overlay_alphabet();
        assert_eq!(
            routes.len(),
            EVERY_ROUTE.len(),
            "one word per route, derived: {routes:?}"
        );
        // Nothing empty, nothing with a space in it: every one of these is a `\w+` token the
        // harness's `LOOP_RE`/`FPS_RE` capture groups have to match, and a word carrying the
        // heartbeat's own ` overlay=` prefix (which is how the mapping table this replaced spelled
        // them) would match nothing at all.
        for w in routes.iter().chain(overlays.iter()) {
            assert!(
                !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "{w:?} is not a heartbeat token"
            );
        }
        // ONE word is in both alphabets, and it names one screen mounted on two stacks. Any other
        // overlap is a route and a surface that would be indistinguishable in a `route=` field.
        let both: Vec<&str> = overlays.iter().copied().filter(|w| routes.contains(w)).collect();
        assert_eq!(both, [crate::screens::registry::word::ONBOARD]);
        // …and the two menus that became surfaces in phase 10 are on the overlay side ONLY. Both
        // MOVED between the tables in the commits that deleted their routes, and each moved with
        // its `manifest.json` scene's re-key (`home-acct-glass`, `item-menu`); a word left behind
        // in the route alphabet would have let a scene keep selecting on a `route=` the app can no
        // longer print, which fails on the television as "never entered this screen".
        for w in [
            crate::screens::registry::word::ACCOUNT,
            crate::screens::registry::word::ITEM_MENU,
        ] {
            assert!(overlays.contains(&w), "{w:?} is a surface's own name");
            assert!(!routes.contains(&w), "{w:?} has no `route_word` arm any more");
        }

        // An EMPTY tree, so `overlay_word`'s second half is what answers: with no surface up the
        // container says nothing and the ROUTE decides, which is exactly the state every BARE
        // playback frame is in. It is the one word no screen owns, which is why it is added by
        // `overlay_words` rather than derived.
        let empty = crate::ui::dispatch::Dispatcher::<super::bridge::AppHost>::new();
        assert_eq!(overlay_word(&empty, Route::Player), Some(NO_OVERLAY));
        assert_eq!(overlay_word(&empty, Route::Home), None);
        assert_eq!(super::overlay_suffix(&empty, Route::Player), " overlay=none");
        assert_eq!(super::overlay_suffix(&empty, Route::Home), "");
        assert!(overlays.contains(&NO_OVERLAY));
        assert!(!routes.contains(&NO_OVERLAY));

        // The player's four panels are `OverlayKind::word` through `Screen::name`, so they arrive
        // in the derived alphabet with everything else — asserted here because it is the one place
        // a reader can see that the panel words and the family words come from ONE source now.
        use crate::screens::player::overlay::OverlayKind;
        for kind in [
            OverlayKind::Tracks { tab: 0 },
            OverlayKind::Info,
            OverlayKind::Chapters,
            OverlayKind::More { quality: false },
        ] {
            assert!(
                overlays.contains(&kind.word()),
                "{kind:?} prints {:?}, which the mounter's own alphabet must carry",
                kind.word()
            );
        }
    }

    /// **A word the app can print but no scene uses is fine; the reverse is not** — and the
    /// derivation is what makes the reverse impossible to write by accident.
    ///
    /// The one thing a derived table cannot catch on its own is a manifest scene keyed on a word
    /// that IS in the alphabet but names a surface the scene's triggers never open. That is a
    /// device question, not a host one. What this pins instead is the direction a host CAN see:
    /// every scene naming an overlay names one the mounter produces, and every scene's route is a
    /// route that exists.
    #[test]
    fn the_manifest_uses_a_subset_of_the_derived_alphabets() {
        let _guard = crate::testlock::serial();
        let routes = route_words();
        let overlays = overlay_alphabet();
        let mut used_overlays = 0;
        for s in scenes() {
            assert!(routes.contains(&s["route"].as_str().expect("a route word")));
            if let Some(ov) = s.get("overlay").and_then(|o| o.as_str()) {
                assert!(overlays.contains(&ov));
                used_overlays += 1;
            }
        }
        assert!(
            used_overlays >= 2,
            "the two menus' scenes select by `overlay=` since phase 10 — if this reaches zero, \
             the re-keys were reverted and every surface scene is measuring its host page"
        );
    }

    /// **The pollution these three tests used to cause, stated as a fact instead of a comment.**
    ///
    /// Deriving the alphabet is a DESTRUCTIVE operation on `browse`: `every_surface_word` mounts
    /// each surface by running real `bridge::frame`s, a frame pumps every store, and `browse`'s
    /// pump reaches `sync_roster`, which treats a source the live registry does not hold as an
    /// identity boundary and calls `browse::reset()`. Every `browse` fixture in the suite seeds
    /// exactly such a table (`ServerId::UNSET` sources), so this wipes it.
    ///
    /// That is correct behaviour for `sync_roster` and it is why the derivation may only run
    /// under `testlock::serial()`. Watched red before the fix in the only way this class can be:
    /// the wipe landed on ANOTHER thread's test — `app::chrome`'s
    /// `four_libraries_on_two_servers_publish_two_type_destinations` came back with only Home and
    /// Search in the strip, about one full-suite run in six. Here the same chain is on one
    /// thread, under the guard, and is therefore deterministic.
    #[test]
    fn deriving_the_surface_alphabet_empties_a_seeded_browse_table() {
        let _guard = crate::testlock::serial();
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        assert_eq!(
            crate::browse::section_count(),
            4,
            "the fixture the browse-backed tests across the suite seed"
        );
        let _ = overlay_alphabet();
        assert_eq!(
            crate::browse::section_count(),
            0,
            "deriving the alphabet runs real frames and empties the section table — so it must \
             never run on a thread that does not hold testlock::serial()"
        );
        crate::browse::reset();
    }

    /// **Pins the focusprobe's player-overlay word to THIS module's `overlay_word`, not a second
    /// hand-written copy.** `app/run.rs`'s `probe_screen` closure (the focus fingerprint's
    /// `Route::Player` arm) used to carry its OWN `match overlay { Overlay::None => "none", … }`
    /// table, with a comment claiming it printed "the same words the heartbeat's `overlay=`
    /// uses" — a claim nothing checked, and exactly the shape that goes stale silently: a word
    /// edited on one side (a rename, a typo, a new `Overlay` variant) would make the focus
    /// fingerprint and the heartbeat disagree about the SAME frame's overlay, and nothing here
    /// would fail. `probe_screen` is a closure local to `run()`, not a free function this test can
    /// call, so — the same idiom `search_owned_tests.rs`'s chrome-guard pin uses for the same
    /// reason — this reads `run.rs`'s own source and asserts the arm DELEGATES to `overlay_word`
    /// rather than re-deriving the mapping inline.
    ///
    /// Observed RED before the unification: the arm read
    /// `overlay: match overlay { Overlay::None => "none", Overlay::Menu => "menu", … }`, which
    /// contains no `overlay_word(` call at all — this test failed as designed. A second manual
    /// check confirmed the pin actually discriminates rather than merely checking for a
    /// substring: with the delegating call in place, temporarily reintroducing a stray
    /// `Overlay::None =>` arm beside it (simulating a partial revert to a hand-rolled table) also
    /// turned this test red; reverted after observing it.
    #[test]
    fn focusprobe_player_overlay_delegates_to_the_shared_overlay_word_function() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/run.rs"),
        )
        .expect("read run.rs");
        let start = src
            .find("crate::focusprobe::Screen::Player {")
            .expect("probe_screen must build a focusprobe::Screen::Player");
        let end = src[start..]
            .find("},")
            .map(|i| start + i)
            .expect("the Player arm must close with `},`");
        let arm = &src[start..end];
        assert!(
            arm.contains("overlay_word("),
            "the focusprobe's Route::Player arm must call the shared overlay_word(...) \
             function (the same one the heartbeat uses) rather than re-deriving the mapping; \
             found:\n{arm}"
        );
        assert!(
            !arm.contains("Overlay::None =>") && !arm.contains("Overlay::Menu =>"),
            "a hand-written Overlay match here means a SECOND overlay-word table exists \
             alongside overlay_word; found:\n{arm}"
        );
    }
}

#[cfg(test)]
mod root_back_tests {
    //! **BACK at a ROOT hands the screen back to the television, and the app keeps running.**
    //!
    //! [`back_at_root`] is driven for real — it is the app's whole answer to "there is nowhere
    //! further back to go", and the regression to catch is a future edit putting `running = false`,
    //! or a modal question, back where the platform call now goes. [`after_cancel`] is pure,
    //! because its callers reach `auth`/`webos`, neither of which a unit test wants to drive.
    //!
    //! **Phase 6 retired the other half this module doc used to describe** — `onboarding_back`,
    //! `OnboardBack` and their five tests, which pinned issues #16-#18's rule as it was reached from
    //! the legacy `key_onboarding` key ladder. The RULE did not change (`input::
    //! login_or_profiles_root_back` still performs exactly the `after_cancel` dance those tests
    //! exercised, immediately below); what moved is WHO decides "is this press the screen's own
    //! modal or the app's root" — since phase 6 that is `screens::login`/`screens::profiles`'s own
    //! job, reading their own focus-engine state (a PIN pad's open flag, on the owned
    //! `ProfilesScreen`) that this module cannot see and must not reach into (`app/` never names a
    //! sibling `screens/` module's internals). A host test of that half now belongs beside the
    //! screens that make the decision, not here.
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
            Route::Player,
        ] {
            assert_eq!(page(r), Node::Home);
        }
    }

    // (`a_card_menu_returns_to_the_page_it_was_opened_over` stood here — the reported bug, that a
    // *Play from Start* on the detail page's RELATED shelf returned to Home instead of to the page
    // the menu was opened over. It graded `origin_here` through `page_of` for all six `MenuHost`
    // variants. The menu is a surface since phase 10 and never moves `app.route` at all, so
    // `origin_here` is asked about the HOST page directly and there is nothing left to resolve —
    // the same removal, and for the same reason, as the three `Route::Account { over: BarHost }`
    // cases that stood beside them until item 2 of this phase.)

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
