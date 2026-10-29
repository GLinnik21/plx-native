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
//! of the stream) is released by [`drive`] at once: its window is destroyed and the slot is freed, so
//! the next Play is not refused as "already running".
//!
//! **What stays out of it.** No Engine thread runs, so nothing is reported to Plex; nothing here
//! sets `ps.finished`, so a stop cannot mark the item watched. The URL is held by [`UriSession`] for
//! the `load` payload and nowhere else: no `Debug`, no log line, and the payload is built by
//! [`build_load`] straight into the transport. Every line the machine writes is complete and printed
//! verbatim; the few lines this driver adds (the window) carry no URL either.
//!
//! The whole module is the `devtriggers` surface (`player/mod.rs`).

use super::log;
use super::uriprobe::{build_load, Config, Kind, Scheme, Settle, UriCall, UriEffect, UriEvent, UriProbe, Variant};
use crate::route::{PlaybackSession, RouteStartAttempt, RouteStartResult, RouteStartTransaction};
use plx_base::task::MainThread;
use plx_platform::tv::uri::{UriMedia, UriReply};
use std::collections::VecDeque;
use std::os::raw::c_int;
use std::time::Instant;

/// The exported window is placed over the whole canvas; the coded size the service reports for the
/// picture (`videoInfo`) only changes the SOURCE rectangle (`engine::place_exported` does the same).
const CANVAS: (c_int, c_int) = (1_920, 1_080);

/// One running probe: the machine, the pipe to `com.webos.media`, and what the driver owns for it.
/// No `Debug`: [`url`](Self::url) is a media URL with a token in it.
pub struct UriSession {
    probe: UriProbe,
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
        if variant != Variant::NoWindow && super::sink().window_mode() == super::VP_EXPORTED {
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
        };
        let mut session = UriSession {
            probe: UriProbe::new(config),
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

    /// The service says the video plays: from `playing` until the session ends.
    pub fn plane_open(&self) -> bool {
        self.probe.plane_open()
    }

    /// Re-place the exported window for a coded picture of `width` x `height`.
    fn place(&self, mt: &MainThread, width: c_int, height: c_int) {
        let rv = unsafe { super::sink().place_window(mt, width, height, 0, 0, CANVAS.0, CANVAS.1) };
        log(&format!("uriprobe: t={} window placed src={width}x{height} rv={rv}", self.now_ms()));
    }

    /// One frame: the replies which arrived, then the clock. Everything a reply or a deadline sets
    /// off, including the events of calls that failed on the spot, runs to the end before this
    /// returns.
    fn advance(&mut self, settle: &mut dyn Settler, mt: &MainThread, replies: Vec<UriReply>, now_ms: u64) {
        self.inbox.extend(replies.iter().map(UriEvent::from_reply));
        self.inbox.push_back(UriEvent::Tick);
        self.drain(settle, mt, now_ms);
    }

    fn drain(&mut self, settle: &mut dyn Settler, mt: &MainThread, now_ms: u64) {
        while let Some(event) = self.inbox.pop_front() {
            self.feed(event, settle, mt, now_ms);
        }
    }

    /// Step the machine once and execute what it asks for, in order.
    fn feed(&mut self, event: UriEvent, settle: &mut dyn Settler, mt: &MainThread, now_ms: u64) {
        let mut effects = Vec::new();
        self.probe.step(event, now_ms, &mut effects);
        for effect in effects {
            if let Some(failed) = self.execute(effect, settle, mt, now_ms) {
                self.inbox.push_back(failed);
            }
        }
        if self.probe.is_done() {
            self.url = String::new();
        }
    }

    /// Run one effect. `Some` is the event of an effect the transport refused to carry out.
    fn execute(&mut self, effect: UriEffect, settle: &mut dyn Settler, mt: &MainThread, now_ms: u64) -> Option<UriEvent> {
        match effect {
            UriEffect::Open(shape) => self.transport.open(shape).err().map(|text| UriEvent::from_failure("register", &text)),
            UriEffect::Call(call) => {
                let method = call.method();
                let payload = match &call {
                    UriCall::Load { window_id } => build_load(&self.url, &self.app_id, window_id.as_deref()),
                    other => other.payload().unwrap_or_default(),
                };
                self.transport.call(method, &payload).err().map(|text| UriEvent::from_failure(method, &text))
            }
            UriEffect::Close => {
                self.transport.close();
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

    /// Let go of what the session holds: the bus registration (idempotent, the machine closed it on
    /// every way out already) and the exported window.
    fn release(mut self, mt: &MainThread) {
        self.url = String::new();
        self.transport.close();
        if self.window {
            unsafe { super::sink().destroy_window(mt) };
        }
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
    let Some(attempt) = crate::route::claim_route_start_attempt(route_start) else {
        log("uriprobe: the prepared route lost its start before the probe began");
        return Err(RouteStartResult::StartFailed);
    };
    let session = UriSession::open(ps, pa.mt(), plx_platform::tv::uri::installed(), attempt, variant, url);
    pa.install_uri(session);
    Ok(attempt)
}

/// Once a frame, before `observe_video_plane`: drain the transport, step the machine, execute its
/// effects, settle the attempt, place the window on `videoInfo`. A session that ended is released
/// here and its slot freed.
pub fn drive(ps: &mut PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    let (session, mt) = pa.uri_split();
    let Some(session) = session else { return };
    let replies = session.transport.poll();
    let now_ms = session.now_ms();
    session.advance(&mut RouteSettler(ps), mt, replies, now_ms);
    let finished = session.probe.is_done();
    if finished {
        if let Some(done) = pa.take_uri() {
            done.release(pa.mt());
        }
    }
}

/// End a session that is still running (`engine::teardown`: Back, or an app switch): the machine's
/// own stop path settles a still-`Starting` attempt as failed, unloads once the service has named a
/// media, closes the registration and writes the summary; then the window goes.
pub(super) fn stop(ps: &mut PlaybackSession, mt: &MainThread, mut session: UriSession) {
    session.inbox.clear();
    let now_ms = session.now_ms();
    let mut settle = RouteSettler(ps);
    session.feed(UriEvent::Back, &mut settle, mt, now_ms);
    session.drain(&mut settle, mt, now_ms);
    session.release(mt);
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_platform::tv::uri::Shape;
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
        refuse_call: Mutex<Option<(&'static str, String)>>,
    }

    impl UriMedia for Scripted {
        fn open(&self, shape: Shape) -> Result<(), String> {
            self.opens.lock().unwrap().push(shape);
            self.refuse_open.lock().unwrap().clone().map_or(Ok(()), Err)
        }
        fn call(&self, method: &'static str, payload: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push((method, payload.to_owned()));
            match self.refuse_call.lock().unwrap().clone() {
                Some((refused, text)) if refused == method => Err(text),
                _ => Ok(()),
            }
        }
        fn poll(&self) -> Vec<UriReply> {
            self.replies.lock().unwrap().drain(..).collect()
        }
        fn close(&self) {
            self.closes.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Scripted {
        fn leak() -> &'static Scripted {
            Box::leak(Box::default())
        }
        fn say(&self, method: &'static str, payload: &str) {
            self.replies.lock().unwrap().push_back(UriReply { method, payload: payload.to_owned(), ok: true });
        }
        fn deny(&self, method: &'static str, text: &str) {
            self.replies.lock().unwrap().push_back(UriReply { method, payload: text.to_owned(), ok: false });
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
            let replies = self.transport.poll();
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
            let session = self.session.take().expect("a live session");
            stop(&mut self.ps, self.pa.mt(), session);
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
            assert_eq!(rig.transport.opened(), [Shape::Named, Shape::Anon], "named, then anonymous");
            assert_eq!(rig.transport.closed(), 1, "the refused registration is closed before the next opens");
            assert!(rig.transport.sent().is_empty(), "nothing is sent before a registration succeeds");
            rig.transport.say("register", "");
            rig.frame(10);
            assert_eq!(rig.transport.sent(), ["load"], "the anonymous registration sends the load again");
            rig.back();
        });
        assert!(line_with(&log, "register shape=named refused").contains("denied"), "{log}");
        assert!(log.contains("register shape=anon ok"), "{log}");
    }

    #[test]
    fn uriprobe_driver_leaves_the_attempt_start_failed_when_both_registrations_are_refused() {
        let _serial = plx_base::testlock::serial();
        let log = logged(|| {
            let mut rig = Rig::new(Variant::AtLoad);
            rig.transport.deny("register", "named refused");
            rig.frame(10);
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
        assert!(line_with(&log, "register shape=named refused").contains("no bus"), "{log}");
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
            transport.deny("register", "anon refused");
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
            let session = rig.pa.take_uri().expect("still live");
            stop(&mut rig.ps, rig.pa.mt(), session);
            assert!(!rig.pa.uri_plane_open());
        });
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
                    assert!(!fork.pa.uri_live(), "the stop ended the session");
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
            assert!(line_with(&log, "register shape=named refused").contains("no bus"), "{log}");
            assert!(line_with(&log, " summary ").contains("end=refused:register"), "{log}");
        }
    }
}
