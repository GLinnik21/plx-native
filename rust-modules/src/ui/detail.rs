//! Detail screen: a full-screen item page — the reused hero (no page dots) over the
//! item backdrop, with a vertical-scroll flow underneath for the episode/related/
//! cast rows and About footer (added in later increments). Reads the loaded item
//! from crate::metadata and the selected catalog row (backdrop art + blur) from the
//! browse catalog. Mirrors the home screen's C-shaped entry points (open/update/
//! draw/move_focus) driven by app.rs.
#![allow(dead_code)]
use crate::metadata;
use crate::pms::PmsMovie;
use crate::ui::consts::*;
use crate::ui::widgets::{cfield, resolve_tex, wrap_two, CircleButton, PillButton};
use crate::ui::{Env, Painter, Rect, Spring, View}; // View: PillButton/CircleButton::draw
use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::ptr::{addr_of, addr_of_mut};

// Selected catalog row (the grid cell that opened detail — gives the backdrop art +
// blur), the two-axis focus (which section, which item in it), and the page scroll.
static mut SELECTED: c_int = -1;
static mut SECTION: c_int = 0; // 0=hero buttons, 1=season tabs, 2=episodes
static mut COL: c_int = 0; // focused item within the section
static mut SCROLL: Spring = Spring::at(0.0);

const NBTN: c_int = 3;
const PW: f32 = 168.0; // Play pill width
const CGAP: f32 = 20.0;
const CD: f32 = 60.0; // circle button diameter

// Below-the-hero layout: each section has an absolute pre-scroll Y (section_top); the
// scroll target lifts the focused section's top to TOP_MARGIN, just under the compact
// title. SCROLLED is the scroll distance past which the backdrop is fully dark.
const TOP_MARGIN: f32 = 120.0;
const SCROLLED: f32 = 890.0; // = TAB_Y - TOP_MARGIN; backdrop-dim saturation reference
const TAB_Y: f32 = 1010.0;
const EP_Y: f32 = 1075.0;
const EP_W: f32 = 420.0;
const EP_H: f32 = 236.0; // 16:9-ish still
const EP_GAP: f32 = 28.0;
// Related row (portrait posters) + Cast & Crew row (circular headshots)
const RELATED_Y: f32 = 1760.0;
const REL_W: f32 = 200.0;
const REL_H: f32 = 300.0;
const REL_GAP: f32 = 28.0;
const CAST_Y: f32 = 2300.0;
const CAST_D: f32 = 150.0; // headshot diameter
const CAST_SLOT: f32 = 200.0; // per-member horizontal pitch (room for the name)
const ABOUT_Y: f32 = 2820.0; // About footer (heading + card + 3 info columns); gap clears the cast row

/// the selected catalog row (backdrop art/blur), if any
fn selected() -> Option<&'static PmsMovie> {
    let idx = unsafe { addr_of!(SELECTED).read() };
    if idx < 0 {
        return None;
    }
    unsafe { crate::ui::home::movie_at(idx / COLS as c_int, idx % COLS as c_int).as_ref() }
}

/// the focused hero button (0=Play), or -1 when the hero section isn't focused
pub(crate) fn focus() -> c_int {
    unsafe {
        if addr_of!(SECTION).read() == 0 {
            addr_of!(COL).read()
        } else {
            -1
        }
    }
}

/// available sections for the loaded item (hero always; tabs/episodes only for shows;
/// related/cast for both when present). Section ids: 0 hero, 1 tabs, 2 episodes,
/// 3 related, 4 cast.
fn sections() -> Vec<c_int> {
    let mut v = vec![0];
    if let Some(d) = metadata::current() {
        if d.is_show && !d.seasons.is_empty() {
            v.push(1);
        }
        if d.is_show && !d.episodes.is_empty() {
            v.push(2);
        }
        if !d.related.is_empty() {
            v.push(3);
        }
        if !d.cast.is_empty() {
            v.push(4);
        }
        v.push(5); // About footer — always present when an item is loaded
    }
    v
}
fn n_items(section: c_int) -> c_int {
    match section {
        0 => NBTN,
        1 => metadata::current().map(|d| d.seasons.len()).unwrap_or(0) as c_int,
        2 => metadata::current().map(|d| d.episodes.len()).unwrap_or(0) as c_int,
        3 => metadata::current().map(|d| d.related.len()).unwrap_or(0) as c_int,
        4 => metadata::current().map(|d| d.cast.len()).unwrap_or(0) as c_int,
        5 => 1, // About footer (a single non-scrolling block)
        _ => 0,
    }
}
/// pre-scroll top Y of a section (drives the scroll target)
fn section_top(section: c_int) -> f32 {
    match section {
        1 | 2 => TAB_Y, // tabs + episodes share one scrolled block
        3 => RELATED_Y,
        4 => CAST_Y,
        5 => ABOUT_Y,
        _ => TOP_MARGIN, // hero -> scroll target 0
    }
}
/// scroll offset that lifts the focused section's top to TOP_MARGIN
fn scroll_target() -> f32 {
    let sec = unsafe { addr_of!(SECTION).read() };
    if sec == 0 {
        0.0
    } else {
        (section_top(sec) - TOP_MARGIN).max(0.0)
    }
}
/// the selected catalog row pointer (for the app to play a movie), or null
pub(crate) fn selected_ptr() -> *mut PmsMovie {
    let idx = unsafe { addr_of!(SELECTED).read() };
    if idx < 0 {
        return std::ptr::null_mut();
    }
    crate::ui::home::movie_at(idx / COLS as c_int, idx % COLS as c_int)
}
/// is the loaded item a TV show?
pub(crate) fn is_show() -> bool {
    metadata::current().map(|d| d.is_show).unwrap_or(false)
}

/// Open the detail page for catalog row `idx`: load its full detail (blocking) and
/// reset focus/scroll.
pub(crate) fn open(idx: c_int) {
    unsafe {
        addr_of_mut!(SELECTED).write(idx);
        addr_of_mut!(SECTION).write(0);
        addr_of_mut!(COL).write(0);
        (*addr_of_mut!(SCROLL)).jump(0.0);
    }
    if let Some(m) = unsafe { crate::ui::home::movie_at(idx / COLS as c_int, idx % COLS as c_int).as_ref() } {
        let rk = cfield(&m.rk);
        if !rk.is_empty() {
            metadata::load_detail(&rk);
        }
    }
}

/// Leave the detail page (drop the loaded item).
pub(crate) fn close() {
    metadata::clear();
    unsafe { addr_of_mut!(SELECTED).write(-1) }
}

pub(crate) fn move_focus(sym: c_int) {
    let sym = sym as u32;
    unsafe {
        let sec = addr_of!(SECTION).read();
        let col = addr_of!(COL).read();
        if sym == SDLK_LEFT || sym == SDLK_RIGHT {
            let n = n_items(sec);
            if n <= 0 {
                return;
            }
            let nc = if sym == SDLK_LEFT { (col - 1).max(0) } else { (col + 1).min(n - 1) };
            if nc != col {
                addr_of_mut!(COL).write(nc);
                // focusing a season tab switches to that season (brief blocking fetch)
                if sec == 1 {
                    metadata::load_season(nc as usize);
                }
            }
        } else if sym == SDLK_UP || sym == SDLK_DOWN {
            let avail = sections();
            let pos = avail.iter().position(|&s| s == sec).unwrap_or(0);
            let np = if sym == SDLK_UP { pos.saturating_sub(1) } else { (pos + 1).min(avail.len().saturating_sub(1)) };
            let ns = avail[np];
            if ns != sec {
                addr_of_mut!(SECTION).write(ns);
                // land on the active season when entering the tabs; else the first item
                let start = if ns == 1 {
                    metadata::current().map(|d| d.cur_season as c_int).unwrap_or(0)
                } else {
                    0
                };
                addr_of_mut!(COL).write(start);
            }
        }
    }
}

pub(crate) fn update(dt: f32) {
    unsafe { (*addr_of_mut!(SCROLL)).step(scroll_target(), K_SCROLL, dt) }
}

fn env_of(dt: f32) -> Env {
    Env { dt, screen: Rect::FULL, fr: focus(), fc: 0, sp: 1.0, hero_a: 1.0 }
}

pub(crate) fn draw() {
    let p = Painter::root();
    let env = env_of(0.0);
    let m = selected();
    let scroll = unsafe { (*addr_of!(SCROLL)).pos };
    draw_backdrop(p, m, scroll);
    let hero_a = (1.0 - scroll / 400.0).clamp(0.0, 1.0);
    let ps = p.translate(0.0, -scroll);
    // hero fades out as the page scrolls down into the rows
    if hero_a > 0.01 {
        draw_hero(ps.alpha(hero_a), &env, m);
    }
    // compact centered title fades in at the top of the scrolled view
    if hero_a < 0.99 {
        draw_compact_title(p.alpha(1.0 - hero_a), m);
    }
    // season tabs + episode row (shows only), then related + cast (both), scrolled
    if is_show() {
        draw_tabs(ps);
        draw_episodes(ps);
    }
    draw_related(ps);
    draw_cast(ps);
    draw_about(ps);
}

fn draw_backdrop(p: Painter, m: Option<&PmsMovie>, scroll: f32) {
    // 0 at the hero, 1 when scrolled down into the rows
    let sf = (scroll / SCROLLED).clamp(0.0, 1.0);
    // ambient wash from the item's UltraBlur corners — kept as the dark warm glow when scrolled
    if let Some(m) = m {
        if m.has_blur != 0 {
            p.ambient(Rect::FULL, 0.55, m.blur);
        }
    }
    // backdrop art: prefer the catalog row's art, else the loaded detail's art. Fades
    // out as the page scrolls into the rows so the episode/row text reads over a dark bg.
    let art_a = 1.0 - sf;
    if art_a > 0.01 {
        let art = m
            .filter(|m| m.art[0] != 0)
            .map(|m| cfield(&m.art))
            .or_else(|| metadata::current().map(|d| d.art.clone()).filter(|s| !s.is_empty()));
        if let Some(art) = art {
            if let Ok(ap) = CString::new(art) {
                let t = resolve_tex(ap.as_ptr(), 1920, 1080, 0);
                if t != 0 {
                    p.tex(t, Rect::FULL, 0.0, [1.0, 1.0, 1.0, art_a]);
                }
            }
        }
    }
    // bottom scrim for the hero's lower-left content (only while the hero is visible)
    if sf < 0.99 {
        p.rect(
            Rect::new(0.0, SCR_H * 0.34, SCR_W, SCR_H * 0.66),
            0.0,
            [0.02, 0.02, 0.03, 0.0],
            [0.02, 0.02, 0.03, 0.95 * (1.0 - sf)],
            0.0,
        );
    }
    // overall dim as the page scrolls into the rows (legibility for the row text)
    let dk = sf * 0.55;
    if dk > 0.001 {
        p.rect(Rect::FULL, 0.0, [0.02, 0.02, 0.03, dk], [0.02, 0.02, 0.03, dk], 0.0);
    }
}

fn draw_hero(p: Painter, env: &Env, m: Option<&PmsMovie>) {
    let tx = MARGIN_X;
    let w_a = [0.97, 0.98, 0.99, 1.0];
    let d_a = [0.74, 0.77, 0.82, 1.0];
    let dim = [0.60, 0.62, 0.66, 1.0];
    let d = metadata::current();

    // ---- title: clearLogo (transparent PNG) if loaded, else bold text ----
    let title_bottom = 566.0f32;
    let rk = d.map(|d| d.rk.clone()).or_else(|| m.map(|m| cfield(&m.rk))).unwrap_or_default();
    let title = d.map(|d| d.title.clone()).or_else(|| m.map(|m| cfield(&m.title))).unwrap_or_default();
    let mut drew_logo = false;
    if !rk.is_empty() {
        if let Ok(lpath) = CString::new(format!("/library/metadata/{rk}/clearLogo")) {
            let mut lk = [0u8; 352];
            crate::posters::poster_key(lk.as_mut_ptr() as *mut c_char, lk.len(), lpath.as_ptr(), 600, 240, 1);
            let lt = crate::posters::poster_get(lk.as_ptr() as *const c_char);
            let (mut lw, mut lh) = (0i32, 0i32);
            crate::posters::poster_wh(lk.as_ptr() as *const c_char, &mut lw, &mut lh);
            if lt != 0 && lh > 0 {
                let mut hh = 120.0f32;
                let mut ww = hh * lw as f32 / lh as f32;
                if ww > 680.0 {
                    ww = 680.0;
                    hh = ww * lh as f32 / lw as f32;
                }
                p.tex(lt, Rect::new(tx, title_bottom - hh, ww, hh), 0.0, w_a);
                drew_logo = true;
            }
        }
    }
    if !drew_logo {
        if let Ok(t) = CString::new(title.clone()) {
            p.text(t.as_ptr(), tx, title_bottom - 68.0, 72, w_a, 0, 1);
        }
    }

    // ---- meta line: "TV Show · Sci-Fi · Adventure · 18+" ----
    let mut parts: Vec<String> = Vec::new();
    if let Some(d) = d {
        parts.push(if d.is_show { "TV Show".into() } else { "Movie".into() });
        for g in d.genres.iter().take(2) {
            parts.push(g.clone());
        }
        if !d.rating.is_empty() {
            parts.push(d.rating.clone());
        }
    }
    let meta_y = title_bottom + 36.0;
    if let Ok(mc) = CString::new(parts.join("   \u{b7}   ")) {
        p.text(mc.as_ptr(), tx, meta_y, 26, d_a, 0, 0);
    }

    // ---- synopsis (two lines) ----
    let summary = d.map(|d| d.summary.clone()).or_else(|| m.map(|m| cfield(&m.summary))).unwrap_or_default();
    let syn_y = meta_y + 46.0;
    if !summary.is_empty() {
        let (l1, l2) = wrap_two(&summary);
        if let Ok(c1) = CString::new(l1) {
            p.text(c1.as_ptr(), tx, syn_y, 24, d_a, 0, 0);
        }
        if !l2.is_empty() {
            if let Ok(c2) = CString::new(l2) {
                p.text(c2.as_ptr(), tx, syn_y + 30.0, 24, d_a, 0, 0);
            }
        }
    }

    // ---- date · runtime ----
    let date_y = syn_y + 82.0;
    if let Some(d) = d {
        let mut info = pretty_date(&d.aired, d.year);
        let mins = d.dur_ms / 60_000;
        if mins > 0 {
            if !info.is_empty() {
                info.push_str("    \u{b7}    ");
            }
            let (h, mm) = (mins / 60, mins % 60);
            info.push_str(&if h > 0 { format!("{h} hr {mm} min") } else { format!("{mm} min") });
        }
        if let Ok(ic) = CString::new(info) {
            p.text(ic.as_ptr(), tx, date_y, 23, dim, 0, 0);
        }
    }

    // ---- buttons ----
    let btn_y = date_y + 46.0;
    draw_buttons(p, env, btn_y);

    // ---- "Starring …" right-aligned near the bottom-right ----
    if let Some(d) = d {
        if !d.cast.is_empty() {
            let names: Vec<String> = d.cast.iter().take(3).map(|c| c.tag.clone()).collect();
            if let Ok(sc) = CString::new(format!("Starring {}", names.join(", "))) {
                // measure (invisible) then draw right-aligned against the right margin
                let w = p.alpha(0.0).text(sc.as_ptr(), 0.0, -200.0, 24, d_a, 0, 1);
                p.text(sc.as_ptr(), SCR_W - MARGIN_X - w, btn_y + 16.0, 24, d_a, 0, 1);
            }
        }
    }
}

fn draw_buttons(p: Painter, env: &Env, y: f32) {
    let tx = MARGIN_X;
    let focus = focus();
    let cx1 = tx + PW + CGAP;
    let cx2 = cx1 + CD + CGAP;
    PillButton::play(c"Play".as_ptr()).at(tx, y).draw(env, p);
    CircleButton::new(c"+".as_ptr()).at(cx1, y).draw(env, p);
    CircleButton::new(c"i".as_ptr()).at(cx2, y).draw(env, p);
    // focus ring on the selected control (only while the hero section is focused)
    let fr = match focus {
        0 => Rect::new(tx, y, PW, 60.0),
        1 => Rect::new(cx1, y, CD, 60.0),
        2 => Rect::new(cx2, y, CD, 60.0),
        _ => return,
    };
    p.ring(fr, 6.0, 30.0, 1.0);
}

/// small centered clearLogo/title shown at the top once the page is scrolled
fn draw_compact_title(p: Painter, m: Option<&PmsMovie>) {
    let d = metadata::current();
    let rk = d.map(|d| d.rk.clone()).or_else(|| m.map(|m| cfield(&m.rk))).unwrap_or_default();
    let title = d.map(|d| d.title.clone()).or_else(|| m.map(|m| cfield(&m.title))).unwrap_or_default();
    let cx = SCR_W * 0.5;
    if !rk.is_empty() {
        if let Ok(lpath) = CString::new(format!("/library/metadata/{rk}/clearLogo")) {
            let mut lk = [0u8; 352];
            crate::posters::poster_key(lk.as_mut_ptr() as *mut c_char, lk.len(), lpath.as_ptr(), 600, 240, 1);
            let lt = crate::posters::poster_get(lk.as_ptr() as *const c_char);
            let (mut lw, mut lh) = (0i32, 0i32);
            crate::posters::poster_wh(lk.as_ptr() as *const c_char, &mut lw, &mut lh);
            if lt != 0 && lh > 0 {
                let hh = 54.0f32;
                let ww = hh * lw as f32 / lh as f32;
                p.tex(lt, Rect::new(cx - ww * 0.5, 40.0, ww, hh), 0.0, [0.97, 0.98, 0.99, 1.0]);
                return;
            }
        }
    }
    if let Ok(t) = CString::new(title) {
        p.text(t.as_ptr(), cx, 54.0, 40, [0.97, 0.98, 0.99, 1.0], 1, 1);
    }
}

/// season tab row: active season bright/bold, focused tab gets a highlight pill
fn draw_tabs(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    if d.seasons.is_empty() {
        return;
    }
    let sec = unsafe { addr_of!(SECTION).read() };
    let col = unsafe { addr_of!(COL).read() };
    let mut x = MARGIN_X;
    for (i, s) in d.seasons.iter().enumerate() {
        let label = if s.title.is_empty() { format!("Season {}", s.index) } else { s.title.clone() };
        let lc = match CString::new(label) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let active = i == d.cur_season;
        let focused = sec == 1 && col == i as c_int;
        let bold = if active { 1 } else { 0 };
        let txt = if active { [0.98, 0.99, 1.0, 1.0] } else { [0.58, 0.60, 0.64, 1.0] };
        let w = p.alpha(0.0).text(lc.as_ptr(), 0.0, -200.0, 30, txt, 0, bold);
        if focused {
            p.rrect(Rect::new(x - 18.0, TAB_Y - 8.0, w + 36.0, 50.0), 25.0, 25.0, [1.0, 1.0, 1.0, 0.14]);
        }
        p.text(lc.as_ptr(), x, TAB_Y, 30, txt, 0, bold);
        x += w + 52.0;
    }
}

/// horizontal row of landscape episode cards with under-card metadata
fn draw_episodes(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    if d.episodes.is_empty() {
        return;
    }
    let sec = unsafe { addr_of!(SECTION).read() };
    let col = unsafe { addr_of!(COL).read() };
    let focus_col = if sec == 2 { col } else { -1 };
    // keep the focused card on-screen (scroll so it sits in the 2nd slot)
    let sx = if focus_col > 1 { (focus_col as f32 - 1.0) * (EP_W + EP_GAP) } else { 0.0 };
    let pe = p.translate(-sx, 0.0);
    let dimc = [0.58, 0.60, 0.64, 1.0];
    for (i, ep) in d.episodes.iter().enumerate() {
        let x = MARGIN_X + i as f32 * (EP_W + EP_GAP);
        if x - sx > SCR_W || x - sx + EP_W < 0.0 {
            continue; // off-screen
        }
        let focused = i as c_int == focus_col;
        let card = Rect::new(x, EP_Y, EP_W, EP_H);
        // episode still (else a dark placeholder)
        let mut drew = false;
        if !ep.thumb.is_empty() {
            if let Ok(tp) = CString::new(ep.thumb.clone()) {
                let t = resolve_tex(tp.as_ptr(), 640, 360, 0);
                if t != 0 {
                    pe.tex(t, card, 12.0, [1.0; 4]);
                    drew = true;
                }
            }
        }
        if !drew {
            pe.rrect(card, 12.0, 12.0, [0.12, 0.13, 0.16, 1.0]);
        }
        // resume bar
        if ep.resume_ms > 0 && ep.dur_ms > 0 {
            let frac = (ep.resume_ms as f32 / ep.dur_ms as f32).clamp(0.0, 1.0);
            let bar = Rect::new(x + 12.0, EP_Y + EP_H - 16.0, EP_W - 24.0, 5.0);
            pe.rrect(bar, 2.5, 2.5, [1.0, 1.0, 1.0, 0.28]);
            pe.rrect(Rect::new(bar.x, bar.y, bar.w * frac, bar.h), 2.5, 2.5, [1.0, 1.0, 1.0, 0.95]);
        }
        if focused {
            pe.ring(card, 6.0, 14.0, 1.0);
        }
        // under-card metadata
        let ty = EP_Y + EP_H + 30.0;
        let titc = if focused { [0.98, 0.99, 1.0, 1.0] } else { [0.80, 0.82, 0.86, 1.0] };
        if let Ok(ec) = CString::new(format!("EPISODE {}", ep.index)) {
            pe.text(ec.as_ptr(), x, ty, 18, dimc, 0, 1);
        }
        if let Ok(tc) = CString::new(ep.title.clone()) {
            pe.text(tc.as_ptr(), x, ty + 26.0, 24, titc, 0, 1);
        }
        if !ep.summary.is_empty() {
            let (l1, l2) = wrap_ep(&ep.summary);
            if let Ok(c1) = CString::new(l1) {
                pe.text(c1.as_ptr(), x, ty + 62.0, 20, dimc, 0, 0);
            }
            if !l2.is_empty() {
                if let Ok(c2) = CString::new(l2) {
                    pe.text(c2.as_ptr(), x, ty + 88.0, 20, dimc, 0, 0);
                }
            }
        }
        let date = pretty_date(&ep.aired, 0);
        if !date.is_empty() {
            if let Ok(dc) = CString::new(date) {
                pe.text(dc.as_ptr(), x, ty + 124.0, 19, dimc, 0, 0);
            }
        }
    }
}

/// "Related" — a horizontal row of portrait poster cards from the related hub
fn draw_related(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    if d.related.is_empty() {
        return;
    }
    p.text(c"Related".as_ptr(), MARGIN_X, RELATED_Y, 28, [0.90, 0.92, 0.95, 1.0], 0, 1);
    let sec = unsafe { addr_of!(SECTION).read() };
    let col = unsafe { addr_of!(COL).read() };
    let focus_col = if sec == 3 { col } else { -1 };
    let row_y = RELATED_Y + 46.0;
    let sx = if focus_col > 1 { (focus_col as f32 - 1.0) * (REL_W + REL_GAP) } else { 0.0 };
    let pr = p.translate(-sx, 0.0);
    for (i, r) in d.related.iter().enumerate() {
        let x = MARGIN_X + i as f32 * (REL_W + REL_GAP);
        if x - sx > SCR_W || x - sx + REL_W < 0.0 {
            continue;
        }
        let focused = i as c_int == focus_col;
        let card = Rect::new(x, row_y, REL_W, REL_H);
        let cr = if focused { card.scaled(1.05) } else { card };
        let mut drew = false;
        if !r.thumb.is_empty() {
            if let Ok(tp) = CString::new(r.thumb.clone()) {
                let t = resolve_tex(tp.as_ptr(), 250, 375, 0);
                if t != 0 {
                    pr.tex(t, cr, 10.0, [1.0; 4]);
                    drew = true;
                }
            }
        }
        if !drew {
            pr.rrect(cr, 10.0, 10.0, [0.12, 0.13, 0.16, 1.0]);
        }
        if focused {
            pr.ring(cr, 6.0, 12.0, 1.0);
            if let Ok(tc) = CString::new(r.title.clone()) {
                pr.text(tc.as_ptr(), x, row_y + REL_H + 30.0, 20, [0.85, 0.87, 0.90, 1.0], 0, 0);
            }
        }
    }
}

/// "Cast & Crew" — a horizontal row of circular headshots with names
fn draw_cast(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    if d.cast.is_empty() {
        return;
    }
    p.text(c"Cast & Crew".as_ptr(), MARGIN_X, CAST_Y, 28, [0.90, 0.92, 0.95, 1.0], 0, 1);
    let sec = unsafe { addr_of!(SECTION).read() };
    let col = unsafe { addr_of!(COL).read() };
    let focus_col = if sec == 4 { col } else { -1 };
    let row_y = CAST_Y + 60.0;
    let sx = if focus_col > 1 { (focus_col as f32 - 1.0) * CAST_SLOT } else { 0.0 };
    let pc = p.translate(-sx, 0.0);
    for (i, c) in d.cast.iter().enumerate() {
        let cxc = MARGIN_X + CAST_D * 0.5 + i as f32 * CAST_SLOT; // circle center x
        if cxc - sx > SCR_W + CAST_D || cxc - sx + CAST_D < 0.0 {
            continue;
        }
        let focused = i as c_int == focus_col;
        let dp = if focused { CAST_D * 1.06 } else { CAST_D };
        let circ = Rect::new(cxc - dp * 0.5, row_y + (CAST_D - dp) * 0.5, dp, dp);
        // headshot (external metadata-static URL → PMS photo transcoder), circular
        let mut drew = false;
        if !c.thumb.is_empty() {
            if let Ok(tp) = CString::new(c.thumb.clone()) {
                let t = resolve_tex(tp.as_ptr(), 300, 300, 0);
                if t != 0 {
                    pc.tex(t, circ, dp * 0.5, [1.0; 4]);
                    drew = true;
                }
            }
        }
        if !drew {
            pc.rect(circ, dp * 0.5, [0.16, 0.17, 0.20, 1.0], [0.10, 0.11, 0.13, 1.0], 0.0);
        }
        if focused {
            let fc = Rect::new(cxc - CAST_D * 0.5, row_y, CAST_D, CAST_D);
            pc.ring(fc, 6.0, CAST_D * 0.5, 1.0);
        }
        let name_c = if focused { [0.98, 0.99, 1.0, 1.0] } else { [0.80, 0.82, 0.86, 1.0] };
        if let Ok(nc) = CString::new(c.tag.clone()) {
            pc.text(nc.as_ptr(), cxc, row_y + CAST_D + 22.0, 21, name_c, 1, if focused { 1 } else { 0 });
        }
        if !c.role.is_empty() {
            if let Ok(rc) = CString::new(c.role.clone()) {
                pc.text(rc.as_ptr(), cxc, row_y + CAST_D + 48.0, 17, [0.56, 0.58, 0.62, 1.0], 1, 0);
            }
        }
    }
}

/// two-line wrap tuned to the narrower episode-card width
fn wrap_ep(s: &str) -> (String, String) {
    let b = s.as_bytes();
    let n = b.len();
    if n <= 42 {
        return (s.to_string(), String::new());
    }
    let mut brk = 42;
    while brk > 20 && b[brk] != b' ' {
        brk -= 1;
    }
    let l1 = String::from_utf8_lossy(&b[..brk]).into_owned();
    let rest = &b[brk + 1..];
    let m = rest.len();
    let mut c2 = m.min(44);
    if m > 44 {
        while c2 > 20 && rest[c2] != b' ' {
            c2 -= 1;
        }
    }
    let mut l2 = String::from_utf8_lossy(&rest[..c2]).into_owned();
    if c2 < m {
        l2.push('\u{2026}');
    }
    (l1, l2)
}

/// OK/SELECT on the detail page: returns true if playback should start (the route
/// URL/HUD have already been set). Section 0 = hero Play, 1 = season tab, 2 = episode.
pub(crate) fn on_ok() -> bool {
    let sec = unsafe { addr_of!(SECTION).read() };
    let col = unsafe { addr_of!(COL).read() };
    match sec {
        0 => {
            if col != 0 {
                return false; // only Play acts (watchlist/info are placeholders)
            }
            if is_show() {
                play_episode_at(0)
            } else {
                let m = selected_ptr();
                if m.is_null() {
                    return false;
                }
                crate::route::play_movie(m);
                true
            }
        }
        1 => {
            metadata::load_season(col.max(0) as usize);
            false
        }
        2 => play_episode_at(col),
        3 => {
            // Related: open that item's detail page in place
            let rk = metadata::current().and_then(|d| d.related.get(col.max(0) as usize)).map(|r| r.rk.clone());
            if let Some(rk) = rk {
                open_rk(&rk);
            }
            false
        }
        _ => false, // cast (4): headshots are not actionable
    }
}

/// Re-open the detail page for an arbitrary ratingKey (e.g. a Related item). Uses the
/// catalog row for the backdrop art/blur when the item is in the browse catalog, else
/// falls back to the loaded detail's own art (no blur).
pub(crate) fn open_rk(rk: &str) {
    let idx = crate::pms::index_of_rk(rk);
    unsafe {
        addr_of_mut!(SELECTED).write(idx);
        addr_of_mut!(SECTION).write(0);
        addr_of_mut!(COL).write(0);
        (*addr_of_mut!(SCROLL)).jump(0.0);
    }
    metadata::load_detail(rk);
}

fn play_episode_at(i: c_int) -> bool {
    let d = match metadata::current() {
        Some(d) => d,
        None => return false,
    };
    let ep = match d.episodes.get(i.max(0) as usize) {
        Some(e) => e,
        None => return false,
    };
    let show = d.title.clone();
    let hud_title = if ep.title.is_empty() { show.clone() } else { ep.title.clone() };
    let hud_ctx = format!("{}  \u{b7}  S{} E{}", show, ep.season, ep.index);
    crate::route::play_episode(&ep.rk, &ep.part, &ep.vcodec, &ep.acodec, &hud_title, &hud_ctx);
    true
}

// ---- About footer (section 5): heading + card + Information/Languages/Accessibility ----

/// wrap `s` into up to `max_lines` lines of ~`budget` chars (word boundaries); the
/// last line gets an ellipsis if the text was truncated.
fn wrap_lines(s: &str, budget: usize, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for w in s.split_whitespace() {
        if !cur.is_empty() && cur.len() + 1 + w.len() > budget {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            while last.len() > budget.saturating_sub(1) {
                last.pop();
            }
            last.push('\u{2026}');
        }
    }
    lines
}

fn text_at(p: Painter, x: f32, y: f32, sz: c_int, col: [f32; 4], bold: c_int, s: &str) -> f32 {
    match CString::new(s) {
        Ok(t) => p.text(t.as_ptr(), x, y, sz, col, 0, bold),
        Err(_) => 0.0,
    }
}

/// a dim label over one/two white value lines; returns the vertical advance
fn draw_pair(p: Painter, x: f32, y: f32, label: &str, value: &str, lbl: [f32; 4], val: [f32; 4]) -> f32 {
    text_at(p, x, y, 20, lbl, 0, label);
    let wrapped = wrap_lines(value, 40, 2);
    for (i, ln) in wrapped.iter().enumerate() {
        text_at(p, x, y + 30.0 + i as f32 * 26.0, 24, val, 1, ln);
    }
    30.0 + wrapped.len().max(1) as f32 * 26.0 + 22.0
}

/// a small rounded accessibility badge (CC / SDH / AD)
fn draw_badge(p: Painter, x: f32, y: f32, label: &str) {
    let (w, h) = (48.0f32, 30.0f32);
    p.rrect(Rect::new(x, y, w, h), 7.0, 7.0, [0.86, 0.88, 0.92, 0.20]);
    if let Ok(t) = CString::new(label) {
        p.text(t.as_ptr(), x + w * 0.5, y + (h - 20.0) * 0.5 - 1.0, 20, [0.9, 0.92, 0.96, 1.0], 1, 1);
    }
}

fn draw_about(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    let tx = MARGIN_X;
    let hd = [0.95, 0.96, 0.98, 1.0]; // headings
    let val = [0.90, 0.92, 0.95, 1.0]; // values
    let lbl = [0.55, 0.57, 0.62, 1.0]; // dim labels
    let dim = [0.66, 0.68, 0.72, 1.0];

    text_at(p, tx, ABOUT_Y, 30, hd, 1, "About");

    // ---- card: title, genres, summary + MORE ----
    let (cw, ch, cy, pad) = (640.0f32, 330.0f32, ABOUT_Y + 50.0, 30.0f32);
    p.rrect(Rect::new(tx, cy, cw, ch), 18.0, 18.0, [1.0, 1.0, 1.0, 0.07]);
    let ix = tx + pad;
    text_at(p, ix, cy + pad, 30, hd, 1, &d.title);
    if !d.genres.is_empty() {
        text_at(p, ix, cy + pad + 44.0, 22, dim, 0, &d.genres.join(", "));
    }
    let sy = cy + pad + 100.0;
    let lines = wrap_lines(&d.summary, 52, 5);
    for (i, ln) in lines.iter().enumerate() {
        let w = text_at(p, ix, sy + i as f32 * 30.0, 22, val, 0, ln);
        if i + 1 == lines.len() {
            text_at(p, ix + w + 8.0, sy + i as f32 * 30.0, 22, hd, 1, "MORE");
        }
    }

    // ---- three columns ----
    let col_y = ABOUT_Y + 430.0;

    // Information
    text_at(p, tx, col_y, 30, hd, 1, "Information");
    let mut yy = col_y + 68.0;
    let released = pretty_date(&d.aired, d.year);
    if !released.is_empty() {
        yy += draw_pair(p, tx, yy, "Released", &released, lbl, val);
    }
    let dur = if d.dur_ms > 0 { d.dur_ms } else { d.episodes.first().map(|e| e.dur_ms).unwrap_or(0) };
    if dur > 0 {
        let mins = dur / 60_000;
        yy += draw_pair(p, tx, yy, "Run Time", &format!("{} hr {} min", mins / 60, mins % 60), lbl, val);
    }
    yy += draw_pair(p, tx, yy, "Rated", if d.rating.is_empty() { "NR" } else { &d.rating }, lbl, val);
    if !d.countries.is_empty() {
        draw_pair(p, tx, yy, "Regions of Origin", &d.countries.join(", "), lbl, val);
    }

    // Languages
    let lx = 760.0f32;
    text_at(p, lx, col_y, 30, hd, 1, "Languages");
    let mut ly = col_y + 68.0;
    if let Some(a0) = d.audio.first() {
        let orig = if a0.lang.is_empty() { "Unknown".to_string() } else { a0.lang.clone() };
        ly += draw_pair(p, lx, ly, "Original Audio", &orig, lbl, val);
    }
    if !d.audio.is_empty() {
        text_at(p, lx, ly, 20, lbl, 0, "Audio");
        let list: Vec<String> = d
            .audio
            .iter()
            .take(8)
            .map(|a| {
                let lang = if a.lang.is_empty() { "Unknown".to_string() } else { a.lang.clone() };
                format!("{} ({})", lang, a.codec.to_uppercase())
            })
            .collect();
        for (i, ln) in wrap_lines(&list.join(", "), 44, 6).iter().enumerate() {
            text_at(p, lx, ly + 30.0 + i as f32 * 28.0, 22, val, 0, ln);
        }
    }

    // Accessibility
    let ax = 1360.0f32;
    text_at(p, ax, col_y, 30, hd, 1, "Accessibility");
    let cc = !d.subs.is_empty();
    let sdh = d.subs.iter().any(|s| s.sdh);
    let ad = d.audio.iter().any(|a| a.ad);
    let items: [(bool, &str, &str); 3] = [
        (cc, "CC", "Closed captions refer to subtitles in available languages with the addition of relevant non-dialogue information."),
        (sdh, "SDH", "Subtitles for the deaf and hard of hearing (SDH) refer to subtitles in the original language with the addition of relevant non-dialogue information."),
        (ad, "AD", "Audio descriptions (AD) refer to a narration track describing what is happening on screen, to provide context for those who are blind or have low vision."),
    ];
    let mut ay = col_y + 64.0;
    let mut any = false;
    for (present, label, desc) in items {
        if !present {
            continue;
        }
        any = true;
        draw_badge(p, ax, ay, label);
        let wrapped = wrap_lines(desc, 40, 4);
        for (i, ln) in wrapped.iter().enumerate() {
            text_at(p, ax, ay + 46.0 + i as f32 * 26.0, 20, val, 0, ln);
        }
        ay += 46.0 + wrapped.len() as f32 * 26.0 + 26.0;
    }
    if !any {
        text_at(p, ax, col_y + 68.0, 22, dim, 0, "\u{2014}");
    }
}

/// "YYYY-MM-DD" -> "D Mon YYYY"; falls back to the year, then empty
fn pretty_date(iso: &str, year: i64) -> String {
    let parts: Vec<&str> = iso.split('-').collect();
    if parts.len() == 3 {
        if let (Ok(y), Ok(mo), Ok(da)) =
            (parts[0].parse::<i64>(), parts[1].parse::<usize>(), parts[2].parse::<i64>())
        {
            const MON: [&str; 12] =
                ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            if (1..=12).contains(&mo) {
                return format!("{da} {} {y}", MON[mo - 1]);
            }
        }
    }
    if year > 0 {
        year.to_string()
    } else {
        String::new()
    }
}
