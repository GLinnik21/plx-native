//! What the easy path (`request_tls_evidence`) puts on the wire, pinned as it is today, so that the
//! header validation of `net::headers` can be seen to change nothing it did not mean to, and what it
//! did mean to change is graded next to it.
//!
//! Which fact is pinned where (the User-Agent tests of `net_user_agent_tests.rs` own the first):
//!
//! * exactly one `User-Agent` per request, every hop, every verb: `net_user_agent_tests.rs`;
//! * the default `Accept: */*`, and the full header-name set of a request: here;
//! * a caller's empty `Expect:` suppresses libcurl's `Expect: 100-continue` on a large POST: here;
//! * a caller's block that names a header the transport writes (`User-Agent`, `Host`), breaks a header
//!   (CR, LF) or rides a request that follows redirects is refused before anything is dialled: here.
//!
//! Header ORDER is not asserted anywhere: libcurl's order is version-dependent and not a contract,
//! so the comparisons are on the set of header names.

use super::*;
use super::user_agent_tests::{install_wire_user_agent, user_agents, WIRE_USER_AGENT};

/// The lowercased names of every header line in a recorded request's head, in a set's order.
fn header_names(request: &str) -> Vec<String> {
    let head = request.split("\r\n\r\n").next().unwrap_or_default();
    let mut names: Vec<String> = head
        .split("\r\n")
        .skip(1)
        .filter_map(|line| line.split_once(':').map(|(n, _)| n.trim().to_ascii_lowercase()))
        .collect();
    names.sort();
    names
}

/// One request of `verb` to a loopback plaintext double, and the head it sent. The double reads the
/// whole request (head, then `Content-Length` bytes) before it answers, so a large body is never cut
/// off by a reply and a close.
fn sent(verb: &str, headers: &[String], body: Option<&[u8]>) -> String {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    let served = std::thread::spawn(move || {
        let (mut sock, _) = srv.accept().expect("accept");
        let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let mut seen = Vec::new();
        let mut buf = [0u8; 4096];
        let head_end = loop {
            if let Some(i) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            match sock.read(&mut buf) {
                Ok(0) | Err(_) => break seen.len(),
                Ok(n) => seen.extend_from_slice(&buf[..n]),
            }
        };
        let head = String::from_utf8_lossy(&seen[..head_end]).into_owned();
        let want = head
            .to_ascii_lowercase()
            .lines()
            .find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()))
            .unwrap_or(0);
        while seen.len() < head_end + want {
            match sock.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => seen.extend_from_slice(&buf[..n]),
            }
        }
        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        head
    });
    let got = request(&format!("http://127.0.0.1:{port}/p"), headers, verb, body, API, false, None, None)
        .expect("answered");
    assert_eq!(got.status, 200);
    served.join().unwrap()
}

/// **The instrument check.** Every test here skips when libcurl cannot be bound, which is a green
/// suite grading nothing; this is the line that makes the difference visible, as
/// `curlio::tests::libcurl_binds_on_this_host_so_the_tests_above_are_not_vacuous` does for the media
/// plane. If it fails, the tests below did not run.
#[test]
fn libcurl_binds_on_this_host_so_the_wire_tests_are_not_vacuous() {
    let _serial = plx_base::testlock::serial();
    assert!(curl_ready(), "no libcurl candidate binds on this host: every easy-path wire test skipped");
}

#[test]
fn an_easy_request_sends_the_default_accept_and_exactly_the_headers_it_was_given() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    install_wire_user_agent();
    let got = sent("GET", &["X-Plex-Token: t".to_string()], None);
    assert_eq!(user_agents(&got), [WIRE_USER_AGENT], "{got:?}");
    assert_eq!(header_names(&got), ["accept", "host", "user-agent", "x-plex-token"], "{got:?}");
    assert!(got.contains("\r\nAccept: */*\r\n"), "the default Accept: {got:?}");
}

/// A caller's own `Accept` replaces libcurl's default rather than joining it.
#[test]
fn a_caller_accept_replaces_the_default_one() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    install_wire_user_agent();
    let got = sent("GET", &["Accept: application/json".to_string()], None);
    let accepts: Vec<&str> = got.split("\r\n").filter(|l| l.to_ascii_lowercase().starts_with("accept:")).collect();
    assert_eq!(accepts, ["Accept: application/json"], "{got:?}");
}

/// The lab's POSTs send `Expect:` with no value so libcurl does not wait a second for a
/// `100 Continue` the receiver never gives. libcurl asks for it on a body over 1024 bytes (7.53.1) or over 1 MiB (8.x).
#[test]
fn an_empty_expect_header_suppresses_the_100_continue_handshake_of_a_large_post() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    install_wire_user_agent();
    // Past both thresholds: 1024 bytes on the television's 7.53.1, 1 MiB on current libcurl.
    let body = vec![b'x'; 1_200_000];
    let without = sent("POST", &[], Some(&body));
    let with = sent("POST", &["Expect:".to_string()], Some(&body));
    assert!(
        without.to_ascii_lowercase().contains("\r\nexpect: 100-continue\r\n"),
        "instrument: libcurl asks for 100-continue on a body this size when nothing says otherwise: {without:?}"
    );
    assert!(!header_names(&with).contains(&"expect".to_string()), "no Expect header at all: {with:?}");
}

/// Run `request` against a loopback listener and report whether it was refused AND nothing was ever
/// dialled.
fn refused_without_dialling(headers: &[String], follow_redirects: bool) -> bool {
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    srv.set_nonblocking(true).expect("nonblocking");
    let port = srv.local_addr().unwrap().port();
    let got = request(&format!("http://127.0.0.1:{port}/p"), headers, "GET", None, API, follow_redirects, None, None);
    std::thread::sleep(std::time::Duration::from_millis(50));
    got.is_none() && srv.accept().is_err()
}

/// libcurl REPLACES the `CURLOPT_USERAGENT` value with a same-named custom header, so a caller's
/// `User-Agent:` line would have been the one sent (and `net.rs` the "one place that applies it" in
/// name only). It is refused, and the instrument is a block that is sent.
#[test]
fn a_caller_user_agent_or_host_line_is_refused_and_a_plain_block_is_sent() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    install_wire_user_agent();
    for line in ["User-Agent: other", "user-agent:", "Host: evil.example", "Content-Length: 0", "Transfer-Encoding: chunked"] {
        assert!(refused_without_dialling(&[line.to_string()], false), "{line:?}");
    }
    let got = sent("GET", &["X-Plex-Token: t".to_string()], None);
    assert_eq!(user_agents(&got), [WIRE_USER_AGENT], "{got:?}");
}

/// A CR or LF in a header would end it and start another; a NUL would end the C string curl reads.
#[test]
fn a_header_line_that_would_split_the_request_is_refused() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    install_wire_user_agent();
    for line in ["X-A: 1\r\nX-B: 2", "X-A: 1\nX-B: 2", "X-A: 1\0", "no colon", "Bad Name: v"] {
        assert!(refused_without_dialling(&[line.to_string()], false), "{line:?}");
    }
}

/// **The token never rides a redirect.** libcurl re-sends a custom `X-Plex-Token:` line to whatever
/// host a `Location:` names (it strips only `Authorization` and `Cookie`, and the television's 7.53.1
/// does not strip even those across hosts), so a request that follows redirects may carry no header of
/// the caller's at all: it is refused whole, nothing dialled. The same block on a redirect-free
/// request is sent.
#[test]
fn a_request_that_follows_redirects_is_refused_when_it_carries_a_caller_header() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    install_wire_user_agent();
    let token = vec!["X-Plex-Token: t".to_string()];
    assert!(refused_without_dialling(&token, true));
    assert!(refused_without_dialling(&["Accept: application/json".to_string()], true));
    let got = sent("GET", &token, None);
    assert!(got.contains("X-Plex-Token: t"), "{got:?}");
}
