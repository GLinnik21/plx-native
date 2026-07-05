//! player::threads — the three worker threads (demux / cue-preflight / load) + the
//! extern "C" trampolines mkv_ctx / the seam call back into. Each thread boxes its
//! own MkvCtx (thread-local); the HttpStream boxes it uses live in the Engine (main
//! owns them, closes them to interrupt, and outlives the threads).
use super::shared::CueEnt;
use super::SHARED;
use crate::aq::AuQueue;
use crate::mkv::MkvCtx;
use crate::stream::HttpStream;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::sync::atomic::Ordering;

/// raw ptr we assert is Send for the spawn (the boxes/queue outlive the thread).
pub(crate) struct SendPtr<T>(pub *mut T);
unsafe impl<T> Send for SendPtr<T> {}

// ---- trampolines mkv_ctx / the seam call ----

/// demux read ud: carries the socket + the mkv ctx, so we can publish duration_ns
/// the instant the demuxer parses Info (same-thread read of the writer).
struct StreamRead {
    hs: *mut HttpStream,
    mkv: *mut MkvCtx,
}
extern "C" fn stream_read(ud: *mut c_void, dst: *mut u8, n: c_int) -> c_int {
    let rc = unsafe { &*(ud as *const StreamRead) };
    let r = crate::stream::http_read(rc.hs, dst, n);
    let d = unsafe { (*rc.mkv).duration_ns };
    if d > 0 {
        SHARED.duration_ns.store(d, Ordering::Relaxed);
    }
    r
}
extern "C" fn hs2_read(ud: *mut c_void, dst: *mut u8, n: c_int) -> c_int {
    crate::stream::http_read(ud as *mut HttpStream, dst, n)
}

/// cue callback ud: the tscale + segment_pos snapshot needed to build absolute
/// (time, byte) cue points. The Vec lives in SHARED.cues.
struct CueSink {
    tscale: i64,
    segment_pos: i64,
}
extern "C" fn cue_cb(ud: *mut c_void, time_ticks: i64, byte: i64) {
    if SHARED.cues_abort.load(Ordering::Acquire) {
        return; // teardown in progress — don't touch the Vec
    }
    let s = unsafe { &*(ud as *const CueSink) };
    let ent = CueEnt { t_ns: time_ticks * s.tscale, byte: s.segment_pos + byte };
    if let Ok(mut v) = SHARED.cues.lock() {
        v.push(ent);
    }
}

// ---- the three worker threads ----

/// demux: open the PMS part URL, run the MKV demuxer pushing AUs to the queue. Loop
/// for seeks — the pump sets seek_byte + closes the socket to interrupt the read; we
/// re-open with a byte Range and resync to the next cluster.
pub(crate) fn stream_thread(host: String, port: c_int, path: String, aq: SendPtr<AuQueue>, hs: SendPtr<HttpStream>) {
    // unwrap_or_default: an interior NUL (only reachable via a malformed /tmp/poc-url)
    // yields an empty CString -> http_open fails gracefully, matching the C's degradation
    // (never a thread panic).
    // mut: a transcode seek re-points these at a new start.mkv?&offset= URL
    let mut host_c = std::ffi::CString::new(host).unwrap_or_default();
    let mut path_c = std::ffi::CString::new(path).unwrap_or_default();
    let mut port = port;
    let hs_p = hs.0;
    let aq_p = aq.0;
    super::log(&format!("stream: host={} port={port}", host_c.to_string_lossy()));
    let mut mkv: Box<MkvCtx> = Box::new(unsafe { std::mem::zeroed() });
    let mkv_p = &mut *mkv as *mut MkvCtx;
    let scratch = unsafe { libc::malloc(4 * 1024 * 1024) as *mut u8 };
    mkv.q = aq_p;
    mkv.scratch = scratch;
    mkv.scratch_cap = 4 * 1024 * 1024;
    let mut rc = StreamRead { hs: hs_p, mkv: mkv_p };
    mkv.read = Some(stream_read);
    mkv.ud = &mut rc as *mut StreamRead as *mut c_void;

    let mut start: i64 = 0;
    let mut first = true;
    loop {
        let extra = if start > 0 {
            Some(std::ffi::CString::new(format!("Range: bytes={start}-\r\n")).unwrap())
        } else {
            None
        };
        let ep = extra.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        if crate::stream::http_open(hs_p, host_c.as_ptr(), port, path_c.as_ptr(), ep) != 0 {
            super::log(&format!("stream: http_open FAILED status={}", crate::stream::hs_status(hs_p)));
            break;
        }
        if first {
            SHARED.file_size.store(crate::stream::hs_content_length(hs_p), Ordering::Release);
        }
        super::log(&format!(
            "stream: open status={} start={start} clen={} filesize={}",
            crate::stream::hs_status(hs_p),
            crate::stream::hs_content_length(hs_p),
            SHARED.file_size.load(Ordering::Relaxed)
        ));
        mkv.eof = 0;
        mkv.pos = 0;
        if first {
            crate::mkv::mkv_run(mkv_p);
            first = false;
        } else {
            crate::mkv::mkv_seek_run(mkv_p);
        }
        crate::stream::http_close(hs_p);
        if unsafe { crate::aq::aq_is_aborted(aq_p) } {
            break;
        }
        let sb = SHARED.seek_byte.swap(-1, Ordering::Acquire);
        if sb >= 0 {
            // a TRANSCODE seek re-points us at a fresh start.mkv?&offset= URL (opened from
            // byte 0); a direct-play seek keeps the same URL + a byte Range.
            if let Some(nu) = SHARED.next_url.lock().unwrap().take() {
                let (h, p, pa) = super::engine::parse_stream_url(&nu);
                host_c = std::ffi::CString::new(h).unwrap_or_default();
                path_c = std::ffi::CString::new(pa).unwrap_or_default();
                port = p;
                start = 0;
                // an audio switch from a direct-play stream needs the transcode's Tracks
                // re-parsed (mkv_run, via first=true) — its numbering differs from the
                // direct-play file's; a plain transcode seek leaves this false (same tracks).
                if SHARED.reparse_next.swap(false, Ordering::Acquire) {
                    first = true;
                }
                super::log("stream: seek → new transcode url (&offset)");
            } else {
                start = sb;
                super::log(&format!("stream: seek → byte {start}"));
            }
            continue;
        }
        break; // real EOF, no pending seek
    }
    crate::aq::aq_set_eof(aq_p);
    super::log("stream: demux ended");
    unsafe { libc::free(scratch as *mut c_void) };
    // mkv box drops here (socket already closed by us or by teardown)
}

/// cue preflight: parse the header for the Cues, fetch them by Range, build the
/// time->byte index in SHARED.cues.
pub(crate) fn cues_thread(host: String, port: c_int, path: String, hs2: SendPtr<HttpStream>) {
    // unwrap_or_default: an interior NUL (only reachable via a malformed /tmp/poc-url)
    // yields an empty CString -> http_open fails gracefully, matching the C's degradation
    // (never a thread panic).
    let host_c = std::ffi::CString::new(host).unwrap_or_default();
    let path_c = std::ffi::CString::new(path).unwrap_or_default();
    let hs2_p = hs2.0;
    super::log(&format!("cues: preflight start {}:{port}", host_c.to_string_lossy()));
    let mut cmkv: Box<MkvCtx> = Box::new(unsafe { std::mem::zeroed() });
    let cmkv_p = &mut *cmkv as *mut MkvCtx;
    cmkv.header_only = 1;
    if crate::stream::http_open(hs2_p, host_c.as_ptr(), port, path_c.as_ptr(), std::ptr::null()) != 0 {
        super::log("cues: preflight http_open FAILED");
        return;
    }
    cmkv.read = Some(hs2_read);
    cmkv.ud = hs2_p as *mut c_void;
    crate::mkv::mkv_run(cmkv_p); // stops at first Cluster; sets segment_pos, cues_pos, tscale
    crate::stream::http_close(hs2_p);
    let segpos = cmkv.segment_pos;
    let cuespos = cmkv.cues_pos;
    let tscale = cmkv.tscale;
    SHARED.segment_pos.store(segpos, Ordering::Relaxed);
    if cmkv.duration_ns > 0 {
        SHARED.duration_ns.store(cmkv.duration_ns, Ordering::Relaxed);
    }
    super::log(&format!("cues: header parsed segpos={segpos} cuespos={cuespos} tscale={tscale}"));
    if cuespos <= 0 || segpos <= 0 {
        super::log("cues: none");
        return;
    }
    if SHARED.cues_abort.load(Ordering::Acquire) {
        return; // teardown began during the header parse
    }
    let cues_abs = segpos + cuespos;
    let rh = std::ffi::CString::new(format!("Range: bytes={cues_abs}-\r\n")).unwrap();
    if crate::stream::http_open(hs2_p, host_c.as_ptr(), port, path_c.as_ptr(), rh.as_ptr()) != 0 {
        return;
    }
    if SHARED.cues_abort.load(Ordering::Acquire) {
        crate::stream::http_close(hs2_p);
        return; // abort raced the reopen
    }
    cmkv.read = Some(hs2_read);
    cmkv.ud = hs2_p as *mut c_void;
    cmkv.eof = 0;
    cmkv.pos = 0;
    let mut sink = CueSink { tscale, segment_pos: segpos };
    cmkv.cue_cb = Some(cue_cb);
    cmkv.cue_ud = &mut sink as *mut CueSink as *mut c_void;
    let mut id: c_uint = 0;
    let mut il: c_int = 0;
    let mut sz: i64 = 0;
    let mut sl: c_int = 0;
    if crate::mkv::ebml_id(cmkv_p, &mut id, &mut il) != 0
        && id == 0x1C53BB6B
        && crate::mkv::ebml_size(cmkv_p, &mut sz, &mut sl) != 0
    {
        crate::mkv::mkv_parse_cues(cmkv_p, sz);
    }
    crate::stream::http_close(hs2_p);
    if !SHARED.cues_abort.load(Ordering::Acquire) {
        SHARED.cues_ready.store(true, Ordering::Release); // don't mark a partial fetch ready
    }
    let ncues = SHARED.cues.lock().map(|v| v.len()).unwrap_or(0);
    super::log(&format!("cues: {ncues} points (tscale={tscale} segpos={segpos})"));
}

/// media/load thread: construct + Load (uid=NULL). The library owns its own
/// GMainContext + loop, so Load returns quickly and callbacks arrive on its thread.
pub(crate) fn load_thread(payload: SendPtr<c_char>) {
    super::log("SMP: calling Load (uid=NULL)");
    let ok = unsafe { super::ffi::sf_load(payload.0) };
    super::log(&format!("SMP: Load returned ok={ok}"));
}
