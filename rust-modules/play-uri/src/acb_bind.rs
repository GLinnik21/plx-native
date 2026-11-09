//! The ACB bind sequence for a URI-loaded pipeline, as ONE pure machine both URI backends step:
//! the production backend (`backend.rs`) and the dev probe (`uriprobe.rs`).
//!
//! **What it is.** The sequence the 2026-11-02 measurement put moving video on the webOS 4.5 set
//! with (`docs/measurements/uri-acb-dass-2026-11-02.md`): the gate, then once the service has
//! loaded the media and its real video facts are known, `setSinkType`+`setMediaId`, LOADED, an
//! optional `connectDass` slot, the video-data descriptor, and the full-screen display with
//! PLAYING. Steps are paced (200 ms after each, 300 ms after the descriptor), anchored to the
//! completion of the previous call and not to the clock of the tick that noticed it. The service's
//! own `play` is never part of it: it is sent at `loadCompleted` by the caller and must not wait
//! for the descriptor.
//!
//! **Shape.** Pure, like `uri_lifecycle`: no clock (time arrives as `now_ms`), no global, no IO. An
//! event goes in, [`Fx`] come out, and a runner (`acb_drive::execute`) performs each [`Op`]
//! through the display sink and answers with [`Event::Answered`] (or [`Event::Deferred`] when the
//! plane owner asks the bind to wait for a previous release). The machine has no text: the probe
//! turns [`Fx`] into its `uriprobe:` log lines and the backend into its own.
use crate::uri_protocol::{MediaId, VideoFacts};

/// The `windowId` an ACB-bound `load` carries. 4.x firmware ignores the option; the service wants
/// the key present (the measured sequence sent this literal).
pub const DUMMY_WINDOW_ID: &str = "window_id_dummy";
/// One step's budget from its dispatch to its answer.
pub const OP_TIMEOUT_MS: u64 = 5_000;
/// How long the sequence waits for real video facts after `loadCompleted`.
pub const VIDEO_TIMEOUT_MS: u64 = 20_000;
/// The pace after every step but the descriptor, and after the descriptor.
pub const PACE_MS: u64 = 200;
pub const PACE_AFTER_VIDEO_DATA_MS: u64 = 300;
/// How often a bind the plane owner deferred is offered again (the owner's fence resolves within
/// `RELEASE_FENCE_MS`, well inside [`OP_TIMEOUT_MS`]).
pub const RETRY_MS: u64 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcbRequest(u64);

impl AcbRequest {
    pub fn number(self) -> u64 { self.0 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step { Check, MediaId, Loaded, Connect, VideoData, Display }

/// One native call the runner performs. `Connect { enabled: false }` is the timed slot without the
/// call (the control arm, and production).
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Check { dass: bool },
    MediaId { media_id: MediaId },
    Loaded,
    Connect { enabled: bool },
    VideoData { media_id: MediaId, video: VideoFacts },
    Display,
}

/// Why the sequence ended without a bind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// A step was answered "not invoked".
    Refused,
    /// A step was not answered in [`OP_TIMEOUT_MS`] (or its answer was later than that).
    Timeout,
    /// No real video facts in [`VIDEO_TIMEOUT_MS`].
    VideoTimeout,
}

pub enum Event {
    /// Evaluate the gate. The sequence proper cannot start before it passes.
    Start,
    /// The service reported `loadCompleted` for this identity: bind as soon as video facts exist.
    Loaded { media_id: MediaId },
    /// Real video facts, observed. Kept from the moment they arrive, even before `Loaded`.
    Video(VideoFacts),
    Answered { request: AcbRequest, invoked: bool, completed_ms: u64 },
    /// The runner could not start the step yet (a previous release is still in flight); offer it
    /// again shortly.
    Deferred { request: AcbRequest },
    Tick,
    /// The session is ending: nothing further runs, and a late answer is inert.
    Cancel,
}

#[derive(Debug, PartialEq)]
pub enum Fx {
    /// Run this step. `retry` is a deferred step offered again.
    Run { request: AcbRequest, step: Step, op: Op, retry: bool },
    /// `Loaded` arrived before any video facts.
    WaitingVideo,
    /// The runner's answer to the step, for the record.
    Answered { step: Step, invoked: bool },
    /// The gate passed: the caller opens its session.
    GatePassed,
    /// The display step was answered: the plane is bound.
    Bound,
    Failed(Failure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Binding { request: AcbRequest, step: Step, deadline: u64 },
    /// The gate passed; waiting for `Loaded`.
    Gated,
    WaitingVideo { deadline: u64 },
    Pacing { next: Step, deadline: u64 },
    Retry { retry_at: u64, deadline: u64 },
    Bound,
    Ended,
}

pub struct AcbBind {
    dass: bool,
    state: State,
    media_id: Option<MediaId>,
    video: Option<VideoFacts>,
    sequence: u64,
}

impl AcbBind {
    /// `dass` arms the `connectDass` call in the Connect slot (the probe's `acb-dass` arm only).
    pub fn new(dass: bool) -> Self {
        Self { dass, state: State::Idle, media_id: None, video: None, sequence: 0 }
    }
    pub fn waiting_for_video(&self) -> bool { matches!(self.state, State::WaitingVideo { .. }) }
    pub fn bound(&self) -> bool { self.state == State::Bound }
    /// The gate answered yes and the sequence waits for (or has begun) the media.
    pub fn passed_gate(&self) -> bool { !matches!(self.state, State::Idle | State::Binding { step: Step::Check, .. } | State::Ended) }
    /// A step is in flight, waiting out its pace, or deferred: ACB verbs may still be issued.
    pub fn running(&self) -> bool {
        matches!(self.state, State::Binding { .. } | State::Pacing { .. } | State::Retry { .. })
    }

    pub fn step(&mut self, event: Event, now_ms: u64, fx: &mut Vec<Fx>) {
        match event {
            Event::Start if self.state == State::Idle => self.dispatch(Step::Check, now_ms, None, fx),
            Event::Start => {}
            Event::Loaded { media_id } if self.state == State::Gated => {
                self.media_id = Some(media_id);
                if self.video.is_some() {
                    self.dispatch(Step::MediaId, now_ms, None, fx);
                } else {
                    self.state = State::WaitingVideo { deadline: now_ms.saturating_add(VIDEO_TIMEOUT_MS) };
                    fx.push(Fx::WaitingVideo);
                }
            }
            Event::Loaded { .. } => {}
            Event::Video(video) => {
                self.video = Some(video);
                if matches!(self.state, State::WaitingVideo { .. }) {
                    self.dispatch(Step::MediaId, now_ms, None, fx);
                }
            }
            Event::Answered { request, invoked, completed_ms } => self.answered(request, invoked, completed_ms, fx),
            Event::Deferred { request } => {
                if let State::Binding { request: expected, step: Step::MediaId, deadline } = self.state {
                    if expected == request {
                        self.state = State::Retry { retry_at: now_ms.saturating_add(RETRY_MS), deadline };
                    }
                }
            }
            Event::Tick => self.tick(now_ms, fx),
            Event::Cancel => self.state = State::Ended,
        }
    }

    fn tick(&mut self, now_ms: u64, fx: &mut Vec<Fx>) {
        match self.state {
            State::Binding { deadline, .. } if now_ms >= deadline => self.fail(Failure::Timeout, fx),
            State::Pacing { next, deadline } if now_ms >= deadline => self.dispatch(next, now_ms, None, fx),
            State::WaitingVideo { deadline } if now_ms >= deadline => self.fail(Failure::VideoTimeout, fx),
            State::Retry { deadline, .. } if now_ms >= deadline => self.fail(Failure::Timeout, fx),
            State::Retry { retry_at, deadline } if now_ms >= retry_at => {
                self.dispatch(Step::MediaId, now_ms, Some(deadline), fx);
            }
            _ => {}
        }
    }

    fn fail(&mut self, failure: Failure, fx: &mut Vec<Fx>) {
        self.state = State::Ended;
        fx.push(Fx::Failed(failure));
    }

    /// `deadline` is kept across a deferred step's offers, so the budget is the first dispatch's.
    fn dispatch(&mut self, step: Step, now_ms: u64, deadline: Option<u64>, fx: &mut Vec<Fx>) {
        let op = match step {
            Step::Check => Op::Check { dass: self.dass },
            Step::MediaId => Op::MediaId { media_id: self.media_id.clone().expect("loaded identity") },
            Step::Loaded => Op::Loaded,
            Step::Connect => Op::Connect { enabled: self.dass },
            Step::VideoData => Op::VideoData {
                media_id: self.media_id.clone().expect("loaded identity"),
                video: self.video.expect("observed video facts"),
            },
            Step::Display => Op::Display,
        };
        self.sequence = self.sequence.checked_add(1).expect("bounded ACB script");
        let request = AcbRequest(self.sequence);
        let retry = deadline.is_some();
        let deadline = deadline.unwrap_or_else(|| now_ms.saturating_add(OP_TIMEOUT_MS));
        self.state = State::Binding { request, step, deadline };
        fx.push(Fx::Run { request, step, op, retry });
    }

    fn answered(&mut self, request: AcbRequest, invoked: bool, completed_ms: u64, fx: &mut Vec<Fx>) {
        let State::Binding { request: expected, step, deadline } = self.state else { return };
        if expected != request {
            return;
        }
        if completed_ms >= deadline {
            self.fail(Failure::Timeout, fx);
            return;
        }
        fx.push(Fx::Answered { step, invoked });
        if !invoked {
            self.fail(Failure::Refused, fx);
            return;
        }
        let next = match step {
            Step::Check => {
                self.state = State::Gated;
                fx.push(Fx::GatePassed);
                return;
            }
            Step::MediaId => Step::Loaded,
            Step::Loaded => Step::Connect,
            Step::Connect => Step::VideoData,
            Step::VideoData => Step::Display,
            Step::Display => {
                self.state = State::Bound;
                fx.push(Fx::Bound);
                return;
            }
        };
        let pace = if step == Step::VideoData { PACE_AFTER_VIDEO_DATA_MS } else { PACE_MS };
        self.state = State::Pacing { next, deadline: completed_ms.saturating_add(pace) };
    }
}

/// What the gate read. `ready` is the verdict; the rest is for the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gate {
    pub mode: i32,
    pub boot_handle: bool,
    pub ptype: i32,
    pub dass_available: bool,
    pub initialized: bool,
    pub ready: bool,
}

/// The ACB binding needs the television's ACB display mode, the URI engine's own boot handle, a handle the library
/// confirmed initialized (a created handle and `playerType 0` alone do not prove it), player type
/// 0 (what the measured sequence initialized), and, for the `connectDass` arm, the entry point.
pub fn gate(dass: bool, mode: i32, boot_handle: bool, ptype: i32, dass_available: bool, initialized: bool) -> Gate {
    let ready = mode == plx_platform::tv::sink::VP_ACB && boot_handle && initialized && ptype == 0
        && (!dass || dass_available);
    Gate { mode, boot_handle, ptype, dass_available, initialized, ready }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIDEO: VideoFacts = VideoFacts { width: 1280, height: 720, frame_rate: 24.0, bit_rate: None };

    struct Rig { m: AcbBind, now: u64 }
    impl Rig {
        fn new(dass: bool) -> Self { Self { m: AcbBind::new(dass), now: 1_000 } }
        fn feed(&mut self, event: Event) -> Vec<Fx> {
            let mut fx = Vec::new();
            self.m.step(event, self.now, &mut fx);
            fx
        }
        fn tick(&mut self, ms: u64) -> Vec<Fx> {
            self.now += ms;
            self.feed(Event::Tick)
        }
        fn answer(&mut self, request: AcbRequest, invoked: bool) -> Vec<Fx> {
            self.feed(Event::Answered { request, invoked, completed_ms: self.now })
        }
        /// Gate passed, loaded, video known: the MediaId step is in flight.
        fn at_media_id(dass: bool) -> (Rig, AcbRequest) {
            let mut rig = Rig::new(dass);
            let request = run_of(&rig.feed(Event::Start)).0;
            rig.answer(request, true);
            rig.feed(Event::Video(VIDEO));
            let request = run_of(&rig.feed(Event::Loaded { media_id: "m1".into() }));
            assert_eq!(request.1, Step::MediaId);
            (rig, request.0)
        }
    }
    fn run_of(fx: &[Fx]) -> (AcbRequest, Step, Op) {
        fx.iter().find_map(|fx| match fx {
            Fx::Run { request, step, op, .. } => Some((*request, *step, op.clone())),
            _ => None,
        }).unwrap_or_else(|| panic!("no step in {fx:?}"))
    }

    #[test]
    fn the_sequence_runs_in_order_with_the_measured_pace() {
        let mut rig = Rig::new(false);
        let (request, step, op) = run_of(&rig.feed(Event::Start));
        assert_eq!((request.number(), step, op), (1, Step::Check, Op::Check { dass: false }));
        assert_eq!(rig.answer(request, true), [Fx::Answered { step: Step::Check, invoked: true }, Fx::GatePassed]);
        // Nothing runs before the media is loaded, whatever time passes.
        assert!(rig.tick(10_000).is_empty());
        rig.feed(Event::Video(VIDEO));
        let fx = rig.feed(Event::Loaded { media_id: "m1".into() });
        let mut trace = Vec::new();
        let mut run = run_of(&fx);
        for _ in 0..5 {
            trace.push((rig.now, run.0.number(), run.1));
            rig.answer(run.0, true);
            let wait = if run.1 == Step::VideoData { 300 } else { 200 };
            if run.1 == Step::Display { break; }
            assert!(rig.tick(wait - 1).is_empty(), "paced {wait} ms after {:?}", run.1);
            run = run_of(&rig.tick(1));
        }
        assert_eq!(trace, [
            (11_000, 2, Step::MediaId), (11_200, 3, Step::Loaded), (11_400, 4, Step::Connect),
            (11_600, 5, Step::VideoData), (11_900, 6, Step::Display)]);
        assert_eq!(run.2, Op::Display);
        assert!(rig.m.bound());
        assert!(rig.tick(100_000).is_empty(), "a bound plane has no deadlines");
    }

    #[test]
    fn the_connect_slot_calls_only_when_armed_and_is_timed_either_way() {
        for dass in [false, true] {
            let (mut rig, mut request) = Rig::at_media_id(dass);
            let mut ops = Vec::new();
            for _ in 0..4 {
                rig.answer(request, true);
                let (next, _, op) = run_of(&rig.tick(300));
                ops.push(op);
                request = next;
            }
            assert_eq!(ops[1], Op::Connect { enabled: dass });
            assert!(matches!(ops[2], Op::VideoData { .. }));
        }
        assert_eq!(run_of(&Rig::new(true).feed(Event::Start)).2, Op::Check { dass: true });
    }

    #[test]
    fn a_pace_is_anchored_to_the_completion_not_to_the_tick() {
        let (mut rig, request) = Rig::at_media_id(false);
        rig.feed(Event::Answered { request, invoked: true, completed_ms: rig.now + 50 });
        assert!(rig.tick(249).is_empty());
        assert_eq!(run_of(&rig.tick(1)).1, Step::Loaded);
    }

    #[test]
    fn video_after_loaded_waits_and_then_binds_once() {
        let mut rig = Rig::new(false);
        let request = run_of(&rig.feed(Event::Start)).0;
        rig.answer(request, true);
        assert_eq!(rig.feed(Event::Loaded { media_id: "m1".into() }), [Fx::WaitingVideo]);
        assert!(rig.m.waiting_for_video());
        assert!(rig.tick(VIDEO_TIMEOUT_MS - 1).is_empty());
        let fx = rig.feed(Event::Video(VIDEO));
        assert_eq!(run_of(&fx).1, Step::MediaId);
        assert!(!rig.m.waiting_for_video());
        // A second report while a step is in flight changes nothing.
        assert!(rig.feed(Event::Video(VIDEO)).is_empty());
        let fx = rig.tick(VIDEO_TIMEOUT_MS);
        assert_eq!(fx, [Fx::Failed(Failure::Timeout)], "the step budget, not the video wait, now applies");
    }

    #[test]
    fn no_video_facts_fail_at_the_video_deadline() {
        let mut rig = Rig::new(false);
        let request = run_of(&rig.feed(Event::Start)).0;
        rig.answer(request, true);
        rig.feed(Event::Loaded { media_id: "m1".into() });
        assert_eq!(rig.tick(VIDEO_TIMEOUT_MS), [Fx::Failed(Failure::VideoTimeout)]);
        assert!(rig.feed(Event::Video(VIDEO)).is_empty(), "an ended sequence stays ended");
    }

    #[test]
    fn a_refused_late_or_unanswered_step_ends_the_sequence() {
        let (mut rig, request) = Rig::at_media_id(false);
        assert_eq!(rig.answer(request, false),
            [Fx::Answered { step: Step::MediaId, invoked: false }, Fx::Failed(Failure::Refused)]);
        let (mut rig, request) = Rig::at_media_id(false);
        let late = rig.now + OP_TIMEOUT_MS;
        assert_eq!(rig.feed(Event::Answered { request, invoked: true, completed_ms: late }),
            [Fx::Failed(Failure::Timeout)], "a late answer is not recorded as an answer");
        let (mut rig, _) = Rig::at_media_id(false);
        assert!(rig.tick(OP_TIMEOUT_MS - 1).is_empty());
        assert_eq!(rig.tick(1), [Fx::Failed(Failure::Timeout)]);
    }

    #[test]
    fn an_answer_for_another_request_is_inert() {
        let (mut rig, request) = Rig::at_media_id(false);
        assert!(rig.answer(AcbRequest(request.number() + 1), true).is_empty());
        assert!(rig.answer(AcbRequest(0), true).is_empty());
        assert!(rig.m.running());
        assert_eq!(rig.answer(request, true).len(), 1);
        assert!(rig.answer(request, true).is_empty(), "an answered request is answered once");
    }

    #[test]
    fn cancel_stops_every_stage() {
        for at in 0..5 {
            let (mut rig, mut request) = Rig::at_media_id(true);
            for _ in 0..at {
                rig.answer(request, true);
                request = run_of(&rig.tick(300)).0;
            }
            rig.feed(Event::Cancel);
            assert!(!rig.m.running());
            assert!(rig.answer(request, true).is_empty());
            assert!(rig.tick(100_000).is_empty());
            assert!(rig.feed(Event::Video(VIDEO)).is_empty());
        }
    }

    #[test]
    fn a_deferred_bind_is_offered_again_within_its_first_budget() {
        let (mut rig, request) = Rig::at_media_id(false);
        let started = rig.now;
        assert!(rig.feed(Event::Deferred { request }).is_empty());
        assert!(rig.m.running());
        assert!(rig.tick(RETRY_MS - 1).is_empty(), "no offer before the retry interval");
        let fx = rig.tick(1);
        let (again, step, op) = run_of(&fx);
        assert_eq!((step, again.number()), (Step::MediaId, request.number() + 1));
        assert!(matches!(op, Op::MediaId { .. }));
        assert!(matches!(fx[0], Fx::Run { retry: true, .. }));
        // The retry does not extend the budget the first dispatch started.
        rig.feed(Event::Deferred { request: again });
        rig.now = started + OP_TIMEOUT_MS - 1;
        rig.feed(Event::Tick);
        rig.now = started + OP_TIMEOUT_MS;
        assert_eq!(rig.feed(Event::Tick), [Fx::Failed(Failure::Timeout)]);
    }

    #[test]
    fn a_deferred_bind_that_is_then_granted_continues_the_sequence() {
        let (mut rig, request) = Rig::at_media_id(false);
        rig.feed(Event::Deferred { request });
        let (again, _, _) = run_of(&rig.tick(RETRY_MS));
        assert!(rig.answer(request, true).is_empty(), "the deferred request is dead");
        rig.answer(again, true);
        assert_eq!(run_of(&rig.tick(PACE_MS)).1, Step::Loaded);
    }

    #[test]
    fn only_the_bind_step_can_be_deferred() {
        let (mut rig, request) = Rig::at_media_id(false);
        rig.answer(request, true);
        let (loaded, step, _) = run_of(&rig.tick(PACE_MS));
        assert_eq!(step, Step::Loaded);
        assert!(rig.feed(Event::Deferred { request: loaded }).is_empty());
        assert!(rig.tick(RETRY_MS).is_empty(), "a deferral of another step is ignored");
        assert_eq!(rig.answer(loaded, true).len(), 1);
    }

    #[test]
    fn the_gate_needs_every_fact_and_the_entry_point_only_for_dass() {
        let vp_acb = plx_platform::tv::sink::VP_ACB;
        for dass in [false, true] {
            assert!(!gate(dass, vp_acb, true, 0, true, false).ready, "a created handle is not an initialized one");
            assert!(!gate(dass, vp_acb, false, 0, true, true).ready);
            assert!(!gate(dass, vp_acb, true, 10, true, true).ready, "the measured sequence initialized player type 0");
            assert!(!gate(dass, plx_platform::tv::sink::VP_EXPORTED, true, 0, true, true).ready);
            assert!(gate(dass, vp_acb, true, 0, true, true).ready);
        }
        assert!(gate(false, vp_acb, true, 0, false, true).ready);
        assert!(!gate(true, vp_acb, true, 0, false, true).ready);
    }
}
