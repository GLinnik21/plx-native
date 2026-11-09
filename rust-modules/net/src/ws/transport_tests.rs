use super::*;
use crate::net::{curl_ready, dead_port, SerialTlsGuard};
use crate::ws::{accept_key, encode, Frame, FrameParser, Opcode, Sender};
use plx_base::task::OffFrame;
use plx_base::testnet::{self, CLIENT_WAIT};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicUsize;
use std::thread::{self, JoinHandle};

const LIMIT: usize = 1 << 20;

struct Peer<S> {
    io: S,
    frames: FrameParser,
}

impl<S: Read + Write> Peer<S> {
    fn upgrade(mut io: S) -> (Peer<S>, String) {
        let head = read_head(&mut io);
        let key = head
            .lines()
            .find_map(|line| line.split_once(':').filter(|(name, _)| name.eq_ignore_ascii_case("sec-websocket-key")))
            .map(|(_, value)| value.trim().to_owned())
            .expect("a Sec-WebSocket-Key");
        let answer = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
            accept_key(&key)
        );
        io.write_all(answer.as_bytes()).unwrap();
        io.flush().unwrap();
        (Peer { io, frames: FrameParser::new(Sender::Client, LIMIT) }, head)
    }

    fn send(&mut self, opcode: Opcode, payload: &[u8]) {
        let mut out = Vec::new();
        encode(opcode, true, payload, None, &mut out);
        self.write(&out);
    }

    fn write(&mut self, bytes: &[u8]) {
        self.io.write_all(bytes).unwrap();
        self.io.flush().unwrap();
    }

    fn next(&mut self) -> Option<Frame> {
        loop {
            if let Some(frame) = self.frames.next_frame() {
                return Some(frame.expect("a valid client frame"));
            }
            let mut buf = [0u8; 4096];
            match self.io.read(&mut buf) {
                Ok(0) | Err(_) => return None,
                Ok(n) => self.frames.push(&buf[..n]),
            }
        }
    }

    fn until_eof(&mut self) -> bool {
        let mut buf = [0u8; 256];
        loop {
            match self.io.read(&mut buf) {
                Ok(0) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
    }
}

fn read_head(io: &mut impl Read) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        assert_eq!(io.read(&mut byte).unwrap(), 1, "the client hung up inside its request head");
        head.push(byte[0]);
    }
    String::from_utf8(head).unwrap()
}

fn accept(listener: &TcpListener) -> TcpStream {
    let (stream, _) = testnet::accept_client(listener, CLIENT_WAIT);
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    stream
}

fn serve(script: impl FnOnce(TcpStream) + Send + 'static) -> (u16, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    (port, thread::spawn(move || script(accept(&listener))))
}

fn serve_ws(script: impl FnOnce(Peer<TcpStream>, String) + Send + 'static) -> (String, JoinHandle<()>) {
    let (port, server) = serve(|stream| {
        let (peer, head) = Peer::upgrade(stream);
        script(peer, head);
    });
    (format!("ws://127.0.0.1:{port}/v1/ws"), server)
}

struct Pem(std::path::PathBuf);

impl Drop for Pem {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn certificate(alpn: &[&[u8]]) -> (Arc<rustls::ServerConfig>, Pem) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).unwrap();
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.der().clone()], rustls::pki_types::PrivateKeyDer::from(signing_key))
        .unwrap();
    config.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
    let path = std::env::temp_dir().join(format!(
        "plxnative-ws-{}-{:?}.pem",
        std::process::id(),
        thread::current().id()
    ));
    std::fs::write(&path, cert.pem()).unwrap();
    (Arc::new(config), Pem(path))
}

type TlsStream = rustls::StreamOwned<rustls::ServerConnection, TcpStream>;

fn serve_wss(script: impl FnOnce(Peer<TlsStream>) + Send + 'static) -> (String, Pem, JoinHandle<()>) {
    serve_wss_offering(&[], script)
}

fn serve_wss_offering(alpn: &[&[u8]], script: impl FnOnce(Peer<TlsStream>) + Send + 'static) -> (String, Pem, JoinHandle<()>) {
    let (config, pem) = certificate(alpn);
    let (port, server) = serve(move |stream| {
        let tls = rustls::StreamOwned::new(rustls::ServerConnection::new(config).unwrap(), stream);
        script(Peer::upgrade(tls).0);
    });
    (format!("wss://127.0.0.1:{port}/v1/ws"), pem, server)
}

fn plain(url: &str) -> Dial<'_> {
    Dial { url, trust: Tls::Ca, limit: LIMIT, idle: Duration::from_secs(5) }
}

fn trusting<'a>(url: &'a str, pem: &'a Pem) -> Dial<'a> {
    Dial { url, trust: Tls::CaBundle(pem.0.to_str().unwrap()), limit: LIMIT, idle: Duration::from_secs(5) }
}

fn go(dial: Dial<'_>, inbox: Inbox, on: &mut dyn FnMut(Event)) -> End {
    run(&OffFrame::for_test(), dial, &AtomicBool::new(false), inbox, on)
}

fn close_code(frame: &Frame) -> u16 {
    u16::from_be_bytes([frame.payload[0], frame.payload[1]])
}

#[test]
fn a_text_message_goes_out_and_its_echo_comes_back_before_a_clean_close() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let (url, server) = serve_ws(|mut peer, head| {
        assert!(head.starts_with("GET /v1/ws HTTP/1.1\r\n"), "{head}");
        assert!(head.contains("\r\nHost: 127.0.0.1:"), "{head}");
        let text = peer.next().unwrap();
        assert_eq!((text.opcode, text.payload.as_slice()), (Opcode::Text, &b"hello"[..]));
        peer.send(Opcode::Text, &text.payload);
        let close = peer.next().unwrap();
        assert_eq!((close.opcode, close_code(&close)), (Opcode::Close, 1000));
        peer.send(Opcode::Close, &close.payload[..2]);
    });
    let (outbox, inbox) = channel().unwrap();
    let mut texts = Vec::new();
    let end = go(plain(&url), inbox, &mut |event| match event {
        Event::Opened => assert!(outbox.text("hello".to_owned())),
        Event::Text(text) => {
            texts.push(text);
            assert!(outbox.close(1000, String::new()));
        }
        other => panic!("{other:?}"),
    });
    server.join().unwrap();
    assert_eq!(texts, ["hello"]);
    assert_eq!(end, End::Closed { code: Some(1000), reason: String::new() });
    assert!(!outbox.text("late".to_owned()));
}

#[test]
fn a_message_queued_before_the_upgrade_is_sent_once_the_connection_opens() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let (url, server) = serve_ws(|mut peer, _| {
        let binary = peer.next().unwrap();
        assert_eq!((binary.opcode, binary.payload.as_slice()), (Opcode::Binary, &[0u8, 255, 7][..]));
        peer.send(Opcode::Close, &1000u16.to_be_bytes());
        assert_eq!(close_code(&peer.next().unwrap()), 1000);
    });
    let (outbox, inbox) = channel().unwrap();
    assert!(outbox.binary(vec![0, 255, 7]));
    let end = go(plain(&url), inbox, &mut |_| {});
    server.join().unwrap();
    assert_eq!(end, End::Closed { code: Some(1000), reason: String::new() });
}

#[test]
fn a_server_ping_is_answered_with_its_payload() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let (url, server) = serve_ws(|mut peer, _| {
        peer.send(Opcode::Ping, b"are you there");
        let pong = peer.next().unwrap();
        assert_eq!((pong.opcode, pong.payload.as_slice()), (Opcode::Pong, &b"are you there"[..]));
        peer.send(Opcode::Close, &[&1000u16.to_be_bytes()[..], b"done"].concat());
        assert_eq!(close_code(&peer.next().unwrap()), 1000);
    });
    let (_outbox, inbox) = channel().unwrap();
    let end = go(plain(&url), inbox, &mut |_| {});
    server.join().unwrap();
    assert_eq!(end, End::Closed { code: Some(1000), reason: "done".to_owned() });
}

#[test]
fn after_the_server_closes_the_client_waits_for_its_tcp_close_and_no_longer() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let (url, server) = serve_ws(|mut peer, _| {
        peer.send(Opcode::Close, &1001u16.to_be_bytes());
        assert_eq!(close_code(&peer.next().unwrap()), 1001);
    });
    let (_outbox, inbox) = channel().unwrap();
    let started = Instant::now();
    let end = go(plain(&url), inbox, &mut |_| {});
    server.join().unwrap();
    assert_eq!(end, End::Closed { code: Some(1001), reason: String::new() });
    assert!(started.elapsed() < LINGER, "{:?}", started.elapsed());
}

#[test]
fn every_frame_of_a_tls_burst_arrives_without_waiting_on_the_socket() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let big = "a".repeat(40_000);
    let sent = big.clone();
    let (url, pem, server) = serve_wss(move |mut peer| {
        let mut burst = Vec::new();
        for text in [sent.as_str(), "b", "c"] {
            encode(Opcode::Text, true, text.as_bytes(), None, &mut burst);
        }
        peer.write(&burst);
        let close = peer.next().unwrap();
        assert_eq!(close_code(&close), 1000);
        peer.send(Opcode::Close, &close.payload);
    });
    let (outbox, inbox) = channel().unwrap();
    let mut texts = Vec::new();
    let mut opened_at = None;
    let end = go(trusting(&url, &pem), inbox, &mut |event| match event {
        Event::Opened => opened_at = Some(Instant::now()),
        Event::Text(text) => {
            texts.push(text);
            if texts.len() == 3 {
                assert!(outbox.close(1000, String::new()));
            }
        }
        other => panic!("{other:?}"),
    });
    let took = opened_at.unwrap().elapsed();
    server.join().unwrap();
    assert_eq!(texts, [big, "b".to_owned(), "c".to_owned()]);
    assert_eq!(end, End::Closed { code: Some(1000), reason: String::new() });
    assert!(took < Duration::from_secs(2), "{took:?}");
}

#[test]
fn a_certificate_outside_the_bundle_is_refused() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let (url, _served, server) = serve_wss(|_| {});
    let (_, other) = certificate(&[]);
    let (_outbox, inbox) = channel().unwrap();
    let end = go(trusting(&url, &other), inbox, &mut |event| panic!("{event:?}"));
    let _ = server.join();
    assert!(matches!(end, End::Refused(Refusal::Open(OpenError::Connect { .. }))), "{end:?}");
}

#[test]
fn nothing_is_dialled_where_only_serial_tls_is_safe() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let _guard = SerialTlsGuard::force();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    for scheme in ["wss", "ws"] {
        let (_outbox, inbox) = channel().unwrap();
        let url = format!("{scheme}://127.0.0.1:{}/v1/ws", listener.local_addr().unwrap().port());
        let end = go(plain(&url), inbox, &mut |event| panic!("{event:?}"));
        assert_eq!(end, End::Refused(Refusal::Open(OpenError::SerialTls)), "{scheme}");
        assert!(listener.accept().is_err(), "the client dialled over {scheme}");
    }
}

#[test]
fn cancelling_sends_going_away_and_returns() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let (url, server) = serve_ws(|mut peer, _| {
        let close = peer.next().unwrap();
        assert_eq!((close.opcode, close_code(&close)), (Opcode::Close, 1001));
    });
    let cancel = AtomicBool::new(false);
    let (_outbox, inbox) = channel().unwrap();
    let started = Instant::now();
    let end = run(&OffFrame::for_test(), plain(&url), &cancel, inbox, &mut |event| {
        assert_eq!(event, Event::Opened);
        cancel.store(true, Ordering::Release);
    });
    server.join().unwrap();
    assert_eq!(end, End::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
}

#[test]
fn a_silent_server_ends_the_connection_as_idle() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let (url, server) = serve_ws(|mut peer, _| {
        peer.until_eof();
    });
    let (_outbox, inbox) = channel().unwrap();
    let dial = Dial { idle: Duration::from_millis(300), ..plain(&url) };
    let started = Instant::now();
    let end = go(dial, inbox, &mut |_| {});
    server.join().unwrap();
    assert_eq!(end, End::Idle);
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
}

#[test]
fn a_dropped_connection_without_a_close_is_lost() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let (url, server) = serve_ws(|peer, _| drop(peer));
    let (_outbox, inbox) = channel().unwrap();
    let end = go(plain(&url), inbox, &mut |_| {});
    server.join().unwrap();
    assert_eq!(end, End::Lost);
}

#[test]
fn an_answer_that_is_not_an_upgrade_fails_the_handshake() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let (port, server) = serve(|mut stream| {
        read_head(&mut stream);
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").unwrap();
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
    });
    let url = format!("ws://127.0.0.1:{port}/v1/ws");
    let (_outbox, inbox) = channel().unwrap();
    let started = Instant::now();
    let end = go(plain(&url), inbox, &mut |event| panic!("{event:?}"));
    server.join().unwrap();
    assert!(matches!(end, End::Failed(Fault::Handshake(_))), "{end:?}");
    assert!(started.elapsed() < LINGER, "{:?}", started.elapsed());
}

#[test]
fn a_refused_connection_is_reported_with_curls_code() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let url = format!("ws://127.0.0.1:{}/v1/ws", dead_port());
    let (_outbox, inbox) = channel().unwrap();
    let end = go(plain(&url), inbox, &mut |event| panic!("{event:?}"));
    assert!(matches!(end, End::Refused(Refusal::Open(OpenError::Connect { rc: 7, .. }))), "{end:?}");
}

#[test]
fn an_address_that_is_not_a_websocket_url_is_refused_before_dialling() {
    for url in ["http://127.0.0.1/v1/ws", "ws://127.0.0.1/v1/ws#part", "ws://user@127.0.0.1/", "ws:///v1/ws", "wss://"] {
        let (_outbox, inbox) = channel().unwrap();
        assert_eq!(go(plain(url), inbox, &mut |event| panic!("{event:?}")), End::Refused(Refusal::Url), "{url}");
    }
}

#[test]
fn an_address_keeps_its_query_and_gains_a_root_path() {
    assert_eq!(split("WSS://relay.example?room=1"), Some((true, "relay.example", "/?room=1".to_owned())));
    assert_eq!(split("ws://[::1]:9001"), Some((false, "[::1]:9001", "/".to_owned())));
    assert_eq!(split("ws://relay.example/v1/ws?x=1"), Some((false, "relay.example", "/v1/ws?x=1".to_owned())));
}

#[test]
fn a_peer_that_never_pauses_cannot_hold_off_a_cancel() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let (url, server) = serve_ws(|mut peer, _| {
        let mut burst = Vec::new();
        while burst.len() < 1 << 16 {
            encode(Opcode::Text, true, "x".repeat(1000).as_bytes(), None, &mut burst);
        }
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until && peer.io.write_all(&burst).is_ok() {}
    });
    let cancel = AtomicBool::new(false);
    let (_outbox, inbox) = channel().unwrap();
    let mut first = None;
    let end = run(&OffFrame::for_test(), plain(&url), &cancel, inbox, &mut |event| {
        if matches!(event, Event::Text(_)) {
            thread::sleep(Duration::from_micros(200));
            if first.is_none() {
                first = Some(Instant::now());
                cancel.store(true, Ordering::Release);
            }
        }
    });
    let took = first.unwrap().elapsed();
    server.join().unwrap();
    assert_eq!(end, End::Cancelled);
    assert!(took < Duration::from_secs(1), "{took:?}");
}

#[test]
fn a_server_that_offers_h2_still_gets_the_http_1_1_upgrade() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let (url, pem, server) = serve_wss_offering(&[b"h2", b"http/1.1"], |mut peer| {
        peer.send(Opcode::Close, &1000u16.to_be_bytes());
        assert_eq!(close_code(&peer.next().unwrap()), 1000);
    });
    let (_outbox, inbox) = channel().unwrap();
    let mut opened = false;
    let end = go(trusting(&url, &pem), inbox, &mut |event| opened |= event == Event::Opened);
    let _ = server.join();
    assert!(opened, "{end:?}");
    assert_eq!(end, End::Closed { code: Some(1000), reason: String::new() });
}

#[test]
fn a_cancel_during_the_tls_handshake_ends_the_dial() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let (_, pem) = certificate(&[]);
    let (port, server) = serve(|mut stream| {
        let mut sink = Vec::new();
        let _ = stream.read_to_end(&mut sink);
    });
    let url = format!("wss://127.0.0.1:{port}/v1/ws");
    let cancel = AtomicBool::new(false);
    let (_outbox, inbox) = channel().unwrap();
    let started = Instant::now();
    let end = thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(300));
            cancel.store(true, Ordering::Release);
        });
        run(&OffFrame::for_test(), trusting(&url, &pem), &cancel, inbox, &mut |event| panic!("{event:?}"))
    });
    let took = started.elapsed();
    server.join().unwrap();
    assert_eq!(end, End::Cancelled);
    assert!(took < Duration::from_millis(1800), "{took:?}");
}

#[test]
fn a_cancel_during_the_close_linger_returns_at_once() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() {
        return;
    }
    let (url, server) = serve_ws(|mut peer, _| {
        peer.send(Opcode::Close, &1000u16.to_be_bytes());
        assert_eq!(close_code(&peer.next().unwrap()), 1000);
        peer.until_eof();
    });
    let cancel = AtomicBool::new(false);
    let (_outbox, inbox) = channel().unwrap();
    let started = Instant::now();
    let end = thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(300));
            cancel.store(true, Ordering::Release);
        });
        run(&OffFrame::for_test(), plain(&url), &cancel, inbox, &mut |_| {})
    });
    let took = started.elapsed();
    server.join().unwrap();
    assert_eq!(end, End::Closed { code: Some(1000), reason: String::new() });
    assert!(took < Duration::from_secs(1), "{took:?}");
}

#[test]
fn an_outbox_takes_nothing_after_its_close() {
    let (outbox, _inbox) = channel().unwrap();
    assert!(outbox.text("before".to_owned()));
    assert!(outbox.close(1000, String::new()));
    assert!(!outbox.text("after".to_owned()));
    assert!(!outbox.close(1000, String::new()));
}

#[test]
fn an_outbox_refuses_a_close_the_protocol_does_not_allow() {
    let (outbox, _inbox) = channel().unwrap();
    assert!(!outbox.close(1005, String::new()));
    assert!(!outbox.close(1000, "x".repeat(124)));
    assert!(outbox.close(1000, "x".repeat(123)));
}

#[test]
fn the_wake_socket_never_reads_as_closed_while_the_inbox_lives() {
    let (outbox, inbox) = channel().unwrap();
    drop(outbox);
    let error = (&inbox.wake).read(&mut [0u8; 1]).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
}

#[test]
fn an_outbox_whose_connection_is_gone_says_so() {
    let (outbox, inbox) = channel().unwrap();
    drop(inbox);
    assert!(!outbox.text("late".to_owned()));
}

#[test]
#[ignore = "needs the Autobahn fuzzing server, AUTOBAHN=ws://127.0.0.1:9001"]
fn autobahn_client_cases() {
    let _serial = plx_base::testlock::serial();
    assert!(curl_ready(), "libcurl did not load");
    let base = std::env::var("AUTOBAHN").expect("AUTOBAHN=ws://host:port");
    let agent = "plxnative";
    let cases = AtomicUsize::new(0);
    let (_outbox, inbox) = channel().unwrap();
    let end = go(plain(&format!("{base}/getCaseCount")), inbox, &mut |event| {
        if let Event::Text(text) = event {
            cases.store(text.trim().parse().expect("a case count"), Ordering::Release);
        }
    });
    eprintln!("getCaseCount: {} ({end:?})", cases.load(Ordering::Acquire));
    assert!(cases.load(Ordering::Acquire) > 0, "the fuzzing server gave no cases: {end:?}");
    for case in 1..=cases.load(Ordering::Acquire) {
        let url = format!("{base}/runCase?case={case}&agent={agent}");
        let (outbox, inbox) = channel().unwrap();
        let dial = Dial { url: &url, trust: Tls::Ca, limit: 64 << 20, idle: Duration::from_secs(60) };
        let end = go(dial, inbox, &mut |event| match event {
            Event::Text(text) => {
                outbox.text(text);
            }
            Event::Binary(data) => {
                outbox.binary(data);
            }
            _ => {}
        });
        eprintln!("case {case}: {end:?}");
    }
    let (_outbox, inbox) = channel().unwrap();
    let end = go(plain(&format!("{base}/updateReports?agent={agent}")), inbox, &mut |_| {});
    eprintln!("updateReports: {end:?}");
}
