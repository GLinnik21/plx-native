//! `StallGuard` arming and abort decisions for a server-paced HLS transfer.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// A completed PMS HLS object is not necessarily delivered at a stationary rate.  The server
/// reports `segmentWait` inside downloads while its JIT encoder catches up, then sends the
/// already-sized remainder as a burst.  On the television the first 214 440-byte prefix of a
/// roughly 6 MB 4K object took about 258 ms, while the adjacent complete objects landed in
/// 1.2--1.4 s.  Extrapolating that prefix over the unseen remainder predicts more than the
/// available 6.4 s reserve and aborts a stream whose complete acquisitions are sustainable.
///
/// The prefix is right-censored evidence: without a model of PMS's future production it can
/// prove only what has already been spent, not how long unseen bytes will take.  Keeping the
/// playhead fixed makes this differential: the broken attained-rate forecast fires; the
/// physical reserve has spent nothing and therefore cannot.
#[test]
fn a_server_paced_prefix_cannot_forecast_its_unseen_remainder() {
    let _serial = crate::testlock::serial();
    let pos = 5_000_000_000i64;
    crate::player::SHARED
        .playpos_ns
        .store(pos, std::sync::atomic::Ordering::Relaxed);
    let guard = StallGuard::arm(6_376).expect("a live reserve arms");
    assert!(
        !guard.should_abort(false, false),
        "a censored JIT prefix cannot prove that the complete response will miss the reserve",
    );
}

/// **But a reserve that was empty when the fetch STARTED never arms the guard at all**, which
/// is a different question and is the one that matters: that is every session's first segment,
/// and arming there aborts the fetch that would have created the picture. Device-measured
/// without this gate: `stall abort seq=0 ... of 0ms reserve`, playback never started, the video
/// plane never bound.
#[test]
fn an_empty_reserve_at_the_start_of_a_fetch_arms_nothing() {
    assert!(
        StallGuard::arm(0).is_none(),
        "so the guard must refuse to be armed"
    );
    assert!(StallGuard::arm(-1).is_none());
    assert!(
        StallGuard::arm(1).is_some(),
        "and must arm as soon as there is a picture to keep"
    );
}

#[test]
fn the_floor_guard_holds_the_clock_without_abandoning_the_only_rung() {
    let hold = StallGuard::arm_clock_hold(2_000).expect("a live floor reserve arms");
    assert!(
        !hold.aborts_fetch(),
        "the floor has no cheaper object to re-fetch"
    );
    assert!(
        StallGuard::arm(2_000)
            .expect("a live non-floor reserve arms")
            .aborts_fetch(),
        "above the floor the same signal must still release the controller to downshift",
    );
}

#[test]
fn an_existing_terminal_hold_arms_no_second_abort() {
    assert!(
        arm_active_stall_guard(Some(2_000), false, true).is_none(),
        "the response rebuilding an empty reserve must be allowed to complete",
    );
    assert!(
        arm_active_stall_guard(Some(2_000), false, false).is_some(),
        "a running clock still needs its terminal boundary",
    );
}

/// The reserve is spent by the PLAYHEAD, not by wall time. A server may wait arbitrarily while
/// playback is paused or re-priming without consuming one millisecond of queued media.
#[test]
fn a_fetch_the_playhead_is_not_consuming_never_aborts() {
    let _serial = crate::testlock::serial();
    let pos = 5_000_000_000i64;
    crate::player::SHARED
        .playpos_ns
        .store(pos, std::sync::atomic::Ordering::Relaxed);
    let guard = StallGuard {
        reserve_ms_at_start: 2_000,
        playhead_at_start_ns: pos,
        hold_epoch_at_arm: SHARED
            .hls_internal_hold_epoch
            .load(std::sync::atomic::Ordering::Acquire),
        action: super::StallAction::AbortFetch,
    };
    assert!(
        !guard.should_abort(false, false),
        "the guard abandoned a fetch while the picture was not consuming its reserve"
    );
}

/// The other half, so the fix cannot be "never abort": a playhead that IS advancing spends the
/// reserve exactly as before, and a fetch that provably cannot land still aborts.
#[test]
fn a_fetch_the_playhead_is_consuming_still_aborts() {
    let _serial = crate::testlock::serial();
    let pos = 5_000_000_000i64;
    let guard = StallGuard {
        reserve_ms_at_start: 2_000,
        playhead_at_start_ns: pos,
        hold_epoch_at_arm: SHARED
            .hls_internal_hold_epoch
            .load(std::sync::atomic::Ordering::Acquire),
        action: super::StallAction::AbortFetch,
    };
    crate::player::SHARED
        .playpos_ns
        .store(pos + 1_999_000_000, std::sync::atomic::Ordering::Relaxed);
    assert!(
        !guard.should_abort(false, false),
        "one millisecond of reserve remains"
    );
    // Exactly the two seconds of media present at the boundary have now been consumed.
    crate::player::SHARED
        .playpos_ns
        .store(pos + 2_000_000_000, std::sync::atomic::Ordering::Relaxed);
    assert!(
        guard.should_abort(false, false),
        "a genuinely exhausted reserve must still abort"
    );
}

/// The same-encoder lookahead used to not observe terminal holds like the ordinary fetch it
/// shadows. Device-measured `pipe_abr_down_collapse`: the link collapsed from 40 Mbps to
/// 500 kbps while on rung 20000; segment 11 (5.4 MB) then ran 86.8 s and froze the picture,
/// because `hls_prefetch_same_encoder` used to build its AVIO policy with no stall guard at all
/// (`ReserveDeadlineState::new(None, false), None` — ff.rs, the same-encoder lookahead call to
/// `hls_demux_segment`), while the ordinary branch armed one via `arm_active_stall_guard` for
/// the identical active cursor. This test now pins that both policies abort under an identical
/// terminal hold.
#[test]
fn the_lookahead_policy_must_abort_under_a_terminal_hold_like_the_ordinary_policy() {
    use std::io::{Read, Write};

    let _serial = crate::testlock::serial();
    crate::player::SHARED
        .playpos_ns
        .store(5_000_000_000, std::sync::atomic::Ordering::Relaxed);

    // Drives `read_cb` twice against a fresh 8-byte loopback body, four bytes per read, with the
    // shared terminal-hold flag published between the two reads (bytes 5..8 still remain, so an
    // armed guard has something to abort). Returns whether the SECOND read reported the
    // production stall-abort outcome (`AVERROR_EOF` + `stall_aborted`), i.e. whether `policy`
    // actually protected this fetch.
    fn drive_two_reads_across_a_terminal_hold(policy: SegmentAcquisition) -> bool {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback server");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut request = Vec::new();
            let mut chunk = [0u8; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let n = socket.read(&mut chunk).expect("read request");
                if n == 0 {
                    return;
                }
                request.extend_from_slice(&chunk[..n]);
            }
            // The whole body is available immediately: this test controls the terminal hold
            // itself, between two synchronous `read_cb` calls, rather than a server pause.
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCDEFGH")
                .expect("headers+body");
            socket.flush().expect("flush");
        });

        SHARED
            .hls_rebuffering
            .store(false, std::sync::atomic::Ordering::Release);
        let host = CString::new("127.0.0.1").unwrap();
        let path = CString::new("/segment.ts").unwrap();
        let mut hs = crate::stream::http_stream_boxed();
        assert_eq!(
            crate::stream::http_open(
                &mut *hs,
                host.as_ptr(),
                port as c_int,
                path.as_ptr(),
                std::ptr::null(),
                "GET",
            ),
            0,
        );
        let mut aq = crate::aq::aq_new(1 << 20);
        let mut state = avio_state_for(
            Src::Socket {
                hs: &mut *hs,
                host,
                port: port as c_int,
                path,
            },
            &mut *aq,
            8,
            policy,
        );
        let op = &mut state as *mut AvioState as *mut c_void;
        let mut dst = [0u8; 4];

        // First 4 bytes: reserve not yet held, guard (if any) does not fire.
        let first = read_cb(op, dst.as_mut_ptr(), dst.len() as c_int);
        assert_eq!(first, 4, "fixture: the first 4 bytes must arrive cleanly");

        // The link collapses; the main thread publishes the terminal hold. 4 of 8 bytes remain.
        SHARED
            .hls_rebuffering
            .store(true, std::sync::atomic::Ordering::Release);
        let second = read_cb(op, dst.as_mut_ptr(), dst.len() as c_int);
        let aborted = second == AVERROR_EOF && avio_stall_aborted(&state);
        drop(state);

        SHARED
            .hls_rebuffering
            .store(false, std::sync::atomic::Ordering::Release);
        crate::stream::http_close(&mut *hs);
        crate::aq::aq_destroy(&mut *aq);
        server.join().expect("loopback server");
        aborted
    }

    let ordinary_policy = SegmentAcquisition::for_test(Some(6_000), false, false);
    assert_eq!(
        ordinary_policy.stall(),
        arm_active_stall_guard(Some(6_000), false, false),
        "fixture: the acquisition arms exactly the ordinary branch's guard"
    );
    assert!(
        drive_two_reads_across_a_terminal_hold(ordinary_policy),
        "sanity: the ordinary branch's own policy must abort under its own terminal hold",
    );

    // The exact evaluation `SegmentAcquisition::for_cursor` now runs for an active-cursor fetch
    // (via the test-only `for_test` escape hatch, which is `active` under a name `ff` cannot
    // reach — see `ff_acquisition.rs`) — not a hand-built guard, the same evaluation the
    // production constructor runs for both the ordinary fetch and the lookahead.
    let lookahead_policy = SegmentAcquisition::for_test(Some(6_000), false, false);
    assert!(
        drive_two_reads_across_a_terminal_hold(lookahead_policy),
        "the same-encoder lookahead must abort under a terminal hold exactly like the ordinary \
         fetch it shadows — reading straight through it is how `pipe_abr_down_collapse` froze \
         the picture for ~84s on segment 11",
    );
}

/// The `Active` role's evaluation must preserve every legitimate unarmed outcome
/// `arm_active_stall_guard` already defined — an unknown reserve, a reserve of zero at the start
/// of a fetch, and an already-held clock all arm nothing, and only as this evaluation's own
/// answer, never a call site's shortcut.
#[test]
fn the_active_acquisition_constructor_preserves_arm_active_stall_guards_exclusions() {
    assert!(
        SegmentAcquisition::for_test(None, false, false)
            .stall()
            .is_none(),
        "an unknowable reserve arms nothing"
    );
    assert!(
        SegmentAcquisition::for_test(Some(0), false, false)
            .stall()
            .is_none(),
        "a reserve of zero at the start of a fetch arms nothing"
    );
    assert!(
        SegmentAcquisition::for_test(Some(2_000), false, true)
            .stall()
            .is_none(),
        "an already-held clock arms no second abort"
    );
    assert!(
        SegmentAcquisition::for_test(Some(2_000), false, false)
            .stall()
            .is_some(),
        "a live reserve with no existing hold must still arm"
    );
}

/// A non-adaptive playback (`controller: None`) has no ladder to abandon a rung on, so
/// `SegmentAcquisition::for_cursor` must hand back an acquisition with no armed guard — the
/// `Fixed` role, reachable only through this same constructor.
#[test]
fn for_cursor_with_no_controller_is_unarmed() {
    assert!(
        SegmentAcquisition::for_cursor(None).stall().is_none(),
        "non-adaptive playback must never arm a stall guard"
    );
}

/// **The regression test for the actual bug: does `hls_prefetch_same_encoder`'s WIRING pass an
/// armed acquisition to `hls_demux_segment`, and does a `StallAbort` from that call come back as
/// the shared reducer's abandoned round — not `Ok(None)`, not a second requeue?**
///
/// `pipe_abr_down_collapse` was not a `StallGuard` defect (that logic was always correct and
/// already covered above) — it was `hls_prefetch_same_encoder` never being FORCED to arm one at
/// all, and a hand-built `AvioState` test cannot see that: it builds the `SegmentAcquisition`
/// itself and never touches the lookahead's own control flow. This drives
/// `hls_prefetch_same_encoder_with` — the exact generic control flow `hls_prefetch_same_encoder`
/// wraps over the real `hls_demux_segment` — with a fake demux that reports whether the
/// acquisition it received was armed and manufactures the terminal-hold outcome a real AVIO
/// `read_cb` would have produced for an armed guard under a published hold.
#[test]
fn the_lookahead_wiring_aborts_through_the_shared_reducer_when_the_guard_is_armed() {
    let _serial = crate::testlock::serial();

    // Live inputs `SegmentAcquisition::for_cursor` samples for ITSELF — a real reserve, no
    // existing hold — so it is the constructor's own evaluation, not a test-supplied bool, that
    // decides whether the guard arms.
    crate::player::SHARED
        .playpos_ns
        .store(0, std::sync::atomic::Ordering::Relaxed);
    SHARED
        .hls_video_tail_ns
        .store(6_000_000_000, std::sync::atomic::Ordering::Release);
    SHARED
        .hls_audio_tail_ns
        .store(-1, std::sync::atomic::Ordering::Release);
    SHARED
        .disp_base
        .store(0, std::sync::atomic::Ordering::Relaxed);
    SHARED
        .hls_rebuffering
        .store(false, std::sync::atomic::Ordering::Release);

    let catalog = crate::abr::HlsActuatorCatalog::measured();
    let controller = crate::abr::Controller::starting_at(crate::abr::Rung::P1080M18, None, catalog);
    assert!(
        !controller.current().at_floor(),
        "fixture: the chosen rung must not be the ladder floor, or the guard would only hold \
         the clock instead of aborting the fetch"
    );

    let master = crate::hls::Resource {
        origin: crate::plex::Origin::http("127.0.0.1", 32400),
        path: "/master.m3u8?X-Plex-Token=test-token".to_string(),
    };
    let auth = crate::hls::InheritedAuth::capture(&master).expect("fixture token pair");
    let mut cursor = HlsCursor {
        publishes_duration: true,
        declared_bps: 0,
        auth,
        media: master,
        tracker: Default::default(),
        pending: std::collections::VecDeque::new(),
        ended: false,
        target_duration_secs: 6,
        start_applied: true,
    };
    let n1 = crate::hls::Segment {
        sequence: 11,
        duration: std::time::Duration::from_secs(6),
        resource: cursor.media.clone(),
    };

    let mut observed_armed = None;
    let result = hls_prefetch_same_encoder_with(
        &mut cursor,
        n1.clone(),
        crate::hls::SegmentTimeline::default(),
        crate::hls::SegmentTimeline::default()
            .begin(std::time::Duration::from_secs(0))
            .expect("fixture clock"),
        Some(&controller),
        |_segment, _clock, acquisition| {
            let armed = acquisition.stall().is_some();
            observed_armed = Some(armed);
            if armed {
                Err(HlsExit::StallAbort(SegmentTransfer {
                    bytes: 1_448,
                    active_us: 84_000_000,
                    total_us: 86_800_000,
                    audio_expected: true,
                }))
            } else {
                Ok(HlsSegmentOutput {
                    aus: Vec::new(),
                    transfer: SegmentTransfer {
                        bytes: 5_400_000,
                        active_us: 1_000_000,
                        total_us: 1_000_000,
                        audio_expected: true,
                    },
                    video_width: 1_920,
                    video_height: 1_080,
                    video_tail_ns: 6_000_000_000,
                    audio_tail_ns: None,
                })
            }
        },
    );

    assert_eq!(
        observed_armed,
        Some(true),
        "an adaptive, off-floor context must arm the guard the fake demux was handed — this is \
         the exact wiring `pipe_abr_down_collapse` found broken"
    );
    let (_segment, output, _clock, fetch_abandoned) = result
        .expect("hls_prefetch_same_encoder_with must not propagate StallAbort via `?`")
        .expect("a StallAbort must still produce a round, never Ok(None)");
    assert!(
        fetch_abandoned,
        "a StallAbort must be reported as an abandoned round, not a completed prefetch"
    );
    assert!(
        output.aus.is_empty(),
        "an abandoned round must carry zero access units"
    );
    assert_eq!(
        cursor.pending.len(),
        1,
        "the segment must be requeued EXACTLY ONCE"
    );
    assert_eq!(cursor.pending[0].sequence, 11);
}

/// The main thread's B=0 hold and the worker's playhead sample are the same physical boundary.
/// The explicit signal covers millisecond quantisation and a callback that resumes just after
/// Starfish was paused.
#[test]
fn a_terminal_hold_aborts_only_an_incomplete_response() {
    let _serial = crate::testlock::serial();
    crate::player::SHARED
        .playpos_ns
        .store(5_000_000_000, std::sync::atomic::Ordering::Relaxed);
    let guard = StallGuard::arm(6_000).expect("a live reserve arms");
    assert!(
        guard.should_abort(false, true),
        "B=0 with bytes remaining is terminal"
    );
    assert!(
        !guard.should_abort(true, true),
        "a complete response must be credited"
    );
}

// -- every blocking leg of an ACTIVE acquisition consults the guard ---------------------------
//
// The tests above drive the guard where it was first evaluated: the top of an AVIO `read_cb`.
// These drive the legs that used to precede or outlast that evaluation — the HTTP open waiting
// for headers, the `NotReady` retry wait, and a body read already blocked when the boundary
// arrives — through the real `hls_demux_segment`/`read_cb` against a scripted loopback PMS.
// Synchronisation is causal: the server reports each request it has read, and the transport's
// own `Pacer::before_wait` reports (via `checkpoint::observe`) that a read is about to block.
// A test that the fix does not rescue is bounded by a teardown (AU abort + socket shutdown,
// exactly what `engine::teardown` does), so a red run fails with a message, never a hang.

/// What the scripted PMS does with request number `n` (0-based, across connections).
#[derive(Clone, Copy)]
enum Reply {
    /// Read the request, send nothing, hold the connection open until the test ends.
    Withhold,
    /// Answer immediately; the connection stays open for keep-alive.
    Send(&'static [u8]),
    /// Answer with a prefix, then hold the connection open until the test ends.
    SendThenWithhold(&'static [u8]),
    /// Hold the headers until [`ScriptedPms::release`], then answer.
    AfterRelease(&'static [u8]),
}

struct ScriptedPms {
    port: u16,
    accepts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Each request's index, reported once the server has read it (and, for `Send`, answered).
    seen: Option<std::sync::mpsc::Receiver<usize>>,
    released: std::sync::Arc<AtomicBool>,
    stop: std::sync::Arc<AtomicBool>,
    acceptor: Option<std::thread::JoinHandle<()>>,
}

impl ScriptedPms {
    fn start(script: fn(usize) -> Reply) -> ScriptedPms {
        use std::io::{Read, Write};
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback PMS");
        let port = listener.local_addr().unwrap().port();
        listener
            .set_nonblocking(true)
            .expect("nonblocking acceptor");
        let accepts = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen_tx, seen) = std::sync::mpsc::sync_channel::<usize>(64);
        let (a, r, rel, st) = (
            accepts.clone(),
            requests.clone(),
            released.clone(),
            stop.clone(),
        );
        let acceptor = std::thread::spawn(move || {
            let mut handlers = Vec::new();
            while !st.load(Ordering::Acquire) {
                let socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        continue;
                    }
                    Err(_) => break,
                };
                a.fetch_add(1, Ordering::AcqRel);
                let (r, rel, st, seen_tx) = (r.clone(), rel.clone(), st.clone(), seen_tx.clone());
                handlers.push(std::thread::spawn(move || {
                    let _ = socket.set_nonblocking(false);
                    let _ = socket.set_read_timeout(Some(std::time::Duration::from_millis(5)));
                    let mut w = match socket.try_clone() {
                        Ok(w) => w,
                        Err(_) => return,
                    };
                    let park = |until: &dyn Fn() -> bool| {
                        while !until() && !st.load(Ordering::Acquire) {
                            std::thread::sleep(std::time::Duration::from_millis(2));
                        }
                    };
                    let mut buf = Vec::new();
                    loop {
                        if st.load(Ordering::Acquire) {
                            return;
                        }
                        if let Some(k) = buf.windows(4).position(|x| x == b"\r\n\r\n") {
                            buf.drain(..k + 4);
                            let n = r.fetch_add(1, Ordering::AcqRel);
                            match script(n) {
                                Reply::Send(bytes) => {
                                    let _ = w.write_all(bytes);
                                    let _ = w.flush();
                                    let _ = seen_tx.send(n);
                                }
                                Reply::SendThenWithhold(bytes) => {
                                    let _ = w.write_all(bytes);
                                    let _ = w.flush();
                                    let _ = seen_tx.send(n);
                                    park(&|| false);
                                    return;
                                }
                                Reply::Withhold => {
                                    let _ = seen_tx.send(n);
                                    park(&|| false);
                                    return;
                                }
                                Reply::AfterRelease(bytes) => {
                                    let _ = seen_tx.send(n);
                                    park(&|| rel.load(Ordering::Acquire));
                                    let _ = w.write_all(bytes);
                                    let _ = w.flush();
                                }
                            }
                            continue;
                        }
                        let mut tmp = [0u8; 1024];
                        match (&socket).read(&mut tmp) {
                            Ok(0) => return,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                            Err(ref e)
                                if matches!(
                                    e.kind(),
                                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                                ) => {}
                            Err(_) => return,
                        }
                    }
                }));
            }
            for handler in handlers {
                let _ = handler.join();
            }
        });
        ScriptedPms {
            port,
            accepts,
            requests,
            seen: Some(seen),
            released,
            stop,
            acceptor: Some(acceptor),
        }
    }

    fn release(&self) {
        self.released.store(true, Ordering::Release);
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::Acquire)
    }

    fn accepts(&self) -> usize {
        self.accepts.load(Ordering::Acquire)
    }
}

impl Drop for ScriptedPms {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
    }
}

/// Restores every `SHARED` fact these tests publish, however the test leaves.
struct SharedHoldReset;

impl SharedHoldReset {
    fn at(playpos_ns: i64) -> SharedHoldReset {
        crate::player::SHARED
            .playpos_ns
            .store(playpos_ns, Ordering::Relaxed);
        SHARED.hls_rebuffering.store(false, Ordering::Release);
        SHARED
            .hls_rebuffer_requested
            .store(false, Ordering::Release);
        SharedHoldReset
    }
}

impl Drop for SharedHoldReset {
    fn drop(&mut self) {
        crate::checkpoint::observe::unwatch();
        SHARED.hls_rebuffering.store(false, Ordering::Release);
        SHARED
            .hls_rebuffer_requested
            .store(false, Ordering::Release);
    }
}

/// The main thread accepting its internal rebuffer hold: `hls_rebuffering` plus a new epoch.
fn publish_accepted_hold() {
    SHARED.hls_rebuffering.store(true, Ordering::Release);
    SHARED
        .hls_internal_hold_epoch
        .fetch_add(1, Ordering::AcqRel);
}

const PLAYHEAD_NS: i64 = 5_000_000_000;
/// How long a red run may stay blocked before the test tears it down. Far below every transport
/// bound the fix could hide behind (15 s media inactivity, the 3-15 s NotReady budget).
const TEARDOWN_AFTER: std::time::Duration = std::time::Duration::from_secs(3);
/// How promptly an armed boundary must end a blocked leg: a few 100 ms checkpoint slices.
const PROMPT: std::time::Duration = std::time::Duration::from_millis(1_500);

fn segment_on(port: u16) -> (crate::hls::Segment, crate::hls::InheritedAuth) {
    let origin = crate::plex::Origin::http("127.0.0.1", i32::from(port));
    let master = crate::hls::Resource {
        origin: origin.clone(),
        path: "/video/:/transcode/universal/session/t/base/index.m3u8?X-Plex-Token=test-token"
            .to_string(),
    };
    let auth = crate::hls::InheritedAuth::capture(&master).expect("fixture token pair");
    let segment = crate::hls::Segment {
        sequence: 7,
        duration: std::time::Duration::from_secs(2),
        resource: crate::hls::Resource {
            origin,
            path: "/video/:/transcode/universal/session/t/base/00007.ts".to_string(),
        },
    };
    (segment, auth)
}

struct DemuxRun {
    result: Result<HlsSegmentOutput, HlsExit>,
    /// From the trigger to `hls_demux_segment` returning; `None` if the server never saw a GET.
    after_trigger: Option<std::time::Duration>,
    /// The test had to tear the fetch down: nothing but teardown ended it.
    torn_down: bool,
}

fn describe(result: &Result<HlsSegmentOutput, HlsExit>) -> String {
    match result {
        Ok(output) => format!(
            "Ok({} AUs, {} bytes)",
            output.aus.len(),
            output.transfer.bytes
        ),
        Err(error) => format!("Err({error:?})"),
    }
}

/// Run the REAL `hls_demux_segment` against `pms`, firing `trigger` once the server has read the
/// first GET. A fetch still running [`TEARDOWN_AFTER`] later is torn down.
fn demux_against(
    pms: &mut ScriptedPms,
    acquisition: SegmentAcquisition,
    trigger: impl FnOnce() + Send + 'static,
) -> DemuxRun {
    let (segment, auth) = segment_on(pms.port);
    let mut hs = crate::stream::http_stream_boxed();
    let mut aq = crate::aq::aq_new(1 << 20);
    let (hs_addr, aq_addr) = (
        &mut *hs as *mut HttpStream as usize,
        &mut *aq as *mut AuQueue as usize,
    );
    let seen = pms.seen.take().expect("one run per server");
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let publisher = std::thread::spawn(move || {
        let saw_get = seen.recv_timeout(std::time::Duration::from_secs(5)).is_ok();
        if saw_get {
            trigger();
        }
        let fired_at = std::time::Instant::now();
        let torn_down = done_rx.recv_timeout(TEARDOWN_AFTER).is_err();
        if torn_down {
            crate::aq::aq_abort(aq_addr as *mut AuQueue);
            crate::stream::http_shutdown(hs_addr as *mut HttpStream);
        }
        (saw_get.then_some(fired_at), torn_down, seen)
    });
    let mut clock = crate::hls::SegmentTimeline::default()
        .begin(segment.duration)
        .expect("fixture clock");
    let mut net = HlsNet {
        hs: &mut *hs,
        curl: None,
    };
    let result = unsafe {
        hls_demux_segment(
            &segment,
            &auth,
            &mut clock,
            &mut *aq,
            &mut net,
            "aac",
            acquisition,
        )
    };
    let returned_at = std::time::Instant::now();
    let _ = done_tx.send(());
    let (fired_at, torn_down, seen) = publisher.join().expect("publisher");
    pms.seen = Some(seen);
    crate::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
    DemuxRun {
        result,
        after_trigger: fired_at.map(|at| returned_at.saturating_duration_since(at)),
        torn_down,
    }
}

fn assert_prompt_zero_byte_stall_abort(run: &DemuxRun, leg: &str) {
    assert!(
        !run.torn_down,
        "{leg}: nothing but teardown ended the fetch ({} after the boundary) — the guard was never \
         consulted while it blocked; result {}",
        run.after_trigger.map_or("never".into(), |d| format!("{d:?}")),
        describe(&run.result),
    );
    match &run.result {
        Err(HlsExit::StallAbort(transfer)) => {
            assert_eq!(transfer.bytes, 0, "{leg}: no body byte existed");
            assert_eq!(transfer.active_us, 0, "{leg}: no body read ran");
            assert!(
                transfer.total_us >= 1,
                "{leg}: the acquisition's elapsed time is kept"
            );
        }
        other => panic!(
            "{leg}: expected a zero-byte StallAbort, got {}",
            describe(other)
        ),
    }
    let after = run.after_trigger.expect("the server saw the GET");
    assert!(
        after < PROMPT,
        "{leg}: the abort took {after:?} after the boundary"
    );
}

/// (a) A server that accepts the GET and withholds its headers, on the active rung above the
/// floor, with reserve still remaining when the fetch began. The main thread then accepts its
/// B=0 hold. Before this change the open leg never consulted the guard and blocked to the 15 s
/// transport watchdog with the picture frozen.
#[test]
fn a_hold_accepted_while_the_open_waits_for_headers_abandons_the_fetch() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let mut pms = ScriptedPms::start(|_| Reply::Withhold);
    let acquisition = SegmentAcquisition::for_test(Some(6_000), false, false);
    let run = demux_against(&mut pms, acquisition, publish_accepted_hold);
    assert_prompt_zero_byte_stall_abort(&run, "open, accepted hold");
}

/// (b) The same open leg with no hold at all: the playhead alone spends the whole reserve the
/// fetch started with.
#[test]
fn a_playhead_that_spends_the_reserve_during_the_open_abandons_the_fetch() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let mut pms = ScriptedPms::start(|_| Reply::Withhold);
    let acquisition = SegmentAcquisition::for_test(Some(2_000), false, false);
    fn spend_the_reserve() {
        crate::player::SHARED
            .playpos_ns
            .store(PLAYHEAD_NS + 2_000_000_000, Ordering::Relaxed);
    }
    let run = demux_against(&mut pms, acquisition, spend_the_reserve);
    assert_prompt_zero_byte_stall_abort(&run, "open, playhead spend");
    assert!(
        SHARED.hls_rebuffer_requested.load(Ordering::Acquire),
        "the boundary still asks the main thread to hold the clock"
    );
}

/// (c) The `NotReady` retry wait: PMS answers 404 (segment not produced yet), the main thread
/// accepts its hold while the fetch waits to retry. The fetch must end promptly and must not go
/// back to the server for the object it has just abandoned.
#[test]
fn a_hold_accepted_during_the_not_ready_wait_abandons_the_fetch_without_another_get() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let mut pms =
        ScriptedPms::start(|_| Reply::Send(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"));
    let acquisition = SegmentAcquisition::for_test(Some(6_000), false, false);
    let run = demux_against(&mut pms, acquisition, publish_accepted_hold);
    let requests = pms.requests();
    assert_prompt_zero_byte_stall_abort(&run, "NotReady wait");
    assert_eq!(requests, 1, "no GET may follow the hold");
}

/// (d) A body read already BLOCKED inside `read_cb` when the hold is accepted: that same
/// invocation must return with the abort latched, not wait for bytes or the watchdog.
#[test]
fn a_hold_accepted_during_a_blocked_body_read_ends_that_read() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let pms = ScriptedPms::start(|_| {
        Reply::SendThenWithhold(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCD")
    });
    let host = CString::new("127.0.0.1").unwrap();
    let path = CString::new("/segment.ts").unwrap();
    let mut hs = crate::stream::http_stream_boxed();
    assert_eq!(
        crate::stream::http_open(
            &mut *hs,
            host.as_ptr(),
            pms.port as c_int,
            path.as_ptr(),
            std::ptr::null(),
            "GET"
        ),
        0,
    );
    let mut aq = crate::aq::aq_new(1 << 20);
    let (hs_addr, aq_addr) = (
        &mut *hs as *mut HttpStream as usize,
        &mut *aq as *mut AuQueue as usize,
    );
    let src = Src::Socket {
        hs: &mut *hs,
        host,
        port: pms.port as c_int,
        path,
    };
    let mut state = avio_state_for(
        src,
        &mut *aq,
        8,
        SegmentAcquisition::for_test(Some(6_000), false, false),
    );
    let op = &mut state as *mut AvioState as *mut c_void;
    let mut dst = [0u8; 4];
    assert_eq!(
        read_cb(op, dst.as_mut_ptr(), 4),
        4,
        "fixture: the prefix arrives"
    );

    let (wait_tx, waits) = std::sync::mpsc::sync_channel::<()>(64);
    crate::checkpoint::observe::watch(std::thread::current().id(), wait_tx);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let publisher = std::thread::spawn(move || {
        // The second read is now inside the transport, about to block for bytes 5..8.
        let blocked = waits
            .recv_timeout(std::time::Duration::from_secs(5))
            .is_ok();
        if blocked {
            publish_accepted_hold();
        }
        let torn_down = done_rx.recv_timeout(TEARDOWN_AFTER).is_err();
        if torn_down {
            crate::aq::aq_abort(aq_addr as *mut AuQueue);
            crate::stream::http_shutdown(hs_addr as *mut HttpStream);
        }
        (blocked, torn_down)
    });
    let second = read_cb(op, dst.as_mut_ptr(), 4);
    let _ = done_tx.send(());
    let (blocked, torn_down) = publisher.join().expect("publisher");
    crate::checkpoint::observe::unwatch();
    let latched = avio_stall_aborted(&state);
    drop(state);
    crate::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
    drop(pms);

    assert!(
        blocked,
        "fixture: the second read must reach a blocking wait"
    );
    assert!(
        !torn_down,
        "the blocked read ignored the hold until teardown ended it (returned {second}, \
         stall latched: {latched})"
    );
    assert_eq!(second, AVERROR_EOF);
    assert!(
        latched,
        "the abort must be latched for the enclosing FFmpeg operation"
    );
}

/// (e) A zero-byte pre-body abort through the shared reducer, from both active-cursor callers:
/// one requeue, an abandoned round, zero AUs — and a sample the controller can act on, where the
/// completed constructor (rightly) refuses zero bytes and so used to drop the event entirely as
/// "invalid segment timing".
#[test]
fn a_zero_byte_abort_reaches_the_controller_as_an_abandoned_sample() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    SHARED
        .hls_video_tail_ns
        .store(PLAYHEAD_NS + 6_000_000_000, Ordering::Release);
    SHARED.hls_audio_tail_ns.store(-1, Ordering::Release);
    SHARED.disp_base.store(0, Ordering::Relaxed);
    let pre_body = SegmentTransfer {
        bytes: 0,
        active_us: 0,
        total_us: 1,
        audio_expected: true,
    };
    let master = crate::hls::Resource {
        origin: crate::plex::Origin::http("127.0.0.1", 32400),
        path: "/master.m3u8?X-Plex-Token=test-token".to_string(),
    };
    let cursor_at = |pending| HlsCursor {
        publishes_duration: true,
        declared_bps: 0,
        auth: crate::hls::InheritedAuth::capture(&master).expect("fixture token pair"),
        media: master.clone(),
        tracker: Default::default(),
        pending,
        ended: false,
        target_duration_secs: 6,
        start_applied: true,
    };
    let segment = crate::hls::Segment {
        sequence: 11,
        duration: std::time::Duration::from_secs(6),
        resource: master.clone(),
    };
    let clock = || {
        crate::hls::SegmentTimeline::default()
            .begin(std::time::Duration::from_secs(0))
            .expect("fixture clock")
    };

    // Ordinary branch: `hls_demux` hands the StallAbort straight to the reducer.
    let mut ordinary = cursor_at(std::collections::VecDeque::new());
    let ordinary_round =
        hls_stall_abort_outcome(segment.clone(), clock(), pre_body, 6_000, &mut ordinary);
    // Lookahead branch: the same reducer through `hls_prefetch_same_encoder_with`.
    let catalog = crate::abr::HlsActuatorCatalog::measured();
    let controller = crate::abr::Controller::starting_at(crate::abr::Rung::P1080M18, None, catalog);
    let mut lookahead = cursor_at(std::collections::VecDeque::new());
    let lookahead_round = hls_prefetch_same_encoder_with(
        &mut lookahead,
        segment.clone(),
        crate::hls::SegmentTimeline::default(),
        clock(),
        Some(&controller),
        |_, _, _| Err(HlsExit::StallAbort(pre_body)),
    )
    .expect("a StallAbort is a round, not an error")
    .expect("a StallAbort is a round, not nothing");

    for (leg, cursor, (seg, output, _, abandoned)) in [
        ("ordinary", &ordinary, ordinary_round),
        ("lookahead", &lookahead, lookahead_round),
    ] {
        assert!(abandoned, "{leg}: fetch_abandoned");
        assert!(output.aus.is_empty(), "{leg}: zero AUs");
        assert_eq!(cursor.pending.len(), 1, "{leg}: requeued exactly once");
        let sample = demux_loop_sample(&output, seg.duration, abandoned);
        let sample = sample.unwrap_or_else(|| {
            panic!("{leg}: the zero-byte abort was dropped as an invalid timing sample")
        });
        assert!(
            !sample.completed(),
            "{leg}: an abandoned acquisition is never completed"
        );
    }
}

// -- controls: what the runtime must NOT stop, and what it must stop first -------------------

fn runtime_for(acquisition: SegmentAcquisition, aq: &mut AuQueue) -> AcquisitionRuntime {
    acquisition.begin(aq).1
}

fn stops(runtime: &mut AcquisitionRuntime) -> bool {
    crate::checkpoint::Checkpoint::check(runtime) == crate::checkpoint::Flow::Stop
}

/// At the ladder floor the boundary asks for the hold exactly once and the SAME open carries on:
/// one connection, one GET, and the open completes when PMS finally sends its headers. Drives
/// `hls_open_source` with the runtime directly — past the open, `hls_input` needs FFmpeg, which
/// the host does not bind.
#[test]
fn at_the_floor_the_open_asks_for_the_hold_once_and_keeps_its_connection() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let mut pms = ScriptedPms::start(|_| {
        Reply::AfterRelease(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nABCD")
    });
    let (segment, auth) = segment_on(pms.port);
    let path = auth.request_path(&segment.resource).expect("fixture path");
    let mut hs = crate::stream::http_stream_boxed();
    let mut aq = crate::aq::aq_new(1 << 20);
    let (hs_addr, aq_addr) = (
        &mut *hs as *mut HttpStream as usize,
        &mut *aq as *mut AuQueue as usize,
    );
    let seen = pms.seen.take().expect("one run per server");
    let released = pms.released.clone();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let publisher = std::thread::spawn(move || {
        if seen.recv_timeout(std::time::Duration::from_secs(5)).is_ok() {
            publish_accepted_hold();
            // The worker's own request is the seam: release the headers once it has asked.
            let asked_by = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !SHARED.hls_rebuffer_requested.load(Ordering::Acquire)
                && std::time::Instant::now() < asked_by
            {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            released.store(true, Ordering::Release);
        }
        let torn_down = done_rx.recv_timeout(TEARDOWN_AFTER).is_err();
        if torn_down {
            crate::aq::aq_abort(aq_addr as *mut AuQueue);
            crate::stream::http_shutdown(hs_addr as *mut HttpStream);
        }
        torn_down
    });
    let mut runtime = runtime_for(
        SegmentAcquisition::for_test(Some(6_000), true, false),
        &mut aq,
    );
    let mut net = HlsNet {
        hs: &mut *hs,
        curl: None,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let opened = hls_open_source(
        &segment.resource,
        &path,
        &mut *aq,
        &mut net,
        Some(deadline),
        &mut runtime,
    );
    let _ = done_tx.send(());
    let torn_down = publisher.join().expect("publisher");
    let outcome = opened
        .as_ref()
        .map(|(_, size)| *size)
        .map_err(|e| format!("{e:?}"));
    drop(opened);
    crate::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);

    assert!(
        !torn_down,
        "the floor open must finish on its own: {outcome:?}"
    );
    assert_eq!(outcome, Ok(4), "the floor never abandons its only response");
    assert!(
        SHARED.hls_rebuffer_requested.load(Ordering::Acquire),
        "the hold was requested"
    );
    assert!(!runtime.armed(), "asked once, then disarmed");
    assert_eq!(pms.accepts(), 1, "the same connection carried on");
    assert_eq!(pms.requests(), 1, "no second GET");
}

#[test]
fn a_floor_runtime_requests_the_hold_once_then_disarms() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut runtime = runtime_for(
        SegmentAcquisition::for_test(Some(6_000), true, false),
        &mut aq,
    );
    publish_accepted_hold();
    assert!(!stops(&mut runtime));
    assert!(
        SHARED.hls_rebuffer_requested.swap(false, Ordering::AcqRel),
        "asked once"
    );
    assert_eq!(
        crate::checkpoint::Checkpoint::check(&mut runtime),
        crate::checkpoint::Flow::Continue { next_check: None },
        "disarmed: nothing left to check"
    );
    assert!(
        !SHARED.hls_rebuffer_requested.load(Ordering::Acquire),
        "and never asks twice"
    );
    crate::aq::aq_destroy(&mut *aq);
}

/// Candidates and already-held fetches carry no guard, so neither a hold nor a spent playhead can
/// stop them; an armed runtime re-asks no later than one slice.
#[test]
fn unarmed_runtimes_never_stop_and_armed_ones_recheck_within_a_slice() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut armed = runtime_for(
        SegmentAcquisition::for_test(Some(60_000), false, false),
        &mut aq,
    );
    let before = std::time::Instant::now();
    match crate::checkpoint::Checkpoint::check(&mut armed) {
        crate::checkpoint::Flow::Continue {
            next_check: Some(at),
        } => assert!(
            at <= before + acquisition::CHECK_SLICE + std::time::Duration::from_millis(50),
            "an armed runtime re-asks within one slice"
        ),
        other => panic!("an armed runtime keeps asking: {other:?}"),
    }
    let mut candidate = runtime_for(
        SegmentAcquisition::candidate(ReserveDeadlineState::new(None, false)),
        &mut aq,
    );
    let mut held = runtime_for(
        SegmentAcquisition::for_test(Some(2_000), false, true),
        &mut aq,
    );
    assert!(!candidate.armed() && !held.armed());
    publish_accepted_hold();
    crate::player::SHARED
        .playpos_ns
        .store(PLAYHEAD_NS + 60_000_000_000, Ordering::Relaxed);
    for (role, runtime) in [("candidate", &mut candidate), ("already held", &mut held)] {
        assert_eq!(
            crate::checkpoint::Checkpoint::check(runtime),
            crate::checkpoint::Flow::Continue { next_check: None },
            "{role}: never stops, never polls"
        );
    }
    crate::aq::aq_destroy(&mut *aq);
}

/// A hold accepted AND released between two checks still moved the epoch: it is seen.
#[test]
fn a_hold_that_came_and_went_between_checks_is_still_seen() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut runtime = runtime_for(
        SegmentAcquisition::for_test(Some(6_000), false, false),
        &mut aq,
    );
    assert!(!stops(&mut runtime));
    publish_accepted_hold();
    SHARED.hls_rebuffering.store(false, Ordering::Release);
    assert!(stops(&mut runtime), "the epoch moved since arming");
    assert!(runtime.stall_aborted());
    assert!(stops(&mut runtime), "and the stop is latched");
    crate::aq::aq_destroy(&mut *aq);
}

/// A sized body whose every byte arrived is credited, whatever arrives after; an unsized body is
/// complete only at its confirmed end.
#[test]
fn a_completed_body_outranks_a_later_hold_and_an_unsized_one_needs_its_end() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let mut aq = crate::aq::aq_new(1 << 20);
    let armed = || SegmentAcquisition::for_test(Some(6_000), false, false);
    let mut sized = runtime_for(armed(), &mut aq);
    let mut unsized_open = runtime_for(armed(), &mut aq);
    let mut unsized_ended = runtime_for(armed(), &mut aq);
    for runtime in [&mut sized, &mut unsized_open, &mut unsized_ended] {
        runtime.enter(Phase::Body);
    }
    sized.note_body(8, 8, false);
    unsized_open.note_body(-1, 1 << 20, false);
    unsized_ended.note_body(-1, 1 << 20, true);
    publish_accepted_hold();
    assert!(!stops(&mut sized), "every declared byte arrived");
    assert!(!stops(&mut unsized_ended), "a confirmed end is complete");
    assert!(
        stops(&mut unsized_open),
        "an unsized body without its end is not complete"
    );
    crate::aq::aq_destroy(&mut *aq);
}

/// Teardown outranks a simultaneous hold: the fetch ends as `Aborted`, never as recovery evidence.
#[test]
fn teardown_outranks_a_simultaneous_hold() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut runtime = runtime_for(
        SegmentAcquisition::for_test(Some(6_000), false, false),
        &mut aq,
    );
    publish_accepted_hold();
    crate::aq::aq_abort(&mut *aq);
    assert!(stops(&mut runtime));
    assert!(!runtime.stall_aborted(), "teardown latched first");
    assert!(matches!(runtime.stopped_exit(true), Some(HlsExit::Aborted)));
    crate::aq::aq_destroy(&mut *aq);
}

/// The retry wait keeps its original end across rechecks, and a stop ends it at once.
#[test]
fn the_retry_wait_keeps_its_end_across_rechecks_and_ends_on_a_stop() {
    let mut aq = crate::aq::aq_new(1 << 20);
    let wait = std::time::Duration::from_millis(120);
    let mut rechecking =
        crate::checkpoint::TestCheckpoint::every(std::time::Duration::from_millis(10));
    let started = std::time::Instant::now();
    assert!(hls_wait(&mut *aq, wait, None, &mut rechecking).is_ok());
    assert!(
        started.elapsed() >= wait,
        "a recheck never shortens the wait"
    );
    assert!(
        rechecking.calls() > 2,
        "the wait re-asked as its checks fell due"
    );
    let mut stopping =
        crate::checkpoint::TestCheckpoint::stopping_after(1, std::time::Duration::from_millis(10));
    let started = std::time::Instant::now();
    let stopped = hls_wait(
        &mut *aq,
        std::time::Duration::from_secs(10),
        None,
        &mut stopping,
    );
    assert!(
        matches!(stopped, Err(HlsExit::Failed(_))),
        "an unlatched stop is a plain failure"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    crate::aq::aq_destroy(&mut *aq);
}

// -- seams onto production state, so each scenario above reads the same before and after --------

/// An HLS AVIO over `src` carrying `acquisition` exactly as `hls_input` would build it: the
/// runtime begun and moved in, in its body phase.
fn avio_state_for(
    src: Src,
    aq: *mut AuQueue,
    size: i64,
    acquisition: SegmentAcquisition,
) -> AvioState {
    let (reserve_deadline, mut runtime) = acquisition.begin(aq);
    runtime.enter(Phase::Body);
    AvioState {
        src,
        aq,
        off: 0,
        size,
        io_failed: false,
        body_active_us: 0,
        body_bytes: 0,
        first_byte_at: None,
        reserve_deadline,
        transport_watchdog: None,
        acquisition: Some(runtime),
        bounce: Vec::new(),
        bounce_pos: 0,
    }
}

fn avio_stall_aborted(state: &AvioState) -> bool {
    state
        .acquisition
        .as_ref()
        .is_some_and(AcquisitionRuntime::stall_aborted)
}

/// The demux loop's own sample selection for one round.
fn demux_loop_sample(
    output: &HlsSegmentOutput,
    duration: std::time::Duration,
    fetch_abandoned: bool,
) -> Option<crate::abr::SegmentSample> {
    hls_round_sample(output, duration, fetch_abandoned)
}
