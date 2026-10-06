//! **The rig `player::pump`'s flight tests drive.** A pump test lives in `player` (the pump and the
//! hostsim Engine fixtures are private there), but the route fixtures it needs — a loopback Plex
//! server that answers `/decision`, a playing transcode installed on it — are `route`'s own test
//! seams. This is the one door between them; it is `cfg(test)` only.
//!
//! The server holds each live `/decision` for `delay` before answering, so a test has a window in
//! which the flight's worker is known to be waiting on the network, and it logs every request line
//! BEFORE it writes the answer, so a client that has been answered can rely on the log. Any other
//! request but a stop is held for `delay` BEFORE it is logged (see the server loop).

use super::test_support::*;
use super::test_support::apply_plan;
use super::*;
use plx_plex::plex::serverinfo::Subscription;
use std::sync::atomic::Ordering;

type RequestLog = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// The refusal a `/decision` answers with while [`FlightRig::refuse_decisions`] has some owed
/// (`generalDecisionCode 2000`, the same body the route tests' `always_refusing_live` serves).
const DECISION_REFUSED: &[u8] = br#"{"MediaContainer":{"generalDecisionCode":2000,"transcodeDecisionCode":4020,"transcodeDecisionText":"synthetic refusal","Metadata":[]}}"#;

/// What the rig's playing route is.
#[derive(Clone, Copy)]
enum Shape {
    /// A progressive-MKV transcode, the shape of a remux or a re-encode.
    Progressive,
    /// A fixed-rung HLS transcode under an applied Auto contract: the route an automatic
    /// HLS-to-Original recovery leaves.
    Hls,
    /// A progressive-MKV Original (a remux) the Auto watchdog is watching, under an applied Auto
    /// contract: the route an automatic Original-to-HLS fallback leaves.
    AutoOriginal,
}

pub(crate) struct FlightRig {
    _serial: plx_base::testlock::Serial,
    done: std::sync::mpsc::Sender<()>,
    server: Option<std::thread::JoinHandle<()>>,
    log: RequestLog,
    /// How many of the next live `/decision` calls the server refuses.
    refusals: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl FlightRig {
    /// A playing progressive-MKV transcode (`rig-1`) on a fresh loopback server, installed into
    /// `ps`. Holds the crate-wide test serialization lock until dropped.
    pub(crate) fn start(ps: &mut PlaybackSession, delay: std::time::Duration) -> FlightRig {
        Self::start_shaped(ps, delay, Shape::Progressive)
    }

    /// [`FlightRig::start`] on a live fixed-rung HLS transcode (`rig-1`) with the quality preference
    /// at Auto, so an automatic HLS-to-Original recovery is allowed. The drop restores Original.
    pub(crate) fn start_hls(ps: &mut PlaybackSession, delay: std::time::Duration) -> FlightRig {
        Self::start_shaped(ps, delay, Shape::Hls)
    }

    /// [`FlightRig::start`] on an Original the Auto watchdog watches (`cur_auto_original_watched`),
    /// the quality preference at Auto, so an automatic Original-to-HLS fallback is allowed. The drop
    /// restores Original.
    pub(crate) fn start_auto_original(ps: &mut PlaybackSession, delay: std::time::Duration) -> FlightRig {
        Self::start_shaped(ps, delay, Shape::AutoOriginal)
    }

    fn start_shaped(ps: &mut PlaybackSession, delay: std::time::Duration, shape: Shape) -> FlightRig {
        let serial = fresh_registry(ps);
        assert!(plx_net::net::global_init() && crate::curlio::available());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port() as i32;
        listener.set_nonblocking(true).unwrap();
        let (done, stop) = std::sync::mpsc::channel();
        let log: RequestLog = Default::default();
        let shared = log.clone();
        let refusals: std::sync::Arc<std::sync::atomic::AtomicUsize> = Default::default();
        let owed = refusals.clone();
        let server = std::thread::spawn(move || loop {
            match plx_base::testnet::accept(&listener) {
                Ok((mut socket, _)) => {
                    let line = drain_http(&mut socket);
                    let probe = line.contains("/decision?") && line.contains("hasMDE=1");
                    let decision = line.contains("/decision?") && !probe;
                    // A selection PUT (or any other worker request but a stop) is logged only after
                    // `delay`, so a frame that returns within it sees an empty log whatever the
                    // machine's load. A `/decision` is logged at once and HELD instead, which is
                    // the window a test waits in; a stop is instant housekeeping, never what a
                    // frame is graded on.
                    if !probe && !decision && !line.contains("/stop?") {
                        std::thread::sleep(delay);
                    }
                    shared.lock().unwrap_or_else(|e| e.into_inner()).push(line.clone());
                    if probe {
                        write_json(&mut socket, MDE_DIRECTPLAY);
                    } else if decision {
                        std::thread::sleep(delay);
                        let refuse = owed
                            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                            .is_ok();
                        write_json(&mut socket, if refuse { DECISION_REFUSED } else { MDE_TRANSCODE_COPY });
                    } else {
                        write_json(&mut socket, EMPTY_MC);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if stop.try_recv().is_ok() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(e) => panic!("fixture accept: {e}"),
            }
        });
        let sid = plx_plex::plex::register_for_test("flight-rig", "127.0.0.1", port, "token", "flight-client");
        plx_plex::plex::client_for(sid).unwrap().set_link(plx_plex::plex::probe::Location::Local);
        plx_plex::plex::serverinfo::store_for_test(sid, Subscription::Yes, "1.43.4");
        let (url, contract) = match shape {
            Shape::Progressive => (
                format!("http://127.0.0.1:{port}/video/:/transcode/universal/start.mkv?session=rig-1"),
                enhanced_remux_contract(plx_plex::plex::AudioEnhancements::NONE, false),
            ),
            Shape::AutoOriginal => {
                restore_quality(Quality::Auto);
                (
                    format!("http://127.0.0.1:{port}/video/:/transcode/universal/start.mkv?session=rig-1"),
                    enhanced_remux_contract(plx_plex::plex::AudioEnhancements::NONE, false),
                )
            }
            Shape::Hls => {
                restore_quality(Quality::Auto);
                (
                    format!("http://127.0.0.1:{port}/video/:/transcode/universal/start.m3u8?session=rig-1"),
                    plx_plex::plex::EncodeContract {
                        delivery: plx_plex::plex::TranscodeDelivery::FixedHls { seconds_per_segment: 2 },
                        ceiling: Some(crate::abr::Rung::P1080High.ceiling()),
                        ..Default::default()
                    },
                )
            }
        };
        apply_plan(
            ps,
            Plan {
                sid,
                url,
                tsession: "rig-1".to_owned(),
                sess: "rig-logical".into(),
                part_id: 960001,
                vcodec: "hevc".into(),
                acodec: "ac3".into(),
                src_vcodec: "hevc".into(),
                src_acodec: "ac3".into(),
                contract,
                enhancement: EnhancementOutcome::Off,
                transport_kbps: 28_000,
                ..Default::default()
            },
            "960001",
        );
        if matches!(shape, Shape::AutoOriginal) {
            ps.cur_auto_original_watched = true;
            ps.cur_src = (28_000, 1_920, 1_080);
        }
        FlightRig { _serial: serial, done, server: Some(server), log, refusals }
    }

    /// The rung the Auto bootstrap chose for a source that never opens (what the unopened-source
    /// fallback rebuilds as HLS).
    pub(crate) fn set_bootstrap_rung(ps: &mut PlaybackSession, rung: crate::abr::Rung) {
        ps.auto_bootstrap_rung = Some(rung);
    }

    /// Offer the Original the playing route can be restored to. A REMUX candidate (`direct ==
    /// false`) makes the recovery's PMS half a real `/decision`, which is what a frame must not
    /// wait on.
    pub(crate) fn offer_original(ps: &mut PlaybackSession, direct: bool) {
        ps.auto_original = Some(AutoOriginalCandidate { direct, ..test_original_candidate(None) });
    }

    /// Arm the Original REMUX trial the viewer's Original pick leaves on the playing route: the
    /// recovery's three steps run in a row (the PMS half is a real `/decision` on the rig's server),
    /// so `PendingOriginal` holds the route as it stood (`rig-1`, the way back) and the reducer is
    /// `OriginalTrial(Prepared)` with the candidate's Load not yet claimed.
    pub(crate) fn arm_original_trial(ps: &mut PlaybackSession, offset_secs: i64) {
        Self::offer_original(ps, false);
        let reload = recover_auto_to_original_for(ps, &worker_ticket(), offset_secs, RecoveryCause::ManualOriginal);
        assert_eq!(reload, Some(AutoOriginalReload::Remux), "the rig's Original trial must arm");
        assert!(original_recovery_pending());
    }

    /// The trial's Load fails on open: the reducer is `OriginalTrial(Failed)`, its rollback still owed.
    pub(crate) fn fail_original_trial_load(ps: &mut PlaybackSession) {
        settle_pending_native_start(ps, RouteStartResult::StartFailed);
    }

    /// The OS suspends the app while the trial's Load awaits its first frame
    /// (`begin_engine_teardown(true)`, as the foreground lifecycle's suspend does): the frame proof
    /// belonged to the Engine destroyed, so the reducer is `OriginalTrial(Prepared)` again.
    pub(crate) fn suspend_in_original_trial(ps: &mut PlaybackSession) {
        settle_pending_native_start(ps, RouteStartResult::Started);
        begin_engine_teardown(true);
    }

    /// The viewer picked Original (and, with `displaced_pick`, a track pick's own reload merged
    /// under it): queue the claim exactly as the quality menu and the track menu do.
    pub(crate) fn queue_manual_original(ps: &PlaybackSession, displaced_pick: bool) {
        queue_user_route_intent(ps, UserRouteIntent::RecoverOriginal(RecoveryCause::ManualOriginal), displaced_pick);
    }

    /// The Auto worker proved the source fits and handed the pump an HLS-to-Original action,
    /// exactly as `ff.rs`'s `publish_original_recovery` does at `position_ns`.
    pub(crate) fn publish_hls_to_original(position_ns: i64) {
        let result = publish_automatic_route_intent(AutomaticRouteIntent::HlsToOriginal {
            ticket: worker_ticket(),
            evidence_kbps: 80_000,
            position_ns,
        });
        assert_eq!(result, AutomaticIntentResult::Accepted);
    }

    /// The Auto worker saw the Original starve and handed the pump an Original-to-HLS action,
    /// exactly as `ff.rs`'s watchdog does through `AutoOriginalWatch::request_hls_fallback`.
    pub(crate) fn publish_original_to_hls(conservative_kbps: u32, position_ns: i64) {
        let result = publish_automatic_route_intent(AutomaticRouteIntent::OriginalToHls {
            ticket: worker_ticket(),
            conservative_kbps,
            position_ns,
        });
        assert_eq!(result, AutomaticIntentResult::Accepted);
    }

    /// A newer play request arrives while a cold resume's flight is outstanding: the one reducer
    /// step every `request_play*` begins with. `true` when it was admitted (the flight is superseded).
    pub(crate) fn begin_newer_play_request() -> bool {
        begin_playback_request()
    }

    /// Refuse the next `n` live `/decision` calls (answered after the delay like any other).
    pub(crate) fn refuse_decisions(&self, n: usize) {
        self.refusals.store(n, Ordering::SeqCst);
    }

    /// Every request line the server has seen, in order.
    pub(crate) fn requests(&self) -> Vec<String> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// How many requests other than encoder stops the server has been asked: what a frame is
    /// graded on, since a stop is queued by an earlier flight's housekeeping on its own worker.
    pub(crate) fn asked(&self) -> usize {
        self.requests().iter().filter(|r| !r.contains("/video/:/transcode/universal/stop")).count()
    }

    /// How many live `/decision` calls (not the `hasMDE=1` probe) the server has been asked.
    pub(crate) fn decisions(&self) -> usize {
        self.requests().iter().filter(|r| r.contains("/decision?") && !r.contains("hasMDE=1")).count()
    }

    /// The transcode session ids the server has been asked to stop.
    pub(crate) fn stopped(&self) -> Vec<String> {
        self.requests()
            .iter()
            .filter(|r| r.contains("/video/:/transcode/universal/stop"))
            .filter_map(|r| query_param(r, "session").map(str::to_owned))
            .collect()
    }

    /// Block (bounded) until `ready` holds over the request log.
    pub(crate) fn wait_for(&self, what: &str, ready: impl Fn(&FlightRig) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready(self) {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}

impl Drop for FlightRig {
    fn drop(&mut self) {
        // The flight's worker (and any encoder stop it queued) finishes against THIS rig's server
        // before the server goes away and the lock is released.
        plx_base::task::drain_workers_for_test();
        let _ = self.done.send(());
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
        plx_plex::plex::reset_servers_for_test();
        // A landing a failed test left in the mailbox belongs to no later test.
        drop(take_claim_landing());
        crate::player::claim_hold::clear();
        // The HLS and Auto-Original shapes set the quality preference; the lock is still held (fields drop after this).
        restore_quality(Quality::Original);
    }
}
