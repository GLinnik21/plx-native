//! In-player Info card (mockup "Info mode"): a horizontal card over the transport with the
//! episode/movie still, title + synopsis, a metadata line with outlined capability badges, and a
//! column of action buttons. Opened from the HUD's "Info" tab; app.rs routes D-pad/OK/BACK here
//! while it's open and hides the normal transport middle behind it. Data from crate::metadata.
#![allow(dead_code)]
use crate::metadata;
use crate::ui::consts::{SDLK_DOWN, SDLK_UP};
use crate::ui::icons::Icon;
use crate::ui::theme;
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
    GoToDetail(String), // rk to open: the show (episode) or the movie
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

/// whether the playing item is an episode (→ "Go to Show") rather than a movie ("Go to Movie")
fn is_episode() -> bool {
    metadata::now_playing().map(|n| n.is_episode).unwrap_or(false)
}

/// the action-button labels for the playing item
fn actions() -> Vec<&'static str> {
    vec!["From Beginning", if is_episode() { "Go to Show" } else { "Go to Movie" }]
}

/// true when focus is on the last action button — a further DOWN should leave the card (back to the
/// tabs) rather than staying pinned to the bottom row
pub(crate) fn at_last() -> bool {
    let f = unsafe { addr_of!(FOCUS).read() };
    f >= actions().len() as c_int - 1
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
    if f <= 0 {
        return InfoAction::FromBeginning;
    }
    // second action opens the show (episode) or the movie
    let rk = metadata::now_playing()
        .map(|n| n.detail_rk.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| metadata::current().map(|d| d.rk.clone()))
        .unwrap_or_default();
    if rk.is_empty() {
        InfoAction::None
    } else {
        InfoAction::GoToDetail(rk)
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
    let col = theme::TEXT_HEADING;
    let border = theme::OVERLAY_BORDER;
    let r = Rect::new(x, cy - h * 0.5, w, h);
    p.rrect(r, 6.0, 6.0, border);
    p.rrect(Rect::new(r.x + 2.0, r.y + 2.0, r.w - 4.0, r.h - 4.0), 5.0, 5.0, theme::SURFACE_PANEL);
    if let Ok(cs) = CString::new(text) {
        let ty = crate::text::text_vcenter_y(sz, 1, cy);
        p.text(cs.as_ptr(), x + w * 0.5, ty, sz, col, 1, 1);
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

// Memoised title/synopsis wrap. `elide`/`wrap2` above each measure MANY throwaway candidate strings
// through `text_width` — a `TTF_RenderUTF8_Blended` + full-surface ink scan + GL upload PER candidate.
// Re-running that every frame (the panel's whole open lifetime) thrashed the glyph cache and dropped
// the panel to ~1fps. The wrapped result only changes when the title/summary/column-width do, so cache
// it and every subsequent frame is three cheap string clones.
struct WrapCache {
    title_src: String,
    summary_src: String,
    tw: f32,
    title: String,
    syn1: String,
    syn2: String,
}
static mut WRAP: Option<WrapCache> = None;

fn wrapped(title_src: &str, summary_src: &str, tw: f32) -> (String, String, String) {
    unsafe {
        if let Some(c) = &*addr_of!(WRAP) {
            if c.title_src == title_src && c.summary_src == summary_src && (c.tw - tw).abs() < 0.5 {
                return (c.title.clone(), c.syn1.clone(), c.syn2.clone());
            }
        }
        let title = elide(title_src, tw, 40, 1);
        let (syn1, syn2) =
            if summary_src.is_empty() { (String::new(), String::new()) } else { wrap2(summary_src, tw, 28) };
        *addr_of_mut!(WRAP) = Some(WrapCache {
            title_src: title_src.to_string(),
            summary_src: summary_src.to_string(),
            tw,
            title: title.clone(),
            syn1: syn1.clone(),
            syn2: syn2.clone(),
        });
        (title, syn1, syn2)
    }
}

pub(crate) fn draw() {
    if !is_open() {
        return;
    }
    let np = metadata::now_playing();
    let d = metadata::current();
    if np.is_none() && d.is_none() {
        return;
    }
    let appear = unsafe { addr_of!(APPEAR).read() }.pos.clamp(0.0, 1.0);
    let rise = (1.0 - appear) * 20.0;
    let p = Painter::root().alpha(appear).translate(0.0, rise);

    // Resolve the playing leaf's fields: `now_playing` describes the episode (show title + SxEy +
    // its still) or the movie; the loaded `Detail` backs the capability badges + genres.
    let is_ep = np.map(|n| n.is_episode).unwrap_or(false);
    let big_title = np.map(|n| n.title.clone()).or_else(|| d.map(|x| x.title.clone())).unwrap_or_default();
    let ep_name = np.map(|n| n.ep_title.clone()).unwrap_or_default();
    let summary = np.map(|n| n.summary.clone()).filter(|s| !s.is_empty())
        .or_else(|| d.map(|x| x.summary.clone())).unwrap_or_default();
    let year = np.map(|n| n.year).or_else(|| d.map(|x| x.year)).unwrap_or(0);
    let dur_ms = np.map(|n| n.dur_ms).or_else(|| d.map(|x| x.dur_ms)).unwrap_or(0);
    let rating = np.map(|n| n.rating.clone()).filter(|s| !s.is_empty())
        .or_else(|| d.map(|x| x.rating.clone())).unwrap_or_default();
    let thumb_path = np.map(|n| n.thumb.clone()).filter(|s| !s.is_empty())
        .or_else(|| d.map(|x| if !x.art.is_empty() { x.art.clone() } else { x.thumb.clone() }))
        .unwrap_or_default();

    // card — tall enough that the still gets equal padding on every side (see `pad`/`sh` below)
    let cx = 80.0f32;
    let cw = SCR_W - 160.0;
    let ch = 236.0f32;
    let cyt = SCR_H - 176.0 - ch; // sit just above the Info/Chapters tabs (tabs at SCR_H-128)
    let card = Rect::new(cx, cyt, cw, ch);
    // near-opaque dark card keeps the title/synopsis legible over any scene
    let cardbg = theme::PANEL_TOP;
    p.rrect(card, 28.0, 28.0, cardbg);

    let pad = 28.0f32;
    // still (16:9), left — the *episode's* thumbnail (or the movie's landscape art). `ch` is sized
    // so (ch - sh)/2 == pad, giving the still an equal `pad` margin on every side.
    let sw = 320.0f32;
    let sh = 180.0f32;
    let sx = cx + pad;
    let sy = cyt + (ch - sh) * 0.5;
    let mut drawn = false;
    if !thumb_path.is_empty() {
        if let Ok(ap) = CString::new(thumb_path) {
            let t = resolve_tex(ap.as_ptr(), 480, 270, 0);
            if t != 0 {
                p.tex(t, Rect::new(sx, sy, sw, sh), 16.0, theme::TINT_WHITE);
                drawn = true;
            }
        }
    }
    if !drawn {
        p.rrect(Rect::new(sx, sy, sw, sh), 16.0, 16.0, theme::CARD_PLACEHOLDER);
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

    // text block (between the still and the buttons): title + synopsis + tags, cap-band centred as a
    // group. Title is the playing leaf's own name (episode name / movie title) — the show-title +
    // SxEy treatment lives on the transport HUD under the playbar, not this card.
    let tx = sx + sw + 34.0;
    let tright = bx - 34.0;
    let tw = tright - tx;
    let white = theme::TEXT_PRIMARY;
    let dim = theme::TEXT_SECONDARY;

    let info_title = if is_ep { ep_name.clone() } else { big_title.clone() };
    let (title, syn1, syn2) = wrapped(&info_title, &summary, tw);
    let n_syn = (!syn1.is_empty()) as i32 + (!syn2.is_empty()) as i32;
    let has_tags = year > 0
        || dur_ms > 0
        || !rating.is_empty()
        || d.map(|x| !x.genres.is_empty() || !x.subs.is_empty() || x.audio.iter().any(|s| s.ad)).unwrap_or(false)
        || d.and_then(|x| x.audio.first()).and_then(|s| audio_badge(&s.codec)).is_some();

    // vertical rhythm — line *advances* (deliberately below the full font line-box) + small gaps
    let title_h = 42.0f32; // title advance (font 40)
    let syn_lh = 31.0f32; // synopsis line advance (font 28)
    let tag_h = 34.0f32;
    let gap_title = 6.0f32; // title → synopsis
    let gap_tags = 12.0f32; // synopsis → tags

    // Cap-band centring: visual top is the title cap-top, visual bottom is the tag badge box (or,
    // tag-less, the last synopsis baseline). Descenders never enter the maths.
    let (tcap_t, tcap_b) = crate::text::text_cap_band(40, 1);
    let syn_span = if n_syn > 0 { gap_title + n_syn as f32 * syn_lh } else { 0.0 };
    let span_bottom = if has_tags {
        title_h + syn_span + gap_tags + tag_h
    } else if n_syn > 0 {
        let (_st, sbase) = crate::text::text_cap_band(28, 0);
        title_h + gap_title + (n_syn as f32 - 1.0) * syn_lh + sbase
    } else {
        tcap_b
    };
    let mut ty = cyt + ch * 0.5 - (tcap_t + span_bottom) * 0.5; // title draw-y

    // title
    if let Ok(cs) = CString::new(title) {
        p.text(cs.as_ptr(), tx, ty, 40, white, 0, 1);
    }
    ty += title_h;
    // synopsis (up to 2 lines)
    if n_syn > 0 {
        ty += gap_title;
        for l in [&syn1, &syn2] {
            if l.is_empty() {
                continue;
            }
            if let Ok(cs) = CString::new(l.as_str()) {
                p.text(cs.as_ptr(), tx, ty, 28, dim, 0, 0);
            }
            ty += syn_lh;
        }
    }
    // metadata line (genres · year · duration) + capability badges, centred on the tag row
    if has_tags {
        ty += gap_tags;
        let my = ty + tag_h * 0.5; // vertical centre of the tag row
        let mut meta = Vec::new();
        if let Some(x) = d {
            for g in x.genres.iter().take(2) {
                meta.push(g.clone());
            }
        }
        if year > 0 {
            meta.push(year.to_string());
        }
        if dur_ms > 0 {
            meta.push(fmt_dur(dur_ms));
        }
        let mut mx = tx;
        if !meta.is_empty() {
            let line = meta.join("   ·   ");
            if let Ok(cs) = CString::new(line) {
                let ly = crate::text::text_vcenter_y(24, 1, my);
                mx += p.text(cs.as_ptr(), tx, ly, 24, white, 0, 1);
            }
            mx += 18.0;
        }
        // badges: rating (from the leaf), top-audio Dolby tag, CC/SDH/AD (from the loaded streams)
        if !rating.is_empty() {
            mx += meta_badge(p, mx, my, &rating) + 12.0;
        }
        if let Some(x) = d {
            if let Some(tag) = x.audio.first().and_then(|s| audio_badge(&s.codec)) {
                mx += meta_badge(p, mx, my, tag) + 12.0;
            }
            if !x.subs.is_empty() {
                mx += meta_badge(p, mx, my, "CC") + 12.0;
            }
            if x.subs.iter().any(|s| s.sdh) {
                mx += meta_badge(p, mx, my, "SDH") + 12.0;
            }
            if x.audio.iter().any(|s| s.ad) {
                mx += meta_badge(p, mx, my, "AD") + 12.0;
            }
        }
        let _ = mx;
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
