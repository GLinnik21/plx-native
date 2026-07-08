//! player — the buffer-feed video engine (was src/playback.c). THREADING: everything
//! here except sf_on_event/acb_on_event runs on the SDL main thread. Those two are
//! #[no_mangle] and run on the StarfishMediaAPIs library thread; they touch ONLY
//! `SHARED`. All other cross-thread state is in shared.rs (atomics + Mutex); the
//! Engine (engine.rs) is main-thread-confined. Design: docs/engine-port-design.md.
#![allow(non_upper_case_globals)]
pub(crate) mod engine;
mod ffi;
mod pump;
mod shared;
pub(crate) mod threads;

use shared::{Shared, SubCue, Transport};
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_long};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::Relaxed};

pub(crate) static SHARED: Shared = Shared::new();
pub(crate) static TX: Transport = Transport::new();
static ACB_OK: AtomicBool = AtomicBool::new(false); // was the g_acb availability flag
static PTYPE: AtomicI32 = AtomicI32::new(10); // g_ptype (PLAYER_TYPE_MSE)

// ---- API app.rs calls (were extern "C" fns in playback.h) ----
pub(crate) use engine::{acb_init, arm_seek, start_bufferfeed, stop_bufferfeed};
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
pub(crate) fn request_audio_switch(sid: i64) {
    SHARED.pending_audio_sid.store(sid, Relaxed);
    SHARED.sub_cues.lock().unwrap().clear(); // the fresh transcode carries no embedded subs
}
/// request a re-transcode at the current position with the CURRENT audio + subtitle —
/// used when a subtitle is (de)selected while already transcoding, so the server
/// re-burns (or drops) it. No-op-ish if not transcoding (the caller gates on that).
pub(crate) fn request_transcode_refresh() {
    SHARED.pending_retranscode.store(true, Relaxed);
    SHARED.sub_cues.lock().unwrap().clear(); // burned/absent in the fresh transcode
}

// ---- client-rendered subtitles (direct-play only; a transcode carries no subs) ----
/// selected subtitle track index (-1 = off); the demuxer reads this per block.
pub(crate) fn desired_sub_idx() -> i32 { SHARED.desired_sub_idx.load(Relaxed) }
/// select a subtitle track by index (-1 = off) and drop stale cues.
pub(crate) fn request_subtitle(idx: i32) {
    SHARED.desired_sub_idx.store(idx, Relaxed);
    SHARED.sub_cues.lock().unwrap().clear();
}
/// desired soft-WebVTT subtitle stream id during a transcode (0 = off). The pump
/// reconciles the subs thread (spawn / re-point / stop) from this.
pub(crate) fn request_soft_subs(sid: i64) {
    SHARED.subs_want_sid.store(sid, Relaxed);
}
/// push a ready (already-clean) subtitle cue into the shared store; keeps the last ~24
/// (ring buffer). Shared sink for the demux path and the WebVTT-sidecar path.
pub(crate) fn push_subtitle_text(start_ns: i64, end_ns: i64, text: String) {
    if text.is_empty() {
        return;
    }
    let mut cues = SHARED.sub_cues.lock().unwrap();
    if cues.len() >= 24 {
        cues.remove(0);
    }
    cues.push(SubCue { start_ns, end_ns, text });
}
/// demux (D-thread) pushes a subtitle cue (content-time ns). Keeps the last ~24 cues.
pub(crate) fn push_subtitle_cue(start_ns: i64, end_ns: i64, payload: &[u8], is_ass: bool) {
    let text = sub_text(payload, is_ass);
    if text.is_empty() {
        return;
    }
    log(&format!("sub cue [{}..{}ms] {:?}", start_ns / 1_000_000, end_ns / 1_000_000,
        text.chars().take(34).collect::<String>()));
    push_subtitle_text(start_ns, end_ns, text);
}
/// the subtitle text active at `now_ns`, or None (also None when subtitles are off).
pub(crate) fn active_subtitle(now_ns: i64) -> Option<String> {
    if SHARED.desired_sub_idx.load(Relaxed) < 0 {
        return None;
    }
    let cues = SHARED.sub_cues.lock().unwrap();
    cues.iter().rev().find(|c| now_ns >= c.start_ns && now_ns < c.end_ns).map(|c| c.text.clone())
}
/// extract displayable text from a subtitle block (SRT = raw UTF-8; ASS = the field
/// after the 8th comma), stripping tags/override codes and normalizing line breaks.
fn sub_text(payload: &[u8], is_ass: bool) -> String {
    let raw = String::from_utf8_lossy(payload);
    let s = if is_ass {
        raw.splitn(9, ',').nth(8).unwrap_or("").to_string()
    } else {
        raw.into_owned()
    };
    let mut out = String::with_capacity(s.len());
    let mut ch = s.chars().peekable();
    while let Some(c) = ch.next() {
        match c {
            '<' => while let Some(x) = ch.next() { if x == '>' { break; } },   // <i></i>
            '{' => while let Some(x) = ch.next() { if x == '}' { break; } },   // {\an8}
            '\\' => match ch.peek() {
                Some('N') | Some('n') => { ch.next(); out.push('\n'); }
                Some('h') => { ch.next(); out.push(' '); }
                _ => out.push('\\'),
            },
            '\r' => {}
            _ => out.push(c),
        }
    }
    out.trim().to_string()
}

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
        SHARED.pres_fed.store(num, Relaxed); // raw fed pts, for the feed-ahead throttle
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
