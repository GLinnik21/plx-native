//! The render-scale decision (issue #1328) with no renderer: the governor is fed the load a
//! moving stretch measures, one UI tick at a time, and the moves it makes are read back.
use super::scale::{Change, Governor, Load, RenderScale};
use super::scale::RenderScale::{Full, Half, TwoThirds};

fn load(cost_full_ms: f64, renderers: usize) -> Load {
    Load { cost_full_ms, renderers, rate: 1.0, target_ms: 33 }
}

/// A governor over a running pipeline: a UI tick every 16 ms and a cost sample every other tick (a
/// frame every 32 ms, which is what a pipeline at the target rate makes).
struct Run {
    gov: Governor,
    now: i64,
    ticks: u32,
    /// `(playhead of the tick, from, to)` for every move.
    moves: Vec<(i64, RenderScale, RenderScale)>,
}

impl Run {
    /// A stretch that has begun and has had its first sample judged at `first`.
    fn begun(first: Load) -> Self {
        let mut run = Run { gov: Governor::default(), now: 10_000, ticks: 0, moves: Vec::new() };
        run.gov.begin_stretch();
        run.gov.note_sample();
        run.record(first);
        run
    }

    /// A stretch past its first sample, still at full size: what the following ticks decide is
    /// decided by a hold, not at once.
    fn settled() -> Self {
        Run::begun(load(1.0, 1))
    }

    fn record(&mut self, load: Load) {
        if let Some(Change { from, to, .. }) = self.gov.observe(self.now, load) {
            self.moves.push((self.now, from, to));
        }
    }

    fn tick(&mut self, load: Load) {
        self.now += 16;
        self.ticks += 1;
        if self.ticks % 2 == 0 {
            self.gov.note_sample();
        }
        self.record(load);
    }

    fn run(&mut self, ms: i64, load: Load) {
        let until = self.now + ms;
        while self.now < until {
            self.tick(load);
        }
    }

    fn scale(&self) -> RenderScale {
        self.gov.scale()
    }
}

#[test]
fn the_rungs_are_fractions_of_the_output_size() {
    assert_eq!(RenderScale::LADDER, [Full, TwoThirds, Half]);
    assert_eq!(Full.render_size(1920, 1080), (1920, 1080));
    assert_eq!(TwoThirds.render_size(1920, 1080), (1280, 720));
    assert_eq!(Half.render_size(1920, 1080), (960, 540));
    // a letterboxed authored viewport keeps its shape to a pixel
    assert_eq!(TwoThirds.render_size(1920, 800), (1280, 533));
    assert_eq!(Half.render_size(1920, 800), (960, 400));
    assert_eq!(Half.render_size(1, 1), (1, 1), "never an empty frame");
    assert_eq!((Full.factor(), TwoThirds.factor(), Half.factor()), (1.0, 2.0 / 3.0, 0.5));
    assert_eq!([Full.label(), TwoThirds.label(), Half.label()], ["1.00", "0.67", "0.50"]);
    assert_eq!(RenderScale::default(), Full);
}

/// The ratios are the measured ones: libass 0.64 and 0.49, the composite 0.49 and 0.30 (host, same
/// frames, frame size changed with the storage size fixed), weighted by the television's 65 ms and
/// 20 ms of an 85 ms frame, and rounded DOWN so a cost taken back to full size is never overstated.
#[test]
fn a_smaller_frame_costs_less_by_the_measured_ratios() {
    assert_eq!(Full.cost_ratio(), 1.0);
    assert_eq!(TwoThirds.cost_ratio(), 0.60);
    assert_eq!(Half.cost_ratio(), 0.44);
    let (lib, comp) = ((0.64, 0.49), (0.49, 0.30));
    let weighted = |l: f64, c: f64| (65.0 * l + 20.0 * c) / 85.0;
    assert!(TwoThirds.cost_ratio() <= weighted(lib.0, comp.0), "{}", weighted(lib.0, comp.0));
    assert!(Half.cost_ratio() <= weighted(lib.1, comp.1), "{}", weighted(lib.1, comp.1));
}

#[test]
fn a_cheap_animation_stays_at_full_size() {
    let mut run = Run::begun(load(12.0, 1));
    run.run(60_000, load(12.0, 1));
    assert_eq!(run.scale(), Full);
    assert!(run.moves.is_empty(), "{:?}", run.moves);
    // even a frame at the edge of the target (33 ms of cost for a 33 ms step) is no reason to shrink
    let mut run = Run::begun(load(33.0, 1));
    run.run(30_000, load(33.0, 1));
    assert!(run.moves.is_empty(), "{:?}", run.moves);
}

#[test]
fn the_first_sample_of_a_heavy_stretch_decides_at_once() {
    // the opening of the issue's file: 85 ms a frame, one renderer
    let run = Run::begun(load(85.0, 1));
    assert_eq!(run.moves, [(10_000, Full, Half)]);
    assert_eq!(run.scale(), Half);
    // a cost that two thirds is enough for does not go further down
    let run = Run::begun(load(50.0, 1));
    assert_eq!(run.moves, [(10_000, Full, TwoThirds)]);
}

#[test]
fn nothing_is_decided_without_a_sample() {
    let mut gov = Governor::default();
    gov.begin_stretch();
    assert!(gov.is_fresh());
    assert_eq!(gov.observe(10_000, load(500.0, 1)), None, "no sample, no evidence: the cost is a default");
    assert_eq!(gov.scale(), Full);
    gov.note_sample();
    assert_eq!(gov.observe(10_016, load(500.0, 1)).map(|c| c.to), Some(Half));
    assert!(!gov.is_fresh(), "the first sample has been judged");
}

#[test]
fn a_sustained_cost_steps_down_after_its_hold_and_not_before() {
    let mut run = Run::settled();
    let t0 = run.now;
    run.run(550, load(85.0, 1));
    assert_eq!(run.scale(), Full, "a cost that has lasted half a second is not yet sustained");
    run.run(300, load(85.0, 1));
    assert_eq!(run.moves.len(), 1, "{:?}", run.moves);
    let (at, from, to) = run.moves[0];
    assert_eq!((from, to), (Full, Half));
    assert!((600..=850).contains(&(at - t0)), "stepped down {} ms into it", at - t0);
    run.run(10_000, load(85.0, 1));
    assert_eq!(run.moves.len(), 1, "and it stays down: nothing is smaller");
}

#[test]
fn it_steps_to_two_thirds_and_then_to_half_as_the_cost_grows() {
    let mut run = Run::settled();
    run.run(2_000, load(50.0, 1));
    assert_eq!(run.moves.iter().map(|m| (m.1, m.2)).collect::<Vec<_>>(), [(Full, TwoThirds)]);
    // the cost grows (a denser stretch): two thirds no longer meets the target either
    run.run(3_000, load(80.0, 1));
    assert_eq!(run.moves.iter().map(|m| (m.1, m.2)).collect::<Vec<_>>(), [(Full, TwoThirds), (TwoThirds, Half)]);
    assert_eq!(run.scale(), Half);
}

#[test]
fn a_spike_shorter_than_the_hold_changes_nothing_however_often_it_comes() {
    let mut run = Run::settled();
    run.run(400, load(85.0, 1));
    run.run(2_000, load(12.0, 1));
    assert!(run.moves.is_empty(), "{:?}", run.moves);
    // one heavy sample in a few: the verdict is reset by every sample that does not agree
    for _ in 0..200 {
        run.run(300, load(85.0, 1));
        run.run(300, load(20.0, 1));
    }
    assert!(run.moves.is_empty(), "{:?}", run.moves);
}

#[test]
fn it_does_not_flap_at_the_edge() {
    // a cost just over the target at full size: it steps down once and stays there for a minute
    let mut run = Run::settled();
    run.run(60_000, load(34.0, 1));
    assert_eq!(run.moves.len(), 1, "{:?}", run.moves);
    assert_eq!((run.moves[0].1, run.moves[0].2), (Full, TwoThirds));
    // a cost that wavers across the target is judged sample by sample and never held
    let mut run = Run::settled();
    for _ in 0..1000 {
        run.tick(load(30.0, 1));
        run.tick(load(36.0, 1));
    }
    assert!(run.moves.is_empty(), "{:?}", run.moves);
}

#[test]
fn it_goes_back_up_only_when_the_larger_size_comfortably_meets_the_target() {
    let mut run = Run::begun(load(85.0, 1));
    assert_eq!(run.scale(), Half);
    // 28 ms at full size meets the 33 ms target but not comfortably (under three quarters of it); the
    // rung below it, at 17 ms, does, so a cost that settles there goes up one rung and no further
    run.run(60_000, load(28.0, 1));
    assert_eq!(run.moves.iter().map(|m| (m.1, m.2)).collect::<Vec<_>>(), [(Full, Half), (Half, TwoThirds)]);
    assert_eq!(run.scale(), TwoThirds);
}

#[test]
fn it_returns_to_full_size_a_rung_at_a_time_once_the_cost_is_low() {
    let mut run = Run::begun(load(85.0, 1));
    assert_eq!(run.scale(), Half);
    let t0 = run.now;
    run.run(2_500, load(20.0, 1));
    assert_eq!(run.scale(), Half, "the cost has not held for three seconds yet");
    run.run(1_000, load(20.0, 1));
    assert_eq!(run.scale(), TwoThirds, "one rung at a time");
    let (at, ..) = run.moves[1];
    assert!((3_000..=3_400).contains(&(at - t0)), "stepped up {} ms in", at - t0);
    run.run(4_000, load(20.0, 1));
    assert_eq!(run.scale(), Full, "and the next rung after another hold");
    run.run(30_000, load(20.0, 1));
    assert_eq!(run.moves.len(), 3, "and then it stays: {:?}", run.moves);
}

#[test]
fn a_failed_step_up_bars_the_next_one_for_half_a_minute() {
    let mut run = Run::begun(load(85.0, 1));
    run.run(4_000, load(20.0, 1));
    assert_eq!(run.scale(), TwoThirds);
    // two thirds turns out to cost more than the projection said: back down at once
    run.run(1_000, load(60.0, 1));
    assert_eq!(run.scale(), Half);
    let down = run.now;
    // the cost is comfortable again: no step up for the half minute that follows
    run.run(25_000, load(20.0, 1));
    assert_eq!(run.scale(), Half, "{:?}", run.moves);
    run.run(10_000, load(20.0, 1));
    assert_eq!(run.scale(), TwoThirds);
    assert!(run.moves.last().unwrap().0 - down >= 30_000, "{:?}", run.moves);
}

#[test]
fn the_renderer_count_the_pool_has_decides_what_meets_the_target() {
    // 85 ms a frame: one renderer cannot make 30 a second at any size, two can at two thirds, three
    // at full size
    for (renderers, expected) in [(1, Some(Half)), (2, Some(TwoThirds)), (3, None)] {
        let run = Run::begun(load(85.0, renderers));
        assert_eq!(run.moves.first().map(|m| m.2), expected, "{renderers} renderer(s)");
    }
    // a pool of none is a pool of one
    let run = Run::begun(load(85.0, 0));
    assert_eq!(run.scale(), Half);
}

#[test]
fn a_faster_clock_makes_the_same_cost_a_longer_step() {
    let fast = Load { rate: 2.0, ..load(20.0, 1) };
    assert!(Run::begun(load(20.0, 1)).moves.is_empty());
    assert_eq!(Run::begun(fast).scale(), TwoThirds, "20 ms of work spans 40 ms of playhead at twice the speed");
}

#[test]
fn the_target_is_a_video_frame_for_slow_content() {
    let film = Load { target_ms: 42, ..load(40.0, 1) };
    assert!(Run::begun(film).moves.is_empty(), "40 ms makes a 24 fps frame");
    assert_eq!(Run::begun(load(40.0, 1)).scale(), TwoThirds, "but not a 33 ms step");
}

#[test]
fn a_move_reports_the_numbers_it_was_made_from() {
    let mut gov = Governor::default();
    gov.begin_stretch();
    gov.note_sample();
    let heavy = load(85.0, 1);
    assert_eq!(gov.observe(5_000, heavy), Some(Change { from: Full, to: Half, load: heavy }));
}

#[test]
fn a_projection_is_the_cost_at_that_size_over_the_renderers() {
    let l = Load { rate: 1.5, ..load(80.0, 2) };
    assert_eq!(l.step_at(Full), 60.0);
    assert!((l.step_at(TwoThirds) - 36.0).abs() < 1e-9);
    assert!((l.step_at(Half) - 26.4).abs() < 1e-9);
}
