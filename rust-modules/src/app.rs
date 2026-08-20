//! plex_run — the Rust app core (was the body of src/main.c). Owns SDL init, the
//! event loop, input decode, the per-frame tick, draw orchestration, app lifecycle,
//! the buffer-feed pump orchestration, and the dev triggers. The C boot shim
//! (main.c) sets up the log + crash tracer, then calls plex_run(). The only C left
//! below us is the starfish.c C++/ACB seam (the engine itself is Rust: crate::player).
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
const SIM_TAG: &str = if cfg!(feature = "hostsim") { " sim=1" } else { "" };

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
const SDL_WINDOW_FLAGS: u32 = if cfg!(feature = "hostsim") { 0x2 | 0x2000 } else { 0x2 | 0x1 };
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
    classify, is_back, is_ok, Key, SDLK_DOWN, SDLK_ESCAPE, SDLK_LEFT, SDLK_PAGEDOWN, SDLK_PAGEUP,
    SDLK_RETURN, SDLK_RIGHT, SDLK_UP, WCODE_CH_DOWN, WCODE_CH_UP, WCODE_PAUSE, WCODE_PLAY,
    WCODE_POINTER_HIDDEN, WCODE_STOP,
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
fn desktop_window_size() -> (c_int, c_int) {
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

extern "C" {
    fn SDL_SetMainReady();
    fn SDL_SetHint(name: *const c_char, value: *const c_char) -> c_int;
    fn SDL_Init(flags: u32) -> c_int;
    fn SDL_GetCurrentVideoDriver() -> *const c_char;
    fn SDL_GL_SetAttribute(attr: c_int, value: c_int) -> c_int;
    fn SDL_CreateWindow(title: *const c_char, x: c_int, y: c_int, w: c_int, h: c_int, flags: u32) -> *mut c_void;
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
use crate::ui::trail::{Node, Trail};
/// The shared top strip's vocabulary: what a pill INDEX means. Every site that turns a pill into a
/// destination `match`es on this, so a pill the app has not been taught about is a compile error
/// rather than a silent library open — see `widgets::Pill`.
use crate::ui::widgets::Pill;

/// Log every Rust panic (message + source location + thread) to the event log AND the
/// persistent crash log BEFORE it unwinds. A panic that crosses an extern "C" boundary
/// (e.g. libav calling ff::read_cb/seek_cb) aborts the process (SIGABRT) — by then the
/// message is gone, so capturing it here is the only way to see WHAT panicked. Pairs with
/// main.c's crash tracer, which re-raises the signal for a full webOS crashd backtrace.
fn install_panic_logger() {
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
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&crate::paths::in_runtime_dir("plxnative-crash.log")) {
            let _ = writeln!(f, "{line}");
        }
        default(info); // preserve default behaviour (stderr -> plxnative-stderr.log)
    }));
}

#[inline]
fn rd_u32(ev: &[u8], off: usize) -> u32 {
    u32::from_ne_bytes([ev[off], ev[off + 1], ev[off + 2], ev[off + 3]])
}

/// A keyboard event's `(state, wcode, sym)`, decoded from the raw event bytes.
///
/// **The two SDLs disagree about this struct, and nothing warns you.** LG's fork writes
/// `state` (u32) at +16, the webOS keycode at +20 and the SDL sym at +24. Stock SDL2 —
/// what the host simulator links — has `SDL_KeyboardEvent { type, timestamp, windowID,
/// state:u8@12, repeat:u8@13, pad, pad, keysym{ scancode:u32@16, sym:i32@20, … } }`, so
/// every field the app reads is at a different offset and there is no webOS keycode at all.
/// Reading the fork's offsets out of a stock event yields the window id as a keystate and
/// a scancode as a sym — plausible-looking garbage rather than a crash.
///
/// `cfg!` rather than `#[cfg]` deliberately: both arms stay compiled on both platforms, so
/// the one nobody is currently building cannot rot. This is the single site that knows the
/// layout — `rd_u32`'s callers elsewhere read pointer events, whose offsets already agree.
#[inline]
fn decode_key(ev: &[u8]) -> (u32, u32, u32) {
    if cfg!(feature = "hostsim") {
        let pressed = *ev.get(12).unwrap_or(&0) as u32;
        let repeat = *ev.get(13).unwrap_or(&0) as u32;
        let sym = rd_u32(ev, 20);
        // Rebuild the fork's packed state byte-for-byte: low byte pressed(1)/released(0),
        // bit 0x100 auto-repeat. Everything downstream tests exactly those two.
        let state = pressed | if repeat != 0 { 0x100 } else { 0 };
        // A synthetic event from the remote FIFO parks its webOS keycode in the spare `unused`
        // field; a real keypress leaves it zero, and then the keyboard mapping supplies one.
        let injected = rd_u32(ev, 28);
        let wcode = if injected != 0 { injected } else { host_wcode(sym) };
        (state, wcode, sym)
    } else {
        (rd_u32(ev, 16), rd_u32(ev, 20), rd_u32(ev, 24))
    }
}

/// The Magic Remote button a desktop keyboard stands in for, or 0.
///
/// Only the keys with NO sym equivalent need this. Navigation and OK/BACK already work on a
/// keyboard through `is_ok`/`is_back`, which accept RETURN/ESCAPE/'q' — those predicates were
/// always keyboard-capable, which is why the simulator needs no remapping layer for them.
#[inline]
fn host_wcode(sym: u32) -> u32 {
    // ASCII literals spelled numerically: `b'p' as u32` is an expression, not a pattern.
    match sym {
        32 => crate::ui::consts::WCODE_PAUSE, // space
        112 => crate::ui::consts::WCODE_PLAY, // 'p'
        115 => crate::ui::consts::WCODE_STOP, // 's'
        8 => crate::ui::consts::WCODE_BACK,   // backspace
        _ => 0,
    }
}

/// The bytes a synthetic key event needs, in whichever layout [`decode_key`] reads.
///
/// **The inverse of `decode_key`, and the pair is only correct together.** They already shipped
/// disagreeing once: the simulator accepted every FIFO token and never moved, because this end
/// wrote LG's fork layout while the reading end had been taught stock SDL2's. Nothing in the
/// compiler couples them, so `key_bytes_round_trip` below is what does.
///
/// Pure, and separate from the `SDL_PushEvent` that consumes it, precisely so that test can run on
/// the host — `make check` links no SDL.
fn encode_key(sym: c_uint, wcode: c_uint, down: bool) -> [u8; 128] {
    let mut ev = [0u8; 128];
    ev[0..4].copy_from_slice(&if down { SDL_KEYDOWN } else { SDL_KEYUP }.to_ne_bytes());
    if cfg!(feature = "hostsim") {
        ev[12] = u8::from(down); // state
        ev[13] = 0; // repeat
        ev[20..24].copy_from_slice(&sym.to_ne_bytes());
        // Stock SDL_Keysym ends in a spare `unused` u32 (event offset +28). A webOS keycode has
        // nowhere else to go on this layout, and several tokens carry ONLY a wcode (`pause` is
        // sym 0, wcode 72), so deriving it from the sym would lose them. `decode_key` prefers
        // this field and falls back to the keyboard mapping when it is zero — which is what a
        // real desktop keypress leaves it as.
        ev[28..32].copy_from_slice(&wcode.to_ne_bytes());
    } else {
        ev[16..20].copy_from_slice(&if down { 1u32 } else { 0 }.to_ne_bytes()); // state
        ev[20..24].copy_from_slice(&wcode.to_ne_bytes());
        ev[24..28].copy_from_slice(&sym.to_ne_bytes());
    }
    ev
}

/// Hide the Magic Remote's on-screen pointer. A webOS-only concept: there is no such cursor to
/// hide on a desktop, and `SDL_webOSCursorVisibility` exists in no SDL but LG's fork.
///
/// One door rather than a branch at each of the five call sites, so the platform question is
/// asked once and the call sites read the same on both.
#[inline]
unsafe fn hide_cursor() {
    #[cfg(not(feature = "hostsim"))]
    {
        SDL_webOSCursorVisibility(0);
    }
}
#[inline]
/// An SDL pointer event's position, converted from window pixels to the authored 1920x1080 canvas.
///
/// THE one place event coordinates enter the UI, so the conversion cannot be forgotten at a new
/// call site — there are nine, and patching them individually is how the tenth ends up wrong.
/// `surface::to_logical` is the identity while the drawable is 1920x1080, which it is on every
/// television seen so far.
fn ptr_xy(ev: &[u8]) -> (f32, f32) {
    crate::surface::to_logical(rd_i32(ev, 20) as f32, rd_i32(ev, 24) as f32)
}

fn rd_i32(ev: &[u8], off: usize) -> i32 {
    i32::from_ne_bytes([ev[off], ev[off + 1], ev[off + 2], ev[off + 3]])
}

/// Map a remote-control token (from the `crate::remote` FIFO) to the `(sym, wcode)` a
/// real Magic-Remote press would carry — the pair the ONE key handler already matches
/// (see `ui::consts`). Returns None for an unknown token. Kept deliberately small: the
/// core nav set + OK/BACK + the transport keys that testing needs.
fn remote_token_key(tok: &str) -> Option<(c_uint, c_uint)> {
    Some(match tok {
        "up" => (SDLK_UP, 0),
        "down" => (SDLK_DOWN, 0),
        "left" => (SDLK_LEFT, 0),
        "right" => (SDLK_RIGHT, 0),
        "ok" | "enter" | "select" => (SDLK_RETURN, 0), // is_ok()
        "back" | "esc" => (SDLK_ESCAPE, 0),            // is_back()
        "pageup" | "chup" => (SDLK_PAGEUP, WCODE_CH_UP),
        "pagedown" | "chdown" => (SDLK_PAGEDOWN, WCODE_CH_DOWN),
        "play" => (0, WCODE_PLAY),
        "pause" => (0, WCODE_PAUSE),
        "stop" => (0, WCODE_STOP),
        // The system keyboard's own two edit keys (`ui::consts`' doc has the protocol). They are
        // here because they are otherwise UNREACHABLE without a human at the panel: no trigger
        // raises the keyboard and `SDL_PushEvent` cannot carry a text event on the simulator, so
        // without these the only grader for backspace and Clear all is somebody's thumb.
        "backspace" | "del" => (crate::ui::consts::SDLK_BACKSPACE, 42),
        "clear" => (crate::ui::consts::SDLK_CLEAR, 156),
        _ => return None,
    })
}

/// Synthesize a Magic-Remote pointer click at authored 1920x1080 coords (the browser
/// remote's click-on-the-stream): two motion events, then button down+up. The first
/// motion is a >=120px jitter so the accumulated pointer distance defeats the
/// D-pad-mode pointer gate (`Pointer::mot_accum < 120` swallows small motions after D-pad use);
/// the second lands on the target. The LG SDL fork's mouse events carry x@20 / y@24
/// (i32) — the only fields the handlers read.
///
/// Click only, deliberately: forwarding hover moved app focus on every pass of the
/// mouse over the streamed picture (parking it on a top-band tab pill, so the next
/// ENTER opened the library). The host page draws its own local crosshair instead.
fn remote_synth_ptr(x: i32, y: i32) {
    let mut ev = [0u8; 128];
    let mut push = |et: u32, px: i32, py: i32| {
        // Authored coords go onto SDL's queue as WINDOW pixels, because that is what a real
        // pointer event carries and `ptr_xy` converts every one of them back. Skipping this would
        // transform the synthetic path twice on a scaled surface — and it is the path the whole
        // headless test harness clicks through, so it would fail in a way that looked like the UI.
        let (px, py) = crate::surface::to_physical(px as f32, py as f32);
        let (px, py) = (px.round() as i32, py.round() as i32);
        ev[0..4].copy_from_slice(&et.to_ne_bytes());
        ev[20..24].copy_from_slice(&px.to_ne_bytes());
        ev[24..28].copy_from_slice(&py.to_ne_bytes());
        unsafe { SDL_PushEvent(ev.as_ptr() as *const c_void) };
    };
    let jx = if x >= 200 { x - 200 } else { x + 200 };
    push(SDL_MOUSEMOTION, jx, y);
    push(SDL_MOUSEMOTION, x, y);
    push(SDL_MOUSEBUTTONDOWN, x, y);
    push(SDL_MOUSEBUTTONUP, x, y);
}

/// Synthesize a full remote-key press (key-down then key-up) and push both onto SDL's
/// own event queue, so the existing poll loop consumes them as if they came off the
/// wayland input path. The LG SDL fork's `SDL_KeyboardEvent` carries state@16 /
/// wcode@20 / sym@24 (native-endian; the TV is LE), and the handler reads press vs
/// release from `state & 0xff` — so the down carries state=1, the up state=0. Both
/// are required: a grid-card OK arms on down and *commits on release*.
fn remote_synth_key(sym: c_uint, wcode: c_uint) {
    remote_synth_key_edge(sym, wcode, true);
    remote_synth_key_edge(sym, wcode, false);
}

/// ONE edge of a remote key press. Split out for the `okdown`/`okup` FIFO tokens, because a
/// **press-and-hold** is only expressible as two tokens with real time between them: the item menu
/// opens on `press::is_long`, which measures the interval between the down and the up. The paired
/// `remote_synth_key` above is this called twice back to back (a tap).
fn remote_synth_key_edge(sym: c_uint, wcode: c_uint, down: bool) {
    let ev = encode_key(sym, wcode, down);
    unsafe { SDL_PushEvent(ev.as_ptr() as *const c_void) };
}

// ui focus state lives in ui::home; reach it through its accessors
#[inline]
fn g_fr() -> c_int { crate::ui::home::row() }
#[inline]
fn g_snap() -> f32 { crate::ui::home::snap_target() }
#[inline]
fn set_fr(v: c_int) { crate::ui::home::set_row(v) }
#[inline]
fn set_snap(v: f32) { crate::ui::home::set_snap_target(v) }

// transport state — was the C playback globals; now crate::player (atomics)
#[inline]
fn paused() -> bool { crate::player::TX.paused.load(Relaxed) }
#[inline]
fn set_paused(v: bool) { crate::player::TX.paused.store(v, Relaxed) }
#[inline]
fn hud_until() -> u32 { crate::player::TX.hud_until.load(Relaxed) }
#[inline]
fn set_hud(x: u32) { crate::player::TX.hud_until.store(x, Relaxed) }
/// Raise the HUD to at least `now + ms`, never PULLING IN a deadline already further out.
///
/// `set_hud` stores an absolute instant, so an unconditional call SHORTENS whatever was there.
/// The headless capture path pins the HUD for `HUD_HEADLESS_MS` (60 s), and the marker prompts
/// below fire mid-playback — calling `set_hud` there cut that pin to the 4.5 s linger and the
/// transport vanished out from under a live Skip button (seen on device, not in review).
/// Comparison is plain `>`, matching `hud_shown`'s own non-wrapping `now < until`.
#[inline]
fn extend_hud(now: u32, ms: u32) {
    let want = now.saturating_add(ms).max(1);
    if want > hud_until() {
        set_hud(want);
    }
}
/// Is the transport HUD on screen? Its timer is live, OR playback is paused, OR the pipeline is
/// BUSY — unless the user explicitly dismissed it (UP from the top row), which holds until the
/// next key but cannot hide a stalled pipeline's read-out.
///
/// **The `loading()` term is load-bearing and there must be exactly ONE predicate.** The draw path
/// and the pointer path used to spell it out inline while the three KEY sites and the focus PARKER
/// did not, and the divergence was worst in the one state this app most needs a user to report:
/// stuck in `Buffering` with the 4.5 s linger expired, the transport is drawn, but every key site
/// believed it hidden — so the parker reset `hud.nav` to the scrubber on EVERY frame, UP was eaten
/// as a "reveal", and focus could not reach the control row at all. The `…` disc, and the
/// diagnostics read-out behind it, were unreachable in exactly the stall they explain.
///
/// It was briefly TWO functions, a timer-only `hud_shown` wrapped by this one. That is the same
/// trap with a friendlier name on it — seven call sites, no compiler help, and "shown" is the
/// obvious one to reach for. One predicate, no wrong choice.
#[inline]
fn hud_visible(now: u32, until: u32, is_paused: bool, dismissed: bool) -> bool {
    ((now < until || is_paused) && !dismissed) || crate::player::loading()
}

/// The transport's visibility predicate. Almost nothing else in this file is host-testable — it is
/// the SDL event loop — but this pair is pure enough to pin, and the bug it encodes cost the
/// diagnostics overlay its whole reason for existing.
#[cfg(test)]
mod hud_visibility_tests {
    use super::*;
    use crate::player::PlaybackState;

    /// Drive the derived playback state through the field the pump owns. Crate-global, so the whole
    /// body holds `testlock::serial()` — `state()` is read by other modules' tests too.
    fn with_state<T>(s: PlaybackState, f: impl FnOnce() -> T) -> T {
        let _g = crate::testlock::serial();
        let prev = crate::player::swap_state_for_test(s);
        let out = f();
        crate::player::restore_state_for_test(prev);
        out
    }

    /// THE regression. Stuck in `Buffering` with the linger long expired and nothing paused, the
    /// timer predicate says hidden while the transport is in fact drawn — so every key site and the
    /// focus parker must use the STATE-aware one, or focus is reset to the scrubber every frame and
    /// the `…` disc cannot be reached in the one state worth reporting.
    #[test]
    fn a_stalled_pipeline_keeps_the_transport_reachable_after_the_linger_expires() {
        with_state(PlaybackState::Buffering, || {
            // the timer alone would say hidden — 9 s past the linger, nothing paused
            let (now, expired) = (10_000u32, 1_000u32);
            assert!(hud_visible(now, expired, false, false), "on screen, so keys must reach it");
        });
    }

    /// While playing normally the two agree — an expired linger really does mean hidden, or the HUD
    /// would never auto-hide at all.
    #[test]
    fn a_healthy_playing_pipeline_still_auto_hides() {
        with_state(PlaybackState::Playing, || {
            assert!(!hud_visible(10_000, 1_000, false, false));
            assert!(hud_visible(500, 1_000, false, false), "inside the linger");
            assert!(hud_visible(10_000, 1_000, true, false), "paused pins it up");
        });
    }

    /// An explicit dismiss (UP from the top row) still hides it while healthy — but must NOT be able
    /// to hide it while the pipeline is stalled, because that is the state the user needs to report
    /// and the read-out is pinned on screen there regardless.
    #[test]
    fn dismiss_wins_while_healthy_and_loses_while_stalled() {
        with_state(PlaybackState::Playing, || {
            assert!(!hud_visible(500, 1_000, false, true), "dismissed during playback");
        });
        with_state(PlaybackState::Buffering, || {
            assert!(hud_visible(500, 1_000, false, true), "a stall outranks the dismiss");
        });
    }
}
#[inline]
fn scrub() -> i64 { crate::player::TX.scrub_ns.load(Relaxed) }
#[inline]
fn set_scrub(x: i64) { crate::player::TX.scrub_ns.store(x, Relaxed) }
#[inline]
fn resume_pend() -> bool { crate::player::TX.resume_pend.load(Relaxed) }
#[inline]
fn set_resume_pend(v: bool) { crate::player::TX.resume_pend.store(v, Relaxed) }
#[inline]
fn dur() -> i64 { crate::player::duration_ns() }
#[inline]
fn playpos() -> i64 { crate::player::playpos_ns() }
/// The playhead the user INTENDED, which is not always the one being published. While a seek is
/// still resolving (request → reopen → prime → Play) `playpos()` keeps reporting the PRE-seek spot,
/// so anything that snapshots "where are we?" inside that window snapshots the position the user
/// just left. The rule — an in-flight seek target wins, else the published position — was open-coded
/// at each reader that remembered it (the scrub seed below; the HUD's frozen playhead in
/// `ui/player_hud.rs`) and simply MISSING at the one that did not: the OS-background save took a bare
/// `playpos()`, so backgrounding right after a seek stored the pre-seek spot and the foreground
/// restore replayed from there — and teardown clears the pending target, so nothing self-corrected.
/// Use this at every reader that means "where the user is"; keep the raw `playpos()` only where the
/// PUBLISHED position is the point (the re-pause gate, which is already behind `seek_pending() < 0`,
/// and the heartbeat's `pos=`, which the harness grades real playback progress from).
#[inline]
fn intended_pos() -> i64 { crate::player::intended_pos_ns() }
#[inline]
fn frames() -> i32 { crate::player::frames() }
/// Advance the once-per-second LOOP-RATE window: bump `iters_ct` and, when a full second has
/// elapsed, recompute `loop_shown`, reset the window, and return `true` so the caller logs the
/// heartbeat with its own route/overlay tag. Shared by the player and home/detail draw paths.
///
/// This counts **loop iterations, not frames**. Since the present gate (`ui::idle`) landed the two
/// are different numbers, and conflating them is the single most reliable way to misread this app:
/// a settled screen runs the loop at the `IDLE_POLL_MS` rate while swapping nothing. The frame
/// count lives beside it in the heartbeat as `fps=`, from `ui::idle::take_presents`.
fn loop_tick(iters_ct: &mut i32, loop_t: &mut u32, loop_shown: &mut i32, now: u32) -> bool {
    *iters_ct += 1;
    if now.wrapping_sub(*loop_t) < 1000 {
        return false;
    }
    *loop_shown = (*iters_ct as f32 * 1000.0 / now.wrapping_sub(*loop_t) as f32 + 0.5) as i32;
    *iters_ct = 0;
    *loop_t = now;
    true
}
#[inline]
fn seek_pending() -> i64 { crate::player::seek_pending() }
#[inline]
fn request_seek(x: i64) { crate::player::request_seek(x) }
/// Commit a scrub to `target` and clear the preview. If we were PAUSED, STAY paused: the feed gate
/// (pump) needs `!paused` to prime the new position, so drop paused just long enough for the seek
/// to present its landed frame, then arm `resume_pend` so the per-frame loop re-freezes on it (same
/// mechanism as background-restore). `repause_at` is that re-freeze wait target. A scrub while
/// playing takes the else branch and simply resumes at the new position.
fn commit_seek(target: i64, repause_at: &mut i64) {
    request_seek(target);
    set_scrub(-1);
    if paused() {
        *repause_at = target;
        set_resume_pend(true);
        set_paused(false);
    }
}
#[inline]
fn is_started() -> bool { crate::player::is_started() }

// ---- the route vocabulary, and the pure questions asked ABOUT a route -------------------------
//
// These are pure functions of a `Route` that read and write no app state, which is what lets
// `route_tests` at the bottom of this file grade them — and grading them is the point, because they
// decide things that have shipped wrong (the teardown rule below, twice), and a `Route` that only
// exists inside the run loop's body is a decision no host test can reach. The loop still owns every
// VALUE — `route` is a local, the trail is a local.

/// Exclusive route state machine (replaces 5 entangled bools). Overlays live INSIDE
/// Player because they only mean anything during playback; Detail and Player are mutually
/// exclusive. Deleting the old bools makes the compiler flag any un-migrated read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Overlay {
    None,
    Menu,
    Info,
    Chapters,
    /// the `…` disc's overflow popover (`ui/more_menu.rs`)
    More,
}
/// Which screen a [`Route::ItemMenu`] popover is sitting over.
///
/// The menu is a popover on a LIVE screen, not a page of its own — the card and its row keep
/// drawing and animating behind it — so the route has to name the screen underneath, both to
/// go on drawing/updating it and to know where the popover closes back to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuHost {
    /// a home shelf card
    Home,
    /// the detail page's episode filmstrip
    Detail,
}
impl MenuHost {
    /// the route the popover returns to when it closes
    fn route(self) -> Route {
        match self {
            MenuHost::Home => Route::Home,
            MenuHost::Detail => Route::Detail,
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Route {
    Login,    // plex.tv sign-in (QR) — shown when there's no usable session
    Profiles, // "who's watching" Plex Home picker
    Home,
    Account,  // Home + the top-left profile menu popover (change profile / sign out)
    /// `over` + the press-and-hold context menu popover (ui/item_menu.rs)
    ItemMenu { over: MenuHost },
    Library,  // the browse grid (ui/library.rs); its sort/filter menus are internal state
    Detail,
    /// The person/actor page (ui/person.rs), reached by OK on a detail page's cast
    /// headshot. Exclusive with Detail like every other node — what is UNDER it is the BACK
    /// trail's business (`ui::trail`), not this enum's, which is exactly why the trail
    /// exists: a `Route` names one screen, and person→detail→person is three.
    Person,
    /// The Search screen (`ui/search/`). A PEER of Home and the Library, not a stacking
    /// page: it is reached from the strip's last pill and BACK from it returns to Home, so
    /// it needs no trail node of its own — what it OPENS stacks, but it does not.
    Search,
    Player { overlay: Overlay },
}

/// Which routes draw the shared top tab bar — the ONE test behind `ui::nav`'s
/// continuous-chrome rule. Exhaustive for the same reason `Nav::wears_tab_bar` is: a new
/// screen must not be able to answer this by accident. (`Account`/`ItemMenu over Home` draw
/// Home underneath, so the bar is on screen there too; Detail and Person do not have one,
/// which is what makes every transition to or from them fade the bar with the page.)
fn route_wears_tab_bar(r: Route) -> bool {
    match r {
        Route::Home | Route::Library | Route::Search => true,
        Route::Account => true,
        Route::ItemMenu { over } => matches!(over, MenuHost::Home),
        Route::Login | Route::Profiles | Route::Detail | Route::Person | Route::Player { .. } => false,
    }
}
/// The PAGE a trail node names — the ONE Node→[`Route`] mapping in the app. Both things
/// that have to know it read it here: `enter_node` flips the route through it after
/// mounting, and [`node_wears_tab_bar`] answers the chrome question by handing it to
/// [`route_wears_tab_bar`], so a node and the page it mounts can never answer differently.
///
/// A free fn and not `Node::route`, which is what it would rather be: `Node` belongs to
/// `ui::trail` (deliberately — the trail decides nothing about screens and cannot see `Route`),
/// so the inherent `impl` would be a foreign one, which `non_local_definitions` warns about.
fn node_route(n: &Node) -> Route {
    match n {
        Node::Home => Route::Home,
        Node::Library => Route::Library,
        Node::Search { .. } => Route::Search,
        Node::Person { .. } => Route::Person,
        Node::Detail { .. } => Route::Detail,
    }
}
/// The same question about a TRAIL node — what a BACK's destination wears, peeked before the
/// pop, and what a forward `Nav::Open` is about to put on screen. DERIVED from
/// [`route_wears_tab_bar`] through [`node_route`] rather than listing the node kinds a
/// second time: a node and the route it mounts are the same page, and the two lists had no
/// way to stay in step beyond someone noticing.
fn node_wears_tab_bar(n: &Node) -> bool {
    route_wears_tab_bar(node_route(n))
}
/// The PAGE a route draws. An `ItemMenu` is a popover on a LIVE screen and `Account` is one
/// over Home, so the page being left by a navigation out of either is the screen underneath
/// — which is what both the teardown and the spot below have to be asked about.
fn page_of(r: Route) -> Route {
    match r {
        Route::ItemMenu { over } => over.route(),
        Route::Account => Route::Home,
        other => other,
    }
}
/// The page's TEARDOWN — what leaving it FOR GOOD has to run, handed to `ui::nav` so it
/// happens at the fade floor instead of on the press frame (see that module's doc: run
/// early, `detail::close`'s `metadata::clear` empties the page *during its own fade-out*).
///
/// WHEN a navigation asks for one is [`stays_on_trail`]'s question, not this function's: this is
/// only "what does leaving this page for good have to run".
///
/// Spelled out route by route, exactly as [`route_wears_tab_bar`] above it is and for the
/// same reason: with a `_ => None` catch-all, a new STACKING screen compiles with no
/// teardown at all and silently leaks the item it loaded, which is invisible until the page
/// it left behind reappears under the next one.
fn leave_of(r: Route) -> Option<fn()> {
    match page_of(r) {
        Route::Detail => Some(crate::ui::detail::close as fn()),
        Route::Person => Some(crate::ui::person::leave as fn()),
        // Nothing loaded that outlives the page. Home and the Library keep their stores for
        // as long as the profile does (`browse.rs` is re-ENTERED, never re-queried — that is
        // why `Node::Library` carries no payload), Login/Profiles are boot gates the app
        // leaves once, and a player session is torn down by its own exit path.
        Route::Home | Route::Library | Route::Login | Route::Profiles | Route::Player { .. } => None,
        // Search DOES have one, and it is not a store: the television's keyboard must come
        // down with the page. Dismissing it at the press instead would drop the panel a
        // frame early, while the screen it belongs to is still on screen behind it.
        Route::Search => Some(crate::ui::search::leave as fn()),
        // Unreachable: `page_of` has already resolved a popover onto the screen it sits on,
        // so neither of these ever arrives here. Listed rather than swept into a `_` so the
        // exhaustiveness above is real.
        Route::Account | Route::ItemMenu { .. } => None,
    }
}
/// Does this page STAY MOUNTED behind a forward navigation — is it a page the BACK trail can put
/// back? This is the whole rule for when [`leave_of`] is asked for, and the honest predicate is
/// **trail membership, not direction**.
///
/// A BACK always tears the page down: it is being left for good, by definition. A FORWARD
/// navigation is the interesting half, and the obvious generalisation ("carry the teardown either
/// way") is WRONG: Detail and Person stay on the trail, so closing one on the way deeper would
/// empty the page the user is about to press BACK to — the exact bug `leave_of`'s doc defends
/// against, and `nav`'s retarget rule is built around.
///
/// [`Route::Search`] is the case that made this a predicate rather than a `None`, and it is the
/// one route where the two questions this file otherwise collapses genuinely come apart. It HAS a
/// node now and a result opened from it does stay on the trail — but its teardown is
/// `search::leave`, which dismisses the TELEVISION'S KEYBOARD and drops nothing else, so running it
/// on the way deeper costs nothing and leaving it un-run risks a system panel floating over the
/// page you navigated to. The other three keep their teardown off a forward navigation because
/// theirs EMPTY the page a BACK is about to return to; this one has nothing to empty.
///
/// So `false` here does not mean "no node" any more. It means "leaving this screen always dismisses
/// its keyboard", and the trail push lives in the commit arm, which is where it always did.
fn stays_on_trail(r: Route) -> bool {
    match page_of(r) {
        // exactly the `Node` variants (`node_route`'s domain): a forward navigation leaves these
        // standing behind the destination, which is what makes the common pop a route flip
        Route::Home | Route::Library | Route::Detail | Route::Person => true,
        // stays on the trail, but its teardown rides every exit — see the doc above
        Route::Search => false,
        // Boot gates the app leaves once, and a player session torn down by its own exit path.
        // None of the three has a `leave_of` at all, so this answer is about being honest rather
        // than about having an effect.
        Route::Login | Route::Profiles | Route::Player { .. } => false,
        // Unreachable: `page_of` resolves a popover onto the screen it sits on. Listed rather than
        // swept into a `_`, exactly as `leave_of` above.
        Route::Account | Route::ItemMenu { .. } => false,
    }
}
/// The teardown a FORWARD navigation off `cur` carries — [`stays_on_trail`] and [`leave_of`]
/// composed, so the two halves of the rule are stated once and cannot drift apart at the two call
/// sites (`nav_to` and `nav_open`).
fn forward_leave(cur: Route) -> Option<fn()> {
    if stays_on_trail(cur) {
        None
    } else {
        leave_of(cur)
    }
}

// ---- the screens, the transitions, and the playback rituals -----------------------------------
//
// Declared here rather than in `plex_run`'s body, where they were until now. Each is an item — an
// `fn`, a `struct`, an `enum`, a `const`, a `static` — and an item cannot capture, so every one
// already took what it reads from the loop as an argument. The move therefore changed no signature.
//
// The loop still owns the VALUES: `route`, `trail`, `nav_pending`, the HUD cursor and every
// input-state local are `plex_run` locals, handed in by reference wherever a helper writes one.
//
// They are NOT `pub`, and that is deliberate. `lib.rs` declares `mod app` private and nothing here
// is exported, so `Route` cannot be named from `ui/` — the boundary [`node_route`] above exists to
// bridge; see its doc, which describes the trail as deciding nothing about screens and unable to
// see a `Route`. `Nav`, `NavReq` and `Modal` sit behind the same wall.

// ---- boot, and the loop's own between-frame state ---------------------------------------------
/// Which screen the boot gate landed on — see the gate itself in `plex_run`, which is where the
/// order of its four cases is argued.
enum BootTo {
    Home,
    Login,
    Profiles,
}
/// WHICH key the remote is holding down, as one value: the sym the client-side repeat timer
/// is driving, the two instants that timer reads, the hardware heartbeat that catches a
/// dropped key-up, and the sym we watched go physically down.
///
/// They are bundled because the two per-frame rules at the bottom of the loop each read
/// three of the five together — the lost-keyup net tests `sym`, `since` and `alive`, and the
/// repeat itself tests `sym`, `since` and `last_rep` — while every arm that arms a hold
/// writes the same three fields in the same order.
struct HeldKey {
    sym: u32,      // the key the client-side repeat is driving; 0 = nothing held
    since: u32,    // when it was armed — the repeat's initial delay is measured from here
    last_rep: u32, // when that repeat last fired
    alive: u32,    // last hardware 0x101 for the held key — a lost-keyup liveness net
    /// The sym we believe is PHYSICALLY DOWN right now — set by a fresh key-down, cleared by
    /// its key-up. It exists to tell a real hardware auto-repeat from a PHANTOM one, which
    /// this TV emits routinely and which the repeat guard below would otherwise swallow.
    ///
    /// Device-measured 2026-08-15, over the system keyboard: the panel does not deliver a
    /// key-up for the press that raised it (`RETURN` down at t=326491 with no up until the
    /// panel's own session ends), so LG's key driver still believes OK is held and stamps
    /// the NEXT press with `state & 0x100`. The guard read that as a repeat and dropped it,
    /// so the first OK after every keyboard session did nothing and the user pressed twice —
    /// reported as "I have to click the search field twice for the keyboard to appear" and
    /// "Enter twice dismisses it". Both are this one field. A repeat for a key we never saw
    /// pressed is not a repeat.
    down_sym: u32,
}
impl HeldKey {
    /// Nothing held, no hold-repeat pending — where the loop starts.
    const IDLE: HeldKey = HeldKey { sym: 0, since: 0, last_rep: 0, alive: 0, down_sym: 0 };
    /// Arm the client-side hold-repeat for `sym` at `now` — the trio every fresh-press arm
    /// writes together. `alive` and `down_sym` are the hardware's own bookkeeping and are
    /// deliberately untouched here: `alive` is stamped by the 0x101 repeat arm, `down_sym`
    /// by the key-down and key-up edges.
    fn arm(&mut self, sym: u32, now: u32) {
        self.sym = sym;
        self.since = now;
        self.last_rep = now;
    }
}
/// Scrub-seek gesture state. This Magic Remote emits a HELD key as auto-repeat keydowns
/// (state 0x101, ~50ms apart) followed by ONE keyup on release; a TAP is a lone
/// keydown(0x001)+keyup(0x000). So: a fresh press does the fixed jump; the 0x101 repeats
/// engage the continuous scrub; the keyup is a reliable release. Taps commit on a short
/// debounce so quick taps accumulate.
///
/// The preview POSITION is not here — it lives in `player::TX` behind `scrub()`/`set_scrub`,
/// because the draw path reads it too.
struct Scrub {
    t: u32,          // last continuous-advance tick
    dir: i32,        // -1 back / +1 forward / 0 = no scrub in progress
    hold: bool,      // a 0x101 repeat arrived → continuous accelerating scrub engaged
    hold_since: u32, // when that hold engaged — the acceleration ramp is measured from here
    alive: u32,      // last held (0x101) event — for the lost-keyup safety commit
    commit_at: u32,  // tap released → commit at this tick (0 = none; a new press cancels)
}
impl Scrub {
    /// No scrub in progress and no tap commit pending — where the loop starts.
    const IDLE: Scrub =
        Scrub { t: 0, dir: 0, hold: false, hold_since: 0, alive: 0, commit_at: 0 };
    /// End the gesture: no direction, no continuous hold. `commit_at` is deliberately NOT
    /// cleared — four of the five call sites leave a pending tap commit alone, and the fifth
    /// IS that commit and clears the field itself right after calling this.
    fn disengage(&mut self) {
        self.dir = 0;
        self.hold = false;
    }
}
/// The player HUD's focus cursor: WHICH row owns focus, plus the index WITHIN each of the
/// two indexed rows. One cursor, not three settings — the three are drawn together every
/// frame (`draw_hud`), moved together by UP/DOWN, and, the reason they are bundled here,
/// must be RESET together when a new playback session begins.
///
/// As three loose `plex_run` locals they were never reset at all: `start_playback` sets the
/// route, the resume point and the HUD timer, but the focus cursor survived from the
/// PREVIOUS session — leave one movie with the Subtitles button focused (`focus == 1`),
/// start another, and the first OK opened the track menu instead of pausing. Bundling makes
/// "reset the HUD focus" one assignment that `start_playback` cannot half-do.
#[derive(Clone, Copy)]
struct HudNav {
    focus: i32, // 0 = scrubber, 1 = right buttons (Subtitles/Audio/More), 2 = bottom tabs
    btn: i32,   // 0 = Subtitles, 1 = Audio, 2 = More (within the buttons row)
    tab: i32,   // 0 = Info, 1 = Chapters (within the tabs row)
}
impl HudNav {
    /// Focus parked on the scrubber, both indexed rows on their first item — where a fresh
    /// session starts and where an auto-hidden HUD is re-parked.
    const HOME: HudNav = HudNav { focus: 0, btn: 0, tab: 0 };
}
/// Everything the loop remembers ABOUT the transport HUD between frames: where its focus
/// cursor is parked, whether the user dismissed it, and the two control-row edges the
/// per-frame block near the bottom of the loop compares against this frame's slot.
///
/// The cursor keeps its own type ([`HudNav`]) rather than dissolving into fields here: the
/// helpers below take it as `&mut HudNav` — `grep 'hud_nav: &mut HudNav'` for the list, which
/// this doc used to carry as a count and which grew the moment the key ladder's arms became
/// functions — and one of them (`start_playback`) is where the per-session reset happens.
///
/// Named `HudState` and not `Hud` so it does not read as `focusprobe::Hud`, which is that
/// module's own snapshot of the cursor plus a computed `visible`, built beside this one at
/// the tail of the loop.
struct HudState {
    /// the focus cursor, reset per session by `start_playback`
    nav: HudNav,
    /// UP-from-the-top explicitly dismisses the HUD even while paused; any other player
    /// input clears it. Without this, paused() would force the HUD permanently visible.
    dismissed: bool,
    /// The last SEGMENT the control row offered. Sticky: it is never cleared back to None,
    /// so each segment raises the HUD exactly once per playback however often the row
    /// flickers.
    last_offer: Option<(crate::metadata::MarkerKind, i64)>,
    /// Did a stand-in own the control row last frame? The reset below is the EDGE of a
    /// stand-in vanishing under the focus ring — see `player_hud::standin_left_the_ring`,
    /// which is where that rule is written down and tested.
    was_standin: bool,
}
impl HudState {
    /// Focus at rest, nothing dismissed, no segment seen yet, discs in the control row.
    const IDLE: HudState =
        HudState { nav: HudNav::HOME, dismissed: false, last_offer: None, was_standin: false };
}
// scrub tuning: a press jumps SCRUB_STEP_NS; holding engages a continuous scrub ramping
// SCRUB_BASE→SCRUB_MAX (playback-seconds per real-second).
const SCRUB_STEP_NS: i64 = 10_000_000_000; // 10s per press
const SCRUB_BASE: f32 = 10.0;
const SCRUB_ACCEL: f32 = 45.0; // added per second of hold
const SCRUB_MAX: f32 = 140.0;
// tap released → commit after this (further taps accumulate). Long enough that a rapid
// ±10s tap burst coalesces into ONE seek — each separate commit is a full reopen+prime on
// the engine, and back-to-back in-flight seeks are what race the demux (the stale-audio
// silence incident); short enough that a single tap still feels immediate.
const TAP_COMMIT_MS: u32 = 450;
const SCRUB_LOST_MS: u32 = 400; // holding but no repeat this long → lost keyup → commit
// HUD auto-hide: how long the HUD lingers after the input that raised it.
const HUD_LINGER_MS: u32 = 4500; // plain transport/nav input
const HUD_MENU_MS: u32 = 8000; // a modal menu is up (track/chapter nav) — longer read time
const HUD_HEADLESS_MS: u32 = 60_000; // autoplay/headless runs pin the HUD up for capture
/// The Magic Remote POINTER, as one value: which input mode the remote is in, what the
/// cursor is doing, and the three gestures that outlive a single event (a scrub drag, a
/// wheel debounce, a click held on the hero chevron).
///
/// `dpad_mode`/`cur_hidden`/`mot_accum` are one rule between them and are why this is a
/// type: the first D-pad press hides the cursor and switches modes, and motion only switches
/// back once it has accumulated past the gate (see `remote_synth_ptr`, which has to defeat
/// that gate to click at all).
struct Pointer {
    dpad_mode: bool,   // D-pad input owns focus; pointer motion below the gate is ignored
    cur_hidden: bool,  // the LG cursor is hidden right now
    mot_accum: f32,    // motion accumulated since D-pad mode was entered, in logical px
    prev_mx: f32,      // last motion's position, for that accumulation (-1 = none yet)
    prev_my: f32,
    last_motion: u32,  // last motion tick — playback hides an idle cursor off this
    drag: bool,        // a click is dragging the HUD scrub band
    last_wheel: u32,   // last wheel tick, for the wheel's own debounce
    /// hero click-hold pager: set when a click lands on the chevron, cleared on button-up;
    /// the per-frame pump keeps paging while it stays held (the pointer twin of holding
    /// RIGHT). 0 = no click held.
    hold_pager: u32,
}
impl Pointer {
    /// Pointer mode, cursor shown, nothing held or dragging — where the loop starts.
    const IDLE: Pointer = Pointer {
        dpad_mode: false,
        cur_hidden: false,
        mot_accum: 0.0,
        prev_mx: -1.0,
        prev_my: -1.0,
        last_motion: 0,
        drag: false,
        last_wheel: 0,
        hold_pager: 0,
    };
}

// ---- the modal overlay: which panel owns the frame, and what its rows do ----------------------
/// Which panel owns the frame — the ONE place that decision lives, read by the pointer
/// arm (and, when the z bands land, the draw composition) so they cannot drift. The key
/// path was always modal for every overlay (each arm `continue`s); the CLICK path used to
/// special-case only Menu, so a click with the Info card up fell through onto the
/// partly-hidden transport's compile-time rects and started a blind scrub-seek.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Modal {
    None,
    Account,
    ItemMenu,
    Menu,
    Info,
    Chapters,
    More,
}
fn modal_of(r: Route) -> Modal {
    match r {
        Route::Account => Modal::Account,
        Route::ItemMenu { .. } => Modal::ItemMenu,
        Route::Player { overlay: Overlay::Menu } => Modal::Menu,
        Route::Player { overlay: Overlay::Info } => Modal::Info,
        Route::Player { overlay: Overlay::Chapters } => Modal::Chapters,
        Route::Player { overlay: Overlay::More } => Modal::More,
        _ => Modal::None,
    }
}
/// Perform what the `…` popover reported. Shared by the OK key and the pointer click, so
/// the two paths can never come to disagree about what a row does.
fn apply_more_action(a: crate::ui::more_menu::Action) {
    match a {
        crate::ui::more_menu::Action::ToggleStats => crate::ui::stats::toggle(),
        crate::ui::more_menu::Action::None => {}
    }
}

// ---- a route change asked for: the request, and the calls that queue or withdraw one ----------
/// A route change the user has ASKED for but which has not been applied yet: the page is
/// fading out (`ui::nav`) and the fader's commit frame applies it. A TYPED value rather than
/// a boxed closure, and the newest simply overwrites the one before it — the shape
/// `library.rs`'s `Pending` already argues for (a fast double press must commit ONCE, to the
/// last thing pressed).
///
/// It carries every ARGUMENT the destination's entry point takes, because both halves of a
/// route change now happen at the fade floor, not just the flip. That is why the two
/// stacking arms hold a `Node`: a trail node already IS "everything needed to put this page
/// on screen without the screen that asked for it", so one type serves the push and the
/// mount, and `enter_node` is the one ritual for both directions.
#[derive(Clone)]
enum Nav {
    /// Home. `focus_pill` is the tab pill that held FOCUS on the way out, carried across so
    /// the pill the user is standing on is still the one under focus when Home takes over —
    /// which is a different question from the pill Home SELECTS ([`Nav::select_pill`],
    /// always the Home pill). One word for both is why they were named apart: on the way
    /// back from the Library the selection moves to Home while focus stays on `Movies`.
    Home { focus_pill: Option<usize> },
    /// The Library browse grid on TAB `tab` (0-based, Home excluded — the strip prepends
    /// it). A tab, not a section: the strip is a projection of the section table
    /// (`browse::tabs`), so several libraries can share one pill and `browse::tab_section`
    /// is what resolves this to the library that opens.
    Library(usize),
    /// A page that STACKS — a detail page or a person page. The [`Node`] is BOTH what
    /// mounts at the floor (through the very `enter_node` a BACK pop uses, whose re-open
    /// guard means "the page you asked for is already the one loaded" costs nothing) and
    /// what is then pushed onto the trail. `season` is the one mount a node cannot express:
    /// a SHOW opened with one season already selected, which a node has no field for
    /// because a trail node names a PAGE, not a tab inside one.
    Open { node: Node, season: Option<c_int> },
    /// The Search screen — a RETURN to it, which is why it carries nothing.
    ///
    /// It used to hold a `query: String` to seed the field with, and every one of the four
    /// interactive entries passed `String::new()`: the pill wiped the term the user was
    /// still reading, the shelves under it and both cursors, on a screen whose BACK-trail
    /// re-entry (`Node::Search`) deliberately preserves all three. The seed's only real
    /// caller was never this enum at all — `/tmp/plxnative-search=<q>` mounts through
    /// `search::enter` directly, with no transition to carry a payload — so the field
    /// existed to be empty. `search::resume` is what the commit arm calls now.
    Search,
    /// BACK off a stacking page: pop the trail at the floor and re-enter what was under it.
    /// The destination is deliberately NOT spelled out here — `enter_node` handles every
    /// node, and re-deriving it at the press would mean peeking a trail the pop re-reads
    /// anyway. `bar` is the one thing the PRESS frame has to know before the pop happens:
    /// whether the page underneath wears the shared top bar.
    Back { bar: bool },
}
impl Nav {
    /// The pill this destination SELECTS — what the shared tab row must read from the press
    /// frame on (`ui::nav::view_tab`). Not to be confused with `Nav::Home`'s `focus_pill`,
    /// which is where the remote's focus LANDS: arriving at Home always selects the Home
    /// pill (0), whatever pill the user was standing on when they left. `None` = leave the
    /// row to whatever screen owns it, which is right both for a destination that has no bar
    /// at all and for a BACK, where the page being restored answers for its own chrome
    /// (`library::view_section`) the moment it is mounted.
    fn select_pill(&self) -> Option<usize> {
        match self {
            Nav::Home { .. } => Some(crate::ui::widgets::pill_of(Pill::Home)),
            // a TAB index, not a section index: the strip is a projection of the table
            // (`browse::tabs`). Placed through `pill_of` rather than by a `+1` here —
            // where the section pills start in the row is the strip's business, not this
            // enum's, and the two must agree with what a CLICK on that pill resolves to.
            Nav::Library(tab) => Some(crate::ui::widgets::pill_of(Pill::Section(*tab))),
            Nav::Search => Some(crate::ui::widgets::pill_of(Pill::Search)),
            Nav::Open { .. } | Nav::Back { .. } => None,
        }
    }
    /// Does the destination draw the shared top bar? Written as a `match` and not a
    /// `matches!` on purpose: a new destination is then a COMPILE ERROR here rather than a
    /// silent `false`, and a silent `false` is a bar that blinks out and back for no reason.
    fn wears_tab_bar(&self) -> bool {
        match self {
            Nav::Home { .. } | Nav::Library(_) | Nav::Search => true,
            // Detail and Person wear no bar today — but the NODE is the destination and can
            // answer for itself, so ask it rather than hard-coding the answer a new stacking
            // page would silently inherit.
            Nav::Open { node, .. } => node_wears_tab_bar(node),
            Nav::Back { bar } => *bar,
        }
    }
}
/// A queued [`Nav`] plus the route it was queued FROM. The `from` is the whole supersede
/// rule: a route change from any OTHER source (an async play resolve, the app-switch
/// lifecycle, a login landing) has moved the app somewhere the user can see, and a stale
/// request must not flip the screen out from under it. One equality test at the commit
/// covers every such site without any of them having to know this exists.
#[derive(Clone)]
struct NavReq {
    to: Nav,
    from: Route,
    /// Where the page being LEFT was standing, snapshotted at the PRESS (`detail::spot`'s
    /// own contract) and written onto its trail node at the floor. Carried rather than
    /// re-read at the commit because the user can still move focus during the 70 ms, and
    /// BACK must return them to where they pressed, not to where the fade found them.
    spot: Option<Spot>,
}
/// Where the page being left is standing, for [`NavReq::spot`]. Only a detail page has a
/// place worth restoring (`Trail::set_top_spot` ignores every other node), so this is the
/// whole rule — no per-arm decision, and no call site that can forget it. On a BACK the
/// node it is recorded onto is the one about to be popped, so the write is simply spent;
/// that costs one struct copy and buys the rule its uniformity.
fn leaving_spot(cur: Route) -> Option<Spot> {
    matches!(page_of(cur), Route::Detail).then(crate::ui::detail::spot)
}
/// Ask for `to`, through the page cross-fade, carrying the outgoing page's teardown.
///
/// **Both halves of a route change land at the floor**: the outgoing page's teardown and
/// the incoming page's mount. That uniformity is the design — the alternative is a per-arm
/// judgement about which stores the screen still on screen happens to read, and the arm
/// that gets it wrong blanks a page in the middle of its own fade. It costs the ~70 ms of
/// `OUT_MS` before a detail fetch is issued, which the fade is spending anyway and the
/// page's own spinner already covers.
fn nav_req(cur: Route, to: Nav, leave: Option<fn()>, pending: &mut Option<NavReq>) {
    crate::ui::nav::begin(route_wears_tab_bar(cur) && to.wears_tab_bar(), to.select_pill(), leave);
    *pending = Some(NavReq { to, from: cur, spot: leaving_spot(cur) });
}
/// A FORWARD navigation. It carries a teardown only when the page it leaves is NOT one the
/// BACK trail can put back — see [`stays_on_trail`], which is where that rule and its two
/// wrong generalisations are argued.
fn nav_to(cur: Route, to: Nav, pending: &mut Option<NavReq>) {
    nav_req(cur, to, forward_leave(cur), pending);
}
/// Open a stacking page (detail / person) through the transition — the ONE forward entry to
/// both, so a new way in cannot push without routing or route without pushing. The mount
/// and the push both happen at the fade floor; see [`nav_req`].
fn nav_open(cur: Route, node: Node, season: Option<c_int>, pending: &mut Option<NavReq>) {
    nav_req(cur, Nav::Open { node, season }, forward_leave(cur), pending);
}
/// BACK off a stacking page, through the transition. The page IS being left for good, so
/// its teardown rides the request; the trail is only PEEKED here (`Trail::under`) and the
/// pop itself happens at the floor, so a second BACK inside the window withdraws this one
/// instead of popping a page that is still on screen.
fn nav_back(cur: Route, trail: &Trail, pending: &mut Option<NavReq>) {
    let bar = trail.under().map(node_wears_tab_bar).unwrap_or(false);
    nav_req(cur, Nav::Back { bar }, leave_of(cur), pending);
}
/// Withdraw a queued transition — but only one that is still THIS screen's to withdraw.
/// Returns whether there was one, so an input that cancelled NOTHING falls through to its
/// normal handling instead of being swallowed.
///
/// The `from == cur` test is the same supersede rule the commit applies, moved earlier: a
/// request whose origin route is no longer the one mounted is already dead (the commit will
/// drop it), so withdrawing it must not consume a press meant for the screen the user is
/// actually on. Without it a BACK could be spent un-asking an invisible transition instead
/// of leaving the player.
fn nav_cancel(cur: Route, pending: &mut Option<NavReq>) -> bool {
    if pending.as_ref().map(|r| r.from != cur).unwrap_or(true) {
        return false;
    }
    let did = crate::ui::nav::cancel();
    if did {
        *pending = None;
    }
    did
}

// ---- navigation targets, page entry, and the playback rituals ---------------------------------
/// A forward navigation to `rk`'s detail page, as a [`Nav`] destination. The ONE builder,
/// so the six ways in cannot drift in what they push: the node carries an EMPTY spot, which
/// is filled in only if the user later navigates deeper off the page (`Trail::set_top_spot`).
fn to_detail(sid: crate::plex::ServerId, rk: &str) -> Node {
    Node::Detail { sid, rk: rk.to_string(), spot: Spot::default() }
}

/// Open the focused Library card's detail page — the ONE library-card activation
/// (OK-press commit AND pointer click). Library cards are movies/shows, so activation is
/// always the detail page (playback then starts from there).
fn open_library_card(cur: Route, nav: &mut Option<NavReq>) {
    let Some(mm) = crate::ui::library::focused_item() else { return };
    if mm.rk.is_empty() {
        return;
    }
    nav_open(cur, to_detail(mm.sid, &mm.rk), None, nav);
}

/// Open the focused person-page shelf card's detail page — the ONE person-card activation
/// (OK-press commit AND pointer click), the twin of [`open_library_card`]. The person page
/// is left standing behind it on the trail, so BACK comes straight back to the same shelf
/// position.
fn open_person_card(cur: Route, nav: &mut Option<NavReq>) {
    let Some(mm) = crate::ui::person::focused_item() else { return };
    if mm.rk.is_empty() {
        return;
    }
    nav_open(cur, to_detail(mm.sid, &mm.rk), None, nav);
}

/// Enter `rk`'s detail page with a HARD CUT — no transition. The one caller left is the
/// `/tmp/plxnative-detail` boot trigger, and the reason is the same one the Library boot
/// trigger gives: at boot there is no outgoing screen to replace, so a dip would fade the
/// page up out of nothing and read as a slow app rather than a navigated one. Every
/// INTERACTIVE way in goes through [`nav_open`] instead.
fn push_detail(trail: &mut Trail, route: &mut Route, sid: crate::plex::ServerId, rk: &str) {
    trail.push(to_detail(sid, rk));
    *route = Route::Detail;
}

/// The trail bookkeeping an item-menu navigation performs on the page it is LEAVING.
///
/// Over HOME the popover is the user acting on the root, exactly as `home_activate` is, so
/// the history behind them is spent. That truncation stays on the PRESS frame while the
/// push it precedes moves to the fade floor, and the asymmetry is deliberate: Home is
/// `stack[0]`, so a reset to the root is idempotent and survives a withdrawn transition
/// unharmed, whereas a PUSH or a POP is history the user would actually lose.
///
/// Over the DETAIL page there is nothing to do here any more — where that page was standing
/// is `NavReq::spot`'s job now, recorded uniformly for every navigation off a detail page
/// rather than by this one arm remembering to.
fn menu_leave(trail: &mut Trail, host: MenuHost) {
    if matches!(host, MenuHost::Home) {
        trail.reset();
    }
}

/// Put page `n` on screen — the ONE entry, shared by every BACK pop AND by every forward
/// navigation onto a stacking page ([`Nav::Open`]). Always at the fade floor.
///
/// Each arm is `person::leave`'s old rule generalized: **re-open only if the page behind is
/// not still the one loaded.** That is what makes the common case free (a detail page opened
/// on top of a person page never disturbed `person`'s store, so BACK is a route flip) and
/// the deep case correct (a page closed two levels ago is re-fetched, by rk, through the
/// same `open_rk` every other entry point uses).
///
/// The same guard is exactly right FORWARD, which is why one function serves both
/// directions: a cast-row OK has already installed the person on the press frame (nothing
/// the detail page underneath reads, so it costs the outgoing page nothing) and must not
/// re-fetch it here; `home_activate`'s play-a-show arm has already mounted the detail page
/// blocking, because deciding play-vs-open required the loaded item. In both cases the
/// honest reading of the guard — "the page you asked for is already the one loaded" — is
/// the wanted no-op.
///
/// The MOUNT is per-node; the route flip is not — it is [`node_route`], applied once at the
/// end, so this function and `node_wears_tab_bar` cannot come to disagree about what page a
/// node is. The `match` stays exhaustive for the mounts themselves.
fn enter_node(n: &Node, route: &mut Route) {
    match n {
        // Nothing to mount for either root. No `library::enter`: `browse.rs` still holds the
        // section, focus and scroll, and re-entering would re-query and lose them.
        // …and nothing for Search either, for the SAME reason and it is worth saying twice:
        // `crate::search` still holds the query and the shelves, `ui::search` still holds
        // the zone and both cursors, and `search::enter` would reset every one of them —
        // re-entering would land the user on an empty field over their own recents list
        // (which is exactly what the first version of this did).
        //
        // Not even `search::resume`, which the PILL now takes: a BACK is a return to the
        // exact spot, so the zone and the shelf scroll stay where the user left them — you
        // came back to the tile you opened. `resume` re-seats those on purpose, because a
        // pill press is an arrival at the screen rather than a return to a place in it.
        Node::Home | Node::Library | Node::Search => {}
        Node::Person { sid, key, guid, name, thumb } => {
            // "already the one loaded?" through the trail's own person-identity rule
            // (`trail::same_person`), so the guid decides when both sides have one and the
            // server-scoped local id decides otherwise. The bare `p.key != *key` this
            // replaces compared a `personId` across machines, where it means nothing.
            let same = crate::person::current()
                .map(|p| {
                    crate::ui::trail::same_person((p.sid, &p.key, &p.guid), (*sid, key, guid))
                })
                .unwrap_or(false);
            if !same {
                // `reopen`, NOT `open`: `open` raises the latch the drain below turns into a
                // route change PLUS a push, so a single BACK would land here and immediately
                // push back the node it just popped — which reads as "BACK does nothing".
                crate::ui::person::reopen(*sid, key, guid, name, thumb);
            }
        }
        Node::Detail { sid, rk, spot } => {
            // …and the same for the detail page: the pair, never the rk alone (a share's
            // item 42 and ours are different pages, and re-entering must fetch the one the
            // node names rather than deciding it is already up).
            if !crate::plex::same_item(
                (crate::ui::detail::mounted_sid(), &crate::ui::detail::mounted_rk()),
                (*sid, rk),
            ) {
                // A RESTORE carries a place to put the page back at. A forward navigation
                // carries the EMPTY spot `to_detail` builds, and must not arm a placement:
                // `open_rk_at`'s two-stage pump fires when the fetch lands, and on a fresh
                // open that would yank focus back to the hero from wherever the user had
                // moved it while waiting. The test is sound because the two branches agree
                // on the value it splits — an empty spot IS the state `open_rk` mounts in.
                if *spot == Spot::default() {
                    crate::ui::detail::open_rk(*sid, rk);
                } else {
                    crate::ui::detail::open_rk_at(*sid, rk, spot);
                }
            }
        }
    }
    *route = node_route(n);
}

/// Resume captured at the keypress, applied when the async resolve lands.
static PENDING_RESUME_NS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// The ONE start-playback ritual (detail OK, home episode OK, and the plxnative-autoplay/
/// -detailplay/-play dev triggers all share it): arm the resume point BEFORE the first
/// Load (direct-play av_seek / transcode &offset restart), start the engine, record the
/// Stop/BACK/EOS return target, reset the HUD focus cursor, and show the HUD. A missed step
/// here used to silently fork behavior between the interactive and headless paths.
fn start_playback(
    mt: &crate::task::MainThread,
    resume_ns: i64,
    from_detail: bool,
    hud_ms: u32,
    route: &mut Route,
    played_from_detail: &mut bool,
    hud_nav: &mut HudNav,
) {
    // A resolve in flight means the route statics are NOT installed yet. Applying the
    // resume now would read a stale/empty TSESSION, so `resume_at` would take its
    // DIRECT-PLAY branch and arm_seek() a transcode — and pump.rs's feed gate requires
    // `seek_to_ns < 0`, so that stray armed seek blocks feeding forever: no frames, no
    // ACB bind, timeline frozen at the resume point. (Exactly what broke
    // transcode_av1_no_dp_audio. Direct-play never noticed because arm_seek is what the
    // correct branch does anyway.) Defer it to `pump_play`, after apply_plan.
    let pending = crate::route::play_pending();
    if resume_ns > 0 && !pending {
        crate::player::resume_at(resume_ns);
    }
    // Flip to the player NOW so the HUD draws its Resolving state this frame; `pump_play`
    // below starts the engine when the plan lands. With nothing pending this is the old
    // synchronous behaviour, byte for byte.
    let entering = if pending {
        PENDING_RESUME_NS.store(resume_ns, Relaxed);
        true
    } else {
        crate::player::start_bufferfeed(mt)
    };
    if entering {
        *played_from_detail = from_detail;
        *route = Route::Player { overlay: Overlay::None };
    }
    // A NEW session starts on the scrubber. The cursor is per-session state that nothing
    // else clears: the auto-hide re-park later in the loop only runs while the route is
    // already Player, and the exit paths leave the player entirely — so leaving a movie
    // with the Subtitles button focused used to carry `focus == 1` into the next one,
    // where the first OK opened the track menu instead of pausing. Unconditional, like the
    // `set_paused`/`set_hud` below it: the HUD that is about to be drawn belongs to THIS
    // attempt either way.
    *hud_nav = HudNav::HOME;
    // Per-session: an auto-advance chain (episode → episode → …) re-enters here without
    // ever passing through `exit_player`, so the finished episode's countdown state must
    // not carry into the next one.
    crate::ui::up_next::reset();
    set_paused(false);
    // Stamp the HUD deadline HERE, from NOW — not from the keypress. Callers used to pass
    // `last_input + HUD_LINGER_MS`, a timestamp taken BEFORE the blocking resolve above, so
    // a load longer than the 4.5 s linger expired the HUD before it was ever drawn and the
    // user got a blank screen instead of a transport. Taking a duration makes that
    // unrepresentable, and keeps the headless 60 s case working.
    set_hud(unsafe { SDL_GetTicks() }.wrapping_add(hud_ms).max(1));
}

/// Resume if a seek landed while paused — the twin of `commit_seek`, which is the
/// stay-paused variant. Written out four separate times in this file before it had a name.
fn resume_if_paused(mt: &crate::task::MainThread) {
    if paused() {
        set_paused(false);
        crate::player::resume(mt);
    }
}

/// Leaving playback (Stop / BACK / EOS / Info's jump-to-detail): close every in-player
/// overlay so no stale popover OPEN flag survives into the next session — the route flip
/// alone hides them but leaves the module state set (the EOS path once forgot the menu).
fn close_player_overlays() {
    crate::ui::track_menu::close();
    crate::ui::info_panel::close();
    crate::ui::chapters_panel::close();
    crate::ui::more_menu::close();
    crate::ui::stats::close(); // a diagnostics panel must not survive into the next session
    crate::ui::up_next::cancel(); // disarm the auto-advance countdown
}

/// The ONE leave-playback ritual (Stop key, BACK, EOS): close the overlays, stop the
/// engine, return to the origin route, and arm the deferred hub refresh so Continue
/// Watching reflects the session that just ended. A new exit path that skips this quietly
/// re-introduces the stale-CW bug.
fn exit_player(
    mt: &crate::task::MainThread,
    route: &mut Route,
    played_from_detail: bool,
    refresh_hubs_at: &mut u32,
    trail: &mut Trail,
) {
    crate::route::cancel_play(); // BACK during a load: supersede, drop the landing
    close_player_overlays();
    crate::player::stop_bufferfeed(mt);
    *route = if played_from_detail { Route::Detail } else { Route::Home };
    // Returning to a detail page after an EPISODE lands on its SHOW, not on the episode.
    // The play paths load the played leaf's detail (that is where the HUD caption and Info
    // card come from), so backing out used to strand the user on an episode hero page —
    // even when they had started from the show page, and even after an auto-advance chain
    // had moved them several episodes along. `detail_rk` is already the "Go to Show" target.
    if played_from_detail {
        // The played leaf's own server — `metadata::playing()` is the store the playback
        // was resolved from, so it names the machine the show is on. `plex::current_server()`
        // would be the wrong answer for anything played off a share.
        let sid = crate::metadata::playing()
            .map(|p| p.sid)
            .unwrap_or_else(crate::plex::current_server);
        if let Some((show_rk, season)) = crate::metadata::now_playing()
            .filter(|n| n.is_episode && !n.detail_rk.is_empty())
            .map(|n| (n.detail_rk.clone(), n.season))
        {
            crate::ui::detail::open_show_at_episode(sid, &show_rk, season, &crate::route::cur_rk());
        }
        // The player is NOT a trail node — it returns to the page it was started from, and
        // that page may be one nothing ever pushed (a dev trigger, or `home_activate`
        // opening a page under the hood purely to fire its Play). Read AFTER the reveal
        // above, so an auto-advance chain names the SHOW rather than the leaf it ended on.
        trail.ensure_detail(crate::ui::detail::mounted_sid(), &crate::ui::detail::mounted_rk());
    } else {
        // …and a session that did not come from a page lands on Home, which IS the root:
        // whatever the trail was describing is behind the user now.
        trail.reset();
    }
    *refresh_hubs_at = unsafe { SDL_GetTicks() }.wrapping_add(800).max(1);
}

/// The episode is OVER — drained to EOS, or the user skipped a `final` credits marker.
/// Starts the queued episode when the show has one, else leaves the player exactly as
/// `exit_player` would. There is no interstitial: "always the next episode".
fn finish_playback(
    mt: &crate::task::MainThread,
    route: &mut Route,
    played_from_detail: &mut bool,
    refresh_hubs_at: &mut u32,
    hud_nav: &mut HudNav,
    trail: &mut Trail,
) {
    if play_up_next(mt, HUD_LINGER_MS, route, played_from_detail, hud_nav) {
        return;
    }
    exit_player(mt, route, *played_from_detail, refresh_hubs_at, trail);
    hud_nav.focus = 0;
}

/// Activate whatever occupies the control row. ONE dispatch for both the OK key and the
/// pointer — they used to hold byte-identical copies of this `match`, and had already
/// drifted (the key path cleared the held key, the pointer path did not). Returns true when
/// the route flipped, which is the only thing the two callers still handle differently.
fn activate_ctrl_row(
    mt: &crate::task::MainThread,
    slot: crate::ui::player_hud::ControlSlot,
    route: &mut Route,
    played_from_detail: &mut bool,
    refresh_hubs_at: &mut u32,
    hud_nav: &mut HudNav,
    trail: &mut Trail,
) -> bool {
    use crate::ui::player_hud::ControlSlot;
    use crate::ui::skip_pill::SkipAction;
    match slot {
        // The row's two items, off the cursor the caller already parked (a click sets it
        // from the hit-test, a key press moved it). *Next Episode* starts the successor;
        // *Watch Credits* does nothing beyond the cancel the frame block below performs
        // for it — the button exists so that "let it run" is a THING YOU CAN PRESS rather
        // than an absence, which on a countdown is the difference between choosing and
        // being caught out.
        ControlSlot::UpNext(_) => {
            if hud_nav.btn == crate::ui::up_next::BTN_NEXT {
                play_up_next(mt, HUD_LINGER_MS, route, played_from_detail, hud_nav)
            } else {
                crate::ui::up_next::cancel();
                false
            }
        }
        ControlSlot::Skip(pr) => match pr.action {
            SkipAction::Seek(ns) => {
                // Retire the segment FIRST: the seek lands on the preceding keyframe, which
                // is usually still inside it, so without this the button comes straight back
                // (see `metadata::mark_skipped`).
                crate::metadata::mark_skipped(pr.marker);
                request_seek(ns);
                resume_if_paused(mt);
                false
            }
            // a `final` credits segment: skipping it IS finishing the item
            SkipAction::Finish => {
                finish_playback(mt, route, played_from_detail, refresh_hubs_at, hud_nav, trail);
                true
            }
        },
        ControlSlot::Discs => false,
    }
}

/// Start the queued episode. Returns false when there is nothing queued (a movie, or the
/// last episode), which is the caller's cue to leave the player.
///
/// It stops the outgoing session ITSELF rather than trusting each call site to: three
/// paths reach here (EOS, Skip Credits on a `final` marker, and OK on the HUD tile while
/// the credits are still rolling) and in all three an Engine is live — `start_bufferfeed`
/// no-ops while one is, so skipping the stop would silently fail to advance. The stop is
/// also what posts the `state=stopped` timeline that commits the watched state, and it
/// must happen BEFORE `request_play_up_next`: teardown reads the outgoing item's session
/// ids and clears the URL, both of which the new plan is about to overwrite.
fn play_up_next(
    mt: &crate::task::MainThread,
    hud_ms: u32,
    route: &mut Route,
    played_from_detail: &mut bool,
    hud_nav: &mut HudNav,
) -> bool {
    // clone off the `&'static` store BEFORE anything can replace it (see up_next::take)
    let Some(u) = crate::ui::up_next::take() else { return false };
    log(&format!("up next: S{}E{} rk={} '{}'", u.season, u.index, u.rk, u.ep_title));
    let (rk, resume) = (u.rk.clone(), crate::metadata::resume_ns(u.resume_ms, u.dur_ms));
    close_player_overlays();
    crate::player::stop_bufferfeed(mt);
    crate::route::request_play_up_next(u);
    // Same ritual as `play_item_now`: retire the finished episode's descriptor so the HUD
    // caption and Info card don't label the new playback with the old one's title for the
    // whole pre-roll, and fetch the new leaf off the loop.
    // Read BEFORE `retire_playing` drops the store: the successor is a row of the queue
    // the finished episode created, so it lives on that episode's server.
    let sid = crate::metadata::playing().map(|p| p.sid).unwrap_or_else(crate::plex::current_server);
    crate::metadata::retire_playing();
    crate::metadata::request_detail(sid, &rk);
    start_playback(mt, resume, *played_from_detail, hud_ms, route, played_from_detail, hud_nav);
    true
}

/// Direct-play a LEAF catalog item (movie or episode) — the hero-pill / Continue-Watching
/// "play now" ritual: route cfg + streams metadata + the shared start ritual.
/// `from_start` ignores the item's resume point — the item menu's "Play from Start", which is
/// the ONLY difference between restarting a Continue Watching tile and resuming it. Taking it
/// as a flag (rather than a resume_ns the caller computes) keeps Plex's resume rule
/// (`metadata::resume_ns`, which also refuses to resume the last few percent) in one place.
unsafe fn play_item_now(
    mt: &crate::task::MainThread,
    mm: &crate::pms::PmsMovie,
    from_start: bool,
    hud_ms: u32,
    route: &mut Route,
    played_from_detail: &mut bool,
    hud_nav: &mut HudNav,
) {
    if mm.rk.is_empty() {
        return;
    }
    crate::route::request_play_movie(mm); // resolve OFF the SDL loop — pump_play starts it
    // Fetch OFF the loop too — pump_detail lands it. Nothing here reads current(): every
    // start_playback argument comes from `mm` (the catalog row), and the in-player track
    // menu reads metadata::playing(), which the resolve worker installs. The one consumer
    // is sync_now_playing()'s descriptor for the HUD caption and Info card, so a landing a
    // beat later costs a few frames of missing caption, never a wrong play.
    // Retire the old descriptor first: it describes the PREVIOUSLY played item, and the
    // HUD caption + Info card read it every frame — leaving it up would label this
    // playback with the last one's title for the whole pre-roll. None is honest (the
    // route's own TITLE/CTXLINE, set synchronously by request_play_movie, still carry
    // this item), and the landing refills it via sync_now_playing.
    crate::metadata::set_now_playing(None);
    crate::metadata::request_detail(mm.sid, &mm.rk);
    start_playback(
        mt,
        if from_start { 0 } else { crate::metadata::resume_ns(mm.resume_ms, mm.dur_ns / 1_000_000) },
        false,
        hud_ms,
        route,
        played_from_detail,
        hud_nav,
    );
}

/// The ONE home activation (OK key AND pointer click): `hf` is the hero action-row focus
/// (-1 chip / 0 pill / 1 info / 2 chevron) in hero view, `i32::MIN` for a grid card.
/// Pill / Continue-Watching tiles / episodes launch playback immediately (a show or season
/// opens its page under the hood and fires its Play, which resolves the right episode +
/// resume); the info circle and ordinary grid cards open the detail page.
unsafe fn home_activate(
    mt: &crate::task::MainThread,
    hf: c_int,
    hud_ms: u32,
    route: &mut Route,
    played_from_detail: &mut bool,
    trail: &mut Trail,
    hud_nav: &mut HudNav,
    nav: &mut Option<NavReq>,
) {
    // every Home-originated activation clears the return trail HERE (it was hand-reset at
    // each call site before — a set-a-flag-in-N-places smell). Home is the trail's ROOT, so
    // acting on it means everything that was behind the user is spent: a page reached from a
    // person page or from the Library is as stale as any other once they are back on Home.
    trail.reset();
    // A Home with no shelves is the loading/empty/error read-out, whose only control is
    // Retry — it takes the press unless it was the top band (chip / tab pills), which
    // stay usable precisely because they are the escapes from an empty Home.
    // NB the trail is truncated ABOVE this early return: a Retry press is still the user
    // acting on Home, so a stale trail must not survive it.
    if crate::ui::home::status_activate(hf) {
        return;
    }
    let hero_view = hf != c_int::MIN;
    if hf == -1 {
        crate::ui::account_menu::open();
        *route = Route::Account;
        return;
    }
    // a tab pill in the top band. (The grid-card sentinel is rejected by hero_pill_index
    // itself — see its doc comment.)
    if let Some(pill) = crate::ui::home::hero_pill_index(hf) {
        match crate::ui::widgets::pill_at(pill) {
            Pill::Search => nav_to(*route, Nav::Search, nav),
            // that section's grid, through the page cross-fade: `library::enter` and the
            // route flip both land at the fade floor, while the selection capsule starts
            // travelling on THIS frame (`nav::view_tab`).
            Pill::Section(tab) => nav_to(*route, Nav::Library(tab), nav),
            // Home is the screen we are on, so OK on its pill is a deliberate no-op —
            // EXCEPT that it withdraws a section switch that is still fading out: the user
            // changed their mind inside the 70 ms window, and the capsule springs back on
            // its own.
            Pill::Home => {
                nav_cancel(*route, nav);
            }
        }
        return;
    }
    if hf == 2 {
        crate::ui::home::hero_flip(1);
        return;
    }
    let m = if hero_view {
        crate::ui::home::hero_item()
    } else {
        crate::ui::home::movie_at(crate::ui::home::row(), crate::ui::home::col())
    };
    let Some(mm) = m else { return };
    let rk = mm.rk.clone();
    if rk.is_empty() {
        return;
    }
    let want_play = hf == 0
        || (!hero_view
            && (crate::pms::hub_is_continue(crate::ui::home::row().max(0) as usize) || mm.kind == 3));
    if want_play {
        match mm.kind {
            0 | 3 => play_item_now(mt, mm, false, hud_ms, route, played_from_detail, hud_nav),
            _ => {
                // show / season: open its page (blocking) and fire its Play — but only
                // once the load actually landed on the expected item (a failed fetch
                // leaves the PREVIOUS detail in place; blindly firing on_ok would play
                // whatever page was open before).
                let expect = if mm.kind == 2 { mm.show_rk.clone() } else { rk.clone() };
                // a show/season row's parent lives on the SAME server as the row itself
                let sid = mm.sid;
                if mm.kind == 2 {
                    crate::ui::detail::open_rk_season(sid, &expect, mm.season_index);
                } else {
                    crate::ui::detail::open_rk_now(sid, &expect); // BLOCKING: `loaded` below gates the play
                }
                let loaded = crate::metadata::current()
                    .map(|d| crate::plex::same_item((d.sid, &d.rk), (sid, &expect)))
                    .unwrap_or(false);
                if loaded && crate::ui::detail::on_ok() {
                    start_playback(mt, crate::ui::detail::last_resume_ns(), false, hud_ms, route, played_from_detail, hud_nav);
                } else {
                    // nothing playable / load failed — land on the page, through the
                    // transition. `season: None`: the mount already happened above (this
                    // arm has to read the loaded item to decide at all), and `enter_node`'s
                    // re-open guard is what turns the floor's mount into a route flip.
                    nav_open(*route, to_detail(sid, &expect), None, nav);
                }
            }
        }
    } else if mm.kind == 2 {
        // season: open the SHOW page with that season selected
        nav_open(*route, to_detail(mm.sid, &mm.show_rk), Some(mm.season_index), nav);
    } else if mm.kind == 3 {
        // an episode's page is its show's page — landed on the EPISODE'S season, so the
        // item the hero/tile advertised is actually in view (mirrors the season arm)
        nav_open(*route, to_detail(mm.sid, &mm.show_rk), (mm.season_index > 0).then_some(mm.season_index), nav);
    } else {
        nav_open(*route, to_detail(mm.sid, &rk), None, nav);
    }
}

/// Open the item context menu on the focused HOME GRID card — the press-and-hold half of the
/// Continue Watching interaction (a SHORT press still plays/opens immediately; see
/// `home_activate`). Reports whether it opened, so the caller only flips the route when a
/// menu is actually up: the hero view has no card, and a shelf can be empty.
fn open_item_menu(route: &mut Route) -> bool {
    let Some(m) = crate::ui::home::movie_at(crate::ui::home::row(), crate::ui::home::col()) else {
        return false;
    };
    if !crate::ui::item_menu::has_actions(m) {
        return false;
    }
    // the Remove-from-deck row only exists on a Continue Watching card — nothing else has a
    // deck to be removed from (see `item_menu::build`)
    let from_deck = crate::pms::hub_is_continue(crate::ui::home::row() as usize);
    crate::ui::item_menu::open(m, from_deck, crate::ui::home::focused_card_rect());
    *route = Route::ItemMenu { over: MenuHost::Home };
    true
}

/// The same popover on the DETAIL page's episode filmstrip — the owner-reported gap: a long
/// press on an episode still did nothing, so there was nowhere to mark an episode watched.
/// Reports whether it opened, so the caller only flips the route when a menu is actually up:
/// the filmstrip may not hold focus, and a season fetch in flight makes the row's contents a
/// lie (see `detail::focused_episode`).
fn open_episode_menu(route: &mut Route) -> bool {
    let Some((rk, watched)) = crate::ui::detail::focused_episode() else {
        return false;
    };
    if rk.is_empty() {
        return false;
    }
    crate::ui::item_menu::open_episode(
        crate::ui::detail::mounted_sid(),
        &rk,
        watched,
        crate::ui::detail::focused_episode_rect(),
    );
    *route = Route::ItemMenu { over: MenuHost::Detail };
    true
}

/// Perform an item-menu [`Action`](crate::ui::item_menu::Action) — the ONE dispatch shared by
/// the OK key and the pointer click, exactly like `home_activate` and `activate_ctrl_row`
/// (the two paths for the profile menu had already drifted before those were unified).
/// The menu itself only reports the choice; every route flip, server call and refresh is here.
///
/// `host` is the screen the popover was over, and it genuinely changes what an action MEANS:
/// on Home the item is a catalog row, so a play resolves through `pms::index_of_rk` and a
/// scrobble only has to refresh the hubs; on the detail page the item is an episode of the
/// loaded season, which the hub catalog usually does not contain at all, and the page itself
/// has to re-read the state that just changed.
unsafe fn apply_item_action(
    mt: &crate::task::MainThread,
    act: crate::ui::item_menu::Action,
    host: MenuHost,
    route: &mut Route,
    played_from_detail: &mut bool,
    trail: &mut Trail,
    hud_nav: &mut HudNav,
    nav: &mut Option<NavReq>,
) {
    use crate::ui::item_menu::Action;
    // WHICH SERVER this menu's rows are about — captured when the popover opened, from the
    // row it was opened on (`item_menu::SID`). Every arm below turns an rk into a fetch, a
    // scrobble or a play, and resolving one against `plex::current_server()` is the reported
    // bug itself: on a merged Continue Watching shelf, Play from Start on a friend's episode
    // found OUR row with the same key and played a different film under the friend's title.
    let sid = crate::ui::item_menu::item_sid();
    // Every arm below turns an rk into a blocking fetch or a play; an empty one would fetch
    // nothing and land on a blank page. `build` already refuses to offer such a row — this
    // is the belt to that braces, since the menu is data-driven off the hub rows.
    let rk_of = |a: &Action| match a {
        Action::GoToItem(rk)
        | Action::MarkWatched(rk, _)
        | Action::PlayFromStart(rk)
        | Action::RemoveFromDeck(rk) => rk.clone(),
        Action::GoToShow(rk, _) => rk.clone(),
        Action::None => String::new(),
    };
    if !matches!(act, Action::None) && rk_of(&act).is_empty() {
        return;
    }
    match act {
        Action::None => {}
        Action::GoToItem(rk) => {
            menu_leave(trail, host);
            nav_open(*route, to_detail(sid, &rk), None, nav);
        }
        Action::GoToShow(show_rk, season) => {
            menu_leave(trail, host);
            // the season arm is BLOCKING (it indexes the loaded show's seasons) — the same
            // trade `home_activate` makes for a season tile, now paid at the fade floor
            // where the stall is behind a screen that is already at alpha 0
            nav_open(*route, to_detail(sid, &show_rk), (season > 0).then_some(season), nav);
        }
        Action::MarkWatched(rk, watched) => {
            // Same ritual as the detail page's watched toggle, and now the same CODE: flip
            // every surface that describes the item at once, write on a worker, refetch the
            // hubs when the write lands so Continue Watching reflects it (a watched episode
            // leaves the shelf; its successor takes the slot).
            //
            // All three used to run inline, on this thread, justified as "~100ms LAN and
            // deliberately so". That priced one server on one LAN; with a share registered
            // the item's server is routinely remote or asleep, and the same press parked the
            // whole UI for seconds — see `crate::viewstate`, which is where the reasoning,
            // the ordering rules and the `client_for(sid)`-never-`client()` note now live.
            //
            // When the popover was over the DETAIL page, that page is the surface the user is
            // watching, so it is re-read too — the rk rides along so the filmstrip lands back
            // on the episode that changed (`detail::KEEP_EP`).
            let w = if watched { crate::viewstate::Write::Unwatched } else { crate::viewstate::Write::Watched };
            let detail = matches!(host, MenuHost::Detail).then(|| rk.clone());
            crate::viewstate::request(sid, &rk, w, detail);
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
            crate::viewstate::request(sid, &rk, crate::viewstate::Write::RemoveFromDeck, None);
        }
        Action::PlayFromStart(rk) => {
            // On the detail page the target is an episode of the LOADED SEASON, which the hub
            // catalog usually doesn't hold at all (only the one Continue Watching is showing
            // ever does) — so it plays through the page's own episode path, the same one OK
            // on the still uses, with the resume dropped.
            if matches!(host, MenuHost::Detail) {
                if crate::ui::detail::play_episode_rk_from_start(&rk) {
                    let resume = crate::ui::detail::last_resume_ns();
                    start_playback(mt, resume, true, HUD_LINGER_MS, route, played_from_detail, hud_nav);
                }
                return;
            }
            // Re-resolve the catalog row by rk rather than holding a borrow across the menu:
            // a hub refetch can rebuild the catalog while the popover is open.
            let i = crate::pms::index_of_rk(sid, &rk);
            if let Some(mm) = (i >= 0).then(|| crate::pms::movie(i as usize)).flatten() {
                play_item_now(mt, mm, true, HUD_LINGER_MS, route, played_from_detail, hud_nav);
            }
        }
    }
}

// ---- the key ladder: one function per arm ------------------------------------------------------
//
// The run loop's key handler is a LADDER: a key-up, a hardware auto-repeat and the preamble every
// fresh press runs; then ten route-scoped arms that each `continue`; then one chained `else if` on
// key identity. Each arm's BODY is a function here, in the order the ladder tries them — bar three
// with no body to name (the pointer-hidden arm is empty, Stop is one call to the exit ritual, and
// Search's body IS `search::key`; see the note at its guard).
//
// Every guard, every `continue` and the order itself stay at the CALL SITE, because the order is
// part of the behaviour: an earlier guard subsumes later ones it overlaps with — `key_player_failed`
// does, on purpose — and that is only legible while the tests sit in one list, in order, in one
// place.
//
// No host test executes any of this: it runs inside the SDL event loop. The gate over it is
// `tools/keytable.py`, which drives the simulator through (screen x key) and diffs the focus
// fingerprint each press produces against a recorded table.

/// A key-up: the reliable release (this remote sends exactly one per press). Clears this sym out of
/// both held-key slots, springs a deferred grid-card press back, and ends or debounces a scrub.
///
/// `repause_at` is handed straight to [`commit_seek`] — see its doc for what it means.
unsafe fn on_key_up(
    sym: c_uint,
    isnav: bool,
    route: Route,
    ok_armed: bool,
    held: &mut HeldKey,
    scrubber: &mut Scrub,
    repause_at: &mut i64,
) {
    if sym == held.sym {
        held.sym = 0;
    }
    if sym == held.down_sym {
        held.down_sym = 0;
    }
    if is_ok(sym) && ok_armed {
        // OK released over a grid card: start the spring-back; the deferred
        // activation commits from the per-frame loop once the bounce has shown.
        crate::ui::press::release(SDL_GetTicks());
    }
    if matches!(route, Route::Player { .. }) && scrubber.dir != 0 && isnav {
        if scrubber.hold {
            log(&format!("scrub: keyup commit (held) {}s", scrub() / 1_000_000_000));
            commit_seek(scrub(), repause_at); // a held scrub → commit on release
            scrubber.disengage();
        } else {
            // a tap → commit on a short debounce so quick taps accumulate first
            scrubber.commit_at = SDL_GetTicks().wrapping_add(TAP_COMMIT_MS);
        }
    }
}

/// A hardware AUTO-REPEAT (held key): the ONLY thing it drives directly is the player's continuous
/// accelerating scrub (a ramp, not a discrete move). Every discrete focus list — home grid, detail,
/// track menu, info, chapters — repeats through the unified client-side held-key timer in the loop,
/// so hold-to-move feels identical everywhere and doesn't depend on the remote's hardware repeat
/// delay.
unsafe fn on_auto_repeat(
    sym: c_uint,
    isnav: bool,
    route: Route,
    ok_armed: bool,
    hud_nav: HudNav,
    held: &mut HeldKey,
    scrubber: &mut Scrub,
) {
    let n = SDL_GetTicks();
    if held.sym != 0 && sym == held.sym {
        held.alive = n; // heartbeat: this held key's hardware repeats are still arriving
    }
    if ok_armed && is_ok(sym) {
        crate::ui::press::note_alive(n); // OK held: keep the dropped-key-up net honest
    }
    if matches!(route, Route::Player { .. }) && hud_nav.focus == 0 && scrubber.dir != 0 && isnav {
        scrubber.alive = n;
        scrubber.commit_at = 0; // holding → not a tap
        if !scrubber.hold {
            scrubber.hold = true;
            scrubber.hold_since = n;
            scrubber.t = n;
            log("scrub: hold engaged (0x101 repeat)");
        }
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
unsafe fn begin_fresh_press(
    key: Key,
    sym: c_uint,
    held: &mut HeldKey,
    hud: &mut HudState,
    ptr: &mut Pointer,
    ok_armed: &mut bool,
) {
    held.down_sym = sym;
    hud.dismissed = false; // any fresh key un-dismisses the HUD (UP-hide re-sets it)
    // a fresh non-OK key (navigation / BACK) while a click is armed aborts the press —
    // spring the card back to rest WITHOUT activating (you "slid off" the control).
    if *ok_armed && !is_ok(sym) {
        crate::ui::press::cancel();
        *ok_armed = false;
    }
    if matches!(key, Key::Up | Key::Down | Key::Left { alt: false } | Key::Right { alt: false }) {
        if !ptr.dpad_mode || !ptr.cur_hidden {
            hide_cursor();
        }
        ptr.dpad_mode = true;
        ptr.cur_hidden = true;
        ptr.mot_accum = 0.0;
    }
}

/// Onboarding screens (login / who's-watching) own every fresh key — nothing is behind them, so
/// route the key to the active screen and skip all other handlers.
unsafe fn key_onboarding(route: Route, sym: c_uint, wcode: c_uint, ok_armed: &mut bool) {
    if matches!(route, Route::Profiles) {
        if is_ok(sym) && crate::ui::profiles::focus_is_avatar() {
            // press the roster avatar; the select commits on the spring-back
            // (route-agnostic press handler). Footer / keypad OK act immediately.
            crate::ui::press::begin(SDL_GetTicks());
            *ok_armed = true;
        } else {
            crate::ui::profiles::key(sym, wcode);
        }
    } else {
        crate::ui::login::key(sym, wcode);
    }
}

/// The Home profile menu is modal — rows nav, OK commits, BACK closes to Home.
fn key_account(sym: c_uint, wcode: c_uint, route: &mut Route) {
    if is_ok(sym) {
        match crate::ui::account_menu::on_ok() {
            crate::ui::account_menu::Action::ChangeProfile => {
                crate::auth::start_switch();
                crate::ui::profiles::enter();
                *route = Route::Profiles;
            }
            crate::ui::account_menu::Action::SignIn => {
                crate::auth::start_login();
                *route = Route::Login;
            }
            crate::ui::account_menu::Action::SignOut => {
                crate::auth::sign_out();
                *route = Route::Login;
            }
            crate::ui::account_menu::Action::None => *route = Route::Home,
        }
    } else if is_back(sym, wcode) {
        crate::ui::account_menu::close();
        *route = Route::Home;
    } else {
        crate::ui::account_menu::move_focus(sym as c_int);
    }
}

/// The press-and-hold item menu is modal too — rows nav, OK commits, BACK closes back to the shelf
/// (or filmstrip) the card is still sitting on. `over` is the screen it is a popover on.
unsafe fn key_item_menu(
    mt: &crate::task::MainThread,
    over: MenuHost,
    sym: c_uint,
    wcode: c_uint,
    now: u32,
    route: &mut Route,
    played_from_detail: &mut bool,
    trail: &mut Trail,
    hud_nav: &mut HudNav,
    nav: &mut Option<NavReq>,
    held: &mut HeldKey,
) {
    if is_ok(sym) {
        let act = crate::ui::item_menu::on_ok();
        *route = over.route(); // the dispatch overrides this when it navigates/plays
        apply_item_action(mt, act, over, route, played_from_detail, trail, hud_nav, nav);
        held.sym = 0; // an async route flip must not repeat a held key into the next screen
    } else if is_back(sym, wcode) {
        crate::ui::item_menu::close();
        *route = over.route();
    } else if sym == SDLK_UP || sym == SDLK_DOWN {
        // move once on the fresh press; holding repeats via the shared
        // client-side timer. Armed ONLY for the two keys the menu acts on, so a
        // held key it ignores can't sit in `HeldKey::sym` driving a per-frame
        // no-op.
        crate::ui::item_menu::move_focus(sym as c_int);
        held.arm(sym, now);
    }
}

/// A playback FAILURE owns the whole frame (`player_hud::transport_hidden`): `draw_hud` returns
/// before painting anything and the overlay panels below are gated the same way, so the scrubber,
/// the control row, the bottom tabs and any panel that happened to be open are all absent from the
/// picture. Nothing that is not drawn may be driven — the rule `ControlSlot::UpNext` states and
/// `up_next::card_active` already keeps for the post-play card.
///
/// BACK is the exception, and it is not optional: the read-out's own hint says "Press BACK to
/// return", and a single screen with every other key swallowed has nowhere else to go. It closes a
/// panel the failure landed on top of (so the route agrees with the frame again) and otherwise
/// leaves the player.
///
/// This arm's GUARD is why the four overlay arms after it (Menu / More / Info / Chapters) do not
/// run while the transport is hidden: it `continue`s whatever the key was. That is the ladder's
/// order doing its job, and it is what `tools/keytable.py` exists for — no host test executes any of
/// the ladder, so a reorder compiles and keeps the suite green.
fn key_player_failed(
    mt: &crate::task::MainThread,
    sym: c_uint,
    wcode: c_uint,
    route: &mut Route,
    played_from_detail: bool,
    refresh_hubs_at: &mut u32,
    trail: &mut Trail,
) {
    if is_back(sym, wcode) {
        if matches!(modal_of(*route), Modal::None) {
            exit_player(mt, route, played_from_detail, refresh_hubs_at, trail);
        } else {
            close_player_overlays();
            *route = Route::Player { overlay: Overlay::None };
        }
    }
}

/// The in-player track menu is modal — it swallows every key while open.
fn key_track_menu(sym: c_uint, wcode: c_uint, now: u32, route: &mut Route, held: &mut HeldKey) {
    if sym == SDLK_LEFT || sym == SDLK_RIGHT || sym == SDLK_UP || sym == SDLK_DOWN {
        // move once on the fresh press; holding repeats via the client-side timer
        crate::ui::track_menu::move_focus(sym as c_int);
        held.arm(sym, now);
        extend_hud(now, HUD_MENU_MS);
    } else if is_ok(sym) {
        crate::ui::track_menu::on_ok();
        *route = Route::Player { overlay: Overlay::None };
        extend_hud(now, HUD_LINGER_MS);
    } else if is_back(sym, wcode) {
        crate::ui::track_menu::close();
        *route = Route::Player { overlay: Overlay::None };
    }
}

/// The `…` overflow popover is modal too, and has ONE column — so LEFT/RIGHT are swallowed without
/// moving anything, rather than falling through to the scrubber.
fn key_more_menu(sym: c_uint, wcode: c_uint, now: u32, route: &mut Route, held: &mut HeldKey) {
    if sym == SDLK_UP || sym == SDLK_DOWN {
        crate::ui::more_menu::move_focus(sym as c_int);
        held.arm(sym, now);
        extend_hud(now, HUD_MENU_MS);
    } else if is_ok(sym) {
        apply_more_action(crate::ui::more_menu::on_ok());
        *route = Route::Player { overlay: Overlay::None };
        extend_hud(now, HUD_LINGER_MS);
    } else if is_back(sym, wcode) {
        crate::ui::more_menu::close();
        *route = Route::Player { overlay: Overlay::None };
    }
}

/// The Info card is modal too — it swallows every key while open.
fn key_info_panel(
    mt: &crate::task::MainThread,
    sym: c_uint,
    wcode: c_uint,
    now: u32,
    route: &mut Route,
    played_from_detail: bool,
    refresh_hubs_at: &mut u32,
    trail: &mut Trail,
    hud_nav: &mut HudNav,
    held: &mut HeldKey,
) {
    if sym == SDLK_DOWN && crate::ui::info_panel::at_last() {
        // past the bottom of the card → drop focus back onto the tabs
        crate::ui::info_panel::close();
        *route = Route::Player { overlay: Overlay::None };
        hud_nav.focus = 2;
        extend_hud(now, HUD_LINGER_MS);
    } else if sym == SDLK_UP || sym == SDLK_DOWN {
        crate::ui::info_panel::move_focus(sym as c_int);
        held.arm(sym, now); // holding repeats via the client-side timer
        extend_hud(now, HUD_MENU_MS);
    } else if is_ok(sym) {
        match crate::ui::info_panel::on_ok() {
            crate::ui::info_panel::InfoAction::FromBeginning => {
                request_seek(0);
                if paused() {
                    set_paused(false);
                    crate::player::resume(mt);
                }
            }
            crate::ui::info_panel::InfoAction::GoToDetail(rk) => {
                // Leave playback through THE exit ritual, then override where
                // it landed. This arm used to hand-roll the exit — overlays +
                // stop_bufferfeed — which is three quarters of `exit_player`
                // and silently dropped the other quarter: `route::cancel_play()`
                // (a jump taken while a play resolve was still in flight left
                // it to land later on Detail, starting audio the user cannot
                // reach) and the armed hub refresh (Continue Watching kept the
                // resume point from BEFORE this session — exactly the stale-CW
                // bug `exit_player`'s doc warns a new exit path re-introduces).
                // The override is the one real difference: the Info card's
                // "Go to Show/Movie" always lands on THIS rk's page, whatever
                // origin route the ritual would otherwise have chosen.
                if !rk.is_empty() {
                    // The played leaf's server, read BEFORE the exit ritual —
                    // `detail_rk` is that item's own show, so it is on the same
                    // machine, and the store this reads is torn down below.
                    let sid = crate::metadata::playing()
                        .map(|p| p.sid)
                        .unwrap_or_else(crate::plex::current_server);
                    exit_player(mt, route, played_from_detail, refresh_hubs_at, trail);
                    crate::ui::detail::open_rk(sid, &rk);
                    // A LANDING, not a navigation, so the trail is made to agree
                    // rather than pushed blindly: the exit above has usually
                    // already put this very page on top (the show playback
                    // started from), and `ensure_detail` is a no-op there. It is
                    // also strictly better than the flag it replaces — a
                    // Library → detail → play → "Go to Show" now returns to the
                    // Library instead of to Home.
                    trail.ensure_detail(sid, &rk);
                    *route = Route::Detail;
                }
            }
            crate::ui::info_panel::InfoAction::None => {}
        }
        // guarded: the GoToDetail arm above set Route::Detail — don't resurrect Player over it
        if matches!(*route, Route::Player { .. }) {
            *route = Route::Player { overlay: Overlay::None };
        }
        extend_hud(now, HUD_LINGER_MS);
    } else if is_back(sym, wcode) {
        crate::ui::info_panel::close();
        *route = Route::Player { overlay: Overlay::None };
        extend_hud(now, HUD_LINGER_MS);
    }
}

/// The Chapters strip is modal too — LEFT/RIGHT pick, OK seeks, BACK closes.
fn key_chapters(
    mt: &crate::task::MainThread,
    key: Key,
    sym: c_uint,
    wcode: c_uint,
    now: u32,
    route: &mut Route,
    hud_nav: &mut HudNav,
    held: &mut HeldKey,
) {
    if matches!(key, Key::Left { .. } | Key::Right { .. }) {
        let dir_sym = if matches!(key, Key::Left { .. }) { SDLK_LEFT } else { SDLK_RIGHT };
        crate::ui::chapters_panel::move_focus(dir_sym as c_int);
        // hold-repeat via the client-side timer, but only when the direction
        // arrived as the plain sym (keyup clears held_key.sym by matching sym;
        // arming it with a normalized key for the alt-d-pad wcodes would stick
        // on release).
        if matches!(key, Key::Left { alt: false } | Key::Right { alt: false }) {
            held.arm(sym, now);
        }
        extend_hud(now, HUD_MENU_MS);
    } else if is_ok(sym) {
        let ns = crate::ui::chapters_panel::on_ok();
        if ns >= 0 {
            request_seek(ns);
            if paused() {
                set_paused(false);
                crate::player::resume(mt);
            }
        }
        *route = Route::Player { overlay: Overlay::None };
        extend_hud(now, HUD_LINGER_MS);
    } else if matches!(key, Key::Down) {
        // drop focus back onto the tabs below the strip
        crate::ui::chapters_panel::close();
        *route = Route::Player { overlay: Overlay::None };
        hud_nav.focus = 2;
        extend_hud(now, HUD_LINGER_MS);
    } else if is_back(sym, wcode) {
        crate::ui::chapters_panel::close();
        *route = Route::Player { overlay: Overlay::None };
        extend_hud(now, HUD_LINGER_MS);
    }
}

/// Playing: UP/DOWN move the HUD focus (scrubber ↔ buttons ↔ tabs). The first press on a hidden HUD
/// just reveals it (focused on the scrubber); pressing UP with nothing focusable above (the buttons
/// row) hides the HUD again.
fn key_player_updown(key: Key, now: u32, hud: &mut HudState, scrubber: &mut Scrub) {
    let vis = hud_visible(now, hud_until(), paused(), hud.dismissed);
    let mut hide = false;
    if !vis {
        hud.nav.focus = 0; // reveal, on the scrubber
    } else if matches!(key, Key::Up) {
        // vertical stack, top → bottom: control row, scrubber, tabs. Both
        // marker stand-ins live IN the control row, so the ring is unchanged.
        match hud.nav.focus {
            0 => hud.nav.focus = 1, // scrubber → control row
            2 => hud.nav.focus = 0, // tabs → scrubber
            _ => {
                hide = true; // control row: nothing above → hide the HUD
                hud.nav.focus = 0;
            }
        }
    } else {
        match hud.nav.focus {
            0 => hud.nav.focus = 2, // scrubber → tabs
            1 => hud.nav.focus = 0, // buttons → scrubber
            _ => {}             // tabs: nothing below → stay
        }
    }
    if hud.nav.focus != 0 || hide {
        // leaving the bar cancels any in-progress scrub preview
        if scrub() >= 0 {
            set_scrub(-1);
        }
        scrubber.disengage();
    }
    if hide {
        hud.dismissed = true; // stays hidden even while paused, until the next key
    } else {
        extend_hud(now, HUD_LINGER_MS);
    }
}

/// D-pad on a NON-player screen: hand the direction to whichever screen owns focus, then arm the
/// client-side hold-repeat.
fn key_move_focus(key: Key, sym: c_uint, route: Route, now: u32, held: &mut HeldKey) {
    if matches!(route, Route::Detail) {
        crate::ui::detail::move_focus(sym as c_int);
    } else if matches!(route, Route::Person) {
        crate::ui::person::move_focus(sym);
    } else if matches!(route, Route::Library) {
        crate::ui::library::move_focus(sym);
    } else if matches!(route, Route::Search) {
        crate::ui::search::move_focus(sym);
    } else if g_snap() < 0.5 {
        if matches!(key, Key::Down) {
            if crate::ui::home::hero_focus() < 0 {
                crate::ui::home::set_hero_focus(0); // chip → back to the action row
            } else {
                set_snap(1.0);
                set_fr(0);
            }
        } else if matches!(key, Key::Left { alt: false } | Key::Right { alt: false }) {
            crate::ui::home::home_hero_key(sym); // walk the action row; RIGHT on the chevron pages
        } else if matches!(key, Key::Up) {
            // hero view: UP focuses the profile chip (OK then opens the menu —
            // the chip is selectable, it no longer springs the menu unbidden)
            crate::ui::home::set_hero_focus(-1);
        }
    } else if matches!(key, Key::Up) && g_fr() == 0 {
        set_snap(0.0);
    } else {
        crate::ui::home::home_move_focus(sym);
    }
    held.arm(sym, now);
}

/// OK, on every screen that has not already `continue`d above.
unsafe fn key_ok(
    mt: &crate::task::MainThread,
    ctrl: crate::ui::player_hud::ControlSlot,
    now: u32,
    route: &mut Route,
    hud: &mut HudState,
    ptr: &mut Pointer,
    held: &mut HeldKey,
    trail: &mut Trail,
    nav: &mut Option<NavReq>,
    played_from_detail: &mut bool,
    refresh_hubs_at: &mut u32,
    ok_armed: &mut bool,
) {
    if matches!(*route, Route::Player { .. }) {
        let vis = hud_visible(now, hud_until(), paused(), hud.dismissed);
        // A stand-in owns row 1 — activate it. Same value the draw used.
        if vis && hud.nav.focus == 1 && !ctrl.is_discs() {
            if activate_ctrl_row(mt, ctrl, route, played_from_detail, refresh_hubs_at, &mut hud.nav, trail) {
                held.sym = 0; // async route flip: don't repeat a held key into the next screen
            }
        } else if vis && hud.nav.focus == 1 {
            // …so the discs are what row 1 holds — the complement of the arm
            // above, and the row's only other occupant.
            // OK on a control disc opens its panel (Subtitles / Audio / More)
            if hud.nav.btn == crate::ui::player_hud::BTN_MORE {
                crate::ui::more_menu::open();
                *route = Route::Player { overlay: Overlay::More };
            } else {
                crate::ui::track_menu::open_tab(if hud.nav.btn == 0 { 1 } else { 0 });
                *route = Route::Player { overlay: Overlay::Menu };
            }
        } else if vis && hud.nav.focus == 2 {
            if hud.nav.tab == 0 {
                crate::ui::info_panel::open(); // Info card
                *route = Route::Player { overlay: Overlay::Info };
            } else if hud.nav.tab == 1 {
                crate::ui::chapters_panel::open(); // Chapters strip
                *route = Route::Player { overlay: Overlay::Chapters };
            }
        } else {
            let np = !paused();
            set_paused(np);
            if np {
                crate::player::pause(mt);
            } else {
                crate::player::resume(mt);
            }
        }
        extend_hud(now, HUD_LINGER_MS);
    } else if matches!(*route, Route::Search) {
        // A result tile takes the tvOS press (dip now, commit on the spring-back
        // — `ok_armed` runs `on_ok` then); the field and the recents rows commit
        // immediately inside the screen.
        let spill = crate::ui::search::focused_pill();
        if spill >= 0 {
            match crate::ui::widgets::pill_at(spill as usize) {
                // the screen we are already on — a deliberate no-op, as Home's
                // own pill is on Home
                Pill::Search => {}
                Pill::Section(tab) => nav_to(*route, Nav::Library(tab), nav),
                // focus lands on the Home pill, which is the pill Home selects
                // anyway — the strip must not appear to move under the swap
                Pill::Home => nav_to(*route, Nav::Home { focus_pill: Some(0) }, nav),
            }
        } else if crate::ui::search::focus_is_card() {
            crate::ui::press::begin(SDL_GetTicks());
            *ok_armed = true;
        } else if let crate::ui::search::Action::Open(node) = crate::ui::search::on_ok() {
            nav_open(*route, node, None, nav);
        }
    } else if matches!(*route, Route::Library) {
        // OK on a browse-grid card → the same tvOS press as home's grid;
        // tabs / toolbar / menus commit immediately inside the screen.
        if crate::ui::library::focus_is_card() {
            crate::ui::press::begin(SDL_GetTicks());
            *ok_armed = true;
        } else {
            match crate::ui::library::on_ok() {
                crate::ui::library::Action::GoHome => {
                    nav_to(*route, Nav::Home { focus_pill: crate::ui::library::focused_pill() }, nav)
                }
                crate::ui::library::Action::GoSearch => {
                    nav_to(*route, Nav::Search, nav)
                }
                crate::ui::library::Action::Card | crate::ui::library::Action::None => {}
            }
        }
    } else if matches!(*route, Route::Detail) {
        // OK on a detail CARD (episode / Related / Cast) → tvOS press: dip now,
        // commit on the spring-back (the route-agnostic press handler runs on_ok
        // then). The Play pill, season tabs and About rows activate immediately.
        if crate::ui::detail::focus_is_card() {
            crate::ui::press::begin(SDL_GetTicks());
            *ok_armed = true;
        } else if crate::ui::detail::on_ok() {
            start_playback(
                mt,
                crate::ui::detail::last_resume_ns(),
                true, // Stop/BACK/EOS returns to this detail page
                HUD_LINGER_MS,
                route,
                played_from_detail,
                &mut hud.nav,
            );
        }
    } else if matches!(*route, Route::Person) {
        // every focusable thing on the person page is a poster card → the
        // same tvOS press as home's grid, committed on the spring-back
        if crate::ui::person::focus_is_card() {
            crate::ui::press::begin(SDL_GetTicks());
            *ok_armed = true;
        }
    } else {
        // home: dispatch through the ONE activation (shared with pointer
        // clicks). Gate hero-vs-grid on the spring POSITION (what's on
        // screen), not the snap target: a DOWN press flips the target to grid
        // instantly while the hero stays visible ~130ms, so a quick DOWN→OK
        // must still act on the hero shown, not the grid's card 0.
        if crate::ui::home::snap_pos() < 0.5 {
            // hero (Play pill / chip / pills / chevron): activate immediately.
            let hf = crate::ui::home::hero_focus();
            home_activate(mt, hf, HUD_LINGER_MS, route, played_from_detail, trail, &mut hud.nav, nav);
        } else {
            // grid card: tvOS press — dip the focused card now, activate on the
            // spring-back (committed from the per-frame loop). Nav cancels, so the
            // focused cell can't move while the press is armed.
            crate::ui::press::begin(SDL_GetTicks());
            *ok_armed = true;
        }
        if !ptr.dpad_mode {
            hide_cursor();
            ptr.dpad_mode = true;
            ptr.cur_hidden = true;
        }
    }
}

/// PAUSE — the dedicated transport key, which only ever pauses (PLAY is its other half).
fn key_pause(mt: &crate::task::MainThread, route: Route, now: u32) {
    if matches!(route, Route::Player { .. }) && !paused() {
        set_paused(true);
        crate::player::pause(mt);
    }
    extend_hud(now, HUD_LINGER_MS);
}

/// PLAY — off the player route it starts the buffer-feed and enters the player; on it, it un-pauses.
unsafe fn key_play(
    mt: &crate::task::MainThread,
    now: u32,
    bg_was_playing: bool,
    route: &mut Route,
    played_from_detail: &mut bool,
    ptr: &mut Pointer,
) {
    if !matches!(*route, Route::Player { .. }) {
        if crate::player::start_bufferfeed(mt) {
            // resuming a suspended session (bg_was_playing) keeps its origin;
            // a fresh play derives it from the current route. Guards the tiny
            // bg→fg window where route is still Home but the session came from detail.
            *played_from_detail = if bg_was_playing { *played_from_detail } else { matches!(*route, Route::Detail) };
            *route = Route::Player { overlay: Overlay::None };
        }
        set_paused(false);
        if !ptr.dpad_mode {
            hide_cursor();
            ptr.dpad_mode = true;
            ptr.cur_hidden = true;
        }
    } else if paused() {
        set_paused(false);
        crate::player::resume(mt);
    }
    extend_hud(now, HUD_LINGER_MS);
}

/// LEFT/RIGHT while playing: move the focused HUD row's cursor, or — on the scrubber — jump the
/// scrub preview. A fresh press (0x001) is the fixed 10s jump; a held key's 0x101 repeats
/// ([`on_auto_repeat`]) then engage the continuous scrub and the keyup commits.
unsafe fn key_scrub(
    key: Key,
    now: u32,
    ctrl: crate::ui::player_hud::ControlSlot,
    hud: &mut HudState,
    ptr: &mut Pointer,
    scrubber: &mut Scrub,
) {
    if !ptr.cur_hidden {
        hide_cursor();
        ptr.cur_hidden = true;
    }
    if ptr.drag {
        ptr.drag = false;
        set_scrub(-1);
    }
    let fwd = matches!(key, Key::Right { .. });
    let vis = hud_visible(now, hud_until(), paused(), hud.dismissed);
    extend_hud(now, HUD_LINGER_MS);
    if !vis {
        hud.nav.focus = 0; // first LEFT/RIGHT reveals the HUD on the scrubber
    }
    if hud.nav.focus == 1 {
        // the row's occupant says how many items it has — no magic pin
        hud.nav.btn = (hud.nav.btn + if fwd { 1 } else { -1 }).clamp(0, ctrl.items() - 1);
    } else if hud.nav.focus == 2 {
        let max_tab = if crate::ui::chapters_panel::has_chapters() { 1 } else { 0 };
        hud.nav.tab = (hud.nav.tab + if fwd { 1 } else { -1 }).clamp(0, max_tab);
    } else if dur() > 0 {
        // scrubber focus, FRESH press (0x001): the fixed 10s jump. A held key's
        // 0x101 repeats (handled above) then engage the continuous scrub; the
        // keyup commits. Quick re-taps before scrubber.commit_at accumulate.
        let cap = dur() - 3 * 1_000_000_000;
        scrubber.commit_at = 0; // more input → cancel a pending tap commit
        scrubber.alive = now;
        if scrubber.dir == 0 && scrub() < 0 {
            // Seed a new scrub at the INTENDED playhead (`intended_pos`). If a
            // prior commit's seek is still landing, playpos() is stale (it still
            // reports the pre-seek spot), so a quick re-press would jump back to
            // where we started and resume there — interrupting the scrub. The
            // divergence IS "a seek is in flight", so log it when the two
            // disagree rather than re-deriving the condition here.
            let seed = intended_pos();
            let live = playpos();
            if seed != live {
                log(&format!("scrub: seed at in-flight target {}s (playpos {}s stale)",
                    seed / 1_000_000_000, live / 1_000_000_000));
            }
            set_scrub(seed);
        }
        if !scrubber.hold {
            let mut s = scrub().max(0) + if fwd { SCRUB_STEP_NS } else { -SCRUB_STEP_NS };
            if s < 0 {
                s = 0;
            }
            if cap > 0 && s > cap {
                s = cap;
            }
            set_scrub(s);
        }
        scrubber.dir = if fwd { 1 } else { -1 };
    }
}

/// CH▲/CH▼ page the browse grid a screenful of rows per press.
fn key_library_page(sym: c_uint, wcode: c_uint) {
    let up = sym == SDLK_PAGEUP || wcode == WCODE_CH_UP;
    crate::ui::library::page(if up { -1 } else { 1 });
}

/// webOS BACK: this Magic Remote sends wcode 482 (0x1E2); 461 kept for others.
///
/// Back stack: player -> the TRAIL (detail/person, at any depth) -> library -> grid -> hero ->
/// exit. Inside the Library, BACK first walks menu -> tab bar (library::back), THEN leaves to Home.
/// The ORDER is unchanged; what changed is that detail/person pop a real trail (`ui::trail`)
/// instead of consulting two booleans that had one slot per screen KIND and so could not describe a
/// detail page standing on another one.
///
/// A BACK inside the page fade's 70 ms window WITHDRAWS the transition rather than acting on a
/// screen that is already half gone: the request is at most four frames old and nothing has changed
/// yet, so it can still be un-asked. `nav_cancel` refuses once the swap has happened, and then this
/// is an ordinary BACK on the NEW screen — the press is never dropped, only ever spent on exactly
/// one of the two. (It matters most at Home's root, where "what BACK would otherwise do" is exit
/// the app.)
fn key_back(
    mt: &crate::task::MainThread,
    route: &mut Route,
    nav: &mut Option<NavReq>,
    trail: &mut Trail,
    played_from_detail: bool,
    refresh_hubs_at: &mut u32,
    running: &mut bool,
) {
    if nav_cancel(*route, nav) {
    } else if matches!(*route, Route::Player { .. }) {
        exit_player(mt, route, played_from_detail, refresh_hubs_at, trail);
    } else if matches!(*route, Route::Detail | Route::Person) {
        // The two stacking screens, through the page transition. All three
        // halves of the pop — the outgoing page's teardown, the trail move and
        // the re-entry — land together at the fade FLOOR (`nav_back`), because
        // a pop is always all three and splitting them across the 70 ms window
        // is how you get a page blanking during its own fade-out or a second
        // BACK popping a node whose page is still on screen. Only the PEEK
        // (does the page underneath wear the tab bar?) happens here.
        //
        // …but a panel the SCREEN has open takes the press first and the page
        // stays: `detail::back()` is `library::back()`'s shape one screen over
        // ("Also available" is part of the detail page, so leaving the page
        // must not be the way to close it). It answers false on Person, which
        // has no panel of its own.
        if !crate::ui::detail::back() {
            nav_back(*route, trail, nav);
        }
    } else if matches!(*route, Route::Search) {
        // `back()` answers true while it still had something to close (the
        // raised keyboard); false means leave, and the destination is Home —
        // Search is a peer of it, not a page stacked on it.
        if !crate::ui::search::back() {
            nav_to(*route, Nav::Home { focus_pill: None }, nav);
        }
    } else if matches!(*route, Route::Library) {
        // read BEFORE `back()`: its first press moves focus ONTO the tab row, so
        // asking afterwards would report the pill it just landed on rather than
        // the one the user was standing on when they chose to leave.
        //
        // No `trail.back()` here: the destination is Home, and the commit frame
        // of the page transition truncates the trail to its root — which is both
        // stronger and cancel-safe (a BACK withdrawn inside the 70 ms window
        // must not have moved the history).
        let pill = crate::ui::library::focused_pill();
        if !crate::ui::library::back() {
            nav_to(*route, Nav::Home { focus_pill: pill }, nav);
        }
    } else if g_snap() > 0.5 {
        set_snap(0.0);
    } else {
        // Home is the ROOT and BACK there exits the app — deliberately NOT
        // trail-driven. The background-suspend arm drops to Home without
        // touching the trail, so route and trail can legitimately disagree;
        // keeping this branch blind to the trail is what stops that divergence
        // teleporting the user into a page they did not navigate to, and what
        // keeps the true root exiting whatever the trail happens to hold.
        *running = false;
    }
}

#[no_mangle]
pub extern "C" fn plex_run(pms_host: *const c_char, pms_port: c_int) -> c_int {
    install_panic_logger();
    // FIRST, before SDL and before anything can fail: which television is this. A report from
    // hardware nobody here owns is worth far more when its opening line names the firmware — see
    // `webos`'s module doc. Reads one file; cannot fail the boot.
    crate::webos::probe();
    // And what it DECODES, from the device's own codec table — the capability profile and the
    // direct-play gate derive from this instead of asserting the dev TV's abilities as universal
    // (issue #22's bug class; docs/plex-pass-audit.md's closing section). Same contract as
    // above: one file read, cannot fail the boot, falls back to the profile that always shipped.
    crate::devcaps::probe();
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
                log(&format!("video driver: {}", std::ffi::CStr::from_ptr(d).to_string_lossy()));
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
                log(&format!("GL: {} / {}", std::ffi::CStr::from_ptr(r).to_string_lossy(),
                    std::ffi::CStr::from_ptr(v).to_string_lossy()));
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
        let wflags = SDL_GetWindowFlags(win);
        log(&format!("keyboard: support={} active={} focus={} winflags=0x{wflags:x}",
            SDL_HasScreenKeyboardSupport(), SDL_IsTextInputActive(),
            i32::from(wflags & SDL_WINDOW_INPUT_FOCUS != 0)));

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
            Err(e) => log(&format!("servers: /tmp/plxnative-servers IGNORED — not valid JSON: {e}")),
            Ok(v) if !v.is_empty() => {
                let usable = v.iter().filter(|s| s.usable()).count();
                for (i, s) in v.iter().enumerate() {
                    let creds = if s.usable() { "ok" } else { "MISSING (empty host/port or token)" };
                    log(&format!("servers: #{i} {} creds={creds}", s.describe()));
                }
                log(&format!("servers: {} extra server(s) injected, {usable} usable", v.len()));
            }
            Ok(_) => {}
        }
        let host_s = std::ffi::CStr::from_ptr(pms_host).to_string_lossy().into_owned();

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
            let nmov = crate::pms::pms_fetch_hubs();
            // section discovery (one small GET) so Home's library tab pills carry real titles
            let nsec = crate::browse::ensure_sections();
            log(&format!("pms: nmovies={nmov} nsections={nsec}"));
        };
        // Install the PMS client (the read layer AND the playback path) as the CURRENT server,
        // then fetch the catalog. Used by the boot gate and again when a login resolves; a later
        // call for the same address just swaps the token (profile switch).
        let install_pms = |host: &str, port: c_int, token: &str| {
            crate::plex::install(host, port, token); // a (re)install is a login / profile switch
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
            for s in crate::dev::servers().unwrap_or_default().iter().filter(|s| s.usable()) {
                let id = crate::plex::register(&s.machine_id, &s.host, s.port as c_int, &s.token);
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
        let mut pick_user: Option<usize> = crate::dev::read("pickuser").and_then(|s| s.parse().ok());
        let session = crate::plex::session::load();
        let boot_to = if crate::dev::flag("login") {
            crate::auth::start_login();
            log("boot: /tmp/plxnative-login — starting QR login");
            BootTo::Login
        } else if !dev_token.is_empty() {
            install_pms(&host_s, pms_port, &dev_token);
            BootTo::Home
        } else if session.can_go_local() {
            if session.home_users.len() > 1 && (!automated_boot() || pick_user.is_some()) {
                // Who's watching first. Only the read client is installed here (the avatars proxy
                // through the PMS photo transcoder); the catalog fetch + playback config happen in
                // take_ready once a profile is picked — done now they'd be thrown out on a switch.
                crate::plex::install(&session.server.address, session.server.port as i32, session.pms_token());
                crate::plex::session::set_current(Some(session.user.clone()));
                crate::auth::start_switch(); // seeds the persisted roster + refreshes it online
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
                // …and re-learn it from plex.tv in the background. The stored roster is what a boot
                // can show IMMEDIATELY (and all it can show offline); this is what makes a share
                // granted since the last sign-in ever appear at all. Non-destructive on failure.
                crate::auth::refresh_roster_online();
                install_pms(&session.server.address, session.server.port as c_int, session.pms_token());
                crate::plex::session::set_current(Some(session.user.clone())); // Home profile chip
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
        // dev: /tmp/plxnative-glasshz=<presents-per-refresh> moves the shared dynamic-backdrop
        // cadence for the cost curve in `docs/backdrop-blur-profiling.md` — 1 is a refresh on every
        // present (60 Hz while the UI presents at 60) and is what ships, 3 is ~20 Hz, 4 is 15 Hz.
        // ABSENT, nothing here runs and the cadence is exactly the shipped one. It is a profiling
        // knob, so it also turns on the heartbeat's `snap=` field (refreshes per second), which
        // is the only way to check the cadence that RAN against the one that was asked for — and
        // is why `fps:home-acct-glass` arms it at the shipped 1 rather than leaving it absent.
        let glass_hz_armed = if let Some(v) = crate::dev::read("glasshz") {
            let asked: u32 = v.parse().unwrap_or(0);
            let got = crate::ui::widgets::set_dynamic_period(asked);
            log(&format!("blur: dynamic cadence asked={asked} presents-per-refresh={got}"));
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
        // dev: /tmp/plxnative-noidle turns the whole-frame present gate (ui::idle) OFF, so a still
        // screen goes back to repainting at panel rate. It is a DIAG trigger (see the list above)
        // precisely so an A/B costs one file and does not also change which screen you boot to —
        // and so that if a frame ever looks wrong on the panel, ruling this feature out is one
        // `rm` rather than a redeploy.
        crate::ui::testpat::boot();
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
        let framedrop_thresh: f64 =
            framedrop.and_then(|s| s.parse().ok()).filter(|v: &f64| *v > 0.0).unwrap_or(22.0);
        let perf_freq = SDL_GetPerformanceFrequency() as f64;
        let perf_ms = |c: u64| c as f64 * 1000.0 / perf_freq;
        let mut fd_worst = 0.0f64; // worst frame-total this second, for a once/sec peak line

        let mut last_input = SDL_GetTicks();
        let t0 = last_input;
        let mut loop_t = t0;
        let mut iters_ct = 0i32;
        let mut loop_shown = 0i32;
        let mut running = true;
        // Dev-only panel proof: advance a red/green counter phase only after SDL_GL_SwapWindow
        // returns. Hold each colour for 30 swaps: per-buffer alternation blends yellow at 60 Hz,
        // while this ~2 Hz change is human-visible and still freezes immediately with presentation.
        #[cfg(feature = "devtools")]
        let mut buffer_flip_count = 0u8;

        let mut held_key = HeldKey::IDLE;
        let mut scrubber = Scrub::IDLE;
        let mut hud = HudState::IDLE;
        let mut marker_tried = false; // dev: the /tmp/plxnative-marker jump has been resolved
        let mut bg_was_playing = false;
        let mut bg_was_paused = false;
        let mut bg_pos = 0i64;
        // ui::press click state: a grid-card OK is deferred (press-in on down, activate on the
        // spring-back after key-up) so `ok_armed` marks "a press is in flight, commit it from the
        // per-frame loop when press::take_commit fires". Only ever set on Home's grid.
        let mut ok_armed = false;
        let mut press_tried = false; // dev: /tmp/plxnative-press fires one simulated grid-card press
        let mut press_release_at = 0u32; // …and the tick at which that simulated press releases
        let mut itemmenu_tried = false; // dev: /tmp/plxnative-itemmenu opens the card context menu once
        let mut ptr = Pointer::IDLE;

        // Initial route from the boot gate: Login when we have no usable creds, Profiles for the
        // boot who's-watching picker, else Home.
        let mut route = match boot_to {
            BootTo::Home => Route::Home,
            BootTo::Login => Route::Login,
            BootTo::Profiles => Route::Profiles,
        };
        // dev: /tmp/plxnative-acct auto-opens the profile menu (headless capture of the popover).
        if crate::dev::flag("acct") && matches!(route, Route::Home) {
            crate::ui::account_menu::open();
            route = Route::Account;
        }
        // Return target for playback started from a detail page: Stop/BACK/EOS from such a session
        // returns to that detail page, else home. Kept OUTSIDE Route (like bg_was_playing keeps the
        // suspended session) — it's navigation history, not the current node, and Route makes
        // Detail/Player exclusive so it can't be encoded there.
        let mut played_from_detail = false;
        // The BACK trail (`ui::trail`): the pages behind the one on screen, top = current. It
        // replaces the `opened_from_library` / `opened_from_person` pair, which were a precedence
        // ladder with one slot per screen KIND and so could not describe a detail page standing on
        // another detail page — the episode filmstrip's text row and the Related shelf both do that,
        // and BACK from such a page fell through to Home. A run-loop LOCAL, exactly like the
        // booleans it replaces and like `played_from_detail` beside it: navigation history belongs
        // to the loop that navigates.
        //
        // `played_from_detail` deliberately does NOT fold into it. It answers a different question —
        // where does THIS SESSION return to — and is written per `start_playback` call, including
        // the deliberate `false` where `home_activate` opens a detail page under the hood just to
        // fire its Play. The app-switch path depends on that independence (the background arm drops
        // to Home without touching either).
        let mut trail = crate::ui::trail::Trail::new();
        // The route change the page cross-fade is carrying, applied at its floor. `None` whenever
        // no transition is in flight — which is every path that deliberately keeps today's hard cut
        // (a boot trigger, a player exit, the app-switch lifecycle, a login landing), so the
        // default really is "nothing changes".
        let mut nav_pending: Option<NavReq> = None;

        let mut auto_tried = false;
        let mut grid_tried = false;
        let mut seek_tried = false;
        // /tmp/plxnative-autoseek seek script (see the parse site): pending steps, the tick of
        // the last fired step, the gap between steps, and the last REQUESTED target (the base
        // for "+10"/"-10" tap-relative steps, like taps on the HUD's frozen scrub playhead).
        let mut seek_script: Vec<String> = Vec::new();
        let mut seek_script_at = 0u32;
        let mut seek_gap_ms = 300u32;
        let mut seek_script_last = 0i64;
        let mut detail_tried = false;
        let mut play_tried = false;
        let mut menu_tried = false;
        let mut menupick_tried = false;
        let mut pause_tried = false;
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
        while running {
            // Resolve the control row ONCE per iteration, before the event pump, and pass this
            // value to input, update and draw alike. `player_hud::slot()` reads `playpos_ns`, which
            // LG's media thread writes and `player::pump` advances mid-iteration — deriving it per
            // call site let a keypress activate a control this same frame then declined to draw.
            let ctrl = crate::ui::player_hud::slot();
            crate::system::ls2_pump();
            if let Some(r) = remote.as_mut() {
                r.drain(|tok| {
                    crate::ui::idle::invalidate(); // an injected key is input like any other
                    // pointer click token "ck:X,Y" — authored 1920x1080 coords
                    if let Some(rest) = tok.strip_prefix("ck:") {
                        if let Some((xs, ys)) = rest.split_once(',') {
                            if let (Ok(x), Ok(y)) = (xs.parse::<i32>(), ys.parse::<i32>()) {
                                log(&format!("remote: click {},{}", x, y));
                                remote_synth_ptr(x.clamp(0, 1919), y.clamp(0, 1079));
                            }
                        }
                    } else if cfg!(feature = "hostsim") && tok == "shot" {
                        // Simulator only. Screenshotting has to be a TOKEN rather than a launch
                        // option, because the interesting frame is the one AFTER driving, and
                        // `PLXNATIVE_SHOT_FRAME` is fixed before the app starts — worse, presented
                        // frames only accrue when something repaints (the idle gate), so no frame
                        // number can be predicted from outside. This makes
                        // `down down right ok shot` a single composable line.
                        #[cfg(feature = "hostsim")]
                        crate::shot::request();
                    } else if tok == "okdown" || tok == "okup" {
                        // the two halves of OK, so a driver can hold it: `okdown`, wait past
                        // press::LONG_MS, `okup` — the only way to exercise a press-and-hold (and so
                        // the item menu) over the FIFO, since every other token is a tap.
                        remote_synth_key_edge(SDLK_RETURN, 0, tok == "okdown");
                    } else if let Some(spec) = tok.strip_prefix("pat:") {
                        // `pat:flat:40` — swap the SYNTHETIC GROUND live. A boot trigger could set
                        // one, but a graded ladder needs a dozen of them in one session and a
                        // trigger is read once; this is what makes a snapshot sweep a single
                        // scripted line instead of a dozen launches that each land on a different
                        // hero. See `ui::testpat`.
                        if !crate::ui::testpat::set(spec) {
                            crate::log(&format!("remote: unrecognised pattern {spec:?}"));
                        }
                    } else if let Some(text) = tok.strip_prefix("txt:") {
                        // `txt:star+wars` — commit text as the system keyboard's IME would. The
                        // only way to get text into the app without a human: no trigger can raise
                        // the panel, and typing into the simulator needs somebody at the keyboard.
                        // `+` is a space, because this protocol is whitespace-DELIMITED
                        // (`remote.rs` splits on it) so a token can never contain one and a query
                        // is mostly spaces; a literal '+' cannot be sent, which no query needs.
                        //
                        // **Handed to `textinput` directly and NOT through `SDL_PushEvent` — the
                        // way every other synthetic input here works, and the way that CRASHES.**
                        // Measured 2026-08-14: pushing a synthetic `SDL_TEXTINPUT` SIGSEGVs
                        // inside SDL itself (`KERN_INVALID_ADDRESS at 0x8`, no Rust panic and no
                        // log line), because the Mac's `libSDL2` is **sdl2-compat forwarding into
                        // libSDL3** — the backtrace runs `libSDL2-2.0.0.dylib SDL_PushEvent_REAL`
                        // → `libSDL3.0.dylib SDL_PushEvent_REAL` — and SDL3's text event carries a
                        // `char *text` POINTER where SDL2's carries an inline `char text[32]`. The
                        // compat layer dereferences it while converting. Keys and `ck:` clicks
                        // push safely only because their fields are all scalars.
                        //
                        // So this exercises `on_event` and everything below it — the platform
                        // layout, the decode, the queue cap, the drain — and deliberately claims
                        // nothing about `SDL_PollEvent`'s own delivery, which only a real panel
                        // (or a real keystroke in `make sim-run`) can prove.
                        let ev = crate::textinput::encode_event(&text.replace('+', " "));
                        crate::textinput::on_event(&ev);
                        // Logged, because until a screen drains this queue the token is otherwise
                        // completely unobservable — and what it prints is the string read back
                        // through the platform's own offset, not the one that was written.
                        log(&format!("txt: decoded {:?} pending={}",
                            crate::textinput::decode(&ev), crate::textinput::pending()));
                    } else if let Some((sym, wcode)) = remote_token_key(tok) {
                        remote_synth_key(sym, wcode);
                    } else {
                        log(&format!("remote: unknown token {tok:?}")); // catch mangling in transit
                    }
                });
            }
            while SDL_PollEvent(ev.as_mut_ptr() as *mut c_void) != 0 {
                // ANY event is a reason to repaint (`ui::idle`): a key changes focus or a label,
                // a lifecycle event changes the whole screen. Marked here — once, for every event
                // kind — rather than in each of the ~30 arms below, where the next one added would
                // silently draw nothing.
                crate::ui::idle::invalidate();
                let et = rd_u32(&ev, 0);
                if et == SDL_KEYDOWN || et == SDL_KEYUP || et == SDL_TEXTINPUT || et == SDL_TEXTEDITING {
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
                    log(&format!("[{}] {what} type=0x{et:x} raw={hex}", SDL_GetTicks()));
                }
                if et == SDL_QUIT {
                    running = false;
                } else if et == 0x103 || et == 0x104 {
                    // WILL/DID ENTER BACKGROUND
                    log(&format!("LIFECYCLE: background (playing={})", matches!(route, Route::Player { .. }) as i32));
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
                    if matches!(route, Route::Player { .. }) && !bg_was_playing {
                        // INTENDED, not published: this snapshot is the only thing the foreground
                        // restore has, and `suspend_bufferfeed` below drops the pending seek target
                        // with the session — so a background that lands while a seek is still
                        // resolving would otherwise save (and restore to) the spot the user just
                        // seeked AWAY from, with nothing left to correct it. See `intended_pos`.
                        bg_pos = intended_pos();
                        bg_was_playing = true;
                        bg_was_paused = paused();
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
                    log(&format!("LIFECYCLE: foreground (wasPlaying={})", bg_was_playing as i32));
                    if bg_was_playing && et == 0x106 {
                        bg_was_playing = false; // clear regardless so a later 0x106 can't re-fire
                        // only resume if a PLAY key didn't already restart playback in the
                        // WILL->DID window (a second start would drop the live Engine -> UAF)
                        if !matches!(route, Route::Player { .. }) {
                            // Restore at the saved position with a SINGLE Load: arm the position via
                            // resume_at() BEFORE start_bufferfeed (same as the Continue-Watching
                            // resume). The old start+request_seek order did an in-place seek right
                            // after the fresh Load, whose reopen stalled the video decoder (BufferFull
                            // — black plane) while audio kept playing.
                            let mut rt = bg_pos;
                            if !bg_was_paused {
                                rt -= RESUME_REWIND_NS;
                                if rt < 0 {
                                    rt = 0;
                                }
                            }
                            crate::player::resume_at(rt);
                            let started = crate::player::start_bufferfeed(mt);
                            if started {
                                route = Route::Player { overlay: Overlay::None };
                                set_hud(SDL_GetTicks() + HUD_LINGER_MS);
                                set_resume_pend(bg_was_paused);
                            }
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
                        on_key_up(sym, isnav, route, ok_armed, &mut held_key, &mut scrubber, &mut bg_pos);
                        continue;
                    }
                    // A repeat is only a repeat if we watched the key go down. See
                    // `HeldKey::down_sym`: the system keyboard eats key-ups, so the driver stamps
                    // 0x100 on presses that are the FIRST of their own gesture, and dropping those
                    // loses one press in two.
                    if state & 0x100 != 0 && sym == held_key.down_sym {
                        on_auto_repeat(sym, isnav, route, ok_armed, hud.nav, &mut held_key, &mut scrubber);
                        continue;
                    }
                    // From here down this IS a fresh press, whatever the driver stamped on it.
                    last_input = SDL_GetTicks();
                    begin_fresh_press(key, sym, &mut held_key, &mut hud, &mut ptr, &mut ok_armed);

                    // ---- the route-scoped arms, each of which `continue`s once it has taken the
                    // press. That makes the chain itself the priority statement: an earlier guard
                    // subsumes each later one it overlaps with, which the playback-failure guard
                    // below does deliberately. Keep it a chain — a `match` over the same routes
                    // compiles and keeps the suite green while silently reordering it, because
                    // exhaustiveness cannot see subsumption.
                    if matches!(route, Route::Login | Route::Profiles) {
                        key_onboarding(route, sym, wcode, &mut ok_armed);
                        continue;
                    }
                    if matches!(route, Route::Account) {
                        key_account(sym, wcode, &mut route);
                        continue;
                    }
                    if let Route::ItemMenu { over } = route {
                        key_item_menu(mt, over, sym, wcode, last_input, &mut route,
                            &mut played_from_detail, &mut trail, &mut hud.nav, &mut nav_pending,
                            &mut held_key);
                        continue;
                    }
                    // …and THIS is the guard the next four sit under: while a playback failure owns
                    // the frame it `continue`s on every key, so the Menu / More / Info / Chapters
                    // arms below do not run at all. Deliberate — see `key_player_failed`.
                    if matches!(route, Route::Player { .. }) && crate::ui::player_hud::transport_hidden() {
                        key_player_failed(mt, sym, wcode, &mut route, played_from_detail,
                            &mut refresh_hubs_at, &mut trail);
                        continue;
                    }
                    if matches!(route, Route::Player { overlay: Overlay::Menu }) {
                        key_track_menu(sym, wcode, last_input, &mut route, &mut held_key);
                        continue;
                    }
                    if matches!(route, Route::Player { overlay: Overlay::More }) {
                        key_more_menu(sym, wcode, last_input, &mut route, &mut held_key);
                        continue;
                    }
                    if matches!(route, Route::Player { overlay: Overlay::Info }) {
                        key_info_panel(mt, sym, wcode, last_input, &mut route, played_from_detail,
                            &mut refresh_hubs_at, &mut trail, &mut hud.nav, &mut held_key);
                        continue;
                    }
                    if matches!(route, Route::Player { overlay: Overlay::Chapters }) {
                        key_chapters(mt, key, sym, wcode, last_input, &mut route, &mut hud.nav,
                            &mut held_key);
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
                        && matches!(key, Key::Up | Key::Down | Key::Left { alt: false } | Key::Right { alt: false })
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
                        key_ok(mt, ctrl, last_input, &mut route, &mut hud, &mut ptr, &mut held_key,
                            &mut trail, &mut nav_pending, &mut played_from_detail,
                            &mut refresh_hubs_at, &mut ok_armed);
                    } else if matches!(key, Key::Pause) {
                        key_pause(mt, route, last_input);
                    } else if matches!(key, Key::Play) {
                        key_play(mt, last_input, bg_was_playing, &mut route, &mut played_from_detail,
                            &mut ptr);
                    } else if matches!(route, Route::Player { .. }) && matches!(key, Key::Stop) {
                        // Stop — the whole arm is the one ritual, already named.
                        exit_player(mt, &mut route, played_from_detail, &mut refresh_hubs_at, &mut trail);
                    } else if matches!(route, Route::Player { .. }) && matches!(key, Key::Left { .. } | Key::Right { .. }) {
                        key_scrub(key, last_input, ctrl, &mut hud, &mut ptr, &mut scrubber);
                    } else if matches!(route, Route::Library)
                        && (sym == SDLK_PAGEUP || sym == SDLK_PAGEDOWN || wcode == WCODE_CH_UP || wcode == WCODE_CH_DOWN)
                    {
                        key_library_page(sym, wcode);
                    } else if matches!(key, Key::Back) {
                        key_back(mt, &mut route, &mut nav_pending, &mut trail, played_from_detail,
                            &mut refresh_hubs_at, &mut running);
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
                    if matches!(route, Route::Profiles) {
                        crate::ui::profiles::pointer_focus(mx, my);
                    } else if matches!(route, Route::Account) {
                        crate::ui::account_menu::pointer_focus(mx, my);
                    } else if matches!(route, Route::Player { overlay: Overlay::More }) {
                        crate::ui::more_menu::pointer_focus(mx, my);
                    } else if matches!(route, Route::ItemMenu { .. }) {
                        crate::ui::item_menu::pointer_focus(mx, my);
                    } else if matches!(route, Route::Library) {
                        crate::ui::library::pointer_focus(mx, my);
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
                                crate::ui::home::set_hero_focus(crate::ui::home::hero_focus_for_pill(i));
                            }
                        } else {
                            crate::ui::home::home_pointer_focus(mx, my);
                        }
                    }
                } else if et == SDL_MOUSEBUTTONDOWN {
                    last_input = SDL_GetTicks();
                    // …and the pointer's half of the same rule: the hit-tests below consult
                    // geometry (`icon_hit`, `scrub_hit`, the tab row) that a failure has erased from
                    // the frame. The key path's BACK exception has no pointer twin — there is no
                    // BACK to click — so a click on a failed read-out is simply nothing.
                    if matches!(route, Route::Player { .. }) && crate::ui::player_hud::transport_hidden() {
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
                                route = Route::Player { overlay: Overlay::None };
                            }
                            Modal::Info => {
                                crate::ui::info_panel::close();
                                route = Route::Player { overlay: Overlay::None };
                            }
                            Modal::Chapters => {
                                crate::ui::chapters_panel::close();
                                route = Route::Player { overlay: Overlay::None };
                            }
                            // Unlike the panels above, this popover's rows are ACTIONS, so a click
                            // that lands on one commits it (and a click outside reports None and
                            // just dismisses) — `account_menu`'s contract, same as its key path.
                            Modal::More => {
                                apply_more_action(crate::ui::more_menu::click(cx, cy));
                                route = Route::Player { overlay: Overlay::None };
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
                                activate_ctrl_row(mt, ctrl, &mut route, &mut played_from_detail, &mut refresh_hubs_at, &mut hud.nav, &mut trail);
                            }
                            _ => {
                                // shared HUD geometry: player_hud owns the button rects + scrub
                                // band — consulted only while that geometry is on screen
                                let icon = if hud_vis { crate::ui::player_hud::icon_hit(ctrl, cx, cy) } else { None };
                                let on_scrub =
                                    if hud_vis && dur() > 0 { crate::ui::player_hud::scrub_hit(cx, cy) } else { None };
                                if let Some(idx) = icon {
                                    if idx == crate::ui::player_hud::BTN_MORE {
                                        crate::ui::more_menu::open();
                                        route = Route::Player { overlay: Overlay::More };
                                    } else {
                                        crate::ui::track_menu::open_tab(if idx == 0 { 1 } else { 0 }); // Subtitles button → subtitles tab
                                        route = Route::Player { overlay: Overlay::Menu };
                                    }
                                    hud.nav.focus = 1;
                                    hud.nav.btn = idx;
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
                                    set_paused(np);
                                    if np {
                                        crate::player::pause(mt);
                                    } else {
                                        crate::player::resume(mt);
                                    }
                                }
                            }
                        }
                        extend_hud(last_input, HUD_LINGER_MS);
                    } else if matches!(route, Route::Home) {
                        let (cx, cy) = ptr_xy(&ev);
                        if crate::ui::home::profile_chip_click(cx, cy) {
                            crate::ui::account_menu::open(); // top-left avatar → profile menu
                            route = Route::Account;
                        } else if let Some(i) = crate::ui::widgets::tab_pill_at(cx, cy) {
                            // the centered tab pills work from BOTH hero and grid views
                            match crate::ui::widgets::pill_at(i) {
                                Pill::Search => nav_to(route, Nav::Search, &mut nav_pending),
                                Pill::Section(tab) => nav_to(route, Nav::Library(tab), &mut nav_pending),
                                // Home is the screen we are on, so a click there just parks focus
                                // on the pill — in hero view, which is where the band's focus is
                                // visible — unless there is a section switch still fading out to
                                // take back, which is the key twin's rule.
                                Pill::Home => {
                                    if !nav_cancel(route, &mut nav_pending) && crate::ui::home::snap_pos() < 0.5 {
                                        crate::ui::home::set_hero_focus(crate::ui::home::hero_focus_for_pill(0));
                                    }
                                }
                            }
                        } else if crate::ui::home::snap_pos() < 0.5 {
                            // hero visible: clicks act on the action row via the ONE activation;
                            // holding the click on the chevron keeps paging (see the per-frame pump)
                            let b = crate::ui::home::hero_button_at(cx, cy);
                            if b >= 0 {
                                crate::ui::home::set_hero_focus(b);
                                home_activate(mt, b, HUD_LINGER_MS, &mut route, &mut played_from_detail, &mut trail, &mut hud.nav, &mut nav_pending);
                                if b == 2 {
                                    ptr.hold_pager = last_input;
                                }
                            }
                        } else if crate::ui::home::home_card_click(cx, cy) {
                            // grid card: click = OK (play a Continue-Watching tile / open detail)
                            home_activate(mt, c_int::MIN, HUD_LINGER_MS, &mut route, &mut played_from_detail, &mut trail, &mut hud.nav, &mut nav_pending);
                        }
                    } else if matches!(route, Route::Search) {
                        let (cx, cy) = ptr_xy(&ev);
                        // The strip is shared chrome and is hit-tested here, not by the screen —
                        // `tab_pill_at` owns the clipped rects, so a pill scrolled half out of the
                        // track is clickable across exactly the half you can see.
                        if let Some(i) = crate::ui::widgets::tab_pill_at(cx, cy) {
                            match crate::ui::widgets::pill_at(i) {
                                Pill::Search => {} // the screen we are already on
                                Pill::Section(tab) => nav_to(route, Nav::Library(tab), &mut nav_pending),
                                Pill::Home => nav_to(route, Nav::Home { focus_pill: Some(0) }, &mut nav_pending),
                            }
                        } else if let crate::ui::search::Action::Open(node) = crate::ui::search::click(cx, cy) {
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
                                nav_to(route, Nav::Home { focus_pill: crate::ui::library::focused_pill() }, &mut nav_pending)
                            }
                            crate::ui::library::Action::Card => {
                                open_library_card(route, &mut nav_pending);
                            }
                            crate::ui::library::Action::None => {}
                        }
                    } else if matches!(route, Route::Detail) {
                        // Magic-Remote click on the detail page: focus what was clicked, then run the
                        // SAME activation the OK key does (detail::click did the hit-test) — a CARD
                        // (episode / Related / Cast) gets the tvOS press dip, committed on the
                        // button-up spring-back below; the Play pill, watched disc and season tabs
                        // act at once, exactly as in the key arm.
                        let (cx, cy) = ptr_xy(&ev);
                        // A FRESH click supersedes a press still in flight from the previous one —
                        // the pointer's twin of the nav-key abort above. Without this, clicking a
                        // card and then something else within the ~210ms commit window let the
                        // card's deferred activation fire AFTER the second click had already acted
                        // (two `on_ok`s: the watched toggle flipped twice, each with its own
                        // blocking refetch). The card branch re-arms from scratch below.
                        if ok_armed {
                            crate::ui::press::cancel();
                            ok_armed = false;
                        }
                        if crate::ui::detail::click(cx, cy) {
                            if crate::ui::detail::focus_is_card() {
                                crate::ui::press::begin(last_input);
                                ok_armed = true;
                            } else if crate::ui::detail::on_ok() {
                                start_playback(
                                    mt,
                                    crate::ui::detail::last_resume_ns(),
                                    true, // Stop/BACK/EOS returns to this detail page
                                    HUD_LINGER_MS,
                                    &mut route,
                                    &mut played_from_detail,
                                    &mut hud.nav,
                                );
                            }
                        }
                    } else if matches!(route, Route::Person) {
                        let (cx, cy) = ptr_xy(&ev);
                        if matches!(crate::ui::person::click(cx, cy), crate::ui::person::Action::Card) {
                            open_person_card(route, &mut nav_pending);
                        }
                    } else if matches!(route, Route::Account) {
                        let (cx, cy) = ptr_xy(&ev);
                        // a click on a row commits it; anywhere else dismisses the popover
                        match crate::ui::account_menu::click(cx, cy) {
                            crate::ui::account_menu::Action::ChangeProfile => {
                                crate::auth::start_switch();
                                crate::ui::profiles::enter();
                                route = Route::Profiles;
                            }
                            crate::ui::account_menu::Action::SignIn => {
                                crate::auth::start_login();
                                route = Route::Login;
                            }
                            crate::ui::account_menu::Action::SignOut => {
                                crate::auth::sign_out();
                                route = Route::Login;
                            }
                            crate::ui::account_menu::Action::None => {
                                crate::ui::account_menu::close();
                                route = Route::Home;
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
                        apply_item_action(mt, act, over, &mut route, &mut played_from_detail, &mut trail, &mut hud.nav, &mut nav_pending);
                    } else if matches!(route, Route::Profiles) {
                        let (cx, cy) = ptr_xy(&ev);
                        crate::ui::profiles::click(cx, cy);
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
                    ptr.hold_pager = 0; // releasing the click stops the hero click-hold pager
                    if ptr.drag {
                        ptr.drag = false;
                        if scrub() >= 0 {
                            commit_seek(scrub(), &mut bg_pos);
                        }
                        extend_hud(last_input, HUD_LINGER_MS);
                    }
                } else if et == SDL_MOUSEWHEEL {
                    last_input = SDL_GetTicks();
                    if last_input.wrapping_sub(ptr.last_wheel) > 250 {
                        ptr.last_wheel = last_input;
                        let dy = rd_i32(&ev, 20);
                        // the wheel scrolls VERTICALLY only, and only on routes with a vertical
                        // flow (it used to drive home's focus behind every other screen)
                        if matches!(route, Route::Home) {
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
                            crate::ui::detail::move_focus(if dy < 0 { SDLK_DOWN } else { SDLK_UP } as c_int);
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

            let now = SDL_GetTicks();
            // dev: /tmp/plxnative-autoplay auto-presses OK once
            if !auto_tried && !matches!(route, Route::Player { .. }) && now.wrapping_sub(t0) > 2000 {
                auto_tried = true;
                if crate::dev::flag("autoplay") {
                    if crate::dev::flag("h265") {
                        // Phase 0 HEVC probe: leave the URL empty so start_bufferfeed feeds
                        // the local /tmp/sample.h265 through the H265 Load payload.
                        crate::route::clear_url();
                    } else {
                        let pidx = crate::dev::read("playidx")
                            .and_then(|s| s.parse::<c_int>().ok()).unwrap_or(0);
                        if let Some(pmm) = crate::ui::home::movie_at(pidx / COLS, pidx % COLS) {
                            crate::route::request_play_movie(pmm);
                            crate::metadata::load_detail_now(pmm.sid, &pmm.rk);
                        }
                    }
                    let fd = matches!(route, Route::Detail);
                    start_playback(mt, 0, fd, HUD_HEADLESS_MS, &mut route, &mut played_from_detail, &mut hud.nav);
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
                // N is a PILL, not a section index, and the two stopped being the same thing when
                // the strip became one pill per TYPE (`browse::tab_section`). With one source they
                // are still the identity map, which is every install this trigger has ever booted;
                // with a friend's server granted, the pills are your own libraries plus any type
                // only they have, so `library=1` is the second PILL rather than the second row of
                // the table.
                if let Some(s) = crate::dev::read("library") {
                    let tab = s.parse::<usize>().unwrap_or(0);
                    // A HARD CUT, deliberately: a transition means "this screen replaced that one",
                    // and at boot there is no outgoing screen to replace. The fade-in that IS wanted
                    // here belongs to the screen (`enter`'s own `xf().mount()`); dipping the whole
                    // page would fade the tab bar up from nothing too, which reads as a slow app
                    // rather than a navigated one.
                    crate::ui::library::enter(tab, crate::ui::library::Arrival::Cut);
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
                        // a dev trigger names a bare rk, so it means the server this headless
                        // boot signed in to — the only one it has
                        let sid = crate::plex::current_server();
                        let idx = crate::pms::index_of_rk(sid, rk);
                        // BLOCKING both ways: the sub-triggers below replay move_focus/on_ok in
                        // THIS frame, and they walk sections() — which is hero-only until the
                        // item lands.
                        if idx >= 0 {
                            crate::ui::detail::open(idx);
                        } else {
                            crate::ui::detail::open_rk_now(sid, rk);
                        }
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
                        // dev: /tmp/plxnative-detailok presses OK on whatever the two triggers
                        // above focused, WITHOUT the play path — the deterministic one-boot route
                        // to a section whose OK navigates rather than plays. Today that means the
                        // cast row → the person page (detailsec/detailcol pick the headshot); the
                        // press animation is skipped on purpose, this is the activation only.
                        if crate::dev::flag("detailok") {
                            crate::ui::detail::on_ok(); // a cast row raises a person request; the
                            // per-frame drain below routes on it, like every other OK path
                        }
                        // dev: /tmp/plxnative-detailplay activates the focused control (headless play test)
                        if crate::dev::flag("detailplay") && crate::ui::detail::on_ok() {
                            let fd = matches!(route, Route::Detail);
                            start_playback(
                                mt,
                                crate::ui::detail::last_resume_ns(),
                                fd,
                                HUD_HEADLESS_MS,
                                &mut route,
                                &mut played_from_detail,
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
            if !play_tried && !matches!(route, Route::Player { .. }) && now.wrapping_sub(t0) > 500 {
                play_tried = true;
                if let Some(rk) = crate::dev::read("play") {
                    let rk = rk.as_str();
                    if !rk.is_empty() {
                        // BLOCKING on purpose: the leaf extraction below reads current() on the
                        // next statement, and this block sits behind a one-shot `play_tried`
                        // latch, so a deferred landing would have nothing left to consume it —
                        // every case in tests/manifest.json drives through here.
                        crate::metadata::load_detail_now(crate::plex::current_server(), rk); // fetch ANY rk (movie/show/episode)
                        // a movie/episode leaf carries its own part+codecs; a show has an
                        // empty part, so fall back to its first episode.
                        let leaf = crate::metadata::current().map(|d| {
                            if !d.part.is_empty() {
                                (d.part.clone(), d.vcodec.clone(), d.acodec.clone(),
                                 d.title.clone(), d.resume_ms, d.dur_ms)
                            } else if let Some(ep) = d.episodes.first() {
                                (ep.part.clone(), ep.vcodec.clone(), ep.acodec.clone(),
                                 d.title.clone(), ep.resume_ms, ep.dur_ms)
                            } else {
                                (String::new(), String::new(), String::new(),
                                 d.title.clone(), 0, 0)
                            }
                        });
                        if let Some((part, vc, ac, title, resume_ms, dur_ms)) = leaf {
                            if !part.is_empty() {
                                log(&format!("plxnative-play: rk={rk} start"));
                                // the dev `play` trigger names a rk on whatever server is current,
                                // which is what `item_sid`'s fallback resolves to — stated through
                                // it rather than by calling `surface_sid` directly, so this reads as
                                // the one deliberate surface-relative play rather than another site
                                // that forgot the item's own server.
                                crate::route::request_play(
                                    crate::route::item_sid(crate::plex::ServerId::UNSET),
                                    rk,
                                    &part,
                                    &vc,
                                    &ac,
                                    &title,
                                    "",
                                );
                                let resume = crate::metadata::resume_ns(resume_ms, dur_ms);
                                let fd = matches!(route, Route::Detail);
                                start_playback(mt, resume, fd, HUD_HEADLESS_MS, &mut route, &mut played_from_detail, &mut hud.nav);
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
            if !seek_tried && matches!(route, Route::Player { .. }) && dur() > 0 && now.wrapping_sub(t0) > 12000 {
                seek_tried = true;
                if let Some(s) = crate::dev::read("autoseek") {
                    let mut steps: Vec<String> =
                        s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
                    if let Some(g) = steps.first().and_then(|f| f.strip_prefix("gap=")) {
                        seek_gap_ms = g.parse().unwrap_or(300).max(50);
                        steps.remove(0);
                    }
                    if steps.is_empty() {
                        steps.push("140".to_string());
                    }
                    seek_script_last = crate::player::playpos_ns();
                    seek_script_at = now.wrapping_sub(seek_gap_ms); // fire the first step now
                    seek_script = steps;
                }
            }
            if !seek_script.is_empty()
                && matches!(route, Route::Player { .. })
                && now.wrapping_sub(seek_script_at) >= seek_gap_ms
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
                log(&format!("autoseek: step → {}s ({} left)", t / 1_000_000_000, seek_script.len()));
                request_seek(t);
            }
            // dev: /tmp/plxnative-autopause pauses once (headless paused-HUD capture)
            if !pause_tried && matches!(route, Route::Player { .. }) && now.wrapping_sub(t0) > 6000 {
                pause_tried = true;
                if crate::dev::flag("autopause") {
                    set_paused(true);
                    set_hud(now + HUD_HEADLESS_MS);
                }
            }
            // dev: /tmp/plxnative-menu=<tab> opens the in-player track menu once (headless capture)
            if !menu_tried && matches!(route, Route::Player { .. }) && now.wrapping_sub(t0) > 6000 {
                menu_tried = true;
                if let Some(t) = crate::dev::read("menu") {
                    crate::ui::track_menu::open_tab(t.parse::<c_int>().unwrap_or(0));
                    route = Route::Player { overlay: Overlay::Menu };
                    set_hud(now + HUD_HEADLESS_MS);
                }
                // dev: /tmp/plxnative-info opens the Info card once (headless capture)
                if crate::dev::flag("info") {
                    crate::ui::info_panel::open();
                    route = Route::Player { overlay: Overlay::Info };
                    hud.nav.focus = 2;
                    hud.nav.tab = 0;
                    set_hud(now + HUD_HEADLESS_MS);
                }
                // dev: /tmp/plxnative-chapters opens the Chapters strip once (headless capture)
                if crate::dev::flag("chapters") {
                    crate::ui::chapters_panel::open();
                    route = Route::Player { overlay: Overlay::Chapters };
                    hud.nav.focus = 2;
                    hud.nav.tab = 1;
                    set_hud(now + HUD_HEADLESS_MS);
                }
            }
            // dev: /tmp/plxnative-menupick="<tab>,<row>" opens the menu, selects that row, and
            // confirms it (headless track switch: e.g. "0,4" = audio tab, row 4).
            if !menupick_tried && matches!(route, Route::Player { .. }) && now.wrapping_sub(t0) > 7000 {
                menupick_tried = true;
                if let Some(s) = crate::dev::read("menupick") {
                    let mut it = s.split(',');
                    let tab = it.next().and_then(|x| x.trim().parse::<c_int>().ok()).unwrap_or(0);
                    let row = it.next().and_then(|x| x.trim().parse::<c_int>().ok()).unwrap_or(0);
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
            if !marker_tried && matches!(route, Route::Player { .. }) && crate::player::is_playing() {
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
                                log(&format!("marker trigger: seek to {}s (5s before {:?})", t / 1_000_000_000, want));
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
            // end-of-stream: the pipeline drained at the credits → hand off to Up Next when the
            // show has another episode queued, else leave the player (back to the detail page or
            // home, whichever is behind), instead of freezing on the last frame.
            if matches!(route, Route::Player { .. }) && crate::player::ended() {
                finish_playback(mt, &mut route, &mut played_from_detail, &mut refresh_hubs_at, &mut hud.nav, &mut trail);
                held_key.sym = 0; // async route flip: don't repeat a still-held key into detail/home
            }
            // Up Next countdown elapsed → start the queued episode on its own. Beside the EOS
            // handoff so the whole auto-advance chain reads in one place.
            if matches!(route, Route::Player { .. }) && crate::ui::up_next::expired(now) {
                if !play_up_next(mt, HUD_LINGER_MS, &mut route, &mut played_from_detail, &mut hud.nav) {
                    crate::ui::up_next::cancel(); // nothing queued after all — don't re-fire
                }
                held_key.sym = 0;
            }
            // post-playback home refresh (armed by every exit_player): refetch the hubs so
            // Continue Watching shows the new resume point / next episode; the small delay lets
            // the final timeline PUT land server-side first. Blocking (~100ms LAN) but the user
            // just navigated, so a one-frame hitch here is invisible.
            if refresh_hubs_at != 0
                && now.wrapping_sub(refresh_hubs_at) < 0x8000_0000
                && !matches!(route, Route::Player { .. })
            {
                refresh_hubs_at = 0;
                let nmov = crate::pms::refetch_hubs_reconcile();
                log(&format!("home: hubs refreshed after playback ({nmov} items)"));
            }
            // hero click-hold pager: while the click stays down on the chevron, keep paging (the
            // pointer twin of holding RIGHT; hero_flip's cooldown sets the pace).
            if ptr.hold_pager != 0
                && now.wrapping_sub(ptr.hold_pager) > 450
                && matches!(route, Route::Home)
                && crate::ui::home::snap_pos() < 0.5
            {
                crate::ui::home::hero_flip(1);
            }
            // lost-keyup safety: the remote streams 0x101 repeats (~50ms) while a key is physically down,
            // so once past the initial settle a stale heartbeat means the release keyup was dropped —
            // clear the held key so it can't repeat forever (mirrors the scrub's SCRUB_LOST_MS). The
            // 500ms gate leaves the first repeat and the heartbeat's own start-up untouched; a normal
            // release clears via the keyup long before this fires.
            if held_key.sym != 0 && now.wrapping_sub(held_key.since) > 500 && now.wrapping_sub(held_key.alive) > 350 {
                held_key.sym = 0;
            }
            // client-side long-press repeat — the ONE hold-to-move path for every discrete focus list
            // (home grid, detail, track menu, info card, chapters). Driven by a held-key timer so it's
            // identical everywhere and independent of the remote's hardware auto-repeat delay.
            // `HeldKey::arm` is what each view's fresh-press handler calls (always with a standard
            // SDLK_*), and the keyup clears `sym`. The player scrubber is deliberately excluded —
            // holding it runs the continuous scrub.
            if held_key.sym != 0 && now.wrapping_sub(held_key.since) > 380 && now.wrapping_sub(held_key.last_rep) > 110 {
                held_key.last_rep = now;
                match route {
                    Route::Home if g_snap() > 0.5 => crate::ui::home::home_move_focus(held_key.sym),
                    Route::Home => crate::ui::home::home_hero_key(held_key.sym), // hero view: hold LEFT/RIGHT pages the billboard
                    Route::ItemMenu { .. } => crate::ui::item_menu::move_focus(held_key.sym as c_int),
                    Route::Library => crate::ui::library::move_focus(held_key.sym),
                    Route::Search => crate::ui::search::move_focus(held_key.sym),
                    Route::Detail => crate::ui::detail::move_focus(held_key.sym as c_int),
                    Route::Player { overlay: Overlay::Menu } => {
                        crate::ui::track_menu::move_focus(held_key.sym as c_int);
                        extend_hud(now, HUD_MENU_MS);
                    }
                    Route::Player { overlay: Overlay::More } => {
                        crate::ui::more_menu::move_focus(held_key.sym as c_int);
                        extend_hud(now, HUD_MENU_MS);
                    }
                    Route::Player { overlay: Overlay::Info } => {
                        crate::ui::info_panel::move_focus(held_key.sym as c_int);
                        extend_hud(now, HUD_MENU_MS);
                    }
                    Route::Player { overlay: Overlay::Chapters } => {
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
                let mut s = scrub() + (scrubber.dir as f64 * speed as f64 * sdt as f64 * 1e9) as i64;
                let cap = dur() - 3 * 1_000_000_000;
                if s < 0 {
                    s = 0;
                }
                if cap > 0 && s > cap {
                    s = cap;
                }
                set_scrub(s);
                extend_hud(now, HUD_LINGER_MS);
                scrubber.t = now;
                // lost-keyup safety: commit if the 0x101 repeats stop without a keyup
                if now.wrapping_sub(scrubber.alive) > SCRUB_LOST_MS {
                    commit_seek(scrub(), &mut bg_pos);
                    scrubber.disengage();
                }
            }
            // tap release debounce: commit the accumulated jump(s) once no further tap arrives
            if scrubber.commit_at != 0 && now.wrapping_sub(scrubber.commit_at) < 0x8000_0000 {
                if scrub() >= 0 {
                    log(&format!("scrub: tap commit {}s", scrub() / 1_000_000_000));
                    commit_seek(scrub(), &mut bg_pos);
                } else {
                    set_scrub(-1);
                }
                scrubber.disengage();
                scrubber.commit_at = 0;
            }
            // Focus follows the control row's OCCUPANT, on both edges. Driven by slot identity
            // rather than a "was something shown" bool, because the two edges have different jobs
            // and the previous bool implemented neither of the ones its comment promised.
            if matches!(route, Route::Player { overlay: Overlay::None }) {
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
                    // A segment beginning raises the HUD and offers the row, so a bare OK acts on it
                    // in one press instead of raise-HUD → navigate → OK. Only from the RESTING
                    // position though: a user who deliberately walked to the Subtitles button or the
                    // Info tab keeps their spot. (The previous version claimed this in a comment and
                    // then claimed focus unconditionally.)
                    extend_hud(now, HUD_LINGER_MS);
                    if hud.nav.focus == 0 {
                        hud.nav.focus = 1;
                        // …on the row's PRIMARY, which is item 0 for a Skip pill and the RIGHT-hand
                        // one for Up Next. Not cosmetic there: the countdown's cancel rule is a
                        // steady state, so parking on Watch Credits would disarm the timer on the
                        // frame after it armed and the tile would never count down at all.
                        hud.nav.btn = ctrl.primary_btn();
                    }
                } else if crate::ui::player_hud::standin_left_the_ring(hud.was_standin, ctrl, hud.nav.focus == 1) {
                    // The stand-in went away under the focus ring. Without this the row swaps back
                    // to the discs with focus still on it and `btn` still 0, so the next OK opened
                    // the SUBTITLES menu instead of toggling pause — exactly the bug class HudNav's
                    // own doc says it exists to kill. Strictly the EDGE: as a steady state it also
                    // fired on a user who walked UP to the discs on purpose, yanking the ring back
                    // the same frame and making OK on a disc unreachable by remote.
                    hud.nav = HudNav::HOME;
                }
                hud.was_standin = !ctrl.is_discs();
                // While the countdown runs, hold the HUD up — a timer nobody can see is a cut to
                // the next episode out of nowhere. `hud.dismissed` has to clear with it, not just
                // the timer: a user who UP-hid the HUD and then touched nothing until the credits
                // still carries the dismissal, which BEATS `extend_hud` inside `hud_visible`, so
                // the countdown would run behind a tile `draw_hud` never draws and the next
                // episode would start out of nowhere. Whether they may re-dismiss it is the
                // cancel's business, one line below — a dismissed HUD is focus off the row.
                //
                // …and that cancel is `up_next::countdown_may_run`, the ONE rule, applied here
                // rather than at each key arm because every way of taking hold of the row (arrows,
                // a click, walking away to the tabs) ends up as a cursor position by the time this
                // frame draws. Reading it as a steady state is what makes that true.
                if crate::ui::up_next::armed() {
                    if crate::ui::up_next::countdown_may_run(hud.nav.focus == 1, hud.nav.btn) {
                        hud.dismissed = false;
                        extend_hud(now, HUD_LINGER_MS);
                    } else {
                        crate::ui::up_next::cancel();
                    }
                }
            }
            // when the HUD auto-hides, park focus back on the scrubber so the next reveal is clean
            if matches!(route, Route::Player { .. }) && !hud_visible(now, hud_until(), paused(), hud.dismissed) {
                hud.nav = HudNav::HOME;
            }
            // hide the idle pointer during playback
            if matches!(route, Route::Player { .. }) && !ptr.cur_hidden && !ptr.drag && ptr.last_motion != 0 && now.wrapping_sub(ptr.last_motion) > 3000 {
                hide_cursor();
                ptr.cur_hidden = true;
            }
            // re-pause after a resume the INSTANT the seek's frame is on screen. `frames()` counts
            // real "frame presented" callbacks (reset on seek), so >= 1 means the target frame is
            // already composited — re-freezing then shows it with the shortest possible play-blip
            // (a paused scrub must briefly Play to decode the frame; buffer-feed has no preroll).
            if resume_pend() && matches!(route, Route::Player { .. }) && !paused()
                && seek_pending() < 0 && frames() >= 1 && playpos() + 15 * 1_000_000_000 >= bg_pos
            {
                set_paused(true);
                crate::player::pause(mt);
                set_resume_pend(false);
            }

            let dt = {
                let mut d = if prev != 0 { now.wrapping_sub(prev) as f32 / 1000.0 } else { 0.016 };
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
            let mut home_underlay_moving = press_moving;
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
                        Route::Home => crate::ui::home::snap_pos() >= 0.5 && open_item_menu(&mut route),
                        // the detail page's episode filmstrip (`focused_episode` declines every
                        // other section, so a held OK on the Related/Cast shelves — which arm the
                        // same press — falls through to the ordinary spring-back, unchanged)
                        Route::Detail => open_episode_menu(&mut route),
                        _ => false,
                    };
                if held_menu {
                    ok_armed = false;
                    crate::ui::press::cancel();
                } else if crate::ui::press::take_commit(now) {
                    ok_armed = false;
                    match route {
                        Route::Home | Route::Account => {
                            home_activate(mt, c_int::MIN, HUD_LINGER_MS, &mut route, &mut played_from_detail, &mut trail, &mut hud.nav, &mut nav_pending);
                        }
                        Route::Library => open_library_card(route, &mut nav_pending),
                        Route::Detail => {
                            if crate::ui::detail::on_ok() {
                                start_playback(mt, crate::ui::detail::last_resume_ns(), true, HUD_LINGER_MS, &mut route, &mut played_from_detail, &mut hud.nav);
                            }
                        }
                        Route::Person => {
                            if matches!(crate::ui::person::on_ok(), crate::ui::person::Action::Card) {
                                open_person_card(route, &mut nav_pending);
                            }
                        }
                        Route::Search => {
                            if let crate::ui::search::Action::Open(node) = crate::ui::search::on_ok() {
                                nav_open(route, node, None, &mut nav_pending);
                            }
                        }
                        Route::Profiles => crate::ui::profiles::select_focused(),
                        _ => {}
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
                        log(&format!("altsources: no client for slot {} — not navigating", sid.raw()));
                    }
                }
            }

            // login flow: install resolved creds on the MAIN thread, then follow the flow phase →
            // route (Login while creating/waiting/discovering/error, Profiles while picking/switching).
            if matches!(route, Route::Login | Route::Profiles) {
                if let Some(c) = crate::auth::take_ready() {
                    install_pms(&c.host, c.port, &c.token);
                    // the fourth store an identity change must not survive, beside the
                    // `browse`/`pms`/`person` resets `install_pms` performs: a new user must never
                    // be able to walk BACK into the previous one's pages. Reset at the CALL SITE
                    // because `install_pms` is a closure that cannot also hold `&mut trail`.
                    trail.reset();
                    log("login: server installed — entering Home");
                    route = Route::Home;
                } else {
                    match crate::auth::phase() {
                        crate::auth::Phase::Profiles | crate::auth::Phase::Switching => {
                            if route != Route::Profiles {
                                crate::ui::profiles::enter();
                            }
                            route = Route::Profiles;
                        }
                        _ => route = Route::Login,
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
                    route = Route::Account;
                } else if !want && route == Route::Account {
                    crate::ui::account_menu::close();
                    route = Route::Home;
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
                        nav_open(route, to_detail(crate::plex::current_server(), &nav_osc_rk), None, &mut nav_pending)
                    }
                    Route::Detail => nav_back(route, &trail, &mut nav_pending),
                    Route::Home => nav_to(route, Nav::Library(0), &mut nav_pending),
                    // pill 1 is that same first TAB — not "the first section", which stopped being
                    // the same thing when the strip became a projection of the table (`browse::tabs`):
                    // several libraries can share one pill. Home comes back in the hero view with the
                    // top band on the pill the round trip started from, so the scene is a loop
                    Route::Library => nav_to(route, Nav::Home { focus_pill: Some(1) }, &mut nav_pending),
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
                        Nav::Library(sec) => {
                            // every teleport `enter` performs (the store swap, `restore_view`'s
                            // scroll jump, the focus band) happens HERE, at alpha 0, off screen
                            crate::ui::library::enter(sec, crate::ui::library::Arrival::Faded);
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
                            if let Some(i) = focus_pill {
                                // keep the pill the user was standing on under focus, and put
                                // Home in the view where the top band's focus is visible
                                crate::ui::home::set_hero_focus(crate::ui::home::hero_focus_for_pill(i));
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
                            // season. Still BLOCKING (its season lookup indexes the loaded item),
                            // but now behind a page already at alpha 0 instead of stalling the
                            // frame the user's press landed on. `enter_node` then finds the page
                            // mounted and only flips the route.
                            if let (Node::Detail { sid, rk, .. }, Some(s)) = (&node, season) {
                                crate::ui::detail::open_rk_season(*sid, rk, s);
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

            if matches!(route, Route::Login) {
                crate::ui::login::update(dt);
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
            } else if matches!(route, Route::Home | Route::Account | Route::ItemMenu { over: MenuHost::Home }) {
                // dev: sweep the grid focus top↔bottom to reproduce the vertical-scroll judder headlessly
                if home_osc && now.wrapping_sub(home_osc_last) > 350 {
                    home_osc_last = now;
                    let sym = if (now / 3000) % 2 == 0 { SDLK_DOWN } else { SDLK_UP };
                    crate::ui::home::home_move_focus(sym as c_uint);
                }
                // only when home is actually drawn — stepping its 16×24 cell springs during
                // Player/Detail frames was pure waste on the A53 (the ui::press dip/commit is driven
                // route-agnostically right after `dt` above)
                let (_, moving) = crate::ui::idle::scoped_motion(|| {
                    crate::ui::home::home_update(dt);
                });
                home_underlay_moving |= moving;
            } else if matches!(route, Route::Library) {
                // dev: libosc sweeps the browse-grid focus down↔up (the library_scroll FPS scene)
                if lib_osc && now.wrapping_sub(lib_osc_last) > 350 {
                    lib_osc_last = now;
                    let sym = if (now / 3000) % 2 == 0 { SDLK_DOWN } else { SDLK_UP };
                    crate::ui::library::move_focus(sym);
                }
                // dev: libswitch cycles EVERY switch (tabs, sort menu, unwatched, filter) on a
                // timer so the re-query + popover paths are FPS-gated too
                if lib_switch && now.wrapping_sub(lib_switch_last) > 1400 {
                    lib_switch_last = now;
                    crate::ui::library::switch_step(lib_switch_step);
                    lib_switch_step = lib_switch_step.wrapping_add(1);
                }
                crate::ui::library::update(dt);
            }
            if matches!(route, Route::Search) {
                // dev: searchosc sweeps the result shelves' focus down↔up (the fps:search-type
                // scene). Same 350ms step / 3s reversal as homeosc and libosc, so the three read
                // the same in a log and one settle predicate covers all of them.
                if search_osc && now.wrapping_sub(search_osc_last) > 350 {
                    search_osc_last = now;
                    let sym = if (now / 3000) % 2 == 0 { SDLK_DOWN } else { SDLK_UP };
                    crate::ui::search::move_focus(sym);
                }
                crate::ui::widgets::tab_row_update(
                    crate::ui::search::selected_pill(),
                    crate::ui::search::focused_pill(),
                    dt,
                );
                crate::ui::search::update(dt);
            }
            // (The television's keyboard used to be dismissed HERE, by an `else` that called
            // `textinput::stop()` on every frame of every other route — because `search::leave`
            // was reached by no route off the screen. It is `forward_leave`'s job now: Search is
            // not a trail page, so every way off it carries its teardown to the fade floor, which
            // is where the panel is meant to come down and is also the half the poll never did —
            // it cleared `textinput`'s own flag and left `search::EDITING` set.)
            if matches!(route, Route::Account) {
                crate::ui::account_menu::update(dt);
            }
            if matches!(route, Route::ItemMenu { .. }) {
                crate::ui::item_menu::update(dt);
            }
            // The detail page keeps DRAWING under its own context-menu popover (that is what makes it
            // a popover and not a page), so it keeps updating too — otherwise every spring behind the
            // panel freezes mid-pop and the page snaps back into motion when the menu closes.
            if matches!(route, Route::Detail | Route::ItemMenu { over: MenuHost::Detail }) {
                // dev: plxnative-detailosc swings the scroll hero<->bottom so the FPS heartbeat samples the
                // transition (the settled ends already hold 60). Only while the PAGE holds focus: the
                // popover is modal, and sweeping focus under it would walk the anchor out from under it.
                if detail_osc && matches!(route, Route::Detail) {
                    let sym = if (now / 450) % 2 == 0 { SDLK_DOWN } else { SDLK_UP };
                    crate::ui::detail::move_focus(sym as c_int);
                }
                crate::ui::detail::update(dt);
            }
            if matches!(route, Route::Person) {
                // owns the `/library/people/{id}/media` pump — the shelves land here, and the
                // retry backoff only ticks while the page is actually up
                crate::ui::person::update(dt);
            }
            if matches!(route, Route::Player { overlay: Overlay::Menu }) {
                crate::ui::track_menu::update(dt); // pill slide + open fade
            }
            if matches!(route, Route::Player { overlay: Overlay::More }) {
                crate::ui::more_menu::update(dt);
            }
            // Re-samples on its own 2 Hz hold; a no-op when the panel is off.
            crate::ui::stats::update(now);
            if matches!(route, Route::Player { overlay: Overlay::Info }) {
                crate::ui::info_panel::update(dt);
            }
            if matches!(route, Route::Player { overlay: Overlay::Chapters }) {
                crate::ui::chapters_panel::update(dt);
            }
            // Stepped for the WHOLE player route, not per-overlay like the panels above: the
            // countdown must keep running whichever overlay state the route reports.
            // Arm the Up Next countdown the frame it takes the control row. Nothing to step: both
            // stand-ins are drawn by `draw_hud`, so they inherit the transport's visibility rather
            // than owning any motion of their own.
            if matches!(route, Route::Player { .. }) {
                crate::ui::up_next::tick(ctrl, now);
            }
            let fd_pc0 = if framedrop_on { SDL_GetPerformanceCounter() } else { 0 };
            // Async play resolve: install the worker's plan and start the engine. Route-
            // unconditional — a landing must never depend on which screen is mounted.
            if crate::route::pump_play() {
                crate::ui::idle::invalidate();
                let r = PENDING_RESUME_NS.swap(0, Relaxed);
                if r > 0 {
                    crate::player::resume_at(r);
                }
                // A live engine with the route anywhere but Player is unrecoverable BY THE USER —
                // every transport key and the EOS teardown are route-gated — so repair the
                // invariant here rather than trust that no path can violate it. The one that
                // could is cancelled above; this is the backstop, and it is the cheaper half.
                if crate::player::start_bufferfeed(mt) && !matches!(route, Route::Player { .. }) {
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
                    route = Route::Player { overlay: Overlay::None };
                }
            }
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
            crate::posters::poster_pump(3); // invalidates from inside, per texture installed
            let fd_pc_pump = if framedrop_on { SDL_GetPerformanceCounter() } else { 0 };

            let player = matches!(route, Route::Player { .. });
            // EXPERIMENT (`/tmp/plxnative-opaque`): one `static` read and a return when the trigger
            // is absent. Route-scoped and edge-triggered — see `system.rs`.
            crate::system::opaque_route(player);
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
            let (mut fd_pc_draw, mut fd_pc_cap, mut fd_pc_swap) = (fd_pc_pump, fd_pc_pump, fd_pc_pump);
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
                        let subs_lift = hud_up || matches!(route, Route::Player { overlay: Overlay::Menu });
                        crate::ui::player_hud::draw_subtitle_bitmap(subs_lift); // PGS/VobSub image subs
                        crate::ui::player_hud::draw_subtitles(subs_lift);
                        if hud_up || !matches!(route, Route::Player { overlay: Overlay::None }) {
                            // hide the transport middle behind the Info card / Chapters strip
                            crate::ui::player_hud::draw_hud(ctrl, busy, hud.nav.focus, hud.nav.btn, hud.nav.tab, now, !matches!(route, Route::Player { overlay: Overlay::Info | Overlay::Chapters }));
                        }
                        // The read-out is NOT transport chrome — it is drawn whether or not the HUD is
                        // up, so a terminal `Error` (which is not `is_busy()`, so it does not pin the
                        // HUD) keeps its message instead of vanishing with the 4.5 s linger. AFTER the
                        // transport, so it is never dimmed by the scrim; BEFORE the overlay panels
                        // below, so an open Info card / Chapters strip still covers it.
                        crate::ui::player_hud::draw_readout(busy, now);
                        // …and the panels are gated on the SAME failure the transport is, from the
                        // one resolved `busy`: `Player Screen.dc.html` sets `infoDisplay` and
                        // `panelDisplay` to none on the failed variant. A panel open when the
                        // failure landed (`busy_surface` resolves Error to Failed over a held frame
                        // too) would otherwise draw over the read-out — a card of stale facts about
                        // a stream that never started. The key/pointer arms refuse the same state,
                        // so this can never hide something the user can still drive.
                        let panels = !crate::ui::player_hud::transport_hidden();
                        if panels && matches!(route, Route::Player { overlay: Overlay::Menu }) {
                            crate::ui::track_menu::draw();
                        }
                        if panels && matches!(route, Route::Player { overlay: Overlay::Info }) {
                            crate::ui::info_panel::draw();
                        }
                        if panels && matches!(route, Route::Player { overlay: Overlay::Chapters }) {
                            crate::ui::chapters_panel::draw();
                        }
                        if panels && matches!(route, Route::Player { overlay: Overlay::More }) {
                            crate::ui::more_menu::draw();
                        }
                        // LAST, over everything including the centred "Buffering…" read-out whose
                        // block sits where this panel wants to be. It is not chrome and not an
                        // overlay route: it stays up until it is turned off.
                        crate::ui::stats::draw();
                    } else {
                        // Resolve every glass owner BEFORE anything on this route draws — that is
                        // `Glass::prepare`'s contract, and the shared top tab track is an owner on
                        // every route that wears it.
                        if route_wears_tab_bar(route) {
                            crate::ui::widgets::tab_glass_prepare();
                        }
                        if matches!(route, Route::Account) {
                            crate::ui::account_menu::prepare_present(
                                home_underlay_moving || crate::ui::idle::present_dirty(),
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
                        let mut page = || {
                            if matches!(route, Route::Login) {
                                crate::ui::login::draw();
                            } else if matches!(route, Route::Profiles) {
                                crate::ui::profiles::draw();
                            } else if matches!(route, Route::Detail | Route::ItemMenu { over: MenuHost::Detail }) {
                                // the page stays live UNDER its context menu — the popover is anchored beside
                                // the episode still it acts on, which has to still be there to be beside
                                crate::ui::detail::draw();
                            } else if matches!(route, Route::Person) {
                                crate::ui::person::draw();
                            } else if matches!(route, Route::Library) {
                                crate::ui::library::draw();
                            } else if matches!(route, Route::Search) {
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
                            crate::ui::account_menu::draw_scrim();
                            crate::ui::item_menu::draw_scrim();
                        };
                        if let Some(reg) = crate::gfx::blur_direct_region() {
                            crate::gfx::blur_snapshot_direct(reg, &mut page);
                        }
                        crate::ui::profile::phase("main.ui", || page());
                        if matches!(route, Route::Account) {
                            crate::ui::account_menu::draw(); // profile popover over Home
                        }
                        if matches!(route, Route::ItemMenu { .. }) {
                            crate::ui::item_menu::draw(); // press-and-hold card menu, over the live screen
                        }
                        // dev: the blurred route transition, then the load dial's glass surfaces.
                        // LAST on the non-player path, so the snapshot either takes is of the
                        // COMPLETE page — which is the honest source for a surface that sits on
                        // top of everything, and the one thing the tab track (drawn inside the
                        // page) cannot have.
                        crate::ui::glassload::draw_nav_blur();
                        crate::ui::glassload::draw();
                        // The on-screen counter, off the player route (chrome over video). It draws
                        // `loop_shown` — LOOP ITERATIONS, the same number the heartbeat logs as
                        // `loop=`, NOT the frame rate. It also necessarily FREEZES on a settled
                        // screen: it is drawn, so it can only update on a frame that presents.
                        //
                        // NOT in a release build (`make RELEASE=1` → --no-default-features). This
                        // costs the fps scenes nothing: they grade the once/sec heartbeat in the
                        // EVENT LOG, never the pixels, so `loop_floor`/`fps_floor`/`fps_ceiling`
                        // are unaffected by whether the digits are painted.
                        #[cfg(feature = "devtools")]
                        {
                            let loop_col = if buffer_flip_count < 30 {
                                crate::ui::theme::DIAG_FLIP_A
                            } else {
                                crate::ui::theme::DIAG_FLIP_B
                            };
                            crate::gfx::draw_number(loop_shown, SCR_W as f32 - 70.0, 64.0, 46.0, loop_col.as_ptr());
                        }
                    }
                    crate::ui::anim::draw_overlay(); // dev diagnostic overlay (all routes)
                    });
                });
                fd_pc_draw = if framedrop_on { SDL_GetPerformanceCounter() } else { 0 };
                // dev capture stream: grab this finished frame before the swap (after the last draw,
                // so the copy's pass-flush is work the swap would submit anyway). One atomic when idle.
                // Deliberately NOT on the player route (the UI plane is transparent over video, so
                // there is nothing to grab) — capture.rs's 5s keepalive resend covers the host's
                // deadness timer while playback is up.
                if !player {
                    crate::capture::tick(now);
                }
                fd_pc_cap = if framedrop_on { SDL_GetPerformanceCounter() } else { 0 };
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
                fd_pc_swap = if framedrop_on { SDL_GetPerformanceCounter() } else { 0 };
                // Inside the gate: `frame_end` is the end of a DRAWN frame. Counting frames the
                // idle gate skipped would pace the profiler's once-per-N-frames log off frames
                // that ran no phases at all.
                crate::ui::profile::frame_end();
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
            let rn = match route {
                Route::Login => "login",
                Route::Profiles => "profiles",
                Route::Account => "account",
                Route::ItemMenu { .. } => "itemmenu",
                Route::Library => "library",
                Route::Detail => "detail",
                Route::Person => "person",
                Route::Search => "search",
                Route::Player { .. } => "player",
                _ => "home",
            };
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
                    Route::Home => crate::focusprobe::Screen::Home,
                    Route::Account => crate::focusprobe::Screen::Account,
                    Route::ItemMenu { over } => {
                        crate::focusprobe::Screen::ItemMenu { over_detail: matches!(over, MenuHost::Detail) }
                    }
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
                let pump = perf_ms(fd_pc_pump.wrapping_sub(fd_pc0));
                let draw = perf_ms(fd_pc_draw.wrapping_sub(fd_pc_pump));
                let cap = perf_ms(fd_pc_cap.wrapping_sub(fd_pc_draw));
                let swap = perf_ms(fd_pc_swap.wrapping_sub(fd_pc_cap));
                let total = pump + draw + cap + swap;
                let (up, px) = crate::posters::take_upload_stats();
                let (cards, cards_off) = crate::gfx::take_card_stats();
                if total > fd_worst {
                    fd_worst = total;
                }
                if total > framedrop_thresh {
                    log(&format!(
                        "FRAMEDROP total={total:.1} pump={pump:.1} draw={draw:.1} cap={cap:.1} swap={swap:.1} up={up} px={px} cards={cards} off={cards_off} route={rn} load={} snap={:.2}",
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
                let ov = match route {
                    Route::Player { overlay: Overlay::Info } => " overlay=info",
                    Route::Player { overlay: Overlay::Chapters } => " overlay=chapters",
                    Route::Player { overlay: Overlay::Menu } => " overlay=menu",
                    Route::Player { overlay: Overlay::More } => " overlay=more",
                    Route::Player { overlay: Overlay::None } => " overlay=none",
                    _ => "",
                };
                // `pos=<s>` rides the heartbeat while frames are actually being presented: the
                // same SHARED.playpos_ns the /:/timeline reporter posts, but at 1 Hz instead of
                // that reporter's 10s cadence. tests/run.py grades playback progress from this.
                // The cadence is the point: to OBSERVE a 15s climb through 10s samples you must
                // play ~30s, so the sparse signal was charging every case double its real floor.
                // Gated on is_playing() (not is_started()) — see that fn for the resume trap.
                let pos_ns = playpos(); // one read — the test and the value must agree
                let pos = if crate::player::is_playing() && pos_ns > 0 {
                    format!(" pos={}s", pos_ns / 1_000_000_000)
                } else {
                    String::new()
                };
                // `fps=<n>` — frames actually SWAPPED this second, which is what `ui::idle` moves,
                // and the only field here that is a frame rate. `loop=` counts LOOP iterations: it
                // is the app's liveness signal and `pos=` is anchored to it, so it must not read 0
                // on a screen that is merely idle. The pair is the diagnostic — `loop=62 fps=0` is
                // a settled screen doing its job, `loop=0` is an app in trouble, and `fps=0` on its
                // own is not a fault at all. Note the on-screen counter still draws `loop=`.
                let pres = crate::ui::idle::take_presents();
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
                    log(&format!("loop={loop_shown} route={rn}{ov}{pos} fps={pres}{ld} worstframe={fd_worst:.1}ms{SIM_TAG}"));
                    fd_worst = 0.0;
                } else {
                    log(&format!("loop={loop_shown} route={rn}{ov}{pos} fps={pres}{ld}{SIM_TAG}"));
                }
            }
        }

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

    /// The generalisation a reviewer already caught, as an assertion. Making a forward navigation
    /// blanket-carry `leave_of(cur)` is the obvious move and it is WRONG: Detail and Person stay on
    /// the BACK trail, so `detail::close` (and its `metadata::clear`) would empty the page the user
    /// is about to press BACK to, *during its own fade-out*.
    #[test]
    fn a_forward_navigation_never_tears_down_a_page_the_trail_can_put_back() {
        for r in [Route::Home, Route::Library, Route::Detail, Route::Person] {
            assert!(stays_on_trail(r), "a page with a `Node` is a page BACK can return to");
            assert!(forward_leave(r).is_none(), "going deeper must leave the page behind it standing");
        }
        // …and the two that HAVE a teardown really do, so the line above is about the RULE rather
        // than about there being nothing to run either way.
        assert!(leave_of(Route::Detail).is_some(), "a BACK off a detail page still closes it");
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
        assert!(!stays_on_trail(Route::Search), "nothing stacks ON Search — its results stack on Home");
        assert!(forward_leave(Route::Search).is_some(), "a pill press or an opened result takes the keyboard with it");
        assert!(leave_of(Route::Search).is_some(), "…and a BACK runs `leave_of` outright");
    }

    /// A popover is not a page: `page_of` resolves both of them onto the screen underneath, so a
    /// navigation out of the item menu over a detail page must behave exactly like one off that
    /// detail page — otherwise opening a card's menu would change what BACK finds behind it.
    #[test]
    fn a_popover_answers_for_the_screen_it_sits_on() {
        let menu = Route::ItemMenu { over: MenuHost::Detail };
        assert!(stays_on_trail(menu));
        assert!(forward_leave(menu).is_none(), "the detail page under the menu stays mounted");
        assert!(leave_of(menu).is_some(), "…and a BACK off it still closes that page");
        assert!(forward_leave(Route::Account).is_none(), "Account is a popover over Home");
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
            Node::Person { sid, key: String::new(), guid: String::new(), name: String::new(), thumb: String::new() },
            Node::Detail { sid, rk: String::new(), spot: Spot::default() },
        ];
        for n in &nodes {
            assert!(stays_on_trail(node_route(n)), "a page the trail holds must survive a forward navigation off it");
        }
    }
}

#[cfg(test)]
mod key_layout_tests {
    use super::{decode_key, encode_key};
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
        for (sym, wcode) in [(SDLK_DOWN, 0), (SDLK_RETURN, 0), (0, WCODE_PAUSE), (8, WCODE_BACK)] {
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
                assert_eq!(state & 0x100, 0, "a synthetic edge must never look like auto-repeat");
            }
        }
    }
}
