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
pub mod card_row;
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

// ---- Shared scroll / cull / hero primitives -------------------------------------------------
// Both screens (home, detail) have the same shape: a top hero that fades as below-hero content
// scrolls up over it, with off-screen children skipped BY HAND because `Painter` has no clip/scissor
// (this is an immediate-mode renderer — every frame clears + redraws the whole tree, so the way to
// "avoid drawing" is to CULL what isn't visible, not to dirty-track what changed). These are the ONE
// mechanism each: `on_axis` is the single off-screen cull test both screens call; `hero_alpha` the
// single hero-fade curve. `ScrollColumn`/`Column` is the scroll-into-content container detail
// composes its below-hero flow from (home's fixed-pitch grid is not a document flow, so it uses only
// the two leaf fns and keeps its own two-pass focused-last draw).

/// Is a child visible along one axis? `start` = its leading edge ALREADY in screen space (the caller
/// subtracted the scroll/offset), `extent` = its size along the axis, `span` = the viewport extent
/// (`SCR_W`/`SCR_H`), `lead` = slack past the near (0) edge (e.g. home's `GLOW_PAD` room for the
/// focus glow). Pure + `#[inline]`, so culling stays zero-alloc on the hot path.
#[inline]
pub fn on_axis(start: f32, extent: f32, span: f32, lead: f32) -> bool {
    start < span && start + extent.max(1.0) > -lead
}

/// The hero-fade curve: a top hero fades to 0 as `progress` rises to `fade_end`. The caller keeps its
/// own `fade_end` (home 0.55 on the snap continuum, detail 400px of scroll) so the motion constants
/// stay byte-identical at the call site. `1.0 - hero_alpha(..)` is the complementary compact-title
/// alpha.
#[inline]
pub fn hero_alpha(progress: f32, fade_end: f32) -> f32 {
    (1.0 - progress / fade_end).clamp(0.0, 1.0)
}

/// A vertical scroll-into-content container: owns the scroll `Spring` + the cumulative child flow
/// ([`child_top`](ScrollColumn::child_top), the single below-hero Y source) + the off-screen band
/// cull. It holds NO child views (that would force per-frame boxing/dynamic dispatch — banned on the
/// weak-ARM hot path); instead the caller implements [`Column`] to supply the present children, their
/// measured heights, gaps, focus, and a local-coord draw. Generic over `impl Column`, so it
/// monomorphizes with no vtable/alloc.
pub struct ScrollColumn {
    pub scroll: Spring,
    pub top: f32,    // first child's pre-scroll top
    pub margin: f32, // the focused child lifts to this distance from the screen top
}

/// The content a [`ScrollColumn`] lays out: the PRESENT children in document order, their measured
/// heights + inter-child gaps, which one holds focus (never culled), and a local-coord draw (the
/// `Painter` is pre-translated to the child's origin, so the child draws from y=0).
pub trait Column {
    fn len(&self) -> usize;
    fn height(&self, i: usize) -> f32;
    fn gap_before(&self, i: usize) -> f32;
    fn focus_child(&self) -> Option<usize>;
    fn draw_child(&self, i: usize, env: &Env, p: Painter);
}

impl ScrollColumn {
    pub const fn new(top: f32, margin: f32) -> Self {
        Self { scroll: Spring::at(0.0), top, margin }
    }
    /// The pre-scroll top of child `i`: stacks the present children's heights from `top`, adding
    /// `gap_before(k)` before each child k>0. This IS the flow — the single below-hero Y source.
    pub fn child_top(&self, c: &impl Column, i: usize) -> f32 {
        let mut y = self.top;
        for k in 1..=i {
            y += c.gap_before(k);
            y += c.height(k - 1);
        }
        y
    }
    /// The scroll offset that lifts child `i`'s top to `margin` (clamped at 0).
    pub fn lift_target(&self, c: &impl Column, i: usize) -> f32 {
        (self.child_top(c, i) - self.margin).max(0.0)
    }
    /// Draw every present child, scrolled and band-culled — off-screen children are SKIPPED (never
    /// clipped; `Painter` has no scissor). The focused child is never culled (the scroll keeps it at
    /// `margin`). The child `Painter` is pre-translated to the child origin, so children draw 0-based.
    pub fn draw(&self, c: &impl Column, env: &Env, p: Painter) {
        let ps = p.translate(0.0, -self.scroll.pos);
        let f = c.focus_child();
        let mut y = self.top;
        for i in 0..c.len() {
            if i > 0 {
                y += c.gap_before(i);
            }
            let h = c.height(i);
            if Some(i) == f || on_axis(y - self.scroll.pos, h, env.screen.h, 0.0) {
                c.draw_child(i, env, ps.translate(0.0, y));
            }
            y += h;
        }
    }
    /// [`Self::draw`] then a DEFERRED overlay pass on the scrolled layer — the supported hook for a
    /// focused-last / cross-row z-order carve-out (home's single focused card would go here if home
    /// were ever a `Column`; detail's sections don't overlap, so detail just calls [`Self::draw`]).
    pub fn draw_with_overlay(&self, c: &impl Column, env: &Env, p: Painter, overlay: impl FnOnce(Painter)) {
        self.draw(c, env, p);
        overlay(p.translate(0.0, -self.scroll.pos));
    }
}
