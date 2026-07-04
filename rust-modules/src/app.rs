//! plex_run — the Rust app core (was the body of src/main.c). Owns SDL init, the
//! event loop, input decode, the per-frame tick, draw orchestration, app lifecycle,
//! the buffer-feed pump orchestration, and the dev triggers. The C boot shim
//! (main.c) sets up the log + crash tracer, then calls plex_run(). The only C left
//! below us is the starfish.c C++/ACB seam (the engine itself is Rust: crate::player).
#![allow(non_upper_case_globals)]
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::ptr::{addr_of, addr_of_mut};
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

#[inline]
fn rd_u32(ev: &[u8], off: usize) -> u32 {
    u32::from_ne_bytes([ev[off], ev[off + 1], ev[off + 2], ev[off + 3]])
}
#[inline]
fn rd_i32(ev: &[u8], off: usize) -> i32 {
    i32::from_ne_bytes([ev[off], ev[off + 1], ev[off + 2], ev[off + 3]])
}

// ui focus globals live in ui::home; access them without a static-mut reference
#[inline]
fn g_fr() -> c_int { unsafe { addr_of!(crate::ui::home::fr).read() } }
#[inline]
fn g_fc() -> c_int { unsafe { addr_of!(crate::ui::home::fc).read() } }
#[inline]
fn g_snap() -> f32 { unsafe { addr_of!(crate::ui::home::snapTarget).read() } }
#[inline]
fn set_fr(v: c_int) { unsafe { addr_of_mut!(crate::ui::home::fr).write(v) } }
#[inline]
fn set_snap(v: f32) { unsafe { addr_of_mut!(crate::ui::home::snapTarget).write(v) } }

// transport state — was the C playback globals; now crate::player (atomics)
#[inline]
fn paused() -> bool { crate::player::TX.paused.load(Relaxed) }
#[inline]
fn set_paused(v: bool) { crate::player::TX.paused.store(v, Relaxed) }
#[inline]
fn hud_until() -> u32 { crate::player::TX.hud_until.load(Relaxed) }
#[inline]
fn set_hud(x: u32) { crate::player::TX.hud_until.store(x, Relaxed) }
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
#[inline]
fn is_started() -> bool { crate::player::is_started() }

#[no_mangle]
pub extern "C" fn plex_run(
    pms_host: *const c_char,
    pms_port: c_int,
    pms_token: *const c_char,
    demo_url: *const c_char,
) -> c_int {
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

        // fetch the catalog once, then spawn the poster workers
        let nmov = crate::pms::pms_fetch_movies(pms_host, pms_port, pms_token, 1);
        log(&format!("pms: nmovies={nmov}"));
        crate::posters::posters_init(pms_host, pms_port, pms_token);
        crate::route::set_config(
            &std::ffi::CStr::from_ptr(pms_host).to_string_lossy(),
            pms_port,
            &std::ffi::CStr::from_ptr(pms_token).to_string_lossy(),
            &std::ffi::CStr::from_ptr(demo_url).to_string_lossy(),
        );

        crate::ui::home::home_init();
        crate::player::acb_init();

        let mut last_input = SDL_GetTicks();
        let t0 = SDL_GetTicks();
        let mut fps_t = t0;
        let mut frames_ct = 0i32;
        let mut fps_shown = 0i32;
        let mut running = true;

        let mut held_sym = 0u32;
        let mut held_since = 0u32;
        let mut last_rep = 0u32;
        let mut scrub_last = 0u32;
        let mut scrub_t = 0u32;
        let mut scrub_dir = 0i32;
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
        let mut playing = false;

        let mut auto_tried = false;
        let mut grid_tried = false;
        let mut seek_tried = false;
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
                    log(&format!("LIFECYCLE: background (playing={})", playing as i32));
                    if playing && !bg_was_playing {
                        bg_pos = playpos();
                        bg_was_playing = true;
                        bg_was_paused = paused();
                        scrub_dir = 0;
                        ptr_drag = false;
                        set_scrub(-1);
                        crate::player::stop_bufferfeed(true);
                        playing = false;
                    }
                } else if et == 0x105 || et == 0x106 {
                    // WILL/DID ENTER FOREGROUND
                    log(&format!("LIFECYCLE: foreground (wasPlaying={})", bg_was_playing as i32));
                    if bg_was_playing && et == 0x106 {
                        bg_was_playing = false; // clear regardless so a later 0x106 can't re-fire
                        // only resume if a PLAY key didn't already restart playback in the
                        // WILL->DID window (a second start would drop the live Engine -> UAF)
                        if !playing {
                            playing = crate::player::start_bufferfeed();
                            if playing {
                                let mut rt = bg_pos;
                                if !bg_was_paused {
                                    rt -= RESUME_REWIND_NS;
                                    if rt < 0 {
                                        rt = 0;
                                    }
                                }
                                request_seek(rt);
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
                        // real key-up → commit the scrub as a seek
                        if sym == held_sym {
                            held_sym = 0;
                        }
                        if playing && scrub_dir != 0 && isnav {
                            request_seek(scrub());
                            set_scrub(-1);
                            scrub_dir = 0;
                            scrub_t = 0;
                        }
                        continue;
                    }
                    if state & 0x100 != 0 {
                        // auto-repeat
                        if playing && scrub_dir != 0 && isnav {
                            scrub_last = SDL_GetTicks();
                        }
                        continue;
                    }
                    last_input = SDL_GetTicks();
                    if !playing && (sym == SDLK_LEFT || sym == SDLK_RIGHT || sym == SDLK_UP || sym == SDLK_DOWN) {
                        if !dpad_mode {
                            SDL_webOSCursorVisibility(0);
                        }
                        dpad_mode = true;
                        mot_accum = 0.0;
                        if g_snap() < 0.5 {
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
                        if !playing {
                            let m = if g_snap() < 0.5 { crate::ui::home::movie_at(0, 0) } else { crate::ui::home::movie_at(g_fr(), g_fc()) };
                            crate::route::play_movie(m);
                            playing = crate::player::start_bufferfeed();
                            set_paused(false);
                            set_hud(last_input + 4500);
                            if !dpad_mode {
                                SDL_webOSCursorVisibility(0);
                                dpad_mode = true;
                            }
                        } else {
                            let np = !paused();
                            set_paused(np);
                            if np {
                                crate::player::pause();
                            } else {
                                crate::player::resume();
                            }
                            set_hud(last_input + 4500);
                        }
                    } else if wcode == 72 || sym == 415 || wcode == 415 {
                        // PAUSE
                        if playing && !paused() {
                            set_paused(true);
                            crate::player::pause();
                        }
                        set_hud(last_input + 4500);
                    } else if wcode == 450 || sym == 19 || wcode == 19 || sym == 402 || wcode == 402 {
                        // PLAY
                        if !playing {
                            playing = crate::player::start_bufferfeed();
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
                    } else if playing && (sym == 413 || wcode == 413) {
                        // Stop
                        crate::player::stop_bufferfeed(false);
                        playing = false;
                    } else if playing
                        && (sym == SDLK_LEFT || sym == SDLK_RIGHT || sym == SDLK_UP || sym == SDLK_DOWN
                            || sym == 417 || wcode == 417 || sym == 412 || wcode == 412)
                    {
                        set_hud(last_input + 4500);
                        if !cur_hidden {
                            SDL_webOSCursorVisibility(0);
                            cur_hidden = true;
                        }
                        if ptr_drag {
                            ptr_drag = false;
                            set_scrub(-1);
                        }
                        let fwd = sym == SDLK_RIGHT || sym == 417 || wcode == 417;
                        let back = sym == SDLK_LEFT || sym == 412 || wcode == 412;
                        if (fwd || back) && dur() > 0 {
                            if scrub() < 0 {
                                set_scrub(playpos());
                            }
                            let mut s = scrub() + (if fwd { 10i64 } else { -10i64 }) * 1_000_000_000;
                            let cap = dur() - 3 * 1_000_000_000;
                            if s < 0 {
                                s = 0;
                            }
                            if cap > 0 && s > cap {
                                s = cap;
                            }
                            set_scrub(s);
                            scrub_dir = if fwd { 1 } else { -1 };
                            scrub_last = last_input;
                        }
                    } else if sym == SDLK_ESCAPE || sym == 'q' as u32 || wcode == 461 {
                        if playing {
                            crate::player::stop_bufferfeed(false);
                            playing = false;
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
                    if playing {
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
                            scrub_last = last_input;
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
                    if playing {
                        let cx = rd_i32(&ev, 20) as f32;
                        let cy = rd_i32(&ev, 24) as f32;
                        let sbx = 90.0f32;
                        let sbw = SCR_W as f32 - 180.0;
                        let on_scrub = dur() > 0
                            && cy > SCR_H as f32 - 270.0
                            && cy < SCR_H as f32 - 110.0
                            && cx >= sbx
                            && cx <= sbx + sbw;
                        if on_scrub {
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
                            scrub_last = last_input;
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
                            request_seek(scrub());
                            set_scrub(-1);
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
            if !auto_tried && !playing && now.wrapping_sub(t0) > 2000 {
                auto_tried = true;
                if std::path::Path::new("/tmp/poc-autoplay").exists() {
                    let pidx = std::fs::read_to_string("/tmp/poc-playidx").ok()
                        .and_then(|s| s.trim().parse::<c_int>().ok()).unwrap_or(0);
                    crate::route::play_movie(crate::ui::home::movie_at(pidx / COLS, pidx % COLS));
                    playing = crate::player::start_bufferfeed();
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
            if !seek_tried && playing && dur() > 0 && now.wrapping_sub(t0) > 12000 {
                seek_tried = true;
                if std::path::Path::new("/tmp/poc-autoseek").exists() {
                    request_seek(140 * 1_000_000_000);
                }
            }
            if is_started() {
                crate::player::pump(now);
            }
            // client-side long-press repeat (grid nav)
            if held_sym != 0 && now.wrapping_sub(held_since) > 400 && now.wrapping_sub(last_rep) > 130 {
                last_rep = now;
                if g_snap() > 0.5 {
                    crate::ui::home::home_move_focus(held_sym);
                }
            }
            // LEFT/RIGHT scrub advance
            if scrub() >= 0 && scrub_dir != 0 && !ptr_drag {
                if now.wrapping_sub(scrub_last) > 1200 {
                    request_seek(scrub());
                    set_scrub(-1);
                    scrub_dir = 0;
                    scrub_t = 0;
                } else {
                    let mut sdt = if scrub_t != 0 { now.wrapping_sub(scrub_t) as f32 / 1000.0 } else { 0.016 };
                    if sdt > 0.1 {
                        sdt = 0.1;
                    }
                    let mut s = scrub() + (scrub_dir as f64 * 35.0 * sdt as f64 * 1e9) as i64;
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
                }
            }
            // hide the idle pointer during playback
            if playing && !cur_hidden && !ptr_drag && last_ptr_motion != 0 && now.wrapping_sub(last_ptr_motion) > 3000 {
                SDL_webOSCursorVisibility(0);
                cur_hidden = true;
            }
            // re-pause after a resume once the seek's frame is on screen
            if resume_pend() && playing && !paused()
                && seek_pending() < 0 && frames() >= 3 && playpos() + 15 * 1_000_000_000 >= bg_pos
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
            crate::posters::poster_pump(3);

            glViewport(0, 0, SCR_W, SCR_H);
            if playing {
                crate::system::clear_opaque_region();
                glClearColor(0.0, 0.0, 0.0, 0.0);
                glClear(GL_COLOR_BUFFER_BIT);
                if now < hud_until() || paused() {
                    crate::ui::player_hud::draw_hud();
                }
                SDL_GL_SwapWindow(win);
                frames_ct += 1;
                if now.wrapping_sub(fps_t) >= 1000 {
                    frames_ct = 0;
                    fps_t = now;
                }
                continue;
            }
            crate::ui::home::home_draw();
            let fps_col = [0.4f32, 1.0, 0.55, 1.0];
            crate::gfx::draw_number(fps_shown, SCR_W as f32 - 70.0, 64.0, 46.0, fps_col.as_ptr());
            SDL_GL_SwapWindow(win);
            frames_ct += 1;
            if now.wrapping_sub(fps_t) >= 1000 {
                fps_shown = (frames_ct as f32 * 1000.0 / now.wrapping_sub(fps_t) as f32 + 0.5) as i32;
                frames_ct = 0;
                fps_t = now;
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
