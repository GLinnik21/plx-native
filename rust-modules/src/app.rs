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
const SDL_WINDOW_FLAGS: u32 = 0x2 | 0x1; // OPENGL | FULLSCREEN
const GL_COLOR_BUFFER_BIT: c_uint = 0x0000_4000;
const GL_RENDERER: c_uint = 0x1F01;
const GL_VERSION: c_uint = 0x1F02;
// SDL_GLattr enum
const A_RED: c_int = 0;
const A_GREEN: c_int = 1;
const A_BLUE: c_int = 2;
const A_ALPHA: c_int = 3;
const A_BUFFER_SIZE: c_int = 4;
const A_CTX_MAJOR: c_int = 17;
const A_CTX_MINOR: c_int = 18;
const A_CTX_PROFILE_MASK: c_int = 21;
const CTX_PROFILE_ES: c_int = 0x0004;
// event types
const SDL_QUIT: u32 = 0x100;
const SDL_KEYDOWN: u32 = 0x300;
const SDL_KEYUP: u32 = 0x301;
const SDL_MOUSEMOTION: u32 = 0x400;
const SDL_MOUSEBUTTONDOWN: u32 = 0x401;
const SDL_MOUSEBUTTONUP: u32 = 0x402;
const SDL_MOUSEWHEEL: u32 = 0x403;
// keysyms + the OK/BACK predicates live in ui::consts (the single keycode home)
use crate::ui::consts::{
    is_back, is_ok, SDLK_DOWN, SDLK_ESCAPE, SDLK_LEFT, SDLK_PAGEDOWN, SDLK_PAGEUP, SDLK_RETURN,
    SDLK_RIGHT, SDLK_UP, WCODE_CH_DOWN, WCODE_CH_UP, WCODE_PAUSE, WCODE_PLAY, WCODE_STOP,
};
// The window we ASK SDL for. `surface::probe` then reads back what we actually got.
const SCR_W: c_int = crate::surface::LOGICAL_W as c_int;
const SCR_H: c_int = crate::surface::LOGICAL_H as c_int;
const COLS: c_int = 10;
const RESUME_REWIND_NS: i64 = 5_000_000_000;

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
    fn SDL_webOSCursorVisibility(visible: c_int) -> c_int;
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
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/plxnative-crash.log") {
            let _ = writeln!(f, "{line}");
        }
        default(info); // preserve default behaviour (stderr -> plxnative-stderr.log)
    }));
}

#[inline]
fn rd_u32(ev: &[u8], off: usize) -> u32 {
    u32::from_ne_bytes([ev[off], ev[off + 1], ev[off + 2], ev[off + 3]])
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
        _ => return None,
    })
}

/// Synthesize a Magic-Remote pointer click at authored 1920x1080 coords (the browser
/// remote's click-on-the-stream): two motion events, then button down+up. The first
/// motion is a >=120px jitter so the accumulated pointer distance defeats the
/// dpad_mode pointer gate (`mot_accum < 120` swallows small motions after D-pad use);
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
    let mut ev = [0u8; 128];
    ev[0..4].copy_from_slice(&if down { SDL_KEYDOWN } else { SDL_KEYUP }.to_ne_bytes());
    ev[16..20].copy_from_slice(&if down { 1u32 } else { 0 }.to_ne_bytes()); // state: pressed / released
    ev[20..24].copy_from_slice(&wcode.to_ne_bytes());
    ev[24..28].copy_from_slice(&sym.to_ne_bytes());
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
/// believed it hidden — so the parker reset `hud_nav` to the scrubber on EVERY frame, UP was eaten
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

/// Register the SECOND authority this boot was handed credentials for, and tell the home layer
/// whose it is — so Home is built from both servers instead of one.
///
/// The channel is `/tmp/plxnative-servers` (`dev::servers`), which is how the harness hands the TV a
/// friend's SHARED server: its own `machineIdentifier`, its own per-(user, server) access token, and
/// a 401 for anybody else's — which is precisely why one `plxnative-token` cannot express two
/// servers. It is empty in a release build and on any boot without the trigger, so an ordinary run
/// registers nothing, `pms` falls back to the CURRENT server as its only source, and every heading
/// is unannotated exactly as before.
///
/// The display `name` stands in for the owner's plex.tv handle. Off the wire that is `sourceTitle`
/// on `/api/v2/resources` (`plex::account::Resource`), which the roster layer will read; this file
/// has only what the trigger was written with, and a hand-written name is what the person running
/// the test expects to see in the shelf heading.
fn register_extra_servers() {
    let Ok(extra) = crate::dev::servers() else {
        return; // the parse error was already logged, once, at boot
    };
    let usable: Vec<&crate::dev::DevServer> = extra.iter().filter(|s| s.usable()).collect();
    if usable.is_empty() {
        return; // one server: `pms` derives its own roster and nothing here has an opinion
    }
    // The primary is whatever `plex::install` just made current — captured here rather than looked
    // up later, because it is also the source that must come FIRST.
    let mut sources = vec![crate::pms::Source { sid: crate::plex::current_server(), handle: String::new() }];
    for s in usable {
        let sid = crate::plex::register(&s.machine_id, &s.host, s.port as c_int, &s.token);
        sources.push(crate::pms::Source { sid, handle: s.name.clone() });
    }
    crate::pms::set_sources(sources);
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
        SDL_GL_SetAttribute(A_CTX_PROFILE_MASK, CTX_PROFILE_ES);
        SDL_GL_SetAttribute(A_CTX_MAJOR, 2);
        SDL_GL_SetAttribute(A_CTX_MINOR, 0);
        // full 32-bit RGBA so the video plane shows through
        SDL_GL_SetAttribute(A_RED, 8);
        SDL_GL_SetAttribute(A_GREEN, 8);
        SDL_GL_SetAttribute(A_BLUE, 8);
        SDL_GL_SetAttribute(A_ALPHA, 8);
        SDL_GL_SetAttribute(A_BUFFER_SIZE, 32);
        let win = SDL_CreateWindow(c"plxnative".as_ptr(), 0, 0, SCR_W, SCR_H, SDL_WINDOW_FLAGS);
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

        crate::system::sys_grab_wayland(win);
        crate::gfx::init_gl();
        crate::text::init_text();
        crate::gfx::init_image();
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

        // Install the PMS client (singleton — the read layer AND the playback path), then fetch
        // the catalog. Used by the boot gate and again when a login resolves; host/port fix on
        // first install, a later call just swaps the token (profile switch).
        let install_pms = |host: &str, port: c_int, token: &str| {
            crate::plex::install(host, port, token);
            // a (re)install is a login / profile switch: the browse store must never carry the
            // previous user's cached grid, watched-state angles, or section tabs forward
            crate::browse::reset();
            // …and the hub twin: a FAILED fetch now keeps the catalog it already had (so one
            // wifi hiccup can't blank a populated Home), which makes this the one place that
            // must still wipe it — otherwise a profile switch whose fetch fails would leave the
            // previous user's shelves on screen.
            crate::pms::reset();
            crate::person::reset(); // ditto for an open person page's shelves
            register_extra_servers();
            let nmov = crate::pms::pms_fetch_hubs();
            // section discovery (one small GET) so Home's library tab pills carry real titles
            let nsec = crate::browse::ensure_sections();
            log(&format!("pms: nmovies={nmov} nsections={nsec}"));
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
        enum BootTo {
            Home,
            Login,
            Profiles,
        }
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
        // dev: /tmp/plxnative-profile turns on the per-phase draw profiler (ui::profile) — logs mean
        // ms/frame per draw phase to the event log (GPU-synced, so absolute FPS drops while it's on).
        if crate::dev::flag("profile") {
            crate::ui::profile::set_enabled(true);
        }
        // dev: /tmp/plxnative-noidle turns the whole-frame present gate (ui::idle) OFF, so a still
        // screen goes back to repainting at panel rate. It is a DIAG trigger (see the list above)
        // precisely so an A/B costs one file and does not also change which screen you boot to —
        // and so that if a frame ever looks wrong on the panel, ruling this feature out is one
        // `rm` rather than a redeploy.
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

        let mut held_sym = 0u32;
        let mut held_since = 0u32;
        let mut last_rep = 0u32;
        let mut held_alive = 0u32; // last hardware 0x101 for the held key — a lost-keyup liveness net
        // Scrub state. This Magic Remote emits a HELD key as auto-repeat keydowns (state 0x101,
        // ~50ms apart) followed by ONE keyup on release; a TAP is a lone keydown(0x001)+keyup(0x000).
        // So: a fresh press does the fixed jump; the 0x101 repeats engage the continuous scrub; the
        // keyup is a reliable release. Taps commit on a short debounce so quick taps accumulate.
        let mut scrub_t = 0u32; // last continuous-advance tick
        let mut scrub_dir = 0i32;
        let mut scrub_hold = false; // a 0x101 repeat arrived → continuous accelerating scrub engaged
        let mut scrub_hold_since = 0u32;
        let mut scrub_alive = 0u32; // last held (0x101) event — for the lost-keyup safety commit
        let mut scrub_commit_at = 0u32; // tap released → commit at this tick (0 = none; a new press cancels)
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
        let mut hud_nav = HudNav::HOME;
        // Was the skip pill offering something last frame? Drives the claim-focus-once rule below;
        // without the edge the pill would re-take focus every frame and pin it there.
        // The last SEGMENT the control row offered. Sticky: it is never cleared back to None, so
        // each segment raises the HUD exactly once per playback however often the row flickers.
        let mut last_offer: Option<(crate::metadata::MarkerKind, i64)> = None;
        // Did a stand-in own the control row last frame? The reset below is the EDGE of a stand-in
        // vanishing under the focus ring — see `player_hud::standin_left_the_ring`, which is where
        // that rule is written down and tested.
        let mut ctrl_was_standin = false;
        let mut marker_tried = false; // dev: the /tmp/plxnative-marker jump has been resolved
        // UP-from-the-top explicitly dismisses the HUD even while paused; any other player input
        // clears it. Without this, paused() would force the HUD permanently visible.
        let mut hud_dismissed = false;
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
        let mut dpad_mode = false;
        let mut ptr_drag = false;
        let mut mot_accum = 0.0f32;
        let mut prev_mx = -1.0f32;
        let mut prev_my = -1.0f32;
        let mut last_ptr_motion = 0u32;
        let mut cur_hidden = false;
        // Exclusive route state machine (replaces 5 entangled bools). Overlays live INSIDE
        // Player because they only mean anything during playback; Detail and Player are mutually
        // exclusive. Deleting the old bools makes the compiler flag any un-migrated read.
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
            Player { overlay: Overlay },
        }
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
            /// the Library browse grid on section `sec` (0-based)
            Library(usize),
            /// A page that STACKS — a detail page or a person page. The [`Node`] is BOTH what
            /// mounts at the floor (through the very `enter_node` a BACK pop uses, whose re-open
            /// guard means "the page you asked for is already the one loaded" costs nothing) and
            /// what is then pushed onto the trail. `season` is the one mount a node cannot express:
            /// a SHOW opened with one season already selected, which a node has no field for
            /// because a trail node names a PAGE, not a tab inside one.
            Open { node: Node, season: Option<c_int> },
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
                    Nav::Home { .. } => Some(0),
                    Nav::Library(sec) => Some(sec + 1),
                    Nav::Open { .. } | Nav::Back { .. } => None,
                }
            }
            /// Does the destination draw the shared top bar? Written as a `match` and not a
            /// `matches!` on purpose: a new destination is then a COMPILE ERROR here rather than a
            /// silent `false`, and a silent `false` is a bar that blinks out and back for no reason.
            fn wears_tab_bar(&self) -> bool {
                match self {
                    Nav::Home { .. } | Nav::Library(_) => true,
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
        /// Which routes draw the shared top tab bar — the ONE test behind `ui::nav`'s
        /// continuous-chrome rule. Exhaustive for the same reason [`Nav::wears_tab_bar`] is: a new
        /// screen must not be able to answer this by accident. (`Account`/`ItemMenu over Home` draw
        /// Home underneath, so the bar is on screen there too; Detail and Person do not have one,
        /// which is what makes every transition to or from them fade the bar with the page.)
        fn route_wears_tab_bar(r: Route) -> bool {
            match r {
                Route::Home | Route::Library => true,
                Route::Account => true,
                Route::ItemMenu { over } => matches!(over, MenuHost::Home),
                Route::Login | Route::Profiles | Route::Detail | Route::Person | Route::Player { .. } => false,
            }
        }
        /// The PAGE a trail node names — the ONE Node→[`Route`] mapping in the loop. Both things
        /// that have to know it read it here: [`enter_node`] flips the route through it after
        /// mounting, and [`node_wears_tab_bar`] answers the chrome question by handing it to
        /// [`route_wears_tab_bar`], so a node and the page it mounts can never answer differently.
        ///
        /// A free fn and not `Node::route`, which is what it would rather be: `Route` is a LOCAL of
        /// this run loop (deliberately — the trail decides nothing about screens, and `ui::trail`
        /// cannot see this type), so an inherent `impl` for it would have to sit inside a function
        /// body, which `non_local_definitions` warns about and a future edition rejects outright.
        fn node_route(n: &Node) -> Route {
            match n {
                Node::Home => Route::Home,
                Node::Library => Route::Library,
                Node::Person { .. } => Route::Person,
                Node::Detail { .. } => Route::Detail,
            }
        }
        /// The same question about a TRAIL node — what a BACK's destination wears, peeked before the
        /// pop, and what a forward [`Nav::Open`] is about to put on screen. DERIVED from
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
        /// Only a BACK asks for one. A FORWARD navigation deliberately leaves the page it came from
        /// mounted behind the destination — that is the whole reason `enter_node`'s re-open guard
        /// makes the common pop a route flip rather than a re-fetch.
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
                // Unreachable: `page_of` has already resolved a popover onto the screen it sits on,
                // so neither of these ever arrives here. Listed rather than swept into a `_` so the
                // exhaustiveness above is real.
                Route::Account | Route::ItemMenu { .. } => None,
            }
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
        /// A FORWARD navigation: the page being left stays mounted behind the destination, so there
        /// is nothing to tear down.
        fn nav_to(cur: Route, to: Nav, pending: &mut Option<NavReq>) {
            nav_req(cur, to, None, pending);
        }
        /// Open a stacking page (detail / person) through the transition — the ONE forward entry to
        /// both, so a new way in cannot push without routing or route without pushing. The mount
        /// and the push both happen at the fade floor; see [`nav_req`].
        fn nav_open(cur: Route, node: Node, season: Option<c_int>, pending: &mut Option<NavReq>) {
            nav_req(cur, Nav::Open { node, season }, None, pending);
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

        /// A forward navigation to `rk`'s detail page, as a [`Nav`] destination. The ONE builder,
        /// so the six ways in cannot drift in what they push: the node carries an EMPTY spot, which
        /// is filled in only if the user later navigates deeper off the page (`Trail::set_top_spot`).
        fn to_detail(rk: &str) -> Node {
            Node::Detail { rk: rk.to_string(), spot: Spot::default() }
        }

        /// Open the focused Library card's detail page — the ONE library-card activation
        /// (OK-press commit AND pointer click). Library cards are movies/shows, so activation is
        /// always the detail page (playback then starts from there).
        fn open_library_card(cur: Route, nav: &mut Option<NavReq>) {
            let Some(mm) = crate::ui::library::focused_item() else { return };
            if mm.rk.is_empty() {
                return;
            }
            nav_open(cur, to_detail(&mm.rk), None, nav);
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
            nav_open(cur, to_detail(&mm.rk), None, nav);
        }

        /// Enter `rk`'s detail page with a HARD CUT — no transition. The one caller left is the
        /// `/tmp/plxnative-detail` boot trigger, and the reason is the same one the Library boot
        /// trigger gives: at boot there is no outgoing screen to replace, so a dip would fade the
        /// page up out of nothing and read as a slow app rather than a navigated one. Every
        /// INTERACTIVE way in goes through [`nav_open`] instead.
        fn push_detail(trail: &mut Trail, route: &mut Route, rk: &str) {
            trail.push(to_detail(rk));
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
                Node::Home | Node::Library => {}
                Node::Person { key, guid, name, thumb } => {
                    if crate::person::current().map(|p| p.key != *key).unwrap_or(true) {
                        // `reopen`, NOT `open`: `open` raises the latch the drain below turns into a
                        // route change PLUS a push, so a single BACK would land here and immediately
                        // push back the node it just popped — which reads as "BACK does nothing".
                        crate::ui::person::reopen(key, guid, name, thumb);
                    }
                }
                Node::Detail { rk, spot } => {
                    if crate::ui::detail::mounted_rk() != *rk {
                        // A RESTORE carries a place to put the page back at. A forward navigation
                        // carries the EMPTY spot `to_detail` builds, and must not arm a placement:
                        // `open_rk_at`'s two-stage pump fires when the fetch lands, and on a fresh
                        // open that would yank focus back to the hero from wherever the user had
                        // moved it while waiting. The test is sound because the two branches agree
                        // on the value it splits — an empty spot IS the state `open_rk` mounts in.
                        if *spot == Spot::default() {
                            crate::ui::detail::open_rk(rk);
                        } else {
                            crate::ui::detail::open_rk_at(rk, spot);
                        }
                    }
                }
            }
            *route = node_route(n);
        }

        /// The ONE start-playback ritual (detail OK, home episode OK, and the plxnative-autoplay/
        /// -detailplay/-play dev triggers all share it): arm the resume point BEFORE the first
        /// Load (direct-play av_seek / transcode &offset restart), start the engine, record the
        /// Stop/BACK/EOS return target, reset the HUD focus cursor, and show the HUD. A missed step
        /// here used to silently fork behavior between the interactive and headless paths.
        /// Resume captured at the keypress, applied when the async resolve lands.
        static PENDING_RESUME_NS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

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
                if let Some((show_rk, season)) = crate::metadata::now_playing()
                    .filter(|n| n.is_episode && !n.detail_rk.is_empty())
                    .map(|n| (n.detail_rk.clone(), n.season))
                {
                    crate::ui::detail::open_show_at_episode(&show_rk, season, &crate::route::cur_rk());
                }
                // The player is NOT a trail node — it returns to the page it was started from, and
                // that page may be one nothing ever pushed (a dev trigger, or `home_activate`
                // opening a page under the hood purely to fire its Play). Read AFTER the reveal
                // above, so an auto-advance chain names the SHOW rather than the leaf it ended on.
                trail.ensure_detail(&crate::ui::detail::mounted_rk());
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
        /// drifted (the key path cleared `held_sym`, the pointer path did not). Returns true when
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
            crate::metadata::retire_playing();
            crate::metadata::request_detail(&rk);
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
            crate::metadata::request_detail(&mm.rk);
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
            // a tab pill in the top band: pill 0 is Home — the screen we are already on, so OK on
            // it is a deliberate no-op; 1.. enter that library section's grid. (The grid-card
            // sentinel is rejected by hero_pill_index itself — see its doc comment.)
            if let Some(pill) = crate::ui::home::hero_pill_index(hf) {
                match pill.checked_sub(1) {
                    // that section's grid, through the page cross-fade: `library::enter` and the
                    // route flip both land at the fade floor, while the selection capsule starts
                    // travelling on THIS frame (`nav::view_tab`).
                    Some(sec) => nav_to(*route, Nav::Library(sec), nav),
                    // pill 0 is Home, the screen we are on — still a no-op, EXCEPT that it now
                    // withdraws a section switch that is still fading out: the user changed their
                    // mind inside the 70 ms window, and the capsule springs back on its own.
                    None => {
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
                        if mm.kind == 2 {
                            crate::ui::detail::open_rk_season(&expect, mm.season_index);
                        } else {
                            crate::ui::detail::open_rk_now(&expect); // BLOCKING: `loaded` below gates the play
                        }
                        let loaded = crate::metadata::current().map(|d| d.rk.as_str() == expect).unwrap_or(false);
                        if loaded && crate::ui::detail::on_ok() {
                            start_playback(mt, crate::ui::detail::last_resume_ns(), false, hud_ms, route, played_from_detail, hud_nav);
                        } else {
                            // nothing playable / load failed — land on the page, through the
                            // transition. `season: None`: the mount already happened above (this
                            // arm has to read the loaded item to decide at all), and `enter_node`'s
                            // re-open guard is what turns the floor's mount into a route flip.
                            nav_open(*route, to_detail(&expect), None, nav);
                        }
                    }
                }
            } else if mm.kind == 2 {
                // season: open the SHOW page with that season selected
                nav_open(*route, to_detail(&mm.show_rk), Some(mm.season_index), nav);
            } else if mm.kind == 3 {
                // an episode's page is its show's page — landed on the EPISODE'S season, so the
                // item the hero/tile advertised is actually in view (mirrors the season arm)
                nav_open(*route, to_detail(&mm.show_rk), (mm.season_index > 0).then_some(mm.season_index), nav);
            } else {
                nav_open(*route, to_detail(&rk), None, nav);
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
            crate::ui::item_menu::open_episode(&rk, watched, crate::ui::detail::focused_episode_rect());
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
                    nav_open(*route, to_detail(&rk), None, nav);
                }
                Action::GoToShow(show_rk, season) => {
                    menu_leave(trail, host);
                    // the season arm is BLOCKING (it indexes the loaded show's seasons) — the same
                    // trade `home_activate` makes for a season tile, now paid at the fade floor
                    // where the stall is behind a screen that is already at alpha 0
                    nav_open(*route, to_detail(&show_rk), (season > 0).then_some(season), nav);
                }
                Action::MarkWatched(rk, watched) => {
                    // Same ritual as the detail page's watched toggle: flip it server-side, then
                    // refetch the hubs so Continue Watching reflects it (a watched episode leaves the
                    // shelf; its successor takes the slot). Blocking (~100ms LAN) and deliberately so
                    // — the shelf must not still show the old state under the user's cursor.
                    if watched {
                        crate::plex::client().unscrobble(&rk);
                    } else {
                        crate::plex::client().scrobble(&rk);
                    }
                    // …and when the popover was over the DETAIL page, that page is the surface the
                    // user is watching: re-read the mounted show so the filmstrip's checks, the
                    // season tab's tick and the hero's own toggle all show what was just committed.
                    // The same refresh its hero watched toggle performs — one function, so the two
                    // ways to mark something watched can't leave the page in two different states.
                    // The rk rides along so the row lands back on the episode that changed (see
                    // `detail::KEEP_EP`).
                    if matches!(host, MenuHost::Detail) {
                        crate::ui::detail::refresh_view_state(&rk);
                    }
                    let n = crate::pms::refetch_hubs_reconcile();
                    log(&format!("item menu: rk={rk} watched={} → hubs refreshed ({n} items)", !watched as i32));
                }
                Action::RemoveFromDeck(rk) => {
                    // A HIDE, not a reset: the server keeps the item's `viewOffset`, so the card leaves
                    // the shelf while the resume point survives and playing it again picks up where it
                    // left off. That is why this is NOT `unscrobble`, which would throw the position
                    // away. See `plex::Client::remove_from_continue_watching`.
                    //
                    // Then the same blocking refetch the watched toggle does, for the same reason: the
                    // card sits under the user's cursor and must not still be there after they removed
                    // it. The shelf is sourced from `/hubs/continueWatching`, which is the hub this
                    // action actually affects — built from `/hubs`'s `home.continue` it would come back
                    // still listing the item (see `pms::build_hubs`).
                    let ok = crate::plex::client().remove_from_continue_watching(&rk);
                    let n = crate::pms::refetch_hubs_reconcile();
                    log(&format!("item menu: rk={rk} removed from deck ok={ok} → hubs refreshed ({n} items)"));
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
                    let i = crate::pms::index_of_rk(&rk);
                    if let Some(mm) = (i >= 0).then(|| crate::pms::movie(i as usize)).flatten() {
                        play_item_now(mt, mm, true, HUD_LINGER_MS, route, played_from_detail, hud_nav);
                    }
                }
            }
        }

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
        let mut last_wheel = 0u32;
        // hero click-hold pager: set when a click lands on the chevron, cleared on button-up; the
        // per-frame pump keeps paging while it stays held (the pointer twin of holding RIGHT).
        let mut ptr_hold_pager = 0u32;
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
                    } else if tok == "okdown" || tok == "okup" {
                        // the two halves of OK, so a driver can hold it: `okdown`, wait past
                        // press::LONG_MS, `okup` — the only way to exercise a press-and-hold (and so
                        // the item menu) over the FIFO, since every other token is a tap.
                        remote_synth_key_edge(SDLK_RETURN, 0, tok == "okdown");
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
                if et == SDL_KEYDOWN || et == SDL_KEYUP {
                    let mut hex = String::with_capacity(64);
                    for b in &ev[..32] {
                        hex.push_str(&format!("{b:02x}"));
                    }
                    log(&format!("[{}] key type=0x{et:x} raw={hex}", SDL_GetTicks()));
                }
                if et == SDL_QUIT {
                    running = false;
                } else if et == 0x103 || et == 0x104 {
                    // WILL/DID ENTER BACKGROUND
                    log(&format!("LIFECYCLE: background (playing={})", matches!(route, Route::Player { .. }) as i32));
                    if matches!(route, Route::Player { .. }) && !bg_was_playing {
                        // INTENDED, not published: this snapshot is the only thing the foreground
                        // restore has, and `suspend_bufferfeed` below drops the pending seek target
                        // with the session — so a background that lands while a seek is still
                        // resolving would otherwise save (and restore to) the spot the user just
                        // seeked AWAY from, with nothing left to correct it. See `intended_pos`.
                        bg_pos = intended_pos();
                        bg_was_playing = true;
                        bg_was_paused = paused();
                        scrub_dir = 0;
                        scrub_hold = false;
                        ptr_drag = false;
                        held_sym = 0; // this async route flip must not leave a held key repeating into Home
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
                    // LG SDL fork: state@16, wcode@20, sym@24
                    let state = rd_u32(&ev, 16);
                    let wcode = rd_u32(&ev, 20);
                    let sym = rd_u32(&ev, 24);
                    let isnav = sym == SDLK_LEFT || sym == SDLK_RIGHT || sym == 417 || wcode == 417 || sym == 412 || wcode == 412;
                    if (state & 0xff) != 1 {
                        // key-up = a reliable release (the remote sends exactly one per press).
                        if sym == held_sym {
                            held_sym = 0;
                        }
                        if is_ok(sym) && ok_armed {
                            // OK released over a grid card: start the spring-back; the deferred
                            // activation commits from the per-frame loop once the bounce has shown.
                            crate::ui::press::release(SDL_GetTicks());
                        }
                        if matches!(route, Route::Player { .. }) && scrub_dir != 0 && isnav {
                            if scrub_hold {
                                log(&format!("scrub: keyup commit (held) {}s", scrub() / 1_000_000_000));
                                commit_seek(scrub(), &mut bg_pos); // a held scrub → commit on release
                                scrub_dir = 0;
                                scrub_hold = false;
                            } else {
                                // a tap → commit on a short debounce so quick taps accumulate first
                                scrub_commit_at = SDL_GetTicks().wrapping_add(TAP_COMMIT_MS);
                            }
                        }
                        continue;
                    }
                    if state & 0x100 != 0 {
                        // hardware AUTO-REPEAT (held key): the ONLY thing it drives directly is the
                        // player's continuous accelerating scrub (a ramp, not a discrete move). Every
                        // discrete focus list — home grid, detail, track menu, info, chapters — repeats
                        // through the unified client-side held-key timer below, so hold-to-move feels
                        // identical everywhere and doesn't depend on the remote's hardware repeat delay.
                        let n = SDL_GetTicks();
                        if held_sym != 0 && sym == held_sym {
                            held_alive = n; // heartbeat: this held key's hardware repeats are still arriving
                        }
                        if ok_armed && is_ok(sym) {
                            crate::ui::press::note_alive(n); // OK held: keep the dropped-key-up net honest
                        }
                        if matches!(route, Route::Player { .. }) && hud_nav.focus == 0 && scrub_dir != 0 && isnav {
                            scrub_alive = n;
                            scrub_commit_at = 0; // holding → not a tap
                            if !scrub_hold {
                                scrub_hold = true;
                                scrub_hold_since = n;
                                scrub_t = n;
                                log("scrub: hold engaged (0x101 repeat)");
                            }
                        }
                        continue;
                    }
                    last_input = SDL_GetTicks();
                    hud_dismissed = false; // any fresh key un-dismisses the HUD (UP-hide re-sets it)
                    // a fresh non-OK key (navigation / BACK) while a click is armed aborts the press —
                    // spring the card back to rest WITHOUT activating (you "slid off" the control).
                    if ok_armed && !is_ok(sym) {
                        crate::ui::press::cancel();
                        ok_armed = false;
                    }
                    // LG pointer convention, GLOBAL (every screen, incl. login/picker which
                    // dispatch early below): the first D-pad press dismisses the Magic-Remote
                    // cursor and puts input in D-pad mode; pointer motion brings it back.
                    if sym == SDLK_LEFT || sym == SDLK_RIGHT || sym == SDLK_UP || sym == SDLK_DOWN {
                        if !dpad_mode || !cur_hidden {
                            SDL_webOSCursorVisibility(0);
                        }
                        dpad_mode = true;
                        cur_hidden = true;
                        mot_accum = 0.0;
                    }
                    // onboarding screens (login / who's-watching) own every fresh key — nothing is
                    // behind them, so route the key to the active screen and skip all other handlers.
                    if matches!(route, Route::Login | Route::Profiles) {
                        if matches!(route, Route::Profiles) {
                            if is_ok(sym) && crate::ui::profiles::focus_is_avatar() {
                                // press the roster avatar; the select commits on the spring-back
                                // (route-agnostic press handler). Footer / keypad OK act immediately.
                                crate::ui::press::begin(SDL_GetTicks());
                                ok_armed = true;
                            } else {
                                crate::ui::profiles::key(sym, wcode);
                            }
                        } else {
                            crate::ui::login::key(sym, wcode);
                        }
                        continue;
                    }
                    // the Home profile menu is modal — rows nav, OK commits, BACK closes to Home.
                    if matches!(route, Route::Account) {
                        if is_ok(sym) {
                            match crate::ui::account_menu::on_ok() {
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
                                crate::ui::account_menu::Action::None => route = Route::Home,
                            }
                        } else if is_back(sym, wcode) {
                            crate::ui::account_menu::close();
                            route = Route::Home;
                        } else {
                            crate::ui::account_menu::move_focus(sym as c_int);
                        }
                        continue;
                    }
                    // the press-and-hold item menu is modal too — rows nav, OK commits, BACK closes
                    // back to the shelf (or filmstrip) the card is still sitting on.
                    if let Route::ItemMenu { over } = route {
                        if is_ok(sym) {
                            let act = crate::ui::item_menu::on_ok();
                            route = over.route(); // the dispatch overrides this when it navigates/plays
                            apply_item_action(mt, act, over, &mut route, &mut played_from_detail, &mut trail, &mut hud_nav, &mut nav_pending);
                            held_sym = 0; // an async route flip must not repeat a held key into the next screen
                        } else if is_back(sym, wcode) {
                            crate::ui::item_menu::close();
                            route = over.route();
                        } else if sym == SDLK_UP || sym == SDLK_DOWN {
                            // move once on the fresh press; holding repeats via the shared
                            // client-side timer. Armed ONLY for the two keys the menu acts on, so a
                            // held key it ignores can't sit in `held_sym` driving a per-frame no-op.
                            crate::ui::item_menu::move_focus(sym as c_int);
                            held_sym = sym;
                            held_since = last_input;
                            last_rep = last_input;
                        }
                        continue;
                    }
                    // A playback FAILURE owns the whole frame (`player_hud::transport_hidden`):
                    // `draw_hud` returns before painting anything and the overlay panels below are
                    // gated the same way, so the scrubber, the control row, the bottom tabs and any
                    // panel that happened to be open are all absent from the picture. Nothing that
                    // is not drawn may be driven — the rule `ControlSlot::UpNext` states and
                    // `up_next::card_active` already keeps for the post-play card.
                    //
                    // BACK is the exception, and it is not optional: the read-out's own hint says
                    // "Press BACK to return", and a single screen with every other key swallowed
                    // has nowhere else to go. It closes a panel the failure landed on top of (so the
                    // route agrees with the frame again) and otherwise leaves the player.
                    if matches!(route, Route::Player { .. }) && crate::ui::player_hud::transport_hidden() {
                        if is_back(sym, wcode) {
                            if matches!(modal_of(route), Modal::None) {
                                exit_player(mt, &mut route, played_from_detail, &mut refresh_hubs_at, &mut trail);
                            } else {
                                close_player_overlays();
                                route = Route::Player { overlay: Overlay::None };
                            }
                        }
                        continue;
                    }
                    // the in-player track menu is modal — it swallows every key while open
                    if matches!(route, Route::Player { overlay: Overlay::Menu }) {
                        if sym == SDLK_LEFT || sym == SDLK_RIGHT || sym == SDLK_UP || sym == SDLK_DOWN {
                            // move once on the fresh press; holding repeats via the client-side timer
                            crate::ui::track_menu::move_focus(sym as c_int);
                            held_sym = sym;
                            held_since = last_input;
                            last_rep = last_input;
                            extend_hud(last_input, HUD_MENU_MS);
                        } else if is_ok(sym) {
                            crate::ui::track_menu::on_ok();
                            route = Route::Player { overlay: Overlay::None };
                            extend_hud(last_input, HUD_LINGER_MS);
                        } else if is_back(sym, wcode) {
                            crate::ui::track_menu::close();
                            route = Route::Player { overlay: Overlay::None };
                        }
                        continue;
                    }
                    // the `…` overflow popover is modal too, and has ONE column — so LEFT/RIGHT are
                    // swallowed without moving anything, rather than falling through to the scrubber.
                    if matches!(route, Route::Player { overlay: Overlay::More }) {
                        if sym == SDLK_UP || sym == SDLK_DOWN {
                            crate::ui::more_menu::move_focus(sym as c_int);
                            held_sym = sym;
                            held_since = last_input;
                            last_rep = last_input;
                            extend_hud(last_input, HUD_MENU_MS);
                        } else if is_ok(sym) {
                            apply_more_action(crate::ui::more_menu::on_ok());
                            route = Route::Player { overlay: Overlay::None };
                            extend_hud(last_input, HUD_LINGER_MS);
                        } else if is_back(sym, wcode) {
                            crate::ui::more_menu::close();
                            route = Route::Player { overlay: Overlay::None };
                        }
                        continue;
                    }
                    // the Info card is modal too — it swallows every key while open
                    if matches!(route, Route::Player { overlay: Overlay::Info }) {
                        if sym == SDLK_DOWN && crate::ui::info_panel::at_last() {
                            // past the bottom of the card → drop focus back onto the tabs
                            crate::ui::info_panel::close();
                            route = Route::Player { overlay: Overlay::None };
                            hud_nav.focus = 2;
                            extend_hud(last_input, HUD_LINGER_MS);
                        } else if sym == SDLK_UP || sym == SDLK_DOWN {
                            crate::ui::info_panel::move_focus(sym as c_int);
                            held_sym = sym; // holding repeats via the client-side timer
                            held_since = last_input;
                            last_rep = last_input;
                            extend_hud(last_input, HUD_MENU_MS);
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
                                        exit_player(mt, &mut route, played_from_detail, &mut refresh_hubs_at, &mut trail);
                                        crate::ui::detail::open_rk(&rk);
                                        // A LANDING, not a navigation, so the trail is made to agree
                                        // rather than pushed blindly: the exit above has usually
                                        // already put this very page on top (the show playback
                                        // started from), and `ensure_detail` is a no-op there. It is
                                        // also strictly better than the flag it replaces — a
                                        // Library → detail → play → "Go to Show" now returns to the
                                        // Library instead of to Home.
                                        trail.ensure_detail(&rk);
                                        route = Route::Detail;
                                    }
                                }
                                crate::ui::info_panel::InfoAction::None => {}
                            }
                            // guarded: the GoToDetail arm above set Route::Detail — don't resurrect Player over it
                            if matches!(route, Route::Player { .. }) {
                                route = Route::Player { overlay: Overlay::None };
                            }
                            extend_hud(last_input, HUD_LINGER_MS);
                        } else if is_back(sym, wcode) {
                            crate::ui::info_panel::close();
                            route = Route::Player { overlay: Overlay::None };
                            extend_hud(last_input, HUD_LINGER_MS);
                        }
                        continue;
                    }
                    // the Chapters strip is modal too — LEFT/RIGHT pick, OK seeks, BACK closes
                    if matches!(route, Route::Player { overlay: Overlay::Chapters }) {
                        if sym == SDLK_LEFT || sym == SDLK_RIGHT || sym == 417 || wcode == 417 || sym == 412 || wcode == 412 {
                            let l = sym == SDLK_LEFT || sym == 412 || wcode == 412;
                            let key = if l { SDLK_LEFT } else { SDLK_RIGHT };
                            crate::ui::chapters_panel::move_focus(key as c_int);
                            // hold-repeat via the client-side timer, but only when the direction is a
                            // real SDLK_* (keyup clears held_sym by matching sym; arming it with a
                            // normalized key for the alt-d-pad wcodes would stick on release).
                            if sym == SDLK_LEFT || sym == SDLK_RIGHT {
                                held_sym = sym;
                                held_since = last_input;
                                last_rep = last_input;
                            }
                            extend_hud(last_input, HUD_MENU_MS);
                        } else if is_ok(sym) {
                            let ns = crate::ui::chapters_panel::on_ok();
                            if ns >= 0 {
                                request_seek(ns);
                                if paused() {
                                    set_paused(false);
                                    crate::player::resume(mt);
                                }
                            }
                            route = Route::Player { overlay: Overlay::None };
                            extend_hud(last_input, HUD_LINGER_MS);
                        } else if sym == SDLK_DOWN {
                            // drop focus back onto the tabs below the strip
                            crate::ui::chapters_panel::close();
                            route = Route::Player { overlay: Overlay::None };
                            hud_nav.focus = 2;
                            extend_hud(last_input, HUD_LINGER_MS);
                        } else if is_back(sym, wcode) {
                            crate::ui::chapters_panel::close();
                            route = Route::Player { overlay: Overlay::None };
                            extend_hud(last_input, HUD_LINGER_MS);
                        }
                        continue;
                    }
                    // playing: UP/DOWN move the HUD focus (scrubber ↔ buttons ↔ tabs). The first
                    // press on a hidden HUD just reveals it (focused on the scrubber); pressing UP
                    // with nothing focusable above (the buttons row) hides the HUD again.
                    if matches!(route, Route::Player { .. }) && (sym == SDLK_UP || sym == SDLK_DOWN) {
                        let vis = hud_visible(last_input, hud_until(), paused(), hud_dismissed);
                        let mut hide = false;
                        if !vis {
                            hud_nav.focus = 0; // reveal, on the scrubber
                        } else if sym == SDLK_UP {
                            // vertical stack, top → bottom: control row, scrubber, tabs. Both
                            // marker stand-ins live IN the control row, so the ring is unchanged.
                            match hud_nav.focus {
                                0 => hud_nav.focus = 1, // scrubber → control row
                                2 => hud_nav.focus = 0, // tabs → scrubber
                                _ => {
                                    hide = true; // control row: nothing above → hide the HUD
                                    hud_nav.focus = 0;
                                }
                            }
                        } else {
                            match hud_nav.focus {
                                0 => hud_nav.focus = 2, // scrubber → tabs
                                1 => hud_nav.focus = 0, // buttons → scrubber
                                _ => {}             // tabs: nothing below → stay
                            }
                        }
                        if hud_nav.focus != 0 || hide {
                            // leaving the bar cancels any in-progress scrub preview
                            if scrub() >= 0 {
                                set_scrub(-1);
                            }
                            scrub_dir = 0;
                            scrub_hold = false;
                        }
                        if hide {
                            hud_dismissed = true; // stays hidden even while paused, until the next key
                        } else {
                            extend_hud(last_input, HUD_LINGER_MS);
                        }
                        continue;
                    }
                    if !matches!(route, Route::Player { .. }) && (sym == SDLK_LEFT || sym == SDLK_RIGHT || sym == SDLK_UP || sym == SDLK_DOWN) {
                        if matches!(route, Route::Detail) {
                            crate::ui::detail::move_focus(sym as c_int);
                        } else if matches!(route, Route::Person) {
                            crate::ui::person::move_focus(sym);
                        } else if matches!(route, Route::Library) {
                            crate::ui::library::move_focus(sym);
                        } else if g_snap() < 0.5 {
                            if sym == SDLK_DOWN {
                                if crate::ui::home::hero_focus() < 0 {
                                    crate::ui::home::set_hero_focus(0); // chip → back to the action row
                                } else {
                                    set_snap(1.0);
                                    set_fr(0);
                                }
                            } else if sym == SDLK_LEFT || sym == SDLK_RIGHT {
                                crate::ui::home::home_hero_key(sym); // walk the action row; RIGHT on the chevron pages
                            } else if sym == SDLK_UP {
                                // hero view: UP focuses the profile chip (OK then opens the menu —
                                // the chip is selectable, it no longer springs the menu unbidden)
                                crate::ui::home::set_hero_focus(-1);
                            }
                        } else if sym == SDLK_UP && g_fr() == 0 {
                            set_snap(0.0);
                        } else {
                            crate::ui::home::home_move_focus(sym);
                        }
                        held_sym = sym;
                        held_since = last_input;
                        last_rep = last_input;
                    } else if wcode == 0x1e4 {
                        // LG pointer auto-hidden; ignore
                    } else if is_ok(sym) {
                        if matches!(route, Route::Player { .. }) {
                            let vis = hud_visible(last_input, hud_until(), paused(), hud_dismissed);
                            // A stand-in owns row 1 — activate it. Same value the draw used.
                            if vis && hud_nav.focus == 1 && !ctrl.is_discs() {
                                if activate_ctrl_row(mt, ctrl, &mut route, &mut played_from_detail, &mut refresh_hubs_at, &mut hud_nav, &mut trail) {
                                    held_sym = 0; // async route flip: don't repeat a held key into the next screen
                                }
                            } else if vis && hud_nav.focus == 1 {
                                // …so the discs are what row 1 holds — the complement of the arm
                                // above, and the row's only other occupant.
                                // OK on a control disc opens its panel (Subtitles / Audio / More)
                                if hud_nav.btn == crate::ui::player_hud::BTN_MORE {
                                    crate::ui::more_menu::open();
                                    route = Route::Player { overlay: Overlay::More };
                                } else {
                                    crate::ui::track_menu::open_tab(if hud_nav.btn == 0 { 1 } else { 0 });
                                    route = Route::Player { overlay: Overlay::Menu };
                                }
                            } else if vis && hud_nav.focus == 2 {
                                if hud_nav.tab == 0 {
                                    crate::ui::info_panel::open(); // Info card
                                    route = Route::Player { overlay: Overlay::Info };
                                } else if hud_nav.tab == 1 {
                                    crate::ui::chapters_panel::open(); // Chapters strip
                                    route = Route::Player { overlay: Overlay::Chapters };
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
                            extend_hud(last_input, HUD_LINGER_MS);
                        } else if matches!(route, Route::Library) {
                            // OK on a browse-grid card → the same tvOS press as home's grid;
                            // tabs / toolbar / menus commit immediately inside the screen.
                            if crate::ui::library::focus_is_card() {
                                crate::ui::press::begin(SDL_GetTicks());
                                ok_armed = true;
                            } else if matches!(crate::ui::library::on_ok(), crate::ui::library::Action::GoHome) {
                                nav_to(route, Nav::Home { focus_pill: crate::ui::library::focused_pill() }, &mut nav_pending);
                            }
                        } else if matches!(route, Route::Detail) {
                            // OK on a detail CARD (episode / Related / Cast) → tvOS press: dip now,
                            // commit on the spring-back (the route-agnostic press handler runs on_ok
                            // then). The Play pill, season tabs and About rows activate immediately.
                            if crate::ui::detail::focus_is_card() {
                                crate::ui::press::begin(SDL_GetTicks());
                                ok_armed = true;
                            } else if crate::ui::detail::on_ok() {
                                start_playback(
                                    mt,
                                    crate::ui::detail::last_resume_ns(),
                                    true, // Stop/BACK/EOS returns to this detail page
                                    HUD_LINGER_MS,
                                    &mut route,
                                    &mut played_from_detail,
                                    &mut hud_nav,
                                );
                            }
                        } else if matches!(route, Route::Person) {
                            // every focusable thing on the person page is a poster card → the
                            // same tvOS press as home's grid, committed on the spring-back
                            if crate::ui::person::focus_is_card() {
                                crate::ui::press::begin(SDL_GetTicks());
                                ok_armed = true;
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
                                home_activate(mt, hf, HUD_LINGER_MS, &mut route, &mut played_from_detail, &mut trail, &mut hud_nav, &mut nav_pending);
                            } else {
                                // grid card: tvOS press — dip the focused card now, activate on the
                                // spring-back (committed from the per-frame loop). Nav cancels, so the
                                // focused cell can't move while the press is armed.
                                crate::ui::press::begin(SDL_GetTicks());
                                ok_armed = true;
                            }
                            if !dpad_mode {
                                SDL_webOSCursorVisibility(0);
                                dpad_mode = true;
                                cur_hidden = true;
                            }
                        }
                    } else if wcode == WCODE_PAUSE || sym == 415 || wcode == 415 {
                        // PAUSE
                        if matches!(route, Route::Player { .. }) && !paused() {
                            set_paused(true);
                            crate::player::pause(mt);
                        }
                        extend_hud(last_input, HUD_LINGER_MS);
                    } else if wcode == WCODE_PLAY || sym == 19 || wcode == 19 || sym == 402 || wcode == 402 {
                        // PLAY
                        if !matches!(route, Route::Player { .. }) {
                            if crate::player::start_bufferfeed(mt) {
                                // resuming a suspended session (bg_was_playing) keeps its origin;
                                // a fresh play derives it from the current route. Guards the tiny
                                // bg→fg window where route is still Home but the session came from detail.
                                played_from_detail = if bg_was_playing { played_from_detail } else { matches!(route, Route::Detail) };
                                route = Route::Player { overlay: Overlay::None };
                            }
                            set_paused(false);
                            if !dpad_mode {
                                SDL_webOSCursorVisibility(0);
                                dpad_mode = true;
                                cur_hidden = true;
                            }
                        } else if paused() {
                            set_paused(false);
                            crate::player::resume(mt);
                        }
                        extend_hud(last_input, HUD_LINGER_MS);
                    } else if matches!(route, Route::Player { .. }) && (sym == WCODE_STOP || wcode == WCODE_STOP) {
                        // Stop
                        exit_player(mt, &mut route, played_from_detail, &mut refresh_hubs_at, &mut trail);
                    } else if matches!(route, Route::Player { .. })
                        && (sym == SDLK_LEFT || sym == SDLK_RIGHT || sym == 417 || wcode == 417 || sym == 412 || wcode == 412)
                    {
                        if !cur_hidden {
                            SDL_webOSCursorVisibility(0);
                            cur_hidden = true;
                        }
                        if ptr_drag {
                            ptr_drag = false;
                            set_scrub(-1);
                        }
                        let fwd = sym == SDLK_RIGHT || sym == 417 || wcode == 417;
                        let vis = hud_visible(last_input, hud_until(), paused(), hud_dismissed);
                        extend_hud(last_input, HUD_LINGER_MS);
                        if !vis {
                            hud_nav.focus = 0; // first LEFT/RIGHT reveals the HUD on the scrubber
                        }
                        if hud_nav.focus == 1 {
                            // the row's occupant says how many items it has — no magic pin
                            hud_nav.btn = (hud_nav.btn + if fwd { 1 } else { -1 }).clamp(0, ctrl.items() - 1);
                        } else if hud_nav.focus == 2 {
                            let max_tab = if crate::ui::chapters_panel::has_chapters() { 1 } else { 0 };
                            hud_nav.tab = (hud_nav.tab + if fwd { 1 } else { -1 }).clamp(0, max_tab);
                        } else if dur() > 0 {
                            // scrubber focus, FRESH press (0x001): the fixed 10s jump. A held key's
                            // 0x101 repeats (handled above) then engage the continuous scrub; the
                            // keyup commits. Quick re-taps before scrub_commit_at accumulate.
                            let cap = dur() - 3 * 1_000_000_000;
                            scrub_commit_at = 0; // more input → cancel a pending tap commit
                            scrub_alive = last_input;
                            if scrub_dir == 0 && scrub() < 0 {
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
                            if !scrub_hold {
                                let mut s = scrub().max(0) + if fwd { SCRUB_STEP_NS } else { -SCRUB_STEP_NS };
                                if s < 0 {
                                    s = 0;
                                }
                                if cap > 0 && s > cap {
                                    s = cap;
                                }
                                set_scrub(s);
                            }
                            scrub_dir = if fwd { 1 } else { -1 };
                        }
                    } else if matches!(route, Route::Library)
                        && (sym == SDLK_PAGEUP || sym == SDLK_PAGEDOWN || wcode == WCODE_CH_UP || wcode == WCODE_CH_DOWN)
                    {
                        // CH▲/CH▼ page the browse grid a screenful of rows per press
                        let up = sym == SDLK_PAGEUP || wcode == WCODE_CH_UP;
                        crate::ui::library::page(if up { -1 } else { 1 });
                    } else if is_back(sym, wcode) {
                        // webOS BACK: this Magic Remote sends wcode 482 (0x1E2); 461 kept for others.
                        // Back stack: player -> the TRAIL (detail/person, at any depth) -> library
                        // -> grid -> hero -> exit. Inside the Library, BACK first walks menu -> tab
                        // bar (library::back), THEN leaves to Home. The ORDER is unchanged; what
                        // changed is that detail/person pop a real trail (`ui::trail`) instead of
                        // consulting two booleans that had one slot per screen KIND and so could not
                        // describe a detail page standing on another one.
                        //
                        // A BACK inside the page fade's 70 ms window WITHDRAWS the transition rather
                        // than acting on a screen that is already half gone: the request is at most
                        // four frames old and nothing has changed yet, so it can still be un-asked.
                        // `nav_cancel` refuses once the swap has happened, and then this is an
                        // ordinary BACK on the NEW screen — the press is never dropped, only ever
                        // spent on exactly one of the two. (It matters most at Home's root, where
                        // "what BACK would otherwise do" is exit the app.)
                        if nav_cancel(route, &mut nav_pending) {
                        } else if matches!(route, Route::Player { .. }) {
                            exit_player(mt, &mut route, played_from_detail, &mut refresh_hubs_at, &mut trail);
                        } else if matches!(route, Route::Detail | Route::Person) {
                            // The two stacking screens, through the page transition. All three
                            // halves of the pop — the outgoing page's teardown, the trail move and
                            // the re-entry — land together at the fade FLOOR (`nav_back`), because
                            // a pop is always all three and splitting them across the 70 ms window
                            // is how you get a page blanking during its own fade-out or a second
                            // BACK popping a node whose page is still on screen. Only the PEEK
                            // (does the page underneath wear the tab bar?) happens here.
                            nav_back(route, &trail, &mut nav_pending);
                        } else if matches!(route, Route::Library) {
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
                                nav_to(route, Nav::Home { focus_pill: pill }, &mut nav_pending);
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
                            running = false;
                        }
                    }
                } else if et == SDL_MOUSEMOTION {
                    last_input = SDL_GetTicks();
                    last_ptr_motion = last_input;
                    cur_hidden = false;
                    let (mx, my) = ptr_xy(&ev);
                    if prev_mx >= 0.0 {
                        mot_accum += (mx - prev_mx).abs() + (my - prev_my).abs();
                    }
                    prev_mx = mx;
                    prev_my = my;
                    if matches!(route, Route::Player { .. }) {
                        hud_dismissed = false;
                        extend_hud(last_input, HUD_LINGER_MS);
                        if ptr_drag && dur() > 0 {
                            let frac = crate::ui::player_hud::scrub_frac_x(mx) as f64;
                            set_scrub((frac * dur() as f64) as i64);
                        }
                        continue;
                    }
                    if dpad_mode {
                        if mot_accum < 120.0 {
                            continue;
                        }
                        dpad_mode = false;
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
                        let hud_vis = hud_visible(last_input, hud_until(), paused(), hud_dismissed);
                        hud_dismissed = false;
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
                                hud_nav.focus = 1;
                                hud_nav.btn = ctrl_click.unwrap_or(0);
                                activate_ctrl_row(mt, ctrl, &mut route, &mut played_from_detail, &mut refresh_hubs_at, &mut hud_nav, &mut trail);
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
                                    hud_nav.focus = 1;
                                    hud_nav.btn = idx;
                                } else if let Some(frac) = on_scrub {
                                    let mut t = (frac as f64 * dur() as f64) as i64;
                                    let cap = dur() - 3 * 1_000_000_000;
                                    if cap > 0 && t > cap {
                                        t = cap;
                                    }
                                    set_scrub(t);
                                    ptr_drag = true;
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
                            // the centered tab pills work from BOTH hero and grid views. Home
                            // (pill 0) is the screen we are on, so a click there just parks focus
                            // on it — in hero view, which is where the band's focus is visible.
                            if let Some(sec) = i.checked_sub(1) {
                                nav_to(route, Nav::Library(sec), &mut nav_pending);
                            } else if nav_cancel(route, &mut nav_pending) {
                                // a section switch still fading out, taken back — the key twin's rule
                            } else if crate::ui::home::snap_pos() < 0.5 {
                                crate::ui::home::set_hero_focus(crate::ui::home::hero_focus_for_pill(0));
                            }
                        } else if crate::ui::home::snap_pos() < 0.5 {
                            // hero visible: clicks act on the action row via the ONE activation;
                            // holding the click on the chevron keeps paging (see the per-frame pump)
                            let b = crate::ui::home::hero_button_at(cx, cy);
                            if b >= 0 {
                                crate::ui::home::set_hero_focus(b);
                                home_activate(mt, b, HUD_LINGER_MS, &mut route, &mut played_from_detail, &mut trail, &mut hud_nav, &mut nav_pending);
                                if b == 2 {
                                    ptr_hold_pager = last_input;
                                }
                            }
                        } else if crate::ui::home::home_card_click(cx, cy) {
                            // grid card: click = OK (play a Continue-Watching tile / open detail)
                            home_activate(mt, c_int::MIN, HUD_LINGER_MS, &mut route, &mut played_from_detail, &mut trail, &mut hud_nav, &mut nav_pending);
                        }
                    } else if matches!(route, Route::Library) {
                        let (cx, cy) = ptr_xy(&ev);
                        match crate::ui::library::click(cx, cy) {
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
                                    &mut hud_nav,
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
                        apply_item_action(mt, act, over, &mut route, &mut played_from_detail, &mut trail, &mut hud_nav, &mut nav_pending);
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
                    ptr_hold_pager = 0; // releasing the click stops the hero click-hold pager
                    if ptr_drag {
                        ptr_drag = false;
                        if scrub() >= 0 {
                            commit_seek(scrub(), &mut bg_pos);
                        }
                        extend_hud(last_input, HUD_LINGER_MS);
                    }
                } else if et == SDL_MOUSEWHEEL {
                    last_input = SDL_GetTicks();
                    if last_input.wrapping_sub(last_wheel) > 250 {
                        last_wheel = last_input;
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
                        }
                    }
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
                            crate::metadata::load_detail_now(&pmm.rk);
                        }
                    }
                    let fd = matches!(route, Route::Detail);
                    start_playback(mt, 0, fd, HUD_HEADLESS_MS, &mut route, &mut played_from_detail, &mut hud_nav);
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
                // section N (empty file = 0) — the deterministic entry for the library FPS scenes.
                if let Some(s) = crate::dev::read("library") {
                    let sec = s.parse::<usize>().unwrap_or(0);
                    // A HARD CUT, deliberately: a transition means "this screen replaced that one",
                    // and at boot there is no outgoing screen to replace. The fade-in that IS wanted
                    // here belongs to the screen (`enter`'s own `xf().mount()`); dipping the whole
                    // page would fade the tab bar up from nothing too, which reads as a slow app
                    // rather than a navigated one.
                    crate::ui::library::enter(sec, crate::ui::library::Arrival::Cut);
                    route = Route::Library;
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
                        let idx = crate::pms::index_of_rk(rk);
                        // BLOCKING both ways: the sub-triggers below replay move_focus/on_ok in
                        // THIS frame, and they walk sections() — which is hero-only until the
                        // item lands.
                        if idx >= 0 {
                            crate::ui::detail::open(idx);
                        } else {
                            crate::ui::detail::open_rk_now(rk);
                        }
                        push_detail(&mut trail, &mut route, rk);
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
                                &mut hud_nav,
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
                        crate::metadata::load_detail_now(rk); // fetch ANY rk (movie/show/episode)
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
                                crate::route::request_play(rk, &part, &vc, &ac, &title, "");
                                let resume = crate::metadata::resume_ns(resume_ms, dur_ms);
                                let fd = matches!(route, Route::Detail);
                                start_playback(mt, resume, fd, HUD_HEADLESS_MS, &mut route, &mut played_from_detail, &mut hud_nav);
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
                    hud_nav.focus = 2;
                    hud_nav.tab = 0;
                    set_hud(now + HUD_HEADLESS_MS);
                }
                // dev: /tmp/plxnative-chapters opens the Chapters strip once (headless capture)
                if crate::dev::flag("chapters") {
                    crate::ui::chapters_panel::open();
                    route = Route::Player { overlay: Overlay::Chapters };
                    hud_nav.focus = 2;
                    hud_nav.tab = 1;
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
                finish_playback(mt, &mut route, &mut played_from_detail, &mut refresh_hubs_at, &mut hud_nav, &mut trail);
                held_sym = 0; // async route flip: don't repeat a still-held key into detail/home
            }
            // Up Next countdown elapsed → start the queued episode on its own. Beside the EOS
            // handoff so the whole auto-advance chain reads in one place.
            if matches!(route, Route::Player { .. }) && crate::ui::up_next::expired(now) {
                if !play_up_next(mt, HUD_LINGER_MS, &mut route, &mut played_from_detail, &mut hud_nav) {
                    crate::ui::up_next::cancel(); // nothing queued after all — don't re-fire
                }
                held_sym = 0;
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
            if ptr_hold_pager != 0
                && now.wrapping_sub(ptr_hold_pager) > 450
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
            if held_sym != 0 && now.wrapping_sub(held_since) > 500 && now.wrapping_sub(held_alive) > 350 {
                held_sym = 0;
            }
            // client-side long-press repeat — the ONE hold-to-move path for every discrete focus list
            // (home grid, detail, track menu, info card, chapters). Driven by a held-key timer so it's
            // identical everywhere and independent of the remote's hardware auto-repeat delay. held_sym
            // is armed by each view's fresh-press handler (always a standard SDLK_*) and cleared on the
            // keyup. The player scrubber is deliberately excluded — holding it runs the continuous scrub.
            if held_sym != 0 && now.wrapping_sub(held_since) > 380 && now.wrapping_sub(last_rep) > 110 {
                last_rep = now;
                match route {
                    Route::Home if g_snap() > 0.5 => crate::ui::home::home_move_focus(held_sym),
                    Route::Home => crate::ui::home::home_hero_key(held_sym), // hero view: hold LEFT/RIGHT pages the billboard
                    Route::ItemMenu { .. } => crate::ui::item_menu::move_focus(held_sym as c_int),
                    Route::Library => crate::ui::library::move_focus(held_sym),
                    Route::Detail => crate::ui::detail::move_focus(held_sym as c_int),
                    Route::Player { overlay: Overlay::Menu } => {
                        crate::ui::track_menu::move_focus(held_sym as c_int);
                        extend_hud(now, HUD_MENU_MS);
                    }
                    Route::Player { overlay: Overlay::More } => {
                        crate::ui::more_menu::move_focus(held_sym as c_int);
                        extend_hud(now, HUD_MENU_MS);
                    }
                    Route::Player { overlay: Overlay::Info } => {
                        crate::ui::info_panel::move_focus(held_sym as c_int);
                        extend_hud(now, HUD_MENU_MS);
                    }
                    Route::Player { overlay: Overlay::Chapters } => {
                        crate::ui::chapters_panel::move_focus(held_sym as c_int);
                        extend_hud(now, HUD_MENU_MS);
                    }
                    _ => {}
                }
            }
            // keep the HUD alive while the track menu / Info card / Chapters strip is open
            if matches!(route, Route::Player { overlay } if overlay != Overlay::None) {
                extend_hud(now, HUD_LINGER_MS);
            }
            // scrub: continuous accelerating advance while a key is held (scrub_hold set by 0x101).
            if scrub_dir != 0 && scrub_hold && scrub() >= 0 && !ptr_drag {
                let held = now.wrapping_sub(scrub_hold_since) as f32 / 1000.0;
                let speed = (SCRUB_BASE + SCRUB_ACCEL * held).min(SCRUB_MAX);
                let mut sdt = now.wrapping_sub(scrub_t) as f32 / 1000.0;
                if sdt > 0.1 {
                    sdt = 0.1;
                }
                let mut s = scrub() + (scrub_dir as f64 * speed as f64 * sdt as f64 * 1e9) as i64;
                let cap = dur() - 3 * 1_000_000_000;
                if s < 0 {
                    s = 0;
                }
                if cap > 0 && s > cap {
                    s = cap;
                }
                set_scrub(s);
                extend_hud(now, HUD_LINGER_MS);
                scrub_t = now;
                // lost-keyup safety: commit if the 0x101 repeats stop without a keyup
                if now.wrapping_sub(scrub_alive) > SCRUB_LOST_MS {
                    commit_seek(scrub(), &mut bg_pos);
                    scrub_dir = 0;
                    scrub_hold = false;
                }
            }
            // tap release debounce: commit the accumulated jump(s) once no further tap arrives
            if scrub_commit_at != 0 && now.wrapping_sub(scrub_commit_at) < 0x8000_0000 {
                if scrub() >= 0 {
                    log(&format!("scrub: tap commit {}s", scrub() / 1_000_000_000));
                    commit_seek(scrub(), &mut bg_pos);
                } else {
                    set_scrub(-1);
                }
                scrub_dir = 0;
                scrub_hold = false;
                scrub_commit_at = 0;
            }
            // Focus follows the control row's OCCUPANT, on both edges. Driven by slot identity
            // rather than a "was something shown" bool, because the two edges have different jobs
            // and the previous bool implemented neither of the ones its comment promised.
            if matches!(route, Route::Player { overlay: Overlay::None }) {
                // Keyed on the SEGMENT, not the slot, and `last_offer` is only ever advanced to a
                // real offer — never cleared back to None. `active_marker` is gated on `is_playing`,
                // so a momentary drop out of Playing mid-segment reads as "no segment" and flips the
                // row to the discs and back; keyed on the slot that round trip looked like a new
                // offer and re-raised the HUD over an intro the user was simply watching.
                let offer = ctrl.offer();
                let fresh = offer.is_some() && offer != last_offer;
                if offer.is_some() {
                    last_offer = offer;
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
                    if hud_nav.focus == 0 {
                        hud_nav.focus = 1;
                        // …on the row's PRIMARY, which is item 0 for a Skip pill and the RIGHT-hand
                        // one for Up Next. Not cosmetic there: the countdown's cancel rule is a
                        // steady state, so parking on Watch Credits would disarm the timer on the
                        // frame after it armed and the tile would never count down at all.
                        hud_nav.btn = ctrl.primary_btn();
                    }
                } else if crate::ui::player_hud::standin_left_the_ring(ctrl_was_standin, ctrl, hud_nav.focus == 1) {
                    // The stand-in went away under the focus ring. Without this the row swaps back
                    // to the discs with focus still on it and `btn` still 0, so the next OK opened
                    // the SUBTITLES menu instead of toggling pause — exactly the bug class HudNav's
                    // own doc says it exists to kill. Strictly the EDGE: as a steady state it also
                    // fired on a user who walked UP to the discs on purpose, yanking the ring back
                    // the same frame and making OK on a disc unreachable by remote.
                    hud_nav = HudNav::HOME;
                }
                ctrl_was_standin = !ctrl.is_discs();
                // While the countdown runs, hold the HUD up — a timer nobody can see is a cut to
                // the next episode out of nowhere. `hud_dismissed` has to clear with it, not just
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
                    if crate::ui::up_next::countdown_may_run(hud_nav.focus == 1, hud_nav.btn) {
                        hud_dismissed = false;
                        extend_hud(now, HUD_LINGER_MS);
                    } else {
                        crate::ui::up_next::cancel();
                    }
                }
            }
            // when the HUD auto-hides, park focus back on the scrubber so the next reveal is clean
            if matches!(route, Route::Player { .. }) && !hud_visible(now, hud_until(), paused(), hud_dismissed) {
                hud_nav = HudNav::HOME;
            }
            // hide the idle pointer during playback
            if matches!(route, Route::Player { .. }) && !cur_hidden && !ptr_drag && last_ptr_motion != 0 && now.wrapping_sub(last_ptr_motion) > 3000 {
                SDL_webOSCursorVisibility(0);
                cur_hidden = true;
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
            crate::ui::press::tick(now, dt);
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
                            home_activate(mt, c_int::MIN, HUD_LINGER_MS, &mut route, &mut played_from_detail, &mut trail, &mut hud_nav, &mut nav_pending);
                        }
                        Route::Library => open_library_card(route, &mut nav_pending),
                        Route::Detail => {
                            if crate::ui::detail::on_ok() {
                                start_playback(mt, crate::ui::detail::last_resume_ns(), true, HUD_LINGER_MS, &mut route, &mut played_from_detail, &mut hud_nav);
                            }
                        }
                        Route::Person => {
                            if matches!(crate::ui::person::on_ok(), crate::ui::person::Action::Card) {
                                open_person_card(route, &mut nav_pending);
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
            if let Some(rk) = crate::ui::detail::take_open_request() {
                if matches!(route, Route::Detail) {
                    nav_open(route, to_detail(&rk), None, &mut nav_pending);
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
                        nav_open(route, to_detail(&nav_osc_rk), None, &mut nav_pending)
                    }
                    Route::Detail => nav_back(route, &trail, &mut nav_pending),
                    Route::Home => nav_to(route, Nav::Library(0), &mut nav_pending),
                    // pill 1 is that same first section: Home comes back in the hero view with the
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
                        Nav::Library(sec) => {
                            // every teleport `enter` performs (the store swap, `restore_view`'s
                            // scroll jump, the focus band) happens HERE, at alpha 0, off screen
                            crate::ui::library::enter(sec, crate::ui::library::Arrival::Faded);
                            // the grid sits directly on Home, and `home_activate` truncated the
                            // trail to that root on the press frame
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
                            if let (Node::Detail { rk, .. }, Some(s)) = (&node, season) {
                                crate::ui::detail::open_rk_season(rk, s);
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
                crate::ui::home::home_update(dt);
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
                    route = Route::Player { overlay: Overlay::None };
                }
            }
            // Async detail load: install the worker's item into CURRENT. Route-unconditional for
            // the same reason as pump_play — play_item_now requests a detail from Home and flips
            // straight to the player, so a Detail-gated pump would never land it.
            if crate::metadata::pump_detail() {
                crate::ui::idle::invalidate(); // a detail landing rewrites the page under us
            }
            crate::posters::poster_pump(3); // invalidates from inside, per texture installed
            let fd_pc_pump = if framedrop_on { SDL_GetPerformanceCounter() } else { 0 };

            let player = matches!(route, Route::Player { .. });
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
                crate::ui::guard(|| {
                    if player {
                        crate::system::clear_opaque_region();
                        glClearColor(0.0, 0.0, 0.0, 0.0);
                        glClear(GL_COLOR_BUFFER_BIT);
                        let hud_up = hud_visible(now, hud_until(), paused(), hud_dismissed);
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
                            crate::ui::player_hud::draw_hud(ctrl, busy, hud_nav.focus, hud_nav.btn, hud_nav.tab, now, !matches!(route, Route::Player { overlay: Overlay::Info | Overlay::Chapters }));
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
                        } else {
                            crate::ui::home::home_draw();
                        }
                        if matches!(route, Route::Account) {
                            crate::ui::account_menu::draw(); // profile popover over Home
                        }
                        if matches!(route, Route::ItemMenu { .. }) {
                            crate::ui::item_menu::draw(); // press-and-hold card menu, over the live screen
                        }
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
                            let loop_col = [0.4f32, 1.0, 0.55, 1.0];
                            crate::gfx::draw_number(loop_shown, SCR_W as f32 - 70.0, 64.0, 46.0, loop_col.as_ptr());
                        }
                    }
                    crate::ui::anim::draw_overlay(); // dev diagnostic overlay (all routes)
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
                SDL_GL_SwapWindow(win);
                fd_pc_swap = if framedrop_on { SDL_GetPerformanceCounter() } else { 0 };
                // Inside the gate: `frame_end` is the end of a DRAWN frame. Counting frames the
                // idle gate skipped would pace the profiler's once-per-N-frames log off frames
                // that ran no phases at all.
                crate::ui::profile::frame_end();
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
                Route::Player { .. } => "player",
                _ => "home",
            };
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
                        "FRAMEDROP total={total:.1} pump={pump:.1} draw={draw:.1} cap={cap:.1} swap={swap:.1} up={up} px={px} cards={cards} off={cards_off} route={rn} snap={:.2}",
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
                if framedrop_on {
                    log(&format!("loop={loop_shown} route={rn}{ov}{pos} fps={pres} worstframe={fd_worst:.1}ms"));
                    fd_worst = 0.0;
                } else {
                    log(&format!("loop={loop_shown} route={rn}{ov}{pos} fps={pres}"));
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
