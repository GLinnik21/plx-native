//! retui — a retained, UIKit-style view tree for the webOS Plex client.
//!
//! Layered purely over the crate's own gfx/text primitives + spring(): it never
//! touches GL. A View is `update -> layout -> draw` each frame; springs live as
//! fields on the views that own them; the `Painter` folds a cascading alpha (and
//! optional translate) into every draw op. Single-threaded, main-thread-only.
//! (Design: docs/ui-framework.md — synthesized from a 3-way design workflow.)
#![allow(dead_code)] // widgets are added module-by-module; some land before their first caller

use std::os::raw::{c_char, c_int};

pub mod anim;
pub mod chapters_panel;
pub mod consts;
pub mod detail;
pub mod home;
pub mod icons;
pub mod info_panel;
pub mod label;
pub mod player_hud;
pub mod profile;
pub mod table;
pub mod text_view;
pub mod theme;
pub mod track_menu;
pub mod widgets;

/// Player HUD accent palette — the mockup's "Snow": a warm off-white focus fill with near-black
/// ink/icons over it. The focused control (button, pill, menu row) fills ACCENT; its label/glyph
/// draws in ACCENT_INK. Idle controls use a faint white fill + white ink.
/// Canonical values now live in [`theme`]; re-exported so existing `crate::ui::ACCENT` sites hold.
pub use theme::{ACCENT, ACCENT_INK};

#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub const FULL: Rect = Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 };
    #[inline]
    pub fn cx(&self) -> f32 {
        self.x + self.w * 0.5
    }
    #[inline]
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
    /// scale about center — reproduces the C card pop exactly (cx = x-(w-W)/2).
    #[inline]
    pub fn scaled(&self, s: f32) -> Rect {
        let (w, h) = (self.w * s, self.h * s);
        Rect::new(self.x - (w - self.w) * 0.5, self.y - (h - self.h) * 0.5, w, h)
    }
}

#[derive(Clone, Copy, Default)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

/// One animated scalar, delegating to the existing critically-damped C spring so
/// motion is byte-identical. "Springs live in views": a view owns one per value.
#[derive(Clone, Copy, Default)]
pub struct Spring {
    pub pos: f32,
    pub vel: f32,
}
impl Spring {
    pub const fn at(p: f32) -> Self {
        Self { pos: p, vel: 0.0 }
    }
    #[inline]
    pub fn step(&mut self, target: f32, k: f32, dt: f32) {
        crate::gfx::spring(&mut self.pos, &mut self.vel, target, k, dt);
    }
    #[inline]
    pub fn jump(&mut self, v: f32) {
        self.pos = v;
        self.vel = 0.0;
    }
}

/// Per-frame context, bridged ONCE from the C globals (fr/fc/snapTarget).
#[derive(Clone, Copy)]
pub struct Env {
    pub dt: f32,
    pub screen: Rect,
    pub fr: c_int,
    pub fc: c_int,
    pub sp: f32,     // snapPos 0..1 (hero -> grid)
    pub hero_a: f32, // clamp(1 - sp/0.55)
}

/// The retained-tree contract. Defaults let leaves implement only `draw`.
pub trait View {
    fn update(&mut self, _env: &Env) {}
    fn layout(&mut self, _frame: Rect, _env: &Env) {}
    fn draw(&self, env: &Env, p: Painter);
    fn measure(&self) -> Size {
        Size::default()
    }
}

/// Folds a cascading alpha (+ optional translate) into every primitive call.
/// Copy + stack-lived, so `p.alpha(x)` / `p.translate(..)` chain with zero alloc.
/// The gfx/text draw fns are `pub extern "C"` (safe to call from Rust).
#[derive(Clone, Copy)]
pub struct Painter {
    dx: f32,
    dy: f32,
    a: f32,
}
impl Painter {
    pub const fn root() -> Self {
        Self { dx: 0.0, dy: 0.0, a: 1.0 }
    }
    pub fn alpha(self, m: f32) -> Self {
        Self { a: self.a * m, ..self }
    }
    pub fn translate(self, dx: f32, dy: f32) -> Self {
        Self { dx: self.dx + dx, dy: self.dy + dy, ..self }
    }
    #[inline]
    fn c(self, c: [f32; 4]) -> [f32; 4] {
        [c[0], c[1], c[2], c[3] * self.a]
    }
    pub fn rect(self, r: Rect, rad: f32, top: [f32; 4], bot: [f32; 4], focus: f32) {
        let (t, b) = (self.c(top), self.c(bot));
        crate::gfx::draw_rect(r.x + self.dx, r.y + self.dy, r.w, r.h, 0.0, rad, t.as_ptr(), b.as_ptr(), focus);
    }
    /// a focus ring/glow with no fill (colors zero, focus drives it). The quad is
    /// inflated by `pad` on every side so the SDF box lands on the card edge and the
    /// glow band has room outside it (matches ui_home.c's draw_rect(cx-PAD, .., w+2*PAD, ..)).
    pub fn ring(self, r: Rect, pad: f32, rad: f32, focus: f32) {
        let z = [0.0f32; 4];
        crate::gfx::draw_rect(r.x + self.dx - pad, r.y + self.dy - pad, r.w + 2.0 * pad, r.h + 2.0 * pad,
            pad, rad, z.as_ptr(), z.as_ptr(), focus);
    }
    pub fn rrect(self, r: Rect, rl: f32, rr: f32, col: [f32; 4]) {
        let c = self.c(col);
        crate::gfx::draw_rrect(r.x + self.dx, r.y + self.dy, r.w, r.h, rl, rr, c.as_ptr());
    }
    pub fn ptri(self, r: Rect, col: [f32; 4]) {
        let c = self.c(col);
        crate::gfx::draw_ptri(r.x + self.dx, r.y + self.dy, r.w, r.h, c.as_ptr());
    }
    pub fn tex(self, tex: u32, r: Rect, rad: f32, tint: [f32; 4]) {
        let t = self.c(tint);
        crate::gfx::draw_tex(tex, r.x + self.dx, r.y + self.dy, r.w, r.h, rad, t.as_ptr());
    }
    /// bilinear 4-corner gradient (opaque; the cascade alpha is intentionally not applied)
    pub fn ambient(self, r: Rect, dim: f32, k: [[f32; 3]; 4]) {
        crate::gfx::draw_ambient(r.x + self.dx, r.y + self.dy, r.w, r.h, dim,
            k[0].as_ptr(), k[1].as_ptr(), k[2].as_ptr(), k[3].as_ptr());
    }
    /// draw text at absolute (x,y) plus the cascade translate; returns width
    pub fn text(self, s: *const c_char, x: f32, y: f32, sz: c_int, col: [f32; 4], align: c_int, bold: c_int) -> f32 {
        let c = self.c(col);
        crate::text::draw_text(s, x + self.dx, y + self.dy, sz, c.as_ptr(), align, bold)
    }
}
