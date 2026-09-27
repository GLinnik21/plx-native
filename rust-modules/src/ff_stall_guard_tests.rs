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
        let (finish, finish_cue) = std::sync::mpsc::channel::<()>();
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
            // The prefix is available immediately; this test controls the terminal hold itself,
            // between two synchronous `read_cb` calls. The remainder stays on the server until
            // the test is done: a remainder already RECEIVED would be complete, not abortable.
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCD")
                .expect("headers+prefix");
            socket.flush().expect("flush");
            let _ = finish_cue.recv();
            let _ = socket.write_all(b"EFGH");
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
        let _ = finish.send(());

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
// Synchronisation is causal: the server reports each request it has read, and the acquisition
// runtime reports (via `acquisition::observe`) every answer it gives, with its phase — so a hold
// is published strictly AFTER a chosen `Continue`, and only a LATER check can have seen it.
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
    /// Answer with a prefix, hold the rest until released, then send it.
    PrefixThenRelease(&'static [u8], &'static [u8]),
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
                let socket = match crate::testnet::accept(&listener) {
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
                                Reply::PrefixThenRelease(prefix, rest) => {
                                    let _ = w.write_all(prefix);
                                    let _ = w.flush();
                                    let _ = seen_tx.send(n);
                                    park(&|| rel.load(Ordering::Acquire));
                                    let _ = w.write_all(rest);
                                    let _ = w.flush();
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
        acquisition::observe::unwatch();
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
/// A leg deadline shorter than `CHECK_SLICE`: the transport ends on it without asking again.
const SHORT_DEADLINE: std::time::Duration = std::time::Duration::from_millis(60);

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

/// When a [`demux_against`] trigger fires.
#[derive(Clone, Copy)]
enum Fire {
    /// Once the server has read the first GET.
    OnGet,
    /// Once the runtime has answered its `n`th `Continue` in `phase` (1-based).
    AfterContinue(Phase, usize),
}

/// Run the REAL `hls_demux_segment` against `pms`, firing `trigger` per `fire`. A fetch still
/// running [`TEARDOWN_AFTER`] later is torn down.
fn demux_against(
    pms: &mut ScriptedPms,
    acquisition: SegmentAcquisition,
    trigger: impl FnOnce() + Send + 'static,
) -> DemuxRun {
    demux_against_when(pms, acquisition, Fire::OnGet, trigger)
}

/// Wait on the runtime's answers for the `n`th `Continue` in `phase`.
fn await_continue(
    answers: &std::sync::mpsc::Receiver<(Phase, crate::checkpoint::Flow)>,
    phase: Phase,
    n: usize,
) -> bool {
    let mut seen = 0;
    while let Ok((at, flow)) = answers.recv_timeout(std::time::Duration::from_secs(5)) {
        if at == phase && matches!(flow, crate::checkpoint::Flow::Continue { .. }) {
            seen += 1;
            if seen == n {
                return true;
            }
        }
    }
    false
}

fn demux_against_when(
    pms: &mut ScriptedPms,
    acquisition: SegmentAcquisition,
    fire: Fire,
    trigger: impl FnOnce() + Send + 'static,
) -> DemuxRun {
    let (answers_tx, answers) = std::sync::mpsc::sync_channel(256);
    acquisition::observe::watch_this_thread(answers_tx);
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
        let saw_get = match fire {
            Fire::OnGet => seen.recv_timeout(std::time::Duration::from_secs(5)).is_ok(),
            Fire::AfterContinue(phase, n) => await_continue(&answers, phase, n),
        };
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
    acquisition::observe::unwatch();
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
    // Strictly inside the wait: after the runtime entered it AND answered its first Continue.
    let fire = Fire::AfterContinue(Phase::RetryWaiting, 1);
    let run = demux_against_when(&mut pms, acquisition, fire, publish_accepted_hold);
    let requests = pms.requests();
    assert_prompt_zero_byte_stall_abort(&run, "NotReady wait");
    assert_eq!(requests, 1, "no GET may follow the hold");
}

/// What one blocked second `read_cb` did across a hold published strictly after its runtime's
/// second `Continue` in the body phase (the read's own top-of-loop check, then the transport's
/// first ask before blocking).
struct BlockedRead {
    second: c_int,
    latched: bool,
    torn_down: bool,
    /// The runtime's answers AFTER the hold was published.
    after_hold: Vec<(Phase, crate::checkpoint::Flow)>,
    /// A third `read_cb`, when the second returned bytes.
    third: Option<c_int>,
    accepts: usize,
}

fn blocked_body_read(
    reply: Reply,
    acquisition: SegmentAcquisition,
    watchdog: Option<std::time::Duration>,
    // Runs on the publisher after the hold; receives the server's release switch.
    after_hold: impl FnOnce(&std::sync::Arc<AtomicBool>) + Send + 'static,
) -> BlockedRead {
    static REPLY: std::sync::Mutex<Option<Reply>> = std::sync::Mutex::new(None);
    *REPLY.lock().unwrap() = Some(reply);
    let pms = ScriptedPms::start(|_| REPLY.lock().unwrap().expect("scripted reply"));
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
    let mut state = avio_state_for(src, &mut *aq, 8, acquisition);
    let op = &mut state as *mut AvioState as *mut c_void;
    let mut dst = [0u8; 4];
    assert_eq!(
        read_cb(op, dst.as_mut_ptr(), 4),
        4,
        "fixture: the prefix arrives"
    );
    state.transport_watchdog = watchdog.map(TransportWatchdog::with_inactivity);

    let (answers_tx, answers) = std::sync::mpsc::sync_channel(256);
    acquisition::observe::watch_this_thread(answers_tx);
    let released = pms.released.clone();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let publisher = std::thread::spawn(move || {
        let continued = await_continue(&answers, Phase::Body, 2);
        if continued {
            publish_accepted_hold();
            after_hold(&released);
        }
        let torn_down = done_rx.recv_timeout(TEARDOWN_AFTER).is_err();
        if torn_down {
            crate::aq::aq_abort(aq_addr as *mut AuQueue);
            crate::stream::http_shutdown(hs_addr as *mut HttpStream);
        }
        assert!(
            continued,
            "fixture: the second read must reach its blocking wait"
        );
        (torn_down, answers.try_iter().collect::<Vec<_>>())
    });
    let second = read_cb(op, dst.as_mut_ptr(), 4);
    let _ = done_tx.send(());
    let (torn_down, after_hold) = publisher.join().expect("publisher");
    acquisition::observe::unwatch();
    let third = (second > 0).then(|| read_cb(op, dst.as_mut_ptr(), 4));
    let latched = avio_stall_aborted(&state);
    drop(state);
    crate::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
    let accepts = pms.accepts();
    drop(pms);
    BlockedRead {
        second,
        latched,
        torn_down,
        after_hold,
        third,
        accepts,
    }
}

/// (d) A body read already BLOCKED inside `read_cb` when the hold is accepted — after the
/// runtime last answered `Continue` — must be ended by a LATER check in that same invocation,
/// with the abort latched, not by bytes or the watchdog.
#[test]
fn a_hold_accepted_during_a_blocked_body_read_ends_that_read() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let run = blocked_body_read(
        Reply::SendThenWithhold(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCD"),
        SegmentAcquisition::for_test(Some(6_000), false, false),
        None,
        |_| {},
    );
    assert!(
        !run.torn_down,
        "the blocked read ignored the hold until teardown ended it (returned {}, stall \
         latched: {})",
        run.second, run.latched
    );
    assert_eq!(run.second, AVERROR_EOF);
    assert!(
        run.latched,
        "the abort must be latched for the enclosing FFmpeg operation"
    );
    assert_eq!(
        run.after_hold.first(),
        Some(&(Phase::Body, crate::checkpoint::Flow::Stop)),
        "a check after the published hold is what stopped it"
    );
}

/// MUST-FIX regression (ii): the read's own deadline (here the transport watchdog) is SHORTER
/// than the checkpoint slice, so the transport ends the wait on its deadline without asking the
/// runtime again. A hold published after the last `Continue` must still settle the read as the
/// stall abort, never as a transport failure.
#[test]
fn a_hold_before_a_body_reads_short_deadline_settles_as_a_stall_abort() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let run = blocked_body_read(
        Reply::SendThenWithhold(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCD"),
        SegmentAcquisition::for_test(Some(6_000), false, false),
        Some(SHORT_DEADLINE),
        |_| {},
    );
    assert!(!run.torn_down);
    assert!(
        run.second == AVERROR_EOF && run.latched,
        "the deadline wake bypassed the guard: returned {} (AVERROR_IO is {AVERROR_IO}), stall \
         latched: {}",
        run.second,
        run.latched
    );
}

/// Floor control for (ii): the same short-deadline wake at the floor asks for the hold once and
/// keeps the transport classification.
#[test]
fn at_the_floor_a_body_reads_short_deadline_keeps_its_transport_classification() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let run = blocked_body_read(
        Reply::SendThenWithhold(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCD"),
        SegmentAcquisition::for_test(Some(6_000), true, false),
        Some(SHORT_DEADLINE),
        |_| {},
    );
    assert!(!run.torn_down);
    assert_eq!(
        run.second, AVERROR_IO,
        "a floor stall is still the transport's to report"
    );
    assert!(!run.latched);
    assert!(
        SHARED.hls_rebuffer_requested.load(Ordering::Acquire),
        "the hold was requested"
    );
}

/// Floor continuation for a blocked body read: the hold is requested once, the SAME read keeps
/// waiting on the same connection, and returns the rest when PMS sends it.
#[test]
fn at_the_floor_a_blocked_body_read_asks_once_and_keeps_reading() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let run = blocked_body_read(
        Reply::PrefixThenRelease(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCD", b"EFGH"),
        SegmentAcquisition::for_test(Some(6_000), true, false),
        None,
        |released| {
            let asked_by = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !SHARED.hls_rebuffer_requested.load(Ordering::Acquire)
                && std::time::Instant::now() < asked_by
            {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            // Clear it: a second request would set it again.
            if SHARED.hls_rebuffer_requested.swap(false, Ordering::AcqRel) {
                released.store(true, Ordering::Release);
            }
        },
    );
    assert!(!run.torn_down, "the floor read never asked for the hold");
    assert_eq!(
        run.second, 4,
        "the same read carried on to the rest of the body"
    );
    assert!(!run.latched);
    assert_eq!(
        run.third,
        Some(AVERROR_EOF),
        "then the sized body is complete"
    );
    assert!(
        !SHARED.hls_rebuffer_requested.load(Ordering::Acquire),
        "asked exactly once"
    );
    assert_eq!(run.accepts, 1, "one connection");
}

/// Floor continuation for the `NotReady` wait: asks once, and the wait carries on into its retry
/// (here PMS then refuses it, which ends the test with an ordinary failure). The retry dials
/// afresh — the plaintext transport does not keep a non-2xx response's socket — so the
/// observable is the second GET, not the connection count.
#[test]
fn at_the_floor_the_not_ready_wait_asks_once_and_retries() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let mut pms = ScriptedPms::start(|n| {
        Reply::Send(if n == 0 {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"
        } else {
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n"
        })
    });
    let acquisition = SegmentAcquisition::for_test(Some(6_000), true, false);
    let fire = Fire::AfterContinue(Phase::RetryWaiting, 1);
    let run = demux_against_when(&mut pms, acquisition, fire, publish_accepted_hold);
    assert!(!run.torn_down, "{}", describe(&run.result));
    assert!(
        matches!(run.result, Err(HlsExit::Failed("HTTP request failed"))),
        "the retry ran and PMS refused it: {}",
        describe(&run.result)
    );
    assert!(
        SHARED.hls_rebuffer_requested.load(Ordering::Acquire),
        "the hold was requested"
    );
    assert_eq!(pms.requests(), 2, "the wait carried on into its retry");
}

/// MUST-FIX regression (i): the open's own deadline is SHORTER than the checkpoint slice, so the
/// transport ends the header wait on its deadline without asking again. A hold published after
/// the open's last `Continue` must still settle the open as a zero-byte stall abort, not as the
/// deadline's own classification.
#[test]
fn a_hold_before_an_opens_short_deadline_settles_as_a_stall_abort() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let mut pms = ScriptedPms::start(|_| Reply::Withhold);
    let acquisition = SegmentAcquisition::for_test_with_deadline(
        Some(6_000),
        false,
        false,
        ReserveDeadlineState::new(Some(std::time::Instant::now() + SHORT_DEADLINE), false),
    );
    let fire = Fire::AfterContinue(Phase::Opening, 1);
    let run = demux_against_when(&mut pms, acquisition, fire, publish_accepted_hold);
    assert_prompt_zero_byte_stall_abort(&run, "open, deadline shorter than a slice");
}

/// Floor control for (i): at the floor the same wake asks for the hold once and keeps the
/// deadline's own classification.
#[test]
fn at_the_floor_an_opens_short_deadline_keeps_its_classification() {
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let mut pms = ScriptedPms::start(|_| Reply::Withhold);
    let acquisition = SegmentAcquisition::for_test_with_deadline(
        Some(6_000),
        true,
        false,
        ReserveDeadlineState::new(Some(std::time::Instant::now() + SHORT_DEADLINE), false),
    );
    let fire = Fire::AfterContinue(Phase::Opening, 1);
    let run = demux_against_when(&mut pms, acquisition, fire, publish_accepted_hold);
    assert!(!run.torn_down, "{}", describe(&run.result));
    assert!(
        matches!(run.result, Err(HlsExit::PrimeExpired)),
        "the floor keeps the deadline's classification: {}",
        describe(&run.result)
    );
    assert!(
        SHARED.hls_rebuffer_requested.load(Ordering::Acquire),
        "the hold was requested"
    );
    assert_eq!(pms.requests(), 1);
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
        .map(|(_, size, _)| *size)
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

// -- a body the transport already HOLDS is complete, whatever FFmpeg has read ----------------
//
// PMS pauses a sized response for its JIT encoder and then bursts the remainder. The burst can
// land entirely in the transport — the socket's header buffer or kernel queue, curl's transfer
// buffer — while FFmpeg has consumed only a prefix through AVIO. A hold accepted then must not
// abandon an object that is already fully downloaded: that is a needless downshift and a longer
// rebuffer, paid for bytes that were already here.

#[derive(Clone, Copy, Debug)]
enum HeldTransport {
    Socket,
    Curl,
}

/// Where the 8-byte body is when the hold lands after FFmpeg has read its first 4 bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum BodyArrival {
    /// In the same write as the headers: it is in the transport's own buffer after the open.
    WithHeaders,
    /// Written after the open and before the first read: in the kernel queue (socket) or pulled
    /// into curl's buffer by the first read's transfer step.
    AfterOpen,
    /// Only the first 4 bytes exist; the rest is still on the server. The control.
    Withheld,
}

struct HeldBodyRead {
    second: c_int,
    stall_aborted: bool,
    bytes: [u8; 4],
}

/// Open an 8-byte response on `transport`, read 4 bytes through `read_cb`, publish an accepted
/// hold, and read again. `None` when the host has no libcurl for the curl leg.
fn read_across_a_hold_with_the_body(
    transport: HeldTransport,
    arrival: BodyArrival,
) -> Option<HeldBodyRead> {
    use std::io::{Read, Write};
    let _serial = match transport {
        HeldTransport::Socket => crate::testlock::serial(),
        HeldTransport::Curl => curl_gate()?,
    };
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback server");
    let port = listener.local_addr().unwrap().port();
    let (send_body, body_cue) = std::sync::mpsc::channel::<()>();
    let (body_sent, body_sent_cue) = std::sync::mpsc::channel::<()>();
    let (finish, finish_cue) = std::sync::mpsc::channel::<()>();
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
        let headers: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n";
        match arrival {
            BodyArrival::WithHeaders => {
                socket
                    .write_all(&[headers, b"ABCDEFGH"].concat())
                    .expect("response");
            }
            BodyArrival::AfterOpen => {
                socket.write_all(headers).expect("headers");
                socket.flush().expect("flush");
                let _ = body_cue.recv();
                socket.write_all(b"ABCDEFGH").expect("body");
            }
            BodyArrival::Withheld => {
                socket
                    .write_all(&[headers, b"ABCD"].concat())
                    .expect("prefix");
            }
        }
        socket.flush().expect("flush");
        let _ = body_sent.send(());
        let _ = finish_cue.recv();
        if arrival == BodyArrival::Withheld {
            let _ = socket.write_all(b"EFGH");
        }
    });

    let mut aq = crate::aq::aq_new(1 << 20);
    let mut hs = crate::stream::http_stream_boxed();
    let src = match transport {
        HeldTransport::Socket => {
            let host = CString::new("127.0.0.1").unwrap();
            let path = CString::new("/segment.ts").unwrap();
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
            Src::Socket {
                hs: &mut *hs,
                host,
                port: port as c_int,
                path,
            }
        }
        HeldTransport::Curl => Src::Curl(
            crate::curlio::CurlSource::open(&format!("http://127.0.0.1:{port}/segment.ts"), 0)
                .expect("fixture: the open must succeed"),
        ),
    };
    let _ = send_body.send(());
    body_sent_cue
        .recv()
        .expect("fixture: the server wrote its bytes");
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
        "fixture: the first 4 bytes must arrive cleanly"
    );
    assert_eq!(&dst, b"ABCD");

    publish_accepted_hold();
    let mut bytes = [0u8; 4];
    let second = read_cb(op, bytes.as_mut_ptr(), 4);
    let stall_aborted = avio_stall_aborted(&state);
    drop(state);
    let _ = finish.send(());
    crate::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
    server.join().expect("loopback server");
    Some(HeldBodyRead {
        second,
        stall_aborted,
        bytes,
    })
}

#[test]
fn a_hold_delivers_a_remainder_the_transport_already_holds() {
    let mut abandoned = Vec::new();
    for transport in [HeldTransport::Socket, HeldTransport::Curl] {
        for arrival in [BodyArrival::WithHeaders, BodyArrival::AfterOpen] {
            let Some(read) = read_across_a_hold_with_the_body(transport, arrival) else {
                continue;
            };
            if !(read.second == 4 && &read.bytes == b"EFGH" && !read.stall_aborted) {
                abandoned.push(format!(
                    "{transport:?}/{arrival:?}: got {} (stall_aborted={})",
                    read.second, read.stall_aborted
                ));
            }
        }
    }
    assert!(
        abandoned.is_empty(),
        "the whole body is already in the transport, so the read after a hold must deliver \
         it: {abandoned:#?}"
    );
}

/// The control: bytes still on the wire are not complete, and the same hold abandons them.
#[test]
fn a_hold_still_abandons_a_remainder_still_on_the_wire() {
    for transport in [HeldTransport::Socket, HeldTransport::Curl] {
        let Some(read) = read_across_a_hold_with_the_body(transport, BodyArrival::Withheld) else {
            continue;
        };
        assert!(
            read.second == AVERROR_EOF && read.stall_aborted,
            "{transport:?}: 4 of 8 bytes still on the server must stall-abort — got {} \
             (stall_aborted={})",
            read.second,
            read.stall_aborted,
        );
    }
}

/// A test-only RAII guard: drop always drains [`super::drain_test_elapsed`] so one test's
/// leftover injected duration can never bleed into the next test that reuses this OS thread. It
/// does not assert the queue is empty — each test's own exact-sum assertion on `body_active_us`
/// already catches a duration going unconsumed, and a second panic from this destructor while
/// that first one unwinds would abort the whole process instead of reporting one clean failure.
/// Bind ONE guard per test (`let _guard = InjectedElapsed::guard();`) and push every duration
/// through [`InjectedElapsed::push_us`], which does not itself guard anything — a per-push guard
/// would drain the queue after every single push, including entries still waiting for a later
/// production call to consume them.
struct InjectedElapsed;

impl InjectedElapsed {
    fn guard() -> InjectedElapsed {
        InjectedElapsed
    }

    fn push_us(micros: u64) {
        super::push_test_elapsed(std::time::Duration::from_micros(micros));
    }
}

impl Drop for InjectedElapsed {
    fn drop(&mut self) {
        // Drain only — never assert here. A test's own exact-sum assertion already catches an
        // injected duration going unconsumed (the tally comes up short); panicking a SECOND time
        // from this destructor while that first assertion is already unwinding would abort the
        // whole process (Rust aborts on a double panic) instead of reporting one clean failure.
        let _ = super::drain_test_elapsed();
    }
}

/// The transfer step a receipt takes is body work, counted once. `read_cb`'s completion query may
/// run curl's `perform` — receiving (on https, decrypting) a burst into the transfer buffer —
/// before the timed read that then only copies from it. That step's time belongs in
/// `body_active_us` alongside the bytes it received, or the capacity observation is inflated and
/// body work lands in the fixed-overhead term.
///
/// Deterministic, not a wall-clock ratio: the receipt's and the read's own elapsed measurements
/// are injected (`InjectedElapsed`), so the assertion is an exact sum rather than "close enough"
/// — a real scheduler can preempt either interval independently and make a wall-clock comparison
/// flake on a loaded runner without the accounting itself being wrong.
#[test]
fn transfer_work_a_receipt_takes_is_counted_in_body_time() {
    use std::io::{Read, Write};
    const BODY: usize = 8 << 20;
    let Some(_gate) = curl_gate() else { return };
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback server");
    let port = listener.local_addr().unwrap().port();
    let (send_body, body_cue) = std::sync::mpsc::channel::<()>();
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
        let headers = format!("HTTP/1.1 200 OK\r\nContent-Length: {BODY}\r\n\r\n");
        socket.write_all(headers.as_bytes()).expect("headers");
        socket.flush().expect("flush");
        let _ = body_cue.recv();
        // Blocks once the socket buffers fill; the reader is dropped before the join, which
        // ends the write.
        let _ = socket.write_all(&vec![0x47u8; BODY]);
    });
    let cs = crate::curlio::CurlSource::open(&format!("http://127.0.0.1:{port}/segment.ts"), 0)
        .expect("fixture: the open must succeed");
    let _ = send_body.send(());
    // Let the burst fill the kernel's buffers while the transfer buffer is empty, so the first
    // completion query is the one that steps curl's `perform` (`stepped == true`) rather than
    // finding bytes already waiting in the transfer buffer.
    std::thread::sleep(std::time::Duration::from_millis(100));
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut state = avio_state_for(
        Src::Curl(cs),
        &mut *aq,
        BODY as i64,
        SegmentAcquisition::for_test(Some(6_000), false, false),
    );
    let op = &mut state as *mut AvioState as *mut c_void;
    let mut dst = [0u8; 4];
    // note_received's stepped receipt, then the successful read: consumed in that order.
    let _guard = InjectedElapsed::guard();
    InjectedElapsed::push_us(1_234);
    InjectedElapsed::push_us(7);
    assert_eq!(read_cb(op, dst.as_mut_ptr(), 4), 4, "fixture: 4 body bytes");
    assert_eq!(
        state.body_active_us, 1_241,
        "the receipt's stepped-query time (1234us) plus the read's own time (7us) must be the \
         whole of body_active_us, exactly",
    );
    drop(state);
    crate::aq::aq_destroy(&mut *aq);
    server.join().expect("loopback server");
}

/// The control for the test above: a receipt that finds bytes already sitting in the transfer
/// buffer never takes a transfer step (`stepped == false`), so its query contributes nothing —
/// only the read that actually copies the bytes out is body time. The plaintext socket transport
/// is the deterministic way to get `stepped == false`: `stream::http_body_receipt` never sets it,
/// on any branch (see `stream.rs`), so this needs no timing race to arrange.
#[test]
fn a_non_stepping_receipt_contributes_nothing_only_the_read_does() {
    use std::io::{Read, Write};
    let _serial = crate::testlock::serial();
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
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
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCDEFGH")
            .expect("response");
        socket.flush().expect("flush");
        std::thread::sleep(std::time::Duration::from_millis(200));
    });
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut hs = crate::stream::http_stream_boxed();
    let host = CString::new("127.0.0.1").unwrap();
    let path = CString::new("/segment.ts").unwrap();
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
    let src = Src::Socket {
        hs: &mut *hs,
        host,
        port: port as c_int,
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
    let _guard = InjectedElapsed::guard();
    InjectedElapsed::push_us(7);
    assert_eq!(read_cb(op, dst.as_mut_ptr(), 4), 4, "fixture: 4 body bytes");
    assert_eq!(
        state.body_active_us, 7,
        "a non-stepping receipt must contribute nothing; only the 7us read may land",
    );
    drop(state);
    crate::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
    server.join().expect("loopback server");
}

/// A failed read must not credit its own (never-measured) duration, and every settlement receipt
/// asked around it — `note_received` at the top of `read_cb`'s loop, then again in its `if r < 0`
/// branch — is credited exactly like any other stepping receipt, exactly once each. `read_cb`
/// only adds the read's OWN timer to `body_active_us` on the success path (`r > 0`); a transport
/// watchdog that fires ends a withheld body with `READ_DEADLINE` (`r < 0`) without touching
/// curl's `done`/`readable`/`poisoned`/`abort` flags, so with the transfer buffer already
/// drained both the query before the blocked read and the settlement query after it genuinely
/// step — deterministic because the deadline, not a hold published from another thread, is what
/// ends the read.
#[test]
fn a_failed_read_credits_nothing_and_its_settlement_receipt_is_credited_once() {
    let Some(_gate) = curl_gate() else { return };
    let _reset = SharedHoldReset::at(PLAYHEAD_NS);
    static REPLY: std::sync::Mutex<Option<Reply>> = std::sync::Mutex::new(None);
    *REPLY.lock().unwrap() =
        Some(Reply::SendThenWithhold(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCD"));
    let pms = ScriptedPms::start(|_| REPLY.lock().unwrap().expect("scripted reply"));
    let cs = crate::curlio::CurlSource::open(&format!("http://127.0.0.1:{}/segment.ts", pms.port), 0)
        .expect("fixture: the open must succeed");
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut state = avio_state_for(
        Src::Curl(cs),
        &mut *aq,
        8,
        SegmentAcquisition::for_test(Some(6_000), false, false),
    );
    let op = &mut state as *mut AvioState as *mut c_void;
    let mut dst = [0u8; 4];
    assert_eq!(
        read_cb(op, dst.as_mut_ptr(), 4),
        4,
        "fixture: the 4-byte prefix arrives"
    );
    let before_second_call = state.body_active_us;
    // The rest of the body never arrives; a short inactivity watchdog ends the second read on
    // its own deadline, deterministically, with no hold-publish race against another thread.
    state.transport_watchdog = Some(TransportWatchdog::with_inactivity(SHORT_DEADLINE));

    // Both queries this call makes find the transfer buffer drained and are stepping: the
    // top-of-loop `note_received` before the blocked read is attempted, and the settlement
    // `note_received` asked right after the deadline fails it.
    let _guard = InjectedElapsed::guard();
    InjectedElapsed::push_us(101);
    InjectedElapsed::push_us(202);
    let second = read_cb(op, dst.as_mut_ptr(), 4);
    assert!(
        second < 0,
        "the withheld body's watchdog deadline must end the read as a failure, got {second}"
    );
    assert_eq!(
        state.body_active_us,
        before_second_call + 101 + 202,
        "both stepping receipts around the failed read must be credited exactly once each, and \
         the failed read itself must credit nothing beyond them",
    );
    drop(state);
    crate::aq::aq_destroy(&mut *aq);
    drop(pms);
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
