//! The bundled public roots fallback, media plane: `curlio::CurlSource` opens, reopens and seeks a
//! `*.plex.direct` server whose certificate chains to a root the device's trust store lacks the
//! same way the control plane does (`net_roots_tests.rs`'s half), through `net::keypin`'s shared
//! decision. "The device store" is `test_ca_bundle`, the shipped bundle is `keypin::test_roots`, and
//! the `*.plex.direct` name reaches loopback through a resolve pin, as in production. Every test
//! holds `testlock::serial()`: both overrides are process-global.

use crate::curlio::test_fixtures::{media_body, pin_to_loopback, PLEX_DIRECT};
use std::sync::Arc;

use crate::net::{curl_ready, keypin, mint_ca_issued_cert, mint_cert, spawn_observed, spawn_redirecting, ymd_from_now, TestCaGuard};

/// A TLS double for `host` serving a fresh CA's leaf: the CA's PEM, the port, the accept counter.
fn serve(host: &str) -> (String, u16, Arc<std::sync::atomic::AtomicUsize>) {
    let cert = Arc::new(mint_ca_issued_cert(&[host], ymd_from_now(-1), ymd_from_now(30)));
    let pem = cert.pem.clone();
    let served = spawn_observed(cert, media_body());
    (pem, served.port, served.accepted)
}

#[test]
fn a_media_open_on_a_plex_direct_host_the_device_store_cannot_verify_reads_bytes_through_the_bundle() {
    let _serial = crate::testlock::serial();
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
    let _serial = crate::testlock::serial();
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
    let _serial = crate::testlock::serial();
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
    let _serial = crate::testlock::serial();
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
fn serve_expired(host: &str) -> (Arc<crate::net::TestCert>, u16, Arc<std::sync::atomic::AtomicUsize>) {
    let cert = Arc::new(mint_ca_issued_cert(&[host], ymd_from_now(-90), ymd_from_now(-30)));
    let served = spawn_observed(Arc::clone(&cert), media_body());
    (cert, served.port, served.accepted)
}

/// **A wrong clock behind an old trust store, media plane.** The device store lacks the issuer
/// (strict fails on it, so roots mode engages), and the clock is wrong too, so the bundle verifies
/// the chain and then refuses the dates. The key learned on an earlier good boot is what is left.
#[test]
fn a_media_open_whose_bundle_attempt_fails_on_the_date_reads_bytes_through_the_remembered_key() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let (cert, port, accepted) = serve_expired(PLEX_DIRECT);
    let _device = TestCaGuard::install(&mint_cert(&["unrelated.invalid"]).pem, "roots-media-date-key");
    let _roots = keypin::test_roots::Guard::install(&cert.pem, "roots-media-date-key");
    pin_to_loopback(PLEX_DIRECT, port);
    let key = keypin::key_of(PLEX_DIRECT, i32::from(port));
    let _key = keypin::Scoped::new(key.clone(), &crate::net::leaf_pin(&cert));
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
    let _serial = crate::testlock::serial();
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
    let _serial = crate::testlock::serial();
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
    let _serial = crate::testlock::serial();
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

/// `https://<plex.direct>:443@127.0.0.1:<port>/` dials the loopback server, not the `*.plex.direct`
/// name in the userinfo; the bundle must not be offered for it (`net::keypin::key_of_url`).
#[test]
fn a_media_open_with_a_plex_direct_name_in_the_userinfo_never_uses_the_bundle() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let (ca, port, _accepted) = serve("127.0.0.1");
    let _device = TestCaGuard::install(&mint_cert(&["unrelated.invalid"]).pem, "roots-media-userinfo");
    let _roots = keypin::test_roots::Guard::install(&ca, "roots-media-userinfo");
    let key = keypin::key_of(PLEX_DIRECT, 443);
    let _watch = keypin::Scoped::watch(&key);
    let err = crate::curlio::CurlSource::open(&format!("https://{PLEX_DIRECT}:443@127.0.0.1:{port}/video.mkv"), 0)
        .err()
        .expect("the bundle is for *.plex.direct hosts, and the host dialled is 127.0.0.1");
    assert_eq!(err, crate::curlio::OpenErr::Transport(60));
    assert!(!keypin::is_roots_latched(&key));
}

/// **A hop is not served by the bundle just because the host that sent it was.** A `*.plex.direct`
/// open that went through the bundle (strict failed on the missing issuer) is redirected to another
/// host, and that host is a request of its own: it begins strict, against the device store, which is
/// the store it is verified by. One CA issues the household's leaf and the redirect target's, and the
/// bundle holds it, so a hop that inherited the open's trust store would verify; the device store
/// holds nothing relevant, so the hop is refused (60 on the television's OpenSSL; this host's libcurl
/// reports a refused hop certificate as 35). The household's own answer is not undone by it: the
/// host that was served through the bundle stays latched, and the target, no `*.plex.direct` name,
/// never asks for the bundle (`a_redirect_from_a_roots_mode_host_is_verified_against_the_device_store`
/// is the hop that does verify).
#[test]
fn a_redirect_hop_is_not_verified_against_the_bundle_the_open_before_it_was_served_through() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = Arc::new(mint_ca_issued_cert(&[PLEX_DIRECT, "127.0.0.1"], ymd_from_now(-1), ymd_from_now(30)));
    let target = spawn_observed(Arc::clone(&cert), media_body());
    let redirecting = spawn_redirecting(Arc::clone(&cert), &format!("https://127.0.0.1:{}/video.mkv", target.port));
    pin_to_loopback(PLEX_DIRECT, redirecting.port);
    let key = keypin::key_of(PLEX_DIRECT, i32::from(redirecting.port));
    let _watch = keypin::Scoped::watch(&key);
    let target_key = keypin::key_of("127.0.0.1", i32::from(target.port));
    let _target_watch = keypin::Scoped::watch(&target_key);
    let _device = TestCaGuard::install(&mint_cert(&["unrelated.invalid"]).pem, "roots-media-redirect-refused");
    let _roots = keypin::test_roots::Guard::install(&cert.pem, "roots-media-redirect-refused");

    let err = crate::curlio::CurlSource::open(&format!("https://{PLEX_DIRECT}:{}/video.mkv", redirecting.port), 0)
        .err()
        .expect("the hop is verified against the device store, which does not hold its issuer");
    assert!(matches!(err, crate::curlio::OpenErr::Transport(60 | 35)), "{err:?}");
    assert!(keypin::is_roots_latched(&key), "the household's own handshake was verified by the bundle");
    assert!(!keypin::is_roots_latched(&target_key), "the bundle was never offered for the target");
    assert_eq!(redirecting.accepted(), 2, "the failed strict handshake and the bundle one, and no third for the hop's failure");
    assert_eq!(target.accepted(), 1, "the target was dialled once, strictly, and the client refused its certificate");
}

/// **A hop that fails verification is the hop's failure, not its server's.** A `*.plex.direct` server
/// the device store verifies (strict, no roots mode) answers with a redirect to a host whose issuer
/// the device store lacks. The hop's certificate failure belongs to the hop: it must not send the
/// SERVER into roots mode (a retry of the server's own handshake against the bundle, and a log line
/// saying the server's certificate failed the device store, which it did not). The household's
/// connection count is the tell: one, never a second for a retry.
#[test]
fn a_redirect_hop_that_fails_verification_does_not_send_its_server_into_roots_mode() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let household = Arc::new(mint_ca_issued_cert(&[PLEX_DIRECT], ymd_from_now(-1), ymd_from_now(30)));
    let elsewhere = Arc::new(mint_ca_issued_cert(&["127.0.0.1"], ymd_from_now(-1), ymd_from_now(30)));
    let target = spawn_observed(Arc::clone(&elsewhere), media_body());
    let redirecting = spawn_redirecting(Arc::clone(&household), &format!("https://127.0.0.1:{}/video.mkv", target.port));
    pin_to_loopback(PLEX_DIRECT, redirecting.port);
    // The device store verifies the household and not the target; a bundle is on disk, so roots mode
    // is available to a server that is wrongly sent there.
    let _device = TestCaGuard::install(&household.pem, "roots-media-hop-fails");
    let _roots = keypin::test_roots::Guard::install(&household.pem, "roots-media-hop-fails");
    let key = keypin::key_of(PLEX_DIRECT, i32::from(redirecting.port));
    let _watch = keypin::Scoped::watch(&key);
    let _target_watch = keypin::Scoped::watch(&keypin::key_of("127.0.0.1", i32::from(target.port)));

    let err = crate::curlio::CurlSource::open(&format!("https://{PLEX_DIRECT}:{}/video.mkv", redirecting.port), 0)
        .err()
        .expect("the hop's issuer is in neither store");
    assert!(matches!(err, crate::curlio::OpenErr::Transport(60 | 35)), "the hop's own failure: {err:?}");
    assert_eq!(redirecting.accepted(), 1, "the server was verified and asked once; the hop's failure did not retry it");
    assert!(!keypin::is_roots_latched(&key), "the server never went through the bundle");
}

/// **A redirect hop is verified by the store ITS host selects.** A household server that is in roots
/// mode (its `*.plex.direct` name latched) answers with a redirect to a CDN: a different host, a
/// different issuer, one the firmware's own store holds and the bundle does not. The CDN is no
/// `*.plex.direct` name, so nothing about it asks for the bundle; the hop is a strict request for its
/// own host and the device store verifies it. The server's own mode does not follow the request
/// into the next host's handshake.
#[test]
fn a_redirect_from_a_roots_mode_host_is_verified_against_the_device_store() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let household = Arc::new(mint_ca_issued_cert(&[PLEX_DIRECT], ymd_from_now(-1), ymd_from_now(30)));
    let cdn = Arc::new(mint_ca_issued_cert(&["127.0.0.1"], ymd_from_now(-1), ymd_from_now(30)));
    let target = spawn_observed(Arc::clone(&cdn), media_body());
    let redirecting = spawn_redirecting(Arc::clone(&household), &format!("https://127.0.0.1:{}/video.mkv", target.port));
    pin_to_loopback(PLEX_DIRECT, redirecting.port);
    // The device store is the CDN's CA; the bundle is the household's.
    let _device = TestCaGuard::install(&cdn.pem, "roots-media-redirect-store");
    let _roots = keypin::test_roots::Guard::install(&household.pem, "roots-media-redirect-store");
    let key = keypin::key_of(PLEX_DIRECT, i32::from(redirecting.port));
    let _watch = keypin::Scoped::watch(&key);
    let _target_watch = keypin::Scoped::watch(&keypin::key_of("127.0.0.1", i32::from(target.port)));
    keypin::roots_established(&key, Some(20));

    let mut src = crate::curlio::CurlSource::open(&format!("https://{PLEX_DIRECT}:{}/video.mkv", redirecting.port), 0)
        .expect("the CDN's issuer is in the device store, which is what verifies its hop");
    assert_eq!(src.status(), 200);
    let mut head = [0u8; 64];
    assert_eq!(src.read(&mut head), 64);
    assert_eq!(head[..], media_body()[..64]);
}
