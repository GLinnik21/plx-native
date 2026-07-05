//! In-player modal track menu: audio + subtitle pickers over the video. Reads the
//! playing item's tracks from crate::metadata; app.rs routes D-pad/OK/BACK here while
//! the menu is open. This increment records the selection only — the audio switch and
//! the subtitle render are wired in later increments (audio_stream_id/sub_stream_id
//! accessors are ready for them).
#![allow(dead_code)]
use crate::gfx::{draw_rect, draw_rrect};
use crate::metadata;
use crate::text::draw_text;
use crate::ui::consts::{SDLK_DOWN, SDLK_LEFT, SDLK_RIGHT, SDLK_UP};
use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

const SCR_W: f32 = 1920.0;
const SCR_H: f32 = 1080.0;

static mut OPEN: bool = false;
static mut TAB: c_int = 0; // 0=Audio, 1=Subtitles
static mut SEL: c_int = 0; // focused row within the tab
static mut ACTIVE_AUDIO: c_int = 0; // index into the audio list
static mut ACTIVE_SUB: c_int = -1; // -1 = Off, else index into the subs list

pub(crate) fn is_open() -> bool {
    unsafe { addr_of!(OPEN).read() }
}
/// index into metadata audio list of the chosen audio track
pub(crate) fn active_audio() -> c_int {
    unsafe { addr_of!(ACTIVE_AUDIO).read() }
}
/// -1 = subtitles off, else index into the metadata subs list
pub(crate) fn active_sub() -> c_int {
    unsafe { addr_of!(ACTIVE_SUB).read() }
}
/// Plex stream id of the chosen audio track (for &audioStreamID), or 0
pub(crate) fn audio_stream_id() -> i64 {
    let i = active_audio();
    metadata::current().and_then(|d| d.audio.get(i.max(0) as usize)).map(|s| s.id).unwrap_or(0)
}
/// Plex stream id of the chosen subtitle track (for &subtitleStreamID), or 0 if Off
pub(crate) fn sub_stream_id() -> i64 {
    let i = active_sub();
    if i < 0 {
        return 0;
    }
    metadata::current().and_then(|d| d.subs.get(i as usize)).map(|s| s.id).unwrap_or(0)
}

fn n_audio() -> c_int {
    metadata::current().map(|d| d.audio.len()).unwrap_or(0) as c_int
}
fn n_sub() -> c_int {
    metadata::current().map(|d| d.subs.len()).unwrap_or(0) as c_int
}
/// rows in a tab — Subtitles has a leading "Off" row
fn n_rows(tab: c_int) -> c_int {
    if tab == 0 {
        n_audio()
    } else {
        n_sub() + 1
    }
}
/// the row that should be focused when entering `tab` (its active selection)
fn sel_for_tab(tab: c_int) -> c_int {
    if tab == 0 {
        active_audio().max(0)
    } else {
        let a = active_sub();
        if a < 0 {
            0
        } else {
            a + 1
        } // +1 for the leading Off row
    }
}

pub(crate) fn open() {
    unsafe {
        let tab = addr_of!(TAB).read();
        addr_of_mut!(SEL).write(sel_for_tab(tab));
        addr_of_mut!(OPEN).write(true);
    }
}
/// open focused on a specific tab (used by the on-screen audio/subs icons)
pub(crate) fn open_tab(tab: c_int) {
    unsafe { addr_of_mut!(TAB).write(tab) }
    open();
}
pub(crate) fn close() {
    unsafe { addr_of_mut!(OPEN).write(false) }
}

pub(crate) fn move_focus(sym: c_int) {
    let sym = sym as u32;
    unsafe {
        let tab = addr_of!(TAB).read();
        if sym == SDLK_UP || sym == SDLK_DOWN {
            let n = n_rows(tab);
            if n <= 0 {
                return;
            }
            let s = addr_of!(SEL).read();
            let ns = if sym == SDLK_UP { (s - 1).max(0) } else { (s + 1).min(n - 1) };
            addr_of_mut!(SEL).write(ns);
        } else if sym == SDLK_LEFT || sym == SDLK_RIGHT {
            let nt = if sym == SDLK_LEFT { 0 } else { 1 };
            if nt != tab {
                addr_of_mut!(TAB).write(nt);
                addr_of_mut!(SEL).write(sel_for_tab(nt));
            }
        }
    }
}

/// commit the focused row as the active track for its tab, then close
pub(crate) fn on_ok() {
    unsafe {
        let tab = addr_of!(TAB).read();
        let sel = addr_of!(SEL).read();
        addr_of_mut!(OPEN).write(false);
        if tab == 0 {
            let changed = addr_of!(ACTIVE_AUDIO).read() != sel;
            addr_of_mut!(ACTIVE_AUDIO).write(sel);
            // switch the audio track (fresh transcode with the chosen source audio)
            if changed {
                crate::player::request_audio_switch(audio_stream_id());
            }
        } else {
            addr_of_mut!(ACTIVE_SUB).write(sel - 1); // row 0 = Off = -1
            // subtitle rendering is wired in increment 3
        }
    }
}

// ---- labels ----
fn audio_label(i: usize) -> String {
    metadata::current()
        .and_then(|d| d.audio.get(i))
        .map(|s| {
            let lang = if s.lang.is_empty() { "Unknown".to_string() } else { s.lang.clone() };
            if s.title.is_empty() {
                format!("{} ({})", lang, s.codec.to_uppercase())
            } else {
                format!("{} \u{2014} {}", lang, s.title)
            }
        })
        .unwrap_or_default()
}
fn sub_label(i: usize) -> String {
    metadata::current()
        .and_then(|d| d.subs.get(i))
        .map(|s| {
            let lang = if s.lang.is_empty() { "Unknown".to_string() } else { s.lang.clone() };
            if s.title.is_empty() {
                lang
            } else {
                format!("{} \u{2014} {}", lang, s.title)
            }
        })
        .unwrap_or_default()
}

pub(crate) fn draw() {
    if !is_open() {
        return;
    }
    let (tab, sel) = unsafe { (addr_of!(TAB).read(), addr_of!(SEL).read()) };
    // dim the whole screen (modal focus; dims the video plane showing through)
    let dim = [0.0f32, 0.0, 0.0, 0.58];
    draw_rect(0.0, 0.0, SCR_W, SCR_H, 0.0, 0.0, dim.as_ptr(), dim.as_ptr(), 0.0);

    // right-side panel
    let pw = 660.0f32;
    let ph = 700.0f32;
    let px = SCR_W - 90.0 - pw;
    let py = (SCR_H - ph) * 0.5;
    let panel = [0.11f32, 0.12, 0.14, 0.97];
    draw_rrect(px, py, pw, ph, 24.0, 24.0, panel.as_ptr());

    let ix = px + 44.0;
    let white = [0.97f32, 0.98, 1.0, 1.0];
    let dimc = [0.52f32, 0.54, 0.60, 1.0];

    // tab headers
    let mut hx = ix;
    let ac = if tab == 0 { white } else { dimc };
    let sc = if tab == 1 { white } else { dimc };
    hx += draw_text(c"Audio".as_ptr(), hx, py + 46.0, 34, ac.as_ptr(), 0, 1) + 46.0;
    draw_text(c"Subtitles".as_ptr(), hx, py + 46.0, 34, sc.as_ptr(), 0, 1);

    // rows
    let n = n_rows(tab);
    let row_h = 64.0f32;
    let ry0 = py + 120.0;
    let active_audio = active_audio();
    let active_sub = active_sub();
    for r in 0..n {
        let ry = ry0 + r as f32 * row_h;
        let focused = r == sel;
        if focused {
            let hl = [1.0f32, 1.0, 1.0, 0.15];
            draw_rect(px + 18.0, ry - 10.0, pw - 36.0, row_h - 8.0, 0.0, 14.0, hl.as_ptr(), hl.as_ptr(), 0.0);
        }
        let label = if tab == 0 {
            audio_label(r as usize)
        } else if r == 0 {
            "Off".to_string()
        } else {
            sub_label((r - 1) as usize)
        };
        let ink = if focused { white } else { [0.84f32, 0.86, 0.91, 1.0] };
        if let Ok(cs) = CString::new(label) {
            draw_text(cs.as_ptr(), ix, ry + 10.0, 27, ink.as_ptr(), 0, focused as c_int);
        }
        // active marker: a filled dot (the app font has no checkmark glyph -> tofu)
        let active = if tab == 0 { r == active_audio } else { (r - 1) == active_sub };
        if active {
            let d = 15.0f32;
            draw_rect(px + pw - 52.0, ry + 12.0, d, d, 0.0, d * 0.5, white.as_ptr(), white.as_ptr(), 0.0);
        }
    }
}
