//! The request/publish mailbox between the SDL thread and the render worker (plan section 4.2).
//!
//! A frame is published with the window of playheads it stays right for ([`Validity`]), so a UI
//! tick inside the window costs no job. The worker also renders the next boundary ahead of its
//! time (an `Ahead` job), and the next tick at or after that boundary promotes the held frame.
//!
//! Pure bookkeeping: no thread, no native call, no clock but the one stamp behind [`Job::queued`].
//! `Runtime` owns the lock and the wake; the worker loop owns the rendering.
use super::{AlignSet, Frame, Key, Source};
use crate::player::ass_timing::Validity;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// What the screen needs now: always beats an Ahead job.
    Normal,
    /// The boundary after the published frame, rendered early. `after` is that frame's `pub_id`.
    Ahead { after: u64 },
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

#[derive(Clone, Copy)]
struct InFlight {
    kind: Kind,
    seq: u64,
}

pub(super) struct Published {
    /// Identity of this publication, for `Ahead { after }`.
    pub pub_id: u64,
    pub epoch: u64,
    pub key: Key,
    pub frame: Arc<Frame>,
    pub validity: Validity,
    /// The alignment classes of the cues active at the playhead the frame was rendered for; only
    /// a Text-origin source has them (plan §2.4). Held beside the frame, so a promoted ahead frame
    /// carries its own and the two are never read from different frames.
    pub aligns: Option<AlignSet>,
}

/// What the worker made of a job.
pub(super) struct Rendered {
    pub frame: Arc<Frame>,
    pub validity: Validity,
    pub aligns: Option<AlignSet>,
}

pub(super) struct Mailbox {
    /// ONE slot, either kind.
    pending: Option<Job>,
    /// Set when the worker takes `pending`, cleared on completion.
    in_flight: Option<InFlight>,
    /// Seq of an in-flight NORMAL job a covered request made moot; its result is discarded.
    withdrawn: u64,
    /// The latest NORMAL job: key, now, seq. Ahead jobs never write it.
    requested: Option<(Key, i64, u64)>,
    /// Seq of the latest NORMAL job the worker completed (published or discarded) or that was
    /// withdrawn while pending.
    answered: u64,
    pub published: Option<Published>,
    /// At most ONE ahead frame.
    next: Option<Published>,
    pub_ids: u64,
    seqs: u64,
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
}

impl Mailbox {
    pub const fn new() -> Self {
        Self {
            pending: None,
            in_flight: None,
            withdrawn: 0,
            requested: None,
            answered: 0,
            published: None,
            next: None,
            pub_ids: 0,
            seqs: 0,
        }
    }

    /// Nothing held: a worker with no source may park without cleanup.
    pub fn is_empty(&self) -> bool {
        self.pending.is_none()
            && self.in_flight.is_none()
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
            _ => matches!(self.in_flight, Some(InFlight { kind: Kind::Ahead { .. }, .. }))
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
        if let (Some(flight), Some((_, _, latest))) = (self.in_flight, self.requested) {
            if flight.kind == Kind::Normal
                && flight.seq == latest
                && flight.seq > self.answered
                && flight.seq != self.withdrawn
            {
                self.withdrawn = flight.seq;
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
        if self.in_flight.is_some_and(|f| same(f.kind)) {
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
    pub fn request(&mut self, key: Key, now_ms: i64, source: &Arc<Source>) -> Requested {
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
        self.in_flight = Some(InFlight { kind: job.kind, seq: job.seq });
        Some(job)
    }

    /// WORKER, under the lock: R3 and R5. `outcome` is `None` when the job did not render or
    /// its epoch ended; the job is finished either way, so no counter can stick.
    pub fn finish(&mut self, job: &Job, outcome: Option<Rendered>) -> Finished {
        self.in_flight = None;
        match job.kind {
            Kind::Normal => {
                self.answered = self.answered.max(job.seq);
                let Some(Rendered { frame, validity, aligns }) = outcome else { return Finished::default() };
                let latest = self.requested.is_some_and(|(key, _, seq)| key == job.key && seq == job.seq);
                if job.seq == self.withdrawn || !latest {
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
                });
                self.next = None;
                self.arm_ahead(&job.source);
                Finished { wake: changed, changed, pub_bytes }
            }
            Kind::Ahead { after } => {
                let Some(Rendered { frame, validity, aligns }) = outcome else { return Finished::default() };
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
                });
                // Nothing to show yet: the promotion on the UI thread is what the screen sees.
                Finished { wake: false, changed, pub_bytes }
            }
        }
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
