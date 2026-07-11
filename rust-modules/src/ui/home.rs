//! The home screen as a retui tree: Backdrop + Hero + Grid([Shelf;5]×[Card;10]).
//! fr/fc/snapTarget are the focus source of truth (private module state); the tree
//! reads them live each frame via Env and writes back through nav. plex_run drives it
//! through home_init/update/draw/move_focus/pointer_focus/wheel (crate path) and the
//! row/col/snap_target accessors.
#![allow(non_upper_case_globals)]
use crate::pms::PmsMovie;
use crate::ui::consts::*;
use crate::ui::icons::Icon;
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::{cfield, draw_poster, Button, CircleButton, ControlStyle, PageDots};
use crate::ui::{Env, Painter, Rect, Spring, View};
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uint};
use std::ptr::{addr_of, addr_of_mut};

// ---- focus state: private main-thread module state. Home internals read/write these
// directly; app.rs (the only outside reader) reaches them through the accessors below. ----
static mut fr: c_int = 0;
static mut fc: c_int = 0;
static mut snapTarget: f32 = 0.0;

pub(crate) fn row() -> c_int {
    unsafe { addr_of!(fr).read() }
}
pub(crate) fn col() -> c_int {
    unsafe { addr_of!(fc).read() }
}
pub(crate) fn snap_target() -> f32 {
    unsafe { addr_of!(snapTarget).read() }
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
// The Shelf/Grid arrays are sized to these maxima; the *actual* counts come from
// pms::hub_count()/hub_len().
const MAX_HUBS: usize = 16; // Continue Watching, On Deck, Recently Added, collections…
const MAX_ITEMS: usize = 24; // cards per shelf

/// the item at (hub row, column) in the home hub grid, or null
pub(crate) fn movie_at(r: c_int, c: c_int) -> *mut PmsMovie {
    if r < 0 || c < 0 {
        return std::ptr::null_mut();
    }
    crate::pms::hub_item_ptr(r as usize, c as usize)
}

/// resume bar along a card bottom (Continue Watching); no-op if not in progress
fn resume_bar(p: Painter, r: Rect, m: &PmsMovie) {
    if m.resume_ms > 0 && m.dur_ns > 0 {
        let frac = (m.resume_ms as f32 * 1_000_000.0 / m.dur_ns as f32).clamp(0.0, 1.0);
        let bh = 5.0f32;
        let (bx, bw) = (r.x + 8.0, r.w - 16.0);
        let by = r.y + r.h - bh - 8.0;
        p.rrect(Rect::new(bx, by, bw, bh), bh * 0.5, bh * 0.5, theme::RAIL_BUFFERED);
        p.rrect(Rect::new(bx, by, bw * frac, bh), bh * 0.5, bh * 0.5, theme::RESUME_FILL);
    }
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
        let hero = unsafe { movie_at(0, 0).as_ref() };
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
struct Hero {
    play: Button,
    actions: [CircleButton; 3],
    dots: PageDots,
}
impl Hero {
    fn new() -> Self {
        let tx = MARGIN_X;
        let pill_y = 510.0 + 200.0; // titleY + 200
        let (pw, cgap, cd) = (168.0f32, 20.0f32, 60.0f32);
        Hero {
            play: Button::new(c"Play".as_ptr(), 30, Rect::new(tx, pill_y, pw, cd))
                .icon(Icon::Play)
                .style(ControlStyle::Primary),
            actions: [
                CircleButton::new(c"+".as_ptr()).at(tx + pw + cgap, pill_y),
                CircleButton::new(c"i".as_ptr()).at(tx + pw + cgap + (cd + cgap), pill_y),
                CircleButton::new(c">".as_ptr()).at(tx + pw + cgap + 2.0 * (cd + cgap), pill_y),
            ],
            dots: PageDots::new(8).at(tx, pill_y + 60.0 + 24.0),
        }
    }
}
impl View for Hero {
    fn draw(&self, env: &Env, p: Painter) {
        let Some(hero) = (unsafe { movie_at(0, 0).as_ref() }) else {
            return;
        };
        let tx = MARGIN_X;
        let title_y = 510.0f32;
        let w_a = theme::TEXT_PRIMARY; // cascade applies hero_a
        let d_a = theme::TEXT_SECONDARY;
        // title: clearLogo (transparent PNG) if loaded, else bold text
        let mut lt = 0u32;
        let (mut lw, mut lh) = (0i32, 0i32);
        if hero.rk[0] != 0 {
            let rk = cfield(&hero.rk);
            if let Ok(lpath) = CString::new(format!("/library/metadata/{rk}/clearLogo")) {
                let mut lk = [0u8; 352];
                crate::posters::poster_key(lk.as_mut_ptr() as *mut c_char, lk.len(), lpath.as_ptr(), 600, 240, 1);
                lt = crate::posters::poster_get(lk.as_ptr() as *const c_char);
                crate::posters::poster_wh(lk.as_ptr() as *const c_char, &mut lw, &mut lh);
            }
        }
        if lt != 0 && lh > 0 {
            let mut hh = 96.0f32;
            let mut ww = hh * lw as f32 / lh as f32;
            if ww > 660.0 {
                ww = 660.0;
                hh = ww * lh as f32 / lw as f32;
            }
            p.tex(lt, Rect::new(tx, title_y + 80.0 - hh, ww, hh), 0.0, w_a);
        } else {
            p.text(hero.title.as_ptr() as *const c_char, tx, title_y, 66, w_a, 0, 1);
        }
        // meta line
        let rating = cfield(&hero.rating);
        let meta = format!("Movie \u{b7} {} \u{b7} {}", hero.year, if rating.is_empty() { "NR" } else { &rating });
        if let Ok(m) = CString::new(meta) {
            p.text(m.as_ptr(), tx, title_y + 92.0, 26, d_a, 0, 0);
        }
        // synopsis: pixel-wrapped to the hero text column, 2 lines max, ellipsized
        let summary = cfield(&hero.summary);
        if !summary.is_empty() {
            TextView::new(&summary, 24, d_a)
                .leading(30.0)
                .max_lines(2)
                .draw(p, Rect::new(tx, title_y + 128.0, 660.0, 0.0));
        }
        // fixed-position action group (cascade scales their base alphas)
        self.play.draw(env, p);
        for c in &self.actions {
            c.draw(env, p);
        }
        self.dots.draw(env, p);
    }
}

// ---- Card: hot collection cell (in [Card;COLS], not boxed). Owns its scale spring.
struct Card {
    row: usize,
    col: usize,
    scale: Spring,
}
impl Card {
    fn new(row: usize, col: usize) -> Self {
        Card { row, col, scale: Spring::at(1.0) }
    }
}

// ---- Shelf: one hub row = [Card;MAX_ITEMS] (only hub_len used) + its own scroll spring
struct Shelf {
    row: usize,
    cards: [Card; MAX_ITEMS],
    scroll_x: Spring,
    base_y: f32,
}
impl Shelf {
    fn new(row: usize) -> Self {
        Shelf { row, cards: std::array::from_fn(|c| Card::new(row, c)), scroll_x: Spring::at(0.0), base_y: 0.0 }
    }
    fn update(&mut self, env: &Env) {
        let (f_r, f_c) = (env.fr as usize, env.fc as usize);
        let n = crate::pms::hub_len(self.row);
        for c in 0..MAX_ITEMS {
            let target = if self.row == f_r && c == f_c { 1.055 } else { 1.0 };
            self.cards[c].scale.step(target, K_SCALE, env.dt);
        }
        // only the focused row's scroll animates (matches ui_home.c: springs scrollX[fr] alone)
        if env.fr as usize == self.row && n > 0 {
            let max_sx = (n as f32 * (CARD_W + GAP) - GAP - (SCR_W - 2.0 * MARGIN_X)).max(0.0);
            let want = (f_c as f32 * (CARD_W + GAP) - (CARD_W + GAP)).clamp(0.0, max_sx);
            self.scroll_x.step(want, K_SCROLL, env.dt);
        }
    }
    fn draw_cells(&self, env: &Env, p: Painter) {
        for c in 0..crate::pms::hub_len(self.row) {
            if self.row == env.fr as usize && c == env.fc as usize && env.sp > 0.5 {
                continue; // focused card drawn last (grid z-order)
            }
            let m = unsafe { movie_at(self.row as c_int, c as c_int).as_ref() };
            let Some(mm) = m else { continue };
            let x = MARGIN_X + c as f32 * (CARD_W + GAP) - self.scroll_x.pos * env.sp;
            if x > SCR_W || x + CARD_W < -GLOW_PAD {
                continue;
            }
            let s = self.cards[c].scale.pos;
            let r = Rect::new(x, self.base_y + 12.0, CARD_W, CARD_H).scaled(s);
            draw_poster(p, m, r, 14.0 * s);
            resume_bar(p, r, mm);
        }
    }
}

// ---- Grid: the collection view. Holds [Shelf;ROWS] + the vertical scroll spring,
// drives nav/hit-test/wheel, draws all non-focused cells then the focused card last.
struct Grid {
    shelves: [Shelf; MAX_HUBS],
    scroll_y: Spring,
}
impl Grid {
    fn new() -> Self {
        Grid { shelves: std::array::from_fn(Shelf::new), scroll_y: Spring::at(0.0) }
    }
    fn update(&mut self, env: &Env) {
        for s in self.shelves.iter_mut() {
            s.update(env);
        }
        let nh = crate::pms::hub_count().max(1);
        let max_y = (nh as f32 * ROW_PITCH - (SCR_H - CONTENT_Y) + 60.0).max(0.0);
        let want_y = (env.fr as f32 * ROW_PITCH - ROW_PITCH * 0.6).clamp(0.0, max_y);
        self.scroll_y.step(want_y, K_SCROLL, env.dt);
    }
    fn layout(&mut self, env: &Env) {
        let shelf_top = PEEK_Y + (GRID_TOP_Y - PEEK_Y) * env.sp; // 828 -> 150
        for r in 0..MAX_HUBS {
            self.shelves[r].base_y = shelf_top + r as f32 * ROW_PITCH - self.scroll_y.pos * env.sp;
        }
    }
    fn draw(&self, env: &Env, p: Painter) {
        let nh = crate::pms::hub_count();
        for r in 0..nh {
            let row_y = self.shelves[r].base_y;
            if row_y > SCR_H || row_y + CARD_H < 0.0 {
                continue;
            }
            // hub title above the row; it rises as the row's focused card magnifies so
            // the title-to-card gap stays proportional (Apple-TV behavior).
            if env.sp > 0.02 {
                let fs = if r == env.fr as usize {
                    self.shelves[r].cards[(env.fc as usize).min(MAX_ITEMS - 1)].scale.pos
                } else {
                    1.0
                };
                let lift = CARD_H * (fs - 1.0) * 0.5;
                if let Ok(t) = CString::new(crate::pms::hub_title(r)) {
                    p.text(t.as_ptr(), MARGIN_X, row_y - 34.0 - lift, 28, theme::with_a(theme::TEXT_PRIMARY, env.sp), 0, 1);
                }
            }
            self.shelves[r].draw_cells(env, p);
        }
        // focused card + ring + title, drawn LAST for z-order (grid mode only)
        if env.sp > 0.5 {
            let (r, c) = (env.fr as usize, env.fc as usize);
            if r >= nh {
                return;
            }
            let s = self.shelves[r].cards[c.min(MAX_ITEMS - 1)].scale.pos;
            let x = MARGIN_X + c as f32 * (CARD_W + GAP) - self.shelves[r].scroll_x.pos * env.sp;
            let rect = Rect::new(x, self.shelves[r].base_y + 12.0, CARD_W, CARD_H).scaled(s);
            let m = unsafe { movie_at(r as c_int, c as c_int).as_ref() };
            draw_poster(p, m, rect, 14.0 * s);
            p.ring(rect, GLOW_PAD, theme::CARD_RING_RAD * s, (s - 1.0) / 0.055);
            if let Some(mm) = m {
                resume_bar(p, rect, mm);
                p.text(mm.title.as_ptr() as *const c_char, rect.cx(), rect.y + rect.h + 12.0, 26, theme::TEXT_PRIMARY, 1, 1);
            }
        }
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
        let cx = MARGIN_X + g_fc() as f32 * (CARD_W + GAP) - self.shelves[cur as usize].scroll_x.pos + CARD_W * 0.5;
        let mut nc =
            ((cx - MARGIN_X - CARD_W * 0.5 + self.shelves[ncur as usize].scroll_x.pos) / (CARD_W + GAP) + 0.5) as c_int;
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
                    let x = MARGIN_X + c as f32 * (CARD_W + GAP) - self.shelves[r].scroll_x.pos;
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
    bg_phase: f32,
    bg: Backdrop,
    hero: Hero,
    grid: Grid,
}
impl Home {
    fn new() -> Self {
        Home { snap: Spring::at(0.0), bg_phase: 0.0, bg: Backdrop::new(), hero: Hero::new(), grid: Grid::new() }
    }
    fn env(&self, dt: f32) -> Env {
        let sp = self.snap.pos;
        // clamp the focus into the current hub bounds so a stray write degrades to a
        // valid shelves[fr]/cards[fc] index rather than reading out of range.
        let nh = crate::pms::hub_count().max(1) as c_int;
        let cfr = g_fr().clamp(0, (nh - 1).min(MAX_HUBS as c_int - 1));
        let ncols = crate::pms::hub_len(cfr as usize).max(1) as c_int;
        let cfc = g_fc().clamp(0, (ncols - 1).min(MAX_ITEMS as c_int - 1));
        Env { dt, screen: Rect::FULL, fr: cfr, fc: cfc, sp, hero_a: (1.0 - sp / 0.55).clamp(0.0, 1.0) }
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
        h.bg_phase += dt * 0.15; // parity (unused by draw, as in C)
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
        let p = Painter::root();
        h.bg.draw(&env, p);
        if env.hero_a > 0.01 {
            h.hero.draw(&env, p.alpha(env.hero_a));
        }
        h.grid.layout(&env);
        h.grid.draw(&env, p);
    });
}

pub(crate) fn home_move_focus(sym: c_uint) {
    guard(|| scene().grid.nav(sym));
}

pub(crate) fn home_pointer_focus(mx: f32, my: f32) {
    guard(|| scene().grid.hit_test(mx, my));
}

pub(crate) fn home_wheel(dy: c_int) {
    guard(|| scene().grid.wheel(dy));
}
