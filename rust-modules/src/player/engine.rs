//! player::engine — the main-thread-confined session object (Engine) + lifecycle
//! (acb_init / start_bufferfeed / stop_bufferfeed) + the feed loops. No worker
//! thread ever names an Engine field: race-free by confinement (like the C
//! main-thread-only flags). The Engine owns the two HttpStream boxes + the AuQueue
//! box; it hands raw ptrs to the workers and outlives them (drops after join).
use super::shared::Stage;
use super::{ffi, log, threads, ACB_OK, PTYPE, SHARED, TX};
use crate::aq::{AuNode, AuQueue};
use crate::stream::HttpStream;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicI64, Ordering};

// BUFFERSTREAM Load payloads (ss4s shape). Video-only for the local sample path;
// video+AC3 for streaming. Copied VERBATIM from playback.c.
const PAYLOAD_V: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"com.glin.plexpoc","externalStreamingInfo":{"contents":{"codec":{"video":"H264"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"plexpoc"},"streamQualityInfo":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":32768},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":false,"queryPosition":false,"lowDelayMode":true,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":1920,"maxHeight":1080,"maxFrameRate":30}}}]}"#;
// NB: pauseAtDecodeTime stays FALSE here. Kodi uses true, but only alongside its decode-time
// trigger machinery (setTimeToDecode); with true and no trigger the decoder never starts
// (verified on-device: Load+Play OK but zero frames decoded). The feed-ahead throttle
// (MAX_FEED_AHEAD_NS in feed_stream) is the anti-stall mechanism; the other Kodi payload
// flags are being re-introduced one at a time.
const PAYLOAD_AV: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"com.glin.plexpoc","externalStreamingInfo":{"contents":{"codec":{"video":"H264","audio":"AC3"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"plexpoc"},"streamQualityInfo":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":1048576},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":true,"queryPosition":false,"lowDelayMode":false,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":1920,"maxHeight":1080,"maxFrameRate":30}}}]}"#;
// Phase 0 HEVC probe payload — identical to PAYLOAD_V but codec video "H265", to isolate
// the single variable: does StarfishMediaAPIs BUFFERSTREAM decode HEVC on this panel?
const PAYLOAD_H265: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"com.glin.plexpoc","externalStreamingInfo":{"contents":{"codec":{"video":"H265"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"plexpoc"},"streamQualityInfo":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":32768},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":false,"queryPosition":false,"lowDelayMode":true,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":3840,"maxHeight":2160,"maxFrameRate":60}}}]}"#;

static VTOT: AtomicI64 = AtomicI64::new(0); // total video AUs fed (log cadence only)

pub(crate) struct SampleBuf {
    pub data: Vec<u8>,
    pub au: Vec<usize>, // AU start offsets
    pub next: usize,
    pub loops: i64,
}

pub(crate) enum Source {
    Stream, // host/port/path are consumed by the demux+cue threads at spawn
    Sample(Box<SampleBuf>),
}

/// owns a popped au_node; frees on drop (paired with the malloc in aq_push).
pub(crate) struct AuBox(pub *mut AuNode);
impl Drop for AuBox {
    fn drop(&mut self) {
        unsafe { libc::free(self.0 as *mut c_void) }
    }
}

/// MAIN-THREAD-CONFINED. No worker thread ever names an Engine field.
pub(crate) struct Engine {
    pub stage: Stage,
    pub video_info_sent: bool, // videoInfoSent
    pub rebase_pending: bool,  // g_rebase_pending
    pub max_fed_pts: i64,      // g_max_fed_pts
    pub aq: Option<Box<AuQueue>>, // g_aq (M owns; ptr handed to D)
    // hs/hs2/payload are RAII: held alive for the workers (which hold raw ptrs into
    // them) and freed only after join — never read back through the field.
    #[allow(dead_code)]
    pub hs: Box<HttpStream>, // demux socket (M owns; D uses via raw ptr)
    #[allow(dead_code)]
    pub hs2: Box<HttpStream>, // cue-preflight socket
    pub pending: Option<AuBox>, // bf_pending (held across BufferFull)
    #[allow(dead_code)]
    pub payload: std::ffi::CString, // bf_payload (kept alive for the session)
    pub source: Source,
    pub stream_th: Option<std::thread::JoinHandle<()>>,
    pub cues_th: Option<std::thread::JoinHandle<()>>,
    pub load_th: Option<std::thread::JoinHandle<()>>,
    pub report_th: Option<std::thread::JoinHandle<()>>, // /:/timeline progress reporter
    // soft WebVTT subtitle sidecar (transcode only): the socket box (M owns; subs thread
    // uses via raw ptr, RAII like hs/hs2), the thread handle (spawned lazily by the pump),
    // and the sid it is CURRENTLY streaming (0 = none; main-thread-confined).
    #[allow(dead_code)]
    pub hs3: Box<HttpStream>,
    pub subs_th: Option<std::thread::JoinHandle<()>>,
    pub subs_active_sid: i64,
}

static mut ENGINE: Option<Engine> = None; // main-thread-only slot
#[inline]
pub(crate) fn engine() -> Option<&'static mut Engine> {
    unsafe { (*std::ptr::addr_of_mut!(ENGINE)).as_mut() }
}

/// Create + initialize the ACB (App Common Binding). We deliberately DON'T register
/// our own com.webos.media client — it collides with the pipeline's uMS connection.
pub(crate) fn acb_init() {
    if let Ok(s) = std::fs::read_to_string("/tmp/poc-ptype") {
        if let Ok(p) = s.trim().parse::<c_int>() {
            PTYPE.store(p, Ordering::Relaxed);
        }
    }
    let pt = PTYPE.load(Ordering::Relaxed);
    log(&format!("ptype={pt}"));
    let app_c = std::env::var("APPID").ok().and_then(|s| std::ffi::CString::new(s).ok());
    let app_ptr = app_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let acb = unsafe { ffi::acb_create(app_ptr, pt) };
    ACB_OK.store(acb != 0, Ordering::Relaxed);
    log(&format!("acb create={acb}"));
}

/// split Annex-B into AUs on the 5-byte AUD prefix 00 00 00 01 <aud5>
/// (H264 AUD = 0x09; HEVC AUD is NAL type 35 → first header byte 0x46).
fn bf_split(data: &[u8], aud5: u8) -> Vec<usize> {
    let mut au = Vec::new();
    let mut i = 0usize;
    while i + 4 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1 && data[i + 4] == aud5 {
            au.push(i);
            i += 4;
        }
        i += 1;
    }
    au
}

/// Build the streamed BUFFERSTREAM Load payload from PAYLOAD_AV, substituting the item's real
/// video/audio codecs + a sink envelope. video = "H264"|"H265", audio = "AC3"|"EAC3"|"AAC".
/// The pipeline reads the true dimensions from the SPS (Phase 0 HEVC probe), so mw/mh are only
/// the sink envelope.
fn build_av_payload(video: &str, audio: &str, mw: i32, mh: i32) -> String {
    PAYLOAD_AV
        .replace(r#""video":"H264""#, &format!(r#""video":"{video}""#))
        .replace(r#""audio":"AC3""#, &format!(r#""audio":"{audio}""#))
        .replace(r#""maxWidth":1920"#, &format!(r#""maxWidth":{mw}"#))
        .replace(r#""maxHeight":1080"#, &format!(r#""maxHeight":{mh}"#))
        .replace(r#""maxFrameRate":30"#, r#""maxFrameRate":60"#)
}

/// parse http://HOST[:PORT]/PATH?query -> (host, port, path)
pub(crate) fn parse_stream_url(url: &str) -> (String, c_int, String) {
    let s = url.strip_prefix("http://").unwrap_or(url);
    let hostend = s.find(|c| c == ':' || c == '/').unwrap_or(s.len());
    let host = s[..hostend].to_string();
    let rest = &s[hostend..];
    if let Some(r) = rest.strip_prefix(':') {
        let pe = r.find('/').unwrap_or(r.len());
        let port = r[..pe].parse::<c_int>().unwrap_or(32400);
        let path = if pe < r.len() { r[pe..].to_string() } else { "/".to_string() };
        (host, port, path)
    } else {
        let path = if rest.is_empty() { "/".to_string() } else { rest.to_string() };
        (host, 32400, path)
    }
}

pub(crate) fn start_bufferfeed() -> bool {
    // Guard a double-start: overwriting a live ENGINE slot would DROP the running
    // Engine, detaching its worker threads and freeing the hs/hs2/aq boxes those
    // threads still hold raw ptrs into -> use-after-free. If already running, no-op.
    // (Reachable via a PLAY key landing in the WILL->DID foreground window.)
    if unsafe { (*std::ptr::addr_of!(ENGINE)).is_some() } {
        log("start_bufferfeed: already running (no-op)");
        return true;
    }
    // resolve the URL: route (a selected movie) wins, then /tmp/poc-url, then a local
    // sample, then the built-in demo movie.
    let mut url = crate::route::url();
    if url.is_empty() {
        if let Ok(s) = std::fs::read_to_string("/tmp/poc-url") {
            let t = s.trim().to_string();
            if !t.is_empty() {
                url = t;
                crate::route::set_url(&url);
            }
        }
    }
    let mut sample: Option<Box<SampleBuf>> = None;
    let mut is_h265 = false;
    if url.is_empty() {
        if let Ok(data) = std::fs::read("/tmp/sample.h264") {
            let au = bf_split(&data, 0x09);
            log(&format!("bf_split h264: {} AUs in {} bytes", au.len(), data.len()));
            if au.len() < 2 {
                return false;
            }
            sample = Some(Box::new(SampleBuf { data, au, next: 0, loops: 0 }));
        } else if let Ok(data) = std::fs::read("/tmp/sample.h265") {
            // Phase 0 probe: feed a local HEVC Annex-B sample to test native HEVC decode.
            let au = bf_split(&data, 0x46);
            log(&format!("bf_split h265: {} AUs in {} bytes", au.len(), data.len()));
            if au.len() < 2 {
                return false;
            }
            is_h265 = true;
            sample = Some(Box::new(SampleBuf { data, au, next: 0, loops: 0 }));
        } else {
            url = crate::route::demo_url();
            crate::route::set_url(&url);
        }
    }
    let stream = sample.is_none();
    // For a streamed direct-play/transcode, pick the Load codecs from the item: video H264 vs
    // H265 (native HEVC direct-play), audio AC3/EAC3/AAC. (The local sample paths keep their
    // fixed payloads.)
    // dev A/B: /tmp/poc-noaudio feeds video only (needAudio:false + skip es=2) to isolate
    // whether the audio ES (E-AC3/Atmos) is what stalls the sink on 4K HEVC.
    let no_audio = std::path::Path::new("/tmp/poc-noaudio").exists();
    crate::ff::set_feed_audio(!no_audio);
    let stream_payload;
    let payload_str: &str = if stream {
        let hevc = crate::route::stream_vcodec() == "hevc";
        if no_audio {
            if hevc { PAYLOAD_H265 } else { PAYLOAD_V }
        } else {
            let vc = if hevc { "H265" } else { "H264" };
            // LG's pipeline names E-AC3 "AC3 PLUS" (Dolby Digital Plus), NOT "EAC3" — the
            // wrong string leaves the audio ES unconfigured, and with audioSync the video
            // sink slaves to the dead audio clock and stalls (verified: video-only plays).
            let ac = match crate::route::stream_acodec().as_str() {
                "eac3" => "AC3 PLUS",
                "aac" => "AAC",
                _ => "AC3",
            };
            let (mw, mh) = if hevc { (3840, 2160) } else { (1920, 1080) };
            stream_payload = build_av_payload(vc, ac, mw, mh);
            &stream_payload
        }
    } else if is_h265 {
        PAYLOAD_H265
    } else {
        PAYLOAD_V
    };
    let payload_c = std::ffi::CString::new(payload_str).unwrap();

    // fd = -1 (CLOSED) so a teardown before/without http_open doesn't close(0)
    let mut hs = crate::stream::http_stream_boxed();
    let mut hs2 = crate::stream::http_stream_boxed();
    let mut hs3 = crate::stream::http_stream_boxed(); // soft-subs sidecar (spawned lazily by the pump)
    let mut aq_box: Option<Box<AuQueue>> = None;
    let mut stream_th = None;
    let mut cues_th = None;
    let source;

    if stream {
        let (host, port, path) = parse_stream_url(&url);
        log(&format!("stream: host={host} port={port} path={}", &path[..path.len().min(80)]));
        let mut q: Box<AuQueue> = Box::new(unsafe { std::mem::zeroed() });
        crate::aq::aq_init(&mut *q);
        let aq_raw = &mut *q as *mut AuQueue;
        let hs_raw = &mut *hs as *mut HttpStream;
        let hs2_raw = &mut *hs2 as *mut HttpStream;
        let hs3_raw = &mut *hs3 as *mut HttpStream;
        SHARED.hs_ptr.store(hs_raw, Ordering::Release);
        SHARED.hs2_ptr.store(hs2_raw, Ordering::Release);
        SHARED.hs3_ptr.store(hs3_raw, Ordering::Release);
        SHARED.seek_byte.store(-1, Ordering::Relaxed);
        {
            let (h, p) = (host.clone(), path.clone());
            let aqp = threads::SendPtr(aq_raw);
            let hsp = threads::SendPtr(hs_raw);
            stream_th = Some(std::thread::spawn(move || threads::stream_thread(h, port, p, aqp, hsp)));
        }
        // skip the cue preflight for a transcode (no byte-cues; a 2nd conn cuts the stream)
        if !SHARED.cues_ready.load(Ordering::Relaxed) && crate::route::transcode_session().is_empty() {
            SHARED.cues_abort.store(false, Ordering::Relaxed);
            let (h, p) = (host.clone(), path.clone());
            let hs2p = threads::SendPtr(hs2_raw);
            cues_th = Some(std::thread::spawn(move || threads::cues_thread(h, port, p, hs2p)));
        }
        aq_box = Some(q);
        let _ = (host, port, path); // consumed above; keep the bindings' last use explicit
        source = Source::Stream;
    } else {
        source = Source::Sample(sample.unwrap());
    }

    // the media thread constructs + loads + runs the loop (owns the GMainContext)
    let payload_ptr = threads::SendPtr(payload_c.as_ptr() as *mut c_char);
    let load_th = Some(std::thread::spawn(move || threads::load_thread(payload_ptr)));

    // progress reporter: post the play position to /:/timeline (updates resume + watched).
    // rk is captured now (fixed for the session); skipped for the sample/demo (no rk).
    SHARED.report_stop.store(false, Ordering::Relaxed);
    let report_th = if stream {
        let rk = crate::route::cur_rk();
        match (rk.is_empty(), crate::route::config()) {
            (false, Some((h, p, t))) => Some(std::thread::spawn(move || threads::timeline_thread(h, p, t, rk))),
            _ => None,
        }
    } else {
        None
    };

    let eng = Engine {
        stage: Stage::Loading,
        video_info_sent: false,
        // if a seek is armed for the FIRST open (resume, or reload_at), rebase the first
        // post-seek keyframe to fed-pts 0 so the pipeline sees a 0-based timeline identical
        // to fresh play (disp_base carries the content offset). Plain fresh play leaves this
        // false (first keyframe is already ~0).
        rebase_pending: SHARED.seek_to_ns.load(Ordering::Relaxed) >= 0,
        max_fed_pts: 0,
        aq: aq_box,
        hs,
        hs2,
        pending: None,
        payload: payload_c,
        source,
        stream_th,
        cues_th,
        load_th,
        report_th,
        hs3,
        subs_th: None,
        subs_active_sid: 0,
    };
    unsafe {
        *std::ptr::addr_of_mut!(ENGINE) = Some(eng);
    }
    TX.started.store(true, Ordering::Relaxed);
    log(&format!("SMP: media thread spawned, stream={}", stream as i32));
    true
}

/// Arm the demuxer to open+seek to `target_ns` on the NEXT Load, displaying honest content
/// time. disp_base=0 and (via start_bufferfeed) rebase_pending=true, so feed_stream rebases
/// the landed keyframe K to fed-pts 0 and the presented position reads as num+K = content
/// time. Call BEFORE start_bufferfeed (resume) or via reload_at (mid-play seek).
pub(crate) fn arm_seek(target_ns: i64) {
    let t = target_ns.max(0);
    SHARED.seek_to_ns.store(t, Ordering::Release);
    SHARED.disp_base.store(0, Ordering::Relaxed);
    SHARED.playpos_ns.store(t, Ordering::Relaxed); // instant HUD feedback until frames land
}

/// Direct-play seek = tear down the pipeline and start a FRESH Load at `target_ns`. The old
/// flush()+refeed path left a STALE GStreamer segment (decompiled ground truth: the no-arg
/// StarfishMediaAPIs::flush() → CustomPipeline::flush() is a degenerate gst_element_seek to
/// GST_CLOCK_TIME_NONE with NO FLUSH_START/STOP and NO fresh SEGMENT; the HW sink/decoder
/// only re-anchor their segment/basetime on a real SEGMENT/FLUSH event). Post-seek buffers
/// were then scheduled against the pre-seek segment, the sink stopped draining, and the fixed
/// ~14.7 MB of upstream buffers filled in ~48 s → permanent BufferFull + "Playing error". A
/// fresh Load re-establishes a correct segment by construction — the known-good fresh-play
/// path, which never wedges. Heavier than a flush (a ~1 s re-preroll) but correct.
pub(crate) fn reload_at(target_ns: i64) {
    if crate::route::url().is_empty() {
        log("reload_at: no url (ignored)");
        return;
    }
    log(&format!("reload_at: fresh Load at {}s", target_ns / 1_000_000_000));
    teardown(true, true); // keep cues; reload mode: preserve the session (no url-clear / stop-scrobble)
    arm_seek(target_ns);
    start_bufferfeed();
}

/// Reload the pipeline for a MODE/CODEC change — an audio-track switch on a direct-play HEVC
/// item forces a transcode (H264/AC3), so the pipeline must be re-Loaded with the H264 payload
/// (feeding H264 into the H265-configured pipeline stalls). Unlike reload_at, the transcode
/// start.mkv is already 0-based at `&offset`, so no av_seek — just set disp_base to the offset.
/// route::retranscode has already set the URL + session + STREAM_VCODEC=h264 before this call.
pub(crate) fn reload_transcode(offset_ns: i64) {
    if crate::route::url().is_empty() {
        log("reload_transcode: no url (ignored)");
        return;
    }
    log(&format!("reload_transcode: fresh Load at offset {}s", offset_ns / 1_000_000_000));
    teardown(true, true); // keep cues/session; reload mode
    SHARED.disp_base.store(offset_ns, Ordering::Relaxed); // transcode is 0-based at content=offset
    SHARED.playpos_ns.store(offset_ns, Ordering::Relaxed);
    start_bufferfeed();
}

/// Stop playback: unblock+join threads, unload+destruct the pipeline, release the
/// video plane, reset all state so a fresh start_bufferfeed() can restart.
pub(crate) fn stop_bufferfeed(keep_cues: bool) {
    teardown(keep_cues, false);
}

/// The teardown body. `for_reload` = this is a direct-play seek reload (reload_at), NOT a real
/// stop: preserve the playback session so start_bufferfeed can restart the SAME item — skip
/// the "stopped" timeline scrobble, the server transcode stop, and the URL clear.
fn teardown(keep_cues: bool, for_reload: bool) {
    let mut eng = match unsafe { (*std::ptr::addr_of_mut!(ENGINE)).take() } {
        Some(e) => e,
        None => return,
    };
    let stream = matches!(eng.source, Source::Stream { .. });

    // capture the final-position report BEFORE teardown zeroes playpos/duration (a reload is
    // not a stop — don't scrobble "stopped", it would falsely pause/mark-watched the item)
    let final_report = if for_reload {
        None
    } else {
        let rk = crate::route::cur_rk();
        let dur = SHARED.duration_ns.load(Ordering::Relaxed);
        if !rk.is_empty() && dur > 0 {
            crate::route::config()
                .map(|(h, p, t)| (h, p, t, rk, SHARED.playpos_ns.load(Ordering::Relaxed) / 1_000_000, dur / 1_000_000))
        } else {
            None
        }
    };

    // 1. stop the cue preflight FIRST + unblock every thread (abort queue, close sockets)
    SHARED.cues_abort.store(true, Ordering::Release);
    SHARED.subs_abort.store(true, Ordering::Release); // stop the soft-subs sidecar thread
    SHARED.report_stop.store(true, Ordering::Release); // stop the /:/timeline reporter
    if stream {
        if let Some(q) = eng.aq.as_mut() {
            crate::aq::aq_abort(&mut **q);
        }
        let p = SHARED.hs_ptr.load(Ordering::Acquire);
        if !p.is_null() {
            crate::stream::http_close(p);
        }
        let p2 = SHARED.hs2_ptr.load(Ordering::Acquire);
        if !p2.is_null() {
            crate::stream::http_close(p2);
        }
        let p3 = SHARED.hs3_ptr.load(Ordering::Acquire);
        if !p3.is_null() {
            crate::stream::http_close(p3);
        }
    }
    // 2. JOIN before touching the cue Vec (cue_cb writes it on the preflight thread)
    if let Some(t) = eng.cues_th.take() {
        let _ = t.join();
    }
    if let Some(t) = eng.stream_th.take() {
        let _ = t.join();
    }
    if let Some(t) = eng.load_th.take() {
        let _ = t.join();
    }
    if let Some(t) = eng.report_th.take() {
        let _ = t.join();
    }
    if let Some(t) = eng.subs_th.take() {
        let _ = t.join();
    }
    // final position report (state=stopped) so the server commits the resume point
    if let Some((h, p, t, rk, pos, dur)) = final_report {
        let path = threads::timeline_path(&rk, "stopped", pos, dur, &t);
        let _ = crate::stream::http_post(&h, p, &path, None);
        log(&format!("timeline stopped t={}s/{}s", pos / 1000, dur / 1000));
    }
    // 3. unload + destruct the pipeline, release the plane
    if unsafe { ffi::sf_ready() } != 0 {
        unsafe { ffi::sf_unload() };
        if ACB_OK.load(Ordering::Relaxed) {
            unsafe { ffi::acb_unload() };
        }
        unsafe { ffi::sf_destroy() };
    }
    // 4. drain + destroy the queue
    if stream {
        drain_aq(&mut eng);
        if let Some(q) = eng.aq.as_mut() {
            crate::aq::aq_destroy(&mut **q);
        }
    }
    eng.pending = None;
    // 5. reset shared + transport. On a real stop also stop the server transcode + clear the
    // URL; on a reload KEEP them so start_bufferfeed restarts the same item (a direct-play
    // reload has no transcode session anyway, so the skip only matters for the URL).
    SHARED.reset_session();
    TX.reset();
    if !for_reload {
        crate::route::stop_transcode();
        crate::route::clear_url();
    }
    // 6. keep the cue index across an app-switch (same file) only if FULLY loaded
    if !keep_cues || !SHARED.cues_ready.load(Ordering::Relaxed) {
        SHARED.cues.lock().unwrap().clear();
        SHARED.cues_ready.store(false, Ordering::Relaxed);
        SHARED.segment_pos.store(0, Ordering::Relaxed);
    }
    log("stop_bufferfeed: torn down");
    // Engine (hs/hs2/aq boxes, payload) drops here — after all joins
}

/// nearest cue at or before t (absolute byte), or None if the index isn't ready.
pub(crate) fn cue_byte_for(t: i64) -> Option<i64> {
    if !SHARED.cues_ready.load(Ordering::Relaxed) {
        return None;
    }
    let v = SHARED.cues.lock().unwrap();
    if v.is_empty() {
        return None;
    }
    let (mut lo, mut hi, mut best) = (0i64, v.len() as i64 - 1, -1i64);
    while lo <= hi {
        let mid = (lo + hi) / 2;
        if v[mid as usize].t_ns <= t {
            best = mid;
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    Some(v[if best < 0 { 0 } else { best as usize }].byte)
}

/// free every queued AU + the held pending one (seek + teardown).
pub(crate) fn drain_aq(eng: &mut Engine) {
    if let Some(q) = eng.aq.as_mut() {
        let qp = &mut **q as *mut AuQueue;
        let mut eof: c_int = 0;
        loop {
            let n = crate::aq::aq_pop(qp, &mut eof);
            if n.is_null() {
                break;
            }
            unsafe { libc::free(n as *mut c_void) };
        }
    }
    eng.pending = None;
}

/// feed streamed AUs from the demux queue; hold the current AU across ticks on
/// BufferFull (backpressure); zero-base the fed timeline on the first post-seek
/// keyframe; drop stale AUs past the B-frame reorder distance.
pub(crate) fn feed_stream(eng: &mut Engine) {
    let qp = match eng.aq.as_mut() {
        Some(q) => &mut **q as *mut AuQueue,
        None => return,
    };
    let mut fed = 0;
    while fed < 120 {
        // NB: Kodi paces the feed to ~1.6s ahead of the presented position (SHARED.pres_fed),
        // but that needs the two-lane audio/video split first — a single combined max_fed_pts
        // is audio-dominated and starves video. Greedy-to-BufferFull for now (task: two-lane feed).
        if eng.pending.is_none() {
            let mut eof: c_int = 0;
            let n = crate::aq::aq_pop(qp, &mut eof);
            if n.is_null() {
                break;
            }
            eng.pending = Some(AuBox(n));
        }
        let n = eng.pending.as_ref().unwrap().0;
        let (es, key, pts, len, data) = unsafe { crate::aq::au_fields(n) };
        if eng.rebase_pending {
            if es == 1 && key != 0 {
                SHARED.pts_shift.store(-pts, Ordering::Relaxed);
                eng.rebase_pending = false;
                log(&format!("rebase: first post-seek keyframe pts={pts} -> pts_shift={}", -pts));
            } else {
                eng.pending = None; // drop pre-keyframe AUs
                continue;
            }
        }
        let mut fp = pts + SHARED.pts_shift.load(Ordering::Relaxed);
        if fp < eng.max_fed_pts - 2_000_000_000 {
            eng.pending = None; // stale (a big backward jump)
            continue;
        }
        if fp < 0 {
            fp = 0;
        }
        let r = unsafe { ffi::sf_feed(data, len as u32, fp, es) };
        if fp > eng.max_fed_pts {
            eng.max_fed_pts = fp;
        }
        if es == 1 {
            let v = VTOT.fetch_add(1, Ordering::Relaxed) + 1;
            if v <= 4 || v % 100 == 0 {
                let qb = crate::aq::aq_bytes(qp);
                log(&format!("feed v#{v} sz={len} fed={fp} reply={} qbytes={qb}", r as u8 as char));
            }
        }
        if (r as u8) != b'O' {
            break; // 'B' BufferFull -> keep pending, retry next tick
        }
        eng.pending = None;
        fed += 1;
    }
}

/// feed the looped /tmp/sample.h264 validation sample (continuous PTS @ 23.976).
pub(crate) fn feed_sample(eng: &mut Engine) {
    let s = match &mut eng.source {
        Source::Sample(s) => s,
        _ => return,
    };
    let naus = s.au.len();
    if naus < 2 {
        return;
    }
    let mut fed = 0;
    while fed < 60 {
        if s.next >= naus - 1 {
            s.next = 0;
            s.loops += 1;
        }
        let off = s.au[s.next];
        let end = s.au[s.next + 1];
        let pts = (s.loops * (naus as i64 - 1) + s.next as i64) * 41708333;
        let r = unsafe { ffi::sf_feed(s.data[off..].as_ptr(), (end - off) as u32, pts, 1) };
        if (r as u8) != b'O' {
            break;
        }
        s.next += 1;
        fed += 1;
    }
}
