//! `GlassPlan` — the frame plan's half of the glass chain (spec §8.3, §15.2).
//!
//! Everything the renderer's ONE blurred-snapshot chain schedules with used to be a `static mut`:
//! the shared refresh cadence in `ui/widgets.rs`, and the dev load dial's whole live state in
//! `ui/glassload.rs`. `ci/allow/statics.txt` carried the eight glassload entries with the reason
//! "owned by the frame plan from phase 11", which is this type. They are fields now, `App` owns
//! one of these, and the draw/prepare path reaches it by `&mut` — so the allowlist shrinks rather
//! than being re-justified.
//!
//! **`App` owns it, not the `Dispatcher`'s rig.** Every live reader is either `app/run.rs`'s own
//! draw phase (the dial's prepare and its two draws, the tile band's cadence) or something
//! `app/run.rs` already calls with `&mut App` in hand (`Bridge::prepare_home_chrome`,
//! `Dispatcher::prepare_present`); the rig reaches none of them without a second `&mut` through
//! `Rig::draw_chrome`, which nothing else needs.
//!
//! **Zero behaviour change is the whole point.** `DEFAULT_DYNAMIC_PERIOD` is still 1 and the
//! cadence arithmetic is byte-for-byte the code that was in the statics' readers — the glass
//! source pass PACES the GPU on this part (glass every present measured 46 fps against 35 with
//! none and 36 at one-in-eight; `docs/backdrop-blur-profiling.md`), so a "simplification" of the
//! cadence here is a frame-rate change wearing a refactor's clothes.
//!
//! Main render thread only, like the snapshot chain it schedules. What instruments read from
//! elsewhere — the dial's live step index, and whether it is armed at all — is a PUBLISHED
//! snapshot the dial writes on change (spec §2.3), not a borrow: `ui/profile.rs` tags every HWCNT
//! phase record with the step and has no `&App` to ask.

use crate::ui::glassload::Dial;
use crate::ui::widgets::{DynamicClock, Glass, GlassState, TabBand, TabLabels};

/// The frame's glass schedule: one shared cadence clock, the surfaces whose lifetime belongs to no
/// screen, and the dev load dial.
pub(crate) struct GlassPlan {
    /// The ONE recurring cadence every dynamic backdrop shares. Two owners opened on different
    /// presents with separate phases would refresh 2/3 or even every frame between them — the
    /// clock is global because the snapshot chain under it is.
    dynamic: DynamicClock,
    /// Persistent state for the shared top tab track: visible lifetime, adaptive density and this
    /// frame's material. The strip borrows it during paint; the Bridge never owns a second copy.
    tab: TabBand,
    /// The tile bands' visible lifetime (`/tmp/plxnative-tileglass`). ONE for every band in a
    /// frame: there is one blur cache and every glass surface converges on one grab, so per-tile
    /// state would buy nothing and would let two tiles disagree about whether this present's
    /// snapshot is stale.
    tile: GlassState,
    /// The dev backdrop-glass load dial and the blurred-transition prototype beside it
    /// (`/tmp/plxnative-glassload`, `/tmp/plxnative-navblur`).
    dial: Dial,
}

impl GlassPlan {
    pub(crate) fn new() -> Self {
        let plan = Self {
            dynamic: DynamicClock::new(),
            tab: TabBand::new(),
            tile: GlassState::new(),
            dial: Dial::new(),
        };
        // The dial's step and armed bit are read by instruments that hold no borrow of this type,
        // so they live in a published snapshot (spec §2.3); a fresh plan owns it from here.
        plan.dial.publish();
        plan
    }

    /// The shared cadence clock, for an owner whose `GlassState` lives on the surface itself
    /// (every popover, the person page's bio panel, the tab track).
    pub(crate) fn clock(&mut self) -> &mut DynamicClock {
        &mut self.dynamic
    }

    /// Resolve one REFRESHING backdrop's cadence before its host page draws.
    pub(crate) fn prepare_dynamic(&mut self, state: &mut GlassState, underlay_changed: bool) {
        Glass::DYNAMIC_BACKDROP.prepare_on(&mut self.dynamic, state, underlay_changed);
    }

    pub(crate) fn prepare_tab_band(&mut self, labels: TabLabels<'_>) {
        self.tab.prepare(labels, &mut self.dynamic);
    }

    pub(crate) fn step_tab_band(&mut self, dt: f32) {
        self.tab.step(dt);
    }

    pub(crate) fn tab_band_mut(&mut self) -> &mut TabBand {
        &mut self.tab
    }

    pub(crate) fn tab_face(&self) -> Option<crate::gfx::GlassFace> {
        self.tab.face()
    }

    #[cfg(test)]
    pub(crate) fn seed_tab_density_for_test(&mut self, value: f32) {
        self.tab.seed_density(value);
    }

    #[cfg(test)]
    pub(crate) fn tab_density_for_test(&self) -> f32 {
        self.tab.density()
    }

    #[cfg(test)]
    pub(crate) fn set_tab_face_for_test(&mut self, face: crate::gfx::GlassFace) {
        self.tab.set_face(face);
    }

    /// Resolve the tile bands' glass cadence BEFORE the page they sit on draws — `Glass::prepare`'s
    /// contract, exactly as the tab track and the person page's bio panel do. A no-op unless the
    /// experiment is armed.
    pub(crate) fn prepare_tile_band(&mut self) {
        if !crate::ui::widgets::tile_glass_armed() || crate::gfx::blur_source_pass() {
            return;
        }
        Glass::DYNAMIC_BACKDROP.prepare_on(
            &mut self.dynamic,
            &mut self.tile,
            crate::ui::idle::present_moving() || crate::ui::idle::present_dirty(),
        );
    }

    /// Arm the load dial from `/tmp/plxnative-glassload`'s content.
    pub(crate) fn configure_dial(&mut self, spec: &str) {
        self.dial.configure(spec);
    }

    /// Arm the blurred-transition prototype from `/tmp/plxnative-navblur`'s content.
    pub(crate) fn configure_navblur(&mut self, spec: &str) {
        self.dial.configure_navblur(spec);
    }

    /// Does the live step want the REAL Account popover open?
    pub(crate) fn wants_account(&self) -> bool {
        self.dial.wants_account()
    }

    /// Advance the dial one presented frame, BEFORE the page draws.
    pub(crate) fn prepare_dial(&mut self, now_ms: u32) {
        self.dial.prepare(now_ms);
    }

    /// Draw the blurred route transition, if one is in flight. Returns whether it drew.
    pub(crate) fn draw_nav_blur(&mut self) -> bool {
        self.dial.draw_nav_blur()
    }

    /// Draw the dial's glass surfaces over whatever screen is up.
    pub(crate) fn draw_dial(&mut self) {
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
    //! changed no cadence.
    use super::*;

    /// **Two plans do not share a step.** The whole point of the move: `configure_dial` on one
    /// instance leaves the other disarmed, which is exactly what a process-wide `SWEEP`/`STEP`
    /// pair made impossible to assert.
    #[test]
    fn the_dial_travels_with_the_plan_it_was_armed_on() {
        let _g = crate::testlock::serial(); // the published step snapshot is process-wide
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
        // (spec §2.3): `ui/profile.rs` reads it from inside an arbitrary draw closure.
        assert_eq!(crate::ui::glassload::step_index(), 0);
        assert!(crate::ui::glassload::armed());
        // Leave the process-wide publication as a fresh plan would: this is a measuring
        // instrument's dial, and a test that armed it for everyone else would be a leg nobody ran.
        drop(GlassPlan::new());
        assert_eq!(crate::ui::glassload::step_index(), -1);
        assert!(!crate::ui::glassload::armed());
    }

    /// **The move changed no cadence**, which recon risk 3 makes the load-bearing claim: the glass
    /// source pass PACES the GPU (46 fps refreshing every present against 36 at one-in-eight), so
    /// a plan that quietly divided the shipped period would read as a refactor and land as a
    /// frame-rate change.
    #[test]
    fn the_shipped_cadence_is_still_every_changed_present() {
        assert_eq!(
            crate::ui::widgets::dynamic_period(),
            1,
            "DEFAULT_DYNAMIC_PERIOD is 1 and the plan reads it rather than holding its own"
        );
    }

    /// The tab band is frame-plan state. Mutating one plan's solve/material must leave another
    /// plan at its fresh values; a process static or Bridge-owned field cannot satisfy this.
    #[test]
    fn two_glass_plans_own_independent_tab_bands() {
        let _guard = crate::testlock::serial();
        let mut a = GlassPlan::new();
        let b = GlassPlan::new();
        a.seed_tab_density_for_test(0.73);
        a.set_tab_face_for_test(crate::gfx::GlassFace {
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
