//! The log file: `<data_dir>/plx-hello.log`, append, one line per call, flushed per line.
//!
//! The C# host appends its own `[host]` lines to the same file, so a tester reads ONE file to see
//! the whole story. Every line is also mirrored to stderr (on the TV 10.0 emulator `sdb dlog` shows
//! nothing, so do not count on it).
//!
//! A log that cannot be opened is reported on stderr and is NOT fatal: the window and key tests are
//! still worth running, and the tester sees the colour change without the file.
//!
//! Network sink: on a retail TV nothing can read the file or dlog, so when the host passes a
//! `HOST:PORT` (the `res/log-host.txt` that `build.sh --log-host` ships), every line written to the
//! file is also sent as ONE UDP datagram `"<seq> <line>"`. `<seq>` is a six-digit counter that
//! increments per line even when a send fails, so a gap at the receiver is a lost datagram. The
//! socket is non-blocking and every failure is ignored: the log must never stall or break the app.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Largest datagram sent: under a 1500-byte Ethernet MTU minus IP and UDP headers. A longer line is
/// cut (marked `...`) in the datagram only; the file keeps it whole. Text that must reach the receiver
/// whole goes through [`long`], which never produces a line this long.
const MAX_DATAGRAM: usize = 1400;

/// Longest text [`long`] puts on one log line: the datagram's budget less the `<seq> ` and `[plx ...] `
/// prefixes (about 40 bytes) and a margin, so no chunk is ever cut by [`datagram`].
const CHUNK_MAX: usize = 1000;

/// What a continuation chunk of an over-long line starts with, so a reader can tell it from a new line.
const CONT: &str = "(cont.) ";

/// The UDP mirror of the log. Independent of the global [`SINK`] so it can be tested alone.
struct NetSink {
    sock: UdpSocket,
    to: SocketAddrV4,
    seq: u64,
}

impl NetSink {
    /// Plain `std::net::UdpSocket` (a thin wrapper over socket/bind/sendto, close-on-exec, no
    /// `unsafe`), switched to non-blocking so a send can never wait.
    fn new(to: SocketAddrV4) -> io::Result<NetSink> {
        let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        sock.set_nonblocking(true)?;
        Ok(NetSink { sock, to, seq: 0 })
    }

    /// Send one line. The sequence number advances whether or not the send worked.
    fn send(&mut self, text: &str) {
        let dgram = datagram(self.seq, text);
        self.seq += 1;
        let _ = self.sock.send_to(dgram.as_bytes(), self.to);
    }
}

/// `"<seq:06> <text>"`, cut to [`MAX_DATAGRAM`] bytes on a char boundary (and ended with `...\n`).
fn datagram(seq: u64, text: &str) -> String {
    let mut s = format!("{seq:06} {text}");
    if s.len() > MAX_DATAGRAM {
        let mut cut = MAX_DATAGRAM - 4;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("...\n");
    }
    s
}

/// `a.b.c.d:port` (IPv4 literal, non-zero port). No name resolution: a blocking `getaddrinfo` has no
/// place in a logger.
fn parse_log_host(s: &str) -> Result<SocketAddrV4, String> {
    let a: SocketAddrV4 = s
        .parse()
        .map_err(|_| "expected IPV4:PORT, e.g. 10.0.2.2:9999".to_string())?;
    if a.port() == 0 {
        return Err("port 0".to_string());
    }
    Ok(a)
}

struct Sink {
    file: Option<File>,
    net: Option<NetSink>,
    t0: Instant,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

/// Mirror to stderr WITHOUT `eprint!`: that panics when the write fails (a closed stderr), and
/// logging also runs inside the panic hook, where a second panic aborts the process with no evidence.
pub fn to_stderr(text: &str) {
    let _ = std::io::stderr().write_all(text.as_bytes());
}

/// Open (append) `<dir>/plx-hello.log` and, when `log_host` is `IPV4:PORT`, start mirroring every
/// line to it over UDP. Idempotent; the first call wins. Returns the path and whether the file
/// opened. The first line written says whether the network sink is on.
pub fn init(dir: &str, log_host: Option<&str>) -> (String, bool) {
    let path = format!("{}/plx-hello.log", dir.trim_end_matches('/'));
    let file = OpenOptions::new().create(true).append(true).open(&path);
    let ok = file.is_ok();
    if let Err(e) = &file {
        to_stderr(&format!("plx: cannot open log {path}: {e}\n"));
    }
    let (net, note) = match log_host.map(str::trim).filter(|h| !h.is_empty()) {
        None => (None, "network log: off (no log host given)".to_string()),
        Some(h) => match parse_log_host(h).map_err(|e| format!("{e} ({h:?})")).and_then(|to| {
            NetSink::new(to)
                .map(|n| (to, n))
                .map_err(|e| format!("socket: {e}"))
        }) {
            Ok((to, n)) => (
                Some(n),
                format!("network log: UDP -> {to}, one \"<seq> <line>\" datagram per line"),
            ),
            Err(e) => (None, format!("network log: OFF, unusable log host: {e}")),
        },
    };
    let _ = SINK.set(Mutex::new(Sink {
        file: file.ok(),
        net,
        t0: Instant::now(),
    }));
    line(&note);
    (path, ok)
}

fn tid() -> i64 {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: gettid takes no arguments and cannot fail.
        unsafe { libc::syscall(libc::SYS_gettid) as i64 }
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// Write one line. Never panics, never blocks on anything but the file.
pub fn line(msg: &str) {
    let Some(m) = SINK.get() else {
        to_stderr(&format!("plx: {msg}\n"));
        return;
    };
    let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
    let t = s.t0.elapsed();
    let text = format!("[plx +{:>4}.{:03}s t{}] {msg}\n", t.as_secs(), t.subsec_millis(), tid());
    if let Some(f) = s.file.as_mut() {
        let _ = f.write_all(text.as_bytes());
        let _ = f.flush();
    }
    if let Some(n) = s.net.as_mut() {
        n.send(&text);
    }
    to_stderr(&text);
}

/// The log lines [`long`] writes for `msg`: one per physical line of it, and a physical line longer than
/// [`CHUNK_MAX`] bytes split on char boundaries into several, each continuation marked [`CONT`]. Empty
/// lines are dropped; every other byte of the text is in exactly one chunk.
fn chunks(msg: &str) -> Vec<String> {
    let mut out = Vec::new();
    for physical in msg.lines().filter(|l| !l.is_empty()) {
        let mut rest = physical;
        let mut first = true;
        while !rest.is_empty() {
            let room = if first { CHUNK_MAX } else { CHUNK_MAX - CONT.len() };
            let mut cut = room.min(rest.len());
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            let (head, tail) = rest.split_at(cut);
            out.push(if first { head.to_string() } else { format!("{CONT}{head}") });
            rest = tail;
            first = false;
        }
    }
    out
}

/// Write `msg` in FULL: a driver's info log, the GL extension string and the panel's result lines can
/// all be longer than one datagram, and [`line`] cuts those for the UDP receiver (only; the file keeps
/// them). Here each physical line is its own log line and a long one is split into continuations, so
/// nothing is lost on the wire.
pub fn long(msg: &str) {
    let parts = chunks(msg);
    if parts.is_empty() {
        line("");
    }
    for part in parts {
        line(&part);
    }
}

/// Seconds since the Unix epoch, for the header line (the host's clock may be unset on a TV).
pub fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

macro_rules! plog {
    ($($arg:tt)*) => { $crate::log::line(&format!($($arg)*)) };
}
pub(crate) use plog;

/// [`plog!`] for text that must not be cut on the wire ([`long`]).
macro_rules! plog_full {
    ($($arg:tt)*) => { $crate::log::long(&format!($($arg)*)) };
}
pub(crate) use plog_full;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn receiver() -> (UdpSocket, SocketAddrV4) {
        let rx = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind receiver");
        rx.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let addr = match rx.local_addr().unwrap() {
            std::net::SocketAddr::V4(a) => a,
            other => panic!("not IPv4: {other}"),
        };
        (rx, addr)
    }

    fn recv_str(rx: &UdpSocket) -> String {
        let mut buf = [0u8; 2048];
        let n = rx.recv(&mut buf).expect("datagram within 2 s");
        String::from_utf8(buf[..n].to_vec()).expect("UTF-8 datagram")
    }

    #[test]
    fn datagrams_arrive_in_order_with_a_sequence_prefix() {
        let (rx, addr) = receiver();
        let mut tx = NetSink::new(addr).unwrap();
        tx.send("[plx +   0.001s t1] first\n");
        tx.send("[plx +   0.002s t1] second\n");
        assert_eq!(recv_str(&rx), "000000 [plx +   0.001s t1] first\n");
        assert_eq!(recv_str(&rx), "000001 [plx +   0.002s t1] second\n");
    }

    #[test]
    fn the_sequence_advances_when_the_send_fails() {
        let (rx, addr) = receiver();
        let mut tx = NetSink::new(addr).unwrap();
        // Port 0 is not a valid destination, so this send fails; the error is ignored and the
        // receiver must see the gap (000000 never arrives).
        tx.to = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0);
        tx.send("lost\n");
        tx.to = addr;
        tx.send("kept\n");
        assert_eq!(tx.seq, 2);
        assert_eq!(recv_str(&rx), "000001 kept\n");
    }

    #[test]
    fn a_long_line_is_cut_on_a_char_boundary_and_stays_one_datagram() {
        let long = "é".repeat(1000) + "\n";
        let d = datagram(7, &long);
        assert!(d.len() <= MAX_DATAGRAM, "{} bytes", d.len());
        assert!(d.starts_with("000007 é"));
        assert!(d.ends_with("...\n"));
        assert_eq!(datagram(8, "short\n"), "000008 short\n");
    }

    /// The text of `chunks`, put back together: continuation chunks glued on, physical lines kept apart.
    fn rejoin(parts: &[String]) -> String {
        let mut out = String::new();
        for p in parts {
            match p.strip_prefix(CONT) {
                Some(more) => out.push_str(more),
                None => {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(p);
                }
            }
        }
        out
    }

    #[test]
    fn long_text_is_split_so_that_no_datagram_is_cut_and_every_byte_arrives() {
        // 3500 bytes of two-byte characters (a cut inside one would be the worst case), a short line, and
        // a CRLF-ended line.
        let text = format!("{}\nsecond line\r\n{}", "é".repeat(1750), "x".repeat(2500));
        let parts = chunks(&text);
        assert!(parts.len() >= 6, "{} chunks", parts.len());
        for (i, p) in parts.iter().enumerate() {
            let d = datagram(123_456, &format!("[plx +9999.999s t99999] {p}\n"));
            assert!(!d.ends_with("...\n") && d.len() <= MAX_DATAGRAM, "chunk {i}: {} bytes, cut: {}", d.len(), d.ends_with("...\n"));
        }
        assert_eq!(rejoin(&parts), text.replace("\r\n", "\n"), "no byte lost, none duplicated");
        assert!(parts.iter().filter(|p| p.starts_with(CONT)).count() >= 4, "the long lines were continued, not cut");
    }

    #[test]
    fn long_text_of_one_short_line_is_one_line_and_empty_lines_are_dropped() {
        assert_eq!(
            chunks("0:37(9): error: syntax error, unexpected NEW_IDENTIFIER"),
            vec!["0:37(9): error: syntax error, unexpected NEW_IDENTIFIER"]
        );
        assert_eq!(chunks("a\n\n  \nb\n"), vec!["a", "  ", "b"], "an empty line goes, a spaces-only one is the text's own");
        assert!(chunks("").is_empty());
        assert_eq!(chunks(&"y".repeat(CHUNK_MAX)).len(), 1, "exactly the budget is one line");
        assert_eq!(chunks(&"y".repeat(CHUNK_MAX + 1)).len(), 2);
    }

    #[test]
    fn log_host_parsing() {
        assert_eq!(parse_log_host("10.0.2.2:9999").unwrap().to_string(), "10.0.2.2:9999");
        for bad in ["", "10.0.2.2", "10.0.2.2:0", "host:9999", "10.0.2.2:99999", "::1:9999", "[::1]:9999"] {
            assert!(parse_log_host(bad).is_err(), "{bad:?} must be refused");
        }
    }
}
