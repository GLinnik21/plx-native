//! FFmpeg FFI (libavformat / libavcodec / libavutil). The TV ships FFmpeg n3.3
//! (SONAMEs libavformat.so.57 / libavcodec.so.57 / libavutil.so.55 = 57.71.100 /
//! 57.89.100 / 55.58.100); we link stub `.so`s carrying those SONAMEs and the device
//! loads the real libraries at runtime — the same stub trick as SDL/GLES/Starfish.
//!
//! This module is the media demuxer that replaces the hand-rolled mkv.rs: robust
//! MKV/MP4/TS demux, HTTP input, and index-based seeking (av_seek_frame). See
//! docs/ffmpeg-demuxer-plan.md. Phase A here is the ABI verification probe: the
//! struct layouts below are the FFmpeg n3.3 ABI and MUST be confirmed on-device
//! (log codec_id/width/height for a known title) before the demuxer is built on them.
#![allow(dead_code)]
use crate::aq::AuQueue;
use crate::player::threads::SendPtr;
use crate::player::SHARED;
use crate::stream::HttpStream;
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uchar, c_uint, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

/// Bisect flag: /tmp/poc-demux=ff routes the demux thread through this libavformat
/// demuxer instead of mkv.rs, so both paths coexist during bring-up (Phases B–E).
static USE_FF: AtomicBool = AtomicBool::new(true);
pub(crate) fn use_ff() -> bool {
    USE_FF.load(Ordering::Relaxed)
}

/// Feed audio (es=2) to the pipeline. Cleared by the /tmp/poc-noaudio dev trigger to
/// A/B whether the audio ES (E-AC3/Atmos) is what stalls the sink on 4K HEVC.
static FEED_AUDIO: AtomicBool = AtomicBool::new(true);
pub(crate) fn set_feed_audio(on: bool) {
    FEED_AUDIO.store(on, Ordering::Relaxed);
}

// ---- opaque handles (pointer-only, never dereferenced) ----
pub enum AVClass {}
pub enum AVInputFormat {}
pub enum AVIOContext {}
pub enum AVDictionary {}
pub enum AVCodec {}
pub enum AVBitStreamFilter {}
pub enum AVBSFInternal {}
pub enum AVBufferRef {}
pub enum AVPacketSideData {}
// AVStream is opaque here: it is 712 bytes with a large "internal but ABI" block, so
// rather than transcribe every field we read only the three we need at their verified
// n3.3 offsets (index +0, time_base +40, codecpar +708). Phase A confirms the offsets.
pub enum AVStream {}
const OFF_STREAM_INDEX: usize = 0;
const OFF_STREAM_TIME_BASE: usize = 40;
const OFF_STREAM_CODECPAR: usize = 708;

// ---- structs (exact n3.3 field order; 32-bit ARM/AAPCS sizes) ----
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AVRational {
    pub num: c_int,
    pub den: c_int,
}

// sizeof = 72
#[repr(C)]
pub struct AVPacket {
    pub buf: *mut AVBufferRef,        // +0
    pub pts: i64,                     // +8
    pub dts: i64,                     // +16
    pub data: *mut u8,                // +24
    pub size: c_int,                  // +28
    pub stream_index: c_int,          // +32
    pub flags: c_int,                 // +36
    pub side_data: *mut AVPacketSideData, // +40
    pub side_data_elems: c_int,       // +44
    pub duration: i64,                // +48
    pub pos: i64,                     // +56
    pub convergence_duration: i64,    // +64  (FF_API_CONVERGENCE_DURATION)
}

// sizeof = 136
#[repr(C)]
pub struct AVCodecParameters {
    pub codec_type: c_int,   // +0
    pub codec_id: c_int,     // +4
    pub codec_tag: u32,      // +8
    pub extradata: *mut u8,  // +12
    pub extradata_size: c_int, // +16
    pub format: c_int,       // +20
    pub bit_rate: i64,       // +24
    pub bits_per_coded_sample: c_int, // +32
    pub bits_per_raw_sample: c_int,   // +36
    pub profile: c_int,      // +40
    pub level: c_int,        // +44
    pub width: c_int,        // +48
    pub height: c_int,       // +52
    pub sample_aspect_ratio: AVRational, // +56
    pub field_order: c_int,  // +64
    pub color_range: c_int,  // +68
    pub color_primaries: c_int, // +72
    pub color_trc: c_int,    // +76
    pub color_space: c_int,  // +80
    pub chroma_location: c_int, // +84
    pub video_delay: c_int,  // +88
    pub channel_layout: u64, // +96
    pub channels: c_int,     // +104
    pub sample_rate: c_int,  // +108
    pub block_align: c_int,  // +112
    pub frame_size: c_int,   // +116
    pub initial_padding: c_int, // +120
    pub trailing_padding: c_int, // +124
    pub seek_preroll: c_int, // +128
}

// AVFormatContext truncated after `duration` — we only ever hold a library-returned
// pointer and read leading fields. NEVER stack-allocate this.
#[repr(C)]
pub struct AVFormatContext {
    pub av_class: *const AVClass,      // +0
    pub iformat: *mut AVInputFormat,   // +4
    pub oformat: *mut c_void,          // +8
    pub priv_data: *mut c_void,        // +12
    pub pb: *mut AVIOContext,          // +16
    pub ctx_flags: c_int,              // +20
    pub nb_streams: c_uint,            // +24
    pub streams: *mut *mut AVStream,   // +28
    pub filename: [c_char; 1024],      // +32
    pub start_time: i64,               // +1056
    pub duration: i64,                 // +1064 (AV_TIME_BASE units)
    // ... more fields omitted; do not construct this struct.
}

// AVBSFContext head (through time_base_out) — we set par_in + time_base_in before init.
#[repr(C)]
pub struct AVBSFContext {
    pub av_class: *const AVClass,         // +0
    pub filter: *const AVBitStreamFilter, // +4
    pub internal: *mut AVBSFInternal,     // +8
    pub priv_data: *mut c_void,           // +12
    pub par_in: *mut AVCodecParameters,   // +16
    pub par_out: *mut AVCodecParameters,  // +20
    pub time_base_in: AVRational,         // +24
    pub time_base_out: AVRational,        // +32
}

// ---- externs ----
#[link(name = "avformat")]
extern "C" {
    fn av_register_all();
    fn avformat_network_init() -> c_int;
    fn avformat_alloc_context() -> *mut AVFormatContext;
    fn avformat_open_input(
        ps: *mut *mut AVFormatContext,
        url: *const c_char,
        fmt: *mut AVInputFormat,
        options: *mut *mut AVDictionary,
    ) -> c_int;
    fn avformat_find_stream_info(ic: *mut AVFormatContext, options: *mut *mut AVDictionary) -> c_int;
    fn av_find_best_stream(
        ic: *mut AVFormatContext,
        type_: c_int,
        wanted: c_int,
        related: c_int,
        decoder_ret: *mut *mut AVCodec,
        flags: c_int,
    ) -> c_int;
    fn av_read_frame(s: *mut AVFormatContext, pkt: *mut AVPacket) -> c_int;
    fn av_seek_frame(s: *mut AVFormatContext, stream_index: c_int, ts: i64, flags: c_int) -> c_int;
    fn avformat_close_input(s: *mut *mut AVFormatContext);
    fn avio_alloc_context(
        buffer: *mut u8,
        buffer_size: c_int,
        write_flag: c_int,
        opaque: *mut c_void,
        read_packet: Option<extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int>,
        write_packet: Option<extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int>,
        seek: Option<extern "C" fn(*mut c_void, i64, c_int) -> i64>,
    ) -> *mut AVIOContext;
}
#[link(name = "avcodec")]
extern "C" {
    fn avcodec_version() -> c_uint;
    fn av_packet_alloc() -> *mut AVPacket;
    fn av_packet_free(pkt: *mut *mut AVPacket);
    fn av_packet_unref(pkt: *mut AVPacket);
    fn avcodec_parameters_copy(dst: *mut AVCodecParameters, src: *const AVCodecParameters) -> c_int;
    fn avcodec_get_name(id: c_int) -> *const c_char;
    fn av_bsf_get_by_name(name: *const c_char) -> *const AVBitStreamFilter;
    fn av_bsf_alloc(f: *const AVBitStreamFilter, ctx: *mut *mut AVBSFContext) -> c_int;
    fn av_bsf_init(ctx: *mut AVBSFContext) -> c_int;
    fn av_bsf_send_packet(ctx: *mut AVBSFContext, pkt: *mut AVPacket) -> c_int;
    fn av_bsf_receive_packet(ctx: *mut AVBSFContext, pkt: *mut AVPacket) -> c_int;
    fn av_bsf_free(ctx: *mut *mut AVBSFContext);
}
#[link(name = "avutil")]
extern "C" {
    fn avutil_version() -> c_uint;
    fn avformat_version() -> c_uint;
    fn av_malloc(size: usize) -> *mut c_void;
    fn av_freep(ptr: *mut c_void);
    fn av_rescale_q(a: i64, bq: AVRational, cq: AVRational) -> i64;
}

// ---- constants (n3.3) ----
pub const AVMEDIA_TYPE_VIDEO: c_int = 0;
pub const AVMEDIA_TYPE_AUDIO: c_int = 1;
pub const AVMEDIA_TYPE_SUBTITLE: c_int = 3;
pub const AV_CODEC_ID_H264: c_int = 28; // FF_API_XVMC present (avutil<56); 4.x = 27
pub const AV_CODEC_ID_HEVC: c_int = 174; // 4.x = 173
pub const AV_CODEC_ID_AAC: c_int = 0x15002;
pub const AV_CODEC_ID_AC3: c_int = 0x15003;
pub const AV_CODEC_ID_EAC3: c_int = 0x15029;
pub const AV_PKT_FLAG_KEY: c_int = 0x0001;
pub const AVSEEK_FLAG_BACKWARD: c_int = 1;
pub const AVERROR_EOF: c_int = -541478725;
pub const AVERROR_EAGAIN: c_int = -11;
pub const AV_NOPTS_VALUE: i64 = i64::MIN;
pub const AV_TIME_BASE: i64 = 1_000_000;
pub const NS_TB: AVRational = AVRational { num: 1, den: 1_000_000_000 };

// ---- AVStream field accessors (offset-based; verified in Phase A) ----
#[inline]
unsafe fn stream_codecpar(s: *mut AVStream) -> *mut AVCodecParameters {
    *((s as *const u8).add(OFF_STREAM_CODECPAR) as *const *mut AVCodecParameters)
}
#[inline]
unsafe fn stream_time_base(s: *mut AVStream) -> AVRational {
    *((s as *const u8).add(OFF_STREAM_TIME_BASE) as *const AVRational)
}
#[inline]
unsafe fn stream_index(s: *mut AVStream) -> c_int {
    *((s as *const u8).add(OFF_STREAM_INDEX) as *const c_int)
}

/// The ffmpeg stream index of the first audio stream whose codec matches `want` (the Load
/// payload's audio codec, e.g. "ac3"), or None. `av_find_best_stream` picks the "highest
/// quality" audio (on an 8-track file it chose DTS over the AC3 default) — but the Load payload
/// carries Media[0].audioCodec, so feeding a different-codec track leaves the audio ES
/// unconfigured and, with audioSync, wedges the video (BufferFull forever). Matching the fed
/// track to the payload codec avoids that.
unsafe fn audio_stream_matching(fmt: *mut AVFormatContext, want: &str) -> Option<c_int> {
    if want.is_empty() {
        return None;
    }
    let streams = (*fmt).streams;
    for i in 0..(*fmt).nb_streams {
        let cp = stream_codecpar(*streams.add(i as usize));
        if (*cp).codec_type == AVMEDIA_TYPE_AUDIO {
            let name = std::ffi::CStr::from_ptr(avcodec_get_name((*cp).codec_id)).to_string_lossy();
            if name.eq_ignore_ascii_case(want) {
                return Some(i as c_int);
            }
        }
    }
    None
}

/// The ffmpeg stream index of the `n`-th audio stream in file order (for native audio-track
/// selection), or None if there are fewer than n+1 audio streams. metadata.audio is filtered
/// in the same file order, so the track menu's 0-based audio index maps 1:1 here.
unsafe fn nth_audio_stream(fmt: *mut AVFormatContext, n: i32) -> Option<c_int> {
    let streams = (*fmt).streams;
    let mut count = 0i32;
    for i in 0..(*fmt).nb_streams {
        let cp = stream_codecpar(*streams.add(i as usize));
        if (*cp).codec_type == AVMEDIA_TYPE_AUDIO {
            if count == n {
                return Some(i as c_int);
            }
            count += 1;
        }
    }
    None
}

static REGISTER: Once = Once::new();
fn ensure_registered() {
    REGISTER.call_once(|| unsafe {
        av_register_all();
        avformat_network_init();
    });
}

fn ver(v: c_uint) -> String {
    format!("{}.{}.{}", v >> 16, (v >> 8) & 0xff, v & 0xff)
}

/// Boot smoke test + optional ABI probe. Called once at startup.
pub(crate) fn boot() {
    unsafe {
        crate::player::log(&format!(
            "ff: avformat={} avcodec={} avutil={}",
            ver(avformat_version()),
            ver(avcodec_version()),
            ver(avutil_version())
        ));
    }
    // The libavformat demuxer is the DEFAULT (robust index-based seeking; fixes the HEVC
    // seek corruption of the hand-rolled path). Set /tmp/poc-demux=mkv to fall back to the
    // legacy mkv.rs demuxer for comparison.
    let fallback_mkv = std::fs::read_to_string("/tmp/poc-demux").map(|s| s.trim() == "mkv").unwrap_or(false);
    USE_FF.store(!fallback_mkv, Ordering::Relaxed);
    crate::player::log(if fallback_mkv {
        "ff: demuxer = mkv.rs (fallback via poc-demux=mkv)"
    } else {
        "ff: demuxer = libavformat"
    });
    // Phase A dev trigger: /tmp/poc-ffprobe holds a media URL to open + dump streams,
    // confirming the FFmpeg-3.3 struct offsets against known media before we build on them.
    if let Ok(u) = std::fs::read_to_string("/tmp/poc-ffprobe") {
        let u = u.trim();
        if !u.is_empty() {
            probe(u);
        }
    }
}

/// Open `url` via libavformat, find streams, and log codec_id/dims/time_base for each —
/// the on-device confirmation that the FFI struct layout is correct. Uses libavformat's
/// own HTTP for the probe (the demuxer proper will use a custom AVIO over stream.rs).
fn probe(url: &str) {
    ensure_registered();
    unsafe {
        let cu = match std::ffi::CString::new(url) {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut fmt: *mut AVFormatContext = std::ptr::null_mut();
        let r = avformat_open_input(&mut fmt, cu.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut());
        if r < 0 || fmt.is_null() {
            crate::player::log(&format!("ffprobe: avformat_open_input failed r={r}"));
            return;
        }
        let r2 = avformat_find_stream_info(fmt, std::ptr::null_mut());
        if r2 < 0 {
            crate::player::log(&format!("ffprobe: find_stream_info failed r={r2}"));
            avformat_close_input(&mut fmt);
            return;
        }
        let ns = (*fmt).nb_streams;
        crate::player::log(&format!(
            "ffprobe: nb_streams={ns} duration_us={} (expect HEVC codec_id=174 3840x1920)",
            (*fmt).duration
        ));
        let streams = (*fmt).streams;
        for i in 0..ns {
            let st = *streams.add(i as usize);
            let cp = stream_codecpar(st);
            let tb = stream_time_base(st);
            crate::player::log(&format!(
                "ffprobe: stream[{}] idx={} type={} codec_id={} {}x{} sr={} tb={}/{}",
                i,
                stream_index(st),
                (*cp).codec_type,
                (*cp).codec_id,
                (*cp).width,
                (*cp).height,
                (*cp).sample_rate,
                tb.num,
                tb.den
            ));
        }
        // BSF presence check (Phase A): both mp4toannexb filters must be compiled in.
        let hn = std::ffi::CString::new("hevc_mp4toannexb").unwrap();
        let an = std::ffi::CString::new("h264_mp4toannexb").unwrap();
        crate::player::log(&format!(
            "ffprobe: bsf hevc={} h264={}",
            !av_bsf_get_by_name(hn.as_ptr()).is_null(),
            !av_bsf_get_by_name(an.as_ptr()).is_null()
        ));
        avformat_close_input(&mut fmt);
    }
}

// ==== the demuxer (Phases B–D) ===================================================

// whence values for the AVIO seek callback
const SEEK_SET: c_int = 0;
const SEEK_CUR: c_int = 1;
const SEEK_END: c_int = 2;
const AVSEEK_SIZE: c_int = 0x10000;

/// AVIO backing state: wraps the Engine-owned demux socket so libavformat reads through
/// stream.rs's raw socket (numeric IP, no DNS, Connection: close) and can seek by
/// re-opening with a byte Range. Boxed so its address is stable for the C callbacks.
struct AvioState {
    hs: *mut HttpStream,
    aq: *mut AuQueue,
    host: CString,
    port: c_int,
    path: CString,
    off: i64,
    size: i64,
}

/// AVIOContext leading fields, so we can free avio->buffer manually (FFmpeg 3.3 has no
/// avio_context_free). `buffer` sits at +4, right after `av_class`.
#[repr(C)]
struct AVIOCtxHead {
    av_class: *const c_void,
    buffer: *mut u8,
}

extern "C" fn read_cb(op: *mut c_void, dst: *mut u8, n: c_int) -> c_int {
    unsafe {
        let s = &mut *(op as *mut AvioState);
        // interrupt: bail out of a blocked read on teardown (aborted) only. A direct-play
        // seek unblocks the read via the pump's http_close(hs) + a next_url reopen — it must
        // NOT bail here on seek_to_ns, or the post-reopen find_stream_info reads would fail.
        // seek_to_ns is now purely the post-reopen av_seek_frame target (§ reopen).
        if crate::aq::aq_is_aborted(s.aq) {
            return AVERROR_EOF;
        }
        let r = crate::stream::http_read(s.hs, dst as *mut c_uchar, n);
        if r <= 0 {
            return AVERROR_EOF;
        }
        s.off += r as i64;
        r
    }
}

extern "C" fn seek_cb(op: *mut c_void, offset: i64, whence: c_int) -> i64 {
    unsafe {
        let s = &mut *(op as *mut AvioState);
        if whence == AVSEEK_SIZE {
            return s.size;
        }
        let target = match whence {
            SEEK_SET => offset,
            SEEK_CUR => s.off + offset,
            SEEK_END => s.size + offset,
            _ => return -1,
        };
        if target < 0 {
            return -1;
        }
        crate::stream::http_close(s.hs);
        let range = CString::new(format!("Range: bytes={}-\r\n", target)).unwrap_or_default();
        if crate::stream::http_open(s.hs, s.host.as_ptr(), s.port, s.path.as_ptr(), range.as_ptr(), "GET") != 0 {
            return -1;
        }
        s.off = target;
        target
    }
}

#[inline]
unsafe fn pts_ns(pkt: *const AVPacket, st: *mut AVStream) -> i64 {
    let t = if (*pkt).pts != AV_NOPTS_VALUE { (*pkt).pts } else { (*pkt).dts };
    if t == AV_NOPTS_VALUE {
        return 0;
    }
    av_rescale_q(t, stream_time_base(st), NS_TB)
}

unsafe fn free_ptr(p: *mut c_void) {
    let mut q = p;
    av_freep(&mut q as *mut *mut c_void as *mut c_void);
}

/// Free a custom AVIOContext (buffer + context): FFmpeg 3.3 has no avio_context_free and
/// avformat_close_input does not free a caller-set pb.
unsafe fn free_avio(avio: *mut AVIOContext) {
    if avio.is_null() {
        return;
    }
    let head = avio as *mut AVIOCtxHead;
    av_freep(std::ptr::addr_of_mut!((*head).buffer) as *mut c_void); // frees + NULLs avio->buffer
    let mut p = avio;
    av_freep(&mut p as *mut *mut AVIOContext as *mut c_void); // frees + NULLs the context
}

/// The libavformat demuxer thread body — replaces mkv.rs's stream_thread when use_ff().
/// Opens the URL through a custom AVIO over stream.rs, reads packets, converts video to
/// Annex-B via the mp4toannexb BSF (VPS/SPS/PPS prepended at every keyframe), feeds video
/// (es=1) + raw audio (es=2) to the AuQueue, and seeks via av_seek_frame.
/// Parse an avcC (H264) / hvcC (HEVC) extradata record into a ready-to-prepend Annex-B
/// parameter-set blob (VPS/SPS/PPS with 4-byte start codes) + the NAL length-prefix size.
/// Mirrors mkv.rs's mkv_parse_avcc/mkv_parse_hvcc — the format the Starfish decoder wants.
unsafe fn parse_extradata(ed: *const u8, len: usize, is_hevc: bool) -> (Vec<u8>, usize) {
    let mut blob = Vec::new();
    if ed.is_null() || len < 7 {
        return (blob, 4);
    }
    let e = std::slice::from_raw_parts(ed, len);
    let sc = [0u8, 0, 0, 1];
    if is_hevc {
        // HEVCDecoderConfigurationRecord: ver@0==1, nal_len@21, numArrays@22, arrays@23.
        if len < 23 || e[0] != 1 {
            return (blob, 4);
        }
        let nls = (e[21] & 3) as usize + 1;
        let narr = e[22] as usize;
        let mut p = 23usize;
        for _ in 0..narr {
            if p + 3 > len {
                break;
            }
            // array header: [completeness|reserved|NAL_type(&0x3f)] u16 count.
            // ONLY VPS(32)/SPS(33)/PPS(34) belong in the prepend blob; skip SEI(39/40) and
            // anything else — a stray SEI here corrupts the parameter-set sequence.
            let keep = matches!(e[p] & 0x3f, 32 | 33 | 34);
            let cnt = ((e[p + 1] as usize) << 8) | e[p + 2] as usize;
            p += 3;
            for _ in 0..cnt {
                if p + 2 > len {
                    break;
                }
                let nl = ((e[p] as usize) << 8) | e[p + 1] as usize;
                p += 2;
                if nl == 0 || p + nl > len {
                    break;
                }
                if keep {
                    blob.extend_from_slice(&sc);
                    blob.extend_from_slice(&e[p..p + nl]);
                }
                p += nl;
            }
        }
        (blob, nls)
    } else {
        // AVCDecoderConfigurationRecord: ver@0==1, nal_len@4, SPS list @5, then PPS list.
        if e[0] != 1 {
            return (blob, 4);
        }
        let nls = (e[4] & 3) as usize + 1;
        let nsps = (e[5] & 0x1f) as usize;
        let mut p = 6usize;
        for _ in 0..nsps {
            if p + 2 > len {
                break;
            }
            let nl = ((e[p] as usize) << 8) | e[p + 1] as usize;
            p += 2;
            if nl == 0 || p + nl > len {
                break;
            }
            blob.extend_from_slice(&sc);
            blob.extend_from_slice(&e[p..p + nl]);
            p += nl;
        }
        if p < len {
            let npps = e[p] as usize;
            p += 1;
            for _ in 0..npps {
                if p + 2 > len {
                    break;
                }
                let nl = ((e[p] as usize) << 8) | e[p + 1] as usize;
                p += 2;
                if nl == 0 || p + nl > len {
                    break;
                }
                blob.extend_from_slice(&sc);
                blob.extend_from_slice(&e[p..p + nl]);
                p += nl;
            }
        }
        (blob, nls)
    }
}

static SEI_STRIPPED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// True if this HEVC NAL is an SEI (prefix=39 / suffix=40) carrying a user-data-registered
/// ITU-T T.35 HDR10+ dynamic-metadata message (payloadType 4, country_code 0xB5, terminal
/// provider 0x003C). webOS's HEVC decoder does NOT support per-frame HDR10+ metadata and
/// stalls on it — Kodi strips it unconditionally ("webOS doesn't support HDR10+ and it can
/// cause issues"). `nal` is the NAL payload WITHOUT the start code (starts at the 2-byte
/// NAL header). Static HDR10 mastering-display / CLL SEI (payloadType 137/144) is kept.
fn is_hdr10plus_sei(nal: &[u8]) -> bool {
    if nal.len() < 2 {
        return false;
    }
    let nal_type = (nal[0] >> 1) & 0x3f;
    if nal_type != 39 && nal_type != 40 {
        return false;
    }
    let mut p = 2usize; // SEI RBSP starts after the 2-byte NAL header
    let mut ptype = 0usize; // payloadType (0xFF-run + terminator)
    while p < nal.len() && nal[p] == 0xff {
        ptype += 255;
        p += 1;
    }
    if p >= nal.len() {
        return false;
    }
    ptype += nal[p] as usize;
    p += 1;
    while p < nal.len() && nal[p] == 0xff {
        p += 1; // skip payloadSize (0xFF-run + terminator); value unused
    }
    p += 1;
    // user_data_registered_itu_t_t35 == 4; T.35 header: country 0xB5, provider 0x003C
    ptype == 4 && p + 3 <= nal.len() && nal[p] == 0xb5 && nal[p + 1] == 0x00 && nal[p + 2] == 0x3c
}

/// Convert one length-prefixed video packet to Annex-B (4-byte start codes) into `out`,
/// prepending `param` (VPS/SPS/PPS) when the AU is a keyframe (H264 IDR type 5 / HEVC IRAP
/// types 16-23). Returns true if it is a keyframe. Mirrors mkv_handle_block.
unsafe fn packet_to_annexb(
    data: *const u8,
    size: usize,
    nls: usize,
    is_hevc: bool,
    param: &[u8],
    out: &mut Vec<u8>,
) -> bool {
    out.clear();
    if data.is_null() || size < nls + 1 {
        return false;
    }
    let d = std::slice::from_raw_parts(data, size);
    // pass 1: is this AU a keyframe?
    let mut is_key = false;
    let mut i = 0usize;
    while i + nls <= size {
        let mut nl = 0usize;
        for k in 0..nls {
            nl = (nl << 8) | d[i + k] as usize;
        }
        i += nls;
        if nl == 0 || i + nl > size {
            break;
        }
        let b0 = d[i];
        let key = if is_hevc {
            (16..=23).contains(&((b0 >> 1) & 0x3f))
        } else {
            (b0 & 0x1f) == 5
        };
        if key {
            is_key = true;
            break;
        }
        i += nl;
    }
    if is_key {
        out.extend_from_slice(param);
    }
    // pass 2: emit each NAL as 00 00 00 01 + bytes, dropping per-frame HDR10+ SEI (HEVC)
    let sc = [0u8, 0, 0, 1];
    let mut i = 0usize;
    while i + nls <= size {
        let mut nl = 0usize;
        for k in 0..nls {
            nl = (nl << 8) | d[i + k] as usize;
        }
        i += nls;
        if nl == 0 || i + nl > size {
            break;
        }
        let nal = &d[i..i + nl];
        if is_hevc && is_hdr10plus_sei(nal) {
            let n = SEI_STRIPPED.fetch_add(1, Ordering::Relaxed) + 1;
            if n <= 3 || n % 500 == 0 {
                crate::player::log(&format!("ff: stripped HDR10+ SEI #{n} ({nl} bytes)"));
            }
            i += nl;
            continue;
        }
        out.extend_from_slice(&sc);
        out.extend_from_slice(nal);
        i += nl;
    }
    is_key
}

static DIAG_FIRST: AtomicBool = AtomicBool::new(true);

pub(crate) fn demux(host: String, port: c_int, path: String, aq: SendPtr<AuQueue>, hs: SendPtr<HttpStream>) {
    DIAG_FIRST.store(true, Ordering::Relaxed);
    ensure_registered();
    let aq_p = aq.0;
    let hs_p = hs.0;
    let mut host_c = CString::new(host).unwrap_or_default();
    let mut path_c = CString::new(path).unwrap_or_default();
    let mut port = port;

    // OUTER loop: a transcode seek / audio-switch re-points us at a fresh start.mkv URL
    // (a live transcode has no seekable index), reopening the whole AVFormatContext.
    'outer: loop {
        unsafe {
            crate::stream::http_close(hs_p);
            if crate::stream::http_open(hs_p, host_c.as_ptr(), port, path_c.as_ptr(), std::ptr::null(), "GET") != 0 {
                crate::player::log(&format!("ff: http_open FAILED status={}", crate::stream::hs_status(hs_p)));
                break;
            }
            let size = crate::stream::hs_content_length(hs_p);
            SHARED.file_size.store(size, Ordering::Release);
            crate::player::log(&format!("ff: open status={} clen={}", crate::stream::hs_status(hs_p), size));

            let mut state = Box::new(AvioState {
                hs: hs_p,
                aq: aq_p,
                host: host_c.clone(),
                port,
                path: path_c.clone(),
                off: 0,
                size,
            });
            let buf = av_malloc(65536) as *mut u8;
            if buf.is_null() {
                crate::player::log("ff: av_malloc failed");
                break;
            }
            let avio = avio_alloc_context(
                buf,
                65536,
                0,
                &mut *state as *mut AvioState as *mut c_void,
                Some(read_cb),
                None,
                Some(seek_cb),
            );
            if avio.is_null() {
                crate::player::log("ff: avio_alloc_context failed");
                free_ptr(buf as *mut c_void);
                break;
            }
            let mut fmt = avformat_alloc_context();
            if fmt.is_null() {
                crate::player::log("ff: avformat_alloc_context failed");
                free_avio(avio);
                break;
            }
            (*fmt).pb = avio;
            let r = avformat_open_input(&mut fmt, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut());
            if r < 0 || fmt.is_null() {
                crate::player::log(&format!("ff: open_input failed r={r}"));
                free_avio(avio);
                break;
            }
            if avformat_find_stream_info(fmt, std::ptr::null_mut()) < 0 {
                crate::player::log("ff: find_stream_info failed");
                avformat_close_input(&mut fmt);
                free_avio(avio);
                break;
            }
            let vi = av_find_best_stream(fmt, AVMEDIA_TYPE_VIDEO, -1, -1, std::ptr::null_mut(), 0);
            // native audio-track selection: feed the chosen Nth audio stream (SHARED.desired_
            // audio_idx, set by the track menu), else the pipeline's best/default audio.
            let want_aidx = SHARED.desired_audio_idx.load(Ordering::Relaxed);
            let ai = if want_aidx >= 0 {
                nth_audio_stream(fmt, want_aidx)
                    .unwrap_or_else(|| av_find_best_stream(fmt, AVMEDIA_TYPE_AUDIO, -1, -1, std::ptr::null_mut(), 0))
            } else {
                // default: feed the track matching the Load payload's codec (Media[0].audioCodec),
                // NOT av_find_best_stream — a codec mismatch stalls the audio ES and wedges video.
                audio_stream_matching(fmt, &crate::route::stream_acodec())
                    .unwrap_or_else(|| av_find_best_stream(fmt, AVMEDIA_TYPE_AUDIO, -1, -1, std::ptr::null_mut(), 0))
            };
            if vi < 0 {
                crate::player::log("ff: no video stream");
                avformat_close_input(&mut fmt);
                free_avio(avio);
                break;
            }
            let streams = (*fmt).streams;
            let vst = *streams.add(vi as usize);
            let vcp = stream_codecpar(vst);
            let dur = (*fmt).duration;
            if dur > 0 {
                SHARED.duration_ns.store(dur.saturating_mul(1000), Ordering::Relaxed);
            }
            crate::player::log(&format!(
                "ff: v=#{vi} codec_id={} {}x{} a=#{ai} dur_ns={}",
                (*vcp).codec_id,
                (*vcp).width,
                (*vcp).height,
                SHARED.duration_ns.load(Ordering::Relaxed)
            ));

            // Hand-roll length-prefix -> Annex-B + prepend VPS/SPS/PPS at every keyframe (the
            // format Starfish decodes). FFmpeg's hevc_mp4toannexb does NOT reliably prepend the
            // parameter sets on this 3.3 build (it leaves the keyframe starting with SEI), so we
            // build the AU from the codecpar extradata; libavformat still owns demux + seeking.
            let is_hevc = (*vcp).codec_id == AV_CODEC_ID_HEVC;
            let (param_blob, nal_len_size) =
                parse_extradata((*vcp).extradata, (*vcp).extradata_size.max(0) as usize, is_hevc);
            crate::player::log(&format!(
                "ff: param_sets={} bytes nal_len={} is_hevc={}",
                param_blob.len(),
                nal_len_size,
                is_hevc
            ));
            let mut aubuf: Vec<u8> = Vec::with_capacity(4 * 1024 * 1024);

            let pkt = av_packet_alloc();
            if pkt.is_null() {
                crate::player::log("ff: packet_alloc failed");
                avformat_close_input(&mut fmt);
                free_avio(avio);
                break;
            }

            // Direct-play seek/resume: the pump reopened us on the same part URL and left a
            // target ns in SHARED.seek_to_ns. Seek on THIS freshly-opened AVFormatContext (an
            // in-place av_seek_frame on a live context corrupts matroska demuxer state ->
            // "Playing error"). A transcode reopen leaves seek_to_ns=-1 (start.mkv is already
            // 0-based at &offset), so it skips this.
            let seek_ns = SHARED.seek_to_ns.swap(-1, Ordering::Acquire);
            if seek_ns >= 0 {
                let ts = av_rescale_q(seek_ns, NS_TB, stream_time_base(vst));
                let sr = av_seek_frame(fmt, vi, ts, AVSEEK_FLAG_BACKWARD);
                crate::player::log(&format!("ff: seek-after-reopen {}s rv={sr}", seek_ns / 1_000_000_000));
            }

            // INNER read loop
            loop {
                let r = av_read_frame(fmt, pkt);
                if r < 0 {
                    // EOF, a direct-play seek, or teardown — break to the outer loop, which
                    // reopens on next_url (same part URL) and av_seek_frame's after reopen.
                    break;
                }
                let si = (*pkt).stream_index;
                if si == vi {
                    let is_key = packet_to_annexb(
                        (*pkt).data,
                        (*pkt).size.max(0) as usize,
                        nal_len_size,
                        is_hevc,
                        &param_blob,
                        &mut aubuf,
                    );
                    let pts = pts_ns(pkt, vst);
                    if DIAG_FIRST.swap(false, Ordering::Relaxed) {
                        let n = aubuf.len().min(40);
                        let head: String =
                            aubuf[..n].iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ");
                        crate::player::log(&format!(
                            "ff: AU#0 size={} key={} head=[{}]",
                            aubuf.len(),
                            is_key,
                            head
                        ));
                    }
                    crate::aq::aq_push(
                        aq_p,
                        aubuf.as_ptr(),
                        aubuf.len() as c_int,
                        pts,
                        if is_key { 1 } else { 0 },
                        1,
                    );
                    av_packet_unref(pkt);
                } else if si == ai && FEED_AUDIO.load(Ordering::Relaxed) {
                    let ast = *streams.add(ai as usize);
                    let pts = pts_ns(pkt, ast);
                    crate::aq::aq_push(aq_p, (*pkt).data, (*pkt).size, pts, 1, 2);
                    av_packet_unref(pkt);
                } else {
                    av_packet_unref(pkt);
                }
                if crate::aq::aq_is_aborted(aq_p) {
                    break;
                }
            }

            // cleanup this stream (we own pb, so close_input won't free the AVIO)
            let mut pkt_m = pkt;
            av_packet_free(&mut pkt_m);
            avformat_close_input(&mut fmt);
            free_avio(avio);
            let _ = &state; // keep the AvioState alive until after free_avio
        }

        if unsafe { crate::aq::aq_is_aborted(aq_p) } {
            break;
        }
        // transcode seek / audio-switch: re-point at the new start.mkv URL and reopen.
        let sb = SHARED.seek_byte.swap(-1, Ordering::Acquire);
        if sb >= 0 {
            if let Some(nu) = SHARED.next_url.lock().unwrap().take() {
                let (h, p, pa) = crate::player::engine::parse_stream_url(&nu);
                host_c = CString::new(h).unwrap_or_default();
                path_c = CString::new(pa).unwrap_or_default();
                port = p;
                let _ = SHARED.reparse_next.swap(false, Ordering::Acquire);
                crate::player::log("ff: seek → new transcode url (&offset)");
                continue 'outer;
            }
        }
        break;
    }
    crate::aq::aq_set_eof(aq_p);
    crate::player::log("ff: demux ended");
}
