//! **The HTTPS media plane: a byte range of a remote file, pulled on demand.**
//!
//! [`crate::stream`] is a raw TCP socket that speaks cleartext to a host name or address of either
//! family. It is the right transport for a plaintext PMS, but it cannot carry an HTTPS origin.
//! LG's QA reviewers will not have a Plex Media Server on their LAN — they sign in with an account
//! we supply and stream from a server on the public internet, over https, at a `plex.direct`
//! hostname whose certificate is issued for that NAME. So the media plane needs a second
//! transport, and this is it.
//!
//! # What this module IS
//!
//! A **pull source**: [`CurlSource::read`], [`CurlSource::seek`], [`CurlSource::size`],
//! [`CurlSource::status`], [`CurlSource::abort`]. That is the entire surface, and it is the same
//! shape `stream.rs` presents to `ff.rs`'s AVIO callbacks, deliberately — `ff.rs` holds a source
//! enum and dispatches, and **never learns curl-multi mechanics**. Every `curl_multi_*` call, the
//! wake pipe, the header parse and the Range validation live behind this door.
//!
//! It is not a general HTTP client. [`crate::net`] is that, for the account/login calls, and it is
//! *blocking by design* — `curl_easy_perform` runs a whole request to completion. A media stream
//! is the opposite shape: it must deliver bytes as they arrive, be seekable by byte offset, and be
//! **interruptible at teardown**, which is what forces the multi interface here.
//!
//! # The libcurl contract is FROZEN to the OLDEST supported television
//!
//! This module's [`dynlib!`] table is a SECOND table, separate from `net.rs`'s, and that is the
//! whole point: [`crate::dynlib::load_into`] is **all-or-nothing**, so one missing symbol empties
//! the table it is in. If the multi symbols shared net's table, a television without them would
//! lose **plex.tv sign-in** — the app would not merely fail to play, it would fail to log in. Two
//! tables means a set that cannot stream over https can still sign in and browse.
//!
//! The table contains exactly seven names. `curl_multi_wait` has existed since curl 7.28.0.
//!
//! **`curl_multi_poll` and `curl_multi_wakeup` are BANNED here, and that is measured rather than
//! argued.** Probed on the dev television 2026-08-23:
//!
//! ```text
//! bound libcurl.so.5        (libcurl.so.4 absent)
//! libcurl/7.53.1 OpenSSL/1.0.2p zlib/1.2.11 c-ares/1.12.0 nghttp2/1.26.0
//! features=0x0029829d   →   AsynchDNS = YES,  IPv6 = YES,  SSL = YES
//! all seven symbols below PRESENT;  curl_multi_poll and curl_multi_wakeup ABSENT
//! ```
//!
//! Both of those resolve on the development Mac, which is exactly the trap the separate table
//! exists to survive: a symbol that is present where you test and absent where you ship.
//!
//! The **easy** half of the API (`curl_easy_init` / `_setopt` / `_cleanup`) is NOT duplicated
//! here — it is read out of `net.rs`'s already-loaded table. Those symbols are ancient and
//! universal; binding them twice would mean two tables that can disagree about which libcurl they
//! came from, for no gain. See [`easy_ready`].
//!
//! # Abort is a WAKE PIPE, not a short poll
//!
//! Teardown's contract everywhere else in the player is "set the flag, then join" — the main
//! thread aborts the AU lanes, fires one `shutdown(2)` at the demux socket, and joins the demux
//! thread. A curl transfer has no socket we own, so `shutdown(2)` has nothing to aim at.
//!
//! `curl_multi_wait` accepts application-owned extra descriptors, so this module creates a pipe
//! and passes the read end. Teardown sets the abort flag and writes one byte; the thread parked in
//! `curl_multi_wait` wakes at once, sees the flag, removes the easy handle and returns. That
//! preserves "set the flag, join" exactly — **without** the 10–100 ms floor a polling timeout
//! would put on every single teardown.
//!
//! Three details are contract, not implementation taste:
//!
//! 1. both ends are **`O_NONBLOCK` and `FD_CLOEXEC`** ([`Wake::new`]);
//! 2. the read side is **drained after every wake** ([`multi_wait`], and it is the only place that
//!    waits, so there is one drain site). A pipe left readable makes every subsequent
//!    `curl_multi_wait` return instantly and turns the loop into a spin — which passes every other
//!    test in this file and only shows up as burnt CPU in production. That is why
//!    `two_sequential_abort_cycles_each_block_until_signalled` exists;
//! 3. **be precise about what "immediate" means.** *Delivery* is immediate. *Observation* is
//!    immediate whenever control is inside `curl_multi_wait` — but on a libcurl with a
//!    SYNCHRONOUS resolver the thread can already be blocked inside `curl_multi_perform`, in
//!    `getaddrinfo`, where no byte in our pipe can reach it. The dev set reports `AsynchDNS = YES`
//!    (c-ares), so the window is much smaller than feared there, but that is ONE firmware.
//!
//! ## The designed fallback for DNS cancellation — documented, deliberately NOT built
//!
//! With `CURLOPT_NOSIGNAL=1` and a synchronous resolver, libcurl cannot honour a timeout during
//! name resolution, so neither can we. **If a device probe ever shows this is a real teardown
//! problem**, the clean fix is our own `getaddrinfo` plus `CURLOPT_RESOLVE`: the `plex.direct`
//! hostname stays in the URL, so TLS SNI and certificate identity are untouched, while libcurl is
//! handed the resolved numeric address separately and never resolves anything itself. It is a
//! dozen lines. It is not written here because nothing has measured it as needed, and a
//! pre-emptive resolver is a second name-resolution path to keep in step with `stream.rs`'s.
//!
//! # Why the abort handle lives in a module-global REGISTRY
//!
//! [`abort_active`] is what `player::engine`'s teardown calls, and a reader's first instinct is
//! that it should take a handle instead — pass the `Arc<Abort>` up to the engine and drop the
//! global. That instinct is right about globals in general and wrong here, for a reason worth
//! writing down so it is not "simplified" back:
//!
//! * The `CurlSource` is **created inside the demux thread** (`ff::demux`), like the `AvioState`
//!   that owns it, because it must not exist at all on the plaintext path. The engine never holds
//!   it. `SHARED.hs_ptr` solves the same problem for `stream.rs` by having the ENGINE own the
//!   `HttpStream` box — an option here only if the engine constructed the curl source too, i.e.
//!   only by moving transport choice out of the demuxer.
//! * Handing the engine a handle instead means publishing it across a thread boundary after the
//!   thread starts. The registry closes that race with an [`OpenReservation`]: the demuxer
//!   publishes the wake target, then re-checks its already-engine-owned AU abort flag, and only
//!   then starts network I/O. Teardown must win one side or the other.
//!
//! So the registry holds an `Arc<Abort>` — atomics and two pipe fds, **no pointer into the
//! source** — and [`abort_active`] signals it under the same mutex that [`CurlSource::drop`]
//! deregisters under. There is no lifetime question left: the `Arc` keeps the pipe open for
//! exactly as long as anyone can signal it, and signalling is a non-blocking `write(2)`, so
//! holding the lock across it cannot block the main thread.
//!
//! One live media transfer at a time is the same assumption `SHARED.hs_ptr` already makes.
#![allow(non_camel_case_types)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_long, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};

pub(crate) type CURL = c_void;
pub(crate) type CURLM = c_void;

// ---- the SECOND libcurl table: curl_multi_*, and nothing else --------------------------------
//
// Same candidate list as `net.rs`, in the same order, so a set carrying both SONAMEs binds both
// tables against the same library. The macOS name is last for the same reason it is there: the
// host simulator and `PlxNative.app` run this code, and the host suite below drives real libcurl
// over plain loopback HTTP.
crate::dynlib! {
    /// The seven multi-interface symbols, frozen to webOS's oldest libcurl (7.53.1). See this
    /// module's doc before adding an eighth — `curl_multi_poll`/`curl_multi_wakeup` are absent on
    /// the dev television and present on the Mac.
    curlmulti: ["libcurl.so.4", "libcurl.so.5", "libcurl.4.dylib"] {
    fn curl_multi_init() -> *mut CURLM;
    fn curl_multi_add_handle(multi: *mut CURLM, easy: *mut CURL) -> c_int;
    fn curl_multi_perform(multi: *mut CURLM, running: *mut c_int) -> c_int;
    fn curl_multi_wait(multi: *mut CURLM, extra: *mut curl_waitfd, n_extra: u32,
                       timeout_ms: c_int, numfds: *mut c_int) -> c_int;
    fn curl_multi_info_read(multi: *mut CURLM, msgs_left: *mut c_int) -> *mut CURLMsg;
    fn curl_multi_remove_handle(multi: *mut CURLM, easy: *mut CURL) -> c_int;
    fn curl_multi_cleanup(multi: *mut CURLM) -> c_int;
}}

/// `struct curl_waitfd` — `multi.h`, unchanged since 7.28.0. `curl_socket_t` is `int` on POSIX.
#[repr(C)]
pub(crate) struct curl_waitfd {
    fd: c_int,
    events: i16,
    revents: i16,
}

/// `struct CURLMsg` — `multi.h`. `data` is a union of `void *whatever` and `CURLcode result`;
/// declaring it pointer-sized keeps this struct's SIZE and field offsets identical to C's on both
/// a 32-bit television and a 64-bit Mac, which a bare `c_int` + hand-written padding would not.
/// Reading the `CURLcode` out of it is [`msg_result`].
#[repr(C)]
pub(crate) struct CURLMsg {
    msg: c_int,
    easy_handle: *mut CURL,
    data: *mut c_void,
}

const CURLMSG_DONE: c_int = 1;
const CURLM_OK: c_int = 0;
const CURL_WAIT_POLLIN: i16 = 0x0001;

// curl.h option ids. STRINGPOINT/OBJECTPOINT/CBPOINT/SLISTPOINT = 10000, FUNCTIONPOINT = 20000,
// LONG = 0 — read off `curl/curl.h` rather than remembered.
const CURLOPT_WRITEDATA: c_int = 10001;
const CURLOPT_URL: c_int = 10002;
const CURLOPT_RANGE: c_int = 10007;
const CURLOPT_WRITEFUNCTION: c_int = 20011;
const CURLOPT_USERAGENT: c_int = 10018;
const CURLOPT_LOW_SPEED_LIMIT: c_int = 19;
const CURLOPT_LOW_SPEED_TIME: c_int = 20;
const CURLOPT_HEADERDATA: c_int = 10029;
const CURLOPT_NOPROGRESS: c_int = 43;
const CURLOPT_FOLLOWLOCATION: c_int = 52;
const CURLOPT_SSL_VERIFYPEER: c_int = 64;
const CURLOPT_MAXREDIRS: c_int = 68;
const CURLOPT_CONNECTTIMEOUT: c_int = 78;
const CURLOPT_HEADERFUNCTION: c_int = 20079;
const CURLOPT_SSL_VERIFYHOST: c_int = 81;
const CURLOPT_BUFFERSIZE: c_int = 98;
const CURLOPT_NOSIGNAL: c_int = 99;
const CURLOPT_TCP_NODELAY: c_int = 121;
/// The numeric protocol options are the compatibility surface for this project's curl floor.
/// Their `_STR` replacements are newer than webOS 4.5's libcurl 7.53.1.
const CURLOPT_PROTOCOLS: c_int = 181;
const CURLOPT_REDIR_PROTOCOLS: c_int = 182;
const CURLPROTO_HTTP: c_long = 1 << 0;
const CURLPROTO_HTTPS: c_long = 1 << 1;

/// Protocol floor for redirects. A TLS request may never cross back into plaintext; an HTTP
/// request may upgrade. Kept pure so the security rule is host-testable without a TLS fixture.
fn allowed_redirect_protocols(url: &[u8]) -> c_long {
    if url.starts_with(b"https://") {
        CURLPROTO_HTTPS
    } else {
        CURLPROTO_HTTP | CURLPROTO_HTTPS
    }
}

/// How long one `curl_multi_wait` may sit before we call `curl_multi_perform` again.
///
/// **This is NOT the abort path** — the wake pipe is, and it fires in microseconds. This bound
/// exists only because `curl_multi_wait` (unlike `curl_multi_poll`, which we may not use) does not
/// consult libcurl's own timer wheel on 7.53, so connect/low-speed timeouts are noticed on the
/// next perform. 200 ms is therefore a *timer resolution*, not a teardown cost, and while bytes
/// are flowing the wait returns on socket activity long before it.
const WAIT_MS: c_int = 200;

/// `CURLOPT_CONNECTTIMEOUT`, seconds. Matches `stream.rs`'s `CONNECT_TIMEOUT_MS` intent.
const CONNECT_TIMEOUT_S: c_long = 15;
/// `CURLOPT_LOW_SPEED_LIMIT`/`_TIME`: fewer than 1 byte/s for 30 s ends the transfer. This is the
/// curl-side equivalent of `stream.rs`'s 15 s `SO_RCVTIMEO` — without it a server that accepts,
/// answers headers and then stops sending parks the demuxer forever, which is exactly the
/// `tools/netcond.py --mode stall` case.
const LOW_SPEED_TIME_S: c_long = 30;

// ---- the wake pipe ---------------------------------------------------------------------------

/// A self-pipe: one byte on the write end wakes anything polling the read end. Owns both
/// descriptors and closes them on drop.
struct Wake {
    r: c_int,
    w: c_int,
}

impl Wake {
    /// `pipe(2)` with **both ends** `O_NONBLOCK | FD_CLOEXEC`.
    ///
    /// `pipe2(2)` would set both atomically but does not exist on Darwin, and this module's host
    /// suite is the gate — so `pipe` + `fcntl`, which is portable. The CLOEXEC race that `pipe2`
    /// closes needs a concurrent `fork`+`exec`, and this process never does either.
    fn new() -> Option<Wake> {
        let mut fds = [-1 as c_int; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return None;
        }
        let w = Wake { r: fds[0], w: fds[1] }; // owned from here: any early return closes both
        for fd in [w.r, w.w] {
            unsafe {
                let fl = libc::fcntl(fd, libc::F_GETFL, 0);
                if fl < 0 || libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK) < 0 {
                    return None;
                }
                let fd_fl = libc::fcntl(fd, libc::F_GETFD, 0);
                if fd_fl < 0 || libc::fcntl(fd, libc::F_SETFD, fd_fl | libc::FD_CLOEXEC) < 0 {
                    return None;
                }
            }
        }
        Some(w)
    }

    /// Make the read end readable. Non-blocking, so a pipe that is already armed simply returns
    /// `EAGAIN` — an abort signalled twice is one wake, which is all it needs to be.
    fn signal(&self) {
        let b = [1u8; 1];
        unsafe { libc::write(self.w, b.as_ptr() as *const c_void, 1) };
    }

    /// Empty the read end. **Clause 2 of the contract**: skip this and every later
    /// `curl_multi_wait` returns instantly on a still-readable fd, which is a spin, not a wake.
    fn drain(&self) {
        let mut b = [0u8; 64];
        loop {
            let n = unsafe { libc::read(self.r, b.as_mut_ptr() as *mut c_void, b.len()) };
            if n <= 0 {
                return; // EAGAIN (empty), or an error we cannot act on here
            }
        }
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        for fd in [self.r, self.w] {
            if fd >= 0 {
                unsafe { libc::close(fd) };
            }
        }
    }
}

/// The teardown signal for one media transfer: a flag anything can read, and the pipe that gets
/// a blocked `curl_multi_wait` to read it. Shared (`Arc`) between the demux thread that owns the
/// [`CurlSource`] and the registry the main thread signals through.
pub(crate) struct Abort {
    flag: AtomicBool,
    wake: Wake,
}

impl Abort {
    fn new() -> Option<Arc<Abort>> {
        Some(Arc::new(Abort { flag: AtomicBool::new(false), wake: Wake::new()? }))
    }

    /// Set the flag, then wake. **Order matters**: a thread woken by the byte must be able to see
    /// the flag already set, or it goes straight back to waiting and the abort is lost.
    pub(crate) fn signal(&self) {
        self.flag.store(true, Ordering::Release);
        self.wake.signal();
    }

    fn is_set(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

/// The one live media transfer's abort handle. See the module doc for why this is a registry and
/// not a handle passed up to the engine.
static ACTIVE: Mutex<Option<Arc<Abort>>> = Mutex::new(None);

fn lock_active() -> std::sync::MutexGuard<'static, Option<Arc<Abort>>> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// An abort handle published before the demux thread's final teardown check.
///
/// Publication has to precede that check: teardown either sees this handle and signals it, or it
/// aborts the AU lane first and the demuxer observes that flag before starting network I/O. A
/// handle created by [`CurlSource::open`] only after the check leaves a lost-wake window between
/// the two operations.
pub(crate) struct OpenReservation {
    abort: Option<Arc<Abort>>,
}

impl OpenReservation {
    fn publish() -> Option<OpenReservation> {
        let abort = Abort::new()?;
        *lock_active() = Some(Arc::clone(&abort));
        Some(OpenReservation { abort: Some(abort) })
    }

    fn into_abort(mut self) -> Arc<Abort> {
        self.abort.take().expect("a reservation is consumed once")
    }
}

impl Drop for OpenReservation {
    fn drop(&mut self) {
        let Some(abort) = self.abort.as_ref() else { return };
        let mut act = lock_active();
        if act.as_ref().is_some_and(|a| Arc::ptr_eq(a, abort)) {
            *act = None;
        }
    }
}

/// **Teardown's entry point** — `player::engine::teardown` calls this beside
/// `crate::stream::http_shutdown`, and exactly one of the two has anything to signal.
///
/// Signals under the registry lock, which is also the lock [`CurlSource::drop`] deregisters under,
/// so this can never write to a pipe whose owner has gone. Costs one non-blocking `write(2)` on
/// the main thread; a no-op when the live source is a plaintext socket, or when there is none.
pub(crate) fn abort_active() {
    if let Some(a) = lock_active().as_ref() {
        a.signal();
    }
}

// ---- availability ----------------------------------------------------------------------------

static MULTI_OK: AtomicBool = AtomicBool::new(false);
static LOAD_ONCE: Once = Once::new();

/// Resolve the multi table. Idempotent; safe to call from anywhere, though [`boot`] is where it
/// normally happens so the `dlopen` and its log line land on the main thread at start-up.
fn ensure_loaded() {
    LOAD_ONCE.call_once(|| {
        match curlmulti::load(None) {
            crate::dynlib::Loaded::Ok(soname) => {
                crate::log(&format!("curlio: bound {soname} curl_multi_* (7 symbols)"));
                MULTI_OK.store(true, Ordering::Release);
            }
            crate::dynlib::Loaded::NoLibrary => {
                crate::log("curlio: no libcurl on this device — https streaming unavailable (sign-in is unaffected)");
            }
            crate::dynlib::Loaded::Incomplete(soname, n) => {
                crate::log(&format!(
                    "curlio: {soname} is missing {n} curl_multi_* symbol(s) — https streaming unavailable \
                     (sign-in is unaffected; this table is separate from net.rs's for exactly that reason)"
                ));
            }
        }
    });
}

/// Boot-time load, called from `ff::boot` — the media plane's own start-up, after
/// `net::global_init` has run `curl_global_init` on the main thread.
pub(crate) fn boot() {
    ensure_loaded();
}

/// Is `net.rs`'s **easy** table live? Read directly rather than duplicated into this module's
/// table: see the module doc. `curl_easy_init` stands for the whole table because
/// [`crate::dynlib::load_into`] is all-or-nothing — one live cell means every cell is live.
///
/// This deliberately does NOT call `net::global_init` itself. `curl_global_init` is not
/// thread-safe and net's doc requires it on the main thread at boot; a demux thread calling it
/// lazily is precisely the bug that doc is warning about.
fn easy_ready() -> bool {
    !crate::net::curl::curl_easy_init.load(Ordering::Relaxed).is_null()
}

/// Can this device stream over https at all? Both tables must be live.
pub(crate) fn available() -> bool {
    ensure_loaded();
    MULTI_OK.load(Ordering::Acquire) && easy_ready()
}

// ---- the transfer ----------------------------------------------------------------------------

/// Why an open or a seek did not produce a readable stream. Every variant is a **clean refusal**:
/// nothing here panics, so a television without the multi table lands on the player's failure
/// read-out instead of killing a thread.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OpenErr {
    /// libcurl, or its multi interface, could not be bound on this device.
    Unavailable,
    /// Teardown fired while the open was in flight.
    Aborted,
    /// A `CURLcode` — DNS, TLS, connect, low-speed. Named the way `net.rs` names them.
    Transport(c_int),
    /// A `CURLMcode` from the multi interface itself. This is local transport machinery failing,
    /// not a clean end of the media resource.
    Multi(c_int),
    /// The server answered, with a status we cannot stream.
    Status(c_int),
    /// **We asked for a byte range and the server ignored it**, answering `200` from byte zero (or
    /// `206` starting somewhere other than where we asked). Its own variant because it is the one
    /// failure that looks superficially like success: the demuxer would read frame data from the
    /// head of the file believing it was at the seek target, and produce corruption rather than an
    /// error. `stream.rs` has the same hazard and the same rule.
    RangeIgnored,
    /// Local setup failed (no pipe, no multi handle, a URL with an interior NUL).
    Local,
}

/// The bits the C callbacks write. Boxed by [`CurlSource`] so its address is stable across the
/// source being moved (it is returned in a `Box`, and `ff.rs` stores it in an `AvioState`).
struct Xfer {
    /// Body bytes received and not yet handed to the reader. Only ever appended to while EMPTY —
    /// [`CurlSource::read`] performs a transfer step exclusively when it has nothing left to
    /// deliver, so this cannot grow without bound.
    buf: Vec<u8>,
    /// How much of `buf` has already been delivered.
    pos: usize,
    /// The status of the most recent response — the FINAL one, since a redirect resets it.
    status: c_int,
    /// `Content-Length` of the current response body, or -1.
    body_len: i64,
    /// `Content-Range`'s total, or -1 when absent/unknown (`bytes 0-9/*`).
    range_total: i64,
    /// `Content-Range`'s first byte, or -1 when there was no `Content-Range` at all. This is what
    /// makes "the server answered 206 from the wrong offset" detectable.
    range_start: i64,
    /// The final response's header block is complete: the point at which an open may return.
    headers_done: bool,
}

impl Xfer {
    fn new() -> Xfer {
        Xfer { buf: Vec::new(), pos: 0, status: 0, body_len: -1, range_total: -1, range_start: -1, headers_done: false }
    }
    fn reset(&mut self) {
        self.buf.clear();
        self.pos = 0;
        self.status = 0;
        self.body_len = -1;
        self.range_total = -1;
        self.range_start = -1;
        self.headers_done = false;
    }
    fn pending(&self) -> usize {
        self.buf.len() - self.pos
    }
}

/// One remote file, pulled over http(s) by byte range.
///
/// Lives on the demux thread for its whole life — created there, read there, dropped there. Only
/// [`Abort`] crosses a thread boundary, and it holds no pointer into this struct.
pub(crate) struct CurlSource {
    multi: *mut CURLM,
    /// The easy handle for the transfer currently attached to `multi`, or null between them. A
    /// seek is a fresh easy handle, because a Range is a request header.
    easy: *mut CURL,
    url: CString,
    ua: CString,
    range: Option<CString>,
    xfer: Box<Xfer>,
    abort: Arc<Abort>,
    /// Byte offset of the next byte [`CurlSource::read`] will deliver.
    off: i64,
    /// Total size of the resource, or -1 when the server never said.
    size: i64,
    /// The transfer reached `CURLMSG_DONE`, or ended through a multi-interface failure.
    done: bool,
    /// The `CURLcode` that DONE carried, when there was one.
    rc: c_int,
    /// Whether the transfer ended badly. Kept separately from `rc` because a `CURLMcode` failure
    /// has no `CURLcode`, and must not become a clean EOF merely because `rc` is still zero.
    failed: bool,
    /// The multi handle itself failed and must be rebuilt before a later seek may retry.
    multi_failed: bool,
    /// Whether the attached transfer has been validated at the byte offset in [`Self::off`].
    /// A failed seek leaves this false: reads fail, but a later seek may still recover.
    readable: bool,
    /// **The range-corruption latch, and only that.**
    ///
    /// Set only when a server ignores a byte Range or answers it from the wrong offset. A 416 is
    /// deliberately not poison: it is the correct response to a seek at or past EOF, and a later
    /// seek to a valid byte may recover.
    poisoned: bool,
}

impl CurlSource {
    /// Open `url` and read up to the end of its response headers, so that [`status`](Self::status)
    /// and [`size`](Self::size) are answerable the moment this returns — the same contract
    /// `stream::http_open` has, and what `ff::demux` publishes into the diagnostics read-out.
    #[cfg(test)]
    pub(crate) fn open(url: &str, at: i64) -> Result<Box<CurlSource>, OpenErr> {
        Self::open_gated(url, at, available())
    }

    /// Publish teardown's wake target before the demuxer's last AU-abort check.
    pub(crate) fn reserve_open() -> Result<OpenReservation, OpenErr> {
        if !available() {
            crate::player::log("curlio: refusing — libcurl multi is not available on this device");
            return Err(OpenErr::Unavailable);
        }
        OpenReservation::publish().ok_or(OpenErr::Local)
    }

    /// Finish an open whose abort handle was already published by [`reserve_open`](Self::reserve_open).
    pub(crate) fn open_reserved(
        url: &str,
        at: i64,
        reservation: OpenReservation,
    ) -> Result<Box<CurlSource>, OpenErr> {
        Self::open_with_reservation(url, at, reservation)
    }

    /// [`open`](Self::open) with the availability verdict injected, so the host suite can grade
    /// the no-libcurl path without poisoning a process-global table.
    #[cfg(test)]
    fn open_gated(url: &str, at: i64, curl_ok: bool) -> Result<Box<CurlSource>, OpenErr> {
        if !curl_ok {
            crate::player::log("curlio: refusing — libcurl multi is not available on this device");
            return Err(OpenErr::Unavailable);
        }
        let reservation = OpenReservation::publish().ok_or(OpenErr::Local)?;
        Self::open_with_reservation(url, at, reservation)
    }

    fn open_with_reservation(
        url: &str,
        at: i64,
        reservation: OpenReservation,
    ) -> Result<Box<CurlSource>, OpenErr> {
        let url_c = CString::new(url).map_err(|_| OpenErr::Local)?;
        let ua = CString::new(crate::plex::identity::user_agent()).map_err(|_| OpenErr::Local)?;
        let multi = unsafe { curl_multi_init() };
        if multi.is_null() {
            return Err(OpenErr::Local);
        }
        let abort = reservation.into_abort();
        let mut src = Box::new(CurlSource {
            multi,
            easy: std::ptr::null_mut(),
            url: url_c,
            ua,
            range: None,
            xfer: Box::new(Xfer::new()),
            abort: Arc::clone(&abort),
            off: 0,
            size: -1,
            done: false,
            rc: 0,
            failed: false,
            multi_failed: false,
            readable: false,
            poisoned: false,
        });
        src.start(at)?;
        Ok(src)
    }

    /// Attach a fresh easy handle at byte `at` and pump until its headers are complete.
    fn start(&mut self, at: i64) -> Result<(), OpenErr> {
        self.stop();
        if self.multi_failed {
            // A CURLM error describes this multi handle, not the remote byte range. Rebuild the
            // local machinery before allowing a later seek to retry; clearing the flag while
            // reusing the same broken handle would immediately fail again.
            if !self.multi.is_null() {
                unsafe { curl_multi_cleanup(self.multi) };
            }
            self.multi = unsafe { curl_multi_init() };
            if self.multi.is_null() {
                return Err(OpenErr::Local);
            }
            self.multi_failed = false;
        }
        self.xfer.reset();
        self.done = false;
        self.rc = 0;
        self.failed = false;
        self.readable = false;
        if self.abort.is_set() {
            return Err(OpenErr::Aborted);
        }
        let easy = unsafe { crate::net::curl_easy_init() };
        if easy.is_null() {
            return Err(OpenErr::Local);
        }
        self.easy = easy;
        // `bytes=N-` open-ended, exactly the header `stream.rs`'s seek path sends. Held in `self`
        // rather than a local: libcurl copies string options (7.17+), but the whole table here is
        // bound at runtime and an owned copy costs nothing to be sure of.
        self.range = if at > 0 { CString::new(format!("{at}-")).ok() } else { None };
        unsafe {
            let xp = &mut *self.xfer as *mut Xfer as *mut c_void;
            crate::net::curl_easy_setopt_ptr(easy, CURLOPT_URL, self.url.as_ptr() as *const c_void);
            crate::net::curl_easy_setopt_ptr(easy, CURLOPT_WRITEFUNCTION, write_cb as *const c_void);
            crate::net::curl_easy_setopt_ptr(easy, CURLOPT_WRITEDATA, xp);
            crate::net::curl_easy_setopt_ptr(easy, CURLOPT_HEADERFUNCTION, header_cb as *const c_void);
            crate::net::curl_easy_setopt_ptr(easy, CURLOPT_HEADERDATA, xp);
            crate::net::curl_easy_setopt_ptr(easy, CURLOPT_USERAGENT, self.ua.as_ptr() as *const c_void);
            if let Some(r) = &self.range {
                crate::net::curl_easy_setopt_ptr(easy, CURLOPT_RANGE, r.as_ptr() as *const c_void);
            }
            // TLS verification ON, both halves — the certificate is issued for the `plex.direct`
            // NAME, which is the entire reason an Origin is parsed from a URL and never rebuilt
            // from an address (`plex/origin.rs`). Turning either of these off would make an
            // https URL "work" against the wrong server.
            crate::net::curl_easy_setopt_long(easy, CURLOPT_SSL_VERIFYPEER, 1);
            crate::net::curl_easy_setopt_long(easy, CURLOPT_SSL_VERIFYHOST, 2);
            // We are on a worker thread; curl must not install signal handlers. This is also what
            // makes DNS uncancellable on a synchronous resolver — see the module doc.
            crate::net::curl_easy_setopt_long(easy, CURLOPT_NOSIGNAL, 1);
            crate::net::curl_easy_setopt_long(easy, CURLOPT_FOLLOWLOCATION, 1);
            crate::net::curl_easy_setopt_long(easy, CURLOPT_MAXREDIRS, 5);
            // A redirect must not downgrade a TLS media request. On the television's libcurl
            // 7.53.1 the redirect default is broader than HTTP(S), and even current curl permits
            // https -> http unless the caller narrows it. The token is in the URL, so a downgrade
            // would expose both it and the stream. A plaintext request may upgrade to TLS; a TLS
            // request may remain TLS only.
            let redirect_protocols = allowed_redirect_protocols(self.url.as_bytes());
            crate::net::curl_easy_setopt_long(easy, CURLOPT_PROTOCOLS, CURLPROTO_HTTP | CURLPROTO_HTTPS);
            crate::net::curl_easy_setopt_long(easy, CURLOPT_REDIR_PROTOCOLS, redirect_protocols);
            crate::net::curl_easy_setopt_long(easy, CURLOPT_CONNECTTIMEOUT, CONNECT_TIMEOUT_S);
            crate::net::curl_easy_setopt_long(easy, CURLOPT_LOW_SPEED_LIMIT, 1);
            crate::net::curl_easy_setopt_long(easy, CURLOPT_LOW_SPEED_TIME, LOW_SPEED_TIME_S);
            crate::net::curl_easy_setopt_long(easy, CURLOPT_NOPROGRESS, 1);
            crate::net::curl_easy_setopt_long(easy, CURLOPT_TCP_NODELAY, 1);
            crate::net::curl_easy_setopt_long(easy, CURLOPT_BUFFERSIZE, 65536);
            // NB no CURLOPT_TIMEOUT: a whole movie has no deadline. The low-speed pair above is
            // what bounds a stall, and unlike a total timeout it cannot kill a healthy transfer.
            // NB no CURLOPT_ACCEPT_ENCODING either — the demuxer wants the bytes the file has.
            let rc = curl_multi_add_handle(self.multi, easy);
            if rc != CURLM_OK {
                crate::player::log(&format!("curlio: curl_multi_add_handle failed mcode={rc}"));
                self.failed = true;
                self.multi_failed = true;
                self.done = true;
                return Err(OpenErr::Multi(rc));
            }
        }
        // Pump until the final response's headers are in, the transfer ends, or teardown fires.
        while !self.xfer.headers_done && !self.done {
            if self.abort.is_set() {
                return Err(OpenErr::Aborted);
            }
            self.perform()?;
            if self.xfer.headers_done || self.done {
                break;
            }
            match multi_wait(self.multi, &self.abort, WAIT_MS) {
                Wait::Woken if self.abort.is_set() => return Err(OpenErr::Aborted),
                Wait::Failed(rc) => {
                    self.fail_multi_wait(rc);
                    return Err(OpenErr::Multi(rc));
                }
                _ => {}
            }
        }
        self.validate(at)
    }

    /// Grade the response we just got headers for. Order matters: a transport failure explains a
    /// missing status, so it is reported first.
    fn validate(&mut self, at: i64) -> Result<(), OpenErr> {
        if self.abort.is_set() {
            return Err(OpenErr::Aborted);
        }
        if self.done && self.failed {
            crate::player::log(&format!("curlio: transport failed rc={} — {}", self.rc, curl_why(self.rc)));
            return Err(OpenErr::Transport(self.rc));
        }
        if self.xfer.status == 0 {
            crate::player::log("curlio: the transfer ended before any response line arrived");
            return Err(OpenErr::Transport(self.rc));
        }
        // Grade the transaction before its Range semantics. A 401, final redirect, 416, or
        // transient 5xx is a failed request, not evidence that this server ignores Range; only a
        // SUCCESSFUL but wrong response can make future reads unsafe.
        if !(200..300).contains(&self.xfer.status) {
            crate::player::log(&format!("curlio: status={}", self.xfer.status));
            return Err(OpenErr::Status(self.xfer.status));
        }
        // **A Range answered 200 is CORRUPTION, not a restart.** The body would be the head of the
        // file while the demuxer believes it is at `at`. A real Range answer is 206 and MUST name
        // the requested first byte in Content-Range. Missing/malformed is `-1` and is just as
        // unverified as a different positive offset.
        if at > 0 {
            if self.xfer.status != 206 || self.xfer.range_start != at {
                crate::player::log(&format!(
                    "curlio: asked for bytes={at}- and got status={} start={} — refusing rather than \
                     feeding the demuxer the wrong offset",
                    self.xfer.status, self.xfer.range_start
                ));
                return Err(OpenErr::RangeIgnored);
            }
        }
        self.off = at;
        // Size, in the order of authority: `Content-Range`'s total names the whole resource;
        // otherwise this response's `Content-Length` plus where it started does.
        self.size = if self.xfer.range_total >= 0 {
            self.xfer.range_total
        } else if self.xfer.body_len >= 0 {
            at + self.xfer.body_len
        } else {
            -1
        };
        self.readable = true;
        Ok(())
    }

    /// One `curl_multi_perform`, reaping the completion message when the last handle finishes.
    fn perform(&mut self) -> Result<(), OpenErr> {
        let mut running: c_int = 0;
        let mut rc = unsafe { curl_multi_perform(self.multi, &mut running) };
        // CURLM_CALL_MULTI_PERFORM (-1) is a pre-7.20 "call me again"; harmless to honour once.
        if rc == -1 {
            rc = unsafe { curl_multi_perform(self.multi, &mut running) };
        }
        // On a real CURLM error libcurl does not promise to write `running`; leaving its initial
        // zero to fall into `reap` would find no DONE message and turn this failure into a clean
        // EOF. Persist the failure so a later read cannot make the same mistake.
        if rc != CURLM_OK {
            crate::player::log(&format!("curlio: curl_multi_perform failed mcode={rc}"));
            self.failed = true;
            self.multi_failed = true;
            self.done = true;
            return Err(OpenErr::Multi(rc));
        }
        if running == 0 {
            self.reap();
        }
        Ok(())
    }

    /// Drain the multi handle's message queue. Only reached with `running == 0`, so the transfer
    /// IS over whether or not a DONE message is waiting for us.
    fn reap(&mut self) {
        loop {
            let mut left: c_int = 0;
            let m = unsafe { curl_multi_info_read(self.multi, &mut left) };
            if m.is_null() {
                break;
            }
            unsafe {
                if (*m).msg == CURLMSG_DONE {
                    self.rc = msg_result(m);
                    self.failed = self.rc != 0;
                }
            }
            if left == 0 {
                break;
            }
        }
        self.done = true;
    }

    /// Detach and free the current easy handle. Idempotent.
    ///
    /// If libcurl refuses the detach, ownership is no longer provable. Its documented cleanup
    /// order requires a successful remove before either handle is cleaned, so that catastrophic
    /// path deliberately abandons both pointers and lets a later seek create a fresh multi. A
    /// bounded leak is safer than freeing an easy that libcurl may still reference.
    fn stop(&mut self) {
        if !self.easy.is_null() {
            unsafe {
                let rc = curl_multi_remove_handle(self.multi, self.easy);
                if rc != CURLM_OK {
                    crate::player::log(&format!("curlio: curl_multi_remove_handle failed mcode={rc}"));
                    self.multi_failed = true;
                    // Neither cleanup is contractually safe while attachment is uncertain.
                    // Forget both values; `start` sees `multi_failed` and builds a fresh owner.
                    self.multi = std::ptr::null_mut();
                    self.easy = std::ptr::null_mut();
                    self.range = None;
                    return;
                }
                crate::net::curl_easy_cleanup(self.easy);
            }
            self.easy = std::ptr::null_mut();
        }
        self.range = None;
    }

    /// Deliver up to `dst.len()` bytes. **`>0` = bytes, `0` = clean end of stream, `<0` = error or
    /// teardown** — the same three-way return `stream::http_read` gives. `ff.rs` preserves curl's
    /// negative result as an I/O error rather than collapsing a truncated transfer into EOF.
    pub(crate) fn read(&mut self, dst: &mut [u8]) -> c_int {
        if dst.is_empty() {
            return 0;
        }
        loop {
            if self.abort.is_set() || self.poisoned || !self.readable {
                return -1;
            }
            if self.xfer.pending() > 0 {
                let n = std::cmp::min(dst.len(), self.xfer.pending());
                dst[..n].copy_from_slice(&self.xfer.buf[self.xfer.pos..self.xfer.pos + n]);
                self.xfer.pos += n;
                if self.xfer.pos == self.xfer.buf.len() {
                    // Fully drained: reset rather than shift, which is what keeps the write
                    // callback appending to an empty Vec and this loop free of memmoves.
                    self.xfer.buf.clear();
                    self.xfer.pos = 0;
                }
                self.off += n as i64;
                return n as c_int;
            }
            if self.done {
                return if self.failed { -1 } else { 0 };
            }
            if self.perform().is_err() {
                return -1;
            }
            if self.xfer.pending() > 0 || self.done {
                continue;
            }
            match multi_wait(self.multi, &self.abort, WAIT_MS) {
                Wait::Woken if self.abort.is_set() => return -1,
                Wait::Failed(rc) => {
                    self.fail_multi_wait(rc);
                    return -1;
                }
                _ => {}
            }
        }
    }

    /// Record a multi-wait failure as a terminal transport error.
    ///
    /// `curl_multi_wait` returns immediately on error. Treating that as a timeout spins at 100%
    /// CPU; retrying on the same multi handle cannot repair it, so fail this transfer immediately
    /// and mark the handle for replacement before a later seek may retry.
    fn fail_multi_wait(&mut self, rc: c_int) {
        crate::player::log(&format!("curlio: curl_multi_wait failed mcode={rc}"));
        self.failed = true;
        self.multi_failed = true;
        self.done = true;
    }

    /// Re-open at byte `to`. `false` means the demuxer must treat the seek as failed — including
    /// the case where the server ignored the Range, which is refused rather than silently served
    /// from byte zero.
    ///
    /// **Checks the abort flag before any curl call**, which is what keeps a teardown from being
    /// answered with a brand-new connection the main thread's join is already waiting behind — the
    /// same invariant `ff.rs`'s `seek_cb` carries for the socket source, and for the same reason.
    pub(crate) fn seek(&mut self, to: i64) -> bool {
        if self.abort.is_set() || self.poisoned {
            return false;
        }
        if to < 0 {
            return false;
        }
        match self.start(to) {
            Ok(()) => true,
            Err(e) => {
                // Detach whatever the failed attempt left attached. `start` cleared `readable`, so
                // reads fail rather than reporting clean EOF, but another seek may recover.
                self.stop();
                // Only a server that ignores byte ranges is permanently unsafe. A 416, 5xx, or
                // dropped connection may all be answered differently by the next seek.
                if e == OpenErr::RangeIgnored {
                    self.poisoned = true;
                }
                crate::player::log(&format!("curlio: seek to {to} failed: {e:?}"));
                false
            }
        }
    }

    /// Total size of the resource, or -1 when the server never said. What `AVSEEK_SIZE` answers.
    pub(crate) fn size(&self) -> i64 {
        self.size
    }

    /// The last response's HTTP status — the diagnostics read-out's `dg_http_status`.
    pub(crate) fn status(&self) -> c_int {
        self.xfer.status
    }

    /// Signal teardown on THIS source. `player::engine` uses [`abort_active`] instead, since it
    /// does not hold the source; this is the direct form, and what the host suite drives.
    pub(crate) fn abort(&self) {
        self.abort.signal();
    }

    #[cfg(test)]
    pub(crate) fn fail_multi_for_test(&mut self) {
        self.fail_multi_wait(1);
    }
}

impl Drop for CurlSource {
    fn drop(&mut self) {
        // Deregister FIRST, under the lock `abort_active` signals under, so nothing can signal a
        // handle whose pipe is about to close. `Arc::ptr_eq` because a later source may already
        // have taken the slot if two ever overlapped.
        {
            let mut act = lock_active();
            let mine = act.as_ref().is_some_and(|a| Arc::ptr_eq(a, &self.abort));
            if mine {
                *act = None;
            }
        }
        self.stop();
        if !self.multi.is_null() {
            unsafe { curl_multi_cleanup(self.multi) };
            self.multi = std::ptr::null_mut();
        }
    }
}

// ---- the wait, and the C callbacks -----------------------------------------------------------

/// What ended a [`multi_wait`].
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Wait {
    /// The abort pipe fired. The pipe has been drained.
    Woken,
    /// A curl socket has activity.
    Ready,
    /// Nothing happened inside the timeout.
    Timeout,
    /// The multi interface itself failed. This is distinct from a timeout because it returns
    /// immediately; callers must terminate the transfer or they busy-spin.
    Failed(c_int),
}

/// `curl_multi_wait` with the abort pipe as an application-owned extra descriptor.
///
/// **The only place this module blocks, and therefore the only place the pipe is drained** — see
/// clause 2 of the module doc. A free function rather than a method so the host suite can drive it
/// against a bare multi handle and prove the drain mechanically.
///
/// A second consequence of the extra fd is worth knowing on 7.53: `curl_multi_wait` on libcurl
/// before 7.68 returns IMMEDIATELY when it has no descriptors to poll (the busy-loop that
/// `curl_multi_poll` was added to fix, and which we may not use here). Passing the pipe means
/// there is always at least one, so that path is never taken.
fn multi_wait(multi: *mut CURLM, abort: &Abort, timeout_ms: c_int) -> Wait {
    let mut fds = [curl_waitfd { fd: abort.wake.r, events: CURL_WAIT_POLLIN, revents: 0 }];
    let mut numfds: c_int = 0;
    let rc = unsafe { curl_multi_wait(multi, fds.as_mut_ptr(), 1, timeout_ms, &mut numfds) };
    if rc != CURLM_OK {
        return Wait::Failed(rc);
    }
    if fds[0].revents != 0 {
        abort.wake.drain();
        return Wait::Woken;
    }
    if numfds > 0 {
        Wait::Ready
    } else {
        Wait::Timeout
    }
}

/// The `CURLcode` out of a `CURLMsg`'s union.
///
/// The union is pointer-sized and `CURLcode` is its first member, so on a little-endian target the
/// code is the low 4 bytes. Both targets this ships to are little-endian (armv7 webOS, and x86-64
/// / arm64 for the host suite and the Mac app); a big-endian port would have to revisit this, and
/// nothing else in the module.
unsafe fn msg_result(m: *const CURLMsg) -> c_int {
    let p = std::ptr::addr_of!((*m).data) as *const c_int;
    std::ptr::read_unaligned(p)
}

/// `CURLOPT_WRITEFUNCTION`: append body bytes for [`CurlSource::read`] to hand out.
extern "C" fn write_cb(ptr: *mut c_char, size: usize, nmemb: usize, ud: *mut c_void) -> usize {
    let n = size.saturating_mul(nmemb);
    if ud.is_null() || ptr.is_null() {
        return 0; // returning anything but n ends the transfer, which is right if we cannot store
    }
    let x = unsafe { &mut *(ud as *mut Xfer) };
    x.buf.extend_from_slice(unsafe { std::slice::from_raw_parts(ptr as *const u8, n) });
    n
}

/// `CURLOPT_HEADERFUNCTION`: one header line at a time, including the blank line that ends a
/// block, and including the lines of every intermediate response a redirect produces.
extern "C" fn header_cb(ptr: *mut c_char, size: usize, nmemb: usize, ud: *mut c_void) -> usize {
    let n = size.saturating_mul(nmemb);
    if ud.is_null() || ptr.is_null() {
        return 0;
    }
    let x = unsafe { &mut *(ud as *mut Xfer) };
    let line = unsafe { std::slice::from_raw_parts(ptr as *const u8, n) };
    parse_header_line(x, line);
    n
}

/// The header-line state machine, split out so the host suite can drive it with no libcurl at all.
///
/// Bytes, not `&str`: a header value is not required to be UTF-8, and one stray byte must not be
/// able to turn a good 206 into a transport failure. `stream.rs` learned the same lesson.
fn parse_header_line(x: &mut Xfer, line: &[u8]) {
    let t = trim_ascii(line);
    if t.is_empty() {
        // End of a header block. A 1xx is informational and a 3xx is about to be followed, so
        // neither is the FINAL response — marking either as done would stop an open early, with
        // the wrong status in hand.
        if !(100..200).contains(&x.status) && !(300..400).contains(&x.status) {
            x.headers_done = true;
        }
        return;
    }
    if t.starts_with(b"HTTP/") {
        // A new response begins: everything learned about the previous one is stale.
        x.status = status_of(t);
        x.body_len = -1;
        x.range_total = -1;
        x.range_start = -1;
        x.headers_done = false;
        return;
    }
    if let Some(v) = header_value(t, b"content-length:") {
        x.body_len = parse_i64(v).unwrap_or(-1);
    } else if let Some(v) = header_value(t, b"content-range:") {
        let (start, total) = parse_content_range(v);
        x.range_start = start;
        x.range_total = total;
    }
}

/// `HTTP/1.1 206 Partial Content` → 206. Zero when there is no three-digit code where one belongs.
fn status_of(line: &[u8]) -> c_int {
    let sp = match line.iter().position(|b| *b == b' ') {
        Some(i) => i + 1,
        None => return 0,
    };
    let digits = &line[sp..std::cmp::min(sp + 3, line.len())];
    if digits.len() < 3 || !digits.iter().all(|b| b.is_ascii_digit()) {
        return 0;
    }
    (digits[0] - b'0') as c_int * 100 + (digits[1] - b'0') as c_int * 10 + (digits[2] - b'0') as c_int
}

/// The value of `line` when it is the named header, matched case-insensitively (RFC 9110 §5.1).
fn header_value<'a>(line: &'a [u8], name_colon: &[u8]) -> Option<&'a [u8]> {
    if line.len() < name_colon.len() {
        return None;
    }
    if !line[..name_colon.len()].eq_ignore_ascii_case(name_colon) {
        return None;
    }
    Some(trim_ascii(&line[name_colon.len()..]))
}

/// `bytes 4-7/8` → `(4, 8)`. `-1` for either half we cannot read, including the `*/`-unknown-total
/// spelling the RFC allows.
fn parse_content_range(v: &[u8]) -> (i64, i64) {
    let v = trim_ascii(v);
    let rest = if v.len() >= 6 && v[..6].eq_ignore_ascii_case(b"bytes ") { &v[6..] } else { v };
    let rest = trim_ascii(rest);
    let slash = rest.iter().position(|b| *b == b'/');
    let (span, total) = match slash {
        Some(i) => (&rest[..i], parse_i64(trim_ascii(&rest[i + 1..])).unwrap_or(-1)),
        None => (rest, -1),
    };
    let start = match span.iter().position(|b| *b == b'-') {
        Some(i) => parse_i64(trim_ascii(&span[..i])).unwrap_or(-1),
        None => -1,
    };
    (start, total)
}

fn parse_i64(v: &[u8]) -> Option<i64> {
    if v.is_empty() || !v.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(v).ok()?.parse::<i64>().ok()
}

fn trim_ascii(mut v: &[u8]) -> &[u8] {
    while let [f, rest @ ..] = v {
        if f.is_ascii_whitespace() {
            v = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., l] = v {
        if l.is_ascii_whitespace() {
            v = rest;
        } else {
            break;
        }
    }
    v
}

/// The `CURLcode`s that mean something different from "the network is down". Same list as
/// `net.rs`'s, because the same firmware-varying OpenSSL and CA store sit under both, and a
/// support log that says "rc=60" and nothing else has already cost this project a day.
fn curl_why(rc: c_int) -> &'static str {
    match rc {
        6 => "could not resolve host",
        7 => "could not connect",
        28 => "timed out (or stalled below the low-speed floor)",
        35 => "TLS handshake failed (protocol too new for this firmware?)",
        60 => "peer certificate could not be verified (CA store too old?)",
        77 => "CA bundle could not be read",
        _ => "transport error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::AtomicUsize;

    /// **The gate every curl-driven test here opens with**, and it returns a HELD LOCK.
    ///
    /// Two process-globals are in play and both are load-bearing rather than incidental: net.rs's
    /// easy table (brought up the way boot does, so `available()` can be true), and this module's
    /// [`ACTIVE`] registry — which holds exactly ONE source, because exactly one media transfer
    /// exists at a time in the app. Under `cargo test`'s thread pool that assumption is false: two
    /// tests opening sources at once overwrite each other's registration, and then `abort_active`
    /// signals somebody else's transfer. That is not a hypothetical — it failed four of these
    /// tests in four different-looking ways (a seek refused, a connect running to its full
    /// timeout, a stalled read unblocking late, a registry found non-empty before its own open),
    /// none of which reads as "another test did this".
    ///
    /// So: hold the crate-wide lock for the WHOLE test (`lib.rs`'s `testlock`, not a local mutex —
    /// `ff.rs`'s curl-backed AVIO tests contend on the same registry from another module).
    /// `None` on a host with no libcurl at all, where these tests are vacuous and skip.
    fn curl_gate() -> Option<std::sync::MutexGuard<'static, ()>> {
        let g = crate::testlock::serial();
        if crate::net::global_init() && available() {
            Some(g)
        } else {
            None
        }
    }

    /// A loopback file server that speaks enough HTTP/1.1 for a byte-range pull, and counts BOTH
    /// connections accepted AND requests served.
    ///
    /// **Two counters, because one of them stopped being enough.** The socket transport sends
    /// `Connection: close` and reopens per seek, so "did it go back to the server" is exactly the
    /// accept count. libcurl keeps the connection in the multi handle's cache and REUSES it, which
    /// is the right behaviour for a media stream — it saves a whole TLS handshake on every seek —
    /// and it means a successful seek makes no new connection at all. Grading a curl seek on
    /// accepts alone would therefore assert nothing: refused and succeeded look identical. The
    /// request count is what moves.
    ///
    /// Keep-alive is likewise not a test convenience: a stand-in that closed after one request
    /// would make our own connection reuse untestable, and a real PMS speaks HTTP/1.1.
    ///
    /// `range_mode` decides what it does with a `Range` header: `Honour` answers 206 with a
    /// `Content-Range`, `Ignore` answers 200 with the whole body (the corruption case gate (b) is
    /// about — `python3 -m http.server` does exactly this), `Stall` answers headers and then never
    /// sends the body.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum RangeMode {
        Honour,
        Ignore,
        Stall,
        /// Answer a Range with 206 but omit the header that proves its starting offset.
        OmitContentRange,
        /// Fail the first seek request with 503, then honour a later retry.
        FailFirstSeek,
    }

    const BODY: &[u8] = b"ABCDEFGH";

    fn with_server(mode: RangeMode, body: impl FnOnce(u16, &AtomicUsize, &AtomicUsize)) {
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().unwrap().port();
        srv.set_nonblocking(true).expect("set_nonblocking"); // so the acceptor can be stopped
        let accepts = AtomicUsize::new(0);
        let requests = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        std::thread::scope(|sc| {
            sc.spawn(|| {
                while !stop.load(Ordering::Acquire) {
                    match srv.accept() {
                        Ok((s, _)) => {
                            // Bumped BEFORE the reply, so it is already final by the time any
                            // open against this listener can return — every assertion is causally
                            // behind that, and needs no sleep and no timing margin.
                            accepts.fetch_add(1, Ordering::AcqRel);
                            // One thread per connection: a keep-alive handler blocks, and a
                            // single-threaded acceptor that blocks cannot accept the second
                            // connection a non-reusing client would make.
                            sc.spawn(|| serve(s, mode, &requests, &stop));
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(1))
                        }
                        Err(_) => break,
                    }
                }
            });
            // Stop everything however we leave. A FAILING assertion in `body` unwinds through here
            // and `scope` joins before it reports, so a flag set only on the success path would
            // turn every real failure into a hang instead of a message.
            struct StopAll<'a>(&'a AtomicBool);
            impl Drop for StopAll<'_> {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Release);
                }
            }
            let _stop_on_exit = StopAll(&stop);
            body(port, &accepts, &requests);
        });
    }

    /// One connection: read a request head, answer it, repeat until the peer closes or the test
    /// ends. Reads into a persistent buffer rather than through `read_line`, because a read
    /// TIMEOUT is how the stop flag gets noticed and `BufRead::read_line` leaves its `String`
    /// unspecified on error — with keep-alive, the bytes after one head are the next request's.
    fn serve(s: std::net::TcpStream, mode: RangeMode, requests: &AtomicUsize, stop: &AtomicBool) {
        use std::io::Read;
        let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(100)));
        let mut w = match s.try_clone() {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let head = loop {
                if let Some(i) = buf.windows(4).position(|x| x == b"\r\n\r\n") {
                    let h = buf[..i + 4].to_vec();
                    buf.drain(..i + 4);
                    break h;
                }
                let mut tmp = [0u8; 1024];
                match (&s).read(&mut tmp) {
                    Ok(0) => return, // peer closed
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    Err(ref e)
                        if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
                    {
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            };
            let request_no = requests.fetch_add(1, Ordering::AcqRel) + 1;
            let start = range_start_of(&head);
            if mode == RangeMode::Stall {
                // Headers, then silence — the shape `tools/netcond.py --mode stall` produces
                // against a real PMS: the transfer is live and delivers nothing.
                let _ = w.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\n\r\n");
                let _ = w.flush();
                while !stop.load(Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                return;
            }
            if matches!(mode, RangeMode::Honour | RangeMode::FailFirstSeek) && start >= BODY.len() as i64 {
                let hdr = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\n\r\n",
                    BODY.len()
                );
                if w.write_all(hdr.as_bytes()).is_err() {
                    return;
                }
                let _ = w.flush();
                continue;
            }
            if mode == RangeMode::FailFirstSeek && start > 0 && request_no == 2 {
                if w.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n").is_err() {
                    return;
                }
                let _ = w.flush();
                continue;
            }
            let ranged = start > 0
                && matches!(mode, RangeMode::Honour | RangeMode::OmitContentRange | RangeMode::FailFirstSeek);
            let hdr = if ranged && mode == RangeMode::OmitContentRange {
                format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\n\r\n", BODY.len() - start as usize)
            } else if ranged {
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\n\r\n",
                    start,
                    BODY.len() - 1,
                    BODY.len(),
                    BODY.len() - start as usize
                )
            } else {
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", BODY.len())
            };
            let payload = if ranged { &BODY[start as usize..] } else { BODY };
            if w.write_all(hdr.as_bytes()).is_err() || w.write_all(payload).is_err() {
                return;
            }
            let _ = w.flush();
        }
    }

    /// `Range: bytes=4-` → 4. Zero when the request carries no usable range.
    fn range_start_of(head: &[u8]) -> i64 {
        for line in head.split(|b| *b == b'\n') {
            let t = String::from_utf8_lossy(line).trim().to_ascii_lowercase();
            if let Some(v) = t.strip_prefix("range: bytes=") {
                return v.trim_end_matches('-').parse().unwrap_or(0);
            }
        }
        0
    }

    fn read_all(src: &mut CurlSource) -> Vec<u8> {
        let mut out = Vec::new();
        let mut b = [0u8; 32];
        loop {
            let n = src.read(&mut b);
            if n <= 0 {
                return out;
            }
            out.extend_from_slice(&b[..n as usize]);
        }
    }

    // -- (a) a Range answered 206 resumes at the right offset -----------------------------------

    #[test]
    fn a_range_answered_206_resumes_at_the_requested_offset() {
        let Some(_gate) = curl_gate() else { return };
        with_server(RangeMode::Honour, |port, accepts, requests| {
            let mut src = CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0).expect("open");
            assert_eq!(read_all(&mut src), BODY, "the whole body at offset 0");
            assert!(src.seek(4), "a 206-honouring server must let the seek succeed");
            assert_eq!(read_all(&mut src), b"EFGH", "a seek to 4 must deliver byte 4 onward");
            assert_eq!(requests.load(Ordering::Acquire), 2, "the open and the seek are two requests");
            assert!(
                accepts.load(Ordering::Acquire) <= 2,
                "…on at most two connections — one is the reuse we want, two is a client that \
                 chose not to; both are correct, three would mean a leak"
            );
        });
    }

    // -- (b) a Range answered 200 is an ERROR ---------------------------------------------------

    /// The failure that looks like success. A server that ignores `Range` answers 200 from byte
    /// zero; accept that and the demuxer reads the head of the file believing it is at the seek
    /// target, which is corruption with a 2xx on it. `python3 -m http.server` behaves exactly this
    /// way, which is why `tests/serve_fixtures.py` exists.
    #[test]
    fn a_range_answered_200_is_refused_rather_than_restarted_from_zero() {
        let Some(_gate) = curl_gate() else { return };
        with_server(RangeMode::Ignore, |port, _, _| {
            let mut src = CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0).expect("open");
            assert!(!src.seek(4), "a 200 answer to a byte Range must FAIL the seek");
            // …and the source is POISONED, because a refused `avio_seek` does not stop
            // libavformat calling `read_cb` again. Serving the transfer the failed seek left
            // behind would be the same corruption by a different door.
            let mut b = [0u8; 8];
            assert_eq!(src.read(&mut b), -1, "after a refused seek nothing is at a known offset");
            assert!(!src.seek(0), "and the source stays refused rather than quietly recovering");
            assert_eq!(
                CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 4).err(),
                Some(OpenErr::RangeIgnored),
                "and an OPEN at a non-zero offset must refuse for the same reason"
            );
        });
    }

    /// A 416 is the correct response to a range at or past EOF. It fails that seek, but it says
    /// nothing bad about the server's range support, so a later valid seek must still work.
    #[test]
    fn a_416_seek_failure_does_not_poison_a_later_valid_seek() {
        let Some(_gate) = curl_gate() else { return };
        with_server(RangeMode::Honour, |port, _, requests| {
            let mut src = CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0).expect("open");
            assert!(!src.seek(BODY.len() as i64), "a seek exactly at EOF receives 416");
            let mut b = [0u8; 8];
            assert_eq!(src.read(&mut b), -1, "the failed seek must not masquerade as clean EOF");
            assert!(src.seek(4), "a valid seek after 416 must recover");
            assert_eq!(read_all(&mut src), b"EFGH");
            assert_eq!(requests.load(Ordering::Acquire), 3, "open, rejected EOF seek, recovered seek");
        });
    }

    #[test]
    fn a_206_without_content_range_is_refused_as_an_unverified_offset() {
        let Some(_gate) = curl_gate() else { return };
        with_server(RangeMode::OmitContentRange, |port, _, requests| {
            let mut src = CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0).expect("open");
            assert!(!src.seek(4), "206 alone does not prove which bytes the server returned");
            assert!(src.poisoned, "an unverified successful Range response is unsafe to reuse");
            assert_eq!(requests.load(Ordering::Acquire), 2, "open plus refused seek");
        });
    }

    #[test]
    fn a_transient_seek_status_does_not_poison_a_later_retry() {
        let Some(_gate) = curl_gate() else { return };
        with_server(RangeMode::FailFirstSeek, |port, _, requests| {
            let mut src = CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0).expect("open");
            assert!(!src.seek(4), "the first seek receives a transient 503");
            assert!(!src.poisoned, "an HTTP failure says nothing about the server's Range support");
            assert!(src.seek(4), "a later seek may recover once the server does");
            assert_eq!(read_all(&mut src), b"EFGH");
            assert_eq!(requests.load(Ordering::Acquire), 3, "open, failed seek, recovered seek");
        });
    }

    // -- (c) size comes from Content-Range / Content-Length --------------------------------------

    #[test]
    fn size_comes_from_content_range_then_content_length() {
        let Some(_gate) = curl_gate() else { return };
        with_server(RangeMode::Honour, |port, _, _| {
            let mut src = CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0).expect("open");
            assert_eq!(src.size(), BODY.len() as i64, "Content-Length at offset 0 IS the size");
            assert!(src.seek(4));
            assert_eq!(src.size(), BODY.len() as i64, "Content-Range's total must not shrink to the tail length");
        });
    }

    /// The header parse itself, with no libcurl involved — so the arithmetic is graded even on a
    /// host where the tests above are vacuous.
    #[test]
    fn content_range_and_content_length_parse_to_the_whole_resource() {
        let mut x = Xfer::new();
        parse_header_line(&mut x, b"HTTP/1.1 206 Partial Content\r\n");
        parse_header_line(&mut x, b"Content-Range: bytes 4-7/8\r\n");
        parse_header_line(&mut x, b"content-length: 4\r\n");
        parse_header_line(&mut x, b"\r\n");
        assert_eq!((x.status, x.range_start, x.range_total, x.body_len), (206, 4, 8, 4));
        assert!(x.headers_done);

        // A redirect's block must NOT end the open — the status that matters is the next one.
        let mut r = Xfer::new();
        parse_header_line(&mut r, b"HTTP/1.1 302 Found\r\n");
        parse_header_line(&mut r, b"\r\n");
        assert!(!r.headers_done, "a 3xx is followed, so its header block is not the final one");
        parse_header_line(&mut r, b"HTTP/1.1 200 OK\r\n");
        parse_header_line(&mut r, b"Content-Length: 99\r\n");
        parse_header_line(&mut r, b"\r\n");
        assert_eq!((r.status, r.body_len, r.headers_done), (200, 99, true));

        // `bytes 0-9/*` — the RFC's unknown-total spelling — must read as unknown, not as 0.
        assert_eq!(parse_content_range(b"bytes 0-9/*"), (0, -1));
        // A non-UTF-8 byte in a value cannot turn a good response into a failure.
        let mut b = Xfer::new();
        parse_header_line(&mut b, b"HTTP/1.1 200 OK\r\n");
        parse_header_line(&mut b, b"Server: caf\xC3\x28\r\n");
        parse_header_line(&mut b, b"Content-Length: 8\r\n");
        assert_eq!((b.status, b.body_len), (200, 8));
    }

    #[test]
    fn redirects_never_downgrade_a_tls_media_origin() {
        assert_eq!(allowed_redirect_protocols(b"https://example.invalid/media"), CURLPROTO_HTTPS);
        assert_eq!(
            allowed_redirect_protocols(b"http://example.invalid/media"),
            CURLPROTO_HTTP | CURLPROTO_HTTPS,
            "plaintext may upgrade, but the inverse is forbidden"
        );
    }

    // -- (d) abort during connect and during read unblocks promptly ------------------------------

    /// `192.0.2.1` is RFC 5737 TEST-NET-1: routed nowhere, so the connect hangs for the full
    /// `CURLOPT_CONNECTTIMEOUT`. Numeric on purpose — this grades the CONNECT window, and going
    /// through a name would put `getaddrinfo` in the way, which is the one place the pipe provably
    /// cannot reach (module doc, clause 3).
    #[test]
    fn abort_during_connect_unblocks_far_inside_the_connect_timeout() {
        let Some(_gate) = curl_gate() else { return };
        let started = std::time::Instant::now();
        let opened = std::sync::Barrier::new(2);
        std::thread::scope(|sc| {
            sc.spawn(|| {
                opened.wait();
                std::thread::sleep(std::time::Duration::from_millis(120));
                abort_active();
            });
            opened.wait();
            let r = CurlSource::open("http://192.0.2.1:32400/f.mkv", 0);
            assert_eq!(r.err(), Some(OpenErr::Aborted), "a teardown during connect must report Aborted");
        });
        let took = started.elapsed();
        assert!(
            took < std::time::Duration::from_secs(CONNECT_TIMEOUT_S as u64 - 5),
            "the abort must not have waited out CONNECT_TIMEOUT ({CONNECT_TIMEOUT_S}s): took {took:?}"
        );
    }

    /// The other half: parked in `curl_multi_wait` with headers in hand and no body coming.
    #[test]
    fn abort_during_a_stalled_read_unblocks_promptly() {
        let Some(_gate) = curl_gate() else { return };
        with_server(RangeMode::Stall, |port, _, _| {
            let mut src = CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0).expect("open");
            let started = std::time::Instant::now();
            std::thread::scope(|sc| {
                // Through `abort_active`, i.e. exactly what `engine::teardown` calls from the MAIN
                // thread — a `&CurlSource` is not `Sync` and must not be handed across a thread.
                sc.spawn(|| {
                    std::thread::sleep(std::time::Duration::from_millis(120));
                    abort_active();
                });
                let mut b = [0u8; 32];
                assert_eq!(src.read(&mut b), -1, "an aborted read must report an error, not EOF");
            });
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "the low-speed floor is {LOW_SPEED_TIME_S}s; the pipe must beat it by an order of magnitude"
            );
        });
    }

    // -- (e) after abort, no second connection --------------------------------------------------

    /// Graded by the ACCEPT COUNT, not by a return value: a `seek` that reconnected and then
    /// failed for an unrelated reason would also return false. The count is the invariant, because
    /// the socket teardown is already waiting on is the one a fresh connection escapes.
    #[test]
    fn repeated_seeks_after_abort_open_no_second_connection() {
        let Some(_gate) = curl_gate() else { return };
        with_server(RangeMode::Honour, |port, accepts, requests| {
            let mut src = CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0).expect("open");
            assert_eq!(accepts.load(Ordering::Acquire), 1, "fixture: exactly one connection so far");
            assert_eq!(requests.load(Ordering::Acquire), 1, "fixture: and exactly one request");
            src.abort();
            let mut b = [0u8; 8];
            for _ in 0..8 {
                assert_eq!(src.read(&mut b), -1, "every aborted read must refuse");
                assert!(!src.seek(4), "every aborted seek must refuse");
            }
            // REQUESTS is the load-bearing half: libcurl would reuse the cached connection, so a
            // source that went back to the server would ask again WITHOUT accepting again.
            assert_eq!(
                requests.load(Ordering::Acquire), 1,
                "an aborted source issued another request — that is the wedge, not merely a slow teardown"
            );
            assert_eq!(accepts.load(Ordering::Acquire), 1, "and it opened no new connection either");
        });
    }

    // -- (f) the open publishes status and length -----------------------------------------------

    #[test]
    fn the_initial_open_publishes_status_and_length_for_the_diagnostics_readout() {
        let Some(_gate) = curl_gate() else { return };
        with_server(RangeMode::Honour, |port, _, _| {
            let src = CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0).expect("open");
            assert_eq!(src.status(), 200, "ff::demux stores this in SHARED.dg_http_status");
            assert_eq!(src.size(), BODY.len() as i64, "…and this in SHARED.file_size");
        });
    }

    // -- (g) the second table being unavailable is a clean failure, not a panic ------------------

    /// **The entire reason this module has its own `dynlib!` table.**
    ///
    /// `dynlib::load_into` is all-or-nothing, so a television missing one `curl_multi_*` symbol
    /// empties whichever table holds it. If that were net.rs's table, the set would lose plex.tv
    /// SIGN-IN — not just streaming. Two halves are graded here: an unavailable multi table
    /// refuses cleanly (no `dynlib::missing_symbol` panic), and a failed load of it leaves net's
    /// easy table — the one sign-in runs on — untouched.
    #[test]
    fn an_unavailable_multi_table_refuses_cleanly_and_leaves_sign_in_alone() {
        let _g = crate::testlock::serial();
        assert_eq!(
            CurlSource::open_gated("http://127.0.0.1:9/f.mkv", 0, false).err(),
            Some(OpenErr::Unavailable),
            "no libcurl multi must be a return value, never a panic through a null pointer"
        );
        let net_ok = crate::net::global_init();
        // A load that finds nothing publishes nothing (dynlib's own contract), so this cannot
        // disturb a table that is already live — which is precisely the claim being made.
        let v = crate::dynlib::load_into(
            None,
            &["libplxnative-no-such-curl.so.99"],
            &[("curl_multi_init", &curlmulti::curl_multi_init)],
        );
        assert!(matches!(v, crate::dynlib::Loaded::NoLibrary));
        if net_ok {
            assert!(easy_ready(), "a failed curl_multi_* load must not cost the app its sign-in transport");
        }
    }

    // -- (h) two sequential wake/abort cycles ---------------------------------------------------

    /// **The mechanical detector for "drain the pipe".**
    ///
    /// An undrained pipe passes every other test in this file: the first abort still works, the
    /// source still tears down, nothing returns a wrong value. It shows up only as a
    /// `curl_multi_wait` that returns instantly forever after, i.e. a spinning demux thread in
    /// production. So the middle assertion here is the one that matters — after a wake, a wait on
    /// an unsignalled pipe must actually BLOCK to its timeout.
    #[test]
    fn two_sequential_abort_cycles_each_block_until_signalled() {
        let Some(_gate) = curl_gate() else { return };
        let abort = Abort::new().expect("pipe");
        let multi = unsafe { curl_multi_init() };
        assert!(!multi.is_null());

        abort.wake.signal();
        assert_eq!(multi_wait(multi, &abort, 2000), Wait::Woken, "cycle 1: the byte must wake the wait");

        let t = std::time::Instant::now();
        let quiet = multi_wait(multi, &abort, 150);
        assert_ne!(quiet, Wait::Woken, "the pipe was not drained — every later wait returns instantly (a spin)");
        assert!(
            t.elapsed() >= std::time::Duration::from_millis(60),
            "an unsignalled wait must block, not return at once: took {:?}",
            t.elapsed()
        );

        abort.wake.signal();
        assert_eq!(multi_wait(multi, &abort, 2000), Wait::Woken, "cycle 2: a drained pipe must still be signallable");

        unsafe { curl_multi_cleanup(multi) };
    }

    #[test]
    fn a_multi_wait_error_is_not_reported_as_a_timeout_tick() {
        let Some(_gate) = curl_gate() else { return };
        let abort = Abort::new().expect("pipe");
        assert!(
            matches!(multi_wait(std::ptr::null_mut(), &abort, 0), Wait::Failed(_)),
            "a bad multi handle returns immediately and must terminate, not spin as Timeout"
        );
    }

    #[test]
    fn a_multi_perform_error_is_not_reported_as_clean_eof() {
        let Some(_gate) = curl_gate() else { return };
        with_server(RangeMode::Honour, |port, _, _| {
            let mut src = CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0).expect("open");
            src.xfer.buf.clear();
            src.xfer.pos = 0;

            // A null handle is libcurl's supported BAD_HANDLE path. Restore the real handle before
            // drop so normal cleanup still removes the attached easy handle from its owner.
            let real_multi = src.multi;
            src.multi = std::ptr::null_mut();
            let result = src.perform();
            src.multi = real_multi;

            assert!(matches!(result, Err(OpenErr::Multi(_))));
            assert!(src.failed && src.done && src.multi_failed, "the failure must persist past its call");
            let mut b = [0u8; 8];
            assert_eq!(src.read(&mut b), -1, "a later read must not reinterpret the failure as EOF");
            assert!(src.seek(4), "a retry must rebuild the failed multi handle before reuse");
            assert_eq!(read_all(&mut src), b"EFGH");
            assert!(!src.multi_failed, "the replacement multi handle is healthy");
        });
    }

    #[test]
    fn add_handle_failure_marks_the_multi_for_replacement() {
        let Some(_gate) = curl_gate() else { return };
        with_server(RangeMode::Honour, |port, _, _| {
            let mut src = CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0).expect("open");
            src.stop();
            let real_multi = src.multi;
            src.multi = std::ptr::null_mut();

            let result = src.start(4);

            assert!(matches!(result, Err(OpenErr::Multi(_))));
            assert!(src.failed && src.done && src.multi_failed);
            // Restore the valid owner only so `start` can retire it safely before constructing the
            // replacement. The easy from the failed add was never attached to it.
            src.multi = real_multi;
            assert!(src.seek(4), "the next seek must replace the failed multi before adding");
            assert_eq!(read_all(&mut src), b"EFGH");
        });
    }

    #[test]
    fn remove_handle_failure_abandons_uncertain_ownership_before_rebuilding() {
        let Some(_gate) = curl_gate() else { return };
        with_server(RangeMode::Honour, |port, _, _| {
            let mut src = CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0).expect("open");
            let real_multi = src.multi;
            let real_easy = src.easy;
            assert_eq!(unsafe { curl_multi_remove_handle(real_multi, real_easy) }, CURLM_OK);
            src.multi = std::ptr::null_mut();

            src.stop();

            assert!(src.easy.is_null());
            assert!(src.multi_failed, "a failed removal makes the old multi unsafe to reuse");
            // The injected failure used an easy we KNOW was pre-detached above, so the test can
            // reclaim it. Production cannot know that and deliberately leaks this exceptional pair.
            unsafe { crate::net::curl_easy_cleanup(real_easy) };
            src.multi = real_multi;
            assert!(src.seek(4), "the next seek must retire and replace that multi");
            assert_eq!(read_all(&mut src), b"EFGH");
        });
    }

    /// **The instrument check.** Every curl-driven test above SKIPS when libcurl cannot be bound,
    /// which is right on a host that has none and is a green suite grading nothing anywhere else.
    /// This is the line that makes the difference visible — the project rule is
    /// `[[silent-instrument-trap]]`: prove the instrument can see the thing before reading its
    /// silence.
    ///
    /// If this fails, the tests above did not run. It is not a bug in them: this host cannot
    /// `dlopen` any of `net.rs`'s three candidates, which also means the app on it cannot sign in
    /// to plex.tv at all. Install libcurl rather than deleting this.
    #[test]
    fn libcurl_binds_on_this_host_so_the_tests_above_are_not_vacuous() {
        let _g = crate::testlock::serial();
        assert!(crate::net::global_init(), "net.rs could not bind any libcurl candidate on this host");
        assert!(available(), "curl_multi_* did not resolve — every curl-driven test here skipped");
    }

    /// The flag and the wake are one signal: a thread woken by the byte must be able to SEE the
    /// abort, or it goes straight back to waiting and the teardown is lost.
    #[test]
    fn the_abort_flag_is_visible_to_whoever_the_byte_wakes() {
        let abort = Abort::new().expect("pipe");
        assert!(!abort.is_set());
        abort.signal();
        assert!(abort.is_set(), "signal sets the flag BEFORE it writes the byte");
    }

    /// The registry, which is what `player::engine::teardown` reaches through. A source registers
    /// on open and retires on drop, so a teardown after playback ends signals nothing.
    #[test]
    fn the_registry_holds_one_source_and_retires_it_on_drop() {
        let Some(_gate) = curl_gate() else { return };
        with_server(RangeMode::Honour, |port, _, _| {
            assert!(lock_active().is_none(), "fixture: nothing registered before the open");
            {
                let src = CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0).expect("open");
                assert!(lock_active().is_some(), "an open source is what teardown must be able to signal");
                abort_active();
                assert!(src.abort.is_set(), "abort_active must reach THIS source");
            }
            assert!(lock_active().is_none(), "a dropped source must retire, so a later teardown is a no-op");
            abort_active(); // must not panic, must not write to a closed fd
        });
    }

    /// The lost-wake regression: demux publishes a reservation, teardown fires, and only then
    /// does the demuxer finish constructing the source. The open must observe the already-latched
    /// abort before it sends even one byte to the server.
    #[test]
    fn teardown_between_reservation_and_open_cannot_start_a_connection() {
        let Some(_gate) = curl_gate() else { return };
        with_server(RangeMode::Honour, |port, accepts, requests| {
            let reservation = CurlSource::reserve_open().expect("reserve");
            abort_active();
            let result = CurlSource::open_reserved(&format!("http://127.0.0.1:{port}/f.mkv"), 0, reservation);
            assert!(matches!(result, Err(OpenErr::Aborted)));
            assert_eq!(accepts.load(Ordering::Acquire), 0, "no connection may begin after teardown");
            assert_eq!(requests.load(Ordering::Acquire), 0, "and no request may leave");
            assert!(lock_active().is_none(), "the failed open retires its reservation");
        });
    }
}
