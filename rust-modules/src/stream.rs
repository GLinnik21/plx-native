//! Rust port of src/stream.c — blocking HTTP/1.1 GET over a raw TCP socket.
//! Same C ABI (stream.h): the C callers (posters/pms/playback) allocate an
//! `http_stream` and pass `&hs`; this operates on it in place via a repr(C)
//! mirror. Header/chunk parsing is bounds-checked (no OOB), unlike the C.
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uchar, c_void};

// Layout MUST match `http_stream` in src/stream.h (repr(C) follows the C ABI).
#[repr(C)]
pub struct HttpStream {
    fd: c_int,
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

/// one raw body byte (buffered first, then socket) — for chunk framing
unsafe fn hs_getb(hs: &mut HttpStream) -> Option<u8> {
    if (hs.bpos as usize) < (hs.blen as usize) {
        let b = hs.buf[hs.bpos as usize];
        hs.bpos += 1;
        return Some(b);
    }
    if hs.fd < 0 {
        return None;
    }
    let mut b: u8 = 0;
    let r = libc::recv(hs.fd, &mut b as *mut u8 as *mut c_void, 1, 0);
    if r == 1 {
        Some(b)
    } else {
        if r == 0 {
            libc::close(hs.fd);
            hs.fd = -1;
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

#[no_mangle]
pub extern "C" fn http_open(hs: *mut HttpStream, ip: *const c_char, port: c_int,
                            path: *const c_char, extra: *const c_char) -> c_int {
    if hs.is_null() || ip.is_null() || path.is_null() {
        return -1;
    }
    unsafe {
        std::ptr::write_bytes(hs as *mut u8, 0, std::mem::size_of::<HttpStream>()); // memset
        let hs = &mut *hs;
        hs.fd = -1;
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
        if libc::connect(fd, &sa as *const _ as *const libc::sockaddr,
                         std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t) < 0 {
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
            "GET {path_s} HTTP/1.1\r\nHost: {ip_s}:{port}\r\nUser-Agent: plexpoc/0.1\r\n{accept}{extra_s}Connection: close\r\n\r\n"
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

        hs.fd = fd;
        hs.bpos = hdr_end as c_int; // first body byte
        if hs.status < 200 || hs.status >= 300 {
            libc::close(fd);
            hs.fd = -1;
            return -1;
        }
        0
    }
}

#[no_mangle]
pub extern "C" fn http_read(hs: *mut HttpStream, dst: *mut c_uchar, n: c_int) -> c_int {
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
                        if hs.fd >= 0 { libc::close(hs.fd); hs.fd = -1; }
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
                } else if hs.fd >= 0 {
                    let r = libc::recv(hs.fd, dst.add(got) as *mut c_void, want - got, 0);
                    if r < 0 {
                        if errno() == libc::EINTR { continue; }
                        break;
                    }
                    if r == 0 { libc::close(hs.fd); hs.fd = -1; break; }
                    got += r as usize;
                } else {
                    break;
                }
            }
            hs.chunk_left -= got as i64;
            hs.consumed += got as i64;
            return if got > 0 { got as c_int } else if hs.fd < 0 { 0 } else { -1 };
        }
        if hs.fd < 0 && (hs.bpos as usize) >= (hs.blen as usize) {
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
        if hs.fd < 0 {
            return 0;
        }
        loop {
            let r = libc::recv(hs.fd, dst as *mut c_void, n, 0);
            if r < 0 {
                if errno() == libc::EINTR { continue; }
                return -1;
            }
            if r == 0 { libc::close(hs.fd); hs.fd = -1; return 0; }
            hs.consumed += r as i64;
            return r as c_int;
        }
    }
}

#[no_mangle]
pub extern "C" fn http_close(hs: *mut HttpStream) {
    if hs.is_null() {
        return;
    }
    unsafe {
        let hs = &mut *hs;
        if hs.fd >= 0 {
            libc::close(hs.fd);
            hs.fd = -1;
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
    if http_open(&mut *hs, host_c.as_ptr(), port, path_c.as_ptr(), extra_ptr) != 0 {
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
