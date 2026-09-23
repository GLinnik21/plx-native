//! Blocking HTTP/1.1 over a raw TCP socket (was src/stream.c). Callers
//! (posters/pms/player) allocate an `HttpStream` and pass `&hs`; this operates on it
//! in place. Header/chunk parsing is bounds-checked (no OOB).
//!
//! The host is a **name or an address literal of either family**, resolved through `getaddrinfo`
//! ([`resolve`]) and dialled down the whole returned chain ([`connect_any_result`]). It used to be
//! four decimal octets parsed by hand into a `sockaddr_in`, which made a hostname and every IPv6
//! server not "degraded" but impossible — the shape of the gap LG's checklist #43 CASE2 asks about.
//! What is still missing here is TLS: this arm stays cleartext. [`crate::http`] sends an `https://`
//! control-plane origin through [`crate::net`], while MEDIA bytes use [`crate::curlio`], a
//! libcurl-multi pull source under the same `ff.rs` AVIO that this module serves for `http`.
//!
//! Failures the legacy `0`/`-1` return cannot carry are reported to the event log instead: a
//! non-2xx response (where the code is known and the socket is about to close) and a body that came
//! up short of its `Content-Length` ([`short_body_line`]). [`http_open_until_result`] additionally
//! preserves the setup failure's typed cause for deadline-aware callers. A truncated body is still
//! handed to its caller, and no line built here carries a query string, which is [`log_endpoint`]'s
//! rule and the reason it exists.
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uchar, c_void};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use crate::checkpoint::{Checkpoint, NoCheckpoint, Pacer};

static FD_GATE: Mutex<()> = Mutex::new(());

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
    /// "A teardown asked this stream to stop" — set by [`http_shutdown`], never cleared by
    /// [`http_open`].
    ///
    /// It exists because resolving gave the open something it never had: a NEXT address to try
    /// after a failed `connect`. A handshake aborted by `shutdown(2)` and an address that is simply
    /// dead are the same value at `connect`'s return, so without this latch a teardown fired during
    /// attempt 1 of 2 is answered by dialling attempt 2 — the interrupt silently consumed, and a
    /// brand-new connection handed back to a caller that was being torn down. Reading the fd cannot
    /// stand in for it: between two attempts the fd is legitimately -1. Production boxes a fresh
    /// [`HttpStream`] per engine, so a leftover latch is not a later session to consume; clearing
    /// it at open let a shutdown that landed after the caller's AU-abort check connect under join.
    interrupted: AtomicI32,
    buf: [u8; 65536],
    blen: c_int,
    bpos: c_int,
    content_length: i64,
    consumed: i64,
    status: c_int,
    chunked: c_int,
    chunk_left: i64,
    /// 1 if this response allows the next [`http_open`] to reuse the live fd. HTTP/1.1
    /// defaults to keep-alive unless the server sent `Connection: close`.
    keep_alive: c_int,
    /// 1 once the body has been fully consumed and the fd is still open. A second open
    /// may reuse only when this is set — an unread body would poison the next request.
    body_done: c_int,
    /// 1 once the current chunked-trailer line has seen a non-CR byte. Drain can return
    /// `HTTP_READ_DEADLINE` mid-trailer and resume on a later call; a stack `line_empty = true`
    /// on every entry treated the first `\n` of `\r\n\r\n` as the terminator and left leftover
    /// bytes that forced a redial. Zeroed default is "empty so far", which `http_stream_boxed`
    /// already writes.
    trailer_line_has_content: c_int,
    peer_port: c_int,
    peer_host_len: c_int,
    peer_host: [u8; 256],
}

fn errno() -> c_int {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn retry_interrupted_recv(result: isize, error: c_int) -> bool {
    result != HTTP_READ_DEADLINE as isize
        && result != HTTP_READ_STOPPED as isize
        && result < 0
        && error == libc::EINTR
}

/// Case-insensitive search for an ASCII `needle` in a byte haystack — the header-block lookup
/// that used to be `hdr.to_ascii_lowercase().find(…)`. Header field names are case-insensitive
/// (RFC 9110 §5.1), so the search has to be; doing it in place drops the 64 KB-worst-case
/// `String` the lowercase copy allocated on every single request, and — the point of the rewrite
/// — needs no UTF-8 in the first place. Returns the offset into `hay`, which is an offset into
/// the ORIGINAL bytes (an ASCII-case fold cannot change any byte's length, but nothing here
/// relies on that any more).
fn find_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
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
    /// Has a teardown asked this open to stop? See the field.
    #[inline]
    fn interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Acquire) != 0
    }
    fn body_is_done(&self) -> bool {
        self.body_done != 0
    }
    /// Reset every per-request field EXCEPT `fd` and `interrupted`. `http_open` used to
    /// `write_bytes`-memset the whole struct, which wrote the ATOMIC fd non-atomically — and
    /// momentarily as 0, i.e. stdin — while another thread could be loading it in
    /// `http_shutdown`. `fd` is reset separately through its atomic store.
    ///
    /// `buf` is deliberately NOT cleared: it is only ever read within `[bpos, blen)`, both of
    /// which are reset here, so zeroing 64 KB on every request was pure cost.
    ///
    /// `interrupted` is not touched. Clearing it here lost a teardown that landed between the
    /// load and the store, so a Transport redial could dial a socket the already-fired shutdown
    /// cannot reach. The latch stays set for the life of this box; a later engine session is a
    /// fresh [`HttpStream`].
    #[inline]
    fn reset_request_fields(&mut self) {
        self.blen = 0;
        self.bpos = 0;
        self.content_length = -1;
        self.consumed = 0;
        self.status = 0;
        self.chunked = 0;
        self.chunk_left = 0;
        self.keep_alive = 0;
        self.body_done = 0;
        self.trailer_line_has_content = 0;
    }
}

/// The part of a request path that may be written to the event log: everything before the query.
///
/// Paths reaching this module routinely carry the PMS token in their query string —
/// `plex::client::with_token` is the data layer's one token choke point and appends
/// `X-Plex-Token=…` to what it is given, and the poster store's paths arrive with one already in
/// them (`Client::fetch_built`, where the built path *is* the LRU key, so the key and the request
/// have to be the same bytes). The event log is this app's support channel: a user is asked to paste
/// `/tmp/plxnative-events.log` into a public issue thread, so a line carrying a query string is a
/// credential leak rather than a possible one — the same rule, for the same reason, that
/// `app::diagnostics` applies to the diagnostics panel. The endpoint on its own is what makes a line
/// diagnosable ("which request failed") and it carries no secret.
///
/// `crate::redact_tokens` catches a line that gets this wrong on the way out; the policy is that
/// nothing built here needs it.
fn log_endpoint(path: &str) -> &str {
    match path.find('?') {
        Some(q) => &path[..q],
        None => path,
    }
}

/// The event-log line for a response body that came up short, or `None` when there is nothing to
/// report. Pure, so the decision can be graded on the host without a socket.
///
/// [`http_read`] reports a clean end as 0 and a recv ERROR as -1 (a mid-body `SO_RCVTIMEO` firing,
/// a reset), and the one-shot wrappers below hand back `Some(body)` for both — deliberately, since
/// changing that would move every caller's behaviour. What the caller sees instead is a body short
/// by however much never arrived: for `plex::client::get_json` that is a `serde_json` parse
/// failure `.ok()`-folded to `None`, which is the same value a server that never answered
/// produces. This line is the difference between those two.
///
/// `sized` is the whole subtlety, and it is why a chunked response cannot report a short body on
/// length. Chunked framing carries no `Content-Length` to fall short of — the sizes are in the
/// body — and [`http_read`]'s chunked branch never consults the field, counting DECODED bytes into
/// `consumed`. A server that sent both headers anyway would therefore have the test comparing two
/// different quantities, so chunked is excluded outright rather than left to rely on
/// `content_length` having stayed -1 (RFC 9112 §6: with both present the chunked framing wins and
/// the length is ignored, which is what the read path already does). A close-delimited body — no
/// length, not chunked — has no completeness test at all, so there only a recv error can say the
/// transfer ended early, and `want` says plainly that nothing knows how much was owed.
fn short_body_line(
    method: &str,
    path: &str,
    consumed: i64,
    content_length: i64,
    chunked: bool,
    recv_err: bool,
) -> Option<String> {
    let sized = !chunked && content_length >= 0;
    if sized && consumed >= content_length {
        return None; // whole, by the only measure the response gave us
    }
    if !sized && !recv_err {
        return None; // nothing to fall short of, and the socket ended cleanly
    }
    let want = if sized {
        content_length.to_string()
    } else {
        "?".to_string()
    };
    let why = if recv_err { "recv error" } else { "EOF" };
    Some(format!(
        "stream: {method} {} SHORT BODY got={consumed} want={want} ({why})",
        log_endpoint(path)
    ))
}

/// [`short_body_line`] applied to a finished stream — call it after the read loop, before or after
/// [`http_close`] (the fields it reads are counters, which `http_close` does not touch).
///
/// `pub(crate)` because the one-shot wrappers that used to call it are gone. They folded away half
/// of every answer — `http_get`/`http_post` dropped the status, `http_put` dropped the body — and
/// the control plane needs both (a `401` is a token problem and a refusal is a reachability one;
/// `plex::probe::Outcome` exists to keep those apart). Their replacement composes this module's
/// primitives directly: [`crate::http`]'s plaintext arm, which is now this function's only caller
/// and the reason it did not go with them. The three fields it reads are private, so the notice
/// could not have been reproduced from outside.
pub(crate) fn note_short_body(method: &str, path: &str, hs: &HttpStream, recv_err: bool) {
    if let Some(line) = short_body_line(
        method,
        path,
        hs.consumed,
        hs.content_length,
        hs.chunked != 0,
        recv_err,
    ) {
        crate::log(&line);
    }
}

/// crate-internal accessors (fields are private) — the player engine reads these.
#[inline]
pub(crate) fn hs_content_length(hs: *const HttpStream) -> i64 {
    unsafe { (*hs).content_length }
}
#[inline]
pub(crate) fn hs_status(hs: *const HttpStream) -> c_int {
    unsafe { (*hs).status }
}

/// Internal read result reserved for a caller-owned wall-clock deadline. Ordinary callers never
/// see it: [`http_read`] has no deadline and retains its historical `-1` error result.
pub(crate) const HTTP_READ_DEADLINE: c_int = -2;

/// Internal read result for a caller whose [`Checkpoint`] answered
/// [`Flow::Stop`](crate::checkpoint::Flow::Stop). Distinct from `-1` and [`HTTP_READ_DEADLINE`]: it is neither a
/// transport failure nor a clock this module owns, so nothing here redials or closes on it.
pub(crate) const HTTP_READ_STOPPED: c_int = -3;

/// What ended a [`wait_fd`].
enum FdWait {
    Ready,
    /// The caller's absolute deadline passed.
    Deadline,
    /// The socket's own inactivity option elapsed while a checkpoint slice was doing the waiting.
    Idle,
    Stopped,
    Error,
}

/// The socket option `opt` (`SO_RCVTIMEO`/`SO_SNDTIMEO`) as a duration; `None` when unset (zero)
/// or unreadable, which is what a blocking `recv` treats as "wait forever" too.
unsafe fn socket_timeout(fd: c_int, opt: c_int) -> Option<std::time::Duration> {
    let mut tv: libc::timeval = std::mem::zeroed();
    let mut len = std::mem::size_of::<libc::timeval>() as libc::socklen_t;
    if libc::getsockopt(
        fd,
        libc::SOL_SOCKET,
        opt,
        &mut tv as *mut _ as *mut c_void,
        &mut len,
    ) < 0
    {
        return None;
    }
    let d = std::time::Duration::from_secs(tv.tv_sec.max(0) as u64)
        + std::time::Duration::from_micros(tv.tv_usec.max(0) as u64);
    (!d.is_zero()).then_some(d)
}

/// When a checkpoint slice replaces a plain blocking call, `poll` no longer sees the socket's own
/// inactivity option, so it is carried as an absolute bound from the start of the call — the same
/// span that option would have granted the blocking call it replaces. Only needed without a caller
/// deadline: with one, the call was already a `poll` loop that ignores the option.
unsafe fn idle_bound(fd: c_int, opt: c_int, deadline: Option<Instant>) -> Option<Instant> {
    if deadline.is_some() {
        return None;
    }
    socket_timeout(fd, opt).and_then(|d| Instant::now().checked_add(d))
}

/// `poll` `fd` for `events` until it is ready, `deadline` or `idle_at` passes, or the caller's
/// checkpoint stops the wait. A checkpoint slice's expiry only re-asks the checkpoint and keeps
/// waiting; it is not a timeout, and it does not move `deadline` or `idle_at`.
unsafe fn wait_fd(
    fd: c_int,
    events: libc::c_short,
    deadline: Option<Instant>,
    idle_at: Option<Instant>,
    pacer: &mut Pacer,
) -> FdWait {
    #[derive(Clone, Copy, PartialEq)]
    enum Bound {
        Deadline,
        Idle,
        Recheck,
    }
    loop {
        if deadline.is_some_and(|at| Instant::now() >= at) {
            return FdWait::Deadline;
        }
        let Ok(slice) = pacer.before_wait() else {
            return FdWait::Stopped;
        };
        // Earliest bound wins; on a tie the caller's deadline, then inactivity, then a recheck.
        let mut bound: Option<(Instant, Bound)> = None;
        for (at, kind) in [
            (deadline, Bound::Deadline),
            (idle_at, Bound::Idle),
            (slice, Bound::Recheck),
        ] {
            if let Some(at) = at {
                if bound.is_none_or(|(b, _)| at < b) {
                    bound = Some((at, kind));
                }
            }
        }
        let timeout_ms = match bound {
            None => -1,
            Some((at, kind)) => {
                if Instant::now() >= at {
                    match kind {
                        Bound::Deadline => return FdWait::Deadline,
                        Bound::Idle => return FdWait::Idle,
                        Bound::Recheck => {}
                    }
                }
                crate::checkpoint::wait_ms_until(at, c_int::MAX)
            }
        };
        let mut pfd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let ready = libc::poll(&mut pfd, 1, timeout_ms);
        if ready > 0 {
            return FdWait::Ready;
        }
        if ready < 0 && errno() != libc::EINTR {
            return FdWait::Error;
        }
        // A timeout or EINTR: the top of the loop decides which bound, if any, has passed.
    }
}

/// `recv(2)` with an optional absolute wall-clock ceiling. A relative socket timeout is not
/// enough for an ABR candidate: every successful dribble resets `SO_RCVTIMEO`, so a transfer that
/// can no longer meet its segment-production budget could still monopolize the demux thread
/// forever. Polling against the original deadline makes progress consume the budget rather than
/// renew it.
///
/// Bytes the kernel has already queued were RECEIVED before this call, and are handed over
/// without asking anyone — the same standing `http_read_until` gives its header buffer. Only then
/// is the checkpoint asked, before blocking: asking first let a hold stop a transfer whose
/// remainder had already arrived. Unarmed with no deadline, the wait is the plain blocking `recv`
/// it always was; otherwise it is sliced at its `next_check`.
unsafe fn recv_until(
    fd: c_int,
    dst: *mut c_void,
    n: usize,
    deadline: Option<Instant>,
    pacer: &mut Pacer,
) -> isize {
    let queued = libc::recv(fd, dst, n, libc::MSG_DONTWAIT);
    if queued >= 0 {
        return queued;
    }
    let e = errno();
    if e != libc::EAGAIN && e != libc::EWOULDBLOCK && e != libc::EINTR {
        return queued;
    }
    if deadline.is_none() {
        match pacer.before_wait() {
            Err(_) => return HTTP_READ_STOPPED as isize,
            Ok(None) => return libc::recv(fd, dst, n, 0),
            Ok(Some(_)) => {}
        }
    }
    let idle_at = idle_bound(fd, libc::SO_RCVTIMEO, deadline);
    loop {
        match wait_fd(fd, libc::POLLIN, deadline, idle_at, pacer) {
            FdWait::Ready => {}
            FdWait::Deadline => return HTTP_READ_DEADLINE as isize,
            FdWait::Stopped => return HTTP_READ_STOPPED as isize,
            // What the blocking call would have returned at its SO_RCVTIMEO: EAGAIN, or the bytes
            // that raced the bound in.
            FdWait::Idle => return libc::recv(fd, dst, n, libc::MSG_DONTWAIT),
            FdWait::Error => return -1,
        }
        let r = libc::recv(fd, dst, n, libc::MSG_DONTWAIT);
        if r < 0 {
            let e = errno();
            if e == libc::EINTR || e == libc::EAGAIN || e == libc::EWOULDBLOCK {
                continue;
            }
        }
        return r;
    }
}

/// The send-side twin of [`recv_until`]. Requests are normally one tiny write, but an absolute
/// candidate-open budget is a whole-chain bound: a peer that stops reading cannot renew it through
/// the relative `SO_SNDTIMEO` on every partial write.
unsafe fn send_until(
    fd: c_int,
    src: *const c_void,
    n: usize,
    deadline: Option<Instant>,
    pacer: &mut Pacer,
) -> isize {
    if deadline.is_none() {
        match pacer.before_wait() {
            Err(_) => return HTTP_READ_STOPPED as isize,
            Ok(None) => return libc::send(fd, src, n, 0),
            Ok(Some(_)) => {}
        }
    }
    let idle_at = idle_bound(fd, libc::SO_SNDTIMEO, deadline);
    loop {
        match wait_fd(fd, libc::POLLOUT, deadline, idle_at, pacer) {
            FdWait::Ready => {}
            FdWait::Deadline => return HTTP_READ_DEADLINE as isize,
            FdWait::Stopped => return HTTP_READ_STOPPED as isize,
            FdWait::Idle => return libc::send(fd, src, n, libc::MSG_DONTWAIT),
            FdWait::Error => return -1,
        }
        let sent = libc::send(fd, src, n, libc::MSG_DONTWAIT);
        if sent < 0 {
            let e = errno();
            if e == libc::EINTR || e == libc::EAGAIN || e == libc::EWOULDBLOCK {
                continue;
            }
        }
        return sent;
    }
}

/// One raw body byte (buffered first, then socket) — for chunk framing.
unsafe fn hs_getb(
    hs: &mut HttpStream,
    deadline: Option<Instant>,
    pacer: &mut Pacer,
) -> Result<Option<u8>, c_int> {
    if (hs.bpos as usize) < (hs.blen as usize) {
        let b = hs.buf[hs.bpos as usize];
        hs.bpos += 1;
        return Ok(Some(b));
    }
    let fd = hs.fd();
    if fd < 0 {
        return Ok(None);
    }
    let mut b: u8 = 0;
    let r = recv_until(fd, &mut b as *mut u8 as *mut c_void, 1, deadline, pacer);
    if r == 1 {
        Ok(Some(b))
    } else {
        if r == 0 {
            close_owned(hs);
        }
        if r < 0 {
            Err(r as c_int)
        } else {
            Ok(None)
        }
    }
}

/// next chunk-size line (skips trailing CRLF + extensions). Some(0)=last chunk.
unsafe fn hs_next_chunk(
    hs: &mut HttpStream,
    deadline: Option<Instant>,
    pacer: &mut Pacer,
) -> Result<Option<i64>, c_int> {
    let mut b;
    loop {
        let Some(next) = hs_getb(hs, deadline, pacer)? else {
            return Ok(None);
        };
        b = next;
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
        match hs_getb(hs, deadline, pacer)? {
            Some(x) => b = x,
            None => return Ok(if any { Some(sz) } else { None }),
        }
    }
    while b != b'\n' {
        match hs_getb(hs, deadline, pacer)? {
            Some(x) => b = x,
            None => break,
        }
    }
    Ok(if any { Some(sz) } else { None })
}

/// After the last-chunk size line, consume trailer fields and the terminating CRLF so the next
/// keep-alive request does not parse leftover bytes as a status line.
unsafe fn hs_skip_chunked_trailers(
    hs: &mut HttpStream,
    deadline: Option<Instant>,
    pacer: &mut Pacer,
) -> Result<(), c_int> {
    loop {
        let Some(b) = hs_getb(hs, deadline, pacer)? else {
            // EOF before the terminating blank line: leftover trailer bytes would be parsed as
            // the next status line if we reused. Close rather than keep-alive.
            return Err(-1);
        };
        if b == b'\n' {
            if hs.trailer_line_has_content == 0 {
                return Ok(());
            }
            hs.trailer_line_has_content = 0;
            continue;
        }
        if b != b'\r' {
            hs.trailer_line_has_content = 1;
        }
    }
}

/// Strip the optional whitespace (RFC 9110 §5.6.3's `OWS` — spaces and horizontal tabs only) from
/// both ends of a header token.
fn trim_ows(v: &[u8]) -> &[u8] {
    let a = v.iter().take_while(|b| **b == b' ' || **b == b'\t').count();
    let v = &v[a..];
    let b = v
        .iter()
        .rev()
        .take_while(|b| **b == b' ' || **b == b'\t')
        .count();
    &v[..v.len() - b]
}

/// `Connection: close` on an HTTP/1.1 response forbids reuse of this fd.
fn header_has_connection_close(hdr: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"\r\nconnection:";
    let mut at = 0usize;
    while let Some(p) = find_ci(&hdr[at..], NEEDLE) {
        let vs = at + p + NEEDLE.len();
        let end = hdr[vs..]
            .iter()
            .position(|&b| b == b'\r' || b == b'\n')
            .map_or(hdr.len(), |i| vs + i);
        if hdr[vs..end]
            .split(|&b| b == b',')
            .any(|t| trim_ows(t).eq_ignore_ascii_case(b"close"))
        {
            return true;
        }
        at = end;
    }
    false
}

fn remember_peer(hs: &mut HttpStream, host: &str, port: c_int) {
    let bytes = host.as_bytes();
    let n = bytes.len().min(hs.peer_host.len());
    hs.peer_host[..n].copy_from_slice(&bytes[..n]);
    hs.peer_host_len = n as c_int;
    hs.peer_port = port;
}

fn same_peer(hs: &HttpStream, host: &str, port: c_int) -> bool {
    let n = hs.peer_host_len as usize;
    n <= hs.peer_host.len()
        && hs.peer_port == port
        && hs.peer_host.get(..n) == Some(host.as_bytes())
}

/// May this open send its request on the live fd instead of dialling?
fn can_reuse(hs: &HttpStream, host: &str, port: c_int) -> bool {
    hs.fd() >= 0
        && !hs.interrupted()
        && hs.keep_alive != 0
        && hs.body_done != 0
        && (hs.bpos as usize) >= (hs.blen as usize)
        && same_peer(hs, host, port)
}

/// Body complete: keep the fd when the server offered keep-alive, otherwise close.
/// `body_done` is set in both cases so a closed Connection: close response is not mistaken for
/// an incomplete transfer (and a mid-body close, which never reaches here, is not mistaken for
/// done).
unsafe fn finish_body(hs: &mut HttpStream) {
    hs.body_done = 1;
    if hs.keep_alive == 0 || hs.fd() < 0 {
        close_owned(hs);
    }
}

/// Is this response body framed with the `chunked` transfer coding?
///
/// This used to be `find_ci(hdr, b"\r\ntransfer-encoding: chunked")` — an exact compare against one
/// spelling, so every legal variation of the same header missed and the body was then read as
/// close-delimited with its chunk-size lines left INLINE in it. Silent corruption, not a failure:
/// `Transfer-Encoding:chunked` (no space, which the grammar allows — OWS is optional after the
/// colon), `Transfer-Encoding: Chunked` (the VALUE is case-insensitive too, RFC 9110 §10.1.4, and
/// `find_ci` folding the whole needle only made that one work by accident), a list such as
/// `gzip, chunked`, or a second `Transfer-Encoding` line, since the field is a list that a sender
/// may split across lines (RFC 9110 §5.3).
///
/// So: walk every `Transfer-Encoding` line, split each on commas, and ask whether any token IS
/// `chunked`. That is deliberately more permissive than the grammar — RFC 9112 §6.1 requires
/// chunked to be the FINAL coding, so `chunked, gzip` is malformed — but a recipient that refuses
/// to see the chunk framing a sender did apply reads the sizes as body bytes, which is the worse of
/// the two failures by a distance.
///
/// The value offset is `NEEDLE.len()`, never a written-out number. `Content-Length`'s parse three
/// lines down still carries a hand-counted `p + 17`, which is correct and is one edit away from not
/// being — the literal and the constant that indexes past it cannot drift apart if only one of them
/// exists.
fn header_is_chunked(hdr: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"\r\ntransfer-encoding:";
    let mut at = 0usize;
    while let Some(p) = find_ci(&hdr[at..], NEEDLE) {
        let vs = at + p + NEEDLE.len();
        // The value runs to the end of the line; a header block always carries its final CRLF, so
        // the fallback to `hdr.len()` is only reachable on a truncated one.
        let end = hdr[vs..]
            .iter()
            .position(|&b| b == b'\r' || b == b'\n')
            .map_or(hdr.len(), |i| vs + i);
        if hdr[vs..end]
            .split(|&b| b == b',')
            .any(|t| trim_ows(t).eq_ignore_ascii_case(b"chunked"))
        {
            return true;
        }
        at = end; // strictly greater than `at` (the needle is non-empty), so this terminates
    }
    false
}

/// How long the handshake phase of ONE open may take, across EVERY address the host resolved to.
///
/// Without a bound at all the app inherits the kernel's SYN-retry budget (~2 min on Linux), during
/// which the 60fps SDL loop is fully blocked — an unreachable PMS (box rebooting, TV on a different
/// VLAN, DHCP re-lease) froze the whole UI rather than failing, and every PMS request in a chain
/// paid it again.
///
/// **2000 ms was a LAN number**, and it was the right one while the only dialable host was a
/// dotted quad on the same subnet. It is the wrong one now the host can be a name on the public
/// internet: Linux's first SYN retransmit is at 1 s, so one lost SYN plus a trans-continental RTT
/// already spends it, and a server that would have answered gets reported unreachable. 8000 ms
/// covers three SYN attempts (t = 0, 1, 3 s) and part of the fourth wait, which is about where a
/// handshake that has not completed is not going to.
///
/// **Be clear about who pays for that today, because it is not who it is chosen for.** No caller
/// hands this module a public-internet name yet: `auth::dial_target` still admits only an IPv4
/// literal over plain HTTP, so every host that reaches here is a dotted quad on the LAN. What the
/// raise actually buys *today* is a 4× longer main-loop freeze when the LAN server is down — a PMS
/// rebooting, the set moved to another VLAN — and what it buys is paid back the day a name is
/// dialled. It is a forward-looking number, on purpose and by instruction, and it is the one line
/// here to re-examine if that day gets further away rather than closer.
///
/// **One number, and it is deliberately NOT derived from the address.** RFC1918 is not "local" in
/// Plex's sense — a NAT'd server reached over a VPN is private and far, and a `plex.direct` name
/// resolves to a LAN address — so an address cannot tell you which connection tier you are on.
/// Choosing a probe ORDER, and how much patience each tier is worth, is `plex::probe`'s policy;
/// this layer owes exactly one honest ceiling on how long the main loop can be held.
///
/// **It is the budget for the whole chain, not per address**, so the worst-case freeze is this
/// number however many addresses the resolver returned — a count this app does not control. The
/// cost of that choice is that a first address which silently blackholes SYNs can spend the budget
/// before a later one is tried. The common shape does not: an address with no route (the usual
/// "this network has no IPv6 at all") fails `connect(2)` synchronously with ENETUNREACH in ~0 ms
/// and costs the chain nothing, and `AI_ADDRCONFIG` keeps that list from being offered in the first
/// place. Doing better than serial-with-a-shared-deadline means the concurrent attempts of Happy
/// Eyeballs (RFC 8305), which a blocking module with no thread of its own cannot run.
const CONNECT_TIMEOUT_MS: c_int = 8000;
const MEDIA_RECV_TIMEOUT_MS: c_int = 15_000;
const MEDIA_SEND_TIMEOUT_MS: c_int = 10_000;

/// The ordinary plaintext media inactivity contract. Candidate reserve projections may wake a
/// read earlier, but retrying an obsolete projection must never renew this physical stall bound.
pub(crate) fn media_stall_budget() -> std::time::Duration {
    std::time::Duration::from_millis(MEDIA_RECV_TIMEOUT_MS as u64)
}

/// Why an HTTP request failed before its response body became readable.
///
/// The legacy open functions return only `0`/`-1`, but a caller composing its own absolute
/// deadline must not infer the cause later from `Instant::now()`: a real HTTP response remains an
/// HTTP response even if that caller is descheduled across the boundary after this function
/// returns.  This type preserves every cause the raw transport can prove at the point it occurs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HttpOpenError {
    /// The supplied absolute deadline stopped the connect/send/header operation. The caller which
    /// selected that instant still owns its meaning (transaction reserve versus liveness).
    Deadline,
    /// A complete, syntactically usable response head carried a non-success status.
    Status(c_int),
    /// [`http_shutdown`] interrupted this request while it was being opened.
    Aborted,
    /// The caller's [`Checkpoint`] stopped the connect/send/header wait. The request is retired
    /// (its socket closed, never redialled); what the stop means belongs to the caller.
    Stopped,
    /// Invalid input, resolution/connect failure, malformed/truncated headers, or another I/O
    /// failure for which this transport has no more specific fact.
    Transport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectAttempt {
    Connected,
    TimedOut,
    Failed,
    Stopped,
}

/// `connect(2)` bounded by `timeout_ms`. Flips the socket to non-blocking for the handshake,
/// waits on `poll(POLLOUT)`, then reads `SO_ERROR` to learn the real outcome (a writable socket
/// does NOT mean success), and restores blocking mode so every read path below is unchanged.
/// The caller owns closing `fd`.
///
/// The address arrives as a `(*const sockaddr, socklen_t)` pair rather than a `&sockaddr_in`
/// because it now comes out of `getaddrinfo` and may be a `sockaddr_in6`: the pair IS the
/// `addrinfo`'s own `ai_addr`/`ai_addrlen`, forwarded without a copy and without this function
/// having to know or test the family. A non-positive `timeout_ms` (the chain budget already spent)
/// is clamped to 0 rather than passed on — `poll` reads a NEGATIVE timeout as "block forever",
/// which would turn an exhausted budget into the unbounded wait this whole function removes.
unsafe fn connect_timeout_cause(
    fd: c_int,
    sa: *const libc::sockaddr,
    salen: libc::socklen_t,
    timeout_ms: c_int,
    pacer: &mut Pacer,
) -> ConnectAttempt {
    let flags = libc::fcntl(fd, libc::F_GETFL, 0);
    if flags < 0 {
        return ConnectAttempt::Failed;
    }
    let restore = |outcome: ConnectAttempt| -> ConnectAttempt {
        libc::fcntl(fd, libc::F_SETFL, flags);
        outcome
    };
    if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
        return ConnectAttempt::Failed;
    }
    if libc::connect(fd, sa, salen) == 0 {
        return restore(ConnectAttempt::Connected); // connected immediately (loopback / same host)
    }
    if errno() != libc::EINPROGRESS {
        return restore(ConnectAttempt::Failed);
    }
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    // The budget as an absolute end, so a checkpoint slice's expiry (a recheck, then more
    // waiting) cannot stretch it. Never negative — that is `poll`'s "wait forever".
    let mut end = Instant::now() + std::time::Duration::from_millis(timeout_ms.max(0) as u64);
    loop {
        let Ok(slice) = pacer.before_wait() else {
            return restore(ConnectAttempt::Stopped);
        };
        let now = Instant::now();
        let (wait_ms, recheck) = if now >= end {
            (0, false)
        } else {
            let left_ms = crate::checkpoint::wait_ms_until(end, c_int::MAX);
            match slice {
                Some(at) if at < end => (crate::checkpoint::wait_ms_until(at, left_ms), true),
                _ => (left_ms, false),
            }
        };
        let r = libc::poll(&mut pfd, 1, wait_ms);
        if r > 0 {
            break;
        }
        if r == 0 {
            if recheck {
                continue;
            }
            return restore(ConnectAttempt::TimedOut); // the host is not answering
        }
        if errno() != libc::EINTR {
            return restore(ConnectAttempt::Failed);
        }
        end = Instant::now(); // a signal ate the wait; poll once more without blocking again
    }
    // Writable does not imply connected — SO_ERROR carries the verdict.
    let mut err: c_int = 0;
    let mut elen = std::mem::size_of::<c_int>() as libc::socklen_t;
    if libc::getsockopt(
        fd,
        libc::SOL_SOCKET,
        libc::SO_ERROR,
        &mut err as *mut _ as *mut c_void,
        &mut elen,
    ) < 0
        || err != 0
    {
        return restore(ConnectAttempt::Failed);
    }
    restore(ConnectAttempt::Connected)
}

/// Legacy result retained for the focused connect tests.
#[cfg(test)]
unsafe fn connect_timeout(
    fd: c_int,
    sa: *const libc::sockaddr,
    salen: libc::socklen_t,
    timeout_ms: c_int,
) -> c_int {
    let mut none = NoCheckpoint;
    let mut pacer = Pacer::new(&mut none);
    if connect_timeout_cause(fd, sa, salen, timeout_ms, &mut pacer) == ConnectAttempt::Connected {
        0
    } else {
        -1
    }
}

/// An address list from `getaddrinfo`, freed on drop — including on the early return every failed
/// `connect` in [`connect_any_result`] can take. Only ever constructed around a NON-NULL head,
/// because `freeaddrinfo(NULL)` is not a documented no-op the way `free(NULL)` is.
struct AddrList {
    head: *mut libc::addrinfo,
}

impl Drop for AddrList {
    fn drop(&mut self) {
        unsafe { libc::freeaddrinfo(self.head) };
    }
}

/// The bytes to hand `getaddrinfo` as its node: a v6 literal WITHOUT its brackets.
///
/// The brackets belong to the URI authority grammar (RFC 3986 §3.2.2) — which is where they are
/// needed, in the `Host:` header and in a URL, and why `plex::probe::host_of` hands one back
/// bracketed. The resolver takes an ADDRESS, not an authority: `getaddrinfo("[::1]", …)` is
/// EAI_NONAME. Stripping here rather than asking every caller to remember means a bracketed host
/// cannot silently become "that server does not resolve", which is a failure nothing in the log
/// would distinguish from a real DNS failure.
fn resolver_node(host: &str) -> &str {
    match host.strip_prefix('[') {
        Some(rest) => rest.strip_suffix(']').unwrap_or(rest),
        None => host,
    }
}

/// Is `host` an address LITERAL rather than a name? Decides only which `getaddrinfo` flag is used —
/// see [`resolve`], where the distinction is load-bearing.
///
/// `IpAddr`'s parse is the whole test, and it is strict in the way this needs: exactly four octets
/// for v4 (so `1.2.3` and `999.1.2.3` are not addresses, matching the hand-rolled parse this
/// replaced). A v6 literal may carry a `%zone` suffix — `fe80::1%eth0` — which `IpAddr` does not
/// parse but every resolver here does accept, so the zone is cut before asking.
fn is_numeric_host(host: &str) -> bool {
    let bare = host.split('%').next().unwrap_or(host);
    bare.parse::<std::net::IpAddr>().is_ok()
}

/// The `Host:` header value for a request — built from what the CALLER named, and never from the
/// address it resolved to.
///
/// Emitting the address is what the code did before there was any resolution, when the two could
/// not differ. They differ now, and a numeric `Host:` breaks name-based virtual hosting outright:
/// a reverse proxy in front of a PMS routes on this header, and a server that answers
/// `plex.example.org` has no vhost named `203.0.113.9`. It is also the value a TLS SNI would have
/// to agree with if https ever lands here.
///
/// A v6 literal is BRACKETED (RFC 9110 §7.2's `Host` → RFC 3986 §3.2.2's `IP-literal`), which is
/// the exact opposite of what [`resolver_node`] hands the resolver. The two live one function apart
/// so the asymmetry is something you can see rather than something you have to remember.
fn host_header(host: &str, port: c_int) -> String {
    let bare = resolver_node(host);
    if bare.contains(':') {
        format!("[{bare}]:{port}") // an IPv6 literal, (re-)bracketed for the authority
    } else {
        format!("{bare}:{port}")
    }
}

/// Resolve an (already unbracketed) host and port into a connect-order address list.
///
/// `AF_UNSPEC` + `SOCK_STREAM`, so the ORDER is the system's own destination-address policy
/// (RFC 6724 under glibc) rather than a family this file picks — which is what makes IPv6 work at
/// all here instead of merely being expressible.
///
/// The flags are the whole subtlety:
///
/// * A **numeric literal takes `AI_NUMERICHOST`**, and that is a correctness fix rather than an
///   optimisation. `AI_ADDRCONFIG` suppresses AF_INET6 results on a host whose only IPv6 address is
///   loopback, so `getaddrinfo("::1", …, AI_ADDRCONFIG)` legitimately resolves to NOTHING — which
///   would make every v6 literal, and this file's own loopback tests, unresolvable on a perfectly
///   healthy machine. A literal has nothing to configure-filter in any case: the caller named one
///   exact address. Keeping the old dotted-quad fast path free is the side effect — no NSS module
///   is loaded and no packet is sent, so the per-seek AVIO reopen still costs what the hand-rolled
///   octet parse did.
/// * A **name takes `AI_ADDRCONFIG`**, which is precisely what that flag is for: do not ask for an
///   AAAA on a set with no IPv6 address of its own, and do not return one it could never reach.
/// * Both take **`AI_NUMERICSERV`**: the service is always our own decimal port, so there is no
///   reason to let it fall through to an `/etc/services` lookup.
///
/// `None` for every resolver failure. EAI_NONAME (a name that does not exist) and EAI_AGAIN (a
/// resolver that did not answer) are genuinely different facts, but [`http_open`] reports a flat
/// -1 whatever went wrong, so the distinction is logged at the call site rather than returned.
///
/// **This call is BLOCKING and nothing in this file can interrupt it.** Every other wait in an open
/// is bounded — `CONNECT_TIMEOUT_MS` for the handshake, `SO_RCVTIMEO` for the read — and every one
/// of them can be cut short by `http_shutdown`, because there is a descriptor published for it to
/// shoot. There is none here: `getaddrinfo` owns its sockets. A NAME whose DNS server is not
/// answering therefore holds the caller for the resolver's own budget, which under glibc is
/// `timeout` × `attempts` × nameservers and defaults to 5 s × 2 each — far past anything this
/// module bounds. Names are ordinary inputs now: control-plane calls reach this code on background
/// workers and plaintext media reaches it on the demux thread, so resolution cannot freeze the
/// SDL loop but can outlive the advertised connect budget or delay a worker join. T4's HTTPS media
/// source uses libcurl after integration; a plaintext hostname remains this resolver's job. The
/// levers, none of them this file's to pull, are an interruptible resolver or an application-owned
/// resolution worker.
unsafe fn resolve(host: &str, port: c_int) -> Option<AddrList> {
    // The offline reproduction (`/tmp/plxnative-nowan`): a name that would have gone to the
    // resolver is refused here, exactly where a dead resolver would have refused it. A literal is
    // untouched — the plaintext transport never needed DNS for one, which is the whole point of
    // the twin `probe::candidates` synthesizes.
    if crate::net::refuse_name(host, (CONNECT_TIMEOUT_MS / 1000) as _) {
        return None;
    }
    // The port is range-checked HERE and not left to `AI_NUMERICSERV`, because the two platforms
    // disagree and the disagreement is SILENT. Darwin rejects an out-of-range numeric service;
    // glibc parses it with `strtoul`, applies no range check at all, and hands back
    // `htons(70000)` — port 4464. That is bit for bit the truncation `(port as u16).to_be()` used
    // to do, so trusting the resolver would have MOVED this bug rather than fixed it, and moved it
    // somewhere worse: `cargo test` runs on Darwin, so the platform that silently truncates is
    // exactly the one no host test can see. A request that lands on a real service at the wrong
    // port reports nothing anywhere.
    if !(0..=65535).contains(&port) {
        return None;
    }
    let node = std::ffi::CString::new(host).ok()?;
    let service = std::ffi::CString::new(port.to_string()).ok()?;
    let mut hints: libc::addrinfo = std::mem::zeroed();
    hints.ai_family = libc::AF_UNSPEC;
    hints.ai_socktype = libc::SOCK_STREAM;
    hints.ai_flags = libc::AI_NUMERICSERV
        | if is_numeric_host(host) {
            libc::AI_NUMERICHOST
        } else {
            libc::AI_ADDRCONFIG
        };
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    if libc::getaddrinfo(node.as_ptr(), service.as_ptr(), &hints, &mut res) != 0 || res.is_null() {
        return None;
    }
    Some(AddrList { head: res })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectFailure {
    TimedOut,
    Aborted,
    Transport,
    Stopped,
}

/// Dial down the address chain until one answers, within `budget_ms` for the WHOLE walk. Returns
/// the connected fd — also left PUBLISHED in `hs` — or the cause with `hs` closed. The legacy
/// test adapter below deliberately folds every failure back to `-1` for the focused walk tests.
///
/// Trying only the first address would be today's single-address limit wearing a resolver. A name
/// with an A and an AAAA, or one behind two front ends, is routinely reachable on the second when
/// it is not on the first; walking the chain is most of what resolving is FOR.
///
/// Two of this file's invariants ride on every iteration, not just the first:
///
/// * **The fd is published into `hs` BEFORE `connect`**, which is what makes the open interruptible
///   at all — see [`http_open`]'s note, where the behaviour is measured on the TV's kernel with
///   `tools/sockprobe.c` and is NOT what the Darwin host these tests run on does.
/// * **Every failed attempt is retired through [`close_owned`], never a bare `close`.** A bare
///   close leaves a stale fd NUMBER armed in the atomic for the next `http_shutdown` to shoot after
///   the kernel has recycled it into some other thread's socket, and leaves `take_fd` nothing to
///   return.
///
/// And the walk stops on [`HttpStream::interrupted`], which is the field's entire reason to exist:
/// answering a teardown by dialling the next address would undo the interruptibility the early
/// publish is there to give.
unsafe fn connect_any_result(
    hs: &HttpStream,
    head: *const libc::addrinfo,
    budget_ms: c_int,
    pacer: &mut Pacer,
) -> Result<c_int, ConnectFailure> {
    let started = std::time::Instant::now();
    let mut ai = head;
    let mut timed_out = false;
    while !ai.is_null() {
        let a = &*ai;
        ai = a.ai_next;
        let fd = libc::socket(a.ai_family, a.ai_socktype, a.ai_protocol);
        if fd < 0 {
            continue; // a family the kernel will not give us (no IPv6 in this build) — try the next
        }
        hs.set_fd(fd); // PUBLISHED before connect, per attempt — see the doc above
                       // Clamped to the budget before the subtraction, so what `connect_timeout` is handed is in
                       // [0, budget] whatever the clock did — a `u128` cast of a negative budget would otherwise
                       // come back enormous and hand the LAST attempt an unbounded-looking wait.
        let spent = started.elapsed().as_millis().min(budget_ms.max(0) as u128) as c_int;
        match connect_timeout_cause(fd, a.ai_addr, a.ai_addrlen, budget_ms.max(0) - spent, pacer) {
            ConnectAttempt::Connected => return Ok(fd),
            ConnectAttempt::TimedOut => timed_out = true,
            ConnectAttempt::Failed => {}
            ConnectAttempt::Stopped => {
                close_owned(hs);
                return Err(ConnectFailure::Stopped); // the caller's stop, not a dead address
            }
        }
        close_owned(hs); // published, so it must be RETIRED
        if hs.interrupted() {
            return Err(ConnectFailure::Aborted); // do not answer teardown with the next address
        }
    }
    if hs.interrupted() {
        Err(ConnectFailure::Aborted)
    } else if timed_out {
        Err(ConnectFailure::TimedOut)
    } else {
        Err(ConnectFailure::Transport)
    }
}

#[cfg(test)]
unsafe fn connect_any(hs: &HttpStream, head: *const libc::addrinfo, budget_ms: c_int) -> c_int {
    connect_any_result(hs, head, budget_ms, &mut Pacer::new(&mut NoCheckpoint)).unwrap_or(-1)
}

unsafe fn set_socket_timeouts(fd: c_int, recv_timeout_ms: c_int, send_timeout_ms: c_int) {
    let recv = libc::timeval {
        tv_sec: (recv_timeout_ms.max(1) / 1000) as libc::time_t,
        tv_usec: ((recv_timeout_ms.max(1) % 1000) * 1000) as libc::suseconds_t,
    };
    libc::setsockopt(
        fd,
        libc::SOL_SOCKET,
        libc::SO_RCVTIMEO,
        &recv as *const _ as *const c_void,
        std::mem::size_of::<libc::timeval>() as libc::socklen_t,
    );
    let send = libc::timeval {
        tv_sec: (send_timeout_ms.max(1) / 1000) as libc::time_t,
        tv_usec: ((send_timeout_ms.max(1) % 1000) * 1000) as libc::suseconds_t,
    };
    libc::setsockopt(
        fd,
        libc::SOL_SOCKET,
        libc::SO_SNDTIMEO,
        &send as *const _ as *const c_void,
        std::mem::size_of::<libc::timeval>() as libc::socklen_t,
    );
}

pub(crate) fn http_open(
    hs: *mut HttpStream,
    host: *const c_char,
    port: c_int,
    path: *const c_char,
    extra: *const c_char,
    method: &str,
) -> c_int {
    legacy_open_result(http_open_with_timeouts(
        hs,
        host,
        port,
        path,
        extra,
        method,
        CONNECT_TIMEOUT_MS,
        MEDIA_RECV_TIMEOUT_MS,
        MEDIA_SEND_TIMEOUT_MS,
        None,
        false,
        &mut NoCheckpoint,
    ))
}

/// [`http_open`] with its typed result, consulting `checkpoint` before and during every blocking
/// connect/send/header wait. The HLS segment open's no-deadline branch, so no branch of that open
/// drops its acquisition's checkpoint.
pub(crate) fn http_open_result(
    hs: *mut HttpStream,
    host: *const c_char,
    port: c_int,
    path: *const c_char,
    extra: *const c_char,
    method: &str,
    checkpoint: &mut dyn Checkpoint,
) -> Result<(), HttpOpenError> {
    http_open_with_timeouts(
        hs,
        host,
        port,
        path,
        extra,
        method,
        CONNECT_TIMEOUT_MS,
        MEDIA_RECV_TIMEOUT_MS,
        MEDIA_SEND_TIMEOUT_MS,
        None,
        false,
        checkpoint,
    )
}

/// [`http_open`] with the whole-chain connect and stalled-I/O ceiling selected by the caller.
/// Candidate discovery is the one caller that knows whether a connection is local or remote; the
/// ordinary request path keeps [`CONNECT_TIMEOUT_MS`] and never infers a tier from an address.
pub(crate) fn http_open_probe(
    hs: *mut HttpStream,
    host: *const c_char,
    port: c_int,
    path: *const c_char,
    extra: *const c_char,
    method: &str,
    timeout_ms: c_int,
) -> c_int {
    legacy_open_result(http_open_with_timeouts(
        hs,
        host,
        port,
        path,
        extra,
        method,
        timeout_ms,
        timeout_ms,
        timeout_ms,
        None,
        false,
        &mut NoCheckpoint,
    ))
}

/// Open a candidate media request inside one absolute setup snapshot. Unlike
/// [`http_open_probe`], this bound applies to the whole connect/send/header chain and is removed
/// after successful headers; body reads receive their own current projection from the caller.
///
/// The result records the cause at the transport seam, before a higher layer can cross `deadline`
/// and accidentally reclassify a known HTTP status as timeout.
///
/// `checkpoint` is consulted before and during every blocking connect/send/header wait (not DNS,
/// which is a synchronous `getaddrinfo`); [`HttpOpenError::Stopped`] is its answer, never a redial.
pub(crate) fn http_open_until_result(
    hs: *mut HttpStream,
    host: *const c_char,
    port: c_int,
    path: *const c_char,
    extra: *const c_char,
    method: &str,
    deadline: Instant,
    checkpoint: &mut dyn Checkpoint,
) -> Result<(), HttpOpenError> {
    http_open_with_timeouts(
        hs,
        host,
        port,
        path,
        extra,
        method,
        CONNECT_TIMEOUT_MS,
        MEDIA_RECV_TIMEOUT_MS,
        MEDIA_SEND_TIMEOUT_MS,
        Some(deadline),
        true,
        checkpoint,
    )
}

fn legacy_open_result(result: Result<(), HttpOpenError>) -> c_int {
    if result.is_ok() {
        0
    } else {
        -1
    }
}

fn http_open_with_timeouts(
    hs: *mut HttpStream,
    host: *const c_char,
    port: c_int,
    path: *const c_char,
    extra: *const c_char,
    method: &str,
    connect_timeout_ms: c_int,
    recv_timeout_ms: c_int,
    send_timeout_ms: c_int,
    open_deadline: Option<Instant>,
    restore_media_timeouts: bool,
    checkpoint: &mut dyn Checkpoint,
) -> Result<(), HttpOpenError> {
    if hs.is_null() || host.is_null() || path.is_null() {
        return Err(HttpOpenError::Transport);
    }
    let mut pacer = Pacer::new(checkpoint);
    let pacer = &mut pacer;
    unsafe {
        let hs = &mut *hs;
        if open_deadline.is_some_and(|at| Instant::now() >= at) {
            close_owned(hs);
            return Err(HttpOpenError::Deadline);
        }

        let host_s = CStr::from_ptr(host).to_string_lossy();
        let path_s = CStr::from_ptr(path).to_string_lossy();
        // A shutdown of this stream is teardown of THIS open, including when fd is already -1
        // (first open, or seek_cb after http_close). Consuming the latch here let a teardown
        // that landed after the caller's AU-abort check connect under join. Production boxes a
        // fresh HttpStream per engine, so there is no later-session latch to consume.
        if hs.interrupted() {
            close_owned(hs);
            return Err(HttpOpenError::Aborted);
        }
        let reuse = can_reuse(hs, &host_s, port);
        if !reuse {
            close_owned(hs);
        }
        let reused_fd = hs.fd();
        hs.reset_request_fields();
        // A shutdown during the reset must still abort a live keep-alive, not send on a
        // half-closed fd and then redial. The latch is still set; we do not restore it.
        if reused_fd >= 0 && hs.interrupted() {
            close_owned(hs);
            return Err(HttpOpenError::Aborted);
        }

        if reused_fd >= 0 {
            match perform_http_request(
                hs,
                reused_fd,
                &host_s,
                port,
                &path_s,
                extra,
                method,
                open_deadline,
                restore_media_timeouts,
                pacer,
            ) {
                Ok(()) => return Ok(()),
                Err(HttpOpenError::Aborted) => return Err(HttpOpenError::Aborted),
                // A controlled stop is the caller's decision, not a stale keep-alive: no redial.
                Err(HttpOpenError::Stopped) => return Err(HttpOpenError::Stopped),
                Err(HttpOpenError::Deadline) => return Err(HttpOpenError::Deadline),
                Err(HttpOpenError::Status(status)) => return Err(HttpOpenError::Status(status)),
                Err(HttpOpenError::Transport) => {
                    // Idle timeout or a half-closed keep-alive: one redial, not a hard failure.
                    // Never consume the latch here: a teardown that lands after the reuse send
                    // would otherwise look like a later session and dial a socket it cannot reach.
                    close_owned(hs);
                    hs.reset_request_fields();
                    if hs.interrupted() {
                        return Err(HttpOpenError::Aborted);
                    }
                }
            }
        }

        // Resolve FIRST, before any descriptor exists: the address family to open is the resolver's
        // answer, not this file's assumption, which is the whole of what makes an AF_INET6
        // connection possible. It also means an unresolvable host now fails with no socket ever
        // created — where the hand-rolled octet parse it replaced failed one step later, after
        // `socket()`, and had to retire an fd on the way out.
        let list = match resolve(resolver_node(&host_s), port) {
            Some(l) => l,
            None => {
                // The host is on the line because "which name failed to resolve" is the only
                // question this failure raises, and it is not a secret the way a query string is
                // (`log_endpoint`) — `player::engine` and `plex::servers` already log `host=…:port`.
                crate::log(&format!(
                    "stream: {method} {} DNS FAILED host={host_s}",
                    log_endpoint(&path_s)
                ));
                return Err(if hs.interrupted() {
                    HttpOpenError::Aborted
                } else {
                    HttpOpenError::Transport
                });
            }
        };
        // PUBLISHED BEFORE CONNECT, on every attempt (`connect_any` does it) — which makes the
        // whole open interruptible by `http_shutdown`, and is why every failure path there and
        // below must retire it through `close_owned` rather than a bare `close`: a bare close
        // leaves a stale number armed in the atomic for the next interrupt to shoot, and leaves
        // `take_fd` nothing to return.
        //
        // This was tried once and REVERTED (docs/async-model-decision.md): it made every reopen
        // interruptible while the pump was firing `http_shutdown` to service a SEEK, which cost
        // `seek_inplace_h264`. That coupling is gone — 5938b5f/71929ee moved seeking into the
        // demux thread's own `av_seek_frame`, so the only `http_shutdown` left in the tree is
        // teardown's (`player/engine.rs`), where cutting an open short is precisely the intent.
        //
        // Worth publishing this early only because `shutdown(2)` aborts a handshake in progress on
        // the TV's kernel — measured with `tools/sockprobe.c`, NOT assumed. Linux is documented to
        // fail this with ENOTCONN and the host (Darwin) does something different again, so the
        // question could not be settled by reading or by `cargo test`. If that ever stops holding,
        // publishing after `connect_timeout` still buys the 15 s `SO_RCVTIMEO` window, which is
        // the bulk of the win.
        //
        // Also the path a teardown takes: `http_shutdown` aborts the handshake, and `connect_any`
        // reports the failure one poll later — and, seeing the interrupt latch, stops walking
        // rather than answering the teardown with the next address.
        //
        // The latch is read on BOTH sides of the walk, which is what makes "a shutdown anywhere in
        // an open aborts that open" true rather than nearly true. Before: `resolve` holds no
        // descriptor, so a teardown during it has nothing to shoot and would otherwise be answered
        // by connecting anyway. After: `connect_any` has a window of its own between `socket()` and
        // the publish, and — on Darwin, per `tools/sockprobe.c` — an aborted handshake can even
        // report SUCCESS, which no `connect` return value would catch.
        if hs.interrupted() {
            return Err(HttpOpenError::Aborted); // torn down while resolving; no descriptor existed
        }
        let base_connect_ms = connect_timeout_ms.max(1);
        let (connect_budget_ms, caller_deadline_is_connect_ceiling) = match open_deadline {
            Some(at) => {
                let now = Instant::now();
                if now >= at {
                    return Err(HttpOpenError::Deadline);
                }
                let left = at.saturating_duration_since(now);
                let left_us = left.as_micros();
                let left_ms = ((left_us.saturating_add(999) / 1_000)
                    .max(1)
                    .min(c_int::MAX as u128)) as c_int;
                (
                    base_connect_ms.min(left_ms),
                    left <= std::time::Duration::from_millis(base_connect_ms as u64),
                )
            }
            None => (base_connect_ms, false),
        };
        let fd = match connect_any_result(hs, list.head, connect_budget_ms, pacer) {
            Ok(fd) => fd,
            Err(ConnectFailure::Aborted) => return Err(HttpOpenError::Aborted),
            Err(ConnectFailure::Stopped) => return Err(HttpOpenError::Stopped),
            Err(ConnectFailure::TimedOut) if caller_deadline_is_connect_ceiling => {
                return Err(HttpOpenError::Deadline);
            }
            Err(ConnectFailure::TimedOut | ConnectFailure::Transport) => {
                return Err(HttpOpenError::Transport);
            }
        };
        if hs.interrupted() {
            close_owned(hs); // connected through a teardown: retire it rather than send on it
            return Err(HttpOpenError::Aborted);
        }
        let one: c_int = 1;
        libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_NODELAY,
            &one as *const _ as *const c_void,
            4,
        );
        // Candidate probes carry their tier budget through this same seam. These socket options
        // remain the ordinary inactivity contract; `open_deadline` below is the separate absolute
        // conservation snapshot and is removed at the header/body boundary.
        set_socket_timeouts(fd, recv_timeout_ms, send_timeout_ms);
        perform_http_request(
            hs,
            fd,
            &host_s,
            port,
            &path_s,
            extra,
            method,
            open_deadline,
            restore_media_timeouts,
            pacer,
        )
    }
}

unsafe fn perform_http_request(
    hs: &mut HttpStream,
    fd: c_int,
    host_s: &str,
    port: c_int,
    path_s: &str,
    extra: *const c_char,
    method: &str,
    open_deadline: Option<Instant>,
    restore_media_timeouts: bool,
    pacer: &mut Pacer,
) -> Result<(), HttpOpenError> {
    // build + send the request (default Accept only if caller set none)
    let extra_s: String = if extra.is_null() {
        String::new()
    } else {
        CStr::from_ptr(extra).to_string_lossy().into_owned()
    };
    let accept = if extra_s.to_ascii_lowercase().contains("accept:") {
        ""
    } else {
        "Accept: */*\r\n"
    };
    // `Host:` is the ORIGIN, never the address `connect_any` reached — see `host_header`.
    let host_hdr = host_header(host_s, port);
    // HTTP/1.1 keep-alive is the default; omitting Connection lets the server reuse this
    // socket for the next HLS segment instead of forcing a fresh TCP handshake each time.
    let req = format!(
        "{method} {path_s} HTTP/1.1\r\nHost: {host_hdr}\r\nUser-Agent: plxnative/0.1\r\n{accept}{extra_s}\r\n"
    );
    let bytes = req.as_bytes();
    let mut off = 0usize;
    while off < bytes.len() {
        let w = send_until(
            fd,
            bytes[off..].as_ptr() as *const c_void,
            bytes.len() - off,
            open_deadline,
            pacer,
        );
        if w <= 0 {
            let error = if hs.interrupted() {
                HttpOpenError::Aborted
            } else if w == HTTP_READ_DEADLINE as isize {
                HttpOpenError::Deadline
            } else if w == HTTP_READ_STOPPED as isize {
                HttpOpenError::Stopped
            } else {
                HttpOpenError::Transport
            };
            close_owned(hs);
            return Err(error);
        }
        off += w as usize;
    }

    // read until end of headers (\r\n\r\n), keeping any body bytes that follow
    let cap = hs.buf.len();
    let mut hdr_end: Option<usize> = None;
    hs.blen = 0;
    while hdr_end.is_none() && (hs.blen as usize) < cap - 1 {
        let r = recv_until(
            fd,
            hs.buf.as_mut_ptr().add(hs.blen as usize) as *mut c_void,
            cap - hs.blen as usize,
            open_deadline,
            pacer,
        );
        // r == 0 is also how an interrupted open surfaces: `http_shutdown` wakes this
        // recv with EOF, so a teardown mid-header costs one syscall, not 15 s of SO_RCVTIMEO.
        if r <= 0 {
            let error = if hs.interrupted() {
                HttpOpenError::Aborted
            } else if r == HTTP_READ_DEADLINE as isize {
                HttpOpenError::Deadline
            } else if r == HTTP_READ_STOPPED as isize {
                HttpOpenError::Stopped
            } else {
                HttpOpenError::Transport
            };
            close_owned(hs);
            return Err(error);
        }
        hs.blen += r as c_int;
        let blen = hs.blen as usize;
        let mut i = 3;
        while i < blen {
            if hs.buf[i - 3] == b'\r'
                && hs.buf[i - 2] == b'\n'
                && hs.buf[i - 1] == b'\r'
                && hs.buf[i] == b'\n'
            {
                hdr_end = Some(i + 1);
                break;
            }
            i += 1;
        }
    }
    let hdr_end = match hdr_end {
        Some(e) => e,
        None => {
            close_owned(hs);
            return Err(if hs.interrupted() {
                HttpOpenError::Aborted
            } else {
                HttpOpenError::Transport
            });
        }
    };

    // Parse status line + Content-Length + chunked. HEADERS ARE BYTES, not UTF-8 (RFC 9110
    // §5.5: field values are octets, and a recipient must not reject the message for them).
    // This used to run on `from_utf8(...).unwrap_or("")`, which meant ONE stray byte anywhere
    // in the block — a Latin-1 character in a filename echoed back in a header, a mojibake
    // title in an `X-Plex-*` round-trip — collapsed the WHOLE header block to "", left
    // `status` at 0, and made the `status < 200` check below close a perfectly good 200 and
    // report it as a transport failure. The bytes we actually care about are all ASCII, so
    // reading them as bytes costs nothing and cannot be poisoned from a distance.
    let hdr = &hs.buf[..hdr_end];
    if hdr.starts_with(b"HTTP/1.") {
        // `hdr[9..]` (a fixed index straight after "HTTP/1.x ") was also a panic: on the old
        // `&str` it split a multi-byte char whose bytes straddled index 9, and on a byte slice
        // it would still be an out-of-range index on a truncated line. `get` makes it total.
        let rest = hdr.get(9..).unwrap_or(&[]);
        let ndig = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        // RFC 9110 §15: status-code is exactly 3DIGIT. Requiring that (rather than folding a
        // digit run of any length) keeps the well-formed case bit-identical while making the
        // accumulate below unable to overflow. Anything else stays 0 — which is what the old
        // `parse().unwrap_or(0)` produced for a malformed line too, and 0 fails the check
        // below exactly as before.
        hs.status = if ndig == 3 {
            rest[..3]
                .iter()
                .fold(0 as c_int, |acc, &b| acc * 10 + (b - b'0') as c_int)
        } else {
            0
        };
    }
    if let Some(p) = find_ci(hdr, b"\r\ncontent-length:") {
        let v = &hdr[p + 17..];
        // Only spaces/tabs are skipped (the OWS the grammar allows after the colon), NOT the
        // `str::trim_start` of before, which also ate CR/LF and so could run on into the next
        // header line's value. Identical on well-formed input, where there is one space.
        let v = &v[v.iter().take_while(|b| **b == b' ' || **b == b'\t').count()..];
        let ndig = v.iter().take_while(|b| b.is_ascii_digit()).count();
        hs.content_length = std::str::from_utf8(&v[..ndig])
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(-1);
    }
    if header_is_chunked(hdr) {
        hs.chunked = 1;
    }
    // HTTP/1.0 `Connection: keep-alive` is unused. PMS is 1.1; offering 1.0 reuse would add a
    // handshake only if we ever saw that framing, which we have not.
    hs.keep_alive = i32::from(hdr.starts_with(b"HTTP/1.1") && !header_has_connection_close(hdr));
    remember_peer(hs, host_s, port);

    hs.bpos = hdr_end as c_int; // first body byte
    if hs.status < 200 || hs.status >= 300 {
        // The code is known exactly here. The typed deadline API returns it directly; legacy
        // callers still receive `-1`, and it also survives in the struct because `close_owned`
        // touches only the fd. A seek reopen remains a legacy caller and therefore still has
        // only the flat failure.
        //
        // `status=0` is not a code any server sent: it is what the parse above leaves when the
        // status line was not `HTTP/1.x` followed by exactly three digits.
        crate::log(&format!(
            "stream: {method} {} status={}",
            log_endpoint(path_s),
            hs.status
        ));
        let error = if hs.status == 0 {
            HttpOpenError::Transport
        } else {
            HttpOpenError::Status(hs.status)
        };
        close_owned(hs);
        return Err(error);
    }
    if hs.content_length == 0 && hs.chunked == 0 {
        finish_body(hs);
    }
    if restore_media_timeouts {
        // The absolute open snapshot and its short socket options belong only to
        // DNS/connect/send/headers. A paused candidate body is still a live media transfer;
        // restore the ordinary inactivity contract before returning it to AVIO.
        set_socket_timeouts(fd, MEDIA_RECV_TIMEOUT_MS, MEDIA_SEND_TIMEOUT_MS);
    }
    Ok(())
}

pub(crate) fn http_read(hs: *mut HttpStream, dst: *mut c_uchar, n: c_int) -> c_int {
    http_read_until(hs, dst, n, None, &mut NoCheckpoint)
}

/// [`http_read`] with an optional absolute wake. This is intentionally not expressed as a shorter
/// socket option: `SO_RCVTIMEO` remains an inactivity bound, while ABR composes a current
/// projection of its playhead-funded reserve and classifies whichever clock actually fired.
///
/// `checkpoint` is consulted only when this read would block for fresh bytes (buffered bytes and a
/// proven end are returned without asking); a stop returns [`HTTP_READ_STOPPED`] with the socket
/// and any partial chunk framing left as they were.
pub(crate) fn http_read_until(
    hs: *mut HttpStream,
    dst: *mut c_uchar,
    n: c_int,
    deadline: Option<Instant>,
    checkpoint: &mut dyn Checkpoint,
) -> c_int {
    if hs.is_null() || dst.is_null() || n <= 0 {
        return if n == 0 { 0 } else { -1 };
    }
    let mut pacer = Pacer::new(checkpoint);
    let pacer = &mut pacer;
    unsafe {
        let hs = &mut *hs;
        let n = n as usize;
        if hs.chunked != 0 {
            if hs.chunk_left < 0 {
                match hs_skip_chunked_trailers(hs, deadline, pacer) {
                    Ok(()) => {
                        finish_body(hs);
                        return 0;
                    }
                    Err(e) => {
                        hs.keep_alive = 0;
                        close_owned(hs);
                        return e;
                    }
                }
            }
            if hs.chunk_left <= 0 {
                match hs_next_chunk(hs, deadline, pacer) {
                    Ok(Some(0)) => match hs_skip_chunked_trailers(hs, deadline, pacer) {
                        Ok(()) => {
                            finish_body(hs);
                            return 0;
                        }
                        Err(e) => {
                            hs.keep_alive = 0;
                            close_owned(hs);
                            return e;
                        }
                    },
                    Ok(Some(cs)) => {
                        if cs < 0 {
                            hs.keep_alive = 0;
                            close_owned(hs);
                            return -1;
                        }
                        hs.chunk_left = cs;
                    }
                    Err(e) => return e,
                    Ok(None) => {
                        hs.keep_alive = 0;
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
                    std::ptr::copy_nonoverlapping(
                        hs.buf.as_ptr().add(hs.bpos as usize),
                        dst.add(got),
                        take,
                    );
                    hs.bpos += take as c_int;
                    got += take;
                } else if hs.fd() >= 0 {
                    let r = recv_until(
                        hs.fd(),
                        dst.add(got) as *mut c_void,
                        want - got,
                        deadline,
                        pacer,
                    );
                    if r < 0 {
                        if retry_interrupted_recv(r, errno()) {
                            continue;
                        }
                        if got == 0 {
                            return r as c_int;
                        }
                        break;
                    }
                    if r == 0 {
                        close_owned(hs);
                        if got == 0 {
                            return -1;
                        }
                        break;
                    }
                    got += r as usize;
                } else {
                    break;
                }
            }
            hs.chunk_left -= got as i64;
            hs.consumed += got as i64;
            return if got > 0 {
                got as c_int
            } else {
                hs_closed_or_idle_read_result(hs)
            };
        }
        if hs.fd() < 0 && (hs.bpos as usize) >= (hs.blen as usize) {
            return hs_closed_or_idle_read_result(hs);
        }
        if hs.content_length >= 0 && hs.consumed >= hs.content_length {
            finish_body(hs);
            return 0;
        }
        let mut n = n;
        if hs.content_length >= 0 {
            let remain = (hs.content_length - hs.consumed).max(0) as usize;
            if remain == 0 {
                finish_body(hs);
                return 0;
            }
            n = n.min(remain);
        }
        // serve buffered body first
        if (hs.bpos as usize) < (hs.blen as usize) {
            let avail = hs.blen as usize - hs.bpos as usize;
            let take = std::cmp::min(avail, n);
            std::ptr::copy_nonoverlapping(hs.buf.as_ptr().add(hs.bpos as usize), dst, take);
            hs.bpos += take as c_int;
            hs.consumed += take as i64;
            if hs.content_length >= 0 && hs.consumed >= hs.content_length {
                finish_body(hs);
            }
            return take as c_int;
        }
        if hs.fd() < 0 {
            return hs_closed_or_idle_read_result(hs);
        }
        // Already-buffered response bytes and an already-proven EOF/completion are facts from the
        // transport before this call began. Only a read which would perform fresh I/O can be
        // stopped by the caller's clock; checking it earlier retrospectively relabelled a complete
        // response when its consumer was descheduled across the boundary.
        if deadline.is_some_and(|at| Instant::now() >= at) {
            return HTTP_READ_DEADLINE;
        }
        loop {
            let r = recv_until(hs.fd(), dst as *mut c_void, n, deadline, pacer);
            if r < 0 {
                if retry_interrupted_recv(r, errno()) {
                    continue;
                }
                return r as c_int;
            }
            if r == 0 {
                let short = hs.content_length >= 0 && hs.consumed < hs.content_length;
                close_owned(hs);
                return if short { -1 } else { 0 };
            }
            hs.consumed += r as i64;
            if hs.content_length >= 0 && hs.consumed >= hs.content_length {
                finish_body(hs);
            }
            return r as c_int;
        }
    }
}

/// Close the socket, once. Only the OWNING thread (the one doing the reads) may call this,
/// or the main thread AFTER that worker has been joined — a `close` racing a live reader frees
/// the fd number for another thread's `socket()` to claim. To interrupt a reader that is still
/// running, use [`http_shutdown`].
unsafe fn close_owned(hs: &HttpStream) {
    let _gate = FD_GATE.lock().unwrap_or_else(|e| e.into_inner());
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

pub(crate) fn http_body_done(hs: *const HttpStream) -> bool {
    if hs.is_null() {
        return true;
    }
    unsafe { (*hs).body_is_done() }
}

/// **What a transport holds of the current response body beyond what its reader has taken** —
/// the one question `ff.rs`'s acquisition asks both transports before it may abandon a fetch.
/// A body is complete when it is RECEIVED, not when FFmpeg has read it: PMS bursts a paused
/// remainder, and a burst sitting in a buffer is already paid for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BodyReceipt {
    /// Body bytes received and not yet read — never framing, never past a declared length.
    pub(crate) ahead: i64,
    /// The transport proved the body's end: every declared byte, the chunked terminator, or a
    /// successful transfer. Never a failure, which is not an end.
    pub(crate) finished: bool,
    /// Answering took a transfer step (curl's `perform`): bytes were received — on https,
    /// decrypted — by the query itself, and the read that later copies them out will not see
    /// that work. The caller counts the query's time as body transfer time exactly then.
    pub(crate) stepped: bool,
}

/// [`BodyReceipt`] for the plaintext socket. `recv` reads straight into the caller's buffer, so
/// what it holds ahead is the header block's leftover bytes plus the kernel's receive queue
/// (`FIONREAD`). A chunked body counts nothing ahead: its buffered bytes are interleaved with
/// framing, and its end is the terminator [`finish_body`] records.
pub(crate) fn http_body_receipt(hs: *const HttpStream) -> BodyReceipt {
    let nothing = BodyReceipt {
        ahead: 0,
        finished: false,
        stepped: false,
    };
    if hs.is_null() {
        return nothing;
    }
    let hs = unsafe { &*hs };
    if hs.body_is_done() {
        return BodyReceipt {
            ahead: 0,
            finished: true,
            stepped: false,
        };
    }
    if hs.chunked != 0 {
        return nothing;
    }
    let buffered = (hs.blen - hs.bpos).max(0) as i64;
    let fd = hs.fd();
    let mut queued: c_int = 0;
    if fd < 0 || unsafe { libc::ioctl(fd, libc::FIONREAD as _, &mut queued) } < 0 {
        queued = 0;
    }
    let mut ahead = buffered + queued.max(0) as i64;
    if hs.content_length >= 0 {
        ahead = ahead.min((hs.content_length - hs.consumed).max(0));
    }
    BodyReceipt {
        ahead,
        finished: false,
        stepped: false,
    }
}

unsafe fn hs_compact_buf(hs: &mut HttpStream) {
    if hs.bpos <= 0 {
        return;
    }
    let n = (hs.blen - hs.bpos) as usize;
    if n > 0 {
        hs.buf.copy_within(hs.bpos as usize..hs.blen as usize, 0);
    }
    hs.blen = n as c_int;
    hs.bpos = 0;
}

unsafe fn hs_poll_in(fd: c_int) -> bool {
    if fd < 0 {
        return false;
    }
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    libc::poll(&mut pfd, 1, 0) > 0 && pfd.revents & libc::POLLIN != 0
}

/// `>0` bytes, `0` peer FIN, `-1` transport error, `-2` nothing ready.
unsafe fn hs_recv_dontwait(fd: c_int, dst: &mut [u8]) -> c_int {
    if dst.is_empty() || fd < 0 {
        return -2;
    }
    let r = libc::recv(
        fd,
        dst.as_mut_ptr() as *mut c_void,
        dst.len(),
        libc::MSG_DONTWAIT,
    );
    if r < 0 {
        let e = errno();
        if e == libc::EAGAIN || e == libc::EWOULDBLOCK || e == libc::EINTR {
            return -2;
        }
        return -1;
    }
    r as c_int
}

unsafe fn hs_fill_buf_dontwait(hs: &mut HttpStream) -> c_int {
    hs_compact_buf(hs);
    let space = hs.buf.len().saturating_sub(hs.blen as usize);
    if space == 0 {
        return 0;
    }
    let fd = hs.fd();
    if !hs_poll_in(fd) {
        return 0;
    }
    let r = hs_recv_dontwait(fd, &mut hs.buf[hs.blen as usize..]);
    if r > 0 {
        hs.blen += r;
        return r;
    }
    if r == 0 {
        close_owned(hs);
        return 0;
    }
    if r == -2 {
        return 0;
    }
    r
}

/// True when `buf` holds a chunk-size line, not merely the previous chunk's trailing CRLF.
/// Drain must not call [`hs_next_chunk`] until this is true: that helper `recv`s after skipping
/// leftover CRLF, and a delayed next size line would sit on `SO_RCVTIMEO`.
fn hs_chunk_size_line_ready(hs: &HttpStream) -> bool {
    let start = hs.bpos as usize;
    let end = hs.blen as usize;
    if start >= end {
        return false;
    }
    let buf = &hs.buf[start..end];
    let Some(i) = buf.iter().position(|&b| b != b'\r' && b != b'\n') else {
        return false;
    };
    buf[i..].contains(&b'\n')
}

fn hs_body_incomplete(hs: &HttpStream) -> bool {
    if hs.body_done != 0 {
        return false;
    }
    if hs.chunked != 0 {
        return true;
    }
    hs.content_length >= 0 && hs.consumed < hs.content_length
}

fn hs_closed_or_idle_read_result(hs: &HttpStream) -> c_int {
    if hs.fd() < 0 && hs_body_incomplete(hs) {
        -1
    } else if hs.fd() < 0 {
        0
    } else {
        -1
    }
}

/// Consume chunked trailers from bytes already queued, without a blocking `recv`.
///
/// `hs_skip_chunked_trailers` with `deadline=now` never reads the socket (`recv_until` returns
/// `HTTP_READ_DEADLINE` before `poll`). Fill dontwait first, same as the next chunk-size line,
/// so a trailer split across two TCP fragments can complete on a later drain.
/// Returns `0` when the body is done or nothing more is ready, or a negative transport error.
unsafe fn hs_finish_chunked_trailers_dontwait(hs: &mut HttpStream) -> c_int {
    let filled = hs_fill_buf_dontwait(hs);
    if filled < 0 {
        return filled;
    }
    match hs_skip_chunked_trailers(hs, Some(Instant::now()), &mut Pacer::new(&mut NoCheckpoint)) {
        Ok(()) => {
            finish_body(hs);
            0
        }
        Err(e) if e == HTTP_READ_DEADLINE => 0,
        Err(e) => {
            hs.keep_alive = 0;
            close_owned(hs);
            e
        }
    }
}

/// Copy bytes already waiting on the socket into `dst` without blocking.
///
/// Original playback parks the demuxer in `aq_push` on the same thread as AVIO. A blocking
/// `recv` there would freeze the TCP window; this is the drain that keeps the window open.
/// Payload copies already-buffered bytes plus at most one `MSG_DONTWAIT` recv. Trailer skip
/// may add one more dontwait fill. Never `http_read`, never `SO_RCVTIMEO`.
/// Returns bytes copied, `0` when the peer has nothing ready, or a negative transport error.
pub(crate) fn http_drain_available(hs: *mut HttpStream, dst: &mut [u8]) -> c_int {
    if hs.is_null() || dst.is_empty() {
        return 0;
    }
    unsafe {
        let hs = &mut *hs;
        if hs.body_done != 0 {
            return 0;
        }
        if hs.fd() < 0 && (hs.bpos as usize) >= (hs.blen as usize) {
            return hs_closed_or_idle_read_result(hs);
        }
        if hs.content_length >= 0 && hs.consumed >= hs.content_length {
            finish_body(hs);
            return 0;
        }
        if hs.chunked != 0 && hs.chunk_left < 0 {
            return hs_finish_chunked_trailers_dontwait(hs);
        }
        if hs.chunked != 0 && hs.chunk_left == 0 {
            if !hs_chunk_size_line_ready(hs) {
                let filled = hs_fill_buf_dontwait(hs);
                if filled < 0 {
                    return filled;
                }
                if !hs_chunk_size_line_ready(hs) {
                    if hs.fd() < 0 {
                        return hs_closed_or_idle_read_result(hs);
                    }
                    return 0;
                }
            }
            // Size line is in `buf`. Deadline-now keeps `hs_next_chunk` from `recv`.
            match hs_next_chunk(hs, Some(Instant::now()), &mut Pacer::new(&mut NoCheckpoint)) {
                Ok(Some(0)) => {
                    hs.chunk_left = -1;
                    return hs_finish_chunked_trailers_dontwait(hs);
                }
                Ok(Some(cs)) => {
                    if cs < 0 {
                        hs.keep_alive = 0;
                        close_owned(hs);
                        return -1;
                    }
                    hs.chunk_left = cs;
                }
                Ok(None) => {
                    hs.keep_alive = 0;
                    close_owned(hs);
                    return -1;
                }
                Err(e) if e == HTTP_READ_DEADLINE => return 0,
                Err(e) => return e,
            }
        }
        let remain = if hs.chunked != 0 {
            hs.chunk_left.max(0) as usize
        } else if hs.content_length >= 0 {
            (hs.content_length - hs.consumed).max(0) as usize
        } else {
            dst.len()
        };
        if remain == 0 {
            if hs.chunked == 0 {
                finish_body(hs);
            }
            return 0;
        }
        let want = dst.len().min(remain);
        let mut got = 0usize;
        let avail = (hs.blen as usize).saturating_sub(hs.bpos as usize);
        if avail > 0 {
            let take = avail.min(want);
            dst[..take].copy_from_slice(&hs.buf[hs.bpos as usize..hs.bpos as usize + take]);
            hs.bpos += take as c_int;
            got = take;
        }
        if got < want {
            let fd = hs.fd();
            if hs_poll_in(fd) {
                let r = hs_recv_dontwait(fd, &mut dst[got..want]);
                if r == -1 {
                    if got == 0 {
                        return -1;
                    }
                } else if r == 0 {
                    close_owned(hs);
                    if got == 0 {
                        return hs_closed_or_idle_read_result(hs);
                    }
                } else if r > 0 {
                    got += r as usize;
                }
            }
        }
        if got == 0 {
            return 0;
        }
        if hs.chunked != 0 {
            hs.chunk_left -= got as i64;
        }
        hs.consumed += got as i64;
        if hs.chunked == 0 && hs.content_length >= 0 && hs.consumed >= hs.content_length {
            finish_body(hs);
        }
        got as c_int
    }
}

/// Interrupt a read in progress WITHOUT closing: `shutdown(2)` wakes a peer blocked in `recv`
/// (which `close(2)` does not — that was the 15 s freeze on BACK during a stall) and leaves the
/// descriptor allocated, so its number cannot be recycled into another thread's socket while
/// the reader is still touching it. The reader then sees EOF and closes it itself.
///
/// It also LATCHES the interrupt, unconditionally — including when the fd is already -1, which is
/// the point. A `connect_any` walk between two attempts holds no descriptor, so a teardown landing
/// in that window has nothing to shut down and would otherwise be lost entirely; the latch is what
/// stops the next address from being dialled. See [`HttpStream::interrupted`].
pub(crate) fn http_shutdown(hs: *mut HttpStream) {
    if hs.is_null() {
        return;
    }
    let _gate = FD_GATE.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        (*hs).interrupted.store(1, Ordering::Release);
        // Re-read UNDER the gate. The `take_fd` swap alone makes exactly one caller close, but it
        // does not make this pair atomic: read the number, lose the CPU, and by the time the
        // syscall runs the owner may have closed it and another thread's `socket()` may have been
        // handed the same number — so the interrupt lands on an unrelated, healthy connection.
        // Holding the gate across BOTH the read and the syscall is what removes that window,
        // because the only close is under the same gate.
        let fd = (*hs).fd();
        if fd >= 0 {
            libc::shutdown(fd, libc::SHUT_RDWR);
        }
    }
}

/// Serialises `shutdown(2)` against `close(2)` on a stream's descriptor — see [`http_shutdown`].
///
/// One process-wide gate rather than a field on `HttpStream`: the struct is `#[repr(C)]` and built
/// by zeroing a `Box` (`http_stream_boxed`), so a `Mutex` field would have to be constructed
/// instead of zeroed, and a zeroed `Mutex` is not a thing to rely on. Contention is not a concern
/// either — between them these two run a handful of times per playback, and each holds the gate for
/// exactly one syscall on an already-open descriptor. Deliberately NOT taken by `set_fd`/`fd()`:
/// `hs_getb` loads the fd per byte on the chunked path, which is why it stays an atomic.

// The three one-shot wrappers that lived here — `http_get`, `http_put`, `http_post` — are GONE.
//
// They were the Plex control plane's transport, and each folded away half of the answer:
// `http_get`/`http_post` returned `Option<Vec<u8>>`, collapsing every non-2xx into the same `None`
// a refused connection produces, and `http_put` returned the status with the body dropped. That
// collapse is precisely what `plex::probe::Outcome` exists to prevent — a final `401` after the
// direct/relay candidates settle is a TOKEN or access-policy problem, while a refusal is a
// REACHABILITY one, and reporting the first as the second sends a user to their friend's router.
// `auth::get_identity` had already had to hand-roll its own open/read/close for that reason.
//
// Their replacement is `crate::http`, the one door that dispatches a control-plane request on its
// origin's SCHEME — this module for plaintext, `net.rs`/libcurl for TLS — and returns the status
// AND the body over either. Its plaintext arm is the same composition the wrappers were, so
// nothing about the bytes on the wire moved; `note_short_body` above went `pub(crate)` to keep the
// short-body notice on that path.

/// A boxed HttpStream in the CLOSED state (fd = -1, never 0 — a stray close on a zeroed box
/// would take stdin), so http_close is a no-op until
/// http_open assigns a real fd. The player engine pre-allocates the demux/cue sockets
/// before the worker threads open them; a plain zeroed box leaves fd = 0, and a
/// teardown before (or without) http_open would then close(0) the process's stdin —
/// and free fd 0 for a later socket() to reuse and be wrongly closed. The zero fill is written
/// directly into the heap allocation: spelling this as `Box::new(mem::zeroed())` materialises a
/// 64 KiB temporary on debug-build worker stacks before moving it into the box.
pub(crate) fn http_stream_boxed() -> Box<HttpStream> {
    let mut slot = Box::<HttpStream>::new_uninit();
    // SAFETY: every HttpStream field has an all-zero valid representation (integers, bytes and
    // AtomicI32), and the allocation is exclusively owned and still MaybeUninit here. Writing the
    // bytes through its heap pointer avoids constructing the large value on this worker's stack.
    unsafe { std::ptr::write_bytes(slot.as_mut_ptr(), 0, 1) };
    // SAFETY: the preceding write initialized every byte of the allocation.
    let hs = unsafe { slot.assume_init() };
    hs.set_fd(-1);
    hs
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
#[path = "stream_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "stream_deadline_connect_tests.rs"]
mod deadline_connect_tests;

#[cfg(test)]
#[path = "stream_header_body_tests.rs"]
mod header_body_tests;

#[cfg(test)]
#[path = "stream_ipv6_resolution_tests.rs"]
mod ipv6_resolution_tests;

#[cfg(test)]
#[path = "stream_keepalive_reuse_tests.rs"]
mod keepalive_reuse_tests;

#[cfg(test)]
#[path = "stream_chunked_drain_tests.rs"]
mod chunked_drain_tests;

#[cfg(test)]
#[path = "stream_checkpoint_tests.rs"]
mod checkpoint_tests;
