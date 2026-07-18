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
use crate::ui::widgets::{resolve_tex, Art, Button, CircleButton, ControlStyle};
use crate::ui::{hero_alpha, on_axis, Column, Env, Painter, Rect, ScrollColumn, Spring, View}; // View: Button/CircleButton::draw
use std::ffi::CString;
use std::os::raw::c_int;
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
    // per-episode focus-pop springs — like a CardRow's per-cell scale springs, so a deselected episode
    // still animates its pop (and thus its lifted shadow) all the way back down instead of snapping.
    // Fixed cap; episodes past it (very long seasons) simply don't animate the pop.
    ep_scale: [Spring; EP_ANIM_MAX],
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
    // per-section focus memory (episodes/related/cast): leaving a row and coming back restores the
    // item you were on — paired with the frozen h-scrolls, a row "stays where it was" instead of
    // snapping to its start whenever focus moves elsewhere. Indexed by section id.
    saved_col: [c_int; 6],
    spin_ms: f32, // spinner phase for the episode row's season-loading state
}
impl DetailView {
    fn new() -> Self {
        Self {
            selected: -1,
            section: 0,
            col: 0,
            column: ScrollColumn::new(CONTENT_TOP, TOP_MARGIN), // top re-derived per frame (update)
            card_scale: Spring::at(1.0),
            ep_scale: [Spring::at(1.0); EP_ANIM_MAX],
            ep_hscroll: Spring::at(0.0),
            tab_hscroll: Spring::at(0.0),
            pending_season: -1,
            season_settle: 0.0,
            related: CardRow::new(),
            cast: CardRow::new(),
            last_resume_ns: 0,
            saved_col: [0; 6],
            spin_ms: 0.0,
        }
    }
}
static mut VIEW: Option<DetailView> = None;
fn view() -> &'static mut DetailView {
    unsafe { (*addr_of_mut!(VIEW)).get_or_insert_with(DetailView::new) }
}

const NBTN: c_int = 2; // Play + watched toggle (an info disc would be a no-op on its own page)
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
const CONTENT_TOP: f32 = 920.0; // nominal flow top; update() re-derives it from the hero buttons
const SECTION_GAP: f32 = theme::space::XL; // vertical padding between top-level blocks (64)
const TAB_EP_GAP: f32 = theme::space::MD; // season tabs → their episode row (they read as one unit)
const SCROLLED: f32 = CONTENT_TOP - TOP_MARGIN; // backdrop-dim saturation reference (= 800)
const BTN_CONTENT_GAP: f32 = theme::space::XL; // hero button row → first below-hero block (64)

// Season tabs (header for the episode row)
const TAB_ROW_H: f32 = CD; // tab pills stand as tall as the hero buttons (one control height)
const TAB_ADVANCE: f32 = 52.0; // per-tab horizontal advance past the label width
const SEASON_SETTLE: f32 = 0.2; // hold a season tab this long (s) before its episodes are fetched
// Episodes: landscape stills + under-card metadata
const EP_ANIM_MAX: usize = 40; // per-episode pop-spring count (episodes past this don't animate the pop)
const EP_W: f32 = 420.0;
const EP_H: f32 = 236.0; // 16:9-ish still
const EP_GAP: f32 = 28.0;
// Under-still metadata: kicker → title → summary → date. The (fine-print) air date sits at the
// BOTTOM of the block, under the summary; offsets are from the meta top (EP_H + EP_META_TOP below
// the still) and the block height is derived from the tallest episode summary (see `ep_meta_h`)
// so the dynamic below-hero flow reflows to fit.
const EP_META_TOP: f32 = 30.0; // still bottom → kicker
const EP_TITLE_DY: f32 = 42.0; // kicker → title
const EP_TITLE_LEAD: f32 = 30.0; // title line pitch (title wraps to at most 2 lines)
const EP_TITLE_SUMMARY_GAP: f32 = 18.0; // title last-line BASELINE → summary cap top (cap-band derived)
const EP_SUMMARY_DATE_GAP: f32 = 16.0; // summary last-line baseline → date cap top
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
    if idx < 0 {
        return None;
    }
    crate::pms::movie(idx as usize)
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
/// The under-still meta layout for ONE episode, measured from the meta top (`ty`): the summary hugs
/// the ACTUAL title height (1 or 2 lines) and the (fine-print, MICRO) air date hangs at the block's
/// BOTTOM under the summary — nothing is reserved, so a 1-line title doesn't leave a 2-line gap.
/// Returns `(date_y, summary_y, total_h)`, all px below ty. Cap-band-derived pacing: each gap is
/// measured last-line-INK (baseline) → next block's cap top, not leading-box to raw texture — the
/// leading slack used to make the gaps read visibly unequal. (`date_y` stays a raw Painter::text
/// draw-y; the cap offsets translate ink-space gaps into it.) measure_h is wrap-cached, so this is
/// cheap after the first frame.
fn ep_meta_layout(title: &str, has_date: bool, summary: &str) -> (f32, f32, f32) {
    let title_h = TextView::new(title, theme::size::LABEL, theme::TEXT_PRIMARY)
        .bold()
        .leading(EP_TITLE_LEAD)
        .max_lines(2)
        .measure_h(EP_W);
    let (lt, lb) = crate::text::text_cap_band(theme::size::LABEL, 1); // title cap-top/baseline offsets
    let title_base = EP_TITLE_DY + (title_h - EP_TITLE_LEAD) + (lb - lt); // last-line baseline below ty
    // summary: a TextView anchors its cap band at the draw-y, so the ink gap needs no correction
    let summary_y = title_base + EP_TITLE_SUMMARY_GAP;
    let (st, sb) = crate::text::text_cap_band(theme::size::CAPTION, 0); // summary line offsets
    let summary_h = if summary.is_empty() {
        0.0
    } else {
        TextView::new(summary, theme::size::CAPTION, theme::TEXT_TERTIARY)
            .leading(EP_SUMMARY_LEAD)
            .max_lines(EP_SUMMARY_MAXLINES)
            .measure_h(EP_W)
    };
    // last ink baseline above the date: the summary's last line, or the title's when there is none
    let base = if summary.is_empty() { title_base } else { summary_y + (summary_h - EP_SUMMARY_LEAD) + (sb - st) };
    let (mt, mb) = crate::text::text_cap_band(theme::size::MICRO, 0); // date offsets
    let date_y = base + EP_SUMMARY_DATE_GAP - mt; // raw Painter::text draw-y for the MICRO date line
    let total = if has_date { date_y + mb } else { base };
    (date_y, summary_y, total)
}
/// dynamic under-still metadata height for the episode row: the TALLEST episode's flowed meta (title
/// 1–2 lines + date + full summary). Content-derived, so the below-hero flow reflows to fit.
/// CACHED per episode set — the O(episodes) wrap-measure sweep only changes on a detail/season
/// load, but `Column::height` re-asks 2-4× per frame (scroll target + per-child draw).
fn ep_meta_h() -> f32 {
    let d = match metadata::current() {
        Some(d) => d,
        None => return EP_META_TOP + EP_TITLE_DY + EP_META_BOT_PAD,
    };
    // fingerprint the episode set (item + season + count + first leaf); recompute only when it
    // changes. The first episode's rk matters now that season loads are async: cur_season flips
    // before the episodes swap, and two seasons can share a length.
    static mut CACHE: (u64, f32) = (0, 0.0);
    let fp = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        d.rk.hash(&mut h);
        d.cur_season.hash(&mut h);
        d.episodes.len().hash(&mut h);
        d.episodes.first().map(|e| e.rk.as_str()).unwrap_or("").hash(&mut h);
        h.finish().max(1) // 0 is the "empty" sentinel
    };
    unsafe {
        if (*addr_of_mut!(CACHE)).0 == fp {
            return (*addr_of_mut!(CACHE)).1;
        }
    }
    let mut maxh = 0.0f32;
    for ep in &d.episodes {
        let has_date = !pretty_date(&ep.aired, 0).is_empty();
        let (_, _, total) = ep_meta_layout(&ep.title, has_date, &ep.summary);
        maxh = maxh.max(total);
    }
    let h = EP_META_TOP + maxh + EP_META_BOT_PAD;
    unsafe { *addr_of_mut!(CACHE) = (fp, h) }
    h
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
/// is the loaded item a TV show?
pub(crate) fn is_show() -> bool {
    metadata::current().map(|d| d.is_show).unwrap_or(false)
}

/// Open the detail page for catalog row `idx`: load its full detail (blocking) and
/// reset focus/scroll.
/// Reset focus + all scroll springs to the top of a freshly-opened page — shared by open/open_rk
/// so the two entry points can't drift (the retained-scroll fields must ALL be zeroed on a new item).
fn reset_view_state(v: &mut DetailView) {
    v.section = 0;
    v.col = 0;
    v.pending_season = -1;
    v.saved_col = [0; 6];
    v.column.scroll.jump(0.0);
    v.ep_hscroll.jump(0.0);
    v.tab_hscroll.jump(0.0);
}

pub(crate) fn open(idx: c_int) {
    let v = view();
    v.selected = idx;
    reset_view_state(v);
    if idx >= 0 {
        if let Some(m) = crate::pms::movie(idx as usize) {
            if !m.rk.is_empty() {
                metadata::load_detail(&m.rk);
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
            // remember where this row was left — only the horizontally-scrolling strips
            // (episodes/related/cast) are ever restored; hero/tabs/About slots stay unused
            if matches!(sec, 2 | 3 | 4) {
                v.saved_col[sec as usize] = col;
            }
            v.section = ns;
            v.card_scale.jump(1.0); // pop the card in the newly-entered row
            // land on the active season when entering the tabs; restore the remembered item for
            // the episode/related/cast rows (their h-scroll held, so the row truly stays put);
            // else the first item
            let start = match ns {
                1 => metadata::current().map(|d| d.cur_season as c_int).unwrap_or(0),
                2 | 3 | 4 => v.saved_col[ns as usize].clamp(0, (n_items(ns) - 1).max(0)),
                _ => 0,
            };
            v.col = start;
        }
    }
}

/// ONE source of truth for the season-tab strip's layout: per tab, its index, content-space
/// label x, label width, and the label CString — the draw pass and the focus/scroll geometry
/// both walk this, so their x-advance can't drift. A label that can't be a CString (interior
/// NUL — never in practice) is skipped entirely (not drawn, no advance).
fn tabs_layout(d: &crate::metadata::Detail) -> Vec<(usize, f32, f32, CString)> {
    let mut x = MARGIN_X;
    let mut out = Vec::with_capacity(d.seasons.len());
    for (i, s) in d.seasons.iter().enumerate() {
        let label = if s.title.is_empty() { format!("Season {}", s.index) } else { s.title.clone() };
        if let Ok(lc) = CString::new(label) {
            let w = crate::text::text_width(lc.as_ptr(), theme::size::BODY, 1);
            out.push((i, x, w, lc));
            x += w + TAB_ADVANCE;
        }
    }
    out
}

/// content-space geometry (label x, label width) of the focused season tab.
fn tab_focus_geom() -> (f32, f32) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return (MARGIN_X, 0.0),
    };
    let col = view().col.max(0) as usize;
    let mut x_end = MARGIN_X;
    for (i, x, w, _lc) in tabs_layout(d) {
        if i == col {
            return (x, w);
        }
        x_end = x + w + TAB_ADVANCE;
    }
    (x_end, 0.0)
}
/// season-tab-row horizontal scroll target: minimal scroll-into-view (the CardRow rule) — move
/// only when the focused tab's pill (± one tab-gap of context) would clip the viewport; a
/// non-focused row HOLDS its scroll (gliding back to the start read as the row "jumping away"
/// whenever focus left it).
fn tab_hscroll_target() -> f32 {
    let v = view();
    if v.section != 1 {
        return v.tab_hscroll.pos;
    }
    let (x, w) = tab_focus_geom();
    let lo = x + w + 18.0 + TAB_ADVANCE - (SCR_W - MARGIN_X); // pill right edge (+context) on screen
    let hi = x - 18.0 - TAB_ADVANCE - MARGIN_X; // pill left edge (−context) on screen
    card_row::reveal(v.tab_hscroll.pos, lo, hi, f32::MAX) // no content-max: hi already bounds it
}

/// episode-row horizontal scroll target: minimal scroll-into-view via the shared CardRow rule;
/// a non-focused row HOLDS its scroll (it snaps to the start only on a season change — see the
/// `pump_season` reset in update()).
fn ep_hscroll_target() -> f32 {
    let v = view();
    if v.section != 2 {
        return v.ep_hscroll.pos;
    }
    let n = n_items(2).max(0) as usize;
    card_row::scroll_into_view(v.ep_hscroll.pos, v.col.max(0) as usize, n, EP_W, EP_GAP, SCR_W - 2.0 * MARGIN_X)
}

/// The ONE hero-synopsis TextView (rung / leading / line cap live here once) — shared by the
/// flow measure (hero_layout) and the paint (draw_hero) so they cannot diverge.
fn hero_synopsis(summary: &str) -> TextView<'_> {
    TextView::new(summary, theme::size::MICRO, theme::TEXT_SECONDARY).leading(29.0).max_lines(4)
}

/// The hero text column's y-chain — title bottom (566) → meta (+36) → synopsis (+46, MEASURED) →
/// date (+20) → buttons (+46) — computed ONCE for both the painter (draw_hero) and the flow
/// (update()'s column.top), so the pixels and the scroll math cannot desync. A 4-line synopsis
/// used to push the Play pill to within 2px of the season tabs; the flow now keeps one
/// standardized gap (`BTN_CONTENT_GAP`) under the buttons for every title.
/// Returns (meta_y, syn_y, date_y, btn_y).
fn hero_layout(m: Option<&PmsMovie>) -> (f32, f32, f32, f32) {
    let d = metadata::current();
    let owned = if d.is_none() { m.map(|m| m.summary.clone()).unwrap_or_default() } else { String::new() };
    let summary: &str = d.map(|d| d.summary.as_str()).unwrap_or(&owned);
    let syn_h = if summary.is_empty() { 0.0 } else { hero_synopsis(summary).measure_h(900.0) };
    let meta_y = 566.0 + 36.0;
    let syn_y = meta_y + 46.0;
    let date_y = syn_y + syn_h.max(34.0) + 20.0;
    (meta_y, syn_y, date_y, date_y + 46.0)
}

pub(crate) fn update(dt: f32) {
    // apply a landed async season fetch: the episode row starts over on the new list (focus +
    // scroll to episode 0 — the remembered position belonged to the previous season)
    if metadata::pump_season() {
        let v = view();
        v.ep_hscroll.jump(0.0);
        v.saved_col[2] = 0;
        if v.section == 2 {
            v.col = 0;
            v.card_scale.jump(1.0);
        }
    }
    view().spin_ms += dt * 1000.0;
    // the flow top rides the measured hero: buttons bottom + one standardized gap (hero_layout)
    view().column.top = hero_layout(selected()).3 + CD + BTN_CONTENT_GAP;
    // targets read view() internally — compute them before borrowing v for the springs
    let sct = scroll_target();
    let hst = ep_hscroll_target();
    let tst = tab_hscroll_target();
    let v = view();
    v.column.scroll.step(sct, K_SCROLL, dt);
    crate::ui::anim::probe("detail.scroll", v.column.scroll.pos, v.column.scroll.vel, sct, dt);
    v.card_scale.step(crate::ui::widgets::CARD_FOCUS_SCALE, 300.0, dt);
    crate::ui::anim::probe("detail.card", v.card_scale.pos, v.card_scale.vel, crate::ui::widgets::CARD_FOCUS_SCALE, dt);
    // per-episode pop springs: focused cell → CARD_FOCUS_SCALE, all others ease back to 1.0 (so a
    // just-deselected episode animates its pop + lifted shadow out instead of snapping). Step every
    // spring each frame (cheap), same discipline as CardRow.
    let ecol = if v.section == 2 { v.col.max(0) as usize } else { usize::MAX };
    let en = n_items(2).max(0) as usize;
    for (i, sp) in v.ep_scale.iter_mut().enumerate() {
        let t = if i == ecol && i < en { crate::ui::widgets::CARD_FOCUS_SCALE } else { 1.0 };
        sp.step(t, 300.0, dt);
    }
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
    // clear to the app's base gray (the backdrop art/ambient draw over it; below-hero rows reveal it)
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
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
    // compact centered title fades in at the top of the scrolled view (pinned, not scrolled) —
    // but only while the scroll sits in the season/episode band; diving into cast/related/about
    // fades it back out (the logo identifies the show while season-browsing, then gets out of the
    // way — it doesn't fit the tightened section gap anyway).
    if hero_a < 0.99 {
        let la = (1.0 - hero_a) * compact_title_vis(scroll);
        if la > 0.01 {
            phase("dt.ctitle", || draw_compact_title(p.alpha(la), m));
        }
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
    fn draw_child(&self, i: usize, env: &Env, p: Painter) {
        use crate::ui::profile::phase;
        let (s, _) = sections();
        match s[i + 1] {
            1 => phase("dt.tabs", || draw_tabs(p)),
            // while a season fetch is in flight the row shows the PREVIOUS season's episodes,
            // dimmed, with a spinner — the switch is async so rapid tab hops never freeze the UI
            2 => phase("dt.eps", || {
                if metadata::season_loading() {
                    draw_episodes(p.alpha(0.35));
                    crate::ui::widgets::Spinner::new(SCR_W * 0.5, EP_H * 0.5, 26.0)
                        .phase(self.spin_ms as u32)
                        .tint(theme::TEXT_PRIMARY)
                        .draw(env, p);
                } else {
                    draw_episodes(p);
                }
            }),
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
        m.filter(|m| !m.art.is_empty())
            .map(|m| m.art.clone())
            .or_else(|| metadata::current().map(|d| d.art.clone()).filter(|s| !s.is_empty()))
            .map(|art| resolve_tex(&art, 1920, 1080, 0))
            .unwrap_or(0)
    } else {
        0
    };
    // ambient wash from the item's UltraBlur corners, dimmed by the scroll (`d`), drawn ONLY when it
    // will actually show: no art texture yet, or the art has faded on scroll enough to reveal it.
    // Because black-over-at-alpha is a linear multiply, dimming each source layer by `d` is
    // pixel-identical to the old separate full-screen dim overlay, one fewer full-screen pass.
    if let Some(m) = m {
        if m.has_blur && (art_tex == 0 || art_a < 0.99) {
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
    // the shelf gray fades in as the page scrolls off the hero, so the below-hero rows sit on the
    // app's base gray (their Related/Cast card shadows read) regardless of the item's backdrop art or
    // UltraBlur wash. Fully transparent in the hero view, so no wasted fill there.
    if sf > 0.001 {
        let g = theme::with_a(theme::SURFACE_APP, sf);
        p.rect(Rect::FULL, 0.0, g, g, 0.0);
    }
}

fn draw_hero(p: Painter, env: &Env, m: Option<&PmsMovie>) {
    let tx = MARGIN_X;
    let w_a = theme::TEXT_PRIMARY;
    let d_a = theme::TEXT_SECONDARY;
    let dim = theme::TEXT_TERTIARY;
    let d = metadata::current();

    // ---- title: clearLogo (transparent PNG) if loaded, else bold text. Borrow from the loaded
    // Detail (the common case) — cloning rk/title/summary every frame was pure churn; the catalog
    // fallback (`m`, pre-load frames only) clones from the catalog row. ----
    let title_bottom = 566.0f32;
    let rk_owned = if d.is_none() { m.map(|m| m.rk.clone()).unwrap_or_default() } else { String::new() };
    let rk: &str = d.map(|d| d.rk.as_str()).unwrap_or(&rk_owned);
    let mut drew_logo = false;
    if let Some((lt, ww, hh)) = crate::posters::logo_tex(rk, 680.0, 120.0) {
        p.tex(lt, Rect::new(tx, title_bottom - hh, ww, hh), 0.0, w_a);
        drew_logo = true;
    }
    if !drew_logo {
        let title_owned = if d.is_none() { m.map(|m| m.title.clone()).unwrap_or_default() } else { String::new() };
        let title: &str = d.map(|d| d.title.as_str()).unwrap_or(&title_owned);
        if let Ok(t) = CString::new(title) {
            p.text(t.as_ptr(), tx, title_bottom - 68.0, theme::size::HERO, w_a, 0, 1);
        }
    }

    // ---- meta line: "TV Show · Sci-Fi · Adventure · 18+" ----
    let mut parts: Vec<&str> = Vec::new();
    if let Some(d) = d {
        parts.push(if d.is_show { "TV Show" } else { "Movie" });
        for g in d.genres.iter().take(2) {
            parts.push(g);
        }
        if !d.rating.is_empty() {
            parts.push(&d.rating);
        }
    }
    // the whole y-chain below comes from the ONE shared layout (also feeds the scroll flow)
    let (meta_y, syn_y, date_y, btn_y) = hero_layout(m);
    if let Ok(mc) = CString::new(parts.join("   \u{b7}   ")) {
        p.text(mc.as_ptr(), tx, meta_y, theme::size::BODY, d_a, 0, 0);
    }

    // ---- synopsis: the shared fine-print TextView (hero_synopsis — one size app-wide, matches
    // the home hero); its measured height is already folded into date_y/btn_y by hero_layout ----
    let summary_owned = if d.is_none() { m.map(|m| m.summary.clone()).unwrap_or_default() } else { String::new() };
    let summary: &str = d.map(|d| d.summary.as_str()).unwrap_or(&summary_owned);
    if !summary.is_empty() {
        hero_synopsis(summary).draw(p, Rect::new(tx, syn_y, 900.0, 0.0));
    }

    // ---- date · runtime ----
    if let Some(d) = d {
        let mut info = pretty_date(&d.aired, d.year);
        if d.dur_ms >= 60_000 {
            if !info.is_empty() {
                info.push_str("    \u{b7}    ");
            }
            info.push_str(&crate::ui::fmt::dur_long(d.dur_ms));
        }
        if let Ok(ic) = CString::new(info) {
            p.text(ic.as_ptr(), tx, date_y, theme::size::CAPTION, dim, 0, 0);
        }
    }

    // ---- buttons (btn_y from hero_layout) ----
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
    // solid dark discs. Play reuses the pill Button; the check disc is the watched toggle (its
    // glyph lights RESUME amber while the item is marked watched).
    let tx = MARGIN_X;
    let focus = focus();
    let cx1 = tx + PW + CGAP;

    Button::new(c"Play".as_ptr(), theme::size::BODY, Rect::new(tx, y, PW, CD))
        .icon(crate::ui::icons::Icon::Play)
        .focused(focus == 0)
        .draw(env, p);
    let watched = metadata::current().map(|d| d.watched).unwrap_or(false);
    let wstyle = if watched && focus != 1 {
        ControlStyle::Custom { fill: theme::CONTROL_IDLE_FILL, ink: theme::RESUME_FILL }
    } else {
        ControlStyle::Accent
    };
    CircleButton::new(c"".as_ptr())
        .icon(crate::ui::icons::Icon::Check)
        .style(wstyle)
        .at(cx1, y)
        .focused(focus == 1)
        .draw(env, p);
}

/// Visibility (0..1) of the pinned compact title at scroll depth `scroll`: 1 while the first
/// below-hero sections (season tabs + episodes) hold the top, fading to 0 across the 300px before
/// the first DEEP section (cast/related/about) lifts to the top margin.
fn compact_title_vis(scroll: f32) -> f32 {
    let v = view();
    let (s, n) = sections();
    let hide_at = s[..n]
        .iter()
        .position(|&x| x >= 3) // first deep section id (cast=4 / related=3 / about=5)
        .filter(|&pos| pos >= 1)
        .map(|pos| {
            let col = v.column;
            col.lift_target(v, pos - 1)
        })
        .unwrap_or(f32::MAX);
    ((hide_at - scroll) / 300.0).clamp(0.0, 1.0)
}

/// small centered clearLogo/title shown at the top once the page is scrolled
fn draw_compact_title(p: Painter, m: Option<&PmsMovie>) {
    let d = metadata::current();
    let rk_owned = if d.is_none() { m.map(|m| m.rk.clone()).unwrap_or_default() } else { String::new() };
    let rk: &str = d.map(|d| d.rk.as_str()).unwrap_or(&rk_owned);
    let cx = SCR_W * 0.5;
    if let Some((lt, ww, hh)) = crate::posters::logo_tex(rk, f32::MAX, 54.0) {
        p.tex(lt, Rect::new(cx - ww * 0.5, 40.0, ww, hh), 0.0, theme::TEXT_PRIMARY);
        return;
    }
    let title_owned = if d.is_none() { m.map(|m| m.title.clone()).unwrap_or_default() } else { String::new() };
    let title: &str = d.map(|d| d.title.as_str()).unwrap_or(&title_owned);
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
    let e = Env::inert();
    for (i, x, w, lc) in tabs_layout(d) {
        let selected = i == d.cur_season;
        let focused = sec == 1 && col == i as c_int;
        // pill sized to the (bold) label — text sits at x, pill padded ±18, tabs advance by label
        // width + TAB_ADVANCE (see tabs_layout). The pill fills the block height (TAB_ROW_H == the
        // hero-button CD), so tabs and buttons read as one control family.
        if on_axis(x - 18.0 - sx, w + 36.0, SCR_W, 0.0) {
            crate::ui::widgets::TabPill::new(lc.as_ptr(), theme::size::BODY, Rect::new(x - 18.0, tab_y, w + 36.0, TAB_ROW_H))
                .segment(selected)
                .focused(focused)
                .draw(&e, pt);
        }
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
    // the ui::press click dip folds into the focused episode's per-cell pop below
    let press = crate::ui::press::scale();
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
        // per-cell pop: the focused episode springs up (press dip folded in); a just-deselected one
        // springs back down, so its scale + lifted shadow animate out instead of snapping. Episodes
        // past the spring cap snap to the focus scale (no per-cell animation) rather than losing the
        // focus cue entirely.
        let base = view()
            .ep_scale
            .get(i)
            .map(|s| s.pos)
            .unwrap_or(if focused { crate::ui::widgets::CARD_FOCUS_SCALE } else { 1.0 });
        let sc = if focused { base * press } else { base };
        // the focused cell is always "popped" so a press dip (sc < 1) still scales it (the dip would
        // otherwise be clamped away); a deselecting cell stays popped until its spring settles back.
        let popped = focused || sc > 1.001;
        let card = Rect::new(x, ep_y, EP_W, EP_H);
        // episode still + focus ring + scale-pop (shared with the chapters strip)
        crate::ui::widgets::draw_card(pe, card, &ep.thumb, (640, 360), 12.0, popped, sc);
        // resume bar (tracks the scaled card while it's popped)
        if ep.resume_ms > 0 && ep.dur_ms > 0 {
            let cr = if popped { card.scaled(sc) } else { card };
            let frac = (ep.resume_ms as f32 / ep.dur_ms as f32).clamp(0.0, 1.0);
            let bar = Rect::new(cr.x + 12.0, cr.y + cr.h - 16.0, cr.w - 24.0, 5.0);
            pe.rrect(bar, 2.5, 2.5, theme::RAIL_BUFFERED);
            pe.rrect(Rect::new(bar.x, bar.y, bar.w * frac, bar.h), 2.5, 2.5, theme::RAIL_FILL);
        }
        // under-card metadata: kicker → title → full summary → air date. The summary flows off the
        // ACTUAL title height (1 or 2 lines) — nothing is reserved, so a 1-line title doesn't leave
        // a gap — and the fine-print date (MICRO) closes the block at the bottom. The block height
        // tracks the tallest episode via ep_meta_h.
        let ty = ep_y + EP_H + EP_META_TOP;
        let titc = if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
        let date = pretty_date(&ep.aired, 0); // formatted once — feeds both the layout and the draw
        let (date_y, summary_y, _) = ep_meta_layout(&ep.title, !date.is_empty(), &ep.summary);
        if let Ok(ec) = CString::new(format!("EPISODE {}", ep.index)) {
            pe.text(ec.as_ptr(), x, ty, theme::size::CAPTION, dimc, 0, 1);
        }
        // title wraps to at most 2 lines (elided past that) — long titles used to run into the next card
        TextView::new(&ep.title, theme::size::LABEL, titc)
            .bold()
            .leading(EP_TITLE_LEAD)
            .max_lines(2)
            .draw(pe, Rect::new(x, ty + EP_TITLE_DY, EP_W, 0.0));
        if !ep.summary.is_empty() {
            TextView::new(&ep.summary, theme::size::CAPTION, dimc)
                .leading(EP_SUMMARY_LEAD)
                .max_lines(EP_SUMMARY_MAXLINES)
                .draw(pe, Rect::new(x, ty + summary_y, EP_W, 0.0));
        }
        if !date.is_empty() {
            if let Ok(dc) = CString::new(date) {
                pe.text(dc.as_ptr(), x, ty + date_y, theme::size::MICRO, dimc, 0, 0);
            }
        }
    }
}

/// The ONE two-pass strip loop for detail's CardRow shelves (Related, Cast): non-focused tiles
/// first (off-axis culled), the focused tile LAST for its z-order (the in-row focused-last rule —
/// a lone strip has no cross-row neighbour). `art` supplies each tile's Art; `title` the focused
/// tile's under-title (Related; None for cast); `extra` any per-tile caption drawn for EVERY tile
/// (cast names/roles; a no-op for Related). `axis_span` widens the cull band where captions
/// should survive slightly off-screen.
#[allow(clippy::too_many_arguments)]
fn draw_strip<'a>(
    p: Painter,
    row: &CardRow,
    n: usize,
    focus_col: c_int,
    row_y: f32,
    size: (f32, f32),
    pitch: f32,
    sty: &RowStyle,
    axis_span: f32,
    art: impl Fn(usize) -> Art<'a>,
    title: impl Fn(usize) -> Option<CString>,
    extra: impl Fn(Painter, usize, f32, bool),
) {
    let sx = row.scroll_x();
    let pr = p.translate(-sx, 0.0);
    for i in 0..n {
        if i as c_int == focus_col {
            continue; // focused tile drawn last
        }
        let x = MARGIN_X + i as f32 * pitch;
        if !on_axis(x - sx, size.0, axis_span, 0.0) {
            continue;
        }
        let s = row.scale(i);
        let rect = Rect::new(x, row_y, size.0, size.1).scaled(s);
        card_row::draw_tile(pr, art(i), rect, s, sty, None);
        extra(pr, i, x, false);
    }
    if focus_col >= 0 && (focus_col as usize) < n {
        let i = focus_col as usize;
        let x = MARGIN_X + i as f32 * pitch;
        // fold the ui::press click dip into the focused tile's scale (1.0 when idle) — same as home
        let s = row.scale(i) * crate::ui::press::scale();
        let rect = Rect::new(x, row_y, size.0, size.1).scaled(s);
        let tc = title(i); // kept alive across the draw call
        let tp = tc.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
        card_row::draw_focused(pr, art(i), rect, s, sty, None, tp, std::ptr::null(), false);
        extra(pr, i, x, true);
    }
}

/// "Related" — a horizontal row of portrait poster cards from the related hub. A real instance of
/// the home shelf: view().related owns the animated per-card scale springs + scroll spring
/// (RowStyle::HOME), so it pops + glides + rings + titles exactly like a home shelf.
fn draw_related(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    if d.related.is_empty() {
        return;
    }
    let related_y = 0.0; // local origin (ScrollColumn pre-translates to this section's top)
    let focus_col = if view().section == 3 { view().col } else { -1 };
    let focused = (focus_col >= 0).then_some(focus_col as usize);
    let lift = card_row::title_lift(&view().related, focused, &RowStyle::HOME, view().related.scroll_x());
    p.text(c"Related".as_ptr(), MARGIN_X, related_y - lift, theme::size::HEADLINE, theme::TEXT_HEADING, 0, 1);
    draw_strip(
        p,
        &view().related,
        d.related.len(),
        focus_col,
        related_y + REL_LABEL_H,
        (REL_W, REL_H),
        REL_W + REL_GAP,
        &RowStyle::HOME,
        SCR_W,
        |i| Art::Thumb { key: &d.related[i].thumb, res: (250, 375) },
        |i| CString::new(d.related[i].title.clone()).ok(),
        |_, _, _, _| {},
    );
}

/// "Cast & Crew" — circular headshots with names, on the shared [`CardRow`] (circular
/// `RowStyle::CAST`): spring focus-magnification + animated scroll + glow ring, exactly like the
/// poster/Related shelves. Cast shows every member's name/role (a poster row titles only the
/// focused tile), so the labels ride the `extra` hook.
fn draw_cast(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    if d.cast.is_empty() {
        return;
    }
    let cast_y = 0.0; // local origin (ScrollColumn pre-translates to this section's top)
    let focus_col = if view().section == 4 { view().col } else { -1 };
    let focused = (focus_col >= 0).then_some(focus_col as usize);
    let lift = card_row::title_lift(&view().cast, focused, &RowStyle::CAST, view().cast.scroll_x());
    p.text(c"Cast & Crew".as_ptr(), MARGIN_X, cast_y - lift, theme::size::HEADLINE, theme::TEXT_HEADING, 0, 1);
    let row_y = cast_y + CAST_LABEL_H;
    draw_strip(
        p,
        &view().cast,
        d.cast.len(),
        focus_col,
        row_y,
        (CAST_D, CAST_D),
        CAST_SLOT,
        &RowStyle::CAST,
        SCR_W + CAST_D,
        |i| Art::Thumb { key: &d.cast[i].thumb, res: (300, 300) },
        |_| None,
        |pc, i, x, focused| cast_label(pc, &d.cast[i].tag, &d.cast[i].role, x + CAST_D * 0.5, row_y, focused),
    );
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
/// apply Plex's resume rule (metadata::resume_ns) and stash the position for app.rs.
fn set_resume(resume_ms: i64, dur_ms: i64) {
    view().last_resume_ns = metadata::resume_ns(resume_ms, dur_ms);
}

/// OK/SELECT on the detail page: returns true if playback should start (the route
/// URL/HUD have already been set). Section 0 = hero Play, 1 = season tab, 2 = episode.
/// Is the focused element a CARD (episode / Related / Cast tile) rather than the Play pill, a season
/// tab, or an About row? Card focus gets the tvOS press — the OK dips it and the activation commits on
/// release (app.rs); the non-card controls activate immediately.
pub(crate) fn focus_is_card() -> bool {
    matches!(view().section, 2 | 3 | 4)
}

pub(crate) fn on_ok() -> bool {
    view().last_resume_ns = 0; // default: no resume (set below for plays)
    let sec = view().section;
    let col = view().col;
    match sec {
        0 => {
            if col == 1 {
                // watched toggle: scrobble/unscrobble, then re-fetch so the button state, the
                // resume bars and the Continue Watching shelf all reflect the new view state.
                if let Some((rk, watched, season)) =
                    metadata::current().map(|d| (d.rk.clone(), d.watched, d.cur_season))
                {
                    if watched {
                        crate::plex::client().unscrobble(&rk);
                    } else {
                        crate::plex::client().scrobble(&rk);
                    }
                    metadata::load_detail(&rk);
                    if season != 0 {
                        metadata::load_season(season); // keep the season the user was browsing
                    }
                    crate::pms::refetch_hubs_reconcile(); // CW shelf changes with the view state
                }
                return false;
            }
            if col != 0 {
                return false; // watched (col 1) handled above; everything else inert
            }
            if is_show() {
                play_episode_at(0)
            } else {
                if let Some(m) = selected() {
                    crate::route::play_movie(m);
                } else if let Some(d) = metadata::current() {
                    // a hub refetch can orphan the page's catalog row (the item left the hubs) —
                    // the loaded Detail carries everything the string-based route entry needs
                    crate::route::play_episode(&d.rk, &d.part, &d.vcodec, &d.acodec, &d.title, "");
                } else {
                    return false;
                }
                set_resume(
                    metadata::current().map(|d| d.resume_ms).unwrap_or(0),
                    metadata::current().map(|d| d.dur_ms).unwrap_or(0),
                );
                true
            }
        }
        1 => {
            // only a season that isn't already selected loads — OK on the highlighted tab used to
            // fire a redundant refetch whose pump-apply then reset the retained episode position
            let cur = metadata::current().map(|d| d.cur_season as c_int).unwrap_or(-1);
            if col != cur {
                metadata::load_season(col.max(0) as usize);
            }
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

/// Re-resolve the selected catalog row after a hub refetch rebuilt the catalog underneath an open
/// detail page (indices move; the rk is the stable identity).
pub(crate) fn reselect() {
    if let Some(rk) = metadata::current().map(|d| d.rk.clone()) {
        view().selected = crate::pms::index_of_rk(&rk);
    }
}

/// Re-open the detail page for an arbitrary ratingKey (e.g. a Related item). Uses the
/// catalog row for the backdrop art/blur when the item is in the browse catalog, else
/// falls back to the loaded detail's own art (no blur).
pub(crate) fn open_rk(rk: &str) {
    let idx = crate::pms::index_of_rk(rk);
    let v = view();
    v.selected = idx;
    reset_view_state(v);
    metadata::load_detail(rk);
}

/// Open the SHOW detail page for `show_rk` with the season numbered `season_num` selected
/// (a season entry point routes to the show page, not a standalone season page).
pub(crate) fn open_rk_season(show_rk: &str, season_num: c_int) {
    open_rk(show_rk);
    if let Some(d) = metadata::current() {
        if let Some(pos) = d.seasons.iter().position(|s| s.index as c_int == season_num) {
            // BLOCKING on purpose: home_activate's season arm plays episodes[0] right after this
            // returns — the async tab-switch path would still hold the previous season's list
            metadata::load_season_now(pos);
        }
    }
}

fn play_episode_at(i: c_int) -> bool {
    // a season fetch is in flight: d.episodes is still the PREVIOUS season's list (drawn dimmed
    // under a spinner) while cur_season/the tab highlight already show the new one — launching
    // from it would play the wrong season's episode. Ignore the press; the row lands in a beat.
    if metadata::season_loading() {
        return false;
    }
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

/// The About block's derived strings, rebuilt only when the loaded item changes — the block used
/// to re-format ~20 strings (info pairs, the audio list, the accessibility rows) every frame it
/// was on screen.
struct AboutRows {
    rk: String,
    info: Vec<(&'static str, String)>,
    orig_audio: Option<String>,
    audio_list: String,
    access: Vec<(&'static str, &'static str)>,
}
static mut ABOUT: Option<AboutRows> = None;
fn about_rows(d: &metadata::Detail) -> &'static AboutRows {
    let slot = unsafe { &mut *addr_of_mut!(ABOUT) };
    if slot.as_ref().map(|c| c.rk != d.rk).unwrap_or(true) {
        // Information: (label, value) pairs
        let mut info: Vec<(&'static str, String)> = Vec::new();
        let released = pretty_date(&d.aired, d.year);
        if !released.is_empty() {
            info.push(("Released", released));
        }
        let dur = if d.dur_ms > 0 { d.dur_ms } else { d.episodes.first().map(|e| e.dur_ms).unwrap_or(0) };
        if dur > 0 {
            info.push(("Run Time", crate::ui::fmt::dur_long(dur)));
        }
        info.push(("Rated", if d.rating.is_empty() { "NR".to_string() } else { d.rating.clone() }));
        if !d.countries.is_empty() {
            info.push(("Regions of Origin", d.countries.join(", ")));
        }
        // Languages: an "Original Audio" pair + a wrapped "Audio" list
        let orig_audio =
            d.audio.first().map(|a| if a.lang.is_empty() { "Unknown".to_string() } else { a.lang.clone() });
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
        let acc_all: [(bool, &'static str, &'static str); 3] = [
            (cc, "CC", "Closed captions refer to subtitles in available languages with the addition of relevant non-dialogue information."),
            (sdh, "SDH", "Subtitles for the deaf and hard of hearing (SDH) refer to subtitles in the original language with the addition of relevant non-dialogue information."),
            (ad, "AD", "Audio descriptions (AD) refer to a narration track describing what is happening on screen, to provide context for those who are blind or have low vision."),
        ];
        let access = acc_all.iter().filter(|(pr, _, _)| *pr).map(|(_, l, ds)| (*l, *ds)).collect();
        *slot = Some(AboutRows { rk: d.rk.clone(), info, orig_audio, audio_list, access });
    }
    slot.as_ref().unwrap()
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

    // card + column geometry — the card's height HUGS its measured content (title + genres +
    // wrapped synopsis + padding); a fixed 330px box used to leave up to ~180px of dead space
    // inside the focus highlight for a short blurb.
    let (cw, cy, pad) = (640.0f32, about_y + 50.0, 30.0f32);
    let syn_w0 = cw - 2.0 * pad;
    let syn_h = TextView::new(&d.summary, theme::size::CAPTION, theme::TEXT_HEADING)
        .leading(30.0)
        .max_lines(5)
        .measure_h(syn_w0);
    let ch = pad + 100.0 + syn_h + pad;
    let col_y = about_y + 430.0;
    let lx = 760.0f32; // Languages column x
    let ax = 1360.0f32; // Accessibility column x

    // The column rows drive BOTH the measured focus highlight (so it wraps the content — a
    // fixed-height panel used to let a long audio list spill past the selection) and the paint
    // below. Cached per item (about_rows); the measures below are wrap-memoised in TextView.
    let rows = about_rows(d);
    let (info, orig_audio, audio_list, access) = (&rows.info, &rows.orig_audio, &rows.audio_list, &rows.access);

    // ---- measured content extent (px below col_y) of each column, mirroring the paint advances ----
    let info_off = 68.0 + info.iter().map(|(_, v)| pair_h(v)).sum::<f32>();
    let lang_off = if orig_audio.is_none() {
        30.0
    } else {
        let list_h = TextView::new(audio_list, theme::size::LABEL, val).leading(32.0).max_lines(6).measure_h(500.0);
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
        // the Information/Accessibility extents end in a per-row trailing gap (22 / 26px) that is
        // flow spacing, not content — trim it so the highlight hugs the last line's ink evenly.
        let hl = match view().col {
            0 => Rect::new(tx, cy, cw, ch),
            1 => Rect::new(tx - 26.0, col_y - HL_TOP, 600.0, info_off - 22.0 + HL_TOP + HL_BOT),
            2 => Rect::new(lx - 26.0, col_y - HL_TOP, 560.0, lang_off + HL_TOP + HL_BOT),
            _ => Rect::new(ax - 26.0, col_y - HL_TOP, 560.0, access_off - 26.0 + HL_TOP + HL_BOT),
        };
        p.rrect(hl, 18.0, 18.0, theme::OVERLAY_FOCUS_SOFT);
    }

    // ---- card: title, genres, summary — every run hard-bounded to the card's inner width (a
    // long title or genre list used to paint past the card frame) ----
    let ix = tx + pad;
    text_at(p, ix, cy + pad, theme::size::HEADLINE, hd, 1,
        &crate::text::elide(&d.title, syn_w0, theme::size::HEADLINE, 1, false));
    if !d.genres.is_empty() {
        text_at(p, ix, cy + pad + 44.0, theme::size::CAPTION, dim, 0,
            &crate::text::elide(&d.genres.join(", "), syn_w0, theme::size::CAPTION, 0, false));
    }
    let sy = cy + pad + 100.0;
    let syn_w = cw - 2.0 * pad;
    let mw = crate::text::text_width(c"MORE".as_ptr(), theme::size::CAPTION, 1);
    // CAPTION (not BODY): in the dense About card the body-size synopsis read oversized next to the
    // compact columns — match it to the Accessibility descriptions below (same CAPTION rung). When
    // the blurb is cut off, the last line dissolves (fade_last) into the MORE zone instead of
    // colliding with the label.
    let syn =
        TextView::new(&d.summary, theme::size::CAPTION, val).leading(30.0).max_lines(5).fade_last(mw + 36.0);
    let syn_hh = syn.draw(p, Rect::new(ix, sy, syn_w, 0.0));
    // "MORE" — quiet grey, pinned to the card's right padding edge ON the last synopsis line's cap
    // band (it used to sit bright in the bottom padding, visually detached from the text block).
    if syn.truncates(syn_w) {
        let (mt, _) = crate::text::text_cap_band(theme::size::CAPTION, 1);
        p.text(c"MORE".as_ptr(), tx + cw - pad, sy + syn_hh - 30.0 - mt, theme::size::CAPTION,
            theme::TEXT_TERTIARY, 2, 1);
    }

    // ---- Information ----
    text_at(p, tx, col_y, theme::size::HEADLINE, hd, 1, "Information");
    let mut yy = col_y + 68.0;
    for (label, value) in info {
        yy += draw_pair(p, tx, yy, label, value, lbl, val);
    }

    // ---- Languages ----
    text_at(p, lx, col_y, theme::size::HEADLINE, hd, 1, "Languages");
    let mut ly = col_y + 68.0;
    if let Some(orig) = orig_audio {
        ly += draw_pair(p, lx, ly, "Original Audio", orig, lbl, val);
    }
    if !audio_list.is_empty() {
        text_at(p, lx, ly, theme::size::CAPTION, lbl, 0, "Audio");
        TextView::new(audio_list, theme::size::LABEL, val).leading(32.0).max_lines(6).draw(p, Rect::new(lx, ly + 34.0, 500.0, 0.0));
    }

    // ---- Accessibility ----
    text_at(p, ax, col_y, theme::size::HEADLINE, hd, 1, "Accessibility");
    if access.is_empty() {
        text_at(p, ax, col_y + 68.0, theme::size::CAPTION, dim, 0, "\u{2014}");
    } else {
        let mut ay = col_y + 64.0;
        for (label, desc) in access {
            // the shared chip leaf (filled style) — cy = chip top + half its 34px height
            crate::ui::widgets::badge(p, ax, ay + 17.0, label, crate::ui::widgets::BadgeStyle::Filled);
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
