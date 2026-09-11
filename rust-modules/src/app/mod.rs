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
    classify, is_bound, is_ok, Key, SDLK_DOWN, SDLK_ESCAPE, SDLK_LEFT, SDLK_PAGEDOWN,
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
pub(crate) mod words;
pub(crate) mod clock;
mod recorder;
pub(crate) mod events;
pub(crate) mod lifecycle;
pub(crate) mod playback;
pub(crate) mod input;
pub(crate) mod bridge;
mod chrome;
pub(crate) mod content;
pub(crate) mod run;
use self::boot::*;
use self::events::*;
use self::lifecycle::*;
use self::playback::*;
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
// **The screen ARGUMENT is `screens::registry`'s** since restructure phase 10 (§2.1): the
// registry owns the concrete `ScreenArg` and the one `mount` match, so `app/` reads it here
// rather than declaring it. Imported at the tree's root because every module under `app/`
// that requests a navigation names it.
use crate::screens::registry::AppArg;
/// The shared top strip's vocabulary: what a pill INDEX means. Every site that turns a pill into a
/// destination `match`es on this, so a pill the app has not been taught about is a compile error
/// rather than a silent library open — see `widgets::Pill`.


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
    // (`route`, `play_from`, `trail` and `nav_pending` stood here — four fields that between them
    // were a second navigation system: which page is on top, which page the live playback returns
    // to, the pages behind the one on screen, and the route change a fade is carrying. Each is the
    // CONTAINER's now — `NavStack`'s top entry, `screens::player::Origin`, the stack itself, and
    // `NavStack::pending` under a `PageDip`. D1, restructure phase 12.)
    /// **`activate_card`'s show/season Play, between its ASYNC detail request and the landing
    /// that decides play-vs-open** (D7: the item-menu/card-row Play arm used to call
    /// `MetadataCmd::LoadDetailNow` — a BLOCKING fetch on the press frame — and read
    /// `metadata::current()` on the very next statement, which only worked because the load had
    /// already finished by then). See `input::menu_play_tick`, driven every frame beside
    /// `pump_detail()` (`app/run.rs`).
    pub(crate) menu_play_await: Option<MenuPlayAwait>,
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
    /// The frame plan's GLASS half (spec §8.3): the one shared backdrop-refresh cadence, the tile
    /// bands' visible lifetime, and the dev load dial's whole live state. Eight `static mut`s in
    /// `ui/glassload.rs` and two in `ui/widgets.rs` until phase 11; fields of this since. (The
    /// budget half lives on the `Dispatcher` — the frame scheduler owns admission, §2.2.)
    pub(crate) glass: crate::ui::frame::glass::GlassPlan,
    /// **The container tree, and since phase 12 (D1) it is the ONE navigation authority.** It was
    /// a shadow through 3b and a half-real tree through 5b, kept in step with an `App.route` field
    /// by `bridge::sync_page` on every frame; both the field and the mirror are deleted. Every
    /// navigation is now a `NavOp` asked at the press that wants it (`app/bridge.rs`'s `nav_root`/
    /// `nav_push`/`nav_pop`/`nav_pop_to`/`nav_cancel`), the pages behind the one on screen are this
    /// stack's entries, and [`App::route`] is a one-line read of its top. The loop still hands it
    /// input and asks it to draw (`app/bridge.rs`'s coexistence contract).
    /// **It carries the ONE frame budget** (spec §2.2/§8.1): the frame scheduler owns admission,
    /// and there is one scheduler. `App` held a second `Budget` of its own until phase 11 — the
    /// poster upload spent that one while the dispatcher's step-8 present decision consulted the
    /// other, whose queue flag had no writer at all, so the two halves of one mechanism could
    /// never agree with each other.
    pub(crate) pages: crate::ui::dispatch::Dispatcher<bridge::AppHost>,
    /// Inputs collected for the dispatcher this iteration (`bridge` module doc).
    pub(crate) inputs: Vec<crate::ui::machine::InputEvent<u32>>,
    /// What that dispatcher borrows: the mounter, the real `TtfMeasure`, the store deliveries,
    /// the consent machine and the queue of requests an owned screen makes of this loop.
    pub(crate) bridge: bridge::Bridge,
}

impl App {
    /// **Which page is on top** (spec §15.2) — the container's answer, and since D1 the ONLY one.
    ///
    /// `App.route` was a `Route` field beside `App.trail`, `App.nav_pending` and `App.play_from`,
    /// kept in step with the tree by `bridge::sync_page` on every frame. All four are deleted:
    /// this is a one-line read of the entry the `NavStack` holds.
    ///
    /// The `Home` fallback covers exactly one moment — the frames before the first page is
    /// minted, which is boot, where Home is the honest answer to "what is behind everything".
    pub(crate) fn route(&self) -> AppArg {
        self.pages.top_arg().cloned().unwrap_or(AppArg::Home)
    }
}

/// Everything `plex_run` does before minting the main-thread token — every probe, gate and
/// dev-trigger arm that has to run BEFORE `boot()`, in the order this doc explains one by one.
/// Extracted so `plex_run` itself stays a ten-line skeleton (D4): this is not a phase-function
/// split of ONGOING per-frame work like `app/run.rs`'s, but the one-shot bring-up sequence, and
/// splitting it out changes nothing about when any of it runs.
///
/// Returns the telemetry guard, which MUST outlive the whole process — `crate::telemetry::boot`'s
/// own doc: the crash channel's scope is snapshotted here, and `diag::event` reads its live
/// published decision for the rest of the run, not only for as long as this function's own stack
/// frame exists. `plex_run` binds it as `_telemetry_guard` for exactly that reason: a bare
/// `pre_boot_diagnostics();` would drop it at the end of THIS call, before a single frame ran.
fn pre_boot_diagnostics() -> crate::telemetry::native::Guard {
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
    let telemetry_guard = crate::telemetry::boot();
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
    telemetry_guard
}

/// The run+teardown sequence `plex_run` hands the mounted `App` to, split out for the same reason
/// as [`pre_boot_diagnostics`] (D4): `plex_run` stays a ten-line skeleton naming only the THREE
/// real phases (pre-boot diagnostics, `boot`, this), never the steps inside any one of them.
unsafe fn run_and_shutdown(app: &mut App) {
    run::run(app);
    std::mem::replace(&mut app.rec, recorder::Recplay::Off).finish();
    run::shutdown(&mut app.player.session, &mut app.adapters.player);
}

/// The ten-line skeleton (D4): pre-boot diagnostics, mint THE main-thread token (this function IS
/// the SDL main thread — `boot`'s own doc says what it does with it), boot, run to completion. See
/// [`pre_boot_diagnostics`]/[`boot`]/[`run_and_shutdown`] for what each phase actually does.
#[no_mangle]
pub extern "C" fn plex_run(pms_host: *const c_char, pms_port: c_int) -> c_int {
    let _telemetry_guard = pre_boot_diagnostics();
    let main_thread = unsafe { crate::task::MainThread::assume() };
    let mut app = match unsafe { boot(pms_host, pms_port, main_thread) } {
        Ok(app) => app,
        Err(code) => return code,
    };
    unsafe { run_and_shutdown(&mut app) };
    0
}


// **Four `#[cfg(test)] mod` blocks stood here** and D8 gives each one the home of the thing it
// grades, so that `plex_run`'s module reads as the entry point it is (`ci/check-deps.sh`'s
// `testmod` gate is zero here now):
//
// * `player_return_tests` — "where playback returns to", over an `Origin::From(Node)` and a
//   `return_page`/`set_origin`/`node_route` trio of pure functions on a described history. D1 made
//   the origin an `EntryId` on the mounted player screen, so the same questions are asked of a
//   real container, beside `playback::enter_player`/`exit_player` — including two the description
//   could not reach: the spot the page comes back at, and an origin that is really gone.
// * `key_layout_tests` — beside `events::decode_key`/`encode_key`, whose byte offsets it pins.
// * `route_tests` — split in two, each half beside its subject: the direct-screen server rule is
//   `boot::resolve_direct_server`'s, and the profile chip's alphabet is `ScreenArg::chrome`'s, in
//   `screens::registry`.
// * `heartbeat_word_tests` — with the whole word alphabet it grades, in [`words`].
