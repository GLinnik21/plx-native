//! A held TLS connection, [`connect_tls`], against the loopback TLS double: the caller's own bytes
//! go out and the server's come back over the verified connection, a silent server reads as
//! `WouldBlock` within a tick, and a certificate the trust store does not vouch for never reaches
//! the session.

use super::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn trusting(cert: &TestCert, tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("plxnative-connect-ca-{tag}-{}.pem", std::process::id()));
    std::fs::write(&path, &cert.pem).expect("write scratch CA bundle");
    path
}

/// Read until the reply head and its 4-byte body are in.
fn read_reply(socket: &mut connect::TlsSocket) -> Vec<u8> {
    let started = Instant::now();
    let mut got = Vec::new();
    let mut buf = [0u8; 1024];
    while !(got.windows(4).any(|w| w == b"\r\n\r\n") && got.ends_with(b"body")) && started.elapsed() < Duration::from_secs(5) {
        match socket.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("read failed: {e}"),
        }
    }
    got
}

#[test]
fn the_session_speaks_its_own_bytes_over_the_verified_connection() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let cert = Arc::new(mint_cert(&["127.0.0.1"]));
    let srv = spawn_observed_keepalive(Arc::clone(&cert), b"body".to_vec());
    let ca = trusting(&cert, "echo");
    let mut replies = Vec::new();
    let end = connect_tls(&format!("https://127.0.0.1:{}/", srv.port), Tls::CaBundle(ca.to_str().unwrap()), None, &mut |s| {
        for _ in 0..2 {
            s.write_all(b"GET /held HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
            replies.push(read_reply(s));
        }
        // Nothing more is coming: a read reports WouldBlock instead of hanging.
        let mut buf = [0u8; 16];
        let started = Instant::now();
        assert_eq!(s.read(&mut buf).unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        assert!(started.elapsed() < Duration::from_secs(2));
    });
    let _ = std::fs::remove_file(&ca);
    assert!(matches!(end, ConnectEnd::Ran), "{end:?}");
    for reply in &replies {
        let reply = String::from_utf8_lossy(reply);
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(reply.ends_with("body"), "{reply}");
    }
    // Both requests went over the one connection libcurl opened, and libcurl sent none of its own.
    assert_eq!(srv.accepted(), 1);
    let requests = srv.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| r.starts_with(b"GET /held ")));
}

#[test]
fn an_unverified_server_never_reaches_the_session() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let cert = Arc::new(mint_cert(&["127.0.0.1"]));
    let srv = spawn_observed_keepalive(Arc::clone(&cert), b"body".to_vec());
    let mut ran = false;
    // The device's own store, which has never heard of this self-signed certificate.
    let end = connect_tls(&format!("https://127.0.0.1:{}/", srv.port), Tls::Ca, None, &mut |_| ran = true);
    assert!(matches!(end, ConnectEnd::Transport(_)), "{end:?}");
    assert!(!ran);
    assert!(srv.requests.lock().unwrap().is_empty());
}
