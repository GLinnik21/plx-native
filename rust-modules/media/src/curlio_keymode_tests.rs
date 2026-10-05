//! Issue #378, media plane: `curlio::CurlSource` opens, reopens and seeks the same way the control
//! plane does when a wrong clock fails a certificate's dates — through the server's remembered key.
//! The control-plane half is `auth_discovery_tests.rs`'s; these moved here because only the media
//! layer may name `curlio`. Every test holds `testlock::serial()`: the CA override is
//! process-global, and each keys the key table by its own loopback server's ephemeral port.

use std::sync::Arc;

use plx_net::net::{curl_ready, expired_leaf, identity_request, key_of_port, leaf_pin, mint_cert, remember, spawn_observed, spawn_redirecting, TestCaGuard};

fn media_body() -> Vec<u8> {
    (0..5000u32).map(|i| (i % 253) as u8).collect()
}

fn media_url(port: u16) -> String {
    format!("https://127.0.0.1:{port}/video.mkv")
}

fn read_n(src: &mut crate::curlio::CurlSource, n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    let mut got = 0;
    while got < n {
        let r = src.read(&mut out[got..]);
        assert!(r > 0, "read returned {r} after {got} bytes");
        got += r as usize;
    }
    out
}

#[test]
fn a_media_open_on_an_expired_leaf_reads_bytes_when_its_remembered_key_is_known() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-media-open");
    let served = plx_net::net::spawn_observed(Arc::clone(&cert), media_body());
    let _key = remember(served.port, &cert);
    let mut src = crate::curlio::CurlSource::open(&media_url(served.port), 0)
        .expect("the date alone must not refuse the server we know");
    assert_eq!(src.status(), 200);
    assert_eq!(read_n(&mut src, 64), media_body()[..64]);
}

#[test]
fn a_media_open_whose_key_differs_from_the_remembered_one_fails_with_a_pin_mismatch() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-media-mismatch");
    let served = plx_net::net::spawn_observed(Arc::clone(&cert), media_body());
    let other = plx_net::net::mint_cert(&["127.0.0.1"]);
    let key = key_of_port(served.port);
    let _key = plx_net::net::keypin::Scoped::new(key.clone(), &leaf_pin(&other));
    let err = crate::curlio::CurlSource::open(&media_url(served.port), 0)
        .err()
        .expect("a stranger's key is not the server's");
    assert_eq!(err, crate::curlio::OpenErr::Transport(90));
    assert!(!plx_net::net::keypin::is_latched(&key));
    assert_eq!(
        plx_net::net::keypin::fact_for(&key).blocked,
        Some(plx_net::net::keypin::Blocked::KeyChanged),
        "the media plane publishes the changed key too",
    );
}

#[test]
fn a_media_open_on_an_expired_leaf_with_no_remembered_key_is_refused_as_before() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-media-nokey");
    let served = plx_net::net::spawn_observed(Arc::clone(&cert), media_body());
    let _watched = plx_net::net::keypin::Scoped::watch(&key_of_port(served.port));
    let err = crate::curlio::CurlSource::open(&media_url(served.port), 0)
        .err()
        .expect("nothing to recognise it by");
    assert_eq!(err, crate::curlio::OpenErr::Transport(60));
    assert_eq!(
        plx_net::net::keypin::fact_for(&key_of_port(served.port)).blocked,
        Some(plx_net::net::keypin::Blocked::NoKey),
        "the media plane publishes a date failure with no key",
    );
}

/// **A redirect hop is a request to another host, and another host's key is not the server's key.**
/// A server served in key mode (its date fails, its remembered key matches) answers with a redirect to
/// a host whose certificate is a different one. The key pin belongs to the server's handshake: the hop
/// is a request of its own, begun strict for its own host, so the key it presents says nothing about
/// the server and must not be published as the SERVER's key having changed (`Blocked::KeyChanged`,
/// which would tell the person to re-pair a server that is fine). The target here chains to nothing
/// the device store trusts, so the open ends in the hop's own failure (60 on the television's OpenSSL;
/// this host's libcurl says 35); what is graded is whose fact that failure is.
#[test]
fn a_redirect_from_a_key_mode_server_to_a_host_with_another_key_does_not_blame_the_server() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-media-redirect");
    let target = spawn_observed(Arc::new(mint_cert(&["127.0.0.1"])), media_body());
    let served = spawn_redirecting(Arc::clone(&cert), &media_url(target.port));
    let key = key_of_port(served.port);
    let target_key = key_of_port(target.port);
    let _key = plx_net::net::keypin::Scoped::new(key.clone(), &leaf_pin(&cert));
    let _target_watch = plx_net::net::keypin::Scoped::watch(&target_key);

    let err = crate::curlio::CurlSource::open(&media_url(served.port), 0)
        .err()
        .expect("the hop's certificate is not one the device store trusts");
    assert!(matches!(err, crate::curlio::OpenErr::Transport(60 | 35)), "the hop's own failure, not a pin mismatch: {err:?}");
    assert_eq!(plx_net::net::keypin::fact_for(&key).blocked, None, "the server's key did not change: another host's did");
    assert!(plx_net::net::keypin::is_latched(&key), "the server's own handshake was established in key mode");
    assert_eq!(plx_net::net::keypin::fact_for(&target_key).blocked, None, "and the target's failure is not a date");
}

/// The media plane's half of `auth_discovery_tests`'s
/// `a_later_strict_failure_that_is_not_the_date_ends_no_key_on_the_control_plane`: a fact is the
/// host's LATEST strict outcome, so a strict failure that is not the date ends NoKey.
#[test]
fn a_later_strict_failure_that_is_not_the_date_ends_no_key_on_the_media_plane() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let ca = TestCaGuard::install(&cert.pem, "clock-later-media");
    let served = plx_net::net::spawn_observed(Arc::clone(&cert), media_body());
    let key = key_of_port(served.port);
    let _watched = plx_net::net::keypin::Scoped::watch(&key);
    let err = crate::curlio::CurlSource::open(&media_url(served.port), 0).err().expect("nothing to recognise it by");
    assert_eq!(err, crate::curlio::OpenErr::Transport(60));
    assert_eq!(plx_net::net::keypin::fact_for(&key).blocked, Some(plx_net::net::keypin::Blocked::NoKey));
    drop(ca);
    let err = crate::curlio::CurlSource::open(&media_url(served.port), 0).err().expect("the issuer is no longer trusted");
    assert_eq!(err, crate::curlio::OpenErr::Transport(60));
    assert_eq!(plx_net::net::keypin::fact_for(&key).blocked, None, "the date is no longer what fails");
}

/// Scenario A as the television has it: the date failure published NoKey, the clock was fixed, and
/// the server is now simply unreachable (a refused connection, rc 7) — nothing about a certificate
/// at all. Both planes end the fact.
#[test]
fn a_refused_connection_ends_no_key_on_both_planes() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let closed_port = || std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    for plane in ["control", "media"] {
        let port = closed_port();
        let key = key_of_port(port);
        let _watched = plx_net::net::keypin::Scoped::watch(&key);
        plx_net::net::keypin::strict_failure(&key, 60, Some(10));
        assert_eq!(plx_net::net::keypin::fact_for(&key).blocked, Some(plx_net::net::keypin::Blocked::NoKey));
        let rc = match plane {
            "control" => identity_request(port, "https", false).err().expect("nothing listens").curl_rc,
            _ => match crate::curlio::CurlSource::open(&media_url(port), 0).err().expect("nothing listens") {
                crate::curlio::OpenErr::Transport(rc) => Some(rc),
                other => panic!("{plane}: {other:?}"),
            },
        };
        assert_eq!(rc, Some(7), "{plane}");
        assert_eq!(plx_net::net::keypin::fact_for(&key).blocked, None, "{plane}: unreachable is not the clock");
    }
}

#[test]
fn media_seeks_and_reopens_in_key_mode_each_do_their_own_handshake() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-media-latch");
    let served = plx_net::net::spawn_observed(Arc::clone(&cert), media_body());
    let key = key_of_port(served.port);
    let _key = plx_net::net::keypin::Scoped::new(key.clone(), &leaf_pin(&cert));

    let mut src = crate::curlio::CurlSource::open(&media_url(served.port), 0).expect("first open");
    assert_eq!(served.accepted(), 2, "the failed strict handshake and the key-mode one");
    assert!(plx_net::net::keypin::is_latched(&key));
    assert_eq!(read_n(&mut src, 16), media_body()[..16]);

    assert!(src.seek(1000), "a seek in key mode");
    assert_eq!(served.accepted(), 3, "exactly one handshake");
    assert_eq!(read_n(&mut src, 16), media_body()[1000..1016]);

    src.reopen_until(&media_url(served.port), None, &mut plx_base::checkpoint::NoCheckpoint)
        .expect("a reopen in key mode");
    assert_eq!(served.accepted(), 4, "exactly one handshake");
    assert_eq!(read_n(&mut src, 16), media_body()[..16]);

    // The remembered key stops being the server's: the next seek fails closed with rc 90 and the
    // host is out of key mode.
    let other = plx_net::net::mint_cert(&["127.0.0.1"]);
    plx_net::net::keypin::set_for_test(&key, &leaf_pin(&other));
    assert!(!src.seek(2000), "the new pin is not the server's");
    assert!(!plx_net::net::keypin::is_latched(&key));
}

/// **A key-mode transfer never rides a kept-alive connection.** The source keeps its multi and
/// connection cache across attempts, and a handle that reused a cached connection would perform no
/// handshake, so there would be no certificate to confirm and the open would be refused as a pin
/// mismatch — which every HLS segment after the first and every seek after a read to EOF would hit.
/// The `Connection: close` double used above cannot see that, so this one keeps connections alive.
#[test]
fn media_key_mode_never_reuses_a_kept_alive_connection() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-media-keepalive");
    let served = plx_net::net::spawn_observed_keepalive(Arc::clone(&cert), media_body());
    let key = key_of_port(served.port);
    let _key = plx_net::net::keypin::Scoped::new(key.clone(), &leaf_pin(&cert));

    let mut src = crate::curlio::CurlSource::open(&media_url(served.port), 0).expect("first open");
    assert_eq!(served.accepted(), 2, "the failed strict handshake and the key-mode one");
    assert!(plx_net::net::keypin::is_latched(&key));

    // Read to the end: the transfer completes cleanly, which is what leaves a connection in the
    // cache for a following handle to find.
    let mut all = Vec::new();
    let mut buf = [0u8; 700];
    loop {
        let n = src.read(&mut buf);
        assert!(n >= 0, "read failed with {n} after {} bytes", all.len());
        if n == 0 {
            break;
        }
        all.extend_from_slice(&buf[..n as usize]);
    }
    assert_eq!(all, media_body());

    assert!(src.seek(1000), "a seek after a read to EOF must succeed in key mode");
    assert_eq!(served.accepted(), 3, "its own handshake, not the cached connection");
    assert_eq!(read_n(&mut src, 16), media_body()[1000..1016]);

    src.reopen_until(&media_url(served.port), None, &mut plx_base::checkpoint::NoCheckpoint)
        .expect("a reopen in key mode");
    assert_eq!(served.accepted(), 4, "its own handshake, not the cached connection");
    assert_eq!(read_n(&mut src, 16), media_body()[..16]);

    assert!(
        plx_net::net::keypin::is_latched(&key),
        "no pin-mismatch outcome: the host is still served in key mode"
    );
    assert_eq!(plx_net::net::keypin::pin_for_test(&key).as_deref(), Some(leaf_pin(&cert).as_str()));
}
