//! Keep-alive connection reuse, redial-on-idle-close, and HTTP/1.0 semantics.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn two_drained_gets_reuse_one_accept() {
    with_keepalive_listener(
        |port, accepts, requests| {
            let host = std::ffi::CString::new("127.0.0.1").unwrap();
            let path = std::ffi::CString::new("/seg").unwrap();
            let mut hs = http_stream_boxed();
            assert_eq!(
                http_open(
                    &mut *hs,
                    host.as_ptr(),
                    port as c_int,
                    path.as_ptr(),
                    std::ptr::null(),
                    "GET",
                ),
                0
            );
            drain_body(&mut *hs);
            assert_eq!(
                http_open(
                    &mut *hs,
                    host.as_ptr(),
                    port as c_int,
                    path.as_ptr(),
                    std::ptr::null(),
                    "GET",
                ),
                0
            );
            drain_body(&mut *hs);
            assert_eq!(
                accepts.load(std::sync::atomic::Ordering::Acquire),
                1,
                "a drained HTTP/1.1 body must reuse the live fd"
            );
            assert_eq!(
                requests.load(std::sync::atomic::Ordering::Acquire),
                2,
                "reuse is a second request on the same accept"
            );
            http_close(&mut *hs);
        },
        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCDEFGH",
    );
}

#[test]
fn an_unread_body_does_not_reuse_the_fd() {
    with_keepalive_listener(
        |port, accepts, _requests| {
            let host = std::ffi::CString::new("127.0.0.1").unwrap();
            let path = std::ffi::CString::new("/seg").unwrap();
            let mut hs = http_stream_boxed();
            assert_eq!(
                http_open(
                    &mut *hs,
                    host.as_ptr(),
                    port as c_int,
                    path.as_ptr(),
                    std::ptr::null(),
                    "GET",
                ),
                0
            );
            assert_eq!(
                http_open(
                    &mut *hs,
                    host.as_ptr(),
                    port as c_int,
                    path.as_ptr(),
                    std::ptr::null(),
                    "GET",
                ),
                0
            );
            assert_eq!(
                accepts.load(std::sync::atomic::Ordering::Acquire),
                2,
                "an unread body must not be followed by a pipelined request"
            );
            http_close(&mut *hs);
        },
        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCDEFGH",
    );
}

#[test]
fn connection_close_forces_a_new_accept() {
    with_keepalive_listener(
        |port, accepts, requests| {
            let host = std::ffi::CString::new("127.0.0.1").unwrap();
            let path = std::ffi::CString::new("/seg").unwrap();
            let mut hs = http_stream_boxed();
            assert_eq!(
                http_open(
                    &mut *hs,
                    host.as_ptr(),
                    port as c_int,
                    path.as_ptr(),
                    std::ptr::null(),
                    "GET",
                ),
                0
            );
            drain_body(&mut *hs);
            assert_eq!(
                http_open(
                    &mut *hs,
                    host.as_ptr(),
                    port as c_int,
                    path.as_ptr(),
                    std::ptr::null(),
                    "GET",
                ),
                0
            );
            drain_body(&mut *hs);
            assert_eq!(
                accepts.load(std::sync::atomic::Ordering::Acquire),
                2,
                "Connection: close must retire the fd"
            );
            assert_eq!(requests.load(std::sync::atomic::Ordering::Acquire), 2);
            http_close(&mut *hs);
        },
        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nABCDEFGH",
    );
}

#[test]
fn two_drained_chunked_gets_reuse_one_accept() {
    with_keepalive_listener(
        |port, accepts, requests| {
            let host = std::ffi::CString::new("127.0.0.1").unwrap();
            let path = std::ffi::CString::new("/seg").unwrap();
            let mut hs = http_stream_boxed();
            assert_eq!(
                http_open(
                    &mut *hs,
                    host.as_ptr(),
                    port as c_int,
                    path.as_ptr(),
                    std::ptr::null(),
                    "GET",
                ),
                0
            );
            drain_body(&mut *hs);
            assert_eq!(
                http_open(
                    &mut *hs,
                    host.as_ptr(),
                    port as c_int,
                    path.as_ptr(),
                    std::ptr::null(),
                    "GET",
                ),
                0
            );
            drain_body(&mut *hs);
            assert_eq!(
                accepts.load(std::sync::atomic::Ordering::Acquire),
                1,
                "a drained chunked body must consume trailers so the next GET can reuse"
            );
            assert_eq!(requests.load(std::sync::atomic::Ordering::Acquire), 2);
            http_close(&mut *hs);
        },
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nABCDEFGH\r\n0\r\n\r\n",
    );
}

#[test]
fn shutdown_of_a_live_keepalive_fd_does_not_dial_a_replacement() {
    with_keepalive_listener(
        |port, accepts, _requests| {
            let host = std::ffi::CString::new("127.0.0.1").unwrap();
            let path = std::ffi::CString::new("/seg").unwrap();
            let mut hs = http_stream_boxed();
            assert_eq!(
                http_open(
                    &mut *hs,
                    host.as_ptr(),
                    port as c_int,
                    path.as_ptr(),
                    std::ptr::null(),
                    "GET",
                ),
                0
            );
            drain_body(&mut *hs);
            http_shutdown(&mut *hs);
            assert_ne!(
                http_open(
                    &mut *hs,
                    host.as_ptr(),
                    port as c_int,
                    path.as_ptr(),
                    std::ptr::null(),
                    "GET",
                ),
                0,
                "teardown must not be answered with a fresh connect"
            );
            assert_eq!(
                accepts.load(std::sync::atomic::Ordering::Acquire),
                1,
                "the already-fired shutdown cannot reach a replacement socket"
            );
            assert_ne!(
                http_open(
                    &mut *hs,
                    host.as_ptr(),
                    port as c_int,
                    path.as_ptr(),
                    std::ptr::null(),
                    "GET",
                ),
                0,
                "the same box stays torn down; a later engine session is a new HttpStream"
            );
            let mut next = http_stream_boxed();
            assert_eq!(
                http_open(
                    &mut *next,
                    host.as_ptr(),
                    port as c_int,
                    path.as_ptr(),
                    std::ptr::null(),
                    "GET",
                ),
                0,
                "a fresh engine box must still be able to dial"
            );
            assert_eq!(accepts.load(std::sync::atomic::Ordering::Acquire), 2);
            http_close(&mut *next);
            http_close(&mut *hs);
        },
        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCDEFGH",
    );
}

#[test]
fn a_media_get_omits_connection_close() {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    std::thread::scope(|sc| {
        sc.spawn(|| {
            let Ok((mut s, _)) = srv.accept() else { return };
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).unwrap_or(0);
            assert!(
                !buf[..n]
                    .windows(b"Connection: close".len())
                    .any(|w| w.eq_ignore_ascii_case(b"connection: close")),
                "media sequential GETs must omit Connection: close so the fd can reuse"
            );
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCDEFGH");
        });
        let host = std::ffi::CString::new("127.0.0.1").unwrap();
        let path = std::ffi::CString::new("/seg").unwrap();
        let mut hs = http_stream_boxed();
        assert_eq!(
            http_open(
                &mut *hs,
                host.as_ptr(),
                port as c_int,
                path.as_ptr(),
                std::ptr::null(),
                "GET",
            ),
            0
        );
        http_close(&mut *hs);
    });
}

#[test]
fn shutdown_after_an_idle_peer_close_does_not_redial() {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    let accepts = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|sc| {
        sc.spawn(|| {
            let Ok((mut s, _)) = srv.accept() else { return };
            accepts.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCDEFGH");
            let _ = s.shutdown(std::net::Shutdown::Both);
            // Stay listening long enough that a Transport redial would show up as a second
            // accept. A correct abort must not connect at all.
            let _ = srv.set_nonblocking(true);
            std::thread::sleep(std::time::Duration::from_millis(150));
            if srv.accept().is_ok() {
                accepts.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            }
        });
        let host = std::ffi::CString::new("127.0.0.1").unwrap();
        let path = std::ffi::CString::new("/seg").unwrap();
        let mut hs = http_stream_boxed();
        assert_eq!(
            http_open(
                &mut *hs,
                host.as_ptr(),
                port as c_int,
                path.as_ptr(),
                std::ptr::null(),
                "GET",
            ),
            0
        );
        drain_body(&mut *hs);
        http_shutdown(&mut *hs);
        assert_ne!(
            http_open(
                &mut *hs,
                host.as_ptr(),
                port as c_int,
                path.as_ptr(),
                std::ptr::null(),
                "GET",
            ),
            0,
            "a teardown on a half-closed keep-alive must not redial"
        );
        assert_eq!(
            accepts.load(std::sync::atomic::Ordering::Acquire),
            1,
            "the already-fired shutdown cannot reach a replacement socket"
        );
        http_close(&mut *hs);
    });
}

#[test]
fn teardown_during_keepalive_transport_redial_does_not_dial() {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    let accepts = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|sc| {
        sc.spawn(|| {
            let Ok((mut s, _)) = srv.accept() else { return };
            accepts.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCDEFGH");
            // Half-close so the next open's reuse send/recv fails Transport. Stay listening
            // long enough that a redial after wiping the latch would show up as accept 2.
            let _ = s.shutdown(std::net::Shutdown::Both);
            let _ = srv.set_nonblocking(true);
            std::thread::sleep(std::time::Duration::from_millis(150));
            if srv.accept().is_ok() {
                accepts.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            }
        });
        let host = std::ffi::CString::new("127.0.0.1").unwrap();
        let path = std::ffi::CString::new("/seg").unwrap();
        let mut hs = http_stream_boxed();
        assert_eq!(
            http_open(
                &mut *hs,
                host.as_ptr(),
                port as c_int,
                path.as_ptr(),
                std::ptr::null(),
                "GET",
            ),
            0
        );
        drain_body(&mut *hs);
        http_shutdown(&mut *hs);
        assert_ne!(
            http_open(
                &mut *hs,
                host.as_ptr(),
                port as c_int,
                path.as_ptr(),
                std::ptr::null(),
                "GET",
            ),
            0,
            "Transport redial must not consume a teardown latch and dial a replacement"
        );
        assert_eq!(
            accepts.load(std::sync::atomic::Ordering::Acquire),
            1,
            "the already-fired shutdown cannot reach a replacement socket"
        );
        http_close(&mut *hs);
    });
}

#[test]
fn an_idle_peer_close_redials_instead_of_failing_the_next_get() {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    let accepts = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|sc| {
        sc.spawn(|| {
            for _ in 0..2 {
                let Ok((mut s, _)) = srv.accept() else { return };
                accepts.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCDEFGH");
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        });
        let host = std::ffi::CString::new("127.0.0.1").unwrap();
        let path = std::ffi::CString::new("/seg").unwrap();
        let mut hs = http_stream_boxed();
        assert_eq!(
            http_open(
                &mut *hs,
                host.as_ptr(),
                port as c_int,
                path.as_ptr(),
                std::ptr::null(),
                "GET",
            ),
            0
        );
        drain_body(&mut *hs);
        assert_eq!(
            http_open(
                &mut *hs,
                host.as_ptr(),
                port as c_int,
                path.as_ptr(),
                std::ptr::null(),
                "GET",
            ),
            0,
            "a peer that dropped an otherwise reusable fd must redial, not fail"
        );
        drain_body(&mut *hs);
        assert_eq!(accepts.load(std::sync::atomic::Ordering::Acquire), 2);
        http_close(&mut *hs);
    });
}

#[test]
fn http_1_0_keep_alive_does_not_reuse_the_fd() {
    with_keepalive_listener(
        |port, accepts, _requests| {
            let host = std::ffi::CString::new("127.0.0.1").unwrap();
            let path = std::ffi::CString::new("/seg").unwrap();
            let mut hs = http_stream_boxed();
            assert_eq!(
                http_open(
                    &mut *hs,
                    host.as_ptr(),
                    port as c_int,
                    path.as_ptr(),
                    std::ptr::null(),
                    "GET",
                ),
                0
            );
            drain_body(&mut *hs);
            assert_eq!(
                http_open(
                    &mut *hs,
                    host.as_ptr(),
                    port as c_int,
                    path.as_ptr(),
                    std::ptr::null(),
                    "GET",
                ),
                0
            );
            drain_body(&mut *hs);
            assert_eq!(
                accepts.load(std::sync::atomic::Ordering::Acquire),
                2,
                "HTTP/1.0 Connection: keep-alive is unused; PMS is 1.1"
            );
            http_close(&mut *hs);
        },
        b"HTTP/1.0 200 OK\r\nContent-Length: 8\r\nConnection: keep-alive\r\n\r\nABCDEFGH",
    );
}
