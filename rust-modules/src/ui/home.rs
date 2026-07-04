//! The home screen as a retui tree: Backdrop + Hero + Grid([Shelf;5]×[Card;10]).
//! Owns the C ABI (fr/fc/snapTarget globals + home_*/movie_at) that main.c drives
//! unchanged. fr/fc/snapTarget stay the source of truth (main.c writes them too);
//! the tree reads them live each frame via Env and writes back through nav.
//! Draw ops are emitted argument-for-argument identical to ui_home.c.
#![allow(non_upper_case_globals)]
use crate::pms::PmsMovie;
use crate::ui::consts::*;
use crate::ui::widgets::{cfield, draw_poster, wrap_two, CircleButton, PageDots, PillButton};
use crate::ui::{Env, Painter, Rect, Spring, View};
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uint};
use std::ptr::{addr_of, addr_of_mut};

// ---- shared focus state; main.c reads AND writes these (incl. the autoplay path) ----
#[no_mangle]
pub static mut fr: c_int = 0;
#[no_mangle]
pub static mut fc: c_int = 0;
#[no_mangle]
pub static mut snapTarget: f32 = 0.0;

#[inline]
fn g_fr() -> c_int {
    unsafe { addr_of!(fr).read() }
}
#[inline]
fn g_fc() -> c_int {
    unsafe { addr_of!(fc).read() }
}

/// index the (Rust) catalog; returns a pointer into pms_movies[] or null
#[no_mangle]
pub extern "C" fn movie_at(r: c_int, c: c_int) -> *mut PmsMovie {
    let idx = r * COLS as c_int + c;
    let n = unsafe { addr_of!(crate::pms::pms_nmovies).read() };
    if idx >= 0 && idx < n {
        unsafe { (addr_of_mut!(crate::pms::pms_movies) as *mut PmsMovie).add(idx as usize) }
    } else {
        std::ptr::null_mut()
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
        let hero = movie_at(0, 0);
        unsafe {
            let mut bt = 0u32;
            if !hero.is_null() && (*hero).art[0] != 0 {
                bt = crate::ui::widgets::resolve_tex((*hero).art.as_ptr() as *const c_char, 1280, 720, 0);
            }
            if !hero.is_null() && (*hero).has_blur != 0 && (sp > 0.004 || bt == 0) {
                p.ambient(env.screen, 0.55, (*hero).blur);
            }
            if bt != 0 && sp < 0.996 {
                let ba = 1.0 - sp;
                p.tex(bt, Rect::new(0.0, -sp * (SCR_H - 120.0), SCR_W, SCR_H), 0.0, [1.0, 1.0, 1.0, ba]);
            }
            if env.hero_a > 0.01 {
                let sa = 0.30 + 0.64 * env.hero_a;
                p.rect(Rect::new(0.0, SCR_H * 0.46, SCR_W, SCR_H * 0.54), 0.0,
                    [0.02, 0.02, 0.03, 0.0], [0.02, 0.02, 0.03, sa], 0.0);
            }
        }
    }
}

// ---- Hero: low-left content composite. Drawn under p.alpha(hero_a) so the whole
// group fades as one; widgets carry base alphas that the cascade scales.
struct Hero {
    play: PillButton,
    actions: [CircleButton; 3],
    dots: PageDots,
}
impl Hero {
    fn new() -> Self {
        let tx = MARGIN_X;
        let pill_y = 510.0 + 200.0; // titleY + 200
        let (pw, cgap, cd) = (168.0f32, 20.0f32, 60.0f32);
        Hero {
            play: PillButton::play(c"Play".as_ptr()).at(tx, pill_y),
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
        let hero = movie_at(0, 0);
        if hero.is_null() {
            return;
        }
        let tx = MARGIN_X;
        let title_y = 510.0f32;
        let w_a = [0.97, 0.98, 0.99, 1.0]; // cascade applies hero_a
        let d_a = [0.70, 0.73, 0.78, 1.0];
        unsafe {
            // title: clearLogo (transparent PNG) if loaded, else bold text
            let mut lt = 0u32;
            let (mut lw, mut lh) = (0i32, 0i32);
            if (*hero).rk[0] != 0 {
                let rk = cfield(&(*hero).rk);
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
                p.text((*hero).title.as_ptr() as *const c_char, tx, title_y, 66, w_a, 0, 1);
            }
            // meta line
            let rating = cfield(&(*hero).rating);
            let meta = format!("Movie \u{b7} {} \u{b7} {}", (*hero).year, if rating.is_empty() { "NR" } else { &rating });
            if let Ok(m) = CString::new(meta) {
                p.text(m.as_ptr(), tx, title_y + 92.0, 26, d_a, 0, 0);
            }
            // synopsis, two lines on a word boundary
            let summary = cfield(&(*hero).summary);
            if !summary.is_empty() {
                let (l1, l2) = wrap_two(&summary);
                if let Ok(c1) = CString::new(l1) {
                    p.text(c1.as_ptr(), tx, title_y + 128.0, 24, d_a, 0, 0);
                }
                if !l2.is_empty() {
                    if let Ok(c2) = CString::new(l2) {
                        p.text(c2.as_ptr(), tx, title_y + 158.0, 24, d_a, 0, 0);
                    }
                }
            }
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

// ---- Shelf: one grid row = [Card;COLS] + its own horizontal scroll spring
struct Shelf {
    row: usize,
    cards: [Card; COLS],
    scroll_x: Spring,
    base_y: f32,
}
impl Shelf {
    fn new(row: usize) -> Self {
        Shelf { row, cards: std::array::from_fn(|c| Card::new(row, c)), scroll_x: Spring::at(0.0), base_y: 0.0 }
    }
    fn update(&mut self, env: &Env) {
        let (f_r, f_c) = (env.fr as usize, env.fc as usize);
        for c in 0..COLS {
            let target = if self.row == f_r && c == f_c { 1.055 } else { 1.0 };
            self.cards[c].scale.step(target, K_SCALE, env.dt);
        }
        // only the focused row's scroll animates (matches ui_home.c: springs scrollX[fr] alone)
        if env.fr as usize == self.row {
            let max_sx = (COLS as f32 * (CARD_W + GAP) - GAP - (SCR_W - 2.0 * MARGIN_X)).max(0.0);
            let want = (f_c as f32 * (CARD_W + GAP) - (CARD_W + GAP)).clamp(0.0, max_sx);
            self.scroll_x.step(want, K_SCROLL, env.dt);
        }
    }
    fn draw_cells(&self, env: &Env, p: Painter) {
        for c in 0..COLS {
            if self.row == env.fr as usize && c == env.fc as usize && env.sp > 0.5 {
                continue; // focused card drawn last (grid z-order)
            }
            let m = movie_at(self.row as c_int, c as c_int);
            if m.is_null() {
                continue;
            }
            let x = MARGIN_X + c as f32 * (CARD_W + GAP) - self.scroll_x.pos * env.sp;
            if x > SCR_W || x + CARD_W < -GLOW_PAD {
                continue;
            }
            let s = self.cards[c].scale.pos;
            let r = Rect::new(x, self.base_y + 12.0, CARD_W, CARD_H).scaled(s);
            draw_poster(p, m, r, 14.0 * s);
        }
    }
}

// ---- Grid: the collection view. Holds [Shelf;ROWS] + the vertical scroll spring,
// drives nav/hit-test/wheel, draws all non-focused cells then the focused card last.
struct Grid {
    shelves: [Shelf; ROWS],
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
        let max_y = (ROWS as f32 * ROW_PITCH - (SCR_H - CONTENT_Y) + 60.0).max(0.0);
        let want_y = (env.fr as f32 * ROW_PITCH - ROW_PITCH * 0.6).clamp(0.0, max_y);
        self.scroll_y.step(want_y, K_SCROLL, env.dt);
    }
    fn layout(&mut self, env: &Env) {
        let shelf_top = PEEK_Y + (GRID_TOP_Y - PEEK_Y) * env.sp; // 828 -> 150
        for r in 0..ROWS {
            self.shelves[r].base_y = shelf_top + r as f32 * ROW_PITCH - self.scroll_y.pos * env.sp;
        }
    }
    fn draw(&self, env: &Env, p: Painter) {
        for r in 0..ROWS {
            let row_y = self.shelves[r].base_y;
            if row_y > SCR_H || row_y + CARD_H < 0.0 {
                continue;
            }
            if movie_at(r as c_int, 0).is_null() {
                continue;
            }
            self.shelves[r].draw_cells(env, p);
        }
        // focused card + ring + title, drawn LAST for z-order (grid mode only)
        if env.sp > 0.5 {
            let (r, c) = (env.fr as usize, env.fc as usize);
            let s = self.shelves[r].cards[c].scale.pos;
            let x = MARGIN_X + c as f32 * (CARD_W + GAP) - self.shelves[r].scroll_x.pos * env.sp;
            let rect = Rect::new(x, self.shelves[r].base_y + 12.0, CARD_W, CARD_H).scaled(s);
            let m = movie_at(r as c_int, c as c_int);
            draw_poster(p, m, rect, 14.0 * s);
            p.ring(rect, GLOW_PAD, 14.0 * s, (s - 1.0) / 0.055);
            if !m.is_null() {
                unsafe {
                    p.text((*m).title.as_ptr() as *const c_char, rect.cx(), rect.y + rect.h + 12.0, 26, [0.96, 0.97, 0.98, 1.0], 1, 1);
                }
            }
        }
    }
    // ---- navigation: writes the fr/fc globals (never caches focus) ----
    fn nav(&self, sym: c_uint) {
        unsafe {
            if sym == SDLK_LEFT && fc > 0 {
                fc -= 1;
            } else if sym == SDLK_RIGHT && fc < COLS as c_int - 1 {
                fc += 1;
            } else if sym == SDLK_UP && fr > 0 {
                self.vert(-1);
            } else if sym == SDLK_DOWN && fr < ROWS as c_int - 1 {
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
        nc = nc.clamp(0, COLS as c_int - 1);
        fr = ncur;
        fc = nc;
    }
    fn hit_test(&self, mx: f32, my: f32) {
        unsafe {
            for r in 0..ROWS {
                let row_y = CONTENT_Y + r as f32 * ROW_PITCH - self.scroll_y.pos + ROW_TITLE_H + 18.0;
                if my < row_y || my > row_y + CARD_H {
                    continue;
                }
                for c in 0..COLS {
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
            if dy < 0 && fr < ROWS as c_int - 1 {
                fr += 1;
            } else if dy > 0 && fr > 0 {
                fr -= 1;
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
        Env { dt, screen: Rect::FULL, fr: g_fr(), fc: g_fc(), sp, hero_a: (1.0 - sp / 0.55).clamp(0.0, 1.0) }
    }
}

static mut SCENE: Option<Home> = None;
#[inline]
fn scene() -> &'static mut Home {
    unsafe { (*addr_of_mut!(SCENE)).as_mut().unwrap() }
}

#[no_mangle]
pub extern "C" fn home_init() {
    unsafe {
        *addr_of_mut!(SCENE) = Some(Home::new());
    }
}

#[no_mangle]
pub extern "C" fn home_update(dt: f32) {
    let h = scene();
    h.bg_phase += dt * 0.15; // parity (unused by draw, as in C)
    let target = unsafe { addr_of!(snapTarget).read() };
    h.snap.step(target, K_SNAP, dt);
    let env = h.env(dt);
    h.grid.update(&env);
}

#[no_mangle]
pub extern "C" fn home_draw() {
    crate::gfx::frame_clear(0.03, 0.03, 0.045);
    let h = scene();
    let env = h.env(0.0);
    let p = Painter::root();
    h.bg.draw(&env, p);
    if env.hero_a > 0.01 {
        h.hero.draw(&env, p.alpha(env.hero_a));
    }
    h.grid.layout(&env);
    h.grid.draw(&env, p);
}

#[no_mangle]
pub extern "C" fn home_move_focus(sym: c_uint) {
    scene().grid.nav(sym);
}

#[no_mangle]
pub extern "C" fn home_pointer_focus(mx: f32, my: f32) {
    scene().grid.hit_test(mx, my);
}

#[no_mangle]
pub extern "C" fn home_wheel(dy: c_int) {
    scene().grid.wheel(dy);
}
