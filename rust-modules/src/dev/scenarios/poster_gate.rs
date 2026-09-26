//! Finite UI workloads that grade work deferred during motion and completed at rest.
//! Focus is seeded, then ordinary navigation keys drive measured motion. No media
//! activation or Plex viewing-history write is performed.
use crate::app::{bridge::Bridge, App};
use crate::screens::registry::{AppArg, HomeCmd, LibraryCmd};
use crate::ui::card_motion_metrics::{self as metrics, Stats};

// The mock warm window demanded more than 8 MiB (persistent 44/50 ready probes).
// 12 MiB fits that window while evicting old windows before 64 source slots recycle.
const PRESSURE_MIB: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode { Settle, Eviction, Dive }
impl Mode {
    fn word(self) -> &'static str { match self { Self::Settle => "settle", Self::Eviction => "eviction", Self::Dive => "dive" } }
}
#[derive(Clone, Copy)]
enum Target { Library(usize), GridSeed, Grid, Hero }
#[derive(Clone, Copy)]
struct Stage { name: &'static str, target: Target, seed: bool, handoff_snap: Option<f32>, sweep: Option<(usize, usize)>, min_ms: u32, max_ms: u32, needs_art: bool }
impl Stage {
    fn sweep_row(self, elapsed: u32) -> Option<usize> {
        self.sweep.map(|(from, to)| {
            let step = (elapsed / 125) as usize;
            if from <= to { (from + step).min(to) } else { from.saturating_sub(step).max(to) }
        })
    }
    fn completion(self, elapsed: u32, reached: bool, full: bool) -> Option<bool> {
        let ready = reached && (!self.needs_art || full);
        if elapsed < self.min_ms || !ready && elapsed < self.max_ms { None } else { Some(ready) }
    }
}
const fn hold(name: &'static str, target: Target, seed: bool) -> Stage {
    Stage { name, target, seed, handoff_snap: None, sweep: None, min_ms: 900, max_ms: 8000, needs_art: true }
}
const SETTLE: &[Stage] = &[
    hold("warm", Target::Library(0), true),
    Stage { name: "move", target: Target::Library(0), seed: false, handoff_snap: None, sweep: Some((0, 18)), min_ms: 2250, max_ms: 3250, needs_art: false },
    hold("settle", Target::Library(18), false),
];
const EVICTION: &[Stage] = &[
    hold("warm", Target::Library(0), true), hold("seed1", Target::Library(6), true), hold("seed2", Target::Library(12), true),
    Stage { name: "reverse", target: Target::Library(12), seed: false, handoff_snap: None, sweep: Some((12, 0)), min_ms: 1500, max_ms: 2500, needs_art: false },
    hold("settle", Target::Library(0), false),
];
const DIVE: &[Stage] = &[
    hold("warm", Target::GridSeed, true),
    Stage { name: "hero", target: Target::Hero, seed: false, handoff_snap: None, sweep: None, min_ms: 1600, max_ms: 6000, needs_art: false },
    // Hand off while the retained horizontal product is still moving fast, so
    // the natural deceleration and resulting requests/uploads belong to settle.
    Stage { name: "dive", target: Target::Grid, seed: false, handoff_snap: Some(0.9), sweep: None, min_ms: 0, max_ms: 4000, needs_art: false },
    hold("settle", Target::Grid, false),
];

#[derive(Clone, Copy)]
struct Witness {
    snap_begin: f32, snap_end: f32,
    first: f32, last: f32, min: f32, max: f32, max_velocity: f32,
}
impl Witness {
    fn new([snap, x, velocity]: [f32; 3]) -> Self {
        Self { snap_begin: snap, snap_end: snap, first: x, last: x, min: x, max: x, max_velocity: velocity.abs() }
    }
    fn observe(&mut self, [snap, x, velocity]: [f32; 3]) {
        self.snap_end = snap; self.last = x;
        self.min = self.min.min(x); self.max = self.max.max(x);
        self.max_velocity = self.max_velocity.max(velocity.abs());
    }
}

#[derive(Default)]
pub(crate) struct Scene {
    checked: bool,
    mode: Option<Mode>,
    stage: usize,
    started: Option<u32>,
    finished: bool,
    witness: Option<Witness>,
}
impl Scene {
    fn plan(&self) -> &'static [Stage] { match self.mode { Some(Mode::Settle) => SETTLE, Some(Mode::Eviction) => EVICTION, Some(Mode::Dive) => DIVE, None => &[] } }
    fn arm(&mut self) {
        if self.checked { return; }
        self.checked = true;
        self.mode = match crate::dev::read("postergate").as_deref().map(str::trim) {
            Some("settle") => Some(Mode::Settle), Some("eviction") => Some(Mode::Eviction), Some("dive") => Some(Mode::Dive), _ => None,
        };
        if let Some(mode) = self.mode {
            metrics::arm();
            // The measured pressure ceiling fits the warm window, but evicts old windows before
            // the source's 64 identities recycle. This is the real cache's byte-LRU
            // transition, not a test setter manufacturing P_EVICTED slots.
            if mode != Mode::Settle { crate::ui::tex::scene_residency_budget(PRESSURE_MIB << 20); }
            crate::log(&format!("poster-gate: kind={} phase=armed budget_mib={}", mode.word(), if mode == Mode::Settle { crate::ui::tex::TEX_RESIDENT_BYTES_MAX >> 20 } else { PRESSURE_MIB }));
        }
    }
    fn positioned(app: &App, target: Target) -> bool {
        match target {
            Target::Library(row) => Bridge::library_grid_position(&app.pages).is_some_and(|(r, _)| r == row),
            Target::GridSeed => app.bridge.home_grid_position(&app.pages) == Some((0, 11)),
            Target::Grid => app.bridge.home_grid_position(&app.pages).is_some_and(|(row, _)| row == 0),
            Target::Hero => !app.bridge.home_grid_focused(&app.pages) && app.bridge.home_snap_target(&app.pages) == 0.0,
        }
    }
    fn drive(app: &mut App, target: Target, seed: bool, now: u32) {
        if Self::positioned(app, target) { return; }
        if seed {
            match target {
                Target::Library(row) => Bridge::library_command(&mut app.pages, LibraryCmd::FocusGrid { row, col: 0 }),
                Target::GridSeed | Target::Grid => { app.bridge.home_command(HomeCmd::FocusGrid { row: 0, col: 11 }); }
                Target::Hero => { app.bridge.home_command(HomeCmd::Hero); }
            }
            return;
        }
        use crate::ui::machine::{Key, Tick};
        let key = match target {
            Target::Library(row) => Bridge::library_grid_position(&app.pages)
                .map(|(current, _)| if current < row { Key::Down } else { Key::Up }),
            Target::Grid | Target::GridSeed => (!app.bridge.home_grid_focused(&app.pages)).then_some(Key::Down),
            Target::Hero => app.bridge.home_grid_focused(&app.pages).then_some(Key::Up),
        };
        if let Some(key) = key {
            app.inputs.extend(crate::app::bridge::script_key(key, Tick { ms: now, dt_us: 0 }));
        }
    }
    fn reached(app: &App, target: Target) -> bool {
        Self::positioned(app, target) && match target {
            Target::Library(_) => true,
            Target::GridSeed | Target::Grid => app.bridge.home_motion_witness(&app.pages)
                .is_some_and(|[snap, _, v]| snap >= 0.99 && v.abs() <= 1.0),
            Target::Hero => app.bridge.home_motion_witness(&app.pages).is_some_and(|[snap, _, _]| snap <= 0.01),
        }
    }
    fn start_stage(&mut self, now: u32) -> u32 {
        *self.started.get_or_insert_with(|| {
            metrics::arm();
            now
        })
    }
    fn finish_stage(&mut self, now: u32) {
        self.stage += 1;
        self.started = None;
        self.witness = None;
        if self.stage == self.plan().len() {
            // The final report includes the last completed present. Later draws
            // are outside this finite scene; there is no successor to reset for.
            self.finished = true;
            crate::log(&format!("poster-gate: kind={} phase=done", self.mode.unwrap().word()));
        } else {
            // advance() reports before this iteration draws. Transfer ownership
            // now: waiting until the next tick would erase the intervening
            // requests, uploads and present from both phases' reports.
            metrics::arm();
            self.started = Some(now);
        }
    }
    fn advance(&mut self, app: &mut App, now: u32) {
        self.arm();
        let Some(mode) = self.mode else { return };
        if self.finished { return; }
        let route_ok = matches!((mode, app.route()), (Mode::Dive, AppArg::Home) | (Mode::Settle | Mode::Eviction, AppArg::Library));
        if !route_ok { return; }
        let stage = self.plan()[self.stage];
        let start = self.start_stage(now);
        let elapsed = now.wrapping_sub(start);
        if mode == Mode::Dive {
            if let Some(sample) = app.bridge.home_motion_witness(&app.pages) {
                self.witness.get_or_insert_with(|| Witness::new(sample)).observe(sample);
            }
        }
        let target = stage.sweep_row(elapsed).map_or(stage.target, Target::Library);
        // Seed only warm windows. All measured movement and the final natural
        // settle follow the same key path as a remote; reseating would jump scroll.
        Self::drive(app, target, stage.seed, now);
        let stats = metrics::snapshot();
        let reached = stage.handoff_snap.map_or_else(|| Self::reached(app, target), |threshold|
            Self::positioned(app, target) && app.bridge.home_motion_witness(&app.pages)
                .is_some_and(|[snap, _, _]| snap >= threshold));
        let Some(ready) = stage.completion(elapsed, reached, stats.full()) else { return };
        report(mode.word(), stage.name, elapsed, stats, ready, self.witness);
        if !ready {
            self.finished = true;
            crate::log(&format!("poster-gate: kind={} phase=failed reason=target-or-art", mode.word()));
            return;
        }
        self.finish_stage(now);
    }
}
fn report(kind: &str, phase: &str, ms: u32, s: Stats, complete: bool, witness: Option<Witness>) {
    let w = witness.unwrap_or(Witness::new([0.0; 3]));
    crate::log(&format!("poster-gate: kind={kind} phase={phase} ms={ms} frames={} draws={} ready={} moving={} moving_frames={} moving_ms={} unknown={} requested={} requested_moving={} refused_new={} refused_evicted={} refused_retry={} rearmed={} uploads={} lost={} last_draws={} last_ready={} complete={} snap_begin_milli={} snap_end_milli={} shelf_start_px={} shelf_end_px={} shelf_span_px={} shelf_v_milli={}",
        s.frames, s.draws, s.ready, s.moving, s.moving_frames, s.moving_last_ms.wrapping_sub(s.moving_first_ms), s.unknown,
        s.requested, s.requested_moving, s.refused_new, s.refused_evicted, s.refused_retry, s.rearmed, s.uploads, s.lost, s.last_draws, s.last_ready, complete as u8,
        (w.snap_begin * 1000.0).round() as i32, (w.snap_end * 1000.0).round() as i32,
        w.first.round() as i32, w.last.round() as i32, (w.max - w.min).round() as i32,
        (w.max_velocity * 1000.0).round() as i32));
}
pub(crate) fn tick(app: &mut App, now: u32) {
    let mut scene = std::mem::take(&mut app.scenarios.poster_gate);
    scene.advance(app, now);
    app.scenarios.poster_gate = scene;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn boundary_frame(now: u32) {
        metrics::frame();
        let _scope = crate::ui::card_motion::Scope::moving_for_test();
        for _ in 0..6 { metrics::draw(true); }
        // Deliberately inject forbidden fast work. A gate must retain it even
        // when this is the first draw after a phase reports its completion.
        metrics::request();
        metrics::refused(metrics::Refused::Evicted);
        metrics::rearmed();
        metrics::upload();
        metrics::evicted();
        metrics::presented(now);
    }
    #[test]
    fn every_phase_owns_its_handoff_draw_before_the_next_tick() {
        for mode in [Mode::Settle, Mode::Eviction, Mode::Dive] {
            let mut scene = Scene { checked: true, mode: Some(mode), ..Scene::default() };
            for stage in 0..scene.plan().len() - 1 {
                scene.stage = stage;
                scene.started = None;
                scene.start_stage(1000);
                boundary_frame(1016);
                let previous = metrics::snapshot();
                assert_eq!(previous.frames, 1);
                scene.finish_stage(1032);
                // advance() runs before draw/upload/swap in app::run. This draw
                // must be owned immediately; the next tick is already too late.
                boundary_frame(1032);
                let next_start = scene.start_stage(1048);
                let next = metrics::snapshot();
                assert_eq!(next.requested_moving, 1, "{mode:?} stage {stage}: lost fast admission at handoff");
                assert_eq!((next.frames, next.moving_frames, next.uploads), (1, 1, 1));
                assert_eq!((next.refused_evicted, next.rearmed, next.lost), (1, 1, 1));
                assert_eq!((next.draws, next.ready, next.last_draws, next.last_ready), (6, 6, 6, 6));
                assert!(next.full());
                assert_eq!(next_start, 1032);
            }
        }
    }
    #[test]
    fn final_phase_keeps_its_last_present_and_does_not_arm_another_phase() {
        let mut scene = Scene { checked: true, mode: Some(Mode::Dive), ..Scene::default() };
        scene.stage = scene.plan().len() - 1;
        scene.start_stage(1000);
        boundary_frame(1016);
        scene.finish_stage(1032);
        assert!(scene.finished);
        assert!(scene.plan().get(scene.stage).is_none());
        assert_eq!(scene.started, None);
        let final_stats = metrics::snapshot();
        assert_eq!((final_stats.frames, final_stats.requested_moving, final_stats.uploads), (1, 1, 1));
        assert!(final_stats.full());
    }
    #[test]
    fn sweeps_stop_at_their_own_end_and_reverse_over_seeded_rows() {
        assert_eq!(SETTLE[1].sweep_row(0), Some(0));
        assert_eq!(SETTLE[1].sweep_row(2250), Some(18));
        assert_eq!(SETTLE[1].sweep_row(9000), Some(18));
        assert_eq!(EVICTION[3].sweep_row(0), Some(12));
        assert_eq!(EVICTION[3].sweep_row(750), Some(6));
        assert_eq!(EVICTION[3].sweep_row(1500), Some(0));
        // The endpoint command lands on a later dispatch turn; do not timeout on
        // the very iteration that first queued it.
        assert_eq!(SETTLE[1].completion(2250, false, false), None);
        assert_eq!(SETTLE[1].completion(2266, true, false), Some(true));
    }
    #[test]
    fn settling_requires_a_full_new_draw_and_has_a_finite_failure_deadline() {
        let s = SETTLE[2];
        assert_eq!(s.completion(899, true, true), None);
        assert_eq!(s.completion(900, true, false), None);
        assert_eq!(s.completion(900, false, true), None);
        assert_eq!(s.completion(900, true, true), Some(true));
        assert_eq!(s.completion(8000, true, false), Some(false));
    }
}
