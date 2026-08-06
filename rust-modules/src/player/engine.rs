//! player::engine — the main-thread-confined session object (Engine) + lifecycle
//! (acb_init / start_bufferfeed / stop_bufferfeed) + the feed loops. No worker
//! thread ever names an Engine field: race-free by confinement (like the C
//! main-thread-only flags). The Engine owns the HttpStream box + the AuQueue
//! boxes; it hands raw ptrs to the workers and outlives them (drops after join).
//!
//! That confinement is **enforced, not just asserted**: the `ENGINE` slot is reachable only
//! through the four accessors below, each of which takes a [`MainThread`]. Which is also the
//! rule for the rest of this file — a function here takes the token **iff** it reaches the slot
//! or the ACB/Starfish seam, so the parameter carries information. `arm_seek` and `resume_at`
//! run on the main thread too and deliberately do not take one: they only publish to `SHARED`.
use super::shared::Stage;
use super::{ffi, log, threads, ACB_OK, PTYPE, SHARED, TX};
use crate::aq::{AuNode, AuQueue};
use crate::stream::HttpStream;
use crate::task::MainThread;
use std::os::raw::{c_char, c_int, c_long, c_void};
use std::sync::atomic::{AtomicI64, Ordering};

// BUFFERSTREAM Load payloads (ss4s shape). Video-only for the local sample path;
// video+AC3 for streaming. Copied VERBATIM from playback.c.
const PAYLOAD_V: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"com.beb.plxnative","externalStreamingInfo":{"contents":{"codec":{"video":"H264"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"plxnative"},"streamQualityInfo":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":32768},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":false,"queryPosition":false,"lowDelayMode":true,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":1920,"maxHeight":1080,"maxFrameRate":30}}}]}"#;
// NB: pauseAtDecodeTime stays FALSE here. Kodi uses true, but only alongside its decode-time
// trigger machinery (setTimeToDecode); with true and no trigger the decoder never starts
// (verified on-device: Load+Play OK but zero frames decoded). The feed-ahead throttle
// (MAX_FEED_AHEAD_NS in feed_stream) is the anti-stall mechanism; the other Kodi payload
// flags are being re-introduced one at a time.
const PAYLOAD_AV: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"com.beb.plxnative","externalStreamingInfo":{"contents":{"codec":{"video":"H264","audio":"AC3"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"plxnative"},"streamQualityInfo":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":1048576},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":true,"queryPosition":false,"lowDelayMode":false,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":1920,"maxHeight":1080,"maxFrameRate":30}}}]}"#;
// Phase 0 HEVC probe payload — identical to PAYLOAD_V but codec video "H265", to isolate
// the single variable: does StarfishMediaAPIs BUFFERSTREAM decode HEVC on this panel?
const PAYLOAD_H265: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"com.beb.plxnative","externalStreamingInfo":{"contents":{"codec":{"video":"H265"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"plxnative"},"streamQualityInfo":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":32768},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":false,"queryPosition":false,"lowDelayMode":true,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":3840,"maxHeight":2160,"maxFrameRate":60}}}]}"#;

static VTOT: AtomicI64 = AtomicI64::new(0); // total video AUs fed (log cadence only)
static ATOT: AtomicI64 = AtomicI64::new(0); // total audio AUs fed (log cadence only)

// Per-lane queue byte caps (two-lane feed). Video matches the pipeline's srcBufferLevelVideo (8MB);
// audio is kept small (the TV is RAM-tight and audio frames are tiny) yet large enough to cushion
// the single demux thread briefly blocking on a full video lane.
const AQ_VIDEO_BYTES: c_long = 8 * 1024 * 1024;
const AQ_AUDIO_BYTES: c_long = 1024 * 1024;

pub(crate) struct SampleBuf {
    pub data: Vec<u8>,
    pub au: Vec<usize>, // AU start offsets
    pub next: usize,
    pub loops: i64,
}

pub(crate) enum Source {
    Stream, // host/port/path are consumed by the demux thread at spawn
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
    pub eos_pushed: bool,      // Kodi VIDEO_DRAIN: pushEOS() sent once at true EOF
    pub rebase_pending: bool,  // g_rebase_pending
    // In-place seek only: keyframes this far AHEAD of the seek target are stale frames the demuxer
    // produced from its pre-flush read position before the reopen+av_seek won the race (playback
    // drifts forward during a long scrub) — reject them so the rebase anchors on the REAL post-seek
    // keyframe, not the drifted one. `rebase_drops` caps the rejections so a sparse-keyframe file or
    // a genuinely-failed av_seek can't hang the rebase.
    pub rebase_drops: i32,
    pub seek_armed_at: u32,    // SDL-ticks when the current in-place seek was armed (stuck watchdog)
    pub seek_retries: i32,     // cheap reopen-retries attempted before escalating to a full reload
    pub flushed: bool,         // Kodi m_flushed: set on an in-place seek flush; the first
    // post-flush keyframe triggers setTimeToDecode + sendSegmentEvent (the fresh GStreamer
    // segment a bare flush() omits), then clears this.
    pub max_fed_video_pts: i64, // high-water fed pts, VIDEO lane (g_max_fed_pts)
    pub max_fed_audio_pts: i64, // high-water fed pts, AUDIO lane (two-lane feed)
    pub seek_base_pts: i64,    // fed pts of the first post-seek keyframe (prime measures buffer
    // depth as max_fed_video_pts - seek_base_pts, since the in-place seek feeds REAL pts, not 0-based)
    // prime-then-play: after a seek/resume the pipeline is PAUSED and data is buffered before
    // Play, so the clock doesn't free-run through the demux reopen / transcode-restart gap (that
    // gap is what makes video "fast-forward" to catch the audio clock on resume). feed_stream
    // fires Play once max_fed_pts reaches PRIME_NS.
    pub prime_play: bool,
    // Two-lane feed (Kodi m_messageQueueVideo/Audio): the demuxer routes es=1 video to aq_video
    // and es=2 audio to aq_audio; each lane is fed independently so a video BufferFull can't stall
    // the audio lane (the audioSync master clock). Both are allocated for a stream. (None only
    // pre-start and on the local-sample source.)
    pub aq_video: Option<Box<AuQueue>>, // g_aq (M owns; ptr handed to D)
    pub aq_audio: Option<Box<AuQueue>>, // audio lane
    // hs/payload are RAII: held alive for the workers (which hold raw ptrs into
    // them) and freed only after join — never read back through the field.
    #[allow(dead_code)]
    pub hs: Box<HttpStream>, // demux socket (M owns; D uses via raw ptr)
    pub pending_video: Option<AuBox>, // bf_pending, VIDEO lane (held across BufferFull)
    pub pending_audio: Option<AuBox>, // bf_pending, AUDIO lane
    #[allow(dead_code)]
    pub payload: std::ffi::CString, // bf_payload (kept alive for the session)
    pub source: Source,
    pub stream_th: Option<std::thread::JoinHandle<()>>,
    pub load_th: Option<std::thread::JoinHandle<()>>,
    pub report_th: Option<std::thread::JoinHandle<()>>, // /:/timeline progress reporter
    pub report_stop: Option<std::sync::Arc<threads::ReportStop>>, // ITS stop signal, not SHARED's
}

static mut ENGINE: Option<Engine> = None; // main-thread-only slot

/// The `ENGINE` slot, borrowed mutably. The [`MainThread`] argument is what confines it: this
/// hands out a `&'static mut` to a `static mut`, so a second caller on another thread is instant
/// UB, and `static mut` carries no `Sync` bound to stop one (verified by compiling the
/// counterexample — see `docs/async-model-decision.md`). The other two touches of `ENGINE` are
/// `start_bufferfeed` and `teardown`, in this file, which take the token for the same reason.
#[inline]
pub(crate) fn engine(_: &MainThread) -> Option<&'static mut Engine> {
    unsafe { (*std::ptr::addr_of_mut!(ENGINE)).as_mut() }
}
/// Is a session live? Distinct from [`engine`] because it answers without handing out the
/// `&'static mut` — `start_bufferfeed`'s double-start guard only needs to ask.
#[inline]
fn engine_is_live(_: &MainThread) -> bool {
    unsafe { (*std::ptr::addr_of!(ENGINE)).is_some() }
}
/// Install the freshly-built session. Overwriting a live slot drops an Engine whose workers hold
/// raw pointers into the boxes it owns — `start_bufferfeed` guards on [`engine_is_live`] first.
#[inline]
fn engine_install(_: &MainThread, e: Engine) {
    unsafe { *std::ptr::addr_of_mut!(ENGINE) = Some(e) }
}
/// Take the session out of the slot; the caller then joins its workers and drops it.
#[inline]
fn engine_take(_: &MainThread) -> Option<Engine> {
    unsafe { (*std::ptr::addr_of_mut!(ENGINE)).take() }
}

/// Bind the decoded video sink to the display plane, whichever way this television does it.
///
/// **Boot-scoped.** Called once, from `plex_run`, and never again — which is why the webOS 5
/// exported window is NOT created here: that one is created per session in [`start_bufferfeed`],
/// beside the payload it has to be spliced into, and destroyed by the matching `teardown`.
///
///   - `VP_ACB` — create + initialize the ACB. We deliberately DON'T register our own
///     com.webos.media client; it collides with the pipeline's uMS connection. The handle lives
///     for the process; what teardown calls is `acb_unload`, a session-scoped state change.
///   - `VP_EXPORTED` — nothing to do at boot. There is no handle, and no state mirroring:
///     webOS 5 deleted that sequence outright rather than replacing it.
pub(crate) fn acb_init(mt: &MainThread) {
    match ffi::vp_mode() {
        ffi::VP_EXPORTED => {
            log("vplane: exported window (webOS 5+) — created per session, UNTESTED path");
            // Nothing to bind later; the pump's ACB stages are skipped by ACB_OK staying false.
            ACB_OK.store(false, Ordering::Relaxed);
        }
        ffi::VP_NONE => {
            log("vplane: this device has NEITHER libAcbAPI nor SDL's exported window — audio \
                 will play and the picture will not appear. Please report your webOS version.");
            ACB_OK.store(false, Ordering::Relaxed);
        }
        _ => acb_init_acb(mt),
    }
}

/// The webOS 4.x half of [`acb_init`].
fn acb_init_acb(mt: &MainThread) {
    if let Some(s) = crate::dev::read("ptype") {
        if let Ok(p) = s.parse::<c_int>() {
            PTYPE.store(p, Ordering::Relaxed);
        }
    }
    let pt = PTYPE.load(Ordering::Relaxed);
    log(&format!("ptype={pt}"));
    let app_c = std::env::var("APPID").ok().and_then(|s| std::ffi::CString::new(s).ok());
    let app_ptr = app_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let acb = unsafe { ffi::acb_create(mt, app_ptr, pt) };
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
    let mut p = PAYLOAD_AV
        .replace(r#""video":"H264""#, &format!(r#""video":"{video}""#))
        .replace(r#""audio":"AC3""#, &format!(r#""audio":"{audio}""#))
        .replace(r#""maxWidth":1920"#, &format!(r#""maxWidth":{mw}"#))
        .replace(r#""maxHeight":1080"#, &format!(r#""maxHeight":{mh}"#))
        .replace(r#""maxFrameRate":30"#, r#""maxFrameRate":60"#);
    // Real source frame rate (direct-play only; 0 on transcode → skip): give the pipeline the true
    // fps for A/V timing instead of the sink-envelope default, + adaptiveResolution so it adapts if
    // the coded dims change. libpf parses videoFpsValue/videoFpsScale/adaptiveResolution (verified).
    if let Some((num, den)) = fps_rational(crate::route::stream_fps()) {
        p = p
            .replace(
                r#""seperatedPTS":true}"#,
                &format!(r#""seperatedPTS":true,"videoFpsValue":{num},"videoFpsScale":{den}}}"#),
            )
            .replace(r#""audioOnly":false"#, r#""audioOnly":false,"adaptiveResolution":true"#);
        log(&format!("esInfo: videoFps {num}/{den} + adaptiveResolution (src {:.3})", crate::route::stream_fps()));
    }
    p
}

/// Create the exported window and splice its id into the Load payload — the webOS 5+ binding, in
/// one place.
///
/// **Session-scoped, and that is the point.** The window is created HERE rather than at boot
/// because `teardown` destroys it, and the two ends have to sit at the same level: created once at
/// boot and destroyed per session, the second play would splice nothing and show no picture, in
/// silence. Creating it beside its only consumer also makes the "must exist before Load" ordering
/// locally visible instead of a promise made across two files.
///
/// On webOS 5 this one string IS the video-plane binding: the compositor assigned it to the window
/// we just created, and the pipeline imports that window by name and punches through to it.
/// Everything else about the BUFFERSTREAM payload is unchanged between the eras — which is what
/// both reference implementations show (ss4s compiles the same payload builder for webOS 3, 4 and
/// 5 and swaps only the resource file; Kodi adds this key and nothing else).
///
/// Inserted after `"appId":"…"` because that is a stable sibling inside `option` in every payload
/// variant here. A no-op on every webOS 4.x set, where `vp_create_window` returns NULL.
fn with_window_id(mt: &MainThread, p: &str) -> String {
    if ffi::vp_mode() != ffi::VP_EXPORTED {
        return p.to_string();
    }
    let id = unsafe { ffi::vp_create_window(mt) };
    if id.is_null() {
        log("windowId: no exported window — video will not bind");
        return p.to_string();
    }
    let id = unsafe { std::ffi::CStr::from_ptr(id) }.to_string_lossy();
    let anchor = r#""appId":"com.beb.plxnative""#;
    if !p.contains(anchor) {
        log("windowId: payload has no appId anchor — NOT spliced; video will not bind");
        return p.to_string();
    }
    log(&format!("vplane: exported windowId={id} spliced into the Load payload"));
    p.replace(anchor, &format!(r#"{anchor},"windowId":"{id}""#))
}

/// Plex decimal fps → (value, scale) rational for the Load esInfo. Broadcast rates map to their
/// exact NTSC/film ratios; integer rates to n/1; anything else to milli-fps. None if fps is unknown.
fn fps_rational(fps: f64) -> Option<(i64, i64)> {
    if fps <= 0.0 {
        return None;
    }
    let near = |a: f64, tol: f64| (fps - a).abs() < tol;
    Some(if near(23.976, 0.01) {
        (24000, 1001)
    } else if near(29.97, 0.01) {
        (30000, 1001)
    } else if near(59.94, 0.02) {
        (60000, 1001)
    } else if near(47.952, 0.02) {
        (48000, 1001)
    } else if fps.fract().abs() < 0.001 {
        (fps.round() as i64, 1)
    } else {
        ((fps * 1000.0).round() as i64, 1000)
    })
}

pub(crate) fn start_bufferfeed(mt: &MainThread) -> bool {
    // Guard a double-start: overwriting a live ENGINE slot would DROP the running
    // Engine, detaching its worker threads and freeing the hs/aq boxes those
    // threads still hold raw ptrs into -> use-after-free. If already running, no-op.
    // (Reachable via a PLAY key landing in the WILL->DID foreground window.)
    if engine_is_live(mt) {
        log("start_bufferfeed: already running (no-op)");
        return true;
    }
    // resolve the URL: route (a selected movie) wins, then /tmp/plxnative-url, then a local sample.
    let mut url = crate::route::url();
    if url.is_empty() {
        if let Some(t) = crate::dev::read("url") {
            if !t.is_empty() {
                url = t;
                crate::route::set_url(&url);
            }
        }
    }
    let mut sample: Option<Box<SampleBuf>> = None;
    let mut is_h265 = false;
    if url.is_empty() {
        if let Some(data) = crate::dev::read_bytes_at("/tmp/sample.h264") {
            let au = bf_split(&data, 0x09);
            log(&format!("bf_split h264: {} AUs in {} bytes", au.len(), data.len()));
            if au.len() < 2 {
                return false;
            }
            sample = Some(Box::new(SampleBuf { data, au, next: 0, loops: 0 }));
        } else if let Some(data) = crate::dev::read_bytes_at("/tmp/sample.h265") {
            // Phase 0 probe: feed a local HEVC Annex-B sample to test native HEVC decode.
            let au = bf_split(&data, 0x46);
            log(&format!("bf_split h265: {} AUs in {} bytes", au.len(), data.len()));
            if au.len() < 2 {
                return false;
            }
            is_h265 = true;
            sample = Some(Box::new(SampleBuf { data, au, next: 0, loops: 0 }));
        } else {
            // nothing to play: no selected item, no /tmp/plxnative-url, no local sample. (The old
            // baked-in demo-movie fallback is gone — the binary carries no URLs/credentials.)
            log("start_bufferfeed: no URL — select an item (or set /tmp/plxnative-url)");
            return false;
        }
    }
    let stream = sample.is_none();
    // For a streamed direct-play/transcode, pick the Load codecs from the item: video H264 vs
    // H265 (native HEVC direct-play), audio AC3/EAC3/AAC. (The local sample paths keep their
    // fixed payloads.)
    // dev A/B: /tmp/plxnative-noaudio feeds video only (needAudio:false + skip es=2) to isolate
    // whether the audio ES (E-AC3/Atmos) is what stalls the sink on 4K HEVC.
    let no_audio = crate::dev::flag("noaudio");
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
            // Sink envelope = the panel max (4K) regardless of codec; the pipeline reads the
            // true dims from the bitstream (SPS), so this is just a ceiling and is correct for a
            // 4K stream (HEVC transcode / HEVC direct-play) AND harmless for a 1080p H264 file.
            let (mw, mh) = (3840, 2160);
            stream_payload = build_av_payload(vc, ac, mw, mh);
            &stream_payload
        }
    } else if is_h265 {
        PAYLOAD_H265
    } else {
        PAYLOAD_V
    };
    // The windowId splice happens HERE, at the single point every payload variant passes through,
    // rather than inside build_av_payload — three of the four variants are static strings that
    // never see the builder, and a binding that works only for streamed A/V would be the kind of
    // bug that reproduces on some content and not others.
    let payload_c = std::ffi::CString::new(with_window_id(mt, payload_str)).unwrap();

    // fd = -1 (CLOSED) so a teardown before/without http_open doesn't close(0)
    let mut hs = crate::stream::http_stream_boxed();
    let mut aqv_box: Option<Box<AuQueue>> = None;
    let mut aqa_box: Option<Box<AuQueue>> = None;
    let mut stream_th = None;
    let source;

    if stream {
        let su = crate::plex::StreamUrl::parse(&url); // the typed layer's URL splitter
        let (host, port, path) = (su.host, su.port, su.path);
        log(&format!("stream: host={host} port={port} path={}", &path[..path.len().min(80)]));
        // Two-lane feed: the demuxer routes es=1 video to aq_video and es=2 audio to
        // aq_audio, each with its own cap + feeder.
        let mut qv = crate::aq::aq_new(AQ_VIDEO_BYTES);
        let mut qa = crate::aq::aq_new(AQ_AUDIO_BYTES);
        let aqv_raw = &mut *qv as *mut AuQueue;
        let aqa_raw = &mut *qa as *mut AuQueue;
        let hs_raw = &mut *hs as *mut HttpStream;
        SHARED.hs_ptr.store(hs_raw, Ordering::Release);
        {
            let aqp = threads::SendPtr(aqv_raw);
            let aqap = threads::SendPtr(aqa_raw);
            let hsp = threads::SendPtr(hs_raw);
            // The Load payload's audio codec, captured BY VALUE here on the main thread.
            // `ff::demux` used to call `route::stream_acodec()` from the demux thread, cloning
            // a `static mut String` that the main thread reassigns (`route::set_stream_codecs`
            // at route.rs:401/424/426/580, `player::request_audio_track` at mod.rs:92) — a data
            // race (writers: `route::set_stream_codecs`, `player::request_audio_track`), and a
            // use-after-free if the reassignment dropped the old buffer mid-clone.
            // Capturing is free: every one of those writers is followed by
            // `teardown(true) + start_bufferfeed()` (reload_at / reload_transcode /
            // switch_audio_native), which respawns this thread with the new value.
            let acodec = crate::route::stream_acodec();
            stream_th = crate::task::spawn("demux", move || crate::ff::demux(host, port, path, acodec, aqp, aqap, hsp));
            if stream_th.is_none() {
                // Nothing will ever fill the AU queues, so there is no session to start. `hs` is
                // about to drop with this early return, so retract the pointer first — the pump
                // and teardown both read it straight off SHARED.
                SHARED.hs_ptr.store(std::ptr::null_mut(), Ordering::Release);
                return false;
            }
        }
        aqv_box = Some(qv);
        aqa_box = Some(qa);
        source = Source::Stream;
    } else {
        source = Source::Sample(sample.unwrap());
    }

    // the media thread constructs + loads + runs the loop (owns the GMainContext)
    let payload_ptr = threads::SendPtr(payload_c.as_ptr() as *mut c_char);
    let load_th = crate::task::spawn("media", move || threads::load_thread(payload_ptr));
    if load_th.is_none() {
        // Without the media thread nothing is ever Loaded and nothing drains the AU queues, so the
        // demuxer would park in aq_push forever holding raw pointers into locals this return is
        // about to drop. Stop it exactly the way teardown's stream branch does — abort both lanes,
        // shutdown to wake the recv, join, and only then the real close.
        for q in [aqv_box.as_mut(), aqa_box.as_mut()].into_iter().flatten() {
            crate::aq::aq_abort(&mut **q);
        }
        let p = SHARED.hs_ptr.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if !p.is_null() {
            crate::stream::http_shutdown(p);
        }
        if let Some(t) = stream_th.take() {
            crate::task::join("demux", t);
        }
        if !p.is_null() {
            crate::stream::http_close(p); // sole owner now: the reader is joined
        }
        return false;
    }

    // progress reporter: post the play position to /:/timeline (updates resume + watched).
    // rk is captured now (fixed for the session); skipped for the sample/demo (no rk).
    let report_stop = threads::ReportStop::new();
    let report_th = if stream {
        let rk = crate::route::cur_rk();
        if rk.is_empty() {
            None
        } else {
            // best-effort: refused, the only loss is that the resume point stops being posted
            let st = report_stop.clone();
            crate::task::spawn("timeline", move || threads::timeline_thread(rk, st))
        }
    } else {
        None
    };

    let eng = Engine {
        stage: Stage::Loading,
        video_info_sent: false,
        eos_pushed: false,
        // if a seek is armed for the FIRST open (resume, or reload_at), rebase the first
        // post-seek keyframe to fed-pts 0 so the pipeline sees a 0-based timeline identical
        // to fresh play (disp_base carries the content offset). Plain fresh play leaves this
        // false (first keyframe is already ~0).
        rebase_pending: SHARED.seek_to_ns.load(Ordering::Relaxed) >= 0,
        rebase_drops: 0,
        seek_armed_at: 0,
        seek_retries: 0,
        flushed: false,
        max_fed_video_pts: 0,
        max_fed_audio_pts: 0,
        seek_base_pts: 0,
        prime_play: false,
        aq_video: aqv_box,
        aq_audio: aqa_box,
        hs,
        pending_video: None,
        pending_audio: None,
        payload: payload_c,
        source,
        stream_th,
        load_th,
        report_th,
        report_stop: Some(report_stop),
    };
    // Re-arm the in-place-seek probe for this session. `feed_stream` clears it when
    // `sf_send_segment` can't reach the pipeline, and that is a fact about the StarfishMediaAPIs
    // object — which `sf_load` constructs and teardown's `sf_destroy` destructs, once per session
    // — not about the TV (see the SCOPE note on INPLACE_SEEK_OK in mod.rs). Left latched for the
    // process, one teardown-window race silently turned every subsequent seek, of every
    // subsequent item, into a full reload for the rest of the app's life. Placed HERE rather than
    // at the top of this function on purpose: the `engine_is_live` no-op return above must NOT
    // re-arm, or a stray PLAY landing mid-session would cancel a fallback that is still needed.
    super::INPLACE_SEEK_OK.store(true, Ordering::Relaxed);
    engine_install(mt, eng);
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

/// Resume/seek AT the first Load. A direct-play item seeks the demuxer (av_seek via arm_seek).
/// A TRANSCODE item's stream is 0-based and NOT seekable (no byte-index, Content-Length=-1), so
/// av_seek fails — instead restart the encode at `&offset=secs` (transcode_seek) and display
/// content time via disp_base. Call BEFORE start_bufferfeed, AFTER route::play_movie has run the
/// decision (so the transcode session + flavor are set). Used for viewOffset resume.
pub(crate) fn resume_at(resume_ns: i64) {
    if resume_ns <= 0 {
        return;
    }
    if !crate::route::is_transcoding() {
        arm_seek(resume_ns); // direct-play: av_seek the file at the first open
    } else if crate::route::transcode_seek(resume_ns / 1_000_000_000).is_some() {
        // transcode: the encode restarts at &offset (0-based); disp_base carries the offset
        SHARED.disp_base.store(resume_ns, Ordering::Relaxed);
        SHARED.playpos_ns.store(resume_ns, Ordering::Relaxed);
        log(&format!("resume(transcode): restart at offset {}s", resume_ns / 1_000_000_000));
    }
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
pub(crate) fn reload_at(mt: &MainThread, target_ns: i64) {
    if crate::route::url().is_empty() {
        log("reload_at: no url (ignored)");
        return;
    }
    log(&format!("reload_at: fresh Load at {}s", target_ns / 1_000_000_000));
    teardown(mt, true); // reload mode: preserve the session (no url-clear / stop-scrobble)
    arm_seek(target_ns);
    start_bufferfeed(mt);
}

/// NATIVE audio-track switch (direct-play, NO transcode): select the Nth audio stream from the
/// same MKV and reload the direct-play pipeline at the current position (route::stream_acodec
/// was already set to the chosen track's codec, so the fresh Load configures the right audio
/// decoder). desired_audio_idx persists across the reload, so the demuxer keeps feeding the
/// chosen stream and the choice survives later seeks.
pub(crate) fn switch_audio_native(mt: &MainThread, audio_idx: i32, pos_ns: i64) {
    SHARED.desired_audio_idx.store(audio_idx, Ordering::Relaxed);
    log(&format!("switch_audio_native: audio_idx={audio_idx} at {}s", pos_ns / 1_000_000_000));
    reload_at(mt, pos_ns); // fresh direct-play Load at the current position, new audio stream
}

/// Reload the pipeline for a MODE/CODEC change — an audio-track switch on a direct-play HEVC
/// item forces a transcode (H264/AC3), so the pipeline must be re-Loaded with the H264 payload
/// (feeding H264 into the H265-configured pipeline stalls). Unlike reload_at, the transcode
/// start.mkv is already 0-based at `&offset`, so no av_seek — just set disp_base to the offset.
/// route::retranscode has already set the URL + session + STREAM_VCODEC=h264 before this call.
pub(crate) fn reload_transcode(mt: &MainThread, offset_ns: i64) {
    if crate::route::url().is_empty() {
        log("reload_transcode: no url (ignored)");
        return;
    }
    log(&format!("reload_transcode: fresh Load at offset {}s", offset_ns / 1_000_000_000));
    teardown(mt, true); // keep the session; reload mode
    SHARED.disp_base.store(offset_ns, Ordering::Relaxed); // transcode is 0-based at content=offset
    SHARED.playpos_ns.store(offset_ns, Ordering::Relaxed);
    start_bufferfeed(mt);
}

/// Stop playback: unblock+join threads, unload+destruct the pipeline, release the
/// video plane, reset all state so a fresh start_bufferfeed() can restart.
pub(crate) fn stop_bufferfeed(mt: &MainThread) {
    teardown(mt, false);
}

/// Suspend for an app-switch: tear the pipeline down (webOS reclaims the video plane while we're
/// backgrounded) but PRESERVE the playback session — keep the URL + transcode session, and
/// don't scrobble "stopped". This makes the foreground restore a clean same-item reload
/// (resume_at + start_bufferfeed), the known-good path, instead of resurrecting a stopped session.
pub(crate) fn suspend_bufferfeed(mt: &MainThread) {
    teardown(mt, true);
}

/// The teardown body. `for_reload` = this is a direct-play seek reload (reload_at), NOT a real
/// stop: preserve the playback session so start_bufferfeed can restart the SAME item — skip
/// the "stopped" timeline scrobble, the server transcode stop, and the URL clear.
fn teardown(mt: &MainThread, for_reload: bool) {
    let mut eng = match engine_take(mt) {
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
            Some((rk, SHARED.playpos_ns.load(Ordering::Relaxed) / 1_000_000, dur / 1_000_000))
        } else {
            None
        }
    };

    // 1. unblock every thread (abort queues, close the demux socket)
    if let Some(st) = eng.report_stop.take() {
        st.stop(); // this reporter's own signal — unaffected by the reset_session below
    }
    if stream {
        // abort BOTH lanes: unblock the demux if it's parked in aq_push on a full lane
        for q in [eng.aq_video.as_mut(), eng.aq_audio.as_mut()].into_iter().flatten() {
            crate::aq::aq_abort(&mut **q);
        }
        let p = SHARED.hs_ptr.load(Ordering::Acquire);
        if !p.is_null() {
            // shutdown, not close: the demux thread is still inside recv here and is joined
            // below. Closing now would free the fd number for another thread to claim while
            // this one is still reading it. The real close happens after the join.
            crate::stream::http_shutdown(p);
        }
    }
    // 2. JOIN every worker before freeing anything they hold raw ptrs into
    // Through `task::join` so a stall leaves a number behind: this is the main thread, and every
    // teardown freeze the engine has had was one of these three.
    if let Some(t) = eng.stream_th.take() {
        crate::task::join("demux", t);
    }
    if let Some(t) = eng.load_th.take() {
        crate::task::join("media", t);
    }
    // 2b. every reader is now joined, so this thread is the sole owner: do the real close.
    // (Before the join it could only shutdown — see step 1.)
    if stream {
        let p = SHARED.hs_ptr.load(Ordering::Acquire);
        if !p.is_null() {
            crate::stream::http_close(p);
        }
    }
    // Final position report (state=stopped, so the server commits the resume point) + the
    // server-side transcode stop, both dispatched to a worker. They used to run inline HERE and
    // twelve lines below — two blocking PMS round trips on the SDL thread, ~17 s each worst case,
    // on 100% of real stops. `scrobble_stop` reads and clears route's session statics on THIS
    // thread and hands the worker owned copies; `plex_run` drains it at exit so the report still
    // lands. Skipped for a reload, which is not a stop.
    // The reporter is NOT joined here. Its handle rides out with the scrobble worker, which
    // joins it before posting `stopped` — so the last `playing` report still lands first, but the
    // MAIN thread stops paying for it. It parked the frame loop for the remainder of SO_RCVTIMEO
    // whenever that POST had stalled (measured: `THREADJOIN timeline 6974ms`, tools/netcond.py in
    // `stall@/:/timeline`). Safe to outlive the Engine: the reporter names no Engine field, only
    // SHARED atomics and its own owned `rk`.
    if !for_reload {
        crate::route::scrobble_stop(final_report, eng.report_th.take());
    }
    // 3. unload + destruct the pipeline, release the plane. (Kodi waits for UNLOADCOMPLETED before
    // destructing, but on webOS 4.5 that event arrives as smp_cb type=23 with no detectable string,
    // SAM force-kills the app during a real stop anyway, and reload — which reconstructs g_smp per
    // seek — has shown no race with immediate destroy across the full suite. So no blocking wait.)
    if unsafe { ffi::sf_ready(mt) } != 0 {
        unsafe { ffi::sf_unload(mt) };
        if ACB_OK.load(Ordering::Relaxed) {
            unsafe { ffi::acb_unload(mt) };
        }
        unsafe { ffi::sf_destroy(mt) };
    }
    // The webOS 5+ counterpart of acb_unload, and the other half of `with_window_id`'s create.
    // Outside the sf_ready guard because the window is created BEFORE Load — a session that failed
    // between the two would otherwise leak it, and the next Load would ask for another. Unguarded
    // because the seam already no-ops in the other modes (see starfish.h).
    unsafe { ffi::vp_destroy_window(mt) };
    // 4. drain + destroy both queues (drain_aq also clears both pendings)
    if stream {
        drain_aq(&mut eng);
        for q in [eng.aq_video.as_mut(), eng.aq_audio.as_mut()].into_iter().flatten() {
            crate::aq::aq_destroy(&mut **q);
        }
    }
    // 5. reset shared + transport. On a real stop also stop the server transcode + clear the
    // URL; on a reload KEEP them so start_bufferfeed restarts the same item (a direct-play
    // reload has no transcode session anyway, so the skip only matters for the URL).
    SHARED.reset_session();
    TX.reset();
    if !for_reload {
        crate::route::clear_url(); // the transcode stop rode out with `scrobble_stop` above
    } else if let Some(t) = eng.report_th.take() {
        // A reload posts no `stopped`, so there is nothing to order against: detach. Its own
        // ReportStop is already set, so it exits after its current POST and cannot be revived by
        // the next session — which is exactly what the shared flag could not guarantee.
        drop(t);
    }
    log("stop_bufferfeed: torn down");
    // Engine (hs/aq boxes, payload) drops here — after all joins
}

/// free every queued AU + the held pending one, BOTH lanes (seek + teardown).
pub(crate) fn drain_aq(eng: &mut Engine) {
    drain_one(eng.aq_video.as_mut());
    drain_one(eng.aq_audio.as_mut());
    eng.pending_video = None;
    eng.pending_audio = None;
}

fn drain_one(q: Option<&mut Box<AuQueue>>) {
    if let Some(q) = q {
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
}

/// feed streamed AUs from the demux queue; hold the current AU across ticks on
/// BufferFull (backpressure); zero-base the fed timeline on the first post-seek
/// keyframe; drop stale AUs past the B-frame reorder distance.
/// prime-then-play buffer depth: how much of the post-seek stream to buffer (paused) before
/// starting the clock. Enough to cover the pipeline's decode latency so the first frame is ready.
const PRIME_NS: i64 = 700_000_000;
// Prime the AUDIO lane too before starting the (audioSync master) clock — else a rapid-seek drain
// can start Play on an empty audio queue and leave audio silent until the next seek. Fallback:
// start anyway once video buffers PRIME_VIDEO_MAX_NS without audio (audioless / briefly starved),
// so a genuinely audioless region can't hang.
const PRIME_AUDIO_NS: i64 = 300_000_000;
const PRIME_VIDEO_MAX_NS: i64 = 2_500_000_000;
// Feed-ahead throttle (Kodi-parity): keep the VIDEO lane at most this far ahead of the presented
// position (SHARED.pres_fed) instead of feeding greedily to BufferFull. Bounding the buffer to
// ~1.6s (was ~10-20s: aq 6MB + the pipeline's own ~8MB) makes seeks flush far less, keeps the
// clock from running ahead, and cuts latency. AUDIO gets a looser bound so it can ride slightly
// ahead (audio buffer is cheap and it's the master clock) without unbounded race on odd muxes.
const MAX_FEED_AHEAD_NS: i64 = 1_600_000_000;
const AUDIO_SLACK_NS: i64 = 2_000_000_000;
// A fed pts this far below a lane's high-water is a stale pre-seek AU (past the B-frame reorder
// distance) → drop it rather than feed a backward jump.
const STALE_BACKJUMP_NS: i64 = 2_000_000_000;
// In-place-seek rebase guard: a first-keyframe this far from the seek target is a stale pre-reopen
// frame from the drifted read position, not the real post-seek keyframe. av_seek(BACKWARD) lands
// AT/BEFORE the target, so a valid anchor is never ahead and is at most ~one GOP behind — hence a
// tight AHEAD bound and a looser BEHIND bound (to tolerate sparse keyframes). Capped by MAX_REBASE_DROPS.
const SEEK_STALE_AHEAD_NS: i64 = 6_000_000_000;
const SEEK_STALE_BEHIND_NS: i64 = 30_000_000_000;
const MAX_REBASE_DROPS: i32 = 240;
// Audio counterpart of the rebase guard: an audio AU this far AHEAD of the video high-water is a
// stale drifted frame (the demuxer's pre-reopen audio after a seek). Feeding it poisons
// max_fed_audio_pts (and then the backjump guard drops all LEGIT audio until the demux catches
// up to the poisoned mark → seconds of silence). Legit audio lead is bounded by the throttle to
// ~MAX_FEED_AHEAD+AUDIO_SLACK (3.6s), so 5s has margin. It was 15s, and a stuck-retry seek fed
// stale audio 14.9s ahead — 13s of user-audible silence after a rapid 10s-back tap burst.
const AUDIO_STALE_AHEAD_NS: i64 = 5_000_000_000;
// Sentinel for SHARED.pres_fed meaning "no post-seek frame has presented yet" — the feed-ahead
// throttle treats it as feed-freely (don't compare the new fed pts against a stale pre-seek
// presented position). Set on a seek; the first presented frame overwrites it with a real pts.
pub(crate) const PRES_NONE: i64 = i64::MIN;

/// VIDEO lane feeder (aq_video is video-only). Owns the seek rebase + in-place-seek handshake + prime→Play, all of
/// which key off the first post-seek VIDEO keyframe. A BufferFull/over-budget breaks THIS lane
/// only — the audio lane (feed_audio_lane) keeps flowing so the audioSync master clock advances.
pub(crate) fn feed_stream(mt: &MainThread, eng: &mut Engine) {
    let qp = match eng.aq_video.as_mut() {
        Some(q) => &mut **q as *mut AuQueue,
        None => return,
    };
    let mut fed = 0;
    while fed < 120 {
        // Feed each AU, throttled to ~MAX_FEED_AHEAD_NS ahead of the presented position per lane
        // (see the throttle below) rather than greedily to BufferFull.
        if eng.pending_video.is_none() {
            let mut eof: c_int = 0;
            let n = crate::aq::aq_pop(qp, &mut eof);
            if n.is_null() {
                // true EOF (producer done + video lane drained): signal end-of-stream ONCE so the
                // pipeline drains its last frames instead of hanging on them (Kodi keys EOS to the
                // video drain). Keyed on the video lane only.
                if eof != 0 && !eng.eos_pushed && eng.stage >= Stage::Streaming {
                    unsafe { ffi::sf_push_eos(mt) };
                    eng.eos_pushed = true;
                    log("EOS pushed at true EOF");
                }
                break;
            }
            eng.pending_video = Some(AuBox(n));
        }
        let n = eng.pending_video.as_ref().unwrap().0;
        let (es, key, pts, len, data) = unsafe { crate::aq::au_fields(n) };
        if eng.rebase_pending {
            if es == 1 && key != 0 {
                // In-place seek: drop a keyframe that landed well AHEAD of the target — it's a stale
                // frame from the pre-flush read position (the reopen+av_seek hasn't taken effect yet),
                // not the real post-seek keyframe. Capped so a failed av_seek can't hang the rebase.
                if eng.flushed {
                    let target = SHARED.seek_target_ns.load(Ordering::Relaxed);
                    let stale = target >= 0
                        && (pts - target > SEEK_STALE_AHEAD_NS || target - pts > SEEK_STALE_BEHIND_NS);
                    if stale && eng.rebase_drops < MAX_REBASE_DROPS {
                        eng.rebase_drops += 1;
                        if eng.rebase_drops == 1 {
                            log(&format!("rebase: dropping stale kf pts={}s (target {}s)",
                                pts / 1_000_000_000, target / 1_000_000_000));
                        }
                        eng.pending_video = None;
                        continue;
                    }
                }
                if eng.flushed {
                    // Kodi IN-PLACE seek (exact): feed the REAL content PTS (no rebase), tell the
                    // pipeline the real decode position, then inject a fresh GStreamer SEGMENT —
                    // this re-anchors the sink WITHOUT a reload/decoder re-init. disp_base=0 +
                    // pts_shift=0 → playpos = presented real pts = content time.
                    SHARED.pts_shift.store(0, Ordering::Relaxed);
                    let ok = unsafe { ffi::sf_set_time_to_decode(mt, pts) };
                    // setTimeToDecode returns 0 on webOS<11 (it needs PausedState); fall back to
                    // the content-info path (loadSpi_getInfo + setContentInfo(ptsToDecode)), which
                    // re-anchors the decode position while Playing. Then always inject the fresh
                    // GStreamer SEGMENT so the sink re-bases instead of stalling.
                    let ci = if ok == 0 { unsafe { ffi::sf_set_content_info(mt, pts) } } else { 1 };
                    let seg = unsafe { ffi::sf_send_segment(mt) };
                    log(&format!("in-place seek: setTimeToDecode({pts}) rv={ok} setContentInfo={ci} sendSegment={seg}"));
                    if seg == 0 {
                        // The pipeline ptr wasn't reachable, so NO fresh GStreamer segment was
                        // injected and THIS seek's buffers are scheduled against the pre-seek
                        // segment — the stale-segment state that stops the sink draining and
                        // fills ~14.7 MB of upstream buffers in ~48 s (permanent BufferFull +
                        // "Playing error"; see reload_at's decompiled ground truth). So drop the
                        // rest of THIS SESSION's seeks to the reload path, which rebuilds the
                        // segment by construction; start_bufferfeed re-arms the flag for the next
                        // session (the SCOPE note in mod.rs argues why that is the right scope).
                        // Logged loudly because this is the single most consequential state
                        // change in the seek path and it used to be invisible: instant seeks
                        // silently became multi-second reloads with nothing in the event log
                        // naming the cause — only the `sendSegment=0` above, whose consequence
                        // was not stated anywhere.
                        super::INPLACE_SEEK_OK.store(false, Ordering::Relaxed);
                        log("in-place seek DOWNGRADE: sendSegment=0 (CustomPipeline unreachable) \
                             -> reload-per-seek for the rest of this session");
                    }
                    eng.flushed = false;
                } else {
                    // reload / initial-resume seek: rebase the landed keyframe to fed-pts 0 (the
                    // fresh Load's pipeline expects a 0-based feed; disp_base carries the offset).
                    SHARED.pts_shift.store(-pts, Ordering::Relaxed);
                }
                eng.rebase_pending = false; // releases the AUDIO lane (which holds until this clears)
                eng.seek_base_pts = pts + SHARED.pts_shift.load(Ordering::Relaxed); // fed-pts base
                log(&format!("rebase: first post-seek keyframe pts={pts} -> pts_shift={}",
                    SHARED.pts_shift.load(Ordering::Relaxed)));
            } else {
                eng.pending_video = None; // drop pre-keyframe AUs
                continue;
            }
        }
        let mut fp = pts + SHARED.pts_shift.load(Ordering::Relaxed);
        if fp < eng.max_fed_video_pts - STALE_BACKJUMP_NS {
            eng.pending_video = None; // stale (a big backward jump)
            continue;
        }
        if fp < 0 {
            fp = 0;
        }
        // Feed-ahead throttle: don't feed an AU that's already more than its lane's budget ahead
        // of the presented position — keep it pending and retry once the pipeline presents more.
        // Skipped while priming (feed freely to reach PRIME_NS before Play). Each lane's queue is
        // pts-ordered, so if the head is over budget everything behind it is too; breaking is right.
        if !eng.prime_play {
            let pres = SHARED.pres_fed.load(Ordering::Relaxed);
            let budget = if es == 1 { MAX_FEED_AHEAD_NS } else { MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS };
            if pres != PRES_NONE && fp - pres > budget {
                break;
            }
        }
        let r = unsafe { ffi::sf_feed(mt, data, len as u32, fp, es) };
        if fp > eng.max_fed_video_pts {
            eng.max_fed_video_pts = fp;
        }
        // prime-then-play: once PRIME_NS of the fresh (post-seek/resume) stream is buffered,
        // start the clock. The pipeline was paused through the reopen gap, so it now presents
        // from the seek point in A/V sync instead of fast-forwarding to a clock that ran ahead.
        // Start the clock once BOTH lanes are buffered past the seek base: video to PRIME_NS AND
        // audio to PRIME_AUDIO_NS. Priming on video ALONE started the audioSync MASTER clock with
        // an empty audio queue, so a rapid-seek drain could leave audio silent until the next seek.
        // The video-buffer fallback still starts an audioless/briefly-starved stream (no hang).
        let vbuf = eng.max_fed_video_pts - eng.seek_base_pts;
        let abuf = eng.max_fed_audio_pts - eng.seek_base_pts;
        if eng.prime_play && vbuf >= PRIME_NS && (abuf >= PRIME_AUDIO_NS || vbuf >= PRIME_VIDEO_MAX_NS) {
            unsafe { ffi::sf_play(mt) };
            eng.prime_play = false;
            SHARED.seeking.store(false, Ordering::Relaxed); // playback resumed at the new position → HUD spinner off
            log(&format!("primed: v={}ms a={}ms -> Play", vbuf / 1_000_000, abuf / 1_000_000));
        }
        if es == 1 {
            let v = VTOT.fetch_add(1, Ordering::Relaxed) + 1;
            if v <= 4 || v % 100 == 0 {
                let qb = crate::aq::aq_bytes(qp);
                log(&format!("feed v#{v} sz={len} fed={fp} reply={} qbytes={qb}", r as u8 as char));
            }
        }
        if (r as u8) != b'O' {
            break; // 'B' BufferFull -> keep pending, retry next tick (VIDEO lane only)
        }
        eng.pending_video = None;
        fed += 1;
    }
}

/// AUDIO lane feeder (two-lane ff path only). Independent of the video lane: its own queue, its
/// own fed-pts high-water, its own BufferFull retry — so a video BufferFull never starves audio.
/// HOLDS while a seek rebase is pending (the VIDEO lane sets pts_shift on its first post-seek
/// keyframe; feeding audio before that would use a stale shift → A/V desync). No prime/Play here —
/// only the video lane starts the clock. Called AFTER feed_stream each tick, so a same-tick rebase
/// is already visible.
pub(crate) fn feed_audio_lane(mt: &MainThread, eng: &mut Engine) {
    if eng.rebase_pending {
        return; // wait for the video lane to publish pts_shift
    }
    let qp = match eng.aq_audio.as_mut() {
        Some(q) => &mut **q as *mut AuQueue,
        None => return,
    };
    // hoisted out of the loop: pts_shift is stable once rebase clears (only the video lane's rebase
    // arm writes it, on this same thread), and one pres_fed sample per tick is plenty against the
    // multi-second audio budget.
    let shift = SHARED.pts_shift.load(Ordering::Relaxed);
    let pres = SHARED.pres_fed.load(Ordering::Relaxed);
    let mut fed = 0;
    while fed < 120 {
        if eng.pending_audio.is_none() {
            let mut eof: c_int = 0;
            let n = crate::aq::aq_pop(qp, &mut eof);
            if n.is_null() {
                break;
            }
            eng.pending_audio = Some(AuBox(n));
        }
        let n = eng.pending_audio.as_ref().unwrap().0;
        let (es, _key, pts, len, data) = unsafe { crate::aq::au_fields(n) };
        let mut fp = pts + shift;
        // Stale drifted audio from before a seek's reopen: far AHEAD of the freshly-anchored video.
        // Drop it (else it poisons max_fed_audio_pts and stalls the audio clock → playback sticks).
        if eng.max_fed_video_pts > 0 && fp > eng.max_fed_video_pts + AUDIO_STALE_AHEAD_NS {
            eng.pending_audio = None;
            continue;
        }
        if fp < eng.max_fed_audio_pts - STALE_BACKJUMP_NS {
            eng.pending_audio = None; // stale (a big backward jump)
            continue;
        }
        if fp < 0 {
            fp = 0;
        }
        if !eng.prime_play && pres != PRES_NONE && fp - pres > MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS {
            break;
        }
        let r = unsafe { ffi::sf_feed(mt, data, len as u32, fp, es) };
        if fp > eng.max_fed_audio_pts {
            eng.max_fed_audio_pts = fp;
        }
        let a = ATOT.fetch_add(1, Ordering::Relaxed) + 1;
        if a <= 4 || a % 200 == 0 {
            let qb = crate::aq::aq_bytes(qp);
            log(&format!("feed a#{a} sz={len} fed={fp} reply={} qbytes={qb}", r as u8 as char));
        }
        if (r as u8) != b'O' {
            break; // 'B' BufferFull -> keep pending, retry next tick (AUDIO lane only)
        }
        eng.pending_audio = None;
        fed += 1;
    }
}

/// feed the looped /tmp/sample.h264 validation sample (continuous PTS @ 23.976).
pub(crate) fn feed_sample(mt: &MainThread, eng: &mut Engine) {
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
        let r = unsafe { ffi::sf_feed(mt, s.data[off..].as_ptr(), (end - off) as u32, pts, 1) };
        if (r as u8) != b'O' {
            break;
        }
        s.next += 1;
        fed += 1;
    }
}
