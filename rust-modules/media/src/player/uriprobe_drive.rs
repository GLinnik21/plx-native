//! The URI-mode probe's driver (issue #1064, the first test): the loop which feeds
//! [`UriProbe::step`] its events, executes the effects each step asks for, and feeds every effect
//! that failed back as the next event. The machine, its events and its effects are
//! [`super::uriprobe`]; the transport it talks through is [`plx_platform::tv::uri`]; the slot that
//! owns a running session is `PlayerAdapter.uri`. The shape is `foreground.rs` driving
//! `lifecycle.rs`: this file holds the IO, the machine stays pure.
//!
//! **Where it runs.** `engine::start_bufferfeed_inner` forks to [`start`] when the developer trigger
//! `plxnative-uriprobe` is present: no Engine is built, the route-start attempt is claimed here and
//! stays `Starting` until the machine settles it, and the session sits in `PlayerAdapter.uri`.
//! [`drive`] runs once a frame (before `observe_video_plane`, so the plane term it feeds reads this
//! frame's answer). `engine::teardown` calls [`stop`] first, which ends the session on Back or an app
//! switch exactly as it ends an Engine. A session that ends on its own (an error, a timeout, the end
//! of the stream) stops logically at once. The ownership reducer keeps its adapter slot and window
//! until the worker acknowledges successful unregister; unresolved release blocks another start.
//! A deadline quarantines ownership, and a late successful acknowledgement still permits cleanup.
//! ACB experiments add a separate fence: after bus release they dispatch UNLOADED once and retain
//! plane quarantine until process exit, because asynchronous ACB completion is not proven.
//!
//! **What stays out of it.** No Engine thread runs, so nothing is reported to Plex; nothing here
//! sets `ps.finished`, so a stop cannot mark the item watched. The URL is held by [`UriSession`] for
//! the `load` payload and nowhere else: no `Debug`, no log line, and the payload is built by
//! [`build_load`] straight into the transport. Every line the machine writes is complete and printed
//! verbatim; the few lines this driver adds (the window) carry no URL either.
//!
//! The whole module is the `devtriggers` surface (`player/mod.rs`).

use super::log;
use super::uri_lifecycle::{Event as LifeEvent, Fx as LifeFx, Lifecycle};
use super::uriprobe::{build_load, Config, Kind, Scheme, Settle, UriCall, UriEffect, UriEvent, UriProbe, Variant, AcbOp};
use crate::route::{PlaybackSession, RouteStartAttempt, RouteStartResult, RouteStartTransaction};
use plx_base::task::MainThread;
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
    let ready=mode==plx_platform::tv::sink::VP_ACB && boot_handle && initialized && ptype==0 && (!dass || dass_available);
    AcbGateCheck {mode,boot_handle,ptype,dass_available,initialized,ready}
}

/// One running probe: the machine, the pipe to `com.webos.media`, and what the driver owns for it.
/// No `Debug`: [`url`](Self::url) is a media URL with a token in it.
pub struct UriSession {
    probe: UriProbe,
    lifecycle: Lifecycle,
    acb_source: Option<super::uri_protocol::AcbSource>,
    transport: &'static dyn UriMedia,
    /// The media URL, for the `load` payload (re-built if the register ladder sends it again). Taken
    /// out the moment the session ends.
    url: String,
    app_id: String,
    /// The route-start attempt this session stands in for; the machine settles it exactly once.
    attempt: RouteStartAttempt,
    /// An exported window was created and is owed a `destroy_window`.
    window: bool,
    /// Events produced by effects that failed on the spot (the transport refused to send). They are
    /// fed on the next [`advance`](Self::advance), never inside the call that started the session,
    /// so a refusal cannot settle the attempt before its start has even returned.
    inbox: VecDeque<UriEvent>,
    clock: Instant,
}

impl UriSession {
    /// Create the exported window for the variants which use one, build the machine and take its
    /// first step (`Start`: the `armed` line and the first registration).
    fn open(
        ps: &PlaybackSession,
        mt: &MainThread,
        transport: &'static dyn UriMedia,
        attempt: RouteStartAttempt,
        variant: Variant,
        url: &str,
    ) -> UriSession {
        let clock = Instant::now();
        let app_id = plx_base::paths::app_id().to_owned();
        let mut window_id = None;
        let mut created = false;
        if !variant.acb() && variant != Variant::NoWindow && super::sink().window_mode() == super::VP_EXPORTED {
            let id = unsafe { super::sink().create_window(mt) };
            if id.is_null() {
                log("uriprobe: t=0 window id=none (the television refused to create one)");
            } else {
                created = true;
                window_id = Some(unsafe { std::ffi::CStr::from_ptr(id) }.to_string_lossy().into_owned());
            }
        }
        let config = Config {
            variant,
            scheme: Scheme::of(url),
            kind: if crate::route::is_segmented_hls(ps) { Kind::Hls } else { Kind::Direct },
            window_id: window_id.clone(),
            app_id: app_id.clone(),
            transport: variant.drives(),
        };
        let mut session = UriSession {
            probe: UriProbe::new(config),
            lifecycle: Lifecycle::new(),
            acb_source: None,
            transport,
            url: url.to_owned(),
            app_id,
            attempt,
            window: created,
            inbox: VecDeque::new(),
            clock,
        };
        if let Some(id) = window_id {
            log(&format!("uriprobe: t=0 window id={id}"));
            session.place(mt, CANVAS.0, CANVAS.1);
        }
        session.feed(UriEvent::Start, &mut NoSettle, mt, 0);
        session
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.clock.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Retain observed plane ownership through stop; ACB quarantine lasts until process exit.
    pub fn plane_open(&self) -> bool {
        self.lifecycle.plane_open()
    }

    pub(super) fn route_start_attempt(&self) -> RouteStartAttempt {
        self.attempt
    }

    pub(super) fn acb_exit_quarantine(&self) -> bool { self.lifecycle.acb_exit_quarantine() }

    pub fn closing(&self) -> bool {
        matches!(self.lifecycle.phase(), super::uri_lifecycle::Phase::Closing | super::uri_lifecycle::Phase::Quarantined)
    }

    /// Re-place the exported window for a coded picture of `width` x `height`.
    fn place(&self, mt: &MainThread, width: c_int, height: c_int) {
        let rv = unsafe { super::sink().place_window(mt, width, height, 0, 0, CANVAS.0, CANVAS.1) };
        log(&format!("uriprobe: t={} window placed src={width}x{height} rv={rv}", self.now_ms()));
    }

    /// One frame: the replies which arrived, then the clock. Everything a reply or a deadline sets
    /// off, including the events of calls that failed on the spot, runs to the end before this
    /// returns.
    fn poll(&self) -> Result<UriPoll, String> {
        match self.lifecycle.poll_session() {
            Some(session) => self.transport.poll(session),
            None => Ok(UriPoll { replies: Vec::new(), close: CloseStatus::Released }),
        }
    }

    fn advance(&mut self, settle: &mut dyn Settler, mt: &MainThread, poll: Result<UriPoll, String>, now_ms: u64) {
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
                    LifeFx::AcbQuarantined => log("uriprobe: acb=quarantined bus=released plane-release=unproven need-relaunch"),
                    LifeFx::Released => { self.inbox.push_back(UriEvent::Closed); }
                    LifeFx::Fault => {
                        log(&format!("uriprobe: t={now_ms} transport ownership unresolved or reply stream lost"));
                        self.inbox.push_back(UriEvent::Back);
                    }
                }
            }
        }
    }

    fn drain(&mut self, settle: &mut dyn Settler, mt: &MainThread, now_ms: u64) {
        while let Some(event) = self.inbox.pop_front() {
            self.feed(event, settle, mt, now_ms);
        }
    }

    /// Step the machine once and execute what it asks for, in order.
    fn feed(&mut self, event: UriEvent, settle: &mut dyn Settler, mt: &MainThread, now_ms: u64) {
        if let UriEvent::AcbReleaseTail {session} = event {
            if !self.lifecycle.acb_tail_due(session) { return; }
            unsafe { super::sink().plane_unload(mt) };
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
    fn execute(&mut self, effect: UriEffect, settle: &mut dyn Settler, mt: &MainThread, now_ms: u64) -> Option<UriEvent> {
        match effect {
            UriEffect::Acb {request, op} => {
                let invoked = self.acb_execute(&op, mt, now_ms);
                // Round up so a frame-start stamp never shortens the requested post-call spacing.
                Some(UriEvent::AcbAnswered {request, invoked, completed_ms: self.now_ms().saturating_add(1)})
            }
            UriEffect::Open(shape) => { self.lifecycle(LifeEvent::Open(shape), now_ms); None }
            UriEffect::Call(call) => { self.lifecycle(LifeEvent::Call(call), now_ms); None }
            UriEffect::Close => {
                let allowance_ms = u64::try_from(super::release::RELEASE_DEADLINE.as_millis()).unwrap_or(u64::MAX);
                self.lifecycle(LifeEvent::Close { now_ms, allowance_ms }, now_ms);
                None
            }
            UriEffect::Settle(how) => {
                let result = match how {
                    Settle::Started => RouteStartResult::Started,
                    Settle::StartFailed => RouteStartResult::StartFailed,
                };
                if !settle.settle(self.attempt, result) {
                    // A Back already moved the route on (`cancel_play` runs before the stop), or a
                    // later start replaced this attempt: nothing is left to settle, and that is fine.
                    log(&format!("uriprobe: t={now_ms} the route no longer waits on this attempt"));
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
                log(&line);
                None
            }
        }
    }

    fn acb_execute(&mut self, op: &AcbOp, mt: &MainThread, now_ms: u64) -> bool {
        let sink = super::sink();
        match op {
            AcbOp::Check {dass} => {
                let check=check_acb_gate(*dass,|fact|match fact {
                    AcbGateFact::Mode=>sink.window_mode(),
                    AcbGateFact::BootHandle=>i32::from(super::ACB_OK.load(std::sync::atomic::Ordering::Relaxed)),
                    AcbGateFact::PlayerType=>super::PTYPE.load(std::sync::atomic::Ordering::Relaxed),
                    AcbGateFact::DassAvailable=>i32::from(unsafe {sink.plane_probe_dass_available(mt)}),
                    AcbGateFact::Initialized=>i32::from(unsafe {sink.plane_probe_initialized(mt)}),
                });
                log("uriprobe: ACB experiment requires known SDR H264720p24 fixture; fallback is explicit fixture data, not content detection");
                log(&format!("uriprobe: t={now_ms} acb gate mode={} boot_handle={} actual_ptype={} dass_available={} initialized={}",check.mode,check.boot_handle,check.ptype,check.dass_available,check.initialized));
                check.ready
            }
            AcbOp::MediaId {media_id} => {
                let Some(session) = self.lifecycle.session() else { return false };
                self.lifecycle(LifeEvent::AcbTouched {session}, now_ms);
                let Ok(id) = std::ffi::CString::new(&**media_id) else { return false };
                unsafe { sink.plane_probe_media_id(mt, id.as_ptr()) == 1 }
            }
            AcbOp::Loaded => unsafe { sink.plane_probe_loaded(mt) == 1 },
            AcbOp::Connect {enabled: false} => { log("uriprobe: acb control connectDass skipped (same timed slot)"); true }
            AcbOp::Connect {enabled: true} => unsafe { sink.plane_probe_connect_dass(mt) == 1 },
            AcbOp::VideoData {media_id, video} => {
                log(&format!("uriprobe: acb video facts width={} height={} frameRate={} bitRate={:?}", video.width, video.height, video.frame_rate, video.bit_rate));
                let observed = self.acb_source.as_ref().filter(|source| &source.media_id == media_id && source.video == *video);
                let payload = observed.map(|source| source.text().to_owned()).or_else(|| super::uri_protocol::build_acb_fixture_video(media_id, *video));
                let Some(payload) = payload else { log("uriprobe: acb invalid descriptor: no observed envelope or exact SDR720p24 fixture"); return false };
                log(if observed.is_some() {"uriprobe: acb descriptor=observed-envelope"} else {"uriprobe: acb descriptor=explicit-SDR720p24-fixture"});
                let Ok(payload) = std::ffi::CString::new(payload) else { return false };
                unsafe { sink.plane_send_video_data(mt, payload.as_ptr()) == 1 }
            }
            AcbOp::Display => unsafe { sink.plane_probe_finish(mt) == 1 }
        }
    }

    /// Destroy the exported window only after the ownership reducer has observed release.
    fn release(self, mt: &MainThread) {
        self.release_with(|| unsafe { super::sink().destroy_window(mt) });
    }

    fn release_with(mut self, destroy: impl FnOnce()) {
        self.url = String::new();
        assert!(self.lifecycle.released(), "window ownership must be released first");
        if self.window { destroy(); }
    }
}

/// Where the machine's `Settle` effect lands. The one production implementation is the route
/// reducer ([`RouteSettler`]); it is a trait only so [`UriSession::open`] can take its first step
/// before a [`PlaybackSession`] exists to settle against (`Start` never settles).
trait Settler {
    fn settle(&mut self, attempt: RouteStartAttempt, result: RouteStartResult) -> bool;
}

/// The route reducer, reached through the session the player owns.
struct RouteSettler<'a>(&'a mut PlaybackSession);

impl Settler for RouteSettler<'_> {
    fn settle(&mut self, attempt: RouteStartAttempt, result: RouteStartResult) -> bool {
        crate::route::settle_route_start(self.0, attempt, result)
    }
}

/// The settler for the one step that cannot settle anything (`Start`).
struct NoSettle;

impl Settler for NoSettle {
    fn settle(&mut self, _attempt: RouteStartAttempt, _result: RouteStartResult) -> bool {
        debug_assert!(false, "the first step of a session settles nothing");
        false
    }
}

/// The fork `engine::start_bufferfeed_inner` takes when the trigger is armed: claim the attempt the
/// Engine would have claimed, open the session and install it. `Ok` is the attempt (the caller raises
/// `LoadStarted` for it as for any start); the attempt stays `Starting` until the machine settles it.
/// Nothing else of the Engine start runs, so no thread starts and nothing reaches Plex.
pub(super) fn start(
    ps: &mut PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    route_start: RouteStartTransaction,
    url: &str,
    variant: Variant,
) -> Result<RouteStartAttempt, RouteStartResult> {
    if pa.uri_live() { return Err(RouteStartResult::StartFailed); }
    let Some(attempt) = crate::route::claim_route_start_attempt(route_start) else {
        log("uriprobe: the prepared route lost its start before the probe began");
        return Err(RouteStartResult::StartFailed);
    };
    let session = UriSession::open(ps, pa.mt(), plx_platform::tv::uri::installed(), attempt, variant, url);
    pa.install_uri(session);
    Ok(attempt)
}

/// Once a frame, before `observe_video_plane`: drain the transport, step the machine, execute its
/// effects, settle the attempt, place the window on `videoInfo`. Logical Done frees the slot only
/// after the registration's physical release; this runs even with no player screen.
pub fn drive(ps: &mut PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    let (session, mt) = pa.uri_split();
    let Some(session) = session else { return };
    let replies = session.poll();
    let now_ms = session.now_ms();
    session.advance(&mut RouteSettler(ps), mt, replies, now_ms);
    let finished = session.probe.is_done() && session.lifecycle.released();
    if finished {
        if let Some(done) = pa.take_uri() {
            done.release(pa.mt());
        }
    }
}

/// End a session that is still running (`engine::teardown`: Back, or an app switch): the machine's
/// own stop path settles a still-`Starting` attempt as failed, unloads once the service has named a
/// media, requests registration close and writes the summary. The adapter keeps the window and
/// session until a later frame observes release.
pub(super) fn stop(ps: &mut PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    let (session, mt) = pa.uri_split();
    let Some(session) = session else { return };
    session.inbox.clear();
    let now_ms = session.now_ms();
    let mut settle = RouteSettler(ps);
    session.feed(UriEvent::Back, &mut settle, mt, now_ms);
    session.drain(&mut settle, mt, now_ms);
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

    /// A session over a scripted transport, with the route reducer in a claimed start.
    struct Rig {
        ps: PlaybackSession,
        pa: super::super::adapter::PlayerAdapter,
        transport: &'static Scripted,
        attempt: RouteStartAttempt,
        session: Option<UriSession>,
        now: u64,
    }

    impl Rig {
        fn new(variant: Variant) -> Rig {
            Rig::build(variant, None)
        }

        /// `refuse_open`: the text the transport answers `open` with, from the very first call.
        fn build(variant: Variant, refuse_open: Option<&str>) -> Rig {
            let ps = PlaybackSession::IDLE;
            crate::route::reset_player_control_for_test(&ps);
            let pa = super::super::adapter::PlayerAdapter::new(unsafe { MainThread::assume() });
            let ticket = crate::route::begin_route_start().expect("a start owner");
            assert!(crate::route::prepare_route_start(ticket));
            let attempt = crate::route::claim_route_start_attempt(ticket).expect("an attempt");
            let transport = Scripted::leak();
            *transport.refuse_open.lock().unwrap() = refuse_open.map(str::to_owned);
            let session = UriSession::open(&ps, pa.mt(), transport, attempt, variant, URL);
            Rig { ps, pa, transport, attempt, session: Some(session), now: 0 }
        }

        /// One frame, `ms` after the last.
        fn frame(&mut self, ms: u64) {
            self.now += ms;
            let replies = self.session.as_ref().unwrap().poll();
            let mut settle = RouteSettler(&mut self.ps);
            let session = self.session.as_mut().expect("a live session");
            session.advance(&mut settle, self.pa.mt(), replies, self.now);
        }

        fn is_done(&self) -> bool {
            self.session.as_ref().is_some_and(|s| s.probe.is_done())
        }

        fn status(&self) -> crate::route::RouteStartStatus {
            crate::route::route_start_status(self.attempt)
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
            let mut session = self.session.take().expect("a live session");
            let mut settle = RouteSettler(&mut self.ps);
            session.feed(UriEvent::Back, &mut settle, self.pa.mt(), self.now);
            session.drain(&mut settle, self.pa.mt(), self.now);
            self.session = Some(session);
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
            assert_eq!(rig.status(), crate::route::RouteStartStatus::Pending, "the attempt waits on the service");
            rig.load_answered();
            assert_eq!(rig.transport.sent(), ["load", "subscribe"]);
            assert_eq!(rig.status(), crate::route::RouteStartStatus::Pending);
            rig.transport.say("subscribe", r#"{"mediaId":"_m1","loadCompleted":true}"#);
            rig.frame(10);
            assert_eq!(rig.status(), crate::route::RouteStartStatus::Started, "loadCompleted settles it");
            assert_eq!(rig.transport.sent(), ["load", "subscribe", "play"]);
            assert!(!rig.session.as_ref().unwrap().plane_open(), "not before `playing`");
            rig.transport.say("subscribe", r#"{"mediaId":"_m1","playing":true}"#);
            rig.frame(10);
            assert!(rig.session.as_ref().unwrap().plane_open(), "the plane opens on `playing`");
            rig.back();
            assert_eq!(rig.transport.sent(), ["load", "subscribe", "play", "unload"]);
            assert!(rig.transport.payload_of("unload").contains("_m1"));
            assert!(rig.transport.closed() >= 1, "the registration is closed");
            assert_eq!(rig.status(), crate::route::RouteStartStatus::Started, "a stop does not unsettle a started attempt");
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
            assert_eq!(payload["payload"]["option"]["appId"], plx_base::paths::app_id());
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
            assert_eq!(rig.status(), crate::route::RouteStartStatus::Failed, "a failed register fails the start");
            assert!(rig.transport.sent().is_empty());
            // The player is not stuck: the Back that follows finds a finished session and ends cleanly.
            rig.back();
            assert_eq!(rig.status(), crate::route::RouteStartStatus::Failed);
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
            assert_eq!(rig.status(), crate::route::RouteStartStatus::Failed, "the attempt the Back interrupted fails");
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
            assert_eq!(rig.status(), crate::route::RouteStartStatus::Failed);
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
            assert_eq!(rig.status(), crate::route::RouteStartStatus::Failed);
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
            assert_eq!(rig.status(), crate::route::RouteStartStatus::Pending);
            assert!(!rig.is_done());
            rig.frame(10);
            assert!(rig.is_done());
            assert_eq!(rig.transport.opened(), [Shape::Named, Shape::Anon], "the ladder still tries the second shape");
            assert_eq!(rig.status(), crate::route::RouteStartStatus::Failed);
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
    fn uriprobe_drive_releases_a_session_that_ended_by_itself_and_frees_the_slot() {
        let _serial = plx_base::testlock::serial();
        let log = logged(|| {
            let mut rig = Rig::new(Variant::AtLoad);
            let session = rig.session.take().unwrap();
            let transport = rig.transport;
            rig.pa.install_uri(session);
            assert!(rig.pa.uri_live());
            transport.deny("register", "named refused");
            drive(&mut rig.ps, &mut rig.pa);
            drive(&mut rig.ps, &mut rig.pa);
            transport.deny("register", "anon refused");
            drive(&mut rig.ps, &mut rig.pa);
            drive(&mut rig.ps, &mut rig.pa);
            assert!(!rig.pa.uri_live(), "a finished session is released, so the next Play is not refused");
            assert!(!rig.pa.uri_plane_open());
            assert_eq!(rig.status(), crate::route::RouteStartStatus::Failed);
            drive(&mut rig.ps, &mut rig.pa);
        });
        assert!(line_with(&log, " summary ").contains("load=fail(register)"), "{log}");
    }

    #[test]
    fn uriprobe_drive_reports_the_plane_open_only_while_the_service_says_playing() {
        let _serial = plx_base::testlock::serial();
        let _ = logged(|| {
            let mut rig = Rig::new(Variant::AtLoad);
            let transport = rig.transport;
            let session = rig.session.take().unwrap();
            rig.pa.install_uri(session);
            assert!(!rig.pa.uri_plane_open());
            for (method, payload) in [
                ("register", ""),
                ("load", r#"{"returnValue":true,"mediaId":"_m1"}"#),
                ("subscribe", r#"{"mediaId":"_m1","loadCompleted":true}"#),
                ("subscribe", r#"{"mediaId":"_m1","playing":true}"#),
            ] {
                transport.say(method, payload);
                drive(&mut rig.ps, &mut rig.pa);
            }
            assert!(rig.pa.uri_plane_open());
            stop(&mut rig.ps, &mut rig.pa);
            drive(&mut rig.ps, &mut rig.pa);
            assert!(!rig.pa.uri_plane_open());
        });
    }
    #[test]
    fn uriprobe_offscreen_stop_keeps_slot_through_deadline_and_releases_after_late_ack() {
        let _serial = plx_base::testlock::serial();
        let _ = logged(|| {
            let mut rig = Rig::new(Variant::AtLoad);
            rig.load_answered();
            rig.transport.hold_close.store(true, Ordering::SeqCst);
            let session = rig.session.take().unwrap(); rig.pa.install_uri(session);
            stop(&mut rig.ps, &mut rig.pa); stop(&mut rig.ps, &mut rig.pa);
            assert_eq!(rig.transport.closed(),1);
            assert!(rig.pa.uri_live()); assert!(rig.pa.uri_closing());
            assert_eq!(rig.status(), crate::route::RouteStartStatus::Failed);
            let (session,mt)=rig.pa.uri_split();
            let session=session.unwrap();
            let poll=session.poll();
            session.advance(&mut RouteSettler(&mut rig.ps),mt,poll,20_000);
            assert!(session.closing()); assert!(!session.lifecycle.released());
            assert!(rig.pa.uri_live());
            // A frame loop with an idle route still polls; no player screen is required.
            let mut idle=PlaybackSession::IDLE;
            *rig.transport.close_status.lock().unwrap()=Some(CloseStatus::Released);
            drive(&mut idle,&mut rig.pa);
            assert!(!rig.pa.uri_live()); assert_eq!(rig.transport.closed(),1);
        });
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

    /// The tests which need the host sink to export a window (the webOS 5+ path) or the whole
    /// engine around the driver: `make test-hostsim C=plx_media T=uriprobe`.
    #[cfg(feature = "hostsim")]
    mod hostsim {
        use super::*;
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        /// The host sink exports a window (`VP_EXPORTED`, id `clocksink-window`) while this lives
        /// and is put back as it was found.
        struct ExportedHost;

        impl ExportedHost {
            fn arm() -> ExportedHost {
                crate::player::ffi_host::reset_native_lifecycle_for_test();
                crate::player::ffi_host::force_clocksink_for_test(true);
                ExportedHost
            }
        }

        impl Drop for ExportedHost {
            fn drop(&mut self) {
                crate::player::ffi_host::force_clocksink_for_test(false);
            }
        }

        #[test]
        fn uriprobe_driver_on_an_exporting_set_names_the_window_in_the_load_and_places_it_on_videoinfo() {
            let _serial = plx_base::testlock::serial();
            let _host = ExportedHost::arm();
            let log = logged(|| {
                let mut rig = Rig::new(Variant::AtLoad);
                assert!(rig.session.as_ref().unwrap().window, "the window was created");
                rig.frame(10);
                rig.transport.say("register", "");
                rig.frame(10);
                let load: serde_json::Value = serde_json::from_str(&rig.transport.payload_of("load")).expect("JSON");
                assert_eq!(load["payload"]["option"]["windowId"], "clocksink-window", "{load}");
                rig.transport.say("load", r#"{"returnValue":true,"mediaId":"_m1"}"#);
                rig.frame(10);
                rig.transport.say("subscribe", r#"{"mediaId":"_m1","videoInfo":{"width":3840,"height":2160,"codec":"hevc"}}"#);
                rig.frame(10);
                rig.back();
            });
            assert!(line_with(&log, " armed ").contains("window=exported"), "{log}");
            assert!(log.contains("window id=clocksink-window"), "{log}");
            assert!(log.contains("window placed src=1920x1080 rv=1"), "placed over the canvas as soon as it exists:\n{log}");
            assert!(log.contains("window placed src=3840x2160 rv=1"), "re-placed for the coded size videoInfo reported:\n{log}");
            assert!(line_with(&log, " summary ").contains("window=exported"), "{log}");
        }

        #[test]
        fn uriprobe_driver_late_variant_sends_the_window_after_loadcompleted_and_plays_after_its_reply() {
            let _serial = plx_base::testlock::serial();
            let _host = ExportedHost::arm();
            let log = logged(|| {
                let mut rig = Rig::new(Variant::Late);
                rig.load_answered();
                let load: serde_json::Value = serde_json::from_str(&rig.transport.payload_of("load")).expect("JSON");
                assert!(load["payload"]["option"].get("windowId").is_none(), "the late variant loads without it: {load}");
                rig.transport.say("subscribe", r#"{"mediaId":"_m1","loadCompleted":true}"#);
                rig.frame(10);
                assert_eq!(rig.transport.sent(), ["load", "subscribe", "setWindowId"], "play waits for the window call");
                assert!(rig.transport.payload_of("setWindowId").contains("clocksink-window"));
                assert_eq!(rig.status(), crate::route::RouteStartStatus::Started, "loadCompleted settles the attempt");
                rig.transport.say("setWindowId", r#"{"returnValue":true}"#);
                rig.frame(10);
                assert_eq!(rig.transport.sent(), ["load", "subscribe", "setWindowId", "play"]);
                rig.back();
            });
            assert!(line_with(&log, " summary ").contains("setWindowId=ok variant=late"), "{log}");
        }

        #[test]
        fn uriprobe_driver_no_window_variant_creates_none_even_on_an_exporting_set() {
            let _serial = plx_base::testlock::serial();
            let _host = ExportedHost::arm();
            let log = logged(|| {
                let mut rig = Rig::new(Variant::NoWindow);
                assert!(!rig.session.as_ref().unwrap().window, "no window was created");
                rig.frame(10);
                rig.transport.say("register", "");
                rig.frame(10);
                let load: serde_json::Value = serde_json::from_str(&rig.transport.payload_of("load")).expect("JSON");
                assert!(load["payload"]["option"].get("windowId").is_none(), "{load}");
                rig.back();
            });
            assert!(line_with(&log, " armed ").contains("window=none"), "{log}");
            assert!(!log.contains("window id="), "{log}");
            assert!(!log.contains("window placed"), "{log}");
        }

        #[test]
        fn uriprobe_closing_registration_refuses_engine_start_and_exit_keeps_display_safe() {
            let _serial=plx_base::testlock::serial();
            let _host=ExportedHost::arm();
            let _=logged(|| {
                let mut rig=Rig::new(Variant::AtLoad);
                rig.load_answered();
                rig.transport.hold_close.store(true,Ordering::SeqCst);
                let session=rig.session.take().unwrap(); rig.pa.install_uri(session);
                stop(&mut rig.ps,&mut rig.pa);
                assert!(rig.pa.uri_live()); assert!(rig.pa.uri_closing());
                assert_eq!(crate::player::start_bufferfeed_tracked(&mut rig.ps,&mut rig.pa),crate::player::BufferfeedStartOutcome::Failed);
                let start=Instant::now(); let mut ticks=0;
                let exit=crate::player::engine::exit_native_with(&mut rig.pa,|| { ticks+=1; start+std::time::Duration::from_secs(ticks*5) },||{});
                assert_eq!(exit,crate::player::engine::NativeExit::WorkerInFirmware);
                assert_eq!(rig.transport.closed(),1);
                assert!(!rig.pa.uri_live(),"unresolved resource owner is deliberately leaked for process exit");
            });
        }

        #[test]
        fn uri_review_regression_prepared_start_is_settled_when_uri_is_closing() {
            let _serial = plx_base::testlock::serial();
            let _host = ExportedHost::arm();
            let _ = logged(|| {
                let mut rig = Rig::new(Variant::AtLoad);
                rig.load_answered();
                rig.transport.hold_close.store(true, Ordering::SeqCst);
                let session = rig.session.take().unwrap();
                rig.pa.install_uri(session);
                stop(&mut rig.ps, &mut rig.pa);
                let transaction = crate::route::begin_route_start().expect("new route owner");
                assert!(crate::route::prepare_route_start(transaction));
                assert_eq!(crate::route::pending_route_start(), Some(transaction));
                let calls = rig.transport.sent();
                let old_status = rig.status();
                assert_eq!(crate::player::start_bufferfeed_tracked(&mut rig.ps, &mut rig.pa), crate::player::BufferfeedStartOutcome::Failed);
                assert!(crate::route::pending_route_start().is_none(), "refused candidate stayed Prepared");
                assert!(crate::route::control_phase_label().starts_with("Failed("));
                assert_eq!(rig.status(), old_status, "refusing the new transaction cannot settle the old URI attempt again");
                assert!(!rig.pa.is_live(), "no native Engine was constructed");
                assert!(rig.pa.uri_live() && rig.pa.uri_closing());
                assert_eq!(rig.transport.sent(), calls);
                assert_eq!(rig.transport.closed(), 1);
                *rig.transport.close_status.lock().unwrap() = Some(CloseStatus::Released);
                drive(&mut rig.ps, &mut rig.pa);
                assert!(!rig.pa.uri_live());
            });
        }

        #[test]
        fn uri_acb_exported_host_is_invalid_without_creating_window_or_registering() {
            let _serial=plx_base::testlock::serial(); let _host=ExportedHost::arm();
            let _=logged(|| {
                for variant in [Variant::AcbControl,Variant::AcbDass] {
                    let mut rig=Rig::new(variant);
                    assert!(!rig.session.as_ref().unwrap().window);
                    rig.frame(0);
                    assert!(rig.is_done()); assert_eq!(rig.status(),crate::route::RouteStartStatus::Failed);
                    assert!(rig.transport.opened().is_empty()); assert!(rig.transport.sent().is_empty());
                }
            });
        }

        #[test]
        fn uri_acb_quarantined_plane_exits_without_waiting_again_for_released_bus() {
            let _serial=plx_base::testlock::serial();
            let _=logged(|| {
                let mut rig=Rig::new(Variant::AtLoad); rig.load_answered();
                let mut session=rig.session.take().unwrap();
                let sid=session.lifecycle.session().unwrap();
                session.lifecycle(LifeEvent::AcbTouched{session:sid},0);
                rig.pa.install_uri(session); stop(&mut rig.ps,&mut rig.pa); drive(&mut rig.ps,&mut rig.pa);
                assert!(rig.pa.uri_closing()); assert!(rig.pa.uri_plane_open());
                let exit=crate::player::engine::exit_native_with(&mut rig.pa,Instant::now,||panic!("released-bus plane quarantine needs no extra wait"));
                assert_eq!(exit,crate::player::engine::NativeExit::WorkerInFirmware);
                assert!(!rig.pa.uri_live()); assert_eq!(rig.transport.closed(),1);
            });
        }

        /// A stand-in for the media server that answers every request `200 {}` and remembers the
        /// request lines, so a test can say what the route sent and what it did not.
        struct CountingPms {
            port: i32,
            seen: Arc<Mutex<Vec<String>>>,
            stop: Arc<AtomicBool>,
            thread: Option<std::thread::JoinHandle<()>>,
        }

        impl CountingPms {
            fn start() -> CountingPms {
                let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
                let port = i32::from(listener.local_addr().unwrap().port());
                let seen = Arc::new(Mutex::new(Vec::new()));
                let stop = Arc::new(AtomicBool::new(false));
                let (log, flag) = (seen.clone(), stop.clone());
                let thread = std::thread::spawn(move || {
                    for socket in listener.incoming() {
                        if flag.load(Ordering::SeqCst) {
                            break;
                        }
                        let Ok(mut socket) = socket else { continue };
                        let _ = socket.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                        let mut request = String::new();
                        {
                            let mut reader = BufReader::new(&socket);
                            if reader.read_line(&mut request).unwrap_or(0) == 0 {
                                continue;
                            }
                            loop {
                                let mut header = String::new();
                                match reader.read_line(&mut header) {
                                    Ok(0) | Err(_) => break,
                                    Ok(_) if header == "\r\n" || header == "\n" => break,
                                    Ok(_) => {}
                                }
                            }
                        }
                        log.lock().unwrap().push(request.trim_end().to_owned());
                        let body = br#"{"MediaContainer":{}}"#;
                        let _ = write!(
                            socket,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = socket.write_all(body);
                    }
                });
                CountingPms { port, seen, stop, thread: Some(thread) }
            }

            fn requests(&self) -> Vec<String> {
                self.seen.lock().unwrap().clone()
            }
        }

        impl Drop for CountingPms {
            fn drop(&mut self) {
                self.stop.store(true, Ordering::SeqCst);
                let _ = std::net::TcpStream::connect(("127.0.0.1", u16::try_from(self.port).unwrap_or(0)));
                if let Some(thread) = self.thread.take() {
                    let _ = thread.join();
                }
            }
        }

        /// A playback the route prepared: a server registered at the stand-in, an encoder the stop
        /// owes that server, and the stream URL the engine would open. The host sink exports a window.
        struct Fork {
            ps: PlaybackSession,
            pa: crate::player::adapter::PlayerAdapter,
            pms: CountingPms,
            _host: ExportedHost,
        }

        impl Fork {
            fn new() -> Fork {
                let host = ExportedHost::arm();
                crate::player::SHARED.reset_session();
                plx_plex::plex::reset_servers_for_test();
                let pms = CountingPms::start();
                let mut ps = PlaybackSession::IDLE;
                crate::route::reset_player_control_for_test(&ps);
                let sid = plx_plex::plex::register_for_test("uriprobe-fork-test", "127.0.0.1", pms.port, "token", "uriprobe-client");
                crate::route::swap_cur_sid_for_test(&mut ps, sid);
                crate::route::install_active_encoder_for_test("probe-encoder");
                crate::route::set_url(&mut ps, URL);
                let pa = crate::player::adapter::PlayerAdapter::new(unsafe { MainThread::assume() });
                Fork { ps, pa, pms, _host: host }
            }

            /// The requests the server saw, once the stop worker has finished.
            fn requests_after_stop(&self) -> Vec<String> {
                crate::route::drain_scrobble();
                self.pms.requests()
            }
        }

        impl Drop for Fork {
            fn drop(&mut self) {
                plx_plex::plex::reset_servers_for_test();
            }
        }

        /// What the server must never hear from a playback that decoded nothing and watched nothing.
        fn viewing_writes(requests: &[String]) -> Vec<&String> {
            requests.iter().filter(|r| ["/:/timeline", "/:/scrobble", "/:/unscrobble"].iter().any(|p| r.contains(p))).collect()
        }

        fn only_the_encoder_stop(requests: &[String]) {
            assert!(viewing_writes(requests).is_empty(), "the probe reported viewing to the server: {requests:?}");
            let stops = requests.iter().filter(|r| r.contains("/video/:/transcode/universal/stop") && r.contains("probe-encoder")).count();
            assert_eq!(stops, 1, "positive control: the stand-in is the server the route talks to, and the stop retired the encoder: {requests:?}");
            assert_eq!(requests.len(), 1, "nothing but the encoder stop: {requests:?}");
        }

        #[test]
        fn uriprobe_fork_builds_no_engine_and_a_back_posts_nothing_to_plex_but_the_encoder_stop() {
            let _serial = plx_base::testlock::serial();
            let mut fork = Fork::new();
            let log = logged(|| {
                plx_base::devtrig::with_private_triggers(|| {
                    std::fs::write(plx_base::devtrig::path("uriprobe"), "late\n").expect("arm the trigger");
                    let attempt = match crate::player::start_bufferfeed_tracked(&mut fork.ps, &mut fork.pa) {
                        crate::player::BufferfeedStartOutcome::Launched(attempt) => attempt,
                        other => panic!("the fork did not launch: {other:?}"),
                    };
                    assert!(fork.pa.uri_live(), "the probe session is installed");
                    assert!(!fork.pa.is_live(), "and no Engine exists");
                    assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Pending);
                    assert_eq!(
                        crate::player::start_bufferfeed_tracked(&mut fork.ps, &mut fork.pa),
                        crate::player::BufferfeedStartOutcome::AlreadyRunning,
                        "a second Play under a live session is a no-op"
                    );
                    crate::player::stop_bufferfeed(&mut fork.ps, &mut fork.pa);
                    drive(&mut fork.ps, &mut fork.pa);
                    assert!(!fork.pa.uri_live(), "the released stop ended the session");
                    assert!(!fork.pa.uri_plane_open());
                });
            });
            only_the_encoder_stop(&fork.requests_after_stop());
            assert!(line_with(&log, " armed ").contains("variant=late"), "{log}");
            assert!(line_with(&log, " armed ").contains("window=exported"), "{log}");
            assert!(line_with(&log, " summary ").contains("end=back"), "{log}");
            for leak in LEAKS {
                assert!(!log.contains(leak), "{leak:?} reached the log:\n{log}");
            }
        }

        #[test]
        fn uriprobe_fork_without_a_bus_fails_the_start_on_the_first_frame_and_frees_the_slot() {
            let _serial = plx_base::testlock::serial();
            let mut fork = Fork::new();
            let log = logged(|| {
                plx_base::devtrig::with_private_triggers(|| {
                    std::fs::write(plx_base::devtrig::path("uriprobe"), "").expect("arm the trigger");
                    let attempt = match crate::player::start_bufferfeed_tracked(&mut fork.ps, &mut fork.pa) {
                        crate::player::BufferfeedStartOutcome::Launched(attempt) => attempt,
                        other => panic!("the fork did not launch: {other:?}"),
                    };
                    assert!(fork.pa.uri_live());
                    // The host has no bus: every registration is refused, and the machine ends the
                    // session on the next frame, which fails the attempt and frees the slot.
                    drive(&mut fork.ps, &mut fork.pa);
                    assert!(!fork.pa.uri_live(), "a session which ended by itself is released");
                    assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Failed);
                    crate::player::stop_bufferfeed(&mut fork.ps, &mut fork.pa);
                });
            });
            only_the_encoder_stop(&fork.requests_after_stop());
            assert!(log.contains("register shape=named refused"), "{log}");
            assert!(line_with(&log, " summary ").contains("end=refused:register"), "{log}");
        }

        #[test]
        fn uriprobe_fork_is_refused_while_an_earlier_session_still_owns_the_native_object() {
            let _serial = plx_base::testlock::serial();
            let mut fork = Fork::new();
            // A timed-out Load an earlier Engine parked: every native start is refused, and the
            // probe's window verbs would reach the same seam under it.
            let epoch = crate::player::SHARED.begin_native_session().expect("the native session is idle");
            assert!(crate::player::SHARED.abandon_native_session(epoch));
            let log = logged(|| {
                plx_base::devtrig::with_private_triggers(|| {
                    std::fs::write(plx_base::devtrig::path("uriprobe"), "").expect("arm the trigger");
                    assert_eq!(
                        crate::player::start_bufferfeed_tracked(&mut fork.ps, &mut fork.pa),
                        crate::player::BufferfeedStartOutcome::Failed
                    );
                    assert!(!fork.pa.uri_live(), "a refused start installs no session");
                    crate::player::stop_bufferfeed(&mut fork.ps, &mut fork.pa);
                });
            });
            assert!(crate::player::SHARED.retire_native_session(epoch), "the parked phase was left as found");
            assert!(line_with(&log, "probe NOT started").contains("earlier session"), "{log}");
            assert!(!log.contains("uriprobe: t="), "the machine never ran: nothing was opened\n{log}");
            assert!(viewing_writes(&fork.requests_after_stop()).is_empty());
        }
    }
}
