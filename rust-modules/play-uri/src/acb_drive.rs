//! The runner half of the shared ACB bind sequence: performs one [`Op`] of `acb_bind` through the
//! display sink and the plane owner, and says how it went. The probe's driver and the production
//! backend both call it, so a step's native calls, their order and their log lines exist once.
//!
//! This is IO (it calls the sink), so it is a runner and never a machine: it decides nothing about
//! the sequence. It does not know which backend called it; the caller supplies the descriptor and
//! stamps the log lines.
use crate::acb_bind::{gate, Op};
use crate::uri_protocol::{MediaId, VideoFacts};
use plx_base::task::MainThread;
use plx_platform::tv::plane::{BindPolicy, BindVerdict, PlaneHandle, PlaneOwner, Refusal};
use plx_platform::tv::sink::DisplaySink;

/// How the caller dresses a log line: `Timed` lines carry the caller's own `t=` stamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stamp { Plain, Timed }

/// The video-data envelope to forward, and what it is called in the log.
pub struct Descriptor { pub text: String, pub label: &'static str }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The native call(s) ran; whether the library accepted them.
    Invoked(bool),
    /// The plane owner asked the bind to wait for a previous release: offer it again later.
    Deferred,
    /// The plane owner refused the bind. Nothing was called.
    Refused(Refusal),
}

pub struct Executed {
    pub verdict: Verdict,
    /// The plane owner granted the bind, so the caller owes the lifecycle an `AcbTouched`: the
    /// binding is this session's from here on, whatever the library answers next.
    pub touched: bool,
}

impl Executed {
    fn invoked(accepted: bool) -> Self { Self { verdict: Verdict::Invoked(accepted), touched: false } }
}

/// Perform `op`. `descriptor` resolves the video-data envelope for the media id and the observed
/// facts, or says why it cannot (nothing is fabricated here).
pub fn execute(op: &Op, mt: &MainThread, sink: &dyn DisplaySink, plane: &PlaneOwner,
    descriptor: &mut dyn FnMut(&MediaId, VideoFacts) -> Result<Descriptor, &'static str>,
    log: &mut dyn FnMut(Stamp, &str)) -> Executed {
    // SAFETY (every `unsafe` below): `mt` witnesses the main thread, the one the verbs require,
    // and each pointer argument is a NUL-terminated string that outlives the call.
    match op {
        Op::Check { dass } => {
            // The URI engine's own handle (player type 0), booted beside Feed's: its facts and the
            // library's confirmation of its `AcbAPI_initialize` are boot facts the owner holds.
            let facts = plane.facts_of(PlaneHandle::Uri);
            #[cfg(feature = "devtriggers")]
            let dass_available = unsafe { sink.plane_probe_dass_available(mt) };
            #[cfg(not(feature = "devtriggers"))]
            let dass_available = false;
            let check = gate(*dass, sink.window_mode(), facts.handle, facts.player_type, dass_available,
                plane.initialized(PlaneHandle::Uri));
            log(Stamp::Timed, &format!("acb gate mode={} boot_handle={} actual_ptype={} dass_available={} initialized={}",
                check.mode, check.boot_handle, check.ptype, check.dass_available, check.initialized));
            Executed::invoked(check.ready)
        }
        Op::MediaId { media_id } => {
            let Ok(id) = std::ffi::CString::new(&**media_id) else { return Executed::invoked(false) };
            // One binding at a time: the plane owner grants the bind only once the previous
            // binding's release (either backend's) is proven.
            match plane.bind(sink, mt, PlaneHandle::Uri, BindPolicy::Strict, media_id) {
                BindVerdict::Granted => {}
                BindVerdict::Deferred => return Executed { verdict: Verdict::Deferred, touched: false },
                BindVerdict::Refused(why) => return Executed { verdict: Verdict::Refused(why), touched: false },
            }
            let accepted = unsafe { sink.plane_media_id(mt, id.as_ptr()) } == 1;
            Executed { verdict: Verdict::Invoked(accepted), touched: true }
        }
        Op::Loaded => Executed::invoked(unsafe { sink.plane_loaded(mt) } == 1),
        Op::Connect { enabled: false } => {
            log(Stamp::Plain, "acb control connectDass skipped (same timed slot)");
            Executed::invoked(true)
        }
        #[cfg(feature = "devtriggers")]
        Op::Connect { enabled: true } => Executed::invoked(unsafe { sink.plane_probe_connect_dass(mt) } == 1),
        #[cfg(not(feature = "devtriggers"))]
        Op::Connect { enabled: true } => {
            log(Stamp::Plain, "acb connectDass is a developer arm and is not in this build");
            Executed::invoked(false)
        }
        Op::VideoData { media_id, video } => {
            log(Stamp::Plain, &format!("acb video facts width={} height={} frameRate={} bitRate={:?}",
                video.width, video.height, video.frame_rate, video.bit_rate));
            let described = match descriptor(media_id, *video) {
                Ok(described) => described,
                Err(why) => {
                    log(Stamp::Plain, &format!("acb invalid descriptor: {why}"));
                    return Executed::invoked(false);
                }
            };
            log(Stamp::Plain, &format!("acb descriptor={}", described.label));
            let Ok(text) = std::ffi::CString::new(described.text) else { return Executed::invoked(false) };
            Executed::invoked(unsafe { sink.plane_send_video_data(mt, text.as_ptr()) } == 1)
        }
        // The window and PLAYING go through the plane owner, which keeps their two task ids so the
        // caller can ask `plane.shown(..)` whether the firmware has completed both.
        Op::Display => Executed::invoked(plane.show(sink, mt)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_platform::tv::plane::{HandleSeat, PlaneFacts};
    use plx_platform::tv::sink::{PlaneUnload, ScriptedPlaneSink, VP_ACB};

    const VIDEO: VideoFacts = VideoFacts { width: 1280, height: 720, frame_rate: 24.0, bit_rate: None };

    struct World { sink: ScriptedPlaneSink, plane: PlaneOwner, mt: MainThread, lines: Vec<(Stamp, String)> }
    impl World {
        fn new(player_type: i32) -> Self {
            Self::booted(player_type, true)
        }
        /// The URI handle booted with this player type, and the library's confirmation of its
        /// initialize as `initialized`.
        fn booted(player_type: i32, initialized: bool) -> Self {
            let plane = PlaneOwner::new();
            plane.boot_handle(PlaneHandle::Uri, PlaneFacts { handle: true, player_type },
                HandleSeat { id: 0, initialized });
            Self { sink: ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 }), plane,
                // SAFETY: a test thread stands in for the main thread.
                mt: unsafe { MainThread::assume() }, lines: Vec::new() }
        }
        fn run(&mut self, op: &Op, descriptor: Result<Descriptor, &'static str>) -> Executed {
            let mut descriptor = Some(descriptor);
            let lines = &mut self.lines;
            execute(op, &self.mt, &self.sink, &self.plane,
                &mut |_, _| descriptor.take().expect("one descriptor"),
                &mut |stamp, line| lines.push((stamp, line.to_owned())))
        }
    }
    fn media(id: &str) -> MediaId { id.into() }
    fn fixture() -> Result<Descriptor, &'static str> { Ok(Descriptor { text: "{}".into(), label: "test-envelope" }) }

    #[test]
    fn the_gate_is_stamped_and_reads_the_uri_handle_the_owner_holds() {
        let mut world = World::new(0);
        assert_eq!(world.run(&Op::Check { dass: false }, fixture()).verdict, Verdict::Invoked(true));
        assert_eq!(world.lines, [(Stamp::Timed, format!("acb gate mode={VP_ACB} boot_handle=true actual_ptype=0 dass_available=false initialized=true"))]);
        let mut world = World::booted(0, false);
        assert_eq!(world.run(&Op::Check { dass: false }, fixture()).verdict, Verdict::Invoked(false),
            "a created handle is not an initialized one");
        let mut world = World::new(10);
        assert_eq!(world.run(&Op::Check { dass: false }, fixture()).verdict, Verdict::Invoked(false));
        assert!(world.sink.calls().is_empty(), "the gate calls no ACB verb that changes the plane");
    }

    #[test]
    fn the_gate_reads_the_uri_handle_and_not_feeds() {
        let mut world = World::new(0);
        // Feed's handle booted beside it with player type 10 (every normal boot): the gate is the
        // URI handle's, so Feed's type does not fail it.
        world.plane.boot(PlaneFacts { handle: true, player_type: 10 });
        assert_eq!(world.run(&Op::Check { dass: false }, fixture()).verdict, Verdict::Invoked(true));
        assert!(world.lines[0].1.contains("actual_ptype=0"), "{:?}", world.lines);
    }

    #[test]
    fn a_granted_bind_touches_and_calls_the_verbs_in_the_measured_order() {
        let mut world = World::new(0);
        let bound = world.run(&Op::MediaId { media_id: media("m1") }, fixture());
        assert_eq!((bound.verdict, bound.touched), (Verdict::Invoked(true), true));
        assert_eq!(world.run(&Op::Loaded, fixture()).verdict, Verdict::Invoked(true));
        assert_eq!(world.run(&Op::Connect { enabled: false }, fixture()).verdict, Verdict::Invoked(true));
        assert_eq!(world.run(&Op::VideoData { media_id: media("m1"), video: VIDEO }, fixture()).verdict, Verdict::Invoked(true));
        assert_eq!(world.run(&Op::Display, fixture()).verdict, Verdict::Invoked(true));
        assert_eq!(world.sink.calls(), ["plane_media_id m1", "plane_loaded", "plane_send_video_data len=2", "plane_finish"]);
        assert_eq!(world.lines.iter().map(|(_, line)| line.as_str()).collect::<Vec<_>>(), [
            "acb control connectDass skipped (same timed slot)",
            "acb video facts width=1280 height=720 frameRate=24 bitRate=None",
            "acb descriptor=test-envelope"]);
    }

    #[test]
    fn the_display_step_goes_through_the_owner_which_then_waits_for_both_completions() {
        use plx_platform::tv::plane::Shown;
        use plx_platform::tv::sink::{SCRIPTED_DISPLAY_TASK, SCRIPTED_PLAYING_TASK};
        let mut world = World::new(0);
        world.run(&Op::MediaId { media_id: media("m1") }, fixture());
        assert_eq!(world.plane.shown(&world.sink, &world.mt), Shown::No, "no display step ran yet");
        assert_eq!(world.run(&Op::Display, fixture()).verdict, Verdict::Invoked(true));
        let waiting = Shown::Waiting { display: Some(SCRIPTED_DISPLAY_TASK.into()), playing: Some(SCRIPTED_PLAYING_TASK.into()) };
        assert_eq!(world.plane.shown(&world.sink, &world.mt), waiting, "dispatched is not completed");
        world.sink.mailbox.push(SCRIPTED_DISPLAY_TASK.into(), 1);
        world.sink.mailbox.push(SCRIPTED_PLAYING_TASK.into(), 1);
        assert_eq!(world.plane.shown(&world.sink, &world.mt), Shown::Done);
        // A refused display answers "not invoked" and leaves nothing to wait for.
        let mut world = World::new(0);
        world.run(&Op::MediaId { media_id: media("m1") }, fixture());
        world.sink.failing.lock().unwrap().push("plane_finish");
        assert_eq!(world.run(&Op::Display, fixture()).verdict, Verdict::Invoked(false));
        assert_eq!(world.plane.shown(&world.sink, &world.mt), Shown::No);
    }

    #[test]
    fn a_bind_over_a_bound_plane_is_refused_without_touching_it() {
        let mut world = World::new(0);
        assert!(world.run(&Op::MediaId { media_id: media("m1") }, fixture()).touched);
        let second = world.run(&Op::MediaId { media_id: media("m2") }, fixture());
        assert_eq!((second.verdict, second.touched), (Verdict::Refused(Refusal::Bound), false));
        assert_eq!(world.sink.calls(), ["plane_media_id m1"], "a refused bind calls nothing");
    }

    #[test]
    fn a_bind_during_a_release_is_deferred_and_a_failed_verb_is_not_accepted() {
        let mut world = World::new(0);
        world.run(&Op::MediaId { media_id: media("m1") }, fixture());
        world.plane.unload(&world.sink, &world.mt);
        assert_eq!(world.plane.phase(), plx_platform::tv::plane::Phase::Releasing);
        let waiting = world.run(&Op::MediaId { media_id: media("m2") }, fixture());
        assert_eq!((waiting.verdict, waiting.touched), (Verdict::Deferred, false));
        let mut world = World::new(0);
        world.sink.failing.lock().unwrap().push("plane_media_id");
        let failed = world.run(&Op::MediaId { media_id: media("m1") }, fixture());
        assert_eq!((failed.verdict, failed.touched), (Verdict::Invoked(false), true),
            "the plane is this session's even when the library refused the id: it must be unloaded");
    }

    #[test]
    fn an_unavailable_descriptor_is_never_replaced_by_one_made_up_here() {
        let mut world = World::new(0);
        let op = Op::VideoData { media_id: media("m1"), video: VIDEO };
        assert_eq!(world.run(&op, Err("no usable envelope")).verdict, Verdict::Invoked(false));
        assert!(world.sink.calls().is_empty());
        assert_eq!(world.lines.last().unwrap().1, "acb invalid descriptor: no usable envelope");
    }
}
