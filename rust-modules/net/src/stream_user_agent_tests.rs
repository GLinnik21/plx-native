//! The plaintext player request names the client with the `User-Agent` boot installs, on the first
//! request and on every redirect hop, and sends none before one is installed.
//!
//! The installed value is a process-wide `OnceLock`, so every test here installs the crate's one
//! wire value (`crate::net::user_agent_tests::install_wire_user_agent`) and the "nothing installed"
//! case runs in a child process of its own.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use crate::net::user_agent_tests::{install_wire_user_agent, user_agents, WIRE_USER_AGENT};

/// The plaintext path used to hard-code `User-Agent: plxnative/0.1`, so a Plex server reached
/// over plain HTTP logged the player under a name nothing else in the process used, and a host
/// that installed its own name with `set_user_agent` still had this one go out. The request must
/// carry the installed value, the same one every curl request sends.
#[test]
fn a_plaintext_request_sends_the_installed_user_agent() {
    install_wire_user_agent();
    let (port, h) = one_shot_echo(
        "127.0.0.1:0",
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
    )
    .expect("bind the v4 loopback");

    let (mut hs, rv) = open_against(port);
    assert_eq!(rv, 0, "the 200 opens");
    http_close(&mut *hs);
    let req = String::from_utf8_lossy(&h.join().unwrap()).into_owned();

    assert_eq!(
        user_agents(&req),
        [WIRE_USER_AGENT],
        "exactly one User-Agent, the installed one: {req:?}"
    );
}

/// A followed hop is a request of its own, built by the same `perform_http_request`: the hop to
/// another origin carries the installed name too, as the curl source's hops do.
#[test]
fn every_plaintext_redirect_hop_sends_the_installed_user_agent() {
    use crate::net::origin::Origin;
    use redirect::{open_following, Opened, Request};
    use std::io::{Read, Write};

    fn allow_all(_: &Origin, _: &str, _: &[&str]) -> bool {
        true
    }
    let serve = |reply: Vec<u8>| {
        let srv = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = srv.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = srv.accept().unwrap();
            let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            let mut raw = Vec::new();
            let mut c = [0u8; 2048];
            while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut c) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => raw.extend_from_slice(&c[..n]),
                }
            }
            let _ = s.write_all(&reply);
            String::from_utf8_lossy(&raw).into_owned()
        });
        (port, h)
    };
    install_wire_user_agent();
    let (cdn, cdn_h) = serve(
        b"HTTP/1.1 206 Partial Content\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec(),
    );
    let (pms, pms_h) = serve(
        format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{cdn}/v\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .into_bytes(),
    );
    let origin = Origin::http("127.0.0.1", pms as i32);
    let mut hs = http_stream_boxed();
    let req = Request {
        origin: &origin,
        path: "/p",
        credentials: None,
        range_from: None,
        deadline: None,
        same_origin_only: false,
        credential_gate: allow_all,
    };
    let got = open_following(&mut *hs, &req, &mut plx_base::checkpoint::NoCheckpoint);
    http_close(&mut *hs);
    let (first, hop) = (pms_h.join().unwrap(), cdn_h.join().unwrap());
    match got {
        Ok(Opened::Socket(t)) => assert_eq!(t.origin.port(), cdn as i32, "the redirect was followed"),
        other => panic!("{other:?}"),
    }
    assert_eq!(user_agents(&first), [WIRE_USER_AGENT], "{first:?}");
    assert_eq!(user_agents(&hop), [WIRE_USER_AGENT], "the hop carries the name too: {hop:?}");
}

/// Before boot installs a value (a host test that never booted, or boot refused it) the request
/// carries no `User-Agent` line at all, and installing one later reaches the next request, because
/// the line is built per request. Runs in a child process: any other test of this binary may have
/// installed a value already, and an install cannot be taken back.
#[test]
fn a_plaintext_request_made_before_a_user_agent_is_installed_sends_none() {
    if !plx_base::testlock::fresh_process(module_path!(), "a_plaintext_request_made_before_a_user_agent_is_installed_sends_none") {
        return;
    }
    assert_eq!(crate::net::user_agent(), None, "the child must start with nothing installed");
    let reply = || b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec();
    let head_of = || {
        let (port, h) = one_shot_echo("127.0.0.1:0", reply()).expect("bind the v4 loopback");
        let (mut hs, rv) = open_against(port);
        assert_eq!(rv, 0, "the 200 opens");
        http_close(&mut *hs);
        String::from_utf8_lossy(&h.join().unwrap()).into_owned()
    };

    let before = head_of();
    assert!(before.starts_with("GET /x HTTP/1.1\r\n"), "the capture saw a request head: {before:?}");
    assert_eq!(user_agents(&before), Vec::<String>::new(), "nothing installed, nothing sent: {before:?}");

    install_wire_user_agent();
    let after = head_of();
    assert_eq!(user_agents(&after), [WIRE_USER_AGENT], "the same capture sees one once it is installed: {after:?}");
}
