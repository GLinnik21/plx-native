//! retui — a retained, UIKit-style view tree for the webOS Plex client.
//!
//! Layered purely over the crate's own gfx/text primitives + spring(): it never
//! touches GL. A View is `update -> layout -> draw` each frame; springs live as
//! fields on the views that own them; the `Painter` folds a cascading alpha (and
//! optional translate) into every draw op. Single-threaded, main-thread-only.
//! (Design: docs/ui-framework.md — synthesized from a 3-way design workflow.)
#![allow(dead_code)] // widgets are added module-by-module; some land before their first caller

use std::os::raw::{c_char, c_int};

pub mod account_menu; // Home top-left profile popover (change profile / sign out)
pub mod anim;
pub mod card_row;
pub mod chapters_panel;
pub mod consts;
pub mod detail;
pub mod fmt; // shared duration/clock display formatters
pub mod home;
pub mod icons;
pub mod info_panel;
pub mod label;
pub mod login; // sign-in screen (QR / short code) for the plex.tv account flow
pub mod profiles; // "who's watching" Plex Home picker + PIN keypad
pub mod player_hud;
pub mod popover; // shared modal open/appear choreography (track menu / info / chapters / account)
pub mod press; // tvOS-style click: OK-down dips the focused card, OK-up springs it back + activates
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
    /// This rect shrunk by `d` on every side (negative grows it).
    #[inline]
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2.0 * d, self.h - 2.0 * d)
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
    /// Step with an UNDERdamped spring (`zeta < 1` → overshoots/rings). The critically-damped
    /// [`step`](Self::step) can't bounce; this drives the `ui::press` click spring-back. See
    /// [`gfx::spring_zeta`](crate::gfx::spring_zeta).
    #[inline]
    pub fn step_zeta(&mut self, target: f32, k: f32, zeta: f32, dt: f32) {
        crate::gfx::spring_zeta(&mut self.pos, &mut self.vel, target, k, zeta, dt);
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
/// Resting→lifted drop-shadow params — penumbra `blur`, downward `off`, ink `alpha` — for a tile of
/// height `h` at focus-pop `f` (0 = resting/close to the shelf, 1 = fully lifted). Shared by the
/// folded card shadow ([`Painter::tex_carded`]) and the standalone one ([`Painter::focus_shadow`],
/// the profile chip). Every tile carries a shadow; it *grows* with the pop rather than appearing.
fn card_shadow_params(h: f32, f: f32) -> (f32, f32, f32) {
    let f = f.clamp(0.0, 1.0);
    let blur_l = (h * 0.13).clamp(6.0, theme::CARD_SHADOW_BLUR);
    let off_l = (h * 0.04).clamp(3.0, theme::CARD_SHADOW_DY);
    let blur_r = (h * 0.05).clamp(3.0, theme::CARD_SHADOW_REST_BLUR);
    let off_r = (h * 0.015).clamp(1.5, theme::CARD_SHADOW_REST_DY);
    let lerp = |a: f32, b: f32| a + (b - a) * f;
    (lerp(blur_r, blur_l), lerp(off_r, off_l), lerp(theme::CARD_SHADOW_REST_A, theme::CARD_SHADOW[3]))
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
    /// CIRCULAR tiles get at least 40px of quad: their radial glow was hard-cut by the tight 6px
    /// strip pad into a visible translucent SQUARE (the home profile chip / picker avatars). The
    /// cut is invisible on rounded-RECT tiles (it runs parallel to the glow contours), and those
    /// keep the caller's tight quad — the episode/chapter strips draw the focused card in-loop,
    /// where a wider quad glowed over the left neighbour but was overdrawn by the right one (an
    /// asymmetric halo). The SDF box tracks the pad via u_pad, so the ring line never moves.
    pub fn ring(self, r: Rect, pad: f32, rad: f32, focus: f32) {
        let z = [0.0f32; 4];
        let circular = rad * 2.0 >= r.w.min(r.h) - 1.0;
        let qp = if circular { pad.max(40.0) } else { pad };
        crate::gfx::draw_rect(r.x + self.dx - qp, r.y + self.dy - qp, r.w + 2.0 * qp, r.h + 2.0 * qp,
            qp, rad, z.as_ptr(), z.as_ptr(), focus);
    }
    pub fn rrect(self, r: Rect, rl: f32, rr: f32, col: [f32; 4]) {
        let c = self.c(col);
        crate::gfx::draw_rrect(r.x + self.dx, r.y + self.dy, r.w, r.h, rl, rr, c.as_ptr());
    }
    /// Soft drop-shadow of `r` (corner `radius`, `w/2` = circle) with `blur` px of penumbra, its box
    /// pushed down `off_y` px. Draw it BEFORE the tile art so the tile sits over its own shadow.
    pub fn shadow(self, r: Rect, radius: f32, blur: f32, off_y: f32, col: [f32; 4]) {
        let c = self.c(col);
        crate::gfx::draw_shadow(r.x + self.dx, r.y + self.dy + off_y, r.w, r.h, radius, blur, off_y, c.as_ptr());
    }
    /// Standalone soft drop-shadow under a tile (its own [`FS_SHADOW`](crate::gfx) pass) — used by the
    /// profile chip, whose avatar isn't a folded card composite. Every tile carries a shadow that GROWS
    /// with the pop `f` (0 = resting/close to the shelf, 1 = lifted). Card tiles fold this into their
    /// texture pass via [`tex_carded`](Self::tex_carded) instead; this remains for the non-folded chip.
    pub fn focus_shadow(self, r: Rect, radius: f32, f: f32) {
        let (blur, off, a) = card_shadow_params(r.h, f);
        self.shadow(r, radius, blur, off, theme::with_a(theme::CARD_SHADOW, a));
    }
    /// The tile-fill colour of the focus edge-sheen (the 1px inset perimeter rim), folded into the
    /// caller's alpha cascade — shared by the sheened fill primitives below.
    #[inline]
    fn sheen_rim(self) -> [f32; 4] {
        theme::with_a(theme::CARD_SHEEN, theme::CARD_SHEEN[3] * self.a)
    }
    /// A rounded-rect FILL that also carries the 1px perimeter edge-sheen in the SAME pass (the
    /// no-texture counterpart of [`tex_stroked`](Self::tex_stroked)) — for skeleton / chip-disc tiles.
    pub fn rect_sheened(self, r: Rect, rad: f32, top: [f32; 4], bot: [f32; 4]) {
        let (t, b) = (self.c(top), self.c(bot));
        let rim = self.sheen_rim();
        crate::gfx::draw_rect_sheened(r.x + self.dx, r.y + self.dy, r.w, r.h, rad, t.as_ptr(), b.as_ptr(), theme::CARD_SHEEN_W, rim.as_ptr());
    }
    /// Flat rounded-rect fill + the 1px perimeter edge-sheen in one pass (the flat-colour placeholder tile).
    pub fn rrect_sheened(self, r: Rect, rad: f32, col: [f32; 4]) {
        let c = self.c(col);
        let rim = self.sheen_rim();
        crate::gfx::draw_rrect_sheened(r.x + self.dx, r.y + self.dy, r.w, r.h, rad, rad, c.as_ptr(), theme::CARD_SHEEN_W, rim.as_ptr());
    }
    pub fn tex(self, tex: u32, r: Rect, rad: f32, tint: [f32; 4]) {
        let t = self.c(tint);
        crate::gfx::draw_tex(tex, r.x + self.dx, r.y + self.dy, r.w, r.h, rad, t.as_ptr());
    }
    /// [`tex`](Self::tex) with the focus edge-sheen (the 1px inset perimeter rim) baked into the SAME
    /// pass — rim only, no shadow. Used for the profile chip avatar.
    pub fn tex_stroked(self, tex: u32, r: Rect, rad: f32, tint: [f32; 4]) {
        let t = self.c(tint);
        crate::gfx::draw_tex_stroked(tex, r.x + self.dx, r.y + self.dy, r.w, r.h, rad, t.as_ptr(), theme::CARD_SHEEN_W, self.sheen_rim().as_ptr());
    }
    /// The full CARD composite in ONE pass — texture + 1px edge-sheen + the soft drop-shadow that
    /// grows with the pop `f` (folded via [`gfx::draw_tex_carded`](crate::gfx::draw_tex_carded)). `r` is
    /// the (already-scaled) card rect; the quad is inflated by the penumbra internally. This is how
    /// every art tile gets its resting-and-rising shadow without a separate soft-shadow pass.
    pub fn tex_carded(self, tex: u32, r: Rect, rad: f32, tint: [f32; 4], f: f32) {
        let t = self.c(tint);
        let (blur, _off, sa) = card_shadow_params(r.h, f); // cards use a symmetric penumbra — offset is chip-only
        let shcol = theme::with_a(theme::CARD_SHADOW, sa * self.a);
        let pad = blur + 1.0; // inflate for the symmetric penumbra (+1 AA margin)
        crate::gfx::draw_tex_carded(tex, r.x + self.dx, r.y + self.dy, r.w, r.h, rad, t.as_ptr(),
            theme::CARD_SHEEN_W, self.sheen_rim().as_ptr(), pad, blur, shcol.as_ptr());
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
    /// [`text`](Self::text) with a horizontal fade-out: glyph alpha runs 1→0 between
    /// `fade_from`..`fade_to` px from the string's left edge (see `text::draw_text_fade`).
    #[allow(clippy::too_many_arguments)]
    pub fn text_fade(self, s: *const c_char, x: f32, y: f32, sz: c_int, col: [f32; 4], bold: c_int,
        fade_from: f32, fade_to: f32) -> f32 {
        let c = self.c(col);
        crate::text::draw_text_fade(s, x + self.dx, y + self.dy, sz, c.as_ptr(), 0, bold, fade_from, fade_to)
    }
    /// Hard-clip subsequent draws to `r` (in this painter's space — the cascade translate is folded
    /// in). `Painter` otherwise has no clip/scissor; a scrolling list uses this so a partial row is
    /// cut cleanly at its frame edge instead of poking over the video / control buttons. ALWAYS pair
    /// with [`clip_clear`](Self::clip_clear) before the frame ends — scissor is global GL state.
    pub fn clip(self, r: Rect) {
        crate::gfx::clip_set(r.x + self.dx, r.y + self.dy, r.w, r.h);
    }
    /// Release the clip set by [`clip`](Self::clip).
    pub fn clip_clear(self) {
        crate::gfx::clip_clear();
    }
}

// ---- Shared scroll / cull / hero primitives -------------------------------------------------
// Both screens (home, detail) have the same shape: a top hero that fades as below-hero content
// scrolls up over it, with off-screen children skipped BY HAND via CULLING — the scroll flow culls
// off-frame children by index rather than scissor-clipping them (`Painter::clip` exists but is
// reserved for a bounded panel like the track menu; culling the whole document flow avoids per-frame
// scissor churn). This is an immediate-mode renderer — every frame clears + redraws the whole tree,
// so the way to "avoid drawing" is to CULL what isn't visible, not to dirty-track what changed. These are the ONE
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
/// monomorphizes with no vtable/alloc. `Copy` so a caller holding `&mut self` can copy it out and
/// pass `self` back as the `&impl Column` without a borrow conflict.
#[derive(Clone, Copy)]
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
    /// Draw every present child, scrolled and band-culled — off-screen children are SKIPPED by
    /// culling (this flow culls rather than using the `Painter::clip` scissor). The focused child is never culled (the scroll keeps it at
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
}
