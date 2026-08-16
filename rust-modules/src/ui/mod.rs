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
pub mod alt_sources; // "Also available": the same item on a second pinned source, as a picker
pub mod anim;
pub mod card_row;
pub mod chapters_panel;
pub mod consts;
pub mod detail;
pub mod fmt; // shared duration/clock display formatters
pub mod hero_logo; // the ONE clearLogo sizing rule + its fallback-to-title band (both heroes, the compact title)
pub mod home;
pub mod icons;
pub mod idle; // whole-FRAME present gating: a screen with nothing moving on it stops repainting
pub mod info_panel;
pub mod item_menu; // press-and-hold card context menu (Go to Show / Mark as Watched / Play from Start)
pub mod label;
pub mod library; // the Library browse screen (poster wall + server-driven sort/filter)
pub mod more_menu; // the player's `…` overflow popover (holds the Stats for nerds toggle)
pub mod login; // sign-in screen (QR / short code) for the plex.tv account flow
pub mod nav; // ROUTE-level page cross-fade + the continuous-chrome rule (the tab bar rides across)
pub mod person; // the person / actor page (Apple-TV shape) — opened from a detail page's cast row
pub mod search; // the Search screen: field + recents + typed result shelves (the last pill in the top strip)
pub mod profiles; // "who's watching" Plex Home picker + PIN keypad
pub mod player_hud;
pub mod skip_pill; // in-player Skip Intro / Skip Credits pill (server marker driven)
pub mod stats; // the "Stats for nerds" diagnostics overlay — how bug reports leave a stranger's TV
pub mod popover; // shared modal open/appear choreography (track menu / info / chapters / account)
pub mod press; // tvOS-style click: OK-down dips the focused card, OK-up springs it back + activates
pub mod profile;
pub mod table;
pub mod text_view;
pub mod theme;
pub mod track_menu;
pub mod trail; // the BACK trail: which pages are behind the one on screen (app.rs pops it)
pub mod up_next; // end-of-episode Up Next card + auto-advance countdown
pub mod widgets;
pub mod xfade; // content cross-fade: fade out → swap the data at the floor → fade in

/// The focus fill — a near-white with near-black ink/icons over it, and the only fill a control
/// lights up with. The focused control (button, pill, menu row) fills ACCENT; its label/glyph
/// draws in ACCENT_INK. Idle controls use a faint white fill + white ink.
/// Canonical values now live in [`theme`]; re-exported so existing `crate::ui::ACCENT` sites hold.
pub use theme::{ACCENT, ACCENT_INK};

// ---- The UI panic barrier -------------------------------------------------------------------

/// Set once the first guarded panic has been reported, so a screen that panics EVERY frame does
/// not write a line per frame. See the rationale in [`guard`].
static GUARD_RECOVERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Run one UI entry point (a screen's `draw`/`update`/key handler) behind a panic barrier: a panic
/// inside `f` unwinds only as far as this call, and the frame is abandoned instead of the process.
///
/// **Why this exists — it is not defensive programming, it is an FFI requirement.** `plex_run` is
/// `#[no_mangle] pub extern "C"` (the C boot shim in `src/main.c` calls it), and a panic that
/// unwinds *out of* an `extern "C"` frame is undefined behaviour; the toolchain lowers it to an
/// immediate `abort()`. Every UI draw runs inside that `extern "C"` frame, so on this device a
/// stray index-out-of-bounds in a screen is not "one bad frame" — it is SIGABRT, a dead app, a
/// live Starfish buffer-feed session torn down mid-`Feed()`, and the TV back at the launcher, with
/// no debugger attached to say why. Wrapping the DISPATCH (see `app.rs`, where the whole
/// route→screen draw is one guarded block) rather than each screen means a screen added later is
/// covered by construction instead of by its author remembering.
///
/// The `Err` arm **must** release the GL scissor. [`Painter::clip`] is global GL state that its
/// user pairs with a [`Painter::clip_clear`] at the end of the same draw (`TableView::draw` is the
/// one user today) — a panic between the two skips the clear, and every subsequent frame in the
/// process would then be silently scissored to whatever rect the dying screen last set, with
/// nothing downstream able to tell why the UI went partly blank. This unwind is the only place
/// that can see it happened, so this is the only place that can repair it.
///
/// **What this does NOT protect** (do not over-trust it):
/// - A panic on a **worker thread** — demux, load, timeline, poster/metadata/browse fetches. That
///   thread dies and its work silently stops; those bodies carry their own `catch_unwind`
///   (`metadata.rs`, `browse.rs`, `pms.rs`, `img.rs`, `player/mod.rs`).
/// - An **abort**, which `catch_unwind` cannot catch by construction: allocation failure, a double
///   panic (a second panic raised by a `Drop` running during *this* unwind), or a panic crossing
///   one of the OTHER `extern "C"` seams we hand to C — the libavformat AVIO callbacks in `ff.rs`
///   and the Starfish/ACB event callbacks. Those still kill the process.
/// - **Consistent state.** The guarded body ran halfway and whatever it mutated before panicking
///   stays mutated — `AssertUnwindSafe` is precisely the assertion that we accept that. The next
///   frame redraws from the same state, so a screen that panics once normally panics every frame;
///   this keeps the app alive and navigable, it does not fix the screen.
///
/// Main-thread only, and free on the happy path: `catch_unwind`'s landing pad is cold, so guarding
/// the per-frame draw dispatch is not a hot-path cost on the A53.
#[inline]
pub fn guard(f: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
        crate::gfx::clip_clear();
        // Log the FIRST recovery only. `app::install_panic_logger` already writes the panic's
        // message + source location to BOTH the event log and the persistent crash log for every
        // panic, so the only news here is "the frame was dropped and the GL clip was released" —
        // and the thing that panics in a draw panics 60x/sec, so repeating this line would double
        // an already-flooding stream while telling nobody anything new. One line marks that the
        // barrier is what is keeping the app alive; the hook's lines say what is wrong.
        if !GUARD_RECOVERED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            crate::log("ui::guard: recovered from a panic — frame dropped, GL clip released (logged once; the panic hook logs every panic)");
        }
    }
}

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
    pub fn cy(&self) -> f32 {
        self.y + self.h * 0.5
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
    /// This rect FILLED by a `tw × th` source with its aspect preserved and centred — the
    /// `background-size: cover` rule, and the counterpart of the CONTAIN math
    /// [`hero_logo::fit`](crate::ui::hero_logo::fit) does (that one keeps a logo INSIDE its column;
    /// this one overflows a picture PAST its frame so the frame is never
    /// letterboxed). Full-bleed artwork needs it because [`Painter::tex`] maps UV 0..1 across the
    /// rect: a source that is not the frame's aspect is SQUASHED, and an episode still is a video
    /// frame whose aspect we do not control.
    ///
    /// A degenerate source (either dimension ≤ 0 — i.e. the texture has not decoded yet, so
    /// `widgets::resolve_tex_wh` answers 0) returns the frame UNCHANGED, so a caller drawing before
    /// the size is known gets today's stretch rather than a zero-area quad that blanks the backdrop.
    ///
    /// The overflow costs no fill: GL rasterizes only inside the viewport, so the off-panel part of
    /// the quad generates no fragments.
    #[inline]
    pub fn cover(&self, tw: f32, th: f32) -> Rect {
        if tw <= 0.0 || th <= 0.0 {
            return *self;
        }
        let s = (self.w / tw).max(self.h / th);
        let (w, h) = (tw * s, th * s);
        Rect::new(self.x + (self.w - w) * 0.5, self.y + (self.h - h) * 0.5, w, h)
    }
    /// The overlap of two rects — the part of `self` that `o` lets through. A miss returns a
    /// ZERO-SIZE rect (never a negative one), so `w > 0` is a clean "any of this is visible?"
    /// test. This is how a scissor-clipped strip records what it actually drew: hit-testing the
    /// clipped rect instead of the laid-out one is what stops an off-screen item staying
    /// clickable at coordinates it no longer occupies.
    #[inline]
    pub fn intersect(&self, o: Rect) -> Rect {
        let (x0, y0) = (self.x.max(o.x), self.y.max(o.y));
        let (x1, y1) = ((self.x + self.w).min(o.x + o.w), (self.y + self.h).min(o.y + o.h));
        Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
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
    /// Teleport, with no motion in between. Reports to [`ui::idle`](crate::ui::idle) — a jump
    /// changes the drawn value without ever reaching a spring integrator, so nothing else would
    /// hear it.
    ///
    /// **Change-guarded, and that guard is mandatory rather than tidy:** `home.rs` calls
    /// `snap.jump(0.0)` on EVERY frame while the hub list is empty, so an unconditional report
    /// would pin the loop at 60 fps on precisely the screen the present gate exists for.
    #[inline]
    pub fn jump(&mut self, v: f32) {
        crate::ui::idle::note_jump(self.pos != v || self.vel != 0.0);
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

impl Env {
    /// The throwaway Env for leaf widgets that draw purely from their own fields and ignore it.
    /// Deliberately NOT `Default`: a screen that should compute a real per-frame Env must not be
    /// able to silently grab a zeroed one — reach for this only where the callee ignores its Env.
    pub const fn inert() -> Self {
        Self { dt: 0.0, screen: Rect::FULL, fr: 0, fc: 0, sp: 0.0, hero_a: 0.0 }
    }
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
    rgb: f32,
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
        Self { dx: 0.0, dy: 0.0, a: 1.0, rgb: 1.0 }
    }
    pub fn alpha(self, m: f32) -> Self {
        Self { a: self.a * m, ..self }
    }
    pub fn translate(self, dx: f32, dy: f32) -> Self {
        Self { dx: self.dx + dx, dy: self.dy + dy, ..self }
    }
    /// Multiply the RGB written by every descendant primitive. With the frame clear multiplied by
    /// the same value, this is algebraically the same result as a final full-screen black scrim,
    /// without paying another 1920x1080 blended pass on the television.
    pub fn rgb(self, m: f32) -> Self {
        Self { rgb: self.rgb * m.clamp(0.0, 1.0), ..self }
    }
    /// This painter's accumulated horizontal offset — what to ADD to a coordinate drawn through it
    /// to get the screen x it lands on.
    ///
    /// Exposed for exactly one job: clamping a run against the PANEL edges from inside a translated
    /// tree. A horizontally scrolled shelf draws through `translate(-scroll_x, 0)`, so a rect
    /// handed to a child is a content coordinate — and `card_row`'s label clamp compared one of
    /// those against `SCR_W` and pinned every focused caption at a fixed screen x once a row had
    /// scrolled a screen's worth (device-observed on the Search episode shelf: the words under the
    /// tile changed with focus while the block itself never moved). Reach for it only where a
    /// SCREEN bound is genuinely the thing being tested; ordinary drawing must stay in the
    /// painter's own space, which is the whole point of the cascade.
    pub fn dx(self) -> f32 {
        self.dx
    }
    #[inline]
    fn c(self, c: [f32; 4]) -> [f32; 4] {
        [c[0] * self.rgb, c[1] * self.rgb, c[2] * self.rgb, c[3] * self.a]
    }
    pub fn rect(self, r: Rect, rad: f32, top: [f32; 4], bot: [f32; 4], focus: f32) {
        let (t, b) = (self.c(top), self.c(bot));
        crate::gfx::draw_rect(
            r.x + self.dx,
            r.y + self.dy,
            r.w,
            r.h,
            0.0,
            rad,
            t.as_ptr(),
            b.as_ptr(),
            focus,
            self.rgb,
        );
    }
    pub fn rrect(self, r: Rect, rl: f32, rr: f32, col: [f32; 4]) {
        let c = self.c(col);
        crate::gfx::draw_rrect(r.x + self.dx, r.y + self.dy, r.w, r.h, rl, rr, c.as_ptr());
    }
    /// A rounded-rect **OUTLINE with nothing inside it** — a `w`-px inset ring in `col`, and the
    /// background composites straight through the middle.
    ///
    /// This is the primitive the SDF was said not to have, and the absence cost real fidelity: an
    /// outlined chip was drawn as a KNOCKOUT (the ring colour, then the interior repainted in a
    /// colour the caller swore was the ground), which is exact on a flat panel and simply wrong
    /// over artwork — the hero's identity line composites a backdrop plus two scrim ramps, so the
    /// "ground" a caller can name is nothing like what is actually behind the chip, and each chip
    /// read as a dark box rather than a hairline. Both mocks spell these `box-shadow: inset 0 0 0
    /// Npx <colour>` with no `background` at all; this is that.
    ///
    /// It rides `fs_src.frag`'s existing rim path (`u_rimw`/`u_rimcol`, the focus edge-sheen's own
    /// band) with a **fully transparent BLACK** fill. Both halves of that colour matter: the shader
    /// premultiplies the fill by coverage alone (`rgb = fill.rgb * aFill`) and not by `fill.a`, so a
    /// transparent WHITE would still add white — the alpha-0 rgb has to be 0 too.
    ///
    /// Two properties of that shared path are worth knowing before reaching for this.
    /// **`rad` must be ≥ 0.5**: below it the fragment shader takes its flat fast-path and returns
    /// the (transparent) fill without ever evaluating the rim, so a square ring draws nothing.
    /// And the rim folds `u_rimcol.a` into its coverage term, so a partly-faded ring composites at
    /// roughly α² rather than α — it reads a touch thin mid-fade and is exact at either end. That is
    /// the sheen's own long-standing behaviour and cannot be corrected here without re-tuning every
    /// card's edge sheen; against a knockout that is wrong while the screen is STILL, it is the far
    /// better trade.
    pub fn rring(self, r: Rect, rad: f32, w: f32, col: [f32; 4]) {
        let c = self.c(col);
        const HOLLOW: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
        crate::gfx::draw_rrect_sheened(r.x + self.dx, r.y + self.dy, r.w, r.h, rad, rad, HOLLOW.as_ptr(), w, c.as_ptr());
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
        self.c(theme::with_a(theme::CARD_SHEEN, theme::CARD_SHEEN[3]))
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
    /// The FROSTED ground: what the frame drew behind `r`, blurred, clipped to `r`'s rounded rect.
    ///
    /// Returns whether it drew. `false` is not an error — the blur latches itself off on a driver
    /// that cannot give it a render target (`gfx`'s backdrop-blur note), and a caller that gets it
    /// must draw an opaque ground instead of a translucent one. Reach for
    /// [`Popover::panel`](crate::ui::popover::Popover::panel) rather than this: it owns that pair,
    /// so no screen has to carry the fallback itself.
    ///
    /// **Order is the argument**: this samples the DEFAULT FRAMEBUFFER as it stands, so it must be
    /// called after everything meant to show through and before anything meant to sit on top.
    /// Painter primitives are immediate, so "behind" means "already drawn this frame" — nothing
    /// else. Never call it on the player route: the video plane is not in our framebuffer, so what
    /// is behind a panel there is punch-through alpha, not a picture.
    /// `rest_dy` is how far this frame's painter has been slid from the panel's RESTING position —
    /// a popover's appear translate, and 0 for anything that does not move. The snapshot is grabbed
    /// around the rest rect rather than around this frame's, so the slide itself does not invalidate
    /// it. Cached glass stays at one snapshot; a dynamic policy may refresh independently;
    /// `gfx::draw_blur_backdrop` has the full argument.
    #[must_use]
    pub fn backdrop_blur(self, r: Rect, rest_dy: f32, rad: f32, tint: [f32; 4]) -> bool {
        let t = self.c(tint);
        let (x, y) = (r.x + self.dx, r.y + self.dy);
        crate::gfx::draw_blur_backdrop(x, y, r.w, r.h, [x, y - rest_dy, r.w, r.h], rad, t.as_ptr())
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
        let shcol = self.c(theme::with_a(theme::CARD_SHADOW, sa));
        let pad = blur + 1.0; // inflate for the symmetric penumbra (+1 AA margin)
        crate::gfx::draw_tex_carded(tex, r.x + self.dx, r.y + self.dy, r.w, r.h, rad, t.as_ptr(),
            theme::CARD_SHEEN_W, self.sheen_rim().as_ptr(), pad, blur, shcol.as_ptr());
    }
    /// Bilinear 4-corner gradient. The written pixels stay OPAQUE — this primitive REPLACES what is
    /// under it (see [`AmbientWash`](crate::ui::widgets::AmbientWash)) — but it is no longer blind
    /// to the cascade: an alpha below 1 mixes every corner toward [`theme::SURFACE_APP`] instead of
    /// being ignored. That is the only reading of "fade this out" an opaque full-screen field HAS,
    /// and it is the right one: the app's ground is what lies behind a page (`gfx::frame_clear` lays
    /// down `theme::CLEAR_RGB`, the same colour), so at alpha 0 the wash IS the ground and
    /// [`ui::nav`](crate::ui::nav)'s page dip has no seam where the framebuffer was cleared and the
    /// next screen takes over.
    ///
    /// Alpha 1 is bit-for-bit the old call, which is every call outside a page transition; only the
    /// corner RGB is touched, so the write stays opaque and no blending is added. This does NOT make
    /// a wash cross-fadeable BETWEEN two items — dissolving one item's colours into another's is
    /// still a spring per corner channel; see `AmbientWash`.
    pub fn ambient(self, r: Rect, dim: f32, k: [[f32; 3]; 4]) {
        let a = self.a.clamp(0.0, 1.0);
        let g = theme::SURFACE_APP; // `theme::mix` is rgba; a wash corner is rgb
        let k = if a >= 1.0 {
            k
        } else {
            k.map(|c| std::array::from_fn(|i| g[i] + (c[i] - g[i]) * a))
        };
        let k = k.map(|c| c.map(|v| v * self.rgb));
        crate::gfx::draw_ambient(r.x + self.dx, r.y + self.dy, r.w, r.h, dim,
            k[0].as_ptr(), k[1].as_ptr(), k[2].as_ptr(), k[3].as_ptr());
    }
    /// Bilinear 4-corner gradient with real per-corner ALPHA, folded through the cascade — the
    /// counterpart of [`ambient`](Self::ambient), which is opaque by contract and therefore cannot
    /// sit OVER artwork. Corner order is the same: tl, tr, br, bl.
    ///
    /// It is the only two-dimensional gradient the renderer has ([`rect`](Self::rect)'s is vertical
    /// only), which is why a corner-weighted scrim over a hero backdrop goes through here rather
    /// than through N abutting strips — see `widgets::hero_scrim`. Straight (non-premultiplied) rgba
    /// interpolates exactly only when the corners share an rgb, so give it ONE ink at four alphas,
    /// not four hues.
    pub fn grad4(self, r: Rect, k: [[f32; 4]; 4]) {
        // bind the mapped array to a `let` first — pointers into a temporary would dangle
        let c = k.map(|q| self.c(q));
        crate::gfx::draw_grad4(r.x + self.dx, r.y + self.dy, r.w, r.h,
            c[0].as_ptr(), c[1].as_ptr(), c[2].as_ptr(), c[3].as_ptr());
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
    /// Child `i`'s height. Queried per frame, so it MAY BE ANIMATED — `child_top`, `content_h`, the
    /// scroll target and every pointer hit-test read it, so a springed height makes the whole flow
    /// below it follow for free (the person page's condensing header band is the first user).
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

#[cfg(test)]
mod tests {
    //! The retui core's pure geometry. Ordinary parallel tests — `Rect` carries no state and
    //! reaches no crate global, so nothing here needs `testlock` or a module mutex.
    use super::*;

    #[test]
    fn painter_rgb_and_alpha_are_independent_multiplicative_cascades() {
        let p = Painter::root().rgb(0.5).rgb(0.8).alpha(0.25).alpha(0.5);
        let c = p.c([0.75, 0.5, 0.25, 0.8]);
        assert_eq!(c, [0.3, 0.2, 0.1, 0.1]);
        assert_eq!(p.rgb, 0.4);
        assert_eq!(p.a, 0.125);
    }

    /// Every field of `a` within `eps` of `b`'s.
    fn near(a: Rect, b: Rect, eps: f32) -> bool {
        (a.x - b.x).abs() <= eps && (a.y - b.y).abs() <= eps && (a.w - b.w).abs() <= eps && (a.h - b.h).abs() <= eps
    }

    /// The guarantee that lets `cover` be applied to EVERY backdrop at once: a source that is
    /// already the frame's aspect comes back as the frame, whatever its pixel size. A normal 16:9
    /// show backdrop is therefore pixel-identical to the stretch it replaces.
    #[test]
    fn cover_is_a_no_op_when_the_source_matches_the_frame() {
        for (tw, th) in [(1920.0, 1080.0), (1280.0, 720.0), (640.0, 360.0), (3840.0, 2160.0)] {
            let r = Rect::FULL.cover(tw, th);
            assert!(near(r, Rect::FULL, 1e-3), "{tw}x{th} → {:?}", (r.x, r.y, r.w, r.h));
        }
    }

    /// Cover, not contain: the frame is always fully painted, the source aspect always survives, and
    /// the overflow is centred so the crop is even on both sides. Letterboxing here would read as a
    /// broken backdrop, which is why the long axis is allowed to run off the panel.
    #[test]
    fn cover_overflows_the_long_axis_and_never_letterboxes() {
        for (tw, th) in [(2592.0, 1080.0), (1440.0, 1080.0), (1000.0, 1000.0), (1000.0, 1500.0)] {
            let r = Rect::FULL.cover(tw, th);
            assert!(r.w >= Rect::FULL.w - 1e-3, "{tw}x{th}: frame not covered horizontally ({})", r.w);
            assert!(r.h >= Rect::FULL.h - 1e-3, "{tw}x{th}: frame not covered vertically ({})", r.h);
            assert!((r.w / r.h - tw / th).abs() < 1e-3, "{tw}x{th}: aspect {} not preserved", r.w / r.h);
            assert!((r.cx() - Rect::FULL.cx()).abs() < 1e-3, "{tw}x{th}: not centred in x");
            assert!((r.cy() - Rect::FULL.cy()).abs() < 1e-3, "{tw}x{th}: not centred in y");
            // exactly one axis overflows (or neither, at the frame's own aspect)
            assert!(r.w <= Rect::FULL.w + 1e-3 || r.h <= Rect::FULL.h + 1e-3, "{tw}x{th}: both axes overflow");
        }
    }

    /// The window that exists on EVERY frame before a texture lands: the store answers 0 until the
    /// slot is READY. A zero-area or negative rect there would blank the backdrop, so the frame must
    /// come back untouched — the caller simply gets the old stretch for a few frames.
    #[test]
    fn cover_leaves_the_frame_alone_for_an_undecoded_source() {
        let f = Rect::new(10.0, 20.0, 300.0, 400.0);
        for (tw, th) in [(0.0, 0.0), (0.0, 720.0), (1280.0, 0.0), (-1.0, -1.0), (-1280.0, 720.0)] {
            let r = f.cover(tw, th);
            assert!(near(r, f, 0.0), "{tw}x{th} must return the frame unchanged");
            assert!(r.w > 0.0 && r.h > 0.0, "{tw}x{th} produced a degenerate rect");
        }
    }

    /// The centring is about the FRAME, not about the panel: home's backdrop layer is parallaxed to
    /// a non-zero origin and slides horizontally, so a `cover` that centred on (0,0) would drift the
    /// crop across the flip.
    #[test]
    fn cover_survives_a_non_origin_frame() {
        let f = Rect::new(200.0, 100.0, 400.0, 200.0);
        let r = f.cover(400.0, 400.0);
        assert!((r.cx() - f.cx()).abs() < 1e-3 && (r.cy() - f.cy()).abs() < 1e-3, "crop must stay on the frame's centre");
        assert!((r.w - 400.0).abs() < 1e-3 && (r.h - 400.0).abs() < 1e-3, "a square source covers a 2:1 frame by its width");
        assert!(r.y < f.y && r.y + r.h > f.y + f.h, "the square must overflow the short axis both ways");
    }
}
