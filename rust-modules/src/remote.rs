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

/// The control FIFO. Kept in the `plxnative-*` dev namespace for consistency — and resolved
/// through [`crate::paths`] rather than a literal, so a host build running several simulators at
/// once gives each its own FIFO instead of all of them draining one.
fn fifo_path() -> std::path::PathBuf {
    crate::paths::in_runtime_dir("plxnative-remote")
}

pub struct Remote {
    fd: RawFd,
    /// Bytes read but not yet terminated by whitespace — an unfinished token held
    /// across frames (a host write can be split over two reads).
    buf: String,
}

impl Remote {
    /// Create + open the control FIFO non-blocking. `O_RDWR` keeps a writer end open
    /// on our side so reads never hit EOF between host writes (the standard self-pipe
    /// trick). Returns `None` on any failure — the app then just runs without a remote:
    /// silently when the feature is off (the expected case in a release build), with a
    /// logged errno when the FIFO itself could not be opened.
    pub fn open() -> Option<Remote> {
        // Gated HERE rather than at the call site, so there is exactly one door and the caller's
        // `remote.as_mut()` type-checks unchanged in both feature sets.
        //
        // This is the surface that most needs to go. `/tmp` is mode 1777 in BOTH jail profiles,
        // the `mkfifo` below deliberately ignores EEXIST, and `open()` then attaches to whatever
        // object already sits at that path — so any co-resident process on an ordinary user's TV
        // can create its own FIFO there first and drive the UI through the one real key and
        // pointer handler. Not a theoretical hole: `ck:X,Y` clicks replay through the same path
        // as a physical remote. It was ungated on every boot, before the event loop, with no
        // trigger file required to arm it.
        if !crate::dev::ENABLED {
            return None;
        }
        // Exact bytes, not `to_string_lossy`: an instance root comes from the environment, and a
        // lossy conversion would silently `mkfifo` a DIFFERENT path than the one asked for.
        use std::os::unix::ffi::OsStringExt;
        let path = std::ffi::CString::new(fifo_path().into_os_string().into_vec()).ok()?;
        unsafe {
            // mkfifo; an existing FIFO (EEXIST) is fine, anything else and open() fails below.
            libc::mkfifo(path.as_ptr(), 0o666);
            let fd = libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK);
            if fd < 0 {
                // Logged, unlike the `dev::ENABLED` return above — that one is a build behaving
                // as it was built, and a line for it would print on every release boot. This one
                // is a real failure, and nothing downstream reports it: `app.rs` calls
                // `Remote::open()` without branching on the answer, so the app runs on and every
                // `ok` / `ck:X,Y` written to the FIFO is read by nobody. A `tools/stream-screen.py`
                // session clicking a picture that never moves looks like a wedged app rather than
                // a channel that was never opened.
                //
                // The errno is the diagnosis, precisely because the `mkfifo` above ignores EEXIST
                // on purpose: `open` therefore attaches to whatever object already sits at this
                // path, and errno is what says what was wrong with it. EACCES is the hazard
                // `docs/agent-reference.md` already records for the event log — "never pre-create the event log
                // on the TV — a root-owned file left in place is one it cannot write" — reaching
                // the same 1777 `/tmp` from the same jailed uid, one path over.
                //
                // `last_os_error()` rather than a raw errno deref, as `capture.rs` does and for
                // its reason: `__errno_location` is a glibc symbol, and reading errno portably is
                // what keeps this crate compiling (and host-testing) off-device. Taken into a
                // local BEFORE the path is rebuilt for the message, so nothing runs between the
                // failed call and the read.
                let err = std::io::Error::last_os_error();
                crate::log(&format!(
                    "remote: open {} failed ({err}) — no remote control this run",
                    fifo_path().display()
                ));
                return None;
            }
            Some(Remote {
                fd,
                buf: String::new(),
            })
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
            self.buf
                .push_str(&String::from_utf8_lossy(&tmp[..n as usize]));
            if (n as usize) < tmp.len() {
                break;
            }
        }
        if self.buf.is_empty() {
            return;
        }
        // Everything up to and including the last whitespace is complete; the tail is
        // an unterminated token to carry over.
        //
        // ASCII whitespace SPECIFICALLY, not `char::is_whitespace`. The split below slices the
        // buffer at `i` and `i + 1` — byte indices — which is only a char boundary when the
        // matched separator is ONE byte long. `char::is_whitespace` also matches U+00A0, U+2028,
        // U+3000 and friends, and `rfind` reports the byte index where such a char STARTS, so
        // both `buf[..=i]` and `buf[i + 1..]` landed mid-codepoint and panicked. This drains a
        // world-writable FIFO in /tmp on a rooted TV, once per frame on every boot, so the
        // panicking input is one stray keystroke away from any process on the box — and a panic
        // here unwinds out of the SDL loop and takes the app down.
        //
        // Restricting the SEPARATOR search to ASCII loses nothing: the protocol is ASCII tokens
        // by construction (`app.rs::remote_token_key` matches `up`/`down`/`ok`/`back`/… and the
        // `ck:X,Y` pointer form, and `tools/stream-screen.py` only ever writes those, one per
        // `\n`). Note the `split_whitespace` below is deliberately left Unicode-aware: it
        // operates on a `&str` and never indexes it, so a stray NBSP *between* two tokens still
        // separates them correctly once an ASCII newline terminates the run.
        match self.buf.rfind(|c: char| c.is_ascii_whitespace()) {
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

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// Run `drain` over a pre-loaded buffer with **no FIFO**: fd = -1 makes the `read(2)` fail
    /// with EBADF on the first call, so `drain` falls straight through to the tokenizer — which
    /// is the half under test. (`Drop` then `close(-1)`s, which is a harmless EBADF too.)
    /// Returns the tokens it emitted and the tail it kept for the next frame.
    fn drain_buffer(pre: &str) -> (Vec<String>, String) {
        let mut r = Remote {
            fd: -1,
            buf: pre.to_string(),
        };
        let mut out = Vec::new();
        r.drain(|t| out.push(t.to_string()));
        (out, r.buf.clone())
    }

    /// Regression: the separator search was `char::is_whitespace`, whose match can be several
    /// bytes wide, while the split that follows is by BYTE index — so any multi-byte whitespace
    /// sliced through the middle of a codepoint and panicked, out of the SDL loop, taking the
    /// app with it. U+00A0 arrives from an ordinary copy-paste; U+3000 from a CJK keyboard; the
    /// FIFO is world-writable in /tmp, so neither needs to be our own fault.
    ///
    /// The trailing-NBSP case is the exact panic: `rfind` returned the index where the NBSP
    /// STARTS, and `buf[..=i]` then cut between its two bytes.
    #[test]
    fn a_multibyte_whitespace_does_not_panic_the_tokenizer() {
        for pre in ["down\u{a0}", "\u{3000}", "ok\u{2028}", "up\u{a0}\u{3000}"] {
            let (toks, tail) = drain_buffer(pre);
            assert!(
                toks.is_empty(),
                "{pre:?}: no ASCII terminator, so nothing is complete yet"
            );
            assert_eq!(
                tail, pre,
                "{pre:?}: the unterminated tail must be carried over intact"
            );
        }
    }

    /// …and once an ASCII terminator does arrive, a multi-byte whitespace sitting BETWEEN two
    /// tokens still separates them: the ready slice is handed to `split_whitespace`, which is
    /// Unicode-aware and never indexes, so restricting only the *search* to ASCII costs nothing.
    #[test]
    fn a_multibyte_whitespace_between_tokens_still_separates_them() {
        let (toks, tail) = drain_buffer("up\u{a0}ok\n");
        assert_eq!(toks, ["up", "ok"]);
        assert!(tail.is_empty(), "a newline-terminated write leaves no tail");
    }

    /// The ordinary protocol, unchanged: complete tokens fire, the unterminated tail waits for
    /// the next frame (a host write can be split across two reads).
    #[test]
    fn complete_tokens_fire_and_a_partial_one_is_held_over() {
        let (toks, tail) = drain_buffer("down ok\r\nck:100,200\nle");
        assert_eq!(toks, ["down", "ok", "ck:100,200"]);
        assert_eq!(
            tail, "le",
            "the partial token must survive to be completed next frame"
        );
    }
}
