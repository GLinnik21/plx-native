//! **The ground scrim: three piecewise-linear alpha terms of ONE ink, evaluated per vertex.**
//!
//! A hero's scrim is a darkening of the photograph under its copy, and Home's is two-dimensional
//! (a top band, an ELLIPTICAL radial behind the text, a bottom band: owner's CW-13 v2 board). The
//! ground's one pass ([`super::draw_ambient_inked`], [`super::draw_art_wash`]) already carries a
//! vertical ink ramp per VERTEX so its fragment does no colour arithmetic (the Mali-T820 is
//! arithmetic-bound; `vs_ambient.vert` records the measurements). This is the same move for a
//! field no straight ramp can describe: the vertex shader evaluates the three terms at each vertex
//! of the field mesh and stacks them, `1 - (1-top)(1-radial)(1-bottom)`, into the alpha the
//! fragment already reads. No new pass, no new fragment arithmetic.
//!
//! **A curve is `a0 + sum_i da_i * clamp((x - x_i) * inv_i, 0, 1)`** — the sum of clamped ramps,
//! which is exactly a piecewise-linear table (flat before its first stop and after its last) and
//! needs no search in a shader: [`SEGS`] unrolled terms. A segment is the four floats the shader
//! reads (`x_i`, `inv_i = 1 / (x_{i+1} - x_i)`, `da_i`, and `a0` in segment 0's `w`); an unused
//! segment is all zeros and adds nothing. [`Curve::at`] and [`GroundScrim::at`] are the SAME
//! arithmetic on the CPU: the legibility contract is graded on them, and the tests walk the real
//! field mesh against them (`the_mesh_carries_the_scrim_...`), which is what bounds what the
//! vertex-evaluated field can differ from the closed form by.
//!
//! **Zero is exact.** A [`GroundScrim::NONE`] is all zeros and the shader's stack is then exactly
//! `0.0` (`1 - 1*1*1`), and it is folded into the ink with `mix(inka, 1.0, s)`, which for `s == 0`
//! is `inka * 1.0 + 0.0` — the ramp's own alpha to the bit. So a screen with no scrim (Detail) is
//! drawn by the very same programs as before and gets the very same pixels.

/// Segments a curve carries (a table of `SEGS + 1` stops at most).
pub const SEGS: usize = 7;
/// `vec4`s the three curves and the radial's frame take in `u_sc`.
pub const VEC4S: usize = 3 * SEGS + 1;

/// One piecewise-linear curve of alpha over a scalar `x` (pixels for the two bands, a normalised
/// elliptical distance for the radial). See the module doc for the encoding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve {
    seg: [[f32; 4]; SEGS],
}

impl Curve {
    /// No alpha anywhere.
    pub const NONE: Curve = Curve { seg: [[0.0; 4]; SEGS] };

    /// The curve through `stops` (`(t, alpha)`, `t` ascending), with `t` placed at
    /// `x = origin + t * span` and every alpha times `k`. Flat before the first stop and after the
    /// last. At most `SEGS + 1` stops (a longer table is a bug in the table, not a case to clip).
    pub fn from_stops(stops: &[(f32, f32)], origin: f32, span: f32, k: f32) -> Curve {
        assert!(stops.len() <= SEGS + 1, "a scrim curve carries at most {} stops", SEGS + 1);
        let mut seg = [[0.0f32; 4]; SEGS];
        if let Some(&(_, a0)) = stops.first() {
            seg[0][3] = a0 * k;
        }
        for (i, pair) in stops.windows(2).enumerate() {
            let ((t0, a0), (t1, a1)) = (pair[0], pair[1]);
            let (x0, x1) = (origin + t0 * span, origin + t1 * span);
            assert!(x1 > x0, "scrim stops must ascend");
            seg[i][0] = x0;
            seg[i][1] = 1.0 / (x1 - x0);
            seg[i][2] = (a1 - a0) * k;
        }
        Curve { seg }
    }

    /// The alpha at `x`: the shader's expression, term for term.
    pub fn at(&self, x: f32) -> f32 {
        let mut a = self.seg[0][3];
        for s in &self.seg {
            a += s[2] * ((x - s[0]) * s[1]).clamp(0.0, 1.0);
        }
        a
    }

    /// Every alpha times `k`.
    fn scaled(mut self, k: f32) -> Curve {
        for s in &mut self.seg {
            s[2] *= k;
            s[3] *= k;
        }
        self
    }

    /// The curve moved to a device frame where `x_dev = x * s + o`.
    fn placed(mut self, o: f32, s: f32) -> Curve {
        for g in &mut self.seg {
            g[0] = g[0] * s + o;
            g[1] /= s;
        }
        self
    }

    /// The `x` of every stop that is a knee of the curve (where its slope changes), for a caller
    /// that wants to cut a rect there.
    pub fn knees(&self) -> impl Iterator<Item = f32> + '_ {
        self.seg.iter().filter(|g| g[1] > 0.0).map(|g| g[0])
    }
}

/// The three terms of a hero scrim, in the frame a draw is made in: `top` and `bottom` are alphas
/// over y, `radial` over the normalised elliptical distance from `centre` (radii `1 / inv_radii`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundScrim {
    pub top: Curve,
    pub radial: Curve,
    pub bottom: Curve,
    pub centre: [f32; 2],
    pub inv_radii: [f32; 2],
}

impl GroundScrim {
    /// No scrim: all zeros, which the shader evaluates to exactly `0.0` (module doc).
    pub const NONE: GroundScrim = GroundScrim {
        top: Curve::NONE,
        radial: Curve::NONE,
        bottom: Curve::NONE,
        centre: [0.0; 2],
        inv_radii: [0.0; 2],
    };

    /// The normalised elliptical distance of `(x, y)` from the radial's centre.
    fn distance(&self, x: f32, y: f32) -> f32 {
        let (qx, qy) = ((x - self.centre[0]) * self.inv_radii[0], (y - self.centre[1]) * self.inv_radii[1]);
        (qx * qx + qy * qy).sqrt()
    }

    /// The stacked alpha at `(x, y)`: `1 - (1-top)(1-radial)(1-bottom)`, as the vertex shader
    /// computes it.
    pub fn at(&self, x: f32, y: f32) -> f32 {
        1.0 - (1.0 - self.top.at(y)) * (1.0 - self.radial.at(self.distance(x, y))) * (1.0 - self.bottom.at(y))
    }

    /// The alpha of the two bands alone at `y`: what a row of the field carries whatever its x
    /// (the glass-over-ground delta takes the ground's ink as a ramp over y).
    pub fn bands_at(&self, y: f32) -> f32 {
        1.0 - (1.0 - self.top.at(y)) * (1.0 - self.bottom.at(y))
    }

    /// Every term's alpha times `k` (a hero fade, a painter's alpha): each term, then stacked —
    /// not the stack times `k`.
    pub fn scaled(self, k: f32) -> GroundScrim {
        GroundScrim { top: self.top.scaled(k), radial: self.radial.scaled(k), bottom: self.bottom.scaled(k), ..self }
    }

    /// The scrim moved to the device frame a draw lands in, where an authored point `p` is at
    /// `p * s + (ox, oy)` (a painter's translate, and its zoom's scale).
    pub fn placed(self, ox: f32, oy: f32, s: f32) -> GroundScrim {
        // No scrim stays NONE exactly (not "zeros at an offset"): the draw's upload cache and the
        // glass's source identity both compare the whole value.
        if self == GroundScrim::NONE {
            return self;
        }
        GroundScrim {
            top: self.top.placed(oy, s),
            radial: self.radial,
            bottom: self.bottom.placed(oy, s),
            centre: [self.centre[0] * s + ox, self.centre[1] * s + oy],
            inv_radii: [self.inv_radii[0] / s, self.inv_radii[1] / s],
        }
    }

    /// The `u_sc` upload: the three curves, then `(cx, cy, 1/rx, 1/ry)`.
    pub fn uniforms(&self) -> [[f32; 4]; VEC4S] {
        let mut u = [[0.0; 4]; VEC4S];
        u[..SEGS].copy_from_slice(&self.top.seg);
        u[SEGS..2 * SEGS].copy_from_slice(&self.radial.seg);
        u[2 * SEGS..3 * SEGS].copy_from_slice(&self.bottom.seg);
        u[3 * SEGS] = [self.centre[0], self.centre[1], self.inv_radii[0], self.inv_radii[1]];
        u
    }

    /// The y of every knee of the two bands, ascending.
    pub fn y_knees(&self) -> Vec<f32> {
        let mut v: Vec<f32> = self.top.knees().chain(self.bottom.knees()).collect();
        v.sort_by(f32::total_cmp);
        v.dedup();
        v
    }
}

#[cfg(any(test, feature = "test-support"))]
impl GroundScrim {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    const STOPS: [(f32, f32); 4] = [(0.0, 0.5), (0.25, 0.2), (0.6, 0.2), (1.0, 0.0)];

    #[test]
    fn a_curve_is_its_table_flat_outside_it() {
        let c = Curve::from_stops(&STOPS, 100.0, 400.0, 1.0);
        for (t, a) in STOPS {
            assert!((c.at(100.0 + t * 400.0) - a).abs() < 1e-6, "t={t}");
        }
        assert_eq!(c.at(-50.0), 0.5, "flat before the first stop");
        // the sum of ramps lands on the last stop to float rounding, not bit for bit
        assert!(c.at(9_999.0).abs() < 1e-6, "flat after the last: {}", c.at(9_999.0));
        assert!((c.at(100.0 + 0.125 * 400.0) - 0.35).abs() < 1e-6, "linear between stops");
    }

    #[test]
    fn none_is_exactly_zero_everywhere() {
        for (x, y) in [(0.0, 0.0), (560.0, 590.0), (1919.0, 1079.0), (-30.0, 5000.0)] {
            assert_eq!(GroundScrim::NONE.at(x, y), 0.0);
        }
        assert_eq!(GroundScrim::NONE.uniforms(), [[0.0; 4]; VEC4S]);
    }

    /// **A screen with no scrim is drawn to the bit as it always was.** The vertex shader folds the
    /// stack into the ramp's alpha as `mix(inka, 1.0, s)` = `inka * (1.0 - s) + 1.0 * s`; with
    /// every uniform zero `s` is exactly `0.0`, and that expression is `inka` unchanged for every
    /// float (Detail's ramp rides this program with no scrim). Both halves are held here: the
    /// stack of nothing is `0.0`, and `x * 1.0 + 1.0 * 0.0` is `x`.
    #[test]
    fn no_scrim_leaves_the_ramps_alpha_to_the_bit() {
        let u = GroundScrim::NONE.uniforms();
        let s = {
            // the shader's own arithmetic over the all-zero uniforms, term for term
            let curve = |base: usize, x: f32| {
                let mut a = u[base][3];
                for i in 0..SEGS {
                    a += u[base + i][2] * ((x - u[base + i][0]) * u[base + i][1]).clamp(0.0, 1.0);
                }
                a
            };
            1.0 - (1.0 - curve(0, 540.0)) * (1.0 - curve(SEGS, 0.5)) * (1.0 - curve(2 * SEGS, 540.0))
        };
        assert_eq!(s.to_bits(), 0.0f32.to_bits());
        for i in 0..=1000 {
            let inka = i as f32 / 1000.0;
            assert_eq!((inka * (1.0 - s) + 1.0 * s).to_bits(), inka.to_bits(), "inka={inka}");
        }
    }

    fn sample() -> GroundScrim {
        GroundScrim {
            top: Curve::from_stops(&STOPS, 0.0, 240.0, 1.0),
            radial: Curve::from_stops(&[(0.0, 0.8), (0.5, 0.4), (1.0, 0.0)], 0.0, 1.0, 1.0),
            bottom: Curve::from_stops(&[(0.0, 0.0), (1.0, 0.9)], 640.0, 440.0, 1.0),
            centre: [560.0, 590.0],
            inv_radii: [1.0 / 1020.0, 1.0 / 460.0],
        }
    }

    #[test]
    fn the_terms_stack_as_independent_layers_of_one_ink() {
        let s = sample();
        let (x, y) = (700.0, 700.0);
        let (t, r, b) = (s.top.at(y), s.radial.at(s.distance(x, y)), s.bottom.at(y));
        let layered = 1.0 - (1.0 - t) * (1.0 - r) * (1.0 - b);
        assert!((s.at(x, y) - layered).abs() < 1e-7);
        assert!(r > 0.0 && b > 0.0, "a point where two terms overlap");
    }

    #[test]
    fn scaling_scales_each_term_then_stacks() {
        let s = sample();
        for k in [0.0, 0.3, 1.0] {
            let ks = s.scaled(k);
            for (x, y) in [(10.0, 10.0), (560.0, 590.0), (900.0, 800.0)] {
                let (t, r, b) = (k * s.top.at(y), k * s.radial.at(s.distance(x, y)), k * s.bottom.at(y));
                let want = 1.0 - (1.0 - t) * (1.0 - r) * (1.0 - b);
                assert!((ks.at(x, y) - want).abs() < 1e-6, "k={k} ({x},{y})");
            }
        }
        assert_eq!(s.scaled(0.0).at(560.0, 590.0), 0.0);
    }

    #[test]
    fn placing_moves_the_field_with_the_draw() {
        let s = sample();
        let (ox, oy, k) = (30.0, -40.0, 1.5);
        let p = s.placed(ox, oy, k);
        for (x, y) in [(0.0, 0.0), (560.0, 590.0), (900.0, 800.0), (1500.0, 200.0)] {
            assert!((p.at(x * k + ox, y * k + oy) - s.at(x, y)).abs() < 1e-5, "({x},{y})");
        }
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
