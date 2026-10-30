//! A WebSocket client (RFC 6455), just enough of it for Jellyfin's `/socket`: the opening
//! handshake, text messages in and out, pings, and the closing frame. Byte transport is a
//! [`Conn`], so the protocol is tested against an in-memory peer and run over whatever socket the
//! origin needs ([`live`](super::live) picks it).

use std::io;

/// A byte stream to the server. `read` returns `Ok(0)` when the server closed it and an error of
/// kind `WouldBlock` or `TimedOut` when nothing arrived within the stream's short read timeout, so
/// the caller can look at its cancel flag between reads.
pub trait Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
}

/// The longest message accepted. Jellyfin's messages are a few kilobytes; a library scan's
/// `LibraryChanged` lists ids and stays well under this.
pub const MAX_MESSAGE: usize = 1024 * 1024;

/// The longest handshake answer accepted.
const MAX_HEADER: usize = 16 * 1024;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The stream ended or failed.
    Io,
    /// The server answered the upgrade with this HTTP status instead of 101.
    Status(u16),
    /// The server broke the protocol: a bad handshake, a masked or oversized frame.
    Protocol,
}

/// The upgrade request for `path` on `host`, carrying the process's `User-Agent` and `headers`
/// (each `Name: value`).
pub fn handshake_request(host: &str, path: &str, key: &str, headers: &[String]) -> String {
    let mut r = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\n{}Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n",
        plx_net::net::user_agent_header_line()
    );
    for h in headers {
        r.push_str(h);
        r.push_str("\r\n");
    }
    r.push_str("\r\n");
    r
}

/// The `Sec-WebSocket-Accept` a server must answer `key` with.
pub fn accept_for(key: &str) -> String {
    plx_base::b64::encode(&sha1(format!("{key}{GUID}").as_bytes()))
}

/// Read the handshake answer off `conn` and check it. Bytes that arrived after the header (the
/// server may send its first frame at once) are left in `rest`.
pub fn read_handshake(conn: &mut dyn Conn, key: &str, rest: &mut Vec<u8>, mut waited: impl FnMut() -> bool)
    -> Result<(), Error> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    let end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEADER {
            return Err(Error::Protocol);
        }
        match conn.read(&mut chunk) {
            Ok(0) => return Err(Error::Io),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if timed_out(&e) => {
                if waited() {
                    return Err(Error::Io);
                }
            }
            Err(_) => return Err(Error::Io),
        }
    };
    let head = std::str::from_utf8(&buf[..end]).map_err(|_| Error::Protocol)?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or(Error::Protocol)?;
    if status != 101 {
        return Err(Error::Status(status));
    }
    let accept = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("sec-websocket-accept"))
        .map(|(_, v)| v.trim().to_string());
    if accept.as_deref() != Some(accept_for(key).as_str()) {
        return Err(Error::Protocol);
    }
    rest.extend_from_slice(&buf[end + 4..]);
    Ok(())
}

pub fn timed_out(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// One message or control frame from the server.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Text(String),
    Ping(Vec<u8>),
    Pong,
    Close,
}

/// Opcodes.
const CONT: u8 = 0x0;
const TEXT: u8 = 0x1;
const BINARY: u8 = 0x2;
const CLOSE: u8 = 0x8;
const PING: u8 = 0x9;
const PONG: u8 = 0xA;

/// Reassembles frames from the bytes the server sends; fragmented messages are joined.
#[derive(Default)]
pub struct Reader {
    buf: Vec<u8>,
    /// The fragments of a message still arriving, and whether it is text.
    partial: Option<(Vec<u8>, bool)>,
}

impl Reader {
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next complete frame, `Ok(None)` when more bytes are needed. A binary message is read
    /// and dropped (Jellyfin sends none).
    pub fn next(&mut self) -> Result<Option<Frame>, Error> {
        loop {
            let Some((fin, opcode, payload, used)) = self.header()? else { return Ok(None) };
            self.buf.drain(..used);
            match opcode {
                PING => return Ok(Some(Frame::Ping(payload))),
                PONG => return Ok(Some(Frame::Pong)),
                CLOSE => return Ok(Some(Frame::Close)),
                TEXT | BINARY | CONT => {
                    let (mut data, text) = match (opcode, self.partial.take()) {
                        (CONT, Some(p)) => p,
                        (CONT, None) => return Err(Error::Protocol),
                        (_, Some(_)) => return Err(Error::Protocol),
                        (op, None) => (Vec::new(), op == TEXT),
                    };
                    data.extend_from_slice(&payload);
                    if data.len() > MAX_MESSAGE {
                        return Err(Error::Protocol);
                    }
                    if !fin {
                        self.partial = Some((data, text));
                        continue;
                    }
                    if text {
                        return String::from_utf8(data).map(|s| Some(Frame::Text(s))).map_err(|_| Error::Protocol);
                    }
                }
                _ => return Err(Error::Protocol),
            }
        }
    }

    /// `(fin, opcode, payload, bytes used)` of the frame at the front, once it is all here.
    fn header(&self) -> Result<Option<(bool, u8, Vec<u8>, usize)>, Error> {
        let b = &self.buf;
        if b.len() < 2 {
            return Ok(None);
        }
        let (fin, opcode) = (b[0] & 0x80 != 0, b[0] & 0x0F);
        if b[1] & 0x80 != 0 {
            return Err(Error::Protocol); // a server never masks
        }
        let (len, at) = match b[1] & 0x7F {
            126 if b.len() >= 4 => (u16::from_be_bytes([b[2], b[3]]) as u64, 4),
            127 if b.len() >= 10 => (u64::from_be_bytes(b[2..10].try_into().map_err(|_| Error::Protocol)?), 10),
            126 | 127 => return Ok(None),
            n => (n as u64, 2),
        };
        if len > MAX_MESSAGE as u64 {
            return Err(Error::Protocol);
        }
        let end = at + len as usize;
        if b.len() < end {
            return Ok(None);
        }
        Ok(Some((fin, opcode, b[at..end].to_vec(), end)))
    }
}

/// A client frame: always masked, as the protocol requires of a client.
pub fn frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut f = vec![0x80 | opcode];
    match payload.len() {
        n if n < 126 => f.push(0x80 | n as u8),
        n if n <= u16::MAX as usize => {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    f.extend_from_slice(&mask);
    f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    f
}

pub fn text_frame(text: &str, mask: [u8; 4]) -> Vec<u8> {
    frame(TEXT, text.as_bytes(), mask)
}
pub fn pong_frame(payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    frame(PONG, payload, mask)
}
pub fn close_frame(mask: [u8; 4]) -> Vec<u8> {
    frame(CLOSE, &[], mask)
}

/// Mask and key bytes. They need to be unpredictable to a proxy, not secret, so a clock-seeded
/// xorshift is enough.
pub struct Noise(u64);

impl Noise {
    pub fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64);
        Noise(seed | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn mask(&mut self) -> [u8; 4] {
        (self.next() as u32).to_be_bytes()
    }
    /// A fresh `Sec-WebSocket-Key`: 16 bytes, base64.
    pub fn key(&mut self) -> String {
        let mut k = self.next().to_be_bytes().to_vec();
        k.extend_from_slice(&self.next().to_be_bytes());
        plx_base::b64::encode(&k)
    }
}

/// SHA-1, for the handshake's accept check only.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server's frame (unmasked).
    fn server(opcode: u8, fin: bool, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![if fin { 0x80 } else { 0 } | opcode];
        if payload.len() < 126 {
            f.push(payload.len() as u8);
        } else {
            f.push(126);
            f.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        f.extend_from_slice(payload);
        f
    }

    #[test]
    fn the_accept_key_is_the_rfcs_example() {
        // RFC 6455 §1.3.
        assert_eq!(accept_for("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn frames_are_read_across_any_split_and_fragments_are_joined() {
        let mut bytes = server(TEXT, false, b"{\"Message");
        bytes.extend(server(PING, true, b"hi"));
        bytes.extend(server(CONT, true, b"Type\":\"KeepAlive\"}"));
        bytes.extend(server(TEXT, true, &[b'x'; 300]));
        bytes.extend(server(CLOSE, true, b""));
        let mut r = Reader::default();
        let mut got = Vec::new();
        for b in bytes {
            r.feed(&[b]);
            while let Some(f) = r.next().unwrap() {
                got.push(f);
            }
        }
        assert_eq!(got, [
            Frame::Ping(b"hi".to_vec()),
            Frame::Text("{\"MessageType\":\"KeepAlive\"}".into()),
            Frame::Text("x".repeat(300)),
            Frame::Close,
        ]);
    }

    #[test]
    fn a_masked_or_oversized_server_frame_is_refused() {
        let mut r = Reader::default();
        r.feed(&[0x81, 0x81, 1, 2, 3, 4, 5]);
        assert_eq!(r.next(), Err(Error::Protocol));
        let mut r = Reader::default();
        r.feed(&[0x81, 127]);
        r.feed(&(MAX_MESSAGE as u64 + 1).to_be_bytes());
        assert_eq!(r.next(), Err(Error::Protocol));
    }

    #[test]
    fn client_frames_are_masked() {
        let f = text_frame("KeepAlive", [1, 2, 3, 4]);
        assert_eq!(&f[..2], &[0x81, 0x80 | 9]);
        assert_eq!(&f[2..6], &[1, 2, 3, 4]);
        let unmasked: Vec<u8> = f[6..].iter().enumerate().map(|(i, b)| b ^ [1, 2, 3, 4][i % 4]).collect();
        assert_eq!(unmasked, b"KeepAlive");
        assert_eq!(frame(TEXT, &[0; 200], [0; 4])[1], 0x80 | 126);
    }

    struct Peer {
        incoming: Vec<u8>,
    }
    impl Conn for Peer {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.incoming.len().min(buf.len()).min(7);
            buf[..n].copy_from_slice(&self.incoming[..n]);
            self.incoming.drain(..n);
            Ok(n)
        }
        fn write_all(&mut self, _: &[u8]) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_handshake_checks_status_and_accept_and_keeps_early_bytes() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let ok = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nsec-websocket-accept: {}\r\n\r\nAB", accept_for(key));
        let mut rest = Vec::new();
        assert_eq!(read_handshake(&mut Peer { incoming: ok.into_bytes() }, key, &mut rest, || false), Ok(()));
        assert_eq!(rest, b"AB");
        let denied = "HTTP/1.1 401 Unauthorized\r\n\r\n".to_string();
        assert_eq!(read_handshake(&mut Peer { incoming: denied.into_bytes() }, key, &mut Vec::new(), || false), Err(Error::Status(401)));
        let wrong = "HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Accept: nope\r\n\r\n".to_string();
        assert_eq!(read_handshake(&mut Peer { incoming: wrong.into_bytes() }, key, &mut Vec::new(), || false), Err(Error::Protocol));
        assert!(handshake_request("h:8096", "/socket", key, &["Authorization: x".into()]).ends_with("Authorization: x\r\n\r\n"));
    }
}
