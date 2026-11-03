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
//! ACB experiments add a separate fence: after bus release they dispatch UNLOADED once and retain
//! plane quarantine until process exit, because asynchronous ACB completion is not proven.
//!
//! **What stays out of it.** No Engine thread runs, so nothing is reported to Plex. The URL is held by [`UriSession`] for
//! the `load` payload and nowhere else: no `Debug`, no log line, and the payload is built by
//! [`build_load`] straight into the transport. Every line the machine writes is complete and printed
//! verbatim; the few lines this driver adds (the window) carry no URL either.
//!
//! The caller decides whether to enable the probe.

use super::uri_lifecycle::{Event as LifeEvent, Fx as LifeFx, Lifecycle};
use super::uriprobe::{build_load, Config, Kind, Scheme, Settle, UriCall, UriEffect, UriEvent, UriProbe, Variant, AcbOp};
use plx_base::task::MainThread;
use plx_platform::tv::sink::{DisplaySink, VP_ACB, VP_EXPORTED};
use plx_platform::tv::uri::{CloseStatus, UriMedia, UriPoll};
use std::collections::VecDeque;
use std::os::raw::c_int;
use std::time::Instant;

/// The exported window is placed over the whole canvas; the coded size the service reports for the
/// picture (`videoInfo`) only changes the SOURCE rectangle (`engine::place_exported` does the same).
const CANVAS: (c_int, c_int) = (1_920, 1_080);

// The actuator reads platform facts through this seam; the probe machine receives only its answer.
#[derive(Clone,Copy)]
enum AcbGateFact { Mode, BootHandle, PlayerType, DassAvailable, Initialized }
struct AcbGateCheck { mode:i32, boot_handle:bool, ptype:i32, dass_available:bool, initialized:bool, ready:bool }
fn check_acb_gate(dass:bool, read:impl Fn(AcbGateFact)->i32)->AcbGateCheck {
    let mode=read(AcbGateFact::Mode); let boot_handle=read(AcbGateFact::BootHandle)!=0;
    let ptype=read(AcbGateFact::PlayerType); let dass_available=read(AcbGateFact::DassAvailable)!=0;
    let initialized=read(AcbGateFact::Initialized)==1;
    let ready=mode==VP_ACB && boot_handle && initialized && ptype==0 && (!dass || dass_available);
    AcbGateCheck {mode,boot_handle,ptype,dass_available,initialized,ready}
}

/// Facts owned by the media adapter, read only when an ACB gate is exercised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcbFacts { pub boot_handle: bool, pub player_type: i32 }

/// Inputs from the caller. The URL is kept private and erased on logical completion.
pub struct DriverConfig<A: Copy> {
    pub attempt: A,
    pub variant: Variant,
    pub kind: Kind,
    pub url: String,
    pub app_id: String,
    pub transport: &'static dyn UriMedia,
    pub sink: &'static dyn DisplaySink,
    pub log: fn(&str),
    pub acb_facts: fn() -> AcbFacts,
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
    acb_facts: fn() -> AcbFacts,
    release_allowance_ms: u64,
    /// An exported window was created and is owed a `destroy_window`.
    window: bool,
    /// Events produced by effects that failed on the spot (the transport refused to send). They are
    /// fed on the next [`advance`](Self::advance), never inside the call that started the session,
    /// so a refusal cannot settle the attempt before its start has even returned.
    inbox: VecDeque<UriEvent>,
    clock: Instant,
}

impl<A: Copy> UriSession<A> {
    /// Create the exported window for the variants which use one, build the machine and take its
    /// first step (`Start`: the `armed` line and the first registration).
    pub fn open(input: DriverConfig<A>, mt: &MainThread) -> UriSession<A> {
        let DriverConfig { attempt, variant, kind, url, app_id, transport, sink, log, acb_facts, release_allowance_ms } = input;
        let clock = Instant::now();
        let mut window_id = None;
        let mut created = false;
        if !variant.acb() && variant != Variant::NoWindow && sink.window_mode() == VP_EXPORTED {
            let id = unsafe { sink.create_window(mt) };
            if id.is_null() {
                log("uriprobe: t=0 window id=none (the television refused to create one)");
            } else {
                created = true;
                window_id = Some(unsafe { std::ffi::CStr::from_ptr(id) }.to_string_lossy().into_owned());
            }
        }
        let config = Config {
            variant,
            scheme: Scheme::of(&url),
            kind,
            window_id: window_id.clone(),
            app_id: app_id.clone(),
            transport: variant.drives(),
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
            acb_facts,
            release_allowance_ms,
            window: created,
            inbox: VecDeque::new(),
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
        let rv = unsafe { self.sink.place_window(mt, width, height, 0, 0, CANVAS.0, CANVAS.1) };
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
                    LifeFx::Open { request, shape } => {
                        let result = self.transport.open(shape).map_err(|text| super::uri_protocol::Message::transport_failure(&text));
                        events.push_back(LifeEvent::OpenAnswered { request, result });
                    }
                    LifeFx::Call { session, request, call } => {
                        self.inbox.push_back(UriEvent::Dispatched { method: super::uri_protocol::Method::of(call.method()), kind: match &call { UriCall::SelectTrack {kind,..} => Some(*kind), _ => None }, request });
                        let payload = match &call {
                            UriCall::Load { window_id } => build_load(&self.url, &self.app_id, window_id.as_deref()),
                            other => other.payload().unwrap_or_default(),
                        };
                        let result = self.transport.call(session, request, call.method(), &payload);
                        let message = result.as_ref().err().map(|text| super::uri_protocol::Message::transport_failure(text)).unwrap_or_default();
                        events.push_back(LifeEvent::CallQueued { session, request, ok: result.is_ok(), message });
                    }
                    LifeFx::Close { session } => {
                        let ok = self.transport.close(session).is_ok();
                        events.push_back(LifeEvent::CloseQueued { session, ok });
                    }
                    LifeFx::Deliver { method, ok, mut message } => {
                        if let Some(source) = message.acb_source.take() { self.acb_source = Some(source); }
                        self.inbox.push_back(UriEvent::Reply { method, ok, message });
                    }
                    LifeFx::UnloadAcb {session} => {
                        // The actual actuator is main-thread only; enqueue its addressed tail.
                        self.inbox.push_back(UriEvent::AcbReleaseTail {session});
                    }
                    LifeFx::AcbQuarantined => (self.log)("uriprobe: acb=quarantined bus=released plane-release=unproven need-relaunch"),
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
            unsafe { self.sink.plane_unload(mt) };
            self.lifecycle(LifeEvent::AcbUnloadDispatched {session}, now_ms);
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
        let sink = self.sink;
        match op {
            AcbOp::Check {dass} => {
                let facts = (self.acb_facts)();
                let check=check_acb_gate(*dass,|fact|match fact {
                    AcbGateFact::Mode=>sink.window_mode(),
                    AcbGateFact::BootHandle=>i32::from(facts.boot_handle),
                    AcbGateFact::PlayerType=>facts.player_type,
                    AcbGateFact::DassAvailable=>i32::from(unsafe {sink.plane_probe_dass_available(mt)}),
                    AcbGateFact::Initialized=>i32::from(unsafe {sink.plane_probe_initialized(mt)}),
                });
                (self.log)("uriprobe: ACB experiment requires known SDR H264720p24 fixture; fallback is explicit fixture data, not content detection");
                (self.log)(&format!("uriprobe: t={now_ms} acb gate mode={} boot_handle={} actual_ptype={} dass_available={} initialized={}",check.mode,check.boot_handle,check.ptype,check.dass_available,check.initialized));
                check.ready
            }
            AcbOp::MediaId {media_id} => {
                let Some(session) = self.lifecycle.session() else { return false };
                self.lifecycle(LifeEvent::AcbTouched {session}, now_ms);
                let Ok(id) = std::ffi::CString::new(&**media_id) else { return false };
                unsafe { sink.plane_probe_media_id(mt, id.as_ptr()) == 1 }
            }
            AcbOp::Loaded => unsafe { sink.plane_probe_loaded(mt) == 1 },
            AcbOp::Connect {enabled: false} => { (self.log)("uriprobe: acb control connectDass skipped (same timed slot)"); true }
            AcbOp::Connect {enabled: true} => unsafe { sink.plane_probe_connect_dass(mt) == 1 },
            AcbOp::VideoData {media_id, video} => {
                (self.log)(&format!("uriprobe: acb video facts width={} height={} frameRate={} bitRate={:?}", video.width, video.height, video.frame_rate, video.bit_rate));
                let observed = self.acb_source.as_ref().filter(|source| &source.media_id == media_id && source.video == *video);
                let payload = observed.map(|source| source.text().to_owned()).or_else(|| super::uri_protocol::build_acb_fixture_video(media_id, *video));
                let Some(payload) = payload else { (self.log)("uriprobe: acb invalid descriptor: no observed envelope or exact SDR720p24 fixture"); return false };
                (self.log)(if observed.is_some() {"uriprobe: acb descriptor=observed-envelope"} else {"uriprobe: acb descriptor=explicit-SDR720p24-fixture"});
                let Ok(payload) = std::ffi::CString::new(payload) else { return false };
                unsafe { sink.plane_send_video_data(mt, payload.as_ptr()) == 1 }
            }
            AcbOp::Display => unsafe { sink.plane_probe_finish(mt) == 1 }
        }
    }

    /// Destroy the exported window only after the ownership reducer has observed release.
    pub fn release(self, mt: &MainThread) {
        let sink = self.sink;
        self.release_with(|| unsafe { sink.destroy_window(mt) });
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
    fn no_acb() -> AcbFacts { AcbFacts { boot_handle: false, player_type: 0 } }

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
            let mt = unsafe { MainThread::assume() };
            let attempt = 17;
            let transport = Scripted::leak();
            *transport.refuse_open.lock().unwrap() = refuse_open.map(str::to_owned);
            let session = UriSession::open(DriverConfig {
                attempt, variant, kind: Kind::Direct, url: URL.to_owned(),
                app_id: "com.example.plxnative".to_owned(), transport, sink: &NO_SINK,
                log: plx_base::eventlog::log, acb_facts: no_acb, release_allowance_ms: 10_000,
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

    #[test]
    fn uri_acb_review_refused_initializer_invalidates_both_arms() {
        for dass in [false,true] {
            let check=check_acb_gate(dass,|fact|match fact {
                AcbGateFact::Mode=>plx_platform::tv::sink::VP_ACB,
                AcbGateFact::BootHandle|AcbGateFact::DassAvailable=>1,
                AcbGateFact::PlayerType|AcbGateFact::Initialized=>0,
            });
            assert!(!check.ready,"created handle and playerType0 cannot prove initialization succeeded");
        }
        for dass in [false,true] {
            let ready=check_acb_gate(dass,|fact|match fact {
                AcbGateFact::Mode=>plx_platform::tv::sink::VP_ACB,
                AcbGateFact::PlayerType=>0,
                _=>1,
            });
            assert!(ready.ready,"valid initialized boot handle must remain usable");
        }

    }

}
