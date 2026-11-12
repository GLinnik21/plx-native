//! The request/publish mailbox between the SDL thread and the render worker (plan section 4.2).
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
//! Pure bookkeeping: no thread, no native call, no clock but the one stamp behind [`Job::queued`].
//! `Runtime` owns the lock and the wake; the worker loop owns the rendering.
// SCAFFOLD: the pipeline's fields are written before the logic that reads them; removed with it.
#![allow(dead_code)]
use super::{AlignSet, Frame, Key, Pace, Source};
use crate::player::ass_timing::Validity;
use std::sync::Arc;
use std::time::Instant;

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
    /// job is still out), and then no rule that asks `key == requested` takes the frame as covering
    /// the request: `request` answers R2.5 until the newer revision's own frame lands.
    pub key: Key,
    pub frame: Arc<Frame>,
    pub validity: Validity,
    /// The alignment classes of the cues active at the playhead the frame was rendered for; only
    /// a Text-origin source has them (plan §2.4). Held beside the frame, so a promoted ahead frame
    /// carries its own and the two are never read from different frames.
    pub aligns: Option<AlignSet>,
    /// The playhead the frame was rendered for: what a moving frame is right for.
    pub target_ms: i64,
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
    /// Seq of the latest NORMAL job the worker completed (published or discarded) or that was
    /// withdrawn while pending.
    answered: u64,
    pub published: Option<Published>,
    /// At most ONE ahead frame.
    next: Option<Published>,
    pub_ids: u64,
    seqs: u64,
    /// How many workers serve this mailbox: a moving cue's frame step is the render cost over this.
    workers: usize,
    /// The render cost the pipeline paces by, wall-clock microseconds (0 = no sample yet).
    cost_us: u32,
}

impl Default for Mailbox {
    fn default() -> Self {
        Self::new()
    }
}

/// What a `request` did, for the caller to act on outside the lock logic.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Requested {
    /// A job was queued: wake the worker.
    pub wake: bool,
    /// The held ahead frame was promoted (`ahead_hit`).
    pub promoted: bool,
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

impl Mailbox {
    pub const fn new() -> Self {
        Self {
            pending: None,
            in_flight: Vec::new(),
            withdrawn: 0,
            requested: None,
            answered: 0,
            published: None,
            next: None,
            pub_ids: 0,
            seqs: 0,
            workers: 1,
            cost_us: 0,
        }
    }

    /// How many workers take jobs from this mailbox (at least one).
    pub fn set_workers(&mut self, workers: usize) {
        self.workers = workers.max(1);
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
        std::mem::replace(self, fresh)
    }

    /// Nothing held: a worker with no source may park without cleanup.
    pub fn is_empty(&self) -> bool {
        self.pending.is_none()
            && self.in_flight.is_empty()
            && self.requested.is_none()
            && self.published.is_none()
            && self.next.is_none()
    }

    /// A Normal job is pending or in flight (or was, and its result is still awaited).
    fn normal_outstanding(&self) -> bool {
        self.requested.is_some_and(|(_, _, seq)| seq != self.answered)
    }

    fn requested_key(&self) -> Option<Key> {
        self.requested.map(|(key, _, _)| key)
    }

    fn enqueue_normal(&mut self, key: Key, now_ms: i64, source: &Arc<Source>) {
        self.seqs += 1;
        self.requested = Some((key, now_ms, self.seqs));
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
        });
        true
    }

    /// R2: MAIN THREAD, under the lock. `now_ms` is the playhead this tick asks about.
    pub fn request(&mut self, key: Key, now_ms: i64, source: &Arc<Source>, _pace: Pace) -> Requested {
        // R2.1: a new key (late push, seek, restyle, size change).
        if self.requested_key() != Some(key) {
            self.enqueue_normal(key, now_ms, source);
            self.next = None;
            return Requested { wake: true, promoted: false };
        }
        let Some(published) = self.published.as_ref().filter(|p| p.key == key) else {
            // R2.5: the key's first frame is still in flight.
            return Requested::default();
        };
        let animated = published.validity.animated;
        let mut out = Requested::default();
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

    /// WORKER, under the lock: take the pending job and mark it in flight.
    pub fn take(&mut self) -> Option<Job> {
        let job = self.pending.take()?;
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
    pub fn finish(&mut self, job: &Job, outcome: Option<Rendered>) -> Finished {
        // A job this mailbox does not hold was handed out before the mailbox was retired (another
        // worker's, the epoch having ended): there is nothing of it to settle.
        let Some(at) = self.in_flight.iter().position(|f| f.seq == job.seq) else {
            return Finished::default();
        };
        let _flight = self.in_flight.swap_remove(at);
        match job.kind {
            Kind::Normal => {
                self.answered = self.answered.max(job.seq);
                let Some(Rendered { frame, validity, aligns, .. }) = outcome else { return Finished::default() };
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
                });
                self.next = None;
                // Only the newest request's frame has a boundary worth rendering ahead; the
                // newer job still pending will arm it when it lands.
                if self.requested_key() == Some(job.key) {
                    self.arm_ahead(&job.source);
                }
                Finished { wake: changed, changed, pub_bytes, stale: false }
            }
            Kind::Ahead { after } => {
                let Some(Rendered { frame, validity, aligns, .. }) = outcome else { return Finished::default() };
                let current = self.published.as_ref().is_some_and(|p| p.pub_id == after && p.key == job.key)
                    && self.requested_key() == Some(job.key)
                    && self.next.is_none();
                if !current {
                    return Finished::default();
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
                });
                // Nothing to show yet: the promotion on the UI thread is what the screen sees.
                Finished { wake: false, changed, pub_bytes, stale: false }
            }
            Kind::Pipe { .. } => Finished::default(),
        }
    }

    /// SCAFFOLD: the frame step in playhead milliseconds.
    pub fn step_ms(&self, _pace: Pace) -> i64 {
        0
    }

    /// SCAFFOLD: how far ahead of the playhead a moving frame is planned.
    pub fn lead_ms(&self, _pace: Pace) -> i64 {
        0
    }

    #[cfg(test)]
    pub fn held_targets(&self) -> Vec<i64> {
        Vec::new()
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
