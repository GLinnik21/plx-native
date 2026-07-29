//! The home screen as a retui tree: Backdrop + Hero + Grid([CardRow; MAX_HUBS]).
//! fr/fc/snapTarget are the focus source of truth (private module state); the tree
//! reads them live each frame via Env and writes back through nav. plex_run drives it
//! through home_init/update/draw/move_focus/pointer_focus/wheel (crate path) and the
//! row/col/snap_target accessors.
#![allow(non_upper_case_globals)]
use crate::pms::PmsMovie;
use crate::ui::consts::*;
use crate::ui::icons::Icon;
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::{Art, Button, CircleButton, PageDots};
// `guard` was a private copy of this barrier living here; it is now the shared `ui::guard` (its
// doc comment carries the FFI-unwind rationale + the GL-scissor repair the local copy was missing).
use crate::ui::{guard, hero_alpha, on_axis, Env, Painter, Rect, Spring, View};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::ptr::{addr_of, addr_of_mut};

// ---- focus state: private main-thread module state. Home internals read/write these
// directly; app.rs (the only outside reader) reaches them through the accessors below. ----
static mut fr: c_int = 0;
static mut fc: c_int = 0;
static mut snapTarget: f32 = 0.0;
// rotating hero: current index into pms::hero_pool + a flip debounce (so a held LEFT/RIGHT, via the
// key-repeat path, can't machine-gun the carousel), the slide transition (outgoing index + a 0→1
// progress spring + direction), an idle auto-flip countdown, the action-row focus, and the
// on-screen action-button rects for pointer hover/clicks.
static mut hero_idx: c_int = 0;
static mut hero_flip_cd: f32 = 0.0;
static mut hero_prev: c_int = -1; // outgoing pool index while sliding (-1 = idle)
static mut hero_slide: Spring = Spring::at(1.0); // slide progress 0→1
static mut hero_dir: f32 = 1.0; // +1 = next (content slides left), -1 = prev
static mut hero_auto: f32 = HERO_AUTO_S; // countdown to the next automatic flip
static mut hero_fc: c_int = 0; // hero focus: -1 = profile chip, 0 = pill, 1 = info, 2 = chevron
static mut hero_btns: [Rect; HERO_NBTN] = [Rect::new(0.0, 0.0, 0.0, 0.0); HERO_NBTN]; // pill / info / chevron
const HERO_FLIP_CD: f32 = 0.35;
const HERO_AUTO_S: f32 = 8.0; // idle seconds between automatic hero flips
const HERO_NBTN: usize = 3;
/// The sliding text column's bottom-anchor line: the tallest stack (96px logo + kicker + 3-line
/// synopsis) tops out at ~438 — the old top-down flow's start. The action row and page dots hang
/// at fixed offsets below it, so the chrome never jumps between hero items.
const HERO_TEXT_BOTTOM: f32 = 692.0;
/// The action row's top edge and control diameter. Module-level because the row SLIDES with its
/// item while the page dots below it do NOT — two draw sites, one geometry, so they cannot drift.
const HERO_ROW_Y: f32 = HERO_TEXT_BOTTOM + theme::space::MD;
const HERO_CTRL_D: f32 = 60.0;
const K_SLIDE: f32 = 130.0; // slide spring — a touch softer than the grid springs, reads cinematic
/// The slide is over once its remaining travel is **sub-pixel**. Threshold in PIXELS, not in
/// spring units: the old `pos > 0.995` cut retired the transition while the incoming layer still
/// sat `0.005 * SCR_W ≈ 10px` short of home, so every flip ended on a visible teleport. The
/// off-screen layers are culled per-frame, so carrying the spring to a true rest costs nothing.
const HERO_SLIDE_REST_PX: f32 = 0.5;
// top-left profile chip (avatar) rect, recorded each draw for pointer hit-testing (opens the
// account menu). See draw_chip / profile_chip_click. `chip_expand` is its focus animation — the
// widget takes the amount as a scalar, and springs belong to the screen, not to a leaf widget.
static mut profile_chip: Rect = Rect::new(0.0, 0.0, 0.0, 0.0);
static mut chip_expand: Spring = Spring::at(0.0);
const K_CHIP: f32 = 300.0; // chip unfurl — brisk, a touch stiffer than the hero slide

/// Focus accessors clamp INTO THE LIVE HUB BOUNDS at read time (mirroring Home::env's per-frame
/// clamp): a hub refetch can shrink the shelves underneath the raw statics, and the OK dispatch
/// reads these — an unclamped stale index made OK silently no-op on a visibly focused card.
pub(crate) fn row() -> c_int {
    let nh = n_hubs() as c_int;
    unsafe { addr_of!(fr).read() }.clamp(0, (nh - 1).max(0))
}
pub(crate) fn col() -> c_int {
    let nc = crate::pms::hub_len(row().max(0) as usize) as c_int;
    unsafe { addr_of!(fc).read() }.clamp(0, (nc - 1).max(0))
}
pub(crate) fn snap_target() -> f32 {
    unsafe { addr_of!(snapTarget).read() }
}
/// The snap spring's live POSITION (0 = hero fully shown, 1 = grid) — unlike [`snap_target`] this
/// lags through the transition, so it reflects what is actually on screen. The home OK handler gates
/// on it so a quick DOWN→OK launches the hero item still visible, not the grid's first card. Falls
/// back to the target before the scene is built.
pub(crate) fn snap_pos() -> f32 {
    unsafe { (*addr_of!(SCENE)).as_ref().map(|h| h.snap.pos).unwrap_or_else(|| addr_of!(snapTarget).read()) }
}
pub(crate) fn set_row(v: c_int) {
    unsafe { addr_of_mut!(fr).write(v) }
}
pub(crate) fn set_snap_target(v: f32) {
    unsafe { addr_of_mut!(snapTarget).write(v) }
}

#[inline]
fn g_fr() -> c_int {
    unsafe { addr_of!(fr).read() }
}
#[inline]
fn g_fc() -> c_int {
    unsafe { addr_of!(fc).read() }
}

// Home grid is now N hub shelves of varying length (not the old fixed ROWS×COLS).
// The Grid's CardRow array + each row's cell springs are sized to these maxima; the
// *actual* counts come from pms::hub_count()/hub_len().
const MAX_HUBS: usize = 16; // Continue Watching, On Deck, Recently Added, collections…
const MAX_ITEMS: usize = 24; // cards per shelf

/// Rows the grid can actually address: the server's hub count clamped to the fixed `shelves`
/// array. **Every `shelves[]` index site must go through this** — a server with 3-4 libraries
/// returns well over `MAX_HUBS` hubs (/hubs promotes several rows per library on top of
/// continue/ondeck/recentlyAdded), and `pms::hub_count()` is uncapped.
fn n_hubs_of(server_hubs: usize) -> usize {
    server_hubs.min(MAX_HUBS)
}
fn n_hubs() -> usize {
    n_hubs_of(crate::pms::hub_count())
}

/// Step the focus row by `dir`, clamped into the addressable rows. Used by every vertical move:
/// the raw `fr + dir` it replaces could walk past the end of `shelves` and panic on the keypress
/// (`Home::env` clamps the value it *returns*, but never writes the clamp back to the global).
fn step_row(cur: c_int, dir: c_int, nrows: usize) -> c_int {
    if nrows == 0 {
        return 0;
    }
    (cur + dir).clamp(0, nrows as c_int - 1)
}

/// the item at (hub row, column) in the home hub grid, or None
pub(crate) fn movie_at(r: c_int, c: c_int) -> Option<&'static PmsMovie> {
    if r < 0 || c < 0 {
        return None;
    }
    crate::pms::hub_item(r as usize, c as usize)
}

/// The currently-shown rotating-hero item (curated pool: Continue Watching then Recently Added),
/// falling back to the first catalog item when the pool is empty. Backdrop, Hero and the home OK
/// handler all read the hero through this so they never disagree on which item is featured.
pub(crate) fn hero_item() -> Option<&'static PmsMovie> {
    let n = crate::pms::hero_pool_len();
    if n == 0 {
        return movie_at(0, 0);
    }
    let i = unsafe { addr_of!(hero_idx).read() }.clamp(0, n as c_int - 1);
    crate::pms::hero_pool_item(i as usize)
}

/// dev/test hook: jump the hero to a specific pool index (the `plxnative-heroidx` trigger) so a flipped
/// state can be captured headlessly. Clamped on read via [`hero_index`]/[`hero_item`].
pub(crate) fn set_hero_idx(i: c_int) {
    unsafe { addr_of_mut!(hero_idx).write(i.max(0)) };
}

/// current hero index, clamped into the live pool (0 when empty) — drives the page indicator.
fn hero_index() -> usize {
    let n = crate::pms::hero_pool_len();
    if n == 0 {
        return 0;
    }
    unsafe { addr_of!(hero_idx).read() }.clamp(0, n as c_int - 1) as usize
}

/// Advance the hero to the next (`dir=+1`) / previous (`dir=-1`) pooled item, wrapping, and start
/// the slide transition (outgoing item + direction + progress spring reset). Debounced by
/// `hero_flip_cd` so a held edge-key (via the repeat path) flips a few times a second, not every
/// frame — the same machine-gun guard the season tabs needed. Any flip (manual or automatic)
/// restarts the idle auto-flip countdown.
pub(crate) fn hero_flip(dir: c_int) {
    let n = crate::pms::hero_pool_len() as c_int;
    if n <= 1 {
        return;
    }
    unsafe {
        if addr_of!(hero_flip_cd).read() > 0.0 {
            return;
        }
        let cur = addr_of!(hero_idx).read().clamp(0, n - 1);
        addr_of_mut!(hero_prev).write(cur);
        addr_of_mut!(hero_dir).write(dir as f32);
        addr_of_mut!(hero_slide).write(Spring::at(0.0));
        addr_of_mut!(hero_idx).write((cur + dir).rem_euclid(n));
        addr_of_mut!(hero_flip_cd).write(HERO_FLIP_CD);
        addr_of_mut!(hero_auto).write(HERO_AUTO_S);
    }
}

/// The slide transition, or None when idle: (outgoing pool index, outgoing x offset, incoming x
/// offset). Backdrop and Hero both read it so art and text move as one phase.
fn hero_slide_state() -> Option<(c_int, f32, f32)> {
    unsafe {
        let prev = addr_of!(hero_prev).read();
        if prev < 0 {
            return None;
        }
        let t = addr_of!(hero_slide).read().pos;
        let dir = addr_of!(hero_dir).read();
        Some((prev, -dir * t * SCR_W, dir * (1.0 - t) * SCR_W))
    }
}

/// The pooled hero item at index `i`, or None (negative, or out of a shrunken pool —
/// hero_pool_item bounds-checks the upper end).
fn hero_item_at(i: c_int) -> Option<&'static PmsMovie> {
    if i < 0 {
        return None;
    }
    crate::pms::hero_pool_item(i as usize)
}

/// Hero action-row focus: -1 = the profile chip, 0 = Play/Continue pill, 1 = info, 2 = chevron.
/// Below -1 sit the CENTERED tab pills (the top band, left→right: chip, then pill i as `-(i+2)`):
/// -2 = **Home**, -3 = the first section (Movies), -4 = the second (TV Shows), …
///
/// Home is pill 0 and a real focus stop like any other — it used to be packed as `-(i+1)`, which
/// aliased it onto the chip's -1, so the top band walked chip → Movies and the Home pill could
/// never take the focused (white) treatment on its own screen.
pub(crate) fn hero_focus() -> c_int {
    unsafe { addr_of!(hero_fc).read() }
}
pub(crate) fn set_hero_focus(v: c_int) {
    // cap at what the tab row can draw — focus must never land on a truncated pill
    let npill = 1 + crate::browse::section_count().min(crate::ui::widgets::MAX_TABS - 1); // Home + sections
    let lo = hero_focus_for_pill(npill - 1);
    unsafe { addr_of_mut!(hero_fc).write(v.clamp(lo, HERO_NBTN as c_int - 1)) }
}

/// Decode a top-band focus value to its tab-pill index (0 = Home, 1.. = sections), or None
/// when the focus isn't on a pill — the ONE home of the `-(i+2)` packing's sign math (inverse:
/// [`hero_focus_for_pill`]); it was previously inlined at four sites.
///
/// `c_int::MIN` is app.rs's "focus is a grid card, not the hero band" sentinel and is NOT a
/// pill: it satisfies the `<= -2` sign test but negates to itself (overflow), so an ungated
/// decode used to send EVERY grid-card OK into the last library section. Rejecting it here —
/// in the one place that owns the packing — keeps every caller safe by construction.
pub(crate) fn hero_pill_index(f: c_int) -> Option<usize> {
    (f <= -2 && f != c_int::MIN).then(|| (-f - 2) as usize)
}
/// Encode: the hero-focus value that puts the top band on tab pill `i` (0 = Home).
pub(crate) fn hero_focus_for_pill(i: usize) -> c_int {
    -(i as c_int) - 2
}

/// The focused thing is a pressable grid card — the same cross-screen predicate `detail` and
/// `library` expose, so app.rs asks one uniform per-route question (Home's grid always holds
/// cards, so "the grid is showing" is the whole answer).
pub(crate) fn focus_is_card() -> bool {
    snap_pos() > 0.5
}

/// Hero pointer hit-test against the action-row rects recorded at draw: returns the button index
/// (0 pill / 1 info / 2 chevron) or -2 for a miss. `hover` moves the hero focus without acting.
pub(crate) fn hero_button_at(mx: f32, my: f32) -> c_int {
    let btns = unsafe { addr_of!(hero_btns).read() };
    btns.iter().position(|r| r.contains(mx, my)).map(|i| i as c_int).unwrap_or(-2)
}
pub(crate) fn hero_pointer_focus(mx: f32, my: f32) {
    let b = hero_button_at(mx, my);
    if b >= 0 {
        set_hero_focus(b);
    }
}

// (the resume-bar fraction rule lives on PmsMovie::resume_frac — shared with the Library grid)

// ---- Backdrop: ambient wash + backdrop art (parallax/fade) + scrim. Uses
// explicit per-element alphas (NOT the cascade) since each fades on its own curve.
struct Backdrop;
impl Backdrop {
    fn new() -> Self {
        Backdrop
    }
}
impl View for Backdrop {
    fn draw(&self, env: &Env, p: Painter) {
        let sp = env.sp;
        // The flat dark-gray base (the shelves sit on this so card shadows read) is ALREADY laid down
        // by `home_draw`'s frame_clear(CLEAR_RGB) — and CLEAR_RGB == SURFACE_APP (#2C2C2E). Painting a
        // full-screen SURFACE_APP rect here was a redundant ~2M-fragment pass over that identical
        // color, so it's dropped; the hero art below blends against the clear (same base).
        // hero backdrop: art if present, else the ambient wash as a fallback — both confined to
        // the hero view, fading out as the grid rises so the shelf area stays flat gray. During a
        // flip the outgoing and incoming items' art slide side-by-side (same phase as the text).
        if sp < 0.996 {
            if let Some((prev, dx_out, dx_in)) = hero_slide_state() {
                backdrop_art(p, hero_item_at(prev), sp, dx_out);
                backdrop_art(p, hero_item(), sp, dx_in);
            } else {
                backdrop_art(p, hero_item(), sp, 0.0);
            }
        }
        if env.hero_a > 0.01 {
            // The hero TEXT SCRIM CONTRACT (design review, two lenses HIGH): the meta/synopsis
            // column must sit on protected ground regardless of art luminance — the old single
            // ramp only reached real strength BELOW the text band, so bright posters (Toy
            // Story 2's white/yellow) washed the copy out. Two stacked stops carry ~0.35–0.5
            // alpha through the text band (y≈550–700) while keeping the same shelf-line depth.
            let sa = 0.30 + 0.64 * env.hero_a;
            let mid = sa * 0.55;
            p.rect(Rect::new(0.0, SCR_H * 0.34, SCR_W, SCR_H * 0.31), 0.0,
                theme::scrim(0.0), theme::scrim(mid), 0.0);
            p.rect(Rect::new(0.0, SCR_H * 0.65, SCR_W, SCR_H * 0.35), 0.0,
                theme::scrim(mid), theme::scrim(sa), 0.0);
        }
    }
}

/// One hero item's backdrop layer at horizontal slide offset `dx`: the 1280×720 art (with the
/// grid-rise parallax/fade) or the ambient wash while the art hasn't resolved. A layer the slide
/// has already carried off the panel is culled — the outgoing art is gone for most of the
/// transition, and skipping it keeps the two-layer flip from doubling a full-screen fill.
fn backdrop_art(p: Painter, item: Option<&PmsMovie>, sp: f32, dx: f32) {
    let Some(h) = item else {
        return;
    };
    if !on_axis(dx, SCR_W, SCR_W, 0.0) {
        return;
    }
    let bt = crate::ui::widgets::resolve_tex(&h.art, 1280, 720, 0);
    if bt != 0 {
        p.tex(bt, Rect::new(dx, -sp * (SCR_H - 120.0), SCR_W, SCR_H), 0.0, [1.0, 1.0, 1.0, 1.0 - sp]);
    } else if h.has_blur {
        p.ambient(Rect::new(dx, 0.0, SCR_W, SCR_H), 0.55 * (1.0 - sp), h.blur);
    }
}

// ---- Hero: low-left content composite. Drawn under p.alpha(hero_a) so the whole
// group fades as one; widgets carry base alphas that the cascade scales.
struct Hero;
impl Hero {
    fn new() -> Self {
        Hero
    }
}
/// One hero item's sliding content column: title band (clearLogo or text) → small meta/kicker →
/// synopsis. For an EPISODE the show is the star: the show's clearLogo/title in the title band and
/// a "S1 E4 · Episode title" kicker — the episode's own name never headlines. The column is
/// **bottom-anchored** on `HERO_TEXT_BOTTOM`: heights are measured first and the stack grows UP,
/// so the synopsis' last line — and with it the pinned action row + page dots below — sits at the
/// same y for every item (top-down flow made the chrome jump on every flip). Every gap is a
/// `theme::space` rung and every step advances by the *measured* height of the element just drawn.
fn hero_content(hero: &PmsMovie, p: Painter, dx: f32) {
    let tx = MARGIN_X;
    let col_w = 660.0f32; // hero text column
    // a column the slide has carried off the panel costs a full text layout + glyph draws for
    // nothing (same rule as the backdrop layer's cull)
    if !on_axis(tx + dx, col_w, SCR_W, 0.0) {
        return;
    }
    let w_a = theme::TEXT_PRIMARY; // cascade applies hero_a
    let d_a = theme::TEXT_SECONDARY;

    let is_ep = hero.kind == 3;
    let logo_rk: &str = if is_ep && !hero.show_rk.is_empty() { &hero.show_rk } else { &hero.rk };
    let logo = crate::posters::logo_tex(logo_rk, col_w, 96.0);
    let title: &str = if is_ep && !hero.show_title.is_empty() { &hero.show_title } else { &hero.title };
    let title_tv = TextView::new(title, theme::size::HERO, w_a).bold().max_lines(1);
    let title_h = logo.map(|(_, _, hh)| hh).unwrap_or_else(|| title_tv.measure_h(col_w));

    // meta/kicker line — episodes: "S1 E4 · Episode title"; else "Movie/Show · YEAR · RATING"
    let meta = if is_ep {
        let ep_title = &hero.title;
        let mut s = String::new();
        if hero.season_index > 0 {
            s.push_str(&format!("S{} ", hero.season_index));
        }
        if hero.ep_index > 0 {
            s.push_str(&format!("E{}", hero.ep_index));
        }
        if !s.is_empty() && !ep_title.is_empty() {
            s.push_str(" \u{b7} ");
        }
        s.push_str(ep_title);
        s
    } else {
        let rating = &hero.rating;
        let noun = if hero.kind == 1 { "Show" } else { "Movie" };
        format!("{} \u{b7} {} \u{b7} {}", noun, hero.year, if rating.is_empty() { "NR" } else { rating })
    };
    let meta_tv = TextView::new(&meta, theme::size::BODY, d_a).max_lines(1);
    let meta_h = meta_tv.measure_h(col_w);

    // synopsis — the hero's fine-print "info" line (size::MICRO per explicit design direction:
    // ~11px ink), pixel-wrapped to the hero column. 3 lines is the ceiling: a 4th would push the
    // pinned action row's clearance into the peeking shelf. The kicker/meta line above stays at
    // BODY — it's the label the eye needs to catch.
    let summary = &hero.summary;
    let syn = (!summary.is_empty())
        .then(|| TextView::new(summary, theme::size::MICRO, d_a).leading(29.0).max_lines(3));
    let syn_h = syn.as_ref().map(|tv| theme::space::SM + tv.measure_h(col_w)).unwrap_or(0.0);

    // stack the measured blocks up from the anchor, then draw top-down
    let mut y = HERO_TEXT_BOTTOM - (title_h + theme::space::MD + meta_h + syn_h);
    if let Some((lt, ww, hh)) = logo {
        p.tex(lt, Rect::new(tx, y, ww, hh), 0.0, w_a);
    } else {
        title_tv.draw(p, Rect::new(tx, y, col_w, 0.0));
    }
    y += title_h + theme::space::MD;
    meta_tv.draw(p, Rect::new(tx, y, col_w, 0.0));
    y += meta_h;
    if let Some(tv) = syn {
        tv.draw(p, Rect::new(tx, y + theme::space::SM, col_w, 0.0));
    }
}

/// One hero item's action row — Play/Continue pill, info circle, chevron — at horizontal slide
/// offset `dx`. It rides WITH its item, in the same phase as the text column and the backdrop art:
/// the pill's label *is* the item's ("Continue" when that item has a resume point, else "Play"),
/// and so is its width, so a row that stayed put would have to relabel and resize itself
/// mid-flip — the one thing on screen contradicting the motion.
///
/// The row sits at a FIXED y (the text column above is bottom-anchored, so the button-to-text air
/// is one MD for every item). Pill + info + chevron are a real focus row (hero_fc), so LEFT/RIGHT
/// walk buttons instead of paging; the chevron is the pager. The pill launches playback directly
/// (the info circle is the road to the detail page). MD, not LG: the synopsis' leading box already
/// carries ~7px of descender slack, and the bigger rung read as the button drifting from its text.
///
/// `live` marks the INCOMING (real) row, the one whose rects become the pointer hit targets. They
/// are recorded at the row's DRAWN position — the same draw-and-hit-must-agree rule the grid's
/// `card_x` keeps — so a click mid-flip lands on the button the eye sees, and the outgoing ghost
/// is never clickable.
fn hero_actions(hero: &PmsMovie, env: &Env, p: Painter, dx: f32, live: bool) {
    let tx = MARGIN_X;
    let pill_y = HERO_ROW_Y;
    let hf = hero_focus();
    let (cd, cgap) = (HERO_CTRL_D, 20.0f32); // control diameter + inter-control gap
    let plabel = if hero.resume_ms > 0 { c"Continue" } else { c"Play" };
    let isz = theme::size::BODY as f32 * 1.15; // icon box (mirrors Button's own layout)
    let pw = isz + 12.0 + crate::text::text_width(plabel.as_ptr(), theme::size::BODY, 1) + 68.0;
    // local (painter-relative) frames, and the screen-space rects that mirror them
    let pill = Rect::new(tx, pill_y, pw, cd);
    let info = Rect::new(tx + pw + cgap, pill_y, cd, cd);
    let chev = Rect::new(info.x + cd + cgap, pill_y, cd, cd);
    if live {
        // recorded BEFORE the cull: a live row carried off the panel must take its hit targets
        // with it, or a click would still land on a button that is no longer there.
        let screen = [pill, info, chev].map(|r| Rect::new(r.x + dx, r.y, r.w, r.h));
        unsafe { addr_of_mut!(hero_btns).write(screen) };
    }
    if !on_axis(tx + dx, chev.x + cd - tx, SCR_W, 0.0) {
        return;
    }
    Button::new(plabel.as_ptr(), theme::size::BODY, pill).icon(Icon::Play).focused(hf == 0).draw(env, p);
    CircleButton::new(c"".as_ptr()).icon(Icon::Info).at(info.x, info.y).focused(hf == 1).draw(env, p);
    CircleButton::new(c"".as_ptr()).icon(Icon::Chevron).at(chev.x, chev.y).focused(hf == 2).draw(env, p);
}

impl View for Hero {
    fn draw(&self, env: &Env, p: Painter) {
        let Some(hero) = hero_item() else {
            return;
        };

        // per-item content + its action row, sliding during a flip (same phase/direction as the
        // backdrop art) — everything that belongs to the ITEM travels together.
        if let Some((prev, dx_out, dx_in)) = hero_slide_state() {
            if let Some(ph) = hero_item_at(prev) {
                let po = p.translate(dx_out, 0.0);
                hero_content(ph, po, dx_out);
                hero_actions(ph, env, po, dx_out, false);
            }
            let pi = p.translate(dx_in, 0.0);
            hero_content(hero, pi, dx_in);
            hero_actions(hero, env, pi, dx_in, true);
        } else {
            hero_content(hero, p, 0.0);
            hero_actions(hero, env, p, 0.0, true);
        }

        // Page indicator: one dot per pooled hero item, the current one lit — CENTRED on the panel,
        // because it paces the whole billboard rather than the action row it happens to sit under
        // (left-aligned it read as a fourth control in that row). SM keeps it on the action row's
        // baseline band — at MD it hovered midway to the peeking shelf and read as stuck to the
        // poster row instead.
        let pool_n = crate::pms::hero_pool_len();
        if pool_n > 1 {
            PageDots::new(pool_n)
                .active(hero_index())
                .centered_at(SCR_W * 0.5, HERO_ROW_Y + HERO_CTRL_D + theme::space::SM)
                .draw(env, p);
        }
    }
}

// ---- Grid: the collection view. Holds one CardRow per hub (the shared animated shelf component) +
// the vertical scroll spring; drives nav/hit-test/wheel, draws all non-focused cells then the single
// focused card LAST (cross-row z-order, invariant #3). CardRow owns the per-cell scale springs + the
// scroll spring + the tile rendering — the same component detail's Related row uses.
struct Grid {
    shelves: [CardRow; MAX_HUBS],
    scroll_y: Spring,
}
impl View for Grid {
    fn update(&mut self, env: &Env) {
        // delegate each row's per-cell scale springs + scroll spring to the shared CardRow (only the
        // focused row's scroll animates — focused=None freezes it, matching the old Shelf::update).
        //
        // The pop belongs to the GRID: in hero view the shelf only peeks and focus is on the
        // billboard, so a magnified tile down there read as "already selected" — and, being scaled
        // about its centre, it also broke the peek row's even margin/gap rhythm. No cell is focused
        // until the snap has committed to the grid, on the SAME 0.5 threshold `draw`'s focused-last
        // pass and [`focus_is_card`] use, so the pop, the label and the z-order all arrive together.
        let grid = env.sp > 0.5;
        for r in 0..MAX_HUBS {
            let focused = (grid && env.fr as usize == r).then_some(env.fc as usize);
            self.shelves[r].update(crate::pms::hub_len(r), focused, &RowStyle::HOME, env.dt);
        }
        let nh = n_hubs().max(1);
        let max_y = (nh as f32 * ROW_PITCH - (SCR_H - CONTENT_Y) + 60.0).max(0.0);
        // minimal scroll-into-view (the vertical twin of CardRow's rule, same reveal core): only
        // move the page when the focused row's block — title band above, card + focused label
        // below — would clip the viewport; a fully visible row never re-seats the page.
        let top = env.fr as f32 * ROW_PITCH;
        let lo = top + GRID_TOP_Y + CARD_DY + CARD_H + 96.0 - (SCR_H - 24.0); // card + title/caption metadata visible
        let hi = top + GRID_TOP_Y - 66.0 - 96.0; // hub title band clear below the chip row
        self.scroll_y.step(card_row::reveal(self.scroll_y.pos, lo, hi, max_y), K_SCROLL, env.dt);
    }
    fn layout(&mut self, _frame: Rect, env: &Env) {
        // full-screen root view: positions absolutely from PEEK_Y/GRID_TOP_Y by env.sp, so the
        // trait's frame rect (env.screen) is unused.
        let shelf_top = PEEK_Y + (GRID_TOP_Y - PEEK_Y) * env.sp; // 828 -> 150
        for r in 0..MAX_HUBS {
            self.shelves[r].base_y = shelf_top + r as f32 * ROW_PITCH - self.scroll_y.pos * env.sp;
        }
    }
    fn draw(&self, env: &Env, p: Painter) {
        let nh = n_hubs();
        // PASS 1 — every shelf's non-focused cells (the globally-focused cell is skipped in grid mode,
        // drawn LAST below so it overlaps neighbouring rows: cross-row z-order, invariant #3).
        for r in 0..nh {
            let row_y = self.shelves[r].base_y;
            if !on_axis(row_y, CARD_H, SCR_H, 0.0) {
                continue;
            }
            // hub title above the row — held clear of the magnified card so the focus glow never
            // washes over it (the shared CardRow heading-clearance rule: it rises once when a card
            // moves under it and STAYS up until none is, rather than tracking the pop)
            if env.sp > 0.02 {
                let lift = self.shelves[r].lift();
                if let Ok(t) = CString::new(crate::pms::hub_title(r)) {
                    p.text(t.as_ptr(), MARGIN_X, row_y - 34.0 - lift, theme::size::HEADLINE, theme::with_a(theme::TEXT_PRIMARY, env.sp), 0, 1);
                }
            }
            for c in 0..crate::pms::hub_len(r) {
                if r == env.fr as usize && c == env.fc as usize && env.sp > 0.5 {
                    continue; // focused card drawn last (grid z-order)
                }
                let m = movie_at(r as c_int, c as c_int);
                let Some(mm) = m else { continue };
                let x = card_x(c, self.eff_scroll(r, env.sp));
                if !on_axis(x, CARD_W, SCR_W, GLOW_PAD) {
                    continue;
                }
                let s = self.shelves[r].scale(c);
                let rect = Rect::new(x, row_y + CARD_DY, CARD_W, CARD_H).scaled(s);
                let resume = mm.resume_frac();
                card_row::draw_tile(p, Art::Poster(m), rect, s, &RowStyle::HOME, resume);
            }
        }
        // PASS 2 — the single focused card + ring + metadata (title / "S1 • E8" / year), drawn
        // LAST for cross-row z-order (grid mode).
        if env.sp > 0.5 {
            let (r, c) = (env.fr as usize, env.fc as usize);
            if r >= nh {
                return;
            }
            // The focused card is the only one that can be pressed; fold the ui::press dip/bounce
            // factor into its scale (1.0 when idle, so a no-op unless a click is in flight).
            let s = self.shelves[r].scale(c.min(MAX_ITEMS - 1)) * crate::ui::press::scale();
            let x = card_x(c, self.eff_scroll(r, env.sp));
            let rect = Rect::new(x, self.shelves[r].base_y + CARD_DY, CARD_W, CARD_H).scaled(s);
            let m = movie_at(r as c_int, c as c_int);
            let cw = crate::pms::hub_is_continue(r); // Continue Watching: amber ▶ + "show · X min left"
            // keep the CStrings alive through the draw
            let title_c = m.and_then(|mm| CString::new(mm.title.as_str()).ok());
            let title = title_c.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
            let caption = m.and_then(|mm| if cw { cw_caption(mm) } else { focused_caption(mm) });
            let cap_ptr = caption.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
            let resume = m.and_then(PmsMovie::resume_frac);
            card_row::draw_focused(p, Art::Poster(m), rect, s, &RowStyle::HOME, resume, title, cap_ptr, cw);
        }
    }
}

/// Metadata caption under the FOCUSED poster: episodes read "S1 • E8", movies their year — the
/// selected item's info lives with the selected poster instead of floating between the rows.
fn focused_caption(m: &PmsMovie) -> Option<CString> {
    let s = if m.kind == 3 && m.ep_index > 0 {
        if m.season_index > 0 {
            format!("S{} \u{2022} E{}", m.season_index, m.ep_index)
        } else {
            format!("E{}", m.ep_index)
        }
    } else if m.kind == 0 && m.year > 0 {
        m.year.to_string()
    } else {
        return None;
    };
    CString::new(s).ok()
}

/// The focused Continue-Watching card's secondary line (Home Screen.dc): an in-progress item reads
/// "<show> · 8 min left" (episodes) or just the time-remaining (a resumed movie); a next-up episode
/// (no resume point yet) reads "<show> · New episode". `title` above it carries the episode name.
fn cw_caption(m: &PmsMovie) -> Option<CString> {
    let show = m.show_title.as_str();
    let s = if m.resume_ms > 0 && m.dur_ns > 0 {
        let left = crate::ui::fmt::time_left(m.dur_ns / 1_000_000 - m.resume_ms);
        if m.kind == 3 && !show.is_empty() {
            format!("{show} \u{00b7} {left}")
        } else {
            left // a resumed movie: time-remaining alone
        }
    } else if m.kind == 3 {
        // next-up episode: no resume point, so no bar and no time — just the "New episode" cue
        if show.is_empty() {
            "New episode".to_string()
        } else {
            format!("{show} \u{00b7} New episode")
        }
    } else {
        return None;
    };
    CString::new(s).ok()
}
/// The drawn left edge of grid column `c` in a row whose EFFECTIVE horizontal scroll is `es`.
/// The one x formula the draw, the pointer hit-test and the vertical column-keeper all share —
/// hit_at/vert used to re-derive it WITHOUT the `* sp` snap fold the draw applies, so the hover
/// target disagreed with the drawn frame mid-snap (hero→grid).
#[inline]
fn card_x(c: usize, es: f32) -> f32 {
    MARGIN_X + c as f32 * (CARD_W + GAP) - es
}
/// Inverse of [`card_x`]: the column of `count` whose drawn span contains `mx` (hit_at's scan).
#[inline]
fn col_at(mx: f32, es: f32, count: usize) -> Option<usize> {
    (0..count).find(|&c| {
        let x = card_x(c, es);
        mx >= x && mx <= x + CARD_W
    })
}
impl Grid {
    fn new() -> Self {
        Grid { shelves: [CardRow::new(); MAX_HUBS], scroll_y: Spring::at(0.0) }
    }
    /// Row `r`'s DRAWN horizontal scroll: the shelf spring folded by the hero→grid snap `sp`
    /// (the hero view shows rows unscrolled; the fold is how draw has always applied it).
    #[inline]
    fn eff_scroll(&self, r: usize, sp: f32) -> f32 {
        self.shelves[r].scroll_x() * sp
    }
    // ---- navigation: writes the fr/fc globals (never caches focus) ----
    fn nav(&self, sym: c_uint, sp: f32) {
        unsafe {
            let nh = n_hubs() as c_int;
            let nc = crate::pms::hub_len(fr.max(0) as usize) as c_int;
            if sym == SDLK_LEFT && fc > 0 {
                fc -= 1;
            } else if sym == SDLK_RIGHT && fc < nc - 1 {
                fc += 1;
            } else if sym == SDLK_UP && fr > 0 {
                self.vert(-1, sp);
            } else if sym == SDLK_DOWN && fr < nh - 1 {
                self.vert(1, sp);
            }
        }
    }
    /// vertical move keeping VISUAL column alignment across rows' animated scroll
    unsafe fn vert(&self, dir: c_int, sp: f32) {
        // Both indices must be clamped to the addressable rows: `fr` is a raw global that a
        // stale write (or a server with >MAX_HUBS hubs) can push past the end of `shelves`,
        // and this runs on the KEYPRESS — it panicked before `draw` was ever reached.
        let n = n_hubs();
        let cur = step_row(g_fr(), 0, n);
        let ncur = step_row(g_fr(), dir, n);
        let cx = card_x(g_fc().max(0) as usize, self.eff_scroll(cur as usize, sp)) + CARD_W * 0.5;
        let mut nc =
            ((cx - MARGIN_X - CARD_W * 0.5 + self.eff_scroll(ncur as usize, sp)) / (CARD_W + GAP) + 0.5) as c_int;
        let ncount = crate::pms::hub_len(ncur as usize) as c_int;
        nc = nc.clamp(0, (ncount - 1).max(0));
        fr = ncur;
        fc = nc;
    }
    /// Card under the pointer, or None. Vertical fly-away guard: a row that is only PARTIALLY on
    /// screen is not hoverable unless it is already the focused row — hovering it would move `fr`
    /// and the page spring would chase it (vertical auto-scroll), which is exactly the "pointer
    /// flies away" the pointer rules ban; horizontal scroll-into-view within a row is kept.
    fn hit_at(&self, mx: f32, my: f32, sp: f32) -> Option<(usize, usize)> {
        for r in 0..n_hubs() {
            let row_y = self.shelves[r].base_y + CARD_DY;
            if my < row_y || my > row_y + CARD_H {
                continue;
            }
            let fully_visible = row_y >= 40.0 && row_y + CARD_H <= SCR_H - 20.0;
            if !fully_visible && r != g_fr() as usize {
                continue;
            }
            if let Some(c) = col_at(mx, self.eff_scroll(r, sp), crate::pms::hub_len(r)) {
                return Some((r, c));
            }
        }
        None
    }
    /// hover/click focus write: focus the card under the pointer; reports whether one was hit
    fn hit_test(&self, mx: f32, my: f32, sp: f32) -> bool {
        if let Some((r, c)) = self.hit_at(mx, my, sp) {
            unsafe {
                fr = r as c_int;
                fc = c as c_int;
            }
            return true;
        }
        false
    }
    fn wheel(&self, dy: c_int) {
        unsafe {
            let nh = n_hubs() as c_int;
            if dy < 0 && fr < nh - 1 {
                fr += 1;
            } else if dy > 0 && fr > 0 {
                fr -= 1;
            }
            let nc = crate::pms::hub_len(fr.max(0) as usize) as c_int;
            if fc >= nc {
                fc = (nc - 1).max(0);
            }
        }
    }
}

// ---- Home root + the C ABI ----
struct Home {
    snap: Spring,
    bg: Backdrop,
    hero: Hero,
    grid: Grid,
}
impl Home {
    fn new() -> Self {
        Home { snap: Spring::at(0.0), bg: Backdrop::new(), hero: Hero::new(), grid: Grid::new() }
    }
    fn env(&self, dt: f32) -> Env {
        let sp = self.snap.pos;
        // clamp the focus into the current hub bounds so a stray write degrades to a
        // valid shelves[fr]/cards[fc] index rather than reading out of range.
        let nh = n_hubs().max(1) as c_int;
        let cfr = g_fr().clamp(0, nh - 1);
        let ncols = crate::pms::hub_len(cfr as usize).max(1) as c_int;
        let cfc = g_fc().clamp(0, (ncols - 1).min(MAX_ITEMS as c_int - 1));
        Env { dt, screen: Rect::FULL, fr: cfr, fc: cfc, sp, hero_a: hero_alpha(sp, 0.55) }
    }
}
impl View for Home {
    // Compose the tree: flat backdrop, the hero group faded as one under p.alpha(hero_a), then the
    // grid. The grid's layout (base_y, &mut) is done by the home_draw wrapper just before this, and
    // snap-step + grid.update happen in home_update — the wrapper owns those because building `env`
    // depends on the freshly-stepped snap, which View::update/draw can't sequence.
    fn draw(&self, env: &Env, p: Painter) {
        use crate::ui::profile::phase;
        phase("hm.backdrop", || self.bg.draw(env, p));
        if env.hero_a > 0.01 {
            phase("hm.hero", || self.hero.draw(env, p.alpha(env.hero_a)));
        }
        phase("hm.grid", || self.grid.draw(env, p));
    }
}

static mut SCENE: Option<Home> = None;
#[inline]
fn scene() -> &'static mut Home {
    unsafe { (*addr_of_mut!(SCENE)).as_mut().expect("home_init not called") }
}

pub(crate) fn home_init() {
    guard(|| unsafe { *addr_of_mut!(SCENE) = Some(Home::new()) });
}

pub(crate) fn home_update(dt: f32) {
    guard(|| {
        let h = scene();
        unsafe {
            let cd = addr_of!(hero_flip_cd).read();
            if cd > 0.0 {
                addr_of_mut!(hero_flip_cd).write((cd - dt).max(0.0));
            }
            // slide transition: step the progress spring, drop the outgoing layer once the travel
            // left is sub-pixel (see HERO_SLIDE_REST_PX) — and land the spring exactly on 1.0 so
            // the retiring frame and the first idle frame draw the same pixels.
            if addr_of!(hero_prev).read() >= 0 {
                let sl = &mut *addr_of_mut!(hero_slide);
                sl.step(1.0, K_SLIDE, dt);
                if (1.0 - sl.pos).abs() * SCR_W < HERO_SLIDE_REST_PX {
                    sl.jump(1.0);
                    addr_of_mut!(hero_prev).write(-1);
                }
            }
            // idle auto-flip: only while the billboard is actually showing; any flip (manual or
            // this one) resets the countdown inside hero_flip.
            if h.snap.pos < 0.05 && crate::pms::hero_pool_len() > 1 {
                let a = addr_of!(hero_auto).read() - dt;
                addr_of_mut!(hero_auto).write(a);
                if a <= 0.0 {
                    hero_flip(1);
                }
            } else {
                addr_of_mut!(hero_auto).write(HERO_AUTO_S);
            }
        }
        let target = unsafe { addr_of!(snapTarget).read() };
        h.snap.step(target, K_SNAP, dt);
        // the profile chip's unfurl (stepped here, drawn from `.pos` — home_draw runs at dt=0)
        unsafe {
            (*addr_of_mut!(chip_expand)).step(if chip_focused() { 1.0 } else { 0.0 }, K_CHIP, dt)
        };
        let env = h.env(dt);
        h.grid.update(&env);
    });
}

pub(crate) fn home_draw() {
    guard(|| {
        crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        let h = scene();
        let env = h.env(0.0);
        h.grid.layout(env.screen, &env); // &mut layout before the &self composite draw
        h.draw(&env, Painter::root());
        draw_chip(Painter::root());
        // the centered library tab pills (shared with the Library screen): Home is the selected
        // tab here; a pill holds focus when the hero top band is on it.
        let pf = hero_pill_index(hero_focus()).map(|i| i as c_int).unwrap_or(-1);
        crate::ui::widgets::draw_tab_row(Painter::root(), 0, pf);
    });
}

/// Is the chip the focused thing? It is a real focus stop (UP from the hero action row), but only
/// while the billboard is showing — the grid view has no top-band focus. The ONE predicate the
/// unfurl spring's target and any future chip state read.
fn chip_focused() -> bool {
    hero_focus() == -1 && snap_pos() < 0.5
}

/// The top-left profile chip — the shared [`widgets::profile_chip`] visual. Records its rect for
/// pointer hit-testing; a click or UP-in-hero opens the account menu (change profile / sign out).
/// Focus is handed over as the spring's live position, so the chip unfurls its name capsule.
fn draw_chip(p: Painter) {
    let d = crate::ui::widgets::CHIP_D;
    let r = Rect::new(MARGIN_X, crate::ui::widgets::TOP_BAR_Y, d, d);
    unsafe { addr_of_mut!(profile_chip).write(r) };
    crate::ui::widgets::profile_chip(p, r, unsafe { addr_of!(chip_expand).read() }.pos);
}

/// Pointer hit-test on the profile chip (returns true so the caller opens the account menu).
pub(crate) fn profile_chip_click(mx: f32, my: f32) -> bool {
    unsafe { addr_of!(profile_chip).read() }.contains(mx, my)
}

pub(crate) fn home_move_focus(sym: c_uint) {
    guard(|| {
        let s = scene();
        let sp = s.snap.pos; // read once, passed down — hit/vert must see the DRAWN scroll
        s.grid.nav(sym, sp);
    });
}

/// Hero-view horizontal key: LEFT/RIGHT walk the action-row focus (pill → info → chevron); RIGHT
/// on the chevron pages the billboard forward and LEFT on the pill (the row's left end) pages it
/// BACK — so holding either edge key keeps paging (the debounced `hero_flip` throttles it), the
/// D-pad counterpart of holding a click on the chevron. app.rs calls this only while the snap is
/// in hero view; non-arrow keys are no-ops. The chip (focus -1) has no horizontal neighbours.
pub(crate) fn home_hero_key(sym: c_uint) {
    let f = hero_focus();
    match sym {
        SDLK_RIGHT if f >= 0 => {
            if f < HERO_NBTN as c_int - 1 {
                set_hero_focus(f + 1);
            } else {
                hero_flip(1);
            }
        }
        // top band (chip + library pills): RIGHT walks chip → Movies → TV Shows (more negative =
        // further right per the -(i+1) encoding); LEFT walks back to the chip.
        SDLK_RIGHT if f < 0 => set_hero_focus(f - 1), // set_hero_focus clamps at the last pill
        SDLK_LEFT if f < -1 => set_hero_focus(f + 1),
        SDLK_LEFT if f > 0 => set_hero_focus(f - 1),
        SDLK_LEFT if f == 0 => hero_flip(-1),
        _ => {}
    }
}

pub(crate) fn home_pointer_focus(mx: f32, my: f32) {
    guard(|| {
        let s = scene();
        let sp = s.snap.pos;
        s.grid.hit_test(mx, my, sp);
    });
}

/// Pointer click on a grid card: focus it and report the hit, so app.rs can run the SAME
/// activation as OK (play / open detail). Uses hit_at's visibility rules — a click on a
/// half-visible row is ignored rather than scrolling the page.
pub(crate) fn home_card_click(mx: f32, my: f32) -> bool {
    let mut hit = false;
    guard(|| {
        let s = scene();
        let sp = s.snap.pos;
        hit = s.grid.hit_test(mx, my, sp);
    });
    hit
}

pub(crate) fn home_wheel(dy: c_int) {
    guard(|| scene().grid.wheel(dy));
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The home screen's focus lives in `static mut fr`/`fc`, so tests that drive it must not
    /// run concurrently. (This is the cost the audit flagged: module-singleton state makes host
    /// tests order-dependent. One lock is cheaper than reshaping the screen right now.)
    static FOCUS: Mutex<()> = Mutex::new(());

    #[test]
    fn n_hubs_clamps_the_server_count_to_the_shelf_array() {
        assert_eq!(n_hubs_of(0), 0);
        assert_eq!(n_hubs_of(3), 3);
        assert_eq!(n_hubs_of(MAX_HUBS), MAX_HUBS);
        // A server with 3-4 libraries returns well over 16 hubs (/hubs promotes several rows
        // per library on top of continue/ondeck/recentlyAdded). `shelves` is a fixed [_; 16].
        assert_eq!(n_hubs_of(MAX_HUBS + 1), MAX_HUBS);
        assert_eq!(n_hubs_of(200), MAX_HUBS);
    }

    /// Regression: the top band packed pill `i` as `-(i+1)`, so Home (pill 0) landed on -1 — the
    /// profile chip's value. `hero_pill_index` rejects anything above -2, so the Home pill was
    /// unreachable by D-pad and could never wear the focused (white) treatment on its own screen.
    /// The packing now round-trips for EVERY pill the row can draw, Home included.
    #[test]
    fn every_tab_pill_round_trips_through_the_focus_packing() {
        for i in 0..crate::ui::widgets::MAX_TABS {
            let f = hero_focus_for_pill(i);
            assert_ne!(f, -1, "pill {i} must not alias the profile chip");
            assert_eq!(hero_pill_index(f), Some(i), "pill {i} must decode back to itself");
        }
        assert_eq!(hero_pill_index(-1), None, "the profile chip is not a pill");
        assert_eq!(hero_pill_index(0), None, "the hero Play pill is not a tab pill");
        assert_eq!(hero_pill_index(c_int::MIN), None, "the grid-card sentinel is not a pill");
    }

    #[test]
    fn step_row_stays_inside_the_addressable_rows() {
        assert_eq!(step_row(0, 1, 5), 1);
        assert_eq!(step_row(4, 1, 5), 4, "DOWN on the last row stays put");
        assert_eq!(step_row(0, -1, 5), 0, "UP on the first row stays put");
        assert_eq!(step_row(9, 1, 5), 4, "a stale focus is pulled back into range");
        assert_eq!(step_row(0, 1, 0), 0, "an empty catalog has no row to move to");
    }

    /// Regression: with more hubs than the fixed `shelves: [CardRow; 16]` array, a DOWN press on
    /// row 15 computed `g_fr() + dir == 16` and indexed the array out of bounds — panicking on
    /// the KEYPRESS, before `draw` was ever reached. `Home::env` clamps the value it *returns*
    /// but never writes it back, so the raw global kept climbing.
    #[test]
    fn vert_cannot_walk_past_the_shelf_array() {
        let _g = FOCUS.lock().unwrap_or_else(|e| e.into_inner());
        let grid = Grid::new();
        set_row(MAX_HUBS as c_int - 1); // 15 — the last shelf the array can hold
        unsafe { grid.vert(1, 1.0) };
        assert!(
            row() < MAX_HUBS as c_int,
            "vert() walked to row {} with only {MAX_HUBS} shelves",
            row()
        );
        set_row(0);
    }

    /// Regression: hit_at re-derived the card x WITHOUT the `* sp` snap fold the draw applies
    /// (`- scroll_x()` vs `- scroll_x() * env.sp`), so mid-snap the hover target and the drawn
    /// card disagreed by `scroll_x * (1 - sp)`. Both now go through card_x/col_at with ONE
    /// effective scroll — this pins the round-trip at every snap phase.
    #[test]
    fn pointer_hit_column_matches_the_drawn_card_at_every_snap_phase() {
        for &scroll in &[0.0f32, 415.0, 830.0] {
            for &sp in &[0.0f32, 0.37, 1.0] {
                let es = scroll * sp; // eff_scroll's fold, applied to both draw and hit
                for c in 0..24usize {
                    let center = card_x(c, es) + CARD_W * 0.5; // the drawn card's center
                    assert_eq!(
                        col_at(center, es, 24),
                        Some(c),
                        "clicking the center of drawn card {c} (scroll={scroll}, sp={sp}) must hit it"
                    );
                }
            }
        }
    }
}
