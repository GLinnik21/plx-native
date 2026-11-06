//! The URI probe's route half and its port (issue #1064, dev trigger `plxnative-uriprobe`). Route
//! ownership stays here: the claimed start attempt, the settle into the route reducer, the facts the
//! attempt view takes, the adapter's one probe slot and when it is freed, and every `uriprobe:` line
//! the route side writes. The probe itself, the independent driver over the URI transport and the
//! machine it steps (the play-uri crate's `uriprobe` and `uriprobe_drive`), is the application's: it
//! implements [`UriProbe`] and [`UriProbeSession`] (`dev::uriprobe`) and installs them at boot
//! ([`PlayerAdapter::install_uri_probe`](super::adapter::PlayerAdapter::install_uri_probe)), so this
//! crate names neither the driver nor its types. Without an installed probe the trigger file is not
//! even read.

use crate::route::{PlaybackSession, RouteStartAttempt, RouteStartResult};
use crate::route::RouteStartTransaction;
use plx_base::task::MainThread;

/// How the route-start attempt a probe stands in for ends: media's own word for what the driver's
/// machine reports, mapped onto [`RouteStartResult`] by [`settle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeSettle {
    Started,
    StartFailed,
}

/// One probe session held by the adapter. The URL stays inside the driver. No `Debug`: a session may
/// hold a tokenised URL.
pub trait UriProbeSession {
    /// The service says the probe's video plays, or the ACB experiment's plane ownership is retained.
    fn plane_open(&self) -> bool;
    /// Logical stop requested or ownership quarantined; the slot stays until release.
    fn closing(&self) -> bool;
    /// The route-start attempt this session stands in for.
    fn route_start_attempt(&self) -> RouteStartAttempt;
    /// ACB plane ownership that outlives the bus until process exit.
    fn acb_exit_quarantine(&self) -> bool;
    /// Logically done AND physically released: [`release`](Self::release) may now free the slot.
    fn ready_to_release(&self) -> bool;
    /// One frame: the replies which arrived, then the clock. `settle` is the route reducer's side of
    /// the attempt (it answers whether this call settled it). Returns the service's reported
    /// position in milliseconds, when it has one.
    fn advance(&mut self, settle: &mut dyn FnMut(RouteStartAttempt, ProbeSettle) -> bool, mt: &MainThread) -> Option<i64>;
    /// End the logical session; physical ownership remains until [`ready_to_release`](Self::ready_to_release).
    fn stop(&mut self, settle: &mut dyn FnMut(RouteStartAttempt, ProbeSettle) -> bool, mt: &MainThread);
    /// Give the physical ownership back (destroy the window). Called once, when ready.
    fn release(self: Box<Self>, mt: &MainThread);
}

/// What media resolves at route start and hands the application to open a session over. Resolved
/// here so the application reads no registry of its own and every line the open writes is the one
/// the in-crate probe wrote. No `Debug`: `url` may carry a token.
pub struct ProbeOpen<'a> {
    pub attempt: RouteStartAttempt,
    pub url: &'a str,
    /// The URI bus ([`plx_platform::tv::uri::installed`]).
    pub transport: &'static dyn plx_platform::tv::uri::UriMedia,
    /// The display sink of the Feed start path this probe diverts from, fence included.
    pub display: &'static dyn plx_platform::tv::sink::DisplaySink,
    /// The process's ACB plane owner.
    pub plane: &'static plx_platform::tv::plane::PlaneOwner,
    pub log: fn(&str),
    pub app_id: String,
    /// The route is segmented HLS (the probe's `Kind::Hls`), not a direct file.
    pub segmented_hls: bool,
    /// The native release deadline, in milliseconds.
    pub release_allowance_ms: u64,
}

/// An armed trigger: what the application read out of the trigger text, waiting to open a session.
pub trait ProbeLaunch {
    /// Create the session (the exported window for the variants which use one, then the first
    /// registration). The attempt is already claimed; the first step settles nothing.
    fn open(self: Box<Self>, input: ProbeOpen<'_>, mt: &MainThread) -> Box<dyn UriProbeSession>;
}

/// The application's probe, installed on the adapter at boot.
pub trait UriProbe {
    /// The launch a `plxnative-uriprobe` trigger text names; `None` when it names no variant, which
    /// leaves the probe unarmed and the play normal.
    fn armed(&self, text: &str) -> Option<Box<dyn ProbeLaunch>>;

    /// Whether the native object is free of an earlier session. The probe's window verbs reach the
    /// same seam an Engine does, so a start refuses, as an Engine's does, while an earlier session
    /// (a parked timed-out Load) still owns it: the tester plays again.
    fn native_slot_available(&self) -> bool;
}

/// The route reducer's side of the attempt: the driver's report mapped onto [`RouteStartResult`].
pub fn settle(ps: &mut PlaybackSession, attempt: RouteStartAttempt, how: ProbeSettle) -> bool {
    let result = match how {
        ProbeSettle::Started => RouteStartResult::Started,
        ProbeSettle::StartFailed => RouteStartResult::StartFailed,
    };
    crate::route::settle_route_start(ps, attempt, result)
}

/// The service clock the probe reported is a reading of the service, never presentation evidence:
/// it becomes the attempt's position and nothing else.
pub fn ingest_position(ps: &mut PlaybackSession, attempt: RouteStartAttempt, position_ms: Option<i64>) {
    ps.ingest_attempt_facts(attempt, super::attempt_view::Backend::Uri, super::attempt_view::AttemptFacts {
        position_ns: position_ms.and_then(|ms| ms.checked_mul(1_000_000)),
        ..Default::default()
    });
}

/// Claim and install the route start before any probe reply can settle it. Only the Feed start path
/// arms the probe (`route_start::start_bufferfeed_inner`), so a build without Feed has no way to
/// open one.
pub(super) fn start(ps: &mut PlaybackSession, pa: &mut super::adapter::PlayerAdapter,
    route_start: RouteStartTransaction, url: &str, launch: Box<dyn ProbeLaunch>)
    -> Result<RouteStartAttempt, RouteStartResult> {
    if pa.uri_live() { return Err(RouteStartResult::StartFailed); }
    let Some(attempt) = crate::route::claim_route_start_attempt(route_start) else {
        super::log("uriprobe: the prepared route lost its start before the probe began");
        return Err(RouteStartResult::StartFailed);
    };
    ps.begin_attempt_view(attempt, super::attempt_view::Backend::Uri);
    let input = ProbeOpen {
        attempt, url,
        transport: plx_platform::tv::uri::installed(),
        display: pa.display_sink(),
        plane: super::plane(),
        log: super::log,
        app_id: plx_base::paths::app_id().to_owned(),
        segmented_hls: crate::route::is_segmented_hls(ps),
        release_allowance_ms: u64::try_from(super::release::RELEASE_DEADLINE.as_millis()).unwrap_or(u64::MAX),
    };
    let session = launch.open(input, pa.mt());
    pa.install_uri(session);
    Ok(attempt)
}

/// Poll even when the player screen is gone; only physical release frees the adapter slot.
pub fn drive(ps: &mut PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    let (session, mt) = pa.uri_split();
    let Some(session) = session else { return };
    let position_ms = session.advance(&mut |attempt, how| settle(ps, attempt, how), mt);
    ingest_position(ps, session.route_start_attempt(), position_ms);
    if session.ready_to_release() {
        if let Some(done) = pa.take_uri() { done.release(pa.mt()); }
    }
}

/// End a logical probe, preserving its slot until unregister is acknowledged.
pub(super) fn stop(ps: &mut PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    let (session, mt) = pa.uri_split();
    if let Some(session) = session {
        session.stop(&mut |attempt, how| settle(ps, attempt, how), mt);
    }
}

/// [`stop`] for a suite outside this crate that drives a session it built over a scripted
/// transport (`dev::uriprobe`'s tests); the product reaches it only through `begin_teardown` and
/// `exit_native`.
#[cfg(any(test, feature = "test-support"))]
pub fn stop_for_test(ps: &mut PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    stop(ps, pa);
}

/// The port, driven through the trait objects with scripted fakes: no play-uri, no transport. These
/// pin what media keeps (the route half): when a session is installed and refused, how a reply
/// settles the route start, which facts reach the attempt view, and when the slot is freed. What the
/// probe DOES on the bus is the application's implementation and its tests (`dev::uriprobe`).
#[cfg(test)]
mod port_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// What the fake session saw and what the test wants it to say next.
    #[derive(Default)]
    struct Script {
        opened: Vec<(String, bool, u64)>,
        advances: u32,
        stops: u32,
        released: u32,
        /// Settled by the next `advance`, then spent.
        settle_next: Option<ProbeSettle>,
        /// What the settle callback answered, in order.
        settled: Vec<bool>,
        position_ms: Option<i64>,
        ready: bool,
        closing: bool,
        plane_open: bool,
        quarantine: bool,
    }

    struct FakeSession { attempt: RouteStartAttempt, script: Arc<Mutex<Script>> }

    impl UriProbeSession for FakeSession {
        fn plane_open(&self) -> bool { self.script.lock().unwrap().plane_open }
        fn closing(&self) -> bool { self.script.lock().unwrap().closing }
        fn route_start_attempt(&self) -> RouteStartAttempt { self.attempt }
        fn acb_exit_quarantine(&self) -> bool { self.script.lock().unwrap().quarantine }
        fn ready_to_release(&self) -> bool { self.script.lock().unwrap().ready }
        fn advance(&mut self, settle: &mut dyn FnMut(RouteStartAttempt, ProbeSettle) -> bool, _mt: &MainThread) -> Option<i64> {
            let mut script = self.script.lock().unwrap();
            script.advances += 1;
            if let Some(how) = script.settle_next.take() {
                let answer = settle(self.attempt, how);
                script.settled.push(answer);
            }
            script.position_ms
        }
        fn stop(&mut self, _settle: &mut dyn FnMut(RouteStartAttempt, ProbeSettle) -> bool, _mt: &MainThread) {
            self.script.lock().unwrap().stops += 1;
        }
        fn release(self: Box<Self>, _mt: &MainThread) {
            self.script.lock().unwrap().released += 1;
        }
    }

    struct FakeLaunch { script: Arc<Mutex<Script>> }

    impl ProbeLaunch for FakeLaunch {
        fn open(self: Box<Self>, input: ProbeOpen<'_>, _mt: &MainThread) -> Box<dyn UriProbeSession> {
            self.script.lock().unwrap().opened.push((input.url.to_owned(), input.segmented_hls, input.release_allowance_ms));
            Box::new(FakeSession { attempt: input.attempt, script: self.script })
        }
    }

    /// Arms on a trigger text starting `fake`, like the application's reads the variant word.
    struct FakeProbe { script: Arc<Mutex<Script>> }

    impl UriProbe for FakeProbe {
        fn armed(&self, text: &str) -> Option<Box<dyn ProbeLaunch>> {
            text.starts_with("fake").then(|| Box::new(FakeLaunch { script: self.script.clone() }) as Box<dyn ProbeLaunch>)
        }
        fn native_slot_available(&self) -> bool { true }
    }

    const URL: &str = "https://example.invalid/library/parts/1/file.mkv?X-Plex-Token=SECRET";

    struct Rig {
        ps: PlaybackSession,
        pa: super::super::adapter::PlayerAdapter,
        script: Arc<Mutex<Script>>,
    }

    impl Rig {
        fn new() -> Rig {
            let ps = PlaybackSession::IDLE;
            crate::route::reset_player_control_for_test(&ps);
            let mut pa = super::super::adapter::PlayerAdapter::new_fed(unsafe { MainThread::assume() });
            let script = Arc::new(Mutex::new(Script::default()));
            pa.install_uri_probe(Box::new(FakeProbe { script: script.clone() }));
            Rig { ps, pa, script }
        }

        /// Prepare a route start and hand it to the probe the way the Feed start path does.
        fn start(&mut self) -> Result<RouteStartAttempt, RouteStartResult> {
            let ticket = crate::route::begin_route_start().expect("a start owner");
            assert!(crate::route::prepare_route_start(ticket));
            let launch = self.pa.uri_probe_armed("fake load").expect("the fake arms on `fake`");
            start(&mut self.ps, &mut self.pa, ticket, URL, launch)
        }
    }

    #[test]
    fn a_probe_installed_through_the_port_is_opened_driven_settled_and_released_by_the_route_half() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new();
        assert!(rig.pa.uri_probe_installed());
        assert!(rig.pa.uri_probe_armed("none of the variants").is_none(), "the probe, not media, reads the trigger text");
        assert!(!rig.pa.uri_live());

        let attempt = rig.start().expect("the probe starts");
        assert!(rig.pa.uri_live(), "the opened session sits in the adapter's slot");
        assert_eq!(rig.pa.uri_attempt(), Some(attempt));
        assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Pending);
        let opened = rig.script.lock().unwrap().opened.clone();
        assert_eq!(opened, [(URL.to_owned(), false, 10_000)], "media hands the launch the url, the stream kind and the release allowance");

        // One frame: the session settles the attempt through media's reducer and reports a position.
        {
            let mut script = rig.script.lock().unwrap();
            script.settle_next = Some(ProbeSettle::Started);
            script.position_ms = Some(1_200);
        }
        drive(&mut rig.ps, &mut rig.pa);
        assert_eq!(rig.script.lock().unwrap().advances, 1);
        assert_eq!(rig.script.lock().unwrap().settled, [true], "the first settle reaches the route reducer");
        assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Started);
        assert_eq!(rig.ps.attempt_view().facts().position_ns, Some(1_200_000_000), "the reported position is ingested as nanoseconds");
        assert!(rig.pa.uri_live(), "a session which is not ready to release keeps its slot");

        // A stop is passed through, and the slot is freed only on the frame the session is ready.
        stop(&mut rig.ps, &mut rig.pa);
        assert_eq!(rig.script.lock().unwrap().stops, 1);
        drive(&mut rig.ps, &mut rig.pa);
        assert!(rig.pa.uri_live() && rig.script.lock().unwrap().released == 0, "closing is not released");
        rig.script.lock().unwrap().ready = true;
        drive(&mut rig.ps, &mut rig.pa);
        assert!(!rig.pa.uri_live(), "a ready session leaves the slot");
        assert_eq!(rig.script.lock().unwrap().released, 1, "and is released exactly once");
        drive(&mut rig.ps, &mut rig.pa);
        assert_eq!(rig.script.lock().unwrap().advances, 3, "an empty slot is not driven");
    }

    #[test]
    fn a_failed_settle_fails_the_route_start() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new();
        let attempt = rig.start().expect("the probe starts");
        rig.script.lock().unwrap().settle_next = Some(ProbeSettle::StartFailed);
        drive(&mut rig.ps, &mut rig.pa);
        assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Failed);
        assert_eq!(rig.script.lock().unwrap().settled, [true]);
        stop(&mut rig.ps, &mut rig.pa);
        rig.script.lock().unwrap().settle_next = Some(ProbeSettle::Started);
        drive(&mut rig.ps, &mut rig.pa);
        assert_eq!(rig.script.lock().unwrap().settled, [true, false], "a settled attempt does not settle twice");
        assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Failed);
    }

    #[test]
    fn a_second_start_under_a_live_session_is_refused_and_opens_nothing() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new();
        // A session already in the slot, whose own start is over (the owner is free again).
        let attempt = claimed_attempt();
        assert!(crate::route::settle_route_start(&mut rig.ps, attempt, RouteStartResult::StartFailed));
        rig.pa.install_uri(Box::new(FakeSession { attempt, script: rig.script.clone() }));
        let ticket = crate::route::begin_route_start().expect("a start owner");
        assert!(crate::route::prepare_route_start(ticket));
        let launch = rig.pa.uri_probe_armed("fake").expect("armed");
        assert_eq!(start(&mut rig.ps, &mut rig.pa, ticket, URL, launch), Err(RouteStartResult::StartFailed));
        assert!(rig.script.lock().unwrap().opened.is_empty(), "a refused start opens nothing");
        let _ = crate::route::abort_route_start(ticket, RouteStartResult::StartFailed);
    }

    #[test]
    fn the_adapter_answers_the_session_questions_through_the_port() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new();
        assert!(!rig.pa.uri_plane_open() && !rig.pa.uri_closing() && !rig.pa.uri_acb_exit_quarantine(), "no session, no answers");
        rig.pa.install_uri(Box::new(FakeSession { attempt: claimed_attempt(), script: rig.script.clone() }));
        assert!(!rig.pa.uri_plane_open() && !rig.pa.uri_closing());
        {
            let mut script = rig.script.lock().unwrap();
            script.plane_open = true;
            script.closing = true;
            script.quarantine = true;
        }
        assert!(rig.pa.uri_plane_open() && rig.pa.uri_closing() && rig.pa.uri_acb_exit_quarantine());
        assert!(rig.pa.take_uri().is_some());
        assert!(!rig.pa.uri_live());
    }

    fn claimed_attempt() -> RouteStartAttempt {
        let ticket = crate::route::begin_route_start().expect("a start owner");
        assert!(crate::route::prepare_route_start(ticket));
        crate::route::claim_route_start_attempt(ticket).expect("an attempt")
    }
}
