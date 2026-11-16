use super::frame::{encode, Frame, FrameError, FrameParser, Opcode, Sender, CONTROL_PAYLOAD};
use super::handshake::{Handshake, HandshakeFault, HeadRead};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Opened,
    Text(String),
    Binary(Vec<u8>),
    Closed { code: Option<u16>, reason: String },
    Failed(Fault),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    Handshake(HandshakeFault),
    Frame(FrameError),
    Fragmentation,
    InvalidText,
    InvalidClose,
    TooBig,
}

impl Fault {
    pub fn close_code(self) -> Option<u16> {
        match self {
            Fault::Handshake(_) => None,
            Fault::Frame(error) => Some(error.close_code()),
            Fault::Fragmentation | Fault::InvalidClose => Some(1002),
            Fault::InvalidText => Some(1007),
            Fault::TooBig => Some(1009),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Connecting,
    Open,
    Closing,
    Closed,
}

struct Partial {
    text: bool,
    data: Vec<u8>,
    valid: usize,
}

pub struct Connection<M> {
    state: State,
    handshake: Handshake,
    frames: FrameParser,
    partial: Option<Partial>,
    limit: usize,
    outgoing: Vec<u8>,
    masks: M,
}

impl<M: FnMut() -> [u8; 4]> Connection<M> {
    pub fn open(host: &str, path: &str, nonce: [u8; 16], limit: usize, masks: M) -> Option<Connection<M>> {
        if !http_authority(host) || !origin_form(path) {
            return None;
        }
        let handshake = Handshake::new(nonce);
        let outgoing = handshake.request(host, path).into_bytes();
        Some(Connection {
            state: State::Connecting,
            handshake,
            frames: FrameParser::new(Sender::Server, limit),
            partial: None,
            limit,
            outgoing,
            masks,
        })
    }

    pub fn receive(&mut self, bytes: &[u8], out: &mut dyn FnMut(Event)) {
        let mut bytes = bytes;
        if self.state == State::Connecting {
            match self.handshake.read(bytes) {
                HeadRead::Pending => return,
                HeadRead::Failed(fault) => return self.fail(Fault::Handshake(fault), out),
                HeadRead::Accepted(used) => {
                    self.state = State::Open;
                    out(Event::Opened);
                    bytes = &bytes[used..];
                }
            }
        }
        if self.state == State::Closed {
            return;
        }
        self.frames.push(bytes);
        while self.state != State::Closed {
            match self.frames.next_frame() {
                Some(Ok(frame)) => self.on_frame(frame, out),
                Some(Err(error)) => self.fail(Fault::Frame(error), out),
                None => break,
            }
        }
    }

    pub fn send_text(&mut self, text: &str) -> bool {
        self.send_data(Opcode::Text, text.as_bytes())
    }

    pub fn send_binary(&mut self, data: &[u8]) -> bool {
        self.send_data(Opcode::Binary, data)
    }

    pub fn close(&mut self, code: u16, reason: &str) -> bool {
        if !close_code_allowed(code) || 2 + reason.len() > CONTROL_PAYLOAD {
            return false;
        }
        match self.state {
            State::Open => {
                self.send(Opcode::Close, &[&code.to_be_bytes()[..], reason.as_bytes()].concat());
                self.state = State::Closing;
                true
            }
            State::Connecting => {
                self.outgoing.clear();
                self.state = State::Closed;
                true
            }
            State::Closing | State::Closed => false,
        }
    }

    pub fn take_outgoing(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.outgoing)
    }

    pub fn is_closed(&self) -> bool {
        self.state == State::Closed
    }

    fn send_data(&mut self, opcode: Opcode, payload: &[u8]) -> bool {
        if self.state != State::Open {
            return false;
        }
        self.send(opcode, payload);
        true
    }

    fn send(&mut self, opcode: Opcode, payload: &[u8]) {
        let mask = (self.masks)();
        encode(opcode, true, payload, Some(mask), &mut self.outgoing);
    }

    fn on_frame(&mut self, frame: Frame, out: &mut dyn FnMut(Event)) {
        match frame.opcode {
            Opcode::Ping => self.send(Opcode::Pong, &frame.payload),
            Opcode::Pong => {}
            Opcode::Close => self.on_close(&frame.payload, out),
            Opcode::Text | Opcode::Binary => {
                if self.partial.is_some() {
                    return self.fail(Fault::Fragmentation, out);
                }
                let partial = Partial { text: frame.opcode == Opcode::Text, data: frame.payload, valid: 0 };
                self.extend(partial, frame.fin, out);
            }
            Opcode::Continuation => {
                let Some(mut partial) = self.partial.take() else {
                    return self.fail(Fault::Fragmentation, out);
                };
                if frame.payload.len() > self.limit.saturating_sub(partial.data.len()) {
                    return self.fail(Fault::TooBig, out);
                }
                partial.data.extend_from_slice(&frame.payload);
                self.extend(partial, frame.fin, out);
            }
        }
    }

    fn extend(&mut self, mut partial: Partial, fin: bool, out: &mut dyn FnMut(Event)) {
        if fin {
            let event = if partial.text {
                match String::from_utf8(partial.data) {
                    Ok(text) => Event::Text(text),
                    Err(_) => return self.fail(Fault::InvalidText, out),
                }
            } else {
                Event::Binary(partial.data)
            };
            return out(event);
        }
        if partial.text {
            match std::str::from_utf8(&partial.data[partial.valid..]) {
                Ok(_) => partial.valid = partial.data.len(),
                Err(error) if error.error_len().is_some() => return self.fail(Fault::InvalidText, out),
                Err(error) => partial.valid += error.valid_up_to(),
            }
        }
        self.partial = Some(partial);
    }

    fn on_close(&mut self, payload: &[u8], out: &mut dyn FnMut(Event)) {
        let (code, reason) = match payload {
            [] => (None, String::new()),
            [hi, lo, rest @ ..] if close_code_allowed(u16::from_be_bytes([*hi, *lo])) => {
                match std::str::from_utf8(rest) {
                    Ok(reason) => (Some(u16::from_be_bytes([*hi, *lo])), reason.to_owned()),
                    Err(_) => return self.fail(Fault::InvalidText, out),
                }
            }
            _ => return self.fail(Fault::InvalidClose, out),
        };
        if self.state == State::Open {
            self.send(Opcode::Close, code.map(u16::to_be_bytes).as_ref().map_or(&[][..], |bytes| &bytes[..]));
        }
        self.finish(Event::Closed { code, reason }, out);
    }

    fn fail(&mut self, fault: Fault, out: &mut dyn FnMut(Event)) {
        if let (State::Open, Some(code)) = (self.state, fault.close_code()) {
            self.send(Opcode::Close, &code.to_be_bytes());
        }
        self.finish(Event::Failed(fault), out);
    }

    fn finish(&mut self, event: Event, out: &mut dyn FnMut(Event)) {
        self.state = State::Closed;
        self.partial = None;
        out(event);
    }
}

pub(super) fn close_code_allowed(code: u16) -> bool {
    matches!(code, 1000..=1003 | 1007..=1014 | 3000..=4999)
}

fn http_authority(text: &str) -> bool {
    let (host_valid, port) = match text.strip_prefix('[') {
        Some(bracketed) => match bracketed.split_once(']') {
            Some((literal, rest)) => (literal.parse::<std::net::Ipv6Addr>().is_ok(), rest),
            None => return false,
        },
        None => {
            let end = text.find(':').unwrap_or(text.len());
            (!text[..end].is_empty() && uri_chars(&text[..end], unreserved_or_sub_delim), &text[end..])
        }
    };
    host_valid && (port.is_empty() || port.strip_prefix(':').is_some_and(|digits| digits.bytes().all(|b| b.is_ascii_digit())))
}

fn origin_form(target: &str) -> bool {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    path.starts_with('/')
        && uri_chars(path, |b| path_char(b) || b == b'/')
        && uri_chars(query, |b| path_char(b) || b == b'/' || b == b'?')
}

fn unreserved_or_sub_delim(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=".contains(&byte)
}

fn path_char(byte: u8) -> bool {
    unreserved_or_sub_delim(byte) || byte == b':' || byte == b'@'
}

fn uri_chars(text: &str, allowed: impl Fn(u8) -> bool) -> bool {
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        let valid = if byte == b'%' {
            bytes.next().is_some_and(|b| b.is_ascii_hexdigit()) && bytes.next().is_some_and(|b| b.is_ascii_hexdigit())
        } else {
            allowed(byte)
        };
        if !valid {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::handshake::accept_key;
    use super::*;

    const NONCE: [u8; 16] = *b"the sample nonce";
    const MASK: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];
    const PONG_MASKED: [u8; 11] = [0x8a, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58];

    struct Rig {
        conn: Connection<fn() -> [u8; 4]>,
        events: Vec<Event>,
    }

    impl Rig {
        fn new(limit: usize) -> Rig {
            let conn = Connection::open("relay.example", "/v1/ws", NONCE, limit, (|| MASK) as fn() -> [u8; 4])
                .expect("a valid request target");
            Rig { conn, events: Vec::new() }
        }

        fn opened(limit: usize) -> Rig {
            let mut rig = Rig::new(limit);
            rig.conn.take_outgoing();
            rig.feed(&upgrade());
            assert_eq!(rig.take_events(), [Event::Opened]);
            rig
        }

        fn feed(&mut self, bytes: &[u8]) {
            let events = &mut self.events;
            self.conn.receive(bytes, &mut |event| events.push(event));
        }

        fn take_events(&mut self) -> Vec<Event> {
            std::mem::take(&mut self.events)
        }

        fn sent(&mut self) -> Vec<Frame> {
            let mut parser = FrameParser::new(Sender::Client, 1 << 20);
            parser.push(&self.conn.take_outgoing());
            std::iter::from_fn(|| parser.next_frame()).map(|frame| frame.expect("a frame the client wrote")).collect()
        }
    }

    fn upgrade() -> Vec<u8> {
        format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
            accept_key("dGhlIHNhbXBsZSBub25jZQ==")
        )
        .into_bytes()
    }

    fn server(opcode: Opcode, fin: bool, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        encode(opcode, fin, payload, None, &mut out);
        out
    }

    fn close_frame(code: u16, reason: &str) -> Vec<u8> {
        server(Opcode::Close, true, &[&code.to_be_bytes()[..], reason.as_bytes()].concat())
    }

    fn control(opcode: Opcode, payload: &[u8]) -> Frame {
        Frame { fin: true, opcode, payload: payload.to_vec() }
    }

    #[test]
    fn the_upgrade_request_is_written_first_and_frames_after_it_are_read() {
        let mut rig = Rig::new(1024);
        let request = String::from_utf8(rig.conn.take_outgoing()).expect("ASCII request");
        assert!(request.starts_with("GET /v1/ws HTTP/1.1\r\nHost: relay.example\r\n"), "{request:?}");
        assert!(request.contains("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"), "{request:?}");

        let mut bytes = upgrade();
        bytes.extend_from_slice(&server(Opcode::Text, true, b"{\"type\":\"joined\"}"));
        for byte in bytes.chunks(1) {
            rig.feed(byte);
        }
        assert_eq!(rig.take_events(), [Event::Opened, Event::Text("{\"type\":\"joined\"}".into())]);
    }

    #[test]
    fn a_target_that_could_break_the_request_is_refused() {
        let masks = || [0u8; 4];
        assert!(Connection::open("relay.example", "/v1/ws", NONCE, 1024, masks).is_some());
        assert!(Connection::open("", "/v1/ws", NONCE, 1024, masks).is_none());
        assert!(Connection::open("relay.example\r\nX: y", "/v1/ws", NONCE, 1024, masks).is_none());
        assert!(Connection::open("relay.example", "v1/ws", NONCE, 1024, masks).is_none());
        assert!(Connection::open("relay.example", "/v1/ws HTTP/1.0", NONCE, 1024, masks).is_none());
    }

    #[test]
    fn the_host_is_an_http_authority_and_the_path_has_no_fragment() {
        let opens = |host: &str, path: &str| Connection::open(host, path, NONCE, 1024, || [0u8; 4]).is_some();
        for (host, path) in [
            ("relay.example", "/v1/ws?room=AB12CD&v=1"),
            ("relay.example:443", "/v1/ws"),
            ("127.0.0.1", "/"),
            ("[::1]:8080", "/v1/ws"),
            ("relay.example", "/a%20b/c:d@e"),
        ] {
            assert!(opens(host, path), "{host} {path}");
        }
        for (host, path) in [
            ("relay.example", "/v1/ws#fragment"),
            ("relay.example", "/v1/ws?room=a#b"),
            ("relay.example/path", "/v1/ws"),
            ("user@relay.example", "/v1/ws"),
            ("relay.example:44a", "/v1/ws"),
            ("[::1", "/v1/ws"),
            ("[:]", "/v1/ws"),
            ("::1", "/v1/ws"),
            ("relay.example", "/v1/ws%zz"),
        ] {
            assert!(!opens(host, path), "{host} {path}");
        }
    }

    #[test]
    fn a_refused_upgrade_fails_without_a_close_frame() {
        let mut rig = Rig::new(1024);
        rig.conn.take_outgoing();
        rig.feed(b"HTTP/1.1 429 Too Many Requests\r\n\r\n");
        assert_eq!(rig.take_events(), [Event::Failed(Fault::Handshake(HandshakeFault::Refused(429)))]);
        assert!(rig.conn.take_outgoing().is_empty());
        assert!(rig.conn.is_closed());
    }

    #[test]
    fn a_ping_is_answered_with_the_rfc_6455_section_5_7_pong() {
        let mut rig = Rig::opened(1024);
        rig.feed(&server(Opcode::Ping, true, b"Hello"));
        assert!(rig.take_events().is_empty());
        assert_eq!(rig.conn.take_outgoing(), PONG_MASKED);
    }

    #[test]
    fn a_fragmented_text_is_joined_around_a_ping() {
        let mut rig = Rig::opened(1024);
        let euro = "€".as_bytes();
        rig.feed(&server(Opcode::Text, false, &[b"price ", &euro[..1]].concat()));
        rig.feed(&server(Opcode::Ping, true, b"p"));
        rig.feed(&server(Opcode::Continuation, false, &euro[1..]));
        rig.feed(&server(Opcode::Continuation, true, b"5"));
        assert_eq!(rig.take_events(), [Event::Text("price €5".into())]);
        assert_eq!(rig.sent(), [control(Opcode::Pong, b"p")]);
    }

    #[test]
    fn a_binary_message_is_delivered_whole() {
        let mut rig = Rig::opened(1024);
        rig.feed(&server(Opcode::Binary, false, &[1, 2]));
        rig.feed(&server(Opcode::Continuation, true, &[3]));
        assert_eq!(rig.take_events(), [Event::Binary(vec![1, 2, 3])]);
    }

    #[test]
    fn the_server_s_close_is_echoed_and_ends_the_connection() {
        let mut rig = Rig::opened(1024);
        let mut bytes = close_frame(1001, "going away");
        bytes.extend_from_slice(&server(Opcode::Text, true, b"late"));
        rig.feed(&bytes);
        assert_eq!(rig.take_events(), [Event::Closed { code: Some(1001), reason: "going away".into() }]);
        assert_eq!(rig.sent(), [control(Opcode::Close, &1001u16.to_be_bytes())]);
        assert!(rig.conn.is_closed());
        rig.feed(&server(Opcode::Ping, true, b"x"));
        assert!(rig.take_events().is_empty());
        assert!(rig.sent().is_empty());
        assert!(!rig.conn.send_text("after"));
    }

    #[test]
    fn an_empty_close_is_echoed_empty() {
        let mut rig = Rig::opened(1024);
        rig.feed(&server(Opcode::Close, true, b""));
        assert_eq!(rig.take_events(), [Event::Closed { code: None, reason: String::new() }]);
        assert_eq!(rig.sent(), [control(Opcode::Close, b"")]);
    }

    #[test]
    fn our_close_waits_for_the_server_s_and_answers_only_its_pings() {
        let mut rig = Rig::opened(1024);
        assert!(rig.conn.close(1000, "bye"));
        assert_eq!(rig.sent(), [control(Opcode::Close, &[&1000u16.to_be_bytes()[..], b"bye"].concat())]);
        assert!(!rig.conn.is_closed());
        assert!(!rig.conn.send_text("after"));
        assert!(!rig.conn.close(1000, ""));
        rig.feed(&server(Opcode::Ping, true, b"x"));
        rig.feed(&server(Opcode::Text, true, b"still here"));
        rig.feed(&close_frame(1000, ""));
        assert_eq!(
            rig.take_events(),
            [Event::Text("still here".into()), Event::Closed { code: Some(1000), reason: String::new() }]
        );
        assert_eq!(rig.sent(), [control(Opcode::Pong, b"x")]);
        assert!(rig.conn.is_closed());
    }

    #[test]
    fn a_close_with_a_code_that_may_not_be_sent_is_refused() {
        let mut rig = Rig::opened(1024);
        for code in [999, 1004, 1005, 1006, 1015, 2999, 5000] {
            assert!(!rig.conn.close(code, ""), "{code}");
        }
        assert!(!rig.conn.close(1000, &"r".repeat(124)));
        assert!(rig.conn.close(1000, &"r".repeat(123)));
    }

    #[test]
    fn closing_before_the_request_is_taken_withdraws_it() {
        let mut rig = Rig::new(1024);
        assert!(rig.conn.close(1000, ""));
        assert!(rig.conn.is_closed());
        assert!(rig.conn.take_outgoing().is_empty());
    }

    #[test]
    fn sending_waits_for_the_upgrade() {
        let mut rig = Rig::new(1024);
        rig.conn.take_outgoing();
        assert!(!rig.conn.send_text("early"));
        rig.feed(&upgrade());
        assert!(rig.conn.send_text("{\"type\":\"ping\"}"));
        assert_eq!(rig.sent(), [Frame { fin: true, opcode: Opcode::Text, payload: b"{\"type\":\"ping\"}".to_vec() }]);
    }

    fn failure(bytes: &[u8], limit: usize) -> (Vec<Event>, Vec<Frame>) {
        let mut rig = Rig::opened(limit);
        rig.feed(bytes);
        (rig.take_events(), rig.sent())
    }

    fn closes_with(code: u16) -> Vec<Frame> {
        vec![control(Opcode::Close, &code.to_be_bytes())]
    }

    #[test]
    fn a_protocol_violation_fails_the_connection_with_1002() {
        let cases: [(Vec<u8>, Fault); 7] = [
            (vec![0x83, 0x00], Fault::Frame(FrameError::UnknownOpcode)),
            (vec![0x81, 0x7e, 0x00, 0x01, 0x61], Fault::Frame(FrameError::NonMinimalLength)),
            (server(Opcode::Continuation, true, b"x"), Fault::Fragmentation),
            (
                [server(Opcode::Text, false, b"a"), server(Opcode::Binary, true, b"b")].concat(),
                Fault::Fragmentation,
            ),
            (server(Opcode::Close, true, &[0x03]), Fault::InvalidClose),
            (close_frame(1005, ""), Fault::InvalidClose),
            (close_frame(999, ""), Fault::InvalidClose),
        ];
        for (bytes, fault) in cases {
            assert_eq!(failure(&bytes, 1024), (vec![Event::Failed(fault)], closes_with(1002)), "{bytes:02x?}");
        }
    }

    #[test]
    fn invalid_text_fails_the_connection_with_1007() {
        assert_eq!(
            failure(&server(Opcode::Text, true, &[0xce, 0xba, 0xff]), 1024),
            (vec![Event::Failed(Fault::InvalidText)], closes_with(1007))
        );
        assert_eq!(
            failure(&server(Opcode::Text, false, &[0xed, 0xa0, 0x80]), 1024),
            (vec![Event::Failed(Fault::InvalidText)], closes_with(1007)),
            "a fragment fails as soon as its bytes cannot start valid text"
        );
        assert_eq!(
            failure(&[server(Opcode::Text, false, &[0xe2, 0x82]), server(Opcode::Continuation, true, b"")].concat(), 1024),
            (vec![Event::Failed(Fault::InvalidText)], closes_with(1007)),
            "a message may not end inside a character"
        );
        assert_eq!(
            failure(&server(Opcode::Close, true, &[0x03, 0xe8, 0xff]), 1024),
            (vec![Event::Failed(Fault::InvalidText)], closes_with(1007))
        );
    }

    #[test]
    fn a_message_past_the_limit_fails_the_connection_with_1009() {
        assert_eq!(
            failure(&server(Opcode::Text, true, &[b'a'; 11]), 10),
            (vec![Event::Failed(Fault::Frame(FrameError::TooBig))], closes_with(1009))
        );
        assert_eq!(
            failure(&[server(Opcode::Text, false, &[b'a'; 6]), server(Opcode::Continuation, true, &[b'a'; 5])].concat(), 10),
            (vec![Event::Failed(Fault::TooBig)], closes_with(1009))
        );
    }

    #[test]
    fn the_close_codes_a_server_may_send_are_accepted() {
        for code in [1000, 1001, 1002, 1003, 1007, 1008, 1009, 1010, 1011, 1012, 1013, 1014, 3000, 3999, 4000, 4999] {
            let mut rig = Rig::opened(1024);
            rig.feed(&close_frame(code, ""));
            assert_eq!(rig.take_events(), [Event::Closed { code: Some(code), reason: String::new() }], "{code}");
        }
    }
}
