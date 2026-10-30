//! Finding a Jellyfin server on the local network, so signing in needs no typing.
//!
//! Jellyfin answers a UDP broadcast of `Who is JellyfinServer?` on port 7359 with one JSON object
//! per server: `{"Address":"http://192.168.1.20:8096","Id":"...","Name":"Home",...}`. This sends
//! the question a few times (a broadcast can be dropped) and collects every distinct answer.

use serde::Deserialize;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

const PORT: u16 = 7359;
const QUESTION: &[u8] = b"Who is JellyfinServer?";

/// One server that answered.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Found {
    pub address: String,
    pub id: String,
    pub name: String,
}

/// Parse one answer. Anything that isn't a server's answer (our own broadcast echoed back, a
/// different service on the port) is `None`.
pub fn parse(datagram: &[u8]) -> Option<Found> {
    let f: Found = serde_json::from_slice(datagram).ok()?;
    (!f.address.is_empty() && !f.id.is_empty()).then_some(f)
}

/// Ask the network for `wait` and return what answered, first answer first.
pub fn discover(wait: Duration) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    let Ok(sock) = UdpSocket::bind(("0.0.0.0", 0)) else { return found };
    if sock.set_broadcast(true).is_err() {
        return found;
    }
    let _ = sock.set_read_timeout(Some(Duration::from_millis(250)));
    let start = Instant::now();
    let mut next_ask = start;
    let mut buf = [0u8; 2048];
    while start.elapsed() < wait {
        if Instant::now() >= next_ask {
            let _ = sock.send_to(QUESTION, ("255.255.255.255", PORT));
            next_ask += Duration::from_millis(700);
        }
        if let Ok((n, _)) = sock.recv_from(&mut buf) {
            if let Some(f) = parse(&buf[..n]) {
                if !found.iter().any(|g| g.id == f.id) {
                    found.push(f);
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_names_the_server_and_anything_else_is_ignored() {
        let f = parse(br#"{"Address":"http://192.168.1.20:8096","Id":"abc","Name":"Home","EndpointAddress":null}"#).unwrap();
        assert_eq!((f.address.as_str(), f.id.as_str(), f.name.as_str()), ("http://192.168.1.20:8096", "abc", "Home"));
        assert_eq!(parse(QUESTION), None);
        assert_eq!(parse(br#"{"Address":"","Id":"abc","Name":"x"}"#), None);
    }
}
