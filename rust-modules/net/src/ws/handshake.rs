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
        let mut lines = head.split(|&b| b == b'\n').map(|line| line.strip_suffix(b"\r").unwrap_or(line));
        let code = lines
            .next()
            .and_then(|status| status.strip_prefix(b"HTTP/1.1 "))
            .filter(|rest| matches!(rest.get(3), None | Some(b' ')))
            .and_then(|rest| rest.get(..3))
            .filter(|digits| digits.iter().all(u8::is_ascii_digit))
            .ok_or(HandshakeFault::Malformed)?;
        let code = code.iter().fold(0, |n, digit| n * 10 + u16::from(digit - b'0'));
        if code != 101 {
            return Err(HandshakeFault::Refused(code));
        }
        let (mut upgrade, mut connection, mut accepts) = (false, false, Vec::new());
        for line in lines.filter(|line| !line.is_empty()) {
            let colon = line.iter().position(|&b| b == b':').ok_or(HandshakeFault::Malformed)?;
            let (name, value) = (&line[..colon], &line[colon + 1..]);
            if name.is_empty() || !name.iter().all(|&b| is_token_byte(b)) || value.iter().any(|&b| b == b'\r' || b == 0) {
                return Err(HandshakeFault::Malformed);
            }
            let value = trim_whitespace(value);
            if name.eq_ignore_ascii_case(b"upgrade") {
                upgrade |= value.eq_ignore_ascii_case(b"websocket");
            } else if name.eq_ignore_ascii_case(b"connection") {
                connection |= value
                    .split(|&b| b == b',')
                    .any(|token| trim_whitespace(token).eq_ignore_ascii_case(b"upgrade"));
            } else if name.eq_ignore_ascii_case(b"sec-websocket-accept") {
                accepts.push(value);
            } else if name.eq_ignore_ascii_case(b"sec-websocket-extensions")
                || name.eq_ignore_ascii_case(b"sec-websocket-protocol")
            {
                return Err(HandshakeFault::Unrequested);
            }
        }
        if !upgrade || !connection {
            return Err(HandshakeFault::NotUpgraded);
        }
        match accepts.as_slice() {
            [accept] if *accept == accept_key(&self.key).as_bytes() => Ok(()),
            _ => Err(HandshakeFault::WrongAccept),
        }
    }
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn trim_whitespace(bytes: &[u8]) -> &[u8] {
    let blank = |b: &u8| matches!(b, b' ' | b'\t');
    let start = bytes.iter().position(|b| !blank(b)).unwrap_or(bytes.len());
    let end = bytes.iter().rposition(|b| !blank(b)).map_or(start, |last| last + 1);
    &bytes[start..end]
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

    fn verdict(head: impl AsRef<[u8]>) -> Result<usize, HandshakeFault> {
        let head = head.as_ref();
        match Handshake::new(SAMPLE_NONCE).read(head) {
            HeadRead::Accepted(used) => Ok(used),
            HeadRead::Failed(fault) => Err(fault),
            HeadRead::Pending => panic!("the head did not end: {:?}", String::from_utf8_lossy(head)),
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
        assert_eq!(verdict(&response(good, "Sec-WebSocket-Extensions:\r\n")), Err(HandshakeFault::Unrequested));
        assert_eq!(verdict(&response(good, "Sec-WebSocket-Protocol: \r\n")), Err(HandshakeFault::Unrequested));
        assert_eq!(verdict(&response(good, "no colon here\r\n")), Err(HandshakeFault::Malformed));
    }

    #[test]
    fn a_field_value_is_opaque_bytes_and_a_field_name_is_a_token() {
        let good = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";
        let latin = [
            format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {good}\r\n").as_bytes(),
            b"X-Label: caf\xe9\r\n\r\n",
        ]
        .concat();
        assert_eq!(verdict(&latin), Ok(latin.len()));
        for field in ["Not A Field: y\r\n", " Folded: y\r\n", ": no name\r\n", "Bare\rReturn: y\r\n", "X-Value: a\rb\r\n"] {
            assert_eq!(verdict(response(good, field)), Err(HandshakeFault::Malformed), "{field:?}");
        }
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
