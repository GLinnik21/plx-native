//! Shared fixtures and helpers for the `stream` test modules split out below.

use super::*;

/// One `GET /x` at a loopback port, through the door the control plane actually uses.
///
/// These assertions used to call `stream::http_get`, and they outlived it: what they grade —
/// that a chunked body is decoded on the READ path, and that a truncated one still reaches its
/// caller — is a property of `http_open`/`http_read`, not of the wrapper that wrapped them. Now
/// they grade it through `crate::http`'s plaintext arm, i.e. through the composition that runs
/// in production, which is strictly more than the wrapper could say.
pub(super) fn loopback_get(port: u16) -> Option<crate::http::Reply> {
    let o = crate::plex::Origin::http("127.0.0.1", port as i32);
    crate::http::request(&o, "/x", crate::http::Method::Get, &[], None)
}

/// Descriptors currently open in this process. `/dev/fd` works on both macOS and Linux;
/// `read_dir` opens one itself, but that is constant between two calls.
pub(super) fn open_fd_count() -> usize {
    std::fs::read_dir("/dev/fd").map(|d| d.count()).unwrap_or(0)
}

pub(super) fn sockaddr(ip: [u8; 4], port: u16) -> libc::sockaddr_in {
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    sa.sin_family = libc::AF_INET as libc::sa_family_t;
    sa.sin_port = port.to_be();
    sa.sin_addr.s_addr = u32::from_ne_bytes(ip);
    sa
}

pub(super) fn sockaddr6(ip: std::net::Ipv6Addr, port: u16) -> libc::sockaddr_in6 {
    let mut sa: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
    sa.sin6_family = libc::AF_INET6 as libc::sa_family_t;
    sa.sin6_port = port.to_be();
    sa.sin6_addr = libc::in6_addr {
        s6_addr: ip.octets(),
    };
    sa
}

/// `connect_timeout` for a v4 address. The real function takes the `(*const sockaddr,
/// socklen_t)` pair straight out of an `addrinfo`, because the address may now be either
/// family; the tests below predate that and say what they mean with a `sockaddr_in`.
pub(super) unsafe fn connect_v4(fd: c_int, sa: &libc::sockaddr_in, timeout_ms: c_int) -> c_int {
    connect_timeout(
        fd,
        sa as *const _ as *const libc::sockaddr,
        std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        timeout_ms,
    )
}

pub(super) unsafe fn connect_v6(fd: c_int, sa: &libc::sockaddr_in6, timeout_ms: c_int) -> c_int {
    connect_timeout(
        fd,
        sa as *const _ as *const libc::sockaddr,
        std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
        timeout_ms,
    )
}

/// One `addrinfo` node pointing at a caller-owned `sockaddr`, for testing [`connect_any`]'s
/// walk without a resolver in the loop. The chain a real DNS answer produces is not something a
/// test can arrange on demand — which address a name yields, and in what order, is the
/// machine's business — so the walk is graded on a list built by hand instead.
pub(super) fn ainfo(
    family: c_int,
    sa: *mut libc::sockaddr,
    len: libc::socklen_t,
    next: *mut libc::addrinfo,
) -> libc::addrinfo {
    let mut ai: libc::addrinfo = unsafe { std::mem::zeroed() };
    ai.ai_family = family;
    ai.ai_socktype = libc::SOCK_STREAM;
    ai.ai_addr = sa;
    ai.ai_addrlen = len;
    ai.ai_next = next;
    ai
}

/// Answer ONE request with `resp` verbatim, then close; hands back the bound port and the
/// server thread to join. `resp` is written as raw bytes precisely so a test can put things in
/// a header that no `&str` could hold. The listener moves into the thread, so the socket is
/// released once the response is out — which is also what gives the reader its EOF.
pub(super) fn one_shot_server(resp: Vec<u8>) -> (u16, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        if let Ok((mut s, _)) = srv.accept() {
            // Drain the request so our send() completes; it arrives in one write.
            let mut req = [0u8; 2048];
            let _ = s.read(&mut req);
            let _ = s.write_all(&resp);
        }
    });
    (port, h)
}

/// `http_open` a GET against a loopback port, handing back the stream AND its verdict.
pub(super) fn open_against(port: u16) -> (Box<HttpStream>, c_int) {
    open_host_against("127.0.0.1", port)
}

/// …and the same for a host that is not the v4 loopback literal — a name, a bracketed v6
/// literal, a bare one.
pub(super) fn open_host_against(host: &str, port: u16) -> (Box<HttpStream>, c_int) {
    let h = std::ffi::CString::new(host).unwrap();
    let path = std::ffi::CString::new("/x").unwrap();
    let mut hs = http_stream_boxed();
    let rv = http_open(
        &mut *hs,
        h.as_ptr(),
        port as c_int,
        path.as_ptr(),
        std::ptr::null(),
        "GET",
    );
    (hs, rv)
}

/// A one-shot server that hands the REQUEST back to the test as well as answering it — the
/// only way to grade a header we emit rather than one we parse.
pub(super) fn one_shot_echo(
    bind: &str,
    resp: Vec<u8>,
) -> std::io::Result<(u16, std::thread::JoinHandle<Vec<u8>>)> {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind(bind)?;
    let port = srv.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        let mut req = Vec::new();
        if let Ok((mut sk, _)) = srv.accept() {
            let mut b = [0u8; 2048];
            if let Ok(n) = sk.read(&mut b) {
                req.extend_from_slice(&b[..n]);
            }
            let _ = sk.write_all(&resp);
        }
        req
    });
    Ok((port, h))
}

/// The `Host:` line of a captured request, without its CRLF.
pub(super) fn host_line(req: &[u8]) -> String {
    let text = String::from_utf8_lossy(req);
    text.split("\r\n")
        .find(|l| l.to_ascii_lowercase().starts_with("host:"))
        .unwrap_or("<no Host header>")
        .to_string()
}

/// Can this machine use the IPv6 loopback at all? A container or a set with IPv6 compiled out
/// cannot, and the v6 cases below are then not failing — they are unrunnable. Say so on the
/// output rather than passing quietly, because a test that reports success having never opened
/// an AF_INET6 socket is exactly the false green this file's own notes warn about.
pub(super) fn v6_loopback_or_skip(what: &str) -> Option<std::net::TcpListener> {
    match std::net::TcpListener::bind("[::1]:0") {
        Ok(l) => Some(l),
        Err(e) => {
            eprintln!("SKIPPED {what}: this host has no usable IPv6 loopback ({e})");
            None
        }
    }
}

pub(super) fn drain_body(hs: &mut HttpStream) {
    let mut buf = [0u8; 64];
    loop {
        let n = http_read(hs, buf.as_mut_ptr(), buf.len() as c_int);
        if n <= 0 {
            break;
        }
    }
}

pub(super) fn with_keepalive_listener(
    body: impl FnOnce(u16, &std::sync::atomic::AtomicUsize, &std::sync::atomic::AtomicUsize),
    reply: &'static [u8],
) {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    srv.set_nonblocking(true).expect("set_nonblocking");
    let accepts = AtomicUsize::new(0);
    let requests = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    std::thread::scope(|sc| {
        sc.spawn(|| {
            while !stop.load(Ordering::Acquire) {
                match srv.accept() {
                    Ok((s, _)) => {
                        accepts.fetch_add(1, Ordering::AcqRel);
                        let (rq, st) = (&requests, &stop);
                        sc.spawn(move || {
                            let _ =
                                s.set_read_timeout(Some(std::time::Duration::from_millis(200)));
                            let mut w = match s.try_clone() {
                                Ok(c) => c,
                                Err(_) => return,
                            };
                            let mut buf: Vec<u8> = Vec::new();
                            loop {
                                if let Some(k) = buf.windows(4).position(|x| x == b"\r\n\r\n") {
                                    buf.drain(..k + 4);
                                    rq.fetch_add(1, Ordering::AcqRel);
                                    if w.write_all(reply).is_err() {
                                        return;
                                    }
                                    let _ = w.flush();
                                    continue;
                                }
                                let mut tmp = [0u8; 1024];
                                match (&s).read(&mut tmp) {
                                    Ok(0) => return,
                                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                                    Err(e)
                                        if e.kind() == std::io::ErrorKind::WouldBlock
                                            || e.kind() == std::io::ErrorKind::TimedOut =>
                                    {
                                        if st.load(Ordering::Acquire) {
                                            return;
                                        }
                                        continue;
                                    }
                                    Err(_) => return,
                                }
                            }
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            }
        });
        struct StopAll<'a>(&'a AtomicBool);
        impl Drop for StopAll<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let _stop_on_exit = StopAll(&stop);
        body(port, &accepts, &requests);
        stop.store(true, Ordering::Release);
    });
}
