//! The bytes `perform_http_request` writes, pinned exactly as the plaintext path sends them today.
//!
//! The head is `{method} {path} HTTP/1.1\r\nHost: {host}\r\n{User-Agent}{Accept}{extra}\r\n`, and
//! `extra` is the caller's block, spliced in verbatim once `net::headers` has passed it. These tests feed the shapes the real
//! producers build (`plex::http::plaintext`'s `Connection: close` block, `stream::redirect`'s
//! credential and `Range` lines) and compare WHOLE heads, so any change to the bytes of a request a
//! producer already makes shows up as a failure rather than as a behaviour somebody notices later.
//!
//! The two cases at the bottom are the validation of `net::headers`: `Accept` is detected by NAME, and a
//! block that is not well formed, or names a header the transport writes itself, is refused before a
//! socket exists. They replaced three cases that recorded the old behaviour (an `X-Accept:` line
//! suppressing the default `Accept`, a second `Host:` and a second `User-Agent:` on the wire).

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

/// The default `Accept` is suppressed by a header NAMED `Accept`, not by a substring of the block: a
/// header that merely ends in `accept:` (`X-Accept:`) leaves it in place.
#[test]
fn an_x_accept_header_does_not_suppress_the_default_accept() {
    install_wire_user_agent();
    let head = head_with(Some("X-Accept: nothing\r\n"), "GET");
    assert!(head.contains("\r\nAccept: */*\r\n"), "{head:?}");
    assert!(head.contains("X-Accept: nothing\r\n"), "{head:?}");
}

/// Open `GET /x` against a loopback listener with `extra` as the caller's block, and report whether
/// the open was refused AND nothing was ever dialled: the listener never sees a connection.
fn refused_without_dialling(extra: &str) -> bool {
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    srv.set_nonblocking(true).expect("nonblocking");
    let port = srv.local_addr().unwrap().port();
    let host = std::ffi::CString::new("127.0.0.1").unwrap();
    let path = std::ffi::CString::new("/x").unwrap();
    let extra = std::ffi::CString::new(extra).unwrap();
    let mut hs = http_stream_boxed();
    let rv = http_open(&mut *hs, host.as_ptr(), port as c_int, path.as_ptr(), extra.as_ptr(), "GET");
    http_close(&mut *hs);
    // Give a connect that did happen a moment to land in the backlog.
    std::thread::sleep(std::time::Duration::from_millis(50));
    let dialled = matches!(srv.accept(), Ok(_));
    rv == -1 && !dialled
}

/// A caller's block cannot add a second `Host:` or `User-Agent:` to the head, nor end it early, nor
/// start another header from inside a value: the whole open is refused before a socket exists.
#[test]
fn a_block_naming_a_header_the_transport_writes_or_breaking_the_head_is_refused_before_dialling() {
    install_wire_user_agent();
    for extra in [
        "Host: evil.example\r\n",
        "User-Agent: other\r\n",
        "user-agent: other\r\n",
        "Content-Length: 5\r\n",
        "Transfer-Encoding: chunked\r\n",
        "X-Plex-Token: t\r\n\r\nGET /smuggled HTTP/1.1\r\n",
        "X-A: 1\nUser-Agent: other\r\n",
        "X-A: 1",
        "not a header\r\n",
    ] {
        assert!(refused_without_dialling(extra), "{extra:?}");
    }
}
