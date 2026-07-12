//! "Who's watching" — the Plex Home profile picker (+ the PIN keypad for protected users).
//!
//! The avatar row is the **shared** [`CardRow`] with a circular [`RowStyle::PROFILES`], so the big
//! round profile pictures get the exact same focus-magnification springs, scroll, and glow ring as
//! the poster/cast/Related shelves — no forked row logic. The screen reads its roster + flow phase
//! from [`crate::auth`]; picking a profile drives `auth::select_profile` / `auth::submit_pin`.
#![allow(non_upper_case_globals)]
use crate::auth::{self, Phase};
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::consts::*;
use crate::ui::widgets::{Art, Spinner};
use crate::ui::{theme, Env, Painter, Rect, View};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::ptr::addr_of_mut;

const ROW_Y: f32 = 384.0;
const PIN_LEN: usize = 4;

// keypad: 4 rows × 3 cols. b'D' = delete; None = an empty (unfocusable) cell.
const KEYS: [[Option<u8>; 3]; 4] = [
    [Some(b'1'), Some(b'2'), Some(b'3')],
    [Some(b'4'), Some(b'5'), Some(b'6')],
    [Some(b'7'), Some(b'8'), Some(b'9')],
    [Some(b'D'), Some(b'0'), None],
];

/// The PIN keypad overlay state (open for a protected profile).
struct Pad {
    open: bool,
    target: usize, // user index the PIN unlocks
    entry: String,
    fr: c_int,
    fc: c_int,
}
impl Pad {
    const fn new() -> Self {
        Pad { open: false, target: 0, entry: String::new(), fr: 0, fc: 0 }
    }
}

struct Scene {
    row: CardRow,
    fc: c_int,
    spin_ms: f32,
    pad: Pad,
}

static mut SCENE: Option<Scene> = None;

fn scene() -> &'static mut Scene {
    unsafe { (*addr_of_mut!(SCENE)).as_mut().expect("profiles::init not called") }
}

pub fn init() {
    unsafe {
        *addr_of_mut!(SCENE) = Some(Scene { row: CardRow::new(), fc: 0, spin_ms: 0.0, pad: Pad::new() });
    }
}

/// Reset focus when the screen (re)appears — call when routing into Profiles.
pub fn enter() {
    let s = scene();
    s.fc = 0;
    s.pad = Pad::new();
}

pub fn update(dt: f32) {
    let s = scene();
    s.spin_ms += dt * 1000.0;
    let n = auth::users().len();
    if s.fc as usize >= n.max(1) {
        s.fc = 0;
    }
    // freeze the row focus (springs settle to unfocused) while the keypad is up
    let focus = if s.pad.open { None } else { Some(s.fc as usize) };
    s.row.update(n, focus, &RowStyle::PROFILES, dt);
}

pub fn draw() {
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    let p = Painter::root();
    p.rect(Rect::FULL, 0.0, theme::SURFACE_APP, theme::SURFACE_APP, 0.0);
    let s = scene();
    let users = auth::users();
    let env = Env { dt: 0.0, screen: Rect::FULL, fr: 0, fc: s.fc, sp: 0.0, hero_a: 0.0 };

    // title
    if let Ok(t) = CString::new("Who's watching?") {
        p.text(t.as_ptr(), SCR_W as f32 * 0.5, 168.0, theme::size::HERO, theme::TEXT_PRIMARY, 1, 1);
    }

    // avatar row — centered when the roster fits, else left-aligned so CardRow can scroll it.
    let sty = RowStyle::PROFILES;
    let n = users.len();
    let slot = sty.w + sty.gap;
    let total = (n as f32 * slot - sty.gap).max(0.0);
    let start_x = ((SCR_W as f32 - total) * 0.5).max(sty.margin_x);
    let scroll = s.row.scroll_x();

    let mut focused: Option<usize> = None;
    for (i, u) in users.iter().enumerate() {
        let cx = start_x + i as f32 * slot + sty.w * 0.5 - scroll;
        let base = Rect::new(cx - sty.w * 0.5, ROW_Y, sty.w, sty.h);
        let sc = s.row.scale(i);
        let is_foc = i as c_int == s.fc && !s.pad.open;
        if is_foc {
            focused = Some(i);
            continue; // draw the focused tile last (ring over neighbours)
        }
        card_row::draw_tile(p, Art::Thumb { key: &u.thumb, res: (300, 300) }, base.scaled(sc), sc, &sty, None);
        draw_name(p, u, cx, false);
    }
    if let Some(i) = focused {
        let u = &users[i];
        let cx = start_x + i as f32 * slot + sty.w * 0.5 - scroll;
        let base = Rect::new(cx - sty.w * 0.5, ROW_Y, sty.w, sty.h);
        let sc = s.row.scale(i);
        card_row::draw_focused(p, Art::Thumb { key: &u.thumb, res: (300, 300) }, base.scaled(sc), sc, &sty, None, std::ptr::null());
        draw_name(p, u, cx, true);
    }

    // switching spinner / keypad overlay
    match auth::phase() {
        Phase::Switching if !s.pad.open => {
            p.rect(Rect::FULL, 0.0, theme::scrim_black(0.55), theme::scrim_black(0.55), 0.0);
            Spinner::new(SCR_W as f32 * 0.5, 500.0, 26.0)
                .phase(s.spin_ms as u32)
                .tint(theme::TEXT_PRIMARY)
                .draw(&env, p);
        }
        _ => {}
    }
    if s.pad.open {
        draw_pad(p, &env, s, &users);
    }
}

fn draw_name(p: Painter, u: &auth::UserTile, cx: f32, focused: bool) {
    let col = if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
    let name = crate::text::elide(&u.title, RowStyle::PROFILES.w + RowStyle::PROFILES.gap - 12.0, theme::size::LABEL, if focused { 1 } else { 0 }, false);
    if let Ok(nc) = CString::new(name) {
        p.text(nc.as_ptr(), cx, ROW_Y + RowStyle::PROFILES.h + 28.0, theme::size::LABEL, col, 1, if focused { 1 } else { 0 });
    }
}

fn draw_pad(p: Painter, env: &Env, s: &Scene, users: &[auth::UserTile]) {
    p.rect(Rect::FULL, 0.0, theme::scrim_black(0.72), theme::scrim_black(0.72), 0.0);
    let name = users.get(s.pad.target).map(|u| u.title.as_str()).unwrap_or("");
    if let Ok(t) = CString::new(format!("Enter {name}'s PIN")) {
        p.text(t.as_ptr(), SCR_W as f32 * 0.5, 300.0, theme::size::TITLE, theme::TEXT_PRIMARY, 1, 1);
    }
    // 4 entry dots
    let dot = 18.0f32;
    let dgap = 34.0f32;
    let dw = PIN_LEN as f32 * dot + (PIN_LEN as f32 - 1.0) * dgap;
    let mut dx = SCR_W as f32 * 0.5 - dw * 0.5;
    for i in 0..PIN_LEN {
        let filled = i < s.pad.entry.len();
        let col = theme::with_a(theme::TEXT_PRIMARY, if filled { 1.0 } else { 0.28 });
        p.rect(Rect::new(dx, 372.0, dot, dot), dot * 0.5, col, col, 0.0);
        dx += dot + dgap;
    }
    // keypad grid
    let key = 108.0f32;
    let kg = 20.0f32;
    let gw = 3.0 * key + 2.0 * kg;
    let gx = SCR_W as f32 * 0.5 - gw * 0.5;
    let gy = 452.0f32;
    for (r, row) in KEYS.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            let Some(k) = cell else { continue };
            let rect = Rect::new(gx + c as f32 * (key + kg), gy + r as f32 * (key + kg), key, key);
            let foc = r as c_int == s.pad.fr && c as c_int == s.pad.fc;
            let (fill, ink) = if foc {
                (theme::ACCENT, theme::ACCENT_INK)
            } else {
                (theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_INK)
            };
            p.rect(rect, 18.0, fill, fill, 0.0);
            let label = if *k == b'D' { "⌫".to_string() } else { (*k as char).to_string() };
            if let Ok(lc) = CString::new(label) {
                let ty = crate::text::text_vcenter_y(theme::size::TITLE, 1, rect.y + rect.h * 0.5);
                p.text(lc.as_ptr(), rect.x + rect.w * 0.5, ty, theme::size::TITLE, ink, 1, 1);
            }
        }
    }
    let _ = env;
}

pub fn key(sym: c_uint, wcode: c_uint) {
    let s = scene();
    if s.pad.open {
        pad_key(s, sym, wcode);
        return;
    }
    let n = auth::users().len() as c_int;
    if n == 0 {
        return;
    }
    if sym == SDLK_LEFT {
        s.fc = (s.fc - 1).max(0);
    } else if sym == SDLK_RIGHT {
        s.fc = (s.fc + 1).min(n - 1);
    } else if is_ok(sym) {
        let idx = s.fc as usize;
        let protected = auth::users().get(idx).map(|u| u.protected).unwrap_or(false);
        if protected {
            s.pad = Pad { open: true, target: idx, entry: String::new(), fr: 0, fc: 0 };
        } else {
            auth::select_profile(idx);
        }
    }
    // BACK on the picker does nothing — you must choose a profile.
}

fn pad_key(s: &mut Scene, sym: c_uint, wcode: c_uint) {
    if is_back(sym, wcode) {
        s.pad = Pad::new();
        return;
    }
    if sym == SDLK_LEFT {
        s.pad.fc = step_focus(s.pad.fr, s.pad.fc, -1, true);
    } else if sym == SDLK_RIGHT {
        s.pad.fc = step_focus(s.pad.fr, s.pad.fc, 1, true);
    } else if sym == SDLK_UP {
        s.pad.fr = (s.pad.fr - 1).max(0);
        s.pad.fc = nearest_col(s.pad.fr, s.pad.fc);
    } else if sym == SDLK_DOWN {
        s.pad.fr = (s.pad.fr + 1).min(KEYS.len() as c_int - 1);
        s.pad.fc = nearest_col(s.pad.fr, s.pad.fc);
    } else if is_ok(sym) {
        if let Some(k) = KEYS[s.pad.fr as usize][s.pad.fc as usize] {
            press(s, k);
        }
    }
}

/// Move the keypad column, skipping empty cells; clamps at the row edges.
fn step_focus(fr: c_int, fc: c_int, dir: c_int, _skip: bool) -> c_int {
    let row = &KEYS[fr as usize];
    let mut c = fc + dir;
    while c >= 0 && (c as usize) < row.len() {
        if row[c as usize].is_some() {
            return c;
        }
        c += dir;
    }
    fc
}

/// Nearest occupied column in `fr` to `fc` (for vertical moves onto a row with an empty cell).
fn nearest_col(fr: c_int, fc: c_int) -> c_int {
    let row = &KEYS[fr as usize];
    if row.get(fc as usize).map(|c| c.is_some()).unwrap_or(false) {
        return fc;
    }
    for d in 1..3 {
        for c in [fc - d, fc + d] {
            if c >= 0 && (c as usize) < row.len() && row[c as usize].is_some() {
                return c;
            }
        }
    }
    0
}

fn press(s: &mut Scene, k: u8) {
    if k == b'D' {
        s.pad.entry.pop();
        return;
    }
    if s.pad.entry.len() < PIN_LEN {
        s.pad.entry.push(k as char);
    }
    if s.pad.entry.len() == PIN_LEN {
        let (idx, pin) = (s.pad.target, s.pad.entry.clone());
        auth::submit_pin(idx, &pin);
        s.pad = Pad::new(); // the flow shows a spinner; a wrong PIN returns to the picker
    }
}
