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
use crate::ui::widgets::{resolve_tex, Art, Button, CircleButton, PageDots};
use crate::ui::{hero_alpha, on_axis, Env, Painter, Rect, Spring, View};
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
const K_SLIDE: f32 = 130.0; // slide spring — a touch softer than the grid springs, reads cinematic
// top-left profile chip (avatar) rect, recorded each draw for pointer hit-testing (opens the
// account menu). See draw_chip / profile_chip_click.
static mut profile_chip: Rect = Rect::new(0.0, 0.0, 0.0, 0.0);

/// Focus accessors clamp INTO THE LIVE HUB BOUNDS at read time (mirroring Home::env's per-frame
/// clamp): a hub refetch can shrink the shelves underneath the raw statics, and the OK dispatch
/// reads these — an unclamped stale index made OK silently no-op on a visibly focused card.
pub(crate) fn row() -> c_int {
    let nh = crate::pms::hub_count() as c_int;
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
pub(crate) fn hero_focus() -> c_int {
    unsafe { addr_of!(hero_fc).read() }
}
pub(crate) fn set_hero_focus(v: c_int) {
    unsafe { addr_of_mut!(hero_fc).write(v.clamp(-1, HERO_NBTN as c_int - 1)) }
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

/// played fraction (0..1) for a Continue-Watching card's resume bar, or None if not in progress.
/// The bar itself is drawn by `card_row::draw_tile`/`draw_focused`; this just supplies the frac.
fn resume_frac(m: &PmsMovie) -> Option<f32> {
    (m.resume_ms > 0 && m.dur_ns > 0).then(|| (m.resume_ms as f32 * 1_000_000.0 / m.dur_ns as f32).clamp(0.0, 1.0))
}

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
            let sa = 0.30 + 0.64 * env.hero_a;
            p.rect(Rect::new(0.0, SCR_H * 0.46, SCR_W, SCR_H * 0.54), 0.0,
                theme::scrim(0.0), theme::scrim(sa), 0.0);
        }
    }
}

/// One hero item's backdrop layer at horizontal slide offset `dx`: the 1280×720 art (with the
/// grid-rise parallax/fade) or the ambient wash while the art hasn't resolved.
fn backdrop_art(p: Painter, item: Option<&PmsMovie>, sp: f32, dx: f32) {
    let Some(h) = item else {
        return;
    };
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
fn hero_content(hero: &PmsMovie, p: Painter) {
    let tx = MARGIN_X;
    let col_w = 660.0f32; // hero text column
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

impl View for Hero {
    fn draw(&self, env: &Env, p: Painter) {
        let Some(hero) = hero_item() else {
            return;
        };
        let tx = MARGIN_X;

        // per-item content, sliding during a flip (same phase/direction as the backdrop art)
        if let Some((prev, dx_out, dx_in)) = hero_slide_state() {
            if let Some(ph) = hero_item_at(prev) {
                hero_content(ph, p.translate(dx_out, 0.0));
            }
            hero_content(hero, p.translate(dx_in, 0.0));
        } else {
            hero_content(hero, p);
        }

        // action row — chrome, does NOT slide, and sits at a FIXED y (the text column above is
        // bottom-anchored, so the button-to-text air is one MD for every item). Pill + info +
        // chevron are a real focus row (hero_fc), so LEFT/RIGHT walk buttons instead of paging;
        // the chevron is the pager. The pill says "Continue" when the hero item has a resume
        // point, else "Play", and launches playback directly (the info circle is the road to the
        // detail page). MD, not LG: the synopsis' leading box already carries ~7px of descender
        // slack, and the bigger rung read as the button drifting away from its text.
        let pill_y = HERO_TEXT_BOTTOM + theme::space::MD;
        let hf = hero_focus();
        let (cd, cgap) = (60.0f32, 20.0f32); // control diameter + inter-control gap
        let plabel = if hero.resume_ms > 0 { c"Continue" } else { c"Play" };
        let isz = theme::size::BODY as f32 * 1.15; // icon box (mirrors Button's own layout)
        let pw = isz + 12.0 + crate::text::text_width(plabel.as_ptr(), theme::size::BODY, 1) + 68.0;
        unsafe { (*addr_of_mut!(hero_btns))[0] = Rect::new(tx, pill_y, pw, cd) };
        Button::new(plabel.as_ptr(), theme::size::BODY, Rect::new(tx, pill_y, pw, cd))
            .icon(Icon::Play)
            .focused(hf == 0)
            .draw(env, p);
        let mut cx = tx + pw + cgap;
        unsafe { (*addr_of_mut!(hero_btns))[1] = Rect::new(cx, pill_y, cd, cd) };
        CircleButton::new(c"".as_ptr()).icon(Icon::Info).at(cx, pill_y).focused(hf == 1).draw(env, p);
        cx += cd + cgap;
        unsafe { (*addr_of_mut!(hero_btns))[2] = Rect::new(cx, pill_y, cd, cd) };
        CircleButton::new(c"".as_ptr()).icon(Icon::Chevron).at(cx, pill_y).focused(hf == 2).draw(env, p);

        // page indicator: one dot per pooled hero item, the current one lit. SM keeps the dots
        // visually grouped with the action row above — at MD they hovered midway to the peeking
        // shelf and read as stuck to the poster row.
        let pool_n = crate::pms::hero_pool_len();
        if pool_n > 1 {
            PageDots::new(pool_n).active(hero_index()).at(tx, pill_y + cd + theme::space::SM).draw(env, p);
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
        for r in 0..MAX_HUBS {
            let focused = (env.fr as usize == r).then_some(env.fc as usize);
            self.shelves[r].update(crate::pms::hub_len(r), focused, &RowStyle::HOME, env.dt);
        }
        let nh = crate::pms::hub_count().max(1);
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
        let nh = crate::pms::hub_count();
        // PASS 1 — every shelf's non-focused cells (the globally-focused cell is skipped in grid mode,
        // drawn LAST below so it overlaps neighbouring rows: cross-row z-order, invariant #3).
        for r in 0..nh {
            let row_y = self.shelves[r].base_y;
            if !on_axis(row_y, CARD_H, SCR_H, 0.0) {
                continue;
            }
            // hub title above the row — rises with the magnified card so it's never covered
            // (the shared card_row::title_lift rule; only lifts when the popped card is under it)
            if env.sp > 0.02 {
                let focused = (r == env.fr as usize).then_some((env.fc as usize).min(MAX_ITEMS - 1));
                let lift = card_row::title_lift(&self.shelves[r], focused, &RowStyle::HOME, self.shelves[r].scroll_x() * env.sp);
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
                let x = MARGIN_X + c as f32 * (CARD_W + GAP) - self.shelves[r].scroll_x() * env.sp;
                if !on_axis(x, CARD_W, SCR_W, GLOW_PAD) {
                    continue;
                }
                let s = self.shelves[r].scale(c);
                let rect = Rect::new(x, row_y + CARD_DY, CARD_W, CARD_H).scaled(s);
                let resume = resume_frac(mm);
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
            let x = MARGIN_X + c as f32 * (CARD_W + GAP) - self.shelves[r].scroll_x() * env.sp;
            let rect = Rect::new(x, self.shelves[r].base_y + CARD_DY, CARD_W, CARD_H).scaled(s);
            let m = movie_at(r as c_int, c as c_int);
            let cw = crate::pms::hub_is_continue(r); // Continue Watching: amber ▶ + "show · X min left"
            // keep the CStrings alive through the draw
            let title_c = m.and_then(|mm| CString::new(mm.title.as_str()).ok());
            let title = title_c.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
            let caption = m.and_then(|mm| if cw { cw_caption(mm) } else { focused_caption(mm) });
            let cap_ptr = caption.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
            let resume = m.and_then(resume_frac);
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
impl Grid {
    fn new() -> Self {
        Grid { shelves: [CardRow::new(); MAX_HUBS], scroll_y: Spring::at(0.0) }
    }
    // ---- navigation: writes the fr/fc globals (never caches focus) ----
    fn nav(&self, sym: c_uint) {
        unsafe {
            let nh = crate::pms::hub_count() as c_int;
            let nc = crate::pms::hub_len(fr.max(0) as usize) as c_int;
            if sym == SDLK_LEFT && fc > 0 {
                fc -= 1;
            } else if sym == SDLK_RIGHT && fc < nc - 1 {
                fc += 1;
            } else if sym == SDLK_UP && fr > 0 {
                self.vert(-1);
            } else if sym == SDLK_DOWN && fr < nh - 1 {
                self.vert(1);
            }
        }
    }
    /// vertical move keeping VISUAL column alignment across rows' animated scroll
    unsafe fn vert(&self, dir: c_int) {
        let (cur, ncur) = (g_fr(), g_fr() + dir);
        let cx = MARGIN_X + g_fc() as f32 * (CARD_W + GAP) - self.shelves[cur as usize].scroll_x() + CARD_W * 0.5;
        let mut nc =
            ((cx - MARGIN_X - CARD_W * 0.5 + self.shelves[ncur as usize].scroll_x()) / (CARD_W + GAP) + 0.5) as c_int;
        let ncount = crate::pms::hub_len(ncur as usize) as c_int;
        nc = nc.clamp(0, (ncount - 1).max(0));
        fr = ncur;
        fc = nc;
    }
    /// Card under the pointer, or None. Vertical fly-away guard: a row that is only PARTIALLY on
    /// screen is not hoverable unless it is already the focused row — hovering it would move `fr`
    /// and the page spring would chase it (vertical auto-scroll), which is exactly the "pointer
    /// flies away" the pointer rules ban; horizontal scroll-into-view within a row is kept.
    fn hit_at(&self, mx: f32, my: f32) -> Option<(usize, usize)> {
        for r in 0..crate::pms::hub_count() {
            let row_y = self.shelves[r].base_y + CARD_DY;
            if my < row_y || my > row_y + CARD_H {
                continue;
            }
            let fully_visible = row_y >= 40.0 && row_y + CARD_H <= SCR_H - 20.0;
            if !fully_visible && r != g_fr() as usize {
                continue;
            }
            for c in 0..crate::pms::hub_len(r) {
                let x = MARGIN_X + c as f32 * (CARD_W + GAP) - self.shelves[r].scroll_x();
                if mx >= x && mx <= x + CARD_W {
                    return Some((r, c));
                }
            }
        }
        None
    }
    /// hover/click focus write: focus the card under the pointer; reports whether one was hit
    fn hit_test(&self, mx: f32, my: f32) -> bool {
        if let Some((r, c)) = self.hit_at(mx, my) {
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
            let nh = crate::pms::hub_count() as c_int;
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
        let nh = crate::pms::hub_count().max(1) as c_int;
        let cfr = g_fr().clamp(0, (nh - 1).min(MAX_HUBS as c_int - 1));
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

/// Run an entry-point body panic-guarded so a stray panic degrades to a skipped
/// frame instead of unwinding into C (matches img/mkv/pms). Main-thread-only.
#[inline]
fn guard(f: impl FnOnce()) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
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
            // slide transition: step the progress spring, drop the outgoing layer once landed
            if addr_of!(hero_prev).read() >= 0 {
                let sl = &mut *addr_of_mut!(hero_slide);
                sl.step(1.0, K_SLIDE, dt);
                if sl.pos > 0.995 {
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
    });
}

/// The top-left profile chip — the signed-in avatar (or an initial fallback). Records its rect for
/// pointer hit-testing; a click or UP-in-hero opens the account menu (change profile / sign out).
/// The session lookup (mutex + 5-String UserRef clone) is snapshotted per profile GENERATION —
/// re-cloning it every frame for a chip that changes only on a profile switch was waste.
fn draw_chip(p: Painter) {
    static mut CHIP: Option<(u32, String, CString)> = None; // (gen, thumb path, initial)
    let d = 64.0f32;
    let (x, y) = (MARGIN_X, 44.0f32);
    let r = Rect::new(x, y, d, d);
    unsafe { addr_of_mut!(profile_chip).write(r) };
    let gen = crate::plex::session::current_gen();
    let chip = unsafe { &mut *addr_of_mut!(CHIP) };
    if chip.as_ref().map(|c| c.0 != gen).unwrap_or(true) {
        let cur = crate::plex::session::current();
        let thumb = cur.as_ref().map(|u| u.thumb.clone()).unwrap_or_default();
        let initial = cur
            .as_ref()
            .and_then(|u| u.title.chars().next())
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        *chip = Some((gen, thumb, CString::new(initial).unwrap_or_default()));
    }
    let (_, thumb_s, initial_c) = chip.as_ref().unwrap();
    // chip is a real focus stop (UP from the hero action row) — the same lifted-card focus treatment
    // as the shelf tiles: soft drop-shadow behind the avatar, top sheen over it.
    let focused = hero_focus() == -1 && snap_pos() < 0.5;
    // resting shadow + perimeter stroke always; lift the shadow when focused (same as the shelf tiles)
    p.focus_shadow(r, d * 0.5, if focused { 1.0 } else { 0.0 });
    let mut drew = false;
    if !thumb_s.is_empty() {
        let t = resolve_tex(thumb_s, 128, 128, 0);
        if t != 0 {
            p.tex_stroked(t, r, d * 0.5, theme::TINT_WHITE);
            drew = true;
        }
    }
    if !drew {
        p.rect_sheened(r, d * 0.5, theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_FILL);
        if initial_c.as_bytes().is_empty() {
            // signed out (no session) — a generic person glyph; the menu behind it offers Sign in
            crate::ui::icons::draw(p, crate::ui::icons::Icon::User, r.inset(14.0), theme::TEXT_SECONDARY);
        } else {
            let ty = crate::text::text_vcenter_y(theme::size::HEADLINE, 1, y + d * 0.5);
            p.text(initial_c.as_ptr(), x + d * 0.5, ty, theme::size::HEADLINE, theme::TEXT_PRIMARY, 1, 1);
        }
    }
}

/// Pointer hit-test on the profile chip (returns true so the caller opens the account menu).
pub(crate) fn profile_chip_click(mx: f32, my: f32) -> bool {
    unsafe { addr_of!(profile_chip).read() }.contains(mx, my)
}

pub(crate) fn home_move_focus(sym: c_uint) {
    guard(|| scene().grid.nav(sym));
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
        SDLK_LEFT if f > 0 => set_hero_focus(f - 1),
        SDLK_LEFT if f == 0 => hero_flip(-1),
        _ => {}
    }
}

pub(crate) fn home_pointer_focus(mx: f32, my: f32) {
    guard(|| {
        scene().grid.hit_test(mx, my);
    });
}

/// Pointer click on a grid card: focus it and report the hit, so app.rs can run the SAME
/// activation as OK (play / open detail). Uses hit_at's visibility rules — a click on a
/// half-visible row is ignored rather than scrolling the page.
pub(crate) fn home_card_click(mx: f32, my: f32) -> bool {
    let mut hit = false;
    guard(|| hit = scene().grid.hit_test(mx, my));
    hit
}

pub(crate) fn home_wheel(dy: c_int) {
    guard(|| scene().grid.wheel(dy));
}
