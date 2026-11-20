//! Remote-control keys: the Ecore event callbacks (main thread) and the queue the render thread
//! drains.
//!
//! Ecore is not thread-safe and its main loop runs on the host's main thread, so the handlers are
//! registered there (inside `plx_tizen_create`) and do almost nothing: log, copy the strings out of
//! the event, push onto [`QUEUE`]. The render thread pops and advances the clear colour.

use crate::log::plog;
use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_uint, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// Mirror of EFL's `Ecore_Event_Key` (Ecore_Input.h) as far as `keycode`.
///
/// UNVERIFIED ON DEVICE. The layout is the upstream EFL header's; Samsung's Tizen EFL fork may
/// insert or reorder fields, and the flutter-tizen embedder only ever reads `key`, `string`,
/// `compose`, `modifiers`, `keycode`, `window` and `dev` through the real header. This crate has no
/// header, so: only `keyname`, `key` (the first two pointers, which every EFL version has first)
/// and `keycode` are read, and the first events are also hex-dumped (see [`on_event`]) so a device
/// log settles the layout. A wrong `keycode` offset reads garbage; it cannot fault, unlike a wrong
/// pointer. `Ecore_Window` is `uintptr_t` upstream, hence `usize`, so the same struct is right for
/// 32- and 64-bit.
#[repr(C)]
struct EcoreEventKey {
    keyname: *const c_char,
    key: *const c_char,
    string: *const c_char,
    compose: *const c_char,
    window: usize,
    root_window: usize,
    event_window: usize,
    timestamp: c_uint,
    modifiers: c_uint,
    same_screen: c_int,
    keycode: c_uint,
}

/// Bytes of the raw event dumped for the first few events. 48 is the size of the 32-bit upstream
/// struct up to and including `dev`: no read can run past a real allocation.
const DUMP_BYTES: usize = 48;
const DUMP_EVENTS: usize = 6;
/// Drop-oldest bound on the queue, only reachable if the render thread has died.
const QUEUE_BOUND: usize = 1024;

pub struct KeyRec {
    pub down: bool,
}

static QUEUE: Mutex<VecDeque<KeyRec>> = Mutex::new(VecDeque::new());
static SEEN: AtomicUsize = AtomicUsize::new(0);

/// Pop every queued key (render thread).
pub fn drain() -> Vec<KeyRec> {
    let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    q.drain(..).collect()
}

/// The longest string read through an event's pointer (key names are a few characters).
const MAX_STR: usize = 64;

/// A `const char *` field of the event, read without trusting it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PtrText {
    Null,
    /// The pointer is not null but its page cannot be read: the struct layout is not what this crate
    /// assumed. Reading it would have been a SIGSEGV on the first key press.
    Unreadable(usize),
    /// The address and the (lossily decoded, at most [`MAX_STR`] bytes of) text.
    Text(usize, String),
}

impl PtrText {
    /// How the log shows the field.
    fn shown(&self) -> String {
        match self {
            PtrText::Null => "(null)".to_string(),
            PtrText::Unreadable(a) => format!("UNREADABLE@0x{a:x}"),
            PtrText::Text(_, s) => format!("{s:?}"),
        }
    }

    fn addr(&self) -> usize {
        match self {
            PtrText::Null => 0,
            PtrText::Unreadable(a) | PtrText::Text(a, _) => *a,
        }
    }

    fn text(&self) -> Option<&str> {
        match self {
            PtrText::Text(_, s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }
}

fn page_size() -> usize {
    // SAFETY: sysconf has no preconditions.
    let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if p > 0 {
        p as usize
    } else {
        4096
    }
}

/// The pipe [`byte_is_readable`] probes through. `None` when it could not be made: nothing is then
/// trusted (a pointer field reads as unreadable).
fn probe_pipe() -> Option<&'static Mutex<(c_int, c_int)>> {
    static PIPE: OnceLock<Option<Mutex<(c_int, c_int)>>> = OnceLock::new();
    PIPE.get_or_init(|| {
        let mut fds = [0 as c_int; 2];
        // SAFETY: `fds` has room for the two descriptors pipe() writes.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return None;
        }
        for fd in fds {
            // SAFETY: descriptors just created. Close-on-exec so a child process never inherits them;
            // the write end is non-blocking so a probe can never wait.
            unsafe {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        // SAFETY: as above.
        unsafe {
            let fl = libc::fcntl(fds[1], libc::F_GETFL);
            libc::fcntl(fds[1], libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
        Some(Mutex::new((fds[0], fds[1])))
    })
    .as_ref()
}

/// Whether the byte at `addr` can be read, without faulting if it cannot: the kernel is asked to copy
/// that one byte into a pipe, and a bad (unmapped or unreadable) address makes `write` fail with EFAULT
/// instead of raising SIGSEGV in this process. The byte is read back out so the pipe stays empty.
fn byte_is_readable(addr: usize) -> bool {
    let Some(pipe) = probe_pipe() else { return false };
    let (r, w) = *pipe.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: write only READS `addr` (inside the kernel, which checks it); `r` and `w` are our own
    // pipe's ends, and the one byte is drained into a local right away.
    unsafe {
        let n = loop {
            let n = libc::write(w, addr as *const c_void, 1);
            if n >= 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break n;
            }
        };
        if n != 1 {
            return false;
        }
        let mut sink = 0u8;
        libc::read(r, &mut sink as *mut u8 as *mut c_void, 1);
    }
    true
}

/// Read a NUL-terminated string at `p` for at most [`MAX_STR`] bytes, checking that every page it
/// touches can be read before reading it. The event layout is the upstream EFL header's and UNVERIFIED
/// on a Samsung device, so `keyname`, `key`, `string` and `compose` are not trusted to be pointers.
///
/// # Safety
/// `p` is null, or an address whose readable bytes are safe to read as bytes (no side effects): true of
/// any address inside a mapping the process owns.
pub(crate) unsafe fn read_cstr(p: *const c_char) -> PtrText {
    if p.is_null() {
        return PtrText::Null;
    }
    let base = p as usize;
    let page = page_size();
    let mut bytes = Vec::new();
    let mut checked_page = usize::MAX;
    for i in 0..MAX_STR {
        let Some(addr) = base.checked_add(i) else { return PtrText::Unreadable(base) };
        let pg = addr / page;
        // The first byte wanted on each page is probed; the rest of that page is then readable too.
        if pg != checked_page {
            if !byte_is_readable(addr) {
                return PtrText::Unreadable(base);
            }
            checked_page = pg;
        }
        // SAFETY: the page holding `addr` was just seen readable.
        let b = unsafe { *(addr as *const u8) };
        if b == 0 {
            return PtrText::Text(base, String::from_utf8_lossy(&bytes).into_owned());
        }
        bytes.push(b);
    }
    let mut s = String::from_utf8_lossy(&bytes).into_owned();
    s.push_str("...");
    PtrText::Text(base, s)
}

fn on_event(down: bool, event: *mut c_void) {
    if event.is_null() {
        plog!("key event with null payload");
        return;
    }
    let n = SEEN.fetch_add(1, Ordering::Relaxed);
    if n < DUMP_EVENTS {
        // SAFETY: DUMP_BYTES is no larger than the smallest real Ecore_Event_Key.
        let raw = unsafe { std::slice::from_raw_parts(event as *const u8, DUMP_BYTES) };
        let hex: Vec<String> = raw.iter().map(|b| format!("{b:02x}")).collect();
        plog!("key raw event #{n} first {DUMP_BYTES} bytes: {}", hex.join(" "));
    }
    // SAFETY: see EcoreEventKey; the integers are plain reads, and the four string pointers are read
    // only through `read_cstr`, which checks each page is mapped.
    let (ev, keyname, key, string, compose) = unsafe {
        let ev = &*(event as *const EcoreEventKey);
        (ev, read_cstr(ev.keyname), read_cstr(ev.key), read_cstr(ev.string), read_cstr(ev.compose))
    };
    plog!(
        "key {} keyname={} key={} string={} compose={} keycode={} modifiers={} (ptrs keyname=0x{:x} key=0x{:x} string=0x{:x} compose=0x{:x})",
        if down { "down" } else { "up  " },
        keyname.shown(),
        key.shown(),
        string.shown(),
        compose.shown(),
        ev.keycode,
        ev.modifiers,
        keyname.addr(),
        key.addr(),
        string.addr(),
        compose.addr()
    );
    // The panel shows the Ecore keyname (the key string when there is none) and the keycode. Any key
    // is just an event: nothing here, or anywhere in the library, treats Back (or any key) as quit.
    crate::panel::key_event(down, keyname.text().or_else(|| key.text()).unwrap_or(""), ev.keycode);
    let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    if q.len() >= QUEUE_BOUND {
        q.pop_front();
    }
    q.push_back(KeyRec { down });
}

/// `ecore_event_handler_add` callback for `ECORE_EVENT_KEY_DOWN` (`data` = 1) and
/// `ECORE_EVENT_KEY_UP` (`data` = 0). Returns `ECORE_CALLBACK_PASS_ON` (1) so the system's own key
/// handling (Back/Exit) still runs. Never unwinds into Ecore.
pub extern "C" fn key_cb(data: *mut c_void, _type: c_int, event: *mut c_void) -> u8 {
    let down = !data.is_null();
    if catch_unwind(AssertUnwindSafe(|| on_event(down, event))).is_err() {
        plog!("PANIC in key callback (swallowed)");
    }
    1
}

/// Focus in/out: not keys, but whether the window has focus decides whether keys arrive at all.
/// `data` = 1 for focus-in, 0 for focus-out. The event body is not read.
pub extern "C" fn focus_cb(data: *mut c_void, _type: c_int, _event: *mut c_void) -> u8 {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let what = if data.is_null() { "focus OUT" } else { "focus IN" };
        plog!("window {what}");
        crate::panel::lifecycle(what);
    }));
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    /// SAFETY of the calls below: every pointer is null, a live CString, or an address that no mapping
    /// covers (which `read_cstr` must refuse to read).
    #[test]
    fn a_valid_string_is_read_and_a_null_one_is_null() {
        let s = CString::new("XF86AudioPlay").unwrap();
        match unsafe { read_cstr(s.as_ptr()) } {
            PtrText::Text(a, t) => assert_eq!((a, t.as_str()), (s.as_ptr() as usize, "XF86AudioPlay")),
            other => panic!("{other:?}"),
        }
        assert_eq!(unsafe { read_cstr(std::ptr::null()) }, PtrText::Null);
        let empty = CString::new("").unwrap();
        assert_eq!(unsafe { read_cstr(empty.as_ptr()) }.text(), None, "an empty string names no key");
    }

    #[test]
    fn a_pointer_into_no_mapping_is_refused_not_dereferenced() {
        // Page 0 is never mapped, so these would be SIGSEGVs if read.
        for bad in [0x10usize, 0x400, 0xfff] {
            assert_eq!(unsafe { read_cstr(bad as *const c_char) }, PtrText::Unreadable(bad), "0x{bad:x}");
        }
        assert_eq!(PtrText::Unreadable(0x10).shown(), "UNREADABLE@0x10");
    }

    #[test]
    fn a_string_that_never_ends_is_cut_at_the_bound() {
        let long = CString::new("k".repeat(500)).unwrap();
        match unsafe { read_cstr(long.as_ptr()) } {
            PtrText::Text(_, t) => assert_eq!(t.len(), MAX_STR + 3, "{t}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_string_that_ends_on_the_last_mapped_byte_of_a_page_is_read() {
        // Map two pages and make the second PROT_NONE (still mapped, so no other thread can take the
        // hole, but unreadable). "Up\0" ends on the last byte of the first page: reading stops at the NUL
        // and never touches the second page. Without the NUL the string runs onto it and is refused.
        let page = page_size();
        // SAFETY: anonymous private mapping, owned here until munmap below.
        let base = unsafe {
            libc::mmap(std::ptr::null_mut(), 2 * page, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANON, -1, 0)
        };
        assert_ne!(base, libc::MAP_FAILED);
        let base = base as *mut u8;
        unsafe {
            assert_eq!(libc::mprotect(base.add(page) as *mut c_void, page, libc::PROT_NONE), 0);
            let at = base.add(page - 3);
            at.write(b'U');
            at.add(1).write(b'p');
            at.add(2).write(0);
            assert_eq!(read_cstr(at as *const c_char), PtrText::Text(at as usize, "Up".to_string()));
            at.add(2).write(b'x');
            assert_eq!(read_cstr(at as *const c_char), PtrText::Unreadable(at as usize), "a string running onto an unreadable page");
            libc::munmap(base as *mut c_void, 2 * page);
        }
    }

    #[test]
    fn a_whole_event_with_real_pointers_reads_back_and_reaches_the_panel_log_format() {
        let (kn, k, s) = (CString::new("Return").unwrap(), CString::new("Return").unwrap(), CString::new("\r").unwrap());
        let ev = EcoreEventKey {
            keyname: kn.as_ptr(),
            key: k.as_ptr(),
            string: s.as_ptr(),
            compose: std::ptr::null(),
            window: 0,
            root_window: 0,
            event_window: 0,
            timestamp: 1,
            modifiers: 0,
            same_screen: 0,
            keycode: 36,
        };
        let (a, b, c, d) = unsafe { (read_cstr(ev.keyname), read_cstr(ev.key), read_cstr(ev.string), read_cstr(ev.compose)) };
        assert_eq!((a.shown().as_str(), b.shown().as_str(), c.shown().as_str(), d.shown().as_str()), ("\"Return\"", "\"Return\"", "\"\\r\"", "(null)"));
        assert_eq!(a.text(), Some("Return"));
        assert_eq!(a.addr(), kn.as_ptr() as usize);
    }
}
