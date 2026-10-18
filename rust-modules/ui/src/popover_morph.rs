//! The **morph** open style: a glass opener (a pill) grows into its panel instead of fading and
//! sliding in on [`crate::popover::K_APPEAR`]. Selected per popover (see
//! [`crate::popover::Popover::morphing`], the ONE selector; open it with
//! [`crate::popover::Popover::open_from`] and the opener's rect, and `Popover::panel` lays the glass
//! at full strength at the morphing rect and radius while the opener's plate fades off it; only
//! the fade-and-slide fallback fades the glass in). The item hold
//! menu is not a morph: it keeps the opener lift and the shared appear.
//!
//! Source: design board `Motion`, card "Sort & Filter" (end state `WatchlistOptions`): the label
//! pill becomes a panel that fits its rows while the label dissolves into them. The board states
//! the springs this module names; where it disagrees with the brief the brief wins (recorded on
//! the constants below).
//!
//! **Four springs, one clock.** The panel rect's width and height are separate critically damped
//! springs (width the slower, so the pill widens after it has begun to open); the corner radius is
//! derived from the height spring with a bulge; the rows ride an UNDER-damped scale spring
//! (`0.82 -> 1`, ζ 0.6, so they settle with a 1.7% overshoot rather than ringing). Durations are
//! not stated, they fall out of the stiffnesses: the height spring (the one the eye follows) is
//! within 6% of rest at 150 ms and the width spring within 6% at 190 ms.
//!
//! **One surface, one clock.** The pill and the panel are ONE surface whose rect, radius and fill
//! animate. Everything but the rows' scale is a function of [`Frame::progress`], the slower of the
//! two rect springs (0 = the opener's rect, 1 = the panel):
//!
//! - the FILL crosses from the opener's own (the focused pill's accent plate) to the glass over
//!   the first [`FILL_AT`] of the progress, a smoothstep: the glass is laid at full strength under
//!   the plate and the plate fades off it ([`Frame::pill_alpha`] `= 1 -` [`Frame::ground_alpha`]),
//!   so the surface is covered on every frame, and there is never a frame with no pill and no
//!   glass. The opener's LABEL has its own, longer ramp ([`Frame::label_alpha`], to
//!   [`LABEL_GONE_AT`]): it dissolves into the rows as they come, and a close brings it back while
//!   the rows are still leaving, so the glass is never bare of both;
//! - the ROWS ([`Frame::content_alpha`]) start at [`ROWS_FROM_AT`], once the glass has taken over,
//!   and are opaque by [`ROWS_OPAQUE_AT`]; on a close they are gone while the rect is still well
//!   larger than the pill. They are clipped to the current rect (`Popover::morph_rows`), so no row
//!   ever draws outside the surface.
//!
//! A close runs the same curve backwards and ends ON the opener: plate and label at full strength
//! at the pill's rect and radius, which is what the page draws there once the menu has gone. The
//! plate's colour is the caller's: the pill AS IT WILL REST (focused when focus returns to it, the
//! idle fill when focus went elsewhere), so the glass never passes through a colour the pill will
//! not have.
//!
//! **No measured opener, no morph.** [`Frame`] with `from == None` is the fade+scale FALLBACK:
//! the panel scales about its centre from [`FALLBACK_SCALE_FROM`] to 1 and its glass fades in on
//! the same fill curve; there is no plate and no label.

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
/// Both rect springs while CLOSING: the height stiffness, so the close takes ≈150 ms (the open's
/// slower width spring would make it ≈190).
pub const K_CLOSE: f32 = K_HEIGHT;
/// The rows' scale spring stiffness (brief: k ~ 720).
pub const K_ROWS: f32 = 720.0;
/// The rows' damping ratio (brief and board: 0.6 — underdamped, one small overshoot).
pub const ZETA_ROWS: f32 = 0.6;
/// The rows' scale when the morph starts (the board's `scale(.82)`).
pub const ROWS_FROM: f32 = 0.82;
/// Progress at which the glass has fully replaced the opener's fill: the growing shape is mostly
/// glass before it gets big (the accent plate used to hold on to ~30% of the growth).
pub const FILL_AT: f32 = 0.18;
/// Progress at which the opener's LABEL is gone (and, on a close, where it is back in full): the
/// label outlasts the plate, so on a close it fades in while the rows are still fading out and
/// the glass is never bare. Linear, so the two crossfades overlap by about a fifth of the way.
pub const LABEL_GONE_AT: f32 = 0.45;
/// Progress at which the rows begin to appear: not before the glass has taken over.
pub const ROWS_FROM_AT: f32 = 0.3;
/// Progress at which the rows are fully opaque.
pub const ROWS_OPAQUE_AT: f32 = 0.6;
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
    /// The glass's share of the fill (0 = the opener's fill only, 1 = glass only). With an opener
    /// the glass is laid at FULL strength once this is above 0 and the plate fades off it; for
    /// the fallback it is the glass's alpha.
    pub ground_alpha: f32,
    /// Alpha of the panel's rows: 0 until [`ROWS_FROM_AT`], so a row is only ever drawn on a
    /// surface whose fill is all glass.
    pub content_alpha: f32,
    /// Scale of the rows about their own centres (0.82 -> 1, with a small overshoot).
    pub rows_scale: f32,
    /// Alpha of the opener's plate: `1 - ground_alpha` with an opener, 0 without.
    pub pill_alpha: f32,
    /// Alpha of the opener's label: it dissolves over `[0, LABEL_GONE_AT]`, past the plate, so a
    /// close brings it back while the rows are still leaving. 0 without an opener.
    pub label_alpha: f32,
}

/// `x` mapped from `[a, b]` to `[0, 1]`, clamped.
fn ramp(x: f32, a: f32, b: f32) -> f32 {
    ((x - a) / (b - a)).clamp(0.0, 1.0)
}

/// The fill crossover at progress `t`: a smoothstep over `[0, FILL_AT]`.
pub fn fill_at(t: f32) -> f32 {
    let u = ramp(t, 0.0, FILL_AT);
    u * u * (3.0 - 2.0 * u)
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
}

impl Morph {
    pub const fn new() -> Self {
        Morph { w: Spring::at(0.0), h: Spring::at(0.0), rows: Spring::at(ROWS_FROM) }
    }

    /// Back to the pill, with no motion in between.
    pub fn reset(&mut self) {
        *self = Morph::new();
    }

    /// Advance one frame toward the open (`true`) or closed (`false`) end.
    pub fn step(&mut self, open: bool, dt: f32) {
        let (t, kw, kh, rows_to) = if open {
            (1.0, K_WIDTH, K_HEIGHT, 1.0)
        } else {
            (0.0, K_CLOSE, K_CLOSE, ROWS_FROM)
        };
        self.w.step(t, kw, dt);
        self.h.step(t, kh, dt);
        self.rows.step_zeta(rows_to, K_ROWS, ZETA_ROWS, dt);
    }

    /// Has the close finished — the rect back on the pill and the rows back at their start?
    pub fn closed(&self) -> bool {
        self.w.pos <= 0.001 && self.h.pos <= 0.001 && self.rows.pos <= ROWS_FROM + 0.001
    }

    /// Has the open visibly arrived?
    pub fn settled(&self) -> bool {
        self.w.pos >= 0.999 && self.h.pos >= 0.999 && (self.rows.pos - 1.0).abs() <= 0.001
    }

    /// The draw state now, for the opener rect `from` (`None` = the fade+scale fallback) and the
    /// final `panel` rect.
    ///
    /// `panel_radius` is the caller's own panel corner radius: the morph ENDS on it (and a close
    /// starts from it), so the settled ground and the morphing one are the same shape.
    pub fn frame(&self, from: Option<Rect>, panel: Rect, panel_radius: f32) -> Frame {
        let (pw, ph) = (self.w.pos.clamp(0.0, 1.0), self.h.pos.clamp(0.0, 1.0));
        let progress = pw.min(ph);
        let ground_alpha = fill_at(progress);
        let pill_radius = from.map_or(panel_radius, |f| f.w.min(f.h) * 0.5);
        Frame {
            rect: rect_at_xy(from, panel, pw, ph),
            radius: if from.is_some() { radius_at(pill_radius, panel_radius, ph) } else { panel_radius },
            progress,
            ground_alpha,
            content_alpha: ramp(progress, ROWS_FROM_AT, ROWS_OPAQUE_AT),
            rows_scale: self.rows.pos,
            pill_alpha: if from.is_some() { 1.0 - ground_alpha } else { 0.0 },
            label_alpha: if from.is_some() { 1.0 - ramp(progress, 0.0, LABEL_GONE_AT) } else { 0.0 },
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
        assert_eq!((f[0].1.ground_alpha, f[0].1.content_alpha, f[0].1.pill_alpha), (0.0, 0.0, 1.0));
        let (m, last) = f.last().copied().unwrap();
        assert!(m.settled());
        assert!((last.rect.w - PANEL.w).abs() < 0.5 && (last.rect.h - PANEL.h).abs() < 0.5);
        assert!((last.rect.x - PANEL.x).abs() < 0.5 && (last.rect.y - PANEL.y).abs() < 0.5);
        assert_eq!((last.ground_alpha, last.content_alpha, last.pill_alpha), (1.0, 1.0, 0.0));
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
        for _ in 0..(0.15 / DT).round() as usize {
            m.step(false, DT);
        }
        let fr = m.frame(Some(PILL), PANEL, RAD);
        assert!((fr.rect.w - PILL.w).abs() < 0.12 * (PANEL.w - PILL.w), "close is near the pill by ~150 ms");
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

    /// Spec (d): the pill's plate and the glass are one surface, so no frame has neither.
    #[test]
    fn the_fill_crosses_from_the_pill_to_the_glass_with_no_frame_of_neither() {
        let (open, close) = open_then_close(Some(PILL));
        for fr in open.iter().chain(&close) {
            assert!((fr.pill_alpha + fr.ground_alpha - 1.0).abs() < 1e-6, "plate + glass cover the surface: {fr:?}");
            if fr.progress >= FILL_AT {
                assert_eq!((fr.ground_alpha, fr.pill_alpha), (1.0, 0.0), "all glass from {FILL_AT}: {fr:?}");
            }
        }
        assert!(open.iter().any(|fr| fr.ground_alpha > 0.0 && fr.ground_alpha < 1.0), "the fill crosses over frames");
        let last = close.last().unwrap();
        assert_eq!((last.rect, last.pill_alpha, last.ground_alpha), (PILL, 1.0, 0.0), "a close lands ON the pill");
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
        let gone = close.iter().find(|fr| fr.content_alpha == 0.0).expect("the rows leave");
        assert!(gone.rect.w > PILL.w + 0.2 * (PANEL.w - PILL.w), "the rows are gone while the rect is still large: {gone:?}");
        assert_eq!(open.last().unwrap().content_alpha, 1.0);
    }

    /// The accent plate is gone early: the growing shape is mostly glass before it gets big.
    #[test]
    fn the_plate_is_gone_by_a_fifth_of_the_growth() {
        assert!(FILL_AT <= 0.18 + 1e-6);
        let (open, _) = open_then_close(Some(PILL));
        let big = open.iter().find(|fr| fr.rect.w > PILL.w + 0.2 * (PANEL.w - PILL.w)).expect("it grows");
        assert_eq!((big.ground_alpha, big.pill_alpha), (1.0, 0.0), "all glass by the time it is 20% grown: {big:?}");
        // the rows still wait for their own threshold, and never draw on a part-pill surface
        assert_eq!(ROWS_FROM_AT, 0.3);
        assert!(FILL_AT < ROWS_FROM_AT && LABEL_GONE_AT > ROWS_FROM_AT);
    }

    /// On a close the glass is never bare: every frame with glass on screen has the rows or the
    /// pill's label (or the plate) visible, so no frame is dark glass with neither.
    #[test]
    fn a_close_never_shows_glass_with_neither_rows_nor_label() {
        let (_, close) = open_then_close(Some(PILL));
        for fr in &close {
            if fr.ground_alpha > 0.0 && fr.pill_alpha < 1.0 {
                assert!(
                    fr.content_alpha.max(fr.label_alpha) >= 0.1,
                    "a frame of bare glass on the close (rows {}, label {}, progress {}): {fr:?}",
                    fr.content_alpha, fr.label_alpha, fr.progress
                );
            }
        }
        // and the label really does overlap the rows' fade-out
        assert!(close.iter().any(|fr| fr.content_alpha > 0.0 && fr.label_alpha > 0.0), "the two crossfade");
        let last = close.last().unwrap();
        assert_eq!((last.label_alpha, last.pill_alpha), (1.0, 1.0), "a close lands with the label whole");
    }

    #[test]
    fn without_an_opener_rect_the_panel_fades_and_scales_about_its_centre() {
        let m = Morph::new();
        let fr = m.frame(None, PANEL, RAD);
        assert!((fr.rect.w - PANEL.w * FALLBACK_SCALE_FROM).abs() < 1e-3);
        assert!((fr.rect.cx() - PANEL.cx()).abs() < 1e-3 && (fr.rect.cy() - PANEL.cy()).abs() < 1e-3);
        assert_eq!(rect_at(None, PANEL, 1.0), PANEL);
        let (open, close) = open_then_close(None);
        assert!(open.iter().chain(&close).all(|fr| fr.pill_alpha == 0.0), "no opener, no plate");
        assert_eq!(rect_at(Some(PILL), PANEL, 0.0), PILL);
        assert_eq!(rect_at(Some(PILL), PANEL, 1.0), PANEL);
    }
}
