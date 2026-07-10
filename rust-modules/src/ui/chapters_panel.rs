//! In-player Chapters strip: a horizontal row of chapter cards (thumbnail + name + timestamp) over
//! the transport, opened from the HUD's Chapters tab. LEFT/RIGHT pick a chapter, OK seeks to its
//! start. Data from metadata::current().chapters (loaded with ?includeChapters=1). Card layout
//! mirrors the detail-page episode picker; modal wiring mirrors info_panel.
#![allow(dead_code)]
use crate::metadata;
use crate::ui::consts::{SDLK_LEFT, SDLK_RIGHT};
use crate::ui::{Painter, Rect, Spring};
use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

const SCR_W: f32 = 1920.0;
const SCR_H: f32 = 1080.0;
const MARGIN_X: f32 = 90.0;
const CH_W: f32 = 288.0;
const CH_H: f32 = 162.0; // 16:9 still
const CH_GAP: f32 = 24.0;
const CH_TOP: f32 = 684.0; // thumbnail top — name/time fit above the tabs (SCR_H-128)
use crate::ui::widgets::CARD_FOCUS_SCALE;

static mut OPEN: bool = false;
static mut SEL: c_int = 0;
static mut APPEAR: Spring = Spring::at(0.0);
static mut SCROLL: Spring = Spring::at(0.0); // horizontal scroll offset (px)
static mut SCALE: Spring = Spring::at(1.0); // focused-card pop (springs 1.0 → FOCUS_SCALE on each move)

pub(crate) fn is_open() -> bool {
    unsafe { addr_of!(OPEN).read() }
}
fn n() -> c_int {
    metadata::current().map(|d| d.chapters.len()).unwrap_or(0) as c_int
}
/// whether the current item has chapters — drives showing/hiding the Chapters tab
pub(crate) fn has_chapters() -> bool {
    n() > 0
}

pub(crate) fn open() {
    // focus the chapter that contains the current playhead
    let pos_ms = crate::player::playpos_ns() / 1_000_000;
    let sel = metadata::current()
        .map(|d| d.chapters.iter().rposition(|c| c.start_ms <= pos_ms).unwrap_or(0) as c_int)
        .unwrap_or(0);
    unsafe {
        addr_of_mut!(SEL).write(sel);
        addr_of_mut!(APPEAR).write(Spring::at(0.0));
        addr_of_mut!(SCROLL).write(Spring::at(scroll_target(sel)));
        addr_of_mut!(SCALE).write(Spring::at(1.0)); // pop in
        addr_of_mut!(OPEN).write(true);
    }
}
pub(crate) fn close() {
    unsafe { addr_of_mut!(OPEN).write(false) }
}
pub(crate) fn reset() {
    close();
    unsafe { addr_of_mut!(SEL).write(0) }
}

fn scroll_target(sel: c_int) -> f32 {
    // pin the focused card to the 2nd slot (like the episode picker)
    if sel > 1 {
        (sel as f32 - 1.0) * (CH_W + CH_GAP)
    } else {
        0.0
    }
}

pub(crate) fn move_focus(sym: c_int) {
    let sym = sym as u32;
    let nn = n();
    if nn == 0 {
        return;
    }
    let s = unsafe { addr_of!(SEL).read() };
    let ns = if sym == SDLK_LEFT {
        (s - 1).max(0)
    } else if sym == SDLK_RIGHT {
        (s + 1).min(nn - 1)
    } else {
        s
    };
    if ns != s {
        unsafe { &mut *addr_of_mut!(SCALE) }.jump(1.0); // re-pop the newly-focused card
    }
    unsafe { addr_of_mut!(SEL).write(ns) }
}

/// seek target (nanoseconds) for the focused chapter, or -1 if none. Closes the panel.
pub(crate) fn on_ok() -> i64 {
    let s = unsafe { addr_of!(SEL).read() };
    close();
    metadata::current()
        .and_then(|d| d.chapters.get(s.max(0) as usize))
        .map(|c| c.start_ms * 1_000_000)
        .unwrap_or(-1)
}

pub(crate) fn update(dt: f32) {
    if !is_open() {
        return;
    }
    let ap = unsafe { &mut *addr_of_mut!(APPEAR) };
    ap.step(1.0, 300.0, dt);
    crate::ui::anim::probe("chapters.appear", ap.pos, ap.vel, 1.0, dt);
    let sel = unsafe { addr_of!(SEL).read() };
    let sctgt = scroll_target(sel);
    let sc = unsafe { &mut *addr_of_mut!(SCROLL) };
    sc.step(sctgt, 220.0, dt);
    crate::ui::anim::probe("chapters.scroll", sc.pos, sc.vel, sctgt, dt);
    let scl = unsafe { &mut *addr_of_mut!(SCALE) };
    scl.step(CARD_FOCUS_SCALE, 300.0, dt);
    crate::ui::anim::probe("chapters.scale", scl.pos, scl.vel, CARD_FOCUS_SCALE, dt);
}

fn fmt_ts(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

pub(crate) fn draw() {
    if !is_open() {
        return;
    }
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    if d.chapters.is_empty() {
        return;
    }
    let appear = unsafe { addr_of!(APPEAR).read() }.pos.clamp(0.0, 1.0);
    let scroll = unsafe { addr_of!(SCROLL).read() }.pos;
    let sel = unsafe { addr_of!(SEL).read() };
    let scale = unsafe { addr_of!(SCALE).read() }.pos;
    let rise = (1.0 - appear) * 20.0;
    let p = Painter::root().alpha(appear).translate(-scroll, rise);

    let dimc = [0.62f32, 0.64, 0.68, 1.0];
    for (i, ch) in d.chapters.iter().enumerate() {
        let x = MARGIN_X + i as f32 * (CH_W + CH_GAP);
        if x - scroll > SCR_W || x - scroll + CH_W < 0.0 {
            continue; // culled off-screen
        }
        let focused = i as c_int == sel;
        let card = Rect::new(x, CH_TOP, CH_W, CH_H);
        crate::ui::widgets::draw_card(p, card, &ch.thumb, (480, 270), 10.0, focused, scale);
        // name + timestamp beneath the card
        let ty = CH_TOP + CH_H + 26.0;
        let titc = if focused { [0.98f32, 0.99, 1.0, 1.0] } else { [0.80f32, 0.82, 0.86, 1.0] };
        let name = if ch.title.trim().is_empty() {
            format!("Chapter {}", ch.index)
        } else {
            ch.title.clone()
        };
        if let Ok(tc) = CString::new(elide(&name, CH_W, 24, 1)) {
            p.text(tc.as_ptr(), x, ty, 24, titc, 0, 1);
        }
        if let Ok(sc) = CString::new(fmt_ts(ch.start_ms)) {
            p.text(sc.as_ptr(), x, ty + 30.0, 20, dimc, 0, 0);
        }
    }
}

/// truncate `s` with an ellipsis so it fits `budget` px at `sz`/`bold`.
fn elide(s: &str, budget: f32, sz: i32, bold: i32) -> String {
    let full = match CString::new(s) {
        Ok(c) => c,
        Err(_) => return s.to_string(),
    };
    if budget <= 0.0 || crate::text::text_width(full.as_ptr(), sz, bold) <= budget {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let mut cand: String = chars[..mid].iter().collect();
        cand.push('…');
        let fits = CString::new(cand.as_str())
            .ok()
            .map(|c| crate::text::text_width(c.as_ptr(), sz, bold) <= budget)
            .unwrap_or(false);
        if fits {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut out: String = chars[..lo].iter().collect();
    out.push('…');
    out
}
