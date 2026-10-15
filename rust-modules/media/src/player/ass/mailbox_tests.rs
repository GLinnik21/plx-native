//! The state machine of plan section 4.2 with a fake renderer: no native library, so these run in
//! `make check`. Each cue paints one pixel holding its tag; tag 0 paints nothing.
use super::*;
use crate::player::ass_timing::{self, Motion, Span};

#[derive(Clone)]
struct Cue {
    start: i64,
    end: i64,
    tag: u8,
    animated: bool,
}

fn cue(start: i64, end: i64, tag: u8) -> Cue {
    Cue { start, end, tag, animated: false }
}

struct Sim {
    mb: Mailbox,
    source: Arc<Source>,
    key: Key,
    cues: Vec<Cue>,
    last: Option<(u8, Arc<Frame>)>,
    normal: Vec<i64>,
    ahead: Vec<i64>,
    promoted: u32,
    /// A windowed script: how far ahead of the playhead the held window reaches.
    slide: Option<i64>,
    window: Option<(i64, i64)>,
}

fn key_of(epoch: u64, revision: u64) -> Key {
    Key {
        epoch,
        source_id: 1,
        revision,
        width: 320,
        height: 180,
        storage_width: 320,
        storage_height: 180,
    }
}

impl Sim {
    fn new(cues: Vec<Cue>) -> Self {
        Self {
            mb: Mailbox::new(),
            source: Arc::new(Source {
                id: 1,
                revision: 1,
                origin: Origin::Authored,
                content: Content::Embedded {
                    header: Arc::from(&b"[Script Info]\n"[..]),
                    events: Arc::from([]),
                    fonts: Arc::from([]),
                },
            }),
            key: key_of(1, 1),
            cues,
            last: None,
            normal: Vec::new(),
            ahead: Vec::new(),
            promoted: 0,
            slide: None,
            window: None,
        }
    }

    /// What the renderer draws at `now`: the last active cue's tag (0 = nothing).
    fn tag_at(&self, now: i64) -> u8 {
        self.cues.iter().filter(|c| (c.start..c.end).contains(&now)).next_back().map_or(0, |c| c.tag)
    }

    fn render(&mut self, job: &Job) -> mailbox::Rendered {
        match job.kind {
            Kind::Normal => self.normal.push(job.now_ms),
            Kind::Ahead { .. } => self.ahead.push(job.now_ms),
        }
        if let Some(span) = self.slide {
            // Engine::slide: a playhead outside the held window feeds the renderer again.
            if self.window.is_none_or(|(lo, hi)| job.now_ms < lo || job.now_ms >= hi) {
                self.window = Some((job.now_ms - 1000, job.now_ms + span));
            }
        }
        let tag = self.tag_at(job.now_ms);
        let frame = match &self.last {
            Some((t, frame)) if *t == tag => frame.clone(),
            _ => {
                let rects = if tag == 0 {
                    Vec::new()
                } else {
                    vec![Rect { x: 0, y: 0, width: 1, height: 1, rgba: Arc::from([tag, 0, 0, 255]) }]
                };
                Arc::new(Frame {
                    source_id: job.key.source_id,
                    serial: next_serial(),
                    width: job.key.width,
                    height: job.key.height,
                    rects,
                    error: None,
                })
            }
        };
        self.last = Some((tag, frame.clone()));
        let mut validity = ass_timing::validity(
            self.cues.iter().map(|c| Span {
                start_ms: c.start,
                end_ms: c.end,
                motion: Motion::Known(c.animated),
            }),
            job.now_ms,
        );
        if let Some((lo, hi)) = self.window {
            validity.from_ms = validity.from_ms.max(lo);
            validity.until_ms = validity.until_ms.min(hi);
        }
        mailbox::Rendered { frame, validity, aligns: None }
    }

    fn complete(&mut self, job: Job) {
        let outcome = self.render(&job);
        self.mb.finish(&job, Some(outcome));
    }

    fn drain(&mut self) {
        while let Some(job) = self.mb.take() {
            self.complete(job);
        }
    }

    fn request(&mut self, now: i64) -> Option<u8> {
        let out = self.mb.request(self.key, now, &self.source);
        self.promoted += u32::from(out.promoted);
        self.mb.view(&self.key).map(|f| f.rects.first().map_or(0, |r| r.rgba[0]))
    }

    /// One UI tick with a worker fast enough to finish whatever it was handed.
    fn tick(&mut self, now: i64) -> Option<u8> {
        self.request(now);
        self.drain();
        self.mb.view(&self.key).map(|f| f.rects.first().map_or(0, |r| r.rgba[0]))
    }

    fn jobs(&self) -> (usize, usize) {
        (self.normal.len(), self.ahead.len())
    }

    fn published(&self) -> &mailbox::Published {
        self.mb.published.as_ref().unwrap()
    }
}

#[test]
fn static_cue_requests_once() {
    let mut sim = Sim::new(vec![cue(1000, 200_000, 7)]);
    for tick in 0..120 {
        assert_eq!(sim.tick(2000 + tick * 16).unwrap_or(7), 7);
    }
    assert_eq!(sim.jobs(), (1, 1), "one Normal job and one Ahead job for a static cue");
    assert_eq!(sim.ahead, [200_000], "the Ahead job renders the cue's end");
}

#[test]
fn error_frame_requests_once() {
    let mut mb = Mailbox::new();
    let source = Sim::new(vec![]).source;
    let key = key_of(1, 1);
    let mut jobs = 0;
    for tick in 0..120 {
        mb.request(key, tick * 16, &source);
        while let Some(job) = mb.take() {
            jobs += 1;
            let frame = error_frame(job.key, Fault::RenderFailed);
            mb.finish(&job, Some(mailbox::Rendered { frame, validity: Validity::ALWAYS, aligns: None }));
        }
        assert_eq!(mb.view(&key).unwrap().error, Some(Fault::RenderFailed));
    }
    assert_eq!(jobs, 1, "an error changes only with the key");
    assert!(mb.pending_job().is_none(), "and no Ahead job follows an infinite window");
}

#[test]
fn boundary_without_visible_change_requests_at_most_once() {
    // back-to-back identical cues, then an empty cue, then the same text again
    let mut sim = Sim::new(vec![cue(0, 1000, 5), cue(1000, 2000, 5), cue(3000, 4000, 0), cue(4000, 5000, 5)]);
    sim.tick(0);
    let mut before = sim.normal.len() + sim.ahead.len();
    let mut serials = vec![sim.mb.view(&sim.key).unwrap().serial];
    for now in (16..6000).step_by(16) {
        sim.tick(now);
        let total = sim.normal.len() + sim.ahead.len();
        assert!(total - before <= 1, "at most one job per tick at {now}");
        before = total;
        let serial = sim.mb.view(&sim.key).unwrap().serial;
        if serials.last() != Some(&serial) {
            serials.push(serial);
        }
    }
    assert_eq!(sim.normal.len(), 1, "only the first frame is asked for by the screen");
    assert_eq!(sim.ahead.len(), 5, "one early render per distinct boundary: 1000 2000 3000 4000 5000");
    // the renderer reuses a frame only against the one it rendered last: text, nothing, text, nothing
    assert_eq!(serials.len(), 4);
}

#[test]
fn a_windowed_slide_costs_one_job_per_boundary() {
    // one long cue, so the only boundaries are the held window's own edges every 5 s
    let mut sim = Sim::new(vec![cue(0, 100_000, 3)]);
    sim.slide = Some(5000);
    sim.tick(0);
    let mut per_edge = Vec::new();
    let mut before = sim.normal.len() + sim.ahead.len();
    for now in (16..30_000).step_by(16) {
        sim.tick(now);
        let total = sim.normal.len() + sim.ahead.len();
        assert!(total - before <= 1, "at most one job per tick at {now}");
        if total != before {
            per_edge.push(now);
        }
        before = total;
        assert_eq!(sim.mb.view(&sim.key).unwrap().rects[0].rgba[0], 3, "the cue never drops at {now}");
    }
    // each window edge costs one early render; none per tick near `hi`
    let edges = per_edge.len();
    assert!(edges <= 8, "{edges} jobs over six windows: {per_edge:?}");
    assert!(sim.normal.len() <= 1 + edges, "{:?}", sim.normal);
}

#[test]
fn in_flight_boundary_is_not_requeued() {
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2)]);
    // the first frame is in flight: ticks past a boundary add nothing
    sim.request(10);
    let first = sim.mb.take().unwrap();
    for now in (20..3000).step_by(16) {
        sim.request(now);
    }
    assert!(sim.mb.pending_job().is_none(), "R2.5: wait for the key's first frame");
    sim.complete(first);
    // the Ahead job is in flight and a boundary passes: ONE Normal job is queued, not one per tick
    let ahead = sim.mb.take().unwrap();
    assert!(matches!(ahead.kind, Kind::Ahead { .. }));
    for now in (1000..1100).step_by(16) {
        sim.request(now);
    }
    assert_eq!(sim.mb.pending_job().map(|(k, _)| k), Some(Kind::Normal));
    let seq = sim.mb.pending_job().unwrap().1;
    for now in (1100..1200).step_by(16) {
        sim.request(now);
    }
    assert_eq!(sim.mb.pending_job(), Some((Kind::Normal, seq)), "the same Normal job waits");
    // the Ahead job lands, the next tick promotes it and the waiting Normal job is withdrawn
    sim.complete(ahead);
    sim.request(1300);
    assert!(sim.mb.pending_job().is_none_or(|(k, _)| k != Kind::Normal), "withdrawn");
    assert_eq!(sim.promoted, 1);
    assert_eq!(sim.normal.len(), 1, "that Normal job was never rendered");
}

#[test]
fn render_ahead_frame_is_ready_at_its_start() {
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2)]);
    assert_eq!(sim.tick(500), Some(1));
    let jobs = sim.jobs();
    assert_eq!(sim.tick(1000), Some(2), "the cue is on screen on the tick at its start");
    assert_eq!(sim.promoted, 1);
    assert_eq!(sim.normal.len(), jobs.0, "no Normal job rendered it");
}

#[test]
fn render_ahead_dropped_on_late_push() {
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2)]);
    sim.tick(500);
    assert!(sim.mb.next_frame().is_some());
    sim.key = key_of(1, 2); // a cue arrived: the revision moved
    sim.request(500);
    assert!(sim.mb.next_frame().is_none(), "the frame rendered from the old revision is dropped");
    assert_eq!(sim.mb.pending_job().map(|(k, _)| k), Some(Kind::Normal));
}

#[test]
fn late_push_inside_window_rerenders_once() {
    let mut sim = Sim::new(vec![cue(0, 100_000, 1)]);
    sim.tick(500);
    let before = sim.jobs();
    sim.key = key_of(1, 2);
    sim.cues.push(cue(2000, 3000, 9));
    for now in (600..2000).step_by(16) {
        sim.tick(now);
    }
    assert_eq!(sim.normal.len(), before.0 + 1, "one Normal job for the new revision");
    assert_eq!(sim.tick(2500), Some(9));
}

#[test]
fn ahead_job_targets_published_until() {
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2), cue(2500, 3000, 3)]);
    sim.request(100);
    let normal = sim.mb.take().unwrap();
    sim.complete(normal);
    let (kind, now) = sim.mb.pending_job().unwrap();
    assert!(matches!(kind, Kind::Ahead { after } if after == sim.published().pub_id));
    assert_eq!(now, sim.published().validity.until_ms);
    assert_eq!(now, 1000);
    sim.drain();
    // promote, then the next Ahead job targets the promoted frame's until, not its from
    sim.request(1000);
    let (kind, now) = sim.mb.pending_job().unwrap();
    assert!(matches!(kind, Kind::Ahead { after } if after == sim.published().pub_id));
    assert_eq!((now, sim.published().validity.from_ms), (2000, 1000));
    assert_eq!(now, sim.published().validity.until_ms);
}

#[test]
fn ahead_frame_gets_fresh_pub_id() {
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2), cue(3000, 4000, 3)]);
    sim.tick(100);
    let first = sim.published().pub_id;
    let next_id = sim.mb.next_frame().unwrap().pub_id;
    assert!(next_id > first);
    sim.request(1000);
    assert_eq!(sim.published().pub_id, next_id, "a promotion keeps the id R5 gave it");
    let (kind, _) = sim.mb.pending_job().unwrap();
    assert_eq!(kind, Kind::Ahead { after: next_id });
}

#[test]
fn at_most_one_ahead_frame() {
    let cues = (0..50).map(|i| cue(i * 1000, i * 1000 + 500, (i % 200) as u8 + 1)).collect();
    let mut sim = Sim::new(cues);
    sim.tick(10);
    assert_eq!(sim.jobs(), (1, 1));
    for now in (20..400).step_by(16) {
        sim.tick(now);
    }
    assert_eq!(sim.jobs(), (1, 1), "no chain through the remaining boundaries");
    assert!(sim.mb.next_frame().is_some());
    assert!(sim.mb.pending_job().is_none());
}

#[test]
fn normal_overwrites_pending_ahead() {
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2)]);
    sim.request(100);
    let normal = sim.mb.take().unwrap();
    sim.complete(normal);
    assert!(matches!(sim.mb.pending_job(), Some((Kind::Ahead { .. }, _))));
    sim.key = key_of(1, 2);
    sim.request(100);
    assert_eq!(sim.mb.pending_job().map(|(k, _)| k), Some(Kind::Normal));
}

#[test]
fn ahead_never_overwrites_pending_normal() {
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2), cue(5000, 6000, 3)]);
    sim.tick(100);
    // a jump enqueues a Normal job; arming ahead meanwhile (a covered request after a withdraw)
    // must leave it in the slot
    sim.request(5500);
    assert_eq!(sim.mb.pending_job().map(|(k, _)| k), Some(Kind::Normal));
    sim.request(5500);
    sim.request(5501);
    assert_eq!(sim.mb.pending_job().map(|(k, _)| k), Some(Kind::Normal));
}

#[test]
fn animated_cue_requests_every_tick() {
    let mut animated = cue(0, 100_000, 4);
    animated.animated = true;
    let mut sim = Sim::new(vec![animated]);
    for tick in 0..120 {
        sim.tick(tick * 16);
    }
    assert_eq!(sim.jobs(), (120, 0), "a moving cue renders per tick and arms nothing ahead");
    // a paused clock does not
    for _ in 0..10 {
        sim.tick(119 * 16);
    }
    assert_eq!(sim.jobs(), (120, 0));
}

#[test]
fn backward_seek_rerenders() {
    let mut sim = Sim::new(vec![cue(10_000, 20_000, 1), cue(30_000, 40_000, 2)]);
    sim.tick(15_000);
    let before = sim.normal.len();
    assert_eq!(sim.tick(5000), Some(0), "before the cue: nothing");
    assert_eq!(sim.normal.len(), before + 1);
    assert_eq!(sim.tick(35_000), Some(2));
}

#[test]
fn offset_jump_mid_cue_renders_once() {
    for (start, target) in [(50_000, 155_000), (155_000, 50_000)] {
        let mut sim = Sim::new(vec![cue(1000, 100_000, 1), cue(150_000, 160_000, 2), cue(10_000_000, 10_001_000, 9)]);
        sim.tick(start);
        let base = sim.jobs();
        for _ in 0..120 {
            sim.tick(target);
        }
        let after = sim.jobs();
        assert_eq!((after.0 - base.0, after.1 - base.1), (1, 1), "one Normal job plus one Ahead job ({start} -> {target})");
    }
    // and after a promotion, where `published` came from an Ahead job
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2), cue(150_000, 160_000, 3), cue(200_000, 201_000, 4)]);
    sim.tick(100);
    sim.tick(1000);
    assert_eq!(sim.promoted, 1);
    let base = sim.jobs();
    for _ in 0..120 {
        assert_eq!(sim.tick(155_000).unwrap_or(3), 3);
    }
    let after = sim.jobs();
    assert_eq!((after.0 - base.0, after.1 - base.1), (1, 1));
    assert_eq!(sim.tick(155_000), Some(3));
}

#[test]
fn promotion_while_normal_pending() {
    // (i) a pending Normal job is withdrawn at a promotion, and the Ahead job is armed
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2), cue(5000, 6000, 3)]);
    sim.tick(100);
    sim.request(5500); // a jump: Normal pending
    assert_eq!(sim.mb.pending_job().map(|(k, _)| k), Some(Kind::Normal));
    // ... (ii) whose own `now` lies outside the promoted frame's validity: the playhead is back
    sim.request(1000);
    assert_eq!(sim.promoted, 1);
    assert!(matches!(sim.mb.pending_job(), Some((Kind::Ahead { .. }, 2000))), "armed, Normal withdrawn");
    sim.drain();
    assert_eq!(sim.mb.view(&sim.key).unwrap().rects[0].rgba[0], 2);
    assert_eq!(sim.normal.len(), 1, "the withdrawn Normal job was never rendered");

    // (iii) a Normal job already in flight finishes, but its result is discarded
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2), cue(5000, 6000, 3)]);
    sim.tick(100);
    sim.request(5500);
    let flying = sim.mb.take().unwrap();
    assert_eq!(flying.kind, Kind::Normal);
    sim.request(1000);
    assert_eq!(sim.promoted, 1);
    let promoted_id = sim.published().pub_id;
    sim.complete(flying);
    assert_eq!(sim.published().pub_id, promoted_id, "published stays the promoted frame");
    assert_eq!(sim.mb.view(&sim.key).unwrap().rects[0].rgba[0], 2);
}

#[test]
fn return_into_published_withdraws_normal() {
    for in_flight in [false, true] {
        let mut sim = Sim::new(vec![cue(1000, 100_000, 1), cue(150_000, 160_000, 2)]);
        sim.tick(2000);
        let id = sim.published().pub_id;
        sim.request(155_000); // the offset jumped
        let flying = in_flight.then(|| sim.mb.take().unwrap());
        sim.request(3000); // ... and jumped back before that job completed
        if let Some(job) = flying {
            sim.complete(job);
        }
        sim.drain();
        assert_eq!(sim.published().pub_id, id, "the jump target was never published ({in_flight})");
        assert_eq!(sim.mb.view(&sim.key).unwrap().rects[0].rgba[0], 1);
        assert_eq!(sim.mb.next_frame().map(|n| n.validity.from_ms), Some(100_000), "an Ahead job is re-armed");
    }
}

#[test]
fn ahead_frame_from_old_epoch_not_promoted() {
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2)]);
    sim.tick(100);
    assert!(sim.mb.next_frame().is_some());
    sim.key = key_of(2, 1); // a seek re-ids the track: the epoch moved
    let out = sim.mb.request(sim.key, 1500, &sim.source);
    assert!(!out.promoted && out.wake);
    assert_eq!(sim.mb.pending_job().map(|(k, _)| k), Some(Kind::Normal));
    assert!(sim.mb.view(&sim.key).is_none(), "the old epoch's frame is never shown");
}

#[test]
fn stale_ahead_result_discarded() {
    for ahead_first in [true, false] {
        let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2), cue(50_000, 60_000, 3), cue(70_000, 71_000, 4)]);
        sim.request(100);
        let normal = sim.mb.take().unwrap();
        sim.complete(normal);
        let ahead = sim.mb.take().unwrap();
        assert!(matches!(ahead.kind, Kind::Ahead { .. }));
        sim.request(55_000); // an offset jump while the Ahead job renders
        if ahead_first {
            sim.complete(ahead);
            sim.drain();
        } else {
            sim.drain();
            sim.complete(ahead);
        }
        let published = sim.published();
        match sim.mb.next_frame() {
            None => {}
            Some(next) => assert_eq!(next.validity.from_ms, published.validity.until_ms, "adjacent, never stale"),
        }
        assert_eq!(sim.mb.view(&sim.key).unwrap().rects[0].rgba[0], 3);
    }
}

#[test]
fn a_cancelled_epoch_finishes_the_job_without_publishing() {
    let mut sim = Sim::new(vec![cue(0, 1000, 1)]);
    sim.request(100);
    let job = sim.mb.take().unwrap();
    sim.mb.finish(&job, None);
    assert!(sim.mb.published.is_none());
    assert_eq!(sim.mb.answered(), job.seq, "a job that did not render is still answered");

    assert!(sim.mb.pending_job().is_none());
    // the Normal job counts as answered, so a later request is not blocked behind it
    sim.key = key_of(2, 1);
    assert!(sim.mb.request(sim.key, 100, &sim.source).wake);
}

#[test]
fn an_ahead_frame_counts_as_changed_with_its_bytes_and_does_not_wake_the_screen() {
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2)]);
    sim.request(100);
    let normal = sim.mb.take().unwrap();
    let outcome = sim.render(&normal);
    let first = sim.mb.finish(&normal, Some(outcome));
    assert_eq!((first.wake, first.changed, first.pub_bytes), (true, true, 4));
    let ahead = sim.mb.take().unwrap();
    let outcome = sim.render(&ahead);
    let held = sim.mb.finish(&ahead, Some(outcome));
    assert_eq!((held.wake, held.changed, held.pub_bytes), (false, true, 4), "a new cue, held for later");
}

#[test]
fn an_ahead_frame_identical_to_the_published_one_is_not_a_change() {
    let mut sim = Sim::new(vec![cue(0, 1000, 5), cue(1000, 2000, 5)]);
    sim.request(100);
    let normal = sim.mb.take().unwrap();
    sim.complete(normal);
    let ahead = sim.mb.take().unwrap();
    let outcome = sim.render(&ahead);
    let held = sim.mb.finish(&ahead, Some(outcome));
    assert_eq!((held.changed, held.pub_bytes), (false, 0));
}

#[test]
fn a_replaced_normal_job_keeps_the_stamp_of_its_wait() {
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2)]);
    sim.request(100);
    let normal = sim.mb.take().unwrap();
    sim.complete(normal);
    let _ahead = sim.mb.take().unwrap(); // rendering, so a Normal job has to wait
    sim.request(1100);
    let since = sim.mb.pending_queued().expect("waits behind the Ahead job");
    sim.request(5000); // a jump replaces the pending Normal job
    assert_eq!(sim.mb.pending_queued(), Some(since), "the wait began with the first one");
}

#[test]
fn aligns_are_read_for_the_published_frames_serial_only_and_follow_a_promotion() {
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2)]);
    sim.request(100);
    let normal = sim.mb.take().unwrap();
    let mut first = sim.render(&normal);
    first.aligns = Some(AlignSet { bottom: true, middle: false, top: false });
    let serial = first.frame.serial;
    sim.mb.finish(&normal, Some(first));
    assert_eq!(sim.mb.aligns_of(serial), Some(AlignSet { bottom: true, middle: false, top: false }));
    assert_eq!(sim.mb.aligns_of(serial + 1), None, "another frame's classes are never handed out");

    // The frame rendered ahead brings its own, and the promotion makes them the published ones.
    let ahead = sim.mb.take().unwrap();
    let mut next = sim.render(&ahead);
    next.aligns = Some(AlignSet { bottom: false, middle: false, top: true });
    let next_serial = next.frame.serial;
    sim.mb.finish(&ahead, Some(next));
    assert_eq!(sim.mb.aligns_of(next_serial), None, "held ahead, not published yet");
    assert!(sim.mb.request(sim.key, 1100, &sim.source).promoted);
    assert_eq!(sim.mb.aligns_of(next_serial), Some(AlignSet { bottom: false, middle: false, top: true }));
    assert_eq!(sim.mb.aligns_of(serial), None);
}
