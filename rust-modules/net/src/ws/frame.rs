pub(super) const CONTROL_PAYLOAD: usize = 125;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opcode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

impl Opcode {
    fn from_bits(bits: u8) -> Option<Opcode> {
        Some(match bits {
            0x0 => Opcode::Continuation,
            0x1 => Opcode::Text,
            0x2 => Opcode::Binary,
            0x8 => Opcode::Close,
            0x9 => Opcode::Ping,
            0xa => Opcode::Pong,
            _ => return None,
        })
    }

    fn bits(self) -> u8 {
        match self {
            Opcode::Continuation => 0x0,
            Opcode::Text => 0x1,
            Opcode::Binary => 0x2,
            Opcode::Close => 0x8,
            Opcode::Ping => 0x9,
            Opcode::Pong => 0xa,
        }
    }

    pub fn is_control(self) -> bool {
        matches!(self, Opcode::Close | Opcode::Ping | Opcode::Pong)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sender {
    Client,
    Server,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub opcode: Opcode,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    ReservedBits,
    UnknownOpcode,
    WrongMasking,
    FragmentedControl,
    LongControl,
    NonMinimalLength,
    LengthOverflow,
    TooBig,
}

impl FrameError {
    pub fn close_code(self) -> u16 {
        match self {
            FrameError::TooBig => 1009,
            _ => 1002,
        }
    }
}

pub fn encode(opcode: Opcode, fin: bool, payload: &[u8], mask: Option<[u8; 4]>, out: &mut Vec<u8>) {
    out.push((u8::from(fin) << 7) | opcode.bits());
    let masked = if mask.is_some() { 0x80 } else { 0 };
    match payload.len() {
        n @ 0..=125 => out.push(masked | n as u8),
        n @ 126..=0xffff => {
            out.push(masked | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(masked | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    match mask {
        Some(key) => {
            out.extend_from_slice(&key);
            out.extend(payload.iter().zip(key.iter().cycle()).map(|(b, k)| b ^ k));
        }
        None => out.extend_from_slice(payload),
    }
}

pub struct FrameParser {
    from: Sender,
    limit: usize,
    buf: Vec<u8>,
    pos: usize,
    failed: bool,
}

impl FrameParser {
    pub fn new(from: Sender, limit: usize) -> FrameParser {
        FrameParser { from, limit, buf: Vec::new(), pos: 0, failed: false }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if self.failed {
            return;
        }
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    pub fn next_frame(&mut self) -> Option<Result<Frame, FrameError>> {
        if self.failed {
            return None;
        }
        match self.parse() {
            Ok(Some((frame, used))) => {
                self.pos += used;
                Some(Ok(frame))
            }
            Ok(None) => None,
            Err(error) => {
                self.failed = true;
                self.buf = Vec::new();
                self.pos = 0;
                Some(Err(error))
            }
        }
    }

    fn parse(&self) -> Result<Option<(Frame, usize)>, FrameError> {
        let bytes = &self.buf[self.pos..];
        let (Some(&first), Some(&second)) = (bytes.first(), bytes.get(1)) else {
            return Ok(None);
        };
        if first & 0x70 != 0 {
            return Err(FrameError::ReservedBits);
        }
        let opcode = Opcode::from_bits(first & 0x0f).ok_or(FrameError::UnknownOpcode)?;
        let fin = first & 0x80 != 0;
        let masked = second & 0x80 != 0;
        if masked != (self.from == Sender::Client) {
            return Err(FrameError::WrongMasking);
        }
        let short = usize::from(second & 0x7f);
        if opcode.is_control() {
            if !fin {
                return Err(FrameError::FragmentedControl);
            }
            if short > CONTROL_PAYLOAD {
                return Err(FrameError::LongControl);
            }
        }
        let (len, length_end): (usize, usize) = match short {
            126 => match bytes.get(2..4) {
                Some(&[hi, lo]) => match usize::from(u16::from_be_bytes([hi, lo])) {
                    0..=125 => return Err(FrameError::NonMinimalLength),
                    n => (n, 4),
                },
                _ => return Ok(None),
            },
            127 => match bytes.get(2..10) {
                Some(wide) => {
                    let n = u64::from_be_bytes(wide.try_into().expect("eight length bytes"));
                    if n >> 63 != 0 {
                        return Err(FrameError::LengthOverflow);
                    }
                    if n <= 0xffff {
                        return Err(FrameError::NonMinimalLength);
                    }
                    (usize::try_from(n).unwrap_or(usize::MAX), 10)
                }
                None => return Ok(None),
            },
            n => (n, 2),
        };
        let head = if masked { length_end + 4 } else { length_end };
        let end = head
            .checked_add(len)
            .filter(|end| len <= self.limit && *end <= isize::MAX as usize)
            .ok_or(FrameError::TooBig)?;
        let mask = if masked {
            let Some(&[a, b, c, d]) = bytes.get(length_end..head) else {
                return Ok(None);
            };
            Some([a, b, c, d])
        } else {
            None
        };
        let Some(body) = bytes.get(head..end) else {
            return Ok(None);
        };
        let mut payload = body.to_vec();
        if let Some(key) = mask {
            for (byte, k) in payload.iter_mut().zip(key.iter().cycle()) {
                *byte ^= k;
            }
        }
        Ok(Some((Frame { fin, opcode, payload }, end)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASK: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];
    const HELLO: [u8; 7] = [0x81, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f];
    const HELLO_MASKED: [u8; 11] = [0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58];
    const HEL: [u8; 5] = [0x01, 0x03, 0x48, 0x65, 0x6c];
    const LO: [u8; 4] = [0x80, 0x02, 0x6c, 0x6f];
    const PING: [u8; 7] = [0x89, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f];
    const PONG_MASKED: [u8; 11] = [0x8a, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58];

    fn frame(fin: bool, opcode: Opcode, payload: &[u8]) -> Frame {
        Frame { fin, opcode, payload: payload.to_vec() }
    }

    fn all(from: Sender, limit: usize, chunks: &[&[u8]]) -> Vec<Result<Frame, FrameError>> {
        let mut parser = FrameParser::new(from, limit);
        let mut out = Vec::new();
        for chunk in chunks {
            parser.push(chunk);
            while let Some(item) = parser.next_frame() {
                out.push(item);
            }
        }
        out
    }

    fn binary(len: usize) -> Vec<u8> {
        (0..len).map(|i| i as u8).collect()
    }

    #[test]
    fn the_rfc_6455_section_5_7_examples_parse() {
        assert_eq!(all(Sender::Server, 1 << 20, &[&HELLO]), [Ok(frame(true, Opcode::Text, b"Hello"))]);
        assert_eq!(all(Sender::Client, 1 << 20, &[&HELLO_MASKED]), [Ok(frame(true, Opcode::Text, b"Hello"))]);
        assert_eq!(
            all(Sender::Server, 1 << 20, &[&HEL, &LO]),
            [Ok(frame(false, Opcode::Text, b"Hel")), Ok(frame(true, Opcode::Continuation, b"lo"))]
        );
        assert_eq!(all(Sender::Server, 1 << 20, &[&PING]), [Ok(frame(true, Opcode::Ping, b"Hello"))]);
        assert_eq!(all(Sender::Client, 1 << 20, &[&PONG_MASKED]), [Ok(frame(true, Opcode::Pong, b"Hello"))]);

        let mut short = vec![0x82, 0x7e, 0x01, 0x00];
        short.extend_from_slice(&binary(256));
        assert_eq!(all(Sender::Server, 1 << 20, &[&short]), [Ok(frame(true, Opcode::Binary, &binary(256)))]);
        let mut long = vec![0x82, 0x7f, 0, 0, 0, 0, 0, 0x01, 0x00, 0x00];
        long.extend_from_slice(&binary(65536));
        assert_eq!(all(Sender::Server, 1 << 20, &[&long]), [Ok(frame(true, Opcode::Binary, &binary(65536)))]);
    }

    #[test]
    fn encoding_reproduces_the_rfc_6455_section_5_7_examples() {
        let mut out = Vec::new();
        encode(Opcode::Text, true, b"Hello", Some(MASK), &mut out);
        assert_eq!(out, HELLO_MASKED);
        out.clear();
        encode(Opcode::Pong, true, b"Hello", Some(MASK), &mut out);
        assert_eq!(out, PONG_MASKED);
        out.clear();
        encode(Opcode::Text, false, b"Hel", None, &mut out);
        encode(Opcode::Continuation, true, b"lo", None, &mut out);
        assert_eq!(out, [&HEL[..], &LO[..]].concat());
        out.clear();
        encode(Opcode::Binary, true, &binary(256), None, &mut out);
        assert_eq!(out.get(0..4), Some(&[0x82, 0x7e, 0x01, 0x00][..]));
        assert_eq!(out.len(), 4 + 256);
        out.clear();
        encode(Opcode::Binary, true, &binary(65536), None, &mut out);
        assert_eq!(out.get(0..10), Some(&[0x82, 0x7f, 0, 0, 0, 0, 0, 0x01, 0x00, 0x00][..]));
        assert_eq!(out.len(), 10 + 65536);
    }

    #[test]
    fn bytes_split_anywhere_give_the_same_frames() {
        let stream = [&HELLO[..], &HEL, &PING, &LO].concat();
        let whole = all(Sender::Server, 1 << 20, &[&stream]);
        let bytes: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(all(Sender::Server, 1 << 20, &bytes), whole);
        assert_eq!(whole.len(), 4);
    }

    #[test]
    fn a_broken_header_is_refused_before_its_payload_arrives() {
        let cases: [(&[u8], FrameError); 8] = [
            (&[0xc1, 0x05], FrameError::ReservedBits),
            (&[0x83, 0x05], FrameError::UnknownOpcode),
            (&[0x8b, 0x05], FrameError::UnknownOpcode),
            (&[0x81, 0x85], FrameError::WrongMasking),
            (&[0x09, 0x05], FrameError::FragmentedControl),
            (&[0x89, 0x7e], FrameError::LongControl),
            (&[0x82, 0x7f, 0x80, 0, 0, 0, 0, 0, 0, 0], FrameError::LengthOverflow),
            (&[0x82, 0x7e, 0x04, 0x01], FrameError::TooBig),
        ];
        for (bytes, error) in cases {
            assert_eq!(all(Sender::Server, 1024, &[bytes]), [Err(error)], "{bytes:02x?}");
        }
        assert_eq!(all(Sender::Client, 1024, &[&HELLO]), [Err(FrameError::WrongMasking)]);
    }

    #[test]
    fn a_length_written_wider_than_it_needs_is_refused() {
        let cases: [&[u8]; 4] = [
            &[0x81, 0x7e, 0x00, 0x01, 0x61],
            &[0x81, 0x7f, 0, 0, 0, 0, 0, 0, 0x00, 0x01, 0x61],
            &[0x82, 0x7e, 0x00, 0x7d],
            &[0x82, 0x7f, 0, 0, 0, 0, 0, 0, 0xff, 0xff],
        ];
        for bytes in cases {
            assert_eq!(all(Sender::Server, 1 << 20, &[bytes]), [Err(FrameError::NonMinimalLength)], "{bytes:02x?}");
        }
    }

    #[test]
    fn each_length_width_starts_where_the_narrower_one_ends() {
        for (len, second) in [(125, 0x7d), (126, 0x7e), (65535, 0x7e), (65536, 0x7f)] {
            let mut bytes = Vec::new();
            encode(Opcode::Binary, true, &binary(len), None, &mut bytes);
            assert_eq!(bytes[1], second, "{len}");
            assert_eq!(all(Sender::Server, 1 << 20, &[&bytes]), [Ok(frame(true, Opcode::Binary, &binary(len)))], "{len}");
        }
    }

    #[test]
    fn a_length_past_the_limit_is_refused_whatever_its_width() {
        assert_eq!(all(Sender::Server, 125, &[&[0x82, 0x7e, 0x00, 0x7e]]), [Err(FrameError::TooBig)]);
        assert_eq!(
            all(Sender::Server, 1 << 20, &[&[0x82, 0x7f, 0, 0, 0, 0x01, 0, 0, 0, 0]]),
            [Err(FrameError::TooBig)]
        );
    }

    #[test]
    fn a_length_no_buffer_can_hold_is_refused_whatever_the_limit() {
        assert_eq!(
            all(Sender::Server, usize::MAX, &[&[0x82, 0x7f, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]]),
            [Err(FrameError::TooBig)]
        );
    }

    #[test]
    fn nothing_is_read_after_a_refusal() {
        let mut parser = FrameParser::new(Sender::Server, 1024);
        parser.push(&[0x83, 0x00]);
        assert_eq!(parser.next_frame(), Some(Err(FrameError::UnknownOpcode)));
        parser.push(&HELLO);
        assert_eq!(parser.next_frame(), None);
    }
}
