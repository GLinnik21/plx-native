//! The probe's tests: the real driver (`plx_play_uri::uriprobe_drive`) over a scripted URI
//! transport, with media's route half (`plx_media::player::uriprobe_drive`) around it. They moved
//! here from the media crate when the driver left it; the assertions are unchanged.

/// The display sink the probe opens its window on, as media's `start` hands it: the simulator's
/// host sink under hostsim, the port's installed sink otherwise.
fn display() -> &'static dyn plx_platform::tv::sink::DisplaySink {
    #[cfg(feature = "hostsim")]
    {
        &crate::app::adapters::hostsim::HOST_SINK
    }
    #[cfg(not(feature = "hostsim"))]
    {
        plx_platform::tv::sink::display_installed()
    }
}

/// What a test needs of a session the trait does not offer: the scripted open, the poll, a chosen
/// clock, and the facts of the driver under it.
impl UriSession {
    /// [`UriSession::open`] over the transport, display sink and plane owner the caller names:
    /// production passes the process's own, a test a scripted transport and its own owner.
    #[allow(clippy::too_many_arguments)]
    fn open_with_ports(ps: &PlaybackSession, mt: &MainThread, transport: &'static dyn UriMedia,
        sink: &'static dyn plx_platform::tv::sink::DisplaySink, plane: &'static plx_platform::tv::plane::PlaneOwner,
        attempt: RouteStartAttempt, variant: Variant, bps: AbrBps, firmware: Firmware, url: &str) -> UriSession {
        let input = ProbeOpen {
            attempt, url, transport, display: sink, plane,
            log: plx_base::eventlog::log,
            app_id: plx_base::paths::app_id().to_owned(),
            segmented_hls: plx_media::route::is_segmented_hls(ps),
            release_allowance_ms: u64::try_from(plx_media::player::release::RELEASE_DEADLINE.as_millis()).unwrap_or(u64::MAX),
        };
        UriSession::open(input, variant, bps, firmware, mt)
    }
    fn poll(&self) -> Result<plx_platform::tv::uri::UriPoll, String> { self.0.poll() }
    fn is_done(&self) -> bool { self.0.is_done() }
    #[cfg(feature = "hostsim")]
    fn window_created(&self) -> bool { self.0.window_created() }
    fn ownership_released(&self) -> bool { self.0.ownership_released() }
    #[cfg(feature = "hostsim")]
    fn touch_acb_for_test(&mut self, now_ms: u64) { self.0.touch_acb_for_test(now_ms); }
    /// One frame at the clock the test names, with media's route half around it (what `drive` does
    /// at the real one).
    fn advance_at(&mut self, ps: &mut PlaybackSession, mt: &MainThread,
        poll: Result<plx_platform::tv::uri::UriPoll, String>, now_ms: u64) {
        self.0.advance(&mut |attempt, how| settle(ps, attempt, settle_of(how)), mt, poll, now_ms);
        ingest_position(ps, self.0.attempt(), self.0.reported_position_ms());
    }
    fn stop_at(&mut self, ps: &mut PlaybackSession, mt: &MainThread, now_ms: u64) {
        self.0.stop_at(&mut |attempt, how| settle(ps, attempt, settle_of(how)), mt, now_ms);
    }
}

/// The adapter's end of an installed session: forwards the port to the session the test also holds.
struct Seated(Rc<RefCell<Option<UriSession>>>);

impl Seated {
    fn with<R>(&self, f: impl FnOnce(&mut UriSession) -> R) -> R {
        f(self.0.borrow_mut().as_mut().expect("the session is still seated"))
    }
}

impl UriProbeSession for Seated {
    fn plane_open(&self) -> bool { self.with(|s| s.plane_open()) }
    fn closing(&self) -> bool { self.with(|s| s.closing()) }
    fn route_start_attempt(&self) -> RouteStartAttempt { self.with(|s| s.route_start_attempt()) }
    fn acb_exit_quarantine(&self) -> bool { self.with(|s| s.acb_exit_quarantine()) }
    fn ready_to_release(&self) -> bool { self.with(|s| s.ready_to_release()) }
    fn advance(&mut self, settle: &mut dyn FnMut(RouteStartAttempt, ProbeSettle) -> bool, mt: &MainThread) -> Option<i64> {
        self.with(|s| UriProbeSession::advance(s, settle, mt))
    }
    fn stop(&mut self, settle: &mut dyn FnMut(RouteStartAttempt, ProbeSettle) -> bool, mt: &MainThread) {
        self.with(|s| UriProbeSession::stop(s, settle, mt));
    }
    fn release(self: Box<Self>, mt: &MainThread) {
        if let Some(session) = self.0.borrow_mut().take() {
            Box::new(session).release(mt);
        }
    }
}

use super::*;
use plx_media::player::attempt_view::Backend;
use plx_media::player::uriprobe_drive::{drive, ingest_position, settle, stop_for_test as stop};
use plx_media::route::PlaybackSession;
use plx_platform::tv::uri::UriMedia;
use std::cell::RefCell;
use std::rc::Rc;
use plx_platform::tv::uri::{CloseStatus, RequestId, SessionId, Shape, UriPoll, UriReply};
use std::collections::VecDeque;
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
    pa: PlayerAdapter,
    transport: &'static Scripted,
    attempt: RouteStartAttempt,
    session: Option<UriSession>,
    /// The session once [`install`](Rig::install) seated it in the adapter, which holds the other
    /// end of the handle.
    installed: Option<Rc<RefCell<Option<UriSession>>>>,
    now: u64,
}

impl Rig {
    fn new(variant: Variant) -> Rig {
        Rig::build(variant, None)
    }

    /// `refuse_open`: the text the transport answers `open` with, from the very first call.
    fn build(variant: Variant, refuse_open: Option<&str>) -> Rig {
        Rig::build_on(variant, refuse_open, display(), plx_platform::tv::plane::installed())
    }

    /// A session whose ACB plane is the one a test hands in (a scripted sink and its own plane
    /// owner), not the process-global pair other tests boot, reset and share.
    #[cfg(feature = "hostsim")]
    fn on_plane(variant: Variant, sink: &'static dyn plx_platform::tv::sink::DisplaySink,
        plane: &'static plx_platform::tv::plane::PlaneOwner) -> Rig {
        Rig::build_on(variant, None, sink, plane)
    }

    fn build_on(variant: Variant, refuse_open: Option<&str>,
        sink: &'static dyn plx_platform::tv::sink::DisplaySink,
        plane: &'static plx_platform::tv::plane::PlaneOwner) -> Rig {
        Rig::build_on_firmware(variant, refuse_open, sink, plane, Firmware::default())
    }

    /// [`build_on`](Rig::build_on) on a set that reported this firmware.
    fn build_on_firmware(variant: Variant, refuse_open: Option<&str>,
        sink: &'static dyn plx_platform::tv::sink::DisplaySink,
        plane: &'static plx_platform::tv::plane::PlaneOwner, firmware: Firmware) -> Rig {
        let mut ps = PlaybackSession::IDLE;
        plx_media::route::reset_player_control_for_test(&ps);
        let pa = crate::app::adapters::backends::test_adapter();
        let ticket = plx_media::route::begin_route_start().expect("a start owner");
        assert!(plx_media::route::prepare_route_start(ticket));
        let attempt = plx_media::route::claim_route_start_attempt(ticket).expect("an attempt");
        ps.begin_attempt_view(attempt, Backend::Uri);
        let transport = Scripted::leak();
        *transport.refuse_open.lock().unwrap() = refuse_open.map(str::to_owned);
        let session = UriSession::open_with_ports(&ps, pa.mt(), transport, sink, plane, attempt, variant, AbrBps::default(), firmware, URL);
        Rig { ps, pa, transport, attempt, session: Some(session), installed: None, now: 0 }
    }

    /// One frame, `ms` after the last.
    fn frame(&mut self, ms: u64) {
        self.now += ms;
        let replies = self.session.as_ref().unwrap().poll();
        let session = self.session.as_mut().expect("a live session");
        session.advance_at(&mut self.ps, self.pa.mt(), replies, self.now);
    }

    /// Seat the session in the adapter's probe slot, as `uriprobe_drive::start` does, keeping a
    /// handle on it for what the trait cannot ask (the poll, a chosen time) and the slot cannot hold.
    fn install(&mut self, session: UriSession) {
        let handle = Rc::new(RefCell::new(Some(session)));
        self.pa.install_uri_for_test(Box::new(Seated(handle.clone())));
        self.installed = Some(handle);
    }

    fn is_done(&self) -> bool {
        self.session.as_ref().is_some_and(UriSession::is_done)
    }

    fn status(&self) -> plx_media::route::RouteStartStatus {
        plx_media::route::route_start_status(self.attempt)
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
        session.stop_at(&mut self.ps, self.pa.mt(), self.now);
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
fn uri_clock_readout_requires_matching_service_evidence_and_never_confirms_picture() {
    let _serial = plx_base::testlock::serial();
    logged(|| {
        let mut rig = Rig::new(Variant::AtLoad);
        rig.load_answered();
        assert_eq!(rig.ps.attempt_view().facts().position_ns, None);
        rig.transport.say("subscribe", r#"{"mediaId":"other","currentTime":9000}"#);
        rig.frame(10);
        assert_eq!(rig.ps.attempt_view().facts().position_ns, None);
        rig.transport.say("subscribe", r#"{"mediaId":"_m1","currentTime":1200,"playing":true}"#);
        rig.frame(10);
        let facts = rig.ps.attempt_view().facts();
        assert_eq!(facts.position_ns, Some(1_200_000_000));
        assert_eq!(facts.reported_frame, None);
        assert_eq!(facts.playable_buffer_ms, None);
        assert_eq!(facts.duration_ns, None);
        rig.back();
    });
}

#[test]
fn uriprobe_driver_runs_a_whole_session_in_order_and_settles_the_attempt_started_once() {
    let _serial = plx_base::testlock::serial();
    let log = logged(|| {
        let mut rig = Rig::new(Variant::AtLoad);
        assert_eq!(rig.transport.opened(), [Shape::Named], "the first registration is the named client");
        assert_eq!(rig.status(), plx_media::route::RouteStartStatus::Pending, "the attempt waits on the service");
        rig.load_answered();
        assert_eq!(rig.transport.sent(), ["load", "subscribe"]);
        assert_eq!(rig.status(), plx_media::route::RouteStartStatus::Pending);
        rig.transport.say("subscribe", r#"{"mediaId":"_m1","loadCompleted":true}"#);
        rig.frame(10);
        assert_eq!(rig.status(), plx_media::route::RouteStartStatus::Started, "loadCompleted settles it");
        assert_eq!(rig.transport.sent(), ["load", "subscribe", "play"]);
        assert!(!rig.session.as_ref().unwrap().plane_open(), "not before `playing`");
        rig.transport.say("subscribe", r#"{"mediaId":"_m1","playing":true}"#);
        rig.frame(10);
        assert!(rig.session.as_ref().unwrap().plane_open(), "the plane opens on `playing`");
        rig.back();
        assert_eq!(rig.transport.sent(), ["load", "subscribe", "play", "unload"]);
        assert!(rig.transport.payload_of("unload").contains("_m1"));
        assert!(rig.transport.closed() >= 1, "the registration is closed");
        assert_eq!(rig.status(), plx_media::route::RouteStartStatus::Started, "a stop does not unsettle a started attempt");
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
fn uriprobe_driver_names_the_firmware_the_set_reported_on_the_armed_line() {
    let _serial = plx_base::testlock::serial();
    let log = logged(|| {
        let firmware = Firmware { release: "11.1.0".to_owned(), build: "11.1.0-3701".to_owned() };
        let _rig = Rig::build_on_firmware(Variant::Drive, None, display(), plx_platform::tv::plane::installed(), firmware);
    });
    let armed = line_with(&log, " armed ");
    assert!(armed.contains("variant=drive") && armed.ends_with("fw=11.1.0 build=11.1.0-3701"), "{armed}");
    // A firmware the set did not report (empty strings) prints dashes.
    let log = logged(|| {
        let _rig = Rig::new(Variant::Drive);
    });
    assert!(line_with(&log, " armed ").ends_with("fw=- build=-"), "{log}");
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
        assert_eq!(rig.status(), plx_media::route::RouteStartStatus::Failed, "a failed register fails the start");
        assert!(rig.transport.sent().is_empty());
        // The player is not stuck: the Back that follows finds a finished session and ends cleanly.
        rig.back();
        assert_eq!(rig.status(), plx_media::route::RouteStartStatus::Failed);
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
        assert_eq!(rig.status(), plx_media::route::RouteStartStatus::Failed, "the attempt the Back interrupted fails");
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
        assert_eq!(rig.status(), plx_media::route::RouteStartStatus::Failed);
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
        assert_eq!(rig.status(), plx_media::route::RouteStartStatus::Failed);
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
        assert_eq!(rig.status(), plx_media::route::RouteStartStatus::Pending);
        assert!(!rig.is_done());
        rig.frame(10);
        assert!(rig.is_done());
        assert_eq!(rig.transport.opened(), [Shape::Named, Shape::Anon], "the ladder still tries the second shape");
        assert_eq!(rig.status(), plx_media::route::RouteStartStatus::Failed);
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
        rig.install(session);
        assert!(rig.pa.uri_live());
        transport.deny("register", "named refused");
        drive(&mut rig.ps, &mut rig.pa);
        drive(&mut rig.ps, &mut rig.pa);
        transport.deny("register", "anon refused");
        drive(&mut rig.ps, &mut rig.pa);
        drive(&mut rig.ps, &mut rig.pa);
        assert!(!rig.pa.uri_live(), "a finished session is released, so the next Play is not refused");
        assert!(!rig.pa.uri_plane_open());
        assert_eq!(rig.status(), plx_media::route::RouteStartStatus::Failed);
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
        rig.install(session);
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
        let session = rig.session.take().unwrap(); rig.install(session);
        stop(&mut rig.ps, &mut rig.pa); stop(&mut rig.ps, &mut rig.pa);
        assert_eq!(rig.transport.closed(),1);
        assert!(rig.pa.uri_live()); assert!(rig.pa.uri_closing());
        assert_eq!(rig.status(), plx_media::route::RouteStartStatus::Failed);
        {
            let handle = rig.installed.clone().unwrap();
            let mut slot = handle.borrow_mut();
            let session = slot.as_mut().unwrap();
            let poll = session.poll();
            session.advance_at(&mut rig.ps, rig.pa.mt(), poll, 20_000);
            assert!(session.closing()); assert!(!session.ownership_released());
        }
        assert!(rig.pa.uri_live());
        // A frame loop with an idle route still polls; no player screen is required.
        let mut idle=PlaybackSession::IDLE;
        *rig.transport.close_status.lock().unwrap()=Some(CloseStatus::Released);
        drive(&mut idle,&mut rig.pa);
        assert!(!rig.pa.uri_live()); assert_eq!(rig.transport.closed(),1);
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

/// The tests which need the host sink to export a window (the webOS 5+ path) or the whole
/// engine around the driver: `make test-hostsim C=plx_media T=uriprobe`.
#[cfg(feature = "hostsim")]
mod hostsim {
    use super::*;
    #[cfg(feature = "play-feed")]
    use plx_play_feed::shared::FEED_SHARED;
    use std::time::Instant;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    /// The host sink exports a window (`VP_EXPORTED`, id `clocksink-window`) while this lives
    /// and is put back as it was found.
    struct ExportedHost;

    impl ExportedHost {
        fn arm() -> ExportedHost {
            plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
            plx_play_feed::ffi_host::force_clocksink_for_test(true);
            ExportedHost
        }
    }

    impl Drop for ExportedHost {
        fn drop(&mut self) {
            plx_play_feed::ffi_host::force_clocksink_for_test(false);
        }
    }

    #[test]
    fn uriprobe_driver_on_an_exporting_set_names_the_window_in_the_load_and_places_it_on_videoinfo() {
        let _serial = plx_base::testlock::serial();
        let _host = ExportedHost::arm();
        let log = logged(|| {
            let mut rig = Rig::new(Variant::AtLoad);
            assert!(rig.session.as_ref().unwrap().window_created(), "the window was created");
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
            assert_eq!(rig.status(), plx_media::route::RouteStartStatus::Started, "loadCompleted settles the attempt");
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
            assert!(!rig.session.as_ref().unwrap().window_created(), "no window was created");
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
            let session=rig.session.take().unwrap(); rig.install(session);
            stop(&mut rig.ps,&mut rig.pa);
            assert!(rig.pa.uri_live()); assert!(rig.pa.uri_closing());
            assert_eq!(plx_media::player::start_bufferfeed_tracked(&mut rig.ps,&mut rig.pa),plx_media::player::BufferfeedStartOutcome::Failed);
            let start=Instant::now(); let mut ticks=0;
            let exit=plx_media::player::exit_native_with(&mut rig.ps,&mut rig.pa,|| { ticks+=1; start+std::time::Duration::from_secs(ticks*5) },||{});
            assert_eq!(exit,plx_media::player::NativeExit::WorkerInFirmware);
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
            rig.install(session);
            stop(&mut rig.ps, &mut rig.pa);
            let transaction = plx_media::route::begin_route_start().expect("new route owner");
            assert!(plx_media::route::prepare_route_start(transaction));
            assert_eq!(plx_media::route::pending_route_start(), Some(transaction));
            let calls = rig.transport.sent();
            let old_status = rig.status();
            assert_eq!(plx_media::player::start_bufferfeed_tracked(&mut rig.ps, &mut rig.pa), plx_media::player::BufferfeedStartOutcome::Failed);
            assert!(plx_media::route::pending_route_start().is_none(), "refused candidate stayed Prepared");
            assert!(plx_media::route::control_phase_label().starts_with("Failed("));
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
                assert!(!rig.session.as_ref().unwrap().window_created());
                rig.frame(0);
                assert!(rig.is_done()); assert_eq!(rig.status(),plx_media::route::RouteStartStatus::Failed);
                assert!(rig.transport.opened().is_empty()); assert!(rig.transport.sent().is_empty());
            }
        });
    }

    /// The ACB plane of a probe that touched it, over a scripted sink that accepts UNLOADED as
    /// task 5 and a plane owner of its own (booted with a handle): the firmware's completion is
    /// whatever the test queues, or `complete_at_once` queues the event-1 one with the dispatch.
    /// Closed through `stop` and one `drive`, as the engine's teardown does; the transport
    /// releases the bus at once.
    fn acb_plane_closed(complete_at_once: bool) -> (Rig, &'static plx_platform::tv::sink::ScriptedPlaneSink, &'static plx_platform::tv::plane::PlaneOwner) {
        use plx_platform::tv::{plane::{HandleSeat, PlaneFacts, PlaneHandle, PlaneOwner}, sink::{PlaneUnload, ScriptedPlaneSink}};
        let sink: &'static ScriptedPlaneSink = Box::leak(Box::new(ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 })));
        sink.complete_at_once.store(complete_at_once, Ordering::SeqCst);
        let plane: &'static PlaneOwner = Box::leak(Box::new(PlaneOwner::new()));
        plane.boot_handle(PlaneHandle::Uri, PlaneFacts { handle: true, player_type: 0 },
            HandleSeat { id: 0, initialized: true });
        let mut rig = Rig::on_plane(Variant::AtLoad, sink, plane);
        rig.load_answered();
        let mut session = rig.session.take().unwrap();
        session.touch_acb_for_test(0);
        rig.install(session);
        stop(&mut rig.ps, &mut rig.pa);
        drive(&mut rig.ps, &mut rig.pa);
        (rig, sink, plane)
    }

    /// REWRITTEN for the fenced contract (it used to assert that the dispatch itself quarantined
    /// the plane): the plane now waits for the firmware's completion of UNLOADED and is
    /// quarantined by a completion which is not event 1 (or by the deadline, which the plane's own
    /// machine tests cover on a clock they own). What stays: once the plane is quarantined, the
    /// bus is already released, so the exit leaves at once without waiting for it again.
    #[test]
    fn uri_acb_quarantined_plane_exits_without_waiting_again_for_released_bus() {
        use plx_platform::tv::plane::{Phase, Why};
        let _serial=plx_base::testlock::serial();
        let log=logged(|| {
            let (mut rig,sink,plane)=acb_plane_closed(false);
            // UNLOADED ran once and the plane waits on the firmware: still closing, plane held.
            assert_eq!(sink.unload_count(),1); assert_eq!(plane.phase(),Phase::Releasing);
            assert!(rig.pa.uri_closing()); assert!(rig.pa.uri_plane_open());
            assert!(!rig.pa.uri_acb_exit_quarantine_for_test(),"a plane still being released is not yet quarantined");
            // The firmware answers the dispatch (task 5) with something other than "complete".
            sink.mailbox.push(5,-1);
            drive(&mut rig.ps,&mut rig.pa);
            assert_eq!(plane.phase(),Phase::Quarantined(Why::Completion(-1)));
            assert!(rig.pa.uri_closing()); assert!(rig.pa.uri_plane_open());
            assert!(rig.pa.uri_acb_exit_quarantine_for_test());
            let exit=plx_media::player::exit_native_with(&mut rig.ps,&mut rig.pa,Instant::now,||panic!("released-bus plane quarantine needs no extra wait"));
            assert_eq!(exit,plx_media::player::NativeExit::WorkerInFirmware);
            assert!(!rig.pa.uri_live()); assert_eq!(rig.transport.closed(),1);
            assert_eq!(sink.unload_count(),1,"UNLOADED is never dispatched a second time");
        });
        assert!(log.contains("acb=quarantined bus=released reason=completion ev=-1"),"{log}");
        assert!(!log.contains("plane-release=proven"),"{log}");
    }

    /// The happy path of the same contract: the firmware completes UNLOADED with event 1 as the
    /// dispatch returns, so the plane is Released within the closing frame, the registration is
    /// released with it, and the exit has nothing to wait for and nothing quarantined to leave.
    #[test]
    fn uri_acb_released_plane_ends_ownership_and_the_exit_is_clean() {
        use plx_platform::tv::plane::Phase;
        let _serial=plx_base::testlock::serial();
        let log=logged(|| {
            let (mut rig,sink,plane)=acb_plane_closed(true);
            assert_eq!(sink.unload_count(),1); assert_eq!(plane.phase(),Phase::Released);
            assert!(!rig.pa.uri_live(),"the completion released the registration, and the slot with it");
            assert_eq!(rig.transport.closed(),1);
            let exit=plx_media::player::exit_native_with(&mut rig.ps,&mut rig.pa,Instant::now,||panic!("a released plane needs no wait"));
            assert_eq!(exit,plx_media::player::NativeExit::Clean);
            assert!(!rig.pa.uri_live()); assert_eq!(rig.transport.closed(),1);
            assert_eq!(sink.unload_count(),1,"UNLOADED is never dispatched a second time");
        });
        assert!(log.contains("acb=released bus=released plane-release=proven"),"{log}");
        assert!(!log.contains("quarantined"),"{log}");
    }

    /// The exit itself waits for a plane still being released, and only for as long as the
    /// firmware takes: its own drive pumps the completion, ownership ends, and it leaves clean.
    #[test]
    fn uri_acb_exit_follows_a_releasing_plane_to_the_firmware_completion() {
        use plx_platform::tv::plane::Phase;
        let _serial=plx_base::testlock::serial();
        let log=logged(|| {
            let (mut rig,sink,plane)=acb_plane_closed(false);
            assert_eq!(plane.phase(),Phase::Releasing); assert!(rig.pa.uri_live() && rig.pa.uri_closing());
            sink.mailbox.push(5,1);
            let exit=plx_media::player::exit_native_with(&mut rig.ps,&mut rig.pa,Instant::now,||panic!("the completion was already queued"));
            assert_eq!(exit,plx_media::player::NativeExit::Clean);
            assert_eq!(plane.phase(),Phase::Released); assert!(!rig.pa.uri_live());
            assert_eq!(rig.transport.closed(),1); assert_eq!(sink.unload_count(),1);
        });
        assert!(log.contains("acb=released bus=released plane-release=proven"),"{log}");
        assert!(!log.contains("quarantined"),"{log}");
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
        pa: PlayerAdapter,
        pms: CountingPms,
        _host: ExportedHost,
    }

    impl Fork {
        fn new() -> Fork {
            let host = ExportedHost::arm();
            plx_play_feed::ff_test_support::reset_session_for_test(&mut plx_media::player::reset_common_for_test);
            plx_plex::plex::reset_servers_for_test();
            let pms = CountingPms::start();
            let mut ps = PlaybackSession::IDLE;
            plx_media::route::reset_player_control_for_test(&ps);
            let sid = plx_plex::plex::register_for_test("uriprobe-fork-test", "127.0.0.1", pms.port, "token", "uriprobe-client");
            plx_media::route::swap_cur_sid_for_test(&mut ps, sid);
            plx_media::route::install_active_encoder_for_test("probe-encoder");
            plx_media::route::set_url(&mut ps, URL);
            assert!(ps.qualify_resolved_feed_offer(plx_media::route::backend_policy::OfferId::FEED_ORIGINAL,
                plx_media::route::CandidatePurpose::NewCandidate),
                "the prepared fixture source must qualify its stable Feed recipe");
            let mut pa = crate::app::adapters::backends::test_adapter();
            // The composition installs the probe at boot (`compose_backends`); the fork is a boot.
            super::super::install(&mut pa);
            Fork { ps, pa, pms, _host: host }
        }

        /// The requests the server saw, once the stop worker has finished.
        fn requests_after_stop(&self) -> Vec<String> {
            plx_media::route::drain_scrobble();
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
                let attempt = match plx_media::player::start_bufferfeed_tracked(&mut fork.ps, &mut fork.pa) {
                    plx_media::player::BufferfeedStartOutcome::Launched(attempt) => attempt,
                    other => panic!("the fork did not launch: {other:?}"),
                };
                assert!(fork.pa.uri_live(), "the probe session is installed");
                assert!(!fork.pa.is_live(), "and no Engine exists");
                assert_eq!(plx_media::route::route_start_status(attempt), plx_media::route::RouteStartStatus::Pending);
                assert_eq!(
                    plx_media::player::start_bufferfeed_tracked(&mut fork.ps, &mut fork.pa),
                    plx_media::player::BufferfeedStartOutcome::AlreadyRunning,
                    "a second Play under a live session is a no-op"
                );
                plx_media::player::stop_bufferfeed(&mut fork.ps, &mut fork.pa);
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
                let attempt = match plx_media::player::start_bufferfeed_tracked(&mut fork.ps, &mut fork.pa) {
                    plx_media::player::BufferfeedStartOutcome::Launched(attempt) => attempt,
                    other => panic!("the fork did not launch: {other:?}"),
                };
                assert!(fork.pa.uri_live());
                // The host has no bus: every registration is refused, and the machine ends the
                // session on the next frame, which fails the attempt and frees the slot.
                drive(&mut fork.ps, &mut fork.pa);
                assert!(!fork.pa.uri_live(), "a session which ended by itself is released");
                assert_eq!(plx_media::route::route_start_status(attempt), plx_media::route::RouteStartStatus::Failed);
                plx_media::player::stop_bufferfeed(&mut fork.ps, &mut fork.pa);
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
        let epoch = FEED_SHARED.begin_native_session().expect("the native session is idle");
        assert!(FEED_SHARED.abandon_native_session(epoch));
        let log = logged(|| {
            plx_base::devtrig::with_private_triggers(|| {
                std::fs::write(plx_base::devtrig::path("uriprobe"), "").expect("arm the trigger");
                assert_eq!(
                    plx_media::player::start_bufferfeed_tracked(&mut fork.ps, &mut fork.pa),
                    plx_media::player::BufferfeedStartOutcome::Failed
                );
                assert!(!fork.pa.uri_live(), "a refused start installs no session");
                plx_media::player::stop_bufferfeed(&mut fork.ps, &mut fork.pa);
            });
        });
        assert!(FEED_SHARED.retire_native_session(epoch), "the parked phase was left as found");
        assert!(line_with(&log, "probe NOT started").contains("earlier session"), "{log}");
        assert!(!log.contains("uriprobe: t="), "the machine never ran: nothing was opened\n{log}");
        assert!(viewing_writes(&fork.requests_after_stop()).is_empty());
    }
}
