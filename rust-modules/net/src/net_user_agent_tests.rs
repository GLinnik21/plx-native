//! The `User-Agent` boot installs reaches the wire as one header value, never as more than one, on
//! every hop of every request the easy path makes, and a request made before boot installs one sends
//! none. The plaintext path and the player's HTTPS source have their own wire tests
//! (`stream_user_agent_tests.rs`, and `curlio_redirect_tests.rs` in the `media` crate).
//!
//! The installed value is a `OnceLock`: the first `set_user_agent` of the test binary wins and
//! nothing takes it back. So every test in this crate that installs one installs
//! [`WIRE_USER_AGENT`] through [`install_wire_user_agent`], and the "nothing installed" test runs in
//! a child process of its own.

use super::*;
use std::sync::Arc;

/// The one value every wire test of this crate installs.
pub(crate) const WIRE_USER_AGENT: &str = "PlxNativeTest/1.2.3 (wire)";

/// Install [`WIRE_USER_AGENT`] and prove it is the installed one: the first call of the process
/// wins, so a test that installed a different value first would otherwise be graded against it.
pub(crate) fn install_wire_user_agent() {
    set_user_agent(WIRE_USER_AGENT.to_string()).expect("a plain value");
    assert_eq!(
        user_agent(),
        Some(WIRE_USER_AGENT),
        "another test installed a different User-Agent first: install WIRE_USER_AGENT only"
    );
}

/// The value of every `User-Agent` header in the head of a recorded request, in order.
pub(crate) fn user_agents(request: &str) -> Vec<String> {
    let head = request.split("\r\n\r\n").next().unwrap_or_default();
    head.split("\r\n")
        .skip(1) // the request line
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim().eq_ignore_ascii_case("user-agent").then(|| value.trim().to_owned())
        })
        .collect()
}

/// Every request `served` has read, verbatim, in arrival order.
fn recorded(served: &Observed) -> Vec<String> {
    served
        .requests
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|r| String::from_utf8_lossy(r).into_owned())
        .collect()
}

/// A value whose second line would be a header of its own on the plaintext path, which writes the
/// installed value into the request text unescaped.
const INJECTED: &str = "PlxNative/1.0\r\nX-Injected: 1";

/// Whatever `set_user_agent` was given, what the transport would send holds no control character:
/// a CR or LF splits the request, and a NUL ends the C string curl reads while the plaintext path
/// would still write it. The process installs one value and keeps it, so this does not depend on
/// which other test installed its own first.
#[test]
fn a_user_agent_with_a_line_break_is_refused_and_never_installed() {
    assert_eq!(set_user_agent(INJECTED.to_string()), Err(InvalidUserAgent));
    assert!(
        user_agent().map_or(true, |ua| !ua.chars().any(char::is_control)),
        "installed: {:?}",
        user_agent()
    );
}

#[test]
fn every_control_character_is_refused_and_a_plain_value_is_kept_as_written() {
    for bad in ["a\rb", "a\nb", "a\0b", "a\tb", "a\x1bb", "a\x7fb", "a\u{85}b", "\r\n"] {
        assert!(UserAgent::new(bad.to_string()).is_err(), "{bad:?}");
    }
    let ua = UserAgent::new("PlxNative/0.9 (webOS; LG) Ünïcode".to_string()).expect("plain text");
    assert_eq!(ua.text, "PlxNative/0.9 (webOS; LG) Ünïcode");
    assert_eq!(ua.c_string.to_str(), Ok(ua.text.as_str()), "the curl string is the same value");
}

/// The easy path, which every account, PMS control, image, telemetry and lab request passes
/// (`request_tls_evidence`): the installed value rides the first request AND the hop a redirect
/// adds, exactly once with or without a caller block (a caller's own `User-Agent:` line is refused,
/// `net_wire_characterization_tests.rs`), and on the verb the telemetry senders use
/// (a POST with a body, through the shipped-bundle `Tls` mode) as on a GET.
#[test]
fn every_hop_and_verb_of_an_easy_request_sends_the_installed_user_agent() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    install_wire_user_agent();
    let cert = Arc::new(mint_ca_issued_cert(&["127.0.0.1"], ymd_from_now(-1), ymd_from_now(30)));
    let _ca = TestCaGuard::install(&cert.pem, "ua-wire-easy");
    let served = spawn_scripted(
        cert,
        [
            ("/start", Reply::redirect(302, "/next")),
            ("/next", Reply::ok(b"{}".to_vec())),
            ("/post", Reply::ok(b"{}".to_vec())),
        ],
    );
    let _watch = keypin::Scoped::watch(&keypin::key_of("127.0.0.1", i32::from(served.port)));
    let base = format!("https://127.0.0.1:{}", served.port);

    // A request that follows redirects carries no header of the caller's (`headers`' redirect rule),
    // so the hop is graded headerless; the caller's own block rides the next, redirect-free request.
    let got = request_result_evidence(&format!("{base}/start"), &[], "GET", None, API, true, None, None, false)
        .expect("the redirect is followed and answered");
    assert_eq!(got.status, 200);
    let with_block = request_result_evidence(
        &format!("{base}/next"),
        &["X-Plex-Token: t".to_string(), "Accept: application/json".to_string()],
        "GET",
        None,
        API,
        false,
        None,
        None,
        false,
    )
    .expect("a request with the caller's block is answered");
    assert_eq!(with_block.status, 200);
    let bundle = test_ca_bundle::get().expect("the guard installed one");
    let posted = request_tls(&format!("{base}/post"), &[], "POST", Some(b"{\"a\":1}"), API, false, Some(4096), Tls::CaBundle(&bundle))
        .expect("the POST is answered");
    assert_eq!(posted.status, 200);

    let seen = recorded(&served);
    let lines: Vec<&str> = seen.iter().map(|r| r.lines().next().unwrap_or_default()).collect();
    assert_eq!(
        lines,
        ["GET /start HTTP/1.1", "GET /next HTTP/1.1", "GET /next HTTP/1.1", "POST /post HTTP/1.1"]
    );
    for request in &seen {
        assert_eq!(user_agents(request), [WIRE_USER_AGENT], "exactly one, the installed one: {request:?}");
    }
}

/// What the easy path sends before boot has installed a value (a host test that never booted, or
/// boot refused the value): no `User-Agent` at all, which is all libcurl sends by default, and the
/// plaintext path's line is empty. Installing later takes effect on the next request, because the
/// value is read per handle. Runs in a child process: any other test of this binary may have
/// installed one already, and an install cannot be taken back.
#[test]
fn an_easy_request_made_before_a_user_agent_is_installed_sends_none() {
    if !plx_base::testlock::fresh_process(module_path!(), "an_easy_request_made_before_a_user_agent_is_installed_sends_none") {
        return;
    }
    assert_eq!(user_agent(), None, "the child must start with nothing installed");
    assert_eq!(user_agent_header_line(), "", "the hand-built request text gets no line either");
    if !curl_ready() { return; }
    let reply = || b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
    let get = |port: u16| {
        request(&format!("http://127.0.0.1:{port}/"), &[], "GET", None, API, false, None, None).expect("answered")
    };

    let (port, head) = crate::stream::test_support::one_shot_echo("127.0.0.1:0", reply()).expect("bind");
    assert_eq!(get(port).status, 200);
    let before = String::from_utf8_lossy(&head.join().unwrap()).into_owned();
    assert!(before.starts_with("GET / HTTP/1.1\r\n"), "the capture saw a request head: {before:?}");
    assert_eq!(user_agents(&before), Vec::<String>::new(), "nothing installed, nothing sent: {before:?}");

    install_wire_user_agent();
    let (port, head) = crate::stream::test_support::one_shot_echo("127.0.0.1:0", reply()).expect("bind");
    assert_eq!(get(port).status, 200);
    let after = String::from_utf8_lossy(&head.join().unwrap()).into_owned();
    assert_eq!(user_agents(&after), [WIRE_USER_AGENT], "the same capture sees one once it is installed: {after:?}");
}
