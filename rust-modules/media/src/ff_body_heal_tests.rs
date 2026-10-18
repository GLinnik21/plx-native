//! A direct-play body that the server stops sending before its length.
//!
//! Field report (FinNative on webOS 6.5.3, 4K direct play from Jellyfin): seven minutes into a
//! film the transfer stopped at 75.9 MB with the connection still open, the queues ran dry and
//! the picture froze; Play did nothing and a seek (a fresh ranged request) recovered it. The
//! plaintext socket source never reopened a body that ended early or went silent. These serve a
//! body that stops short, then answer the ranged reopen, and read through `read_cb`.
//!
//! A second report (an 18.4 GB direct play from Jellyfin on exFAT): the body ended at the same byte
//! every time and every reopen there answered with nothing. Nine reopens later the demuxer took the
//! cut for the end of the file, pushed EOS and the player sat at "Playing (stalled)" for good.

use super::*;
use std::io::{Read, Write};

/// A loopback server. The first connection gets `first` (a head promising 8 bytes and fewer) and
/// is then closed, or held open without a byte more when `hold` is set; every later connection
/// gets the 206 answer for its `Range`, or a 206 with an empty body when `stuck` is set. Records
/// each request head.
struct ShortBody {
    port: u16,
    /// Lets a held first connection continue: it then gets [`REST`] and is closed.
    release: std::sync::Arc<AtomicBool>,
    heads: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    stop: std::sync::Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

const BODY: &[u8] = b"ABCDEFGH";

/// What a held connection sends once released.
const REST: &[u8] = b"EFGH";

/// A body with no length, as a progressive transcode (`start.mkv`) answers.
const UNSIZED_HEAD: &[u8] = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nABCD";

/// How the server answers a reopen (every request after the first).
#[derive(Clone, Copy, PartialEq)]
enum Reopen {
    /// A 206 for the `Range`, from the byte asked for.
    Honest,
    /// A 206 with an empty body.
    Stuck,
    /// A 200 with the whole body: the Range was ignored (or a transcode restarted).
    Full200,
    /// A 206 whose `Content-Range` starts at byte 0, whatever was asked.
    WrongStart,
}

const SHORT_HEAD: &[u8] =
    b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-7/8\r\nContent-Length: 8\r\n\r\nABCD";

impl ShortBody {
    fn start(first: &'static [u8], hold: bool, stuck: bool) -> ShortBody {
        ShortBody::start_with(first, hold, if stuck { Reopen::Stuck } else { Reopen::Honest })
    }

    fn start_with(first: &'static [u8], hold: bool, reopen: Reopen) -> ShortBody {
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().unwrap().port();
        srv.set_nonblocking(true).expect("nonblocking listener");
        let heads = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let release = std::sync::Arc::new(AtomicBool::new(false));
        let (heads_t, stop_t, release_t) = (heads.clone(), stop.clone(), release.clone());
        let thread = std::thread::spawn(move || {
            let mut held: Vec<std::net::TcpStream> = Vec::new();
            while !stop_t.load(Ordering::Acquire) {
                if release_t.load(Ordering::Acquire) {
                    for mut h in held.drain(..) {
                        let _ = h.write_all(REST);
                    }
                }
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
                let rest = match reopen {
                    Reopen::Stuck => &[][..],
                    Reopen::Full200 | Reopen::WrongStart => BODY,
                    Reopen::Honest => &BODY[from.min(BODY.len())..],
                };
                let (status, range_from) = match reopen {
                    Reopen::Full200 => ("200 OK", None),
                    Reopen::WrongStart => ("206 Partial Content", Some(0)),
                    _ => ("206 Partial Content", Some(from)),
                };
                let content_range = range_from.map_or_else(String::new, |f| {
                    format!("Content-Range: bytes {f}-{}/{}\r\n", BODY.len() - 1, BODY.len())
                });
                let mut reply = format!(
                    "HTTP/1.1 {status}\r\n{content_range}Content-Length: {}\r\n\
                     Connection: close\r\n\r\n",
                    rest.len()
                )
                .into_bytes();
                reply.extend_from_slice(rest);
                let _ = s.write_all(&reply);
                let _ = s.flush();
            }
        });
        ShortBody { port, release, heads, stop, thread: Some(thread) }
    }

    fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
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

/// Open the body at byte 0 through `seek_cb` (the reopen path) and hand the AVIO to `f`.
fn with_source<R>(port: u16, f: impl FnOnce(*mut c_void) -> R) -> R {
    with_sized_source(port, BODY.len() as i64, f)
}

/// [`with_source`] for a body of `size` bytes (-1: no length, as a transcode).
fn with_sized_source<R>(port: u16, size: i64, f: impl FnOnce(*mut c_void) -> R) -> R {
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
        size,
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
        heal: HealBudget::default(),
    };
    let op = &mut state as *mut AvioState as *mut c_void;
    assert_eq!(seek_cb(op, 0, SEEK_SET), 0, "the first open");
    let out = f(op);
    plx_net::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
    out
}

/// Read through `read_cb` until EOF or an error: the bytes read and the terminal answer.
fn read_to_end(op: *mut c_void) -> (Vec<u8>, c_int) {
    let mut got = Vec::new();
    let mut buf = [0u8; 16];
    loop {
        let r = read_cb(op, buf.as_mut_ptr(), buf.len() as c_int);
        if r <= 0 {
            return (got, r);
        }
        got.extend_from_slice(&buf[..r as usize]);
    }
}

#[test]
fn a_body_closed_before_its_length_is_reopened_at_the_byte_reached() {
    let _serial = plx_base::testlock::serial();
    let srv = ShortBody::start(SHORT_HEAD, false, false);
    let got = with_source(srv.port, |op| read_to_end(op).0);
    assert_eq!(got, BODY, "the rest of the body must come from a ranged reopen");
    let heads = srv.heads();
    assert!(heads.len() >= 2 && heads[1].contains("Range: bytes=4-"), "{heads:?}");
}

#[test]
fn a_body_that_goes_silent_is_reopened_at_the_byte_reached() {
    let _serial = plx_base::testlock::serial();
    let srv = ShortBody::start(SHORT_HEAD, true, false);
    let got = with_source(srv.port, |op| read_to_end(op).0);
    assert_eq!(got, BODY, "a silent connection must give way to a ranged reopen");
    let heads = srv.heads();
    assert!(heads.len() >= 2 && heads[1].contains("Range: bytes=4-"), "{heads:?}");
}

#[test]
fn a_cut_no_reopen_crosses_is_a_transport_failure_not_the_end_of_the_file() {
    let _serial = plx_base::testlock::serial();
    let srv = ShortBody::start(SHORT_HEAD, false, true);
    with_source(srv.port, |op| {
        let (got, end) = read_to_end(op);
        assert_eq!(got, b"ABCD");
        assert_eq!(end, AVERROR_IO, "a body that stops below its length did not end");
        assert!(unsafe { (*(op as *mut AvioState)).io_failed }, "the failure is published");
        // libavformat answers the error by seeking back to the byte and reading again. The budget
        // spans those reads: the seek's own reopen, and no fresh round of heals behind it.
        assert_eq!(seek_cb(op, 4, SEEK_SET), 4);
        let reopened = srv.heads().len();
        let mut buf = [0u8; 16];
        assert_eq!(read_cb(op, buf.as_mut_ptr(), buf.len() as c_int), AVERROR_IO);
        assert_eq!(srv.heads().len(), reopened, "{:?}", srv.heads());
    });
}

/// A progressive transcode (`start.mkv`) is read through this same AVIO with no length. It cannot
/// be reopened at a byte (the PMS restarts the session and answers 200 from the head), so a cut is
/// not healed: it ends the way it did before bodies were healed at all, for the player to answer.
#[test]
fn an_unsized_body_that_is_cut_is_not_reopened() {
    let _serial = plx_base::testlock::serial();
    let srv = ShortBody::start(UNSIZED_HEAD, false, false);
    let (got, end) = with_sized_source(srv.port, -1, read_to_end);
    assert_eq!(got, b"ABCD");
    assert_eq!(end, AVERROR_EOF, "the close of a body with no length is its end");
    assert_eq!(srv.heads().len(), 1, "no reopen: {:?}", srv.heads());
}

#[test]
fn an_unsized_body_is_not_given_the_silence_deadline() {
    let _serial = plx_base::testlock::serial();
    let srv = ShortBody::start(UNSIZED_HEAD, true, false);
    let release = srv.release.clone();
    // Well past the silence a sized body would be reopened after.
    let quiet = BODY_SILENCE * 3;
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(quiet);
        release.store(true, Ordering::Release);
    });
    let began = std::time::Instant::now();
    let (got, end) = with_sized_source(srv.port, -1, read_to_end);
    let _ = releaser.join();
    assert!(began.elapsed() >= quiet, "the read returned before the server spoke again");
    assert_eq!((got.as_slice(), end), (&b"ABCDEFGH"[..], AVERROR_EOF));
    assert_eq!(srv.heads().len(), 1, "a transcode that is slow is not a cut: {:?}", srv.heads());
}

/// The reopen of a sized body must come back from the byte asked for. A server that ignores the
/// Range answers 200 from byte 0; splicing that in as the bytes at the cut is corruption that
/// looks like success.
fn assert_reopen_refused(reopen: Reopen) {
    let srv = ShortBody::start_with(SHORT_HEAD, false, reopen);
    with_source(srv.port, |op| {
        let (got, end) = read_to_end(op);
        assert_eq!(got, b"ABCD", "nothing from the wrong offset may be delivered");
        assert_eq!(end, AVERROR_IO);
        assert!(unsafe { (*(op as *mut AvioState)).io_failed }, "the failure is published");
    });
    assert!(srv.heads().len() >= 2, "the reopen was tried: {:?}", srv.heads());
}

#[test]
fn a_reopen_answered_with_the_whole_body_is_refused() {
    let _serial = plx_base::testlock::serial();
    assert_reopen_refused(Reopen::Full200);
}

#[test]
fn a_reopen_answered_from_the_wrong_byte_is_refused() {
    let _serial = plx_base::testlock::serial();
    assert_reopen_refused(Reopen::WrongStart);
}

/// The same with a transfer that dies (a head promising bytes the connection then never sends)
/// rather than ends: no reopen, whatever the transport reports.
#[test]
fn an_unsized_body_that_dies_is_not_reopened() {
    let _serial = plx_base::testlock::serial();
    let srv = ShortBody::start(SHORT_HEAD, false, false);
    let (got, end) = with_sized_source(srv.port, -1, read_to_end);
    assert_eq!(got, b"ABCD");
    assert!(end < 0 || end == AVERROR_EOF, "{end}");
    assert_eq!(srv.heads().len(), 1, "no reopen: {:?}", srv.heads());
}

/// Read the cut the way the demux loop does while the AU queues are full: through `drain_wire`,
/// which parks the failure in `io_failed` and leaves the bytes it did receive in the bounce.
fn drain_until_failed(op: *mut c_void) {
    let s = unsafe { &mut *(op as *mut AvioState) };
    let began = std::time::Instant::now();
    while !s.io_failed {
        s.drain_wire();
        assert!(began.elapsed() < std::time::Duration::from_secs(5), "the drain never saw the cut");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(s.bounce_remaining(), "the bytes sent before the cut are in the bounce");
}

/// The usual mid-playback case: the demuxer sits in `aq_push_with_drain`, so it is `drain_wire`,
/// not `read_cb`, that meets the reset or early close. `io_failed` was then answered with EIO
/// before any reopen was considered, and the session ended.
#[test]
fn a_cut_seen_by_the_drain_is_reopened_at_the_byte_reached() {
    let _serial = plx_base::testlock::serial();
    let srv = ShortBody::start(SHORT_HEAD, false, false);
    let got = with_source(srv.port, |op| {
        drain_until_failed(op);
        read_to_end(op).0
    });
    assert_eq!(got, BODY, "the rest of the body must come from a ranged reopen");
    let heads = srv.heads();
    assert!(heads.len() == 2 && heads[1].contains("Range: bytes=4-"), "{heads:?}");
}

/// The reopen a drained cut asks for is bounded and checked like any other: a server that
/// answers it with the whole body ends the read, once, with nothing from the wrong offset.
#[test]
fn a_cut_seen_by_the_drain_whose_reopen_is_refused_fails_without_looping() {
    let _serial = plx_base::testlock::serial();
    let srv = ShortBody::start_with(SHORT_HEAD, false, Reopen::Full200);
    with_source(srv.port, |op| {
        drain_until_failed(op);
        let (got, end) = read_to_end(op);
        assert_eq!(got, b"ABCD", "nothing from the wrong offset may be delivered");
        assert_eq!(end, AVERROR_IO);
        assert!(unsafe { (*(op as *mut AvioState)).io_failed }, "the failure is published");
        let mut buf = [0u8; 16];
        for _ in 0..8 {
            assert_eq!(read_cb(op, buf.as_mut_ptr(), buf.len() as c_int), AVERROR_IO);
        }
        let reopens = srv.heads().len() - 1;
        assert!(reopens <= BODY_HEAL_ATTEMPTS as usize, "unbounded reopens: {:?}", srv.heads());
    });
}

/// An unsized body is not reopened by a drained cut either.
#[test]
fn an_unsized_body_whose_cut_the_drain_saw_is_not_reopened() {
    let _serial = plx_base::testlock::serial();
    let srv = ShortBody::start(SHORT_HEAD, false, false);
    with_sized_source(srv.port, -1, |op| {
        drain_until_failed(op);
        let (got, end) = read_to_end(op);
        assert_eq!(got, b"ABCD");
        assert_eq!(end, AVERROR_IO);
    });
    assert_eq!(srv.heads().len(), 1, "no reopen: {:?}", srv.heads());
}
