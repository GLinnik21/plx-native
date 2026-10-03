//! Read/connect deadline enforcement and fd lifecycle during connection establishment.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn a_deadline_sentinel_is_never_retried_as_stale_eintr() {
    assert!(!retry_interrupted_recv(
        HTTP_READ_DEADLINE as isize,
        libc::EINTR,
    ));
    assert!(retry_interrupted_recv(-1, libc::EINTR));
}

#[test]
fn an_absolute_read_deadline_beats_the_socket_inactivity_timeout() {
    use std::os::fd::AsRawFd;
    let (reader, _silent_peer) = std::os::unix::net::UnixStream::pair().expect("socket pair");
    let mut byte = 0u8;
    let started = Instant::now();
    let deadline = started + std::time::Duration::from_millis(80);
    let r = unsafe {
        recv_until(
            reader.as_raw_fd(),
            &mut byte as *mut u8 as *mut c_void,
            1,
            Some(deadline),
            &mut Pacer::new(&mut NoCheckpoint),
        )
    };
    let took = started.elapsed();
    assert_eq!(r, HTTP_READ_DEADLINE as isize);
    assert!(
        took >= std::time::Duration::from_millis(50)
            && took < std::time::Duration::from_secs(2),
        "absolute deadline took {took:?}"
    );
}

#[test]
fn a_typed_open_reports_the_header_deadline_that_stopped_it() {
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (_silent_peer, _) = srv.accept().expect("accept");
        let _ = release_rx.recv();
    });

    let host = std::ffi::CString::new("127.0.0.1").unwrap();
    let path = std::ffi::CString::new("/stall").unwrap();
    let mut hs = http_stream_boxed();
    let started = Instant::now();
    let result = http_open_until_result(
        &mut *hs,
        host.as_ptr(),
        port as c_int,
        path.as_ptr(),
        std::ptr::null(),
        "GET",
        started + std::time::Duration::from_millis(80),
        &mut NoCheckpoint,
    );
    let took = started.elapsed();

    let _ = release_tx.send(());
    server.join().unwrap();
    assert_eq!(result, Err(HttpOpenError::Deadline));
    assert_eq!(
        hs.fd(),
        -1,
        "a deadline failure must retire the published fd"
    );
    assert!(
        took >= std::time::Duration::from_millis(50)
            && took < std::time::Duration::from_secs(2),
        "typed open deadline took {took:?}"
    );
}

/// Regression: `connect(2)` was called blocking with no deadline, so an unreachable PMS
/// froze the 60fps main loop for the kernel's SYN-retry budget (~2 min), once per request.
/// 192.0.2.0/24 is TEST-NET-1 (RFC 5737) — guaranteed non-routable, so the handshake can
/// never complete and the only thing that can end this call is the timeout. Port 32400 is
/// the PMS port this client actually dials; :80 can complete locally through an HTTP
/// interceptor without the address being reachable.
#[test]
fn connect_to_a_black_hole_gives_up_on_the_deadline() {
    let sa = sockaddr([192, 0, 2, 1], 32400);
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    assert!(fd >= 0);
    let t0 = Instant::now();
    let r = unsafe { connect_v4(fd, &sa, 300) };
    let waited = t0.elapsed();
    unsafe { libc::close(fd) };
    assert_eq!(r, -1, "an unroutable host must fail, not connect");
    assert!(
        waited.as_millis() < 3_000,
        "took {waited:?} — the deadline is not being honoured"
    );
}

/// A refused connection must be reported immediately, not waited out: port 1 on loopback
/// has no listener, so the kernel answers RST within the first poll.
#[test]
fn a_refused_connection_fails_fast_and_is_not_reported_as_success() {
    let sa = sockaddr([127, 0, 0, 1], 1);
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    assert!(fd >= 0);
    let t0 = Instant::now();
    let r = unsafe { connect_v4(fd, &sa, 5_000) };
    let waited = t0.elapsed();
    unsafe { libc::close(fd) };
    assert_eq!(
        r, -1,
        "SO_ERROR must be consulted — a writable socket is not a connected one"
    );
    assert!(
        waited.as_millis() < 2_000,
        "a refusal should be immediate, waited {waited:?}"
    );
}

/// A failed `http_open` must leave the stream CLOSED (fd = -1) and leak no descriptor,
/// whichever early exit it took. This is what makes `http_stream_boxed`'s fd = -1 contract
/// hold end-to-end, and it is the invariant a future interruptible-open design has to keep.
#[test]
fn every_failed_open_retires_its_fd_and_leaks_nothing() {
    let ip_refused = std::ffi::CString::new("127.0.0.1").unwrap(); // nothing listens on :1
    let path = std::ffi::CString::new("/x").unwrap();

    // The first case used to be the dotted quad `999.1.2.3`, rejected by the hand parse after
    // `socket()`. That parse is gone, and sending the same string on would have made this
    // OFFLINE suite do a DNS lookup: `999.1.2.3` is not an address, so it goes to the resolver
    // as a NAME — where a network with NXDOMAIN hijacking answers it with a real web server
    // (and the open then SUCCEEDS, failing this test), and a network with a dead resolver
    // spends glibc's whole `timeout × attempts × nameservers` budget inside a ~0.3 s suite.
    // An out-of-range port is the same early exit — refused in `resolve`, before any socket —
    // and it cannot leave the machine.
    for (label, ip, port) in [
        ("port out of range", &ip_refused, 70_000),
        ("refused connection", &ip_refused, 1),
    ] {
        let mut hs = http_stream_boxed();
        let rv = http_open(
            &mut *hs,
            ip.as_ptr(),
            port,
            path.as_ptr(),
            std::ptr::null(),
            "GET",
        );
        assert_eq!(rv, -1, "{label}: open must fail");
        assert_eq!(
            hs.fd(),
            -1,
            "{label}: the fd must be retired, not left published"
        );
    }

    // …and the descriptor is genuinely closed, not merely un-published.
    //
    // The slack is deliberately loose, because `open_fd_count` is PROCESS-wide and this suite
    // runs in parallel: the sibling socket tests (loopback listeners, the two `ff.rs` counting
    // accepts) hold descriptors open across this window, so a strict +2 made the assertion fire
    // on their scheduling rather than on a leak — it was already failing ~1 run in 6 before this
    // branch and got worse as the suite grew, which is a red gate that says nothing. What is
    // being detected is 32 leaked sockets; anything under a handful is other tests, and the two
    // are three quarters of an order of magnitude apart.
    let before = open_fd_count();
    for _ in 0..32 {
        let mut hs = http_stream_boxed();
        let _ = http_open(
            &mut *hs,
            ip_refused.as_ptr(),
            1,
            path.as_ptr(),
            std::ptr::null(),
            "GET",
        );
    }
    let after = open_fd_count();
    assert!(
        after <= before + 8,
        "failed opens leaked descriptors: {before} -> {after}"
    );
}

/// The claim the whole single-closer protocol rests on: `shutdown(2)` wakes a peer that is
/// already blocked in `recv`, which is what the interrupt sites need. (`close(2)` does not —
/// that is why BACK during a stall used to wait out the 15 s SO_RCVTIMEO.) Two threads, a
/// real loopback socket, no mocking.
#[test]
fn shutdown_wakes_a_reader_that_is_already_blocked_in_recv() {
    use std::sync::mpsc;
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    let sa = sockaddr([127, 0, 0, 1], port);
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    assert_eq!(unsafe { connect_v4(fd, &sa, 2_000) }, 0);
    let _peer = srv.accept().expect("accept"); // held open: nothing will ever be sent
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut b = [0u8; 16];
        // blocks here until the socket is shut down (the peer never writes)
        let r = unsafe { libc::recv(fd, b.as_mut_ptr() as *mut c_void, b.len(), 0) };
        let _ = tx.send(r);
    });
    // give the reader time to actually enter recv, then interrupt it
    std::thread::sleep(std::time::Duration::from_millis(150));
    unsafe { libc::shutdown(fd, libc::SHUT_RDWR) };
    let r = rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .expect("shutdown did not wake the blocked recv");
    assert_eq!(r, 0, "a shut-down socket must report EOF");
    reader.join().unwrap();
    unsafe { libc::close(fd) };
}

/// The point of publishing the fd at `socket()`: an open that stalls is now interruptible.
/// A listener that accepts and never answers puts `http_open` in its header `recv`, where it
/// would otherwise sit out the full 15 s `SO_RCVTIMEO` — and the main thread waits on that in
/// `teardown`'s join, which is the freeze this whole change exists to remove. The interrupt
/// must also leave the stream RETIRED, not merely woken: a stale fd left in the atomic is one
/// the next `http_shutdown` would shoot after the number had been recycled.
#[test]
fn an_open_stalled_in_the_header_read_is_interruptible() {
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    let ip = std::ffi::CString::new("127.0.0.1").unwrap();
    let path = std::ffi::CString::new("/stall").unwrap();

    let mut hs = http_stream_boxed();
    let addr = (&mut *hs) as *mut HttpStream as usize; // raw ptr isn't Send; the box outlives the scope
    let t0 = Instant::now();
    let (rv, waited) = std::thread::scope(|sc| {
        let opener = sc.spawn(move || {
            let rv = http_open_until_result(
                addr as *mut HttpStream,
                ip.as_ptr(),
                port as c_int,
                path.as_ptr(),
                std::ptr::null(),
                "GET",
                t0 + std::time::Duration::from_secs(10),
                &mut NoCheckpoint,
            );
            (rv, t0.elapsed())
        });
        let _peer = srv.accept().expect("accept"); // held open, never written to
        std::thread::sleep(std::time::Duration::from_millis(200)); // let it reach the recv
        http_shutdown(addr as *mut HttpStream);
        opener.join().unwrap()
    });

    assert_eq!(
        rv,
        Err(HttpOpenError::Aborted),
        "an interrupted open must preserve the abort cause"
    );
    assert!(
        waited.as_secs() < 3,
        "took {waited:?} — the open sat out SO_RCVTIMEO, so it was NOT interrupted"
    );
    assert_eq!(
        hs.fd(),
        -1,
        "the interrupted open left its fd published — that is the stale \
                             descriptor a later http_shutdown would shoot"
    );
}

/// `take_fd` is the single-closer gate: concurrent claimers must produce exactly one
/// winner, so a descriptor can never be closed twice (and so never recycled underneath
/// a thread still using it).
#[test]
fn exactly_one_caller_can_claim_the_fd() {
    let hs = http_stream_boxed();
    hs.set_fd(4242);
    let winners: i32 = std::thread::scope(|sc| {
        let hs = &hs;
        let hs2 = (0..8)
            .map(|_| sc.spawn(move || i32::from(hs.take_fd() >= 0)))
            .collect::<Vec<_>>();
        hs2.into_iter().map(|h| h.join().unwrap()).sum()
    });
    assert_eq!(
        winners, 1,
        "the fd was claimed {winners} times — that is a double close"
    );
    assert!(hs.fd() < 0, "the slot must be left closed");
}

/// The happy path still connects, and — the part that matters for every read below —
/// the socket is handed back in BLOCKING mode.
#[test]
fn a_live_listener_connects_and_the_socket_is_left_blocking() {
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    let sa = sockaddr([127, 0, 0, 1], port);
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    assert!(fd >= 0);
    let r = unsafe { connect_v4(fd, &sa, 2_000) };
    assert_eq!(r, 0, "a listening socket must connect");
    let fl = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    assert_eq!(
        fl & libc::O_NONBLOCK,
        0,
        "O_NONBLOCK leaked out of the handshake"
    );
    unsafe { libc::close(fd) };
}
