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
// keysyms (scancode | SDLK_SCANCODE_MASK, or ASCII)
const SDLK_LEFT: u32 = 80 | (1 << 30);
const SDLK_RIGHT: u32 = 79 | (1 << 30);
const SDLK_UP: u32 = 82 | (1 << 30);
const SDLK_DOWN: u32 = 81 | (1 << 30);
const SDLK_RETURN: u32 = 13;
const SDLK_KP_ENTER: u32 = 88 | (1 << 30);
const SDLK_SELECT: u32 = 77 | (1 << 30);
const SDLK_ESCAPE: u32 = 27;
const SCR_W: c_int = 1920;
const SCR_H: c_int = 1080;
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
    fn SDL_PollEvent(event: *mut c_void) -> c_int;
    fn SDL_GL_SwapWindow(win: *mut c_void);
    fn SDL_Quit();
    fn SDL_webOSCursorVisibility(visible: c_int) -> c_int;
    fn glGetString(name: c_uint) -> *const c_char;
    fn glViewport(x: c_int, y: c_int, w: c_int, h: c_int);
    fn glClearColor(r: f32, g: f32, b: f32, a: f32);
    fn glClear(mask: c_uint);
}

fn log(m: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/poc-events.log") {
        let _ = writeln!(f, "{m}");
    }
}

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
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/poc-crash.log") {
            let _ = writeln!(f, "{line}");
        }
        default(info); // preserve default behaviour (stderr -> poc-stderr.log)
    }));
}

#[inline]
fn rd_u32(ev: &[u8], off: usize) -> u32 {
    u32::from_ne_bytes([ev[off], ev[off + 1], ev[off + 2], ev[off + 3]])
}
#[inline]
fn rd_i32(ev: &[u8], off: usize) -> i32 {
    i32::from_ne_bytes([ev[off], ev[off + 1], ev[off + 2], ev[off + 3]])
}

// ui focus state lives in ui::home; reach it through its accessors
#[inline]
fn g_fr() -> c_int { crate::ui::home::row() }
#[inline]
fn g_fc() -> c_int { crate::ui::home::col() }
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
/// the transport HUD is shown while its timer is live OR playback is paused, unless the user
/// explicitly dismissed it (UP from the top row) — the dismiss holds until the next key.
#[inline]
fn hud_shown(now: u32, until: u32, is_paused: bool, dismissed: bool) -> bool {
    (now < until || is_paused) && !dismissed
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
#[inline]
fn frames() -> i32 { crate::player::frames() }
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
/// resume position (ns) for a directly-played item: only if past 10s and before 95% of
/// its duration (Plex's in-progress rule), else 0 (start from the beginning).
fn resume_ns(resume_ms: i64, dur_ns: i64) -> i64 {
    let dur_ms = dur_ns / 1_000_000;
    if resume_ms > 10_000 && (dur_ms <= 0 || (resume_ms as f64) < 0.95 * dur_ms as f64) {
        resume_ms * 1_000_000
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn plex_run(
    pms_host: *const c_char,
    pms_port: c_int,
    pms_token: *const c_char,
    demo_url: *const c_char,
) -> c_int {
    install_panic_logger();
    unsafe {
        SDL_SetMainReady();
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
        let win = SDL_CreateWindow(c"plexpoc".as_ptr(), 0, 0, SCR_W, SCR_H, SDL_WINDOW_FLAGS);
        if win.is_null() {
            log("CreateWindow failed");
            return 1;
        }
        let ctx = SDL_GL_CreateContext(win);
        if ctx.is_null() {
            log("GL ctx failed");
            return 1;
        }
        SDL_GL_SetSwapInterval(1);
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

        // Effective PMS token: the one compiled into the binary (owner), unless the dev trigger
        // /tmp/poc-token holds an override — used by the regression harness to run as the Guest
        // user so test playback/scrobbles land on Guest's history, not the real account. The
        // value is NEVER logged (only that an override is in effect).
        let eff_token = {
            let compiled = std::ffi::CStr::from_ptr(pms_token).to_string_lossy().into_owned();
            match std::fs::read_to_string("/tmp/poc-token") {
                Ok(s) if !s.trim().is_empty() => {
                    log("token: using /tmp/poc-token override (test/guest user)");
                    s.trim().to_owned()
                }
                _ => compiled,
            }
        };
        // Install the process-wide typed Plex client (the read data layer — pms/metadata/
        // posters — reads this singleton instead of threading host/port/token). Must precede
        // the first fetch. route::CFG keeps its own copy for the playback layer (engine's
        // timeline reporter still reads route::config()).
        let host_s = std::ffi::CStr::from_ptr(pms_host).to_string_lossy().into_owned();
        crate::plex::init(&host_s, pms_port, &eff_token);

        // fetch the catalog once, then spawn the poster workers
        let nmov = crate::pms::pms_fetch_hubs();
        log(&format!("pms: nmovies={nmov}"));
        crate::posters::posters_init();
        crate::route::set_config(
            &host_s,
            pms_port,
            &eff_token,
            &std::ffi::CStr::from_ptr(demo_url).to_string_lossy(),
        );

        crate::ui::home::home_init();
        crate::player::acb_init();
        crate::ff::boot(); // FFmpeg version smoke test + optional /tmp/poc-ffprobe ABI probe
        // dev: the animation-diagnostic overlay is OFF by default; /tmp/poc-anim enables it (its
        // trace goes to /tmp/poc-anim.log, a separate stream from the main event log)
        if std::path::Path::new("/tmp/poc-anim").exists() {
            crate::ui::anim::set_enabled(true);
        }
        // dev: /tmp/poc-profile turns on the per-phase draw profiler (ui::profile) — logs mean
        // ms/frame per draw phase to the event log (GPU-synced, so absolute FPS drops while it's on).
        if std::path::Path::new("/tmp/poc-profile").exists() {
            crate::ui::profile::set_enabled(true);
        }

        let mut last_input = SDL_GetTicks();
        let t0 = SDL_GetTicks();
        let mut fps_t = t0;
        let mut frames_ct = 0i32;
        let mut fps_shown = 0i32;
        let mut running = true;

        let mut held_sym = 0u32;
        let mut held_since = 0u32;
        let mut last_rep = 0u32;
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
        // player HUD focus: 0 = scrubber, 1 = right buttons (Subtitles/Audio), 2 = bottom tabs.
        let mut hud_focus = 0i32;
        let mut hud_btn = 0i32; // 0 = Subtitles, 1 = Audio (within the buttons row)
        let mut hud_tab = 0i32; // 0 = Info, 1 = Chapters (within the tabs row)
        // UP-from-the-top explicitly dismisses the HUD even while paused; any other player input
        // clears it. Without this, paused() would force the HUD permanently visible.
        let mut hud_dismissed = false;
        let mut menu_rep = 0u32; // track-menu scroll throttle (a held UP/DOWN repeats via 0x101)
        // scrub tuning: a press jumps SCRUB_STEP_NS; holding engages a continuous scrub ramping
        // SCRUB_BASE→SCRUB_MAX (playback-seconds per real-second).
        const SCRUB_STEP_NS: i64 = 10_000_000_000; // 10s per press
        const SCRUB_BASE: f32 = 10.0;
        const SCRUB_ACCEL: f32 = 45.0; // added per second of hold
        const SCRUB_MAX: f32 = 140.0;
        const TAP_COMMIT_MS: u32 = 240; // tap released → commit after this (further taps accumulate)
        const SCRUB_LOST_MS: u32 = 400; // holding but no repeat this long → lost keyup → commit
        const MENU_REPEAT_MS: u32 = 110; // held-menu scroll cadence (0x101 arrives faster; throttle)
        let mut bg_was_playing = false;
        let mut bg_was_paused = false;
        let mut bg_pos = 0i64;
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
        }
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Route {
            Home,
            Detail,
            Player { overlay: Overlay },
        }
        let mut route = Route::Home;
        // Return target for playback started from a detail page: Stop/BACK/EOS from such a session
        // returns to that detail page, else home. Kept OUTSIDE Route (like bg_was_playing keeps the
        // suspended session) — it's navigation history, not the current node, and Route makes
        // Detail/Player exclusive so it can't be encoded there.
        let mut played_from_detail = false;

        let mut auto_tried = false;
        let mut grid_tried = false;
        let mut seek_tried = false;
        let mut detail_tried = false;
        let mut play_tried = false;
        let mut menu_tried = false;
        let mut menupick_tried = false;
        let mut pause_tried = false;
        let mut prev = 0u32;
        let mut last_wheel = 0u32;

        let mut ev = [0u8; 128];
        while running {
            crate::system::ls2_pump();
            while SDL_PollEvent(ev.as_mut_ptr() as *mut c_void) != 0 {
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
                        bg_pos = playpos();
                        bg_was_playing = true;
                        bg_was_paused = paused();
                        scrub_dir = 0;
                        scrub_hold = false;
                        ptr_drag = false;
                        set_scrub(-1);
                        crate::player::suspend_bufferfeed(); // preserve the session for a clean fg reload
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
                            let started = crate::player::start_bufferfeed();
                            if started {
                                route = Route::Player { overlay: Overlay::None };
                                set_hud(SDL_GetTicks() + 4500);
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
                        // hardware AUTO-REPEAT (held key): drive the continuous scrub / menu scroll.
                        let n = SDL_GetTicks();
                        if matches!(route, Route::Player { overlay: Overlay::Menu }) {
                            if (sym == SDLK_UP || sym == SDLK_DOWN) && n.wrapping_sub(menu_rep) > MENU_REPEAT_MS {
                                menu_rep = n;
                                crate::ui::track_menu::move_focus(sym as c_int);
                                set_hud(n + 8000);
                            }
                        } else if matches!(route, Route::Player { .. }) && hud_focus == 0 && scrub_dir != 0 && isnav {
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
                    // the in-player track menu is modal — it swallows every key while open
                    if matches!(route, Route::Player { overlay: Overlay::Menu }) {
                        if sym == SDLK_LEFT || sym == SDLK_RIGHT || sym == SDLK_UP || sym == SDLK_DOWN {
                            // move once on the fresh press; a held UP/DOWN repeats via the 0x101 path
                            crate::ui::track_menu::move_focus(sym as c_int);
                            menu_rep = last_input;
                            set_hud(last_input + 8000);
                        } else if sym == SDLK_RETURN || sym == SDLK_KP_ENTER || sym == SDLK_SELECT {
                            crate::ui::track_menu::on_ok();
                            route = Route::Player { overlay: Overlay::None };
                            set_hud(last_input + 4500);
                        } else if sym == SDLK_ESCAPE || sym == 'q' as u32 || wcode == 461 || wcode == 482 {
                            crate::ui::track_menu::close();
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
                            hud_focus = 2;
                            set_hud(last_input + 4500);
                        } else if sym == SDLK_UP || sym == SDLK_DOWN {
                            crate::ui::info_panel::move_focus(sym as c_int);
                            set_hud(last_input + 8000);
                        } else if sym == SDLK_RETURN || sym == SDLK_KP_ENTER || sym == SDLK_SELECT {
                            match crate::ui::info_panel::on_ok() {
                                crate::ui::info_panel::InfoAction::FromBeginning => {
                                    request_seek(0);
                                    if paused() {
                                        set_paused(false);
                                        crate::player::resume();
                                    }
                                }
                                crate::ui::info_panel::InfoAction::GoToDetail(rk) => {
                                    // stop playback and open the show (episode) or movie detail page
                                    if !rk.is_empty() {
                                        crate::player::stop_bufferfeed(false);
                                        crate::ui::detail::open_rk(&rk);
                                        route = Route::Detail;
                                    }
                                }
                                crate::ui::info_panel::InfoAction::None => {}
                            }
                            // guarded: the GoToDetail arm above set Route::Detail — don't resurrect Player over it
                            if matches!(route, Route::Player { .. }) {
                                route = Route::Player { overlay: Overlay::None };
                            }
                            set_hud(last_input + 4500);
                        } else if sym == SDLK_ESCAPE || sym == 'q' as u32 || wcode == 461 || wcode == 482 {
                            crate::ui::info_panel::close();
                            route = Route::Player { overlay: Overlay::None };
                            set_hud(last_input + 4500);
                        }
                        continue;
                    }
                    // the Chapters strip is modal too — LEFT/RIGHT pick, OK seeks, BACK closes
                    if matches!(route, Route::Player { overlay: Overlay::Chapters }) {
                        if sym == SDLK_LEFT || sym == SDLK_RIGHT || sym == 417 || wcode == 417 || sym == 412 || wcode == 412 {
                            let l = sym == SDLK_LEFT || sym == 412 || wcode == 412;
                            let key = if l { SDLK_LEFT } else { SDLK_RIGHT };
                            crate::ui::chapters_panel::move_focus(key as c_int);
                            set_hud(last_input + 8000);
                        } else if sym == SDLK_RETURN || sym == SDLK_KP_ENTER || sym == SDLK_SELECT {
                            let ns = crate::ui::chapters_panel::on_ok();
                            if ns >= 0 {
                                request_seek(ns);
                                if paused() {
                                    set_paused(false);
                                    crate::player::resume();
                                }
                            }
                            route = Route::Player { overlay: Overlay::None };
                            set_hud(last_input + 4500);
                        } else if sym == SDLK_DOWN {
                            // drop focus back onto the tabs below the strip
                            crate::ui::chapters_panel::close();
                            route = Route::Player { overlay: Overlay::None };
                            hud_focus = 2;
                            set_hud(last_input + 4500);
                        } else if sym == SDLK_ESCAPE || sym == 'q' as u32 || wcode == 461 || wcode == 482 {
                            crate::ui::chapters_panel::close();
                            route = Route::Player { overlay: Overlay::None };
                            set_hud(last_input + 4500);
                        }
                        continue;
                    }
                    // playing: UP/DOWN move the HUD focus (scrubber ↔ buttons ↔ tabs). The first
                    // press on a hidden HUD just reveals it (focused on the scrubber); pressing UP
                    // with nothing focusable above (the buttons row) hides the HUD again.
                    if matches!(route, Route::Player { .. }) && (sym == SDLK_UP || sym == SDLK_DOWN) {
                        if !cur_hidden {
                            SDL_webOSCursorVisibility(0);
                            cur_hidden = true;
                        }
                        let vis = hud_shown(last_input, hud_until(), paused(), hud_dismissed);
                        let mut hide = false;
                        if !vis {
                            hud_focus = 0; // reveal, on the scrubber
                        } else if sym == SDLK_UP {
                            match hud_focus {
                                0 => hud_focus = 1, // scrubber → buttons
                                2 => hud_focus = 0, // tabs → scrubber
                                _ => {
                                    hide = true; // buttons: nothing above → hide the HUD
                                    hud_focus = 0;
                                }
                            }
                        } else {
                            match hud_focus {
                                0 => hud_focus = 2, // scrubber → tabs
                                1 => hud_focus = 0, // buttons → scrubber
                                _ => {}             // tabs: nothing below → stay
                            }
                        }
                        if hud_focus != 0 || hide {
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
                            set_hud(last_input + 4500);
                        }
                        continue;
                    }
                    if !matches!(route, Route::Player { .. }) && (sym == SDLK_LEFT || sym == SDLK_RIGHT || sym == SDLK_UP || sym == SDLK_DOWN) {
                        if !dpad_mode {
                            SDL_webOSCursorVisibility(0);
                        }
                        dpad_mode = true;
                        mot_accum = 0.0;
                        if matches!(route, Route::Detail) {
                            crate::ui::detail::move_focus(sym as c_int);
                        } else if g_snap() < 0.5 {
                            if sym == SDLK_DOWN {
                                set_snap(1.0);
                                set_fr(0);
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
                    } else if sym == SDLK_RETURN || sym == SDLK_KP_ENTER || sym == SDLK_SELECT {
                        if matches!(route, Route::Player { .. }) {
                            let vis = hud_shown(last_input, hud_until(), paused(), hud_dismissed);
                            if vis && hud_focus == 1 {
                                // OK on a control button opens its panel (Subtitles / Audio)
                                crate::ui::track_menu::open_tab(if hud_btn == 0 { 1 } else { 0 });
                                route = Route::Player { overlay: Overlay::Menu };
                            } else if vis && hud_focus == 2 {
                                if hud_tab == 0 {
                                    crate::ui::info_panel::open(); // Info card
                                    route = Route::Player { overlay: Overlay::Info };
                                } else if hud_tab == 1 {
                                    crate::ui::chapters_panel::open(); // Chapters strip
                                    route = Route::Player { overlay: Overlay::Chapters };
                                }
                            } else {
                                let np = !paused();
                                set_paused(np);
                                if np {
                                    crate::player::pause();
                                } else {
                                    crate::player::resume();
                                }
                            }
                            set_hud(last_input + 4500);
                        } else if matches!(route, Route::Detail) {
                            // OK on the detail page: Play/episode starts playback (route
                            // already set by on_ok); a season tab just switches season.
                            if crate::ui::detail::on_ok() {
                                let resume = crate::ui::detail::last_resume_ns();
                                if resume > 0 {
                                    crate::player::resume_at(resume); // seek AT the first Load (direct-play) or restart the transcode at &offset
                                }
                                if crate::player::start_bufferfeed() {
                                    played_from_detail = true; // Stop/BACK/EOS returns to this detail page
                                    route = Route::Player { overlay: Overlay::None };
                                }
                                set_paused(false);
                                set_hud(last_input + 4500);
                            }
                        } else {
                            // home: route by the focused hub item's type
                            let m = if g_snap() < 0.5 {
                                crate::ui::home::movie_at(0, 0)
                            } else {
                                crate::ui::home::movie_at(g_fr(), g_fc())
                            };
                            if let Some(mm) = m.as_ref() {
                                let rk = crate::ui::widgets::cfield(&mm.rk);
                                if !rk.is_empty() {
                                    if mm.kind == 3 {
                                        // episode (Continue Watching / On Deck): play directly,
                                        // no detail page — Back returns to the home hubs. Load the
                                        // episode metadata (streams) + reset the track menu so the
                                        // in-player audio/subtitle lists are populated.
                                        crate::route::play_movie(m);
                                        crate::metadata::load_detail(&rk);
                                        crate::ui::track_menu::reset();
                                        let resume = resume_ns(mm.resume_ms, mm.dur_ns);
                                        if resume > 0 {
                                            crate::player::resume_at(resume); // seek AT the first Load (direct-play) or restart the transcode at &offset
                                        }
                                        if crate::player::start_bufferfeed() {
                                            played_from_detail = false;
                                            route = Route::Player { overlay: Overlay::None };
                                        }
                                        set_paused(false);
                                        set_hud(last_input + 4500);
                                    } else if mm.kind == 2 {
                                        // season: open the SHOW page with that season selected
                                        crate::ui::detail::open_rk_season(
                                            &crate::ui::widgets::cfield(&mm.show_rk),
                                            mm.season_index,
                                        );
                                        route = Route::Detail;
                                    } else {
                                        // movie / show: open the detail page
                                        crate::ui::detail::open_rk(&rk);
                                        route = Route::Detail;
                                    }
                                    if !dpad_mode {
                                        SDL_webOSCursorVisibility(0);
                                        dpad_mode = true;
                                    }
                                }
                            }
                        }
                    } else if wcode == 72 || sym == 415 || wcode == 415 {
                        // PAUSE
                        if matches!(route, Route::Player { .. }) && !paused() {
                            set_paused(true);
                            crate::player::pause();
                        }
                        set_hud(last_input + 4500);
                    } else if wcode == 450 || sym == 19 || wcode == 19 || sym == 402 || wcode == 402 {
                        // PLAY
                        if !matches!(route, Route::Player { .. }) {
                            if crate::player::start_bufferfeed() {
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
                            }
                        } else if paused() {
                            set_paused(false);
                            crate::player::resume();
                        }
                        set_hud(last_input + 4500);
                    } else if matches!(route, Route::Player { .. }) && (sym == 413 || wcode == 413) {
                        // Stop
                        crate::player::stop_bufferfeed(false);
                        route = if played_from_detail { Route::Detail } else { Route::Home };
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
                        let vis = hud_shown(last_input, hud_until(), paused(), hud_dismissed);
                        set_hud(last_input + 4500);
                        if !vis {
                            hud_focus = 0; // first LEFT/RIGHT reveals the HUD on the scrubber
                        }
                        if hud_focus == 1 {
                            hud_btn = (hud_btn + if fwd { 1 } else { -1 }).clamp(0, 1);
                        } else if hud_focus == 2 {
                            let max_tab = if crate::ui::chapters_panel::has_chapters() { 1 } else { 0 };
                            hud_tab = (hud_tab + if fwd { 1 } else { -1 }).clamp(0, max_tab);
                        } else if dur() > 0 {
                            // scrubber focus, FRESH press (0x001): the fixed 10s jump. A held key's
                            // 0x101 repeats (handled above) then engage the continuous scrub; the
                            // keyup commits. Quick re-taps before scrub_commit_at accumulate.
                            let cap = dur() - 3 * 1_000_000_000;
                            scrub_commit_at = 0; // more input → cancel a pending tap commit
                            scrub_alive = last_input;
                            if scrub_dir == 0 && scrub() < 0 {
                                // Seed a new scrub at the INTENDED playhead. If a prior commit's
                                // seek is still landing, playpos() is stale (it still reports the
                                // pre-seek spot), so a quick re-press would jump back to where we
                                // started and resume there — interrupting the scrub. While a seek is
                                // in flight, seed from its target instead.
                                let seed = if crate::player::loading() && crate::player::seek_display_ns() >= 0 {
                                    let t = crate::player::seek_display_ns();
                                    log(&format!("scrub: seed at in-flight target {}s (playpos {}s stale)",
                                        t / 1_000_000_000, playpos() / 1_000_000_000));
                                    t
                                } else {
                                    playpos()
                                };
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
                    } else if sym == SDLK_ESCAPE || sym == 'q' as u32 || wcode == 461 || wcode == 482 {
                        // webOS BACK: this Magic Remote sends wcode 482 (0x1E2); 461 kept for others.
                        // Back stack: player -> detail (if opened from there) -> grid -> hero -> exit.
                        if matches!(route, Route::Player { .. }) {
                            crate::player::stop_bufferfeed(false);
                            route = if played_from_detail { Route::Detail } else { Route::Home };
                        } else if matches!(route, Route::Detail) {
                            crate::ui::detail::close();
                            route = Route::Home;
                        } else if g_snap() > 0.5 {
                            set_snap(0.0);
                        } else {
                            running = false;
                        }
                    }
                } else if et == SDL_MOUSEMOTION {
                    last_input = SDL_GetTicks();
                    last_ptr_motion = last_input;
                    cur_hidden = false;
                    let mx = rd_i32(&ev, 20) as f32;
                    let my = rd_i32(&ev, 24) as f32;
                    if prev_mx >= 0.0 {
                        mot_accum += (mx - prev_mx).abs() + (my - prev_my).abs();
                    }
                    prev_mx = mx;
                    prev_my = my;
                    if matches!(route, Route::Player { .. }) {
                        hud_dismissed = false;
                        set_hud(last_input + 4500);
                        if ptr_drag && dur() > 0 {
                            let sbx = 90.0f32;
                            let sbw = SCR_W as f32 - 180.0;
                            let mut frac = ((mx - sbx) / sbw) as f64;
                            if frac < 0.0 {
                                frac = 0.0;
                            }
                            if frac > 1.0 {
                                frac = 1.0;
                            }
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
                    crate::ui::home::home_pointer_focus(mx, my);
                } else if et == SDL_MOUSEBUTTONDOWN {
                    last_input = SDL_GetTicks();
                    if matches!(route, Route::Player { .. }) {
                        hud_dismissed = false;
                        let cx = rd_i32(&ev, 20) as f32;
                        let cy = rd_i32(&ev, 24) as f32;
                        let sbx = 90.0f32;
                        let sbw = SCR_W as f32 - 180.0;
                        let on_scrub = dur() > 0
                            && cy > SCR_H as f32 - 270.0
                            && cy < SCR_H as f32 - 110.0
                            && cx >= sbx
                            && cx <= sbx + sbw;
                        // the Subtitles/Audio control icons (player_hud::icon_hit is the shared geometry)
                        let icon = crate::ui::player_hud::icon_hit(cx, cy);
                        if matches!(route, Route::Player { overlay: Overlay::Menu }) {
                            crate::ui::track_menu::close();
                            route = Route::Player { overlay: Overlay::None };
                        } else if let Some(idx) = icon {
                            crate::ui::track_menu::open_tab(if idx == 0 { 1 } else { 0 }); // Subtitles button → subtitles tab
                            route = Route::Player { overlay: Overlay::Menu };
                            hud_focus = 1;
                            hud_btn = idx;
                        } else if on_scrub {
                            let mut frac = ((cx - sbx) / sbw) as f64;
                            if frac < 0.0 {
                                frac = 0.0;
                            }
                            if frac > 1.0 {
                                frac = 1.0;
                            }
                            let mut t = (frac * dur() as f64) as i64;
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
                                crate::player::pause();
                            } else {
                                crate::player::resume();
                            }
                        }
                        set_hud(last_input + 4500);
                    }
                } else if et == SDL_MOUSEBUTTONUP {
                    last_input = SDL_GetTicks();
                    if ptr_drag {
                        ptr_drag = false;
                        if scrub() >= 0 {
                            commit_seek(scrub(), &mut bg_pos);
                        }
                        set_hud(last_input + 4500);
                    }
                } else if et == SDL_MOUSEWHEEL {
                    let wnow = SDL_GetTicks();
                    last_input = wnow;
                    if wnow.wrapping_sub(last_wheel) > 250 {
                        last_wheel = wnow;
                        crate::ui::home::home_wheel(rd_i32(&ev, 20));
                    }
                }
            }

            let now = SDL_GetTicks();
            // dev: /tmp/poc-autoplay auto-presses OK once
            if !auto_tried && !matches!(route, Route::Player { .. }) && now.wrapping_sub(t0) > 2000 {
                auto_tried = true;
                if std::path::Path::new("/tmp/poc-autoplay").exists() {
                    if std::path::Path::new("/tmp/poc-h265").exists() {
                        // Phase 0 HEVC probe: leave the URL empty so start_bufferfeed feeds
                        // the local /tmp/sample.h265 through the H265 Load payload.
                        crate::route::clear_url();
                    } else {
                        let pidx = std::fs::read_to_string("/tmp/poc-playidx").ok()
                            .and_then(|s| s.trim().parse::<c_int>().ok()).unwrap_or(0);
                        let pm = crate::ui::home::movie_at(pidx / COLS, pidx % COLS);
                        crate::route::play_movie(pm);
                        if let Some(pmm) = pm.as_ref() {
                            crate::metadata::load_detail(&crate::ui::widgets::cfield(&pmm.rk));
                            crate::ui::track_menu::reset();
                        }
                    }
                    if crate::player::start_bufferfeed() {
                        played_from_detail = matches!(route, Route::Detail);
                        route = Route::Player { overlay: Overlay::None };
                    }
                    set_paused(false);
                    set_hud(now + 60000);
                }
            }
            if !grid_tried && now.wrapping_sub(t0) > 400 {
                grid_tried = true;
                if std::path::Path::new("/tmp/poc-grid").exists() {
                    set_snap(1.0);
                    set_fr(0);
                }
            }
            // dev: /tmp/poc-detail=<ratingKey> opens that catalog item's detail page once
            if !detail_tried && now.wrapping_sub(t0) > 500 {
                detail_tried = true;
                if let Ok(rk) = std::fs::read_to_string("/tmp/poc-detail") {
                    let rk = rk.trim();
                    if !rk.is_empty() {
                        let idx = crate::pms::index_of_rk(rk);
                        if idx >= 0 {
                            crate::ui::detail::open(idx);
                            route = Route::Detail;
                            // dev: /tmp/poc-detailsec=N jumps N sections down (headless episode/row capture)
                            if let Ok(n) = std::fs::read_to_string("/tmp/poc-detailsec") {
                                for _ in 0..n.trim().parse::<u32>().unwrap_or(0) {
                                    crate::ui::detail::move_focus(SDLK_DOWN as c_int);
                                }
                            }
                            // dev: /tmp/poc-detailcol=N then moves the focus N to the right
                            if let Ok(n) = std::fs::read_to_string("/tmp/poc-detailcol") {
                                for _ in 0..n.trim().parse::<u32>().unwrap_or(0) {
                                    crate::ui::detail::move_focus(SDLK_RIGHT as c_int);
                                }
                            }
                            // dev: /tmp/poc-detailplay activates the focused control (headless play test)
                            if std::path::Path::new("/tmp/poc-detailplay").exists()
                                && crate::ui::detail::on_ok()
                            {
                                let resume = crate::ui::detail::last_resume_ns();
                                if resume > 0 {
                                    crate::player::resume_at(resume); // seek AT the first Load (direct-play) or restart the transcode at &offset
                                }
                                if crate::player::start_bufferfeed() {
                                    played_from_detail = matches!(route, Route::Detail);
                                    route = Route::Player { overlay: Overlay::None };
                                }
                                set_paused(false);
                                set_hud(now + 60000);
                            }
                        } else {
                            crate::metadata::load_detail(rk); // off-catalog rk: load data only
                        }
                    }
                }
            }
            // dev: /tmp/poc-play=<ratingKey> plays ANY library item (regression harness).
            // Unlike poc-detail it does NOT depend on the item being in the home catalog:
            // it fetches the item's metadata fresh and drives the same field-based play
            // path the detail Play button uses (route::play_episode is generic — movie or
            // episode), so tests can target arbitrary rks deterministically.
            if !play_tried && !matches!(route, Route::Player { .. }) && now.wrapping_sub(t0) > 500 {
                play_tried = true;
                if let Ok(rk) = std::fs::read_to_string("/tmp/poc-play") {
                    let rk = rk.trim();
                    if !rk.is_empty() {
                        crate::metadata::load_detail(rk); // fetch ANY rk (movie/show/episode)
                        crate::ui::track_menu::reset(); // populate the audio/subtitle lists
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
                                log(&format!("poc-play: rk={rk} start"));
                                crate::route::play_episode(rk, &part, &vc, &ac, &title, "");
                                // resume_ns wants the duration in NANOSECONDS (dur_ms is ms)
                                let resume = resume_ns(resume_ms, dur_ms * 1_000_000);
                                if resume > 0 {
                                    crate::player::resume_at(resume); // seek AT first Load / restart transcode at &offset
                                }
                                if crate::player::start_bufferfeed() {
                                    played_from_detail = matches!(route, Route::Detail);
                                    route = Route::Player { overlay: Overlay::None };
                                }
                                set_paused(false);
                                set_hud(now + 60000);
                            }
                        }
                    }
                }
            }
            // resume is armed BEFORE start_bufferfeed (crate::player::arm_seek) so the very
            // first Load opens at the viewOffset — no play-from-start flash, no post-frames seek.
            if !seek_tried && matches!(route, Route::Player { .. }) && dur() > 0 && now.wrapping_sub(t0) > 12000 {
                seek_tried = true;
                if std::path::Path::new("/tmp/poc-autoseek").exists() {
                    request_seek(140 * 1_000_000_000);
                }
            }
            // dev: /tmp/poc-autopause pauses once (headless paused-HUD capture)
            if !pause_tried && matches!(route, Route::Player { .. }) && now.wrapping_sub(t0) > 6000 {
                pause_tried = true;
                if std::path::Path::new("/tmp/poc-autopause").exists() {
                    set_paused(true);
                    set_hud(now + 60000);
                }
            }
            // dev: /tmp/poc-menu=<tab> opens the in-player track menu once (headless capture)
            if !menu_tried && matches!(route, Route::Player { .. }) && now.wrapping_sub(t0) > 6000 {
                menu_tried = true;
                if let Ok(t) = std::fs::read_to_string("/tmp/poc-menu") {
                    crate::ui::track_menu::open_tab(t.trim().parse::<c_int>().unwrap_or(0));
                    route = Route::Player { overlay: Overlay::Menu };
                    set_hud(now + 60000);
                }
                // dev: /tmp/poc-info opens the Info card once (headless capture)
                if std::path::Path::new("/tmp/poc-info").exists() {
                    crate::ui::info_panel::open();
                    route = Route::Player { overlay: Overlay::Info };
                    hud_focus = 2;
                    hud_tab = 0;
                    set_hud(now + 60000);
                }
                // dev: /tmp/poc-chapters opens the Chapters strip once (headless capture)
                if std::path::Path::new("/tmp/poc-chapters").exists() {
                    crate::ui::chapters_panel::open();
                    route = Route::Player { overlay: Overlay::Chapters };
                    hud_focus = 2;
                    hud_tab = 1;
                    set_hud(now + 60000);
                }
            }
            // dev: /tmp/poc-menupick="<tab>,<row>" opens the menu, selects that row, and
            // confirms it (headless track switch: e.g. "0,4" = audio tab, row 4).
            if !menupick_tried && matches!(route, Route::Player { .. }) && now.wrapping_sub(t0) > 7000 {
                menupick_tried = true;
                if let Ok(s) = std::fs::read_to_string("/tmp/poc-menupick") {
                    let mut it = s.trim().split(',');
                    let tab = it.next().and_then(|x| x.trim().parse::<c_int>().ok()).unwrap_or(0);
                    let row = it.next().and_then(|x| x.trim().parse::<c_int>().ok()).unwrap_or(0);
                    crate::ui::track_menu::open_tab(tab);
                    for _ in 0..row {
                        crate::ui::track_menu::move_focus(SDLK_DOWN as c_int);
                    }
                    crate::ui::track_menu::on_ok();
                }
            }
            if is_started() {
                crate::player::pump(now);
            }
            // end-of-stream: the pipeline drained at the credits → leave the player (back to the
            // detail page or home, whichever is behind), instead of freezing on the last frame.
            if matches!(route, Route::Player { .. }) && crate::player::ended() {
                crate::player::stop_bufferfeed(false);
                route = if played_from_detail { Route::Detail } else { Route::Home };
                crate::ui::info_panel::close();
                crate::ui::chapters_panel::close();
                hud_focus = 0;
            }
            // client-side long-press repeat: scrolls the track menu, or the home grid. Driven by a
            // held-key timer so it doesn't depend on the remote's hardware auto-repeat delay.
            if held_sym != 0 && now.wrapping_sub(held_since) > 380 && now.wrapping_sub(last_rep) > 110 {
                last_rep = now;
                if matches!(route, Route::Home) && g_snap() > 0.5 {
                    crate::ui::home::home_move_focus(held_sym);
                }
            }
            // keep the HUD alive while the track menu / Info card / Chapters strip is open
            if matches!(route, Route::Player { overlay } if overlay != Overlay::None) {
                set_hud(now + 4500);
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
                set_hud(now + 4500);
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
            // when the HUD auto-hides, park focus back on the scrubber so the next reveal is clean
            if matches!(route, Route::Player { .. }) && !hud_shown(now, hud_until(), paused(), hud_dismissed) {
                hud_focus = 0;
                hud_btn = 0;
                hud_tab = 0;
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
                crate::player::pause();
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

            crate::ui::home::home_update(dt);
            // dev: /tmp/poc-detailosc perpetually swings the detail scroll hero<->bottom so the FPS
            // heartbeat samples the scroll TRANSITION (the settled ends already hold 60).
            if matches!(route, Route::Detail) && std::path::Path::new("/tmp/poc-detailosc").exists() {
                let sym = if (now / 450) % 2 == 0 { SDLK_DOWN } else { SDLK_UP };
                crate::ui::detail::move_focus(sym as c_int);
            }
            if matches!(route, Route::Detail) {
                crate::ui::detail::update(dt);
            }
            if matches!(route, Route::Player { overlay: Overlay::Menu }) {
                crate::ui::track_menu::update(dt); // pill slide + open fade
            }
            if matches!(route, Route::Player { overlay: Overlay::Info }) {
                crate::ui::info_panel::update(dt);
            }
            if matches!(route, Route::Player { overlay: Overlay::Chapters }) {
                crate::ui::chapters_panel::update(dt);
            }
            crate::posters::poster_pump(3);

            glViewport(0, 0, SCR_W, SCR_H);
            if matches!(route, Route::Player { .. }) {
                crate::system::clear_opaque_region();
                glClearColor(0.0, 0.0, 0.0, 0.0);
                glClear(GL_COLOR_BUFFER_BIT);
                crate::ui::player_hud::draw_subtitle_bitmap(); // PGS/VobSub image subs
                let hud_up = hud_shown(now, hud_until(), paused(), hud_dismissed) || crate::player::loading();
                crate::ui::player_hud::draw_subtitles(hud_up || matches!(route, Route::Player { overlay: Overlay::Menu }));
                if hud_up || !matches!(route, Route::Player { overlay: Overlay::None }) {
                    // hide the transport middle behind the Info card / Chapters strip
                    crate::ui::player_hud::draw_hud(hud_focus, hud_btn, hud_tab, now, !matches!(route, Route::Player { overlay: Overlay::Info | Overlay::Chapters }));
                }
                if matches!(route, Route::Player { overlay: Overlay::Menu }) {
                    crate::ui::track_menu::draw();
                }
                if matches!(route, Route::Player { overlay: Overlay::Info }) {
                    crate::ui::info_panel::draw();
                }
                if matches!(route, Route::Player { overlay: Overlay::Chapters }) {
                    crate::ui::chapters_panel::draw();
                }
                crate::ui::anim::draw_overlay();
                SDL_GL_SwapWindow(win);
                crate::ui::profile::frame_end();
                frames_ct += 1;
                if now.wrapping_sub(fps_t) >= 1000 {
                    frames_ct = 0;
                    fps_t = now;
                }
                continue;
            }
            if matches!(route, Route::Detail) {
                crate::ui::detail::draw();
            } else {
                crate::ui::home::home_draw();
            }
            let fps_col = [0.4f32, 1.0, 0.55, 1.0];
            crate::gfx::draw_number(fps_shown, SCR_W as f32 - 70.0, 64.0, 46.0, fps_col.as_ptr());
            crate::ui::anim::draw_overlay(); // home/detail animations (episode scale-pop, scroll)
            SDL_GL_SwapWindow(win);
            crate::ui::profile::frame_end();
            frames_ct += 1;
            if now.wrapping_sub(fps_t) >= 1000 {
                fps_shown = (frames_ct as f32 * 1000.0 / now.wrapping_sub(fps_t) as f32 + 0.5) as i32;
                frames_ct = 0;
                fps_t = now;
                // once/sec render heartbeat on the home/detail path (quiet in the log, unlike the
                // feed-stat-heavy player path) — so FPS is greppable without reading the on-screen counter
                log(&format!("FPS={fps_shown} route={}", if matches!(route, Route::Detail) { "detail" } else { "home" }));
            }
        }

        if is_started() {
            crate::player::stop_bufferfeed(false);
        }
        crate::posters::posters_shutdown();
        SDL_Quit();
        0
    }
}
