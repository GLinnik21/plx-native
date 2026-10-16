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
