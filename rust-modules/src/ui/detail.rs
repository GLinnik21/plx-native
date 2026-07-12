//! Detail screen: a full-screen item page — the reused hero (no page dots) over the
//! item backdrop, with a vertical-scroll flow underneath for the episode/related/
//! cast rows and About footer (added in later increments). Reads the loaded item
//! from crate::metadata and the selected catalog row (backdrop art + blur) from the
//! browse catalog. Mirrors the home screen's C-shaped entry points (open/update/
//! draw/move_focus) driven by app.rs.
#![allow(dead_code)]
use crate::metadata;
use crate::pms::PmsMovie;
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::consts::*;
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::{cfield, resolve_tex, Art, Button, CircleButton};
use crate::ui::{hero_alpha, on_axis, Column, Env, Painter, Rect, ScrollColumn, Spring, View}; // View: Button/CircleButton::draw
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
    // the below-hero sections are a shared ScrollColumn: it owns the vertical scroll spring, the
    // section flow (child_top == the old section_y), and the off-screen band cull (via on_axis).
    column: ScrollColumn,
    card_scale: Spring, // focused card-row item pop (springs on selection change)
    ep_hscroll: Spring, // episode row horizontal scroll — glides instead of snapping
    tab_hscroll: Spring, // season-tab row horizontal scroll (many-season shows overflow the width)
    // season-tab load is DEBOUNCED: focusing a tab records the target here + resets the settle timer;
    // update() loads it only once focus has been stable (SEASON_SETTLE). Otherwise a held LEFT/RIGHT
    // through the tabs would fire a blocking PMS `/children` fetch every repeat tick.
    pending_season: c_int, // tab focused but not yet loaded (-1 = none / already loaded)
    season_settle: f32,    // seconds the pending season has been stable
    related: CardRow,   // the Related row = the SAME animated shelf component the home grid uses
    cast: CardRow,      // Cast & Crew row — the same component, circular RowStyle::CAST
    last_resume_ns: i64, // resume position (ns) on_ok just started (0 = from start); app.rs seeks here
}
impl DetailView {
    fn new() -> Self {
        Self {
            selected: -1,
            section: 0,
            col: 0,
            column: ScrollColumn::new(CONTENT_TOP, TOP_MARGIN),
            card_scale: Spring::at(1.0),
            ep_hscroll: Spring::at(0.0),
            tab_hscroll: Spring::at(0.0),
            pending_season: -1,
            season_settle: 0.0,
            related: CardRow::new(),
            cast: CardRow::new(),
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

// Below-the-hero content is ONE vertical scroll of stacked blocks, driven by the shared retui
// `ScrollColumn` (`impl Column for DetailView`): its `child_top` COMPUTES each block's pre-scroll top
// by stacking the *present* blocks' heights (`block_h`, via `Column::height`) from CONTENT_TOP with a
// single SECTION_GAP between them (season tabs → episodes hug with TAB_EP_GAP) — no hard-coded
// per-section Y to keep in sync. A block's height is content-derived (e.g. the Related block tracks
// REL_H → the shared home poster size), so resizing one block reflows everything below it. The
// container's scroll lifts the focused block's top to TOP_MARGIN under the compact title.
const TOP_MARGIN: f32 = 120.0;
const CONTENT_TOP: f32 = 920.0; // first block sits just under the hero buttons
const SECTION_GAP: f32 = 110.0; // vertical padding between top-level blocks
const TAB_EP_GAP: f32 = 21.0; // season tabs → their episode row (they read as one unit)
const SCROLLED: f32 = CONTENT_TOP - TOP_MARGIN; // backdrop-dim saturation reference (= 800)

// Season tabs (header for the episode row)
const TAB_ROW_H: f32 = 44.0;
const TAB_ADVANCE: f32 = 52.0; // per-tab horizontal advance past the label width
const TAB_LEAD: f32 = 200.0; // context kept left of the focused tab once the row scrolls
const SEASON_SETTLE: f32 = 0.2; // hold a season tab this long (s) before its episodes are fetched
// Episodes: landscape stills + under-card metadata
const EP_W: f32 = 420.0;
const EP_H: f32 = 236.0; // 16:9-ish still
const EP_GAP: f32 = 28.0;
// Under-still metadata: kicker → title → date → summary. The summary is LAST so it can be any length;
// offsets are from the meta top (EP_H + EP_META_TOP below the still) and the block height is derived
// from the tallest episode summary (see `ep_meta_h`) so the dynamic below-hero flow reflows to fit.
const EP_META_TOP: f32 = 30.0; // still bottom → kicker
const EP_TITLE_DY: f32 = 34.0; // kicker → title
const EP_TITLE_LEAD: f32 = 30.0; // title line pitch (title wraps to at most 2 lines)
const EP_TITLE_DATE_GAP: f32 = 10.0; // title bottom → date (flows off the ACTUAL title height)
const EP_DATE_H: f32 = 30.0; // date line height
const EP_DATE_SUMMARY_GAP: f32 = 10.0; // date → summary
const EP_SUMMARY_LEAD: f32 = 30.0;
const EP_SUMMARY_MAXLINES: usize = 8; // bounds the pathological case; ordinary episode blurbs fit fully
const EP_META_BOT_PAD: f32 = 24.0;
// Related row (portrait posters) reuses the home shelf poster geometry (consts::CARD_*) so a poster
// is one size app-wide (its texture request was already 250×375 → now drawn 1:1 sharp).
const REL_W: f32 = CARD_W; // 250
const REL_H: f32 = CARD_H; // 375
const REL_GAP: f32 = GAP; // 30
const REL_LABEL_H: f32 = 46.0; // "Related" heading → poster row
const REL_UNDER_H: f32 = 54.0; // poster row → tile title → block bottom
// Cast & Crew row (circular headshots)
const CAST_D: f32 = 190.0; // headshot diameter
const CAST_SLOT: f32 = 230.0; // per-member horizontal pitch (room for the name)
const CAST_LABEL_H: f32 = 60.0; // "Cast & Crew" heading → headshot row
const CAST_UNDER_H: f32 = 92.0; // headshot → name/role → block bottom (name LABEL + role CAPTION)

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
fn sections() -> ([c_int; 6], usize) {
    // fixed stack array (max ids 0..=5) + a live count — no per-frame heap alloc on the draw path.
    let mut v = [0 as c_int; 6];
    let mut n = 1; // hero (0) always present; v[0] is already 0
    if let Some(d) = metadata::current() {
        if d.is_show && !d.seasons.is_empty() {
            v[n] = 1;
            n += 1;
        }
        if d.is_show && !d.episodes.is_empty() {
            v[n] = 2;
            n += 1;
        }
        // Cast & Crew (credits) before Related — the flow order is the push order here (block_h /
        // draw_child / move_focus all key off the section id, not its position, so this is the one
        // place ordering lives).
        if !d.cast.is_empty() {
            v[n] = 4;
            n += 1;
        }
        if !d.related.is_empty() {
            v[n] = 3;
            n += 1;
        }
        v[n] = 5; // About footer — always present when an item is loaded
        n += 1;
    }
    (v, n)
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
        2 => EP_H + ep_meta_h(),
        3 => REL_LABEL_H + REL_H + REL_UNDER_H,
        4 => CAST_LABEL_H + CAST_D + CAST_UNDER_H,
        _ => 0.0,
    }
}
/// The under-still meta layout for ONE episode, measured from the meta top (`ty`): the date hugs the
/// ACTUAL title height (1 or 2 lines) and the summary hugs the date — nothing is reserved, so a
/// 1-line title doesn't leave a 2-line gap. Returns `(date_y, summary_y, total_h)`, all px below ty.
/// measure_h is wrap-cached, so this is cheap after the first frame.
fn ep_meta_layout(title: &str, aired: &str, summary: &str) -> (f32, f32, f32) {
    let title_h = TextView::new(title, theme::size::LABEL, theme::TEXT_PRIMARY)
        .bold()
        .leading(EP_TITLE_LEAD)
        .max_lines(2)
        .measure_h(EP_W);
    let date_y = EP_TITLE_DY + title_h + EP_TITLE_DATE_GAP;
    let has_date = !pretty_date(aired, 0).is_empty();
    let summary_y = if has_date { date_y + EP_DATE_H + EP_DATE_SUMMARY_GAP } else { date_y };
    let summary_h = if summary.is_empty() {
        0.0
    } else {
        TextView::new(summary, theme::size::CAPTION, theme::TEXT_TERTIARY)
            .leading(EP_SUMMARY_LEAD)
            .max_lines(EP_SUMMARY_MAXLINES)
            .measure_h(EP_W)
    };
    (date_y, summary_y, summary_y + summary_h)
}
/// dynamic under-still metadata height for the episode row: the TALLEST episode's flowed meta (title
/// 1–2 lines + date + full summary). Content-derived, so the below-hero flow reflows to fit.
fn ep_meta_h() -> f32 {
    let d = match metadata::current() {
        Some(d) => d,
        None => return EP_META_TOP + EP_TITLE_DY + EP_META_BOT_PAD,
    };
    let mut maxh = 0.0f32;
    for ep in &d.episodes {
        let (_, _, total) = ep_meta_layout(&ep.title, &ep.aired, &ep.summary);
        maxh = maxh.max(total);
    }
    EP_META_TOP + maxh + EP_META_BOT_PAD
}
/// scroll offset that lifts the focused section's top to TOP_MARGIN
fn scroll_target() -> f32 {
    let v = view();
    if v.section == 0 {
        return 0.0;
    }
    // episodes anchor on the season tabs above them, so the tabs stay visible while browsing episodes
    let anchor = if v.section == 2 { 1 } else { v.section };
    let (s, n) = sections();
    // the anchor's non-hero ScrollColumn index; lift its top to margin (== the old section_y math)
    match s[..n].iter().position(|&x| x == anchor) {
        Some(pos) if pos >= 1 => {
            let col = v.column;
            col.lift_target(v, pos - 1)
        }
        _ => 0.0,
    }
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
    v.pending_season = -1;
    v.column.scroll.jump(0.0);
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
        // About (5) is a 2D block: the card (col 0) has no horizontal neighbours; the three columns
        // (Information/Languages/Accessibility = cols 1..=3) move among themselves.
        if sec == 5 {
            if col >= 1 {
                let nc = if sym == SDLK_LEFT { (col - 1).max(1) } else { (col + 1).min(3) };
                if nc != col {
                    v.col = nc;
                    v.card_scale.jump(1.0);
                }
            }
            return;
        }
        let n = n_items(sec);
        if n <= 0 {
            return;
        }
        let nc = if sym == SDLK_LEFT { (col - 1).max(0) } else { (col + 1).min(n - 1) };
        if nc != col {
            v.col = nc;
            v.card_scale.jump(1.0); // re-pop the newly-focused card
            // focusing a season tab queues that season; update() fetches it once focus settles, so a
            // held LEFT/RIGHT scans the tabs without a blocking fetch on every repeat tick.
            if sec == 1 {
                v.pending_season = nc;
                v.season_settle = 0.0;
            }
        }
    } else if sym == SDLK_UP || sym == SDLK_DOWN {
        // About (5) is a 2D block: DOWN descends from the card (col 0) into the columns row (col 1);
        // UP climbs a column back to the card. Only UP *from the card* falls through to the generic
        // section change (leaving About upward). This makes DOWN — not RIGHT — enter the columns.
        if sec == 5 {
            if sym == SDLK_DOWN {
                if col == 0 {
                    v.col = 1;
                    v.card_scale.jump(1.0);
                }
                return; // columns row is the bottom of the last section
            } else if col >= 1 {
                v.col = 0;
                v.card_scale.jump(1.0);
                return;
            }
            // UP from the card → fall through to leave the section
        }
        let (secs, n) = sections();
        let avail = &secs[..n];
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

/// content-space left edge (px) of the focused season tab — sums the widths of the tabs before it,
/// mirroring draw_tabs' `x` advance so the scroll target and the draw can't drift.
fn tab_focus_left() -> f32 {
    let d = match metadata::current() {
        Some(d) => d,
        None => return MARGIN_X,
    };
    let col = view().col.max(0) as usize;
    let mut x = MARGIN_X;
    for (i, s) in d.seasons.iter().enumerate() {
        if i == col {
            break;
        }
        let label = if s.title.is_empty() { format!("Season {}", s.index) } else { s.title.clone() };
        if let Ok(lc) = CString::new(label) {
            x += crate::text::text_width(lc.as_ptr(), theme::size::BODY, 1) + TAB_ADVANCE;
        }
    }
    x
}
/// season-tab-row horizontal scroll target: keep the focused tab on-screen with ~one tab of context
/// to its left; 0 (glide back to the start) when the tab row isn't the focused section.
fn tab_hscroll_target() -> f32 {
    if view().section != 1 {
        return 0.0;
    }
    (tab_focus_left() - MARGIN_X - TAB_LEAD).max(0.0)
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
    let tst = tab_hscroll_target();
    let v = view();
    v.column.scroll.step(sct, K_SCROLL, dt);
    crate::ui::anim::probe("detail.scroll", v.column.scroll.pos, v.column.scroll.vel, sct, dt);
    v.card_scale.step(crate::ui::widgets::CARD_FOCUS_SCALE, 300.0, dt);
    crate::ui::anim::probe("detail.card", v.card_scale.pos, v.card_scale.vel, crate::ui::widgets::CARD_FOCUS_SCALE, dt);
    v.ep_hscroll.step(hst, 240.0, dt);
    crate::ui::anim::probe("detail.epscroll", v.ep_hscroll.pos, v.ep_hscroll.vel, hst, dt);
    v.tab_hscroll.step(tst, 240.0, dt);
    crate::ui::anim::probe("detail.tabscroll", v.tab_hscroll.pos, v.tab_hscroll.vel, tst, dt);
    // Related is the shared home-shelf component now: step its per-card scale springs + scroll spring
    // (focused only when the Related section holds focus, else the scales ease back and scroll freezes).
    let rfoc = (v.section == 3).then_some(v.col.max(0) as usize);
    v.related.update(n_items(3) as usize, rfoc, &RowStyle::HOME, dt);
    // Cast is the same shared shelf component (circular RowStyle::CAST): spring magnification + scroll.
    let cfoc = (v.section == 4).then_some(v.col.max(0) as usize);
    v.cast.update(n_items(4) as usize, cfoc, &RowStyle::CAST, dt);
    // debounced season fetch: load the queued season only after its tab has held focus for a beat, so
    // scanning tabs (a hold, or fast taps) coalesces into ONE blocking `/children` fetch, not one per step.
    if v.pending_season >= 0 {
        v.season_settle += dt;
        if v.season_settle >= SEASON_SETTLE {
            let ps = v.pending_season;
            v.pending_season = -1;
            let cur = metadata::current().map(|d| d.cur_season as c_int).unwrap_or(-1);
            if ps != cur {
                metadata::load_season(ps.max(0) as usize);
            }
        }
    }
}

fn env_of(dt: f32) -> Env {
    Env { dt, screen: Rect::FULL, fr: focus(), fc: 0, sp: 1.0, hero_a: 1.0 }
}

pub(crate) fn draw() {
    let p = Painter::root();
    let env = env_of(0.0);
    let m = selected();
    let scroll = view().column.scroll.pos;
    use crate::ui::profile::phase;
    phase("dt.backdrop", || draw_backdrop(p, m, scroll));
    let hero_a = hero_alpha(scroll, 400.0);
    let ps = p.translate(0.0, -scroll);
    // hero fades out as the page scrolls down into the rows (pinned but scrolled)
    if hero_a > 0.01 {
        phase("dt.hero", || draw_hero(ps.alpha(hero_a), &env, m));
    }
    // compact centered title fades in at the top of the scrolled view (pinned, not scrolled)
    if hero_a < 0.99 {
        phase("dt.ctitle", || draw_compact_title(p.alpha(1.0 - hero_a), m));
    }
    // Below-hero sections through the shared ScrollColumn: it owns the flow (child_top == the old
    // section_y), the scroll, and the off-screen cull (on_axis) — Painter has no clip/scissor, so a
    // section fully below the fold (~35ms/frame of wasted strings+draws) is SKIPPED, not clipped. Each
    // present section draws local-coord under a painter pre-translated to its child origin (see the
    // `impl Column for DetailView` dispatch). The focused section is never culled.
    let col = view().column;
    col.draw(view(), &env, p);
}

// The below-hero sections ARE the ScrollColumn's children (the hero at section id 0 is pinned, drawn
// separately, and excluded here). Column index i maps to the (i+1)-th present section id, so child 0
// is the first non-hero section sitting at CONTENT_TOP.
impl Column for DetailView {
    fn len(&self) -> usize {
        let (_, n) = sections();
        n - 1 // exclude the pinned hero (section id 0 at index 0)
    }
    fn height(&self, i: usize) -> f32 {
        let (s, _) = sections();
        block_h(s[i + 1])
    }
    fn gap_before(&self, i: usize) -> f32 {
        // gap between the previous child (section s[i]) and this one (s[i+1]); tabs→episodes hug
        let (s, _) = sections();
        if s[i] == 1 && s[i + 1] == 2 {
            TAB_EP_GAP
        } else {
            SECTION_GAP
        }
    }
    fn focus_child(&self) -> Option<usize> {
        if self.section == 0 {
            return None; // hero focused → no scrolling child is focused
        }
        let (s, n) = sections();
        s[..n].iter().position(|&x| x == self.section).map(|p| p - 1)
    }
    fn draw_child(&self, i: usize, _env: &Env, p: Painter) {
        use crate::ui::profile::phase;
        let (s, _) = sections();
        match s[i + 1] {
            1 => phase("dt.tabs", || draw_tabs(p)),
            2 => phase("dt.eps", || draw_episodes(p)),
            3 => phase("dt.related", || draw_related(p)),
            4 => phase("dt.cast", || draw_cast(p)),
            5 => phase("dt.about", || draw_about(p)),
            _ => {}
        }
    }
}

fn draw_backdrop(p: Painter, m: Option<&PmsMovie>, scroll: f32) {
    // 0 at the hero, 1 when scrolled down into the rows
    let sf = (scroll / SCROLLED).clamp(0.0, 1.0);
    // Overall scroll-darkening for row-text legibility. Folded into the wash + art tints below
    // instead of a separate full-screen dim pass — one fewer full-screen blit per scrolled frame.
    let d = 1.0 - sf * 0.55;
    // Backdrop art: prefer the catalog row's art, else the loaded detail's art. Fades out as the page
    // scrolls into the rows so the episode/row text reads over a dark bg. Resolved FIRST (the texture
    // is cached — this is a cheap lookup) so the ambient wash below can be skipped whenever the opaque
    // full-screen art already covers it — drawing both is a wasted full-screen pass on the weak GPU
    // (the hero's fill-rate cost; mirrors home's Backdrop which draws art OR ambient, never both).
    let art_a = 1.0 - sf;
    let art_tex = if art_a > 0.01 {
        m.filter(|m| m.art[0] != 0)
            .map(|m| cfield(&m.art))
            .or_else(|| metadata::current().map(|d| d.art.clone()).filter(|s| !s.is_empty()))
            .and_then(|art| CString::new(art).ok())
            .map(|ap| resolve_tex(ap.as_ptr(), 1920, 1080, 0))
            .unwrap_or(0)
    } else {
        0
    };
    // ambient wash from the item's UltraBlur corners, dimmed by the scroll (`d`), drawn ONLY when it
    // will actually show: no art texture yet, or the art has faded on scroll enough to reveal it.
    // Because black-over-at-alpha is a linear multiply, dimming each source layer by `d` is
    // pixel-identical to the old separate full-screen dim overlay, one fewer full-screen pass.
    if let Some(m) = m {
        if m.has_blur != 0 && (art_tex == 0 || art_a < 0.99) {
            p.ambient(Rect::FULL, 0.55 * d, m.blur);
        }
    }
    if art_tex != 0 {
        p.tex(art_tex, Rect::FULL, 0.0, theme::with_a(theme::dim(theme::TINT_WHITE, d), art_a)); // white dimmed by scroll `d`
    }
    // bottom scrim for the hero's lower-left content — tied to the hero's own fade (it serves that
    // text), so it clears once the hero is gone rather than lingering the whole scroll (a wasted
    // full-screen-width pass through the back half of the transition).
    let hero_vis = hero_alpha(scroll, 400.0);
    if hero_vis > 0.01 {
        p.rect(
            Rect::new(0.0, SCR_H * 0.34, SCR_W, SCR_H * 0.66),
            0.0,
            theme::scrim(0.0),
            theme::scrim(0.95 * hero_vis),
            0.0,
        );
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
            p.text(t.as_ptr(), tx, title_bottom - 68.0, theme::size::HERO, w_a, 0, 1);
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
        p.text(mc.as_ptr(), tx, meta_y, theme::size::BODY, d_a, 0, 0);
    }

    // ---- synopsis: pixel-wrapped to the hero text column, up to 4 lines; the date/buttons below
    // flow off its MEASURED height so a long synopsis pushes them down instead of overlapping ----
    let summary = d.map(|d| d.summary.clone()).or_else(|| m.map(|m| cfield(&m.summary))).unwrap_or_default();
    let syn_y = meta_y + 46.0;
    let syn_h = if !summary.is_empty() {
        TextView::new(&summary, theme::size::BODY, d_a)
            .leading(34.0)
            .max_lines(4)
            .draw(p, Rect::new(tx, syn_y, 900.0, 0.0))
    } else {
        0.0
    };

    // ---- date · runtime ----
    let date_y = syn_y + syn_h.max(34.0) + 20.0;
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
            p.text(ic.as_ptr(), tx, date_y, theme::size::CAPTION, dim, 0, 0);
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
                let w = crate::text::text_width(sc.as_ptr(), theme::size::LABEL, 1);
                p.text(sc.as_ptr(), SCR_W - MARGIN_X - w, btn_y + 16.0, theme::size::LABEL, d_a, 0, 1);
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

    Button::new(c"Play".as_ptr(), theme::size::BODY, Rect::new(tx, y, PW, CD))
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
        p.text(t.as_ptr(), cx, 54.0, theme::size::TITLE, theme::TEXT_PRIMARY, 1, 1);
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
    let tab_y = 0.0; // local origin — the ScrollColumn pre-translates the painter to this section's top
    // many-season shows (Top Gear) overflow the row width, so the whole strip scrolls horizontally to
    // keep the focused tab visible (spring target in `tab_hscroll_target`); off-screen tabs are culled.
    let sx = view().tab_hscroll.pos;
    let pt = p.translate(-sx, 0.0);
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
        // advance by label width + TAB_ADVANCE.
        let w = crate::text::text_width(lc.as_ptr(), theme::size::BODY, 1);
        if on_axis(x - 18.0 - sx, w + 36.0, SCR_W, 0.0) {
            crate::ui::widgets::TabPill::new(lc.as_ptr(), theme::size::BODY, Rect::new(x - 18.0, tab_y - 8.0, w + 36.0, 50.0))
                .segment(selected)
                .focused(focused)
                .draw(&e, pt);
        }
        x += w + TAB_ADVANCE;
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
    let ep_y = 0.0; // local origin (ScrollColumn pre-translates to this section's top)
    for (i, ep) in d.episodes.iter().enumerate() {
        let x = MARGIN_X + i as f32 * (EP_W + EP_GAP);
        if !on_axis(x - sx, EP_W, SCR_W, 0.0) {
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
        // under-card metadata: kicker → title → date → full summary. The date + summary flow off the
        // ACTUAL title height (1 or 2 lines) — nothing is reserved, so a 1-line title doesn't leave a
        // gap before the date. The block height tracks the tallest episode via ep_meta_h.
        let ty = ep_y + EP_H + EP_META_TOP;
        let titc = if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
        let (date_y, summary_y, _) = ep_meta_layout(&ep.title, &ep.aired, &ep.summary);
        if let Ok(ec) = CString::new(format!("EPISODE {}", ep.index)) {
            pe.text(ec.as_ptr(), x, ty, theme::size::CAPTION, dimc, 0, 1);
        }
        // title wraps to at most 2 lines (elided past that) — long titles used to run into the next card
        TextView::new(&ep.title, theme::size::LABEL, titc)
            .bold()
            .leading(EP_TITLE_LEAD)
            .max_lines(2)
            .draw(pe, Rect::new(x, ty + EP_TITLE_DY, EP_W, 0.0));
        let date = pretty_date(&ep.aired, 0);
        if !date.is_empty() {
            if let Ok(dc) = CString::new(date) {
                pe.text(dc.as_ptr(), x, ty + date_y, theme::size::CAPTION, dimc, 0, 0);
            }
        }
        if !ep.summary.is_empty() {
            TextView::new(&ep.summary, theme::size::CAPTION, dimc)
                .leading(EP_SUMMARY_LEAD)
                .max_lines(EP_SUMMARY_MAXLINES)
                .draw(pe, Rect::new(x, ty + summary_y, EP_W, 0.0));
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
    let related_y = 0.0; // local origin (ScrollColumn pre-translates to this section's top)
    p.text(c"Related".as_ptr(), MARGIN_X, related_y, theme::size::HEADLINE, theme::TEXT_HEADING, 0, 1);
    let focus_col = if view().section == 3 { view().col } else { -1 };
    let row_y = related_y + REL_LABEL_H;
    // The Related row is a real instance of the home shelf: view().related owns the animated per-card
    // scale springs + the animated scroll spring (RowStyle::HOME), so it pops + glides + rings + titles
    // exactly like a home shelf. A lone row has no cross-row neighbour, so the focused-last pass is just
    // in-row: non-focused posters first, then the focused one (over its neighbours) last.
    let sx = view().related.scroll_x();
    let pr = p.translate(-sx, 0.0);
    for (i, r) in d.related.iter().enumerate() {
        if i as c_int == focus_col {
            continue; // focused poster drawn last
        }
        let x = MARGIN_X + i as f32 * (REL_W + REL_GAP);
        if !on_axis(x - sx, REL_W, SCR_W, 0.0) {
            continue;
        }
        let s = view().related.scale(i);
        let rect = Rect::new(x, row_y, REL_W, REL_H).scaled(s);
        card_row::draw_tile(pr, Art::Thumb { key: &r.thumb, res: (250, 375) }, rect, s, &RowStyle::HOME, None);
    }
    if focus_col >= 0 {
        if let Some(r) = d.related.get(focus_col as usize) {
            let x = MARGIN_X + focus_col as f32 * (REL_W + REL_GAP);
            let s = view().related.scale(focus_col as usize);
            let rect = Rect::new(x, row_y, REL_W, REL_H).scaled(s);
            let tc = CString::new(r.title.clone()).ok(); // kept alive across the draw call
            let title = tc.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
            card_row::draw_focused(pr, Art::Thumb { key: &r.thumb, res: (250, 375) }, rect, s, &RowStyle::HOME, None, title);
        }
    }
}

/// "Cast & Crew" — circular headshots with names, on the shared [`CardRow`] (circular
/// `RowStyle::CAST`): spring focus-magnification + animated scroll + glow ring, exactly like the
/// poster/Related shelves. The caller draws the per-member name/role labels (a poster row shows a
/// title only on focus; cast shows every name), and the focused headshot draws LAST for its z-order.
fn draw_cast(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    if d.cast.is_empty() {
        return;
    }
    let cast_y = 0.0; // local origin (ScrollColumn pre-translates to this section's top)
    p.text(c"Cast & Crew".as_ptr(), MARGIN_X, cast_y, theme::size::HEADLINE, theme::TEXT_HEADING, 0, 1);
    let focus_col = if view().section == 4 { view().col } else { -1 };
    let row_y = cast_y + CAST_LABEL_H;
    let sx = view().cast.scroll_x();
    let pc = p.translate(-sx, 0.0);
    let sty = &RowStyle::CAST;
    for (i, c) in d.cast.iter().enumerate() {
        if i as c_int == focus_col {
            continue; // focused headshot drawn last
        }
        let x = MARGIN_X + i as f32 * CAST_SLOT;
        if !on_axis(x - sx, CAST_D, SCR_W + CAST_D, 0.0) {
            continue;
        }
        let s = view().cast.scale(i);
        let rect = Rect::new(x, row_y, CAST_D, CAST_D).scaled(s);
        card_row::draw_tile(pc, Art::Thumb { key: &c.thumb, res: (300, 300) }, rect, s, sty, None);
        cast_label(pc, &c.tag, &c.role, x + CAST_D * 0.5, row_y, false);
    }
    if focus_col >= 0 {
        if let Some(c) = d.cast.get(focus_col as usize) {
            let x = MARGIN_X + focus_col as f32 * CAST_SLOT;
            let s = view().cast.scale(focus_col as usize);
            let rect = Rect::new(x, row_y, CAST_D, CAST_D).scaled(s);
            card_row::draw_focused(pc, Art::Thumb { key: &c.thumb, res: (300, 300) }, rect, s, sty, None, std::ptr::null());
            cast_label(pc, &c.tag, &c.role, x + CAST_D * 0.5, row_y, true);
        }
    }
}

/// A cast member's name + role, centred under the headshot and elided to the per-member slot (long
/// names like "Benedict Cumberbatch" would otherwise run into the neighbour at couch-legible sizes).
fn cast_label(p: Painter, name: &str, role: &str, cx: f32, row_y: f32, focused: bool) {
    let name_c = if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
    let budget = CAST_SLOT - 12.0;
    if let Ok(nc) = CString::new(crate::text::elide(name, budget, theme::size::LABEL, 1, false)) {
        p.text(nc.as_ptr(), cx, row_y + CAST_D + 26.0, theme::size::LABEL, name_c, 1, if focused { 1 } else { 0 });
    }
    if !role.is_empty() {
        if let Ok(rc) = CString::new(crate::text::elide(role, budget, theme::size::CAPTION, 1, false)) {
            p.text(rc.as_ptr(), cx, row_y + CAST_D + 58.0, theme::size::CAPTION, theme::TEXT_TERTIARY, 1, 0);
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
            view().pending_season = -1; // explicit selection → cancel the debounced load
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
    v.pending_season = -1;
    v.column.scroll.jump(0.0);
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
    text_at(p, x, y, theme::size::CAPTION, lbl, 0, label);
    let h = TextView::new(value, theme::size::LABEL, val).bold().leading(30.0).max_lines(2).draw(p, Rect::new(x, y + 34.0, 520.0, 0.0));
    34.0 + h.max(30.0) + 22.0
}

/// the vertical advance [`draw_pair`] will consume for `value` — same formula, measured not painted,
/// so a column's focus highlight can be sized to wrap its content before the rows are drawn.
fn pair_h(value: &str) -> f32 {
    let h = TextView::new(value, theme::size::LABEL, theme::TEXT_HEADING).bold().leading(30.0).max_lines(2).measure_h(520.0);
    34.0 + h.max(30.0) + 22.0
}

/// a small rounded accessibility badge (CC / SDH / AD)
fn draw_badge(p: Painter, x: f32, y: f32, label: &str) {
    let (w, h) = (56.0f32, 34.0f32);
    p.rrect(Rect::new(x, y, w, h), 7.0, 7.0, [0.86, 0.88, 0.92, 0.20]);
    if let Ok(t) = CString::new(label) {
        let ty = crate::text::text_vcenter_y(theme::size::CAPTION, 1, y + h * 0.5);
        p.text(t.as_ptr(), x + w * 0.5, ty, theme::size::CAPTION, theme::TEXT_HEADING, 1, 1);
    }
}

fn draw_about(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    let tx = MARGIN_X;
    let about_y = 0.0; // local origin (ScrollColumn pre-translates to this section's top)
    let hd = theme::TEXT_PRIMARY; // headings (brighter)
    let val = theme::TEXT_HEADING; // values (a step below headings)
    let lbl = theme::TEXT_TERTIARY; // dim labels
    let dim = theme::TEXT_TERTIARY;

    text_at(p, tx, about_y, theme::size::HEADLINE, hd, 1, "About");

    // card + column geometry
    let (cw, ch, cy, pad) = (640.0f32, 330.0f32, about_y + 50.0, 30.0f32);
    let col_y = about_y + 430.0;
    let lx = 760.0f32; // Languages column x
    let ax = 1360.0f32; // Accessibility column x

    // ---- Build each column's rows ONCE. They drive BOTH the measured focus highlight (so it wraps
    // the content — a fixed-height panel used to let a long audio list spill past the selection) and
    // the paint below. ----
    // Information: (label, value) pairs
    let mut info: Vec<(&str, String)> = Vec::new();
    let released = pretty_date(&d.aired, d.year);
    if !released.is_empty() {
        info.push(("Released", released));
    }
    let dur = if d.dur_ms > 0 { d.dur_ms } else { d.episodes.first().map(|e| e.dur_ms).unwrap_or(0) };
    if dur > 0 {
        let mins = dur / 60_000;
        info.push(("Run Time", format!("{} hr {} min", mins / 60, mins % 60)));
    }
    info.push(("Rated", if d.rating.is_empty() { "NR".to_string() } else { d.rating.clone() }));
    if !d.countries.is_empty() {
        info.push(("Regions of Origin", d.countries.join(", ")));
    }
    // Languages: an "Original Audio" pair + a wrapped "Audio" list
    let orig_audio = d.audio.first().map(|a| if a.lang.is_empty() { "Unknown".to_string() } else { a.lang.clone() });
    let audio_list = d
        .audio
        .iter()
        .take(8)
        .map(|a| {
            let lang = if a.lang.is_empty() { "Unknown".to_string() } else { a.lang.clone() };
            format!("{} ({})", lang, a.codec.to_uppercase())
        })
        .collect::<Vec<_>>()
        .join(", ");
    // Accessibility: the present capability descriptions
    let cc = !d.subs.is_empty();
    let sdh = d.subs.iter().any(|s| s.sdh);
    let ad = d.audio.iter().any(|a| a.ad);
    let acc_all: [(bool, &str, &str); 3] = [
        (cc, "CC", "Closed captions refer to subtitles in available languages with the addition of relevant non-dialogue information."),
        (sdh, "SDH", "Subtitles for the deaf and hard of hearing (SDH) refer to subtitles in the original language with the addition of relevant non-dialogue information."),
        (ad, "AD", "Audio descriptions (AD) refer to a narration track describing what is happening on screen, to provide context for those who are blind or have low vision."),
    ];
    let access: Vec<(&str, &str)> = acc_all.iter().filter(|(pr, _, _)| *pr).map(|(_, l, ds)| (*l, *ds)).collect();

    // ---- measured content extent (px below col_y) of each column, mirroring the paint advances ----
    let info_off = 68.0 + info.iter().map(|(_, v)| pair_h(v)).sum::<f32>();
    let lang_off = if orig_audio.is_none() {
        30.0
    } else {
        let list_h = TextView::new(&audio_list, theme::size::LABEL, val).leading(32.0).max_lines(6).measure_h(500.0);
        68.0 + orig_audio.as_ref().map(|o| pair_h(o)).unwrap_or(0.0) + 34.0 + list_h
    };
    let access_off = if access.is_empty() {
        102.0
    } else {
        64.0 + access
            .iter()
            .map(|(_, ds)| 52.0 + TextView::new(ds, theme::size::CAPTION, val).leading(30.0).max_lines(4).measure_h(500.0) + 26.0)
            .sum::<f32>()
    };

    // ---- focus highlight: a translucent rounded panel under the SELECTED block that WRAPS its
    // content — the card is a fixed box; the three columns size to their measured height. ----
    if view().section == 5 {
        const HL_TOP: f32 = 36.0; // pad above the column heading
        const HL_BOT: f32 = 24.0; // pad below the last content line
        let hl = match view().col {
            0 => Rect::new(tx, cy, cw, ch),
            1 => Rect::new(tx - 26.0, col_y - HL_TOP, 600.0, info_off + HL_TOP + HL_BOT),
            2 => Rect::new(lx - 26.0, col_y - HL_TOP, 560.0, lang_off + HL_TOP + HL_BOT),
            _ => Rect::new(ax - 26.0, col_y - HL_TOP, 560.0, access_off + HL_TOP + HL_BOT),
        };
        p.rrect(hl, 18.0, 18.0, theme::OVERLAY_FOCUS_SOFT);
    }

    // ---- card: title, genres, summary; MORE parked in the bottom-right corner ----
    let ix = tx + pad;
    text_at(p, ix, cy + pad, theme::size::HEADLINE, hd, 1, &d.title);
    if !d.genres.is_empty() {
        text_at(p, ix, cy + pad + 44.0, theme::size::CAPTION, dim, 0, &d.genres.join(", "));
    }
    let sy = cy + pad + 100.0;
    let syn_w = cw - 2.0 * pad;
    // CAPTION (not BODY): in the dense About card the body-size synopsis read oversized next to the
    // compact columns — match it to the Accessibility descriptions below (same CAPTION rung).
    let syn = TextView::new(&d.summary, theme::size::CAPTION, val).leading(30.0).max_lines(5);
    syn.draw(p, Rect::new(ix, sy, syn_w, 0.0));
    // "MORE" affordance in the card's bottom-right corner (out of flow) when the blurb is cut off — an
    // inline trailing run used to spill past the card edge.
    if syn.truncates(syn_w) {
        let mw = crate::text::text_width(c"MORE".as_ptr(), theme::size::CAPTION, 1);
        let my = crate::text::text_vcenter_y(theme::size::CAPTION, 1, cy + ch - pad - 2.0);
        p.text(c"MORE".as_ptr(), tx + cw - pad - mw, my, theme::size::CAPTION, hd, 0, 1);
    }

    // ---- Information ----
    text_at(p, tx, col_y, theme::size::HEADLINE, hd, 1, "Information");
    let mut yy = col_y + 68.0;
    for (label, value) in &info {
        yy += draw_pair(p, tx, yy, label, value, lbl, val);
    }

    // ---- Languages ----
    text_at(p, lx, col_y, theme::size::HEADLINE, hd, 1, "Languages");
    let mut ly = col_y + 68.0;
    if let Some(orig) = &orig_audio {
        ly += draw_pair(p, lx, ly, "Original Audio", orig, lbl, val);
    }
    if !audio_list.is_empty() {
        text_at(p, lx, ly, theme::size::CAPTION, lbl, 0, "Audio");
        TextView::new(&audio_list, theme::size::LABEL, val).leading(32.0).max_lines(6).draw(p, Rect::new(lx, ly + 34.0, 500.0, 0.0));
    }

    // ---- Accessibility ----
    text_at(p, ax, col_y, theme::size::HEADLINE, hd, 1, "Accessibility");
    if access.is_empty() {
        text_at(p, ax, col_y + 68.0, theme::size::CAPTION, dim, 0, "\u{2014}");
    } else {
        let mut ay = col_y + 64.0;
        for (label, desc) in &access {
            draw_badge(p, ax, ay, label);
            let h = TextView::new(desc, theme::size::CAPTION, val).leading(30.0).max_lines(4).draw(p, Rect::new(ax, ay + 52.0, 500.0, 0.0));
            ay += 52.0 + h + 26.0;
        }
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
