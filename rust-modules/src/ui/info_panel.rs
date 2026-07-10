//! In-player Info card (mockup "Info mode"): a horizontal card over the transport with the
//! episode/movie still, title + synopsis, a metadata line with outlined capability badges, and a
//! column of action buttons. Opened from the HUD's "Info" tab; app.rs routes D-pad/OK/BACK here
//! while it's open and hides the normal transport middle behind it. Data from crate::metadata.
#![allow(dead_code)]
use crate::metadata;
use crate::ui::consts::{SDLK_DOWN, SDLK_UP};
use crate::ui::icons::Icon;
use crate::ui::widgets::resolve_tex;
use crate::ui::{Painter, Rect, Spring, View};
use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

const SCR_W: f32 = 1920.0;
const SCR_H: f32 = 1080.0;

pub enum InfoAction {
    None,
    FromBeginning,
    GoToMovie,
}

static mut OPEN: bool = false;
static mut FOCUS: c_int = 0; // index into the action-button column
static mut APPEAR: Spring = Spring::at(0.0);

pub(crate) fn is_open() -> bool {
    unsafe { addr_of!(OPEN).read() }
}
pub(crate) fn open() {
    unsafe {
        addr_of_mut!(FOCUS).write(0);
        addr_of_mut!(APPEAR).write(Spring::at(0.0));
        addr_of_mut!(OPEN).write(true);
    }
}
pub(crate) fn close() {
    unsafe { addr_of_mut!(OPEN).write(false) }
}
pub(crate) fn reset() {
    close();
}

/// the action-button labels for the current item
fn actions() -> Vec<&'static str> {
    vec!["From Beginning", "Go to Movie"]
}

pub(crate) fn move_focus(sym: c_int) {
    let n = actions().len() as c_int;
    let sym = sym as u32;
    let f = unsafe { addr_of!(FOCUS).read() };
    let nf = if sym == SDLK_UP {
        (f - 1).max(0)
    } else if sym == SDLK_DOWN {
        (f + 1).min(n - 1)
    } else {
        f
    };
    unsafe { addr_of_mut!(FOCUS).write(nf) }
}

/// activate the focused action, then close
pub(crate) fn on_ok() -> InfoAction {
    let f = unsafe { addr_of!(FOCUS).read() };
    close();
    match actions().get(f.max(0) as usize).copied() {
        Some("From Beginning") => InfoAction::FromBeginning,
        Some("Go to Movie") => InfoAction::GoToMovie,
        _ => InfoAction::None,
    }
}

pub(crate) fn update(dt: f32) {
    if !is_open() {
        return;
    }
    unsafe { &mut *addr_of_mut!(APPEAR) }.step(1.0, 300.0, dt);
}

// ---- helpers ----
fn fmt_dur(ms: i64) -> String {
    let mins = (ms / 60_000).max(0);
    let (h, m) = (mins / 60, mins % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// a friendly one-word audio tag for the badge row ("Dolby TrueHD", "Dolby Digital+", …)
fn audio_badge(codec: &str) -> Option<&'static str> {
    match codec.to_ascii_lowercase().as_str() {
        "truehd" => Some("Dolby TrueHD"),
        "eac3" | "ec-3" => Some("Dolby Digital+"),
        "ac3" => Some("Dolby Digital"),
        "dts" | "dca" => Some("DTS"),
        _ => None,
    }
}

/// an outlined metadata chip at left edge `x`, centered on `cy`; returns its width. Mirrors the
/// mockup's 18+/4K/CC pills (2px border, radius 6).
fn meta_badge(p: Painter, x: f32, cy: f32, text: &str) -> f32 {
    let sz = 22;
    let w = text.chars().count() as f32 * (sz as f32 * 0.60) + 22.0;
    let h = 34.0f32;
    let col = [0.90f32, 0.90, 0.90, 1.0];
    let border = [1.0f32, 1.0, 1.0, 0.55];
    let r = Rect::new(x, cy - h * 0.5, w, h);
    p.rrect(r, 6.0, 6.0, border);
    p.rrect(Rect::new(r.x + 2.0, r.y + 2.0, r.w - 4.0, r.h - 4.0), 5.0, 5.0, [0.133, 0.133, 0.141, 1.0]);
    if let Ok(cs) = CString::new(text) {
        p.text(cs.as_ptr(), x + w * 0.5, cy - sz as f32 * 0.58, sz, col, 1, 1);
    }
    w
}

/// wrap `s` to two lines fitting `budget` px at `sz` (word boundary), eliding the second.
fn wrap2(s: &str, budget: f32, sz: i32) -> (String, String) {
    let fits = |t: &str| {
        CString::new(t).ok().map(|c| crate::text::text_width(c.as_ptr(), sz, 0) <= budget).unwrap_or(true)
    };
    if fits(s) {
        return (s.to_string(), String::new());
    }
    let mut l1 = String::new();
    let mut rest = String::new();
    let mut in_second = false;
    for w in s.split_whitespace() {
        if in_second {
            rest.push_str(w);
            rest.push(' ');
            continue;
        }
        let cand = if l1.is_empty() { w.to_string() } else { format!("{l1} {w}") };
        if fits(&cand) {
            l1 = cand;
        } else {
            in_second = true;
            rest.push_str(w);
            rest.push(' ');
        }
    }
    // elide line 2 with an ellipsis
    let mut l2 = rest.trim().to_string();
    while !l2.is_empty() && !fits(&format!("{l2}…")) {
        l2.pop();
        while l2.ends_with(' ') {
            l2.pop();
        }
    }
    if !rest.trim().is_empty() && rest.trim() != l2 {
        l2.push('…');
    }
    (l1, l2)
}

pub(crate) fn draw() {
    if !is_open() {
        return;
    }
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    let appear = unsafe { addr_of!(APPEAR).read() }.pos.clamp(0.0, 1.0);
    let rise = (1.0 - appear) * 20.0;
    let p = Painter::root().alpha(appear).translate(0.0, rise);

    // card
    let cx = 80.0f32;
    let cw = SCR_W - 160.0;
    let ch = 214.0f32;
    let cyt = SCR_H - 176.0 - ch; // sit just above the Info/Chapters tabs (tabs at SCR_H-128)
    let card = Rect::new(cx, cyt, cw, ch);
    // dark frosted card (matches the track-menu panels) — the mockup's 0.06 light frost washes
    // out over bright video; a near-opaque dark card keeps the title/synopsis legible on any scene
    let cardbg = [0.133f32, 0.133, 0.141, 0.9];
    p.rrect(card, 28.0, 28.0, cardbg);

    let pad = 28.0f32;
    // still (16:9), left; art → thumb fallback
    let sw = 320.0f32;
    let sh = 180.0f32;
    let sx = cx + pad;
    let sy = cyt + (ch - sh) * 0.5;
    let art = if !d.art.is_empty() { d.art.clone() } else { d.thumb.clone() };
    let mut drawn = false;
    if !art.is_empty() {
        if let Ok(ap) = CString::new(art) {
            let t = resolve_tex(ap.as_ptr(), 480, 270, 0);
            if t != 0 {
                p.tex(t, Rect::new(sx, sy, sw, sh), 16.0, [1.0; 4]);
                drawn = true;
            }
        }
    }
    if !drawn {
        p.rrect(Rect::new(sx, sy, sw, sh), 16.0, 16.0, [0.12, 0.13, 0.16, 1.0]);
    }

    // action buttons (right column)
    let acts = actions();
    let bw = 352.0f32;
    let bh = 70.0f32;
    let bx = cx + cw - pad - bw;
    let focus = unsafe { addr_of!(FOCUS).read() };
    let total_bh = acts.len() as f32 * bh + (acts.len().saturating_sub(1)) as f32 * 16.0;
    let mut by = cyt + (ch - total_bh) * 0.5;
    let env = crate::ui::Env { dt: 0.0, screen: Rect::FULL, fr: 0, fc: 0, sp: 0.0, hero_a: 0.0 };
    for (i, label) in acts.iter().enumerate() {
        let icon = if *label == "From Beginning" { Icon::Play } else { Icon::Info };
        if let Ok(cs) = CString::new(*label) {
            crate::ui::widgets::Button::new(cs.as_ptr(), 30, Rect::new(bx, by, bw, bh))
                .icon(icon)
                .focused(i as c_int == focus)
                .draw(&env, p);
        }
        by += bh + 16.0;
    }

    // text block (between the still and the buttons)
    let tx = sx + sw + 34.0;
    let tright = bx - 34.0;
    let tw = tright - tx;
    let white = [0.97f32, 0.98, 1.0, 1.0];
    let dim = [0.69f32, 0.69, 0.71, 1.0]; // #b0b0b3-ish

    // title
    if let Ok(cs) = CString::new(elide(&d.title, tw, 40, 1)) {
        p.text(cs.as_ptr(), tx, cyt + 26.0, 40, white, 0, 1);
    }
    // synopsis (up to 2 lines)
    let syn_y = cyt + 84.0;
    let mut lines = 0.0f32;
    if !d.summary.is_empty() {
        let (l1, l2) = wrap2(&d.summary, tw, 28);
        if let Ok(cs) = CString::new(l1) {
            p.text(cs.as_ptr(), tx, syn_y, 28, dim, 0, 0);
        }
        lines = 1.0;
        if !l2.is_empty() {
            if let Ok(cs) = CString::new(l2) {
                p.text(cs.as_ptr(), tx, syn_y + 34.0, 28, dim, 0, 0);
            }
            lines = 2.0;
        }
    }
    // metadata line: genres · year · duration, then capability badges — a clear gap below the
    // synopsis (was pinned to the card bottom, which crowded a 2-line synopsis)
    let syn_bottom = syn_y + (lines - 1.0).max(0.0) * 34.0 + 28.0;
    let mut meta = Vec::new();
    for g in d.genres.iter().take(2) {
        meta.push(g.clone());
    }
    if d.year > 0 {
        meta.push(d.year.to_string());
    }
    if d.dur_ms > 0 {
        meta.push(fmt_dur(d.dur_ms));
    }
    let my = syn_bottom + 34.0; // 20px gap above the tags row + half its height
    let mut mx = tx;
    if !meta.is_empty() {
        let line = meta.join("   ·   ");
        if let Ok(cs) = CString::new(line.clone()) {
            mx += p.text(cs.as_ptr(), tx, my - 24.0 * 0.58, 24, white, 0, 1);
        }
        mx += 18.0;
    }
    // badges: rating, top-audio Dolby tag, CC/SDH/AD from streams
    if !d.rating.is_empty() {
        mx += meta_badge(p, mx, my, &d.rating) + 12.0;
    }
    if let Some(tag) = d.audio.first().and_then(|s| audio_badge(&s.codec)) {
        mx += meta_badge(p, mx, my, tag) + 12.0;
    }
    if !d.subs.is_empty() {
        mx += meta_badge(p, mx, my, "CC") + 12.0;
    }
    if d.subs.iter().any(|s| s.sdh) {
        mx += meta_badge(p, mx, my, "SDH") + 12.0;
    }
    if d.audio.iter().any(|s| s.ad) {
        mx += meta_badge(p, mx, my, "AD") + 12.0;
    }
    let _ = mx;
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
