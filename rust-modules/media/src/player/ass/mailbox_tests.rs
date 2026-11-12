//! The state machine of plan section 4.2 with a fake renderer: no native library, so these run in
//! `make check`. Each cue paints one pixel holding its tag; tag 0 paints nothing.
use super::mailbox::Kind;
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
    /// How the playhead moves. A paused pace is the one every test below was written for: a moving
    /// cue is then one frame per clock (the animated pipeline needs a running pace, tested apart).
    pace: Pace,
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
            pace: Pace::PAUSED,
        }
    }

    /// What the renderer draws at `now`: the last active cue's tag (0 = nothing).
    fn tag_at(&self, now: i64) -> u8 {
        self.cues.iter().filter(|c| (c.start..c.end).contains(&now)).next_back().map_or(0, |c| c.tag)
    }

    fn render(&mut self, job: &Job) -> mailbox::Rendered {
        match job.kind {
            Kind::Normal => self.normal.push(job.now_ms),
            Kind::Ahead { .. } | Kind::Pipe { .. } => self.ahead.push(job.now_ms),
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
                    scale: job.scale,
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
        mailbox::Rendered { frame, validity, aligns: None, cost_us: None, moves_on: false }
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
        let out = self.mb.request(self.key, now, &self.source, self.pace);
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
        mb.request(key, tick * 16, &source, Pace::PAUSED);
        while let Some(job) = mb.take() {
            jobs += 1;
            let frame = error_frame(job.key, Fault::RenderFailed);
            mb.finish(&job, Some(mailbox::Rendered { frame, validity: Validity::ALWAYS, aligns: None, cost_us: None, moves_on: false }));
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

/// A moving cue under a pace that is NOT running (paused, a frame step, a scrub): one frame per
/// distinct clock, nothing ahead. With a running playhead the cue is pipelined instead, and
/// `an_animated_run_is_shown_on_time_with_one_worker_through_every_append` says how.
#[test]
fn animated_cue_requests_every_tick() {
    let mut animated = cue(0, 100_000, 4);
    animated.animated = true;
    let mut sim = Sim::new(vec![animated]);
    sim.pace = Pace::PAUSED;
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
    let out = sim.mb.request(sim.key, 1500, &sim.source, Pace::PAUSED);
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
    assert!(sim.mb.request(sim.key, 100, &sim.source, Pace::PAUSED).wake);
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
    assert!(sim.mb.request(sim.key, 1100, &sim.source, Pace::PAUSED).promoted);
    assert_eq!(sim.mb.aligns_of(next_serial), Some(AlignSet { bottom: false, middle: false, top: true }));
    assert_eq!(sim.mb.aligns_of(serial), None);
}

#[test]
fn a_render_slower_than_the_packet_rate_still_publishes_issue_1328() {
    // A karaoke line whose picture changes every 100 ms, so every tick asks for a render.
    let cues: Vec<Cue> = (0..40)
        .map(|i| Cue { start: i * 100, end: (i + 1) * 100, tag: (i + 1) as u8, animated: true })
        .collect();
    let mut sim = Sim::new(cues);
    // Not running: the clock here moves per tick with no playhead behind it, which is the per-clock path.
    // The running path's version of this regression is the on-time test with revisions appended.
    sim.pace = Pace::PAUSED;
    let (mut now, mut revision) = (0, 1);
    sim.tick(now);
    let mut published = 0;
    for _ in 0..20 {
        let job = sim.mb.take().unwrap_or_else(|| {
            now += 16;
            sim.request(now);
            sim.mb.take().expect("an animated frame asks for a job on every tick")
        });
        // On the television one render takes ~150 ms (9 ticks) while a demuxed packet lands every
        // ~33 ms (2 ticks): the revision moves several times before the render completes.
        for tick in 0..9 {
            now += 16;
            if tick % 2 == 0 {
                revision += 1;
                sim.key = key_of(1, revision);
            }
            sim.request(now);
        }
        let before = sim.published().pub_id;
        sim.complete(job);
        published += u32::from(sim.published().pub_id != before);
    }
    assert!(published >= 18, "a completed render reaches the screen ({published} of 20)");
    let shown = sim.mb.view(&sim.key).map(|f| i64::from(f.rects[0].rgba[0]));
    assert!(shown.is_some_and(|tag| tag + 3 >= now / 100), "the picture moved with the clock ({shown:?} at {now} ms)");
}

#[test]
fn a_render_lagging_only_by_revision_publishes_and_leaves_the_newer_job_alone() {
    let mut sim = Sim::new(vec![cue(0, 100_000, 1), cue(2000, 3000, 9)]);
    sim.request(100);
    let flying = sim.mb.take().unwrap();
    let old = flying.key;
    sim.key = key_of(1, 2); // a packet was appended while the job rendered
    assert!(sim.mb.request(sim.key, 116, &sim.source, Pace::PAUSED).wake, "the new revision queues its own job");
    let outcome = sim.render(&flying);
    let done = sim.mb.finish(&flying, Some(outcome));
    assert!(done.wake && done.changed, "the screen is woken for the frame");
    assert_eq!(sim.published().key, old, "published under the key it was rendered for");
    assert_eq!(sim.mb.view(&sim.key).unwrap().rects[0].rgba[0], 1, "and shown at once");
    assert!(sim.mb.next_frame().is_none(), "no Ahead job is armed from a lagging frame");
    assert!(matches!(sim.mb.pending_job(), Some((Kind::Normal, 116))), "the newer job still runs");
    // The lagging key falls through to R2.5: nothing extra is queued while the newer job is out.
    assert_eq!(sim.mb.request(sim.key, 132, &sim.source, Pace::PAUSED), mailbox::Requested::default());
    sim.drain();
    assert_eq!(sim.published().key, sim.key, "the newest revision lands as usual");
    let boundary = sim.published().validity.until_ms;
    assert_eq!(sim.mb.next_frame().map(|n| n.validity.from_ms), Some(boundary), "the newest frame arms the next boundary");
}

#[test]
fn a_render_for_a_replaced_timeline_is_still_discarded() {
    let base = key_of(1, 1);
    let newer = [
        ("epoch", Key { epoch: 2, revision: 2, ..base }),
        ("source", Key { source_id: 2, revision: 2, ..base }),
        ("width", Key { width: 640, revision: 2, ..base }),
        ("height", Key { height: 360, revision: 2, ..base }),
        ("storage width", Key { storage_width: 640, revision: 2, ..base }),
        ("storage height", Key { storage_height: 360, revision: 2, ..base }),
    ];
    for (what, key) in newer {
        let mut sim = Sim::new(vec![cue(0, 100_000, 1)]);
        sim.request(100);
        let flying = sim.mb.take().unwrap();
        sim.key = key;
        sim.request(116);
        let outcome = sim.render(&flying);
        let done = sim.mb.finish(&flying, Some(outcome));
        assert_eq!(done, mailbox::Finished::default(), "a changed {what} discards the result");
        assert!(sim.mb.published.is_none(), "nothing was published for a changed {what}");
        assert!(matches!(sim.mb.pending_job(), Some((Kind::Normal, 116))), "the newer job still runs ({what})");
    }
}

// ---- the animated pipeline (issue #1328) -------------------------------------------------------
//
// A moving cue was rendered once per tick for the clock of that tick, and the result landed one
// render later, so a 80 ms render showed pictures 80-160 ms old, a dozen times a second. With a
// running playhead the mailbox now plans the frames: it renders ahead for future playheads on a
// step the measured cost sets, holds them, and shows each at its playhead, never before it.
//
// The rig is a discrete-event model: a moving cue over the whole script, `workers` workers that each
// spend `cost_ms` of wall-clock time per render, a UI tick every 16 ms and a playhead of `rate` x
// wall time (plus an `offset` the viewer can move). Each frame carries the playhead it was rendered
// for in its one pixel, so a test reads what the screen shows and how old it is. A worker looks for
// work only when it finishes a job or is woken (`Requested::wake`), as the real one parks, so a
// missing wake shows up as an idle worker.

use std::collections::BTreeSet;

const END: i64 = 1_000_000;

fn playing(rate: f64, frame_ms: i64) -> Pace {
    Pace { running: true, rate, frame_ms, tick_ms: 16 }
}

fn event_at(start_ms: i64) -> Event {
    Event { start_ms, duration_ms: 1000, payload: Arc::from(&b"0,0,Default,,0,0,0,,x"[..]) }
}

fn script_of(revision: u64, events: Vec<Event>) -> Arc<Source> {
    Arc::new(Source {
        id: 1,
        revision,
        origin: Origin::Authored,
        content: Content::Embedded {
            header: Arc::from(&b"[Script Info]\n"[..]),
            events: events.into(),
            fonts: Arc::from([]),
        },
    })
}

/// The frame for `job`'s playhead: one pixel holding it, animated until the cue ends at `cue_end`
/// and a still empty picture after. `cost_ms` is what the worker reports for the job.
fn moving(job: &Job, cost_ms: i64, cue_end: i64) -> mailbox::Rendered {
    moving_between(job, cost_ms, cue_end, 0)
}

/// As [`moving`], with an event starting or ending every `boundary_ms` playhead milliseconds (0:
/// none before `cue_end`): the frame's window of validity ends at the next of them, as a dense
/// karaoke script's does (the issue's fixture has ~53 edges a second).
fn moving_between(job: &Job, cost_ms: i64, cue_end: i64, boundary_ms: i64) -> mailbox::Rendered {
    let target = job.now_ms;
    let (rects, validity) = if target < cue_end {
        let rgba: Arc<[u8]> = Arc::from((target as i32).to_le_bytes());
        let (from_ms, until_ms) = match boundary_ms {
            0 => (0, cue_end),
            b => (target.div_euclid(b) * b, (target.div_euclid(b) * b + b).min(cue_end)),
        };
        (vec![Rect { x: 0, y: 0, width: 1, height: 1, rgba }], Validity { from_ms, until_ms, animated: true })
    } else {
        (Vec::new(), Validity { from_ms: cue_end, until_ms: i64::MAX, animated: false })
    };
    let frame = Arc::new(Frame {
        source_id: job.key.source_id,
        serial: next_serial(),
        width: job.key.width,
        height: job.key.height,
        rects,
        scale: job.scale,
        error: None,
    });
    // the boundary is the end of the moving stretch only when it is the cue's: before that the
    // picture after it moves as well
    let moves_on = validity.animated && validity.until_ms < cue_end;
    mailbox::Rendered { frame, validity, aligns: None, cost_us: Some(cost_ms as u32 * 1000), moves_on }
}

/// The playhead a `moving` frame was rendered for.
fn target_of(frame: &Frame) -> Option<i64> {
    frame.rects.first().map(|r| i64::from(i32::from_le_bytes(r.rgba[..4].try_into().unwrap())))
}

/// A mailbox with `workers` workers and a moving frame published at 1000 whose render cost
/// `cost_ms`: the state a pipeline starts from.
fn warm(workers: usize, cost_ms: i64) -> (Mailbox, Arc<Source>, Key) {
    let mut mb = Mailbox::new();
    mb.set_workers(workers);
    let (source, key) = (script_of(1, Vec::new()), key_of(1, 1));
    mb.request(key, 1000, &source, playing(1.0, 33));
    let job = mb.take().expect("the first frame is a Normal job");
    mb.finish(&job, Some(moving(&job, cost_ms, END)));
    (mb, source, key)
}

struct Busy {
    done_at: i64,
    cost: i64,
    job: Job,
}

struct Rig {
    mb: Mailbox,
    source: Arc<Source>,
    events: Vec<Event>,
    key: Key,
    pace: Pace,
    cost_ms: i64,
    cue_end: i64,
    /// An event boundary every this many playhead milliseconds (0: only the cue's end).
    boundary_ms: i64,
    /// Added to the playhead: the viewer's subtitle timing offset moving.
    offset: i64,
    wall: i64,
    busy: Vec<Option<Busy>>,
    parked: Vec<bool>,
    /// The costs of the next jobs, ahead of `cost_ms`: a stall.
    slow: Vec<i64>,
    normals: u32,
    pipes: u32,
    stale: u32,
    /// Every job taken: its kind, the playhead it renders and the revision it was handed.
    taken: Vec<(Kind, i64, u64)>,
    /// `(playhead, playhead of the frame)` at each promotion.
    promotions: Vec<(i64, i64)>,
    /// `(playhead, playhead of the frame on screen)` after each tick.
    shown: Vec<(i64, Option<i64>)>,
    /// What a frame costs at each rung over what it costs at full size (full, two thirds, half): the
    /// model's by default, which a test sets to something else to see what a wrong model does.
    ratios: [f64; 3],
    /// The scale and kind of every job taken.
    scales: Vec<(Kind, RenderScale)>,
    /// The scale of the frame on screen after each tick (`None`: no frame).
    shown_scales: Vec<Option<RenderScale>>,
    /// `(playhead, from, to)` for each move of the scale a request reported.
    changes: Vec<(i64, RenderScale, RenderScale)>,
}

impl Rig {
    fn new(workers: usize, cost_ms: i64) -> Self {
        let mut mb = Mailbox::new();
        mb.set_workers(workers);
        Self {
            mb,
            source: script_of(1, Vec::new()),
            events: Vec::new(),
            key: key_of(1, 1),
            pace: playing(1.0, 33),
            cost_ms,
            cue_end: END,
            boundary_ms: 0,
            offset: 0,
            wall: 1000,
            busy: (0..workers).map(|_| None).collect(),
            parked: vec![true; workers],
            slow: Vec::new(),
            normals: 0,
            pipes: 0,
            stale: 0,
            taken: Vec::new(),
            promotions: Vec::new(),
            shown: Vec::new(),
            ratios: RenderScale::LADDER.map(RenderScale::cost_ratio),
            scales: Vec::new(),
            shown_scales: Vec::new(),
            changes: Vec::new(),
        }
    }

    fn now(&self) -> i64 {
        (self.wall as f64 * self.pace.rate) as i64 + self.offset
    }

    fn try_take(&mut self, worker: usize) {
        if self.busy[worker].is_some() {
            return;
        }
        match self.mb.take() {
            Some(job) => {
                match job.kind {
                    Kind::Normal => self.normals += 1,
                    _ => self.pipes += 1,
                }
                self.taken.push((job.kind, job.now_ms, job.key.revision));
                self.scales.push((job.kind, job.scale));
                let rung = RenderScale::LADDER.iter().position(|&s| s == job.scale).unwrap();
                let cost = if self.slow.is_empty() {
                    (self.cost_ms as f64 * self.ratios[rung]).round() as i64
                } else {
                    self.slow.remove(0)
                };
                self.busy[worker] = Some(Busy { done_at: self.wall + cost, cost, job });
                self.parked[worker] = false;
            }
            None => self.parked[worker] = true,
        }
    }

    /// Finish every job due by wall time `until`, in order, each worker taking its next job at the
    /// moment it finished.
    fn finish_due(&mut self, until: i64) {
        while let Some((at, worker)) = self
            .busy
            .iter()
            .enumerate()
            .filter_map(|(w, b)| b.as_ref().map(|b| (b.done_at, w)))
            .filter(|&(at, _)| at <= until)
            .min()
        {
            self.wall = at;
            let Busy { cost, job, .. } = self.busy[worker].take().unwrap();
            let done = self.mb.finish(&job, Some(moving_between(&job, cost, self.cue_end, self.boundary_ms)));
            self.stale += u32::from(done.stale);
            self.try_take(worker);
        }
        self.wall = until;
    }

    fn tick(&mut self) {
        self.finish_due(self.wall + self.pace.tick_ms);
        let now = self.now();
        let out = self.mb.request(self.key, now, &self.source, self.pace);
        if let Some(change) = out.scale_change {
            self.changes.push((now, change.from, change.to));
        }
        if out.promoted {
            let target = self.mb.published.as_ref().map_or(0, |p| p.target_ms);
            self.promotions.push((now, target));
        }
        if out.wake {
            for worker in 0..self.busy.len() {
                if self.parked[worker] {
                    self.try_take(worker);
                }
            }
        }
        let frame = self.mb.view(&self.key);
        self.shown_scales.push(frame.as_ref().map(|f| f.scale));
        self.shown.push((now, frame.and_then(|f| target_of(&f))));
    }

    fn run(&mut self, ms: i64) {
        let until = self.wall + ms;
        while self.wall < until {
            self.tick();
        }
    }

    /// A packet whose event starts at `start_ms` arrives: a new revision of the same script.
    fn append(&mut self, start_ms: i64) {
        self.events.push(event_at(start_ms));
        let revision = self.source.revision + 1;
        self.source = script_of(revision, self.events.clone());
        self.key = Key { revision, ..self.key };
    }

    /// How old the frame on screen was at each tick from `from` on, in playhead milliseconds.
    fn ages(&self, from: usize) -> Vec<i64> {
        self.shown[from..].iter().map(|&(now, target)| now - target.expect("a frame is on screen")).collect()
    }

    fn distinct_shown(&self, from: usize) -> usize {
        self.shown[from..].iter().map(|&(_, target)| target).collect::<BTreeSet<_>>().len()
    }

    fn late_promotions(&self) -> usize {
        self.promotions.iter().filter(|&&(now, target)| now - target > 16).count()
    }
}

#[test]
fn an_animated_run_is_shown_on_time_with_one_worker_through_every_append() {
    let mut rig = Rig::new(1, 80);
    rig.run(1000);
    let (normals, from) = (rig.normals, rig.shown.len());
    for i in 0..300 {
        if i % 2 == 0 {
            // a demuxed packet every other tick, for an event well ahead of the playhead
            let now = rig.now();
            rig.append(now + 5000);
        }
        rig.tick();
    }
    assert_eq!(rig.normals, normals, "an append far ahead of the playhead asks for no Normal job");
    let ages = rig.ages(from);
    assert!(
        ages.iter().all(|age| (0..=100).contains(age)),
        "a frame is never early, and at most a step (80 ms) and a tick old: {ages:?}"
    );
    assert!(rig.distinct_shown(from) >= 55, "{} different frames in 4.8 s", rig.distinct_shown(from));
    assert!(
        rig.late_promotions() * 10 <= rig.promotions.len(),
        "{} of {} frames were promoted more than a tick after their playhead",
        rig.late_promotions(),
        rig.promotions.len()
    );
}

#[test]
fn three_workers_reach_thirty_frames_a_second_from_an_eighty_millisecond_render() {
    let mut rig = Rig::new(3, 80);
    rig.run(1000);
    let (normals, from, pipes) = (rig.normals, rig.shown.len(), rig.pipes);
    rig.run(5000);
    assert_eq!(rig.normals, normals);
    let distinct = rig.distinct_shown(from);
    assert!(distinct >= 140, "{distinct} different frames in 5 s");
    let ages = rig.ages(from);
    assert!(ages.iter().all(|age| (0..=50).contains(age)), "a 33 ms step and a tick: {ages:?}");
    assert!(rig.pipes - pipes <= 165, "{} renders for about 150 frames: little is wasted", rig.pipes - pipes);
    assert!(rig.late_promotions() * 10 <= rig.promotions.len());
}

/// The television's heavy karaoke opening (issue #1328, `fonts-over-budget.mkv`): about 2400 events in
/// 90 s, so an event starts or ends every ~19 ms and each frame's window of validity is shorter than a
/// render. The pipeline never engaged there: for twenty seconds every job was a Normal one, nothing was
/// planned ahead and no frame was ever shown by the plan.
#[test]
fn a_moving_cue_with_an_event_boundary_every_few_milliseconds_is_still_pipelined() {
    // (workers, render cost, ms between event boundaries, frames to show in 5 s)
    for (workers, cost_ms, boundary_ms, frames) in
        [(1, 90, 19, 50_u32), (3, 90, 19, 130), (1, 90, 7, 50), (3, 90, 7, 130), (1, 90, 40, 50), (1, 90, 250, 50)]
    {
        let mut rig = Rig::new(workers, cost_ms);
        rig.boundary_ms = boundary_ms;
        rig.run(2000);
        let (normals, from, pipes) = (rig.normals, rig.shown.len(), rig.pipes);
        rig.run(5000);
        let name = format!("{workers} worker(s) at {cost_ms} ms, a boundary every {boundary_ms} ms");
        assert!(rig.normals - normals <= 1, "{name}: {} Normal jobs in 5 s, the plan was given up", rig.normals - normals);
        assert!(rig.pipes - pipes >= frames, "{name}: {} planned renders in 5 s", rig.pipes - pipes);
        assert!(rig.distinct_shown(from) >= frames as usize, "{name}: {} different frames in 5 s", rig.distinct_shown(from));
        let ages = rig.ages(from);
        let step = rig.mb.step_ms(rig.pace);
        assert!(
            ages.iter().all(|age| (0..=step + 40).contains(age)),
            "{name}: a frame is never early and at most a step and a tick old: {ages:?}"
        );
    }
}

/// Boundaries inside the moving stretch are the plan's to resolve at its own step, so the end of a dense
/// stretch is shown within a step of it (a lone cue's end is exact: `when_the_moving_cue_ends...`),
/// and the plan hands over to the still picture without giving up on the way.
#[test]
fn a_dense_stretch_hands_over_to_the_still_picture_within_a_step_of_its_end() {
    let mut rig = Rig::new(1, 90);
    rig.boundary_ms = 19;
    rig.cue_end = 3501;
    rig.run(3000);
    assert!(rig.normals <= 1, "{} Normal jobs: the plan is not given up on the way", rig.normals);
    let published = rig.mb.published.as_ref().expect("a frame");
    assert!(!published.validity.animated, "the still picture after the cue is what is shown");
    let handed_over = rig
        .shown
        .iter()
        .find(|&&(now, shown)| now >= 3501 && shown.is_none())
        .map(|&(now, _)| now)
        .expect("the still picture was shown");
    assert!(handed_over - 3501 <= 90 + 32, "the cue ended at 3501 and the picture changed at {handed_over}");
    let pipes = rig.pipes;
    rig.run(1000);
    assert_eq!(rig.pipes, pipes, "nothing is planned for a picture that does not move");
}

#[test]
fn the_frame_step_follows_the_render_cost_over_the_workers_and_never_drops_below_a_video_frame() {
    let pace = playing(1.0, 33);
    let (mut mb, ..) = warm(1, 80);
    assert_eq!((mb.step_ms(pace), mb.lead_ms(pace)), (80, 116), "the cost; and the cost a quarter over, plus a tick");
    mb.set_workers(3);
    assert_eq!(mb.step_ms(pace), 33, "27 ms of cost per frame is under the 30 a second ceiling");
    assert_eq!(mb.step_ms(playing(1.0, 42)), 42, "24 fps content gets no step under its frame period");
    assert_eq!(mb.lead_ms(pace), 116, "workers do not change how long one render takes");
    assert_eq!(mb.step_ms(playing(2.0, 33)), 54, "at twice the speed a render spans twice the playhead: 160 over 3");
    assert_eq!(mb.lead_ms(playing(2.0, 33)), 232);
    let (slow, ..) = warm(1, 2000);
    assert_eq!(slow.step_ms(pace), 250, "a pathological render does not stretch the step without bound");
    assert_eq!(slow.lead_ms(pace), 641, "and its sample is capped at half a second");
}

#[test]
fn a_jump_of_the_clock_inside_one_key_restarts_the_pipeline_with_one_normal_job() {
    for jump in [-5000, 5000] {
        let mut rig = Rig::new(3, 80);
        rig.run(1500);
        let normals = rig.normals;
        rig.offset = jump;
        rig.tick();
        assert!(rig.mb.held_targets().is_empty(), "the frames planned for the old clock are of no use ({jump})");
        rig.run(1500);
        assert_eq!(rig.normals, normals + 1, "one Normal job for the new clock ({jump})");
        let from = rig.shown.len();
        rig.run(1000);
        let ages = rig.ages(from);
        assert!(ages.iter().all(|age| (0..=50).contains(age)), "on time again at the new clock ({jump}): {ages:?}");
    }
}

#[test]
fn a_stalled_worker_is_recovered_from_without_showing_a_frame_early() {
    let mut rig = Rig::new(1, 80);
    rig.run(1000);
    let normals = rig.normals;
    rig.slow = vec![900];
    rig.run(3000);
    let early = rig.shown.iter().filter(|&&(now, target)| target.is_some_and(|t| t > now)).count();
    assert_eq!(early, 0, "a frame is never shown before its playhead");
    assert!(rig.normals - normals <= 3, "the stall re-anchors at most a few times: {}", rig.normals - normals);
    let from = rig.shown.len() - 60;
    let ages = rig.ages(from);
    assert!(ages.iter().all(|age| (0..=100).contains(age)), "on time again after the stall: {ages:?}");
}

#[test]
fn no_frame_is_planned_past_the_horizon_so_a_spare_worker_parks() {
    for workers in [1usize, 3] {
        let pace = playing(1.0, 33);
        let (mut mb, source, key) = warm(workers, 80);
        mb.request(key, 1100, &source, pace);
        let (step, lead) = (mb.step_ms(pace), mb.lead_ms(pace));
        let mut targets = Vec::new();
        while let Some(job) = mb.take() {
            assert!(matches!(job.kind, Kind::Pipe { .. }), "{:?}", job.kind);
            targets.push(job.now_ms);
        }
        assert_eq!(targets.first(), Some(&(1100 + lead)), "the first lands in time: a render and a tick ahead");
        assert!(targets.windows(2).all(|w| w[1] - w[0] == step), "one step apart: {targets:?}");
        assert!(targets.last().is_some_and(|&t| t <= 1100 + lead + workers as i64 * step));
        assert_eq!(targets.len(), workers + 1, "up to the horizon and no further ({workers} workers): {targets:?}");
    }
}

#[test]
fn a_held_frame_is_never_shown_before_its_playhead() {
    let pace = playing(1.0, 33);
    let (mut mb, source, key) = warm(1, 80);
    mb.request(key, 1100, &source, pace);
    let job = mb.take().expect("a frame is planned");
    let target = job.now_ms;
    mb.finish(&job, Some(moving(&job, 80, END)));
    for now in (1100..target).step_by(4) {
        let out = mb.request(key, now, &source, pace);
        assert!(!out.promoted, "promoted at {now}, before {target}");
        assert_eq!(mb.published.as_ref().map(|p| p.target_ms), Some(1000));
    }
    let out = mb.request(key, target, &source, pace);
    assert!(out.promoted, "and promoted at its own playhead");
    assert_eq!(mb.published.as_ref().map(|p| p.target_ms), Some(target));
}

#[test]
fn the_greatest_frame_not_after_the_playhead_is_shown_whatever_order_the_frames_landed_in() {
    let pace = playing(1.0, 33);
    for order in ["later first", "earlier first", "earlier after the later was shown"] {
        let (mut mb, source, key) = warm(2, 80);
        mb.request(key, 1100, &source, pace);
        let (early, late) = (mb.take().unwrap(), mb.take().unwrap());
        assert!(early.now_ms < late.now_ms);
        let land = |mb: &mut Mailbox, job: &Job| mb.finish(job, Some(moving(job, 80, END)));
        let mut last = mailbox::Finished::default();
        match order {
            "later first" => {
                land(&mut mb, &late);
                land(&mut mb, &early);
            }
            "earlier first" => {
                land(&mut mb, &early);
                land(&mut mb, &late);
            }
            _ => {
                land(&mut mb, &late);
                assert!(mb.request(key, late.now_ms, &source, pace).promoted);
                last = land(&mut mb, &early);
            }
        }
        if order != "earlier after the later was shown" {
            let out = mb.request(key, late.now_ms + 5, &source, pace);
            assert!(out.promoted, "{order}");
        }
        assert_eq!(mb.published.as_ref().map(|p| p.target_ms), Some(late.now_ms), "{order}");
        assert!(mb.held_targets().iter().all(|&t| t > late.now_ms), "the older frame is dropped ({order})");
        if order == "earlier after the later was shown" {
            assert!(last.stale, "a frame older than the one on screen is thrown away");
        }
    }
}

#[test]
fn a_seek_a_new_source_or_a_new_size_throws_the_whole_plan_away() {
    let pace = playing(1.0, 33);
    let key = key_of(1, 1);
    let newer = [
        ("epoch", Key { epoch: 2, ..key }),
        ("source", Key { source_id: 2, ..key }),
        ("size", Key { width: 640, ..key }),
        ("storage size", Key { storage_height: 360, ..key }),
    ];
    for (what, newer) in newer {
        let (mut mb, source, key) = warm(3, 80);
        mb.request(key, 1100, &source, pace);
        let (flying, also) = (mb.take().unwrap(), mb.take().unwrap());
        let landed = mb.take().unwrap();
        mb.finish(&landed, Some(moving(&landed, 80, END)));
        assert!(!mb.held_targets().is_empty(), "{what}: a frame is held");
        assert!(mb.request(newer, 1116, &source, pace).wake, "{what}");
        assert!(mb.held_targets().is_empty(), "{what}: the held frames are dropped");
        assert_eq!(mb.pending_job().map(|(k, _)| k), Some(Kind::Normal), "{what}");
        for job in [flying, also] {
            let done = mb.finish(&job, Some(moving(&job, 80, END)));
            assert!(done.stale && !done.changed, "{what}: a frame of the old timeline is discarded");
        }
        assert!(mb.held_targets().is_empty(), "{what}");
    }
}

#[test]
fn an_append_keeps_the_frames_before_its_first_event_and_replans_from_there() {
    let mut rig = Rig::new(3, 80);
    rig.run(1500);
    let normals = rig.normals;
    let held = rig.mb.held_targets();
    assert!(held.len() >= 2, "three workers hold frames ahead: {held:?}");
    let cut = held[1];
    rig.append(cut);
    rig.tick();
    // a worker that took a job before this tick's request read the revision before it
    let taken = rig.taken.len();
    let after = rig.mb.held_targets();
    assert!(after.iter().all(|&t| t < cut), "the frames from the new event on are dropped: {after:?} (cut {cut})");
    let from = rig.shown.len();
    rig.run(1500);
    assert_eq!(rig.normals, normals, "the pipeline replans; it does not ask for a Normal job");
    assert!(
        rig.taken[taken..].iter().all(|&(_, _, revision)| revision == rig.key.revision),
        "every render after the append reads the new revision"
    );
    let ages = rig.ages(from);
    assert!(ages.iter().all(|age| (0..=120).contains(age)), "no hole is left where the frames were dropped: {ages:?}");
}

#[test]
fn an_append_that_reaches_the_playhead_restarts_with_a_normal_job() {
    let mut rig = Rig::new(3, 80);
    rig.run(1500);
    let normals = rig.normals;
    let now = rig.now();
    rig.append(now - 20);
    rig.tick();
    assert!(rig.mb.held_targets().is_empty(), "what was planned is dropped with the picture it was planned from");
    rig.run(300);
    assert_eq!(rig.normals, normals + 1, "the picture on screen may be wrong: it is rendered again");
    rig.run(1200);
    assert_eq!(rig.normals, normals + 1, "and the pipeline starts again from that frame");
    let from = rig.shown.len() - 30;
    assert!(rig.ages(from).iter().all(|age| (0..=50).contains(age)));
}

#[test]
fn a_normal_job_waits_only_when_every_worker_is_busy_with_a_frame_ahead() {
    let pace = playing(1.0, 33);
    for (workers, busy, waits) in [(2usize, 2usize, true), (2, 1, false), (1, 1, true)] {
        let (mut mb, source, key) = warm(workers, 80);
        mb.request(key, 1100, &source, pace);
        let _rendering: Vec<Job> = (0..busy).map(|_| mb.take().unwrap()).collect();
        mb.request(Key { epoch: 2, ..key }, 1116, &source, pace);
        assert_eq!(mb.pending_queued().is_some(), waits, "{workers} workers, {busy} busy");
    }
}

#[test]
fn a_paused_player_plans_nothing_and_a_new_clock_renders_one_frame_for_it() {
    let (mut mb, source, key) = warm(3, 80);
    mb.request(key, 1100, &source, playing(1.0, 33));
    let flying = mb.take().expect("a frame is planned while playing");
    let paused = Pace { running: false, ..playing(1.0, 33) };
    mb.request(key, 1000, &source, paused);
    assert!(mb.held_targets().is_empty());
    assert!(mb.take().is_none(), "nothing is planned while paused");
    let done = mb.finish(&flying, Some(moving(&flying, 80, END)));
    assert!(done.stale && mb.held_targets().is_empty(), "a frame planned before the pause is discarded");
    // a frame step or an offset change gives a new clock: one frame for it, as for any moving cue
    assert!(mb.request(key, 1500, &source, paused).wake);
    assert_eq!(mb.pending_job(), Some((Kind::Normal, 1500)));
    let job = mb.take().unwrap();
    mb.finish(&job, Some(moving(&job, 80, END)));
    // playing again plans from the frame on screen
    mb.request(key, 1520, &source, playing(1.0, 33));
    assert!(matches!(mb.take().map(|j| j.kind), Some(Kind::Pipe { .. })));
}

#[test]
fn a_faster_playhead_plans_further_ahead_in_playhead_time() {
    for (rate, lead, step) in [(1.0, 116, 80), (2.0, 232, 160), (0.5, 58, 40)] {
        let pace = playing(rate, 33);
        let (mut mb, source, key) = warm(1, 80);
        mb.request(key, 1100, &source, pace);
        let first = mb.take().expect("a frame is planned");
        assert_eq!(first.now_ms, 1100 + lead, "rate {rate}");
        let second = mb.take().expect("and one more within the horizon");
        assert_eq!(second.now_ms - first.now_ms, step, "rate {rate}");
    }
}

#[test]
fn a_still_cue_costs_the_same_jobs_whatever_the_pace() {
    for pace in [Pace::PAUSED, playing(1.0, 33), playing(2.0, 42)] {
        let mut sim = Sim::new(vec![cue(1000, 200_000, 7), cue(300_000, 400_000, 8)]);
        sim.pace = pace;
        for tick in 0..120 {
            assert_eq!(sim.tick(2000 + tick * 16).unwrap_or(7), 7);
        }
        assert_eq!(sim.jobs(), (1, 1), "one Normal job and one Ahead job for a still cue ({pace:?})");
        assert_eq!(sim.ahead, [200_000], "the Ahead job renders the cue's end");
        assert_eq!(sim.tick(200_000), Some(0), "and the boundary is promoted exactly as before");
        assert_eq!(sim.promoted, 1);
    }
}

#[test]
fn with_one_worker_one_job_is_in_flight_and_only_a_few_frames_are_held() {
    let mut rig = Rig::new(1, 80);
    for _ in 0..300 {
        rig.tick();
        assert!(rig.mb.in_flight_len() <= 1);
        assert!(rig.mb.held_targets().len() <= 3, "{:?}", rig.mb.held_targets());
    }
    assert!(rig.pipes > 40, "the single worker kept rendering ahead: {}", rig.pipes);
}

#[test]
fn finishing_a_job_clears_it_and_a_job_of_a_retired_mailbox_is_ignored() {
    let pace = playing(1.0, 33);
    let (mut mb, source, key) = warm(2, 80);
    assert_eq!(mb.in_flight_len(), 0, "the warm-up's job was finished");
    mb.request(key, 1100, &source, pace);
    let (a, b) = (mb.take().unwrap(), mb.take().unwrap());
    assert_eq!(mb.in_flight_len(), 2);
    mb.finish(&a, None);
    assert_eq!(mb.in_flight_len(), 1, "a job that did not render is finished too");
    // the epoch ended while `b` was rendering: its mailbox was retired and is a fresh one now
    drop(mb.retire());
    assert!(mb.is_empty());
    assert_eq!(mb.finish(&b, Some(moving(&b, 80, END))), mailbox::Finished::default());
    assert!(mb.is_empty(), "nothing of a job the mailbox does not hold is settled against it");
    mb.request(key, 2000, &source, pace);
    let next = mb.take().unwrap();
    assert!(next.seq > b.seq, "a retired mailbox never hands out a seq an old job may still carry");
}

#[test]
fn when_the_moving_cue_ends_the_pipeline_stops_and_the_still_picture_takes_over() {
    let mut rig = Rig::new(1, 80);
    rig.cue_end = 1700;
    rig.run(1800);
    assert!(
        rig.taken.iter().any(|&(_, target, _)| target == 1700),
        "the end of the cue is rendered at its own playhead, not at the next step: {:?}",
        rig.taken
    );
    let pipes = rig.pipes;
    rig.run(1500);
    let published = rig.mb.published.as_ref().expect("a frame");
    assert!(!published.validity.animated, "the still picture after the cue is what is shown");
    assert_eq!(rig.pipes, pipes, "no frame is planned for a picture that does not move");
    assert!(rig.mb.held_targets().is_empty());
}

#[test]
fn the_earliest_change_of_an_append_is_the_first_event_it_adds() {
    let old = script_of(1, vec![event_at(100), event_at(200)]);
    let appended = script_of(2, vec![event_at(100), event_at(200), event_at(900), event_at(700)]);
    assert_eq!(earliest_change_ms(&old, &appended), 700, "the earliest start among the new events");
    assert_eq!(earliest_change_ms(&old, &script_of(2, vec![event_at(100), event_at(200)])), i64::MAX, "nothing new");
    // expiry from the front and an append: only the added event counts, what expired is in the past
    let slid = script_of(3, vec![event_at(200), event_at(800)]);
    assert_eq!(earliest_change_ms(&old, &slid), 800);
    // anything the renderer cannot express as an append can change every playhead
    let mut other_id = (*appended).clone();
    other_id.id = 2;
    assert_eq!(earliest_change_ms(&old, &other_id), i64::MIN);
    let mut restyled = (*appended).clone();
    restyled.content = Content::Embedded {
        header: Arc::from(&b"[Script Info]\nTitle: x\n"[..]),
        events: Arc::from([event_at(100), event_at(200), event_at(900)]),
        fonts: Arc::from([]),
    };
    assert_eq!(earliest_change_ms(&old, &restyled), i64::MIN);
    let whole = Source {
        id: 1,
        revision: 2,
        origin: Origin::Authored,
        content: Content::Script { bytes: Arc::from(&b"[Script Info]\n"[..]), fonts: Arc::from([]) },
    };
    assert_eq!(earliest_change_ms(&old, &whole), i64::MIN);
}

#[test]
fn the_pace_reads_the_players_values_and_clamps_the_odd_ones() {
    let film = Pace::of(true, 1000, 23.976, 16);
    assert_eq!((film.running, film.rate, film.frame_ms, film.tick_ms), (true, 1.0, 42, 16));
    assert_eq!(Pace::of(true, 1000, 0.0, 16).frame_ms, 42, "an unknown rate is film");
    assert_eq!(Pace::of(true, 1000, f64::NAN, 16).frame_ms, 42);
    assert_eq!(Pace::of(true, 1000, 59.94, 16).frame_ms, 17);
    assert_eq!(Pace::of(true, 1000, 29.97, 16).frame_ms, 33);
    assert_eq!(Pace::of(true, 1000, 1000.0, 16).frame_ms, 4, "never a zero step");
    assert_eq!(Pace::of(true, 2000, 24.0, 16).rate, 2.0);
    assert_eq!(Pace::of(true, 50, 24.0, 16).rate, 0.25);
    assert_eq!(Pace::of(true, 90_000, 24.0, 16).rate, 4.0);
    assert_eq!(Pace::of(false, 1000, 24.0, 0).tick_ms, 4);
    assert_eq!(Pace::of(false, 1000, 24.0, 4_000_000_000).tick_ms, 100, "a stall in the caller's ticks");
    assert!(!Pace::of(false, 1000, 24.0, 16).running);
}

// The workers a pipeline can use, and what the screen and the renderers are told about its frames.

#[test]
fn a_job_carries_the_newest_landed_frame_as_its_reference() {
    let pace = playing(1.0, 33);
    let (mut mb, source, key) = warm(2, 80);
    mb.request(key, 1100, &source, pace);
    let reference = |job: &Job| job.reference.as_ref().and_then(|frame| target_of(frame));
    let (first, second) = (mb.take().unwrap(), mb.take().unwrap());
    assert_eq!(reference(&first), Some(1000), "the frame on screen, while nothing newer has landed");
    assert_eq!(reference(&second), Some(1000));
    let first_target = first.now_ms;
    mb.finish(&first, Some(moving(&first, 80, END)));
    let third = mb.take().expect("a third frame is within the horizon of two workers");
    assert_eq!(reference(&third), Some(first_target), "the newest frame that landed, not the one on screen");
    // a Normal job after the plan was thrown away refers to what is on screen again
    mb.request(Key { width: 640, ..key }, 1130, &source, pace);
    let normal = mb.take().unwrap();
    assert_eq!(normal.kind, Kind::Normal);
    assert_eq!(reference(&normal), Some(1000), "the plan's frames were dropped with it");
}

#[test]
fn a_still_cues_ahead_job_refers_to_the_frame_it_follows() {
    let mut sim = Sim::new(vec![cue(1000, 2000, 7), cue(2000, 3000, 8)]);
    sim.request(1100);
    let normal = sim.mb.take().unwrap();
    assert!(normal.reference.is_none(), "nothing was shown before the first frame");
    sim.complete(normal);
    let ahead = sim.mb.take().unwrap();
    assert!(matches!(ahead.kind, Kind::Ahead { .. }));
    let shown = sim.mb.view(&sim.key).unwrap();
    assert!(ahead.reference.as_ref().is_some_and(|frame| Arc::ptr_eq(frame, &shown)));
}

#[test]
fn the_pipeline_asks_for_the_workers_that_reach_thirty_frames_a_second() {
    for (cost_ms, rate, wanted) in [(20, 1.0, 1), (50, 1.0, 2), (80, 1.0, 3), (80, 2.0, 5), (500, 1.0, 16)] {
        let (mut mb, source, key) = warm(1, cost_ms);
        let out = mb.request(key, 1100, &source, playing(rate, 33));
        assert_eq!(out.workers_wanted, wanted, "{cost_ms} ms at {rate}x");
    }
    let (mut mb, source, key) = warm(1, 80);
    assert_eq!(mb.request(key, 1100, &source, Pace::PAUSED).workers_wanted, 0, "nothing planned while paused");
    let mut sim = Sim::new(vec![cue(1000, 200_000, 7)]);
    sim.pace = playing(1.0, 33);
    assert_eq!(sim.mb.request(sim.key, 2000, &sim.source, sim.pace).workers_wanted, 0, "a still cue plans nothing");
}

#[test]
fn a_promoted_frame_of_the_pipeline_says_it_was_late_only_past_a_tick() {
    let pace = playing(1.0, 33);
    for (after, late) in [(0, false), (16, false), (17, true), (60, true)] {
        let (mut mb, source, key) = warm(1, 80);
        mb.request(key, 1100, &source, pace);
        let job = mb.take().unwrap();
        mb.finish(&job, Some(moving(&job, 80, END)));
        let out = mb.request(key, job.now_ms + after, &source, pace);
        assert!(out.promoted && out.piped, "{after} ms after its playhead");
        assert_eq!(out.late, late, "{after} ms after its playhead, one tick being 16");
    }
    // a still cue's frame promoted early is the boundary one, not the pipeline's
    let mut sim = Sim::new(vec![cue(0, 1000, 1), cue(1000, 2000, 2)]);
    sim.request(100);
    sim.drain();
    let out = sim.mb.request(sim.key, 1100, &sim.source, Pace::PAUSED);
    assert!(out.promoted && !out.piped && !out.late);
}

// The render scale (issue #1328): the pipeline draws a moving stretch smaller only while it cannot
// meet the target at full size, over the renderers the pool has, and never shows a frame less for it.
// The rig's jobs cost what the scale they are given makes them cost (`Rig::ratios`).

use RenderScale::{Full, Half, TwoThirds};

/// A rig whose pool allows `renderers` renderers, all of them warm.
fn pooled(renderers: usize, cost_ms: i64) -> Rig {
    let mut rig = Rig::new(renderers, cost_ms);
    rig.mb.set_capacity(Some(renderers));
    rig
}

/// Hold the playhead at `frozen` for `ms` of wall time: the player is paused.
fn pause(rig: &mut Rig, frozen: i64, ms: i64) {
    rig.pace = Pace { running: false, ..rig.pace };
    let until = rig.wall + ms;
    while rig.wall < until {
        rig.offset = frozen - (rig.wall + rig.pace.tick_ms);
        rig.tick();
    }
}

/// The opening of the issue's file on the television: ~85 ms a frame at full size, which one renderer
/// cannot make thirty of a second at any size, two can at two thirds and three at full size.
#[test]
fn a_heavy_stretch_is_drawn_smaller_so_the_renderers_the_pool_has_reach_thirty_frames_a_second() {
    // (renderers, the rung the stretch settles at, different frames shown in 5 s at least)
    for (renderers, expected, frames) in [(1, Half, 110), (2, TwoThirds, 140), (3, Full, 140)] {
        let mut rig = pooled(renderers, 85);
        rig.run(1500);
        let (from, normals) = (rig.shown.len(), rig.normals);
        rig.run(5000);
        let name = format!("{renderers} renderer(s)");
        assert_eq!(rig.mb.scale(), expected, "{name}: {:?}", rig.changes);
        assert_eq!(rig.normals, normals, "{name}: the plan is not given up on the way");
        assert!(rig.distinct_shown(from) >= frames, "{name}: {} different frames in 5 s", rig.distinct_shown(from));
        let step = rig.mb.step_ms(rig.pace);
        let ages = rig.ages(from);
        assert!(ages.iter().all(|age| (0..=step + 40).contains(age)), "{name}: a step is {step} ms: {ages:?}");
        assert!(rig.changes.len() <= 1, "{name}: decided once: {:?}", rig.changes);
    }
}

#[test]
fn the_first_frame_of_a_stretch_is_full_size_and_the_pipeline_then_draws_smaller() {
    let mut rig = pooled(1, 85);
    rig.run(3000);
    assert_eq!(rig.mb.scale(), Half);
    let kinds: Vec<_> = rig.scales.iter().map(|&(kind, scale)| (matches!(kind, Kind::Pipe { .. }), scale)).collect();
    assert_eq!(kinds[0], (false, Full), "the stretch's first frame is a Normal job at full size: {kinds:?}");
    assert!(kinds[1..].iter().any(|&(pipe, scale)| pipe && scale == Half), "{kinds:?}");
    assert!(
        kinds.iter().take_while(|&&(pipe, _)| !pipe).all(|&(_, scale)| scale == Full),
        "what the screen needs before the pipeline plans anything is drawn at full size: {kinds:?}"
    );
}

#[test]
fn a_cheap_moving_cue_and_a_still_one_stay_at_full_size() {
    let mut rig = pooled(1, 12);
    rig.run(8000);
    assert_eq!(rig.mb.scale(), Full);
    assert!(rig.changes.is_empty() && rig.scales.iter().all(|&(_, scale)| scale == Full), "{:?}", rig.changes);
    // a still cue, however small the pool would make it: every job full size, and the frame
    let mut sim = Sim::new(vec![cue(1000, 200_000, 7)]);
    sim.pace = playing(1.0, 33);
    sim.mb.set_capacity(Some(1));
    for tick in 0..120 {
        sim.tick(2000 + tick * 16);
    }
    assert_eq!(sim.published().frame.scale, Full);
    assert_eq!(sim.mb.scale(), Full);
}

#[test]
fn no_scale_is_decided_before_the_pool_has_said_how_many_renderers_it_has() {
    let mut rig = Rig::new(1, 85);
    rig.run(3000);
    assert_eq!(rig.mb.scale(), Full, "the pool has not answered: there is no evidence of what it can make");
    assert!(rig.changes.is_empty());
    rig.mb.set_capacity(Some(1));
    rig.run(200);
    assert_eq!(rig.mb.scale(), Half);
    assert_eq!(rig.changes.len(), 1);
}

/// A change of scale is not a change of picture: the frame on screen stays until a frame at the new
/// scale lands, the frames already planned are still shown, and the plan is not given up.
#[test]
fn a_scale_change_keeps_the_frame_on_screen_until_the_first_one_at_the_new_scale_lands() {
    let mut rig = pooled(1, 25);
    rig.run(3000);
    assert_eq!(rig.mb.scale(), Full, "25 ms a frame meets the target at full size");
    let (normals, from) = (rig.normals, rig.shown.len());
    rig.cost_ms = 70;
    rig.run(4000);
    assert_eq!(rig.changes.iter().map(|c| (c.1, c.2)).collect::<Vec<_>>(), [(Full, Half)]);
    assert_eq!(rig.normals, normals, "the plan was kept: no Normal job, no blank while one renders");
    let scales = &rig.shown_scales[from..];
    assert!(scales.iter().all(Option::is_some), "a frame was on screen at every tick of the change");
    let first_half = scales.iter().position(|&s| s == Some(Half)).expect("frames at the new scale landed");
    assert!(first_half > 0 && scales[..first_half].iter().all(|&s| s == Some(Full)), "the old frames stood until then");
    assert!(scales[first_half..].iter().all(|&s| s == Some(Half)), "and the new ones took over for good");
    assert!(rig.ages(from).iter().all(|age| *age >= 0), "and none was shown early");
}

/// The last frame of a reduced stretch is the still picture after it: shown from the pipeline at the
/// size it was drawn, then drawn again at full size. The reduced frame stands until then, and the
/// full-size job is not withdrawn as "covered" by it.
#[test]
fn the_still_picture_after_a_reduced_stretch_is_redrawn_at_full_size() {
    let mut rig = pooled(1, 85);
    rig.cue_end = 4000;
    rig.run(2500);
    assert_eq!(rig.mb.scale(), Half);
    let (normals, from) = (rig.normals, rig.shown.len());
    rig.run(4000);
    let published = rig.mb.published.as_ref().expect("a frame");
    assert!(!published.validity.animated, "the cue has ended");
    assert_eq!(published.frame.scale, Full, "the still picture is full size");
    assert_eq!(rig.normals - normals, 1, "one full-size job for it, and no more");
    assert_eq!(rig.scales.last().map(|s| s.1), Some(Full));
    assert!(rig.shown_scales[from..].iter().all(Option::is_some), "a frame was on screen all the way");
    let pipes = rig.pipes;
    rig.run(1000);
    assert_eq!((rig.pipes, rig.normals - normals), (pipes, 1), "and nothing follows");
}

#[test]
fn a_paused_player_shows_a_full_size_frame_of_a_reduced_stretch() {
    let mut rig = pooled(1, 85);
    rig.run(3000);
    assert_eq!(rig.mb.published.as_ref().map(|p| p.frame.scale), Some(Half));
    let (normals, frozen) = (rig.normals, rig.now());
    pause(&mut rig, frozen, 1000);
    let published = rig.mb.published.as_ref().expect("a frame");
    assert_eq!(published.frame.scale, Full, "a paused picture is drawn at full size");
    assert_eq!(rig.normals - normals, 1, "once");
    assert_eq!(rig.mb.scale(), Half, "the stretch's rung is kept for when it runs again");
    // played again from where it stopped: the pipeline goes on at the rung it was at
    rig.pace = playing(1.0, 33);
    rig.offset = frozen - (rig.wall + rig.pace.tick_ms);
    rig.run(2000);
    let last_pipes: Vec<_> = rig.scales.iter().rev().filter(|(kind, _)| matches!(kind, Kind::Pipe { .. })).take(5).collect();
    assert!(!last_pipes.is_empty() && last_pipes.iter().all(|(_, scale)| *scale == Half), "{last_pipes:?}");
}

#[test]
fn a_cost_sample_is_taken_back_to_full_size_by_the_scale_it_was_drawn_at() {
    let mut rig = pooled(1, 80);
    rig.run(3000);
    assert_eq!(rig.mb.scale(), Half);
    // the frames it timed cost 35 ms each: that is 80 ms at full size, not a cheaper stretch
    let cost = rig.mb.cost_full_ms();
    assert!((cost - 80.0).abs() <= 3.0, "{cost}");
    let step = rig.mb.step_ms(rig.pace);
    assert!((35..=36).contains(&step), "and the step follows the cost at the rung in use: {step}");
}

#[test]
fn the_pool_is_asked_about_what_full_size_would_want_and_the_plan_wants_what_its_scale_needs() {
    let pace = playing(1.0, 33);
    for (cost_ms, pool) in [(85, 3), (20, 1)] {
        let (mut mb, source, key) = warm(1, cost_ms);
        let out = mb.request(key, 1100, &source, pace);
        assert_eq!(out.pool_wanted, pool, "{cost_ms} ms");
        assert_eq!(out.workers_wanted, pool, "the workers the plan wants are the same while it draws at full size");
    }
    // drawing smaller, it wants fewer workers than the pool is asked for
    let mut rig = pooled(1, 85);
    rig.run(3000);
    let out = rig.mb.request(rig.key, rig.now(), &rig.source, rig.pace);
    assert_eq!((out.pool_wanted, out.workers_wanted), (3, 2));
}

/// The model may be wrong about what two thirds costs: the move up looks comfortable and costs more
/// than projected, and it is taken back. That happens once per half minute at most.
#[test]
fn a_model_that_is_wrong_flaps_a_bounded_number_of_times() {
    let mut rig = pooled(1, 40);
    rig.ratios = [1.0, 0.9, 0.44];
    rig.run(60_000);
    assert!(rig.changes.len() <= 6, "{} moves in a minute: {:?}", rig.changes.len(), rig.changes);
    let moves = rig.changes.iter().map(|c| c.0).collect::<Vec<_>>();
    assert!(moves.windows(2).all(|w| w[1] - w[0] >= 500), "never twice within half a second: {moves:?}");
}

/// The rung belongs to the selection it was decided for: a track switched to directly (a new epoch, no
/// `clear` between) starts at full size again, whatever the track before it was drawn at, and waits for
/// the pool to say what it has.
#[test]
fn a_new_selection_starts_at_full_size_and_waits_for_the_pool() {
    let mut rig = pooled(1, 85);
    rig.run(3000);
    assert_eq!(rig.mb.scale(), Half);
    // the next selection: the runtime has not heard from the pool for it, and its frames are cheap
    rig.key = Key { epoch: 2, ..rig.key };
    rig.mb.set_capacity(None);
    rig.cost_ms = 12;
    let (from, moves) = (rig.scales.len(), rig.changes.len());
    rig.run(3000);
    assert_eq!(rig.mb.scale(), Full, "the old track's rung is not the new one's");
    assert!(rig.scales.len() > from + 5, "the new selection plans frames");
    // a worker may take one more frame of the old plan before the tick that sees the new key
    let first = from + rig.scales[from..].iter().position(|(kind, _)| *kind == Kind::Normal).expect("its first frame");
    assert!(rig.scales[first..].iter().all(|&(_, scale)| scale == Full), "{:?}", &rig.scales[first..]);
    assert_eq!(rig.changes.len(), moves, "no move of its own: {:?}", rig.changes);
}

/// A hole in a reduced stretch is recovered at the rung the stretch is drawn at: a frame at full size
/// would make libass empty its caches, and the next frame at the old size would pay for them again.
#[test]
fn a_hole_in_a_reduced_stretch_is_recovered_at_its_own_size() {
    let mut rig = pooled(1, 85);
    rig.run(3000);
    assert_eq!(rig.mb.scale(), Half);
    let (from, normals) = (rig.scales.len(), rig.normals);
    rig.slow = vec![700];
    rig.run(4000);
    assert!(rig.normals > normals, "the stall left a hole, and a Normal job recovered it");
    let recoveries: Vec<_> = rig.scales[from..].iter().filter(|(kind, _)| *kind == Kind::Normal).collect();
    assert!(recoveries.iter().all(|(_, scale)| *scale == Half), "{recoveries:?}");
    assert_eq!(rig.mb.scale(), Half);
}
