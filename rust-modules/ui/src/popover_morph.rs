//! The **morph** open style: a glass opener (a pill) grows into its panel instead of fading and
//! sliding in on [`crate::popover::K_APPEAR`]. Selected per popover (see
//! [`crate::popover::Popover::morphing`]) and named on the opener by
//! [`crate::popover::Opener::open_style`] = [`crate::popover::OpenStyle::Morph`]. The item hold
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
//! **Never an empty glass frame.** The glass ground, the rows and the pill face are all functions
//! of the ONE rows-progress number, and the ramps are ordered so the content always leads the
//! ground: [`Frame::content_alpha`] reaches 1 at half the rows progress that [`Frame::ground_alpha`]
//! needs, so a frame with any glass visible has rows on it (`content_alpha >= ground_alpha`,
//! tested for every frame of an open and a close). The pill fades out as the ground fades in
//! (`pill_alpha = 1 - ground_alpha`): a cross-dissolve, never a gap.
//!
//! **No measured opener, no morph.** [`Frame`] with `from == None` is the fade+scale FALLBACK:
//! the panel scales about its centre from [`FALLBACK_SCALE_FROM`] to 1 and fades.

use crate::{Rect, Spring};

/// The panel's corner radius at rest — the board's morph ends on `border-radius: 22px`.
/// (Shipped panels state their own radius and keep it; this is the morph's own end value, so the
/// handover from the morphing rect to the settled panel does not jump.)
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
/// Rows progress at which the rows are fully opaque.
pub const ROWS_OPAQUE_AT: f32 = 0.5;
/// Rows progress at which the glass ground is fully opaque. Larger than [`ROWS_OPAQUE_AT`] on
/// purpose: that ordering is the "never an empty glass frame" invariant.
pub const GROUND_OPAQUE_AT: f32 = 0.8;
/// Fallback when the opener has no measured rect to morph from: a fade+scale of the panel about
/// its centre, starting at this scale. The board's `menuout` keyframes leave a menu at 0.92, so an
/// entrance from the same scale is the matching motion.
pub const FALLBACK_SCALE_FROM: f32 = 0.92;

/// Everything a draw needs from the morph at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The glass panel's current rect.
    pub rect: Rect,
    /// Its current corner radius.
    pub radius: f32,
    /// Alpha of the glass ground (0 = pill only).
    pub ground_alpha: f32,
    /// Alpha of the panel's rows; never below [`ground_alpha`](Self::ground_alpha).
    pub content_alpha: f32,
    /// Scale of the rows about their own centres (0.82 -> 1, with a small overshoot).
    pub rows_scale: f32,
    /// Alpha of the opener's pill face: the cross-dissolve partner of the ground.
    pub pill_alpha: f32,
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

/// The corner radius at height progress `ph`: the straight ramp from the pill's own radius to
/// [`PANEL_RADIUS`], plus a [`RADIUS_BULGE`] swell that is zero at both ends.
pub fn radius_at(pill_radius: f32, ph: f32) -> f32 {
    let ph = ph.clamp(0.0, 1.0);
    pill_radius + (PANEL_RADIUS - pill_radius) * ph + RADIUS_BULGE * 4.0 * ph * (1.0 - ph)
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

    /// The rows spring's progress, 0 at [`ROWS_FROM`], 1 at rest. Overshoot is clamped away.
    fn rows_progress(&self) -> f32 {
        ((self.rows.pos - ROWS_FROM) / (1.0 - ROWS_FROM)).clamp(0.0, 1.0)
    }

    /// The draw state now, for the opener rect `from` (`None` = the fade+scale fallback) and the
    /// final `panel` rect.
    pub fn frame(&self, from: Option<Rect>, panel: Rect) -> Frame {
        let (pw, ph) = (self.w.pos.clamp(0.0, 1.0), self.h.pos.clamp(0.0, 1.0));
        let rp = self.rows_progress();
        let content_alpha = (rp / ROWS_OPAQUE_AT).clamp(0.0, 1.0);
        let ground_alpha = (rp / GROUND_OPAQUE_AT).clamp(0.0, 1.0);
        let pill_radius = from.map_or(PANEL_RADIUS, |f| f.w.min(f.h) * 0.5);
        Frame {
            rect: rect_at_xy(from, panel, pw, ph),
            radius: if from.is_some() { radius_at(pill_radius, ph) } else { PANEL_RADIUS },
            ground_alpha,
            content_alpha,
            rows_scale: self.rows.pos,
            pill_alpha: 1.0 - ground_alpha,
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

    fn open_frames() -> Vec<(Morph, Frame)> {
        let mut m = Morph::new();
        let mut out = vec![(m, m.frame(Some(PILL), PANEL))];
        for _ in 0..120 {
            m.step(true, DT);
            out.push((m, m.frame(Some(PILL), PANEL)));
        }
        out
    }

    #[test]
    fn starts_as_the_pill_and_settles_on_the_panel() {
        let f = open_frames();
        assert_eq!(f[0].1.rect, PILL);
        assert_eq!(f[0].1.ground_alpha, 0.0);
        assert_eq!(f[0].1.pill_alpha, 1.0);
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
        let fr = m.frame(Some(PILL), PANEL);
        let (pw, ph) = ((fr.rect.w - PILL.w) / (PANEL.w - PILL.w), (fr.rect.h - PILL.h) / (PANEL.h - PILL.h));
        assert!(ph > pw, "the stiffer height spring is ahead of the width ({ph} vs {pw})");
    }

    #[test]
    fn open_and_close_durations_follow_from_the_stiffnesses() {
        let mut m = Morph::new();
        for _ in 0..(0.19 / DT).round() as usize {
            m.step(true, DT);
        }
        let fr = m.frame(Some(PILL), PANEL);
        assert!((fr.rect.w - PANEL.w).abs() < 0.07 * (PANEL.w - PILL.w), "open lands by ~190 ms");
        for _ in 0..(0.15 / DT).round() as usize {
            m.step(false, DT);
        }
        let fr = m.frame(Some(PILL), PANEL);
        assert!((fr.rect.w - PILL.w).abs() < 0.12 * (PANEL.w - PILL.w), "close is near the pill by ~150 ms");
        for _ in 0..120 {
            m.step(false, DT);
        }
        assert!(m.closed());
    }

    #[test]
    fn radius_bulges_and_ends_at_the_panel_radius() {
        let f = open_frames();
        let pill_r = PILL.h * 0.5;
        assert_eq!(f[0].1.radius, pill_r);
        let peak = f.iter().map(|(_, fr)| fr.radius).fold(0.0_f32, f32::max);
        assert!(peak > PANEL_RADIUS + 8.0, "the corner swells past both ends ({peak})");
        assert!((f.last().unwrap().1.radius - PANEL_RADIUS).abs() < 0.05);
        assert_eq!(radius_at(pill_r, 0.0), pill_r);
        assert_eq!(radius_at(pill_r, 1.0), PANEL_RADIUS);
    }

    #[test]
    fn rows_scale_from_start_to_one_with_overshoot_bounded_by_zeta() {
        let f = open_frames();
        assert_eq!(f[0].1.rows_scale, ROWS_FROM);
        let max = f.iter().map(|(_, fr)| fr.rows_scale).fold(0.0_f32, f32::max);
        let bound = 1.0 + (1.0 - ROWS_FROM) * (-std::f32::consts::PI * ZETA_ROWS / (1.0 - ZETA_ROWS * ZETA_ROWS).sqrt()).exp();
        assert!(max > 1.0 && max <= bound + 1e-3, "overshoot {max} within the ζ bound {bound}");
        assert!((f.last().unwrap().1.rows_scale - 1.0).abs() < 0.002);
    }

    #[test]
    fn no_frame_shows_glass_without_rows_on_open_or_close() {
        let mut frames: Vec<Frame> = open_frames().into_iter().map(|(_, fr)| fr).collect();
        let mut m = Morph::new();
        for _ in 0..120 {
            m.step(true, DT);
        }
        for _ in 0..120 {
            m.step(false, DT);
            frames.push(m.frame(Some(PILL), PANEL));
        }
        for fr in frames {
            assert!(fr.content_alpha >= fr.ground_alpha, "rows lead the glass: {fr:?}");
            if fr.ground_alpha > 0.0 {
                assert!(fr.content_alpha > 0.0, "an empty glass frame: {fr:?}");
            }
            assert!((fr.pill_alpha + fr.ground_alpha - 1.0).abs() < 1e-6, "a cross-dissolve");
        }
    }

    #[test]
    fn without_an_opener_rect_the_panel_fades_and_scales_about_its_centre() {
        let m = Morph::new();
        let fr = m.frame(None, PANEL);
        assert!((fr.rect.w - PANEL.w * FALLBACK_SCALE_FROM).abs() < 1e-3);
        assert!((fr.rect.cx() - PANEL.cx()).abs() < 1e-3 && (fr.rect.cy() - PANEL.cy()).abs() < 1e-3);
        assert_eq!(rect_at(None, PANEL, 1.0), PANEL);
        assert_eq!(rect_at(Some(PILL), PANEL, 0.0), PILL);
        assert_eq!(rect_at(Some(PILL), PANEL, 1.0), PANEL);
    }
}
