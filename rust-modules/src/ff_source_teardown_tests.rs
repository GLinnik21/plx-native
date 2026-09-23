//! HTTP and libcurl source teardown/abort behavior: seek-after-teardown, aborted
//! read/seek races, and stalled or IO-failed transports across both transports.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// Teardown fires ONE `shutdown(2)` at the demux socket, but our AVIO is SEEKABLE, so
/// libavformat heals the resulting broken read by calling `seek_cb` — which reopened the URL
/// with a byte Range. That fresh socket is one the already-fired shutdown cannot reach, so the
/// demuxer reads on while the main thread sits in `stream_th.join()`. `read_cb` has bailed on
/// an aborted lane since it was written, and the reopen site in `demux` carries the same guard
/// with a comment naming this exact failure; `seek_cb` was the third way in and had neither.
///
/// The invariant is carried by the ACCEPT COUNT, not the return value — a `seek_cb` that
/// reopened and then failed for an unrelated reason would also return -1. The trailing
/// AVSEEK_SIZE assertion pins the guard's PLACEMENT: bolted to the top of `seek_cb` it would
/// start reporting the stream as unsized, a second behaviour change smuggled in under a
/// teardown fix.
#[test]
fn a_seek_after_teardown_fails_instead_of_opening_a_second_connection() {
    with_counting_listener(|port, accepts, _| {
        let (mut hs, mut aq, ip, path) = opened_stream_with_aborted_lane(port);
        assert_eq!(
            accepts.load(Ordering::Acquire),
            1,
            "fixture: exactly one connection so far"
        );
        let mut st = AvioState {
            src: Src::Socket {
                hs: &mut *hs,
                host: ip,
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
            acquisition: None,
            bounce: Vec::new(),
            bounce_pos: 0,
        };
        let op = &mut st as *mut AvioState as *mut c_void;

        let rv = seek_cb(op, 4, SEEK_SET);

        assert_eq!(
            accepts.load(Ordering::Acquire),
            1,
            "seek_cb opened a SECOND connection during teardown — the one the main thread's \
             join is waiting on, and the one its shutdown(2) can no longer reach"
        );
        assert_eq!(
            rv, -1,
            "an aborted seek must report failure so libavformat stops healing"
        );
        assert_eq!(
            seek_cb(op, 0, AVSEEK_SIZE),
            8,
            "a size query is not I/O — the guard belongs AFTER that branch"
        );
        crate::stream::http_close(&mut *hs);
        crate::aq::aq_destroy(&mut *aq);
    });
}

/// The pair invariant, and the answer to "can an aborted demuxer ping-pong forever?".
/// `read_cb` returns AVERROR_EOF unconditionally once the lane is aborted, and libavformat's
/// recovery for a failed read on a seekable AVIO is to seek and read again — so if `seek_cb`
/// succeeds, every hop of that loop is a full `http_open` (connect + request + header read)
/// against a PMS the main thread is already waiting on. Measured on the unguarded code: nine
/// accepts for eight hops, i.e. one whole reopen per hop, not the single wasted open the
/// static reading of this suggested.
#[test]
fn an_aborted_read_and_seek_cannot_ping_pong_into_new_connections() {
    with_counting_listener(|port, accepts, _| {
        let (mut hs, mut aq, ip, path) = opened_stream_with_aborted_lane(port);
        let mut st = AvioState {
            src: Src::Socket {
                hs: &mut *hs,
                host: ip,
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
            acquisition: None,
            bounce: Vec::new(),
            bounce_pos: 0,
        };
        let op = &mut st as *mut AvioState as *mut c_void;
        let mut dst = [0u8; 8];
        let mut reads = Vec::new();
        let mut seeks = Vec::new();
        for _ in 0..8 {
            reads.push(read_cb(op, dst.as_mut_ptr(), dst.len() as c_int));
            seeks.push(seek_cb(op, 4, SEEK_SET)); // 4, not 0: a seek to 0 returns 0, and 0 is success
        }
        assert_eq!(
            accepts.load(Ordering::Acquire),
            1,
            "the read/seek recovery loop reconnected once per hop — that is the wedge, \
             not merely a slow teardown"
        );
        assert!(
            reads.iter().all(|r| *r == AVERROR_EOF),
            "aborted reads must all report EOF: {reads:?}"
        );
        assert!(
            seeks.iter().all(|r| *r == -1),
            "every hop must refuse the seek: {seeks:?}"
        );
        crate::stream::http_close(&mut *hs);
        crate::aq::aq_destroy(&mut *aq);
    });
}

#[test]
fn an_expired_candidate_deadline_stops_before_touching_its_transport() {
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut st = AvioState {
        // A null stream would crash if dispatch were reached; the deadline must settle first.
        src: Src::Socket {
            hs: std::ptr::null_mut(),
            host: CString::new("unused").unwrap(),
            port: 1,
            path: CString::new("/unused").unwrap(),
        },
        aq: &mut *aq,
        off: 0,
        size: -1,
        io_failed: false,
        body_active_us: 0,
        body_bytes: 0,
        first_byte_at: None,
        reserve_deadline: ReserveDeadlineState::new(Some(std::time::Instant::now()), false),
        transport_watchdog: None,
        acquisition: None,
        bounce: Vec::new(),
        bounce_pos: 0,
    };
    let mut dst = [0u8; 8];
    let result = read_cb(
        &mut st as *mut AvioState as *mut c_void,
        dst.as_mut_ptr(),
        dst.len() as c_int,
    );
    assert_eq!(result, AVERROR_EOF);
    assert!(st.reserve_deadline.expired);
    assert!(
        !st.io_failed,
        "a rejected prime is not an active-stream transport failure"
    );
    crate::aq::aq_destroy(&mut *aq);
}

#[test]
fn a_stalled_candidate_body_ends_at_transport_liveness_not_recursive_reserve_retries() {
    use std::io::{Read, Write};

    let _guard = crate::testlock::serial();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stall server");
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept");
        let _ = socket.set_read_timeout(Some(std::time::Duration::from_secs(1)));
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
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n")
            .expect("headers");
        socket.flush().expect("flush headers");
        std::thread::sleep(std::time::Duration::from_millis(160));
    });

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
    let start_ns = 80_000_000_000;
    let old_playpos = SHARED.playpos_ns.swap(start_ns, Ordering::AcqRel);
    let old_paused = crate::player::TX.paused.swap(false, Ordering::AcqRel);
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
        reserve_deadline: ReserveDeadlineState::from_playhead_at(
            start_ns,
            std::time::Duration::from_millis(2),
            false,
        ),
        transport_watchdog: Some(TransportWatchdog::with_inactivity(
            std::time::Duration::from_millis(35),
        )),
        acquisition: None,
        bounce: Vec::new(),
        bounce_pos: 0,
    };
    let started = std::time::Instant::now();
    let mut dst = [0u8; 8];
    let result = read_cb(
        &mut state as *mut AvioState as *mut c_void,
        dst.as_mut_ptr(),
        dst.len() as c_int,
    );
    let elapsed = started.elapsed();

    assert_eq!(result, AVERROR_IO);
    assert!(state.io_failed);
    assert!(!state.reserve_deadline.expired);
    assert!(
        elapsed >= std::time::Duration::from_millis(20)
            && elapsed < std::time::Duration::from_millis(140),
        "one no-progress epoch should bound every stale reserve wake: {elapsed:?}",
    );

    SHARED.playpos_ns.store(old_playpos, Ordering::Release);
    crate::player::TX
        .paused
        .store(old_paused, Ordering::Release);
    crate::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
    server.join().expect("stall server");
}

#[test]
fn abort_while_a_playlist_body_is_blocked_wins_over_the_transport_result() {
    use std::io::{Read, Write};

    let _guard = crate::testlock::serial();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stall server");
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
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n")
            .expect("headers");
        socket.flush().expect("flush headers");
        std::thread::sleep(std::time::Duration::from_millis(160));
    });

    let host = CString::new("127.0.0.1").unwrap();
    let path = CString::new("/playlist.m3u8").unwrap();
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
    let aq_addr = (&mut *aq) as *mut AuQueue as usize;
    let hs_addr = (&mut *hs) as *mut HttpStream as usize;
    let mut src = Src::Socket {
        hs: &mut *hs,
        host,
        port: port as c_int,
        path,
    };
    let mut dst = [0u8; 8];
    let outcome = std::thread::scope(|scope| {
        scope.spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            crate::aq::aq_abort(aq_addr as *mut AuQueue);
            crate::stream::http_shutdown(hs_addr as *mut HttpStream);
        });
        hls_source_read(
            &mut src,
            &mut *aq,
            &mut dst,
            Some(std::time::Instant::now() + std::time::Duration::from_secs(1)),
        )
    });

    assert!(matches!(outcome, Err(HlsExit::Aborted)), "got {outcome:?}");
    crate::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
    server.join().expect("stall server");
}

#[test]
fn a_prefetched_segment_does_not_move_the_published_buffer() {
    let prev_video = SHARED.hls_video_tail_ns.swap(-1, Ordering::AcqRel);
    let prev_audio = SHARED.hls_audio_tail_ns.swap(-1, Ordering::AcqRel);
    let prev_pos = SHARED.playpos_ns.swap(0, Ordering::AcqRel);
    let prev_base = SHARED.disp_base.swap(0, Ordering::AcqRel);
    let held = HlsSegmentOutput {
        aus: Vec::new(),
        transfer: SegmentTransfer {
            bytes: 1,
            active_us: 1,
            total_us: 1,
            audio_expected: true,
        },
        video_width: 1920,
        video_height: 1080,
        video_tail_ns: 2_000_000_000,
        audio_tail_ns: Some(2_000_000_000),
    };
    let published = hls_buffer_snapshot(None).buffered_ms();
    let if_counted = hls_buffer_snapshot(Some(&held)).buffered_ms();
    SHARED
        .hls_video_tail_ns
        .store(prev_video, Ordering::Release);
    SHARED
        .hls_audio_tail_ns
        .store(prev_audio, Ordering::Release);
    SHARED.playpos_ns.store(prev_pos, Ordering::Release);
    SHARED.disp_base.store(prev_base, Ordering::Release);
    assert_ne!(
        published, if_counted,
        "feeding N+1 into the snapshot is what would inflate E; holding it off-queue must not"
    );
    assert!(
        if_counted.unwrap_or(0) > published.unwrap_or(0),
        "the trap is specifically that Some(prefetch) looks like more reserve"
    );
}

#[test]
fn lookahead_errors_are_not_fatal_except_teardown() {
    assert!(hls_prefetch_is_fatal(&HlsExit::Aborted));
    assert!(!hls_prefetch_is_fatal(&HlsExit::Failed(
        "HTTP request failed"
    )));
    assert!(!hls_prefetch_is_fatal(&HlsExit::NotReady));
    assert!(!hls_prefetch_is_fatal(&HlsExit::PrimeExpired));
    assert!(!hls_prefetch_is_fatal(&HlsExit::StallAbort(
        SegmentTransfer {
            bytes: 1,
            active_us: 1,
            total_us: 1,
            audio_expected: false,
        }
    )));
}

#[test]
fn drain_keeps_polling_only_while_more_body_is_expected() {
    assert!(
        drain_keep_polling(true, false),
        "a live body with bounce room must keep the 10 ms window-open wait"
    );
    assert!(
        !drain_keep_polling(true, true),
        "a finished body must cond_wait, not spin"
    );
    assert!(!drain_keep_polling(false, false), "cap must cond_wait");
}

#[test]
fn commit_skips_lookahead_and_stay_does_not() {
    assert!(hls_should_prefetch_after_abr(true, false, true));
    assert!(
        !hls_should_prefetch_after_abr(true, false, false),
        "a commit must not GET N+1"
    );
    assert!(!hls_should_prefetch_after_abr(true, true, true));
    assert!(!hls_should_prefetch_after_abr(false, false, true));
}

#[test]
fn io_failed_after_drain_surfaces_once_bounce_is_empty() {
    assert!(!park_read_fails_closed(true, true));
    assert!(park_read_fails_closed(false, true));
    assert!(!park_read_fails_closed(false, false));

    let mut aq = crate::aq::aq_new(1 << 20);
    let mut st = AvioState {
        src: Src::Idle,
        aq: &mut *aq,
        off: 0,
        size: 8,
        io_failed: true,
        body_active_us: 0,
        body_bytes: 0,
        first_byte_at: None,
        reserve_deadline: ReserveDeadlineState::new(None, false),
        transport_watchdog: None,
        acquisition: None,
        bounce: b"ABCD".to_vec(),
        bounce_pos: 0,
    };
    let op = &mut st as *mut AvioState as *mut c_void;
    let mut dst = [0u8; 8];
    assert_eq!(read_cb(op, dst.as_mut_ptr(), dst.len() as c_int), 4);
    assert_eq!(&dst[..4], b"ABCD");
    assert_eq!(
        read_cb(op, dst.as_mut_ptr(), dst.len() as c_int),
        AVERROR_IO,
        "park-time RST must not look like EOS after bounce is consumed"
    );
    crate::aq::aq_destroy(&mut *aq);
}

#[test]
fn take_curl_leaves_idle_and_seek_on_idle_does_not_open() {
    let mut src = Src::Socket {
        hs: std::ptr::null_mut(),
        host: CString::new("127.0.0.1").unwrap(),
        port: 1,
        path: CString::new("/").unwrap(),
    };
    assert!(src.take_curl().is_none());
    assert!(
        matches!(src, Src::Socket { port: 1, .. }),
        "a socket source must come back unchanged"
    );

    let mut aq = crate::aq::aq_new(1 << 20);
    let mut st = AvioState {
        src: Src::Idle,
        aq: &mut *aq,
        off: 0,
        size: 8,
        io_failed: false,
        body_active_us: 0,
        body_bytes: 0,
        first_byte_at: None,
        reserve_deadline: ReserveDeadlineState::new(None, false),
        transport_watchdog: None,
        acquisition: None,
        bounce: Vec::new(),
        bounce_pos: 0,
    };
    let op = &mut st as *mut AvioState as *mut c_void;
    assert_eq!(
        seek_cb(op, 4, SEEK_SET),
        -1,
        "Idle must refuse a seek instead of http_open on a dummy socket"
    );
    assert_eq!(seek_cb(op, 0, AVSEEK_SIZE), 8);
    let mut dst = [0u8; 8];
    assert_eq!(
        read_cb(op, dst.as_mut_ptr(), dst.len() as c_int),
        AVERROR_EOF,
        "Idle must not panic or dial during avformat_close_input"
    );
    assert!(!st.drain_wire(), "Idle has nothing to drain");
    crate::aq::aq_destroy(&mut *aq);
}

#[test]
fn take_curl_from_a_live_curl_source_leaves_idle() {
    let Some(_gate) = curl_gate() else { return };
    with_counting_listener(|port, _, _| {
        let cs = crate::curlio::CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0)
            .expect("open");
        let mut src = Src::Curl(cs);
        assert!(src.take_curl().is_some());
        assert!(matches!(src, Src::Idle));
    });
}

#[test]
fn a_curl_seek_after_teardown_fails_instead_of_opening_a_second_connection() {
    let Some(_gate) = curl_gate() else { return };
    with_counting_listener(|port, accepts, requests| {
        let (cs, mut aq) = opened_curl_with_aborted_lane(port);
        assert_eq!(
            accepts.load(Ordering::Acquire),
            1,
            "fixture: exactly one connection so far"
        );
        assert_eq!(
            requests.load(Ordering::Acquire),
            1,
            "fixture: and exactly one request"
        );
        let mut st = AvioState {
            src: Src::Curl(cs),
            aq: &mut *aq,
            off: 0,
            size: 8,
            io_failed: false,
            body_active_us: 0,
            body_bytes: 0,
            first_byte_at: None,
            reserve_deadline: ReserveDeadlineState::new(None, false),
            transport_watchdog: None,
            acquisition: None,
            bounce: Vec::new(),
            bounce_pos: 0,
        };
        let op = &mut st as *mut AvioState as *mut c_void;

        let rv = seek_cb(op, 4, SEEK_SET);

        assert_eq!(
            requests.load(Ordering::Acquire),
            1,
            "seek_cb went back to the server through curlio during teardown — and it would do \
             so on the CACHED connection, which is why this grades requests and not accepts"
        );
        assert_eq!(
            accepts.load(Ordering::Acquire),
            1,
            "and opened no new connection either"
        );
        assert_eq!(
            rv, -1,
            "an aborted seek must report failure so libavformat stops healing"
        );
        assert_eq!(
            seek_cb(op, 0, AVSEEK_SIZE),
            8,
            "a size query is not I/O — the guard belongs AFTER that branch, on both transports"
        );
        crate::aq::aq_destroy(&mut *aq);
    });
}

#[test]
fn an_aborted_curl_read_and_seek_cannot_ping_pong_into_new_connections() {
    let Some(_gate) = curl_gate() else { return };
    with_counting_listener(|port, accepts, requests| {
        let (cs, mut aq) = opened_curl_with_aborted_lane(port);
        let mut st = AvioState {
            src: Src::Curl(cs),
            aq: &mut *aq,
            off: 0,
            size: 8,
            io_failed: false,
            body_active_us: 0,
            body_bytes: 0,
            first_byte_at: None,
            reserve_deadline: ReserveDeadlineState::new(None, false),
            transport_watchdog: None,
            acquisition: None,
            bounce: Vec::new(),
            bounce_pos: 0,
        };
        let op = &mut st as *mut AvioState as *mut c_void;
        let mut dst = [0u8; 8];
        let mut reads = Vec::new();
        let mut seeks = Vec::new();
        for _ in 0..8 {
            reads.push(read_cb(op, dst.as_mut_ptr(), dst.len() as c_int));
            seeks.push(seek_cb(op, 4, SEEK_SET));
        }
        assert_eq!(
            requests.load(Ordering::Acquire),
            1,
            "the read/seek recovery loop asked the server again once per hop — that is the \
             wedge, not merely a slow teardown"
        );
        assert_eq!(
            accepts.load(Ordering::Acquire),
            1,
            "and it opened no new connection either"
        );
        assert!(
            reads.iter().all(|r| *r == AVERROR_EOF),
            "aborted reads must all report EOF: {reads:?}"
        );
        assert!(
            seeks.iter().all(|r| *r == -1),
            "every hop must refuse the seek: {seeks:?}"
        );
        crate::aq::aq_destroy(&mut *aq);
    });
}

/// A curl machinery failure is not the same event as the server finishing the file. Pin the
/// distinction at the AVIO seam, where an earlier implementation collapsed every non-positive
/// source result to EOF and thereby hid mid-playback truncation from both FFmpeg and the HUD.
#[test]
fn a_curl_transport_failure_crosses_avio_as_io_error_not_eof() {
    let Some(_gate) = curl_gate() else { return };
    with_counting_listener(|port, _, _| {
        struct ClearIoFailure;
        impl Drop for ClearIoFailure {
            fn drop(&mut self) {
                SHARED.demux_io_failed.store(false, Ordering::Relaxed);
            }
        }
        let _clear = ClearIoFailure;
        SHARED.demux_io_failed.store(false, Ordering::Relaxed);

        let mut cs =
            crate::curlio::CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0)
                .expect("fixture: open");
        cs.fail_multi_for_test();
        let mut aq = crate::aq::aq_new(1 << 20);
        let mut st = AvioState {
            src: Src::Curl(cs),
            aq: &mut *aq,
            off: 0,
            size: 8,
            io_failed: false,
            body_active_us: 0,
            body_bytes: 0,
            first_byte_at: None,
            reserve_deadline: ReserveDeadlineState::new(None, false),
            transport_watchdog: None,
            acquisition: None,
            bounce: Vec::new(),
            bounce_pos: 0,
        };
        let op = &mut st as *mut AvioState as *mut c_void;
        let mut dst = [0u8; 8];

        // Headers and all eight body bytes may have arrived together. Drain any buffered bytes;
        // the terminal callback result is what must retain the machinery failure.
        let mut terminal = 1;
        for _ in 0..3 {
            terminal = read_cb(op, dst.as_mut_ptr(), dst.len() as c_int);
            if terminal <= 0 {
                break;
            }
        }

        assert_eq!(
            terminal, AVERROR_IO,
            "transport failure must not masquerade as AVERROR_EOF"
        );
        assert!(
            st.io_failed,
            "the callback leaves the error pending on its enclosing operation"
        );
        assert!(
            !SHARED.demux_io_failed.load(Ordering::Acquire),
            "a callback alone is not fatal because libavformat may still recover via seek"
        );
        assert!(
            !frame_read_failed(&mut st, 0),
            "a successful packet is not a demux failure"
        );
        assert!(
            st.io_failed,
            "packet success must not hide a transport failure that only drained bounce"
        );
        assert!(!SHARED.demux_io_failed.load(Ordering::Acquire));

        let terminal = read_cb(op, dst.as_mut_ptr(), dst.len() as c_int);
        assert_eq!(terminal, AVERROR_IO);
        assert!(
            frame_read_failed(&mut st, terminal),
            "an unrecovered frame read ends the demux loop"
        );
        assert!(
            SHARED.demux_io_failed.load(Ordering::Acquire),
            "the main-thread pump must see the failure even after frames were presented"
        );
        crate::aq::aq_destroy(&mut *aq);
    });
}
