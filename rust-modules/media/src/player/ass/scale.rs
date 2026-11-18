//! The render scale of a moving cue's frames (issue #1328): the size libass is asked to draw at,
//! as a fraction of the output size, and the pure decision that picks it.
//!
//! A moving cue's frames cost what libass draws in them, which is the number of pixels as much as
//! anything: the opening of the issue's file costs ~85 ms a frame at 1080p on the television, so
//! one renderer makes ~12 a second against the ~30 the cue deserves, and more renderers are limited
//! by the set's cores and memory. Drawn at half the size the same frames cost ~0.45 as much
//! (measured on the host with the storage size fixed: libass 0.64x at 720p and 0.49x at 540p, the
//! composite 0.49x and 0.30x, bytes published 0.54x and 0.37x). So a stretch is drawn smaller only
//! while it cannot meet the target at full size, and every other picture (a still cue, a paused
//! player, the first frame of a stretch) stays at full size.
//!
//! The choice is a [`Governor`] with hysteresis: a stretch's first measured frame decides its rung
//! at once (what the last stretch cost says nothing of this one); inside a stretch it steps down
//! when the projection at the current rung misses the target for [`DOWN_HOLD_MS`], and back up one
//! rung only when the larger rung would comfortably meet it ([`COMFORT`]) for [`UP_HOLD_MS`], with a
//! bar after a step up that had to be taken back.
//!
//! Pure: no clock, no thread and no native call. The measurements arrive as a [`Load`] and the
//! playhead as a value; [`Mailbox`](super::mailbox::Mailbox) owns the [`Governor`] and the runtime
//! logs what it reports.

/// What a frame drawn at two thirds of each side costs over one at full size: the host's
/// measurement weighted by the television's split of an 85 ms frame (libass 65 ms, composite 20 ms)
/// is 0.605, and the constant is rounded DOWN so that a cost taken back to full size is never
/// overstated and a step up never projects better than it should.
const COST_RATIO_TWO_THIRDS: f64 = 0.60;
/// The same at half of each side: 0.445 measured, rounded down.
const COST_RATIO_HALF: f64 = 0.44;
/// A larger rung is worth going back to only when its projected step is under this share of the
/// target: the band between it and the target is where a rung stays put.
const COMFORT: f64 = 0.75;
/// How long a projection that misses the target has to last, in playhead milliseconds at normal
/// speed, before the rung steps down.
const DOWN_HOLD_MS: i64 = 600;
/// How long a comfortable projection has to last before the rung steps up one.
const UP_HOLD_MS: i64 = 3_000;
/// A hold is only counted over frames of the rung's own: this many, drawn since the last move,
/// have to have been timed first.
const SETTLE_SAMPLES: u32 = 6;
/// A step down within this long of a step up means the projection was too kind; the next step up
/// is barred for [`FLIP_BAR_MS`], so a model that is wrong costs one flap per half minute at most.
const FLIP_WINDOW_MS: i64 = 10_000;
const FLIP_BAR_MS: i64 = 30_000;

/// A frame size as a fraction of the output size. libass is told the smaller frame size but the
/// SAME storage size, so the script lays out identically and only the number of pixels it draws
/// changes; the screen draws the regions back up to the output rectangle with linear filtering.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RenderScale {
    /// The output size: the picture a still cue, a paused player and the first frame of a moving
    /// cue are drawn at.
    #[default]
    Full,
    /// Two thirds of each side (720p of a 1080p output).
    TwoThirds,
    /// Half of each side (540p of a 1080p output).
    Half,
}

impl RenderScale {
    /// The rungs, the largest first.
    pub const LADDER: [RenderScale; 3] = [RenderScale::Full, RenderScale::TwoThirds, RenderScale::Half];

    /// The side of the frame over the side of the output.
    pub fn factor(self) -> f64 {
        match self {
            RenderScale::Full => 1.0,
            RenderScale::TwoThirds => 2.0 / 3.0,
            RenderScale::Half => 0.5,
        }
    }

    /// The frame size libass is asked for when the output is `width` x `height`: each side scaled
    /// and rounded, never under one pixel. `Full` is the output size itself.
    pub fn render_size(self, width: i32, height: i32) -> (i32, i32) {
        let side = |n: i32| ((f64::from(n) * self.factor()).round() as i32).max(1);
        (side(width), side(height))
    }

    /// What a frame costs at this size over what it costs at full size.
    pub fn cost_ratio(self) -> f64 {
        match self {
            RenderScale::Full => 1.0,
            RenderScale::TwoThirds => COST_RATIO_TWO_THIRDS,
            RenderScale::Half => COST_RATIO_HALF,
        }
    }

    /// The next rung up, if there is one.
    fn larger(self) -> Option<RenderScale> {
        match self {
            RenderScale::Full => None,
            RenderScale::TwoThirds => Some(RenderScale::Full),
            RenderScale::Half => Some(RenderScale::TwoThirds),
        }
    }

    /// The number the event log prints: the side's factor to two places.
    pub fn label(self) -> &'static str {
        match self {
            RenderScale::Full => "1.00",
            RenderScale::TwoThirds => "0.67",
            RenderScale::Half => "0.50",
        }
    }
}

/// What the decision is made from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Load {
    /// What a frame costs at full size, in milliseconds: the mean over the moving frames the
    /// workers made, each taken back to full size by its own rung's cost ratio.
    pub cost_full_ms: f64,
    /// How many renderers the pool allows the selection (the primary included).
    pub renderers: usize,
    /// Playhead milliseconds per wall-clock millisecond.
    pub rate: f64,
    /// The step the pipeline aims at, in playhead milliseconds: about thirty a second, or one video
    /// frame, whichever is longer.
    pub target_ms: i64,
}

impl Load {
    /// The frame step the plan settles at drawing at `scale`: the cost over the renderers, in
    /// playhead milliseconds.
    pub fn step_at(&self, scale: RenderScale) -> f64 {
        self.cost_full_ms * scale.cost_ratio() * self.rate / self.renderers.max(1) as f64
    }

    /// The largest rung whose projection meets the target, else the smallest there is.
    fn fitting(&self) -> RenderScale {
        let target = self.target_ms as f64;
        RenderScale::LADDER.into_iter().find(|&rung| self.step_at(rung) <= target).unwrap_or(RenderScale::Half)
    }

    /// Where a stretch drawn at `current` should be: down to what fits when it misses the target,
    /// one rung up when the larger one would comfortably meet it, else where it is.
    fn wanted(&self, current: RenderScale) -> RenderScale {
        let target = self.target_ms as f64;
        if self.step_at(current) > target {
            return self.fitting();
        }
        match current.larger() {
            Some(larger) if self.step_at(larger) <= COMFORT * target => larger,
            _ => current,
        }
    }
}

/// A move from one rung to another.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Change {
    pub from: RenderScale,
    pub to: RenderScale,
    /// The numbers it was decided from, for the log.
    pub load: Load,
}

/// The scale a moving stretch is drawn at, with the hysteresis that keeps it from flapping.
#[derive(Clone, Copy, Debug, Default)]
pub struct Governor {
    scale: RenderScale,
    /// Frames of the current rung timed since the last move or the start of the stretch.
    samples: u32,
    /// A stretch has begun and its first sample has not been judged.
    fresh: bool,
    /// The rung a projection has been asking for, and the playhead it first did.
    pending: Option<(RenderScale, i64)>,
    /// The playhead of the last step up, for [`FLIP_WINDOW_MS`].
    last_up_ms: Option<i64>,
    /// No step up before this playhead.
    barred_until_ms: i64,
}

impl Governor {
    /// At full size, with the first sample deciding: nothing has been drawn yet.
    pub const fn new() -> Self {
        Self { scale: RenderScale::Full, samples: 0, fresh: true, pending: None, last_up_ms: None, barred_until_ms: 0 }
    }

    /// The rung moving frames are drawn at now.
    pub fn scale(&self) -> RenderScale {
        self.scale
    }

    /// A moving stretch begins after a still picture (or on a new timeline): what the last one
    /// cost says nothing of this one, so its first sample replaces the cost and decides at once.
    pub fn begin_stretch(&mut self) {
        self.fresh = true;
        self.samples = 0;
        self.pending = None;
    }

    /// The next sample is the first of a stretch: it replaces what was known of the cost.
    pub fn is_fresh(&self) -> bool {
        self.fresh && self.samples == 0
    }

    /// A steady frame of the current rung was timed.
    pub fn note_sample(&mut self) {
        self.samples = self.samples.saturating_add(1);
    }

    /// One UI tick of a running pipeline at playhead `now_ms`: the move to make, if any. Nothing is
    /// decided before a sample has been timed (the cost is a default until then).
    pub fn observe(&mut self, now_ms: i64, load: Load) -> Option<Change> {
        if self.samples == 0 {
            return None;
        }
        if self.fresh {
            self.fresh = false;
            let rung = load.fitting();
            return (rung != self.scale).then(|| self.apply(now_ms, load, rung));
        }
        if self.samples < SETTLE_SAMPLES {
            return None;
        }
        let wanted = load.wanted(self.scale);
        if wanted == self.scale {
            self.pending = None;
            return None;
        }
        let up = wanted.factor() > self.scale.factor();
        if up && now_ms < self.barred_until_ms {
            self.pending = None;
            return None;
        }
        let since = match self.pending {
            Some((rung, since)) if rung == wanted && now_ms >= since => since,
            _ => {
                self.pending = Some((wanted, now_ms));
                now_ms
            }
        };
        let hold = if up { UP_HOLD_MS } else { DOWN_HOLD_MS };
        let hold = (hold as f64 * load.rate).round() as i64;
        (now_ms - since >= hold).then(|| self.apply(now_ms, load, wanted))
    }

    fn apply(&mut self, now_ms: i64, load: Load, to: RenderScale) -> Change {
        let from = self.scale;
        if to.factor() > from.factor() {
            self.last_up_ms = Some(now_ms);
        } else if self.last_up_ms.is_some_and(|up| now_ms - up < FLIP_WINDOW_MS) {
            self.barred_until_ms = now_ms + FLIP_BAR_MS;
        }
        self.scale = to;
        self.samples = 0;
        self.pending = None;
        Change { from, to, load }
    }
}
