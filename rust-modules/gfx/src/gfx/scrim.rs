//! **The ground shade: soft rounded blocks of ONE ink, evaluated per vertex.**
//!
//! A hero's shade is a darkening of the photograph under its copy, and it follows the copy: one
//! soft block per piece of content drawn on the art (the logo, a text column, the action row, the
//! pager), each a rounded rectangle whose darkness holds inside it and feathers to nothing
//! over a stated distance outside it. The ground's one pass ([`super::draw_ambient_inked`],
//! [`super::draw_art_wash`]) already carries a vertical ink ramp per VERTEX so its fragment does no
//! colour arithmetic (the Mali-T820 is arithmetic-bound; `vs_ambient.vert` records the
//! measurements). This is the same move for a field no straight ramp can describe: the vertex
//! shader evaluates every block at each vertex of the field mesh and stacks them,
//! `1 - prod(1 - a_i)`, into the alpha the fragment already reads. No new pass, no new fragment
//! arithmetic, no full-screen quad.
//!
//! **A block is the rounded-rect signed distance** `sd = |max(|p - c| - (h - r), 0)| - r` (centre
//! `c`, half extents `h`, corner radius `r`) and the alpha `peak * (1 - smoothstep(0, feather,
//! sd))`: `peak` anywhere inside the rounded rect, falling smoothly (zero slope at both ends) to
//! exactly 0 at `feather` px outside it. [`ShadeBlock::at`] and [`GroundShade::at`] are the SAME
//! arithmetic on the CPU: the legibility contract is graded on them, and the tests walk the real
//! field mesh against them (`mesh_error`), which is what bounds what the vertex-evaluated field can
//! differ from the closed form by.
//!
//! The blocks come from the screen's LIVE layout every frame, so a block that moves (Detail's logo
//! rising into the trailer corner) carries its shade with it: the screen builds a new
//! [`GroundShade`] from the rects it is drawing, and the upload cache below sends it whenever it
//! changed. [`MAX_BLOCKS`] is the size of the uniform array, a limit of the shader and not of any
//! screen's content (`ui::hero_shade` folds an overflowing block into its neighbour).
//!
//! **Zero is exact.** An unused block is all zeros: its `peak` is 0, so its alpha is exactly `0.0`
//! (`0 * (1 - t)` for a finite `t`), and the stack of nothing is exactly `0.0` (`1 - 1*1*...`). It
//! is folded into the ink with `mix(inka, 1.0, s)`, which for `s == 0` is `inka * 1.0 + 0.0` — the
//! ramp's own alpha to the bit. So a screen with no shade is drawn by the very same programs as
//! before and gets the very same pixels.

/// Blocks the shader's uniform array holds. A limit of the program (two `vec4`s a block), sized for
/// the busiest hero: Detail's logo, text column, facts line, action row and credits column.
pub const MAX_BLOCKS: usize = 6;
/// `vec4`s the blocks take in `u_sc`: `(cx, cy, hx - r, hy - r)` then `(r, 1/feather, peak, 0)`, in
/// device pixels.
pub const VEC4S: usize = 2 * MAX_BLOCKS;

/// One soft rounded block of shade.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadeBlock {
    /// `(cx, cy, hx - r, hy - r)`: the rounded rect's centre and the half extents of the rectangle
    /// its corner arcs are centred on.
    frame: [f32; 4],
    /// `(r, 1 / feather, peak, 0)`.
    shape: [f32; 4],
}

impl ShadeBlock {
    /// No shade anywhere: all zeros, which the shader evaluates to exactly `0.0` (module doc).
    pub const NONE: ShadeBlock = ShadeBlock { frame: [0.0; 4], shape: [0.0; 4] };

    /// The block over `rect = (x, y, w, h)`: alpha `peak` inside it (corners rounded to `radius`,
    /// which is held to half its shorter side), falling to 0 over `feather` px beyond its edge.
    pub fn new(rect: [f32; 4], radius: f32, feather: f32, peak: f32) -> ShadeBlock {
        let [x, y, w, h] = rect;
        let (hx, hy) = (w.max(0.0) * 0.5, h.max(0.0) * 0.5);
        let r = radius.clamp(0.0, hx.min(hy));
        ShadeBlock {
            frame: [x + hx, y + hy, hx - r, hy - r],
            shape: [r, 1.0 / feather.max(f32::MIN_POSITIVE), peak, 0.0],
        }
    }

    /// The signed distance from `(x, y)` to the rounded rect's edge: negative inside it (clamped to
    /// the corner arcs' own inset), the shader's expression term for term.
    fn distance(&self, x: f32, y: f32) -> f32 {
        let (dx, dy) = (((x - self.frame[0]).abs() - self.frame[2]).max(0.0), ((y - self.frame[1]).abs() - self.frame[3]).max(0.0));
        (dx * dx + dy * dy).sqrt() - self.shape[0]
    }

    /// The block's alpha at `(x, y)`.
    pub fn at(&self, x: f32, y: f32) -> f32 {
        let t = (self.distance(x, y) * self.shape[1]).clamp(0.0, 1.0);
        self.shape[2] * (1.0 - t * t * (3.0 - 2.0 * t))
    }

    /// The block's peak alpha (0 for an unused one).
    pub fn peak(&self) -> f32 {
        self.shape[2]
    }

    /// The block's rounded rect as `(x, y, w, h)` and its corner radius.
    pub fn rect(&self) -> ([f32; 4], f32) {
        let r = self.shape[0];
        let (hx, hy) = (self.frame[2] + r, self.frame[3] + r);
        ([self.frame[0] - hx, self.frame[1] - hy, 2.0 * hx, 2.0 * hy], r)
    }

    /// The block's feather in px.
    pub fn feather(&self) -> f32 {
        1.0 / self.shape[1]
    }

    /// The peak times `k`.
    fn scaled(mut self, k: f32) -> ShadeBlock {
        self.shape[2] *= k;
        self
    }

    /// The block moved to a device frame where `x_dev = x * s + o`.
    fn placed(mut self, ox: f32, oy: f32, s: f32) -> ShadeBlock {
        self.frame = [self.frame[0] * s + ox, self.frame[1] * s + oy, self.frame[2] * s, self.frame[3] * s];
        self.shape[0] *= s;
        self.shape[1] /= s;
        self
    }
}

/// The blocks of a hero's shade, in the frame a draw is made in. Built by `ui::hero_shade` from the
/// rects a screen is drawing this frame; a screen with no shade passes [`GroundShade::NONE`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundShade {
    blocks: [ShadeBlock; MAX_BLOCKS],
}

impl GroundShade {
    /// No shade: all zeros, which the shader evaluates to exactly `0.0` (module doc).
    pub const NONE: GroundShade = GroundShade { blocks: [ShadeBlock::NONE; MAX_BLOCKS] };

    /// The shade of `blocks` (at most [`MAX_BLOCKS`]; a longer list is a bug in the caller, not a
    /// case to clip).
    pub fn from_blocks(blocks: &[ShadeBlock]) -> GroundShade {
        assert!(blocks.len() <= MAX_BLOCKS, "a ground shade carries at most {MAX_BLOCKS} blocks");
        let mut s = GroundShade::NONE;
        s.blocks[..blocks.len()].copy_from_slice(blocks);
        s
    }

    /// The live blocks (peak above 0).
    pub fn blocks(&self) -> impl Iterator<Item = &ShadeBlock> {
        self.blocks.iter().filter(|b| b.peak() > 0.0)
    }

    /// The stacked alpha at `(x, y)`: `1 - prod(1 - a_i)`, as the vertex shader computes it.
    pub fn at(&self, x: f32, y: f32) -> f32 {
        1.0 - self.blocks.iter().fold(1.0, |keep, b| keep * (1.0 - b.at(x, y)))
    }

    /// Every block's peak times `k` (a hero fade, a painter's alpha): each block, then stacked — not
    /// the stack times `k`.
    pub fn scaled(self, k: f32) -> GroundShade {
        GroundShade { blocks: self.blocks.map(|b| b.scaled(k)) }
    }

    /// The shade moved to the device frame a draw lands in, where an authored point `p` is at
    /// `p * s + (ox, oy)` (a painter's translate, and its zoom's scale).
    pub fn placed(self, ox: f32, oy: f32, s: f32) -> GroundShade {
        // No shade stays NONE exactly (not "zeros at an offset"): the draw's upload cache and the
        // glass's source identity both compare the whole value.
        if self == GroundShade::NONE {
            return self;
        }
        GroundShade { blocks: self.blocks.map(|b| if b.peak() > 0.0 { b.placed(ox, oy, s) } else { b }) }
    }

    /// The `u_sc` upload: per block `(cx, cy, hx - r, hy - r)` then `(r, 1/feather, peak, 0)`.
    pub fn uniforms(&self) -> [[f32; 4]; VEC4S] {
        let mut u = [[0.0; 4]; VEC4S];
        for (i, b) in self.blocks.iter().enumerate() {
            u[2 * i] = b.frame;
            u[2 * i + 1] = b.shape;
        }
        u
    }
}

#[cfg(any(test, feature = "test-support"))]
impl GroundShade {
    /// **The vertex shader's field over the REAL mesh**, for a test: each vertex of
    /// [`super::field_mesh`] carries the closed form [`at`](Self::at) and the rasterizer
    /// interpolates linearly inside a triangle. Returns the worst `|mesh - closed form|` over
    /// `rect` drawn as `cols x rows` separate mesh draws, sampled at 15 points inside every
    /// triangle. This is what bounds how far the painted field can stand from the field the
    /// legibility contract is graded on.
    pub fn mesh_error(&self, rect: [f32; 4], cols: usize, rows: usize) -> f32 {
        let mesh = super::field_mesh();
        let [rx, ry, rw, rh] = rect;
        let mut worst = 0.0f32;
        for cj in 0..rows {
            for ci in 0..cols {
                let (cx, cy) = (rx + rw * ci as f32 / cols as f32, ry + rh * cj as f32 / rows as f32);
                let (cw, ch) = (rw / cols as f32, rh / rows as f32);
                let at = |u: f32, v: f32| self.at(cx + u * cw, cy + v * ch);
                for tri in mesh.chunks(6) {
                    let (p0, p1, p2) = ((tri[0], tri[1]), (tri[2], tri[3]), (tri[4], tri[5]));
                    let (f0, f1, f2) = (at(p0.0, p0.1), at(p1.0, p1.1), at(p2.0, p2.1));
                    for i in 0..=4 {
                        for j in 0..=(4 - i) {
                            let (a, b) = (i as f32 / 4.0, j as f32 / 4.0);
                            let c = 1.0 - a - b;
                            let (u, v) = (a * p0.0 + b * p1.0 + c * p2.0, a * p0.1 + b * p1.1 + c * p2.1);
                            worst = worst.max((a * f0 + b * f1 + c * f2 - at(u, v)).abs());
                        }
                    }
                }
            }
        }
        worst
    }

    /// **The field the rasterizer paints at the authored point `(x, y)`** when `rect` is drawn as
    /// `cols x rows` mesh draws: the closed form at the three vertices of the mesh triangle the
    /// point is in, interpolated linearly (the triangles are [`super::field_mesh`]'s: a square's
    /// lower-left half, then its upper-right half). What a screen's content actually stands on.
    pub fn painted_at(&self, rect: [f32; 4], cols: usize, rows: usize, x: f32, y: f32) -> f32 {
        let [rx, ry, rw, rh] = rect;
        let (cw, ch) = (rw / cols as f32, rh / rows as f32);
        let cell = |p: f32, o: f32, len: f32, n: usize| (((p - o) / len).floor().max(0.0) as usize).min(n - 1);
        let (ci, cj) = (cell(x, rx, cw, cols), cell(y, ry, ch, rows));
        let (cx, cy) = (rx + cw * ci as f32, ry + ch * cj as f32);
        let n = super::FIELD_N as f32;
        let (u, v) = (((x - cx) / cw * n).clamp(0.0, n), ((y - cy) / ch * n).clamp(0.0, n));
        let (i, j) = (u.floor().min(n - 1.0), v.floor().min(n - 1.0));
        let (s, t) = (u - i, v - j);
        let at = |di: f32, dj: f32| self.at(cx + (i + di) / n * cw, cy + (j + dj) / n * ch);
        if s + t <= 1.0 {
            let f00 = at(0.0, 0.0);
            f00 + (at(1.0, 0.0) - f00) * s + (at(0.0, 1.0) - f00) * t
        } else {
            at(1.0, 0.0) * (1.0 - t) + at(1.0, 1.0) * (s + t - 1.0) + at(0.0, 1.0) * (1.0 - s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> ShadeBlock {
        ShadeBlock::new([200.0, 300.0, 400.0, 100.0], 24.0, 120.0, 0.6)
    }

    #[test]
    fn a_block_holds_its_peak_inside_and_feathers_to_exactly_nothing() {
        let b = card();
        assert_eq!(b.at(400.0, 350.0), 0.6, "the centre");
        assert_eq!(b.at(200.0, 300.0 + 100.0 * 0.5), 0.6, "an edge midpoint is still inside");
        assert_eq!(b.at(200.0 + 24.0 * 0.3, 300.0 + 24.0 * 0.3), 0.6, "inside the rounded corner's arc");
        assert!((b.at(200.0 - 60.0, 350.0) - 0.3).abs() < 1e-6, "half the feather out is half the peak");
        assert_eq!(b.at(200.0 - 120.0, 350.0), 0.0, "the feather's end");
        assert_eq!(b.at(0.0, 0.0), 0.0, "far away");
        // monotone non-increasing away from the edge, in every direction
        for dir in [(1.0f32, 0.0f32), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0), (0.7, 0.7)] {
            let mut prev = f32::INFINITY;
            for i in 0..=140 {
                let a = b.at(400.0 + dir.0 * (200.0 + i as f32), 350.0 + dir.1 * (50.0 + i as f32));
                assert!(a <= prev + 1e-7, "{dir:?} step {i}: {prev} -> {a}");
                prev = a;
            }
        }
    }

    #[test]
    fn a_corner_is_rounded_not_square() {
        let b = card();
        // the square corner's own point is outside the rounded rect, so it has begun to fall
        assert!(b.at(200.0, 300.0) < 0.6, "the sharp corner is cut by its radius");
        assert_eq!(b.at(200.0, 300.0 + 24.0), 0.6, "…and the arc's tangent point is not");
    }

    #[test]
    fn none_is_exactly_zero_everywhere() {
        for (x, y) in [(0.0, 0.0), (560.0, 590.0), (1919.0, 1079.0), (-30.0, 5000.0)] {
            assert_eq!(GroundShade::NONE.at(x, y), 0.0);
        }
        assert_eq!(GroundShade::NONE.uniforms(), [[0.0; 4]; VEC4S]);
    }

    /// **A screen with no shade is drawn to the bit as it always was.** The vertex shader folds the
    /// stack into the ramp's alpha as `mix(inka, 1.0, s)` = `inka * (1.0 - s) + 1.0 * s`; with
    /// every uniform zero `s` is exactly `0.0`, and that expression is `inka` unchanged for every
    /// float. Both halves are held here: the stack of nothing is `0.0`, and `x * 1.0 + 1.0 * 0.0`
    /// is `x`.
    #[test]
    fn no_shade_leaves_the_ramps_alpha_to_the_bit() {
        let u = GroundShade::NONE.uniforms();
        let s = {
            // the shader's own arithmetic over the all-zero uniforms, term for term
            let term = |i: usize, px: [f32; 2]| {
                let (dx, dy) = (((px[0] - u[2 * i][0]).abs() - u[2 * i][2]).max(0.0), ((px[1] - u[2 * i][1]).abs() - u[2 * i][3]).max(0.0));
                let t = (((dx * dx + dy * dy).sqrt() - u[2 * i + 1][0]) * u[2 * i + 1][1]).clamp(0.0, 1.0);
                u[2 * i + 1][2] * (1.0 - t * t * (3.0 - 2.0 * t))
            };
            (0..MAX_BLOCKS).fold(1.0f32, |keep, i| keep * (1.0 - term(i, [960.0, 540.0])))
        };
        assert_eq!((1.0 - s).to_bits(), 0.0f32.to_bits());
        let s = 1.0 - s;
        for i in 0..=1000 {
            let inka = i as f32 / 1000.0;
            assert_eq!((inka * (1.0 - s) + 1.0 * s).to_bits(), inka.to_bits(), "inka={inka}");
        }
    }

    fn sample() -> GroundShade {
        GroundShade::from_blocks(&[
            ShadeBlock::new([96.0, 500.0, 943.0, 120.0], 28.0, 140.0, 0.6),
            ShadeBlock::new([96.0, 668.0, 420.0, 60.0], 30.0, 140.0, 0.5),
        ])
    }

    #[test]
    fn blocks_stack_as_independent_layers_of_one_ink() {
        let s = sample();
        let (x, y) = (300.0, 640.0);
        let (a, b) = (s.blocks[0].at(x, y), s.blocks[1].at(x, y));
        assert!((s.at(x, y) - (1.0 - (1.0 - a) * (1.0 - b))).abs() < 1e-7);
        assert!(a > 0.0 && b > 0.0, "a point where two blocks overlap");
    }

    #[test]
    fn scaling_scales_each_block_then_stacks() {
        let s = sample();
        for k in [0.0, 0.3, 1.0] {
            let ks = s.scaled(k);
            for (x, y) in [(10.0, 10.0), (300.0, 640.0), (600.0, 700.0)] {
                let want = 1.0 - (1.0 - k * s.blocks[0].at(x, y)) * (1.0 - k * s.blocks[1].at(x, y));
                assert!((ks.at(x, y) - want).abs() < 1e-6, "k={k} ({x},{y})");
            }
        }
        assert_eq!(s.scaled(0.0).at(300.0, 540.0), 0.0);
    }

    #[test]
    fn placing_moves_the_field_with_the_draw() {
        let s = sample();
        let (ox, oy, k) = (30.0, -40.0, 1.5);
        let p = s.placed(ox, oy, k);
        for (x, y) in [(0.0, 0.0), (300.0, 540.0), (600.0, 700.0), (1500.0, 200.0), (96.0 - 60.0, 560.0)] {
            assert!((p.at(x * k + ox, y * k + oy) - s.at(x, y)).abs() < 1e-5, "({x},{y})");
        }
        assert_eq!(GroundShade::NONE.placed(ox, oy, k), GroundShade::NONE, "no shade stays no shade");
    }

    #[test]
    fn a_block_reads_back_the_rect_it_was_made_from() {
        let (r, rad) = card().rect();
        assert_eq!((r, rad), ([200.0, 300.0, 400.0, 100.0], 24.0));
        // the feather is carried as its reciprocal (one multiply a vertex), so it reads back to a ulp
        assert!((card().feather() - 120.0).abs() < 1e-3);
    }

    /// A radius past half the shorter side is held there (a stadium), never an inverted corner.
    #[test]
    fn the_radius_is_held_to_half_the_shorter_side() {
        let b = ShadeBlock::new([0.0, 0.0, 200.0, 40.0], 500.0, 50.0, 0.5);
        assert_eq!(b.rect().1, 20.0);
        assert_eq!(b.at(100.0, 20.0), 0.5);
    }

    /// The painted point is the closed form at the mesh's own vertices, and nowhere farther from it
    /// than `mesh_error` says (it is the same interpolation, sampled at the point asked for).
    #[test]
    fn the_painted_point_is_the_closed_form_at_vertices_and_inside_the_mesh_error_elsewhere() {
        let s = sample();
        let panel = [0.0, 0.0, 1920.0, 1080.0];
        let n = super::super::FIELD_N as f32;
        for (ci, cj) in [(0usize, 0usize), (1, 2), (3, 3)] {
            for (i, j) in [(0.0f32, 0.0f32), (5.0, 7.0), (16.0, 16.0)] {
                let (x, y) = ((ci as f32 + i / n) * 480.0, (cj as f32 + j / n) * 270.0);
                assert!((s.painted_at(panel, 4, 4, x, y) - s.at(x, y)).abs() < 1e-5, "vertex ({x},{y})");
            }
        }
        let bound = s.mesh_error(panel, 4, 4);
        let mut worst = 0.0f32;
        for iy in 0..=108 {
            for ix in 0..=192 {
                let (x, y) = (ix as f32 * 10.0, iy as f32 * 10.0);
                worst = worst.max((s.painted_at(panel, 4, 4, x, y) - s.at(x, y)).abs());
            }
        }
        assert!(worst <= bound + 1e-5, "{worst} vs the mesh error {bound}");
        assert!(worst > 0.0, "the premise: the mesh is not exact between vertices");
    }

    #[test]
    fn finer_cells_carry_the_field_closer() {
        let s = sample();
        let panel = [0.0, 0.0, 1920.0, 1080.0];
        let one = s.mesh_error(panel, 1, 1);
        let four = s.mesh_error(panel, 4, 4);
        assert!(one > 0.0 && four < one, "{one} -> {four}");
    }
}
