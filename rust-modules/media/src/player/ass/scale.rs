//! The render scale of a moving cue's frames (issue #1328): the size libass is asked to draw at,
//! as a fraction of the output size, and the pure decision that picks it.
//!
//! Pure: no clock, no thread and no native call. The measurements arrive as a [`Load`] and the
//! playhead as a value; [`Mailbox`](super::mailbox::Mailbox) owns the [`Governor`] and the runtime
//! logs what it reports.

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
        0.0
    }

    /// The next rung down, if there is one.
    fn smaller(self) -> Option<RenderScale> {
        None
    }

    /// The next rung up, if there is one.
    fn larger(self) -> Option<RenderScale> {
        None
    }

    /// The number the event log prints: the side's factor to two places.
    pub fn label(self) -> &'static str {
        "?"
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
        let _ = scale;
        0.0
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
}

impl Governor {
    /// The rung moving frames are drawn at now.
    pub fn scale(&self) -> RenderScale {
        self.scale
    }

    /// A moving stretch begins after a still picture (or a new timeline): what the last one cost
    /// says nothing of this one, so its first sample decides at once.
    pub fn begin_stretch(&mut self) {}

    /// The next sample, if it is a first one, replaces what was known of the cost.
    pub fn is_fresh(&self) -> bool {
        false
    }

    /// A steady frame of the current rung was timed.
    pub fn note_sample(&mut self) {}

    /// One UI tick of a running pipeline at playhead `now_ms`: the move to make, if any.
    pub fn observe(&mut self, now_ms: i64, load: Load) -> Option<Change> {
        let _ = (now_ms, load);
        None
    }
}
