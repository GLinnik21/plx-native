//! Blocking HTTP/1.1 over a raw TCP socket (was src/stream.c). Callers
//! (posters/pms/player) allocate an `HttpStream` and pass `&hs`; this operates on it
//! in place. Header/chunk parsing is bounds-checked (no OOB).
//!
//! The host is a **name or an address literal of either family**, resolved through `getaddrinfo`
//! ([`resolve`]) and dialled down the whole returned chain ([`connect_any`]). It used to be four
//! decimal octets parsed by hand into a `sockaddr_in`, which made a hostname and every IPv6 server
//! not "degraded" but impossible — the shape of the gap LG's checklist #43 CASE2 asks about.
//! What is still missing here is TLS: this arm stays cleartext. [`crate::http`] sends an `https://`
//! control-plane origin through [`crate::net`], while MEDIA bytes use [`crate::curlio`], a
//! libcurl-multi pull source under the same `ff.rs` AVIO that this module serves for `http`.
//!
//! Failures the return value cannot carry are reported to the event log instead: a non-2xx
//! response (in [`http_open`], where the code is known and the socket is about to close) and a
//! body that came up short of its `Content-Length` ([`short_body_line`]). Neither changes what any
//! function returns — a truncated body is still handed to its caller — and no line built here
//! carries a query string, which is [`log_endpoint`]'s rule and the reason it exists.
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uchar, c_void};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

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
    /// "A teardown asked this stream to stop" — set by [`http_shutdown`], cleared at the top of
    /// every [`http_open`].
    ///
    /// It exists because resolving gave the open something it never had: a NEXT address to try
    /// after a failed `connect`. A handshake aborted by `shutdown(2)` and an address that is simply
    /// dead are the same value at `connect`'s return, so without this latch a teardown fired during
    /// attempt 1 of 2 is answered by dialling attempt 2 — the interrupt silently consumed, and a
    /// brand-new connection handed back to a caller that was being torn down. Reading the fd cannot
    /// stand in for it: between two attempts the fd is legitimately -1.
    interrupted: AtomicI32,
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
    hay.windows(needle.len()).position(|w| w.eq_ignore_ascii_case(needle))
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
    /// Reset every field EXCEPT `fd`. `http_open` used to `write_bytes`-memset the whole
    /// struct, which wrote the ATOMIC fd non-atomically — and momentarily as 0, i.e. stdin —
    /// while another thread could be loading it in `http_shutdown`. `fd` is reset separately
    /// through its atomic store.
    ///
    /// `buf` is deliberately NOT cleared: it is only ever read within `[bpos, blen)`, both of
    /// which are reset here, so zeroing 64 KB on every request was pure cost.
    ///
    /// `interrupted` IS reset here, and through its atomic store like `fd` — it is per-request
    /// state, and the window it leaves open is the correct one: a `http_shutdown` landing between
    /// this reset and the connect loop is a teardown of the open now starting, which is exactly
    /// what the latch is for.
    #[inline]
    fn reset_fields(&mut self) {
        self.interrupted.store(0, Ordering::Release);
        self.blen = 0;
        self.bpos = 0;
        self.content_length = -1;
        self.consumed = 0;
        self.status = 0;
        self.chunked = 0;
        self.chunk_left = 0;
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
/// `ui::stats` applies to the diagnostics panel. The endpoint on its own is what makes a line
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
fn short_body_line(method: &str, path: &str, consumed: i64, content_length: i64,
                   chunked: bool, recv_err: bool) -> Option<String> {
    let sized = !chunked && content_length >= 0;
    if sized && consumed >= content_length {
        return None; // whole, by the only measure the response gave us
    }
    if !sized && !recv_err {
        return None; // nothing to fall short of, and the socket ended cleanly
    }
    let want = if sized { content_length.to_string() } else { "?".to_string() };
    let why = if recv_err { "recv error" } else { "EOF" };
    Some(format!("stream: {method} {} SHORT BODY got={consumed} want={want} ({why})",
                 log_endpoint(path)))
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
    if let Some(line) = short_body_line(method, path, hs.consumed, hs.content_length,
                                        hs.chunked != 0, recv_err) {
        crate::log(&line);
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

/// Strip the optional whitespace (RFC 9110 §5.6.3's `OWS` — spaces and horizontal tabs only) from
/// both ends of a header token.
fn trim_ows(v: &[u8]) -> &[u8] {
    let a = v.iter().take_while(|b| **b == b' ' || **b == b'\t').count();
    let v = &v[a..];
    let b = v.iter().rev().take_while(|b| **b == b' ' || **b == b'\t').count();
    &v[..v.len() - b]
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
        let end = hdr[vs..].iter().position(|&b| b == b'\r' || b == b'\n').map_or(hdr.len(), |i| vs + i);
        if hdr[vs..end].split(|&b| b == b',').any(|t| trim_ows(t).eq_ignore_ascii_case(b"chunked")) {
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

/// `connect(2)` bounded by `timeout_ms`. Flips the socket to non-blocking for the handshake,
/// waits on `poll(POLLOUT)`, then reads `SO_ERROR` to learn the real outcome (a writable socket
/// does NOT mean success), and restores blocking mode so every read path below is unchanged.
/// Returns 0 on a connected socket, -1 otherwise. The caller owns closing `fd`.
///
/// The address arrives as a `(*const sockaddr, socklen_t)` pair rather than a `&sockaddr_in`
/// because it now comes out of `getaddrinfo` and may be a `sockaddr_in6`: the pair IS the
/// `addrinfo`'s own `ai_addr`/`ai_addrlen`, forwarded without a copy and without this function
/// having to know or test the family. A non-positive `timeout_ms` (the chain budget already spent)
/// is clamped to 0 rather than passed on — `poll` reads a NEGATIVE timeout as "block forever",
/// which would turn an exhausted budget into the unbounded wait this whole function removes.
unsafe fn connect_timeout(fd: c_int, sa: *const libc::sockaddr, salen: libc::socklen_t,
                          timeout_ms: c_int) -> c_int {
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
    if libc::connect(fd, sa, salen) == 0 {
        return restore(0); // connected immediately (loopback / same host)
    }
    if errno() != libc::EINPROGRESS {
        return restore(-1);
    }
    let mut pfd = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
    // EINTR must not be treated as a timeout: retry with the remaining budget.
    let mut left = timeout_ms.max(0); // never negative — that is `poll`'s "wait forever"

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

/// An address list from `getaddrinfo`, freed on drop — including on the early return every failed
/// `connect` in [`connect_any`] can take. Only ever constructed around a NON-NULL head, because
/// `freeaddrinfo(NULL)` is not a documented no-op the way `free(NULL)` is.
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
        | if is_numeric_host(host) { libc::AI_NUMERICHOST } else { libc::AI_ADDRCONFIG };
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    if libc::getaddrinfo(node.as_ptr(), service.as_ptr(), &hints, &mut res) != 0 || res.is_null() {
        return None;
    }
    Some(AddrList { head: res })
}

/// Dial down the address chain until one answers, within `budget_ms` for the WHOLE walk. Returns
/// the connected fd — also left PUBLISHED in `hs` — or -1 with `hs` closed.
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
unsafe fn connect_any(hs: &HttpStream, head: *const libc::addrinfo, budget_ms: c_int) -> c_int {
    let started = std::time::Instant::now();
    let mut ai = head;
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
        if connect_timeout(fd, a.ai_addr, a.ai_addrlen, budget_ms.max(0) - spent) == 0 {
            return fd;
        }
        close_owned(hs); // published, so it must be RETIRED
        if hs.interrupted() {
            break; // a teardown, not a dead address: do not answer it by dialling the next one
        }
    }
    -1
}

pub(crate) fn http_open(hs: *mut HttpStream, host: *const c_char, port: c_int,
                            path: *const c_char, extra: *const c_char, method: &str) -> c_int {
    if hs.is_null() || host.is_null() || path.is_null() {
        return -1;
    }
    unsafe {
        let hs = &mut *hs;
        hs.reset_fields();
        hs.set_fd(-1);

        let host_s = CStr::from_ptr(host).to_string_lossy();
        let path_s = CStr::from_ptr(path).to_string_lossy();

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
                crate::log(&format!("stream: {method} {} DNS FAILED host={host_s}",
                                    log_endpoint(&path_s)));
                return -1;
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
            return -1; // torn down while resolving; no descriptor was ever created
        }
        let fd = connect_any(hs, list.head, CONNECT_TIMEOUT_MS);
        if fd < 0 {
            return -1; // every attempt retired its own fd; `hs` is closed
        }
        if hs.interrupted() {
            close_owned(hs); // connected through a teardown: retire it rather than send on it
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
        let extra_s: String = if extra.is_null() {
            String::new()
        } else {
            CStr::from_ptr(extra).to_string_lossy().into_owned()
        };
        let accept = if extra_s.to_ascii_lowercase().contains("accept:") { "" } else { "Accept: */*\r\n" };
        // `Host:` is the ORIGIN, never the address `connect_any` reached — see `host_header`.
        let host_hdr = host_header(&host_s, port);
        let req = format!(
            "{method} {path_s} HTTP/1.1\r\nHost: {host_hdr}\r\nUser-Agent: plxnative/0.1\r\n{accept}{extra_s}Connection: close\r\n\r\n"
        );
        let bytes = req.as_bytes();
        let mut off = 0usize;
        while off < bytes.len() {
            let w = libc::send(fd, bytes[off..].as_ptr() as *const c_void, bytes.len() - off, 0);
            if w <= 0 {
                close_owned(hs);
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
            // r == 0 is also how an interrupted open surfaces: `http_shutdown` wakes this
            // recv with EOF, so a teardown mid-header costs one syscall, not 15 s of SO_RCVTIMEO.
            if r <= 0 {
                close_owned(hs);
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
                close_owned(hs);
                return -1;
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
                rest[..3].iter().fold(0 as c_int, |acc, &b| acc * 10 + (b - b'0') as c_int)
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
            hs.content_length = std::str::from_utf8(&v[..ndig]).ok()
                .and_then(|s| s.parse::<i64>().ok()).unwrap_or(-1);
        }
        if header_is_chunked(hdr) {
            hs.chunked = 1;
        }

        hs.bpos = hdr_end as c_int; // first body byte
        if hs.status < 200 || hs.status >= 300 {
            // The code is known exactly here and the return value cannot carry it: the open
            // reports a flat `-1` whatever went wrong. It does survive in the struct —
            // `close_owned` touches only the fd — and both current consumers read it: the
            // plaintext control arm in `crate::http` returns the status with the body, while the
            // AVIO open uses it to diagnose a refused media request. A seek reopen still has only
            // the flat failure, because it has no HTTP response surface to return through.
            //
            // `status=0` is not a code any server sent: it is what the parse above leaves when the
            // status line was not `HTTP/1.x` followed by exactly three digits.
            crate::log(&format!("stream: {method} {} status={}", log_endpoint(&path_s), hs.status));
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
// collapse is precisely what `plex::probe::Outcome` exists to prevent — a `401` is a TOKEN problem
// (every other address of that server answers identically) while a refusal is a REACHABILITY one,
// and reporting the first as the second sends a user to look at their friend's router.
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

    /// One `GET /x` at a loopback port, through the door the control plane actually uses.
    ///
    /// These assertions used to call `stream::http_get`, and they outlived it: what they grade —
    /// that a chunked body is decoded on the READ path, and that a truncated one still reaches its
    /// caller — is a property of `http_open`/`http_read`, not of the wrapper that wrapped them. Now
    /// they grade it through `crate::http`'s plaintext arm, i.e. through the composition that runs
    /// in production, which is strictly more than the wrapper could say.
    fn loopback_get(port: u16) -> Option<crate::http::Reply> {
        let o = crate::plex::Origin::http("127.0.0.1", port as i32);
        crate::http::request(&o, "/x", crate::http::Method::Get, &[])
    }

    /// Descriptors currently open in this process. `/dev/fd` works on both macOS and Linux;
    /// `read_dir` opens one itself, but that is constant between two calls.
    fn open_fd_count() -> usize {
        std::fs::read_dir("/dev/fd").map(|d| d.count()).unwrap_or(0)
    }


    fn sockaddr(ip: [u8; 4], port: u16) -> libc::sockaddr_in {
        let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        sa.sin_family = libc::AF_INET as libc::sa_family_t;
        sa.sin_port = port.to_be();
        sa.sin_addr.s_addr = u32::from_ne_bytes(ip);
        sa
    }

    fn sockaddr6(ip: std::net::Ipv6Addr, port: u16) -> libc::sockaddr_in6 {
        let mut sa: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
        sa.sin6_family = libc::AF_INET6 as libc::sa_family_t;
        sa.sin6_port = port.to_be();
        sa.sin6_addr = libc::in6_addr { s6_addr: ip.octets() };
        sa
    }

    /// `connect_timeout` for a v4 address. The real function takes the `(*const sockaddr,
    /// socklen_t)` pair straight out of an `addrinfo`, because the address may now be either
    /// family; the tests below predate that and say what they mean with a `sockaddr_in`.
    unsafe fn connect_v4(fd: c_int, sa: &libc::sockaddr_in, timeout_ms: c_int) -> c_int {
        connect_timeout(fd, sa as *const _ as *const libc::sockaddr,
                        std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t, timeout_ms)
    }

    unsafe fn connect_v6(fd: c_int, sa: &libc::sockaddr_in6, timeout_ms: c_int) -> c_int {
        connect_timeout(fd, sa as *const _ as *const libc::sockaddr,
                        std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t, timeout_ms)
    }

    /// One `addrinfo` node pointing at a caller-owned `sockaddr`, for testing [`connect_any`]'s
    /// walk without a resolver in the loop. The chain a real DNS answer produces is not something a
    /// test can arrange on demand — which address a name yields, and in what order, is the
    /// machine's business — so the walk is graded on a list built by hand instead.
    fn ainfo(family: c_int, sa: *mut libc::sockaddr, len: libc::socklen_t,
             next: *mut libc::addrinfo) -> libc::addrinfo {
        let mut ai: libc::addrinfo = unsafe { std::mem::zeroed() };
        ai.ai_family = family;
        ai.ai_socktype = libc::SOCK_STREAM;
        ai.ai_addr = sa;
        ai.ai_addrlen = len;
        ai.ai_next = next;
        ai
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
        let r = unsafe { connect_v4(fd, &sa, 300) };
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
        let r = unsafe { connect_v4(fd, &sa, 5_000) };
        let waited = t0.elapsed();
        unsafe { libc::close(fd) };
        assert_eq!(r, -1, "SO_ERROR must be consulted — a writable socket is not a connected one");
        assert!(waited.as_millis() < 2_000, "a refusal should be immediate, waited {waited:?}");
    }

    /// A failed `http_open` must leave the stream CLOSED (fd = -1) and leak no descriptor,
    /// whichever early exit it took. This is what makes `http_stream_boxed`'s fd = -1 contract
    /// hold end-to-end, and it is the invariant a future interruptible-open design has to keep.
    #[test]
    fn every_failed_open_retires_its_fd_and_leaks_nothing() {
        let ip_refused = std::ffi::CString::new("127.0.0.1").unwrap(); // nothing listens on :1
        let path = std::ffi::CString::new("/x").unwrap();

        // The first case used to be the dotted quad `999.1.2.3`, rejected by the hand parse after
        // `socket()`. That parse is gone, and sending the same string on would have made this
        // OFFLINE suite do a DNS lookup: `999.1.2.3` is not an address, so it goes to the resolver
        // as a NAME — where a network with NXDOMAIN hijacking answers it with a real web server
        // (and the open then SUCCEEDS, failing this test), and a network with a dead resolver
        // spends glibc's whole `timeout × attempts × nameservers` budget inside a ~0.3 s suite.
        // An out-of-range port is the same early exit — refused in `resolve`, before any socket —
        // and it cannot leave the machine.
        for (label, ip, port) in [
            ("port out of range", &ip_refused, 70_000),
            ("refused connection", &ip_refused, 1),
        ] {
            let mut hs = http_stream_boxed();
            let rv = http_open(&mut *hs, ip.as_ptr(), port, path.as_ptr(), std::ptr::null(), "GET");
            assert_eq!(rv, -1, "{label}: open must fail");
            assert_eq!(hs.fd(), -1, "{label}: the fd must be retired, not left published");
        }

        // …and the descriptor is genuinely closed, not merely un-published.
        //
        // The slack is deliberately loose, because `open_fd_count` is PROCESS-wide and this suite
        // runs in parallel: the sibling socket tests (loopback listeners, the two `ff.rs` counting
        // accepts) hold descriptors open across this window, so a strict +2 made the assertion fire
        // on their scheduling rather than on a leak — it was already failing ~1 run in 6 before this
        // branch and got worse as the suite grew, which is a red gate that says nothing. What is
        // being detected is 32 leaked sockets; anything under a handful is other tests, and the two
        // are three quarters of an order of magnitude apart.
        let before = open_fd_count();
        for _ in 0..32 {
            let mut hs = http_stream_boxed();
            let _ = http_open(&mut *hs, ip_refused.as_ptr(), 1, path.as_ptr(), std::ptr::null(), "GET");
        }
        let after = open_fd_count();
        assert!(after <= before + 8, "failed opens leaked descriptors: {before} -> {after}");
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
        assert_eq!(unsafe { connect_v4(fd, &sa, 2_000) }, 0);
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

    /// The point of publishing the fd at `socket()`: an open that stalls is now interruptible.
    /// A listener that accepts and never answers puts `http_open` in its header `recv`, where it
    /// would otherwise sit out the full 15 s `SO_RCVTIMEO` — and the main thread waits on that in
    /// `teardown`'s join, which is the freeze this whole change exists to remove. The interrupt
    /// must also leave the stream RETIRED, not merely woken: a stale fd left in the atomic is one
    /// the next `http_shutdown` would shoot after the number had been recycled.
    #[test]
    fn an_open_stalled_in_the_header_read_is_interruptible() {
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().unwrap().port();
        let ip = std::ffi::CString::new("127.0.0.1").unwrap();
        let path = std::ffi::CString::new("/stall").unwrap();

        let mut hs = http_stream_boxed();
        let addr = (&mut *hs) as *mut HttpStream as usize; // raw ptr isn't Send; the box outlives the scope
        let t0 = Instant::now();
        let (rv, waited) = std::thread::scope(|sc| {
            let opener = sc.spawn(move || {
                let rv = http_open(addr as *mut HttpStream, ip.as_ptr(), port as c_int,
                                   path.as_ptr(), std::ptr::null(), "GET");
                (rv, t0.elapsed())
            });
            let _peer = srv.accept().expect("accept"); // held open, never written to
            std::thread::sleep(std::time::Duration::from_millis(200)); // let it reach the recv
            http_shutdown(addr as *mut HttpStream);
            opener.join().unwrap()
        });

        assert_eq!(rv, -1, "an interrupted open must report failure");
        assert!(waited.as_secs() < 3,
                "took {waited:?} — the open sat out SO_RCVTIMEO, so it was NOT interrupted");
        assert_eq!(hs.fd(), -1, "the interrupted open left its fd published — that is the stale \
                                 descriptor a later http_shutdown would shoot");
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
        let r = unsafe { connect_v4(fd, &sa, 2_000) };
        assert_eq!(r, 0, "a listening socket must connect");
        let fl = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
        assert_eq!(fl & libc::O_NONBLOCK, 0, "O_NONBLOCK leaked out of the handshake");
        unsafe { libc::close(fd) };
    }

    /// Answer ONE request with `resp` verbatim, then close; hands back the bound port and the
    /// server thread to join. `resp` is written as raw bytes precisely so a test can put things in
    /// a header that no `&str` could hold. The listener moves into the thread, so the socket is
    /// released once the response is out — which is also what gives the reader its EOF.
    fn one_shot_server(resp: Vec<u8>) -> (u16, std::thread::JoinHandle<()>) {
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
    fn open_against(port: u16) -> (Box<HttpStream>, c_int) {
        open_host_against("127.0.0.1", port)
    }

    /// …and the same for a host that is not the v4 loopback literal — a name, a bracketed v6
    /// literal, a bare one.
    fn open_host_against(host: &str, port: u16) -> (Box<HttpStream>, c_int) {
        let h = std::ffi::CString::new(host).unwrap();
        let path = std::ffi::CString::new("/x").unwrap();
        let mut hs = http_stream_boxed();
        let rv = http_open(&mut *hs, h.as_ptr(), port as c_int, path.as_ptr(), std::ptr::null(), "GET");
        (hs, rv)
    }

    /// A one-shot server that hands the REQUEST back to the test as well as answering it — the
    /// only way to grade a header we emit rather than one we parse.
    fn one_shot_echo(bind: &str, resp: Vec<u8>)
        -> std::io::Result<(u16, std::thread::JoinHandle<Vec<u8>>)> {
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
    fn host_line(req: &[u8]) -> String {
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
    fn v6_loopback_or_skip(what: &str) -> Option<std::net::TcpListener> {
        match std::net::TcpListener::bind("[::1]:0") {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!("SKIPPED {what}: this host has no usable IPv6 loopback ({e})");
                None
            }
        }
    }

    /// Regression: the header block was parsed as STRICT UTF-8 (`from_utf8(…).unwrap_or("")`), so a
    /// single non-UTF-8 byte anywhere in it — here a lone Latin-1 `0xE9` in a header value, which
    /// is exactly what a PMS echo of a filename or title produces — emptied the whole block. The
    /// status then stayed 0, and `http_open`'s `status < 200` check closed a perfectly good 200 and
    /// reported it as a transport failure: an unplayable item / a missing poster with a healthy
    /// server on the other end. The response is otherwise entirely well formed.
    #[test]
    fn one_non_utf8_byte_in_a_header_does_not_turn_a_200_into_a_failure() {
        let mut resp: Vec<u8> = Vec::new();
        resp.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Plex-Title: caf");
        resp.push(0xE9); // 'é' in Latin-1 — a lone 0xE9 is not valid UTF-8 in any position
        resp.extend_from_slice(b"\r\n\r\nhello");
        let (port, h) = one_shot_server(resp);

        let (mut hs, rv) = open_against(port);
        assert_eq!(rv, 0, "a 200 must open, whatever bytes the other headers carry");
        assert_eq!(hs.status, 200, "the status line is ASCII and was never in doubt");
        assert_eq!(hs.content_length, 5, "…and Content-Length must survive the same way");

        let mut body = Vec::new();
        let mut chunk = [0u8; 16];
        loop {
            let r = http_read(&mut *hs, chunk.as_mut_ptr(), chunk.len() as c_int);
            if r <= 0 {
                break;
            }
            body.extend_from_slice(&chunk[..r as usize]);
        }
        assert_eq!(body.as_slice(), b"hello", "the body must be delivered intact");
        http_close(&mut *hs);
        h.join().unwrap();
    }

    /// The status extraction indexed a fixed `[9..]`. On the old `&str` that panicked outright when
    /// a multi-byte character straddled index 9 — a panic inside `http_open`, which runs on the
    /// demux and poster workers as well as the main loop. A garbage status line must be REJECTED
    /// (status 0 → open fails), never fatal.
    #[test]
    fn a_status_line_that_straddles_the_status_offset_is_rejected_not_fatal() {
        // The 4-byte U+1F600 starts at index 7, so it OWNS index 9 — the old `&str[9..]` split it.
        let (port, h) = one_shot_server("HTTP/1.\u{1F600} 200 OK\r\n\r\n".as_bytes().to_vec());

        let (mut hs, rv) = open_against(port);
        assert_eq!(rv, -1, "an unparseable status line must fail the open");
        assert_eq!(hs.status, 0, "…with no status invented for it");
        assert_eq!(hs.fd(), -1, "a failed open retires its fd (see the leak test above)");
        http_close(&mut *hs); // already closed by the failure path; keeps the intent explicit
        h.join().unwrap();
    }

    /// Header field NAMES are case-insensitive (RFC 9110 §5.1) and PMS does not send one casing
    /// consistently. The old code got that from lowercasing the whole block; `find_ci` has to give
    /// it back, and — the part that matters to the caller — the offset it returns must index the
    /// ORIGINAL bytes, since the value is read from there. Tested directly rather than through a
    /// loopback round trip: it is a pure function, and every socket a test holds open is one the
    /// fd-leak test above can miscount while the two run in parallel.
    #[test]
    fn header_names_are_found_whatever_their_casing() {
        let hdr = b"HTTP/1.1 200 OK\r\nCONTENT-Length: 42\r\nTransfer-Encoding: chunked\r\n\r\n";
        let p = find_ci(hdr, b"\r\ncontent-length:").expect("a shouted header name must be found");
        assert_eq!(&hdr[p + 17..p + 20], b" 42", "the offset must index the ORIGINAL bytes");
        assert!(find_ci(hdr, b"\r\ntransfer-encoding: chunked").is_some());
        assert!(find_ci(hdr, b"\r\ncontent-range:").is_none(), "no false positives");
        assert!(find_ci(b"HT", b"\r\ncontent-length:").is_none(), "a needle longer than the hay");
    }

    /// The redaction rule these log lines rest on: what reaches the event log is the endpoint, and
    /// a query string never is. `with_token` is "the ONLY place `X-Plex-Token` is appended"
    /// (`plex/client.rs`'s own module doc) and it appends it to the QUERY, while the event log is
    /// what a user pastes into a public issue thread — so this is graded on the token being
    /// ABSENT, not on the split being pretty.
    #[test]
    fn a_logged_endpoint_drops_the_query_and_with_it_the_token() {
        let p = "/library/metadata/4/children?includeChildren=1&X-Plex-Token=aBcD1234xyzQ";
        assert_eq!(log_endpoint(p), "/library/metadata/4/children");
        assert!(!log_endpoint(p).contains("X-Plex-Token"), "the token reached the log line");
        assert!(!log_endpoint(p).contains("aBcD1234xyzQ"));
        // A poster path arrives with the token already in it (`Client::fetch_built`).
        let poster = "/photo/:/transcode?width=300&url=%2Flibrary%2F1&X-Plex-Token=aBcD1234xyzQ";
        assert_eq!(log_endpoint(poster), "/photo/:/transcode");
        assert_eq!(log_endpoint("/identity"), "/identity", "a path with no query is itself");
        assert_eq!(log_endpoint("?X-Plex-Token=t"), "", "a path that is nothing but a query");
    }

    /// The completeness test itself. A body that reached its `Content-Length` reports nothing; one
    /// that stopped short reports how far it got and what was owed, and names WHICH end it was —
    /// a mid-body `SO_RCVTIMEO` (recv error) reads nothing like a peer that closed early (EOF),
    /// which is why the read loops keep -1 and 0 apart rather than folding them into `r <= 0`.
    #[test]
    fn a_short_body_is_reported_and_a_complete_one_is_not() {
        assert_eq!(short_body_line("GET", "/hubs?X-Plex-Token=t", 5000, 5000, false, false), None,
                   "a body that reached its length is whole");
        assert_eq!(short_body_line("GET", "/hubs", 5001, 5000, false, false), None,
                   "…and one past it is not short either");

        let l = short_body_line("GET", "/hubs?X-Plex-Token=aBcD1234xyzQ", 900, 5000, false, false)
            .expect("a body 900 bytes into a 5000-byte response must be reported");
        assert!(l.contains("SHORT BODY got=900 want=5000"), "{l}");
        assert!(l.contains("/hubs") && !l.contains("aBcD1234xyzQ"), "the line leaked the query: {l}");
        assert!(l.contains("EOF"), "a clean end must not read as an error: {l}");

        let e = short_body_line("POST", "/playQueues", 900, 5000, false, true).expect("reported");
        assert!(e.contains("recv error"), "a recv error must be named as one: {e}");
        assert!(e.starts_with("stream: POST "), "the verb belongs on the line: {e}");

        // No length at all — a close-delimited body. A clean end is the ONLY end it has, so
        // silence; an error is still an error, with nothing to state as `want`.
        assert_eq!(short_body_line("GET", "/x", 900, -1, false, false), None);
        let u = short_body_line("GET", "/x", 900, -1, false, true).expect("reported");
        assert!(u.contains("got=900 want=? (recv error)"), "{u}");
    }

    /// Chunked framing has no `Content-Length` to fall short of, and `http_read`'s chunked branch
    /// counts DECODED bytes into `consumed` without ever reading the field — so the length test
    /// must not run there, INCLUDING for a server that sent both headers, where the two numbers
    /// are not the same quantity. Only a recv error can call a chunked transfer incomplete.
    #[test]
    fn a_chunked_response_cannot_report_a_short_body_on_length() {
        assert_eq!(short_body_line("GET", "/x", 900, -1, true, false), None,
                   "the ordinary chunked case: no length, clean end");
        assert_eq!(short_body_line("GET", "/x", 900, 5000, true, false), None,
                   "both headers present — the chunked framing wins, the length means nothing");
        let e = short_body_line("GET", "/x", 900, 5000, true, true).expect("reported");
        assert!(e.contains("want=?"), "a chunked transfer owes no stated length: {e}");
    }


    /// The v6 sibling of the two connect tests above, on a real AF_INET6 socket: a live `::1`
    /// listener connects and is handed back BLOCKING (every read path below depends on that, and
    /// `connect_timeout` restores the flags itself), and a `::1` port with no listener is refused
    /// at once rather than waited out. Neither is inferrable from the v4 pair — a family this file
    /// never opened before is exactly where a wrong `socklen_t` or a stray `sockaddr_in` cast shows
    /// up, and it shows up as a `connect` that fails for a reason nothing logs.
    #[test]
    fn a_v6_listener_connects_blocking_and_a_v6_refusal_is_immediate() {
        let Some(srv) = v6_loopback_or_skip("the AF_INET6 connect pair") else { return };
        let port = srv.local_addr().unwrap().port();

        let sa = sockaddr6(std::net::Ipv6Addr::LOCALHOST, port);
        let fd = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_STREAM, 0) };
        assert!(fd >= 0, "AF_INET6 sockets must be creatable — the loopback bound above");
        assert_eq!(unsafe { connect_v6(fd, &sa, 2_000) }, 0, "a listening ::1 socket must connect");
        let fl = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
        assert_eq!(fl & libc::O_NONBLOCK, 0, "O_NONBLOCK leaked out of the v6 handshake");
        unsafe { libc::close(fd) };

        let sa = sockaddr6(std::net::Ipv6Addr::LOCALHOST, 1); // nothing listens on ::1:1
        let fd = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_STREAM, 0) };
        let t0 = Instant::now();
        let r = unsafe { connect_v6(fd, &sa, 5_000) };
        let waited = t0.elapsed();
        unsafe { libc::close(fd) };
        assert_eq!(r, -1, "SO_ERROR must be consulted on v6 too — a writable socket is not connected");
        assert!(waited.as_millis() < 2_000, "a refusal should be immediate, waited {waited:?}");
    }

    /// The bracket asymmetry, which is the single easiest thing to get backwards here: the URI
    /// authority carries them (RFC 3986 §3.2.2, and so `Host:` does — RFC 9110 §7.2), the RESOLVER
    /// does not. `getaddrinfo("[::1]", …)` is EAI_NONAME, so passing the authority form through
    /// would make every IPv6 server read as "does not resolve".
    #[test]
    fn a_v6_literal_is_bracketed_for_the_host_header_and_bare_for_the_resolver() {
        assert_eq!(resolver_node("[2001:db8::1]"), "2001:db8::1");
        assert_eq!(resolver_node("2001:db8::1"), "2001:db8::1", "already bare: unchanged");
        assert_eq!(resolver_node("nas.local"), "nas.local");
        assert_eq!(resolver_node("192.0.2.10"), "192.0.2.10");

        assert_eq!(host_header("2001:db8::1", 32400), "[2001:db8::1]:32400",
                   "a bare v6 literal must be bracketed for the authority");
        assert_eq!(host_header("[2001:db8::1]", 32400), "[2001:db8::1]:32400",
                   "…and one that arrived bracketed must not be double-bracketed");
        assert_eq!(host_header("nas.local", 32400), "nas.local:32400");
        assert_eq!(host_header("192.0.2.10", 32400), "192.0.2.10:32400");
        assert_eq!(host_header("::1", 80), "[::1]:80");
    }

    /// Which `getaddrinfo` flag a host takes turns on this, and getting it wrong is silent: a
    /// literal misfiled as a name goes to DNS (and, under `AI_ADDRCONFIG`, can resolve to nothing
    /// at all), while a name misfiled as a literal fails outright under `AI_NUMERICHOST`.
    #[test]
    fn an_address_literal_is_told_apart_from_a_name() {
        for a in ["127.0.0.1", "192.0.2.10", "::1", "2001:db8::1", "fe80::1%en0"] {
            assert!(is_numeric_host(a), "{a} is an address literal");
        }
        for n in ["nas.local", "plex.example.org", "localhost", "999.1.2.3", "1.2.3", "1.2.3.4.5"] {
            assert!(!is_numeric_host(n), "{n} is not an address literal");
        }
    }

    /// `AI_ADDRCONFIG` suppresses AF_INET6 results on a host whose only IPv6 address is loopback —
    /// which is most developer machines — so a v6 LITERAL must not be resolved under it. This is
    /// the assertion behind `resolve`'s flag split; without it `::1` resolves to nothing on a
    /// perfectly healthy machine and every v6 case below fails for a reason that looks like ours.
    /// Both of these are purely local: `AI_NUMERICHOST` sends no packet and loads no NSS module.
    #[test]
    fn an_address_literal_resolves_without_a_resolver() {
        assert!(unsafe { resolve("127.0.0.1", 80) }.is_some(), "a v4 literal must resolve");
        assert!(unsafe { resolve("::1", 80) }.is_some(),
                "a v6 literal must resolve — if this fails, AI_ADDRCONFIG leaked onto a literal");
        assert!(unsafe { resolve("2001:db8::1", 80) }.is_some(), "a non-loopback v6 literal too");

        // An out-of-range port FAILS instead of wrapping into a plausible one — 70000 used to dial
        // 4464. Note what this is asserting: `resolve`'s OWN range check, not `AI_NUMERICSERV`'s.
        // Darwin rejects the service string and glibc does not, so had this been left to the
        // resolver the assertion would have passed here and the app would still have truncated on
        // the television. It is the shape this file's own notes warn about — a green host run about
        // a platform difference — and it was caught in review, not by the suite.
        assert!(unsafe { resolve("127.0.0.1", 70_000) }.is_none(),
                "an out-of-range port must fail, not truncate into a dialable one");
        assert!(unsafe { resolve("127.0.0.1", 65_536) }.is_none(), "…one past the top");
        assert!(unsafe { resolve("127.0.0.1", -1) }.is_none(), "…nor a negative one");
        assert!(unsafe { resolve("127.0.0.1", 65_535) }.is_some(), "…and the top itself is fine");
    }

    /// IPv6 end to end, which is checklist #43 CASE2: a listener on `::1`, an AF_INET6 socket
    /// opened because the RESOLVER said so, a 200 read back, and — the half a connect alone cannot
    /// show — a bracketed `Host:` on the wire. Both spellings of the host reach the same server,
    /// since `plex::probe::host_of` hands back the bracketed one.
    #[test]
    fn a_v6_literal_connects_and_sends_a_bracketed_host_header() {
        let Some(listener) = v6_loopback_or_skip("the IPv6 end-to-end open") else { return };
        drop(listener); // proven bindable; `one_shot_echo` needs the address for itself

        for host in ["::1", "[::1]"] {
            let (port, h) = one_shot_echo("[::1]:0", b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi".to_vec())
                .expect("bind ::1");
            let (mut hs, rv) = open_host_against(host, port);
            assert_eq!(rv, 0, "{host}: an IPv6 server must open");
            assert_eq!(hs.status, 200, "{host}");

            let mut buf = [0u8; 8];
            let n = http_read(&mut *hs, buf.as_mut_ptr(), buf.len() as c_int);
            assert_eq!(&buf[..n.max(0) as usize], b"hi", "{host}: the body must come back intact");
            http_close(&mut *hs);

            let req = h.join().unwrap();
            assert_eq!(host_line(&req), format!("Host: [::1]:{port}"),
                       "{host}: the authority form is bracketed whichever spelling was handed in");
        }
    }

    /// A NAME, which is the other half of the limitation being removed — and the three things that
    /// have to hold at once for one to work. `localhost` resolves to both families on every machine
    /// this runs on, while the listener is bound to 127.0.0.1 ONLY, so on a host that offers `::1`
    /// first this only passes by WALKING past a refused address to a live one.
    ///
    /// And `Host:` must carry the name. Sending the address it resolved to instead is what breaks
    /// name-based virtual hosting, and it is invisible from the connect: the TCP session is
    /// identical either way.
    #[test]
    fn a_hostname_resolves_and_the_host_header_carries_the_name_not_the_address() {
        let (port, h) = one_shot_echo("127.0.0.1:0", b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec())
            .expect("bind 127.0.0.1");
        let (mut hs, rv) = open_host_against("localhost", port);
        assert_eq!(rv, 0, "a name the system resolves must open — this is the whole DNS gap");
        assert_eq!(hs.status, 200);
        http_close(&mut *hs);

        let req = h.join().unwrap();
        assert_eq!(host_line(&req), format!("Host: localhost:{port}"),
                   "the Host header is the ORIGIN; a resolved address here breaks vhosting");
    }

    /// The v4 literal path still says what it always said — the regression guard for every existing
    /// caller, all of which hand `http_open` a dotted quad.
    #[test]
    fn a_v4_literal_still_sends_its_own_address_as_the_host_header() {
        let (port, h) = one_shot_echo("127.0.0.1:0", b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec())
            .expect("bind");
        let (mut hs, rv) = open_against(port);
        assert_eq!(rv, 0);
        http_close(&mut *hs);
        assert_eq!(host_line(&h.join().unwrap()), format!("Host: 127.0.0.1:{port}"));
    }

    /// Resolving to several addresses and dialling only the first is the old single-address limit
    /// wearing a resolver. The chain here is built by hand rather than resolved, because which
    /// addresses a name yields and in what order is the machine's business and not something a test
    /// can arrange: a REFUSED v6 loopback port first, a live v4 listener second. It is also a
    /// mixed-family chain on purpose — the socket family comes from each node, so a walk that
    /// assumed AF_INET would open the wrong socket for the first, and one that assumed AF_INET6
    /// would open the wrong socket for the second.
    #[test]
    fn the_whole_address_chain_is_walked_until_one_connects() {
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().unwrap().port();

        let mut dead = sockaddr6(std::net::Ipv6Addr::LOCALHOST, 1); // nothing listens on ::1:1
        let mut live = sockaddr([127, 0, 0, 1], port);
        let mut second = ainfo(libc::AF_INET, &mut live as *mut _ as *mut libc::sockaddr,
                               std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                               std::ptr::null_mut());
        let first = ainfo(libc::AF_INET6, &mut dead as *mut _ as *mut libc::sockaddr,
                          std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                          &mut second);

        let hs = http_stream_boxed();
        let fd = unsafe { connect_any(&hs, &first, 2_000) };
        assert!(fd >= 0, "the walk stopped at the first dead address instead of trying the second");
        assert_eq!(hs.fd(), fd, "the connected fd must be left PUBLISHED for http_shutdown to reach");
        let _peer = srv.accept().expect("the live address must actually have been dialled");
        unsafe { close_owned(&hs) };
    }

    /// …and a chain with nothing live fails as one failure, leaving no descriptor behind: every
    /// attempt has to be retired through `close_owned`, not bare-closed and not simply abandoned.
    #[test]
    fn a_chain_with_no_live_address_fails_closed_and_leaks_nothing() {
        let before = open_fd_count();
        for _ in 0..64 {
            let mut a = sockaddr([127, 0, 0, 1], 1);
            let mut b = sockaddr([127, 0, 0, 1], 1);
            let mut second = ainfo(libc::AF_INET, &mut b as *mut _ as *mut libc::sockaddr,
                                   std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                                   std::ptr::null_mut());
            let first = ainfo(libc::AF_INET, &mut a as *mut _ as *mut libc::sockaddr,
                              std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                              &mut second);
            let hs = http_stream_boxed();
            assert_eq!(unsafe { connect_any(&hs, &first, 2_000) }, -1, "nothing here can connect");
            assert_eq!(hs.fd(), -1, "a spent walk must leave the stream CLOSED, not published");
        }
        // Same slack, and the same reason, as `every_failed_open_retires_its_fd_and_leaks_nothing`:
        // `open_fd_count` is PROCESS-wide and this suite runs in parallel, so the sibling socket
        // tests hold descriptors open across this window and the reading drifts by a handful either
        // way — measured at +9 against a first draft that allowed +8, on a run with nothing wrong.
        // The separation is what makes the gate mean something rather than the tightness: 64 rounds
        // of a two-address walk leak 128 descriptors if a single `close_owned` is missed, which is
        // most of an order of magnitude clear of the noise.
        let after = open_fd_count();
        assert!(after <= before + 24, "the walk leaked descriptors: {before} -> {after}");
    }

    /// A teardown mid-open must not be ANSWERED by dialling the next address — that would consume
    /// the interrupt and hand a caller being torn down a brand-new connection, quietly undoing the
    /// interruptibility the publish-before-connect invariant exists to give.
    ///
    /// The latch is armed here instead of raced, deliberately: `shutdown(2)` aborting a handshake in
    /// progress is TRUE on the TV's kernel and NOT on the Darwin host these tests run on
    /// (`tools/sockprobe.c`), so timing the real interrupt would be asserting the host's behaviour.
    /// What is portable, and what actually decides the outcome, is the branch — a failed attempt
    /// plus a latched interrupt stops the walk — and `http_shutdown` is the real API that arms it,
    /// including with the fd already retired, which is the between-attempts window.
    #[test]
    fn an_interrupted_walk_does_not_dial_the_next_address() {
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        srv.set_nonblocking(true).expect("nonblocking accept");
        let port = srv.local_addr().unwrap().port();

        let mut dead = sockaddr([127, 0, 0, 1], 1);
        let mut live = sockaddr([127, 0, 0, 1], port);
        let mut second = ainfo(libc::AF_INET, &mut live as *mut _ as *mut libc::sockaddr,
                               std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                               std::ptr::null_mut());
        let first = ainfo(libc::AF_INET, &mut dead as *mut _ as *mut libc::sockaddr,
                          std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                          &mut second);

        let mut hs = http_stream_boxed();
        http_shutdown(&mut *hs); // a teardown with no descriptor to shoot: the latch is the point
        assert!(hs.interrupted(), "http_shutdown must latch even when the fd is already -1");

        assert_eq!(unsafe { connect_any(&hs, &first, 2_000) }, -1, "an interrupted walk fails");
        assert_eq!(hs.fd(), -1);
        assert!(matches!(srv.accept(), Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock),
                "the second address was dialled anyway — the teardown was answered with a connection");
    }

    /// …and the latch is per-request state: the next `http_open` on the same stream must not
    /// inherit the last teardown, or one interrupted open would poison the reused struct for good.
    /// (`player::engine` pre-allocates its streams and reopens them for the life of a playback.)
    #[test]
    fn a_new_open_clears_the_interrupt_from_the_last_one() {
        let (port, h) = one_shot_server(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec());
        let mut hs = http_stream_boxed();
        http_shutdown(&mut *hs);
        assert!(hs.interrupted());

        let ip = std::ffi::CString::new("127.0.0.1").unwrap();
        let path = std::ffi::CString::new("/x").unwrap();
        let rv = http_open(&mut *hs, ip.as_ptr(), port as c_int, path.as_ptr(), std::ptr::null(), "GET");
        assert_eq!(rv, 0, "a stale interrupt must not fail the next open");
        assert!(!hs.interrupted(), "the latch belongs to one request");
        http_close(&mut *hs);
        h.join().unwrap();
    }

    /// The chunked detection was `find_ci(hdr, b"\r\ntransfer-encoding: chunked")` — one exact
    /// spelling, single space, single token. Every other legal way to write the same header missed,
    /// and a miss is not a failure: the body is then read as close-delimited with the chunk-size
    /// lines left INLINE in it, i.e. silent corruption of whatever the caller parses next.
    #[test]
    fn chunked_is_recognised_however_the_header_is_spelled() {
        let hdr = |te: &str| format!("HTTP/1.1 200 OK\r\n{te}\r\n\r\n").into_bytes();
        for te in [
            "Transfer-Encoding: chunked",   // the one spelling that already worked
            "Transfer-Encoding:chunked",    // OWS after the colon is OPTIONAL (RFC 9110 §5.6.3)
            "Transfer-Encoding:   chunked", // …and may be more than one
            "Transfer-Encoding: chunked ",  // trailing OWS is not part of the value
            "Transfer-Encoding: Chunked",   // the VALUE is case-insensitive too (§10.1.4)
            "transfer-encoding: CHUNKED",
            "Transfer-Encoding: gzip, chunked",   // the legal list form: chunked applied LAST
            "Transfer-Encoding: chunked, gzip",   // malformed per §6.1, but the framing IS chunked
            "Transfer-Encoding: gzip\r\nTransfer-Encoding: chunked", // a list split across lines
        ] {
            assert!(header_is_chunked(&hdr(te)), "missed: {te}");
        }
        for te in [
            "Transfer-Encoding: gzip",
            "Transfer-Encoding: chunkedy",   // a token that merely starts the same way
            "Transfer-Encoding: xchunked",
            "Content-Length: 5",
            "X-Chunked: chunked",            // not the field this decides on
        ] {
            assert!(!header_is_chunked(&hdr(te)), "false positive: {te}");
        }
        // Termination, not just correctness: a block whose last line has no CRLF must still end the
        // scan rather than spin on the same offset.
        assert!(!header_is_chunked(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip"));
        assert!(header_is_chunked(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked"));
        assert!(!header_is_chunked(b""));
    }

    /// …and the spelling reaches the READ path, not merely the predicate. A server answering
    /// `Transfer-Encoding:chunked` (no space) used to hand its caller `4\r\nabcd\r\n0\r\n\r\n`
    /// verbatim — a body that parses as neither JSON nor a media stream, from a healthy server.
    #[test]
    fn a_chunked_body_spelled_without_a_space_is_still_decoded() {
        let (port, h) = one_shot_server(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding:chunked\r\n\r\n4\r\nabcd\r\n3\r\nefg\r\n0\r\n\r\n".to_vec());
        let r = loopback_get(port).expect("a 200 must open");
        assert_eq!((r.status, r.body.as_slice()), (200, &b"abcdefg"[..]), "the chunk framing was left in the body");
        h.join().unwrap();
    }

    /// The constraint the reporting is bound by: it is observability only. A server that promises
    /// 10 bytes and closes after 4 still hands the caller those 4 bytes as `Some(body)`, so what
    /// the data layer does with them (`get_json`'s serde failure, `.ok()`-folded to `None`) is
    /// decided exactly where it was — the event log is the only thing that gained a fact.
    #[test]
    fn a_truncated_body_is_still_returned_to_the_caller() {
        let (port, h) = one_shot_server(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabcd".to_vec());
        let r = loopback_get(port).expect("a truncated body is still a body — this must not become None");
        assert_eq!(r.body.as_slice(), b"abcd", "the bytes that did arrive must be handed over intact");
        assert_eq!(r.status, 200, "…and the server's own verdict travels beside them");
        h.join().unwrap();
    }

    /// **A non-2xx is a RESPONSE, and it must arrive as one.** The wrapper these two tests used to
    /// call answered `None` here, indistinguishable from a refused connection — the collapse the
    /// whole `plex::probe::Outcome` model is built to avoid, and the reason `crate::http` replaced
    /// it. Graded against a real socket rather than against the header parser, because what is
    /// being asserted is the composition: `http_open` reports the failure through its return value
    /// and leaves the code on the struct, and only reading `hs_status` afterwards recovers it.
    #[test]
    fn a_401_reaches_the_caller_as_a_status_and_not_as_a_transport_failure() {
        let (port, h) = one_shot_server(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".to_vec());
        let r = loopback_get(port).expect("the server ANSWERED — that is not a transport failure");
        assert_eq!(r.status, 401);
        assert!(!r.ok(), "…and it is still not a success");
        h.join().unwrap();
    }

    /// Nothing listening is the OTHER outcome, and it must not wear a status. `0` is what
    /// `http_open`'s parser leaves when no `HTTP/1.x NNN` line ever arrived, and `crate::http`
    /// turns that into `None` so a caller cannot read it as a refusal (`classify` would score it
    /// `Unreachable` either way, but by luck rather than by decision).
    #[test]
    fn a_connection_that_never_answers_is_none_rather_than_a_status_of_zero() {
        // Bind and drop, so the port is one nothing is listening on any more.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().expect("addr").port()
        };
        assert!(loopback_get(port).is_none());
    }
}
