//! The one response header the easy path keeps: `ETag` (plan item NET1). The header callback is
//! graded on its own (case, CRLF, a weak validator, a header that is absent, a redirect hop's
//! validator not leaking into the final answer) and then through the real libcurl against a
//! loopback double, including the `If-None-Match` round trip that answers 304 with no body.

use super::*;

/// Feed `lines` to the real callback, one call each, as libcurl does.
fn fed(lines: &[&str]) -> BodySink {
    let mut sink = BodySink::new(None);
    for line in lines {
        let n = header_cb(
            line.as_ptr() as *mut c_char,
            1,
            line.len(),
            (&mut sink as *mut BodySink) as *mut c_void,
        );
        assert_eq!(n, line.len(), "the callback must take every line whole: {line:?}");
    }
    sink
}

#[test]
fn an_etag_header_is_kept_trimmed_and_verbatim() {
    let sink = fed(&["HTTP/1.1 200 OK\r\n", "Content-Type: application/json\r\n", "ETag: \"abc123\"\r\n", "\r\n"]);
    assert_eq!(sink.etag.as_deref(), Some("\"abc123\""));
}

#[test]
fn the_weak_prefix_is_kept() {
    assert_eq!(fed(&["HTTP/1.1 200 OK\r\n", "ETag: W/\"x-1\"\r\n"]).etag.as_deref(), Some("W/\"x-1\""));
}

#[test]
fn the_header_name_is_case_insensitive_and_the_value_padding_is_dropped() {
    assert_eq!(fed(&["etag:   W/\"p\"  \r\n"]).etag.as_deref(), Some("W/\"p\""));
    assert_eq!(fed(&["ETAG:\t\"q\"\r\n"]).etag.as_deref(), Some("\"q\""));
    assert_eq!(fed(&["eTag:\"r\"\n"]).etag.as_deref(), Some("\"r\""), "a bare LF ends a line too");
}

#[test]
fn no_etag_header_means_none_and_lookalike_names_do_not_count() {
    assert_eq!(fed(&["HTTP/1.1 200 OK\r\n", "X-Etag: \"no\"\r\n", "ETagX: \"no\"\r\n", "\r\n"]).etag, None);
    assert_eq!(fed(&["ETag:\r\n"]).etag, None, "an empty value is no validator");
    assert_eq!(fed(&[]).etag, None);
}

#[test]
fn a_new_status_line_forgets_the_validator_of_the_hop_before() {
    let sink = fed(&["HTTP/1.1 302 Found\r\n", "ETag: \"hop\"\r\n", "\r\n", "HTTP/1.1 200 OK\r\n", "\r\n"]);
    assert_eq!(sink.etag, None);
    let sink = fed(&["HTTP/1.1 302 Found\r\n", "ETag: \"hop\"\r\n", "\r\n", "HTTP/1.1 200 OK\r\n", "ETag: \"final\"\r\n"]);
    assert_eq!(sink.etag.as_deref(), Some("\"final\""));
}

#[test]
fn a_null_sink_or_a_non_utf8_line_is_taken_without_a_panic() {
    let line = b"ETag: \xff\xfe\r\n";
    let n = header_cb(line.as_ptr() as *mut c_char, 1, line.len(), std::ptr::null_mut());
    assert_eq!(n, line.len());
    let mut sink = BodySink::new(None);
    let n = header_cb(line.as_ptr() as *mut c_char, 1, line.len(), (&mut sink as *mut BodySink) as *mut c_void);
    assert_eq!(n, line.len());
    assert_eq!(sink.etag, None, "a validator that is not text is no validator");
}

/// One request to a loopback double that answers `reply` verbatim; returns the request head it saw.
fn served(reply: &'static str, headers: &[String]) -> (Option<Resp>, String) {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    let th = std::thread::spawn(move || {
        let (mut sock, _) = srv.accept().expect("accept");
        let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let mut seen = Vec::new();
        let mut buf = [0u8; 4096];
        while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
            match sock.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => seen.extend_from_slice(&buf[..n]),
            }
        }
        let _ = sock.write_all(reply.as_bytes());
        String::from_utf8_lossy(&seen).into_owned()
    });
    let got = request(&format!("http://127.0.0.1:{port}/list"), headers, "GET", None, API, false, None, None);
    (got, th.join().unwrap())
}

#[test]
fn a_real_response_carries_its_etag_in_resp() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let (got, _) = served(
        "HTTP/1.1 200 OK\r\netag: W/\"v7\"\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}", &[]);
    let got = got.expect("answered");
    assert_eq!((got.status, got.body.as_slice(), got.etag.as_deref()), (200, b"{}".as_slice(), Some("W/\"v7\"")));
}

#[test]
fn a_response_without_one_has_none() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let (got, _) = served("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", &[]);
    assert_eq!(got.expect("answered").etag, None);
}

#[test]
fn if_none_match_rides_the_caller_headers_and_a_304_comes_back_empty_with_the_validator() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let (got, head) = served(
        "HTTP/1.1 304 Not Modified\r\nETag: W/\"v7\"\r\nConnection: close\r\n\r\n",
        &["If-None-Match: W/\"v7\"".to_string()]);
    assert!(head.contains("\r\nIf-None-Match: W/\"v7\"\r\n"), "{head:?}");
    let got = got.expect("a 304 is an answer");
    assert_eq!((got.status, got.body.len(), got.etag.as_deref()), (304, 0, Some("W/\"v7\"")));
}

/// A LAN entry may upgrade, but the following public hop must never downgrade again.
#[test]
fn an_http_https_http_redirect_chain_is_aborted_before_the_downgrade() {
    let mut sink = BodySink::new(None);
    sink.redirect = Some(RedirectFloor::new(b"http://192.168.1.20/hud"));
    for line in ["HTTP/1.1 302 Found\r\n", "Location: https://example.invalid/hud\r\n", "\r\n", "HTTP/1.1 302 Found\r\n"] {
        assert_eq!(header_cb(line.as_ptr() as *mut c_char, 1, line.len(), (&mut sink as *mut BodySink).cast()), line.len());
    }
    let line = "Location: http://example.invalid/hud\r\n";
    assert_eq!(header_cb(line.as_ptr() as *mut c_char, 1, line.len(), (&mut sink as *mut BodySink).cast()), 0,
        "the header callback must refuse this hop before curl follows it");
}

#[test]
fn the_bounded_public_fetch_never_reaches_plaintext_after_upgrading() {
    let _serial = plx_base::testlock::serial();
    assert!(curl_ready(), "this regression must exercise the host curl");
    let cert = std::sync::Arc::new(mint_ca_issued_cert(&["127.0.0.1"], ymd_from_now(-1), ymd_from_now(30)));
    let _ca = TestCaGuard::install(&cert.pem, "hud-redirect-floor");
    let plaintext = std::net::TcpListener::bind("127.0.0.1:0").expect("bind forbidden final hop");
    plaintext.set_nonblocking(true).unwrap();
    let final_port = plaintext.local_addr().unwrap().port();
    let tls = spawn_scripted(cert, [("/secure", Reply::redirect(302, &format!("http://127.0.0.1:{final_port}/final")))]);
    let response = format!("HTTP/1.1 302 Found\r\nLocation: https://127.0.0.1:{}/secure\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", tls.port);
    let (start_port, start) = crate::stream::test_support::one_shot_echo("127.0.0.1:0", response.into_bytes()).expect("bind HTTP entry");
    let got = https_get_public_bounded(&format!("http://127.0.0.1:{start_port}/start"), 32);
    start.join().unwrap();
    assert_eq!(tls.requests.lock().unwrap().len(), 1, "the HTTPS hop really answered");
    assert!(matches!(plaintext.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "the forbidden plaintext listener must never receive even a connection");
    let Err(refusal) = got else { panic!("the plaintext final body must never be returned after HTTPS") };
    assert_eq!(refusal.curl_rc, Some(23), "the header callback ended the transfer before following");
    assert_eq!(refusal.status, None, "the redirect's status is not final response evidence");
}

#[test]
fn the_public_redirect_floor_inherits_tls_through_relative_hops_and_ignores_other_locations() {
    let mut floor = RedirectFloor::new(b"http://192.168.1.20/hud");
    for line in [
        "HTTP/1.1 200 OK\r\n", "Location: https://example.invalid/unused\r\n",
        "HTTP/1.1 302 Found\r\n", "Location: http://192.168.1.20/next\r\n",
        "location:\tHtTpS://example.invalid/hud\r\n",
        "HTTP/2 307 Temporary Redirect\r\n", "Location: /relative\r\n",
        "HTTP/2 308 Permanent Redirect\r\n", "Location: //example.invalid/another\r\n",
        "HTTP/2 200 OK\r\n", "Location: http://example.invalid/not-a-redirect\r\n",
        "HTTP/2 302 Found\r\n",
    ] {
        assert!(floor.accepts(line.as_bytes()), "{line:?}");
    }
    assert!(!floor.accepts(b"LoCaTiOn:  hTtP://example.invalid/downgrade\r\n"));
    let mut secure = RedirectFloor::new(b"HTTPS://example.invalid/hud");
    assert!(secure.accepts(b"HTTP/1.1 301 Moved Permanently\r\n"));
    assert!(!secure.accepts(b"Location: http:other\r\n"), "absolute schemes need not have //");
}

#[test]
fn the_bounded_public_fetch_can_upgrade_and_follow_relative_tls_locations() {
    let _serial = plx_base::testlock::serial();
    assert!(curl_ready(), "this regression must exercise the host curl");
    let cert = std::sync::Arc::new(mint_ca_issued_cert(&["127.0.0.1"], ymd_from_now(-1), ymd_from_now(30)));
    let _ca = TestCaGuard::install(&cert.pem, "hud-upgrade-allowed");
    let tls = spawn_scripted(cert, [("/secure", Reply::redirect(302, "/final")), ("/final", Reply::ok(b"hud".to_vec()))]);
    let response = format!("HTTP/1.1 302 Found\r\nLocation: https://127.0.0.1:{}/secure\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", tls.port);
    let (port, start) = crate::stream::test_support::one_shot_echo("127.0.0.1:0", response.into_bytes()).expect("bind HTTP entry");
    let got = https_get_public_bounded(&format!("http://127.0.0.1:{port}/start"), 32).expect("an upgrade and relative TLS hop are legal");
    start.join().unwrap();
    assert_eq!(got.status, 200);
    assert_eq!(got.body, b"hud");
    assert_eq!(tls.requests.lock().unwrap().len(), 2);
}
