//! The plaintext player request names the client with the `User-Agent` boot installed.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// The plaintext path used to hard-code `User-Agent: plxnative/0.1`, so a Plex server reached
/// over plain HTTP logged the player under a name nothing else in the process used, and a host
/// that installed its own name with `set_user_agent` still had this one go out. The request must
/// carry the installed value, the same one every curl request sends.
#[test]
fn a_plaintext_request_sends_the_installed_user_agent() {
    const INSTALLED: &str = "PlxNativeTest/1.2.3 (stream)";
    crate::net::set_user_agent(INSTALLED.to_string()).expect("a plain value");
    let (port, h) = one_shot_echo(
        "127.0.0.1:0",
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
    )
    .expect("bind the v4 loopback");

    let (mut hs, rv) = open_against(port);
    assert_eq!(rv, 0, "the 200 opens");
    http_close(&mut *hs);
    let req = String::from_utf8_lossy(&h.join().unwrap()).into_owned();

    let agents: Vec<&str> = req
        .split("\r\n")
        .filter(|l| l.to_ascii_lowercase().starts_with("user-agent:"))
        .collect();
    assert_eq!(
        agents,
        [format!("User-Agent: {INSTALLED}")],
        "exactly one User-Agent, the installed one: {req:?}"
    );
}
