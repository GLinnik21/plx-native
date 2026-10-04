//! Application-owned live backdrop schedule and chrome material state.
//! Every dynamic surface enters `backdrop` through the painter; the dispatcher supplies z bands.
//! The layer walk, geometric occlusion and region validity live in `backdrop`, with no route tests.

use crate::glassload::Dial;
use crate::widgets::TabBand;
use super::backdrop::{Sources, Z};
use std::{rc::Rc, cell::RefCell};

/// The frame's glass schedule, the surfaces whose lifetime belongs to no
/// screen, and the dev load dial.
pub struct GlassPlan {
    pub sources: Rc<RefCell<Sources>>,
    /// Persistent state for the shared top tab track: visible lifetime, adaptive density and this
    /// frame's material. The strip borrows it during paint; the Bridge never owns a second copy.
    tab: TabBand,
    /// The dev backdrop-glass load dial and the blurred-transition prototype beside it
    /// (`/tmp/plxnative-glassload`, `/tmp/plxnative-navblur`).
    dial: Dial,
}

impl GlassPlan {
    pub fn new() -> Self {
        let plan = Self {
            sources: Rc::new(RefCell::new(Sources::default())),
            tab: TabBand::new(),
            dial: Dial::new(),
        };
        // The dial's step and armed bit are read by instruments that hold no borrow of this type,
        // so they live in a published snapshot (spec §2.3); a fresh plan owns it from here.
        plan.dial.publish();
        plan
    }

    pub fn walk(&self, ceiling: Z) -> super::backdrop::Scope {
        super::backdrop::enter(self.sources.clone(), ceiling)
    }

    pub fn step_tab_band(&mut self, dt: f32) {
        self.tab.step(dt);
    }

    pub fn tab_band_mut(&mut self) -> &mut TabBand {
        &mut self.tab
    }

    pub fn tab_face(&self) -> Option<plx_gfx::gfx::GlassFace> {
        self.tab.face()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_tab_density_for_test(&mut self, value: f32) {
        self.tab.seed_density(value);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn tab_density_for_test(&self) -> f32 {
        self.tab.density()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_tab_face_for_test(&mut self, face: plx_gfx::gfx::GlassFace) {
        self.tab.set_face(face);
    }

    /// Arm the load dial from `/tmp/plxnative-glassload`'s content.
    pub fn configure_dial(&mut self, spec: &str) {
        self.dial.configure(spec);
    }

    /// Arm the blurred-transition prototype from `/tmp/plxnative-navblur`'s content.
    pub fn configure_navblur(&mut self, spec: &str) {
        self.dial.configure_navblur(spec);
    }

    /// Does the live step want the REAL Account popover open?
    pub fn wants_account(&self) -> bool {
        self.dial.wants_account()
    }

    /// Advance the dial one presented frame, BEFORE the page draws.
    pub fn prepare_dial(&mut self, now_ms: u32) {
        self.dial.prepare(now_ms);
    }

    /// Draw the blurred route transition, if one is in flight. Returns whether it drew.
    pub fn draw_nav_blur(&mut self) -> bool {
        self.dial.draw_nav_blur()
    }

    /// Draw the dial's glass surfaces over whatever screen is up.
    pub fn draw_dial(&mut self) {
        self.dial.draw();
    }
}

impl Default for GlassPlan {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! Two properties, and both are about OWNERSHIP rather than about glass: that the dial's live
    //! state travels with the instance it was armed on (eight `static mut`s could not have this
    //! test at all — a second plan would have read the first one's step), and that moving it
    //! preserves the every-changed-present rule.
    use super::*;

    /// **Two plans do not share a step.** The whole point of the move: `configure_dial` on one
    /// instance leaves the other disarmed, which is exactly what a process-wide `SWEEP`/`STEP`
    /// pair made impossible to assert.
    #[test]
    fn the_dial_travels_with_the_plan_it_was_armed_on() {
        let _g = plx_base::testlock::serial(); // the published step snapshot is process-wide
        let mut armed = GlassPlan::new();
        let untouched = GlassPlan::new();
        assert!(!armed.dial.armed(), "a fresh plan is disarmed");
        armed.configure_dial("hold=6;1x608x396@3,2x400x300@1");
        assert!(armed.dial.armed(), "a good spec arms the plan it was given");
        assert!(
            !untouched.dial.armed(),
            "…and only that one — a second plan is still disarmed"
        );
        // The instruments' half is a PUBLISHED snapshot of the last plan to change, not a borrow
        // (spec §2.3): `gfx/profile.rs` reads it from inside an arbitrary draw closure.
        assert_eq!(crate::glassload::step_index(), 0);
        assert!(crate::glassload::armed());
        // Leave the process-wide publication as a fresh plan would: this is a measuring
        // instrument's dial, and a test that armed it for everyone else would be a leg nobody ran.
        drop(GlassPlan::new());
        assert_eq!(crate::glassload::step_index(), -1);
        assert!(!crate::glassload::armed());
    }

    /// **The move changed no cadence**, which recon risk 3 makes the load-bearing claim: the glass
    /// source pass PACES the GPU (46 fps refreshing every present against 36 at one-in-eight), so
    /// a plan that quietly divided the shipped period would read as a refactor and land as a
    /// frame-rate change.
    #[test]
    fn the_shipped_cadence_is_still_every_changed_present() {
        use super::super::backdrop::{decide, Damage, Request, Z, canvas};
        for _ in 0..16 {
            assert!(decide(Request {z:Z::CHROME,rect:canvas(),valid:true}, &[],
                &[Damage {z:Z::PAGE,rect:canvas()}]).refresh);
        }
    }

    /// The tab band is frame-plan state. Mutating one plan's solve/material must leave another
    /// plan at its fresh values; a process static or Bridge-owned field cannot satisfy this.
    #[test]
    fn two_glass_plans_own_independent_tab_bands() {
        let _guard = plx_base::testlock::serial();
        let mut a = GlassPlan::new();
        let b = GlassPlan::new();
        a.seed_tab_density_for_test(0.73);
        a.set_tab_face_for_test(plx_gfx::gfx::GlassFace {
            scrim_top: [0.1, 0.2, 0.3, 0.4],
            scrim_bot: [0.5, 0.6, 0.7, 0.8],
            rim: [0.0; 4], rim_lit: [0.0; 4], rim_w: 1.0,
        });
        assert!((a.tab_density_for_test() - 0.73).abs() < 1e-6);
        assert_eq!(a.tab_face().unwrap().scrim_top, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(b.tab_density_for_test(), 0.0);
        assert!(b.tab_face().is_none());
    }
}
