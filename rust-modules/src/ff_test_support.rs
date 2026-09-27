//! Shared fixtures and helpers for the `ff` test modules split out below.

use super::*;

/// Build a length-prefixed (AVCC-style) packet: 4-byte big-endian length + payload, repeated.
pub(super) fn avcc(nals: &[&[u8]]) -> Vec<u8> {
    let mut v = Vec::new();
    for n in nals {
        v.extend_from_slice(&(n.len() as u32).to_be_bytes());
        v.extend_from_slice(n);
    }
    v
}

pub(super) fn to_annexb(buf: &[u8], is_hevc: bool, param: &[u8]) -> (bool, Vec<u8>) {
    let mut out = Vec::new();
    let key = unsafe { packet_to_annexb(buf.as_ptr(), buf.len(), 4, is_hevc, param, &mut out) };
    (key, out)
}

// -- parse_dovi_conf: the Dolby Vision configuration record ---------------------------
//
// The record is nine plain bytes, so these fixtures ARE the wire format — no builder, no
// FFmpeg. That is the point: `dovi_conf`'s pointer walk cannot be host-tested (dlopen returns
// None on Darwin, so a test naming it would pass without executing one line of it), but the
// parse is where a field could be transposed, and the parse is pure.

/// The nine bytes as the header lays them out, so a fixture reads like the spec table.
pub(super) fn dovi_bytes(profile: u8, level: u8, rpu: u8, el: u8, bl: u8, compat: u8) -> Vec<u8> {
    vec![1, 0, profile, level, rpu, el, bl, compat, 0]
}

// -- the AVIO callbacks under teardown -------------------------------------------------
//
// These drive `read_cb`/`seek_cb` directly, which works on the host precisely because neither
// one touches FFmpeg: they are plain `extern "C"` fns over an `AvioState`, whose every field
// (an HttpStream, an AuQueue, two CStrings, three integers) is ordinary Rust. So long as a
// test stays off `av_*`, the callbacks link and run here exactly as they do on the TV.

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
pub(super) fn with_counting_listener(
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
                match crate::testnet::accept(&srv) {
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

/// An open stream plus the aborted video lane behind its AVIO — the state teardown leaves:
/// `engine::teardown` aborts both lanes, `http_shutdown`s this socket, then joins the demuxer.
pub(super) fn opened_stream_with_aborted_lane(
    port: u16,
) -> (Box<HttpStream>, Box<AuQueue>, CString, CString) {
    let ip = CString::new("127.0.0.1").unwrap();
    let path = CString::new("/library/parts/1/file.mkv").unwrap();
    let mut hs = crate::stream::http_stream_boxed();
    let rv = crate::stream::http_open(
        &mut *hs,
        ip.as_ptr(),
        port as c_int,
        path.as_ptr(),
        std::ptr::null(),
        "GET",
    );
    assert_eq!(rv, 0, "fixture: the first open must succeed");
    let mut aq = crate::aq::aq_new(1 << 20);
    crate::aq::aq_abort(&mut *aq);
    (hs, aq, ip, path)
}

// -- the same two invariants, with libcurl under the AVIO instead of a socket ------------
//
// The guards above live in `read_cb`/`seek_cb`, ABOVE the dispatch, so they are transport
// independent by construction — which is exactly the kind of claim that stops being true the
// first time somebody moves a check into a branch. These pin it. The listener speaks plain
// HTTP and curl speaks plain HTTP, so no TLS is needed to grade the abort path; what a host
// cannot reach is a real handshake, which is why the PR carries a device recipe for aborting
// during DNS and during TLS instead of pretending these cover it.

/// libcurl bound and both tables live, with the crate-wide lock HELD for the caller's whole
/// test — `curlio`'s one-source registry is a process-global these two contend on with
/// `curlio`'s own suite, in another module, which is exactly what `testlock` is for. `None`
/// on a host with no libcurl at all, where these two would be grading nothing.
pub(super) fn curl_gate() -> Option<crate::testlock::Serial> {
    let g = crate::testlock::serial();
    if crate::net::global_init() && crate::curlio::available() {
        Some(g)
    } else {
        None
    }
}

/// The curl twin of `opened_stream_with_aborted_lane`: a live https-capable source over the
/// counting listener, plus the aborted video lane teardown leaves behind.
pub(super) fn opened_curl_with_aborted_lane(port: u16) -> (Box<crate::curlio::CurlSource>, Box<AuQueue>) {
    let cs = crate::curlio::CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0)
        .expect("fixture: the first open must succeed");
    let mut aq = crate::aq::aq_new(1 << 20);
    crate::aq::aq_abort(&mut *aq);
    (cs, aq)
}
