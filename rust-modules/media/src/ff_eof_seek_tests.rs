//! A seek to the END of the resource — `avio_seek(pb, size)` — is a position, not a request.
//!
//! **The failure under test.** An MP4 laid out `ftyp`, `mdat` (64-bit size), `moov` at the end,
//! with a QuickTime CHAPTER track whose text samples sit at the head of `mdat` (where FFmpeg's own
//! mp4 muxer writes them: `mov_create_chapter_track` runs from `mov_write_header`). libavformat's
//! `mov_read_header` parses `moov`, which leaves the AVIO at exactly EOF, then calls
//! `mov_read_chapters`, which records `cur_pos = avio_tell()` (= the file size), seeks back to the
//! chapter samples, reads them, and RESTORES `avio_seek(pb, cur_pos)`. That restore reached
//! `seek_cb` as a target equal to the size, and the socket arm reopened with
//! `Range: bytes=<size>-` — which a server must answer `416 Range Not Satisfiable` (PMS does; RFC
//! 9110 §14.1.2: a first-byte-pos at or past the length is unsatisfiable). `seek_cb` had already
//! `http_close`d the live stream, so the 416 left NO transport, while libavformat's buffer still
//! sat on the chapter samples. The first video sample (0xa87 on the reported film) lies a few
//! bytes past them — inside the buffer, so no `seek_cb` heals it — and the next `fill_buffer`
//! read a dead socket: `stream 0, offset 0xa87: partial file`, one 279-byte AU, `demux ended`,
//! a player at t=0 forever. The device log, in order: `ff: open status=200`, `stream: GET … status=416`,
//! `ff: container names …`, `ff: AU#0 size=279`, `ff: demux ended`.
//!
//! Nothing here is about 32 bits: the size is an `i64` all the way through and the 416 is for the
//! correct offset. The host reproduces it with a 64-bit build at both 2.26 GB and 23 GB.
//!
//! **What is asserted.** On both transports, a seek to (or past) the size answers the target
//! WITHOUT a request, the next read is a clean EOF, and the next real seek reopens normally. Then,
//! with the bundled FFmpeg, the whole open of a virtual 23 GB moov-at-end MP4 with a chapter track
//! sends no request the server refuses and delivers the film's video.
//!
//! The FFmpeg tests need the host build of the bundled FFmpeg and the `ffmpeg` / `ffprobe` CLI, so
//! they are `#[ignore]`d in the plain suite (they skip, loudly, when the CLI is absent):
//! `cd rust-modules && PLX_FFMPEG_DIR=<dir holding libav*-plx.*.dylib> CARGO_INCREMENTAL=0
//! cargo test --lib -p plx_media ff::eof_seek_tests -- --ignored`.

use super::*;
use std::io::{Read, Write};
use std::os::raw::c_long;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// -- a server that refuses an unsatisfiable Range the way PMS does ------------------------------

/// A resource made of real bytes with a run of zeros spliced in: `head`, then `gap` zero bytes,
/// then `tail`. That is how a 23 GB file is served from a few hundred KB of fixture — FFmpeg never
/// reads the inside of `mdat` that no sample points at.
#[derive(Clone)]
struct Virtual {
    head: Arc<Vec<u8>>,
    gap: u64,
    tail: Arc<Vec<u8>>,
}

impl Virtual {
    fn flat(bytes: Vec<u8>) -> Virtual {
        Virtual {
            head: Arc::new(bytes),
            gap: 0,
            tail: Arc::new(Vec::new()),
        }
    }

    fn len(&self) -> u64 {
        self.head.len() as u64 + self.gap + self.tail.len() as u64
    }

    /// Write `[from, len)` to `w` in slices; stops quietly when the client goes away.
    fn write_from(&self, from: u64, w: &mut impl Write) {
        let head = self.head.len() as u64;
        let tail_at = head + self.gap;
        let mut at = from;
        let zeros = [0u8; 64 * 1024];
        while at < self.len() {
            let r = if at < head {
                w.write_all(&self.head[at as usize..]).map(|_| head - at)
            } else if at < tail_at {
                let n = (tail_at - at).min(zeros.len() as u64);
                w.write_all(&zeros[..n as usize]).map(|_| n)
            } else {
                w.write_all(&self.tail[(at - tail_at) as usize..])
                    .map(|_| self.len() - at)
            };
            match r {
                Ok(n) => at += n,
                Err(_) => return,
            }
        }
        let _ = w.flush();
    }
}

/// One request the server answered: the `Range` start (`None` for a plain GET) and the status.
type Served = Arc<Mutex<Vec<(Option<u64>, u16)>>>;

/// A loopback server over a [`Virtual`] that answers like PMS: `200` without a Range, `206` with a
/// `Content-Range` for a satisfiable one, and `416` with `Content-Range: bytes */<len>` for a
/// first byte at or past the end. One thread per connection, `Connection: close`; stops on drop.
struct StrictServer {
    port: u16,
    served: Served,
    stop: Arc<AtomicBool>,
}

impl StrictServer {
    fn start(file: Virtual) -> StrictServer {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).expect("set_nonblocking");
        let served: Served = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, accept_stop) = (served.clone(), stop.clone());
        std::thread::spawn(move || {
            while !accept_stop.load(Ordering::Acquire) {
                match plx_base::testnet::accept(&listener) {
                    Ok((s, _)) => {
                        let (file, log) = (file.clone(), log.clone());
                        std::thread::spawn(move || serve_one(s, &file, &log));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(_) => break,
                }
            }
        });
        StrictServer { port, served, stop }
    }

    fn served(&self) -> Vec<(Option<u64>, u16)> {
        self.served.lock().unwrap().clone()
    }

    fn refused(&self) -> Vec<Option<u64>> {
        self.served()
            .into_iter()
            .filter(|(_, status)| *status == 416)
            .map(|(at, _)| at)
            .collect()
    }
}

impl Drop for StrictServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn serve_one(mut s: std::net::TcpStream, file: &Virtual, log: &Served) {
    let _ = s.set_nonblocking(false);
    let mut head = Vec::new();
    let mut tmp = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&tmp[..n]),
        }
    }
    let text = String::from_utf8_lossy(&head).to_ascii_lowercase();
    let start = text.lines().find_map(|l| {
        l.strip_prefix("range: bytes=")
            .and_then(|r| r.split('-').next())
            .and_then(|a| a.trim().parse::<u64>().ok())
    });
    let n = file.len();
    let (status, header, from) = match start {
        Some(at) if at >= n => (
            416,
            format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{n}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ),
            None,
        ),
        Some(at) => (
            206,
            format!(
                "HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {at}-{}/{n}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                n - 1,
                n - at
            ),
            Some(at),
        ),
        None => (
            200,
            format!(
                "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Length: {n}\r\nConnection: close\r\n\r\n"
            ),
            Some(0),
        ),
    };
    log.lock().unwrap().push((start, status));
    if s.write_all(header.as_bytes()).is_err() {
        return;
    }
    if let Some(from) = from {
        file.write_from(from, &mut s);
    }
}

// -- the callbacks alone: no FFmpeg, both transports ---------------------------------------------

/// The fixture body: 64 distinct bytes, so a read at the wrong offset cannot look right.
fn pattern() -> Vec<u8> {
    (0..64u8)
        .map(|i| i.wrapping_mul(37).wrapping_add(11))
        .collect()
}

fn avio_over(src: Src, size: i64, aq: *mut AuQueue) -> AvioState {
    AvioState {
        src,
        aq,
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
    }
}

/// The invariant on one opened source: what libavformat's `mov_read_chapters` does — read a
/// little, seek to the size (or `SEEK_END` 0), read, then seek back into the file and read.
fn seek_to_the_end_is_a_position(server: &StrictServer, st: &mut AvioState, body: &[u8]) {
    let op = st as *mut AvioState as *mut c_void;
    let n = body.len() as i64;
    let mut dst = [0u8; 16];
    assert_eq!(
        read_cb(op, dst.as_mut_ptr(), 4),
        4,
        "fixture: the open reads"
    );
    let before = server.served().len();

    for (offset, whence, what) in [(n, SEEK_SET, "SEEK_SET size"), (0, SEEK_END, "SEEK_END 0")] {
        assert_eq!(
            seek_cb(op, offset, whence),
            n,
            "a seek to the end ({what}) must answer the size — libavformat restores an `avio_tell` \
             taken at EOF this way, and a -1 leaves it reading a transport that is gone"
        );
        assert_eq!(
            server.served().len(),
            before,
            "a seek to the end ({what}) must not request `Range: bytes={n}-`, which the server \
             can only refuse: served {:?}",
            server.served()
        );
        assert_eq!(
            read_cb(op, dst.as_mut_ptr(), dst.len() as c_int),
            AVERROR_EOF,
            "at the end ({what}) a read is a clean EOF, not a transport error"
        );
        assert!(
            !st.io_failed,
            "an EOF at the end is not an I/O failure ({what})"
        );
    }
    assert!(
        server.refused().is_empty(),
        "nothing may be refused: {:?}",
        server.served()
    );

    // And the source is still a source: the next real seek reopens at the byte asked for.
    assert_eq!(
        seek_cb(op, 10, SEEK_SET),
        10,
        "a seek back into the file reopens"
    );
    let got = read_cb(op, dst.as_mut_ptr(), dst.len() as c_int);
    assert!(got > 0, "the reopened source reads, got {got}");
    assert_eq!(
        &dst[..got as usize],
        &body[10..10 + got as usize],
        "bytes from offset 10"
    );
    assert_eq!(
        server.served().last(),
        Some(&(Some(10), 206)),
        "{:?}",
        server.served()
    );
}

/// **The regression, on the transport the reported set used** (plaintext PMS on the LAN). Before
/// the fix `seek_cb` closed the stream, asked for `Range: bytes=64-`, got the 416 and returned -1.
#[test]
fn a_socket_seek_to_the_end_answers_the_size_without_a_request() {
    let body = pattern();
    let server = StrictServer::start(Virtual::flat(body.clone()));
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut hs = plx_net::stream::http_stream_boxed();
    let origin = plx_plex::plex::Origin::http("127.0.0.1", i32::from(server.port));
    let (src, size) =
        open_plain_progressive(&mut *hs, &origin, "/library/parts/1/file.mp4", &mut *aq)
            .unwrap_or_else(|e| panic!("fixture: the plain open failed: {e:?}"));
    assert_eq!(
        size,
        body.len() as i64,
        "fixture: the size is the Content-Length"
    );
    let mut st = avio_over(src, size, &mut *aq);
    seek_to_the_end_is_a_position(&server, &mut st, &body);
    drop(st);
    plx_net::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
}

/// The same on the https arm's source. `curlio` already calls the 416 "the correct response to a
/// seek at or past EOF" and keeps it from poisoning the source, but `seek_cb` still reported the
/// seek as failed, which is the half libavformat acts on.
#[test]
fn a_curl_seek_to_the_end_answers_the_size_without_a_request() {
    // The curlio suite's own gate: serialised, and a skip where libcurl's multi table is absent.
    let _g = plx_base::testlock::serial();
    if !(plx_net::net::global_init() && crate::curlio::available()) {
        eprintln!("SKIP: libcurl multi is not available on this host");
        return;
    }
    let body = pattern();
    let server = StrictServer::start(Virtual::flat(body.clone()));
    let mut aq = crate::aq::aq_new(1 << 20);
    let url = format!("http://127.0.0.1:{}/library/parts/1/file.mp4", server.port);
    let cs = crate::curlio::CurlSource::open(&url, 0)
        .unwrap_or_else(|e| panic!("fixture: the curl open failed: {e:?}"));
    let size = cs.size();
    assert_eq!(
        size,
        body.len() as i64,
        "fixture: the size is the Content-Length"
    );
    let mut st = avio_over(Src::Curl(cs), size, &mut *aq);
    seek_to_the_end_is_a_position(&server, &mut st, &body);
    drop(st);
    crate::aq::aq_destroy(&mut *aq);
}

/// The non-regression half of the invariant above: a seek that lands INSIDE the file, however
/// close to the end, is still a request. Each one closes the stream, reopens it with
/// `Range: bytes=<target>-` (one request per seek, answered `206`), and the bytes read afterwards
/// are the file's bytes from `target`. Only `target >= size` is parked, so `size - 1` is the last
/// byte that is fetched, and a seek into the file after a parked end reopens as well.
fn a_seek_inside_the_file_still_reopens_with_a_range(
    server: &StrictServer,
    st: &mut AvioState,
    body: &[u8],
) {
    let op = st as *mut AvioState as *mut c_void;
    let n = body.len() as i64;
    let mut dst = [0u8; 16];
    assert_eq!(
        read_cb(op, dst.as_mut_ptr(), 4),
        4,
        "fixture: the open reads"
    );

    // (offset, whence, the byte it lands on, what it is)
    let inside = [
        (20, SEEK_SET, 20, "SEEK_SET into the middle"),
        (-8, SEEK_END, n - 8, "SEEK_END -8"),
        (n - 1, SEEK_SET, n - 1, "SEEK_SET to the last byte"),
        (30, SEEK_SET, 30, "SEEK_SET back into the middle"),
    ];
    for (offset, whence, target, what) in inside {
        let before = server.served().len();
        assert_eq!(
            seek_cb(op, offset, whence),
            target,
            "{what} answers the byte it landed on"
        );
        let served = server.served();
        assert_eq!(
            served.len(),
            before + 1,
            "{what} must reopen the connection, exactly one request: {served:?}"
        );
        assert_eq!(
            served.last(),
            Some(&(Some(target as u64), 206)),
            "{what} must ask for `Range: bytes={target}-` and be served it: {served:?}"
        );
        let got = read_cb(op, dst.as_mut_ptr(), dst.len() as c_int);
        assert!(got > 0, "{what}: the reopened source reads, got {got}");
        assert_eq!(
            &dst[..got as usize],
            &body[target as usize..target as usize + got as usize],
            "{what}: bytes from offset {target}"
        );
        assert!(!st.io_failed, "{what} is not an I/O failure");
    }

    // Park at the end (no request), then seek back into the file: it reopens with a Range.
    let before = server.served().len();
    assert_eq!(seek_cb(op, n, SEEK_SET), n);
    assert_eq!(server.served().len(), before, "the seek to the end is free");
    assert_eq!(
        read_cb(op, dst.as_mut_ptr(), dst.len() as c_int),
        AVERROR_EOF
    );
    assert_eq!(seek_cb(op, 12, SEEK_SET), 12);
    assert_eq!(
        server.served().last(),
        Some(&(Some(12), 206)),
        "a seek into the file after a parked end reopens with a Range: {:?}",
        server.served()
    );
    let got = read_cb(op, dst.as_mut_ptr(), dst.len() as c_int);
    assert!(
        got > 0,
        "the source reads again after the parked end, got {got}"
    );
    assert_eq!(&dst[..got as usize], &body[12..12 + got as usize]);
    assert!(
        server.refused().is_empty(),
        "nothing may be refused: {:?}",
        server.served()
    );
}

/// **Mid-file seeks are unchanged on the socket arm** (the webOS 4+ behaviour the EOF guard must
/// not touch): each one reopens with a Range request.
#[test]
fn a_socket_seek_inside_the_file_still_reopens_with_a_range() {
    let body = pattern();
    let server = StrictServer::start(Virtual::flat(body.clone()));
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut hs = plx_net::stream::http_stream_boxed();
    let origin = plx_plex::plex::Origin::http("127.0.0.1", i32::from(server.port));
    let (src, size) =
        open_plain_progressive(&mut *hs, &origin, "/library/parts/1/file.mp4", &mut *aq)
            .unwrap_or_else(|e| panic!("fixture: the plain open failed: {e:?}"));
    assert_eq!(
        size,
        body.len() as i64,
        "fixture: the size is the Content-Length"
    );
    let mut st = avio_over(src, size, &mut *aq);
    a_seek_inside_the_file_still_reopens_with_a_range(&server, &mut st, &body);
    drop(st);
    plx_net::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
}

/// The same on the https arm's source.
#[test]
fn a_curl_seek_inside_the_file_still_reopens_with_a_range() {
    let _g = plx_base::testlock::serial();
    if !(plx_net::net::global_init() && crate::curlio::available()) {
        eprintln!("SKIP: libcurl multi is not available on this host");
        return;
    }
    let body = pattern();
    let server = StrictServer::start(Virtual::flat(body.clone()));
    let mut aq = crate::aq::aq_new(1 << 20);
    let url = format!("http://127.0.0.1:{}/library/parts/1/file.mp4", server.port);
    let cs = crate::curlio::CurlSource::open(&url, 0)
        .unwrap_or_else(|e| panic!("fixture: the curl open failed: {e:?}"));
    let size = cs.size();
    assert_eq!(
        size,
        body.len() as i64,
        "fixture: the size is the Content-Length"
    );
    let mut st = avio_over(Src::Curl(cs), size, &mut *aq);
    a_seek_inside_the_file_still_reopens_with_a_range(&server, &mut st, &body);
    drop(st);
    crate::aq::aq_destroy(&mut *aq);
}

/// An unsized source has no end to compare with: a seek there still goes to the transport, as it
/// always did. Pins the guard to a KNOWN size.
#[test]
fn an_unsized_source_still_asks_the_transport() {
    let body = pattern();
    let server = StrictServer::start(Virtual::flat(body.clone()));
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut hs = plx_net::stream::http_stream_boxed();
    let origin = plx_plex::plex::Origin::http("127.0.0.1", i32::from(server.port));
    let (src, _) = open_plain_progressive(&mut *hs, &origin, "/library/parts/1/file.mp4", &mut *aq)
        .unwrap_or_else(|e| panic!("fixture: the plain open failed: {e:?}"));
    let mut st = avio_over(src, -1, &mut *aq);
    let op = &mut st as *mut AvioState as *mut c_void;
    assert_eq!(
        seek_cb(op, 64, SEEK_SET),
        -1,
        "the server refuses it, and so does the seek"
    );
    assert_eq!(server.refused(), vec![Some(64)], "{:?}", server.served());
    drop(st);
    plx_net::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
}

// -- the whole open, with the bundled FFmpeg ------------------------------------------------------

/// Where the reported film's `moov` started, and so where this fixture's does.
const REPORTED_MOOV_AT: u64 = 23_335_526_369;
/// The same layout at the size of the reported film that played (2.26 GB): the defect is the
/// layout, not the size.
const SMALLER_MOOV_AT: u64 = 2_260_000_000;

fn tool_present(name: &str) -> bool {
    Command::new(name)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("plx-eof-seek-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Write a small MP4 with FFmpeg's mp4 muxer: 2 s of 320x240 H.264 + AAC and three chapters, no
/// `+faststart`, so the layout is `ftyp`, `free`, `mdat`, `moov` with the chapter track's samples at
/// the head of `mdat`.
fn generate(dir: &Path) -> PathBuf {
    let meta = dir.join("chapters.txt");
    std::fs::write(
        &meta,
        ";FFMETADATA1\n\
         [CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=700\ntitle=Opening\n\
         [CHAPTER]\nTIMEBASE=1/1000\nSTART=700\nEND=1400\ntitle=Middle\n\
         [CHAPTER]\nTIMEBASE=1/1000\nSTART=1400\nEND=2000\ntitle=End\n",
    )
    .expect("write the chapter metadata");
    let out = dir.join("chapters.mp4");
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=s=320x240:r=25:d=2"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
        ])
        .arg("-i")
        .arg(&meta)
        .args(["-map", "0:v", "-map", "1:a", "-map_chapters", "2"])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "25",
        ])
        .args(["-c:a", "aac", "-b:a", "64k"])
        .arg(&out)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg could not write the fixture");
    out
}

/// The top-level boxes of an ISO-BMFF file as `(type, offset, size)`; 32-bit sizes only, which
/// is what the small fixture has.
fn top_level_boxes(file: &[u8]) -> Vec<([u8; 4], usize, usize)> {
    let mut boxes = Vec::new();
    let mut at = 0;
    while at + 8 <= file.len() {
        let size = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
        assert!(size >= 8, "fixture: a box with size {size} at {at}");
        boxes.push((file[at + 4..at + 8].try_into().unwrap(), at, size));
        at += size;
    }
    boxes
}

/// Re-lay the fixture as the reported film: `ftyp`, ONE `mdat` with a 64-bit size whose payload is
/// the fixture's followed by zeros, `moov` starting at `moov_at`. FFmpeg's `free` (8) + `mdat`
/// header (8) is exactly a 16-byte 64-bit `mdat` header — the same rewrite the muxer itself makes
/// for an mdat over 4 GiB — so every chunk offset in `moov` stays right and nothing in it is edited.
fn as_large_file(file: &[u8], moov_at: u64) -> Virtual {
    let boxes = top_level_boxes(file);
    let kinds: Vec<&[u8]> = boxes.iter().map(|b| &b.0[..]).collect();
    assert_eq!(
        kinds,
        [&b"ftyp"[..], b"free", b"mdat", b"moov"],
        "fixture: expected ftyp, free, mdat, moov"
    );
    let (ftyp, free, mdat, moov) = (boxes[0], boxes[1], boxes[2], boxes[3]);
    assert_eq!(free.2, 8, "fixture: an 8-byte free box ahead of mdat");
    let payload = &file[mdat.1 + 8..mdat.1 + mdat.2];
    let mut head = file[..ftyp.2].to_vec();
    let mdat_size = moov_at - ftyp.2 as u64;
    head.extend_from_slice(&1u32.to_be_bytes());
    head.extend_from_slice(b"mdat");
    head.extend_from_slice(&mdat_size.to_be_bytes());
    head.extend_from_slice(payload);
    assert_eq!(
        head.len(),
        mdat.1 + mdat.2,
        "fixture: chunk offsets unmoved"
    );
    Virtual {
        gap: moov_at - head.len() as u64,
        head: Arc::new(head),
        tail: Arc::new(file[moov.1..moov.1 + moov.2].to_vec()),
    }
}

/// The fixture has the shape the test claims: a chapter track whose samples precede the first
/// video sample. Measured with ffprobe, not trusted from the command line that wrote it.
fn assert_chapters_lead_the_video(path: &Path) {
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pos",
        ])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(probe.status.success(), "ffprobe could not read the fixture");
    let first_video = String::from_utf8_lossy(&probe.stdout)
        .lines()
        .filter_map(|l| l.trim().trim_end_matches(',').parse::<u64>().ok())
        .min()
        .expect("fixture: no video packets");
    let chapters = Command::new("ffprobe")
        .args(["-v", "error", "-show_chapters", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    let count = String::from_utf8_lossy(&chapters.stdout).lines().count();
    assert_eq!(count, 3, "fixture: three chapters");
    assert!(
        first_video > 48,
        "fixture: the first video sample ({first_video}) must follow the chapter samples at the \
         head of mdat"
    );
}

/// Demux `file` through the real `ff::demux` over a [`StrictServer`]; returns the video AUs it
/// delivered within the deadline and everything the server answered.
fn demux_video_aus(file: Virtual, want: usize) -> (usize, Vec<(Option<u64>, u16)>) {
    SHARED.seek_to_ns.store(-1, Ordering::Relaxed);
    SHARED.desired_audio_idx.store(-1, Ordering::Relaxed);
    SHARED.side_subs_owner.store(false, Ordering::Relaxed);
    SHARED.demux_failed.store(false, Ordering::Relaxed);
    SHARED.demux_io_failed.store(false, Ordering::Relaxed);
    FEED_AUDIO.store(true, Ordering::Relaxed);
    let server = StrictServer::start(file);
    let (video_cap, audio_cap) = crate::player::engine::aq_caps();
    let mut video = crate::aq::aq_new(video_cap as c_long);
    let mut audio = crate::aq::aq_new(audio_cap as c_long);
    let mut hs = plx_net::stream::http_stream_boxed();
    let (vp, ap, hp) = (
        SendPtr(&mut *video as *mut AuQueue),
        SendPtr(&mut *audio as *mut AuQueue),
        SendPtr(&mut *hs as *mut HttpStream),
    );
    let origin = plx_plex::plex::Origin::http("127.0.0.1", i32::from(server.port));
    let thread = std::thread::spawn(move || {
        let off = plx_base::task::OffFrame::for_test();
        demux(
            &off,
            origin,
            "/library/parts/1/file.mp4".into(),
            "aac".into(),
            None,
            None,
            vp,
            ap,
            hp,
        )
    });
    let until = Instant::now() + Duration::from_secs(10);
    let mut aus = 0;
    while aus < want && Instant::now() < until {
        let node = crate::aq::aq_pop(&mut *video, std::ptr::null_mut());
        if !node.is_null() {
            unsafe { libc::free(node as *mut c_void) };
            aus += 1;
            continue;
        }
        // Keep the audio lane moving too; nothing here grades it.
        let a = crate::aq::aq_pop(&mut *audio, std::ptr::null_mut());
        if !a.is_null() {
            unsafe { libc::free(a as *mut c_void) };
            continue;
        }
        if thread.is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    crate::aq::aq_abort(&mut *video);
    crate::aq::aq_abort(&mut *audio);
    plx_net::stream::http_shutdown(&mut *hs);
    let stop_by = Instant::now() + Duration::from_secs(10);
    while !thread.is_finished() && Instant::now() < stop_by {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        thread.is_finished(),
        "the demuxer did not stop within 10 s of its lanes aborting"
    );
    let _ = thread.join();
    let served = server.served();
    plx_net::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *video);
    crate::aq::aq_destroy(&mut *audio);
    (aus, served)
}

fn moov_at_end_with_chapters_plays(tag: &str, moov_at: u64) {
    if !tool_present("ffmpeg") || !tool_present("ffprobe") {
        eprintln!("SKIP {tag}: the ffmpeg / ffprobe command line tools are not installed");
        return;
    }
    let _ffmpeg = super::image_subtitle_tests::bind_host_ffmpeg();
    let scratch = Scratch::new(tag);
    let path = generate(&scratch.0);
    assert_chapters_lead_the_video(&path);
    let file = as_large_file(&std::fs::read(&path).expect("read the fixture"), moov_at);
    let size = file.len();
    // 2 s at 25 fps.
    let (aus, served) = demux_video_aus(file, 50);
    let refused: Vec<_> = served.iter().filter(|(_, s)| *s == 416).collect();
    assert!(
        refused.is_empty(),
        "{tag}: the open sent a Range the server can only refuse — {refused:?} of a {size}-byte \
         file (all requests: {served:?})"
    );
    assert_eq!(
        aus, 50,
        "{tag}: only {aus} of 50 video AUs reached the lane (requests: {served:?})"
    );
}

/// **The regression, end to end**: the reported film's layout and size.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg and the ffmpeg CLI — see the module doc"]
fn a_23_gb_moov_at_end_mp4_with_chapters_opens_and_plays() {
    let _g = plx_base::testlock::serial();
    moov_at_end_with_chapters_plays("23gb", REPORTED_MOOV_AT);
}

/// The same layout under 4 GiB. Not a control for the fix — the defect never depended on the size —
/// but the evidence that it does not: before the fix this failed exactly like the 23 GB case.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg and the ffmpeg CLI — see the module doc"]
fn a_2_gb_moov_at_end_mp4_with_chapters_opens_and_plays() {
    let _g = plx_base::testlock::serial();
    moov_at_end_with_chapters_plays("2gb", SMALLER_MOOV_AT);
}
