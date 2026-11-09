//! The URI-mode probe's driver (issue #1064, the first test): the loop which feeds
//! [`UriProbe::step`] its events, executes the effects each step asks for, and feeds every effect
//! that failed back as the next event. The machine, its events and its effects are
//! [`super::uriprobe`]; the transport it talks through is [`plx_platform::tv::uri`]. The caller
//! owns the session slot, the exact start attempt, and settlement; this file holds the IO while
//! the machine stays pure.
//!
//! **Where it runs.** The media adapter opens it when the developer trigger is armed, then calls
//! [`UriSession::advance`] each frame. It calls [`UriSession::stop`] on Back or an app switch. A
//! session that ends on its own (an error, a timeout, the end
//! of the stream) stops logically at once. The ownership reducer keeps its adapter slot and window
//! until the worker acknowledges successful unregister; unresolved release blocks another start.
//! A deadline quarantines ownership, and a late successful acknowledgement still permits cleanup.
//! ACB experiments add a separate fence: after bus release they dispatch UNLOADED once through the
//! plane owner and are released only when the firmware's completion for that task arrives; a failed
//! completion or the plane owner's deadline retains the quarantine until process exit.
//!
//! **What stays out of it.** No Engine thread runs, so nothing is reported to Plex. The URL is held by [`UriSession`] for
//! the `load` payload and nowhere else: no `Debug`, no log line, and the payload is built by
//! [`super::uri_protocol::build_load`] straight into the transport. Every line the machine writes is complete and printed
//! verbatim; the few lines this driver adds (the window) carry no URL either.
//!
//! The caller decides whether to enable the probe.

use super::uri_lifecycle::{Event as LifeEvent, Fx as LifeFx, Lifecycle};
use super::uriprobe::{AbrBps, Config, Firmware, Kind, Scheme, Settle, UriCall, UriEffect, UriEvent, UriProbe, Variant, AcbOp};
use plx_base::task::MainThread;
use plx_platform::tv::plane::PlaneOwner;
use plx_platform::tv::sink::{DisplaySink, VP_EXPORTED};
use plx_platform::tv::uri::{CloseStatus, UriMedia, UriPoll};
use std::collections::VecDeque;
use std::os::raw::c_int;
use std::time::Instant;

/// The exported window is placed over the whole canvas; the coded size the service reports for the
/// picture (`videoInfo`) only changes the SOURCE rectangle (`engine::place_exported` does the same).
const CANVAS: (c_int, c_int) = (1_920, 1_080);

/// Inputs from the caller. The URL is kept private and erased on logical completion.
pub struct DriverConfig<A: Copy> {
    pub attempt: A,
    pub variant: Variant,
    /// The native adaptive-streaming rates the `load` payload carries (none unless the trigger
    /// named them).
    pub bps: AbrBps,
    pub kind: Kind,
    pub url: String,
    pub app_id: String,
    /// The set's firmware, read by the caller from the platform (the machine reads no global).
    pub firmware: Firmware,
    pub transport: &'static dyn UriMedia,
    pub sink: &'static dyn DisplaySink,
    pub log: fn(&str),
    /// The process's plane owner: the boot facts the ACB gate reads, and the one binding/release.
    pub plane: &'static PlaneOwner,
    pub release_allowance_ms: u64,
}

/// One running probe. No `Debug`: the held URL may include a token.
pub struct UriSession<A: Copy> {
    probe: UriProbe,
    lifecycle: Lifecycle,
    acb_source: Option<super::uri_protocol::AcbSource>,
    transport: &'static dyn UriMedia,
    /// The media URL, for the `load` payload (re-built if the register ladder sends it again). Taken
    /// out the moment the session ends.
    url: String,
    app_id: String,
    /// The route-start attempt this session stands in for; the machine settles it exactly once.
    attempt: A,
    sink: &'static dyn DisplaySink,
    log: fn(&str),
    plane: &'static PlaneOwner,
    release_allowance_ms: u64,
    /// An exported window was created and is owed a `destroy_window`.
    window: bool,
    /// Events produced by effects that failed on the spot (the transport refused to send). They are
    /// fed on the next [`advance`](Self::advance), never inside the call that started the session,
    /// so a refusal cannot settle the attempt before its start has even returned.
    inbox: VecDeque<UriEvent>,
    /// Last correlated, nonnegative service clock; never presentation evidence.
    reported_position_ms: Option<i64>,
    clock: Instant,
}

impl<A: Copy> UriSession<A> {
    /// Create the exported window for the variants which use one, build the machine and take its
    /// first step (`Start`: the `armed` line and the first registration).
    pub fn open(input: DriverConfig<A>, mt: &MainThread) -> UriSession<A> {
        let DriverConfig { attempt, variant, bps, kind, url, app_id, firmware, transport, sink, log, plane, release_allowance_ms } = input;
        let clock = Instant::now();
        let mut window_id = None;
        let mut created = false;
        if !variant.acb() && variant != Variant::NoWindow && sink.window_mode() == VP_EXPORTED {
            if let Some(id) = super::session_io::create_exported_window(sink, mt) {
                created = true;
                window_id = Some(id);
            } else {
                log("uriprobe: t=0 window id=none (the television refused to create one)");
            }
        }
        let config = Config {
            variant,
            scheme: Scheme::of(&url),
            kind,
            window_id: window_id.clone(),
            app_id: app_id.clone(),
            firmware,
            transport: variant.drives(),
            bps,
        };
        let mut session = UriSession {
            probe: UriProbe::new(config),
            lifecycle: Lifecycle::new(),
            acb_source: None,
            transport,
            url,
            app_id,
            attempt,
            sink,
            log,
            plane,
            release_allowance_ms,
            window: created,
            inbox: VecDeque::new(),
            reported_position_ms: None,
            clock,
        };
        if let Some(id) = window_id {
            (session.log)(&format!("uriprobe: t=0 window id={id}"));
            session.place(mt, CANVAS.0, CANVAS.1);
        }
        session.feed(UriEvent::Start, &mut |_, _| { debug_assert!(false, "the first step settles nothing"); false }, mt, 0);
        session
    }

    pub fn now_ms(&self) -> u64 {
        u64::try_from(self.clock.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Retain observed plane ownership through stop; ACB quarantine lasts until process exit.
    pub fn plane_open(&self) -> bool {
        self.lifecycle.plane_open()
    }

    pub fn attempt(&self) -> A {
        self.attempt
    }

    pub fn reported_position_ms(&self) -> Option<i64> { self.reported_position_ms }

    pub fn acb_exit_quarantine(&self) -> bool { self.lifecycle.acb_exit_quarantine() }

    pub fn closing(&self) -> bool {
        matches!(self.lifecycle.phase(), super::uri_lifecycle::Phase::Closing | super::uri_lifecycle::Phase::Quarantined)
    }

    pub fn ready_to_release(&self) -> bool { self.probe.is_done() && self.lifecycle.released() }

    pub fn is_done(&self) -> bool { self.probe.is_done() }

    pub fn window_created(&self) -> bool { self.window }

    pub fn ownership_released(&self) -> bool { self.lifecycle.released() }

    #[cfg(any(test, feature = "test-support"))]
    pub fn touch_acb_for_test(&mut self, now_ms: u64) {
        let session = self.lifecycle.session().expect("registered URI session");
        self.lifecycle(LifeEvent::AcbTouched { session }, now_ms);
    }

    /// End the logical session. Physical ownership remains until `ready_to_release`.
    pub fn stop_at(&mut self, settle: &mut dyn FnMut(A, Settle) -> bool, mt: &MainThread, now_ms: u64) {
        self.inbox.clear();
        self.feed(UriEvent::Back, settle, mt, now_ms);
        self.drain(settle, mt, now_ms);
    }

    pub fn stop(&mut self, settle: &mut dyn FnMut(A, Settle) -> bool, mt: &MainThread) {
        self.stop_at(settle, mt, self.now_ms());
    }

    /// Re-place the exported window for a coded picture of `width` x `height`.
    fn place(&self, mt: &MainThread, width: c_int, height: c_int) {
        let rv = super::session_io::place_exported_window(self.sink, mt, (width, height), CANVAS);
        (self.log)(&format!("uriprobe: t={} window placed src={width}x{height} rv={rv}", self.now_ms()));
    }

    /// One frame: the replies which arrived, then the clock. Everything a reply or a deadline sets
    /// off, including the events of calls that failed on the spot, runs to the end before this
    /// returns.
    pub fn poll(&self) -> Result<UriPoll, String> {
        match self.lifecycle.poll_session() {
            Some(session) => self.transport.poll(session),
            None => Ok(UriPoll { replies: Vec::new(), close: CloseStatus::Released }),
        }
    }

    pub fn advance(&mut self, settle: &mut dyn FnMut(A, Settle) -> bool, mt: &MainThread, poll: Result<UriPoll, String>, now_ms: u64) {
        let polled_session = self.lifecycle.poll_session();
        match poll {
            Ok(poll) => {
                for reply in poll.replies {
                    self.lifecycle(LifeEvent::Reply(reply), now_ms);
                    self.drain(settle, mt, now_ms);
                }
                if let Some(session) = polled_session {
                    self.lifecycle(LifeEvent::Status { session, status: poll.close }, now_ms);
                    self.drain(settle, mt, now_ms);
                }
            }
            Err(_) => {
                if let Some(session) = polled_session { self.lifecycle(LifeEvent::PollFailed {session}, now_ms); }
            }
        }
        self.lifecycle(LifeEvent::Tick { now_ms }, now_ms);
        if let Some(session) = self.lifecycle.session().filter(|session| self.lifecycle.acb_fence_due(*session)) {
            // UNLOADED is in flight: drain the firmware's completions into the plane owner.
            let plane = self.plane.pump(self.sink, mt);
            self.lifecycle(LifeEvent::AcbPlane { session, plane }, now_ms);
        }
        self.inbox.push_back(UriEvent::Tick);
        self.drain(settle, mt, now_ms);
    }

    /// Execute the ownership machine's typed effects; every immediate answer is addressed too.
    fn lifecycle(&mut self, event: LifeEvent, now_ms: u64) {
        let mut events = VecDeque::from([event]);
        while let Some(event) = events.pop_front() {
            let mut effects = Vec::new();
            self.lifecycle.step(event, &mut effects);
            for effect in effects {
                match effect {
                    LifeFx::Open { .. } => {
                        events.push_back(super::session_io::transport_answer(&effect, self.transport, &self.url, &self.app_id).expect("open answer"));
                    }
                    LifeFx::Call { request, ref call, .. } => {
                        self.inbox.push_back(UriEvent::Dispatched { method: super::uri_protocol::Method::of(call.method()), kind: match &call { UriCall::SelectTrack {kind,..} => Some(*kind), _ => None }, request });
                        events.push_back(super::session_io::transport_answer(&effect, self.transport, &self.url, &self.app_id).expect("call answer"));
                    }
                    LifeFx::Close { .. } => {
                        events.push_back(super::session_io::transport_answer(&effect, self.transport, &self.url, &self.app_id).expect("close answer"));
                    }
                    LifeFx::Deliver { method, ok, mut message } => {
                        if ok && message.accepted() != Some(false) {
                            if let Some(position) = message.position_ms().filter(|p| *p >= 0) {
                                self.reported_position_ms = Some(position);
                            }
                        }
                        if let Some(source) = message.acb_source.take() { self.acb_source = Some(source); }
                        self.inbox.push_back(UriEvent::Reply { method, ok, message });
                    }
                    LifeFx::UnloadAcb {session} => {
                        // The actual actuator is main-thread only; enqueue its addressed tail.
                        self.inbox.push_back(UriEvent::AcbReleaseTail {session});
                    }
                    LifeFx::AcbReleased => (self.log)("uriprobe: acb=released bus=released plane-release=proven"),
                    LifeFx::AcbQuarantined(why) => (self.log)(&format!("uriprobe: acb=quarantined bus=released reason={why} need-relaunch")),
                    LifeFx::Released => { self.inbox.push_back(UriEvent::Closed); }
                    LifeFx::Fault => {
                        (self.log)(&format!("uriprobe: t={now_ms} transport ownership unresolved or reply stream lost"));
                        self.inbox.push_back(UriEvent::Back);
                    }
                }
            }
        }
    }

    fn drain(&mut self, settle: &mut dyn FnMut(A, Settle) -> bool, mt: &MainThread, now_ms: u64) {
        while let Some(event) = self.inbox.pop_front() {
            self.feed(event, settle, mt, now_ms);
        }
    }

    /// Step the machine once and execute what it asks for, in order.
    fn feed(&mut self, event: UriEvent, settle: &mut dyn FnMut(A, Settle) -> bool, mt: &MainThread, now_ms: u64) {
        if let UriEvent::AcbReleaseTail {session} = event {
            if !self.lifecycle.acb_tail_due(session) { return; }
            let plane = self.plane.unload(self.sink, mt);
            self.lifecycle(LifeEvent::AcbUnloadDispatched {session, plane}, now_ms);
            return;
        }
        let mut effects = Vec::new();
        self.probe.step(event, now_ms, &mut effects);
        for effect in effects {
            if let Some(failed) = self.execute(effect, settle, mt, now_ms) {
                self.inbox.push_back(failed);
            }
        }
        if self.probe.is_done() { self.url = String::new(); }
    }

    /// Run one effect. `Some` is the event of an effect the transport refused to carry out.
    fn execute(&mut self, effect: UriEffect, settle: &mut dyn FnMut(A, Settle) -> bool, mt: &MainThread, now_ms: u64) -> Option<UriEvent> {
        match effect {
            UriEffect::Acb {request, op} => {
                let invoked = self.acb_execute(&op, mt, now_ms);
                // Round up so a frame-start stamp never shortens the requested post-call spacing.
                Some(UriEvent::AcbAnswered {request, invoked, completed_ms: self.now_ms().saturating_add(1)})
            }
            UriEffect::Open(shape) => { self.lifecycle(LifeEvent::Open(shape), now_ms); None }
            UriEffect::Call(call) => { self.lifecycle(LifeEvent::Call(call), now_ms); None }
            UriEffect::Close => {
                let allowance_ms = self.release_allowance_ms;
                self.lifecycle(LifeEvent::Close { now_ms, allowance_ms }, now_ms);
                None
            }
            UriEffect::Settle(how) => {
                if !settle(self.attempt, how) {
                    // A Back already moved the route on (`cancel_play` runs before the stop), or a
                    // later start replaced this attempt: nothing is left to settle, and that is fine.
                    (self.log)(&format!("uriprobe: t={now_ms} the route no longer waits on this attempt"));
                }
                None
            }
            UriEffect::PlaceWindow { width, height } => {
                if self.window {
                    self.place(mt, c_int::try_from(width).unwrap_or(CANVAS.0), c_int::try_from(height).unwrap_or(CANVAS.1));
                }
                None
            }
            UriEffect::Log(line) => {
                (self.log)(&line);
                None
            }
        }
    }

    fn acb_execute(&mut self, op: &AcbOp, mt: &MainThread, now_ms: u64) -> bool {
        use super::acb_drive::{execute, Descriptor, Stamp, Verdict};
        let session = self.lifecycle.session();
        match op {
            AcbOp::Check {..} => (self.log)("uriprobe: ACB experiment requires known SDR H264720p24 fixture; fallback is explicit fixture data, not content detection"),
            // One binding at a time: the plane owner grants the bind only once the previous
            // binding's release (either backend's) is proven.
            AcbOp::MediaId {..} if session.is_none() => return false,
            _ => {}
        }
        let acb_source = self.acb_source.as_ref();
        let mut descriptor = |media_id: &super::uri_protocol::MediaId, video: super::uri_protocol::VideoFacts| {
            let observed = acb_source.filter(|source| &source.media_id == media_id && source.video == video);
            let label = if observed.is_some() { "observed-envelope" } else { "explicit-SDR720p24-fixture" };
            observed.map(|source| source.text().to_owned()).or_else(|| super::uri_protocol::build_acb_fixture_video(media_id, video))
                .map(|text| Descriptor { text, label })
                .ok_or("no observed envelope or exact SDR720p24 fixture")
        };
        let log = self.log;
        let executed = execute(op, mt, self.sink, self.plane, &mut descriptor, &mut |stamp, line| match stamp {
            Stamp::Plain => log(&format!("uriprobe: {line}")),
            Stamp::Timed => log(&format!("uriprobe: t={now_ms} {line}")),
        });
        if let (true, Some(session)) = (executed.touched, session) {
            self.lifecycle(LifeEvent::AcbTouched {session}, now_ms);
        }
        // A deferred or refused bind is a failed experiment here: the probe never retries a bind.
        match executed.verdict {
            Verdict::Invoked(accepted) => accepted,
            Verdict::Deferred => {
                (self.log)("uriprobe: acb bind not granted (Deferred); the experiment cannot bind");
                false
            }
            Verdict::Refused(why) => {
                (self.log)(&format!("uriprobe: acb bind not granted (Refused({why:?})); the experiment cannot bind"));
                false
            }
        }
    }

    /// Destroy the exported window only after the ownership reducer has observed release.
    pub fn release(self, mt: &MainThread) {
        let sink = self.sink;
        self.release_with(|| super::session_io::destroy_exported_window(sink, mt));
    }

    fn release_with(mut self, destroy: impl FnOnce()) {
        self.url = String::new();
        assert!(self.lifecycle.released(), "window ownership must be released first");
        if self.window { destroy(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_platform::tv::uri::{Shape, SessionId, RequestId, UriReply};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// A media URL as the planner hands it over: a `plex.direct` host, a port, a path and a token.
    const URL: &str = "https://192-168-1-5.0123456789abcdef.plex.direct:32400/library/parts/42/file.mkv?X-Plex-Token=SECRETTOKEN123&x=1";
    /// Every fragment of [`URL`] that must never reach the event log.
    const LEAKS: [&str; 9] =
        ["192.168", "192-168", "plex.direct", "SECRETTOKEN", "X-Plex-Token", "32400", "/library", "file.mkv", "0123456789abcdef"];

    /// The transport a test scripts: it records what the driver sends and hands back the replies the
    /// test queued. Leaked, because the driver holds a `&'static dyn UriMedia`.
    #[derive(Default)]
    struct Scripted {
        opens: Mutex<Vec<Shape>>,
        calls: Mutex<Vec<(&'static str, String)>>,
        closes: AtomicUsize,
        replies: Mutex<VecDeque<UriReply>>,
        refuse_open: Mutex<Option<String>>,
        current: Mutex<Option<SessionId>>,
        requests: Mutex<Vec<(&'static str, RequestId)>>,
        close_status: Mutex<Option<CloseStatus>>,
        hold_close: std::sync::atomic::AtomicBool,
        refuse_call: Mutex<Option<(&'static str, String)>>,
    }

    impl UriMedia for Scripted {
        fn open(&self, shape: Shape) -> Result<SessionId, String> {
            self.opens.lock().unwrap().push(shape);
            if let Some(error) = self.refuse_open.lock().unwrap().clone() { return Err(error); }
            let id = SessionId::new(self.opens.lock().unwrap().len() as u64);
            *self.current.lock().unwrap() = Some(id);
            *self.close_status.lock().unwrap() = Some(CloseStatus::Open);
            self.requests.lock().unwrap().clear();
            Ok(id)
        }
        fn call(&self, session: SessionId, request: RequestId, method: &'static str, payload: &str) -> Result<(), String> {
            assert_eq!(*self.current.lock().unwrap(), Some(session));
            self.calls.lock().unwrap().push((method, payload.to_owned()));
            self.requests.lock().unwrap().push((method, request));
            match self.refuse_call.lock().unwrap().clone() { Some((refused, text)) if refused == method => Err(text), _ => Ok(()) }
        }
        fn poll(&self, session: SessionId) -> Result<UriPoll, String> {
            assert_eq!(*self.current.lock().unwrap(), Some(session));
            Ok(UriPoll { replies: self.replies.lock().unwrap().drain(..).collect(), close: self.close_status.lock().unwrap().unwrap_or(CloseStatus::Open) })
        }
        fn close(&self, session: SessionId) -> Result<(), String> {
            assert_eq!(*self.current.lock().unwrap(), Some(session));
            self.closes.fetch_add(1, Ordering::SeqCst);
            *self.close_status.lock().unwrap() = Some(if self.hold_close.load(Ordering::SeqCst) { CloseStatus::Closing } else { CloseStatus::Released });
            Ok(())
        }
    }

    impl Scripted {
        fn leak() -> &'static Scripted {
            Box::leak(Box::default())
        }
        fn say(&self, method: &'static str, payload: &str) {
            self.replies.lock().unwrap().push_back(UriReply { session: self.current.lock().unwrap().unwrap(), request: if method == "register" { RequestId::new(0) } else { self.requests.lock().unwrap().iter().rev().find(|(m,_)| *m==method).map(|(_,id)| *id).unwrap_or(RequestId::new(0)) }, method, payload: payload.to_owned(), ok: true });
        }
        fn deny(&self, method: &'static str, text: &str) {
            self.replies.lock().unwrap().push_back(UriReply { session: self.current.lock().unwrap().unwrap(), request: if method == "register" { RequestId::new(0) } else { self.requests.lock().unwrap().iter().rev().find(|(m,_)| *m==method).map(|(_,id)| *id).unwrap_or(RequestId::new(0)) }, method, payload: text.to_owned(), ok: false });
        }
        fn sent(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().iter().map(|(method, _)| *method).collect()
        }
        fn payload_of(&self, method: &str) -> String {
            self.calls.lock().unwrap().iter().find(|(m, _)| *m == method).map(|(_, p)| p.clone()).unwrap_or_default()
        }
        fn opened(&self) -> Vec<Shape> {
            self.opens.lock().unwrap().clone()
        }
        fn closed(&self) -> usize {
            self.closes.load(Ordering::SeqCst)
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Status { Pending, Started, Failed }

    static NO_SINK: plx_platform::tv::sink::NoSink = plx_platform::tv::sink::NoSink;
    fn no_acb() -> &'static PlaneOwner { Box::leak(Box::new(PlaneOwner::new())) }

    /// A session over a scripted transport, with a local addressed start reducer.
    struct Rig {
        mt: MainThread,
        transport: &'static Scripted,
        attempt: u64,
        status: Status,
        session: Option<UriSession<u64>>,
        now: u64,
    }

    impl Rig {
        fn new(variant: Variant) -> Rig {
            Rig::build(variant, None)
        }

        /// `refuse_open`: the text the transport answers `open` with, from the very first call.
        fn build(variant: Variant, refuse_open: Option<&str>) -> Rig {
            Rig::build_with(variant, AbrBps::default(), refuse_open)
        }

        fn build_with(variant: Variant, bps: AbrBps, refuse_open: Option<&str>) -> Rig {
            let mt = unsafe { MainThread::assume() };
            let attempt = 17;
            let transport = Scripted::leak();
            *transport.refuse_open.lock().unwrap() = refuse_open.map(str::to_owned);
            let session = UriSession::open(DriverConfig {
                attempt, variant, bps, kind: Kind::Direct, url: URL.to_owned(),
                app_id: "com.example.plxnative".to_owned(), firmware: Firmware::default(), transport, sink: &NO_SINK,
                log: plx_base::eventlog::log, plane: no_acb(), release_allowance_ms: 10_000,
            }, &mt);
            Rig { mt, transport, attempt, status: Status::Pending, session: Some(session), now: 0 }
        }

        /// One frame, `ms` after the last.
        fn frame(&mut self, ms: u64) {
            self.now += ms;
            let replies = self.session.as_ref().unwrap().poll();
            let session = self.session.as_mut().expect("a live session");
            let status = &mut self.status;
            let attempt = self.attempt;
            session.advance(&mut |id, answer| {
                assert_eq!(id, attempt);
                if *status != Status::Pending { return false; }
                *status = match answer { Settle::Started => Status::Started, Settle::StartFailed => Status::Failed };
                true
            }, &self.mt, replies, self.now);
        }

        fn is_done(&self) -> bool {
            self.session.as_ref().is_some_and(UriSession::is_done)
        }

        fn status(&self) -> Status {
            self.status
        }

        /// Register ok, the `load` reply naming `_m1`.
        fn load_answered(&mut self) {
            self.frame(10);
            self.transport.say("register", "");
            self.frame(10);
            self.transport.say("load", r#"{"returnValue":true,"mediaId":"_m1"}"#);
            self.frame(10);
        }

        /// Back: what `engine::teardown` does with a live session.
        fn back(&mut self) {
            let session = self.session.as_mut().expect("a live session");
            let status = &mut self.status;
            let attempt = self.attempt;
            session.stop_at(&mut |id, answer| {
                assert_eq!(id, attempt);
                if *status != Status::Pending { return false; }
                *status = match answer { Settle::Started => Status::Started, Settle::StartFailed => Status::Failed };
                true
            }, &self.mt, self.now);
        }
    }

    /// Run `f` against a private event log and return what it logged.
    fn logged(f: impl FnOnce()) -> String {
        plx_base::eventlog::with_private_log(|| {
            f();
            std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default()
        })
    }

    fn line_with<'a>(log: &'a str, needle: &str) -> &'a str {
        log.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("no log line contains {needle:?}:\n{log}"))
    }

    #[test]
    fn uriprobe_driver_runs_a_whole_session_in_order_and_settles_the_attempt_started_once() {
        let _serial = plx_base::testlock::serial();
        let log = logged(|| {
            let mut rig = Rig::new(Variant::AtLoad);
            assert_eq!(rig.transport.opened(), [Shape::Named], "the first registration is the named client");
            assert_eq!(rig.status(), Status::Pending, "the attempt waits on the service");
            rig.load_answered();
            assert_eq!(rig.transport.sent(), ["load", "subscribe"]);
            assert_eq!(rig.status(), Status::Pending);
            rig.transport.say("subscribe", r#"{"mediaId":"_m1","loadCompleted":true}"#);
            rig.frame(10);
            assert_eq!(rig.status(), Status::Started, "loadCompleted settles it");
            assert_eq!(rig.transport.sent(), ["load", "subscribe", "play"]);
            assert!(!rig.session.as_ref().unwrap().plane_open(), "not before `playing`");
            rig.transport.say("subscribe", r#"{"mediaId":"_m1","playing":true}"#);
            rig.frame(10);
            assert!(rig.session.as_ref().unwrap().plane_open(), "the plane opens on `playing`");
            rig.back();
            assert_eq!(rig.transport.sent(), ["load", "subscribe", "play", "unload"]);
            assert!(rig.transport.payload_of("unload").contains("_m1"));
            assert!(rig.transport.closed() >= 1, "the registration is closed");
            assert_eq!(rig.status(), Status::Started, "a stop does not unsettle a started attempt");
        });
        let summary = line_with(&log, " summary ");
        for want in ["load=ok", "playing=yes", "window=none", "end=back", "variant=at-load"] {
            assert!(summary.contains(want), "{want:?} missing from {summary}");
        }
        assert!(log.contains("uriprobe: t=") && line_with(&log, " armed ").contains("window=none"), "{log}");
        assert!(!log.contains("no longer waits"), "the attempt was settled exactly once:\n{log}");
        assert!(log.contains("tester: report picture yes/no and sound yes/no"));
    }

    #[test]
    fn uriprobe_driver_sends_the_armed_rates_in_the_load_payload_and_logs_them() {
        let _serial = plx_base::testlock::serial();
        let bps = AbrBps { start: None, maximum: Some(720_000), minimum: None };
        let log = logged(|| {
            let mut rig = Rig::build_with(Variant::AtLoad, bps, None);
            rig.frame(10);
            rig.transport.say("register", "");
            rig.frame(10);
            let payload: serde_json::Value = serde_json::from_str(&rig.transport.payload_of("load")).expect("JSON");
            assert_eq!(payload["payload"]["option"]["adaptiveStreaming"], serde_json::json!({ "bps": { "maximum": 720_000 } }));
            rig.back();
        });
        assert!(line_with(&log, " armed ").ends_with(" bpsmax=720000"), "{log}");
    }

    #[test]
    fn uriprobe_driver_builds_the_load_payload_from_the_held_url_without_a_window_on_a_set_that_exports_none() {
        let _serial = plx_base::testlock::serial();
        let _ = logged(|| {
            let mut rig = Rig::new(Variant::AtLoad);
            rig.frame(10);
            rig.transport.say("register", "");
            rig.frame(10);
            let payload: serde_json::Value = serde_json::from_str(&rig.transport.payload_of("load")).expect("JSON");
            assert_eq!(payload["uri"], URL);
            assert_eq!(payload["type"], "media");
            assert_eq!(payload["payload"]["option"]["appId"], "com.example.plxnative");
            assert!(payload["payload"]["option"].get("windowId").is_none(), "no exported window on this sink: {payload}");
            rig.back();
        });
    }

    #[test]
    fn uriprobe_driver_walks_the_register_ladder_close_then_open_the_anonymous_shape() {
        let _serial = plx_base::testlock::serial();
        let log = logged(|| {
            let mut rig = Rig::new(Variant::AtLoad);
            rig.transport.deny("register", "denied: com.webos.media");
            rig.frame(10);
            assert_eq!(rig.transport.opened(), [Shape::Named], "no reuse before close ACK");
            rig.frame(0);
            assert_eq!(rig.transport.opened(), [Shape::Named, Shape::Anon], "named, then anonymous");
            assert_eq!(rig.transport.closed(), 1, "the refused registration is closed before the next opens");
            assert!(rig.transport.sent().is_empty(), "nothing is sent before a registration succeeds");
            rig.transport.say("register", "");
            rig.frame(10);
            assert_eq!(rig.transport.sent(), ["load"], "the anonymous registration sends the load again");
            rig.back();
        });
        assert!(log.contains("register shape=named refused"), "{log}");
        assert!(log.contains("register shape=anon ok"), "{log}");
    }

    #[test]
    fn uriprobe_driver_leaves_the_attempt_start_failed_when_both_registrations_are_refused() {
        let _serial = plx_base::testlock::serial();
        let log = logged(|| {
            let mut rig = Rig::new(Variant::AtLoad);
            rig.transport.deny("register", "named refused");
            rig.frame(10);
            rig.frame(0);
            rig.transport.deny("register", "anon refused");
            rig.frame(10);
            assert!(rig.is_done());
            assert_eq!(rig.status(), Status::Failed, "a failed register fails the start");
            assert!(rig.transport.sent().is_empty());
            // The player is not stuck: the Back that follows finds a finished session and ends cleanly.
            rig.back();
            assert_eq!(rig.status(), Status::Failed);
        });
        let summary = line_with(&log, " summary ");
        assert!(summary.contains("load=fail(register)") && summary.contains("end=refused:register"), "{summary}");
        assert!(!log.contains("no longer waits"), "settled once, not twice:\n{log}");
    }

    #[test]
    fn uriprobe_driver_settles_a_back_during_loading_once_and_sends_no_unload_without_a_media_id() {
        let _serial = plx_base::testlock::serial();
        let log = logged(|| {
            let mut rig = Rig::new(Variant::AtLoad);
            rig.frame(10);
            rig.transport.say("register", "");
            rig.frame(10);
            assert_eq!(rig.transport.sent(), ["load"]);
            rig.back();
            assert_eq!(rig.status(), Status::Failed, "the attempt the Back interrupted fails");
            assert_eq!(rig.transport.sent(), ["load"], "no mediaId, so there is nothing to unload");
            assert!(rig.transport.closed() >= 1, "closing the registration is what makes the service drop the load");
        });
        assert!(line_with(&log, " summary ").contains("end=back"), "{log}");
        assert!(!log.contains("no longer waits"), "{log}");
    }

    #[test]
    fn uriprobe_driver_unloads_a_back_that_comes_after_the_load_reply_but_before_loadcompleted() {
        let _serial = plx_base::testlock::serial();
        let _ = logged(|| {
            let mut rig = Rig::new(Variant::AtLoad);
            rig.load_answered();
            rig.back();
            assert_eq!(rig.transport.sent(), ["load", "subscribe", "unload"]);
            assert_eq!(rig.status(), Status::Failed);
        });
    }

    #[test]
    fn uriprobe_driver_times_out_a_load_the_service_never_answers() {
        let _serial = plx_base::testlock::serial();
        let log = logged(|| {
            let mut rig = Rig::new(Variant::AtLoad);
            rig.frame(10);
            rig.transport.say("register", "");
            rig.frame(10);
            rig.frame(30_000);
            assert!(rig.is_done());
            assert_eq!(rig.status(), Status::Failed);
            rig.back();
        });
        assert!(line_with(&log, " summary ").contains("end=timeout:load"), "{log}");
    }

    #[test]
    fn uriprobe_driver_fails_the_start_on_the_first_frame_when_the_transport_cannot_open() {
        let _serial = plx_base::testlock::serial();
        let log = logged(|| {
            let mut rig = Rig::build(Variant::AtLoad, Some("no bus"));
            // `open` was refused on the spot, but the start has already returned its attempt: the
            // refusal is fed on the first frame, never inside the call that started the session.
            assert_eq!(rig.status(), Status::Pending);
            assert!(!rig.is_done());
            rig.frame(10);
            assert!(rig.is_done());
            assert_eq!(rig.transport.opened(), [Shape::Named, Shape::Anon], "the ladder still tries the second shape");
            assert_eq!(rig.status(), Status::Failed);
            rig.back();
        });
        assert!(log.contains("register shape=named refused"), "{log}");
        assert!(line_with(&log, " summary ").contains("end=refused:register"), "{log}");
    }

    #[test]
    fn uriprobe_driver_never_writes_the_url_to_the_log_even_when_the_transport_echoes_it() {
        let _serial = plx_base::testlock::serial();
        let log = logged(|| {
            let mut rig = Rig::new(Variant::AtLoad);
            *rig.transport.refuse_call.lock().unwrap() = Some(("load", format!("could not send {URL}")));
            rig.frame(10);
            rig.transport.say("register", "");
            rig.frame(10);
            rig.transport.say("register", "");
            rig.frame(10);
            rig.back();
        });
        assert!(log.contains("call load err("), "the refusal is reported:\n{log}");
        for leak in LEAKS {
            assert!(!log.contains(leak), "{leak:?} reached the log:\n{log}");
        }
    }

    #[test]
    fn uriprobe_driver_window_destroy_runs_only_after_ack_and_once() {
        let _serial = plx_base::testlock::serial();
        let _ = logged(|| {
            let mut rig = Rig::new(Variant::AtLoad);
            rig.load_answered();
            rig.transport.hold_close.store(true,Ordering::SeqCst);
            rig.session.as_mut().unwrap().window=true;
            rig.back(); rig.back();
            assert!(rig.session.as_ref().unwrap().window);
            assert!(!rig.session.as_ref().unwrap().lifecycle.released());
            assert_eq!(rig.transport.closed(),1);
            *rig.transport.close_status.lock().unwrap()=Some(CloseStatus::Released);
            rig.frame(0);
            let destroys=std::cell::Cell::new(0);
            rig.session.take().unwrap().release_with(||destroys.set(destroys.get()+1));
            assert_eq!(destroys.get(),1);
        });
    }

    #[test]
    fn uriprobe_driver_delayed_initial_play_reply_cannot_grade_resume() {
        let _serial = plx_base::testlock::serial();
        let output=logged(|| {
            let mut rig=Rig::new(Variant::Drive); rig.load_answered();
            rig.transport.say("subscribe",r#"{"mediaId":"_m1","loadCompleted":true}"#); rig.frame(10);
            let initial=*rig.transport.requests.lock().unwrap().iter().find(|(method,_)|*method=="play").unwrap();
            rig.transport.say("subscribe",r#"{"mediaId":"_m1","playing":true}"#); rig.frame(10);
            rig.frame(3_000); rig.frame(2_500);
            let session=*rig.transport.current.lock().unwrap();
            rig.transport.replies.lock().unwrap().push_back(UriReply{session:session.unwrap(),request:initial.1,method:"play",payload:r#"{"returnValue":false}"#.into(),ok:true});
            rig.frame(10);
            rig.back();
        });
        assert!(line_with(&output," summary ").contains("resume=sent"),"delayed initial ACK changed resume: {output}");
    }

}
