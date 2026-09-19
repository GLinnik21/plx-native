//! **THE UNDERLAY FIELD — an overlay inherits the light of whatever is under it, where it is.**
//!
//! An overlay in this app has had two ways to relate to the page beneath it, and both throw away
//! *where*:
//!
//! * a **scrim** — `theme::scrim_black(a)` over the whole screen — says nothing about the page at
//!   all, so a modal over a green-lit hero and a modal over a blue-lit one are the same picture;
//! * a **four-corner envelope** — four sampled corners keyed through [`AmbientWash`] — says the
//!   page is greenish, with four degrees of freedom. It cannot say that the green is on the LEFT
//!   of the bottom edge and the red on the right, because a bilinear surface has no such shape in
//!   it. (`RouteGround`'s own live-frame sample used to read this way, through the four
//!   `glReadPixels` taps of `gfx::sample_modal_ambient`; PR2 stage B retired both in favour of the
//!   field below, and [`UnderlayField::latch_from_corners`] is what a caller with no readable
//!   frame — the video plane, a route that opens before Home has drawn — still reaches for.)
//!
//! This is the third: a 15x8 grid reduced from the frame itself (`gfx::field_kick`),
//! low-passed, graded, reconstructed to 60x32 and drawn as one magnified quad with the shared
//! dither. **It is spatially faithful** — the green stays where the green is — and it costs one
//! texture fetch a fragment, because the expensive part happened once, at the latch.
//!
//! **There is no blur pass and there must not be one.** A blur of the page is a different object:
//! `docs/liquid-glass.md` sizes what it costs, and the thing it buys — letter-scale structure
//! softened rather than destroyed — is exactly what a GROUND must not have. Reducing to 15x8 is a
//! box filter with a support 128 screen pixels wide; titles, faces and poster edges cannot survive
//! it, which is the property a four-corner wash always had and which this keeps while adding the
//! one thing four corners lack — see [`RouteGround`](crate::ui::route_screen::RouteGround), whose
//! ground this module has been since PR2 stage B.
//!
//! # The pipeline, and why each step is where it is
//!
//! Every step below is a pure function over arrays, so all of it is gradeable by a host test with
//! no GL context; only [`UnderlayField::latch_from_frame`] and [`UnderlayField::draw`] touch the
//! renderer.
//!
//! 1. **Linearise.** Everything that follows is a WEIGHTED SUM, and a weighted sum of
//!    display-encoded values is not a colour — it is an artefact of the transfer curve.
//!    `gfx::lin` is the one implementation (it is `gfx::diffuse_ground_mean`'s, factored out).
//! 2. **Low-pass** the 15x8 grid with a separable Gaussian at [`SIGMA`] cells, edge-clamped. The
//!    grid arrives from a box filter, whose frequency response has sidelobes; without this a
//!    single bright poster on a cell boundary reads as a rectangle in the finished field, because
//!    Catmull-Rom will happily interpolate a step.
//! 3. **Grade** each cell — and through the SAME functions the four-corner wash uses
//!    ([`AmbientWash::keyed_one`]), not a copy of them, so a ground built here and a ground built
//!    from an envelope are the same colour for the same source. That is what makes stage B's
//!    migration of `RouteGround` a change of SHAPE and not of palette. The grade is defined on
//!    display-encoded values (`GROUND_LUMA` is a Rec.709 ceiling over the stored codes — see its
//!    doc), so this step encodes, grades and linearises again rather than pretending otherwise.
//! 4. **Reconstruct** 15x8 → 60x32 with Catmull-Rom, clamped to the local min/max so a cubic
//!    cannot overshoot a cell pair and invent a colour the page does not contain. Linear
//!    extrapolation outside the grid, so the field does not flatten in the outer half-cell against
//!    the screen edge — which is exactly where a page's own artwork usually is.
//! 5. **Encode** to display RGBA8 and upload through `gfx::upload_rgba`, which already sets the
//!    LINEAR + CLAMP_TO_EDGE quartet an NPOT texture REQUIRES on this driver.
//!
//! # Why 60x32 and not 15x8
//!
//! GL_LINEAR would magnify 15x8 for free. It magnifies BILINEARLY, which is C0: the derivative
//! steps at every cell boundary, and a 128px-wide facet meeting its neighbour at a crease is
//! visible on a 1080p panel as a faint quilt — the same defect that makes a four-corner wash the
//! *right* shape for four corners and the wrong one for 120 cells. Catmull-Rom is C1, so the
//! reconstruction is done on the CPU, once, into a texture that bilinear magnification then only
//! has to smooth 32x further. 4x per axis is where the crease stops being findable.
use crate::gfx;
use crate::ui::theme;
use crate::ui::widgets::AmbientWash;
use crate::ui::{Painter, Rect};

/// The sampled grid, from `gfx` so there is one shape and not two.
const W: usize = gfx::FIELD_W as usize;
const H: usize = gfx::FIELD_H as usize;
const N: usize = gfx::FIELD_CELLS;

/// Output texels per cell, per axis — see the module doc's last section.
const UPSAMPLE: usize = 4;
const TEX_W: usize = W * UPSAMPLE;
const TEX_H: usize = H * UPSAMPLE;

/// **The low-pass width, in CELLS.** 1.2 is a little over one cell, which is the whole intent: it
/// removes the box filter's sidelobes and the one-cell steps they leave, and it does not smear a
/// side of the screen into the opposite one. Larger and the field converges on its own mean, at
/// which point 120 cells are an expensive way to store four corners.
const SIGMA: f32 = 1.2;
/// Taps either side of centre — `ceil(3*SIGMA)`, past which a Gaussian contributes under 1.1e-2 of
/// the kernel and cannot move an 8-bit code.
const RADIUS: usize = 4;

/// **How a sampled cell becomes a drawable colour.** The grade is the only place this module makes
/// a design decision, and both variants exist because both are wanted by the same mechanism.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Grade {
    /// A page GROUND: the source is artwork, so it is held under `GROUND_LUMA` and leaned from
    /// `theme::SURFACE_APP` toward that colour by [`AmbientWash::GROUND_W`] — the legibility
    /// contract `widgets_ambient_ground_tests.rs` grades, inherited whole rather than restated.
    Ground,
    /// IDENTITY. A dim is drawn OVER the page it sampled, not in place of it, so capping and
    /// leaning it would be grading the same light twice; the weight in
    /// [`Role::Dim`] is the whole of that surface's strength.
    Dim,
}

/// **What the field is being drawn AS.** Separate from [`Grade`] because they answer different
/// questions — the grade is baked at the latch and the role is chosen per draw — and because a
/// dim's strength is a continuous knob while a ground's is not.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Role {
    /// The OPAQUE ground of a page: it replaces what is under it, and a fade below 1 mixes toward
    /// `theme::SURFACE_APP` rather than toward transparency, exactly as `Painter::ambient` reads a
    /// cascade below 1.
    Ground,
    /// A scrim that carries the page's own light. `weight` is how much of the field survives the
    /// black ink: `mix(theme::SCRIM_BLACK_INK, field, weight)`, which is a plain multiply because
    /// the ink is zero. **`weight == 0` is today's flat `theme::scrim_black` and must stay
    /// BIT-IDENTICAL to it** — see [`plan`].
    ///
    /// `RouteGround` (PR2 stage B) only ever draws [`Role::Ground`]; this variant's consumer is
    /// PR3's `ModalUnderlay` (`containers::modal::ModalStack::draw_scrims`), which constructs it as
    /// `Role::Dim { weight: theme::underlay::TINT }` for every surface's scrim.
    Dim { weight: f32 },
}

/// **What a [`UnderlayField::draw`] resolves to, as a VALUE.**
///
/// A value rather than four branches inside `draw`, because the one property that must not drift
/// is unobservable from a host test the moment it is expressed as GL calls: at `weight == 0`, and
/// on an unlatched field, this surface is the flat scrim the app already drew, to the last bit.
/// A field tinted to `[0,0,0,a]` is NOT that — `plx_dither` would add its ±1 LSB of noise to a
/// surface whose whole content is one code — so the flat path is a different draw, not a special
/// case of the same one.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Draw {
    /// One uniform rect of this colour, through `fs_flat.frag`.
    Flat([f32; 4]),
    /// The field texture, multiplied by this tint.
    Field([f32; 4]),
}

/// [`Draw`] for a `Role::Dim` — pure, and the home of the bit-identity contract above.
pub(crate) fn plan(latched: bool, weight: f32, alpha: f32) -> Draw {
    let w = weight.clamp(0.0, 1.0);
    if !latched || w <= 0.0 {
        Draw::Flat(theme::scrim_black(alpha))
    } else {
        // `mix(SCRIM_BLACK_INK, field, w)` with a zero ink IS `field * w`, which is what the
        // shader's one multiply already does. No second uniform, no second mix.
        Draw::Field([w, w, w, alpha])
    }
}

/// **A latched colour field of the page under an overlay.**
///
/// It owns a GL texture, so it is not `Copy` — the owner holds it (a screen field; since PR2 stage
/// B, [`RouteGround`](crate::ui::route_screen::RouteGround) holds one this way, which is what
/// makes RouteGround itself no longer `Copy` either) and dropping the owner frees the texture.
pub(crate) struct UnderlayField {
    /// The graded field in LINEAR light, row-major from the top-left. Linear because every
    /// reconstruction below is a weighted sum; display-encoded is what `sample` and the texture
    /// hand back.
    cells: [[f32; 3]; N],
    /// The 60x32 reconstruction, or 0 before the first latch. Re-specced in place on every latch
    /// (`upload_rgba` reuses `prev`), so a field costs one texture name for its whole life.
    tex: u32,
    /// Rec.709 luma of every texel of that reconstruction, over its display CODES — what a panel's
    /// luma ceiling is solved against ([`panel_plan`](Self::panel_plan)). Kept beside the texture
    /// rather than recomputed per draw: a panel asks every frame, the field changes only at a latch.
    luma: [u8; TEX_W * TEX_H],
    latched: bool,
    /// A page reduction queued by [`latch_from_frame`](Self::latch_from_frame)
    /// and not yet read back.
    pending: Option<gfx::FieldTicket>,
}

/// What [`UnderlayField::latch_from_frame`] answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FrameLatch {
    /// The field holds a picture (this call's, or an earlier one's).
    Latched,
    /// The read is in flight; ask again next frame.
    Pending,
    /// No honest answer this frame (`gfx::field_kick`'s refusals): the caller's fallback.
    Refused,
}

/// **What [`UnderlayField::draw_panel`] resolves to, as a VALUE** — [`Draw`]'s counterpart for a
/// popover's material, so the one decision a host test cannot see through GL (which window of the
/// field, how bright) is gradeable without a context.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum PanelDraw {
    /// Nothing latched: the flat panel sheet (`theme::PANEL_TOP`/`PANEL_BOT`), never a blank.
    Flat,
    /// The field's window `uv` — `(x, y, w, h)` of the panel over the screen size — multiplied by
    /// the opaque `tint`.
    Field { uv: [f32; 4], tint: [f32; 4] },
}

/// **The panel's window into the field**: its SCREEN rect over the screen size. The field maps the
/// whole screen, so this is the only UV rect under which a cell stays where it is on the page —
/// green under the panel's bottom-left is sampled at the panel's bottom-left.
pub(crate) fn panel_uv(screen: Rect) -> [f32; 4] {
    let (sw, sh) = (crate::ui::consts::SCR_W, crate::ui::consts::SCR_H);
    [screen.x / sw, screen.y / sh, screen.w / sw, screen.h / sh]
}

impl UnderlayField {
    pub(crate) const fn new() -> Self {
        Self {
            cells: [[0.0; 3]; N],
            tex: 0,
            luma: [0; TEX_W * TEX_H],
            latched: false,
            pending: None,
        }
    }

    /// **Freeze the frame that has already been drawn — without ever waiting for it.** Idempotent
    /// while latched, which is what makes it safe to call from a `draw` that runs every frame.
    ///
    /// The page is reduced on the first call (`gfx::field_kick`) and read back on a LATER one, once
    /// the GPU has actually finished the reduction (`gfx::field_collect`; [`FrameLatch::Pending`]
    /// until then). Reading it on the frame that asked — which this used to do whenever the
    /// owner's surface was already visible — made the `glReadPixels` wait for everything the GPU
    /// had queued: 26–37 ms of a modal's open frame on the television (2026-09-19). A field that
    /// is not latched yet draws its owner's fallback (the flat ground, the flat panel sheet) for
    /// the frame or two the read takes, at the very bottom of an appear ramp.
    ///
    /// `src` is `gfx::field_kick`'s: a texture that already holds the page, or `None` for the
    /// framebuffer as it stands. [`FrameLatch::Refused`] is `field_kick`'s refusals (a blur source
    /// pass, a video-plane frame, a frozen page, a drawable the exact-2x chain cannot be built
    /// for): the caller keeps what it was drawing, may ask again next frame, or falls back to
    /// [`latch_from_corners`](Self::latch_from_corners).
    pub(crate) fn latch_from_frame(&mut self, grade: Grade, src: Option<u32>) -> FrameLatch {
        if self.latched {
            return FrameLatch::Latched;
        }
        if let Some(t) = self.pending.take() {
            match gfx::field_collect(t) {
                gfx::FieldRead::Ready(raw) => {
                    let c = cells_from_frame(&raw, grade);
                    self.adopt(c);
                    return FrameLatch::Latched;
                }
                gfx::FieldRead::Pending => {
                    self.pending = Some(t);
                    crate::ui::idle::wake();
                    return FrameLatch::Pending;
                }
                gfx::FieldRead::Lost => {}
            }
        }
        match gfx::field_kick(src) {
            Some(t) => {
                self.pending = Some(t);
                // A frame for the read to land in, whether or not anything else moves.
                crate::ui::idle::wake();
                FrameLatch::Pending
            }
            None => FrameLatch::Refused,
        }
    }

    /// **Latch from a four-corner envelope instead of the framebuffer** — the CPU source, for the
    /// two cases that have no readable frame: an overlay over the hardware video plane, and a
    /// route that opens before Home has ever drawn.
    ///
    /// It deliberately does NOT low-pass. The source is already a bilinear surface, i.e. band-
    /// limited far below this grid, and the edge-clamped kernel would only flatten it against the
    /// border — while this is precisely the path that has to agree with the `AmbientWash` it
    /// stands in for. Grading the CORNERS and then interpolating is also the order `AmbientWash`
    /// uses, and the order matters: `ground_capped` is not linear.
    pub(crate) fn latch_from_corners(&mut self, corners: [[f32; 3]; 4], grade: Grade) {
        if self.latched {
            return;
        }
        self.adopt(cells_from_corners(corners, grade));
    }

    /// **Replace the latch with a grid somebody else already sampled** — the RE-latch, for an owner
    /// whose page changed under a field that is already up (the container's, when the host
    /// snapshot is re-taken: `containers::modal::ModalUnderlay`).
    ///
    /// Unlike [`latch_from_frame`](Self::latch_from_frame) it is NOT idempotent, and that is the
    /// point: the owner samples FIRST and only calls this with a real answer, so a frame on which
    /// `gfx::field_kick` has none (a blur source pass, a frozen page) keeps the field
    /// it had instead of dropping to the flat dim for a frame. It is also the seam a host test
    /// drives the latch through, since the sample is the one step that needs a GL context.
    pub(crate) fn latch_sampled(&mut self, raw: &[[f32; 3]; N], grade: Grade) {
        self.adopt(cells_from_frame(raw, grade));
    }

    /// Re-arm. The texture name is kept — the next latch re-specs it — because a field that is
    /// reset is a field that is about to be latched again.
    pub(crate) fn reset(&mut self) {
        self.cells = [[0.0; 3]; N];
        self.latched = false;
        self.pending = None;
    }

    pub(crate) fn is_latched(&self) -> bool {
        self.latched
    }

    /// **The field's own colour at one screen point**, display-encoded — the same contract as
    /// `RouteGround::sample` and `AmbientWash::sample`, and for the same caller: an edge fade that
    /// has to paint the exact colour of the ground it is cutting across.
    ///
    /// Unlike those two it is not a bilinear of four corners, so the answer differs from side to
    /// side of the screen as the page under it does. Points outside `Rect::FULL` clamp.
    pub(crate) fn sample(&self, x: f32, y: f32) -> [f32; 3] {
        let u = (x / crate::ui::consts::SCR_W).clamp(0.0, 1.0);
        let v = (y / crate::ui::consts::SCR_H).clamp(0.0, 1.0);
        reconstruct(&self.cells, u, v).map(gfx::enc)
    }

    /// **The one colour that stands for the whole field** — the mean taken in LINEAR light and
    /// returned display-encoded, the same contract the old four-corner sampler's `key` field kept
    /// (mean-in-linear, encoded back), so `RouteGround::palette` keys a `ControlPalette` from this
    /// wherever it used to key one from that.
    pub(crate) fn key(&self) -> [f32; 3] {
        let mut acc = [0.0f32; 3];
        for c in &self.cells {
            for (a, v) in acc.iter_mut().zip(c) {
                *a += v;
            }
        }
        acc.map(|v| gfx::enc(v / N as f32))
    }

    /// Paint it over `r`. See [`Role`] for what each variant means and [`plan`] for the dim's
    /// bit-identity contract.
    pub(crate) fn draw(&self, p: Painter, r: Rect, role: Role, alpha: f32) {
        match role {
            Role::Ground => {
                if !self.latched || self.tex == 0 {
                    // Nothing sampled yet: the app's own flat ground, which is what the page would
                    // have been anyway (`gfx::frame_clear` lays down the same colour).
                    p.rect(r, 0.0, theme::SURFACE_APP, theme::SURFACE_APP, 0.0);
                    return;
                }
                p.field_ground(r, self.tex, alpha);
            }
            Role::Dim { weight } => match plan(self.latched && self.tex != 0, weight, alpha) {
                Draw::Flat(c) => p.rect(r, 0.0, c, c, 0.0),
                Draw::Field(tint) => p.field(r, self.tex, tint),
            },
        }
    }

    /// **A popover panel's material, decided** — see [`PanelDraw`]. `screen` is the panel's rect
    /// as DRAWN (the cascade's translate folded in); `weight` is `theme::underlay::PANEL_TINT` or
    /// its sweep.
    ///
    /// The tint is one scalar for the whole window: `weight`, lowered just far enough that the
    /// brightest texel the panel covers lands at `theme::underlay::PANEL_LUMA_MAX` — the ground's
    /// `ground_capped` rule, applied per panel rather than per cell so that the field's shape
    /// under the panel survives the cap instead of being flattened by it.
    pub(crate) fn panel_plan(&self, screen: Rect, weight: f32) -> PanelDraw {
        if !self.latched {
            return PanelDraw::Flat;
        }
        let peak = self.peak_luma(screen);
        let cap = crate::ui::theme::underlay::PANEL_LUMA_MAX;
        let w = weight.clamp(0.0, 1.0);
        let k = if peak * w > cap { cap / peak } else { w };
        PanelDraw::Field {
            uv: panel_uv(screen),
            tint: [k, k, k, 1.0],
        }
    }

    /// **Paint the field as a panel's material over `r`** (corner `radius`) through `p`. Returns
    /// whether it drew; `false` — unlatched, or no program/texture — is the caller's cue to lay
    /// down the flat sheet (`widgets::panel_ground`).
    pub(crate) fn draw_panel(&self, p: Painter, r: Rect, radius: f32, weight: f32) -> bool {
        let (_, screen, _) = p.to_screen(r);
        match self.panel_plan(screen, weight) {
            PanelDraw::Flat => false,
            PanelDraw::Field { uv, tint } => p.field_panel(r, radius, self.tex, uv, tint),
        }
    }

    /// The brightest texel the magnified field can put inside `screen`, 0..1. Bilinear
    /// magnification never leaves the hull of the two texels either side of a point, so the texels
    /// bracketing the rect's texel-centre span bound every fragment in it.
    fn peak_luma(&self, screen: Rect) -> f32 {
        let span = |lo: f32, len: f32, extent: f32, n: usize| -> (usize, usize) {
            let a = (lo / extent * n as f32 - 0.5).floor();
            let b = ((lo + len) / extent * n as f32 - 0.5).floor() + 1.0;
            let clamp = |v: f32| v.clamp(0.0, (n - 1) as f32) as usize;
            (clamp(a), clamp(b))
        };
        let (i0, i1) = span(screen.x, screen.w, crate::ui::consts::SCR_W, TEX_W);
        let (j0, j1) = span(screen.y, screen.h, crate::ui::consts::SCR_H, TEX_H);
        let mut peak = 0u8;
        for j in j0..=j1 {
            for &l in &self.luma[j * TEX_W + i0..=j * TEX_W + i1] {
                peak = peak.max(l);
            }
        }
        peak as f32 / 255.0
    }

    fn adopt(&mut self, cells: [[f32; 3]; N]) {
        self.cells = cells;
        let px = texture_rgba(&self.cells);
        for (l, t) in self.luma.iter_mut().zip(px.chunks_exact(4)) {
            let y = 0.2126 * t[0] as f32 + 0.7152 * t[1] as f32 + 0.0722 * t[2] as f32;
            *l = (y + 0.5).min(255.0) as u8;
        }
        self.tex = upload(self.tex, &px);
        self.latched = true;
    }
}

impl Drop for UnderlayField {
    fn drop(&mut self) {
        gfx::delete_tex(self.tex);
        self.tex = 0;
    }
}

/// Upload the reconstruction, or — in a HOST TEST — do not.
///
/// The same seam, and the same reason, as `gfx::delete_tex`'s `cfg(not(test))` arm: the test
/// binary LINKS OpenGL but never creates a context, so the driver's per-thread dispatch table is a
/// null vtable and `glTexImage2D` dereferences a fixed offset into it — an immediate SIGSEGV that
/// takes the whole test binary down. Everything a test wants to grade about this module is the
/// arithmetic above the upload, and it is all reachable with `tex` left at 0.
fn upload(prev: u32, px: &[u8]) -> u32 {
    #[cfg(not(test))]
    {
        gfx::upload_rgba(prev, TEX_W as i32, TEX_H as i32, px.as_ptr())
    }
    #[cfg(test)]
    {
        let _ = px;
        prev
    }
}

// ── The pure pipeline ────────────────────────────────────────────────────────────────────────

/// Steps 1–3 for a grid sampled off the framebuffer: linearise, low-pass, grade, and hand back
/// LINEAR graded cells.
pub(crate) fn cells_from_frame(raw: &[[f32; 3]; N], grade: Grade) -> [[f32; 3]; N] {
    let lin: [[f32; 3]; N] = std::array::from_fn(|i| raw[i].map(gfx::lin));
    let smooth = low_pass(&lin);
    std::array::from_fn(|i| graded(smooth[i], grade))
}

/// [`cells_from_frame`]'s counterpart for a four-corner envelope: grade the CORNERS, then evaluate
/// the bilinear at the cell centres — the order and the space `AmbientWash` uses.
///
/// Corner order is the painter's, and the wash's: top-left, top-right, bottom-right, bottom-left.
pub(crate) fn cells_from_corners(corners: [[f32; 3]; 4], grade: Grade) -> [[f32; 3]; N] {
    let k = corners.map(|c| match grade {
        Grade::Ground => {
            let g = AmbientWash::keyed_one(c, AmbientWash::GROUND_W);
            [g[0], g[1], g[2]]
        }
        Grade::Dim => c,
    });
    std::array::from_fn(|i| {
        let (col, row) = (i % W, i / W);
        let u = (col as f32 + 0.5) / W as f32;
        let v = (row as f32 + 0.5) / H as f32;
        std::array::from_fn(|ch| {
            let top = k[0][ch] + (k[1][ch] - k[0][ch]) * u; // tl -> tr
            let bot = k[3][ch] + (k[2][ch] - k[3][ch]) * u; // bl -> br
            gfx::lin(top + (bot - top) * v)
        })
    })
}

/// One LINEAR cell through the grade, back to LINEAR.
///
/// The encode/decode pair around the middle is not a round trip for nothing: `ground_capped`'s
/// ceiling is Rec.709 over the STORED display codes (its own doc says so, and the contrast numbers
/// that justify 0.42 were measured that way), and `theme::mix` is the wash's mix, also over stored
/// codes. Grading in linear light would be a different, unmeasured design.
fn graded(c: [f32; 3], grade: Grade) -> [f32; 3] {
    match grade {
        Grade::Dim => c,
        Grade::Ground => {
            let d = c.map(gfx::enc);
            let g = AmbientWash::keyed_one(d, AmbientWash::GROUND_W);
            [gfx::lin(g[0]), gfx::lin(g[1]), gfx::lin(g[2])]
        }
    }
}

/// `exp(-d²/2σ²)` at `d = 0..=RADIUS` for [`SIGMA`], as literals: the transcendental surface
/// outside `ui/motion.rs` is allow-listed and "shrink, never grow" (`ci/allow/libm.txt`), and a
/// fixed kernel has no business calling `exp` at run time. The host test
/// `the_kernel_table_is_the_gaussian_at_sigma` recomputes it from the formula.
const GAUSS: [f32; RADIUS + 1] =
    [1.0, 0.706_648_3, 0.249_352_2, 0.043_936_93, 0.003_865_920];

/// The separable Gaussian's one-dimensional kernel, normalised.
fn kernel() -> [f32; 2 * RADIUS + 1] {
    let mut k = [0.0f32; 2 * RADIUS + 1];
    let mut sum = 0.0;
    for (i, w) in k.iter_mut().enumerate() {
        *w = GAUSS[i.abs_diff(RADIUS)];
        sum += *w;
    }
    for w in k.iter_mut() {
        *w /= sum;
    }
    k
}

/// Separable Gaussian over the grid, edge-clamped. Normalised, so a FLAT field is preserved
/// exactly — a low-pass that invents or loses energy on a uniform input is a bug, not a taste.
pub(crate) fn low_pass(cells: &[[f32; 3]; N]) -> [[f32; 3]; N] {
    let k = kernel();
    let mut mid = [[0.0f32; 3]; N];
    for row in 0..H {
        for col in 0..W {
            for ch in 0..3 {
                let mut acc = 0.0;
                for (t, w) in k.iter().enumerate() {
                    let s = (col as isize + t as isize - RADIUS as isize)
                        .clamp(0, W as isize - 1) as usize;
                    acc += cells[row * W + s][ch] * w;
                }
                mid[row * W + col][ch] = acc;
            }
        }
    }
    let mut out = [[0.0f32; 3]; N];
    for row in 0..H {
        for col in 0..W {
            for ch in 0..3 {
                let mut acc = 0.0;
                for (t, w) in k.iter().enumerate() {
                    let s = (row as isize + t as isize - RADIUS as isize)
                        .clamp(0, H as isize - 1) as usize;
                    acc += mid[s * W + col][ch] * w;
                }
                out[row * W + col][ch] = acc;
            }
        }
    }
    out
}

/// One channel of the grid at integer cell `(i, j)`, with LINEAR EXTRAPOLATION outside it.
///
/// Extrapolation rather than clamping, and it is the difference between a field that reaches the
/// edge of the screen and one that goes flat for its outer half-cell — 64 screen pixels, which on
/// this page is where the artwork usually is. It also makes the reconstruction EXACT on linear
/// data everywhere rather than only in the interior, which is what lets
/// [`UnderlayField::latch_from_corners`] reproduce the bilinear it replaces.
fn cell_at(cells: &[[f32; 3]; N], i: isize, j: isize, ch: usize) -> f32 {
    // (anchor, mirror, k): value = v[anchor] + (v[anchor] - v[mirror]) * k
    let ix = |i: isize, n: isize| -> (usize, usize, f32) {
        if i < 0 {
            (0, 1.min(n - 1) as usize, (-i) as f32)
        } else if i >= n {
            ((n - 1) as usize, (n - 2).max(0) as usize, (i - n + 1) as f32)
        } else {
            (i as usize, i as usize, 0.0)
        }
    };
    let (ia, ib, ik) = ix(i, W as isize);
    let (ja, jb, jk) = ix(j, H as isize);
    let v = |i: usize, j: usize| cells[j * W + i][ch];
    // Extrapolate along x first, then y. Both are affine, so the order cannot change the answer.
    let row = |j: usize| v(ia, j) + (v(ia, j) - v(ib, j)) * ik;
    row(ja) + (row(ja) - row(jb)) * jk
}

/// Uniform Catmull-Rom (tension 1/2) through four knots, **clamped to the bracketing pair**.
///
/// The clamp is the whole reason a cubic is safe here. Catmull-Rom overshoots a step by up to ~9%
/// of it, and an overshoot in a colour field is not a ringing artefact you squint at — it is a
/// colour the page does not contain, sitting in a 128px-wide band. Bracketing by `p1..p2` costs
/// two `min`/`max` and cannot touch data that is already monotone, which linear data always is.
fn crom(p: [f32; 4], t: f32) -> f32 {
    let a = -0.5 * p[0] + 1.5 * p[1] - 1.5 * p[2] + 0.5 * p[3];
    let b = p[0] - 2.5 * p[1] + 2.0 * p[2] - 0.5 * p[3];
    let c = -0.5 * p[0] + 0.5 * p[2];
    let v = ((a * t + b) * t + c) * t + p[1];
    v.clamp(p[1].min(p[2]), p[1].max(p[2]))
}

/// **The reconstructed field at `(u, v)` in `[0,1]²` of the rect, in LINEAR light.** Cell `(i, j)`
/// sits at its CENTRE, `((i+0.5)/15, (j+0.5)/8)` — a cell is an area, not a lattice point, and
/// putting the knots on the boundaries instead would shift the whole field half a cell left.
pub(crate) fn reconstruct(cells: &[[f32; 3]; N], u: f32, v: f32) -> [f32; 3] {
    let tx = u.clamp(0.0, 1.0) * W as f32 - 0.5;
    let ty = v.clamp(0.0, 1.0) * H as f32 - 0.5;
    let (i0, j0) = (tx.floor(), ty.floor());
    let (fx, fy) = (tx - i0, ty - j0);
    let (i0, j0) = (i0 as isize, j0 as isize);
    std::array::from_fn(|ch| {
        let col: [f32; 4] = std::array::from_fn(|k| {
            let j = j0 - 1 + k as isize;
            let p: [f32; 4] = std::array::from_fn(|m| cell_at(cells, i0 - 1 + m as isize, j, ch));
            crom(p, fx)
        });
        crom(col, fy)
    })
}

/// The 60x32 display-encoded RGBA8 the shader samples. Alpha is opaque: coverage is the tint's
/// business (`fs_field.frag` multiplies `c.a * u_tint.a`), never the texture's.
///
/// **[`reconstruct`] evaluated SEPARABLY**, and to the bit: `reconstruct` already runs its cubic
/// along x for each of four rows and then once along y, so the x pass depends only on a texel's
/// COLUMN and a grid row, and is shared by every texel of that column. Computing it once per
/// (row, column) and then running the y pass per texel performs exactly the arithmetic
/// `reconstruct` does, in the same order, with a quarter of the cubics and none of the per-tap
/// extrapolation — `a_separable_texture_is_reconstruct_to_the_bit` holds it to that. Evaluated per
/// texel it was 7.5–8.6 ms of the frame a modal's dim first latched on the television
/// (2026-09-19), the one CPU cost left in a frame the GPU already fills.
pub(crate) fn texture_rgba(cells: &[[f32; 3]; N]) -> [u8; TEX_W * TEX_H * 4] {
    // The grid rows a texel's y pass can reach: `j0 - 1 ..= j0 + 2` over every texel row.
    let row_lo = texel_knot(0, TEX_H, H).0 - 1;
    let row_hi = texel_knot(TEX_H - 1, TEX_H, H).0 + 2;
    let rows = (row_hi - row_lo + 1) as usize;
    // x pass: `xs[r][i][ch]` is the cubic along x through grid row `row_lo + r` at texel column i.
    let mut xs = vec![[[0.0f32; 3]; TEX_W]; rows];
    for (r, out) in xs.iter_mut().enumerate() {
        let j = row_lo + r as isize;
        for (i, o) in out.iter_mut().enumerate() {
            let (i0, fx) = texel_knot(i, TEX_W, W);
            *o = std::array::from_fn(|ch| {
                let p: [f32; 4] = std::array::from_fn(|m| cell_at(cells, i0 - 1 + m as isize, j, ch));
                crom(p, fx)
            });
        }
    }
    let mut px = [0u8; TEX_W * TEX_H * 4];
    for j in 0..TEX_H {
        let (j0, fy) = texel_knot(j, TEX_H, H);
        for i in 0..TEX_W {
            let o = (j * TEX_W + i) * 4;
            for ch in 0..3 {
                let col: [f32; 4] =
                    std::array::from_fn(|k| xs[(j0 - 1 + k as isize - row_lo) as usize][i][ch]);
                let c = crom(col, fy);
                px[o + ch] = (gfx::enc(c).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
            px[o + 3] = 255;
        }
    }
    px
}

/// Texel `t` of `n` across a grid of `cells`: its left/top knot and the fraction past it —
/// [`reconstruct`]'s own `(i0, fx)` for `u = (t + 0.5) / n`, by the same expressions.
fn texel_knot(t: usize, n: usize, cells: usize) -> (isize, f32) {
    let u = (t as f32 + 0.5) / n as f32;
    let x = u.clamp(0.0, 1.0) * cells as f32 - 0.5;
    let x0 = x.floor();
    (x0 as isize, x - x0)
}

#[cfg(test)]
#[path = "underlay_tests.rs"]
mod tests;
