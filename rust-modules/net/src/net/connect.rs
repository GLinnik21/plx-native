//! **A TLS connection held open both ways** ([`super::connect_tls`]): libcurl connects and
//! verifies the server exactly as for any request (`CURLOPT_CONNECT_ONLY` on the one request
//! function, so the trust store, the key-pin and roots rungs and the timeouts are the ordinary
//! ones), and then hands the connection to a caller that speaks its own protocol over it with
//! `curl_easy_send` / `curl_easy_recv`. FinNative's Jellyfin WebSocket is that caller.
//!
//! # The two symbols are a table of their own
//!
//! [`plx_base::dynlib::load_into`] is all-or-nothing, so a symbol added to `net.rs`'s table would,
//! on a firmware without it, empty that table and take sign-in with it. These two are therefore a
//! second table, as `curlio`'s multi symbols are: a libcurl without them loses only this. Both
//! have existed since libcurl 7.18.2; the oldest the project records on a television is 7.53.1.
//!
//! # Reads wait on the socket, a tick at a time
//!
//! `curl_easy_recv` does not block: it answers `CURLE_AGAIN` when nothing is buffered. A read
//! then waits on the connection's socket (`CURLINFO_LASTSOCKET`, the info `CONNECT_ONLY` exists
//! to pair with) for at most [`TlsSocket`]'s tick and reports `WouldBlock`, so the caller can look
//! at its cancel flag between reads. TLS can hold decrypted bytes curl has not handed out yet, so
//! the socket is only waited on after `curl_easy_recv` said it had nothing.

use super::{c_int, c_long, c_void, CURL};
use std::cell::{Cell, RefCell};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;
use std::time::{Duration, Instant};

plx_base::dynlib! {
    /// libcurl's connect-only I/O (7.18.2), apart from `net.rs`'s table: see the module doc.
    curlsock: ["libcurl.so.4", "libcurl.so.5", "libcurl.4.dylib"] {
    fn curl_easy_send(handle: *mut CURL, buffer: *const c_void, length: usize, sent: *mut usize) -> c_int;
    fn curl_easy_recv(handle: *mut CURL, buffer: *mut c_void, length: usize, received: *mut usize) -> c_int;
}}

/// `CURLOPT_CONNECT_ONLY` (LONG + 141): connect, handshake, and stop before any request.
pub(super) const CURLOPT_CONNECT_ONLY: c_int = 141;
/// `CURLINFO_LASTSOCKET` (LONG + 29): the socket a connect-only handle holds.
const CURLINFO_LASTSOCKET: c_int = 0x20_0000 + 29;
/// `CURLE_AGAIN`: nothing to read, or the socket cannot take more yet.
const CURLE_AGAIN: c_int = 81;

/// How long one read waits before it reports `WouldBlock`.
const TICK: Duration = Duration::from_millis(250);
/// How long one write may wait for the socket to take its bytes.
const WRITE_WAIT: Duration = Duration::from_secs(10);

static LOAD: Once = Once::new();
static OK: AtomicBool = AtomicBool::new(false);

/// Can this libcurl hold a connection for a caller? Loads the table once.
pub fn available() -> bool {
    LOAD.call_once(|| match curlsock::load(None) {
        plx_base::dynlib::Loaded::Ok(soname) => {
            plx_base::eventlog::log(&format!("net: bound {soname} curl_easy_send/recv"));
            OK.store(true, Ordering::Release);
        }
        plx_base::dynlib::Loaded::NoLibrary => {}
        plx_base::dynlib::Loaded::Incomplete(soname, n) => plx_base::eventlog::log(&format!(
            "net: {soname} is missing {n} of curl_easy_send/recv — no held TLS connections (requests are unaffected)"
        )),
    });
    OK.load(Ordering::Acquire)
}

/// A verified TLS connection, valid for the length of the caller's session.
pub struct TlsSocket {
    easy: *mut CURL,
    fd: c_int,
}

impl TlsSocket {
    /// Bytes from the server: `Ok(0)` when it closed the connection, an error of kind
    /// `WouldBlock` when nothing arrived within a tick.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        for waited in [false, true] {
            let mut got = 0usize;
            // SAFETY: `easy` is the live connect-only handle `ConnectSink::run` lent us; `buf` is ours.
            let rc = unsafe { curl_easy_recv(self.easy, buf.as_mut_ptr() as *mut c_void, buf.len(), &mut got) };
            match rc {
                0 => return Ok(got),
                CURLE_AGAIN if !waited => {
                    if !wait(self.fd, libc::POLLIN, TICK) {
                        break;
                    }
                }
                CURLE_AGAIN => break,
                _ => return Err(io::Error::other(format!("curl rc={rc}"))),
            }
        }
        Err(io::ErrorKind::WouldBlock.into())
    }

    pub fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        let deadline = Instant::now() + WRITE_WAIT;
        while !bytes.is_empty() {
            let mut sent = 0usize;
            // SAFETY: as in `read`.
            let rc = unsafe { curl_easy_send(self.easy, bytes.as_ptr() as *const c_void, bytes.len(), &mut sent) };
            match rc {
                0 => bytes = &bytes[sent..],
                CURLE_AGAIN => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() || !wait(self.fd, libc::POLLOUT, left) {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                }
                _ => return Err(io::Error::other(format!("curl rc={rc}"))),
            }
        }
        Ok(())
    }
}

/// Wait for `events` on `fd` for at most `how_long`. True when it is ready.
fn wait(fd: c_int, events: libc::c_short, how_long: Duration) -> bool {
    let mut p = libc::pollfd { fd, events, revents: 0 };
    let ms = c_int::try_from(how_long.as_millis()).unwrap_or(c_int::MAX);
    // SAFETY: one valid pollfd for the duration of the call.
    unsafe { libc::poll(&mut p, 1, ms) > 0 }
}

/// The caller's session, run once on the attempt whose handshake succeeded.
pub(super) struct ConnectSink<'a> {
    session: RefCell<&'a mut dyn FnMut(&mut TlsSocket)>,
    ran: Cell<bool>,
}

impl<'a> ConnectSink<'a> {
    pub(super) fn new(session: &'a mut dyn FnMut(&mut TlsSocket)) -> Self {
        ConnectSink { session: RefCell::new(session), ran: Cell::new(false) }
    }

    /// Run the session on `easy`, a connect-only handle whose handshake succeeded. A handle that
    /// does not name its socket is not run: there would be nothing to wait on.
    ///
    /// # Safety
    /// `easy` is a live easy handle that stays valid until this returns.
    pub(super) unsafe fn run(&self, easy: *mut CURL) {
        let mut fd: c_long = -1;
        if super::curl_easy_getinfo_long(easy, CURLINFO_LASTSOCKET, &mut fd) != 0 || fd < 0 {
            return;
        }
        let mut socket = TlsSocket { easy, fd: fd as c_int };
        self.ran.set(true);
        (self.session.borrow_mut())(&mut socket);
    }

    pub(super) fn ran(&self) -> bool {
        self.ran.get()
    }
}
