//! The bundled public roots fallback, media plane: `curlio::CurlSource` opens, reopens and seeks a
//! `*.plex.direct` server whose certificate chains to a root the device's trust store lacks the
//! same way the control plane does (`net_roots_tests.rs`'s half), through `net::keypin`'s shared
//! decision. "The device store" is `test_ca_bundle`, the shipped bundle is `keypin::test_roots`, and
//! the `*.plex.direct` name reaches loopback through a resolve pin, as in production. Every test
//! holds `testlock::serial()`: both overrides are process-global.

use std::sync::Arc;

use plx_net::net::origin::ResolvePin;
use plx_net::net::{curl_ready, keypin, mint_ca_issued_cert, mint_cert, resolve, spawn_observed, ymd_from_now, TestCaGuard};

fn media_body() -> Vec<u8> {
    (0..5000u32).map(|i| (i % 253) as u8).collect()
}

/// A TLS double for `host` serving a fresh CA's leaf: the CA's PEM, the port, the accept counter.
fn serve(host: &str) -> (String, u16, Arc<std::sync::atomic::AtomicUsize>) {
    let cert = Arc::new(mint_ca_issued_cert(&[host], ymd_from_now(-1), ymd_from_now(30)));
    let pem = cert.pem.clone();
    let served = spawn_observed(cert, media_body());
    (pem, served.port, served.accepted)
}

const PLEX_DIRECT: &str = "127-0-0-1.0123456789abcdef0123456789abcdef.plex.direct";

fn pin_to_loopback(host: &str, port: u16) {
    resolve::add(&ResolvePin::for_test(host, i32::from(port), std::net::IpAddr::from([127, 0, 0, 1])));
}

#[test]
fn a_media_open_on_a_plex_direct_host_the_device_store_cannot_verify_reads_bytes_through_the_bundle() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let (ca, port, accepted) = serve(PLEX_DIRECT);
    let _device = TestCaGuard::install(&mint_cert(&["unrelated.invalid"]).pem, "roots-media-ok");
    let _roots = keypin::test_roots::Guard::install(&ca, "roots-media-ok");
    pin_to_loopback(PLEX_DIRECT, port);
    let key = keypin::key_of(PLEX_DIRECT, i32::from(port));
    let _watch = keypin::Scoped::watch(&key);
    let url = format!("https://{PLEX_DIRECT}:{port}/video.mkv");

    let mut src = crate::curlio::CurlSource::open(&url, 0).expect("the bundle holds the issuing root");
    assert_eq!(src.status(), 200);
    let mut head = [0u8; 64];
    assert_eq!(src.read(&mut head), 64);
    assert_eq!(head[..], media_body()[..64]);
    assert!(keypin::is_roots_latched(&key), "a success through the bundle latches the host");
    assert_eq!(accepted.load(std::sync::atomic::Ordering::Acquire), 2, "the failed strict handshake and the bundle one");

    // A reopen of a latched host starts in roots mode: ONE handshake, not a doomed strict one first.
    drop(src);
    let again = crate::curlio::CurlSource::open(&url, 0).expect("the latched host opens");
    assert_eq!(again.status(), 200);
    assert_eq!(accepted.load(std::sync::atomic::Ordering::Acquire), 3, "exactly one handshake");
}

#[test]
fn a_media_open_without_the_issuing_root_in_the_bundle_fails_as_before() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let (_ca, port, _accepted) = serve(PLEX_DIRECT);
    let _device = TestCaGuard::install(&mint_cert(&["unrelated.invalid"]).pem, "roots-media-lacks");
    let _roots = keypin::test_roots::Guard::install(&mint_cert(&["another-root.invalid"]).pem, "roots-media-lacks");
    pin_to_loopback(PLEX_DIRECT, port);
    let key = keypin::key_of(PLEX_DIRECT, i32::from(port));
    let _watch = keypin::Scoped::watch(&key);
    let err = crate::curlio::CurlSource::open(&format!("https://{PLEX_DIRECT}:{port}/video.mkv"), 0)
        .err()
        .expect("the bundle does not hold the issuing root");
    assert_eq!(err, crate::curlio::OpenErr::Transport(60));
    assert!(!keypin::is_roots_latched(&key));
}

#[test]
fn a_media_open_on_a_host_that_is_not_a_plex_direct_name_never_uses_the_bundle() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    for host in ["127.0.0.1", "plex.direct.evil.example", "notplex.direct"] {
        let (ca, port, _accepted) = serve(host);
        let _device = TestCaGuard::install(&mint_cert(&["unrelated.invalid"]).pem, "roots-media-name");
        let _roots = keypin::test_roots::Guard::install(&ca, "roots-media-name");
        if host != "127.0.0.1" {
            pin_to_loopback(host, port);
        }
        let key = keypin::key_of(host, i32::from(port));
        let _watch = keypin::Scoped::watch(&key);
        let err = crate::curlio::CurlSource::open(&format!("https://{host}:{port}/video.mkv"), 0)
            .err()
            .expect("the bundle is for *.plex.direct names only");
        assert_eq!(err, crate::curlio::OpenErr::Transport(60), "{host}");
        assert!(!keypin::is_roots_latched(&key), "{host}");
    }
}

#[test]
fn a_media_open_with_no_bundle_on_disk_fails_as_before() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let (_ca, port, _accepted) = serve(PLEX_DIRECT);
    let _device = TestCaGuard::install(&mint_cert(&["unrelated.invalid"]).pem, "roots-media-absent");
    keypin::test_roots::set(Some("/nonexistent/le-roots.pem"));
    pin_to_loopback(PLEX_DIRECT, port);
    let key = keypin::key_of(PLEX_DIRECT, i32::from(port));
    let _watch = keypin::Scoped::watch(&key);
    let err = crate::curlio::CurlSource::open(&format!("https://{PLEX_DIRECT}:{port}/video.mkv"), 0)
        .err()
        .expect("nothing to verify it by");
    keypin::test_roots::set(None);
    assert_eq!(err, crate::curlio::OpenErr::Transport(60));
    assert!(!keypin::is_roots_latched(&key));
}

/// A TLS double for `host` serving a CA-issued leaf whose dates ended a month ago (a wrong
/// television clock): the certificate, the port, the accept counter.
fn serve_expired(host: &str) -> (Arc<plx_net::net::TestCert>, u16, Arc<std::sync::atomic::AtomicUsize>) {
    let cert = Arc::new(mint_ca_issued_cert(&[host], ymd_from_now(-90), ymd_from_now(-30)));
    let served = spawn_observed(Arc::clone(&cert), media_body());
    (cert, served.port, served.accepted)
}

/// **A wrong clock behind an old trust store, media plane.** The device store lacks the issuer
/// (strict fails on it, so roots mode engages), and the clock is wrong too, so the bundle verifies
/// the chain and then refuses the dates. The key learned on an earlier good boot is what is left.
#[test]
fn a_media_open_whose_bundle_attempt_fails_on_the_date_reads_bytes_through_the_remembered_key() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let (cert, port, accepted) = serve_expired(PLEX_DIRECT);
    let _device = TestCaGuard::install(&mint_cert(&["unrelated.invalid"]).pem, "roots-media-date-key");
    let _roots = keypin::test_roots::Guard::install(&cert.pem, "roots-media-date-key");
    pin_to_loopback(PLEX_DIRECT, port);
    let key = keypin::key_of(PLEX_DIRECT, i32::from(port));
    let _key = keypin::Scoped::new(key.clone(), &plx_net::net::leaf_pin(&cert));
    let mut src = crate::curlio::CurlSource::open(&format!("https://{PLEX_DIRECT}:{port}/video.mkv"), 0)
        .expect("the remembered key must answer once the bundle has refused the date");
    assert_eq!(src.status(), 200);
    let mut head = [0u8; 64];
    assert_eq!(src.read(&mut head), 64);
    assert_eq!(head[..], media_body()[..64]);
    assert!(keypin::is_latched(&key), "key mode answered, so the host is latched in it");
    assert!(!keypin::is_roots_latched(&key), "the bundle did not answer for this host");
    assert_eq!(accepted.load(std::sync::atomic::Ordering::Acquire), 3, "strict, the bundle, the key: three handshakes and no more");
}

/// The same with no key held: nothing recognises the server, and the app is told why.
#[test]
fn a_media_open_whose_bundle_attempt_fails_on_the_date_with_no_key_publishes_no_key() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let (cert, port, _accepted) = serve_expired(PLEX_DIRECT);
    let _device = TestCaGuard::install(&mint_cert(&["unrelated.invalid"]).pem, "roots-media-date-nokey");
    let _roots = keypin::test_roots::Guard::install(&cert.pem, "roots-media-date-nokey");
    pin_to_loopback(PLEX_DIRECT, port);
    let key = keypin::key_of(PLEX_DIRECT, i32::from(port));
    let _watch = keypin::Scoped::watch(&key);
    let err = crate::curlio::CurlSource::open(&format!("https://{PLEX_DIRECT}:{port}/video.mkv"), 0)
        .err()
        .expect("a date failure with no key is refused");
    assert_eq!(err, crate::curlio::OpenErr::Transport(60));
    assert_eq!(keypin::fact_for(&key).blocked, Some(keypin::Blocked::NoKey));
    assert!(!keypin::is_roots_latched(&key) && !keypin::is_latched(&key));
}

/// **A latched roots start the bundle then refuses.** The host was served through the bundle, and its
/// certificate has since moved to an issuer the device store trusts but the bundle lacks. The next
/// open or seek starts in roots mode, is refused, and goes strict once in the same open.
#[test]
fn a_media_open_on_a_latched_host_the_bundle_now_refuses_goes_strict_once() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let (ca, port, accepted) = serve(PLEX_DIRECT);
    let _device = TestCaGuard::install(&ca, "roots-media-latched");
    let _roots = keypin::test_roots::Guard::install(&mint_cert(&["another-root.invalid"]).pem, "roots-media-latched");
    pin_to_loopback(PLEX_DIRECT, port);
    let key = keypin::key_of(PLEX_DIRECT, i32::from(port));
    let _watch = keypin::Scoped::watch(&key);
    keypin::roots_established(&key, Some(20));
    assert!(keypin::is_roots_latched(&key));
    let src = crate::curlio::CurlSource::open(&format!("https://{PLEX_DIRECT}:{port}/video.mkv"), 0)
        .expect("the device store holds the issuer, so the strict retry verifies");
    assert_eq!(src.status(), 200);
    assert!(!keypin::is_roots_latched(&key), "the refused latch is gone");
    assert_eq!(accepted.load(std::sync::atomic::Ordering::Acquire), 2, "the refused bundle handshake and the strict one");
}

/// The retry is once: refused by the bundle and by the device store, the open fails.
#[test]
fn a_media_open_on_a_latched_host_both_stores_refuse_fails_after_one_strict_retry() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let (_ca, port, accepted) = serve(PLEX_DIRECT);
    let _device = TestCaGuard::install(&mint_cert(&["unrelated.invalid"]).pem, "roots-media-latched-both");
    let _roots = keypin::test_roots::Guard::install(&mint_cert(&["another-root.invalid"]).pem, "roots-media-latched-both");
    pin_to_loopback(PLEX_DIRECT, port);
    let key = keypin::key_of(PLEX_DIRECT, i32::from(port));
    let _watch = keypin::Scoped::watch(&key);
    keypin::roots_established(&key, Some(20));
    let err = crate::curlio::CurlSource::open(&format!("https://{PLEX_DIRECT}:{port}/video.mkv"), 0)
        .err()
        .expect("neither store holds the issuer");
    assert_eq!(err, crate::curlio::OpenErr::Transport(60));
    assert!(!keypin::is_roots_latched(&key));
    assert_eq!(accepted.load(std::sync::atomic::Ordering::Acquire), 2, "bundle then strict, and the bundle is not offered again");
}
