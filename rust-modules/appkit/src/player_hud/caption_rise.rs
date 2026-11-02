//! **How far the captions have risen to clear an open section** (`docs/hud-plugins.md`, "Captions: the
//! band a HUD occupies"), as the motion the HUD owns: [`CaptionRise`] is a field of
//! [`HudMotion`](super::motion::HudMotion), stepped where the sections' appear springs are, and its
//! presented value reaches the caption readers inside the [`HudBand`] they already take
//! ([`HudBand::with_rise`]), so no writer into the media layer exists.
//!
//! **The rule.** A caption's bottom limit is `min(P, S - 12)` while any section is open: P is the HUD-up
//! limit of the viewer's Position and S the resting top of everything the HUD has open
//! ([`HudGeometry::open_top`](super::geometry::HudGeometry::open_top)). It is kept as a RISE, `P - limit`
//! per Position ([`HudBand::rise_to_clear`]), so the three Positions are three springs on one shared
//! stiffness: a single spring on S would leave a Middle or High caption waiting for the section to reach
//! it and then creeping, where each Position's own limit moves from the first appear frame.
//!
//! **Rise only.** While sections stay open the target only grows: the block top held is the lowest S of
//! every state visited since the first section opened ([`CaptionRise::follow`]), so a second section or a
//! taller one lifts the limit, a shorter one or the close of one of two holds it, and the last close sends it
//! home. One place, driven by the same open-set the appear springs follow.
//!
//! **Motion.** `plx_machine::motion::spring` on `plx_ui::popover::K_APPEAR` (300), the critically damped
//! closed form the sections' own appear uses, so it cannot pass either end. It takes its first step on the
//! frame the section's appear takes its first ([`CaptionRise::step`]'s `hold`): held while any section's
//! spring is still held at appear 0 and on the frame a section was dismissed, on open and on close alike.
use plx_machine::machine::{PresentHandle, Tick};
use plx_machine::motion;
use plx_media::player::text_style::HudBand;
use plx_ui::popover::K_APPEAR;

/// The rise per Position (Low, Middle, High) and the spring that moves it.
#[derive(Clone, Copy, Debug)]
pub struct CaptionRise {
    rise: [f32; 3],
    vel: [f32; 3],
    target: [f32; 3],
    /// The lowest block top of every state visited while a section has been open, `None` with none open.
    held_top: Option<f32>,
    /// The `ceil_y` the rest value was built on (NaN until the first call).
    ceil: f32,
}

impl Default for CaptionRise {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptionRise {
    pub const fn new() -> Self {
        Self { rise: [0.0; 3], vel: [0.0; 3], target: [0.0; 3], held_top: None, ceil: f32::NAN }
    }

    /// **Aim the springs** at the sections that are open now. `open` is `HudState::sections` as bits (`0`
    /// when none is), `top` the resting top of the block the state for `open` lays out (`None` when it has no
    /// section, or the package has no state for the frame), `band` the HUD's own.
    ///
    /// Nothing open: the block held is forgotten and the target is home. Otherwise the block held can only get
    /// taller, so the target can only grow. A band that is not the one the springs were built for (a package
    /// swapped in) puts them back at rest, once, with no section open.
    pub fn follow(&mut self, open: u8, band: HudBand, top: Option<f32>) {
        if open == 0 {
            self.held_top = None;
        } else if let Some(top) = top {
            self.held_top = Some(self.held_top.map_or(top, |held| held.min(top)));
        }
        if self.ceil != band.ceil_y {
            self.ceil = band.ceil_y;
            if self.held_top.is_none() {
                *self = Self { ceil: band.ceil_y, ..Self::new() };
            }
        }
        self.target = self.held_top.map_or([0.0; 3], |held| band.rise_to_clear(held));
    }

    /// **Step the springs**, once per frame from the same call that steps the sections' appear. `hold` is
    /// true on the frames the appear springs take no step (a section held at appear 0, or dismissed on this
    /// very frame), so the first frame the limit moves is the first frame the section does.
    pub fn step(&mut self, t: Tick, present: &mut PresentHandle<'_>, hold: bool) {
        if hold {
            return;
        }
        for i in 0..3 {
            motion::spring(&mut self.rise[i], &mut self.vel[i], self.target[i], K_APPEAR, t, present);
            if !motion::moving(self.rise[i], self.target[i], self.vel[i], t.dt()) {
                let changed = self.rise[i] != self.target[i] || self.vel[i] != 0.0;
                self.rise[i] = self.target[i];
                self.vel[i] = 0.0;
                if changed {
                    present.note(plx_machine::present::PresentEvent::Motion);
                }
            }
            self.rise[i] = self.rise[i].max(0.0);
        }
    }

    /// The rise the captions are drawn at this frame, per Position.
    pub fn presented(&self) -> [f32; 3] {
        self.rise
    }

    /// Where the springs are headed (a test reads it; nothing in a draw does).
    #[cfg(any(test, feature = "test-support"))]
    pub fn target(&self) -> [f32; 3] {
        self.target
    }

    /// Back at rest at once, no section held: the sections were cut (`HudMotion::cut_sections`).
    pub fn cut(&mut self) {
        *self = Self { ceil: self.ceil, ..Self::new() };
    }
}
