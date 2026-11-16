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
//! The blocks come from Catalog's live layout every frame, so a moving block carries its shade.
//! The screen builds a new
//! [`GroundShade`] from the rects it is drawing, and the upload cache below sends it whenever it
//! changed. [`MAX_BLOCKS`] is the size of the uniform array, a limit of the shader and not of any
//! screen's content (`ui::hero_shade` folds an overflowing block into its neighbour).
//!
//! **Zero is exact.** An unused block is all zeros: its `peak` is 0, so its alpha is exactly `0.0`
//! (`0 * (1 - t)` for a finite `t`), and the stack of nothing is exactly `0.0` (`1 - 1*1*...`). It
//! is folded into the ink with `mix(inka, 1.0, s)`, which for `s == 0` is `inka * 1.0 + 0.0` — the
//! ramp's own alpha to the bit. So a screen with no shade is drawn by the very same programs as
//! before and gets the very same pixels. And an unused block is a factor of exactly `1.0`, so the
//! shader runs only the first [`GroundShade::live`] blocks (a uniform count; the live blocks are a
//! prefix): the stack stops there without changing a bit of it, a ground with no shade pays one
//! compare a vertex, a hero with three blocks pays three.
//!
//! **Zero is also local.** A block is exactly 0 from `feather` px beyond its rounded rect on, so the
//! shade has a SUPPORT, and [`GroundShade::reaches`] asks whether a rect meets it. The layer that
//! paints the shade alone ([`super::draw_scrim_layer`]) pays one vertex-shaded mesh a cell whatever
//! the cell holds; it asks first, and a cell the shade cannot reach is not drawn (it could only
//! have blended zeros), nor are the rows of a cell's mesh above or below the part the shade reaches
//! ([`GroundShade::reached_rows`]).

/// Blocks the shader's uniform array holds. A limit of the program (two `vec4`s a block), sized for
/// the busiest hero: Detail's logo, text column, facts line, action row and credits column.
pub const MAX_BLOCKS: usize = 6;
/// `vec4`s the blocks take in `u_sc`: `(cx, cy, hx - r, hy - r)` then `(r, 1/feather, peak, 0)`, in
/// device pixels.
pub const VEC4S: usize = 2 * MAX_BLOCKS;

/// Pixels [`ShadeBlock::reaches`] errs on the reaching side of a block's support. The shader reads
/// `feather` as the reciprocal it was handed (one rounding) and computes the distance in `f32` at
/// coordinates of a few thousand, so its alpha can be a hair above 0 a hair past the closed form's
/// edge; a pixel is generously more than either, and costs a row of mesh squares at most.
const REACH_SLACK: f32 = 1.0;

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

    /// **Can this block's alpha be above 0 anywhere in `rect = (x, y, w, h)`?** `false` is EXACT:
    /// every point of the rect is `feather` px or more from the rounded rect, where [`at`](Self::at)
    /// is exactly `0.0` (and so is the shader's vertex alpha, and the interpolation of zeros). `true`
    /// is conservative by [`REACH_SLACK`] px.
    ///
    /// The block is `peak` inside the rounded rect and nothing from `feather` beyond it, so the
    /// block's support is that rounded rect grown by `feather`, which this tests against `rect`
    /// exactly (the nearest point of the rect to the core rectangle the corner arcs are centred on,
    /// then [`distance`](Self::distance)'s own expression there) rather than against a bounding box
    /// of it, so a rect that only meets the box's square corner, outside the arc, is not reached.
    pub fn reaches(&self, rect: [f32; 4]) -> bool {
        if self.shape[2] <= 0.0 {
            return false;
        }
        let [x, y, w, h] = rect;
        // Per axis, the least `max(|p - c| - half, 0)` over `p` in `[lo, hi]`: how far the centre is
        // outside the interval, less the core's half extent, held at 0.
        let gap = |lo: f32, hi: f32, c: f32, half: f32| ((lo - c).max(c - hi).max(0.0) - half).max(0.0);
        let (dx, dy) = (gap(x, x + w, self.frame[0], self.frame[2]), gap(y, y + h, self.frame[1], self.frame[3]));
        let nearest = (dx * dx + dy * dy).sqrt() - self.shape[0];
        // A NaN block is reached, as the shader would paint it.
        nearest.is_nan() || nearest < self.feather() + REACH_SLACK
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

/// Edge-anchored shade and shelf ramp sharing one ink. The overlap is a soft maximum,
/// so it cannot produce the dark seam of two independently blended overlays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeShade {
    horizontal: [f32; 2],
    vertical: [f32; 2],
    top: Option<([f32; 2], f32)>,
    shelf: [(f32, f32); 3],
    peak: f32,
    join: f32,
}

impl EdgeShade {
    /// An optional full-width top fade, given as its vertical bounds and peak alpha. Its
    /// alpha and the side alpha can crossfade before the lower ramp joins them.
    pub fn new(horizontal: [f32; 2], vertical: [f32; 2], top: Option<([f32; 2], f32)>,
        shelf: [(f32, f32); 3], peak: f32, join: f32) -> Self {
        Self { horizontal, vertical, top, shelf, peak, join }
    }

    fn at(self, x: f32, y: f32) -> f32 {
        let progress = |v: f32, lo: f32, hi: f32| ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
        let smooth = |t: f32| t * t * (3.0 - 2.0 * t);
        let side = self.peak
            * (1.0 - smooth(progress(x, self.horizontal[0], self.horizontal[1])))
            * smooth(progress(y, self.vertical[0], self.vertical[1]));
        let upper = side + self.top.map_or(0.0, |([start, end], peak)| {
            peak * (1.0 - smooth(progress(y, start, end)))
        });
        let [(top, a), (mid, b), (foot, c)] = self.shelf;
        let shelf = if y < mid {
            a + (b - a) * progress(y, top, mid)
        } else {
            b + (c - b) * progress(y, mid, foot)
        };
        let mix = (self.join - (upper - shelf).abs()).max(0.0);
        (upper.max(shelf) + mix * mix * 0.25 / self.join).min(upper + shelf)
    }

    fn scaled(self, k: f32) -> Self {
        Self {
            shelf: self.shelf.map(|(y, a)| (y, a * k)),
            top: self.top.map(|(bounds, peak)| (bounds, peak * k)),
            peak: self.peak * k,
            join: self.join * k,
            ..self
        }
    }

    fn placed(self, ox: f32, oy: f32, s: f32) -> Self {
        Self {
            horizontal: self.horizontal.map(|x| x * s + ox),
            vertical: self.vertical.map(|y| y * s + oy),
            top: self.top.map(|(bounds, peak)| (bounds.map(|y| y * s + oy), peak)),
            shelf: self.shelf.map(|(y, a)| (y * s + oy, a)),
            ..self
        }
    }
}

/// The blocks of a hero's shade, in the frame a draw is made in. Built by `ui::hero_shade` from the
/// rects a screen is drawing this frame; a screen with no shade passes [`GroundShade::NONE`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundShade {
    blocks: [ShadeBlock; MAX_BLOCKS],
    edge: Option<EdgeShade>,
}

impl GroundShade {
    /// No shade: all zeros, which the shader evaluates to exactly `0.0` (module doc).
    pub const NONE: GroundShade = GroundShade { blocks: [ShadeBlock::NONE; MAX_BLOCKS], edge: None };

    /// A continuous field in place of the content blocks.
    pub fn from_edge(edge: EdgeShade) -> Self {
        Self { edge: Some(edge), ..Self::NONE }
    }

    /// The shader's mode/count uniform. The edge field has its own three-vector layout.
    pub fn shader_count(&self) -> f32 {
        if self.edge.is_some() { -1.0 } else { self.live() as f32 }
    }

    /// The shade of `blocks` (at most [`MAX_BLOCKS`]; a longer list is a bug in the caller, not a
    /// case to clip). The live blocks come FIRST, in the order given, and any unused one after them
    /// (a stable partition, which changes nothing for a list of live blocks): that is what makes
    /// [`live`](Self::live) a prefix the shader can stop at.
    pub fn from_blocks(blocks: &[ShadeBlock]) -> GroundShade {
        assert!(blocks.len() <= MAX_BLOCKS, "a ground shade carries at most {MAX_BLOCKS} blocks");
        let mut s = GroundShade::NONE;
        let live = blocks.iter().filter(|b| b.peak() > 0.0);
        let unused = blocks.iter().filter(|b| !(b.peak() > 0.0));
        for (slot, b) in s.blocks.iter_mut().zip(live.chain(unused)) {
            *slot = *b;
        }
        s
    }

    /// **How many blocks the vertex shader has to run** (`u_scn`): the live ones, counted to the last
    /// of them, so that a block past it is a factor of exactly 1.0 in the stack and stopping there
    /// changes no bit. 0 for [`GroundShade::NONE`] and for a shade scaled to nothing. For a shade
    /// built by [`from_blocks`](Self::from_blocks) it is simply the number of live blocks (they are
    /// a prefix); counting to the last keeps it right for any other order, at worst running a
    /// dead block.
    pub fn live(&self) -> usize {
        self.blocks.iter().rposition(|b| b.peak() > 0.0).map_or(0, |i| i + 1)
    }

    /// The live blocks (peak above 0).
    pub fn blocks(&self) -> impl Iterator<Item = &ShadeBlock> {
        self.blocks.iter().filter(|b| b.peak() > 0.0)
    }

    /// The stacked alpha at `(x, y)`: `1 - prod(1 - a_i)`, as the vertex shader computes it.
    pub fn at(&self, x: f32, y: f32) -> f32 {
        if let Some(edge) = self.edge {
            return edge.at(x, y);
        }
        1.0 - self.blocks.iter().fold(1.0, |keep, b| keep * (1.0 - b.at(x, y)))
    }

    /// **Can the stacked alpha be above 0 anywhere in `rect = (x, y, w, h)`?** `false` is exact: the
    /// field is `0.0` at every point of the rect ([`ShadeBlock::reaches`], any block). This is the
    /// SUPPORT of the shade asked cell by cell: a draw that would only paint zeros (a mesh cell of the
    /// scrim layer far from any copy) is skipped, which changes no pixel. [`GroundShade::NONE`]
    /// reaches nowhere.
    pub fn reaches(&self, rect: [f32; 4]) -> bool {
        if let Some(edge) = self.edge {
            let [x, y, _, h] = rect;
            return y + h > edge.shelf[0].0
                || (edge.peak > 0.0 && x < edge.horizontal[1] && y + h > edge.vertical[0])
                || edge.top.is_some_and(|([_, end], peak)| peak > 0.0 && y < end);
        }
        self.blocks.iter().any(|b| b.reaches(rect))
    }

    /// **The rows of a mesh over `rect` the shade can reach**: `rect` cut into `rows` equal horizontal
    /// strips, from the first strip [`reaches`](Self::reaches) to the last (`None`: none does, and the
    /// rect is not reached). A mesh stored row by row ([`super::field_mesh`]) can then be drawn from
    /// its first reached row through its last, one call, and the rows left out are the ones whose every
    /// vertex is alpha 0: the picture is the whole mesh's to the bit. Rows between the two are kept
    /// even when unreached (a single call, not one per run).
    pub fn reached_rows(&self, rect: [f32; 4], rows: usize) -> Option<std::ops::Range<usize>> {
        let [x, y, w, h] = rect;
        // both edges from the rect and the row index, as the shader's `a_pos.y * u_rect.w` places them
        let edge = |j: usize| y + h * j as f32 / rows as f32;
        let mut reached = (0..rows).filter(|&j| self.reaches([x, edge(j), w, edge(j + 1) - edge(j)]));
        let first = reached.next()?;
        Some(first..reached.last().unwrap_or(first) + 1)
    }

    /// Every block's peak times `k` (a hero fade, a painter's alpha): each block, then stacked — not
    /// the stack times `k`.
    pub fn scaled(self, k: f32) -> GroundShade {
        if k <= 0.0 { return Self::NONE; }
        GroundShade { blocks: self.blocks.map(|b| b.scaled(k)), edge: self.edge.map(|edge| edge.scaled(k)) }
    }

    /// The shade moved to the device frame a draw lands in, where an authored point `p` is at
    /// `p * s + (ox, oy)` (a painter's translate, and its zoom's scale).
    pub fn placed(self, ox: f32, oy: f32, s: f32) -> GroundShade {
        // No shade stays NONE exactly (not "zeros at an offset"): the draw's upload cache and the
        // glass's source identity both compare the whole value.
        if self == GroundShade::NONE {
            return self;
        }
        GroundShade {
            blocks: self.blocks.map(|b| if b.peak() > 0.0 { b.placed(ox, oy, s) } else { b }),
            edge: self.edge.map(|edge| edge.placed(ox, oy, s)),
        }
    }

    /// The `u_sc` upload: per block `(cx, cy, hx - r, hy - r)` then `(r, 1/feather, peak, 0)`. How
    /// many of them the shader runs is [`live`](Self::live), a uniform of its own.
    pub fn uniforms(&self) -> [[f32; 4]; VEC4S] {
        let mut u = [[0.0; 4]; VEC4S];
        if let Some(edge) = self.edge {
            let [left, right] = edge.horizontal;
            let [start, full] = edge.vertical;
            let [(top, a), (mid, b), (foot, c)] = edge.shelf;
            u[0] = [left, right, start, full];
            u[1] = [top, mid, foot, a];
            u[2] = [b, c, edge.peak, edge.join];
            if let Some(([start, end], peak)) = edge.top {
                u[3] = [start, end, peak, 0.0];
            }
            return u;
        }
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
    fn edge_shade_translates_and_scales_with_its_painter() {
        let edge = EdgeShade::new([280.0, 960.0], [110.0, 405.0], Some(([20.0, 260.0], 0.32)),
            [(768.0, 0.0), (886.0, 0.25), (1012.0, 0.93)], 0.59, 0.10);
        let shade = GroundShade::from_edge(edge);
        assert_eq!(shade.shader_count(), -1.0);
        let moved = shade.scaled(0.5).placed(13.0, -40.0, 0.75);
        for (x, y) in [(0.0, 0.0), (500.0, 190.0), (250.0, 580.0), (250.0, 950.0), (1900.0, 1040.0)] {
            assert!((moved.at(x * 0.75 + 13.0, y * 0.75 - 40.0) - shade.at(x, y) * 0.5).abs() < 1e-6);
        }
        assert_eq!(shade.scaled(0.0), GroundShade::NONE);
        assert!(shade.reaches([1000.0, 150.0, 500.0, 100.0]));
        assert!(!shade.reaches([1000.0, 260.0, 500.0, 500.0]));
        assert_eq!(moved.uniforms()[3], [-25.0, 155.0, 0.16, 0.0]);
        assert!(shade.reaches([1000.0, 770.0, 500.0, 100.0]));
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

    /// The shader's stack, term for term, over `s`'s uniforms: the first `s.live()` blocks, as
    /// `vs_ambient.vert`'s `scrim_alpha` loops over them.
    fn shader_alpha(s: &GroundShade, px: [f32; 2]) -> f32 {
        let u = s.uniforms();
        let term = |i: usize| {
            let (dx, dy) = (((px[0] - u[2 * i][0]).abs() - u[2 * i][2]).max(0.0), ((px[1] - u[2 * i][1]).abs() - u[2 * i][3]).max(0.0));
            let t = (((dx * dx + dy * dy).sqrt() - u[2 * i + 1][0]) * u[2 * i + 1][1]).clamp(0.0, 1.0);
            u[2 * i + 1][2] * (1.0 - t * t * (3.0 - 2.0 * t))
        };
        let n = s.live() as f32;
        let mut keep = 1.0f32;
        for i in 0..MAX_BLOCKS {
            if i as f32 >= n {
                break;
            }
            keep *= 1.0 - term(i);
        }
        1.0 - keep
    }

    /// **The count is the live blocks, as a prefix.** No shade and a shade scaled to nothing run none
    /// of the six; content-block shades run exactly theirs; and a list with unused blocks
    /// among the live ones comes out with the live ones first, in order (so `uniforms` packs them
    /// where the shader's loop reads), the unused ones after, all zero or not.
    #[test]
    fn the_shader_runs_exactly_the_live_blocks_which_are_a_prefix() {
        assert_eq!(GroundShade::NONE.live(), 0);
        assert_eq!(GroundShade::NONE.uniforms(), [[0.0; 4]; VEC4S], "and packs nothing");
        assert_eq!(sample().live(), 2);
        assert_eq!(sample().scaled(0.0).live(), 0, "a hero faded to nothing");
        assert_eq!(sample().scaled(0.5).live(), 2);
        assert_eq!(sample().placed(30.0, -40.0, 1.5).live(), 2);
        assert_eq!(GroundShade::from_blocks(&[card(); MAX_BLOCKS]).live(), MAX_BLOCKS);
        for s in shades() {
            assert_eq!(s.live(), s.blocks().count());
        }
        let (a, b) = (card(), ShadeBlock::new([900.0, 100.0, 300.0, 80.0], 20.0, 90.0, 0.3));
        let gap = ShadeBlock::new([10.0, 10.0, 10.0, 10.0], 2.0, 50.0, 0.0);
        let s = GroundShade::from_blocks(&[ShadeBlock::NONE, a, gap, b, ShadeBlock::NONE]);
        assert_eq!(s.live(), 2, "the unused blocks are not run");
        assert_eq!(s.blocks().copied().collect::<Vec<_>>(), [a, b], "…and the live ones keep their order");
        let u = s.uniforms();
        assert_eq!((u[0], u[1]), (a.frame, a.shape), "the first live block is packed first");
        assert_eq!((u[2], u[3]), (b.frame, b.shape));
        assert!(u[4..].iter().all(|v| v[2] == 0.0 || v == &gap.frame || v == &gap.shape), "nothing live follows");
        // a list of live blocks is packed exactly as it was given
        let all = [a, b, a];
        let u = GroundShade::from_blocks(&all).uniforms();
        for (i, blk) in all.iter().enumerate() {
            assert_eq!((u[2 * i], u[2 * i + 1]), (blk.frame, blk.shape));
        }
    }

    /// **Stopping at the live count is not a different field.** The shader's loop over the first
    /// `live()` blocks equals the closed form [`GroundShade::at`] (which runs all six) to the bit, at
    /// every sampled point, for every probe shade at full strength, faded, and with unused blocks mixed
    /// in: an unused block is a factor of exactly 1.0.
    #[test]
    fn stopping_at_the_live_count_is_the_closed_form_to_the_bit() {
        let mixed = GroundShade::from_blocks(&[ShadeBlock::NONE, card(), ShadeBlock::new([5.0, 5.0, 4.0, 4.0], 1.0, 9.0, 0.0), sample().blocks[0]]);
        let mut cases = shades();
        cases.push(mixed);
        cases.push(GroundShade::NONE);
        for (k, s) in cases.into_iter().enumerate() {
            for strength in [1.0f32, 0.37, 0.0] {
                let s = s.scaled(strength);
                for iy in 0..=27 {
                    for ix in 0..=48 {
                        let (x, y) = (-60.0 + ix as f32 * 41.3, -40.0 + iy as f32 * 43.7);
                        assert_eq!(shader_alpha(&s, [x, y]).to_bits(), s.at(x, y).to_bits(), "shade {k} x{strength} at ({x},{y})");
                    }
                }
            }
        }
    }

    /// The shades the support tests walk: a hero's two blocks, one rounded card, a square-cornered
    /// block with a short feather (the support's corners are tight there), a stadium, and two blocks
    /// far apart (the support is two islands, not their hull).
    fn shades() -> Vec<GroundShade> {
        vec![
            sample(),
            GroundShade::from_blocks(&[card()]),
            GroundShade::from_blocks(&[ShadeBlock::new([700.0, 400.0, 200.0, 100.0], 0.0, 30.0, 0.8)]),
            GroundShade::from_blocks(&[ShadeBlock::new([900.0, 500.0, 600.0, 40.0], 500.0, 140.0, 0.45)]),
            GroundShade::from_blocks(&[
                ShadeBlock::new([20.0, 20.0, 200.0, 80.0], 32.0, 140.0, 0.6),
                ShadeBlock::new([1500.0, 800.0, 300.0, 120.0], 32.0, 140.0, 0.45),
            ]),
        ]
    }

    /// The panel as 1x1, 4x4 (the scrim layer's cells), 8x8 and 16x16 grids, and 400 deterministic
    /// rects of every size and place, a few off the panel.
    fn probe_rects() -> Vec<[f32; 4]> {
        let mut rects = Vec::new();
        for n in [1usize, 4, 8, 16] {
            let (cw, ch) = (1920.0 / n as f32, 1080.0 / n as f32);
            for j in 0..n {
                for i in 0..n {
                    rects.push([i as f32 * cw, j as f32 * ch, cw, ch]);
                }
            }
        }
        let mut seed = 0x2545_f491_u32;
        let mut next = move |lo: f32, hi: f32| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            lo + (hi - lo) * (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        for _ in 0..400 {
            rects.push([next(-200.0, 2100.0), next(-200.0, 1200.0), next(1.0, 700.0), next(1.0, 400.0)]);
        }
        rects
    }

    /// **The support is exact on the side that matters: wherever `reaches` says no, the field is
    /// `0.0`, bit for bit, at every point of the rect** (a 21 x 21 grid of it, the edges and corners
    /// included), for the block and for the stack. And it is not vacuous: each shade has rects it
    /// reaches and rects it does not.
    #[test]
    fn a_rect_the_shade_does_not_reach_has_exactly_zero_field_everywhere() {
        let rects = probe_rects();
        for (k, s) in shades().into_iter().enumerate() {
            let (mut reached, mut skipped) = (0, 0);
            for &[x, y, w, h] in &rects {
                if s.reaches([x, y, w, h]) {
                    reached += 1;
                    continue;
                }
                skipped += 1;
                for iy in 0..=20 {
                    for ix in 0..=20 {
                        let (px, py) = (x + w * ix as f32 / 20.0, y + h * iy as f32 / 20.0);
                        assert_eq!(s.at(px, py), 0.0, "shade {k}: ({px},{py}) in the unreached rect {:?}", [x, y, w, h]);
                        for b in s.blocks() {
                            assert_eq!(b.at(px, py), 0.0, "shade {k}: a block at ({px},{py})");
                        }
                    }
                }
            }
            assert!(reached > 10 && skipped > 10, "shade {k}: {reached} reached, {skipped} skipped");
        }
    }

    /// **…and the other side is tight to a pixel or two of the feather**, so the skip is worth
    /// something: a rect whose nearest point is `0.5` px inside the feather is reached (and the field
    /// there is above 0), one `2` px past it is not, on each of the four sides.
    #[test]
    fn reaching_stops_within_two_pixels_of_the_feathers_end() {
        let b = card(); // (200, 300, 400, 100), feather 120: the rect spans x 200..600, y 300..400
        let (f, probe) = (120.0, 10.0);
        let at = |gap: f32| {
            [
                [600.0 + gap, 340.0, probe, probe],          // right
                [200.0 - gap - probe, 340.0, probe, probe],  // left
                [380.0, 400.0 + gap, probe, probe],          // below
                [380.0, 300.0 - gap - probe, probe, probe],  // above
            ]
        };
        for r in at(f - 0.5) {
            assert!(b.reaches(r), "{r:?}: half a pixel inside the feather");
            let nearest = [(r[0], r[1]), (r[0] + r[2], r[1]), (r[0], r[1] + r[3]), (r[0] + r[2], r[1] + r[3])];
            assert!(nearest.iter().any(|&(x, y)| b.at(x, y) > 0.0), "{r:?}: and the field is above 0 at its edge");
        }
        for r in at(f + 2.0) {
            assert!(!b.reaches(r), "{r:?}: two pixels past the feather");
        }
        // The rounded corner: a rect in the bounding box's square corner, (80,180), is past the arc
        // (centred (224,324), r 24): nearest corner (90,190) is 165 px from the rounded rect, not 120.
        let corner = [80.0, 180.0, 10.0, 10.0];
        assert!(!b.reaches(corner), "the box's corner is outside the arc's feather");
        assert!(b.reaches([150.0, 250.0, 10.0, 10.0]), "…and a rect nearer the arc (66 px from it) is reached");
    }

    /// No shade, a block of zero peak and a stack scaled to nothing reach nowhere; a rect containing
    /// a block's rect is reached; and moving the shade with `placed` moves its support with it.
    #[test]
    fn nothing_reaches_nowhere_and_the_support_moves_with_the_draw() {
        let whole = [0.0, 0.0, 1920.0, 1080.0];
        assert!(!GroundShade::NONE.reaches(whole));
        assert!(!ShadeBlock::NONE.reaches(whole));
        assert!(!sample().scaled(0.0).reaches(whole), "a hero faded to nothing");
        assert!(sample().reaches(whole));
        assert!(card().reaches([0.0, 0.0, 2000.0, 2000.0]) && card().reaches([300.0, 320.0, 1.0, 1.0]));
        let (ox, oy, k) = (30.0, -40.0, 1.5);
        let placed = sample().placed(ox, oy, k);
        for r in probe_rects() {
            let moved = [r[0] * k + ox, r[1] * k + oy, r[2] * k, r[3] * k];
            // away from the 1 px slack's edge the two agree: skip the rects that straddle it
            let (near, far) = (sample().reaches(r), placed.reaches(moved));
            if near != far {
                let grown = [r[0] - 2.0, r[1] - 2.0, r[2] + 4.0, r[3] + 4.0];
                let shrunk = [r[0] + 2.0, r[1] + 2.0, (r[2] - 4.0).max(0.0), (r[3] - 4.0).max(0.0)];
                assert!(!sample().reaches(shrunk) && sample().reaches(grown), "{r:?}: placed {far}, in place {near}");
            }
        }
    }

    /// **The rows left out of a cell's mesh were all zero**: for every probe rect, the rows outside
    /// [`GroundShade::reached_rows`] carry exactly 0 across their whole strip (the vertices on both of
    /// a row's lines are in it), the range's first and last rows are reached, and the range is empty
    /// exactly where the rect is not reached. The cuts are real: a shade that reaches a cell's middle
    /// leaves rows out.
    #[test]
    fn the_rows_left_out_of_a_mesh_are_exactly_zero() {
        const N: usize = super::super::FIELD_N;
        let mut trimmed = 0;
        for (k, s) in shades().into_iter().enumerate() {
            for rect in probe_rects() {
                let [x, y, w, h] = rect;
                let rows = s.reached_rows(rect, N);
                assert_eq!(rows.is_some(), s.reaches(rect), "shade {k}: {rect:?}");
                let Some(rows) = rows else { continue };
                assert!(rows.start < rows.end && rows.end <= N);
                let strip = |j: usize| [x, y + h * j as f32 / N as f32, w, h / N as f32];
                assert!(s.reaches(strip(rows.start)) && s.reaches(strip(rows.end - 1)), "shade {k}: {rect:?} {rows:?}");
                trimmed += (rows.len() < N) as usize;
                for j in (0..N).filter(|j| !rows.contains(j)) {
                    let [sx, sy, sw, sh] = strip(j);
                    for iy in 0..=4 {
                        for ix in 0..=20 {
                            let (px, py) = (sx + sw * ix as f32 / 20.0, sy + sh * iy as f32 / 4.0);
                            assert_eq!(s.at(px, py), 0.0, "shade {k}: ({px},{py}) in left-out row {j} of {rect:?} ({rows:?})");
                        }
                    }
                }
            }
        }
        assert!(trimmed > 20, "the premise: many cells are only partly reached ({trimmed})");
        let card_shade = GroundShade::from_blocks(&[card()]);
        // card: y 300..400 + feather 120 = 180..520: of a 270-tall cell at y 270, rows from 270 to 520
        assert_eq!(card_shade.reached_rows([0.0, 270.0, 1920.0, 270.0], N), Some(0..15), "…and a cell ends where the feather does");
        assert_eq!(GroundShade::NONE.reached_rows([0.0, 0.0, 1920.0, 1080.0], N), None);
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
