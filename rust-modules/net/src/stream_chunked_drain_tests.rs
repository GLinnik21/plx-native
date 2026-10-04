//! Chunked trailer parsing and drain_available's partial-chunk/partial-trailer behavior.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn drain_available_returns_immediately_when_the_body_is_already_done() {
    with_keepalive_listener(
        |port, _, _| {
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
            let started = std::time::Instant::now();
            let mut dst = [0u8; 32];
            let n = http_drain_available(&mut *hs, &mut dst);
            assert!(n <= 0);
            assert!(
                started.elapsed() < std::time::Duration::from_millis(200),
                "drain must not wait out SO_RCVTIMEO on an already-finished body"
            );
            http_close(&mut *hs);
        },
        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCDEFGH",
    );
}

#[test]
fn named_chunked_trailers_reuse_one_accept() {
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
            assert_eq!(accepts.load(std::sync::atomic::Ordering::Acquire), 1);
            assert_eq!(requests.load(std::sync::atomic::Ordering::Acquire), 2);
            http_close(&mut *hs);
        },
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nABCDEFGH\r\n0\r\nExpires: never\r\n\r\n",
    );
}

#[test]
fn incomplete_chunked_trailers_do_not_reuse_the_fd() {
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
                let _ = s.write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nABCDEFGH\r\n0\r\nFoo: ",
                );
                let _ = s.shutdown(std::net::Shutdown::Write);
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
        let second = http_open(
            &mut *hs,
            host.as_ptr(),
            port as c_int,
            path.as_ptr(),
            std::ptr::null(),
            "GET",
        );
        assert_eq!(
            second, 0,
            "a poisoned keep-alive must redial, not parse trailers"
        );
        drain_body(&mut *hs);
        assert_eq!(
            accepts.load(std::sync::atomic::Ordering::Acquire),
            2,
            "incomplete trailers must not reuse the fd"
        );
        http_close(&mut *hs);
    });
}

#[test]
fn chunked_drain_completes_trailers_split_across_two_writes() {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    let accepts = std::sync::atomic::AtomicUsize::new(0);
    let first_drain = std::sync::Barrier::new(2);
    std::thread::scope(|sc| {
        sc.spawn(|| {
            let Ok((mut s, _)) = srv.accept() else { return };
            accepts.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nABCDEFGH\r\n0\r\nFoo: ",
            );
            first_drain.wait();
            let _ = s.write_all(b"bar\r\n\r\n");
            let _ = s.read(&mut buf);
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nABCDEFGH\r\n0\r\n\r\n",
            );
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
        let mut body = [0u8; 8];
        assert_eq!(http_read(&mut *hs, body.as_mut_ptr(), 8), 8);
        let started = std::time::Instant::now();
        let mut dst = [0u8; 32];
        let n = http_drain_available(&mut *hs, &mut dst);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "drain must not wait out SO_RCVTIMEO on a partial trailer"
        );
        assert!(n <= 0);
        assert!(
            !http_body_done(&mut *hs),
            "a split trailer must not mark the body done before the rest arrives"
        );
        first_drain.wait();
        let started = std::time::Instant::now();
        while !http_body_done(&mut *hs) {
            assert!(
                started.elapsed() < std::time::Duration::from_millis(200),
                "the second trailer fragment must complete without a blocking recv"
            );
            let n = http_drain_available(&mut *hs, &mut dst);
            assert!(n >= 0, "split trailers are not a transport error");
            if n == 0 {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
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
            "trailers split across two writes must still reuse the fd"
        );
        http_close(&mut *hs);
    });
}

#[test]
fn chunked_drain_completes_trailers_split_after_field_before_blank_line() {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    let accepts = std::sync::atomic::AtomicUsize::new(0);
    let first_drain = std::sync::Barrier::new(2);
    std::thread::scope(|sc| {
        sc.spawn(|| {
            let Ok((mut s, _)) = srv.accept() else { return };
            accepts.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nABCDEFGH\r\n0\r\nFoo: bar",
            );
            first_drain.wait();
            let _ = s.write_all(b"\r\n\r\n");
            let _ = s.read(&mut buf);
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nABCDEFGH\r\n0\r\n\r\n",
            );
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
        let mut body = [0u8; 8];
        assert_eq!(http_read(&mut *hs, body.as_mut_ptr(), 8), 8);
        let started = std::time::Instant::now();
        let mut dst = [0u8; 32];
        let n = http_drain_available(&mut *hs, &mut dst);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "drain must not wait out SO_RCVTIMEO on a partial trailer"
        );
        assert!(n <= 0);
        assert!(
            !http_body_done(&mut *hs),
            "splitting after Foo: bar and before the blank line must not finish the body"
        );
        first_drain.wait();
        let started = std::time::Instant::now();
        while !http_body_done(&mut *hs) {
            assert!(
                started.elapsed() < std::time::Duration::from_millis(200),
                "the blank-line fragment must complete without a blocking recv"
            );
            let n = http_drain_available(&mut *hs, &mut dst);
            assert!(n >= 0, "split trailers are not a transport error");
            if n == 0 {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
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
            "a trailer split after the field must still reuse the fd"
        );
        http_close(&mut *hs);
    });
}

#[test]
fn chunked_drain_returns_immediately_when_the_peer_stalls_after_a_partial_chunk() {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    let ready = std::sync::Barrier::new(2);
    std::thread::scope(|sc| {
        sc.spawn(|| {
            let Ok((mut s, _)) = srv.accept() else { return };
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nABCD");
            ready.wait();
            std::thread::sleep(std::time::Duration::from_millis(400));
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
        ready.wait();
        let started = std::time::Instant::now();
        let mut dst = [0u8; 32];
        let n = http_drain_available(&mut *hs, &mut dst);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "drain must not wait out SO_RCVTIMEO on a stalled chunked body"
        );
        assert!(n >= 0);
        http_close(&mut *hs);
    });
}

#[test]
fn chunked_drain_does_not_block_waiting_for_the_next_size_line() {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    std::thread::scope(|sc| {
        sc.spawn(|| {
            let Ok((mut s, _)) = srv.accept() else { return };
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nABCDEFGH\r\n",
            );
            std::thread::sleep(std::time::Duration::from_millis(400));
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
        let mut body = [0u8; 8];
        let got = http_read(&mut *hs, body.as_mut_ptr(), 8);
        assert_eq!(got, 8);
        let started = std::time::Instant::now();
        let mut dst = [0u8; 32];
        let n = http_drain_available(&mut *hs, &mut dst);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "drain must not block for the next chunk-size line"
        );
        assert!(n <= 0);
        http_close(&mut *hs);
    });
}

#[test]
fn mid_body_eof_is_a_transport_error() {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    std::thread::scope(|sc| {
        sc.spawn(|| {
            let Ok((mut s, _)) = srv.accept() else { return };
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\nABCDEFGH");
            let _ = s.shutdown(std::net::Shutdown::Write);
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
        let mut buf = [0u8; 16];
        let n = http_read(&mut *hs, buf.as_mut_ptr(), 8);
        assert_eq!(n, 8);
        let n = http_read(&mut *hs, buf.as_mut_ptr(), 8);
        assert!(n < 0, "peer FIN before Content-Length is not clean EOF");
        http_close(&mut *hs);
    });
}

#[test]
fn drain_mid_body_eof_is_a_transport_error() {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    std::thread::scope(|sc| {
        sc.spawn(|| {
            let Ok((mut s, _)) = srv.accept() else { return };
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\nABCDEFGH");
            let _ = s.shutdown(std::net::Shutdown::Write);
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
        let mut dst = [0u8; 32];
        let mut got = 0i32;
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_millis(200) {
            let n = http_drain_available(&mut *hs, &mut dst);
            if n < 0 {
                assert!(got > 0, "FIN must not hide the bytes already in buf");
                http_close(&mut *hs);
                return;
            }
            got += n;
        }
        panic!("park-time mid-body FIN must surface as a drain error, not idle-done");
    });
}
