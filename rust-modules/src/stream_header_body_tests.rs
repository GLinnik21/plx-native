//! Header parsing, body-completeness accounting, and chunked/status response handling.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// Regression: the header block was parsed as STRICT UTF-8 (`from_utf8(…).unwrap_or("")`), so a
/// single non-UTF-8 byte anywhere in it — here a lone Latin-1 `0xE9` in a header value, which
/// is exactly what a PMS echo of a filename or title produces — emptied the whole block. The
/// status then stayed 0, and `http_open`'s `status < 200` check closed a perfectly good 200 and
/// reported it as a transport failure: an unplayable item / a missing poster with a healthy
/// server on the other end. The response is otherwise entirely well formed.
#[test]
fn one_non_utf8_byte_in_a_header_does_not_turn_a_200_into_a_failure() {
    let mut resp: Vec<u8> = Vec::new();
    resp.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Plex-Title: caf");
    resp.push(0xE9); // 'é' in Latin-1 — a lone 0xE9 is not valid UTF-8 in any position
    resp.extend_from_slice(b"\r\n\r\nhello");
    let (port, h) = one_shot_server(resp);

    let (mut hs, rv) = open_against(port);
    assert_eq!(
        rv, 0,
        "a 200 must open, whatever bytes the other headers carry"
    );
    assert_eq!(
        hs.status, 200,
        "the status line is ASCII and was never in doubt"
    );
    assert_eq!(
        hs.content_length, 5,
        "…and Content-Length must survive the same way"
    );

    let mut body = Vec::new();
    let mut chunk = [0u8; 16];
    loop {
        let r = http_read(&mut *hs, chunk.as_mut_ptr(), chunk.len() as c_int);
        if r <= 0 {
            break;
        }
        body.extend_from_slice(&chunk[..r as usize]);
    }
    assert_eq!(
        body.as_slice(),
        b"hello",
        "the body must be delivered intact"
    );
    http_close(&mut *hs);
    h.join().unwrap();
}

/// The status extraction indexed a fixed `[9..]`. On the old `&str` that panicked outright when
/// a multi-byte character straddled index 9 — a panic inside `http_open`, which runs on the
/// demux and poster workers as well as the main loop. A garbage status line must be REJECTED
/// (status 0 → open fails), never fatal.
#[test]
fn a_status_line_that_straddles_the_status_offset_is_rejected_not_fatal() {
    // The 4-byte U+1F600 starts at index 7, so it OWNS index 9 — the old `&str[9..]` split it.
    let (port, h) = one_shot_server("HTTP/1.\u{1F600} 200 OK\r\n\r\n".as_bytes().to_vec());

    let (mut hs, rv) = open_against(port);
    assert_eq!(rv, -1, "an unparseable status line must fail the open");
    assert_eq!(hs.status, 0, "…with no status invented for it");
    assert_eq!(
        hs.fd(),
        -1,
        "a failed open retires its fd (see the leak test above)"
    );
    http_close(&mut *hs); // already closed by the failure path; keeps the intent explicit
    h.join().unwrap();
}

/// Header field NAMES are case-insensitive (RFC 9110 §5.1) and PMS does not send one casing
/// consistently. The old code got that from lowercasing the whole block; `find_ci` has to give
/// it back, and — the part that matters to the caller — the offset it returns must index the
/// ORIGINAL bytes, since the value is read from there. Tested directly rather than through a
/// loopback round trip: it is a pure function, and every socket a test holds open is one the
/// fd-leak test above can miscount while the two run in parallel.
#[test]
fn header_names_are_found_whatever_their_casing() {
    let hdr = b"HTTP/1.1 200 OK\r\nCONTENT-Length: 42\r\nTransfer-Encoding: chunked\r\n\r\n";
    let p = find_ci(hdr, b"\r\ncontent-length:").expect("a shouted header name must be found");
    assert_eq!(
        &hdr[p + 17..p + 20],
        b" 42",
        "the offset must index the ORIGINAL bytes"
    );
    assert!(find_ci(hdr, b"\r\ntransfer-encoding: chunked").is_some());
    assert!(
        find_ci(hdr, b"\r\ncontent-range:").is_none(),
        "no false positives"
    );
    assert!(
        find_ci(b"HT", b"\r\ncontent-length:").is_none(),
        "a needle longer than the hay"
    );
}

/// The redaction rule these log lines rest on: what reaches the event log is the endpoint, and
/// a query string never is. `with_token` is "the ONLY place `X-Plex-Token` is appended"
/// (`plex/client.rs`'s own module doc) and it appends it to the QUERY, while the event log is
/// what a user pastes into a public issue thread — so this is graded on the token being
/// ABSENT, not on the split being pretty.
#[test]
fn a_logged_endpoint_drops_the_query_and_with_it_the_token() {
    let p = "/library/metadata/4/children?includeChildren=1&X-Plex-Token=aBcD1234xyzQ";
    assert_eq!(log_endpoint(p), "/library/metadata/4/children");
    assert!(
        !log_endpoint(p).contains("X-Plex-Token"),
        "the token reached the log line"
    );
    assert!(!log_endpoint(p).contains("aBcD1234xyzQ"));
    // A poster path arrives with the token already in it (`Client::fetch_built`).
    let poster = "/photo/:/transcode?width=300&url=%2Flibrary%2F1&X-Plex-Token=aBcD1234xyzQ";
    assert_eq!(log_endpoint(poster), "/photo/:/transcode");
    assert_eq!(
        log_endpoint("/identity"),
        "/identity",
        "a path with no query is itself"
    );
    assert_eq!(
        log_endpoint("?X-Plex-Token=t"),
        "",
        "a path that is nothing but a query"
    );
}

/// The completeness test itself. A body that reached its `Content-Length` reports nothing; one
/// that stopped short reports how far it got and what was owed, and names WHICH end it was —
/// a mid-body `SO_RCVTIMEO` (recv error) reads nothing like a peer that closed early (EOF),
/// which is why the read loops keep -1 and 0 apart rather than folding them into `r <= 0`.
#[test]
fn a_short_body_is_reported_and_a_complete_one_is_not() {
    assert_eq!(
        short_body_line("GET", "/hubs?X-Plex-Token=t", 5000, 5000, false, false),
        None,
        "a body that reached its length is whole"
    );
    assert_eq!(
        short_body_line("GET", "/hubs", 5001, 5000, false, false),
        None,
        "…and one past it is not short either"
    );

    let l = short_body_line(
        "GET",
        "/hubs?X-Plex-Token=aBcD1234xyzQ",
        900,
        5000,
        false,
        false,
    )
    .expect("a body 900 bytes into a 5000-byte response must be reported");
    assert!(l.contains("SHORT BODY got=900 want=5000"), "{l}");
    assert!(
        l.contains("/hubs") && !l.contains("aBcD1234xyzQ"),
        "the line leaked the query: {l}"
    );
    assert!(
        l.contains("EOF"),
        "a clean end must not read as an error: {l}"
    );

    let e = short_body_line("POST", "/playQueues", 900, 5000, false, true).expect("reported");
    assert!(
        e.contains("recv error"),
        "a recv error must be named as one: {e}"
    );
    assert!(
        e.starts_with("stream: POST "),
        "the verb belongs on the line: {e}"
    );

    // No length at all — a close-delimited body. A clean end is the ONLY end it has, so
    // silence; an error is still an error, with nothing to state as `want`.
    assert_eq!(short_body_line("GET", "/x", 900, -1, false, false), None);
    let u = short_body_line("GET", "/x", 900, -1, false, true).expect("reported");
    assert!(u.contains("got=900 want=? (recv error)"), "{u}");
}

/// Chunked framing has no `Content-Length` to fall short of, and `http_read`'s chunked branch
/// counts DECODED bytes into `consumed` without ever reading the field — so the length test
/// must not run there, INCLUDING for a server that sent both headers, where the two numbers
/// are not the same quantity. Only a recv error can call a chunked transfer incomplete.
#[test]
fn a_chunked_response_cannot_report_a_short_body_on_length() {
    assert_eq!(
        short_body_line("GET", "/x", 900, -1, true, false),
        None,
        "the ordinary chunked case: no length, clean end"
    );
    assert_eq!(
        short_body_line("GET", "/x", 900, 5000, true, false),
        None,
        "both headers present — the chunked framing wins, the length means nothing"
    );
    let e = short_body_line("GET", "/x", 900, 5000, true, true).expect("reported");
    assert!(
        e.contains("want=?"),
        "a chunked transfer owes no stated length: {e}"
    );
}

/// The chunked detection was `find_ci(hdr, b"\r\ntransfer-encoding: chunked")` — one exact
/// spelling, single space, single token. Every other legal way to write the same header missed,
/// and a miss is not a failure: the body is then read as close-delimited with the chunk-size
/// lines left INLINE in it, i.e. silent corruption of whatever the caller parses next.
#[test]
fn chunked_is_recognised_however_the_header_is_spelled() {
    let hdr = |te: &str| format!("HTTP/1.1 200 OK\r\n{te}\r\n\r\n").into_bytes();
    for te in [
        "Transfer-Encoding: chunked",   // the one spelling that already worked
        "Transfer-Encoding:chunked",    // OWS after the colon is OPTIONAL (RFC 9110 §5.6.3)
        "Transfer-Encoding:   chunked", // …and may be more than one
        "Transfer-Encoding: chunked ",  // trailing OWS is not part of the value
        "Transfer-Encoding: Chunked",   // the VALUE is case-insensitive too (§10.1.4)
        "transfer-encoding: CHUNKED",
        "Transfer-Encoding: gzip, chunked", // the legal list form: chunked applied LAST
        "Transfer-Encoding: chunked, gzip", // malformed per §6.1, but the framing IS chunked
        "Transfer-Encoding: gzip\r\nTransfer-Encoding: chunked", // a list split across lines
    ] {
        assert!(header_is_chunked(&hdr(te)), "missed: {te}");
    }
    for te in [
        "Transfer-Encoding: gzip",
        "Transfer-Encoding: chunkedy", // a token that merely starts the same way
        "Transfer-Encoding: xchunked",
        "Content-Length: 5",
        "X-Chunked: chunked", // not the field this decides on
    ] {
        assert!(!header_is_chunked(&hdr(te)), "false positive: {te}");
    }
    // Termination, not just correctness: a block whose last line has no CRLF must still end the
    // scan rather than spin on the same offset.
    assert!(!header_is_chunked(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip"
    ));
    assert!(header_is_chunked(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked"
    ));
    assert!(!header_is_chunked(b""));
}

/// …and the spelling reaches the READ path, not merely the predicate. A server answering
/// `Transfer-Encoding:chunked` (no space) used to hand its caller `4\r\nabcd\r\n0\r\n\r\n`
/// verbatim — a body that parses as neither JSON nor a media stream, from a healthy server.
#[test]
fn a_chunked_body_spelled_without_a_space_is_still_decoded() {
    let (port, h) = one_shot_server(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding:chunked\r\n\r\n4\r\nabcd\r\n3\r\nefg\r\n0\r\n\r\n".to_vec());
    let r = loopback_get(port).expect("a 200 must open");
    assert_eq!(
        (r.status, r.body.as_slice()),
        (200, &b"abcdefg"[..]),
        "the chunk framing was left in the body"
    );
    h.join().unwrap();
}

/// The constraint the reporting is bound by: it is observability only. A server that promises
/// 10 bytes and closes after 4 still hands the caller those 4 bytes as `Some(body)`, so what
/// the data layer does with them (`get_json`'s serde failure, `.ok()`-folded to `None`) is
/// decided exactly where it was — the event log is the only thing that gained a fact.
#[test]
fn a_truncated_body_is_still_returned_to_the_caller() {
    let (port, h) =
        one_shot_server(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabcd".to_vec());
    let r = loopback_get(port)
        .expect("a truncated body is still a body — this must not become None");
    assert_eq!(
        r.body.as_slice(),
        b"abcd",
        "the bytes that did arrive must be handed over intact"
    );
    assert_eq!(
        r.status, 200,
        "…and the server's own verdict travels beside them"
    );
    h.join().unwrap();
}

/// **A non-2xx is a RESPONSE, and it must arrive as one.** The wrapper these two tests used to
/// call answered `None` here, indistinguishable from a refused connection — the collapse the
/// whole `plex::probe::Outcome` model is built to avoid, and the reason `crate::http` replaced
/// it. Graded against a real socket rather than against the header parser, because what is
/// being asserted is the composition: `http_open` reports the failure through its return value
/// and leaves the code on the struct, and only reading `hs_status` afterwards recovers it.
#[test]
fn a_401_reaches_the_caller_as_a_status_and_not_as_a_transport_failure() {
    let (port, h) =
        one_shot_server(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".to_vec());
    let r = loopback_get(port).expect("the server ANSWERED — that is not a transport failure");
    assert_eq!(r.status, 401);
    assert!(!r.ok(), "…and it is still not a success");
    h.join().unwrap();
}

/// Regression: the caller used to receive only `-1` and infer `deadline` afterwards. A valid
/// PMS 500 parsed just before that caller was descheduled across the boundary therefore wore a
/// timeout and triggered a reserve retry. The transport has the exact status while parsing the
/// head; crossing the clock afterwards cannot erase that fact.
#[test]
fn a_500_response_remains_a_status_after_its_deadline_passes() {
    let (port, h) = one_shot_server(
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n".to_vec(),
    );
    let host = std::ffi::CString::new("127.0.0.1").unwrap();
    let path = std::ffi::CString::new("/boundary").unwrap();
    let mut hs = http_stream_boxed();
    let deadline = Instant::now() + std::time::Duration::from_millis(250);

    let result = http_open_until_result(
        &mut *hs,
        host.as_ptr(),
        port as c_int,
        path.as_ptr(),
        std::ptr::null(),
        "GET",
        deadline,
    );
    h.join().unwrap();
    let until_boundary = deadline.saturating_duration_since(Instant::now());
    if !until_boundary.is_zero() {
        std::thread::sleep(until_boundary + std::time::Duration::from_millis(10));
    }

    assert!(
        Instant::now() >= deadline,
        "the fixture did not cross its deadline"
    );
    assert_eq!(
        result,
        Err(HttpOpenError::Status(500)),
        "a known server response must not become a retrospective timeout"
    );
    assert_eq!(
        hs_status(&*hs),
        500,
        "the response code must also remain observable on hs"
    );
    assert_eq!(
        hs.fd(),
        -1,
        "a rejected response must retire the published fd"
    );
}

/// Nothing listening is the OTHER outcome, and it must not wear a status. `0` is what
/// `http_open`'s parser leaves when no `HTTP/1.x NNN` line ever arrived, and `crate::http`
/// turns that into `None` so a caller cannot read it as a refusal (`classify` would score it
/// `Unreachable` either way, but by luck rather than by decision).
#[test]
fn a_connection_that_never_answers_is_none_rather_than_a_status_of_zero() {
    // Bind and drop, so the port is one nothing is listening on any more.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        l.local_addr().expect("addr").port()
    };
    assert!(loopback_get(port).is_none());
}
