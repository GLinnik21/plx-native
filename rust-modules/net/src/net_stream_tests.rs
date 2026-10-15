//! The streaming HTTPS read, [`stream_tls`], against a scripted loopback TLS server
//! (`spawn_streaming`): bytes forwarded in order while the response is still open, cancel and idle
//! latency, a failed strict handshake, no retry once a body byte has been delivered, a non-2xx
//! answer reported before any byte, and the `Unsupported` answer under the serialised-TLS fallback.
//!
//! Every test that needs libcurl skips (loudly) when none binds, as the sibling suites do; the
//! instrument check at the top is the line that says whether they ran.

use super::*;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn pem_file(cert: &TestCert, tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("plxnative-stream-ca-{tag}-{}.pem", std::process::id()));
    std::fs::write(&path, &cert.pem).expect("write scratch CA bundle");
    path
}

fn url(port: u16) -> String {
    format!("https://127.0.0.1:{port}/:/eventsource/notifications?filters=playing")
}

fn headers() -> Vec<String> {
    vec!["Accept: text/event-stream".to_owned()]
}

/// One streaming read against `srv`, trusting `cert`; returns how it ended and every callback's
/// (arrival instant, bytes).
fn run(
    srv: &Observed,
    cert: &TestCert,
    tag: &str,
    idle: Duration,
    cancel: &AtomicBool,
) -> (StreamEnd, Vec<(Instant, Vec<u8>)>) {
    let ca = pem_file(cert, tag);
    let seen = Mutex::new(Vec::new());
    let end = stream_tls(
        &url(srv.port),
        &headers(),
        Tls::CaBundle(ca.to_str().unwrap()),
        None,
        idle,
        cancel,
        &mut |bytes: &[u8]| {
            seen.lock().unwrap().push((Instant::now(), bytes.to_vec()));
            true
        },
    );
    let _ = std::fs::remove_file(&ca);
    (end, seen.into_inner().unwrap())
}

fn joined(seen: &[(Instant, Vec<u8>)]) -> Vec<u8> {
    seen.iter().flat_map(|(_, b)| b.iter().copied()).collect()
}

#[test]
fn libcurl_binds_on_this_host_so_the_stream_tests_are_not_vacuous() {
    let _serial = plx_base::testlock::serial();
    assert!(curl_ready(), "no libcurl candidate binds on this host: every stream test skipped");
}

#[test]
fn chunks_arrive_in_order_across_delays() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let cert = Arc::new(mint_cert(&["127.0.0.1"]));
    let srv = spawn_streaming(
        Arc::clone(&cert),
        vec![
            StreamStep::Chunk(b"one\n".to_vec()),
            StreamStep::Sleep(300),
            StreamStep::Chunk(b"two\n".to_vec()),
            StreamStep::Sleep(300),
            StreamStep::Chunk(b"three\n".to_vec()),
            StreamStep::Close,
        ],
    );
    let cancel = AtomicBool::new(false);
    let (end, seen) = run(&srv, &cert, "order", Duration::from_secs(10), &cancel);
    assert_eq!(end, StreamEnd::Closed);
    assert_eq!(joined(&seen), b"one\ntwo\nthree\n");
    // The first bytes were handed over while the server was still talking, not after it closed.
    let spread = seen.last().unwrap().0.duration_since(seen.first().unwrap().0);
    assert!(spread >= Duration::from_millis(400), "chunks were buffered to the end: spread {spread:?}");
    // The request carried the caller's header and asked for a plain GET.
    let request = String::from_utf8_lossy(&srv.requests.lock().unwrap()[0]).into_owned();
    assert!(request.starts_with("GET /:/eventsource/notifications?filters=playing "), "{request}");
    assert!(request.to_ascii_lowercase().contains("accept: text/event-stream"), "{request}");
}

#[test]
fn cancel_returns_within_1500ms() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let cert = Arc::new(mint_cert(&["127.0.0.1"]));
    let srv = spawn_streaming(Arc::clone(&cert), vec![StreamStep::Hang]);
    let cancel = Arc::new(AtomicBool::new(false));
    let set_at = Arc::new(Mutex::new(None));
    let setter = {
        let (cancel, set_at) = (Arc::clone(&cancel), Arc::clone(&set_at));
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            *set_at.lock().unwrap() = Some(Instant::now());
            cancel.store(true, std::sync::atomic::Ordering::Release);
        })
    };
    let (end, seen) = run(&srv, &cert, "cancel", Duration::from_secs(60), &cancel);
    let returned = Instant::now();
    setter.join().unwrap();
    assert_eq!(end, StreamEnd::Cancelled);
    assert!(seen.is_empty());
    let latency = returned.duration_since(set_at.lock().unwrap().expect("cancel was set"));
    assert!(latency <= Duration::from_secs(3), "cancel took {latency:?}");
}

#[test]
fn cancel_set_before_the_call_never_dials() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let cert = Arc::new(mint_cert(&["127.0.0.1"]));
    let srv = spawn_streaming(Arc::clone(&cert), vec![StreamStep::Hang]);
    let cancel = AtomicBool::new(true);
    let began = Instant::now();
    let (end, seen) = run(&srv, &cert, "precancel", Duration::from_secs(60), &cancel);
    assert_eq!(end, StreamEnd::Cancelled);
    assert!(seen.is_empty());
    assert!(began.elapsed() < Duration::from_secs(3), "took {:?}", began.elapsed());
    assert_eq!(srv.accepted(), 0);
}

#[test]
fn idle_aborts() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let cert = Arc::new(mint_cert(&["127.0.0.1"]));
    let srv = spawn_streaming(Arc::clone(&cert), vec![StreamStep::Chunk(b": hi\n\n".to_vec()), StreamStep::Hang]);
    let cancel = AtomicBool::new(false);
    let (end, seen) = run(&srv, &cert, "idle", Duration::from_secs(2), &cancel);
    let returned = Instant::now();
    assert_eq!(end, StreamEnd::Idle);
    assert_eq!(joined(&seen), b": hi\n\n");
    // Idle is measured from the last delivered byte, not from the dial.
    let took = returned.duration_since(seen.last().expect("one chunk arrived").0);
    assert!(took >= Duration::from_secs(2) && took <= Duration::from_secs(5), "idle abort took {took:?}");
}

#[test]
fn pings_keep_alive() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let cert = Arc::new(mint_cert(&["127.0.0.1"]));
    let mut steps = Vec::new();
    for _ in 0..5 {
        steps.push(StreamStep::Chunk(b":".to_vec()));
        steps.push(StreamStep::Sleep(1000));
    }
    steps.push(StreamStep::Close);
    let srv = spawn_streaming(Arc::clone(&cert), steps);
    let cancel = AtomicBool::new(false);
    let (end, seen) = run(&srv, &cert, "pings", Duration::from_secs(4), &cancel);
    assert_eq!(end, StreamEnd::Closed, "a ping every second must keep a 4 s idle bound quiet");
    assert_eq!(joined(&seen), b":::::");
}

#[test]
fn strict_tls_failure_is_transport() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let cert = Arc::new(mint_cert(&["127.0.0.1"]));
    let srv = spawn_streaming(Arc::clone(&cert), vec![StreamStep::Chunk(b"never\n".to_vec()), StreamStep::Close]);
    let cancel = AtomicBool::new(false);
    let mut delivered = 0usize;
    // `Tls::Ca` is the device store, which does not trust a throwaway self-signed certificate.
    let end = stream_tls(
        &url(srv.port),
        &headers(),
        Tls::Ca,
        None,
        Duration::from_secs(5),
        &cancel,
        &mut |b: &[u8]| {
            delivered += b.len();
            true
        },
    );
    assert!(matches!(end, StreamEnd::Transport(f) if f.curl_rc == Some(60)), "{end:?}");
    assert_eq!(delivered, 0, "an unverified peer's bytes must never reach the consumer");
}

/// A transport failure after body bytes is final and is not redialled. (This holds because the
/// request function does not retry a plain transport failure at all; the ladder gate that stops a
/// *rung* after a body is `a_retry_rung_needs_a_stream_that_has_delivered_nothing`.)
#[test]
fn a_transport_failure_after_body_is_final_and_not_redialled() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let cert = Arc::new(mint_cert(&["127.0.0.1"]));
    let srv = spawn_streaming(Arc::clone(&cert), vec![StreamStep::Chunk(b"data: x\n\n".to_vec()), StreamStep::Abort]);
    let cancel = AtomicBool::new(false);
    let (end, seen) = run(&srv, &cert, "noretry", Duration::from_secs(5), &cancel);
    assert!(matches!(end, StreamEnd::Transport(_)), "{end:?}");
    assert_eq!(joined(&seen), b"data: x\n\n");
    assert_eq!(srv.accepted(), 1, "a stream that has delivered bytes is never replayed");
}

#[test]
fn a_retry_rung_needs_a_stream_that_has_delivered_nothing() {
    let cancel = AtomicBool::new(false);
    let mut sink = |_: &[u8]| true;
    let stream = StreamSink::new(&mut sink, &cancel, Duration::from_secs(1));
    assert!(stream.may_retry(), "nothing delivered yet: a handshake rung may run again");
    stream.mark_body();
    assert!(!stream.may_retry(), "once a body byte went out the failure is final");
}

#[test]
fn non_2xx_is_status() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let cert = Arc::new(mint_cert(&["127.0.0.1"]));
    let srv = spawn_streaming(Arc::clone(&cert), vec![StreamStep::Status(401)]);
    let cancel = AtomicBool::new(false);
    let (end, seen) = run(&srv, &cert, "status", Duration::from_secs(5), &cancel);
    assert_eq!(end, StreamEnd::Status(401));
    assert!(seen.is_empty(), "the body of a refusal is not stream data: {seen:?}");
}

#[test]
fn consumer_false_ends_the_stream() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let cert = Arc::new(mint_cert(&["127.0.0.1"]));
    let srv = spawn_streaming(Arc::clone(&cert), vec![StreamStep::Chunk(b"a".to_vec()), StreamStep::Hang]);
    let ca = pem_file(&cert, "consumer");
    let cancel = AtomicBool::new(false);
    let began = Instant::now();
    let end = stream_tls(
        &url(srv.port),
        &headers(),
        Tls::CaBundle(ca.to_str().unwrap()),
        None,
        Duration::from_secs(60),
        &cancel,
        &mut |_: &[u8]| false,
    );
    let _ = std::fs::remove_file(&ca);
    assert_eq!(end, StreamEnd::Consumer);
    assert!(began.elapsed() < Duration::from_secs(5));
}

#[test]
fn unsupported_under_serial_tls() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let cert = Arc::new(mint_cert(&["127.0.0.1"]));
    let srv = spawn_streaming(Arc::clone(&cert), vec![StreamStep::Hang]);
    let _forced = SerialTlsGuard::force();
    assert!(!threaded_tls_ready());
    let cancel = AtomicBool::new(false);
    let (end, seen) = run(&srv, &cert, "serial", Duration::from_secs(5), &cancel);
    assert_eq!(end, StreamEnd::Unsupported, "a device that cannot stream is not a state of the moment: it must park, not back off");
    assert!(seen.is_empty());
    assert_eq!(srv.accepted(), 0, "an unsupported stream must not even dial");
}

/// Key mode lowers `VERIFYPEER`, so the sink re-checks the peer's key itself before the first byte
/// reaches the consumer. With no certificate chain to read (here: no handle at all) it must fail
/// closed, deliver nothing and say why.
#[test]
fn a_key_mode_write_with_an_unreadable_chain_fails_closed() {
    let cancel = AtomicBool::new(false);
    let mut delivered = 0usize;
    let mut sink = |b: &[u8]| {
        delivered += b.len();
        true
    };
    let stream = StreamSink::new(&mut sink, &cancel, Duration::from_secs(5));
    stream.begin_attempt(std::ptr::null_mut(), Some("sha256//AAAA"));
    assert!(!stream.write(b"data: x\n\n"), "an unconfirmed key must stop the transfer");
    assert!(stream.pin_refused());
    assert!(!stream.may_retry(), "a mid-body pin mismatch is final: the ladder must not run another rung");
    assert!(
        matches!(stream.ended(), Some(StreamEnd::Transport(f)) if f.curl_rc == Some(keypin::PIN_MISMATCH as i32)),
        "{:?}",
        stream.ended()
    );
    drop(stream);
    assert_eq!(delivered, 0);
}

#[test]
fn a_fully_verified_attempt_has_no_key_to_confirm() {
    let cancel = AtomicBool::new(false);
    let mut sink = |_: &[u8]| true;
    let stream = StreamSink::new(&mut sink, &cancel, Duration::from_secs(5));
    stream.begin_attempt(std::ptr::null_mut(), None);
    assert!(stream.pin_confirmed());
    assert!(!stream.pin_refused());
}

#[test]
fn key_mode_delivers_bytes_only_for_the_remembered_key() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    // An expired leaf under a trusted CA: strict verification fails (so a stale pin cannot be
    // rescued by the fallback rung), and only the remembered key can admit it.
    let cert = expired_leaf(&["127.0.0.1"]);
    let other = mint_cert(&["127.0.0.1"]);
    let cancel = AtomicBool::new(false);

    let good = spawn_streaming(Arc::clone(&cert), vec![StreamStep::Chunk(b"ok\n".to_vec()), StreamStep::Close]);
    let _kept = remember(good.port, &cert);
    let (end, seen) = run(&good, &cert, "key-ok", Duration::from_secs(5), &cancel);
    assert_eq!(end, StreamEnd::Closed);
    assert_eq!(joined(&seen), b"ok\n");

    let bad = spawn_streaming(Arc::clone(&cert), vec![StreamStep::Chunk(b"leak\n".to_vec()), StreamStep::Close]);
    let _wrong = remember(bad.port, &other);
    let (end, seen) = run(&bad, &cert, "key-bad", Duration::from_secs(5), &cancel);
    assert!(matches!(end, StreamEnd::Transport(_)), "{end:?}");
    assert!(seen.is_empty(), "a peer whose key does not match the remembered one must not reach the consumer");
}
