# Engine port design (Rust-first step 5)

> Synthesized design for porting the buffer-feed video engine (src/playback.c)
> to Rust (`rust-modules/src/player/`). From the design-engine-port workflow.

I have everything I need. Here is the concrete design.

---

# Rust port of the buffer-feed video engine (`playback.c` → `player/`)

## 1. Verdict on module structure

Split into a **`player/` directory of five files**, not one `player.rs`. The engine has four genuinely distinct concerns (lifecycle, main-thread pump, worker threads, the C seam) plus a shared-state definition that all four import. A single file would put the `#[no_mangle]` library-thread callbacks next to the main-thread pump and invite exactly the confusion this port must eliminate. The split *is* the race-safety argument: each file's threading domain is stated at the top.

```
rust-modules/src/player/
├── mod.rs      pub(crate) API surface + the two #[no_mangle] callbacks + the
│               SHARED/TX statics.  This is the file app.rs / player_hud.rs see.
├── shared.rs   struct Shared, struct Transport, CueEnt, Stage — all interior-
│               mutability types (atomics + Mutex).  THE cross-thread surface.
├── engine.rs   struct Engine (main-thread-confined session object) +
│               acb_init / start_bufferfeed / stop_bufferfeed.
├── pump.rs     bufferfeed_pump: seek handler, ACB bind state machine, feed loops.
├── threads.rs  stream_thread / cues_thread / load_thread + the extern "C"
│               trampolines (read, cue) that mkv_ctx calls back into.
└── ffi.rs      extern "C" block for the sf_* / acb_* seam verbs.
```

`lib.rs`: `mod player;` replaces nothing else; the Makefile drops `playback.c`/`playback.o` from `OBJS` (only `starfish.c` + `main.c` stay C — a one-line build change, not covered further here).

## 2. The race-freedom argument (the whole point of step 5)

After the port, exactly four threads touch player state:

| Thread | What it runs | What it may touch |
|---|---|---|
| **M** main / `plex_run` | `acb_init`, `start`/`stop_bufferfeed`, `pump`, all transport writes | `Engine`, `SHARED`, `TX` |
| **D** demux | `stream_thread` | `SHARED` (atomics + `aq`), its own boxes |
| **C** cue-preflight | `cues_thread` | `SHARED.cues`/`cues_ready`/`cues_abort`, its own boxes |
| **L**/**K** load + library callback | `load_thread`, `sf_on_event`, `acb_on_event` | `SHARED` |

**Invariant 1 — `Engine` is main-thread-confined.** `stage`, the `aq` box, `pending`, `payload`, `max_fed_pts`, `rebase_pending`, `video_info_sent`, and the `JoinHandle`s are read/written *only* by M (start/stop/pump). D/C/L/K never name `Engine`. Race-free by confinement, so its fields are plain (no atomics), exactly like the C main-thread-only flags.

**Invariant 2 — `SHARED` is the only multi-thread object, and every field is an atomic or a `Mutex`.** This is the direct replacement for the C `volatile` globals. No field is a bare value.

**Invariant 3 — the one deliberate non-atomic interaction is `close(fd)`-to-interrupt.** M closes the demux/cue socket to unblock a blocked `recv` in D/C. That is the POSIX idiom the C already depends on. It is mediated by `AtomicPtr<HttpStream>` (atomic load/store of the pointer); the *only* raced datum is the single `fd` int inside the box, and the box lifetime is bounded (published at thread start, nulled+dropped at thread end; M closes only *before* `join`, while the box is provably alive). Documented, unavoidable without forking `stream.rs`.

Because the whole app is single-threaded except D/C/L/K, and those only ever reach `SHARED` (all-synchronized) plus the internally-pthread-locked `AuQueue`, the design is race-free by construction rather than by hope.

## 3. `shared.rs` — the cross-thread surface

```rust
use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicPtr, AtomicU32, Ordering};
use std::sync::Mutex;
use crate::stream::HttpStream;

#[derive(Clone, Copy)]
pub(crate) struct CueEnt { pub t_ns: i64, pub byte: i64 }   // was struct cue_ent

/// Every field replaces a `volatile` global from playback.c. One long-lived
/// `static SHARED` (NOT an Arc): it outlives every start/stop cycle and is *reset*,
/// never freed — so a late library callback after teardown writes to a live object,
/// exactly as the C static globals behaved. All fields const-constructible.
pub(crate) struct Shared {
    // library callback thread (K) -> main (M)
    pub playpos_ns:     AtomicI64,               // g_playpos_ns
    pub frames:         AtomicI32,               // bf_frames
    pub load_completed: AtomicBool,              // bf_loaded signal (K or pump sets)
    pub media_id:       Mutex<Option<CString>>,  // bf_mediaId (captured once)
    pub source_info:    Mutex<Option<Vec<u8>>>,  // sourceInfoRaw, VERBATIM incl NUL

    // main (M) -> library callback thread (K)
    pub pts_shift:      AtomicI64,               // g_pts_shift

    // main/pump (M) -> demux (D)
    pub seek_byte:      AtomicI64,               // g_seek_byte  (-1 = none)

    // demux (D) -> main (M)
    pub file_size:      AtomicI64,               // g_file_size
    pub duration_ns:    AtomicI64,               // was g_mkv.duration_ns (published)

    // cue preflight (C) <-> main (M)
    pub cues:       Mutex<Vec<CueEnt>>,          // g_cues (+ g_ncues = .len())
    pub cues_ready: AtomicBool,                  // g_cues_ready
    pub cues_abort: AtomicBool,                  // g_cues_abort

    // close-to-interrupt handles (Invariant 3)
    pub hs_ptr:  AtomicPtr<HttpStream>,          // was &g_hs
    pub hs2_ptr: AtomicPtr<HttpStream>,          // was &g_hs2
}

impl Shared {
    pub const fn new() -> Self {
        Shared {
            playpos_ns: AtomicI64::new(0), frames: AtomicI32::new(0),
            load_completed: AtomicBool::new(false),
            media_id: Mutex::new(None), source_info: Mutex::new(None),
            pts_shift: AtomicI64::new(0), seek_byte: AtomicI64::new(-1),
            file_size: AtomicI64::new(0), duration_ns: AtomicI64::new(0),
            cues: Mutex::new(Vec::new()),
            cues_ready: AtomicBool::new(false), cues_abort: AtomicBool::new(false),
            hs_ptr: AtomicPtr::new(std::ptr::null_mut()),
            hs2_ptr: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
    /// reset the per-file state on stop (mirrors the tail of stop_bufferfeed);
    /// does NOT touch the cue table (that has its own keep_cues rule).
    pub fn reset_session(&self) {
        self.playpos_ns.store(0, Ordering::Relaxed);
        self.frames.store(0, Ordering::Relaxed);
        self.load_completed.store(false, Ordering::Relaxed);
        *self.media_id.lock().unwrap() = None;
        *self.source_info.lock().unwrap() = None;
        self.pts_shift.store(0, Ordering::Relaxed);
        self.seek_byte.store(-1, Ordering::Relaxed);
        self.file_size.store(0, Ordering::Relaxed);
        self.duration_ns.store(0, Ordering::Relaxed);
    }
}

/// UI-facing transport state. Main-thread-only in practice (plex_run + pump +
/// player_hud all run on M), but exposed as atomics so app.rs / player_hud.rs
/// read/write it with plain .load()/.store() instead of the old extern static-mut
/// + addr_of dance. Replaces the #[no_mangle] transport globals from playback.h.
pub(crate) struct Transport {
    pub started:     AtomicBool,  // bf_started
    pub paused:      AtomicBool,  // pl_paused
    pub resume_pend: AtomicBool,  // resumePausePending
    pub hud_until:   AtomicU32,   // pl_hud_until (SDL ticks)
    pub scrub_ns:    AtomicI64,   // pl_scrub_ns (-1 = not scrubbing)
    pub seek_to_ns:  AtomicI64,   // g_seek_to_ns (UI seek request, -1 = none)
}
impl Transport {
    pub const fn new() -> Self {
        Transport { started: AtomicBool::new(false), paused: AtomicBool::new(false),
            resume_pend: AtomicBool::new(false), hud_until: AtomicU32::new(0),
            scrub_ns: AtomicI64::new(-1), seek_to_ns: AtomicI64::new(-1) }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Stage { Idle = 0, Loading, Playing, Bound, Streaming }
```

`pl_dur_ns` is **deleted** — there is one duration source of truth, `SHARED.duration_ns`, read through `player::duration_ns()`. The pump line `if bf_stream && g_mkv.duration_ns>0 pl_dur_ns=…` goes away; duration is published by the demux read trampoline (§6).

Ordering policy: `Relaxed` for the display/counter atomics (matches the C `volatile` semantics — a stale read is at worst one frame of position jitter). `seek_byte` uses **Release on the store / Acquire on the demux load** so the byte offset is visible after the socket close unblocks D. `media_id`/`source_info` visibility rides their `Mutex`, which also orders them ahead of the `frames>=2` gate.

## 4. `mod.rs` — the statics, the API, the two callbacks

```rust
//! player — the buffer-feed video engine (was src/playback.c). THREADING:
//! everything here except sf_on_event/acb_on_event runs on the SDL main thread.
//! Those two are #[no_mangle] and run on the StarfishMediaAPIs library thread;
//! they touch ONLY `SHARED`.
mod shared; mod engine; mod pump; mod threads; mod ffi;

use shared::{Shared, Transport};
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_long};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::Relaxed};

pub(crate) static SHARED: Shared = Shared::new();
pub(crate) static TX: Transport = Transport::new();
static ACB_OK: AtomicBool = AtomicBool::new(false);  // was engine-local g_acb flag
static PTYPE:  AtomicI32  = AtomicI32::new(10);      // g_ptype (PLAYER_TYPE_MSE)

// ---- API app.rs calls (were extern "C" fns in playback.h) ----
pub(crate) use engine::{acb_init, start_bufferfeed, stop_bufferfeed};
pub(crate) use pump::pump;
pub(crate) fn pause()  { unsafe { ffi::sf_pause(); } }   // playback_pause
pub(crate) fn resume() { unsafe { ffi::sf_play(); } }    // playback_resume

// ---- transport accessors app.rs / player_hud.rs call ----
pub(crate) fn is_started()  -> bool { TX.started.load(Relaxed) }
pub(crate) fn playpos_ns()  -> i64  { SHARED.playpos_ns.load(Relaxed) }
pub(crate) fn frames()      -> i32  { SHARED.frames.load(Relaxed) }
pub(crate) fn duration_ns() -> i64  { SHARED.duration_ns.load(Relaxed) }
pub(crate) fn seek_pending() -> i64 { TX.seek_to_ns.load(Relaxed) }
pub(crate) fn request_seek(ns: i64) { TX.seek_to_ns.store(ns, Relaxed) }
```

The two library-thread edges. Panic-guarded (unwinding into C is UB), and they touch only `SHARED`:

```rust
/// pipeline event on the LIBRARY thread. type 0 = frame presented (num = fed pts).
#[no_mangle]
pub extern "C" fn sf_on_event(ty: c_int, num: i64, s: *const c_char) {
    let _ = catch_unwind(AssertUnwindSafe(|| sf_on_event_inner(ty, num, s)));
}
fn sf_on_event_inner(ty: c_int, num: i64, s: *const c_char) {
    if ty != 0 { crate::log(&format!("smp_cb type={ty} num={num} ...")); }
    if ty == 0 {                                   // a frame was presented
        SHARED.frames.fetch_add(1, Relaxed);
        SHARED.playpos_ns.store(num - SHARED.pts_shift.load(Relaxed), Relaxed);
    }
    if s.is_null() { return; }
    let b = unsafe { CStr::from_ptr(s) }.to_bytes();

    { let mut mid = SHARED.media_id.lock().unwrap();          // capture mediaId once
      if mid.is_none() {
          if let Some(id) = between(b, b"\"context\":\"", b'"')
                              .or_else(|| between(b, b"\"mediaId\":\"", b'"')) {
              *mid = std::ffi::CString::new(id).ok();
      } } }

    if !SHARED.load_completed.load(Relaxed)                   // loadCompleted latch
       && (find(b, b"loadCompleted") || find(b, b"\"loaded\"")) {
        SHARED.load_completed.store(true, Relaxed);
    }

    { let mut si = SHARED.source_info.lock().unwrap();         // VERBATIM sourceInfo
      if si.is_none() && find(b, b"\"video\":") && find(b, b"\"context\":") {
          let mut v = Vec::with_capacity(b.len() + 1);
          v.extend_from_slice(b); v.push(0);   // byte-for-byte + NUL, never re-encoded
          *si = Some(v);
      } }
}

#[no_mangle]
pub extern "C" fn acb_on_event(ev: c_long, reply: *const c_char) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let r = if reply.is_null() { String::new() }
                else { unsafe { CStr::from_ptr(reply) }.to_string_lossy().into_owned() };
        crate::log(&format!("acb_cb ev={ev} reply={r}"));
    }));
}
```

The **VERBATIM rule is mechanically enforced**: `source_info` stores the exact bytes `CStr::from_ptr` yields (identical to the C `strlen`+`memcpy` into `sourceInfoRaw`), and §5's `acb_send_video_data(v.as_ptr())` hands those same bytes to ACB — no parse, no rebuild.

## 5. `pump.rs` — the crux (seek + bind order + backpressure)

```rust
use super::{ffi, shared::Stage, ACB_OK, SHARED, TX};
use super::engine::{engine, drain_aq, feed_stream, feed_sample, Source};
use std::os::raw::c_char;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

pub(crate) fn pump(now: u32) {
    let _ = now;
    let eng = match engine() { Some(e) => e, None => return };
    if eng.stage == Stage::Idle || unsafe { ffi::sf_ready() } == 0 { return; }
    let stream = matches!(eng.source, Source::Stream { .. });

    // ---------- pending seek ----------
    let t = TX.seek_to_ns.load(Relaxed);
    if stream && t >= 0 && eng.stage >= Stage::Playing
        && SHARED.file_size.load(Relaxed) > 0 && SHARED.duration_ns.load(Relaxed) > 0
    {
        TX.seek_to_ns.store(-1, Relaxed);
        let t = t.max(0);
        unsafe { ffi::sf_flush(); ffi::sf_set_playtime(0); ffi::sf_play(); }
        drain_aq(eng);                        // free queued + pending AUs
        let dur = SHARED.duration_ns.load(Relaxed);
        let fsz = SHARED.file_size.load(Relaxed);
        let byte = super::engine::cue_byte_for(t)
            .unwrap_or_else(|| (t as f64 / dur as f64 * fsz as f64) as i64).max(0);
        SHARED.seek_byte.store(byte, Release);            // publish BEFORE the close
        let p = SHARED.hs_ptr.load(Acquire);
        if !p.is_null() { unsafe { crate::stream::http_close(p); } }  // unblock D
        eng.rebase_pending = true;
        eng.max_fed_pts = 0;
        SHARED.frames.store(0, Relaxed);       // count only post-seek frames
        SHARED.playpos_ns.store(t, Relaxed);
    }

    // ---------- load -> Play ----------
    if eng.stage == Stage::Loading
        && (SHARED.load_completed.load(Relaxed) || unsafe { ffi::sf_is_load_completed() } != 0)
    {
        unsafe { ffi::sf_play(); }
        eng.stage = Stage::Playing;
    }

    // ---------- ACB bind (order matters; mirrors Kodi/ss4s) ----------
    if eng.stage == Stage::Playing && ACB_OK.load(Relaxed) {
        if let Some(id) = SHARED.media_id.lock().unwrap().clone() {
            unsafe { ffi::acb_bind(id.as_ptr()); }        // sinkType MAIN + mediaId + LOADED
            eng.stage = Stage::Bound;
        }
    }

    // ---------- setMediaVideoData (VERBATIM) + window + PLAYING ----------
    if eng.stage == Stage::Bound && !eng.video_info_sent && SHARED.frames.load(Relaxed) >= 2 {
        if let Some(bytes) = SHARED.source_info.lock().unwrap().clone() {
            let rv = unsafe { ffi::acb_send_video_data(bytes.as_ptr() as *const c_char) };
            if rv != -1 {                                  // -1 = client isJsonError reject
                eng.video_info_sent = true;
                unsafe { ffi::acb_start(0, 0, 1920, 1080); }
                eng.stage = Stage::Streaming;
            }
        }
    }

    // ---------- feed (from Playing on; NOT while a seek is armed) ----------
    if eng.stage >= Stage::Playing && !TX.paused.load(Relaxed) && TX.seek_to_ns.load(Relaxed) < 0 {
        match eng.source {
            Source::Stream { .. } => feed_stream(eng),
            Source::Sample(_)     => feed_sample(eng),
        }
    }
}
```

The `Stage` enum linearizes the four C latch-flags. `stage >= Playing` reproduces the C `bf_playing` feed gate while bind/videodata advance the stage in parallel (they need `media_id` / `frames>=2`, which only arrive after feeding starts, so the natural order `Playing → Bound → Streaming` holds). The exact bind sequence — `acb_bind` (sinkType MAIN → setMediaId → setState LOADED) then, after `frames>=2`, `acb_send_video_data(verbatim)` then `acb_start` (setDisplayWindow → setState PLAYING) — is unchanged; those stay in the C seam.

`feed_stream` reproduces the rebase + backpressure precisely (lives in `engine.rs` next to the `Engine` fields it mutates):

```rust
pub(crate) fn feed_stream(eng: &mut Engine) {
    let mut fed = 0;
    while fed < 120 {
        if eng.pending.is_none() {
            let mut eof = 0;
            let n = crate::aq::aq_pop(&mut *eng.aq, &mut eof);
            if n.is_null() { break; }
            eng.pending = Some(AuBox(n));          // AuBox frees on drop
        }
        let n = eng.pending.as_ref().unwrap().0;
        let (es, key, pts, len, data) = unsafe { crate::aq::au_fields(n) };

        if eng.rebase_pending {                     // zero-base on first post-seek keyframe
            if es == 1 && key != 0 {
                SHARED.pts_shift.store(-pts, Relaxed);   // read by the K callback
                eng.rebase_pending = false;
            } else { eng.pending = None; continue; } // drop pre-keyframe AUs
        }
        let mut fp = pts + SHARED.pts_shift.load(Relaxed);
        if fp < eng.max_fed_pts - 2_000_000_000 { eng.pending = None; continue; } // stale
        if fp < 0 { fp = 0; }
        let r = unsafe { ffi::sf_feed(data, len as u32, fp, es) };
        if fp > eng.max_fed_pts { eng.max_fed_pts = fp; }
        if r != b'O' as i8 { break; }               // 'B' BufferFull -> keep pending, retry
        eng.pending = None;                          // accepted -> free + advance
        fed += 1;
    }
}
```

## 6. Wiring `mkv_ctx`'s callbacks + boxing the 64 KB `http_stream` — `threads.rs`

`mkv_ctx` carries `read: extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int` + `ud`, and `cue_cb: extern "C" fn(*mut c_void, i64, i64)` + `cue_ud`. The engine supplies **extern-C trampolines** whose `ud` points at a small per-thread context. Both `HttpStream` (64 KB) and `MkvCtx` are heap-`Box`ed inside the thread; the `HttpStream` raw pointer is published into `SHARED.hs_ptr` so M can close it.

```rust
use super::{ffi, shared::CueEnt, SHARED, TX};
use crate::mkv::{ebml_id, ebml_size, mkv_parse_cues, mkv_run, mkv_seek_run, MkvCtx};
use crate::stream::{http_close, http_open, http_read, HttpStream};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

/// ud for the demux read: carries the socket AND the mkv ctx so we can publish
/// duration_ns as soon as the demuxer parses Info. Read of (*mkv).duration_ns is
/// SAME-THREAD as its writer (mkv_parse_info on D), so intra-thread safe; the
/// atomic store is the cross-thread publish to M.
struct StreamRead { hs: *mut HttpStream, mkv: *mut MkvCtx }
extern "C" fn stream_read(ud: *mut c_void, dst: *mut u8, n: c_int) -> c_int {
    let rc = unsafe { &*(ud as *const StreamRead) };
    let r = http_read(rc.hs, dst, n);              // http_read is crate::stream (Rust)
    let d = unsafe { (*rc.mkv).duration_ns };
    if d > 0 { SHARED.duration_ns.store(d, Relaxed); }
    r
}
extern "C" fn hs2_read(ud: *mut c_void, dst: *mut u8, n: c_int) -> c_int {
    http_read(ud as *mut HttpStream, dst, n)       // cue preflight: plain reader
}

/// ud for the cue callback — carries the two values cue_cb needs (C read c->tscale
/// + global g_segment_pos). The cue Vec is reached via the SHARED global directly.
struct CueSink { tscale: i64, segment_pos: i64 }
extern "C" fn cue_cb(ud: *mut c_void, time_ticks: i64, byte: i64) {
    if SHARED.cues_abort.load(Acquire) { return; }  // teardown -> don't touch the Vec
    let s = unsafe { &*(ud as *const CueSink) };
    let ent = CueEnt { t_ns: time_ticks * s.tscale, byte: s.segment_pos + byte };
    if let Ok(mut v) = SHARED.cues.lock() { v.push(ent); }
}

/// raw ptr that we assert is Send for the spawn (aq/box lifetimes outlive the thread)
struct SendMut<T>(*mut T);
unsafe impl<T> Send for SendMut<T> {}

pub(crate) fn stream_thread(host: String, port: c_int, path: String, aq: SendMut<crate::aq::AuQueue>) {
    let host_c = std::ffi::CString::new(host).unwrap();
    let path_c = std::ffi::CString::new(path).unwrap();
    let mut hs: Box<HttpStream>  = Box::new(unsafe { std::mem::zeroed() });   // 64 KB off-stack
    let mut mkv: Box<MkvCtx>     = Box::new(unsafe { std::mem::zeroed() });
    let hs_p  = std::ptr::addr_of_mut!(*hs);
    let mkv_p = std::ptr::addr_of_mut!(*mkv);
    SHARED.hs_ptr.store(hs_p, Release);            // M may now close this socket

    let scratch = unsafe { libc::malloc(4 * 1024 * 1024) as *mut u8 };
    mkv.q = aq.0; mkv.scratch = scratch; mkv.scratch_cap = 4 * 1024 * 1024;
    let mut rc = StreamRead { hs: hs_p, mkv: mkv_p };   // lives on this stack for the loop
    mkv.read = Some(stream_read); mkv.ud = std::ptr::addr_of_mut!(rc) as *mut c_void;

    let (mut start, mut first) = (0i64, true);
    loop {
        let extra = (start > 0).then(|| std::ffi::CString::new(format!("Range: bytes={start}-\r\n")).unwrap());
        let ep = extra.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        if http_open(hs_p, host_c.as_ptr(), port, path_c.as_ptr(), ep) != 0 { break; }
        if first { SHARED.file_size.store(unsafe { (*hs_p).content_length }, Release); }
        mkv.eof = 0; mkv.pos = 0;
        if first { mkv_run(mkv_p); first = false; } else { mkv_seek_run(mkv_p); }
        http_close(hs_p);
        if unsafe { crate::aq::aq_is_aborted(aq.0) } { break; }
        let sb = SHARED.seek_byte.swap(-1, Acquire);
        if sb >= 0 { start = sb; continue; }
        break;
    }
    crate::aq::aq_set_eof(aq.0);
    SHARED.hs_ptr.store(std::ptr::null_mut(), Release);   // M must not close after this
    unsafe { libc::free(scratch as *mut c_void); }
    // hs / mkv Boxes drop here (fd already closed)
}
```

`cues_thread` follows the same shape: box `hs2` + `cm`, publish `hs2_ptr`; pass 1 `mkv_run` (header_only) to get `segment_pos`/`cues_pos`/`tscale` (+ publish `duration_ns` opportunistically — it beats the demux for transcodes too); check `cues_abort`; pass 2 reopen with `Range: bytes={segment_pos+cues_pos}-`, install `cue_cb`+`CueSink`, `ebml_id`/`ebml_size`/`mkv_parse_cues`; then `if !cues_abort { cues_ready.store(true, Release); }`; null `hs2_ptr` and drop.

`load_thread(payload_ptr: usize)` is trivial: `unsafe { ffi::sf_load(payload_ptr as *const c_char); }`. The `CString` it points at lives in `Engine.payload` (stable heap buffer; survives the `Engine` move into the slot, and `stop` joins `load_th` before `Engine` drops).

Note `http_read`, `aq_pop`, `aq_push`, `mkv_run`, `ebml_id/size`, etc. are all called as ordinary `crate::` Rust fns even though they carry `#[no_mangle] extern "C"` — the trampolines only exist because `mkv_ctx` stores C function pointers.

## 7. `engine.rs` — session object + lifecycle

```rust
pub(crate) enum Source {
    Stream { host: String, port: c_int, path: String },
    Sample(Box<SampleBuf>),                        // /tmp/sample.h264 validation path
}
pub(crate) struct AuBox(pub *mut crate::aq::AuNode);
impl Drop for AuBox { fn drop(&mut self) { unsafe { libc::free(self.0 as *mut _) } } }

/// MAIN-THREAD-CONFINED. No worker thread ever names an Engine field.
pub(crate) struct Engine {
    pub stage: Stage,
    pub video_info_sent: bool,        // videoInfoSent
    pub rebase_pending: bool,         // g_rebase_pending
    pub max_fed_pts: i64,             // g_max_fed_pts
    pub aq: Box<crate::aq::AuQueue>,  // g_aq (M owns; ptr handed to D)
    pub pending: Option<AuBox>,       // bf_pending (held across BufferFull)
    pub payload: std::ffi::CString,   // bf_payload (kept alive for the session)
    pub source: Source,
    pub stream_th: Option<std::thread::JoinHandle<()>>,
    pub cues_th:   Option<std::thread::JoinHandle<()>>,
    pub load_th:   Option<std::thread::JoinHandle<()>>,
}

static mut ENGINE: Option<Engine> = None;         // main-thread-only slot
#[inline] pub(crate) fn engine() -> Option<&'static mut Engine> {
    unsafe { (*std::ptr::addr_of_mut!(ENGINE)).as_mut() }
}
```

`acb_init` (reads `/tmp/plxnative-ptype`, `getenv("APPID")`, `acb_create`, sets `ACB_OK`/`PTYPE`), `start_bufferfeed` (resolve URL from `route::url()` → `/tmp/plxnative-url` → sample → `route::demo_url()`; `aq_init`; spawn D via `SendMut(aq_ptr)`; spawn C unless `route::transcode_session()` is non-empty; spawn L with `payload.as_ptr() as usize`; `stage=Loading`; `TX.started=true`), and `stop_bufferfeed(keep_cues)` reproduce the C teardown **order exactly**:

```
cues_abort=true
  → if stream { aq_abort; http_close(hs_ptr); http_close(hs2_ptr) }
  → join cues_th, stream_th, load_th          // JOIN before any cue-Vec touch (UAF guard)
  → if sf_ready { sf_unload; if ACB_OK { acb_unload }; sf_destroy }
  → if stream { drain_aq(eng); aq_destroy }
  → SHARED.reset_session()
  → route::stop_transcode()                    // was the inline /transcode/.../stop GET
  → TX reset (paused/resume_pend/scrub/seek/started)  + route::clear_url()
  → if !keep_cues || !cues_ready { SHARED.cues.lock().clear(); cues_ready=false }
  → Engine drops (aq box, payload, pending)
```

The cue **join-before-free** invariant is preserved and now stronger: the `Vec` lives in the global `SHARED.cues` `Mutex`, so even a stray post-join push would be memory-safe, but we still `cues_abort`+`join` first exactly as C did.

## 8. The C-ABI boundary after the port

| Symbol | Before | After |
|---|---|---|
| `sf_on_event`, `acb_on_event` | C fns in playback.c | **`#[no_mangle] pub extern "C"` in `player/mod.rs`** — the C seam still calls them by name |
| `sf_*` / `acb_*` verbs | defined in starfish.c, called from C | **unchanged C**; the engine declares them in `player/ffi.rs extern "C"` and calls them |
| `acb_init`, `start_bufferfeed`, `stop_bufferfeed`, `bufferfeed_pump`, `playback_pause/resume` | `extern "C"` in playback.h, `extern` block in app.rs | **plain `crate::player::…` Rust fns**; app.rs's whole `extern "C"` playback block (lines 68–84) is deleted |
| `bf_started, pl_paused, resumePausePending, pl_hud_until, pl_scrub_ns, g_seek_to_ns, g_playpos_ns, bf_frames`, `pl_dur_ns` | `#[no_mangle]` C globals, `static mut` externs in app.rs & player_hud.rs | **`player::TX.*` atomics + `player::{playpos_ns,frames,duration_ns,seek_pending,request_seek,is_started}()`**; the `#[no_mangle]` disappears (no C reader remains). `pl_dur_ns` is folded into `SHARED.duration_ns` |
| `g_url, g_transcode_session` (route) | `#[no_mangle] pub static mut`, read by C playback | read by the Rust engine via `route::url()/set_url()/clear_url()/transcode_session()`; `#[no_mangle]` can drop (route cleanup) |

**app.rs diff (`plex_run`)**: delete the `extern "C"` block and the `v_playpos/v_frames/v_seek/set_v_seek/get/getu/geti64` shims. Replace call sites:
- `acb_init()` → `player::acb_init()`; `start_bufferfeed()!=0` → `player::start_bufferfeed()`; `stop_bufferfeed(1)` → `player::stop_bufferfeed(true)`; `bufferfeed_pump(now)` → `player::pump(now)`; `playback_pause()/resume()` → `player::pause()/resume()`.
- `pl_paused` toggle → `let p = player::TX.paused.load(Relaxed); player::TX.paused.store(!p, Relaxed);`
- `pl_hud_until = x` → `player::TX.hud_until.store(x, Relaxed)`; `pl_scrub_ns` → `player::TX.scrub_ns`; `resumePausePending` → `player::TX.resume_pend`.
- `v_playpos()` → `player::playpos_ns()`; `v_frames()` → `player::frames()`; `v_seek()`/`set_v_seek()` → `player::seek_pending()`/`player::request_seek()`; `geti64(addr_of!(pl_dur_ns))` → `player::duration_ns()`; `get(addr_of!(bf_started))!=0` → `player::is_started()`.

**player_hud.rs diff**: delete its `extern "C"` block (lines 16–21). `pl_scrub_ns` → `crate::player::TX.scrub_ns.load(Relaxed)`; `pl_dur_ns` → `crate::player::duration_ns()`; `pl_paused` → `crate::player::TX.paused.load(Relaxed)`; `g_playpos_ns` → `crate::player::playpos_ns()`. (`route::g_title`/`g_ctxline` reads are untouched.)

**route.rs additions** (small, keeps URL/session ownership where it already is): `pub(crate) fn url() -> String`, `set_url(&str)`, `clear_url()`, `transcode_session() -> String`, `demo_url() -> String`, and `stop_transcode()` (moves the `/video/:/transcode/universal/stop` GET + `g_transcode_session[0]=0` out of the C stop path, using the existing `CFG`). `demo_url` is fed in by extending `plex_run(host,port,token)` → `plex_run(host,port,token,demo_url)` and `route::set_config(host,port,token,demo_url)`; the boot shim passes `DEMO_STREAM_URL` (the only remaining use of that C macro).

**aq.rs additions** (accessors so `feed_stream` can read a popped node without re-exposing raw offsets):
```rust
pub(crate) unsafe fn au_fields(n: *mut AuNode) -> (c_int, c_int, i64, c_int, *const u8) {
    (( *n).es, (*n).key, (*n).pts, (*n).len, node_data(n))     // es, key, pts, len, data
}
```
plus `pub(crate)` on `AuNode`/`AuQueue` visibility already present, and `aq_is_aborted` is already `pub(crate)`.

## 9. Correctness properties preserved (the hard-won ones)

- **VERBATIM sourceInfo** — captured as raw bytes in `sf_on_event`, handed to `acb_send_video_data` unmodified (§4/§5). No JSON round-trip anywhere.
- **ACB bind order** — `acb_bind` → wait `frames>=2` → `acb_send_video_data` → `acb_start`, unchanged; all four still in the C seam, driven by the `Stage` machine.
- **Seek/rebase timing** — publish `seek_byte` (Release) *before* `http_close`; `rebase_pending` zero-bases `pts_shift` on the first post-seek keyframe in the feed loop; `frames` reset so the resume re-pause gate counts only post-seek frames — byte-identical to the C pump.
- **Backpressure** — `pending: Option<AuBox>` holds the un-accepted AU across `BufferFull` ticks; `AuBox::drop` frees exactly where C called `free(bf_pending)`; the `AQ_MAX_BYTES` producer block is unchanged in `aq.rs`.
- **Cue UAF guard** — `cues_abort` + join before the `Vec` is cleared; `keep_cues` keeps only a fully-loaded table.
- **Duration liveness** — republished by the demux read trampoline from the same thread that writes it, so the HUD sees duration the instant Info is parsed, without the C's unguarded 64-bit cross-thread read.

**Key files**: `/Users/gleblinnik/Developer/plex/plex-native-poc/src/playback.c` (source, to delete), `/Users/gleblinnik/Developer/plex/plex-native-poc/src/starfish.c`+`.h` (the seam, stays C), and the new `/Users/gleblinnik/Developer/plex/plex-native-poc/rust-modules/src/player/{mod,shared,engine,pump,threads,ffi}.rs`; touched: `rust-modules/src/{lib,app,route,aq}.rs` and `rust-modules/src/ui/player_hud.rs`.