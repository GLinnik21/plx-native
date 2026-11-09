#![no_main]
use libfuzzer_sys::fuzz_target;
use plx_net::ws::{accept_key, Connection, Event, FrameParser, Opcode, Sender};
use plxnative_fuzz as _;

const LIMIT: usize = 1024;
const NONCE: [u8; 16] = [7; 16];

fn upgrade() -> Vec<u8> {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(&plx_base::b64::encode(&NONCE))
    )
    .into_bytes()
}

fn run(stream: &[u8], stride: usize) -> (Vec<Event>, Vec<u8>) {
    let mut counter = 0u32;
    let masks = move || {
        counter = counter.wrapping_add(1);
        counter.to_be_bytes()
    };
    let mut conn = Connection::open("relay.test", "/v1/ws", NONCE, LIMIT, masks).expect("a valid request target");
    assert!(conn.take_outgoing().starts_with(b"GET /v1/ws HTTP/1.1\r\n"));
    let mut events = Vec::new();
    for piece in stream.chunks(stride) {
        conn.receive(piece, &mut |event| events.push(event));
    }
    (events, conn.take_outgoing())
}

fn ends(event: &Event) -> bool {
    matches!(event, Event::Closed { .. } | Event::Failed(_))
}

fuzz_target!(|data: &[u8]| {
    let Some((&control, input)) = data.split_first() else { return };
    let upgraded = control & 1 == 0;
    let stream = if upgraded { [upgrade().as_slice(), input].concat() } else { input.to_vec() };
    let whole = run(&stream, stream.len().max(1));
    assert_eq!(whole, run(&stream, usize::from(control >> 1) + 1), "the answer depends on where the bytes were split");
    let (events, sent) = whole;

    if upgraded {
        assert_eq!(events.first(), Some(&Event::Opened), "{events:?}");
    }
    assert!(events.iter().skip(1).all(|event| *event != Event::Opened), "{events:?}");
    assert!(events.iter().rev().skip(1).all(|event| !ends(event)), "an event after the end: {events:?}");
    for event in &events {
        match event {
            Event::Text(text) => assert!(text.len() <= LIMIT, "{} bytes", text.len()),
            Event::Binary(bytes) => assert!(bytes.len() <= LIMIT, "{} bytes", bytes.len()),
            _ => {}
        }
    }

    let mut parser = FrameParser::new(Sender::Client, LIMIT);
    parser.push(&sent);
    let frames: Vec<_> = std::iter::from_fn(|| parser.next_frame())
        .map(|frame| frame.expect("the client wrote a frame its own parser refuses"))
        .collect();
    assert!(frames.iter().all(|f| f.fin && matches!(f.opcode, Opcode::Pong | Opcode::Close)), "{frames:?}");
    let closes = frames.iter().filter(|f| f.opcode == Opcode::Close).count();
    let closed_open = match events.last() {
        Some(Event::Closed { .. }) => true,
        Some(Event::Failed(fault)) => fault.close_code().is_some(),
        _ => false,
    };
    assert_eq!(closes, usize::from(closed_open), "{events:?} {frames:?}");
    assert!(closes == 0 || frames.last().is_some_and(|f| f.opcode == Opcode::Close), "a frame after the close: {frames:?}");
});
