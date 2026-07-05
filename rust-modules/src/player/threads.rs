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
        if crate::stream::http_open(hs_p, host_c.as_ptr(), port, path_c.as_ptr(), ep, "GET") != 0 {
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
    if crate::stream::http_open(hs2_p, host_c.as_ptr(), port, path_c.as_ptr(), std::ptr::null(), "GET") != 0 {
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
    if crate::stream::http_open(hs2_p, host_c.as_ptr(), port, path_c.as_ptr(), rh.as_ptr(), "GET") != 0 {
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

/// progress-reporter thread: every ~10s, POST the current position to Plex's
/// /:/timeline so the server updates viewOffset (the resume point) + watched state.
/// `rk` is captured at spawn (fixed per playback session, no static-mut race). Exits
/// when SHARED.report_stop is set; the final state=stopped report is sent by
/// stop_bufferfeed (main thread) with the last position.
pub(crate) fn timeline_thread(host: String, port: c_int, token: String, rk: String) {
    loop {
        // sleep ~10s in 1s steps so we exit promptly on teardown
        for _ in 0..10 {
            if SHARED.report_stop.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        if SHARED.report_stop.load(Ordering::Acquire) {
            return;
        }
        let dur = SHARED.duration_ns.load(Ordering::Relaxed);
        if dur <= 0 || rk.is_empty() {
            continue;
        }
        let t = SHARED.playpos_ns.load(Ordering::Relaxed) / 1_000_000;
        let d = dur / 1_000_000;
        let state = if super::TX.paused.load(Ordering::Relaxed) { "paused" } else { "playing" };
        let path = timeline_path(&rk, state, t, d, &token);
        let _ = crate::stream::http_post(&host, port, &path, None);
        super::log(&format!("timeline {state} t={}s/{}s", t / 1000, d / 1000));
    }
}

/// Build the POST /:/timeline query string (the spec verb): identity + session + PlayQueue +
/// the SELECTED audio/subtitle stream ids, so /status/sessions shows the right track and the
/// Direct Play vs Transcode badge (correlated by X-Plex-Session-Identifier == transcode session=).
pub(crate) fn timeline_path(rk: &str, state: &str, t_ms: i64, d_ms: i64, token: &str) -> String {
    let sess = crate::route::sess();
    let (pq, pqi) = (crate::route::pq_id(), crate::route::pq_item_id());
    let (a, s) = (crate::route::cur_audio_sid(), crate::route::cur_sub_sid());
    let mut p = format!(
        "/:/timeline?ratingKey={rk}&key=%2Flibrary%2Fmetadata%2F{rk}\
         &identifier=com.plexapp.plugins.library&state={state}&time={t_ms}&duration={d_ms}\
         &X-Plex-Session-Identifier={sess}{id}&X-Plex-Token={token}",
        id = crate::route::identity_qs()
    );
    if !pq.is_empty() {
        p.push_str(&format!("&playQueueID={pq}"));
    }
    if !pqi.is_empty() {
        p.push_str(&format!("&playQueueItemID={pqi}"));
    }
    if a > 0 {
        p.push_str(&format!("&audioStreamID={a}"));
    }
    if s > 0 {
        p.push_str(&format!("&subtitleStreamID={s}"));
    }
    p
}

/// soft-subtitle sidecar (transcode only): open the /video/:/transcode/universal/subtitles
/// URL on the SAME transcode session, read the WebVTT body incrementally, and push cues
/// into SHARED.sub_cues (rebased by disp_base) — the SAME store the direct-play demuxer
/// fills, so player_hud::draw_subtitles renders both unchanged. Loops for seek/retranscode/
/// track-switch: the pump publishes subs_next_url + closes hs3 to interrupt the blocked recv;
/// we re-open on the new-offset URL and (the pump having cleared) re-seed cues.
pub(crate) fn subs_thread(host: String, port: c_int, path: String, hs3: SendPtr<HttpStream>) {
    let host_c = std::ffi::CString::new(host).unwrap_or_default();
    let mut path_c = std::ffi::CString::new(path).unwrap_or_default();
    let hs3_p = hs3.0;
    super::log("subs: sidecar thread start");
    loop {
        if crate::stream::http_open(hs3_p, host_c.as_ptr(), port, path_c.as_ptr(), std::ptr::null(), "GET") != 0 {
            super::log(&format!("subs: http_open FAILED status={}", crate::stream::hs_status(hs3_p)));
        } else {
            let mut parser = crate::webvtt::VttParser::new();
            let mut buf = vec![0u8; 65536];
            loop {
                let r = crate::stream::http_read(hs3_p, buf.as_mut_ptr(), buf.len() as c_int);
                if r <= 0 {
                    break; // EOF, or unblocked by http_close on seek/teardown
                }
                if SHARED.subs_abort.load(Ordering::Acquire) {
                    break;
                }
                for cue in parser.push(&buf[..r as usize]) {
                    push_vtt_cue(cue);
                }
            }
            for cue in parser.finish() {
                push_vtt_cue(cue);
            }
        }
        crate::stream::http_close(hs3_p);
        if SHARED.subs_abort.load(Ordering::Acquire) {
            break;
        }
        // re-open on the URL the pump published (seek / retranscode / track switch). Host/port
        // never change (same PMS) — only the subtitles?…&offset= path.
        let nu = SHARED.subs_next_url.lock().unwrap().take();
        if let Some(nu) = nu {
            let (_, _, pa) = super::engine::parse_stream_url(&nu);
            path_c = std::ffi::CString::new(pa).unwrap_or_default();
            continue;
        }
        break; // real EOF, no pending re-open
    }
    super::log("subs: thread ended");
}

/// push one parsed WebVTT cue into the shared store, rebased onto content-time ns by the
/// same disp_base the fed video PTS uses (§4). Logged so on-device verification can see
/// cues flowing alongside the video feed.
fn push_vtt_cue(cue: crate::webvtt::VttCue) {
    if cue.text.trim().is_empty() {
        return;
    }
    let base = SHARED.disp_base.load(Ordering::Relaxed);
    let (s, e) = (cue.start_ns + base, cue.end_ns + base);
    super::log(&format!("subs cue [{}..{}ms] {:?}", s / 1_000_000, e / 1_000_000,
        cue.text.chars().take(34).collect::<String>()));
    super::push_subtitle_text(s, e, cue.text);
}
