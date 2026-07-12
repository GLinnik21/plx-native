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
use crate::ui::widgets::{cfield, resolve_tex, Art, Button, CircleButton, ControlStyle, PageDots};
use crate::ui::{hero_alpha, on_axis, Env, Painter, Rect, Spring, View};
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uint};
use std::ptr::{addr_of, addr_of_mut};

// ---- focus state: private main-thread module state. Home internals read/write these
// directly; app.rs (the only outside reader) reaches them through the accessors below. ----
static mut fr: c_int = 0;
static mut fc: c_int = 0;
static mut snapTarget: f32 = 0.0;
// rotating hero: current index into pms::hero_pool + a flip debounce (so a held LEFT/RIGHT, via the
// key-repeat path, can't machine-gun the carousel) + the on-screen chevron rect for pointer clicks.
static mut hero_idx: c_int = 0;
static mut hero_flip_cd: f32 = 0.0;
static mut hero_chevron: [f32; 4] = [0.0; 4]; // x, y, w, h
const HERO_FLIP_CD: f32 = 0.35;
// top-left profile chip (avatar) rect, recorded each draw for pointer hit-testing (opens the
// account menu). See draw_chip / profile_chip_click.
static mut profile_chip: [f32; 4] = [0.0; 4];

pub(crate) fn row() -> c_int {
    unsafe { addr_of!(fr).read() }
}
pub(crate) fn col() -> c_int {
    unsafe { addr_of!(fc).read() }
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

/// the item at (hub row, column) in the home hub grid, or null
pub(crate) fn movie_at(r: c_int, c: c_int) -> *mut PmsMovie {
    if r < 0 || c < 0 {
        return std::ptr::null_mut();
    }
    crate::pms::hub_item_ptr(r as usize, c as usize)
}

/// The currently-shown rotating-hero item (curated pool: Continue Watching then Recently Added),
/// falling back to the first catalog item when the pool is empty. Backdrop, Hero and the home OK
/// handler all read the hero through this so they never disagree on which item is featured.
pub(crate) fn hero_item() -> *mut PmsMovie {
    let n = crate::pms::hero_pool_len();
    if n == 0 {
        return movie_at(0, 0);
    }
    let i = unsafe { addr_of!(hero_idx).read() }.clamp(0, n as c_int - 1);
    crate::pms::hero_pool_ptr(i as usize)
}

/// dev/test hook: jump the hero to a specific pool index (the `poc-heroidx` trigger) so a flipped
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

/// Advance the hero to the next (`dir=+1`) / previous (`dir=-1`) pooled item, wrapping. Debounced
/// by `hero_flip_cd` so a held edge-key (via the repeat path) flips a few times a second, not every
/// frame — the same machine-gun guard the season tabs needed.
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
        addr_of_mut!(hero_idx).write((cur + dir).rem_euclid(n));
        addr_of_mut!(hero_flip_cd).write(HERO_FLIP_CD);
    }
}

/// Pointer hit-test on the hero flip chevron (valid while the hero is visible). Flips forward and
/// returns true when the click lands on it, so the caller can swallow the click.
pub(crate) fn hero_chevron_click(mx: f32, my: f32) -> bool {
    let r = unsafe { addr_of!(hero_chevron).read() };
    if mx >= r[0] && mx <= r[0] + r[2] && my >= r[1] && my <= r[1] + r[3] {
        hero_flip(1);
        return true;
    }
    false
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
        let hero = unsafe { hero_item().as_ref() };
        // flat dark-gray base — the shelves sit on this (so card focus shadows read),
        // NOT the hero's ambient wash. Only the hero itself carries a backdrop.
        let g = theme::SURFACE_APP;
        p.rect(env.screen, 0.0, g, g, 0.0);
        let mut bt = 0u32;
        if let Some(h) = hero {
            if h.art[0] != 0 {
                bt = crate::ui::widgets::resolve_tex(h.art.as_ptr() as *const c_char, 1280, 720, 0);
            }
            // hero backdrop: art if present, else the ambient wash as a fallback — both
            // confined to the hero view, fading out as the grid rises so the shelf area
            // stays flat gray
            if bt != 0 && sp < 0.996 {
                p.tex(bt, Rect::new(0.0, -sp * (SCR_H - 120.0), SCR_W, SCR_H), 0.0, [1.0, 1.0, 1.0, 1.0 - sp]);
            } else if bt == 0 && h.has_blur != 0 && sp < 0.996 {
                p.ambient(env.screen, 0.55 * (1.0 - sp), h.blur);
            }
        }
        if env.hero_a > 0.01 {
            let sa = 0.30 + 0.64 * env.hero_a;
            p.rect(Rect::new(0.0, SCR_H * 0.46, SCR_W, SCR_H * 0.54), 0.0,
                theme::scrim(0.0), theme::scrim(sa), 0.0);
        }
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
impl View for Hero {
    fn draw(&self, env: &Env, p: Painter) {
        let Some(hero) = (unsafe { hero_item().as_ref() }) else {
            return;
        };
        let tx = MARGIN_X;
        let col_w = 660.0f32; // hero text column
        let w_a = theme::TEXT_PRIMARY; // cascade applies hero_a
        let d_a = theme::TEXT_SECONDARY;

        // The hero content is a top-anchored vertical flow: title band → meta → synopsis →
        // action row → page dots. Every gap is a `theme::space` rung and every step advances by
        // the *measured* height of the element just drawn, so nothing is glued and the rhythm
        // holds whether the title renders as a tall logo, a short one, or fallback text.
        let mut y = 440.0f32;

        // title: clearLogo (transparent PNG) if loaded, else bold HERO text
        let rk = if hero.rk[0] != 0 { cfield(&hero.rk) } else { String::new() };
        if let Some((lt, ww, hh)) = crate::posters::logo_tex(&rk, col_w, 96.0) {
            p.tex(lt, Rect::new(tx, y, ww, hh), 0.0, w_a);
            y += hh;
        } else {
            let title = cfield(&hero.title);
            let tv = TextView::new(&title, theme::size::HERO, w_a).bold().max_lines(1);
            tv.draw(p, Rect::new(tx, y, col_w, 0.0));
            y += tv.measure_h(col_w);
        }

        // meta line — "Movie · YEAR · RATING"
        let rating = cfield(&hero.rating);
        let meta = format!("Movie \u{b7} {} \u{b7} {}", hero.year, if rating.is_empty() { "NR" } else { &rating });
        let meta_tv = TextView::new(&meta, theme::size::BODY, d_a).max_lines(1);
        y += theme::space::MD;
        meta_tv.draw(p, Rect::new(tx, y, col_w, 0.0));
        y += meta_tv.measure_h(col_w);

        // synopsis — pixel-wrapped to the hero column, 2 lines max
        let summary = cfield(&hero.summary);
        if !summary.is_empty() {
            let syn = TextView::new(&summary, theme::size::BODY, d_a).leading(34.0).max_lines(2);
            y += theme::space::SM;
            syn.draw(p, Rect::new(tx, y, col_w, 0.0));
            y += syn.measure_h(col_w);
        }

        // action row: the primary pill hugs its label, then the +/i/chevron circles reflow after
        // it (cascade scales their base alphas). The pill says "Continue" when the hero item has a
        // resume point (mirrors the official app), else "Play" — same play glyph for both.
        y += theme::space::LG;
        let pill_y = y;
        let (cd, cgap) = (60.0f32, 20.0f32); // control diameter + inter-control gap
        let plabel = if hero.resume_ms > 0 { c"Continue" } else { c"Play" };
        let isz = theme::size::BODY as f32 * 1.15; // icon box (mirrors Button's own layout)
        let pw = isz + 12.0 + crate::text::text_width(plabel.as_ptr(), theme::size::BODY, 1) + 68.0;
        Button::new(plabel.as_ptr(), theme::size::BODY, Rect::new(tx, pill_y, pw, cd))
            .icon(Icon::Play)
            .style(ControlStyle::Primary)
            .draw(env, p);
        let mut cx = tx + pw + cgap;
        // +/i are text glyphs (both symmetric, read fine); the forward affordance is a real chevron
        // icon (the ">" character otherwise renders as thin, mis-centred math punctuation).
        CircleButton::new(c"+".as_ptr()).at(cx, pill_y).draw(env, p);
        cx += cd + cgap;
        CircleButton::new(c"i".as_ptr()).at(cx, pill_y).draw(env, p);
        cx += cd + cgap;
        // record the chevron rect so a pointer click on it flips the hero (see hero_chevron_click)
        unsafe { addr_of_mut!(hero_chevron).write([cx, pill_y, cd, cd]) };
        CircleButton::new(c"".as_ptr()).icon(Icon::Chevron).at(cx, pill_y).draw(env, p);

        // page indicator: one dot per pooled hero item, the current one lit
        let pool_n = crate::pms::hero_pool_len();
        if pool_n > 1 {
            y = pill_y + cd + theme::space::MD;
            PageDots::new(pool_n).active(hero_index()).at(tx, y).draw(env, p);
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
        let want_y = (env.fr as f32 * ROW_PITCH - ROW_PITCH * 0.6).clamp(0.0, max_y);
        self.scroll_y.step(want_y, K_SCROLL, env.dt);
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
            // hub title above the row; it rises as the row's focused card magnifies so
            // the title-to-card gap stays proportional (Apple-TV behavior).
            if env.sp > 0.02 {
                let fs = if r == env.fr as usize {
                    self.shelves[r].scale((env.fc as usize).min(MAX_ITEMS - 1))
                } else {
                    1.0
                };
                let lift = CARD_H * (fs - 1.0) * 0.5;
                if let Ok(t) = CString::new(crate::pms::hub_title(r)) {
                    p.text(t.as_ptr(), MARGIN_X, row_y - 34.0 - lift, theme::size::HEADLINE, theme::with_a(theme::TEXT_PRIMARY, env.sp), 0, 1);
                }
            }
            for c in 0..crate::pms::hub_len(r) {
                if r == env.fr as usize && c == env.fc as usize && env.sp > 0.5 {
                    continue; // focused card drawn last (grid z-order)
                }
                let m = unsafe { movie_at(r as c_int, c as c_int).as_ref() };
                let Some(mm) = m else { continue };
                let x = MARGIN_X + c as f32 * (CARD_W + GAP) - self.shelves[r].scroll_x() * env.sp;
                if !on_axis(x, CARD_W, SCR_W, GLOW_PAD) {
                    continue;
                }
                let s = self.shelves[r].scale(c);
                let rect = Rect::new(x, row_y + 12.0, CARD_W, CARD_H).scaled(s);
                card_row::draw_tile(p, Art::Poster(m), rect, s, &RowStyle::HOME, resume_frac(mm));
            }
        }
        // PASS 2 — the single focused card + ring + title, drawn LAST for cross-row z-order (grid mode).
        if env.sp > 0.5 {
            let (r, c) = (env.fr as usize, env.fc as usize);
            if r >= nh {
                return;
            }
            let s = self.shelves[r].scale(c.min(MAX_ITEMS - 1));
            let x = MARGIN_X + c as f32 * (CARD_W + GAP) - self.shelves[r].scroll_x() * env.sp;
            let rect = Rect::new(x, self.shelves[r].base_y + 12.0, CARD_W, CARD_H).scaled(s);
            let m = unsafe { movie_at(r as c_int, c as c_int).as_ref() };
            let title = m.map(|mm| mm.title.as_ptr() as *const c_char).unwrap_or(std::ptr::null());
            card_row::draw_focused(p, Art::Poster(m), rect, s, &RowStyle::HOME, m.and_then(resume_frac), title);
        }
    }
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
    fn hit_test(&self, mx: f32, my: f32) {
        unsafe {
            for r in 0..crate::pms::hub_count() {
                let row_y = self.shelves[r].base_y;
                if my < row_y || my > row_y + CARD_H {
                    continue;
                }
                for c in 0..crate::pms::hub_len(r) {
                    let x = MARGIN_X + c as f32 * (CARD_W + GAP) - self.shelves[r].scroll_x();
                    if mx >= x && mx <= x + CARD_W {
                        fr = r as c_int;
                        fc = c as c_int;
                    }
                }
            }
        }
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
    static mut CHIP: Option<(u32, CString, CString)> = None; // (gen, thumb path, initial)
    let d = 64.0f32;
    let (x, y) = (MARGIN_X, 44.0f32);
    unsafe { addr_of_mut!(profile_chip).write([x, y, d, d]) };
    let r = Rect::new(x, y, d, d);
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
        *chip = Some((
            gen,
            CString::new(thumb).unwrap_or_default(),
            CString::new(initial).unwrap_or_default(),
        ));
    }
    let (_, thumb_c, initial_c) = chip.as_ref().unwrap();
    let mut drew = false;
    if !thumb_c.as_bytes().is_empty() {
        let t = resolve_tex(thumb_c.as_ptr(), 128, 128, 0);
        if t != 0 {
            p.tex(t, r, d * 0.5, theme::TINT_WHITE);
            drew = true;
        }
    }
    if !drew {
        p.rect(r, d * 0.5, theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_FILL, 0.0);
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
    let r = unsafe { addr_of!(profile_chip).read() };
    mx >= r[0] && mx <= r[0] + r[2] && my >= r[1] && my <= r[1] + r[3]
}

pub(crate) fn home_move_focus(sym: c_uint) {
    guard(|| scene().grid.nav(sym));
}

/// Hero-view horizontal key: LEFT/RIGHT page the rotating billboard — the D-pad counterpart of the
/// chevron click (`hero_flip` is debounced, so holding the key pages a few times a second). app.rs
/// calls this only while the snap is in hero view; non-arrow keys are no-ops.
pub(crate) fn home_hero_key(sym: c_uint) {
    match sym {
        SDLK_RIGHT => hero_flip(1),
        SDLK_LEFT => hero_flip(-1),
        _ => {}
    }
}

pub(crate) fn home_pointer_focus(mx: f32, my: f32) {
    guard(|| scene().grid.hit_test(mx, my));
}

pub(crate) fn home_wheel(dy: c_int) {
    guard(|| scene().grid.wheel(dy));
}
