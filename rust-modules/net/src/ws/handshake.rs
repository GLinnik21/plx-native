use plx_base::{b64, sha1::sha1};

const ACCEPT_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const HEAD_LIMIT: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandshakeFault {
    Oversized,
    Malformed,
    Refused(u16),
    NotUpgraded,
    WrongAccept,
    Unrequested,
}

pub fn accept_key(key: &str) -> String {
    b64::encode(&sha1(format!("{key}{ACCEPT_GUID}").as_bytes()))
}

pub(super) enum HeadRead {
    Pending,
    Accepted(usize),
    Failed(HandshakeFault),
}

pub(super) struct Handshake {
    key: String,
    head: Vec<u8>,
}

impl Handshake {
    pub(super) fn new(nonce: [u8; 16]) -> Handshake {
        Handshake { key: b64::encode(&nonce), head: Vec::new() }
    }

    pub(super) fn request(&self, host: &str, path: &str) -> String {
        format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n{agent}\r\n",
            key = self.key,
            agent = crate::net::user_agent_header_line(),
        )
    }

    pub(super) fn read(&mut self, bytes: &[u8]) -> HeadRead {
        let before = self.head.len();
        let room = (HEAD_LIMIT + 1).saturating_sub(before);
        self.head.extend_from_slice(&bytes[..room.min(bytes.len())]);
        let from = before.saturating_sub(3);
        let Some(at) = self.head[from..].windows(4).position(|w| w == b"\r\n\r\n") else {
            return if self.head.len() > HEAD_LIMIT {
                HeadRead::Failed(HandshakeFault::Oversized)
            } else {
                HeadRead::Pending
            };
        };
        let end = from + at + 4;
        if end > HEAD_LIMIT {
            return HeadRead::Failed(HandshakeFault::Oversized);
        }
        let head = std::mem::take(&mut self.head);
        match self.verify(&head[..end]) {
            Ok(()) => HeadRead::Accepted(end - before),
            Err(fault) => HeadRead::Failed(fault),
        }
    }

    fn verify(&self, head: &[u8]) -> Result<(), HandshakeFault> {
        let text = std::str::from_utf8(head).map_err(|_| HandshakeFault::Malformed)?;
        let mut lines = text.split("\r\n");
        let mut status = lines.next().unwrap_or_default().splitn(3, ' ');
        let (Some("HTTP/1.1"), Some(code)) = (status.next(), status.next()) else {
            return Err(HandshakeFault::Malformed);
        };
        if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(HandshakeFault::Malformed);
        }
        let code: u16 = code.parse().map_err(|_| HandshakeFault::Malformed)?;
        if code != 101 {
            return Err(HandshakeFault::Refused(code));
        }
        let (mut upgrade, mut connection, mut accepts) = (false, false, Vec::new());
        for line in lines.filter(|line| !line.is_empty()) {
            let (name, value) = line.split_once(':').ok_or(HandshakeFault::Malformed)?;
            let value = value.trim();
            if name.eq_ignore_ascii_case("upgrade") {
                upgrade |= value.eq_ignore_ascii_case("websocket");
            } else if name.eq_ignore_ascii_case("connection") {
                connection |= value.split(',').any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
            } else if name.eq_ignore_ascii_case("sec-websocket-accept") {
                accepts.push(value);
            } else if (name.eq_ignore_ascii_case("sec-websocket-extensions")
                || name.eq_ignore_ascii_case("sec-websocket-protocol"))
                && !value.is_empty()
            {
                return Err(HandshakeFault::Unrequested);
            }
        }
        if !upgrade || !connection {
            return Err(HandshakeFault::NotUpgraded);
        }
        match accepts.as_slice() {
            [accept] if *accept == accept_key(&self.key) => Ok(()),
            _ => Err(HandshakeFault::WrongAccept),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_NONCE: [u8; 16] = *b"the sample nonce";

    fn response(accept: &str, extra: &str) -> String {
        format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n{extra}\r\n"
        )
    }

    fn verdict(text: &str) -> Result<usize, HandshakeFault> {
        match Handshake::new(SAMPLE_NONCE).read(text.as_bytes()) {
            HeadRead::Accepted(used) => Ok(used),
            HeadRead::Failed(fault) => Err(fault),
            HeadRead::Pending => panic!("the head did not end: {text:?}"),
        }
    }

    #[test]
    fn the_accept_matches_rfc_6455_section_1_3() {
        assert_eq!(b64::encode(&SAMPLE_NONCE), "dGhlIHNhbXBsZSBub25jZQ==");
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn the_request_asks_for_version_13_with_the_nonce_as_key() {
        let request = Handshake::new(SAMPLE_NONCE).request("relay.example:443", "/v1/ws");
        let lines: Vec<&str> = request.split("\r\n").collect();
        assert_eq!(lines.first(), Some(&"GET /v1/ws HTTP/1.1"));
        for line in [
            "Host: relay.example:443",
            "Upgrade: websocket",
            "Connection: Upgrade",
            "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==",
            "Sec-WebSocket-Version: 13",
        ] {
            assert!(lines.contains(&line), "{line:?} missing from {request:?}");
        }
        assert!(request.ends_with("\r\n\r\n"), "{request:?}");
    }

    #[test]
    fn the_upgrade_is_accepted_and_the_bytes_after_it_are_left() {
        let head = response("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=", "");
        let mut bytes = head.clone().into_bytes();
        bytes.extend_from_slice(&[0x81, 0x00]);
        let mut handshake = Handshake::new(SAMPLE_NONCE);
        assert!(matches!(handshake.read(&bytes), HeadRead::Accepted(n) if n == head.len()));

        let mut handshake = Handshake::new(SAMPLE_NONCE);
        let (first, rest) = bytes.split_at(head.len() - 2);
        assert!(matches!(handshake.read(first), HeadRead::Pending));
        assert!(matches!(handshake.read(rest), HeadRead::Accepted(2)));
    }

    #[test]
    fn header_names_and_tokens_are_read_without_case() {
        let head = "HTTP/1.1 101 Switching Protocols\r\nupgrade: WebSocket\r\nCONNECTION: keep-alive, upgrade\r\nsec-websocket-accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";
        assert_eq!(verdict(head), Ok(head.len()));
    }

    #[test]
    fn a_response_that_is_not_this_upgrade_is_refused() {
        let good = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";
        assert_eq!(verdict("HTTP/1.1 403 Forbidden\r\n\r\n"), Err(HandshakeFault::Refused(403)));
        assert_eq!(verdict("HTTP/1.0 101 Switching Protocols\r\n\r\n"), Err(HandshakeFault::Malformed));
        assert_eq!(verdict("HTTP/1.1 1o1 Switching\r\n\r\n"), Err(HandshakeFault::Malformed));
        assert_eq!(verdict(&response("AAAAAAAAAAAAAAAAAAAAAAAAAAA=", "")), Err(HandshakeFault::WrongAccept));
        assert_eq!(
            verdict(&response(good, &format!("Sec-WebSocket-Accept: {good}\r\n"))),
            Err(HandshakeFault::WrongAccept)
        );
        assert_eq!(
            verdict(&format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {good}\r\n\r\n")),
            Err(HandshakeFault::NotUpgraded)
        );
        assert_eq!(
            verdict(&format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: keep-alive\r\nSec-WebSocket-Accept: {good}\r\n\r\n")),
            Err(HandshakeFault::NotUpgraded)
        );
        assert_eq!(
            verdict(&response(good, "Sec-WebSocket-Extensions: permessage-deflate\r\n")),
            Err(HandshakeFault::Unrequested)
        );
        assert_eq!(verdict(&response(good, "Sec-WebSocket-Protocol: chat\r\n")), Err(HandshakeFault::Unrequested));
        assert_eq!(verdict(&response(good, "no colon here\r\n")), Err(HandshakeFault::Malformed));
    }

    #[test]
    fn a_head_that_never_ends_is_refused_at_the_limit() {
        let mut handshake = Handshake::new(SAMPLE_NONCE);
        let filler = vec![b'a'; HEAD_LIMIT];
        assert!(matches!(handshake.read(&filler), HeadRead::Pending));
        assert!(matches!(handshake.read(b"a"), HeadRead::Failed(HandshakeFault::Oversized)));

        let mut long = response("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=", &format!("X-Pad: {}\r\n", "p".repeat(HEAD_LIMIT)));
        long.push_str("tail");
        assert_eq!(verdict(&long), Err(HandshakeFault::Oversized));
    }
}
