//! The bytes `perform_http_request` writes, pinned exactly as the plaintext path sends them today.
//!
//! The head is `{method} {path} HTTP/1.1\r\nHost: {host}\r\n{User-Agent}{Accept}{extra}\r\n`, and
//! `extra` is the caller's blocks spliced in verbatim. These tests feed the shapes the real
//! producers build (`plex::http::plaintext`'s `Connection: close` block, `stream::redirect`'s
//! credential and `Range` lines) and compare WHOLE heads, so any change to the bytes of a request a
//! producer already makes shows up as a failure rather than as a behaviour somebody notices later.
//!
//! The three cases at the bottom (`Accept:` detection by substring, a duplicate `Host:`) record what
//! the head does with input that is not well formed, which is the behaviour the request validation
//! of the next change deliberately replaces; they are the visible flip.

use super::*;
use super::test_support::*;
use crate::net::user_agent_tests::{install_wire_user_agent, WIRE_USER_AGENT};

/// Open a `GET /x` against a one-shot loopback server with `extra` as the caller's header block,
/// and return the request head the server read.
fn head_with(extra: Option<&str>, method: &str) -> String {
    let (port, served) =
        one_shot_echo("127.0.0.1:0", b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec()).expect("bind");
    let host = std::ffi::CString::new("127.0.0.1").unwrap();
    let path = std::ffi::CString::new("/x").unwrap();
    let extra = extra.map(|e| std::ffi::CString::new(e).unwrap());
    let mut hs = http_stream_boxed();
    let rv = http_open(
        &mut *hs,
        host.as_ptr(),
        port as c_int,
        path.as_ptr(),
        extra.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
        method,
    );
    assert_eq!(rv, 0, "the 200 opens");
    http_close(&mut *hs);
    String::from_utf8_lossy(&served.join().unwrap()).into_owned()
}

fn host_of(port_head: &str) -> &str {
    port_head
        .split("\r\n")
        .find_map(|l| l.strip_prefix("Host: "))
        .expect("a Host line")
}

#[test]
fn a_request_with_no_extra_block_is_exactly_the_request_line_host_user_agent_and_accept() {
    install_wire_user_agent();
    let head = head_with(None, "GET");
    let host = host_of(&head).to_owned();
    assert_eq!(
        head,
        format!("GET /x HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {WIRE_USER_AGENT}\r\nAccept: */*\r\n\r\n")
    );
}

/// The control plane's block (`plex::http::plaintext`): `Connection: close`, then the caller's lines.
#[test]
fn the_control_plane_block_is_spliced_in_after_the_user_agent_and_accept() {
    install_wire_user_agent();
    let head = head_with(Some("Connection: close\r\nX-Plex-Token: t\r\nX-Plex-Language: en\r\n"), "GET");
    let host = host_of(&head).to_owned();
    assert_eq!(
        head,
        format!(
            "GET /x HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {WIRE_USER_AGENT}\r\nAccept: */*\r\n\
             Connection: close\r\nX-Plex-Token: t\r\nX-Plex-Language: en\r\n\r\n"
        )
    );
}

/// The media plane's block (`stream::redirect::open_following`): credential lines, then `Range`.
#[test]
fn the_media_plane_block_carries_the_credential_lines_then_the_range() {
    install_wire_user_agent();
    let head = head_with(Some("X-Plex-Token: t\r\nRange: bytes=1048576-\r\n"), "GET");
    let host = host_of(&head).to_owned();
    assert_eq!(
        head,
        format!(
            "GET /x HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {WIRE_USER_AGENT}\r\nAccept: */*\r\n\
             X-Plex-Token: t\r\nRange: bytes=1048576-\r\n\r\n"
        )
    );
}

#[test]
fn a_non_get_verb_is_written_as_given() {
    install_wire_user_agent();
    let head = head_with(Some("Connection: close\r\n"), "PUT");
    assert!(head.starts_with("PUT /x HTTP/1.1\r\nHost: "), "{head:?}");
}

/// A caller's own `Accept` replaces the default one.
#[test]
fn a_caller_accept_header_replaces_the_default_accept() {
    install_wire_user_agent();
    let head = head_with(Some("Accept: application/json\r\n"), "GET");
    let host = host_of(&head).to_owned();
    assert_eq!(
        head,
        format!("GET /x HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {WIRE_USER_AGENT}\r\nAccept: application/json\r\n\r\n")
    );
}

/// RECORDED, NOT ENDORSED: the default `Accept` is suppressed by a substring test on the whole
/// block, so a header that merely ends in `accept:` (`X-Accept:`) removes it. The request validation
/// that follows replaces the test with an exact-name one, and this expectation flips there.
#[test]
fn an_x_accept_header_suppresses_the_default_accept_today() {
    install_wire_user_agent();
    let head = head_with(Some("X-Accept: nothing\r\n"), "GET");
    assert!(!head.contains("Accept: */*"), "{head:?}");
    assert!(head.contains("X-Accept: nothing\r\n"), "{head:?}");
}

/// RECORDED, NOT ENDORSED: a second `Host:` or `User-Agent:` line in the caller's block goes onto
/// the wire after the transport's own. The request validation that follows refuses both.
#[test]
fn a_duplicate_host_and_user_agent_in_the_extra_block_reach_the_wire_today() {
    install_wire_user_agent();
    let head = head_with(Some("Host: evil.example\r\nUser-Agent: other\r\n"), "GET");
    assert_eq!(head.matches("\r\nHost: ").count(), 2, "{head:?}");
    assert_eq!(head.matches("\r\nUser-Agent: ").count(), 2, "{head:?}");
}
