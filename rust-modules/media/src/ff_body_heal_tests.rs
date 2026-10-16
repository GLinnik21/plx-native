//! A direct-play body that the server stops sending before its length.
//!
//! Field report (FinNative on webOS 6.5.3, 4K direct play from Jellyfin): seven minutes into a
//! film the transfer stopped at 75.9 MB with the connection still open, the queues ran dry and
//! the picture froze; Play did nothing and a seek (a fresh ranged request) recovered it. The
//! plaintext socket source never reopened a body that ended early or went silent. These serve a
//! body that stops short, then answer the ranged reopen, and read through `read_cb`.

use super::*;
use std::io::{Read, Write};

/// A loopback server. The first connection gets `first` (a head promising 8 bytes and fewer) and
/// is then closed, or held open without a byte more when `hold` is set; every later connection
/// gets the 206 answer for its `Range`. Records each request head.
struct ShortBody {
    port: u16,
    heads: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    stop: std::sync::Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

const BODY: &[u8] = b"ABCDEFGH";

impl ShortBody {
    fn start(first: &'static [u8], hold: bool) -> ShortBody {
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().unwrap().port();
        srv.set_nonblocking(true).expect("nonblocking listener");
        let heads = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let (heads_t, stop_t) = (heads.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let mut held = Vec::new();
            while !stop_t.load(Ordering::Acquire) {
                let mut s = match plx_base::testnet::accept(&srv) {
                    Ok((s, _)) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => return,
                };
                let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(300)));
                let mut raw = Vec::new();
                let mut chunk = [0u8; 2048];
                while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => raw.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&raw).into_owned();
                let first_request = heads_t.lock().unwrap().is_empty();
                heads_t.lock().unwrap().push(head.clone());
                if first_request {
                    let _ = s.write_all(first);
                    let _ = s.flush();
                    if hold {
                        held.push(s);
                    }
                    continue;
                }
                let from = head
                    .split("Range: bytes=")
                    .nth(1)
                    .and_then(|r| r.split('-').next())
                    .and_then(|n| n.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                let rest = &BODY[from.min(BODY.len())..];
                let mut reply = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {from}-{}/{}\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    BODY.len() - 1,
                    BODY.len(),
                    rest.len()
                )
                .into_bytes();
                reply.extend_from_slice(rest);
                let _ = s.write_all(&reply);
                let _ = s.flush();
            }
        });
        ShortBody { port, heads, stop, thread: Some(thread) }
    }
}

impl Drop for ShortBody {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Open the body at byte 0 through `seek_cb` (the reopen path) and read it through `read_cb`
/// until EOF or an error. Returns the bytes read.
fn read_all(port: u16) -> Vec<u8> {
    let mut hs = plx_net::stream::http_stream_boxed();
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut state = AvioState {
        src: Src::Socket {
            hs: &mut *hs,
            host: CString::new("127.0.0.1").unwrap(),
            port: port as c_int,
            path: CString::new("/Videos/a/stream.mkv?static=true").unwrap(),
        },
        aq: &mut *aq,
        off: 0,
        size: BODY.len() as i64,
        io_failed: false,
        body_active_us: 0,
        body_bytes: 0,
        first_byte_at: None,
        reserve_deadline: ReserveDeadlineState::new(None, false),
        transport_watchdog: None,
        acquisition: None,
        bounce: Vec::new(),
        bounce_pos: 0,
        parked_at_end: false,
    };
    let op = &mut state as *mut AvioState as *mut c_void;
    let mut got = Vec::new();
    if seek_cb(op, 0, SEEK_SET) == 0 {
        let mut buf = [0u8; 16];
        loop {
            let r = read_cb(op, buf.as_mut_ptr(), buf.len() as c_int);
            if r <= 0 {
                break;
            }
            got.extend_from_slice(&buf[..r as usize]);
        }
    }
    plx_net::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
    got
}

#[test]
fn a_body_closed_before_its_length_is_reopened_at_the_byte_reached() {
    let _serial = plx_base::testlock::serial();
    let srv = ShortBody::start(
        b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-7/8\r\nContent-Length: 8\r\n\r\nABCD",
        false,
    );
    assert_eq!(read_all(srv.port), BODY, "the rest of the body must come from a ranged reopen");
    let heads = srv.heads.lock().unwrap().clone();
    assert!(heads.len() >= 2 && heads[1].contains("Range: bytes=4-"), "{heads:?}");
}

#[test]
fn a_body_that_goes_silent_is_reopened_at_the_byte_reached() {
    let _serial = plx_base::testlock::serial();
    let srv = ShortBody::start(
        b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-7/8\r\nContent-Length: 8\r\n\r\nABCD",
        true,
    );
    assert_eq!(read_all(srv.port), BODY, "a silent connection must give way to a ranged reopen");
    let heads = srv.heads.lock().unwrap().clone();
    assert!(heads.len() >= 2 && heads[1].contains("Range: bytes=4-"), "{heads:?}");
}
