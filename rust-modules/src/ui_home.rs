//! Rust port of src/ui_home.c — gallery/home model + view + navigation (ui_home.h).
//! Same C ABI: fr/fc/snapTarget are shared globals the C event loop (main.c) drives;
//! movie_at + home_* are the entry points. Draw is composed from a few reusable
//! component helpers (card/pill_button/circle_button/label) — the start of the
//! UIKit-style element set the user asked for — over the Rust gfx/text/posters.
#![allow(non_upper_case_globals)]
use crate::gfx::{draw_ambient, draw_ptri, draw_rect, draw_rrect, draw_tex, spring};
use crate::pms::PmsMovie;
use crate::posters::{poster_get, poster_key, poster_wh};
use crate::text::draw_text;
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uint};
use std::ptr::{addr_of, addr_of_mut};

const ROWS: usize = 5;
const COLS: usize = 10;
const CARD_W: f32 = 250.0;
const CARD_H: f32 = 375.0;
const GAP: f32 = 30.0;
const MARGIN_X: f32 = 90.0;
const ROW_TITLE_H: f32 = 30.0;
const ROW_PITCH: f32 = CARD_H + ROW_TITLE_H + 54.0;
const CONTENT_Y: f32 = 200.0;
const GLOW_PAD: f32 = 48.0;
const SCR_W: f32 = 1920.0;
const SCR_H: f32 = 1080.0;

const SDLK_RIGHT: c_uint = 79 | (1 << 30);
const SDLK_LEFT: c_uint = 80 | (1 << 30);
const SDLK_DOWN: c_uint = 81 | (1 << 30);
const SDLK_UP: c_uint = 82 | (1 << 30);
const GL_COLOR_BUFFER_BIT: c_uint = 0x0000_4000;

extern "C" {
    fn glClearColor(r: f32, g: f32, b: f32, a: f32);
    fn glClear(mask: c_uint);
}

// shared with the C event loop (main.c reads/writes these via ui_home.h externs)
#[no_mangle]
pub static mut fr: c_int = 0;
#[no_mangle]
pub static mut fc: c_int = 0;
#[no_mangle]
pub static mut snapTarget: f32 = 0.0;

// private animation state
static mut SCALE: [[f32; COLS]; ROWS] = [[1.0; COLS]; ROWS];
static mut SCALEV: [[f32; COLS]; ROWS] = [[0.0; COLS]; ROWS];
static mut SCROLLX: [f32; ROWS] = [0.0; ROWS];
static mut SCROLLXV: [f32; ROWS] = [0.0; ROWS];
static mut SCROLLY: f32 = 0.0;
static mut SCROLLYV: f32 = 0.0;
static mut SNAPPOS: f32 = 0.0;
static mut SNAPVEL: f32 = 0.0;
static mut BGPHASE: f32 = 0.0;

/// map a grid cell to a catalog movie (flat all-movies grid, row-major)
#[no_mangle]
pub extern "C" fn movie_at(r: c_int, c: c_int) -> *mut PmsMovie {
    let idx = r * COLS as c_int + c;
    let n = unsafe { addr_of!(crate::pms::pms_nmovies).read() };
    if idx >= 0 && idx < n {
        let movies = addr_of_mut!(crate::pms::pms_movies) as *mut PmsMovie;
        unsafe { movies.add(idx as usize) }
    } else {
        std::ptr::null_mut()
    }
}

// ---- reusable component helpers (the beginnings of a UIKit-like element set) ----

const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// a poster card: artwork if loaded, else a dark skeleton rect
fn card(m: *const PmsMovie, cx: f32, cy: f32, w: f32, h: f32, rad: f32) {
    const SK_T: [f32; 4] = [0.13, 0.14, 0.17, 1.0];
    const SK_B: [f32; 4] = [0.08, 0.09, 0.11, 1.0];
    unsafe {
        if !m.is_null() && (*m).thumb[0] != 0 {
            let mut key = [0u8; 352];
            poster_key(key.as_mut_ptr() as *mut c_char, key.len(), (*m).thumb.as_ptr() as *const c_char, 250, 375, 0);
            let t = poster_get(key.as_ptr() as *const c_char);
            if t != 0 {
                draw_tex(t, cx, cy, w, h, rad, WHITE.as_ptr());
                return;
            }
        }
        draw_rect(cx, cy, w, h, 0.0, rad, SK_T.as_ptr(), SK_B.as_ptr(), 0.0);
    }
}

/// a text label (thin wrapper over draw_text). align: 0 L, 1 C, 2 R.
fn label(s: &str, x: f32, y: f32, sz: c_int, col: &[f32; 4], align: c_int, bold: c_int) {
    if let Ok(cs) = CString::new(s) {
        draw_text(cs.as_ptr(), x, y, sz, col.as_ptr(), align, bold);
    }
}
fn label_c(s: *const c_char, x: f32, y: f32, sz: c_int, col: &[f32; 4], align: c_int, bold: c_int) {
    draw_text(s, x, y, sz, col.as_ptr(), align, bold);
}

/// primary "Play" pill: rounded fill + play triangle + label, as a group
fn pill_button(x: f32, y: f32, w: f32, h: f32, fill: &[f32; 4], ink: &[f32; 4]) {
    draw_rrect(x, y, w, h, h * 0.5, h * 0.5, fill.as_ptr());
    let tri_h = h * 0.40;
    draw_ptri(x + 40.0, y + (h - tri_h) * 0.5, tri_h, tri_h, ink.as_ptr());
    label("Play", x + 76.0, y + (h - 30.0) * 0.5 - 1.0, 30, ink, 0, 1);
}

/// circular secondary button with a centered glyph
fn circle_button(x: f32, y: f32, d: f32, glyph: &str, face: &[f32; 4], gly: &[f32; 4]) {
    draw_rect(x, y, d, d, 0.0, d * 0.5, face.as_ptr(), face.as_ptr(), 0.0);
    label(glyph, x + d * 0.5, y + (d - 32.0) * 0.5 - 2.0, 32, gly, 1, 1);
}

// ---- model / navigation ----

#[no_mangle]
pub extern "C" fn home_init() {
    unsafe {
        let scale = &mut *addr_of_mut!(SCALE);
        for row in scale.iter_mut() {
            for s in row.iter_mut() {
                *s = 1.0;
            }
        }
    }
}

/// vertical move keeps VISUAL alignment: pick the card under the focused one
fn vert_move(dir: c_int) {
    unsafe {
        let sx = &*addr_of!(SCROLLX);
        let (f_r, f_c) = (fr, fc);
        let nr = f_r + dir;
        let cx = MARGIN_X + f_c as f32 * (CARD_W + GAP) - sx[f_r as usize] + CARD_W * 0.5;
        let mut nc = ((cx - MARGIN_X - CARD_W * 0.5 + sx[nr as usize]) / (CARD_W + GAP) + 0.5) as c_int;
        if nc < 0 {
            nc = 0;
        }
        if nc > COLS as c_int - 1 {
            nc = COLS as c_int - 1;
        }
        fr = nr;
        fc = nc;
    }
}

#[no_mangle]
pub extern "C" fn home_move_focus(s: c_uint) {
    unsafe {
        if s == SDLK_LEFT && fc > 0 {
            fc -= 1;
        } else if s == SDLK_RIGHT && fc < COLS as c_int - 1 {
            fc += 1;
        } else if s == SDLK_UP && fr > 0 {
            vert_move(-1);
        } else if s == SDLK_DOWN && fr < ROWS as c_int - 1 {
            vert_move(1);
        }
    }
}

#[no_mangle]
pub extern "C" fn home_pointer_focus(mx: f32, my: f32) {
    unsafe {
        let sx = &*addr_of!(SCROLLX);
        let sy = SCROLLY;
        for r in 0..ROWS {
            let row_y = CONTENT_Y + r as f32 * ROW_PITCH - sy + ROW_TITLE_H + 18.0;
            if my < row_y || my > row_y + CARD_H {
                continue;
            }
            for c in 0..COLS {
                let x = MARGIN_X + c as f32 * (CARD_W + GAP) - sx[r];
                if mx >= x && mx <= x + CARD_W {
                    fr = r as c_int;
                    fc = c as c_int;
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn home_wheel(dy: c_int) {
    unsafe {
        if dy < 0 && fr < ROWS as c_int - 1 {
            fr += 1;
        } else if dy > 0 && fr > 0 {
            fr -= 1;
        }
    }
}

#[no_mangle]
pub extern "C" fn home_update(dt: f32) {
    unsafe {
        BGPHASE += dt * 0.15;
        let (f_r, f_c) = (fr as usize, fc as usize);
        let scale = &mut *addr_of_mut!(SCALE);
        let scalev = &mut *addr_of_mut!(SCALEV);
        for r in 0..ROWS {
            for c in 0..COLS {
                let target = if r == f_r && c == f_c { 1.055 } else { 1.0 };
                spring(&mut scale[r][c], &mut scalev[r][c], target, 320.0, dt);
            }
        }
        let max_sx = COLS as f32 * (CARD_W + GAP) - GAP - (SCR_W - 2.0 * MARGIN_X);
        let mut want = f_c as f32 * (CARD_W + GAP) - (CARD_W + GAP);
        if want < 0.0 {
            want = 0.0;
        }
        if want > max_sx {
            want = max_sx;
        }
        let sx = &mut *addr_of_mut!(SCROLLX);
        let sxv = &mut *addr_of_mut!(SCROLLXV);
        spring(&mut sx[f_r], &mut sxv[f_r], want, 170.0, dt);

        let mut want_y = f_r as f32 * ROW_PITCH - ROW_PITCH * 0.6;
        if want_y < 0.0 {
            want_y = 0.0;
        }
        let mut max_y = ROWS as f32 * ROW_PITCH - (SCR_H - CONTENT_Y) + 60.0;
        if max_y < 0.0 {
            max_y = 0.0;
        }
        if want_y > max_y {
            want_y = max_y;
        }
        spring(&mut *addr_of_mut!(SCROLLY), &mut *addr_of_mut!(SCROLLYV), want_y, 170.0, dt);
        spring(&mut *addr_of_mut!(SNAPPOS), &mut *addr_of_mut!(SNAPVEL), snapTarget, 200.0, dt);
    }
}

#[no_mangle]
pub extern "C" fn home_draw() {
    unsafe {
        glClearColor(0.03, 0.03, 0.045, 1.0);
        glClear(GL_COLOR_BUFFER_BIT);

        let sp = SNAPPOS;
        let hero_a = (1.0 - sp / 0.55).clamp(0.0, 1.0);
        let shelf_top_y = 828.0 + (150.0 - 828.0) * sp;
        let hero = movie_at(0, 0);

        // ambient UltraBlurColors wash + hero backdrop
        let mut bt = 0u32;
        let mut bk = [0u8; 352];
        if !hero.is_null() && (*hero).art[0] != 0 {
            poster_key(bk.as_mut_ptr() as *mut c_char, bk.len(), (*hero).art.as_ptr() as *const c_char, 1280, 720, 0);
            bt = poster_get(bk.as_ptr() as *const c_char);
        }
        if !hero.is_null() && (*hero).has_blur != 0 && (sp > 0.004 || bt == 0) {
            let b = &(*hero).blur;
            draw_ambient(0.0, 0.0, SCR_W, SCR_H, 0.55, b[0].as_ptr(), b[1].as_ptr(), b[2].as_ptr(), b[3].as_ptr());
        }
        if bt != 0 && sp < 0.996 {
            let bd_tint = [1.0, 1.0, 1.0, 1.0 - sp];
            draw_tex(bt, 0.0, -sp * (SCR_H - 120.0), SCR_W, SCR_H, 0.0, bd_tint.as_ptr());
        }
        if hero_a > 0.01 {
            let sa = 0.30 + 0.64 * hero_a;
            let scrim_t = [0.02, 0.02, 0.03, 0.0];
            let scrim_b = [0.02, 0.02, 0.03, sa];
            draw_rect(0.0, SCR_H * 0.46, SCR_W, SCR_H * 0.54, 0.0, 0.0, scrim_t.as_ptr(), scrim_b.as_ptr(), 0.0);
        }

        // hero content (low-left), fades as the grid rises
        if !hero.is_null() && hero_a > 0.01 {
            let tx = MARGIN_X;
            let title_y = 510.0f32;
            let w_a = [0.97, 0.98, 0.99, hero_a];
            let d_a = [0.70, 0.73, 0.78, hero_a];
            // title: clearLogo (transparent PNG) if loaded, else bold text
            let mut lt = 0u32;
            let (mut lw, mut lh) = (0i32, 0i32);
            if (*hero).rk[0] != 0 {
                let rk = cfield(&(*hero).rk);
                if let Ok(lpath) = CString::new(format!("/library/metadata/{rk}/clearLogo")) {
                    let mut lk = [0u8; 352];
                    poster_key(lk.as_mut_ptr() as *mut c_char, lk.len(), lpath.as_ptr(), 600, 240, 1);
                    lt = poster_get(lk.as_ptr() as *const c_char);
                    poster_wh(lk.as_ptr() as *const c_char, &mut lw, &mut lh);
                }
            }
            if lt != 0 && lh > 0 {
                let mut hh = 96.0f32;
                let mut ww = hh * lw as f32 / lh as f32;
                if ww > 660.0 {
                    ww = 660.0;
                    hh = ww * lh as f32 / lw as f32;
                }
                draw_tex(lt, tx, title_y + 80.0 - hh, ww, hh, 0.0, w_a.as_ptr());
            } else {
                label_c((*hero).title.as_ptr() as *const c_char, tx, title_y, 66, &w_a, 0, 1);
            }
            let rating = cfield(&(*hero).rating);
            let meta = format!("Movie \u{b7} {} \u{b7} {}", (*hero).year, if rating.is_empty() { "NR" } else { &rating });
            label(&meta, tx, title_y + 92.0, 26, &d_a, 0, 0);
            // synopsis wrapped to two lines on a word boundary
            let summary = cfield(&(*hero).summary);
            if !summary.is_empty() {
                let (l1, l2) = wrap_two(&summary);
                label(&l1, tx, title_y + 128.0, 24, &d_a, 0, 0);
                if !l2.is_empty() {
                    label(&l2, tx, title_y + 158.0, 24, &d_a, 0, 0);
                }
            }
            // Play pill + secondary buttons + page dots
            let pill_w = 168.0;
            let pill_h = 60.0;
            let pill_y = title_y + 200.0;
            let pill_c = [0.97, 0.98, 0.99, hero_a];
            let ink = [0.05, 0.06, 0.08, hero_a];
            pill_button(tx, pill_y, pill_w, pill_h, &pill_c, &ink);
            let c_d = 60.0;
            let c_gap = 20.0;
            let circ = [0.42, 0.44, 0.50, 0.5 * hero_a];
            let gly = [0.92, 0.94, 0.97, hero_a];
            for (b, glyph) in ["+", "i", ">"].iter().enumerate() {
                let bx = tx + pill_w + c_gap + b as f32 * (c_d + c_gap);
                circle_button(bx, pill_y, c_d, glyph, &circ, &gly);
            }
            let dot_y = pill_y + pill_h + 24.0;
            for d in 0..8 {
                let dw = if d == 0 { 26.0 } else { 11.0 };
                let dc = [0.85, 0.87, 0.9, (if d == 0 { 0.95 } else { 0.35 }) * hero_a];
                draw_rect(tx + d as f32 * 20.0, dot_y, dw, 11.0, 0.0, 5.5, dc.as_ptr(), dc.as_ptr(), 0.0);
            }
        }

        // shelves: peek at the bottom in hero mode, full grid when snapped
        let scale = &*addr_of!(SCALE);
        let sx = &*addr_of!(SCROLLX);
        let sy = SCROLLY;
        for r in 0..ROWS {
            let row_y = shelf_top_y + r as f32 * ROW_PITCH - sy * sp;
            if row_y > SCR_H || row_y + CARD_H < 0.0 {
                continue;
            }
            if movie_at(r as c_int, 0).is_null() {
                continue;
            }
            for c in 0..COLS {
                if r == fr as usize && c == fc as usize && sp > 0.5 {
                    continue; // focused drawn last
                }
                let m = movie_at(r as c_int, c as c_int);
                if m.is_null() {
                    continue;
                }
                let x = MARGIN_X + c as f32 * (CARD_W + GAP) - sx[r] * sp;
                if x > SCR_W || x + CARD_W < -GLOW_PAD {
                    continue;
                }
                let s = scale[r][c];
                let (w, h) = (CARD_W * s, CARD_H * s);
                let cx = x - (w - CARD_W) / 2.0;
                let cy = (row_y + 12.0) - (h - CARD_H) / 2.0;
                card(m, cx, cy, w, h, 14.0 * s);
            }
        }
        // focused card ring + label — grid mode only
        if sp > 0.5 {
            let m = movie_at(fr, fc);
            let row_y = shelf_top_y + fr as f32 * ROW_PITCH - sy * sp;
            let x = MARGIN_X + fc as f32 * (CARD_W + GAP) - sx[fr as usize] * sp;
            let s = scale[fr as usize][fc as usize];
            let (w, h) = (CARD_W * s, CARD_H * s);
            let cx = x - (w - CARD_W) / 2.0;
            let cy = (row_y + 12.0) - (h - CARD_H) / 2.0;
            card(m, cx, cy, w, h, 14.0 * s);
            let clear0 = [0.0, 0.0, 0.0, 0.0];
            draw_rect(cx - GLOW_PAD, cy - GLOW_PAD, w + 2.0 * GLOW_PAD, h + 2.0 * GLOW_PAD, GLOW_PAD, 14.0 * s, clear0.as_ptr(), clear0.as_ptr(), (s - 1.0) / 0.055);
            if !m.is_null() {
                let lc = [0.96, 0.97, 0.98, 1.0];
                label_c((*m).title.as_ptr() as *const c_char, cx + w * 0.5, cy + h + 12.0, 26, &lc, 1, 1);
            }
        }
    }
}

/// read a NUL-terminated C-string field into a Rust String
fn cfield(b: &[u8]) -> String {
    let n = b.iter().position(|&x| x == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).into_owned()
}

/// wrap a synopsis to two lines on a word boundary (mirrors the C's rule)
fn wrap_two(s: &str) -> (String, String) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut brk = n;
    if n > 62 {
        brk = 62;
        while brk > 24 && bytes[brk] != b' ' {
            brk -= 1;
        }
    }
    let c1 = brk.min(87);
    let l1 = String::from_utf8_lossy(&bytes[..c1]).into_owned();
    if brk >= n {
        return (l1, String::new());
    }
    let s2 = &bytes[brk + 1..];
    let m = s2.len();
    let mut c2 = m;
    if m > 66 {
        c2 = 66;
        while c2 > 24 && s2[c2] != b' ' {
            c2 -= 1;
        }
    }
    c2 = c2.min(92);
    let mut l2 = String::from_utf8_lossy(&s2[..c2]).into_owned();
    if c2 < m {
        l2.push('\u{2026}'); // …
    }
    (l1, l2)
}
