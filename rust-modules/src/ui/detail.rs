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
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::{cfield, resolve_tex, Button, CircleButton};
use crate::ui::{Env, Painter, Rect, Spring, View}; // View: Button/CircleButton::draw
use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::ptr::addr_of_mut;

// ---- retained view-tree migration (step 6.2): detail's mutable state lives in DetailView, reached
// through the lazy view() accessor. The frozen pub(crate) fns (open/update/draw/move_focus/…) keep
// their identical signatures and read/write view() fields directly. LAZY init (unlike home's eager
// scene()) because detail has no detail_init C-ABI — open/open_rk are usually the first calls, but a
// draw before open must not panic, so view() builds a default DetailView on first touch.
struct DetailView {
    selected: c_int,
    section: c_int, // 0=hero buttons, 1=season tabs, 2=episodes, 3=related, 4=cast, 5=about
    col: c_int,     // focused item within the section
    scroll: Spring,
    card_scale: Spring, // focused card-row item pop (springs on selection change)
    ep_hscroll: Spring, // episode row horizontal scroll — glides instead of snapping
    last_resume_ns: i64, // resume position (ns) on_ok just started (0 = from start); app.rs seeks here
}
impl DetailView {
    fn new() -> Self {
        Self {
            selected: -1,
            section: 0,
            col: 0,
            scroll: Spring::at(0.0),
            card_scale: Spring::at(1.0),
            ep_hscroll: Spring::at(0.0),
            last_resume_ns: 0,
        }
    }
}
static mut VIEW: Option<DetailView> = None;
fn view() -> &'static mut DetailView {
    unsafe { (*addr_of_mut!(VIEW)).get_or_insert_with(DetailView::new) }
}

const NBTN: c_int = 3;
const PW: f32 = 168.0; // Play pill width
const CGAP: f32 = 20.0;
const CD: f32 = 60.0; // circle button diameter

// Below-the-hero content is ONE vertical scroll of stacked blocks. Each block's pre-scroll top Y is
// COMPUTED (`section_y`) by stacking the *present* blocks' heights from CONTENT_TOP with a single
// SECTION_GAP between them — no hard-coded per-section Y constants to keep in sync. A block's height
// is derived from its content (e.g. the Related block tracks REL_H, which tracks the shared home
// poster size), so resizing one block reflows everything below it automatically. The scroll lifts
// the focused block's top to TOP_MARGIN under the compact title.
const TOP_MARGIN: f32 = 120.0;
const CONTENT_TOP: f32 = 920.0; // first block sits just under the hero buttons
const SECTION_GAP: f32 = 110.0; // vertical padding between top-level blocks
const TAB_EP_GAP: f32 = 21.0; // season tabs → their episode row (they read as one unit)
const SCROLLED: f32 = CONTENT_TOP - TOP_MARGIN; // backdrop-dim saturation reference (= 800)

// Season tabs (header for the episode row)
const TAB_ROW_H: f32 = 44.0;
// Episodes: landscape stills + under-card metadata
const EP_W: f32 = 420.0;
const EP_H: f32 = 236.0; // 16:9-ish still
const EP_GAP: f32 = 28.0;
const EP_META_H: f32 = 170.0; // kicker/title/summary/date drawn below the still
// Related row (portrait posters) reuses the home shelf poster geometry (consts::CARD_*) so a poster
// is one size app-wide (its texture request was already 250×375 → now drawn 1:1 sharp).
const REL_W: f32 = CARD_W; // 250
const REL_H: f32 = CARD_H; // 375
const REL_GAP: f32 = GAP; // 30
const REL_LABEL_H: f32 = 46.0; // "Related" heading → poster row
const REL_UNDER_H: f32 = 54.0; // poster row → tile title → block bottom
// Cast & Crew row (circular headshots)
const CAST_D: f32 = 150.0; // headshot diameter
const CAST_SLOT: f32 = 200.0; // per-member horizontal pitch (room for the name)
const CAST_LABEL_H: f32 = 60.0; // "Cast & Crew" heading → headshot row
const CAST_UNDER_H: f32 = 76.0; // headshot → name/role → block bottom

/// the selected catalog row (backdrop art/blur), if any
fn selected() -> Option<&'static PmsMovie> {
    let idx = view().selected;
    if idx < 0 || idx as usize >= crate::pms::nmovies() {
        return None;
    }
    unsafe { crate::pms::movie_ptr(idx as usize).as_ref() }
}

/// the focused hero button (0=Play), or -1 when the hero section isn't focused
pub(crate) fn focus() -> c_int {
    let v = view();
    if v.section == 0 {
        v.col
    } else {
        -1
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
        5 => 4, // About: card + Information + Languages + Accessibility (selection moves between them)
        _ => 0,
    }
}
/// content height of a scrollable block — how far the next block is pushed down. Content-derived, so
/// changing a size (poster, still) reflows the stack. About (5) is last, so its height pushes nothing.
fn block_h(section: c_int) -> f32 {
    match section {
        1 => TAB_ROW_H,
        2 => EP_H + EP_META_H,
        3 => REL_LABEL_H + REL_H + REL_UNDER_H,
        4 => CAST_LABEL_H + CAST_D + CAST_UNDER_H,
        _ => 0.0,
    }
}
/// pre-scroll top Y of a section: stack the present blocks from CONTENT_TOP, SECTION_GAP between each
/// (season tabs → episodes hug with TAB_EP_GAP so they read as one unit). The single source of every
/// below-hero Y — both the draws and the scroll target read it.
fn section_y(target: c_int) -> f32 {
    let mut y = CONTENT_TOP;
    let mut prev: Option<c_int> = None;
    for &s in sections().iter() {
        if s == 0 {
            continue; // hero is pinned, not part of the scroll stack
        }
        if let Some(p) = prev {
            y += if p == 1 && s == 2 { TAB_EP_GAP } else { SECTION_GAP };
        }
        if s == target {
            return y;
        }
        y += block_h(s);
        prev = Some(s);
    }
    y
}
/// scroll offset that lifts the focused section's top to TOP_MARGIN
fn scroll_target() -> f32 {
    let sec = view().section;
    if sec == 0 {
        return 0.0;
    }
    // episodes anchor on the season tabs above them, so the tabs stay visible while browsing episodes
    let anchor = if sec == 2 { 1 } else { sec };
    (section_y(anchor) - TOP_MARGIN).max(0.0)
}
/// the selected catalog row pointer (for the app to play a movie), or null
pub(crate) fn selected_ptr() -> *mut PmsMovie {
    let idx = view().selected;
    if idx < 0 || idx as usize >= crate::pms::nmovies() {
        return std::ptr::null_mut();
    }
    crate::pms::movie_ptr(idx as usize)
}
/// is the loaded item a TV show?
pub(crate) fn is_show() -> bool {
    metadata::current().map(|d| d.is_show).unwrap_or(false)
}

/// Open the detail page for catalog row `idx`: load its full detail (blocking) and
/// reset focus/scroll.
pub(crate) fn open(idx: c_int) {
    crate::ui::track_menu::reset(); // fresh item → drop the previous item's track selection
    let v = view();
    v.selected = idx;
    v.section = 0;
    v.col = 0;
    v.scroll.jump(0.0);
    if idx >= 0 && (idx as usize) < crate::pms::nmovies() {
        if let Some(m) = unsafe { crate::pms::movie_ptr(idx as usize).as_ref() } {
            let rk = cfield(&m.rk);
            if !rk.is_empty() {
                metadata::load_detail(&rk);
            }
        }
    }
}

/// Leave the detail page (drop the loaded item).
pub(crate) fn close() {
    metadata::clear();
    view().selected = -1;
}

pub(crate) fn move_focus(sym: c_int) {
    let sym = sym as u32;
    let v = view();
    let sec = v.section;
    let col = v.col;
    if sym == SDLK_LEFT || sym == SDLK_RIGHT {
        let n = n_items(sec);
        if n <= 0 {
            return;
        }
        let nc = if sym == SDLK_LEFT { (col - 1).max(0) } else { (col + 1).min(n - 1) };
        if nc != col {
            v.col = nc;
            v.card_scale.jump(1.0); // re-pop the newly-focused card
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
            v.section = ns;
            v.card_scale.jump(1.0); // pop the card in the newly-entered row
            // land on the active season when entering the tabs; else the first item
            let start = if ns == 1 {
                metadata::current().map(|d| d.cur_season as c_int).unwrap_or(0)
            } else {
                0
            };
            v.col = start;
        }
    }
}

/// episode-row horizontal scroll target: pin the focused card to the 2nd slot (0 when the episode
/// row isn't the focused section, so it glides back to the start)
fn ep_hscroll_target() -> f32 {
    let v = view();
    let (sec, col) = (v.section, v.col);
    if sec == 2 && col > 1 {
        (col as f32 - 1.0) * (EP_W + EP_GAP)
    } else {
        0.0
    }
}

pub(crate) fn update(dt: f32) {
    // targets read view() internally — compute them before borrowing v for the springs
    let sct = scroll_target();
    let hst = ep_hscroll_target();
    let v = view();
    v.scroll.step(sct, K_SCROLL, dt);
    crate::ui::anim::probe("detail.scroll", v.scroll.pos, v.scroll.vel, sct, dt);
    v.card_scale.step(crate::ui::widgets::CARD_FOCUS_SCALE, 300.0, dt);
    crate::ui::anim::probe("detail.card", v.card_scale.pos, v.card_scale.vel, crate::ui::widgets::CARD_FOCUS_SCALE, dt);
    v.ep_hscroll.step(hst, 240.0, dt);
    crate::ui::anim::probe("detail.epscroll", v.ep_hscroll.pos, v.ep_hscroll.vel, hst, dt);
}

fn env_of(dt: f32) -> Env {
    Env { dt, screen: Rect::FULL, fr: focus(), fc: 0, sp: 1.0, hero_a: 1.0 }
}

pub(crate) fn draw() {
    let p = Painter::root();
    let env = env_of(0.0);
    let m = selected();
    let scroll = view().scroll.pos;
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
                    p.tex(t, Rect::FULL, 0.0, theme::with_a(theme::TINT_WHITE, art_a));
                }
            }
        }
    }
    // bottom scrim for the hero's lower-left content (only while the hero is visible)
    if sf < 0.99 {
        p.rect(
            Rect::new(0.0, SCR_H * 0.34, SCR_W, SCR_H * 0.66),
            0.0,
            theme::scrim(0.0),
            theme::scrim(0.95 * (1.0 - sf)),
            0.0,
        );
    }
    // overall dim as the page scrolls into the rows (legibility for the row text)
    let dk = sf * 0.55;
    if dk > 0.001 {
        p.rect(Rect::FULL, 0.0, theme::scrim(dk), theme::scrim(dk), 0.0);
    }
}

fn draw_hero(p: Painter, env: &Env, m: Option<&PmsMovie>) {
    let tx = MARGIN_X;
    let w_a = theme::TEXT_PRIMARY;
    let d_a = theme::TEXT_SECONDARY;
    let dim = theme::TEXT_TERTIARY;
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

    // ---- synopsis: pixel-wrapped to the hero text column, 2 lines max ----
    let summary = d.map(|d| d.summary.clone()).or_else(|| m.map(|m| cfield(&m.summary))).unwrap_or_default();
    let syn_y = meta_y + 46.0;
    if !summary.is_empty() {
        TextView::new(&summary, 24, d_a)
            .leading(30.0)
            .max_lines(2)
            .draw(p, Rect::new(tx, syn_y, 900.0, 0.0));
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
                // right-aligned against the right margin (measured directly, no invisible fake-draw)
                let w = crate::text::text_width(sc.as_ptr(), 24, 1);
                p.text(sc.as_ptr(), SCR_W - MARGIN_X - w, btn_y + 16.0, 24, d_a, 0, 1);
            }
        }
    }
}

fn draw_buttons(p: Painter, env: &Env, y: f32) {
    // One shared control family (widgets::Button/CircleButton, default ControlStyle::Accent — the
    // same as the info card): the focused control fills warm ACCENT with dark ink, the rest are
    // solid dark discs. Play reuses the pill Button; +/i are the disc CircleButton.
    let tx = MARGIN_X;
    let focus = focus();
    let cx1 = tx + PW + CGAP;
    let cx2 = cx1 + CD + CGAP;

    Button::new(c"Play".as_ptr(), 30, Rect::new(tx, y, PW, CD))
        .icon(crate::ui::icons::Icon::Play)
        .focused(focus == 0)
        .draw(env, p);
    CircleButton::new(c"+".as_ptr()).at(cx1, y).focused(focus == 1).draw(env, p);
    CircleButton::new(c"i".as_ptr()).at(cx2, y).focused(focus == 2).draw(env, p);
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
                p.tex(lt, Rect::new(cx - ww * 0.5, 40.0, ww, hh), 0.0, theme::TEXT_PRIMARY);
                return;
            }
        }
    }
    if let Ok(t) = CString::new(title) {
        p.text(t.as_ptr(), cx, 54.0, 40, theme::TEXT_PRIMARY, 1, 1);
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
    let sec = view().section;
    let col = view().col;
    let tab_y = section_y(1);
    // segmented control: the *selected* season carries a subtle pill (bright ACCENT while the tab row
    // is focused); non-selected seasons are plain dim text. (TabPill handles the state → look.)
    let e = Env { dt: 0.0, screen: Rect::FULL, fr: 0, fc: 0, sp: 0.0, hero_a: 0.0 };
    let mut x = MARGIN_X;
    for (i, s) in d.seasons.iter().enumerate() {
        let label = if s.title.is_empty() { format!("Season {}", s.index) } else { s.title.clone() };
        let lc = match CString::new(label) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let selected = i == d.cur_season;
        let focused = sec == 1 && col == i as c_int;
        // pill sized to the (bold) label; layout preserved — text sits at x, pill padded ±18, tabs
        // advance by label width + 52.
        let w = crate::text::text_width(lc.as_ptr(), 30, 1);
        crate::ui::widgets::TabPill::new(lc.as_ptr(), 30, Rect::new(x - 18.0, tab_y - 8.0, w + 36.0, 50.0))
            .segment(selected)
            .focused(focused)
            .draw(&e, p);
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
    let sec = view().section;
    let col = view().col;
    let focus_col = if sec == 2 { col } else { -1 };
    let scale = view().card_scale.pos;
    // keep the focused card on-screen (spring-scrolled so it glides to the 2nd slot instead of
    // snapping — matches the chapters strip; fixes the "scatter" on LEFT/RIGHT)
    let sx = view().ep_hscroll.pos;
    let pe = p.translate(-sx, 0.0);
    let dimc = theme::TEXT_TERTIARY;
    let ep_y = section_y(2);
    for (i, ep) in d.episodes.iter().enumerate() {
        let x = MARGIN_X + i as f32 * (EP_W + EP_GAP);
        if x - sx > SCR_W || x - sx + EP_W < 0.0 {
            continue; // off-screen
        }
        let focused = i as c_int == focus_col;
        let card = Rect::new(x, ep_y, EP_W, EP_H);
        // episode still + focus ring + scale-pop (shared with the chapters strip)
        crate::ui::widgets::draw_card(pe, card, &ep.thumb, (640, 360), 12.0, focused, scale);
        // resume bar (tracks the scaled card when focused)
        if ep.resume_ms > 0 && ep.dur_ms > 0 {
            let cr = if focused { card.scaled(scale) } else { card };
            let frac = (ep.resume_ms as f32 / ep.dur_ms as f32).clamp(0.0, 1.0);
            let bar = Rect::new(cr.x + 12.0, cr.y + cr.h - 16.0, cr.w - 24.0, 5.0);
            pe.rrect(bar, 2.5, 2.5, theme::RAIL_BUFFERED);
            pe.rrect(Rect::new(bar.x, bar.y, bar.w * frac, bar.h), 2.5, 2.5, theme::RAIL_FILL);
        }
        // under-card metadata
        let ty = ep_y + EP_H + 30.0;
        let titc = if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
        if let Ok(ec) = CString::new(format!("EPISODE {}", ep.index)) {
            pe.text(ec.as_ptr(), x, ty, 18, dimc, 0, 1);
        }
        if let Ok(tc) = CString::new(ep.title.clone()) {
            pe.text(tc.as_ptr(), x, ty + 26.0, 24, titc, 0, 1);
        }
        if !ep.summary.is_empty() {
            TextView::new(&ep.summary, 20, dimc)
                .leading(26.0)
                .max_lines(2)
                .draw(pe, Rect::new(x, ty + 62.0, EP_W, 0.0));
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
    let related_y = section_y(3);
    p.text(c"Related".as_ptr(), MARGIN_X, related_y, 28, theme::TEXT_HEADING, 0, 1);
    let sec = view().section;
    let col = view().col;
    let focus_col = if sec == 3 { col } else { -1 };
    let row_y = related_y + REL_LABEL_H;
    let sx = if focus_col > 1 { (focus_col as f32 - 1.0) * (REL_W + REL_GAP) } else { 0.0 };
    let pr = p.translate(-sx, 0.0);
    for (i, r) in d.related.iter().enumerate() {
        let x = MARGIN_X + i as f32 * (REL_W + REL_GAP);
        if x - sx > SCR_W || x - sx + REL_W < 0.0 {
            continue;
        }
        let focused = i as c_int == focus_col;
        // same shared art card as the episode / chapters strips (portrait poster + tight focus ring)
        crate::ui::widgets::draw_card(pr, Rect::new(x, row_y, REL_W, REL_H), &r.thumb, (250, 375), 10.0, focused, 1.05);
        if focused {
            if let Ok(tc) = CString::new(r.title.clone()) {
                pr.text(tc.as_ptr(), x, row_y + REL_H + 30.0, 20, theme::TEXT_HEADING, 0, 0);
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
    let cast_y = section_y(4);
    p.text(c"Cast & Crew".as_ptr(), MARGIN_X, cast_y, 28, theme::TEXT_HEADING, 0, 1);
    let sec = view().section;
    let col = view().col;
    let focus_col = if sec == 4 { col } else { -1 };
    let row_y = cast_y + CAST_LABEL_H;
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
                    pc.tex(t, circ, dp * 0.5, theme::TINT_WHITE);
                    drew = true;
                }
            }
        }
        if !drew {
            pc.rect(circ, dp * 0.5, theme::SKELETON_TOP, theme::SKELETON_BOT, 0.0);
        }
        if focused {
            let fc = Rect::new(cxc - CAST_D * 0.5, row_y, CAST_D, CAST_D);
            pc.ring(fc, 6.0, CAST_D * 0.5, 1.0);
        }
        let name_c = if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
        if let Ok(nc) = CString::new(c.tag.clone()) {
            pc.text(nc.as_ptr(), cxc, row_y + CAST_D + 22.0, 21, name_c, 1, if focused { 1 } else { 0 });
        }
        if !c.role.is_empty() {
            if let Ok(rc) = CString::new(c.role.clone()) {
                pc.text(rc.as_ptr(), cxc, row_y + CAST_D + 48.0, 17, theme::TEXT_TERTIARY, 1, 0);
            }
        }
    }
}

/// resume position (ns) for the last on_ok play case (0 = from the beginning). app.rs
/// captures this after start_bufferfeed and seeks there once the pipeline is ready.
pub(crate) fn last_resume_ns() -> i64 {
    view().last_resume_ns
}
/// apply Plex's resume rule (skip <10s and >95%) and stash the position for app.rs.
fn set_resume(resume_ms: i64, dur_ms: i64) {
    let ns = if resume_ms > 10_000 && (dur_ms <= 0 || (resume_ms as f64) < 0.95 * dur_ms as f64) {
        resume_ms * 1_000_000
    } else {
        0
    };
    view().last_resume_ns = ns;
}

/// OK/SELECT on the detail page: returns true if playback should start (the route
/// URL/HUD have already been set). Section 0 = hero Play, 1 = season tab, 2 = episode.
pub(crate) fn on_ok() -> bool {
    view().last_resume_ns = 0; // default: no resume (set below for plays)
    let sec = view().section;
    let col = view().col;
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
                set_resume(
                    metadata::current().map(|d| d.resume_ms).unwrap_or(0),
                    metadata::current().map(|d| d.dur_ms).unwrap_or(0),
                );
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
    let v = view();
    v.selected = idx;
    v.section = 0;
    v.col = 0;
    v.scroll.jump(0.0);
    metadata::load_detail(rk);
}

/// Open the SHOW detail page for `show_rk` with the season numbered `season_num` selected
/// (a season entry point routes to the show page, not a standalone season page).
pub(crate) fn open_rk_season(show_rk: &str, season_num: c_int) {
    open_rk(show_rk);
    if let Some(d) = metadata::current() {
        if let Some(pos) = d.seasons.iter().position(|s| s.index as c_int == season_num) {
            metadata::load_season(pos);
        }
    }
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
    // describe the playing episode for the in-player Info card (current() stays on the show here)
    let ep_year = ep.aired.get(0..4).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    metadata::set_now_playing(Some(metadata::NowPlaying {
        is_episode: true,
        title: show.clone(),
        ep_title: ep.title.clone(),
        season: ep.season,
        index: ep.index,
        summary: ep.summary.clone(),
        year: ep_year,
        dur_ms: ep.dur_ms,
        rating: ep.rating.clone(),
        thumb: ep.thumb.clone(),
        detail_rk: d.rk.clone(),
    }));
    set_resume(ep.resume_ms, ep.dur_ms);
    crate::route::play_episode(&ep.rk, &ep.part, &ep.vcodec, &ep.acodec, &hud_title, &hud_ctx);
    true
}

// ---- About footer (section 5): heading + card + Information/Languages/Accessibility ----

fn text_at(p: Painter, x: f32, y: f32, sz: c_int, col: [f32; 4], bold: c_int, s: &str) -> f32 {
    match CString::new(s) {
        Ok(t) => p.text(t.as_ptr(), x, y, sz, col, 0, bold),
        Err(_) => 0.0,
    }
}

/// a dim label over one/two pixel-wrapped value lines; returns the vertical advance
fn draw_pair(p: Painter, x: f32, y: f32, label: &str, value: &str, lbl: [f32; 4], val: [f32; 4]) -> f32 {
    text_at(p, x, y, 20, lbl, 0, label);
    let h = TextView::new(value, 24, val).bold().leading(26.0).max_lines(2).draw(p, Rect::new(x, y + 30.0, 520.0, 0.0));
    30.0 + h.max(26.0) + 22.0
}

/// a small rounded accessibility badge (CC / SDH / AD)
fn draw_badge(p: Painter, x: f32, y: f32, label: &str) {
    let (w, h) = (48.0f32, 30.0f32);
    p.rrect(Rect::new(x, y, w, h), 7.0, 7.0, [0.86, 0.88, 0.92, 0.20]);
    if let Ok(t) = CString::new(label) {
        let ty = crate::text::text_vcenter_y(20, 1, y + h * 0.5);
        p.text(t.as_ptr(), x + w * 0.5, ty, 20, theme::TEXT_HEADING, 1, 1);
    }
}

fn draw_about(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    let tx = MARGIN_X;
    let about_y = section_y(5);
    let hd = theme::TEXT_PRIMARY; // headings (brighter)
    let val = theme::TEXT_HEADING; // values (a step below headings)
    let lbl = theme::TEXT_TERTIARY; // dim labels
    let dim = theme::TEXT_TERTIARY;

    text_at(p, tx, about_y, 30, hd, 1, "About");

    // ---- selection highlight: the translucent rounded panel is a FOCUS indicator that
    // sits under whichever About block is selected (card / Information / Languages /
    // Accessibility) and moves with focus — NOT a fixed card background. ----
    let (cw, ch, cy, pad) = (640.0f32, 330.0f32, about_y + 50.0, 30.0f32);
    if view().section == 5 {
        let cy2 = about_y + 430.0 - 36.0; // column highlight top (col_y - pad)
        let hl = match view().col {
            0 => Rect::new(tx, cy, cw, ch),               // card
            1 => Rect::new(tx - 26.0, cy2, 600.0, 384.0), // Information
            2 => Rect::new(734.0, cy2, 560.0, 384.0),     // Languages
            _ => Rect::new(1334.0, cy2, 560.0, 384.0),    // Accessibility
        };
        p.rrect(hl, 18.0, 18.0, theme::OVERLAY_FOCUS_SOFT);
    }
    // ---- card: title, genres, summary + MORE ----
    let ix = tx + pad;
    text_at(p, ix, cy + pad, 30, hd, 1, &d.title);
    if !d.genres.is_empty() {
        text_at(p, ix, cy + pad + 44.0, 22, dim, 0, &d.genres.join(", "));
    }
    let sy = cy + pad + 100.0;
    TextView::new(&d.summary, 22, val)
        .leading(30.0)
        .max_lines(5)
        .trailing("MORE", hd) // only painted when the summary is actually cut off
        .draw(p, Rect::new(ix, sy, cw - 2.0 * pad, 0.0));

    // ---- three columns ----
    let col_y = about_y + 430.0;

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
        TextView::new(&list.join(", "), 22, val).leading(28.0).max_lines(6).draw(p, Rect::new(lx, ly + 30.0, 500.0, 0.0));
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
        let h = TextView::new(desc, 20, val).leading(26.0).max_lines(4).draw(p, Rect::new(ax, ay + 46.0, 500.0, 0.0));
        ay += 46.0 + h + 26.0;
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
