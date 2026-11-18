//! The request/publish mailbox between the SDL thread and the render workers (plan section 4.2).
//!
//! A frame is published with the window of playheads it stays right for ([`Validity`]), so a UI
//! tick inside the window costs no job. The worker also renders the next boundary ahead of its
//! time (an `Ahead` job), and the next tick at or after that boundary promotes the held frame.
//!
//! A Normal result is published unless the timeline it was rendered for (epoch, source, size) was
//! replaced meanwhile or the job was withdrawn, not only when it answers the latest request: a
//! revision that moved only appended events, which start ahead of the playhead, so the frame is
//! still right for its time. A render that takes several packet intervals would otherwise never land.
//!
//! **A moving cue is a pipeline** (issue #1328). A frame of a moving cue is right for one playhead
//! only, so asking for it when the screen needs it shows it one render late (50-150 ms on the
//! television). While the player is running ([`Pace`]) the mailbox instead plans frames for FUTURE
//! playheads on a grid of one `step` (the render cost over the workers, never under one video
//! frame), a `lead` ahead of the playhead (a render, a quarter more, and a tick), hands them out as
//! `Pipe` jobs, holds the frames that land, and shows the one with the greatest playhead not after
//! now: never early, and at most a step old. Frames are planned up to a horizon only (`lead` plus a
//! step per worker), so a spare worker parks rather than rendering what the screen will not reach.
//! An event boundary inside a moving stretch (a karaoke script has one every ~19 ms) does not end a
//! frame or move the plan off its grid: the plan resolves it at its own step, and only the boundary
//! that ends the stretch is rendered at its own playhead ([`Published::ends_at`]).
//! Appended events leave the plan alone from the first event they add onward only ([`adopt_revision`]).
//! Anything the plan cannot absorb (a jump of the clock, a seek, a size, a source, a stalled worker)
//! throws it away and asks for one Normal job, exactly as a still cue does.
//!
//! **A moving stretch the renderers cannot keep up with is drawn smaller** (issue #1328, [`Governor`]).
//! The mailbox keeps the cost of a frame at full size (a frame drawn smaller is taken back to it by
//! its rung's cost ratio), asks the governor on every tick of the running pipeline which rung the
//! stretch should be drawn at given the renderers the pool allows ([`Mailbox::set_capacity`]), and
//! hands the `Pipe` jobs that rung. Only those are drawn smaller (and the Normal job that fills a
//! hole inside a reduced stretch, so libass's caches are not emptied by a frame of another size and
//! again by the next): a Normal or Ahead job otherwise, the first frame of a stretch, a still picture
//! and a paused one are drawn at full size, and a still picture the pipeline handed over drawn
//! smaller is drawn again at full size by one Normal job. The scale is not part of the [`Key`], so a
//! move never replaces the frame on screen: it stands until a frame of the new scale lands, and each
//! frame says which scale it was drawn at. The rung belongs to the selection it was decided for: a
//! new epoch, source or size starts at full size again and waits for the pool.
//!
//! Pure bookkeeping: no thread, no native call, no clock but the one stamp behind [`Job::queued`].
//! The pace and the playhead arrive as values in [`Mailbox::request`]; the worker takes frames
//! against the latest of them between ticks. `Runtime` owns the lock and the wake; the worker loop
//! owns the rendering.
//!
//! [`adopt_revision`]: Mailbox::adopt_revision
use super::scale::{Change, Governor, Load};
use super::{earliest_change_ms, AlignSet, Frame, Key, Pace, RenderScale, Source};
use crate::player::ass_timing::Validity;
use std::sync::Arc;
use std::time::Instant;

/// A moving cue is never asked for closer together than this (about thirty a second), whatever
/// the render cost over the workers allows: past it the pictures are not told apart.
const STEP_FLOOR_MS: i64 = 33;
/// The step is not stretched past this however slow the render, so a pathological sample cannot
/// freeze the plan.
const STEP_CEIL_MS: i64 = 250;
/// One render's cost in the mean is capped here: a stall is not what a frame costs.
const COST_CEIL_US: u32 = 500_000;
/// The render cost assumed before the first sample (a television; a host only does better).
const COST_DEFAULT_MS: i64 = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// What the screen needs now: always beats an Ahead job.
    Normal,
    /// The boundary after the published frame, rendered early. `after` is that frame's `pub_id`.
    Ahead { after: u64 },
    /// A frame of a moving cue for a future playhead, planned by the animated pipeline; `run` is
    /// the pipeline generation it was planned under, and a result of another one is discarded.
    Pipe { run: u64 },
}

impl Kind {
    /// Rendered ahead of the playhead rather than for what the screen needs now.
    pub fn is_ahead(self) -> bool {
        !matches!(self, Kind::Normal)
    }
}

pub(super) struct Job {
    pub kind: Kind,
    pub key: Key,
    pub now_ms: i64,
    pub seq: u64,
    pub source: Arc<Source>,
    /// A Normal job enqueued while an Ahead job was rendering: when it began waiting (R6).
    pub queued: Option<Instant>,
    /// The newest frame that had landed when the job was taken: the renderer shares the regions it
    /// draws identically with that frame's, so a static sign keeps the allocation the screen has
    /// already uploaded whichever worker renders the next frame. Set when a worker takes the job.
    pub reference: Option<Arc<Frame>>,
    /// The size libass draws the frame at, as a fraction of the output (issue #1328).
    pub scale: RenderScale,
}

/// A job a worker holds. The mailbox serves several workers, so these are a set keyed by `seq`.
#[derive(Clone, Copy)]
struct InFlight {
    kind: Kind,
    seq: u64,
    /// The playhead the job renders.
    target_ms: i64,
    /// Results for a target at or past this are discarded: events were appended from there on
    /// since the job began, and the frame cannot show them.
    stale_from: i64,
}

pub(super) struct Published {
    /// Identity of this publication, for `Ahead { after }`.
    pub pub_id: u64,
    pub epoch: u64,
    /// The key it was rendered for. It may trail the latest request's by revision alone (the newer
    /// job is still out, or the pipeline adopted the revision), and then no rule that asks
    /// `key == requested` takes the frame as covering the request: `request` answers R2.5 until
    /// the newer revision's own frame lands.
    pub key: Key,
    pub frame: Arc<Frame>,
    pub validity: Validity,
    /// The alignment classes of the cues active at the playhead the frame was rendered for; only
    /// a Text-origin source has them (plan §2.4). Held beside the frame, so a promoted ahead frame
    /// carries its own and the two are never read from different frames.
    pub aligns: Option<AlignSet>,
    /// The playhead the frame was rendered for: what a moving frame is right for.
    pub target_ms: i64,
    /// The picture past `validity.until_ms` moves as well ([`Rendered::moves_on`]).
    pub moves_on: bool,
}

impl Published {
    /// The playhead from which the frame no longer stands for the clock, besides its age.
    ///
    /// A moving frame's window ends at the next event boundary, but a boundary INSIDE a moving
    /// stretch (an event starts or ends while others still move) is one the plan resolves at its
    /// own step, as it resolves everything else about the picture: a karaoke opening has an event
    /// boundary every ~19 ms, far inside one render, and a frame that died at each of them would
    /// never be shown or covered the screen (issue #1328). So such a frame stands until a newer
    /// one replaces it or its age runs out ([`Mailbox::max_age_ms`]). Only the end of the stretch,
    /// where the picture stops moving, is a boundary the frame ends at, and the plan renders that
    /// one at its own playhead.
    fn ends_at(&self) -> i64 {
        if self.validity.animated && self.moves_on {
            i64::MAX
        } else {
            self.validity.until_ms
        }
    }
}

/// What the worker made of a job.
pub(super) struct Rendered {
    pub frame: Arc<Frame>,
    pub validity: Validity,
    pub aligns: Option<AlignSet>,
    /// What the job cost in wall-clock microseconds, for the pipeline's pacing; `None` when it is
    /// no sample of a steady render (the worker was not timing it, or the job opened or re-synced a
    /// renderer, which re-reads every font and says nothing of what a frame costs).
    pub cost_us: Option<u32>,
    /// The picture at the end of the frame's window moves too: the boundary is inside a moving
    /// stretch, not its end ([`Published::ends_at`]).
    pub moves_on: bool,
}

/// What the pipeline plans against: the pace and the playhead of the latest request. The worker
/// takes its next frame between ticks, so this is a value the mailbox was handed, not a clock it reads.
#[derive(Clone, Copy)]
struct Plan {
    pace: Pace,
    now_ms: i64,
}

/// The frame the next one is planned after.
enum Frontier<'a> {
    /// Rendered and known: its validity says where a boundary falls.
    Landed(&'a Published),
    /// Still rendering: only its playhead is known.
    Flying(i64),
}

pub(super) struct Mailbox {
    /// ONE slot, a Normal job or the Ahead job of a still cue's next boundary.
    pending: Option<Job>,
    /// Set when a worker takes a job, cleared on completion: one per busy worker.
    in_flight: Vec<InFlight>,
    /// Seq of an in-flight NORMAL job a covered request made moot; its result is discarded.
    withdrawn: u64,
    /// The latest NORMAL job: key, now, seq. Ahead jobs never write it. A job's result is
    /// published when its key shares this key's timeline, whatever its seq (`finish`).
    requested: Option<(Key, i64, u64)>,
    /// The source of the latest request: what a frame planned now is rendered from, and what the
    /// next revision is compared with to find where it can change a picture.
    requested_source: Option<Arc<Source>>,
    /// Seq of the latest NORMAL job the worker completed (published or discarded) or that was
    /// withdrawn while pending.
    answered: u64,
    pub published: Option<Published>,
    /// At most ONE ahead frame of a still cue.
    next: Option<Published>,
    /// The frames of a moving cue that landed ahead of their playhead, by playhead.
    held: Vec<Published>,
    /// The pipeline generation: a `Pipe` job of another one is discarded when it lands.
    run: u64,
    /// Set while the pipeline plans; `None` while paused, still or waiting for a Normal job.
    plan: Option<Plan>,
    pub_ids: u64,
    seqs: u64,
    /// How many workers serve this mailbox: a moving cue's frame step is the render cost over this.
    workers: usize,
    /// What a frame costs at FULL size, the mean the pipeline paces by, wall-clock microseconds (0 =
    /// no sample yet). A frame drawn smaller is taken back to full size by its rung's cost ratio
    /// before it joins the mean, so the cost does not change with the scale it is measured at.
    cost_us: u32,
    /// How many renderers the pool allows the selection, once it has said: what the scale is
    /// decided against, and nothing is decided before it has.
    capacity: Option<usize>,
    /// The scale a moving stretch's frames are drawn at.
    governor: Governor,
}

impl Default for Mailbox {
    fn default() -> Self {
        Self::new()
    }
}

/// What a `request` did, for the caller to act on outside the lock logic.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub(super) struct Requested {
    /// A job was queued: wake the worker.
    pub wake: bool,
    /// A held frame was promoted: the still cue's boundary frame (`ahead_hit`), or a frame of the
    /// pipeline (`piped`).
    pub promoted: bool,
    /// The promoted frame was one the pipeline planned (`shown`).
    pub piped: bool,
    /// ... and it was promoted more than a tick after its playhead: it landed late (`late`).
    pub late: bool,
    /// How many workers the pipeline could use to reach about thirty frames a second; 0 when it
    /// plans nothing. The runtime starts helpers up to what the device allows.
    pub workers_wanted: usize,
    /// How many renderers the plan would want if it drew at full size: the pool is consulted about
    /// the selection when this is more than one (`Runtime::start_helpers`), and the scale is not
    /// decided before it has answered.
    pub pool_wanted: usize,
    /// The scale moving frames are drawn at moved with this request (to be logged once).
    pub scale_change: Option<Change>,
}

/// What finishing a job published, for the worker's accounting.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Finished {
    /// A Normal job published a frame whose pixels differ from the one it replaced: the screen
    /// has something new, wake it.
    pub wake: bool,
    /// The frame the job produced (published, or held ahead) differs from the one on screen,
    /// for the `assperf` account.
    pub changed: bool,
    pub pub_bytes: u64,
    /// An ahead frame was thrown away: its timeline, run or target no longer stands (`assperf`'s `stale=`).
    pub stale: bool,
}

/// `ceil` for a millisecond count computed in floating point: a product that is exact on paper
/// must not round up for the last bit of a representation error.
fn ceil_ms(x: f64) -> i64 {
    (x - 1e-6).ceil() as i64
}

impl Mailbox {
    pub const fn new() -> Self {
        Self {
            pending: None,
            in_flight: Vec::new(),
            withdrawn: 0,
            requested: None,
            requested_source: None,
            answered: 0,
            published: None,
            next: None,
            held: Vec::new(),
            run: 0,
            plan: None,
            pub_ids: 0,
            seqs: 0,
            workers: 1,
            cost_us: 0,
            capacity: None,
            governor: Governor::new(),
        }
    }

    /// How many workers take jobs from this mailbox (at least one).
    pub fn set_workers(&mut self, workers: usize) {
        self.workers = workers.max(1);
    }

    /// How many renderers the pool allows the selection, once it has said (`None` before): what the
    /// scale is decided against. Nothing is decided while it has not.
    pub fn set_capacity(&mut self, renderers: Option<usize>) {
        self.capacity = renderers;
    }


    /// Everything the mailbox holds, handed out whole so it can be destroyed outside the lock, and
    /// a fresh mailbox left in its place that keeps what is not a thing to retire: the worker
    /// count, the cost it measured and the counters (a job another worker still holds finishes
    /// against the new mailbox, and its `seq` must not be one the new mailbox hands out).
    pub fn retire(&mut self) -> Mailbox {
        let mut fresh = Mailbox::new();
        fresh.workers = self.workers;
        fresh.cost_us = self.cost_us;
        fresh.pub_ids = self.pub_ids;
        fresh.seqs = self.seqs;
        fresh.run = self.run + 1;
        std::mem::replace(self, fresh)
    }

    /// Nothing held: a worker with no source may park without cleanup.
    pub fn is_empty(&self) -> bool {
        self.pending.is_none()
            && self.in_flight.is_empty()
            && self.requested.is_none()
            && self.published.is_none()
            && self.next.is_none()
            && self.held.is_empty()
    }

    /// A Normal job is pending or in flight (or was, and its result is still awaited).
    fn normal_outstanding(&self) -> bool {
        self.requested.is_some_and(|(_, _, seq)| seq != self.answered)
    }

    fn requested_key(&self) -> Option<Key> {
        self.requested.map(|(key, _, _)| key)
    }

    /// Queue a Normal job: what the screen needs now, drawn at full size. (A hole inside a reduced
    /// stretch is recovered at the stretch's own size: [`Mailbox::enqueue_normal_at`].)
    fn enqueue_normal(&mut self, key: Key, now_ms: i64, source: &Arc<Source>) {
        self.enqueue_normal_at(key, now_ms, source, RenderScale::Full);
    }

    fn enqueue_normal_at(&mut self, key: Key, now_ms: i64, source: &Arc<Source>, scale: RenderScale) {
        self.seqs += 1;
        self.requested = Some((key, now_ms, self.seqs));
        self.requested_source = Some(source.clone());
        // A Normal job this one replaces keeps its earlier stamp: the wait began then (R6).
        let queued = match self.pending.as_ref() {
            Some(Job { kind: Kind::Normal, queued: Some(since), .. }) => Some(*since),
            _ => (self.in_flight.len() >= self.workers && self.in_flight.iter().any(|f| f.kind.is_ahead()))
                .then(Instant::now),
        };
        self.pending = Some(Job {
            kind: Kind::Normal,
            key,
            now_ms,
            seq: self.seqs,
            source: source.clone(),
            queued,
            reference: None,
            scale,
        });
    }

    /// Withdraw the outstanding Normal job of the current key: the playhead is covered by
    /// `published`, so whatever it was asked to render is moot. True if anything was withdrawn.
    fn withdraw(&mut self) -> bool {
        let mut withdrew = false;
        if let Some(Job { kind: Kind::Normal, seq, .. }) = &self.pending {
            self.answered = self.answered.max(*seq);
            self.pending = None;
            withdrew = true;
        }
        if let Some((_, _, latest)) = self.requested {
            if self.in_flight.iter().any(|f| f.kind == Kind::Normal && f.seq == latest)
                && latest > self.answered
                && latest != self.withdrawn
            {
                self.withdrawn = latest;
                withdrew = true;
            }
        }
        withdrew
    }

    /// R4: the only place an Ahead job is created. True if one was queued.
    fn arm_ahead(&mut self, source: &Arc<Source>) -> bool {
        let Some(published) = &self.published else { return false };
        let until = published.validity.until_ms;
        if published.validity.animated || until == i64::MAX || self.next.is_some() {
            return false;
        }
        let after = published.pub_id;
        let same = |kind: Kind| kind == Kind::Ahead { after };
        if self.in_flight.iter().any(|f| same(f.kind)) {
            return false;
        }
        match &self.pending {
            None => {}
            Some(job) if job.kind == Kind::Normal => return false,
            Some(job) if same(job.kind) => return false,
            Some(_) => {}
        }
        self.seqs += 1;
        self.pending = Some(Job {
            kind: Kind::Ahead { after },
            key: published.key,
            now_ms: until,
            seq: self.seqs,
            source: source.clone(),
            queued: None,
            reference: None,
            scale: RenderScale::Full,
        });
        true
    }

    /// The frame step of the pipeline, in playhead milliseconds: what one render costs over the
    /// workers sharing it, scaled to the playhead's speed, never under a video frame or about
    /// thirty a second, and never past [`STEP_CEIL_MS`].
    pub fn step_ms(&self, pace: Pace) -> i64 {
        let per_frame = self.cost_ms() * pace.rate / self.workers as f64;
        ceil_ms(per_frame).max(pace.frame_ms.max(STEP_FLOOR_MS)).min(STEP_CEIL_MS)
    }

    /// How far ahead of the playhead a moving frame is planned, in playhead milliseconds: a
    /// render and a quarter more for the variation of one, and a tick for the wait for the tick
    /// that shows it. Workers do not change it: one render takes as long whoever does it.
    pub fn lead_ms(&self, pace: Pace) -> i64 {
        ceil_ms((self.cost_ms() * 1.25 + pace.tick_ms as f64) * pace.rate)
    }

    /// How many workers the plan could use: enough that the frame step reaches the thirty-a-second
    /// floor at the cost measured, drawing at the scale it does (more would render frames the step
    /// never asks for).
    pub fn workers_wanted(&self, pace: Pace) -> usize {
        self.workers_at(pace, self.governor.scale())
    }

    /// How many workers the plan would want drawing at full size: what the pool is asked about, since
    /// the scale is what it can make of the answer.
    pub fn pool_wanted(&self, pace: Pace) -> usize {
        self.workers_at(pace, RenderScale::Full)
    }

    fn workers_at(&self, pace: Pace, scale: RenderScale) -> usize {
        let floor = pace.frame_ms.max(STEP_FLOOR_MS) as f64;
        ((self.cost_full_ms() * scale.cost_ratio() * pace.rate / floor) - 1e-6).ceil().max(1.0) as usize
    }

    /// The oldest a held or published moving frame may be and still stand for the playhead: a
    /// frame later than this is a hole, and the plan is restarted.
    fn max_age_ms(&self, pace: Pace) -> i64 {
        self.lead_ms(pace) + 2 * self.step_ms(pace)
    }

    /// What a frame costs at full size, in milliseconds.
    pub fn cost_full_ms(&self) -> f64 {
        if self.cost_us == 0 {
            COST_DEFAULT_MS as f64
        } else {
            f64::from(self.cost_us) / 1000.0
        }
    }

    /// What a frame of the moving stretch costs now, at the scale it is drawn at.
    fn cost_ms(&self) -> f64 {
        self.cost_full_ms() * self.governor.scale().cost_ratio()
    }

    /// A sample of what a render drawn at `scale` costs, into the mean the pipeline paces by (a
    /// quarter of each new one; the first seeds it), taken back to full size first. The first
    /// sample of a moving stretch replaces the mean: what the last stretch cost says nothing of
    /// this one.
    fn note_cost(&mut self, sample_us: u32, scale: RenderScale) {
        let full = (f64::from(sample_us) / scale.cost_ratio()).round() as u32;
        let sample = full.clamp(1, COST_CEIL_US);
        let first = self.governor.is_fresh();
        self.cost_us = if self.cost_us == 0 || first {
            sample
        } else {
            ((u64::from(self.cost_us) * 3 + u64::from(sample) + 2) / 4) as u32
        };
        // only a frame of the rung in use says how that rung is doing; a frame of another one (a
        // Normal job's, one that was in flight over a move) feeds the mean and no more
        if first || scale == self.governor.scale() {
            self.governor.note_sample();
        }
    }

    /// The scale for the next frames of the running pipeline, when the pool has said what it has.
    fn decide_scale(&mut self, now_ms: i64, pace: Pace) -> Option<Change> {
        let renderers = self.capacity?;
        let load = Load {
            cost_full_ms: self.cost_full_ms(),
            renderers,
            rate: pace.rate,
            target_ms: pace.frame_ms.max(STEP_FLOOR_MS),
        };
        self.governor.observe(now_ms, load)
    }

    /// A still picture is on screen: the next moving frame begins a stretch (issue #1328).
    fn picture_shown(&mut self) {
        if self.published.as_ref().is_some_and(|p| !p.validity.animated) {
            self.governor.begin_stretch();
        }
    }

    /// The pipeline is the way to show the moving frame on screen: the player is running and the
    /// frame published for this timeline moves with the clock.
    fn piping(&self, key: &Key, pace: Pace) -> bool {
        pace.running
            && self.published.as_ref().is_some_and(|p| p.validity.animated && p.key.same_timeline(key))
    }

    /// Throw the plan away: the frames held, the jobs in flight (they are discarded when they
    /// land, for the generation moved on) and the plan itself.
    fn stop_pipe(&mut self) {
        self.held.clear();
        self.run += 1;
        self.plan = None;
    }

    /// The first frame the plan may have jobs for after the ones it has: the greatest playhead
    /// among the frames held and the frames in flight, else the one on screen.
    fn frontier(&self) -> Option<Frontier<'_>> {
        let flying = self
            .in_flight
            .iter()
            .filter(|f| matches!(f.kind, Kind::Pipe { run } if run == self.run) && f.target_ms < f.stale_from)
            .map(|f| f.target_ms)
            .max();
        match (self.held.last(), flying) {
            (Some(held), Some(flying)) if flying > held.target_ms => Some(Frontier::Flying(flying)),
            (Some(held), _) => Some(Frontier::Landed(held)),
            (None, Some(flying)) => Some(Frontier::Flying(flying)),
            (None, None) => self.published.as_ref().filter(|p| p.validity.animated).map(Frontier::Landed),
        }
    }

    /// The playhead of the next frame to plan, `None` when there is nothing more to plan yet.
    ///
    /// One step after the frontier, but never closer than a `lead` to the playhead: a render
    /// started now lands by then. A frame that landed knows where its picture ends, and a
    /// frontier whose next step would pass that boundary plans the boundary itself instead, so
    /// the end of a moving stretch is rendered at its own playhead (a boundary inside the stretch
    /// is not one: [`Published::ends_at`]). A still picture after a moving one needs no more
    /// frames, and nothing is planned beyond the horizon.
    fn next_target(&self) -> Option<i64> {
        let Plan { pace, now_ms } = self.plan?;
        let (step, lead) = (self.step_ms(pace), self.lead_ms(pace));
        let floor = now_ms + lead;
        let horizon = floor + self.workers as i64 * step;
        if self.held.iter().any(|f| !f.validity.animated && f.validity.until_ms == i64::MAX) {
            // the still picture after the cue has landed
            return None;
        }
        let target = match self.frontier() {
            None => floor,
            Some(Frontier::Flying(at)) => (at + step).max(floor),
            Some(Frontier::Landed(frame)) if frame.validity.animated => {
                let target = (frame.target_ms + step).max(floor);
                if frame.ends_at() < target {
                    frame.ends_at().max(now_ms)
                } else {
                    target
                }
            }
            Some(Frontier::Landed(frame)) => match frame.validity.until_ms {
                i64::MAX => return None,
                until => until.max(now_ms),
            },
        };
        (target <= horizon).then_some(target)
    }

    /// A worker is idle and could be given a job: the wake to send.
    fn wants_job(&self) -> bool {
        self.in_flight.len() < self.workers && (self.pending.is_some() || self.next_target().is_some())
    }

    /// A revision of the timeline the pipeline is running appended events: keep the plan for every
    /// playhead before the first of them (appended events start ahead of the playhead, so a frame
    /// of an earlier playhead is right whatever was added) and replan from there. Not for a
    /// revision that reaches the playhead or that the renderer cannot take as an append.
    fn adopt_revision(&mut self, key: Key, now_ms: i64, source: &Arc<Source>, pace: Pace) -> bool {
        let Some(old) = self.requested_key() else { return false };
        if !pace.running || !key.same_timeline(&old) || key.revision <= old.revision {
            return false;
        }
        // With a Normal job out the plan is stopped and its frame will start it anew; with none, the
        // frame on screen has to be one the appended events leave right.
        let moving = self.published.as_ref().is_some_and(|p| p.validity.animated && p.key.same_timeline(&key));
        if !self.normal_outstanding() && !moving {
            return false;
        }
        let Some(earliest) = self.requested_source.as_ref().map(|old| earliest_change_ms(old, source)) else {
            return false;
        };
        if earliest <= now_ms {
            return false;
        }
        self.requested = self.requested.map(|(_, now, seq)| (key, now, seq));
        self.requested_source = Some(source.clone());
        self.held.retain(|f| f.target_ms < earliest);
        for flight in self.in_flight.iter_mut().filter(|f| f.kind != Kind::Normal) {
            flight.stale_from = flight.stale_from.min(earliest);
        }
        true
    }

    /// R2: MAIN THREAD, under the lock. `now_ms` is the playhead this tick asks about; `pace` is
    /// how it is moving.
    pub fn request(&mut self, key: Key, now_ms: i64, source: &Arc<Source>, pace: Pace) -> Requested {
        // R2.1: a new key (late push, seek, restyle, size change), but for a revision the pipeline
        // takes in.
        if self.requested_key() != Some(key) && !self.adopt_revision(key, now_ms, source, pace) {
            if self.requested_key().is_none_or(|old| !old.same_timeline(&key)) {
                // another selection (or size): the rung was decided for the last one, and the pool has
                // not said what this one may have
                self.governor = Governor::new();
            }
            self.enqueue_normal(key, now_ms, source);
            self.next = None;
            self.stop_pipe();
            return Requested { wake: true, ..Requested::default() };
        }
        if self.piping(&key, pace) {
            return self.request_piping(key, now_ms, source, pace);
        }
        if self.plan.is_some() {
            // the player stopped running, or the moving picture stopped moving
            self.stop_pipe();
        }
        let lagging = self.published.as_ref().is_some_and(|p| p.key != key && p.key.same_timeline(&key));
        if lagging && !self.normal_outstanding() {
            // The pipeline took revisions in and left no Normal job behind: one frame under this
            // revision hands over to the rules below, which wait for the key's own first frame.
            self.enqueue_normal(key, now_ms, source);
            return Requested { wake: true, ..Requested::default() };
        }
        self.request_static(key, now_ms, source)
    }

    /// R2.2-R2.5: a still cue, or a moving one for a clock that is not running.
    fn request_static(&mut self, key: Key, now_ms: i64, source: &Arc<Source>) -> Requested {
        let Some(published) = self.published.as_ref().filter(|p| p.key == key) else {
            // R2.5: the key's first frame is still in flight.
            return Requested::default();
        };
        let animated = published.validity.animated;
        let mut out = Requested::default();
        if !animated && published.validity.contains(now_ms) && published.frame.scale != RenderScale::Full {
            // The still picture after a reduced stretch is drawn smaller than the output: it is drawn
            // again at full size, and the reduced frame stands until that lands. It is not "covered"
            // (below), which would withdraw that job.
            if !self.normal_outstanding() {
                self.enqueue_normal(key, now_ms, source);
                out.wake = true;
            }
            return out;
        }
        if !animated && published.validity.contains(now_ms) {
            // R2.2: covered.
            if self.withdraw() {
                out.wake = self.arm_ahead(source);
            }
            return out;
        }
        if !animated
            && self.next.as_ref().is_some_and(|n| {
                n.key == key && n.epoch == key.epoch && n.validity.contains(now_ms)
            })
        {
            // R2.3: promote the frame rendered ahead, keeping its pub_id.
            self.published = self.next.take();
            self.picture_shown();
            self.withdraw();
            out.promoted = true;
            out.wake = self.arm_ahead(source);
            return out;
        }
        // R2.4: a jump in `now` inside one key, or an animated frame whose clock moved.
        let latest_now = self.requested.map(|(_, now, _)| now);
        if (!animated || latest_now != Some(now_ms)) && !self.normal_outstanding() {
            self.enqueue_normal(key, now_ms, source);
            out.wake = true;
        }
        out
    }

    /// Whether a held frame can be shown at `now_ms`: a moving one from its own playhead on, for
    /// as long as a plan's lateness allows and before the boundary that ends its stretch
    /// ([`Published::ends_at`]); a still one inside its window.
    fn showable(frame: &Published, now_ms: i64, max_age: i64) -> bool {
        if frame.validity.animated {
            (frame.target_ms..frame.ends_at()).contains(&now_ms) && now_ms - frame.target_ms <= max_age
        } else {
            frame.validity.contains(now_ms)
        }
    }

    /// R2 for a moving cue while the player runs: promote the frame whose playhead has come, keep
    /// the plan going, or give it up for one Normal job if the screen would otherwise be left
    /// without a picture for the playhead.
    fn request_piping(&mut self, key: Key, now_ms: i64, source: &Arc<Source>, pace: Pace) -> Requested {
        let mut out = Requested::default();
        if self.normal_outstanding() {
            // the pipeline starts again from the frame that job is rendering
            self.plan = None;
            return out;
        }
        self.plan = Some(Plan { pace, now_ms });
        out.scale_change = self.decide_scale(now_ms, pace);
        let (lead, step, max_age) = (self.lead_ms(pace), self.step_ms(pace), self.max_age_ms(pace));
        // what can no longer be shown: past its boundary or too late
        self.held.retain(|f| {
            let past = f.ends_at() <= now_ms || (f.validity.animated && f.target_ms + max_age < now_ms);
            !past
        });
        let on_screen = self.published.as_ref().map_or(i64::MIN, |p| p.target_ms);
        let pick = self.held.iter().rposition(|f| f.target_ms > on_screen && Self::showable(f, now_ms, max_age));
        if let Some(at) = pick {
            let frame = self.held.remove(at);
            self.held.drain(..at.min(self.held.len()));
            // frames older than the one shown are of no use
            self.held.retain(|f| f.target_ms > frame.target_ms);
            let moving = frame.validity.animated;
            // a frame is on time when the first tick at or after its playhead shows it
            out.late = moving && now_ms - frame.target_ms > ceil_ms(pace.tick_ms as f64 * pace.rate);
            self.published = Some(frame);
            out.promoted = true;
            out.piped = true;
            if !moving {
                // the cue ended: its still picture is shown, and the boundary logic takes over
                self.picture_shown();
                self.stop_pipe();
                let lagging = self.published.as_ref().is_some_and(|p| p.key != key);
                if lagging {
                    self.enqueue_normal(key, now_ms, source);
                    out.wake = true;
                } else {
                    out.wake = self.arm_ahead(source);
                }
                return out;
            }
        }
        let Some(published) = self.published.as_ref() else { return out };
        let slack = pace.frame_ms;
        let covered = published.target_ms - now_ms <= slack
            && now_ms - published.target_ms <= max_age
            && now_ms < published.ends_at();
        if !covered {
            let (lo, hi) = (now_ms - max_age, now_ms + lead + (self.workers as i64 + 1) * step);
            let coming = self.held.iter().any(|f| (lo..=hi).contains(&f.target_ms))
                || self.in_flight.iter().any(|f| {
                    matches!(f.kind, Kind::Pipe { run } if run == self.run)
                        && f.target_ms < f.stale_from
                        && (lo..=hi).contains(&f.target_ms)
                });
            if !coming {
                // a hole: nothing on screen is right and nothing is on its way. The frame that fills
                // it is drawn at the size the stretch is: a frame at another size makes libass empty
                // its caches, and the pipeline's next frame would pay for them again.
                self.enqueue_normal_at(key, now_ms, source, self.governor.scale());
                self.stop_pipe();
                out.wake = true;
                return out;
            }
        }
        out.wake = self.wants_job();
        out.workers_wanted = self.workers_wanted(pace);
        out.pool_wanted = self.pool_wanted(pace);
        out
    }

    /// The alignment classes of the published frame whose `serial` is asked for; `None` when the
    /// published frame is another one, or has none.
    pub fn aligns_of(&self, serial: u64) -> Option<AlignSet> {
        self.published.as_ref().filter(|p| p.frame.serial == serial)?.aligns
    }

    /// The frame to show under today's epoch/source/size test.
    pub fn view(&self, key: &Key) -> Option<Arc<Frame>> {
        let p = self.published.as_ref()?;
        (p.epoch == key.epoch
            && p.frame.source_id == key.source_id
            && p.frame.width == key.width
            && p.frame.height == key.height)
            .then(|| p.frame.clone())
    }

    /// The start-failure path: nothing can render, so an error frame stands for the key.
    pub fn publish_error(&mut self, key: Key, frame: Arc<Frame>) -> Arc<Frame> {
        if let Some(p) = self.published.as_ref().filter(|p| {
            p.epoch == key.epoch && p.frame.width == key.width && p.frame.height == key.height
        }) {
            return p.frame.clone();
        }
        self.pub_ids += 1;
        self.published = Some(Published {
            pub_id: self.pub_ids,
            epoch: key.epoch,
            key,
            frame: frame.clone(),
            validity: Validity::ALWAYS,
            aligns: None,
            target_ms: 0,
            moves_on: false,
        });
        frame
    }

    /// Whether `frame` differs from the published one, and the RGBA bytes it carries if so.
    fn differs(&self, frame: &Frame, epoch: u64) -> (bool, u64) {
        let changed = self
            .published
            .as_ref()
            .is_none_or(|old| old.epoch != epoch || old.frame.serial != frame.serial);
        let bytes = if changed { frame.rects.iter().map(|r| r.rgba.len() as u64).sum() } else { 0 };
        (changed, bytes)
    }

    /// WORKER, under the lock: take a job and mark it in flight. The pending one first (a Normal
    /// job, or a still cue's Ahead job); else, while the pipeline plans, the next frame of the
    /// moving cue. A worker asks only when it is idle, so jobs are never more than workers.
    pub fn take(&mut self) -> Option<Job> {
        let mut job = match self.pending.take() {
            Some(job) => job,
            None => {
                let now_ms = self.next_target()?;
                let (key, source) = (self.requested_key()?, self.requested_source.clone()?);
                self.seqs += 1;
                let scale = self.governor.scale();
                Job { kind: Kind::Pipe { run: self.run }, key, now_ms, seq: self.seqs, source, queued: None, reference: None, scale }
            }
        };
        // The newest picture that has landed: the frame this one follows on screen, or close to it.
        job.reference = self.held.last().or(self.published.as_ref()).map(|p| p.frame.clone());
        self.in_flight.push(InFlight { kind: job.kind, seq: job.seq, target_ms: job.now_ms, stale_from: i64::MAX });
        Some(job)
    }

    /// WORKER, under the lock: R3 and R5. `outcome` is `None` when the job did not render or
    /// its epoch ended; the job is finished either way, so no counter can stick.
    ///
    /// A Normal result is published unless the job was withdrawn or its timeline (epoch, source,
    /// size) is no longer the latest request's. A job whose key differs from the latest request's
    /// by revision alone still publishes, under its own key: appended events start ahead of the
    /// playhead, so the frame stays right for its time, and on a slow set one render outlives
    /// several appends (issue #1328). Such a frame arms no Ahead job and leaves the newer pending
    /// job alone; that job publishes the newest revision when it lands.
    ///
    /// A `Pipe` result is held for its playhead when the plan it was made under still stands: the
    /// generation, the timeline, the first appended event and the frame on screen all allow it.
    pub fn finish(&mut self, job: &Job, outcome: Option<Rendered>) -> Finished {
        // A job this mailbox does not hold was handed out before the mailbox was retired (another
        // worker's, the epoch having ended): there is nothing of it to settle.
        let Some(at) = self.in_flight.iter().position(|f| f.seq == job.seq) else {
            return Finished::default();
        };
        let flight = self.in_flight.swap_remove(at);
        // The pipeline paces by what a MOVING frame costs: a still picture, an empty one or an error
        // costs next to nothing and says nothing of the frames the pipeline is for.
        if let Some(cost_us) = outcome.as_ref().filter(|o| o.validity.animated).and_then(|o| o.cost_us) {
            self.note_cost(cost_us, job.scale);
        }
        match job.kind {
            Kind::Normal => {
                self.answered = self.answered.max(job.seq);
                let Some(Rendered { frame, validity, aligns, moves_on, .. }) = outcome else { return Finished::default() };
                // A result stands unless the playhead was covered without it (`withdrawn`) or the
                // timeline it was rendered for was replaced. A revision that moved meanwhile only
                // appended events, which start ahead of the playhead, so the frame is still right
                // for its time; on a slow set a render outlives several appends, and holding the
                // frame back until one lands in a quiet gap shows nothing for a whole opening.
                let timeline = self.requested.is_some_and(|(key, _, _)| key.same_timeline(&job.key));
                if job.seq == self.withdrawn || !timeline {
                    return Finished::default();
                }
                let (changed, pub_bytes) = self.differs(&frame, job.key.epoch);
                self.pub_ids += 1;
                self.published = Some(Published {
                    pub_id: self.pub_ids,
                    epoch: job.key.epoch,
                    key: job.key,
                    frame,
                    validity,
                    aligns,
                    target_ms: job.now_ms,
                    moves_on,
                });
                self.picture_shown();
                self.next = None;
                self.held.clear();
                // Only the newest request's frame has a boundary worth rendering ahead; the
                // newer job still pending will arm it when it lands.
                if self.requested_key() == Some(job.key) {
                    self.arm_ahead(&job.source);
                }
                Finished { wake: changed, changed, pub_bytes, stale: false }
            }
            Kind::Ahead { after } => {
                let Some(Rendered { frame, validity, aligns, moves_on, .. }) = outcome else { return Finished::default() };
                let current = self.published.as_ref().is_some_and(|p| p.pub_id == after && p.key == job.key)
                    && self.requested_key() == Some(job.key)
                    && self.next.is_none();
                if !current {
                    return Finished { stale: true, ..Finished::default() };
                }
                let (changed, pub_bytes) = self.differs(&frame, job.key.epoch);
                self.pub_ids += 1;
                self.next = Some(Published {
                    pub_id: self.pub_ids,
                    epoch: job.key.epoch,
                    key: job.key,
                    frame,
                    validity,
                    aligns,
                    target_ms: job.now_ms,
                    moves_on,
                });
                // Nothing to show yet: the promotion on the UI thread is what the screen sees.
                Finished { wake: false, changed, pub_bytes, stale: false }
            }
            Kind::Pipe { run } => {
                let Some(Rendered { frame, validity, aligns, moves_on, .. }) = outcome else { return Finished::default() };
                let stale = Finished { stale: true, ..Finished::default() };
                let on_screen = self.published.as_ref().filter(|p| p.validity.animated && p.key.same_timeline(&job.key));
                let standing = run == self.run
                    && self.requested_key().is_some_and(|key| key.same_timeline(&job.key))
                    && job.now_ms < flight.stale_from
                    && on_screen.is_some_and(|p| p.target_ms < job.now_ms);
                let at = self.held.partition_point(|f| f.target_ms < job.now_ms);
                if !standing || self.held.get(at).is_some_and(|f| f.target_ms == job.now_ms) {
                    return stale;
                }
                // different from the frame before it in time, which is what is on screen when it is shown
                let before = at.checked_sub(1).and_then(|i| self.held.get(i)).or(self.published.as_ref());
                let changed = before.is_none_or(|old| old.frame.serial != frame.serial);
                let pub_bytes = if changed { frame.rects.iter().map(|r| r.rgba.len() as u64).sum() } else { 0 };
                self.pub_ids += 1;
                self.held.insert(
                    at,
                    Published {
                        pub_id: self.pub_ids,
                        epoch: job.key.epoch,
                        key: job.key,
                        frame,
                        validity,
                        aligns,
                        target_ms: job.now_ms,
                        moves_on,
                    },
                );
                // Nothing to show yet: the tick at its playhead is what the screen sees.
                Finished { wake: false, changed, pub_bytes, stale: false }
            }
        }
    }

    /// The scale a moving stretch's frames are drawn at now. Everything else is drawn at full size:
    /// a still picture, a paused one, the first frame of a stretch and any job that is not the
    /// pipeline's (`take`).
    #[cfg(test)]
    pub fn scale(&self) -> RenderScale {
        self.governor.scale()
    }

    #[cfg(test)]
    pub fn held_targets(&self) -> Vec<i64> {
        self.held.iter().map(|f| f.target_ms).collect()
    }

    #[cfg(test)]
    pub fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    #[cfg(test)]
    pub fn pending_queued(&self) -> Option<Instant> {
        self.pending.as_ref().and_then(|j| j.queued)
    }

    #[cfg(test)]
    pub fn answered(&self) -> u64 {
        self.answered
    }

    #[cfg(test)]
    pub fn pending_job(&self) -> Option<(Kind, i64)> {
        self.pending.as_ref().map(|j| (j.kind, j.now_ms))
    }

    #[cfg(test)]
    pub fn next_frame(&self) -> Option<&Published> {
        self.next.as_ref()
    }
}
