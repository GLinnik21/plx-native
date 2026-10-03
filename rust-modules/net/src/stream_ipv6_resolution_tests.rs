//! Address-family resolution: v4/v6 literals, hostnames, and multi-address chains.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// The v6 sibling of the two connect tests above, on a real AF_INET6 socket: a live `::1`
/// listener connects and is handed back BLOCKING (every read path below depends on that, and
/// `connect_timeout` restores the flags itself), and a `::1` port with no listener is refused
/// at once rather than waited out. Neither is inferrable from the v4 pair — a family this file
/// never opened before is exactly where a wrong `socklen_t` or a stray `sockaddr_in` cast shows
/// up, and it shows up as a `connect` that fails for a reason nothing logs.
#[test]
fn a_v6_listener_connects_blocking_and_a_v6_refusal_is_immediate() {
    let Some(srv) = v6_loopback_or_skip("the AF_INET6 connect pair") else {
        return;
    };
    let port = srv.local_addr().unwrap().port();

    let sa = sockaddr6(std::net::Ipv6Addr::LOCALHOST, port);
    let fd = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_STREAM, 0) };
    assert!(
        fd >= 0,
        "AF_INET6 sockets must be creatable — the loopback bound above"
    );
    assert_eq!(
        unsafe { connect_v6(fd, &sa, 2_000) },
        0,
        "a listening ::1 socket must connect"
    );
    let fl = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    assert_eq!(
        fl & libc::O_NONBLOCK,
        0,
        "O_NONBLOCK leaked out of the v6 handshake"
    );
    unsafe { libc::close(fd) };

    let sa = sockaddr6(std::net::Ipv6Addr::LOCALHOST, 1); // nothing listens on ::1:1
    let fd = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_STREAM, 0) };
    let t0 = Instant::now();
    let r = unsafe { connect_v6(fd, &sa, 5_000) };
    let waited = t0.elapsed();
    unsafe { libc::close(fd) };
    assert_eq!(
        r, -1,
        "SO_ERROR must be consulted on v6 too — a writable socket is not connected"
    );
    assert!(
        waited.as_millis() < 2_000,
        "a refusal should be immediate, waited {waited:?}"
    );
}

/// The bracket asymmetry, which is the single easiest thing to get backwards here: the URI
/// authority carries them (RFC 3986 §3.2.2, and so `Host:` does — RFC 9110 §7.2), the RESOLVER
/// does not. `getaddrinfo("[::1]", …)` is EAI_NONAME, so passing the authority form through
/// would make every IPv6 server read as "does not resolve".
#[test]
fn a_v6_literal_is_bracketed_for_the_host_header_and_bare_for_the_resolver() {
    assert_eq!(resolver_node("[2001:db8::1]"), "2001:db8::1");
    assert_eq!(
        resolver_node("2001:db8::1"),
        "2001:db8::1",
        "already bare: unchanged"
    );
    assert_eq!(resolver_node("nas.local"), "nas.local");
    assert_eq!(resolver_node("192.0.2.10"), "192.0.2.10");

    assert_eq!(
        host_header("2001:db8::1", 32400),
        "[2001:db8::1]:32400",
        "a bare v6 literal must be bracketed for the authority"
    );
    assert_eq!(
        host_header("[2001:db8::1]", 32400),
        "[2001:db8::1]:32400",
        "…and one that arrived bracketed must not be double-bracketed"
    );
    assert_eq!(host_header("nas.local", 32400), "nas.local:32400");
    assert_eq!(host_header("192.0.2.10", 32400), "192.0.2.10:32400");
    assert_eq!(host_header("::1", 80), "[::1]:80");
}

/// Which `getaddrinfo` flag a host takes turns on this, and getting it wrong is silent: a
/// literal misfiled as a name goes to DNS (and, under `AI_ADDRCONFIG`, can resolve to nothing
/// at all), while a name misfiled as a literal fails outright under `AI_NUMERICHOST`.
#[test]
fn an_address_literal_is_told_apart_from_a_name() {
    for a in [
        "127.0.0.1",
        "192.0.2.10",
        "::1",
        "2001:db8::1",
        "fe80::1%en0",
    ] {
        assert!(is_numeric_host(a), "{a} is an address literal");
    }
    for n in [
        "nas.local",
        "plex.example.org",
        "localhost",
        "999.1.2.3",
        "1.2.3",
        "1.2.3.4.5",
    ] {
        assert!(!is_numeric_host(n), "{n} is not an address literal");
    }
}

/// `AI_ADDRCONFIG` suppresses AF_INET6 results on a host whose only IPv6 address is loopback —
/// which is most developer machines — so a v6 LITERAL must not be resolved under it. This is
/// the assertion behind `resolve`'s flag split; without it `::1` resolves to nothing on a
/// perfectly healthy machine and every v6 case below fails for a reason that looks like ours.
/// Both of these are purely local: `AI_NUMERICHOST` sends no packet and loads no NSS module.
#[test]
fn an_address_literal_resolves_without_a_resolver() {
    assert!(
        unsafe { resolve("127.0.0.1", 80) }.is_some(),
        "a v4 literal must resolve"
    );
    assert!(
        unsafe { resolve("::1", 80) }.is_some(),
        "a v6 literal must resolve — if this fails, AI_ADDRCONFIG leaked onto a literal"
    );
    assert!(
        unsafe { resolve("2001:db8::1", 80) }.is_some(),
        "a non-loopback v6 literal too"
    );

    // An out-of-range port FAILS instead of wrapping into a plausible one — 70000 used to dial
    // 4464. Note what this is asserting: `resolve`'s OWN range check, not `AI_NUMERICSERV`'s.
    // Darwin rejects the service string and glibc does not, so had this been left to the
    // resolver the assertion would have passed here and the app would still have truncated on
    // the television. It is the shape this file's own notes warn about — a green host run about
    // a platform difference — and it was caught in review, not by the suite.
    assert!(
        unsafe { resolve("127.0.0.1", 70_000) }.is_none(),
        "an out-of-range port must fail, not truncate into a dialable one"
    );
    assert!(
        unsafe { resolve("127.0.0.1", 65_536) }.is_none(),
        "…one past the top"
    );
    assert!(
        unsafe { resolve("127.0.0.1", -1) }.is_none(),
        "…nor a negative one"
    );
    assert!(
        unsafe { resolve("127.0.0.1", 65_535) }.is_some(),
        "…and the top itself is fine"
    );
}

/// IPv6 end to end, which is checklist #43 CASE2: a listener on `::1`, an AF_INET6 socket
/// opened because the RESOLVER said so, a 200 read back, and — the half a connect alone cannot
/// show — a bracketed `Host:` on the wire. Both spellings of the host reach the same server,
/// since `plex::probe::host_of` hands back the bracketed one.
#[test]
fn a_v6_literal_connects_and_sends_a_bracketed_host_header() {
    let Some(listener) = v6_loopback_or_skip("the IPv6 end-to-end open") else {
        return;
    };
    drop(listener); // proven bindable; `one_shot_echo` needs the address for itself

    for host in ["::1", "[::1]"] {
        let (port, h) = one_shot_echo(
            "[::1]:0",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi".to_vec(),
        )
        .expect("bind ::1");
        let (mut hs, rv) = open_host_against(host, port);
        assert_eq!(rv, 0, "{host}: an IPv6 server must open");
        assert_eq!(hs.status, 200, "{host}");

        let mut buf = [0u8; 8];
        let n = http_read(&mut *hs, buf.as_mut_ptr(), buf.len() as c_int);
        assert_eq!(
            &buf[..n.max(0) as usize],
            b"hi",
            "{host}: the body must come back intact"
        );
        http_close(&mut *hs);

        let req = h.join().unwrap();
        assert_eq!(
            host_line(&req),
            format!("Host: [::1]:{port}"),
            "{host}: the authority form is bracketed whichever spelling was handed in"
        );
    }
}

/// A NAME, which is the other half of the limitation being removed — and the three things that
/// have to hold at once for one to work. `localhost` resolves to both families on every machine
/// this runs on, while the listener is bound to 127.0.0.1 ONLY, so on a host that offers `::1`
/// first this only passes by WALKING past a refused address to a live one.
///
/// And `Host:` must carry the name. Sending the address it resolved to instead is what breaks
/// name-based virtual hosting, and it is invisible from the connect: the TCP session is
/// identical either way.
#[test]
fn a_hostname_resolves_and_the_host_header_carries_the_name_not_the_address() {
    let (port, h) = one_shot_echo(
        "127.0.0.1:0",
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
    )
    .expect("bind 127.0.0.1");
    let (mut hs, rv) = open_host_against("localhost", port);
    assert_eq!(
        rv, 0,
        "a name the system resolves must open — this is the whole DNS gap"
    );
    assert_eq!(hs.status, 200);
    http_close(&mut *hs);

    let req = h.join().unwrap();
    assert_eq!(
        host_line(&req),
        format!("Host: localhost:{port}"),
        "the Host header is the ORIGIN; a resolved address here breaks vhosting"
    );
}

/// The v4 literal path still says what it always said — the regression guard for every existing
/// caller, all of which hand `http_open` a dotted quad.
#[test]
fn a_v4_literal_still_sends_its_own_address_as_the_host_header() {
    let (port, h) = one_shot_echo(
        "127.0.0.1:0",
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
    )
    .expect("bind");
    let (mut hs, rv) = open_against(port);
    assert_eq!(rv, 0);
    http_close(&mut *hs);
    assert_eq!(
        host_line(&h.join().unwrap()),
        format!("Host: 127.0.0.1:{port}")
    );
}

/// Resolving to several addresses and dialling only the first is the old single-address limit
/// wearing a resolver. The chain here is built by hand rather than resolved, because which
/// addresses a name yields and in what order is the machine's business and not something a test
/// can arrange: a REFUSED v6 loopback port first, a live v4 listener second. It is also a
/// mixed-family chain on purpose — the socket family comes from each node, so a walk that
/// assumed AF_INET would open the wrong socket for the first, and one that assumed AF_INET6
/// would open the wrong socket for the second.
#[test]
fn the_whole_address_chain_is_walked_until_one_connects() {
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();

    let mut dead = sockaddr6(std::net::Ipv6Addr::LOCALHOST, 1); // nothing listens on ::1:1
    let mut live = sockaddr([127, 0, 0, 1], port);
    let mut second = ainfo(
        libc::AF_INET,
        &mut live as *mut _ as *mut libc::sockaddr,
        std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        std::ptr::null_mut(),
    );
    let first = ainfo(
        libc::AF_INET6,
        &mut dead as *mut _ as *mut libc::sockaddr,
        std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
        &mut second,
    );

    let hs = http_stream_boxed();
    let fd = unsafe { connect_any(&hs, &first, 2_000) };
    assert!(
        fd >= 0,
        "the walk stopped at the first dead address instead of trying the second"
    );
    assert_eq!(
        hs.fd(),
        fd,
        "the connected fd must be left PUBLISHED for http_shutdown to reach"
    );
    let _peer = srv
        .accept()
        .expect("the live address must actually have been dialled");
    unsafe { close_owned(&hs) };
}

/// …and a chain with nothing live fails as one failure, leaving no descriptor behind: every
/// attempt has to be retired through `close_owned`, not bare-closed and not simply abandoned.
#[test]
fn a_chain_with_no_live_address_fails_closed_and_leaks_nothing() {
    let before = open_fd_count();
    for _ in 0..64 {
        let mut a = sockaddr([127, 0, 0, 1], 1);
        let mut b = sockaddr([127, 0, 0, 1], 1);
        let mut second = ainfo(
            libc::AF_INET,
            &mut b as *mut _ as *mut libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            std::ptr::null_mut(),
        );
        let first = ainfo(
            libc::AF_INET,
            &mut a as *mut _ as *mut libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            &mut second,
        );
        let hs = http_stream_boxed();
        assert_eq!(
            unsafe { connect_any(&hs, &first, 2_000) },
            -1,
            "nothing here can connect"
        );
        assert_eq!(
            hs.fd(),
            -1,
            "a spent walk must leave the stream CLOSED, not published"
        );
    }
    // Same slack, and the same reason, as `every_failed_open_retires_its_fd_and_leaks_nothing`:
    // `open_fd_count` is PROCESS-wide and this suite runs in parallel, so the sibling socket
    // tests hold descriptors open across this window and the reading drifts by a handful either
    // way — measured at +9 against a first draft that allowed +8, on a run with nothing wrong.
    // The separation is what makes the gate mean something rather than the tightness: 64 rounds
    // of a two-address walk leak 128 descriptors if a single `close_owned` is missed, which is
    // most of an order of magnitude clear of the noise.
    let after = open_fd_count();
    assert!(
        after <= before + 24,
        "the walk leaked descriptors: {before} -> {after}"
    );
}

/// A teardown mid-open must not be ANSWERED by dialling the next address — that would consume
/// the interrupt and hand a caller being torn down a brand-new connection, quietly undoing the
/// interruptibility the publish-before-connect invariant exists to give.
///
/// The latch is armed here instead of raced, deliberately: `shutdown(2)` aborting a handshake in
/// progress is TRUE on the TV's kernel and NOT on the Darwin host these tests run on
/// (`tools/sockprobe.c`), so timing the real interrupt would be asserting the host's behaviour.
/// What is portable, and what actually decides the outcome, is the branch — a failed attempt
/// plus a latched interrupt stops the walk — and `http_shutdown` is the real API that arms it,
/// including with the fd already retired, which is the between-attempts window.
#[test]
fn an_interrupted_walk_does_not_dial_the_next_address() {
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    srv.set_nonblocking(true).expect("nonblocking accept");
    let port = srv.local_addr().unwrap().port();

    let mut dead = sockaddr([127, 0, 0, 1], 1);
    let mut live = sockaddr([127, 0, 0, 1], port);
    let mut second = ainfo(
        libc::AF_INET,
        &mut live as *mut _ as *mut libc::sockaddr,
        std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        std::ptr::null_mut(),
    );
    let first = ainfo(
        libc::AF_INET,
        &mut dead as *mut _ as *mut libc::sockaddr,
        std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        &mut second,
    );

    let mut hs = http_stream_boxed();
    http_shutdown(&mut *hs); // a teardown with no descriptor to shoot: the latch is the point
    assert!(
        hs.interrupted(),
        "http_shutdown must latch even when the fd is already -1"
    );

    assert_eq!(
        unsafe { connect_any(&hs, &first, 2_000) },
        -1,
        "an interrupted walk fails"
    );
    assert_eq!(hs.fd(), -1);
    assert!(
        matches!(srv.accept(), Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "the second address was dialled anyway — the teardown was answered with a connection"
    );
}

/// A leftover interrupt with fd already -1 is still THIS teardown, not a later session.
/// Consuming it let `http_open` connect under join after the caller's AU-abort check.
/// Production boxes a fresh `HttpStream` per engine; that new box can still dial.
#[test]
fn an_interrupt_with_fd_closed_does_not_dial() {
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    srv.set_nonblocking(true).expect("nonblocking accept");
    let port = srv.local_addr().unwrap().port();
    let mut hs = http_stream_boxed();
    http_shutdown(&mut *hs);
    assert!(hs.interrupted());
    assert!(hs.fd() < 0);

    let ip = std::ffi::CString::new("127.0.0.1").unwrap();
    let path = std::ffi::CString::new("/x").unwrap();
    let rv = http_open(
        &mut *hs,
        ip.as_ptr(),
        port as c_int,
        path.as_ptr(),
        std::ptr::null(),
        "GET",
    );
    assert_ne!(
        rv, 0,
        "teardown with fd already -1 must not connect under join"
    );
    assert!(
        matches!(srv.accept(), Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "http_open dialled a socket the already-fired shutdown cannot reach"
    );
    assert!(hs.interrupted(), "the latch belongs to this box");

    let mut next = http_stream_boxed();
    let (port, h) = one_shot_server(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec());
    let ip = std::ffi::CString::new("127.0.0.1").unwrap();
    let rv = http_open(
        &mut *next,
        ip.as_ptr(),
        port as c_int,
        path.as_ptr(),
        std::ptr::null(),
        "GET",
    );
    assert_eq!(rv, 0, "a fresh engine box must still be able to dial");
    http_close(&mut *next);
    h.join().unwrap();
}
