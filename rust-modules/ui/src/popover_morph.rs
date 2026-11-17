//! The **morph** open style: a glass opener (a pill) grows into its panel instead of fading and
//! sliding in on [`crate::popover::K_APPEAR`]. Selected per popover (see
//! [`crate::popover::Popover::morphing`], the ONE selector; open it with
//! [`crate::popover::Popover::open_from`] and the opener's rect, and `Popover::panel` lays the glass
//! at the morphing rect and radius on the one fill ramp below). The item hold menu is not a morph: it keeps the opener lift and the shared appear.
//!
//! **Every header menu (the Library's, the Watchlist's Sort & Filter, the profile menu) morphs the
//! GLASS, and the opener is CONSUMED by it** (owner decision, 2026-11-17, which REVERSES design
//! review S11/S12's "the opener is never part of the morph"). The host's opener lift still paints
//! the opener above the scrim and the panel, but at [`Frame::opener_alpha`]: it fades out as the
//! glass detaches from it on open and fades back in as the droplet merges into it on close. The
//! morph draws no opener face of its own; it hands the glass a MERGE shape ([`Frame::merge`], the
//! opener's own rect shrinking to nothing) that the glass shader joins to the panel with a smooth
//! minimum, so the panel looks pinched off the button and drained back into it.
//!
//! Source: design board `Motion`, card "Sort & Filter" (end state `WatchlistOptions`): the label
//! pill becomes a panel that fits its rows while the label dissolves into them. The board states
//! the springs this module names; where it disagrees with the brief the brief wins (recorded on
//! the constants below).
//!
//! **Five springs, one clock.** The panel rect's width and height are separate critically damped
//! springs (width the slower, so the pill widens after it has begun to open); the corner radius is
//! derived from the height spring with a bulge; the rows ride an UNDER-damped scale spring
//! (`0.82 -> 1`, ζ 0.6, so they settle with a 1.7% overshoot rather than ringing); and a fifth,
//! stiff one ([`K_OUT`]) carries the CLOSE's first phase, the content and the frost leaving.
//! Durations are not stated, they fall out of the stiffnesses: the height spring (the one the eye
//! follows) is within 6% of rest at 150 ms and the width spring within 6% at 190 ms.
//!
//! **One surface, one clock.** The pill and the panel are ONE surface whose rect, radius and fill
//! animate. Everything but the rows' scale and the close's `out` is a function of
//! [`Frame::progress`], the slower of the two rect springs (0 = the opener's rect, 1 = the panel):
//!
//! - the glass and its SHADOW cross in together over the first [`FILL_AT`] of the progress, a
//!   smoothstep ([`Frame::ground_alpha`]): the ONE ramp, so a rect that has left the opener is
//!   never opaque (an opaque first frame hung a slab ~25 px below the pill, N-b);
//! - the OPENER fades out over the first [`OPENER_OUT_AT`] ([`Frame::opener_alpha`]) and the glass
//!   is JOINED to a shrinking copy of the opener's rect ([`Frame::merge`]) until [`MERGE_END_AT`],
//!   so the glass reads as detaching from the button rather than appearing over it;
//! - the glass is CLEAR (the sharp page refracted through it) while it is small and FROSTED once it
//!   is big ([`Frame::clear`], from [`FROST_FROM`] to [`FROST_TO`]);
//! - the ROWS ([`Frame::content_alpha`]) start at [`ROWS_FROM_AT`], once the glass has taken over,
//!   and are opaque by [`ROWS_OPAQUE_AT`]. They are clipped to the current rect
//!   (`Popover::morph_rows`), so no row ever draws outside the surface.
//!
//! **A close is NOT the open reversed**; it is three phases (iOS 26's Photos filter menu):
//! 1. the rows and the frost vanish ([`Frame::content_alpha`] and [`Frame::clear`] ride the `out`
//!    spring, [`K_OUT`], about 33 ms), leaving clear glass in the panel's rect; the rect HOLDS
//!    until `out` reaches [`OUT_GATE`];
//! 2. the clear slab shrinks and slides back toward the opener (the rect springs, [`K_CLOSE`]);
//! 3. as `progress` falls under [`MERGE_END_AT`] the opener re-forms as a blob at its own spot and
//!    the slab drains into it through a neck (the same [`Frame::merge`] curve, now growing), and
//!    the opener's own face fades back in over the last [`OPENER_OUT_AT`].
//!
//! **No measured opener, no morph.** [`Frame`] with `from == None` is the fade+scale FALLBACK:
//! the panel scales about its centre from [`FALLBACK_SCALE_FROM`] to 1 and its glass fades in on
//! the same fill curve.

use crate::{Rect, Spring, Zoom};

/// The board's panel corner radius, `border-radius: 22px` — what the design board's morph ends on.
/// The shipped morph does NOT end here: shipped panels state their own radius (20/24/26) and the
/// morph's end radius IS the caller's ([`Morph::frame`]'s `panel_radius`), so the handover to the
/// settled panel and the hand-back at the start of a close do not snap. This is the stand-in for
/// a caller with no panel of its own (the rows' painter, which reads no radius).
pub const PANEL_RADIUS: f32 = 22.0;
/// How far past the straight pill-to-panel radius ramp the corner swells mid-open, in px. The
/// board's radius runs 19 -> 34 -> 22 for a 38 px pill (a swell of about 13 over the ramp's 20).
pub const RADIUS_BULGE: f32 = 13.0;
/// Width spring stiffness while opening (brief: k ~ 620; ≈190 ms to within 6% of rest).
pub const K_WIDTH: f32 = 620.0;
/// Height spring stiffness while opening (brief: k ~ 950; ≈150 ms to within 6% of rest).
pub const K_HEIGHT: f32 = 950.0;
/// Both rect springs while CLOSING: the height stiffness, so the slab's travel back to the opener
/// takes ≈150 ms (the open's slower width spring would make it ≈190). They only start once the
/// content has left ([`OUT_GATE`]).
pub const K_CLOSE: f32 = K_HEIGHT;
/// The stiffness of the `out` spring that carries a close's first phase, the rows and the frost
/// leaving (0 = content in, 1 = content out). Critically damped, so ≈48 ms to within 6% of rest and
/// ≈33 ms to within 18%: the content is gone in about two frames, leaving clear glass.
pub const K_OUT: f32 = 9000.0;
/// The `out` value at which a close's rect springs are released: until then the panel holds its
/// rect while its rows and frost leave, so the rect starts shrinking when the content is almost
/// gone (two frames at 60 fps).
pub const OUT_GATE: f32 = 0.8;
/// The rows' scale spring stiffness (brief: k ~ 720).
pub const K_ROWS: f32 = 720.0;
/// The rows' damping ratio (brief and board: 0.6 — underdamped, one small overshoot).
pub const ZETA_ROWS: f32 = 0.6;
/// The rows' scale when the morph starts (the board's `scale(.82)`).
pub const ROWS_FROM: f32 = 0.82;
/// Progress at which the glass is fully in: the growing shape is all glass before it gets big.
pub const FILL_AT: f32 = 0.18;
/// Progress at which the rows begin to appear: not before the glass has taken over.
pub const ROWS_FROM_AT: f32 = 0.3;
/// Progress at which the rows are fully opaque.
pub const ROWS_OPAQUE_AT: f32 = 0.6;
/// Progress at which the opener's own face is fully gone on an open (and starts to return on a
/// close): it fades out over `[0, OPENER_OUT_AT]` as the glass detaches from it
/// ([`Frame::opener_alpha`]; the host's opener lift paints the opener at that alpha).
pub const OPENER_OUT_AT: f32 = 0.15;
/// Progress at which the merge shape has shrunk to nothing on an open (and begins to re-form on a
/// close): the glass is joined to a copy of the opener's rect that scales to 0 over
/// `[0, MERGE_END_AT]` ([`Frame::merge`]).
pub const MERGE_END_AT: f32 = 0.35;
/// The smooth-minimum reach of the merge, in px: shapes closer than this are joined by a neck.
/// The glass is also inflated by up to a quarter of it where the two shapes coincide.
pub const MERGE_K: f32 = 28.0;
/// Progress at which the glass starts to frost: below it the glass is fully CLEAR (the sharp page
/// refracted through it, [`Frame::clear`] = 1).
pub const FROST_FROM: f32 = 0.2;
/// Progress at which the glass is fully frosted: the material the settled panel wears
/// ([`Frame::clear`] = 0).
pub const FROST_TO: f32 = 0.7;
/// Fallback when the opener has no measured rect to morph from: a fade+scale of the panel about
/// its centre, starting at this scale. The board's `menuout` keyframes leave a menu at 0.92, so an
/// entrance from the same scale is the matching motion.
pub const FALLBACK_SCALE_FROM: f32 = 0.92;

/// Everything a draw needs from the morph at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The surface's current rect.
    pub rect: Rect,
    /// Its current corner radius.
    pub radius: f32,
    /// The morph's progress, 0 = the opener's rect, 1 = the panel: the slower rect spring.
    pub progress: f32,
    /// The glass's share of the fill (0 = none, 1 = all glass): the alpha of the glass, with an
    /// opener also its shadow's (the one ramp); for the fallback the glass's alpha alone.
    pub ground_alpha: f32,
    /// Alpha of the panel's rows: 0 until [`ROWS_FROM_AT`], so a row is only ever drawn on a
    /// surface whose fill is all glass.
    pub content_alpha: f32,
    /// Scale of the rows about their own centres (0.82 -> 1, with a small overshoot).
    pub rows_scale: f32,
    /// How much of the OPENER's own face is left (1 = whole, 0 = consumed): fades out over the
    /// first [`OPENER_OUT_AT`] of the progress on an open and back in on a close. 1 for the
    /// fallback, which has no opener to consume. The host's opener lift paints at this alpha
    /// (`Screen::opener_alpha`).
    pub opener_alpha: f32,
    /// The shape the glass is JOINED to, or `None` once there is nothing to join (the settled
    /// panel, the fallback): the opener's rect scaled about its own centre by
    /// `1 - smoothstep(0, MERGE_END_AT, progress)`.
    pub merge: Option<Merge>,
    /// How CLEAR the glass is, 0 = the frosted material the settled panel wears, 1 = clear glass
    /// (the sharp page refracted through the whole face). A close is clear from its first frames
    /// (the `out` spring), an open frosts from [`FROST_FROM`] to [`FROST_TO`]. 0 for the fallback.
    pub clear: f32,
}

/// The second shape of a merging glass: where it is, how round, and how far its neck reaches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Merge {
    /// The shape's rect (the opener's, scaled about its own centre).
    pub rect: Rect,
    /// Its corner radius (the opener's pill radius, scaled with it).
    pub radius: f32,
    /// The smooth-minimum reach in px ([`MERGE_K`]).
    pub k: f32,
}

/// `x` mapped from `[a, b]` to `[0, 1]`, clamped.
fn ramp(x: f32, a: f32, b: f32) -> f32 {
    ((x - a) / (b - a)).clamp(0.0, 1.0)
}

/// The smoothstep of `x` over `[a, b]`.
fn smooth(x: f32, a: f32, b: f32) -> f32 {
    let u = ramp(x, a, b);
    u * u * (3.0 - 2.0 * u)
}

/// The fill crossover at progress `t`: a smoothstep over `[0, FILL_AT]`.
pub fn fill_at(t: f32) -> f32 {
    smooth(t, 0.0, FILL_AT)
}

/// The opener's own alpha at progress `t`: `1 - smoothstep(0, OPENER_OUT_AT, t)`.
pub fn opener_alpha_at(t: f32) -> f32 {
    1.0 - smooth(t, 0.0, OPENER_OUT_AT)
}

/// The merge shape's scale at progress `t`: `1 - smoothstep(0, MERGE_END_AT, t)`.
pub fn merge_scale_at(t: f32) -> f32 {
    1.0 - smooth(t, 0.0, MERGE_END_AT)
}

/// The glass's clarity at progress `t` before a close's `out`: `1 - smoothstep(FROST_FROM,
/// FROST_TO, t)`.
pub fn clear_at(t: f32) -> f32 {
    1.0 - smooth(t, FROST_FROM, FROST_TO)
}

/// The panel's rect when the width spring is at `pw` and the height spring at `ph` (0 = the
/// opener's pill rect, 1 = the final panel). `from` is `None` when the opener has no rect: the
/// panel scales about its centre ([`FALLBACK_SCALE_FROM`] -> 1, driven by `pw`).
pub fn rect_at_xy(from: Option<Rect>, panel: Rect, pw: f32, ph: f32) -> Rect {
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    match from {
        Some(f) => Rect::new(
            lerp(f.x, panel.x, pw),
            lerp(f.y, panel.y, ph),
            lerp(f.w, panel.w, pw),
            lerp(f.h, panel.h, ph),
        ),
        None => {
            let s = lerp(FALLBACK_SCALE_FROM, 1.0, pw);
            Rect::new(
                panel.cx() - panel.w * s * 0.5,
                panel.cy() - panel.h * s * 0.5,
                panel.w * s,
                panel.h * s,
            )
        }
    }
}

/// The panel's rect at one morph progress `t` for both axes; see [`rect_at_xy`].
pub fn rect_at(from: Option<Rect>, panel: Rect, t: f32) -> Rect {
    rect_at_xy(from, panel, t, t)
}

/// The corner radius at height progress `ph`: the straight ramp from the pill's own radius to the
/// panel's (`end_radius`, the CALLER's), plus a [`RADIUS_BULGE`] swell that is zero at both ends.
pub fn radius_at(pill_radius: f32, end_radius: f32, ph: f32) -> f32 {
    let ph = ph.clamp(0.0, 1.0);
    pill_radius + (end_radius - pill_radius) * ph + RADIUS_BULGE * 4.0 * ph * (1.0 - ph)
}

/// The point the rows scale about while the morph runs: the point of the final `panel` nearest
/// the opener's centre (the edge of the panel the pill sits on), or the panel's centre when the
/// opener has no rect. Rows grow out of the pill instead of out of the middle of the glass.
pub fn rows_anchor(from: Option<Rect>, panel: Rect) -> (f32, f32) {
    match from {
        Some(f) => (f.cx().clamp(panel.x, panel.x + panel.w), f.cy().clamp(panel.y, panel.y + panel.h)),
        None => (panel.cx(), panel.cy()),
    }
}

/// The scale the rows are drawn under at one instant: `scale` about [`rows_anchor`].
pub fn rows_zoom(from: Option<Rect>, panel: Rect, scale: f32) -> Zoom {
    let (ox, oy) = rows_anchor(from, panel);
    Zoom { s: scale, ox, oy }
}

/// The morph's motion state. `const`-constructible so a `static Popover` can hold one.
#[derive(Clone, Copy)]
pub struct Morph {
    w: Spring,
    h: Spring,
    rows: Spring,
    /// A close's content-and-frost phase: 0 = in, 1 = out ([`K_OUT`]).
    out: Spring,
}

impl Morph {
    pub const fn new() -> Self {
        Morph { w: Spring::at(0.0), h: Spring::at(0.0), rows: Spring::at(ROWS_FROM), out: Spring::at(0.0) }
    }

    /// Back to the pill, with no motion in between.
    pub fn reset(&mut self) {
        *self = Morph::new();
    }

    /// Advance one frame toward the open (`true`) or closed (`false`) end.
    ///
    /// An open is the rect springs alone (`out` returns to 0 on [`K_OUT`] if a close had raised
    /// it). A close is NOT that reversed: `out` rises first, and the rect springs hold where they
    /// are (their velocity dropped, so they do not coast) until it passes [`OUT_GATE`].
    pub fn step(&mut self, open: bool, dt: f32) {
        if open {
            self.out.step(0.0, K_OUT, dt);
            self.w.step(1.0, K_WIDTH, dt);
            self.h.step(1.0, K_HEIGHT, dt);
            self.rows.step_zeta(1.0, K_ROWS, ZETA_ROWS, dt);
            return;
        }
        self.out.step(1.0, K_OUT, dt);
        if self.out.pos >= OUT_GATE {
            self.w.step(0.0, K_CLOSE, dt);
            self.h.step(0.0, K_CLOSE, dt);
        } else {
            self.w.vel = 0.0;
            self.h.vel = 0.0;
        }
        self.rows.step_zeta(ROWS_FROM, K_ROWS, ZETA_ROWS, dt);
    }

    /// Has the close finished — the rect back on the pill and the rows back at their start? (`out`
    /// is not asked: a rect that never left the pill has nothing to wait for, and `Popover::update`
    /// resets the morph, `out` included, when it lands.)
    pub fn closed(&self) -> bool {
        self.w.pos <= 0.001 && self.h.pos <= 0.001 && self.rows.pos <= ROWS_FROM + 0.001
    }

    /// Has the open visibly arrived? A close that has begun ([`step`](Self::step) with `false`)
    /// raises `out` on its first frame, so the settled panel stops being settled with it.
    pub fn settled(&self) -> bool {
        self.w.pos >= 0.999
            && self.h.pos >= 0.999
            && (self.rows.pos - 1.0).abs() <= 0.001
            && self.out.pos <= 0.001
    }

    /// The draw state now, for the opener rect `from` (`None` = the fade+scale fallback) and the
    /// final `panel` rect.
    ///
    /// `panel_radius` is the caller's own panel corner radius: the morph ENDS on it (and a close
    /// starts from it), so the settled ground and the morphing one are the same shape.
    pub fn frame(&self, from: Option<Rect>, panel: Rect, panel_radius: f32) -> Frame {
        let (pw, ph) = (self.w.pos.clamp(0.0, 1.0), self.h.pos.clamp(0.0, 1.0));
        let out = self.out.pos.clamp(0.0, 1.0);
        let progress = pw.min(ph);
        let ground_alpha = fill_at(progress);
        let pill_radius = from.map_or(panel_radius, |f| f.w.min(f.h) * 0.5);
        // The merge shape: the opener's rect shrinking about its own centre, gone at `s == 0`.
        let merge = from.and_then(|f| {
            let s = merge_scale_at(progress);
            (s > 0.0).then(|| Merge {
                rect: Rect::new(f.cx() - f.w * s * 0.5, f.cy() - f.h * s * 0.5, f.w * s, f.h * s),
                radius: pill_radius * s,
                k: MERGE_K,
            })
        });
        Frame {
            rect: rect_at_xy(from, panel, pw, ph),
            radius: if from.is_some() { radius_at(pill_radius, panel_radius, ph) } else { panel_radius },
            progress,
            ground_alpha,
            content_alpha: ramp(progress, ROWS_FROM_AT, ROWS_OPAQUE_AT) * (1.0 - out),
            rows_scale: self.rows.pos,
            opener_alpha: if from.is_some() { opener_alpha_at(progress) } else { 1.0 },
            merge,
            clear: if from.is_some() { clear_at(progress).max(out) } else { 0.0 },
        }
    }
}

impl Default for Morph {
    fn default() -> Self {
        Morph::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;
    const PILL: Rect = Rect::new(1500.0, 40.0, 196.0, 38.0);
    const PANEL: Rect = Rect::new(1380.0, 40.0, 316.0, 400.0);
    /// A caller's own panel radius, deliberately not the board's 22.
    const RAD: f32 = 26.0;

    fn open_frames() -> Vec<(Morph, Frame)> {
        let mut m = Morph::new();
        let mut out = vec![(m, m.frame(Some(PILL), PANEL, RAD))];
        for _ in 0..120 {
            m.step(true, DT);
            out.push((m, m.frame(Some(PILL), PANEL, RAD)));
        }
        out
    }

    #[test]
    fn starts_as_the_pill_and_settles_on_the_panel() {
        let f = open_frames();
        assert_eq!(f[0].1.rect, PILL);
        assert_eq!(f[0].1.radius, PILL.h * 0.5);
        assert_eq!((f[0].1.ground_alpha, f[0].1.content_alpha), (0.0, 0.0));
        let (m, last) = f.last().copied().unwrap();
        assert!(m.settled());
        assert!((last.rect.w - PANEL.w).abs() < 0.5 && (last.rect.h - PANEL.h).abs() < 0.5);
        assert!((last.rect.x - PANEL.x).abs() < 0.5 && (last.rect.y - PANEL.y).abs() < 0.5);
        assert_eq!((last.ground_alpha, last.content_alpha), (1.0, 1.0));
        assert!((last.rows_scale - 1.0).abs() < 0.002);
    }

    #[test]
    fn width_and_height_grow_monotonically_without_overshoot() {
        let f = open_frames();
        for pair in f.windows(2) {
            assert!(pair[1].1.rect.w >= pair[0].1.rect.w - 1e-3, "width never shrinks on open");
            assert!(pair[1].1.rect.h >= pair[0].1.rect.h - 1e-3, "height never shrinks on open");
        }
        assert!(f.iter().all(|(_, fr)| fr.rect.w <= PANEL.w + 1e-3 && fr.rect.h <= PANEL.h + 1e-3));
    }

    #[test]
    fn height_leads_width_by_the_stiffness_ratio() {
        let mut m = Morph::new();
        for _ in 0..6 {
            m.step(true, DT);
        }
        let fr = m.frame(Some(PILL), PANEL, RAD);
        let (pw, ph) = ((fr.rect.w - PILL.w) / (PANEL.w - PILL.w), (fr.rect.h - PILL.h) / (PANEL.h - PILL.h));
        assert!(ph > pw, "the stiffer height spring is ahead of the width ({ph} vs {pw})");
    }

    #[test]
    fn open_and_close_durations_follow_from_the_stiffnesses() {
        let mut m = Morph::new();
        for _ in 0..(0.19 / DT).round() as usize {
            m.step(true, DT);
        }
        let fr = m.frame(Some(PILL), PANEL, RAD);
        assert!((fr.rect.w - PANEL.w).abs() < 0.07 * (PANEL.w - PILL.w), "open lands by ~190 ms");
        // A close leads with its content phase (two frames to `OUT_GATE`, then the rect springs),
        // so the slab is near the pill by ~150 ms after the gate, ~50 ms after the press.
        for _ in 0..((0.15 + 3.0 * DT) / DT).round() as usize {
            m.step(false, DT);
        }
        let fr = m.frame(Some(PILL), PANEL, RAD);
        assert!((fr.rect.w - PILL.w).abs() < 0.12 * (PANEL.w - PILL.w), "close is near the pill ~150 ms after the gate");
        for _ in 0..120 {
            m.step(false, DT);
        }
        assert!(m.closed());
    }

    #[test]
    fn radius_bulges_and_ends_at_the_callers_panel_radius() {
        let f = open_frames();
        let pill_r = PILL.h * 0.5;
        assert_eq!(f[0].1.radius, pill_r);
        let peak = f.iter().map(|(_, fr)| fr.radius).fold(0.0_f32, f32::max);
        assert!(peak > RAD + 8.0, "the corner swells past both ends ({peak})");
        assert!((f.last().unwrap().1.radius - RAD).abs() < 0.05, "the morph ends on the caller's radius, not 22");
        assert_eq!(radius_at(pill_r, RAD, 0.0), pill_r);
        assert_eq!(radius_at(pill_r, RAD, 1.0), RAD);
    }

    /// The handover frames carry no radius snap: from the first frame `settled()` reports, to the
    /// caller's own radius, the step is a sub-pixel; and the first close frame is the same value.
    #[test]
    fn the_radius_is_continuous_into_and_out_of_the_settled_panel() {
        let mut m = Morph::new();
        let mut prev = m.frame(Some(PILL), PANEL, RAD).radius;
        let mut settled_at = None;
        for _ in 0..240 {
            m.step(true, DT);
            let r = m.frame(Some(PILL), PANEL, RAD).radius;
            if settled_at.is_none() && m.settled() {
                settled_at = Some(r);
                assert!((r - RAD).abs() < 0.05, "settling hands the caller's radius {RAD} a {r}");
                assert!((r - prev).abs() < 1.0, "no snap at settle ({prev} -> {r})");
            }
            prev = r;
        }
        assert!(settled_at.is_some());
        let at_rest = m.frame(Some(PILL), PANEL, RAD).radius;
        assert!((at_rest - RAD).abs() < 0.05, "the close starts on the caller's radius ({at_rest})");
    }

    #[test]
    fn rows_scale_from_start_to_one_with_overshoot_bounded_by_zeta() {
        let f = open_frames();
        assert_eq!(f[0].1.rows_scale, ROWS_FROM);
        let max = f.iter().map(|(_, fr)| fr.rows_scale).fold(0.0_f32, f32::max);
        // The underdamped overshoot bound, 1 + (1 - from) * e^(-pi z / sqrt(1 - z^2)), at z = 0.6 and
        // from = 0.82: e^-2.3562 = 0.09479. Written as a number because the transcendental
        // surface is gated outside machine/src/motion.rs (ci/check-deps.sh libm).
        assert_eq!(ZETA_ROWS, 0.6);
        let bound = 1.0 + (1.0 - ROWS_FROM) * 0.09479;
        assert!(max > 1.0 && max <= bound + 1e-3, "overshoot {max} within the ζ bound {bound}");
        assert!((f.last().unwrap().1.rows_scale - 1.0).abs() < 0.002);
    }

    /// An open and the close after it, every frame.
    fn open_then_close(from: Option<Rect>) -> (Vec<Frame>, Vec<Frame>) {
        let mut m = Morph::new();
        let mut open = vec![m.frame(from, PANEL, RAD)];
        for _ in 0..120 {
            m.step(true, DT);
            open.push(m.frame(from, PANEL, RAD));
        }
        let mut close = Vec::new();
        for _ in 0..120 {
            m.step(false, DT);
            close.push(m.frame(from, PANEL, RAD));
        }
        (open, close)
    }

    /// Spec (d): the glass's share crosses in over the first [`FILL_AT`] of the growth and a close
    /// lands ON the pill's rect with none of it left.
    #[test]
    fn the_glass_crosses_in_over_the_first_of_the_growth_and_a_close_lands_on_the_pill() {
        let (open, close) = open_then_close(Some(PILL));
        for fr in open.iter().chain(&close) {
            if fr.progress >= FILL_AT {
                assert_eq!(fr.ground_alpha, 1.0, "all glass from {FILL_AT}: {fr:?}");
            }
        }
        assert!(open.iter().any(|fr| fr.ground_alpha > 0.0 && fr.ground_alpha < 1.0), "the fill crosses over frames");
        let last = close.last().unwrap();
        assert_eq!((last.rect, last.ground_alpha), (PILL, 0.0), "a close lands ON the pill");
    }

    /// Spec (c): no row before the glass has taken over, and none left by the time the rect is back
    /// near the pill.
    #[test]
    fn rows_only_draw_on_a_surface_that_is_all_glass() {
        let (open, close) = open_then_close(Some(PILL));
        for fr in open.iter().chain(&close) {
            if fr.content_alpha > 0.0 {
                assert!(fr.progress > ROWS_FROM_AT && fr.ground_alpha == 1.0, "a row on a part-pill surface: {fr:?}");
            }
        }
        assert!(open.iter().take_while(|fr| fr.progress < ROWS_FROM_AT).count() >= 2, "the rows wait for the glass");
        // The rows leave FIRST on a close (the `out` spring), while the rect is still the panel's.
        let gone = close.iter().find(|fr| fr.content_alpha < 0.05).expect("the rows leave");
        assert!(gone.rect.w > PILL.w + 0.4 * (PANEL.w - PILL.w), "the rows are gone while the rect is still large: {gone:?}");
        assert_eq!(open.last().unwrap().content_alpha, 1.0);
    }

    /// The growing shape is all glass before it gets big.
    #[test]
    fn the_glass_is_whole_by_a_fifth_of_the_growth() {
        assert!(FILL_AT <= 0.18 + 1e-6);
        let (open, _) = open_then_close(Some(PILL));
        let big = open.iter().find(|fr| fr.rect.w > PILL.w + 0.2 * (PANEL.w - PILL.w)).expect("it grows");
        assert_eq!(big.ground_alpha, 1.0, "all glass by the time it is 20% grown: {big:?}");
        // the rows still wait for their own threshold, and never draw on a part-glass surface
        assert_eq!(ROWS_FROM_AT, 0.3);
        assert!(FILL_AT < ROWS_FROM_AT);
    }

    #[test]
    fn without_an_opener_rect_the_panel_fades_and_scales_about_its_centre() {
        let m = Morph::new();
        let fr = m.frame(None, PANEL, RAD);
        assert!((fr.rect.w - PANEL.w * FALLBACK_SCALE_FROM).abs() < 1e-3);
        assert!((fr.rect.cx() - PANEL.cx()).abs() < 1e-3 && (fr.rect.cy() - PANEL.cy()).abs() < 1e-3);
        assert_eq!(rect_at(None, PANEL, 1.0), PANEL);
        let (open, close) = open_then_close(None);
        assert!(open.iter().chain(&close).all(|fr| fr.rect.w <= PANEL.w + 1e-3), "no opener: never past the panel");
        assert_eq!(rect_at(Some(PILL), PANEL, 0.0), PILL);
        assert_eq!(rect_at(Some(PILL), PANEL, 1.0), PANEL);
        // and with no opener there is nothing to consume, join or clear
        for fr in open.iter().chain(&close) {
            assert_eq!((fr.opener_alpha, fr.merge, fr.clear), (1.0, None, 0.0), "the fallback has no droplet: {fr:?}");
        }
    }

    /// A settled open panel draws exactly as it did before the droplet: no merge, frosted, whole.
    #[test]
    fn a_settled_panel_has_no_merge_no_clear_and_a_consumed_opener() {
        let (open, _) = open_then_close(Some(PILL));
        let last = open.last().unwrap();
        assert!((last.rect.x - PANEL.x).abs() + (last.rect.y - PANEL.y).abs() < 0.01, "on the panel");
        assert!((last.rect.w - PANEL.w).abs() + (last.rect.h - PANEL.h).abs() < 0.01, "on the panel");
        assert_eq!((last.merge, last.clear), (None, 0.0));
        assert_eq!((last.ground_alpha, last.content_alpha), (1.0, 1.0));
        assert_eq!(last.opener_alpha, 0.0, "the opener is consumed by the open panel");
    }

    /// OPEN: the glass is clear and joined to the whole opener at the start, and the join and the
    /// clarity leave on their own thresholds.
    #[test]
    fn the_open_starts_as_a_clear_droplet_on_the_opener_and_frosts_as_it_grows() {
        let (open, _) = open_then_close(Some(PILL));
        let first = open[0];
        let m = first.merge.expect("a merge at progress 0");
        assert_eq!((m.rect, m.k), (PILL, MERGE_K));
        assert_eq!(m.radius, PILL.h * 0.5);
        assert_eq!((first.clear, first.opener_alpha), (1.0, 1.0));
        for fr in &open {
            if fr.progress >= MERGE_END_AT {
                assert_eq!(fr.merge, None, "no merge once the shape is gone: {fr:?}");
            }
            if fr.progress >= FROST_TO {
                assert_eq!(fr.clear, 0.0, "frosted from {FROST_TO}: {fr:?}");
            }
            if fr.progress <= FROST_FROM {
                assert_eq!(fr.clear, 1.0, "clear to {FROST_FROM}: {fr:?}");
            }
            if let Some(m) = fr.merge {
                assert!((m.rect.cx() - PILL.cx()).abs() < 1e-3 && (m.rect.cy() - PILL.cy()).abs() < 1e-3, "about its own centre");
                assert!(m.rect.w <= PILL.w + 1e-3 && m.radius <= PILL.h * 0.5 + 1e-3);
            }
        }
        for pair in open.windows(2) {
            let s = |f: &Frame| f.merge.map_or(0.0, |m| m.rect.w);
            assert!(s(&pair[1]) <= s(&pair[0]) + 1e-3, "the merge only shrinks on an open");
            assert!(pair[1].clear <= pair[0].clear + 1e-3, "the glass only frosts on an open");
            assert!(pair[1].opener_alpha <= pair[0].opener_alpha + 1e-3, "the opener only fades on an open");
        }
        assert!(open.iter().any(|f| f.opener_alpha > 0.0 && f.opener_alpha < 1.0), "the opener fades over frames");
        assert!(open.iter().all(|f| f.progress < OPENER_OUT_AT || f.opener_alpha == 0.0), "consumed by {OPENER_OUT_AT}");
    }

    /// CLOSE, phase 1: the rows and the frost leave in a couple of frames while the rect HOLDS.
    #[test]
    fn a_close_leads_with_the_content_and_the_frost_and_holds_the_rect_until_the_gate() {
        let (open, close) = open_then_close(Some(PILL));
        let rest = open.last().unwrap().rect;
        let c1 = close[0];
        assert_eq!(c1.rect, rest, "the rect has not moved on the first close frame");
        assert!(c1.content_alpha < 0.6 && c1.clear > 0.4, "the content and the frost are already leaving: {c1:?}");
        let moved = close.iter().position(|f| f.rect != rest).expect("the rect leaves");
        assert!((1..=3).contains(&moved), "the gate opens within a few frames ({moved})");
        let at = close[moved];
        assert!(at.content_alpha <= 1.0 - OUT_GATE + 0.01 && at.clear >= OUT_GATE - 0.01, "gated on out: {at:?}");
        assert!(close.iter().take(moved).all(|f| f.rect == rest), "held, not drifting");
        assert!(close.iter().skip(4).all(|f| f.clear >= 0.97), "the slab is clear for the rest of the close");
        assert!(close.iter().skip(5).all(|f| f.content_alpha < 0.02), "no rows on the way back");
    }

    /// CLOSE, phases 2 and 3: the clear slab shrinks back, the droplet re-forms under the neck
    /// reach as progress falls, and the opener returns whole at the very end.
    #[test]
    fn a_close_drains_into_a_reforming_droplet_and_the_opener_returns() {
        let (_, close) = open_then_close(Some(PILL));
        for fr in &close {
            if fr.progress >= MERGE_END_AT {
                assert_eq!(fr.merge, None, "no droplet while the slab is far: {fr:?}");
            }
            if fr.progress >= OPENER_OUT_AT {
                assert_eq!(fr.opener_alpha, 0.0, "the opener stays consumed until the last 15%: {fr:?}");
            }
        }
        let forming = close.iter().find(|f| f.merge.is_some()).expect("the droplet re-forms");
        assert!(forming.progress < MERGE_END_AT && forming.clear > 0.99);
        for pair in close.windows(2) {
            let s = |f: &Frame| f.merge.map_or(0.0, |m| m.rect.w);
            assert!(s(&pair[1]) >= s(&pair[0]) - 1e-3, "the droplet only grows on a close");
            assert!(pair[1].opener_alpha >= pair[0].opener_alpha - 1e-3, "the opener only returns on a close");
        }
        let last = close.last().unwrap();
        assert_eq!((last.rect, last.opener_alpha), (PILL, 1.0), "whole again, on the opener");
        let m = last.merge.expect("the droplet is the opener's own shape at the end");
        assert_eq!((m.rect, m.radius), (PILL, PILL.h * 0.5));
    }

    /// The `out` spring is stiff (k 9000: ω·dt = 1.6 at 60 fps), but it is the closed form, so it
    /// is stable at any frame time: in range and monotone at 60, 30 and 15 fps.
    #[test]
    fn the_content_phase_is_monotone_and_in_range_at_any_frame_time() {
        for dt in [DT, 2.0 * DT, 4.0 * DT] {
            let mut m = Morph::new();
            for _ in 0..120 {
                m.step(true, dt);
            }
            assert!(m.settled());
            let mut prev = m.frame(Some(PILL), PANEL, RAD);
            for _ in 0..40 {
                m.step(false, dt);
                let f = m.frame(Some(PILL), PANEL, RAD);
                for v in [f.content_alpha, f.clear, f.opener_alpha, f.ground_alpha] {
                    assert!((0.0..=1.0).contains(&v), "in range at dt {dt}: {f:?}");
                }
                assert!(f.content_alpha <= prev.content_alpha + 1e-4, "content only leaves at dt {dt}");
                assert!(f.clear >= prev.clear - 1e-4, "frost only leaves at dt {dt}");
                prev = f;
            }
            assert!(m.closed());
        }
    }

    /// A re-open during a close brings the content back: `out` springs home and the panel settles.
    #[test]
    fn a_reopen_during_a_close_brings_the_content_back_and_settles() {
        let mut m = Morph::new();
        for _ in 0..120 {
            m.step(true, DT);
        }
        for _ in 0..4 {
            m.step(false, DT);
        }
        assert!(!m.settled() && m.frame(Some(PILL), PANEL, RAD).content_alpha < 0.2);
        for _ in 0..120 {
            m.step(true, DT);
        }
        let f = m.frame(Some(PILL), PANEL, RAD);
        assert!(m.settled(), "the re-open settles");
        assert!(f.content_alpha > 0.999 && f.clear < 0.001 && f.merge.is_none(), "{f:?}");
    }
}
