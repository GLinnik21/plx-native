//! Finite UI workloads that grade work deferred during motion and completed at rest.
//! Focus is seeded, then ordinary navigation keys drive measured motion. No media
//! activation or Plex viewing-history write is performed.
use crate::app::{bridge::Bridge, App};
use crate::screens::registry::{AppArg, HomeCmd, LibraryCmd};
use crate::ui::card_motion_metrics::{self as metrics, Stats};

// The mock warm window demanded more than 8 MiB (persistent 44/50 ready probes).
// 12 MiB fits that window while evicting old windows before 64 source slots recycle.
const PRESSURE_MIB: usize = 12;

/// The eviction scene's ceiling once its warm window has been measured: room for one window and a
/// half, never more than [`PRESSURE_MIB`]. A deeper grid window shows more cards than the head one
/// under its heading band — measured on the dev set, a 6.1 MiB head window against an 8.6 MiB
/// mid-grid one (1.42x) — so one and a half still holds any single window, while the seeds' newer
/// windows push the head's art out. The fixed 12 MiB alone was sized
/// for seeds six rows apart; on a shallow catalog, whose seeds are closer and bring fewer new bytes,
/// it holds nearly the whole grid, and the reversal crosses no evicted art until focus has already
/// landed. Authored bytes in and out, as [`crate::ui::tex::scene_residency_budget`] takes.
fn pressure_ceiling(window_bytes: usize) -> usize {
    (window_bytes + window_bytes / 2).min(PRESSURE_MIB << 20)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode { Settle, Eviction, Dive }
impl Mode {
    fn word(self) -> &'static str { match self { Self::Settle => "settle", Self::Eviction => "eviction", Self::Dive => "dive" } }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target { Library(usize), GridSeed(usize), Grid, Hero }
#[derive(Clone, Copy, Debug, PartialEq)]
struct Stage { name: &'static str, target: Target, seed: bool, handoff_snap: Option<f32>, sweep: Option<(usize, usize)>, min_ms: u32, max_ms: u32, needs_art: bool }
impl Stage {
    fn sweep_row(self, elapsed: u32) -> Option<usize> {
        self.sweep.map(|(from, to)| {
            let step = (elapsed / ROW_MS) as usize;
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
/// A fast sweep of whole rows at one remote-repeat per row, then a grace for its last command.
const fn sweep(name: &'static str, from: usize, to: usize) -> Stage {
    let rows = if from <= to { to - from } else { from - to } as u32;
    Stage { name, target: Target::Library(from), seed: false, handoff_snap: None, sweep: Some((from, to)),
        min_ms: rows * ROW_MS, max_ms: rows * ROW_MS + SWEEP_GRACE_MS, needs_art: false }
}

// The scenes are sized to the catalog they are given, never to one a test server happens to have.
// These are the CAPS they were written against; a larger catalog changes nothing, and a smaller one
// moves the targets in to rows and columns that exist.
const SETTLE_END_ROW: usize = 18;
const EVICTION_END_ROW: usize = 12;
const DIVE_COL: usize = 11;
/// One remote key-repeat per row: the sweep's pace, whatever its length.
const ROW_MS: u32 = 125;
/// How long after a sweep's last row its endpoint command may take to land.
const SWEEP_GRACE_MS: u32 = 1000;
/// The retained-offset bar `tests/poster_gate.py` grades the dive by (`shelf_end_px < 1000`).
/// Mirrored here so a shelf too short to reach it is refused up front and by name, instead of
/// failing later as though the renderer had regressed.
const DIVE_MIN_OFFSET_PX: f32 = 1000.0;

/// What the booted catalog lets a scene address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Content {
    /// The Library grid: its rows, and how many rows one screenful spans.
    Grid { rows: usize, per_screen: usize },
    /// Home shelf 0 — the one the hero's DOWN door lands on — and its card count.
    Shelf { cards: usize },
}

/// The catalog is too small for the scene to prove its property at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Unfit { what: &'static str, have: usize, need: usize }

fn plan_for(mode: Mode, content: Content) -> Result<Vec<Stage>, Unfit> {
    match (mode, content) {
        (Mode::Settle, Content::Grid { rows, per_screen }) => settle_plan(rows, per_screen),
        (Mode::Eviction, Content::Grid { rows, per_screen }) => eviction_plan(rows, per_screen),
        (Mode::Dive, Content::Shelf { cards }) => dive_plan(cards),
        _ => Err(Unfit { what: "content", have: 0, need: 1 }),
    }
}

/// Warm the grid's head, sweep fast to the deepest row available (at most 18), settle there.
/// The settled window must be art the warm head never put on screen, or the settle's requests
/// and uploads could be answered from the warm window's residents and prove nothing.
fn settle_plan(rows: usize, per_screen: usize) -> Result<Vec<Stage>, Unfit> {
    let end = SETTLE_END_ROW.min(rows.saturating_sub(1));
    if end < per_screen { return Err(Unfit { what: "library-rows", have: rows, need: per_screen + 1 }); }
    Ok(vec![hold("warm", Target::Library(0), true), sweep("move", 0, end), hold("settle", Target::Library(end), false)])
}

/// Warm three windows — the head, a middle row and the deepest row available (at most 12) — under
/// the pressure ceiling, sweep fast back to the head, and settle there. Each seed must bring a
/// screenful the head never drew, so it is new bytes the ceiling can only admit by evicting.
fn eviction_plan(rows: usize, per_screen: usize) -> Result<Vec<Stage>, Unfit> {
    let end = EVICTION_END_ROW.min(rows.saturating_sub(1));
    let mid = end.div_ceil(2);
    if mid < per_screen || mid >= end { return Err(Unfit { what: "library-rows", have: rows, need: 2 * per_screen }); }
    Ok(vec![
        hold("warm", Target::Library(0), true), hold("seed1", Target::Library(mid), true), hold("seed2", Target::Library(end), true),
        sweep("reverse", end, 0), hold("settle", Target::Library(0), false),
    ])
}

/// Where seating column `col` of an `n`-card Home shelf scrolls it, from rest: the same minimal
/// reveal the shelf's own spring steers to.
fn shelf_offset(col: usize, n: usize) -> f32 {
    use crate::ui::card_row::{scroll_into_view, RowStyle};
    let s = RowStyle::HOME;
    scroll_into_view(0.0, col, n, s.w, s.gap, crate::ui::consts::SCR_W - 2.0 * s.margin_x)
}

/// Warm Home shelf 0 at its deepest card (at most column 11), go up to the hero, and dive back.
fn dive_plan(cards: usize) -> Result<Vec<Stage>, Unfit> {
    let col = DIVE_COL.min(cards.saturating_sub(1));
    if cards == 0 || shelf_offset(col, cards) < DIVE_MIN_OFFSET_PX {
        let need = (1..=DIVE_COL + 1).find(|&n| shelf_offset(DIVE_COL.min(n - 1), n) >= DIVE_MIN_OFFSET_PX).unwrap_or(DIVE_COL + 1);
        return Err(Unfit { what: "home-shelf0-cards", have: cards, need });
    }
    Ok(vec![
        hold("warm", Target::GridSeed(col), true),
        Stage { name: "hero", target: Target::Hero, seed: false, handoff_snap: None, sweep: None, min_ms: 1600, max_ms: 6000, needs_art: false },
        // Hand off while the retained horizontal product is still moving fast, so
        // the natural deceleration and resulting requests/uploads belong to settle.
        Stage { name: "dive", target: Target::Grid, seed: false, handoff_snap: Some(0.9), sweep: None, min_ms: 0, max_ms: 4000, needs_art: false },
        hold("settle", Target::Grid, false),
    ])
}

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
    /// Derived once, from the catalog the scene actually booted into; empty until it has rows.
    plan: Vec<Stage>,
    /// The texture LRU clock taken after the previous `advance`, i.e. before that iteration's draws.
    mark: u64,
    /// The bytes the last iteration that drew anything used: the working set of the frame a
    /// completion judges. A settled page is not redrawn every iteration, so "since the previous
    /// iteration" alone would usually read zero.
    drawn_bytes: usize,
}
impl Scene {
    fn plan(&self) -> &[Stage] { &self.plan }
    /// What the catalog on screen lets this scene address, once it has arrived.
    fn content(app: &App, mode: Mode) -> Option<Content> {
        match mode {
            Mode::Settle | Mode::Eviction => Bridge::library_grid_extent(&app.pages)
                .map(|(rows, per_screen)| Content::Grid { rows, per_screen }),
            Mode::Dive => app.bridge.home_shelf_len(&app.pages, 0).filter(|&cards| cards > 0)
                .map(|cards| Content::Shelf { cards }),
        }
    }
    /// Size the plan to the catalog, or refuse by name a catalog too small to prove anything.
    /// Returns false while the plan cannot run (content not arrived yet, or refused).
    fn derive(&mut self, app: &App, mode: Mode) -> bool {
        if !self.plan.is_empty() { return true; }
        let Some(content) = Self::content(app, mode) else { return false };
        match plan_for(mode, content) {
            Ok(plan) => {
                let targets: Vec<String> = plan.iter().map(|stage| match stage.target {
                    Target::Library(row) => format!("{}@row{row}", stage.name),
                    Target::GridSeed(col) => format!("{}@col{col}", stage.name),
                    Target::Grid => format!("{}@grid", stage.name),
                    Target::Hero => format!("{}@hero", stage.name),
                }).collect();
                crate::log(&format!("poster-gate: kind={} phase=planned content={content:?} stages={}", mode.word(), targets.join(",")));
                self.plan = plan;
                true
            }
            Err(Unfit { what, have, need }) => {
                self.finished = true;
                crate::log(&format!("poster-gate: kind={} phase=unfit what={what} have={have} need={need}", mode.word()));
                false
            }
        }
    }
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
            // transition, not a test setter manufacturing P_EVICTED slots. The eviction scene
            // tightens it to its measured window once warm (pressure_ceiling).
            if mode != Mode::Settle { crate::ui::tex::scene_residency_budget(PRESSURE_MIB << 20); }
            crate::log(&format!("poster-gate: kind={} phase=armed budget_mib={}", mode.word(), if mode == Mode::Settle { crate::ui::tex::TEX_RESIDENT_BYTES_MAX >> 20 } else { PRESSURE_MIB }));
        }
    }
    fn positioned(app: &App, target: Target) -> bool {
        match target {
            Target::Library(row) => Bridge::library_grid_position(&app.pages).is_some_and(|(r, _)| r == row),
            Target::GridSeed(col) => app.bridge.home_grid_position(&app.pages) == Some((0, col)),
            Target::Grid => app.bridge.home_grid_position(&app.pages).is_some_and(|(row, _)| row == 0),
            Target::Hero => !app.bridge.home_grid_focused(&app.pages) && app.bridge.home_snap_target(&app.pages) == 0.0,
        }
    }
    fn drive(app: &mut App, target: Target, seed: bool, now: u32) {
        if Self::positioned(app, target) { return; }
        if seed {
            match target {
                Target::Library(row) => Bridge::library_command(&mut app.pages, LibraryCmd::FocusGrid { row, col: 0 }),
                Target::GridSeed(col) => { app.bridge.home_command(HomeCmd::FocusGrid { row: 0, col }); }
                // A plan never seeds its settling grid target; the dive reaches it by key.
                Target::Grid => {}
                Target::Hero => { app.bridge.home_command(HomeCmd::Hero); }
            }
            return;
        }
        use crate::ui::machine::{Key, Tick};
        let key = match target {
            Target::Library(row) => Bridge::library_grid_position(&app.pages)
                .map(|(current, _)| if current < row { Key::Down } else { Key::Up }),
            Target::Grid | Target::GridSeed(_) => (!app.bridge.home_grid_focused(&app.pages)).then_some(Key::Down),
            Target::Hero => app.bridge.home_grid_focused(&app.pages).then_some(Key::Up),
        };
        if let Some(key) = key {
            app.inputs.extend(crate::app::bridge::script_key(key, Tick { ms: now, dt_us: 0 }));
        }
    }
    fn reached(app: &App, target: Target) -> bool {
        Self::positioned(app, target) && match target {
            Target::Library(_) => true,
            Target::GridSeed(_) | Target::Grid => app.bridge.home_motion_witness(&app.pages)
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
        if !route_ok || !self.derive(app, mode) { return; }
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
        let window = self.drawn_bytes / crate::ui::tex::render_area();
        report(mode.word(), stage.name, elapsed, stats, ready, self.witness, window);
        if !ready {
            self.finished = true;
            crate::log(&format!("poster-gate: kind={} phase=failed reason=target-or-art", mode.word()));
            return;
        }
        if mode == Mode::Eviction && self.stage == 0 {
            // Size the pressure to the window the catalog actually drew (see pressure_ceiling).
            // Still the real cache's byte-LRU: the seeds' own uploads are what evict.
            let ceiling = pressure_ceiling(window);
            crate::ui::tex::scene_residency_budget(ceiling);
            crate::log(&format!("poster-gate: kind={} phase=ceiling window_kib={} budget_kib={}", mode.word(), window >> 10, ceiling >> 10));
        }
        self.finish_stage(now);
    }
}
fn report(kind: &str, phase: &str, ms: u32, s: Stats, complete: bool, witness: Option<Witness>, window: usize) {
    let w = witness.unwrap_or(Witness::new([0.0; 3]));
    crate::log(&format!("poster-gate: kind={kind} phase={phase} ms={ms} frames={} draws={} ready={} moving={} moving_frames={} moving_ms={} unknown={} requested={} requested_moving={} refused_new={} refused_evicted={} refused_retry={} rearmed={} uploads={} lost={} last_draws={} last_ready={} complete={} snap_begin_milli={} snap_end_milli={} shelf_start_px={} shelf_end_px={} shelf_span_px={} shelf_v_milli={} window_kib={} resident_kib={}",
        s.frames, s.draws, s.ready, s.moving, s.moving_frames, s.moving_last_ms.wrapping_sub(s.moving_first_ms), s.unknown,
        s.requested, s.requested_moving, s.refused_new, s.refused_evicted, s.refused_retry, s.rearmed, s.uploads, s.lost, s.last_draws, s.last_ready, complete as u8,
        (w.snap_begin * 1000.0).round() as i32, (w.snap_end * 1000.0).round() as i32,
        w.first.round() as i32, w.last.round() as i32, (w.max - w.min).round() as i32,
        (w.max_velocity * 1000.0).round() as i32, window >> 10, (crate::ui::tex::resident_bytes() / crate::ui::tex::render_area()) >> 10));
}
pub(crate) fn tick(app: &mut App, now: u32) {
    let mut scene = std::mem::take(&mut app.scenarios.poster_gate);
    if scene.mode.is_some() && !scene.finished {
        let used = crate::ui::tex::bytes_used_since(scene.mark);
        if used > 0 { scene.drawn_bytes = used; }
    }
    scene.advance(app, now);
    if scene.mode.is_some() && !scene.finished { scene.mark = crate::ui::tex::use_clock(); }
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
    /// A catalog at least as large as every cap: the plans the scenes were written against.
    fn large(mode: Mode) -> Vec<Stage> {
        let content = if mode == Mode::Dive { Content::Shelf { cards: 20 } } else { Content::Grid { rows: 26, per_screen: 3 } };
        plan_for(mode, content).unwrap()
    }
    #[test]
    fn every_phase_owns_its_handoff_draw_before_the_next_tick() {
        for mode in [Mode::Settle, Mode::Eviction, Mode::Dive] {
            let mut scene = Scene { checked: true, mode: Some(mode), plan: large(mode), ..Scene::default() };
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
        let mut scene = Scene { checked: true, mode: Some(Mode::Dive), plan: large(Mode::Dive), ..Scene::default() };
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
        let (settle, eviction) = (large(Mode::Settle), large(Mode::Eviction));
        assert_eq!(settle[1].sweep_row(0), Some(0));
        assert_eq!(settle[1].sweep_row(2250), Some(18));
        assert_eq!(settle[1].sweep_row(9000), Some(18));
        assert_eq!(eviction[3].sweep_row(0), Some(12));
        assert_eq!(eviction[3].sweep_row(750), Some(6));
        assert_eq!(eviction[3].sweep_row(1500), Some(0));
        // The endpoint command lands on a later dispatch turn; do not timeout on
        // the very iteration that first queued it.
        assert_eq!(settle[1].completion(2250, false, false), None);
        assert_eq!(settle[1].completion(2266, true, false), Some(true));
    }
    #[test]
    fn settling_requires_a_full_new_draw_and_has_a_finite_failure_deadline() {
        let s = large(Mode::Settle)[2];
        assert_eq!(s.completion(899, true, true), None);
        assert_eq!(s.completion(900, true, false), None);
        assert_eq!(s.completion(900, false, true), None);
        assert_eq!(s.completion(900, true, true), Some(true));
        assert_eq!(s.completion(8000, true, false), Some(false));
    }
    fn rows(plan: &[Stage]) -> Vec<(&'static str, Target)> { plan.iter().map(|stage| (stage.name, stage.target)).collect() }
    #[test]
    fn a_large_catalog_keeps_the_plans_the_scenes_were_written_against() {
        use Target::*;
        let settle = large(Mode::Settle);
        assert_eq!(rows(&settle), [("warm", Library(0)), ("move", Library(0)), ("settle", Library(18))]);
        assert_eq!((settle[1].sweep, settle[1].min_ms, settle[1].max_ms), (Some((0, 18)), 2250, 3250));
        let eviction = large(Mode::Eviction);
        assert_eq!(rows(&eviction), [("warm", Library(0)), ("seed1", Library(6)), ("seed2", Library(12)), ("reverse", Library(12)), ("settle", Library(0))]);
        assert_eq!((eviction[3].sweep, eviction[3].min_ms, eviction[3].max_ms), (Some((12, 0)), 1500, 2500));
        assert_eq!(rows(&large(Mode::Dive)), [("warm", GridSeed(11)), ("hero", Hero), ("dive", Grid), ("settle", Grid)]);
    }
    #[test]
    fn a_small_catalog_moves_every_target_in_to_rows_and_columns_that_exist() {
        use Target::*;
        // 37 items in a six-column grid: rows 0..=6. Every target must exist, the sweeps keep
        // their per-row pace, and each seed still brings a screenful the head never drew.
        let settle = settle_plan(7, 3).unwrap();
        assert_eq!(rows(&settle), [("warm", Library(0)), ("move", Library(0)), ("settle", Library(6))]);
        assert_eq!((settle[1].sweep, settle[1].min_ms, settle[1].max_ms), (Some((0, 6)), 750, 1750));
        let eviction = eviction_plan(7, 3).unwrap();
        assert_eq!(rows(&eviction), [("warm", Library(0)), ("seed1", Library(3)), ("seed2", Library(6)), ("reverse", Library(6)), ("settle", Library(0))]);
        assert_eq!((eviction[3].sweep, eviction[3].min_ms, eviction[3].max_ms), (Some((6, 0)), 750, 1750));
        // Ten cards: the deepest one is column 9, which still retains more than the graded offset.
        assert_eq!(rows(&dive_plan(10).unwrap())[0], ("warm", GridSeed(9)));
        assert!(shelf_offset(9, 10) >= DIVE_MIN_OFFSET_PX);
    }
    #[test]
    fn a_catalog_too_small_to_prove_anything_is_refused_by_name() {
        // Settle: the settled row must lie beyond the head's screenful.
        assert_eq!(settle_plan(3, 3), Err(Unfit { what: "library-rows", have: 3, need: 4 }));
        assert_eq!(settle_plan(4, 3).map(|plan| plan[2].target), Ok(Target::Library(3)));
        // Eviction: the middle seed must lie beyond the head's screenful, the deepest beyond it.
        assert_eq!(eviction_plan(5, 3), Err(Unfit { what: "library-rows", have: 5, need: 6 }));
        assert_eq!(eviction_plan(6, 3).map(|plan| (plan[1].target, plan[2].target)), Ok((Target::Library(3), Target::Library(5))));
        assert_eq!(eviction_plan(1, 3).map(|_| ()), Err(Unfit { what: "library-rows", have: 1, need: 6 }));
        // Dive: the retained offset must reach the bar the grader holds it to.
        assert!(shelf_offset(8, 9) < DIVE_MIN_OFFSET_PX);
        assert_eq!(dive_plan(9), Err(Unfit { what: "home-shelf0-cards", have: 9, need: 10 }));
        assert_eq!(dive_plan(0), Err(Unfit { what: "home-shelf0-cards", have: 0, need: 10 }));
        // A catalog of the wrong shape can never be planned against.
        assert!(plan_for(Mode::Dive, Content::Grid { rows: 26, per_screen: 3 }).is_err());
    }
    #[test]
    fn the_pressure_ceiling_holds_a_window_and_a_half_and_never_exceeds_twelve_mib() {
        let mib = 1 << 20;
        assert_eq!(pressure_ceiling(4 * mib), 6 * mib, "a shallow catalog's small window: 1.5x");
        assert_eq!(pressure_ceiling(8 * mib), PRESSURE_MIB << 20, "the deep plan keeps its 12 MiB");
        assert_eq!(pressure_ceiling(20 * mib), PRESSURE_MIB << 20);
    }
}
