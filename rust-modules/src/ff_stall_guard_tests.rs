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
        !guard.should_abort(5_785_560, false),
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
        action: super::StallAction::AbortFetch,
    };
    assert!(
        !guard.should_abort(400_000, false),
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
        action: super::StallAction::AbortFetch,
    };
    crate::player::SHARED
        .playpos_ns
        .store(pos + 1_999_000_000, std::sync::atomic::Ordering::Relaxed);
    assert!(
        !guard.should_abort(400_000, false),
        "one millisecond of reserve remains"
    );
    // Exactly the two seconds of media present at the boundary have now been consumed.
    crate::player::SHARED
        .playpos_ns
        .store(pos + 2_000_000_000, std::sync::atomic::Ordering::Relaxed);
    assert!(
        guard.should_abort(400_000, false),
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
    fn drive_two_reads_across_a_terminal_hold(policy: Option<StallGuard>) -> bool {
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
        let mut state = AvioState {
            src: Src::Socket {
                hs: &mut *hs,
                host,
                port: port as c_int,
                path,
            },
            aq: &mut *aq,
            off: 0,
            size: 8,
            io_failed: false,
            body_active_us: 0,
            body_bytes: 0,
            first_byte_at: None,
            reserve_deadline: ReserveDeadlineState::new(None, false),
            transport_watchdog: None,
            stall: policy,
            stall_aborted: false,
            bounce: Vec::new(),
            bounce_pos: 0,
        };
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
        let aborted = second == AVERROR_EOF && state.stall_aborted;

        SHARED
            .hls_rebuffering
            .store(false, std::sync::atomic::Ordering::Release);
        crate::stream::http_close(&mut *hs);
        crate::aq::aq_destroy(&mut *aq);
        server.join().expect("loopback server");
        aborted
    }

    let ordinary_policy = arm_active_stall_guard(Some(6_000), false, false);
    assert!(
        drive_two_reads_across_a_terminal_hold(ordinary_policy),
        "sanity: the ordinary branch's own policy must abort under its own terminal hold",
    );

    // The exact evaluation `SegmentAcquisition::for_cursor` now runs for an active-cursor fetch
    // (via the test-only `for_test` escape hatch, which is `active` under a name `ff` cannot
    // reach — see `ff_acquisition.rs`) — not a hand-built guard, the same evaluation the
    // production constructor runs for both the ordinary fetch and the lookahead.
    let lookahead_policy = SegmentAcquisition::for_test(Some(6_000), false, false).stall();
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
    SHARED.disp_base.store(0, std::sync::atomic::Ordering::Relaxed);
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
        guard.should_abort(1, true),
        "B=0 with bytes remaining is terminal"
    );
    assert!(
        !guard.should_abort(0, true),
        "a complete sized response must be credited"
    );
    assert!(
        !guard.should_abort(-1, true),
        "past the declared end is complete too"
    );
}
