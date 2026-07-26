//! Blocking HTTP/1.1 GET over a raw TCP socket (was src/stream.c). Callers
//! (posters/pms/player) allocate an `HttpStream` and pass `&hs`; this operates on it
//! in place. Header/chunk parsing is bounds-checked (no OOB).
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uchar, c_void};
use std::sync::atomic::{AtomicI32, Ordering};

// repr(C) for a stable layout: the player boxes it + hands raw ptrs across threads.
#[repr(C)]
pub struct HttpStream {
    /// The socket, reached from TWO threads: the demux thread reads (and owns closing) it,
    /// while the main thread interrupts it on a seek/teardown. It was a bare `c_int` mutated
    /// from both sides behind two live `&mut`, and the interrupt was a `close(2)` — which on
    /// Linux does NOT wake a peer blocked in `recv`, so BACK during a stall waited out the
    /// 15 s SO_RCVTIMEO, and the freed fd NUMBER could be handed to a poster worker's
    /// `socket()` and then read into the video AU buffer. Now: atomic, `shutdown(2)` to
    /// interrupt (wakes the reader, keeps the number allocated), and exactly one closer via
    /// `take_fd`'s swap. See `http_shutdown` / `http_close`.
    fd: AtomicI32,
    buf: [u8; 65536],
    blen: c_int,
    bpos: c_int,
    content_length: i64,
    consumed: i64,
    status: c_int,
    chunked: c_int,
    chunk_left: i64,
}

fn errno() -> c_int {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

impl HttpStream {
    /// the live socket, or < 0 once it has been closed
    #[inline]
    fn fd(&self) -> c_int {
        self.fd.load(Ordering::Acquire)
    }
    #[inline]
    fn set_fd(&self, v: c_int) {
        self.fd.store(v, Ordering::Release);
    }
    /// Claim the socket for closing. The swap makes exactly one caller win, so the fd can
    /// never be closed twice (and therefore never be recycled out from under another thread).
    #[inline]
    fn take_fd(&self) -> c_int {
        self.fd.swap(-1, Ordering::AcqRel)
    }
}

/// crate-internal accessors (fields are private) — the player engine reads these.
#[inline]
pub(crate) fn hs_content_length(hs: *const HttpStream) -> i64 { unsafe { (*hs).content_length } }
#[inline]
pub(crate) fn hs_status(hs: *const HttpStream) -> c_int { unsafe { (*hs).status } }

/// one raw body byte (buffered first, then socket) — for chunk framing
unsafe fn hs_getb(hs: &mut HttpStream) -> Option<u8> {
    if (hs.bpos as usize) < (hs.blen as usize) {
        let b = hs.buf[hs.bpos as usize];
        hs.bpos += 1;
        return Some(b);
    }
    let fd = hs.fd();
    if fd < 0 {
        return None;
    }
    let mut b: u8 = 0;
    let r = libc::recv(fd, &mut b as *mut u8 as *mut c_void, 1, 0);
    if r == 1 {
        Some(b)
    } else {
        if r == 0 {
            close_owned(hs);
        }
        None
    }
}

/// next chunk-size line (skips trailing CRLF + extensions). Some(0)=last chunk.
unsafe fn hs_next_chunk(hs: &mut HttpStream) -> Option<i64> {
    let mut b;
    loop {
        b = hs_getb(hs)?;
        if b != b'\r' && b != b'\n' {
            break;
        }
    }
    let mut sz: i64 = 0;
    let mut any = false;
    loop {
        let d = match b {
            b'0'..=b'9' => (b - b'0') as i64,
            b'a'..=b'f' => (b - b'a' + 10) as i64,
            b'A'..=b'F' => (b - b'A' + 10) as i64,
            _ => break,
        };
        sz = sz * 16 + d;
        any = true;
        match hs_getb(hs) {
            Some(x) => b = x,
            None => return if any { Some(sz) } else { None },
        }
    }
    while b != b'\n' {
        match hs_getb(hs) {
            Some(x) => b = x,
            None => break,
        }
    }
    if any { Some(sz) } else { None }
}

/// How long to wait for the TCP handshake. Without this the app inherits the kernel's SYN-retry
/// budget (~2 min on Linux), during which the 60fps SDL loop is fully blocked — an unreachable
/// PMS (box rebooting, TV on a different VLAN, DHCP re-lease) froze the whole UI rather than
/// failing. Every PMS request in a chain paid it again.
const CONNECT_TIMEOUT_MS: c_int = 2000;

/// `connect(2)` bounded by `timeout_ms`. Flips the socket to non-blocking for the handshake,
/// waits on `poll(POLLOUT)`, then reads `SO_ERROR` to learn the real outcome (a writable socket
/// does NOT mean success), and restores blocking mode so every read path below is unchanged.
/// Returns 0 on a connected socket, -1 otherwise. The caller owns closing `fd`.
unsafe fn connect_timeout(fd: c_int, sa: &libc::sockaddr_in, timeout_ms: c_int) -> c_int {
    let flags = libc::fcntl(fd, libc::F_GETFL, 0);
    if flags < 0 {
        return -1;
    }
    let restore = |ok: c_int| -> c_int {
        libc::fcntl(fd, libc::F_SETFL, flags);
        ok
    };
    if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
        return -1;
    }
    let len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    if libc::connect(fd, sa as *const _ as *const libc::sockaddr, len) == 0 {
        return restore(0); // connected immediately (loopback / same host)
    }
    if errno() != libc::EINPROGRESS {
        return restore(-1);
    }
    let mut pfd = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
    // EINTR must not be treated as a timeout: retry with the remaining budget.
    let mut left = timeout_ms;
    loop {
        let r = libc::poll(&mut pfd, 1, left);
        if r > 0 {
            break;
        }
        if r == 0 {
            return restore(-1); // timed out — the host is not answering
        }
        if errno() != libc::EINTR {
            return restore(-1);
        }
        left = 0; // a signal ate the wait; poll once more without blocking again
    }
    // Writable does not imply connected — SO_ERROR carries the verdict.
    let mut err: c_int = 0;
    let mut elen = std::mem::size_of::<c_int>() as libc::socklen_t;
    if libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_ERROR,
                        &mut err as *mut _ as *mut c_void, &mut elen) < 0 || err != 0 {
        return restore(-1);
    }
    restore(0)
}

pub(crate) fn http_open(hs: *mut HttpStream, ip: *const c_char, port: c_int,
                            path: *const c_char, extra: *const c_char, method: &str) -> c_int {
    if hs.is_null() || ip.is_null() || path.is_null() {
        return -1;
    }
    unsafe {
        std::ptr::write_bytes(hs as *mut u8, 0, std::mem::size_of::<HttpStream>()); // memset
        let hs = &mut *hs;
        hs.set_fd(-1);
        hs.content_length = -1;

        let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return -1;
        }
        let mut sa: libc::sockaddr_in = std::mem::zeroed();
        sa.sin_family = libc::AF_INET as libc::sa_family_t;
        sa.sin_port = (port as u16).to_be(); // htons
        // numeric dotted-quad -> in_addr (network byte order). No DNS, matching C.
        let ip_str = CStr::from_ptr(ip).to_string_lossy();
        let mut oct = [0u8; 4];
        let mut k = 0;
        for part in ip_str.split('.') {
            if k >= 4 { k = 5; break; }
            match part.parse::<u8>() {
                Ok(v) => { oct[k] = v; k += 1; }
                Err(_) => { k = 5; break; }
            }
        }
        if k != 4 {
            libc::close(fd);
            return -1;
        }
        sa.sin_addr.s_addr = u32::from_ne_bytes(oct); // memory bytes = [a,b,c,d]
        if connect_timeout(fd, &sa, CONNECT_TIMEOUT_MS) < 0 {
            libc::close(fd);
            return -1;
        }
        let one: c_int = 1;
        libc::setsockopt(fd, libc::IPPROTO_TCP, libc::TCP_NODELAY,
                         &one as *const _ as *const c_void, 4);
        // cap a stalled recv so teardown can't hang (matches the C's 15s SO_RCVTIMEO)
        let tv = libc::timeval { tv_sec: 15, tv_usec: 0 };
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVTIMEO,
                         &tv as *const _ as *const c_void,
                         std::mem::size_of::<libc::timeval>() as libc::socklen_t);
        // …and the send side, which had no bound at all: a peer that stops reading blocks the
        // request write for as long as its window stays shut.
        let stv = libc::timeval { tv_sec: 10, tv_usec: 0 };
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_SNDTIMEO,
                         &stv as *const _ as *const c_void,
                         std::mem::size_of::<libc::timeval>() as libc::socklen_t);

        // build + send the request (default Accept only if caller set none)
        let path_s = CStr::from_ptr(path).to_string_lossy();
        let ip_s = CStr::from_ptr(ip).to_string_lossy();
        let extra_s: String = if extra.is_null() {
            String::new()
        } else {
            CStr::from_ptr(extra).to_string_lossy().into_owned()
        };
        let accept = if extra_s.to_ascii_lowercase().contains("accept:") { "" } else { "Accept: */*\r\n" };
        let req = format!(
            "{method} {path_s} HTTP/1.1\r\nHost: {ip_s}:{port}\r\nUser-Agent: plxnative/0.1\r\n{accept}{extra_s}Connection: close\r\n\r\n"
        );
        let bytes = req.as_bytes();
        let mut off = 0usize;
        while off < bytes.len() {
            let w = libc::send(fd, bytes[off..].as_ptr() as *const c_void, bytes.len() - off, 0);
            if w <= 0 {
                libc::close(fd);
                return -1;
            }
            off += w as usize;
        }

        // read until end of headers (\r\n\r\n), keeping any body bytes that follow
        let cap = hs.buf.len();
        let mut hdr_end: Option<usize> = None;
        hs.blen = 0;
        while hdr_end.is_none() && (hs.blen as usize) < cap - 1 {
            let r = libc::recv(fd, hs.buf.as_mut_ptr().add(hs.blen as usize) as *mut c_void,
                               cap - hs.blen as usize, 0);
            if r <= 0 {
                libc::close(fd);
                return -1;
            }
            hs.blen += r as c_int;
            let blen = hs.blen as usize;
            let mut i = 3;
            while i < blen {
                if hs.buf[i - 3] == b'\r' && hs.buf[i - 2] == b'\n'
                    && hs.buf[i - 1] == b'\r' && hs.buf[i] == b'\n' {
                    hdr_end = Some(i + 1);
                    break;
                }
                i += 1;
            }
        }
        let hdr_end = match hdr_end {
            Some(e) => e,
            None => {
                libc::close(fd);
                return -1;
            }
        };

        // parse status line + Content-Length + chunked (headers are ASCII)
        let hdr = std::str::from_utf8(&hs.buf[..hdr_end]).unwrap_or("");
        if hdr.starts_with("HTTP/1.") {
            hs.status = hdr[9..].chars().take_while(|c| c.is_ascii_digit())
                .collect::<String>().parse().unwrap_or(0);
        }
        let lower = hdr.to_ascii_lowercase();
        if let Some(p) = lower.find("\r\ncontent-length:") {
            let v = &hdr[p + 17..];
            let num: String = v.trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
            hs.content_length = num.parse().unwrap_or(-1);
        }
        if lower.contains("\r\ntransfer-encoding: chunked") {
            hs.chunked = 1;
        }

        hs.set_fd(fd);
        hs.bpos = hdr_end as c_int; // first body byte
        if hs.status < 200 || hs.status >= 300 {
            close_owned(hs);
            return -1;
        }
        0
    }
}

pub(crate) fn http_read(hs: *mut HttpStream, dst: *mut c_uchar, n: c_int) -> c_int {
    if hs.is_null() || dst.is_null() || n <= 0 {
        return if n == 0 { 0 } else { -1 };
    }
    unsafe {
        let hs = &mut *hs;
        let n = n as usize;
        if hs.chunked != 0 {
            if hs.chunk_left <= 0 {
                match hs_next_chunk(hs) {
                    Some(cs) if cs > 0 => hs.chunk_left = cs,
                    _ => {
                        close_owned(hs);
                        return 0;
                    }
                }
            }
            let want = std::cmp::min(n as i64, hs.chunk_left) as usize;
            let mut got = 0usize;
            while got < want {
                if (hs.bpos as usize) < (hs.blen as usize) {
                    let avail = hs.blen as usize - hs.bpos as usize;
                    let take = std::cmp::min(want - got, avail);
                    std::ptr::copy_nonoverlapping(hs.buf.as_ptr().add(hs.bpos as usize), dst.add(got), take);
                    hs.bpos += take as c_int;
                    got += take;
                } else if hs.fd() >= 0 {
                    let r = libc::recv(hs.fd(), dst.add(got) as *mut c_void, want - got, 0);
                    if r < 0 {
                        if errno() == libc::EINTR { continue; }
                        break;
                    }
                    if r == 0 { close_owned(hs); break; }
                    got += r as usize;
                } else {
                    break;
                }
            }
            hs.chunk_left -= got as i64;
            hs.consumed += got as i64;
            return if got > 0 { got as c_int } else if hs.fd() < 0 { 0 } else { -1 };
        }
        if hs.fd() < 0 && (hs.bpos as usize) >= (hs.blen as usize) {
            return 0;
        }
        if hs.content_length >= 0 && hs.consumed >= hs.content_length {
            return 0;
        }
        // serve buffered body first
        if (hs.bpos as usize) < (hs.blen as usize) {
            let avail = hs.blen as usize - hs.bpos as usize;
            let take = std::cmp::min(avail, n);
            std::ptr::copy_nonoverlapping(hs.buf.as_ptr().add(hs.bpos as usize), dst, take);
            hs.bpos += take as c_int;
            hs.consumed += take as i64;
            return take as c_int;
        }
        if hs.fd() < 0 {
            return 0;
        }
        loop {
            let r = libc::recv(hs.fd(), dst as *mut c_void, n, 0);
            if r < 0 {
                if errno() == libc::EINTR { continue; }
                return -1;
            }
            if r == 0 { close_owned(hs); return 0; }
            hs.consumed += r as i64;
            return r as c_int;
        }
    }
}

/// Close the socket, once. Only the OWNING thread (the one doing the reads) may call this,
/// or the main thread AFTER that worker has been joined — a `close` racing a live reader frees
/// the fd number for another thread's `socket()` to claim. To interrupt a reader that is still
/// running, use [`http_shutdown`].
unsafe fn close_owned(hs: &HttpStream) {
    let fd = hs.take_fd();
    if fd >= 0 {
        libc::close(fd);
    }
}

pub(crate) fn http_close(hs: *mut HttpStream) {
    if hs.is_null() {
        return;
    }
    unsafe { close_owned(&*hs) }
}

/// Interrupt a read in progress WITHOUT closing: `shutdown(2)` wakes a peer blocked in `recv`
/// (which `close(2)` does not — that was the 15 s freeze on BACK during a stall) and leaves the
/// descriptor allocated, so its number cannot be recycled into another thread's socket while
/// the reader is still touching it. The reader then sees EOF and closes it itself.
pub(crate) fn http_shutdown(hs: *mut HttpStream) {
    if hs.is_null() {
        return;
    }
    unsafe {
        let fd = (*hs).fd();
        if fd >= 0 {
            libc::shutdown(fd, libc::SHUT_RDWR);
        }
    }
}

/// Rust-friendly one-shot GET: open -> read to end -> close. Used by pms.
/// (http_stream carries a 64KB buffer, so box it off the caller's stack.)
pub(crate) fn http_get(host: &str, port: c_int, path: &str, extra: Option<&str>) -> Option<Vec<u8>> {
    let host_c = std::ffi::CString::new(host).ok()?;
    let path_c = std::ffi::CString::new(path).ok()?;
    let extra_c = extra.and_then(|e| std::ffi::CString::new(e).ok());
    let extra_ptr = extra_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let mut hs: Box<HttpStream> = Box::new(unsafe { std::mem::zeroed() });
    if http_open(&mut *hs, host_c.as_ptr(), port, path_c.as_ptr(), extra_ptr, "GET") != 0 {
        return None;
    }
    let mut body = Vec::new();
    let mut chunk = vec![0u8; 65536];
    loop {
        let r = http_read(&mut *hs, chunk.as_mut_ptr(), chunk.len() as c_int);
        if r <= 0 {
            break;
        }
        body.extend_from_slice(&chunk[..r as usize]);
    }
    http_close(&mut *hs);
    Some(body)
}

/// Minimal HTTP PUT (no request body); returns the response status, or -1 on failure.
/// Used to SELECT a stream server-side — PUT /library/parts/{id}?allParts=1&audioStreamID=…
/// — because the transcoder encodes the part's *selected* audio, not a query-param one
/// (a GET on the same path does not change the selection; only PUT does).
pub(crate) fn http_put(host: &str, port: c_int, path: &str) -> c_int {
    let host_c = match std::ffi::CString::new(host) {
        Ok(c) => c,
        Err(_) => return -1,
    };
    let path_c = match std::ffi::CString::new(path) {
        Ok(c) => c,
        Err(_) => return -1,
    };
    let mut hs: Box<HttpStream> = Box::new(unsafe { std::mem::zeroed() });
    if http_open(&mut *hs, host_c.as_ptr(), port, path_c.as_ptr(), std::ptr::null(), "PUT") != 0 {
        return if hs.status != 0 { hs.status } else { -1 };
    }
    let status = hs.status;
    let mut chunk = vec![0u8; 4096];
    while http_read(&mut *hs, chunk.as_mut_ptr(), chunk.len() as c_int) > 0 {}
    http_close(&mut *hs);
    status
}

/// Minimal HTTP POST (no request body); returns the response body, or None on failure.
/// Used for POST /playQueues (parse the returned ids) and POST /:/timeline (body ignored) —
/// the Plex spec verb for both. Params ride the query string like the GET/PUT wrappers.
pub(crate) fn http_post(host: &str, port: c_int, path: &str, extra: Option<&str>) -> Option<Vec<u8>> {
    let host_c = std::ffi::CString::new(host).ok()?;
    let path_c = std::ffi::CString::new(path).ok()?;
    let extra_c = extra.and_then(|e| std::ffi::CString::new(e).ok());
    let extra_ptr = extra_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let mut hs: Box<HttpStream> = Box::new(unsafe { std::mem::zeroed() });
    if http_open(&mut *hs, host_c.as_ptr(), port, path_c.as_ptr(), extra_ptr, "POST") != 0 {
        return None;
    }
    let mut body = Vec::new();
    let mut chunk = vec![0u8; 65536];
    loop {
        let r = http_read(&mut *hs, chunk.as_mut_ptr(), chunk.len() as c_int);
        if r <= 0 {
            break;
        }
        body.extend_from_slice(&chunk[..r as usize]);
    }
    http_close(&mut *hs);
    Some(body)
}

/// A boxed HttpStream in the CLOSED state (fd = -1), so http_close is a no-op until
/// http_open assigns a real fd. The player engine pre-allocates the demux/cue sockets
/// before the worker threads open them; a plain zeroed box leaves fd = 0, and a
/// teardown before (or without) http_open would then close(0) the process's stdin —
/// and free fd 0 for a later socket() to reuse and be wrongly closed. (Box: 64KB.)
pub(crate) fn http_stream_boxed() -> Box<HttpStream> {
    // `set_fd` takes &self now (the field is atomic), so the binding no longer needs `mut`.
    let hs: Box<HttpStream> = Box::new(unsafe { std::mem::zeroed() });
    hs.set_fd(-1);
    hs
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn sockaddr(ip: [u8; 4], port: u16) -> libc::sockaddr_in {
        let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        sa.sin_family = libc::AF_INET as libc::sa_family_t;
        sa.sin_port = port.to_be();
        sa.sin_addr.s_addr = u32::from_ne_bytes(ip);
        sa
    }

    /// Regression: `connect(2)` was called blocking with no deadline, so an unreachable PMS
    /// froze the 60fps main loop for the kernel's SYN-retry budget (~2 min), once per request.
    /// 192.0.2.0/24 is TEST-NET-1 (RFC 5737) — guaranteed non-routable, so the handshake can
    /// never complete and the only thing that can end this call is the timeout.
    #[test]
    fn connect_to_a_black_hole_gives_up_on_the_deadline() {
        let sa = sockaddr([192, 0, 2, 1], 80);
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        assert!(fd >= 0);
        let t0 = Instant::now();
        let r = unsafe { connect_timeout(fd, &sa, 300) };
        let waited = t0.elapsed();
        unsafe { libc::close(fd) };
        assert_eq!(r, -1, "an unroutable host must fail, not connect");
        assert!(waited.as_millis() < 3_000, "took {waited:?} — the deadline is not being honoured");
    }

    /// A refused connection must be reported immediately, not waited out: port 1 on loopback
    /// has no listener, so the kernel answers RST within the first poll.
    #[test]
    fn a_refused_connection_fails_fast_and_is_not_reported_as_success() {
        let sa = sockaddr([127, 0, 0, 1], 1);
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        assert!(fd >= 0);
        let t0 = Instant::now();
        let r = unsafe { connect_timeout(fd, &sa, 5_000) };
        let waited = t0.elapsed();
        unsafe { libc::close(fd) };
        assert_eq!(r, -1, "SO_ERROR must be consulted — a writable socket is not a connected one");
        assert!(waited.as_millis() < 2_000, "a refusal should be immediate, waited {waited:?}");
    }

    /// The claim the whole single-closer protocol rests on: `shutdown(2)` wakes a peer that is
    /// already blocked in `recv`, which is what the interrupt sites need. (`close(2)` does not —
    /// that is why BACK during a stall used to wait out the 15 s SO_RCVTIMEO.) Two threads, a
    /// real loopback socket, no mocking.
    #[test]
    fn shutdown_wakes_a_reader_that_is_already_blocked_in_recv() {
        use std::sync::mpsc;
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().unwrap().port();
        let sa = sockaddr([127, 0, 0, 1], port);
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        assert_eq!(unsafe { connect_timeout(fd, &sa, 2_000) }, 0);
        let _peer = srv.accept().expect("accept"); // held open: nothing will ever be sent
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut b = [0u8; 16];
            // blocks here until the socket is shut down (the peer never writes)
            let r = unsafe { libc::recv(fd, b.as_mut_ptr() as *mut c_void, b.len(), 0) };
            let _ = tx.send(r);
        });
        // give the reader time to actually enter recv, then interrupt it
        std::thread::sleep(std::time::Duration::from_millis(150));
        unsafe { libc::shutdown(fd, libc::SHUT_RDWR) };
        let r = rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("shutdown did not wake the blocked recv");
        assert_eq!(r, 0, "a shut-down socket must report EOF");
        reader.join().unwrap();
        unsafe { libc::close(fd) };
    }

    /// `take_fd` is the single-closer gate: concurrent claimers must produce exactly one
    /// winner, so a descriptor can never be closed twice (and so never recycled underneath
    /// a thread still using it).
    #[test]
    fn exactly_one_caller_can_claim_the_fd() {
        let hs = http_stream_boxed();
        hs.set_fd(4242);
        let winners: i32 = std::thread::scope(|sc| {
            let hs = &hs;
            let hs2 = (0..8).map(|_| sc.spawn(move || i32::from(hs.take_fd() >= 0))).collect::<Vec<_>>();
            hs2.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(winners, 1, "the fd was claimed {winners} times — that is a double close");
        assert!(hs.fd() < 0, "the slot must be left closed");
    }

    /// The happy path still connects, and — the part that matters for every read below —
    /// the socket is handed back in BLOCKING mode.
    #[test]
    fn a_live_listener_connects_and_the_socket_is_left_blocking() {
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().unwrap().port();
        let sa = sockaddr([127, 0, 0, 1], port);
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        assert!(fd >= 0);
        let r = unsafe { connect_timeout(fd, &sa, 2_000) };
        assert_eq!(r, 0, "a listening socket must connect");
        let fl = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
        assert_eq!(fl & libc::O_NONBLOCK, 0, "O_NONBLOCK leaked out of the handshake");
        unsafe { libc::close(fd) };
    }
}
