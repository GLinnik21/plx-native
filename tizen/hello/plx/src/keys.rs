//! Remote-control keys: the Ecore event callbacks (main thread) and the queue the render thread
//! drains.
//!
//! Ecore is not thread-safe and its main loop runs on the host's main thread, so the handlers are
//! registered there (inside `plx_tizen_create`) and do almost nothing: log, copy the strings out of
//! the event, push onto [`QUEUE`]. The render thread pops and advances the clear colour.

use crate::log::plog;
use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

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

fn text(p: *const c_char) -> String {
    if p.is_null() {
        "(null)".to_string()
    } else {
        // SAFETY: the event owns a NUL-terminated string for the duration of the callback and the
        // text is copied before returning. (Layout caveat: see EcoreEventKey.)
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
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
    // SAFETY: see EcoreEventKey; only keyname/key (pointers) and keycode (integer) are read.
    let ev = unsafe { &*(event as *const EcoreEventKey) };
    plog!(
        "key {} keyname={:?} key={:?} keycode={}",
        if down { "down" } else { "up  " },
        text(ev.keyname),
        text(ev.key),
        ev.keycode
    );
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
        plog!("window focus {}", if data.is_null() { "OUT" } else { "IN" });
    }));
    1
}
