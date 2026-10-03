//! `TextLift` — the ONE animated focus treatment for a block of prose that is a focus stop but not
//! a card: the About synopsis and Languages columns, a person's bio, a collection's blurb, an
//! episode's title+summary block. While focused the block grows to [`TEXT_LIFT_SCALE`] over a
//! translucent plate with a soft shadow, the mockup's `textLift` component.
//!
//! **Scoped, so it cannot leak.** [`draw`] hands its `content` closure a painter that zooms every
//! primitive about the block's origin ([`Painter::zoomed`]); that painter exists only inside the
//! closure, so a sibling drawn afterwards through the caller's own `p` is never scaled. The zoom is
//! a GPU-side quad resize of the already-cached glyph textures — no re-rasterizing, no new
//! glyph-cache entries — and `Label`/`TextView` need no changes.
//!
//! The mockup's overshooting cubic-bezier is deliberately not reproduced: the block steps the same
//! critically damped spring at `K_SCALE` as every other pop here (the episode still pops that way a
//! row above its text block), and the two must agree on one frame.

use super::{theme, Painter, Rect, Spring, Zoom};

/// The block's pop at full focus — the mockup's flat `scale(1.05)`. Distinct from
/// [`theme::EP_CARD_FOCUS_SCALE`], which is the episode STILL's own pop.
pub(crate) const TEXT_LIFT_SCALE: f32 = 1.05;

/// Grow from the centre of the block.
pub(crate) const CENTRE: (f32, f32) = (0.5, 0.5);
/// Grow downward from a fixed top edge — for a block that sits under something that does not move.
pub(crate) const TOP_CENTRE: (f32, f32) = (0.5, 0.0);

/// One block's animated focus state: a single spring in the normalized 0..1 focus factor, from
/// which [`scale`](Self::scale) is derived.
///
/// The spring runs in 0..1, not over the 1.0..1.05 scale band, because `plx_machine::idle` calls a spring at
/// rest by a threshold relative to its own magnitude: over the narrow band that threshold is a large
/// fraction of the span, leaving a visible plate behind. On top of that, [`step`](Self::step) snaps
/// a settled spring onto its target, so at rest the factor is exactly 0 and the scale exactly 1.0
/// and every `> 0.0` / `!= 1.0` guard downstream skips its work.
#[derive(Clone, Copy, Default)]
pub(crate) struct TextLift(Spring);

impl TextLift {
    pub(crate) const fn new() -> Self {
        Self(Spring::at(0.0))
    }

    /// Advance toward `focused`'s target for this frame.
    pub(crate) fn step(&mut self, focused: bool, dt: f32) {
        let target = if focused { 1.0 } else { 0.0 };
        self.0.step(target, crate::ui::consts::K_SCALE, dt);
        if plx_machine::idle::settled(self.0.pos, target, self.0.vel) {
            // `jump` reports to idle (it is a change no integrator saw), so the exact resting frame
            // is presented.
            self.0.jump(target);
        }
    }

    /// Drop straight to rest with no motion in between.
    pub(crate) fn reset(&mut self) {
        self.0.jump(0.0);
    }

    /// This frame's scale: exactly 1.0 at rest, [`TEXT_LIFT_SCALE`] at full focus.
    pub(crate) fn scale(&self) -> f32 {
        1.0 + self.0.pos * (TEXT_LIFT_SCALE - 1.0)
    }

    /// 0 at rest, 1 at full focus — drives the shadow and, by default, the plate.
    pub(crate) fn factor(&self) -> f32 {
        self.0.pos.clamp(0.0, 1.0)
    }
}

/// The plate's colour at plate factor `f`.
pub(crate) fn plate_colour(f: f32) -> [f32; 4] {
    let mut plate = theme::OVERLAY_FOCUS_SOFT;
    plate[3] *= f;
    plate
}

/// `r` as it is drawn while lifted by `scale` about `origin` — for focus geometry that must follow
/// the drawn block.
pub(crate) fn lifted(r: Rect, origin: (f32, f32), scale: f32) -> Rect {
    Zoom::about(r, origin, scale).map(r)
}

/// Draw a text-lift block at its RESTING rect `r` (hit rects and focus geometry key off the rest
/// rect, like a card's) and run `content` on a painter zoomed to match, so what it draws grows with
/// the plate. `plate_factor` is separate from the lift's own factor so the episode filmstrip can
/// show the plate for either of a cell's focus stops and reserve the scale and shadow for the text
/// stop.
///
/// At rest nothing is drawn but `content`, on the plain painter.
pub(crate) fn draw(
    p: Painter,
    r: Rect,
    radius: f32,
    lift: &TextLift,
    plate_factor: f32,
    origin: (f32, f32),
    content: impl FnOnce(Painter),
) {
    let scale = lift.scale();
    let factor = lift.factor();
    if scale == 1.0 && factor <= 0.0 && plate_factor <= 0.0 {
        return content(p);
    }
    let z = p.zoomed(Zoom::about(r, origin, scale));
    if factor > 0.0 {
        z.focus_shadow_outside(r, radius, factor);
    }
    if plate_factor > 0.0 {
        z.rrect(r, radius, radius, plate_colour(plate_factor));
    }
    content(z);
}

/// [`draw`] for the common case: one [`TextLift`] drives the scale, plate and shadow together.
pub(crate) fn draw_focused(
    p: Painter,
    r: Rect,
    radius: f32,
    lift: &TextLift,
    origin: (f32, f32),
    content: impl FnOnce(Painter),
) {
    draw(p, r, radius, lift, lift.factor(), origin, content);
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_base::testlock;
    use crate::ui::draw_census;

    const DT: f32 = 1.0 / 60.0;
    const R: Rect = Rect { x: 100.0, y: 100.0, w: 200.0, h: 100.0 };

    fn settle(lift: &mut TextLift, focused: bool) {
        for _ in 0..240 {
            lift.step(focused, DT);
        }
    }

    #[test]
    fn focus_settles_exactly_at_full_lift_and_exactly_back_at_rest() {
        let _g = testlock::serial();
        let mut lift = TextLift::new();
        assert_eq!((lift.scale(), lift.factor()), (1.0, 0.0));

        settle(&mut lift, true);
        assert_eq!((lift.scale(), lift.factor()), (TEXT_LIFT_SCALE, 1.0));

        settle(&mut lift, false);
        assert_eq!((lift.scale(), lift.factor()), (1.0, 0.0), "a settled spring snaps to exact rest");
    }

    #[test]
    fn reset_jumps_with_no_motion_in_between() {
        let _g = testlock::serial();
        let mut lift = TextLift::new();
        lift.step(true, DT);
        assert!(lift.scale() > 1.0);
        lift.reset();
        assert_eq!((lift.scale(), lift.factor()), (1.0, 0.0));
    }

    /// The census of one block draw: its plate, its shadow, and one text run inside.
    fn census_of(lift: &TextLift) -> Vec<(u64, Rect)> {
        let s = std::ffi::CString::new("inside").unwrap();
        draw_census::capture(|| {
            draw_focused(Painter::recording(), R, 12.0, lift, CENTRE, |p| {
                p.text(s.as_ptr(), 120.0, 120.0, theme::size::CAPTION, theme::TEXT_PRIMARY, 0, 0);
            });
        })
    }

    #[test]
    fn a_resting_block_draws_only_its_content() {
        let _g = testlock::serial();
        let mut lift = TextLift::new();
        let at_rest = census_of(&lift);
        assert_eq!(at_rest.len(), 1, "content only: {at_rest:?}");
        assert_eq!(at_rest[0].1.x, 120.0, "and unscaled");

        // The ghost-plate case: focus, then leave and settle — the same census as never focused.
        settle(&mut lift, true);
        assert!(census_of(&lift).len() > 1, "a focused block draws its plate and shadow");
        settle(&mut lift, false);
        assert_eq!(census_of(&lift), at_rest, "no plate or shadow survives the settle");
    }

    #[test]
    fn a_focused_block_grows_its_plate_and_content_about_the_origin() {
        let _g = testlock::serial();
        let mut lift = TextLift::new();
        settle(&mut lift, true);
        let census = census_of(&lift);
        let plate = census.iter().find(|(tag, _)| *tag == 2).expect("a plate").1;
        let text = census.iter().find(|(tag, _)| *tag == 100).expect("the content").1;

        assert_eq!(plate, lifted(R, CENTRE, TEXT_LIFT_SCALE));
        assert!((plate.cx() - R.cx()).abs() < 1e-3 && (plate.cy() - R.cy()).abs() < 1e-3);
        assert!(plate.w > R.w && plate.h > R.h);
        assert!(text.x < 120.0, "left of the centre, so it moves left with the block: {text:?}");
    }

    #[test]
    fn the_top_centre_origin_keeps_the_top_edge_fixed() {
        let grown = lifted(R, TOP_CENTRE, TEXT_LIFT_SCALE);
        assert!((grown.y - R.y).abs() < 1e-3);
        assert!((grown.cx() - R.cx()).abs() < 1e-3);
        assert!(grown.y + grown.h > R.y + R.h, "grows downward");
    }

    #[test]
    fn the_shadow_and_plate_ramp_from_zero() {
        let alpha = |f| crate::ui::text_shadow_params(R.h, f).2;
        assert_eq!(alpha(0.0), 0.0);
        assert_eq!(plate_colour(0.0)[3], 0.0);
        assert!(alpha(0.5) > 0.0 && alpha(0.5) < alpha(1.0));
    }
}
