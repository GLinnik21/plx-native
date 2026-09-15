//! A raw dwell accumulator, shared so the next caller cannot reintroduce the two bugs that already
//! shipped against `season_settle`.
//!
//! The idle gate parks the frame loop unless the screen reports [`PresentEvent::Motion`] on every
//! accumulating frame. A timer that only adds `dt` never completes on an otherwise-idle page. The
//! note is inside [`accumulate`] so a caller cannot take the increment and forget the report.
//!
//! It stays a raw `f32` add. `motion::Ramp` computes the same quantity from an absolute tick, and
//! that different float sequence diverges a hashed dwell (`SHAPE`'s `season_settle:f32`) against
//! the committed replay fixtures. Do not "upgrade" this to a ramp.

use crate::ui::present::PresentEvent;

/// `elapsed = elapsed + dt`, then note motion. The assignment form matches the hashed
/// `season_settle` sequence; a `+=` that a later pass rewrites into a ramp is the bug.
pub(crate) fn accumulate(elapsed: &mut f32, dt: f32, note: &mut dyn FnMut(PresentEvent)) {
    *elapsed = *elapsed + dt;
    note(PresentEvent::Motion);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dwell_tick_reports_motion_and_stays_a_raw_add() {
        let mut elapsed = 0.05f32;
        let mut noted = 0u32;
        accumulate(&mut elapsed, 0.10, &mut |_| noted += 1);
        assert!((elapsed - 0.15).abs() < 1e-6);
        assert_eq!(noted, 1);
    }
}
