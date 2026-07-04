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
const PAYLOAD_AV: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"com.glin.plexpoc","externalStreamingInfo":{"contents":{"codec":{"video":"H264","audio":"AC3"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"plexpoc"},"streamQualityInfo":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":1048576},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":true,"queryPosition":false,"lowDelayMode":false,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":1920,"maxHeight":1080,"maxFrameRate":30}}}]}"#;

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

/// split Annex-B into AUs on the 5-byte AUD prefix 00 00 00 01 09
fn bf_split(data: &[u8]) -> Vec<usize> {
    let mut au = Vec::new();
    let mut i = 0usize;
    while i + 4 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1 && data[i + 4] == 0x09 {
            au.push(i);
            i += 4;
        }
        i += 1;
    }
    au
}

/// parse http://HOST[:PORT]/PATH?query -> (host, port, path)
fn parse_stream_url(url: &str) -> (String, c_int, String) {
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
    if url.is_empty() {
        match std::fs::read("/tmp/sample.h264") {
            Ok(data) => {
                let au = bf_split(&data);
                log(&format!("bf_split: {} AUs in {} bytes", au.len(), data.len()));
                if au.len() < 2 {
                    return false;
                }
                sample = Some(Box::new(SampleBuf { data, au, next: 0, loops: 0 }));
            }
            Err(_) => {
                url = crate::route::demo_url();
                crate::route::set_url(&url);
            }
        }
    }
    let stream = sample.is_none();
    let payload_c = std::ffi::CString::new(if stream { PAYLOAD_AV } else { PAYLOAD_V }).unwrap();

    let mut hs: Box<HttpStream> = Box::new(unsafe { std::mem::zeroed() });
    let mut hs2: Box<HttpStream> = Box::new(unsafe { std::mem::zeroed() });
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
        SHARED.hs_ptr.store(hs_raw, Ordering::Release);
        SHARED.hs2_ptr.store(hs2_raw, Ordering::Release);
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

    let eng = Engine {
        stage: Stage::Loading,
        video_info_sent: false,
        rebase_pending: false,
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
    };
    unsafe {
        *std::ptr::addr_of_mut!(ENGINE) = Some(eng);
    }
    TX.started.store(true, Ordering::Relaxed);
    log(&format!("SMP: media thread spawned, stream={}", stream as i32));
    true
}

/// Stop playback: unblock+join threads, unload+destruct the pipeline, release the
/// video plane, reset all state so a fresh start_bufferfeed() can restart.
pub(crate) fn stop_bufferfeed(keep_cues: bool) {
    let mut eng = match unsafe { (*std::ptr::addr_of_mut!(ENGINE)).take() } {
        Some(e) => e,
        None => return,
    };
    let stream = matches!(eng.source, Source::Stream { .. });

    // 1. stop the cue preflight FIRST + unblock every thread (abort queue, close sockets)
    SHARED.cues_abort.store(true, Ordering::Release);
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
    // 5. reset shared + transport, stop the server transcode, clear the URL
    SHARED.reset_session();
    crate::route::stop_transcode();
    TX.reset();
    crate::route::clear_url();
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
