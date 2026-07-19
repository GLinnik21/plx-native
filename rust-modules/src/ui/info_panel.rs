//! In-player Info card (mockup "Info mode"): a horizontal card over the transport with the
//! episode/movie still, title + synopsis, a metadata line with outlined capability badges, and a
//! column of action buttons. Opened from the HUD's "Info" tab; app.rs routes D-pad/OK/BACK here
//! while it's open and hides the normal transport middle behind it. Data from crate::metadata.
#![allow(dead_code)]
use crate::metadata;
use crate::ui::consts::{SCR_H, SCR_W, SDLK_DOWN, SDLK_UP};
use crate::ui::icons::Icon;
use crate::ui::popover::Popover;
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::{badge, resolve_tex, BadgeStyle};
use crate::ui::{Painter, Rect, View};
use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

pub enum InfoAction {
    None,
    FromBeginning,
    GoToDetail(String), // rk to open: the show (episode) or the movie
}

static mut POP: Popover = Popover::new(); // shared open/appear choreography
static mut FOCUS: c_int = 0; // index into the action-button column

fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}

pub(crate) fn is_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}
pub(crate) fn open() {
    unsafe { addr_of_mut!(FOCUS).write(0) }
    pop().open();
}
pub(crate) fn close() {
    pop().close();
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
    pop().update(dt);
}

// ---- helpers ----

/// A premium audio format worth badging on the meta line, named by the ONE codec map
/// ([`metadata::friendly_codec`]); everyday codecs (AAC/MP3/…) get no badge.
fn audio_badge(codec: &str) -> Option<String> {
    matches!(codec.to_ascii_lowercase().as_str(), "truehd" | "eac3" | "ec-3" | "ac3" | "dts" | "dca")
        .then(|| metadata::friendly_codec(codec))
}

/// the shared outlined chip in this panel's colours (TEXT_HEADING border/label over the card)
fn meta_badge(p: Painter, x: f32, cy: f32, text: &str) -> f32 {
    badge(p, x, cy, text, BadgeStyle::Outlined { col: theme::TEXT_HEADING, bg: theme::SURFACE_PANEL })
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
    let p = pop().painter(0.0, 20.0); // no scrim — the card floats over the transport

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
    // capability badges come from the PLAYING item's own tracks — `current()` is the show
    // (episode-1 streams) during a show-page episode play, or another item entirely
    let (audio, subs): (&[metadata::Stream], &[metadata::Stream]) = match metadata::playing() {
        Some(t) => (&t.audio, &t.subs),
        None => (
            d.map(|x| x.audio.as_slice()).unwrap_or(&[]),
            d.map(|x| x.subs.as_slice()).unwrap_or(&[]),
        ),
    };

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
        let t = resolve_tex(&thumb_path, 480, 270, 0);
        if t != 0 {
            p.tex(t, Rect::new(sx, sy, sw, sh), 16.0, theme::TINT_WHITE);
            drawn = true;
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
    let env = crate::ui::Env::inert();
    for (i, label) in acts.iter().enumerate() {
        let icon = if *label == "From Beginning" { Icon::Play } else { Icon::Info };
        if let Ok(cs) = CString::new(*label) {
            crate::ui::widgets::Button::new(cs.as_ptr(), theme::size::BODY, Rect::new(bx, by, bw, bh))
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
    let has_tags = year > 0
        || dur_ms > 0
        || !rating.is_empty()
        || d.map(|x| !x.genres.is_empty()).unwrap_or(false)
        || !subs.is_empty()
        || audio.iter().any(|s| s.ad)
        || audio.first().and_then(|s| audio_badge(&s.codec)).is_some();

    // vertical rhythm — line *advances* (deliberately below the full font line-box) + small gaps
    let title_h = 42.0f32; // title advance (font 40)
    let syn_lh = 31.0f32; // synopsis line advance (font 28)
    let tag_h = 34.0f32;
    let gap_title = 6.0f32; // title → synopsis
    let gap_tags = 12.0f32; // synopsis → tags

    // title (1 line, elided) + synopsis (up to 2 lines, ellipsized) through the shared TextView —
    // its wrap is memoised internally, replacing this panel's old hand-rolled wrap2/WrapCache.
    let title_v = TextView::new(&info_title, theme::size::TITLE, white).bold().max_lines(1);
    let syn_v = TextView::new(&summary, theme::size::BODY, dim).leading(syn_lh).max_lines(2);
    let syn_h = if summary.is_empty() { 0.0 } else { syn_v.measure_h(tw) };

    // centre the [title + synopsis + tag row] group in the card (cap-top coordinates)
    let span = title_h
        + if syn_h > 0.0 { gap_title + syn_h } else { 0.0 }
        + if has_tags { gap_tags + tag_h } else { 0.0 };
    let mut ty = cyt + (ch - span) * 0.5;
    title_v.draw(p, Rect::new(tx, ty, tw, 0.0));
    ty += title_h;
    if syn_h > 0.0 {
        ty += gap_title;
        syn_v.draw(p, Rect::new(tx, ty, tw, 0.0));
        ty += syn_h;
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
            meta.push(crate::ui::fmt::dur_short(dur_ms));
        }
        let mut mx = tx;
        if !meta.is_empty() {
            let line = meta.join("   ·   ");
            if let Ok(cs) = CString::new(line) {
                let ly = crate::text::text_vcenter_y(theme::size::CAPTION, 1, my);
                mx += p.text(cs.as_ptr(), tx, ly, theme::size::CAPTION, white, 0, 1);
            }
            mx += 18.0;
        }
        // badges: rating (from the leaf), top-audio Dolby tag, CC/SDH/AD (from the loaded streams)
        if !rating.is_empty() {
            mx += meta_badge(p, mx, my, &rating) + 12.0;
        }
        if let Some(tag) = audio.first().and_then(|s| audio_badge(&s.codec)) {
            mx += meta_badge(p, mx, my, &tag) + 12.0;
        }
        if !subs.is_empty() {
            mx += meta_badge(p, mx, my, "CC") + 12.0;
        }
        if subs.iter().any(|s| s.sdh) {
            mx += meta_badge(p, mx, my, "SDH") + 12.0;
        }
        if audio.iter().any(|s| s.ad) {
            mx += meta_badge(p, mx, my, "AD") + 12.0;
        }
        let _ = mx;
    }
}

