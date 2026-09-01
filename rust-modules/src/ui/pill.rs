//! **THE CAPSULE OUTLINE — three circular arcs per corner, blended.** The shape every control face
//! in this app used to draw as a stadium.
//!
//! A stadium is two semicircles bolted to two straight sides: its sides have zero curvature and its
//! ends maximum, and the switch happens in one pixel. That jump is what reads as the end bulging out
//! of the side. The capsule solved here has no straight side and no ellipse — every part is a
//! circular arc, and consecutive arcs are joined by a BLEND arc tangent to both, the way a fillet
//! joins two radii on a drawing. Per corner, from the middle outwards:
//!
//! * `big` — the arc carrying the top and bottom, its apex touching the box at the middle;
//! * `blend` — `f = √(R·r)`, the GEOMETRIC MEAN, so the curvature falls by the same factor twice. A
//!   blend only reads as a blend if no step dominates: at `f = 2r` the first step was ×54 and the
//!   second ×2, and the outline looked broken;
//! * `end` — the end circle, `r = PILL_END_R × height`.
//!
//! Two numbers are free ([`theme::PILL_END_R`], [`theme::PILL_BLEND_SLACK`]) and everything else is
//! solved; `tokens/shape.css` in the design project documents both and every consequence, and
//! `components/core/pillPath.js` is the same solve for the web. This module is the port, and it is
//! deliberately PURE — no painter, no GL — because the geometry is the half that can be graded on
//! the host.
//!
//! **What it is worth at our sizes, measured rather than assumed.** The leftover band
//! `B − r` is both how far the ends sit inside the box and how far the top and bottom bow, and at
//! `PILL_END_R` .492 on a 61px box it is **0.488px**. So on this app's 60px controls the outline
//! lands half a pixel off the stadium it replaces — which is exactly what the design says it should
//! ("reads as almost a stadium"), and which means the visible difference is the CORNER's blended
//! transition rather than a pillowed side. The dial between stadium and pillow is `PILL_END_R`, and
//! lowering it is what would make the bow legible from a couch.

use crate::ui::theme;

/// The solved outline, in the frame this app's SDF evaluates: **centred on the box**, `x` right,
/// `y` down, and folded into the TOP-RIGHT quadrant (`|x|`, `−|y|`) which the shape's two axes of
/// symmetry make sufficient. Every centre below is stated in that frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pill {
    /// `R` — the big arc, centred at `(0, big_cy)` **below** the box centre, apex on the top edge.
    pub big: f32,
    pub big_cy: f32,
    /// `f = √(R·r)` — the blend, centred at `blend_c`, internally tangent to both neighbours.
    pub blend: f32,
    pub blend_c: (f32, f32),
    /// `r` — the end circle, centred at `(end_cx, 0)`, apex on the right edge.
    pub end: f32,
    pub end_cx: f32,
}

/// The BOX height that renders a wanted **optical** height — the end circle's diameter.
///
/// The end circle is under half the box ([`theme::PILL_END_R`] < .5, forced by tangency), so a box
/// asked for `optical` tall would draw its ends `2 × PILL_END_R × optical` across and a capsule
/// would stop matching a disc of the same stated height at the ends. The box carries the deficit
/// instead: everything a caller lays out stays the size it always was, and only the middle of the
/// top and bottom edges bows out past the frame — by half the band, a quarter of a pixel here.
///
/// **Not rounded, where the design project's `pillBox` rounds.** That rounding is CSS's — it sets an
/// element's height in integer px — and taking it here would step the drawn plate by a whole pixel
/// somewhere in the middle of the focus pop, which scales the box continuously. Exact, the ends come
/// out at the optical height to the bit; rounded, they land 0.02px off it and jitter.
pub(crate) fn box_h(optical: f32) -> f32 {
    optical / (2.0 * theme::PILL_END_R)
}

/// Solve the outline for a `w × h` box. `None` is "this box cannot carry the shape" — a box that is
/// not wider than its ends, or one whose band has vanished — and the caller's answer to that is the
/// stadium it already drew ([`theme::RADIUS_PILL`] is the design's own fallback for the same case).
///
/// **Solved in `f64` and stored in `f32`, deliberately.** The big arc is enormous at this app's
/// sizes — 73 000px on a 260×61 control — and the bisection's own bracket runs to `r + 1e9`, where
/// a 24-bit mantissa cannot tell `hypot(100, 1e9)` from `1e9`: in `f32` the closure test comes back
/// negative at the top of the bracket and every capsule in the app silently falls back to a
/// stadium. Nothing about that failure is visible in a screenshot, which is exactly why it is
/// written here. The stored radii keep ~0.005px of absolute error at those magnitudes, which is
/// under a hundredth of the AA ramp they feed.
pub(crate) fn solve(w: f32, h: f32) -> Option<Pill> {
    if !w.is_finite() || !h.is_finite() {
        return None;
    }
    let (w, h) = (w as f64, h as f64);
    let r = theme::PILL_END_R as f64 * h;
    let (b, half) = (h * 0.5, w * 0.5);
    let band = b - r;
    if !(half > r) || band <= 0.0 {
        return None;
    }
    // The closure condition — the blend may eat `slack` of the band:
    //     (R − r) − hypot(half − r, R − B) = slack · band
    // `pillPath.js` BISECTS it, on the sound argument that the left side is monotone in R. It does
    // not have to be solved that way: squaring once cancels R² and leaves R linear, so
    //     R = [ (half − r)² + B² − (r + k)² ] / [ 2·(B − r − k) ],   k = slack · band
    // which agrees with that bisection to a part in 10⁶ (`it_reproduces_the_design_projects_own
    // _outline` grades this against numbers computed from the JS). Worth having as a formula rather
    // than as 120 iterations: this is solved per control per FRAME, the pop scales the box
    // continuously so no cache would hold, and the television is **armv7 soft-float** — every f64
    // operation here is a library call, and a bisection would put ~1200 of them in a frame that has
    // 16.7ms for everything.
    //
    // The denominator is what "at slack 1 the shape returns to a stadium" means arithmetically: k
    // walks up to the whole band, B − r − k goes to zero, and R runs away. Non-positive is that
    // case, and the caller draws the stadium it already had.
    let k = theme::PILL_BLEND_SLACK as f64 * band;
    let den = 2.0 * (b - r - k);
    if den <= 0.0 {
        return None;
    }
    let big = ((half - r).powi(2) + b * b - (r + k).powi(2)) / den;
    if !(big > r) || !big.is_finite() {
        return None;
    }
    let blend = (big * r).sqrt();
    // The blend's centre: internally tangent to both neighbours, so it sits at `R − f` from the big
    // arc's centre and `f − r` from the end's — one circle-circle intersection, and the branch taken
    // is the one inside this quadrant.
    let (cx, cy) = (0.0_f64, big - b); // big arc centre, centred frame
    let (ex, ey) = (half - r, 0.0_f64); // end circle centre
    let d = ((ex - cx).powi(2) + (ey - cy).powi(2)).sqrt();
    let (a, c) = (big - blend, blend - r);
    if a + c < d || (a - c).abs() > d || d <= 0.0 {
        return None;
    }
    let (ux, uy) = ((ex - cx) / d, (ey - cy) / d);
    let x = (d * d + a * a - c * c) / (2.0 * d);
    let yy = (a * a - x * x).max(0.0).sqrt();
    // `+ux*x − uy*yy` in the design's own orientation; here the quadrant wanted is the one with a
    // positive x and a negative y (up), which is the other sign of the perpendicular.
    let fx = cx + ux * x + uy * yy;
    let fy = cy + uy * x - ux * yy;
    Some(Pill {
        big: big as f32,
        big_cy: cy as f32,
        blend: blend as f32,
        blend_c: (fx as f32, fy as f32),
        end: r as f32,
        end_cx: ex as f32,
    })
}

impl Pill {
    /// The solved outline as `fs_src.frag` takes it — see [`crate::gfx::PillArgs`]. The trailing 1
    /// is the shader's switch, and it is part of the vector rather than a separate uniform so that
    /// "armed" and "the geometry" cannot be set apart from each other.
    pub(crate) fn args(&self) -> [f32; 8] {
        [
            self.big,
            self.blend,
            self.end,
            self.big_cy,
            self.end_cx,
            self.blend_c.0,
            self.blend_c.1,
            1.0,
        ]
    }

    /// The same outline `s` times the size. The shape is SIMILAR under a uniform scale — every
    /// radius and every centre is homogeneous of degree one in the box — so the focus pop scales the
    /// solved geometry instead of re-solving it, which is also what lets a caller solve once from
    /// the frame it laid out rather than once per frame from the popped one.
    pub(crate) fn scaled(self, s: f32) -> Self {
        Self {
            big: self.big * s,
            big_cy: self.big_cy * s,
            blend: self.blend * s,
            blend_c: (self.blend_c.0 * s, self.blend_c.1 * s),
            end: self.end * s,
            end_cx: self.end_cx * s,
        }
    }

    /// The signed distance from `p` (centred frame, `y` down) to the outline — negative inside. The
    /// CPU twin of `fs_src.frag`'s `sdPill`, and the reason both can be trusted: the shader is the
    /// only copy that draws, and this one is the only copy that can be asserted about.
    ///
    /// The point is folded into the top-right quadrant and the governing arc chosen by the
    /// TANGENCY DIRECTIONS — the outline is smooth, so at each hand-over the two arcs agree on both
    /// the distance and the normal, and there is no seam to place.
    pub(crate) fn sd(&self, p: (f32, f32)) -> f32 {
        let q = (p.0.abs(), -p.1.abs());
        let (c, rad) = self.arc(q);
        // f64 for the same reason `solve` uses it: this is `length(q − C) − R` with both terms at
        // 73 000 and an answer near zero. The SHADER does it in `highp` (fp32) and carries ~0.007px
        // of error there, a hundredth of its own AA ramp — but a host test asserting a gradient
        // over a 0.05px step cannot afford it.
        let (dx, dy) = (q.0 as f64 - c.0 as f64, q.1 as f64 - c.1 as f64);
        ((dx * dx + dy * dy).sqrt() - rad as f64) as f32
    }

    /// Which arc governs a point already folded into the top-right quadrant — `(centre, radius)`.
    fn arc(&self, q: (f32, f32)) -> ((f32, f32), f32) {
        let dir = |from: (f32, f32), to: (f32, f32)| {
            let (dx, dy) = (to.0 - from.0, to.1 - from.1);
            let l = (dx * dx + dy * dy).sqrt().max(1e-6);
            (dx / l, dy / l)
        };
        let big_c = (0.0, self.big_cy);
        let end_c = (self.end_cx, 0.0);
        // Seen from the big arc's centre, its own span runs from straight UP (the apex, on the top
        // edge) to the blend's tangency; the x of the normalized direction grows monotonically
        // across it, which is what makes one comparison enough.
        let t1 = dir(big_c, self.blend_c);
        let v1 = dir(big_c, q);
        if v1.0 <= t1.0 {
            return (big_c, self.big);
        }
        // …and seen from the end circle's centre, its span runs from the blend's tangency out to
        // straight RIGHT, so the same comparison runs the other way. The tangency direction points
        // AWAY from the blend's centre — the blend contains the end circle and touches it from the
        // inside, so the contact is on the far side of it — which is `C_end − C_blend`, and reading
        // it the other way round puts the hand-over a whole quadrant out.
        let t2 = dir(self.blend_c, end_c);
        let v2 = dir(end_c, q);
        if v2.0 >= t2.0 {
            return (end_c, self.end);
        }
        (self.blend_c, self.blend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The box a 60px control actually draws — [`box_h`]'s worked example, and the size every
    /// number below is taken at.
    const W: f32 = 260.0;
    const H: f32 = 61.0;

    /// **It is the design project's own solve, to the hundredth of a pixel.** `pillPath.js` is the
    /// other implementation of this geometry and the one the specimen cards render; a port that
    /// merely looks capsule-shaped is worth nothing, so the four solved numbers are pinned against
    /// values computed from that file's algorithm.
    #[test]
    fn it_reproduces_the_design_projects_own_outline() {
        let p = solve(W, H).expect("a 260x61 box carries the shape");
        assert!((p.end - 30.0120).abs() < 0.001, "end circle: {}", p.end);
        assert!((p.big - 73_197.914).abs() < 1.0, "big arc: {}", p.big);
        assert!((p.blend - 1482.1659).abs() < 0.05, "blend: {}", p.blend);
        assert!(
            (p.big_cy - 73_167.414).abs() < 1.0,
            "big centre: {}",
            p.big_cy
        );
        assert!(
            (p.end_cx - 99.9880).abs() < 0.001,
            "end centre: {}",
            p.end_cx
        );
        assert!(
            (p.blend_c.0 - 63.4423).abs() < 0.05,
            "blend centre x: {}",
            p.blend_c.0
        );
        assert!(
            (p.blend_c.1 - 1451.6940).abs() < 0.05,
            "blend centre y: {}",
            p.blend_c.1
        );
        // and the rule that fixes it: the blend is the GEOMETRIC MEAN of the two arcs it joins, so
        // the curvature falls by the same factor twice (×49.4 and ×49.4 here).
        assert!((p.blend * p.blend - p.big * p.end).abs() / (p.big * p.end) < 1e-4);
    }

    /// **`box_h` is what keeps a capsule and a disc the same object.** The end circle is under half
    /// the box, so a 60px optical control is drawn in a 61px box and its ends come out 60.02 across
    /// — the disc beside it on the HUD is 60. Getting this backwards is not a rounding error: it
    /// makes every capsule in the app half a pixel shorter at the ends than every circle.
    #[test]
    fn the_box_carries_the_deficit_so_the_ends_are_the_stated_height() {
        assert!((box_h(60.0) - 60.9756).abs() < 0.001, "{}", box_h(60.0));
        let p = solve(W, box_h(60.0)).unwrap();
        assert!(
            (p.end * 2.0 - 60.0).abs() < 0.01,
            "the drawn end IS the optical height: {}",
            p.end * 2.0
        );
    }

    /// **The pop scales the outline instead of re-solving it**, which is only sound because the
    /// shape is similar under a uniform scale. Graded as a field, not as six numbers: a scaled solve
    /// and a solve of the scaled box have to agree everywhere, or a capsule would change shape
    /// slightly as it grows and nothing would say so.
    #[test]
    fn scaling_the_solved_outline_is_solving_the_scaled_box() {
        let s = 1.07;
        let a = solve(W, H).unwrap().scaled(s);
        let b = solve(W * s, H * s).unwrap();
        for i in 0..200 {
            let ang = i as f32 / 200.0 * std::f32::consts::TAU;
            let q = (ang.cos() * W * 0.5 * s, ang.sin() * H * 0.5 * s);
            assert!(
                (a.sd(q) - b.sd(q)).abs() < 0.01,
                "at {q:?}: {} vs {}",
                a.sd(q),
                b.sd(q)
            );
        }
    }

    /// **A true distance field, across both hand-overs** — the property that catches a mis-chosen
    /// arc, which nothing else does. The outline is smooth, so |∇sd| is 1 everywhere; a sector test
    /// that hands over to the wrong circle leaves the field continuous in VALUE at some angles and
    /// still bends the gradient, and the shape then draws with a kink no screenshot names.
    #[test]
    fn the_field_is_a_true_distance_everywhere_around_the_corner() {
        let p = solve(W, H).unwrap();
        let e = 0.05;
        for i in 0..400 {
            // a ring of sample points around the whole outline, at a few offsets in and out
            let a = i as f32 / 400.0 * std::f32::consts::TAU;
            for k in [-3.0_f32, -1.0, 0.5, 2.0] {
                let (cx, cy) = (a.cos() * (W * 0.5 + k), a.sin() * (H * 0.5 + k));
                let gx = (p.sd((cx + e, cy)) - p.sd((cx - e, cy))) / (2.0 * e);
                let gy = (p.sd((cx, cy + e)) - p.sd((cx, cy - e))) / (2.0 * e);
                let g = (gx * gx + gy * gy).sqrt();
                assert!((g - 1.0).abs() < 0.02, "|grad| = {g} at ({cx}, {cy})");
            }
        }
    }

    /// **The four apexes touch the box and nothing leaves it.** The ends are on the left and right
    /// edges, the big arc's apex on the top and bottom, and every other boundary point is strictly
    /// inside — which is what makes the frame a caller lays out with also the frame that is drawn.
    #[test]
    fn the_outline_touches_the_box_at_four_points_and_leaves_it_nowhere() {
        let p = solve(W, H).unwrap();
        let (half, b) = (W * 0.5, H * 0.5);
        for apex in [(half, 0.0), (-half, 0.0), (0.0, -b), (0.0, b)] {
            assert!(
                p.sd(apex).abs() < 0.01,
                "apex {apex:?} is on the outline: {}",
                p.sd(apex)
            );
        }
        for i in 0..=200 {
            let x = -half + W * i as f32 / 200.0;
            // walk down from outside to find the boundary, then check it is inside the box
            let mut y = -b - 1.0;
            while p.sd((x, y)) > 0.0 && y < 0.0 {
                y += 0.01;
            }
            assert!(
                y >= -b - 0.02,
                "the outline never rises above the box: {y} at x={x}"
            );
        }
    }

    /// **The bow is the BAND, and at this height the band is half a pixel.** Stated as a test
    /// because it is the number that decides whether this shape is worth its shader: `PILL_END_R`
    /// .492 leaves `B − r` = 0.488px, so the outline is 99.2% of the stadium it replaces and what
    /// actually reads is the blended corner. Lowering `PILL_END_R` is the dial that makes the side
    /// visibly pillowed — this test is what will fail, loudly and with the new number, the day
    /// somebody turns it.
    #[test]
    fn the_side_bows_by_the_band_and_the_band_is_documented() {
        let p = solve(W, H).unwrap();
        let band = H * 0.5 - p.end;
        assert!((band - 0.488).abs() < 0.01, "band = {band}");
        // the ends sit exactly that far inside the box's own corner box, top and bottom
        assert!((p.sd((p.end_cx, -H * 0.5 + band)).abs()) < 0.02);
    }

    /// A box that cannot carry the shape says so, and the caller draws the stadium it already had.
    /// Both refusals are real: a control narrower than its own ends (a disc asked for a capsule),
    /// and a degenerate box with no band to blend in.
    #[test]
    fn a_box_that_cannot_carry_the_shape_refuses_rather_than_approximating_one() {
        assert!(solve(40.0, 61.0).is_none(), "narrower than its own ends");
        assert!(solve(260.0, 0.0).is_none());
        assert!(solve(f32::NAN, 61.0).is_none());
    }
}
