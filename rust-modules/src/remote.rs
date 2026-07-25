//! Remote-control channel (dev / on-device testing): a FIFO the event loop drains
//! once per frame, so a key token written from another machine — `echo down >
//! /tmp/plxnative-remote` over SSH — drives the UI exactly like the physical remote.
//!
//! Why in-app instead of injecting system input: on this webOS 4.5 build the wayland
//! compositor (`surface-manager`) opens a FIXED set of evdev nodes at boot (the RCU /
//! Magic-Remote devices) and does not pick up hotplugged or `uinput` devices, so an
//! external virtual keyboard never reaches our SDL surface; and LG's
//! `com.webos.service.tv.keymanager/createKeyEvent` injects into the webOS web-app key
//! layer, not the wayland path we read. The reliable path is therefore in-app: read a
//! token here, and let `app.rs` synthesize the SDL key event so the ONE real key
//! handler runs unchanged. This mirrors the existing `/tmp/plxnative-*` dev-trigger
//! design; `tools/stream-screen.py` is the host-side driver. Boot-neutral: the FIFO is
//! excluded from `automated_boot`, so its presence never changes the boot flow.

use libc::c_void;
use std::os::unix::io::RawFd;

/// The control FIFO. Kept in the `/tmp/plxnative-*` dev namespace for consistency.
pub const FIFO_PATH: &str = "/tmp/plxnative-remote";

pub struct Remote {
    fd: RawFd,
    /// Bytes read but not yet terminated by whitespace — an unfinished token held
    /// across frames (a host write can be split over two reads).
    buf: String,
}

impl Remote {
    /// Create + open the control FIFO non-blocking. `O_RDWR` keeps a writer end open
    /// on our side so reads never hit EOF between host writes (the standard self-pipe
    /// trick). Returns `None` on any failure — the app then just runs without a remote.
    pub fn open() -> Option<Remote> {
        let path = std::ffi::CString::new(FIFO_PATH).ok()?;
        unsafe {
            // mkfifo; an existing FIFO (EEXIST) is fine, anything else and open() fails below.
            libc::mkfifo(path.as_ptr(), 0o666);
            let fd = libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK);
            if fd < 0 {
                return None;
            }
            Some(Remote { fd, buf: String::new() })
        }
    }

    /// Drain all pending bytes and call `f` once per complete whitespace-delimited
    /// token. A trailing partial token (no terminating whitespace yet) is retained for
    /// the next frame so a split write is never mis-parsed.
    pub fn drain(&mut self, mut f: impl FnMut(&str)) {
        let mut tmp = [0u8; 512];
        loop {
            let n = unsafe { libc::read(self.fd, tmp.as_mut_ptr() as *mut c_void, tmp.len()) };
            if n <= 0 {
                break; // EAGAIN (empty) or error → nothing more this frame
            }
            self.buf.push_str(&String::from_utf8_lossy(&tmp[..n as usize]));
            if (n as usize) < tmp.len() {
                break;
            }
        }
        if self.buf.is_empty() {
            return;
        }
        // Everything up to and including the last whitespace is complete; the tail is
        // an unterminated token to carry over.
        match self.buf.rfind(char::is_whitespace) {
            Some(i) => {
                let ready = self.buf[..=i].to_string();
                self.buf = self.buf[i + 1..].to_string();
                for tok in ready.split_whitespace() {
                    f(tok);
                }
            }
            None => {} // no complete token yet — keep buffering
        }
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}
