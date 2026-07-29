//! In-player Chapters strip: a horizontal row of chapter cards (thumbnail + name + timestamp) over
//! the transport, opened from the HUD's Chapters tab. LEFT/RIGHT pick a chapter, OK seeks to its
//! start. Card layout mirrors the detail-page episode picker; modal wiring mirrors info_panel.
//!
//! Data comes from the PLAYING leaf (`metadata::playing_chapters`, loaded with `?includeChapters=1`
//! on the same fetch the track store already makes), never from `metadata::current()` — the same
//! identity rule `ui/track_menu.rs` and `ui/skip_pill.rs` state. Reading `current()` is what made
//! the Chapters tab vanish for every episode started from a show detail page: `current()` is then
//! the SHOW, and a show container carries no `Chapter[]`.
#![allow(dead_code)]
use crate::metadata;
use crate::ui::consts::{MARGIN_X, SCR_W, SDLK_LEFT, SDLK_RIGHT};
use crate::ui::popover::Popover;
use crate::ui::theme;
use crate::ui::{Rect, Spring};
use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

const CH_W: f32 = 288.0;
const CH_H: f32 = 162.0; // 16:9 still
const CH_GAP: f32 = 24.0;
const CH_TOP: f32 = 684.0; // thumbnail top — name/time fit above the tabs (SCR_H-128)
use crate::ui::widgets::CARD_FOCUS_SCALE;

static mut POP: Popover = Popover::new(); // shared open/appear choreography
static mut SEL: c_int = 0;
static mut SCROLL: Spring = Spring::at(0.0); // horizontal scroll offset (px)
static mut SCALE: Spring = Spring::at(1.0); // focused-card pop (springs 1.0 → FOCUS_SCALE on each move)

fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}

pub(crate) fn is_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}
/// the playing leaf's chapters — the ONE read, so within a frame the count, the open, the seek and
/// the draw cannot end up describing different items. ACROSS frames the store can still be replaced
/// (a new play retires it, `route::request_play`), which is why `update` re-clamps the selection.
fn chapters() -> &'static [metadata::Chapter] {
    metadata::playing_chapters()
}
fn n() -> c_int {
    chapters().len() as c_int
}
/// whether the PLAYING item has chapters — drives showing/hiding the Chapters tab
pub(crate) fn has_chapters() -> bool {
    n() > 0
}

pub(crate) fn open() {
    // focus the chapter that contains the current playhead
    let pos_ms = crate::player::playpos_ns() / 1_000_000;
    let sel = chapters().iter().rposition(|c| c.start_ms <= pos_ms).unwrap_or(0) as c_int;
    unsafe {
        addr_of_mut!(SEL).write(sel);
        addr_of_mut!(SCROLL).write(Spring::at(scroll_target(sel)));
        addr_of_mut!(SCALE).write(Spring::at(1.0)); // pop in
    }
    pop().open();
}
pub(crate) fn close() {
    pop().close();
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
    chapters().get(s.max(0) as usize).map(|c| c.start_ms * 1_000_000).unwrap_or(-1)
}

pub(crate) fn update(dt: f32) {
    if !is_open() {
        return;
    }
    pop().update(dt);
    // The store this indexes belongs to the PLAYING item and a new play retires it, so re-clamp
    // rather than spring the scroll toward a slot that no longer exists (which culls every card
    // and leaves an empty panel). `on_ok`/`draw` are `.get()`-based, so this is about the strip
    // staying coherent, not about safety.
    let sel = unsafe { addr_of!(SEL).read() }.min((n() - 1).max(0));
    unsafe { addr_of_mut!(SEL).write(sel) };
    let sctgt = scroll_target(sel);
    let sc = unsafe { &mut *addr_of_mut!(SCROLL) };
    sc.step(sctgt, 220.0, dt);
    crate::ui::anim::probe("chapters.scroll", sc.pos, sc.vel, sctgt, dt);
    let scl = unsafe { &mut *addr_of_mut!(SCALE) };
    scl.step(CARD_FOCUS_SCALE, 300.0, dt);
    crate::ui::anim::probe("chapters.scale", scl.pos, scl.vel, CARD_FOCUS_SCALE, dt);
}

pub(crate) fn draw() {
    if !is_open() {
        return;
    }
    let chs = chapters();
    if chs.is_empty() {
        return;
    }
    let scroll = unsafe { addr_of!(SCROLL).read() }.pos;
    let sel = unsafe { addr_of!(SEL).read() };
    let scale = unsafe { addr_of!(SCALE).read() }.pos;
    let p = pop().painter(0.0, 20.0).translate(-scroll, 0.0);

    // timecode uses SECONDARY (not the dim TERTIARY): it's drawn straight over the video, where the
    // dim grey washed out even up close. SECONDARY matches the (readable) chapter-name grey; the
    // name still leads by size (LABEL vs CAPTION) + bold.
    let dimc = theme::TEXT_SECONDARY;
    for (i, ch) in chs.iter().enumerate() {
        let x = MARGIN_X + i as f32 * (CH_W + CH_GAP);
        if !crate::ui::on_axis(x - scroll, CH_W, SCR_W, 0.0) {
            continue; // culled off-screen (the shared cull primitive)
        }
        let focused = i as c_int == sel;
        let card = Rect::new(x, CH_TOP, CH_W, CH_H);
        crate::ui::widgets::draw_card(p, card, &ch.thumb, (480, 270), 10.0, focused, scale);
        // name + timestamp beneath the card
        let ty = CH_TOP + CH_H + 26.0;
        let titc = if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
        let name = if ch.title.trim().is_empty() {
            format!("Chapter {}", ch.index)
        } else {
            ch.title.clone()
        };
        if let Ok(tc) = CString::new(crate::text::elide(&name, CH_W, theme::size::LABEL, 1, false)) {
            p.text(tc.as_ptr(), x, ty, theme::size::LABEL, titc, 0, 1);
        }
        if let Ok(sc) = CString::new(crate::ui::fmt::clock(ch.start_ms)) {
            p.text(sc.as_ptr(), x, ty + 34.0, theme::size::CAPTION, dimc, 0, 0);
        }
    }
}

