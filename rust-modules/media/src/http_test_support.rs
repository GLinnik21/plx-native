//! Loopback HTTP fixtures shared by feed and subtitle tests.

use std::sync::atomic::{AtomicBool, Ordering};

/// A loopback PMS stand-in that COUNTS both accepted connections and requests served — the
/// observables that matter, since "did the callback go back to the server" is the whole
/// question and no return value can answer it.
///
/// **Why two counters.** Sequential HLS/media GETs now reuse a keep-alive fd, so a new
/// request is no longer a new connection. The accept count still answers "did we dial",
/// which is what teardown and Range-seek tests care about (`seek_cb` still `http_close`s
/// first). libcurl keeps the connection in its multi handle's cache; HLS HTTPS now holds
/// that multi across playlist and segment URLs. The request count is what moves when reuse
/// works.
///
/// Each connection gets its own handler thread and is served keep-alive; the accept count is
/// bumped BEFORE the reply is written, so it is already final by the time any `http_open`
/// against this listener can return — every assertion below is causally ordered behind that,
/// and needs no sleep and no timing margin.
pub(crate) fn with_counting_listener(
    body: impl FnOnce(u16, &std::sync::atomic::AtomicUsize, &std::sync::atomic::AtomicUsize),
) {
    use std::io::{Read, Write};
    use std::sync::atomic::AtomicUsize;
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    srv.set_nonblocking(true).expect("set_nonblocking"); // so the acceptor can be stopped
    let accepts = AtomicUsize::new(0);
    let requests = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    std::thread::scope(|sc| {
        sc.spawn(|| {
            while !stop.load(Ordering::Acquire) {
                match plx_base::testnet::accept(&srv) {
                    Ok((s, _)) => {
                        accepts.fetch_add(1, Ordering::AcqRel);
                        let (rq, st) = (&requests, &stop);
                        sc.spawn(move || {
                            // Read each request head, count it, answer 200 with an 8-byte
                            // body — small enough that it arrives inside the client's header
                            // read, so `http_read` serves it from `HttpStream`'s buffer and
                            // never needs the socket again.
                            let _ =
                                s.set_read_timeout(Some(std::time::Duration::from_millis(100)));
                            let mut w = match s.try_clone() {
                                Ok(c) => c,
                                Err(_) => return,
                            };
                            let mut buf: Vec<u8> = Vec::new();
                            loop {
                                if let Some(k) = buf.windows(4).position(|x| x == b"\r\n\r\n") {
                                    buf.drain(..k + 4);
                                    rq.fetch_add(1, Ordering::AcqRel);
                                    if w.write_all(
                                        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCDEFGH",
                                    )
                                    .is_err()
                                    {
                                        return;
                                    }
                                    let _ = w.flush();
                                    continue;
                                }
                                let mut tmp = [0u8; 1024];
                                match (&s).read(&mut tmp) {
                                    Ok(0) => return,
                                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                                    Err(ref e)
                                        if matches!(
                                            e.kind(),
                                            std::io::ErrorKind::WouldBlock
                                                | std::io::ErrorKind::TimedOut
                                        ) =>
                                    {
                                        if st.load(Ordering::Acquire) {
                                            return;
                                        }
                                    }
                                    Err(_) => return,
                                }
                            }
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(1))
                    }
                    Err(_) => break,
                }
            }
        });
        // Stop everything on the way out however we leave. A FAILING assertion in `body`
        // unwinds through here and `scope` joins before it reports, so a flag set only on the
        // success path would turn every real failure into a hang instead of a message.
        struct StopAcceptor<'a>(&'a AtomicBool);
        impl Drop for StopAcceptor<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let _stop_on_exit = StopAcceptor(&stop);
        body(port, &accepts, &requests);
    });
}

/// Serve media bytes over HTTP with Range support for the native subtitle readers.
pub(crate) fn serve_file(body: Vec<u8>, stop: &'static AtomicBool) -> (u16, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = srv.local_addr().unwrap().port();
    srv.set_nonblocking(true).unwrap();
    let h = std::thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            let Ok((mut s, _)) = plx_base::testnet::accept(&srv) else {
                std::thread::yield_now();
                continue;
            };
            let body = body.clone();
            std::thread::spawn(move || {
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let Some(k) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        match s.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => {
                                buf.extend_from_slice(&chunk[..n]);
                                continue;
                            }
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..k]).to_ascii_lowercase();
                    buf.drain(..k + 4);
                    let from = head
                        .lines()
                        .find_map(|l| l.strip_prefix("range: bytes="))
                        .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok());
                    let reply = match from {
                        Some(n) if n < body.len() => format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {n}-{}/{}\r\n\r\n",
                            body.len() - n,
                            body.len() - 1,
                            body.len()
                        ),
                        _ => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()),
                    };
                    let n = from.filter(|n| *n < body.len()).unwrap_or(0);
                    if s.write_all(reply.as_bytes()).is_err() || s.write_all(&body[n..]).is_err() {
                        return;
                    }
                }
            });
        }
    });
    (port, h)
}
