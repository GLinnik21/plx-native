//! player — the buffer-feed video engine (was src/playback.c). THREADING: everything
//! here except sf_on_event/acb_on_event runs on the SDL main thread. Those two are
//! #[no_mangle] and run on the StarfishMediaAPIs library thread; they touch ONLY
//! `SHARED`. All other cross-thread state is in shared.rs (atomics + Mutex); the
//! Engine (engine.rs) is main-thread-confined. Design: docs/engine-port-design.md.
#![allow(non_upper_case_globals)]
mod engine;
mod ffi;
mod pump;
mod shared;
mod threads;

use shared::{Shared, Transport};
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_long};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::Relaxed};

pub(crate) static SHARED: Shared = Shared::new();
pub(crate) static TX: Transport = Transport::new();
static ACB_OK: AtomicBool = AtomicBool::new(false); // was the g_acb availability flag
static PTYPE: AtomicI32 = AtomicI32::new(10); // g_ptype (PLAYER_TYPE_MSE)

// ---- API app.rs calls (were extern "C" fns in playback.h) ----
pub(crate) use engine::{acb_init, start_bufferfeed, stop_bufferfeed};
pub(crate) use pump::pump;
pub(crate) fn pause() {
    unsafe { ffi::sf_pause(); }
} // playback_pause
pub(crate) fn resume() {
    unsafe { ffi::sf_play(); }
} // playback_resume

// ---- transport accessors app.rs / player_hud.rs call ----
pub(crate) fn is_started() -> bool { TX.started.load(Relaxed) }
pub(crate) fn playpos_ns() -> i64 { SHARED.playpos_ns.load(Relaxed) }
pub(crate) fn frames() -> i32 { SHARED.frames.load(Relaxed) }
pub(crate) fn duration_ns() -> i64 { SHARED.duration_ns.load(Relaxed) }
pub(crate) fn seek_pending() -> i64 { TX.seek_to_ns.load(Relaxed) }
pub(crate) fn request_seek(ns: i64) { TX.seek_to_ns.store(ns, Relaxed) }
/// request an audio-track switch (Plex audioStreamID); the pump forces a fresh
/// transcode with that source audio at the current position next tick.
pub(crate) fn request_audio_switch(sid: i64) { SHARED.pending_audio_sid.store(sid, Relaxed) }

pub(crate) fn log(m: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/poc-events.log") {
        let _ = writeln!(f, "{m}");
    }
}

fn find(h: &[u8], n: &[u8]) -> bool {
    !n.is_empty() && h.windows(n.len()).any(|w| w == n)
}
/// bytes between `prefix` and the next `term`, or None if `prefix` absent.
fn between(h: &[u8], prefix: &[u8], term: u8) -> Option<Vec<u8>> {
    let start = h.windows(prefix.len()).position(|w| w == prefix)? + prefix.len();
    let rest = &h[start..];
    let end = rest.iter().position(|&b| b == term).unwrap_or(rest.len());
    Some(rest[..end].to_vec())
}

/// pipeline event on the LIBRARY thread. type 0 = frame presented (num = fed pts).
/// Panic-guarded (unwinding into C is UB); touches only SHARED.
#[no_mangle]
pub extern "C" fn sf_on_event(ty: c_int, num: i64, s: *const c_char) {
    let _ = catch_unwind(AssertUnwindSafe(|| sf_on_event_inner(ty, num, s)));
}
fn sf_on_event_inner(ty: c_int, num: i64, s: *const c_char) {
    if ty != 0 {
        let preview = if s.is_null() { String::new() } else {
            unsafe { CStr::from_ptr(s) }.to_string_lossy().chars().take(1400).collect()
        };
        log(&format!("smp_cb type={ty} num={num} str={preview}"));
    }
    if ty == 0 {
        // a frame was PRESENTED — map fed pts -> real content position
        SHARED.frames.fetch_add(1, Relaxed);
        SHARED.playpos_ns
            .store(num - SHARED.pts_shift.load(Relaxed) + SHARED.disp_base.load(Relaxed), Relaxed);
    }
    if s.is_null() {
        return;
    }
    let b = unsafe { CStr::from_ptr(s) }.to_bytes();

    {
        let mut mid = SHARED.media_id.lock().unwrap();
        if mid.is_none() {
            if let Some(id) = between(b, b"\"context\":\"", b'"').or_else(|| between(b, b"\"mediaId\":\"", b'"')) {
                if let Ok(c) = std::ffi::CString::new(id.clone()) {
                    log(&format!("SMP context/mediaId={}", String::from_utf8_lossy(&id)));
                    *mid = Some(c);
                }
            }
        }
    }

    if !SHARED.load_completed.load(Relaxed) && (find(b, b"loadCompleted") || find(b, b"\"loaded\"")) {
        SHARED.load_completed.store(true, Relaxed);
        log("SMP loadCompleted");
    }

    {
        // capture the WHOLE sourceInfo envelope VERBATIM (byte-for-byte + NUL), never re-encoded
        let mut si = SHARED.source_info.lock().unwrap();
        if si.is_none() && find(b, b"\"video\":") && find(b, b"\"context\":") {
            let mut v = Vec::with_capacity(b.len() + 1);
            v.extend_from_slice(b);
            v.push(0);
            log(&format!("SMP sourceInfoRaw captured ({} bytes)", b.len()));
            *si = Some(v);
        }
    }
}

#[no_mangle]
pub extern "C" fn acb_on_event(ev: c_long, reply: *const c_char) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let r = if reply.is_null() { String::new() } else {
            unsafe { CStr::from_ptr(reply) }.to_string_lossy().into_owned()
        };
        log(&format!("acb_cb ev={ev} reply={r}"));
    }));
}
