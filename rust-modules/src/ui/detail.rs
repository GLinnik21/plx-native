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
// blur), the focused hero button, and the page's vertical scroll.
static mut SELECTED: c_int = -1;
static mut FOCUS: c_int = 0; // 0=Play, 1=watchlist, 2=info
static mut SCROLL: Spring = Spring::at(0.0);

const NBTN: c_int = 3;
const PW: f32 = 168.0; // Play pill width
const CGAP: f32 = 20.0;
const CD: f32 = 60.0; // circle button diameter

/// the selected catalog row (backdrop art/blur), if any
fn selected() -> Option<&'static PmsMovie> {
    let idx = unsafe { addr_of!(SELECTED).read() };
    if idx < 0 {
        return None;
    }
    unsafe { crate::ui::home::movie_at(idx / COLS as c_int, idx % COLS as c_int).as_ref() }
}

/// the focused hero button (0=Play)
pub(crate) fn focus() -> c_int {
    unsafe { addr_of!(FOCUS).read() }
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
        addr_of_mut!(FOCUS).write(0);
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
        let f = addr_of_mut!(FOCUS);
        if sym == SDLK_LEFT && f.read() > 0 {
            f.write(f.read() - 1);
        } else if sym == SDLK_RIGHT && f.read() < NBTN - 1 {
            f.write(f.read() + 1);
        }
    }
}

pub(crate) fn update(dt: f32) {
    unsafe { (*addr_of_mut!(SCROLL)).step(0.0, K_SCROLL, dt) } // increment 1: hero only, no scroll yet
}

fn env_of(dt: f32) -> Env {
    Env { dt, screen: Rect::FULL, fr: focus(), fc: 0, sp: 1.0, hero_a: 1.0 }
}

pub(crate) fn draw() {
    let p = Painter::root();
    let env = env_of(0.0);
    let m = selected();
    draw_backdrop(p, m);
    let scroll = unsafe { (*addr_of!(SCROLL)).pos };
    draw_hero(p.translate(0.0, -scroll), &env, m);
}

fn draw_backdrop(p: Painter, m: Option<&PmsMovie>) {
    // ambient wash from the item's UltraBlur corners (catalog rows only)
    if let Some(m) = m {
        if m.has_blur != 0 {
            p.ambient(Rect::FULL, 0.55, m.blur);
        }
    }
    // backdrop art: prefer the catalog row's art, else the loaded detail's art
    let art = m
        .filter(|m| m.art[0] != 0)
        .map(|m| cfield(&m.art))
        .or_else(|| metadata::current().map(|d| d.art.clone()).filter(|s| !s.is_empty()));
    if let Some(art) = art {
        if let Ok(ap) = CString::new(art) {
            let t = resolve_tex(ap.as_ptr(), 1920, 1080, 0);
            if t != 0 {
                p.tex(t, Rect::FULL, 0.0, [1.0; 4]);
            }
        }
    }
    // bottom scrim so the lower-left content stays legible over bright art
    p.rect(
        Rect::new(0.0, SCR_H * 0.34, SCR_W, SCR_H * 0.66),
        0.0,
        [0.02, 0.02, 0.03, 0.0],
        [0.02, 0.02, 0.03, 0.95],
        0.0,
    );
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
    // focus ring on the selected control
    let fr = match focus {
        0 => Rect::new(tx, y, PW, 60.0),
        1 => Rect::new(cx1, y, CD, 60.0),
        _ => Rect::new(cx2, y, CD, 60.0),
    };
    p.ring(fr, 6.0, 30.0, 1.0);
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
