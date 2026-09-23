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

/// **Same-encoder lookahead must observe a terminal hold exactly like the ordinary fetch it
/// shadows.** Device-measured `pipe_abr_down_collapse`: the link collapsed from 40 Mbps to
/// 500 kbps while on rung 20000; segment 11 (5.4 MB) then ran 86.8 s and froze the picture,
/// because `hls_prefetch_same_encoder` builds its AVIO policy with NO stall guard at all
/// (`ReserveDeadlineState::new(None, false), None` — ff.rs, the same-encoder lookahead call to
/// `hls_demux_segment`), while the ordinary branch two lines above arms one via
/// `arm_active_stall_guard` for the identical active cursor. This drives the real `read_cb`
/// callback under each policy against the identical terminal-hold-with-bytes-remaining
/// condition and shows the asymmetry directly: the ordinary policy aborts, the lookahead's
/// (as constructed today, unconditionally `None`) reads straight through it.
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

    // The exact production wiring `hls_prefetch_same_encoder` now uses for its lookahead fetch
    // (ff.rs: `SegmentAcquisition::active(...)` built from `at_floor: Some(...)`) — not a
    // hand-built guard, the same constructor call the lookahead itself makes.
    let lookahead_policy = SegmentAcquisition::active(Some(6_000), false, false).stall();
    assert!(
        drive_two_reads_across_a_terminal_hold(lookahead_policy),
        "the same-encoder lookahead must abort under a terminal hold exactly like the ordinary \
         fetch it shadows — reading straight through it is how `pipe_abr_down_collapse` froze \
         the picture for ~84s on segment 11",
    );
}

/// The `Active` role's one constructor must preserve every legitimate unarmed outcome
/// `arm_active_stall_guard` already defined — an unknown reserve, a reserve of zero at the start
/// of a fetch, and an already-held clock all arm nothing, and only as this evaluation's own
/// answer, never a call site's shortcut.
#[test]
fn the_active_acquisition_constructor_preserves_arm_active_stall_guards_exclusions() {
    assert!(
        SegmentAcquisition::active(None, false, false)
            .stall()
            .is_none(),
        "an unknowable reserve arms nothing"
    );
    assert!(
        SegmentAcquisition::active(Some(0), false, false)
            .stall()
            .is_none(),
        "a reserve of zero at the start of a fetch arms nothing"
    );
    assert!(
        SegmentAcquisition::active(Some(2_000), false, true)
            .stall()
            .is_none(),
        "an already-held clock arms no second abort"
    );
    assert!(
        SegmentAcquisition::active(Some(2_000), false, false)
            .stall()
            .is_some(),
        "a live reserve with no existing hold must still arm"
    );
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
