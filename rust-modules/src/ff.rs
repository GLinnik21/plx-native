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
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::sync::Once;

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
